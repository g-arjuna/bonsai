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

/// EV1-5 T7: GET /api/governance/pressure
///
/// Returns a lightweight pressure summary for the Python ML job engine.
/// The sidecar polls this every 30s and pauses heavy ML jobs (training,
/// clustering) when `should_shed` is true. Inference and embedding workers
/// continue running (smaller footprint).
pub(super) async fn governance_pressure_handler(State(state): State<AppState>) -> impl IntoResponse {
    match &state.governor {
        Some(g) => {
            let write_pressure = g.write_pressure_active();
            let memory_pressure = g.memory_pressure_active();
            let rate_shedding = g.is_shedding();
            let should_shed = g.should_shed();
            (StatusCode::OK, Json(serde_json::json!({
                "write_pressure": write_pressure,
                "memory_pressure": memory_pressure,
                "rate_shedding": rate_shedding,
                "should_shed": should_shed,
            }))).into_response()
        }
        None => (
            StatusCode::OK,
            Json(serde_json::json!({
                "write_pressure": false,
                "memory_pressure": false,
                "rate_shedding": false,
                "should_shed": false,
            })),
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

// ── D4-21 T3: Governance history (RSS + rate sparkline data) ──────────────

pub(super) async fn governance_history_handler(
    State(state): State<AppState>,
) -> impl IntoResponse {
    match &state.governor {
        Some(g) => {
            let snap = g.snapshot();
            let rss = crate::memory_profile::rss_bytes();
            (StatusCode::OK, Json(serde_json::json!({
                "current_rss_bytes": rss,
                "current_rss_mb": rss / (1024 * 1024),
                "memory_budget_mb": snap.memory_budget_mb,
                "rate_budget_eps": snap.rate_budget_eps,
                "profile": snap.profile,
                "memory_pressure_active": snap.memory_pressure_active,
                "write_pressure_active": snap.write_pressure_active,
                "rate_shedding_active": snap.rate_shedding_active,
                "counters": {
                    "memory_shrink": snap.memory_shrink_count,
                    "memory_flush": snap.memory_flush_count,
                    "write_batch_expand": snap.write_batch_expand_count,
                    "rate_shed": snap.rate_shed_count,
                },
            }))).into_response()
        },
        None => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "governance_not_started"})),
        ).into_response(),
    }
}

// ── D4-21 T4: Profile switcher ────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct ProfileSwitchRequest {
    pub profile: String,
}

pub(super) async fn governance_profile_handler(
    State(_state): State<AppState>,
    Json(req): Json<ProfileSwitchRequest>,
) -> impl IntoResponse {
    let valid = ["tiny", "small", "medium", "large", "xlarge"];
    if !valid.contains(&req.profile.to_lowercase().as_str()) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "error": format!("Invalid profile '{}'. Valid: {:?}", req.profile, valid)
        }))).into_response();
    }

    // Profile switching at runtime requires restarting the governor loops.
    // For now, acknowledge the request and document that a restart is needed.
    // The governor reads profile from config at startup; hot-swap requires
    // a channel to the governor loops (follow-up work).
    tracing::info!(profile = %req.profile, "governance profile switch requested (requires restart to take effect)");
    (StatusCode::OK, Json(serde_json::json!({
        "status": "accepted",
        "profile": req.profile,
        "note": "Profile change takes effect after restart. Hot-swap is planned for a future release.",
    }))).into_response()
}

// ── D4-9 T2: Sidecar status (proxied health + registry snapshot) ─────────

#[derive(Serialize)]
pub(super) struct SidecarStatusResponse {
    pub sidecars: Vec<SidecarStatusEntry>,
}

#[derive(Serialize)]
pub(super) struct SidecarStatusEntry {
    pub name: String,
    pub kind: String,
    pub status: String,
    pub version: String,
    pub rules_loaded: i64,
    pub last_detection_at_ns: u64,
    pub detections_today: i64,
    pub uptime_secs: f64,
    pub queue_depth: i64,
    pub health_reachable: bool,
}

// ── D4-9 T4: Sidecar rules visibility ─────────────────────────────────────

#[derive(Serialize)]
pub(super) struct SidecarRulesResponse {
    pub rules: Vec<SidecarRuleEntry>,
}

