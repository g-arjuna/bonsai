//! Native structured LLDP neighbor normalization (BN-024-06).
//!
//! Initial genuine structured sources:
//!
//! - Cumulus Linux `net show lldp json` (lldpd JSON), pinned upstream
//!   selector `suzieq/config/lldp.yml` @ `75198b5…` (`platform=cumulus`,
//!   `version: all`). The qualified fixture in this repository is genuine
//!   upstream Cumulus Linux 4.3.0 capture evidence; `version: all` remains
//!   an upstream selector claim, not broad release qualification.
//! - Arista EOS `show lldp neighbors detail` structured JSON, same pinned
//!   upstream selector (`version: all`). The qualified fixture is genuine
//!   upstream capture evidence whose local EOS release is not recorded;
//!   release applicability remains explicitly unknown.
//!
//! Source-informed deviations from SuzieQ `services/lldp.py`:
//!
//! - every neighbor entry in an array is preserved; no first-entry selection;
//! - management addresses and capabilities are preserved as bounded lists;
//! - MAC-form identifiers are normalized only when their subtype establishes
//!   MAC semantics, with raw spelling preserved when normalization changed it;
//! - `local` remains `locally assigned` opaque evidence and is never treated
//!   as an ifindex merely because it is numeric;
//! - rows missing protocol-mandatory chassis or port identifiers are counted
//!   as invalid evidence rather than being silently dropped or coerced;
//! - empty neighbor arrays are positive empty-interface evidence and are
//!   distinct from a failed/partial capture;
//! - no hostname, management-address or port-alias resolution is performed
//!   here. Resolution belongs to deterministic reconciliation with
//!   independently established identity evidence.
//!
//! The module is offline/pure normalization. It does not execute upstream
//! command strings, write graphs, or claim production acquisition.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Value;

use crate::interface_facts::{FieldAvailability, ObservedField, normalize_mac, valid_name};
use crate::interface_normalizer::{
    CaptureOutcome, CommandCapture, NormalizerLimits, ObservationContext, SourceCaptureReport,
    parse_rfc3339, sha256_hex,
};
use crate::lldp_facts::{
    CapabilityObservation, ChassisIdSubtype, FACT_KIND_LLDP_NEIGHBOR, LldpCaptureOutcome,
    LldpCoverage, LldpNeighborPayload, LldpTemporalEvidence, LldpValidityBasis, ManagementAddress,
    ManagementAddressType, NeighborProtocol, PopulationCoverage, PortIdSubtype, SubtypeObservation,
};
use crate::lldp_relationships::CaptureCoverageEvidence;
use crate::typed_facts::{
    Completeness, EvidenceRef, FactPayload, FactSource, FactSubject, Lifecycle, PayloadState,
    Quality, TrustClass, TypedFact,
};

pub const LLDP_PARSER_REVISION: &str = "lldp-structured-json-2026.11.26";
pub const LLDP_MAPPING_REVISION: &str = "bn024-lldp-map-1";
const MAX_NEIGHBORS_PER_INTERFACE: usize = 64;
const MAX_INVALID_SAMPLES: usize = 16;
const MAX_MGMT_ADDRS: usize = 16;
const MAX_CAPABILITIES: usize = 32;

/// Structured source profile. The enum names the input contract precisely;
/// it is not a generic NOS qualification claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LldpStructuredSource {
    /// Cumulus Linux lldpd `net show lldp json` structured output.
    CumulusLldpdJson,
    /// Arista EOS `show lldp neighbors detail` structured JSON object.
    AristaEosJson,
}

impl LldpStructuredSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::CumulusLldpdJson => "cumulus_lldpd_json",
            Self::AristaEosJson => "arista_eos_json",
        }
    }

    pub fn applicability(self) -> &'static str {
        match self {
            Self::CumulusLldpdJson => {
                "platform=cumulus version=all upstream-selector; genuine fixture release=4.3.0 only"
            }
            Self::AristaEosJson => {
                "platform=eos version=all upstream-selector; genuine fixture local release unknown"
            }
        }
    }

    pub fn produced_by(self) -> &'static str {
        match self {
            Self::CumulusLldpdJson => "native_cumulus_lldpd",
            Self::AristaEosJson => "native_arista_eos",
        }
    }

    pub fn source_kind(self) -> &'static str {
        "cli_structured_json"
    }

    pub fn command(self) -> &'static str {
        match self {
            Self::CumulusLldpdJson => "net show lldp json",
            Self::AristaEosJson => "show lldp neighbors detail",
        }
    }
}

/// Versioned input: one structured capture plus canonical context.
pub struct LldpObservation<'a> {
    pub source: LldpStructuredSource,
    pub capture: CommandCapture<'a>,
    pub context: ObservationContext<'a>,
}

/// Raw neighbor extraction retained for oracle comparison/diagnostics.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct LldpNeighborDiagnostic {
    pub local_interface: String,
    /// Independently established occurrence identity when the source contract
    /// supplies one; absent for generated array indices.
    pub source_neighbor_id: Option<String>,
    /// Capture-local record locator retained for diagnostics only.
    pub capture_record_locator: String,
    pub remote_chassis_id: Option<String>,
    pub remote_chassis_id_subtype_raw: Option<String>,
    pub remote_port_id: Option<String>,
    pub remote_port_id_subtype_raw: Option<String>,
    pub remote_system_name: Option<String>,
    pub remote_port_description: Option<String>,
    pub management_addresses: Vec<String>,
    pub ttl_seconds: Option<u32>,
    pub age_seconds: Option<u64>,
}

/// Honest coverage/diagnostics for one LLDP normalization run.
#[derive(Clone, Debug, Serialize)]
pub struct LldpNormalizationReport {
    pub source: &'static str,
    pub applicability: &'static str,
    pub interfaces_seen: usize,
    pub neighbors_decoded: usize,
    pub duplicate_records: usize,
    pub invalid_records: usize,
    pub empty_local_interfaces: usize,
    pub empty_local_interface_names: Vec<String>,
    pub covered_interfaces: Vec<String>,
    pub malformed_management_address_entries: usize,
    pub omitted_management_address_entries: usize,
    pub malformed_capability_entries: usize,
    pub omitted_capability_entries: usize,
    pub repeated_neighbor_records: usize,
    pub conflicting_neighbor_records: usize,
    pub facts_emitted: usize,
    pub facts_rejected: usize,
    pub output_facts_omitted: usize,
    pub capture: SourceCaptureReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_error: Option<String>,
    /// Bounded invalid-record explanations. Never trusted as the coverage
    /// contract; typed coverage is in the payload/report fields above.
    pub invalid_samples: Vec<String>,
    pub notes: Vec<String>,
}

pub struct LldpNormalizationOutput {
    pub facts: Vec<TypedFact>,
    pub neighbors: Vec<LldpNeighborDiagnostic>,
    pub report: LldpNormalizationReport,
    /// Coverage derived directly from the completed normalization outcome.
    /// Keeping this object outside per-neighbor facts preserves positively
    /// empty captures: an all-empty valid population emits no neighbor entity
    /// fact, but its scoped coverage evidence must not disappear.
    pub capture_coverage: Vec<CaptureCoverageEvidence>,
}

#[derive(Clone, Debug, Serialize)]
struct RawNeighbor {
    local_interface: String,
    /// Independently established source occurrence identifier. Neither
    /// qualified source currently provides one; generated locators are not
    /// promoted into this field.
    source_neighbor_id: Option<String>,
    capture_record_locator: String,
    chassis_id: Option<String>,
    chassis_id_raw: Option<String>,
    chassis_subtype: SubtypeObservation<ChassisIdSubtype>,
    port_id: Option<String>,
    port_id_raw: Option<String>,
    port_subtype: SubtypeObservation<PortIdSubtype>,
    system_name: Option<String>,
    port_description: Option<String>,
    management_addresses: Option<Vec<ManagementAddress>>,
    capabilities: Option<Vec<CapabilityObservation>>,
    ttl_seconds: Option<u32>,
    age_seconds: Option<u64>,
    temporal: Option<LldpTemporalEvidence>,
}

impl RawNeighbor {
    fn diagnostic(&self) -> LldpNeighborDiagnostic {
        LldpNeighborDiagnostic {
            local_interface: self.local_interface.clone(),
            source_neighbor_id: self.source_neighbor_id.clone(),
            capture_record_locator: self.capture_record_locator.clone(),
            remote_chassis_id: self.chassis_id.clone(),
            remote_chassis_id_subtype_raw: self.chassis_subtype.raw.clone(),
            remote_port_id: self.port_id.clone(),
            remote_port_id_subtype_raw: self.port_subtype.raw.clone(),
            remote_system_name: self.system_name.clone(),
            remote_port_description: self.port_description.clone(),
            management_addresses: self
                .management_addresses
                .as_ref()
                .map(|v| v.iter().map(|a| a.address.clone()).collect())
                .unwrap_or_default(),
            ttl_seconds: self.ttl_seconds,
            age_seconds: self.age_seconds,
        }
    }

    fn exact_duplicate_key(&self) -> String {
        // Exact duplicate detection uses the complete decoded observation
        // apart from capture-local locators. A generated array index is only a
        // position and cannot make two otherwise identical records distinct;
        // an independently established source occurrence identifier, when one
        // exists, remains part of source-record identity.
        let mut clone = self.clone();
        clone.capture_record_locator.clear();
        serde_json::to_string(&clone).expect("raw neighbor serializes")
    }

    fn neighbor_identity_key(&self) -> String {
        serde_json::json!([
            self.local_interface,
            self.chassis_subtype.value,
            self.chassis_id,
            self.port_subtype.value,
            self.port_id,
        ])
        .to_string()
    }

    fn non_temporal_evidence_key(&self) -> String {
        // Age/TTL and packet timing can legitimately change across repeated
        // observations. System identity, port evidence, management addresses,
        // capabilities and descriptions must not contradict within one batch.
        serde_json::json!([
            self.local_interface,
            self.chassis_subtype.value,
            self.chassis_id,
            self.port_subtype.value,
            self.port_id,
            self.system_name,
            self.port_description,
            self.management_addresses,
            self.capabilities,
        ])
        .to_string()
    }
}

