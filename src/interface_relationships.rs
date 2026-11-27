//! BN-024-06 capability-led relationship assessment slice.
//!
//! Consumes typed `interface.state` facts and answers bounded operational
//! questions about LAG, bridge, parent/master and VRF evidence without
//! inventing missing relationships. This is deliberately a pure analysis
//! module: it performs no graph writes, no acquisition and no device access.
//! Operator/API exposure is deferred to the BN-030 interface workspace work.
//!
//! Input contract: one call may contain evidence for multiple devices or
//! scopes, but each device/scope/network-namespace is analyzed per coherent
//! observation batch (collector, source kind, exporter, capture id and
//! session epoch). Facts outside the operational plane, expired facts and
//! non-active lifecycle states are explicit input issues rather than current
//! evidence. Multiple unreconciled batches for the same scope are preserved
//! as separate evidence and can never produce a merged/passing verdict.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::interface_facts::{
    FACT_KIND_INTERFACE_STATE, FieldAvailability, InterfaceStatePayload, InterfaceType, MasterKind,
    OperState,
};
use crate::typed_facts::{Completeness, InterfaceIdentityKind, Lifecycle, PayloadState, TypedFact};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentVerdict {
    Pass,
    Fail,
    Unknown,
    NotApplicable,
}

/// Reusable semantic interface entity key used by this analyzer.
///
/// This is not a second identity authority: it is exactly the typed subject's
/// canonical device, identity scope, ASIC/network namespace and canonical
/// interface entity, refined only by the structured identity kind and numeric
/// subinterface axis already present on `TypedFact`. Native display names,
/// interface classification and parent relationships are evidence, not key
/// material. In particular, a physical/base interface, subinterface zero and
/// numbered subinterfaces remain distinct even though numbered subinterfaces
/// use the canonical parent as `subject.entity`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct InterfaceEntityKey {
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_namespace: Option<String>,
    pub canonical_interface: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_kind: Option<InterfaceIdentityKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subinterface_index: Option<u32>,
}

impl InterfaceEntityKey {
    pub fn from_fact(fact: &TypedFact) -> Option<Self> {
        let canonical_interface = fact.subject.entity.clone()?;
        let identity = &fact.subject.interface_identity;
        Some(Self {
            device_id: fact.subject.device_id.clone(),
            scope: fact.subject.scope.clone(),
            network_namespace: fact.subject.network_namespace.clone(),
            canonical_interface,
            identity_kind: identity.as_ref().map(|i| i.kind),
            subinterface_index: identity.as_ref().and_then(|i| i.subinterface_index),
        })
    }

    fn is_subinterface(&self) -> bool {
        self.identity_kind == Some(InterfaceIdentityKind::Subinterface)
    }

    fn is_base_target_candidate(&self) -> bool {
        !self.is_subinterface()
    }

