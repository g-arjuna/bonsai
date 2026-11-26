//! Deterministic LLDP adjacency reconciliation (BN-024-06).
//!
//! This is a bounded pure analysis operation over typed facts plus explicitly
//! supplied identity evidence. It deliberately does not infer canonical
//! device ownership from a hostname or management address alone, does not
//! resolve port names through aliases, and does not treat one-sided LLDP as
//! proof of link failure. The analysis time is explicit (`as_of`); wall-clock
//! time is never read inside the operation.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::interface_facts::InterfaceStatePayload;
use crate::interface_normalizer::parse_rfc3339;
use crate::lldp_facts::{
    ChassisIdSubtype, LldpCaptureOutcome, LldpNeighborPayload, PopulationCoverage, PortIdSubtype,
};
use crate::typed_facts::{Lifecycle, TypedFact};

#[derive(Clone, Debug)]
pub struct DeviceIdentityEvidence {
    pub device_id: String,
    pub scope: Option<String>,
    pub network_namespace: Option<String>,
    /// Canonical chassis identifiers already established independently.
    /// MAC-form values are expected in normalized lowercase colon form.
    pub chassis_ids: Vec<String>,
    /// Canonical hostnames already established independently. A hostname is
    /// corroborating evidence only; it never establishes device ownership by itself.
    pub hostnames: Vec<String>,
    /// Canonical management addresses already established independently.
    /// Like hostnames these are candidates/corroboration, not sole authority.
    pub management_addresses: Vec<String>,
}

/// Explicit acquisition-population evidence for a device/namespace. This is
/// the structured replacement for scraping diagnostic strings when deciding
/// whether absence of reverse evidence is meaningful.
#[derive(Clone, Debug)]
pub struct CaptureCoverageEvidence {
    pub device_id: String,
    pub scope: Option<String>,
    pub network_namespace: Option<String>,
    pub capture_outcome: LldpCaptureOutcome,
    pub population_coverage: PopulationCoverage,
}

pub struct AdjacencyReconciliationInput<'a> {
    pub lldp_facts: &'a [TypedFact],
    pub interface_facts: &'a [TypedFact],
    pub device_identities: &'a [DeviceIdentityEvidence],
    pub capture_coverage: &'a [CaptureCoverageEvidence],
    /// Explicit analysis time (RFC-3339). Required for all freshness results.
    pub as_of: &'a str,
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

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct EndpointCandidate {
    pub device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
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
}

#[derive(Clone, Debug, Serialize)]
pub struct AdjacencyReconciliationReport {
    pub input_lldp_facts: usize,
    pub parsed_lldp_facts: usize,
    pub assessments: usize,
    pub input_errors: Vec<String>,
    pub status_counts: BTreeMap<String, usize>,
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
    device_id: String,
    scope: Option<String>,
    network_namespace: Option<String>,
    entity: String,
    names: BTreeSet<String>,
    mac: Option<String>,
    ifindex: Option<u64>,
}

fn same_env(
    a_scope: &Option<String>,
    a_ns: &Option<String>,
    b_scope: &Option<String>,
    b_ns: &Option<String>,
) -> bool {
    // Namespace separation is explicit. Unknown namespace evidence does not
    // merge with a known namespace: that would cross an identity boundary.
    a_scope == b_scope && a_ns == b_ns
}

fn freshness(fact: &TypedFact, as_of: chrono::DateTime<chrono::FixedOffset>) -> FreshnessStatus {
    match fact.lifecycle {
        Lifecycle::Withdrawn => return FreshnessStatus::Withdrawn,
        Lifecycle::Superseded | Lifecycle::Stale => return FreshnessStatus::Stale,
        Lifecycle::Active => {}
    }
    if let Some(valid_until) = fact.valid_until.as_deref().and_then(parse_rfc3339) {
        if valid_until <= as_of {
            return FreshnessStatus::Stale;
        }
    }
    if let Some(valid_from) = fact.valid_from.as_deref().and_then(parse_rfc3339) {
        if valid_from > as_of {
            return FreshnessStatus::NotYetValid;
        }
    }
    if fact.valid_until.is_none() && fact.valid_from.is_none() {
        FreshnessStatus::Unknown
    } else {
        FreshnessStatus::Current
    }
}

