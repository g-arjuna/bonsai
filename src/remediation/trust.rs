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
    Snapshot {
        json: String,
        rev: u64,
    },
    Flush {
        min_rev: u64,
        ack: std::sync::mpsc::Sender<FlushOutcome>,
    },
}

/// Result of a `flush_pending_writes` barrier. A flush ack is only
/// `Persisted` when the requested snapshot revision or a newer
/// complete snapshot has actually been renamed into place — failed,
/// dropped or timed-out persistence must never be reported as durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushOutcome {
    /// A snapshot at or above the revision requested by the flush
    /// barrier landed on disk (the newest complete state is durable).
    Persisted,
    /// The newest snapshot write covering the barrier failed and no
    /// newer queued snapshot succeeded; on-disk trust state may be
    /// stale. In-memory records are unaffected.
    WriteFailed,
    /// The writer did not acknowledge within the bounded wait (dead
    /// thread or saturated pending-flush barrier list).
    Timeout,
}

/// Bounded hand-off between persist() callers and the single writer
/// thread. Every snapshot is a COMPLETE serialized state, so consecutive
/// snapshots coalesce into the newest one — intermediate states can
/// never be observed on disk anyway. The queue is capped; overflow drops
/// the OLDEST queued snapshot (never a Flush ack) and is counted, since
/// the newest snapshot always carries the full current state.
const TRUST_WRITE_QUEUE_CAP: usize = 64;

/// Test-only fault injection for the parent-directory durability
/// step, keyed by the exact trust_state.json path so parallel unit
/// tests cannot consume each other's injected failure.
#[cfg(test)]
static DIR_SYNC_FAULT_PATH: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);

/// Outcome of the parent-directory durability step.
///
/// Directory fsync makes the completed rename itself durable. On
/// platforms that support it the step is REQUIRED: `Failed` means the
/// snapshot must not count as persisted. `Unsupported` is reported
/// explicitly where the platform cannot fsync directories, so "not
/// required here" is never confused with "required but failed".
enum DirSync {
    Synced,
    Unsupported,
    Failed(std::io::Error),
}

/// Whether an fsync failure means "this filesystem cannot fsync
/// directories" rather than a genuine I/O error.
#[cfg(unix)]
fn dir_sync_unsupported(e: &std::io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::EOPNOTSUPP)
    )
}

/// fsync the directory containing `path` so a completed rename is
/// durable. Only invoked after the file itself has been fsynced and
/// renamed into place.
fn sync_parent_dir(path: &Path) -> DirSync {
    let Some(dir) = path.parent() else {
        return DirSync::Unsupported;
    };
    #[cfg(not(unix))]
    {
        // Windows cannot open directory handles for fsync; directory
        // durability is not available on this platform.
        let _ = dir;
        DirSync::Unsupported
    }
    #[cfg(unix)]
    {
        // Test-only fault injection, matched on the exact state-file
        // path so parallel unit tests stay isolated.
        #[cfg(test)]
        {
            let faulted = DIR_SYNC_FAULT_PATH
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|p| p == path);
            if faulted {
                return DirSync::Failed(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "injected parent-directory sync failure",
                ));
            }
        }
        match std::fs::File::open(dir) {
            Ok(f) => match f.sync_all() {
                Ok(()) => DirSync::Synced,
                Err(e) if dir_sync_unsupported(&e) => DirSync::Unsupported,
                Err(e) => DirSync::Failed(e),
            },
            Err(e) => DirSync::Failed(e),
        }
    }
}

/// Pending flush barriers awaiting their covering snapshot revision.
/// Bounded: a barrier that cannot be confirmed is dropped and its
/// caller observes `Timeout` — never a silent durable claim.
const TRUST_PENDING_FLUSH_CAP: usize = TRUST_WRITE_QUEUE_CAP;

struct TrustWriterState {
    queue: std::collections::VecDeque<TrustWrite>,
    /// Set under the same mutex as `queue` so the writer can never
    /// miss the shutdown notification (no notify-before-wait race).
    shutdown: bool,
}

