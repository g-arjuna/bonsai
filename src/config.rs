use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct Config {
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
    pub target: Vec<TargetConfig>,
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

fn default_api_addr() -> String {
    "[::1]:50051".to_string()
}

fn default_metrics_addr() -> String {
    "[::1]:9090".to_string()
}

#[derive(Deserialize, Clone)]
pub struct TargetConfig {
    pub address: String,
    /// TLS server name (SNI). Required when ca_cert is set.
    pub tls_domain: Option<String>,
    /// Path to PEM CA cert. Enables TLS for this target.
    pub ca_cert: Option<String>,
    /// Override vendor detection. If absent, Capabilities RPC auto-detects.
    pub vendor: Option<String>,
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
