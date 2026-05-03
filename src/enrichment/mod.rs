//! Enrichment foundation — trait, types, registry, and background runner.
//!
//! Enrichers read external CMDBs / IPAMs and write business-context properties
//! onto the bonsai graph (device owner, VLAN assignments, application bindings).
//! All enrichers:
//!   - Access credentials only via the vault (never inline config)
//!   - Declare their write surface so enforcement can prevent namespace bleed
//!   - Write every credential resolve to the audit log with purpose = Enrich
//!   - Are environment-aware (can declare which archetypes they apply to)
//!   - Are idempotent — re-running is always safe

pub mod factory;
pub mod netbox;
pub mod servicenow;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

// ── Schedule ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum EnrichmentSchedule {
    /// Never run automatically; only triggered manually via API.
    Manual,
    /// Run every N seconds.
    Interval { secs: u64 },
}

impl Default for EnrichmentSchedule {
    fn default() -> Self {
        EnrichmentSchedule::Interval { secs: 3600 }
    }
}

// ── Write surface ─────────────────────────────────────────────────────────────

/// Declares which parts of the graph an enricher may write.
/// Enforced by the runner — writes outside this surface are rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentWriteSurface {
    /// Prefix for all device/site properties this enricher owns, e.g. `"netbox_"`.
    pub property_namespace: String,
    /// New node labels this enricher may create, e.g. `["VLAN", "Prefix"]`.
    pub owned_labels: Vec<String>,
    /// New edge types this enricher may create, e.g. `["ACCESS_VLAN", "HAS_PREFIX"]`.
    pub owned_edge_types: Vec<String>,
}

// ── Report ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnrichmentReport {
    pub enricher_name: String,
    pub duration_ms: u64,
    pub nodes_touched: usize,
    pub edges_created: usize,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

// ── Audit bridge ──────────────────────────────────────────────────────────────

/// Thin handle passed into enrichers so they can write audit log entries without
/// knowing the underlying audit module internals.
#[derive(Clone)]
pub struct EnricherAuditLog {
    root: PathBuf,
    enricher_name: String,
}

impl EnricherAuditLog {
    pub fn new(root: &Path, enricher_name: &str) -> Self {
        Self {
            root: root.to_path_buf(),
            enricher_name: enricher_name.to_string(),
        }
    }

    /// Log a credential resolve performed by this enricher.
    pub fn log_credential_resolve(&self, alias: &str, outcome: &str, error: Option<&str>) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        if let Err(e) = crate::audit::append_credential_resolve(
            &self.root,
            ts,
            alias,
            "enrich",
            outcome,
            error,
        ) {
            warn!(enricher = %self.enricher_name, "failed to write enrichment audit entry: {e}");
        }
    }

    /// Log an enrichment run event (not a credential resolve).
    pub fn log_run(&self, outcome: &str, nodes_touched: usize, error: Option<&str>) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        if let Err(e) = crate::audit::append_enrichment_run(
            &self.root,
            ts,
            &self.enricher_name,
            outcome,
            nodes_touched,
            error,
        ) {
            warn!(enricher = %self.enricher_name, "failed to write enrichment run audit entry: {e}");
        }
    }
}

// ── Trait ─────────────────────────────────────────────────────────────────────

#[async_trait::async_trait]
pub trait GraphEnricher: Send + Sync {
    fn name(&self) -> &str;
    fn schedule(&self) -> EnrichmentSchedule;
    fn writes_to(&self) -> EnrichmentWriteSurface;

    /// Run the enrichment pass. Must be idempotent.
    /// Credentials accessed only through `creds` vault with purpose = Enrich.
    async fn enrich(
        &self,
        store: &dyn crate::store::BonsaiStore,
        creds: &crate::credentials::CredentialVault,
        audit: &EnricherAuditLog,
    ) -> Result<EnrichmentReport>;

    /// Dial the external endpoint and return Ok if reachable.
    /// Used by the "Test connection" button in the UI.
    async fn test_connection(
        &self,
        creds: &crate::credentials::CredentialVault,
        audit: &EnricherAuditLog,
    ) -> Result<()>;
}

