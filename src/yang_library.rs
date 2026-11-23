//! Bounded, read-only RFC 8525 YANG Library discovery over gNMI Get
//! (M6C-1B).
//!
//! Reuses the existing gNMI transport, credential and TLS machinery —
//! no new transport, no device writes, no NETCONF. Two response shapes
//! are handled:
//!
//! 1. A single `json_val`/`json_ietf_val` TypedValue carrying the whole
//!    subtree (common on modern implementations).
//! 2. Many scalar updates whose gNMI paths are reconstructed into a
//!    JSON tree before parsing (the "actual gNMI Get response
//!    structure" — data need not arrive as one JSON object).
//!
//! Everything is bounded: candidate path count, RPC count/timeouts,
//! update count, JSON bytes, module/feature/deviation counts and JSON
//! nesting. Malformed or oversized evidence fails closed into an
//! explicit `YangLibraryOutcome` — never an assumed complete library.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;
use tonic::transport::Channel;
use tonic::{Request, Status};

use crate::discovery::{DiscoveryInput, add_auth_metadata, connect};
use crate::proto::gnmi::g_nmi_client::GNmiClient;
use crate::proto::gnmi::{Encoding, GetRequest, Path, PathElem, TypedValue};
use crate::yang_schema::applicability::{
    YangLibraryDatastore, YangLibraryEvidence, YangLibraryModule, YangLibraryModuleSet,
    YangLibraryOutcome, YangLibrarySchema, YangLibraryTree,
};

/// Whole-probe deadline — capabilities + library must finish inside it.
pub const LIBRARY_PROBE_TIMEOUT: Duration = Duration::from_secs(25);
/// Per-Get deadline.
const GET_TIMEOUT: Duration = Duration::from_secs(6);
/// Maximum gNMI updates reconstructed from one Get response.
const MAX_UPDATES: usize = 100_000;
/// Maximum bytes accepted in one JSON TypedValue payload.
const MAX_JSON_VALUE_BYTES: usize = 8 * 1024 * 1024;
/// Maximum modules (implemented + import-only) across the effective set.
const MAX_MODULES: usize = 4_096;
/// Maximum feature entries per module. Live SR Linux evidence: the
/// `srl_nokia-features` module legitimately advertises 620 features, so
/// the bound must exceed real vendor catalogs while still capping
/// per-module memory.
const MAX_FEATURES_PER_MODULE: usize = 2_048;
/// Maximum deviation entries per module.
const MAX_DEVIATIONS_PER_MODULE: usize = 128;
/// Maximum submodule entries per module.
const MAX_SUBMODULES_PER_MODULE: usize = 64;
/// Maximum schema/module-set/datastore entries.
const MAX_LIST_ENTRIES: usize = 4_096;
/// serde_json's default recursion limit is 128; stay well under it.
const MAX_JSON_DEPTH: usize = 48;

/// Ordered NMDA `/yang-library` candidates. The canonical RFC 8525
/// module-qualified name `ietf-yang-library:yang-library` is tried
/// first — it is the only form guaranteed correct on strict devices
/// (live SR Linux evidence rejects the `yang:`/`yang-library:` prefix
/// aliases with INVALID_ARGUMENT). The remaining aliases and the bare
/// name are kept as compatibility fallbacks for implementations that
/// only accept those forms. `ietf` origin is tried before the empty
/// origin for each candidate.
const NMDA_CANDIDATES: &[&str] = &[
    "ietf-yang-library:yang-library",
    "yang:yang-library",
    "yang-library:yang-library",
    "yang-library",
];
/// Ordered legacy RFC 7895 `/modules-state` candidates — canonical
/// `ietf-yang-library` module name first, aliases and bare name as
/// fallbacks (same live-evidence rationale as NMDA above).
const LEGACY_CANDIDATES: &[&str] = &[
    "ietf-yang-library:modules-state",
    "yang:modules-state",
    "modules-state:modules-state",
    "modules-state",
];

/// Result of one bounded library probe.
#[derive(Clone, Debug)]
pub struct YangLibraryProbeResult {
    pub outcome: YangLibraryOutcome,
    /// Diagnostics: which candidates were tried and why they failed.
    pub attempts: Vec<String>,
    pub probed_at_ns: i64,
}

