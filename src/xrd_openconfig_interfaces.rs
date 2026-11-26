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
//! The pinned interpretation model for this implementation is the exact
//! revision set advertised by the captured XRd 24.4.2 image:
//! `openconfig-interfaces` 3.0.0 (2022-10-25), `openconfig-if-ip` 3.5.0
//! (2023-08-14), `openconfig-if-ethernet` 2.12.1 (2021-07-20), plus the
//! Cisco XR deviation module revision 2024-02-21. gNMI origin, path target,
//! timestamp, delete, duplicate, atomicity and terminal sync semantics are
//! validated fail-closed before payload interpretation. JSON module-qualified
//! identity collisions are rejected rather than canonicalized first-wins.
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
    CaptureOutcome, CommandCapture, FieldCoverage, InterfaceDiagnostic, MAX_INPUT_BYTES,
    NormalizationOutput, NormalizationReport, NormalizerLimits, ObservationContext,
    SourceCaptureReport, addr_field, mac_of, parse_rfc3339, sanitize_bounded, sha256_hex,
    unavailable,
};
use crate::typed_facts::{
    Completeness, EvidenceRef, FactPayload, FactSource, FactSubject, Lifecycle, PayloadState,
    Quality, TrustClass, TypedFact,
};

pub const XRD_OC_PARSER_REVISION: &str = "xrd-oc-iface-2026.11-r2";
pub const XRD_OC_MAPPING_REVISION: &str = "bn025-iface-map-2";
pub const XRD_OC_REQUIREMENT_REVISION: &str = "bn025-iface-requirements-r2";
pub const XRD_OC_ACQUISITION_CONTRACT: &str = "xrd_openconfig_gnmi_subscribe_once_interfaces";

/// Implementation-wide subscribe-once ceilings. Profile `NormalizerLimits`
/// may tighten input/entity/fact budgets but must never relax these ceilings.
const MAX_RESPONSES_PER_OBSERVATION: usize = 1024;
const MAX_UPDATES_PER_OBSERVATION: usize = 8192;
const MAX_DECODED_JSONIETF_BYTES: usize = 4 * 1024 * 1024;
const MAX_JSON_NESTING_DEPTH: usize = 128;
const MAX_SUBINTERFACES_PER_INTERFACE: usize = 256;
const MAX_INTERFACES_PER_OBSERVATION: usize = 4096;
const MAX_OUTPUT_FACTS_PER_OBSERVATION: usize = 4096;
const MAX_ADDR_ENTRIES_PER_FAMILY: usize = 64;
const MAX_CONFLICT_SAMPLES_PER_ENTITY: usize = 128;
const MAX_CONFLICT_PATHS_PER_ENTITY: usize = 1024;
const MAX_SOURCE_CONFLICTS_EMITTED: usize = 8;
const EXPECTED_OC_ORIGIN: &str = "openconfig-interfaces";

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
    timestamp_ns: u64,
}

#[derive(Debug)]
struct DecodedCapture {
    updates: Vec<DecodedUpdate>,
    sync_complete: bool,
    response_count: usize,
    update_count: usize,
    decoded_json_bytes: usize,
    min_timestamp_ns: Option<u64>,
    max_timestamp_ns: Option<u64>,
}

#[derive(Debug)]
struct MergedInterface {
    value: Value,
    updates: usize,
    min_timestamp_ns: Option<u64>,
    max_timestamp_ns: Option<u64>,
}

impl Default for MergedInterface {
    fn default() -> Self {
        Self {
            value: Value::Object(serde_json::Map::new()),
            updates: 0,
            min_timestamp_ns: None,
            max_timestamp_ns: None,
        }
    }
}

#[derive(Clone, Debug)]
struct PathElemRep {
    name: String,
    keys: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default)]
struct PathRep {
    origin: Option<String>,
    target: Option<String>,
    elem: Vec<PathElemRep>,
}

fn split_qualified(key: &str) -> (Option<&str>, &str) {
    key.split_once(':')
        .map(|(module, local)| (Some(module), local))
        .unwrap_or((None, key))
}

fn local_name(key: &str) -> &str {
    split_qualified(key).1
}

/// Narrow, explicit JSON-IETF module mapping for this qualified fixture
/// format. Foreign modules remain qualified and therefore cannot overwrite
/// an OpenConfig leaf; the three expected augmentation/interface modules are
/// canonicalized so equivalent notifications merge and conflict honestly.
fn canonical_json_key<'a>(key: &'a str) -> &'a str {
    match split_qualified(key) {
        (Some("openconfig-interfaces"), local)
        | (Some("openconfig-if-ethernet"), local)
        | (Some("openconfig-if-ip"), local) => local,
        _ => key,
    }
}

fn valid_json_key(key: &str) -> bool {
    if key.is_empty() || key.chars().any(|c| c.is_control()) {
        return false;
    }
    key.matches(':').count() <= 1
}