fn payload_of(fact: &TypedFact) -> Option<LldpNeighborPayload> {
    if fact.fact_kind != crate::lldp_facts::FACT_KIND_LLDP_NEIGHBOR {
        return None;
    }
    let fields = fact.payload.fields.as_ref()?;
    serde_json::from_value(fields.clone()).ok()
}

fn interface_entries(facts: &[TypedFact]) -> (Vec<InterfaceEntry>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for fact in facts {
        if fact.fact_kind != crate::interface_facts::FACT_KIND_INTERFACE_STATE {
            continue;
        }
        let Some(fields) = fact.payload.fields.clone() else {
            errors.push(format!(
                "{}: interface fact has no typed fields",
                fact.fact_id
            ));
            continue;
        };
        let payload: InterfaceStatePayload = match serde_json::from_value(fields) {
            Ok(p) => p,
            Err(e) => {
                errors.push(format!(
                    "{}: interface payload malformed: {e}",
                    fact.fact_id
                ));
                continue;
            }
        };
        let Some(entity) = fact.subject.entity.clone() else {
            errors.push(format!("{}: interface fact has no entity", fact.fact_id));
            continue;
        };
        let mut names = BTreeSet::from([entity.clone(), payload.interface_name.clone()]);
        if let Some(n) = payload.normalized_name {
            names.insert(n);
        }
        out.push(InterfaceEntry {
            device_id: fact.subject.device_id.clone(),
            scope: fact.subject.scope.clone(),
            network_namespace: fact.subject.network_namespace.clone(),
            entity,
            names,
            mac: payload.mac_address.value,
            ifindex: payload.ifindex.value,
        });
    }
    (out, errors)
}

fn matching_identity<'a>(
    identities: &'a [DeviceIdentityEvidence],
    scope: &Option<String>,
    ns: &Option<String>,
    predicate: impl Fn(&DeviceIdentityEvidence) -> bool,
) -> Vec<&'a DeviceIdentityEvidence> {
    identities
        .iter()
        .filter(|id| same_env(&id.scope, &id.network_namespace, scope, ns) && predicate(id))
        .collect()
}

