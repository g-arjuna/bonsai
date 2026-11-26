//! Offline ARP/IPv6-ND and MAC forwarding-table normalization.
//!
//! The first qualified input contracts are exact source shapes present in
//! pinned upstream evidence: EOS `show arp vrf all` / `show mac-address-table
//! unicast` JSON, NXOS native JSON ARP/MAC output, and Cumulus/Linux
//! `ip neigh show` / `bridge fdb show` text. The normalizer is offline and
//! write-free. It preserves malformed records, unknown fields, compound
//! interface evidence and capture outcomes rather than importing SuzieQ's
//! lossy defaults (for example zero-MAC fabrication or stale→reachable
//! collapsing).

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use crate::interface_facts::{
    FieldAvailability, ObservedField, normalize_mac as normalize_mac_address,
};
use crate::interface_normalizer::{CaptureOutcome, CommandCapture, ObservationContext};
use crate::neighbor_facts::{
    AddressFamily, ArpndNeighborPayload, FACT_KIND_ARPND_NEIGHBOR, FACT_KIND_MAC_FORWARDING,
    MacEntryType, MacForwardingPayload, NeighborResolutionState,
};
use crate::typed_facts::{
    Completeness, EvidenceRef, FactPayload, FactSource, FactSubject, Lifecycle, Quality,
    TrustClass, TypedFact,
};
use crate::xrd_openconfig_interfaces::reject_duplicate_json_keys;

pub const NEIGHBOR_PARSER_REVISION: &str = "neighbor-structured-2026.11.26";
pub const NEIGHBOR_MAPPING_REVISION: &str = "bn024-neighbor-map-1";
const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_RECORDS: usize = 20_000;
const MAX_OUTPUT_FACTS: usize = 20_000;
const MAX_INVALID_SAMPLES: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArpndSourceProfile {
    EosShowArpVrfAllJson,
    NxosShowIpArpDetailJsonNative,
    CumulusIpNeighShow,
}

impl ArpndSourceProfile {
    pub fn label(self) -> &'static str {
        match self {
            Self::EosShowArpVrfAllJson => "eos/show-arp-vrf-all-json",
            Self::NxosShowIpArpDetailJsonNative => "nxos/show-ip-arp-detail-vrf-all-json-native",
            Self::CumulusIpNeighShow => "cumulus/ip-neigh-show",
        }
    }
    fn source_kind(self) -> &'static str {
        match self {
            Self::EosShowArpVrfAllJson | Self::NxosShowIpArpDetailJsonNative => {
                "structured_cli_json"
            }
            Self::CumulusIpNeighShow => "cli_text",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacSourceProfile {
    EosShowMacAddressTableUnicastJson,
    NxosShowMacAddressTableJsonNative,
    CumulusBridgeFdbShow,
}

impl MacSourceProfile {
    pub fn label(self) -> &'static str {
        match self {
            Self::EosShowMacAddressTableUnicastJson => "eos/show-mac-address-table-unicast-json",
            Self::NxosShowMacAddressTableJsonNative => "nxos/show-mac-address-table-json-native",
            Self::CumulusBridgeFdbShow => "cumulus/bridge-fdb-show",
        }
    }
    fn source_kind(self) -> &'static str {
        match self {
            Self::EosShowMacAddressTableUnicastJson | Self::NxosShowMacAddressTableJsonNative => {
                "structured_cli_json"
            }
            Self::CumulusBridgeFdbShow => "cli_text",
        }
    }
}

/// Optional structured interface identity evidence independently established
/// by the interface slice. Keys are exact source-native interface names. The
/// neighbor normalizer deliberately does not infer subinterface axes by
/// splitting display strings.
#[derive(Default)]
pub struct InterfaceIdentityEvidence {
    pub by_native_name: BTreeMap<String, crate::typed_facts::InterfaceSubjectIdentity>,
}

pub struct ArpndObservation<'a> {
    pub profile: ArpndSourceProfile,
    pub capture: CommandCapture<'a>,
    pub context: ObservationContext<'a>,
    pub interface_identities: Option<&'a InterfaceIdentityEvidence>,
}

pub struct MacObservation<'a> {
    pub profile: MacSourceProfile,
    pub capture: CommandCapture<'a>,
    pub context: ObservationContext<'a>,
    pub interface_identities: Option<&'a InterfaceIdentityEvidence>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct NeighborNormalizationReport {
    pub profile: String,
    pub capture_outcome: String,
    pub input_bytes: usize,
    pub records_decoded: usize,
    pub input_records_omitted: usize,
    pub malformed_records: usize,
    pub duplicate_records: usize,
    pub facts_emitted: usize,
    pub facts_rejected: usize,
    pub output_facts_omitted: usize,
    pub empty_records: bool,
    pub collection_presence: BTreeMap<String, String>,
    pub invalid_samples: Vec<String>,
    pub notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal_reason: Option<String>,
}

pub struct NeighborNormalizationOutput {
    pub facts: Vec<TypedFact>,
    pub report: NeighborNormalizationReport,
}

