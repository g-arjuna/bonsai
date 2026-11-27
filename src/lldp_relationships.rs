//! Deterministic LLDP adjacency reconciliation (BN-024-06).
//!
//! Bounded pure analysis over typed facts plus explicitly supplied identity
//! and acquisition-coverage evidence. It does not infer canonical device
//! ownership from a hostname or management address alone, does not coerce
//! opaque identifiers, and does not treat one-sided LLDP as proof of link
//! failure. Analysis time is explicit; wall-clock time is never read here.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::interface_facts::InterfaceStatePayload;
use crate::interface_normalizer::parse_rfc3339;
use crate::lldp_facts::{
    ChassisIdSubtype, LldpCaptureOutcome, LldpNeighborPayload, PopulationCoverage, PortIdSubtype,
};
use crate::semantic_evidence::{
    CoverageStreamEvidence, ExpectedFactContract, FreshnessPolicy, observation_freshness,
    select_usable_coverage, semantic_time_key, validate_fact_envelope,
};
use crate::typed_facts::{EvidenceRef, InterfaceSubjectIdentity, Lifecycle, TrustClass, TypedFact};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChassisIdentityEvidence {
    pub subtype: ChassisIdSubtype,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct DeviceIdentityEvidence {
    pub device_id: String,
    pub scope: Option<String>,
    pub network_namespace: Option<String>,
    /// Legacy MAC-form canonical chassis identifiers (normalized lowercase
    /// colon form). New evidence should use `typed_chassis_ids`.
    pub chassis_ids: Vec<String>,
    /// Subtype-aware independently established chassis identity. A raw LLDP
    /// value is compared only with evidence of the same subtype semantics.
    pub typed_chassis_ids: Vec<ChassisIdentityEvidence>,
    /// Canonical hostnames already established independently. Corroborating
    /// evidence only, never sole canonical authority.
    pub hostnames: Vec<String>,
    /// Canonical management addresses already established independently.
    /// Corroborating evidence only, never sole canonical authority.
    pub management_addresses: Vec<String>,
    /// Provenance for this independently established identity evidence.
    pub evidence_refs: Vec<EvidenceRef>,
}

/// Explicit acquisition-population evidence for a device/scope. Coverage is
/// usable for absence only when it is complete, current at analysis time,
/// non-conflicting and actually covers the interface being evaluated.
#[derive(Clone, Debug)]
pub struct CaptureCoverageEvidence {
    pub device_id: String,
    pub scope: Option<String>,
    pub network_namespace: Option<String>,
    /// Complete collector/source stream identity. Generations are ordered
    /// only within one collector+source+acquisition stream, never globally.
    pub collector_id: Option<String>,
    pub source_kind: Option<String>,
    pub capture_outcome: LldpCaptureOutcome,
    pub population_coverage: PopulationCoverage,
    pub capture_id: Option<String>,
    pub source_generation: Option<u64>,
    pub acquisition_scope: Option<String>,
    pub observation_time: Option<String>,
    pub valid_until: Option<String>,
    pub covered_interfaces: BTreeSet<String>,
    pub evidence_refs: Vec<EvidenceRef>,
}

impl CoverageStreamEvidence for CaptureCoverageEvidence {
    fn stream_key(&self) -> (String, String, String) {
        (
            self.collector_id.clone().unwrap_or_default(),
            self.source_kind.clone().unwrap_or_default(),
            self.acquisition_scope.clone().unwrap_or_default(),
        )
    }

    fn capture_id(&self) -> Option<&str> {
        self.capture_id.as_deref()
    }

    fn source_generation(&self) -> Option<u64> {
        self.source_generation
    }

    fn observation_time(&self) -> Option<&str> {
        self.observation_time.as_deref()
    }

    fn usable_for_absence(
        &self,
        as_of: crate::semantic_evidence::Time,
        policy: FreshnessPolicy,
    ) -> bool {
        let Some(observed) = self.observation_time.as_deref() else {
            return false;
        };
        self.capture_outcome == LldpCaptureOutcome::Complete
            && self.population_coverage == PopulationCoverage::Complete
            && observation_freshness(
                Lifecycle::Active,
                observed,
                None,
                self.valid_until.as_deref(),
                as_of,
                policy,
            )
            .is_current()
    }

    fn replay_signature(&self) -> String {
        serde_json::json!({
            "device_id": self.device_id,
            "scope": self.scope,
            "network_namespace": self.network_namespace,
            "stream": self.stream_key(),
            "capture_outcome": self.capture_outcome,
            "population_coverage": self.population_coverage,
            "capture_id": self.capture_id,
            "source_generation": self.source_generation,
            "observation_time": semantic_time_key(self.observation_time.as_deref()),
            "valid_until": semantic_time_key(self.valid_until.as_deref()),
            "covered_interfaces": self.covered_interfaces,
            "evidence_refs": self.evidence_refs,
        })
        .to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconciliationLimits {
    pub max_input_facts: usize,
    pub max_interface_facts: usize,
    pub max_candidates: usize,
    pub max_diagnostics: usize,
    pub max_output_assessments: usize,
}

impl Default for ReconciliationLimits {
    fn default() -> Self {
        Self {
            max_input_facts: 20_000,
            max_interface_facts: 20_000,
            max_candidates: 64,
            max_diagnostics: 128,
            max_output_assessments: 20_000,
        }
    }
}

pub struct AdjacencyReconciliationInput<'a> {
    pub lldp_facts: &'a [TypedFact],
    pub interface_facts: &'a [TypedFact],
    pub device_identities: &'a [DeviceIdentityEvidence],
    pub capture_coverage: &'a [CaptureCoverageEvidence],
    /// Explicit analysis time (RFC-3339). Required for all freshness results.
    pub as_of: &'a str,
    /// Explicit consumer freshness policy for capture coverage whose source
    /// did not establish its own validity boundary. This is a consumer policy,
    /// not fabricated source evidence; `None` keeps such coverage unknown.
    pub coverage_freshness: FreshnessPolicy,
    pub limits: Option<ReconciliationLimits>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStatus {
    Resolved,
    Candidate,
    Ambiguous,
    Unresolved,
    Conflicting,
    Stale,
    Withdrawn,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct EndpointCandidate {
    pub device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interface_identity: Option<InterfaceSubjectIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_fact_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct EndpointResolution {
    pub status: ResolutionStatus,
    pub candidates: Vec<EndpointCandidate>,
    pub reason: String,
}

impl EndpointResolution {
    fn unresolved(reason: impl Into<String>) -> Self {
        Self {
            status: ResolutionStatus::Unresolved,
            candidates: vec![],
            reason: reason.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReverseEvidenceStatus {
    Confirmed,
    Candidate,
    Conflicting,
    MissingReverse,
    CoverageUnknown,
    Stale,
    Withdrawn,
    NotResolvable,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IdentifierAgreement {
    Agree,
    Conflict,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessStatus {
    Current,
    Stale,
    Withdrawn,
    NotYetValid,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCompatibility {
    Compatible,
    Duplicate,
    Repeated,
    Conflicting,
    Unknown,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdjacencyAssessment {
    pub fact_id: String,
    pub local_device_id: String,
    pub scope: Option<String>,
    pub network_namespace: Option<String>,
    pub local_interface: String,
    pub local_interface_resolution: EndpointResolution,
    pub remote_chassis_id: Option<String>,
    pub remote_chassis_id_subtype: Option<String>,
    pub remote_port_id: Option<String>,
    pub remote_port_id_subtype: Option<String>,
    pub remote_system_name: Option<String>,
    pub remote_device_resolution: EndpointResolution,
    pub remote_interface_resolution: EndpointResolution,
    pub reverse_evidence: ReverseEvidenceStatus,
    pub identifier_agreement: IdentifierAgreement,
    pub freshness: FreshnessStatus,
    pub observation_compatibility: ObservationCompatibility,
    pub unresolved_prerequisites: Vec<String>,
    pub evidence_fact_ids: Vec<String>,
    pub supporting_interface_fact_ids: Vec<String>,
    pub identity_evidence_refs: Vec<EvidenceRef>,
    pub coverage_evidence_refs: Vec<EvidenceRef>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdjacencyReconciliationReport {
    pub input_lldp_facts: usize,
    pub parsed_lldp_facts: usize,
    pub assessments: usize,
    pub input_errors: Vec<String>,
    pub status_counts: BTreeMap<String, usize>,
    pub input_facts_omitted: usize,
    pub interface_facts_omitted: usize,
    pub candidate_evaluations_omitted: usize,
    pub output_assessments_omitted: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdjacencyReconciliationOutput {
    pub assessments: Vec<AdjacencyAssessment>,
    pub report: AdjacencyReconciliationReport,
}

#[derive(Clone)]
struct LldpEntry {
    fact: TypedFact,
    payload: LldpNeighborPayload,
}

#[derive(Clone)]
struct InterfaceEntry {
    fact_ids: Vec<String>,
    device_id: String,
    scope: Option<String>,
    network_namespace: Option<String>,
    entity: String,
    names: BTreeSet<String>,
    mac: Option<String>,
    ifindex: Option<u64>,
    identity: Option<InterfaceSubjectIdentity>,
}

type Scope = Option<String>;
type Ns = Option<String>;
type DeviceScopeKey = (String, Scope);
type DeviceEnvKey = (String, Scope, Ns);

fn freshness(fact: &TypedFact, as_of: chrono::DateTime<chrono::FixedOffset>) -> FreshnessStatus {
    match fact.lifecycle {
        Lifecycle::Withdrawn => return FreshnessStatus::Withdrawn,
        Lifecycle::Superseded | Lifecycle::Stale => return FreshnessStatus::Stale,
        Lifecycle::Active => {}
    }
    let Ok(observed) = chrono::DateTime::parse_from_rfc3339(&fact.observed_at) else {
        return FreshnessStatus::Unknown;
    };
    if observed > as_of {
        return FreshnessStatus::NotYetValid;
    }
    let Some(valid_until) = fact.valid_until.as_deref().and_then(parse_rfc3339) else {
        return FreshnessStatus::Unknown;
    };
    if valid_until <= as_of {
        return FreshnessStatus::Stale;
    }
    if let Some(valid_from) = fact.valid_from.as_deref().and_then(parse_rfc3339) {
        if valid_from > as_of {
            return FreshnessStatus::NotYetValid;
        }
    }
    FreshnessStatus::Current
}

fn payload_of(fact: &TypedFact) -> Option<LldpNeighborPayload> {
    if fact.fact_kind != crate::lldp_facts::FACT_KIND_LLDP_NEIGHBOR {
        return None;
    }
    let fields = fact.payload.fields.as_ref()?;
    serde_json::from_value(fields.clone()).ok()
}

fn bounded_error(errors: &mut Vec<String>, limits: ReconciliationLimits, msg: String) {
    if errors.len() < limits.max_diagnostics {
        errors.push(msg);
    }
}

fn interface_entries(
    facts: &[TypedFact],
    as_of: chrono::DateTime<chrono::FixedOffset>,
    limits: ReconciliationLimits,
) -> (Vec<InterfaceEntry>, Vec<String>, usize) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let mut omitted = 0usize;
    for fact in facts.iter().take(limits.max_interface_facts) {
        if fact.fact_kind != crate::interface_facts::FACT_KIND_INTERFACE_STATE {
            continue;
        }
        if fact.subject.namespace != "interface"
            || fact.subject.plane.as_deref() != Some("operational")
        {
            bounded_error(
                &mut errors,
                limits,
                format!(
                    "{}: interface fact has wrong namespace/plane; skipped",
                    fact.fact_id
                ),
            );
            continue;
        }
        if let Err(e) = fact.validate() {
            bounded_error(
                &mut errors,
                limits,
                format!(
                    "{}: interface fact failed typed validation ({e}); skipped",
                    fact.fact_id
                ),
            );
            continue;
        }
        if freshness(fact, as_of) != FreshnessStatus::Current {
            // Lifecycle/temporal applicability is enforced before any name,
            // MAC or ifindex mapping may use this evidence.
            continue;
        }
        let Some(fields) = fact.payload.fields.clone() else {
            bounded_error(
                &mut errors,
                limits,
                format!("{}: interface fact has no typed fields", fact.fact_id),
            );
            continue;
        };
        let payload: InterfaceStatePayload = match serde_json::from_value(fields) {
            Ok(p) => p,
            Err(e) => {
                bounded_error(
                    &mut errors,
                    limits,
                    format!("{}: interface payload malformed: {e}", fact.fact_id),
                );
                continue;
            }
        };
        let Some(entity) = fact.subject.entity.clone() else {
            bounded_error(
                &mut errors,
                limits,
                format!("{}: interface fact has no entity", fact.fact_id),
            );
            continue;
        };
        let mut names = BTreeSet::from([entity.clone(), payload.interface_name.clone()]);
        if let Some(n) = payload.normalized_name {
            names.insert(n);
        }
        if let Some(identity) = &fact.subject.interface_identity {
            if let Some(native) = &identity.vendor_native_name {
                names.insert(native.clone());
            }
        }
        out.push(InterfaceEntry {
            fact_ids: vec![fact.fact_id.clone()],
            device_id: fact.subject.device_id.clone(),
            scope: fact.subject.scope.clone(),
            network_namespace: fact.subject.network_namespace.clone(),
            entity,
            names,
            mac: payload.mac_address.value,
            ifindex: payload.ifindex.value,
            identity: fact.subject.interface_identity.clone(),
        });
    }
    if facts.len() > limits.max_interface_facts {
        omitted = facts.len() - limits.max_interface_facts;
    }
    (merge_consistent_interface_entries(out), errors, omitted)
}

fn merge_consistent_interface_entries(entries: Vec<InterfaceEntry>) -> Vec<InterfaceEntry> {
    type EndpointKey = (String, Scope, Ns, String, String);
    type ExactEvidenceKey = (Vec<String>, Option<String>, Option<u64>);
    let mut endpoint_groups: BTreeMap<EndpointKey, Vec<InterfaceEntry>> = BTreeMap::new();
    for entry in entries {
        let identity = serde_json::to_string(&entry.identity).unwrap_or_default();
        endpoint_groups
            .entry((
                entry.device_id.clone(),
                entry.scope.clone(),
                entry.network_namespace.clone(),
                entry.entity.clone(),
                identity,
            ))
            .or_default()
            .push(entry);
    }

    let mut merged = Vec::new();
    for mut group in endpoint_groups.into_values() {
        let macs: BTreeSet<String> = group
            .iter()
            .filter_map(|e| e.mac.as_deref().map(str::to_ascii_lowercase))
            .collect();
        let ifindexes: BTreeSet<u64> = group.iter().filter_map(|e| e.ifindex).collect();
        if macs.len() <= 1 && ifindexes.len() <= 1 {
            // Absence is unknown, not negative evidence. Multiple aliases for
            // one canonical endpoint can be unioned; distinct known MAC or
            // ifindex values cannot. This merge is independent of input order.
            let mut first = group.remove(0);
            for entry in group {
                first.names.extend(entry.names);
                if first.mac.is_none() {
                    first.mac = entry.mac;
                }
                if first.ifindex.is_none() {
                    first.ifindex = entry.ifindex;
                }
                first.fact_ids.extend(entry.fact_ids);
            }
            first.fact_ids.sort();
            first.fact_ids.dedup();
            merged.push(first);
            continue;
        }

        // Contradictory mappings remain visible as separate candidates. Exact
        // replay duplicates inside one contradictory evidence class are still
        // deduplicated deterministically.
        let mut exact: BTreeMap<ExactEvidenceKey, InterfaceEntry> = BTreeMap::new();
        for entry in group {
            let key = (
                entry.names.iter().cloned().collect(),
                entry.mac.as_deref().map(str::to_ascii_lowercase),
                entry.ifindex,
            );
            match exact.get_mut(&key) {
                Some(existing) => {
                    existing.fact_ids.extend(entry.fact_ids);
                    existing.fact_ids.sort();
                    existing.fact_ids.dedup();
                }
                None => {
                    exact.insert(key, entry);
                }
            }
        }
        merged.extend(exact.into_values());
    }
    merged
}

#[derive(Default)]
struct InterfaceIndexes {
    by_device_scope: BTreeMap<DeviceScopeKey, Vec<usize>>,
    name_remote: BTreeMap<(DeviceScopeKey, String), Vec<usize>>,
    mac_remote: BTreeMap<(DeviceScopeKey, String), Vec<usize>>,
    ifindex_remote: BTreeMap<(DeviceScopeKey, u64), Vec<usize>>,
    name_local: BTreeMap<(DeviceEnvKey, String), Vec<usize>>,
}

fn build_interface_indexes(interfaces: &[InterfaceEntry]) -> InterfaceIndexes {
    let mut idx = InterfaceIndexes::default();
    for (i, entry) in interfaces.iter().enumerate() {
        let remote_key = (entry.device_id.clone(), entry.scope.clone());
        idx.by_device_scope
            .entry(remote_key.clone())
            .or_default()
            .push(i);
        let local_key = (
            entry.device_id.clone(),
            entry.scope.clone(),
            entry.network_namespace.clone(),
        );
        for name in &entry.names {
            idx.name_remote
                .entry((remote_key.clone(), name.clone()))
                .or_default()
                .push(i);
            idx.name_local
                .entry((local_key.clone(), name.clone()))
                .or_default()
                .push(i);
        }
        if let Some(mac) = &entry.mac {
            idx.mac_remote
                .entry((remote_key.clone(), mac.to_ascii_lowercase()))
                .or_default()
                .push(i);
        }
        if let Some(ifindex) = entry.ifindex {
            idx.ifindex_remote
                .entry((remote_key, ifindex))
                .or_default()
                .push(i);
        }
    }
    idx
}

fn candidates_for(
    interfaces: &[InterfaceEntry],
    indexes: &[usize],
    with_interface: bool,
) -> Vec<EndpointCandidate> {
    indexes
        .iter()
        .map(|i| {
            let entry = &interfaces[*i];
            EndpointCandidate {
                device_id: entry.device_id.clone(),
                interface: with_interface.then(|| entry.entity.clone()),
                interface_identity: entry.identity.clone(),
                evidence_fact_ids: entry.fact_ids.clone(),
            }
        })
        .collect()
}

fn resolve_local_interface(
    entry: &LldpEntry,
    interfaces: &[InterfaceEntry],
    indexes: &InterfaceIndexes,
) -> EndpointResolution {
    let key = (
        entry.fact.subject.device_id.clone(),
        entry.fact.subject.scope.clone(),
        entry.fact.subject.network_namespace.clone(),
    );
    let entity = entry
        .fact
        .subject
        .entity
        .as_deref()
        .unwrap_or(&entry.payload.local_interface_name);
    let mut matches = indexes
        .name_local
        .get(&(key.clone(), entity.to_string()))
        .cloned()
        .unwrap_or_default();
    if matches.is_empty() {
        matches = indexes
            .name_local
            .get(&(key, entry.payload.local_interface_name.clone()))
            .cloned()
            .unwrap_or_default();
    }
    match matches.len() {
        0 => EndpointResolution::unresolved(
            "no current independently validated interface evidence for local endpoint",
        ),
        1 => EndpointResolution {
            status: ResolutionStatus::Resolved,
            candidates: candidates_for(interfaces, &matches, true),
            reason: "unique current local interface evidence".into(),
        },
        _ => EndpointResolution {
            status: ResolutionStatus::Ambiguous,
            candidates: candidates_for(interfaces, &matches, true),
            reason: "multiple current local interface records match endpoint".into(),
        },
    }
}

#[derive(Default)]
struct IdentityIndexes {
    chassis: BTreeMap<(Scope, ChassisIdSubtype, String), BTreeSet<usize>>,
    hostname: BTreeMap<(Scope, String), BTreeSet<usize>>,
    mgmt: BTreeMap<(Scope, String), BTreeSet<usize>>,
}

fn typed_chassis_values(id: &DeviceIdentityEvidence) -> Vec<ChassisIdentityEvidence> {
    let mut out = id.typed_chassis_ids.clone();
    for mac in &id.chassis_ids {
        out.push(ChassisIdentityEvidence {
            subtype: ChassisIdSubtype::MacAddress,
            value: mac.to_ascii_lowercase(),
        });
    }
    out.sort();
    out.dedup();
    out
}

fn build_identity_indexes(identities: &[DeviceIdentityEvidence]) -> IdentityIndexes {
    let mut idx = IdentityIndexes::default();
    for (i, id) in identities.iter().enumerate() {
        for chassis in typed_chassis_values(id) {
            idx.chassis
                .entry((id.scope.clone(), chassis.subtype, chassis.value))
                .or_default()
                .insert(i);
        }
        for host in &id.hostnames {
            idx.hostname
                .entry((id.scope.clone(), host.clone()))
                .or_default()
                .insert(i);
        }
        for mgmt in &id.management_addresses {
            idx.mgmt
                .entry((id.scope.clone(), mgmt.clone()))
                .or_default()
                .insert(i);
        }
    }
    idx
}

fn identity_candidate(id: &DeviceIdentityEvidence, interface: Option<String>) -> EndpointCandidate {
    EndpointCandidate {
        device_id: id.device_id.clone(),
        interface,
        interface_identity: None,
        evidence_fact_ids: vec![],
    }
}

fn resolve_remote_device(
    entry: &LldpEntry,
    identities: &[DeviceIdentityEvidence],
    indexes: &IdentityIndexes,
) -> EndpointResolution {
    let scope = entry.fact.subject.scope.clone();
    if let (Some(chassis), Some(subtype)) = (
        entry.payload.remote_chassis_id.value.as_deref(),
        entry.payload.remote_chassis_id_subtype.value,
    ) {
        let value = if subtype == ChassisIdSubtype::MacAddress {
            chassis.to_ascii_lowercase()
        } else {
            chassis.to_string()
        };
        if let Some(matches) = indexes.chassis.get(&(scope.clone(), subtype, value)) {
            match matches.len() {
                1 => {
                    let id = &identities[*matches.iter().next().expect("one")];
                    return EndpointResolution {
                        status: ResolutionStatus::Resolved,
                        candidates: vec![identity_candidate(id, None)],
                        reason: "unique subtype-aware independently established chassis identifier"
                            .into(),
                    };
                }
                n if n > 1 => {
                    return EndpointResolution {
                        status: ResolutionStatus::Conflicting,
                        candidates: matches
                            .iter()
                            .map(|i| identity_candidate(&identities[*i], None))
                            .collect(),
                        reason: "subtype-aware chassis identifier matches multiple independent device identities".into(),
                    };
                }
                _ => {}
            }
        }
    }

    let mut candidates = BTreeSet::new();
    if let Some(hostname) = entry.payload.remote_system_name.value.as_deref() {
        if let Some(matches) = indexes.hostname.get(&(scope.clone(), hostname.to_string())) {
            candidates.extend(matches.iter().copied());
        }
    }
    if let Some(mgmt) = entry.payload.remote_management_addresses.value.as_ref() {
        for addr in mgmt {
            if let Some(matches) = indexes.mgmt.get(&(scope.clone(), addr.address.clone())) {
                candidates.extend(matches.iter().copied());
            }
        }
    }
    if candidates.is_empty() {
        return EndpointResolution::unresolved(
            "no subtype-aware chassis identity; hostname/management evidence absent",
        );
    }
    let status = if candidates.len() == 1 {
        ResolutionStatus::Candidate
    } else {
        ResolutionStatus::Ambiguous
    };
    EndpointResolution {
        status,
        candidates: candidates
            .iter()
            .map(|i| identity_candidate(&identities[*i], None))
            .collect(),
        reason: "hostname/management-address evidence is corroborating only, not sole canonical authority".into(),
    }
}

fn resolve_remote_interface(
    entry: &LldpEntry,
    device_resolution: &EndpointResolution,
    interfaces: &[InterfaceEntry],
    indexes: &InterfaceIndexes,
) -> EndpointResolution {
    if device_resolution.status != ResolutionStatus::Resolved {
        return EndpointResolution::unresolved(
            "remote device is not resolved by independent identity evidence",
        );
    }
    let device = &device_resolution.candidates[0].device_id;
    let key = (device.clone(), entry.fact.subject.scope.clone());
    let Some(port) = entry.payload.remote_port_id.value.as_deref() else {
        return EndpointResolution::unresolved("remote port identifier absent");
    };
    let matches = match entry.payload.remote_port_id_subtype.value {
        Some(PortIdSubtype::InterfaceName) => indexes
            .name_remote
            .get(&(key, port.to_string()))
            .cloned()
            .unwrap_or_default(),
        Some(PortIdSubtype::MacAddress) => indexes
            .mac_remote
            .get(&(key, port.to_ascii_lowercase()))
            .cloned()
            .unwrap_or_default(),
        Some(PortIdSubtype::IfIndex) => port
            .parse::<u64>()
            .ok()
            .and_then(|i| indexes.ifindex_remote.get(&(key, i)).cloned())
            .unwrap_or_default(),
        Some(PortIdSubtype::LocallyAssigned) => {
            return EndpointResolution::unresolved(
                "remote port identifier is locally assigned opaque evidence; no interpretation is assumed",
            );
        }
        Some(PortIdSubtype::NetworkAddress | PortIdSubtype::InterfaceAlias) => {
            return EndpointResolution::unresolved(
                "remote port identifier subtype is not interface-name/MAC/ifindex authority",
            );
        }
        Some(PortIdSubtype::Unknown) | None => {
            return EndpointResolution::unresolved(
                "remote port identifier subtype unknown or absent",
            );
        }
    };
    match matches.len() {
        0 => EndpointResolution::unresolved(format!(
            "no current interface on {device} matches subtype-aware remote port identifier"
        )),
        1 => EndpointResolution {
            status: ResolutionStatus::Resolved,
            candidates: candidates_for(interfaces, &matches, true),
            reason: "unique subtype-aware interface match in current independent evidence".into(),
        },
        _ => EndpointResolution {
            status: ResolutionStatus::Ambiguous,
            candidates: candidates_for(interfaces, &matches, true),
            reason: "multiple interfaces match the remote port identifier across device-local namespaces".into(),
        },
    }
}

fn canonical_local_key(entry: &LldpEntry, resolution: &EndpointResolution) -> String {
    if resolution.status == ResolutionStatus::Resolved {
        if let Some(interface) = resolution.candidates[0].interface.as_deref() {
            return interface.to_string();
        }
    }
    entry
        .payload
        .normalized_local_interface_name
        .clone()
        .unwrap_or_else(|| entry.payload.local_interface_name.clone())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReverseEvaluation {
    Agree,
    Conflict,
    Unknown,
    NotApplicable,
}

fn chassis_relation(
    reverse: &LldpEntry,
    local_identity: Option<&DeviceIdentityEvidence>,
) -> ReverseEvaluation {
    let Some(identity) = local_identity else {
        return ReverseEvaluation::Unknown;
    };
    let (Some(value), Some(subtype)) = (
        reverse.payload.remote_chassis_id.value.as_deref(),
        reverse.payload.remote_chassis_id_subtype.value,
    ) else {
        return ReverseEvaluation::Unknown;
    };
    let value = if subtype == ChassisIdSubtype::MacAddress {
        value.to_ascii_lowercase()
    } else {
        value.to_string()
    };
    let values: Vec<_> = typed_chassis_values(identity)
        .into_iter()
        .filter(|c| c.subtype == subtype)
        .collect();
    if values.is_empty() {
        return ReverseEvaluation::Unknown;
    }
    if values.iter().any(|c| c.value == value) {
        ReverseEvaluation::Agree
    } else {
        ReverseEvaluation::NotApplicable
    }
}

fn reverse_port_matches(reverse: &LldpEntry, local_interface: &InterfaceEntry) -> Option<bool> {
    let port = reverse.payload.remote_port_id.value.as_deref()?;
    match reverse.payload.remote_port_id_subtype.value {
        Some(PortIdSubtype::InterfaceName) => Some(local_interface.names.iter().any(|n| n == port)),
        Some(PortIdSubtype::MacAddress) => local_interface
            .mac
            .as_deref()
            .map(|m| m.eq_ignore_ascii_case(port)),
        Some(PortIdSubtype::IfIndex) => port
            .parse::<u64>()
            .ok()
            .and_then(|idx| local_interface.ifindex.map(|v| v == idx)),
        _ => None,
    }
}

fn advertised_corroboration_conflicts(
    reverse: &LldpEntry,
    local_identity: &DeviceIdentityEvidence,
) -> bool {
    if let Some(system) = reverse.payload.remote_system_name.value.as_deref() {
        if !local_identity.hostnames.is_empty()
            && !local_identity.hostnames.iter().any(|h| h == system)
        {
            return true;
        }
    }
    if let Some(addrs) = reverse.payload.remote_management_addresses.value.as_ref() {
        if !local_identity.management_addresses.is_empty() {
            for addr in addrs {
                if !local_identity
                    .management_addresses
                    .iter()
                    .any(|m| m == &addr.address)
                {
                    return true;
                }
            }
        }
    }
    false
}

fn evaluate_reverse(
    reverse: &LldpEntry,
    local_identity: Option<&DeviceIdentityEvidence>,
    local_interface: Option<&InterfaceEntry>,
) -> ReverseEvaluation {
    let device = chassis_relation(reverse, local_identity);
    if device == ReverseEvaluation::NotApplicable {
        // A different chassis on the same shared interface is a distinct
        // legitimate neighbor, not a contradictory observation of this one.
        return ReverseEvaluation::NotApplicable;
    }
    let Some(local_interface) = local_interface else {
        return ReverseEvaluation::Unknown;
    };
    let corroboration_conflict = local_identity
        .map(|id| advertised_corroboration_conflicts(reverse, id))
        .unwrap_or(false);
    match reverse_port_matches(reverse, local_interface) {
        Some(true) if device == ReverseEvaluation::Agree && corroboration_conflict => {
            ReverseEvaluation::Conflict
        }
        Some(true) if device == ReverseEvaluation::Agree => ReverseEvaluation::Agree,
        Some(true) => ReverseEvaluation::Unknown,
        Some(false) if device == ReverseEvaluation::Agree => ReverseEvaluation::Conflict,
        Some(false) => ReverseEvaluation::Unknown,
        None => ReverseEvaluation::Unknown,
    }
}

fn coverage_absence_evidence<'a>(
    remote_device: &str,
    scope: &Scope,
    interface_names: &BTreeSet<String>,
    network_namespaces: &BTreeSet<Ns>,
    coverage: &'a [CaptureCoverageEvidence],
    coverage_index: &BTreeMap<(String, Scope), Vec<usize>>,
    as_of: chrono::DateTime<chrono::FixedOffset>,
    coverage_freshness: FreshnessPolicy,
) -> (bool, Vec<EvidenceRef>) {
    let key = (remote_device.to_string(), scope.clone());
    let candidates = coverage_index
        .get(&key)
        .into_iter()
        .flat_map(|v| v.iter())
        .map(|i| &coverage[*i])
        .filter(|c| {
            network_namespaces.contains(&c.network_namespace)
                && !c.covered_interfaces.is_empty()
                && c.covered_interfaces
                    .intersection(interface_names)
                    .next()
                    .is_some()
        });

    // Shared stream selection owns exact-replay consolidation, contradictory
    // same-capture detection, parsed timestamp ordering, future exclusion and
    // newest-before-completeness selection. LLDP-specific applicability remains
    // the device/scope/namespace/interface filter above.
    if let Some(selected) = select_usable_coverage(candidates, as_of, coverage_freshness) {
        (true, selected.evidence_refs.clone())
    } else {
        (false, vec![])
    }
}

fn reverse_status(
    local_index: usize,
    entries: &[LldpEntry],
    local_resolutions: &[EndpointResolution],
    reverse_index: &BTreeMap<(String, Scope, String), Vec<usize>>,
    observation_indexes: &ObservationIndexes,
    interfaces: &[InterfaceEntry],
    device_resolution: &EndpointResolution,
    interface_resolution: &EndpointResolution,
    identities: &[DeviceIdentityEvidence],
    identity_by_device_scope: &BTreeMap<(String, Scope), usize>,
    interface_by_fact_id: &BTreeMap<String, usize>,
    coverage: &[CaptureCoverageEvidence],
    coverage_index: &BTreeMap<(String, Scope), Vec<usize>>,
    as_of: chrono::DateTime<chrono::FixedOffset>,
    coverage_freshness: FreshnessPolicy,
    limits: ReconciliationLimits,
) -> (
    ReverseEvidenceStatus,
    IdentifierAgreement,
    Vec<String>,
    Vec<EvidenceRef>,
    usize,
) {
    let local = &entries[local_index];
    if device_resolution.status != ResolutionStatus::Resolved
        || interface_resolution.status != ResolutionStatus::Resolved
    {
        return (
            ReverseEvidenceStatus::NotResolvable,
            IdentifierAgreement::Unknown,
            vec![],
            vec![],
            0,
        );
    }
    match freshness(&local.fact, as_of) {
        FreshnessStatus::Withdrawn => {
            return (
                ReverseEvidenceStatus::Withdrawn,
                IdentifierAgreement::Unknown,
                vec![],
                vec![],
                0,
            );
        }
        FreshnessStatus::Current => {}
        _ => {
            return (
                ReverseEvidenceStatus::Stale,
                IdentifierAgreement::Unknown,
                vec![],
                vec![],
                0,
            );
        }
    }
    let remote_device = device_resolution.candidates[0].device_id.clone();
    let remote_interface = interface_resolution.candidates[0]
        .interface
        .clone()
        .unwrap_or_default();
    let key = (
        remote_device.clone(),
        local.fact.subject.scope.clone(),
        remote_interface.clone(),
    );
    let candidate_indexes = reverse_index.get(&key).cloned().unwrap_or_default();
    if candidate_indexes.is_empty() {
        let mut names = BTreeSet::from([remote_interface]);
        let mut network_namespaces = BTreeSet::new();
        for candidate in &interface_resolution.candidates {
            if let Some(interface) = &candidate.interface {
                names.insert(interface.clone());
            }
            if let Some(entry) = candidate
                .evidence_fact_ids
                .first()
                .and_then(|fid| interface_by_fact_id.get(fid))
                .map(|i| &interfaces[*i])
            {
                names.extend(entry.names.iter().cloned());
                network_namespaces.insert(entry.network_namespace.clone());
            }
        }
        let (covered, coverage_refs) = coverage_absence_evidence(
            &remote_device,
            &local.fact.subject.scope,
            &names,
            &network_namespaces,
            coverage,
            coverage_index,
            as_of,
            coverage_freshness,
        );
        return (
            if covered {
                ReverseEvidenceStatus::MissingReverse
            } else {
                ReverseEvidenceStatus::CoverageUnknown
            },
            IdentifierAgreement::Unknown,
            vec![],
            coverage_refs,
            0,
        );
    }

    // Device identity is device/tenant scoped. network_namespace is a
    // device-local placement axis for interfaces and must not split one
    // device's chassis identity into unrelated identities.
    let local_identity = identity_by_device_scope
        .get(&(
            local.fact.subject.device_id.clone(),
            local.fact.subject.scope.clone(),
        ))
        .map(|i| &identities[*i]);
    let local_if = if local_resolutions[local_index].status == ResolutionStatus::Resolved {
        local_resolutions[local_index].candidates[0]
            .evidence_fact_ids
            .first()
            .and_then(|fid| interface_by_fact_id.get(fid))
            .map(|i| &interfaces[*i])
    } else {
        None
    };
    let mut saw_stale = false;
    let mut saw_withdrawn = false;
    let mut agree = 0usize;
    let mut conflict = 0usize;
    let mut unknown = 0usize;
    let mut evidence = BTreeSet::new();
    let mut omitted = 0usize;
    for (n, idx) in candidate_indexes.iter().copied().enumerate() {
        if n >= limits.max_candidates {
            omitted += 1;
            continue;
        }
        let reverse = &entries[idx];
        match freshness(&reverse.fact, as_of) {
            FreshnessStatus::Withdrawn => {
                saw_withdrawn = true;
                continue;
            }
            FreshnessStatus::Current => {}
            _ => {
                saw_stale = true;
                continue;
            }
        }
        if compatibility(idx, observation_indexes) == ObservationCompatibility::Conflicting {
            conflict += 1;
            evidence.insert(reverse.fact.fact_id.clone());
            continue;
        }
        match evaluate_reverse(reverse, local_identity, local_if) {
            ReverseEvaluation::Agree => {
                agree += 1;
                evidence.insert(reverse.fact.fact_id.clone());
            }
            ReverseEvaluation::Conflict => {
                conflict += 1;
                evidence.insert(reverse.fact.fact_id.clone());
            }
            ReverseEvaluation::Unknown => {
                unknown += 1;
                evidence.insert(reverse.fact.fact_id.clone());
            }
            ReverseEvaluation::NotApplicable => {}
        }
    }
    let evidence: Vec<String> = evidence.into_iter().collect();
    if agree > 0 && conflict == 0 {
        return (
            ReverseEvidenceStatus::Confirmed,
            IdentifierAgreement::Agree,
            evidence,
            vec![],
            omitted,
        );
    }
    if conflict > 0 {
        return (
            ReverseEvidenceStatus::Conflicting,
            IdentifierAgreement::Conflict,
            evidence,
            vec![],
            omitted,
        );
    }
    if unknown > 0 || agree > 0 {
        return (
            ReverseEvidenceStatus::Candidate,
            IdentifierAgreement::Unknown,
            evidence,
            vec![],
            omitted,
        );
    }
    if saw_withdrawn {
        (
            ReverseEvidenceStatus::Withdrawn,
            IdentifierAgreement::Unknown,
            vec![],
            vec![],
            omitted,
        )
    } else if saw_stale {
        (
            ReverseEvidenceStatus::Stale,
            IdentifierAgreement::Unknown,
            vec![],
            vec![],
            omitted,
        )
    } else {
        // Candidates existed at the local-interface key but every one was a
        // clearly different neighbor. That is multiple-neighbor evidence,
        // not absence or contradiction for this adjacency.
        (
            ReverseEvidenceStatus::CoverageUnknown,
            IdentifierAgreement::Unknown,
            vec![],
            vec![],
            omitted,
        )
    }
}

fn observation_identity(payload: &LldpNeighborPayload) -> String {
    serde_json::json!([
        payload.remote_chassis_id_subtype.value,
        payload.remote_chassis_id.value,
        payload.remote_port_id_subtype.value,
        payload.remote_port_id.value,
    ])
    .to_string()
}

fn non_temporal_observation(payload: &LldpNeighborPayload) -> String {
    serde_json::json!([
        payload.remote_chassis_id_subtype.value,
        payload.remote_chassis_id.value,
        payload.remote_port_id_subtype.value,
        payload.remote_port_id.value,
        payload.remote_system_name,
        payload.remote_port_description,
        payload.remote_management_addresses,
        payload.capabilities,
    ])
    .to_string()
}

fn full_observation(payload: &LldpNeighborPayload) -> String {
    serde_json::to_string(payload).expect("LLDP payload serializes")
}

type EndpointKey = (String, Scope, Ns, String);
type ObservationIdentityKey = (EndpointKey, String);
type EvidenceObservationGroups = BTreeMap<String, BTreeMap<String, usize>>;

struct ObservationIndexes {
    endpoint_counts: BTreeMap<EndpointKey, usize>,
    identity_groups: BTreeMap<ObservationIdentityKey, EvidenceObservationGroups>,
    endpoint_keys: Vec<EndpointKey>,
    identity_keys: Vec<String>,
    evidence_keys: Vec<String>,
    full_keys: Vec<String>,
}

fn endpoint_key(entry: &LldpEntry, local_key: &str) -> EndpointKey {
    (
        entry.fact.subject.device_id.clone(),
        entry.fact.subject.scope.clone(),
        entry.fact.subject.network_namespace.clone(),
        local_key.to_string(),
    )
}

fn build_observation_indexes(entries: &[LldpEntry], local_keys: &[String]) -> ObservationIndexes {
    let mut indexes = ObservationIndexes {
        endpoint_counts: BTreeMap::new(),
        identity_groups: BTreeMap::new(),
        endpoint_keys: Vec::with_capacity(entries.len()),
        identity_keys: Vec::with_capacity(entries.len()),
        evidence_keys: Vec::with_capacity(entries.len()),
        full_keys: Vec::with_capacity(entries.len()),
    };
    for (i, entry) in entries.iter().enumerate() {
        let endpoint = endpoint_key(entry, &local_keys[i]);
        let identity = observation_identity(&entry.payload);
        let evidence = non_temporal_observation(&entry.payload);
        let full = full_observation(&entry.payload);
        *indexes.endpoint_counts.entry(endpoint.clone()).or_insert(0) += 1;
        *indexes
            .identity_groups
            .entry((endpoint.clone(), identity.clone()))
            .or_default()
            .entry(evidence.clone())
            .or_default()
            .entry(full.clone())
            .or_insert(0) += 1;
        indexes.endpoint_keys.push(endpoint);
        indexes.identity_keys.push(identity);
        indexes.evidence_keys.push(evidence);
        indexes.full_keys.push(full);
    }
    indexes
}

fn compatibility(entry_index: usize, indexes: &ObservationIndexes) -> ObservationCompatibility {
    let endpoint = &indexes.endpoint_keys[entry_index];
    if indexes.endpoint_counts.get(endpoint).copied().unwrap_or(0) <= 1 {
        return ObservationCompatibility::Compatible;
    }
    let key = (endpoint.clone(), indexes.identity_keys[entry_index].clone());
    let Some(group) = indexes.identity_groups.get(&key) else {
        return ObservationCompatibility::Compatible;
    };
    let mut duplicate = false;
    let mut repeated = false;
    let mut conflict = false;
    for (evidence, fulls) in group {
        if *evidence == indexes.evidence_keys[entry_index] {
            repeated |= fulls
                .keys()
                .any(|full| *full != indexes.full_keys[entry_index]);
            duplicate |= fulls
                .get(&indexes.full_keys[entry_index])
                .copied()
                .unwrap_or(0)
                > 1;
        } else {
            conflict = true;
        }
    }
    if conflict {
        ObservationCompatibility::Conflicting
    } else if duplicate {
        ObservationCompatibility::Duplicate
    } else if repeated {
        ObservationCompatibility::Repeated
    } else {
        ObservationCompatibility::Compatible
    }
}

fn subtype_text_port(v: Option<PortIdSubtype>) -> Option<String> {
    v.map(|v| match v {
        PortIdSubtype::InterfaceAlias => "interface_alias",
        PortIdSubtype::MacAddress => "mac_address",
        PortIdSubtype::NetworkAddress => "network_address",
        PortIdSubtype::InterfaceName => "interface_name",
        PortIdSubtype::IfIndex => "if_index",
        PortIdSubtype::LocallyAssigned => "locally_assigned",
        PortIdSubtype::Unknown => "unknown",
    })
    .map(str::to_string)
}

fn subtype_text_chassis(v: Option<ChassisIdSubtype>) -> Option<String> {
    v.map(|v| match v {
        ChassisIdSubtype::MacAddress => "mac_address",
        ChassisIdSubtype::NetworkAddress => "network_address",
        ChassisIdSubtype::InterfaceName => "interface_name",
        ChassisIdSubtype::InterfaceAlias => "interface_alias",
        ChassisIdSubtype::ChassisComponent => "chassis_component",
        ChassisIdSubtype::LocallyAssigned => "locally_assigned",
        ChassisIdSubtype::Unknown => "unknown",
    })
    .map(str::to_string)
}

/// Build capture coverage from the normalization facts themselves. This is
/// the supported connection between actual normalization output and
/// reconciliation input; it prevents independently invented device-wide
/// completeness claims. Source-established validity is propagated conservatively;
/// when no such boundary exists, coverage remains unable to establish current
/// absence rather than treating caller-invented freshness as source evidence.
pub fn capture_coverage_from_lldp_facts(facts: &[TypedFact]) -> Vec<CaptureCoverageEvidence> {
    #[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct Key {
        device: String,
        scope: Scope,
        ns: Ns,
        collector: String,
        source_kind: String,
        capture: String,
        generation: u64,
        observed: String,
    }
    let mut groups: BTreeMap<Key, CaptureCoverageEvidence> = BTreeMap::new();
    for fact in facts {
        // Coverage is derived only through the same validated consumer boundary
        // used by reconciliation. A well-formed digest is necessary but not
        // sufficient: namespace, plane, trust and subject/payload consistency
        // must also hold before coverage can be trusted.
        if validate_fact_envelope(
            fact,
            ExpectedFactContract {
                fact_kind: crate::lldp_facts::FACT_KIND_LLDP_NEIGHBOR,
                subject_namespace: "interface",
                operational_plane: "operational",
                minimum_trust: TrustClass::DeviceAsserted,
            },
        )
        .is_err()
        {
            continue;
        }
        let Some(payload) = payload_of(fact) else {
            continue;
        };
        // Coverage identity must corroborate the fact's acquisition envelope;
        // otherwise a sealed but internally inconsistent payload could merge
        // unrelated capture streams.
        if payload.coverage.capture_id.as_deref() != Some(fact.source.capture_id.as_str())
            || payload.coverage.source_generation != Some(fact.source.session_epoch)
            || payload.coverage.observation_time.as_deref() != Some(fact.observed_at.as_str())
        {
            continue;
        }
        let key = Key {
            device: fact.subject.device_id.clone(),
            scope: fact.subject.scope.clone(),
            ns: fact.subject.network_namespace.clone(),
            collector: fact.source.collector_id.clone(),
            source_kind: fact.source.source_kind.clone(),
            capture: fact.source.capture_id.clone(),
            generation: fact.source.session_epoch,
            observed: fact.observed_at.clone(),
        };
        let entry = groups
            .entry(key.clone())
            .or_insert_with(|| CaptureCoverageEvidence {
                device_id: key.device.clone(),
                scope: key.scope.clone(),
                network_namespace: key.ns.clone(),
                collector_id: Some(key.collector.clone()),
                source_kind: Some(key.source_kind.clone()),
                capture_outcome: payload.coverage.capture_outcome,
                population_coverage: payload.coverage.population_coverage,
                capture_id: Some(key.capture.clone()),
                source_generation: Some(key.generation),
                acquisition_scope: payload.coverage.acquisition_scope.clone(),
                observation_time: Some(key.observed.clone()),
                valid_until: fact.valid_until.clone(),
                covered_interfaces: BTreeSet::new(),
                evidence_refs: vec![],
            });
        // A multi-fact capture is validity-bound only until the earliest
        // source-established fact validity boundary. If any contributing fact
        // has unknown validity, coverage validity remains unknown rather than
        // being invented by this derivation.
        entry.valid_until = match (entry.valid_until.clone(), fact.valid_until.clone()) {
            (Some(left), Some(right)) => {
                let left_time = parse_rfc3339(&left);
                let right_time = parse_rfc3339(&right);
                match (left_time, right_time) {
                    (Some(left_time), Some(right_time)) if right_time < left_time => Some(right),
                    (Some(_), Some(_)) => Some(left),
                    _ => None,
                }
            }
            _ => None,
        };
        if payload.coverage.population_coverage != PopulationCoverage::Complete {
            entry.population_coverage = payload.coverage.population_coverage;
        }
        if payload.coverage.capture_outcome != LldpCaptureOutcome::Complete {
            entry.capture_outcome = payload.coverage.capture_outcome;
        }
        entry
            .covered_interfaces
            .insert(payload.local_interface_name.clone());
        for interface in &payload.coverage.covered_interfaces {
            entry.covered_interfaces.insert(interface.clone());
        }
        for interface in &payload.coverage.empty_local_interface_names {
            entry.covered_interfaces.insert(interface.clone());
        }
        for evidence in fact
            .evidence
            .iter()
            .chain(payload.coverage.evidence_references.iter())
        {
            if !entry.evidence_refs.contains(evidence) {
                entry.evidence_refs.push(evidence.clone());
            }
        }
    }
    groups.into_values().collect()
}

/// Reconcile typed LLDP observations against independently established
/// device/interface identity at one explicit analysis time.
pub fn reconcile_lldp_adjacency(
    input: &AdjacencyReconciliationInput,
) -> AdjacencyReconciliationOutput {
    let limits = input.limits.unwrap_or_default();
    let mut errors = Vec::new();
    let empty_report = |errors, parsed| AdjacencyReconciliationOutput {
        assessments: vec![],
        report: AdjacencyReconciliationReport {
            input_lldp_facts: input.lldp_facts.len(),
            parsed_lldp_facts: parsed,
            assessments: 0,
            input_errors: errors,
            status_counts: BTreeMap::new(),
            input_facts_omitted: 0,
            interface_facts_omitted: 0,
            candidate_evaluations_omitted: 0,
            output_assessments_omitted: 0,
        },
    };
    let Some(as_of) = parse_rfc3339(input.as_of) else {
        return empty_report(
            vec!["as_of is not valid RFC-3339; analysis refused".into()],
            0,
        );
    };
    let (interfaces, interface_errors, interface_omitted) =
        interface_entries(input.interface_facts, as_of, limits);
    errors.extend(interface_errors);
    let interface_indexes = build_interface_indexes(&interfaces);
    let identity_indexes = build_identity_indexes(input.device_identities);

    let mut entries = Vec::new();
    for fact in input.lldp_facts.iter().take(limits.max_input_facts) {
        if fact.fact_kind != crate::lldp_facts::FACT_KIND_LLDP_NEIGHBOR {
            continue;
        }
        if fact.subject.namespace != "interface"
            || fact.subject.plane.as_deref() != Some("operational")
        {
            bounded_error(
                &mut errors,
                limits,
                format!(
                    "{}: LLDP fact has wrong namespace/plane; skipped",
                    fact.fact_id
                ),
            );
            continue;
        }
        if let Err(e) = fact.validate() {
            bounded_error(
                &mut errors,
                limits,
                format!(
                    "{}: LLDP fact failed typed validation ({e}); skipped",
                    fact.fact_id
                ),
            );
            continue;
        }
        match payload_of(fact) {
            Some(payload) => entries.push(LldpEntry {
                fact: fact.clone(),
                payload,
            }),
            None => bounded_error(
                &mut errors,
                limits,
                format!(
                    "{}: LLDP typed fields absent or malformed; skipped",
                    fact.fact_id
                ),
            ),
        }
    }
    let input_omitted = input
        .lldp_facts
        .len()
        .saturating_sub(limits.max_input_facts);

    let local_resolutions: Vec<_> = entries
        .iter()
        .map(|entry| resolve_local_interface(entry, &interfaces, &interface_indexes))
        .collect();
    let local_keys: Vec<_> = entries
        .iter()
        .zip(&local_resolutions)
        .map(|(entry, resolution)| canonical_local_key(entry, resolution))
        .collect();

    let mut interface_by_fact_id: BTreeMap<String, usize> = BTreeMap::new();
    for (i, entry) in interfaces.iter().enumerate() {
        for fact_id in &entry.fact_ids {
            interface_by_fact_id.insert(fact_id.clone(), i);
        }
    }
    let mut identity_by_device_scope: BTreeMap<(String, Scope), usize> = BTreeMap::new();
    for (i, id) in input.device_identities.iter().enumerate() {
        identity_by_device_scope
            .entry((id.device_id.clone(), id.scope.clone()))
            .or_insert(i);
    }
    let mut coverage_index: BTreeMap<(String, Scope), Vec<usize>> = BTreeMap::new();
    for (i, cov) in input.capture_coverage.iter().enumerate() {
        coverage_index
            .entry((cov.device_id.clone(), cov.scope.clone()))
            .or_default()
            .push(i);
    }
    let observation_indexes = build_observation_indexes(&entries, &local_keys);

    let mut reverse_index: BTreeMap<(String, Scope, String), Vec<usize>> = BTreeMap::new();
    for (i, entry) in entries.iter().enumerate() {
        reverse_index
            .entry((
                entry.fact.subject.device_id.clone(),
                entry.fact.subject.scope.clone(),
                local_keys[i].clone(),
            ))
            .or_default()
            .push(i);
    }

    let mut assessments = Vec::new();
    let mut candidate_omitted = 0usize;
    for (i, entry) in entries.iter().enumerate() {
        if assessments.len() >= limits.max_output_assessments {
            break;
        }
        let remote_device =
            resolve_remote_device(entry, input.device_identities, &identity_indexes);
        let remote_interface =
            resolve_remote_interface(entry, &remote_device, &interfaces, &interface_indexes);
        let (reverse, agreement, reverse_ids, coverage_refs, omitted) = reverse_status(
            i,
            &entries,
            &local_resolutions,
            &reverse_index,
            &observation_indexes,
            &interfaces,
            &remote_device,
            &remote_interface,
            input.device_identities,
            &identity_by_device_scope,
            &interface_by_fact_id,
            input.capture_coverage,
            &coverage_index,
            as_of,
            input.coverage_freshness,
            limits,
        );
        candidate_omitted += omitted;
        let mut prerequisites = Vec::new();
        if local_resolutions[i].status != ResolutionStatus::Resolved {
            prerequisites.push("local interface identity unresolved".into());
        }
        if remote_device.status != ResolutionStatus::Resolved {
            prerequisites.push("remote device canonical identity unresolved".into());
        }
        if remote_interface.status != ResolutionStatus::Resolved {
            prerequisites.push("remote interface identity unresolved".into());
        }
        if matches!(
            reverse,
            ReverseEvidenceStatus::CoverageUnknown | ReverseEvidenceStatus::NotResolvable
        ) {
            prerequisites.push("reverse adjacency coverage unresolved".into());
        }
        if agreement != IdentifierAgreement::Agree {
            prerequisites.push("reverse identifier agreement not established".into());
        }
        let fresh = freshness(&entry.fact, as_of);
        let compat = compatibility(i, &observation_indexes);
        let mut evidence_fact_ids = BTreeSet::from([entry.fact.fact_id.clone()]);
        evidence_fact_ids.extend(reverse_ids);
        let mut supporting_interface_fact_ids = BTreeSet::new();
        supporting_interface_fact_ids.extend(
            local_resolutions[i]
                .candidates
                .iter()
                .flat_map(|c| c.evidence_fact_ids.iter().cloned()),
        );
        supporting_interface_fact_ids.extend(
            remote_interface
                .candidates
                .iter()
                .flat_map(|c| c.evidence_fact_ids.iter().cloned()),
        );
        evidence_fact_ids.extend(supporting_interface_fact_ids.iter().cloned());
        let identity_refs = remote_device
            .candidates
            .first()
            .and_then(|c| {
                identity_by_device_scope
                    .get(&(c.device_id.clone(), entry.fact.subject.scope.clone()))
                    .map(|i| &input.device_identities[*i])
            })
            .map(|id| id.evidence_refs.clone())
            .unwrap_or_default();
        assessments.push(AdjacencyAssessment {
            fact_id: entry.fact.fact_id.clone(),
            local_device_id: entry.fact.subject.device_id.clone(),
            scope: entry.fact.subject.scope.clone(),
            network_namespace: entry.fact.subject.network_namespace.clone(),
            local_interface: entry.payload.local_interface_name.clone(),
            local_interface_resolution: local_resolutions[i].clone(),
            remote_chassis_id: entry.payload.remote_chassis_id.value.clone(),
            remote_chassis_id_subtype: subtype_text_chassis(
                entry.payload.remote_chassis_id_subtype.value,
            ),
            remote_port_id: entry.payload.remote_port_id.value.clone(),
            remote_port_id_subtype: subtype_text_port(entry.payload.remote_port_id_subtype.value),
            remote_system_name: entry.payload.remote_system_name.value.clone(),
            remote_device_resolution: remote_device,
            remote_interface_resolution: remote_interface,
            reverse_evidence: reverse,
            identifier_agreement: agreement,
            freshness: fresh,
            observation_compatibility: compat,
            unresolved_prerequisites: prerequisites,
            evidence_fact_ids: evidence_fact_ids.into_iter().collect(),
            supporting_interface_fact_ids: supporting_interface_fact_ids.into_iter().collect(),
            identity_evidence_refs: identity_refs,
            coverage_evidence_refs: coverage_refs,
        });
    }
    let output_omitted = entries.len().saturating_sub(assessments.len());
    assessments.sort_by(|a, b| {
        (
            &a.local_device_id,
            &a.local_interface,
            &a.remote_chassis_id,
            &a.remote_port_id,
            &a.fact_id,
        )
            .cmp(&(
                &b.local_device_id,
                &b.local_interface,
                &b.remote_chassis_id,
                &b.remote_port_id,
                &b.fact_id,
            ))
    });
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for a in &assessments {
        *counts
            .entry(format!("reverse:{:?}", a.reverse_evidence).to_lowercase())
            .or_default() += 1;
        *counts
            .entry(format!("freshness:{:?}", a.freshness).to_lowercase())
            .or_default() += 1;
        *counts
            .entry(format!("device:{:?}", a.remote_device_resolution.status).to_lowercase())
            .or_default() += 1;
    }
    AdjacencyReconciliationOutput {
        report: AdjacencyReconciliationReport {
            input_lldp_facts: input.lldp_facts.len(),
            parsed_lldp_facts: entries.len(),
            assessments: assessments.len(),
            input_errors: errors,
            status_counts: counts,
            input_facts_omitted: input_omitted,
            interface_facts_omitted: interface_omitted,
            candidate_evaluations_omitted: candidate_omitted,
            output_assessments_omitted: output_omitted,
        },
        assessments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_facts::{AdminState, InterfaceType, OperState};
    use crate::interface_facts::{FieldAvailability, ObservedField};
    use crate::lldp_facts::{LldpCoverage, NeighborProtocol, SubtypeObservation};
    use crate::typed_facts::{
        Completeness, EvidenceRef, FactPayload, FactSource, FactSubject, InterfaceIdentityKind,
        InterfaceSubjectIdentity, Quality, TrustClass,
    };

    const AS_OF: &str = "2026-01-01T00:01:00Z";
    const OBSERVED: &str = "2026-01-01T00:00:00Z";

    fn unknown<T>() -> ObservedField<T> {
        ObservedField::unavailable(FieldAvailability::Unknown, None)
    }

    fn base_fact(
        kind: &str,
        device: &str,
        entity: Option<&str>,
        ns: Option<&str>,
        seq: u64,
        fields: Option<serde_json::Value>,
    ) -> TypedFact {
        let mut fact = TypedFact {
            schema_version: crate::typed_facts::SCHEMA_VERSION,
            fact_id: String::new(),
            fact_kind: kind.to_string(),
            subject: FactSubject {
                device_id: device.to_string(),
                scope: ns.map(str::to_string),
                namespace: "interface".into(),
                network_namespace: ns.map(str::to_string),
                entity: entity.map(str::to_string),
                vrf: None,
                afi_safi: None,
                plane: Some("operational".into()),
                interface_identity: None,
            },
            source: FactSource {
                collector_id: "test-collector".into(),
                source_kind: "test".into(),
                capture_id: format!("capture-{seq}"),
                session_epoch: 1,
                sequence: seq,
                exporter_id: None,
            },
            observed_at: OBSERVED.into(),
            ingested_at: None,
            valid_from: Some(OBSERVED.into()),
            valid_until: Some("2026-01-01T00:02:00Z".into()),
            payload: FactPayload {
                produced_by: "test".into(),
                parser_revision: "test-1".into(),
                mapping_revision: None,
                fields,
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
    fn neighbor_fact(
        device: &str,
        iface: &str,
        chassis: &str,
        port: &str,
        system: &str,
        subtype: PortIdSubtype,
        ns: Option<&str>,
        seq: u64,
    ) -> TypedFact {
        let payload = LldpNeighborPayload {
            local_interface_name: iface.into(),
            normalized_local_interface_name: None,
            source_neighbor_id: Some(format!("n{seq}")),
            capture_record_locator: None,
            protocol: NeighborProtocol::Lldp,
            remote_chassis_id: ObservedField::known(chassis.to_string()),
            remote_chassis_id_raw: None,
            remote_chassis_id_subtype: SubtypeObservation::known(
                ChassisIdSubtype::MacAddress,
                Some("macAddress".into()),
            ),
            remote_port_id: ObservedField::known(port.to_string()),
            remote_port_id_raw: None,
            remote_port_id_subtype: SubtypeObservation::known(subtype, Some("test".into())),
            remote_system_name: ObservedField::known(system.to_string()),
            remote_port_description: unknown(),
            remote_management_addresses: unknown(),
            capabilities: unknown(),
            ttl_seconds: ObservedField::known(120),
            age_seconds: unknown(),
            temporal: None,
            coverage: LldpCoverage {
                capture_outcome: LldpCaptureOutcome::Complete,
                population_coverage: PopulationCoverage::Complete,
                empty_local_interfaces: 0,
                empty_local_interface_names: vec![],
                capture_id: Some(format!("capture-{seq}")),
                source_generation: Some(1),
                observation_time: Some(OBSERVED.into()),
                acquisition_scope: None,
                covered_interfaces: vec![iface.into()],
                evidence_references: vec![],
            },
        };
        base_fact(
            crate::lldp_facts::FACT_KIND_LLDP_NEIGHBOR,
            device,
            Some(iface),
            ns,
            seq,
            Some(serde_json::to_value(payload).unwrap()),
        )
    }

    fn interface_fact(
        device: &str,
        iface: &str,
        mac: &str,
        ifindex: u64,
        ns: Option<&str>,
        seq: u64,
    ) -> TypedFact {
        let payload = InterfaceStatePayload {
            interface_name: iface.into(),
            normalized_name: None,
            administrative_state: ObservedField::known(AdminState::Up),
            operational_state: ObservedField::known(OperState::Up),
            ifindex: ObservedField::known(ifindex),
            interface_type: ObservedField::known(InterfaceType::Physical),
            mtu: unknown(),
            speed: unknown(),
            mac_address: ObservedField::known(mac.to_string()),
            description: None,
            parent_interface: None,
            member_of: None,
            lag_members: None,
            parent_relationship: None,
            master_relationship: None,
            source_conflicts: vec![],
            ip_addresses: None,
            ipv6_addresses: None,
        };
        base_fact(
            crate::interface_facts::FACT_KIND_INTERFACE_STATE,
            device,
            Some(iface),
            ns,
            seq,
            Some(serde_json::to_value(payload).unwrap()),
        )
    }

    fn identity(
        device: &str,
        chassis: &str,
        host: &str,
        ns: Option<&str>,
    ) -> DeviceIdentityEvidence {
        DeviceIdentityEvidence {
            device_id: device.into(),
            scope: ns.map(str::to_string),
            network_namespace: ns.map(str::to_string),
            chassis_ids: vec![chassis.into()],
            typed_chassis_ids: vec![],
            hostnames: vec![host.into()],
            management_addresses: vec![],
            evidence_refs: vec![],
        }
    }

    fn coverage(
        device: &str,
        ns: Option<&str>,
        population: PopulationCoverage,
    ) -> CaptureCoverageEvidence {
        CaptureCoverageEvidence {
            device_id: device.into(),
            scope: ns.map(str::to_string),
            network_namespace: ns.map(str::to_string),
            collector_id: Some("collector-test".into()),
            source_kind: Some("test".into()),
            capture_outcome: LldpCaptureOutcome::Complete,
            population_coverage: population,
            capture_id: Some("test-capture".into()),
            source_generation: Some(1),
            acquisition_scope: None,
            observation_time: Some("2026-01-01T00:00:00+00:00".into()),
            valid_until: Some("2026-01-01T01:00:00+00:00".into()),
            covered_interfaces: std::collections::BTreeSet::from(["Ethernet1".to_string()]),
            evidence_refs: vec![],
        }
    }

    fn input<'a>(
        lldp: &'a [TypedFact],
        interfaces: &'a [TypedFact],
        identities: &'a [DeviceIdentityEvidence],
        coverage: &'a [CaptureCoverageEvidence],
    ) -> AdjacencyReconciliationInput<'a> {
        AdjacencyReconciliationInput {
            lldp_facts: lldp,
            interface_facts: interfaces,
            device_identities: identities,
            capture_coverage: coverage,
            as_of: AS_OF,
            coverage_freshness: FreshnessPolicy::default(),
            limits: None,
        }
    }

    fn two_device_fixture(
        ns: Option<&str>,
    ) -> (
        Vec<TypedFact>,
        Vec<TypedFact>,
        Vec<DeviceIdentityEvidence>,
        Vec<CaptureCoverageEvidence>,
    ) {
        let lldp = vec![
            neighbor_fact(
                "leaf1",
                "Ethernet1",
                "bb:bb:bb:bb:bb:02",
                "Ethernet1",
                "leaf2",
                PortIdSubtype::InterfaceName,
                ns,
                1,
            ),
            neighbor_fact(
                "leaf2",
                "Ethernet1",
                "bb:bb:bb:bb:bb:01",
                "Ethernet1",
                "leaf1",
                PortIdSubtype::InterfaceName,
                ns,
                2,
            ),
        ];
        let interfaces = vec![
            interface_fact("leaf1", "Ethernet1", "aa:aa:aa:aa:aa:01", 11, ns, 11),
            interface_fact("leaf2", "Ethernet1", "aa:aa:aa:aa:aa:02", 22, ns, 12),
        ];
        let identities = vec![
            identity("leaf1", "bb:bb:bb:bb:bb:01", "leaf1", ns),
            identity("leaf2", "bb:bb:bb:bb:bb:02", "leaf2", ns),
        ];
        let cov = vec![
            coverage("leaf1", ns, PopulationCoverage::Complete),
            coverage("leaf2", ns, PopulationCoverage::Complete),
        ];
        (lldp, interfaces, identities, cov)
    }

    #[test]
    fn two_sided_consistent_adjacency_is_confirmed_not_forwarding_proof() {
        let (lldp, interfaces, identities, cov) = two_device_fixture(Some("default"));
        let out = reconcile_lldp_adjacency(&input(&lldp, &interfaces, &identities, &cov));
        assert_eq!(out.assessments.len(), 2);
        assert!(out.assessments.iter().all(|a| {
            a.remote_device_resolution.status == ResolutionStatus::Resolved
                && a.remote_interface_resolution.status == ResolutionStatus::Resolved
                && a.reverse_evidence == ReverseEvidenceStatus::Confirmed
                && a.identifier_agreement == IdentifierAgreement::Agree
                && a.freshness == FreshnessStatus::Current
        }));
    }

    #[test]
    fn one_sided_adjacency_distinguishes_confirmed_missing_from_unknown_coverage() {
        let (lldp, interfaces, identities, _cov) = two_device_fixture(Some("default"));
        let one = vec![lldp[0].clone()];
        let unknown = reconcile_lldp_adjacency(&input(&one, &interfaces, &identities, &[]));
        assert_eq!(
            unknown.assessments[0].reverse_evidence,
            ReverseEvidenceStatus::CoverageUnknown
        );
        assert!(
            unknown.assessments[0]
                .unresolved_prerequisites
                .iter()
                .any(|p| p.contains("reverse adjacency coverage"))
        );
        let complete = vec![coverage(
            "leaf2",
            Some("default"),
            PopulationCoverage::Complete,
        )];
        let missing = reconcile_lldp_adjacency(&input(&one, &interfaces, &identities, &complete));
        assert_eq!(
            missing.assessments[0].reverse_evidence,
            ReverseEvidenceStatus::MissingReverse
        );
        let partial = vec![coverage(
            "leaf2",
            Some("default"),
            PopulationCoverage::Partial,
        )];
        let part = reconcile_lldp_adjacency(&input(&one, &interfaces, &identities, &partial));
        assert_eq!(
            part.assessments[0].reverse_evidence,
            ReverseEvidenceStatus::CoverageUnknown
        );
    }

    #[test]
    fn parallel_links_multiple_neighbors_mac_and_ifindex_remain_distinct() {
        let identities = vec![
            identity("leaf1", "bb:bb:bb:bb:bb:01", "leaf1", Some("default")),
            identity("leaf2", "bb:bb:bb:bb:bb:02", "leaf2", Some("default")),
        ];
        let interfaces = vec![
            interface_fact(
                "leaf2",
                "Ethernet5",
                "aa:aa:aa:aa:02:05",
                25,
                Some("default"),
                25,
            ),
            interface_fact(
                "leaf2",
                "Ethernet6",
                "aa:aa:aa:aa:02:06",
                26,
                Some("default"),
                26,
            ),
        ];
        let lldp = vec![
            neighbor_fact(
                "leaf1",
                "Ethernet5",
                "bb:bb:bb:bb:bb:02",
                "Ethernet5",
                "leaf2",
                PortIdSubtype::InterfaceName,
                Some("default"),
                5,
            ),
            neighbor_fact(
                "leaf1",
                "Ethernet6",
                "bb:bb:bb:bb:bb:02",
                "aa:aa:aa:aa:02:06",
                "leaf2",
                PortIdSubtype::MacAddress,
                Some("default"),
                6,
            ),
            neighbor_fact(
                "leaf1",
                "Ethernet7",
                "bb:bb:bb:bb:bb:02",
                "25",
                "leaf2",
                PortIdSubtype::IfIndex,
                Some("default"),
                7,
            ),
            neighbor_fact(
                "leaf1",
                "Ethernet8",
                "cc:cc:cc:cc:cc:01",
                "x1",
                "other1",
                PortIdSubtype::InterfaceName,
                Some("default"),
                8,
            ),
            neighbor_fact(
                "leaf1",
                "Ethernet8",
                "cc:cc:cc:cc:cc:02",
                "x2",
                "other2",
                PortIdSubtype::InterfaceName,
                Some("default"),
                9,
            ),
        ];
        let out = reconcile_lldp_adjacency(&input(&lldp, &interfaces, &identities, &[]));
        let by_local: BTreeMap<_, Vec<_>> = out
            .assessments
            .iter()
            .map(|a| {
                (
                    a.local_interface.as_str(),
                    a.remote_interface_resolution.status,
                )
            })
            .fold(BTreeMap::new(), |mut m, x| {
                m.entry(x.0).or_default().push(x.1);
                m
            });
        assert_eq!(by_local["Ethernet5"], vec![ResolutionStatus::Resolved]);
        assert_eq!(by_local["Ethernet6"], vec![ResolutionStatus::Resolved]);
        assert_eq!(by_local["Ethernet7"], vec![ResolutionStatus::Resolved]);
        // Two unresolved neighbors on one interface are separate valid observations.
        assert_eq!(
            out.assessments
                .iter()
                .filter(|a| a.local_interface == "Ethernet8")
                .count(),
            2
        );
        assert!(
            out.assessments
                .iter()
                .filter(|a| a.local_interface == "Ethernet8")
                .all(|a| a.observation_compatibility == ObservationCompatibility::Compatible)
        );
    }

    #[test]
    fn hostname_collision_is_not_authority_and_namespaces_do_not_merge() {
        let identities = vec![
            identity("leaf2a", "bb:bb:bb:bb:bb:2a", "leaf2", Some("blue")),
            identity("leaf2b", "bb:bb:bb:bb:bb:2b", "leaf2", Some("blue")),
        ];
        let ambiguous = neighbor_fact(
            "leaf1",
            "Ethernet1",
            "cc:cc:cc:cc:cc:99",
            "Ethernet1",
            "leaf2",
            PortIdSubtype::InterfaceName,
            Some("blue"),
            20,
        );
        let out = reconcile_lldp_adjacency(&input(&[ambiguous], &[], &identities, &[]));
        assert_eq!(
            out.assessments[0].remote_device_resolution.status,
            ResolutionStatus::Ambiguous
        );

        let red_identity = vec![identity("red2", "bb:bb:bb:bb:bb:02", "leaf2", Some("red"))];
        let blue_fact = neighbor_fact(
            "leaf1",
            "Ethernet1",
            "bb:bb:bb:bb:bb:02",
            "Ethernet1",
            "leaf2",
            PortIdSubtype::InterfaceName,
            Some("blue"),
            21,
        );
        let out = reconcile_lldp_adjacency(&input(&[blue_fact], &[], &red_identity, &[]));
        assert_eq!(
            out.assessments[0].remote_device_resolution.status,
            ResolutionStatus::Unresolved
        );
    }

    #[test]
    fn conflicting_duplicate_stale_and_withdrawn_evidence_are_explicit() {
        let (lldp, interfaces, identities, cov) = two_device_fixture(Some("default"));
        let mut conflicting_reverse = lldp[1].clone();
        conflicting_reverse.payload.fields.as_mut().unwrap()["remote_system_name"] =
            serde_json::json!({"state":"known","value":"not-leaf1"});
        conflicting_reverse.seal();
        let facts = vec![lldp[0].clone(), conflicting_reverse];
        let out = reconcile_lldp_adjacency(&input(&facts, &interfaces, &identities, &cov));
        assert_eq!(
            out.assessments[0].reverse_evidence,
            ReverseEvidenceStatus::Conflicting
        );

        let mut stale_reverse = lldp[1].clone();
        stale_reverse.valid_until = Some("2026-01-01T00:00:30Z".into());
        stale_reverse.seal();
        let facts = vec![lldp[0].clone(), stale_reverse];
        let out = reconcile_lldp_adjacency(&input(&facts, &interfaces, &identities, &cov));
        assert_eq!(
            out.assessments[0].reverse_evidence,
            ReverseEvidenceStatus::Stale
        );

        let mut withdrawn_reverse = lldp[1].clone();
        withdrawn_reverse.lifecycle = Lifecycle::Withdrawn;
        withdrawn_reverse.withdrawn_reason = Some("explicit neighbor withdrawal".into());
        withdrawn_reverse.lifecycle_changed_at = Some(OBSERVED.into());
        withdrawn_reverse.seal();
        let facts = vec![lldp[0].clone(), withdrawn_reverse];
        let out = reconcile_lldp_adjacency(&input(&facts, &interfaces, &identities, &cov));
        assert_eq!(
            out.assessments[0].reverse_evidence,
            ReverseEvidenceStatus::Withdrawn
        );

        let mut a = lldp[0].clone();
        let mut b = lldp[0].clone();
        b.source.sequence = 99;
        b.seal();
        a.source.sequence = 98;
        a.seal();
        let dup = reconcile_lldp_adjacency(&input(&[a, b], &interfaces, &identities, &cov));
        assert!(
            dup.assessments
                .iter()
                .any(|x| x.observation_compatibility == ObservationCompatibility::Duplicate)
        );
    }

    #[test]
    fn malformed_payload_is_input_error_and_structured_subinterface_is_not_string_inferred() {
        let malformed = base_fact(
            crate::lldp_facts::FACT_KIND_LLDP_NEIGHBOR,
            "leaf1",
            Some("Ethernet1"),
            Some("default"),
            50,
            None,
        );
        let out = reconcile_lldp_adjacency(&input(&[malformed], &[], &[], &[]));
        assert_eq!(out.assessments.len(), 0);
        assert!(
            out.report
                .input_errors
                .iter()
                .any(|e| e.contains("malformed") || e.contains("failed typed validation"))
        );

        let mut sub = neighbor_fact(
            "leaf1",
            "Ethernet1.100",
            "bb:bb:bb:bb:bb:02",
            "Ethernet1",
            "leaf2",
            PortIdSubtype::InterfaceName,
            Some("default"),
            51,
        );
        sub.subject.entity = Some("Ethernet1".into());
        sub.subject.interface_identity = Some(InterfaceSubjectIdentity {
            kind: InterfaceIdentityKind::Subinterface,
            parent_entity: Some("Ethernet1".into()),
            subinterface_index: Some(100),
            vendor_native_name: Some("Ethernet1.100".into()),
        });
        sub.payload.fields.as_mut().unwrap()["normalized_local_interface_name"] =
            serde_json::json!("Ethernet1");
        sub.seal();
        assert!(
            sub.validate().is_ok(),
            "structured subinterface should satisfy typed consistency without dotted-name inference"
        );
    }
    #[test]
    fn invalid_typed_evidence_cannot_establish_resolution() {
        let (lldp, interfaces, identities, cov) = two_device_fixture(Some("default"));
        let mut bad_interface = interfaces[1].clone();
        bad_interface.payload.fields.as_mut().unwrap()["mac_address"] =
            serde_json::json!({"state":"known","value":"aa:aa:aa:aa:02:06"});
        // Deliberately do not reseal: digest/fact identity must be rejected.
        let facts = vec![lldp[0].clone()];
        let out = reconcile_lldp_adjacency(&input(&facts, &[bad_interface], &identities, &cov));
        assert_eq!(
            out.assessments[0].remote_interface_resolution.status,
            ResolutionStatus::Unresolved
        );
        assert!(
            out.report
                .input_errors
                .iter()
                .any(|e| e.contains("failed typed validation"))
        );
    }

    #[test]
    fn matching_chassis_with_opaque_reverse_port_is_candidate_not_confirmed() {
        let (lldp, interfaces, identities, cov) = two_device_fixture(Some("default"));
        let mut reverse = lldp[1].clone();
        reverse.payload.fields.as_mut().unwrap()["remote_port_id_subtype"] =
            serde_json::json!({"state":"known","value":"locally_assigned","raw":"local"});
        reverse.payload.fields.as_mut().unwrap()["remote_port_id"] =
            serde_json::json!({"state":"known","value":"opaque-7"});
        reverse.seal();
        let facts = vec![lldp[0].clone(), reverse];
        let out = reconcile_lldp_adjacency(&input(&facts, &interfaces, &identities, &cov));
        let forward = out
            .assessments
            .iter()
            .find(|a| a.local_device_id == "leaf1")
            .unwrap();
        assert_eq!(forward.reverse_evidence, ReverseEvidenceStatus::Candidate);
        assert_ne!(forward.identifier_agreement, IdentifierAgreement::Agree);
    }

    #[test]
    fn permutation_of_lldp_interface_identity_and_coverage_inputs_is_independent() {
        let (lldp, interfaces, identities, cov) = two_device_fixture(Some("default"));
        let forward = reconcile_lldp_adjacency(&input(&lldp, &interfaces, &identities, &cov));
        let mut lldp_rev = lldp.clone();
        lldp_rev.reverse();
        let mut interfaces_rev = interfaces.clone();
        interfaces_rev.reverse();
        let mut identities_rev = identities.clone();
        identities_rev.reverse();
        let mut cov_rev = cov.clone();
        cov_rev.reverse();
        let reverse = reconcile_lldp_adjacency(&input(
            &lldp_rev,
            &interfaces_rev,
            &identities_rev,
            &cov_rev,
        ));
        assert_eq!(
            serde_json::to_value(&forward.assessments).unwrap(),
            serde_json::to_value(&reverse.assessments).unwrap()
        );
    }

    #[test]
    fn same_chassis_value_with_different_subtype_is_not_identity_evidence() {
        let mut id = identity("leaf2", "bb:bb:bb:bb:bb:02", "leaf2", Some("default"));
        id.chassis_ids.clear();
        id.typed_chassis_ids = vec![ChassisIdentityEvidence {
            subtype: ChassisIdSubtype::ChassisComponent,
            value: "bb:bb:bb:bb:bb:02".into(),
        }];
        let fact = neighbor_fact(
            "leaf1",
            "Ethernet1",
            "bb:bb:bb:bb:bb:02",
            "Ethernet1",
            "leaf2",
            PortIdSubtype::InterfaceName,
            Some("default"),
            70,
        );
        let out = reconcile_lldp_adjacency(&input(&[fact], &[], &[id], &[]));
        assert_eq!(
            out.assessments[0].remote_device_resolution.status,
            ResolutionStatus::Candidate
        );
        assert!(
            out.assessments[0]
                .remote_device_resolution
                .reason
                .contains("corroborating only")
        );
    }

    #[test]
    fn conflicting_reverse_wins_over_agreeing_candidate_regardless_of_order() {
        let (lldp, interfaces, identities, cov) = two_device_fixture(Some("default"));
        let mut conflicting = lldp[1].clone();
        conflicting.payload.fields.as_mut().unwrap()["remote_system_name"] =
            serde_json::json!({"state":"known","value":"not-leaf1"});
        conflicting.source.sequence = 88;
        conflicting.seal();
        for facts in [
            vec![lldp[0].clone(), lldp[1].clone(), conflicting.clone()],
            vec![lldp[0].clone(), conflicting.clone(), lldp[1].clone()],
        ] {
            let out = reconcile_lldp_adjacency(&input(&facts, &interfaces, &identities, &cov));
            let forward = out
                .assessments
                .iter()
                .find(|a| a.local_device_id == "leaf1")
                .unwrap();
            assert_eq!(forward.reverse_evidence, ReverseEvidenceStatus::Conflicting);
        }
    }

    #[test]
    fn coverage_from_wrong_network_namespace_cannot_establish_absence() {
        let (lldp, interfaces, identities, mut cov) = two_device_fixture(Some("default"));
        cov[1].network_namespace = Some("other-asic".into());
        let one = vec![lldp[0].clone()];
        let out = reconcile_lldp_adjacency(&input(&one, &interfaces, &identities, &cov));
        assert_eq!(
            out.assessments[0].reverse_evidence,
            ReverseEvidenceStatus::CoverageUnknown,
            "coverage from another device-local namespace must not prove this interface empty"
        );
    }

    #[test]
    fn conflicting_same_generation_coverage_cannot_establish_absence() {
        let (lldp, interfaces, identities, _cov) = two_device_fixture(Some("default"));
        let one = vec![lldp[0].clone()];
        let mut a = coverage("leaf2", Some("default"), PopulationCoverage::Complete);
        let mut b = a.clone();
        a.capture_id = Some("capture-a".into());
        b.capture_id = Some("capture-b".into());
        let out = reconcile_lldp_adjacency(&input(&one, &interfaces, &identities, &[a, b]));
        assert_eq!(
            out.assessments[0].reverse_evidence,
            ReverseEvidenceStatus::CoverageUnknown
        );
    }

    #[test]
    fn reconciliation_results_carry_interface_identity_and_supporting_evidence() {
        let (lldp, mut interfaces, identities, cov) = two_device_fixture(Some("default"));
        interfaces[1].subject.interface_identity = Some(InterfaceSubjectIdentity {
            kind: InterfaceIdentityKind::Physical,
            parent_entity: None,
            subinterface_index: None,
            vendor_native_name: Some("Ethernet1".into()),
        });
        interfaces[1].seal();
        let out = reconcile_lldp_adjacency(&input(&lldp, &interfaces, &identities, &cov));
        let forward = out
            .assessments
            .iter()
            .find(|a| a.local_device_id == "leaf1")
            .unwrap();
        assert_eq!(
            forward.remote_interface_resolution.candidates[0]
                .interface_identity
                .as_ref()
                .map(|i| i.kind.clone()),
            Some(InterfaceIdentityKind::Physical)
        );
        assert!(!forward.supporting_interface_fact_ids.is_empty());
        assert!(forward.evidence_fact_ids.len() >= 3);
    }

    #[test]
    fn replayed_consistent_interface_evidence_dedupes_and_conflicts_remain_ambiguous() {
        let (lldp, interfaces, identities, cov) = two_device_fixture(Some("default"));
        let mut replay = interfaces[0].clone();
        replay.source.sequence = 13;
        replay.seal();
        assert_ne!(replay.fact_id, interfaces[0].fact_id);
        let with_replay = vec![interfaces[0].clone(), replay, interfaces[1].clone()];
        let out = reconcile_lldp_adjacency(&input(&lldp, &with_replay, &identities, &cov));
        let forward = out
            .assessments
            .iter()
            .find(|a| a.local_device_id == "leaf1")
            .unwrap();
        assert_eq!(
            forward.local_interface_resolution.status,
            ResolutionStatus::Resolved
        );
        assert_eq!(
            forward.local_interface_resolution.candidates[0]
                .evidence_fact_ids
                .len(),
            2,
            "consistent replay evidence for one canonical endpoint must merge rather than become ambiguous"
        );

        let conflicting = interface_fact(
            "leaf1",
            "Ethernet1",
            "aa:aa:aa:aa:aa:99",
            11,
            Some("default"),
            14,
        );
        let with_conflict = vec![interfaces[0].clone(), conflicting, interfaces[1].clone()];
        let out = reconcile_lldp_adjacency(&input(&lldp, &with_conflict, &identities, &cov));
        let forward = out
            .assessments
            .iter()
            .find(|a| a.local_device_id == "leaf1")
            .unwrap();
        assert_eq!(
            forward.local_interface_resolution.status,
            ResolutionStatus::Ambiguous,
            "contradictory endpoint mappings must remain visible rather than being deduplicated"
        );
    }

    #[test]
    fn conflicting_reverse_observation_batch_cannot_confirm_reciprocal_adjacency() {
        let (lldp, interfaces, identities, cov) = two_device_fixture(Some("default"));
        let mut conflicting = lldp[1].clone();
        conflicting.payload.fields.as_mut().unwrap()["remote_port_description"] =
            serde_json::json!({"state":"known","value":"contradictory description"});
        conflicting.seal();
        let facts = vec![lldp[0].clone(), lldp[1].clone(), conflicting];
        let out = reconcile_lldp_adjacency(&input(&facts, &interfaces, &identities, &cov));
        let forward = out
            .assessments
            .iter()
            .find(|a| a.local_device_id == "leaf1")
            .unwrap();
        assert_eq!(
            forward.reverse_evidence,
            ReverseEvidenceStatus::Conflicting,
            "two incompatible current reverse observations must not collapse into confirmation"
        );
    }

    #[test]
    fn newer_incomplete_coverage_masks_older_complete_coverage_only_within_source() {
        let (lldp, interfaces, identities, _cov) = two_device_fixture(Some("default"));
        let one = vec![lldp[0].clone()];
        let mut older = coverage("leaf2", Some("default"), PopulationCoverage::Complete);
        older.capture_id = Some("capture-old".into());
        older.source_generation = Some(1);
        let mut newer = coverage("leaf2", Some("default"), PopulationCoverage::Partial);
        newer.capture_id = Some("capture-new".into());
        newer.source_generation = Some(2);
        newer.observation_time = Some("2026-01-01T00:00:30+00:00".into());
        let out = reconcile_lldp_adjacency(&input(
            &one,
            &interfaces,
            &identities,
            &[older.clone(), newer.clone()],
        ));
        assert_eq!(
            out.assessments[0].reverse_evidence,
            ReverseEvidenceStatus::CoverageUnknown,
            "an older complete capture must not mask newer incomplete evidence from the same source"
        );

        // A high generation in an unrelated collector stream does not order or
        // invalidate this collector's complete evidence.
        let mut unrelated = newer;
        unrelated.collector_id = Some("other-collector".into());
        unrelated.source_generation = Some(99);
        let out =
            reconcile_lldp_adjacency(&input(&one, &interfaces, &identities, &[older, unrelated]));
        assert_eq!(
            out.assessments[0].reverse_evidence,
            ReverseEvidenceStatus::MissingReverse
        );
    }

    #[test]
    fn coverage_without_source_validity_uses_only_explicit_consumer_age_policy() {
        let (lldp, interfaces, identities, _cov) = two_device_fixture(Some("default"));
        let one = vec![lldp[0].clone()];
        let mut cov = coverage("leaf2", Some("default"), PopulationCoverage::Complete);
        cov.valid_until = None;
        let coverage = [cov];
        let mut request = input(&one, &interfaces, &identities, &coverage);
        let out = reconcile_lldp_adjacency(&request);
        assert_eq!(
            out.assessments[0].reverse_evidence,
            ReverseEvidenceStatus::CoverageUnknown,
            "unknown source validity must not silently become current evidence"
        );
        request.coverage_freshness = FreshnessPolicy::observation_age(120);
        let out = reconcile_lldp_adjacency(&request);
        assert_eq!(
            out.assessments[0].reverse_evidence,
            ReverseEvidenceStatus::MissingReverse,
            "an explicit consumer observation-age policy may use the coverage"
        );
    }

    #[test]
    fn invalid_or_capture_inconsistent_facts_cannot_derive_trusted_coverage() {
        let (lldp, _interfaces, _identities, _cov) = two_device_fixture(Some("default"));
        let mut wrong_plane = lldp[1].clone();
        wrong_plane.subject.plane = Some("configured".into());
        wrong_plane.seal();
        assert!(capture_coverage_from_lldp_facts(&[wrong_plane]).is_empty());

        let mut mismatched = lldp[1].clone();
        if let Some(fields) = mismatched.payload.fields.as_mut() {
            fields["coverage"]["capture_id"] = serde_json::json!("different-capture");
        }
        mismatched.seal();
        assert!(
            mismatched.validate().is_ok(),
            "digest-valid but internally inconsistent fixture"
        );
        assert!(capture_coverage_from_lldp_facts(&[mismatched]).is_empty());
    }

    #[test]
    fn normalization_coverage_output_can_drive_reverse_absence_evidence() {
        let (lldp, interfaces, identities, _cov) = two_device_fixture(Some("default"));
        let coverage = capture_coverage_from_lldp_facts(&[lldp[1].clone()]);
        assert_eq!(coverage.len(), 1);
        assert_eq!(coverage[0].device_id, "leaf2");
        assert_eq!(coverage[0].capture_id.as_deref(), Some("capture-2"));
        assert!(coverage[0].covered_interfaces.contains("Ethernet1"));
        assert_eq!(coverage[0].observation_time.as_deref(), Some(OBSERVED));
        assert_eq!(
            coverage[0].valid_until.as_deref(),
            Some("2026-01-01T00:02:00Z")
        );
        let one = vec![lldp[0].clone()];
        let out = reconcile_lldp_adjacency(&input(&one, &interfaces, &identities, &coverage));
        assert_eq!(
            out.assessments[0].reverse_evidence,
            ReverseEvidenceStatus::MissingReverse
        );
    }

    #[test]
    #[ignore = "synthetic scaling measurement; run explicitly for performance evidence"]
    fn reconciliation_scaling_100_1000_10000_completes_with_indexed_bounds() {
        for n in [100usize, 1_000, 10_000] {
            let mut facts = Vec::new();
            let mut ifacts = Vec::new();
            for i in 0..n {
                let name = format!("Ethernet{i}");
                facts.push(neighbor_fact(
                    "leaf1",
                    &name,
                    "bb:bb:bb:bb:bb:02",
                    &name,
                    "leaf2",
                    PortIdSubtype::InterfaceName,
                    Some("default"),
                    (i * 2 + 1) as u64,
                ));
                facts.push(neighbor_fact(
                    "leaf2",
                    &name,
                    "bb:bb:bb:bb:bb:01",
                    &name,
                    "leaf1",
                    PortIdSubtype::InterfaceName,
                    Some("default"),
                    (i * 2 + 2) as u64,
                ));
                ifacts.push(interface_fact(
                    "leaf1",
                    &name,
                    "aa:aa:aa:aa:aa:01",
                    (i + 1) as u64,
                    Some("default"),
                    (i * 2 + 1) as u64,
                ));
                ifacts.push(interface_fact(
                    "leaf2",
                    &name,
                    "aa:aa:aa:aa:aa:02",
                    (i + 1) as u64,
                    Some("default"),
                    (i * 2 + 2) as u64,
                ));
            }
            let identities = vec![
                identity("leaf1", "bb:bb:bb:bb:bb:01", "leaf1", Some("default")),
                identity("leaf2", "bb:bb:bb:bb:bb:02", "leaf2", Some("default")),
            ];
            let start = std::time::Instant::now();
            let out = reconcile_lldp_adjacency(&input(&facts, &ifacts, &identities, &[]));
            println!(
                "lldp_reconcile_scale observations={} assessments={} elapsed_ms={}",
                facts.len(),
                out.assessments.len(),
                start.elapsed().as_millis()
            );
            assert_eq!(out.assessments.len(), facts.len());
            assert_eq!(out.report.candidate_evaluations_omitted, 0);
        }
    }
}
