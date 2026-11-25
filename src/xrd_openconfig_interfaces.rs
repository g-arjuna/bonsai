//! BN-025 — Cisco IOS XRd OpenConfig interface-state normalization.
//!
//! This module consumes a versioned gNMI subscribe-once JSON representation for
//! `/openconfig-interfaces:interfaces/interface[name=*]` and emits the shared
//! typed `interface.state` fact family. It is explicitly scoped to the genuine
//! Cisco XRd control-plane image evidence in
//! `tests/fixtures/bn025/xrd-openconfig-interfaces/`; it is not a generic Cisco
//! IOS XE/IOS XR qualification claim and it is not yet a production acquisition
//! authority.
//!
//! Source semantics established from the genuine XRd 24.4.2 capture:
//!
//! * gNMI returns a sequence of independent JSON-IETF subtree notifications,
//!   followed by `sync_response`. The sequence is not an atomic interface
//!   snapshot. `sync_response` establishes initial-update completion for the
//!   subscribed path scope, not a device-wide population denominator.
//! * Interface identity is the OpenConfig list key `interfaces/interface/name`.
//! * `state/admin-status`, `state/oper-status`, `state/ifindex`, `state/mtu`,
//!   `state/type`, `state/logical` and `state/management` are interface-level
//!   operational state.
//! * XRd reports the interface-level MTU as the L2 frame MTU (for example
//!   1514 on the management Ethernet interface). Subinterface IPv4 MTU (1500)
//!   is a distinct protocol-layer datum and is deliberately not merged into the
//!   common interface MTU field.
//! * Ethernet MAC and speed evidence is under the namespaced
//!   `openconfig-if-ethernet:ethernet/state` subtree. `SPEED_*` tokens are
//!   interpreted in megabits per second using the OpenConfig speed identity
//!   names. `negotiated-port-speed` is preferred; when it is unknown and
//!   auto-negotiation is disabled, `port-speed` is used as the forced port
//!   speed. `SPEED_UNKNOWN` remains explicit unavailability.
//! * OpenConfig subinterface index 0 represents addressing on the base
//!   interface in this capture and is folded into that interface only for IP
//!   addressing. Non-zero subinterfaces are not collapsed into ambiguous
//!   dotted strings; they are counted as unsupported in this first slice and
//!   require a deliberate structured identity extension before emission.
//!
//! Unknown, invalid, unsupported and absent remain distinct. Ignored vendor or
//! OpenConfig augmentation detail is preserved only in raw evidence, never
//! silently promoted into typed facts.

use base64::Engine as _;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

use crate::interface_facts::{
    AdminState, ConflictResolution, FACT_KIND_INTERFACE_STATE, FieldAvailability,
    InterfaceStatePayload, InterfaceType, ObservedField, OperState, SourceFieldConflict,
    SpeedObservation, SpeedUnit, valid_name,
};
use crate::interface_normalizer::{
    CaptureOutcome, CommandCapture, FieldCoverage, InterfaceDiagnostic, NormalizationOutput,
    NormalizationReport, NormalizerLimits, ObservationContext, SourceCaptureReport, addr_field,
    mac_of, parse_rfc3339, sanitize_bounded, sha256_hex, unavailable,
};
use crate::typed_facts::{
    Completeness, EvidenceRef, FactPayload, FactSource, FactSubject, Lifecycle, PayloadState,
    Quality, TrustClass, TypedFact,
};

pub const XRD_OC_PARSER_REVISION: &str = "xrd-oc-iface-2026.11-r1";
pub const XRD_OC_MAPPING_REVISION: &str = "bn025-iface-map-1";
pub const XRD_OC_REQUIREMENT_REVISION: &str = "bn025-iface-requirements-r1";
pub const XRD_OC_ACQUISITION_CONTRACT: &str = "xrd_openconfig_gnmi_subscribe_once_interfaces";
const MAX_UPDATES_PER_OBSERVATION: usize = 8192;

/// Versioned input for the XRd OpenConfig adapter. `capture.output` is the
/// JSON representation of a subscribe-once response sequence as emitted by the
/// bounded gNMI evidence tool; production collectors may emit the same logical
/// shape without using that helper. No SSH command semantics are attached.
pub struct XrdOpenConfigInterfaceObservation<'a> {
    pub capture: CommandCapture<'a>,
    pub context: ObservationContext<'a>,
    /// Explicit logical acquisition contract used to obtain this capture.
    pub acquisition_contract: &'a str,
}

#[derive(Debug)]
struct DecodedUpdate {
    interface_name: String,
    value: Value,
}

#[derive(Debug)]
struct MergedInterface {
    value: Value,
    updates: usize,
}

impl Default for MergedInterface {
    fn default() -> Self {
        Self {
            value: Value::Object(serde_json::Map::new()),
            updates: 0,
        }
    }
}

fn strip_namespace(key: &str) -> &str {
    key.rsplit_once(':').map(|(_, tail)| tail).unwrap_or(key)
}

