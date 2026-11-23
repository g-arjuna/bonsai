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
    /// Acquisition intent ("automatic" | "explicit" | "disabled") and the
    /// durable schema-bound approval summary — the workspace needs both
    /// to distinguish desired vs approved vs observed collection.
    acquisition: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema_binding: Option<crate::config::SchemaBindingConfig>,
    inventory_only: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    external_id: String,
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
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use lbug::{Connection, Value};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::AppState;
use super::{
    BgpJson, SubscriptionStatusJson, compute_health, read_i64, read_str,
    read_subscription_statuses, read_ts_ns,
};
use crate::config::{SelectedSubscriptionPath, TargetConfig};
use crate::credentials::{CredentialVault, ResolvePurpose, ResolvedCredential};
use crate::discovery::{self, DiscoveryInput};
use crate::enrichment::EnricherConfig;
use crate::graph::DetectionRow;
use crate::graph::GraphStore;
use crate::streaming::{self, StreamingReadinessReport};
use crate::yang::YangLibrary;
use crate::{
    change_detection::{self},
    synthesizer,
};

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
    // Graph Device nodes are keyed by bare IP — strip any gNMI port from the path param.
    let addr_clone = crate::registry::strip_port(&address).to_string();

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
                     RETURN n.peer_address, n.session_state, n.peer_as, n.source \
                     ORDER BY n.peer_address",
            )
            .map_err(|e| e.to_string())?;
        let bgp_rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(addr_clone.clone()))])
            .map_err(|e| e.to_string())?;
        // BN-016-U1: same deterministic resolution contract as topology —
        // peer_device is populated only for a unique resolution.
        let bgp: Vec<BgpJson> = bgp_rows
            .map(|row| {
                let peer = read_str(&row[0]);
                let resolution = crate::graph::common::resolve_peer_typed(&conn, &peer);
                BgpJson {
                    peer,
                    state: read_str(&row[1]),
                    peer_as: read_i64(&row[2]),
                    peer_device: resolution.device().unwrap_or_default().to_string(),
                    source: read_str(&row[3]),
                    resolution: resolution.status().to_string(),
                    reason: resolution.reason(),
                }
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
        acquisition: target.acquisition.as_str().to_string(),
        schema_binding: target.schema_binding.clone(),
        inventory_only: target.inventory_only,
        external_id: target.external_id.clone().unwrap_or_default(),
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
    let addr_clone = crate::registry::strip_port(&address).to_string();

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
    let addr_clone = crate::registry::strip_port(&address).to_string();

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
    let addr_clone = crate::registry::strip_port(&address).to_string();

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
        if let Some(row) = conn
            .execute(&mut loc_stmt, vec![("addr", Value::String(addr_clone))])
            .map_err(|e| e.to_string())?
            .next()
        {
            location = Some(CmdbLocationJson {
                location_id: read_str(&row[0]),
                location_name: read_str(&row[1]),
                full_address: read_str(&row[2]),
                parent_name: read_str(&row[3]),
            });
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

    Ok(Json(DeviceCmdbResponse { address, ..result }))
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
            security: target.security,
            client_cert_path: target.client_cert.clone(),
            client_key_path: target.client_key.clone(),
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
                    security: target.security,
                    client_cert_path: target.client_cert.clone(),
                    client_key_path: target.client_key.clone(),
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
        security: target.security,
        client_cert_path: target.client_cert.clone(),
        client_key_path: target.client_key.clone(),
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

#[derive(Serialize)]
pub(super) struct YangSourcesResponse {
    manifest_path: String,
    sources: Vec<crate::yang_source::YangSourceEntry>,
}

pub(super) async fn yang_sources_handler(
    State(_state): State<AppState>,
) -> Result<Json<YangSourcesResponse>, (StatusCode, String)> {
    let manifest = crate::yang_source::load_default_manifest()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(YangSourcesResponse {
        manifest_path: crate::yang_source::DEFAULT_MANIFEST_PATH.to_string(),
        sources: manifest.source,
    }))
}

#[derive(Serialize)]
pub(super) struct YangCompiledResponse {
    bundles: Vec<crate::yang_schema::CompiledBundleInfo>,
}

fn yang_roots(state: &AppState) -> (String, String, String) {
    (
        state.yang_library_root.clone(),
        state.yang_cache_root.clone(),
        state.yang_bundle_key_env.clone(),
    )
}

pub(super) async fn yang_compiled_handler(
    State(state): State<AppState>,
) -> Result<Json<YangCompiledResponse>, (StatusCode, String)> {
    let (lib, cache, key) = yang_roots(&state);
    let bundles = tokio::task::spawn_blocking(move || {
        let library = YangLibrary::open(
            std::path::Path::new(&lib),
            std::path::Path::new(&cache),
            &key,
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        crate::yang_schema::list_compiled(&library)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok(Json(YangCompiledResponse { bundles }))
}

#[derive(Deserialize)]
pub(super) struct YangCompileParams {
    #[serde(default)]
    all_trust: bool,
    #[serde(default = "default_true_http")]
    activate: bool,
}
fn default_true_http() -> bool {
    true
}

/// Optional JSON body for `POST /api/yang/compile` (M6F-F PR3): an
/// operator-approved module-revision pin set and/or an explicit module
/// scope for device-specific schema closures compiled out of a shared
/// multi-version library. Absent body = unrestricted compile (the
/// historical behavior). Pins/scope are recorded in the request audit;
/// the resulting bundle is content-addressed exactly like any other.
#[derive(Deserialize)]
pub(super) struct YangCompileBody {
    #[serde(default)]
    module_revisions: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default)]
    module_scope: Option<Vec<String>>,
}

#[derive(Serialize)]
pub(super) struct YangCompileResponse {
    schema_id: String,
    status: String,
    module_count: usize,
    path_count: usize,
    diagnostics: Vec<String>,
    activated: bool,
}

pub(super) async fn yang_compile_handler(
    State(state): State<AppState>,
    Query(params): Query<YangCompileParams>,
    body: Option<Json<YangCompileBody>>,
) -> Result<Json<YangCompileResponse>, (StatusCode, String)> {
    let (lib, cache, key) = yang_roots(&state);
    let all_trust = params.all_trust;
    let activate = params.activate;
    let module_revisions = body
        .as_ref()
        .and_then(|b| b.module_revisions.clone())
        .unwrap_or_default();
    let module_scope: std::collections::BTreeSet<String> = body
        .as_ref()
        .and_then(|b| b.module_scope.clone())
        .unwrap_or_default()
        .into_iter()
        .collect();
    // M6F-F: bundles referenced by device selections or active approvals
    // are retention-protected — a compile must never evict them.
    // Unavailable reference state fails closed (retention skipped).
    let references = state
        .registry
        .sqlite_store()
        .and_then(|store| store.compiled_schema_references().ok());
    // Compilation is CPU/IO-bound filesystem work — keep it off the async
    // executor so admin compiles cannot stall live collection/control plane.
    let (catalogue, activated) = tokio::task::spawn_blocking(move || {
        let library = YangLibrary::open(
            std::path::Path::new(&lib),
            std::path::Path::new(&cache),
            &key,
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let catalogue = crate::yang_schema::compile_library_with_selection(
            &library,
            !all_trust,
            &module_revisions,
            &module_scope,
            references.as_ref(),
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        // Activation policy: only a `complete` catalogue may be activated
        // implicitly. Partial/failed catalogues remain browsable as
        // diagnostics but are never promoted to active authoritative state
        // without an explicit separate activation call.
        let mut activated = false;
        if activate && catalogue.status == "complete" {
            crate::yang_schema::activate_compiled(&library, &catalogue.schema_id)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            activated = true;
        }
        Ok::<_, (StatusCode, String)>((catalogue, activated))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    if activated {
        let revoked = crate::schema_binding::revoke_bindings_for_schema_change(
            state.registry.as_ref(),
            &catalogue.schema_id,
            "system",
            "yang_compile_activate",
        )
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
        for target in revoked {
            if let Some(cid) = target.collector_id.as_deref()
                && let Some(manager) = state.collector_manager.as_ref()
            {
                manager.resync_collector(cid).await;
            }
        }
    }
    Ok(Json(YangCompileResponse {
        schema_id: catalogue.schema_id,
        status: catalogue.status,
        module_count: catalogue.module_count,
        path_count: catalogue.path_count,
        diagnostics: catalogue.diagnostics,
        activated,
    }))
}

#[derive(Deserialize)]
pub(super) struct YangActivateRequest {
    schema_id: String,
}

#[derive(Serialize)]
pub(super) struct YangActivateResponse {
    active: String,
}

pub(super) async fn yang_activate_handler(
    State(state): State<AppState>,
    Json(req): Json<YangActivateRequest>,
) -> Result<Json<YangActivateResponse>, (StatusCode, String)> {
    let (lib, cache, key) = yang_roots(&state);
    let schema_id = req.schema_id.clone();
    tokio::task::spawn_blocking(move || {
        let library = YangLibrary::open(
            std::path::Path::new(&lib),
            std::path::Path::new(&cache),
            &key,
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        crate::yang_schema::activate_compiled(&library, &schema_id)
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    let revoked = crate::schema_binding::revoke_bindings_for_schema_change(
        state.registry.as_ref(),
        &req.schema_id,
        "system",
        "yang_activate",
    )
    .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
    for target in revoked {
        if let Some(cid) = target.collector_id.as_deref()
            && let Some(manager) = state.collector_manager.as_ref()
        {
            manager.resync_collector(cid).await;
        }
    }
    Ok(Json(YangActivateResponse {
        active: req.schema_id,
    }))
}

#[derive(Deserialize, Default)]
pub(super) struct YangPathsParams {
    #[serde(default)]
    q: String,
    #[serde(default)]
    module: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    config: Option<String>,
    #[serde(default)]
    feature: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Serialize)]
pub(super) struct YangPathsResponse {
    result: crate::yang_schema::SchemaPathSearchResult,
}

pub(super) async fn yang_paths_handler(
    State(state): State<AppState>,
    Query(params): Query<YangPathsParams>,
) -> Result<Json<YangPathsResponse>, (StatusCode, String)> {
    let (lib, cache, key) = yang_roots(&state);
    let config = match params.config.as_deref() {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    };
    let query = crate::yang_schema::SchemaPathQuery {
        query: params.q,
        module: params.module,
        kind: params.kind,
        config,
        required_feature: params.feature,
        limit: params.limit.unwrap_or(200),
    };
    let result = tokio::task::spawn_blocking(move || {
        let library = YangLibrary::open(
            std::path::Path::new(&lib),
            std::path::Path::new(&cache),
            &key,
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        crate::yang_schema::search_active_paths(&library, &query)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok(Json(YangPathsResponse { result }))
}

// ── M6C-1 device applicability ─────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct YangApplicabilityParams {
    /// Filter paths by status (applicable|candidate|not_applicable|
    /// unknown|stale|conflicted).
    status: Option<String>,
    /// Restrict to one module name.
    module: Option<String>,
    /// Restrict to one schema kind (container|list|leaf|...).
    kind: Option<String>,
    /// Substring search over canonical path, module, namespace and type.
    q: Option<String>,
    /// "true" = configuration data only, "false" = state data only.
    config: Option<String>,
    /// Restrict to paths declaring this required feature.
    feature: Option<String>,
    /// Page size for path results (default 500, max 5000).
    limit: Option<usize>,
    /// Page offset for path results.
    offset: Option<usize>,
}

#[derive(Serialize)]
pub(super) struct YangApplicabilityResponse {
    address: String,
    /// Persisted snapshot view with computed freshness; `null` when no
    /// refresh has ever run for this device.
    snapshot: Option<crate::yang_applicability::ApplicabilitySnapshotView>,
    /// Filtered/paginated path-level results.
    paths: Vec<crate::yang_schema::applicability::PathResult>,
    /// Total path results matching the filters (before pagination).
    paths_matching: usize,
    /// Module-level results (bounded by the resolver).
    modules: Vec<crate::yang_schema::applicability::ModuleResult>,
}

#[derive(Serialize)]
pub(super) struct YangApplicabilityRefreshResponse {
    snapshot_id: i64,
    device_key: String,
    overall_status: String,
    outcome: String,
    schema_id: Option<String>,
    content_id: Option<String>,
    datastore: Option<String>,
    evidence_fingerprint: String,
    paths_total: usize,
    modules_total: usize,
    evaluated_at_ns: i64,
    error: Option<String>,
    /// True when this refresh superseded an active schema-bound approval;
    /// the operator must re-preview/re-activate against the new evidence.
    schema_binding_revoked: bool,
}

/// GET — read the persisted applicability snapshot. Never connects to
/// the device; freshness is computed against the current active
/// catalogue and evidence age policy.
pub(super) async fn device_yang_applicability_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(params): Query<YangApplicabilityParams>,
) -> Result<Json<YangApplicabilityResponse>, (StatusCode, String)> {
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
    let store = state.registry.sqlite_store().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "applicability store unavailable".to_string(),
    ))?;
    let (lib, cache, key) = yang_roots(&state);
    let library = YangLibrary::open(
        std::path::Path::new(&lib),
        std::path::Path::new(&cache),
        &key,
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let now_ns = crate::graph::common::now_ns();
    let view = crate::yang_applicability::load_snapshot_view(&store, &library, &target, now_ns)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let (paths, paths_matching, modules, snapshot_view) = match &view {
        Some(v) => {
            // The embedded snapshot must honor the same response bound as
            // the top-level page: never serialize the full unpaginated
            // `result.paths` array inside `snapshot.result`.
            let mut snapshot = v.snapshot.clone();
            snapshot.result.paths.clear();
            snapshot.result.paths_total = v.snapshot.result.paths_total;
            snapshot.result.paths_truncated = v.snapshot.result.paths_truncated
                || v.snapshot.result.paths_total > v.snapshot.result.paths.len();
            let filtered_snapshot = crate::yang_applicability::ApplicabilitySnapshotView {
                snapshot,
                freshness: v.freshness,
                freshness_reasons: v.freshness_reasons.clone(),
                active_schema_id: v.active_schema_id.clone(),
                effective_schema_id: v.effective_schema_id.clone(),
                selected_schema_id: v.selected_schema_id.clone(),
                catalogue_state: v.catalogue_state,
            };
            let config_filter = match params.config.as_deref() {
                Some("true") => Some(true),
                Some("false") => Some(false),
                _ => None,
            };
            let q = params
                .q
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .to_lowercase();
            let feature = params.feature.as_deref().map(str::trim).unwrap_or("");
            let mut filtered: Vec<_> = v
                .snapshot
                .result
                .paths
                .iter()
                .filter(|p| {
                    params
                        .status
                        .as_deref()
                        .is_none_or(|s| p.status.as_str() == s)
                        && params.module.as_deref().is_none_or(|m| p.module == m)
                        && params.kind.as_deref().is_none_or(|k| p.kind == k)
                        && config_filter.is_none_or(|c| p.config == c)
                        && (feature.is_empty()
                            || p.required_features
                                .iter()
                                .any(|rf| rf == feature || rf.ends_with(&format!(":{feature}"))))
                        && (q.is_empty()
                            || p.path.to_lowercase().contains(&q)
                            || p.module.to_lowercase().contains(&q)
                            || p.namespace.to_lowercase().contains(&q)
                            || p.type_name
                                .as_deref()
                                .unwrap_or("")
                                .to_lowercase()
                                .contains(&q))
                })
                .cloned()
                .collect();
            let matching = filtered.len();
            let offset = params.offset.unwrap_or(0);
            let limit = params.limit.unwrap_or(500).min(5000);
            let page: Vec<_> = filtered
                .drain(offset.min(filtered.len())..)
                .take(limit)
                .collect();
            let modules = v
                .snapshot
                .result
                .modules
                .iter()
                .filter(|m| params.module.as_deref().is_none_or(|mm| m.name == mm))
                .cloned()
                .collect();
            (page, matching, modules, Some(filtered_snapshot))
        }
        None => (Vec::new(), 0, Vec::new(), None),
    };

    Ok(Json(YangApplicabilityResponse {
        address,
        snapshot: snapshot_view,
        paths,
        paths_matching,
        modules,
    }))
}

#[derive(Deserialize)]
pub(super) struct YangApplicabilityRefreshBody {
    /// Datastore to evaluate (e.g. "ietf-datastores:running" or
    /// "operational"); defaults to the unambiguous device default.
    datastore: Option<String>,
    /// Discovery execution path (M6E):
    /// - "auto" (default): use the assigned collector when it is
    ///   connected and advertises the discovery contract; otherwise
    ///   Core-direct. Documented deterministic policy — never a silent
    ///   downgrade of an explicitly requested path.
    /// - "core_direct": synchronous Core-executed discovery.
    /// - "assigned_collector": asynchronous remote discovery via the
    ///   assigned authenticated collector (HTTP 202 + job polling).
    executor: Option<String>,
}

/// Async job view returned for remote discovery requests (202).
#[derive(Serialize)]
pub(super) struct YangDiscoveryJobAccepted {
    job_id: String,
    state: String,
    device_key: String,
    device_address: String,
    collector_id: String,
    datastore: Option<String>,
    operation: String,
    created_at_ns: i64,
    deadline_ns: i64,
    /// Poll `GET /api/devices/{address}/yang-discovery/jobs` for status;
    /// the completed snapshot appears via the applicability endpoint.
    status_url: String,
}

fn request_error_status(e: crate::yang_discovery::RequestError) -> (StatusCode, String) {
    use crate::yang_discovery::RequestError::*;
    match e {
        NoCollector => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "device has no assigned collector for remote discovery".to_string(),
        ),
        CollectorUnsupported => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "assigned collector does not support remote yang discovery".to_string(),
        ),
        QueueFull(which) => (
            StatusCode::TOO_MANY_REQUESTS,
            format!("discovery queue full: {which}"),
        ),
        Internal(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
}

/// POST — bounded refresh. `executor` selects the discovery path:
/// Core-direct stays synchronous; the assigned-collector path is
/// asynchronous (202 + durable job). Uses existing credential
/// resolution and transport security; never writes to the device and
/// never touches acquisition intent.
pub(super) async fn device_yang_applicability_refresh_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    body: Option<Json<YangApplicabilityRefreshBody>>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let body = body.map(|b| b.0);
    let executor = body
        .as_ref()
        .and_then(|b| b.executor.clone())
        .unwrap_or_else(|| "auto".to_string());
    if !matches!(
        executor.as_str(),
        "auto" | "core_direct" | "assigned_collector"
    ) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("unknown discovery executor '{executor}'"),
        ));
    }
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
    let datastore = body.and_then(|b| b.datastore);

    // ── Remote (assigned-collector) path ─────────────────────────────
    // Auto policy: remote only when the device is assigned to a
    // collector whose ACTIVE session advertises the discovery contract.
    // An explicit "assigned_collector" request never silently falls
    // back to Core-direct — it queues (or fails) honestly.
    let remote = match executor.as_str() {
        "assigned_collector" => true,
        "core_direct" => false,
        _ => {
            target.collector_id.is_some()
                && state
                    .collector_manager
                    .as_ref()
                    .and_then(|m| m.collector_discovery_support(target.collector_id.as_deref()?))
                    .unwrap_or(false)
        }
    };
    if remote {
        let store = state.registry.sqlite_store().ok_or((
            StatusCode::SERVICE_UNAVAILABLE,
            "applicability store unavailable".to_string(),
        ))?;
        let manager = state.collector_manager.as_ref().ok_or((
            StatusCode::SERVICE_UNAVAILABLE,
            "collector manager unavailable".to_string(),
        ))?;
        let job = crate::yang_discovery::request_remote_discovery(
            state.registry.as_ref(),
            &store,
            manager.as_ref(),
            &target,
            datastore,
            "admin",
        )
        .map_err(request_error_status)?;
        return Ok(axum::response::Response::builder()
            .status(StatusCode::ACCEPTED)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(
                serde_json::to_string(&YangDiscoveryJobAccepted {
                    job_id: job.job_id.clone(),
                    state: job.state.clone(),
                    device_key: job.device_key.clone(),
                    device_address: job.device_address.clone(),
                    collector_id: job.collector_id.clone(),
                    datastore: job.datastore.clone(),
                    operation: job.operation.clone(),
                    created_at_ns: job.created_at_ns,
                    deadline_ns: job.deadline_ns,
                    status_url: format!("/api/devices/{address}/yang-discovery/jobs"),
                })
                .unwrap_or_default(),
            ))
            .unwrap());
    }

    // ── Core-direct synchronous path ─────────────────────────────────
    let store = state.registry.sqlite_store().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "applicability store unavailable".to_string(),
    ))?;
    let (lib, cache, key) = yang_roots(&state);
    let library = YangLibrary::open(
        std::path::Path::new(&lib),
        std::path::Path::new(&cache),
        &key,
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let resolved = resolve_target_credentials_for_discovery(&target, &state.credentials)
        .map_err(|e| (StatusCode::FAILED_DEPENDENCY, e.to_string()))?;
    let discovery_input = DiscoveryInput {
        address: target.address.clone(),
        username: resolved.as_ref().map(|creds| creds.username.clone()),
        password: resolved.as_ref().map(|creds| creds.password.to_string()),
        username_env: None,
        password_env: None,
        ca_cert_path: target.ca_cert.clone(),
        tls_domain: target.tls_domain.clone(),
        security: target.security,
        client_cert_path: target.client_cert.clone(),
        client_key_path: target.client_key.clone(),
        role_hint: target.role.clone(),
        environment_archetype: None,
        vault: Some(std::sync::Arc::clone(&state.credentials)),
    };

    let identity_ambiguous = device_identity_ambiguous(&state, &target);

    let snapshot = crate::yang_applicability::refresh_device_applicability(
        &crate::yang_applicability::RefreshInput {
            target: &target,
            library: &library,
            discovery_input: &discovery_input,
            identity_ambiguous,
            requested_datastore: datastore,
            store: &store,
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // A persisted applicability snapshot supersedes the evidence an
    // active schema-bound approval was derived from — but ONLY an
    // approval that is not itself derived from the new evidence. The
    // check runs at the registry mutation boundary against the CURRENT
    // binding (never the pre-refresh `target` snapshot): a newer approval
    // committed while discovery was in flight is preserved, and the
    // revocation is fingerprint-guarded against concurrent activation.
    let invalidation = crate::schema_binding::revoke_binding_superseded_by_evidence(
        state.registry.as_ref(),
        &target.address,
        snapshot.snapshot_id,
        &snapshot.evidence_fingerprint,
        "system",
        "yang_applicability_refresh",
    )
    .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
    let schema_binding_revoked = match invalidation {
        crate::schema_binding::EvidenceInvalidation::Revoked(updated) => {
            if let Some(cid) = updated.collector_id.as_deref()
                && let Some(manager) = state.collector_manager.as_ref()
            {
                manager.resync_collector(cid).await;
            }
            true
        }
        _ => false,
    };

    Ok(Json(YangApplicabilityRefreshResponse {
        snapshot_id: snapshot.snapshot_id,
        device_key: snapshot.device_key,
        overall_status: snapshot.overall_status,
        outcome: snapshot.outcome,
        schema_id: snapshot.schema_id,
        content_id: snapshot.content_id,
        datastore: snapshot.datastore,
        evidence_fingerprint: snapshot.evidence_fingerprint,
        paths_total: snapshot.result.paths_total,
        modules_total: snapshot.result.modules.len(),
        evaluated_at_ns: snapshot.evaluated_at_ns,
        error: snapshot.error,
        schema_binding_revoked,
    })
    .into_response())
}

/// Bounded discovery-job view for status polling (M6E). Read-only —
/// Viewer-accessible like the applicability GET. Never exposes
/// credentials, tokens or evidence payloads.
#[derive(Serialize)]
pub(super) struct YangDiscoveryJobView {
    job_id: String,
    state: String,
    device_key: String,
    device_address: String,
    collector_id: String,
    datastore: Option<String>,
    operation: String,
    authority_epoch: i64,
    assignment_generation: i64,
    collector_session_generation: Option<i64>,
    created_at_ns: i64,
    deadline_ns: i64,
    dispatched_at_ns: Option<i64>,
    finished_at_ns: Option<i64>,
    attempt_count: i64,
    result_classification: Option<String>,
    reason_code: Option<String>,
    snapshot_id: Option<i64>,
    evidence_fingerprint: Option<String>,
    created_by: Option<String>,
}

fn job_view(job: &crate::sqlite_store::YangDiscoveryJobRow) -> YangDiscoveryJobView {
    YangDiscoveryJobView {
        job_id: job.job_id.clone(),
        state: job.state.clone(),
        device_key: job.device_key.clone(),
        device_address: job.device_address.clone(),
        collector_id: job.collector_id.clone(),
        datastore: job.datastore.clone(),
        operation: job.operation.clone(),
        authority_epoch: job.authority_epoch,
        assignment_generation: job.assignment_generation,
        collector_session_generation: job.collector_session_generation,
        created_at_ns: job.created_at_ns,
        deadline_ns: job.deadline_ns,
        dispatched_at_ns: job.dispatched_at_ns,
        finished_at_ns: job.finished_at_ns,
        attempt_count: job.attempt_count,
        result_classification: job.result_classification.clone(),
        reason_code: job.reason_code.clone(),
        snapshot_id: job.snapshot_id,
        evidence_fingerprint: job.evidence_fingerprint.clone(),
        created_by: job.created_by.clone(),
    }
}

/// GET /api/devices/{address}/yang-discovery/jobs — bounded recent job
/// history for one device (newest first). Status polling for async
/// remote discovery; the completed snapshot is read via the
/// applicability endpoint.
pub(super) async fn device_yang_discovery_jobs_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(params): Query<YangApplicabilityParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
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
    let store = state.registry.sqlite_store().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "applicability store unavailable".to_string(),
    ))?;
    let device_key = crate::registry::device_identity_key(&target);
    let limit = params.limit.unwrap_or(16).min(64);
    let jobs = store
        .list_yang_discovery_jobs(&device_key, limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({
        "device_key": device_key,
        "jobs": jobs.iter().map(job_view).collect::<Vec<_>>(),
    })))
}

// ── M6C-2 schema-bound acquisition authority ────────────────────────

/// Read-only schema-bound plan preview. Uses the current verified active
/// catalogue, persisted applicability snapshot, and read-time freshness;
/// it never mutates target, assignment, lifecycle, or subscription state.
pub(super) async fn schema_binding_preview_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Json(req): Json<crate::schema_binding::SchemaPlanRequest>,
) -> Result<Json<crate::schema_binding::SchemaPlanPreview>, (StatusCode, String)> {
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
    let store = state.registry.sqlite_store().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "schema-binding store unavailable".to_string(),
    ))?;
    let (lib, cache, key) = yang_roots(&state);
    let now_ns = crate::graph::common::now_ns();
    tokio::task::spawn_blocking(move || {
        let library = YangLibrary::open(
            std::path::Path::new(&lib),
            std::path::Path::new(&cache),
            &key,
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        crate::schema_binding::evaluate_preview(&library, store.as_ref(), &target, &req, now_ns)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
}

#[derive(Deserialize)]
pub(super) struct SchemaBindingRevokeRequest {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Serialize)]
pub(super) struct SchemaBindingMutationResponse {
    success: bool,
    device_address: String,
    approval_revision: Option<i64>,
    plan_id: Option<String>,
    binding_fingerprint: Option<String>,
    state: String,
}

fn schema_binding_actor(
    subject: Option<axum::Extension<crate::http_server::auth::AuthenticatedSubject>>,
) -> Result<String, (StatusCode, String)> {
    subject
        .map(|axum::Extension(s)| s.0)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            (
                StatusCode::FORBIDDEN,
                "authenticated subject is required for schema-binding approval".to_string(),
            )
        })
}