#[derive(Serialize)]
pub(super) struct SidecarRuleEntry {
    pub rule_id: String,
    pub sidecar_name: String,
    pub sidecar_kind: String,
    pub enabled: bool,
}

/// GET /api/sidecar/rules — list all rule_ids advertised by registered sidecars.
/// Each rule_id comes from the `capabilities` field set at RegisterSidecar.
/// The `enabled` flag reads from a ConfigItem with id=`sidecar-rule:{rule_id}`;
/// if no ConfigItem exists, the rule is considered enabled by default.
pub(super) async fn sidecar_rules_handler(
    State(state): State<AppState>,
) -> Result<Json<SidecarRulesResponse>, (StatusCode, String)> {
    let snapshots = state.sidecar_registry.snapshot().await;
    // Load disable-list from ConfigItem DB
    let disabled_items = state
        .store
        .list_config_items(Some("sidecar_rule_toggle".to_string()))
        .await
        .unwrap_or_default();
    let disabled_set: std::collections::HashSet<String> = disabled_items
        .iter()
        .filter(|ci| !ci.enabled)
        .map(|ci| ci.id.clone())
        .collect();

    let mut rules = Vec::new();
    for snap in &snapshots {
        for cap in &snap.entry.capabilities {
            let toggle_id = format!("sidecar-rule:{}", cap);
            rules.push(SidecarRuleEntry {
                rule_id: cap.clone(),
                sidecar_name: snap.entry.name.clone(),
                sidecar_kind: snap.entry.kind.clone(),
                enabled: !disabled_set.contains(&toggle_id),
            });
        }
    }
    Ok(Json(SidecarRulesResponse { rules }))
}