#[derive(Default)]
struct ParseOutcome {
    neighbors: Vec<RawNeighbor>,
    invalid_records: usize,
    empty_interfaces: BTreeSet<String>,
    covered_interfaces: BTreeSet<String>,
    invalid_samples: Vec<String>,
    malformed_management_address_entries: usize,
    omitted_management_address_entries: usize,
    malformed_capability_entries: usize,
    omitted_capability_entries: usize,
}

impl ParseOutcome {
    fn note_invalid(&mut self, msg: String) {
        self.invalid_records += 1;
        if self.invalid_samples.len() < MAX_INVALID_SAMPLES {
            self.invalid_samples.push(msg);
        }
    }
}

fn capture_coverage(outcome: CaptureOutcome) -> LldpCaptureOutcome {
    match outcome {
        CaptureOutcome::Complete => LldpCaptureOutcome::Complete,
        CaptureOutcome::Partial => LldpCaptureOutcome::Partial,
        CaptureOutcome::Unavailable => LldpCaptureOutcome::Unavailable,
        CaptureOutcome::Failed => LldpCaptureOutcome::Failed,
        CaptureOutcome::TimedOut => LldpCaptureOutcome::TimedOut,
        CaptureOutcome::Truncated => LldpCaptureOutcome::Truncated,
        CaptureOutcome::Unsupported => LldpCaptureOutcome::Unsupported,
        CaptureOutcome::Unknown => LldpCaptureOutcome::Unknown,
    }
}

fn normalization_capture_coverage(
    obs: &LldpObservation,
    population: PopulationCoverage,
    empty_interface_names: &BTreeSet<String>,
    covered_interface_names: &BTreeSet<String>,
    evidence_refs: Vec<EvidenceRef>,
) -> CaptureCoverageEvidence {
    let covered_interfaces = covered_interface_names
        .iter()
        .chain(empty_interface_names.iter())
        .cloned()
        .collect();
    CaptureCoverageEvidence {
        device_id: obs.context.device_id.to_string(),
        scope: obs.context.scope.map(str::to_string),
        network_namespace: obs.context.network_namespace.map(str::to_string),
        collector_id: Some(obs.context.collector_id.to_string()),
        source_kind: Some(obs.source.source_kind().to_string()),
        capture_outcome: capture_coverage(obs.capture.outcome),
        population_coverage: population,
        capture_id: Some(obs.context.capture_id.to_string()),
        source_generation: Some(obs.context.session_epoch),
        acquisition_scope: Some(format!(
            "device={} scope={} network_namespace={} source={}",
            obs.context.device_id,
            obs.context.scope.unwrap_or("<default>"),
            obs.context.network_namespace.unwrap_or("<unspecified>"),
            obs.source.label()
        )),
        observation_time: Some(obs.context.observed_at.to_string()),
        // LLDP capture coverage has no independent source-established expiry.
        // Consumers must apply explicit freshness policy rather than treating
        // caller-supplied validity as evidence invented by normalization.
        valid_until: None,
        covered_interfaces,
        evidence_refs,
    }
}

fn observation_context_error(obs: &LldpObservation) -> Option<String> {
    for (name, v) in [
        ("device_id", obs.context.device_id),
        ("collector_id", obs.context.collector_id),
        ("capture_id", obs.context.capture_id),
    ] {
        if v.trim().is_empty() {
            return Some(format!("observation context '{name}' is empty"));
        }
    }
    if parse_rfc3339(obs.context.observed_at).is_none() {
        return Some("observation context 'observed_at' is not RFC-3339".to_string());
    }
    None
}

fn get_array<'a>(obj: &'a Value, key: &str) -> Option<&'a Vec<Value>> {
    obj.get(key).and_then(Value::as_array)
}