#[derive(Clone, Debug, PartialEq)]
struct ArpndRecord {
    payload: ArpndNeighborPayload,
    interface: String,
    vrf: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct MacRecord {
    payload: MacForwardingPayload,
    interface: String,
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

fn unknown<T>(detail: impl Into<String>) -> ObservedField<T> {
    ObservedField::unavailable(FieldAvailability::Unknown, Some(detail.into()))
}

fn invalid<T>(detail: impl Into<String>) -> ObservedField<T> {
    ObservedField::unavailable(FieldAvailability::Invalid, Some(detail.into()))
}

fn bound_invalid_samples(report: &mut NeighborNormalizationReport) {
    if report.invalid_samples.len() > MAX_INVALID_SAMPLES {
        let overflow = report.invalid_samples.len() - MAX_INVALID_SAMPLES;
        report.invalid_samples.truncate(MAX_INVALID_SAMPLES - 1);
        report.invalid_samples.push(format!(
            "{overflow} additional invalid samples omitted by diagnostic bound"
        ));
    }
}

fn capture_outcome_label(o: CaptureOutcome) -> &'static str {
    match o {
        CaptureOutcome::Complete => "complete",
        CaptureOutcome::Partial => "partial",
        CaptureOutcome::Unavailable => "unavailable",
        CaptureOutcome::Failed => "failed",
        CaptureOutcome::TimedOut => "timed_out",
        CaptureOutcome::Truncated => "truncated",
        CaptureOutcome::Unsupported => "unsupported",
        CaptureOutcome::Unknown => "unknown",
    }
}

fn context_error(ctx: &ObservationContext<'_>) -> Option<String> {
    for (name, value) in [
        ("device_id", ctx.device_id),
        ("collector_id", ctx.collector_id),
        ("capture_id", ctx.capture_id),
    ] {
        if value.trim().is_empty() {
            return Some(format!("observation context '{name}' is empty"));
        }
    }
    if chrono::DateTime::parse_from_rfc3339(ctx.observed_at).is_err() {
        return Some("observation context 'observed_at' is not RFC-3339".into());
    }
    None
}

fn completeness(
    capture: &CommandCapture<'_>,
    malformed: usize,
    omitted: usize,
    rejected: usize,
) -> Completeness {
    if capture.outcome == CaptureOutcome::Complete
        && malformed == 0
        && omitted == 0
        && rejected == 0
    {
        Completeness::Complete
    } else if capture.outcome == CaptureOutcome::Unknown {
        Completeness::Unknown
    } else {
        Completeness::Partial
    }
}

fn interface_identity_for<'a>(
    evidence: Option<&'a InterfaceIdentityEvidence>,
    native: &str,
) -> Option<crate::typed_facts::InterfaceSubjectIdentity> {
    evidence.and_then(|e| e.by_native_name.get(native)).cloned()
}

fn fact_for_interface_payload(
    ctx: &ObservationContext<'_>,
    source_kind: &str,
    fact_kind: &str,
    interface: &str,
    vrf: Option<&str>,
    interface_identity: Option<crate::typed_facts::InterfaceSubjectIdentity>,
    fields: Value,
    raw_output: &str,
    locator: &str,
    completeness: Completeness,
    _sequence_offset: u64,
) -> TypedFact {
    let evidence = vec![EvidenceRef {
        kind: "raw_output".to_string(),
        sha256: sha256_hex(raw_output.as_bytes()),
        locator: Some(locator.to_string()),
    }];
    let mut fact = TypedFact {
        schema_version: crate::typed_facts::SCHEMA_VERSION,
        fact_id: String::new(),
        fact_kind: fact_kind.to_string(),
        subject: FactSubject {
            device_id: ctx.device_id.to_string(),
            scope: ctx.scope.map(str::to_string),
            namespace: "interface".to_string(),
            network_namespace: ctx.network_namespace.map(str::to_string),
            entity: Some(interface.to_string()),
            vrf: vrf.map(str::to_string),
            afi_safi: None,
            plane: Some("operational".to_string()),
            interface_identity,
        },
        source: FactSource {
            collector_id: ctx.collector_id.to_string(),
            source_kind: source_kind.to_string(),
            capture_id: ctx.capture_id.to_string(),
            session_epoch: ctx.session_epoch,
            sequence: ctx.sequence,
            exporter_id: None,
        },
        observed_at: ctx.observed_at.to_string(),
        ingested_at: None,
        valid_from: None,
        valid_until: None,
        payload: FactPayload {
            produced_by: "native_neighbor_normalizer".to_string(),
            parser_revision: NEIGHBOR_PARSER_REVISION.to_string(),
            mapping_revision: Some(NEIGHBOR_MAPPING_REVISION.to_string()),
            fields: Some(fields),
            state: None,
        },
        payload_sha256: String::new(),
        evidence_sha256: String::new(),
        quality: Quality {
            completeness,
            missing_fields: vec![],
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

fn parse_json_input(input: &str) -> Result<Value, String> {
    reject_duplicate_json_keys(input.as_bytes()).map_err(|e| format!("duplicate JSON key: {e}"))?;
    serde_json::from_str(input).map_err(|e| format!("malformed JSON: {e}"))
}

fn rows(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(items)) => items.clone(),
        Some(obj @ Value::Object(_)) => vec![obj.clone()],
        _ => vec![],
    }
}

fn str_field<'a>(obj: &'a Value, key: &str) -> Option<&'a str> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn vlan_from_svi(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("Vlan")?;
    let vlan: u32 = rest.parse().ok()?;
    (vlan <= 4095).then_some(vlan)
}

fn parse_iso8601_duration_seconds(raw: &str) -> Option<u64> {
    let rest = raw.strip_prefix('P')?;
    let time = rest.strip_prefix('T').unwrap_or(rest);
    let mut num = String::new();
    let mut total = 0u64;
    let mut saw = false;
    for ch in time.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
            continue;
        }
        let value: u64 = num.parse().ok()?;
        num.clear();
        let scale = match ch {
            'D' if time == rest => 86_400,
            'H' => 3_600,
            'M' => 60,
            'S' => 1,
            _ => return None,
        };
        total = total.checked_add(value.checked_mul(scale)?)?;
        saw = true;
    }
    (saw && num.is_empty()).then_some(total)
}

