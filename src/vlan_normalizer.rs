//! Offline VLAN/bridge forwarding-domain normalization.
//!
//! Qualified first source contracts are the pinned SuzieQ upstream shapes:
//! EOS `show vlan brief` JSON, NXOS `show vlan brief | json native`, and
//! Cumulus/Linux `/sbin/bridge -c vlan show`. This implementation preserves
//! scoped identity and uncertainty rather than upstream's lossy VLAN-centric
//! collapse: Linux port rows are not merged across bridges by VLAN number,
//! tagging/PVID flags remain explicit, and absent state/mode/VRF evidence is
//! unknown rather than synthesized.

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use crate::interface_facts::{FieldAvailability, ObservedField};
use crate::interface_normalizer::{CaptureOutcome, CommandCapture, ObservationContext};
use crate::semantic_evidence::{
    AcquisitionScopeContract, ScopedCaptureCoverage, ScopedPopulationCoverage,
};
use crate::typed_facts::{
    Completeness, EvidenceRef, FactPayload, FactSource, FactSubject, Lifecycle, Quality,
    TrustClass, TypedFact,
};
use crate::vlan_facts::{
    FACT_KIND_VLAN_BRIDGE_DOMAIN, MembershipAvailability, MembershipPlane, VlanAdminState,
    VlanBridgePayload, VlanMember, VlanObservationShape, VlanOperationalState,
    bridge_vlan_entity_key, port_membership_entity_key, validate_vlan_typed,
    vlan_database_entity_key,
};

pub const VLAN_PARSER_REVISION: &str = "vlan-structured-2026.11.26";
pub const VLAN_MAPPING_REVISION: &str = "bn024-vlan-map-1";
const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_RECORDS: usize = 10_000;
const MAX_OUTPUT_FACTS: usize = 10_000;
const MAX_INVALID_SAMPLES: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VlanSourceProfile {
    EosShowVlanBriefJson,
    NxosShowVlanBriefJsonNative,
    CumulusBridgeCVlanShow,
}

impl VlanSourceProfile {
    pub fn label(self) -> &'static str {
        match self {
            Self::EosShowVlanBriefJson => "eos/show-vlan-brief-json",
            Self::NxosShowVlanBriefJsonNative => "nxos/show-vlan-brief-json-native",
            Self::CumulusBridgeCVlanShow => "cumulus/bridge-c-vlan-show",
        }
    }

    pub fn source_kind(self) -> &'static str {
        match self {
            Self::EosShowVlanBriefJson | Self::NxosShowVlanBriefJsonNative => "cli_json",
            Self::CumulusBridgeCVlanShow => "cli_text",
        }
    }
}

pub struct VlanObservation<'a> {
    pub profile: VlanSourceProfile,
    pub capture: CommandCapture<'a>,
    pub context: ObservationContext<'a>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct VlanNormalizationReport {
    pub profile: String,
    pub capture_outcome: String,
    pub input_bytes: usize,
    pub records_decoded: usize,
    pub facts_emitted: usize,
    pub malformed_records: usize,
    pub input_records_omitted: usize,
    pub output_facts_omitted: usize,
    pub facts_rejected: usize,
    pub duplicate_records: usize,
    pub conflicting_records: usize,
    pub conflicting_pvid_records: usize,
    pub empty_records: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub collection_presence: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invalid_samples: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct VlanNormalizationOutput {
    pub facts: Vec<TypedFact>,
    pub report: VlanNormalizationReport,
    pub coverage: ScopedCaptureCoverage,
}

fn known<T>(value: T) -> ObservedField<T> {
    ObservedField::known(value)
}

fn unknown<T>(detail: &str) -> ObservedField<T> {
    ObservedField::unavailable(FieldAvailability::Unknown, Some(detail.to_string()))
}

fn invalid<T>(detail: &str) -> ObservedField<T> {
    ObservedField::unavailable(FieldAvailability::Invalid, Some(detail.to_string()))
}

fn sha256_hex(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    format!("{:x}", h.finalize())
}

fn sample(report: &mut VlanNormalizationReport, detail: impl Into<String>) {
    if report.invalid_samples.len() < MAX_INVALID_SAMPLES {
        report.invalid_samples.push(detail.into());
    }
}

fn capture_outcome_label(outcome: CaptureOutcome) -> &'static str {
    match outcome {
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
        ("observed_at", ctx.observed_at),
    ] {
        if value.trim().is_empty() {
            return Some(format!("observation context '{name}' is empty"));
        }
    }
    None
}

