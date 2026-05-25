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
    pub(super) name: String,
    pub(super) in_errors: i64,
    pub(super) out_errors: i64,
    pub(super) in_octets: i64,
    pub(super) out_octets: i64,
    pub(super) carrier_transitions: i64,
    pub(super) updated_at_ns: i64,
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
                    source_types: vec![],
                    latency_ns: 0,
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

// ── Enrichment conflicts endpoint ─────────────────────────────────────────────

#[derive(Serialize)]
pub(super) struct EnrichmentConflictJson {
    key: String,
    sources: Vec<ConflictSourceJson>,
}

#[derive(Serialize)]
pub(super) struct ConflictSourceJson {
    source_name: String,
    value: String,
    confidence: String,
    updated_at_ns: i64,
    is_winner: bool,
}

#[derive(Serialize)]
pub(super) struct DeviceConflictsResponse {
    address: String,
    conflicts: Vec<EnrichmentConflictJson>,
}

pub(super) async fn device_enrichment_conflicts_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceConflictsResponse>, (StatusCode, String)> {
    let db = state.store.db();
    let addr_clone = address.clone();

    let conflicts = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        // Find all enrichment properties for this device, grouped by key,
        // where the same key appears from multiple sources.
        let mut stmt = conn
            .prepare(
                "MATCH (d:Device {address: $addr})-[:HAS_ENRICHMENT_PROPERTY]->(p:EnrichmentProperty) \
                 OPTIONAL MATCH (p)-[:ENRICHMENT_PROPERTY_PROVENANCE]->(prov:PropertyProvenance) \
                 RETURN p.key, p.value, p.source_name, p.updated_at, \
                        prov.confidence, prov.details_json \
                 ORDER BY p.key, p.source_name",
            )
            .map_err(|e| e.to_string())?;
        let rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(addr_clone))])
            .map_err(|e| e.to_string())?;

        // Group by key
        let mut by_key: std::collections::HashMap<String, Vec<ConflictSourceJson>> =
            std::collections::HashMap::new();
        for row in rows {
            let key = read_str(&row[0]);
            let value = read_str(&row[1]);
            let source_name = read_str(&row[2]);
            let updated_at_ns = read_ts_ns(&row[3]);
            let confidence = read_str(&row[4]);
            let details = read_str(&row[5]);
            let is_winner = if details.contains("\"conflict\":true") {
                details.contains(&format!("\"winner\":\"{source_name}\""))
            } else {
                true // no conflict = default winner
            };
            by_key.entry(key).or_default().push(ConflictSourceJson {
                source_name,
                value,
                confidence,
                updated_at_ns,
                is_winner,
            });
        }

        // Only return keys where multiple sources exist
        let conflicts: Vec<EnrichmentConflictJson> = by_key
            .into_iter()
            .filter(|(_, sources)| {
                let unique: std::collections::HashSet<&str> =
                    sources.iter().map(|s| s.source_name.as_str()).collect();
                unique.len() > 1
            })
            .map(|(key, sources)| EnrichmentConflictJson { key, sources })
            .collect();

        Ok::<_, String>(conflicts)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(DeviceConflictsResponse { address, conflicts }))
}

// ── CMDB hierarchy endpoint ───────────────────────────────────────────────────

#[derive(Serialize)]
pub(super) struct CmdbRelJson {
    direction: String, // "parent" or "child"
    peer_hostname: String,
    rel_type: String,
    source_name: String,
}

#[derive(Serialize)]
pub(super) struct CmdbServiceJson {
    app_id: String,
    app_name: String,
    rel_type: String, // "RUNS_SERVICE" or "CARRIES_APPLICATION"
}

#[derive(Serialize)]
pub(super) struct CmdbLocationJson {
    location_id: String,
    location_name: String,
    full_address: String,
    parent_name: String,
}

#[derive(Serialize)]
pub(super) struct DeviceCmdbResponse {
    address: String,
    ci_relationships: Vec<CmdbRelJson>,
    services: Vec<CmdbServiceJson>,
    location: Option<CmdbLocationJson>,
}

