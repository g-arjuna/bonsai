use std::collections::HashMap;
use std::fs;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{info, warn};

use lru::LruCache;

use bonsai::{
    api::{
        BonsaiGraphServer, CollectorService, CoreService,
        pb::{
            AddDeviceRequest, ListManagedDevicesRequest, ManagedDevice, RemoveDeviceRequest,
            UpdateDeviceRequest, bonsai_graph_client::BonsaiGraphClient,
        },
    },
    archive, audit, catalogue, config,
    config::{resolve_buffer_pool_collector, resolve_buffer_pool_core},
    output::OutputAdapter,
    config::TargetConfig,
    credentials::{CredentialVault, ResolvePurpose, ResolvedCredential},
    event_bus::InProcessBus,
    graph, ingest,
    registry::{ApiRegistry, DeviceRegistry, RegistryChange},
    retention,
    store::BonsaiStore,
    subscriber::{self, SubscriberHandleMap, stop_all_subscribers, stop_subscriber},
    subscription_status::{self, SubscriptionPlan},
    telemetry::TelemetryEvent,
};
use metrics_exporter_prometheus::PrometheusBuilder;
use tonic::codec::CompressionEncoding;
use tonic::transport::{Certificate, Identity, ServerTlsConfig};

const CONFIG_PATH: &str = "bonsai.toml";
const GRAPH_PATH_DEFAULT: &str = "bonsai.db";
const REGISTRY_PATH: &str = "bonsai-registry.json";

