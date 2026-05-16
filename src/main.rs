#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use anyhow::Result;

mod cli;
mod server_startup;

const CONFIG_PATH: &str = "bonsai.toml";
const GRAPH_PATH_DEFAULT: &str = "bonsai.db";
const REGISTRY_PATH: &str = "bonsai-registry.json";

#[tokio::main]
async fn main() -> Result<()> {
    install_rustls_crypto_provider();

    if cli::SelfTestCliCommand::parse() {
        return cli::run_self_test().await;
    }
    if let Some(command) = cli::AuditCliCommand::parse()? {
        return cli::run_audit_cli(command).await;
    }
    if let Some(command) = cli::DeviceCliCommand::parse()? {
        return cli::run_device_cli(command).await;
    }
    if let Some(command) = cli::CatalogueCliCommand::parse()? {
        return cli::run_catalogue_cli(command).await;
    }
    if let Some(command) = cli::YangCliCommand::parse()? {
        return cli::run_yang_cli(command).await;
    }


    server_startup::run_server().await
}

fn config_path() -> String {
    std::env::var("BONSAI_CONFIG")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| CONFIG_PATH.to_string())
}

fn install_rustls_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