fn parse_eos_arp(root: &Value, report: &mut NeighborNormalizationReport) -> Vec<ArpndRecord> {
    let mut out = Vec::new();
    let Some(vrfs) = root.get("vrfs").and_then(Value::as_object) else {
        report.malformed_records += 1;
        report
            .invalid_samples
            .push("root vrfs object absent or wrong type".into());
        return out;
    };
    let mut vrf_names: Vec<_> = vrfs.keys().cloned().collect();
    vrf_names.sort();
    for vrf in vrf_names {
        let body = &vrfs[&vrf];
        match body.get("ipV4Neighbors") {
            None => {
                report
                    .collection_presence
                    .insert(vrf.clone(), "unknown".into());
                report
                    .notes
                    .push(format!("vrf {vrf}: ipV4Neighbors collection absent"));
                continue;
            }
            Some(Value::Array(items)) if items.is_empty() => {
                report
                    .collection_presence
                    .insert(vrf.clone(), "empty".into());
                continue;
            }
            Some(Value::Array(items)) => {
                report
                    .collection_presence
                    .insert(vrf.clone(), "populated".into());
                for item in items {
                    let Some(address) = str_field(item, "address") else {
                        report.malformed_records += 1;
                        report
                            .invalid_samples
                            .push(format!("vrf {vrf}: neighbor missing address"));
                        continue;
                    };
                    if address.parse::<IpAddr>().is_err() {
                        report.malformed_records += 1;
                        report
                            .invalid_samples
                            .push(format!("vrf {vrf}: invalid address {address}"));
                        continue;
                    }
                    let Some(raw_interface) = str_field(item, "interface") else {
                        report.malformed_records += 1;
                        report
                            .invalid_samples
                            .push(format!("vrf {vrf} {address}: missing interface"));
                        continue;
                    };
                    let Some(mac_raw) = str_field(item, "hwAddress") else {
                        report.malformed_records += 1;
                        report
                            .invalid_samples
                            .push(format!("vrf {vrf} {address}: missing hwAddress"));
                        continue;
                    };
                    let Some(mac) = normalize_mac_address(mac_raw) else {
                        report.malformed_records += 1;
                        report
                            .invalid_samples
                            .push(format!("vrf {vrf} {address}: invalid hwAddress"));
                        continue;
                    };
                    let mut parts = raw_interface.split(',').map(str::trim);
                    let logical = parts.next().unwrap_or(raw_interface).to_string();
                    let physical = parts.next().filter(|p| !p.is_empty()).map(str::to_string);
                    let vlan = vlan_from_svi(&logical);
                    let age = match item.get("age") {
                        None => unknown("EOS age absent"),
                        Some(v) if v.as_u64().is_some() => {
                            ObservedField::known(v.as_u64().unwrap_or_default())
                        }
                        Some(_) => invalid("EOS age is not an unsigned integer"),
                    };
                    out.push(ArpndRecord {
                        interface: logical.clone(),
                        vrf: Some(vrf.clone()),
                        payload: ArpndNeighborPayload {
                            ip_address: address.to_string(),
                            address_family: AddressFamily::Ipv4,
                            mac_address: ObservedField::known(mac),
                            resolution_state: unknown(
                                "EOS ARP output does not expose reachability state",
                            ),
                            raw_state: None,
                            age_seconds: age,
                            outgoing_interface: logical,
                            physical_interface: physical.clone(),
                            interface_vlan_id: match vlan {
                                Some(v) => ObservedField::known(v),
                                None => unknown("interface is not a qualified Vlan<N> SVI"),
                            },
                            remote: match physical.as_deref() {
                                Some("Vxlan1") => ObservedField::known(true),
                                _ => unknown(
                                    "EOS output does not positively establish non-VXLAN locality",
                                ),
                            },
                            protocol: None,
                            router: None,
                            source_interface_raw: Some(raw_interface.to_string()),
                        },
                    });
                }
            }
            Some(_) => {
                report
                    .collection_presence
                    .insert(vrf.clone(), "malformed".into());
                report.malformed_records += 1;
                report
                    .invalid_samples
                    .push(format!("vrf {vrf}: ipV4Neighbors wrong type"));
            }
        }
    }
    out
}

fn parse_nxos_arp(root: &Value, report: &mut NeighborNormalizationReport) -> Vec<ArpndRecord> {
    let mut out = Vec::new();
    let vrf_rows = rows(root.get("TABLE_vrf").and_then(|v| v.get("ROW_vrf")));
    if vrf_rows.is_empty() {
        report
            .collection_presence
            .insert("TABLE_vrf.ROW_vrf".into(), "empty".into());
        return out;
    }
    report
        .collection_presence
        .insert("TABLE_vrf.ROW_vrf".into(), "populated".into());
    for vrf_row in vrf_rows {
        let vrf_raw = str_field(&vrf_row, "vrf-name-out");
        let vrf = vrf_raw.filter(|v| *v != "all").map(str::to_string);
        if vrf.is_none() {
            report
                .notes
                .push("NXOS vrf-name-out is aggregate/unknown; VRF left unscoped".into());
        }
        for item in rows(vrf_row.get("TABLE_adj").and_then(|v| v.get("ROW_adj"))) {
            let (Some(ip), Some(interface), Some(mac_raw)) = (
                str_field(&item, "ip-addr-out"),
                str_field(&item, "intf-out"),
                str_field(&item, "mac"),
            ) else {
                report.malformed_records += 1;
                report
                    .invalid_samples
                    .push("NXOS adjacency missing ip/intf/mac".into());
                continue;
            };
            if ip.parse::<IpAddr>().is_err() {
                report.malformed_records += 1;
                report
                    .invalid_samples
                    .push(format!("NXOS invalid address {ip}"));
                continue;
            }
            let mac = normalize_mac_address(mac_raw);
            let lower_mac = mac_raw.to_ascii_lowercase();
            let failed = mac.is_none() || lower_mac == "incomplete";
            let vlan = vlan_from_svi(interface);
            let age = match str_field(&item, "time-stamp") {
                None => unknown("NXOS time-stamp absent"),
                Some(raw) => match parse_iso8601_duration_seconds(raw) {
                    Some(seconds) => ObservedField::known(seconds),
                    None => invalid("NXOS time-stamp is not a supported ISO-8601 duration"),
                },
            };
            out.push(ArpndRecord {
                interface: interface.to_string(),
                vrf: vrf.clone(),
                payload: ArpndNeighborPayload {
                    ip_address: ip.to_string(),
                    address_family: if ip.parse::<IpAddr>().is_ok_and(|a| a.is_ipv6()) {
                        AddressFamily::Ipv6
                    } else {
                        AddressFamily::Ipv4
                    },
                    mac_address: match mac {
                        Some(mac) if !failed => ObservedField::known(mac),
                        _ => unknown("NXOS adjacency has absent/incomplete MAC evidence"),
                    },
                    resolution_state: if failed {
                        ObservedField::known(NeighborResolutionState::Failed)
                    } else {
                        unknown("NXOS ARP detail does not expose reachability state")
                    },
                    raw_state: str_field(&item, "flags").map(str::to_string),
                    age_seconds: age,
                    outgoing_interface: interface.to_string(),
                    physical_interface: str_field(&item, "phy-intf").map(str::to_string),
                    interface_vlan_id: match vlan {
                        Some(v) => ObservedField::known(v),
                        None => unknown("interface is not a qualified Vlan<N> SVI"),
                    },
                    remote: unknown("NXOS ARP output does not expose EVPN remote locality"),
                    protocol: None,
                    router: None,
                    source_interface_raw: Some(interface.to_string()),
                },
            });
        }
    }
    out
}