/// POST /api/sidecar/rules/{rule_id}/toggle — flip the enabled state.
/// Persists a ConfigItem with config_class=sidecar_rule_toggle.
pub(super) async fn sidecar_rule_toggle_handler(
    State(state): State<AppState>,
    axum::extract::Path(rule_id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let toggle_id = format!("sidecar-rule:{}", rule_id);
    let existing = state
        .store
        .list_config_items(Some("sidecar_rule_toggle".to_string()))
        .await
        .unwrap_or_default();
    let current = existing.iter().find(|ci| ci.id == toggle_id);
    let new_enabled = current.map(|ci| !ci.enabled).unwrap_or(false); // first toggle disables
    let item = crate::graph::ConfigItemRecord {
        id: toggle_id,
        config_class: "sidecar_rule_toggle".to_string(),
        vendor: String::new(),
        name: rule_id,
        version: String::new(),
        content_json: "{}".to_string(),
        enabled: new_enabled,
        created_by: "ui".to_string(),
    };
    state
        .store
        .upsert_config_item(item)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
}

// ── EV1-7 T2/T3/T6/T7: Rule parameters, shadow mode, analytics, syslog rules ─────────────

/// GET /api/sidecar/rules/{rule_id}/parameters — current parameters + defaults.
pub(super) async fn sidecar_rule_parameters_handler(
    State(state): State<AppState>,
    axum::extract::Path(rule_id): axum::extract::Path<String>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, String)> {
    let item_id = format!("sidecar-rule-params:{}", rule_id);
    let items = state
        .store
        .list_config_items(Some("sidecar_rule_params".to_string()))
        .await
        .unwrap_or_default();
    let current = items.iter().find(|ci| ci.id == item_id);
    let params: serde_json::Value = current
        .and_then(|ci| serde_json::from_str(&ci.content_json).ok())
        .unwrap_or(serde_json::json!({}));
    Ok(axum::Json(serde_json::json!({
        "rule_id": rule_id,
        "parameters": params,
    })))
}

#[derive(serde::Deserialize)]
pub(super) struct PatchRuleParamsBody {
    pub parameters: serde_json::Value,
}

/// PATCH /api/sidecar/rules/{rule_id}/parameters — save parameter overrides to ConfigItem DB.
pub(super) async fn patch_sidecar_rule_parameters_handler(
    State(state): State<AppState>,
    axum::extract::Path(rule_id): axum::extract::Path<String>,
    axum::Json(body): axum::Json<PatchRuleParamsBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let item_id = format!("sidecar-rule-params:{}", rule_id);
    let content_json = serde_json::to_string(&body.parameters)
        .unwrap_or_else(|_| "{}".to_string());
    let item = crate::graph::ConfigItemRecord {
        id: item_id,
        config_class: "sidecar_rule_params".to_string(),
        vendor: String::new(),
        name: rule_id,
        version: String::new(),
        content_json,
        enabled: true,
        created_by: "ui".to_string(),
    };
    state
        .store
        .upsert_config_item(item)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
}

#[derive(serde::Deserialize)]
pub(super) struct ShadowModeBody {
    pub enabled: bool,
}

/// POST /api/sidecar/rules/{rule_id}/shadow-mode — toggle shadow mode.
/// Picked up by rule override poller on next cycle.
pub(super) async fn sidecar_rule_shadow_mode_handler(
    State(state): State<AppState>,
    axum::extract::Path(rule_id): axum::extract::Path<String>,
    axum::Json(body): axum::Json<ShadowModeBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let item_id = format!("sidecar-rule-shadow:{}", rule_id);
    let item = crate::graph::ConfigItemRecord {
        id: item_id,
        config_class: "sidecar_rule_shadow".to_string(),
        vendor: String::new(),
        name: rule_id,
        version: String::new(),
        content_json: serde_json::json!({"shadow_mode": body.enabled}).to_string(),
        enabled: body.enabled,
        created_by: "ui".to_string(),
    };
    state
        .store
        .upsert_config_item(item)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
}

/// GET /api/sidecar/rules/analytics — per-rule firing analytics from DetectionEvent counts.
pub(super) async fn sidecar_rule_analytics_handler(
    State(state): State<AppState>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, String)> {
    use lbug::{Connection, Value};
    let db = state.store.db();
    let rows = tokio::task::spawn_blocking(move || -> Result<Vec<serde_json::Value>, String> {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let result = conn.query(
            "MATCH (d:DetectionEvent) \
             RETURN d.rule_id, d.severity, count(*) AS cnt, max(d.occurred_at_ns) AS last_fired_ns \
             ORDER BY cnt DESC LIMIT 100",
        )
        .map_err(|e| e.to_string())?;
        let rows: Vec<serde_json::Value> = result.map(|r| {
            let s = |v: &Value| match v { Value::String(s) => s.clone(), _ => String::new() };
            let n = |v: &Value| match v { Value::Int64(n) => *n, _ => 0 };
            serde_json::json!({
                "rule_id": s(&r[0]),
                "severity": s(&r[1]),
                "firing_count": n(&r[2]),
                "last_fired_ns": n(&r[3]),
            })
        }).collect();
        Ok(rows)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(axum::Json(serde_json::json!({ "analytics": rows })))
}

/// GET /api/sidecar/rules/{rule_id}/shadow-firings?since=ns — shadow firings for a rule.
/// The Python sidecar stores these in memory; this endpoint proxies via sidecar health API.
pub(super) async fn sidecar_rule_shadow_firings_handler(
    State(state): State<AppState>,
    axum::extract::Path(rule_id): axum::extract::Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, String)> {
    let since_ns = params.get("since").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let snapshots = state.sidecar_registry.snapshot().await;
    let first = snapshots.first();
    if first.is_none() {
        return Ok(axum::Json(serde_json::json!({ "shadow_firings": [] })));
    }
    let snap = first.unwrap();
    let host = snap.entry.address.split(':').next().unwrap_or("127.0.0.1");
    let health_port = std::env::var("BONSAI_SIDECAR_HEALTH_PORT").unwrap_or_else(|_| "9292".to_string());
    let url = format!("http://{}:{}/shadow-firings/{}?since={}", host, health_port, rule_id, since_ns);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_default();
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::json!({"shadow_firings": []}));
            Ok(axum::Json(body))
        }
        _ => Ok(axum::Json(serde_json::json!({ "shadow_firings": [] }))),
    }
}

// ── EV1-7 T6: Syslog pattern rule creation via UI ──────────────────────────────

#[derive(serde::Deserialize)]
pub(super) struct CreateSyslogRuleRequest {
    pub rule_id: String,
    pub description: String,
    pub pattern: String,
    pub event_type: String,
    pub severity: String,
    pub vendor: Option<String>,
    pub shadow_mode: Option<bool>,
}