// ── Config (persisted) ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnricherConfig {
    /// Stable identifier — matches `GraphEnricher::name()`.
    pub name: String,
    /// Type tag: "netbox" | "servicenow" | "cli_scrape".
    pub enricher_type: String,
    pub enabled: bool,
    pub base_url: String,
    /// Alias in the credential vault.
    pub credential_alias: String,
    /// How often to run automatically (seconds). 0 = manual only.
    pub poll_interval_secs: u64,
    /// Environment archetypes this enricher applies to. Empty = all.
    pub environment_scope: Vec<String>,
    /// Type-specific extra fields (e.g. NetBox token header name).
    #[serde(default)]
    pub extra: serde_json::Value,
}

// ── Runtime state (in-memory, not persisted) ──────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnricherRunState {
    pub last_run_at_ns: Option<i64>,
    pub last_run_duration_ms: Option<u64>,
    pub last_run_nodes_touched: Option<usize>,
    pub last_run_warnings: Vec<String>,
    pub last_run_error: Option<String>,
    pub is_running: bool,
}

// ── Registry ──────────────────────────────────────────────────────────────────

const CONFIGS_FILE: &str = "enrichment_configs.json";

#[derive(Default)]
pub struct EnricherRegistry {
    configs: Vec<EnricherConfig>,
    states: std::collections::HashMap<String, EnricherRunState>,
    configs_path: PathBuf,
    audit_root: PathBuf,
}

impl EnricherRegistry {
    /// Load from `runtime_dir/enrichment_configs.json`, or start empty.
    pub fn load(runtime_dir: &Path) -> Self {
        let configs_path = runtime_dir.join(CONFIGS_FILE);
        let configs: Vec<EnricherConfig> = std::fs::read_to_string(&configs_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        info!(count = configs.len(), "enricher configs loaded");
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
            warn!("failed to persist enricher configs: {e}");
        }
    }

    pub fn list(&self) -> Vec<(EnricherConfig, EnricherRunState)> {
        self.configs
            .iter()
            .map(|c| {
                let state = self.states.get(&c.name).cloned().unwrap_or_default();
                (c.clone(), state)
            })
            .collect()
    }

    pub fn upsert(&mut self, config: EnricherConfig) {
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

    pub fn get(&self, name: &str) -> Option<&EnricherConfig> {
        self.configs.iter().find(|c| c.name == name)
    }

    pub fn set_running(&mut self, name: &str, running: bool) {
        self.states.entry(name.to_string()).or_default().is_running = running;
    }

    pub fn record_run(&mut self, name: &str, report: &EnrichmentReport) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let outcome = if report.error.is_some() { "error" } else { "success" };
        if let Err(e) = crate::audit::append_enrichment_run(
            &self.audit_root,
            ts,
            name,
            outcome,
            report.nodes_touched,
            report.error.as_deref(),
        ) {
            warn!(enricher = name, "failed to write enrichment run audit entry: {e}");
        }
        let state = self.states.entry(name.to_string()).or_default();
        state.last_run_at_ns = Some(ts);
        state.last_run_duration_ms = Some(report.duration_ms);
        state.last_run_nodes_touched = Some(report.nodes_touched);
        state.last_run_warnings = report.warnings.clone();
        state.last_run_error = report.error.clone();
        state.is_running = false;
    }
}

// ── Stub enricher (used in integration tests) ─────────────────────────────────

/// No-op enricher used to validate the trait compiles and the registry works.
pub struct StubEnricher {
    pub name: String,
}

#[async_trait::async_trait]
impl GraphEnricher for StubEnricher {
    fn name(&self) -> &str {
        &self.name
    }

    fn schedule(&self) -> EnrichmentSchedule {
        EnrichmentSchedule::Manual
    }

    fn writes_to(&self) -> EnrichmentWriteSurface {
        EnrichmentWriteSurface {
            property_namespace: "stub_".to_string(),
            owned_labels: vec![],
            owned_edge_types: vec![],
        }
    }

    async fn enrich(
        &self,
        _store: &dyn crate::store::BonsaiStore,
        _creds: &crate::credentials::CredentialVault,
        audit: &EnricherAuditLog,
    ) -> Result<EnrichmentReport> {
        audit.log_run("success", 0, None);
        Ok(EnrichmentReport {
            enricher_name: self.name.clone(),
            ..Default::default()
        })
    }

    async fn test_connection(
        &self,
        _creds: &crate::credentials::CredentialVault,
        _audit: &EnricherAuditLog,
    ) -> Result<()> {
        Ok(())
    }
}