fn wrapped_values(obj: Option<&Value>, key: &str) -> Vec<String> {
    let Some(obj) = obj else {
        return vec![];
    };
    get_array(obj, key)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("value").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn first_wrapped_value(obj: Option<&Value>, key: &str) -> Option<String> {
    wrapped_values(obj, key).into_iter().next()
}

fn sanitize_scalar(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 512 || trimmed.chars().any(|c| c.is_control()) {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn normalize_identifier_by_subtype(
    raw: Option<String>,
    is_mac_subtype: bool,
) -> (Option<String>, Option<String>, bool) {
    let Some(raw_value) = raw.and_then(|v| sanitize_scalar(&v)) else {
        return (None, None, false);
    };
    if is_mac_subtype {
        if let Some(normalized) = normalize_mac(&raw_value) {
            let raw_preserved = (normalized != raw_value).then_some(raw_value);
            return (Some(normalized), raw_preserved, true);
        }
        return (None, Some(raw_value), false);
    }
    (Some(raw_value.clone()), None, true)
}

fn map_cumulus_port_subtype(raw: Option<String>) -> SubtypeObservation<PortIdSubtype> {
    let Some(token) = raw.and_then(|v| sanitize_scalar(&v)) else {
        return SubtypeObservation::unavailable(
            FieldAvailability::Unknown,
            None,
            Some("port id subtype absent".into()),
        );
    };
    let mapped = match token.as_str() {
        "ifname" => Some(PortIdSubtype::InterfaceName),
        "mac" => Some(PortIdSubtype::MacAddress),
        "local" => Some(PortIdSubtype::LocallyAssigned),
        "ip" => Some(PortIdSubtype::NetworkAddress),
        _ => None,
    };
    if let Some(value) = mapped {
        SubtypeObservation::known(value, Some(token))
    } else {
        SubtypeObservation::known(PortIdSubtype::Unknown, Some(token))
    }
}

fn map_cumulus_chassis_subtype(raw: Option<String>) -> SubtypeObservation<ChassisIdSubtype> {
    let Some(token) = raw.and_then(|v| sanitize_scalar(&v)) else {
        return SubtypeObservation::unavailable(
            FieldAvailability::Unknown,
            None,
            Some("chassis id subtype absent".into()),
        );
    };
    let mapped = match token.as_str() {
        "mac" => Some(ChassisIdSubtype::MacAddress),
        "ip" => Some(ChassisIdSubtype::NetworkAddress),
        "ifname" => Some(ChassisIdSubtype::InterfaceName),
        "local" => Some(ChassisIdSubtype::LocallyAssigned),
        _ => None,
    };
    if let Some(value) = mapped {
        SubtypeObservation::known(value, Some(token))
    } else {
        SubtypeObservation::known(ChassisIdSubtype::Unknown, Some(token))
    }
}

fn map_eos_port_subtype(raw: Option<String>) -> SubtypeObservation<PortIdSubtype> {
    let Some(token) = raw.and_then(|v| sanitize_scalar(&v)) else {
        return SubtypeObservation::unavailable(
            FieldAvailability::Unknown,
            None,
            Some("interfaceIdType absent".into()),
        );
    };
    let mapped = match token.as_str() {
        "interfaceName" => Some(PortIdSubtype::InterfaceName),
        "macAddress" => Some(PortIdSubtype::MacAddress),
        "local" => Some(PortIdSubtype::LocallyAssigned),
        "networkAddress" => Some(PortIdSubtype::NetworkAddress),
        "interfaceAlias" => Some(PortIdSubtype::InterfaceAlias),
        _ => None,
    };
    if let Some(value) = mapped {
        SubtypeObservation::known(value, Some(token))
    } else {
        SubtypeObservation::known(PortIdSubtype::Unknown, Some(token))
    }
}

fn map_eos_chassis_subtype(raw: Option<String>) -> SubtypeObservation<ChassisIdSubtype> {
    let Some(token) = raw.and_then(|v| sanitize_scalar(&v)) else {
        return SubtypeObservation::unavailable(
            FieldAvailability::Unknown,
            None,
            Some("chassisIdType absent".into()),
        );
    };
    let mapped = match token.as_str() {
        "macAddress" => Some(ChassisIdSubtype::MacAddress),
        "networkAddress" => Some(ChassisIdSubtype::NetworkAddress),
        "interfaceName" => Some(ChassisIdSubtype::InterfaceName),
        "interfaceAlias" => Some(ChassisIdSubtype::InterfaceAlias),
        "locallyAssigned" | "local" => Some(ChassisIdSubtype::LocallyAssigned),
        "chassisComponent" => Some(ChassisIdSubtype::ChassisComponent),
        _ => None,
    };
    if let Some(value) = mapped {
        SubtypeObservation::known(value, Some(token))
    } else {
        SubtypeObservation::known(ChassisIdSubtype::Unknown, Some(token))
    }
}

fn parse_age_seconds(raw: Option<String>) -> Option<u64> {
    // lldpd form: "0 day, 01:31:34" (plural day/days accepted). This is a
    // direct source encoding, not an estimate from wall-clock time.
    let raw = raw?;
    let (days_part, hms) = raw.split_once(',').map(|(d, h)| (d.trim(), h.trim()))?;
    let days = days_part.split_whitespace().next()?.parse::<u64>().ok()?;
    let mut parts = hms.split(':');
    let hours = parts.next()?.parse::<u64>().ok()?;
    let minutes = parts.next()?.parse::<u64>().ok()?;
    let seconds = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() || hours > 23 || minutes > 59 || seconds > 59 {
        return None;
    }
    days.checked_mul(86_400)?
        .checked_add(hours.checked_mul(3_600)?)?
        .checked_add(minutes.checked_mul(60)?)?
        .checked_add(seconds)
}

fn parse_u32_value(raw: Option<String>) -> Option<u32> {
    // TTL zero is a real LLDP value (immediate expiry/withdrawal semantics in
    // the LLDP TLV contract). It must not be erased into an absent field.
    raw.and_then(|v| v.parse::<u32>().ok())
}

fn strip_eos_quoted_interface_id(raw: &str) -> String {
    // EOS structured JSON historically emits interfaceId as a quoted token
    // (for example "\"Ethernet2\"") even though the value is the interface
    // name. This is a serialization artifact of this source shape, not an
    // interface-name inference. Only one exact pair of surrounding quotes is
    // removed; embedded whitespace/control content remains invalid evidence.
    let trimmed = raw.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

fn single_array_object<'a>(obj: &'a Value, key: &str) -> Result<Option<&'a Value>, &'static str> {
    let Some(value) = obj.get(key) else {
        return Ok(None);
    };
    let Some(items) = value.as_array() else {
        return Err("container is not an array");
    };
    match items.len() {
        0 => Err("container array is empty"),
        1 => items[0]
            .as_object()
            .map(|_| Some(&items[0]))
            .ok_or("container entry is not an object"),
        _ => Err("container cardinality is greater than one"),
    }
}

fn unknown_time(detail: &str) -> ObservedField<String> {
    ObservedField::unavailable(FieldAvailability::Unknown, Some(detail.to_string()))
}

fn time_field(value: Option<String>, missing_detail: &str) -> ObservedField<String> {
    match value {
        Some(v) => ObservedField::known(v),
        None => unknown_time(missing_detail),
    }
}

fn epoch_seconds_to_rfc3339(value: f64) -> Option<String> {
    if !value.is_finite() || value < 0.0 || value > 4_102_444_800.0 {
        return None;
    }
    let secs = value.trunc() as i64;
    let nanos64 = ((value - value.trunc()) * 1_000_000_000.0).round() as i64;
    let (secs, nanos) = if nanos64 >= 1_000_000_000 {
        (secs.checked_add(1)?, 0u32)
    } else {
        (secs, nanos64 as u32)
    };
    chrono::DateTime::from_timestamp(secs, nanos)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
}

fn eos_epoch_field(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_f64)
        .and_then(epoch_seconds_to_rfc3339)
}

fn parse_cumulus_mgmt(
    chassis: &Value,
    local: &str,
    outcome: &mut ParseOutcome,
) -> Option<Vec<ManagementAddress>> {
    let value = chassis.get("mgmt-ip")?;
    let Some(items) = value.as_array() else {
        outcome.malformed_management_address_entries += 1;
        outcome.note_invalid(format!("{local}: mgmt-ip collection is not an array"));
        return None;
    };
    let mut outv = Vec::new();
    for item in items.iter().take(MAX_MGMT_ADDRS) {
        let address = item.get("value").and_then(Value::as_str);
        let Some(address) = address.and_then(sanitize_scalar) else {
            outcome.malformed_management_address_entries += 1;
            continue;
        };
        let address_type = if address.parse::<std::net::Ipv4Addr>().is_ok() {
            ManagementAddressType::Ipv4
        } else if address.parse::<std::net::Ipv6Addr>().is_ok() {
            ManagementAddressType::Ipv6
        } else {
            outcome.malformed_management_address_entries += 1;
            continue;
        };
        outv.push(ManagementAddress {
            address,
            address_type,
            interface_num_type: None,
            interface_num: None,
        });
    }
    if items.len() > MAX_MGMT_ADDRS {
        outcome.omitted_management_address_entries += items.len() - MAX_MGMT_ADDRS;
    }
    Some(outv)
}

fn parse_cumulus_capabilities(
    chassis: &Value,
    local: &str,
    outcome: &mut ParseOutcome,
) -> Option<Vec<CapabilityObservation>> {
    let value = chassis.get("capability")?;
    let Some(items) = value.as_array() else {
        outcome.malformed_capability_entries += 1;
        outcome.note_invalid(format!("{local}: capability collection is not an array"));
        return None;
    };
    let mut outv = Vec::new();
    for item in items.iter().take(MAX_CAPABILITIES) {
        let name = item
            .get("type")
            .and_then(Value::as_str)
            .and_then(sanitize_scalar);
        let enabled = item.get("enabled").and_then(Value::as_bool);
        let (Some(name), Some(enabled)) = (name, enabled) else {
            outcome.malformed_capability_entries += 1;
            continue;
        };
        outv.push(CapabilityObservation {
            name,
            advertised: None,
            enabled: Some(enabled),
        });
    }
    if items.len() > MAX_CAPABILITIES {
        outcome.omitted_capability_entries += items.len() - MAX_CAPABILITIES;
    }
    Some(outv)
}

fn parse_cumulus(root: &Value, limits: NormalizerLimits) -> ParseOutcome {
    let mut outcome = ParseOutcome::default();
    let mut record_index = 0usize;

    let Some(lldp) = get_array(root, "lldp") else {
        outcome.note_invalid("root lldp array absent or wrong type".into());
        return outcome;
    };
    for container in lldp {
        let Some(interfaces) = get_array(container, "interface") else {
            if container.as_object().is_some_and(|o| !o.is_empty()) {
                outcome.note_invalid("lldp container lacks interface array".into());
            }
            continue;
        };
        for interface in interfaces {
            let Some(local) = interface
                .get("name")
                .and_then(Value::as_str)
                .and_then(sanitize_scalar)
                .filter(|v| valid_name(v))
            else {
                outcome.note_invalid("interface record missing valid name".into());
                continue;
            };
            match interface.get("via").and_then(Value::as_str) {
                Some("LLDP") => {}
                other => {
                    outcome.note_invalid(format!(
                        "{local}: unsupported or absent discovery protocol evidence {other:?}"
                    ));
                    continue;
                }
            }
            let chassis_result = single_array_object(interface, "chassis");
            let port_result = single_array_object(interface, "port");
            let chassis_absent = matches!(chassis_result, Ok(None));
            let port_absent = matches!(port_result, Ok(None));
            if chassis_absent && port_absent {
                // In lldpd JSON a genuinely empty interface has no neighbor
                // containers. This positive empty evidence is valid only after
                // the interface's LLDP protocol provenance has been established;
                // present-but-malformed containers below are not rewritten into
                // positive empty evidence.
                outcome.empty_interfaces.insert(local.clone());
                outcome.covered_interfaces.insert(local.clone());
                continue;
            }
            let (Ok(chassis_opt), Ok(port_opt)) = (chassis_result, port_result) else {
                outcome.note_invalid(format!(
                    "{local}: malformed chassis/port container cardinality or type"
                ));
                continue;
            };
            let (Some(chassis), Some(port)) = (chassis_opt, port_opt) else {
                outcome.note_invalid(format!(
                    "{local}: neighbor record missing chassis or port container"
                ));
                continue;
            };
            if outcome.neighbors.len() >= limits.max_entities {
                outcome.note_invalid(format!("neighbor on {local} omitted at entity bound"));
                continue;
            }
            record_index += 1;
            let locator = interface
                .get("rid")
                .and_then(Value::as_str)
                .and_then(sanitize_scalar)
                .unwrap_or_else(|| format!("record-{record_index}"));
            let chassis_id_obj = match single_array_object(chassis, "id") {
                Ok(Some(v)) => v,
                _ => {
                    outcome.note_invalid(format!("{local}: chassis id container malformed"));
                    continue;
                }
            };
            let port_id_obj = match single_array_object(port, "id") {
                Ok(Some(v)) => v,
                _ => {
                    outcome.note_invalid(format!("{local}: port id container malformed"));
                    continue;
                }
            };
            let chassis_subtype_raw = chassis_id_obj
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string);
            let chassis_subtype = map_cumulus_chassis_subtype(chassis_subtype_raw);
            let chassis_raw = chassis_id_obj
                .get("value")
                .and_then(Value::as_str)
                .map(str::to_string);
            let (chassis_id, chassis_id_raw, _) = normalize_identifier_by_subtype(
                chassis_raw,
                chassis_subtype.value == Some(ChassisIdSubtype::MacAddress),
            );
            let port_subtype_raw = port_id_obj
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string);
            let port_subtype = map_cumulus_port_subtype(port_subtype_raw);
            let port_raw = port_id_obj
                .get("value")
                .and_then(Value::as_str)
                .map(str::to_string);
            let (port_id, port_id_raw, _) = normalize_identifier_by_subtype(
                port_raw,
                port_subtype.value == Some(PortIdSubtype::MacAddress),
            );
            outcome.covered_interfaces.insert(local.clone());
            let management_addresses = parse_cumulus_mgmt(chassis, &local, &mut outcome);
            let capabilities = parse_cumulus_capabilities(chassis, &local, &mut outcome);
            outcome.neighbors.push(RawNeighbor {
                local_interface: local.clone(),
                source_neighbor_id: None,
                capture_record_locator: locator,
                chassis_id,
                chassis_id_raw,
                chassis_subtype,
                port_id,
                port_id_raw,
                port_subtype,
                system_name: first_wrapped_value(Some(chassis), "name")
                    .and_then(|v| sanitize_scalar(&v)),
                port_description: first_wrapped_value(Some(port), "descr")
                    .and_then(|v| sanitize_scalar(&v)),
                management_addresses,
                capabilities,
                ttl_seconds: parse_u32_value(first_wrapped_value(Some(port), "ttl")),
                age_seconds: parse_age_seconds(
                    interface
                        .get("age")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                ),
                temporal: Some(LldpTemporalEvidence {
                    capture_at: unknown_time("capture completion time not supplied"),
                    last_contact_at: unknown_time(
                        "Cumulus lldpd JSON does not expose exact last-contact time",
                    ),
                    last_change_at: unknown_time(
                        "Cumulus lldpd JSON does not expose exact last-change time",
                    ),
                    neighbor_discovery_at: unknown_time(
                        "Cumulus lldpd JSON exposes relationship age, not discovery timestamp",
                    ),
                    validity_basis: LldpValidityBasis::Unknown,
                }),
            });
        }
    }
    outcome
}

