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
    pub target: Vec<TargetConfig>,
}

fn default_api_addr() -> String {
    "[::1]:50051".to_string()
}

fn default_metrics_addr() -> String {
    "[::1]:9090".to_string()
}

#[derive(Deserialize)]
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
