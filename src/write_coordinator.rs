use anyhow::{Context, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::graph::{GraphStore, SubscriptionStatusWrite};
use crate::resource_governor::GovernorHandle;
use crate::telemetry::TelemetryUpdate;

static GLOBAL_QUEUE_DEPTH: AtomicUsize = AtomicUsize::new(0);
static GLOBAL_QUEUE_CAPACITY: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, Default)]
pub struct CoordinatorSnapshot {
    pub queue_depth: usize,
    pub queue_capacity: usize,
    pub queue_pct: u64,
}

pub fn snapshot() -> CoordinatorSnapshot {
    let depth = GLOBAL_QUEUE_DEPTH.load(Ordering::Relaxed);
    let capacity = GLOBAL_QUEUE_CAPACITY.load(Ordering::Relaxed);
    let pct = if capacity > 0 {
        (depth as u64 * 100) / capacity as u64
    } else {
        0
    };
    CoordinatorSnapshot {
        queue_depth: depth,
        queue_capacity: capacity,
        queue_pct: pct,
    }
}

/// High-throughput telemetry and subscription updates — batched.
pub enum WriteRequest {
    /// Batchable telemetry updates.
    Telemetry(TelemetryUpdate),
    /// Subscription status tracking — coalesced, never interrupts telemetry batch.
    SubscriptionStatus(SubscriptionStatusWrite),
}

/// Low-volume priority writes that need the freshest graph state.
/// Kept on a separate small channel so they never force-flush the telemetry batch.
pub enum PriorityWriteRequest {
    /// Detection events.
    Detection {
        device_address: String,
        rule_id: String,
        severity: String,
        features_json: String,
        source_types_json: String,
        latency_ns: i64,
        fired_at_ns: i64,
        state_change_event_id: String,
        /// Response sender for the generated ID.
        reply_to: oneshot::Sender<Result<String>>,
    },
    /// Remediation actions.
    Remediation {
        detection_id: String,
        action: String,
        status: String,
        detail_json: String,
        attempted_at_ns: i64,
        completed_at_ns: i64,
        /// Response sender for the generated ID.
        reply_to: oneshot::Sender<Result<String>>,
    },
}

#[derive(Clone)]
pub struct WriteCoordinatorConfig {
    pub batch_size: usize,
    pub flush_interval: Duration,
    pub queue_capacity: usize,
    /// When set, the coordinator consults write pressure and memory pressure flags
    /// to expand the effective batch size under load (C4-N2 / T6-1).
    pub governor: Option<GovernorHandle>,
}

impl Default for WriteCoordinatorConfig {
    fn default() -> Self {
        Self {
            batch_size: 256,
            flush_interval: Duration::from_secs(1),
            queue_capacity: 4096,
            governor: None,
        }
    }
}

pub struct WriteCoordinator {
    tx: mpsc::Sender<WriteRequest>,
    priority_tx: mpsc::Sender<PriorityWriteRequest>,
    depth: Arc<AtomicUsize>,
    capacity: usize,
}

