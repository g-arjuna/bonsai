//! OutputAdapter trait — parallel to GraphEnricher.
//!
//! Adapters are bus subscribers that push data to external systems.
//! All adapters:
//!   - Are read-only on the bus (never publish back)
//!   - Access credentials only via the vault
//!   - Write every push cycle to the audit log
//!   - Are environment-scoped
//!   - Fail in isolation (one adapter down does not stop others)

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, watch};
use tracing::{info, warn};

use crate::credentials::CredentialVault;
use crate::event_bus::InProcessBus;

// ── Topic hint ────────────────────────────────────────────────────────────────

/// Which data streams an adapter consumes. Informational — the adapter
/// self-filters from the raw bus; the registry uses this to document intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputTopic {
    /// Raw `TelemetryUpdate` messages (counter readings, state changes).
    /// Suitable for collector-side metric exporters (Prometheus, TSDB).
    RawTelemetry,
    /// Detection events aggregated at core.
    /// Suitable for AIOps / log-analytics outputs (Splunk, Elastic, SNOW EM).
    DetectionEvents,
    /// Remediation outcomes.
    RemediationOutcomes,
    /// Audit log entries (compliance exports).
    AuditEntries,
}

// ── Report ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OutputReport {
    pub adapter_name: String,
    pub duration_ms: u64,
    pub events_pushed: usize,
    pub bytes_sent: u64,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

// ── Audit bridge ──────────────────────────────────────────────────────────────

/// Thin handle passed into adapters so they can write audit entries without
/// depending on the audit module internals.
#[derive(Clone)]
pub struct OutputAdapterAuditLog {
    root: PathBuf,
    adapter_name: String,
}

impl OutputAdapterAuditLog {
    pub fn new(root: &Path, adapter_name: &str) -> Self {
        Self {
            root: root.to_path_buf(),
            adapter_name: adapter_name.to_string(),
        }
    }

    pub fn log_push(&self, events_pushed: usize, bytes_sent: u64, error: Option<&str>) {
        let ts = now_ns();
        if let Err(e) = crate::audit::append_adapter_push(
            &self.root,
            ts,
            &self.adapter_name,
            if error.is_some() { "error" } else { "success" },
            events_pushed,
            bytes_sent,
            error,
        ) {
            warn!(adapter = %self.adapter_name, "failed to write adapter push audit entry: {e}");
        }
    }

    pub fn log_credential_resolve(&self, alias: &str, outcome: &str, error: Option<&str>) {
        let ts = now_ns();
        if let Err(e) = crate::audit::append_credential_resolve(
            &self.root,
            ts,
            alias,
            "adapter_push",
            outcome,
            error,
        ) {
            warn!(adapter = %self.adapter_name, "failed to write adapter cred audit entry: {e}");
        }
    }
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

// ── Trait ─────────────────────────────────────────────────────────────────────

#[async_trait::async_trait]
pub trait OutputAdapter: Send + Sync {
    fn name(&self) -> &str;

    /// Which bus topics this adapter subscribes to (declarative, for documentation
    /// and routing decisions).
    fn topics(&self) -> &[OutputTopic];

    /// Environment archetypes this adapter applies to. Empty = all environments.
    fn applies_to_environments(&self) -> &[String];

    /// Run the adapter push loop until `shutdown` fires.
    ///
    /// The adapter subscribes to `bus`, transforms received events to the vendor
    /// format, and pushes to the configured endpoint. Never writes back to `bus`.
    async fn run(
        &self,
        bus: Arc<InProcessBus>,
        creds: Arc<CredentialVault>,
        audit: OutputAdapterAuditLog,
        shutdown: watch::Receiver<bool>,
    ) -> Result<()>;

    /// Verify the external endpoint is reachable and credentials are valid.
    /// Called by the "Test connection" button in the UI.
    async fn test_connection(
        &self,
        creds: Arc<CredentialVault>,
        audit: &OutputAdapterAuditLog,
    ) -> Result<()>;
}

// ── Config (persisted to runtime/adapter_configs.json) ────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputAdapterConfig {
    /// Stable identifier — matches the running adapter's `name()`.
    pub name: String,
    /// Type tag: "prometheus_remote_write" | "splunk_hec" | "elastic" | "servicenow_em".
    pub adapter_type: String,
    pub enabled: bool,
    /// Target endpoint (remote-write URL, HEC endpoint, SNOW instance URL, etc.).
    pub endpoint_url: String,
    /// Credential alias in the vault. Empty string = no authentication.
    #[serde(default)]
    pub credential_alias: String,
    /// How often the adapter batches and flushes metrics (seconds).
    #[serde(default = "default_flush_interval")]
    pub flush_interval_secs: u64,
    /// Environment archetypes this adapter applies to. Empty = all.
    #[serde(default)]
    pub environment_scope: Vec<String>,
    /// Adapter-type-specific extra fields (e.g. job label for Prometheus).
    #[serde(default)]
    pub extra: serde_json::Value,
}

fn default_flush_interval() -> u64 {
    30
}

// ── Runtime state (in-memory) ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OutputAdapterRunState {
    pub last_push_at_ns: Option<i64>,
    pub last_push_duration_ms: Option<u64>,
    pub last_push_events: Option<usize>,
    pub last_push_bytes: Option<u64>,
    pub last_push_warnings: Vec<String>,
    pub last_push_error: Option<String>,
    pub is_running: bool,
    pub total_events_pushed: u64,
    pub total_bytes_sent: u64,
}