fn canonicalized_value(source: &Value) -> Value {
    match source {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| (strip_namespace(key).to_string(), canonicalized_value(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(canonicalized_value).collect()),
        _ => source.clone(),
    }
}

fn canonical_object_merge(
    target: &mut Value,
    source: &Value,
    path: &str,
    conflicts: &mut BTreeMap<String, (String, String)>,
) {
    match (target, source) {
        (Value::Object(dst), Value::Object(src)) => {
            for (raw_key, src_value) in src {
                let key = strip_namespace(raw_key).to_string();
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match dst.entry(key) {
                    serde_json::map::Entry::Vacant(entry) => {
                        entry.insert(canonicalized_value(src_value));
                    }
                    serde_json::map::Entry::Occupied(mut entry) => {
                        canonical_object_merge(entry.get_mut(), src_value, &child_path, conflicts);
                    }
                }
            }
        }
        (dst, src) => {
            if dst != src && conflicts.len() < 128 {
                conflicts.insert(path.to_string(), (bounded_json(dst), bounded_json(src)));
            }
        }
    }
}

fn bounded_json(v: &Value) -> String {
    sanitize_bounded(&v.to_string(), 200)
}

fn decode_capture(raw: &str) -> Result<(Vec<DecodedUpdate>, bool), String> {
    let top: Value = serde_json::from_str(raw).map_err(|e| format!("invalid capture JSON: {e}"))?;
    if top.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(
            "capture envelope does not report a successful subscribe-once operation".into(),
        );
    }
    let responses = top
        .get("responses")
        .and_then(Value::as_array)
        .ok_or_else(|| "capture envelope missing responses array".to_string())?;
    if responses.len() > MAX_UPDATES_PER_OBSERVATION {
        return Err(format!(
            "capture contains {} responses, exceeding bounded update ceiling {MAX_UPDATES_PER_OBSERVATION}",
            responses.len()
        ));
    }

    let mut updates = Vec::new();
    let mut sync_complete = false;
    for (response_index, response) in responses.iter().enumerate() {
        if response.get("sync_response").and_then(Value::as_bool) == Some(true) {
            sync_complete = true;
            continue;
        }
        let Some(update_list) = response.get("update").and_then(Value::as_array) else {
            continue;
        };
        for (update_index, update) in update_list.iter().enumerate() {
            let elems = update
                .pointer("/path/elem")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    format!(
                        "response {response_index} update {update_index} has no gNMI path elements"
                    )
                })?;
            if elems.len() != 2
                || elems[0].get("name").and_then(Value::as_str) != Some("interfaces")
                || elems[1].get("name").and_then(Value::as_str) != Some("interface")
            {
                return Err(format!(
                    "response {response_index} update {update_index} is outside the qualified interfaces/interface[name=*] root subtree"
                ));
            }
            let interface_name = elems[1]
                .pointer("/key/name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    format!(
                        "response {response_index} update {update_index} lacks interface name key"
                    )
                })?
                .to_string();
            let encoded = update
                .pointer("/val/json_ietf_val")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    format!("response {response_index} update {update_index} lacks json_ietf_val")
                })?;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|e| format!("response {response_index} update {update_index} has invalid base64 JSON-IETF value: {e}"))?;
            let value: Value = serde_json::from_slice(&decoded).map_err(|e| {
                format!("response {response_index} update {update_index} has invalid JSON-IETF payload: {e}")
            })?;
            if !value.is_object() {
                return Err(format!(
                    "response {response_index} update {update_index} JSON-IETF payload is not an object"
                ));
            }
            updates.push(DecodedUpdate {
                interface_name,
                value,
            });
        }
    }
    Ok((updates, sync_complete))
}

fn get<'a>(object: &'a Value, key: &str) -> Option<&'a Value> {
    object.get(key)
}

fn object_at<'a>(object: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = object;
    for key in path {
        cur = get(cur, key)?;
    }
    Some(cur)
}

fn bool_at(object: &Value, path: &[&str]) -> Option<bool> {
    object_at(object, path)?.as_bool()
}

fn string_at<'a>(object: &'a Value, path: &[&str]) -> Option<&'a str> {
    object_at(object, path)?.as_str()
}

fn u64_at(object: &Value, path: &[&str]) -> Result<Option<u64>, ()> {
    let Some(v) = object_at(object, path) else {
        return Ok(None);
    };
    if let Some(n) = v.as_u64() {
        return Ok(Some(n));
    }
    if let Some(s) = v.as_str() {
        return s.parse::<u64>().map(Some).map_err(|_| ());
    }
    Err(())
}

fn conflicted(paths: &BTreeSet<String>, leaf: &str) -> bool {
    paths
        .iter()
        .any(|p| p == leaf || p.ends_with(&format!(".{leaf}")))
}

fn admin_state(state: Option<&Value>, conflicts: &BTreeSet<String>) -> ObservedField<AdminState> {
    if conflicted(conflicts, "admin-status") {
        return unavailable(
            FieldAvailability::Invalid,
            Some("conflicting gNMI updates for admin-status"),
        );
    }
    match string_at(state.unwrap_or(&Value::Null), &["admin-status"])
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("up") => ObservedField::known(AdminState::Up),
        Some("down") => ObservedField::known(AdminState::Down),
        Some(_) => unavailable(
            FieldAvailability::Invalid,
            Some("unrecognized OpenConfig admin-status token"),
        ),
        None => unavailable(
            FieldAvailability::Unknown,
            Some("admin-status not observed"),
        ),
    }
}

fn oper_state(state: Option<&Value>, conflicts: &BTreeSet<String>) -> ObservedField<OperState> {
    if conflicted(conflicts, "oper-status") {
        return unavailable(
            FieldAvailability::Invalid,
            Some("conflicting gNMI updates for oper-status"),
        );
    }
    let raw = string_at(state.unwrap_or(&Value::Null), &["oper-status"]);
    match raw.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("up") => ObservedField::known(OperState::Up),
        Some("down") => ObservedField::known(OperState::Down),
        Some("lower_layer_down") | Some("lowerlayerdown") => {
            ObservedField::known(OperState::LowerLayerDown)
        }
        Some("dormant") => ObservedField::known(OperState::Dormant),
        Some("testing") => ObservedField::known(OperState::Testing),
        Some("unknown") => ObservedField::known(OperState::Unknown),
        Some(_) => unavailable(
            FieldAvailability::Invalid,
            Some("unrecognized OpenConfig oper-status token"),
        ),
        None => unavailable(FieldAvailability::Unknown, Some("oper-status not observed")),
    }
}

fn ifindex(state: Option<&Value>, conflicts: &BTreeSet<String>) -> ObservedField<u64> {
    if conflicted(conflicts, "ifindex") {
        return unavailable(
            FieldAvailability::Invalid,
            Some("conflicting gNMI updates for ifindex"),
        );
    }
    match u64_at(state.unwrap_or(&Value::Null), &["ifindex"]) {
        Ok(Some(v)) if v > 0 => ObservedField::known(v),
        Ok(Some(_)) => unavailable(
            FieldAvailability::Invalid,
            Some("ifindex 0 failed validation"),
        ),
        Ok(None) => unavailable(FieldAvailability::Unknown, Some("ifindex not observed")),
        Err(_) => unavailable(
            FieldAvailability::Invalid,
            Some("ifindex had non-numeric type"),
        ),
    }
}

