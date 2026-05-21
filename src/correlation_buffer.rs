/// Late-arrival multi-source correlation buffer.
///
/// Problem: the same physical event (e.g. a BGP peer going down) produces
/// signals from multiple independent sources — BMP PeerDown, a gNMI
/// session-state path update, and a syslog BGP adjacency message — arriving
/// within a short window (typically 1–30 s) but out of order. Without
/// correlation, each signal fires a separate DetectionEvent in the graph.
///
/// This module provides a `CorrelationBuffer` that:
///   1. Holds the first-arriving signal for a (device, semantic_key) pair
///      open for `window_secs` seconds.
///   2. Absorbs late-arriving signals from other sources into the same slot.
///   3. Emits a single `CorrelatedSignal` containing all source types and
///      all StateChangeEvent IDs when the window expires or is explicitly
///      flushed.
///
/// Callers (e.g. `ingest.rs`) call `record()` for every state-change event
/// and receive back either `Absorbed` (the event was merged into an open slot)
/// or `NewSlot` (a new slot was opened). A background sweep task calls
/// `drain_expired()` periodically to collect slots whose window has closed.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::debug;

/// The semantic event key groups signals that describe the same physical event.
/// Derived from the event_type written to StateChangeEvent nodes.
/// Examples: "bgp_neighbor_down", "interface_down", "bfd_session_down".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CorrelationKey {
    pub device_address: String,
    pub semantic_type: String,
    /// Optional sub-key for disambiguation (e.g. peer address, interface name).
    pub sub_key: String,
}

impl CorrelationKey {
    pub fn new(device_address: impl Into<String>, semantic_type: impl Into<String>, sub_key: impl Into<String>) -> Self {
        Self {
            device_address: device_address.into(),
            semantic_type: semantic_type.into(),
            sub_key: sub_key.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingCorrelation {
    pub key: CorrelationKey,
    pub opened_at: Instant,
    /// All StateChangeEvent IDs that have been absorbed into this slot.
    pub state_change_event_ids: Vec<String>,
    /// All source types (e.g. "gnmi", "bmp", "syslog", "snmp") that contributed.
    pub source_types: Vec<String>,
    /// Timestamp (ns) of the first-arriving signal — used as fired_at for the detection.
    pub first_signal_ns: i64,
    /// JSON detail string from the first-arriving signal for feature context.
    pub detail_json: String,
}

impl PendingCorrelation {
    pub fn new(
        key: CorrelationKey,
        state_change_event_id: String,
        source_type: String,
        first_signal_ns: i64,
        detail_json: String,
    ) -> Self {
        Self {
            key,
            opened_at: Instant::now(),
            state_change_event_ids: vec![state_change_event_id],
            source_types: vec![source_type],
            first_signal_ns,
            detail_json,
        }
    }

    pub fn absorb(&mut self, state_change_event_id: String, source_type: String) {
        if !self.state_change_event_ids.contains(&state_change_event_id) {
            self.state_change_event_ids.push(state_change_event_id);
        }
        if !self.source_types.contains(&source_type) {
            self.source_types.push(source_type);
        }
    }

    /// True when more than one distinct source has contributed.
    pub fn is_multi_source(&self) -> bool {
        self.source_types.len() > 1
    }
}

/// Return value from `CorrelationBuffer::record`.
#[derive(Debug)]
pub enum RecordOutcome {
    /// Signal absorbed into an existing open slot (late arrival matched).
    Absorbed,
    /// A new correlation slot was opened for this key.
    NewSlot,
}

#[derive(Clone)]
pub struct CorrelationBuffer {
    inner: Arc<Mutex<CorrelationBufferInner>>,
    window: Duration,
}

struct CorrelationBufferInner {
    slots: HashMap<CorrelationKey, PendingCorrelation>,
}

impl CorrelationBuffer {
    /// Create a buffer with the given late-arrival window.
    /// `window_secs` of 30–60 s covers typical multi-source clock skew.
    pub fn new(window_secs: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CorrelationBufferInner {
                slots: HashMap::new(),
            })),
            window: Duration::from_secs(window_secs),
        }
    }

