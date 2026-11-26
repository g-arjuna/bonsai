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
use crate::neighbor_facts::{
    ArpndNeighborPayload, FACT_KIND_ARPND_NEIGHBOR, FACT_KIND_MAC_FORWARDING, MacForwardingPayload,
};
use crate::semantic_evidence::{
    CaptureTimeCompatibility, EvidenceFreshness, ExpectedFactContract, FreshnessPolicy,
    ScopedCaptureCoverage, capture_time_compatibility, fact_freshness, select_usable_coverage,
    validate_fact_envelope,
};
use crate::typed_facts::{EvidenceRef, TrustClass, TypedFact};

pub const MAX_RECONCILIATION_INPUT_FACTS: usize = 100_000;
pub const MAX_RECONCILIATION_RESULTS: usize = 50_000;
pub const MAX_RESULT_EVIDENCE_REFS: usize = 64;
pub const MAX_CANDIDATE_EVALUATIONS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossTableOutcome {
    /// Exactly one applicable MAC observation exists in the same established
    /// forwarding domain and no independently comparable interface evidence
    /// contradicts it. Logical SVI ↔ physical-port differences are not
    /// misreported as contradictions.
    Consistent,
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
    /// Network instance explicitly associated with this domain, when known.
    pub vrf: Option<String>,
    /// Logical/routed interfaces (for example an SVI) attached to the domain.
    pub routed_interfaces: BTreeSet<String>,
    /// Physical/bridged forwarding interfaces attached to the domain.
    pub forwarding_interfaces: BTreeSet<String>,
    /// Positively established interface-equivalence evidence inside this
    /// forwarding domain (for example LAG aggregate ↔ member or tunnel
    /// endpoint aliases). This is never inferred from spelling or case.
    pub interface_equivalents: BTreeMap<String, BTreeSet<String>>,
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

fn exact_arp_interface_names(fact: &TypedFact, payload: &ArpndNeighborPayload) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Some(entity) = &fact.subject.entity {
        names.insert(entity.clone());
    }
    names.insert(payload.outgoing_interface.clone());
    if let Some(native) = fact
        .subject
        .interface_identity
        .as_ref()
        .and_then(|i| i.vendor_native_name.clone())
    {
        names.insert(native);
    }
    names
}

fn exact_mac_interface_names(fact: &TypedFact, payload: &MacForwardingPayload) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Some(entity) = &fact.subject.entity {
        names.insert(entity.clone());
    }
    names.insert(payload.outgoing_interface.clone());
    names.insert(mac_interface(payload));
    if let Some(native) = fact
        .subject
        .interface_identity
        .as_ref()
        .and_then(|i| i.vendor_native_name.clone())
    {
        names.insert(native);
    }
    names
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DomainAssignment {
    Established { domain: String, vlan: Option<u32> },
    Indeterminate,
    Conflicting,
}

fn unique_assignment(
    direct: Option<(String, Option<u32>)>,
    matches: Vec<&ForwardingDomainEvidence>,
    fallback_vlan: Option<u32>,
) -> DomainAssignment {
    let mut domains = BTreeMap::<String, Option<u32>>::new();
    if let Some((domain, vlan)) = direct {
        domains.insert(domain, vlan.or(fallback_vlan));
    }
    for evidence in matches {
        domains
            .entry(evidence.domain_id.clone())
            .or_insert(evidence.vlan_id.or(fallback_vlan));
    }
    match domains.len() {
        0 => DomainAssignment::Indeterminate,
        1 => {
            let (domain, vlan) = domains.into_iter().next().expect("one domain");
            DomainAssignment::Established { domain, vlan }
        }
        _ => DomainAssignment::Conflicting,
    }
}

