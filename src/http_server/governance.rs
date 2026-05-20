#[derive(Serialize)]
pub(super) struct EnvironmentsResponse {
    environments: Vec<EnvironmentJson>,
}
#[derive(Serialize)]
pub(super) struct EnvironmentJson {
    id: String,
    name: String,
    archetype: String,
    created_at_ns: i64,
    metadata_json: String,
    site_count: i64,
    device_count: i64,
}
#[derive(Deserialize)]
pub(super) struct CreateEnvironmentRequest {
    #[serde(default)]
    id: String,
    name: String,
    archetype: String,
    #[serde(default)]
    metadata_json: String,
}
#[derive(Deserialize)]
pub(super) struct UpdateEnvironmentRequest {
    id: String,
    name: String,
    archetype: String,
    #[serde(default)]
    metadata_json: String,
}
#[derive(Deserialize)]
pub(super) struct RemoveEnvironmentRequest {
    id: String,
}
#[derive(Deserialize)]
pub(super) struct AssignSiteEnvironmentRequest {
    site_id: String,
    environment_id: String,
}
#[derive(Serialize)]
pub(super) struct EnvironmentMutationResponse {
    success: bool,
    error: String,
}
#[derive(Serialize)]
pub(super) struct SetupStatusResponse {
    is_first_run: bool,
    has_environments: bool,
    has_credentials: bool,
    has_devices: bool,
}
#[derive(Serialize)]
pub(super) struct AssignmentRulesResponse {
    rules: Vec<AssignmentRule>,
}
#[derive(Deserialize)]
pub(super) struct SetAssignmentRulesRequest {
    rules: Vec<AssignmentRule>,
}
#[derive(Serialize, Clone)]
pub(super) struct StreamingReceiverBadge {
    pub enabled: bool,
    pub addr: String,
    pub protocol: String,
}

#[derive(Serialize)]
pub(super) struct CollectorStatusJson {
    id: String,
    connected: bool,
    assigned_device_count: usize,
    assigned_targets: Vec<String>,
    queue_depth_updates: u64,
    subscription_count: u32,
    uptime_secs: i64,
    last_heartbeat_ns: i64,
    observed_subscriptions: usize,
    pending_subscriptions: usize,
    silent_subscriptions: usize,
    /// Streaming receiver status per protocol — populated from heartbeat-reported
    /// data for remote collectors; empty until receiver supervisor lands (D3-13).
    streaming_status: HashMap<String, StreamingReceiverBadge>,
    queue_bytes: u64,
    queue_utilization_pct: f32,
    active_subscribers: u32,
    failed_subscribers: u32,
    memory_used_mb: u64,
    recent_warn_count: u32,
    recent_error_count: u32,
    receiver_statuses: Vec<ReceiverStatusJson>,
}
#[derive(Serialize)]
pub(super) struct ReceiverStatusJson {
    name: String,
    state: String,
    addr: String,
    packet_count: u64,
    error_count: u64,
    last_error: Option<String>,
}
#[derive(Serialize)]
pub(super) struct AssignmentStatusResponse {
    collectors: Vec<CollectorStatusJson>,
    unassigned_count: usize,
    unassigned_devices: Vec<String>,
}
#[derive(Deserialize)]
pub(super) struct AssignmentOverrideRequest {
    device_address: String,
    collector_id: Option<String>,
}
#[derive(Serialize)]
pub(super) struct AssignmentOverrideResponse {
    success: bool,
    error: String,
}
#[derive(Serialize)]
pub(super) struct CollectorsResponse {
    collectors: Vec<CollectorStatusJson>,
    unassigned_count: usize,
    unassigned_devices: Vec<String>,
}
#[derive(Serialize)]
pub(super) struct SidecarsResponse {
    sidecars: Vec<crate::sidecar_registry::SidecarSnapshot>,
    required_kinds: Vec<String>,
    /// `None` while no kinds are required OR while still in the startup grace
    /// window. `Some([])` means all required kinds present. `Some([...])`
    /// means those kinds are missing or lost.
    missing_required: Option<Vec<String>>,
}
#[derive(Serialize)]
pub(super) struct HealthResponse {
    status: &'static str,
    version: &'static str,
    git_sha: &'static str,
    build_ts: &'static str,
    uptime_secs: u64,
    subsystems: SubsystemHealth,
}

