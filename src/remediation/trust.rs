//! TrustState model and per-tuple persistence.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// Ordered persistence messages for the single writer thread. Snapshots
/// are applied strictly in submission order — concurrent writer threads
/// could otherwise complete out of order and resurrect stale state.
enum TrustWrite {
    Snapshot(String),
    Flush(std::sync::mpsc::Sender<()>),
}

/// Bounded hand-off between persist() callers and the single writer
/// thread. Every snapshot is a COMPLETE serialized state, so consecutive
/// snapshots coalesce into the newest one — intermediate states can
/// never be observed on disk anyway. The queue is capped; overflow drops
/// the OLDEST queued snapshot (never a Flush ack) and is counted, since
/// the newest snapshot always carries the full current state.
const TRUST_WRITE_QUEUE_CAP: usize = 64;

struct TrustWriter {
    queue: Mutex<std::collections::VecDeque<TrustWrite>>,
    ready: std::sync::Condvar,
    /// Set when the last TrustStore clone drops so the writer can exit
    /// instead of parking on the condvar forever.
    shutdown: std::sync::atomic::AtomicBool,
}

impl TrustWriter {
    fn submit(&self, write: TrustWrite) {
        let mut queue = self.queue.lock().unwrap();
        // Coalesce: a queued snapshot is fully superseded by the next
        // one — only the newest pending state needs to hit disk.
        if matches!(write, TrustWrite::Snapshot(_))
            && matches!(queue.back(), Some(TrustWrite::Snapshot(_)))
        {
            *queue.back_mut().unwrap() = write;
            self.ready.notify_one();
            return;
        }
        if queue.len() >= TRUST_WRITE_QUEUE_CAP {
            // Drop the oldest SNAPSHOT; Flush markers are never dropped
            // (their ack senders would be lost). Under sustained overflow
            // a flush may still be dropped once snapshots are exhausted —
            // its bounded recv_timeout then returns, never hangs.
            if let Some(pos) = queue
                .iter()
                .position(|w| matches!(w, TrustWrite::Snapshot(_)))
            {
                queue.remove(pos);
                metrics::counter!("bonsai_trust_write_coalesced_total").increment(1);
            } else {
                queue.pop_front();
            }
        }
        queue.push_back(write);
        self.ready.notify_one();
    }
}

pub struct TrustStore {
    records: HashMap<String, TrustRecord>,
    config: RemediationConfig,
    writer: Arc<TrustWriter>,
}