fn mtu(state: Option<&Value>, conflicts: &BTreeSet<String>) -> ObservedField<u32> {
    if conflicted(conflicts, "mtu") {
        return unavailable(
            FieldAvailability::Invalid,
            Some("conflicting gNMI updates for interface mtu"),
        );
    }
    match u64_at(state.unwrap_or(&Value::Null), &["mtu"]) {
        Ok(Some(v)) if v > 0 && v <= u32::MAX as u64 => ObservedField::known(v as u32),
        Ok(Some(_)) => unavailable(
            FieldAvailability::Invalid,
            Some("interface mtu out of range"),
        ),
        Ok(None) => unavailable(
            FieldAvailability::Unknown,
            Some("interface mtu not observed"),
        ),
        Err(_) => unavailable(
            FieldAvailability::Invalid,
            Some("interface mtu had non-numeric type"),
        ),
    }
}

fn interface_type(
    state: Option<&Value>,
    ifname: &str,
    conflicts: &BTreeSet<String>,
) -> ObservedField<InterfaceType> {
    if conflicted(conflicts, "type") {
        return unavailable(
            FieldAvailability::Invalid,
            Some("conflicting gNMI updates for interface type"),
        );
    }
    let raw = string_at(state.unwrap_or(&Value::Null), &["type"]);
    let logical = state.and_then(|s| bool_at(s, &["logical"]));
    let Some(raw) = raw else {
        return unavailable(
            FieldAvailability::Unknown,
            Some("interface type not observed"),
        );
    };
    let ty = strip_namespace(raw).to_ascii_lowercase();
    let mapped = match (ty.as_str(), logical) {
        ("ethernetcsmacd", Some(false)) => Some(InterfaceType::Physical),
        ("ethernetcsmacd", Some(true)) => Some(InterfaceType::Virtual),
        ("softwareloopback", _) => Some(InterfaceType::Loopback),
        ("other", Some(true)) => Some(InterfaceType::Virtual),
        ("other", _) => Some(InterfaceType::Unknown),
        _ => None,
    };
    if ty == "ethernetcsmacd" && logical.is_none() {
        return unavailable(
            FieldAvailability::Unknown,
            Some("ethernet interface type observed but physical/logical state not observed"),
        );
    }
    match mapped {
        Some(InterfaceType::Unknown) => unavailable(
            FieldAvailability::Unknown,
            Some(format!(
                "OpenConfig type '{raw}' has no safe common classification for '{ifname}'"
            )),
        ),
        Some(v) => ObservedField::known(v),
        None => unavailable(
            FieldAvailability::Unknown,
            Some(format!(
                "unmapped OpenConfig interface type '{raw}' for '{ifname}'"
            )),
        ),
    }
}

fn speed_from_token(token: &str) -> Option<u64> {
    match strip_namespace(token).to_ascii_uppercase().as_str() {
        "SPEED_10MB" => Some(10),
        "SPEED_100MB" => Some(100),
        "SPEED_1GB" => Some(1_000),
        "SPEED_2500MB" | "SPEED_2_5GB" => Some(2_500),
        "SPEED_5GB" => Some(5_000),
        "SPEED_10GB" => Some(10_000),
        "SPEED_25GB" => Some(25_000),
        "SPEED_40GB" => Some(40_000),
        "SPEED_50GB" => Some(50_000),
        "SPEED_100GB" => Some(100_000),
        "SPEED_200GB" => Some(200_000),
        "SPEED_400GB" => Some(400_000),
        "SPEED_800GB" => Some(800_000),
        _ => None,
    }
}

fn speed(
    ethernet_state: Option<&Value>,
    conflicts: &BTreeSet<String>,
) -> ObservedField<SpeedObservation> {
    if conflicted(conflicts, "port-speed") || conflicted(conflicts, "negotiated-port-speed") {
        return unavailable(
            FieldAvailability::Invalid,
            Some("conflicting gNMI updates for ethernet speed"),
        );
    }
    let Some(state) = ethernet_state else {
        return unavailable(
            FieldAvailability::Unsupported,
            Some("ethernet state subtree not observed"),
        );
    };
    let negotiated = string_at(state, &["negotiated-port-speed"]);
    if let Some(token) = negotiated {
        if strip_namespace(token).eq_ignore_ascii_case("SPEED_UNKNOWN") {
            // Fall through to forced port-speed only when negotiation is off.
        } else if let Some(v) = speed_from_token(token) {
            return ObservedField::known(SpeedObservation {
                value: v,
                unit: SpeedUnit::MegabitsPerSecond,
            });
        } else {
            return unavailable(
                FieldAvailability::Invalid,
                Some(format!(
                    "unrecognized negotiated-port-speed token '{token}'"
                )),
            );
        }
    }
    let forced = string_at(state, &["port-speed"]);
    let auto_negotiate = bool_at(state, &["auto-negotiate"]);
    match (forced, auto_negotiate) {
        (Some(token), Some(false)) if speed_from_token(token).is_some() => {
            ObservedField::known(SpeedObservation {
                value: speed_from_token(token).expect("checked above"),
                unit: SpeedUnit::MegabitsPerSecond,
            })
        }
        (Some(token), Some(false))
            if strip_namespace(token).eq_ignore_ascii_case("SPEED_UNKNOWN") =>
        {
            unavailable(
                FieldAvailability::Unknown,
                Some("device reported SPEED_UNKNOWN"),
            )
        }
        (Some(token), Some(false)) => unavailable(
            FieldAvailability::Invalid,
            Some(format!("unrecognized forced port-speed token '{token}'")),
        ),
        (Some(_), _) => unavailable(
            FieldAvailability::Unknown,
            Some(
                "negotiated speed unknown and auto-negotiation state does not establish forced port speed",
            ),
        ),
        (None, _) => unavailable(
            FieldAvailability::Unknown,
            Some("ethernet speed not observed"),
        ),
    }
}

