use serde_json::Value as JsonValue;

/// A single decoded gNMI update forwarded from any subscriber task.
#[derive(Debug, Clone)]
pub struct TelemetryUpdate {
    pub target: String,
    pub vendor: String,
    /// Configured hostname for this device (e.g. "srl1"). Empty when not configured.
    pub hostname: String,
    pub role: String,
    pub site: String,
    pub timestamp_ns: i64,
    pub path: String,
    pub value: JsonValue,
}

/// Classified form of a TelemetryUpdate after path parsing.
pub enum TelemetryEvent {
    InterfaceStats {
        if_name: String,
    },
    InterfaceSummary {
        if_name: String,
    },
    BfdSessionState {
        if_name: String,
        local_discriminator: String,
        /// Pre-extracted state object for blob-style updates.
        /// When None, callers read fields directly from `TelemetryUpdate::value`.
        state_value: Option<serde_json::Value>,
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
        /// Pre-normalized `{"chassis-id", "system-name", "port-id"}` for vendors
        /// whose native format differs from the flat-field shape expected by graph.rs.
        state_value: Option<serde_json::Value>,
    },
    /// Interface operational status change (up/down). Emitted as a BonsaiEvent;
    /// the Interface node itself is not updated (oper-status is not a counter).
    InterfaceOperStatus {
        if_name: String,
        oper_status: String,
    },
    Ignored,
}