    fn label(&self) -> String {
        match (self.identity_kind, self.subinterface_index) {
            (Some(InterfaceIdentityKind::Subinterface), Some(index)) => {
                format!("{}#subinterface-{}", self.canonical_interface, index)
            }
            (Some(kind), _) => format!(
                "{}#{}",
                self.canonical_interface,
                format!("{kind:?}").to_ascii_lowercase()
            ),
            (None, _) => format!("{}#unrefined", self.canonical_interface),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservationBatchBoundary {
    pub collector_id: String,
    pub source_kind: String,
    pub capture_id: String,
    pub session_epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exporter_id: Option<String>,
    pub fact_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_min: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_max: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at_end: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputContractStatus {
    CoherentObservationBatch,
    MultipleUnreconciledBatches,
    DegradedObservationBatch,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum InputIssueCode {
    InvalidFact,
    PayloadAbsent,
    PayloadUndecodable,
    MissingEntity,
    NonOperationalPlane,
    NonCurrentLifecycle,
    ExpiredBeforeObservation,
    DegradedPayloadState,
    MixedObservationBatch,
    ConflictingObservations,
    ScopeCoverageIncomplete,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InputIssue {
    pub code: InputIssueCode,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fact_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipTargetResolution {
    ResolvedBaseInterface,
    ResolvedLogicalInterface,
    ResolvedSubinterface,
    ResolvedUnrefinedInterface,
    AmbiguousStructuredTarget,
    MissingTarget,
    ConflictingTargetEvidence,
}

impl RelationshipTargetResolution {
    fn resolved(self) -> bool {
        matches!(
            self,
            Self::ResolvedBaseInterface
                | Self::ResolvedLogicalInterface
                | Self::ResolvedSubinterface
                | Self::ResolvedUnrefinedInterface
        )
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipIssueCode {
    ConflictingEntityObservations,
    UnavailableForwardMemberList,
    IncompatibleLagTargetType,
    SubinterfaceLagTarget,
    SelfRelationship,
    RelationshipCycle,
    ConflictingVrfAttachment,
    MissingForwardMember,
    MissingReverseMemberEvidence,
    UnexpectedReverseMember,
    AmbiguousRelationshipTarget,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelationshipIssue {
    pub code: RelationshipIssueCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<InterfaceEntityKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fact_ids: Vec<String>,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelationshipEdge {
    /// Canonical interface name axis. This display convenience is not the
    /// complete identity; consumers must use `interface_key` for identity.
    pub interface: String,
    pub interface_key: InterfaceEntityKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_display: Option<String>,
    /// Relationship target exactly as reported by the source.
    pub target: String,
    pub kind: String,
    pub fact_id: String,
    pub target_resolution: RelationshipTargetResolution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_key: Option<InterfaceEntityKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LagMemberAssessment {
    /// Canonical interface name axis. Use `interface_key` for identity.
    pub interface: String,
    pub interface_key: InterfaceEntityKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_name: Option<String>,
    pub operational_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub administrative_state: Option<String>,
    /// Fact establishing the member interface state.
    pub state_fact_id: String,
    /// Fact establishing membership. For reverse evidence this is normally
    /// the member fact; for aggregate-reported forward membership it is the
    /// aggregate fact. `membership_evidence_axis` makes that distinction.
    pub relationship_fact_id: String,
    pub membership_evidence_axis: String,
    /// Backwards-compatible shorthand for `state_fact_id` from the initial
    /// review candidate.
    pub fact_id: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LagMembershipStatus {
    /// The aggregate itself reported an empty member list and no reverse
    /// member evidence contradicts it.
    ConfirmedEmpty,
    /// Aggregate-reported membership and reverse member evidence cover the
    /// same resolved member set.
    CompleteObserved,
    /// Only reverse member evidence exists. This supports narrow observed
    /// member-state consistency, not total intended/active membership.
    ReverseObservedOnly,
    /// Aggregate-reported membership is only partly corroborated by reverse
    /// member evidence or resolved member facts.
    Partial,
    /// Forward and reverse membership evidence contradict one another.
    Conflicting,
    /// No evidence establishes whether the aggregate has members.
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LagAssessment {
    /// Aggregate target as reported in relationship evidence, or the
    /// canonical aggregate entity for independently observed aggregates.
    pub aggregate: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_key: Option<InterfaceEntityKey>,
    /// Whether an aggregate entity observation exists. This is deliberately
    /// separate from whether its type is established as a LAG.
    pub aggregate_present: bool,
    pub aggregate_type_known_lag: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_fact_id: Option<String>,
    pub aggregate_operational_state: Option<String>,
    pub membership_status: LagMembershipStatus,
    pub member_count: usize,
    pub oper_up_members: usize,
    pub oper_down_members: usize,
    pub lower_layer_down_members: usize,
    pub dormant_members: usize,
    pub testing_members: usize,
    pub unknown_members: usize,
    pub members: Vec<LagMemberAssessment>,
    pub verdict: AssessmentVerdict,
    pub findings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VrfAttachmentEvidence {
    /// Canonical interface name axis. Use `interface_key` for identity.
    pub interface: String,
    pub interface_key: InterfaceEntityKey,
    pub network_instance: String,
    pub evidence_axis: String,
    pub fact_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeRelationshipReport {
    pub device_id: String,
    pub scope: Option<String>,
    pub network_namespace: Option<String>,
    pub observation_batch: ObservationBatchBoundary,
    pub input_contract_status: InputContractStatus,
    pub interface_count: usize,
    pub verdict: AssessmentVerdict,
    pub lag_assessments: Vec<LagAssessment>,
    pub bridge_memberships: Vec<RelationshipEdge>,
    pub parent_relationships: Vec<RelationshipEdge>,
    pub vrf_attachments: Vec<VrfAttachmentEvidence>,
    pub unresolved_relationships: Vec<RelationshipEdge>,
    pub relationship_issues: Vec<RelationshipIssue>,
    pub input_issues: Vec<InputIssue>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InterfaceRelationshipReport {
    pub scopes: Vec<ScopeRelationshipReport>,
    /// Valid-shaped facts that could not contribute because the payload was
    /// absent or not decodable as `interface.state`, plus integrity-invalid
    /// facts. Missing evidence is explicit rather than silently treated as
    /// absence. New consumers should prefer structured `input_issues` on the
    /// relevant scope; fact-level validation failures that cannot be safely
    /// assigned to a coherent scope remain here.
    pub rejected_inputs: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ScopeKey<'a> {
    device_id: &'a str,
    scope: Option<&'a str>,
    network_namespace: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct BatchKey<'a> {
    collector_id: &'a str,
    source_kind: &'a str,
    capture_id: &'a str,
    session_epoch: u64,
    exporter_id: Option<&'a str>,
}

#[derive(Clone)]
struct InterfaceRow<'a> {
    key: InterfaceEntityKey,
    entity: String,
    native_name: Option<String>,
    payload: InterfaceStatePayload,
    fact: &'a TypedFact,
}

struct EntityEvidence<'a> {
    rows: Vec<InterfaceRow<'a>>,
    representative: Option<usize>,
    conflicting: bool,
}

impl<'a> EntityEvidence<'a> {
    fn usable(&self) -> Option<&InterfaceRow<'a>> {
        self.representative.map(|index| &self.rows[index])
    }
}

struct BatchInput<'a> {
    rows: Vec<InterfaceRow<'a>>,
    input_issues: Vec<InputIssue>,
    boundary: ObservationBatchBoundary,
}

fn oper_name(value: Option<OperState>) -> Option<String> {
    value.map(|v| format!("{v:?}").to_ascii_lowercase())
}

fn admin_name(value: Option<crate::interface_facts::AdminState>) -> Option<String> {
    value.map(|v| format!("{v:?}").to_ascii_lowercase())
}

fn relationship_kind(kind: MasterKind) -> &'static str {
    match kind {
        MasterKind::Lag => "lag",
        MasterKind::Bridge => "bridge",
        MasterKind::Vrf => "vrf",
        MasterKind::Parent => "parent",
        MasterKind::Unresolved => "unresolved",
    }
}

fn input_issue(
    code: InputIssueCode,
    detail: impl Into<String>,
    fact_ids: impl IntoIterator<Item = String>,
) -> InputIssue {
    let mut fact_ids: Vec<_> = fact_ids.into_iter().collect();
    fact_ids.sort();
    fact_ids.dedup();
    InputIssue {
        code,
        detail: detail.into(),
        fact_ids,
    }
}

fn relationship_issue(
    code: RelationshipIssueCode,
    interface: Option<InterfaceEntityKey>,
    target: Option<String>,
    kind: Option<&str>,
    fact_ids: impl IntoIterator<Item = String>,
    detail: impl Into<String>,
) -> RelationshipIssue {
    let mut fact_ids: Vec<_> = fact_ids.into_iter().collect();
    fact_ids.sort();
    fact_ids.dedup();
    RelationshipIssue {
        code,
        interface,
        target,
        kind: kind.map(str::to_string),
        fact_ids,
        detail: detail.into(),
    }
}

fn parse_time(value: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(value).ok()
}

fn batch_boundary<'a>(key: BatchKey<'a>, facts: &[&'a TypedFact]) -> ObservationBatchBoundary {
    let mut sequence_min = None;
    let mut sequence_max = None;
    let mut start: Option<chrono::DateTime<chrono::FixedOffset>> = None;
    let mut end: Option<chrono::DateTime<chrono::FixedOffset>> = None;
    let mut start_raw = None;
    let mut end_raw = None;
    for fact in facts {
        sequence_min =
            Some(sequence_min.map_or(fact.source.sequence, |v: u64| v.min(fact.source.sequence)));
        sequence_max =
            Some(sequence_max.map_or(fact.source.sequence, |v: u64| v.max(fact.source.sequence)));
        if let Some(ts) = parse_time(&fact.observed_at) {
            if start.is_none_or(|s| ts < s) {
                start = Some(ts);
                start_raw = Some(fact.observed_at.clone());
            }
            if end.is_none_or(|e| ts > e) {
                end = Some(ts);
                end_raw = Some(fact.observed_at.clone());
            }
        }
    }
    ObservationBatchBoundary {
        collector_id: key.collector_id.into(),
        source_kind: key.source_kind.into(),
        capture_id: key.capture_id.into(),
        session_epoch: key.session_epoch,
        exporter_id: key.exporter_id.map(str::to_string),
        fact_count: facts.len(),
        sequence_min,
        sequence_max,
        observed_at_start: start_raw,
        observed_at_end: end_raw,
    }
}

fn is_expired_before_observation(fact: &TypedFact) -> bool {
    match (&fact.valid_until, parse_time(&fact.observed_at)) {
        (Some(until), Some(observed)) => parse_time(until).is_some_and(|u| u < observed),
        _ => false,
    }
}

fn consolidate_rows<'a>(
    rows: Vec<InterfaceRow<'a>>,
) -> (
    BTreeMap<InterfaceEntityKey, EntityEvidence<'a>>,
    Vec<InputIssue>,
    Vec<RelationshipIssue>,
) {
    let mut sorted = rows;
    sorted.sort_by(|a, b| {
        (&a.key, &a.fact.fact_id, &a.fact.payload_sha256).cmp(&(
            &b.key,
            &b.fact.fact_id,
            &b.fact.payload_sha256,
        ))
    });
    let mut grouped: BTreeMap<InterfaceEntityKey, Vec<InterfaceRow<'a>>> = BTreeMap::new();
    for row in sorted {
        grouped.entry(row.key.clone()).or_default().push(row);
    }

    let mut entities = BTreeMap::new();
    let mut input_issues = Vec::new();
    let mut relationship_issues = Vec::new();
    for (key, rows) in grouped {
        let mut deduped = Vec::new();
        let mut seen_fact_ids = BTreeSet::new();
        for row in rows {
            // Replaying exactly the same fact is idempotent and cannot add a
            // second member or a second interface.
            if seen_fact_ids.insert(row.fact.fact_id.clone()) {
                deduped.push(row);
            }
        }
        let payload_digests: BTreeSet<_> = deduped
            .iter()
            .map(|r| r.fact.payload_sha256.as_str())
            .collect();
        let vrfs: BTreeSet<_> = deduped
            .iter()
            .map(|r| r.fact.subject.vrf.as_deref())
            .collect();
        let native_names: BTreeSet<_> = deduped.iter().map(|r| r.native_name.as_deref()).collect();
        let conflicting = payload_digests.len() > 1 || vrfs.len() > 1 || native_names.len() > 1;
        let representative = (!conflicting).then_some(0usize);
        if conflicting {
            let ids: Vec<_> = deduped.iter().map(|r| r.fact.fact_id.clone()).collect();
            input_issues.push(input_issue(
                InputIssueCode::ConflictingObservations,
                format!(
                    "multiple distinct same-generation observations exist for {}; none is selected by input order",
                    key.label()
                ),
                ids.clone(),
            ));
            relationship_issues.push(relationship_issue(
                RelationshipIssueCode::ConflictingEntityObservations,
                Some(key.clone()),
                None,
                None,
                ids,
                "conflicting observations of one semantic interface entity remain explicit",
            ));
        }
        entities.insert(
            key.clone(),
            EntityEvidence {
                rows: deduped,
                representative,
                conflicting,
            },
        );
    }
    (entities, input_issues, relationship_issues)
}

fn resolution_for_key(key: &InterfaceEntityKey) -> RelationshipTargetResolution {
    match key.identity_kind {
        Some(InterfaceIdentityKind::Physical) => {
            RelationshipTargetResolution::ResolvedBaseInterface
        }
        Some(InterfaceIdentityKind::Logical) => {
            RelationshipTargetResolution::ResolvedLogicalInterface
        }
        Some(InterfaceIdentityKind::Subinterface) => {
            RelationshipTargetResolution::ResolvedSubinterface
        }
        None => RelationshipTargetResolution::ResolvedUnrefinedInterface,
    }
}

fn resolve_target(
    target: &str,
    entities: &BTreeMap<InterfaceEntityKey, EntityEvidence<'_>>,
    by_canonical: &BTreeMap<String, Vec<InterfaceEntityKey>>,
) -> (RelationshipTargetResolution, Option<InterfaceEntityKey>) {
    let Some(candidates) = by_canonical.get(target) else {
        return (RelationshipTargetResolution::MissingTarget, None);
    };
    if candidates
        .iter()
        .any(|key| entities.get(key).is_some_and(|e| e.conflicting))
    {
        return (
            RelationshipTargetResolution::ConflictingTargetEvidence,
            None,
        );
    }
    let usable: Vec<_> = candidates
        .iter()
        .filter(|key| {
            entities
                .get(*key)
                .and_then(EntityEvidence::usable)
                .is_some()
        })
        .collect();
    // A bare target name normally denotes the physical/base or logical
    // interface, never a numbered child by inference. If exactly one
    // non-subinterface uses the canonical name, that target is unambiguous
    // even when subinterfaces coexist. A sole numbered subinterface is not
    // selected either: its canonical subject entity intentionally equals the
    // parent, so accepting it as the bare parent target would collapse the
    // physical/subinterface identity axis. Two base/logical/unrefined
    // claimants likewise remain ambiguous.
    let base: Vec<_> = usable
        .iter()
        .filter(|key| key.is_base_target_candidate())
        .collect();
    if base.len() == 1 {
        let key = (*base[0]).clone();
        return (resolution_for_key(&key), Some(key));
    }
    (
        RelationshipTargetResolution::AmbiguousStructuredTarget,
        None,
    )
}

fn edge(
    row: &InterfaceRow<'_>,
    target: &str,
    kind: &str,
    resolution: RelationshipTargetResolution,
    target_key: Option<InterfaceEntityKey>,
    reason_code: Option<&str>,
) -> RelationshipEdge {
    RelationshipEdge {
        interface: row.entity.clone(),
        interface_key: row.key.clone(),
        interface_display: row.native_name.clone(),
        target: target.into(),
        kind: kind.into(),
        fact_id: row.fact.fact_id.clone(),
        target_resolution: resolution,
        target_key,
        reason_code: reason_code.map(str::to_string),
    }
}

fn known_type(row: &InterfaceRow<'_>, interface_type: InterfaceType) -> bool {
    row.payload.interface_type.state == FieldAvailability::Known
        && row.payload.interface_type.value == Some(interface_type)
}

fn state_is_known_usable(row: &InterfaceRow<'_>) -> bool {
    row.payload.operational_state.state == FieldAvailability::Known
        && row.payload.operational_state.value != Some(OperState::Unknown)
        && row.payload.operational_state.value.is_some()
}

fn capture_population_gap(rows: &[&InterfaceRow<'_>]) -> bool {
    rows.iter().any(|row| {
        row.fact.quality.notes.iter().any(|note| {
            note.contains("entity population truncated")
                || note.contains("capture outcome=")
                || note.contains("resource limit")
        })
    })
}

fn find_parent_cycles(
    adjacency: &BTreeMap<InterfaceEntityKey, BTreeSet<InterfaceEntityKey>>,
) -> Vec<Vec<InterfaceEntityKey>> {
    let mut cycles = Vec::new();
    let mut seen_signatures = BTreeSet::new();
    for start in adjacency.keys() {
        let mut path: Vec<InterfaceEntityKey> = Vec::new();
        let mut positions = BTreeMap::new();
        let mut current = Some(start.clone());
        while let Some(node) = current {
            if let Some(pos) = positions.get(&node) {
                let cycle: Vec<_> = path[*pos..].to_vec();
                if !cycle.is_empty() {
                    let mut signature_nodes = cycle.clone();
                    signature_nodes.push(cycle[0].clone());
                    let signature = format!("{signature_nodes:?}");
                    if seen_signatures.insert(signature) {
                        cycles.push(cycle);
                    }
                }
                break;
            }
            if !adjacency.contains_key(&node) {
                break;
            }
            positions.insert(node.clone(), path.len());
            path.push(node.clone());
            // Parent graphs should be singular in well-formed evidence, but
            // contradictory extra edges are preserved. Follow the smallest
            // deterministic successor; every start node is still visited.
            current = adjacency
                .get(&node)
                .and_then(|next| next.iter().next().cloned());
        }
    }
    cycles
}

fn analyze_batch(
    scope_key: ScopeKey<'_>,
    input: BatchInput<'_>,
    multiple_batches: bool,
) -> ScopeRelationshipReport {
    let BatchInput {
        rows,
        mut input_issues,
        boundary,
    } = input;
    if multiple_batches {
        input_issues.push(input_issue(
            InputIssueCode::MixedObservationBatch,
            "multiple observation batches for this scope are preserved separately but are not reconciled into one operational snapshot",
            [],
        ));
    }

    let usable_row_refs: Vec<&InterfaceRow<'_>> = rows.iter().collect();
    let population_gap = capture_population_gap(&usable_row_refs);
    if population_gap {
        let ids = rows
            .iter()
            .map(|r| r.fact.fact_id.clone())
            .collect::<Vec<_>>();
        input_issues.push(input_issue(
            InputIssueCode::ScopeCoverageIncomplete,
            "capture/evidence notes establish omitted entities, resource truncation or a degraded capture plane",
            ids,
        ));
    }

    let (entities, mut consolidation_input_issues, mut relationship_issues) =
        consolidate_rows(rows);
    input_issues.append(&mut consolidation_input_issues);

    let mut by_canonical: BTreeMap<String, Vec<InterfaceEntityKey>> = BTreeMap::new();
    for key in entities.keys() {
        by_canonical
            .entry(key.canonical_interface.clone())
            .or_default()
            .push(key.clone());
    }
    for keys in by_canonical.values_mut() {
        keys.sort();
        keys.dedup();
    }

    let mut reverse_members_by_lag: BTreeMap<
        String,
        BTreeMap<InterfaceEntityKey, InterfaceRow<'_>>,
    > = BTreeMap::new();
    let mut forward_members_by_lag: BTreeMap<String, Vec<(InterfaceRow<'_>, Vec<String>)>> =
        BTreeMap::new();
    let mut unavailable_forward_by_lag: BTreeMap<String, Vec<InterfaceRow<'_>>> = BTreeMap::new();
    let mut bridge_memberships = Vec::new();
    let mut parent_relationships = Vec::new();
    let mut vrf_attachments = Vec::new();
    let mut unresolved_relationships = Vec::new();
    let mut seen_vrf: BTreeSet<(InterfaceEntityKey, String, &'static str)> = BTreeSet::new();
    let mut vrf_by_interface: BTreeMap<InterfaceEntityKey, BTreeMap<String, Vec<String>>> =
        BTreeMap::new();
    let mut parent_adjacency: BTreeMap<InterfaceEntityKey, BTreeSet<InterfaceEntityKey>> =
        BTreeMap::new();
    let mut parent_cycle_fact_ids: BTreeMap<(InterfaceEntityKey, InterfaceEntityKey), String> =
        BTreeMap::new();

    for evidence in entities.values() {
        let Some(row) = evidence.usable() else {
            continue;
        };

        if let Some(lag) = &row.payload.member_of {
            reverse_members_by_lag
                .entry(lag.clone())
                .or_default()
                .insert(row.key.clone(), row.clone());
        }

        if let Some(members) = &row.payload.lag_members {
            match members.state {
                FieldAvailability::Known => {
                    if let Some(values) = &members.value {
                        forward_members_by_lag
                            .entry(row.entity.clone())
                            .or_default()
                            .push((row.clone(), values.clone()));
                    }
                }
                _ => {
                    if known_type(row, InterfaceType::PortChannel) {
                        unavailable_forward_by_lag
                            .entry(row.entity.clone())
                            .or_default()
                            .push(row.clone());
                        relationship_issues.push(relationship_issue(
                            RelationshipIssueCode::UnavailableForwardMemberList,
                            Some(row.key.clone()),
                            Some(row.entity.clone()),
                            Some("lag_members"),
                            [row.fact.fact_id.clone()],
                            "aggregate membership list is present in schema but unavailable/invalid in evidence",
                        ));
                    }
                }
            }
        }

        if let Some(master) = &row.payload.master_relationship {
            let (resolution, target_key) = resolve_target(&master.target, &entities, &by_canonical);
            let reason = match resolution {
                RelationshipTargetResolution::MissingTarget => Some("missing_target"),
                RelationshipTargetResolution::AmbiguousStructuredTarget => {
                    Some("ambiguous_structured_target")
                }
                RelationshipTargetResolution::ConflictingTargetEvidence => {
                    Some("conflicting_target_evidence")
                }
                _ => None,
            };
            let e = edge(
                row,
                &master.target,
                relationship_kind(master.kind),
                resolution,
                target_key.clone(),
                reason,
            );
            if target_key.as_ref() == Some(&row.key) {
                relationship_issues.push(relationship_issue(
                    RelationshipIssueCode::SelfRelationship,
                    Some(row.key.clone()),
                    Some(master.target.clone()),
                    Some(relationship_kind(master.kind)),
                    [row.fact.fact_id.clone()],
                    "relationship target resolves to the same structured interface entity",
                ));
            }
            if resolution == RelationshipTargetResolution::AmbiguousStructuredTarget {
                relationship_issues.push(relationship_issue(
                    RelationshipIssueCode::AmbiguousRelationshipTarget,
                    Some(row.key.clone()),
                    Some(master.target.clone()),
                    Some(relationship_kind(master.kind)),
                    [row.fact.fact_id.clone()],
                    "bare target name has multiple incompatible structured interface claimants",
                ));
            }
            match master.kind {
                MasterKind::Bridge => {
                    if resolution.resolved() {
                        bridge_memberships.push(e);
                    } else {
                        unresolved_relationships.push(e);
                    }
                }
                MasterKind::Parent => {
                    if resolution.resolved() {
                        if let Some(target) = &target_key {
                            parent_adjacency
                                .entry(row.key.clone())
                                .or_default()
                                .insert(target.clone());
                            parent_cycle_fact_ids.insert(
                                (row.key.clone(), target.clone()),
                                row.fact.fact_id.clone(),
                            );
                        }
                        parent_relationships.push(e);
                    } else {
                        unresolved_relationships.push(e);
                    }
                }
                MasterKind::Vrf => {
                    if resolution.resolved() {
                        if seen_vrf.insert((row.key.clone(), master.target.clone(), "master")) {
                            vrf_attachments.push(VrfAttachmentEvidence {
                                interface: row.entity.clone(),
                                interface_key: row.key.clone(),
                                network_instance: master.target.clone(),
                                evidence_axis: "master_relationship".into(),
                                fact_id: row.fact.fact_id.clone(),
                            });
                        }
                        vrf_by_interface
                            .entry(row.key.clone())
                            .or_default()
                            .entry(master.target.clone())
                            .or_default()
                            .push(row.fact.fact_id.clone());
                    } else {
                        unresolved_relationships.push(e);
                    }
                }
                MasterKind::Unresolved => unresolved_relationships.push(e),
                MasterKind::Lag => {
                    // LAG membership is assessed below even when the aggregate
                    // is missing; that distinction is part of the assessment.
                    if !resolution.resolved() {
                        unresolved_relationships.push(e);
                    }
                }
            }
        }

        if let Some(parent) = &row.payload.parent_relationship {
            let (resolution, target_key) = resolve_target(&parent.target, &entities, &by_canonical);
            let reason = match resolution {
                RelationshipTargetResolution::MissingTarget => Some("missing_parent_target"),
                RelationshipTargetResolution::AmbiguousStructuredTarget => {
                    Some("ambiguous_structured_target")
                }
                RelationshipTargetResolution::ConflictingTargetEvidence => {
                    Some("conflicting_target_evidence")
                }
                _ => None,
            };
            let e = edge(
                row,
                &parent.target,
                relationship_kind(parent.kind),
                resolution,
                target_key.clone(),
                reason,
            );
            if resolution.resolved() {
                if let Some(target) = &target_key {
                    parent_adjacency
                        .entry(row.key.clone())
                        .or_default()
                        .insert(target.clone());
                    parent_cycle_fact_ids
                        .insert((row.key.clone(), target.clone()), row.fact.fact_id.clone());
                }
                parent_relationships.push(e);
            } else {
                unresolved_relationships.push(e);
            }
        }

        if let Some(vrf) = &row.fact.subject.vrf
            && seen_vrf.insert((row.key.clone(), vrf.clone(), "subject"))
        {
            vrf_attachments.push(VrfAttachmentEvidence {
                interface: row.entity.clone(),
                interface_key: row.key.clone(),
                network_instance: vrf.clone(),
                evidence_axis: "subject.vrf".into(),
                fact_id: row.fact.fact_id.clone(),
            });
        }
        if let Some(vrf) = &row.fact.subject.vrf {
            vrf_by_interface
                .entry(row.key.clone())
                .or_default()
                .entry(vrf.clone())
                .or_default()
                .push(row.fact.fact_id.clone());
        }
    }

    for (interface, instances) in &vrf_by_interface {
        if instances.len() > 1 {
            let ids = instances
                .values()
                .flat_map(|v| v.iter().cloned())
                .collect::<Vec<_>>();
            relationship_issues.push(relationship_issue(
                RelationshipIssueCode::ConflictingVrfAttachment,
                Some(interface.clone()),
                None,
                Some("vrf"),
                ids,
                format!(
                    "interface has contradictory network-instance attachments: {}",
                    instances.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            ));
        }
    }

    for cycle in find_parent_cycles(&parent_adjacency) {
        let mut ids = Vec::new();
        for pair in cycle.windows(2) {
            if let Some(id) = parent_cycle_fact_ids.get(&(pair[0].clone(), pair[1].clone())) {
                ids.push(id.clone());
            }
        }
        if let (Some(first), Some(last)) = (cycle.first(), cycle.last())
            && let Some(id) = parent_cycle_fact_ids.get(&(last.clone(), first.clone()))
        {
            ids.push(id.clone());
        }
        relationship_issues.push(relationship_issue(
            RelationshipIssueCode::RelationshipCycle,
            cycle.first().cloned(),
            cycle.last().map(|k| k.canonical_interface.clone()),
            Some("parent"),
            ids,
            format!(
                "parent relationship cycle: {}",
                cycle
                    .iter()
                    .map(InterfaceEntityKey::label)
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
        ));
    }

    // Independent aggregate candidates come from both aggregate observations
    // and membership evidence. A LAG row with no member rows therefore remains
    // explicitly assessable instead of disappearing from the iteration.
    let mut aggregate_names = BTreeSet::new();
    for evidence in entities.values() {
        if let Some(row) = evidence.usable()
            && known_type(row, InterfaceType::PortChannel)
        {
            aggregate_names.insert(row.entity.clone());
        }
    }
    aggregate_names.extend(reverse_members_by_lag.keys().cloned());
    aggregate_names.extend(forward_members_by_lag.keys().cloned());

    let has_conflicting_entities = entities.values().any(|e| e.conflicting);
    let mut lag_assessments = Vec::new();
    for aggregate in aggregate_names {
        let (aggregate_resolution, aggregate_key) =
            resolve_target(&aggregate, &entities, &by_canonical);
        let aggregate_row = aggregate_key
            .as_ref()
            .and_then(|key| entities.get(key))
            .and_then(EntityEvidence::usable);
        let aggregate_present = aggregate_row.is_some();
        let aggregate_is_subinterface = aggregate_key
            .as_ref()
            .is_some_and(InterfaceEntityKey::is_subinterface);
        let aggregate_type_known_lag = aggregate_row.is_some_and(|row| {
            known_type(row, InterfaceType::PortChannel) && !aggregate_is_subinterface
        });
        let aggregate_oper =
            aggregate_row.and_then(|r| oper_name(r.payload.operational_state.value));
        let aggregate_fact_id = aggregate_row.map(|r| r.fact.fact_id.clone());

        if aggregate_is_subinterface {
            relationship_issues.push(relationship_issue(
                RelationshipIssueCode::SubinterfaceLagTarget,
                aggregate_key.clone(),
                Some(aggregate.clone()),
                Some("lag"),
                aggregate_fact_id.iter().cloned().collect::<Vec<_>>(),
                "numbered subinterface evidence cannot be selected as a LAG aggregate target",
            ));
        } else if aggregate_present
            && aggregate_row.is_some_and(|row| {
                row.payload.interface_type.state == FieldAvailability::Known
                    && row.payload.interface_type.value.is_some()
                    && row.payload.interface_type.value != Some(InterfaceType::PortChannel)
            })
        {
            relationship_issues.push(relationship_issue(
                RelationshipIssueCode::IncompatibleLagTargetType,
                aggregate_key.clone(),
                Some(aggregate.clone()),
                Some("lag"),
                aggregate_fact_id.iter().cloned().collect::<Vec<_>>(),
                "relationship target exists but its observed type is not a LAG aggregate",
            ));
        }

        let reverse = reverse_members_by_lag
            .get(&aggregate)
            .cloned()
            .unwrap_or_default();
        let forward_entries = forward_members_by_lag.get(&aggregate);
        if let Some(entries) = forward_entries
            && entries.len() > 1
        {
            let ids = entries
                .iter()
                .map(|(row, _)| row.fact.fact_id.clone())
                .collect::<Vec<_>>();
            relationship_issues.push(relationship_issue(
                RelationshipIssueCode::ConflictingEntityObservations,
                aggregate_key.clone(),
                Some(aggregate.clone()),
                Some("lag_members"),
                ids,
                "multiple structured entities with the same canonical aggregate name carry forward membership evidence; none is selected by input order",
            ));
        }
        let forward = forward_entries
            .and_then(|entries| (entries.len() == 1).then(|| (&entries[0].0, &entries[0].1)));
        let conflicting_forward_entries = forward_entries.is_some_and(|entries| entries.len() > 1);
        let forward_membership_unavailable = unavailable_forward_by_lag.contains_key(&aggregate);
        let mut member_keys = BTreeSet::new();
        member_keys.extend(reverse.keys().cloned());
        let mut missing_forward_names = Vec::new();
        let mut forward_keys = BTreeSet::new();
        let mut forward_state = None;
        if let Some((_, names)) = forward {
            forward_state = Some(names.clone());
            for name in names {
                let (member_resolution, member_key) =
                    resolve_target(name, &entities, &by_canonical);
                if member_resolution.resolved()
                    && member_key.as_ref().is_some_and(|key| {
                        entities.get(key).and_then(EntityEvidence::usable).is_some()
                    })
                {
                    let key = member_key.expect("checked resolved member key");
                    forward_keys.insert(key.clone());
                    member_keys.insert(key);
                } else {
                    missing_forward_names.push(name.clone());
                }
            }
        }

        let reverse_keys: BTreeSet<_> = reverse.keys().cloned().collect();
        let membership_status = match &forward_state {
            None => {
                if reverse_keys.is_empty() {
                    LagMembershipStatus::Unknown
                } else {
                    LagMembershipStatus::ReverseObservedOnly
                }
            }
            Some(names) if names.is_empty() && reverse_keys.is_empty() => {
                LagMembershipStatus::ConfirmedEmpty
            }
            Some(_) if !missing_forward_names.is_empty() => LagMembershipStatus::Partial,
            Some(_) => {
                let missing_reverse: Vec<_> =
                    forward_keys.difference(&reverse_keys).cloned().collect();
                let unexpected_reverse: Vec<_> =
                    reverse_keys.difference(&forward_keys).cloned().collect();
                if !unexpected_reverse.is_empty() {
                    LagMembershipStatus::Conflicting
                } else if !missing_reverse.is_empty() {
                    LagMembershipStatus::Partial
                } else {
                    LagMembershipStatus::CompleteObserved
                }
            }
        };

        if let Some((aggregate_row_evidence, _)) = forward {
            for name in &missing_forward_names {
                relationship_issues.push(relationship_issue(
                    RelationshipIssueCode::MissingForwardMember,
                    aggregate_key.clone(),
                    Some(name.clone()),
                    Some("lag_members"),
                    [aggregate_row_evidence.fact.fact_id.clone()],
                    "aggregate-reported member does not resolve to a usable observed interface entity",
                ));
            }
            if membership_status == LagMembershipStatus::Partial {
                let missing_reverse: Vec<_> =
                    forward_keys.difference(&reverse_keys).cloned().collect();
                if !missing_reverse.is_empty() {
                    relationship_issues.push(relationship_issue(
                        RelationshipIssueCode::MissingReverseMemberEvidence,
                        aggregate_key.clone(),
                        Some(aggregate.clone()),
                        Some("lag_members"),
                        [aggregate_row_evidence.fact.fact_id.clone()],
                        "aggregate-reported membership lacks reverse member relationship evidence",
                    ));
                }
            }
            if membership_status == LagMembershipStatus::Conflicting {
                let ids = reverse
                    .values()
                    .map(|row| row.fact.fact_id.clone())
                    .chain([aggregate_row_evidence.fact.fact_id.clone()])
                    .collect::<Vec<_>>();
                relationship_issues.push(relationship_issue(
                    RelationshipIssueCode::UnexpectedReverseMember,
                    aggregate_key.clone(),
                    Some(aggregate.clone()),
                    Some("lag_members"),
                    ids,
                    "reverse member evidence is absent from the aggregate-reported membership list",
                ));
            }
        }

        let mut member_rows = Vec::new();
        let mut up = 0usize;
        let mut down = 0usize;
        let mut lower_layer_down = 0usize;
        let mut dormant = 0usize;
        let mut testing = 0usize;
        let mut unknown = 0usize;
        let mut admin_down = 0usize;
        for key in &member_keys {
            let Some(entity) = entities.get(key) else {
                continue;
            };
            let Some(member) = entity.usable() else {
                continue;
            };
            let state = member.payload.operational_state.value;
            match state {
                Some(OperState::Up)
                    if member.payload.operational_state.state == FieldAvailability::Known =>
                {
                    up += 1
                }
                Some(OperState::Down)
                    if member.payload.operational_state.state == FieldAvailability::Known =>
                {
                    down += 1
                }
                Some(OperState::LowerLayerDown)
                    if member.payload.operational_state.state == FieldAvailability::Known =>
                {
                    lower_layer_down += 1
                }
                Some(OperState::Dormant)
                    if member.payload.operational_state.state == FieldAvailability::Known =>
                {
                    dormant += 1
                }
                Some(OperState::Testing)
                    if member.payload.operational_state.state == FieldAvailability::Known =>
                {
                    testing += 1
                }
                _ => unknown += 1,
            }
            if member.payload.administrative_state.state == FieldAvailability::Known
                && member.payload.administrative_state.value
                    == Some(crate::interface_facts::AdminState::Down)
            {
                admin_down += 1;
            }
            let reverse_row = reverse.get(key);
            let relationship_fact_id = reverse_row
                .map(|r| r.fact.fact_id.clone())
                .or_else(|| forward.and_then(|(agg, _)| Some(agg.fact.fact_id.clone())))
                .unwrap_or_else(|| member.fact.fact_id.clone());
            let axis = if reverse_row.is_some() {
                "member_of"
            } else {
                "aggregate_lag_members"
            };
            member_rows.push(LagMemberAssessment {
                interface: member.entity.clone(),
                interface_key: member.key.clone(),
                native_name: member.native_name.clone(),
                operational_state: oper_name(state),
                administrative_state: admin_name(member.payload.administrative_state.value),
                state_fact_id: member.fact.fact_id.clone(),
                relationship_fact_id,
                membership_evidence_axis: axis.into(),
                fact_id: member.fact.fact_id.clone(),
            });
        }
        member_rows.sort_by(|a, b| a.interface_key.cmp(&b.interface_key));
        let member_count = member_rows.len();

        let mut findings = Vec::new();
        if up > 0 {
            findings.push("observed_member_oper_up".into());
        }
        if down > 0 {
            findings.push("observed_member_oper_down".into());
        }
        if lower_layer_down > 0 {
            findings.push("observed_member_oper_lower_layer_down".into());
        }
        if dormant > 0 {
            findings.push("observed_member_oper_dormant".into());
        }
        if testing > 0 {
            findings.push("observed_member_oper_testing".into());
        }
        if unknown > 0 {
            findings.push("member_operational_state_unknown".into());
        }
        if admin_down > 0 {
            findings.push("observed_member_admin_down".into());
        }
        if member_count > 0 && up == 0 && unknown == 0 {
            findings.push("no_observed_oper_up_members".into());
        }
        if matches!(
            aggregate_row.and_then(|r| r.payload.operational_state.value),
            Some(OperState::Down) | Some(OperState::LowerLayerDown) | Some(OperState::Dormant)
        ) && up > 0
        {
            findings.push("aggregate_oper_down_with_observed_oper_up_member".into());
        }

        let mut proven_inconsistency = false;
        let mut evidence_missing = false;
        if matches!(
            aggregate_resolution,
            RelationshipTargetResolution::AmbiguousStructuredTarget
                | RelationshipTargetResolution::ConflictingTargetEvidence
        ) {
            evidence_missing = true;
            findings.push("aggregate_target_resolution_ambiguous_or_conflicting".into());
        }
        if !aggregate_present {
            evidence_missing = true;
            findings.push("aggregate_missing_or_not_independently_observed".into());
        } else if !aggregate_type_known_lag {
            if aggregate_row.is_some_and(|row| {
                row.payload.interface_type.state == FieldAvailability::Known
                    && row.payload.interface_type.value.is_some()
                    && row.payload.interface_type.value != Some(InterfaceType::PortChannel)
            }) || aggregate_is_subinterface
            {
                proven_inconsistency = true;
            } else {
                evidence_missing = true;
            }
            findings.push("aggregate_type_unknown_or_not_lag".into());
        }
        if !aggregate_row.is_some_and(state_is_known_usable) {
            evidence_missing = true;
            findings.push("aggregate_operational_state_unknown".into());
        }
        match membership_status {
            LagMembershipStatus::Unknown => {
                evidence_missing = true;
                findings.push("lag_membership_unknown".into());
            }
            LagMembershipStatus::Partial => {
                evidence_missing = true;
                findings.push("lag_membership_partial".into());
            }
            LagMembershipStatus::Conflicting => {
                proven_inconsistency = true;
                findings.push("lag_membership_conflicting".into());
            }
            LagMembershipStatus::ReverseObservedOnly => {
                findings.push("reverse_membership_evidence_only".into());
            }
            LagMembershipStatus::ConfirmedEmpty | LagMembershipStatus::CompleteObserved => {}
        }
        if unknown > 0 {
            evidence_missing = true;
        }
        if has_conflicting_entities {
            evidence_missing = true;
            findings.push("conflicting_interface_observations_present".into());
        }
        if conflicting_forward_entries {
            evidence_missing = true;
            findings.push("forward_membership_evidence_conflicting".into());
        }
        if forward_membership_unavailable {
            evidence_missing = true;
            findings.push("forward_membership_evidence_unavailable".into());
        }
        if population_gap || multiple_batches {
            evidence_missing = true;
            findings.push("observation_scope_incomplete_or_unreconciled".into());
        }

        let verdict = if proven_inconsistency {
            AssessmentVerdict::Fail
        } else if evidence_missing {
            AssessmentVerdict::Unknown
        } else {
            // PASS means only that the bounded evidence prerequisites are
            // complete and internally consistent for observed interface link
            // state. It is not a claim of LACP selection, collecting/
            // distributing state, forwarding participation, intended member
            // count or usable LAG capacity.
            AssessmentVerdict::Pass
        };

        lag_assessments.push(LagAssessment {
            aggregate,
            aggregate_key,
            aggregate_present,
            aggregate_type_known_lag,
            aggregate_fact_id,
            aggregate_operational_state: aggregate_oper,
            membership_status,
            member_count,
            oper_up_members: up,
            oper_down_members: down,
            lower_layer_down_members: lower_layer_down,
            dormant_members: dormant,
            testing_members: testing,
            unknown_members: unknown,
            members: member_rows,
            verdict,
            findings,
        });
    }
    lag_assessments.sort_by(|a, b| a.aggregate.cmp(&b.aggregate));

    bridge_memberships.sort_by(|a, b| {
        (&a.interface_key, &a.target, &a.fact_id).cmp(&(&b.interface_key, &b.target, &b.fact_id))
    });
    parent_relationships.sort_by(|a, b| {
        (&a.interface_key, &a.target, &a.fact_id).cmp(&(&b.interface_key, &b.target, &b.fact_id))
    });
    unresolved_relationships.sort_by(|a, b| {
        (&a.interface_key, &a.target, &a.kind, &a.fact_id).cmp(&(
            &b.interface_key,
            &b.target,
            &b.kind,
            &b.fact_id,
        ))
    });
    vrf_attachments.sort_by(|a, b| {
        (&a.interface_key, &a.network_instance, &a.fact_id).cmp(&(
            &b.interface_key,
            &b.network_instance,
            &b.fact_id,
        ))
    });
    relationship_issues
        .sort_by(|a, b| (&a.code, &a.detail, &a.fact_ids).cmp(&(&b.code, &b.detail, &b.fact_ids)));
    relationship_issues.dedup_by(|a, b| {
        a.code == b.code
            && a.interface == b.interface
            && a.target == b.target
            && a.kind == b.kind
            && a.fact_ids == b.fact_ids
            && a.detail == b.detail
    });
    input_issues
        .sort_by(|a, b| (&a.code, &a.detail, &a.fact_ids).cmp(&(&b.code, &b.detail, &b.fact_ids)));
    input_issues
        .dedup_by(|a, b| a.code == b.code && a.detail == b.detail && a.fact_ids == b.fact_ids);

    let degraded_input = input_issues
        .iter()
        .any(|issue| !matches!(issue.code, InputIssueCode::MixedObservationBatch));
    let input_contract_status = if multiple_batches {
        InputContractStatus::MultipleUnreconciledBatches
    } else if degraded_input {
        InputContractStatus::DegradedObservationBatch
    } else {
        InputContractStatus::CoherentObservationBatch
    };

    let verdict = if lag_assessments
        .iter()
        .any(|l| l.verdict == AssessmentVerdict::Fail)
    {
        AssessmentVerdict::Fail
    } else if multiple_batches
        || degraded_input
        || !unresolved_relationships.is_empty()
        || lag_assessments
            .iter()
            .any(|l| l.verdict == AssessmentVerdict::Unknown)
    {
        AssessmentVerdict::Unknown
    } else if lag_assessments.is_empty() {
        // NOT_APPLICABLE is reserved for a coherent scope with no aggregate,
        // no reverse/forward membership and no unresolved relationship or
        // coverage evidence suggesting that LAG applicability is unknown.
        AssessmentVerdict::NotApplicable
    } else {
        AssessmentVerdict::Pass
    };

    ScopeRelationshipReport {
        device_id: scope_key.device_id.into(),
        scope: scope_key.scope.map(str::to_string),
        network_namespace: scope_key.network_namespace.map(str::to_string),
        observation_batch: boundary,
        input_contract_status,
        interface_count: entities.len(),
        verdict,
        lag_assessments,
        bridge_memberships,
        parent_relationships,
        vrf_attachments,
        unresolved_relationships,
        relationship_issues,
        input_issues,
    }
}

/// Analyze typed interface facts without mutating inventory, topology or any
/// device. Positive LAG verdicts are deliberately narrow evidence-consistency
/// verdicts: they do not assert LACP selection, collecting/distributing state,
/// active forwarding participation or intended capacity.
pub fn analyze_interface_relationships(facts: &[TypedFact]) -> InterfaceRelationshipReport {
    let mut grouped: BTreeMap<ScopeKey<'_>, BTreeMap<BatchKey<'_>, Vec<&TypedFact>>> =
        BTreeMap::new();
    let mut rejected_inputs = Vec::new();

    for fact in facts {
        if fact.fact_kind != FACT_KIND_INTERFACE_STATE {
            continue;
        }
        let scope_key = ScopeKey {
            device_id: fact.subject.device_id.as_str(),
            scope: fact.subject.scope.as_deref(),
            network_namespace: fact.subject.network_namespace.as_deref(),
        };
        let batch_key = BatchKey {
            collector_id: fact.source.collector_id.as_str(),
            source_kind: fact.source.source_kind.as_str(),
            capture_id: fact.source.capture_id.as_str(),
            session_epoch: fact.source.session_epoch,
            exporter_id: fact.source.exporter_id.as_deref(),
        };
        grouped
            .entry(scope_key)
            .or_default()
            .entry(batch_key)
            .or_default()
            .push(fact);
    }

    let mut scopes = Vec::new();
    for (scope_key, batches) in grouped {
        let multiple_batches = batches.len() > 1;
        for (batch_key, batch_facts) in batches {
            let mut input = BatchInput {
                rows: Vec::new(),
                input_issues: Vec::new(),
                boundary: batch_boundary(batch_key, &batch_facts),
            };
            for fact in batch_facts {
                if let Err(err) = fact.validate() {
                    rejected_inputs.push(format!("{}: typed fact invalid ({err:?})", fact.fact_id));
                    input.input_issues.push(input_issue(
                        InputIssueCode::InvalidFact,
                        format!("typed fact invalid: {err}"),
                        [fact.fact_id.clone()],
                    ));
                    continue;
                }
                if fact.lifecycle != Lifecycle::Active {
                    input.input_issues.push(input_issue(
                        InputIssueCode::NonCurrentLifecycle,
                        format!(
                            "lifecycle {:?} is not current operational evidence",
                            fact.lifecycle
                        ),
                        [fact.fact_id.clone()],
                    ));
                    continue;
                }
                if fact.subject.plane.as_deref() != Some("operational") {
                    input.input_issues.push(input_issue(
                        InputIssueCode::NonOperationalPlane,
                        "fact is absent from or outside the operational truth plane",
                        [fact.fact_id.clone()],
                    ));
                    continue;
                }
                if fact.quality.completeness != Completeness::Complete {
                    input.input_issues.push(input_issue(
                        InputIssueCode::ScopeCoverageIncomplete,
                        format!(
                            "fact quality completeness is {:?}, not complete for its declared observation scope",
                            fact.quality.completeness
                        ),
                        [fact.fact_id.clone()],
                    ));
                }
                if is_expired_before_observation(fact) {
                    input.input_issues.push(input_issue(
                        InputIssueCode::ExpiredBeforeObservation,
                        "valid_until precedes the fact's own observation timestamp",
                        [fact.fact_id.clone()],
                    ));
                    continue;
                }
                let Some(fields) = &fact.payload.fields else {
                    let degraded = fact
                        .payload
                        .state
                        .is_some_and(|state| state != PayloadState::Empty);
                    input.input_issues.push(input_issue(
                        if degraded {
                            InputIssueCode::DegradedPayloadState
                        } else {
                            InputIssueCode::PayloadAbsent
                        },
                        format!(
                            "payload fields absent (state {:?}, completeness {:?})",
                            fact.payload.state, fact.quality.completeness
                        ),
                        [fact.fact_id.clone()],
                    ));
                    continue;
                };
                let payload: InterfaceStatePayload = match serde_json::from_value(fields.clone()) {
                    Ok(payload) => payload,
                    Err(_) => {
                        input.input_issues.push(input_issue(
                            InputIssueCode::PayloadUndecodable,
                            "interface payload undecodable",
                            [fact.fact_id.clone()],
                        ));
                        continue;
                    }
                };
                let Some(key) = InterfaceEntityKey::from_fact(fact) else {
                    input.input_issues.push(input_issue(
                        InputIssueCode::MissingEntity,
                        "canonical subject entity absent",
                        [fact.fact_id.clone()],
                    ));
                    continue;
                };
                let entity = fact
                    .subject
                    .entity
                    .clone()
                    .expect("entity key construction checked presence");
                input.rows.push(InterfaceRow {
                    key,
                    entity,
                    native_name: Some(payload.interface_name.clone()),
                    payload,
                    fact,
                });
            }
            scopes.push(analyze_batch(scope_key, input, multiple_batches));
        }
    }
    scopes.sort_by(|a, b| {
        (
            &a.device_id,
            &a.scope,
            &a.network_namespace,
            &a.observation_batch.collector_id,
            &a.observation_batch.capture_id,
            a.observation_batch.session_epoch,
        )
            .cmp(&(
                &b.device_id,
                &b.scope,
                &b.network_namespace,
                &b.observation_batch.collector_id,
                &b.observation_batch.capture_id,
                b.observation_batch.session_epoch,
            ))
    });
    rejected_inputs.sort();
    rejected_inputs.dedup();

    InterfaceRelationshipReport {
        scopes,
        rejected_inputs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_normalizer::{
        CaptureOutcome, CommandCapture, InterfaceObservation, NormalizerNos, ObservationContext,
        normalize,
    };
    use crate::typed_facts::{
        Completeness, EvidenceRef, FactPayload, FactSource, FactSubject, Quality, SCHEMA_VERSION,
        TrustClass,
    };
    use serde_json::json;

    const TS: &str = "2026-11-24T10:00:00Z";

    fn ctx<'a>() -> ObservationContext<'a> {
        ObservationContext {
            device_id: "leaf01",
            scope: None,
            network_namespace: Some("host"),
            collector_id: "collector-a",
            capture_id: "cap-iface-genuine",
            session_epoch: 1,
            sequence: 7,
            observed_at: TS,
            release_raw: None,
            platform: None,
            release_train: None,
            release_build: None,
            release_components: std::collections::BTreeMap::new(),
            platform_sku: None,
            asic: None,
            hardware_attributes: std::collections::BTreeMap::new(),
        }
    }

    fn genuine_cumulus_facts() -> Vec<TypedFact> {
        // Genuine upstream-derived committed fixture pair. This is not a
        // synthetic expected-value claim: the expected relationships below
        // are independently visible in the iproute2 master/link lines.
        let vtysh =
            include_str!("../tests/fixtures/bn024/interface-cumulus/vtysh-show-int-vrf-all.txt");
        let ip = include_str!("../tests/fixtures/bn024/interface-cumulus/ip-d-address-show.txt");
        normalize(&InterfaceObservation {
            nos: NormalizerNos::Cumulus,
            vtysh: CommandCapture::new(vtysh, CaptureOutcome::Complete),
            ip_addr: CommandCapture::new(ip, CaptureOutcome::Complete),
            context: ctx(),
        })
        .facts
    }

    fn payload_json(name: &str, interface_type: &str, oper: Option<&str>) -> serde_json::Value {
        json!({
            "interface_name": name,
            "administrative_state": {"state": "known", "value": "up"},
            "operational_state": match oper {
                Some(value) => json!({"state": "known", "value": value}),
                None => json!({"state": "unknown"}),
            },
            "ifindex": {"state": "known", "value": 3},
            "interface_type": {"state": "known", "value": interface_type},
            "mtu": {"state": "known", "value": 1500},
            "speed": {"state": "unknown"},
            "mac_address": {"state": "unknown"},
        })
    }

    fn synthetic_fact(name: &str, interface_type: &str, oper: Option<&str>) -> TypedFact {
        synthetic_fact_with_payload(name, payload_json(name, interface_type, oper))
    }

    fn synthetic_fact_with_payload(name: &str, mut fields: serde_json::Value) -> TypedFact {
        fields
            .as_object_mut()
            .expect("object payload")
            .insert("interface_name".into(), name.into());
        let mut fact = TypedFact {
            schema_version: SCHEMA_VERSION,
            fact_id: String::new(),
            fact_kind: FACT_KIND_INTERFACE_STATE.into(),
            subject: FactSubject {
                device_id: "leaf01".into(),
                scope: None,
                namespace: "interface".into(),
                network_namespace: Some("host".into()),
                entity: Some(name.into()),
                vrf: None,
                afi_safi: None,
                plane: Some("operational".into()),
                interface_identity: None,
            },
            source: FactSource {
                collector_id: "collector-a".into(),
                source_kind: "cli_ssh".into(),
                capture_id: "cap-1".into(),
                session_epoch: 1,
                sequence: 7,
                exporter_id: None,
            },
            observed_at: TS.into(),
            ingested_at: None,
            valid_from: None,
            valid_until: None,
            payload: FactPayload {
                produced_by: "synthetic_regression".into(),
                parser_revision: "synthetic-1".into(),
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
                kind: "synthetic_negative_control".into(),
                sha256: "a".repeat(64),
                locator: None,
            }],
        };
        fact.seal();
        fact.validate().expect("synthetic fact valid");
        fact
    }

    fn mutate_payload(fact: &mut TypedFact, update: impl FnOnce(&mut serde_json::Value)) {
        let mut fields = fact.payload.fields.clone().expect("fields");
        update(&mut fields);
        fact.payload.fields = Some(fields);
        fact.seal();
        fact.validate().expect("mutated fact valid");
    }

    fn lag_facts() -> Vec<TypedFact> {
        let aggregate = synthetic_fact("bond0", "port_channel", Some("up"));
        let member_payload = payload_json("eth1", "physical", Some("up"));
        let mut member = synthetic_fact_with_payload("eth1", member_payload);
        mutate_payload(&mut member, |fields| {
            fields["member_of"] = json!("bond0");
            fields["master_relationship"] = json!({"target": "bond0", "kind": "lag"});
        });
        vec![aggregate, member]
    }

    fn add_structured_identity(
        mut fact: TypedFact,
        kind: InterfaceIdentityKind,
        index: Option<u32>,
        parent: Option<&str>,
        native_name: &str,
    ) -> TypedFact {
        if kind == InterfaceIdentityKind::Subinterface {
            fact.subject.entity = Some(parent.expect("subif parent").into());
        }
        fact.subject.interface_identity = Some(crate::typed_facts::InterfaceSubjectIdentity {
            kind,
            parent_entity: parent.map(str::to_string),
            subinterface_index: index,
            vendor_native_name: Some(native_name.into()),
        });
        mutate_payload(&mut fact, |fields| {
            fields["interface_name"] = json!(native_name);
        });
        fact
    }

    #[test]
    fn genuine_cumulus_lag_bridge_and_vrf_relationships_are_reported() {
        let facts = genuine_cumulus_facts();
        let report = analyze_interface_relationships(&facts);
        assert!(report.rejected_inputs.is_empty());
        assert_eq!(report.scopes.len(), 1);
        let scope = &report.scopes[0];
        assert!(scope.interface_count > 20);

        assert!(
            !scope
                .lag_assessments
                .iter()
                .any(|l| l.aggregate == "bond01" && l.aggregate_type_known_lag)
        );
        assert!(
            scope
                .unresolved_relationships
                .iter()
                .any(|e| e.interface == "swp1" && e.target == "bond01")
        );

        let peerlink = scope
            .lag_assessments
            .iter()
            .find(|l| l.aggregate == "peerlink")
            .expect("peerlink assessment");
        assert_eq!(peerlink.member_count, 2);
        assert_eq!(peerlink.oper_up_members, 2);
        assert!(
            peerlink
                .findings
                .contains(&"observed_member_oper_up".to_string())
        );
        assert!(
            !peerlink
                .findings
                .iter()
                .any(|f| f.contains("active") || f.contains("capacity"))
        );
        assert!(peerlink.aggregate_fact_id.is_some());
        assert!(
            peerlink
                .members
                .iter()
                .all(|m| !m.state_fact_id.is_empty() && !m.relationship_fact_id.is_empty())
        );

        assert!(
            scope
                .bridge_memberships
                .iter()
                .any(|e| e.interface == "bond02" && e.target == "bridge")
        );
        assert!(
            scope
                .vrf_attachments
                .iter()
                .any(|v| v.interface == "vlan13" && v.network_instance == "evpn-vrf")
        );
    }

    #[test]
    fn structured_physical_and_subinterfaces_do_not_collapse() {
        let physical = add_structured_identity(
            synthetic_fact("Ethernet1", "physical", Some("up")),
            InterfaceIdentityKind::Physical,
            None,
            None,
            "Ethernet1",
        );
        let subzero = add_structured_identity(
            synthetic_fact("Ethernet1", "physical", Some("up")),
            InterfaceIdentityKind::Subinterface,
            Some(0),
            Some("Ethernet1"),
            "Ethernet1.0",
        );
        let sub100 = add_structured_identity(
            synthetic_fact("Ethernet1", "physical", Some("up")),
            InterfaceIdentityKind::Subinterface,
            Some(100),
            Some("Ethernet1"),
            "Ethernet1.100",
        );
        let sub200 = add_structured_identity(
            synthetic_fact("Ethernet1", "physical", Some("up")),
            InterfaceIdentityKind::Subinterface,
            Some(200),
            Some("Ethernet1"),
            "Ethernet1.200",
        );
        let report = analyze_interface_relationships(&[physical, subzero, sub100, sub200]);
        assert_eq!(report.scopes[0].interface_count, 4);
        let keys: BTreeSet<_> = report.scopes[0]
            .lag_assessments
            .iter()
            .flat_map(|l| l.members.iter().map(|m| m.interface_key.clone()))
            .collect();
        assert_eq!(keys.len(), 0, "no LAG evidence should exist");
    }

    #[test]
    fn identical_names_in_separate_network_namespaces_remain_separate() {
        let mut a = synthetic_fact("Ethernet1", "physical", Some("up"));
        a.subject.network_namespace = Some("asic-0".into());
        a.seal();
        let mut b = synthetic_fact("Ethernet1", "physical", Some("up"));
        b.subject.network_namespace = Some("asic-1".into());
        b.seal();
        let report = analyze_interface_relationships(&[a, b]);
        assert_eq!(report.scopes.len(), 2);
        assert!(report.scopes.iter().all(|s| s.interface_count == 1));
    }

    #[test]
    fn subinterface_cannot_be_selected_as_lag_aggregate() {
        let sub = add_structured_identity(
            synthetic_fact("Ethernet1", "port_channel", Some("up")),
            InterfaceIdentityKind::Subinterface,
            Some(100),
            Some("Ethernet1"),
            "Ethernet1.100",
        );
        let mut member = synthetic_fact("eth2", "physical", Some("up"));
        mutate_payload(&mut member, |fields| {
            fields["member_of"] = json!("Ethernet1");
            fields["master_relationship"] = json!({"target": "Ethernet1", "kind": "lag"});
        });
        let report = analyze_interface_relationships(&[sub, member]);
        let lag = report
            .scopes
            .iter()
            .flat_map(|s| &s.lag_assessments)
            .find(|l| l.aggregate == "Ethernet1")
            .expect("aggregate assessment");
        assert!(!lag.aggregate_type_known_lag);
        assert!(lag.aggregate_key.is_none());
        assert!(
            lag.findings
                .contains(&"aggregate_target_resolution_ambiguous_or_conflicting".to_string())
        );
        assert_eq!(lag.verdict, AssessmentVerdict::Unknown);
    }

    #[test]
    fn exact_duplicate_evidence_is_idempotent_and_permutation_deterministic() {
        let facts = lag_facts();
        let mut duplicated = facts.clone();
        duplicated.push(facts[1].clone());
        let report = analyze_interface_relationships(&duplicated);
        let lag = &report.scopes[0].lag_assessments[0];
        assert_eq!(lag.member_count, 1);

        let mut reversed = duplicated.clone();
        reversed.reverse();
        let reversed_report = analyze_interface_relationships(&reversed);
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            serde_json::to_value(&reversed_report).unwrap()
        );

        let shuffled = vec![
            duplicated[2].clone(),
            duplicated[0].clone(),
            duplicated[1].clone(),
        ];
        let shuffled_report = analyze_interface_relationships(&shuffled);
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            serde_json::to_value(&shuffled_report).unwrap()
        );
    }

    #[test]
    fn different_capture_ids_and_epochs_cannot_produce_merged_pass() {
        for mutate in ["capture", "epoch"] {
            let mut facts = lag_facts();
            if mutate == "capture" {
                facts[1].source.capture_id = "cap-2".into();
            } else {
                facts[1].source.session_epoch = 2;
            }
            facts[1].seal();
            let report = analyze_interface_relationships(&facts);
            assert!(report.scopes.len() >= 2);
            assert!(
                report
                    .scopes
                    .iter()
                    .all(|s| s.verdict != AssessmentVerdict::Pass)
            );
            assert!(report.scopes.iter().all(
                |s| s.input_contract_status == InputContractStatus::MultipleUnreconciledBatches
            ));
        }
    }

    #[test]
    fn withdrawn_superseded_expired_and_non_operational_facts_are_not_current() {
        for case in ["withdrawn", "superseded", "expired", "configured_plane"] {
            let mut facts = lag_facts();
            match case {
                "withdrawn" => {
                    facts[1] = facts[1]
                        .clone()
                        .withdraw("test withdrawal", "2026-11-24T10:01:00Z")
                        .unwrap();
                }
                "superseded" => {
                    facts[1] = facts[1]
                        .clone()
                        .supersede("b".repeat(64), "2026-11-24T10:01:00Z")
                        .unwrap();
                }
                "expired" => {
                    facts[1].valid_until = Some("2026-11-24T09:59:00Z".into());
                }
                _ => {
                    facts[1].subject.plane = Some("configured".into());
                    facts[1].seal();
                }
            }
            let report = analyze_interface_relationships(&facts);
            assert!(
                report
                    .scopes
                    .iter()
                    .all(|s| s.verdict != AssessmentVerdict::Pass),
                "{case} must not contribute to a passing operational verdict"
            );
        }
    }

    #[test]
    fn unknown_aggregate_state_and_unknown_membership_cannot_pass() {
        let mut facts = lag_facts();
        mutate_payload(&mut facts[0], |fields| {
            fields["operational_state"] = json!({"state": "unknown"});
        });
        let report = analyze_interface_relationships(&facts);
        let lag = &report.scopes[0].lag_assessments[0];
        assert_eq!(lag.verdict, AssessmentVerdict::Unknown);
        assert!(
            lag.findings
                .contains(&"aggregate_operational_state_unknown".to_string())
        );

        let aggregate_only = vec![synthetic_fact("bond0", "port_channel", Some("up"))];
        let report = analyze_interface_relationships(&aggregate_only);
        let lag = &report.scopes[0].lag_assessments[0];
        assert_eq!(lag.membership_status, LagMembershipStatus::Unknown);
        assert_eq!(lag.verdict, AssessmentVerdict::Unknown);
    }

    #[test]
    fn forward_membership_distinguishes_empty_partial_and_complete() {
        let mut complete = lag_facts();
        mutate_payload(&mut complete[0], |fields| {
            fields["lag_members"] = json!({"state": "known", "value": ["eth1"]});
        });
        let report = analyze_interface_relationships(&complete);
        assert_eq!(
            report.scopes[0].lag_assessments[0].membership_status,
            LagMembershipStatus::CompleteObserved
        );

        let mut empty = vec![synthetic_fact("bond0", "port_channel", Some("up"))];
        mutate_payload(&mut empty[0], |fields| {
            fields["lag_members"] = json!({"state": "known", "value": []});
        });
        let report = analyze_interface_relationships(&empty);
        assert_eq!(
            report.scopes[0].lag_assessments[0].membership_status,
            LagMembershipStatus::ConfirmedEmpty
        );

        let mut partial = lag_facts();
        mutate_payload(&mut partial[0], |fields| {
            fields["lag_members"] = json!({"state": "known", "value": ["eth1", "eth2"]});
        });
        let report = analyze_interface_relationships(&partial);
        assert_eq!(
            report.scopes[0].lag_assessments[0].membership_status,
            LagMembershipStatus::Partial
        );
        assert_eq!(
            report.scopes[0].lag_assessments[0].verdict,
            AssessmentVerdict::Unknown
        );
    }

    #[test]
    fn unresolved_evidence_is_unknown_not_not_applicable() {
        let mut fact = synthetic_fact("swp1", "physical", Some("up"));
        mutate_payload(&mut fact, |fields| {
            fields["master_relationship"] = json!({"target": "bond99", "kind": "unresolved"});
        });
        let report = analyze_interface_relationships(&[fact]);
        assert_eq!(report.scopes[0].verdict, AssessmentVerdict::Unknown);
        assert!(!report.scopes[0].unresolved_relationships.is_empty());
    }

    #[test]
    fn conflicting_same_generation_observations_are_explicit_and_order_independent() {
        let facts = lag_facts();
        let mut conflict = facts[1].clone();
        mutate_payload(&mut conflict, |fields| {
            fields["operational_state"] = json!({"state": "known", "value": "down"});
        });
        let input = vec![facts[0].clone(), facts[1].clone(), conflict.clone()];
        let mut reversed = input.clone();
        reversed.reverse();
        let a = analyze_interface_relationships(&input);
        let b = analyze_interface_relationships(&reversed);
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::to_value(&b).unwrap()
        );
        assert!(
            a.scopes[0]
                .input_issues
                .iter()
                .any(|i| i.code == InputIssueCode::ConflictingObservations)
        );
        assert_ne!(a.scopes[0].verdict, AssessmentVerdict::Pass);
    }

    #[test]
    fn operational_link_findings_do_not_claim_active_capacity_and_admin_down_is_not_fault() {
        let mut facts = lag_facts();
        mutate_payload(&mut facts[1], |fields| {
            fields["administrative_state"] = json!({"state": "known", "value": "down"});
            fields["operational_state"] = json!({"state": "known", "value": "down"});
        });
        let report = analyze_interface_relationships(&facts);
        let lag = &report.scopes[0].lag_assessments[0];
        assert_ne!(lag.verdict, AssessmentVerdict::Fail);
        assert!(
            lag.findings
                .contains(&"observed_member_oper_down".to_string())
        );
        assert!(
            lag.findings
                .contains(&"observed_member_admin_down".to_string())
        );
        assert!(
            !lag.findings
                .iter()
                .any(|f| f.contains("active") || f.contains("capacity"))
        );
    }

    #[test]
    fn incompatible_lag_target_type_remains_explicit() {
        let bridge_type = synthetic_fact("bond0", "virtual", Some("up"));
        let mut member = synthetic_fact("eth1", "physical", Some("up"));
        mutate_payload(&mut member, |fields| {
            fields["member_of"] = json!("bond0");
            fields["master_relationship"] = json!({"target": "bond0", "kind": "lag"});
        });
        let report = analyze_interface_relationships(&[bridge_type, member]);
        assert!(
            report.scopes[0]
                .relationship_issues
                .iter()
                .any(|i| i.code == RelationshipIssueCode::IncompatibleLagTargetType)
        );
        assert_eq!(report.scopes[0].verdict, AssessmentVerdict::Fail);
    }

    #[test]
    fn reverse_observed_membership_can_pass_only_as_narrow_consistency() {
        let report = analyze_interface_relationships(&lag_facts());
        let lag = &report.scopes[0].lag_assessments[0];
        assert_eq!(
            lag.membership_status,
            LagMembershipStatus::ReverseObservedOnly
        );
        assert_eq!(lag.verdict, AssessmentVerdict::Pass);
        assert_eq!(report.scopes[0].verdict, AssessmentVerdict::Pass);
        assert!(
            !lag.findings
                .iter()
                .any(|f| f.contains("active") || f.contains("capacity"))
        );
    }

    #[test]
    fn ambiguous_structured_target_is_explicit_unknown() {
        let physical = add_structured_identity(
            synthetic_fact("Ethernet1", "physical", Some("up")),
            InterfaceIdentityKind::Physical,
            None,
            None,
            "Ethernet1",
        );
        let logical = add_structured_identity(
            synthetic_fact("Ethernet1", "physical", Some("up")),
            InterfaceIdentityKind::Logical,
            None,
            None,
            "Ethernet1",
        );
        let mut member = synthetic_fact("eth2", "physical", Some("up"));
        mutate_payload(&mut member, |fields| {
            fields["master_relationship"] = json!({"target": "Ethernet1", "kind": "bridge"});
        });
        let report = analyze_interface_relationships(&[physical, logical, member]);
        let scope = &report.scopes[0];
        assert!(scope.unresolved_relationships.iter().any(|e| {
            e.target_resolution == RelationshipTargetResolution::AmbiguousStructuredTarget
        }));
        assert!(
            scope
                .relationship_issues
                .iter()
                .any(|i| i.code == RelationshipIssueCode::AmbiguousRelationshipTarget)
        );
        assert_eq!(scope.verdict, AssessmentVerdict::Unknown);
    }

    #[test]
    fn partial_completeness_and_unavailable_forward_membership_are_not_passable() {
        let mut facts = lag_facts();
        facts[0].quality.completeness = Completeness::Partial;
        facts[0].seal();
        mutate_payload(&mut facts[0], |fields| {
            fields["lag_members"] = json!({"state": "unknown"});
        });
        let report = analyze_interface_relationships(&facts);
        let scope = &report.scopes[0];
        assert!(
            scope
                .input_issues
                .iter()
                .any(|i| i.code == InputIssueCode::ScopeCoverageIncomplete)
        );
        assert!(
            scope
                .relationship_issues
                .iter()
                .any(|i| i.code == RelationshipIssueCode::UnavailableForwardMemberList)
        );
        assert!(
            scope.lag_assessments[0]
                .findings
                .contains(&"forward_membership_evidence_unavailable".to_string())
        );
        assert_eq!(scope.lag_assessments[0].verdict, AssessmentVerdict::Unknown);
        assert_eq!(scope.verdict, AssessmentVerdict::Unknown);
    }

    #[test]
    fn legacy_facts_without_structured_identity_remain_readable() {
        let facts = lag_facts();
        assert!(facts.iter().all(|f| f.subject.interface_identity.is_none()));
        let report = analyze_interface_relationships(&facts);
        assert_eq!(report.scopes[0].interface_count, 2);
        assert!(report.scopes[0].input_issues.is_empty());
    }
}
