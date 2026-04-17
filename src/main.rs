use anyhow::{Context, Result};
use tracing::{info, warn};

use bonsai::{config, graph, subscriber, telemetry};

const CONFIG_PATH: &str = "bonsai.toml";
const GRAPH_PATH_DEFAULT: &str = "bonsai.db";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("bonsai=debug".parse()?),
        )
        .init();

    info!("bonsai starting — Phase 2: The Graph");

    let cfg = config::load(CONFIG_PATH).await?;
    let graph_path = if cfg.graph_path.is_empty() {
        GRAPH_PATH_DEFAULT
    } else {
        cfg.graph_path.as_str()
    };

    let graph = std::sync::Arc::new(
        tokio::task::spawn_blocking({
            let p = graph_path.to_string();
            move || graph::GraphStore::open(&p)
        })
        .await
        .context("graph open panicked")?
        .context("graph open failed")?,
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel::<telemetry::TelemetryUpdate>(1024);

    let graph_writer = std::sync::Arc::clone(&graph);
    tokio::spawn(async move {
        while let Some(update) = rx.recv().await {
            if let Err(e) = graph_writer.write(update).await {
                warn!(error = %e, "graph write failed");
            }
        }
        info!("graph writer stopped");
    });

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let mut handles = Vec::new();
    for t in &cfg.target {
        let ca_cert_pem = match &t.ca_cert {
            Some(path) => {
                let bytes = tokio::fs::read(path)
                    .await
                    .with_context(|| format!("could not read CA cert from '{path}'"))?;
                Some(bytes)
            }
            None => None,
        };

        let sub = subscriber::GnmiSubscriber::new(
            t.address.clone(),
            t.resolved_username(),
            t.resolved_password(),
            t.vendor.clone(),
            t.tls_domain.clone().unwrap_or_default(),
            ca_cert_pem,
            tx.clone(),
        );
        let rx = shutdown_rx.clone();
        handles.push(tokio::spawn(async move { sub.run_forever(rx).await }));
    }

    tokio::signal::ctrl_c().await?;
    info!("Ctrl+C received — shutting down");
    let _ = shutdown_tx.send(true);

    for handle in handles {
        let _ = handle.await;
    }

    graph::log_graph_summary(graph.db().as_ref());
    info!("bonsai stopped");
    Ok(())
}
