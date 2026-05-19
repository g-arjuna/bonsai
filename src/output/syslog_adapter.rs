//! Syslog output adapter for health events.
//!
//! Consumes health events from the event bus and sends them to syslog.
//! Supports standard syslog format with severity mapping.

use std::sync::Arc;
use crate::event_bus::InProcessBus;
use crate::health_emitter::{HealthEvent, HealthEventType, HealthSeverity};
use tokio::sync::RwLock;

/// Syslog configuration
#[derive(Clone, Debug)]
pub struct SyslogConfig {
    pub enabled: bool,
    pub endpoint: String, // syslog server address
    pub facility: String, // syslog facility (local0-local7)
}

impl Default for SyslogConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "127.0.0.1:514".to_string(),
            facility: "local0".to_string(),
        }
    }
}

/// Syslog output adapter for health events
pub struct SyslogAdapter {
    config: SyslogConfig,
    bus: Arc<InProcessBus>,
    running: Arc<RwLock<bool>>,
}

impl SyslogAdapter {
    pub fn new(config: SyslogConfig, bus: Arc<InProcessBus>) -> Self {
        Self {
            config,
            bus,
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// Convert HealthSeverity to syslog severity (0-7)
    fn to_syslog_severity(severity: &HealthSeverity) -> u8 {
        match severity {
            HealthSeverity::Info => 6, // informational
            HealthSeverity::Warning => 4, // warning
            HealthSeverity::Critical => 2, // critical
        }
    }

    /// Convert HealthEventType to syslog tag
    fn to_syslog_tag(event_type: &HealthEventType) -> &'static str {
        match event_type {
            HealthEventType::CollectorConnected => "bonsai_collector_connected",
            HealthEventType::CollectorDisconnected => "bonsai_collector_disconnected",
            HealthEventType::CollectorHeartbeatStale => "bonsai_collector_heartbeat_stale",
            HealthEventType::GraphWriteFailed => "bonsai_graph_write_failed",
            HealthEventType::QueueSaturation => "bonsai_queue_saturation",
            HealthEventType::GovernorViolation => "bonsai_governor_violation",
            HealthEventType::EnricherFailed => "bonsai_enricher_failed",
            HealthEventType::SidecarLost => "bonsai_sidecar_lost",
            HealthEventType::DiskSpaceCritical => "bonsai_disk_space_critical",
        }
    }

    /// Format health event as syslog message
    fn format_syslog_message(&self, event: &HealthEvent) -> String {
        let severity = Self::to_syslog_severity(&event.severity);
        let tag = Self::to_syslog_tag(&event.event_type);
        let facility = &self.config.facility;
        
        // RFC5424 format: <PRIVAL>VERSION TIMESTAMP HOSTNAME APP-NAME PROCID MSGID STRUCTURED-DATA MSG
        // For simplicity, using basic format
        format!(
            "<{}>{} {}: {} - {}",
            16 * 8 + severity, // facility * 8 + severity
            tag,
            event.component,
            event.message,
            event.metadata.iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }

    /// Start the syslog adapter
    pub async fn start(&self) {
        *self.running.write().await = true;
        
        if !self.config.enabled {
            tracing::info!("Syslog adapter disabled");
            return;
        }

        tracing::info!(
            endpoint = %self.config.endpoint,
            facility = %self.config.facility,
            "Syslog adapter starting"
        );

        let bus = self.bus.clone();
        let running = self.running.clone();
        let config = self.config.clone();

        tokio::spawn(async move {
            let mut rx = bus.subscribe();

            while *running.read().await {
                tokio::select! {
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {
                        // Check running flag
                    }
                    result = rx.recv() => {
                        if let Ok(update) = result {
                            if update.event_type == "health_event" {
                                if let Some(payload) = &update.payload {
                                    if let Ok(event) = serde_json::from_str::<HealthEvent>(payload) {
                                        let msg = Self::format_syslog_message(&config, &event);
                                        tracing::debug!(msg, "Syslog: {}", msg);
                                        // TODO: Send to actual syslog server via UDP/TCP
                                        // For now, just log
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    /// Stop the syslog adapter
    pub async fn stop(&self) {
        *self.running.write().await = false;
    }
}
