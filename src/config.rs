use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct Config {
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub collector: CollectorConfig,
    pub graph_path: String,
    /// gRPC listen address for the Bonsai API server. Default: "[::1]:50051".
    #[serde(default = "default_api_addr")]
    pub api_addr: String,
    /// Prometheus /metrics HTTP listener. Default: "[::1]:9090". Set to "" to disable.
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: String,
    #[serde(default)]
    pub retention: RetentionConfig,
    #[serde(default)]
    pub event_bus: EventBusConfig,
    #[serde(default)]
    pub archive: ArchiveConfig,
    #[serde(default)]
    pub credentials: CredentialsConfig,
    #[serde(default)]
    pub target: Vec<TargetConfig>,
}

#[derive(Deserialize, Clone, Default)]
pub struct CollectorConfig {
    #[serde(default = "default_collector_graph_path")]
    pub graph_path: String,
    #[serde(default)]
    pub queue: CollectorQueueConfig,
    #[serde(default)]
    pub filter: CollectorFilterConfig,
}

impl CollectorConfig {
    pub fn default_with_paths() -> Self {
        Self {
            graph_path: default_collector_graph_path(),
            ..Default::default()
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct CollectorFilterConfig {
    /// Minimum interval between counter forwards per (device, interface). Default: 10s.
    #[serde(default = "default_debounce_secs")]
    pub counter_debounce_secs: u64,
    /// Forwarding mode: "raw" (no filtering), "debounced" (drops updates within window).
    #[serde(default = "default_counter_forward_mode")]
    pub counter_forward_mode: String,
}

impl Default for CollectorFilterConfig {
    fn default() -> Self {
        Self {
            counter_debounce_secs: default_debounce_secs(),
            counter_forward_mode: default_counter_forward_mode(),
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct CollectorQueueConfig {
    /// Directory containing append-only collector queue files.
    #[serde(default = "default_collector_queue_path")]
    pub path: String,
    /// Maximum queue bytes before oldest unacked records are dropped. 0 = unlimited.
    #[serde(default = "default_collector_queue_max_bytes")]
    pub max_bytes: u64,
    /// Drop records older than this many hours. 0 = unlimited.
    #[serde(default = "default_collector_queue_max_age_hours")]
    pub max_age_hours: u64,
    /// Maximum records sent in one client-streaming replay.
    #[serde(default = "default_collector_queue_drain_batch_size")]
    pub drain_batch_size: usize,
    /// Periodic operator visibility interval. 0 disables periodic queue logs.
    #[serde(default = "default_collector_queue_log_interval_seconds")]
    pub log_interval_seconds: u64,
}

impl Default for CollectorQueueConfig {
    fn default() -> Self {
        Self {
            path: default_collector_queue_path(),
            max_bytes: default_collector_queue_max_bytes(),
            max_age_hours: default_collector_queue_max_age_hours(),
            drain_batch_size: default_collector_queue_drain_batch_size(),
            log_interval_seconds: default_collector_queue_log_interval_seconds(),
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct RuntimeConfig {
    /// One binary, three modes: "all" (default), "core", or "collector".
    #[serde(default = "default_runtime_mode")]
    pub mode: String,
    /// Stable collector identity added to TelemetryIngest records.
    #[serde(default = "default_collector_id")]
    pub collector_id: String,
    /// Core gRPC endpoint used by collector mode.
    #[serde(default = "default_core_ingest_endpoint")]
    pub core_ingest_endpoint: String,
    /// Optional TLS/mTLS settings for the distributed collector-core channel.
    #[serde(default)]
    pub tls: RuntimeTlsConfig,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            mode: default_runtime_mode(),
            collector_id: default_collector_id(),
            core_ingest_endpoint: default_core_ingest_endpoint(),
            tls: RuntimeTlsConfig::default(),
        }
    }
}

#[derive(Deserialize, Clone, Default)]
pub struct RuntimeTlsConfig {
    /// Enables TLS on the core listener and mTLS on collector connections.
    #[serde(default)]
    pub enabled: bool,
    /// CA certificate used by collectors to verify the core and by cores to verify collectors.
    pub ca_cert: Option<String>,
    /// Local certificate chain presented by this process.
    pub cert: Option<String>,
    /// Local private key presented by this process.
    pub key: Option<String>,
    /// Server name collectors use when verifying the core certificate.
    pub server_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeMode {
    All,
    Core,
    Collector,
}

impl RuntimeMode {
    pub fn runs_core(self) -> bool {
        matches!(self, RuntimeMode::All | RuntimeMode::Core)
    }

    pub fn runs_collector(self) -> bool {
        matches!(self, RuntimeMode::All | RuntimeMode::Collector)
    }
}

impl RuntimeConfig {
    pub fn parsed_mode(&self) -> Result<RuntimeMode> {
        match self.mode.trim().to_ascii_lowercase().as_str() {
            "all" => Ok(RuntimeMode::All),
            "core" => Ok(RuntimeMode::Core),
            "collector" => Ok(RuntimeMode::Collector),
            other => anyhow::bail!(
                "invalid runtime.mode '{other}' - expected one of: all, core, collector"
            ),
        }
    }
}

#[derive(Deserialize, Default)]
pub struct RetentionConfig {
    /// Enable periodic pruning of old StateChangeEvents. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Delete StateChangeEvents older than this many hours. Default: 72.
    #[serde(default = "default_retention_hours")]
    pub max_age_hours: u64,
    /// Hard cap on total StateChangeEvents kept. 0 = unlimited. Default: 50000.
    /// When the count exceeds this, oldest events are deleted to get back under the limit.
    #[serde(default = "default_max_state_change_events")]
    pub max_state_change_events: u64,
}

#[derive(Deserialize)]
pub struct EventBusConfig {
    /// broadcast channel capacity. Default: 2048.
    #[serde(default = "default_bus_capacity")]
    pub capacity: usize,
    /// Minimum interval between counter writes per (device, interface). Default: 10s.
    #[serde(default = "default_debounce_secs")]
    pub counter_debounce_secs: u64,
}

impl Default for EventBusConfig {
    fn default() -> Self {
        Self {
            capacity: default_bus_capacity(),
            counter_debounce_secs: default_debounce_secs(),
        }
    }
}

#[derive(Deserialize)]
pub struct ArchiveConfig {
    /// Enable the Parquet archive consumer. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Root directory for parquet archive output. Default: "archive".
    #[serde(default = "default_archive_path")]
    pub path: String,
    /// Flush buffered rows every N seconds. Default: 10.
    #[serde(default = "default_archive_flush_interval_seconds")]
    pub flush_interval_seconds: u64,
    /// Flush immediately when the in-memory batch reaches this size. Default: 1000.
    #[serde(default = "default_archive_max_batch_rows")]
    pub max_batch_rows: usize,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: default_archive_path(),
            flush_interval_seconds: default_archive_flush_interval_seconds(),
            max_batch_rows: default_archive_max_batch_rows(),
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct CredentialsConfig {
    /// Directory containing vault.age and metadata.json. Default: "bonsai-credentials".
    #[serde(default = "default_credentials_path")]
    pub path: String,
    /// Environment variable containing the vault passphrase for this backend slice.
    #[serde(default = "default_credentials_passphrase_env")]
    pub passphrase_env: String,
}

impl Default for CredentialsConfig {
    fn default() -> Self {
        Self {
            path: default_credentials_path(),
            passphrase_env: default_credentials_passphrase_env(),
        }
    }
}

fn default_retention_hours() -> u64 {
    72
}

fn default_max_state_change_events() -> u64 {
    50_000
}

fn default_bus_capacity() -> usize {
    2048
}

fn default_debounce_secs() -> u64 {
    10
}

fn default_archive_path() -> String {
    "archive".to_string()
}

fn default_archive_flush_interval_seconds() -> u64 {
    10
}

fn default_archive_max_batch_rows() -> usize {
    1000
}

fn default_credentials_path() -> String {
    "bonsai-credentials".to_string()
}

fn default_credentials_passphrase_env() -> String {
    "BONSAI_VAULT_PASSPHRASE".to_string()
}

fn default_counter_forward_mode() -> String {
    "debounced".to_string()
}

fn default_collector_queue_path() -> String {
    "runtime/collector-queue".to_string()
}

fn default_collector_graph_path() -> String {
    "runtime/collector.db".to_string()
}

fn default_collector_queue_max_bytes() -> u64 {
    1_073_741_824
}

fn default_collector_queue_max_age_hours() -> u64 {
    24
}

fn default_collector_queue_drain_batch_size() -> usize {
    1_000
}

fn default_collector_queue_log_interval_seconds() -> u64 {
    30
}

fn default_runtime_mode() -> String {
    "all".to_string()
}

fn default_collector_id() -> String {
    "local".to_string()
}

fn default_core_ingest_endpoint() -> String {
    "http://[::1]:50051".to_string()
}

fn default_target_enabled() -> bool {
    true
}

fn default_api_addr() -> String {
    "[::1]:50051".to_string()
}

fn default_metrics_addr() -> String {
    "[::1]:9090".to_string()
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct TargetConfig {
    pub address: String,
    /// Whether the runtime subscriber should be running for this target.
    #[serde(default = "default_target_enabled")]
    pub enabled: bool,
    /// TLS server name (SNI). Required when ca_cert is set.
    pub tls_domain: Option<String>,
    /// Path to PEM CA cert. Enables TLS for this target.
    pub ca_cert: Option<String>,
    /// Override vendor detection. If absent, Capabilities RPC auto-detects.
    pub vendor: Option<String>,
    /// Alias into the local encrypted credential vault.
    pub credential_alias: Option<String>,
    /// Env var name whose value is the username. Takes precedence over `username`.
    pub username_env: Option<String>,
    /// Env var name whose value is the password. Takes precedence over `password`.
    pub password_env: Option<String>,
    /// Inline username — lab use only; bonsai.toml must not be committed with real creds.
    pub username: Option<String>,
    /// Inline password — lab use only.
    pub password: Option<String>,
    /// Human-readable device hostname for graph indexing (e.g. "srl1").
    /// Used to match LLDP system-name when building CONNECTED_TO edges.
    pub hostname: Option<String>,
    /// Logical role hint for future onboarding/profile selection (e.g. "leaf", "spine", "pe").
    pub role: Option<String>,
    /// Site label for future topology grouping and TSDB/graph enrichment.
    pub site: Option<String>,
    /// The stable ID of the collector responsible for this device.
    pub collector_id: Option<String>,
    /// Operator-selected subscription paths from onboarding discovery.
    #[serde(default)]
    pub selected_paths: Vec<SelectedSubscriptionPath>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq)]
pub struct SelectedSubscriptionPath {
    pub path: String,
    #[serde(default)]
    pub origin: String,
    pub mode: String,
    #[serde(default)]
    pub sample_interval_ns: u64,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub optional: bool,
}

impl TargetConfig {
    pub fn resolved_username(&self) -> Option<String> {
        if let Some(ref key) = self.username_env {
            return std::env::var(key).ok();
        }
        self.username.clone()
    }

    pub fn resolved_password(&self) -> Option<String> {
        if let Some(ref key) = self.password_env {
            return std::env::var(key).ok();
        }
        self.password.clone()
    }

    pub fn uses_tls(&self) -> bool {
        self.ca_cert.is_some()
    }
}

pub async fn load(path: &str) -> Result<Config> {
    let text = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("cannot read config '{path}' — copy bonsai.toml.example to bonsai.toml and fill in your targets"))?;
    toml::from_str(&text).context("TOML parse error in config file")
}

#[cfg(test)]
mod tests {
    use super::{Config, RuntimeConfig, RuntimeMode};

    #[test]
    fn runtime_mode_accepts_the_three_supported_modes() {
        for (mode, expected) in [
            ("all", RuntimeMode::All),
            ("core", RuntimeMode::Core),
            ("collector", RuntimeMode::Collector),
        ] {
            let cfg = RuntimeConfig {
                mode: mode.to_string(),
                ..Default::default()
            };
            assert_eq!(cfg.parsed_mode().unwrap(), expected);
        }
    }

    #[test]
    fn runtime_mode_rejects_unknown_values() {
        let cfg = RuntimeConfig {
            mode: "sidecar".to_string(),
            ..Default::default()
        };
        assert!(cfg.parsed_mode().is_err());
    }

    #[test]
    fn runtime_tls_config_deserializes_under_runtime() {
        let cfg: Config = toml::from_str(
            r#"
graph_path = "bonsai.db"

[runtime]
mode = "collector"
core_ingest_endpoint = "https://127.0.0.1:50051"

[runtime.tls]
enabled = true
ca_cert = "config/tls/ca.pem"
cert = "config/tls/collector.pem"
key = "config/tls/collector-key.pem"
server_name = "bonsai-core.local"

[[target]]
address = "127.0.0.1:57400"
"#,
        )
        .unwrap();

        assert!(cfg.runtime.tls.enabled);
        assert_eq!(
            cfg.runtime.tls.server_name.as_deref(),
            Some("bonsai-core.local")
        );
    }
}