fn resolve_remote_device(
    entry: &LldpEntry,
    identities: &[DeviceIdentityEvidence],
) -> EndpointResolution {
    let scope = &entry.fact.subject.scope;
    let ns = &entry.fact.subject.network_namespace;
    let chassis = entry.payload.remote_chassis_id.value.as_deref();
    if let Some(chassis) = chassis {
        let matches = matching_identity(identities, scope, ns, |id| {
            id.chassis_ids
                .iter()
                .any(|c| c.eq_ignore_ascii_case(chassis))
        });
        if matches.len() == 1 {
            return EndpointResolution {
                status: ResolutionStatus::Resolved,
                candidates: vec![EndpointCandidate {
                    device_id: matches[0].device_id.clone(),
                    interface: None,
                }],
                reason: "unique independently established chassis identifier".into(),
            };
        }
        if matches.len() > 1 {
            return EndpointResolution {
                status: ResolutionStatus::Conflicting,
                candidates: matches
                    .iter()
                    .map(|id| EndpointCandidate {
                        device_id: id.device_id.clone(),
                        interface: None,
                    })
                    .collect(),
                reason: "chassis identifier matches multiple independent device identities".into(),
            };
        }
    }

    let mut candidates: BTreeMap<String, Vec<&'static str>> = BTreeMap::new();
    if let Some(hostname) = entry.payload.remote_system_name.value.as_deref() {
        for id in matching_identity(identities, scope, ns, |id| {
            id.hostnames.iter().any(|h| h == hostname)
        }) {
            candidates
                .entry(id.device_id.clone())
                .or_default()
                .push("hostname");
        }
    }
    if let Some(mgmt) = entry.payload.remote_management_addresses.value.as_ref() {
        for addr in mgmt {
            for id in matching_identity(identities, scope, ns, |id| {
                id.management_addresses.iter().any(|m| m == &addr.address)
            }) {
                candidates
                    .entry(id.device_id.clone())
                    .or_default()
                    .push("management_address");
            }
        }
    }
    if candidates.is_empty() {
        return EndpointResolution::unresolved(
            "no independently established chassis identity; hostname/management evidence absent",
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
            .keys()
            .map(|device_id| EndpointCandidate { device_id: device_id.clone(), interface: None })
            .collect(),
        reason: "hostname/management-address evidence is corroborating only, not sole canonical authority".into(),
    }
}

fn resolve_remote_interface(
    entry: &LldpEntry,
    device_resolution: &EndpointResolution,
    interfaces: &[InterfaceEntry],
) -> EndpointResolution {
    if device_resolution.status != ResolutionStatus::Resolved {
        return EndpointResolution::unresolved(
            "remote device is not resolved by independent identity evidence",
        );
    }
    let device = &device_resolution.candidates[0].device_id;
    let scope = &entry.fact.subject.scope;
    let ns = &entry.fact.subject.network_namespace;
    let port = entry.payload.remote_port_id.value.as_deref();
    let subtype = entry.payload.remote_port_id_subtype.value;
    let Some(port) = port else {
        return EndpointResolution::unresolved("remote port identifier absent");
    };
    let local_matches: Vec<&InterfaceEntry> = interfaces
        .iter()
        .filter(|i| &i.device_id == device && same_env(&i.scope, &i.network_namespace, scope, ns))
        .collect();
    let matches: Vec<&InterfaceEntry> = match subtype {
        Some(PortIdSubtype::InterfaceName) => local_matches
            .iter()
            .copied()
            .filter(|i| i.names.iter().any(|n| n == port))
            .collect(),
        Some(PortIdSubtype::MacAddress) => local_matches
            .iter()
            .copied()
            .filter(|i| {
                i.mac
                    .as_deref()
                    .is_some_and(|m| m.eq_ignore_ascii_case(port))
            })
            .collect(),
        Some(PortIdSubtype::IfIndex) => port
            .parse::<u64>()
            .ok()
            .map(|idx| {
                local_matches
                    .iter()
                    .copied()
                    .filter(|i| i.ifindex == Some(idx))
                    .collect::<Vec<_>>()
            })
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
            "no independently established interface on {device} matches subtype-aware remote port identifier"
        )),
        1 => EndpointResolution {
            status: ResolutionStatus::Resolved,
            candidates: vec![EndpointCandidate {
                device_id: device.clone(),
                interface: Some(matches[0].entity.clone()),
            }],
            reason: "unique subtype-aware interface match in independent interface evidence".into(),
        },
        _ => EndpointResolution {
            status: ResolutionStatus::Ambiguous,
            candidates: matches
                .iter()
                .map(|i| EndpointCandidate {
                    device_id: device.clone(),
                    interface: Some(i.entity.clone()),
                })
                .collect(),
            reason: "multiple independent interfaces match the remote port identifier".into(),
        },
    }
}

fn parse_ifindex(subtype: Option<PortIdSubtype>, value: Option<&str>) -> Option<u64> {
    (subtype == Some(PortIdSubtype::IfIndex))
        .then_some(value?)
        .and_then(|v| v.parse::<u64>().ok())
}

fn reverse_agreement(
    reverse: &LldpEntry,
    local: &LldpEntry,
    local_identity: Option<&DeviceIdentityEvidence>,
    interfaces: &[InterfaceEntry],
) -> IdentifierAgreement {
    let mut saw = false;
    let mut agreed = false;
    if let (Some(remote_chassis), Some(identity)) = (
        reverse.payload.remote_chassis_id.value.as_deref(),
        local_identity,
    ) {
        if !identity.chassis_ids.is_empty() {
            saw = true;
            if identity
                .chassis_ids
                .iter()
                .any(|c| c.eq_ignore_ascii_case(remote_chassis))
            {
                agreed = true;
            } else {
                return IdentifierAgreement::Conflict;
            }
        }
    }
    if let (Some(remote_name), Some(identity)) = (
        reverse.payload.remote_system_name.value.as_deref(),
        local_identity,
    ) {
        if !identity.hostnames.is_empty() {
            saw = true;
            if identity.hostnames.iter().any(|h| h == remote_name) {
                agreed = true;
            } else {
                return IdentifierAgreement::Conflict;
            }
        }
    }
    let reverse_port = reverse.payload.remote_port_id.value.as_deref();
    match reverse.payload.remote_port_id_subtype.value {
        Some(PortIdSubtype::InterfaceName) => {
            if let Some(port) = reverse_port {
                saw = true;
                if port == local.payload.local_interface_name {
                    agreed = true;
                } else {
                    return IdentifierAgreement::Conflict;
                }
            }
        }
        Some(PortIdSubtype::MacAddress) => {
            if let Some(port) = reverse_port {
                if let Some(local_if) = interfaces.iter().find(|i| {
                    i.device_id == local.fact.subject.device_id
                        && same_env(
                            &i.scope,
                            &i.network_namespace,
                            &local.fact.subject.scope,
                            &local.fact.subject.network_namespace,
                        )
                        && i.names
                            .iter()
                            .any(|n| n == &local.payload.local_interface_name)
                }) {
                    if let Some(mac) = local_if.mac.as_deref() {
                        saw = true;
                        if mac.eq_ignore_ascii_case(port) {
                            agreed = true;
                        } else {
                            return IdentifierAgreement::Conflict;
                        }
                    }
                }
            }
        }
        Some(PortIdSubtype::IfIndex) => {
            if let Some(idx) =
                parse_ifindex(reverse.payload.remote_port_id_subtype.value, reverse_port)
            {
                if let Some(local_if) = interfaces.iter().find(|i| {
                    i.device_id == local.fact.subject.device_id
                        && same_env(
                            &i.scope,
                            &i.network_namespace,
                            &local.fact.subject.scope,
                            &local.fact.subject.network_namespace,
                        )
                        && i.names
                            .iter()
                            .any(|n| n == &local.payload.local_interface_name)
                }) {
                    if let Some(local_idx) = local_if.ifindex {
                        saw = true;
                        if local_idx == idx {
                            agreed = true;
                        } else {
                            return IdentifierAgreement::Conflict;
                        }
                    }
                }
            }
        }
        _ => {}
    }
    if agreed {
        IdentifierAgreement::Agree
    } else if saw {
        IdentifierAgreement::Conflict
    } else {
        IdentifierAgreement::Unknown
    }
}

