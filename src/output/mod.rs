//! Output adapters — bus subscribers that push data to external systems.
//!
//! Each adapter implements `OutputAdapter` and runs as an independent background
//! task. Adapters are:
//!   - **Read-only on the bus** — they subscribe but never publish.
//!   - **Credential-safe** — credentials resolved via vault, audit-logged.
//!   - **Environment-scoped** — an adapter can limit itself to specific archetypes.
//!   - **Failure-isolated** — one adapter failing does not affect others or the bus.
//!
//! Adapters that consume raw telemetry counters run collector-side.
//! Adapters that consume detections and remediations run core-side.
//!
//! # Architecture
//!
//! ```text
//! InProcessBus ──broadcast──► PrometheusRemoteWriteAdapter (collector-side)
//! Graph poll   ──timer──────► SplunkHecAdapter            (core-side)
//!              └──timer──────► ElasticAdapter              (core-side)
//!              └──timer──────► ServiceNowEmAdapter         (core-side, refactored Sprint 9)
//! ```

pub mod elastic;
pub mod prometheus;
pub mod servicenow_em;
pub mod splunk_hec;
pub mod traits;

use std::sync::Arc;

use anyhow::{Result, anyhow};
use lbug::Database;

pub use traits::{
    OutputAdapter, OutputAdapterAuditLog, OutputAdapterConfig, OutputAdapterRegistry,
    OutputAdapterRunState, OutputReport, OutputTopic, SharedAdapterRegistry, StubAdapter,
    new_adapter_registry,
};

pub fn build_adapter(
    config: &OutputAdapterConfig,
    db: Arc<Database>,
) -> Option<Box<dyn OutputAdapter>> {
    if let Some(adapter) = prometheus::build(config) {
        return Some(Box::new(adapter));
    }
    if let Some(adapter) = splunk_hec::build(config, Arc::clone(&db)) {
        return Some(Box::new(adapter));
    }
    if let Some(adapter) = elastic::build(config, Arc::clone(&db)) {
        return Some(Box::new(adapter));
    }
    if let Some(adapter) = servicenow_em::build(config, db) {
        return Some(Box::new(adapter));
    }
    None
}

pub fn ensure_supported_adapter_type(config: &OutputAdapterConfig) -> Result<()> {
    if matches!(
        config.adapter_type.as_str(),
        "prometheus_remote_write" | "splunk_hec" | "elastic" | "servicenow_em"
    ) {
        Ok(())
    } else {
        Err(anyhow!("unknown adapter type '{}'", config.adapter_type))
    }
}
