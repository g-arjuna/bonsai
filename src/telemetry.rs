use serde_json::Value as JsonValue;

/// A single decoded gNMI update forwarded from any subscriber task.
#[derive(Debug, Clone)]
pub struct TelemetryUpdate {
    pub target: String,
    pub vendor: String,
    /// Configured hostname for this device (e.g. "srl1"). Empty when not configured.
    pub hostname: String,
    pub timestamp_ns: i64,
    pub path: String,
    pub value: JsonValue,
}

/// Classified form of a TelemetryUpdate after path parsing.
pub enum TelemetryEvent {
    InterfaceStats {
        if_name: String,
    },
    BgpNeighborState {
        peer_address: String,
        /// Pre-extracted `state` object for blob-style updates (e.g. XRd network-instances).
        /// When None, callers read fields directly from `TelemetryUpdate::value`.
        state_value: Option<serde_json::Value>,
    },
    LldpNeighbor {
        local_if: String,
        neighbor_id: String,
    },
    Ignored,
}

impl TelemetryUpdate {
    pub fn classify(&self) -> TelemetryEvent {
        // ── SR Linux native paths ──────────────────────────────────────────────
        // interface[name=X]/statistics
        if self.path.contains("interface[name=") && self.path.ends_with("/statistics") {
            if let Some(name) = extract_bracketed(&self.path, "interface[name=") {
                return TelemetryEvent::InterfaceStats { if_name: name };
            }
        }

        // network-instance[name=default]/protocols/bgp/neighbor[peer-address=X]
        if self.path.contains("bgp/neighbor[peer-address=") && self.path.ends_with(']') {
            if let Some(addr) = extract_bracketed(&self.path, "bgp/neighbor[peer-address=") {
                if self.value.get("session-state").is_some() {
                    return TelemetryEvent::BgpNeighborState { peer_address: addr, state_value: None };
                }
            }
        }

        // SRL native: system/lldp/interface[name=X]/neighbor[id=Y]
        if self.path.contains("lldp/interface[name=")
            && self.path.contains("/neighbor[id=")
            && self.path.ends_with(']')
            && self.value.get("chassis-id").is_some()
        {
            if let (Some(local_if), Some(neighbor_id)) = (
                extract_bracketed(&self.path, "lldp/interface[name="),
                extract_bracketed(&self.path, "neighbor[id="),
            ) {
                return TelemetryEvent::LldpNeighbor { local_if, neighbor_id };
            }
        }

        // OC LLDP (cEOS): lldp/interfaces/interface[name=X]/neighbors/neighbor[id=Y]/state
        if self.path.contains("lldp/interfaces/interface[name=")
            && self.path.contains("/neighbors/neighbor[id=")
            && (self.path.ends_with("/state") || self.path.ends_with(']'))
            && json_find(&self.value, "chassis-id").is_some()
        {
            if let (Some(local_if), Some(neighbor_id)) = (
                extract_bracketed(&self.path, "lldp/interfaces/interface[name="),
                extract_bracketed(&self.path, "neighbor[id="),
            ) {
                return TelemetryEvent::LldpNeighbor { local_if, neighbor_id };
            }
        }

        // ── OpenConfig paths (XRd OC, cEOS, cRPD BGP) ────────────────────────
        // interfaces/interface[name=X]/state/counters
        if self.path.contains("interfaces/interface[name=")
            && self.path.ends_with("/state/counters")
        {
            if let Some(name) = extract_bracketed(&self.path, "interfaces/interface[name=") {
                return TelemetryEvent::InterfaceStats { if_name: name };
            }
        }

        // .../bgp/neighbors/neighbor[neighbor-address=X] or .../neighbor[neighbor-address=X]/state
        if self.path.contains("neighbors/neighbor[neighbor-address=") {
            let ends_ok = self.path.ends_with(']') || self.path.ends_with("/state");
            if ends_ok && self.value.get("session-state").is_some() {
                if let Some(addr) =
                    extract_bracketed(&self.path, "neighbor[neighbor-address=")
                {
                    return TelemetryEvent::BgpNeighborState { peer_address: addr, state_value: None };
                }
            }
        }

        // XRd ON_CHANGE BGP: path="network-instances", value is a partial OC tree blob.
        // Each notification covers exactly one neighbor. Navigate the nested JSON to extract
        // neighbor-address and state without requiring a specific sub-path.
        if self.path == "network-instances" {
            if let Some(event) = walk_xrd_bgp_blob(&self.value) {
                return event;
            }
        }

        // ── Cisco IOS-XR native (infra-statsd-oper generic-counters) ──────────
        // XRd drops the Cisco-IOS-XR-infra-statsd-oper: module prefix in responses,
        // so match on the key/tail patterns only. Key is `interface-name` (not `name`).
        if self.path.contains("interface[interface-name=")
            && self.path.ends_with("/generic-counters")
        {
            if let Some(name) = extract_bracketed(&self.path, "interface[interface-name=") {
                return TelemetryEvent::InterfaceStats { if_name: name };
            }
        }

        // ── Junos native interface stats (no origin → junos-state-interfaces) ─
        // interfaces/interface[name=X]/... with Junos field names (input-bytes, output-bytes)
        if self.path.starts_with("interfaces/interface[name=")
            && (self.value.get("input-bytes").is_some()
                || self.value.get("output-bytes").is_some()
                || self.value.get("input-packets").is_some())
        {
            if let Some(name) = extract_bracketed(&self.path, "interface[name=") {
                return TelemetryEvent::InterfaceStats { if_name: name };
            }
        }

        TelemetryEvent::Ignored
    }
}

