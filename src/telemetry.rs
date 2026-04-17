use serde_json::Value as JsonValue;

/// A single decoded gNMI update forwarded from any subscriber task.
#[derive(Debug, Clone)]
pub struct TelemetryUpdate {
    pub target: String,
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
        // SR Linux path: *:interface[name=X]/statistics
        if self.path.contains("interface[name=") && self.path.ends_with("/statistics") {
            if let Some(name) = extract_bracketed(&self.path, "interface[name=") {
                return TelemetryEvent::InterfaceStats { if_name: name };
            }
        }
        // SR Linux path: *.../bgp/neighbor[peer-address=X]  (top-level, ends with ])
        // Sub-paths (e.g. /as-path-options) end with a non-] character.
        if self.path.contains("bgp/neighbor[peer-address=") && self.path.ends_with(']') {
            if let Some(addr) = extract_bracketed(&self.path, "bgp/neighbor[peer-address=") {
                // Only process the notification that actually contains session-state
                if self.value.get("session-state").is_some() {
                    return TelemetryEvent::BgpNeighborState { peer_address: addr };
                }
            }
        }
        // SR Linux path: system/lldp/interface[name=X]/neighbor[id=Y]
        // Only process the top-level notification that carries chassis-id (ignore sub-path updates).
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
        TelemetryEvent::Ignored
    }
}

fn extract_bracketed(path: &str, prefix: &str) -> Option<String> {
    let start = path.find(prefix)? + prefix.len();
    let rest = &path[start..];
    let end = rest.find(']')?;
    Some(rest[..end].to_string())
}

/// Extract an i64 from a JSON object field that may arrive as either a number or a string.
/// SR Linux sends counter values as quoted strings ("in-packets": "646").
pub fn json_i64(obj: &JsonValue, key: &str) -> i64 {
    match obj.get(key) {
        Some(JsonValue::Number(n)) => n.as_i64().unwrap_or(0),
        Some(JsonValue::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

pub fn json_str<'a>(obj: &'a JsonValue, key: &str) -> &'a str {
    obj.get(key).and_then(|v| v.as_str()).unwrap_or("")
}