#[tokio::main]
async fn main() -> Result<()> {
    install_rustls_crypto_provider();

    if SelfTestCliCommand::parse() {
        return run_self_test().await;
    }
    if let Some(command) = AuditCliCommand::parse()? {
        return run_audit_cli(command).await;
    }
    if let Some(command) = DeviceCliCommand::parse()? {
        return run_device_cli(command).await;
    }
    if let Some(command) = CatalogueCliCommand::parse()? {
        return run_catalogue_cli(command).await;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("bonsai=debug".parse()?),
        )
        .init();

    info!(
        protocol_version = bonsai::api::PROTOCOL_VERSION,
        "bonsai starting"
    );

    let startup_start = Instant::now();

    let t = Instant::now();
    let config_path = config_path();
    let cfg = config::load(&config_path).await?;
    info!(phase = "config_load", elapsed_ms = t.elapsed().as_millis() as u64, "startup");

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

    #[derive(Clone)]
    enum Store {
        Core(std::sync::Arc<graph::GraphStore>),
        Collector(std::sync::Arc<bonsai::collector::graph::CollectorGraphStore>),
    }

    #[tonic::async_trait]
    impl BonsaiStore for Store {
        fn db(&self) -> std::sync::Arc<lbug::Database> {
            match self {
                Store::Core(s) => s.db(),
                Store::Collector(s) => s.db(),
            }
        }
        fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<graph::BonsaiEvent> {
            match self {
                Store::Core(s) => s.subscribe_events(),
                Store::Collector(s) => s.subscribe_events(),
            }
        }
        async fn write(&self, update: bonsai::telemetry::TelemetryUpdate) -> Result<()> {
            match self {
                Store::Core(s) => s.write(update).await,
                Store::Collector(s) => s.write(update).await,
            }
        }
        async fn write_detection(
            &self,
            device_address: String,
            rule_id: String,
            severity: String,
            features_json: String,
            fired_at_ns: i64,
            state_change_event_id: String,
        ) -> Result<String> {
            match self {
                Store::Core(s) => {
                    s.write_detection(
                        device_address,
                        rule_id,
                        severity,
                        features_json,
                        fired_at_ns,
                        state_change_event_id,
                    )
                    .await
                }
                Store::Collector(s) => {
                    s.write_detection(
                        device_address,
                        rule_id,
                        severity,
                        features_json,
                        fired_at_ns,
                        state_change_event_id,
                    )
                    .await
                }
            }
        }
        async fn write_remediation(
            &self,
            detection_id: String,
            action: String,
            status: String,
            detail_json: String,
            attempted_at_ns: i64,
            completed_at_ns: i64,
        ) -> Result<String> {
            match self {
                Store::Core(s) => {
                    s.write_remediation(
                        detection_id,
                        action,
                        status,
                        detail_json,
                        attempted_at_ns,
                        completed_at_ns,
                    )
                    .await
                }
                Store::Collector(s) => {
                    s.write_remediation(
                        detection_id,
                        action,
                        status,
                        detail_json,
                        attempted_at_ns,
                        completed_at_ns,
                    )
                    .await
                }
            }
        }
        async fn sync_sites_from_targets(&self, targets: Vec<TargetConfig>) -> Result<()> {
            match self {
                Store::Core(s) => s.sync_sites_from_targets(targets).await,
                Store::Collector(s) => s.sync_sites_from_targets(targets).await,
            }
        }
        async fn list_sites(&self) -> Result<Vec<graph::SiteRecord>> {
            match self {
                Store::Core(s) => s.list_sites().await,
                Store::Collector(s) => s.list_sites().await,
            }
        }
        async fn upsert_site(&self, site: graph::SiteRecord) -> Result<graph::SiteRecord> {
            match self {
                Store::Core(s) => s.upsert_site(site).await,
                Store::Collector(s) => s.upsert_site(site).await,
            }
        }
        async fn write_subscription_status(
            &self,
            status: graph::SubscriptionStatusWrite,
        ) -> Result<()> {
            match self {
                Store::Core(s) => s.write_subscription_status(status).await,
                Store::Collector(s) => s.write_subscription_status(status).await,
            }
        }
        fn publish_event(&self, event: graph::BonsaiEvent) {
            match self {
                Store::Core(s) => s.publish_event(event),
                Store::Collector(s) => s.publish_event(event),
            }
        }
    }

    let store = if run_core {
        let graph_path = if cfg.graph_path.is_empty() {
            GRAPH_PATH_DEFAULT
        } else {
            cfg.graph_path.as_str()
        };
        let pool_bytes = resolve_buffer_pool_core(cfg.graph.buffer_pool_bytes);

        let t = Instant::now();
        let s = tokio::task::spawn_blocking({
            let p = graph_path.to_string();
            move || graph::GraphStore::open(&p, pool_bytes)
        })
        .await
        .context("graph open panicked")?
        .context("graph open failed")?;
        info!(phase = "graph_open", elapsed_ms = t.elapsed().as_millis() as u64, "startup");
        Some(Store::Core(std::sync::Arc::new(s)))
    } else if run_collector {
        let graph_path = if cfg.collector.graph_path.is_empty() {
            "runtime/collector.db"
        } else {
            cfg.collector.graph_path.as_str()
        };
        let pool_bytes = resolve_buffer_pool_collector(cfg.graph.buffer_pool_bytes);

        let t = Instant::now();
        let s = tokio::task::spawn_blocking({
            let p = graph_path.to_string();
            move || bonsai::collector::graph::CollectorGraphStore::open(&p, pool_bytes)
        })
        .await
        .context("collector graph open panicked")?
        .context("collector graph open failed")?;
        info!(phase = "graph_open", elapsed_ms = t.elapsed().as_millis() as u64, "startup");
        Some(Store::Collector(std::sync::Arc::new(s)))
    } else {
        None
    };

    if let Some(ref store) = store {
        let store_writer = store.clone();
        let mut rx = bus.subscribe();
        tokio::spawn(async move {
            let mut last_counter_write: LruCache<String, Instant> =
                LruCache::new(NonZeroUsize::new(1024).unwrap());
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
                                .peek(&key)
                                .is_some_and(|t| now.duration_since(*t) < debounce);
                            if skip {
                                continue;
                            }
                            last_counter_write.put(key, now);
                        }

                        if let Err(error) = store_writer.write(update).await {
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

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    if cfg.archive.enabled && run_collector {
        let archive_root = std::path::PathBuf::from(&cfg.archive.path);
        let flush_interval = Duration::from_secs(cfg.archive.flush_interval_seconds);
        let max_batch_rows = cfg.archive.max_batch_rows;
        let compression_level = cfg.archive.compression_level;
        let writer_max_idle_secs = cfg.archive.writer_max_idle_secs;
        let bus_for_archive = std::sync::Arc::clone(&bus);
        let archive_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            if let Err(error) = archive::run_archiver(
                bus_for_archive,
                archive_root,
                flush_interval,
                max_batch_rows,
                compression_level,
                writer_max_idle_secs,
                archive_shutdown,
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

    // Memory profiler — always on when run_core, sampling every 60s
    if run_core {
        let mem_shutdown = shutdown_rx.clone();
        tokio::spawn(bonsai::memory_profile::run_memory_profiler(
            Duration::from_secs(60),
            None,
            mem_shutdown,
        ));
    }

    // Disk guard
    let storage_config_for_http = cfg.storage.clone();
    if run_core && (cfg.storage.max_archive_bytes > 0 || cfg.storage.max_graph_bytes > 0) {
        let archive_path = std::path::PathBuf::from(&cfg.archive.path);
        let graph_path = std::path::PathBuf::from(&cfg.graph_path);
        let storage_cfg = cfg.storage;
        let dg_shutdown = shutdown_rx.clone();
        tokio::spawn(bonsai::disk_guard::run_disk_guard(
            archive_path,
            graph_path,
            storage_cfg,
            dg_shutdown,
        ));
    }

    let registry = std::sync::Arc::new(ApiRegistry::open(REGISTRY_PATH, cfg.target.clone())?);
    let credentials = std::sync::Arc::new(CredentialVault::open(
        &cfg.credentials.path,
        &cfg.credentials.passphrase_env,
    )?);

    let collector_manager = if run_core {
        Some(std::sync::Arc::new(
            bonsai::assignment::CollectorManager::new(
                std::sync::Arc::clone(&registry),
                std::sync::Arc::clone(&credentials),
                cfg.assignment.rules.clone(),
            ),
        ))
    } else {
        None
    };

    if credentials.is_unlocked()? {
        info!(
            path = %cfg.credentials.path,
            "credential vault unlocked"
        );
    } else {
        info!(
            path = %cfg.credentials.path,
            passphrase_env = %cfg.credentials.passphrase_env,
            "credential vault locked; alias-based credentials are unavailable until restart with passphrase"
        );
    }
    if let Some(ref store) = store {
        match registry.list_active() {
            Ok(targets) => {
                store
                    .sync_sites_from_targets(targets)
                    .await
                    .context("failed to sync registry sites into graph")?;
                info!("registry site labels synced into graph");
            }
            Err(error) => warn!(%error, "failed to list managed devices for site graph sync"),
        }
    }

    if let Some(Store::Core(ref core_store)) = store {
        match core_store.migrate_sites_to_default_environment() {
            Ok(count) if count > 0 => info!(count, "environment migration complete"),
            Ok(_) => {}
            Err(error) => warn!(%error, "environment migration failed (non-fatal)"),
        }
    }

    // Seed the collector manager's site cache and keep it refreshed so that
    // hierarchy-aware assignment rules reflect current graph state.
    if let (Some(store), Some(manager)) = (&store, &collector_manager) {
        match store.list_sites().await {
            Ok(sites) => manager.set_sites(sites),
            Err(e) => warn!(%e, "failed to seed assignment site cache"),
        }
        let manager_for_refresh = std::sync::Arc::clone(manager);
        let store_for_refresh = store.clone();
        let mut refresh_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        match store_for_refresh.list_sites().await {
                            Ok(sites) => manager_for_refresh.set_sites(sites),
                            Err(e) => warn!(%e, "site cache refresh failed"),
                        }
                    }
                    _ = refresh_shutdown.changed() => break,
                }
            }
        });
    }

    let subscription_plan_tx = if let Some(ref store) = store {
        let (subscription_plan_tx, subscription_plan_rx) =
            tokio::sync::mpsc::channel::<SubscriptionPlan>(128);
        let verifier_store = store.clone();
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

    let subscriber_manager = if runtime_mode == bonsai::config::RuntimeMode::All {
        let registry = std::sync::Arc::clone(&registry);
        let credentials = std::sync::Arc::clone(&credentials);
        let bus = std::sync::Arc::clone(&bus);
        let subscription_plan_tx = subscription_plan_tx.clone();
        let mut shutdown = shutdown_rx.clone();
        Some(tokio::spawn(async move {
            let mut change_rx = registry.subscribe_changes();
            let mut subscribers: SubscriberHandleMap = HashMap::new();

            match registry.list_active() {
                Ok(targets) => {
                    for target in targets {
                        if !target.enabled {
                            info!(address = %target.address, "subscriber disabled by registry");
                            continue;
                        }
                        if let Err(error) = spawn_subscriber(
                            target,
                            &credentials,
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
                                if let Err(error) = spawn_subscriber(target, &credentials, &bus, subscription_plan_tx.as_ref(), &mut subscribers).await {
                                    warn!(%error, "failed to start subscriber for added device");
                                }
                            }
                            RegistryChange::Updated(target) => {
                                if target.enabled {
                                    if let Err(error) = restart_subscriber(target, &credentials, &bus, subscription_plan_tx.as_ref(), &mut subscribers).await {
                                        warn!(%error, "failed to restart subscriber for updated device");
                                    }
                                } else {
                                    stop_subscriber(&target.address, &mut subscribers).await;
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
        let collector_config = cfg.collector.clone();
        let tls_config = cfg.runtime.tls.clone();
        let forwarder_shutdown = shutdown_rx.clone();

        tokio::spawn(async move {
            ingest::run_core_forwarder(
                forwarder_bus,
                core_endpoint,
                collector_id,
                collector_config,
                tls_config,
                forwarder_shutdown,
            )
            .await;
        });

        let collector_cfg = std::sync::Arc::new(cfg.collector.clone());
        let runtime_cfg = std::sync::Arc::new(cfg.runtime.clone());
        let collector_bus = std::sync::Arc::clone(&bus);
        let collector_plan_tx = subscription_plan_tx.clone();
        let collector_shutdown = shutdown_rx.clone();

        tokio::spawn(async move {
            if let Err(error) = ingest::run_collector_manager(
                runtime_cfg,
                collector_cfg,
                collector_bus,
                collector_plan_tx,
                collector_shutdown,
            )
            .await
            {
                warn!(%error, "collector manager failed");
            }
        });

        let diag_port = cfg.collector.diagnostic_port;
        if diag_port > 0 {
            let diag_state = bonsai::collector::diagnostic_server::DiagnosticState::new(
                &cfg.runtime.collector_id,
            );
            let diag_shutdown = shutdown_rx.clone();
            tokio::spawn(bonsai::collector::diagnostic_server::start(
                diag_port,
                diag_state,
                diag_shutdown,
            ));
        }
    }

    if let Some(ref store) = store {
        let api_addr = cfg
            .api_addr
            .parse()
            .with_context(|| format!("invalid api_addr '{}'", cfg.api_addr))?;

        let registry_for_api = std::sync::Arc::clone(&registry);
        let credentials_for_api = std::sync::Arc::clone(&credentials);
        let bus_for_api = std::sync::Arc::clone(&bus);
        let store_for_api = store.clone();
        let collector_manager_for_api = collector_manager.clone();

        let mut server = tonic::transport::Server::builder();
        if cfg.runtime.tls.enabled {
            server = server
                .tls_config(server_tls_config(&cfg.runtime.tls)?)
                .context("failed to configure runtime.tls for gRPC server")?;
            info!(
                %api_addr,
                ingest_compression = "zstd",
                mtls = true,
                "gRPC API and telemetry ingest server listening"
            );
        } else {
            info!(%api_addr, ingest_compression = "zstd", mtls = false, "gRPC API and telemetry ingest server listening");
        }

        tokio::spawn(async move {
            match store_for_api {
                Store::Core(s) => {
                    let svc = BonsaiGraphServer::new(CoreService::new(
                        s,
                        registry_for_api,
                        credentials_for_api,
                        bus_for_api,
                        collector_manager_for_api,
                    ))
                    .accept_compressed(CompressionEncoding::Zstd);
                    if let Err(error) = server.add_service(svc).serve(api_addr).await {
                        warn!(%error, "gRPC core server error");
                    }
                }
                Store::Collector(s) => {
                    let svc = BonsaiGraphServer::new(CollectorService::new(
                        s,
                        registry_for_api,
                        credentials_for_api,
                        bus_for_api,
                        None,
                    ))
                    .accept_compressed(CompressionEncoding::Zstd);
                    if let Err(error) = server.add_service(svc).serve(api_addr).await {
                        warn!(%error, "gRPC collector server error");
                    }
                }
            }
        });

        if run_core {
            let http_store = if let Store::Core(s) = store {
                std::sync::Arc::clone(s)
            } else {
                unreachable!()
            };
            let http_addr: std::net::SocketAddr = "0.0.0.0:3000".parse().unwrap();
            info!(%http_addr, "HTTP UI server listening");
            let registry_for_http = std::sync::Arc::clone(&registry);
            let credentials_for_http = std::sync::Arc::clone(&credentials);
            let collector_manager_for_http = collector_manager.clone();
            let catalogue_dir = "config/path_profiles".to_string();
            let catalogue = std::sync::Arc::new(tokio::sync::RwLock::new(
                catalogue::load_catalogue(std::path::Path::new(&catalogue_dir)),
            ));
            let runtime_dir = "runtime".to_string();
            let enricher_registry =
                bonsai::enrichment::new_registry(std::path::Path::new(&runtime_dir));
            let adapter_registry =
                bonsai::output::traits::new_adapter_registry(std::path::Path::new(&runtime_dir));
            let adapter_registry_for_startup = std::sync::Arc::clone(&adapter_registry);
            let trust_store = bonsai::remediation::trust::new_trust_store(
                std::path::Path::new(&runtime_dir),
                cfg.remediation.clone(),
            );
            let rollback_registry = bonsai::remediation::rollback::new_rollback_registry();
            let remediation_config = cfg.remediation.clone();
            tokio::spawn(async move {
                let listener = tokio::net::TcpListener::bind(http_addr)
                    .await
                    .expect("failed to bind HTTP port 3000");
                axum::serve(
                    listener,
                    bonsai::http_server::router(
                        http_store,
                        registry_for_http,
                        credentials_for_http,
                        collector_manager_for_http,
                        catalogue,
                        catalogue_dir,
                        enricher_registry,
                        adapter_registry,
                        trust_store,
                        rollback_registry,
                        remediation_config,
                        runtime_dir,
                        cfg.archive.path.clone(),
                        cfg.graph_path.clone(),
                        storage_config_for_http,
                    ),
                )
                .await
                .expect("HTTP server error");
            });

            // Start enabled output adapters as background tasks.
            {
                let configs: Vec<_> = {
                    let reg = adapter_registry_for_startup.read().await;
                    reg.list()
                        .into_iter()
                        .filter(|(c, _)| c.enabled)
                        .map(|(c, _)| c)
                        .collect()
                };
                for config in configs {
                    if let Some(adapter) =
                        bonsai::output::prometheus::build(&config)
                    {
                        let bus_for_adapter = std::sync::Arc::clone(&bus);
                        let creds_for_adapter = std::sync::Arc::clone(&credentials);
                        let audit = bonsai::output::traits::OutputAdapterAuditLog::new(
                            std::path::Path::new("runtime"),
                            &config.name,
                        );
                        let adapter_shutdown = shutdown_rx.clone();
                        tokio::spawn(async move {
                            if let Err(e) = adapter
                                .run(bus_for_adapter, creds_for_adapter, audit, adapter_shutdown)
                                .await
                            {
                                warn!(adapter = %adapter.name(), error = %e, "output adapter exited with error");
                            }
                        });
                        info!(adapter = %config.name, "output adapter started");
                    }
                }
            }
        }

        // T2-4: ServiceNow EM push task — start if enabled in [integrations.servicenow]
        if run_core
            && cfg.integrations.servicenow.enabled
            && cfg.integrations.servicenow.em_push_enabled
        {
            let snow_cfg = cfg.integrations.servicenow.clone();
            let creds_for_snow = std::sync::Arc::clone(&credentials);
            let (_, shutdown_rx) = tokio::sync::watch::channel(false);
            let db_for_snow = store.db();
            bonsai::output::servicenow_em::maybe_start(
                &snow_cfg,
                db_for_snow,
                creds_for_snow,
                std::path::PathBuf::from("runtime"),
                shutdown_rx,
            );
        }

        if run_core && cfg.retention.enabled {
            let store_for_retention = if let Store::Core(s) = store {
                std::sync::Arc::clone(s)
            } else {
                unreachable!()
            };
            let max_age_h = cfg.retention.max_age_hours;
            let max_count = cfg.retention.max_state_change_events;
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(3600));
                loop {
                    interval.tick().await;
                    let cutoff =
                        time::OffsetDateTime::now_utc() - time::Duration::hours(max_age_h as i64);
                    if let Err(error) =
                        retention::prune_events(std::sync::Arc::clone(&store_for_retention), cutoff)
                            .await
                    {
                        warn!(%error, "retention age-prune failed");
                    }
                    if let Err(error) = retention::prune_events_by_count(
                        std::sync::Arc::clone(&store_for_retention),
                        max_count,
                    )
                    .await
                    {
                        warn!(%error, "retention count-prune failed");
                    }
                }
            });
        }
    }

    info!(phase = "ready", elapsed_ms = startup_start.elapsed().as_millis() as u64, "startup");

    tokio::signal::ctrl_c().await?;
    info!("Ctrl+C received - shutting down");
    let _ = shutdown_tx.send(true);
    if let Some(subscriber_manager) = subscriber_manager {
        let _ = subscriber_manager.await;
    }

    if let Some(ref store) = store {
        graph::log_graph_summary(store.db().as_ref());
    }
    info!("bonsai stopped");
    Ok(())
}

fn server_tls_config(tls: &config::RuntimeTlsConfig) -> Result<ServerTlsConfig> {
    let cert_path = required_tls_path(tls.cert.as_deref(), "runtime.tls.cert")?;
    let key_path = required_tls_path(tls.key.as_deref(), "runtime.tls.key")?;
    let ca_path = required_tls_path(tls.ca_cert.as_deref(), "runtime.tls.ca_cert")?;
    let cert = fs::read(cert_path)
        .with_context(|| format!("failed to read runtime.tls.cert '{cert_path}'"))?;
    let key = fs::read(key_path)
        .with_context(|| format!("failed to read runtime.tls.key '{key_path}'"))?;
    let ca = fs::read(ca_path)
        .with_context(|| format!("failed to read runtime.tls.ca_cert '{ca_path}'"))?;

    Ok(ServerTlsConfig::new()
        .identity(Identity::from_pem(cert, key))
        .client_ca_root(Certificate::from_pem(ca)))
}

fn required_tls_path<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("{field} is required when runtime.tls.enabled = true"))
}

enum DeviceCliCommand {
    Help,
    List,
    Add(Box<DeviceCliAdd>),
    Remove { address: String },
    SetEnabled { address: String, enabled: bool },
    Restart { address: String },
}

enum AuditCliCommand {
    Help,
    Export {
        since: String,
        until: String,
        output: Option<String>,
    },
}

enum CatalogueCliCommand {
    Help,
    List,
    Install { url: String, name: Option<String> },
    Uninstall { name: String },
}

struct DeviceCliAdd {
    address: String,
    hostname: Option<String>,
    vendor: Option<String>,
    role: Option<String>,
    site: Option<String>,
    credential_alias: Option<String>,
    username_env: Option<String>,
    password_env: Option<String>,
    tls_domain: Option<String>,
    ca_cert: Option<String>,
    enabled: bool,
}

impl DeviceCliCommand {
    fn parse() -> Result<Option<Self>> {
        let mut args = std::env::args().skip(1).collect::<Vec<_>>();
        if args.first().map(String::as_str) != Some("device") {
            return Ok(None);
        }
        args.remove(0);
        let Some(action) = args.first().cloned() else {
            return Ok(Some(Self::Help));
        };
        args.remove(0);

        match action.as_str() {
            "list" => Ok(Some(Self::List)),
            "add" => Ok(Some(Self::Add(Box::new(DeviceCliAdd::parse(args)?)))),
            "remove" => Ok(Some(Self::Remove {
                address: parse_address_arg(args)?,
            })),
            "stop" => Ok(Some(Self::SetEnabled {
                address: parse_address_arg(args)?,
                enabled: false,
            })),
            "start" => Ok(Some(Self::SetEnabled {
                address: parse_address_arg(args)?,
                enabled: true,
            })),
            "restart" => Ok(Some(Self::Restart {
                address: parse_address_arg(args)?,
            })),
            "help" | "--help" | "-h" => Ok(Some(Self::Help)),
            other => anyhow::bail!("unknown device command '{other}'"),
        }
    }
}

impl AuditCliCommand {
    fn parse() -> Result<Option<Self>> {
        let mut args = std::env::args().skip(1).collect::<Vec<_>>();
        if args.first().map(String::as_str) != Some("audit") {
            return Ok(None);
        }
        args.remove(0);
        let Some(action) = args.first().cloned() else {
            return Ok(Some(Self::Help));
        };
        args.remove(0);

        match action.as_str() {
            "export" => {
                let mut since = None;
                let mut until = None;
                let mut output = None;
                let mut iter = args.into_iter();
                while let Some(arg) = iter.next() {
                    match arg.as_str() {
                        "--since" => since = Some(require_flag_value("--since", iter.next())?),
                        "--until" => until = Some(require_flag_value("--until", iter.next())?),
                        "--output" => output = Some(require_flag_value("--output", iter.next())?),
                        "--help" | "-h" => return Ok(Some(Self::Help)),
                        other => anyhow::bail!("unknown audit export argument '{other}'"),
                    }
                }
                let since =
                    since.ok_or_else(|| anyhow::anyhow!("audit export requires --since"))?;
                let until =
                    until.ok_or_else(|| anyhow::anyhow!("audit export requires --until"))?;
                Ok(Some(Self::Export {
                    since,
                    until,
                    output,
                }))
            }
            "help" | "--help" | "-h" => Ok(Some(Self::Help)),
            other => anyhow::bail!("unknown audit command '{other}'"),
        }
    }
}

impl CatalogueCliCommand {
    fn parse() -> Result<Option<Self>> {
        let mut args = std::env::args().skip(1).collect::<Vec<_>>();
        if args.first().map(String::as_str) != Some("catalogue") {
            return Ok(None);
        }
        args.remove(0);
        let Some(action) = args.first().cloned() else {
            return Ok(Some(Self::Help));
        };
        args.remove(0);

        match action.as_str() {
            "list" => Ok(Some(Self::List)),
            "install" => {
                let mut url = None;
                let mut name = None;
                let mut iter = args.into_iter();
                while let Some(arg) = iter.next() {
                    match arg.as_str() {
                        "--name" => name = Some(require_flag_value("--name", iter.next())?),
                        "--help" | "-h" => return Ok(Some(Self::Help)),
                        other if url.is_none() && !other.starts_with("--") => {
                            url = Some(other.to_string());
                        }
                        other => anyhow::bail!("unknown catalogue install argument '{other}'"),
                    }
                }
                let url = url.ok_or_else(|| anyhow::anyhow!("catalogue install requires a URL"))?;
                Ok(Some(Self::Install { url, name }))
            }
            "uninstall" | "remove" => {
                let name = args
                    .into_iter()
                    .find(|a| !a.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("catalogue uninstall requires a plugin name"))?;
                Ok(Some(Self::Uninstall { name }))
            }
            "help" | "--help" | "-h" => Ok(Some(Self::Help)),
            other => anyhow::bail!("unknown catalogue command '{other}'"),
        }
    }
}

impl DeviceCliAdd {
    fn parse(args: Vec<String>) -> Result<Self> {
        let mut add = Self {
            address: String::new(),
            hostname: None,
            vendor: None,
            role: None,
            site: None,
            credential_alias: None,
            username_env: None,
            password_env: None,
            tls_domain: None,
            ca_cert: None,
            enabled: true,
        };

        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--address" => add.address = require_flag_value("--address", iter.next())?,
                "--hostname" => add.hostname = Some(require_flag_value("--hostname", iter.next())?),
                "--vendor" => add.vendor = Some(require_flag_value("--vendor", iter.next())?),
                "--role" => add.role = Some(require_flag_value("--role", iter.next())?),
                "--site" => add.site = Some(require_flag_value("--site", iter.next())?),
                "--credential-alias" => {
                    add.credential_alias =
                        Some(require_flag_value("--credential-alias", iter.next())?)
                }
                "--username-env" => {
                    add.username_env = Some(require_flag_value("--username-env", iter.next())?)
                }
                "--password-env" => {
                    add.password_env = Some(require_flag_value("--password-env", iter.next())?)
                }
                "--tls-domain" => {
                    add.tls_domain = Some(require_flag_value("--tls-domain", iter.next())?)
                }
                "--ca-cert" => add.ca_cert = Some(require_flag_value("--ca-cert", iter.next())?),
                "--disabled" => add.enabled = false,
                "--enabled" => add.enabled = true,
                other if add.address.is_empty() && !other.starts_with("--") => {
                    add.address = other.to_string();
                }
                other => anyhow::bail!("unknown device add argument '{other}'"),
            }
        }

        if add.address.trim().is_empty() {
            anyhow::bail!("device add requires --address <host:port>");
        }
        Ok(add)
    }
}

async fn run_device_cli(command: DeviceCliCommand) -> Result<()> {
    if matches!(command, DeviceCliCommand::Help) {
        print_device_cli_usage();
        return Ok(());
    }

    let config_path = config_path();
    let cfg = config::load(&config_path).await?;
    match run_device_cli_api(&command, &cfg.api_addr).await {
        Ok(()) => return Ok(()),
        Err(error) => {
            eprintln!(
                "warning: gRPC device API unavailable ({error:#}); falling back to local registry file"
            );
        }
    }
    run_device_cli_local(command, cfg).await
}

async fn run_device_cli_api(command: &DeviceCliCommand, api_addr: &str) -> Result<()> {
    let endpoint = device_cli_endpoint(api_addr);
    let channel = tonic::transport::Channel::from_shared(endpoint.clone())
        .with_context(|| format!("invalid device API endpoint '{endpoint}'"))?
        .timeout(Duration::from_secs(5))
        .connect()
        .await
        .with_context(|| format!("failed to connect to device API at {endpoint}"))?;
    let mut client = BonsaiGraphClient::new(channel);

    match command {
        DeviceCliCommand::Help => print_device_cli_usage(),
        DeviceCliCommand::List => {
            let response = client
                .list_managed_devices(ListManagedDevicesRequest {})
                .await?
                .into_inner();
            print_managed_devices(response.devices);
        }
        DeviceCliCommand::Add(add) => {
            let response = client
                .add_device(AddDeviceRequest {
                    device: Some(managed_device_from_cli_add(add)),
                })
                .await?
                .into_inner();
            ensure_device_cli_success(response.success, response.error)?;
            let address = response
                .device
                .map(|device| device.address)
                .unwrap_or_else(|| add.address.clone());
            println!("added {address}");
        }
        DeviceCliCommand::Remove { address } => {
            let response = client
                .remove_device(RemoveDeviceRequest {
                    address: address.clone(),
                })
                .await?
                .into_inner();
            ensure_device_cli_success(response.success, response.error)?;
            println!("removed {address}");
        }
        DeviceCliCommand::SetEnabled { address, enabled } => {
            let mut device = find_managed_device(&mut client, address).await?;
            device.enabled = Some(*enabled);
            let response = client
                .update_device(UpdateDeviceRequest {
                    device: Some(device),
                })
                .await?
                .into_inner();
            ensure_device_cli_success(response.success, response.error)?;
            println!(
                "{} {address}",
                if *enabled {
                    "started/enabled"
                } else {
                    "stopped/disabled"
                }
            );
        }
        DeviceCliCommand::Restart { address } => {
            let mut device = find_managed_device(&mut client, address).await?;
            device.enabled = Some(true);
            let response = client
                .update_device(UpdateDeviceRequest {
                    device: Some(device),
                })
                .await?
                .into_inner();
            ensure_device_cli_success(response.success, response.error)?;
            println!("restart requested for {address}");
        }
    }

    Ok(())
}

async fn find_managed_device(
    client: &mut BonsaiGraphClient<tonic::transport::Channel>,
    address: &str,
) -> Result<ManagedDevice> {
    client
        .list_managed_devices(ListManagedDevicesRequest {})
        .await?
        .into_inner()
        .devices
        .into_iter()
        .find(|device| device.address == address)
        .ok_or_else(|| anyhow::anyhow!("device {address} not found"))
}

fn device_cli_endpoint(api_addr: &str) -> String {
    let trimmed = api_addr.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    }
}

fn ensure_device_cli_success(success: bool, error: String) -> Result<()> {
    if success {
        Ok(())
    } else {
        anyhow::bail!("{}", error.trim())
    }
}

fn managed_device_from_cli_add(add: &DeviceCliAdd) -> ManagedDevice {
    ManagedDevice {
        address: add.address.clone(),
        enabled: Some(add.enabled),
        tls_domain: add.tls_domain.clone().unwrap_or_default(),
        ca_cert: add.ca_cert.clone().unwrap_or_default(),
        vendor: add.vendor.clone().unwrap_or_default(),
        credential_alias: add.credential_alias.clone().unwrap_or_default(),
        username_env: add.username_env.clone().unwrap_or_default(),
        password_env: add.password_env.clone().unwrap_or_default(),
        hostname: add.hostname.clone().unwrap_or_default(),
        role: add.role.clone().unwrap_or_default(),
        site: add.site.clone().unwrap_or_default(),
        selected_paths: Vec::new(),
        collector_id: String::new(),
    }
}

fn managed_device_from_target(target: TargetConfig) -> ManagedDevice {
    ManagedDevice {
        address: target.address,
        enabled: Some(target.enabled),
        tls_domain: target.tls_domain.unwrap_or_default(),
        ca_cert: target.ca_cert.unwrap_or_default(),
        vendor: target.vendor.unwrap_or_default(),
        credential_alias: target.credential_alias.unwrap_or_default(),
        username_env: target.username_env.unwrap_or_default(),
        password_env: target.password_env.unwrap_or_default(),
        hostname: target.hostname.unwrap_or_default(),
        role: target.role.unwrap_or_default(),
        site: target.site.unwrap_or_default(),
        selected_paths: Vec::new(),
        collector_id: target.collector_id.unwrap_or_default(),
    }
}

fn print_managed_devices(devices: Vec<ManagedDevice>) {
    println!(
        "{:<24} {:<8} {:<16} {:<12} {:<12} credential",
        "address", "state", "hostname", "vendor", "site"
    );
    for device in devices {
        println!(
            "{:<24} {:<8} {:<16} {:<12} {:<12} {}",
            device.address,
            if device.enabled.unwrap_or(true) {
                "enabled"
            } else {
                "stopped"
            },
            device.hostname,
            device.vendor,
            device.site,
            device.credential_alias,
        );
    }
}

async fn run_device_cli_local(command: DeviceCliCommand, cfg: config::Config) -> Result<()> {
    let registry = ApiRegistry::open(REGISTRY_PATH, cfg.target.clone())?;

    match command {
        DeviceCliCommand::Help => print_device_cli_usage(),
        DeviceCliCommand::List => {
            let devices = registry.list_active()?;
            print_managed_devices(
                devices
                    .into_iter()
                    .map(managed_device_from_target)
                    .collect(),
            );
        }
        DeviceCliCommand::Add(add) => {
            let device = registry.add_device_with_audit(
                TargetConfig {
                    address: add.address,
                    enabled: add.enabled,
                    tls_domain: add.tls_domain,
                    ca_cert: add.ca_cert,
                    vendor: add.vendor,
                    credential_alias: add.credential_alias,
                    username_env: add.username_env,
                    password_env: add.password_env,
                    username: None,
                    password: None,
                    hostname: add.hostname,
                    role: add.role,
                    site: add.site,
                    collector_id: None,
                    selected_paths: Vec::new(),
                    created_at_ns: 0,
                    updated_at_ns: 0,
                    created_by: String::new(),
                    updated_by: String::new(),
                    last_operator_action: String::new(),
                },
                "cli",
                "cli_add_device",
            )?;
            println!("added {}", device.address);
        }
        DeviceCliCommand::Remove { address } => match registry.remove_device(&address)? {
            Some(_) => println!("removed {address}"),
            None => println!("device {address} not found"),
        },
        DeviceCliCommand::SetEnabled { address, enabled } => {
            let mut device = registry
                .get_device(&address)?
                .ok_or_else(|| anyhow::anyhow!("device {address} not found"))?;
            device.enabled = enabled;
            registry.update_device_with_audit(device, "cli", "cli_set_enabled_device")?;
            println!(
                "{} {address}",
                if enabled {
                    "started/enabled"
                } else {
                    "stopped/disabled"
                }
            );
        }
        DeviceCliCommand::Restart { address } => {
            let mut device = registry
                .get_device(&address)?
                .ok_or_else(|| anyhow::anyhow!("device {address} not found"))?;
            device.enabled = true;
            registry.update_device_with_audit(device, "cli", "cli_restart_device")?;
            println!("restart requested for {address}");
        }
    }
    Ok(())
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

fn parse_address_arg(args: Vec<String>) -> Result<String> {
    let mut iter = args.into_iter();
    let Some(arg) = iter.next() else {
        anyhow::bail!("device command requires an address");
    };
    match arg.as_str() {
        "--address" => require_flag_value("--address", iter.next()),
        other if !other.starts_with("--") => Ok(other.to_string()),
        other => anyhow::bail!("unknown address argument '{other}'"),
    }
}

fn require_flag_value(flag: &str, value: Option<String>) -> Result<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}

fn print_device_cli_usage() {
    println!(
        "usage:\n  bonsai device list\n  bonsai device add --address <host:port> [--hostname name] [--vendor label] [--role role] [--site site] [--credential-alias alias]\n  bonsai device remove <host:port>\n  bonsai device stop <host:port>\n  bonsai device start <host:port>\n  bonsai device restart <host:port>"
    );
}

async fn run_audit_cli(command: AuditCliCommand) -> Result<()> {
    if matches!(command, AuditCliCommand::Help) {
        print_audit_cli_usage();
        return Ok(());
    }

    let config_path = config_path();
    let cfg = config::load(&config_path).await?;
    let root = std::path::Path::new(&cfg.credentials.path);

    match command {
        AuditCliCommand::Help => {
            print_audit_cli_usage();
            Ok(())
        }
        AuditCliCommand::Export {
            since,
            until,
            output,
        } => {
            let output_path = output
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("bonsai-audit-export.tar"));
            let result = audit::export_tarball(root, &since, &until, &output_path)?;
            println!(
                "exported {} audit events to {}",
                result.entry_count,
                result.output_path.display()
            );
            Ok(())
        }
    }
}