/// Walk an XRd `network-instances` blob to extract a single BGP neighbor state.
/// XRd sends one partial tree per neighbor: network-instance.protocols.protocol.bgp.neighbors.neighbor.
fn walk_xrd_bgp_blob(value: &JsonValue) -> Option<TelemetryEvent> {
    let neighbor = value
        .get("network-instance")?
        .get("protocols")?
        .get("protocol")?
        .get("bgp")?
        .get("neighbors")?
        .get("neighbor")?;
    let peer_address = neighbor.get("neighbor-address")?.as_str()?.to_string();
    let state = neighbor.get("state")?;
    if json_find(state, "session-state").is_none() {
        return None;
    }
    Some(TelemetryEvent::BgpNeighborState {
        peer_address,
        state_value: Some(state.clone()),
    })
}

fn extract_bracketed(path: &str, prefix: &str) -> Option<String> {
    let start = path.find(prefix)? + prefix.len();
    let rest = &path[start..];
    let end = rest.find(']')?;
    Some(rest[..end].to_string())
}

/// Look up a JSON key, also accepting namespace-prefixed variants (cEOS JSON_IETF
/// returns keys like `"openconfig-interfaces:in-pkts"` instead of `"in-pkts"`).
fn json_find<'a>(obj: &'a JsonValue, key: &str) -> Option<&'a JsonValue> {
    if let Some(v) = obj.get(key) {
        return Some(v);
    }
    let suffix = format!(":{key}");
    obj.as_object()?.iter().find_map(|(k, v)| {
        if k.ends_with(&suffix) { Some(v) } else { None }
    })
}

/// Extract an i64 trying each key in order; first present key wins.
/// SR Linux sends counter values as quoted strings ("in-packets": "646").
pub fn json_i64(obj: &JsonValue, key: &str) -> i64 {
    match json_find(obj, key) {
        Some(JsonValue::Number(n)) => n.as_i64().unwrap_or(0),
        Some(JsonValue::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

/// Like json_i64 but tries multiple key names — handles SRL native vs OpenConfig naming.
pub fn json_i64_multi(obj: &JsonValue, keys: &[&str]) -> i64 {
    for key in keys {
        if json_find(obj, key).is_some() {
            return json_i64(obj, key);
        }
    }
    0
}

pub fn json_str<'a>(obj: &'a JsonValue, key: &str) -> &'a str {
    json_find(obj, key).and_then(|v| v.as_str()).unwrap_or("")
}
