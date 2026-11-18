use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::event_bus::InProcessBus;
use crate::graph::{BonsaiEvent, SubscriptionStatusWrite};
use crate::store::BonsaiStore;
use crate::subscription_sessions::SubscriptionSessionRegistry;
use crate::telemetry::TelemetryUpdate;

pub const VERIFICATION_WINDOW: Duration = Duration::from_secs(30);

// ── Bounded pending-observation buffer (M5.2B ordering) ──────────────────
//
// gNMI delivers initial-sync Updates BEFORE the SyncResponse that makes
// the subscriber emit `synchronized`, and telemetry/lifecycle travel on
// independent authenticated RPC streams — so a valid early observation
// can reach the verifier before its session is current. Dropping it
// permanently would falsely report `subscribed_but_silent` for
// ON_CHANGE paths that then stay quiet.
//
// The verifier therefore keeps a SMALL bounded buffer of gNMI
// observations that carry session provenance but are not yet
// current-session-authorized:
//
//   Identity key: (target, plan_id, session_id) — the exact
//   (plan_id, session_id) pair the authenticated lifecycle stream
//   later authorizes. Buffered evidence can NEVER become authoritative
//   on its own: release happens only when the session registry reports
//   that exact pair as the current synchronized+verified session, and
//   every replayed update re-passes `update_has_current_session`.
//
//   Limits: PENDING_OBSERVATION_PER_SESSION_CAP updates per session,
//   PENDING_OBSERVATION_SESSION_CAP distinct sessions, and
//   PENDING_OBSERVATION_GLOBAL_CAP updates total. Overflow drops the
//   OLDEST buffered evidence first (per-session FIFO, then the
//   oldest-created session bucket) — recent evidence is preferred.
//
//   Expiration/invalidation: a bucket is discarded when its newest
//   observation is older than PENDING_OBSERVATION_TTL, or when the
//   session reaches a terminal lifecycle state (failed/disconnected/
//   disabled/superseded) or is superseded by a newer session for the
//   target. Plan registration does not clear buffers: stale-session
//   buckets simply never match `current_session` and expire by TTL.
//
//   Non-gNMI updates and updates without (plan_id, session_id)
//   provenance are never buffered — they can never gain coverage.
const PENDING_OBSERVATION_PER_SESSION_CAP: usize = 128;
const PENDING_OBSERVATION_SESSION_CAP: usize = 512;
const PENDING_OBSERVATION_GLOBAL_CAP: usize = 16_384;
const PENDING_OBSERVATION_TTL: Duration = Duration::from_secs(90);
/// Conservative per-observation evidence budget. Structured gNMI paths
/// are normally hundreds of bytes; anything larger is discarded rather
/// than buffered so per-entry memory stays bounded by construction,
/// not by assumption.
const PENDING_OBSERVATION_MAX_EVIDENCE_BYTES: usize = 8 * 1024;
/// Hard caps on structured-path shape retained as evidence.
const PENDING_OBSERVATION_MAX_PATHS: usize = 8;
const PENDING_OBSERVATION_MAX_ELEMS: usize = 256;
const PENDING_OBSERVATION_MAX_KEYS_PER_ELEM: usize = 16;

/// (target, plan_id, session_id) identity of a pending-observation bucket.
type PendingKey = (String, String, String);

/// Compact coverage evidence retained for a buffered observation.
/// Only the fields the fenced coverage path consumes are kept — the
/// telemetry `value` payload and classification metadata are NEVER
/// buffered, bounding per-entry memory regardless of update size.
#[derive(Clone)]
struct PendingObservation {
    target: String,
    plan_id: String,
    session_id: String,
    source_protocol: String,
    /// Authenticated collector stamp applied at the core ingest
    /// boundary (empty for local/dev-compat). Retained so replayed
    /// evidence is fenced by the SAME collector binding as live
    //  evidence — a foreign collector's buffered observation can never
    //  be released under another collector's session.
    collector_id: String,
    gnmi_path: Vec<crate::proto::gnmi::Path>,
    is_delete: bool,
    timestamp_ns: i64,
    /// Trusted receipt time — the verifier's processing instant when the
    /// observation arrived. Carried through replay so buffered evidence
    /// keeps its ORIGINAL age: an observation buffered for 80s must not
    /// become fresh SAMPLE evidence merely because the session has just
    /// synchronized. Distinct from `timestamp_ns` (device time),
    /// replay time and persistence time.
    received_at: Instant,
}

impl PendingObservation {
    /// Conservative byte estimate of the retained evidence: identity
    /// strings plus every structured-path name/key byte. The telemetry
    /// `value` payload is never retained, so this bounds the entry.
    fn evidence_bytes(update: &TelemetryUpdate) -> usize {
        let mut n = update.target.len()
            + update.plan_id.len()
            + update.session_id.len()
            + update.source_protocol.len()
            + update.collector_id.len()
            + 64;
        for path in &update.gnmi_path {
            n += path.origin.len() + 16;
            for elem in &path.elem {
                n += elem.name.len() + 16;
                for (k, v) in &elem.key {
                    n += k.len() + v.len() + 8;
                }
            }
        }
        n
    }

    /// Returns None for evidence exceeding the size/shape budget —
    /// oversized observations are dropped with a metric rather than
    /// buffered, keeping per-entry memory bounded by construction.
    fn from_update(update: &TelemetryUpdate, received_at: Instant) -> Option<Self> {
        let elems: usize = update.gnmi_path.iter().map(|p| p.elem.len()).sum();
        let max_keys = update
            .gnmi_path
            .iter()
            .flat_map(|p| p.elem.iter())
            .map(|e| e.key.len())
            .max()
            .unwrap_or(0);
        if update.gnmi_path.len() > PENDING_OBSERVATION_MAX_PATHS
            || elems > PENDING_OBSERVATION_MAX_ELEMS
            || max_keys > PENDING_OBSERVATION_MAX_KEYS_PER_ELEM
            || Self::evidence_bytes(update) > PENDING_OBSERVATION_MAX_EVIDENCE_BYTES
        {
            metrics::counter!("bonsai_subscription_pending_oversized_total").increment(1);
            return None;
        }
        Some(Self {
            target: update.target.clone(),
            plan_id: update.plan_id.clone(),
            session_id: update.session_id.clone(),
            source_protocol: update.source_protocol.clone(),
            collector_id: update.collector_id.clone(),
            gnmi_path: update.gnmi_path.clone(),
            is_delete: update.is_delete,
            timestamp_ns: update.timestamp_ns,
            received_at,
        })
    }

    /// Rehydrate into the update shape `observe_update` consumes. The
    /// value/classification fields are irrelevant to coverage and are
    /// left empty — replayed evidence can only re-prove acquisition,
    /// never fabricate a semantic classification. Returns the update
    /// plus its ORIGINAL receipt instant for freshness accounting.
    fn into_update(self) -> (std::sync::Arc<TelemetryUpdate>, Instant) {
        let received_at = self.received_at;
        (
            std::sync::Arc::new(TelemetryUpdate {
                target: self.target,
                vendor: String::new(),
                hostname: String::new(),
                role: String::new(),
                site: String::new(),
                timestamp_ns: self.timestamp_ns,
                path: String::new(),
                value: serde_json::Value::Null,
                plan_id: self.plan_id,
                session_id: self.session_id,
                source_protocol: self.source_protocol,
                collector_id: self.collector_id,
                gnmi_path: self.gnmi_path,
                is_delete: self.is_delete,
            }),
            received_at,
        )
    }
}

struct PendingBucket {
    /// Arrival-ordered observations with their buffer-insertion time.
    updates: VecDeque<(Instant, PendingObservation)>,
    created: Instant,
    last_seen: Instant,
}

struct PendingObservations {
    buckets: HashMap<PendingKey, PendingBucket>,
    /// Observation count across all buckets — the enforced global cap.
    total: usize,
    /// Per-observation TTL; a field (not just the const) so tests can
    /// exercise expiry without waiting out the production window.
    ttl: Duration,
}

impl Default for PendingObservations {
    fn default() -> Self {
        Self {
            buckets: HashMap::new(),
            total: 0,
            ttl: PENDING_OBSERVATION_TTL,
        }
    }
}

impl PendingObservations {
    /// Buffer one non-current gNMI observation. Non-gNMI producers and
    /// updates lacking session provenance are ignored — they can never
    /// become coverage evidence later. `received_at` is the trusted
    /// receipt instant of THIS evidence — replayed observations keep
    /// their original receipt time rather than being re-aged.
    fn note(&mut self, update: &TelemetryUpdate, received_at: Instant) {
        if update.source_protocol != "gnmi"
            || update.plan_id.is_empty()
            || update.session_id.is_empty()
        {
            return;
        }
        let now = received_at;
        let Some(obs) = PendingObservation::from_update(update, now) else {
            return;
        };
        let key: PendingKey = (
            update.target.clone(),
            update.plan_id.clone(),
            update.session_id.clone(),
        );
        if let Some(bucket) = self.buckets.get_mut(&key) {
            if bucket.updates.len() >= PENDING_OBSERVATION_PER_SESSION_CAP {
                bucket.updates.pop_front();
                self.total = self.total.saturating_sub(1);
                metrics::counter!("bonsai_subscription_pending_session_overflow_total")
                    .increment(1);
            }
            bucket.updates.push_back((now, obs));
            bucket.last_seen = now;
        } else {
            if self.buckets.len() >= PENDING_OBSERVATION_SESSION_CAP {
                self.evict_oldest_bucket();
            }
            let mut updates = VecDeque::new();
            updates.push_back((now, obs));
            self.buckets.insert(
                key,
                PendingBucket {
                    updates,
                    created: now,
                    last_seen: now,
                },
            );
        }
        // The global cap binds EVERY insertion — including growth of an
        // existing bucket — so `total` can never exceed it.
        self.total += 1;
        while self.total > PENDING_OBSERVATION_GLOBAL_CAP {
            if !self.evict_oldest_bucket() {
                break;
            }
        }
    }