fn singleton_or_list_entries<'a>(value: Option<&'a Value>, identity_key: &str) -> Vec<&'a Value> {
    let Some(value) = value else {
        return vec![];
    };
    if let Some(items) = value.as_array() {
        return items.iter().collect();
    }
    if let Some(object) = value.as_object() {
        if object.contains_key(identity_key) {
            return vec![value];
        }
        return object.values().filter(|v| v.is_object()).collect();
    }
    vec![]
}

fn extract_addresses(interface: &Value) -> (Vec<String>, Vec<String>, usize, usize) {
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    let mut unsupported_subinterfaces = 0usize;
    let mut malformed_entries = 0usize;
    let subinterfaces = object_at(interface, &["subinterfaces"])
        .map(|v| singleton_or_list_entries(get(v, "subinterface"), "index"))
        .unwrap_or_default();
    for subinterface in subinterfaces {
        let index = match u64_at(subinterface, &["index"]) {
            Ok(Some(v)) => v,
            _ => {
                malformed_entries += 1;
                continue;
            }
        };
        if index != 0 {
            unsupported_subinterfaces += 1;
            continue;
        }
        for family in ["ipv4", "ipv6"] {
            let Some(ip_container) = object_at(subinterface, &[family]) else {
                continue;
            };
            let entries = object_at(ip_container, &["addresses"])
                .map(|v| singleton_or_list_entries(get(v, "address"), "ip"))
                .unwrap_or_default();
            for entry in entries.into_iter().take(64) {
                let ip = string_at(entry, &["state", "ip"]).or_else(|| string_at(entry, &["ip"]));
                let prefix = match u64_at(entry, &["state", "prefix-length"]) {
                    Ok(v) => v,
                    Err(_) => {
                        malformed_entries += 1;
                        continue;
                    }
                };
                match (ip, prefix) {
                    (Some(ip), Some(prefix)) => {
                        if family == "ipv4" {
                            v4.push(format!("{ip}/{prefix}"));
                        } else {
                            v6.push(format!("{ip}/{prefix}"));
                        }
                    }
                    _ => malformed_entries += 1,
                }
            }
        }
    }
    (v4, v6, unsupported_subinterfaces, malformed_entries)
}

fn record_field_state(
    coverage: &mut BTreeMap<String, FieldCoverage>,
    name: &str,
    state: FieldAvailability,
) {
    let entry = coverage.entry(name.into()).or_default();
    match state {
        FieldAvailability::Known => entry.known += 1,
        FieldAvailability::Unknown => entry.unknown += 1,
        FieldAvailability::Unsupported => entry.unsupported += 1,
        FieldAvailability::Invalid => entry.invalid += 1,
    }
}

fn context_error(context: &ObservationContext<'_>) -> Option<String> {
    if context.device_id.trim().is_empty() {
        return Some("device_id empty".into());
    }
    if context.collector_id.trim().is_empty() {
        return Some("collector_id empty".into());
    }
    if context.capture_id.trim().is_empty() {
        return Some("capture_id empty".into());
    }
    if parse_rfc3339(context.observed_at).is_none() {
        return Some("observed_at is not RFC-3339".into());
    }
    if let Some(nn) = context.network_namespace {
        if nn.trim().is_empty() {
            return Some("network_namespace empty".into());
        }
    }
    None
}

fn append_bounded_note(notes: &mut Vec<String>, text: impl AsRef<str>) {
    if notes.len() < 32 {
        notes.push(sanitize_bounded(text.as_ref(), 240));
    }
}

fn finish_bounded_notes(notes: Vec<String>) -> Vec<String> {
    notes.into_iter().take(32).collect()
}

fn finish_fact_quality_notes(notes: Vec<String>) -> Vec<String> {
    notes.into_iter().take(16).collect()
}

