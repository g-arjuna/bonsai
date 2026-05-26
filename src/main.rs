#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use anyhow::Result;
use std::path::{Path, PathBuf};

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
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--config=") {
            if !value.trim().is_empty() {
                return value.to_string();
            }
        }
        if arg == "--config"
            && let Some(value) = args.next()
            && !value.trim().is_empty()
        {
            return value;
        }
    }

    std::env::var("BONSAI_CONFIG")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| CONFIG_PATH.to_string())
}

fn registry_path_for_graph_path(graph_path: &str) -> PathBuf {
    let effective_graph_path = if graph_path.trim().is_empty() {
        GRAPH_PATH_DEFAULT
    } else {
        graph_path
    };
    let graph_path = Path::new(effective_graph_path);
    let runtime_dir = graph_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    runtime_dir.join(REGISTRY_PATH)
}

fn install_rustls_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