fn print_audit_cli_usage() {
    println!(
        "usage:\n  bonsai audit export --since <RFC3339> --until <RFC3339> [--output path.tar]"
    );
}

fn catalogue_plugins_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("config/path_profiles/plugins")
}

async fn run_catalogue_cli(command: CatalogueCliCommand) -> Result<()> {
    match command {
        CatalogueCliCommand::Help => {
            print_catalogue_cli_usage();
            Ok(())
        }
        CatalogueCliCommand::List => {
            let catalogue_dir = "config/path_profiles";
            let state = catalogue::load_catalogue(std::path::Path::new(catalogue_dir));

            if !state.load_errors.is_empty() {
                for e in &state.load_errors {
                    eprintln!("warning: {e}");
                }
            }

            println!("Built-in profiles ({}):", state.profiles.len());
            for p in &state.profiles {
                let env = if p.environment.is_empty() {
                    "all".to_string()
                } else {
                    p.environment.join(", ")
                };
                let roles = if p.roles.is_empty() {
                    "all".to_string()
                } else {
                    p.roles.join(", ")
                };
                println!(
                    "  {:<30} env={:<20} roles={}",
                    p.name, env, roles
                );
            }

            if state.plugins.is_empty() {
                println!("\nNo plugins installed.");
            } else {
                println!("\nInstalled plugins ({}):", state.plugins.len());
                for plugin in &state.plugins {
                    let m = &plugin.manifest;
                    println!(
                        "  {:<24} v{:<10} by {}",
                        m.name, m.version, m.author
                    );
                    for p in &plugin.profiles {
                        println!("    profile: {}", p.name);
                    }
                    for conflict in &plugin.conflicts {
                        println!("    conflict: {conflict}");
                    }
                }
            }
            Ok(())
        }
        CatalogueCliCommand::Install { url, name } => {
            // Derive plugin name from --name flag or URL basename
            let plugin_name = name.unwrap_or_else(|| {
                url.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("plugin")
                    .trim_end_matches(".git")
                    .to_string()
            });

            if plugin_name.is_empty() || plugin_name.contains(['/', '\\', '.']) {
                anyhow::bail!(
                    "plugin name '{plugin_name}' is invalid. Use --name to override."
                );
            }

            let plugins_dir = catalogue_plugins_dir();
            let dest = plugins_dir.join(&plugin_name);

            if dest.exists() {
                anyhow::bail!(
                    "plugin directory '{}' already exists. Uninstall first with: bonsai catalogue uninstall {plugin_name}",
                    dest.display()
                );
            }

            std::fs::create_dir_all(&plugins_dir)
                .with_context(|| format!("cannot create plugins dir '{}'", plugins_dir.display()))?;

            println!("cloning {url} → {}", dest.display());
            let status = std::process::Command::new("git")
                .args(["clone", "--depth=1", "--quiet", &url, &dest.to_string_lossy()])
                .status()
                .with_context(|| "git not found — install git and retry")?;

            if !status.success() {
                anyhow::bail!("git clone failed (exit code {:?})", status.code());
            }

            let manifest_path = dest.join("MANIFEST.yaml");
            if !manifest_path.exists() {
                // Clean up
                let _ = std::fs::remove_dir_all(&dest);
                anyhow::bail!(
                    "no MANIFEST.yaml found in the cloned repository. \
                     Bonsai plugins must have a MANIFEST.yaml at the repo root."
                );
            }

            let manifest_bytes = std::fs::read(&manifest_path)
                .with_context(|| "cannot read MANIFEST.yaml")?;
            let manifest: catalogue::PluginManifest = serde_yaml::from_slice(&manifest_bytes)
                .with_context(|| "MANIFEST.yaml is not valid YAML or missing required fields")?;

            // SHA256 fingerprint of the manifest for operator audit records
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            hasher.update(&manifest_bytes);
            let fingerprint = hex::encode(hasher.finalize());

            println!("installed plugin: {}", manifest.name);
            println!("  version : {}", manifest.version);
            if !manifest.author.is_empty() {
                println!("  author  : {}", manifest.author);
            }
            println!("  profiles: {}", manifest.profiles.len());
            for p in &manifest.profiles {
                println!("    {p}");
            }
            println!("  manifest SHA256: {fingerprint}");
            println!(
                "\nPlugin will be active on next bonsai start (or server reload).\n\
                 To list installed plugins: bonsai catalogue list"
            );
            Ok(())
        }
        CatalogueCliCommand::Uninstall { name } => {
            let plugins_dir = catalogue_plugins_dir();
            let target = plugins_dir.join(&name);
            if !target.exists() {
                anyhow::bail!("plugin '{name}' not found in {}", plugins_dir.display());
            }
            // Require MANIFEST.yaml to avoid accidentally removing arbitrary directories
            if !target.join("MANIFEST.yaml").exists() {
                anyhow::bail!(
                    "'{name}' does not look like a bonsai plugin (no MANIFEST.yaml). \
                     Remove manually if intended."
                );
            }
            std::fs::remove_dir_all(&target)
                .with_context(|| format!("cannot remove '{}'", target.display()))?;
            println!("uninstalled plugin: {name}");
            Ok(())
        }
    }
}