fn parse_eos_mgmt(
    nbr: &Value,
    local: &str,
    outcome: &mut ParseOutcome,
) -> Option<Vec<ManagementAddress>> {
    let value = nbr.get("managementAddresses")?;
    let Some(items) = value.as_array() else {
        outcome.malformed_management_address_entries += 1;
        outcome.note_invalid(format!("{local}: managementAddresses is not an array"));
        return None;
    };
    let mut outv = Vec::new();
    for item in items.iter().take(MAX_MGMT_ADDRS) {
        let Some(obj) = item.as_object() else {
            outcome.malformed_management_address_entries += 1;
            continue;
        };
        let address = obj
            .get("address")
            .and_then(Value::as_str)
            .and_then(sanitize_scalar);
        let Some(address) = address else {
            outcome.malformed_management_address_entries += 1;
            continue;
        };
        let type_token = obj.get("addressType").and_then(Value::as_str);
        let address_type = match type_token {
            Some("ipv4") if address.parse::<std::net::Ipv4Addr>().is_ok() => {
                ManagementAddressType::Ipv4
            }
            Some("ipv6") if address.parse::<std::net::Ipv6Addr>().is_ok() => {
                ManagementAddressType::Ipv6
            }
            Some("ipv4") | Some("ipv6") => {
                outcome.malformed_management_address_entries += 1;
                continue;
            }
            _ => ManagementAddressType::Unknown,
        };
        let interface_num = match obj.get("interfaceNum") {
            None => None,
            Some(v) => v.as_u64(),
        };
        if obj.get("interfaceNum").is_some() && interface_num.is_none() {
            outcome.malformed_management_address_entries += 1;
            continue;
        }
        outv.push(ManagementAddress {
            address,
            address_type,
            interface_num_type: obj
                .get("interfaceNumType")
                .and_then(Value::as_str)
                .and_then(sanitize_scalar),
            interface_num,
        });
    }
    if items.len() > MAX_MGMT_ADDRS {
        outcome.omitted_management_address_entries += items.len() - MAX_MGMT_ADDRS;
    }
    Some(outv)
}

fn parse_eos_capabilities(
    nbr: &Value,
    local: &str,
    outcome: &mut ParseOutcome,
) -> Option<Vec<CapabilityObservation>> {
    let value = nbr.get("systemCapabilities")?;
    let Some(items) = value.as_object() else {
        outcome.malformed_capability_entries += 1;
        outcome.note_invalid(format!("{local}: systemCapabilities is not an object"));
        return None;
    };
    let mut outv = Vec::new();
    for (name, enabled) in items.iter().take(MAX_CAPABILITIES) {
        let (Ok(name), Some(enabled)) = (sanitize_scalar(name).ok_or(()), enabled.as_bool()) else {
            outcome.malformed_capability_entries += 1;
            continue;
        };
        outv.push(CapabilityObservation {
            name,
            advertised: None,
            enabled: Some(enabled),
        });
    }
    if items.len() > MAX_CAPABILITIES {
        outcome.omitted_capability_entries += items.len() - MAX_CAPABILITIES;
    }
    Some(outv)
}

fn eos_temporal(nbr: &Value, ttl: Option<u32>) -> LldpTemporalEvidence {
    let last_contact = eos_epoch_field(nbr.get("lastContactTime"));
    let last_change = eos_epoch_field(nbr.get("lastChangeTime"));
    let discovery = eos_epoch_field(nbr.get("neighborDiscoveryTime"));
    let validity_basis = if last_contact.is_some() && ttl.is_some() {
        LldpValidityBasis::LastContactPlusAdvertisedTtl
    } else {
        LldpValidityBasis::Unknown
    };
    LldpTemporalEvidence {
        capture_at: unknown_time("capture completion time not supplied"),
        last_contact_at: time_field(last_contact, "EOS lastContactTime absent or malformed"),
        last_change_at: time_field(last_change, "EOS lastChangeTime absent or malformed"),
        neighbor_discovery_at: time_field(
            discovery,
            "EOS neighborDiscoveryTime absent or malformed",
        ),
        validity_basis,
    }
}

fn parse_eos(root: &Value, limits: NormalizerLimits) -> ParseOutcome {
    let mut outcome = ParseOutcome::default();
    let Some(neighbors_obj) = root.get("lldpNeighbors").and_then(Value::as_object) else {
        outcome.note_invalid("root lldpNeighbors object absent or wrong type".into());
        return outcome;
    };
    for (local_raw, entry) in neighbors_obj {
        let Some(local) = sanitize_scalar(local_raw).filter(|v| valid_name(v)) else {
            outcome.note_invalid("lldpNeighbors key is not a valid interface name".into());
            continue;
        };
        let Some(info) = entry.get("lldpNeighborInfo").and_then(Value::as_array) else {
            outcome.note_invalid(format!("{local}: lldpNeighborInfo absent or wrong type"));
            continue;
        };
        outcome.covered_interfaces.insert(local.clone());
        if info.is_empty() {
            outcome.empty_interfaces.insert(local.clone());
            continue;
        }
        for (idx, nbr) in info.iter().enumerate().take(MAX_NEIGHBORS_PER_INTERFACE) {
            if outcome.neighbors.len() >= limits.max_entities {
                outcome.note_invalid(format!("{local}: neighbor omitted at entity bound"));
                continue;
            }
            if !nbr.is_object() {
                outcome.note_invalid(format!("{local}: neighbor entry {idx} is not an object"));
                continue;
            }
            let ifinfo = nbr.get("neighborInterfaceInfo").unwrap_or(&Value::Null);
            let chassis_subtype_raw = nbr
                .get("chassisIdType")
                .and_then(Value::as_str)
                .map(str::to_string);
            let chassis_subtype = map_eos_chassis_subtype(chassis_subtype_raw);
            let chassis_raw = nbr
                .get("chassisId")
                .and_then(Value::as_str)
                .map(str::to_string);
            let (chassis_id, chassis_id_raw, _) = normalize_identifier_by_subtype(
                chassis_raw,
                chassis_subtype.value == Some(ChassisIdSubtype::MacAddress),
            );
            let port_subtype_raw = ifinfo
                .get("interfaceIdType")
                .and_then(Value::as_str)
                .map(str::to_string);
            let port_subtype = map_eos_port_subtype(port_subtype_raw);
            let port_raw = ifinfo
                .get("interfaceId")
                .and_then(Value::as_str)
                .map(strip_eos_quoted_interface_id);
            let (port_id, port_id_raw, _) = normalize_identifier_by_subtype(
                port_raw,
                port_subtype.value == Some(PortIdSubtype::MacAddress),
            );
            let ttl = nbr
                .get("ttl")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok());
            let management_addresses = parse_eos_mgmt(nbr, &local, &mut outcome);
            let capabilities = parse_eos_capabilities(nbr, &local, &mut outcome);
            outcome.neighbors.push(RawNeighbor {
                local_interface: local.clone(),
                source_neighbor_id: None,
                capture_record_locator: format!("{local}#{idx}"),
                chassis_id,
                chassis_id_raw,
                chassis_subtype,
                port_id,
                port_id_raw,
                port_subtype,
                system_name: nbr
                    .get("systemName")
                    .and_then(Value::as_str)
                    .and_then(sanitize_scalar),
                port_description: ifinfo
                    .get("interfaceDescription")
                    .and_then(Value::as_str)
                    .and_then(sanitize_scalar),
                management_addresses,
                capabilities,
                ttl_seconds: ttl,
                age_seconds: None,
                temporal: Some(eos_temporal(nbr, ttl)),
            });
        }
        if info.len() > MAX_NEIGHBORS_PER_INTERFACE {
            outcome.invalid_records += info.len() - MAX_NEIGHBORS_PER_INTERFACE;
            if outcome.invalid_samples.len() < MAX_INVALID_SAMPLES {
                outcome.invalid_samples.push(format!(
                    "{local}: {} neighbors omitted at per-interface bound",
                    info.len() - MAX_NEIGHBORS_PER_INTERFACE
                ));
            }
        }
    }
    outcome
}

fn observed<T>(value: Option<T>) -> ObservedField<T> {
    match value {
        Some(value) => ObservedField::known(value),
        None => ObservedField::unavailable(
            FieldAvailability::Unknown,
            Some("field absent or malformed in source observation".into()),
        ),
    }
}

fn missing_field(name: &'static str, field: &ObservedField<impl Sized>, missing: &mut Vec<String>) {
    if field.state != FieldAvailability::Known {
        missing.push(name.to_string());
    }
}

fn status_fact(
    obs: &LldpObservation,
    state: PayloadState,
    completeness: Completeness,
    note: &str,
    evidence: Vec<EvidenceRef>,
) -> TypedFact {
    let mut fact = TypedFact {
        schema_version: crate::typed_facts::SCHEMA_VERSION,
        fact_id: String::new(),
        fact_kind: FACT_KIND_LLDP_NEIGHBOR.to_string(),
        subject: FactSubject {
            device_id: obs.context.device_id.to_string(),
            scope: obs.context.scope.map(str::to_string),
            namespace: "interface".to_string(),
            network_namespace: obs.context.network_namespace.map(str::to_string),
            entity: None,
            vrf: None,
            afi_safi: None,
            plane: Some("operational".to_string()),
            interface_identity: None,
        },
        source: FactSource {
            collector_id: obs.context.collector_id.to_string(),
            source_kind: obs.source.source_kind().to_string(),
            capture_id: obs.context.capture_id.to_string(),
            session_epoch: obs.context.session_epoch,
            sequence: obs.context.sequence,
            exporter_id: None,
        },
        observed_at: obs.context.observed_at.to_string(),
        ingested_at: None,
        valid_from: None,
        valid_until: None,
        payload: FactPayload {
            produced_by: obs.source.produced_by().to_string(),
            parser_revision: LLDP_PARSER_REVISION.to_string(),
            mapping_revision: Some(LLDP_MAPPING_REVISION.to_string()),
            fields: None,
            state: Some(state),
        },
        payload_sha256: String::new(),
        evidence_sha256: String::new(),
        quality: Quality {
            completeness,
            missing_fields: vec![],
            notes: vec![note.to_string()],
        },
        trust: TrustClass::DeviceAsserted,
        authority: None,
        lifecycle: Lifecycle::Active,
        withdrawn_reason: None,
        superseded_by: None,
        lifecycle_changed_at: None,
        evidence,
    };
    fact.seal();
    fact
}

