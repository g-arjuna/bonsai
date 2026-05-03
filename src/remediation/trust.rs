//! TrustState model and per-tuple persistence.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::warn;

use crate::config::RemediationConfig;

// ── TrustState enum ────────────────────────────────────────────────────────���──

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustState {
    /// Never executes; proposal created for operator review only.
    SuggestOnly,
    /// Operator must approve every execution.
    #[default]
    ApproveEach,
    /// Executes immediately; N-second rollback window; operator notified.
    AutoWithNotification,
    /// Executes; recorded in audit but not surfaced unless failure.
    AutoSilent,
}

impl TrustState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SuggestOnly => "suggest_only",
            Self::ApproveEach => "approve_each",
            Self::AutoWithNotification => "auto_with_notification",
            Self::AutoSilent => "auto_silent",
        }
    }

    pub fn parse_state(s: &str) -> Self {
        match s {
            "suggest_only" => Self::SuggestOnly,
            "auto_with_notification" => Self::AutoWithNotification,
            "auto_silent" => Self::AutoSilent,
            _ => Self::ApproveEach,
        }
    }

    /// Returns true if this state requires an explicit operator decision.
    pub fn requires_approval(&self) -> bool {
        matches!(self, Self::SuggestOnly | Self::ApproveEach)
    }
}

// ── TrustKey ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrustKey {
    pub rule_id: String,
    pub environment_archetype: String,
    pub site_id: String,
    pub playbook_id: String,
}

impl TrustKey {
    pub fn new(
        rule_id: &str,
        environment_archetype: &str,
        site_id: &str,
        playbook_id: &str,
    ) -> Self {
        Self {
            rule_id: rule_id.to_string(),
            environment_archetype: environment_archetype.to_string(),
            site_id: site_id.to_string(),
            playbook_id: playbook_id.to_string(),
        }
    }

    pub fn to_storage_key(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.rule_id, self.environment_archetype, self.site_id, self.playbook_id
        )
    }
}

// ── TrustRecord ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TrustRecord {
    pub state: TrustState,
    pub consecutive_successes: u32,
    pub last_success_at_ns: i64,
    /// Total failure count since record creation (not time-decayed; resets to 0 after
    /// `consecutive_successes` reaches 10, indicating the tuple has recovered).
    #[serde(alias = "failure_count_30d")] // backwards compat with persisted files from v9
    pub total_failures: u32,
    pub operator_approvals: u32,
    pub operator_rejections: u32,
    pub updated_at_ns: i64,
}

// ── TrustStore ────────────────────────────────────────────────────────────────

const TRUST_STATE_FILE: &str = "trust_state.json";

pub struct TrustStore {
    records: HashMap<String, TrustRecord>,
    path: PathBuf,
    config: RemediationConfig,
}

pub type SharedTrustStore = Arc<RwLock<TrustStore>>;

impl TrustStore {
    pub fn load(runtime_dir: &Path, config: RemediationConfig) -> Self {
        let path = runtime_dir.join(TRUST_STATE_FILE);
        let records: HashMap<String, TrustRecord> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        tracing::info!(count = records.len(), "trust state records loaded");
        Self {
            records,
            path,
            config,
        }
    }

    fn persist(&self) {
        // Serialize under the lock, then write on a background thread so the disk I/O
        // doesn't block other lock-holders (Q-10).
        match serde_json::to_string_pretty(&self.records) {
            Ok(json) => {
                let path = self.path.clone();
                std::thread::spawn(move || {
                    if let Err(e) = std::fs::write(&path, &json) {
                        tracing::warn!("failed to persist trust_state.json: {e}");
                    }
                });
            }
            Err(e) => warn!("failed to serialize trust state: {e}"),
        }
    }

    /// Return the trust record for `key`, creating a default if not present.
    pub fn get_or_default(&mut self, key: &TrustKey) -> TrustRecord {
        let k = key.to_storage_key();
        if !self.records.contains_key(&k) {
            let state = self.default_state_for(&key.rule_id, &key.environment_archetype);
            self.records.insert(
                k.clone(),
                TrustRecord {
                    state,
                    ..Default::default()
                },
            );
        }
        self.records[&k].clone()
    }

    pub fn get(&self, key: &TrustKey) -> Option<&TrustRecord> {
        self.records.get(&key.to_storage_key())
    }

