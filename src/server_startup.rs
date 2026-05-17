use std::collections::HashMap;
use std::fs;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use tracing::{info, warn};

use bonsai::{
    api::{BonsaiGraphServer, CollectorService, CoreService},
    archive, catalogue, config,
    config::TargetConfig,
    config::{resolve_buffer_pool_collector, resolve_buffer_pool_core},
    credentials::{CredentialVault, ResolvePurpose, ResolvedCredential},
    event_bus::InProcessBus,
    graph, ingest,
    registry::{ApiRegistry, DeviceRegistry, RegistryChange},
    retention,
    store::BonsaiStore,
    subscriber::{self, SubscriberHandleMap, stop_all_subscribers, stop_subscriber},
    subscription_status::{self, SubscriptionPlan},
};
use metrics_exporter_prometheus::PrometheusBuilder;
use tonic::codec::CompressionEncoding;
use tonic::transport::{Certificate, Identity, ServerTlsConfig};

use super::{GRAPH_PATH_DEFAULT, REGISTRY_PATH};

fn config_path() -> String {
    super::config_path()
}


pub(super) async fn run_server() -> anyhow::Result<()> {
    // ── Logging setup (T2-1/T2-2) ─────────────────────────────────────────────
    // Load config early (just for logging config) so we can set up rotation
    // before the first log line. Re-load below for the full config.
    let early_cfg_path = config_path();
    let log_cfg = match config::load(&early_cfg_path).await {
        Ok(c) => c.logging,
        Err(_) => config::LoggingConfig::default(),
    };

    // Build env filter: base level from config, overridden by RUST_LOG, then per-module targets.
    let base_directive = format!("bonsai={}", log_cfg.level);
    let mut filter =
        tracing_subscriber::EnvFilter::from_default_env().add_directive(base_directive.parse()?);
    for (module, level) in &log_cfg.targets {
        if let Ok(dir) = format!("{module}={level}").parse() {
            filter = filter.add_directive(dir);
        }
    }

    if log_cfg.file_path.is_empty() {
        // Stderr only (foreground / development mode).
        tracing_subscriber::fmt().with_env_filter(filter).init();
    } else {
        use tracing_appender::rolling::{RollingFileAppender, Rotation};
        use tracing_subscriber::prelude::*;

        let rotation = match log_cfg.rotation.to_lowercase().as_str() {
            "hourly" => Rotation::HOURLY,
            "never" => Rotation::NEVER,
            _ => Rotation::DAILY,
        };

        let log_path = std::path::Path::new(&log_cfg.file_path);
        let log_dir = log_path.parent().unwrap_or(std::path::Path::new("."));
        let log_file = log_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("bonsai.log");

        // T2-4: Pre-flight disk space check.
        if log_cfg.min_free_bytes > 0 {
            preflight_disk_check(log_dir, log_cfg.min_free_bytes)?
        }

        let file_appender = RollingFileAppender::builder()
            .rotation(rotation)
            .filename_prefix(log_file)
            .max_log_files(log_cfg.retention_days as usize)
            .build(log_dir)
            .context("failed to create rolling file appender")?;

        let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

        // T2-3: Log volume layer — count every event.
        let log_volume_layer = LogVolumeLayer;

        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(non_blocking)
                    .with_ansi(false),
            )
            .with(log_volume_layer)
            .init();

        // Keep _guard alive for the lifetime of the process by leaking it.
        // The guard flushes the non-blocking writer on drop; leaking is intentional here
        // because the process lifetime equals the guard lifetime.
        std::mem::forget(_guard);
    }

    info!(
        protocol_version = bonsai::api::PROTOCOL_VERSION,
        "bonsai starting"
    );

    let startup_start = Instant::now();

    let t = Instant::now();
    let config_path = config_path();
    let cfg = config::load(&config_path).await?;
    info!(
        phase = "config_load",
        elapsed_ms = t.elapsed().as_millis() as u64,
        "startup"
    );

    // T4-1: probe runtime environment → ResourceProfile with tuning defaults.
    let probe = bonsai::resource_profile::probe(
        std::path::Path::new(&cfg.archive.path),
        std::path::Path::new(if cfg.logging.file_path.is_empty() {
            "."
        } else {
            &cfg.logging.file_path
        }),
    );
    let governor_profile = if let Some(override_name) = cfg.runtime.resource_profile.as_deref() {
        let profile = bonsai::resource_profile::ResourceProfile::from_name(override_name)
            .ok_or_else(|| anyhow!("invalid runtime.resource_profile '{override_name}'"))?;
        info!(
            requested = override_name,
            selected = profile.as_str(),
            "resource profile override applied"
        );
        profile
    } else {
        probe.profile
    };
    let governor_defaults = governor_profile.defaults();

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
    let queue_depth = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let debouncer = std::sync::Arc::new(bonsai::ingest::TelemetryDebouncer::new(
        cfg.ingest.counter_debounce_secs,
        std::sync::Arc::clone(&queue_depth),
        cfg.ingest.backpressure.clone(),
        cfg.event_bus.capacity,
        cfg.ingest.debounce_memory_bytes,
    ));

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
        fn write_lock(&self) -> std::sync::Arc<std::sync::Mutex<()>> {
            match self {
                Store::Core(s) => s.write_lock(),
                Store::Collector(s) => s.write_lock(),
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
        info!(
            phase = "graph_open",
            elapsed_ms = t.elapsed().as_millis() as u64,
            "startup"
        );
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
        info!(
            phase = "graph_open",
            elapsed_ms = t.elapsed().as_millis() as u64,
            "startup"
        );
        Some(Store::Collector(std::sync::Arc::new(s)))
    } else {
        None
    };

    // Shutdown channel and governor are created before receivers so all subsystems
    // share the same governor handle for pressure-based control (T6-1 / C4-N2).
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let shared_governor: Option<bonsai::resource_governor::GovernorHandle> = if run_core {
        Some(bonsai::resource_governor::start(
            governor_profile,
            governor_defaults,
            shutdown_rx.clone(),
        ))
    } else {
        None
    };

    // D2-10 T5: Register the debounce-cache shrink callback on the governor so that
    // govern_memory_soft (50%) and govern_memory_hard (25%) actually reduce RSS by
    // evicting entries from the three ShardedLruCaches, not just emitting a metric.
    if let Some(ref gov) = shared_governor {
        let debouncer_for_gov = std::sync::Arc::clone(&debouncer);
        gov.register_memory_pressure_callback(move |pct| {
            debouncer_for_gov.shrink_debounce_caches(pct);
        });
    }

    let coordinator = if let Some(Store::Core(ref s)) = store {
        let coordinator_cfg = bonsai::write_coordinator::WriteCoordinatorConfig {
            governor: shared_governor.clone(),
            ..Default::default()
        };
        Some(std::sync::Arc::new(
            bonsai::write_coordinator::WriteCoordinator::new(
                std::sync::Arc::clone(s),
                coordinator_cfg,
                std::sync::Arc::clone(&queue_depth),
            ),
        ))
    } else {
        None
    };

    if let Some(ref coordinator) = coordinator {
        let coordinator = std::sync::Arc::clone(coordinator);
        let (sub, mut rx) = bonsai::event_bus::MpscSubscriber::new(
            "graph_writer",
            cfg.event_bus.capacity.max(4096),
            bonsai::event_bus::OverflowPolicy::BlockProducer,
        );
        bus.add_subscriber(sub).await;

        tokio::spawn(async move {
            while let Some(arc_update) = rx.recv().await {
                if let Err(error) = coordinator
                    .submit(bonsai::write_coordinator::WriteRequest::Telemetry(
                        std::sync::Arc::unwrap_or_clone(arc_update),
                    ))
                    .await
                {
                    warn!(%error, "failed to submit telemetry to write coordinator");
                }
            }
            info!("event bus closed - graph writer stopping");
        });
    }

    if cfg.signals.syslog.enabled && run_collector {
        let syslog_cfg = cfg.signals.syslog.clone();
        let syslog_pattern_dir = cfg.layered_ingestion.syslog_patterns_path.clone();
        let syslog_targets = cfg.target.clone();
        let syslog_bus = std::sync::Arc::clone(&bus);
        let syslog_shutdown = shutdown_rx.clone();
        let syslog_governor = shared_governor.clone();
        tokio::spawn(async move {
            if let Err(error) = bonsai::signals::syslog::run_syslog_receiver(
                syslog_cfg,
                syslog_pattern_dir,
                syslog_targets,
                syslog_bus,
                syslog_shutdown,
                syslog_governor,
            )
            .await
            {
                warn!(%error, "syslog receiver stopped");
            }
        });
    } else if cfg.signals.syslog.enabled {
        info!(
            "syslog receiver enabled but runtime mode has no collector role; skipping syslog receiver"
        );
    }

    if cfg.signals.snmp.enabled && run_collector {
        let snmp_cfg = cfg.signals.snmp.clone();
        let snmp_targets = cfg.target.clone();
        let snmp_bus = std::sync::Arc::clone(&bus);
        let snmp_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            if let Err(error) = bonsai::signals::snmp::run_snmp_receiver(
                snmp_cfg,
                snmp_targets,
                snmp_bus,
                snmp_shutdown,
            )
            .await
            {
                warn!(%error, "snmp receiver stopped");
            }
        });
    } else if cfg.signals.snmp.enabled {
        info!(
            "snmp receiver enabled but runtime mode has no collector role; skipping snmp receiver"
        );
    }

    if cfg.streaming.bmp.enabled && run_collector {
        let bmp_cfg = cfg.streaming.bmp.clone();
        let bmp_targets = cfg.target.clone();
        let bmp_bus = std::sync::Arc::clone(&bus);
        let bmp_shutdown = shutdown_rx.clone();
        let bmp_governor = shared_governor.clone();
        tokio::spawn(async move {
            if let Err(error) = bonsai::streaming::bmp::run_bmp_receiver(
                bmp_cfg,
                bmp_targets,
                bmp_bus,
                bmp_shutdown,
                bmp_governor,
            )
            .await
            {
                warn!(%error, "BMP receiver stopped");
            }
        });
    } else if cfg.streaming.bmp.enabled {
        info!("BMP receiver enabled but runtime mode has no collector role; skipping BMP receiver");
    }

    if cfg.streaming.bgp_ls.enabled && run_collector {
        let bgp_ls_cfg = cfg.streaming.bgp_ls.clone();
        let bgp_ls_targets = cfg.target.clone();
        let bgp_ls_bus = std::sync::Arc::clone(&bus);
        let bgp_ls_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            if let Err(error) = bonsai::streaming::bgp_ls::run_bgp_ls_receiver(
                bgp_ls_cfg,
                bgp_ls_targets,
                bgp_ls_bus,
                bgp_ls_shutdown,
            )
            .await
            {
                warn!(%error, "BGP-LS receiver stopped");
            }
        });
    } else if cfg.streaming.bgp_ls.enabled {
        info!(
            "BGP-LS receiver enabled but runtime mode has no collector role; skipping BGP-LS receiver"
        );
    }

    if cfg.streaming.pcep.enabled {
        info!(
            "PCEP ingest is configured but intentionally deferred in CV2 Sprint 4; no runtime receiver will be started yet"
        );
    }

    if cfg.streaming.otlp.enabled && run_collector {
        let otlp_cfg = cfg.streaming.otlp.clone();
        let otlp_bus = std::sync::Arc::clone(&bus);
        let otlp_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            if let Err(error) =
                bonsai::streaming::otlp::run_otlp_receiver(otlp_cfg, otlp_bus, otlp_shutdown)
                    .await
            {
                warn!(%error, "OTLP receiver stopped");
            }
        });
    } else if cfg.streaming.otlp.enabled {
        info!("OTLP receiver enabled but runtime mode has no collector role; skipping");
    }

    if cfg.streaming.netflow.enabled && run_collector {
        let netflow_cfg = cfg.streaming.netflow.clone();
        let netflow_bus = std::sync::Arc::clone(&bus);
        let netflow_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            if let Err(error) = bonsai::streaming::netflow::run_netflow_receiver(
                netflow_cfg,
                netflow_bus,
                netflow_shutdown,
            )
            .await
            {
                warn!(%error, "Netflow receiver stopped");
            }
        });
    } else if cfg.streaming.netflow.enabled {
        info!("Netflow receiver enabled but runtime mode has no collector role; skipping");
    }

    if cfg.archive.enabled && (run_collector || run_core) {
        let archive_root = std::path::PathBuf::from(&cfg.archive.path);
        let flush_interval = Duration::from_secs(cfg.archive.flush_interval_seconds);
        let max_batch_rows = cfg.archive.max_batch_rows;
        let compression_level = cfg.archive.compression_level;
        let writer_policy = archive::WriterPolicy {
            max_idle_secs: cfg.archive.writer_max_idle_secs,
            max_file_age_secs: cfg.archive.max_file_age_seconds,
        };
        let bus_for_archive = std::sync::Arc::clone(&bus);
        let archive_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            if let Err(error) = archive::run_archiver(
                bus_for_archive,
                archive_root,
                flush_interval,
                max_batch_rows,
                compression_level,
                writer_policy,
                archive_shutdown,
            )
            .await
            {
                warn!(%error, "archive consumer stopped");
            }
        });
    } else if cfg.archive.enabled {
        info!(
            "archive enabled but runtime mode has no telemetry ingest role; skipping archive consumer"
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

    // CV7 T4-2: sidecar registry. Tracks Python (and future) sidecar processes
    // that bond to bonsai over gRPC. BONSAI_REQUIRE_SIDECAR=<comma-list> turns
    // missing required kinds into a /health degraded status (T4-6).
    let required_sidecar_kinds =
        bonsai::sidecar_registry::SidecarRegistry::parse_required_kinds(
            &std::env::var("BONSAI_REQUIRE_SIDECAR").unwrap_or_default(),
        );
    if !required_sidecar_kinds.is_empty() {
        info!(
            required = ?required_sidecar_kinds,
            "BONSAI_REQUIRE_SIDECAR set — /health will be degraded until these sidecars register"
        );
    }
    let sidecar_registry = std::sync::Arc::new(
        bonsai::sidecar_registry::SidecarRegistry::new(required_sidecar_kinds),
    );

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

    // Wire the graph event channel into the collector manager so that
    // collector connect/disconnect events appear on the SSE stream.
    if let (Some(Store::Core(core_store)), Some(manager)) = (&store, &collector_manager) {
        manager.set_event_sender(core_store.event_sender());
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

    if runtime_mode == bonsai::config::RuntimeMode::Core
        && let Some(subscription_plan_tx) = subscription_plan_tx.clone()
    {
        let registry_for_verifier = std::sync::Arc::clone(&registry);
        let mut verifier_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            match registry_for_verifier.list_active() {
                Ok(targets) => {
                    for target in targets {
                        seed_subscription_plan(target, &subscription_plan_tx).await;
                    }
                }
                Err(error) => warn!(%error, "failed to seed subscription verifier targets"),
            }

            let mut change_rx = registry_for_verifier.subscribe_changes();
            loop {
                tokio::select! {
                    _ = verifier_shutdown.changed() => break,
                    maybe_change = change_rx.recv() => {
                        let Some(change) = maybe_change else {
                            break;
                        };
                        match change {
                            RegistryChange::Added(target) | RegistryChange::Updated(target) => {
                                seed_subscription_plan(target, &subscription_plan_tx).await;
                            }
                            RegistryChange::Removed(_) => {}
                        }
                    }
                }
            }
        });
    }

    let subscriber_manager = if runtime_mode == bonsai::config::RuntimeMode::All {
        let registry = std::sync::Arc::clone(&registry);
        let credentials = std::sync::Arc::clone(&credentials);
        let bus = std::sync::Arc::clone(&bus);
        let debouncer_for_subs = std::sync::Arc::clone(&debouncer);
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
                            Some(std::sync::Arc::clone(&debouncer_for_subs)),
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
                                if let Err(error) = spawn_subscriber(target, &credentials, &bus, Some(std::sync::Arc::clone(&debouncer_for_subs)), subscription_plan_tx.as_ref(), &mut subscribers).await {
                                    warn!(%error, "failed to start subscriber for added device");
                                }
                            }
                            RegistryChange::Updated(target) => {
                                if target.enabled {
                                    if let Err(error) = restart_subscriber(target, &credentials, &bus, Some(std::sync::Arc::clone(&debouncer_for_subs)), subscription_plan_tx.as_ref(), &mut subscribers).await {
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

    // D1-T1 (DV1): HTTP task handle — tracked so an unexpected HTTP exit triggers shutdown.
    let mut http_task: Option<tokio::task::JoinHandle<()>> = None;

    if let Some(ref store) = store {
        let change_detection_runtime = if run_core {
            Some(bonsai::change_detection::ChangeDetectionRuntime::start(
                if let Store::Core(s) = store {
                    std::sync::Arc::clone(s)
                } else {
                    unreachable!()
                },
                std::sync::Arc::clone(&registry),
                std::sync::Arc::clone(&credentials),
                std::sync::Arc::clone(&bus),
                cfg.layered_ingestion.clone(),
            )?)
        } else {
            None
        };

        let api_addr = cfg
            .api_addr
            .parse()
            .with_context(|| format!("invalid api_addr '{}'", cfg.api_addr))?;

        let registry_for_api = std::sync::Arc::clone(&registry);
        let credentials_for_api = std::sync::Arc::clone(&credentials);
        let bus_for_api = std::sync::Arc::clone(&bus);
        let store_for_api = store.clone();
        let collector_manager_for_api = collector_manager.clone();
        let sidecar_registry_for_api = std::sync::Arc::clone(&sidecar_registry);

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
                        Some(std::sync::Arc::clone(&debouncer)),
                        collector_manager_for_api,
                        sidecar_registry_for_api,
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
                        Some(std::sync::Arc::clone(&debouncer)),
                        None,
                        sidecar_registry_for_api,
                    ))
                    .accept_compressed(CompressionEncoding::Zstd);
                    if let Err(error) = server.add_service(svc).serve(api_addr).await {
                        warn!(%error, "gRPC collector server error");
                    }
                }
            }
        });

        let mut startup_adapter_registry: Option<bonsai::output::traits::SharedAdapterRegistry> =
            None;
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
            let change_detection_for_http = change_detection_runtime
                .as_ref()
                .map(std::sync::Arc::clone)
                .expect("change detection runtime should exist in core mode");
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
            let adapter_registry_handle = std::sync::Arc::clone(&adapter_registry);
            startup_adapter_registry.replace(std::sync::Arc::clone(&adapter_registry));
            let trust_store = bonsai::remediation::trust::new_trust_store(
                std::path::Path::new(&runtime_dir),
                cfg.remediation.clone(),
            );
            let rollback_registry = bonsai::remediation::rollback::new_rollback_registry();
            let remediation_config = cfg.remediation.clone();
            let servicenow_config = cfg.integrations.servicenow.clone();

            // T4-1/T4-2/T4-3/T4-4: governor was started early (shared_governor) so all
            // receivers and the write coordinator already share it.
            let governor_for_http = shared_governor.clone();

            // D1-T1 (DV1): bind the listener on the main task so a port-conflict fails
            // startup with a clear error instead of silently panicking the spawned task
            // and leaving bonsai alive with a dead HTTP server.
            let http_listener = tokio::net::TcpListener::bind(http_addr)
                .await
                .with_context(|| format!("failed to bind HTTP port at {http_addr}"))?;
            info!(addr = %http_addr, "HTTP listener bound");

            http_task = Some(tokio::spawn(async move {
                if let Err(error) = axum::serve(
                    http_listener,
                    bonsai::http_server::router(
                        http_store,
                        registry_for_http,
                        credentials_for_http,
                        change_detection_for_http,
                        collector_manager_for_http,
                        catalogue,
                        catalogue_dir,
                        enricher_registry,
                        adapter_registry,
                        trust_store,
                        rollback_registry,
                        remediation_config,
                        servicenow_config,
                        runtime_dir,
                        cfg.archive.path.clone(),
                        cfg.graph_path.clone(),
                        storage_config_for_http,
                        cfg.layered_ingestion.clone(),
                        cfg.streaming.clone(),
                        cfg.yang.library_root.clone(),
                        cfg.yang.cache_root.clone(),
                        cfg.yang.bundle_key_env.clone(),
                        cfg.collector.filter.counter_forward_mode.clone(),
                        cfg.collector.filter.counter_window_secs,
                        cfg.collector.filter.counter_debounce_secs,
                        governor_for_http,
                        std::sync::Arc::clone(&sidecar_registry),
                    ),
                )
                .await
                {
                    warn!(%error, "HTTP server exited with error");
                }
            }));

            // Start enabled output adapters as background tasks.
            {
                let configs: Vec<_> = {
                    let reg = adapter_registry_handle.read().await;
                    reg.list()
                        .into_iter()
                        .filter(|(c, _)| c.enabled)
                        .map(|(c, _)| c)
                        .collect()
                };
                for config in configs {
                    if let Some(adapter) = bonsai::output::build_adapter(&config, store.db()) {
                        let bus_for_adapter = std::sync::Arc::clone(&bus);
                        let creds_for_adapter = std::sync::Arc::clone(&credentials);
                        let audit = bonsai::output::traits::OutputAdapterAuditLog::new(
                            std::path::Path::new("runtime"),
                            &config.name,
                        );
                        let adapter_shutdown = shutdown_rx.clone();
                        let adapter_registry = std::sync::Arc::clone(&adapter_registry_handle);
                        let adapter_name = config.name.clone();
                        tokio::spawn(async move {
                            adapter_registry
                                .write()
                                .await
                                .set_running(&adapter_name, true);
                            if let Err(e) = adapter
                                .run(bus_for_adapter, creds_for_adapter, audit, adapter_shutdown)
                                .await
                            {
                                warn!(adapter = %adapter.name(), error = %e, "output adapter exited with error");
                            }
                            adapter_registry
                                .write()
                                .await
                                .set_running(&adapter_name, false);
                        });
                        info!(adapter = %config.name, adapter_type = %config.adapter_type, "output adapter started");
                    }
                }
            }
        }

        // T2-4: ServiceNow EM push task — start if enabled in [integrations.servicenow]
        if run_core
            && cfg.integrations.servicenow.enabled
            && cfg.integrations.servicenow.em_push_enabled
        {
            let Some(adapter_registry_for_startup) = startup_adapter_registry.as_ref() else {
                unreachable!("adapter registry should exist in core mode");
            };
            let servicenow_adapter_enabled = {
                let reg = adapter_registry_for_startup.read().await;
                reg.list()
                    .into_iter()
                    .any(|(c, _)| c.enabled && c.adapter_type == "servicenow_em")
            };
            if servicenow_adapter_enabled {
                info!(
                    "skipping legacy ServiceNow EM pusher because servicenow_em output adapter is enabled"
                );
            } else {
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
        }

        if run_core
            && cfg.integrations.servicenow.enabled
            && cfg.integrations.servicenow.aiops.enabled
        {
            let snow_cfg = cfg.integrations.servicenow.clone();
            let creds_for_snow = std::sync::Arc::clone(&credentials);
            let store_for_snow = if let Store::Core(s) = store {
                std::sync::Arc::clone(s)
            } else {
                unreachable!()
            };
            bonsai::integrations::servicenow_aiops::maybe_start(
                &snow_cfg,
                store_for_snow,
                creds_for_snow,
                shutdown_rx.clone(),
            );
        }

        if run_core {
            let store_for_reconciler = if let Store::Core(s) = store {
                std::sync::Arc::clone(s)
            } else {
                unreachable!()
            };
            let reconciler_shutdown = shutdown_rx.clone();
            tokio::spawn(async move {
                bonsai::reconciler::run_reconciler(store_for_reconciler, reconciler_shutdown).await;
            });
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

    info!(
        phase = "ready",
        elapsed_ms = startup_start.elapsed().as_millis() as u64,
        "startup"
    );

    // T5-2: used by the startup-time CI budget workflow to measure cold-start latency.
    if std::env::args().any(|a| a == "--once-and-exit") {
        info!("--once-and-exit: startup verified, exiting");
        return Ok(());
    }

    // CV7 T3-4: react to BOTH SIGINT (Ctrl-C) and SIGTERM (systemd / pkill /
    // wrapper teardown). On either, propagate shutdown so the archive
    // consumer flushes open parquet writers before exit — the CV6 archive
    // code already handles `shutdown.changed()` correctly (src/archive.rs),
    // it just needed both signals to reach it.
    // D1-T1 (DV1): also react to the HTTP task completing — if axum::serve
    // returns, the listener closed and the process should exit rather than
    // running headless.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate())
            .context("install SIGTERM handler")?;
        if let Some(http) = http_task {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => info!("SIGINT received — shutting down"),
                _ = sigterm.recv()           => info!("SIGTERM received — shutting down"),
                _ = http                     => info!("HTTP server task ended — shutting down"),
            }
        } else {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => info!("SIGINT received — shutting down"),
                _ = sigterm.recv()           => info!("SIGTERM received — shutting down"),
            }
        }
    }
    #[cfg(not(unix))]
    {
        if let Some(http) = http_task {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => info!("Ctrl+C received - shutting down"),
                _ = http                     => info!("HTTP server task ended — shutting down"),
            }
        } else {
            tokio::signal::ctrl_c().await?;
            info!("Ctrl+C received - shutting down");
        }
    }
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

