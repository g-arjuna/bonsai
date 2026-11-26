//! Typed payload contract for `fact_kind = "lldp.neighbor"` (BN-024-06).
//!
//! The payload is deliberately subtype-aware: an LLDP port identifier is not
//! always an interface name, a locally assigned opaque value is not an
//! ifindex without independent source evidence, and a chassis identifier is
//! never a system-name substitute. Every observed attribute uses the same
//! explicit availability discipline as `interface.state`; malformed,
//! unsupported, missing and positively empty values remain distinct.
//!
//! This module is pure data + validation. It performs no acquisition, no
//! device resolution and no graph mutation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::interface_facts::{FieldAvailability, ObservedField, normalize_mac, valid_name};
use crate::typed_facts::{EvidenceRef, FactError};

/// Dotted fact kind for one observed LLDP neighbor record.
pub const FACT_KIND_LLDP_NEIGHBOR: &str = "lldp.neighbor";

const MAX_IDENTIFIER_LEN: usize = 255;
const MAX_DETAIL_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 512;
const MAX_MGMT_ADDRS: usize = 16;
const MAX_CAPABILITIES: usize = 32;

/// Normalized semantics of an LLDP remote port identifier. `LocallyAssigned`
/// preserves the LLDP subtype exactly; it is NOT an ifindex interpretation.
/// `IfIndex` may only be used when the producing source contract establishes
/// that the identifier is an interface index.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PortIdSubtype {
    InterfaceName,
    MacAddress,
    InterfaceAlias,
    LocallyAssigned,
    NetworkAddress,
    IfIndex,
    /// Subtype evidence exists but is outside the typed vocabulary.
    Unknown,
}

/// Normalized semantics of an LLDP chassis identifier.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ChassisIdSubtype {
    MacAddress,
    NetworkAddress,
    InterfaceName,
    InterfaceAlias,
    LocallyAssigned,
    ChassisComponent,
    /// Subtype evidence exists but is outside the typed vocabulary.
    Unknown,
}

/// Subtype evidence with both normalized semantics and the exact source
/// spelling. `raw` is evidence only; consumers must use `value` for typed
/// reasoning. `Known` requires `value`; non-known states forbid it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(bound(deserialize = "T: serde::Deserialize<'de>"))]
pub struct SubtypeObservation<T> {
    pub state: FieldAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    /// Exact subtype token as reported by the source, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl<T> SubtypeObservation<T> {
    pub fn known(value: T, raw: Option<String>) -> Self {
        Self {
            state: FieldAvailability::Known,
            value: Some(value),
            raw,
            detail: None,
        }
    }

    pub fn unavailable(
        state: FieldAvailability,
        raw: Option<String>,
        detail: Option<String>,
    ) -> Self {
        debug_assert_ne!(state, FieldAvailability::Known);
        Self {
            state,
            value: None,
            raw,
            detail,
        }
    }
}

/// Management-address family as reported by the device. `Unknown` is not an
/// IP parse failure; it means the source did not establish the family.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ManagementAddressType {
    Ipv4,
    Ipv6,
    Unknown,
}

/// One remote management-address TLV. Interface-number semantics are source
/// evidence only; `interface_num` is not an authorization to resolve the
/// neighbor port through ifindex unless the port-ID subtype independently
/// establishes that interpretation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagementAddress {
    pub address: String,
    pub address_type: ManagementAddressType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_num_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_num: Option<u64>,
}

/// One capability entry. LLDP sources differ in whether they expose the
/// supported/advertised bit, the enabled bit, or only one combined set;
/// absence remains `None` rather than being collapsed into the other bit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityObservation {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertised: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// Source temporal evidence semantics for LLDP validity. An advertised TTL
/// is a lifetime input, not automatically a fresh lifetime starting at the
/// replay/normalization time.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LldpValidityBasis {
    /// The source contract does not establish enough information to compute
    /// exact expiry. Consumers must use explicit unknown freshness.
    Unknown,
    /// The source contract establishes expiry as last-contact time plus the
    /// advertised TTL seconds (Arista EOS structured JSON in the qualified
    /// fixture). This does not merge last-contact, last-change, discovery or
    /// capture time; those remain separate evidence fields.
    LastContactPlusAdvertisedTtl,
}