fn print_catalogue_cli_usage() {
    println!(
        "usage:\n\
         \x20 bonsai catalogue list\n\
         \x20 bonsai catalogue install <git-url> [--name <plugin-name>]\n\
         \x20 bonsai catalogue uninstall <plugin-name>\n\
         \n\
         Plugins are cloned into config/path_profiles/plugins/<name>/.\n\
         Each plugin must have a MANIFEST.yaml at its root.\n\
         \n\
         Example:\n\
         \x20 bonsai catalogue install https://github.com/example/bonsai-plugin-nokia-sr.git\n\
         \x20 bonsai catalogue list\n\
         \x20 bonsai catalogue uninstall bonsai-plugin-nokia-sr"
    );
}

struct SelfTestCliCommand;

impl SelfTestCliCommand {
    fn parse() -> bool {
        std::env::args().nth(1).as_deref() == Some("self-test")
    }
}

async fn run_self_test() -> Result<()> {
    let mut passed: u32 = 0;
    let mut failed: u32 = 0;

    macro_rules! check {
        ($label:expr, $body:block) => {{
            let result: Result<()> = async { $body; Ok(()) }.await;
            match result {
                Ok(()) => { println!("  [✓] {}", $label); passed += 1; }
                Err(e) => { println!("  [✗] {} — {e}", $label); failed += 1; }
            }
        }};
    }

    println!("bonsai self-test");
    println!("================");

    check!("crypto provider (rustls/ring)", {
        rustls::crypto::ring::default_provider()
            .install_default()
            .or_else(|_| {
                // Already installed by a prior call — that's fine.
                Ok::<(), rustls::Error>(())
            })?;
    });

    check!("tokio runtime", {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    });

    check!("config parser (TOML round-trip)", {
        let _cfg: bonsai::config::Config = toml::from_str(
            "graph_path = \"/tmp/bonsai-selftest-unused.db\"\n\
             [runtime]\nmode = \"all\"\n\
             [event_bus]\ncapacity = 512\n",
        )?;
    });

    check!("LadybugDB linkage (open temp database)", {
        let db_path = std::env::temp_dir()
            .join(format!("bonsai-self-test-{}", std::process::id()));
        // Remove any leftover from a previous run so Kuzu creates a fresh DB.
        let _ = std::fs::remove_dir_all(&db_path);
        let path = db_path.to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF8 temp path"))?
            .to_owned();
        let result = bonsai::graph::GraphStore::open(&path, 64 * 1024 * 1024);
        let _ = std::fs::remove_dir_all(&db_path);
        result.map(|_| ())?;
    });

    println!("================");
    println!("{passed} passed, {failed} failed");

    if failed > 0 {
        anyhow::bail!("{failed} check(s) failed");
    }
    Ok(())
}

