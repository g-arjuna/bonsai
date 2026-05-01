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
    /// Failures within the past 30 days (not auto-decremented; reset on state change).
    pub failure_count_30d: u32,
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
        if let Ok(json) = serde_json::to_string_pretty(&self.records)
            && let Err(e) = std::fs::write(&self.path, &json)
        {
            warn!("failed to persist trust_state.json: {e}");
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
        self.persist();
    }

    pub fn record_failure(&mut self, key: &TrustKey, ts_ns: i64) {
        let r = self.records.entry(key.to_storage_key()).or_default();
        r.consecutive_successes = 0;
        r.failure_count_30d += 1;
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
            _ => &String::new(),
        };
        TrustState::parse_state(s)
    }
}

pub fn new_trust_store(runtime_dir: &Path, config: RemediationConfig) -> SharedTrustStore {
    Arc::new(RwLock::new(TrustStore::load(runtime_dir, config)))
}