fn parse_json_input(input: &str) -> Result<Value, String> {
    crate::xrd_openconfig_interfaces::reject_duplicate_json_keys(input.as_bytes())
        .map_err(|e| format!("duplicate JSON key: {e}"))?;
    serde_json::from_str(input).map_err(|e| format!("malformed JSON: {e}"))
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn fact_for_vlan_payload(
    ctx: &ObservationContext<'_>,
    source_kind: &str,
    entity: &str,
    payload: VlanBridgePayload,
    raw_sha256: &str,
    profile: &str,
    completeness: Completeness,
    sequence: u64,
) -> TypedFact {
    let fields = serde_json::to_value(&payload).unwrap_or(Value::Null);
    let mut fact = TypedFact {
        schema_version: crate::typed_facts::SCHEMA_VERSION,
        fact_id: String::new(),
        fact_kind: FACT_KIND_VLAN_BRIDGE_DOMAIN.to_string(),
        subject: FactSubject {
            device_id: ctx.device_id.to_string(),
            scope: ctx.scope.map(str::to_string),
            namespace: "forwarding".to_string(),
            network_namespace: ctx.network_namespace.map(str::to_string),
            entity: Some(entity.to_string()),
            vrf: None,
            afi_safi: None,
            plane: Some("operational".to_string()),
            interface_identity: None,
        },
        source: FactSource {
            collector_id: ctx.collector_id.to_string(),
            source_kind: source_kind.to_string(),
            capture_id: ctx.capture_id.to_string(),
            session_epoch: ctx.session_epoch,
            sequence,
            exporter_id: None,
        },
        observed_at: ctx.observed_at.to_string(),
        ingested_at: None,
        valid_from: Some(ctx.observed_at.to_string()),
        valid_until: None,
        payload: FactPayload {
            produced_by: "bonsai.vlan_normalizer".into(),
            parser_revision: VLAN_PARSER_REVISION.into(),
            mapping_revision: Some(VLAN_MAPPING_REVISION.into()),
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
        evidence: vec![EvidenceRef {
            kind: "raw_output".into(),
            sha256: raw_sha256.to_string(),
            locator: Some(profile.to_string()),
        }],
    };
    fact.seal();
    fact
}

fn member_unknown(interface: &str) -> VlanMember {
    VlanMember {
        interface_name: interface.to_string(),
        interface_key: None,
        tagged: unknown("source does not expose tagged membership"),
        untagged: unknown("source does not expose untagged membership"),
        pvid: unknown("source does not expose PVID/native VLAN membership"),
        mode: unknown("source does not expose access/trunk/routed mode"),
        attachment_kind: unknown("source does not expose interface attachment type"),
    }
}

fn database_payload(
    vlan_id: u32,
    name: ObservedField<String>,
    operational_state: ObservedField<VlanOperationalState>,
    admin_state: ObservedField<VlanAdminState>,
    dynamic: ObservedField<bool>,
    interfaces: Vec<String>,
    membership_availability: MembershipAvailability,
) -> VlanBridgePayload {
    let domain = vlan_database_entity_key(vlan_id);
    VlanBridgePayload {
        observation_shape: VlanObservationShape::VlanDatabaseEntry,
        bridge_domain: known(domain.clone()),
        vlan_id,
        vlan_name: name,
        operational_state,
        admin_state,
        dynamic,
        membership_plane: known(MembershipPlane::SourceReported),
        membership_availability,
        members: interfaces.iter().map(|name| member_unknown(name)).collect(),
        routed_interfaces: unknown("VLAN membership output does not expose routed/SVI attachment"),
        vrf: unknown("VLAN output does not expose VRF/network-instance attachment"),
        domain_entity_key: Some(domain),
    }
}

fn parse_eos_vlan(root: &Value, report: &mut VlanNormalizationReport) -> Vec<VlanBridgePayload> {
    let mut out = Vec::new();
    let Some(vlans) = root.get("vlans") else {
        report
            .collection_presence
            .insert("vlans".into(), "missing".into());
        return out;
    };
    let Some(object) = vlans.as_object() else {
        report
            .collection_presence
            .insert("vlans".into(), "malformed".into());
        report.malformed_records += 1;
        sample(report, "EOS vlans is not an object");
        return out;
    };
    report.collection_presence.insert(
        "vlans".into(),
        if object.is_empty() {
            "empty".into()
        } else {
            "populated".into()
        },
    );
    let mut entries: Vec<_> = object.iter().collect();
    entries.sort_by_key(|(key, _)| key.parse::<u32>().unwrap_or(u32::MAX));
    for (vlan_key, item) in entries {
        if out.len() >= MAX_RECORDS {
            report.input_records_omitted += 1;
            continue;
        }
        let Ok(vlan_id) = vlan_key.parse::<u32>() else {
            report.malformed_records += 1;
            sample(report, format!("EOS nonnumeric VLAN key {vlan_key}"));
            continue;
        };
        if vlan_id == 0 || vlan_id > 4095 {
            report.malformed_records += 1;
            sample(report, format!("EOS VLAN key out of range {vlan_id}"));
            continue;
        }
        let name = match str_field(item, "name") {
            Some(name) if !name.is_empty() => known(name.to_string()),
            Some(_) => invalid("EOS VLAN name is empty"),
            None => unknown("EOS VLAN name absent"),
        };
        let state = match str_field(item, "status") {
            Some("active") => known(VlanOperationalState::Active),
            Some("suspended") => known(VlanOperationalState::Suspended),
            Some(_) => invalid("EOS VLAN status unrecognized"),
            None => unknown("EOS VLAN status absent"),
        };
        let dynamic = match item.get("dynamic").and_then(Value::as_bool) {
            Some(value) => known(value),
            None => unknown("EOS dynamic flag absent"),
        };
        let (interfaces, membership_availability) = match item.get("interfaces") {
            Some(Value::Object(members)) => {
                let mut interfaces: Vec<String> = members.keys().cloned().collect();
                interfaces.sort();
                interfaces.dedup();
                report.collection_presence.insert(
                    format!("vlans.{vlan_id}.interfaces"),
                    if interfaces.is_empty() {
                        "empty"
                    } else {
                        "populated"
                    }
                    .into(),
                );
                (
                    interfaces.clone(),
                    if interfaces.is_empty() {
                        MembershipAvailability::KnownEmpty
                    } else {
                        MembershipAvailability::KnownPopulated
                    },
                )
            }
            Some(_) => {
                report.malformed_records += 1;
                sample(
                    report,
                    format!("EOS VLAN {vlan_id} interfaces collection malformed"),
                );
                report
                    .collection_presence
                    .insert(format!("vlans.{vlan_id}.interfaces"), "malformed".into());
                (vec![], MembershipAvailability::Malformed)
            }
            None => {
                report
                    .collection_presence
                    .insert(format!("vlans.{vlan_id}.interfaces"), "missing".into());
                (vec![], MembershipAvailability::Missing)
            }
        };
        report.records_decoded += 1;
        out.push(database_payload(
            vlan_id,
            name,
            state,
            unknown("EOS VLAN output does not expose shutdown state"),
            dynamic,
            interfaces,
            membership_availability,
        ));
    }
    out
}

fn nxos_rows(
    container: Option<&Value>,
    label: &str,
    report: &mut VlanNormalizationReport,
) -> Vec<Value> {
    match container {
        None => {
            report
                .collection_presence
                .insert(label.into(), "missing".into());
            vec![]
        }
        Some(Value::Array(rows)) => {
            report.collection_presence.insert(
                label.into(),
                if rows.is_empty() {
                    "empty"
                } else {
                    "populated"
                }
                .into(),
            );
            rows.clone()
        }
        Some(row @ Value::Object(_)) => {
            report
                .collection_presence
                .insert(label.into(), "populated".into());
            vec![row.clone()]
        }
        Some(_) => {
            report
                .collection_presence
                .insert(label.into(), "malformed".into());
            report.malformed_records += 1;
            sample(report, format!("NXOS {label} malformed"));
            vec![]
        }
    }
}

fn nxos_u32(item: &Value, key: &str) -> Option<u32> {
    item.get(key).and_then(|value| {
        value
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .or_else(|| value.as_str()?.parse().ok())
    })
}

fn parse_nxos_vlan(root: &Value, report: &mut VlanNormalizationReport) -> Vec<VlanBridgePayload> {
    let mut out = Vec::new();
    let rows = nxos_rows(
        root.get("TABLE_vlanbriefxbrief")
            .and_then(|v| v.get("ROW_vlanbriefxbrief")),
        "TABLE_vlanbriefxbrief.ROW_vlanbriefxbrief",
        report,
    );
    for item in rows {
        if out.len() >= MAX_RECORDS {
            report.input_records_omitted += 1;
            continue;
        }
        let Some(vlan_id) = nxos_u32(&item, "vlanshowbr-vlanid") else {
            report.malformed_records += 1;
            sample(report, "NXOS VLAN row missing/invalid vlan id");
            continue;
        };
        if vlan_id == 0 || vlan_id > 4095 {
            report.malformed_records += 1;
            sample(report, format!("NXOS VLAN id out of range {vlan_id}"));
            continue;
        }
        let name = match str_field(&item, "vlanshowbr-vlanname") {
            Some(name) if !name.is_empty() => known(name.to_string()),
            Some(_) => invalid("NXOS VLAN name is empty"),
            None => unknown("NXOS VLAN name absent"),
        };
        let state = match str_field(&item, "vlanshowbr-vlanstate") {
            Some("active") => known(VlanOperationalState::Active),
            Some("suspend") | Some("suspended") => known(VlanOperationalState::Suspended),
            Some(_) => invalid("NXOS VLAN state unrecognized"),
            None => unknown("NXOS VLAN state absent"),
        };
        let admin = match str_field(&item, "vlanshowbr-shutstate") {
            Some("shutdown") => known(VlanAdminState::Shutdown),
            Some("noshutdown") => known(VlanAdminState::NoShutdown),
            Some(_) => invalid("NXOS VLAN shutdown state unrecognized"),
            None => unknown("NXOS VLAN shutdown state absent"),
        };
        let mut interfaces = Vec::new();
        let mut invalid_members = 0usize;
        let membership_availability = match item.get("vlanshowplist-ifidx") {
            Some(Value::String(raw)) => {
                interfaces.extend(
                    raw.split(',')
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(str::to_string),
                );
                if interfaces.is_empty() {
                    MembershipAvailability::KnownEmpty
                } else {
                    MembershipAvailability::KnownPopulated
                }
            }
            Some(Value::Array(values)) => {
                for value in values {
                    if let Some(raw) = value.as_str() {
                        interfaces.extend(
                            raw.split(',')
                                .map(str::trim)
                                .filter(|v| !v.is_empty())
                                .map(str::to_string),
                        );
                    } else {
                        invalid_members += 1;
                    }
                }
                if invalid_members > 0 {
                    report.malformed_records += invalid_members;
                    sample(
                        report,
                        format!("NXOS VLAN {vlan_id} interface array contains non-string elements"),
                    );
                    if interfaces.is_empty() {
                        MembershipAvailability::Malformed
                    } else {
                        MembershipAvailability::Partial
                    }
                } else if interfaces.is_empty() {
                    MembershipAvailability::KnownEmpty
                } else {
                    MembershipAvailability::KnownPopulated
                }
            }
            Some(_) => {
                report.malformed_records += 1;
                sample(
                    report,
                    format!("NXOS VLAN {vlan_id} interface list malformed"),
                );
                MembershipAvailability::Malformed
            }
            None => MembershipAvailability::Missing,
        };
        report.collection_presence.insert(
            format!("vlans.{vlan_id}.interfaces"),
            match membership_availability {
                MembershipAvailability::KnownEmpty => "empty",
                MembershipAvailability::KnownPopulated | MembershipAvailability::Partial => {
                    "populated"
                }
                MembershipAvailability::Missing => "missing",
                MembershipAvailability::Malformed => "malformed",
                _ => "unknown",
            }
            .into(),
        );
        interfaces.sort();
        interfaces.dedup();
        report.records_decoded += 1;
        out.push(database_payload(
            vlan_id,
            name,
            state,
            admin,
            unknown("NXOS VLAN output does not expose dynamic/static creation flag"),
            interfaces,
            membership_availability,
        ));
    }
    out
}

fn parse_cumulus_vlan(input: &str, report: &mut VlanNormalizationReport) -> Vec<VlanBridgePayload> {
    let mut out = Vec::new();
    let mut lines = input.lines();
    let Some(header) = lines.next() else {
        report
            .collection_presence
            .insert("bridge-vlan-header".into(), "missing".into());
        report.malformed_records += 1;
        sample(
            report,
            "Cumulus bridge VLAN output is empty; expected 'port\\tvlan ids' header",
        );
        return out;
    };
    let header_parts: Vec<_> = header.split_whitespace().collect();
    if header_parts != ["port", "vlan", "ids"] {
        report
            .collection_presence
            .insert("bridge-vlan-header".into(), "malformed".into());
        report.malformed_records += 1;
        sample(
            report,
            format!("Cumulus bridge VLAN header malformed or diagnostic output received: {header}"),
        );
        return out;
    }
    report
        .collection_presence
        .insert("bridge-vlan-header".into(), "populated".into());

    let mut saw_row = false;
    let mut current_port: Option<String> = None;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        if out.len() >= MAX_RECORDS {
            report.input_records_omitted += 1;
            continue;
        }
        let continuation = line.chars().next().is_some_and(|c| c.is_whitespace());
        let parts: Vec<_> = line.split_whitespace().collect();
        let (port, vlan_token, flag_start, valid_port_transition) = if continuation {
            let Some(port) = current_port.clone() else {
                report.malformed_records += 1;
                sample(
                    report,
                    "Cumulus VLAN continuation before any valid port row",
                );
                continue;
            };
            (port, parts.first().copied().unwrap_or_default(), 1, true)
        } else {
            if parts.len() < 2 {
                report.malformed_records += 1;
                sample(report, format!("Cumulus VLAN row missing VLAN id: {line}"));
                current_port = None;
                continue;
            }
            let port = parts[0].to_string();
            current_port = Some(port.clone());
            (port, parts[1], 2, true)
        };
        if !valid_port_transition {
            continue;
        }
        if vlan_token.contains('-') || vlan_token.contains(',') {
            report.malformed_records += 1;
            sample(
                report,
                format!("Cumulus compressed VLAN range/list token is unsupported: {vlan_token}"),
            );
            continue;
        }
        let Ok(vlan_id) = vlan_token.parse::<u32>() else {
            report.malformed_records += 1;
            sample(report, format!("Cumulus invalid VLAN token {vlan_token}"));
            continue;
        };
        if vlan_id == 0 || vlan_id > 4095 {
            report.malformed_records += 1;
            sample(report, format!("Cumulus VLAN id out of range {vlan_id}"));
            continue;
        }
        let flags: Vec<_> = parts.iter().skip(flag_start).copied().collect();
        let mut seen_flags = BTreeSet::new();
        let mut flags_valid = true;
        for flag in &flags {
            if !matches!(*flag, "PVID" | "Egress" | "Untagged") || !seen_flags.insert(*flag) {
                report.malformed_records += 1;
                sample(
                    report,
                    format!(
                        "Cumulus unsupported or duplicate VLAN flag {flag} on {port} vlan {vlan_id}"
                    ),
                );
                flags_valid = false;
            }
        }
        if !flags_valid {
            continue;
        }
        let untagged = flags.iter().any(|flag| *flag == "Untagged");
        let pvid = flags.iter().any(|flag| *flag == "PVID");
        if untagged && !flags.iter().any(|flag| *flag == "Egress") {
            report.malformed_records += 1;
            sample(
                report,
                format!(
                    "Cumulus unsupported flag combination on {port} vlan {vlan_id}: Untagged without Egress"
                ),
            );
            continue;
        }
        saw_row = true;
        let interface = port;
        let key = port_membership_entity_key(&interface, vlan_id);
        report.records_decoded += 1;
        out.push(VlanBridgePayload {
            observation_shape: VlanObservationShape::PortMembership,
            bridge_domain: unknown(
                "bridge vlan output does not identify the master bridge for this port",
            ),
            vlan_id,
            vlan_name: unknown("bridge vlan output does not expose VLAN names"),
            operational_state: unknown("bridge vlan output does not expose VLAN operational state"),
            admin_state: unknown("bridge vlan output does not expose VLAN administrative state"),
            dynamic: unknown("bridge vlan output does not expose dynamic/static creation"),
            membership_plane: known(MembershipPlane::Operational),
            membership_availability: MembershipAvailability::KnownPopulated,
            members: vec![VlanMember {
                interface_name: interface,
                interface_key: None,
                tagged: known(!untagged),
                untagged: known(untagged),
                pvid: known(pvid),
                mode: unknown("bridge vlan output does not expose access/trunk mode"),
                attachment_kind: unknown(
                    "bridge vlan output does not expose interface attachment type",
                ),
            }],
            routed_interfaces: unknown("bridge vlan output does not expose routed/SVI attachment"),
            vrf: unknown("bridge vlan output does not expose VRF/network-instance attachment"),
            domain_entity_key: Some(key),
        });
    }
    report.collection_presence.insert(
        "bridge-vlan-lines".into(),
        if saw_row { "populated" } else { "empty" }.into(),
    );
    out
}

fn positive_empty_population(report: &VlanNormalizationReport) -> bool {
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

fn coverage_population(
    capture: &CommandCapture<'_>,
    report: &VlanNormalizationReport,
) -> ScopedPopulationCoverage {
    if report.refusal_reason.is_some() || capture.outcome == CaptureOutcome::Unknown {
        return ScopedPopulationCoverage::Unknown;
    }
    let mut positive = false;
    let mut bad = false;
    for state in report.collection_presence.values() {
        match state.as_str() {
            "empty" | "populated" => positive = true,
            _ => bad = true,
        }
    }
    if capture.outcome == CaptureOutcome::Complete
        && report.malformed_records == 0
        && report.input_records_omitted == 0
        && report.output_facts_omitted == 0
        && report.facts_rejected == 0
        && !bad
    {
        ScopedPopulationCoverage::Complete
    } else if bad && !positive && report.records_decoded == 0 {
        ScopedPopulationCoverage::Unknown
    } else {
        ScopedPopulationCoverage::Partial
    }
}

fn declared_scope_contract(profile: VlanSourceProfile) -> AcquisitionScopeContract {
    match profile {
        VlanSourceProfile::EosShowVlanBriefJson
        | VlanSourceProfile::NxosShowVlanBriefJsonNative
        | VlanSourceProfile::CumulusBridgeCVlanShow => {
            AcquisitionScopeContract::CompleteScopedTable
        }
    }
}

fn scoped_coverage(
    obs: &VlanObservation<'_>,
    report: &VlanNormalizationReport,
    facts: &[TypedFact],
    raw_sha256: Option<&str>,
) -> ScopedCaptureCoverage {
    let mut observed_interfaces = BTreeSet::new();
    let mut observed_forwarding_domains = BTreeSet::new();
    let mut observed_vlan_ids = BTreeSet::new();
    for fact in facts {
        let Some(fields) = &fact.payload.fields else {
            continue;
        };
        if let Ok(payload) = serde_json::from_value::<VlanBridgePayload>(fields.clone()) {
            observed_vlan_ids.insert(payload.vlan_id);
            if payload.bridge_domain.state == FieldAvailability::Known
                && let Some(domain) = payload.bridge_domain.value
            {
                observed_forwarding_domains.insert(domain);
            }
            for member in payload.members {
                observed_interfaces.insert(member.interface_name);
            }
        }
    }
    ScopedCaptureCoverage {
        device_id: obs.context.device_id.to_string(),
        scope: obs.context.scope.map(str::to_string),
        network_namespace: obs.context.network_namespace.map(str::to_string),
        collector_id: obs.context.collector_id.to_string(),
        source_kind: obs.profile.source_kind().to_string(),
        capture_id: obs.context.capture_id.to_string(),
        source_generation: Some(obs.context.session_epoch),
        acquisition_scope: obs.profile.label().to_string(),
        covered_fact_kind: FACT_KIND_VLAN_BRIDGE_DOMAIN.to_string(),
        declared_scope: declared_scope_contract(obs.profile),
        capture_outcome: obs.capture.outcome,
        population_coverage: coverage_population(&obs.capture, report),
        positive_empty_population: report.empty_records,
        observation_time: Some(obs.context.observed_at.to_string()),
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
                    locator: Some(obs.profile.label().to_string()),
                }]
            })
            .unwrap_or_default(),
    }
}