fn require_schema_binding_admin(
    role: Option<axum::Extension<crate::http_server::auth::Role>>,
) -> Result<(), (StatusCode, String)> {
    match role {
        Some(axum::Extension(crate::http_server::auth::Role::Admin)) => Ok(()),
        _ => Err((
            StatusCode::FORBIDDEN,
            "schema-binding activation requires an authenticated admin".to_string(),
        )),
    }
}

/// Admin activation: re-evaluate server-side, then commit the durable
/// binding and the approved manifest through the registry. The API never
/// submits a gNMI subscription directly; collector resync carries the new
/// assignment authority.
pub(super) async fn schema_binding_activate_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    role: Option<axum::Extension<crate::http_server::auth::Role>>,
    subject: Option<axum::Extension<crate::http_server::auth::AuthenticatedSubject>>,
    Json(req): Json<crate::schema_binding::SchemaPlanRequest>,
) -> Result<Json<SchemaBindingMutationResponse>, (StatusCode, String)> {
    require_schema_binding_admin(role)?;
    let actor = schema_binding_actor(subject)?;
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
    let store = state.registry.sqlite_store().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "schema-binding store unavailable".to_string(),
    ))?;
    let (lib, cache, key) = yang_roots(&state);
    let now_ns = crate::graph::common::now_ns();
    let activation = {
        let store = store.clone();
        let target = target.clone();
        let actor = actor.clone();
        tokio::task::spawn_blocking(move || {
            let library = YangLibrary::open(
                std::path::Path::new(&lib),
                std::path::Path::new(&cache),
                &key,
            )
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            crate::schema_binding::activate(&library, store.as_ref(), &target, &req, &actor, now_ns)
                .map_err(|e| (StatusCode::CONFLICT, e.to_string()))
        })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??
    };
    let expected_revision = activation.record.approval_revision - 1;
    let expected_config_revision = activation.evaluation.config_revision;
    let effective_schema_id = activation.effective_schema_id.clone();
    let updated = state
        .registry
        .activate_schema_binding(
            activation.device,
            activation.record.clone(),
            expected_revision,
            expected_config_revision,
            &actor,
            "api_schema_binding_activate",
            effective_schema_id.as_deref(),
        )
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;

    if let Some(cid) = updated.collector_id.as_deref()
        && let Some(manager) = state.collector_manager.as_ref()
    {
        manager.resync_collector(cid).await;
    }

    Ok(Json(SchemaBindingMutationResponse {
        success: true,
        device_address: updated.address,
        approval_revision: Some(activation.record.approval_revision),
        plan_id: Some(activation.evaluation.plan_id),
        binding_fingerprint: Some(activation.evaluation.binding_fingerprint),
        state: "active".to_string(),
    }))
}