fn parse_cumulus_arp(input: &str, report: &mut NeighborNormalizationReport) -> Vec<ArpndRecord> {
    let mut out = Vec::new();
    report.collection_presence.insert(
        "ip-neigh-lines".into(),
        if input.trim().is_empty() {
            "empty".into()
        } else {
            "populated".into()
        },
    );
    for line in input.lines().filter(|l| !l.trim().is_empty()) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 3 {
            report.malformed_records += 1;
            report
                .invalid_samples
                .push(format!("short ip neigh line: {line}"));
            continue;
        }
        let ip = tokens[0];
        if ip.parse::<IpAddr>().is_err() {
            report.malformed_records += 1;
            report
                .invalid_samples
                .push(format!("invalid neighbor address in line: {line}"));
            continue;
        }
        let Some(dev_pos) = tokens.iter().position(|t| *t == "dev") else {
            report.malformed_records += 1;
            report
                .invalid_samples
                .push(format!("missing dev in line: {line}"));
            continue;
        };
        let Some(interface) = tokens.get(dev_pos + 1).copied() else {
            report.malformed_records += 1;
            report
                .invalid_samples
                .push(format!("missing interface in line: {line}"));
            continue;
        };
        let mac = tokens
            .iter()
            .position(|t| *t == "lladdr")
            .and_then(|p| tokens.get(p + 1))
            .and_then(|raw| normalize_mac_address(raw));
        let protocol = tokens
            .iter()
            .position(|t| *t == "proto")
            .and_then(|p| tokens.get(p + 1))
            .map(|s| s.to_string());
        let router = tokens.iter().any(|t| *t == "router");
        let state_token = tokens.iter().rev().copied().find(|t| {
            matches!(
                t.to_ascii_uppercase().as_str(),
                "REACHABLE" | "STALE" | "DELAY" | "PERMANENT" | "FAILED" | "INCOMPLETE" | "NOARP"
            )
        });
        let (state, raw_state) = match state_token {
            Some(raw) => {
                let upper = raw.to_ascii_uppercase();
                let state = match upper.as_str() {
                    "REACHABLE" | "NOARP" => NeighborResolutionState::Resolved,
                    "PERMANENT" => NeighborResolutionState::Permanent,
                    "STALE" | "DELAY" => NeighborResolutionState::Stale,
                    "FAILED" | "INCOMPLETE" => NeighborResolutionState::Failed,
                    _ => NeighborResolutionState::Unknown,
                };
                (ObservedField::known(state), Some(raw.to_string()))
            }
            None => (unknown("ip neigh state absent"), None),
        };
        out.push(ArpndRecord {
            interface: interface.to_string(),
            vrf: None,
            payload: ArpndNeighborPayload {
                ip_address: ip.to_string(),
                address_family: if ip.parse::<IpAddr>().is_ok_and(|a| a.is_ipv6()) {
                    AddressFamily::Ipv6
                } else {
                    AddressFamily::Ipv4
                },
                mac_address: match mac {
                    Some(mac) => ObservedField::known(mac),
                    None => unknown("ip neigh lladdr absent or invalid"),
                },
                resolution_state: state,
                raw_state,
                age_seconds: unknown("ip neigh output does not expose age"),
                outgoing_interface: interface.to_string(),
                physical_interface: None,
                interface_vlan_id: unknown(
                    "Linux interface names are not interpreted as VLAN evidence",
                ),
                remote: unknown("ip neigh output does not expose EVPN remote locality"),
                protocol,
                router: router.then_some(true),
                source_interface_raw: Some(interface.to_string()),
            },
        });
    }
    out
}

fn parse_eos_mac(root: &Value, report: &mut NeighborNormalizationReport) -> Vec<MacRecord> {
    let mut out = Vec::new();
    let entries_value = root.get("unicastTable").and_then(|v| v.get("tableEntries"));
    let Some(entries) = entries_value.and_then(Value::as_array) else {
        report
            .collection_presence
            .insert("unicastTable.tableEntries".into(), "unknown".into());
        report.malformed_records += 1;
        report
            .invalid_samples
            .push("EOS unicastTable.tableEntries absent or wrong type".into());
        return out;
    };
    report.collection_presence.insert(
        "unicastTable.tableEntries".into(),
        if entries.is_empty() {
            "empty".into()
        } else {
            "populated".into()
        },
    );
    for item in entries {
        let (Some(mac_raw), Some(interface), Some(entry_type_raw)) = (
            str_field(item, "macAddress"),
            str_field(item, "interface"),
            str_field(item, "entryType"),
        ) else {
            report.malformed_records += 1;
            report
                .invalid_samples
                .push("EOS MAC entry missing mac/interface/entryType".into());
            continue;
        };
        let Some(mac) = normalize_mac_address(mac_raw) else {
            report.malformed_records += 1;
            report
                .invalid_samples
                .push(format!("EOS invalid MAC {mac_raw}"));
            continue;
        };
        let vlan = match item.get("vlanId") {
            None => unknown("EOS vlanId absent"),
            Some(v) if v.as_u64().is_some_and(|n| n <= 4095) => {
                ObservedField::known(v.as_u64().unwrap_or_default() as u32)
            }
            Some(_) => invalid("EOS vlanId invalid"),
        };
        let entry_type = match entry_type_raw {
            "static" => ObservedField::known(MacEntryType::Static),
            "dynamic" => ObservedField::known(MacEntryType::Dynamic),
            _ => ObservedField::known(MacEntryType::Unknown),
        };
        out.push(MacRecord {
            interface: interface.to_string(),
            payload: MacForwardingPayload {
                mac_address: mac,
                vlan_id: vlan,
                outgoing_interface: interface.to_string(),
                normalized_outgoing_interface: None,
                entry_type,
                remote_vtep_ip: None,
                flags: vec![],
                move_count: match item.get("moves").and_then(Value::as_u64) {
                    Some(v) => ObservedField::known(v),
                    None => unknown("EOS moves absent"),
                },
                last_move_epoch_seconds: match item.get("lastMove").and_then(Value::as_f64) {
                    Some(v) => ObservedField::known(v),
                    None => unknown("EOS lastMove absent"),
                },
            },
        });
    }
    out
}