fn status_fact(
    obs: &XrdOpenConfigInterfaceObservation<'_>,
    state: PayloadState,
    completeness: Completeness,
    note: &str,
    evidence: Vec<EvidenceRef>,
) -> TypedFact {
    let mut fact = TypedFact {
        schema_version: crate::typed_facts::SCHEMA_VERSION,
        fact_id: String::new(),
        fact_kind: FACT_KIND_INTERFACE_STATE.into(),
        subject: FactSubject {
            device_id: obs.context.device_id.into(),
            scope: obs.context.scope.map(str::to_string),
            namespace: "interface".into(),
            network_namespace: obs.context.network_namespace.map(str::to_string),
            entity: None,
            vrf: None,
            afi_safi: None,
            plane: Some("operational".into()),
        },
        source: FactSource {
            collector_id: obs.context.collector_id.into(),
            source_kind: "gnmi_openconfig".into(),
            capture_id: obs.context.capture_id.into(),
            session_epoch: obs.context.session_epoch,
            sequence: obs.context.sequence,
            exporter_id: None,
        },
        observed_at: obs.context.observed_at.into(),
        ingested_at: None,
        valid_from: None,
        valid_until: None,
        payload: FactPayload {
            produced_by: "native_xrd_openconfig".into(),
            parser_revision: XRD_OC_PARSER_REVISION.into(),
            mapping_revision: Some(XRD_OC_MAPPING_REVISION.into()),
            fields: None,
            state: Some(state),
        },
        payload_sha256: String::new(),
        evidence_sha256: String::new(),
        quality: Quality {
            completeness,
            missing_fields: vec![],
            notes: finish_fact_quality_notes(vec![note.into()]),
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

fn invalid_output(
    obs: &XrdOpenConfigInterfaceObservation<'_>,
    reason: &str,
    state: PayloadState,
    bytes: usize,
    limits: NormalizerLimits,
) -> NormalizationOutput {
    let oversized = bytes > limits.max_input_bytes;
    let (kind, sha) = if oversized {
        (
            "capture_refused_input_bound",
            sha256_hex(format!("bn025-input-bound-exceeded:xrd_openconfig={bytes}").as_bytes()),
        )
    } else {
        (
            "raw_gnmi_subscribe_once_openconfig_interfaces",
            sha256_hex(obs.capture.output.as_bytes()),
        )
    };
    let evidence = vec![EvidenceRef {
        kind: kind.into(),
        sha256: sha,
        locator: None,
    }];
    let mut facts = vec![];
    let f = status_fact(obs, state, Completeness::Unknown, reason, evidence);
    if f.validate().is_ok() {
        facts.push(f);
    }
    let emitted = facts.len();
    NormalizationOutput {
        facts,
        interfaces: vec![],
        report: NormalizationReport {
            nos: "cisco_iosxr",
            applicability: "xrd 24.4.2 openconfig-interfaces gnmi subscribe-once",
            interfaces_observed: 0,
            records_decoded: None,
            entities_encountered: None,
            entities_accepted: Some(0),
            status_facts_emitted: Some(emitted),
            validated_field_coverage: Some(BTreeMap::new()),
            expected_interface_total: None,
            facts_emitted: emitted,
            duplicate_ifnames: 0,
            merge_conflicts: 0,
            unparsed_lines: 0,
            field_coverage: BTreeMap::new(),
            sources: vec![SourceCaptureReport {
                source: XRD_OC_ACQUISITION_CONTRACT,
                outcome: obs.capture.outcome,
                parsed: false,
                rejected: oversized,
                bytes,
                captured_at: obs.capture.captured_at.map(str::to_string),
            }],
            entities_omitted: 0,
            addresses_omitted: 0,
            invalid_entities: 0,
            facts_rejected: usize::from(emitted == 0),
            output_facts_omitted: 0,
            conflicts_omitted: 0,
            context_error: None,
            capture_skew_seconds: None,
            notes: finish_bounded_notes(vec![reason.into()]),
        },
    }
}

/// Normalize one XRd OpenConfig subscribe-once interface observation.
pub fn normalize_xrd_openconfig_interfaces(
    obs: &XrdOpenConfigInterfaceObservation<'_>,
    limits: NormalizerLimits,
) -> NormalizationOutput {
    let bytes = obs.capture.output.len();
    if let Some(err) = context_error(&obs.context) {
        let mut out = invalid_output(
            obs,
            "observation context invalid",
            PayloadState::InvalidInput,
            bytes,
            limits,
        );
        out.report.context_error = Some(err);
        out.facts.clear();
        out.report.facts_emitted = 0;
        out.report.facts_rejected = 0;
        out.report.status_facts_emitted = Some(0);
        return out;
    }
    if obs.acquisition_contract != XRD_OC_ACQUISITION_CONTRACT {
        return invalid_output(
            obs,
            "observation acquisition contract does not match the qualified XRd OpenConfig subscribe-once contract",
            PayloadState::InvalidInput,
            bytes,
            limits,
        );
    }
    if bytes > limits.max_input_bytes {
        return invalid_output(
            obs,
            "observation exceeded input byte bound; refused",
            PayloadState::InvalidInput,
            bytes,
            limits,
        );
    }
    if !obs.capture.outcome.usable() {
        return invalid_output(
            obs,
            "gNMI capture failed/unavailable/timed out; no usable observation",
            PayloadState::CaptureFailed,
            bytes,
            limits,
        );
    }
    if let Some(captured_at) = obs.capture.captured_at {
        if parse_rfc3339(captured_at).is_none() {
            return invalid_output(
                obs,
                "capture metadata 'captured_at' is not RFC-3339; observation rejected",
                PayloadState::InvalidInput,
                bytes,
                limits,
            );
        }
    }

    let (updates, sync_complete) = match decode_capture(obs.capture.output) {
        Ok(v) => v,
        Err(e) => {
            return invalid_output(
                obs,
                &format!("invalid or unexpected XRd OpenConfig gNMI capture: {e}"),
                PayloadState::InvalidInput,
                bytes,
                limits,
            );
        }
    };

    let sha = sha256_hex(obs.capture.output.as_bytes());
    let evidence = || {
        vec![EvidenceRef {
            kind: "raw_gnmi_subscribe_once_openconfig_interfaces".into(),
            sha256: sha.clone(),
            locator: None,
        }]
    };

    let mut merged: BTreeMap<String, MergedInterface> = BTreeMap::new();
    let mut conflict_maps: BTreeMap<String, BTreeMap<String, (String, String)>> = BTreeMap::new();
    for update in updates {
        if !valid_name(&update.interface_name) {
            continue;
        }
        let entry = merged.entry(update.interface_name.clone()).or_default();
        entry.updates += 1;
        let conflicts = conflict_maps
            .entry(update.interface_name.clone())
            .or_default();
        canonical_object_merge(&mut entry.value, &update.value, "", conflicts);
    }

    let records_decoded = merged.len();
    let mut facts = Vec::new();
    let mut diagnostics = Vec::new();
    let mut coverage: BTreeMap<String, FieldCoverage> = BTreeMap::new();
    let mut validated_coverage: BTreeMap<String, FieldCoverage> = BTreeMap::new();
    let mut entities_omitted = 0usize;
    let mut invalid_entities = 0usize;
    let mut output_facts_omitted = 0usize;
    let mut facts_rejected = 0usize;
    let mut rejected_samples = Vec::new();
    let mut entities_accepted = 0usize;
    let mut unsupported_subinterfaces_total = 0usize;
    let mut malformed_address_entries_total = 0usize;

    for (entity_index, (name, record)) in merged.iter().enumerate() {
        if entity_index >= limits.max_entities {
            entities_omitted += 1;
            continue;
        }
        if !valid_name(name) {
            invalid_entities += 1;
            continue;
        }
        if facts.len() >= limits.max_output_facts {
            output_facts_omitted += 1;
            continue;
        }

        let state = object_at(&record.value, &["state"]);
        let ethernet_state = object_at(&record.value, &["ethernet", "state"]);
        let raw_conflicts = conflict_maps.get(name).cloned().unwrap_or_default();
        let conflict_paths: BTreeSet<String> = raw_conflicts.keys().cloned().collect();
        let mut source_conflicts = Vec::new();
        for (path, (first, later)) in raw_conflicts.iter().take(8) {
            source_conflicts.push(SourceFieldConflict {
                field: path.clone(),
                effective_source: "gnmi_update_first".into(),
                effective_value: first.clone(),
                alternate_source: "gnmi_update_later".into(),
                alternate_value: later.clone(),
                resolution: ConflictResolution::Unresolved,
            });
        }

        let admin = admin_state(state, &conflict_paths);
        let oper = oper_state(state, &conflict_paths);
        let ifindex = ifindex(state, &conflict_paths);
        let itype = interface_type(state, name, &conflict_paths);
        let mtu = mtu(state, &conflict_paths);
        let speed = speed(ethernet_state, &conflict_paths);
        let mac_raw = ethernet_state.and_then(|s| string_at(s, &["mac-address"]));
        let mac = if conflicted(&conflict_paths, "mac-address") {
            unavailable(
                FieldAvailability::Invalid,
                Some("conflicting gNMI updates for ethernet mac-address"),
            )
        } else {
            mac_of(mac_raw)
        };
        let description = state
            .and_then(|s| string_at(s, &["description"]))
            .map(|d| sanitize_bounded(d, 240));
        let (v4, v6, unsupported_subinterfaces, malformed_addrs) = extract_addresses(&record.value);
        unsupported_subinterfaces_total += unsupported_subinterfaces;
        malformed_address_entries_total += malformed_addrs;
        let ip4 = if malformed_addrs > 0 {
            unavailable(
                FieldAvailability::Invalid,
                Some("subinterface ipv4 address entry malformed or incomplete"),
            )
        } else {
            addr_field(
                &v4,
                false,
                0,
                "no ipv4 address subtree observed on base interface",
            )
        };
        let ip6 = if malformed_addrs > 0 {
            unavailable(
                FieldAvailability::Invalid,
                Some("subinterface ipv6 address entry malformed or incomplete"),
            )
        } else {
            addr_field(
                &v6,
                true,
                0,
                "no ipv6 address subtree observed on base interface",
            )
        };

        let payload = InterfaceStatePayload {
            interface_name: name.clone(),
            normalized_name: None,
            administrative_state: admin.clone(),
            operational_state: oper.clone(),
            ifindex: ifindex.clone(),
            interface_type: itype.clone(),
            mtu: mtu.clone(),
            speed: speed.clone(),
            mac_address: mac.clone(),
            description,
            parent_interface: None,
            member_of: None,
            parent_relationship: None,
            master_relationship: None,
            source_conflicts,
            ip_addresses: Some(ip4.clone()),
            ipv6_addresses: Some(ip6.clone()),
        };

        let mut local_coverage: BTreeMap<String, FieldCoverage> = BTreeMap::new();
        for (fname, state_value) in [
            ("administrative_state", admin.state),
            ("operational_state", oper.state),
            ("ifindex", ifindex.state),
            ("interface_type", itype.state),
            ("mtu", mtu.state),
            ("speed", speed.state),
            ("mac_address", mac.state),
            ("ip_addresses", ip4.state),
            ("ipv6_addresses", ip6.state),
        ] {
            record_field_state(&mut coverage, fname, state_value);
            record_field_state(&mut local_coverage, fname, state_value);
        }

        let mut missing_fields = Vec::new();
        for (fname, state_value) in [
            ("administrative_state", admin.state),
            ("operational_state", oper.state),
            ("ifindex", ifindex.state),
            ("interface_type", itype.state),
            ("mtu", mtu.state),
        ] {
            if state_value != FieldAvailability::Known {
                missing_fields.push(fname.to_string());
            }
        }
        if ip4.state == FieldAvailability::Invalid {
            missing_fields.push("ip_addresses".into());
        }
        if ip6.state == FieldAvailability::Invalid {
            missing_fields.push("ipv6_addresses".into());
        }
        missing_fields.truncate(32);

        let mut qnotes = Vec::new();
        if unsupported_subinterfaces > 0 {
            qnotes.push(format!(
                "{unsupported_subinterfaces} non-zero OpenConfig subinterfaces not emitted in this slice; structured subinterface identity is not yet part of the common contract"
            ));
        }
        if !sync_complete {
            qnotes.push(
                "gNMI sync_response absent; initial update completion not established".into(),
            );
        }
        if record.updates > 1 {
            qnotes.push(format!(
                "{} non-atomic gNMI subtree notifications merged by interface identity",
                record.updates
            ));
        }
        if state.and_then(|s| bool_at(s, &["management"])) == Some(true) {
            qnotes
                .push("OpenConfig management=true; interface is a management-plane entity".into());
        }
        if state.and_then(|s| bool_at(s, &["logical"])) == Some(true) {
            qnotes.push("OpenConfig logical=true; entity is logical/pseudo rather than a front-panel physical port".into());
        }
        if !conflict_paths.is_empty() {
            qnotes.push(format!(
                "{} conflicting gNMI leaf updates preserved as source conflicts",
                conflict_paths.len()
            ));
        }

        let semantic_gap = !missing_fields.is_empty()
            || unsupported_subinterfaces > 0
            || malformed_addrs > 0
            || !conflict_paths.is_empty();
        let completeness = if obs.capture.outcome == CaptureOutcome::Unknown || !sync_complete {
            Completeness::Unknown
        } else if obs.capture.outcome == CaptureOutcome::Complete
            && !semantic_gap
            && record.updates == 1
        {
            Completeness::Complete
        } else {
            Completeness::Partial
        };

        let fields = match serde_json::to_value(&payload) {
            Ok(v) => v,
            Err(e) => {
                facts_rejected += 1;
                if rejected_samples.len() < 8 {
                    rejected_samples.push(format!("{name}: payload serialization failed: {e}"));
                }
                continue;
            }
        };
        let mut fact = TypedFact {
            schema_version: crate::typed_facts::SCHEMA_VERSION,
            fact_id: String::new(),
            fact_kind: FACT_KIND_INTERFACE_STATE.into(),
            subject: FactSubject {
                device_id: obs.context.device_id.into(),
                scope: obs.context.scope.map(str::to_string),
                namespace: "interface".into(),
                network_namespace: obs.context.network_namespace.map(str::to_string),
                entity: Some(name.clone()),
                vrf: None,
                afi_safi: None,
                plane: Some("operational".into()),
            },
            source: FactSource {
                collector_id: obs.context.collector_id.into(),
                source_kind: "gnmi_openconfig".into(),
                capture_id: obs.context.capture_id.into(),
                session_epoch: obs.context.session_epoch,
                sequence: obs.context.sequence,
                exporter_id: None,
            },
            observed_at: obs.context.observed_at.into(),
            ingested_at: None,
            valid_from: None,
            valid_until: None,
            payload: FactPayload {
                produced_by: "native_xrd_openconfig".into(),
                parser_revision: XRD_OC_PARSER_REVISION.into(),
                mapping_revision: Some(XRD_OC_MAPPING_REVISION.into()),
                fields: Some(fields),
                state: None,
            },
            payload_sha256: String::new(),
            evidence_sha256: String::new(),
            quality: Quality {
                completeness,
                missing_fields,
                notes: finish_fact_quality_notes(qnotes),
            },
            trust: TrustClass::DeviceAsserted,
            authority: None,
            lifecycle: Lifecycle::Active,
            withdrawn_reason: None,
            superseded_by: None,
            lifecycle_changed_at: None,
            evidence: evidence(),
        };
        fact.seal();
        let diagnostic = InterfaceDiagnostic {
            ifname: name.clone(),
            ifname_raw: None,
            vrf: None,
            ifindex: ifindex.value,
            admin_state_raw: state
                .and_then(|s| string_at(s, &["admin-status"]))
                .map(str::to_string),
            oper_state_raw: state
                .and_then(|s| string_at(s, &["oper-status"]))
                .map(str::to_string),
            type_raw: state
                .and_then(|s| string_at(s, &["type"]))
                .map(str::to_string),
            mtu_raw: mtu.value.map(|v| v.to_string()),
            speed_raw: ethernet_state
                .and_then(|s| string_at(s, &["negotiated-port-speed"]))
                .or_else(|| ethernet_state.and_then(|s| string_at(s, &["port-speed"])))
                .map(str::to_string),
            mac_raw: mac_raw.map(str::to_string),
            master_raw: None,
            vlan_raw: None,
            ip_addresses: v4,
            ipv6_addresses: v6,
            addresses_omitted: 0,
            conflict_fields: conflict_paths.iter().cloned().take(8).collect(),
        };
        diagnostics.push(diagnostic);
        if let Err(e) = fact.validate() {
            facts_rejected += 1;
            if rejected_samples.len() < 8 {
                rejected_samples.push(format!("{name}: {e:?}"));
            }
            continue;
        }
        for (fname, counts) in local_coverage {
            let aggregate = validated_coverage.entry(fname).or_default();
            aggregate.known += counts.known;
            aggregate.unknown += counts.unknown;
            aggregate.unsupported += counts.unsupported;
            aggregate.invalid += counts.invalid;
        }
        entities_accepted += 1;
        facts.push(fact);
    }

    let mut status_facts_emitted = 0usize;
    if facts.is_empty() {
        let (state, note) = if merged.is_empty() {
            (
                PayloadState::Empty,
                "gNMI subscribe-once completed with no interface update records".to_string(),
            )
        } else if output_facts_omitted > 0
            && invalid_entities == 0
            && entities_omitted == 0
            && facts_rejected == 0
        {
            (
                PayloadState::OutputBudgetExhausted,
                format!(
                    "observation decoded {records_decoded} interface identities, but the output-fact budget exhausted before any fact was emitted"
                ),
            )
        } else {
            (
                PayloadState::NoUsableRecords,
                format!(
                    "observation decoded {records_decoded} interface identities but produced no usable validated entity fact"
                ),
            )
        };
        let status_completeness = if state == PayloadState::Empty
            && obs.capture.outcome == CaptureOutcome::Complete
            && sync_complete
        {
            Completeness::Complete
        } else if state == PayloadState::OutputBudgetExhausted {
            Completeness::Partial
        } else {
            Completeness::Unknown
        };
        let f = status_fact(obs, state, status_completeness, &note, evidence());
        if f.validate().is_ok() {
            status_facts_emitted = 1;
            facts.push(f);
        } else {
            facts_rejected += 1;
        }
    }

    let mut notes = Vec::new();
    append_bounded_note(
        &mut notes,
        "genuine XRd 24.4.2 OpenConfig evidence; scoped offline normalization, not IOS XE or all IOS XR qualification",
    );
    append_bounded_note(
        &mut notes,
        "gNMI notifications are merged by interface identity and are not an atomic snapshot; sync_response is initial-update completion only",
    );
    append_bounded_note(
        &mut notes,
        "interface MTU is the interface-level OpenConfig state value; subinterface IPv4 MTU is not merged",
    );
    append_bounded_note(
        &mut notes,
        format!(
            "requirements revision {}: non-zero subinterfaces are explicit unsupported scope until structured subinterface identity exists",
            XRD_OC_REQUIREMENT_REVISION
        ),
    );
    if unsupported_subinterfaces_total > 0 {
        append_bounded_note(
            &mut notes,
            format!(
                "{unsupported_subinterfaces_total} non-zero subinterfaces omitted from typed output"
            ),
        );
    }
    if malformed_address_entries_total > 0 {
        append_bounded_note(
            &mut notes,
            format!("{malformed_address_entries_total} address entries malformed/incomplete"),
        );
    }
    if entities_omitted > 0 {
        append_bounded_note(
            &mut notes,
            format!("{entities_omitted} entities omitted at resource limit"),
        );
    }
    if output_facts_omitted > 0 {
        append_bounded_note(
            &mut notes,
            format!("{output_facts_omitted} facts omitted at effective output-fact budget"),
        );
    }
    if facts_rejected > 0 {
        append_bounded_note(
            &mut notes,
            format!("{facts_rejected} facts rejected by canonical validation"),
        );
        for sample in rejected_samples.into_iter().take(8) {
            append_bounded_note(&mut notes, format!("rejected: {sample}"));
        }
    }

    let facts_emitted = facts.len();
    NormalizationOutput {
        facts,
        interfaces: diagnostics,
        report: NormalizationReport {
            nos: "cisco_iosxr",
            applicability: "xrd 24.4.2 openconfig-interfaces gnmi subscribe-once",
            interfaces_observed: entities_accepted,
            records_decoded: Some(records_decoded),
            entities_encountered: Some(records_decoded),
            entities_accepted: Some(entities_accepted),
            status_facts_emitted: Some(status_facts_emitted),
            validated_field_coverage: Some(validated_coverage),
            expected_interface_total: None,
            facts_emitted,
            duplicate_ifnames: 0,
            merge_conflicts: conflict_maps.values().map(|m| m.len()).sum(),
            unparsed_lines: 0,
            field_coverage: coverage,
            sources: vec![SourceCaptureReport {
                source: XRD_OC_ACQUISITION_CONTRACT,
                outcome: obs.capture.outcome,
                parsed: true,
                rejected: false,
                bytes,
                captured_at: obs.capture.captured_at.map(str::to_string),
            }],
            entities_omitted,
            addresses_omitted: 0,
            invalid_entities,
            facts_rejected,
            output_facts_omitted,
            conflicts_omitted: conflict_maps
                .values()
                .map(|m| m.len().saturating_sub(8))
                .sum(),
            context_error: None,
            capture_skew_seconds: None,
            notes: finish_bounded_notes(notes),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENUINE: &str = include_str!(
        "../tests/fixtures/bn025/xrd-openconfig-interfaces/openconfig-interface-root-subscribe-xrd-24.4.2.json"
    );

    fn obs<'a>(json: &'a str) -> XrdOpenConfigInterfaceObservation<'a> {
        let mut context = ObservationContext::minimal(
            "xrd-bn025",
            "collector-test",
            "capture-xrd-oc-1",
            "2026-11-25T18:55:00Z",
        );
        context.session_epoch = 3;
        context.sequence = 9;
        context.network_namespace = Some("default");
        context.platform = Some("cisco.iosxr");
        context.release_raw = Some("24.4.2");
        XrdOpenConfigInterfaceObservation {
            capture: CommandCapture::new(json, CaptureOutcome::Complete)
                .captured_at("2026-11-25T18:55:00Z"),
            context,
            acquisition_contract: XRD_OC_ACQUISITION_CONTRACT,
        }
    }

    fn payload(fact: &TypedFact) -> InterfaceStatePayload {
        serde_json::from_value(fact.payload.fields.clone().expect("entity payload"))
            .expect("payload decodes")
    }

    #[test]
    fn genuine_xrd_capture_expected_values() {
        let out = normalize_xrd_openconfig_interfaces(&obs(GENUINE), NormalizerLimits::default());
        assert_eq!(out.report.records_decoded, Some(2));
        assert_eq!(out.report.entities_accepted, Some(2));
        assert_eq!(out.report.facts_rejected, 0);
        assert_eq!(out.report.expected_interface_total, None);
        assert_eq!(out.facts.len(), 2);

        let mgmt = out
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("MgmtEth0/RP0/CPU0/0"))
            .expect("management interface fact");
        let p = payload(mgmt);
        assert_eq!(p.administrative_state.value, Some(AdminState::Up));
        assert_eq!(p.operational_state.value, Some(OperState::Up));
        assert_eq!(p.ifindex.value, Some(3));
        assert_eq!(p.interface_type.value, Some(InterfaceType::Physical));
        assert_eq!(p.mtu.value, Some(1514));
        assert_eq!(
            p.speed.value,
            Some(SpeedObservation {
                value: 1000,
                unit: SpeedUnit::MegabitsPerSecond,
            })
        );
        assert_eq!(p.mac_address.value.as_deref(), Some("d2:72:77:bd:1d:65"));
        assert_eq!(
            p.ip_addresses.as_ref().and_then(|v| v.value.clone()),
            Some(vec!["172.100.106.21/24".to_string()])
        );
        assert_eq!(
            p.ipv6_addresses.as_ref().map(|v| v.state),
            Some(FieldAvailability::Unknown)
        );
        assert_eq!(mgmt.quality.completeness, Completeness::Partial);

        let null0 = out
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("Null0"))
            .expect("Null0 fact");
        let np = payload(null0);
        assert_eq!(np.interface_type.value, Some(InterfaceType::Virtual));
        assert_eq!(np.ifindex.value, Some(2));
        assert_eq!(np.mtu.value, Some(1500));
        assert_eq!(np.speed.state, FieldAvailability::Unsupported);
    }

    #[test]
    fn missing_sync_marks_completeness_unknown() {
        let mut v: Value = serde_json::from_str(GENUINE).unwrap();
        v["responses"].as_array_mut().unwrap().pop();
        let raw = v.to_string();
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert!(!out.facts.is_empty());
        assert!(
            out.facts
                .iter()
                .all(|f| f.quality.completeness == Completeness::Unknown)
        );
    }

    #[test]
    fn malformed_envelope_is_invalid_not_empty_success() {
        let out =
            normalize_xrd_openconfig_interfaces(&obs("not json"), NormalizerLimits::default());
        assert_eq!(out.report.records_decoded, None);
        assert_eq!(out.facts.len(), 1);
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
    }

    #[test]
    fn wrong_contract_is_rejected() {
        let mut o = obs(GENUINE);
        o.acquisition_contract = "wrong";
        let out = normalize_xrd_openconfig_interfaces(&o, NormalizerLimits::default());
        assert_eq!(out.facts.len(), 1);
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
    }
}