    /// Remove the bucket with the oldest first-seen observation.
    /// Returns false when nothing can be evicted.
    fn evict_oldest_bucket(&mut self) -> bool {
        let victim = self
            .buckets
            .iter()
            .min_by_key(|(_, b)| b.created)
            .map(|(k, _)| k.clone());
        let Some(key) = victim else { return false };
        metrics::counter!("bonsai_subscription_pending_evicted_total").increment(1);
        if let Some(bucket) = self.buckets.remove(&key) {
            // Bucket-count and observation-count metrics are distinct:
            // this counter tracks evicted BUCKETS, the obs counter the
            // observations they carried.
            metrics::counter!("bonsai_subscription_pending_evicted_obs_total")
                .increment(bucket.updates.len() as u64);
            self.total = self.total.saturating_sub(bucket.updates.len());
            true
        } else {
            false
        }
    }

    /// Drop expired buckets and buckets whose session is terminal.
    /// Returns the keys whose session is CURRENT — the caller drains
    /// those through the normal fenced observation path.
    fn sweep(&mut self, sessions: &SubscriptionSessionRegistry) -> Vec<PendingKey> {
        let now = Instant::now();
        let mut ready = Vec::new();
        self.buckets.retain(|key, bucket| {
            // Per-observation expiry FIRST: a steady stream of new
            // evidence must not keep aged-out observations alive —
            // each entry expires on its own receipt instant.
            let before = bucket.updates.len();
            bucket
                .updates
                .retain(|(received, _)| now.duration_since(*received) <= self.ttl);
            let expired_obs = before - bucket.updates.len();
            if expired_obs > 0 {
                metrics::counter!("bonsai_subscription_pending_expired_obs_total")
                    .increment(expired_obs as u64);
            }
            if bucket.updates.is_empty() {
                metrics::counter!("bonsai_subscription_pending_expired_total").increment(1);
                return false;
            }
            // A bucket whose newest evidence is itself older than the
            // TTL can hold nothing live.
            if now.duration_since(bucket.last_seen) > self.ttl {
                metrics::counter!("bonsai_subscription_pending_expired_total").increment(1);
                return false;
            }
            // Terminal sessions can never become current again — their
            // buffered evidence is historical, not pending.
            if let Some((state, _, _)) = sessions.session_state(&key.0, &key.2)
                && crate::subscription_sessions::is_terminal(&state)
            {
                metrics::counter!("bonsai_subscription_pending_terminal_total").increment(1);
                return false;
            }
            // Release gate: the exact (plan_id, session_id) must be
            // current. Per-observation collector binding is enforced
            // again inside observe_update — buffered foreign-collector
            // evidence can be released but never authorized.
            if sessions
                .current_session(&key.0)
                .is_some_and(|(plan_id, session_id)| plan_id == key.1 && session_id == key.2)
            {
                ready.push(key.clone());
            }
            true
        });
        // Recompute total after retain (retain does not report removals).
        self.total = self.buckets.values().map(|b| b.updates.len()).sum();
        ready
    }

    /// Take the LIVE buffered observations for `key` (arrival order),
    /// rehydrated into the update shape the fenced path consumes.
    /// Per-observation TTL is enforced again immediately before
    /// release: evidence that aged out while waiting for its session
    /// can never become coverage.
    fn take(&mut self, key: &PendingKey) -> Vec<(std::sync::Arc<TelemetryUpdate>, Instant)> {
        let Some(bucket) = self.buckets.remove(key) else {
            return Vec::new();
        };
        let now = Instant::now();
        let len = bucket.updates.len();
        let live: Vec<_> = bucket
            .updates
            .into_iter()
            .filter(|(received, _)| now.duration_since(*received) <= self.ttl)
            .map(|(_, u)| u.into_update())
            .collect();
        self.total = self.total.saturating_sub(len);
        live
    }
}

#[derive(Clone, Debug)]
pub struct SubscriptionPlan {
    pub target: String,
    pub paths: Vec<SubscriptionPathExpectation>,
    /// Deterministic identity of the desired acquisition plan (hash of the
    /// normalized intent + path set). Stable across reconnects for an
    /// unchanged configuration; changes when the effective plan changes.
    pub plan_id: String,
    /// "submitted" — paths were sent in a Subscribe RPC.
    /// "disabled" — acquisition disabled; no RPC emitted.
    /// "rejected" — explicit plan failed validation; no RPC emitted.
    pub state: String,
    /// "automatic" | "explicit" | "disabled" — operator acquisition intent.
    pub acquisition: String,
    /// Device-facing subscription session that owns this plan's coverage.
    /// Minted per Subscribe session by the collector; empty for
    /// non-submitted states. A reconnect under the same plan_id carries a
    /// NEW session_id and must supersede, never merge with, the old one.
    pub session_id: String,
    /// Operator-visible detail (rejection reason, disable note).
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct SubscriptionPathExpectation {
    pub path: String,
    pub origin: String,
    pub mode: String,
    pub sample_interval_ns: u64,
}

#[derive(Clone, Debug)]
struct TrackedPath {
    expectation: SubscriptionPathExpectation,
    /// Plan identity that registered this expectation.
    plan_id: String,
    /// Device-facing session expected to produce coverage for this
    /// path. Empty for desired/predicted plans that carry no session
    /// binding yet — such paths are covered by whatever current
    /// session the registry authorizes for the plan.
    session_id: String,
    /// Desired logical status. Transitions commit here immediately;
    /// `durable_status` records what the store has actually persisted
    /// so failed writes are retried until they land (M5.2B review B7).
    status: String,
    /// Last status confirmed written to the graph store ("" = nothing
    /// persisted yet). The northbound reads the store, so presentation
    /// stays truthful while `status` reconciles.
    durable_status: String,
    /// `last_observed_at_ns` value confirmed persisted alongside
    /// `durable_status`. A successful write acknowledges ONLY the exact
    /// row identity AND the timestamp snapshot it carried — a queued
    /// write for path A can never mark path B durable, and a stale
    /// same-status snapshot cannot silently absorb newer evidence
    /// timestamps (M5.2B review E).
    durable_last_observed_at_ns: i64,
    first_observed_at_ns: i64,
    last_observed_at_ns: i64,
    deadline: Instant,
    /// Monotonic timestamp of the most recent covering observation —
    /// drives SAMPLE freshness independent of wall-clock ns.
    last_seen: Instant,
    /// Receipt instant of the newest APPLIED covering observation.
    /// `None` until the first covering observation lands — pre-sync
    /// buffered evidence is legitimately older than registration and
    /// must still apply. Once set, earlier-received evidence can never
    /// overwrite newer authoritative state — the buffered→live
    /// ordering fence (M5.2B review B6).
    last_evidence_at: Option<Instant>,
    /// How long an observed SAMPLE path may stay quiet before it is
    /// reported stale. `Duration::MAX` for ON_CHANGE: a quiet ON_CHANGE
    /// subscription remains valid (M5.2B B3).
    stale_after: Duration,
}

pub async fn run_subscription_verifier<S: BonsaiStore + 'static>(
    store: S,
    bus: Arc<InProcessBus>,
    plan_rx: tokio::sync::mpsc::Receiver<SubscriptionPlan>,
    sessions: Arc<SubscriptionSessionRegistry>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) {
    run_subscription_verifier_with_windows(
        store,
        bus,
        plan_rx,
        sessions,
        shutdown,
        Duration::from_secs(5),
        VERIFICATION_WINDOW,
    )
    .await;
}

