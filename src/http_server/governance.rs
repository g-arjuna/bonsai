#![allow(unused_imports, dead_code, unused_variables)]
use super::*;

// ── Incidents endpoint ────────────────────────────────────────────────────────

async fn incidents_handler(
    State(state): State<AppState>,
    Query(params): Query<IncidentsParams>,
) -> Result<Json<IncidentsResponse>, (StatusCode, String)> {
    let detections = state
        .store
        .read_detections(params.limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Build a device-degree map from LLDP topology. Higher-degree devices are treated as
    // more "upstream" when selecting the root detection within a grouped incident.
    let db = state.store.db();
    let degree_map: HashMap<String, usize> = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let rows = conn
            .query(
                "MATCH (a:Interface)-[:CONNECTED_TO]->(:Interface) \
                 RETURN a.device_address",
            )
            .map_err(|e| e.to_string())?;
        let mut map: HashMap<String, usize> = HashMap::new();
        for row in rows {
            *map.entry(read_str(&row[0])).or_insert(0) += 1;
        }
        Ok::<_, String>(map)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .unwrap_or_default();

    let incidents = group_into_incidents(detections, params.window_secs, &degree_map);
    Ok(Json(IncidentsResponse {
        schema_version: API_SCHEMA_VERSION.to_string(),
        incidents,
    }))
}

/// Groups a list of detections into incidents by time window.
/// Root = highest-degree device (most upstream in topology) among the group;
/// tie-breaks by earliest fired_at_ns. Incidents are returned newest-first.
fn group_into_incidents(
    mut detections: Vec<DetectionRow>,
    window_secs: u64,
    degree_map: &HashMap<String, usize>,
) -> Vec<IncidentJson> {
    detections.sort_by_key(|d| d.fired_at_ns);
    let window_ns = (window_secs as i64).saturating_mul(1_000_000_000);

    let mut groups: Vec<Vec<DetectionRow>> = Vec::new();

    for det in detections {
        let joined = groups
            .iter_mut()
            .rev()
            .find(|g| det.fired_at_ns - g[0].fired_at_ns <= window_ns);
        if let Some(group) = joined {
            group.push(det);
        } else {
            groups.push(vec![det]);
        }
    }

    let severity_rank = |s: &str| match s {
        "critical" => 3,
        "high" => 2,
        "warn" | "warning" => 1,
        _ => 0,
    };

    let mut incidents: Vec<IncidentJson> = groups
        .into_iter()
        .map(|mut group| {
            group.sort_by_key(|d| d.fired_at_ns);
            let started_at_ns = group[0].fired_at_ns;
            let ended_at_ns = group.last().map_or(started_at_ns, |d| d.fired_at_ns);

            // Pick root: highest topology degree (most upstream), then earliest time.
            let root_idx = group
                .iter()
                .enumerate()
                .max_by_key(|(_, d)| {
                    (
                        *degree_map.get(&d.device_address).unwrap_or(&0),
                        -(d.fired_at_ns),
                    )
                })
                .map(|(i, _)| i)
                .unwrap_or(0);
            let root = group.remove(root_idx);
            let id = root.id.clone();

            let severity = std::iter::once(&root)
                .chain(group.iter())
                .max_by_key(|d| severity_rank(&d.severity))
                .map_or("info".to_string(), |d| d.severity.clone());
            let remediation_status = std::iter::once(&root)
                .chain(group.iter())
                .find(|d| !d.remediation_status.is_empty())
                .map_or("none".to_string(), |d| d.remediation_status.clone());
            let mut affected_devices: Vec<String> = std::iter::once(&root)
                .chain(group.iter())
                .map(|d| d.device_address.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            affected_devices.sort();

            IncidentJson {
                id,
                root,
                cascading: group,
                affected_devices,
                severity,
                started_at_ns,
                ended_at_ns,
                remediation_status,
            }
        })
        .collect();

    incidents.sort_by_key(|incident| std::cmp::Reverse(incident.started_at_ns));
    incidents
}

// ── Assignment rule endpoints ─────────────────────────────────────────────────

// ── Device detail types ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct DeviceDetailResponse {
    address: String,
    hostname: String,
    vendor: String,
    role: String,
    site: String,
    enabled: bool,
    collector_id: String,
    credential_alias: String,
    health: String,
    interfaces: Vec<InterfaceDetailJson>,
    bgp_neighbors: Vec<BgpJson>,
    lldp_neighbors: Vec<LldpNeighborJson>,
    recent_state_changes: Vec<StateChangeJson>,
    recent_detections: Vec<DetectionRow>,
    selected_paths: Vec<SelectedSubscriptionPath>,
    subscription_statuses: Vec<SubscriptionStatusJson>,
    resolution_audit: Vec<String>,
    created_at_ns: i64,
    updated_at_ns: i64,
    created_by: String,
    updated_by: String,
    last_operator_action: String,
}

#[derive(Serialize)]
struct DeviceRecommendationsResponse {
    report: synthesizer::SynthesizerReport,
}

#[derive(Serialize)]
struct YangModulesResponse {
    modules: Vec<crate::yang::YangModuleRecord>,
}

#[derive(Deserialize, Default)]
struct YangSearchParams {
    #[serde(default)]
    q: String,
}

#[derive(Serialize)]
struct YangSearchResponse {
    result: crate::yang::YangSearchResult,
}

#[derive(Deserialize)]
struct ApplySelectedPathsRequest {
    #[serde(default)]
    selected_paths: Vec<SelectedSubscriptionPath>,
}

#[derive(Serialize)]
struct ApplySelectedPathsResponse {
    success: bool,
    error: String,
    selected_paths: Vec<SelectedSubscriptionPath>,
}

#[derive(Serialize)]
struct InterfaceDetailJson {
    name: String,
    in_errors: i64,
    out_errors: i64,
    in_octets: i64,
    out_octets: i64,
    carrier_transitions: i64,
    updated_at_ns: i64,
}

#[derive(Serialize)]
struct LldpNeighborJson {
    local_if: String,
    system_name: String,
    port_id: String,
    chassis_id: String,
}

#[derive(Serialize)]
struct StateChangeJson {
    event_type: String,
    detail: String,
    occurred_at_ns: i64,
}

// ── Assignment types ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct AssignmentRulesResponse {
    rules: Vec<AssignmentRule>,
}

