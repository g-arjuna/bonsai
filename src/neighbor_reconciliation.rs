//! Deterministic ARP/ND ↔ MAC forwarding-table reconciliation.
//!
//! The result is evidence consistency inside an explicitly established
//! forwarding domain, not forwarding-plane proof. VLAN+MAC alone is never a
//! global key: canonical device, tenant scope, network namespace/ASIC,
//! forwarding-domain evidence, VRF/interface attachment and capture-time
//! compatibility all participate before a definitive result is produced.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

use crate::interface_facts::FieldAvailability;
use crate::interface_relationships::InterfaceEntityKey;
use crate::neighbor_facts::{
    ArpndNeighborPayload, FACT_KIND_ARPND_NEIGHBOR, FACT_KIND_MAC_FORWARDING, MacForwardingPayload,
    NeighborResolutionState,
};
use crate::semantic_evidence::{
    AcquisitionScopeContract, CaptureTimeCompatibility, EvidenceFreshness, ExpectedFactContract,
    FreshnessPolicy, ScopedCaptureCoverage, capture_time_compatibility,
    evidence_time_compatibility, fact_freshness, observation_freshness, select_usable_coverage,
    validate_fact_envelope,
};
use crate::typed_facts::{EvidenceRef, InterfaceIdentityKind, Lifecycle, TrustClass, TypedFact};

pub const MAX_RECONCILIATION_INPUT_FACTS: usize = 100_000;
pub const MAX_RECONCILIATION_RESULTS: usize = 50_000;
pub const MAX_RESULT_EVIDENCE_REFS: usize = 64;
pub const MAX_CANDIDATE_EVALUATIONS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossTableOutcome {
    /// Exactly one applicable MAC observation exists in the same established
    /// forwarding domain and independently comparable forwarding-interface
    /// evidence agrees. This is evidence consistency, not forwarding-plane proof.
    Consistent,
    /// Exactly one applicable MAC observation exists in the same established
    /// forwarding domain, but the logical neighbor attachment and physical MAC
    /// attachment are not independently comparable. This positive scoped
    /// correlation is deliberately distinct from verified interface agreement.
    ScopedCorrelationInterfaceNotComparable,
    /// Exactly one applicable MAC observation exists and independently
    /// comparable forwarding-interface evidence disagrees.
    ConflictingInterface,
    /// Multiple distinct MAC observations exist for the same scoped
    /// forwarding-domain/VLAN/MAC key.
    MultipleCandidates,
    /// Neighbor evidence does not establish the VLAN needed for an L2 join
    /// where VLAN scoping is applicable.
    IndeterminateMissingVlan,
    /// Neighbor evidence does not establish a valid MAC.
    IndeterminateMissingMac,
    /// The neighbor cache entry is failed/incomplete or otherwise unresolved;
    /// residual MAC evidence cannot be promoted to a resolved adjacency.
    IndeterminateNeighborUnresolved,
    /// No evidence establishes which bridge/L2 forwarding domain the neighbor
    /// belongs to. VLAN number or SVI naming alone is not treated as proof.
    IndeterminateMissingForwardingDomain,
    /// Forwarding-domain evidence places the observations in conflicting or
    /// multiple domains, so no deterministic join is possible.
    IndeterminateConflictingForwardingDomain,
    /// The relevant forwarding domain is inside applicable, current, complete
    /// declared MAC coverage and no matching row was observed.
    AbsentInDeclaredCoverage,
    /// No row exists, but coverage is incomplete/unknown/stale or does not
    /// apply to this exact acquisition scope.
    UnknownOutsideCoverage,
    /// Usable rows exist only in observation batches outside the explicit
    /// capture-skew policy (or their times are unknown).
    IncompatibleObservations,
    /// The candidate-evaluation bound was reached before uniqueness,
    /// absence or temporal compatibility could be established. This is
    /// deliberately indeterminate: truncation must never manufacture a
    /// consistent, absent or incompatible observation result.
    IndeterminateCandidateEvaluationTruncated,
    /// Typed payloads were malformed or semantically inconsistent. Per-input
    /// invalid facts are normally counted and skipped rather than emitted as
    /// results; this variant is retained for API completeness.
    InvalidInput,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CrossTableResult {
    pub arpnd_fact_id: String,
    pub ip_address: String,
    pub neighbor_interface: String,
    pub neighbor_forwarding_interface: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forwarding_domain: Option<String>,
    pub vlan_id: Option<u32>,
    pub mac_address: Option<String>,
    pub outcome: CrossTableOutcome,
    pub mac_fact_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domain_evidence_refs: Vec<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coverage_evidence_refs: Vec<EvidenceRef>,
    pub reason: String,
}

/// Independently established association between a routing/attachment scope
/// and an L2 forwarding domain. This evidence may come from interface/VLAN or
/// bridge normalization; it is never inferred from an SVI display name here.
#[derive(Clone, Debug, PartialEq)]
pub struct ForwardingDomainEvidence {
    pub device_id: String,
    pub scope: Option<String>,
    pub network_namespace: Option<String>,
    /// Canonical bridge/L2 forwarding-domain identity within
    /// device+scope+network namespace.
    pub domain_id: String,
    pub vlan_id: Option<u32>,
    /// Availability of the source membership collection used to establish
    /// forwarding interface population. `None` is used by external/hand-written
    /// domain evidence that predates the explicit VLAN membership axis.
    pub membership_availability: Option<crate::vlan_facts::MembershipAvailability>,
    /// Evidence plane of the source membership collection. This preserves the
    /// distinction between operational bridge evidence and weaker
    /// source-reported VLAN output; consumers must not upgrade it silently.
    pub membership_plane: Option<crate::vlan_facts::MembershipPlane>,
    /// Network instance explicitly associated with this domain, when known.
    pub vrf: Option<String>,
    /// Logical/routed interface aliases/native names attached to the domain.
    /// These strings are qualified lookup evidence only; a canonical parent
    /// name does not automatically match its subinterfaces.
    pub routed_interfaces: BTreeSet<String>,
    /// Physical/bridged forwarding interface aliases/native names attached to
    /// the domain, with the same qualified-alias caveat.
    pub forwarding_interfaces: BTreeSet<String>,
    /// Structured canonical interface keys independently established by the
    /// interface evidence model. When present these are identity evidence;
    /// strings above remain aliases/native lookup evidence only.
    pub routed_interface_keys: BTreeSet<InterfaceEntityKey>,
    pub forwarding_interface_keys: BTreeSet<InterfaceEntityKey>,
    /// Positively established interface relationship evidence inside this
    /// forwarding domain (for example LAG aggregate ↔ member). It is used only
    /// for relationship-aware comparison, never as unrestricted identity
    /// equivalence or as a parent/subinterface implication.
    pub interface_equivalents: BTreeMap<String, BTreeSet<String>>,
    pub interface_equivalent_keys: BTreeMap<InterfaceEntityKey, BTreeSet<InterfaceEntityKey>>,
    /// Analysis-batch source identity for the observations establishing this
    /// binding. Missing source identity cannot establish a current binding.
    pub collector_id: Option<String>,
    pub source_kind: Option<String>,
    pub capture_id: Option<String>,
    pub source_generation: Option<u64>,
    /// Temporal applicability metadata evaluated through the shared
    /// semantic-evidence freshness contract.
    pub observation_time: Option<String>,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub lifecycle: Lifecycle,
    pub evidence_refs: Vec<EvidenceRef>,
}

/// Explicit temporal and bounded-evaluation policy. There is no implicit wall
/// clock and no default infinite freshness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NeighborReconciliationPolicy {
    pub freshness: FreshnessPolicy,
    pub max_capture_skew_seconds: u64,
    pub max_candidate_evaluations: usize,
}