/// Verifier core parameterized on the sweep tick and the pending/
/// freshness floor so tests can exercise stale transitions without
/// waiting out the production 30s window. Production callers must use
/// `run_subscription_verifier`.
#[doc(hidden)]
pub async fn run_subscription_verifier_with_windows<S: BonsaiStore + 'static>(
    store: S,
    bus: Arc<InProcessBus>,
    mut plan_rx: tokio::sync::mpsc::Receiver<SubscriptionPlan>,
    sessions: Arc<SubscriptionSessionRegistry>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    tick: Duration,
    freshness_floor: Duration,
) {
    let (sub, mut telemetry_rx) = crate::event_bus::MpscSubscriber::new(
        "subscription_verifier",
        1024,
        crate::event_bus::OverflowPolicy::DropNewest,
    );
    bus.add_subscriber(sub).await;

    let mut interval = tokio::time::interval(tick);
    let mut tracked: HashMap<(String, String), TrackedPath> = HashMap::new();
    // Bounded pending-observation buffer: gNMI initial-sync updates can
    // legitimately arrive before their session's `synchronized`
    // lifecycle transition is applied (independent RPC streams). They
    // are held under (target, plan_id, session_id) and released only
    // when that exact session becomes current — see PendingObservations.
    let mut pending = PendingObservations::default();
    // Bounded durable-retry queue (M5.2B review B7): status writes that
    // failed at transition time are retained here and retried each
    // sweep. Entries are keyed by status identity so a re-registered
    // path drops its obsolete superseded write — a stale retry can
    // never overwrite newer session state.
    let mut pending_writes: VecDeque<SubscriptionStatusWrite> = VecDeque::new();
    // Persisted rows describe the PREVIOUS boot's sessions: nothing is
    // current until new verified evidence arrives, so historical
    // observed/deleted/pending rows are expired to `stale` up front
    // (raw graph state and the northbound read the same rows).
    if let Err(error) = store.expire_subscription_statuses_on_boot().await {
        warn!(%error, "failed to expire persisted subscription statuses on boot");
    }
    // target -> last registered plan identity INCLUDING lifecycle
    // revision (state + detail). plan_id alone is the stable desired/
    // effective identity; a changed failure outcome under the same
    // plan_id must still be delivered, while an identical repeat is
    // deduplicated. A reconnect under an unchanged plan carries a NEW
    // session_id and is NOT deduplicated.
    let mut registered_plans: HashMap<String, (String, String, String, String)> = HashMap::new();

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                info!("subscription verifier received shutdown");
                break;
            }
            maybe_plan = plan_rx.recv() => {
                let Some(plan) = maybe_plan else {
                    info!("subscription verifier plan channel closed");
                    break;
                };
                let key = (
                    plan.plan_id.clone(),
                    plan.session_id.clone(),
                    plan.state.clone(),
                    plan.detail.clone(),
                );
                if registered_plans.get(&plan.target) == Some(&key) {
                    continue;
                }
                registered_plans.insert(plan.target.clone(), key);
                register_plan(&store, &mut tracked, &mut pending_writes, plan, freshness_floor)
                    .await;
            }
            event = telemetry_rx.recv() => {
                match event {
                    Some(update) => {
                        observe_update(
                            &store,
                            &mut tracked,
                            &sessions,
                            &mut pending,
                            &mut pending_writes,
                            &update,
                            Instant::now(),
                        )
                        .await;
                        // drain backlog without yielding — avoids queue saturation
                        while let Ok(update) = telemetry_rx.try_recv() {
                            observe_update(
                                &store,
                                &mut tracked,
                                &sessions,
                                &mut pending,
                                &mut pending_writes,
                                &update,
                                Instant::now(),
                            )
                            .await;
                        }
                    }
                    None => break,
                }
            }
            _ = interval.tick() => {
                // Release buffered early-sync observations whose session
                // has since become current, expire stale buckets, then
                // run the silence/freshness/authority sweeps and retry
                // failed durable writes.
                for key in pending.sweep(&sessions) {
                    for (update, received_at) in pending.take(&key) {
                        observe_update(
                            &store,
                            &mut tracked,
                            &sessions,
                            &mut pending,
                            &mut pending_writes,
                            &update,
                            received_at,
                        )
                        .await;
                    }
                }
                reconcile_statuses(&store, &sessions, &mut tracked, &mut pending_writes).await;
            }
        }
    }
}

/// Identity of a persisted status row — mirrors the store's
/// `subscription_status_id` so durable retries can be deduplicated and
/// obsolete entries dropped when a path re-registers.
fn status_write_id(status: &SubscriptionStatusWrite) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        status.device_address, status.origin, status.mode, status.sample_interval_ns, status.path
    )
}

/// Queue a failed durable write for bounded retry. Dedup by row
/// identity: the newest desired write for a row replaces any older
/// queued one, so retries always converge to current state and an
/// obsolete superseded write can never overwrite a re-registered path
/// (its queued entry is dropped at re-registration).
async fn queue_or_persist<S: BonsaiStore>(
    store: &S,
    pending_writes: &mut VecDeque<SubscriptionStatusWrite>,
    status: SubscriptionStatusWrite,
) {
    const PENDING_WRITE_CAP: usize = 4096;
    let id = status_write_id(&status);
    pending_writes.retain(|w| status_write_id(w) != id);
    if let Err(error) = persist_status(store, &status).await {
        warn!(%error, target = %status.device_address, path = %status.path, "subscription status write failed; queued for retry");
        if pending_writes.len() >= PENDING_WRITE_CAP {
            pending_writes.pop_front();
            metrics::counter!("bonsai_subscription_status_retry_overflow_total").increment(1);
        }
        pending_writes.push_back(status);
    }
}

async fn register_plan<S: BonsaiStore>(
    store: &S,
    tracked: &mut HashMap<(String, String), TrackedPath>,
    pending_writes: &mut VecDeque<SubscriptionStatusWrite>,
    plan: SubscriptionPlan,
    freshness_floor: Duration,
) {
    let now = now_ns();
    let target = plan.target.clone();
    let submitted = plan.state == "submitted";

    // Supersede every previously-tracked path for this target that is not
    // part of the new submitted plan, so an obsolete plan is never left
    // represented as current/active. Disabled and rejected plans supersede
    // the entire previous plan. The durable write is queued for bounded
    // retry BEFORE the in-memory entry is dropped — a failed supersede
    // can never silently lose the reconciliation (M5.2B review B7).
    let new_keys: std::collections::HashSet<String> = plan
        .paths
        .iter()
        .map(|e| tracked_key(&target, e).1)
        .collect();
    let stale_keys: Vec<(String, String)> = tracked
        .iter()
        .filter(|((t, k), _)| t == &target && !(submitted && new_keys.contains(k)))
        .map(|(key, _)| key.clone())
        .collect();
    for key in stale_keys {
        if let Some(tracked_path) = tracked.remove(&key) {
            let status = SubscriptionStatusWrite {
                device_address: target.clone(),
                path: tracked_path.expectation.path.clone(),
                origin: tracked_path.expectation.origin.clone(),
                mode: tracked_path.expectation.mode.clone(),
                sample_interval_ns: tracked_path.expectation.sample_interval_ns as i64,
                status: "superseded".to_string(),
                first_observed_at_ns: tracked_path.first_observed_at_ns,
                last_observed_at_ns: tracked_path.last_observed_at_ns,
                updated_at_ns: now,
            };
            queue_or_persist(store, pending_writes, status).await;
        }
    }

    // Supersede PERSISTED statuses outside the new plan as well. The
    // in-memory `tracked` sweep above only covers this process's memory;
    // after a restart it is empty while the graph still holds statuses
    // written by the previous plan. Without this, an obsolete plan's rows
    // would remain displayed as current alongside (or instead of) the new
    // plan. Disabled/rejected plans keep nothing — every persisted row for
    // the target is superseded.
    let keep: std::collections::HashSet<(String, String, String, i64)> = if submitted {
        plan.paths
            .iter()
            .map(|e| {
                (
                    e.path.clone(),
                    e.origin.clone(),
                    e.mode.clone(),
                    e.sample_interval_ns as i64,
                )
            })
            .collect()
    } else {
        std::collections::HashSet::new()
    };
    if let Err(error) = store
        .supersede_subscription_statuses_outside_plan(&target, &keep)
        .await
    {
        warn!(%error, target = %target, "failed to supersede persisted subscription statuses");
    }

    if !submitted {
        info!(
            target = %target,
            plan_id = %plan.plan_id,
            state = %plan.state,
            acquisition = %plan.acquisition,
            detail = %plan.detail,
            "acquisition plan not submitted to device"
        );
        publish_plan_state(store, &plan).await;
        return;
    }

    let deadline = Instant::now() + freshness_floor;

    for expectation in plan.paths {
        let key = tracked_key(&target, &expectation);
        // Unchanged re-registration (same plan, same session, same
        // expectation — e.g. an unrelated inventory update re-seeding
        // the plan) must not reset valid coverage: carry the existing
        // status and evidence timestamps forward. A NEW session_id or
        // plan_id always starts pending — coverage is session-bound.
        let carried = tracked.get(&key).and_then(|existing| {
            (existing.plan_id == plan.plan_id && existing.session_id == plan.session_id).then(
                || TrackedPath {
                    expectation: expectation.clone(),
                    plan_id: plan.plan_id.clone(),
                    session_id: plan.session_id.clone(),
                    status: existing.status.clone(),
                    durable_status: existing.durable_status.clone(),
                    durable_last_observed_at_ns: existing.durable_last_observed_at_ns,
                    first_observed_at_ns: existing.first_observed_at_ns,
                    last_observed_at_ns: existing.last_observed_at_ns,
                    deadline,
                    last_seen: existing.last_seen,
                    last_evidence_at: existing.last_evidence_at,
                    stale_after: sample_stale_after(&expectation, freshness_floor),
                },
            )
        });
        let is_new = carried.is_none();
        let tracked_path = carried.unwrap_or_else(|| TrackedPath {
            expectation: expectation.clone(),
            plan_id: plan.plan_id.clone(),
            session_id: plan.session_id.clone(),
            status: "pending".to_string(),
            durable_status: String::new(),
            durable_last_observed_at_ns: 0,
            first_observed_at_ns: 0,
            last_observed_at_ns: 0,
            deadline,
            last_seen: Instant::now(),
            last_evidence_at: None,
            stale_after: sample_stale_after(&expectation, freshness_floor),
        });
        tracked.insert(key.clone(), tracked_path);
        if is_new {
            // A re-registered path may have a queued obsolete
            // superseded write — drop it so the retry can never
            // overwrite the fresh pending/observed state.
            pending_writes.retain(|w| {
                !(w.device_address == target
                    && w.path == expectation.path
                    && w.origin == expectation.origin
                    && w.mode == expectation.mode
                    && w.sample_interval_ns == expectation.sample_interval_ns as i64)
            });
            let status = SubscriptionStatusWrite {
                device_address: target.clone(),
                path: expectation.path.clone(),
                origin: expectation.origin.clone(),
                mode: expectation.mode.clone(),
                sample_interval_ns: expectation.sample_interval_ns as i64,
                status: "pending".to_string(),
                first_observed_at_ns: 0,
                last_observed_at_ns: 0,
                updated_at_ns: now,
            };
            queue_or_persist(store, pending_writes, status).await;
        }
    }
}

