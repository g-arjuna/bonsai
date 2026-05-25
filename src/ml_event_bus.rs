//! EV1-4 T1 — ML event bus: SSE channel for ML job progress, GNN alerts,
//! embedding completion, model activation.
//!
//! Python sidecar POSTs to `POST /api/ml/events/publish`. The Rust HTTP layer
//! fanouts to all SSE subscribers on `GET /api/ml/events/stream`.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Capacity for the ML event broadcast channel.
const ML_EVENT_BUS_CAPACITY: usize = 2048;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MlEvent {
    JobStarted {
        job_id: String,
        job_type: String,
        triggered_by: String,
    },
    JobProgress {
        job_id: String,
        step: i64,
        total_steps: i64,
        metric_name: String,
        metric_value: f64,
    },
    JobCompleted {
        job_id: String,
        job_type: String,
        outcome: String,
        val_auc: f64,
        val_f1: f64,
        model_path: String,
    },
    JobFailed {
        job_id: String,
        job_type: String,
        error: String,
    },
    ExportStarted {
        export_id: String,
        export_type: String,
        estimated_rows: i64,
    },
    ExportCompleted {
        export_id: String,
        row_count: i64,
        quality_passed: bool,
    },
    GnnInferenceCompleted {
        snapshot_ns: i64,
        anomalous_device_count: i64,
        top_score: f64,
        model_id: String,
    },
    GnnUncertainHighAlert {
        device_address: String,
        anomaly_score: f64,
        uncertainty_margin: f64,
    },
    EmbeddingBatchCompleted {
        events_embedded: i64,
        model_name: String,
        embedding_type: String,
    },
    ModelActivated {
        model_id: String,
        model_type: String,
        val_auc: f64,
    },
    TrainingReadinessChanged {
        export_type: String,
        was_ready: bool,
        is_ready: bool,
    },
    QueuePressure {
        queue_depth: i64,
        queue_capacity: i64,
        drops_total: i64,
    },
    JobDeadLetter {
        job_id: String,
        job_type: String,
        retries: i64,
        error: String,
    },
    JobRetryRequested {
        run_id: String,
        job_type: String,
    },
    MlScheduledJobFired {
        run_id: String,
        job_type: String,
        schedule_id: String,
        fired_at_ns: i64,
    },
}

/// Shared broadcast channel for ML events.
#[derive(Clone, Debug)]
pub struct MlEventBus {
    tx: broadcast::Sender<MlEvent>,
}

impl MlEventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(ML_EVENT_BUS_CAPACITY);
        Self { tx }
    }

    pub fn publish(&self, event: MlEvent) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MlEvent> {
        self.tx.subscribe()
    }

    pub fn sender(&self) -> broadcast::Sender<MlEvent> {
        self.tx.clone()
    }
}

impl Default for MlEventBus {
    fn default() -> Self {
        Self::new()
    }
}