/// POST /api/syslog-rules — create a new syslog pattern rule stored as ConfigItem.
/// The Python rule engine's syslog module loads these from the DB on next poller cycle.
pub(super) async fn create_syslog_rule_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<CreateSyslogRuleRequest>,
) -> Result<(StatusCode, axum::Json<serde_json::Value>), (StatusCode, String)> {
    if req.rule_id.is_empty() || req.pattern.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "rule_id and pattern are required".into()));
    }
    let content = serde_json::json!({
        "rule_id": req.rule_id,
        "description": req.description,
        "pattern": req.pattern,
        "event_type": req.event_type,
        "severity": req.severity,
        "shadow_mode": req.shadow_mode.unwrap_or(false),
    });
    let item = crate::graph::ConfigItemRecord {
        id: format!("syslog-rule:{}", req.rule_id),
        config_class: "syslog_pattern".to_string(),
        vendor: req.vendor.unwrap_or_default(),
        name: req.rule_id.clone(),
        version: "1".to_string(),
        content_json: content.to_string(),
        enabled: true,
        created_by: "ui".to_string(),
    };
    state
        .store
        .upsert_config_item(item)
        .await
        .map(|_| (StatusCode::CREATED, axum::Json(serde_json::json!({"rule_id": req.rule_id, "ok": true}))))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
}

/// GET /api/syslog-rules — list custom syslog pattern rules from ConfigItem DB.
pub(super) async fn list_syslog_rules_handler(
    State(state): State<AppState>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, String)> {
    let items = state
        .store
        .list_config_items(Some("syslog_pattern".to_string()))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let rules: Vec<serde_json::Value> = items
        .iter()
        .filter(|ci| ci.id.starts_with("syslog-rule:"))
        .map(|ci| {
            let mut v: serde_json::Value = serde_json::from_str(&ci.content_json)
                .unwrap_or(serde_json::json!({}));
            if let serde_json::Value::Object(ref mut m) = v {
                m.insert("enabled".to_string(), serde_json::json!(ci.enabled));
            }
            v
        })
        .collect();
    Ok(axum::Json(serde_json::json!({ "syslog_rules": rules })))
}

pub(super) async fn sidecar_status_handler(
    State(state): State<AppState>,
) -> Json<SidecarStatusResponse> {
    let snapshots = state.sidecar_registry.snapshot().await;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_default();

    let mut entries = Vec::new();
    for snap in &snapshots {
        let addr = &snap.entry.address;
        // Try to reach the sidecar's health HTTP endpoint.
        // The Python sidecar runs health on port 9292 by default; derive the
        // health URL from the registered gRPC address by using the same host
        // with the sidecar health port.
        let host = addr.split(':').next().unwrap_or("127.0.0.1");
        let health_port = std::env::var("BONSAI_SIDECAR_HEALTH_PORT").unwrap_or_else(|_| "9292".to_string());
        let health_url = format!("http://{}:{}/health", host, health_port);

        let (health_reachable, rules_loaded, last_det, det_today, uptime, queue) =
            match client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(body) = resp.json::<serde_json::Value>().await {
                        (
                            true,
                            body.get("rules_loaded").and_then(|v| v.as_i64()).unwrap_or(0),
                            body.get("last_detection_at_ns").and_then(|v| v.as_u64()).unwrap_or(0),
                            body.get("detections_today").and_then(|v| v.as_i64()).unwrap_or(0),
                            body.get("uptime_secs").and_then(|v| v.as_f64()).unwrap_or(0.0),
                            body.get("queue_depth").and_then(|v| v.as_i64()).unwrap_or(0),
                        )
                    } else {
                        (true, 0, 0, 0, 0.0, 0)
                    }
                }
                _ => (false, 0, snap.entry.last_heartbeat_ns, 0, 0.0, 0),
            };

        entries.push(SidecarStatusEntry {
            name: snap.entry.name.clone(),
            kind: snap.entry.kind.clone(),
            status: format!("{:?}", snap.status).to_lowercase(),
            version: snap.entry.version.clone(),
            rules_loaded,
            last_detection_at_ns: last_det,
            detections_today: det_today,
            uptime_secs: uptime,
            queue_depth: queue,
            health_reachable,
        });
    }
    Json(SidecarStatusResponse { sidecars: entries })
}