/// Admin revocation/rollback: mark the durable policy revoked, clear the
/// device runtime binding, disable acquisition, and resync the collector
/// so assignment fencing withdraws the old execution authority.
pub(super) async fn schema_binding_revoke_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    role: Option<axum::Extension<crate::http_server::auth::Role>>,
    subject: Option<axum::Extension<crate::http_server::auth::AuthenticatedSubject>>,
    Json(req): Json<SchemaBindingRevokeRequest>,
) -> Result<Json<SchemaBindingMutationResponse>, (StatusCode, String)> {
    require_schema_binding_admin(role)?;
    let actor = schema_binding_actor(subject)?;
    let reason = req
        .reason
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "operator_revoked".to_string());
    let updated = state
        .registry
        .revoke_schema_binding(&address, &actor, &reason, "api_schema_binding_revoke")
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("device '{address}' not found"),
            )
        })?;

    if let Some(cid) = updated.collector_id.as_deref()
        && let Some(manager) = state.collector_manager.as_ref()
    {
        manager.resync_collector(cid).await;
    }

    Ok(Json(SchemaBindingMutationResponse {
        success: true,
        device_address: updated.address,
        approval_revision: None,
        plan_id: None,
        binding_fingerprint: None,
        state: "revoked".to_string(),
    }))
}