#[derive(Serialize, Default)]
pub(super) struct SubsystemHealth {
    graph_db: ComponentHealth,
    collectors: CollectorHealth,
    sidecars: SidecarHealth,
    disk: DiskHealth,
    enrichers: EnricherHealth,
    governor: GovernorHealth,
    event_bus: EventBusHealth,
}

#[derive(Serialize, Default)]
pub(super) struct ComponentHealth {
    status: &'static str,       // "ok", "degraded", "failed"
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Serialize, Default)]
pub(super) struct CollectorHealth {
    status: &'static str,
    total: usize,
    connected: usize,
    disconnected: usize,
    stale_heartbeat: usize,     // heartbeat older than 90s
    unassigned_devices: usize,
}

#[derive(Serialize, Default)]
pub(super) struct SidecarHealth {
    status: &'static str,
    total: usize,
    healthy: usize,
    stale: usize,
    lost: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    missing_required: Option<Vec<String>>,
}

#[derive(Serialize, Default)]
pub(super) struct DiskHealth {
    status: &'static str,
    archive_bytes: u64,
    archive_pct: u8,
    graph_bytes: u64,
    graph_pct: u8,
}

#[derive(Serialize, Default)]
pub(super) struct EnricherHealth {
    status: &'static str,
    total: usize,
    enabled: usize,
    errored: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    errored_names: Vec<String>,
}

#[derive(Serialize, Default)]
pub(super) struct GovernorHealth {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Serialize, Default)]
pub(super) struct EventBusHealth {
    status: &'static str,
    subscriber_count: usize,
}

/// Simple liveness probe — returns 200 if the process is running.
pub(super) async fn healthz_handler() -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::OK, Json(serde_json::json!({"status": "alive"})))
}

