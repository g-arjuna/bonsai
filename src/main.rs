use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{info, warn};

use bonsai::{
    api::{BonsaiGraphServer, BonsaiService},
    archive, config,
    config::TargetConfig,
    event_bus::InProcessBus,
    graph, ingest,
    registry::{ApiRegistry, DeviceRegistry, RegistryChange},
    retention, subscriber,
    subscription_status::{self, SubscriptionPlan},
    telemetry::TelemetryEvent,
};
use metrics_exporter_prometheus::PrometheusBuilder;

const CONFIG_PATH: &str = "bonsai.toml";
const GRAPH_PATH_DEFAULT: &str = "bonsai.db";
const REGISTRY_PATH: &str = "bonsai-registry.json";

type SubscriberHandleMap = HashMap<
    String,
    (
        tokio::sync::watch::Sender<bool>,
        tokio::task::JoinHandle<()>,
    ),
>;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("bonsai=debug".parse()?),
        )
        .init();

    info!("bonsai starting - distributed runtime capable");

    let cfg = config::load(CONFIG_PATH).await?;
    let runtime_mode = cfg.runtime.parsed_mode()?;
    let run_core = runtime_mode.runs_core();
    let run_collector = runtime_mode.runs_collector();
    info!(
        mode = ?runtime_mode,
        run_core,
        run_collector,
        "runtime mode selected"
    );

    if run_core && !cfg.metrics_addr.is_empty() {
        let metrics_addr: std::net::SocketAddr = cfg
            .metrics_addr
            .parse()
            .with_context(|| format!("invalid metrics_addr '{}'", cfg.metrics_addr))?;
        PrometheusBuilder::new()
            .with_http_listener(metrics_addr)
            .install()
            .context("failed to install Prometheus metrics exporter")?;
        info!(%metrics_addr, "Prometheus metrics listening");
    }

    let bus = InProcessBus::new(cfg.event_bus.capacity);
    let debounce_secs = cfg.event_bus.counter_debounce_secs;

    let graph = if run_core {
        let graph_path = if cfg.graph_path.is_empty() {
            GRAPH_PATH_DEFAULT
        } else {
            cfg.graph_path.as_str()
        };

        Some(std::sync::Arc::new(
            tokio::task::spawn_blocking({
                let p = graph_path.to_string();
                move || graph::GraphStore::open(&p)
            })
            .await
            .context("graph open panicked")?
            .context("graph open failed")?,
        ))
    } else {
        info!("collector-only mode selected; graph store and local API are disabled");
        None
    };

    if let Some(graph) = &graph {
        let graph_writer = std::sync::Arc::clone(graph);
        let mut rx = bus.subscribe();
        tokio::spawn(async move {
            let mut last_counter_write: HashMap<String, Instant> = HashMap::new();
            let debounce = Duration::from_secs(debounce_secs);

            loop {
                match rx.recv().await {
                    Ok(update) => {
                        let classified = update.classify();
                        let is_counter =
                            matches!(classified, TelemetryEvent::InterfaceStats { .. });

                        if is_counter {
                            let key = format!(
                                "{}:{}",
                                update.target,
                                if let TelemetryEvent::InterfaceStats { ref if_name } = classified {
                                    if_name.clone()
                                } else {
                                    String::new()
                                }
                            );
                            let now = Instant::now();
                            let skip = last_counter_write
                                .get(&key)
                                .is_some_and(|t| now.duration_since(*t) < debounce);
                            if skip {
                                continue;
                            }
                            last_counter_write.insert(key, now);
                        }

                        if let Err(error) = graph_writer.write(update).await {
                            warn!(%error, "graph write failed");
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
                        warn!(dropped, "graph writer lagged on event bus");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        info!("event bus closed - graph writer stopping");
                        break;
                    }
                }
            }
        });
    }

    if cfg.archive.enabled && run_collector {
        let archive_root = std::path::PathBuf::from(&cfg.archive.path);
        let flush_interval = Duration::from_secs(cfg.archive.flush_interval_seconds);
        let max_batch_rows = cfg.archive.max_batch_rows;
        let bus_for_archive = std::sync::Arc::clone(&bus);
        tokio::spawn(async move {
            if let Err(error) = archive::run_archiver(
                bus_for_archive,
                archive_root,
                flush_interval,
                max_batch_rows,
            )
            .await
            {
                warn!(%error, "archive consumer stopped");
            }
        });
    } else if cfg.archive.enabled {
        info!(
            "archive enabled but runtime mode has no collector role; skipping collector-local archive"
        );
    }

    let registry = std::sync::Arc::new(ApiRegistry::open(REGISTRY_PATH, cfg.target.clone())?);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let subscription_plan_tx = if let Some(graph) = &graph {
        let (subscription_plan_tx, subscription_plan_rx) =
            tokio::sync::mpsc::channel::<SubscriptionPlan>(128);
        let verifier_store = std::sync::Arc::clone(graph);
        let verifier_bus = std::sync::Arc::clone(&bus);
        let verifier_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            subscription_status::run_subscription_verifier(
                verifier_store,
                verifier_bus,
                subscription_plan_rx,
                verifier_shutdown,
            )
            .await;
        });
        Some(subscription_plan_tx)
    } else {
        None
    };

    let subscriber_manager = if run_collector {
        let registry = std::sync::Arc::clone(&registry);
        let bus = std::sync::Arc::clone(&bus);
        let subscription_plan_tx = subscription_plan_tx.clone();
        let mut shutdown = shutdown_rx.clone();
        Some(tokio::spawn(async move {
            let mut change_rx = registry.subscribe_changes();
            let mut subscribers: SubscriberHandleMap = HashMap::new();

            match registry.list_active() {
                Ok(targets) => {
                    for target in targets {
                        if let Err(error) = spawn_subscriber(
                            target,
                            &bus,
                            subscription_plan_tx.as_ref(),
                            &mut subscribers,
                        )
                        .await
                        {
                            warn!(%error, "initial subscriber start failed");
                        }
                    }
                }
                Err(error) => warn!(%error, "failed to list managed devices at startup"),
            }

            loop {
                tokio::select! {
                    _ = shutdown.changed() => {
                        info!("subscriber manager received shutdown");
                        break;
                    }
                    maybe_change = change_rx.recv() => {
                        let Some(change) = maybe_change else {
                            info!("registry change channel closed");
                            break;
                        };

                        match change {
                            RegistryChange::Added(target) => {
                                if let Err(error) = spawn_subscriber(target, &bus, subscription_plan_tx.as_ref(), &mut subscribers).await {
                                    warn!(%error, "failed to start subscriber for added device");
                                }
                            }
                            RegistryChange::Updated(target) => {
                                if let Err(error) = restart_subscriber(target, &bus, subscription_plan_tx.as_ref(), &mut subscribers).await {
                                    warn!(%error, "failed to restart subscriber for updated device");
                                }
                            }
                            RegistryChange::Removed(address) => {
                                stop_subscriber(&address, &mut subscribers).await;
                            }
                        }
                    }
                }
            }

            stop_all_subscribers(&mut subscribers).await;
        }))
    } else {
        None
    };

    if run_collector && !run_core {
        let forwarder_bus = std::sync::Arc::clone(&bus);
        let core_endpoint = cfg.runtime.core_ingest_endpoint.clone();
        let collector_id = cfg.runtime.collector_id.clone();
        let forwarder_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            ingest::run_core_forwarder(
                forwarder_bus,
                core_endpoint,
                collector_id,
                forwarder_shutdown,
            )
            .await;
        });
    }

    if let Some(graph) = &graph {
        let api_addr = cfg
            .api_addr
            .parse()
            .with_context(|| format!("invalid api_addr '{}'", cfg.api_addr))?;
        let svc = BonsaiGraphServer::new(BonsaiService::new(
            std::sync::Arc::clone(graph),
            std::sync::Arc::clone(&registry),
            std::sync::Arc::clone(&bus),
        ));
        info!(%api_addr, "gRPC API and telemetry ingest server listening");
        tokio::spawn(async move {
            if let Err(error) = tonic::transport::Server::builder()
                .add_service(svc)
                .serve(api_addr)
                .await
            {
                warn!(%error, "gRPC server error");
            }
        });

        let http_store = std::sync::Arc::clone(graph);
        let http_addr: std::net::SocketAddr = "0.0.0.0:3000".parse().unwrap();
        info!(%http_addr, "HTTP UI server listening");
        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(http_addr)
                .await
                .expect("failed to bind HTTP port 3000");
            axum::serve(
                listener,
                bonsai::http_server::router(http_store, std::sync::Arc::clone(&registry)),
            )
            .await
            .expect("HTTP server error");
        });

        if cfg.retention.enabled {
            let store = std::sync::Arc::clone(graph);
            let max_age_h = cfg.retention.max_age_hours;
            let max_count = cfg.retention.max_state_change_events;
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(3600));
                loop {
                    interval.tick().await;
                    let cutoff =
                        time::OffsetDateTime::now_utc() - time::Duration::hours(max_age_h as i64);
                    if let Err(error) =
                        retention::prune_events(std::sync::Arc::clone(&store), cutoff).await
                    {
                        warn!(%error, "retention age-prune failed");
                    }
                    if let Err(error) =
                        retention::prune_events_by_count(std::sync::Arc::clone(&store), max_count)
                            .await
                    {
                        warn!(%error, "retention count-prune failed");
                    }
                }
            });
        }
    }

    tokio::signal::ctrl_c().await?;
    info!("Ctrl+C received - shutting down");
    let _ = shutdown_tx.send(true);
    if let Some(subscriber_manager) = subscriber_manager {
        let _ = subscriber_manager.await;
    }

    if let Some(graph) = &graph {
        graph::log_graph_summary(graph.db().as_ref());
    }
    info!("bonsai stopped");
    Ok(())
}