fn normalize_nxos_port(raw: &str) -> (String, Option<String>) {
    if let Some(start) = raw.strip_prefix("nve1(").and_then(|s| s.strip_suffix(')')) {
        return ("nve1".to_string(), Some(start.to_string()));
    }
    if let Some(rest) = raw.strip_prefix("Po")
        && !rest.is_empty()
        && rest.bytes().all(|b| b.is_ascii_digit())
    {
        return (format!("port-channel{rest}"), None);
    }
    (raw.to_string(), None)
}

fn parse_nxos_mac(root: &Value, report: &mut NeighborNormalizationReport) -> Vec<MacRecord> {
    let mut out = Vec::new();
    let entries = rows(
        root.get("TABLE_mac_address")
            .and_then(|v| v.get("ROW_mac_address")),
    );
    report.collection_presence.insert(
        "TABLE_mac_address.ROW_mac_address".into(),
        if entries.is_empty() {
            "empty".into()
        } else {
            "populated".into()
        },
    );
    for item in entries {
        let (Some(mac_raw), Some(port_raw)) = (
            str_field(&item, "disp_mac_addr"),
            str_field(&item, "disp_port"),
        ) else {
            report.malformed_records += 1;
            report
                .invalid_samples
                .push("NXOS MAC entry missing mac/port".into());
            continue;
        };
        let Some(mac) = normalize_mac_address(mac_raw) else {
            report.malformed_records += 1;
            report
                .invalid_samples
                .push(format!("NXOS invalid MAC {mac_raw}"));
            continue;
        };
        let vlan = match str_field(&item, "disp_vlan") {
            None | Some("-") => unknown("NXOS disp_vlan absent or dash"),
            Some(raw) => match raw.parse::<u32>() {
                Ok(v) if v <= 4095 => ObservedField::known(v),
                _ => invalid("NXOS disp_vlan is not a valid VLAN number"),
            },
        };
        let entry_type = match str_field(&item, "disp_type") {
            Some("static") => ObservedField::known(MacEntryType::Static),
            Some("dynamic") => ObservedField::known(MacEntryType::Dynamic),
            _ => ObservedField::known(MacEntryType::Unknown),
        };
        let (normalized, remote_vtep) = normalize_nxos_port(port_raw);
        out.push(MacRecord {
            interface: normalized.clone(),
            payload: MacForwardingPayload {
                mac_address: mac,
                vlan_id: vlan,
                outgoing_interface: port_raw.to_string(),
                normalized_outgoing_interface: Some(normalized),
                entry_type,
                remote_vtep_ip: remote_vtep,
                flags: vec![],
                move_count: unknown("NXOS disp_age is age, not move count"),
                last_move_epoch_seconds: unknown("NXOS output does not expose last-move epoch"),
            },
        });
    }
    out
}

fn parse_cumulus_mac(input: &str, report: &mut NeighborNormalizationReport) -> Vec<MacRecord> {
    let mut out = Vec::new();
    report.collection_presence.insert(
        "bridge-fdb-lines".into(),
        if input.trim().is_empty() {
            "empty".into()
        } else {
            "populated".into()
        },
    );
    for line in input.lines().filter(|l| !l.trim().is_empty()) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 3 {
            report.malformed_records += 1;
            report
                .invalid_samples
                .push(format!("short bridge fdb line: {line}"));
            continue;
        }
        let Some(mac) = normalize_mac_address(tokens[0]) else {
            report.malformed_records += 1;
            report
                .invalid_samples
                .push(format!("invalid bridge fdb MAC in line: {line}"));
            continue;
        };
        let Some(dev_pos) = tokens.iter().position(|t| *t == "dev") else {
            report.malformed_records += 1;
            report
                .invalid_samples
                .push(format!("missing dev in line: {line}"));
            continue;
        };
        let Some(interface) = tokens.get(dev_pos + 1).copied() else {
            report.malformed_records += 1;
            report
                .invalid_samples
                .push(format!("missing interface in line: {line}"));
            continue;
        };
        let vlan = tokens
            .iter()
            .position(|t| *t == "vlan")
            .and_then(|p| tokens.get(p + 1))
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v <= 4095);
        let flags: Vec<String> = tokens
            .iter()
            .skip(dev_pos + 2)
            .filter(|t| !matches!(**t, "vlan" | "master" | "bridge"))
            .filter(|t| t.parse::<u32>().is_err())
            .map(|s| s.to_string())
            .collect();
        let entry_type = if tokens.iter().any(|t| *t == "permanent") {
            ObservedField::known(MacEntryType::Permanent)
        } else if tokens.iter().any(|t| *t == "static" || *t == "sticky") {
            ObservedField::known(MacEntryType::Static)
        } else {
            ObservedField::known(MacEntryType::Dynamic)
        };
        out.push(MacRecord {
            interface: interface.to_string(),
            payload: MacForwardingPayload {
                mac_address: mac,
                vlan_id: match vlan {
                    Some(v) => ObservedField::known(v),
                    None => unknown("bridge fdb vlan absent"),
                },
                outgoing_interface: interface.to_string(),
                normalized_outgoing_interface: None,
                entry_type,
                remote_vtep_ip: None,
                flags,
                move_count: unknown("bridge fdb does not expose move count"),
                last_move_epoch_seconds: unknown("bridge fdb does not expose last-move epoch"),
            },
        });
    }
    out
}

fn dedup_key_arp(record: &ArpndRecord) -> String {
    serde_json::to_string(&(record.vrf.as_deref(), &record.interface, &record.payload))
        .unwrap_or_default()
}

fn dedup_key_mac(record: &MacRecord) -> String {
    serde_json::to_string(&record.payload).unwrap_or_default()
}