fn source_validity_window(raw: &RawNeighbor) -> (Option<String>, Option<String>) {
    let Some(temporal) = &raw.temporal else {
        return (None, None);
    };
    if temporal.validity_basis != LldpValidityBasis::LastContactPlusAdvertisedTtl {
        return (None, None);
    }
    let Some(last_contact) = temporal
        .last_contact_at
        .value
        .as_deref()
        .and_then(parse_rfc3339)
    else {
        return (None, None);
    };
    let Some(ttl) = raw.ttl_seconds else {
        return (None, None);
    };
    let until = last_contact.checked_add_signed(chrono::Duration::seconds(i64::from(ttl)));
    (
        Some(last_contact.to_rfc3339()),
        until.map(|u| u.to_rfc3339()),
    )
}

fn fact_for_neighbor(
    obs: &LldpObservation,
    raw: &RawNeighbor,
    population: PopulationCoverage,
    empty_interface_names: &BTreeSet<String>,
    covered_interface_names: &BTreeSet<String>,
    evidence: Vec<EvidenceRef>,
    degraded: bool,
) -> TypedFact {
    let mut missing_fields = Vec::new();
    let chassis = observed(raw.chassis_id.clone());
    let port = observed(raw.port_id.clone());
    let system_name = observed(raw.system_name.clone());
    let description = observed(raw.port_description.clone());
    let mgmt = observed(raw.management_addresses.clone());
    let caps = observed(raw.capabilities.clone());
    let ttl = observed(raw.ttl_seconds);
    let age = observed(raw.age_seconds);
    missing_field("remote_chassis_id", &chassis, &mut missing_fields);
    missing_field("remote_port_id", &port, &mut missing_fields);
    missing_field("remote_system_name", &system_name, &mut missing_fields);
    missing_field("remote_port_description", &description, &mut missing_fields);
    missing_field("remote_management_addresses", &mgmt, &mut missing_fields);
    missing_field("capabilities", &caps, &mut missing_fields);
    missing_field("ttl_seconds", &ttl, &mut missing_fields);
    missing_field("age_seconds", &age, &mut missing_fields);

    let completeness = if obs.capture.outcome == CaptureOutcome::Unknown {
        Completeness::Unknown
    } else if degraded
        || obs.capture.outcome.degrades()
        || population != PopulationCoverage::Complete
    {
        Completeness::Partial
    } else {
        Completeness::Complete
    };
    let payload = LldpNeighborPayload {
        local_interface_name: raw.local_interface.clone(),
        normalized_local_interface_name: None,
        source_neighbor_id: raw.source_neighbor_id.clone(),
        // Generated array positions remain diagnostics only. Embedding them in
        // typed fields would make fact identity depend on source ordering.
        capture_record_locator: None,
        protocol: NeighborProtocol::Lldp,
        remote_chassis_id: chassis,
        remote_chassis_id_raw: raw.chassis_id_raw.clone(),
        remote_chassis_id_subtype: raw.chassis_subtype.clone(),
        remote_port_id: port,
        remote_port_id_raw: raw.port_id_raw.clone(),
        remote_port_id_subtype: raw.port_subtype.clone(),
        remote_system_name: system_name,
        remote_port_description: description,
        remote_management_addresses: mgmt,
        capabilities: caps,
        ttl_seconds: ttl.clone(),
        age_seconds: age,
        temporal: raw.temporal.clone().map(|mut temporal| {
            temporal.capture_at = match obs.capture.captured_at.and_then(parse_rfc3339) {
                Some(_) => {
                    ObservedField::known(obs.capture.captured_at.unwrap_or_default().to_string())
                }
                None => unknown_time("capture completion time absent or not RFC-3339"),
            };
            temporal
        }),
        coverage: LldpCoverage {
            capture_outcome: capture_coverage(obs.capture.outcome),
            population_coverage: population,
            empty_local_interfaces: empty_interface_names.len() as u32,
            empty_local_interface_names: empty_interface_names.iter().cloned().collect(),
            capture_id: Some(obs.context.capture_id.to_string()),
            source_generation: Some(obs.context.session_epoch),
            observation_time: Some(obs.context.observed_at.to_string()),
            acquisition_scope: Some(format!(
                "device={} scope={} network_namespace={} source={}",
                obs.context.device_id,
                obs.context.scope.unwrap_or("<default>"),
                obs.context.network_namespace.unwrap_or("<unspecified>"),
                obs.source.label()
            )),
            covered_interfaces: covered_interface_names.iter().cloned().collect(),
            evidence_references: evidence.clone(),
        },
    };
    let fields = serde_json::to_value(&payload).expect("typed LLDP payload serializes");
    let (valid_from, valid_until) = source_validity_window(raw);
    let mut fact = TypedFact {
        schema_version: crate::typed_facts::SCHEMA_VERSION,
        fact_id: String::new(),
        fact_kind: FACT_KIND_LLDP_NEIGHBOR.to_string(),
        subject: FactSubject {
            device_id: obs.context.device_id.to_string(),
            scope: obs.context.scope.map(str::to_string),
            namespace: "interface".to_string(),
            network_namespace: obs.context.network_namespace.map(str::to_string),
            entity: Some(raw.local_interface.clone()),
            vrf: None,
            afi_safi: None,
            plane: Some("operational".to_string()),
            interface_identity: None,
        },
        source: FactSource {
            collector_id: obs.context.collector_id.to_string(),
            source_kind: obs.source.source_kind().to_string(),
            capture_id: obs.context.capture_id.to_string(),
            session_epoch: obs.context.session_epoch,
            sequence: obs.context.sequence,
            exporter_id: None,
        },
        observed_at: obs.context.observed_at.to_string(),
        ingested_at: None,
        valid_from,
        valid_until,
        payload: FactPayload {
            produced_by: obs.source.produced_by().to_string(),
            parser_revision: LLDP_PARSER_REVISION.to_string(),
            mapping_revision: Some(LLDP_MAPPING_REVISION.to_string()),
            fields: Some(fields),
            state: None,
        },
        payload_sha256: String::new(),
        evidence_sha256: String::new(),
        quality: Quality {
            completeness,
            missing_fields,
            notes: vec![],
        },
        trust: TrustClass::DeviceAsserted,
        authority: None,
        lifecycle: Lifecycle::Active,
        withdrawn_reason: None,
        superseded_by: None,
        lifecycle_changed_at: None,
        evidence,
    };
    fact.seal();
    fact
}

/// Normalize one structured LLDP observation with implementation-default
/// bounds. Input refusal happens before parsing; malformed input can never
/// panic or allocate proportionally to hostile structure beyond the input cap.
pub fn normalize_lldp_neighbors(obs: &LldpObservation) -> LldpNormalizationOutput {
    normalize_lldp_neighbors_with_limits(obs, NormalizerLimits::default())
}