#[derive(Deserialize)]
struct SetAssignmentRulesRequest {
    rules: Vec<AssignmentRule>,
}

#[derive(Serialize)]
struct CollectorStatusJson {
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
struct AssignmentStatusResponse {
    collectors: Vec<CollectorStatusJson>,
    unassigned_count: usize,
    unassigned_devices: Vec<String>,
}

#[derive(Deserialize)]
struct AssignmentOverrideRequest {
    device_address: String,
    collector_id: Option<String>,
}

#[derive(Serialize)]
struct AssignmentOverrideResponse {
    success: bool,
    error: String,
}

#[derive(Serialize)]
struct CollectorsResponse {
    collectors: Vec<CollectorStatusJson>,
    unassigned_count: usize,
    unassigned_devices: Vec<String>,
}

async fn assignment_rules_handler(
    State(state): State<AppState>,
) -> Result<Json<AssignmentRulesResponse>, (StatusCode, String)> {
    let rules = state
        .collector_manager
        .as_ref()
        .map(|m| m.get_rules())
        .unwrap_or_default();
    Ok(Json(AssignmentRulesResponse { rules }))
}

async fn collectors_handler(
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

async fn set_assignment_rules_handler(
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

async fn assignment_status_handler(
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

fn collector_status_json(s: CollectorStatus) -> CollectorStatusJson {
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

fn collector_status_with_subscription_json(
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

async fn assignment_override_handler(
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

// ── Environment handlers ──────────────────────────────────────────────────────

async fn environments_handler(
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

async fn create_environment_handler(
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

async fn update_environment_handler(
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

async fn remove_environment_handler(
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

async fn assign_site_environment_handler(
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
async fn setup_status_handler(
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