struct TrustWriter {
    state: Mutex<TrustWriterState>,
    ready: std::sync::Condvar,
    /// Monotonic snapshot revision counter. Every accepted snapshot
    /// gets a unique increasing `rev`; flush barriers carry the newest
    /// submitted rev so `Persisted` is only reported once a snapshot
    /// with `rev >= min_rev` has actually landed on disk.
    next_rev: std::sync::atomic::AtomicU64,
    /// Join handle for deterministic shutdown drain.
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl TrustWriter {
    fn submit(&self, write: TrustWrite) {
        let mut st = self.state.lock().unwrap();
        // Coalesce: a queued snapshot is fully superseded by the next
        // one — only the newest pending state needs to hit disk. The
        // replacement carries the NEWER revision, so flush barriers
        // below it remain satisfiable.
        if matches!(write, TrustWrite::Snapshot { .. })
            && matches!(st.queue.back(), Some(TrustWrite::Snapshot { .. }))
        {
            *st.queue.back_mut().unwrap() = write;
            self.ready.notify_one();
            return;
        }
        if st.queue.len() >= TRUST_WRITE_QUEUE_CAP {
            // Drop the oldest SNAPSHOT; Flush markers are never dropped
            // (their ack senders would be lost). Under sustained
            // overflow a flush may still be dropped once snapshots are
            // exhausted — its bounded recv_timeout then returns
            // Timeout, never a false success.
            if let Some(pos) = st
                .queue
                .iter()
                .position(|w| matches!(w, TrustWrite::Snapshot { .. }))
            {
                st.queue.remove(pos);
                metrics::counter!("bonsai_trust_write_coalesced_total").increment(1);
            } else {
                st.queue.pop_front();
            }
        }
        st.queue.push_back(write);
        self.ready.notify_one();
    }

    /// Signal shutdown and block until the writer has drained every
    /// accepted write. Called from `TrustStore::drop` so graceful
    /// process shutdown deterministically persists queued snapshots
    /// instead of racing a detached thread.
    fn shutdown(&self) {
        {
            let mut st = self.state.lock().unwrap();
            st.shutdown = true;
        }
        self.ready.notify_all();
        if let Some(handle) = self.thread.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

pub struct TrustStore {
    records: HashMap<String, TrustRecord>,
    config: RemediationConfig,
    writer: Arc<TrustWriter>,
}

impl Drop for TrustStore {
    fn drop(&mut self) {
        // Signal the writer and JOIN it: queued snapshots are drained
        // before this returns, so graceful process shutdown cannot
        // exit while accepted writes are still in flight.
        self.writer.shutdown();
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
            state: Mutex::new(TrustWriterState {
                queue: std::collections::VecDeque::new(),
                shutdown: false,
            }),
            ready: std::sync::Condvar::new(),
            next_rev: std::sync::atomic::AtomicU64::new(0),
            thread: Mutex::new(None),
        });
        {
            let writer_for_thread = Arc::clone(&writer);
            let writer_path = path.clone();
            let handle = std::thread::spawn(move || {
                // Highest snapshot revision durably renamed into place.
                let mut durable_rev: u64 = 0;
                // Outcome of the most recently processed snapshot.
                let mut last_rev: u64 = 0;
                let mut last_ok = true;
                // Flush barriers not yet resolved, oldest first.
                let mut pending_flushes: Vec<(u64, std::sync::mpsc::Sender<FlushOutcome>)> =
                    Vec::new();

                // Resolve every pending barrier whose covering
                // revision has landed, and fail barriers that can no
                // longer be satisfied (covering write failed and no
                // snapshot remains queued to supersede it).
                macro_rules! evaluate_flushes {
                    () => {{
                        let has_queued_snapshot = writer_for_thread
                            .state
                            .lock()
                            .unwrap()
                            .queue
                            .iter()
                            .any(|w| matches!(w, TrustWrite::Snapshot { .. }));
                        let hopeless = !has_queued_snapshot && !last_ok && last_rev > 0;
                        pending_flushes.retain_mut(|(min_rev, ack)| {
                            if durable_rev >= *min_rev {
                                let _ = ack.send(FlushOutcome::Persisted);
                                false
                            } else if hopeless && *min_rev <= last_rev {
                                let _ = ack.send(FlushOutcome::WriteFailed);
                                false
                            } else {
                                true
                            }
                        });
                    }};
                }

                loop {
                    let msg = {
                        let mut st = writer_for_thread.state.lock().unwrap();
                        loop {
                            if let Some(msg) = st.queue.pop_front() {
                                break msg;
                            }
                            if st.shutdown {
                                // Queue drained: fail any barrier that
                                // could not be satisfied, then exit.
                                for (_, ack) in pending_flushes.drain(..) {
                                    let _ = ack.send(FlushOutcome::WriteFailed);
                                }
                                return;
                            }
                            st = writer_for_thread.ready.wait(st).unwrap();
                        }
                    };
                    match msg {
                        TrustWrite::Snapshot { json, rev } => {
                            // Atomic replacement: write to a sibling
                            // temp file, fsync it, rename, then fsync
                            // the parent directory where the platform
                            // supports it, so readers never observe a
                            // partial trust_state.json and a crash
                            // cannot resurrect a torn page-cache write
                            // or lose the newest rename.
                            let tmp = writer_path.with_extension("json.tmp");
                            let result = std::fs::write(&tmp, &json)
                                .and_then(|_| std::fs::File::open(&tmp).and_then(|f| f.sync_all()))
                                .and_then(|_| std::fs::rename(&tmp, &writer_path))
                                .and_then(|_| match sync_parent_dir(&writer_path) {
                                    // Synced and explicitly-unsupported
                                    // platforms both count as durable;
                                    // a genuine failure means the rename
                                    // may not survive a crash, so this
                                    // snapshot is NOT persisted.
                                    DirSync::Synced | DirSync::Unsupported => Ok(()),
                                    DirSync::Failed(e) => Err(e),
                                });
                            match result {
                                Ok(()) => {
                                    durable_rev = rev;
                                    last_rev = rev;
                                    last_ok = true;
                                }
                                Err(e) => {
                                    last_rev = rev;
                                    last_ok = false;
                                    tracing::warn!("failed to persist trust_state.json: {e}");
                                }
                            }
                            evaluate_flushes!();
                        }
                        TrustWrite::Flush { min_rev, ack } => {
                            // Register the barrier, then resolve it
                            // against everything already durable or
                            // still queued — never against a blanket
                            // "last write" flag.
                            if pending_flushes.len() >= TRUST_PENDING_FLUSH_CAP {
                                // Saturated: drop the OLDEST barrier;
                                // its caller observes Timeout.
                                pending_flushes.remove(0);
                                metrics::counter!("bonsai_trust_flush_dropped_total").increment(1);
                            }
                            pending_flushes.push((min_rev, ack));
                            evaluate_flushes!();
                        }
                    }
                }
            });
            *writer.thread.lock().unwrap() = Some(handle);
        }
        Self {
            records,
            config,
            writer,
        }
    }

    /// Block until every spawned persistence write has finished and
    /// report whether the newest snapshot actually reached disk.
    /// Persistence is asynchronous by design (Q-10); callers that need
    /// durability — shutdown paths and tests — must flush explicitly
    /// instead of racing the background thread, and must treat
    /// `WriteFailed`/`Timeout` as NOT durable.
    pub fn flush_pending_writes(&self) -> FlushOutcome {
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        // Barrier revision: the newest snapshot submitted so far.
        // `Persisted` is only possible once a snapshot with
        // `rev >= min_rev` has actually been renamed into place —
        // queue coalescing or overflow can never invalidate that.
        let min_rev = self
            .writer
            .next_rev
            .load(std::sync::atomic::Ordering::SeqCst);
        self.writer.submit(TrustWrite::Flush {
            min_rev,
            ack: ack_tx,
        });
        // Bounded wait: a dead writer must not hang shutdown/tests.
        match ack_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(outcome) => outcome,
            Err(_) => FlushOutcome::Timeout,
        }
    }

