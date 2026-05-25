use std::collections::HashMap;
use std::fs;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use tracing::{error, info, warn};

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

use super::{GRAPH_PATH_DEFAULT, registry_path_for_graph_path};

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
    let mut cfg = config::load(&config_path).await?;
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
            source_types_json: String,
            latency_ns: i64,
            fired_at_ns: i64,
            state_change_event_id: String,
            source_event_ids: Vec<String>,
        ) -> Result<String> {
            match self {
                Store::Core(s) => {
                    s.write_detection(
                        device_address,
                        rule_id,
                        severity,
                        features_json,
                        source_types_json,
                        latency_ns,
                        fired_at_ns,
                        state_change_event_id,
                        source_event_ids,
                    )
                    .await
                }
                Store::Collector(s) => {
                    s.write_detection(
                        device_address,
                        rule_id,
                        severity,
                        features_json,
                        source_types_json,
                        latency_ns,
                        fired_at_ns,
                        state_change_event_id,
                        source_event_ids,
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
            let ddos_cfg = cfg.ddos.clone();
            move || graph::GraphStore::open_with_ddos(&p, pool_bytes, ddos_cfg)
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

    // D3-6 T6: channel for auto-investigate on unmatched detections.
    // Created unconditionally so the receiver can be moved into router() regardless;
    // the coordinator only sends when auto_investigate_unmatched=true AND the API key exists.
    let (investigation_tx, investigation_rx) = tokio::sync::mpsc::channel::<
        bonsai::write_coordinator::AutoInvestigateRequest,
    >(64);
    let auto_investigate = cfg.ai.auto_investigate_unmatched
        && std::env::var(&cfg.ai.api_key_env).is_ok();

    // D4-7 T5: Run YAML→ConfigItem migration synchronously BEFORE loading patterns,
    // so DB-first loaders see the migrated data on first boot.
    if let Some(Store::Core(ref core_store)) = store {
        match core_store.migrate_yaml_config("config").await {
            Ok(n) if n > 0 => info!(count = n, "config YAML migration complete"),
            Ok(_) => {}
            Err(e) => warn!(error = %e, "config YAML migration failed (non-fatal, will use disk fallback)"),
        }
    }

    // D4-7 T5: Restore all runtime config overrides from DB (streaming, retention, ai, etc.).
    if let Some(Store::Core(ref core_store)) = store {
        let n = bonsai::http_server::settings::apply_runtime_overrides_from_db(core_store, &mut cfg).await;
        if n > 0 {
            info!(count = n, "applied runtime config overrides from DB");
        }
    }

    // D4-7 T5: Load syslog/SNMP patterns from ConfigItem DB with disk fallback.
    let syslog_pattern_dir = cfg.layered_ingestion.syslog_patterns_path.clone();
    let snmp_oid_dir = cfg.signals.snmp.oid_pattern_dir
        .clone()
        .unwrap_or_else(|| "config/snmp_oid_patterns".to_string());

    let init_syslog_extractor = if let Some(Store::Core(ref core_store)) = store {
        let items = core_store.load_config_yaml_by_class("syslog_pattern").await.unwrap_or_default();
        std::sync::Arc::new(bonsai::signals::syslog::SyslogFactExtractor::load_from_yaml_strings(
            &items, &syslog_pattern_dir,
        ))
    } else {
        std::sync::Arc::new(bonsai::signals::syslog::SyslogFactExtractor::load_from_dir(
            &syslog_pattern_dir,
        ))
    };
    let (syslog_pattern_tx, syslog_pattern_rx) =
        tokio::sync::watch::channel(std::sync::Arc::clone(&init_syslog_extractor));
    let syslog_pattern_tx = std::sync::Arc::new(syslog_pattern_tx);

    let init_snmp_extractor = if let Some(Store::Core(ref core_store)) = store {
        let items = core_store.load_config_yaml_by_class("snmp_oid_pattern").await.unwrap_or_default();
        std::sync::Arc::new(bonsai::signals::snmp::SnmpFactExtractor::load_from_yaml_strings(
            &items, &snmp_oid_dir,
        ))
    } else {
        std::sync::Arc::new(bonsai::signals::snmp::SnmpFactExtractor::load_from_dir(&snmp_oid_dir))
    };
    let (snmp_pattern_tx, snmp_pattern_rx) =
        tokio::sync::watch::channel(std::sync::Arc::clone(&init_snmp_extractor));
    let snmp_pattern_tx = std::sync::Arc::new(snmp_pattern_tx);

    let coordinator = if let Some(Store::Core(ref s)) = store {
        let playbook_library = (cfg.remediation.auto_propose || auto_investigate).then(|| {
            bonsai::playbook::PlaybookLibrary::load_dir(&cfg.remediation.playbook_library_dir)
        });
        let coordinator_cfg = bonsai::write_coordinator::WriteCoordinatorConfig {
            governor: shared_governor.clone(),
            playbook_library,
            auto_propose: cfg.remediation.auto_propose,
            investigation_tx: auto_investigate.then_some(investigation_tx),
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

    // ── Correlation buffer sweep task ─────────────────────────────────────────
    if let Some(Store::Core(ref s)) = store {
        let corr_buf = std::sync::Arc::clone(&s.correlation_buffer);
        let detection_store = std::sync::Arc::clone(s);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let flushed = corr_buf.drain_expired();
                for slot in &flushed {
                    let severity = match slot.key.semantic_type.as_str() {
                        "bgp_neighbor_down" | "interface_down" | "bfd_session_down"
                        | "isis_adjacency_down" | "ospf_neighbor_down" => "critical",
                        "app_flow_high_utilization" | "flow_exporter_silent"
                        | "bgp_rib_prefix_spike" | "bgp_policy_filter_spike" => "medium",
                        "thermal_sensor_critical" => "critical",
                        "thermal_sensor_warning" => "warning",
                        "redundancy_lost" => "critical",
                        "redundancy_degraded" => "high",
                        "ddos_interface_pps_spike" => "high",
                        _ => "warning",
                    };
                    let source_types_json =
                        serde_json::to_string(&slot.source_types).unwrap_or_default();
                    let first_sce_id =
                        slot.state_change_event_ids.first().cloned().unwrap_or_default();
                    match detection_store
                        .write_detection(
                            slot.key.device_address.clone(),
                            slot.key.semantic_type.clone(),
                            severity.to_string(),
                            slot.detail_json.clone(),
                            source_types_json,
                            0,
                            slot.first_signal_ns,
                            first_sce_id,
                            slot.state_change_event_ids.clone(),
                        )
                        .await
                    {
                        Ok(det_id) => {
                            tracing::info!(
                                detection_id = %det_id,
                                device = %slot.key.device_address,
                                semantic = %slot.key.semantic_type,
                                sub_key = %slot.key.sub_key,
                                sources = ?slot.source_types,
                                multi_source = slot.is_multi_source(),
                                "detection event written"
                            );
                            if slot.is_multi_source() {
                                metrics::counter!(
                                    "bonsai_correlation_multi_source_total",
                                    "semantic" => slot.key.semantic_type.clone(),
                                )
                                .increment(1);
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                device = %slot.key.device_address,
                                semantic = %slot.key.semantic_type,
                                "failed to write detection event from correlation buffer"
                            );
                        }
                    }
                }
            }
        });
    }

    // ── D4-10 T1: flow_exporter_silent sweep ─────────────────────────────────
    // Every 5 minutes: detect exporters that have stopped sending flows.
    // An AppFlow node is considered silent if updated_at has not changed in
    // >300s (5× the expected 60s sFlow/NetFlow cycle).
    if let Some(Store::Core(ref s)) = store {
        let db_for_silent = s.db();
        let detection_for_silent = std::sync::Arc::clone(s);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let result = tokio::task::spawn_blocking({
                        let db = db_for_silent.clone();
                        move || -> anyhow::Result<Vec<(String, String, i64)>> {
                            use bonsai::graph::common::{now_ns, ts};
                            use lbug::Connection;
                            let conn = Connection::new(&db)?;
                            let cutoff_ns = now_ns() - 300_000_000_000_i64;
                            let cutoff_val = ts(cutoff_ns);
                            let mut stmt = conn.prepare(
                                "MATCH (f:AppFlow) \
                                 WHERE f.updated_at < $cutoff \
                                 RETURN f.exporter_address, f.id, f.updated_at",
                            )?;
                            let rows = conn.execute(&mut stmt, vec![("cutoff", cutoff_val)])?;
                            let mut out = Vec::new();
                            for row in rows {
                                let exp = match &row[0] { lbug::Value::String(s) => s.clone(), _ => continue };
                                let fid = match &row[1] { lbug::Value::String(s) => s.clone(), _ => continue };
                                let last_ns = match &row[2] {
                                    lbug::Value::TimestampNs(dt) => dt.unix_timestamp_nanos() as i64,
                                    _ => 0,
                                };
                                out.push((exp, fid, last_ns));
                            }
                            Ok(out)
                        }
                    })
                    .await;
                if let Ok(Ok(silent)) = result {
                    // Deduplicate by exporter — fire one detection per silent exporter
                    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                    for (exp, _fid, last_ns) in silent {
                        if seen.insert(exp.clone()) {
                            let detail = serde_json::json!({
                                "exporter_address": exp,
                                "last_seen_ns": last_ns,
                                "silent_seconds": 300,
                            })
                            .to_string();
                            let _ = detection_for_silent
                                .write_detection(
                                    exp,
                                    "flow_exporter_silent".to_string(),
                                    "medium".to_string(),
                                    detail,
                                    "[\"flow\"]".to_string(),
                                    0,
                                    last_ns,
                                    String::new(),
                                    vec![],
                                )
                                .await;
                        }
                    }
                }
            }
        });
    }

    // ── D4-2: Syslog shun engine — load active rules from graph DB at startup ──
    let shun_engine = bonsai::shun::ShunEngine::new();
    if let Some(Store::Core(ref s)) = store {
        match s.list_shun_rules().await {
            Ok(rules) => {
                info!(count = rules.len(), "syslog shun rules loaded");
                shun_engine.reload(rules);
            }
            Err(e) => warn!(error = %e, "failed to load shun rules at startup"),
        }
    }

    // ── D3-13 T2: Receiver Supervisor — all receivers managed via supervisor ──
    let supervisor = bonsai::receiver_supervisor::new_shared();

    macro_rules! spawn_or_register {
        ($name:literal, $addr:expr, $enabled:expr, $run_collector:expr, $factory:expr) => {{
            let mut sup = supervisor.write().await;
            if $enabled && $run_collector {
                sup.spawn($name, $addr.to_string(), $factory);
            } else {
                sup.register_disabled($name, $addr.to_string());
                if $enabled && !$run_collector {
                    info!(receiver = $name, "receiver enabled but no collector role; skipping");
                }
            }
        }};
    }

    {
        let syslog_cfg  = cfg.signals.syslog.clone();
        let pattern_dir = cfg.layered_ingestion.syslog_patterns_path.clone();
        let targets     = cfg.target.clone();
        let syslog_bus  = std::sync::Arc::clone(&bus);
        let governor    = shared_governor.clone();
        let shun_engine_for_syslog = if run_core {
            Some(std::sync::Arc::clone(&shun_engine))
        } else {
            None
        };
        spawn_or_register!(
            "syslog",
            cfg.signals.syslog.udp_addr.clone() + "/" + &cfg.signals.syslog.tcp_addr,
            cfg.signals.syslog.enabled,
            run_collector,
            |shutdown| async move {
                bonsai::signals::syslog::run_syslog_receiver(
                    syslog_cfg, pattern_dir, targets, syslog_bus, shutdown, governor,
                    shun_engine_for_syslog,
                    Some(syslog_pattern_rx),
                ).await
            }
        );
    }

    {
        let snmp_cfg  = cfg.signals.snmp.clone();
        let targets   = cfg.target.clone();
        let snmp_bus  = std::sync::Arc::clone(&bus);
        let snmp_gov  = shared_governor.clone().map(std::sync::Arc::new);
        spawn_or_register!(
            "snmp",
            cfg.signals.snmp.udp_addr.clone(),
            cfg.signals.snmp.enabled,
            run_collector,
            |shutdown| async move {
                bonsai::signals::snmp::run_snmp_receiver(
                    snmp_cfg, targets, snmp_bus, shutdown, snmp_gov,
                    Some(snmp_pattern_rx),
                ).await
            }
        );
    }

    {
        let bmp_cfg  = cfg.streaming.bmp.clone();
        let targets  = cfg.target.clone();
        let bmp_bus  = std::sync::Arc::clone(&bus);
        let governor = shared_governor.clone();
        spawn_or_register!(
            "bmp",
            cfg.streaming.bmp.tcp_addr.clone(),
            cfg.streaming.bmp.enabled,
            run_collector,
            |shutdown| async move {
                bonsai::streaming::bmp::run_bmp_receiver(
                    bmp_cfg, targets, bmp_bus, shutdown, governor,
                ).await
            }
        );
    }

    {
        let bgp_ls_cfg = cfg.streaming.bgp_ls.clone();
        let targets    = cfg.target.clone();
        let bgp_ls_bus = std::sync::Arc::clone(&bus);
        spawn_or_register!(
            "bgp_ls",
            cfg.streaming.bgp_ls.tcp_addr.clone(),
            cfg.streaming.bgp_ls.enabled,
            run_collector,
            |shutdown| async move {
                bonsai::streaming::bgp_ls::run_bgp_ls_receiver(
                    bgp_ls_cfg, targets, bgp_ls_bus, shutdown,
                ).await
            }
        );
    }

    if cfg.streaming.pcep.enabled {
        info!("PCEP ingest deferred; no runtime receiver started");
        let mut sup = supervisor.write().await;
        sup.register_disabled("pcep", cfg.streaming.pcep.tcp_addr.clone());
    }

    {
        let otlp_cfg = cfg.streaming.otlp.clone();
        let otlp_bus = std::sync::Arc::clone(&bus);
        spawn_or_register!(
            "otlp",
            cfg.streaming.otlp.http_addr.clone(),
            cfg.streaming.otlp.enabled,
            run_collector,
            |shutdown| async move {
                bonsai::streaming::otlp::run_otlp_receiver(
                    otlp_cfg, otlp_bus, shutdown,
                ).await
            }
        );
    }

    {
        let netflow_cfg = cfg.streaming.netflow.clone();
        let netflow_bus = std::sync::Arc::clone(&bus);
        spawn_or_register!(
            "netflow",
            cfg.streaming.netflow.udp_addr.clone(),
            cfg.streaming.netflow.enabled,
            run_collector,
            |shutdown| async move {
                bonsai::streaming::netflow::run_netflow_receiver(
                    netflow_cfg, netflow_bus, shutdown,
                ).await
            }
        );
    }

    {
        let sflow_cfg = cfg.streaming.sflow.clone();
        let sflow_bus = std::sync::Arc::clone(&bus);
        spawn_or_register!(
            "sflow",
            cfg.streaming.sflow.udp_addr.clone(),
            cfg.streaming.sflow.enabled,
            run_collector,
            |shutdown| async move {
                bonsai::streaming::sflow::run_sflow_receiver(
                    sflow_cfg, sflow_bus, shutdown,
                ).await
            }
        );
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
    let health_emitter_for_disk = std::sync::Arc::new(bonsai::health_emitter::HealthEmitter::new(bus.clone()));
    if run_core && (cfg.storage.max_archive_bytes > 0 || cfg.storage.max_graph_bytes > 0) {
        let archive_path = std::path::PathBuf::from(&cfg.archive.path);
        let graph_path = std::path::PathBuf::from(&cfg.graph_path);
        let storage_cfg = cfg.storage;
        let dg_shutdown = shutdown_rx.clone();
        tokio::spawn(bonsai::disk_guard::start(
            archive_path,
            graph_path,
            storage_cfg,
            dg_shutdown,
            Some(health_emitter_for_disk.clone()),
        ));
    }

    let registry_path = registry_path_for_graph_path(&cfg.graph_path);
    info!(path = %registry_path.display(), "opening API registry");
    let registry = std::sync::Arc::new(ApiRegistry::open(&registry_path, cfg.target.clone())?);
    info!(path = %registry_path.display(), "API registry opened");
    info!(
        path = %cfg.credentials.path,
        passphrase_env = %cfg.credentials.passphrase_env,
        "opening credential vault"
    );
    let credentials = std::sync::Arc::new(
        CredentialVault::open(&cfg.credentials.path, &cfg.credentials.passphrase_env)
            .map_err(|e| {
                // D4-14 T4: emit a structured log before propagating so the error
                // appears in the journal even if the process exits immediately.
                error!(
                    path = %cfg.credentials.path,
                    passphrase_env = %cfg.credentials.passphrase_env,
                    error = %e,
                    "FATAL: credential vault failed to open — bonsai cannot start"
                );
                e
            })?,
    );
    info!(path = %cfg.credentials.path, "credential vault open complete");

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
            )
            .with_health_emitter(health_emitter_for_disk.clone()),
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
        info!("syncing registry site labels into graph");
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

    // (D4-7 T2 migration now runs synchronously before pattern loading — see above.)

    // Wire the graph event channel into the collector manager so that
    // collector connect/disconnect events appear on the SSE stream.
    if let (Some(Store::Core(core_store)), Some(manager)) = (&store, &collector_manager) {
        manager.set_event_sender(core_store.event_sender());
    }

    // D4-21 T2: Wire governance state transitions into the SSE event stream.
    if let (Some(Store::Core(core_store)), Some(gov)) = (&store, &shared_governor) {
        gov.set_event_sender(core_store.event_sender());
    }

    // D4-9 T3: In mode=all, auto-register the in-process collector so
    // `/api/collectors` returns a real entry (fixes S-49/S-50).
    if run_core && run_collector {
        if let Some(ref manager) = collector_manager {
            let local_id = cfg.runtime.collector_id.clone();
            info!(%local_id, "mode=all: auto-registering local in-process collector");
            // register_collector returns a Receiver for assignment updates;
            // in mode=all the core already manages subscriptions directly, so
            // we just drop the receiver — it keeps the collector marked as connected.
            match manager.register_collector(local_id).await {
                Ok(_assignment_rx) => info!("local in-process collector registered"),
                Err(e) => warn!(%e, "failed to auto-register local collector (non-fatal)"),
            }
        }
    }

    // Seed the collector manager's site cache and keep it refreshed so that
    // hierarchy-aware assignment rules reflect current graph state.
    if let (Some(store), Some(manager)) = (&store, &collector_manager) {
        info!("seeding assignment site cache");
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
        info!("starting subscriber manager");
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
        let queue_depth_counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let queue_bytes_counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let queue_max_bytes_counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));

        let diag_port = cfg.collector.diagnostic_port;
        let diag_state = if diag_port > 0 {
            let ds = bonsai::collector::diagnostic_server::DiagnosticState::new(
                &cfg.runtime.collector_id,
            );
            let diag_shutdown = shutdown_rx.clone();
            tokio::spawn(bonsai::collector::diagnostic_server::start(
                diag_port,
                ds.clone(),
                diag_shutdown,
            ));
            Some(ds)
        } else {
            None
        };

        let forwarder_bus = std::sync::Arc::clone(&bus);
        let core_endpoint = cfg.runtime.core_ingest_endpoint.clone();
        let collector_id = cfg.runtime.collector_id.clone();
        let collector_config = cfg.collector.clone();
        let tls_config = cfg.runtime.tls.clone();
        let forwarder_shutdown = shutdown_rx.clone();
        let forwarder_counter = std::sync::Arc::clone(&queue_depth_counter);
        let forwarder_bytes = std::sync::Arc::clone(&queue_bytes_counter);
        let forwarder_max_bytes = std::sync::Arc::clone(&queue_max_bytes_counter);
        let forwarder_diag = diag_state.clone();

        tokio::spawn(async move {
            ingest::run_core_forwarder(
                forwarder_bus,
                core_endpoint,
                collector_id,
                collector_config,
                tls_config,
                forwarder_shutdown,
                forwarder_counter,
                forwarder_bytes,
                forwarder_max_bytes,
                forwarder_diag,
            )
            .await;
        });

        let collector_cfg = std::sync::Arc::new(cfg.collector.clone());
        let runtime_cfg = std::sync::Arc::new(cfg.runtime.clone());
        let collector_bus = std::sync::Arc::clone(&bus);
        let collector_plan_tx = subscription_plan_tx.clone();
        let collector_shutdown = shutdown_rx.clone();
        let manager_counter = std::sync::Arc::clone(&queue_depth_counter);
        let manager_bytes = std::sync::Arc::clone(&queue_bytes_counter);
        let manager_max_bytes = std::sync::Arc::clone(&queue_max_bytes_counter);
        let manager_diag = diag_state;

        let manager_supervisor = if run_collector {
            Some(std::sync::Arc::clone(&supervisor))
        } else {
            None
        };
        tokio::spawn(async move {
            if let Err(error) = ingest::run_collector_manager(
                runtime_cfg,
                collector_cfg,
                collector_bus,
                collector_plan_tx,
                collector_shutdown,
                manager_counter,
                manager_bytes,
                manager_max_bytes,
                manager_diag,
                manager_supervisor,
            )
            .await
            {
                warn!(%error, "collector manager failed");
            }
        });
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
                        None,
                        None,
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
                        None,
                        None,
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
        let investigation_api_key_env = cfg.ai.api_key_env.clone();
        let investigation_http_addr = cfg.http_addr.clone();
        if run_core {
            let http_store = if let Store::Core(s) = store {
                std::sync::Arc::clone(s)
            } else {
                unreachable!()
            };
            let http_addr: std::net::SocketAddr = cfg
                .http_addr
                .parse()
                .unwrap_or_else(|_| "0.0.0.0:3000".parse().unwrap());
            info!(%http_addr, "HTTP UI server listening");
            let registry_for_http = std::sync::Arc::clone(&registry);
            let credentials_for_http = std::sync::Arc::clone(&credentials);
            let change_detection_for_http = change_detection_runtime
                .as_ref()
                .map(std::sync::Arc::clone)
                .expect("change detection runtime should exist in core mode");
            let collector_manager_for_http = collector_manager.clone();
            let catalogue_dir = "config/path_profiles".to_string();
            let catalogue_state = if let Store::Core(cs) = &store {
                let items = cs.load_config_yaml_by_class("gnmi_path_profile").await.unwrap_or_default();
                catalogue::load_catalogue_from_yaml_strings(&items, std::path::Path::new(&catalogue_dir))
            } else {
                catalogue::load_catalogue(std::path::Path::new(&catalogue_dir))
            };
            let catalogue = std::sync::Arc::new(tokio::sync::RwLock::new(catalogue_state));
            let runtime_dir = "runtime".to_string();

            // D4-3 T7: Enforce runtime/ directory permissions (mode 700)
            {
                let rd = std::path::Path::new(&runtime_dir);
                if !rd.exists() {
                    fs::create_dir_all(rd).context("failed to create runtime/ directory")?;
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let meta = fs::metadata(rd).context("failed to stat runtime/ directory")?;
                    let mode = meta.permissions().mode() & 0o777;
                    if mode != 0o700 {
                        info!("runtime/ directory mode is {mode:03o}, enforcing 0700");
                        fs::set_permissions(rd, std::fs::Permissions::from_mode(0o700))
                            .context("failed to set runtime/ directory to mode 0700")?;
                    }
                }
            }

            let sqlite_store = std::sync::Arc::new(
                bonsai::sqlite_store::SqliteStore::open(std::path::Path::new(&runtime_dir))
                    .unwrap_or_else(|e| {
                        warn!("failed to open SQLite config store: {e}, falling back to JSON");
                        // Continue without SQLite - will use JSON fallback
                        panic!("SQLite store required for G5 features"); // For now, fail hard if SQLite unavailable
                    }),
            );

            // G3 Session 1: HA coordinator with etcd configuration
            let ha_mode = if std::env::var("BONSAI_HA_MODE").as_deref() == Ok("cluster") {
                let node_id = std::env::var("BONSAI_NODE_ID").unwrap_or_else(|_| "node-1".to_string());
                bonsai::ha_coordinator::HAMode::Cluster { node_id }
            } else {
                bonsai::ha_coordinator::HAMode::Standalone
            };

            // G3 Session 6: ConfigReplicator for config replication
            let cluster_node_id = if let bonsai::ha_coordinator::HAMode::Cluster { ref node_id } = ha_mode {
                node_id.clone()
            } else {
                "standalone".to_string()
            };

            // Build HACoordinator and ConfigReplicator, applying etcd config before Arc wrapping
            let mut ha_coordinator_val = bonsai::ha_coordinator::HACoordinator::new(ha_mode.clone());
            let mut config_replicator = bonsai::ha_coordinator::ConfigReplicator::new(cluster_node_id);

            // Add etcd configuration if in cluster mode and env vars are set
            if matches!(ha_mode, bonsai::ha_coordinator::HAMode::Cluster { .. }) {
                let etcd_endpoints_str = std::env::var("BONSAI_ETCD_ENDPOINTS").unwrap_or_default();
                if !etcd_endpoints_str.is_empty() {
                    let etcd_config = bonsai::ha_coordinator::EtcdConfig {
                        endpoints: etcd_endpoints_str
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect(),
                        election_ttl_secs: std::env::var("BONSAI_ETCD_ELECTION_TTL")
                            .unwrap_or_else(|_| "10".to_string())
                            .parse()
                            .unwrap_or(10),
                        config_prefix: std::env::var("BONSAI_ETCD_CONFIG_PREFIX")
                            .unwrap_or_else(|_| "/bonsai/config".to_string()),
                    };
                    ha_coordinator_val = ha_coordinator_val.with_etcd_config(etcd_config.clone());
                    config_replicator = config_replicator.with_etcd_config(etcd_config);
                }
            }
            let ha_coordinator = std::sync::Arc::new(ha_coordinator_val);

            tokio::spawn({
                let ha = ha_coordinator.clone();
                async move {
                    ha.start_election().await;
                }
            });

            // G3 Session 6: Start config change watcher
            if matches!(ha_mode, bonsai::ha_coordinator::HAMode::Cluster { .. }) {
                // Wire ConfigReplicator to SQLite store
                config_replicator = config_replicator.with_sqlite_store(sqlite_store.clone());
                let config_replicator = std::sync::Arc::new(config_replicator);
                let shutdown_signal = ha_coordinator.shutdown_signal();
                tokio::spawn(async move {
                    if let Err(e) = config_replicator.watch_and_apply_changes(shutdown_signal).await {
                        error!(error = %e, "config watcher failed");
                    }
                });
            }

            let enricher_registry =
                bonsai::enrichment::new_registry(std::path::Path::new(&runtime_dir));
            // G4: Start syslog adapter for health events if configured
            if std::env::var("BONSAI_SYSLOG_ENABLED").as_deref() == Ok("true") {
                let syslog_config = bonsai::output::syslog_adapter::SyslogConfig {
                    enabled: true,
                    endpoint: std::env::var("BONSAI_SYSLOG_ENDPOINT").unwrap_or_else(|_| "127.0.0.1:514".to_string()),
                    facility: std::env::var("BONSAI_SYSLOG_FACILITY").unwrap_or_else(|_| "local0".to_string()),
                };
                let syslog_adapter = std::sync::Arc::new(bonsai::output::syslog_adapter::SyslogAdapter::new(syslog_config, bus.clone()));
                syslog_adapter.start().await;
                info!("Syslog adapter started for health events");
            }

            // G4: Start SNMP adapter for health events if configured
            if std::env::var("BONSAI_SNMP_ENABLED").as_deref() == Ok("true") {
                let snmp_config = bonsai::output::snmp_adapter::SnmpConfig {
                    enabled: true,
                    target: std::env::var("BONSAI_SNMP_TARGET").unwrap_or_else(|_| "127.0.0.1".to_string()),
                    community: std::env::var("BONSAI_SNMP_COMMUNITY").unwrap_or_else(|_| "public".to_string()),
                    port: std::env::var("BONSAI_SNMP_PORT")
                        .unwrap_or_else(|_| "162".to_string())
                        .parse()
                        .unwrap_or(162),
                };
                let snmp_adapter = std::sync::Arc::new(bonsai::output::snmp_adapter::SnmpAdapter::new(snmp_config, bus.clone()));
                snmp_adapter.start().await;
                info!("SNMP adapter started for health events");
            }

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

            // D4-3 T7 / cert-mgmt: HTTPS — check for vault-managed cert_config overrides
            // first (set via POST /api/certs/apply), then fall back to bonsai.toml paths.
            let http_tls_acceptor: Option<tokio_rustls::TlsAcceptor> = {
                // Check DB for vault cert refs applied via UI.
                let db_cert = if let Store::Core(s) = &store {
                    let items = s.list_config_items(Some("cert_config".to_string())).await.unwrap_or_default();
                    let cert_ref = items.iter().find(|i| i.name == "cert_config:http_tls:cert" && i.enabled).map(|i| i.content_json.clone());
                    let key_ref  = items.iter().find(|i| i.name == "cert_config:http_tls:key"  && i.enabled).map(|i| i.content_json.clone());
                    cert_ref.zip(key_ref)
                } else { None };

                if let Some((cert_ref, key_ref)) = db_cert {
                    // Vault-managed cert path: resolve PEM from vault or filesystem.
                    // Non-fatal: if the vault ref is broken, log a warning and fall back
                    // to bonsai.toml [tls] config (or plain HTTP) so startup never fails
                    // due to a stale cert_config DB entry.
                    let vault_result = async {
                        let cert_pem = bonsai::tls_util::read_cert_pem(&cert_ref, &credentials).await
                            .with_context(|| format!("cert_config:http_tls:cert '{cert_ref}' unreadable"))?;
                        let key_pem = bonsai::tls_util::read_cert_pem(&key_ref, &credentials).await
                            .with_context(|| format!("cert_config:http_tls:key '{key_ref}' unreadable"))?;
                        build_http_tls_acceptor_from_pem(&cert_pem, &key_pem)
                            .context("failed to configure HTTP TLS from vault cert")
                    }.await;
                    match vault_result {
                        Ok(acceptor) => {
                            info!(addr = %http_addr, cert = %cert_ref, "HTTPS listener bound (TLS via vault)");
                            Some(acceptor)
                        }
                        Err(e) => {
                            warn!(addr = %http_addr, cert = %cert_ref, error = %e,
                                "vault cert_config found but cert unreadable — falling back to bonsai.toml tls or plain HTTP");
                            if cfg.tls.enabled {
                                let acceptor = build_http_tls_acceptor(&cfg.tls)
                                    .context("failed to configure HTTP TLS (toml fallback after vault failure)")?;
                                info!(addr = %http_addr, cert = %cfg.tls.cert_path, "HTTPS listener bound (TLS via toml fallback)");
                                Some(acceptor)
                            } else {
                                info!(addr = %http_addr, "HTTP listener bound (vault cert failed, TLS disabled)");
                                None
                            }
                        }
                    }
                } else if cfg.tls.enabled {
                    let acceptor = build_http_tls_acceptor(&cfg.tls)
                        .context("failed to configure HTTP TLS")?;
                    info!(addr = %http_addr, cert = %cfg.tls.cert_path, "HTTPS listener bound (TLS enabled)");
                    Some(acceptor)
                } else {
                    info!(addr = %http_addr, "HTTP listener bound");
                    None
                }
            };

            let bus_for_http = std::sync::Arc::clone(&bus);
            let ml_event_bus = std::sync::Arc::new(bonsai::ml_event_bus::MlEventBus::new());
            let ml_event_bus_for_scheduler = std::sync::Arc::clone(&ml_event_bus);

            // Initialize security module with selective feature enablement
            if let Err(e) = bonsai::security::initialize_security(cfg.security.clone()).await {
                error!(error = %e, "failed to initialize security module");
            }
            
            http_task = Some(tokio::spawn(async move {
                let router = bonsai::http_server::router(
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
                        cfg.signals.clone(),
                        cfg.yang.library_root.clone(),
                        cfg.yang.cache_root.clone(),
                        cfg.yang.bundle_key_env.clone(),
                        cfg.collector.filter.counter_forward_mode.clone(),
                        cfg.collector.filter.counter_window_secs,
                        cfg.collector.filter.counter_debounce_secs,
                        governor_for_http,
                        std::sync::Arc::clone(&sidecar_registry),
                        std::sync::Arc::clone(&supervisor),
                        bus_for_http,
                        Some(ha_coordinator),
                        cfg.target.clone(),
                        cfg.ai.clone(),
                        cfg.gnn.clone(),
                        auto_investigate.then_some(investigation_rx),
                        Some(std::sync::Arc::clone(&shun_engine)),
                        Some(std::sync::Arc::clone(&syslog_pattern_tx)),
                        Some(std::sync::Arc::clone(&snmp_pattern_tx)),
                        cfg.layered_ingestion.syslog_patterns_path.clone(),
                        snmp_oid_dir.clone(),
                        cfg.auth.ldap.clone(),
                        cfg.integrations.tsdb.clone(),
                        std::sync::Arc::clone(&ml_event_bus),
                );
                let serve_result = if let Some(tls) = http_tls_acceptor {
                    // HTTPS: accept each TCP connection, upgrade to TLS, then serve.
                    let mut join_set: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
                    loop {
                        let (tcp, _peer) = match http_listener.accept().await {
                            Ok(p) => p,
                            Err(e) => { warn!(%e, "HTTPS accept error"); continue; }
                        };
                        let tls_clone = tls.clone();
                        let svc = router.clone();
                        join_set.spawn(async move {
                            match tls_clone.accept(tcp).await {
                                Ok(stream) => {
                                    let io = hyper_util::rt::TokioIo::new(stream);
                                    let _ = hyper_util::server::conn::auto::Builder::new(
                                        hyper_util::rt::TokioExecutor::new(),
                                    )
                                    .serve_connection(io, hyper::service::service_fn(move |req| {
                                        let mut svc = svc.clone();
                                        async move { tower::Service::call(&mut svc, req).await }
                                    }))
                                    .await;
                                }
                                Err(e) => warn!(%e, "TLS handshake failed"),
                            }
                        });
                    }
                    #[allow(unreachable_code)]
                    Ok::<_, std::convert::Infallible>(())
                } else {
                    axum::serve(http_listener, router).await.map_err(|e| {
                        warn!(%e, "HTTP server exited with error");
                    }).ok();
                    Ok(())
                };
                if let Err(e) = serve_result {
                    warn!(error = %e, "HTTP server exited with error");
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

        // ── ServiceNow Change Management sync ────────────────────────────────
        if run_core
            && cfg.integrations.servicenow.enabled
            && cfg.integrations.servicenow.change_management.enabled
        {
            let snow_cfg = cfg.integrations.servicenow.clone();
            let creds_for_chg = std::sync::Arc::clone(&credentials);
            let store_for_chg = if let Store::Core(s) = store {
                std::sync::Arc::clone(s)
            } else {
                unreachable!()
            };
            bonsai::integrations::change_management::maybe_start(
                &snow_cfg,
                store_for_chg,
                creds_for_chg,
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

        // ── Investigation auto-trigger ───────────────────────────────────────
        if run_core {
            // Enable trigger if any AI provider env-var is set (vault-backed providers are
            // checked at trigger time via resolve_active_ai_provider; here we just gate on
            // whether the feature should be active at all).
            let has_api_key = !investigation_api_key_env.is_empty()
                && std::env::var(&investigation_api_key_env)
                    .map(|k| !k.is_empty())
                    .unwrap_or(false);
            let trigger_store = if let Store::Core(s) = store {
                std::sync::Arc::clone(s)
            } else {
                unreachable!()
            };
            let trigger_config = bonsai::investigation_trigger::InvestigationTriggerConfig {
                enabled: has_api_key,
                base_url: format!(
                    "http://127.0.0.1:{}",
                    investigation_http_addr.split(':').last().unwrap_or("3000")
                ),
                gnn_uncertainty_threshold: std::env::var("BONSAI_GNN_UNCERTAINTY_GATE")
                    .ok()
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.0),
            };
            let trigger_shutdown = shutdown_rx.clone();
            tokio::spawn(bonsai::investigation_trigger::run_investigation_trigger(
                trigger_store,
                trigger_config,
                trigger_shutdown,
            ));
        }

        // ── ML export schedule seeding + tick loop ───────────────────────────
        if run_core {
            let sched_store = if let Store::Core(s) = store {
                std::sync::Arc::clone(s)
            } else {
                unreachable!()
            };
            let sched_db = sched_store.db();
            let sched_wl = sched_store.write_lock();
            bonsai::http_server::ml_jobs::seed_default_ml_schedules(sched_db.clone(), sched_wl.clone());
            let sched_shutdown = shutdown_rx.clone();
            tokio::spawn(bonsai::http_server::ml_jobs::run_ml_schedule_tick(
                sched_db,
                sched_wl,
                ml_event_bus_for_scheduler,
                sched_shutdown,
            ));
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

/// Build a `tokio_rustls::TlsAcceptor` from raw PEM byte slices.
/// Used when cert/key come from the vault (resolved by tls_util::read_cert_pem).
fn build_http_tls_acceptor_from_pem(
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<tokio_rustls::TlsAcceptor> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_pem.as_ref())
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to parse TLS certificate PEM from vault")?;
    if certs.is_empty() {
        anyhow::bail!("no certificates found in vault cert PEM");
    }

    let key = rustls_pemfile::private_key(&mut key_pem.as_ref())
        .context("failed to parse TLS private key PEM from vault")?
        .ok_or_else(|| anyhow::anyhow!("no private key found in vault key PEM"))?;

    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, PrivateKeyDer::try_from(key).context("invalid private key")?)
        .context("failed to build rustls ServerConfig for HTTP TLS (vault)")?;

    Ok(tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(server_config)))
}

/// Build a `tokio_rustls::TlsAcceptor` from `[tls]` cert/key in bonsai.toml.
/// Called only when `tls.enabled = true` for the HTTP API server.
fn build_http_tls_acceptor(
    tls: &config::HttpTlsConfig,
) -> Result<tokio_rustls::TlsAcceptor> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let cert_path = if tls.cert_path.trim().is_empty() {
        anyhow::bail!("tls.cert_path is required when tls.enabled = true");
    } else {
        &tls.cert_path
    };
    let key_path = if tls.key_path.trim().is_empty() {
        anyhow::bail!("tls.key_path is required when tls.enabled = true");
    } else {
        &tls.key_path
    };

    let cert_pem = fs::read(cert_path)
        .with_context(|| format!("failed to read tls.cert_path '{cert_path}'"))?;
    let key_pem = fs::read(key_path)
        .with_context(|| format!("failed to read tls.key_path '{key_path}'"))?;

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to parse TLS certificate PEM")?;
    if certs.is_empty() {
        anyhow::bail!("no certificates found in tls.cert_path '{cert_path}'");
    }

    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
        .context("failed to parse TLS private key PEM")?
        .ok_or_else(|| anyhow::anyhow!("no private key found in tls.key_path '{key_path}'"))?;

    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, PrivateKeyDer::try_from(key).context("invalid private key")?)
        .context("failed to build rustls ServerConfig for HTTP TLS")?;

    Ok(tokio_rustls::TlsAcceptor::from(
        std::sync::Arc::new(server_config),
    ))
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

    let ca_cert_pem = match &target.ca_cert {
        Some(path) => Some(bonsai::tls_util::read_cert_pem(path, credentials).await?),
        None => None,
    };
    let resolved_credentials = resolve_target_credentials(&target, credentials)?;
    let (username, password) = match resolved_credentials {
        Some(credentials) => {
            let password = credentials.password_string();
            (Some(credentials.username), Some(password))
        }
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
            (Some(username), Some(password)) => Some(ResolvedCredential { username, password: zeroize::Zeroizing::new(password) }),
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
