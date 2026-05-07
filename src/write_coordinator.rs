use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};
use anyhow::{Result, Context};

use crate::telemetry::TelemetryUpdate;
use crate::graph::{GraphStore, SubscriptionStatusWrite};

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
    let pct = if capacity > 0 { (depth as u64 * 100) / capacity as u64 } else { 0 };
    CoordinatorSnapshot { queue_depth: depth, queue_capacity: capacity, queue_pct: pct }
}

/// A request to write data to the graph database.
pub enum WriteRequest {
    /// Batchable telemetry updates.
    Telemetry(TelemetryUpdate),
    /// Subscription status tracking.
    SubscriptionStatus(SubscriptionStatusWrite),
    /// Detection events.
    Detection {
        device_address: String,
        rule_id: String,
        severity: String,
        features_json: String,
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

#[derive(Clone, Debug)]
pub struct WriteCoordinatorConfig {
    pub batch_size: usize,
    pub flush_interval: Duration,
    pub queue_capacity: usize,
}

impl Default for WriteCoordinatorConfig {
    fn default() -> Self {
        Self {
            batch_size: 256,
            flush_interval: Duration::from_secs(1),
            queue_capacity: 4096,
        }
    }
}

pub struct WriteCoordinator {
    tx: mpsc::Sender<WriteRequest>,
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
        let depth_clone = Arc::clone(&depth);
        
        tokio::spawn(async move {
            run_coordinator(rx, store, cfg, depth_clone).await;
        });

        Self { tx, depth, capacity }
    }

    pub async fn submit(&self, req: WriteRequest) -> Result<()> {
        let res = self.tx.send(req).await.context("write coordinator channel closed");
        self.update_depth_metric();
        res
    }

    pub fn try_submit(&self, req: WriteRequest) -> Result<()> {
        let res = self.tx.try_send(req).context("write coordinator channel full or closed");
        self.update_depth_metric();
        res
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
    let mut flush_timer = tokio::time::interval(cfg.flush_interval);
    flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            req = rx.recv() => match req {
                Some(WriteRequest::Telemetry(u)) => {
                    telemetry_batch.push(u);
                    if telemetry_batch.len() >= cfg.batch_size {
                        flush_telemetry_batch(&store, &mut telemetry_batch).await;
                    }
                }
                Some(WriteRequest::SubscriptionStatus(s)) => {
                    if !telemetry_batch.is_empty() {
                        flush_telemetry_batch(&store, &mut telemetry_batch).await;
                    }
                    if let Err(e) = store.write_subscription_status(s).await {
                        warn!(error = %e, "subscription status write failed");
                    }
                }
                Some(WriteRequest::Detection { device_address, rule_id, severity, features_json, fired_at_ns, state_change_event_id, reply_to }) => {
                    if !telemetry_batch.is_empty() {
                        flush_telemetry_batch(&store, &mut telemetry_batch).await;
                    }
                    let res = store.write_detection(device_address, rule_id, severity, features_json, fired_at_ns, state_change_event_id).await;
                    let _ = reply_to.send(res);
                }
                Some(WriteRequest::Remediation { detection_id, action, status, detail_json, attempted_at_ns, completed_at_ns, reply_to }) => {
                    if !telemetry_batch.is_empty() {
                        flush_telemetry_batch(&store, &mut telemetry_batch).await;
                    }
                    let res = store.write_remediation(detection_id, action, status, detail_json, attempted_at_ns, completed_at_ns).await;
                    let _ = reply_to.send(res);
                }
                None => break,
            },
            _ = flush_timer.tick() => {
                if !telemetry_batch.is_empty() {
                    flush_telemetry_batch(&store, &mut telemetry_batch).await;
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

async fn flush_telemetry_batch(
    store: &Arc<GraphStore>,
    batch: &mut Vec<TelemetryUpdate>,
) {
    let updates = std::mem::replace(batch, Vec::with_capacity(batch.capacity()));
    if let Err(e) = store.write_batch(updates).await {
        warn!(error = %e, "telemetry batch write failed");
    }
}