fn empty_coverage(
    obs: &VlanObservation<'_>,
    report: &VlanNormalizationReport,
) -> ScopedCaptureCoverage {
    scoped_coverage(obs, report, &[], None)
}

/// Normalize one VLAN/bridge capture into typed facts. Malformed rows are
/// counted and omitted; no VLAN name, state, mode, bridge domain or VRF is
/// inferred from naming conventions.
pub fn normalize_vlan(obs: &VlanObservation<'_>) -> VlanNormalizationOutput {
    let mut report = VlanNormalizationReport {
        profile: obs.profile.label().to_string(),
        capture_outcome: capture_outcome_label(obs.capture.outcome).to_string(),
        input_bytes: obs.capture.output.len(),
        ..Default::default()
    };
    if obs.capture.output.len() > MAX_INPUT_BYTES {
        report.refusal_reason = Some("input exceeds maximum byte budget before parsing".into());
        let coverage = empty_coverage(obs, &report);
        return VlanNormalizationOutput {
            facts: vec![],
            report,
            coverage,
        };
    }
    let raw_sha256 = sha256_hex(obs.capture.output);
    if let Some(err) = context_error(&obs.context) {
        report.refusal_reason = Some(err);
        let coverage = empty_coverage(obs, &report);
        return VlanNormalizationOutput {
            facts: vec![],
            report,
            coverage,
        };
    }
    if !obs.capture.outcome.usable() {
        report.refusal_reason = Some("capture outcome is not usable".into());
        let coverage = empty_coverage(obs, &report);
        return VlanNormalizationOutput {
            facts: vec![],
            report,
            coverage,
        };
    }

    let payloads = match obs.profile {
        VlanSourceProfile::EosShowVlanBriefJson => match parse_json_input(obs.capture.output) {
            Ok(root) => parse_eos_vlan(&root, &mut report),
            Err(e) => {
                report.refusal_reason = Some(e);
                vec![]
            }
        },
        VlanSourceProfile::NxosShowVlanBriefJsonNative => {
            match parse_json_input(obs.capture.output) {
                Ok(root) => parse_nxos_vlan(&root, &mut report),
                Err(e) => {
                    report.refusal_reason = Some(e);
                    vec![]
                }
            }
        }
        VlanSourceProfile::CumulusBridgeCVlanShow => {
            parse_cumulus_vlan(obs.capture.output, &mut report)
        }
    };
    report.empty_records = positive_empty_population(&report);

    let comp = if obs.capture.outcome == CaptureOutcome::Complete
        && report.malformed_records == 0
        && report.input_records_omitted == 0
        && report.output_facts_omitted == 0
    {
        Completeness::Complete
    } else if obs.capture.outcome == CaptureOutcome::Unknown {
        Completeness::Unknown
    } else {
        Completeness::Partial
    };
    let mut facts = Vec::new();
    let mut entity_payloads: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut port_pvid_vlans: BTreeMap<String, BTreeMap<bool, BTreeSet<u32>>> = BTreeMap::new();
    for payload in &payloads {
        if let Some(entity) = &payload.domain_entity_key {
            entity_payloads
                .entry(entity.clone())
                .or_default()
                .insert(serde_json::to_string(payload).unwrap_or_default());
        }
        if payload.observation_shape == VlanObservationShape::PortMembership
            && let Some(member) = payload.members.first()
            && member.pvid.state == FieldAvailability::Known
            && let Some(pvid) = member.pvid.value
        {
            port_pvid_vlans
                .entry(member.interface_name.clone())
                .or_default()
                .entry(pvid)
                .or_default()
                .insert(payload.vlan_id);
        }
    }
    report.conflicting_records = entity_payloads
        .values()
        .filter(|variants| variants.len() > 1)
        .count();
    report.conflicting_pvid_records = port_pvid_vlans
        .values()
        .filter(|by_value| by_value.get(&true).is_some_and(|v| v.len() > 1))
        .count();
    let mut seen = BTreeSet::new();
    for payload in payloads.into_iter() {
        if facts.len() >= MAX_OUTPUT_FACTS {
            report.output_facts_omitted += 1;
            continue;
        }
        if validate_vlan_typed(&payload).is_err() {
            report.facts_rejected += 1;
            continue;
        }
        let entity = payload
            .domain_entity_key
            .clone()
            .unwrap_or_else(|| bridge_vlan_entity_key("unknown", payload.vlan_id));
        let payload_key = serde_json::to_string(&payload).unwrap_or_default();
        if !seen.insert((entity.clone(), payload_key)) {
            report.duplicate_records += 1;
            continue;
        }
        let payload_completeness = if comp == Completeness::Complete
            && !matches!(
                payload.membership_availability,
                MembershipAvailability::KnownEmpty | MembershipAvailability::KnownPopulated
            ) {
            Completeness::Partial
        } else {
            comp
        };
        let fact = fact_for_vlan_payload(
            &obs.context,
            obs.profile.source_kind(),
            &entity,
            payload,
            &raw_sha256,
            obs.profile.label(),
            payload_completeness,
            0,
        );
        if fact.validate().is_ok() {
            facts.push(fact);
        } else {
            report.facts_rejected += 1;
        }
    }
    report.facts_emitted = facts.len();
    let coverage = scoped_coverage(obs, &report, &facts, Some(&raw_sha256));
    VlanNormalizationOutput {
        facts,
        report,
        coverage,
    }
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
    fn genuine_eos_vlan_fixture_expected_values() {
        let input = include_str!("../tests/fixtures/bn024/vlan/eos-leaf02-show-vlan-brief.json");
        let out = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::EosShowVlanBriefJson,
            capture: complete(input),
            context: ctx(),
        });
        assert_eq!(out.report.malformed_records, 0);
        assert_eq!(out.facts.len(), 5);
        let payload: VlanBridgePayload =
            serde_json::from_value(out.facts[1].payload.fields.clone().unwrap()).unwrap();
        assert_eq!(payload.vlan_id, 10);
        assert_eq!(payload.vlan_name.value.as_deref(), Some("VLAN0010"));
        assert_eq!(
            payload.operational_state.value,
            Some(VlanOperationalState::Active)
        );
        let members: BTreeSet<_> = payload
            .members
            .iter()
            .map(|m| m.interface_name.as_str())
            .collect();
        assert_eq!(
            members,
            BTreeSet::from(["Cpu", "Port-Channel1", "Port-Channel3", "Vxlan1"])
        );
        assert!(payload.members[0].tagged.state == FieldAvailability::Unknown);
    }

    #[test]
    fn genuine_nxos_vlan_fixture_expected_values() {
        let input = include_str!(
            "../tests/fixtures/bn024/vlan/nxos-leaf03-show-vlan-brief-json-native.json"
        );
        let out = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::NxosShowVlanBriefJsonNative,
            capture: complete(input),
            context: ctx(),
        });
        assert_eq!(out.report.malformed_records, 0);
        assert_eq!(out.facts.len(), 4);
        let payload: VlanBridgePayload =
            serde_json::from_value(out.facts[1].payload.fields.clone().unwrap()).unwrap();
        assert_eq!(payload.vlan_id, 20);
        assert_eq!(payload.vlan_name.value.as_deref(), Some("VLAN0020"));
        assert_eq!(payload.admin_state.value, Some(VlanAdminState::NoShutdown));
        assert!(
            payload
                .members
                .iter()
                .any(|m| m.interface_name == "port-channel3")
        );
    }

    #[test]
    fn genuine_cumulus_vlan_fixture_preserves_tagging_and_pvid() {
        let input =
            include_str!("../tests/fixtures/bn024/vlan/cumulus-leaf01-bridge-c-vlan-show.txt");
        let out = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::CumulusBridgeCVlanShow,
            capture: complete(input),
            context: ctx(),
        });
        assert_eq!(out.report.malformed_records, 0);
        assert_eq!(out.facts.len(), 7);
        let peerlink1 = out
            .facts
            .iter()
            .map(|f| {
                serde_json::from_value::<VlanBridgePayload>(f.payload.fields.clone().unwrap())
                    .unwrap()
            })
            .find(|p| p.members[0].interface_name == "peerlink" && p.vlan_id == 1)
            .unwrap();
        assert_eq!(peerlink1.members[0].pvid.value, Some(true));
        assert_eq!(peerlink1.members[0].untagged.value, Some(true));
        assert_eq!(peerlink1.members[0].tagged.value, Some(false));
        let peerlink13 = out
            .facts
            .iter()
            .map(|f| {
                serde_json::from_value::<VlanBridgePayload>(f.payload.fields.clone().unwrap())
                    .unwrap()
            })
            .find(|p| p.members[0].interface_name == "peerlink" && p.vlan_id == 13)
            .unwrap();
        assert_eq!(peerlink13.members[0].pvid.value, Some(false));
        assert_eq!(peerlink13.members[0].untagged.value, Some(false));
        assert_eq!(peerlink13.members[0].tagged.value, Some(true));
        assert_eq!(
            peerlink13.bridge_domain.state,
            FieldAvailability::Unknown,
            "VLAN number alone must not manufacture a Linux bridge domain"
        );
    }

    #[test]
    fn eos_missing_interfaces_is_explicit_missing_not_empty_membership() {
        let input = r#"{"vlans":{"10":{"name":"VLAN0010","status":"active","dynamic":false}}}"#;
        let out = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::EosShowVlanBriefJson,
            capture: complete(input),
            context: ctx(),
        });
        assert_eq!(out.facts.len(), 1);
        let payload: VlanBridgePayload =
            serde_json::from_value(out.facts[0].payload.fields.clone().unwrap()).unwrap();
        assert_eq!(
            payload.membership_availability,
            MembershipAvailability::Missing
        );
        assert!(payload.members.is_empty());
        assert_eq!(
            out.facts[0].quality.completeness,
            crate::typed_facts::Completeness::Partial
        );
    }

    #[test]
    fn nxos_partially_malformed_member_array_preserves_valid_members_as_partial() {
        let input = r#"{"TABLE_vlanbriefxbrief":{"ROW_vlanbriefxbrief":[
            {"vlanshowbr-vlanid":20,"vlanshowbr-vlanname":"VLAN0020","vlanshowbr-vlanstate":"active","vlanshowbr-shutstate":"noshutdown","vlanshowplist-ifidx":["port-channel3",7]}
        ]}}"#;
        let out = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::NxosShowVlanBriefJsonNative,
            capture: complete(input),
            context: ctx(),
        });
        assert_eq!(out.facts.len(), 1);
        let payload: VlanBridgePayload =
            serde_json::from_value(out.facts[0].payload.fields.clone().unwrap()).unwrap();
        assert_eq!(
            payload.membership_availability,
            MembershipAvailability::Partial
        );
        assert_eq!(payload.members.len(), 1);
        assert_eq!(payload.members[0].interface_name, "port-channel3");
    }

    #[test]
    fn cumulus_exact_duplicate_rows_dedup_but_conflicting_rows_are_counted() {
        let input =
            "port\tvlan ids\nbond01 13 PVID Egress Untagged\nbond01 13 PVID Egress Untagged\n";
        let out = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::CumulusBridgeCVlanShow,
            capture: complete(input),
            context: ctx(),
        });
        assert_eq!(out.facts.len(), 1);
        assert_eq!(out.report.duplicate_records, 1);

        let conflicting = "port\tvlan ids\nbond01 13 PVID Egress Untagged\nbond01 13\n";
        let out = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::CumulusBridgeCVlanShow,
            capture: complete(conflicting),
            context: ctx(),
        });
        assert_eq!(out.facts.len(), 2);
        assert_eq!(out.report.conflicting_records, 1);
    }

    #[test]
    fn cumulus_header_only_is_positive_empty_population() {
        let input = "port\tvlan ids\n";
        let out = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::CumulusBridgeCVlanShow,
            capture: complete(input),
            context: ctx(),
        });
        assert!(out.facts.is_empty());
        assert!(out.report.empty_records);
        assert_eq!(out.report.malformed_records, 0);
    }

    #[test]
    fn cumulus_diagnostic_or_missing_header_is_rejected_without_facts() {
        for input in [
            "Error: permission denied\nport\tvlan ids\nbond01 13\n",
            "bridge vlan output missing header\nbond01 13\n",
            "",
        ] {
            let out = normalize_vlan(&VlanObservation {
                profile: VlanSourceProfile::CumulusBridgeCVlanShow,
                capture: complete(input),
                context: ctx(),
            });
            assert!(out.facts.is_empty(), "input {input:?}");
            assert!(out.report.malformed_records > 0, "input {input:?}");
        }
    }

    #[test]
    fn cumulus_compressed_ranges_and_bad_continuation_are_malformed_not_invented() {
        let input = "port\tvlan ids\nbond01 10-12\n 13 PVID\norphan-continuation-bad\n";
        let out = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::CumulusBridgeCVlanShow,
            capture: complete(input),
            context: ctx(),
        });
        assert!(out.facts.len() <= 1);
        assert!(out.report.malformed_records >= 2);
        assert!(
            out.facts.iter().all(|fact| {
                let payload: VlanBridgePayload =
                    serde_json::from_value(fact.payload.fields.clone().unwrap()).unwrap();
                payload.vlan_id < 10 || payload.vlan_id > 12
            }),
            "compressed range endpoints must not be invented"
        );
    }
}