async fn spawn_subscriber(
    target: TargetConfig,
    bus: &std::sync::Arc<InProcessBus>,
    subscription_plan_tx: Option<&tokio::sync::mpsc::Sender<SubscriptionPlan>>,
    subscribers: &mut SubscriberHandleMap,
) -> Result<()> {
    let address = target.address.clone();
    if subscribers.contains_key(&address) {
        info!(address = %address, "subscriber already running");
        return Ok(());
    }

    let ca_cert_pem = load_ca_cert_pem(&target).await?;
    let subscriber = subscriber::GnmiSubscriber::new(
        target.address.clone(),
        target.resolved_username(),
        target.resolved_password(),
        target.vendor.clone(),
        target.hostname.clone(),
        target.tls_domain.clone().unwrap_or_default(),
        ca_cert_pem,
        std::sync::Arc::clone(bus),
        subscription_plan_tx.cloned(),
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(async move { subscriber.run_forever(shutdown_rx).await });
    subscribers.insert(address.clone(), (shutdown_tx, handle));
    info!(address = %address, "subscriber started");
    Ok(())
}

async fn restart_subscriber(
    target: TargetConfig,
    bus: &std::sync::Arc<InProcessBus>,
    subscription_plan_tx: Option<&tokio::sync::mpsc::Sender<SubscriptionPlan>>,
    subscribers: &mut SubscriberHandleMap,
) -> Result<()> {
    let address = target.address.clone();
    stop_subscriber(&address, subscribers).await;
    spawn_subscriber(target, bus, subscription_plan_tx, subscribers).await
}

async fn stop_subscriber(address: &str, subscribers: &mut SubscriberHandleMap) {
    if let Some((shutdown_tx, handle)) = subscribers.remove(address) {
        let _ = shutdown_tx.send(true);
        let _ = handle.await;
        info!(address = %address, "subscriber stopped");
    }
}

async fn stop_all_subscribers(subscribers: &mut SubscriberHandleMap) {
    let addresses: Vec<String> = subscribers.keys().cloned().collect();
    for address in addresses {
        stop_subscriber(&address, subscribers).await;
    }
}

async fn load_ca_cert_pem(target: &TargetConfig) -> Result<Option<Vec<u8>>> {
    match &target.ca_cert {
        Some(path) => {
            let bytes = tokio::fs::read(path)
                .await
                .with_context(|| format!("could not read CA cert from '{path}'"))?;
            Ok(Some(bytes))
        }
        None => Ok(None),
    }
}