async fn observe_update<S: BonsaiStore>(
    store: &S,
    tracked: &mut HashMap<(String, String), TrackedPath>,
    sessions: &SubscriptionSessionRegistry,
    pending: &mut PendingObservations,
    pending_writes: &mut VecDeque<SubscriptionStatusWrite>,
    update: &TelemetryUpdate,
    // Trusted receipt instant of THIS evidence. Live bus events pass
    // `Instant::now()`; buffered replays pass their ORIGINAL receipt
    // time so replay never renews freshness.
    received_at: Instant,
) {
    if !update_has_current_session(sessions, update) {
        // Not current-authorized. A gNMI observation carrying session
        // provenance may be early initial-sync evidence whose
        // `synchronized` lifecycle transition is still in flight — hold
        // it in the bounded pending buffer; release is gated on the
        // exact (plan_id, session_id, collector) becoming current.
        // Everything else (non-gNMI, missing provenance, stale/
        // superseded sessions past TTL) is dropped by `note`'s filters
        // or expires unauthenticated.
        pending.note(update, received_at);
        return;
    }
    let matching: Vec<((String, String), CoverageMatch)> = tracked
        .iter()
        .filter_map(|(key, tracked_path)| {
            // Coverage is plan-bound: an observation authorized under
            // one plan can never satisfy expectations registered under
            // a different plan, even on the same target/path (B1/B3).
            (key.0 == update.target && tracked_path.plan_id == update.plan_id)
                .then(|| {
                    coverage_match(&tracked_path.expectation, update).map(|m| (key.clone(), m))
                })
                .flatten()
        })
        .collect();

    if matching.is_empty() {
        // Current-session evidence that matches no registered
        // expectation can be legitimate early evidence: the plan
        // registration may still be in flight on the plan channel.
        // Hold it in the same bounded buffer — release replays through
        // this function once expectations exist (M5.2B review B6).
        // Non-gNMI/unprovenanced updates are filtered by `note`.
        pending.note(update, received_at);
        return;
    }

    // Persisted evidence timestamps use CORE receipt time, never the
    // device timestamp: a future or malformed device timestamp must not
    // become trusted freshness (M5.2B review B7). Device time remains
    // available on the telemetry update itself for diagnostics.
    let now = now_ns();

    for (key, coverage) in matching {
        let Some(tracked_path) = tracked.get_mut(&key) else {
            continue;
        };
        // Ordering fence (M5.2B review B6): evidence received BEFORE the
        // newest already-applied observation can never overwrite newer
        // authoritative state. This is what keeps an older buffered
        // update from reversing a newer live delete after the session
        // synchronized.
        if tracked_path
            .last_evidence_at
            .is_some_and(|prev| received_at < prev)
        {
            metrics::counter!("bonsai_subscription_stale_evidence_total").increment(1);
            continue;
        }
        tracked_path.last_evidence_at = Some(received_at);
        // `last_seen` mirrors the newest APPLIED evidence receipt — the
        // fence above guarantees `received_at` never regresses, so a
        // replayed pre-sync observation keeps its original (older)
        // freshness instead of being renewed by replay time (B6/B7).
        tracked_path.last_seen = received_at;
        // Delete/tombstone evidence (M5.2B B3/B9): a current-session
        // Notification.delete that is the expectation path or a proper
        // ancestor invalidates coverage as `deleted`; a delete INSIDE
        // the expectation's subtree only QUALIFIES it — current
        // `observed` coverage drops to `stale` because one deleted
        // descendant never proves complete subtree absence, but the
        // evidence can no longer support a current claim. Stale-session
        // deletes are already fenced by update_has_current_session.
        let new_status = match coverage {
            CoverageMatch::Observe => "observed",
            CoverageMatch::DeleteComplete => "deleted",
            CoverageMatch::DeletePartial => {
                if tracked_path.status == "observed" {
                    "stale"
                } else {
                    continue;
                }
            }
        };
        if tracked_path.status == new_status {
            // Same-status positive evidence still advances the
            // last-observed timestamp — repeated ON_CHANGE/SAMPLE
            // observations are real freshness. The dirty-status
            // reconcile persists the advance on the next sweep, so
            // persisted timestamps move forward without one durable
            // write per observation (M5.2B review F).
            if new_status == "observed" && !update.is_delete {
                tracked_path.last_observed_at_ns = now;
            }
            continue;
        }
        // Desired-status transition commits in memory immediately; the
        // durable write is attempted now and queued for bounded retry
        // on failure, so a one-shot observation survives a transient
        // store error without needing another device update (B7).
        let first_observed = if new_status == "observed" {
            now
        } else {
            tracked_path.first_observed_at_ns
        };
        let last_observed = if update.is_delete {
            tracked_path.last_observed_at_ns
        } else {
            now
        };
        tracked_path.status = new_status.to_string();
        tracked_path.first_observed_at_ns = first_observed;
        tracked_path.last_observed_at_ns = last_observed;
        let status = SubscriptionStatusWrite {
            device_address: key.0.clone(),
            path: tracked_path.expectation.path.clone(),
            origin: tracked_path.expectation.origin.clone(),
            mode: tracked_path.expectation.mode.clone(),
            sample_interval_ns: tracked_path.expectation.sample_interval_ns as i64,
            status: new_status.to_string(),
            first_observed_at_ns: first_observed,
            last_observed_at_ns: last_observed,
            updated_at_ns: now,
        };
        match persist_status(store, &status).await {
            Ok(()) => {
                if let Some(tp) = tracked.get_mut(&key) {
                    tp.durable_status = new_status.to_string();
                    tp.durable_last_observed_at_ns = status.last_observed_at_ns;
                }
            }
            Err(error) => {
                warn!(%error, target = %key.0, path_key = %key.1, "failed to write subscription status; queued for retry");
                if pending_writes.len() >= 4096 {
                    pending_writes.pop_front();
                    metrics::counter!("bonsai_subscription_status_retry_overflow_total")
                        .increment(1);
                }
                pending_writes.push_back(status);
            }
        }
    }
}

/// Persist one status transition and publish its event only after the
/// store write succeeds. Returns the store error so callers can keep
/// the previous in-memory state and retry on the next transition.
async fn persist_status<S: BonsaiStore>(
    store: &S,
    status: &SubscriptionStatusWrite,
) -> anyhow::Result<()> {
    store.write_subscription_status(status.clone()).await?;
    store.publish_event(subscription_status_event(status));
    Ok(())
}

/// SAMPLE freshness bound (M5.2B B3): an observed SAMPLE path is stale
/// after max(3 × sample_interval, VERIFICATION_WINDOW) without a new
/// covering observation — roughly three missed reporting periods, with
/// a floor so sub-second intervals still get a sane bound. ON_CHANGE
/// paths return `Duration::MAX`: quiet is valid, never stale.
fn sample_stale_after(expectation: &SubscriptionPathExpectation, floor: Duration) -> Duration {
    if !expectation.mode.eq_ignore_ascii_case("sample") {
        return Duration::MAX;
    }
    let interval = Duration::from_nanos(expectation.sample_interval_ns);
    (interval.saturating_mul(3)).max(floor)
}

/// Whether the session bound to a tracked path still holds current
/// coverage authority. Session-bound paths (real `session_id`) require
/// the exact current (plan_id, session_id); desired/legacy plans with
/// no session binding are covered by whatever session the registry
/// currently authorizes for the target — plan identity is reconciled
/// at registration. Loss of current authority (disconnect, withdrawal,
/// supersession, reassignment) makes observed evidence historical.
fn tracked_session_current(
    sessions: &SubscriptionSessionRegistry,
    target: &str,
    plan_id: &str,
    session_id: &str,
) -> bool {
    let Some((cur_plan, cur_session, _)) = sessions.current_session_provenance(target) else {
        return false;
    };
    if cur_session.is_empty() {
        return false;
    }
    if session_id.is_empty() {
        // Session-unbound (desired/predicted) expectations are covered
        // by whatever current session the registry authorizes for the
        // plan — but ONLY while such a session exists. With no live
        // verified session there is no current coverage authority.
        cur_plan == plan_id
    } else {
        cur_session == session_id && cur_plan == plan_id
    }
}

/// Build the durable write for a tracked path's CURRENT desired state.
fn status_write_for(
    key: &(String, String),
    tracked_path: &TrackedPath,
    now: i64,
) -> SubscriptionStatusWrite {
    SubscriptionStatusWrite {
        device_address: key.0.clone(),
        path: tracked_path.expectation.path.clone(),
        origin: tracked_path.expectation.origin.clone(),
        mode: tracked_path.expectation.mode.clone(),
        sample_interval_ns: tracked_path.expectation.sample_interval_ns as i64,
        status: tracked_path.status.clone(),
        first_observed_at_ns: tracked_path.first_observed_at_ns,
        last_observed_at_ns: tracked_path.last_observed_at_ns,
        updated_at_ns: now,
    }
}

