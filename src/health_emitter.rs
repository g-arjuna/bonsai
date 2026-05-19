//! Health event emitter for external monitoring integration.
//!
//! Emits health events to the event bus for consumption by output adapters
//! (syslog, PagerDuty, OpsGenie, etc.) on:
//! - Collector connect/disconnect
//! - Graph write failures
//! - Queue saturation
//! - Resource governor violations
//! - Enricher failures

use crate::event_bus::InProcessBus;
use crate::telemetry::TelemetryUpdate;

#[derive(Clone, Debug)]
pub struct HealthEvent {
    pub event_type: HealthEventType,
    pub severity: HealthSeverity,
    pub component: String,
    pub message: String,
    pub metadata: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HealthEventType {
    CollectorConnected,
    CollectorDisconnected,
    CollectorHeartbeatStale,
    GraphWriteFailed,
    QueueSaturation,
    GovernorViolation,
    EnricherFailed,
    SidecarLost,
    DiskSpaceCritical,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HealthSeverity {
    Info,
    Warning,
    Critical,
}

pub struct HealthEmitter {
    bus: Arc<InProcessBus>,
}

impl HealthEmitter {
    pub fn new(bus: Arc<InProcessBus>) -> Self {
        Self { bus }
    }

    pub fn emit(&self, event: HealthEvent) {
        let event_json = serde_json::to_string(&event).unwrap_or_default();
        let update = TelemetryUpdate {
            source: "health-emitter".to_string(),
            device_address: None,
            event_type: "health_event".to_string(),
            payload: Some(event_json),
            timestamp_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0),
        };
        self.bus.publish(update);
    }

    pub fn emit_collector_connected(&self, collector_id: &str, hostname: &str) {
        self.emit(HealthEvent {
            event_type: HealthEventType::CollectorConnected,
            severity: HealthSeverity::Info,
            component: "collector".to_string(),
            message: format!("Collector {} ({}) connected", collector_id, hostname),
            metadata: vec![
                ("collector_id".to_string(), collector_id.to_string()),
                ("hostname".to_string(), hostname.to_string()),
            ],
        });
    }

    pub fn emit_collector_disconnected(&self, collector_id: &str, hostname: &str) {
        self.emit(HealthEvent {
            event_type: HealthEventType::CollectorDisconnected,
            severity: HealthSeverity::Warning,
            component: "collector".to_string(),
            message: format!("Collector {} ({}) disconnected", collector_id, hostname),
            metadata: vec![
                ("collector_id".to_string(), collector_id.to_string()),
                ("hostname".to_string(), hostname.to_string()),
            ],
        });
    }

    pub fn emit_graph_write_failed(&self, error: &str) {
        self.emit(HealthEvent {
            event_type: HealthEventType::GraphWriteFailed,
            severity: HealthSeverity::Critical,
            component: "graph".to_string(),
            message: format!("Graph write failed: {}", error),
            metadata: vec![("error".to_string(), error.to_string())],
        });
    }

    pub fn emit_queue_saturation(&self, queue_name: &str, depth: usize, capacity: usize) {
        self.emit(HealthEvent {
            event_type: HealthEventType::QueueSaturation,
            severity: HealthSeverity::Warning,
            component: queue_name.to_string(),
            message: format!("Queue {} saturated: {}/{}", queue_name, depth, capacity),
            metadata: vec![
                ("queue".to_string(), queue_name.to_string()),
                ("depth".to_string(), depth.to_string()),
                ("capacity".to_string(), capacity.to_string()),
            ],
        });
    }

    pub fn emit_governor_violation(&self, violation_type: &str) {
        self.emit(HealthEvent {
            event_type: HealthEventType::GovernorViolation,
            severity: HealthSeverity::Warning,
            component: "governor".to_string(),
            message: format!("Resource governor violation: {}", violation_type),
            metadata: vec![("violation_type".to_string(), violation_type.to_string())],
        });
    }

    pub fn emit_enricher_failed(&self, enricher_name: &str, error: &str) {
        self.emit(HealthEvent {
            event_type: HealthEventType::EnricherFailed,
            severity: HealthSeverity::Warning,
            component: "enricher".to_string(),
            message: format!("Enricher {} failed: {}", enricher_name, error),
            metadata: vec![
                ("enricher".to_string(), enricher_name.to_string()),
                ("error".to_string(), error.to_string()),
            ],
        });
    }

    pub fn emit_sidecar_lost(&self, sidecar_id: &str, kind: &str) {
        self.emit(HealthEvent {
            event_type: HealthEventType::SidecarLost,
            severity: HealthSeverity::Warning,
            component: "sidecar".to_string(),
            message: format!("Sidecar {} ({}) lost", sidecar_id, kind),
            metadata: vec![
                ("sidecar_id".to_string(), sidecar_id.to_string()),
                ("kind".to_string(), kind.to_string()),
            ],
        });
    }

    pub fn emit_disk_space_critical(&self, path: &str, usage_pct: f64) {
        self.emit(HealthEvent {
            event_type: HealthEventType::DiskSpaceCritical,
            severity: HealthSeverity::Critical,
            component: "disk".to_string(),
            message: format!("Disk space critical: {} at {:.1}%", path, usage_pct),
            metadata: vec![
                ("path".to_string(), path.to_string()),
                ("usage_pct".to_string(), usage_pct.to_string()),
            ],
        });
    }
}