/// Probe the device's YANG Library over an already-established,
/// policy-checked gNMI connection description. Performs: connect →
/// optional Capabilities (for provenance) → bounded `/yang-library`
/// Get → legacy `/modules-state` fallback → bounded parse.
///
/// Never issues writes. Returns an explicit outcome for every failure
/// class; only `Complete`/`Legacy` carry usable evidence.
pub async fn probe_yang_library(input: &DiscoveryInput) -> YangLibraryProbeResult {
    let probed_at_ns = crate::graph::common::now_ns();
    let mut attempts = Vec::new();
    let deadline = tokio::time::Instant::now() + LIBRARY_PROBE_TIMEOUT;

    let channel = match tokio::time::timeout_at(deadline, connect(input)).await {
        Ok(Ok(channel)) => channel,
        Ok(Err(e)) => {
            attempts.push(format!("connect failed: {e:#}"));
            return finish(
                YangLibraryOutcome::TransportFailure {
                    reason: format!("connect failed: {e:#}"),
                },
                attempts,
                probed_at_ns,
            );
        }
        Err(_) => {
            attempts.push("connect timed out".to_string());
            return finish(YangLibraryOutcome::Timeout, attempts, probed_at_ns);
        }
    };

    let username = input.username.clone().or_else(|| {
        input
            .username_env
            .as_deref()
            .and_then(|v| std::env::var(v).ok())
    });
    let password = input.password.clone().or_else(|| {
        input
            .password_env
            .as_deref()
            .and_then(|v| std::env::var(v).ok())
    });

    // ── NMDA /yang-library ────────────────────────────────────────────
    match probe_tree(
        &channel,
        username.as_deref(),
        password.as_deref(),
        NMDA_CANDIDATES,
        TreeKind::Nmda,
        deadline,
        &mut attempts,
    )
    .await
    {
        ProbeTreeResult::Found(tree) => {
            match parse_nmda_library(&tree, probed_at_ns) {
                Ok(evidence) => {
                    return finish(
                        YangLibraryOutcome::Complete { evidence },
                        attempts,
                        probed_at_ns,
                    );
                }
                Err(reason) => {
                    attempts.push(format!("yang-library parse failed: {reason}"));
                    // Malformed NMDA data must not fall through to a
                    // weaker legacy interpretation silently — but the
                    // legacy tree is a different container, so a missing
                    // /yang-library with a valid /modules-state is still
                    // legitimate. Only short-circuit on *conflicting*
                    // module definitions inside a parsed NMDA tree.
                    if reason.starts_with("conflicting") {
                        return finish(
                            YangLibraryOutcome::ConflictingModuleDefinitions { reason },
                            attempts,
                            probed_at_ns,
                        );
                    }
                    // Otherwise continue to legacy probe.
                }
            }
        }
        ProbeTreeResult::Denied(reason) => {
            attempts.push(format!("yang-library permission denied: {reason}"));
            return finish(YangLibraryOutcome::PermissionDenied, attempts, probed_at_ns);
        }
        ProbeTreeResult::UnsupportedEncoding => {
            attempts.push("yang-library unsupported encoding".to_string());
            return finish(
                YangLibraryOutcome::UnsupportedEncoding,
                attempts,
                probed_at_ns,
            );
        }
        ProbeTreeResult::Fatal(outcome) => {
            attempts.push(format!(
                "yang-library probe failed: {}",
                outcome_name(&outcome)
            ));
            // A transport/timeout failure on the primary tree does not
            // prove the legacy tree is absent; still try it within the
            // deadline — bounded by the same overall timeout.
        }
        ProbeTreeResult::Absent => {}
    }

    // ── legacy /modules-state ─────────────────────────────────────────
    match probe_tree(
        &channel,
        username.as_deref(),
        password.as_deref(),
        LEGACY_CANDIDATES,
        TreeKind::Legacy,
        deadline,
        &mut attempts,
    )
    .await
    {
        ProbeTreeResult::Found(tree) => match parse_legacy_library(&tree, probed_at_ns) {
            Ok(evidence) => finish(
                YangLibraryOutcome::Legacy { evidence },
                attempts,
                probed_at_ns,
            ),
            Err(reason) => finish(
                YangLibraryOutcome::Malformed {
                    reason: format!("modules-state parse failed: {reason}"),
                },
                attempts,
                probed_at_ns,
            ),
        },
        ProbeTreeResult::Denied(_) => {
            finish(YangLibraryOutcome::PermissionDenied, attempts, probed_at_ns)
        }
        ProbeTreeResult::UnsupportedEncoding => finish(
            YangLibraryOutcome::UnsupportedEncoding,
            attempts,
            probed_at_ns,
        ),
        ProbeTreeResult::Fatal(outcome) => finish(outcome, attempts, probed_at_ns),
        ProbeTreeResult::Absent => {
            attempts.push("no yang-library or modules-state data returned".to_string());
            finish(YangLibraryOutcome::Unsupported, attempts, probed_at_ns)
        }
    }
}