/// Set a tracked path's desired status and persist-or-queue it.
async fn transition_status<S: BonsaiStore>(
    store: &S,
    pending_writes: &mut VecDeque<SubscriptionStatusWrite>,
    key: &(String, String),
    tracked_path: &mut TrackedPath,
    new_status: &str,
    now: i64,
) {
    tracked_path.status = new_status.to_string();
    let status = status_write_for(key, tracked_path, now);
    match persist_status(store, &status).await {
        Ok(()) => {
            tracked_path.durable_status = new_status.to_string();
            tracked_path.durable_last_observed_at_ns = tracked_path.last_observed_at_ns;
        }
        Err(error) => {
            warn!(%error, target = %key.0, path_key = %key.1, "failed to write subscription status; queued for retry");
            if pending_writes.len() >= 4096 {
                pending_writes.pop_front();
                metrics::counter!("bonsai_subscription_status_retry_overflow_total").increment(1);
            }
            pending_writes.push_back(status);
        }
    }
}

/// Periodic reconciliation sweep (M5.2B review B5/B7):
///
/// - pending paths past their deadline become `subscribed_but_silent`;
/// - observed SAMPLE paths quiet past their freshness bound become
///   `stale`;
/// - observed paths whose bound session lost current authority
///   (disconnect, withdrawal, supersession, reassignment, core restart)
///   become `stale` — observed history is never presented as current
///   monitoring without a live verified session;
/// - every desired status that has not been durably persisted is
///   retried, and the bounded pending-write queue is drained, so failed
///   pending/superseded/disconnected/observed/deleted/stale transitions
///   eventually reconcile without needing new telemetry.
async fn reconcile_statuses<S: BonsaiStore>(
    store: &S,
    sessions: &SubscriptionSessionRegistry,
    tracked: &mut HashMap<(String, String), TrackedPath>,
    pending_writes: &mut VecDeque<SubscriptionStatusWrite>,
) {
    let now_instant = Instant::now();
    let now = now_ns();

    // Drain the bounded durable-retry queue first. A queued write is a
    // snapshot: if the row is still tracked and now desires a DIFFERENT
    // status, the queued write is obsolete — drop it (the dirty-status
    // reconcile below persists the current desire). Otherwise persist
    // it; on success reflect durable progress on the tracked path.
    let mut i = 0;
    while i < pending_writes.len() {
        let write = pending_writes[i].clone();
        // A queued write is a snapshot. It is obsolete when the tracked
        // path's desired status has since advanced — drop it (the
        // dirty-status reconcile below persists the current desire).
        // On success acknowledge ONLY the exact persisted row identity
        // with the timestamp snapshot the write actually carried: one
        // path's successful retry can never mark a different path
        // durable, and a stale same-status snapshot cannot absorb newer
        // evidence timestamps — any residual skew is re-dirtied and
        // reconciled on the next pass (M5.2B review E).
        let current = tracked.iter().find_map(|(key, tp)| {
            let e = &tp.expectation;
            (key.0 == write.device_address
                && e.path == write.path
                && e.origin == write.origin
                && e.mode == write.mode
                && e.sample_interval_ns as i64 == write.sample_interval_ns)
                .then(|| (tp.status.clone(), tp.last_observed_at_ns))
        });
        if current.as_ref().is_some_and(|(s, _)| *s != write.status) {
            pending_writes.remove(i);
            continue;
        }
        match persist_status(store, &write).await {
            Ok(()) => {
                pending_writes.remove(i);
                if let Some(tp) = tracked
                    .iter_mut()
                    .find(|(key, tp)| {
                        let e = &tp.expectation;
                        key.0 == write.device_address
                            && e.path == write.path
                            && e.origin == write.origin
                            && e.mode == write.mode
                            && e.sample_interval_ns as i64 == write.sample_interval_ns
                    })
                    .map(|(_, tp)| tp)
                {
                    tp.durable_status = write.status.clone();
                    tp.durable_last_observed_at_ns = write.last_observed_at_ns;
                }
            }
            Err(error) => {
                warn!(%error, target = %write.device_address, path = %write.path, "subscription status retry failed");
                i += 1;
            }
        }
    }

    // Pending → subscribed_but_silent on deadline.
    let silent_keys: Vec<(String, String)> = tracked
        .iter()
        .filter(|(_, path)| path.status == "pending" && now_instant >= path.deadline)
        .map(|(key, _)| key.clone())
        .collect();
    for key in silent_keys {
        if let Some(tracked_path) = tracked.get_mut(&key) {
            transition_status(
                store,
                pending_writes,
                &key,
                tracked_path,
                "subscribed_but_silent",
                now,
            )
            .await;
        }
    }

    // SAMPLE freshness: observed SAMPLE paths quiet past their bound
    // are stale. ON_CHANGE and non-observed states are untouched.
    let stale_keys: Vec<(String, String)> = tracked
        .iter()
        .filter(|(_, path)| {
            path.status == "observed"
                && path.stale_after != Duration::MAX
                && now_instant.saturating_duration_since(path.last_seen) >= path.stale_after
        })
        .map(|(key, _)| key.clone())
        .collect();
    for key in stale_keys {
        if let Some(tracked_path) = tracked.get_mut(&key) {
            transition_status(store, pending_writes, &key, tracked_path, "stale", now).await;
        }
    }

    // Current-vs-historical authority (M5.2B review B5): an observed
    // path stays "observed" only while its bound session remains the
    // current verified session. Disconnect, withdrawal, supersession,
    // reassignment and core restart all revoke that — quiet ON_CHANGE
    // values are legitimate only inside a live session.
    let revoked_keys: Vec<(String, String)> = tracked
        .iter()
        .filter(|((target, _), path)| {
            path.status == "observed"
                && !tracked_session_current(sessions, target, &path.plan_id, &path.session_id)
        })
        .map(|(key, _)| key.clone())
        .collect();
    for key in revoked_keys {
        if let Some(tracked_path) = tracked.get_mut(&key) {
            transition_status(store, pending_writes, &key, tracked_path, "stale", now).await;
        }
    }

    // Durable reconciliation: any desired status — or last-observed
    // timestamp advance — not yet confirmed persisted is retried every
    // sweep, so a one-shot observation whose write failed transiently
    // converges without further telemetry, and repeated same-status
    // observations still move persisted freshness forward.
    let dirty_keys: Vec<(String, String)> = tracked
        .iter()
        .filter(|(_, path)| {
            path.status != path.durable_status
                || path.last_observed_at_ns != path.durable_last_observed_at_ns
        })
        .map(|(key, _)| key.clone())
        .collect();
    for key in dirty_keys {
        let Some(tracked_path) = tracked.get_mut(&key) else {
            continue;
        };
        let status = status_write_for(&key, tracked_path, now);
        match persist_status(store, &status).await {
            Ok(()) => {
                tracked_path.durable_status = tracked_path.status.clone();
                tracked_path.durable_last_observed_at_ns = status.last_observed_at_ns;
            }
            Err(error) => {
                warn!(%error, target = %key.0, path_key = %key.1, "subscription status reconcile failed; will retry");
            }
        }
    }
}

async fn publish_plan_state<S: BonsaiStore>(store: &S, plan: &SubscriptionPlan) {
    let event = BonsaiEvent {
        device_address: plan.target.clone(),
        event_type: "subscription_plan_state".to_string(),
        detail_json: serde_json::json!({
            "plan_id": &plan.plan_id,
            "state": &plan.state,
            "acquisition": &plan.acquisition,
            "detail": &plan.detail,
            "paths": plan.paths.len(),
        })
        .to_string(),
        occurred_at_ns: now_ns(),
        state_change_event_id: String::new(),
        source_type: "gnmi".to_string(),
    };
    store.publish_event(event);
}

fn subscription_status_event(status: &SubscriptionStatusWrite) -> BonsaiEvent {
    BonsaiEvent {
        device_address: status.device_address.clone(),
        event_type: "subscription_status_change".to_string(),
        detail_json: serde_json::json!({
            "path": &status.path,
            "origin": &status.origin,
            "mode": &status.mode,
            "sample_interval_ns": status.sample_interval_ns,
            "status": &status.status,
            "first_observed_at_ns": status.first_observed_at_ns,
            "last_observed_at_ns": status.last_observed_at_ns,
        })
        .to_string(),
        occurred_at_ns: status.updated_at_ns,
        state_change_event_id: String::new(),
        source_type: "gnmi".to_string(),
    }
}

/// Structured gNMI coverage (M5.2B): an observation satisfies an
/// expectation only through exact structured path evidence — the
/// resolved notification path(s) carried on the update must contain
/// the parsed expectation path (origin-compatible, element-exact with
/// unkeyed elements acting as instance wildcards). Expectations or
/// How a single observation relates to one tracked expectation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CoverageMatch {
    /// Positive evidence: the observation covers the expectation.
    Observe,
    /// Complete delete evidence: the tombstone is the expectation path
    /// or a proper ancestor — the whole expectation scope is absent.
    DeleteComplete,
    /// Qualified delete evidence: the tombstone lies INSIDE the
    /// expectation's subtree (one deleted instance/leaf). It must
    /// invalidate current coverage conservatively — a single descendant
    /// delete never proves complete subtree absence (M5.2B review B9).
    DeletePartial,
}