/// Temporal evidence for one LLDP observation. Every timestamp is RFC-3339
/// after exact source-unit conversion. These fields are intentionally
/// separate: discovery age, last contact, last change, capture time and
/// observation time are not interchangeable.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LldpTemporalEvidence {
    pub capture_at: ObservedField<String>,
    pub last_contact_at: ObservedField<String>,
    pub last_change_at: ObservedField<String>,
    pub neighbor_discovery_at: ObservedField<String>,
    pub validity_basis: LldpValidityBasis,
}

/// Capture/population coverage carried structurally in every neighbor
/// payload. This is the analysis input contract — consumers must not infer
/// coverage by searching free-form quality notes.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LldpCaptureOutcome {
    Complete,
    Partial,
    Unavailable,
    Failed,
    TimedOut,
    Truncated,
    Unsupported,
    Unknown,
}

/// Population coverage for the declared acquisition scope. `Complete` means
/// the decoded input covered the source's declared LLDP neighbor population
/// (including positively empty interfaces); it does not prove every device
/// in a fleet was captured. This axis is deliberately independent from
/// capture transport outcome: a complete capture can still have partial or
/// unknown population coverage when records are malformed or unavailable.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PopulationCoverage {
    Complete,
    Partial,
    Unknown,
}

/// Typed acquisition coverage for the observation batch that contained this
/// neighbor. Per-neighbor facts duplicate this batch-level metadata so pure
/// reconciliation can reason without string scraping.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LldpCoverage {
    pub capture_outcome: LldpCaptureOutcome,
    pub population_coverage: PopulationCoverage,
    /// Number of interfaces in the same decoded capture whose neighbor list
    /// was present and empty. Retained for compact compatibility; the names
    /// are carried separately because a count alone cannot establish which
    /// interface a reverse lookup covers.
    pub empty_local_interfaces: u32,
    /// Positively empty interface identities, bounded. Empty means either no
    /// positive-empty evidence or that the producer could not establish it;
    /// use `covered_interfaces` plus `population_coverage` for interpretation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub empty_local_interface_names: Vec<String>,
    /// Capture identity copied from the producing fact source so batch
    /// coverage remains interpretable when payload evidence is exported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_generation: Option<u64>,
    /// Observation time of the acquisition batch (RFC-3339), distinct from
    /// neighbor last-contact/change times.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_time: Option<String>,
    /// Exact acquisition scope selector established by the producer. This is
    /// evidence, not a tenant-authority expansion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquisition_scope: Option<String>,
    /// Interface identities whose neighbor collections were actually decoded
    /// in this acquisition batch, including positively empty interfaces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covered_interfaces: Vec<String>,
    /// Supporting evidence references for the coverage statement itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_references: Vec<EvidenceRef>,
}

/// Neighbor-discovery protocol. CDP must never be silently represented as
/// LLDP merely because a upstream table merges the two.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NeighborProtocol {
    Lldp,
}

