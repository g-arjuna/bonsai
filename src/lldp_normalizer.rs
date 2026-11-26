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
    LldpCoverage, LldpNeighborPayload, ManagementAddress, ManagementAddressType, NeighborProtocol,
    PopulationCoverage, PortIdSubtype, SubtypeObservation,
};
use crate::typed_facts::{
    Completeness, EvidenceRef, FactPayload, FactSource, FactSubject, Lifecycle, PayloadState,
    Quality, TrustClass, TypedFact,
};

pub const LLDP_PARSER_REVISION: &str = "lldp-structured-json-2026.11.26";
pub const LLDP_MAPPING_REVISION: &str = "bn024-lldp-map-1";
const MAX_NEIGHBORS_PER_INTERFACE: usize = 64;
const MAX_INVALID_SAMPLES: usize = 16;

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
    pub source_neighbor_id: String,
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
}

#[derive(Clone, Debug)]
struct RawNeighbor {
    local_interface: String,
    source_neighbor_id: String,
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
}

impl RawNeighbor {
    fn diagnostic(&self) -> LldpNeighborDiagnostic {
        LldpNeighborDiagnostic {
            local_interface: self.local_interface.clone(),
            source_neighbor_id: self.source_neighbor_id.clone(),
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

    fn semantic_key(&self) -> String {
        serde_json::json!([
            self.local_interface,
            self.chassis_id,
            self.port_id,
            self.system_name,
        ])
        .to_string()
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

fn first_array_object<'a>(obj: &'a Value, key: &str) -> Option<&'a Value> {
    get_array(obj, key)?.first().filter(|v| v.is_object())
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
    Some(
        days.saturating_mul(86_400)
            .saturating_add(hours * 3600 + minutes * 60 + seconds),
    )
}

fn parse_u32_value(raw: Option<String>) -> Option<u32> {
    raw.and_then(|v| v.parse::<u32>().ok()).filter(|v| *v > 0)
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

fn parse_cumulus(
    root: &Value,
    limits: NormalizerLimits,
) -> (Vec<RawNeighbor>, usize, usize, Vec<String>) {
    let mut out = Vec::new();
    let mut invalid = 0usize;
    let mut empty_interfaces = 0usize;
    let mut samples = Vec::new();
    let mut record_index = 0usize;
    let note_invalid = |samples: &mut Vec<String>, msg: String, invalid: &mut usize| {
        *invalid += 1;
        if samples.len() < MAX_INVALID_SAMPLES {
            samples.push(msg);
        }
    };

    let Some(lldp) = get_array(root, "lldp") else {
        note_invalid(
            &mut samples,
            "root lldp array absent or wrong type".into(),
            &mut invalid,
        );
        return (out, invalid, empty_interfaces, samples);
    };
    for container in lldp {
        let Some(interfaces) = get_array(container, "interface") else {
            if container.as_object().is_some_and(|o| !o.is_empty()) {
                note_invalid(
                    &mut samples,
                    "lldp container lacks interface array".into(),
                    &mut invalid,
                );
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
                note_invalid(
                    &mut samples,
                    "interface record missing valid name".into(),
                    &mut invalid,
                );
                continue;
            };
            let chassis = first_array_object(interface, "chassis");
            let port = first_array_object(interface, "port");
            if chassis.is_none() && port.is_none() {
                empty_interfaces += 1;
                continue;
            }
            let (Some(chassis), Some(port)) = (chassis, port) else {
                note_invalid(
                    &mut samples,
                    format!("{local}: neighbor record missing chassis or port container"),
                    &mut invalid,
                );
                continue;
            };
            if out.len() >= limits.max_entities {
                note_invalid(
                    &mut samples,
                    format!("neighbor on {local} omitted at entity bound"),
                    &mut invalid,
                );
                continue;
            }
            record_index += 1;
            let rid = interface
                .get("rid")
                .and_then(Value::as_str)
                .and_then(sanitize_scalar)
                .unwrap_or_else(|| format!("record-{record_index}"));
            let chassis_id_obj = first_array_object(chassis, "id");
            let chassis_subtype_raw = chassis_id_obj
                .and_then(|v| v.get("type"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let chassis_subtype = map_cumulus_chassis_subtype(chassis_subtype_raw);
            let chassis_raw = chassis_id_obj
                .and_then(|v| v.get("value"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let (chassis_id, chassis_id_raw, _) = normalize_identifier_by_subtype(
                chassis_raw,
                chassis_subtype.value == Some(ChassisIdSubtype::MacAddress),
            );
            let port_id_obj = first_array_object(port, "id");
            let port_subtype_raw = port_id_obj
                .and_then(|v| v.get("type"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let port_subtype = map_cumulus_port_subtype(port_subtype_raw);
            let port_raw = port_id_obj
                .and_then(|v| v.get("value"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let (port_id, port_id_raw, _) = normalize_identifier_by_subtype(
                port_raw,
                port_subtype.value == Some(PortIdSubtype::MacAddress),
            );
            let mgmt_values = wrapped_values(Some(chassis), "mgmt-ip");
            let mgmt = if get_array(chassis, "mgmt-ip").is_some() {
                Some(
                    mgmt_values
                        .iter()
                        .filter_map(|address| {
                            let address_type = if address.parse::<std::net::Ipv4Addr>().is_ok() {
                                ManagementAddressType::Ipv4
                            } else if address.parse::<std::net::Ipv6Addr>().is_ok() {
                                ManagementAddressType::Ipv6
                            } else {
                                ManagementAddressType::Unknown
                            };
                            sanitize_scalar(address).map(|address| ManagementAddress {
                                address,
                                address_type,
                                interface_num_type: None,
                                interface_num: None,
                            })
                        })
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            };
            let capabilities = get_array(chassis, "capability").map(|caps| {
                caps.iter()
                    .filter_map(|cap| {
                        let name = cap
                            .get("type")
                            .and_then(Value::as_str)
                            .and_then(sanitize_scalar)?;
                        Some(CapabilityObservation {
                            name,
                            advertised: None,
                            enabled: cap.get("enabled").and_then(Value::as_bool),
                        })
                    })
                    .collect::<Vec<_>>()
            });
            out.push(RawNeighbor {
                local_interface: local,
                source_neighbor_id: rid,
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
                management_addresses: mgmt,
                capabilities,
                ttl_seconds: parse_u32_value(first_wrapped_value(Some(port), "ttl")),
                age_seconds: parse_age_seconds(
                    interface
                        .get("age")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                ),
            });
        }
    }
    (out, invalid, empty_interfaces, samples)
}

fn parse_eos(
    root: &Value,
    limits: NormalizerLimits,
) -> (Vec<RawNeighbor>, usize, usize, Vec<String>) {
    let mut out = Vec::new();
    let mut invalid = 0usize;
    let mut empty_interfaces = 0usize;
    let mut samples = Vec::new();
    let Some(neighbors_obj) = root.get("lldpNeighbors").and_then(Value::as_object) else {
        return (
            out,
            1,
            empty_interfaces,
            vec!["root lldpNeighbors object absent or wrong type".into()],
        );
    };
    for (local_raw, entry) in neighbors_obj {
        let Some(local) = sanitize_scalar(local_raw).filter(|v| valid_name(v)) else {
            invalid += 1;
            if samples.len() < MAX_INVALID_SAMPLES {
                samples.push("lldpNeighbors key is not a valid interface name".into());
            }
            continue;
        };
        let Some(info) = entry.get("lldpNeighborInfo").and_then(Value::as_array) else {
            invalid += 1;
            if samples.len() < MAX_INVALID_SAMPLES {
                samples.push(format!("{local}: lldpNeighborInfo absent or wrong type"));
            }
            continue;
        };
        if info.is_empty() {
            empty_interfaces += 1;
            continue;
        }
        for (idx, nbr) in info.iter().enumerate().take(MAX_NEIGHBORS_PER_INTERFACE) {
            if out.len() >= limits.max_entities {
                invalid += 1;
                if samples.len() < MAX_INVALID_SAMPLES {
                    samples.push(format!("{local}: neighbor omitted at entity bound"));
                }
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
            let mgmt = nbr
                .get("managementAddresses")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            let address = item
                                .get("address")
                                .and_then(Value::as_str)
                                .and_then(sanitize_scalar)?;
                            let address_type = match item.get("addressType").and_then(Value::as_str)
                            {
                                Some("ipv4") => ManagementAddressType::Ipv4,
                                Some("ipv6") => ManagementAddressType::Ipv6,
                                _ => ManagementAddressType::Unknown,
                            };
                            Some(ManagementAddress {
                                address,
                                address_type,
                                interface_num_type: item
                                    .get("interfaceNumType")
                                    .and_then(Value::as_str)
                                    .and_then(sanitize_scalar),
                                interface_num: item.get("interfaceNum").and_then(Value::as_u64),
                            })
                        })
                        .collect::<Vec<_>>()
                });
            let capabilities = nbr
                .get("systemCapabilities")
                .and_then(Value::as_object)
                .map(|caps| {
                    caps.iter()
                        .map(|(name, enabled)| CapabilityObservation {
                            name: name.clone(),
                            advertised: None,
                            enabled: enabled.as_bool(),
                        })
                        .collect::<Vec<_>>()
                });
            out.push(RawNeighbor {
                local_interface: local.clone(),
                source_neighbor_id: format!("{local}#{idx}"),
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
                management_addresses: mgmt,
                capabilities,
                ttl_seconds: nbr
                    .get("ttl")
                    .and_then(Value::as_u64)
                    .and_then(|v| u32::try_from(v).ok())
                    .filter(|v| *v > 0),
                age_seconds: None,
            });
        }
        if info.len() > MAX_NEIGHBORS_PER_INTERFACE {
            invalid += info.len() - MAX_NEIGHBORS_PER_INTERFACE;
            if samples.len() < MAX_INVALID_SAMPLES {
                samples.push(format!(
                    "{local}: {} neighbors omitted at per-interface bound",
                    info.len() - MAX_NEIGHBORS_PER_INTERFACE
                ));
            }
        }
    }
    (out, invalid, empty_interfaces, samples)
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

fn ttl_valid_until(observed_at: &str, ttl: Option<u32>) -> Option<String> {
    let observed = parse_rfc3339(observed_at)?;
    let ttl = ttl?;
    let until = observed + chrono::Duration::seconds(i64::from(ttl));
    Some(until.to_rfc3339())
}

fn fact_for_neighbor(
    obs: &LldpObservation,
    raw: &RawNeighbor,
    population: PopulationCoverage,
    empty_local_interfaces: usize,
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
        source_neighbor_id: Some(raw.source_neighbor_id.clone()),
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
        coverage: LldpCoverage {
            capture_outcome: capture_coverage(obs.capture.outcome),
            population_coverage: population,
            empty_local_interfaces: empty_local_interfaces as u32,
        },
    };
    let fields = serde_json::to_value(&payload).expect("typed LLDP payload serializes");
    let valid_until = ttl_valid_until(obs.context.observed_at, ttl.value);
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
        valid_from: Some(obs.context.observed_at.to_string()),
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
            report: base_report(
                0,
                Some(err.clone()),
                vec![format!(
                    "observation context invalid: {err}; nothing emitted"
                )],
            ),
        };
    }
    let evidence = vec![EvidenceRef {
        kind: "raw_output".to_string(),
        sha256: sha256_hex(obs.capture.output.as_bytes()),
        locator: Some(obs.source.command().to_string()),
    }];
    if oversized {
        let mut facts = vec![];
        let status = status_fact(
            obs,
            PayloadState::InvalidInput,
            Completeness::Unknown,
            "LLDP observation exceeded input byte bound; refused",
            evidence,
        );
        if status.validate().is_ok() {
            facts.push(status);
        }
        let emitted = facts.len();
        return LldpNormalizationOutput {
            facts,
            neighbors: vec![],
            report: base_report(
                emitted,
                None,
                vec!["LLDP observation exceeded input byte bound; refused".into()],
            ),
        };
    }
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
            report: base_report(
                emitted,
                None,
                vec!["LLDP capture was not usable; no observation parsed".into()],
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
            report: base_report(
                emitted,
                None,
                vec![format!("malformed JSON: {parse_error}; no neighbor parsed")],
            ),
        };
    };

    let (raw_neighbors, mut invalid_records, empty_interfaces, mut invalid_samples) =
        match obs.source {
            LldpStructuredSource::CumulusLldpdJson => parse_cumulus(&root, limits),
            LldpStructuredSource::AristaEosJson => parse_eos(&root, limits),
        };

    // Protocol-mandatory identity: LLDP without chassis-ID or port-ID is not a
    // usable neighbor record. It remains counted evidence in the report.
    let mut usable = Vec::new();
    for raw in raw_neighbors {
        if raw.chassis_id.is_none() || raw.port_id.is_none() {
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

    // Exact duplicate observations are redundant. Conflicting observations
    // whose identifiers differ are deliberately retained for reconciliation.
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    let mut duplicates = 0usize;
    for raw in usable {
        if seen.insert(raw.semantic_key()) {
            deduped.push(raw);
        } else {
            duplicates += 1;
        }
    }

    let population = if obs.capture.outcome == CaptureOutcome::Unknown {
        PopulationCoverage::Unknown
    } else if invalid_records > 0 || obs.capture.outcome.degrades() {
        PopulationCoverage::Partial
    } else {
        PopulationCoverage::Complete
    };
    let degraded = invalid_records > 0 || duplicates > 0;
    let mut facts = Vec::new();
    let mut facts_rejected = 0usize;
    let mut output_facts_omitted = 0usize;
    let mut diagnostics = Vec::new();
    for raw in &deduped {
        diagnostics.push(raw.diagnostic());
        if facts.len() >= limits.max_output_facts {
            output_facts_omitted += 1;
            continue;
        }
        let fact = fact_for_neighbor(
            obs,
            raw,
            population,
            empty_interfaces,
            evidence.clone(),
            degraded,
        );
        match fact.validate() {
            Ok(()) => facts.push(fact),
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

    if facts.is_empty() && output_facts_omitted == 0 {
        let (state, note) = if deduped.is_empty() && invalid_records == 0 {
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
    } else if facts.is_empty() && output_facts_omitted > 0 {
        output_facts_omitted = output_facts_omitted.saturating_sub(1);
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
    LldpNormalizationOutput {
        facts,
        neighbors: diagnostics,
        report: LldpNormalizationReport {
            source: obs.source.label(),
            applicability: obs.source.applicability(),
            interfaces_seen,
            neighbors_decoded: diagnostics_len,
            duplicate_records: duplicates,
            invalid_records,
            empty_local_interfaces: empty_interfaces,
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
        source_neighbor_id: sanitize_scalar(neighbor_id),
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
        assert_eq!(
            out.facts[0].valid_until.as_deref(),
            Some("2021-03-21T18:28:43+00:00")
        );
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
}