pub fn normalize_lldp_neighbors_with_limits(
    obs: &LldpObservation,
    limits: NormalizerLimits,
) -> LldpNormalizationOutput {
    let context_err = observation_context_error(obs);
    let oversized = obs.capture.output.len() > limits.max_input_bytes;
    let refused = context_err.is_some() || oversized;
    let parsed = obs.capture.outcome.usable() && !refused;
    let capture = SourceCaptureReport {
        source: match obs.source {
            LldpStructuredSource::CumulusLldpdJson => "net_show_lldp_json",
            LldpStructuredSource::AristaEosJson => "show_lldp_neighbors_detail",
        },
        outcome: obs.capture.outcome,
        parsed,
        rejected: refused,
        bytes: obs.capture.output.len(),
        captured_at: obs.capture.captured_at.map(str::to_string),
    };
    let base_report = |facts_emitted, context_error, notes| LldpNormalizationReport {
        source: obs.source.label(),
        applicability: obs.source.applicability(),
        interfaces_seen: 0,
        neighbors_decoded: 0,
        duplicate_records: 0,
        invalid_records: 0,
        empty_local_interfaces: 0,
        empty_local_interface_names: vec![],
        covered_interfaces: vec![],
        malformed_management_address_entries: 0,
        omitted_management_address_entries: 0,
        malformed_capability_entries: 0,
        omitted_capability_entries: 0,
        repeated_neighbor_records: 0,
        conflicting_neighbor_records: 0,
        facts_emitted,
        facts_rejected: 0,
        output_facts_omitted: 0,
        capture: capture.clone(),
        context_error,
        invalid_samples: vec![],
        notes,
    };
    if let Some(err) = context_err {
        return LldpNormalizationOutput {
            facts: vec![],
            neighbors: vec![],
            capture_coverage: vec![],
            report: base_report(
                0,
                Some(err.clone()),
                vec![format!(
                    "observation context invalid: {err}; nothing emitted"
                )],
            ),
        };
    }
    if oversized {
        let mut facts = vec![];
        let status = status_fact(
            obs,
            PayloadState::InvalidInput,
            Completeness::Unknown,
            "LLDP observation exceeded input byte bound; refused",
            vec![],
        );
        if status.validate().is_ok() {
            facts.push(status);
        }
        let emitted = facts.len();
        return LldpNormalizationOutput {
            facts,
            neighbors: vec![],
            capture_coverage: vec![],
            report: base_report(
                emitted,
                None,
                vec!["LLDP observation exceeded input byte bound; refused".into()],
            ),
        };
    }
    let evidence = vec![EvidenceRef {
        kind: "raw_output".to_string(),
        sha256: sha256_hex(obs.capture.output.as_bytes()),
        locator: Some(obs.source.command().to_string()),
    }];
    if !parsed {
        let state = if obs.capture.outcome == CaptureOutcome::Unsupported {
            PayloadState::Unsupported
        } else {
            PayloadState::CaptureFailed
        };
        let mut facts = vec![];
        let status = status_fact(
            obs,
            state,
            Completeness::Unknown,
            "LLDP capture was not usable; no observation parsed",
            evidence,
        );
        if status.validate().is_ok() {
            facts.push(status);
        }
        let emitted = facts.len();
        return LldpNormalizationOutput {
            facts,
            neighbors: vec![],
            capture_coverage: vec![],
            report: base_report(
                emitted,
                None,
                vec!["LLDP capture was not usable; no observation parsed".into()],
            ),
        };
    }

    if let Err(e) =
        crate::xrd_openconfig_interfaces::reject_duplicate_json_keys(obs.capture.output.as_bytes())
    {
        let mut facts = vec![];
        let status = status_fact(
            obs,
            PayloadState::InvalidInput,
            Completeness::Unknown,
            "LLDP structured input contained duplicate JSON object keys",
            evidence.clone(),
        );
        if status.validate().is_ok() {
            facts.push(status);
        }
        let emitted = facts.len();
        return LldpNormalizationOutput {
            facts,
            neighbors: vec![],
            capture_coverage: vec![],
            report: base_report(
                emitted,
                None,
                vec![format!(
                    "ambiguous JSON object identity: {e}; no neighbor parsed"
                )],
            ),
        };
    }

    let root_result = serde_json::from_str::<Value>(obs.capture.output);
    let Ok(root) = root_result else {
        let parse_error = root_result.err().map(|e| e.to_string()).unwrap_or_default();
        let mut facts = vec![];
        let status = status_fact(
            obs,
            PayloadState::InvalidInput,
            Completeness::Unknown,
            "LLDP structured input was malformed JSON",
            evidence,
        );
        if status.validate().is_ok() {
            facts.push(status);
        }
        let emitted = facts.len();
        return LldpNormalizationOutput {
            facts,
            neighbors: vec![],
            capture_coverage: vec![],
            report: base_report(
                emitted,
                None,
                vec![format!("malformed JSON: {parse_error}; no neighbor parsed")],
            ),
        };
    };

    let parse_outcome = match obs.source {
        LldpStructuredSource::CumulusLldpdJson => parse_cumulus(&root, limits),
        LldpStructuredSource::AristaEosJson => parse_eos(&root, limits),
    };
    let mut invalid_records = parse_outcome.invalid_records;
    let mut invalid_samples = parse_outcome.invalid_samples.clone();
    let ancillary_errors = parse_outcome.malformed_management_address_entries
        + parse_outcome.omitted_management_address_entries
        + parse_outcome.malformed_capability_entries
        + parse_outcome.omitted_capability_entries;

    // Protocol-mandatory identity: LLDP without chassis-ID or port-ID is not a
    // usable neighbor record. It remains counted evidence in the report.
    let mut usable = Vec::new();
    for raw in parse_outcome.neighbors {
        if raw.chassis_id.is_none()
            || raw.port_id.is_none()
            || raw.chassis_subtype.value.is_none()
            || raw.port_subtype.value.is_none()
        {
            invalid_records += 1;
            if invalid_samples.len() < MAX_INVALID_SAMPLES {
                invalid_samples.push(format!(
                    "{}: neighbor missing mandatory chassis or port identifier",
                    raw.local_interface
                ));
            }
        } else if raw.local_interface.len() > 255 {
            invalid_records += 1;
        } else {
            usable.push(raw);
        }
    }

    // Exact duplicate observations are redundant. Generated array positions
    // are deliberately excluded from that comparison. Repeated or conflicting
    // observations of the same semantic neighbor remain separate evidence for
    // reconciliation, while distinct chassis/port identities remain distinct
    // neighbors even when they share one local interface.
    let mut seen_exact = BTreeSet::new();
    let mut identity_evidence: std::collections::BTreeMap<String, BTreeSet<String>> =
        std::collections::BTreeMap::new();
    let mut deduped = Vec::new();
    let mut duplicates = 0usize;
    let mut repeated_neighbor_records = 0usize;
    let mut conflicting_neighbor_records = 0usize;
    for raw in usable {
        if !seen_exact.insert(raw.exact_duplicate_key()) {
            duplicates += 1;
            continue;
        }
        let identity = raw.neighbor_identity_key();
        let evidence_key = raw.non_temporal_evidence_key();
        let observations = identity_evidence.entry(identity).or_default();
        if observations.contains(&evidence_key) {
            repeated_neighbor_records += 1;
        } else if !observations.is_empty() {
            conflicting_neighbor_records += 1;
        }
        observations.insert(evidence_key);
        deduped.push(raw);
    }

    // Validate before finalizing coverage: validation failures and output
    // omissions are part of population coverage, not an afterthought that can
    // leave a falsely complete coverage claim embedded in emitted facts.
    let provisional_degraded = invalid_records > 0
        || ancillary_errors > 0
        || duplicates > 0
        || conflicting_neighbor_records > 0;
    let mut validated_raw = Vec::new();
    let mut facts_rejected = 0usize;
    for raw in &deduped {
        let provisional = fact_for_neighbor(
            obs,
            raw,
            PopulationCoverage::Unknown,
            &parse_outcome.empty_interfaces,
            &parse_outcome.covered_interfaces,
            evidence.clone(),
            provisional_degraded,
        );
        match provisional.validate() {
            Ok(()) => validated_raw.push(raw),
            Err(e) => {
                facts_rejected += 1;
                if invalid_samples.len() < MAX_INVALID_SAMPLES {
                    invalid_samples.push(format!(
                        "{}: fact validation rejected: {e}",
                        raw.local_interface
                    ));
                }
            }
        }
    }
    let output_facts_omitted = validated_raw.len().saturating_sub(limits.max_output_facts);
    let population = if obs.capture.outcome == CaptureOutcome::Unknown {
        PopulationCoverage::Unknown
    } else if invalid_records > 0
        || ancillary_errors > 0
        || facts_rejected > 0
        || output_facts_omitted > 0
        || conflicting_neighbor_records > 0
        || obs.capture.outcome.degrades()
    {
        PopulationCoverage::Partial
    } else {
        PopulationCoverage::Complete
    };
    let degraded = invalid_records > 0
        || ancillary_errors > 0
        || duplicates > 0
        || facts_rejected > 0
        || output_facts_omitted > 0
        || conflicting_neighbor_records > 0;
    let mut facts = Vec::new();
    let mut diagnostics = Vec::new();
    for raw in &deduped {
        diagnostics.push(raw.diagnostic());
    }
    for raw in validated_raw.into_iter().take(limits.max_output_facts) {
        let fact = fact_for_neighbor(
            obs,
            raw,
            population,
            &parse_outcome.empty_interfaces,
            &parse_outcome.covered_interfaces,
            evidence.clone(),
            degraded,
        );
        match fact.validate() {
            Ok(()) => facts.push(fact),
            Err(e) => {
                facts_rejected += 1;
                if invalid_samples.len() < MAX_INVALID_SAMPLES {
                    invalid_samples.push(format!(
                        "{}: final fact validation rejected: {e}",
                        raw.local_interface
                    ));
                }
            }
        }
    }

    if facts.is_empty() {
        let (state, note) = if output_facts_omitted > 0 {
            (
                PayloadState::OutputBudgetExhausted,
                "effective output-fact budget prevented every validated neighbor fact",
            )
        } else if deduped.is_empty() && invalid_records == 0 {
            (
                PayloadState::Empty,
                "capture decoded successfully but contained no LLDP neighbor records",
            )
        } else {
            (
                PayloadState::NoUsableRecords,
                "decoded input existed but no usable validated neighbor fact was produced",
            )
        };
        let completeness = if obs.capture.outcome == CaptureOutcome::Unknown {
            Completeness::Unknown
        } else if degraded || obs.capture.outcome.degrades() {
            Completeness::Partial
        } else {
            Completeness::Complete
        };
        let status = status_fact(obs, state, completeness, note, evidence.clone());
        if status.validate().is_ok() {
            facts.push(status);
        } else {
            facts_rejected += 1;
        }
    }

    let mut notes = vec![
        "offline structured normalization only; production acquisition/operator exposure not established".to_string(),
        "remote identity is observation evidence only; no hostname, management-address or alias authority inferred".to_string(),
    ];
    if obs.context.release_raw.is_none() {
        notes.push("local device release unknown; upstream version=all selector is not release qualification".into());
    }
    if obs.context.network_namespace.is_none() {
        notes.push("network namespace unspecified; namespace-separated reconciliation requires explicit acquisition context".into());
    }
    if duplicates > 0 {
        notes.push(format!(
            "{duplicates} exact duplicate neighbor records deduplicated"
        ));
    }
    if invalid_records > 0 {
        notes.push(format!(
            "{invalid_records} invalid/unusable neighbor records counted"
        ));
    }
    if output_facts_omitted > 0 {
        notes.push(format!(
            "{output_facts_omitted} neighbor facts omitted at output budget"
        ));
    }

    let interfaces_seen = match obs.source {
        LldpStructuredSource::CumulusLldpdJson => root
            .get("lldp")
            .and_then(Value::as_array)
            .map(|lldp| {
                lldp.iter()
                    .filter_map(|c| c.get("interface").and_then(Value::as_array))
                    .map(Vec::len)
                    .sum()
            })
            .unwrap_or(0),
        LldpStructuredSource::AristaEosJson => root
            .get("lldpNeighbors")
            .and_then(Value::as_object)
            .map(|o| o.len())
            .unwrap_or(0),
    };

    let facts_len = facts.len();
    let diagnostics_len = diagnostics.len();
    let capture_coverage = vec![normalization_capture_coverage(
        obs,
        population,
        &parse_outcome.empty_interfaces,
        &parse_outcome.covered_interfaces,
        evidence,
    )];
    LldpNormalizationOutput {
        facts,
        neighbors: diagnostics,
        capture_coverage,
        report: LldpNormalizationReport {
            source: obs.source.label(),
            applicability: obs.source.applicability(),
            interfaces_seen,
            neighbors_decoded: diagnostics_len,
            duplicate_records: duplicates,
            invalid_records,
            empty_local_interfaces: parse_outcome.empty_interfaces.len(),
            empty_local_interface_names: parse_outcome.empty_interfaces.iter().cloned().collect(),
            covered_interfaces: parse_outcome.covered_interfaces.iter().cloned().collect(),
            malformed_management_address_entries: parse_outcome
                .malformed_management_address_entries,
            omitted_management_address_entries: parse_outcome.omitted_management_address_entries,
            malformed_capability_entries: parse_outcome.malformed_capability_entries,
            omitted_capability_entries: parse_outcome.omitted_capability_entries,
            repeated_neighbor_records,
            conflicting_neighbor_records,
            facts_emitted: facts_len,
            facts_rejected,
            output_facts_omitted,
            capture,
            context_error: None,
            invalid_samples,
            notes,
        },
    }
}

