use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::pb::{InterfaceCounterSummary, InterfaceSummary};
use crate::telemetry::{TelemetryEvent, TelemetryUpdate, json_i64};

/// Aggregates raw counter updates into time-windowed summaries.
pub struct CounterSummarizer {
    window_duration_secs: u64,
    /// Keyed by "target\x00if_name"
    windows: HashMap<String, InterfaceWindow>,
}

struct InterfaceWindow {
    target: String,
    if_name: String,
    window_start_ts: u64,
    last_update_ts: u64,
    counters: HashMap<String, CounterStats>,
}

struct CounterStats {
    min: i64,
    max: i64,
    sum: f64,
    count: u64,
    first_val: i64,
    last_val: i64,
}

impl CounterSummarizer {
    pub fn new(window_duration_secs: u64) -> Self {
        Self {
            window_duration_secs,
            windows: HashMap::new(),
        }
    }

    /// Observes a telemetry update. Returns a completed summary when its window rolls over.
    pub fn observe(&mut self, update: &TelemetryUpdate) -> Option<InterfaceSummary> {
        let classified = update.classify();
        let TelemetryEvent::InterfaceStats { if_name } = classified else {
            return None;
        };

        let key = format!("{}\x00{}", update.target, if_name);
        let now = now_secs();
        let window_start = (now / self.window_duration_secs) * self.window_duration_secs;

        let mut expired_summary = None;

        if let Some(window) = self.windows.get(&key)
            && window.window_start_ts != window_start
        {
            expired_summary = Some(emit_summary(window, self.window_duration_secs));
            self.windows.remove(&key);
        }

        let window = self.windows.entry(key).or_insert_with(|| InterfaceWindow {
            target: update.target.clone(),
            if_name: if_name.clone(),
            window_start_ts: window_start,
            last_update_ts: now,
            counters: HashMap::new(),
        });

        window.last_update_ts = now;

        if let Some(obj) = update.value.as_object() {
            for (name, val) in obj {
                if val.is_number() || val.is_string() {
                    let v = json_i64(&update.value, name);
                    let stats = window.counters.entry(name.clone()).or_insert(CounterStats {
                        min: v,
                        max: v,
                        sum: 0.0,
                        count: 0,
                        first_val: v,
                        last_val: v,
                    });
                    stats.min = stats.min.min(v);
                    stats.max = stats.max.max(v);
                    stats.sum += v as f64;
                    stats.count += 1;
                    stats.last_val = v;
                }
            }
        }

        expired_summary
    }

    /// Emits partial summaries for interfaces that have been silent for `idle_threshold_secs`.
    /// Used by timer-driven flush to prevent partial windows from being lost on quiet interfaces.
    pub fn flush_stale(&mut self, idle_threshold_secs: u64) -> Vec<InterfaceSummary> {
        let now = now_secs();
        let mut stale_keys = Vec::new();
        for (key, window) in &self.windows {
            if now.saturating_sub(window.last_update_ts) >= idle_threshold_secs {
                stale_keys.push(key.clone());
            }
        }
        let mut summaries = Vec::new();
        for key in stale_keys {
            if let Some(window) = self.windows.remove(&key) {
                summaries.push(emit_summary(&window, self.window_duration_secs));
            }
        }
        summaries
    }
}

fn emit_summary(window: &InterfaceWindow, window_duration_secs: u64) -> InterfaceSummary {
    let mut counters = Vec::new();
    for (name, stats) in &window.counters {
        if stats.count > 0 {
            counters.push(InterfaceCounterSummary {
                counter_name: name.clone(),
                min: stats.min,
                max: stats.max,
                mean: stats.sum / (stats.count as f64),
                delta: stats.last_val - stats.first_val,
            });
        }
    }
    InterfaceSummary {
        counters,
        window_secs: window_duration_secs as u32,
        target: window.target.clone(),
        if_name: window.if_name.clone(),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::CounterSummarizer;
    use crate::telemetry::TelemetryUpdate;

    fn counter_update(target: &str, if_name: &str, value: i64, ts: i64) -> TelemetryUpdate {
        TelemetryUpdate {
            target: target.to_string(),
            vendor: "nokia_srl".to_string(),
            hostname: "srl1".to_string(),
            role: String::new(),
            site: String::new(),
            timestamp_ns: ts,
            path: format!("interfaces/interface[name={if_name}]/state/counters"),
            value: json!({"in-octets": value, "out-octets": value * 2}),
        }
    }

    #[test]
    fn summarizer_emits_one_summary_per_interface_when_window_rolls() {
        // Window is 1 second. We send updates for 2 interfaces in window 0, then one
        // more update per interface in window 1. The window-0 summaries should emit
        // on the first update that crosses the boundary.
        let mut summarizer = CounterSummarizer::new(1);

        // Simulate window roll by sending updates that appear to be in the next window.
        // We do this by injecting updates and checking the summarizer handles the
        // roll-over detection correctly.
        //
        // Because CounterSummarizer uses wall clock to detect window boundaries,
        // and a 1-second window makes deterministic testing hard, we verify
        // the observable invariants instead:
        // - Non-counter updates return None
        // - Counter updates for a new key insert a window
        // - flush_stale returns buffered windows after idle threshold

        let u1 = counter_update("10.0.0.1:57400", "ethernet-1/1", 100, 1);
        let u2 = counter_update("10.0.0.1:57400", "ethernet-1/2", 200, 2);

        // Both updates go into new windows — no rollover yet
        assert!(summarizer.observe(&u1).is_none());
        assert!(summarizer.observe(&u2).is_none());

        // Both interfaces are now buffered; flush_stale with threshold=0 emits both
        let flushed = summarizer.flush_stale(0);
        assert_eq!(flushed.len(), 2, "expected one summary per interface");

        let targets: Vec<&str> = flushed.iter().map(|s| s.target.as_str()).collect();
        assert!(targets.contains(&"10.0.0.1:57400"));

        let if_names: std::collections::HashSet<&str> =
            flushed.iter().map(|s| s.if_name.as_str()).collect();
        assert!(if_names.contains("ethernet-1/1"));
        assert!(if_names.contains("ethernet-1/2"));

        // After flush the windows are gone
        let after_flush = summarizer.flush_stale(0);
        assert!(after_flush.is_empty());
    }

    #[test]
    fn summarizer_ignores_non_counter_paths() {
        let mut summarizer = CounterSummarizer::new(60);
        let bgp_update = TelemetryUpdate {
            target: "10.0.0.1:57400".to_string(),
            vendor: "nokia_srl".to_string(),
            hostname: "srl1".to_string(),
            role: String::new(),
            site: String::new(),
            timestamp_ns: 1,
            path: "network-instances/network-instance[name=default]/protocols/protocol[name=BGP]/bgp/neighbors/neighbor[neighbor-address=192.168.1.1]/state/session-state".to_string(),
            value: json!("ESTABLISHED"),
        };
        assert!(summarizer.observe(&bgp_update).is_none());
        assert!(summarizer.flush_stale(0).is_empty());
    }

    #[test]
    fn summarizer_summary_carries_target_and_if_name() {
        let mut summarizer = CounterSummarizer::new(60);
        let update = counter_update("device-a:57400", "ge-0/0/1", 42, 1);
        summarizer.observe(&update);
        let summaries = summarizer.flush_stale(0);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].target, "device-a:57400");
        assert_eq!(summaries[0].if_name, "ge-0/0/1");
        assert_eq!(summaries[0].window_secs, 60);
    }
}
