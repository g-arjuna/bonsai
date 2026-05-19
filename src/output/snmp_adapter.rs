//! SNMP trap adapter for health events.
//!
//! Consumes health events from the event bus and sends them as SNMP traps.
//! Supports SNMPv2c traps with configurable OID mappings.

use std::sync::Arc;
use crate::event_bus::InProcessBus;
use crate::health_emitter::{HealthEvent, HealthEventType, HealthSeverity};
use tokio::sync::RwLock;

/// SNMP trap configuration
#[derive(Clone, Debug)]
pub struct SnmpConfig {
    pub enabled: bool,
    pub target: String, // SNMP manager address
    pub community: String, // SNMPv2c community string
    pub port: u16, // SNMP manager port (default 162)
}

impl Default for SnmpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target: "127.0.0.1".to_string(),
            community: "public".to_string(),
            port: 162,
        }
    }
}

/// SNMP trap adapter for health events
pub struct SnmpAdapter {
    config: SnmpConfig,
    bus: Arc<InProcessBus>,
    running: Arc<RwLock<bool>>,
}

impl SnmpAdapter {
    pub fn new(config: SnmpConfig, bus: Arc<InProcessBus>) -> Self {
        Self {
            config,
            bus,
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// Convert HealthEventType to SNMP OID suffix
    fn to_event_oid(event_type: &HealthEventType) -> &'static str {
        match event_type {
            HealthEventType::CollectorConnected => "1.3.6.1.4.1.9999.1.1",
            HealthEventType::CollectorDisconnected => "1.3.6.1.4.1.9999.1.2",
            HealthEventType::CollectorHeartbeatStale => "1.3.6.1.4.1.9999.1.3",
            HealthEventType::GraphWriteFailed => "1.3.6.1.4.1.9999.2.1",
            HealthEventType::QueueSaturation => "1.3.6.1.4.1.9999.2.2",
            HealthEventType::GovernorViolation => "1.3.6.1.4.1.9999.3.1",
            HealthEventType::EnricherFailed => "1.3.6.1.4.1.9999.3.2",
            HealthEventType::SidecarLost => "1.3.6.1.4.1.9999.3.3",
            HealthEventType::DiskSpaceCritical => "1.3.6.1.4.1.9999.4.1",
        }
    }

    /// Convert HealthSeverity to SNMP trap severity
    fn to_trap_severity(severity: &HealthSeverity) -> u32 {
        match severity {
            HealthSeverity::Info => 1, // informational
            HealthSeverity::Warning => 3, // warning
            HealthSeverity::Critical => 5, // critical
        }
    }

    /// Format health event as SNMP trap
    fn format_trap(&self, event: &HealthEvent) -> String {
        let oid = Self::to_event_oid(&event.event_type);
        let severity = Self::to_trap_severity(&event.severity);
        
        // Format as SNMPv2c trap
        format!(
            "SNMPv2c Trap: target={}, community={}, oid={}, severity={}, component={}, message={}",
            self.config.target,
            self.config.community,
            oid,
            severity,
            event.component,
            event.message
        )
    }

    /// Start the SNMP adapter
    pub async fn start(&self) {
        *self.running.write().await = true;
        
        if !self.config.enabled {
            tracing::info!("SNMP adapter disabled");
            return;
        }

        tracing::info!(
            target = %self.config.target,
            port = self.config.port,
            community = %self.config.community,
            "SNMP adapter starting"
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
                                        let trap = Self::format_trap(&config, &event);
                                        tracing::debug!(trap, "SNMP: {}", trap);
                                        // TODO: Send actual SNMP trap via UDP to manager
                                        // Requires SNMP library (e.g., snmp-rs)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    /// Stop the SNMP adapter
    pub async fn stop(&self) {
        *self.running.write().await = false;
    }
}
