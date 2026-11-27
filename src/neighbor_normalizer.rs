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
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use crate::interface_facts::{
    FieldAvailability, ObservedField, normalize_mac as normalize_mac_address,
};
use crate::interface_normalizer::{CaptureOutcome, CommandCapture, ObservationContext};
use crate::neighbor_facts::{
    AddressFamily, ArpndNeighborPayload, FACT_KIND_ARPND_NEIGHBOR, FACT_KIND_MAC_FORWARDING,
    MacEntryContext, MacEntryType, MacForwardingPayload, NeighborResolutionState,
    arpnd_attachment_entity_key, mac_forwarding_entity_key,
};
use crate::semantic_evidence::{
    AcquisitionScopeContract, ScopedCaptureCoverage, ScopedPopulationCoverage,
};
use crate::typed_facts::{
    Completeness, EvidenceRef, FactPayload, FactSource, FactSubject, InterfaceIdentityKind,
    Lifecycle, Quality, TrustClass, TypedFact,
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
    LinuxIpNeighShow,
}

impl ArpndSourceProfile {
    pub fn label(self) -> &'static str {
        match self {
            Self::EosShowArpVrfAllJson => "eos/show-arp-vrf-all-json",
            Self::NxosShowIpArpDetailJsonNative => "nxos/show-ip-arp-detail-vrf-all-json-native",
            Self::CumulusIpNeighShow => "cumulus/ip-neigh-show",
            Self::LinuxIpNeighShow => "linux/ip-neigh-show",
        }
    }
    fn source_kind(self) -> &'static str {
        match self {
            Self::EosShowArpVrfAllJson | Self::NxosShowIpArpDetailJsonNative => {
                "structured_cli_json"
            }
            Self::CumulusIpNeighShow | Self::LinuxIpNeighShow => "cli_text",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacSourceProfile {
    EosShowMacAddressTableUnicastJson,
    NxosShowMacAddressTableJsonNative,
    CumulusBridgeFdbShow,
    LinuxBridgeFdbShow,
}

impl MacSourceProfile {
    pub fn label(self) -> &'static str {
        match self {
            Self::EosShowMacAddressTableUnicastJson => "eos/show-mac-address-table-unicast-json",
            Self::NxosShowMacAddressTableJsonNative => "nxos/show-mac-address-table-json-native",
            Self::CumulusBridgeFdbShow => "cumulus/bridge-fdb-show",
            Self::LinuxBridgeFdbShow => "linux/bridge-fdb-show",
        }
    }
    fn source_kind(self) -> &'static str {
        match self {
            Self::EosShowMacAddressTableUnicastJson | Self::NxosShowMacAddressTableJsonNative => {
                "structured_cli_json"
            }
            Self::CumulusBridgeFdbShow | Self::LinuxBridgeFdbShow => "cli_text",
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
    pub invalid_samples_omitted: usize,
    pub notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal_reason: Option<String>,
}

pub struct NeighborNormalizationOutput {
    pub facts: Vec<TypedFact>,
    pub report: NeighborNormalizationReport,
    /// Actual acquisition/population coverage derived after decoding,
    /// validation, omission accounting and final empty classification. This
    /// object exists even when the validated population is empty.
    pub coverage: ScopedCaptureCoverage,
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

fn push_invalid_sample(report: &mut NeighborNormalizationReport, sample: impl Into<String>) {
    if report.invalid_samples.len() < MAX_INVALID_SAMPLES {
        let mut sample = sample.into();
        if sample.len() > 512 {
            sample.truncate(512);
            sample.push_str("…<truncated diagnostic>");
        }
        report.invalid_samples.push(sample);
    } else {
        report.invalid_samples_omitted += 1;
    }
}

fn bound_invalid_samples(report: &mut NeighborNormalizationReport) {
    if report.invalid_samples.len() > MAX_INVALID_SAMPLES {
        let overflow = report.invalid_samples.len() - MAX_INVALID_SAMPLES;
        report.invalid_samples.truncate(MAX_INVALID_SAMPLES);
        report.invalid_samples_omitted += overflow;
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
    raw_sha256: &str,
    locator: &str,
    completeness: Completeness,
    _sequence_offset: u64,
) -> TypedFact {
    let evidence = vec![EvidenceRef {
        kind: "raw_output".to_string(),
        sha256: raw_sha256.to_string(),
        locator: Some(locator.to_string()),
    }];
    let entity = match interface_identity.as_ref() {
        Some(identity)
            if identity.kind == InterfaceIdentityKind::Subinterface
                && identity.parent_entity.is_some() =>
        {
            identity.parent_entity.clone().unwrap_or_default()
        }
        _ => interface.to_string(),
    };
    let mut fact = TypedFact {
        schema_version: crate::typed_facts::SCHEMA_VERSION,
        fact_id: String::new(),
        fact_kind: fact_kind.to_string(),
        subject: FactSubject {
            device_id: ctx.device_id.to_string(),
            scope: ctx.scope.map(str::to_string),
            namespace: "interface".to_string(),
            network_namespace: ctx.network_namespace.map(str::to_string),
            entity: Some(entity),
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
        // The observation is valid evidence at least at its observation
        // instant. No expiry is invented; consumers apply explicit freshness
        // policy when the source cannot establish one.
        valid_from: Some(ctx.observed_at.to_string()),
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

fn push_arp_record(
    out: &mut Vec<ArpndRecord>,
    report: &mut NeighborNormalizationReport,
    record: ArpndRecord,
) {
    report.records_decoded += 1;
    if out.len() < MAX_RECORDS {
        out.push(record);
    } else {
        report.input_records_omitted += 1;
    }
}

fn push_mac_record(
    out: &mut Vec<MacRecord>,
    report: &mut NeighborNormalizationReport,
    record: MacRecord,
) {
    report.records_decoded += 1;
    if out.len() < MAX_RECORDS {
        out.push(record);
    } else {
        report.input_records_omitted += 1;
    }
}

fn sample(report: &mut NeighborNormalizationReport, value: impl Into<String>) {
    push_invalid_sample(report, value);
}

fn parse_eos_arp(root: &Value, report: &mut NeighborNormalizationReport) -> Vec<ArpndRecord> {
    let mut out = Vec::new();
    let Some(vrfs) = root.get("vrfs").and_then(Value::as_object) else {
        report.malformed_records += 1;
        sample(report, "root vrfs object absent or wrong type");
        report
            .collection_presence
            .insert("vrfs".into(), "missing_or_malformed".into());
        return out;
    };
    if vrfs.is_empty() {
        // An empty root object is not positive evidence that every VRF's
        // neighbor collection is empty: the output does not establish which
        // VRF collections were present at all. Keep that acquisition-scope
        // ignorance explicit instead of manufacturing an empty global table.
        report
            .collection_presence
            .insert("vrfs".into(), "unknown_scope".into());
        report
            .notes
            .push("EOS vrfs object is empty; per-VRF collection presence unknown".into());
    }
    let mut vrf_names: Vec<_> = vrfs.keys().cloned().collect();
    vrf_names.sort();
    for vrf in vrf_names {
        let body = &vrfs[&vrf];
        match body.get("ipV4Neighbors") {
            None => {
                report
                    .collection_presence
                    .insert(vrf.clone(), "missing".into());
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
                        sample(report, format!("vrf {vrf}: neighbor missing address"));
                        continue;
                    };
                    if address.parse::<IpAddr>().is_err() {
                        report.malformed_records += 1;
                        sample(report, format!("vrf {vrf}: invalid address {address}"));
                        continue;
                    }
                    let Some(raw_interface) = str_field(item, "interface") else {
                        report.malformed_records += 1;
                        sample(report, format!("vrf {vrf} {address}: missing interface"));
                        continue;
                    };
                    // EOS incomplete/unresolved entries can legitimately lack a
                    // usable hardware address. Preserve the neighbor evidence;
                    // absence/invalidity of the MAC is not a protocol failure.
                    let (mac, mac_note) = match str_field(item, "hwAddress") {
                        None => (
                            unknown(
                                "EOS hwAddress absent; incomplete/unresolved evidence retained",
                            ),
                            Some(format!("vrf {vrf} {address}: hwAddress absent")),
                        ),
                        Some(raw) => match normalize_mac_address(raw) {
                            Some(mac) => (ObservedField::known(mac), None),
                            None => (
                                invalid("EOS hwAddress is not a valid MAC"),
                                Some(format!("vrf {vrf} {address}: invalid hwAddress")),
                            ),
                        },
                    };
                    if let Some(note) = mac_note {
                        report.notes.push(note);
                    }
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
                    push_arp_record(
                        &mut out,
                        report,
                        ArpndRecord {
                            interface: logical.clone(),
                            vrf: Some(vrf.clone()),
                            payload: ArpndNeighborPayload {
                                ip_address: address.to_string(),
                                address_family: AddressFamily::Ipv4,
                                mac_address: mac,
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
                                neighbor_entity_key: None,
                                forwarding_domain: unknown(
                                    "EOS ARP output does not establish an L2 forwarding domain",
                                ),
                            },
                        },
                    );
                }
            }
            Some(_) => {
                report
                    .collection_presence
                    .insert(vrf.clone(), "malformed".into());
                report.malformed_records += 1;
                sample(report, format!("vrf {vrf}: ipV4Neighbors wrong type"));
            }
        }
    }
    out
}

#[derive(Debug)]
enum RowCollection {
    Rows(Vec<Value>),
    Empty,
    Missing,
    Malformed,
}

fn row_collection(value: Option<&Value>) -> RowCollection {
    match value {
        Some(Value::Array(items)) if items.is_empty() => RowCollection::Empty,
        Some(Value::Array(items)) => RowCollection::Rows(items.clone()),
        Some(obj @ Value::Object(_)) => RowCollection::Rows(vec![obj.clone()]),
        None => RowCollection::Missing,
        Some(_) => RowCollection::Malformed,
    }
}

fn nxos_rows(
    value: Option<&Value>,
    label: &str,
    report: &mut NeighborNormalizationReport,
) -> Vec<Value> {
    match row_collection(value) {
        RowCollection::Rows(rows) => {
            report
                .collection_presence
                .insert(label.into(), "populated".into());
            rows
        }
        RowCollection::Empty => {
            report
                .collection_presence
                .insert(label.into(), "empty".into());
            vec![]
        }
        RowCollection::Missing => {
            report
                .collection_presence
                .insert(label.into(), "missing".into());
            report.notes.push(format!("NXOS collection {label} absent"));
            vec![]
        }
        RowCollection::Malformed => {
            report
                .collection_presence
                .insert(label.into(), "malformed".into());
            report.malformed_records += 1;
            sample(
                report,
                format!("NXOS collection {label} has unsupported shape"),
            );
            vec![]
        }
    }
}

fn parse_nxos_arp(root: &Value, report: &mut NeighborNormalizationReport) -> Vec<ArpndRecord> {
    let mut out = Vec::new();
    let vrf_rows = nxos_rows(
        root.get("TABLE_vrf").and_then(|v| v.get("ROW_vrf")),
        "TABLE_vrf.ROW_vrf",
        report,
    );
    for vrf_row in vrf_rows {
        let vrf_raw = str_field(&vrf_row, "vrf-name-out");
        let vrf = vrf_raw.filter(|v| *v != "all").map(str::to_string);
        if vrf.is_none() {
            report
                .notes
                .push("NXOS vrf-name-out is aggregate/unknown; VRF left unscoped".into());
        }
        let adjacency = nxos_rows(
            vrf_row.get("TABLE_adj").and_then(|v| v.get("ROW_adj")),
            "TABLE_adj.ROW_adj",
            report,
        );
        for item in adjacency {
            let (Some(ip), Some(interface)) = (
                str_field(&item, "ip-addr-out"),
                str_field(&item, "intf-out"),
            ) else {
                report.malformed_records += 1;
                sample(report, "NXOS adjacency missing ip/intf");
                continue;
            };
            if ip.parse::<IpAddr>().is_err() {
                report.malformed_records += 1;
                sample(report, format!("NXOS invalid address {ip}"));
                continue;
            }
            let mac_raw = str_field(&item, "mac");
            let mac = mac_raw.and_then(normalize_mac_address);
            let explicit_incomplete =
                mac_raw.is_some_and(|raw| raw.eq_ignore_ascii_case("incomplete"));
            let vlan = vlan_from_svi(interface);
            let age = match str_field(&item, "time-stamp") {
                None => unknown("NXOS time-stamp absent"),
                Some(raw) => match parse_iso8601_duration_seconds(raw) {
                    Some(seconds) => ObservedField::known(seconds),
                    None => invalid("NXOS time-stamp is not a supported ISO-8601 duration"),
                },
            };
            let (mac_field, state) = if explicit_incomplete {
                (
                    unknown("NXOS adjacency is explicitly incomplete"),
                    ObservedField::known(NeighborResolutionState::Incomplete),
                )
            } else if let Some(mac) = mac {
                (
                    ObservedField::known(mac),
                    unknown("NXOS ARP detail does not expose reachability state"),
                )
            } else {
                (
                    match mac_raw {
                        Some(_) => invalid("NXOS MAC text is not a valid MAC"),
                        None => unknown("NXOS MAC absent"),
                    },
                    unknown(
                        "NXOS MAC syntax/presence does not establish neighbor resolution state",
                    ),
                )
            };
            push_arp_record(
                &mut out,
                report,
                ArpndRecord {
                    interface: interface.to_string(),
                    vrf: vrf.clone(),
                    payload: ArpndNeighborPayload {
                        ip_address: ip.to_string(),
                        address_family: if ip.parse::<IpAddr>().is_ok_and(|a| a.is_ipv6()) {
                            AddressFamily::Ipv6
                        } else {
                            AddressFamily::Ipv4
                        },
                        mac_address: mac_field,
                        resolution_state: state,
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
                        neighbor_entity_key: None,
                        forwarding_domain: unknown(
                            "NXOS ARP output does not establish an L2 forwarding domain",
                        ),
                    },
                },
            );
        }
    }
    out
}

fn linux_neighbor_state(raw: &str) -> NeighborResolutionState {
    match raw.to_ascii_uppercase().as_str() {
        "REACHABLE" => NeighborResolutionState::Reachable,
        "STALE" => NeighborResolutionState::Stale,
        "DELAY" => NeighborResolutionState::Delay,
        "PROBE" => NeighborResolutionState::Probe,
        "INCOMPLETE" => NeighborResolutionState::Incomplete,
        "FAILED" => NeighborResolutionState::Failed,
        "PERMANENT" => NeighborResolutionState::Permanent,
        "NOARP" => NeighborResolutionState::NoArp,
        _ => NeighborResolutionState::Unknown,
    }
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
            sample(report, format!("short ip neigh line: {line}"));
            continue;
        }
        let ip = tokens[0];
        if ip.parse::<IpAddr>().is_err() {
            report.malformed_records += 1;
            sample(report, format!("invalid neighbor address in line: {line}"));
            continue;
        }
        let Some(dev_pos) = tokens.iter().position(|t| *t == "dev") else {
            report.malformed_records += 1;
            sample(report, format!("missing dev in line: {line}"));
            continue;
        };
        let Some(interface) = tokens.get(dev_pos + 1).copied() else {
            report.malformed_records += 1;
            sample(report, format!("missing interface in line: {line}"));
            continue;
        };
        let lladdr_raw = tokens
            .iter()
            .position(|t| *t == "lladdr")
            .and_then(|p| tokens.get(p + 1));
        let mac = lladdr_raw.and_then(|raw| normalize_mac_address(raw));
        let protocol = tokens
            .iter()
            .position(|t| *t == "proto")
            .and_then(|p| tokens.get(p + 1))
            .map(|s| s.to_string());
        let router = tokens.iter().any(|t| *t == "router");
        let state_token = tokens.iter().rev().copied().find(|t| {
            matches!(
                t.to_ascii_uppercase().as_str(),
                "REACHABLE"
                    | "STALE"
                    | "DELAY"
                    | "PROBE"
                    | "PERMANENT"
                    | "FAILED"
                    | "INCOMPLETE"
                    | "NOARP"
            )
        });
        let (state, raw_state) = match state_token {
            Some(raw) => (
                ObservedField::known(linux_neighbor_state(raw)),
                Some(raw.to_string()),
            ),
            None => (unknown("ip neigh state absent"), None),
        };
        push_arp_record(
            &mut out,
            report,
            ArpndRecord {
                interface: interface.to_string(),
                vrf: None,
                payload: ArpndNeighborPayload {
                    ip_address: ip.to_string(),
                    address_family: if ip.parse::<IpAddr>().is_ok_and(|a| a.is_ipv6()) {
                        AddressFamily::Ipv6
                    } else {
                        AddressFamily::Ipv4
                    },
                    mac_address: match (lladdr_raw, mac) {
                        (Some(_), Some(mac)) => ObservedField::known(mac),
                        (Some(_), None) => invalid("ip neigh lladdr is not a valid MAC"),
                        (None, _) => unknown("ip neigh lladdr absent"),
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
                    neighbor_entity_key: None,
                    forwarding_domain: unknown(
                        "ip neigh output does not establish an L2 forwarding domain",
                    ),
                },
            },
        );
    }
    out
}

fn parse_eos_mac(root: &Value, report: &mut NeighborNormalizationReport) -> Vec<MacRecord> {
    let mut out = Vec::new();
    let entries_value = root.get("unicastTable").and_then(|v| v.get("tableEntries"));
    let Some(entries) = entries_value.and_then(Value::as_array) else {
        let state = if entries_value.is_some() {
            "malformed"
        } else {
            "missing"
        };
        report
            .collection_presence
            .insert("unicastTable.tableEntries".into(), state.into());
        if entries_value.is_some() {
            report.malformed_records += 1;
            sample(report, "EOS unicastTable.tableEntries wrong type");
        } else {
            report
                .notes
                .push("EOS unicastTable.tableEntries absent".into());
        }
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
            sample(report, "EOS MAC entry missing mac/interface/entryType");
            continue;
        };
        let Some(mac) = normalize_mac_address(mac_raw) else {
            report.malformed_records += 1;
            sample(report, format!("EOS invalid MAC {mac_raw}"));
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
        push_mac_record(
            &mut out,
            report,
            MacRecord {
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
                    bridge_domain: unknown("EOS MAC output does not expose bridge-domain identity"),
                    entry_contexts: if interface == "Vxlan1" {
                        vec![MacEntryContext::Remote]
                    } else {
                        vec![]
                    },
                    forwarding_entity_key: None,
                },
            },
        );
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
    let entries = nxos_rows(
        root.get("TABLE_mac_address")
            .and_then(|v| v.get("ROW_mac_address")),
        "TABLE_mac_address.ROW_mac_address",
        report,
    );
    for item in entries {
        let (Some(mac_raw), Some(port_raw)) = (
            str_field(&item, "disp_mac_addr"),
            str_field(&item, "disp_port"),
        ) else {
            report.malformed_records += 1;
            sample(report, "NXOS MAC entry missing mac/port");
            continue;
        };
        let Some(mac) = normalize_mac_address(mac_raw) else {
            report.malformed_records += 1;
            sample(report, format!("NXOS invalid MAC {mac_raw}"));
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
        let mut contexts = Vec::new();
        if remote_vtep.is_some() {
            contexts.push(MacEntryContext::Remote);
        }
        if port_raw.ends_with("(R)") {
            contexts.push(MacEntryContext::Special);
        }
        push_mac_record(
            &mut out,
            report,
            MacRecord {
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
                    bridge_domain: unknown(
                        "NXOS MAC output does not expose bridge-domain identity",
                    ),
                    entry_contexts: contexts,
                    forwarding_entity_key: None,
                },
            },
        );
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
            sample(report, format!("short bridge fdb line: {line}"));
            continue;
        }
        let Some(mac) = normalize_mac_address(tokens[0]) else {
            report.malformed_records += 1;
            sample(report, format!("invalid bridge fdb MAC in line: {line}"));
            continue;
        };
        let Some(dev_pos) = tokens.iter().position(|t| *t == "dev") else {
            report.malformed_records += 1;
            sample(report, format!("missing dev in line: {line}"));
            continue;
        };
        let Some(interface) = tokens.get(dev_pos + 1).copied() else {
            report.malformed_records += 1;
            sample(report, format!("missing interface in line: {line}"));
            continue;
        };

        let mut vlan = None;
        let mut master_name = None;
        let mut remote_vtep = None;
        let mut flags = Vec::new();
        let mut contexts = BTreeSet::new();
        let mut saw_permanent = false;
        let mut saw_static = false;
        let mut saw_stale = false;
        let mut saw_dynamic = false;
        let mut i = dev_pos + 2;
        while i < tokens.len() {
            match tokens[i] {
                "vlan" => {
                    if let Some(raw) = tokens.get(i + 1) {
                        vlan = raw.parse::<u32>().ok().filter(|v| *v <= 4095);
                        i += 2;
                    } else {
                        flags.push("vlan-missing-value".into());
                        i += 1;
                    }
                }
                "master" => {
                    contexts.insert(MacEntryContext::MasterTable);
                    if let Some(name) = tokens.get(i + 1) {
                        master_name = Some((*name).to_string());
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                "self" => {
                    contexts.insert(MacEntryContext::SelfTable);
                    i += 1;
                }
                "extern_learn" => {
                    contexts.insert(MacEntryContext::ExternallyLearned);
                    flags.push("extern_learn".into());
                    i += 1;
                }
                "offload" => {
                    contexts.insert(MacEntryContext::Offloaded);
                    flags.push("offload".into());
                    i += 1;
                }
                "router" => {
                    contexts.insert(MacEntryContext::Router);
                    flags.push("router".into());
                    i += 1;
                }
                "dst" => {
                    if let Some(ip) = tokens.get(i + 1) {
                        remote_vtep = Some((*ip).to_string());
                        contexts.insert(MacEntryContext::Remote);
                        i += 2;
                    } else {
                        flags.push("dst-missing-value".into());
                        i += 1;
                    }
                }
                "permanent" | "local" => {
                    saw_permanent = true;
                    contexts.insert(MacEntryContext::Local);
                    flags.push(tokens[i].into());
                    i += 1;
                }
                "static" | "sticky" => {
                    saw_static = true;
                    flags.push(tokens[i].into());
                    i += 1;
                }
                "stale" => {
                    saw_stale = true;
                    flags.push("stale".into());
                    i += 1;
                }
                "dynamic" => {
                    saw_dynamic = true;
                    flags.push("dynamic".into());
                    i += 1;
                }
                other => {
                    flags.push(other.into());
                    if !other.parse::<u32>().is_ok() {
                        contexts.insert(MacEntryContext::Other);
                    }
                    i += 1;
                }
            }
        }
        // iproute2 prints no state token for a reachable FDB entry. For bridge
        // FDB, reachable without the static/NOARP or permanent/local bit is
        // the dynamic learn state; stale is reachability state, not a type.
        let entry_type = if saw_permanent {
            ObservedField::known(MacEntryType::Permanent)
        } else if saw_static {
            ObservedField::known(MacEntryType::Static)
        } else if saw_stale {
            ObservedField::known(MacEntryType::Unknown)
        } else {
            // iproute2 prints no state token for NUD_REACHABLE. In bridge FDB
            // semantics, reachable without static/NOARP or permanent/local is
            // the dynamic learn state. An explicit dynamic token is therefore
            // corroborating evidence, not a required marker.
            let _ = saw_dynamic;
            ObservedField::known(MacEntryType::Dynamic)
        };
        push_mac_record(
            &mut out,
            report,
            MacRecord {
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
                    remote_vtep_ip: remote_vtep,
                    flags,
                    move_count: unknown("bridge fdb does not expose move count"),
                    last_move_epoch_seconds: unknown("bridge fdb does not expose last-move epoch"),
                    bridge_domain: match master_name {
                        Some(name) => ObservedField::known(name),
                        None => unknown("bridge fdb master/bridge identity absent"),
                    },
                    entry_contexts: contexts.into_iter().collect(),
                    forwarding_entity_key: None,
                },
            },
        );
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

fn canonical_interface_entity(
    interface: &str,
    identity: Option<&crate::typed_facts::InterfaceSubjectIdentity>,
) -> String {
    match identity {
        Some(identity)
            if identity.kind == InterfaceIdentityKind::Subinterface
                && identity.parent_entity.is_some() =>
        {
            identity.parent_entity.clone().unwrap_or_default()
        }
        _ => interface.to_string(),
    }
}

fn coverage_population(
    capture: &CommandCapture<'_>,
    report: &NeighborNormalizationReport,
) -> ScopedPopulationCoverage {
    if report.refusal_reason.is_some() || capture.outcome == CaptureOutcome::Unknown {
        return ScopedPopulationCoverage::Unknown;
    }
    let mut saw_positive_collection = false;
    let mut saw_missing_or_malformed_collection = false;
    for state in report.collection_presence.values() {
        match state.as_str() {
            "empty" | "populated" => saw_positive_collection = true,
            "missing" | "malformed" | "missing_or_malformed" | "unsupported" => {
                saw_missing_or_malformed_collection = true
            }
            _ => saw_missing_or_malformed_collection = true,
        }
    }
    if capture.outcome == CaptureOutcome::Complete
        && report.malformed_records == 0
        && report.input_records_omitted == 0
        && report.output_facts_omitted == 0
        && report.facts_rejected == 0
        && !saw_missing_or_malformed_collection
    {
        ScopedPopulationCoverage::Complete
    } else if saw_missing_or_malformed_collection
        && !saw_positive_collection
        && report.records_decoded == 0
    {
        // A missing/unsupported table container is not positive evidence of an
        // empty or partially decoded population. Keep that ignorance explicit.
        ScopedPopulationCoverage::Unknown
    } else {
        ScopedPopulationCoverage::Partial
    }
}

fn positive_empty_population(report: &NeighborNormalizationReport) -> bool {
    if report.records_decoded != 0
        || report.malformed_records != 0
        || report.input_records_omitted != 0
        || report.refusal_reason.is_some()
    {
        return false;
    }
    let mut saw_empty = false;
    for state in report.collection_presence.values() {
        match state.as_str() {
            "empty" => saw_empty = true,
            "populated" => {}
            _ => return false,
        }
    }
    saw_empty
}

fn declared_scope_contract(profile: &str) -> AcquisitionScopeContract {
    match profile {
        // These qualified commands/subscriptions are whole-table observations
        // within the explicitly supplied device/scope/network namespace. This
        // is a property of the qualified acquisition contract, not of whether
        // any rows happened to decode.
        "eos/show-arp-vrf-all-json"
        | "nxos/show-ip-arp-detail-vrf-all-json-native"
        | "cumulus/ip-neigh-show"
        | "eos/show-mac-address-table-unicast-json"
        | "nxos/show-mac-address-table-json-native"
        | "cumulus/bridge-fdb-show" => AcquisitionScopeContract::CompleteScopedTable,
        _ => AcquisitionScopeContract::Unknown,
    }
}

fn scoped_coverage(
    ctx: &ObservationContext<'_>,
    capture: &CommandCapture<'_>,
    source_kind: &str,
    profile: &str,
    covered_fact_kind: &str,
    report: &NeighborNormalizationReport,
    facts: &[TypedFact],
    raw_sha256: Option<&str>,
) -> ScopedCaptureCoverage {
    let mut observed_interfaces = BTreeSet::new();
    let mut observed_forwarding_domains = BTreeSet::new();
    let mut observed_vlan_ids = BTreeSet::new();
    for fact in facts {
        if let Some(entity) = &fact.subject.entity {
            observed_interfaces.insert(entity.clone());
        }
        let Some(fields) = &fact.payload.fields else {
            continue;
        };
        match fact.fact_kind.as_str() {
            FACT_KIND_ARPND_NEIGHBOR => {
                if let Ok(payload) = serde_json::from_value::<ArpndNeighborPayload>(fields.clone())
                {
                    if payload.interface_vlan_id.state == FieldAvailability::Known
                        && let Some(vlan) = payload.interface_vlan_id.value
                    {
                        observed_vlan_ids.insert(vlan);
                    }
                    if payload.forwarding_domain.state == FieldAvailability::Known
                        && let Some(domain) = payload.forwarding_domain.value
                    {
                        observed_forwarding_domains.insert(domain);
                    }
                }
            }
            FACT_KIND_MAC_FORWARDING => {
                if let Ok(payload) = serde_json::from_value::<MacForwardingPayload>(fields.clone())
                {
                    if payload.vlan_id.state == FieldAvailability::Known
                        && let Some(vlan) = payload.vlan_id.value
                    {
                        observed_vlan_ids.insert(vlan);
                    }
                    if payload.bridge_domain.state == FieldAvailability::Known
                        && let Some(domain) = payload.bridge_domain.value
                    {
                        observed_forwarding_domains.insert(domain);
                    }
                }
            }
            _ => {}
        }
    }
    ScopedCaptureCoverage {
        device_id: ctx.device_id.to_string(),
        scope: ctx.scope.map(str::to_string),
        network_namespace: ctx.network_namespace.map(str::to_string),
        collector_id: ctx.collector_id.to_string(),
        source_kind: source_kind.to_string(),
        capture_id: ctx.capture_id.to_string(),
        source_generation: Some(ctx.session_epoch),
        acquisition_scope: profile.to_string(),
        covered_fact_kind: covered_fact_kind.to_string(),
        declared_scope: declared_scope_contract(profile),
        capture_outcome: capture.outcome,
        population_coverage: coverage_population(capture, report),
        positive_empty_population: report.empty_records,
        observation_time: Some(ctx.observed_at.to_string()),
        valid_until: None,
        observed_interfaces,
        observed_forwarding_domains,
        observed_vlan_ids,
        records_decoded: report.records_decoded,
        malformed_records: report.malformed_records,
        omitted_records: report.input_records_omitted + report.output_facts_omitted,
        rejected_records: report.facts_rejected,
        evidence_refs: raw_sha256
            .map(|sha256| {
                vec![EvidenceRef {
                    kind: "raw_output".into(),
                    sha256: sha256.to_string(),
                    locator: Some(profile.to_string()),
                }]
            })
            .unwrap_or_default(),
    }
}

fn empty_coverage(
    ctx: &ObservationContext<'_>,
    capture: &CommandCapture<'_>,
    source_kind: &str,
    profile: &str,
    covered_fact_kind: &str,
    report: &NeighborNormalizationReport,
) -> ScopedCaptureCoverage {
    scoped_coverage(
        ctx,
        capture,
        source_kind,
        profile,
        covered_fact_kind,
        report,
        &[],
        None,
    )
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
    if obs.capture.output.len() > MAX_INPUT_BYTES {
        report.refusal_reason = Some("input exceeds maximum byte budget before parsing".into());
        let coverage = empty_coverage(
            &obs.context,
            &obs.capture,
            obs.profile.source_kind(),
            obs.profile.label(),
            FACT_KIND_ARPND_NEIGHBOR,
            &report,
        );
        return NeighborNormalizationOutput {
            facts: vec![],
            report,
            coverage,
        };
    }
    // Evidence integrity is bounded first and then hashed once per capture;
    // every emitted fact references this same digest.
    let raw_sha256 = sha256_hex(obs.capture.output.as_bytes());
    if let Some(err) = context_error(&obs.context) {
        report.refusal_reason = Some(err);
        let coverage = empty_coverage(
            &obs.context,
            &obs.capture,
            obs.profile.source_kind(),
            obs.profile.label(),
            FACT_KIND_ARPND_NEIGHBOR,
            &report,
        );
        return NeighborNormalizationOutput {
            facts: vec![],
            report,
            coverage,
        };
    }
    if !obs.capture.outcome.usable() {
        report.refusal_reason = Some("capture outcome is not usable".into());
        let coverage = empty_coverage(
            &obs.context,
            &obs.capture,
            obs.profile.source_kind(),
            obs.profile.label(),
            FACT_KIND_ARPND_NEIGHBOR,
            &report,
        );
        return NeighborNormalizationOutput {
            facts: vec![],
            report,
            coverage,
        };
    }

    let records = match obs.profile {
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
        ArpndSourceProfile::CumulusIpNeighShow | ArpndSourceProfile::LinuxIpNeighShow => {
            parse_cumulus_arp(obs.capture.output, &mut report)
        }
    };
    if report.input_records_omitted > 0 {
        report.notes.push(format!(
            "input record budget omitted {} decoded records",
            report.input_records_omitted
        ));
    }
    report.empty_records = positive_empty_population(&report);

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
        let canonical_entity = canonical_interface_entity(&record.interface, identity.as_ref());
        let mut payload = record.payload.clone();
        payload.neighbor_entity_key = Some(arpnd_attachment_entity_key(
            record.vrf.as_deref(),
            payload.address_family,
            &payload.ip_address,
            &canonical_entity,
            identity
                .as_ref()
                .and_then(|identity| identity.subinterface_index),
        ));
        let fields = serde_json::to_value(&payload).unwrap_or(Value::Null);
        let fact = fact_for_interface_payload(
            &obs.context,
            obs.profile.source_kind(),
            FACT_KIND_ARPND_NEIGHBOR,
            &record.interface,
            record.vrf.as_deref(),
            identity,
            fields,
            &raw_sha256,
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
    let coverage = scoped_coverage(
        &obs.context,
        &obs.capture,
        obs.profile.source_kind(),
        obs.profile.label(),
        FACT_KIND_ARPND_NEIGHBOR,
        &report,
        &facts,
        Some(&raw_sha256),
    );
    NeighborNormalizationOutput {
        facts,
        report,
        coverage,
    }
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
    if obs.capture.output.len() > MAX_INPUT_BYTES {
        report.refusal_reason = Some("input exceeds maximum byte budget before parsing".into());
        let coverage = empty_coverage(
            &obs.context,
            &obs.capture,
            obs.profile.source_kind(),
            obs.profile.label(),
            FACT_KIND_MAC_FORWARDING,
            &report,
        );
        return NeighborNormalizationOutput {
            facts: vec![],
            report,
            coverage,
        };
    }
    let raw_sha256 = sha256_hex(obs.capture.output.as_bytes());
    if let Some(err) = context_error(&obs.context) {
        report.refusal_reason = Some(err);
        let coverage = empty_coverage(
            &obs.context,
            &obs.capture,
            obs.profile.source_kind(),
            obs.profile.label(),
            FACT_KIND_MAC_FORWARDING,
            &report,
        );
        return NeighborNormalizationOutput {
            facts: vec![],
            report,
            coverage,
        };
    }
    if !obs.capture.outcome.usable() {
        report.refusal_reason = Some("capture outcome is not usable".into());
        let coverage = empty_coverage(
            &obs.context,
            &obs.capture,
            obs.profile.source_kind(),
            obs.profile.label(),
            FACT_KIND_MAC_FORWARDING,
            &report,
        );
        return NeighborNormalizationOutput {
            facts: vec![],
            report,
            coverage,
        };
    }

    let records = match obs.profile {
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
        MacSourceProfile::CumulusBridgeFdbShow | MacSourceProfile::LinuxBridgeFdbShow => {
            parse_cumulus_mac(obs.capture.output, &mut report)
        }
    };
    if report.input_records_omitted > 0 {
        report.notes.push(format!(
            "input record budget omitted {} decoded records",
            report.input_records_omitted
        ));
    }
    report.empty_records = positive_empty_population(&report);

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
        let identity =
            interface_identity_for(obs.interface_identities, &record.interface).or_else(|| {
                interface_identity_for(obs.interface_identities, &record.payload.outgoing_interface)
            });
        let mut payload = record.payload.clone();
        let bridge_domain = (payload.bridge_domain.state == FieldAvailability::Known)
            .then_some(payload.bridge_domain.value.clone())
            .flatten();
        let vlan = (payload.vlan_id.state == FieldAvailability::Known)
            .then_some(payload.vlan_id.value)
            .flatten();
        payload.forwarding_entity_key = Some(mac_forwarding_entity_key(
            bridge_domain.as_deref(),
            vlan,
            &payload.mac_address,
        ));
        let fields = serde_json::to_value(&payload).unwrap_or(Value::Null);
        let fact = fact_for_interface_payload(
            &obs.context,
            obs.profile.source_kind(),
            FACT_KIND_MAC_FORWARDING,
            &record.interface,
            None,
            identity,
            fields,
            &raw_sha256,
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
    let coverage = scoped_coverage(
        &obs.context,
        &obs.capture,
        obs.profile.source_kind(),
        obs.profile.label(),
        FACT_KIND_MAC_FORWARDING,
        &report,
        &facts,
        Some(&raw_sha256),
    );
    NeighborNormalizationOutput {
        facts,
        report,
        coverage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    #[test]
    fn linux_neighbor_states_preserve_probe_delay_noarp_and_unknown() {
        let input = "10.0.0.1 dev swp1 lladdr aa:bb:cc:dd:ee:01 PROBE\n\
                     10.0.0.2 dev swp1 lladdr aa:bb:cc:dd:ee:02 DELAY\n\
                     10.0.0.3 dev swp1 lladdr aa:bb:cc:dd:ee:03 NOARP\n\
                     10.0.0.4 dev swp1 lladdr aa:bb:cc:dd:ee:04 STALE\n";
        let out = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::CumulusIpNeighShow,
            capture: complete(input),
            context: ctx(),
            interface_identities: None,
        });
        assert_eq!(out.report.malformed_records, 0);
        let states: BTreeSet<_> = out
            .facts
            .iter()
            .map(|f| {
                serde_json::from_value::<ArpndNeighborPayload>(f.payload.fields.clone().unwrap())
                    .unwrap()
                    .resolution_state
                    .value
                    .unwrap()
            })
            .collect();
        assert!(states.contains(&NeighborResolutionState::Probe));
        assert!(states.contains(&NeighborResolutionState::Delay));
        assert!(states.contains(&NeighborResolutionState::NoArp));
        assert!(states.contains(&NeighborResolutionState::Stale));
    }

    #[test]
    fn malformed_linux_lladdr_is_invalid_evidence_not_protocol_failure() {
        let input = "10.0.0.9 dev swp1 lladdr not-a-mac\n";
        let out = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::CumulusIpNeighShow,
            capture: complete(input),
            context: ctx(),
            interface_identities: None,
        });
        assert_eq!(out.facts.len(), 1);
        let p: ArpndNeighborPayload =
            serde_json::from_value(out.facts[0].payload.fields.clone().unwrap()).unwrap();
        assert_eq!(p.mac_address.state, FieldAvailability::Invalid);
        assert_eq!(
            p.resolution_state.state,
            FieldAvailability::Unknown,
            "malformed MAC text must not fabricate a resolution state"
        );
    }

    #[test]
    fn eos_neighbor_without_hardware_address_is_retained_as_incomplete_unknown() {
        let input = r#"{"vrfs":{"default":{"ipV4Neighbors":[{"address":"192.0.2.44","age":0,"interface":"Ethernet1"}]}}}"#;
        let out = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::EosShowArpVrfAllJson,
            capture: complete(input),
            context: ctx(),
            interface_identities: None,
        });
        assert_eq!(out.report.malformed_records, 0);
        assert_eq!(out.facts.len(), 1);
        let p: ArpndNeighborPayload =
            serde_json::from_value(out.facts[0].payload.fields.clone().unwrap()).unwrap();
        assert_eq!(p.mac_address.state, FieldAvailability::Unknown);
        assert!(p.mac_address.value.is_none());
        assert_eq!(p.resolution_state.state, FieldAvailability::Unknown);
    }

    #[test]
    fn positive_empty_scoped_table_is_distinct_from_missing_scope() {
        let out = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::CumulusIpNeighShow,
            capture: complete(""),
            context: ctx(),
            interface_identities: None,
        });
        assert!(out.facts.is_empty());
        assert!(out.report.empty_records);
        assert!(out.coverage.positive_empty_population);
        assert_eq!(
            out.coverage.declared_scope,
            AcquisitionScopeContract::CompleteScopedTable
        );
        assert_eq!(
            out.coverage.population_coverage,
            ScopedPopulationCoverage::Complete
        );
    }

    #[test]
    fn eos_empty_vrf_root_is_unknown_scope_not_positive_empty_table() {
        let out = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::EosShowArpVrfAllJson,
            capture: complete(r#"{"vrfs":{}}"#),
            context: ctx(),
            interface_identities: None,
        });
        assert!(out.facts.is_empty());
        assert!(!out.report.empty_records);
        assert_eq!(out.report.collection_presence["vrfs"], "unknown_scope");
        assert_eq!(
            out.coverage.population_coverage,
            ScopedPopulationCoverage::Unknown
        );
    }

    #[test]
    fn missing_or_malformed_nxos_containers_are_not_confirmed_empty_tables() {
        let missing = normalize_mac(&MacObservation {
            profile: MacSourceProfile::NxosShowMacAddressTableJsonNative,
            capture: complete(r#"{"TABLE_mac_address":{}}"#),
            context: ctx(),
            interface_identities: None,
        });
        assert!(missing.facts.is_empty());
        assert!(!missing.report.empty_records);
        assert_eq!(
            missing.coverage.population_coverage,
            ScopedPopulationCoverage::Unknown
        );
        assert_eq!(
            missing.report.collection_presence["TABLE_mac_address.ROW_mac_address"],
            "missing"
        );

        let malformed = normalize_mac(&MacObservation {
            profile: MacSourceProfile::NxosShowMacAddressTableJsonNative,
            capture: complete(r#"{"TABLE_mac_address":{"ROW_mac_address":"wrong"}}"#),
            context: ctx(),
            interface_identities: None,
        });
        assert!(malformed.facts.is_empty());
        assert!(!malformed.report.empty_records);
        assert!(malformed.report.malformed_records > 0);
        assert_ne!(
            malformed.coverage.population_coverage,
            ScopedPopulationCoverage::Complete
        );
    }

    #[test]
    fn raw_capture_evidence_digest_is_shared_across_all_emitted_facts() {
        let input = include_str!(
            "../tests/fixtures/bn024/neighbor/eos-leaf02-show-mac-address-table-unicast.json"
        );
        let out = normalize_mac(&MacObservation {
            profile: MacSourceProfile::EosShowMacAddressTableUnicastJson,
            capture: complete(input),
            context: ctx(),
            interface_identities: None,
        });
        assert!(out.facts.len() > 1);
        let expected = sha256_hex(input.as_bytes());
        assert!(
            out.facts
                .iter()
                .all(|f| f.evidence.len() == 1 && f.evidence[0].sha256 == expected)
        );
        assert_eq!(out.coverage.evidence_refs[0].sha256, expected);
    }

    #[test]
    fn cumulus_bridge_master_and_entry_contexts_are_preserved() {
        let input = "aa:bb:cc:dd:ee:ff dev bond1 vlan 100 master bridge extern_learn offload\n";
        let out = normalize_mac(&MacObservation {
            profile: MacSourceProfile::CumulusBridgeFdbShow,
            capture: complete(input),
            context: ctx(),
            interface_identities: None,
        });
        assert_eq!(out.facts.len(), 1);
        let p: MacForwardingPayload =
            serde_json::from_value(out.facts[0].payload.fields.clone().unwrap()).unwrap();
        assert_eq!(p.bridge_domain.value.as_deref(), Some("bridge"));
        assert!(p.entry_contexts.contains(&MacEntryContext::MasterTable));
        assert!(
            p.entry_contexts
                .contains(&MacEntryContext::ExternallyLearned)
        );
        assert!(p.entry_contexts.contains(&MacEntryContext::Offloaded));
        assert!(out.coverage.observed_forwarding_domains.contains("bridge"));
        assert!(out.coverage.observed_vlan_ids.contains(&100));
    }

    #[test]
    fn structured_subinterface_subject_uses_canonical_parent_without_losing_native_name() {
        let mut identities = InterfaceIdentityEvidence::default();
        identities.by_native_name.insert(
            "Ethernet1.100".into(),
            crate::typed_facts::InterfaceSubjectIdentity {
                kind: InterfaceIdentityKind::Subinterface,
                parent_entity: Some("Ethernet1".into()),
                subinterface_index: Some(100),
                vendor_native_name: Some("Ethernet1.100".into()),
            },
        );
        let input = "10.0.0.9 dev Ethernet1.100 lladdr aa:bb:cc:dd:ee:ff REACHABLE\n";
        let out = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::CumulusIpNeighShow,
            capture: complete(input),
            context: ctx(),
            interface_identities: Some(&identities),
        });
        assert_eq!(out.report.facts_rejected, 0);
        assert_eq!(out.facts.len(), 1);
        let fact = &out.facts[0];
        assert_eq!(fact.subject.entity.as_deref(), Some("Ethernet1"));
        let identity = fact.subject.interface_identity.as_ref().unwrap();
        assert_eq!(identity.subinterface_index, Some(100));
        let p: ArpndNeighborPayload =
            serde_json::from_value(fact.payload.fields.clone().unwrap()).unwrap();
        assert_eq!(p.outgoing_interface, "Ethernet1.100");
        assert!(
            p.neighbor_entity_key
                .as_deref()
                .is_some_and(|key| key.contains("Ethernet1"))
        );
    }

    #[test]
    fn normalization_performance_smoke_reports_once_per_capture_digest_cost() {
        let input = include_str!(
            "../tests/fixtures/bn024/neighbor/eos-leaf02-show-mac-address-table-unicast.json"
        );
        let iterations = 100usize;
        let started = std::time::Instant::now();
        let mut facts = 0usize;
        for _ in 0..iterations {
            let out = normalize_mac(&MacObservation {
                profile: MacSourceProfile::EosShowMacAddressTableUnicastJson,
                capture: complete(input),
                context: ctx(),
                interface_identities: None,
            });
            facts = std::hint::black_box(out.facts.len());
        }
        let elapsed = started.elapsed();
        assert_eq!(facts, 11);
        println!(
            "neighbor_mac_normalization_perf iterations={} total_ms={:.3} avg_us={:.1} normalizations_per_sec={:.1}",
            iterations,
            elapsed.as_secs_f64() * 1000.0,
            elapsed.as_secs_f64() * 1_000_000.0 / iterations as f64,
            iterations as f64 / elapsed.as_secs_f64().max(f64::EPSILON)
        );
    }
    #[test]
    fn genuine_linux_frr_neighbor_and_fdb_shapes_are_explicit() {
        let neigh = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::LinuxIpNeighShow,
            capture: complete(include_str!(
                "../tests/fixtures/bn024/l2-vertical/frr2-ip-neigh-show.txt"
            )),
            context: ctx(),
            interface_identities: None,
        });
        assert!(neigh.facts.is_empty());
        assert!(neigh.coverage.positive_empty_population);
        assert_eq!(
            neigh.coverage.population_coverage,
            ScopedPopulationCoverage::Complete
        );

        let mac = normalize_mac(&MacObservation {
            profile: MacSourceProfile::LinuxBridgeFdbShow,
            capture: complete(include_str!(
                "../tests/fixtures/bn024/l2-vertical/frr2-bridge-fdb-show.txt"
            )),
            context: ctx(),
            interface_identities: None,
        });
        assert_eq!(mac.report.malformed_records, 0);
        assert_eq!(mac.facts.len(), 2);
        assert_eq!(
            mac.coverage.population_coverage,
            ScopedPopulationCoverage::Complete
        );
    }
}