/// Normalize one ARP/ND capture into typed facts. Invalid records are counted
/// and omitted; no zero-MAC placeholder or reachability default is invented.
pub fn normalize_arpnd(obs: &ArpndObservation<'_>) -> NeighborNormalizationOutput {
    let mut report = NeighborNormalizationReport {
        profile: obs.profile.label().to_string(),
        capture_outcome: capture_outcome_label(obs.capture.outcome).to_string(),
        input_bytes: obs.capture.output.len(),
        ..Default::default()
    };
    if let Some(err) = context_error(&obs.context) {
        report.refusal_reason = Some(err);
        return NeighborNormalizationOutput {
            facts: vec![],
            report,
        };
    }
    if obs.capture.output.len() > MAX_INPUT_BYTES {
        report.refusal_reason = Some("input exceeds maximum byte budget before parsing".into());
        return NeighborNormalizationOutput {
            facts: vec![],
            report,
        };
    }
    if !obs.capture.outcome.usable() {
        report.refusal_reason = Some("capture outcome is not usable".into());
        return NeighborNormalizationOutput {
            facts: vec![],
            report,
        };
    }

    let mut records = match obs.profile {
        ArpndSourceProfile::EosShowArpVrfAllJson => match parse_json_input(obs.capture.output) {
            Ok(root) => parse_eos_arp(&root, &mut report),
            Err(e) => {
                report.refusal_reason = Some(e);
                vec![]
            }
        },
        ArpndSourceProfile::NxosShowIpArpDetailJsonNative => {
            match parse_json_input(obs.capture.output) {
                Ok(root) => parse_nxos_arp(&root, &mut report),
                Err(e) => {
                    report.refusal_reason = Some(e);
                    vec![]
                }
            }
        }
        ArpndSourceProfile::CumulusIpNeighShow => {
            parse_cumulus_arp(obs.capture.output, &mut report)
        }
    };
    let decoded_records = records.len();
    if decoded_records > MAX_RECORDS {
        let omitted = decoded_records - MAX_RECORDS;
        report.input_records_omitted = report.input_records_omitted.saturating_add(omitted);
        report.notes.push(format!(
            "input record budget truncated {omitted} decoded records"
        ));
        records.truncate(MAX_RECORDS);
    }
    report.records_decoded = decoded_records;
    report.empty_records = records.is_empty() && report.refusal_reason.is_none();

    let mut seen = BTreeSet::new();
    let mut facts = Vec::new();
    let comp = completeness(
        &obs.capture,
        report.malformed_records,
        report.output_facts_omitted + report.input_records_omitted,
        report.facts_rejected,
    );
    for (index, record) in records.iter().enumerate() {
        if !seen.insert(dedup_key_arp(record)) {
            report.duplicate_records += 1;
            continue;
        }
        if facts.len() >= MAX_OUTPUT_FACTS {
            report.output_facts_omitted += 1;
            continue;
        }
        let identity = interface_identity_for(obs.interface_identities, &record.interface);
        let fields = serde_json::to_value(&record.payload).unwrap_or(Value::Null);
        let fact = fact_for_interface_payload(
            &obs.context,
            obs.profile.source_kind(),
            FACT_KIND_ARPND_NEIGHBOR,
            &record.interface,
            record.vrf.as_deref(),
            identity,
            fields,
            obs.capture.output,
            obs.profile.label(),
            comp,
            index as u64,
        );
        if fact.validate().is_ok() {
            facts.push(fact);
        } else {
            report.facts_rejected += 1;
        }
    }
    if report.output_facts_omitted > 0
        || report.input_records_omitted > 0
        || report.facts_rejected > 0
    {
        for fact in &mut facts {
            if fact.quality.completeness == Completeness::Complete {
                fact.quality.completeness = Completeness::Partial;
                fact.quality
                    .notes
                    .push("normalization omitted or rejected records".into());
                fact.seal();
            }
        }
    }
    bound_invalid_samples(&mut report);
    report.facts_emitted = facts.len();
    NeighborNormalizationOutput { facts, report }
}

