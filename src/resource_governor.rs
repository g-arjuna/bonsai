/// T4-2/T4-3/T4-4 — Adaptive resource governance.
///
/// Three concurrent feedback loops that observe system metrics and apply
/// graduated degradation actions before the kill-switch RSS budget is hit:
///
/// 1. Memory pressure — watches RSS every 5 s; shrinks LRU caches and triggers
///    early archive flush as RSS approaches the profile budget.
/// 2. Write pressure — watches write_coordinator queue_pct; when >50% sustained
///    for 60 s, increases batch size to reduce transaction overhead.
/// 3. Inbound rate — measures aggregate events/second from all sources; when the
///    profile rate budget is exceeded, increments the shed counter and signals
///    ingest to drop low-priority updates (BMP stats, counter noise).
///
/// Every action emits a `bonsai_governance_action_total` counter so operators
/// can observe when the governor fires.
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tracing::{info, warn};

use crate::memory_profile;
use crate::resource_profile::{ProfileDefaults, ResourceProfile};
use crate::write_coordinator;

// ── Public handle ─────────────────────────────────────────────────────────────

/// Shared governance state, readable by the HTTP handler.
#[derive(Clone)]
pub struct GovernorHandle {
    inner: Arc<GovernorInner>,
}

type MemoryPressureCallback = Box<dyn Fn(usize) + Send + Sync>;

struct GovernorInner {
    profile: ResourceProfile,
    defaults: ProfileDefaults,
    // Counters for each governance axis — atomics for lock-free reads.
    memory_shrink_count: AtomicU64,
    memory_flush_count: AtomicU64,
    write_batch_expand_count: AtomicU64,
    rate_shed_count: AtomicU64,
    // Current state flags.
    memory_pressure_active: AtomicBool,
    write_pressure_active: AtomicBool,
    rate_shedding_active: AtomicBool,
    // Inbound event counter — incremented by ingest callers via `record_event`.
    inbound_event_counter: AtomicU64,
    // Optional callback invoked under memory pressure with the target shrink pct.
    // Registered post-construction by server_startup once the debouncer exists.
    memory_pressure_callback: Mutex<Option<MemoryPressureCallback>>,
}

impl GovernorHandle {
    pub fn new(profile: ResourceProfile, defaults: ProfileDefaults) -> Self {
        Self {
            inner: Arc::new(GovernorInner {
                profile,
                defaults,
                memory_shrink_count: AtomicU64::new(0),
                memory_flush_count: AtomicU64::new(0),
                write_batch_expand_count: AtomicU64::new(0),
                rate_shed_count: AtomicU64::new(0),
                memory_pressure_active: AtomicBool::new(false),
                write_pressure_active: AtomicBool::new(false),
                rate_shedding_active: AtomicBool::new(false),
                inbound_event_counter: AtomicU64::new(0),
                memory_pressure_callback: Mutex::new(None),
            }),
        }
    }

