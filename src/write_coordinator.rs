use anyhow::{Context, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

/// Sent by the write coordinator when an unmatched detection fires and
/// `auto_investigate_unmatched` is enabled. The HTTP layer consumes this
/// after AppState is available to create and spawn the investigation.
#[derive(Debug)]
pub struct AutoInvestigateRequest {
    pub detection_id: String,
    pub device_address: String,
    pub rule_id: String,
}

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
        source_event_ids: Vec<String>,
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
    /// When Some, auto-proposals are enabled and this library is consulted on every Detection write.
    pub playbook_library: Option<std::sync::Arc<crate::playbook::PlaybookLibrary>>,
    pub auto_propose: bool,
    /// When Some, unmatched detections are forwarded here so the HTTP layer
    /// can spawn an AI investigation after AppState is available.
    pub investigation_tx: Option<mpsc::Sender<AutoInvestigateRequest>>,
}

impl Default for WriteCoordinatorConfig {
    fn default() -> Self {
        Self {
            batch_size: 256,
            flush_interval: Duration::from_secs(1),
            queue_capacity: 4096,
            governor: None,
            playbook_library: None,
            auto_propose: false,
            investigation_tx: None,
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

        // G6: Track queue saturation events
        let fill_pct = (current_depth as u64 * 100) / (self.capacity as u64);
        if fill_pct >= 95 {
            metrics::counter!("bonsai_queue_saturation_total", "threshold" => "95pct").increment(1);
        } else if fill_pct >= 80 {
            metrics::counter!("bonsai_queue_saturation_total", "threshold" => "80pct").increment(1);
        }
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
                Some(PriorityWriteRequest::Detection { device_address, rule_id, severity, features_json, source_types_json, latency_ns, fired_at_ns, state_change_event_id, source_event_ids, reply_to }) => {
                    // Flush pending telemetry so the detection sees the latest graph state.
                    if !telemetry_batch.is_empty() {
                        flush_telemetry_batch(&store, &mut telemetry_batch).await;
                    }
                    if !sub_status_pending.is_empty() {
                        flush_sub_status_batch(&store, &mut sub_status_pending).await;
                    }
                    let res = store.write_detection(device_address.clone(), rule_id.clone(), severity, features_json.clone(), source_types_json, latency_ns, fired_at_ns, state_change_event_id, source_event_ids).await;
                    let vendor = extract_vendor(&features_json);
                    let playbook_matched = if cfg.auto_propose {
                        if let (Ok(det_id), Some(lib)) = (&res, &cfg.playbook_library) {
                            let vendor_ref = vendor.as_deref();
                            if let Some(pb) = lib.find(&rule_id, vendor_ref) {
                                let trust_key = crate::remediation::trust::TrustKey::new(
                                    &rule_id, "", "", &pb.name,
                                ).to_storage_key();
                                let steps = serde_json::to_string(&pb.steps)
                                    .unwrap_or_else(|_| "[]".to_string());
                                if let Err(e) = store.write_remediation_proposal(
                                    det_id.clone(),
                                    pb.name.clone(),
                                    trust_key,
                                    steps,
                                    "[]".to_string(),
                                    fired_at_ns,
                                ).await {
                                    warn!(error = %e, rule_id = %rule_id, "auto-proposal write failed");
                                }
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        // When auto_propose is off we still track whether a playbook
                        // exists so the auto-investigate gate works correctly.
                        cfg.playbook_library.as_ref()
                            .and_then(|lib| lib.find(&rule_id, vendor.as_deref()))
                            .is_some()
                    };

                    if let (Ok(det_id), Some(tx)) = (&res, &cfg.investigation_tx) {
                        if !playbook_matched {
                            let req = AutoInvestigateRequest {
                                detection_id: det_id.clone(),
                                device_address: device_address.clone(),
                                rule_id: rule_id.clone(),
                            };
                            if let Err(e) = tx.try_send(req) {
                                warn!(error = %e, rule_id = %rule_id, "auto-investigate channel full or closed");
                            }
                        }
                    }
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

fn extract_vendor(features_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(features_json).ok()?;
    v.get("vendor")
        .or_else(|| v.get("device_vendor"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
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
