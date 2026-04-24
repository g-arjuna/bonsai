use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::pb::{InterfaceCounterSummary, InterfaceSummary};
use crate::telemetry::{TelemetryEvent, TelemetryUpdate, json_i64};

/// Aggregates raw counter updates into time-windowed summaries.
pub struct CounterSummarizer {
    window_duration_secs: u64,
    /// Keyed by "target:if_name"
    windows: HashMap<String, InterfaceWindow>,
}

struct InterfaceWindow {
    window_start_ts: u64,
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

    /// Observes a telemetry update. If a window has completed for an interface,
    /// returns a summary to be forwarded.
    pub fn observe(&mut self, update: &TelemetryUpdate) -> Option<InterfaceSummary> {
        let classified = update.classify();
        let TelemetryEvent::InterfaceStats { if_name } = classified else {
            return None;
        };

        let key = format!("{}:{}", update.target, if_name);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let window_start = (now / self.window_duration_secs) * self.window_duration_secs;

        let mut expired_summary = None;

        if let Some(window) = self.windows.get(&key) {
            if window.window_start_ts != window_start {
                expired_summary = Some(self.emit_summary(window));
                self.windows.remove(&key);
            }
        }

        let window = self.windows.entry(key).or_insert_with(|| InterfaceWindow {
            window_start_ts: window_start,
            counters: HashMap::new(),
        });

        if let Some(obj) = update.value.as_object() {
            for (name, val) in obj {
                // Heuristic: only summarize numeric-looking fields
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

    fn emit_summary(&self, window: &InterfaceWindow) -> InterfaceSummary {
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
            window_secs: self.window_duration_secs as u32,
        }
    }
}