/// Typed `lldp.neighbor` payload. Identity axes for the LOCAL endpoint live
/// on `TypedFact::subject`; this struct carries the source-native local
/// occurrence and the remote neighbor evidence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LldpNeighborPayload {
    /// Local interface name exactly as observed.
    pub local_interface_name: String,
    /// Canonical/normalized local occurrence name when it differs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_local_interface_name: Option<String>,
    /// Independently established source occurrence identifier (for example a
    /// protocol/path key the source contract assigns to this neighbor). This
    /// is evidence for occurrence identity, never canonical device identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_neighbor_id: Option<String>,
    /// Capture-local record locator such as an array index. It identifies a
    /// position in this capture for diagnostics only and is not independent
    /// neighbor identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_record_locator: Option<String>,
    pub protocol: NeighborProtocol,
    /// Remote chassis identifier. MAC identifiers are normalized to
    /// lowercase colon form; `remote_chassis_id_raw` preserves the exact
    /// source spelling when normalization changed it.
    pub remote_chassis_id: ObservedField<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_chassis_id_raw: Option<String>,
    pub remote_chassis_id_subtype: SubtypeObservation<ChassisIdSubtype>,
    /// Remote port identifier exactly according to its subtype semantics.
    /// It is a string even when numeric; only `IfIndex` subtype permits a
    /// numeric interpretation by consumers.
    pub remote_port_id: ObservedField<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_port_id_raw: Option<String>,
    pub remote_port_id_subtype: SubtypeObservation<PortIdSubtype>,
    /// Remote system name exactly as advertised. Absence is explicit; this
    /// is never derived from chassis ID.
    pub remote_system_name: ObservedField<String>,
    pub remote_port_description: ObservedField<String>,
    /// All remote management addresses preserved in source order. `Known`
    /// with an empty list is positive evidence that none were advertised;
    /// `Unknown` means the source did not expose the field.
    pub remote_management_addresses: ObservedField<Vec<ManagementAddress>>,
    pub capabilities: ObservedField<Vec<CapabilityObservation>>,
    /// LLDP TTL seconds when directly observed. Consumers may use this as a
    /// validity input; it is distinct from observation/capture interval.
    pub ttl_seconds: ObservedField<u32>,
    /// Age seconds when directly observed and unambiguously reported.
    pub age_seconds: ObservedField<u64>,
    /// Exact temporal evidence and validity semantics when the source
    /// contract establishes them. Absence means unknown, not a fresh TTL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal: Option<LldpTemporalEvidence>,
    pub coverage: LldpCoverage,
}

fn valid_identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_IDENTIFIER_LEN && !s.chars().any(|c| c.is_control())
}

fn valid_detail(s: &Option<String>) -> bool {
    s.as_deref()
        .is_none_or(|d| d.len() <= MAX_DETAIL_LEN && !d.chars().any(|c| c.is_control()))
}

fn check_field<T>(
    field: &ObservedField<T>,
    name: &'static str,
    extra: impl Fn(&T) -> bool,
) -> Result<(), FactError> {
    match field.state {
        FieldAvailability::Known => {
            let Some(v) = &field.value else {
                return Err(FactError::InvalidField(name));
            };
            if !extra(v) {
                return Err(FactError::InvalidField(name));
            }
        }
        _ => {
            if field.value.is_some() {
                return Err(FactError::InvalidField(name));
            }
        }
    }
    if !valid_detail(&field.detail) {
        return Err(FactError::InvalidField(name));
    }
    Ok(())
}

fn check_subtype<T>(field: &SubtypeObservation<T>, name: &'static str) -> Result<(), FactError> {
    match field.state {
        FieldAvailability::Known => {
            if field.value.is_none() {
                return Err(FactError::InvalidField(name));
            }
        }
        _ => {
            if field.value.is_some() {
                return Err(FactError::InvalidField(name));
            }
        }
    }
    if let Some(raw) = &field.raw
        && (raw.is_empty() || raw.len() > 128 || raw.chars().any(|c| c.is_control()))
    {
        return Err(FactError::InvalidField(name));
    }
    if !valid_detail(&field.detail) {
        return Err(FactError::InvalidField(name));
    }
    Ok(())
}

fn valid_mgmt_addr(addr: &ManagementAddress) -> bool {
    if addr.address.is_empty()
        || addr.address.len() > 64
        || addr.address.chars().any(|c| c.is_control())
    {
        return false;
    }
    (match addr.address_type {
        ManagementAddressType::Ipv4 => addr.address.parse::<std::net::Ipv4Addr>().is_ok(),
        ManagementAddressType::Ipv6 => addr.address.parse::<std::net::Ipv6Addr>().is_ok(),
        ManagementAddressType::Unknown => true,
    }) && addr
        .interface_num_type
        .as_deref()
        .is_none_or(|v| !v.is_empty() && v.len() <= 64 && !v.chars().any(|c| c.is_control()))
}

fn valid_capability(cap: &CapabilityObservation) -> bool {
    !cap.name.is_empty()
        && cap.name.len() <= 64
        && !cap.name.chars().any(|c| c.is_control())
        && (cap.advertised.is_some() || cap.enabled.is_some())
}