impl WriteCoordinator {
    pub fn new(
        store: Arc<GraphStore>,
        cfg: WriteCoordinatorConfig,
        depth: Arc<AtomicUsize>,
    ) -> Self {
        let capacity = cfg.queue_capacity;
        GLOBAL_QUEUE_CAPACITY.store(capacity, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(capacity);
        let (priority_tx, priority_rx) = mpsc::channel(256);
        let depth_clone = Arc::clone(&depth);

        tokio::spawn(async move {
            run_coordinator(rx, priority_rx, store, cfg, depth_clone).await;
        });

        Self {
            tx,
            priority_tx,
            depth,
            capacity,
        }
    }

    pub async fn submit(&self, req: WriteRequest) -> Result<()> {
        let res = self
            .tx
            .send(req)
            .await
            .context("write coordinator channel closed");
        self.update_depth_metric();
        res
    }

    pub fn try_submit(&self, req: WriteRequest) -> Result<()> {
        let res = self
            .tx
            .try_send(req)
            .context("write coordinator channel full or closed");
        self.update_depth_metric();
        res
    }

    pub async fn submit_priority(&self, req: PriorityWriteRequest) -> Result<()> {
        self.priority_tx
            .send(req)
            .await
            .context("write coordinator priority channel closed")
    }

    pub fn queue_depth(&self) -> usize {
        self.depth.load(Ordering::Relaxed)
    }

    pub fn queue_fill_pct(&self) -> u64 {
        (self.queue_depth() as u64 * 100) / (self.capacity as u64)
    }

    fn update_depth_metric(&self) {
        let current_depth = self.capacity - self.tx.capacity();
        self.depth.store(current_depth, Ordering::Relaxed);
        metrics::gauge!("bonsai_write_coordinator_queue_depth").set(current_depth as f64);
    }
}

async fn run_coordinator(
    mut rx: mpsc::Receiver<WriteRequest>,
    mut priority_rx: mpsc::Receiver<PriorityWriteRequest>,
    store: Arc<GraphStore>,
    cfg: WriteCoordinatorConfig,
    depth: Arc<AtomicUsize>,
) {
    info!(
        batch_size = cfg.batch_size,
        flush_interval = ?cfg.flush_interval,
        "write coordinator started"
    );

    let mut telemetry_batch: Vec<TelemetryUpdate> = Vec::with_capacity(cfg.batch_size);
    // Subscription status writes are cheap MERGE upserts. Accumulating them here
    // and flushing alongside the telemetry batch (on timer or when batch is full)
    // prevents each subscription renewal from interrupting mid-batch writes.
    let mut sub_status_pending: Vec<SubscriptionStatusWrite> = Vec::with_capacity(64);
    let mut flush_timer = tokio::time::interval(cfg.flush_interval);
    flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        // Under write pressure, expand batch size to 2× to make each DB write
        // cover more rows and reduce per-write overhead (C4-N2 / T6-1).
        let effective_batch = cfg
            .governor
            .as_ref()
            .filter(|g| g.write_pressure_active())
            .map(|_| cfg.batch_size * 2)
            .unwrap_or(cfg.batch_size);

        // biased: priority_rx is checked first on every iteration so Detection/Remediation
        // writes are never starved, but they also never interrupt a filling telemetry batch
        // because they arrive on a separate channel.
        tokio::select! {
            biased;
            preq = priority_rx.recv() => match preq {
                Some(PriorityWriteRequest::Detection { device_address, rule_id, severity, features_json, source_types_json, latency_ns, fired_at_ns, state_change_event_id, reply_to }) => {
                    // Flush pending telemetry so the detection sees the latest graph state.
                    if !telemetry_batch.is_empty() {
                        flush_telemetry_batch(&store, &mut telemetry_batch).await;
                    }
                    if !sub_status_pending.is_empty() {
                        flush_sub_status_batch(&store, &mut sub_status_pending).await;
                    }
                    let res = store.write_detection(device_address, rule_id, severity, features_json, source_types_json, latency_ns, fired_at_ns, state_change_event_id).await;
                    let _ = reply_to.send(res);
                }
                Some(PriorityWriteRequest::Remediation { detection_id, action, status, detail_json, attempted_at_ns, completed_at_ns, reply_to }) => {
                    if !telemetry_batch.is_empty() {
                        flush_telemetry_batch(&store, &mut telemetry_batch).await;
                    }
                    if !sub_status_pending.is_empty() {
                        flush_sub_status_batch(&store, &mut sub_status_pending).await;
                    }
                    let res = store.write_remediation(detection_id, action, status, detail_json, attempted_at_ns, completed_at_ns).await;
                    let _ = reply_to.send(res);
                }
                None => break,
            },
            req = rx.recv() => match req {
                Some(WriteRequest::Telemetry(u)) => {
                    telemetry_batch.push(u);
                    if telemetry_batch.len() >= effective_batch {
                        flush_telemetry_batch(&store, &mut telemetry_batch).await;
                        flush_sub_status_batch(&store, &mut sub_status_pending).await;
                    }
                }
                Some(WriteRequest::SubscriptionStatus(s)) => {
                    // Coalesce — do not flush the telemetry batch on every subscription tick.
                    sub_status_pending.push(s);
                    if sub_status_pending.len() >= 128 {
                        flush_sub_status_batch(&store, &mut sub_status_pending).await;
                    }
                }
                None => break,
            },
            _ = flush_timer.tick() => {
                if !telemetry_batch.is_empty() {
                    flush_telemetry_batch(&store, &mut telemetry_batch).await;
                }
                if !sub_status_pending.is_empty() {
                    flush_sub_status_batch(&store, &mut sub_status_pending).await;
                }
            }
        }

        let current_depth = cfg.queue_capacity.saturating_sub(rx.capacity());
        depth.store(current_depth, Ordering::Relaxed);
        GLOBAL_QUEUE_DEPTH.store(current_depth, Ordering::Relaxed);
        metrics::gauge!("bonsai_write_coordinator_queue_depth").set(current_depth as f64);
    }

    info!("write coordinator stopping");
}

async fn flush_telemetry_batch(store: &Arc<GraphStore>, batch: &mut Vec<TelemetryUpdate>) {
    let updates = std::mem::replace(batch, Vec::with_capacity(batch.capacity()));
    if let Err(e) = store.write_batch(updates).await {
        warn!(error = %e, "telemetry batch write failed");
    }
}

async fn flush_sub_status_batch(
    store: &Arc<GraphStore>,
    pending: &mut Vec<SubscriptionStatusWrite>,
) {
    let writes = std::mem::replace(pending, Vec::with_capacity(pending.capacity()));
    for s in writes {
        if let Err(e) = store.write_subscription_status(s).await {
            warn!(error = %e, "subscription status write failed");
        }
    }
}