// ── Registry ──────────────────────────────────────────────────────────────────

const CONFIGS_FILE: &str = "adapter_configs.json";

#[derive(Default)]
pub struct OutputAdapterRegistry {
    configs: Vec<OutputAdapterConfig>,
    states: std::collections::HashMap<String, OutputAdapterRunState>,
    configs_path: PathBuf,
    audit_root: PathBuf,
}

impl OutputAdapterRegistry {
    /// Load configs from `runtime_dir/adapter_configs.json`, or start empty.
    pub fn load(runtime_dir: &Path) -> Self {
        let configs_path = runtime_dir.join(CONFIGS_FILE);
        let configs: Vec<OutputAdapterConfig> = std::fs::read_to_string(&configs_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        info!(count = configs.len(), "output adapter configs loaded");
        Self {
            configs,
            states: std::collections::HashMap::new(),
            configs_path,
            audit_root: runtime_dir.to_path_buf(),
        }
    }

    fn persist(&self) {
        if let Ok(json) = serde_json::to_string_pretty(&self.configs)
            && let Err(e) = std::fs::write(&self.configs_path, json)
        {
            warn!("failed to persist adapter configs: {e}");
        }
    }

    pub fn list(&self) -> Vec<(OutputAdapterConfig, OutputAdapterRunState)> {
        self.configs
            .iter()
            .map(|c| {
                let state = self.states.get(&c.name).cloned().unwrap_or_default();
                (c.clone(), state)
            })
            .collect()
    }

    pub fn upsert(&mut self, config: OutputAdapterConfig) {
        if let Some(existing) = self.configs.iter_mut().find(|c| c.name == config.name) {
            *existing = config;
        } else {
            self.configs.push(config);
        }
        self.persist();
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.configs.len();
        self.configs.retain(|c| c.name != name);
        let removed = self.configs.len() < before;
        if removed {
            self.states.remove(name);
            self.persist();
        }
        removed
    }

    pub fn get(&self, name: &str) -> Option<&OutputAdapterConfig> {
        self.configs.iter().find(|c| c.name == name)
    }

    pub fn set_running(&mut self, name: &str, running: bool) {
        self.states.entry(name.to_string()).or_default().is_running = running;
    }

    pub fn record_push(&mut self, name: &str, report: &OutputReport) {
        let ts = now_ns();
        let outcome = if report.error.is_some() { "error" } else { "success" };
        if let Err(e) = crate::audit::append_adapter_push(
            &self.audit_root,
            ts,
            name,
            outcome,
            report.events_pushed,
            report.bytes_sent,
            report.error.as_deref(),
        ) {
            warn!(adapter = name, "failed to write adapter push audit entry: {e}");
        }
        let state = self.states.entry(name.to_string()).or_default();
        state.last_push_at_ns = Some(ts);
        state.last_push_duration_ms = Some(report.duration_ms);
        state.last_push_events = Some(report.events_pushed);
        state.last_push_bytes = Some(report.bytes_sent);
        state.last_push_warnings = report.warnings.clone();
        state.last_push_error = report.error.clone();
        state.is_running = false;
        state.total_events_pushed += report.events_pushed as u64;
        state.total_bytes_sent += report.bytes_sent;
    }
}

pub type SharedAdapterRegistry = Arc<RwLock<OutputAdapterRegistry>>;

pub fn new_adapter_registry(runtime_dir: &Path) -> SharedAdapterRegistry {
    Arc::new(RwLock::new(OutputAdapterRegistry::load(runtime_dir)))
}

// ── Stub adapter (validates the trait + registry in tests) ────────────────────

pub struct StubAdapter {
    pub name: String,
}

#[async_trait::async_trait]
impl OutputAdapter for StubAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn topics(&self) -> &[OutputTopic] {
        &[OutputTopic::RawTelemetry]
    }

    fn applies_to_environments(&self) -> &[String] {
        &[]
    }

    async fn run(
        &self,
        bus: Arc<InProcessBus>,
        _creds: Arc<CredentialVault>,
        audit: OutputAdapterAuditLog,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        let mut rx = bus.subscribe();
        let mut count = 0usize;
        loop {
            tokio::select! {
                res = rx.recv() => {
                    if res.is_ok() { count += 1; }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() { break; }
                }
            }
        }
        audit.log_push(count, 0, None);
        Ok(())
    }

    async fn test_connection(
        &self,
        _creds: Arc<CredentialVault>,
        _audit: &OutputAdapterAuditLog,
    ) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_bus::InProcessBus;

    #[test]
    fn stub_adapter_compiles() {
        let adapter = StubAdapter { name: "test".to_string() };
        assert_eq!(adapter.name(), "test");
        assert_eq!(adapter.topics(), &[OutputTopic::RawTelemetry]);
        assert!(adapter.applies_to_environments().is_empty());
    }

    #[test]
    fn registry_upsert_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = OutputAdapterRegistry::load(dir.path());
        let cfg = OutputAdapterConfig {
            name: "prom1".to_string(),
            adapter_type: "prometheus_remote_write".to_string(),
            enabled: true,
            endpoint_url: "http://localhost:9090/api/v1/write".to_string(),
            credential_alias: String::new(),
            flush_interval_secs: 30,
            environment_scope: vec![],
            extra: serde_json::Value::Null,
        };
        reg.upsert(cfg.clone());
        assert_eq!(reg.list().len(), 1);
        reg.remove("prom1");
        assert!(reg.list().is_empty());
    }
}