    /// Record an incoming signal. Returns whether it was absorbed into an
    /// existing slot or opened a new one.
    pub fn record(
        &self,
        key: CorrelationKey,
        state_change_event_id: String,
        source_type: String,
        signal_ns: i64,
        detail_json: String,
    ) -> RecordOutcome {
        let mut inner = self.inner.lock().expect("correlation buffer lock poisoned");
        if let Some(slot) = inner.slots.get_mut(&key) {
            if slot.opened_at.elapsed() < self.window {
                slot.absorb(state_change_event_id, source_type);
                debug!(
                    device = %slot.key.device_address,
                    semantic = %slot.key.semantic_type,
                    sources = ?slot.source_types,
                    "correlation buffer: late arrival absorbed"
                );
                return RecordOutcome::Absorbed;
            }
            // Window expired — fall through to replace the slot.
            inner.slots.remove(&key);
        }
        let slot = PendingCorrelation::new(key.clone(), state_change_event_id, source_type, signal_ns, detail_json);
        inner.slots.insert(key, slot);
        RecordOutcome::NewSlot
    }

    /// Drain all slots whose window has expired. Returns the flushed correlations.
    /// Call this from a background sweep task every 5–10 s.
    pub fn drain_expired(&self) -> Vec<PendingCorrelation> {
        let mut inner = self.inner.lock().expect("correlation buffer lock poisoned");
        let window = self.window;
        let expired_keys: Vec<CorrelationKey> = inner
            .slots
            .iter()
            .filter(|(_, slot)| slot.opened_at.elapsed() >= window)
            .map(|(k, _)| k.clone())
            .collect();
        let mut flushed = Vec::with_capacity(expired_keys.len());
        for key in expired_keys {
            if let Some(slot) = inner.slots.remove(&key) {
                flushed.push(slot);
            }
        }
        if !flushed.is_empty() {
            metrics::counter!(
                "bonsai_correlation_buffer_flushes_total",
                "multi_source" => if flushed.iter().any(|s| s.is_multi_source()) { "true" } else { "false" }
            )
            .increment(flushed.len() as u64);
        }
        flushed
    }

    /// Force-flush all slots regardless of window expiry. Used on shutdown.
    pub fn drain_all(&self) -> Vec<PendingCorrelation> {
        let mut inner = self.inner.lock().expect("correlation buffer lock poisoned");
        inner.slots.drain().map(|(_, v)| v).collect()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("correlation buffer lock poisoned").slots.len()
    }
}