impl TelemetryUpdate {
    pub fn classify(&self) -> TelemetryEvent {
        // ── Summarized paths (Collector summary mode) ───────────────────────
        if self.path.ends_with("/summary") {
            if let Some(name) = extract_bracketed(&self.path, "interface[name=") {
                return TelemetryEvent::InterfaceSummary { if_name: name };
            }
            if let Some(name) = extract_bracketed(&self.path, "interfaces/interface[name=") {
                return TelemetryEvent::InterfaceSummary { if_name: name };
            }
        }

        // ── SR Linux native paths ──────────────────────────────────────────────
        // interface[name=X]/statistics
        if self.path.contains("interface[name=")
            && self.path.ends_with("/statistics")
            && let Some(name) = extract_bracketed(&self.path, "interface[name=")
        {
            return TelemetryEvent::InterfaceStats { if_name: name };
        }

        // network-instance[name=default]/protocols/bgp/neighbor[peer-address=X]
        if self.path.contains("bgp/neighbor[peer-address=")
            && self.path.ends_with(']')
            && let Some(addr) = extract_bracketed(&self.path, "bgp/neighbor[peer-address=")
            && self.value.get("session-state").is_some()
        {
            return TelemetryEvent::BgpNeighborState {
                peer_address: addr,
                state_value: None,
            };
        }

        // SRL native: system/lldp/interface[name=X]/neighbor[id=Y]
        if self.path.contains("lldp/interface[name=")
            && self.path.contains("/neighbor[id=")
            && self.path.ends_with(']')
            && self.value.get("chassis-id").is_some()
            && let (Some(local_if), Some(neighbor_id)) = (
                extract_bracketed(&self.path, "lldp/interface[name="),
                extract_bracketed(&self.path, "neighbor[id="),
            )
        {
            return TelemetryEvent::LldpNeighbor {
                local_if,
                neighbor_id,
                state_value: None,
            };
        }

        // OC LLDP (cEOS): lldp/interfaces/interface[name=X]/neighbors/neighbor[id=Y]/state
        // cEOS sends chassis-id and system-name/port-id in SEPARATE notifications,
        // so trigger on any useful LLDP field, not just chassis-id.
        if self.path.contains("lldp/interfaces/interface[name=")
            && self.path.contains("/neighbors/neighbor[id=")
            && (self.path.ends_with("/state") || self.path.ends_with(']'))
            && (json_find(&self.value, "chassis-id").is_some()
                || json_find(&self.value, "system-name").is_some()
                || json_find(&self.value, "port-id").is_some())
            && let (Some(local_if), Some(neighbor_id)) = (
                extract_bracketed(&self.path, "lldp/interfaces/interface[name="),
                extract_bracketed(&self.path, "neighbor[id="),
            )
        {
            return TelemetryEvent::LldpNeighbor {
                local_if,
                neighbor_id,
                state_value: None,
            };
        }

        // ── OpenConfig paths (XRd OC, cEOS, cRPD BGP) ────────────────────────
        // interfaces/interface[name=X]/state/counters
        if self.path.contains("interfaces/interface[name=")
            && self.path.ends_with("/state/counters")
            && let Some(name) = extract_bracketed(&self.path, "interfaces/interface[name=")
        {
            return TelemetryEvent::InterfaceStats { if_name: name };
        }

        // SRL native BFD: bfd/network-instance[name=X]/peer[local-discriminator=Y]
        // Peer sessions live under network-instance, not subinterface (subinterface holds config only).
        if self.path.contains("bfd/network-instance[name=")
            && self.path.contains("/peer[local-discriminator=")
            && (self.path.ends_with(']') || self.path.ends_with("/state"))
            && json_find(&self.value, "session-state").is_some()
            && let (Some(if_name), Some(local_discriminator)) = (
                extract_bracketed(&self.path, "network-instance[name="),
                extract_bracketed(&self.path, "peer[local-discriminator="),
            )
        {
            return TelemetryEvent::BfdSessionState {
                if_name,
                local_discriminator,
                state_value: None,
            };
        }

        // OpenConfig BFD: bfd/interfaces/interface[id=X]/peers/peer[local-discriminator=Y]/state
        if self.path.contains("bfd/interfaces/interface[id=")
            && self.path.contains("/peers/peer[local-discriminator=")
            && (self.path.ends_with("/state") || self.path.ends_with(']'))
            && json_find(&self.value, "session-state").is_some()
            && let (Some(if_name), Some(local_discriminator)) = (
                extract_bracketed(&self.path, "interface[id="),
                extract_bracketed(&self.path, "peer[local-discriminator="),
            )
        {
            return TelemetryEvent::BfdSessionState {
                if_name,
                local_discriminator,
                state_value: None,
            };
        }

        // .../bgp/neighbors/neighbor[neighbor-address=X] or .../neighbor[neighbor-address=X]/state
        if self.path.contains("neighbors/neighbor[neighbor-address=")
            && (self.path.ends_with(']') || self.path.ends_with("/state"))
            && self.value.get("session-state").is_some()
            && let Some(addr) = extract_bracketed(&self.path, "neighbor[neighbor-address=")
        {
            return TelemetryEvent::BgpNeighborState {
                peer_address: addr,
                state_value: None,
            };
        }

        // XRd ON_CHANGE BGP: path="network-instances", value is a partial OC tree blob.
        // Each notification covers exactly one neighbor. Navigate the nested JSON to extract
        // neighbor-address and state without requiring a specific sub-path.
        if self.path == "network-instances"
            && let Some(event) = walk_xrd_bgp_blob(&self.value)
        {
            return event;
        }

        // ── Cisco IOS-XR native (infra-statsd-oper generic-counters) ──────────
        // XRd drops the Cisco-IOS-XR-infra-statsd-oper: module prefix in responses,
        // so match on the key/tail patterns only. Key is `interface-name` (not `name`).
        if self.path.contains("interface[interface-name=")
            && self.path.ends_with("/generic-counters")
            && let Some(name) = extract_bracketed(&self.path, "interface[interface-name=")
        {
            return TelemetryEvent::InterfaceStats { if_name: name };
        }

        // ── Cisco IOS-XR native LLDP (ethernet-lldp-oper detail) ────────────────
        // path: lldp/nodes/node[node-name=X]/neighbors/details/detail[interface-name=Y][device-id=Z]
        // XRd drops the module prefix; keys: interface-name (local), device-id (neighbor system-name).
        if self.path.contains("lldp/nodes/node[node-name=")
            && self.path.contains("[interface-name=")
            && self.path.contains("[device-id=")
            && self.value.get("lldp-neighbor").is_some()
            && let (Some(local_if), Some(neighbor_id)) = (
                extract_bracketed(&self.path, "interface-name="),
                extract_bracketed(&self.path, "device-id="),
            )
        {
            let state_value = walk_xr_lldp_blob(&self.value);
            return TelemetryEvent::LldpNeighbor {
                local_if,
                neighbor_id,
                state_value,
            };
        }

        // ── SRL native oper-state (ON_CHANGE) ────────────────────────────────
        // Subscribed to interface[name=*]/oper-state (a scalar leaf).
        // The subscriber's leaf-grouping collects it at the parent container:
        //   path  = srl_nokia-interfaces:interface[name=X]   (no /oper-state suffix)
        //   value = {"oper-state": "up"|"down"}
        // Guard on the oper-state key so statistics updates at the same container don't match.
        if self.path.contains("interface[name=")
            && json_find(&self.value, "oper-state").is_some()
            && let Some(name) = extract_bracketed(&self.path, "interface[name=")
        {
            let status = json_str(&self.value, "oper-state").to_string();
            if !status.is_empty() {
                return TelemetryEvent::InterfaceOperStatus {
                    if_name: name,
                    oper_status: status,
                };
            }
        }

        // ── OC oper-status (cEOS leaf update) ────────────────────────────────
        // interfaces/interface[name=X]/state → {"oper-status": "UP"/"DOWN"}
        if self.path.contains("interfaces/interface[name=")
            && (self.path.ends_with("/state") || self.path.ends_with(']'))
            && json_find(&self.value, "oper-status").is_some()
            && let Some(name) = extract_bracketed(&self.path, "interface[name=")
        {
            let status = json_str(&self.value, "oper-status").to_lowercase();
            if !status.is_empty() {
                return TelemetryEvent::InterfaceOperStatus {
                    if_name: name,
                    oper_status: status,
                };
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
    json_find(state, "session-state")?;
    Some(TelemetryEvent::BgpNeighborState {
        peer_address,
        state_value: Some(state.clone()),
    })
}

/// Normalize an XRd `lldp/nodes/.../detail` blob into a flat `{"chassis-id", "system-name", "port-id"}`.
/// XR native: value.lldp-neighbor[0] has chassis-id, port-id-detail, and detail.system-name.
fn walk_xr_lldp_blob(value: &JsonValue) -> Option<JsonValue> {
    let nbr = value.get("lldp-neighbor")?;
    let entry = nbr.get(0).or(Some(nbr))?;
    let chassis_id = json_find(entry, "chassis-id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let port_id = json_find(entry, "port-id-detail")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let system_name = entry
        .get("detail")
        .and_then(|d| json_find(d, "system-name"))
        .and_then(|v| v.as_str())
        .or_else(|| json_find(entry, "system-name").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    Some(serde_json::json!({
        "chassis-id":  chassis_id,
        "system-name": system_name,
        "port-id":     port_id,
    }))
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
    obj.as_object()?
        .iter()
        .find_map(|(k, v)| if k.ends_with(&suffix) { Some(v) } else { None })
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