impl Default for NeighborReconciliationPolicy {
    fn default() -> Self {
        Self {
            freshness: FreshnessPolicy::default(),
            max_capture_skew_seconds: 0,
            max_candidate_evaluations: MAX_CANDIDATE_EVALUATIONS,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CrossTableReport {
    pub as_of: String,
    pub arpnd_facts_considered: usize,
    pub mac_facts_considered: usize,
    pub invalid_arpnd_facts: usize,
    pub invalid_mac_facts: usize,
    pub excluded_arpnd_facts: usize,
    pub excluded_mac_facts: usize,
    pub invalid_domain_evidence: usize,
    pub temporally_ineligible_domain_evidence: usize,
    pub domain_indeterminate_arpnd_facts: usize,
    pub domain_indeterminate_mac_facts: usize,
    pub temporal_exclusions: BTreeMap<String, usize>,
    pub candidate_evaluations_omitted: usize,
    pub outcome_counts: BTreeMap<String, usize>,
    pub results: Vec<CrossTableResult>,
}

fn known_vlan(payload: &ArpndNeighborPayload) -> Option<u32> {
    (payload.interface_vlan_id.state == FieldAvailability::Known)
        .then_some(payload.interface_vlan_id.value)
        .flatten()
}

fn known_mac(payload: &ArpndNeighborPayload) -> Option<String> {
    (payload.mac_address.state == FieldAvailability::Known)
        .then_some(payload.mac_address.value.clone())
        .flatten()
}

fn mac_vlan(payload: &MacForwardingPayload) -> Option<u32> {
    (payload.vlan_id.state == FieldAvailability::Known)
        .then_some(payload.vlan_id.value)
        .flatten()
}

fn known_bridge_domain(payload: &MacForwardingPayload) -> Option<String> {
    (payload.bridge_domain.state == FieldAvailability::Known)
        .then_some(payload.bridge_domain.value.clone())
        .flatten()
}

fn mac_interface(payload: &MacForwardingPayload) -> String {
    payload
        .normalized_outgoing_interface
        .clone()
        .unwrap_or_else(|| payload.outgoing_interface.clone())
}

fn same_scope(fact: &TypedFact, evidence: &ForwardingDomainEvidence) -> bool {
    fact.subject.device_id == evidence.device_id
        && fact.subject.scope == evidence.scope
        && fact.subject.network_namespace == evidence.network_namespace
}

#[derive(Clone, Debug)]
struct InterfaceOccurrence {
    /// Complete typed interface entity key. For a numbered subinterface the
    /// canonical parent alone is not equality evidence: kind and numeric
    /// subinterface axis participate in the key.
    key: Option<InterfaceEntityKey>,
    /// Qualified exact occurrence/native aliases supplied by the source. The
    /// canonical parent entity is deliberately removed for subinterfaces so a
    /// parent-only attachment cannot match every child.
    aliases: BTreeSet<String>,
}

fn occurrence_with_key(fact: &TypedFact, mut aliases: BTreeSet<String>) -> InterfaceOccurrence {
    let key = InterfaceEntityKey::from_fact(fact);
    if let Some(key) = &key {
        if key.identity_kind != Some(InterfaceIdentityKind::Subinterface) {
            aliases.insert(key.canonical_interface.clone());
        } else {
            aliases.retain(|alias| alias != &key.canonical_interface);
        }
    }
    InterfaceOccurrence { key, aliases }
}

fn arp_interface_occurrence(
    fact: &TypedFact,
    payload: &ArpndNeighborPayload,
) -> InterfaceOccurrence {
    let mut aliases = BTreeSet::new();
    aliases.insert(payload.outgoing_interface.clone());
    if let Some(native) = fact
        .subject
        .interface_identity
        .as_ref()
        .and_then(|i| i.vendor_native_name.clone())
    {
        aliases.insert(native);
    }
    occurrence_with_key(fact, aliases)
}

fn mac_interface_occurrence(
    fact: &TypedFact,
    payload: &MacForwardingPayload,
) -> InterfaceOccurrence {
    let mut aliases = BTreeSet::new();
    aliases.insert(payload.outgoing_interface.clone());
    aliases.insert(mac_interface(payload));
    if let Some(native) = fact
        .subject
        .interface_identity
        .as_ref()
        .and_then(|i| i.vendor_native_name.clone())
    {
        aliases.insert(native);
    }
    occurrence_with_key(fact, aliases)
}

fn alias_only_occurrence(name: &str) -> InterfaceOccurrence {
    InterfaceOccurrence {
        key: None,
        aliases: BTreeSet::from([name.to_string()]),
    }
}

fn occurrence_matches_name(occurrence: &InterfaceOccurrence, name: &str) -> bool {
    occurrence.aliases.contains(name)
}

fn occurrence_matches_key(occurrence: &InterfaceOccurrence, key: &InterfaceEntityKey) -> bool {
    occurrence.key.as_ref() == Some(key)
}

fn occurrences_same_interface(left: &InterfaceOccurrence, right: &InterfaceOccurrence) -> bool {
    if let (Some(left_key), Some(right_key)) = (&left.key, &right.key)
        && left_key == right_key
    {
        return true;
    }
    !left.aliases.is_disjoint(&right.aliases)
}

fn routed_interface_match(
    occurrence: &InterfaceOccurrence,
    evidence: &ForwardingDomainEvidence,
) -> bool {
    evidence
        .routed_interface_keys
        .iter()
        .any(|key| occurrence_matches_key(occurrence, key))
        || evidence
            .routed_interfaces
            .iter()
            .any(|name| occurrence_matches_name(occurrence, name))
}

fn forwarding_interface_match(
    occurrence: &InterfaceOccurrence,
    evidence: &ForwardingDomainEvidence,
) -> bool {
    evidence
        .forwarding_interface_keys
        .iter()
        .any(|key| occurrence_matches_key(occurrence, key))
        || evidence
            .forwarding_interfaces
            .iter()
            .any(|name| occurrence_matches_name(occurrence, name))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DomainAssignment {
    Established {
        domain: String,
        vlan: Option<u32>,
        evidence_refs: Vec<EvidenceRef>,
    },
    Indeterminate,
    Conflicting,
}

#[derive(Default)]
struct AssignmentPiece {
    vlan: Option<u32>,
    contradictory_vlan: bool,
    evidence_refs: Vec<EvidenceRef>,
}

fn merge_refs(target: &mut Vec<EvidenceRef>, refs: &[EvidenceRef]) {
    for evidence in refs {
        if !target.contains(evidence) {
            target.push(evidence.clone());
        }
    }
}

fn merge_assignment(
    assignments: &mut BTreeMap<String, AssignmentPiece>,
    domain: String,
    vlan: Option<u32>,
    refs: &[EvidenceRef],
) {
    let piece = assignments.entry(domain).or_default();
    match (piece.vlan, vlan) {
        (Some(existing), Some(new)) if existing != new => piece.contradictory_vlan = true,
        (None, Some(new)) => piece.vlan = Some(new),
        _ => {}
    }
    merge_refs(&mut piece.evidence_refs, refs);
}

fn merge_assignment_with_fallback(
    assignments: &mut BTreeMap<String, AssignmentPiece>,
    domain: String,
    evidence_vlan: Option<u32>,
    payload_vlan: Option<u32>,
    refs: &[EvidenceRef],
) {
    if let (Some(evidence), Some(payload)) = (evidence_vlan, payload_vlan)
        && evidence != payload
    {
        assignments
            .entry(domain.clone())
            .or_default()
            .contradictory_vlan = true;
    }
    merge_assignment(assignments, domain, evidence_vlan.or(payload_vlan), refs);
}

type DirectAssignment = (String, Option<u32>, Vec<EvidenceRef>);

fn unique_assignment(
    direct: Option<DirectAssignment>,
    matches: Vec<&ForwardingDomainEvidence>,
    fallback_vlan: Option<u32>,
) -> DomainAssignment {
    let mut domains = BTreeMap::<String, AssignmentPiece>::new();
    if let Some((domain, vlan, refs)) = direct {
        merge_assignment(&mut domains, domain, vlan.or(fallback_vlan), &refs);
    }
    for evidence in matches {
        merge_assignment_with_fallback(
            &mut domains,
            evidence.domain_id.clone(),
            evidence.vlan_id,
            fallback_vlan,
            &evidence.evidence_refs,
        );
    }
    if domains.is_empty() {
        return DomainAssignment::Indeterminate;
    }
    if domains.values().any(|piece| piece.contradictory_vlan) || domains.len() != 1 {
        return DomainAssignment::Conflicting;
    }
    let (domain, piece) = domains.into_iter().next().expect("one domain");
    DomainAssignment::Established {
        domain,
        vlan: piece.vlan,
        evidence_refs: piece.evidence_refs,
    }
}

fn assign_arp_domain(
    fact: &TypedFact,
    payload: &ArpndNeighborPayload,
    evidence: &[ForwardingDomainEvidence],
) -> DomainAssignment {
    let vlan = known_vlan(payload);
    let occurrence = arp_interface_occurrence(fact, payload);
    let direct = (payload.forwarding_domain.state == FieldAvailability::Known)
        .then_some(payload.forwarding_domain.value.clone())
        .flatten()
        .map(|domain| (domain, vlan, fact.evidence.clone()));
    let physical_occurrence = payload
        .physical_interface
        .as_deref()
        .map(alias_only_occurrence);
    let matches = evidence
        .iter()
        .filter(|ev| same_scope(fact, ev))
        .filter(|ev| {
            if ev.vrf.is_some() && fact.subject.vrf.is_some() && ev.vrf != fact.subject.vrf {
                return false;
            }
            // A VRF-to-L2 association corroborates routed-interface evidence,
            // but it is not sufficient by itself: ordinary routed interfaces
            // can coexist in the same network instance without belonging to
            // the bridge domain. Logical/L3 attachment is primary evidence.
            if routed_interface_match(&occurrence, ev) {
                return true;
            }
            // Compound neighbor evidence can independently expose the physical
            // forwarding attachment (EOS "Vlan10, Port-Channel3" or NXOS
            // phy-intf). That attachment can select the domain, but a trunk
            // may carry many VLANs: nonmatching VLAN memberships are other
            // domains, not contradictions of this neighbor's VLAN.
            if let Some(physical) = &physical_occurrence
                && forwarding_interface_match(physical, ev)
            {
                if let (Some(ev_vlan), Some(neighbor_vlan)) = (ev.vlan_id, vlan)
                    && ev_vlan != neighbor_vlan
                {
                    return false;
                }
                return true;
            }
            false
        })
        .collect();
    unique_assignment(direct, matches, vlan)
}

fn assign_mac_domain(
    fact: &TypedFact,
    payload: &MacForwardingPayload,
    evidence: &[ForwardingDomainEvidence],
) -> DomainAssignment {
    let vlan = mac_vlan(payload);
    let occurrence = mac_interface_occurrence(fact, payload);
    let direct = known_bridge_domain(payload).map(|domain| (domain, vlan, fact.evidence.clone()));
    let matches = evidence
        .iter()
        .filter(|ev| same_scope(fact, ev))
        .filter(|ev| {
            let direct_domain_match = ev.domain_id == direct_domain(&direct);
            // Forwarding/trunk interfaces can legitimately carry multiple VLANs.
            // Once the MAC entry independently establishes its VLAN, nonmatching
            // interface memberships are not applicable to this row. Evidence for
            // the row's direct bridge-domain identity remains applicable, however:
            // a contradictory VLAN for that same domain must not be silently
            // filtered away and allowed to lose to the direct payload claim.
            if !direct_domain_match
                && let (Some(ev_vlan), Some(mac_vlan)) = (ev.vlan_id, vlan)
                && ev_vlan != mac_vlan
            {
                return false;
            }
            forwarding_interface_match(&occurrence, ev) || direct_domain_match
        })
        .collect();
    unique_assignment(direct, matches, vlan)
}

fn direct_domain(direct: &Option<DirectAssignment>) -> String {
    direct
        .as_ref()
        .map(|(domain, _, _)| domain.clone())
        .unwrap_or_default()
}

struct ArpEntry<'a> {
    fact: &'a TypedFact,
    payload: ArpndNeighborPayload,
    assignment: DomainAssignment,
}

struct MacEntry<'a> {
    fact: &'a TypedFact,
    payload: MacForwardingPayload,
    assignment: DomainAssignment,
}

type Scope = Option<String>;
type Namespace = Option<String>;
type MacIndexKey = (String, Scope, Namespace, String, Option<u32>, String);
type DomainContextKey = (String, Scope, Namespace, String);

fn decode_payload<T: serde::de::DeserializeOwned>(fact: &TypedFact) -> Option<T> {
    serde_json::from_value(fact.payload.fields.clone()?).ok()
}

fn eligible_envelope(fact: &TypedFact, kind: &'static str) -> Result<(), String> {
    validate_fact_envelope(
        fact,
        ExpectedFactContract {
            fact_kind: kind,
            subject_namespace: "interface",
            operational_plane: "operational",
            minimum_trust: TrustClass::DeviceAsserted,
        },
    )
    .map_err(|e| format!("{e:?}"))
}

fn bump(map: &mut BTreeMap<String, usize>, key: EvidenceFreshness) {
    *map.entry(format!("{key:?}").to_string()).or_insert(0) += 1;
}

fn coverage_applies(
    coverage: &ScopedCaptureCoverage,
    arp_fact: &TypedFact,
    max_capture_skew_seconds: u64,
) -> bool {
    coverage.device_id == arp_fact.subject.device_id
        && coverage.scope == arp_fact.subject.scope
        && coverage.network_namespace == arp_fact.subject.network_namespace
        && coverage.covered_fact_kind == FACT_KIND_MAC_FORWARDING
        && coverage.declared_scope == AcquisitionScopeContract::CompleteScopedTable
        && evidence_time_compatibility(
            arp_fact,
            coverage.observation_time.as_deref(),
            max_capture_skew_seconds,
        ) == CaptureTimeCompatibility::Compatible
}

fn unresolved_relevant_mac_rows(
    coverage: &ScopedCaptureCoverage,
    macs: &[MacEntry<'_>],
    vlan: Option<u32>,
) -> usize {
    macs.iter()
        .filter(|entry| {
            entry.fact.subject.device_id == coverage.device_id
                && entry.fact.subject.scope == coverage.scope
                && entry.fact.subject.network_namespace == coverage.network_namespace
                && entry.fact.source.collector_id == coverage.collector_id
                && entry.fact.source.source_kind == coverage.source_kind
                && entry.fact.source.capture_id == coverage.capture_id
                && coverage
                    .source_generation
                    .is_none_or(|generation| generation == entry.fact.source.session_epoch)
        })
        .filter(|entry| !matches!(entry.assignment, DomainAssignment::Established { .. }))
        .filter(|entry| {
            let row_vlan = mac_vlan(&entry.payload);
            row_vlan.is_none() || vlan.is_none() || row_vlan == vlan
        })
        .count()
}

#[derive(Default)]
struct DomainInterfaceContext {
    forwarding_interfaces: BTreeSet<String>,
    forwarding_interface_keys: BTreeSet<InterfaceEntityKey>,
    interface_equivalents: BTreeMap<String, BTreeSet<String>>,
    interface_equivalent_keys: BTreeMap<InterfaceEntityKey, BTreeSet<InterfaceEntityKey>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InterfaceComparison {
    Agree,
    Conflict,
    NotComparable,
}

fn interfaces_related(
    equivalents: &BTreeMap<String, BTreeSet<String>>,
    equivalent_keys: &BTreeMap<InterfaceEntityKey, BTreeSet<InterfaceEntityKey>>,
    left: &InterfaceOccurrence,
    right: &InterfaceOccurrence,
) -> bool {
    if let (Some(left_key), Some(right_key)) = (&left.key, &right.key) {
        if equivalent_keys
            .get(left_key)
            .is_some_and(|set| set.contains(right_key))
            || equivalent_keys
                .get(right_key)
                .is_some_and(|set| set.contains(left_key))
        {
            return true;
        }
    }
    left.aliases.iter().any(|left_name| {
        right.aliases.iter().any(|right_name| {
            equivalents
                .get(left_name)
                .is_some_and(|set| set.contains(right_name))
                || equivalents
                    .get(right_name)
                    .is_some_and(|set| set.contains(left_name))
        })
    })
}

fn comparable_forwarding_evidence(
    arp_fact: &TypedFact,
    arp: &ArpndNeighborPayload,
    mac_fact: &TypedFact,
    mac: &MacForwardingPayload,
    context: &DomainInterfaceContext,
) -> InterfaceComparison {
    let mac_occurrence = mac_interface_occurrence(mac_fact, mac);
    let arp_occurrence = arp_interface_occurrence(arp_fact, arp);
    if occurrences_same_interface(&arp_occurrence, &mac_occurrence)
        || interfaces_related(
            &context.interface_equivalents,
            &context.interface_equivalent_keys,
            &arp_occurrence,
            &mac_occurrence,
        )
    {
        return InterfaceComparison::Agree;
    }
    if let Some(physical) = &arp.physical_interface {
        let physical_occurrence = alias_only_occurrence(physical);
        if occurrences_same_interface(&physical_occurrence, &mac_occurrence)
            || interfaces_related(
                &context.interface_equivalents,
                &context.interface_equivalent_keys,
                &physical_occurrence,
                &mac_occurrence,
            )
        {
            return InterfaceComparison::Agree;
        }
        // The source explicitly supplied a forwarding/physical attachment;
        // that is comparable evidence even when the logical SVI differs.
        return InterfaceComparison::Conflict;
    }
    let arp_is_forwarding_attached = arp_occurrence
        .key
        .as_ref()
        .is_some_and(|key| context.forwarding_interface_keys.contains(key))
        || arp_occurrence
            .aliases
            .iter()
            .any(|name| context.forwarding_interfaces.contains(name));
    if arp_is_forwarding_attached {
        return InterfaceComparison::Conflict;
    }
    InterfaceComparison::NotComparable
}

fn nonempty(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

pub(crate) fn domain_batch_established(evidence: &ForwardingDomainEvidence) -> bool {
    nonempty(evidence.collector_id.as_deref())
        && nonempty(evidence.source_kind.as_deref())
        && nonempty(evidence.capture_id.as_deref())
        && evidence.source_generation.is_some()
        && nonempty(evidence.observation_time.as_deref())
        && !evidence.evidence_refs.is_empty()
}

pub(crate) fn domain_evidence_current(
    evidence: &ForwardingDomainEvidence,
    as_of: crate::semantic_evidence::Time,
    policy: FreshnessPolicy,
) -> bool {
    if !domain_batch_established(evidence) {
        return false;
    }
    observation_freshness(
        evidence.lifecycle,
        evidence.observation_time.as_deref().unwrap_or_default(),
        evidence.valid_from.as_deref(),
        evidence.valid_until.as_deref(),
        as_of,
        policy,
    )
    .is_current()
}

fn unresolved_neighbor(payload: &ArpndNeighborPayload) -> bool {
    payload.resolution_state.state == FieldAvailability::Known
        && matches!(
            payload.resolution_state.value,
            Some(NeighborResolutionState::Incomplete | NeighborResolutionState::Failed)
        )
}

/// Reconcile validated current typed neighbor and MAC facts at an explicit
/// `as_of`. `coverage` must come from actual normalization/acquisition output;
/// callers do not manufacture global completeness booleans.
pub fn reconcile_neighbor_tables(
    facts: &[TypedFact],
    coverage: &[ScopedCaptureCoverage],
    forwarding_domains: &[ForwardingDomainEvidence],
    policy: &NeighborReconciliationPolicy,
    as_of: &str,
) -> Result<CrossTableReport, String> {
    let as_of = crate::semantic_evidence::parse_time(as_of)
        .ok_or_else(|| "as_of is not RFC-3339".to_string())?;
    if facts.len() > MAX_RECONCILIATION_INPUT_FACTS {
        return Err(format!(
            "reconciliation input fact count {} exceeds bound {MAX_RECONCILIATION_INPUT_FACTS}",
            facts.len()
        ));
    }

    let mut invalid_arpnd = 0usize;
    let mut invalid_mac = 0usize;
    let mut excluded_arpnd = 0usize;
    let mut excluded_mac = 0usize;
    let mut temporal_exclusions = BTreeMap::new();
    let mut arp_entries = Vec::new();
    let mut mac_entries = Vec::new();

    for fact in facts {
        match fact.fact_kind.as_str() {
            FACT_KIND_ARPND_NEIGHBOR => {
                if eligible_envelope(fact, FACT_KIND_ARPND_NEIGHBOR).is_err() {
                    invalid_arpnd += 1;
                    continue;
                }
                let Some(payload) = decode_payload::<ArpndNeighborPayload>(fact) else {
                    invalid_arpnd += 1;
                    continue;
                };
                let freshness = fact_freshness(fact, as_of, policy.freshness);
                if !freshness.is_current() {
                    excluded_arpnd += 1;
                    bump(&mut temporal_exclusions, freshness);
                    continue;
                }
                arp_entries.push((fact, payload));
            }
            FACT_KIND_MAC_FORWARDING => {
                if eligible_envelope(fact, FACT_KIND_MAC_FORWARDING).is_err() {
                    invalid_mac += 1;
                    continue;
                }
                let Some(payload) = decode_payload::<MacForwardingPayload>(fact) else {
                    invalid_mac += 1;
                    continue;
                };
                let freshness = fact_freshness(fact, as_of, policy.freshness);
                if !freshness.is_current() {
                    excluded_mac += 1;
                    bump(&mut temporal_exclusions, freshness);
                    continue;
                }
                mac_entries.push((fact, payload));
            }
            _ => {}
        }
    }

    let mut valid_domains = Vec::new();
    let mut invalid_domain_evidence = 0usize;
    let mut temporally_ineligible_domain_evidence = 0usize;
    for evidence in forwarding_domains {
        let shape_valid = !evidence.device_id.is_empty()
            && !evidence.domain_id.is_empty()
            && evidence.vlan_id.is_none_or(|v| v <= 4095);
        if !shape_valid {
            invalid_domain_evidence += 1;
        } else if !domain_evidence_current(evidence, as_of, policy.freshness) {
            temporally_ineligible_domain_evidence += 1;
        } else {
            valid_domains.push(evidence.clone());
        }
    }

    let mut arps = Vec::new();
    let mut macs = Vec::new();
    let mut domain_indeterminate_arpnd = 0usize;
    let mut domain_indeterminate_mac = 0usize;
    for (fact, payload) in arp_entries {
        let assignment = assign_arp_domain(fact, &payload, &valid_domains);
        if !matches!(assignment, DomainAssignment::Established { .. }) {
            domain_indeterminate_arpnd += 1;
        }
        arps.push(ArpEntry {
            fact,
            payload,
            assignment,
        });
    }
    for (fact, payload) in mac_entries {
        let assignment = assign_mac_domain(fact, &payload, &valid_domains);
        if !matches!(assignment, DomainAssignment::Established { .. }) {
            domain_indeterminate_mac += 1;
        }
        macs.push(MacEntry {
            fact,
            payload,
            assignment,
        });
    }
    arps.sort_by(|a, b| a.fact.fact_id.cmp(&b.fact.fact_id));
    macs.sort_by(|a, b| a.fact.fact_id.cmp(&b.fact.fact_id));
    if arps.len() > MAX_RECONCILIATION_RESULTS {
        return Err(format!(
            "reconciliation candidate result count {} exceeds bound {MAX_RECONCILIATION_RESULTS}",
            arps.len()
        ));
    }

    let mut mac_index: BTreeMap<MacIndexKey, Vec<usize>> = BTreeMap::new();
    for (index, entry) in macs.iter().enumerate() {
        let DomainAssignment::Established { domain, vlan, .. } = &entry.assignment else {
            continue;
        };
        let key = (
            entry.fact.subject.device_id.clone(),
            entry.fact.subject.scope.clone(),
            entry.fact.subject.network_namespace.clone(),
            domain.clone(),
            *vlan,
            entry.payload.mac_address.clone(),
        );
        mac_index.entry(key).or_default().push(index);
    }
    // Canonicalize each index bucket once. Lookups below borrow these slices;
    // concentrated keys must not clone/sort a full candidate vector per ARP row.
    for bucket in mac_index.values_mut() {
        bucket.sort_by(|a, b| macs[*a].fact.fact_id.cmp(&macs[*b].fact.fact_id));
        bucket.dedup();
    }

    // Pre-index forwarding-domain interface and relationship evidence once by
    // scoped domain identity instead of rebuilding those sets per result.
    let mut domain_contexts: BTreeMap<DomainContextKey, DomainInterfaceContext> = BTreeMap::new();
    for evidence in &valid_domains {
        let key = (
            evidence.device_id.clone(),
            evidence.scope.clone(),
            evidence.network_namespace.clone(),
            evidence.domain_id.clone(),
        );
        let context = domain_contexts.entry(key).or_default();
        context
            .forwarding_interfaces
            .extend(evidence.forwarding_interfaces.iter().cloned());
        context
            .forwarding_interface_keys
            .extend(evidence.forwarding_interface_keys.iter().cloned());
        for (interface, related) in &evidence.interface_equivalents {
            context
                .interface_equivalents
                .entry(interface.clone())
                .or_default()
                .extend(related.iter().cloned());
        }
        for (interface, related) in &evidence.interface_equivalent_keys {
            context
                .interface_equivalent_keys
                .entry(interface.clone())
                .or_default()
                .extend(related.iter().cloned());
        }
    }
    let candidate_limit = policy
        .max_candidate_evaluations
        .min(MAX_CANDIDATE_EVALUATIONS);

    let mut results = Vec::new();
    let mut candidate_evaluations_omitted = 0usize;
    for entry in &arps {
        let arp = &entry.payload;
        let forwarding = arp
            .physical_interface
            .clone()
            .unwrap_or_else(|| arp.outgoing_interface.clone());
        let vlan = known_vlan(arp);
        let mac = known_mac(arp);
        let result_mac = mac.clone();
        let (assignment_domain, assignment_vlan, domain_evidence_refs) = match &entry.assignment {
            DomainAssignment::Established {
                domain,
                vlan: domain_vlan,
                evidence_refs,
            } => (
                Some(domain.clone()),
                (*domain_vlan).or(vlan),
                evidence_refs.clone(),
            ),
            _ => (None, vlan, vec![]),
        };
        let mut coverage_evidence_refs = Vec::new();
        let (outcome, mut ids, mut reason) = if mac.is_none() {
            (
                CrossTableOutcome::IndeterminateMissingMac,
                vec![],
                "neighbor evidence does not establish a valid peer MAC".to_string(),
            )
        } else if unresolved_neighbor(arp) {
            (
                CrossTableOutcome::IndeterminateNeighborUnresolved,
                vec![],
                "neighbor resolution state is failed/incomplete; residual MAC evidence is not a resolved forwarding adjacency".to_string(),
            )
        } else if entry.assignment == DomainAssignment::Conflicting {
            (
                CrossTableOutcome::IndeterminateConflictingForwardingDomain,
                vec![],
                "forwarding-domain evidence is conflicting or ambiguous for this neighbor"
                    .to_string(),
            )
        } else if entry.assignment == DomainAssignment::Indeterminate {
            (
                CrossTableOutcome::IndeterminateMissingForwardingDomain,
                vec![],
                "no independent evidence associates the neighbor attachment/VRF with an L2 forwarding domain".to_string(),
            )
        } else {
            let domain = assignment_domain.clone().expect("established domain");
            let vlan_key = assignment_vlan;
            if vlan_key.is_none() && known_vlan(arp).is_none() {
                (
                    CrossTableOutcome::IndeterminateMissingVlan,
                    vec![],
                    "neighbor evidence and domain evidence do not establish an applicable VLAN"
                        .to_string(),
                )
            } else {
                let key = (
                    entry.fact.subject.device_id.clone(),
                    entry.fact.subject.scope.clone(),
                    entry.fact.subject.network_namespace.clone(),
                    domain.clone(),
                    vlan_key,
                    mac.clone().expect("checked mac"),
                );
                let all_candidates = mac_index.get(&key).map(Vec::as_slice).unwrap_or(&[]);
                let total_candidates = all_candidates.len();
                if total_candidates > candidate_limit {
                    candidate_evaluations_omitted += total_candidates - candidate_limit;
                }
                let candidates = &all_candidates[..total_candidates.min(candidate_limit)];
                let mut compatible = Vec::new();
                let mut incompatible = 0usize;
                for idx in candidates {
                    let mac_entry = &macs[*idx];
                    match capture_time_compatibility(
                        entry.fact,
                        mac_entry.fact,
                        policy.max_capture_skew_seconds,
                    ) {
                        CaptureTimeCompatibility::Compatible => compatible.push(mac_entry),
                        CaptureTimeCompatibility::ExcessiveSkew
                        | CaptureTimeCompatibility::UnknownTime => incompatible += 1,
                    }
                }
                let truncated = total_candidates > candidate_limit;
                if compatible.is_empty() {
                    if truncated {
                        (
                            CrossTableOutcome::IndeterminateCandidateEvaluationTruncated,
                            vec![],
                            format!(
                                "candidate evaluation bounded at {} of {total_candidates} scoped rows; absence and temporal compatibility remain indeterminate",
                                candidate_limit
                            ),
                        )
                    } else if total_candidates > 0 {
                        (
                            CrossTableOutcome::IncompatibleObservations,
                            vec![],
                            format!(
                                "{incompatible} scoped candidate rows exist but are outside the capture-skew policy or have unknown capture time"
                            ),
                        )
                    } else {
                        let applicable = coverage.iter().filter(|cov| {
                            coverage_applies(cov, entry.fact, policy.max_capture_skew_seconds)
                                && unresolved_relevant_mac_rows(cov, &macs, vlan_key) == 0
                        });
                        if let Some(selected) =
                            select_usable_coverage(applicable, as_of, policy.freshness)
                        {
                            coverage_evidence_refs = selected.evidence_refs.clone();
                            (
                                CrossTableOutcome::AbsentInDeclaredCoverage,
                                vec![],
                                "VLAN+MAC absent inside current complete coverage for the exact device/scope/namespace acquisition".to_string(),
                            )
                        } else {
                            (
                                CrossTableOutcome::UnknownOutsideCoverage,
                                vec![],
                                "no matching row, but applicable MAC coverage is incomplete, stale or not established".to_string(),
                            )
                        }
                    }
                } else {
                    let ids: Vec<String> = compatible
                        .iter()
                        .map(|entry| entry.fact.fact_id.clone())
                        .collect();
                    // Distinct typed facts can represent the same forwarding
                    // attachment (for example Linux self/master table context
                    // or exact replay). Uniqueness is evaluated at attachment
                    // identity, not merely serialized fact count; all supporting
                    // fact ids remain evidence.
                    let mut attachments: BTreeMap<(String, Option<String>), Vec<&MacEntry<'_>>> =
                        BTreeMap::new();
                    for mac_entry in &compatible {
                        attachments
                            .entry((
                                mac_interface(&mac_entry.payload),
                                mac_entry.payload.remote_vtep_ip.clone(),
                            ))
                            .or_default()
                            .push(*mac_entry);
                    }
                    if attachments.len() == 1 && truncated {
                        (
                            CrossTableOutcome::IndeterminateCandidateEvaluationTruncated,
                            ids,
                            format!(
                                "candidate evaluation bounded at {} of {total_candidates} scoped rows; uniqueness remains indeterminate",
                                candidate_limit
                            ),
                        )
                    } else if attachments.len() == 1 {
                        let representative = attachments
                            .values()
                            .next()
                            .and_then(|entries| entries.first())
                            .copied()
                            .expect("one nonempty attachment group");
                        let empty_context = DomainInterfaceContext::default();
                        let context_key = (
                            entry.fact.subject.device_id.clone(),
                            entry.fact.subject.scope.clone(),
                            entry.fact.subject.network_namespace.clone(),
                            domain.clone(),
                        );
                        let context = domain_contexts.get(&context_key).unwrap_or(&empty_context);
                        let comparison = comparable_forwarding_evidence(
                            entry.fact,
                            arp,
                            representative.fact,
                            &representative.payload,
                            context,
                        );
                        match comparison {
                            InterfaceComparison::Agree => (
                                CrossTableOutcome::Consistent,
                                ids,
                                "exactly one scoped forwarding-domain candidate agrees on comparable interface evidence".to_string(),
                            ),
                            InterfaceComparison::Conflict => (
                                CrossTableOutcome::ConflictingInterface,
                                ids,
                                "single scoped forwarding-domain candidate has contradictory comparable forwarding-interface evidence".to_string(),
                            ),
                            InterfaceComparison::NotComparable => (
                                CrossTableOutcome::ScopedCorrelationInterfaceNotComparable,
                                ids,
                                "exactly one scoped forwarding-domain candidate exists; logical and physical attachment evidence is not independently comparable, so interface agreement is not claimed".to_string(),
                            ),
                        }
                    } else {
                        (
                            CrossTableOutcome::MultipleCandidates,
                            ids,
                            "multiple distinct MAC observations exist for the same scoped forwarding-domain VLAN+MAC".to_string(),
                        )
                    }
                }
            }
        };
        if ids.len() > MAX_RESULT_EVIDENCE_REFS {
            let omitted = ids.len() - MAX_RESULT_EVIDENCE_REFS;
            ids.truncate(MAX_RESULT_EVIDENCE_REFS);
            reason.push_str(&format!(
                "; {omitted} additional candidate evidence references omitted by diagnostic bound"
            ));
        }
        results.push(CrossTableResult {
            arpnd_fact_id: entry.fact.fact_id.clone(),
            ip_address: arp.ip_address.clone(),
            neighbor_interface: arp.outgoing_interface.clone(),
            neighbor_forwarding_interface: forwarding,
            forwarding_domain: assignment_domain,
            vlan_id: assignment_vlan,
            mac_address: result_mac,
            outcome,
            mac_fact_ids: ids,
            domain_evidence_refs,
            coverage_evidence_refs,
            reason,
        });
    }
    results.sort_by(|a, b| {
        (
            &a.ip_address,
            &a.forwarding_domain,
            a.vlan_id,
            &a.mac_address,
            &a.arpnd_fact_id,
        )
            .cmp(&(
                &b.ip_address,
                &b.forwarding_domain,
                b.vlan_id,
                &b.mac_address,
                &b.arpnd_fact_id,
            ))
    });
    let mut outcome_counts = BTreeMap::new();
    for result in &results {
        *outcome_counts
            .entry(format!("{:?}", result.outcome))
            .or_insert(0) += 1;
    }
    Ok(CrossTableReport {
        as_of: as_of.to_rfc3339(),
        arpnd_facts_considered: results.len(),
        mac_facts_considered: macs.len(),
        invalid_arpnd_facts: invalid_arpnd,
        invalid_mac_facts: invalid_mac,
        excluded_arpnd_facts: excluded_arpnd,
        excluded_mac_facts: excluded_mac,
        invalid_domain_evidence,
        temporally_ineligible_domain_evidence,
        domain_indeterminate_arpnd_facts: domain_indeterminate_arpnd,
        domain_indeterminate_mac_facts: domain_indeterminate_mac,
        temporal_exclusions,
        candidate_evaluations_omitted,
        outcome_counts,
        results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_facts::ObservedField;
    use crate::interface_normalizer::{CaptureOutcome, CommandCapture, ObservationContext};
    use crate::neighbor_facts::{
        AddressFamily, MacEntryType, NeighborResolutionState, arpnd_attachment_entity_key,
        mac_forwarding_entity_key,
    };
    use crate::neighbor_normalizer::{
        ArpndObservation, ArpndSourceProfile, MacObservation, MacSourceProfile, normalize_arpnd,
        normalize_mac,
    };
    use crate::semantic_evidence::ScopedPopulationCoverage;
    use crate::typed_facts::{
        Completeness, FactPayload, FactSource, FactSubject, InterfaceIdentityKind,
        InterfaceSubjectIdentity, Lifecycle, Quality,
    };

    const OBSERVED: &str = "2026-01-01T00:00:00Z";
    const LATER: &str = "2026-01-01T00:05:00Z";
    const AS_OF: &str = "2026-01-01T00:01:00Z";
    const VALID_UNTIL: &str = "2026-01-01T00:10:00Z";
    const MAC: &str = "aa:bb:cc:dd:ee:ff";

    fn known<T>(value: T) -> ObservedField<T> {
        ObservedField::known(value)
    }

    fn unknown<T>() -> ObservedField<T> {
        ObservedField::unavailable(FieldAvailability::Unknown, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn typed_fact(
        kind: &str,
        device: &str,
        scope: Option<&str>,
        network_namespace: Option<&str>,
        vrf: Option<&str>,
        entity: &str,
        interface_identity: Option<InterfaceSubjectIdentity>,
        fields: serde_json::Value,
        sequence: u64,
        observed_at: &str,
    ) -> TypedFact {
        let mut fact = TypedFact {
            schema_version: crate::typed_facts::SCHEMA_VERSION,
            fact_id: String::new(),
            fact_kind: kind.to_string(),
            subject: FactSubject {
                device_id: device.into(),
                scope: scope.map(str::to_string),
                namespace: "interface".into(),
                network_namespace: network_namespace.map(str::to_string),
                entity: Some(entity.into()),
                vrf: vrf.map(str::to_string),
                afi_safi: None,
                plane: Some("operational".into()),
                interface_identity,
            },
            source: FactSource {
                collector_id: "collector-test".into(),
                source_kind: "cli_text".into(),
                capture_id: format!("capture-{sequence}"),
                session_epoch: 1,
                sequence,
                exporter_id: None,
            },
            observed_at: observed_at.into(),
            ingested_at: None,
            valid_from: Some(observed_at.into()),
            valid_until: Some(VALID_UNTIL.into()),
            payload: FactPayload {
                produced_by: "test".into(),
                parser_revision: "test-1".into(),
                mapping_revision: None,
                fields: Some(fields),
                state: None,
            },
            payload_sha256: String::new(),
            evidence_sha256: String::new(),
            quality: Quality {
                completeness: Completeness::Complete,
                missing_fields: vec![],
                notes: vec![],
            },
            trust: TrustClass::DeviceAsserted,
            authority: None,
            lifecycle: Lifecycle::Active,
            withdrawn_reason: None,
            superseded_by: None,
            lifecycle_changed_at: None,
            evidence: vec![EvidenceRef {
                kind: "test".into(),
                sha256: "a".repeat(64),
                locator: None,
            }],
        };
        fact.seal();
        fact
    }

    #[allow(clippy::too_many_arguments)]
    fn arp_fact_with_identity(
        device: &str,
        scope: Option<&str>,
        network_namespace: Option<&str>,
        vrf: Option<&str>,
        entity: &str,
        outgoing_interface: &str,
        physical_interface: Option<&str>,
        ip: &str,
        vlan: u32,
        interface_identity: Option<InterfaceSubjectIdentity>,
        sequence: u64,
        observed_at: &str,
    ) -> TypedFact {
        let family = if ip.parse::<std::net::IpAddr>().unwrap().is_ipv6() {
            AddressFamily::Ipv6
        } else {
            AddressFamily::Ipv4
        };
        let payload = ArpndNeighborPayload {
            ip_address: ip.into(),
            address_family: family,
            mac_address: known(MAC.to_string()),
            resolution_state: known(NeighborResolutionState::Reachable),
            raw_state: None,
            age_seconds: unknown(),
            outgoing_interface: outgoing_interface.into(),
            physical_interface: physical_interface.map(str::to_string),
            interface_vlan_id: known(vlan),
            remote: unknown(),
            protocol: None,
            router: None,
            source_interface_raw: Some(outgoing_interface.into()),
            neighbor_entity_key: Some(arpnd_attachment_entity_key(
                vrf,
                family,
                ip,
                entity,
                interface_identity
                    .as_ref()
                    .and_then(|identity| identity.subinterface_index),
            )),
            forwarding_domain: unknown(),
        };
        let fact = typed_fact(
            FACT_KIND_ARPND_NEIGHBOR,
            device,
            scope,
            network_namespace,
            vrf,
            entity,
            interface_identity,
            serde_json::to_value(payload).unwrap(),
            sequence,
            observed_at,
        );
        assert!(fact.validate().is_ok(), "{:?}", fact.validate());
        fact
    }

    #[allow(clippy::too_many_arguments)]
    fn arp_fact(
        device: &str,
        scope: Option<&str>,
        network_namespace: Option<&str>,
        vrf: Option<&str>,
        outgoing_interface: &str,
        physical_interface: Option<&str>,
        ip: &str,
        vlan: u32,
        sequence: u64,
        observed_at: &str,
    ) -> TypedFact {
        arp_fact_with_identity(
            device,
            scope,
            network_namespace,
            vrf,
            outgoing_interface,
            outgoing_interface,
            physical_interface,
            ip,
            vlan,
            None,
            sequence,
            observed_at,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn mac_fact(
        device: &str,
        scope: Option<&str>,
        network_namespace: Option<&str>,
        outgoing_interface: &str,
        vlan: u32,
        domain: &str,
        sequence: u64,
        observed_at: &str,
    ) -> TypedFact {
        let payload = MacForwardingPayload {
            mac_address: MAC.into(),
            vlan_id: known(vlan),
            outgoing_interface: outgoing_interface.into(),
            normalized_outgoing_interface: None,
            entry_type: known(MacEntryType::Dynamic),
            remote_vtep_ip: None,
            flags: vec![],
            move_count: unknown(),
            last_move_epoch_seconds: unknown(),
            bridge_domain: known(domain.to_string()),
            entry_contexts: vec![],
            forwarding_entity_key: Some(mac_forwarding_entity_key(Some(domain), Some(vlan), MAC)),
        };
        let fact = typed_fact(
            FACT_KIND_MAC_FORWARDING,
            device,
            scope,
            network_namespace,
            None,
            outgoing_interface,
            None,
            serde_json::to_value(payload).unwrap(),
            sequence,
            observed_at,
        );
        assert!(fact.validate().is_ok(), "{:?}", fact.validate());
        fact
    }

    fn domain_evidence(
        device: &str,
        scope: Option<&str>,
        network_namespace: Option<&str>,
        domain: &str,
        vlan: u32,
        vrf: Option<&str>,
        routed: &[&str],
        forwarding: &[&str],
    ) -> ForwardingDomainEvidence {
        ForwardingDomainEvidence {
            device_id: device.into(),
            scope: scope.map(str::to_string),
            network_namespace: network_namespace.map(str::to_string),
            domain_id: domain.into(),
            vlan_id: Some(vlan),
            membership_availability: None,
            membership_plane: None,
            vrf: vrf.map(str::to_string),
            routed_interfaces: routed.iter().map(|s| s.to_string()).collect(),
            forwarding_interfaces: forwarding.iter().map(|s| s.to_string()).collect(),
            routed_interface_keys: BTreeSet::new(),
            forwarding_interface_keys: BTreeSet::new(),
            interface_equivalents: BTreeMap::new(),
            interface_equivalent_keys: BTreeMap::new(),
            collector_id: Some("collector-test".into()),
            source_kind: Some("test/domain-binding".into()),
            capture_id: Some(format!("domain-capture-{domain}")),
            source_generation: Some(1),
            observation_time: Some(OBSERVED.into()),
            valid_from: Some(OBSERVED.into()),
            valid_until: Some(VALID_UNTIL.into()),
            lifecycle: Lifecycle::Active,
            evidence_refs: vec![EvidenceRef {
                kind: "test_domain_binding".into(),
                sha256: "b".repeat(64),
                locator: Some(domain.into()),
            }],
        }
    }

    fn policy() -> NeighborReconciliationPolicy {
        NeighborReconciliationPolicy {
            freshness: FreshnessPolicy::default(),
            max_capture_skew_seconds: 60,
            max_candidate_evaluations: MAX_CANDIDATE_EVALUATIONS,
        }
    }

    fn coverage(
        device: &str,
        scope: Option<&str>,
        network_namespace: Option<&str>,
        generation: u64,
        population: ScopedPopulationCoverage,
        capture_outcome: CaptureOutcome,
    ) -> ScopedCaptureCoverage {
        ScopedCaptureCoverage {
            device_id: device.into(),
            scope: scope.map(str::to_string),
            network_namespace: network_namespace.map(str::to_string),
            collector_id: "collector-test".into(),
            source_kind: "cli_text".into(),
            capture_id: format!("mac-capture-{generation}"),
            source_generation: Some(generation),
            acquisition_scope: "test/bridge-fdb".into(),
            covered_fact_kind: FACT_KIND_MAC_FORWARDING.into(),
            declared_scope: crate::semantic_evidence::AcquisitionScopeContract::CompleteScopedTable,
            capture_outcome,
            population_coverage: population,
            positive_empty_population: false,
            observation_time: Some(OBSERVED.into()),
            valid_until: Some(VALID_UNTIL.into()),
            observed_interfaces: BTreeSet::new(),
            observed_forwarding_domains: BTreeSet::from(["bridge-a".to_string()]),
            observed_vlan_ids: BTreeSet::from([100]),
            records_decoded: 0,
            malformed_records: 0,
            omitted_records: 0,
            rejected_records: 0,
            evidence_refs: vec![EvidenceRef {
                kind: "test_mac_coverage".into(),
                sha256: "c".repeat(64),
                locator: Some("mac-capture".into()),
            }],
        }
    }

    fn one_result(report: &CrossTableReport) -> &CrossTableResult {
        assert_eq!(report.results.len(), 1, "{:#?}", report.results);
        &report.results[0]
    }

    fn valid_match_fixture() -> (Vec<TypedFact>, Vec<ForwardingDomainEvidence>) {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            Some("port-channel10"),
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let mac = mac_fact(
            "leaf1",
            None,
            None,
            "port-channel10",
            100,
            "bridge-a",
            2,
            OBSERVED,
        );
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["port-channel10"],
        );
        (vec![arp, mac], vec![domain])
    }

    #[test]
    fn valid_evidence_matches_only_inside_established_forwarding_domain() {
        let (facts, domains) = valid_match_fixture();
        let report = reconcile_neighbor_tables(&facts, &[], &domains, &policy(), AS_OF).unwrap();
        let result = one_result(&report);
        assert_eq!(result.outcome, CrossTableOutcome::Consistent);
        assert_eq!(result.forwarding_domain.as_deref(), Some("bridge-a"));
        assert_eq!(result.mac_fact_ids.len(), 1);
        assert_eq!(report.invalid_arpnd_facts, 0);
        assert_eq!(report.invalid_mac_facts, 0);
    }

    #[test]
    fn same_vlan_and_mac_do_not_match_across_device_tenant_or_asic_scope() {
        let cases = [
            ("device", "leaf1", "leaf2", None, None, None, None),
            (
                "tenant",
                "leaf1",
                "leaf1",
                Some("tenant-a"),
                Some("tenant-b"),
                None,
                None,
            ),
            (
                "asic",
                "leaf1",
                "leaf1",
                None,
                None,
                Some("asic-0"),
                Some("asic-1"),
            ),
        ];
        for (label, arp_device, mac_device, arp_scope, mac_scope, arp_ns, mac_ns) in cases {
            let arp = arp_fact(
                arp_device,
                arp_scope,
                arp_ns,
                Some("red"),
                "Vlan100",
                Some("port-channel10"),
                "192.0.2.10",
                100,
                1,
                OBSERVED,
            );
            let mac = mac_fact(
                mac_device,
                mac_scope,
                mac_ns,
                "port-channel10",
                100,
                "bridge-a",
                2,
                OBSERVED,
            );
            let domain = domain_evidence(
                arp_device,
                arp_scope,
                arp_ns,
                "bridge-a",
                100,
                Some("red"),
                &["Vlan100"],
                &["port-channel10"],
            );
            let report =
                reconcile_neighbor_tables(&[arp, mac], &[], &[domain], &policy(), AS_OF).unwrap();
            let result = one_result(&report);
            assert_eq!(
                result.outcome,
                CrossTableOutcome::UnknownOutsideCoverage,
                "{label}: {}",
                result.reason
            );
            assert!(result.mac_fact_ids.is_empty(), "{label}");
            assert_eq!(report.mac_facts_considered, 1, "{label}");
        }
    }

    #[test]
    fn same_vlan_and_mac_do_not_match_across_bridge_domains() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            Some("port-channel10"),
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let mac = mac_fact(
            "leaf1",
            None,
            None,
            "port-channel10",
            100,
            "bridge-b",
            2,
            OBSERVED,
        );
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["port-channel10"],
        );
        let report =
            reconcile_neighbor_tables(&[arp, mac], &[], &[domain], &policy(), AS_OF).unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::UnknownOutsideCoverage
        );
    }

    #[test]
    fn same_vrf_alone_does_not_establish_bridge_domain_attachment() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("default"),
            "Ethernet1",
            None,
            "192.0.2.1",
            30,
            1,
            OBSERVED,
        );
        let mac = mac_fact(
            "leaf1",
            None,
            None,
            "Port-Channel1",
            30,
            "bridge-a",
            2,
            OBSERVED,
        );
        let evidence = [domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            30,
            Some("default"),
            &["Vlan30"],
            &["Port-Channel1"],
        )];
        let report =
            reconcile_neighbor_tables(&[arp, mac], &[], &evidence, &policy(), AS_OF).unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::IndeterminateMissingForwardingDomain
        );
    }

    #[test]
    fn missing_or_conflicting_domain_evidence_is_indeterminate_not_guessed() {
        let (facts, _domains) = valid_match_fixture();
        let report = reconcile_neighbor_tables(&facts, &[], &[], &policy(), AS_OF).unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::IndeterminateMissingForwardingDomain
        );

        let (facts, domains) = valid_match_fixture();
        let mut conflicting = domains.clone();
        conflicting.push(domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-b",
            100,
            Some("red"),
            &["Vlan100"],
            &["port-channel10"],
        ));
        let report =
            reconcile_neighbor_tables(&facts, &[], &conflicting, &policy(), AS_OF).unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::IndeterminateConflictingForwardingDomain
        );
    }