    pub fn list(&self) -> Vec<(String, TrustRecord)> {
        self.records
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn record_approval(&mut self, key: &TrustKey, ts_ns: i64) {
        let r = self.records.entry(key.to_storage_key()).or_default();
        r.operator_approvals += 1;
        r.consecutive_successes += 1;
        r.last_success_at_ns = ts_ns;
        r.updated_at_ns = ts_ns;
        self.persist();
    }

    pub fn record_rejection(&mut self, key: &TrustKey, ts_ns: i64) {
        let r = self.records.entry(key.to_storage_key()).or_default();
        r.operator_rejections += 1;
        r.consecutive_successes = 0;
        r.updated_at_ns = ts_ns;
        self.persist();
    }

    pub fn record_auto_success(&mut self, key: &TrustKey, ts_ns: i64) {
        let r = self.records.entry(key.to_storage_key()).or_default();
        r.consecutive_successes += 1;
        r.last_success_at_ns = ts_ns;
        r.updated_at_ns = ts_ns;
        // After 10 consecutive successes the tuple is considered recovered — reset failure count
        // so graduation logic isn't permanently blocked by old failures.
        if r.consecutive_successes >= 10 {
            r.total_failures = 0;
        }
        self.persist();
    }

    pub fn record_failure(&mut self, key: &TrustKey, ts_ns: i64) {
        let r = self.records.entry(key.to_storage_key()).or_default();
        r.consecutive_successes = 0;
        r.total_failures += 1;
        r.updated_at_ns = ts_ns;
        self.persist();
    }

    /// Operator explicitly sets a new state (graduation or downgrade).
    pub fn set_state(&mut self, key: &TrustKey, state: TrustState, ts_ns: i64) {
        let r = self.records.entry(key.to_storage_key()).or_default();
        r.state = state;
        r.updated_at_ns = ts_ns;
        self.persist();
    }

    fn default_state_for(&self, rule_id: &str, archetype: &str) -> TrustState {
        let defaults = self
            .config
            .rule_defaults
            .get(rule_id)
            .unwrap_or(&self.config.defaults);
        let s = match archetype {
            "home_lab" => &defaults.home_lab,
            "data_center" => &defaults.data_center,
            "service_provider" => &defaults.service_provider,
            "campus_wired" => &defaults.campus_wired,
            "campus_wireless" => &defaults.campus_wireless,
            other => {
                // Unknown archetype: no configured default — falls through to ApproveEach.
                // Operator should add [remediation.defaults.{archetype}] in bonsai.toml.
                warn!(
                    archetype = other,
                    rule_id,
                    "unknown environment archetype; defaulting trust state to ApproveEach"
                );
                &String::new()
            }
        };
        TrustState::parse_state(s)
    }
}

pub fn new_trust_store(runtime_dir: &Path, config: RemediationConfig) -> SharedTrustStore {
    Arc::new(RwLock::new(TrustStore::load(runtime_dir, config)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RemediationConfig, RemediationDefaultsConfig};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_ns() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64
    }

    fn tmp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("create temp dir")
    }