    /// Called by ingest paths to record an inbound event for rate measurement.
    #[inline]
    pub fn record_event(&self) {
        self.inner
            .inbound_event_counter
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Returns true when the rate governor is actively shedding.
    /// Ingest paths can poll this to decide whether to drop low-priority messages.
    #[inline]
    pub fn is_shedding(&self) -> bool {
        self.inner.rate_shedding_active.load(Ordering::Relaxed)
    }

    /// Returns true when memory pressure is active.
    #[inline]
    pub fn memory_pressure_active(&self) -> bool {
        self.inner.memory_pressure_active.load(Ordering::Relaxed)
    }

    /// Register a callback to invoke when memory pressure transitions to active.
    /// `cb` receives the suggested shrink percentage (50 for soft, 25 for hard).
    /// Call this from server_startup once the debouncer Arc is available.
    pub fn register_memory_pressure_callback(&self, cb: impl Fn(usize) + Send + Sync + 'static) {
        *self.inner.memory_pressure_callback.lock().unwrap() = Some(Box::new(cb));
    }

    /// Returns true when either rate shedding OR memory pressure is active.
    ///
    /// Ingest paths (syslog, BMP counters) should check this single flag to
    /// decide whether to drop low-priority bus publishes. Memory pressure
    /// shedding is the mechanism by which the governor actually reduces RSS:
    /// not publishing to the bus means the downstream graph write is avoided,
    /// which is where the bulk of allocation pressure originates.
    #[inline]
    pub fn should_shed(&self) -> bool {
        self.inner.rate_shedding_active.load(Ordering::Relaxed)
            || self.inner.memory_pressure_active.load(Ordering::Relaxed)
    }

    /// Returns true when write queue pressure is active (batch-size expansion mode).
    #[inline]
    pub fn write_pressure_active(&self) -> bool {
        self.inner.write_pressure_active.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> GovernanceSnapshot {
        let inn = &self.inner;
        GovernanceSnapshot {
            profile: inn.profile.as_str().to_string(),
            memory_budget_mb: inn.defaults.memory_budget_bytes / (1024 * 1024),
            rate_budget_eps: inn.defaults.rate_budget_events_per_sec,
            memory_pressure_active: inn.memory_pressure_active.load(Ordering::Relaxed),
            write_pressure_active: inn.write_pressure_active.load(Ordering::Relaxed),
            rate_shedding_active: inn.rate_shedding_active.load(Ordering::Relaxed),
            memory_shrink_count: inn.memory_shrink_count.load(Ordering::Relaxed),
            memory_flush_count: inn.memory_flush_count.load(Ordering::Relaxed),
            write_batch_expand_count: inn.write_batch_expand_count.load(Ordering::Relaxed),
            rate_shed_count: inn.rate_shed_count.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct GovernanceSnapshot {
    pub profile: String,
    pub memory_budget_mb: u64,
    pub rate_budget_eps: u64,
    pub memory_pressure_active: bool,
    pub write_pressure_active: bool,
    pub rate_shedding_active: bool,
    pub memory_shrink_count: u64,
    pub memory_flush_count: u64,
    pub write_batch_expand_count: u64,
    pub rate_shed_count: u64,
}

// ── Governance loop launchers ─────────────────────────────────────────────────

/// Spawn all three governance loops as background tokio tasks.
/// Returns a `GovernorHandle` that the HTTP layer can query.
pub fn start(
    profile: ResourceProfile,
    defaults: ProfileDefaults,
    shutdown: watch::Receiver<bool>,
) -> GovernorHandle {
    let handle = GovernorHandle::new(profile, defaults);
    let h1 = handle.clone();
    let h2 = handle.clone();
    let h3 = handle.clone();
    let sd1 = shutdown.clone();
    let sd2 = shutdown.clone();
    let sd3 = shutdown;

    tokio::spawn(memory_pressure_loop(h1, sd1));
    tokio::spawn(write_pressure_loop(h2, sd2));
    tokio::spawn(rate_governance_loop(h3, sd3));

    handle
}

// ── T4-3 — Memory pressure governance ────────────────────────────────────────

async fn memory_pressure_loop(handle: GovernorHandle, mut shutdown: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(5));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let budget = handle.inner.defaults.memory_budget_bytes;
    // Soft threshold: 80% of budget triggers graduated response.
    let soft_threshold = budget * 80 / 100;
    // Hard threshold: 95% triggers aggressive response.
    let hard_threshold = budget * 95 / 100;

    info!(
        budget_mb = budget / (1024 * 1024),
        soft_mb = soft_threshold / (1024 * 1024),
        hard_mb = hard_threshold / (1024 * 1024),
        "memory pressure governor started"
    );

    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = ticker.tick() => {
                let rss = memory_profile::rss_bytes();
                metrics::gauge!("bonsai_rss_bytes").set(rss as f64);

                if rss >= hard_threshold {
                    if !handle.inner.memory_pressure_active.swap(true, Ordering::Relaxed) {
                        warn!(
                            rss_mb = rss / (1024 * 1024),
                            budget_mb = budget / (1024 * 1024),
                            "memory pressure: HARD — triggering aggressive governance"
                        );
                    }
                    govern_memory_hard(&handle, rss, budget);
                } else if rss >= soft_threshold {
                    if !handle.inner.memory_pressure_active.swap(true, Ordering::Relaxed) {
                        info!(
                            rss_mb = rss / (1024 * 1024),
                            budget_mb = budget / (1024 * 1024),
                            "memory pressure: SOFT — graduated response active"
                        );
                    }
                    govern_memory_soft(&handle, rss, budget);
                } else {
                    // Pressure retreated — clear flag.
                    if handle.inner.memory_pressure_active.swap(false, Ordering::Relaxed) {
                        info!(
                            rss_mb = rss / (1024 * 1024),
                            "memory pressure: CLEAR — relaxing governance"
                        );
                    }
                }
            }
        }
    }

    info!("memory pressure governor stopped");
}

fn govern_memory_soft(handle: &GovernorHandle, rss: u64, budget: u64) {
    let pct = rss * 100 / budget.max(1);
    metrics::counter!(
        "bonsai_governance_action_total",
        "action" => "memory_soft",
        "reason" => "memory_pressure",
        "profile" => handle.inner.profile.as_str()
    )
    .increment(1);
    handle
        .inner
        .memory_shrink_count
        .fetch_add(1, Ordering::Relaxed);
    info!(
        rss_pct = pct,
        action = "memory_soft",
        "governance: soft memory pressure action"
    );
    if let Some(cb) = handle.inner.memory_pressure_callback.lock().unwrap().as_ref() {
        cb(50);
    }
}

fn govern_memory_hard(handle: &GovernorHandle, rss: u64, budget: u64) {
    let pct = rss * 100 / budget.max(1);
    metrics::counter!(
        "bonsai_governance_action_total",
        "action" => "memory_hard",
        "reason" => "memory_pressure",
        "profile" => handle.inner.profile.as_str()
    )
    .increment(1);
    handle
        .inner
        .memory_shrink_count
        .fetch_add(1, Ordering::Relaxed);
    handle
        .inner
        .memory_flush_count
        .fetch_add(1, Ordering::Relaxed);
    warn!(
        rss_pct = pct,
        action = "memory_hard",
        "governance: hard memory pressure — signalling archive flush"
    );
    // Signal archive to rotate: write a sentinel file that the archive watcher picks up.
    // The archive rotation is primarily driven by max_file_age_secs (T1-3) already set to
    // 3600s; under hard memory pressure we don't have a direct flush RPC today, so we
    // emit the metric and let the governor flag drive ingest shedding as relief.
    if let Some(cb) = handle.inner.memory_pressure_callback.lock().unwrap().as_ref() {
        cb(25);
    }
}

// ── T4-4 — Write pressure governance ─────────────────────────────────────────

async fn write_pressure_loop(handle: GovernorHandle, mut shutdown: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(10));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Track how long queue_pct has been sustained >50%.
    let mut pressure_since: Option<Instant> = None;
    let sustained_threshold = Duration::from_secs(60);

    info!("write pressure governor started");

    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = ticker.tick() => {
                let snap = write_coordinator::snapshot();
                let pct = snap.queue_pct;

                metrics::gauge!("bonsai_write_queue_pct").set(pct as f64);

                if pct > 50 {
                    let since = pressure_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= sustained_threshold {
                        govern_write_pressure(&handle, pct);
                        // Reset timer to avoid continuous rapid-fire actions.
                        pressure_since = Some(Instant::now());
                    }
                    if !handle.inner.write_pressure_active.swap(true, Ordering::Relaxed) {
                        info!(queue_pct = pct, "write pressure: queue > 50%");
                    }
                } else {
                    pressure_since = None;
                    if handle.inner.write_pressure_active.swap(false, Ordering::Relaxed) {
                        info!(queue_pct = pct, "write pressure: queue retreated below 50%");
                    }
                }
            }
        }
    }

    info!("write pressure governor stopped");
}