/// Structured coverage classification. Positive matches use
/// [`crate::gnmi_path::observation_covers`]; delete/tombstone evidence
/// uses [`crate::gnmi_path::delete_covers`] for complete absence and
/// falls back to `observation_covers` to detect in-scope descendant
/// deletes that only QUALIFY coverage. Observations that fail to parse,
/// and updates without structured paths, are explicit non-coverage;
/// string containment is never used.
fn coverage_match(
    expectation: &SubscriptionPathExpectation,
    update: &TelemetryUpdate,
) -> Option<CoverageMatch> {
    if update.gnmi_path.is_empty() {
        return None;
    }
    let Ok(expected) = crate::gnmi_path::parse_path(&expectation.origin, &expectation.path) else {
        return None;
    };
    if update.is_delete {
        let mut qualified = false;
        for obs in &update.gnmi_path {
            if crate::gnmi_path::delete_covers(&expected, obs) {
                return Some(CoverageMatch::DeleteComplete);
            }
            if crate::gnmi_path::observation_covers(&expected, obs) {
                qualified = true;
            }
        }
        qualified.then_some(CoverageMatch::DeletePartial)
    } else {
        update
            .gnmi_path
            .iter()
            .any(|obs| crate::gnmi_path::observation_covers(&expected, obs))
            .then_some(CoverageMatch::Observe)
    }
}

/// Session + protocol + collector fencing (M5.2B review B1): only
/// observations carrying the CURRENT authorized (plan_id, session_id)
/// pair from a gNMI producer AND stamped with the session's owning
/// authenticated collector may satisfy coverage. The current session is
/// established exclusively by authoritative lifecycle registration —
/// telemetry can never self-register, and payload-supplied identifiers
/// alone never establish authority: the collector stamp is applied by
/// the core at the authenticated ingest boundary. Legacy/empty
/// provenance, non-gNMI protocols, unauthenticated development-compat
/// ingestion (empty stamp vs verified collector session) and late data
/// from a superseded session are received telemetry but grant no
/// subscription coverage.
fn update_has_current_session(
    sessions: &SubscriptionSessionRegistry,
    update: &TelemetryUpdate,
) -> bool {
    if update.source_protocol != "gnmi" {
        return false;
    }
    sessions
        .current_session_provenance(&update.target)
        .is_some_and(|(plan_id, session_id, collector_id)| {
            !session_id.is_empty()
                && update.plan_id == plan_id
                && update.session_id == session_id
                && update.collector_id == collector_id
        })
}

fn tracked_key(target: &str, expectation: &SubscriptionPathExpectation) -> (String, String) {
    (
        target.to_string(),
        format!(
            "{}|{}|{}|{}",
            expectation.origin, expectation.mode, expectation.sample_interval_ns, expectation.path
        ),
    )
}