/// K8s readiness probe — returns 200 only if graph is writable and at least
/// one collector is connected (or we're in standalone mode).
pub(super) async fn readyz_handler(
    State(state): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    // Probe graph
    let graph_ok = probe_graph_db(&state).await;
    let collectors_ok = state.collector_manager.as_ref()
        .map(|m| {
            let summary = m.collector_status_summary();
            summary.collectors.iter().any(|c| c.connected)
        })
        .unwrap_or(true); // standalone mode: no collectors needed

    let ready = graph_ok && collectors_ok;
    let code = if ready { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    let reason = if ready { "ready" } else if !graph_ok { "graph_db_unavailable" } else { "no_collectors_connected" };
    (code, Json(serde_json::json!({"ready": ready, "reason": reason})))
}
use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use axum::{Json, response::IntoResponse, extract::State, http::StatusCode};

use super::AppState;
use super::{SubscriptionStatusJson, read_subscription_statuses};
use crate::assignment::CollectorStatus;
use crate::config::AssignmentRule;
use crate::graph::EnvironmentRecord;
use crate::registry::DeviceRegistry;

pub(super) async fn assignment_rules_handler(
    State(state): State<AppState>,
) -> Result<Json<AssignmentRulesResponse>, (StatusCode, String)> {
    let rules = state
        .collector_manager
        .as_ref()
        .map(|m| m.get_rules())
        .unwrap_or_default();
    Ok(Json(AssignmentRulesResponse { rules }))
}
pub(super) async fn collectors_handler(
    State(state): State<AppState>,
) -> Result<Json<CollectorsResponse>, (StatusCode, String)> {
    let summary = state
        .collector_manager
        .as_ref()
        .map(|manager| manager.collector_status_summary())
        .unwrap_or_else(|| crate::assignment::CollectorStatusSummary {
            collectors: Vec::new(),
            unassigned_devices: Vec::new(),
        });
    let statuses = read_subscription_statuses(state.store.db()).await?;
    let collectors = summary
        .collectors
        .into_iter()
        .map(|collector| collector_status_with_subscription_json(collector, &statuses))
        .collect();
    Ok(Json(CollectorsResponse {
        unassigned_count: summary.unassigned_devices.len(),
        unassigned_devices: summary.unassigned_devices,
        collectors,
    }))
}
pub(super) async fn set_assignment_rules_handler(
    State(state): State<AppState>,
    Json(body): Json<SetAssignmentRulesRequest>,
) -> Result<Json<AssignmentRulesResponse>, (StatusCode, String)> {
    let manager = state.collector_manager.as_ref().ok_or_else(|| {
        (
            StatusCode::NOT_IMPLEMENTED,
            "assignment not enabled on this node".to_string(),
        )
    })?;
    manager.set_rules(body.rules);
    let rules = manager.get_rules();
    Ok(Json(AssignmentRulesResponse { rules }))
}
pub(super) async fn assignment_status_handler(
    State(state): State<AppState>,
) -> Result<Json<AssignmentStatusResponse>, (StatusCode, String)> {
    let summary = state
        .collector_manager
        .as_ref()
        .map(|m| m.collector_status_summary())
        .unwrap_or_else(|| crate::assignment::CollectorStatusSummary {
            collectors: vec![],
            unassigned_devices: vec![],
        });
    let statuses = read_subscription_statuses(state.store.db()).await?;
    let unassigned_count = summary.unassigned_devices.len();
    let collectors = summary
        .collectors
        .into_iter()
        .map(|collector| collector_status_with_subscription_json(collector, &statuses))
        .collect();
    Ok(Json(AssignmentStatusResponse {
        collectors,
        unassigned_count,
        unassigned_devices: summary.unassigned_devices,
    }))
}

pub(super) fn collector_status_json(s: CollectorStatus) -> CollectorStatusJson {
    CollectorStatusJson {
        id: s.id,
        connected: s.connected,
        assigned_device_count: s.assigned_device_count,
        assigned_targets: s.assigned_targets,
        queue_depth_updates: s.queue_depth_updates,
        subscription_count: s.subscription_count,
        uptime_secs: s.uptime_secs,
        last_heartbeat_ns: s.last_heartbeat_ns,
        observed_subscriptions: 0,
        pending_subscriptions: 0,
        silent_subscriptions: 0,
        streaming_status: HashMap::new(),
        queue_bytes: s.queue_bytes,
        queue_utilization_pct: s.queue_utilization_pct,
        active_subscribers: s.active_subscribers,
        failed_subscribers: s.failed_subscribers,
        memory_used_mb: s.memory_used_bytes / (1024 * 1024),
        recent_warn_count: s.recent_warn_count,
        recent_error_count: s.recent_error_count,
        receiver_statuses: s.receiver_statuses.into_iter().map(|r| ReceiverStatusJson {
            name: r.name,
            state: r.state,
            addr: r.addr,
            packet_count: r.packet_count,
            error_count: r.error_count,
            last_error: r.last_error,
        }).collect(),
    }
}

#[allow(dead_code)]
pub(super) fn streaming_status_from_config(
    streaming: &crate::config::StreamingConfig,
) -> HashMap<String, StreamingReceiverBadge> {
    let mut m = HashMap::new();
    m.insert("bmp".into(),     StreamingReceiverBadge { enabled: streaming.bmp.enabled,     addr: streaming.bmp.tcp_addr.clone(),    protocol: "tcp".into() });
    m.insert("bgp_ls".into(),  StreamingReceiverBadge { enabled: streaming.bgp_ls.enabled,  addr: streaming.bgp_ls.tcp_addr.clone(), protocol: "tcp".into() });
    m.insert("pcep".into(),    StreamingReceiverBadge { enabled: streaming.pcep.enabled,    addr: streaming.pcep.tcp_addr.clone(),   protocol: "tcp".into() });
    m.insert("otlp".into(),    StreamingReceiverBadge { enabled: streaming.otlp.enabled,    addr: streaming.otlp.http_addr.clone(),  protocol: "http".into() });
    m.insert("netflow".into(), StreamingReceiverBadge { enabled: streaming.netflow.enabled, addr: streaming.netflow.udp_addr.clone(), protocol: "udp".into() });
    m
}
pub(super) fn collector_status_with_subscription_json(
    collector: CollectorStatus,
    statuses: &HashMap<String, Vec<SubscriptionStatusJson>>,
) -> CollectorStatusJson {
    let mut json = collector_status_json(collector);
    for address in &json.assigned_targets {
        for status in statuses.get(address).cloned().unwrap_or_default() {
            match status.status.as_str() {
                "observed" => json.observed_subscriptions += 1,
                "pending" => json.pending_subscriptions += 1,
                _ => json.silent_subscriptions += 1,
            }
        }
    }
    json
}
pub(super) async fn assignment_override_handler(
    State(state): State<AppState>,
    Json(req): Json<AssignmentOverrideRequest>,
) -> Result<Json<AssignmentOverrideResponse>, (StatusCode, String)> {
    match state.registry.assign_device_with_audit(
        &req.device_address,
        req.collector_id,
        "api",
        "api_assignment_override",
    ) {
        Ok(_) => Ok(Json(AssignmentOverrideResponse {
            success: true,
            error: String::new(),
        })),
        Err(e) => Ok(Json(AssignmentOverrideResponse {
            success: false,
            error: format!("{e:#}"),
        })),
    }
}
pub(super) async fn environments_handler(
    State(state): State<AppState>,
) -> Result<Json<EnvironmentsResponse>, (StatusCode, String)> {
    let envs = state
        .store
        .list_environments()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(EnvironmentsResponse {
        environments: envs
            .into_iter()
            .map(|e| EnvironmentJson {
                id: e.id,
                name: e.name,
                archetype: e.archetype,
                created_at_ns: e.created_at_ns,
                metadata_json: e.metadata_json,
                site_count: e.site_count,
                device_count: e.device_count,
            })
            .collect(),
    }))
}
pub(super) async fn create_environment_handler(
    State(state): State<AppState>,
    Json(req): Json<CreateEnvironmentRequest>,
) -> Result<Json<EnvironmentMutationResponse>, (StatusCode, String)> {
    let record = EnvironmentRecord {
        id: req.id,
        name: req.name,
        archetype: req.archetype,
        created_at_ns: 0,
        metadata_json: req.metadata_json,
    };
    match state.store.create_environment(record).await {
        Ok(_) => Ok(Json(EnvironmentMutationResponse {
            success: true,
            error: String::new(),
        })),
        Err(e) => Ok(Json(EnvironmentMutationResponse {
            success: false,
            error: format!("{e:#}"),
        })),
    }
}
pub(super) async fn update_environment_handler(
    State(state): State<AppState>,
    Json(req): Json<UpdateEnvironmentRequest>,
) -> Result<Json<EnvironmentMutationResponse>, (StatusCode, String)> {
    let record = EnvironmentRecord {
        id: req.id,
        name: req.name,
        archetype: req.archetype,
        created_at_ns: 0,
        metadata_json: req.metadata_json,
    };
    match state.store.update_environment(record).await {
        Ok(_) => Ok(Json(EnvironmentMutationResponse {
            success: true,
            error: String::new(),
        })),
        Err(e) => Ok(Json(EnvironmentMutationResponse {
            success: false,
            error: format!("{e:#}"),
        })),
    }
}
pub(super) async fn remove_environment_handler(
    State(state): State<AppState>,
    Json(req): Json<RemoveEnvironmentRequest>,
) -> Result<Json<EnvironmentMutationResponse>, (StatusCode, String)> {
    match state.store.delete_environment(req.id).await {
        Ok(Ok(())) => Ok(Json(EnvironmentMutationResponse {
            success: true,
            error: String::new(),
        })),
        Ok(Err(msg)) => Ok(Json(EnvironmentMutationResponse {
            success: false,
            error: msg,
        })),
        Err(e) => Ok(Json(EnvironmentMutationResponse {
            success: false,
            error: format!("{e:#}"),
        })),
    }
}
pub(super) async fn assign_site_environment_handler(
    State(state): State<AppState>,
    Json(req): Json<AssignSiteEnvironmentRequest>,
) -> Result<Json<EnvironmentMutationResponse>, (StatusCode, String)> {
    match state
        .store
        .assign_site_to_environment(req.site_id, req.environment_id)
        .await
    {
        Ok(()) => Ok(Json(EnvironmentMutationResponse {
            success: true,
            error: String::new(),
        })),
        Err(e) => Ok(Json(EnvironmentMutationResponse {
            success: false,
            error: format!("{e:#}"),
        })),
    }
}
/// Returns first-run state so the UI can decide whether to route to /setup.
pub(super) async fn setup_status_handler(
    State(state): State<AppState>,
) -> Result<Json<SetupStatusResponse>, (StatusCode, String)> {
    let envs = state
        .store
        .list_environments()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let non_default_envs = envs
        .iter()
        .any(|e| e.id != crate::graph::DEFAULT_ENVIRONMENT_ID);
    let has_credentials = state
        .credentials
        .list()
        .map(|creds| !creds.is_empty())
        .unwrap_or(false);
    let has_devices = state
        .registry
        .list_active()
        .map(|devices| !devices.is_empty())
        .unwrap_or(false);

    let is_first_run = !non_default_envs && !has_credentials && !has_devices;

    Ok(Json(SetupStatusResponse {
        is_first_run,
        has_environments: non_default_envs,
        has_credentials,
        has_devices,
    }))
}
pub(super) async fn governance_state_handler(State(state): State<AppState>) -> impl IntoResponse {
    match &state.governor {
        Some(g) => (StatusCode::OK, Json(serde_json::json!(g.snapshot()))).into_response(),
        None => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "governance_not_started"})),
        )
            .into_response(),
    }
}
pub(super) async fn sidecars_handler(State(state): State<AppState>) -> Json<SidecarsResponse> {
    let sidecars = state.sidecar_registry.snapshot().await;
    let required_kinds = state.sidecar_registry.required_kinds().await;
    let missing_required = state.sidecar_registry.missing_required().await;
    Json(SidecarsResponse {
        sidecars,
        required_kinds,
        missing_required,
    })
}
pub(super) async fn health_handler(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    let uptime_secs = start.elapsed().as_secs();

    // ── Graph DB probe ────────────────────────────────────────────────────
    let graph_ok = probe_graph_db(&state).await;
    let graph_db = ComponentHealth {
        status: if graph_ok { "ok" } else { "failed" },
        detail: if graph_ok { None } else { Some("graph DB unreachable or read failed".into()) },
    };

    // ── Collectors ─────────────────────────────────────────────────────────
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0);
    let stale_threshold_ns: i64 = 90 * 1_000_000_000; // 3× heartbeat interval
    let collectors = if let Some(ref m) = state.collector_manager {
        let summary = m.collector_status_summary();
        let connected = summary.collectors.iter().filter(|c| c.connected).count();
        let disconnected = summary.collectors.len() - connected;
        let stale = summary.collectors.iter().filter(|c| {
            c.connected && c.last_heartbeat_ns > 0
                && (now_ns - c.last_heartbeat_ns) > stale_threshold_ns
        }).count();
        let status = if summary.collectors.is_empty() {
            "ok" // no collectors expected
        } else if connected == 0 {
            "failed"
        } else if stale > 0 || disconnected > 0 {
            "degraded"
        } else {
            "ok"
        };
        CollectorHealth {
            status,
            total: summary.collectors.len(),
            connected,
            disconnected,
            stale_heartbeat: stale,
            unassigned_devices: summary.unassigned_devices.len(),
        }
    } else {
        CollectorHealth { status: "ok", ..Default::default() }
    };

    // ── Sidecars ──────────────────────────────────────────────────────────
    let sidecar_snapshots = state.sidecar_registry.snapshot().await;
    let missing_req = state.sidecar_registry.missing_required().await;
    let sc_healthy = sidecar_snapshots.iter()
        .filter(|s| matches!(s.status, crate::sidecar_registry::SidecarStatus::Healthy))
        .count();
    let sc_stale = sidecar_snapshots.iter()
        .filter(|s| matches!(s.status, crate::sidecar_registry::SidecarStatus::Stale))
        .count();
    let sc_lost = sidecar_snapshots.iter()
        .filter(|s| matches!(s.status, crate::sidecar_registry::SidecarStatus::Lost))
        .count();
    let sidecar_status = match &missing_req {
        Some(m) if !m.is_empty() => "degraded",
        _ if sc_lost > 0 => "degraded",
        _ => "ok",
    };
    let sidecars = SidecarHealth {
        status: sidecar_status,
        total: sidecar_snapshots.len(),
        healthy: sc_healthy,
        stale: sc_stale,
        lost: sc_lost,
        missing_required: missing_req.clone(),
    };

    // ── Disk ──────────────────────────────────────────────────────────────
    let disk_snap = crate::disk_guard::snapshot(
        std::path::Path::new(&state.archive_path),
        std::path::Path::new(&state.graph_path),
        &state.storage_config,
    );
    let disk_status = if disk_snap.archive_pct >= 95 || disk_snap.graph_pct >= 95 {
        "failed"
    } else if disk_snap.archive_pct >= 80 || disk_snap.graph_pct >= 80 {
        "degraded"
    } else {
        "ok"
    };
    let disk = DiskHealth {
        status: disk_status,
        archive_bytes: disk_snap.archive_bytes,
        archive_pct: disk_snap.archive_pct,
        graph_bytes: disk_snap.graph_bytes,
        graph_pct: disk_snap.graph_pct,
    };

    // ── Enrichers ────────────────────────────────────────────────────────
    let (enricher_total, enricher_enabled, errored_names) = {
        let reg = state.enricher_registry.read().await;
        let items = reg.list();
        let total = items.len();
        let enabled = items.iter().filter(|(c, _)| c.enabled).count();
        let errored: Vec<String> = items.iter()
            .filter(|(c, s)| c.enabled && s.last_run_error.is_some())
            .map(|(c, _)| c.name.clone())
            .collect();
        (total, enabled, errored)
    };
    let enrichers = EnricherHealth {
        status: if errored_names.is_empty() { "ok" } else { "degraded" },
        total: enricher_total,
        enabled: enricher_enabled,
        errored: errored_names.len(),
        errored_names,
    };

    // ── Governor ──────────────────────────────────────────────────────────
    let governor = match &state.governor {
        Some(g) => {
            let snap = g.snapshot();
            let any_pressure = snap.memory_pressure_active
                || snap.write_pressure_active
                || snap.rate_shedding_active;
            let status = if any_pressure { "degraded" } else { "ok" };
            GovernorHealth {
                status,
                detail: if any_pressure {
                    let mut reasons = Vec::new();
                    if snap.memory_pressure_active { reasons.push("memory_pressure"); }
                    if snap.write_pressure_active { reasons.push("write_pressure"); }
                    if snap.rate_shedding_active { reasons.push("rate_shedding"); }
                    Some(format!("active: {}", reasons.join(", ")))
                } else {
                    None
                },
            }
        }
        None => GovernorHealth { status: "ok", detail: None },
    };

    // ── Event bus ────────────────────────────────────────────────────────
    let bus_subs = state.event_bus.subscriber_count();
    let event_bus = EventBusHealth {
        status: "ok",
        subscriber_count: bus_subs,
    };

    // ── Aggregate ────────────────────────────────────────────────────────
    let subsystems = SubsystemHealth {
        graph_db,
        collectors,
        sidecars,
        disk,
        enrichers,
        governor,
        event_bus,
    };

    let worst = worst_status(&[
        subsystems.graph_db.status,
        subsystems.collectors.status,
        subsystems.sidecars.status,
        subsystems.disk.status,
        subsystems.enrichers.status,
        subsystems.governor.status,
    ]);

    let http_code = match worst {
        "ok" => StatusCode::OK,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };

    // Prometheus gauge
    let health_val = match worst {
        "ok" => 1.0_f64,
        "degraded" => 0.5,
        _ => 0.0,
    };
    metrics::gauge!("bonsai_health_status").set(health_val);

    (http_code, Json(HealthResponse {
        status: worst,
        version: env!("CARGO_PKG_VERSION"),
        git_sha: env!("BONSAI_GIT_SHA"),
        build_ts: env!("BONSAI_BUILD_TS"),
        uptime_secs,
        subsystems,
    }))
}

/// Probe graph DB with a trivial read query.
async fn probe_graph_db(state: &AppState) -> bool {
    let db = state.store.db();
    tokio::task::spawn_blocking(move || {
        let conn = match lbug::Connection::new(&db) {
            Ok(c) => c,
            Err(_) => return false,
        };
        conn.query("MATCH (d:Device) RETURN count(d) LIMIT 1").is_ok()
    })
    .await
    .unwrap_or(false)
}

fn worst_status(statuses: &[&str]) -> &'static str {
    if statuses.iter().any(|s| *s == "failed") {
        "failed"
    } else if statuses.iter().any(|s| *s == "degraded") {
        "degraded"
    } else {
        "ok"
    }
}
