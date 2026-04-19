use anyhow::{Context, Result};
use tracing::{info, warn};
use time;
use axum;

use bonsai::{api::{BonsaiGraphServer, BonsaiService, TargetConnInfo}, config, graph, retention, registry::{self, DeviceRegistry}, subscriber, telemetry};
use metrics_exporter_prometheus::PrometheusBuilder;

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

    info!("bonsai starting — Phase 4: Detect-Predict-Heal");

    let cfg = config::load(CONFIG_PATH).await?;

    // Install Prometheus metrics exporter (disabled when metrics_addr is empty)
    if !cfg.metrics_addr.is_empty() {
        let metrics_addr: std::net::SocketAddr = cfg.metrics_addr.parse()
            .with_context(|| format!("invalid metrics_addr '{}'", cfg.metrics_addr))?;
        PrometheusBuilder::new()
            .with_http_listener(metrics_addr)
            .install()
            .context("failed to install Prometheus metrics exporter")?;
        info!(%metrics_addr, "Prometheus metrics listening");
    }
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
            t.hostname.clone(),
            t.tls_domain.clone().unwrap_or_default(),
            ca_cert_pem,
            tx.clone(),
        );
        let rx = shutdown_rx.clone();
        handles.push(tokio::spawn(async move { sub.run_forever(rx).await }));
    }

    // Build TargetConnInfo vec for PushRemediation credential lookup.
    let mut target_conn_infos: Vec<TargetConnInfo> = Vec::new();
    for t in &cfg.target {
        let ca_cert_pem = match &t.ca_cert {
            Some(path) => tokio::fs::read(path).await.ok(),
            None => None,
        };
        target_conn_infos.push(TargetConnInfo {
            address:     t.address.clone(),
            username:    t.resolved_username(),
            password:    t.resolved_password(),
            ca_cert_pem,
            tls_domain:  t.tls_domain.clone().unwrap_or_default(),
        });
    }

    // Start gRPC API server
    let api_addr = cfg.api_addr.parse()
        .with_context(|| format!("invalid api_addr '{}'", cfg.api_addr))?;
    let svc = BonsaiGraphServer::new(BonsaiService::new(std::sync::Arc::clone(&graph), target_conn_infos));
    info!(%api_addr, "gRPC API server listening");
    tokio::spawn(async move {
        if let Err(e) = tonic::transport::Server::builder()
            .add_service(svc)
            .serve(api_addr)
            .await
        {
            warn!(error = %e, "gRPC server error");
        }
    });

    // Start HTTP UI server (Axum) on port 3000
    {
        let http_store = std::sync::Arc::clone(&graph);
        let http_addr: std::net::SocketAddr = "0.0.0.0:3000".parse().unwrap();
        info!(%http_addr, "HTTP UI server listening");
        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(http_addr).await
                .expect("failed to bind HTTP port 3000");
            axum::serve(listener, bonsai::http_server::router(http_store))
                .await
                .expect("HTTP server error");
        });
    }

    // Retention: prune old StateChangeEvents on a 1-hour interval (disabled by default)
    if cfg.retention.enabled {
        let store       = std::sync::Arc::clone(&graph);
        let max_age_h   = cfg.retention.max_age_hours;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                let cutoff = time::OffsetDateTime::now_utc()
                    - time::Duration::hours(max_age_h as i64);
                if let Err(e) = retention::prune_events(std::sync::Arc::clone(&store), cutoff).await {
                    warn!(error = %e, "retention prune failed");
                }
            }
        });
    }

    // DeviceRegistry: consume change events (seam for future dynamic onboarding)
    let reg = registry::FileRegistry::new(cfg.target.clone());
    let mut change_rx = reg.subscribe_changes();
    tokio::spawn(async move {
        while let Some(change) = change_rx.recv().await {
            match change {
                registry::RegistryChange::Added(t)   => info!(address = %t.address, "registry: device added"),
                registry::RegistryChange::Removed(a) => info!(address = %a, "registry: device removed"),
                registry::RegistryChange::Updated(t) => info!(address = %t.address, "registry: device updated"),
            }
        }
    });

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