/// Normalize one MAC forwarding-table capture into typed facts. The facts are
/// table observations only and are not promoted to forwarding-plane proof.
pub fn normalize_mac(obs: &MacObservation<'_>) -> NeighborNormalizationOutput {
    let mut report = NeighborNormalizationReport {
        profile: obs.profile.label().to_string(),
        capture_outcome: capture_outcome_label(obs.capture.outcome).to_string(),
        input_bytes: obs.capture.output.len(),
        ..Default::default()
    };
    if let Some(err) = context_error(&obs.context) {
        report.refusal_reason = Some(err);
        return NeighborNormalizationOutput {
            facts: vec![],
            report,
        };
    }
    if obs.capture.output.len() > MAX_INPUT_BYTES {
        report.refusal_reason = Some("input exceeds maximum byte budget before parsing".into());
        return NeighborNormalizationOutput {
            facts: vec![],
            report,
        };
    }
    if !obs.capture.outcome.usable() {
        report.refusal_reason = Some("capture outcome is not usable".into());
        return NeighborNormalizationOutput {
            facts: vec![],
            report,
        };
    }

    let mut records = match obs.profile {
        MacSourceProfile::EosShowMacAddressTableUnicastJson => {
            match parse_json_input(obs.capture.output) {
                Ok(root) => parse_eos_mac(&root, &mut report),
                Err(e) => {
                    report.refusal_reason = Some(e);
                    vec![]
                }
            }
        }
        MacSourceProfile::NxosShowMacAddressTableJsonNative => {
            match parse_json_input(obs.capture.output) {
                Ok(root) => parse_nxos_mac(&root, &mut report),
                Err(e) => {
                    report.refusal_reason = Some(e);
                    vec![]
                }
            }
        }
        MacSourceProfile::CumulusBridgeFdbShow => {
            parse_cumulus_mac(obs.capture.output, &mut report)
        }
    };
    let decoded_records = records.len();
    if decoded_records > MAX_RECORDS {
        let omitted = decoded_records - MAX_RECORDS;
        report.input_records_omitted = report.input_records_omitted.saturating_add(omitted);
        report.notes.push(format!(
            "input record budget truncated {omitted} decoded records"
        ));
        records.truncate(MAX_RECORDS);
    }
    report.records_decoded = decoded_records;
    report.empty_records = records.is_empty() && report.refusal_reason.is_none();

    let mut seen = BTreeSet::new();
    let mut facts = Vec::new();
    let comp = completeness(
        &obs.capture,
        report.malformed_records,
        report.output_facts_omitted + report.input_records_omitted,
        report.facts_rejected,
    );
    for (index, record) in records.iter().enumerate() {
        if !seen.insert(dedup_key_mac(record)) {
            report.duplicate_records += 1;
            continue;
        }
        if facts.len() >= MAX_OUTPUT_FACTS {
            report.output_facts_omitted += 1;
            continue;
        }
        let identity = interface_identity_for(obs.interface_identities, &record.interface);
        let fields = serde_json::to_value(&record.payload).unwrap_or(Value::Null);
        let fact = fact_for_interface_payload(
            &obs.context,
            obs.profile.source_kind(),
            FACT_KIND_MAC_FORWARDING,
            &record.interface,
            None,
            identity,
            fields,
            obs.capture.output,
            obs.profile.label(),
            comp,
            index as u64,
        );
        if fact.validate().is_ok() {
            facts.push(fact);
        } else {
            report.facts_rejected += 1;
        }
    }
    if report.output_facts_omitted > 0
        || report.input_records_omitted > 0
        || report.facts_rejected > 0
    {
        for fact in &mut facts {
            if fact.quality.completeness == Completeness::Complete {
                fact.quality.completeness = Completeness::Partial;
                fact.quality
                    .notes
                    .push("normalization omitted or rejected records".into());
                fact.seal();
            }
        }
    }
    bound_invalid_samples(&mut report);
    report.facts_emitted = facts.len();
    NeighborNormalizationOutput { facts, report }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ObservationContext<'static> {
        ObservationContext::minimal(
            "device-leaf02",
            "collector-test",
            "capture-test",
            "2021-06-07T00:00:00+00:00",
        )
    }

    fn complete(output: &'static str) -> CommandCapture<'static> {
        CommandCapture::new(output, CaptureOutcome::Complete)
    }

    #[test]
    fn genuine_eos_arp_fixture_expected_values() {
        let input =
            include_str!("../tests/fixtures/bn024/neighbor/eos-leaf02-show-arp-vrf-all.json");
        let out = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::EosShowArpVrfAllJson,
            capture: complete(input),
            context: ctx(),
            interface_identities: None,
        });
        assert_eq!(out.report.malformed_records, 0);
        assert_eq!(out.report.facts_rejected, 0);
        assert_eq!(out.facts.len(), 7);
        let vxlan = out
            .facts
            .iter()
            .find(|f| {
                f.payload
                    .fields
                    .as_ref()
                    .is_some_and(|v| v["ip_address"] == json!("172.16.3.202"))
            })
            .unwrap();
        let p: ArpndNeighborPayload =
            serde_json::from_value(vxlan.payload.fields.clone().unwrap()).unwrap();
        assert_eq!(p.source_interface_raw.as_deref(), Some("Vlan30, Vxlan1"));
        assert_eq!(p.physical_interface.as_deref(), Some("Vxlan1"));
        assert_eq!(p.interface_vlan_id.value, Some(30));
        assert_eq!(p.remote.value, Some(true));
        assert_eq!(p.age_seconds.state, FieldAvailability::Unknown);
        assert_eq!(vxlan.subject.vrf.as_deref(), Some("evpn-vrf"));
    }

    #[test]
    fn genuine_nxos_arp_fixture_preserves_physical_and_unknown_vrf() {
        let input = include_str!(
            "../tests/fixtures/bn024/neighbor/nxos-leaf03-show-ip-arp-detail-vrf-all.json"
        );
        let out = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::NxosShowIpArpDetailJsonNative,
            capture: complete(input),
            context: ctx(),
            interface_identities: None,
        });
        assert_eq!(out.report.malformed_records, 0);
        assert_eq!(out.facts.len(), 6);
        let svifact = out
            .facts
            .iter()
            .find(|f| {
                f.payload
                    .fields
                    .as_ref()
                    .is_some_and(|v| v["ip_address"] == json!("172.16.3.202"))
            })
            .unwrap();
        let p: ArpndNeighborPayload =
            serde_json::from_value(svifact.payload.fields.clone().unwrap()).unwrap();
        assert_eq!(p.outgoing_interface, "Vlan30");
        assert_eq!(p.physical_interface.as_deref(), Some("port-channel4"));
        assert_eq!(p.age_seconds.value, Some(23));
        assert_eq!(svifact.subject.vrf, None);
    }

    #[test]
    fn genuine_cumulus_arp_fixture_preserves_ipv6_router_and_permanent_states() {
        let input =
            include_str!("../tests/fixtures/bn024/neighbor/cumulus-spine02-ip-neigh-show.txt");
        let out = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::CumulusIpNeighShow,
            capture: complete(input),
            context: ctx(),
            interface_identities: None,
        });
        assert_eq!(out.report.malformed_records, 0);
        assert_eq!(out.facts.len(), 13);
        assert!(out.facts.iter().any(|f| {
            f.payload
                .fields
                .as_ref()
                .is_some_and(|v| v["address_family"] == json!("ipv6") && v["router"] == json!(true))
        }));
        assert!(out.facts.iter().any(|f| {
            f.payload.fields.as_ref().is_some_and(|v| {
                v["resolution_state"]["value"] == json!("permanent")
                    && v["protocol"] == json!("zebra")
            })
        }));
    }

    #[test]
    fn genuine_eos_mac_fixture_expected_values() {
        let input = include_str!(
            "../tests/fixtures/bn024/neighbor/eos-leaf02-show-mac-address-table-unicast.json"
        );
        let out = normalize_mac(&MacObservation {
            profile: MacSourceProfile::EosShowMacAddressTableUnicastJson,
            capture: complete(input),
            context: ctx(),
            interface_identities: None,
        });
        assert_eq!(out.report.malformed_records, 0);
        assert_eq!(out.facts.len(), 11);
        assert!(out.facts.iter().any(|f| {
            f.payload.fields.as_ref().is_some_and(|v| {
                v["mac_address"] == json!("d2:74:8c:66:e7:e2")
                    && v["vlan_id"]["value"] == json!(30)
                    && v["outgoing_interface"] == json!("Vxlan1")
            })
        }));
    }

    #[test]
    fn genuine_nxos_mac_fixture_separates_nve_evidence_without_overinterpretation() {
        let input = include_str!(
            "../tests/fixtures/bn024/neighbor/nxos-leaf03-show-mac-address-table.json"
        );
        let out = normalize_mac(&MacObservation {
            profile: MacSourceProfile::NxosShowMacAddressTableJsonNative,
            capture: complete(input),
            context: ctx(),
            interface_identities: None,
        });
        assert_eq!(out.report.malformed_records, 0);
        assert_eq!(out.facts.len(), 15);
        assert!(out.facts.iter().any(|f| {
            f.payload.fields.as_ref().is_some_and(|v| {
                v["outgoing_interface"] == json!("nve1(10.0.0.112)")
                    && v["remote_vtep_ip"] == json!("10.0.0.112")
            })
        }));
        assert!(out.facts.iter().any(|f| {
            f.payload
                .fields
                .as_ref()
                .is_some_and(|v| v["outgoing_interface"] == json!("vPC Peer-Link(R)"))
        }));
    }

    #[test]
    fn genuine_cumulus_mac_fixture_preserves_vlan_and_static_permanent_distinction() {
        let input =
            include_str!("../tests/fixtures/bn024/neighbor/cumulus-leaf01-bridge-fdb-show.txt");
        let out = normalize_mac(&MacObservation {
            profile: MacSourceProfile::CumulusBridgeFdbShow,
            capture: complete(input),
            context: ctx(),
            interface_identities: None,
        });
        assert_eq!(out.report.malformed_records, 0);
        assert_eq!(out.facts.len(), 9);
        assert!(out.facts.iter().any(|f| {
            f.payload.fields.as_ref().is_some_and(|v| {
                v["vlan_id"]["value"] == json!(24) && v["entry_type"]["value"] == json!("static")
            })
        }));
        assert!(out.facts.iter().any(|f| {
            f.payload.fields.as_ref().is_some_and(|v| {
                v["vlan_id"]["state"] == json!("unknown")
                    && v["entry_type"]["value"] == json!("permanent")
            })
        }));
    }

    #[test]
    fn failed_neighbor_does_not_fabricate_zero_mac() {
        let input = "10.0.0.9 dev swp1 FAILED\n";
        let out = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::CumulusIpNeighShow,
            capture: complete(input),
            context: ctx(),
            interface_identities: None,
        });
        assert_eq!(out.facts.len(), 1);
        let p: ArpndNeighborPayload =
            serde_json::from_value(out.facts[0].payload.fields.clone().unwrap()).unwrap();
        assert_eq!(
            p.resolution_state.value,
            Some(NeighborResolutionState::Failed)
        );
        assert_eq!(p.mac_address.state, FieldAvailability::Unknown);
        assert!(p.mac_address.value.is_none());
    }

    #[test]
    fn eos_reordered_source_arrays_preserve_semantic_identity_sets() {
        use std::collections::BTreeSet;
        let input =
            include_str!("../tests/fixtures/bn024/neighbor/eos-leaf02-show-arp-vrf-all.json");
        let semantic_key = |f: &TypedFact| {
            serde_json::to_string(&(&f.subject.entity, &f.subject.vrf, &f.payload.fields)).unwrap()
        };
        let normalize_keys = |text: &str| {
            normalize_arpnd(&ArpndObservation {
                profile: ArpndSourceProfile::EosShowArpVrfAllJson,
                capture: CommandCapture::new(text, CaptureOutcome::Complete),
                context: ctx(),
                interface_identities: None,
            })
            .facts
            .iter()
            .map(semantic_key)
            .collect::<BTreeSet<_>>()
        };
        let before = normalize_keys(input);
        let mut root: Value = serde_json::from_str(input).unwrap();
        for vrf in root["vrfs"].as_object_mut().unwrap().values_mut() {
            if let Some(items) = vrf["ipV4Neighbors"].as_array_mut() {
                items.reverse();
            }
        }
        let reordered = serde_json::to_string(&root).unwrap();
        let after = normalize_keys(&reordered);
        // Reordered bytes are distinct raw evidence, so typed fact/evidence
        // digests may legitimately differ. Semantic neighbor identity and
        // population classification must not: array position is not identity.
        assert_eq!(before, after);

        let mac_input = include_str!(
            "../tests/fixtures/bn024/neighbor/eos-leaf02-show-mac-address-table-unicast.json"
        );
        let normalize_mac_keys = |text: &str| {
            normalize_mac(&MacObservation {
                profile: MacSourceProfile::EosShowMacAddressTableUnicastJson,
                capture: CommandCapture::new(text, CaptureOutcome::Complete),
                context: ctx(),
                interface_identities: None,
            })
            .facts
            .iter()
            .map(semantic_key)
            .collect::<BTreeSet<_>>()
        };
        let before = normalize_mac_keys(mac_input);
        let mut root: Value = serde_json::from_str(mac_input).unwrap();
        root["unicastTable"]["tableEntries"]
            .as_array_mut()
            .unwrap()
            .reverse();
        let reordered = serde_json::to_string(&root).unwrap();
        assert_eq!(before, normalize_mac_keys(&reordered));
    }

    #[test]
    fn malformed_input_is_refusal_not_empty_success() {
        let out = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::EosShowArpVrfAllJson,
            capture: complete("{\"vrfs\":"),
            context: ctx(),
            interface_identities: None,
        });
        assert!(out.facts.is_empty());
        assert!(out.report.refusal_reason.is_some());
    }

    #[test]
    fn duplicate_json_keys_are_rejected() {
        let input =
            "{\"vrfs\":{\"default\":{\"ipV4Neighbors\":[]},\"default\":{\"ipV4Neighbors\":[]}}}";
        let out = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::EosShowArpVrfAllJson,
            capture: complete(input),
            context: ctx(),
            interface_identities: None,
        });
        assert!(
            out.report
                .refusal_reason
                .is_some_and(|r| r.contains("duplicate"))
        );
    }
}
