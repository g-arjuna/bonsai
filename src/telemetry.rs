use serde_json::Value as JsonValue;

/// A single decoded gNMI update forwarded from any subscriber task.
#[derive(Debug, Clone)]
pub struct TelemetryUpdate {
    pub target: String,
    pub vendor: String,
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
                    return TelemetryEvent::BgpNeighborState { peer_address: addr };
                }
            }
        }

        // system/lldp/interface[name=X]/neighbor[id=Y]
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

        // ── OpenConfig paths (XRd, cRPD) ──────────────────────────────────────
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
                    return TelemetryEvent::BgpNeighborState { peer_address: addr };
                }
            }
        }

        TelemetryEvent::Ignored
    }
}

fn extract_bracketed(path: &str, prefix: &str) -> Option<String> {
    let start = path.find(prefix)? + prefix.len();
    let rest = &path[start..];
    let end = rest.find(']')?;
    Some(rest[..end].to_string())
}

/// Extract an i64 trying each key in order; first present key wins.
/// SR Linux sends counter values as quoted strings ("in-packets": "646").
pub fn json_i64(obj: &JsonValue, key: &str) -> i64 {
    match obj.get(key) {
        Some(JsonValue::Number(n)) => n.as_i64().unwrap_or(0),
        Some(JsonValue::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

/// Like json_i64 but tries multiple key names — handles SRL native vs OpenConfig naming.
pub fn json_i64_multi(obj: &JsonValue, keys: &[&str]) -> i64 {
    for key in keys {
        if obj.get(key).is_some() {
            return json_i64(obj, key);
        }
    }
    0
}

pub fn json_str<'a>(obj: &'a JsonValue, key: &str) -> &'a str {
    obj.get(key).and_then(|v| v.as_str()).unwrap_or("")
}