    #[test]
    fn same_ipv4_address_in_different_vrfs_remains_separately_scoped() {
        let red_arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            Some("Ethernet1"),
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let blue_arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("blue"),
            "Vlan200",
            Some("Ethernet2"),
            "192.0.2.10",
            200,
            2,
            OBSERVED,
        );
        let mac = mac_fact(
            "leaf1",
            None,
            None,
            "Ethernet1",
            100,
            "bridge-red",
            3,
            OBSERVED,
        );
        let domains = vec![
            domain_evidence(
                "leaf1",
                None,
                None,
                "bridge-red",
                100,
                Some("red"),
                &["Vlan100"],
                &["Ethernet1"],
            ),
            domain_evidence(
                "leaf1",
                None,
                None,
                "bridge-blue",
                200,
                Some("blue"),
                &["Vlan200"],
                &["Ethernet2"],
            ),
        ];
        let facts = vec![red_arp.clone(), blue_arp.clone(), mac];
        let report = reconcile_neighbor_tables(&facts, &[], &domains, &policy(), AS_OF).unwrap();
        assert_eq!(report.results.len(), 2);
        let red = report
            .results
            .iter()
            .find(|r| r.arpnd_fact_id == red_arp.fact_id)
            .unwrap();
        let blue = report
            .results
            .iter()
            .find(|r| r.arpnd_fact_id == blue_arp.fact_id)
            .unwrap();
        assert_eq!(red.outcome, CrossTableOutcome::Consistent);
        assert_eq!(blue.outcome, CrossTableOutcome::UnknownOutsideCoverage);
    }

    #[test]
    fn same_ipv6_link_local_on_different_interfaces_remains_distinct() {
        let first = arp_fact(
            "leaf1",
            None,
            None,
            None,
            "Ethernet1.100",
            None,
            "fe80::1",
            100,
            1,
            OBSERVED,
        );
        let second = arp_fact(
            "leaf1",
            None,
            None,
            None,
            "Ethernet2.100",
            None,
            "fe80::1",
            100,
            2,
            OBSERVED,
        );
        let mac = mac_fact(
            "leaf1",
            None,
            None,
            "Ethernet1",
            100,
            "bridge-a",
            3,
            OBSERVED,
        );
        let domains = vec![
            domain_evidence(
                "leaf1",
                None,
                None,
                "bridge-a",
                100,
                None,
                &["Ethernet1.100"],
                &["Ethernet1"],
            ),
            domain_evidence(
                "leaf1",
                None,
                None,
                "bridge-b",
                100,
                None,
                &["Ethernet2.100"],
                &["Ethernet2"],
            ),
        ];
        let facts = vec![first.clone(), second.clone(), mac];
        let report = reconcile_neighbor_tables(&facts, &[], &domains, &policy(), AS_OF).unwrap();
        assert_eq!(report.results.len(), 2);
        assert!(
            report
                .results
                .iter()
                .any(|r| r.arpnd_fact_id == first.fact_id
                    && r.outcome == CrossTableOutcome::ScopedCorrelationInterfaceNotComparable)
        );
        assert!(
            report
                .results
                .iter()
                .any(|r| r.arpnd_fact_id == second.fact_id
                    && r.outcome == CrossTableOutcome::UnknownOutsideCoverage)
        );
    }

    #[test]
    fn structured_subinterfaces_do_not_collapse_to_parent_identity() {
        let identity_100 = InterfaceSubjectIdentity {
            kind: InterfaceIdentityKind::Subinterface,
            parent_entity: Some("Ethernet1".into()),
            subinterface_index: Some(100),
            vendor_native_name: Some("Ethernet1.100".into()),
        };
        let identity_200 = InterfaceSubjectIdentity {
            kind: InterfaceIdentityKind::Subinterface,
            parent_entity: Some("Ethernet1".into()),
            subinterface_index: Some(200),
            vendor_native_name: Some("Ethernet1.200".into()),
        };
        let sub100 = arp_fact_with_identity(
            "leaf1",
            None,
            None,
            None,
            "Ethernet1",
            "Ethernet1.100",
            None,
            "192.0.2.10",
            100,
            Some(identity_100),
            1,
            OBSERVED,
        );
        let sub200 = arp_fact_with_identity(
            "leaf1",
            None,
            None,
            None,
            "Ethernet1",
            "Ethernet1.200",
            None,
            "192.0.2.10",
            200,
            Some(identity_200),
            2,
            OBSERVED,
        );
        assert_ne!(
            serde_json::from_value::<ArpndNeighborPayload>(sub100.payload.fields.clone().unwrap())
                .unwrap()
                .neighbor_entity_key,
            serde_json::from_value::<ArpndNeighborPayload>(sub200.payload.fields.clone().unwrap())
                .unwrap()
                .neighbor_entity_key
        );
        let mac = mac_fact(
            "leaf1",
            None,
            None,
            "Ethernet1",
            100,
            "bridge-a",
            3,
            OBSERVED,
        );
        let domains = vec![
            domain_evidence(
                "leaf1",
                None,
                None,
                "bridge-a",
                100,
                None,
                &["Ethernet1.100"],
                &["Ethernet1"],
            ),
            domain_evidence(
                "leaf1",
                None,
                None,
                "bridge-b",
                200,
                None,
                &["Ethernet1.200"],
                &["Ethernet1"],
            ),
        ];
        let facts = vec![sub100.clone(), sub200.clone(), mac];
        let report = reconcile_neighbor_tables(&facts, &[], &domains, &policy(), AS_OF).unwrap();
        assert_eq!(report.results.len(), 2);
        assert!(
            report
                .results
                .iter()
                .any(|r| r.arpnd_fact_id == sub100.fact_id
                    && r.outcome == CrossTableOutcome::ScopedCorrelationInterfaceNotComparable)
        );
        assert!(
            report
                .results
                .iter()
                .any(|r| r.arpnd_fact_id == sub200.fact_id
                    && r.outcome == CrossTableOutcome::UnknownOutsideCoverage)
        );
    }

    #[test]
    fn logical_svi_and_physical_port_difference_is_not_false_conflict() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            None,
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let mac = mac_fact(
            "leaf1",
            None,
            None,
            "Ethernet1",
            100,
            "bridge-a",
            2,
            OBSERVED,
        );
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["Ethernet1"],
        );
        let report =
            reconcile_neighbor_tables(&[arp, mac], &[], &[domain], &policy(), AS_OF).unwrap();
        let result = one_result(&report);
        assert_eq!(
            result.outcome,
            CrossTableOutcome::ScopedCorrelationInterfaceNotComparable
        );
        assert!(result.reason.contains("not independently comparable"));
        assert_ne!(result.outcome, CrossTableOutcome::Consistent);
    }

    #[test]
    fn explicit_forwarding_interface_conflict_requires_comparable_evidence() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            Some("Ethernet2"),
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let mac = mac_fact(
            "leaf1",
            None,
            None,
            "Ethernet1",
            100,
            "bridge-a",
            2,
            OBSERVED,
        );
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["Ethernet1", "Ethernet2"],
        );
        let report =
            reconcile_neighbor_tables(&[arp, mac], &[], &[domain], &policy(), AS_OF).unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::ConflictingInterface
        );
    }

    #[test]
    fn positively_established_lag_member_aggregate_relationship_is_not_conflict() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            Some("Ethernet1"),
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let mac = mac_fact(
            "leaf1",
            None,
            None,
            "port-channel1",
            100,
            "bridge-a",
            2,
            OBSERVED,
        );
        let mut domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["port-channel1", "Ethernet1"],
        );
        domain.interface_equivalents.insert(
            "port-channel1".into(),
            BTreeSet::from(["Ethernet1".to_string()]),
        );
        let report =
            reconcile_neighbor_tables(&[arp, mac], &[], &[domain], &policy(), AS_OF).unwrap();
        assert_eq!(one_result(&report).outcome, CrossTableOutcome::Consistent);
    }

    #[test]
    fn replayed_or_context_split_rows_for_one_attachment_are_not_multiple_candidates() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            Some("port-channel10"),
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let first = mac_fact(
            "leaf1",
            None,
            None,
            "port-channel10",
            100,
            "bridge-a",
            2,
            OBSERVED,
        );
        let second = mac_fact(
            "leaf1",
            None,
            None,
            "port-channel10",
            100,
            "bridge-a",
            3,
            OBSERVED,
        );
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["port-channel10"],
        );
        let report =
            reconcile_neighbor_tables(&[arp, first, second], &[], &[domain], &policy(), AS_OF)
                .unwrap();
        let result = one_result(&report);
        assert_eq!(result.outcome, CrossTableOutcome::Consistent);
        assert_eq!(result.mac_fact_ids.len(), 2);
    }

    #[test]
    fn capture_time_skew_produces_incompatible_observations_not_consistency() {
        let (mut facts, domains) = valid_match_fixture();
        facts[1] = mac_fact(
            "leaf1",
            None,
            None,
            "port-channel10",
            100,
            "bridge-a",
            2,
            LATER,
        );
        let report =
            reconcile_neighbor_tables(&facts, &[], &domains, &policy(), "2026-01-01T00:06:00Z")
                .unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::IncompatibleObservations
        );
    }

    #[test]
    fn unknown_or_stale_freshness_is_counted_and_excluded() {
        let (facts, domains) = valid_match_fixture();
        let mut stale_arp = facts[0].clone();
        stale_arp.valid_until = Some("2026-01-01T00:00:30Z".into());
        stale_arp.seal();
        let mut unknown_mac = facts[1].clone();
        unknown_mac.valid_until = None;
        unknown_mac.seal();
        let report =
            reconcile_neighbor_tables(&[stale_arp, unknown_mac], &[], &domains, &policy(), AS_OF)
                .unwrap();
        assert!(report.results.is_empty());
        assert_eq!(report.excluded_arpnd_facts, 1);
        assert_eq!(report.excluded_mac_facts, 1);
        assert_eq!(report.temporal_exclusions["Historical"], 1);
        assert_eq!(report.temporal_exclusions["UnknownFreshness"], 1);
    }

    #[test]
    fn invalid_or_wrong_plane_facts_cannot_contribute_to_matching() {
        let (facts, domains) = valid_match_fixture();
        let mut wrong_plane = facts[1].clone();
        wrong_plane.subject.plane = Some("configured".into());
        wrong_plane.seal();
        let report = reconcile_neighbor_tables(
            &[facts[0].clone(), wrong_plane],
            &[],
            &domains,
            &policy(),
            AS_OF,
        )
        .unwrap();
        assert_eq!(report.invalid_mac_facts, 1);
        assert_eq!(report.mac_facts_considered, 0);
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::UnknownOutsideCoverage
        );
    }

    #[test]
    fn complete_exactly_scoped_coverage_can_support_absence() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            Some("port-channel10"),
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["port-channel10"],
        );
        let cov = coverage(
            "leaf1",
            None,
            None,
            2,
            ScopedPopulationCoverage::Complete,
            CaptureOutcome::Complete,
        );
        let report =
            reconcile_neighbor_tables(&[arp], &[cov], &[domain], &policy(), AS_OF).unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::AbsentInDeclaredCoverage
        );
    }

    #[test]
    fn arpnd_coverage_cannot_establish_mac_table_absence() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            Some("port-channel10"),
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["port-channel10"],
        );
        let mut cov = coverage(
            "leaf1",
            None,
            None,
            2,
            ScopedPopulationCoverage::Complete,
            CaptureOutcome::Complete,
        );
        cov.covered_fact_kind = FACT_KIND_ARPND_NEIGHBOR.into();
        let report =
            reconcile_neighbor_tables(&[arp], &[cov], &[domain], &policy(), AS_OF).unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::UnknownOutsideCoverage
        );
    }

    #[test]
    fn newer_incomplete_coverage_masks_older_complete_coverage_in_same_source() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            Some("port-channel10"),
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["port-channel10"],
        );
        let older_complete = coverage(
            "leaf1",
            None,
            None,
            1,
            ScopedPopulationCoverage::Complete,
            CaptureOutcome::Complete,
        );
        let newer_incomplete = coverage(
            "leaf1",
            None,
            None,
            2,
            ScopedPopulationCoverage::Partial,
            CaptureOutcome::Complete,
        );
        let report = reconcile_neighbor_tables(
            &[arp],
            &[older_complete, newer_incomplete],
            &[domain],
            &policy(),
            AS_OF,
        )
        .unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::UnknownOutsideCoverage
        );
    }

    #[test]
    fn genuine_eos_same_device_captures_reconcile_with_explicitly_synthetic_domain_bindings() {
        // The ARP and MAC inputs are genuine pinned captures from the same EOS
        // device. EOS does not expose the required L2 domain binding in these
        // commands, so the three binding objects below are deliberately
        // synthetic test evidence; they are not presented as genuine capture
        // evidence.
        let arp_input =
            include_str!("../tests/fixtures/bn024/neighbor/eos-leaf02-show-arp-vrf-all.json");
        let mac_input = include_str!(
            "../tests/fixtures/bn024/neighbor/eos-leaf02-show-mac-address-table-unicast.json"
        );
        let mut arp_ctx = ObservationContext::minimal(
            "device-leaf02",
            "collector-test",
            "eos-arp-capture",
            "2021-06-07T00:00:00+00:00",
        );
        arp_ctx.session_epoch = 7;
        let mut mac_ctx = ObservationContext::minimal(
            "device-leaf02",
            "collector-test",
            "eos-mac-capture",
            "2021-06-07T00:00:20+00:00",
        );
        mac_ctx.session_epoch = 7;
        let arps = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::EosShowArpVrfAllJson,
            capture: CommandCapture::new(arp_input, CaptureOutcome::Complete),
            context: arp_ctx,
            interface_identities: None,
        });
        let macs = normalize_mac(&MacObservation {
            profile: MacSourceProfile::EosShowMacAddressTableUnicastJson,
            capture: CommandCapture::new(mac_input, CaptureOutcome::Complete),
            context: mac_ctx,
            interface_identities: None,
        });
        assert_eq!(arps.report.malformed_records, 0);
        assert_eq!(macs.report.malformed_records, 0);
        let mut domains = vec![
            domain_evidence(
                "device-leaf02",
                None,
                None,
                "eos-vlan-10",
                10,
                Some("evpn-vrf"),
                &["Vlan10"],
                &["Port-Channel3"],
            ),
            domain_evidence(
                "device-leaf02",
                None,
                None,
                "eos-vlan-30",
                30,
                Some("evpn-vrf"),
                &["Vlan30"],
                &["Port-Channel4", "Vxlan1"],
            ),
            domain_evidence(
                "device-leaf02",
                None,
                None,
                "eos-vlan-4094",
                4094,
                Some("default"),
                &["Vlan4094"],
                &["Port-Channel1"],
            ),
        ];
        for binding in &mut domains {
            binding.observation_time = Some("2021-06-07T00:00:10+00:00".into());
            binding.valid_from = Some("2021-06-07T00:00:10+00:00".into());
            binding.valid_until = Some("2021-06-07T00:10:00+00:00".into());
        }
        let facts: Vec<_> = arps
            .facts
            .iter()
            .chain(macs.facts.iter())
            .cloned()
            .collect();
        let policy = NeighborReconciliationPolicy {
            freshness: crate::semantic_evidence::FreshnessPolicy::observation_age(120),
            max_capture_skew_seconds: 60,
            max_candidate_evaluations: MAX_CANDIDATE_EVALUATIONS,
        };
        let report =
            reconcile_neighbor_tables(&facts, &[], &domains, &policy, "2021-06-07T00:00:30+00:00")
                .unwrap();
        assert_eq!(report.results.len(), 7);
        for ip in [
            "169.254.1.0",
            "172.16.1.101",
            "172.16.3.102",
            "172.16.3.202",
        ] {
            let result = report
                .results
                .iter()
                .find(|r| r.ip_address == ip)
                .unwrap_or_else(|| panic!("missing result for {ip}"));
            assert_eq!(
                result.outcome,
                CrossTableOutcome::Consistent,
                "{ip}: {}",
                result.reason
            );
            assert_eq!(result.mac_fact_ids.len(), 1, "{ip}");
        }
        for ip in ["10.0.0.21", "10.0.0.22", "10.255.2.1"] {
            let result = report
                .results
                .iter()
                .find(|r| r.ip_address == ip)
                .unwrap_or_else(|| panic!("missing result for {ip}"));
            assert_eq!(
                result.outcome,
                CrossTableOutcome::IndeterminateMissingForwardingDomain,
                "{ip}: routed physical/management interfaces must not be absorbed into an L2 domain merely by VRF"
            );
        }
    }

    #[test]
    fn contradictory_same_domain_vlan_assignments_are_order_independent() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            None,
            "Vlan100",
            None,
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let mut first = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            None,
            &["Vlan100"],
            &[],
        );
        first.vlan_id = Some(100);
        let mut second = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            None,
            &["Vlan100"],
            &[],
        );
        second.vlan_id = Some(200);
        for domains in [vec![first.clone(), second.clone()], vec![second, first]] {
            let report =
                reconcile_neighbor_tables(&[arp.clone()], &[], &domains, &policy(), AS_OF).unwrap();
            assert_eq!(
                one_result(&report).outcome,
                CrossTableOutcome::IndeterminateConflictingForwardingDomain,
                "same-domain contradictory VLAN evidence must never select by input order"
            );
        }
    }

    #[test]
    fn unknown_domain_vlan_is_refined_by_known_evidence_in_either_order() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            None,
            "Vlan100",
            None,
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let mut unknown_vlan = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            None,
            &["Vlan100"],
            &[],
        );
        unknown_vlan.vlan_id = None;
        let known_vlan = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            None,
            &["Vlan100"],
            &[],
        );
        for domains in [
            vec![unknown_vlan.clone(), known_vlan.clone()],
            vec![known_vlan, unknown_vlan],
        ] {
            let report =
                reconcile_neighbor_tables(&[arp.clone()], &[], &domains, &policy(), AS_OF).unwrap();
            let result = one_result(&report);
            assert_eq!(result.forwarding_domain.as_deref(), Some("bridge-a"));
            assert_eq!(result.vlan_id, Some(100));
            assert_eq!(
                result.outcome,
                CrossTableOutcome::UnknownOutsideCoverage,
                "compatible unknown→known refinement must be order independent"
            );
        }
    }

    #[test]
    fn direct_payload_assignment_contradicting_domain_evidence_is_indeterminate() {
        let mut arp = arp_fact(
            "leaf1",
            None,
            None,
            None,
            "Vlan100",
            None,
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let mut payload: ArpndNeighborPayload =
            serde_json::from_value(arp.payload.fields.clone().unwrap()).unwrap();
        payload.forwarding_domain = known("bridge-a".to_string());
        arp.payload.fields = Some(serde_json::to_value(payload).unwrap());
        arp.seal();
        assert!(arp.validate().is_ok());
        let mut contradictory = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            None,
            &["Vlan100"],
            &[],
        );
        contradictory.vlan_id = Some(200);
        let report =
            reconcile_neighbor_tables(&[arp], &[], &[contradictory], &policy(), AS_OF).unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::IndeterminateConflictingForwardingDomain
        );
    }

    #[test]
    fn stale_forwarding_domain_binding_cannot_establish_current_consistency() {
        let (facts, domains) = valid_match_fixture();
        let mut stale = domains[0].clone();
        stale.valid_until = Some("2026-01-01T00:00:30Z".into());
        let report = reconcile_neighbor_tables(&facts, &[], &[stale], &policy(), AS_OF).unwrap();
        assert_eq!(report.temporally_ineligible_domain_evidence, 1);
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::IndeterminateMissingForwardingDomain
        );
    }

    #[test]
    fn parent_only_domain_attachment_does_not_apply_to_numbered_subinterface() {
        let identity = InterfaceSubjectIdentity {
            kind: InterfaceIdentityKind::Subinterface,
            parent_entity: Some("Ethernet1".into()),
            subinterface_index: Some(100),
            vendor_native_name: Some("Ethernet1.100".into()),
        };
        let arp = arp_fact_with_identity(
            "leaf1",
            None,
            None,
            None,
            "Ethernet1",
            "Ethernet1.100",
            None,
            "192.0.2.10",
            100,
            Some(identity),
            1,
            OBSERVED,
        );
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            None,
            &["Ethernet1"],
            &["Ethernet1"],
        );
        let report = reconcile_neighbor_tables(&[arp], &[], &[domain], &policy(), AS_OF).unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::IndeterminateMissingForwardingDomain,
            "the canonical parent string is not child-interface identity evidence"
        );
    }

    #[test]
    fn base_interface_and_subinterface_zero_remain_distinct_in_comparison() {
        let base_identity = InterfaceSubjectIdentity {
            kind: InterfaceIdentityKind::Physical,
            parent_entity: None,
            subinterface_index: None,
            vendor_native_name: Some("Ethernet1".into()),
        };
        let subzero_identity = InterfaceSubjectIdentity {
            kind: InterfaceIdentityKind::Subinterface,
            parent_entity: Some("Ethernet1".into()),
            subinterface_index: Some(0),
            vendor_native_name: Some("Ethernet1.0".into()),
        };
        let arp = arp_fact_with_identity(
            "leaf1",
            None,
            None,
            None,
            "Ethernet1",
            "Ethernet1",
            None,
            "192.0.2.10",
            100,
            Some(base_identity),
            1,
            OBSERVED,
        );
        let mut macfact = mac_fact(
            "leaf1",
            None,
            None,
            "Ethernet1.0",
            100,
            "bridge-a",
            2,
            OBSERVED,
        );
        macfact.subject.entity = Some("Ethernet1".into());
        macfact.subject.interface_identity = Some(subzero_identity);
        macfact.seal();
        assert!(macfact.validate().is_ok());
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            None,
            &["Ethernet1"],
            &["Ethernet1.0"],
        );
        let report =
            reconcile_neighbor_tables(&[arp, macfact], &[], &[domain], &policy(), AS_OF).unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::ScopedCorrelationInterfaceNotComparable,
            "base and subinterface zero may correlate in-domain but must not prove interface agreement"
        );
    }

    #[test]
    fn lag_membership_does_not_make_sibling_members_identical() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            Some("Ethernet1"),
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let mac = mac_fact(
            "leaf1",
            None,
            None,
            "Ethernet2",
            100,
            "bridge-a",
            2,
            OBSERVED,
        );
        let mut domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["port-channel1", "Ethernet1", "Ethernet2"],
        );
        domain.interface_equivalents.insert(
            "port-channel1".into(),
            BTreeSet::from(["Ethernet1".to_string(), "Ethernet2".to_string()]),
        );
        let report =
            reconcile_neighbor_tables(&[arp, mac], &[], &[domain], &policy(), AS_OF).unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::ConflictingInterface,
            "two members of one aggregate are related to the aggregate, not identical to each other"
        );
    }

    #[test]
    fn failed_neighbor_with_residual_mac_is_not_resolved_adjacency() {
        let mut arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            Some("port-channel10"),
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let mut payload: ArpndNeighborPayload =
            serde_json::from_value(arp.payload.fields.clone().unwrap()).unwrap();
        payload.resolution_state = known(NeighborResolutionState::Failed);
        arp.payload.fields = Some(serde_json::to_value(payload).unwrap());
        arp.seal();
        assert!(arp.validate().is_ok());
        let mac = mac_fact(
            "leaf1",
            None,
            None,
            "port-channel10",
            100,
            "bridge-a",
            2,
            OBSERVED,
        );
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["port-channel10"],
        );
        let report =
            reconcile_neighbor_tables(&[arp, mac], &[], &[domain], &policy(), AS_OF).unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::IndeterminateNeighborUnresolved
        );
    }

    #[test]
    fn absence_result_carries_actual_coverage_evidence_reference() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            Some("port-channel10"),
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["port-channel10"],
        );
        let cov = coverage(
            "leaf1",
            None,
            None,
            1,
            ScopedPopulationCoverage::Complete,
            CaptureOutcome::Complete,
        );
        let report =
            reconcile_neighbor_tables(&[arp], &[cov], &[domain], &policy(), AS_OF).unwrap();
        let result = one_result(&report);
        assert_eq!(result.outcome, CrossTableOutcome::AbsentInDeclaredCoverage);
        assert_eq!(result.coverage_evidence_refs.len(), 1);
        assert_eq!(result.coverage_evidence_refs[0].kind, "test_mac_coverage");
        assert!(!result.domain_evidence_refs.is_empty());
    }

    #[test]
    fn exact_duplicate_domain_assignment_is_replay_not_conflict() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            None,
            "Vlan100",
            None,
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let first = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            None,
            &["Vlan100"],
            &[],
        );
        let duplicate = first.clone();
        for domains in [
            vec![first.clone(), duplicate.clone()],
            vec![duplicate, first],
        ] {
            let report =
                reconcile_neighbor_tables(&[arp.clone()], &[], &domains, &policy(), AS_OF).unwrap();
            let result = one_result(&report);
            assert_eq!(result.forwarding_domain.as_deref(), Some("bridge-a"));
            assert_eq!(result.vlan_id, Some(100));
            assert_eq!(result.outcome, CrossTableOutcome::UnknownOutsideCoverage);
        }
    }

    #[test]
    fn direct_mac_bridge_assignment_contradicting_domain_vlan_is_indeterminate() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            Some("port-channel10"),
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let macfact = mac_fact(
            "leaf1",
            None,
            None,
            "port-channel10",
            100,
            "bridge-a",
            2,
            OBSERVED,
        );
        let mut contradictory = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["port-channel10"],
        );
        contradictory.vlan_id = Some(200);
        let payload: MacForwardingPayload =
            serde_json::from_value(macfact.payload.fields.clone().unwrap()).unwrap();
        assert_eq!(
            assign_mac_domain(&macfact, &payload, std::slice::from_ref(&contradictory)),
            DomainAssignment::Conflicting,
            "same-domain VLAN contradiction must not be filtered as inapplicable trunk membership"
        );
        let report =
            reconcile_neighbor_tables(&[arp, macfact], &[], &[contradictory], &policy(), AS_OF)
                .unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::IndeterminateConflictingForwardingDomain
        );
    }

    #[test]
    fn distinct_numbered_subinterfaces_do_not_agree_through_shared_parent() {
        let identity_100 = InterfaceSubjectIdentity {
            kind: InterfaceIdentityKind::Subinterface,
            parent_entity: Some("Ethernet1".into()),
            subinterface_index: Some(100),
            vendor_native_name: Some("Ethernet1.100".into()),
        };
        let identity_200 = InterfaceSubjectIdentity {
            kind: InterfaceIdentityKind::Subinterface,
            parent_entity: Some("Ethernet1".into()),
            subinterface_index: Some(200),
            vendor_native_name: Some("Ethernet1.200".into()),
        };
        let arp = arp_fact_with_identity(
            "leaf1",
            None,
            None,
            None,
            "Ethernet1",
            "Ethernet1.100",
            None,
            "192.0.2.10",
            100,
            Some(identity_100),
            1,
            OBSERVED,
        );
        let mut macfact = mac_fact(
            "leaf1",
            None,
            None,
            "Ethernet1.200",
            100,
            "bridge-a",
            2,
            OBSERVED,
        );
        macfact.subject.entity = Some("Ethernet1".into());
        macfact.subject.interface_identity = Some(identity_200);
        macfact.seal();
        assert!(macfact.validate().is_ok());
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            None,
            &["Ethernet1.100"],
            &["Ethernet1.100", "Ethernet1.200"],
        );
        let report =
            reconcile_neighbor_tables(&[arp, macfact], &[], &[domain], &policy(), AS_OF).unwrap();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::ConflictingInterface,
            "two distinct children of Ethernet1 share a canonical parent string but are not the same interface"
        );
    }

    #[test]
    fn repeated_arp_rows_do_not_reprocess_full_concentrated_candidate_bucket() {
        let mut facts = Vec::new();
        for i in 0..128u64 {
            facts.push(arp_fact(
                "leaf1",
                None,
                None,
                Some("red"),
                "Vlan100",
                Some("port-channel10"),
                &format!("192.0.2.{i}"),
                100,
                1 + i,
                OBSERVED,
            ));
        }
        for i in 0..256u64 {
            facts.push(mac_fact(
                "leaf1",
                None,
                None,
                "port-channel10",
                100,
                "bridge-a",
                1_000 + i,
                OBSERVED,
            ));
        }
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["port-channel10"],
        );
        let started = std::time::Instant::now();
        let report = reconcile_neighbor_tables(&facts, &[], &[domain], &policy(), AS_OF).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(report.results.len(), 128);
        assert!(
            report
                .results
                .iter()
                .all(|r| r.outcome == CrossTableOutcome::IndeterminateCandidateEvaluationTruncated)
        );
        assert_eq!(report.candidate_evaluations_omitted, 128 * 192);
        println!(
            "neighbor_reconciliation_repeated_concentrated_bucket_perf arps=128 candidates_per_key=256 elapsed_ms={:.3}",
            elapsed.as_secs_f64() * 1000.0
        );
    }

    #[test]
    fn candidate_truncation_is_indeterminate_and_counted_not_false_consistency() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            Some("port-channel10"),
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let mut facts = vec![arp];
        for i in 0..70u64 {
            facts.push(mac_fact(
                "leaf1",
                None,
                None,
                &format!("Ethernet{i}"),
                100,
                "bridge-a",
                10 + i,
                if i == 0 { OBSERVED } else { LATER },
            ));
        }
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["port-channel10"],
        );
        let bounded = NeighborReconciliationPolicy {
            max_candidate_evaluations: 1,
            ..policy()
        };
        let report =
            reconcile_neighbor_tables(&facts, &[], &[domain], &bounded, "2026-01-01T00:06:00Z")
                .unwrap();
        let result = one_result(&report);
        assert_eq!(
            result.outcome,
            CrossTableOutcome::IndeterminateCandidateEvaluationTruncated
        );
        assert_eq!(report.candidate_evaluations_omitted, 69);
    }

    #[test]
    fn concentrated_candidate_key_scale_is_bounded_and_reported() {
        let arp = arp_fact(
            "leaf1",
            None,
            None,
            Some("red"),
            "Vlan100",
            Some("port-channel10"),
            "192.0.2.10",
            100,
            1,
            OBSERVED,
        );
        let mut facts = vec![arp];
        // Concentrate one VLAN+MAC key with repeated evidence for the same
        // attachment. This is the adversarial index shape that must not clone
        // or evaluate an unbounded candidate vector once per ARP row.
        for i in 0..256u64 {
            facts.push(mac_fact(
                "leaf1",
                None,
                None,
                "port-channel10",
                100,
                "bridge-a",
                10 + i,
                OBSERVED,
            ));
        }
        let domain = domain_evidence(
            "leaf1",
            None,
            None,
            "bridge-a",
            100,
            Some("red"),
            &["Vlan100"],
            &["port-channel10"],
        );
        let started = std::time::Instant::now();
        let report = reconcile_neighbor_tables(&facts, &[], &[domain], &policy(), AS_OF).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(
            one_result(&report).outcome,
            CrossTableOutcome::IndeterminateCandidateEvaluationTruncated
        );
        assert_eq!(report.candidate_evaluations_omitted, 192);
        println!(
            "neighbor_reconciliation_concentrated_key_perf candidates=256 evaluated_max={} elapsed_ms={:.3}",
            policy().max_candidate_evaluations,
            elapsed.as_secs_f64() * 1000.0
        );
    }

    #[test]
    #[ignore = "synthetic scaling evidence; run explicitly"]
    fn synthetic_cross_table_scaling_100_1000_10000() {
        for n in [100usize, 1_000, 10_000] {
            let mut facts = Vec::with_capacity(n * 2);
            for i in 0..n {
                let mac = format!(
                    "02:00:{:02x}:{:02x}:{:02x}:{:02x}",
                    (i >> 24) & 0xff,
                    (i >> 16) & 0xff,
                    (i >> 8) & 0xff,
                    i & 0xff,
                );
                let ip = format!("10.{}.{}.{}", (i >> 16) & 0xff, (i >> 8) & 0xff, i & 0xff);
                let mut arp = arp_fact(
                    "leaf1",
                    None,
                    None,
                    Some("red"),
                    "Vlan100",
                    Some("port-channel10"),
                    &ip,
                    100,
                    1 + i as u64,
                    OBSERVED,
                );
                let mut arp_payload: ArpndNeighborPayload =
                    serde_json::from_value(arp.payload.fields.clone().unwrap()).unwrap();
                arp_payload.mac_address = known(mac.clone());
                arp.payload.fields = Some(serde_json::to_value(arp_payload).unwrap());
                arp.seal();
                assert!(arp.validate().is_ok());

                let mut macfact = mac_fact(
                    "leaf1",
                    None,
                    None,
                    "port-channel10",
                    100,
                    "bridge-a",
                    20_000 + i as u64,
                    OBSERVED,
                );
                let mut mac_payload: MacForwardingPayload =
                    serde_json::from_value(macfact.payload.fields.clone().unwrap()).unwrap();
                mac_payload.mac_address = mac.clone();
                mac_payload.forwarding_entity_key =
                    Some(mac_forwarding_entity_key(Some("bridge-a"), Some(100), &mac));
                macfact.payload.fields = Some(serde_json::to_value(mac_payload).unwrap());
                macfact.seal();
                assert!(macfact.validate().is_ok());
                facts.push(arp);
                facts.push(macfact);
            }
            let domain = domain_evidence(
                "leaf1",
                None,
                None,
                "bridge-a",
                100,
                Some("red"),
                &["Vlan100"],
                &["port-channel10"],
            );
            let start = std::time::Instant::now();
            let report =
                reconcile_neighbor_tables(&facts, &[], &[domain], &policy(), AS_OF).unwrap();
            let elapsed = start.elapsed();
            let consistent = report
                .results
                .iter()
                .filter(|r| r.outcome == CrossTableOutcome::Consistent)
                .count();
            assert_eq!(consistent, n);
            println!(
                "synthetic neighbor reconciliation observations={} results={} elapsed_ms={}",
                n * 2,
                report.results.len(),
                elapsed.as_millis()
            );
        }
    }
}