fn reverse_status(
    local: &LldpEntry,
    entries: &[LldpEntry],
    device_resolution: &EndpointResolution,
    interface_resolution: &EndpointResolution,
    identities: &[DeviceIdentityEvidence],
    interfaces: &[InterfaceEntry],
    coverage: &[CaptureCoverageEvidence],
    as_of: chrono::DateTime<chrono::FixedOffset>,
) -> (ReverseEvidenceStatus, IdentifierAgreement, Vec<String>) {
    if device_resolution.status != ResolutionStatus::Resolved
        || interface_resolution.status != ResolutionStatus::Resolved
    {
        return (
            ReverseEvidenceStatus::NotResolvable,
            IdentifierAgreement::Unknown,
            vec![],
        );
    }
    let remote_device = &device_resolution.candidates[0].device_id;
    let remote_interface = interface_resolution.candidates[0]
        .interface
        .as_deref()
        .unwrap_or_default();
    let scope = &local.fact.subject.scope;
    let ns = &local.fact.subject.network_namespace;
    let reverses: Vec<&LldpEntry> = entries
        .iter()
        .filter(|r| {
            r.fact.subject.device_id == *remote_device
                && same_env(
                    &r.fact.subject.scope,
                    &r.fact.subject.network_namespace,
                    scope,
                    ns,
                )
                && r.payload.local_interface_name == remote_interface
        })
        .collect();
    if reverses.is_empty() {
        let cov = coverage.iter().find(|c| {
            c.device_id == *remote_device && same_env(&c.scope, &c.network_namespace, scope, ns)
        });
        let status = match cov {
            Some(c)
                if c.capture_outcome == LldpCaptureOutcome::Complete
                    && c.population_coverage == PopulationCoverage::Complete =>
            {
                ReverseEvidenceStatus::MissingReverse
            }
            _ => ReverseEvidenceStatus::CoverageUnknown,
        };
        return (status, IdentifierAgreement::Unknown, vec![]);
    }
    let local_identity = identities.iter().find(|id| {
        id.device_id == local.fact.subject.device_id
            && same_env(&id.scope, &id.network_namespace, scope, ns)
    });
    let mut saw_stale = false;
    let mut saw_withdrawn = false;
    let mut best = (
        ReverseEvidenceStatus::CoverageUnknown,
        IdentifierAgreement::Unknown,
        vec![],
    );
    for reverse in reverses {
        let fresh = freshness(&reverse.fact, as_of);
        if fresh == FreshnessStatus::Withdrawn {
            saw_withdrawn = true;
            continue;
        }
        if fresh == FreshnessStatus::Stale || fresh == FreshnessStatus::NotYetValid {
            saw_stale = true;
            continue;
        }
        let agreement = reverse_agreement(reverse, local, local_identity, interfaces);
        let status = match agreement {
            IdentifierAgreement::Agree => ReverseEvidenceStatus::Confirmed,
            IdentifierAgreement::Conflict => ReverseEvidenceStatus::Conflicting,
            IdentifierAgreement::Unknown => ReverseEvidenceStatus::Candidate,
        };
        let evidence = vec![reverse.fact.fact_id.clone()];
        if status == ReverseEvidenceStatus::Confirmed {
            return (status, agreement, evidence);
        }
        if best.0 == ReverseEvidenceStatus::CoverageUnknown
            || status == ReverseEvidenceStatus::Conflicting
        {
            best = (status, agreement, evidence);
        }
    }
    if best.0 != ReverseEvidenceStatus::CoverageUnknown {
        return best;
    }
    if saw_withdrawn {
        (
            ReverseEvidenceStatus::Withdrawn,
            IdentifierAgreement::Unknown,
            vec![],
        )
    } else if saw_stale {
        (
            ReverseEvidenceStatus::Stale,
            IdentifierAgreement::Unknown,
            vec![],
        )
    } else {
        (
            ReverseEvidenceStatus::CoverageUnknown,
            IdentifierAgreement::Unknown,
            vec![],
        )
    }
}