/// Derive a semantic event type and optional sub-key from a raw `event_type`
/// string as written to `StateChangeEvent.event_type` in the graph.
///
/// The returned `(semantic_type, sub_key)` pair is used as the `CorrelationKey`
/// so that signals from BMP, gNMI, syslog and SNMP that all describe the same
/// physical event are correlated into a single slot.
pub fn semantic_key_for_event(event_type: &str, detail_json: &str) -> Option<(String, String)> {
    // Parse detail for sub-key fields. Best-effort — fall back to empty sub_key.
    let detail: serde_json::Value = serde_json::from_str(detail_json).unwrap_or_default();

    // BGP/BFD detail uses "peer"; syslog/SNMP use "peer_address"/"peer_addr".
    let peer = || {
        detail.get("peer")
            .or_else(|| detail.get("peer_address"))
            .or_else(|| detail.get("peer_addr"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let if_name = || {
        detail.get("interface_name")
            .or_else(|| detail.get("if_name"))
            .or_else(|| detail.get("interface"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let new_state = || {
        detail.get("new_state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase()
    };

    match event_type {
        // gNMI BGP: direction from new_state, sub-keyed by peer
        "bgp_session_change" => {
            let state = new_state();
            if state == "established" {
                Some(("bgp_neighbor_up".to_string(), peer()))
            } else if !state.is_empty() {
                Some(("bgp_neighbor_down".to_string(), peer()))
            } else {
                None
            }
        }
        // BMP / syslog BGP legacy names
        "bgp_session_down" | "bgp_neighbor_down" | "bgp_peer_state"
        | "bgp_peer_backward_transition" | "peer_down" => {
            Some(("bgp_neighbor_down".to_string(), peer()))
        }
        "bgp_session_up" | "bgp_neighbor_up" | "peer_up" => {
            Some(("bgp_neighbor_up".to_string(), peer()))
        }
        // gNMI BFD: direction from new_state
        "bfd_session_change" => {
            let state = new_state();
            if state == "up" {
                Some(("bfd_session_up".to_string(), peer()))
            } else if !state.is_empty() {
                Some(("bfd_session_down".to_string(), peer()))
            } else {
                None
            }
        }
        "bfd_session_down" | "bfd_down" => Some(("bfd_session_down".to_string(), peer())),
        "bfd_session_up" | "bfd_up" => Some(("bfd_session_up".to_string(), peer())),
        // gNMI IS-IS: direction from new_state, sub-keyed by system_id
        "isis_adjacency_change" => {
            let state = new_state();
            let sid = detail.get("system_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if state == "up" {
                Some(("isis_adjacency_up".to_string(), sid))
            } else if !state.is_empty() {
                Some(("isis_adjacency_down".to_string(), sid))
            } else {
                None
            }
        }
        "isis_adj_state_change" | "isis_adjacency_down" => {
            Some(("isis_adjacency_down".to_string(), peer()))
        }
        // BMP peer state — top-level session_state + peer_address
        "bmp_session_change" => {
            let state = detail
                .get("session_state")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let bmp_peer = detail
                .get("peer_address")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if state == "established" {
                Some(("bgp_neighbor_up".to_string(), bmp_peer))
            } else if state == "down" {
                Some(("bgp_neighbor_down".to_string(), bmp_peer))
            } else {
                None
            }
        }
        // SNMP fact events: direction from fact_type, interface from fields.interface_name
        "snmp_fact_joined" | "snmp_fact_orphan" => {
            let fact_type = detail.get("fact_type").and_then(|v| v.as_str()).unwrap_or("");
            let fields = detail.get("fields").cloned().unwrap_or_default();
            let field_str = |k: &str| {
                fields.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
            };
            match fact_type {
                "link_down" => Some(("interface_down".to_string(), field_str("interface_name"))),
                "link_up" => Some(("interface_up".to_string(), field_str("interface_name"))),
                "bgp_peer_state" | "bgp_peer_backward_transition" => {
                    let state = field_str("peer_state");
                    if state == "established" || state == "6" {
                        Some(("bgp_neighbor_up".to_string(), peer()))
                    } else {
                        Some(("bgp_neighbor_down".to_string(), peer()))
                    }
                }
                _ => None,
            }
        }
        // Syslog structured fact events (same event_type pattern as SNMP facts)
        "syslog_fact_joined" | "syslog_fact_orphan" => {
            let fact_type = detail.get("fact_type").and_then(|v| v.as_str()).unwrap_or("");
            let fields = detail.get("fields").cloned().unwrap_or_default();
            let field_str = |k: &str| {
                fields.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
            };
            let new_state = || field_str("new_state").to_lowercase();
            match fact_type {
                "bgp_neighbor" => {
                    let state = new_state();
                    let p = field_str("peer_address");
                    if state == "established" || state == "up" {
                        Some(("bgp_neighbor_up".to_string(), p))
                    } else if !state.is_empty() {
                        Some(("bgp_neighbor_down".to_string(), p))
                    } else {
                        None
                    }
                }
                "interface_state" => {
                    let state = new_state();
                    let iface = field_str("if_name");
                    if state == "up" {
                        Some(("interface_up".to_string(), iface))
                    } else if !state.is_empty() {
                        Some(("interface_down".to_string(), iface))
                    } else {
                        None
                    }
                }
                "isis_adjacency" => {
                    let state = new_state();
                    let nbr = field_str("neighbor_id");
                    if state == "up" {
                        Some(("isis_adjacency_up".to_string(), nbr))
                    } else if !state.is_empty() {
                        Some(("isis_adjacency_down".to_string(), nbr))
                    } else {
                        None
                    }
                }
                "bfd_session" => {
                    let state = new_state();
                    let remote = field_str("remote_address");
                    if state == "up" {
                        Some(("bfd_session_up".to_string(), remote))
                    } else if !state.is_empty() {
                        Some(("bfd_session_down".to_string(), remote))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        // Direct interface events
        "interface_down" | "link_down" => Some(("interface_down".to_string(), if_name())),
        "interface_up" | "link_up" => Some(("interface_up".to_string(), if_name())),
        // OSPF
        "ospf_neighbor_down" | "ospf_nbr_state_change" => {
            Some(("ospf_neighbor_down".to_string(), peer()))
        }
        // Inherently single-source — do not buffer
        _ => None,
    }
}