impl Drop for TrustStore {
    fn drop(&mut self) {
        // Signal the writer to drain and exit once the last store is
        // gone; queued snapshots still land before the thread returns.
        self.writer
            .shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.writer.ready.notify_all();
    }
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
        // Single ordered writer: snapshots are serialized at submit
        // time and written in submission order, so a later state can
        // never be overwritten by an earlier snapshot's delayed write.
        // Writes go through a bounded coalescing queue and land via
        // atomic rename so a crash mid-write can never leave a
        // truncated trust_state.json.
        let writer = Arc::new(TrustWriter {
            queue: Mutex::new(std::collections::VecDeque::new()),
            ready: std::sync::Condvar::new(),
            shutdown: std::sync::atomic::AtomicBool::new(false),
        });
        {
            let writer = Arc::clone(&writer);
            let writer_path = path.clone();
            std::thread::spawn(move || {
                loop {
                    let msg = {
                        let mut queue = writer.queue.lock().unwrap();
                        loop {
                            if let Some(msg) = queue.pop_front() {
                                break msg;
                            }
                            if writer.shutdown.load(std::sync::atomic::Ordering::SeqCst) {
                                // Drain remaining queued writes before
                                // exit so shutdown loses nothing that
                                // was already accepted.
                                if queue.is_empty() {
                                    return;
                                }
                                continue;
                            }
                            queue = writer.ready.wait(queue).unwrap();
                        }
                    };
                    match msg {
                        TrustWrite::Snapshot(json) => {
                            // Atomic replacement: write to a sibling
                            // temp file then rename, so readers never
                            // observe a partial trust_state.json.
                            let tmp = writer_path.with_extension("json.tmp");
                            let result = std::fs::write(&tmp, &json)
                                .and_then(|_| std::fs::rename(&tmp, &writer_path));
                            if let Err(e) = result {
                                tracing::warn!("failed to persist trust_state.json: {e}");
                            }
                        }
                        TrustWrite::Flush(ack) => {
                            let _ = ack.send(());
                        }
                    }
                }
            });
        }
        Self {
            records,
            config,
            writer,
        }
    }

    /// Block until every spawned persistence write has finished. Persistence
    /// is asynchronous by design (Q-10); callers that need durability —
    /// shutdown paths and tests — must flush explicitly instead of racing
    /// the background thread.
    pub fn flush_pending_writes(&self) {
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        self.writer.submit(TrustWrite::Flush(ack_tx));
        // Bounded wait: a dead writer must not hang shutdown/tests.
        let _ = ack_rx.recv_timeout(Duration::from_secs(30));
    }

    fn persist(&self) {
        // Serialize under the lock, then hand the snapshot to the
        // ordered writer thread so disk I/O doesn't block other
        // lock-holders (Q-10) while still guaranteeing write order.
        match serde_json::to_string_pretty(&self.records) {
            Ok(json) => self.writer.submit(TrustWrite::Snapshot(json)),
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
                    rule_id, "unknown environment archetype; defaulting trust state to ApproveEach"
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

    fn config_with_defaults(home_lab: &str, data_center: &str) -> RemediationConfig {
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
        let store = TrustStore::load(
            dir.path(),
            config_with_defaults("auto_silent", "approve_each"),
        );
        assert_eq!(
            store.default_state_for("r", "home_lab"),
            TrustState::AutoSilent
        );
    }

    #[test]
    fn default_state_data_center_maps_to_config() {
        let dir = tmp_dir();
        let store = TrustStore::load(
            dir.path(),
            config_with_defaults("auto_silent", "approve_each"),
        );
        assert_eq!(
            store.default_state_for("r", "data_center"),
            TrustState::ApproveEach
        );
    }

    #[test]
    fn unknown_archetype_returns_approve_each() {
        let dir = tmp_dir();
        let store = TrustStore::load(dir.path(), RemediationConfig::default());
        // Should return ApproveEach (safe default) without panicking
        assert_eq!(
            store.default_state_for("r", "exotic_network_type"),
            TrustState::ApproveEach
        );
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
        assert_eq!(
            after, 0,
            "total_failures should reset after 10 consecutive successes"
        );
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
            // Persistence is asynchronous; join the writer instead of
            // racing it with a fixed sleep (flaky under load).
            store.flush_pending_writes();
        }
        // Re-load from the same dir
        let store2 = TrustStore::load(dir.path(), RemediationConfig::default());
        let k = key("bgp-rule");
        let rec = store2.get(&k).unwrap();
        assert_eq!(rec.state, TrustState::AutoWithNotification);
        assert_eq!(rec.operator_approvals, 1);
    }

    /// Rapid back-to-back persists must land in submission order: the
    /// on-disk snapshot is always the NEWEST state, never an older
    /// delayed write resurrecting stale records (the original race).
    #[test]
    fn rapid_persists_converge_to_newest_state() {
        let dir = tmp_dir();
        {
            let mut store = TrustStore::load(dir.path(), RemediationConfig::default());
            let k = key("ordered-rule");
            for i in 0..50 {
                store.record_approval(&k, 100 + i);
            }
            store.flush_pending_writes();
        }
        let store2 = TrustStore::load(dir.path(), RemediationConfig::default());
        let rec = store2.get(&key("ordered-rule")).unwrap();
        assert_eq!(
            rec.consecutive_successes, 50,
            "every approved mutation must be reflected in the final snapshot"
        );
    }

    /// More persists than the bounded queue capacity: coalescing keeps
    /// only the newest pending snapshot, which is always the complete
    /// current state — the final durable record must still be correct.
    #[test]
    fn queue_overflow_still_converges_to_latest_state() {
        let dir = tmp_dir();
        {
            let mut store = TrustStore::load(dir.path(), RemediationConfig::default());
            let k = key("overflow-rule");
            for i in 0..(TRUST_WRITE_QUEUE_CAP * 4) {
                store.record_approval(&k, 100 + i as i64);
            }
            store.flush_pending_writes();
        }
        let store2 = TrustStore::load(dir.path(), RemediationConfig::default());
        let rec = store2.get(&key("overflow-rule")).unwrap();
        assert_eq!(
            rec.consecutive_successes,
            (TRUST_WRITE_QUEUE_CAP * 4) as u32,
            "coalesced queue must not lose the newest complete snapshot"
        );
    }

    /// A failing writer (unwritable state file) must not hang
    /// flush_pending_writes, panic, or corrupt in-memory state; the
    /// bounded ack still returns and the store remains usable.
    #[test]
    fn failed_persistence_write_is_bounded_and_non_fatal() {
        let dir = tmp_dir();
        {
            // Make the runtime dir unwritable so snapshot+rename fail.
            let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                perms.set_mode(0o500);
            }
            std::fs::set_permissions(dir.path(), perms).unwrap();

            let mut store = TrustStore::load(dir.path(), RemediationConfig::default());
            let k = key("readonly-rule");
            store.record_approval(&k, 1);
            // Bounded: returns via the flush ack even though every
            // snapshot write fails; must not hang for the timeout.
            store.flush_pending_writes();
            // In-memory state is unaffected by the durable failure.
            assert_eq!(store.get(&k).unwrap().operator_approvals, 1);
            drop(store);

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
                perms.set_mode(0o700);
                std::fs::set_permissions(dir.path(), perms).unwrap();
            }
        }
    }
}