fn compatibility(entry: &LldpEntry, entries: &[LldpEntry]) -> ObservationCompatibility {
    let same_endpoint: Vec<&LldpEntry> = entries
        .iter()
        .filter(|r| {
            r.fact.subject.device_id == entry.fact.subject.device_id
                && same_env(
                    &r.fact.subject.scope,
                    &r.fact.subject.network_namespace,
                    &entry.fact.subject.scope,
                    &entry.fact.subject.network_namespace,
                )
                && r.payload.local_interface_name == entry.payload.local_interface_name
        })
        .collect();
    if same_endpoint.len() <= 1 {
        return ObservationCompatibility::Compatible;
    }
    let key = (
        entry.payload.remote_chassis_id.value.as_deref(),
        entry.payload.remote_port_id.value.as_deref(),
        entry.payload.remote_system_name.value.as_deref(),
    );
    let mut exact = 0usize;
    let mut identity_conflict = false;
    for other in same_endpoint {
        let other_key = (
            other.payload.remote_chassis_id.value.as_deref(),
            other.payload.remote_port_id.value.as_deref(),
            other.payload.remote_system_name.value.as_deref(),
        );
        if other_key == key {
            exact += 1;
        } else if other.payload.remote_chassis_id.value == entry.payload.remote_chassis_id.value
            && other.payload.remote_port_id.value == entry.payload.remote_port_id.value
        {
            identity_conflict = true;
        }
    }
    if identity_conflict {
        ObservationCompatibility::Conflicting
    } else if exact > 1 {
        ObservationCompatibility::Duplicate
    } else {
        // Multiple distinct neighbors on one interface are valid evidence, not a conflict.
        ObservationCompatibility::Compatible
    }
}

