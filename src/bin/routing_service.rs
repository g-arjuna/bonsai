//! B10: Reduced Bonsai routing service binary (`bonsai-routing`).
//! Runs the canonical BMP listener (`rbmp-core`), composite-keyed RIB (`rbmp-rib`),
//! durable raw + event journals (`rbmp-store`), and internal routing API (`/internal/routing/v1/*`)
//! without Kafka, NATS, or ML sidecars.

use bonsai::routing_service::{
    RoutingServiceConfig, RoutingServiceState, build_internal_routing_router, run_bmp_listener,
};
use tokio::net::TcpListener;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .init();

    let cfg = RoutingServiceConfig::default();
    cfg.validate_deployment()
        .map_err(|e| anyhow::anyhow!("Invalid routing service deployment configuration: {e}"))?;

    // One executable owns each durable journal pair. Keep leases until process exit;
    // a second service must not allocate independent sequences into the same files.
    let mut _journal_leases = Vec::new();
    for path in [&cfg.bmp_journal_path, &cfg.event_journal_path]
        .into_iter()
        .flatten()
    {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let lease = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        lease
            .try_lock()
            .map_err(|e| anyhow::anyhow!("Cannot lock routing journal {}: {e}", path.display()))?;
        _journal_leases.push(lease);
    }

    let state = RoutingServiceState::new(cfg.clone()).await?;
    let bmp_listener = TcpListener::bind(&cfg.bmp_listen_addr).await?;
    let http_listener = TcpListener::bind(&cfg.http_listen_addr).await?;

    info!(
        bmp_addr = %bmp_listener.local_addr()?,
        http_addr = %http_listener.local_addr()?,
        "Started reduced bonsai-routing service"
    );

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let bmp_state = std::sync::Arc::clone(&state);
    let bmp_handle = tokio::spawn(async move {
        run_bmp_listener(bmp_listener, bmp_state, shutdown_rx).await;
    });

    let app = build_internal_routing_router(state);
    axum::serve(http_listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = shutdown_tx.send(true);
        })
        .await?;

    let _ = bmp_handle.await;
    Ok(())
}