pub(super) async fn device_cmdb_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceCmdbResponse>, (StatusCode, String)> {
    let db = state.store.db();
    let addr_clone = address.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;

        // 1. CMDB parent/child relationships (Device→Device)
        let mut ci_rels = Vec::new();
        // Children
        let mut child_stmt = conn
            .prepare(
                "MATCH (d:Device {address: $addr})-[r:CMDB_PARENT_OF]->(c:Device) \
                 RETURN c.hostname, r.rel_type, r.source_name",
            )
            .map_err(|e| e.to_string())?;
        for row in conn
            .execute(&mut child_stmt, vec![("addr", Value::String(addr_clone.clone()))])
            .map_err(|e| e.to_string())?
        {
            ci_rels.push(CmdbRelJson {
                direction: "child".to_string(),
                peer_hostname: read_str(&row[0]),
                rel_type: read_str(&row[1]),
                source_name: read_str(&row[2]),
            });
        }
        // Parents
        let mut parent_stmt = conn
            .prepare(
                "MATCH (p:Device)-[r:CMDB_PARENT_OF]->(d:Device {address: $addr}) \
                 RETURN p.hostname, r.rel_type, r.source_name",
            )
            .map_err(|e| e.to_string())?;
        for row in conn
            .execute(&mut parent_stmt, vec![("addr", Value::String(addr_clone.clone()))])
            .map_err(|e| e.to_string())?
        {
            ci_rels.push(CmdbRelJson {
                direction: "parent".to_string(),
                peer_hostname: read_str(&row[0]),
                rel_type: read_str(&row[1]),
                source_name: read_str(&row[2]),
            });
        }

        // 2. Business services (RUNS_SERVICE / CARRIES_APPLICATION)
        let mut services = Vec::new();
        let mut svc_stmt = conn
            .prepare(
                "MATCH (d:Device {address: $addr})-[r:RUNS_SERVICE|CARRIES_APPLICATION]->(a:Application) \
                 RETURN a.id, a.name, type(r)",
            )
            .map_err(|e| e.to_string())?;
        for row in conn
            .execute(&mut svc_stmt, vec![("addr", Value::String(addr_clone.clone()))])
            .map_err(|e| e.to_string())?
        {
            services.push(CmdbServiceJson {
                app_id: read_str(&row[0]),
                app_name: read_str(&row[1]),
                rel_type: read_str(&row[2]),
            });
        }

        // 3. Location (via IN_LOCATION or snow_location enrichment property)
        let mut location = None;
        let mut loc_stmt = conn
            .prepare(
                "MATCH (d:Device {address: $addr})-[:IN_LOCATION]->(l:Location) \
                 OPTIONAL MATCH (p:Location)-[:LOC_PARENT_OF]->(l) \
                 RETURN l.id, l.name, l.full_address, p.name",
            )
            .map_err(|e| e.to_string())?;
        for row in conn
            .execute(&mut loc_stmt, vec![("addr", Value::String(addr_clone))])
            .map_err(|e| e.to_string())?
        {
            location = Some(CmdbLocationJson {
                location_id: read_str(&row[0]),
                location_name: read_str(&row[1]),
                full_address: read_str(&row[2]),
                parent_name: read_str(&row[3]),
            });
            break; // take first
        }

        Ok::<_, String>(DeviceCmdbResponse {
            address: String::new(), // filled below
            ci_relationships: ci_rels,
            services,
            location,
        })
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(DeviceCmdbResponse {
        address,
        ..result
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
            password: resolved.as_ref().map(|creds| creds.password.to_string()),
            username_env: None,
            password_env: None,
            ca_cert_path: target.ca_cert.clone(),
            tls_domain: target.tls_domain.clone(),
            role_hint: target.role.clone(),
            environment_archetype: None,
            vault: Some(std::sync::Arc::clone(&state.credentials)),
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
                    password: resolved.as_ref().map(|creds| creds.password.to_string()),
                    username_env: None,
                    password_env: None,
                    ca_cert_path: target.ca_cert.clone(),
                    tls_domain: target.tls_domain.clone(),
                    role_hint: target.role.clone(),
                    environment_archetype: None,
                    vault: Some(std::sync::Arc::clone(&state.credentials)),
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
        password: resolved.as_ref().map(|creds| creds.password.to_string()),
        username_env: None,
        password_env: None,
        ca_cert_path: target.ca_cert.clone(),
        tls_domain: target.tls_domain.clone(),
        role_hint: target.role.clone(),
        environment_archetype: None,
        vault: Some(std::sync::Arc::clone(&state.credentials)),
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
            (Some(username), Some(password)) => Some(ResolvedCredential { username, password: zeroize::Zeroizing::new(password) }),
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
// ── NetBox import endpoint (D3-2 T2 / D3-4 T2) ───────────────────────────────

#[derive(Debug, serde::Deserialize)]
pub(super) struct NetboxImportRequest {
    pub url: String,
    pub token: String,
    /// Optional site slug to filter devices (empty = all active devices)
    #[serde(default)]
    pub site_slug: String,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct NetboxImportCandidate {
    pub name: String,
    pub address: String,
    pub site: String,
    pub role: String,
    pub vendor: String,
    pub platform: String,
    pub status: String,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct NetboxImportResponse {
    pub candidates: Vec<NetboxImportCandidate>,
    pub netbox_version: String,
    pub warnings: Vec<String>,
}

pub(super) async fn netbox_import_handler(
    Json(req): Json<NetboxImportRequest>,
) -> Result<Json<NetboxImportResponse>, (axum::http::StatusCode, String)> {
    let base = req.url.trim_end_matches('/').to_string();
    let token = req.token.trim().to_string();
    if base.is_empty() || token.is_empty() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "url and token are required".to_string(),
        ));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Auto-detect NetBox version
    let version_url = format!("{base}/api/");
    let nb_version = match client
        .get(&version_url)
        .header("Authorization", format!("Token {token}"))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            resp.json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| {
                    v.get("netbox-version")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "unknown".to_string())
        }
        Ok(resp) => {
            return Err((
                axum::http::StatusCode::BAD_GATEWAY,
                format!("NetBox /api/ returned {}", resp.status()),
            ));
        }
        Err(e) => {
            return Err((
                axum::http::StatusCode::BAD_GATEWAY,
                format!("Cannot reach NetBox at {base}: {e}"),
            ));
        }
    };

    // Fetch active devices — same endpoint for both 3.x and 4.x
    let mut devices_url = format!("{base}/api/dcim/devices/?status=active&limit=200");
    if !req.site_slug.trim().is_empty() {
        devices_url.push_str(&format!("&site={}", req.site_slug.trim()));
    }

    let mut warnings: Vec<String> = Vec::new();
    let mut candidates: Vec<NetboxImportCandidate> = Vec::new();
    let mut offset: usize = 0;

    loop {
        let page_url = format!("{devices_url}&offset={offset}");
        let resp = client
            .get(&page_url)
            .header("Authorization", format!("Token {token}"))
            .send()
            .await
            .map_err(|e| (axum::http::StatusCode::BAD_GATEWAY, e.to_string()))?;

        if !resp.status().is_success() {
            return Err((
                axum::http::StatusCode::BAD_GATEWAY,
                format!("NetBox dcim/devices returned {}", resp.status()),
            ));
        }

        #[derive(serde::Deserialize)]
        struct NbPage {
            results: Vec<NbDeviceImport>,
        }
        #[derive(serde::Deserialize)]
        struct NbDeviceImport {
            name: Option<String>,
            primary_ip: Option<NbIpImport>,
            site: Option<NbNestedImport>,
            role: Option<NbNestedImport>,
            device_type: Option<NbDeviceTypeImport>,
            platform: Option<NbNestedImport>,
            status: Option<NbStatusImport>,
        }
        #[derive(serde::Deserialize)]
        struct NbIpImport {
            address: String,
        }
        #[derive(serde::Deserialize)]
        struct NbNestedImport {
            name: String,
            #[serde(default)]
            slug: String,
        }
        #[derive(serde::Deserialize)]
        struct NbDeviceTypeImport {
            #[serde(default)]
            #[allow(dead_code)]
            model: String,
            manufacturer: Option<NbNestedImport>,
        }
        #[derive(serde::Deserialize)]
        struct NbStatusImport {
            value: String,
        }

        let page: NbPage = resp
            .json()
            .await
            .map_err(|e| (axum::http::StatusCode::BAD_GATEWAY, format!("parse error: {e}")))?;

        let fetched = page.results.len();

        for dev in page.results {
            let name = dev.name.unwrap_or_default();
            // Strip the prefix length from primary IP (e.g. "10.0.0.1/32" → "10.0.0.1")
            let raw_ip = dev
                .primary_ip
                .as_ref()
                .map(|ip| ip.address.split('/').next().unwrap_or("").to_string())
                .unwrap_or_default();

            if raw_ip.is_empty() {
                warnings.push(format!("device '{name}' has no primary IP — skipped"));
                continue;
            }

            let site = dev
                .site
                .as_ref()
                .map(|s| s.name.clone())
                .unwrap_or_default();
            let role = dev
                .role
                .as_ref()
                .map(|r| r.slug.clone())
                .unwrap_or_default();
            let vendor = dev
                .device_type
                .as_ref()
                .and_then(|dt| dt.manufacturer.as_ref())
                .map(|m| m.name.clone())
                .unwrap_or_default();
            let platform = dev
                .platform
                .as_ref()
                .map(|p| p.slug.clone())
                .unwrap_or_default();
            let status = dev
                .status
                .as_ref()
                .map(|s| s.value.clone())
                .unwrap_or_else(|| "active".to_string());

            candidates.push(NetboxImportCandidate {
                name,
                address: raw_ip,
                site,
                role,
                vendor,
                platform,
                status,
            });
        }

        if fetched < 200 {
            break;
        }
        offset += 200;
    }

    Ok(Json(NetboxImportResponse {
        candidates,
        netbox_version: nb_version,
        warnings,
    }))
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

#[derive(Serialize)]
pub(super) struct SensorReadingJson {
    pub id: String,
    pub component_name: String,
    pub sensor_type: String,
    pub temperature_c: Option<f64>,
    pub power_w: Option<f64>,
    pub fan_rpm: Option<i64>,
    pub humidity_pct: Option<f64>,
    pub updated_at: Option<i64>,
}

pub(super) async fn device_sensors_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<Vec<SensorReadingJson>>, (StatusCode, String)> {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "MATCH (s:SensorReading)-[:SENSOR_REPORTED_BY]->(d:Device {address: $addr}) \
                 RETURN s.id, s.component_name, s.sensor_type, s.temperature_c, \
                        s.power_w, s.fan_rpm, s.humidity_pct, s.updated_at \
                 ORDER BY s.component_name",
            )
            .map_err(|e| e.to_string())?;
        let rows_iter = conn
            .execute(&mut stmt, vec![("addr", Value::String(address))])
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for vals in rows_iter {
            out.push(SensorReadingJson {
                id: vals.get(0).and_then(|v| if let Value::String(s) = v { Some(s.clone()) } else { None }).unwrap_or_default(),
                component_name: vals.get(1).and_then(|v| if let Value::String(s) = v { Some(s.clone()) } else { None }).unwrap_or_default(),
                sensor_type: vals.get(2).and_then(|v| if let Value::String(s) = v { Some(s.clone()) } else { None }).unwrap_or_default(),
                temperature_c: vals.get(3).and_then(|v| if let Value::Double(f) = v { Some(*f) } else { None }),
                power_w: vals.get(4).and_then(|v| if let Value::Double(f) = v { Some(*f) } else { None }),
                fan_rpm: vals.get(5).and_then(|v| if let Value::Int64(i) = v { Some(*i) } else { None }),
                humidity_pct: vals.get(6).and_then(|v| if let Value::Double(f) = v { Some(*f) } else { None }),
                updated_at: vals.get(7).and_then(|v| if let Value::Int64(i) = v { Some(*i) } else { None }),
            });
        }
        Ok::<_, String>(out)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(result))
}