/// Reconcile typed LLDP observations against independently established
/// device/interface identity at one explicit analysis time.
pub fn reconcile_lldp_adjacency(
    input: &AdjacencyReconciliationInput,
) -> AdjacencyReconciliationOutput {
    let mut errors = Vec::new();
    let Some(as_of) = parse_rfc3339(input.as_of) else {
        return AdjacencyReconciliationOutput {
            assessments: vec![],
            report: AdjacencyReconciliationReport {
                input_lldp_facts: input.lldp_facts.len(),
                parsed_lldp_facts: 0,
                assessments: 0,
                input_errors: vec!["as_of is not valid RFC-3339; analysis refused".into()],
                status_counts: BTreeMap::new(),
            },
        };
    };
    let (interfaces, interface_errors) = interface_entries(input.interface_facts);
    errors.extend(interface_errors);
    let mut entries = Vec::new();
    for fact in input.lldp_facts {
        if fact.fact_kind != crate::lldp_facts::FACT_KIND_LLDP_NEIGHBOR {
            continue;
        }
        match payload_of(fact) {
            Some(payload) => entries.push(LldpEntry {
                fact: fact.clone(),
                payload,
            }),
            None => errors.push(format!(
                "{}: LLDP typed fields absent or malformed; skipped",
                fact.fact_id
            )),
        }
    }

    let mut assessments = Vec::new();
    for entry in &entries {
        let remote_device = resolve_remote_device(entry, input.device_identities);
        let remote_interface = resolve_remote_interface(entry, &remote_device, &interfaces);
        let (reverse, agreement, reverse_evidence_ids) = reverse_status(
            entry,
            &entries,
            &remote_device,
            &remote_interface,
            input.device_identities,
            &interfaces,
            input.capture_coverage,
            as_of,
        );
        let mut prerequisites = Vec::new();
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
        let compat = compatibility(entry, &entries);
        let mut evidence_fact_ids = vec![entry.fact.fact_id.clone()];
        evidence_fact_ids.extend(reverse_evidence_ids);
        assessments.push(AdjacencyAssessment {
            fact_id: entry.fact.fact_id.clone(),
            local_device_id: entry.fact.subject.device_id.clone(),
            scope: entry.fact.subject.scope.clone(),
            network_namespace: entry.fact.subject.network_namespace.clone(),
            local_interface: entry.payload.local_interface_name.clone(),
            remote_chassis_id: entry.payload.remote_chassis_id.value.clone(),
            remote_chassis_id_subtype: entry
                .payload
                .remote_chassis_id_subtype
                .value
                .map(|v| match v {
                    ChassisIdSubtype::MacAddress => "mac_address",
                    ChassisIdSubtype::NetworkAddress => "network_address",
                    ChassisIdSubtype::InterfaceName => "interface_name",
                    ChassisIdSubtype::InterfaceAlias => "interface_alias",
                    ChassisIdSubtype::ChassisComponent => "chassis_component",
                    ChassisIdSubtype::LocallyAssigned => "locally_assigned",
                    ChassisIdSubtype::Unknown => "unknown",
                })
                .map(str::to_string),
            remote_port_id: entry.payload.remote_port_id.value.clone(),
            remote_port_id_subtype: entry
                .payload
                .remote_port_id_subtype
                .value
                .map(|v| match v {
                    PortIdSubtype::InterfaceAlias => "interface_alias",
                    PortIdSubtype::MacAddress => "mac_address",
                    PortIdSubtype::NetworkAddress => "network_address",
                    PortIdSubtype::InterfaceName => "interface_name",
                    PortIdSubtype::IfIndex => "if_index",
                    PortIdSubtype::LocallyAssigned => "locally_assigned",
                    PortIdSubtype::Unknown => "unknown",
                })
                .map(str::to_string),
            remote_system_name: entry.payload.remote_system_name.value.clone(),
            remote_device_resolution: remote_device,
            remote_interface_resolution: remote_interface,
            reverse_evidence: reverse,
            identifier_agreement: agreement,
            freshness: fresh,
            observation_compatibility: compat,
            unresolved_prerequisites: prerequisites,
            evidence_fact_ids,
        });
    }
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
        },
        assessments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_facts::{
        AdminState, InterfaceType, OperState, SpeedObservation, SpeedUnit,
    };
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
                scope: Some("test".into()),
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
            coverage: LldpCoverage {
                capture_outcome: LldpCaptureOutcome::Complete,
                population_coverage: PopulationCoverage::Complete,
                empty_local_interfaces: 0,
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
            speed: ObservedField::known(SpeedObservation {
                value: 0,
                unit: SpeedUnit::Unknown,
            }),
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
            scope: Some("test".into()),
            network_namespace: ns.map(str::to_string),
            chassis_ids: vec![chassis.into()],
            hostnames: vec![host.into()],
            management_addresses: vec![],
        }
    }

    fn coverage(
        device: &str,
        ns: Option<&str>,
        population: PopulationCoverage,
    ) -> CaptureCoverageEvidence {
        CaptureCoverageEvidence {
            device_id: device.into(),
            scope: Some("test".into()),
            network_namespace: ns.map(str::to_string),
            capture_outcome: LldpCaptureOutcome::Complete,
            population_coverage: population,
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
        let mut ambiguous = neighbor_fact(
            "leaf1",
            "Ethernet1",
            "cc:cc:cc:cc:cc:99",
            "Ethernet1",
            "leaf2",
            PortIdSubtype::InterfaceName,
            Some("blue"),
            20,
        );
        ambiguous.payload.fields.as_mut().unwrap()["remote_chassis_id"] =
            serde_json::json!({"state":"unknown"});
        ambiguous.seal();
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
                .any(|e| e.contains("malformed"))
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
}