// ── Subscriber lifecycle ──────────────────────────────────────────────────────

pub(super) async fn spawn_subscriber(
    target: TargetConfig,
    credentials: &std::sync::Arc<CredentialVault>,
    bus: &std::sync::Arc<InProcessBus>,
    debouncer: Option<std::sync::Arc<ingest::TelemetryDebouncer>>,
    subscription_plan_tx: Option<&tokio::sync::mpsc::Sender<SubscriptionPlan>>,
    subscribers: &mut SubscriberHandleMap,
) -> Result<()> {
    use bonsai::subscriber::stop_subscriber;
    let _ = stop_subscriber; // keep import live — used by restart_subscriber
    let address = target.address.clone();
    if !target.enabled {
        tracing::info!(address = %address, "subscriber start skipped because target is disabled");
        return Ok(());
    }
    if subscribers.contains_key(&address) {
        tracing::info!(address = %address, "subscriber already running");
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
        debouncer,
        subscription_plan_tx.cloned(),
        target.selected_paths.clone(),
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(async move { subscriber.run_forever(shutdown_rx).await });
    subscribers.insert(address.clone(), (shutdown_tx, handle));
    tracing::info!(address = %address, "subscriber started");
    Ok(())
}

pub(super) async fn restart_subscriber(
    target: TargetConfig,
    credentials: &std::sync::Arc<CredentialVault>,
    bus: &std::sync::Arc<InProcessBus>,
    debouncer: Option<std::sync::Arc<ingest::TelemetryDebouncer>>,
    subscription_plan_tx: Option<&tokio::sync::mpsc::Sender<SubscriptionPlan>>,
    subscribers: &mut SubscriberHandleMap,
) -> Result<()> {
    let address = target.address.clone();
    bonsai::subscriber::stop_subscriber(&address, subscribers).await;
    spawn_subscriber(
        target,
        credentials,
        bus,
        debouncer,
        subscription_plan_tx,
        subscribers,
    )
    .await
}

pub(super) async fn load_ca_cert_pem(target: &TargetConfig) -> Result<Option<Vec<u8>>> {
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

pub(super) async fn seed_subscription_plan(
    target: TargetConfig,
    tx: &tokio::sync::mpsc::Sender<SubscriptionPlan>,
) {
    if !target.enabled {
        return;
    }
    let plan = subscriber::planned_subscription_plan_for_target(&target);
    if plan.paths.is_empty() {
        return;
    }
    if let Err(error) = tx.send(plan).await {
        warn!(%error, address = %target.address, "failed to seed subscription verifier plan");
    }
}

pub(super) fn resolve_target_credentials(
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

// ── Pre-flight disk space check ───────────────────────────────────────────────

pub(super) fn preflight_disk_check(log_dir: &std::path::Path, min_free_bytes: u64) -> Result<()> {
    let dir = if log_dir.exists() {
        log_dir.to_path_buf()
    } else {
        std::path::PathBuf::from(".")
    };

    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        let path_str = dir.to_str().unwrap_or(".");
        let c_path = CString::new(path_str).unwrap_or_default();
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if ret == 0 {
            let free_bytes = stat.f_bsize as u64 * stat.f_bavail as u64;
            if free_bytes < min_free_bytes {
                anyhow::bail!(
                    "insufficient disk space at '{}': {:.1} GiB free, {:.1} GiB required. \
                     Adjust [logging] min_free_bytes or free disk space before starting bonsai.",
                    dir.display(),
                    free_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
                    min_free_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
                );
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (dir, min_free_bytes);
    }

    Ok(())
}

// ── Log volume tracing Layer ──────────────────────────────────────────────────

/// Tracing Layer that increments a Prometheus counter for every log event.
pub(super) struct LogVolumeLayer;

impl<S> tracing_subscriber::Layer<S> for LogVolumeLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = event.metadata().level().as_str();
        metrics::counter!("bonsai_log_lines_total", "level" => level).increment(1);
    }
}