#[derive(Serialize)]
pub(super) struct OpticsTelemetryJson {
    pub id: String,
    pub if_name: String,
    pub rx_power_dbm: Option<f64>,
    pub tx_power_dbm: Option<f64>,
    pub laser_bias_ma: Option<f64>,
    pub temperature_c: Option<f64>,
    pub voltage_v: Option<f64>,
    pub updated_at: Option<i64>,
}

pub(super) async fn device_optics_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<Vec<OpticsTelemetryJson>>, (StatusCode, String)> {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "MATCH (o:OpticsTelemetry {device_address: $addr})-[:OPTICS_ON]->(i:Interface) \
                 RETURN o.id, i.name, o.rx_power_dbm, o.tx_power_dbm, \
                        o.laser_bias_ma, o.temperature_c, o.voltage_v, o.updated_at \
                 ORDER BY i.name",
            )
            .map_err(|e| e.to_string())?;
        let rows_iter = conn
            .execute(&mut stmt, vec![("addr", Value::String(address))])
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for vals in rows_iter {
            out.push(OpticsTelemetryJson {
                id: vals.get(0).and_then(|v| if let Value::String(s) = v { Some(s.clone()) } else { None }).unwrap_or_default(),
                if_name: vals.get(1).and_then(|v| if let Value::String(s) = v { Some(s.clone()) } else { None }).unwrap_or_default(),
                rx_power_dbm: vals.get(2).and_then(|v| if let Value::Double(f) = v { Some(*f) } else { None }),
                tx_power_dbm: vals.get(3).and_then(|v| if let Value::Double(f) = v { Some(*f) } else { None }),
                laser_bias_ma: vals.get(4).and_then(|v| if let Value::Double(f) = v { Some(*f) } else { None }),
                temperature_c: vals.get(5).and_then(|v| if let Value::Double(f) = v { Some(*f) } else { None }),
                voltage_v: vals.get(6).and_then(|v| if let Value::Double(f) = v { Some(*f) } else { None }),
                updated_at: vals.get(7).and_then(|v| if let Value::Int64(i) = v { Some(*i) } else { None }),
            });
        }
        Ok::<_, String>(out)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(result))
}