/// Validate a serialized `lldp.neighbor` payload. Called by
/// `TypedFact::validate` for this registered fact kind.
pub fn validate_payload(fields: &Value) -> Result<(), FactError> {
    let payload: LldpNeighborPayload = serde_json::from_value(fields.clone())
        .map_err(|_| FactError::InvalidField("payload.fields.lldp.neighbor"))?;
    validate_typed(&payload)
}

/// Semantic validation of an already-typed payload.
pub fn validate_typed(p: &LldpNeighborPayload) -> Result<(), FactError> {
    if !valid_name(&p.local_interface_name) {
        return Err(FactError::InvalidField("local_interface_name"));
    }
    if let Some(n) = &p.normalized_local_interface_name
        && !valid_name(n)
    {
        return Err(FactError::InvalidField("normalized_local_interface_name"));
    }
    if let Some(n) = &p.source_neighbor_id
        && !valid_identifier(n)
    {
        return Err(FactError::InvalidField("source_neighbor_id"));
    }
    check_field(&p.remote_chassis_id, "remote_chassis_id", |v| {
        valid_identifier(v)
    })?;
    check_field(&p.remote_port_id, "remote_port_id", |v| valid_identifier(v))?;
    check_field(&p.remote_system_name, "remote_system_name", |v| {
        valid_identifier(v)
    })?;
    check_field(&p.remote_port_description, "remote_port_description", |v| {
        v.len() <= MAX_DESCRIPTION_LEN && !v.chars().any(|c| c.is_control())
    })?;
    check_subtype(&p.remote_chassis_id_subtype, "remote_chassis_id_subtype")?;
    check_subtype(&p.remote_port_id_subtype, "remote_port_id_subtype")?;
    // LLDP chassis-ID and port-ID are protocol-mandatory neighbor identity.
    // Malformed/absent identity belongs in a degraded batch status or diagnostic,
    // never in a validated neighbor entity fact.
    if p.remote_chassis_id.state != FieldAvailability::Known || p.remote_chassis_id.value.is_none()
    {
        return Err(FactError::InvalidField("remote_chassis_id mandatory"));
    }
    if p.remote_port_id.state != FieldAvailability::Known || p.remote_port_id.value.is_none() {
        return Err(FactError::InvalidField("remote_port_id mandatory"));
    }
    if p.remote_chassis_id_subtype.state != FieldAvailability::Known
        || p.remote_chassis_id_subtype.value.is_none()
        || p.remote_port_id_subtype.state != FieldAvailability::Known
        || p.remote_port_id_subtype.value.is_none()
    {
        return Err(FactError::InvalidField("identity subtype mandatory"));
    }

    for raw in [&p.remote_chassis_id_raw, &p.remote_port_id_raw]
        .into_iter()
        .flatten()
    {
        if !valid_identifier(raw) {
            return Err(FactError::InvalidField("identifier raw evidence"));
        }
    }

    // MAC subtype/identifier agreement: when both pieces are known, the
    // canonical stored value must already be normalized lowercase colon form.
    if p.remote_chassis_id_subtype.value == Some(ChassisIdSubtype::MacAddress)
        && p.remote_chassis_id.state == FieldAvailability::Known
    {
        let value = p.remote_chassis_id.value.as_deref().unwrap_or("");
        if normalize_mac(value).as_deref() != Some(value) {
            return Err(FactError::InvalidField(
                "remote_chassis_id mac normalization",
            ));
        }
    }
    if p.remote_port_id_subtype.value == Some(PortIdSubtype::MacAddress)
        && p.remote_port_id.state == FieldAvailability::Known
    {
        let value = p.remote_port_id.value.as_deref().unwrap_or("");
        if normalize_mac(value).as_deref() != Some(value) {
            return Err(FactError::InvalidField("remote_port_id mac normalization"));
        }
    }
    if p.remote_port_id_subtype.value == Some(PortIdSubtype::IfIndex)
        && p.remote_port_id.state == FieldAvailability::Known
    {
        let value = p.remote_port_id.value.as_deref().unwrap_or("");
        if value.parse::<u64>().ok().filter(|v| *v > 0).is_none() {
            return Err(FactError::InvalidField("remote_port_id ifindex"));
        }
    }

    check_field(
        &p.remote_management_addresses,
        "remote_management_addresses",
        |addrs| {
            addrs.len() <= MAX_MGMT_ADDRS && addrs.iter().all(valid_mgmt_addr) && {
                let mut seen = std::collections::BTreeSet::new();
                addrs
                    .iter()
                    .all(|a| seen.insert((a.address.as_str(), a.address_type, a.interface_num)))
            }
        },
    )?;
    check_field(&p.capabilities, "capabilities", |caps| {
        caps.len() <= MAX_CAPABILITIES && caps.iter().all(valid_capability) && {
            let mut seen = std::collections::BTreeSet::new();
            caps.iter().all(|c| seen.insert(c.name.as_str()))
        }
    })?;
    check_field(&p.ttl_seconds, "ttl_seconds", |_| true)?;
    check_field(&p.age_seconds, "age_seconds", |_| true)?;
    if let Some(locator) = &p.capture_record_locator
        && !valid_identifier(locator)
    {
        return Err(FactError::InvalidField("capture_record_locator"));
    }
    if let Some(temporal) = &p.temporal {
        if temporal.validity_basis == LldpValidityBasis::LastContactPlusAdvertisedTtl
            && (temporal.last_contact_at.state != FieldAvailability::Known
                || temporal.last_contact_at.value.is_none()
                || p.ttl_seconds.state != FieldAvailability::Known
                || p.ttl_seconds.value.is_none())
        {
            return Err(FactError::InvalidField("temporal.validity_basis evidence"));
        }
        for (field, name) in [
            (&temporal.capture_at, "temporal.capture_at"),
            (&temporal.last_contact_at, "temporal.last_contact_at"),
            (&temporal.last_change_at, "temporal.last_change_at"),
            (
                &temporal.neighbor_discovery_at,
                "temporal.neighbor_discovery_at",
            ),
        ] {
            check_field(field, name, |v| {
                chrono::DateTime::parse_from_rfc3339(v).is_ok()
            })?;
        }
    }

    if p.coverage.empty_local_interfaces > 4096 {
        return Err(FactError::InvalidField("coverage.empty_local_interfaces"));
    }
    if p.coverage.empty_local_interface_names.len() > 4096
        || !p
            .coverage
            .empty_local_interface_names
            .iter()
            .all(|n| valid_name(n))
    {
        return Err(FactError::InvalidField(
            "coverage.empty_local_interface_names",
        ));
    }
    if p.coverage.covered_interfaces.len() > 4096
        || !p.coverage.covered_interfaces.iter().all(|n| valid_name(n))
    {
        return Err(FactError::InvalidField("coverage.covered_interfaces"));
    }
    if let Some(ts) = &p.coverage.observation_time
        && chrono::DateTime::parse_from_rfc3339(ts).is_err()
    {
        return Err(FactError::InvalidField("coverage.observation_time"));
    }
    for (value, name) in [
        (&p.coverage.capture_id, "coverage.capture_id"),
        (&p.coverage.acquisition_scope, "coverage.acquisition_scope"),
    ] {
        if let Some(value) = value
            && (value.is_empty() || value.len() > 255 || value.chars().any(|c| c.is_control()))
        {
            return Err(FactError::InvalidField(name));
        }
    }
    if p.coverage.evidence_references.len() > 32
        || p.coverage.evidence_references.iter().any(|e| {
            e.kind.is_empty()
                || e.sha256.len() != 64
                || !e.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        })
    {
        return Err(FactError::InvalidField("coverage.evidence_references"));
    }
    if matches!(
        p.coverage.capture_outcome,
        LldpCaptureOutcome::Unavailable
            | LldpCaptureOutcome::Failed
            | LldpCaptureOutcome::TimedOut
            | LldpCaptureOutcome::Unsupported
    ) {
        return Err(FactError::InvalidField(
            "coverage.capture_outcome unusable for neighbor fact",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known<T>(value: T) -> ObservedField<T> {
        ObservedField::known(value)
    }

    fn payload() -> LldpNeighborPayload {
        LldpNeighborPayload {
            local_interface_name: "swp1".into(),
            normalized_local_interface_name: None,
            source_neighbor_id: Some("0".into()),
            capture_record_locator: None,
            protocol: NeighborProtocol::Lldp,
            remote_chassis_id: known("52:54:00:a0:1a:f4".into()),
            remote_chassis_id_raw: None,
            remote_chassis_id_subtype: SubtypeObservation::known(
                ChassisIdSubtype::MacAddress,
                Some("mac".into()),
            ),
            remote_port_id: known("swp2".into()),
            remote_port_id_raw: None,
            remote_port_id_subtype: SubtypeObservation::known(
                PortIdSubtype::InterfaceName,
                Some("ifname".into()),
            ),
            remote_system_name: known("spine01".into()),
            remote_port_description: known("swp2".into()),
            remote_management_addresses: known(vec![]),
            capabilities: known(vec![CapabilityObservation {
                name: "Router".into(),
                advertised: None,
                enabled: Some(true),
            }]),
            ttl_seconds: known(120),
            age_seconds: known(384),
            temporal: None,
            coverage: LldpCoverage {
                capture_outcome: LldpCaptureOutcome::Complete,
                population_coverage: PopulationCoverage::Complete,
                empty_local_interfaces: 0,
                empty_local_interface_names: vec![],
                capture_id: None,
                source_generation: None,
                observation_time: None,
                acquisition_scope: None,
                covered_interfaces: vec![],
                evidence_references: vec![],
            },
        }
    }

    #[test]
    fn valid_payload_accepted() {
        validate_typed(&payload()).expect("valid payload");
    }

    #[test]
    fn locally_assigned_is_not_ifindex() {
        let mut p = payload();
        p.remote_port_id = known("42".into());
        p.remote_port_id_subtype =
            SubtypeObservation::known(PortIdSubtype::LocallyAssigned, Some("local".into()));
        validate_typed(&p).expect("opaque local assignment is valid");
        p.remote_port_id_subtype =
            SubtypeObservation::known(PortIdSubtype::IfIndex, Some("ifindex".into()));
        validate_typed(&p).expect("numeric ifindex valid when established");
        p.remote_port_id = known("not-a-number".into());
        assert!(validate_typed(&p).is_err());
    }

    #[test]
    fn mac_subtype_requires_normalized_mac() {
        let mut p = payload();
        p.remote_port_id_subtype =
            SubtypeObservation::known(PortIdSubtype::MacAddress, Some("mac".into()));
        p.remote_port_id = known("5254.003b.5f14".into());
        assert!(validate_typed(&p).is_err());
        p.remote_port_id = known("52:54:00:3b:5f:14".into());
        validate_typed(&p).expect("normalized mac valid");
    }

    #[test]
    fn empty_management_list_is_positive_empty_not_unknown() {
        let p = payload();
        assert_eq!(
            p.remote_management_addresses.value.as_ref().map(Vec::len),
            Some(0)
        );
        validate_typed(&p).expect("known empty list valid");
    }

    #[test]
    fn missing_protocol_mandatory_identity_is_invalid() {
        let mut p = payload();
        p.remote_port_id = ObservedField::unavailable(FieldAvailability::Unknown, None);
        assert!(validate_typed(&p).is_err());
        let mut p = payload();
        p.remote_chassis_id_subtype =
            SubtypeObservation::unavailable(FieldAvailability::Unknown, None, None);
        assert!(validate_typed(&p).is_err());
    }

    #[test]
    fn last_contact_ttl_validity_basis_requires_exact_evidence() {
        let mut p = payload();
        p.temporal = Some(LldpTemporalEvidence {
            capture_at: ObservedField::unavailable(FieldAvailability::Unknown, None),
            last_contact_at: ObservedField::known("2026-01-01T00:00:00Z".into()),
            last_change_at: ObservedField::unavailable(FieldAvailability::Unknown, None),
            neighbor_discovery_at: ObservedField::unavailable(FieldAvailability::Unknown, None),
            validity_basis: LldpValidityBasis::LastContactPlusAdvertisedTtl,
        });
        p.ttl_seconds = ObservedField::unavailable(FieldAvailability::Unknown, None);
        assert!(validate_typed(&p).is_err());
    }
}