// ── M6F-F per-device schema selection ────────────────────────────────

#[derive(Serialize)]
pub(super) struct DeviceSchemaSelectionResponse {
    device_key: String,
    device_address: String,
    /// Durable explicit selection, when the device has one.
    selection: Option<crate::sqlite_store::DeviceSchemaSelection>,
    /// schema_id the device is effectively bound to (selection or the
    /// globally active catalogue fallback).
    effective_schema_id: Option<String>,
    /// schema_id of the globally active catalogue — context only.
    active_schema_id: Option<String>,
    /// Why the effective catalogue is not authoritative, when it is not.
    catalogue_state: Option<crate::yang_schema::applicability::ReasonCode>,
}

fn device_schema_selection_response(
    store: &crate::sqlite_store::SqliteStore,
    library: &YangLibrary,
    target: &TargetConfig,
) -> Result<DeviceSchemaSelectionResponse, (StatusCode, String)> {
    let device_key = crate::registry::device_identity_key(target);
    let selection = store
        .get_device_schema_selection(&device_key)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let effective =
        crate::yang_applicability::resolve_effective_catalogue(library, store, &device_key)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(DeviceSchemaSelectionResponse {
        device_key,
        device_address: target.address.clone(),
        selection,
        effective_schema_id: effective.effective_schema_id,
        active_schema_id: effective.active_schema_id,
        catalogue_state: effective.catalogue_state,
    })
}