fn assign_arp_domain(
    fact: &TypedFact,
    payload: &ArpndNeighborPayload,
    evidence: &[ForwardingDomainEvidence],
) -> DomainAssignment {
    let vlan = known_vlan(payload);
    let names = exact_arp_interface_names(fact, payload);
    let direct = (payload.forwarding_domain.state == FieldAvailability::Known)
        .then_some(payload.forwarding_domain.value.clone())
        .flatten()
        .map(|domain| (domain, vlan));
    let matches = evidence
        .iter()
        .filter(|ev| same_scope(fact, ev))
        .filter(|ev| {
            if let (Some(ev_vlan), Some(arp_vlan)) = (ev.vlan_id, vlan)
                && ev_vlan != arp_vlan
            {
                return false;
            }
            if ev.vrf.is_some() && fact.subject.vrf.is_some() && ev.vrf != fact.subject.vrf {
                return false;
            }
            // A VRF-to-L2 association corroborates routed-interface evidence,
            // but it is not sufficient by itself: ordinary routed interfaces
            // can coexist in the same network instance without belonging to
            // the bridge domain. Require the observed logical/L3 attachment to
            // be independently present in the domain evidence.
            !ev.routed_interfaces.is_disjoint(&names)
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
    let names = exact_mac_interface_names(fact, payload);
    let direct = known_bridge_domain(payload).map(|domain| (domain, vlan));
    let matches = evidence
        .iter()
        .filter(|ev| same_scope(fact, ev))
        .filter(|ev| {
            if let (Some(ev_vlan), Some(mac_vlan)) = (ev.vlan_id, vlan)
                && ev_vlan != mac_vlan
            {
                return false;
            }
            !ev.forwarding_interfaces.is_disjoint(&names) || ev.domain_id == direct_domain(&direct)
        })
        .collect();
    unique_assignment(direct, matches, vlan)
}

fn direct_domain(direct: &Option<(String, Option<u32>)>) -> String {
    direct
        .as_ref()
        .map(|(domain, _)| domain.clone())
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
    device: &str,
    scope: &Scope,
    namespace: &Namespace,
    domain: &str,
    vlan: Option<u32>,
) -> bool {
    if coverage.device_id != device
        || &coverage.scope != scope
        || &coverage.network_namespace != namespace
        || coverage.covered_fact_kind != FACT_KIND_MAC_FORWARDING
        || !coverage.covers_declared_scope
    {
        return false;
    }
    // Explicit domain/VLAN membership is narrowing evidence when present. An
    // empty set is valid for an all-empty complete capture and does not void
    // the declared whole-table scope.
    if !coverage.covered_forwarding_domains.is_empty()
        && !coverage.covered_forwarding_domains.contains(domain)
    {
        return false;
    }
    if let Some(vlan) = vlan
        && !coverage.covered_vlan_ids.is_empty()
        && !coverage.covered_vlan_ids.contains(&vlan)
    {
        return false;
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InterfaceComparison {
    Agree,
    Conflict,
    NotComparable,
}

fn interfaces_related(
    equivalents: &BTreeMap<String, BTreeSet<String>>,
    left: &str,
    right: &str,
) -> bool {
    equivalents.get(left).is_some_and(|set| set.contains(right))
        || equivalents.get(right).is_some_and(|set| set.contains(left))
}

fn any_related_interface(
    equivalents: &BTreeMap<String, BTreeSet<String>>,
    left: &BTreeSet<String>,
    right: &BTreeSet<String>,
) -> bool {
    left.iter()
        .any(|l| right.iter().any(|r| interfaces_related(equivalents, l, r)))
}

fn comparable_forwarding_evidence(
    arp_fact: &TypedFact,
    arp: &ArpndNeighborPayload,
    mac_fact: &TypedFact,
    mac: &MacForwardingPayload,
    forwarding_interfaces: &BTreeSet<String>,
    interface_equivalents: &BTreeMap<String, BTreeSet<String>>,
) -> InterfaceComparison {
    let mac_names = exact_mac_interface_names(mac_fact, mac);
    let arp_names = exact_arp_interface_names(arp_fact, arp);
    if !arp_names.is_disjoint(&mac_names)
        || any_related_interface(interface_equivalents, &arp_names, &mac_names)
    {
        return InterfaceComparison::Agree;
    }
    if let Some(physical) = &arp.physical_interface {
        if mac_names.contains(physical)
            || mac_names
                .iter()
                .any(|mac_name| interfaces_related(interface_equivalents, physical, mac_name))
        {
            return InterfaceComparison::Agree;
        }
        // The source explicitly supplied a forwarding/physical attachment;
        // that is comparable evidence even when the logical SVI differs.
        return InterfaceComparison::Conflict;
    }
    if arp_names.iter().any(|n| forwarding_interfaces.contains(n)) {
        return InterfaceComparison::Conflict;
    }
    InterfaceComparison::NotComparable
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

    let valid_domains: Vec<ForwardingDomainEvidence> = forwarding_domains
        .iter()
        .filter(|ev| {
            !ev.device_id.is_empty()
                && !ev.domain_id.is_empty()
                && ev.vlan_id.is_none_or(|v| v <= 4095)
        })
        .cloned()
        .collect();
    let invalid_domain_evidence = forwarding_domains.len() - valid_domains.len();

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
        let DomainAssignment::Established { domain, vlan } = &entry.assignment else {
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
        let (assignment_domain, assignment_vlan) = match &entry.assignment {
            DomainAssignment::Established {
                domain,
                vlan: domain_vlan,
            } => (Some(domain.clone()), (*domain_vlan).or(vlan)),
            _ => (None, vlan),
        };
        let (outcome, mut ids, mut reason) = if mac.is_none() {
            (
                CrossTableOutcome::IndeterminateMissingMac,
                vec![],
                "neighbor evidence does not establish a valid peer MAC".to_string(),
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
                let mut candidates = mac_index.get(&key).cloned().unwrap_or_default();
                candidates.sort_by(|a, b| macs[*a].fact.fact_id.cmp(&macs[*b].fact.fact_id));
                candidates.dedup();
                let total_candidates = candidates.len();
                if candidates.len() > policy.max_candidate_evaluations {
                    candidate_evaluations_omitted +=
                        candidates.len() - policy.max_candidate_evaluations;
                    candidates.truncate(policy.max_candidate_evaluations);
                }
                let mut compatible = Vec::new();
                let mut incompatible = 0usize;
                for idx in candidates {
                    let mac_entry = &macs[idx];
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
                let truncated = total_candidates > policy.max_candidate_evaluations;
                if compatible.is_empty() {
                    if truncated {
                        (
                            CrossTableOutcome::IndeterminateCandidateEvaluationTruncated,
                            vec![],
                            format!(
                                "candidate evaluation bounded at {} of {total_candidates} scoped rows; absence and temporal compatibility remain indeterminate",
                                policy.max_candidate_evaluations
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
                            coverage_applies(
                                cov,
                                &entry.fact.subject.device_id,
                                &entry.fact.subject.scope,
                                &entry.fact.subject.network_namespace,
                                &domain,
                                vlan_key,
                            )
                        });
                        if select_usable_coverage(applicable, as_of, policy.freshness).is_some() {
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
                                policy.max_candidate_evaluations
                            ),
                        )
                    } else if attachments.len() == 1 {
                        let representative = attachments
                            .values()
                            .next()
                            .and_then(|entries| entries.first())
                            .copied()
                            .expect("one nonempty attachment group");
                        let domain_forwarding_interfaces: BTreeSet<String> = valid_domains
                            .iter()
                            .filter(|ev| {
                                ev.device_id == entry.fact.subject.device_id
                                    && ev.scope == entry.fact.subject.scope
                                    && ev.network_namespace == entry.fact.subject.network_namespace
                                    && ev.domain_id == domain
                            })
                            .flat_map(|ev| ev.forwarding_interfaces.iter().cloned())
                            .collect();
                        let mut interface_equivalents: BTreeMap<String, BTreeSet<String>> =
                            BTreeMap::new();
                        for ev in valid_domains.iter().filter(|ev| {
                            ev.device_id == entry.fact.subject.device_id
                                && ev.scope == entry.fact.subject.scope
                                && ev.network_namespace == entry.fact.subject.network_namespace
                                && ev.domain_id == domain
                        }) {
                            for (interface, related) in &ev.interface_equivalents {
                                interface_equivalents
                                    .entry(interface.clone())
                                    .or_default()
                                    .extend(related.iter().cloned());
                            }
                        }
                        let comparison = comparable_forwarding_evidence(
                            entry.fact,
                            arp,
                            representative.fact,
                            &representative.payload,
                            &domain_forwarding_interfaces,
                            &interface_equivalents,
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
                                CrossTableOutcome::Consistent,
                                ids,
                                "exactly one scoped forwarding-domain candidate exists; logical and physical attachment evidence is not independently comparable, so no interface contradiction is claimed".to_string(),
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
            vrf: vrf.map(str::to_string),
            routed_interfaces: routed.iter().map(|s| s.to_string()).collect(),
            forwarding_interfaces: forwarding.iter().map(|s| s.to_string()).collect(),
            interface_equivalents: BTreeMap::new(),
            evidence_refs: vec![],
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
            capture_outcome,
            population_coverage: population,
            covers_declared_scope: true,
            observation_time: Some(OBSERVED.into()),
            valid_until: Some(VALID_UNTIL.into()),
            covered_interfaces: BTreeSet::new(),
            covered_forwarding_domains: BTreeSet::from(["bridge-a".to_string()]),
            covered_vlan_ids: BTreeSet::from([100]),
            records_decoded: 0,
            malformed_records: 0,
            omitted_records: 0,
            rejected_records: 0,
            evidence_refs: vec![],
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
        assert!(report.results.iter().any(
            |r| r.arpnd_fact_id == first.fact_id && r.outcome == CrossTableOutcome::Consistent
        ));
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
        assert!(report.results.iter().any(
            |r| r.arpnd_fact_id == sub100.fact_id && r.outcome == CrossTableOutcome::Consistent
        ));
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
        assert_eq!(result.outcome, CrossTableOutcome::Consistent);
        assert!(result.reason.contains("not independently comparable"));
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
        let domains = vec![
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