/// Shared registry handle — clone freely, all clones point to the same data.
pub type SharedEnricherRegistry = Arc<RwLock<EnricherRegistry>>;

pub fn new_registry(runtime_dir: &Path) -> SharedEnricherRegistry {
    Arc::new(RwLock::new(EnricherRegistry::load(runtime_dir)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    fn base_config(name: &str) -> EnricherConfig {
        EnricherConfig {
            name: name.to_string(),
            enricher_type: "netbox".to_string(),
            enabled: true,
            base_url: "http://netbox.local".to_string(),
            credential_alias: "netbox-token".to_string(),
            poll_interval_secs: 3600,
            environment_scope: vec![],
            extra: serde_json::Value::Null,
        }
    }

    // ── load ──────────────────────────────────────────────────────────────────

    #[test]
    fn load_returns_empty_registry_when_no_file() {
        let dir = tmp();
        let reg = EnricherRegistry::load(dir.path());
        assert!(reg.list().is_empty());
    }

    #[test]
    fn load_returns_empty_and_does_not_panic_on_corrupted_file() {
        let dir = tmp();
        let cfg_path = dir.path().join("enrichment_configs.json");
        std::fs::write(&cfg_path, b"this is not json {{{{").unwrap();
        let reg = EnricherRegistry::load(dir.path());
        assert!(reg.list().is_empty(), "corrupted file should yield empty registry");
    }

    // ── upsert ────────────────────────────────────────────────────────────────

    #[test]
    fn upsert_adds_new_entry() {
        let dir = tmp();
        let mut reg = EnricherRegistry::load(dir.path());
        reg.upsert(base_config("nb1"));
        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.get("nb1").unwrap().name, "nb1");
    }

    #[test]
    fn upsert_updates_existing_entry_no_duplicate() {
        let dir = tmp();
        let mut reg = EnricherRegistry::load(dir.path());
        reg.upsert(base_config("nb1"));
        let mut updated = base_config("nb1");
        updated.base_url = "http://netbox2.local".to_string();
        reg.upsert(updated);
        assert_eq!(reg.list().len(), 1, "upsert must not duplicate");
        assert_eq!(reg.get("nb1").unwrap().base_url, "http://netbox2.local");
    }

    // ── remove ────────────────────────────────────────────────────────────────

    #[test]
    fn remove_deletes_entry_and_its_state() {
        let dir = tmp();
        let mut reg = EnricherRegistry::load(dir.path());
        reg.upsert(base_config("nb1"));
        reg.set_running("nb1", true);
        let removed = reg.remove("nb1");
        assert!(removed);
        assert!(reg.get("nb1").is_none());
        assert!(reg.list().is_empty());
    }

    #[test]
    fn remove_nonexistent_returns_false() {
        let dir = tmp();
        let mut reg = EnricherRegistry::load(dir.path());
        assert!(!reg.remove("does_not_exist"));
    }

    // ── record_run updates state ──────────────────────────────────────────────

    #[test]
    fn record_run_updates_state_and_clears_is_running() {
        let dir = tmp();
        let mut reg = EnricherRegistry::load(dir.path());
        reg.upsert(base_config("nb1"));
        reg.set_running("nb1", true);

        let report = EnrichmentReport {
            enricher_name: "nb1".to_string(),
            duration_ms: 250,
            nodes_touched: 42,
            edges_created: 7,
            warnings: vec![],
            error: None,
        };
        reg.record_run("nb1", &report);

        let (_, state) = reg.list().into_iter().next().unwrap();
        assert!(!state.is_running);
        assert_eq!(state.last_run_duration_ms, Some(250));
        assert_eq!(state.last_run_nodes_touched, Some(42));
        assert!(state.last_run_error.is_none());
    }

    // ── persistence round-trip ────────────────────────────────────────────────

    #[test]
    fn configs_persist_and_reload() {
        let dir = tmp();
        {
            let mut reg = EnricherRegistry::load(dir.path());
            reg.upsert(base_config("nb1"));
            reg.upsert(base_config("snow1"));
        }
        let reg2 = EnricherRegistry::load(dir.path());
        assert_eq!(reg2.list().len(), 2);
        assert!(reg2.get("nb1").is_some());
        assert!(reg2.get("snow1").is_some());
    }
}