/// Read the device's schema selection and effective catalogue identity.
/// Viewer-readable; never mutates state.
pub(super) async fn device_schema_selection_get_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceSchemaSelectionResponse>, (StatusCode, String)> {
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
    let store = state.registry.sqlite_store().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "schema-selection store unavailable".to_string(),
    ))?;
    let (lib, cache, key) = yang_roots(&state);
    tokio::task::spawn_blocking(move || {
        let library = YangLibrary::open(
            std::path::Path::new(&lib),
            std::path::Path::new(&cache),
            &key,
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        device_schema_selection_response(store.as_ref(), &library, &target)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
}

#[derive(Deserialize)]
pub(super) struct DeviceSchemaSelectionRequest {
    schema_id: String,
    #[serde(default)]
    note: Option<String>,
}

/// Admin: bind the device to an explicitly selected verified compiled
/// bundle. The bundle must exist and pass integrity verification as a
/// COMPLETE catalogue — partial catalogues are never selectable
/// authority. A schema-bound approval derived from a different schema is
/// revoked through the fingerprint-guarded device-scoped path before the
/// selection commits ordering-wise (revoke first, then select: the
/// device can never hold an approval whose schema differs from its
/// effective catalogue).
pub(super) async fn device_schema_selection_put_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    role: Option<axum::Extension<crate::http_server::auth::Role>>,
    subject: Option<axum::Extension<crate::http_server::auth::AuthenticatedSubject>>,
    Json(req): Json<DeviceSchemaSelectionRequest>,
) -> Result<Json<DeviceSchemaSelectionResponse>, (StatusCode, String)> {
    require_schema_binding_admin(role)?;
    let actor = schema_binding_actor(subject)?;
    let schema_id = req.schema_id.trim().to_string();
    if schema_id.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "schema_id is required".to_string()));
    }
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
    let store = state.registry.sqlite_store().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "schema-selection store unavailable".to_string(),
    ))?;
    let (lib, cache, key) = yang_roots(&state);

    // Verify the bundle is a complete, integrity-checked catalogue
    // BEFORE mutating any authority.
    let bundle_state = {
        let (l, c, k) = (lib.clone(), cache.clone(), key.clone());
        let sid = schema_id.clone();
        tokio::task::spawn_blocking(move || {
            let library = YangLibrary::open(std::path::Path::new(&l), std::path::Path::new(&c), &k)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            Ok::<_, (StatusCode, String)>(crate::yang_schema::compiled_bundle_state(&library, &sid))
        })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??
    };
    match bundle_state {
        crate::yang_schema::ActiveCatalogueState::Verified(_) => {}
        crate::yang_schema::ActiveCatalogueState::Partial(_) => {
            return Err((
                StatusCode::CONFLICT,
                "selected bundle is not a complete catalogue; partial schemas are not selectable authority".to_string(),
            ));
        }
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "schema bundle '{schema_id}' is not present or fails integrity verification"
                ),
            ));
        }
    }

    // Revoke-first ordering: an approval derived from a different schema
    // is withdrawn before the selection commits, so the device can never
    // sit in a state where its binding schema differs from its effective
    // catalogue. Fingerprint-guarded — a concurrent newer approval wins.
    let revoked = crate::schema_binding::revoke_binding_for_device_schema_change(
        state.registry.as_ref(),
        &target.address,
        &schema_id,
        &actor,
        "api_device_schema_select",
    )
    .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;

    let device_key = crate::registry::device_identity_key(&target);
    let selection = crate::sqlite_store::DeviceSchemaSelection {
        device_key: device_key.clone(),
        device_address: target.address.clone(),
        schema_id: schema_id.clone(),
        selection_source: "admin".to_string(),
        selected_by: Some(actor.clone()),
        note: req
            .note
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty()),
        selection_revision: 0,
        created_at_ns: 0,
        updated_at_ns: 0,
    };
    let store_for_write = store.clone();
    let actor_for_write = actor.clone();
    tokio::task::spawn_blocking(move || {
        store_for_write
            .set_device_schema_selection(&selection, &actor_for_write, "api_device_schema_select")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    // Post-commit sweep: an approval evaluated against the OLD selection
    // may have committed between the pre-revoke and this write (the
    // in-transaction check only sees the selection state at its own
    // commit time). Re-run the fingerprint-guarded device-scoped revoke
    // against the NEW effective schema so no inconsistent approval can
    // survive the mutation window.
    let post_revoked = crate::schema_binding::revoke_binding_for_device_schema_change(
        state.registry.as_ref(),
        &target.address,
        &schema_id,
        &actor,
        "api_device_schema_select_post",
    )
    .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;

    for updated in revoked.into_iter().chain(post_revoked) {
        if let Some(cid) = updated.collector_id.as_deref()
            && let Some(manager) = state.collector_manager.as_ref()
        {
            manager.resync_collector(cid).await;
        }
    }

    let response = {
        let (l, c, k) = (lib.clone(), cache.clone(), key.clone());
        let target = target.clone();
        let store = store.clone();
        tokio::task::spawn_blocking(move || {
            let library = YangLibrary::open(std::path::Path::new(&l), std::path::Path::new(&c), &k)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            device_schema_selection_response(store.as_ref(), &library, &target)
        })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??
    };
    Ok(Json(response))
}

/// Admin: clear the device's explicit selection — the device reverts to
/// the globally active catalogue. A schema-bound approval whose schema
/// differs from the now-effective catalogue is revoked first (same
/// fail-closed ordering as selection).
pub(super) async fn device_schema_selection_delete_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    role: Option<axum::Extension<crate::http_server::auth::Role>>,
    subject: Option<axum::Extension<crate::http_server::auth::AuthenticatedSubject>>,
) -> Result<Json<DeviceSchemaSelectionResponse>, (StatusCode, String)> {
    require_schema_binding_admin(role)?;
    let actor = schema_binding_actor(subject)?;
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
    let store = state.registry.sqlite_store().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "schema-selection store unavailable".to_string(),
    ))?;
    let (lib, cache, key) = yang_roots(&state);

    // The now-effective schema after clearing is the global active
    // catalogue (empty string when none — revokes any schema-bound
    // approval, which cannot be authoritative without a catalogue).
    let effective_after_clear = {
        let (l, c, k) = (lib.clone(), cache.clone(), key.clone());
        tokio::task::spawn_blocking(move || {
            let library = YangLibrary::open(std::path::Path::new(&l), std::path::Path::new(&c), &k)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            Ok::<_, (StatusCode, String)>(
                crate::yang_schema::active_catalogue_state(&library)
                    .schema_id()
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??
    };

    let revoked = crate::schema_binding::revoke_binding_for_device_schema_change(
        state.registry.as_ref(),
        &target.address,
        &effective_after_clear,
        &actor,
        "api_device_schema_clear",
    )
    .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;

    let device_key = crate::registry::device_identity_key(&target);
    let store_for_write = store.clone();
    let dk = device_key.clone();
    let actor_for_write = actor.clone();
    tokio::task::spawn_blocking(move || {
        store_for_write
            .clear_device_schema_selection(&dk, &actor_for_write, "api_device_schema_clear")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    // Post-commit sweep against the now-effective (global active)
    // catalogue — same mutation-window coverage as selection PUT.
    let post_revoked = crate::schema_binding::revoke_binding_for_device_schema_change(
        state.registry.as_ref(),
        &target.address,
        &effective_after_clear,
        &actor,
        "api_device_schema_clear_post",
    )
    .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;

    for updated in revoked.into_iter().chain(post_revoked) {
        if let Some(cid) = updated.collector_id.as_deref()
            && let Some(manager) = state.collector_manager.as_ref()
        {
            manager.resync_collector(cid).await;
        }
    }

    let response = {
        let (l, c, k) = (lib.clone(), cache.clone(), key.clone());
        let target = target.clone();
        let store = store.clone();
        tokio::task::spawn_blocking(move || {
            let library = YangLibrary::open(std::path::Path::new(&l), std::path::Path::new(&c), &k)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            device_schema_selection_response(store.as_ref(), &library, &target)
        })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??
    };
    Ok(Json(response))
}

/// True when another registered device shares this device's canonical
/// address or external identity — evidence must stay unmerged.
fn device_identity_ambiguous(state: &AppState, target: &TargetConfig) -> bool {
    let canonical = crate::registry::canonical_device_key(&target.address);
    let targets = match state.registry.list_all_targets() {
        Ok(t) => t,
        Err(_) => return true, // cannot disprove ambiguity → preserve it
    };
    targets.iter().any(|other| {
        other.address != target.address
            && (crate::registry::canonical_device_key(&other.address) == canonical
                || match (&target.external_id, &other.external_id) {
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                })
    })
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
    if target.schema_binding.is_some() {
        return Err((
            StatusCode::CONFLICT,
            "schema-bound acquisition plan is approval-managed; use schema-binding activation/revocation".to_string(),
        ));
    }

    // This endpoint applies an explicit operator path selection, so it
    // participates in the same acquisition contract as the managed-device
    // API: every path must be a well-formed structured gNMI path and the
    // resulting intent is Explicit — an empty list is an explicit empty
    // selection (no collection), never a silent return to automatic.
    let paths: Vec<crate::config::SelectedSubscriptionPath> = req
        .selected_paths
        .into_iter()
        .filter(|path| !path.path.trim().is_empty())
        .collect();
    super::managed_devices::validate_explicit_paths(
        &crate::config::AcquisitionMode::Explicit,
        &paths,
    )?;
    target.selected_paths = paths;
    target.acquisition = crate::config::AcquisitionMode::Explicit;

    let updated = state
        .registry
        .update_device_with_audit(target, "api", "api_apply_recommendation_paths")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Push a fresh full sync so a running collector drops any obsolete
    // broader subscription promptly rather than on next reconnect.
    if let Some(cid) = updated.collector_id.as_deref()
        && let Some(manager) = state.collector_manager.as_ref()
    {
        manager.resync_collector(cid).await;
    }

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
    let address = crate::registry::strip_port(address).to_string();
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
    let address = crate::registry::strip_port(address).to_string();
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
            (Some(username), Some(password)) => Some(ResolvedCredential {
                username,
                password: zeroize::Zeroizing::new(password),
            }),
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
pub(super) async fn enrichment_list_handler(
    State(state): State<AppState>,
) -> Json<EnrichmentListResponse> {
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
pub(super) async fn enrichment_audit_handler(
    State(state): State<AppState>,
) -> Json<EnrichmentAuditResponse> {
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
    /// NetBox immutable object ID (e.g. 1234). 0 when the source did not
    /// provide one — such candidates cannot be imported as stable
    /// external identities.
    pub netbox_id: u64,
    /// Stable external identity derived from the NetBox instance
    /// identity (scheme + host + port, never credentials) and the
    /// immutable NetBox object ID, e.g.
    /// `netbox:https:netbox.example.com:443:device:123`. Unique across
    /// NetBox deployments — two instances can both have device id 123.
    pub external_id: String,
    pub name: String,
    pub address: String,
    pub site: String,
    pub role: String,
    pub vendor: String,
    pub platform: String,
    /// Inventory-declared lifecycle status; `None` when NetBox did not
    /// report one — stays unknown, never fabricated as "active".
    pub status: Option<String>,
    pub tenant: String,
    /// False when the candidate cannot be imported as-is (e.g. no primary
    /// IP, no stable NetBox object ID). Surfaced explicitly, never
    /// silently dropped.
    pub eligible: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub skip_reason: String,
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
        Ok(resp) if resp.status().is_success() => resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| {
                v.get("netbox-version")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "unknown".to_string()),
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

    // BN-015-U1/N1: external identities are scoped to THIS NetBox
    // instance. The instance identity is derived from the connection
    // URL's scheme + normalized authority — never from credentials or
    // tokens — so `netbox:device:123` on two different deployments
    // cannot collide.
    let instance_identity = netbox_instance_identity(&base).map_err(|e| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            format!("cannot derive NetBox instance identity: {e}"),
        )
    })?;

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
            id: Option<u64>,
            name: Option<String>,
            primary_ip: Option<NbIpImport>,
            site: Option<NbNestedImport>,
            role: Option<NbNestedImport>,
            device_type: Option<NbDeviceTypeImport>,
            platform: Option<NbNestedImport>,
            status: Option<NbStatusImport>,
            tenant: Option<NbNestedImport>,
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

        let page: NbPage = resp.json().await.map_err(|e| {
            (
                axum::http::StatusCode::BAD_GATEWAY,
                format!("parse error: {e}"),
            )
        })?;

        let fetched = page.results.len();

        for dev in page.results {
            let name = dev.name.unwrap_or_default();
            // Strip the prefix length from primary IP (e.g. "10.0.0.1/32" → "10.0.0.1")
            let raw_ip = dev
                .primary_ip
                .as_ref()
                .map(|ip| ip.address.split('/').next().unwrap_or("").to_string())
                .unwrap_or_default();

            let netbox_id = dev.id.unwrap_or(0);
            let (eligible, skip_reason) = if raw_ip.is_empty() {
                (false, "no primary IP address".to_string())
            } else if netbox_id == 0 {
                (false, "no stable NetBox object id".to_string())
            } else {
                (true, String::new())
            };
            if !eligible {
                warnings.push(format!(
                    "device '{name}' ineligible for import: {skip_reason}"
                ));
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
            let status = dev.status.as_ref().map(|s| s.value.clone());

            let tenant = dev
                .tenant
                .as_ref()
                .map(|t| t.name.clone())
                .unwrap_or_default();

            candidates.push(NetboxImportCandidate {
                netbox_id,
                external_id: if netbox_id > 0 {
                    format!("netbox:{instance_identity}:device:{netbox_id}")
                } else {
                    String::new()
                },
                name,
                address: raw_ip,
                site,
                role,
                vendor,
                platform,
                status,
                tenant,
                eligible,
                skip_reason,
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

/// Derive the stable NetBox instance identity from a base URL.
///
/// Format: `{scheme}:{host}:{port}` — e.g. `https:netbox.example.com:443`.
/// Only the scheme and normalized authority participate: credentials,
/// userinfo, path, query and fragment are never part of the identity.
/// The host is lowercased (DNS is case-insensitive), IPv6 literals are
/// bracketed, and the port is always explicit (defaulting to 80/443 for
/// http/https). Two NetBox deployments therefore always produce
/// distinct external identities even when they share device IDs.
fn netbox_instance_identity(base_url: &str) -> Result<String, String> {
    let url = base_url.trim();
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| "URL must include a scheme (http:// or https://)".to_string())?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "unsupported scheme '{scheme}' (expected http or https)"
        ));
    }
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    if authority.is_empty() {
        return Err("missing host in NetBox URL".to_string());
    }

    let (host_raw, explicit_port) = if let Some(inner) = authority.strip_prefix('[') {
        let end = inner
            .find(']')
            .ok_or_else(|| "malformed bracketed IPv6 host".to_string())?;
        let host = &inner[..end];
        let remainder = &inner[end + 1..];
        let port = if remainder.is_empty() {
            None
        } else {
            let p = remainder
                .strip_prefix(':')
                .ok_or_else(|| "malformed authority after IPv6 host".to_string())?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("invalid port '{p}'"));
            }
            Some(p.to_string())
        };
        (host.to_string(), port)
    } else if authority.contains(':') {
        let (host, port) = authority
            .rsplit_once(':')
            .ok_or_else(|| "malformed authority".to_string())?;
        if host.contains(':') {
            return Err("bare IPv6 literals must be bracketed in the NetBox URL".to_string());
        }
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("invalid port '{port}'"));
        }
        (host.to_string(), Some(port.to_string()))
    } else {
        (authority.to_string(), None)
    };

    let host = host_raw.to_ascii_lowercase();
    if host.is_empty() {
        return Err("missing host in NetBox URL".to_string());
    }
    let port: u16 = match explicit_port {
        Some(p) => p
            .parse::<u16>()
            .map_err(|_| format!("invalid port '{p}'"))?,
        None => {
            if scheme == "https" {
                443
            } else {
                80
            }
        }
    };
    if port == 0 {
        return Err("port must be non-zero".to_string());
    }
    let display_host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    Ok(format!("{scheme}:{display_host}:{port}"))
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
                id: vals
                    .get(0)
                    .and_then(|v| {
                        if let Value::String(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default(),
                component_name: vals
                    .get(1)
                    .and_then(|v| {
                        if let Value::String(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default(),
                sensor_type: vals
                    .get(2)
                    .and_then(|v| {
                        if let Value::String(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default(),
                temperature_c: vals.get(3).and_then(|v| {
                    if let Value::Double(f) = v {
                        Some(*f)
                    } else {
                        None
                    }
                }),
                power_w: vals.get(4).and_then(|v| {
                    if let Value::Double(f) = v {
                        Some(*f)
                    } else {
                        None
                    }
                }),
                fan_rpm: vals.get(5).and_then(|v| {
                    if let Value::Int64(i) = v {
                        Some(*i)
                    } else {
                        None
                    }
                }),
                humidity_pct: vals.get(6).and_then(|v| {
                    if let Value::Double(f) = v {
                        Some(*f)
                    } else {
                        None
                    }
                }),
                updated_at: vals.get(7).and_then(|v| {
                    if let Value::Int64(i) = v {
                        Some(*i)
                    } else {
                        None
                    }
                }),
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
                id: vals
                    .get(0)
                    .and_then(|v| {
                        if let Value::String(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default(),
                if_name: vals
                    .get(1)
                    .and_then(|v| {
                        if let Value::String(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default(),
                rx_power_dbm: vals.get(2).and_then(|v| {
                    if let Value::Double(f) = v {
                        Some(*f)
                    } else {
                        None
                    }
                }),
                tx_power_dbm: vals.get(3).and_then(|v| {
                    if let Value::Double(f) = v {
                        Some(*f)
                    } else {
                        None
                    }
                }),
                laser_bias_ma: vals.get(4).and_then(|v| {
                    if let Value::Double(f) = v {
                        Some(*f)
                    } else {
                        None
                    }
                }),
                temperature_c: vals.get(5).and_then(|v| {
                    if let Value::Double(f) = v {
                        Some(*f)
                    } else {
                        None
                    }
                }),
                voltage_v: vals.get(6).and_then(|v| {
                    if let Value::Double(f) = v {
                        Some(*f)
                    } else {
                        None
                    }
                }),
                updated_at: vals.get(7).and_then(|v| {
                    if let Value::Int64(i) = v {
                        Some(*i)
                    } else {
                        None
                    }
                }),
            });
        }
        Ok::<_, String>(out)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::netbox_instance_identity;

    #[test]
    fn instance_identity_distinguishes_deployments() {
        // Two NetBox instances that both contain device id 123 must
        // produce different external identities.
        let a = netbox_instance_identity("https://netbox-a.example.com").unwrap();
        let b = netbox_instance_identity("https://netbox-b.example.com").unwrap();
        assert_ne!(a, b);
        assert_eq!(a, "https:netbox-a.example.com:443");
        assert_eq!(b, "https:netbox-b.example.com:443");
    }

    #[test]
    fn instance_identity_never_contains_credentials() {
        // Userinfo, tokens in path/query — none may leak into identity.
        let id = netbox_instance_identity(
            "https://admin:s3cr3t@netbox.example.com:8443/api?token=abc#frag",
        )
        .unwrap();
        assert_eq!(id, "https:netbox.example.com:8443");
        assert!(!id.contains("s3cr3t"));
        assert!(!id.contains("abc"));
    }

    #[test]
    fn instance_identity_normalizes_case_scheme_and_default_port() {
        assert_eq!(
            netbox_instance_identity("HTTPS://NetBox.Example.COM").unwrap(),
            "https:netbox.example.com:443"
        );
        assert_eq!(
            netbox_instance_identity("http://NetBox.Example.COM:80").unwrap(),
            "http:netbox.example.com:80"
        );
        // Explicit default port equals implicit default port.
        assert_eq!(
            netbox_instance_identity("https://netbox.example.com:443").unwrap(),
            netbox_instance_identity("https://netbox.example.com").unwrap()
        );
    }

    #[test]
    fn instance_identity_handles_ipv6_and_rejects_malformed() {
        assert_eq!(
            netbox_instance_identity("https://[2001:db8::1]:8443").unwrap(),
            "https:[2001:db8::1]:8443"
        );
        assert!(netbox_instance_identity("netbox.example.com").is_err());
        assert!(netbox_instance_identity("ftp://netbox.example.com").is_err());
        assert!(netbox_instance_identity("https://").is_err());
        assert!(netbox_instance_identity("https://netbox.example.com:0").is_err());
        assert!(netbox_instance_identity("https://netbox.example.com:notaport").is_err());
    }
}