fn validate_namespace_shapes(value: &Value, path: &str) -> Result<(), String> {
    match value {
        Value::Object(object) => {
            let mut local_seen: BTreeMap<&str, &str> = BTreeMap::new();
            for (key, child) in object {
                if !valid_json_key(key) {
                    return Err(format!(
                        "malformed or multiply-qualified JSON-IETF key at {path}"
                    ));
                }
                let local = local_name(key);
                if let Some(first) = local_seen.insert(local, key) {
                    if first != key {
                        return Err(format!(
                            "ambiguous module-qualified representations for '{local}' at {path}: '{first}' and '{key}'"
                        ));
                    }
                    return Err(format!("duplicate JSON-IETF key '{key}' at {path}"));
                }
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                validate_namespace_shapes(child, &child_path)?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                validate_namespace_shapes(child, &format!("{path}[{index}]"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn canonicalized_value(source: &Value) -> Value {
    match source {
        Value::Object(object) => {
            let mut out = serde_json::Map::new();
            for (key, value) in object {
                let canonical = canonical_json_key(key).to_string();
                // Shape validation runs before canonicalization and rejects
                // same-local-name collisions. Insert explicitly so a future
                // regression cannot silently become first/last-wins here.
                if out
                    .insert(canonical.clone(), canonicalized_value(value))
                    .is_some()
                {
                    debug_assert!(false, "namespace collision reached canonicalization");
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalized_value).collect()),
        _ => source.clone(),
    }
}

// Semantic conflict bits. Detailed conflict samples are bounded separately;
// these bits are set for every detected conflict regardless of sample budget,
// so a required field can never become first-value-wins merely because the
// diagnostic sample budget was exhausted.
const C_ADMIN: u32 = 1 << 0;
const C_OPER: u32 = 1 << 1;
const C_IFINDEX: u32 = 1 << 2;
const C_MTU: u32 = 1 << 3;
const C_TYPE: u32 = 1 << 4;
const C_LOGICAL: u32 = 1 << 5;
const C_SPEED: u32 = 1 << 6;
const C_AUTO_NEGOTIATE: u32 = 1 << 7;
const C_MAC: u32 = 1 << 8;
const C_DESCRIPTION: u32 = 1 << 9;
const C_SUBINTERFACE_INDEX: u32 = 1 << 10;
const C_IPV4_ADDRESSING: u32 = 1 << 11;
const C_IPV6_ADDRESSING: u32 = 1 << 12;
const C_INTERFACE_IDENTITY: u32 = 1 << 13;

fn semantic_conflict_bits(path: &str) -> u32 {
    let leaf = path.rsplit('.').next().unwrap_or(path);
    let in_ipv4 = path.contains(".ipv4.") || path.starts_with("ipv4.");
    let in_ipv6 = path.contains(".ipv6.") || path.starts_with("ipv6.");
    let mut bits = match leaf {
        "admin-status" => C_ADMIN,
        "oper-status" => C_OPER,
        "ifindex" => C_IFINDEX,
        "mtu" => C_MTU,
        "type" => C_TYPE,
        "logical" => C_LOGICAL,
        "port-speed" | "negotiated-port-speed" => C_SPEED,
        "auto-negotiate" => C_AUTO_NEGOTIATE,
        "mac-address" => C_MAC,
        "description" => C_DESCRIPTION,
        "index" => C_SUBINTERFACE_INDEX,
        "name" => C_INTERFACE_IDENTITY,
        "ip" | "prefix-length" => {
            if in_ipv4 {
                C_IPV4_ADDRESSING
            } else if in_ipv6 {
                C_IPV6_ADDRESSING
            } else {
                0
            }
        }
        _ => 0,
    };
    if in_ipv4 && matches!(leaf, "ip" | "prefix-length") {
        bits |= C_IPV4_ADDRESSING;
    }
    if in_ipv6 && matches!(leaf, "ip" | "prefix-length") {
        bits |= C_IPV6_ADDRESSING;
    }
    bits
}

#[derive(Clone, Default)]
struct ConflictTracker {
    samples: BTreeMap<String, (String, String)>,
    retained_paths: BTreeSet<String>,
    detected_count: usize,
    omitted_count: usize,
    accounting_incomplete: bool,
    semantic_bits: u32,
}

impl ConflictTracker {
    fn record(&mut self, path: &str, first: String, later: String) {
        let already_retained =
            self.retained_paths.contains(path) || self.samples.contains_key(path);
        if !already_retained {
            self.detected_count = self.detected_count.saturating_add(1);
            if self.retained_paths.len() < MAX_CONFLICT_PATHS_PER_ENTITY {
                self.retained_paths.insert(path.to_string());
            } else {
                self.accounting_incomplete = true;
                self.omitted_count = self.omitted_count.saturating_add(1);
            }
            if self.samples.len() < MAX_CONFLICT_SAMPLES_PER_ENTITY {
                self.samples.insert(path.to_string(), (first, later));
            } else {
                self.omitted_count = self.omitted_count.saturating_add(1);
            }
        }
        self.semantic_bits |= semantic_conflict_bits(path);
    }

    fn has(&self, bits: u32) -> bool {
        self.semantic_bits & bits != 0
    }
}

fn canonical_object_merge(
    target: &mut Value,
    source: &Value,
    path: &str,
    conflicts: &mut ConflictTracker,
) {
    match (target, source) {
        (Value::Object(dst), Value::Object(src)) => {
            for (key, src_value) in src {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match dst.entry(key.clone()) {
                    serde_json::map::Entry::Vacant(entry) => {
                        entry.insert(src_value.clone());
                    }
                    serde_json::map::Entry::Occupied(mut entry) => {
                        canonical_object_merge(entry.get_mut(), src_value, &child_path, conflicts);
                    }
                }
            }
        }
        (dst, src) => {
            if dst != src {
                conflicts.record(path, bounded_json(dst), bounded_json(src));
            }
        }
    }
}

fn bounded_json(v: &Value) -> String {
    sanitize_bounded(&v.to_string(), 200)
}

// ── Bounded duplicate-key scanner ─────────────────────────────────────────
// serde_json intentionally represents objects as maps and therefore cannot
// report duplicate keys. JSON duplicates are ambiguous evidence, especially
// for list identity keys (`name`, `index`, `ip`), so the bounded input is
// scanned structurally before map decoding. This is not a second YANG/JSON
// parser: it only tracks object-key uniqueness and nesting depth; serde_json
// still performs actual value validation.
struct DuplicateKeyScanner<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> DuplicateKeyScanner<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn skip_ws(&mut self) {
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|b| matches!(b, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.pos += 1;
        }
    }

    fn scan_string(&mut self) -> Result<String, String> {
        if self.bytes.get(self.pos) != Some(&b'"') {
            return Err("expected JSON string".into());
        }
        let start = self.pos;
        self.pos += 1;
        while let Some(&b) = self.bytes.get(self.pos) {
            match b {
                b'\\' => {
                    self.pos += 2;
                }
                b'"' => {
                    self.pos += 1;
                    let raw = &self.bytes[start..self.pos];
                    return serde_json::from_slice::<String>(raw)
                        .map_err(|e| format!("invalid JSON string escape: {e}"));
                }
                _ => self.pos += 1,
            }
        }
        Err("unterminated JSON string".into())
    }

    fn scan_atom(&mut self) -> Result<(), String> {
        let start = self.pos;
        while let Some(&b) = self.bytes.get(self.pos) {
            if matches!(b, b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t') {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            Err("expected JSON value".into())
        } else {
            Ok(())
        }
    }

    fn scan_value(&mut self, depth: usize) -> Result<(), String> {
        if depth > MAX_JSON_NESTING_DEPTH {
            return Err(format!("JSON nesting exceeds {MAX_JSON_NESTING_DEPTH}"));
        }
        self.skip_ws();
        match self.bytes.get(self.pos) {
            Some(b'{') => self.scan_object(depth + 1),
            Some(b'[') => self.scan_array(depth + 1),
            Some(b'"') => self.scan_string().map(|_| ()),
            Some(_) => self.scan_atom(),
            None => Err("unexpected end of JSON".into()),
        }
    }

    fn scan_array(&mut self, depth: usize) -> Result<(), String> {
        self.pos += 1;
        self.skip_ws();
        if self.bytes.get(self.pos) == Some(&b']') {
            self.pos += 1;
            return Ok(());
        }
        loop {
            self.scan_value(depth)?;
            self.skip_ws();
            match self.bytes.get(self.pos) {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(());
                }
                _ => return Err("malformed JSON array".into()),
            }
        }
    }

    fn scan_object(&mut self, depth: usize) -> Result<(), String> {
        self.pos += 1;
        self.skip_ws();
        if self.bytes.get(self.pos) == Some(&b'}') {
            self.pos += 1;
            return Ok(());
        }
        let mut keys = BTreeSet::new();
        loop {
            self.skip_ws();
            let key = self.scan_string()?;
            if !keys.insert(key.clone()) {
                return Err(format!("duplicate JSON object key '{key}'"));
            }
            self.skip_ws();
            if self.bytes.get(self.pos) != Some(&b':') {
                return Err("malformed JSON object: missing ':'".into());
            }
            self.pos += 1;
            self.scan_value(depth)?;
            self.skip_ws();
            match self.bytes.get(self.pos) {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(());
                }
                _ => return Err("malformed JSON object".into()),
            }
        }
    }
}

pub(crate) fn reject_duplicate_json_keys(bytes: &[u8]) -> Result<(), String> {
    let mut scanner = DuplicateKeyScanner::new(bytes);
    scanner.scan_value(0)
}

fn parse_timestamp_ns(value: Option<&Value>, context: &str) -> Result<u64, String> {
    let Some(value) = value else {
        return Err(format!("{context} lacks gNMI notification timestamp"));
    };
    let ns = match value {
        Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => s
            .parse::<u64>()
            .map_err(|_| format!("{context} timestamp is outside u64 nanoseconds range"))?,
        Value::Number(n) => n
            .as_u64()
            .ok_or_else(|| format!("{context} timestamp is not an unsigned nanoseconds integer"))?,
        _ => return Err(format!("{context} timestamp is malformed")),
    };
    // gNMI timestamp is int64 nanoseconds since epoch in the protobuf
    // contract. Reject values that cannot fit the signed protocol type.
    if ns > i64::MAX as u64 {
        return Err(format!("{context} timestamp exceeds gNMI int64 range"));
    }
    Ok(ns)
}

fn validate_optional_timestamp(value: Option<&Value>, context: &str) -> Result<(), String> {
    if value.is_some() {
        parse_timestamp_ns(value, context)?;
    }
    Ok(())
}

fn read_path_rep(path: &Value, context: &str) -> Result<PathRep, String> {
    let object = path
        .as_object()
        .ok_or_else(|| format!("{context} gNMI path is not an object"))?;
    if object
        .get("element")
        .and_then(Value::as_array)
        .is_some_and(|v| !v.is_empty())
    {
        return Err(format!(
            "{context} uses deprecated gNMI path element encoding; elem is required"
        ));
    }
    let mut rep = PathRep::default();
    if let Some(origin) = object.get("origin") {
        let origin = origin
            .as_str()
            .ok_or_else(|| format!("{context} path origin is not a string"))?;
        if origin.is_empty() || !crate::gnmi_path::valid_origin(origin) {
            return Err(format!("{context} path origin '{origin}' is malformed"));
        }
        rep.origin = Some(origin.to_string());
    }
    if let Some(target) = object.get("target") {
        let target = target
            .as_str()
            .ok_or_else(|| format!("{context} path target is not a string"))?;
        if !target.is_empty() {
            rep.target = Some(target.to_string());
        }
    }
    let elems: &[Value] = match object.get("elem") {
        None => &[],
        Some(v) => v
            .as_array()
            .map(Vec::as_slice)
            .ok_or_else(|| format!("{context} path elem is not an array"))?,
    };
    if elems.len() > 16 {
        return Err(format!("{context} path exceeds bounded elem depth"));
    }
    for (index, elem) in elems.iter().enumerate() {
        let name = elem
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{context} path elem {index} lacks string name"))?;
        if name.is_empty() || name.chars().any(|c| c.is_control()) {
            return Err(format!("{context} path elem {index} has invalid name"));
        }
        let mut keys = BTreeMap::new();
        if let Some(key_object) = elem.get("key") {
            let key_object = key_object
                .as_object()
                .ok_or_else(|| format!("{context} path elem {index} key is not an object"))?;
            for (k, v) in key_object {
                let v = v.as_str().ok_or_else(|| {
                    format!("{context} path elem {index} key '{k}' is not a string")
                })?;
                if k.is_empty() || keys.insert(k.clone(), v.to_string()).is_some() {
                    return Err(format!(
                        "{context} path elem {index} has invalid/duplicate key"
                    ));
                }
            }
        }
        rep.elem.push(PathElemRep {
            name: name.to_string(),
            keys,
        });
    }
    Ok(rep)
}

fn resolve_interface_path(
    prefix: Option<&Value>,
    update: &Value,
    context: &str,
) -> Result<String, String> {
    let prefix_rep = match prefix {
        Some(prefix) => read_path_rep(prefix, context)?,
        None => PathRep::default(),
    };
    let update_path = update
        .get("path")
        .ok_or_else(|| format!("{context} update lacks path"))?;
    let update_rep = read_path_rep(update_path, context)?;

    for target in [prefix_rep.target.as_deref(), update_rep.target.as_deref()]
        .into_iter()
        .flatten()
    {
        return Err(format!(
            "{context} carries gNMI path target '{target}', but this offline observation contract has no authenticated target-binding axis"
        ));
    }
    let effective_origin = match (prefix_rep.origin.as_deref(), update_rep.origin.as_deref()) {
        (Some(a), Some(b)) if a != b => {
            return Err(format!(
                "{context} has conflicting prefix origin '{a}' and update origin '{b}'"
            ));
        }
        (Some(a), _) => a,
        (_, Some(b)) => b,
        (None, None) => {
            return Err(format!(
                "{context} lacks gNMI model origin; cannot establish OpenConfig semantics"
            ));
        }
    };
    if effective_origin != EXPECTED_OC_ORIGIN {
        return Err(format!(
            "{context} uses origin '{effective_origin}', outside qualified '{EXPECTED_OC_ORIGIN}' subscription scope"
        ));
    }

    let mut combined = prefix_rep.elem.clone();
    combined.extend(update_rep.elem);
    if combined.len() != 2
        || combined[0].name != "interfaces"
        || combined[1].name != "interface"
        || !combined[0].keys.is_empty()
    {
        return Err(format!(
            "{context} is outside the qualified interfaces/interface[name=*] root subtree or uses unsupported leaf-level paths"
        ));
    }
    if combined[1].keys.len() != 1 || !combined[1].keys.contains_key("name") {
        return Err(format!(
            "{context} interface list path does not carry exactly the OpenConfig 'name' key"
        ));
    }
    let name = combined[1]
        .keys
        .get("name")
        .expect("checked contains name")
        .clone();
    if name.is_empty() {
        return Err(format!("{context} interface name key is empty"));
    }
    Ok(name)
}

fn decode_capture(raw: &str) -> Result<DecodedCapture, String> {
    reject_duplicate_json_keys(raw.as_bytes())
        .map_err(|e| format!("capture envelope has ambiguous JSON identity: {e}"))?;
    let top: Value = serde_json::from_str(raw).map_err(|e| format!("invalid capture JSON: {e}"))?;
    if top.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(
            "capture envelope does not report a successful subscribe-once operation".into(),
        );
    }
    if top.get("error").is_some_and(|v| !v.is_null()) {
        return Err("capture envelope reports ok=true with non-null error".into());
    }
    let responses = top
        .get("responses")
        .and_then(Value::as_array)
        .ok_or_else(|| "capture envelope missing responses array".to_string())?;
    if let Some(count) = top.get("count") {
        let count = count
            .as_u64()
            .ok_or_else(|| "capture envelope count is not an unsigned integer".to_string())?;
        if count as usize != responses.len() {
            return Err(format!(
                "capture envelope count {count} disagrees with {} response entries",
                responses.len()
            ));
        }
    }
    if responses.len() > MAX_RESPONSES_PER_OBSERVATION {
        return Err(format!(
            "capture contains {} responses, exceeding bounded response ceiling {MAX_RESPONSES_PER_OBSERVATION}",
            responses.len()
        ));
    }

    let mut updates = Vec::new();
    let mut sync_complete = false;
    let mut sync_seen = false;
    let mut update_count = 0usize;
    let mut decoded_json_bytes = 0usize;
    let mut min_timestamp_ns = None::<u64>;
    let mut max_timestamp_ns = None::<u64>;

    for (response_index, response) in responses.iter().enumerate() {
        let context = format!("response {response_index}");
        if sync_seen {
            return Err(format!(
                "{context} appears after terminal gNMI sync_response"
            ));
        }
        if response.get("sync_response").is_some() {
            if response.get("sync_response").and_then(Value::as_bool) != Some(true) {
                return Err(format!("{context} has malformed sync_response value"));
            }
            validate_optional_timestamp(response.get("timestamp"), &context)?;
            sync_seen = true;
            sync_complete = true;
            continue;
        }
        if response.get("atomic").and_then(Value::as_bool) == Some(true) {
            return Err(format!(
                "{context} uses atomic notification semantics not supported by this subscribe-once normalizer"
            ));
        }
        if let Some(delete) = response.get("delete") {
            let delete_count = delete
                .as_array()
                .ok_or_else(|| format!("{context} delete field is not an array"))?
                .len();
            if delete_count > 0 {
                return Err(format!(
                    "{context} carries {delete_count} gNMI delete paths; delete lifecycle is explicitly unsupported in this subscribe-once slice"
                ));
            }
        }
        let timestamp_ns = parse_timestamp_ns(response.get("timestamp"), &context)?;
        min_timestamp_ns = Some(min_timestamp_ns.map_or(timestamp_ns, |v| v.min(timestamp_ns)));
        max_timestamp_ns = Some(max_timestamp_ns.map_or(timestamp_ns, |v| v.max(timestamp_ns)));
        let Some(update_list) = response.get("update").and_then(Value::as_array) else {
            return Err(format!(
                "{context} is neither a valid notification update nor sync_response"
            ));
        };
        update_count = update_count.saturating_add(update_list.len());
        if update_count > MAX_UPDATES_PER_OBSERVATION {
            return Err(format!(
                "capture contains {update_count} gNMI updates, exceeding bounded update ceiling {MAX_UPDATES_PER_OBSERVATION}"
            ));
        }
        for (update_index, update) in update_list.iter().enumerate() {
            let ucontext = format!("response {response_index} update {update_index}");
            if update
                .get("duplicates")
                .and_then(Value::as_u64)
                .is_some_and(|v| v != 0)
            {
                return Err(format!(
                    "{ucontext} uses coalesced duplicates; lifecycle interpretation unsupported"
                ));
            }
            let interface_name = resolve_interface_path(response.get("prefix"), update, &ucontext)?;
            let val = update
                .get("val")
                .and_then(Value::as_object)
                .ok_or_else(|| format!("{ucontext} lacks typed val object"))?;
            if val.len() != 1 || !val.contains_key("json_ietf_val") {
                return Err(format!(
                    "{ucontext} typed value is not exactly one json_ietf_val"
                ));
            }
            let encoded = val["json_ietf_val"]
                .as_str()
                .ok_or_else(|| format!("{ucontext} json_ietf_val is not base64 text"))?;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|e| format!("{ucontext} has invalid base64 JSON-IETF value: {e}"))?;
            decoded_json_bytes = decoded_json_bytes.saturating_add(decoded.len());
            if decoded_json_bytes > MAX_DECODED_JSONIETF_BYTES {
                return Err(format!(
                    "decoded JSON-IETF payload bytes exceed bounded ceiling {MAX_DECODED_JSONIETF_BYTES}"
                ));
            }
            reject_duplicate_json_keys(&decoded).map_err(|e| {
                format!("{ucontext} JSON-IETF payload has ambiguous object identity: {e}")
            })?;
            let value: Value = serde_json::from_slice(&decoded)
                .map_err(|e| format!("{ucontext} has invalid JSON-IETF payload: {e}"))?;
            if !value.is_object() {
                return Err(format!("{ucontext} JSON-IETF payload is not an object"));
            }
            validate_namespace_shapes(&value, "")?;
            updates.push(DecodedUpdate {
                interface_name,
                value: canonicalized_value(&value),
                timestamp_ns,
            });
        }
    }
    Ok(DecodedCapture {
        updates,
        sync_complete,
        response_count: responses.len(),
        update_count,
        decoded_json_bytes,
        min_timestamp_ns,
        max_timestamp_ns,
    })
}

fn rfc3339_from_unix_ns(ns: u64) -> Option<String> {
    let secs = (ns / 1_000_000_000) as i64;
    let nanos = (ns % 1_000_000_000) as u32;
    chrono::DateTime::from_timestamp(secs, nanos)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
}

fn interval_strings(
    min_ns: Option<u64>,
    max_ns: Option<u64>,
) -> (Option<String>, Option<String>, Option<String>) {
    let min = min_ns.and_then(rfc3339_from_unix_ns);
    let max = max_ns.and_then(rfc3339_from_unix_ns);
    (min.clone(), max.clone(), max.or(min))
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

fn strict_string_at<'a>(object: &'a Value, path: &[&str]) -> Result<Option<&'a str>, ()> {
    match object_at(object, path) {
        None => Ok(None),
        Some(v) => v.as_str().map(Some).ok_or(()),
    }
}

fn strict_bool_at(object: &Value, path: &[&str]) -> Result<Option<bool>, ()> {
    match object_at(object, path) {
        None => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or(()),
    }
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

fn admin_state(state: Option<&Value>, conflicts: &ConflictTracker) -> ObservedField<AdminState> {
    if conflicts.has(C_ADMIN) {
        return unavailable(
            FieldAvailability::Invalid,
            Some("conflicting gNMI updates for admin-status"),
        );
    }
    match strict_string_at(state.unwrap_or(&Value::Null), &["admin-status"]) {
        Err(()) => unavailable(
            FieldAvailability::Invalid,
            Some("admin-status had non-string type"),
        ),
        Ok(Some("UP")) => ObservedField::known(AdminState::Up),
        Ok(Some("DOWN")) => ObservedField::known(AdminState::Down),
        Ok(Some("TESTING")) => unavailable(
            FieldAvailability::Unknown,
            Some("schema-valid OpenConfig TESTING admin-status has no common contract value"),
        ),
        Ok(Some(_)) => unavailable(
            FieldAvailability::Invalid,
            Some("unrecognized OpenConfig admin-status token"),
        ),
        Ok(None) => unavailable(
            FieldAvailability::Unknown,
            Some("admin-status not observed"),
        ),
    }
}

fn oper_state(state: Option<&Value>, conflicts: &ConflictTracker) -> ObservedField<OperState> {
    if conflicts.has(C_OPER) {
        return unavailable(
            FieldAvailability::Invalid,
            Some("conflicting gNMI updates for oper-status"),
        );
    }
    let raw = strict_string_at(state.unwrap_or(&Value::Null), &["oper-status"]);
    match raw {
        Err(()) => unavailable(
            FieldAvailability::Invalid,
            Some("oper-status had non-string type"),
        ),
        Ok(Some("UP")) => ObservedField::known(OperState::Up),
        Ok(Some("DOWN")) => ObservedField::known(OperState::Down),
        Ok(Some("LOWER_LAYER_DOWN")) => ObservedField::known(OperState::LowerLayerDown),
        Ok(Some("DORMANT")) => ObservedField::known(OperState::Dormant),
        Ok(Some("TESTING")) => ObservedField::known(OperState::Testing),
        Ok(Some("UNKNOWN")) => ObservedField::known(OperState::Unknown),
        Ok(Some("NOT_PRESENT")) => unavailable(
            FieldAvailability::Unknown,
            Some("schema-valid OpenConfig NOT_PRESENT oper-status has no common contract value"),
        ),
        Ok(Some(_)) => unavailable(
            FieldAvailability::Invalid,
            Some("unrecognized OpenConfig oper-status token"),
        ),
        Ok(None) => unavailable(FieldAvailability::Unknown, Some("oper-status not observed")),
    }
}

fn ifindex(state: Option<&Value>, conflicts: &ConflictTracker) -> ObservedField<u64> {
    if conflicts.has(C_IFINDEX) {
        return unavailable(
            FieldAvailability::Invalid,
            Some("conflicting gNMI updates for ifindex"),
        );
    }
    match u64_at(state.unwrap_or(&Value::Null), &["ifindex"]) {
        Ok(Some(v)) if v > 0 && v <= u32::MAX as u64 => ObservedField::known(v),
        Ok(Some(_)) => unavailable(
            FieldAvailability::Invalid,
            Some("ifindex outside uint32 nonzero range"),
        ),
        Ok(None) => unavailable(FieldAvailability::Unknown, Some("ifindex not observed")),
        Err(_) => unavailable(
            FieldAvailability::Invalid,
            Some("ifindex had non-numeric type"),
        ),
    }
}

fn mtu(state: Option<&Value>, conflicts: &ConflictTracker) -> ObservedField<u32> {
    if conflicts.has(C_MTU) {
        return unavailable(
            FieldAvailability::Invalid,
            Some("conflicting gNMI updates for interface mtu"),
        );
    }
    match u64_at(state.unwrap_or(&Value::Null), &["mtu"]) {
        Ok(Some(v)) if v > 0 && v <= u16::MAX as u64 => ObservedField::known(v as u32),
        Ok(Some(_)) => unavailable(
            FieldAvailability::Invalid,
            Some("interface mtu outside uint16 nonzero range"),
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

fn identityref_module(token: &str) -> (Option<&str>, &str) {
    split_qualified(token)
}

fn interface_type(
    state: Option<&Value>,
    ifname: &str,
    conflicts: &ConflictTracker,
) -> (ObservedField<InterfaceType>, Option<bool>) {
    if conflicts.has(C_TYPE | C_LOGICAL) {
        return (
            unavailable(
                FieldAvailability::Invalid,
                Some("conflicting gNMI updates for interface type or its logical prerequisite"),
            ),
            None,
        );
    }
    let raw = match strict_string_at(state.unwrap_or(&Value::Null), &["type"]) {
        Ok(v) => v,
        Err(()) => {
            return (
                unavailable(
                    FieldAvailability::Invalid,
                    Some("interface type had non-string type"),
                ),
                None,
            );
        }
    };
    let logical = match state.map_or(Ok(None), |s| strict_bool_at(s, &["logical"])) {
        Ok(v) => v,
        Err(()) => {
            return (
                unavailable(
                    FieldAvailability::Invalid,
                    Some("logical prerequisite had non-boolean type"),
                ),
                None,
            );
        }
    };
    let Some(raw) = raw else {
        return (
            unavailable(
                FieldAvailability::Unknown,
                Some("interface type not observed"),
            ),
            None,
        );
    };
    let (module, local) = identityref_module(raw);
    let ty = local;
    let is_ethernet = match ty {
        "ethernetCsmacd" => Some(true),
        "softwareLoopback" | "other" => Some(false),
        _ => None,
    };
    let module_valid = module == Some("iana-if-type");
    if !module_valid {
        return (
            unavailable(
                FieldAvailability::Invalid,
                Some(format!(
                    "interface type identityref '{raw}' uses foreign module qualification for '{ifname}'"
                )),
            ),
            None,
        );
    }
    if ty == "ethernetCsmacd" && logical.is_none() {
        return (
            unavailable(
                FieldAvailability::Unknown,
                Some("ethernet interface type observed but physical/logical state not observed"),
            ),
            is_ethernet,
        );
    }
    let mapped = match (ty, logical) {
        ("ethernetCsmacd", Some(false)) => Some(InterfaceType::Physical),
        ("ethernetCsmacd", Some(true)) => Some(InterfaceType::Virtual),
        ("softwareLoopback", _) => Some(InterfaceType::Loopback),
        ("other", Some(true)) => Some(InterfaceType::Virtual),
        ("other", _) => Some(InterfaceType::Unknown),
        _ => None,
    };
    let field = match mapped {
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
    };
    (field, is_ethernet)
}

fn speed_from_token(token: &str) -> Result<Option<u64>, ()> {
    let (module, local) = split_qualified(token);
    if !module.is_none_or(|m| m == "openconfig-if-ethernet") {
        return Err(());
    }
    Ok(match local {
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
    })
}

fn speed_token_is_unknown(token: &str) -> bool {
    split_qualified(token).1 == "SPEED_UNKNOWN"
}

fn speed(
    ethernet_state: Option<&Value>,
    conflicts: &ConflictTracker,
    is_ethernet_type: Option<bool>,
) -> ObservedField<SpeedObservation> {
    if conflicts.has(C_SPEED | C_AUTO_NEGOTIATE) {
        return unavailable(
            FieldAvailability::Invalid,
            Some("conflicting gNMI updates for ethernet speed or auto-negotiation prerequisite"),
        );
    }
    if is_ethernet_type == Some(false) {
        return unavailable(
            FieldAvailability::Unsupported,
            Some("ethernet speed is inapplicable to established non-Ethernet interface type"),
        );
    }
    let Some(state) = ethernet_state else {
        return unavailable(
            FieldAvailability::Unknown,
            Some("ethernet state subtree not observed"),
        );
    };
    let negotiated = string_at(state, &["negotiated-port-speed"]);
    if let Some(token) = negotiated {
        if speed_from_token(token).is_err() {
            return unavailable(
                FieldAvailability::Invalid,
                Some(format!(
                    "foreign-module negotiated-port-speed identityref '{token}'"
                )),
            );
        }
        if !speed_token_is_unknown(token) {
            return match speed_from_token(token) {
                Ok(Some(v)) => ObservedField::known(SpeedObservation {
                    value: v,
                    unit: SpeedUnit::MegabitsPerSecond,
                }),
                Ok(None) => unavailable(
                    FieldAvailability::Unknown,
                    Some(format!("unmapped negotiated-port-speed identity '{token}'")),
                ),
                Err(()) => unreachable!("foreign module checked above"),
            };
        }
        // SPEED_UNKNOWN falls through to forced port-speed only when negotiation is off.
    }
    let forced = string_at(state, &["port-speed"]);
    let auto_negotiate = match strict_bool_at(state, &["auto-negotiate"]) {
        Ok(v) => v,
        Err(()) => {
            return unavailable(
                FieldAvailability::Invalid,
                Some("auto-negotiate prerequisite had non-boolean type"),
            );
        }
    };
    match (forced, auto_negotiate) {
        (Some(token), Some(false)) => match speed_from_token(token) {
            Ok(Some(v)) => ObservedField::known(SpeedObservation {
                value: v,
                unit: SpeedUnit::MegabitsPerSecond,
            }),
            Ok(None) if speed_token_is_unknown(token) => unavailable(
                FieldAvailability::Unknown,
                Some("device reported SPEED_UNKNOWN"),
            ),
            Ok(None) => unavailable(
                FieldAvailability::Unknown,
                Some(format!("unmapped forced port-speed identity '{token}'")),
            ),
            Err(()) => unavailable(
                FieldAvailability::Invalid,
                Some(format!(
                    "foreign-module forced port-speed identityref '{token}'"
                )),
            ),
        },
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

#[derive(Default)]
struct AddressExtraction {
    v4: Vec<String>,
    v6: Vec<String>,
    unsupported_subinterfaces: usize,
    malformed_entries: usize,
    omitted_entries: usize,
    malformed_v4: usize,
    malformed_v6: usize,
    omitted_v4: usize,
    omitted_v6: usize,
}

fn extract_addresses(interface: &Value) -> AddressExtraction {
    let mut out = AddressExtraction::default();
    let subinterfaces = object_at(interface, &["subinterfaces"])
        .map(|v| singleton_or_list_entries(get(v, "subinterface"), "index"))
        .unwrap_or_default();
    let mut seen_subindices = BTreeSet::new();
    if subinterfaces.len() > MAX_SUBINTERFACES_PER_INTERFACE {
        out.omitted_entries = out
            .omitted_entries
            .saturating_add(subinterfaces.len() - MAX_SUBINTERFACES_PER_INTERFACE);
    }
    for subinterface in subinterfaces
        .iter()
        .copied()
        .take(MAX_SUBINTERFACES_PER_INTERFACE)
    {
        let index = match u64_at(subinterface, &["index"])
            .or_else(|_| u64_at(subinterface, &["state", "index"]))
        {
            Ok(Some(v)) => v,
            _ => {
                out.malformed_entries = out.malformed_entries.saturating_add(1);
                continue;
            }
        };
        if index > u32::MAX as u64 {
            out.malformed_entries = out.malformed_entries.saturating_add(1);
            continue;
        }
        if index != 0 {
            out.unsupported_subinterfaces = out.unsupported_subinterfaces.saturating_add(1);
            continue;
        }
        if !seen_subindices.insert(index) {
            out.malformed_entries = out.malformed_entries.saturating_add(1);
            continue;
        }
        for family in ["ipv4", "ipv6"] {
            let Some(ip_container) = object_at(subinterface, &[family]) else {
                continue;
            };
            let entries = object_at(ip_container, &["addresses"])
                .map(|v| singleton_or_list_entries(get(v, "address"), "ip"))
                .unwrap_or_default();
            let mut family_malformed = 0usize;
            let mut seen_ips = BTreeSet::new();
            let mut accepted = 0usize;
            for entry in entries.iter().copied().take(MAX_ADDR_ENTRIES_PER_FAMILY) {
                let key_ip = string_at(entry, &["ip"]);
                let ip = string_at(entry, &["state", "ip"]).or(key_ip);
                if key_ip.is_some() && ip.is_some() && key_ip != ip {
                    family_malformed = family_malformed.saturating_add(1);
                    continue;
                }
                if ip.is_some_and(|ip| !seen_ips.insert(ip.to_string())) {
                    family_malformed = family_malformed.saturating_add(1);
                    continue;
                }
                let prefix = match u64_at(entry, &["state", "prefix-length"])
                    .or_else(|_| u64_at(entry, &["prefix-length"]))
                {
                    Ok(v) => v,
                    Err(_) => {
                        family_malformed = family_malformed.saturating_add(1);
                        continue;
                    }
                };
                let max_prefix = if family == "ipv4" { 32 } else { 128 };
                match (ip, prefix) {
                    (Some(ip), Some(prefix))
                        if prefix <= max_prefix
                            && crate::interface_facts::valid_addr_entry(
                                &format!("{ip}/{prefix}"),
                                family == "ipv6",
                            ) =>
                    {
                        let rendered = format!("{ip}/{prefix}");
                        if family == "ipv4" {
                            out.v4.push(rendered);
                        } else {
                            out.v6.push(rendered);
                        }
                        accepted = accepted.saturating_add(1);
                    }
                    _ => family_malformed = family_malformed.saturating_add(1),
                }
            }
            let family_omitted = entries.len().saturating_sub(accepted);
            if family == "ipv4" {
                out.malformed_v4 = out.malformed_v4.saturating_add(family_malformed);
                out.omitted_v4 = out.omitted_v4.saturating_add(family_omitted);
            } else {
                out.malformed_v6 = out.malformed_v6.saturating_add(family_malformed);
                out.omitted_v6 = out.omitted_v6.saturating_add(family_omitted);
            }
            out.malformed_entries = out.malformed_entries.saturating_add(family_malformed);
            out.omitted_entries = out.omitted_entries.saturating_add(family_omitted);
        }
    }
    out
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
            interface_identity: None,
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

fn payload_identity_errors(value: &Value, expected_name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    // OpenConfig list-key identity can appear at the entry root (`name`) as
    // well as in the state/config instantiations. A malformed identity leaf
    // is invalid evidence, not absent evidence; a mismatch must never make
    // the update unambiguous evidence for the path key.
    for path in [
        &["name"][..],
        &["state", "name"][..],
        &["config", "name"][..],
    ] {
        if let Some(node) = object_at(value, path) {
            match node.as_str() {
                Some(observed) if observed != expected_name => {
                    errors.push(format!(
                        "payload identity '{}' at {} disagrees with path key '{}'",
                        observed,
                        path.join("."),
                        expected_name
                    ));
                }
                Some(_) => {}
                None => errors.push(format!(
                    "payload identity at {} has non-string type",
                    path.join(".")
                )),
            }
        }
    }
    let subinterfaces = object_at(value, &["subinterfaces"])
        .map(|v| singleton_or_list_entries(get(v, "subinterface"), "index"))
        .unwrap_or_default();
    let mut seen_indices = BTreeSet::new();
    for subinterface in subinterfaces
        .iter()
        .copied()
        .take(MAX_SUBINTERFACES_PER_INTERFACE)
    {
        let key_index = u64_at(subinterface, &["index"]).ok().flatten();
        let state_index = u64_at(subinterface, &["state", "index"]).ok().flatten();
        if let (Some(key), Some(state)) = (key_index, state_index) {
            if key != state {
                errors.push(format!(
                    "subinterface key index {key} disagrees with state index {state}"
                ));
            }
        }
        if let Some(index) = key_index.or(state_index) {
            if index > u32::MAX as u64 {
                errors.push(format!("subinterface index {index} outside uint32 range"));
            }
            if !seen_indices.insert(index) {
                errors.push(format!("duplicate subinterface index {index}"));
            }
        }
        for family in ["ipv4", "ipv6"] {
            let entries = object_at(subinterface, &[family, "addresses"])
                .map(|v| singleton_or_list_entries(get(v, "address"), "ip"))
                .unwrap_or_default();
            let mut seen_ips = BTreeSet::new();
            for entry in entries.into_iter().take(MAX_ADDR_ENTRIES_PER_FAMILY) {
                let key_ip = string_at(entry, &["ip"]);
                let state_ip = string_at(entry, &["state", "ip"]);
                if let (Some(key), Some(state)) = (key_ip, state_ip) {
                    if key != state {
                        errors.push(format!(
                            "{family} address key '{key}' disagrees with state ip '{state}'"
                        ));
                    }
                }
                if let Some(ip) = key_ip.or(state_ip) {
                    if !seen_ips.insert(ip.to_string()) {
                        errors.push(format!("duplicate {family} address identity '{ip}'"));
                    }
                }
            }
        }
    }
    errors.truncate(16);
    errors
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
    let max_input = limits.max_input_bytes.min(MAX_INPUT_BYTES);
    if bytes == 0 || bytes > max_input {
        return invalid_output(
            obs,
            "observation empty or exceeded implementation input byte bound; refused",
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

    let decoded = match decode_capture(obs.capture.output) {
        Ok(v) => v,
        Err(e) => {
            #[cfg(test)]
            eprintln!("xrd capture decode rejected: {e}");
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
    let mut conflict_maps: BTreeMap<String, ConflictTracker> = BTreeMap::new();
    let mut invalid_identity_names = BTreeSet::new();
    for update in decoded.updates {
        if !valid_name(&update.interface_name) {
            invalid_identity_names.insert(update.interface_name.clone());
            continue;
        }
        let entry = merged.entry(update.interface_name.clone()).or_default();
        entry.updates = entry.updates.saturating_add(1);
        entry.min_timestamp_ns = Some(
            entry
                .min_timestamp_ns
                .map_or(update.timestamp_ns, |v| v.min(update.timestamp_ns)),
        );
        entry.max_timestamp_ns = Some(
            entry
                .max_timestamp_ns
                .map_or(update.timestamp_ns, |v| v.max(update.timestamp_ns)),
        );
        let conflicts = conflict_maps
            .entry(update.interface_name.clone())
            .or_default();
        canonical_object_merge(&mut entry.value, &update.value, "", conflicts);
    }

    let records_decoded = merged.len();
    let max_entities = limits.max_entities.min(MAX_INTERFACES_PER_OBSERVATION);
    let max_output_facts = limits
        .max_output_facts
        .min(MAX_OUTPUT_FACTS_PER_OBSERVATION);
    let mut facts = Vec::new();
    let mut diagnostics = Vec::new();
    let mut coverage: BTreeMap<String, FieldCoverage> = BTreeMap::new();
    let mut validated_coverage: BTreeMap<String, FieldCoverage> = BTreeMap::new();
    let mut entities_omitted = 0usize;
    let mut invalid_entities = invalid_identity_names.len();
    let mut output_facts_omitted = 0usize;
    let mut facts_rejected = 0usize;
    let mut rejected_samples = Vec::new();
    let mut entities_accepted = 0usize;
    let mut unsupported_subinterfaces_total = 0usize;
    let mut malformed_address_entries_total = 0usize;
    let mut address_entries_omitted_total = 0usize;

    for (entity_index, (name, record)) in merged.iter().enumerate() {
        if entity_index >= max_entities {
            entities_omitted = entities_omitted.saturating_add(1);
            continue;
        }
        if facts.len() >= max_output_facts {
            output_facts_omitted = output_facts_omitted.saturating_add(1);
            continue;
        }
        let conflicts = conflict_maps.get(name).cloned().unwrap_or_default();
        let mut identity_errors = payload_identity_errors(&record.value, name);
        if conflicts.has(C_INTERFACE_IDENTITY) {
            identity_errors.push("conflicting gNMI updates for interface identity leaves".into());
        }
        if !identity_errors.is_empty() {
            invalid_entities = invalid_entities.saturating_add(1);
            if rejected_samples.len() < 8 {
                rejected_samples.push(format!(
                    "{name}: {}",
                    sanitize_bounded(&identity_errors.join("; "), 220)
                ));
            }
            continue;
        }

        let state = object_at(&record.value, &["state"]);
        let ethernet_state = object_at(&record.value, &["ethernet", "state"]);
        let mut source_conflicts = Vec::new();
        for (path, (first, later)) in conflicts.samples.iter().take(MAX_SOURCE_CONFLICTS_EMITTED) {
            source_conflicts.push(SourceFieldConflict {
                field: path.clone(),
                effective_source: "gnmi_update_first".into(),
                effective_value: first.clone(),
                alternate_source: "gnmi_update_later".into(),
                alternate_value: later.clone(),
                resolution: ConflictResolution::Unresolved,
            });
        }

        let admin = admin_state(state, &conflicts);
        let oper = oper_state(state, &conflicts);
        let ifindex = ifindex(state, &conflicts);
        let (itype, is_ethernet_type) = interface_type(state, name, &conflicts);
        let mtu = mtu(state, &conflicts);
        let speed = speed(ethernet_state, &conflicts, is_ethernet_type);
        let mac_raw = ethernet_state.and_then(|s| string_at(s, &["mac-address"]));
        let mac = if conflicts.has(C_MAC) {
            unavailable(
                FieldAvailability::Invalid,
                Some("conflicting gNMI updates for ethernet mac-address"),
            )
        } else {
            mac_of(mac_raw)
        };
        let description = if conflicts.has(C_DESCRIPTION) {
            None
        } else {
            state
                .and_then(|s| string_at(s, &["description"]))
                .map(|d| sanitize_bounded(d, 240))
        };
        let extracted = extract_addresses(&record.value);
        unsupported_subinterfaces_total =
            unsupported_subinterfaces_total.saturating_add(extracted.unsupported_subinterfaces);
        malformed_address_entries_total =
            malformed_address_entries_total.saturating_add(extracted.malformed_entries);
        address_entries_omitted_total =
            address_entries_omitted_total.saturating_add(extracted.omitted_entries);
        let ip4 = if conflicts.has(C_IPV4_ADDRESSING) {
            unavailable(
                FieldAvailability::Invalid,
                Some("conflicting gNMI updates for ipv4 addressing"),
            )
        } else if extracted.malformed_v4 > 0 || extracted.omitted_v4 > 0 {
            unavailable(
                FieldAvailability::Unknown,
                Some("ipv4 address population incomplete; entries omitted or malformed"),
            )
        } else {
            addr_field(
                &extracted.v4,
                false,
                0,
                "no ipv4 address subtree observed on base interface",
            )
        };
        let ip6 = if conflicts.has(C_IPV6_ADDRESSING) {
            unavailable(
                FieldAvailability::Invalid,
                Some("conflicting gNMI updates for ipv6 addressing"),
            )
        } else if extracted.malformed_v6 > 0 || extracted.omitted_v6 > 0 {
            unavailable(
                FieldAvailability::Unknown,
                Some("ipv6 address population incomplete; entries omitted or malformed"),
            )
        } else {
            addr_field(
                &extracted.v6,
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
            lag_members: None,
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
        if extracted.unsupported_subinterfaces > 0 {
            qnotes.push(format!(
                "{} non-zero OpenConfig subinterfaces not emitted in this slice; structured subinterface identity is not yet part of the common contract",
                extracted.unsupported_subinterfaces
            ));
        }
        if !decoded.sync_complete {
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
        if conflicts.detected_count > 0 {
            qnotes.push(format!(
                "{} conflicting gNMI leaf updates preserved as source conflicts",
                conflicts.detected_count
            ));
        }
        if conflicts.has(C_DESCRIPTION) {
            qnotes.push("conflicting description updates; description omitted".into());
        }
        if extracted.omitted_entries > 0 {
            qnotes.push(format!(
                "{} address entries omitted or malformed under bounded extraction",
                extracted.omitted_entries
            ));
        }

        let semantic_gap = !missing_fields.is_empty()
            || extracted.unsupported_subinterfaces > 0
            || extracted.malformed_entries > 0
            || extracted.omitted_entries > 0
            || conflicts.detected_count > 0;
        let completeness =
            if obs.capture.outcome == CaptureOutcome::Unknown || !decoded.sync_complete {
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
                facts_rejected = facts_rejected.saturating_add(1);
                if rejected_samples.len() < 8 {
                    rejected_samples.push(format!("{name}: payload serialization failed: {e}"));
                }
                continue;
            }
        };
        let (valid_from, valid_until, observed_interval_end) =
            interval_strings(record.min_timestamp_ns, record.max_timestamp_ns);
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
                interface_identity: None,
            },
            source: FactSource {
                collector_id: obs.context.collector_id.into(),
                source_kind: "gnmi_openconfig".into(),
                capture_id: obs.context.capture_id.into(),
                session_epoch: obs.context.session_epoch,
                sequence: obs.context.sequence,
                exporter_id: None,
            },
            observed_at: observed_interval_end.unwrap_or_else(|| obs.context.observed_at.into()),
            ingested_at: None,
            valid_from,
            valid_until,
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
            ip_addresses: extracted.v4,
            ipv6_addresses: extracted.v6,
            addresses_omitted: extracted.omitted_entries,
            conflict_fields: conflicts
                .retained_paths
                .iter()
                .cloned()
                .take(MAX_SOURCE_CONFLICTS_EMITTED)
                .collect(),
        };
        diagnostics.push(diagnostic);
        if let Err(e) = fact.validate() {
            #[cfg(test)]
            eprintln!("xrd fact validation rejected {name}: {e:?}");
            facts_rejected = facts_rejected.saturating_add(1);
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
        entities_accepted = entities_accepted.saturating_add(1);
        facts.push(fact);
    }

    let mut status_facts_emitted = 0usize;
    if facts.is_empty() {
        let (state, note) = if merged.is_empty() && invalid_entities == 0 {
            if decoded.sync_complete {
                (
                    PayloadState::Empty,
                    "gNMI subscribe-once completed with no interface update records".to_string(),
                )
            } else {
                (
                    PayloadState::UnknownQuality,
                    "gNMI subscribe-once yielded no interface updates but terminal sync_response was absent; empty population not established".to_string(),
                )
            }
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
            && decoded.sync_complete
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
            facts_rejected = facts_rejected.saturating_add(1);
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
            "decoded {} gNMI responses / {} updates / {} JSON-IETF bytes under bounded subscribe-once ceilings",
            decoded.response_count, decoded.update_count, decoded.decoded_json_bytes
        ),
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
    if address_entries_omitted_total > 0 {
        append_bounded_note(
            &mut notes,
            format!("{address_entries_omitted_total} address entries omitted/not accepted"),
        );
    }
    if entities_omitted > 0 {
        append_bounded_note(
            &mut notes,
            format!("{entities_omitted} entities omitted at resource limit"),
        );
    }
    if invalid_entities > 0 {
        append_bounded_note(
            &mut notes,
            format!("{invalid_entities} invalid interface identities/entities rejected"),
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
    }
    for sample in rejected_samples.into_iter().take(8) {
        append_bounded_note(&mut notes, format!("rejected: {sample}"));
    }

    let facts_emitted = facts.len();
    let detected_conflicts: usize = conflict_maps.values().map(|m| m.detected_count).sum();
    let conflicts_omitted: usize = conflict_maps.values().map(|m| m.omitted_count).sum();
    NormalizationOutput {
        facts,
        interfaces: diagnostics,
        report: NormalizationReport {
            nos: "cisco_iosxr",
            applicability: "xrd 24.4.2 openconfig-interfaces gnmi subscribe-once",
            interfaces_observed: entities_accepted,
            records_decoded: Some(records_decoded),
            entities_encountered: Some(records_decoded.saturating_add(invalid_entities)),
            entities_accepted: Some(entities_accepted),
            status_facts_emitted: Some(status_facts_emitted),
            validated_field_coverage: Some(validated_coverage),
            expected_interface_total: None,
            facts_emitted,
            duplicate_ifnames: 0,
            merge_conflicts: detected_conflicts,
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
            addresses_omitted: address_entries_omitted_total,
            invalid_entities,
            facts_rejected,
            output_facts_omitted,
            conflicts_omitted,
            context_error: None,
            capture_skew_seconds: match (decoded.min_timestamp_ns, decoded.max_timestamp_ns) {
                (Some(min), Some(max)) => Some(((max - min) / 1_000_000_000) as i64),
                _ => None,
            },
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

    fn decode_payload(fact: &TypedFact) -> InterfaceStatePayload {
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
        let p = decode_payload(mgmt);
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
        let np = decode_payload(null0);
        assert_eq!(np.interface_type.value, Some(InterfaceType::Virtual));
        assert_eq!(np.ifindex.value, Some(2));
        assert_eq!(np.mtu.value, Some(1500));
        assert_eq!(np.speed.state, FieldAvailability::Unsupported);
    }

    #[test]
    fn missing_sync_marks_completeness_unknown() {
        let mut v: Value = serde_json::from_str(GENUINE).unwrap();
        v["responses"].as_array_mut().unwrap().pop();
        let response_count = v["responses"].as_array().unwrap().len();
        v["count"] = Value::from(response_count);
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

    fn response_for(name: &str, payload: &Value, timestamp_ns: u64) -> Value {
        let encoded = base64::engine::general_purpose::STANDARD.encode(payload.to_string());
        serde_json::json!({
            "timestamp": timestamp_ns.to_string(),
            "prefix": { "origin": "openconfig-interfaces" },
            "update": [{
                "path": {"elem": [
                    {"name": "interfaces"},
                    {"name": "interface", "key": {"name": name}}
                ]},
                "val": {"json_ietf_val": encoded}
            }]
        })
    }

    fn capture_with(responses: Vec<Value>, sync: bool) -> String {
        let mut responses = responses;
        if sync {
            responses.push(serde_json::json!({"sync_response": true}));
        }
        serde_json::json!({
            "ok": true,
            "error": null,
            "responses": responses,
            "count": responses.len()
        })
        .to_string()
    }

    fn state_payload(name: &str) -> Value {
        serde_json::json!({
            "state": {
                "name": name,
                "type": "iana-if-type:ethernetCsmacd",
                "mtu": 1514,
                "ifindex": 3,
                "admin-status": "UP",
                "oper-status": "UP",
                "logical": false
            }
        })
    }

    #[test]
    fn wrong_model_origin_is_rejected() {
        let mut v: Value = serde_json::from_str(GENUINE).unwrap();
        for response in v["responses"].as_array_mut().unwrap() {
            if let Some(prefix) = response.get_mut("prefix") {
                prefix["origin"] = Value::String("foreign-model".into());
            }
        }
        let raw = v.to_string();
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.facts.len(), 1);
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
    }

    #[test]
    fn foreign_module_state_cannot_inject_openconfig_fields() {
        let payload = serde_json::json!({
            "foreign-module:state": {
                "name": "Eth0",
                "admin-status": "DOWN",
                "oper-status": "DOWN",
                "ifindex": 99,
                "mtu": 9000,
                "type": "iana-if-type:ethernetCsmacd",
                "logical": false
            }
        });
        let raw = capture_with(
            vec![response_for("Eth0", &payload, 1_700_000_000_000_000_000)],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.facts.len(), 1);
        let p = decode_payload(&out.facts[0]);
        assert_eq!(p.administrative_state.state, FieldAvailability::Unknown);
        assert_ne!(p.administrative_state.value, Some(AdminState::Down));
        assert_eq!(p.ifindex.state, FieldAvailability::Unknown);
    }

    #[test]
    fn invalid_interface_identities_do_not_become_empty_success() {
        let payload = state_payload("bad\nname");
        let raw = capture_with(
            vec![response_for(
                "bad\nname",
                &payload,
                1_700_000_000_000_000_000,
            )],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.report.invalid_entities, 1);
        assert_eq!(out.facts.len(), 1);
        assert_eq!(
            out.facts[0].payload.state,
            Some(PayloadState::NoUsableRecords)
        );
        assert_ne!(out.facts[0].payload.state, Some(PayloadState::Empty));
    }

    #[test]
    fn path_payload_identity_mismatch_invalidates_entity() {
        let payload = state_payload("Other0");
        let raw = capture_with(
            vec![response_for("Eth0", &payload, 1_700_000_000_000_000_000)],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.report.invalid_entities, 1);
        assert_eq!(out.facts.len(), 1);
        assert_eq!(
            out.facts[0].payload.state,
            Some(PayloadState::NoUsableRecords)
        );
    }

    #[test]
    fn root_payload_identity_mismatch_invalidates_entity() {
        let mut payload = state_payload("Eth0");
        payload["name"] = Value::String("Ethernet2".into());
        let raw = capture_with(
            vec![response_for("Eth0", &payload, 1_700_000_000_000_000_000)],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.report.invalid_entities, 1);
        assert_eq!(
            out.facts[0].payload.state,
            Some(PayloadState::NoUsableRecords)
        );
    }

    #[test]
    fn malformed_payload_identity_types_invalidate_entity() {
        for mutate in [
            (|payload: &mut Value| {
                payload["name"] = serde_json::json!(42);
            }) as fn(&mut Value),
            (|payload: &mut Value| {
                payload["state"]["name"] = serde_json::json!(false);
            }),
            (|payload: &mut Value| {
                payload["config"] = serde_json::json!({"name": 100});
            }),
        ] {
            let mut payload = state_payload("Eth0");
            mutate(&mut payload);
            let raw = capture_with(
                vec![response_for("Eth0", &payload, 1_700_000_000_000_000_000)],
                true,
            );
            let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
            assert_eq!(out.report.invalid_entities, 1);
            assert_eq!(
                out.facts[0].payload.state,
                Some(PayloadState::NoUsableRecords)
            );
        }
    }

    #[test]
    fn conflicting_identity_across_notifications_invalidates_entity() {
        let first = state_payload("Eth0");
        let mut second = state_payload("Eth0");
        second["state"]["name"] = Value::String("Ethernet2".into());
        let raw = capture_with(
            vec![
                response_for("Eth0", &first, 1_700_000_000_000_000_000),
                response_for("Eth0", &second, 1_700_000_000_000_000_001),
            ],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.report.invalid_entities, 1);
        assert_eq!(
            out.facts[0].payload.state,
            Some(PayloadState::NoUsableRecords)
        );
    }

    #[test]
    fn actual_update_count_is_bounded() {
        let payload = state_payload("Eth0");
        let update = response_for("Eth0", &payload, 1_700_000_000_000_000_000)["update"][0].clone();
        let mut response = serde_json::json!({
            "timestamp": "1700000000000000000",
            "prefix": {"origin": "openconfig-interfaces"},
            "update": []
        });
        for _ in 0..=MAX_UPDATES_PER_OBSERVATION {
            response["update"]
                .as_array_mut()
                .unwrap()
                .push(update.clone());
        }
        let raw = capture_with(vec![response], true);
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.facts.len(), 1);
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
    }

    #[test]
    fn address_entries_beyond_limit_are_counted() {
        let mut addresses = Vec::new();
        for i in 0..65u8 {
            addresses.push(serde_json::json!({
                "ip": format!("10.0.0.{i}"),
                "state": {"ip": format!("10.0.0.{i}"), "prefix-length": 24}
            }));
        }
        let mut payload = state_payload("Eth0");
        payload["subinterfaces"] = serde_json::json!({
            "subinterface": [{
                "index": 0,
                "openconfig-if-ip:ipv4": {"addresses": {"address": addresses}}
            }]
        });
        let raw = capture_with(
            vec![response_for("Eth0", &payload, 1_700_000_000_000_000_000)],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.report.addresses_omitted, 1);
        assert_eq!(out.interfaces[0].addresses_omitted, 1);
        let p = decode_payload(&out.facts[0]);
        assert!(
            p.ip_addresses
                .as_ref()
                .and_then(|f| f.detail.as_deref())
                .is_some_and(|d| d.contains("omitted"))
        );
    }

    #[test]
    fn missing_ethernet_state_on_ethernet_interface_is_unknown_not_unsupported() {
        let raw = capture_with(
            vec![response_for(
                "Eth0",
                &state_payload("Eth0"),
                1_700_000_000_000_000_000,
            )],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        let p = decode_payload(&out.facts[0]);
        assert_eq!(p.speed.state, FieldAvailability::Unknown);
    }

    #[test]
    fn empty_without_sync_is_not_authoritative_empty() {
        let raw = capture_with(vec![], false);
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.facts.len(), 1);
        assert_ne!(out.facts[0].payload.state, Some(PayloadState::Empty));
        assert_eq!(out.facts[0].quality.completeness, Completeness::Unknown);
    }

    #[test]
    fn sync_only_response_is_genuinely_empty_scoped_population() {
        let raw = capture_with(vec![], true);
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.facts.len(), 1);
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::Empty));
        assert_eq!(out.facts[0].quality.completeness, Completeness::Complete);
    }

    #[test]
    fn deletes_are_explicitly_rejected_in_subscribe_once_slice() {
        let mut response = response_for("Eth0", &state_payload("Eth0"), 1_700_000_000_000_000_000);
        response.as_object_mut().unwrap().insert(
            "delete".into(),
            serde_json::json!([
                {"elem": [{"name":"interfaces"},{"name":"interface","key":{"name":"Eth0"}}]}
            ]),
        );
        let raw = capture_with(vec![response], true);
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
    }

    #[test]
    fn update_after_terminal_sync_is_rejected() {
        let mut v: Value = serde_json::from_str(&capture_with(
            vec![response_for(
                "Eth0",
                &state_payload("Eth0"),
                1_700_000_000_000_000_000,
            )],
            true,
        ))
        .unwrap();
        let sync = v["responses"].as_array_mut().unwrap().pop().unwrap();
        v["responses"].as_array_mut().unwrap().insert(0, sync);
        let raw = v.to_string();
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
    }

    #[test]
    fn duplicate_sync_is_rejected() {
        let mut v: Value = serde_json::from_str(&capture_with(vec![], true)).unwrap();
        v["responses"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"sync_response": true}));
        let raw = v.to_string();
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
    }

    #[test]
    fn unexpected_response_type_is_rejected() {
        let raw = capture_with(vec![serde_json::json!({"unexpected": true})], true);
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
    }

    #[test]
    fn malformed_notification_timestamp_is_rejected() {
        let mut response = response_for("Eth0", &state_payload("Eth0"), 1_700_000_000_000_000_000);
        response["timestamp"] = Value::String("not-a-timestamp".into());
        let raw = capture_with(vec![response], true);
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
    }

    #[test]
    fn nonempty_prefix_with_valid_relative_path_is_resolved() {
        let mut response = response_for("Eth0", &state_payload("Eth0"), 1_700_000_000_000_000_000);
        response["prefix"] = serde_json::json!({
            "origin": "openconfig-interfaces",
            "elem": [{"name": "interfaces"}]
        });
        response["update"][0]["path"] = serde_json::json!({
            "elem": [{"name": "interface", "key": {"name": "Eth0"}}]
        });
        let raw = capture_with(vec![response], true);
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.report.entities_accepted, Some(1));
    }

    #[test]
    fn conflicting_prefix_and_update_origin_are_rejected() {
        let mut response = response_for("Eth0", &state_payload("Eth0"), 1_700_000_000_000_000_000);
        response["update"][0]["path"]["origin"] = Value::String("foreign-model".into());
        let raw = capture_with(vec![response], true);
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
    }

    #[test]
    fn namespace_collision_is_rejected_not_first_wins() {
        let mut payload = state_payload("Eth0");
        payload["foreign-module:state"] = serde_json::json!({"admin-status": "DOWN"});
        let raw = capture_with(
            vec![response_for("Eth0", &payload, 1_700_000_000_000_000_000)],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
    }

    #[test]
    fn duplicate_json_identity_keys_are_rejected() {
        let duplicate = br#"{"state":{"name":"Eth0","name":"Other0","type":"iana-if-type:ethernetCsmacd","logical":false}}"#;
        let encoded = base64::engine::general_purpose::STANDARD.encode(duplicate);
        let response = serde_json::json!({
            "timestamp": "1700000000000000000",
            "prefix": {"origin": "openconfig-interfaces"},
            "update": [{
                "path": {"elem": [
                    {"name": "interfaces"},
                    {"name": "interface", "key": {"name": "Eth0"}}
                ]},
                "val": {"json_ietf_val": encoded}
            }]
        });
        let raw = capture_with(vec![response], true);
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
    }

    #[test]
    fn conflicting_logical_prerequisite_invalidates_interface_type() {
        let mut a = state_payload("Eth0");
        a["state"]["logical"] = Value::Bool(false);
        let mut b = state_payload("Eth0");
        b["state"]["logical"] = Value::Bool(true);
        let raw = capture_with(
            vec![
                response_for("Eth0", &a, 1_700_000_000_000_000_000),
                response_for("Eth0", &b, 1_700_000_000_000_000_001),
            ],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        let p = decode_payload(&out.facts[0]);
        assert_eq!(p.interface_type.state, FieldAvailability::Invalid);
    }

    #[test]
    fn conflicting_auto_negotiation_prerequisite_invalidates_speed() {
        let mut a = state_payload("Eth0");
        let mut b = state_payload("Eth0");
        for (payload, auto) in [(&mut a, true), (&mut b, false)] {
            payload["openconfig-if-ethernet:ethernet"] = serde_json::json!({
                "state": {
                    "auto-negotiate": auto,
                    "port-speed": "SPEED_1GB",
                    "negotiated-port-speed": "SPEED_UNKNOWN"
                }
            });
        }
        let raw = capture_with(
            vec![
                response_for("Eth0", &a, 1_700_000_000_000_000_000),
                response_for("Eth0", &b, 1_700_000_000_000_000_001),
            ],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        let p = decode_payload(&out.facts[0]);
        assert_eq!(p.speed.state, FieldAvailability::Invalid);
    }

    #[test]
    fn foreign_identityref_namespace_is_invalid_not_stripped() {
        let mut payload = state_payload("Eth0");
        payload["state"]["type"] = Value::String("foreign-if-types:ethernetCsmacd".into());
        let raw = capture_with(
            vec![response_for("Eth0", &payload, 1_700_000_000_000_000_000)],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        let p = decode_payload(&out.facts[0]);
        assert_eq!(p.interface_type.state, FieldAvailability::Invalid);
    }

    #[test]
    fn lowercase_enum_is_invalid_not_case_folded() {
        let mut payload = state_payload("Eth0");
        payload["state"]["admin-status"] = Value::String("up".into());
        let raw = capture_with(
            vec![response_for("Eth0", &payload, 1_700_000_000_000_000_000)],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        let p = decode_payload(&out.facts[0]);
        assert_eq!(p.administrative_state.state, FieldAvailability::Invalid);
    }

    #[test]
    fn yang_numeric_bounds_are_enforced() {
        let mut payload = state_payload("Eth0");
        payload["state"]["mtu"] = Value::from(65536u32);
        payload["state"]["ifindex"] = Value::from(u32::MAX as u64 + 1);
        let raw = capture_with(
            vec![response_for("Eth0", &payload, 1_700_000_000_000_000_000)],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        let p = decode_payload(&out.facts[0]);
        assert_eq!(p.mtu.state, FieldAvailability::Invalid);
        assert_eq!(p.ifindex.state, FieldAvailability::Invalid);
    }

    #[test]
    fn nonboolean_logical_prerequisite_is_invalid() {
        let mut payload = state_payload("Eth0");
        payload["state"]["logical"] = Value::String("false".into());
        let raw = capture_with(
            vec![response_for("Eth0", &payload, 1_700_000_000_000_000_000)],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        let p = decode_payload(&out.facts[0]);
        assert_eq!(p.interface_type.state, FieldAvailability::Invalid);
    }

    #[test]
    fn subinterface_key_state_mismatch_invalidates_entity() {
        let mut payload = state_payload("Eth0");
        payload["subinterfaces"] = serde_json::json!({
            "subinterface": [{"index": 0, "state": {"index": 1}}]
        });
        let raw = capture_with(
            vec![response_for("Eth0", &payload, 1_700_000_000_000_000_000)],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.report.invalid_entities, 1);
        assert_eq!(
            out.facts[0].payload.state,
            Some(PayloadState::NoUsableRecords)
        );
    }

    #[test]
    fn foreign_speed_identityref_namespace_is_invalid() {
        let mut payload = state_payload("Eth0");
        payload["openconfig-if-ethernet:ethernet"] = serde_json::json!({
            "state": {
                "auto-negotiate": false,
                "port-speed": "foreign-speeds:SPEED_1GB"
            }
        });
        let raw = capture_with(
            vec![response_for("Eth0", &payload, 1_700_000_000_000_000_000)],
            true,
        );
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        let p = decode_payload(&out.facts[0]);
        assert_eq!(p.speed.state, FieldAvailability::Invalid);
    }

    #[test]
    fn path_target_and_atomic_notification_are_fail_closed() {
        let mut targeted = response_for("Eth0", &state_payload("Eth0"), 1_700_000_000_000_000_000);
        targeted["prefix"]["target"] = Value::String("unbound-target".into());
        let raw = capture_with(vec![targeted], true);
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));

        let mut atomic = response_for("Eth0", &state_payload("Eth0"), 1_700_000_000_000_000_000);
        atomic["atomic"] = Value::Bool(true);
        let raw = capture_with(vec![atomic], true);
        let out = normalize_xrd_openconfig_interfaces(&obs(&raw), NormalizerLimits::default());
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
    }
}
