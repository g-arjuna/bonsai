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

pub use traits::{
    new_adapter_registry, OutputAdapter, OutputAdapterAuditLog, OutputAdapterConfig,
    OutputAdapterRegistry, OutputAdapterRunState, OutputReport, OutputTopic,
    SharedAdapterRegistry, StubAdapter,
};