/// Convert one already-classified model-driven telemetry LLDP occurrence
/// into the same typed payload shape. This reuses the existing telemetry
/// extraction boundary rather than creating a competing parser. Subtype,
/// management-address, capability and age evidence are explicitly unknown
/// because the current SR Linux/OpenConfig/XR flattened extraction does not
/// carry them. `neighbor_index` is an occurrence key only.
pub fn payload_from_flat_model_driven(
    local_interface: &str,
    neighbor_id: &str,
    value: &Value,
    coverage: LldpCoverage,
) -> Option<LldpNeighborPayload> {
    let local = sanitize_scalar(local_interface).filter(|v| valid_name(v))?;
    let chassis = value
        .get("chassis-id")
        .and_then(Value::as_str)
        .and_then(sanitize_scalar);
    let port = value
        .get("port-id")
        .and_then(Value::as_str)
        .and_then(sanitize_scalar);
    let system = value
        .get("system-name")
        .and_then(Value::as_str)
        .and_then(sanitize_scalar);
    Some(LldpNeighborPayload {
        local_interface_name: local,
        normalized_local_interface_name: None,
        source_neighbor_id: None,
        capture_record_locator: sanitize_scalar(neighbor_id),
        protocol: NeighborProtocol::Lldp,
        remote_chassis_id: observed(chassis),
        remote_chassis_id_raw: None,
        remote_chassis_id_subtype: SubtypeObservation::unavailable(
            FieldAvailability::Unknown,
            None,
            Some("model-driven flattened extraction does not carry chassis subtype".into()),
        ),
        remote_port_id: observed(port),
        remote_port_id_raw: None,
        remote_port_id_subtype: SubtypeObservation::unavailable(
            FieldAvailability::Unknown,
            None,
            Some("model-driven flattened extraction does not carry port subtype".into()),
        ),
        remote_system_name: observed(system),
        remote_port_description: observed(None),
        remote_management_addresses: observed(None),
        capabilities: observed(None),
        ttl_seconds: observed(None),
        age_seconds: observed(None),
        temporal: None,
        coverage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUMULUS: &str =
        include_str!("../tests/fixtures/bn024/lldp/cumulus-leaf02-net-show-lldp-json.txt");
    const EOS: &str =
        include_str!("../tests/fixtures/bn024/lldp/eos-leaf02-show-lldp-neighbors-detail.json");

    fn ctx<'a>(device: &'a str, observed_at: &'a str) -> ObservationContext<'a> {
        let mut ctx = ObservationContext::minimal(device, "collector-a", "capture-1", observed_at);
        ctx.network_namespace = Some("default");
        ctx.session_epoch = 7;
        ctx.sequence = 42;
        ctx
    }

    fn obs<'a>(
        source: LldpStructuredSource,
        output: &'a str,
        device: &'a str,
        at: &'a str,
    ) -> LldpObservation<'a> {
        LldpObservation {
            source,
            capture: CommandCapture::new(output, CaptureOutcome::Complete),
            context: ctx(device, at),
        }
    }

    fn payload(fact: &TypedFact) -> LldpNeighborPayload {
        serde_json::from_value(fact.payload.fields.clone().expect("fields")).expect("typed payload")
    }

    #[test]
    fn genuine_cumulus_430_fixture_expected_values() {
        let out = normalize_lldp_neighbors(&obs(
            LldpStructuredSource::CumulusLldpdJson,
            CUMULUS,
            "leaf02",
            "2021-03-21T18:26:43Z",
        ));
        assert_eq!(
            out.report.invalid_records, 0,
            "{:?}",
            out.report.invalid_samples
        );
        assert_eq!(out.report.neighbors_decoded, 2);
        assert_eq!(out.facts.len(), 2);
        assert!(out.facts.iter().all(|f| f.validate().is_ok()));
        let first = payload(&out.facts[0]);
        assert_eq!(first.local_interface_name, "swp1");
        assert_eq!(first.remote_system_name.value.as_deref(), Some("spine01"));
        assert_eq!(
            first.remote_chassis_id.value.as_deref(),
            Some("52:54:00:a0:1a:f4")
        );
        assert_eq!(first.remote_port_id.value.as_deref(), Some("swp2"));
        assert_eq!(
            first.remote_port_id_subtype.value,
            Some(PortIdSubtype::InterfaceName)
        );
        assert_eq!(first.ttl_seconds.value, Some(120));
        assert_eq!(first.age_seconds.value, Some(384));
        let mgmt = first
            .remote_management_addresses
            .value
            .as_ref()
            .expect("mgmt");
        assert_eq!(mgmt.len(), 2);
        assert_eq!(mgmt[0].address, "10.0.0.21");
        assert_eq!(mgmt[1].address, "fe80::5054:ff:fea0:1af4");
        // Cumulus exposes age/TTL but does not establish exact last-contact
        // semantics in this contract; expiry is therefore explicitly unknown
        // rather than a fresh full TTL assigned to replayed output.
        assert_eq!(out.facts[0].valid_until, None);
        assert_eq!(out.facts[0].valid_from, None);
        let locals: BTreeSet<_> = out
            .facts
            .iter()
            .map(|f| f.subject.entity.clone().unwrap())
            .collect();
        assert_eq!(locals.len(), 2);
    }

    #[test]
    fn genuine_eos_fixture_preserves_parallel_links_and_empty_interfaces() {
        let out = normalize_lldp_neighbors(&obs(
            LldpStructuredSource::AristaEosJson,
            EOS,
            "leaf02",
            "2021-06-06T22:59:36Z",
        ));
        assert_eq!(
            out.report.invalid_records, 0,
            "{:?}",
            out.report.invalid_samples
        );
        assert_eq!(out.report.interfaces_seen, 7);
        assert_eq!(out.report.empty_local_interfaces, 3);
        assert_eq!(out.report.neighbors_decoded, 4);
        assert_eq!(out.facts.len(), 4);
        let parsed: Vec<_> = out.facts.iter().map(payload).collect();
        let leaf01_parallel: Vec<_> = parsed
            .iter()
            .filter(|p| p.remote_system_name.value.as_deref() == Some("leaf01"))
            .collect();
        assert_eq!(leaf01_parallel.len(), 2);
        let ports: BTreeSet<_> = leaf01_parallel
            .iter()
            .filter_map(|p| p.remote_port_id.value.as_deref())
            .collect();
        assert_eq!(ports, BTreeSet::from(["Ethernet5", "Ethernet6"]));
        assert!(
            parsed
                .iter()
                .all(|p| p.remote_port_id_subtype.value == Some(PortIdSubtype::InterfaceName))
        );
        assert!(
            parsed
                .iter()
                .all(|p| p.remote_chassis_id_subtype.value == Some(ChassisIdSubtype::MacAddress))
        );
        assert_eq!(
            parsed[0].remote_chassis_id.value.as_deref(),
            Some("44:38:39:ba:d2:34")
        );
        assert_eq!(
            parsed[0].remote_chassis_id_raw.as_deref(),
            Some("4438.39ba.d234")
        );
        assert_eq!(
            parsed[0].temporal.as_ref().map(|t| t.validity_basis),
            Some(LldpValidityBasis::LastContactPlusAdvertisedTtl)
        );
        assert!(out.facts[0].valid_from.is_some());
        assert!(out.facts[0].valid_until.is_some());
        assert!(
            out.facts[0].valid_until.as_deref().unwrap()
                > out.facts[0].valid_from.as_deref().unwrap()
        );
        assert_eq!(
            out.report.empty_local_interface_names,
            vec!["Ethernet3", "Ethernet4", "Management1"]
        );
        assert!(
            out.report
                .covered_interfaces
                .iter()
                .any(|i| i == "Management1")
        );
    }

    #[test]
    fn malformed_input_is_explicit_status_not_panic() {
        let observation = obs(
            LldpStructuredSource::CumulusLldpdJson,
            "{",
            "d1",
            "2026-01-01T00:00:00Z",
        );
        let out = normalize_lldp_neighbors(&observation);
        assert_eq!(out.facts.len(), 1);
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
        assert!(out.facts[0].validate().is_ok());
    }

    #[test]
    fn eos_multiple_neighbors_on_one_interface_are_preserved() {
        let mut root: Value = serde_json::from_str(EOS).unwrap();
        let info = root
            .pointer_mut("/lldpNeighbors/Ethernet1/lldpNeighborInfo")
            .and_then(Value::as_array_mut)
            .unwrap();
        let second = info[0].clone();
        info.push(second);
        let text = serde_json::to_string(&root).unwrap();
        let out = normalize_lldp_neighbors(&obs(
            LldpStructuredSource::AristaEosJson,
            &text,
            "leaf02",
            "2021-06-06T22:59:36Z",
        ));
        // The copy is an exact duplicate and is counted/deduplicated rather
        // than silently emitted twice; distinct neighbors remain separate.
        assert_eq!(out.report.duplicate_records, 1);
        assert_eq!(out.facts.len(), 4);
    }

    #[test]
    fn missing_mandatory_identity_is_counted_not_dropped_silently() {
        let mut root: Value = serde_json::from_str(EOS).unwrap();
        root.pointer_mut("/lldpNeighbors/Ethernet1/lldpNeighborInfo/0")
            .and_then(Value::as_object_mut)
            .unwrap()
            .remove("chassisId");
        let text = serde_json::to_string(&root).unwrap();
        let out = normalize_lldp_neighbors(&obs(
            LldpStructuredSource::AristaEosJson,
            &text,
            "leaf02",
            "2021-06-06T22:59:36Z",
        ));
        assert_eq!(out.report.invalid_records, 1);
        assert_eq!(out.facts.len(), 3);
        assert!(
            out.report
                .invalid_samples
                .iter()
                .any(|s| s.contains("mandatory chassis"))
        );
    }

    #[test]
    fn eos_distinct_neighbor_on_same_interface_is_not_duplicate() {
        let mut root: Value = serde_json::from_str(EOS).unwrap();
        let info = root
            .pointer_mut("/lldpNeighbors/Ethernet1/lldpNeighborInfo")
            .and_then(Value::as_array_mut)
            .unwrap();
        let mut second = info[0].clone();
        second["neighborInterfaceInfo"]["interfaceId"] = Value::String("Ethernet9".into());
        info.push(second);
        let text = serde_json::to_string(&root).unwrap();
        let out = normalize_lldp_neighbors(&obs(
            LldpStructuredSource::AristaEosJson,
            &text,
            "leaf02",
            "2021-06-06T22:59:36Z",
        ));
        assert_eq!(out.report.duplicate_records, 0);
        assert_eq!(out.report.repeated_neighbor_records, 0);
        assert_eq!(out.report.conflicting_neighbor_records, 0);
        assert_eq!(out.facts.len(), 5);
    }

    #[test]
    fn eos_conflicting_attributes_are_retained_and_classified() {
        let mut root: Value = serde_json::from_str(EOS).unwrap();
        let info = root
            .pointer_mut("/lldpNeighbors/Ethernet1/lldpNeighborInfo")
            .and_then(Value::as_array_mut)
            .unwrap();
        let mut second = info[0].clone();
        second["systemName"] = Value::String("conflicting-name".into());
        info.push(second);
        let text = serde_json::to_string(&root).unwrap();
        let out = normalize_lldp_neighbors(&obs(
            LldpStructuredSource::AristaEosJson,
            &text,
            "leaf02",
            "2021-06-06T22:59:36Z",
        ));
        assert_eq!(out.report.duplicate_records, 0);
        assert_eq!(out.report.conflicting_neighbor_records, 1);
        assert_eq!(out.facts.len(), 5);
    }

    #[test]
    fn eos_repeated_observation_with_changed_age_is_not_exact_duplicate() {
        let mut root: Value = serde_json::from_str(EOS).unwrap();
        let info = root
            .pointer_mut("/lldpNeighbors/Ethernet1/lldpNeighborInfo")
            .and_then(Value::as_array_mut)
            .unwrap();
        let mut second = info[0].clone();
        second["lastContactTime"] = Value::from(1623025180.0);
        info.push(second);
        let text = serde_json::to_string(&root).unwrap();
        let out = normalize_lldp_neighbors(&obs(
            LldpStructuredSource::AristaEosJson,
            &text,
            "leaf02",
            "2021-06-06T22:59:36Z",
        ));
        assert_eq!(out.report.duplicate_records, 0);
        assert_eq!(out.report.repeated_neighbor_records, 1);
        assert_eq!(out.report.conflicting_neighbor_records, 0);
        assert_eq!(out.facts.len(), 5);
    }

    #[test]
    fn eos_reordered_source_array_preserves_fact_identity_set() {
        let original = normalize_lldp_neighbors(&obs(
            LldpStructuredSource::AristaEosJson,
            EOS,
            "leaf02",
            "2021-06-06T22:59:36Z",
        ));
        let mut root: Value = serde_json::from_str(EOS).unwrap();
        let info = root
            .pointer_mut("/lldpNeighbors/Ethernet1/lldpNeighborInfo")
            .and_then(Value::as_array_mut)
            .unwrap();
        info.reverse();
        let text = serde_json::to_string(&root).unwrap();
        let reordered = normalize_lldp_neighbors(&obs(
            LldpStructuredSource::AristaEosJson,
            &text,
            "leaf02",
            "2021-06-06T22:59:36Z",
        ));
        // Reordered bytes are distinct raw evidence, so typed fact/evidence
        // digests may legitimately differ. Semantic neighbor identity and
        // population classification must not: array position is not identity.
        let key = |f: &TypedFact| {
            let p = payload(f);
            (
                p.local_interface_name,
                p.remote_chassis_id_subtype.value,
                p.remote_chassis_id.value,
                p.remote_port_id_subtype.value,
                p.remote_port_id.value,
                p.remote_system_name.value,
            )
        };
        let a: BTreeSet<_> = original.facts.iter().map(key).collect();
        let b: BTreeSet<_> = reordered.facts.iter().map(key).collect();
        assert_eq!(a, b);
        assert_eq!(
            original.report.neighbors_decoded,
            reordered.report.neighbors_decoded
        );
        assert_eq!(
            original.report.duplicate_records,
            reordered.report.duplicate_records
        );
    }

    #[test]
    fn zero_output_budget_is_exact_omission_not_empty_success() {
        let observation = obs(
            LldpStructuredSource::AristaEosJson,
            EOS,
            "leaf02",
            "2021-06-06T22:59:36Z",
        );
        let out = normalize_lldp_neighbors_with_limits(
            &observation,
            NormalizerLimits {
                max_output_facts: 0,
                ..NormalizerLimits::default()
            },
        );
        assert_eq!(out.report.output_facts_omitted, 4);
        assert_eq!(out.facts.len(), 1);
        assert_eq!(
            out.facts[0].payload.state,
            Some(PayloadState::OutputBudgetExhausted)
        );
    }

    #[test]
    fn cumulus_empty_without_lldp_provenance_is_not_positive_empty() {
        let raw = br#"{"lldp":[{"interface":[{"name":"swp1"}]}]}"#;
        let out = normalize_lldp_neighbors(&obs(
            LldpStructuredSource::CumulusLldpdJson,
            std::str::from_utf8(raw).unwrap(),
            "leaf02",
            "2021-03-21T18:26:43Z",
        ));
        assert_eq!(out.report.empty_local_interfaces, 0);
        assert!(out.report.invalid_records > 0);
        assert!(
            out.report
                .invalid_samples
                .iter()
                .any(|s| s.contains("discovery protocol")),
            "missing protocol provenance must be explicit invalid evidence"
        );
    }

    #[test]
    fn cumulus_positive_empty_differs_from_malformed_containers() {
        let text = serde_json::json!({
            "lldp": [{"interface": [
                {"name":"swp1","via":"LLDP"},
                {"name":"swp2","via":"LLDP","chassis":{},"port":[]}
            ]}]
        })
        .to_string();
        let out = normalize_lldp_neighbors(&obs(
            LldpStructuredSource::CumulusLldpdJson,
            &text,
            "leaf02",
            "2021-03-21T18:26:43Z",
        ));
        assert_eq!(out.report.empty_local_interface_names, vec!["swp1"]);
        assert_eq!(out.report.invalid_records, 1);
        assert!(
            !out.report
                .empty_local_interface_names
                .contains(&"swp2".to_string())
        );
    }

    #[test]
    fn positive_empty_capture_preserves_scoped_coverage_without_neighbor_facts() {
        let text = serde_json::json!({
            "lldp": [{"interface": [
                {"name":"swp1","via":"LLDP"}
            ]}]
        })
        .to_string();
        let out = normalize_lldp_neighbors(&obs(
            LldpStructuredSource::CumulusLldpdJson,
            &text,
            "leaf02",
            "2021-03-21T18:26:43Z",
        ));
        assert!(out.facts.iter().all(|f| f.subject.entity.is_none()));
        assert_eq!(out.capture_coverage.len(), 1);
        let coverage = &out.capture_coverage[0];
        assert_eq!(coverage.population_coverage, PopulationCoverage::Complete);
        assert!(coverage.covered_interfaces.contains("swp1"));
        assert_eq!(coverage.collector_id.as_deref(), Some("collector-a"));
        assert_eq!(coverage.capture_id.as_deref(), Some("capture-1"));
        assert_eq!(coverage.valid_until, None);
    }

    #[test]
    fn cumulus_multi_object_chassis_cardinality_is_invalid_not_first_selected() {
        let mut root: Value = serde_json::from_str(CUMULUS).unwrap();
        let chassis = root
            .pointer_mut("/lldp/0/interface/0/chassis")
            .and_then(Value::as_array_mut)
            .unwrap();
        let second = chassis[0].clone();
        chassis.push(second);
        let text = serde_json::to_string(&root).unwrap();
        let out = normalize_lldp_neighbors(&obs(
            LldpStructuredSource::CumulusLldpdJson,
            &text,
            "leaf02",
            "2021-03-21T18:26:43Z",
        ));
        assert_eq!(out.report.invalid_records, 1);
        assert_eq!(out.facts.len(), 1);
        assert!(
            out.report
                .invalid_samples
                .iter()
                .any(|s| s.contains("cardinality"))
        );
    }

    #[test]
    fn duplicate_json_keys_are_rejected_before_field_interpretation() {
        let text = CUMULUS.replacen(
            "\"name\": \"swp1\"",
            "\"name\":\"swp1\",\"name\":\"swp1x\"",
            1,
        );
        let out = normalize_lldp_neighbors(&obs(
            LldpStructuredSource::CumulusLldpdJson,
            &text,
            "leaf02",
            "2021-03-21T18:26:43Z",
        ));
        assert_eq!(out.facts.len(), 1);
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
    }

    #[test]
    fn oversized_input_is_refused_before_raw_evidence_hashing() {
        let observation = obs(
            LldpStructuredSource::AristaEosJson,
            EOS,
            "leaf02",
            "2021-06-06T22:59:36Z",
        );
        let out = normalize_lldp_neighbors_with_limits(
            &observation,
            NormalizerLimits {
                max_input_bytes: 8,
                ..NormalizerLimits::default()
            },
        );
        assert_eq!(out.facts.len(), 1);
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
        assert!(out.facts[0].evidence.is_empty());
    }

    #[test]
    fn eos_ttl_zero_expires_at_last_contact_not_capture_plus_full_ttl() {
        let mut root: Value = serde_json::from_str(EOS).unwrap();
        for entry in root
            .pointer_mut("/lldpNeighbors/Ethernet1/lldpNeighborInfo")
            .and_then(Value::as_array_mut)
            .unwrap()
        {
            entry["ttl"] = Value::from(0);
        }
        let text = serde_json::to_string(&root).unwrap();
        let out = normalize_lldp_neighbors(&obs(
            LldpStructuredSource::AristaEosJson,
            &text,
            "leaf02",
            "2021-06-06T22:59:36Z",
        ));
        let fact = out
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("Ethernet1"))
            .unwrap();
        assert_eq!(fact.valid_from, fact.valid_until);
    }
}