fn now_ns() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn expectation(path: &str, mode: &str) -> SubscriptionPathExpectation {
        SubscriptionPathExpectation {
            path: path.to_string(),
            origin: String::new(),
            mode: mode.to_string(),
            sample_interval_ns: 0,
        }
    }

    fn gnmi_update(path: &str, gnmi_path: &str) -> TelemetryUpdate {
        TelemetryUpdate {
            plan_id: String::new(),
            session_id: String::new(),
            source_protocol: "gnmi".to_string(),
            gnmi_path: crate::gnmi_path::parse_path("", gnmi_path)
                .map(|p| vec![p])
                .unwrap_or_default(),
            is_delete: false,
            collector_id: String::new(),
            target: "dut:57400".to_string(),
            vendor: "openconfig".to_string(),
            hostname: "dut".to_string(),
            role: String::new(),
            site: String::new(),
            timestamp_ns: 1,
            path: path.to_string(),
            value: json!({}),
        }
    }

    #[test]
    fn structured_path_expectation_matching_is_exact() {
        let update = gnmi_update(
            "interfaces/interface[name=ethernet-1/1]/state/counters/in-octets",
            "interfaces/interface[name=ethernet-1/1]/state/counters/in-octets",
        );
        // Container expectation is covered by a descendant leaf.
        assert_eq!(
            coverage_match(
                &expectation(
                    "interfaces/interface[name=ethernet-1/1]/state/counters",
                    "SAMPLE"
                ),
                &update
            ),
            Some(CoverageMatch::Observe)
        );
        // A different keyed instance never satisfies.
        assert_eq!(
            coverage_match(
                &expectation(
                    "interfaces/interface[name=ethernet-1/2]/state/counters",
                    "SAMPLE"
                ),
                &update
            ),
            None
        );
        // A deeper expectation is not covered by a shallower observation.
        assert_eq!(
            coverage_match(
                &expectation(
                    "interfaces/interface[name=ethernet-1/1]/state/counters/in-octets/extra",
                    "SAMPLE"
                ),
                &update
            ),
            None
        );
        // Malformed expectations are explicit non-coverage.
        assert_eq!(
            coverage_match(
                &expectation("interfaces/interface[name=ethernet-1/1", "SAMPLE"),
                &update
            ),
            None
        );
        // Updates without structured paths never satisfy.
        let mut bare = update.clone();
        bare.gnmi_path = Vec::new();
        assert_eq!(
            coverage_match(&expectation("interfaces", "SAMPLE"), &bare),
            None
        );
    }

    #[test]
    fn bgp_update_matches_network_instances_expectation() {
        let mut update = TelemetryUpdate {
            plan_id: String::new(),
            session_id: String::new(),
            source_protocol: String::new(),
            gnmi_path: Vec::new(),
            is_delete: false,
            collector_id: String::new(),
            target: "dut:57400".to_string(),
            vendor: "openconfig".to_string(),
            hostname: "dut".to_string(),
            role: String::new(),
            site: String::new(),
            timestamp_ns: 1,
            path: "network-instances".to_string(),
            value: json!({
                "network-instance": {
                    "protocols": {
                        "protocol": {
                            "bgp": {
                                "neighbors": {
                                    "neighbor": {
                                        "neighbor-address": "192.0.2.1",
                                        "state": {"session-state": "ESTABLISHED"}
                                    }
                                }
                            }
                        }
                    }
                }
            }),
        };
        update.gnmi_path = crate::gnmi_path::parse_path("", "network-instances")
            .map(|p| vec![p])
            .unwrap_or_default();

        assert_eq!(
            coverage_match(&expectation("network-instances", "ON_CHANGE"), &update),
            Some(CoverageMatch::Observe)
        );
        // Ancestor observation cannot cover a descendant expectation.
        assert_eq!(
            coverage_match(
                &expectation("network-instances/protocols/bgp", "ON_CHANGE"),
                &update
            ),
            None
        );
    }

    fn provenance_update(target: &str, plan_id: &str, session_id: &str) -> TelemetryUpdate {
        TelemetryUpdate {
            plan_id: plan_id.to_string(),
            session_id: session_id.to_string(),
            source_protocol: "gnmi".to_string(),
            gnmi_path: Vec::new(),
            is_delete: false,
            collector_id: String::new(),
            target: target.to_string(),
            vendor: String::new(),
            hostname: String::new(),
            role: String::new(),
            site: String::new(),
            timestamp_ns: 1,
            path: "interfaces".to_string(),
            value: json!({}),
        }
    }

    fn registered_sessions(entries: &[(&str, &str, &str)]) -> SubscriptionSessionRegistry {
        let reg = SubscriptionSessionRegistry::new();
        for (target, plan_id, session_id) in entries {
            use crate::subscription_sessions::state;
            for (st, seq) in [(state::SUBMITTED, 1), (state::SYNCHRONIZED, 2)] {
                reg.apply(
                    &crate::subscription_sessions::SessionLifecycleEvent {
                        collector_id: String::new(),
                        target: target.to_string(),
                        plan_id: plan_id.to_string(),
                        session_id: session_id.to_string(),
                        state: st.to_string(),
                        transition_seq: seq,
                        authority_epoch: 0,
                        assignment_generation: 0,
                        occurred_at_ns: 0,
                        detail: String::new(),
                        submitted_paths: Vec::new(),
                        acquisition: String::new(),
                    },
                    true,
                );
            }
        }
        reg
    }

    #[test]
    fn current_session_provenance_authorizes_coverage() {
        let sessions = registered_sessions(&[("dut:57400", "plan-1", "sess-a")]);
        assert!(update_has_current_session(
            &sessions,
            &provenance_update("dut:57400", "plan-1", "sess-a")
        ));
    }

    #[test]
    fn superseded_session_cannot_satisfy_current_coverage() {
        let sessions = registered_sessions(&[("dut:57400", "plan-1", "sess-b")]);
        // Same plan_id, previous session — late/reconnect data fenced.
        assert!(!update_has_current_session(
            &sessions,
            &provenance_update("dut:57400", "plan-1", "sess-a")
        ));
        // Different plan entirely.
        assert!(!update_has_current_session(
            &sessions,
            &provenance_update("dut:57400", "plan-2", "sess-b")
        ));
        // Legacy/empty provenance never satisfies a session-bound plan.
        assert!(!update_has_current_session(
            &sessions,
            &provenance_update("dut:57400", "", "")
        ));
        // Wrong target.
        assert!(!update_has_current_session(
            &sessions,
            &provenance_update("other:57400", "plan-1", "sess-b")
        ));
    }

    #[test]
    fn non_gnmi_protocol_cannot_establish_coverage() {
        // Even with exact session identity, a non-gNMI producer can
        // never satisfy gNMI subscription coverage.
        let sessions = registered_sessions(&[("dut:57400", "plan-1", "sess-a")]);
        let mut update = provenance_update("dut:57400", "plan-1", "sess-a");
        update.source_protocol = "syslog".to_string();
        assert!(!update_has_current_session(&sessions, &update));
        update.source_protocol = String::new();
        assert!(!update_has_current_session(&sessions, &update));
        // Submitted-but-not-synchronized sessions grant no coverage.
        let reg = SubscriptionSessionRegistry::new();
        use crate::subscription_sessions::state;
        reg.apply(
            &crate::subscription_sessions::SessionLifecycleEvent {
                collector_id: String::new(),
                target: "dut:57400".to_string(),
                plan_id: "plan-1".to_string(),
                session_id: "sess-a".to_string(),
                state: state::SUBMITTED.to_string(),
                transition_seq: 1,
                authority_epoch: 0,
                assignment_generation: 0,
                occurred_at_ns: 0,
                detail: String::new(),
                submitted_paths: Vec::new(),
                acquisition: String::new(),
            },
            true,
        );
        assert!(!update_has_current_session(
            &reg,
            &provenance_update("dut:57400", "plan-1", "sess-a")
        ));
    }

    #[test]
    fn sample_freshness_bound_is_mode_aware() {
        // SAMPLE paths get a bounded freshness window: 3x the sample
        // interval, floored at VERIFICATION_WINDOW.
        let mut sample = expectation("interfaces", "SAMPLE");
        sample.sample_interval_ns = 5_000_000_000; // 5s
        assert_eq!(
            sample_stale_after(&sample, VERIFICATION_WINDOW),
            Duration::from_secs(30)
        );
        sample.sample_interval_ns = 60_000_000_000; // 60s
        assert_eq!(
            sample_stale_after(&sample, VERIFICATION_WINDOW),
            Duration::from_secs(180)
        );
        // The floor dominates small intervals.
        sample.sample_interval_ns = 0;
        assert_eq!(
            sample_stale_after(&sample, Duration::from_millis(100)),
            Duration::from_millis(100)
        );
        // Mode matching is case-insensitive.
        let mut lower = expectation("interfaces", "sample");
        lower.sample_interval_ns = 0;
        assert_eq!(
            sample_stale_after(&lower, VERIFICATION_WINDOW),
            VERIFICATION_WINDOW
        );
        // ON_CHANGE never goes stale — quiet is valid.
        for mode in ["ON_CHANGE", "on_change", ""] {
            let mut e = expectation("interfaces", mode);
            e.sample_interval_ns = 1;
            assert_eq!(
                sample_stale_after(&e, VERIFICATION_WINDOW),
                Duration::MAX,
                "mode {mode}"
            );
        }
    }

    fn pending_gnmi_update(target: &str, plan_id: &str, session_id: &str) -> TelemetryUpdate {
        let mut u = provenance_update(target, plan_id, session_id);
        u.gnmi_path = crate::gnmi_path::parse_path(
            "",
            "interfaces/interface[name=ethernet-1/1]/state/counters/in-octets",
        )
        .map(|p| vec![p])
        .unwrap_or_default();
        u
    }

    #[test]
    fn pending_buffer_global_cap_binds_every_insertion() {
        let mut pending = PendingObservations::default();
        let now = Instant::now();
        // Grow MANY existing buckets past the global cap: every session
        // bucket is filled beyond its per-session cap (which pops its
        // own oldest), so `total` can only stay bounded if the global
        // cap is enforced on the existing-bucket growth path too.
        let per_session = PENDING_OBSERVATION_PER_SESSION_CAP;
        let sessions = PENDING_OBSERVATION_GLOBAL_CAP / per_session + 2;
        for s in 0..sessions {
            for _ in 0..(per_session + 8) {
                pending.note(
                    &pending_gnmi_update("dut:57400", &format!("plan-{s}"), &format!("sess-{s}")),
                    now,
                );
            }
        }
        // Hammer one existing bucket further — the growth branch must
        // still never let `total` exceed the global cap.
        for _ in 0..500 {
            pending.note(&pending_gnmi_update("dut:57400", "plan-0", "sess-0"), now);
        }
        assert!(pending.total <= PENDING_OBSERVATION_GLOBAL_CAP);
        assert!(pending.buckets.len() <= PENDING_OBSERVATION_SESSION_CAP);
        let accounted: usize = pending.buckets.values().map(|b| b.updates.len()).sum();
        assert_eq!(pending.total, accounted);
    }

    #[test]
    fn pending_buffer_expires_each_observation_by_receipt_time() {
        let mut pending = PendingObservations::default();
        pending.ttl = Duration::from_millis(250);
        let key = (
            "dut:57400".to_string(),
            "plan-1".to_string(),
            "sess-1".to_string(),
        );
        // Old evidence first, then NEWER evidence for the same session:
        // bucket.last_seen stays fresh, but the old observation must
        // still expire on its own receipt instant.
        pending.note(
            &pending_gnmi_update("dut:57400", "plan-1", "sess-1"),
            Instant::now() - Duration::from_millis(400),
        );
        pending.note(
            &pending_gnmi_update("dut:57400", "plan-1", "sess-1"),
            Instant::now(),
        );
        let sessions = SubscriptionSessionRegistry::new();
        assert!(pending.sweep(&sessions).is_empty());
        assert_eq!(pending.total, 1, "only the fresh observation survives");
        // Immediate pre-release expiry: evidence aged past TTL while
        // waiting can never be released even if take() runs first.
        pending.note(
            &pending_gnmi_update("dut:57400", "plan-1", "sess-1"),
            Instant::now() - Duration::from_secs(120),
        );
        let live = pending.take(&key);
        assert_eq!(live.len(), 1, "expired evidence filtered at release");
        assert!(pending.total <= 1);
    }

    #[test]
    fn pending_take_preserves_arrival_order_and_receipt_times() {
        let mut pending = PendingObservations::default();
        let key = (
            "dut:57400".to_string(),
            "plan-1".to_string(),
            "sess-1".to_string(),
        );
        let t0 = Instant::now();
        let mut upd = pending_gnmi_update("dut:57400", "plan-1", "sess-1");
        upd.is_delete = false;
        pending.note(&upd, t0);
        let mut del = pending_gnmi_update("dut:57400", "plan-1", "sess-1");
        del.is_delete = true;
        pending.note(&del, t0 + Duration::from_millis(1));
        let live = pending.take(&key);
        assert_eq!(live.len(), 2);
        // Buffered update/delete ordering is preserved for replay.
        assert!(!live[0].0.is_delete);
        assert!(live[1].0.is_delete);
        // Original trusted receipt instants survive the round trip.
        assert_eq!(live[0].1, t0);
        assert_eq!(live[1].1, t0 + Duration::from_millis(1));
    }

    #[test]
    fn pending_buffer_rejects_oversized_evidence() {
        let mut pending = PendingObservations::default();
        let mut u = pending_gnmi_update("dut:57400", "plan-1", "sess-1");
        // Byte-budget violation: one enormous element name.
        let mut huge = u.gnmi_path[0].clone();
        huge.elem[0].name = "x".repeat(PENDING_OBSERVATION_MAX_EVIDENCE_BYTES);
        u.gnmi_path = vec![huge];
        pending.note(&u, Instant::now());
        assert_eq!(pending.total, 0);
        assert!(pending.buckets.is_empty());
        // Shape-cap violation: more resolved paths than the budget.
        let mut u2 = pending_gnmi_update("dut:57400", "plan-1", "sess-1");
        let one = u2.gnmi_path[0].clone();
        u2.gnmi_path = vec![one; PENDING_OBSERVATION_MAX_PATHS + 1];
        pending.note(&u2, Instant::now());
        assert_eq!(pending.total, 0);
        assert!(pending.buckets.is_empty());
    }

    #[tokio::test]
    async fn replayed_pending_evidence_keeps_original_receipt_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = crate::graph::GraphStore::open(
            dir.path().join("g.lbug").to_str().unwrap(),
            256 * 1024 * 1024,
        )
        .expect("open graph store");
        let sessions = registered_sessions(&[("dut:57400", "plan-1", "sess-1")]);
        let exp = expectation(
            "interfaces/interface[name=ethernet-1/1]/state/counters/in-octets",
            "SAMPLE",
        );
        let key = tracked_key("dut:57400", &exp);
        let mut tracked: HashMap<(String, String), TrackedPath> = HashMap::new();
        tracked.insert(
            key.clone(),
            TrackedPath {
                expectation: exp,
                plan_id: "plan-1".to_string(),
                session_id: "sess-1".to_string(),
                status: "pending".to_string(),
                durable_status: String::new(),
                durable_last_observed_at_ns: 0,
                first_observed_at_ns: 0,
                last_observed_at_ns: 0,
                deadline: Instant::now() + Duration::from_secs(60),
                last_seen: Instant::now(),
                last_evidence_at: None,
                stale_after: Duration::from_secs(30),
            },
        );
        let mut pending = PendingObservations::default();
        // Evidence received ~80s ago: covering update then delete.
        let t0 = Instant::now() - Duration::from_secs(80);
        pending.note(&pending_gnmi_update("dut:57400", "plan-1", "sess-1"), t0);
        let mut del = pending_gnmi_update("dut:57400", "plan-1", "sess-1");
        del.is_delete = true;
        pending.note(&del, t0 + Duration::from_secs(1));

        // Session already current: sweep releases in arrival order and
        // each replayed update passes its ORIGINAL receipt instant.
        let ready = pending.sweep(&sessions);
        assert_eq!(ready.len(), 1);
        let mut pending_writes: VecDeque<crate::graph::SubscriptionStatusWrite> = VecDeque::new();
        for (update, received_at) in pending.take(&ready[0]) {
            observe_update(
                &store,
                &mut tracked,
                &sessions,
                &mut pending,
                &mut pending_writes,
                &update,
                received_at,
            )
            .await;
        }
        let tp = tracked.get(&key).expect("tracked path");
        // Update→delete replay order yields "deleted", and last_seen
        // reflects the ~80s-old receipt — replay did NOT renew freshness.
        assert_eq!(tp.status, "deleted");
        assert!(
            tp.last_seen.elapsed() >= Duration::from_secs(75),
            "freshness must use original receipt time, not replay time"
        );
    }

    #[test]
    fn pending_sweep_fences_foreign_and_discards_terminal_sessions() {
        use crate::subscription_sessions::{SessionLifecycleEvent, state};
        let ev = |session_id: &str, st: &str, seq: u64, verified_collector: &str| {
            SessionLifecycleEvent {
                collector_id: verified_collector.to_string(),
                target: "dut:57400".to_string(),
                plan_id: "plan-1".to_string(),
                session_id: session_id.to_string(),
                state: st.to_string(),
                transition_seq: seq,
                authority_epoch: 0,
                assignment_generation: 0,
                occurred_at_ns: 0,
                detail: String::new(),
                submitted_paths: Vec::new(),
                acquisition: String::new(),
            }
        };
        let mut pending = PendingObservations::default();
        pending.note(
            &pending_gnmi_update("dut:57400", "plan-1", "sess-old"),
            Instant::now(),
        );
        let sessions = SubscriptionSessionRegistry::new();

        // A foreign collector claims the SAME plan/session identifiers
        // but its lifecycle is unverified — never current, never ready.
        sessions.apply(
            &ev("sess-old", state::SUBMITTED, 1, "other-collector"),
            false,
        );
        assert!(pending.sweep(&sessions).is_empty());
        assert_eq!(pending.total, 1, "unverified claim must not release");

        // The authorized session is a DIFFERENT session id for the same
        // plan. Entering the live lifecycle supersedes sess-old's record
        // (terminal), so its buffered evidence is fenced AND discarded —
        // it can never be released under the sibling session.
        for (st, seq) in [
            (state::SUBMITTED, 1),
            (state::SYNCHRONIZING, 2),
            (state::SYNCHRONIZED, 3),
        ] {
            sessions.apply(&ev("sess-new", st, seq, "owner-collector"), true);
        }
        assert!(pending.sweep(&sessions).is_empty());
        assert_eq!(pending.total, 0, "superseded session evidence discarded");
        assert!(pending.buckets.is_empty());
    }

    /// Delegating store that fails subscription-status writes for ONE
    /// configured path — per-path fault injection for durable-ack
    /// identity (M5.2B review E).
    struct SelectiveFailStore {
        inner: crate::graph::GraphStore,
        fail_path: std::sync::Mutex<Option<String>>,
    }

    #[tonic::async_trait]
    impl BonsaiStore for SelectiveFailStore {
        fn db(&self) -> Arc<lbug::Database> {
            BonsaiStore::db(&self.inner)
        }
        fn write_lock(&self) -> Arc<std::sync::Mutex<()>> {
            BonsaiStore::write_lock(&self.inner)
        }
        fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<crate::graph::BonsaiEvent> {
            BonsaiStore::subscribe_events(&self.inner)
        }
        async fn write(&self, u: TelemetryUpdate) -> anyhow::Result<()> {
            BonsaiStore::write(&self.inner, u).await
        }
        async fn write_detection(
            &self,
            a: String,
            b: String,
            c: String,
            d: String,
            e: String,
            f: i64,
            g: i64,
            h: String,
            i: Vec<String>,
        ) -> anyhow::Result<String> {
            BonsaiStore::write_detection(&self.inner, a, b, c, d, e, f, g, h, i).await
        }
        async fn write_remediation(
            &self,
            a: String,
            b: String,
            c: String,
            d: String,
            e: i64,
            f: i64,
        ) -> anyhow::Result<String> {
            BonsaiStore::write_remediation(&self.inner, a, b, c, d, e, f).await
        }
        async fn sync_sites_from_targets(
            &self,
            t: Vec<crate::config::TargetConfig>,
        ) -> anyhow::Result<()> {
            BonsaiStore::sync_sites_from_targets(&self.inner, t).await
        }
        async fn list_sites(&self) -> anyhow::Result<Vec<crate::graph::SiteRecord>> {
            BonsaiStore::list_sites(&self.inner).await
        }
        async fn upsert_site(
            &self,
            site: crate::graph::SiteRecord,
        ) -> anyhow::Result<crate::graph::SiteRecord> {
            BonsaiStore::upsert_site(&self.inner, site).await
        }
        async fn write_subscription_status(
            &self,
            status: crate::graph::SubscriptionStatusWrite,
        ) -> anyhow::Result<()> {
            if self.fail_path.lock().unwrap().as_deref() == Some(status.path.as_str()) {
                anyhow::bail!("injected per-path status write failure");
            }
            BonsaiStore::write_subscription_status(&self.inner, status).await
        }
        fn publish_event(&self, e: crate::graph::BonsaiEvent) {
            BonsaiStore::publish_event(&self.inner, e)
        }
    }

    /// M5.2B review E: a successful queued write acknowledges ONLY its
    /// exact row identity. Two paths on one device both desire
    /// `observed`; path B's writes keep failing while A's succeed —
    /// A becomes durable, B stays dirty and converges only after its
    /// own failure clears.
    #[tokio::test]
    async fn durable_ack_is_exact_row_identity_not_device_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SelectiveFailStore {
            inner: crate::graph::GraphStore::open(
                dir.path().join("g.lbug").to_str().unwrap(),
                256 * 1024 * 1024,
            )
            .expect("open graph store"),
            fail_path: std::sync::Mutex::new(Some(
                "interfaces/interface[name=ethernet-1/2]/state/counters/in-octets".to_string(),
            )),
        };
        let sessions = registered_sessions(&[("dut:57400", "plan-1", "sess-1")]);
        let mut tracked: HashMap<(String, String), TrackedPath> = HashMap::new();
        let mut pending_writes: VecDeque<crate::graph::SubscriptionStatusWrite> = VecDeque::new();
        let now = now_ns();
        for path in [
            "interfaces/interface[name=ethernet-1/1]/state/counters/in-octets",
            "interfaces/interface[name=ethernet-1/2]/state/counters/in-octets",
        ] {
            let exp = expectation(path, "ON_CHANGE");
            let key = tracked_key("dut:57400", &exp);
            tracked.insert(
                key.clone(),
                TrackedPath {
                    expectation: exp,
                    plan_id: "plan-1".to_string(),
                    session_id: "sess-1".to_string(),
                    status: "observed".to_string(),
                    durable_status: "pending".to_string(),
                    durable_last_observed_at_ns: 0,
                    first_observed_at_ns: now - 1000,
                    last_observed_at_ns: now,
                    deadline: Instant::now() + Duration::from_secs(60),
                    last_seen: Instant::now(),
                    last_evidence_at: None,
                    stale_after: Duration::MAX,
                },
            );
            let tp = tracked.get(&key).unwrap();
            pending_writes.push_back(status_write_for(&key, tp, now));
        }

        // First reconcile: A's queued write lands, B's keeps failing.
        reconcile_statuses(&store, &sessions, &mut tracked, &mut pending_writes).await;
        let key_a = tracked_key(
            "dut:57400",
            &expectation(
                "interfaces/interface[name=ethernet-1/1]/state/counters/in-octets",
                "ON_CHANGE",
            ),
        );
        let key_b = tracked_key(
            "dut:57400",
            &expectation(
                "interfaces/interface[name=ethernet-1/2]/state/counters/in-octets",
                "ON_CHANGE",
            ),
        );
        let a = tracked.get(&key_a).unwrap();
        let b = tracked.get(&key_b).unwrap();
        assert_eq!(a.durable_status, "observed", "A's own write acknowledged");
        assert_eq!(
            a.durable_last_observed_at_ns, now,
            "A's durable timestamp is the snapshot its write carried"
        );
        assert_ne!(
            b.durable_status, "observed",
            "B must NOT be marked durable by A's successful write"
        );

        // B's failure clears: the next reconcile converges B without
        // any new telemetry.
        *store.fail_path.lock().unwrap() = None;
        reconcile_statuses(&store, &sessions, &mut tracked, &mut pending_writes).await;
        let b = tracked.get(&key_b).unwrap();
        assert_eq!(b.durable_status, "observed");
        assert_eq!(b.durable_last_observed_at_ns, now);
    }
}