fn finish(
    outcome: YangLibraryOutcome,
    attempts: Vec<String>,
    probed_at_ns: i64,
) -> YangLibraryProbeResult {
    YangLibraryProbeResult {
        outcome,
        attempts,
        probed_at_ns,
    }
}

fn outcome_name(outcome: &YangLibraryOutcome) -> &'static str {
    match outcome {
        YangLibraryOutcome::Complete { .. } => "complete",
        YangLibraryOutcome::Legacy { .. } => "legacy",
        YangLibraryOutcome::Unsupported => "unsupported",
        YangLibraryOutcome::PermissionDenied => "permission_denied",
        YangLibraryOutcome::Incomplete { .. } => "incomplete",
        YangLibraryOutcome::Malformed { .. } => "malformed",
        YangLibraryOutcome::Timeout => "timeout",
        YangLibraryOutcome::TransportFailure { .. } => "transport_failure",
        YangLibraryOutcome::ConflictingModuleDefinitions { .. } => "conflicting_module_definitions",
        YangLibraryOutcome::UnsupportedEncoding => "unsupported_encoding",
        YangLibraryOutcome::DatastoreSchemaMissing { .. } => "datastore_schema_missing",
        YangLibraryOutcome::DatastoreSchemaAmbiguous { .. } => "datastore_schema_ambiguous",
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TreeKind {
    Nmda,
    Legacy,
}

enum ProbeTreeResult {
    /// Data found and reconstructed into a JSON tree.
    Found(Value),
    /// All candidates returned empty — try the next tree kind.
    Absent,
    /// Auth failure — do not retry weaker paths.
    Denied(String),
    /// Encoding not usable.
    UnsupportedEncoding,
    /// Fatal transport/protocol failure.
    Fatal(YangLibraryOutcome),
}

/// Try each candidate path (ietf origin, then empty origin) until one
/// returns update data. Reconstructs the JSON tree from gNMI updates.
async fn probe_tree(
    channel: &Channel,
    username: Option<&str>,
    password: Option<&str>,
    candidates: &[&str],
    kind: TreeKind,
    deadline: tokio::time::Instant,
    attempts: &mut Vec<String>,
) -> ProbeTreeResult {
    let mut client = GNmiClient::new(channel.clone());
    for candidate in candidates {
        for origin in ["ietf", ""] {
            if tokio::time::Instant::now() >= deadline {
                return ProbeTreeResult::Fatal(YangLibraryOutcome::Timeout);
            }
            let path = Path {
                origin: origin.to_string(),
                elem: vec![PathElem {
                    name: candidate.to_string(),
                    key: Default::default(),
                    ..Default::default()
                }],
                ..Default::default()
            };
            // Prefer JSON_IETF; fall back to plain JSON for the same
            // path when the device rejects the IETF encoding.
            for encoding in [Encoding::JsonIetf, Encoding::Json] {
                let request = GetRequest {
                    prefix: None,
                    path: vec![path.clone()],
                    r#type: crate::proto::gnmi::get_request::DataType::All as i32,
                    encoding: encoding as i32,
                    use_models: vec![],
                    extension: vec![],
                };
                let mut req = Request::new(request);
                add_auth_metadata(&mut req, username, password);

                let response = match tokio::time::timeout_at(
                    deadline.min(tokio::time::Instant::now() + GET_TIMEOUT),
                    client.get(req),
                )
                .await
                {
                    Err(_) => {
                        attempts.push(format!(
                            "{candidate} (origin '{origin}', {}): timed out",
                            encoding.as_str_name()
                        ));
                        continue;
                    }
                    Ok(Err(status)) => {
                        let mapped = map_get_error(&status);
                        attempts.push(format!(
                            "{candidate} (origin '{origin}', {}): {}",
                            encoding.as_str_name(),
                            status.message()
                        ));
                        match mapped {
                            ProbeTreeResult::Denied(r) => return ProbeTreeResult::Denied(r),
                            ProbeTreeResult::UnsupportedEncoding => {
                                // Try the next encoding before giving up.
                                continue;
                            }
                            ProbeTreeResult::Fatal(o) => {
                                if matches!(
                                    o,
                                    YangLibraryOutcome::TransportFailure { .. }
                                        | YangLibraryOutcome::Incomplete { .. }
                                ) {
                                    continue;
                                }
                                return ProbeTreeResult::Fatal(o);
                            }
                            other => return other,
                        }
                    }
                    Ok(Ok(resp)) => resp.into_inner(),
                };

                match reconstruct_tree(&response, kind) {
                    Ok(Some(tree)) => return ProbeTreeResult::Found(tree),
                    Ok(None) => {
                        attempts.push(format!(
                            "{candidate} (origin '{origin}', {}): empty",
                            encoding.as_str_name()
                        ));
                        break;
                    }
                    Err(reason) => {
                        attempts.push(format!(
                            "{candidate} (origin '{origin}', {}): {reason}",
                            encoding.as_str_name()
                        ));
                        return ProbeTreeResult::Fatal(YangLibraryOutcome::Malformed { reason });
                    }
                }
            }
        }
    }
    ProbeTreeResult::Absent
}

fn map_get_error(status: &Status) -> ProbeTreeResult {
    use tonic::Code;
    match status.code() {
        Code::Unauthenticated | Code::PermissionDenied => {
            ProbeTreeResult::Denied(status.message().to_string())
        }
        Code::Unimplemented => ProbeTreeResult::UnsupportedEncoding,
        Code::NotFound | Code::InvalidArgument => {
            ProbeTreeResult::Fatal(YangLibraryOutcome::TransportFailure {
                reason: format!("{}: {}", status.code(), status.message()),
            })
        }
        Code::DeadlineExceeded => ProbeTreeResult::Fatal(YangLibraryOutcome::Timeout),
        Code::ResourceExhausted => ProbeTreeResult::Fatal(YangLibraryOutcome::Incomplete {
            reason: "response too large".to_string(),
        }),
        _ => ProbeTreeResult::Fatal(YangLibraryOutcome::TransportFailure {
            reason: format!("{}: {}", status.code(), status.message()),
        }),
    }
}

// ── gNMI response → JSON tree ──────────────────────────────────────────

/// Reconstruct a JSON tree from a GetResponse. Returns Ok(None) when the
/// response carries no usable data.
fn reconstruct_tree(
    response: &crate::proto::gnmi::GetResponse,
    _kind: TreeKind,
) -> Result<Option<Value>, String> {
    let mut root = Value::Object(serde_json::Map::new());
    let mut update_count = 0usize;
    let mut json_bytes = 0usize;

    for notification in &response.notification {
        // RFC 6901-style gNMI relative-path rule: when a notification
        // carries a prefix, each update path is relative to it. Some
        // implementations send the full path in `update.path` anyway;
        // joining is still correct because an absolute update path is
        // treated as already rooted (see `join_path_segments`).
        let prefix_segments = match notification.prefix.as_ref() {
            Some(prefix) => Some(path_segments(prefix)?),
            None => None,
        };
        for update in &notification.update {
            update_count += 1;
            if update_count > MAX_UPDATES {
                return Err(format!("too many updates (>{MAX_UPDATES})"));
            }
            let (Some(update_path), Some(update_val)) = (&update.path, &update.val) else {
                continue;
            };
            let mut segments = prefix_segments.clone().unwrap_or_default();
            let update_segments = path_segments(update_path)?;
            if update_segments.first().is_some_and(|(name, _)| name == "/") {
                // Absolute update path replaces the prefix rather than
                // being appended to it.
                segments = update_segments;
            } else {
                segments.extend(update_segments);
            }
            if segments.is_empty() {
                return Err("empty reconstructed path".to_string());
            }
            let value = typed_value_to_json(update_val, &mut json_bytes)?;
            insert_at_path(&mut root, &segments, value)?;
        }
    }
    if update_count == 0 {
        return Ok(None);
    }
    Ok(Some(root))
}

/// Convert a proto Path into (elem_name, sorted key pairs) segments.
fn path_segments(path: &Path) -> Result<Vec<(String, Vec<(String, String)>)>, String> {
    let mut out = Vec::new();
    for elem in &path.elem {
        if elem.name.is_empty() {
            return Err("empty path element name".to_string());
        }
        let mut keys: Vec<(String, String)> = elem
            .key
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        keys.sort();
        out.push((elem.name.clone(), keys));
    }
    if out.is_empty() {
        return Err("empty path".to_string());
    }
    Ok(out)
}

/// Insert `value` at the location described by `segments`, building
/// nested objects and keyed list entries as needed. Intermediate
/// segments become objects; keyed segments become arrays of objects
/// (YANG lists) keyed by their key fields; a keyed FINAL segment is a
/// leaf-list element appended to an array.
fn insert_at_path(
    root: &mut Value,
    segments: &[(String, Vec<(String, String)>)],
    value: Value,
) -> Result<(), String> {
    if segments.is_empty() {
        return Err("empty path segments".to_string());
    }
    let mut node = root;
    for (i, (name, keys)) in segments.iter().enumerate() {
        if i + 1 == segments.len() {
            return insert_leaf(node, name, keys, value);
        }
        node = descend_into(node, name, keys)?;
    }
    Ok(())
}

/// Ensure `parent[name]` exists as a container and return the node for
/// this segment: a keyed segment resolves to its list-entry object; an
/// unkeyed segment resolves to the child object itself.
fn descend_into<'a>(
    parent: &'a mut Value,
    name: &str,
    keys: &[(String, String)],
) -> Result<&'a mut Value, String> {
    let obj = parent
        .as_object_mut()
        .ok_or_else(|| "path parent is not an object".to_string())?;
    if keys.is_empty() {
        let entry = obj
            .entry(name.to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if !entry.is_object() {
            return Err(format!("path segment '{name}' is not a container"));
        }
        return Ok(entry);
    }
    // Keyed segment → YANG list entry inside an array.
    let list = obj
        .entry(name.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let arr = list
        .as_array_mut()
        .ok_or_else(|| format!("path segment '{name}' is not a list"))?;
    let idx = arr.iter().position(|item| {
        item.as_object().is_some_and(|m| {
            keys.iter()
                .all(|(k, v)| m.get(k).and_then(Value::as_str) == Some(v.as_str()))
        })
    });
    let idx = match idx {
        Some(i) => i,
        None => {
            let mut entry_obj = serde_json::Map::new();
            for (k, v) in keys {
                entry_obj.insert(k.clone(), Value::String(v.clone()));
            }
            arr.push(Value::Object(entry_obj));
            arr.len() - 1
        }
    };
    match arr.get_mut(idx) {
        Some(entry @ Value::Object(_)) => Ok(entry),
        Some(_) => Err(format!("list entry for '{name}' is not an object")),
        None => Err(format!("list entry for '{name}' missing")),
    }
}

/// Set the leaf `value` at `name` inside `parent`. Keyed leaves are
/// leaf-list elements: appended to an array (deduplicated) rather than
/// overwriting sibling entries.
fn insert_leaf(
    parent: &mut Value,
    name: &str,
    keys: &[(String, String)],
    value: Value,
) -> Result<(), String> {
    let obj = parent
        .as_object_mut()
        .ok_or_else(|| "leaf parent is not an object".to_string())?;
    if keys.is_empty() {
        obj.insert(name.to_string(), value);
        return Ok(());
    }
    // Leaf-list: element identity is the key value (gNMI uses "." for
    // leaf-list keys on some implementations, the leaf name otherwise).
    let element = keys
        .iter()
        .find(|(k, _)| k == ".")
        .map(|(_, v)| v.clone())
        .or_else(|| keys.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
        .unwrap_or_else(|| value.to_string());
    let list = obj
        .entry(name.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let arr = list
        .as_array_mut()
        .ok_or_else(|| format!("leaf-list '{name}' is not an array"))?;
    let already = arr.iter().any(|item| match item {
        Value::String(s) => *s == element,
        other => other.to_string() == element,
    });
    if !already {
        // Prefer the scalar value itself when it is a string; otherwise
        // store the key-derived element string.
        match &value {
            Value::String(s) => arr.push(Value::String(s.clone())),
            _ => arr.push(Value::String(element)),
        }
    }
    Ok(())
}

/// Strict bounded TypedValue → JSON conversion. Only scalar and JSON
/// encodings are accepted; anything else is an explicit error.
fn typed_value_to_json(value: &TypedValue, json_bytes: &mut usize) -> Result<Value, String> {
    use crate::proto::gnmi::typed_value::Value as V;
    let inner = value
        .value
        .as_ref()
        .ok_or_else(|| "empty TypedValue".to_string())?;
    match inner {
        V::StringVal(s) | V::AsciiVal(s) => Ok(Value::String(s.clone())),
        V::IntVal(i) => Ok(Value::from(*i)),
        V::UintVal(u) => Ok(Value::from(*u)),
        V::BoolVal(b) => Ok(Value::Bool(*b)),
        V::DoubleVal(d) => serde_json::Number::from_f64(*d)
            .map(Value::Number)
            .ok_or_else(|| "non-finite double".to_string()),
        V::FloatVal(f) => serde_json::Number::from_f64(*f as f64)
            .map(Value::Number)
            .ok_or_else(|| "non-finite float".to_string()),
        V::JsonVal(bytes) | V::JsonIetfVal(bytes) => {
            *json_bytes += bytes.len();
            if *json_bytes > MAX_JSON_VALUE_BYTES {
                return Err(format!("JSON payload exceeds {MAX_JSON_VALUE_BYTES} bytes"));
            }
            let text =
                std::str::from_utf8(bytes).map_err(|_| "JSON payload is not UTF-8".to_string())?;
            check_json_depth(text)?;
            serde_json::from_str::<Value>(text).map_err(|e| format!("invalid JSON: {e}"))
        }
        V::LeaflistVal(list) => {
            let mut out = Vec::new();
            for elem in &list.element {
                out.push(typed_value_to_json(elem, json_bytes)?);
            }
            Ok(Value::Array(out))
        }
        other => Err(format!(
            "unsupported TypedValue encoding: {}",
            match other {
                V::BytesVal(_) => "bytes_val",
                V::AnyVal(_) => "any_val",
                V::ProtoBytes(_) => "proto_bytes",
                V::DecimalVal(_) => "decimal_val",
                _ => "unknown",
            }
        )),
    }
}

/// Cheap pre-parse depth check: count max bracket nesting in the raw
/// JSON text so pathological nesting is rejected before serde_json
/// recursion (defense in depth; serde_json also enforces its own limit).
fn check_json_depth(text: &str) -> Result<(), String> {
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for ch in text.chars() {
        if in_str {
            if esc {
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' | '[' => {
                depth += 1;
                if depth > MAX_JSON_DEPTH {
                    return Err(format!("JSON nesting exceeds {MAX_JSON_DEPTH}"));
                }
            }
            '}' | ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

// ── JSON tree → YangLibraryEvidence ────────────────────────────────────

/// Locate the library container inside a reconstructed tree: the root
/// may be the container itself or wrap it under its element name
/// (possibly `prefix:name` form — matched on the local part).
fn library_container<'a>(tree: &'a Value, names: &[&str]) -> Option<&'a Value> {
    if let Value::Object(map) = tree {
        for (key, inner) in map {
            let local = key.rsplit(':').next().unwrap_or(key.as_str());
            if names.iter().any(|n| *n == local) {
                return Some(inner);
            }
        }
        // Unwrapped: the root itself looks like the container.
        if map.contains_key("module-set")
            || map.contains_key("content-id")
            || map.contains_key("module-set-id")
            || map.contains_key("schema")
            || map.contains_key("datastore")
        {
            return Some(tree);
        }
    }
    None
}

fn child<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object()?.get(key)
}

/// Normalize a YANG list node into entries. Accepts a JSON array of
/// objects, a single object, or an object keyed by entry name.
fn list_entries<'a>(v: Option<&'a Value>, what: &str) -> Result<Vec<&'a Value>, String> {
    let Some(v) = v else { return Ok(Vec::new()) };
    match v {
        Value::Array(items) => {
            if items.len() > MAX_LIST_ENTRIES {
                return Err(format!("{what} exceeds {MAX_LIST_ENTRIES} entries"));
            }
            Ok(items.iter().collect())
        }
        Value::Object(map) => {
            if map.len() > MAX_LIST_ENTRIES {
                return Err(format!("{what} exceeds {MAX_LIST_ENTRIES} entries"));
            }
            Ok(map.values().collect())
        }
        _ => Err(format!("{what} is not a list")),
    }
}

fn leaf_string(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Name of one YANG-library list entry. RFC 8525 `feature`/`deviation`
/// arrive as leaf-list strings (live SR Linux evidence), while legacy
/// `modules-state` deviations and `submodule` entries are objects
/// carrying a `name` leaf — accept both forms.
fn entry_name(item: &Value) -> Option<String> {
    leaf_string(Some(item)).or_else(|| leaf_string(child(item, "name")))
}

/// Parse one module entry (implemented or import-only) from JSON.
fn parse_module_entry(
    entry: &Value,
    import_only: bool,
    counts: &mut ModuleCounts,
) -> Result<YangLibraryModule, String> {
    counts.modules += 1;
    if counts.modules > MAX_MODULES {
        return Err(format!("module count exceeds {MAX_MODULES}"));
    }
    let name =
        leaf_string(child(entry, "name")).ok_or_else(|| "module entry missing name".to_string())?;
    let revision = leaf_string(child(entry, "revision"));
    let namespace = leaf_string(child(entry, "namespace"));

    let mut features = std::collections::BTreeSet::new();
    for f in list_entries(child(entry, "feature"), "feature")? {
        if features.len() >= MAX_FEATURES_PER_MODULE {
            return Err(format!("feature count exceeds {MAX_FEATURES_PER_MODULE}"));
        }
        if let Some(fname) = entry_name(f) {
            features.insert(fname);
        }
    }

    let mut deviations = Vec::new();
    for d in list_entries(child(entry, "deviation"), "deviation")? {
        if deviations.len() >= MAX_DEVIATIONS_PER_MODULE {
            return Err(format!(
                "deviation count exceeds {MAX_DEVIATIONS_PER_MODULE}"
            ));
        }
        if let Some(dname) = entry_name(d) {
            deviations.push(dname);
        }
    }

    let mut submodules = Vec::new();
    if !import_only {
        for s in list_entries(child(entry, "submodule"), "submodule")? {
            if submodules.len() >= MAX_SUBMODULES_PER_MODULE {
                return Err(format!(
                    "submodule count exceeds {MAX_SUBMODULES_PER_MODULE}"
                ));
            }
            if let Some(sname) = entry_name(s) {
                submodules.push(sname);
            }
        }
    }

    Ok(YangLibraryModule {
        name,
        revision,
        namespace,
        features,
        deviations,
        submodules,
    })
}

#[derive(Default)]
struct ModuleCounts {
    modules: usize,
}

/// Parse an RFC 8525 NMDA `/yang-library` container.
pub fn parse_nmda_library(tree: &Value, fetched_at_ns: i64) -> Result<YangLibraryEvidence, String> {
    let container = library_container(tree, &["yang-library", "yang_library"])
        .ok_or_else(|| "no yang-library container in response".to_string())?;

    let mut diagnostics = Vec::new();
    let content_id = leaf_string(child(container, "content-id"))
        .or_else(|| leaf_string(child(container, "content_id")));
    if content_id.is_none() {
        diagnostics.push("content-id absent — freshness binding weakened".to_string());
    }

    let mut module_sets = Vec::new();
    let mut counts = ModuleCounts::default();
    for ms in list_entries(child(container, "module-set"), "module-set")? {
        if module_sets.len() >= MAX_LIST_ENTRIES {
            return Err(format!("module-set count exceeds {MAX_LIST_ENTRIES}"));
        }
        let name = leaf_string(child(ms, "name"))
            .ok_or_else(|| "module-set entry missing name".to_string())?;
        let mut modules = Vec::new();
        for m in list_entries(child(ms, "module"), "module")? {
            modules.push(parse_module_entry(m, false, &mut counts)?);
        }
        let mut import_only = Vec::new();
        for m in list_entries(child(ms, "import-only-module"), "import-only-module")? {
            import_only.push(parse_module_entry(m, true, &mut counts)?);
        }
        module_sets.push(YangLibraryModuleSet {
            name,
            modules,
            import_only_modules: import_only,
        });
    }

    let mut schemas = Vec::new();
    for s in list_entries(child(container, "schema"), "schema")? {
        if schemas.len() >= MAX_LIST_ENTRIES {
            return Err(format!("schema count exceeds {MAX_LIST_ENTRIES}"));
        }
        let name =
            leaf_string(child(s, "name")).ok_or_else(|| "schema entry missing name".to_string())?;
        let mut module_sets_ref = Vec::new();
        match child(s, "module-set") {
            Some(Value::Array(items)) => {
                for item in items {
                    if let Some(n) = leaf_string(Some(item)) {
                        module_sets_ref.push(n);
                    }
                }
            }
            Some(Value::String(n)) if !n.is_empty() => module_sets_ref.push(n.clone()),
            _ => {}
        }
        schemas.push(YangLibrarySchema {
            name,
            module_sets: module_sets_ref,
        });
    }

    let mut datastores = Vec::new();
    for d in list_entries(child(container, "datastore"), "datastore")? {
        if datastores.len() >= MAX_LIST_ENTRIES {
            return Err(format!("datastore count exceeds {MAX_LIST_ENTRIES}"));
        }
        let name = leaf_string(child(d, "name"))
            .ok_or_else(|| "datastore entry missing name".to_string())?;
        let schema = leaf_string(child(d, "schema"))
            .ok_or_else(|| format!("datastore '{name}' missing schema reference"))?;
        datastores.push(YangLibraryDatastore { name, schema });
    }

    if module_sets.is_empty() {
        return Err("yang-library contains no module-set".to_string());
    }

    // Conflicting module definitions are evaluated at the correct
    // standards scope: a schema's effective module set is the UNION of
    // the module-sets it references (RFC 8525). Different datastore
    // schemas may legitimately pin different revisions of the same
    // module — that is not a conflict. A conflict exists only when
    // module-sets combined into ONE schema disagree on a module's
    // revision; unreferenced module-sets are inert. Dangling
    // module-set references are left for the resolver
    // (`DatastoreSchemaMissing`), not rejected here.
    //
    // Only IMPLEMENTED modules participate: RFC 8525 permits a
    // module-set to carry multiple import-only revisions of the same
    // module name (different modules legitimately import different
    // revisions), so import-only entries can never conflict — neither
    // with each other nor with the implemented module of the same
    // name. Implemented identity is (name, revision, namespace):
    // disagreeing revisions OR disagreeing reported namespaces inside
    // one effective schema are real conflicts.
    for schema in &schemas {
        let mut seen: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
        for ms_name in &schema.module_sets {
            let Some(ms) = module_sets.iter().find(|m| &m.name == ms_name) else {
                continue;
            };
            for m in &ms.modules {
                match seen.get(&m.name) {
                    Some((prev_rev, prev_ns))
                        if *prev_rev != m.revision
                            || (prev_ns.is_some()
                                && m.namespace.is_some()
                                && prev_ns != &m.namespace) =>
                    {
                        return Err(format!(
                            "conflicting module definitions for '{}' in schema '{}'",
                            m.name, schema.name
                        ));
                    }
                    _ => {
                        seen.insert(m.name.clone(), (m.revision.clone(), m.namespace.clone()));
                    }
                }
            }
        }
    }

    Ok(YangLibraryEvidence {
        tree: YangLibraryTree::Nmda,
        content_id,
        module_sets,
        schemas,
        datastores,
        origin: "ietf".to_string(),
        path: "/yang-library".to_string(),
        fetched_at_ns,
        diagnostics,
    })
}

/// Parse a legacy RFC 7895 `/modules-state` container into normalized
/// evidence. The legacy tree reports conformance-type per module but
/// has NO datastore→schema association.
pub fn parse_legacy_library(
    tree: &Value,
    fetched_at_ns: i64,
) -> Result<YangLibraryEvidence, String> {
    let container = library_container(tree, &["modules-state", "modules_state"])
        .ok_or_else(|| "no modules-state container in response".to_string())?;

    let module_set_id = leaf_string(child(container, "module-set-id"))
        .or_else(|| leaf_string(child(container, "module_set_id")));
    let mut diagnostics = Vec::new();
    if module_set_id.is_none() {
        diagnostics.push("module-set-id absent".to_string());
    }

    let mut modules = Vec::new();
    let mut import_only = Vec::new();
    let mut counts = ModuleCounts::default();
    for m in list_entries(child(container, "module"), "module")? {
        counts.modules += 1;
        if counts.modules > MAX_MODULES {
            return Err(format!("module count exceeds {MAX_MODULES}"));
        }
        let conformance = leaf_string(child(m, "conformance-type"))
            .or_else(|| leaf_string(child(m, "conformance_type")));
        let parsed = parse_module_entry(m, conformance.as_deref() == Some("import"), &mut counts)?;
        if conformance.as_deref() == Some("import") {
            import_only.push(parsed);
        } else {
            modules.push(parsed);
        }
    }

    if modules.is_empty() && import_only.is_empty() {
        return Err("modules-state contains no modules".to_string());
    }

    let set_name = module_set_id
        .clone()
        .unwrap_or_else(|| "modules-state".to_string());
    Ok(YangLibraryEvidence {
        tree: YangLibraryTree::Legacy,
        content_id: module_set_id,
        module_sets: vec![YangLibraryModuleSet {
            name: set_name,
            modules,
            import_only_modules: import_only,
        }],
        schemas: Vec::new(),
        datastores: Vec::new(),
        origin: "ietf".to_string(),
        path: "/modules-state".to_string(),
        fetched_at_ns,
        diagnostics,
    })
}