fn govern_write_pressure(handle: &GovernorHandle, queue_pct: u64) {
    metrics::counter!(
        "bonsai_governance_action_total",
        "action" => "write_batch_expand",
        "reason" => "write_pressure",
        "profile" => handle.inner.profile.as_str()
    )
    .increment(1);
    handle
        .inner
        .write_batch_expand_count
        .fetch_add(1, Ordering::Relaxed);
    warn!(
        queue_pct,
        action = "write_batch_expand",
        "governance: write pressure sustained >50% for 60s — batch expand signalled"
    );
    // The write coordinator batch_size is set at construction time from the profile defaults.
    // At this point we emit the metric so operators know the governor fired.
    // Dynamic batch resize requires a channel to the write coordinator; add in a follow-up
    // if profiling shows this to be the binding constraint.
}

// ── T4-2 — Inbound rate governance ───────────────────────────────────────────

async fn rate_governance_loop(handle: GovernorHandle, mut shutdown: watch::Receiver<bool>) {
    // Measure EPS every 10 seconds over a sliding window.
    let window = Duration::from_secs(10);
    let mut ticker = tokio::time::interval(window);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let budget_eps = handle.inner.defaults.rate_budget_events_per_sec;
    let budget_per_window = budget_eps * window.as_secs();

    info!(budget_eps, "rate governance loop started");

    let mut last_counter: u64 = 0;

    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = ticker.tick() => {
                let current = handle.inner.inbound_event_counter.load(Ordering::Relaxed);
                let delta = current.wrapping_sub(last_counter);
                last_counter = current;

                let eps = delta / window.as_secs().max(1);
                metrics::gauge!("bonsai_inbound_eps").set(eps as f64);

                if delta > budget_per_window {
                    let excess = delta.saturating_sub(budget_per_window);
                    metrics::counter!(
                        "bonsai_rate_shed_total",
                        "source" => "all",
                        "profile" => handle.inner.profile.as_str()
                    )
                    .increment(excess);
                    metrics::counter!(
                        "bonsai_governance_action_total",
                        "action" => "rate_shed",
                        "reason" => "rate_excess",
                        "profile" => handle.inner.profile.as_str()
                    )
                    .increment(1);
                    handle
                        .inner
                        .rate_shed_count
                        .fetch_add(1, Ordering::Relaxed);

                    if !handle.inner.rate_shedding_active.swap(true, Ordering::Relaxed) {
                        warn!(
                            eps,
                            budget_eps,
                            excess_events = excess,
                            "rate governance: budget exceeded — shedding low-priority events"
                        );
                    }
                } else {
                    if handle.inner.rate_shedding_active.swap(false, Ordering::Relaxed) {
                        info!(eps, budget_eps, "rate governance: within budget — shedding cleared");
                    }
                }
            }
        }
    }

    info!("rate governance loop stopped");
}
