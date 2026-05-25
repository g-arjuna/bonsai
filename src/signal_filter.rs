/// DS-5: Per-device / per-site / per-role signal feature gates.
///
/// `SignalFilter` wraps an in-memory snapshot of `SignalPolicy` DB rows and
/// answers a single question at write-time:
///   "should this (device_address, site, role, signal_type) combination be
///    processed, or silently dropped?"
///
/// Scope precedence (most-specific wins):
///   device  >  role  >  site  >  (default allow)
///
/// The filter is refreshed from the DB every `REFRESH_INTERVAL_SECS` seconds
/// via `refresh()`.  It is safe to clone — the inner `Arc<Snapshot>` makes
/// clones free.
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use lbug::{Connection, Value};
use tracing::warn;

pub const REFRESH_INTERVAL_SECS: u64 = 30;

/// All valid signal type tokens.  Must match the values stored in DB and
/// accepted by the HTTP API.
pub const SIGNAL_TYPES: &[&str] = &[
    "gnmi", "syslog", "snmp", "bmp", "netflow", "sflow", "bgp_ls", "otlp",
];

/// Map from (scope_type, scope_value, signal_type) → enabled.
type PolicyMap = HashMap<(String, String, String), bool>;

#[derive(Default)]
struct Snapshot {
    policies: PolicyMap,
    loaded_at: Option<Instant>,
}

/// Classifies a `TelemetryUpdate.path` into a signal type token.
/// Returns `None` for gNMI paths (they are handled by subscriber enable/disable).
pub fn signal_type_for_path(path: &str) -> &'static str {
    if path.starts_with("streaming/sflow/") {
        "sflow"
    } else if path.starts_with("streaming/netflow/") {
        "netflow"
    } else if path.starts_with("streaming/bmp/") {
        "bmp"
    } else if path.starts_with("streaming/bgp-ls/") {
        "bgp_ls"
    } else if path.starts_with("streaming/otlp/") {
        "otlp"
    } else if path.starts_with("signals/syslog/") || path.starts_with("signals/syslog_fact/") {
        "syslog"
    } else if path.starts_with("signals/snmp/") || path.starts_with("signals/snmp_fact/") {
        "snmp"
    } else {
        "gnmi"
    }
}

#[derive(Clone)]
pub struct SignalFilter {
    inner: Arc<RwLock<Snapshot>>,
}

impl Default for SignalFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl SignalFilter {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Snapshot::default())),
        }
    }

    /// Load all SignalPolicy rows from the DB into the in-memory snapshot.
    /// Called at startup and periodically.
    pub fn refresh(&self, conn: &Connection<'_>) {
        let mut stmt = match conn.prepare(
            "MATCH (p:SignalPolicy) \
             RETURN p.scope_type, p.scope_value, p.signal_type, p.enabled",
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "SignalFilter: prepare refresh failed");
                return;
            }
        };
        let mut rows = match conn.execute(&mut stmt, vec![]) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "SignalFilter: query refresh failed");
                return;
            }
        };
        let mut map = PolicyMap::new();
        while let Some(row) = rows.next() {
            let scope_type = match &row[0] {
                Value::String(s) => s.clone(),
                _ => continue,
            };
            let scope_value = match &row[1] {
                Value::String(s) => s.clone(),
                _ => continue,
            };
            let signal_type = match &row[2] {
                Value::String(s) => s.clone(),
                _ => continue,
            };
            let enabled = match &row[3] {
                Value::Bool(b) => *b,
                _ => true,
            };
            map.insert((scope_type, scope_value, signal_type), enabled);
        }
        if let Ok(mut snap) = self.inner.write() {
            snap.policies = map;
            snap.loaded_at = Some(Instant::now());
        }
    }

    /// Returns `true` if the update should be processed, `false` if it should
    /// be silently dropped.
    ///
    /// Scope precedence: device > role > site > allow.
    pub fn is_allowed(
        &self,
        device_address: &str,
        site: &str,
        role: &str,
        signal_type: &str,
    ) -> bool {
        let snap = match self.inner.read() {
            Ok(s) => s,
            Err(_) => return true,
        };

        let check = |scope_type: &str, scope_value: &str| -> Option<bool> {
            if scope_value.is_empty() {
                return None;
            }
            snap.policies
                .get(&(
                    scope_type.to_string(),
                    scope_value.to_string(),
                    signal_type.to_string(),
                ))
                .copied()
        };

        if let Some(v) = check("device", device_address) {
            return v;
        }
        if let Some(v) = check("role", role) {
            return v;
        }
        if let Some(v) = check("site", site) {
            return v;
        }
        true
    }

    /// Returns true if a refresh is due.
    pub fn needs_refresh(&self) -> bool {
        match self.inner.read() {
            Ok(s) => s
                .loaded_at
                .map(|t| t.elapsed() > Duration::from_secs(REFRESH_INTERVAL_SECS))
                .unwrap_or(true),
            Err(_) => true,
        }
    }
}
