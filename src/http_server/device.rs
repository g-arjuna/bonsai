#[derive(Serialize)]
pub(super) struct EnrichmentPropertyJson {
    key: String,
    value: String,
    source_name: String,
    updated_at_ns: i64,
    confidence: String,
    parser: String,
}
#[derive(Serialize)]
pub(super) struct DeviceEnrichmentResponse {
    address: String,
    /// Properties grouped by source_name for display.
    properties: Vec<EnrichmentPropertyJson>,
}
#[derive(Serialize)]
pub(super) struct DeviceConfigHistoryResponse {
    address: String,
    snapshots: Vec<change_detection::ConfigSnapshotSummary>,
    changes: Vec<change_detection::ConfigChangeSummary>,
}
#[derive(Serialize)]
pub(super) struct DeviceGnmiReadinessResponse {
    address: String,
    report: discovery::GnmiReadinessReport,
}
#[derive(Serialize)]
pub(super) struct DeviceStreamingReadinessResponse {
    address: String,
    report: StreamingReadinessReport,
}
#[derive(Deserialize)]
pub(super) struct DeviceReparseRequest {
    #[serde(default)]
    reason: String,
}
#[derive(Serialize)]
pub(super) struct DeviceReparseResponse {
    success: bool,
    message: String,
}
#[derive(Serialize)]
pub(super) struct DeviceDetailResponse {
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
pub(super) struct DeviceRecommendationsResponse {
    report: synthesizer::SynthesizerReport,
}
#[derive(Serialize)]
pub(super) struct YangModulesResponse {
    modules: Vec<crate::yang::YangModuleRecord>,
}
#[derive(Deserialize, Default)]
pub(super) struct YangSearchParams {
    #[serde(default)]
    q: String,
}
#[derive(Serialize)]
pub(super) struct YangSearchResponse {
    result: crate::yang::YangSearchResult,
}
#[derive(Deserialize)]
pub(super) struct ApplySelectedPathsRequest {
    #[serde(default)]
    selected_paths: Vec<SelectedSubscriptionPath>,
}
#[derive(Serialize)]
pub(super) struct ApplySelectedPathsResponse {
    success: bool,
    error: String,
    selected_paths: Vec<SelectedSubscriptionPath>,
}
#[derive(Serialize)]
pub(super) struct InterfaceDetailJson {
    name: String,
    in_errors: i64,
    out_errors: i64,
    in_octets: i64,
    out_octets: i64,
    carrier_transitions: i64,
    updated_at_ns: i64,
}
#[derive(Serialize)]
pub(super) struct LldpNeighborJson {
    local_if: String,
    system_name: String,
    port_id: String,
    chassis_id: String,
}
#[derive(Serialize)]
pub(super) struct StateChangeJson {
    event_type: String,
    detail: String,
    occurred_at_ns: i64,
}
#[derive(Serialize)]
pub(super) struct ProfilesResponse {
    profiles: Vec<ProfileJson>,
    plugins: Vec<PluginJson>,
    load_errors: Vec<String>,
}
#[derive(Serialize)]
pub(super) struct ProfileJson {
    name: String,
    environment: Vec<String>,
    vendor_scope: Vec<String>,
    roles: Vec<String>,
    description: String,
    rationale: String,
    path_count: usize,
    source: String,
}
#[derive(Serialize)]
pub(super) struct PluginJson {
    name: String,
    version: String,
    author: String,
    profile_count: usize,
    conflicts: Vec<String>,
}
#[derive(Deserialize)]
pub(super) struct SaveCustomProfileRequest {
    name: String,
    description: String,
    rationale: String,
    environment: Vec<String>,
    vendor_scope: Vec<String>,
    roles: Vec<String>,
    paths: Vec<serde_json::Value>,
}
#[derive(Serialize)]
pub(super) struct SaveCustomProfileResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
#[derive(Serialize)]
pub(super) struct EnricherEntry {
    config: crate::enrichment::EnricherConfig,
    state: crate::enrichment::EnricherRunState,
}
#[derive(Serialize)]
pub(super) struct EnrichmentListResponse {
    enrichers: Vec<EnricherEntry>,
}
#[derive(Deserialize)]
pub(super) struct EnrichmentUpsertRequest {
    config: EnricherConfig,
}
#[derive(Serialize)]
pub(super) struct EnrichmentMutationResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
#[derive(Deserialize)]
pub(super) struct EnrichmentNameRequest {
    name: String,
}
#[derive(Serialize)]
pub(super) struct EnrichmentTestResponse {
    success: bool,
    message: String,
}
#[derive(Serialize)]
pub(super) struct EnrichmentRunResponse {
    success: bool,
    message: String,
}
#[derive(Serialize)]
pub(super) struct EnrichmentAuditResponse {
    entries: Vec<serde_json::Value>,
}
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use axum::{Json, extract::{Path, Query, State}, http::StatusCode};
use lbug::{Connection, Value};

use super::AppState;
use super::{
    read_str, read_i64, read_ts_ns,
    compute_health,
    BgpJson, read_subscription_statuses, SubscriptionStatusJson,
};
use crate::config::{SelectedSubscriptionPath, TargetConfig};
use crate::credentials::{CredentialVault, ResolvePurpose, ResolvedCredential};
use crate::discovery::{self, DiscoveryInput};
use crate::enrichment::EnricherConfig;
use crate::graph::GraphStore;
use crate::streaming::{self, StreamingReadinessReport};
use crate::yang::YangLibrary;
use crate::store::BonsaiStore;
use crate::graph::DetectionRow;
use crate::{change_detection::{self}, synthesizer};

pub(super) async fn device_detail_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceDetailResponse>, (StatusCode, String)> {
    let target = state
        .registry
        .get_device(&address)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("device '{address}' not found"),
            )
        })?;

    let db = state.store.db();
    let addr_clone = address.clone();

    let (ifaces, bgp, lldp, state_changes, detections) = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;

        let mut stmt = conn
            .prepare(
                "MATCH (i:Interface) WHERE i.device_address = $addr \
                     RETURN i.name, i.in_errors, i.out_errors, i.in_octets, i.out_octets, \
                            i.carrier_transitions, i.updated_at \
                     ORDER BY i.name",
            )
            .map_err(|e| e.to_string())?;
        let iface_rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(addr_clone.clone()))])
            .map_err(|e| e.to_string())?;
        let ifaces: Vec<InterfaceDetailJson> = iface_rows
            .map(|row| InterfaceDetailJson {
                name: read_str(&row[0]),
                in_errors: read_i64(&row[1]),
                out_errors: read_i64(&row[2]),
                in_octets: read_i64(&row[3]),
                out_octets: read_i64(&row[4]),
                carrier_transitions: read_i64(&row[5]),
                updated_at_ns: read_ts_ns(&row[6]),
            })
            .collect();

        let mut stmt = conn
            .prepare(
                "MATCH (n:BgpNeighbor) WHERE n.device_address = $addr \
                     RETURN n.peer_address, n.session_state, n.peer_as \
                     ORDER BY n.peer_address",
            )
            .map_err(|e| e.to_string())?;
        let bgp_rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(addr_clone.clone()))])
            .map_err(|e| e.to_string())?;
        let bgp: Vec<BgpJson> = bgp_rows
            .map(|row| BgpJson {
                peer: read_str(&row[0]),
                state: read_str(&row[1]),
                peer_as: read_i64(&row[2]),
            })
            .collect();

        let mut stmt = conn
            .prepare(
                "MATCH (n:LldpNeighbor) WHERE n.device_address = $addr \
                     RETURN n.local_if, n.system_name, n.port_id, n.chassis_id \
                     ORDER BY n.local_if",
            )
            .map_err(|e| e.to_string())?;
        let lldp_rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(addr_clone.clone()))])
            .map_err(|e| e.to_string())?;
        let lldp: Vec<LldpNeighborJson> = lldp_rows
            .map(|row| LldpNeighborJson {
                local_if: read_str(&row[0]),
                system_name: read_str(&row[1]),
                port_id: read_str(&row[2]),
                chassis_id: read_str(&row[3]),
            })
            .collect();

        let mut stmt = conn
            .prepare(
                "MATCH (e:StateChangeEvent) WHERE e.device_address = $addr \
                     RETURN e.event_type, e.detail, e.occurred_at \
                     ORDER BY e.occurred_at DESC LIMIT 20",
            )
            .map_err(|e| e.to_string())?;
        let sc_rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(addr_clone.clone()))])
            .map_err(|e| e.to_string())?;
        let state_changes: Vec<StateChangeJson> = sc_rows
            .map(|row| StateChangeJson {
                event_type: read_str(&row[0]),
                detail: read_str(&row[1]),
                occurred_at_ns: read_ts_ns(&row[2]),
            })
            .collect();

        let mut stmt = conn
            .prepare(
                "MATCH (e:DetectionEvent) WHERE e.device_address = $addr \
                     OPTIONAL MATCH (r:Remediation)-[:RESOLVES]->(e) \
                     RETURN e.id, e.device_address, e.rule_id, e.severity, \
                            e.features_json, e.fired_at, r.id, r.action, r.status \
                     ORDER BY e.fired_at DESC LIMIT 10",
            )
            .map_err(|e| e.to_string())?;
        let det_rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(addr_clone.clone()))])
            .map_err(|e| e.to_string())?;
        let mut detections: Vec<DetectionRow> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for row in det_rows {
            let id = read_str(&row[0]);
            if seen.insert(id.clone()) {
                detections.push(crate::graph::DetectionRow {
                    id,
                    device_address: read_str(&row[1]),
                    rule_id: read_str(&row[2]),
                    severity: read_str(&row[3]),
                    features_json: read_str(&row[4]),
                    fired_at_ns: read_ts_ns(&row[5]),
                    remediation_id: read_str(&row[6]),
                    remediation_action: read_str(&row[7]),
                    remediation_status: read_str(&row[8]),
                });
            }
        }

        Ok::<_, String>((ifaces, bgp, lldp, state_changes, detections))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let all_statuses = read_subscription_statuses(state.store.db()).await?;
    let subscription_statuses = all_statuses.get(&address).cloned().unwrap_or_default();
    let health = compute_health(&bgp);

    let overrides = state.registry.list_overrides().unwrap_or_default();
    let (_, audit) = crate::discovery::resolve_subscription_paths(&target, &overrides);
    Ok(Json(DeviceDetailResponse {
        address: address.clone(),
        hostname: target.hostname.unwrap_or_default(),
        vendor: target.vendor.unwrap_or_default(),
        role: target.role.unwrap_or_default(),
        site: target.site.unwrap_or_default(),
        enabled: target.enabled,
        collector_id: target.collector_id.unwrap_or_default(),
        credential_alias: target.credential_alias.unwrap_or_default(),
        health,
        interfaces: ifaces,
        bgp_neighbors: bgp,
        lldp_neighbors: lldp,
        recent_state_changes: state_changes,
        recent_detections: detections,
        selected_paths: target.selected_paths.clone(),
        subscription_statuses,
        resolution_audit: audit,
        created_at_ns: target.created_at_ns,
        updated_at_ns: target.updated_at_ns,
        created_by: target.created_by,
        updated_by: target.updated_by,
        last_operator_action: target.last_operator_action,
    }))
}
pub(super) async fn device_enrichment_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceEnrichmentResponse>, (StatusCode, String)> {
    let db = state.store.db();
    let addr_clone = address.clone();

    let props = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "MATCH (d:Device {address: $addr})-[:HAS_ENRICHMENT_PROPERTY]->(p:EnrichmentProperty) \
                 OPTIONAL MATCH (p)-[:ENRICHMENT_PROPERTY_PROVENANCE]->(prov:PropertyProvenance) \
                 RETURN p.key, p.value, p.source_name, p.updated_at, prov.confidence, prov.parser \
                 ORDER BY p.source_name, p.key",
            )
            .map_err(|e| e.to_string())?;
        let rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(addr_clone))])
            .map_err(|e| e.to_string())?;
        let props: Vec<EnrichmentPropertyJson> = rows
            .map(|row| EnrichmentPropertyJson {
                key: read_str(&row[0]),
                value: read_str(&row[1]),
                source_name: read_str(&row[2]),
                updated_at_ns: read_ts_ns(&row[3]),
                confidence: read_str(&row[4]),
                parser: read_str(&row[5]),
            })
            .collect();
        Ok::<_, String>(props)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(DeviceEnrichmentResponse {
        address,
        properties: props,
    }))
}
pub(super) async fn device_config_history_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceConfigHistoryResponse>, (StatusCode, String)> {
    let (snapshots, changes) = change_detection::config_history(
        Arc::clone(&state.store),
        address.clone(),
        state.change_detection.history_limit,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(DeviceConfigHistoryResponse {
        address,
        snapshots,
        changes,
    }))
}
pub(super) async fn device_gnmi_readiness_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceGnmiReadinessResponse>, (StatusCode, String)> {
    let target = state
        .registry
        .get_device(&address)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("device '{address}' not found"),
            )
        })?;
    let resolved = resolve_target_credentials_for_discovery(&target, &state.credentials)
        .map_err(|e| (StatusCode::FAILED_DEPENDENCY, e.to_string()))?;
    let report = discovery::gnmi_readiness_report(
        DiscoveryInput {
            address: target.address.clone(),
            username: resolved.as_ref().map(|creds| creds.username.clone()),
            password: resolved.as_ref().map(|creds| creds.password.clone()),
            username_env: None,
            password_env: None,
            ca_cert_path: target.ca_cert.clone(),
            tls_domain: target.tls_domain.clone(),
            role_hint: target.role.clone(),
            environment_archetype: None,
        },
        &state.layered_ingestion.gnmi_known_issues_path,
    )
    .await;
    persist_gnmi_readiness(Arc::clone(&state.store), &address, &report)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(DeviceGnmiReadinessResponse { address, report }))
}
pub(super) async fn device_streaming_readiness_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceStreamingReadinessResponse>, (StatusCode, String)> {
    let target = state
        .registry
        .get_device(&address)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("device '{address}' not found"),
            )
        })?;

    let resolved = resolve_target_credentials_for_discovery(&target, &state.credentials)
        .map_err(|e| (StatusCode::FAILED_DEPENDENCY, e.to_string()))?;
    let gnmi = if resolved.is_some() || target.ca_cert.is_some() {
        Some(
            discovery::gnmi_readiness_report(
                DiscoveryInput {
                    address: target.address.clone(),
                    username: resolved.as_ref().map(|creds| creds.username.clone()),
                    password: resolved.as_ref().map(|creds| creds.password.clone()),
                    username_env: None,
                    password_env: None,
                    ca_cert_path: target.ca_cert.clone(),
                    tls_domain: target.tls_domain.clone(),
                    role_hint: target.role.clone(),
                    environment_archetype: None,
                },
                &state.layered_ingestion.gnmi_known_issues_path,
            )
            .await,
        )
    } else {
        None
    };

    let report =
        streaming::build_streaming_readiness_report(&target, gnmi.as_ref(), &state.streaming);
    persist_streaming_readiness(Arc::clone(&state.store), &address, &report)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(DeviceStreamingReadinessResponse { address, report }))
}
pub(super) async fn device_recommendations_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceRecommendationsResponse>, (StatusCode, String)> {
    let target = state
        .registry
        .get_device(&address)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("device '{address}' not found"),
            )
        })?;

    let mut warnings = Vec::new();
    let resolved = match resolve_target_credentials_for_discovery(&target, &state.credentials) {
        Ok(resolved) => resolved,
        Err(error) => {
            warnings.push(format!(
                "could not resolve device credentials for live discovery: {error:#}"
            ));
            None
        }
    };

    let discovery_input = DiscoveryInput {
        address: target.address.clone(),
        username: resolved.as_ref().map(|creds| creds.username.clone()),
        password: resolved.as_ref().map(|creds| creds.password.clone()),
        username_env: None,
        password_env: None,
        ca_cert_path: target.ca_cert.clone(),
        tls_domain: target.tls_domain.clone(),
        role_hint: target.role.clone(),
        environment_archetype: None,
    };

    let discovery_report = match discovery::discover_device(discovery_input.clone()).await {
        Ok(report) => Some(report),
        Err(error) => {
            warnings.push(format!(
                "live capabilities discovery unavailable: {error:#}"
            ));
            None
        }
    };

    let readiness_report = if resolved.is_some() || target.ca_cert.is_some() {
        Some(
            discovery::gnmi_readiness_report(
                discovery_input,
                &state.layered_ingestion.gnmi_known_issues_path,
            )
            .await,
        )
    } else {
        None
    };
    let streaming_readiness = streaming::build_streaming_readiness_report(
        &target,
        readiness_report.as_ref(),
        &state.streaming,
    );

    let overrides = state.registry.list_overrides().unwrap_or_default();
    let yang_library_state = YangLibrary::open(
        &state.yang_library_root,
        &state.yang_cache_root,
        &state.yang_bundle_key_env,
    )
    .ok()
    .and_then(|library| library.load_state().ok());

    let report = synthesizer::synthesize_for_target(
        &target,
        discovery_report.as_ref(),
        readiness_report.as_ref(),
        Some(&streaming_readiness),
        warnings,
        &overrides,
        yang_library_state.as_ref(),
    );
    Ok(Json(DeviceRecommendationsResponse { report }))
}
pub(super) async fn yang_modules_handler(
    State(state): State<AppState>,
) -> Result<Json<YangModulesResponse>, (StatusCode, String)> {
    let library = YangLibrary::open(
        &state.yang_library_root,
        &state.yang_cache_root,
        &state.yang_bundle_key_env,
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let modules = library
        .list_modules()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(YangModulesResponse { modules }))
}
pub(super) async fn yang_search_handler(
    State(state): State<AppState>,
    Query(params): Query<YangSearchParams>,
) -> Result<Json<YangSearchResponse>, (StatusCode, String)> {
    let library = YangLibrary::open(
        &state.yang_library_root,
        &state.yang_cache_root,
        &state.yang_bundle_key_env,
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let result = library
        .search(&params.q)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(YangSearchResponse { result }))
}
pub(super) async fn apply_device_selected_paths_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Json(req): Json<ApplySelectedPathsRequest>,
) -> Result<Json<ApplySelectedPathsResponse>, (StatusCode, String)> {
    let mut target = state
        .registry
        .get_device(&address)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("device '{address}' not found"),
            )
        })?;

    target.selected_paths = req
        .selected_paths
        .into_iter()
        .filter(|path| !path.path.trim().is_empty())
        .collect();

    let updated = state
        .registry
        .update_device_with_audit(target, "api", "api_apply_recommendation_paths")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(ApplySelectedPathsResponse {
        success: true,
        error: String::new(),
        selected_paths: updated.selected_paths,
    }))
}
pub(super) async fn persist_gnmi_readiness(
    store: Arc<GraphStore>,
    address: &str,
    report: &discovery::GnmiReadinessReport,
) -> anyhow::Result<()> {
    let db = store.db();
    let write_lock = store.write_lock();
    let address = address.to_string();
    let report = report.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = write_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("graph write lock poisoned"))?;
        let conn = Connection::new(&db)?;
        let readiness_id = format!("{address}:gnmi-readiness");
        let checked_at =
            time::OffsetDateTime::from_unix_timestamp_nanos(report.checked_at_ns.into())?;
        let mut stmt = conn.prepare(
            "MERGE (r:GnmiReadiness {id: $id}) \
             SET r.device_address = $addr, r.service_status = $service_status, \
                 r.tls_status = $tls_status, r.encoding_support = $encoding_support, \
                 r.models_advertised = $models_advertised, r.known_issues = $known_issues, \
                 r.blockers = $blockers, r.recommended_actions = $recommended_actions, \
                 r.checked_at = $checked_at",
        )?;
        conn.execute(
            &mut stmt,
            vec![
                ("id", Value::String(readiness_id.clone())),
                ("addr", Value::String(address.clone())),
                ("service_status", Value::String(report.service_status)),
                ("tls_status", Value::String(report.tls_status)),
                (
                    "encoding_support",
                    Value::String(serde_json::to_string(&report.encoding_support)?),
                ),
                (
                    "models_advertised",
                    Value::String(serde_json::to_string(&report.models_advertised)?),
                ),
                (
                    "known_issues",
                    Value::String(serde_json::to_string(&report.known_issues)?),
                ),
                (
                    "blockers",
                    Value::String(serde_json::to_string(&report.blockers)?),
                ),
                (
                    "recommended_actions",
                    Value::String(serde_json::to_string(&report.recommended_actions)?),
                ),
                ("checked_at", Value::TimestampNs(checked_at)),
            ],
        )?;

        let mut rel_stmt = conn.prepare(
            "MATCH (d:Device {address: $addr}), (r:GnmiReadiness {id: $id}) \
             MERGE (d)-[:HAS_GNMI_READINESS]->(r)",
        )?;
        conn.execute(
            &mut rel_stmt,
            vec![
                ("addr", Value::String(address)),
                ("id", Value::String(readiness_id)),
            ],
        )?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("gNMI readiness persistence task panicked: {e}"))?
}
pub(super) async fn persist_streaming_readiness(
    store: Arc<GraphStore>,
    address: &str,
    report: &StreamingReadinessReport,
) -> anyhow::Result<()> {
    let db = store.db();
    let write_lock = store.write_lock();
    let address = address.to_string();
    let report = report.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = write_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("graph write lock poisoned"))?;
        let conn = Connection::new(&db)?;
        let readiness_id = format!("{address}:streaming-readiness");
        let checked_at =
            time::OffsetDateTime::from_unix_timestamp_nanos(report.checked_at_ns.into())?;
        let mut stmt = conn.prepare(
            "MERGE (r:StreamingReadiness {id: $id}) \
             SET r.device_address = $addr, r.vendor = $vendor, r.role = $role, \
                 r.protocols_json = $protocols_json, r.recommended_protocols_json = $recommended_protocols_json, \
                 r.checked_at = $checked_at",
        )?;
        conn.execute(
            &mut stmt,
            vec![
                ("id", Value::String(readiness_id.clone())),
                ("addr", Value::String(address.clone())),
                ("vendor", Value::String(report.vendor)),
                ("role", Value::String(report.role)),
                (
                    "protocols_json",
                    Value::String(serde_json::to_string(&report.protocols)?),
                ),
                (
                    "recommended_protocols_json",
                    Value::String(serde_json::to_string(&report.recommended_protocols)?),
                ),
                ("checked_at", Value::TimestampNs(checked_at)),
            ],
        )?;
        let mut rel_stmt = conn.prepare(
            "MATCH (d:Device {address: $addr}), (r:StreamingReadiness {id: $id}) \
             MERGE (d)-[:HAS_STREAMING_READINESS]->(r)",
        )?;
        conn.execute(
            &mut rel_stmt,
            vec![
                ("addr", Value::String(address)),
                ("id", Value::String(readiness_id)),
            ],
        )?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("streaming readiness persistence task panicked: {e}"))?
}
pub(super) async fn device_reparse_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Json(req): Json<DeviceReparseRequest>,
) -> Json<DeviceReparseResponse> {
    let reason = if req.reason.trim().is_empty() {
        "operator-triggered re-parse".to_string()
    } else {
        req.reason
    };
    match state
        .change_detection
        .enqueue_manual(&address, &reason)
        .await
    {
        Ok(()) => Json(DeviceReparseResponse {
            success: true,
            message: format!("re-parse queued for {address}"),
        }),
        Err(error) => Json(DeviceReparseResponse {
            success: false,
            message: error.to_string(),
        }),
    }
}
pub(super) fn resolve_target_credentials_for_discovery(
    target: &TargetConfig,
    credentials: &CredentialVault,
) -> anyhow::Result<Option<ResolvedCredential>> {
    if let Some(alias) = target.credential_alias.as_deref() {
        return credentials
            .resolve(alias, ResolvePurpose::Discover)
            .map(Some);
    }
    Ok(
        match (target.resolved_username(), target.resolved_password()) {
            (Some(username), Some(password)) => Some(ResolvedCredential { username, password }),
            _ => None,
        },
    )
}
pub(super) async fn profiles_handler(State(state): State<AppState>) -> Json<ProfilesResponse> {
    let cat = state.catalogue.read().await;

    let profiles: Vec<ProfileJson> = cat
        .profiles
        .iter()
        .map(|p| ProfileJson {
            name: p.name.clone(),
            environment: p.environment.clone(),
            vendor_scope: p.vendor_scope.clone(),
            roles: p.roles.clone(),
            description: p.description.clone(),
            rationale: p.rationale.clone(),
            path_count: p.paths.len(),
            source: "built-in".to_string(),
        })
        .chain(cat.plugins.iter().flat_map(|plugin| {
            plugin.profiles.iter().map(move |p| ProfileJson {
                name: p.name.clone(),
                environment: p.environment.clone(),
                vendor_scope: p.vendor_scope.clone(),
                roles: p.roles.clone(),
                description: p.description.clone(),
                rationale: p.rationale.clone(),
                path_count: p.paths.len(),
                source: format!("plugin:{}", plugin.manifest.name),
            })
        }))
        .collect();

    let plugins: Vec<PluginJson> = cat
        .plugins
        .iter()
        .map(|p| PluginJson {
            name: p.manifest.name.clone(),
            version: p.manifest.version.clone(),
            author: p.manifest.author.clone(),
            profile_count: p.profiles.len(),
            conflicts: p.conflicts.clone(),
        })
        .collect();

    Json(ProfilesResponse {
        profiles,
        plugins,
        load_errors: cat.load_errors.clone(),
    })
}
pub(super) async fn save_custom_profile_handler(
    State(state): State<AppState>,
    Json(req): Json<SaveCustomProfileRequest>,
) -> Json<SaveCustomProfileResponse> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Json(SaveCustomProfileResponse {
            success: false,
            error: Some("profile name is required".to_string()),
        });
    }
    // Sanitise: only alphanumeric, underscore, hyphen
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Json(SaveCustomProfileResponse {
            success: false,
            error: Some(
                "profile name may only contain letters, digits, underscores, and hyphens"
                    .to_string(),
            ),
        });
    }

    let user_plugin_dir = std::path::Path::new(&state.catalogue_dir)
        .join("plugins")
        .join("user");

    if let Err(e) = std::fs::create_dir_all(&user_plugin_dir) {
        return Json(SaveCustomProfileResponse {
            success: false,
            error: Some(format!("cannot create user plugin dir: {e}")),
        });
    }

    // Build the profile YAML document
    let profile_doc = serde_json::json!({
        "name": name,
        "environment": req.environment,
        "vendor_scope": req.vendor_scope,
        "roles": req.roles,
        "description": req.description,
        "rationale": req.rationale,
        "paths": req.paths,
    });
    let yaml_str = match serde_yaml::to_string(&profile_doc) {
        Ok(s) => s,
        Err(e) => {
            return Json(SaveCustomProfileResponse {
                success: false,
                error: Some(format!("yaml serialisation error: {e}")),
            });
        }
    };

    let profile_filename = format!("{name}.yaml");
    let profile_path = user_plugin_dir.join(&profile_filename);
    if let Err(e) = std::fs::write(&profile_path, yaml_str) {
        return Json(SaveCustomProfileResponse {
            success: false,
            error: Some(format!("cannot write profile file: {e}")),
        });
    }

    // Rebuild the MANIFEST.yaml from all YAMLs in the user plugin dir
    let mut profile_files: Vec<String> = std::fs::read_dir(&user_plugin_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("yaml")
                && p.file_name().and_then(|x| x.to_str()) != Some("MANIFEST.yaml")
            {
                p.file_name()
                    .and_then(|x| x.to_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();
    profile_files.sort();

    let manifest_doc = serde_json::json!({
        "name": "user",
        "version": "0.1.0",
        "author": "operator",
        "profiles": profile_files,
    });
    let manifest_str = match serde_yaml::to_string(&manifest_doc) {
        Ok(s) => s,
        Err(e) => {
            return Json(SaveCustomProfileResponse {
                success: false,
                error: Some(format!("manifest serialisation error: {e}")),
            });
        }
    };
    if let Err(e) = std::fs::write(user_plugin_dir.join("MANIFEST.yaml"), manifest_str) {
        return Json(SaveCustomProfileResponse {
            success: false,
            error: Some(format!("cannot write MANIFEST.yaml: {e}")),
        });
    }

    // Reload catalogue and swap in
    let new_catalogue =
        crate::catalogue::load_catalogue(std::path::Path::new(&state.catalogue_dir));
    *state.catalogue.write().await = new_catalogue;

    Json(SaveCustomProfileResponse {
        success: true,
        error: None,
    })
}
pub(super) async fn enrichment_list_handler(State(state): State<AppState>) -> Json<EnrichmentListResponse> {
    let reg = state.enricher_registry.read().await;
    let enrichers = reg
        .list()
        .into_iter()
        .map(|(config, st)| EnricherEntry { config, state: st })
        .collect();
    Json(EnrichmentListResponse { enrichers })
}
pub(super) async fn enrichment_upsert_handler(
    State(state): State<AppState>,
    Json(req): Json<EnrichmentUpsertRequest>,
) -> Json<EnrichmentMutationResponse> {
    state.enricher_registry.write().await.upsert(req.config);
    Json(EnrichmentMutationResponse {
        success: true,
        error: None,
    })
}
pub(super) async fn enrichment_remove_handler(
    State(state): State<AppState>,
    Json(req): Json<EnrichmentNameRequest>,
) -> Json<EnrichmentMutationResponse> {
    let removed = state.enricher_registry.write().await.remove(&req.name);
    if removed {
        Json(EnrichmentMutationResponse {
            success: true,
            error: None,
        })
    } else {
        Json(EnrichmentMutationResponse {
            success: false,
            error: Some(format!("enricher '{}' not found", req.name)),
        })
    }
}
pub(super) async fn enrichment_test_handler(
    State(state): State<AppState>,
    Json(req): Json<EnrichmentNameRequest>,
) -> Json<EnrichmentTestResponse> {
    let config = {
        let reg = state.enricher_registry.read().await;
        reg.get(&req.name).cloned()
    };
    let Some(config) = config else {
        return Json(EnrichmentTestResponse {
            success: false,
            message: format!("enricher '{}' not found", req.name),
        });
    };

    let audit = crate::enrichment::EnricherAuditLog::new(
        std::path::Path::new(&state.runtime_dir),
        &config.name,
    );

    match crate::enrichment::factory::build_enricher(&config) {
        Err(e) => Json(EnrichmentTestResponse {
            success: false,
            message: format!("cannot build enricher: {e:#}"),
        }),
        Ok(enricher) => match enricher.test_connection(&state.credentials, &audit).await {
            Ok(()) => Json(EnrichmentTestResponse {
                success: true,
                message: "connection successful".to_string(),
            }),
            Err(e) => Json(EnrichmentTestResponse {
                success: false,
                message: format!("{e:#}"),
            }),
        },
    }
}
pub(super) async fn enrichment_run_handler(
    State(state): State<AppState>,
    Json(req): Json<EnrichmentNameRequest>,
) -> Json<EnrichmentRunResponse> {
    let config = {
        let reg = state.enricher_registry.read().await;
        reg.get(&req.name).cloned()
    };
    let Some(config) = config else {
        return Json(EnrichmentRunResponse {
            success: false,
            message: format!("enricher '{}' not found", req.name),
        });
    };

    let enricher = match crate::enrichment::factory::build_enricher(&config) {
        Ok(e) => e,
        Err(e) => {
            return Json(EnrichmentRunResponse {
                success: false,
                message: format!("cannot build enricher: {e:#}"),
            });
        }
    };

    state
        .enricher_registry
        .write()
        .await
        .set_running(&req.name, true);

    let registry_clone = Arc::clone(&state.enricher_registry);
    let name = req.name.clone();
    let runtime_dir = state.runtime_dir.clone();
    let store = Arc::clone(&state.store);
    let creds = Arc::clone(&state.credentials);

    tokio::spawn(async move {
        let audit =
            crate::enrichment::EnricherAuditLog::new(std::path::Path::new(&runtime_dir), &name);
        let report = match enricher.enrich(store.as_ref(), &creds, &audit).await {
            Ok(r) => r,
            Err(e) => crate::enrichment::EnrichmentReport {
                enricher_name: name.clone(),
                error: Some(format!("{e:#}")),
                ..Default::default()
            },
        };
        registry_clone.write().await.record_run(&name, &report);
    });

    Json(EnrichmentRunResponse {
        success: true,
        message: format!("enricher '{}' run started", req.name),
    })
}
pub(super) async fn enrichment_audit_handler(State(state): State<AppState>) -> Json<EnrichmentAuditResponse> {
    // Read audit log files and return the last 100 enrichment_run entries.
    let audit_dir = std::path::Path::new(&state.runtime_dir).join("audit");
    let entries = read_recent_enrichment_audit(&audit_dir, 100);
    Json(EnrichmentAuditResponse { entries })
}
pub(super) fn read_recent_enrichment_audit(
    audit_dir: &std::path::Path,
    limit: usize,
) -> Vec<serde_json::Value> {
    let mut files: Vec<_> = std::fs::read_dir(audit_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                Some(p)
            } else {
                None
            }
        })
        .collect();
    files.sort();

    let mut entries: Vec<serde_json::Value> = Vec::new();
    for file in files.iter().rev() {
        if let Ok(content) = std::fs::read_to_string(file) {
            for line in content.lines().rev() {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
                    && val.get("event").and_then(|v| v.as_str()) == Some("enrichment_run")
                {
                    entries.push(val);
                    if entries.len() >= limit {
                        return entries;
                    }
                }
            }
        }
    }
    entries
}