async fn spawn_subscriber(
    target: TargetConfig,
    credentials: &std::sync::Arc<CredentialVault>,
    bus: &std::sync::Arc<InProcessBus>,
    subscription_plan_tx: Option<&tokio::sync::mpsc::Sender<SubscriptionPlan>>,
    subscribers: &mut SubscriberHandleMap,
) -> Result<()> {
    let address = target.address.clone();
    if !target.enabled {
        info!(address = %address, "subscriber start skipped because target is disabled");
        return Ok(());
    }
    if subscribers.contains_key(&address) {
        info!(address = %address, "subscriber already running");
        return Ok(());
    }

    let ca_cert_pem = load_ca_cert_pem(&target).await?;
    let resolved_credentials = resolve_target_credentials(&target, credentials)?;
    let (username, password) = match resolved_credentials {
        Some(credentials) => (Some(credentials.username), Some(credentials.password)),
        None => (None, None),
    };
    let subscriber = subscriber::GnmiSubscriber::new(
        target.address.clone(),
        username,
        password,
        target.vendor.clone(),
        target.hostname.clone(),
        target.role.clone(),
        target.site.clone(),
        target.tls_domain.clone().unwrap_or_default(),
        ca_cert_pem,
        std::sync::Arc::clone(bus),
        subscription_plan_tx.cloned(),
        target.selected_paths.clone(),
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(async move { subscriber.run_forever(shutdown_rx).await });
    subscribers.insert(address.clone(), (shutdown_tx, handle));
    info!(address = %address, "subscriber started");
    Ok(())
}

async fn restart_subscriber(
    target: TargetConfig,
    credentials: &std::sync::Arc<CredentialVault>,
    bus: &std::sync::Arc<InProcessBus>,
    subscription_plan_tx: Option<&tokio::sync::mpsc::Sender<SubscriptionPlan>>,
    subscribers: &mut SubscriberHandleMap,
) -> Result<()> {
    let address = target.address.clone();
    stop_subscriber(&address, subscribers).await;
    spawn_subscriber(target, credentials, bus, subscription_plan_tx, subscribers).await
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

fn resolve_target_credentials(
    target: &TargetConfig,
    credentials: &CredentialVault,
) -> Result<Option<ResolvedCredential>> {
    if let Some(alias) = target.credential_alias.as_deref() {
        return credentials
            .resolve(alias, ResolvePurpose::Subscribe)
            .map(Some);
    }

    Ok(
        match (target.resolved_username(), target.resolved_password()) {
            (Some(username), Some(password)) => Some(ResolvedCredential { username, password }),
            _ => None,
        },
    )
}
