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
    #[serde(skip_serializing_if = "Option::is_none")]
    missing_required_sidecars: Option<Vec<String>>,
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
    }
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
    match state.sidecar_registry.missing_required().await {
        Some(missing) if !missing.is_empty() => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResponse {
                status: "degraded",
                version: env!("CARGO_PKG_VERSION"),
                git_sha: env!("BONSAI_GIT_SHA"),
                build_ts: env!("BONSAI_BUILD_TS"),
                missing_required_sidecars: Some(missing),
            }),
        ),
        _ => (
            StatusCode::OK,
            Json(HealthResponse {
                status: "ok",
                version: env!("CARGO_PKG_VERSION"),
                git_sha: env!("BONSAI_GIT_SHA"),
                build_ts: env!("BONSAI_BUILD_TS"),
                missing_required_sidecars: None,
            }),
        ),
    }
}