    fn config_with_defaults(
        home_lab: &str,
        data_center: &str,
    ) -> RemediationConfig {
        RemediationConfig {
            defaults: RemediationDefaultsConfig {
                home_lab: home_lab.to_string(),
                data_center: data_center.to_string(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn key(rule: &str) -> TrustKey {
        TrustKey::new(rule, "home_lab", "site1", "pb1")
    }

    // ── default state per archetype ───────────────────────────────────────────

    #[test]
    fn default_state_home_lab_maps_to_config() {
        let dir = tmp_dir();
        let store = TrustStore::load(dir.path(), config_with_defaults("auto_silent", "approve_each"));
        assert_eq!(store.default_state_for("r", "home_lab"), TrustState::AutoSilent);
    }

    #[test]
    fn default_state_data_center_maps_to_config() {
        let dir = tmp_dir();
        let store = TrustStore::load(dir.path(), config_with_defaults("auto_silent", "approve_each"));
        assert_eq!(store.default_state_for("r", "data_center"), TrustState::ApproveEach);
    }

    #[test]
    fn unknown_archetype_returns_approve_each() {
        let dir = tmp_dir();
        let store = TrustStore::load(dir.path(), RemediationConfig::default());
        // Should return ApproveEach (safe default) without panicking
        assert_eq!(store.default_state_for("r", "exotic_network_type"), TrustState::ApproveEach);
    }

    // ── get_or_default creates missing keys ───────────────────────────────────

    #[test]
    fn get_or_default_creates_new_record_with_default_state() {
        let dir = tmp_dir();
        let mut store = TrustStore::load(dir.path(), config_with_defaults("auto_silent", ""));
        let k = TrustKey::new("r", "home_lab", "s", "p");
        let rec = store.get_or_default(&k);
        assert_eq!(rec.state, TrustState::AutoSilent);
        assert_eq!(rec.consecutive_successes, 0);
        assert_eq!(rec.total_failures, 0);
    }

    #[test]
    fn get_or_default_returns_existing_record_unchanged() {
        let dir = tmp_dir();
        let mut store = TrustStore::load(dir.path(), RemediationConfig::default());
        let k = key("bgp-flap");
        store.record_approval(&k, now_ns());
        store.record_approval(&k, now_ns());
        let rec = store.get_or_default(&k);
        assert_eq!(rec.consecutive_successes, 2);
        assert_eq!(rec.operator_approvals, 2);
    }

    // ── record_approval / rejection ───────────────────────────────────────────

    #[test]
    fn approval_increments_consecutive_and_approval_counters() {
        let dir = tmp_dir();
        let mut store = TrustStore::load(dir.path(), RemediationConfig::default());
        let k = key("r");
        store.record_approval(&k, 1);
        store.record_approval(&k, 2);
        let rec = store.get(&k).unwrap().clone();
        assert_eq!(rec.consecutive_successes, 2);
        assert_eq!(rec.operator_approvals, 2);
        assert_eq!(rec.last_success_at_ns, 2);
    }

    #[test]
    fn rejection_resets_consecutive_successes() {
        let dir = tmp_dir();
        let mut store = TrustStore::load(dir.path(), RemediationConfig::default());
        let k = key("r");
        store.record_approval(&k, 1);
        store.record_approval(&k, 2);
        store.record_rejection(&k, 3);
        let rec = store.get(&k).unwrap().clone();
        assert_eq!(rec.consecutive_successes, 0);
        assert_eq!(rec.operator_approvals, 2);
        assert_eq!(rec.operator_rejections, 1);
    }

    // ── Q-9: total_failures resets after 10 consecutive successes ─────────────

    #[test]
    fn total_failures_resets_after_ten_consecutive_auto_successes() {
        let dir = tmp_dir();
        let mut store = TrustStore::load(dir.path(), RemediationConfig::default());
        let k = key("r");
        // 3 failures
        for _ in 0..3 {
            store.record_failure(&k, 1);
        }
        let before = store.get(&k).unwrap().total_failures;
        assert_eq!(before, 3);

        // 10 consecutive auto successes — should trigger reset
        for i in 0..10 {
            store.record_auto_success(&k, i as i64 + 2);
        }
        let after = store.get(&k).unwrap().total_failures;
        assert_eq!(after, 0, "total_failures should reset after 10 consecutive successes");
    }

    #[test]
    fn total_failures_not_reset_before_ten_consecutive() {
        let dir = tmp_dir();
        let mut store = TrustStore::load(dir.path(), RemediationConfig::default());
        let k = key("r");
        store.record_failure(&k, 1);
        for i in 0..9 {
            store.record_auto_success(&k, i as i64 + 2);
        }
        assert_eq!(store.get(&k).unwrap().total_failures, 1);
    }

    // ── set_state ─────────────────────────────────────────────────────────────

    #[test]
    fn set_state_persists_and_updates_timestamp() {
        let dir = tmp_dir();
        let mut store = TrustStore::load(dir.path(), RemediationConfig::default());
        let k = key("r");
        store.set_state(&k, TrustState::AutoWithNotification, 999);
        let rec = store.get(&k).unwrap().clone();
        assert_eq!(rec.state, TrustState::AutoWithNotification);
        assert_eq!(rec.updated_at_ns, 999);
    }

    // ── persistence round-trip ────────────────────────────────────────────────

    #[test]
    fn persistence_round_trip() {
        let dir = tmp_dir();
        {
            let mut store = TrustStore::load(dir.path(), RemediationConfig::default());
            let k = key("bgp-rule");
            store.record_approval(&k, 100);
            store.set_state(&k, TrustState::AutoWithNotification, 200);
            // Background thread writes; give it a moment
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // Re-load from the same dir
        let store2 = TrustStore::load(dir.path(), RemediationConfig::default());
        let k = key("bgp-rule");
        let rec = store2.get(&k).unwrap();
        assert_eq!(rec.state, TrustState::AutoWithNotification);
        assert_eq!(rec.operator_approvals, 1);
    }
}