    fn persist(&self) {
        // Serialize under the lock, then hand the snapshot to the
        // ordered writer thread so disk I/O doesn't block other
        // lock-holders (Q-10) while still guaranteeing write order.
        match serde_json::to_string_pretty(&self.records) {
            Ok(json) => {
                let rev = self
                    .writer
                    .next_rev
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                self.writer.submit(TrustWrite::Snapshot { json, rev });
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
            assert_eq!(store.flush_pending_writes(), FlushOutcome::Persisted);
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
            assert_eq!(store.flush_pending_writes(), FlushOutcome::Persisted);
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
            assert_eq!(store.flush_pending_writes(), FlushOutcome::Persisted);
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
            // snapshot write fails; must not hang for the timeout —
            // and the ack must report the failure, never a silent
            // durable claim.
            assert_eq!(store.flush_pending_writes(), FlushOutcome::WriteFailed);
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

    /// A failed snapshot followed by a successful one converges:
    /// the flush reports Persisted because the NEWEST complete
    /// snapshot is what reaches disk — intermediate failures that are
    /// superseded by a later write do not poison the ack.
    #[test]
    fn failed_then_successful_write_reports_persisted() {
        let dir = tmp_dir();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
            perms.set_mode(0o500);
            std::fs::set_permissions(dir.path(), perms).unwrap();
        }
        let k = key("transient-rule");
        {
            let mut store = TrustStore::load(dir.path(), RemediationConfig::default());
            store.record_approval(&k, 1);
            assert_eq!(store.flush_pending_writes(), FlushOutcome::WriteFailed);
            drop(store);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(dir.path(), perms).unwrap();
        }
        {
            let mut store = TrustStore::load(dir.path(), RemediationConfig::default());
            store.record_approval(&k, 2);
            assert_eq!(store.flush_pending_writes(), FlushOutcome::Persisted);
        }
        let store2 = TrustStore::load(dir.path(), RemediationConfig::default());
        assert_eq!(store2.get(&k).unwrap().operator_approvals, 1);
    }

    /// Dropping the last store signals shutdown; queued snapshots are
    /// drained before the writer exits, so durability does not depend
    /// on an explicit flush racing the drop.
    #[test]
    fn shutdown_drains_queued_snapshots() {
        let dir = tmp_dir();
        {
            let mut store = TrustStore::load(dir.path(), RemediationConfig::default());
            let k = key("drain-rule");
            store.record_approval(&k, 42);
            // No flush: drop must still let the writer land the
            // queued snapshot before exiting.
        }
        // The writer is detached; poll briefly for the drained file.
        let path = dir.path().join(TRUST_STATE_FILE);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(path.exists(), "shutdown must drain queued snapshots");
        let store2 = TrustStore::load(dir.path(), RemediationConfig::default());
        assert_eq!(
            store2.get(&key("drain-rule")).unwrap().operator_approvals,
            1
        );
    }

    /// Deterministic queue-pressure regression for flush barriers.
    ///
    /// The writer's sibling temp path is replaced by a FIFO, so the
    /// FIRST snapshot write blocks inside `open(2)` until a reader
    /// attaches — a genuinely stalled writer, not a timing assumption.
    /// While stalled we submit more snapshot+flush pairs than the
    /// bounded queue capacity; overflow deterministically drops the
    /// OLDEST queued snapshots, including snapshots that precede
    /// existing Flush markers. After the stall is released every
    /// barrier must resolve on revision evidence — `Persisted` only
    /// because a covering-or-newer snapshot actually landed — and a
    /// deliberately failing phase must yield `WriteFailed`, never a
    /// concealed success.
    #[test]
    #[cfg(unix)]
    fn flush_barriers_survive_stalled_writer_queue_pressure() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tmp_dir();
        // trust_state.json -> with_extension("json.tmp") yields this.
        let tmp_path = dir.path().join("trust_state.json.tmp");
        let c_path = std::ffi::CString::new(tmp_path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

        let k = key("pressure-rule");
        let mut store = TrustStore::load(dir.path(), RemediationConfig::default());
        // rev1: the writer picks this up and stalls on the FIFO open.
        store.record_approval(&k, 1);

        // 40 snapshot+flush pairs = 80 queued items > cap 64, so
        // overflow deterministically drops the oldest snapshots —
        // while staying below the point where flush markers themselves
        // would be dropped (each barrier must still be acknowledged).
        // Each flush carries the newest submitted revision, exactly
        // like flush_pending_writes().
        let mut flush_rxs = Vec::new();
        for _ in 0..(TRUST_WRITE_QUEUE_CAP / 2 + 8) {
            store.record_approval(&k, 2);
            let (tx, rx) = std::sync::mpsc::channel();
            let min_rev = store
                .writer
                .next_rev
                .load(std::sync::atomic::Ordering::SeqCst);
            store.writer.submit(TrustWrite::Flush { min_rev, ack: tx });
            flush_rxs.push(rx);
        }

        // Release the stall: an O_RDWR handle on the FIFO satisfies
        // BOTH directions without blocking, so the writer's
        // open-for-write rendezvous completes AND its later
        // open-for-read (the fsync step) can never deadlock waiting
        // for a writer that has already closed. The first snapshot
        // then fails at fsync(2) (EINVAL on a FIFO) and the rename is
        // skipped; unlink the FIFO promptly so the NEXT snapshot
        // creates a regular temp file and persistence recovers. Even
        // if a snapshot or two still observes the FIFO, every flush
        // barrier stays satisfiable by a newer successful snapshot.
        let _fifo_end = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&tmp_path)
            .unwrap();
        for _ in 0..10_000 {
            if !tmp_path.exists() || std::fs::remove_file(&tmp_path).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        // Phase 1: every barrier must resolve Persisted — each flush's
        // covering revision (or a newer surviving snapshot) lands.
        for rx in &flush_rxs {
            let outcome = rx
                .recv_timeout(Duration::from_secs(30))
                .expect("flush barrier must be acknowledged");
            assert_eq!(
                outcome,
                FlushOutcome::Persisted,
                "covering-or-newer snapshot landed; barrier must succeed"
            );
        }

        // Phase 2: make every subsequent write fail, then submit more
        // barriers under the same pressure. With no queued snapshot
        // able to supersede the failed covering revision, each barrier
        // must report WriteFailed — a failed newer snapshot must not
        // be concealed by the earlier successful writes.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
            perms.set_mode(0o500);
            std::fs::set_permissions(dir.path(), perms).unwrap();
        }
        let mut failed_rxs = Vec::new();
        for _ in 0..(TRUST_WRITE_QUEUE_CAP / 2 + 8) {
            store.record_approval(&k, 3);
            let (tx, rx) = std::sync::mpsc::channel();
            let min_rev = store
                .writer
                .next_rev
                .load(std::sync::atomic::Ordering::SeqCst);
            store.writer.submit(TrustWrite::Flush { min_rev, ack: tx });
            failed_rxs.push(rx);
        }
        for rx in &failed_rxs {
            let outcome = rx
                .recv_timeout(Duration::from_secs(30))
                .expect("flush barrier must be acknowledged");
            assert_eq!(
                outcome,
                FlushOutcome::WriteFailed,
                "covering write failed with nothing newer queued"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(dir.path(), perms).unwrap();
        }

        // Phase 3: recovery — a fresh barrier succeeds and the newest
        // COMPLETE snapshot (including the phase-2 mutations that
        // failed durably) lands on disk.
        store.record_approval(&k, 4);
        assert_eq!(store.flush_pending_writes(), FlushOutcome::Persisted);
        drop(store);

        let store2 = TrustStore::load(dir.path(), RemediationConfig::default());
        let rec = store2.get(&k).unwrap();
        assert_eq!(
            rec.operator_approvals,
            1 + (TRUST_WRITE_QUEUE_CAP / 2 + 8) as u32 * 2 + 1,
            "final durable snapshot must be the newest complete state"
        );
    }

    /// Fault-injection regression: a genuine parent-directory sync
    /// failure must NOT advance durable_rev nor produce `Persisted`.
    /// The covering snapshot is treated exactly like any other
    /// persistence failure — the barrier reports `WriteFailed`,
    /// in-memory state is unaffected, and a later successful snapshot
    /// still converges to `Persisted`.
    #[test]
    #[cfg(unix)]
    fn failed_parent_dir_sync_cannot_report_persisted() {
        let dir = tmp_dir();
        let state_path = dir.path().join(TRUST_STATE_FILE);
        *DIR_SYNC_FAULT_PATH.lock().unwrap() = Some(state_path);
        let mut store = TrustStore::load(dir.path(), RemediationConfig::default());
        let k = key("dirsync-rule");
        store.record_approval(&k, 1);
        // The injected directory-sync failure makes the covering
        // snapshot non-durable; the barrier must report WriteFailed.
        assert_eq!(store.flush_pending_writes(), FlushOutcome::WriteFailed);
        // In-memory state is unaffected by the durable failure.
        assert_eq!(store.get(&k).unwrap().operator_approvals, 1);
        // Recovery: clear the fault; a newer complete snapshot lands
        // and the barrier reports Persisted on revision evidence.
        *DIR_SYNC_FAULT_PATH.lock().unwrap() = None;
        store.record_approval(&k, 2);
        assert_eq!(store.flush_pending_writes(), FlushOutcome::Persisted);
        drop(store);
        *DIR_SYNC_FAULT_PATH.lock().unwrap() = None;
    }
}
