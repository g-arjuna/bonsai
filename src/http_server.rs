use axum::response::IntoResponse;
/// Phase 6 HTTP API + SSE server (Axum).
///
/// Runs on port 3000 alongside the Tonic gRPC server (port 50051).
/// Shares the same Arc<GraphStore> — handlers call GraphStore read methods
/// directly, with zero extra serialization vs the gRPC path.
///
/// Endpoints:
///   GET /api/topology          — devices, LLDP links, BGP sessions, health
///   GET /api/detections        — recent DetectionEvents + Remediations
///   GET /api/trace/:id         — closed-loop trace for one DetectionEvent
///   GET /api/events            — SSE stream of live BonsaiEvents
///   GET / (and assets/*)       — Svelte SPA static files from ui/dist/
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use futures::stream::{Stream, StreamExt};
use lbug::{Connection, Value};
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
use tower_http::{cors::CorsLayer, services::ServeDir};

use crate::assignment::{CollectorManager, CollectorStatus};
use crate::catalogue::CatalogueState;
use crate::enrichment::{EnricherConfig, SharedEnricherRegistry};
use crate::gnmi_set::gnmi_set;
use crate::graph::{
    DetectionRow, EnvironmentRecord, GraphStore, REMEDIATION_TRUST_CUTOFF_ISO,
    RemediationProposalRow, SiteRecord, TraceStep,
};
use crate::output::traits::{OutputAdapterConfig, OutputAdapterRunState, SharedAdapterRegistry};
use crate::resource_governor::GovernorHandle;
use crate::{
    archive, audit,
    change_detection::{self, ChangeDetectionRuntime},
    config::{
        AssignmentRule, LayeredIngestionConfig, RemediationConfig, SelectedSubscriptionPath,
        ServiceNowConfig, StorageConfig, StreamingConfig, TargetConfig,
    },
    credentials::{CredentialSummary, CredentialVault, ResolvePurpose, ResolvedCredential},
    discovery::{self, DiscoveryInput},
    disk_guard, event_bus, memory_profile,
    registry::{ApiRegistry, DeviceRegistry, RegistryChange},
    remediation::{
        SharedRollbackRegistry, SharedTrustStore, TrustKey, TrustState, check_graduation,
    },
    signals::syslog::{SyslogEvent, SyslogFact},
    store::BonsaiStore,
    streaming::{self, StreamingReadinessReport},
    synthesizer,
    yang::YangLibrary,
};

// ── JSON response types ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct TopologyResponse {
    #[serde(rename = "_schema_version")]
    schema_version: String,
    devices: Vec<DeviceJson>,
    links: Vec<LinkJson>,
}

#[derive(Serialize)]
struct DeviceJson {
    address: String,
    hostname: String,
    vendor: String,
    role: String,
    site: String,
    site_id: String,
    site_path: String,
    health: String, // "healthy" | "warn" | "critical"
    bgp: Vec<BgpJson>,
}

#[derive(Serialize)]
struct BgpJson {
    peer: String,
    state: String,
    peer_as: i64,
}

#[derive(Serialize)]
struct LinkJson {
    src_device: String,
    src_iface: String,
    dst_device: String,
    dst_iface: String,
    /// Combined bytes on this link (sum of both interface in_octets + out_octets) — used for
    /// link utilisation heatmap. Zero when counter data is unavailable.
    bytes_total: i64,
    /// True for MGMT_LINK edges (out-of-band management plane). Hidden by default in the UI.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    is_mgmt: bool,
}

#[derive(Serialize)]
struct PathResponse {
    #[serde(rename = "_schema_version")]
    schema_version: String,
    /// Device addresses in hop order, source first.
    hops: Vec<String>,
    /// (src_device, src_iface, dst_device, dst_iface) for each hop's link.
    links: Vec<(String, String, String, String)>,
}

#[derive(Serialize)]
struct DetectionsResponse {
    #[serde(rename = "_schema_version")]
    schema_version: String,
    detections: Vec<DetectionRow>,
}

#[derive(Serialize)]
struct TraceResponse {
    #[serde(rename = "_schema_version")]
    schema_version: String,
    steps: Vec<TraceStep>,
}

#[derive(Serialize)]
struct IncidentJson {
    id: String,
    root: DetectionRow,
    cascading: Vec<DetectionRow>,
    affected_devices: Vec<String>,
    severity: String,
    started_at_ns: i64,
    ended_at_ns: i64,
    remediation_status: String,
}

#[derive(Serialize)]
struct IncidentsResponse {
    #[serde(rename = "_schema_version")]
    schema_version: String,
    incidents: Vec<IncidentJson>,
}

#[derive(Deserialize, Default)]
struct IncidentsParams {
    #[serde(default = "default_incident_window")]
    window_secs: u64,
    #[serde(default = "default_incident_limit")]
    limit: u32,
}

fn default_incident_window() -> u64 {
    30
}
fn default_incident_limit() -> u32 {
    200
}

#[derive(Serialize)]
struct ReadinessResponse {
    #[serde(rename = "_schema_version")]
    schema_version: String,
    detection_events: usize,
    state_change_events: usize,
    rule_distribution: HashMap<String, usize>,
    cutoff_iso: String,
    remediation_rows_post_cutoff: usize,
    action_distribution_post_cutoff: HashMap<String, usize>,
    status_distribution_post_cutoff: HashMap<String, usize>,
}

#[derive(Serialize)]
struct OperationsResponse {
    #[serde(rename = "_schema_version")]
    schema_version: String,
    detection_events: usize,
    state_change_events: usize,
    remediation_rows_post_cutoff: usize,
    rule_distribution: HashMap<String, usize>,
    action_distribution_post_cutoff: HashMap<String, usize>,
    status_distribution_post_cutoff: HashMap<String, usize>,
    device_count: usize,
    enabled_device_count: usize,
    observed_subscriptions: usize,
    pending_subscriptions: usize,
    silent_subscriptions: usize,
    collectors_connected: usize,
    collectors_total: usize,
    unassigned_devices: usize,
    event_bus_depth: u64,
    event_bus_receivers: u64,
    archive_lag_millis: i64,
    archive_buffer_rows: u64,
    archive_last_flush_millis: u64,
    archive_last_compression_ppm: u64,
    cutoff_iso: String,
    // Memory + disk (T4-6)
    rss_bytes: u64,
    archive_disk_bytes: u64,
    archive_disk_pct: u8,
    graph_disk_bytes: u64,
    graph_disk_pct: u8,
    // Memory health (T1-4)
    memory_budget_bytes: u64,
    memory_rss_pct_of_budget: f64,
    // Counter mode (T1-8 / C-9) — operational visibility of ingest mode
    counter_mode: String,
    counter_window_secs: u64,
    counter_debounce_secs: u64,
}

const RSS_BUDGET_BYTES: u64 = 1_610_612_736; // 1.5 GiB
const COORDINATOR_QUEUE_BUDGET_PCT: u64 = 75;
const API_SCHEMA_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Serialize)]
struct BudgetBreach {
    name: &'static str,
    current: f64,
    budget: f64,
    unit: &'static str,
}

#[derive(Serialize)]
struct TestStatusResponse {
    ts_unix: u64,
    memory: memory_profile::MemorySnapshot,
    disk: DiskStatusJson,
    budget_breaches: Vec<BudgetBreach>,
    external: serde_json::Value,
    driver_results: serde_json::Value,
}

#[derive(Serialize)]
struct DiskStatusJson {
    archive_bytes: u64,
    archive_max_bytes: u64,
    archive_pct: u8,
    graph_bytes: u64,
    graph_max_bytes: u64,
    graph_pct: u8,
}

/// Outbound SSE payload — mirrors BonsaiEvent but serialised as JSON.
#[derive(Serialize)]
struct ManagedDevicesResponse {
    devices: Vec<ManagedDeviceJson>,
}

#[derive(Serialize)]
struct ManagedDeviceJson {
    address: String,
    enabled: bool,
    collector_id: String,
    tls_domain: String,
    ca_cert: String,
    vendor: String,
    credential_alias: String,
    username_env: String,
    password_env: String,
    hostname: String,
    role: String,
    site: String,
    selected_paths: Vec<SelectedSubscriptionPath>,
    subscription_statuses: Vec<SubscriptionStatusJson>,
    resolution_audit: Vec<String>,
}

#[derive(Serialize, Clone)]
struct SubscriptionStatusJson {
    path: String,
    origin: String,
    mode: String,
    sample_interval_ns: i64,
    status: String,
    first_observed_at_ns: i64,
    last_observed_at_ns: i64,
    updated_at_ns: i64,
}

#[derive(Deserialize)]
struct OnboardingDiscoveryRequest {
    address: String,
    #[serde(default)]
    username_env: String,
    #[serde(default)]
    password_env: String,
    #[serde(default)]
    credential_alias: String,
    #[serde(default)]
    ca_cert_path: String,
    #[serde(default)]
    tls_domain: String,
    #[serde(default)]
    role_hint: String,
    #[serde(default)]
    environment_archetype: String,
}

#[derive(Deserialize)]
struct ManagedDeviceRequest {
    address: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    tls_domain: String,
    #[serde(default)]
    ca_cert: String,
    #[serde(default)]
    vendor: String,
    #[serde(default)]
    credential_alias: String,
    #[serde(default)]
    username_env: String,
    #[serde(default)]
    password_env: String,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    site: String,
    #[serde(default)]
    selected_paths: Vec<SelectedSubscriptionPath>,
}

#[derive(Deserialize)]
struct RemoveManagedDeviceRequest {
    address: String,
}

#[derive(Deserialize)]
struct BulkManagedDeviceActionRequest {
    addresses: Vec<String>,
    action: String,
}

#[derive(Serialize)]
struct BulkManagedDeviceActionResponse {
    success: bool,
    error: String,
    devices: Vec<ManagedDeviceJson>,
}

#[derive(Serialize)]
struct RemoveImpactResponse {
    address: String,
    subscription_total: usize,
    subscription_observed: usize,
    subscription_pending: usize,
    trust_marks_total: usize,
    trust_marks_active: usize,
}

#[derive(Serialize)]
struct SitesResponse {
    sites: Vec<SiteJson>,
}

#[derive(Serialize, Deserialize)]
struct SiteJson {
    #[serde(default)]
    id: String,
    name: String,
    #[serde(default)]
    parent_id: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    lat: f64,
    #[serde(default)]
    lon: f64,
    #[serde(default)]
    metadata_json: String,
    #[serde(default)]
    environment_id: String,
}

#[derive(Serialize)]
struct EnvironmentsResponse {
    environments: Vec<EnvironmentJson>,
}

#[derive(Serialize)]
struct EnvironmentJson {
    id: String,
    name: String,
    archetype: String,
    created_at_ns: i64,
    metadata_json: String,
    site_count: i64,
    device_count: i64,
}

#[derive(Deserialize)]
struct CreateEnvironmentRequest {
    #[serde(default)]
    id: String,
    name: String,
    archetype: String,
    #[serde(default)]
    metadata_json: String,
}

#[derive(Deserialize)]
struct UpdateEnvironmentRequest {
    id: String,
    name: String,
    archetype: String,
    #[serde(default)]
    metadata_json: String,
}

#[derive(Deserialize)]
struct RemoveEnvironmentRequest {
    id: String,
}

#[derive(Deserialize)]
struct AssignSiteEnvironmentRequest {
    site_id: String,
    environment_id: String,
}

#[derive(Serialize)]
struct EnvironmentMutationResponse {
    success: bool,
    error: String,
}

#[derive(Serialize)]
struct SetupStatusResponse {
    is_first_run: bool,
    has_environments: bool,
    has_credentials: bool,
    has_devices: bool,
}

#[derive(Serialize)]
struct SiteMutationResponse {
    success: bool,
    error: String,
    site: Option<SiteJson>,
}

#[derive(Deserialize)]
struct RemoveSiteRequest {
    id: String,
}

#[derive(Serialize)]
struct SiteSummaryResponse {
    site: SiteJson,
    child_site_count: usize,
    device_count: usize,
    health: SiteHealthJson,
    subscription_summary: SiteSubscriptionSummaryJson,
    devices: Vec<SiteDeviceJson>,
    recent_detections: Vec<DetectionRow>,
}

#[derive(Serialize, Default)]
struct SiteHealthJson {
    healthy: usize,
    warn: usize,
    critical: usize,
}

#[derive(Serialize, Default)]
struct SiteSubscriptionSummaryJson {
    observed: usize,
    pending: usize,
    silent: usize,
}

#[derive(Serialize)]
struct SiteDeviceJson {
    address: String,
    hostname: String,
    vendor: String,
    role: String,
    collector_id: String,
    health: String,
}

#[derive(Serialize)]
struct CredentialsResponse {
    credentials: Vec<CredentialJson>,
    unlocked: bool,
}

#[derive(Serialize)]
struct CredentialJson {
    alias: String,
    created_at_ns: i64,
    updated_at_ns: i64,
    last_used_at_ns: i64,
    device_count: usize,
}

#[derive(Deserialize)]
struct AddCredentialRequest {
    alias: String,
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct RemoveCredentialRequest {
    alias: String,
}

#[derive(Deserialize)]
struct TestCredentialRequest {
    alias: String,
    address: String,
    #[serde(default)]
    tls_domain: String,
    #[serde(default)]
    ca_cert_path: String,
    #[serde(default)]
    role_hint: String,
}

#[derive(Serialize)]
struct CredentialMutationResponse {
    success: bool,
    error: String,
    credential: Option<CredentialJson>,
}

#[derive(Serialize)]
struct MutationResponse {
    success: bool,
    error: String,
    device: Option<ManagedDeviceJson>,
}

#[derive(Serialize)]
struct SsePayload {
    device_address: String,
    event_type: String,
    detail_json: String,
    occurred_at_ns: i64,
    state_change_event_id: String,
}

// ── Query params ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct DetectionsParams {
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    50
}

fn default_enabled() -> bool {
    true
}

// ── App state ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<GraphStore>,
    pub registry: Arc<ApiRegistry>,
    pub credentials: Arc<CredentialVault>,
    pub change_detection: Arc<ChangeDetectionRuntime>,
    pub collector_manager: Option<Arc<CollectorManager>>,
    pub catalogue: Arc<RwLock<CatalogueState>>,
    pub catalogue_dir: String,
    pub enricher_registry: SharedEnricherRegistry,
    pub adapter_registry: SharedAdapterRegistry,
    pub trust_store: SharedTrustStore,
    pub rollback_registry: SharedRollbackRegistry,
    pub remediation_config: RemediationConfig,
    pub servicenow_config: ServiceNowConfig,
    pub runtime_dir: String,
    pub archive_path: String,
    pub graph_path: String,
    pub storage_config: StorageConfig,
    pub layered_ingestion: LayeredIngestionConfig,
    pub streaming: StreamingConfig,
    pub yang_library_root: String,
    pub yang_cache_root: String,
    pub yang_bundle_key_env: String,
    /// Counter ingest mode for operations visibility (C-9 / T1-8).
    pub counter_mode: String,
    pub counter_window_secs: u64,
    pub counter_debounce_secs: u64,
    /// T4-5: Resource governance handle — None until governor is started (non-core modes).
    pub governor: Option<GovernorHandle>,
}

// ── Router ────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn router(
    store: Arc<GraphStore>,
    registry: Arc<ApiRegistry>,
    credentials: Arc<CredentialVault>,
    change_detection: Arc<ChangeDetectionRuntime>,
    collector_manager: Option<Arc<CollectorManager>>,
    catalogue: Arc<RwLock<CatalogueState>>,
    catalogue_dir: String,
    enricher_registry: SharedEnricherRegistry,
    adapter_registry: SharedAdapterRegistry,
    trust_store: SharedTrustStore,
    rollback_registry: SharedRollbackRegistry,
    remediation_config: RemediationConfig,
    servicenow_config: ServiceNowConfig,
    runtime_dir: String,
    archive_path: String,
    graph_path: String,
    storage_config: StorageConfig,
    layered_ingestion: LayeredIngestionConfig,
    streaming: StreamingConfig,
    yang_library_root: String,
    yang_cache_root: String,
    yang_bundle_key_env: String,
    counter_mode: String,
    counter_window_secs: u64,
    counter_debounce_secs: u64,
    governor: Option<GovernorHandle>,
) -> Router {
    let state = AppState {
        store,
        registry,
        credentials,
        change_detection,
        collector_manager,
        catalogue,
        catalogue_dir,
        enricher_registry,
        adapter_registry,
        trust_store,
        rollback_registry,
        remediation_config,
        servicenow_config,
        runtime_dir,
        archive_path,
        graph_path,
        storage_config,
        layered_ingestion,
        streaming,
        yang_library_root,
        yang_cache_root,
        yang_bundle_key_env,
        counter_mode,
        counter_window_secs,
        counter_debounce_secs,
        governor,
    };

    // Serve the Svelte SPA from ui/dist/. Fall back to index.html so
    // client-side routing works (the SPA handles /events and /trace/:id paths).
    let spa = ServeDir::new("ui/dist")
        .not_found_service(tower_http::services::ServeFile::new("ui/dist/index.html"));

    Router::new()
        .route("/api/topology", get(topology_handler))
        .route("/api/yang/modules", get(yang_modules_handler))
        .route("/api/yang/search", get(yang_search_handler))
        .route("/api/overrides", get(list_overrides).post(add_override))
        .route("/api/overrides/remove", post(remove_override))
        .route("/api/path", get(path_handler))
        .route("/api/blast-radius/{address}", get(blast_radius_handler))
        .route("/api/incidents/grouped", get(incidents_handler))
        .route(
            "/api/devices/{address}/gnmi-readiness",
            get(device_gnmi_readiness_handler),
        )
        .route(
            "/api/devices/{address}/streaming-readiness",
            get(device_streaming_readiness_handler),
        )
        .route(
            "/api/devices/{address}/recommendations",
            get(device_recommendations_handler),
        )
        .route(
            "/api/devices/{address}/selected-paths",
            post(apply_device_selected_paths_handler),
        )
        .route(
            "/api/devices/{address}/config-history",
            get(device_config_history_handler),
        )
        .route(
            "/api/devices/{address}/reparse",
            post(device_reparse_handler),
        )
        .route(
            "/api/onboarding/devices",
            get(managed_devices_handler).post(add_managed_device_handler),
        )
        .route(
            "/api/onboarding/devices/with_paths",
            post(add_managed_device_with_paths_handler),
        )
        .route(
            "/api/onboarding/devices/remove",
            post(remove_managed_device_handler),
        )
        .route(
            "/api/onboarding/devices/remove-impact",
            post(remove_impact_handler),
        )
        .route(
            "/api/onboarding/devices/bulk",
            post(bulk_managed_device_action_handler),
        )
        .route("/api/onboarding/discover", post(discover_handler))
        .route("/api/sites", get(sites_handler).post(upsert_site_handler))
        .route("/api/sites/{id}", get(site_summary_handler))
        .route("/api/sites/remove", post(remove_site_handler))
        .route(
            "/api/environments",
            get(environments_handler).post(create_environment_handler),
        )
        .route("/api/environments/update", post(update_environment_handler))
        .route("/api/environments/remove", post(remove_environment_handler))
        .route(
            "/api/environments/assign-site",
            post(assign_site_environment_handler),
        )
        .route("/api/setup/status", get(setup_status_handler))
        .route("/api/profiles", get(profiles_handler))
        .route(
            "/api/profiles/save-custom",
            post(save_custom_profile_handler),
        )
        .route(
            "/api/enrichment",
            get(enrichment_list_handler).post(enrichment_upsert_handler),
        )
        .route("/api/enrichment/remove", post(enrichment_remove_handler))
        .route("/api/enrichment/test", post(enrichment_test_handler))
        .route("/api/enrichment/run", post(enrichment_run_handler))
        .route("/api/enrichment/audit", get(enrichment_audit_handler))
        .route(
            "/api/adapters",
            get(adapter_list_handler).post(adapter_upsert_handler),
        )
        .route("/api/adapters/remove", post(adapter_remove_handler))
        .route("/api/adapters/test", post(adapter_test_handler))
        .route("/api/adapters/audit", get(adapter_audit_handler))
        .route(
            "/api/approvals",
            get(approvals_list_handler).post(approvals_create_handler),
        )
        .route(
            "/api/approvals/{id}/approve",
            post(approvals_approve_handler),
        )
        .route("/api/approvals/{id}/reject", post(approvals_reject_handler))
        .route(
            "/api/approvals/{id}/rollback",
            post(approvals_rollback_handler),
        )
        .route("/api/trust", get(trust_list_handler))
        .route("/api/trust/graduate", post(trust_graduate_handler))
        .route(
            "/api/integrations/servicenow/test",
            post(snow_integration_test_handler),
        )
        .route(
            "/api/integrations/servicenow/aiops/sync",
            post(servicenow_aiops_sync_handler),
        )
        .route(
            "/api/credentials",
            get(credentials_handler).post(add_credential_handler),
        )
        .route("/api/credentials/update", post(update_credential_handler))
        .route("/api/credentials/remove", post(remove_credential_handler))
        .route("/api/credentials/test", post(test_credential_handler))
        .route("/api/detections", get(detections_handler))
        .route("/api/incidents", get(incidents_handler))
        .route("/api/readiness", get(readiness_handler))
        .route("/api/operations", get(operations_handler))
        .route("/api/operations/daily-check", get(daily_check_handler))
        .route("/api/operations/weekly-trend", get(weekly_trend_handler))
        .route("/api/governance/state", get(governance_state_handler))
        .route("/api/_test/status", get(test_status_handler))
        .route(
            "/api/_test/inject_detection",
            post(inject_detection_handler),
        )
        .route(
            "/api/_test/syslog/parse",
            post(parse_syslog_fixture_handler),
        )
        .route("/api/trace/{id}", get(trace_handler))
        .route("/api/events", get(events_handler))
        .route("/api/devices/{address}", get(device_detail_handler))
        .route(
            "/api/devices/{address}/enrichment",
            get(device_enrichment_handler),
        )
        .route("/api/collectors", get(collectors_handler))
        .route(
            "/api/assignment/rules",
            get(assignment_rules_handler).post(set_assignment_rules_handler),
        )
        .route("/api/assignment/status", get(assignment_status_handler))
        .route(
            "/api/assignment/override",
            post(assignment_override_handler),
        )
        // graph insights + explorer (T1-4, T1-5, T1-6)
        .route("/api/graph/insights", get(graph_insights_handler))
        .route("/api/explorer/query", post(explorer_query_handler))
        .route(
            "/api/explorer/saved-queries",
            get(list_saved_queries_handler).post(create_saved_query_handler),
        )
        .route(
            "/api/explorer/saved-queries/{id}/delete",
            post(delete_saved_query_handler),
        )
        // investigations (T3-1/T3-2)
        .route(
            "/api/investigations",
            get(list_investigations_handler).post(create_investigation_handler),
        )
        .route("/api/investigations/{id}", get(get_investigation_handler))
        .route(
            "/api/investigations/{id}/tool-calls",
            get(list_tool_calls_handler),
        )
        .route(
            "/api/investigations/{id}/complete",
            post(complete_investigation_handler),
        )
        // graph embeddings (T2-1)
        .route(
            "/api/graph/embeddings/upsert",
            post(upsert_embeddings_handler),
        )
        .route(
            "/api/graph/embeddings/{address}",
            get(list_embeddings_handler),
        )
        // T5-1: MCP server (agent-friendly JSON-RPC endpoint)
        .route("/mcp", post(crate::mcp_server::mcp_handler))
        // T5-2: Grounded incident response
        .route(
            "/api/incidents/{id}/grounded",
            get(grounded_incident_handler),
        )
        // T5-3: Self-describing OpenAPI schema
        .route("/api/schema", get(schema_handler))
        // T5-5: Natural-language reference resolution
        .route("/api/resolve", get(resolve_handler))
        // CV6 T1-2: Swagger UI + canonical spec endpoint
        .route("/api/docs", get(swagger_ui_handler))
        .route("/api/openapi.json", get(openapi_json_handler))
        .fallback_service(spa)
        .with_state(state)
        .layer(CorsLayer::permissive())
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn topology_handler(
    State(state): State<AppState>,
) -> Result<Json<TopologyResponse>, (StatusCode, String)> {
    let db = state.store.db();

    let (devices_raw, links_raw, bgp_raw) = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;

        // Devices
        let dev_rows = conn
            .query("MATCH (d:Device) RETURN d.address, d.vendor, d.hostname")
            .map_err(|e| e.to_string())?;
        let devices_raw: Vec<(String, String, String)> = dev_rows
            .map(|row| (read_str(&row[0]), read_str(&row[1]), read_str(&row[2])))
            .collect();

        // LLDP fabric links with interface counter totals for heatmap
        let link_rows = conn
            .query(
                "MATCH (a:Interface)-[:CONNECTED_TO]->(b:Interface) \
                 RETURN a.device_address, a.name, b.device_address, b.name, \
                        a.in_octets + a.out_octets + b.in_octets + b.out_octets",
            )
            .map_err(|e| e.to_string())?;
        let links_raw: Vec<(String, String, String, String, i64, bool)> = link_rows
            .map(|row| {
                (
                    read_str(&row[0]),
                    read_str(&row[1]),
                    read_str(&row[2]),
                    read_str(&row[3]),
                    read_i64(&row[4]),
                    false,
                )
            })
            .collect();

        // Management-plane LLDP links (out-of-band; hidden by default in UI)
        let mgmt_rows = conn
            .query(
                "MATCH (a:Interface)-[:MGMT_LINK]->(b:Interface) \
                 RETURN a.device_address, a.name, b.device_address, b.name",
            )
            .map_err(|e| e.to_string())?;
        let mgmt_raw: Vec<(String, String, String, String, i64, bool)> = mgmt_rows
            .map(|row| {
                (
                    read_str(&row[0]),
                    read_str(&row[1]),
                    read_str(&row[2]),
                    read_str(&row[3]),
                    0i64,
                    true,
                )
            })
            .collect();
        let links_raw: Vec<(String, String, String, String, i64, bool)> =
            links_raw.into_iter().chain(mgmt_raw).collect();

        // BGP neighbors
        let bgp_rows = conn
            .query(
                "MATCH (n:BgpNeighbor) \
                 RETURN n.device_address, n.peer_address, n.session_state, n.peer_as",
            )
            .map_err(|e| e.to_string())?;
        let bgp_raw: Vec<(String, String, String, i64)> = bgp_rows
            .map(|row| {
                (
                    read_str(&row[0]),
                    read_str(&row[1]),
                    read_str(&row[2]),
                    read_i64(&row[3]),
                )
            })
            .collect();

        Ok::<_, String>((devices_raw, links_raw, bgp_raw))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let all_sites = state.store.list_sites().await.unwrap_or_else(|error| {
        tracing::warn!(%error, "failed to list sites for topology metadata");
        Vec::new()
    });
    let site_id_by_name: HashMap<String, String> = all_sites
        .iter()
        .map(|site| (site.name.clone(), site.id.clone()))
        .collect();
    let site_path_by_id = build_site_path_by_id(&all_sites);

    // Build role + site map from registry
    let mut role_site: HashMap<String, (String, String, String, String)> = HashMap::new();
    if let Ok(targets) = state.registry.list_all_targets() {
        for t in targets {
            let site = t.site.unwrap_or_default();
            let (site_id, site_path) =
                resolve_site_metadata(&site, &site_id_by_name, &site_path_by_id);
            role_site.insert(
                t.address.clone(),
                (t.role.unwrap_or_default(), site, site_id, site_path),
            );
        }
    }

    // Group BGP by device
    let mut bgp_by_device: HashMap<String, Vec<BgpJson>> = HashMap::new();
    for (dev, peer, st, peer_as) in bgp_raw {
        bgp_by_device.entry(dev).or_default().push(BgpJson {
            peer,
            state: st,
            peer_as,
        });
    }

    // Build device list with computed health + registry metadata
    let devices: Vec<DeviceJson> = devices_raw
        .into_iter()
        .map(|(address, vendor, hostname)| {
            let bgp = bgp_by_device.remove(&address).unwrap_or_default();
            let health = compute_health(&bgp);
            let (role, site, site_id, site_path) = role_site.remove(&address).unwrap_or_default();
            DeviceJson {
                address,
                hostname,
                vendor,
                role,
                site,
                site_id,
                site_path,
                health,
                bgp,
            }
        })
        .collect();

    let links = links_raw
        .into_iter()
        .map(
            |(src_device, src_iface, dst_device, dst_iface, bytes_total, is_mgmt)| LinkJson {
                src_device,
                src_iface,
                dst_device,
                dst_iface,
                bytes_total,
                is_mgmt,
            },
        )
        .collect();

    Ok(Json(TopologyResponse {
        schema_version: API_SCHEMA_VERSION.to_string(),
        devices,
        links,
    }))
}

#[derive(Deserialize)]
struct PathParams {
    src: String,
    dst: String,
}

/// Shortest path between two devices, computed in the graph database.
///
/// Replaces Rust-side BFS that loaded all CONNECTED_TO edges into a Vec.
/// The graph DB traverses HAS_INTERFACE|CONNECTED_TO edges with a variable-
/// length pattern and returns a single path; no edge-loading into Rust.
async fn path_handler(
    State(state): State<AppState>,
    Query(params): Query<PathParams>,
) -> Result<Json<PathResponse>, (StatusCode, String)> {
    let db = state.store.db();
    let (src, dst) = (params.src.clone(), params.dst.clone());

    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::queries::shortest_topology_path(&conn, &src, &dst).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    match result {
        None => Ok(Json(PathResponse {
            schema_version: API_SCHEMA_VERSION.to_string(),
            hops: vec![],
            links: vec![],
        })),
        Some(path) => Ok(Json(PathResponse {
            schema_version: API_SCHEMA_VERSION.to_string(),
            hops: path.hops,
            links: path.links,
        })),
    }
}

// ─── blast radius ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct BlastRadiusParams {
    #[serde(default = "default_max_hops")]
    max_hops: usize,
}

fn default_max_hops() -> usize {
    2
}

/// Devices, applications, and active detections reachable from `address` within
/// `max_hops` physical network hops.
///
/// Example: GET /api/blast-radius/10.0.0.1?max_hops=2
async fn blast_radius_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(params): Query<BlastRadiusParams>,
) -> Result<Json<crate::graph::queries::BlastRadiusResult>, (StatusCode, String)> {
    let db = state.store.db();
    let max_hops = params.max_hops.min(5); // cap at 5 to bound query time

    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::queries::blast_radius(&conn, &address, max_hops).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn managed_devices_handler(
    State(state): State<AppState>,
) -> Result<Json<ManagedDevicesResponse>, (StatusCode, String)> {
    let targets = state
        .registry
        .list_active()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let statuses = read_subscription_statuses(state.store.db()).await?;

    let overrides = state.registry.list_overrides().unwrap_or_default();
    let devices = targets
        .into_iter()
        .map(|target| managed_device_json(target, &statuses, &overrides))
        .collect();

    Ok(Json(ManagedDevicesResponse { devices }))
}

async fn discover_handler(
    State(state): State<AppState>,
    Json(req): Json<OnboardingDiscoveryRequest>,
) -> Result<Json<discovery::DiscoveryReport>, (StatusCode, String)> {
    let credentials = resolve_request_credentials(
        &state.credentials,
        option_string(req.credential_alias),
        option_string(req.username_env),
        option_string(req.password_env),
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    let (username, password) = match credentials {
        Some(credentials) => (Some(credentials.username), Some(credentials.password)),
        None => (None, None),
    };

    let report = discovery::discover_device(DiscoveryInput {
        address: req.address,
        username,
        password,
        username_env: None,
        password_env: None,
        ca_cert_path: option_string(req.ca_cert_path),
        tls_domain: option_string(req.tls_domain),
        role_hint: option_string(req.role_hint),
        environment_archetype: option_string(req.environment_archetype),
    })
    .await
    .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    Ok(Json(report))
}

async fn credentials_handler(
    State(state): State<AppState>,
) -> Result<Json<CredentialsResponse>, (StatusCode, String)> {
    let device_counts = credential_device_counts(&state.registry)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let credentials = state
        .credentials
        .list()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?
        .into_iter()
        .map(|credential| credential_json(credential, &device_counts))
        .collect();
    let unlocked = state
        .credentials
        .is_unlocked()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(CredentialsResponse {
        credentials,
        unlocked,
    }))
}

async fn add_credential_handler(
    State(state): State<AppState>,
    Json(req): Json<AddCredentialRequest>,
) -> Result<Json<CredentialMutationResponse>, (StatusCode, String)> {
    match state
        .credentials
        .add(&req.alias, &req.username, &req.password)
    {
        Ok(credential) => {
            let device_counts = credential_device_counts(&state.registry)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            Ok(Json(CredentialMutationResponse {
                success: true,
                error: String::new(),
                credential: Some(credential_json(credential, &device_counts)),
            }))
        }
        Err(error) => Ok(Json(CredentialMutationResponse {
            success: false,
            error: format!("{error:#}"),
            credential: None,
        })),
    }
}

async fn update_credential_handler(
    State(state): State<AppState>,
    Json(req): Json<AddCredentialRequest>,
) -> Result<Json<CredentialMutationResponse>, (StatusCode, String)> {
    let username = if req.username.trim().is_empty() {
        state
            .credentials
            .username_for_alias(&req.alias)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?
    } else {
        req.username.clone()
    };
    match state
        .credentials
        .update(&req.alias, &username, &req.password)
    {
        Ok(credential) => {
            let device_counts = credential_device_counts(&state.registry)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            Ok(Json(CredentialMutationResponse {
                success: true,
                error: String::new(),
                credential: Some(credential_json(credential, &device_counts)),
            }))
        }
        Err(error) => Ok(Json(CredentialMutationResponse {
            success: false,
            error: format!("{error:#}"),
            credential: None,
        })),
    }
}

async fn remove_credential_handler(
    State(state): State<AppState>,
    Json(req): Json<RemoveCredentialRequest>,
) -> Result<Json<CredentialMutationResponse>, (StatusCode, String)> {
    let device_counts = credential_device_counts(&state.registry)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    if device_counts
        .get(req.alias.trim())
        .copied()
        .unwrap_or_default()
        > 0
    {
        return Ok(Json(CredentialMutationResponse {
            success: false,
            error: format!(
                "credential alias '{}' is still referenced by {} device(s)",
                req.alias.trim(),
                device_counts
                    .get(req.alias.trim())
                    .copied()
                    .unwrap_or_default()
            ),
            credential: None,
        }));
    }
    match state.credentials.remove(&req.alias) {
        Ok(Some(credential)) => Ok(Json(CredentialMutationResponse {
            success: true,
            error: String::new(),
            credential: Some(credential_json(credential, &device_counts)),
        })),
        Ok(None) => Ok(Json(CredentialMutationResponse {
            success: false,
            error: format!("credential alias '{}' not found", req.alias),
            credential: None,
        })),
        Err(error) => Ok(Json(CredentialMutationResponse {
            success: false,
            error: format!("{error:#}"),
            credential: None,
        })),
    }
}

async fn test_credential_handler(
    State(state): State<AppState>,
    Json(req): Json<TestCredentialRequest>,
) -> Result<Json<discovery::DiscoveryReport>, (StatusCode, String)> {
    let credentials = state
        .credentials
        .resolve(&req.alias, ResolvePurpose::Test)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    let report = discovery::discover_device(DiscoveryInput {
        address: req.address,
        username: Some(credentials.username),
        password: Some(credentials.password),
        username_env: None,
        password_env: None,
        ca_cert_path: option_string(req.ca_cert_path),
        tls_domain: option_string(req.tls_domain),
        role_hint: option_string(req.role_hint),
        environment_archetype: None,
    })
    .await
    .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    Ok(Json(report))
}

async fn add_managed_device_handler(
    State(state): State<AppState>,
    Json(req): Json<ManagedDeviceRequest>,
) -> Result<Json<MutationResponse>, (StatusCode, String)> {
    save_managed_device(state, req).await
}

async fn add_managed_device_with_paths_handler(
    State(state): State<AppState>,
    Json(req): Json<ManagedDeviceRequest>,
) -> Result<Json<MutationResponse>, (StatusCode, String)> {
    if req.selected_paths.is_empty() {
        return Ok(Json(MutationResponse {
            success: false,
            error: "selected_paths is required for /api/onboarding/devices/with_paths".to_string(),
            device: None,
        }));
    }
    save_managed_device(state, req).await
}

async fn save_managed_device(
    state: AppState,
    req: ManagedDeviceRequest,
) -> Result<Json<MutationResponse>, (StatusCode, String)> {
    let mut target = target_from_request(req)?;
    if let Ok(Some(existing)) = state.registry.get_device(&target.address) {
        if target.credential_alias.is_none() {
            target.credential_alias = existing.credential_alias;
        }
        if target.username_env.is_none() {
            target.username_env = existing.username_env;
        }
        if target.password_env.is_none() {
            target.password_env = existing.password_env;
        }
        if target.username.is_none() {
            target.username = existing.username;
        }
        if target.password.is_none() {
            target.password = existing.password;
        }
        if target.selected_paths.is_empty() {
            target.selected_paths = existing.selected_paths;
        }
    }
    let address = target.address.clone();
    let result = match state
        .registry
        .add_device_with_audit(target.clone(), "api", "api_add_device")
    {
        Ok(device) => Ok(device),
        Err(add_error) => match state.registry.get_device(&address) {
            Ok(Some(_)) => state
                .registry
                .update_device_with_audit(target, "api", "api_update_device")
                .map_err(|update_error| {
                    format!("add failed: {add_error:#}; update failed: {update_error:#}")
                }),
            _ => Err(add_error.to_string()),
        },
    };

    match result {
        Ok(device) => {
            if let Err(error) = state
                .store
                .sync_sites_from_targets(vec![device.clone()])
                .await
            {
                return Ok(Json(MutationResponse {
                    success: false,
                    error: format!("device saved but site graph sync failed: {error:#}"),
                    device: Some(managed_device_json(
                        device,
                        &HashMap::new(),
                        &state.registry.list_overrides().unwrap_or_default(),
                    )),
                }));
            }
            let statuses = read_subscription_statuses(state.store.db()).await?;
            Ok(Json(MutationResponse {
                success: true,
                error: String::new(),
                device: Some(managed_device_json(
                    device,
                    &statuses,
                    &state.registry.list_overrides().unwrap_or_default(),
                )),
            }))
        }
        Err(error) => Ok(Json(MutationResponse {
            success: false,
            error,
            device: None,
        })),
    }
}

async fn sites_handler(
    State(state): State<AppState>,
) -> Result<Json<SitesResponse>, (StatusCode, String)> {
    let sites = state
        .store
        .list_sites()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?
        .into_iter()
        .map(site_json)
        .collect();
    Ok(Json(SitesResponse { sites }))
}

async fn upsert_site_handler(
    State(state): State<AppState>,
    Json(req): Json<SiteJson>,
) -> Result<Json<SiteMutationResponse>, (StatusCode, String)> {
    match state.store.upsert_site(site_record(req)).await {
        Ok(site) => Ok(Json(SiteMutationResponse {
            success: true,
            error: String::new(),
            site: Some(site_json(site)),
        })),
        Err(error) => Ok(Json(SiteMutationResponse {
            success: false,
            error: format!("{error:#}"),
            site: None,
        })),
    }
}

async fn site_summary_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SiteSummaryResponse>, (StatusCode, String)> {
    let all_sites = state
        .store
        .list_sites()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let site = all_sites
        .iter()
        .find(|site| site.id == id)
        .cloned()
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("site '{id}' not found")))?;

    let subtree_ids = site_subtree_ids(&all_sites, &site.id);
    let subtree_names: std::collections::HashSet<String> = all_sites
        .iter()
        .filter(|candidate| subtree_ids.contains(&candidate.id))
        .map(|candidate| candidate.name.clone())
        .collect();

    let targets = state
        .registry
        .list_all_targets()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let site_targets: Vec<TargetConfig> = targets
        .into_iter()
        .filter(|target| {
            target
                .site
                .as_ref()
                .map(|site_name| subtree_names.contains(site_name))
                .unwrap_or(false)
        })
        .collect();
    let device_addresses: std::collections::HashSet<String> = site_targets
        .iter()
        .map(|target| target.address.clone())
        .collect();

    let db = state.store.db();
    let bgp_rows = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let rows = conn
            .query(
                "MATCH (n:BgpNeighbor) \
                 RETURN n.device_address, n.session_state",
            )
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(
            rows.map(|row| (read_str(&row[0]), read_str(&row[1])))
                .collect::<Vec<_>>(),
        )
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut bgp_by_device: HashMap<String, Vec<BgpJson>> = HashMap::new();
    for (address, session_state) in bgp_rows {
        if !device_addresses.contains(&address) {
            continue;
        }
        bgp_by_device.entry(address).or_default().push(BgpJson {
            peer: String::new(),
            state: session_state,
            peer_as: 0,
        });
    }

    let mut health = SiteHealthJson::default();
    let devices = site_targets
        .iter()
        .map(|target| {
            let device_health = compute_health(
                bgp_by_device
                    .get(&target.address)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            );
            match device_health.as_str() {
                "healthy" => health.healthy += 1,
                "warn" => health.warn += 1,
                _ => health.critical += 1,
            }
            SiteDeviceJson {
                address: target.address.clone(),
                hostname: target.hostname.clone().unwrap_or_default(),
                vendor: target.vendor.clone().unwrap_or_default(),
                role: target.role.clone().unwrap_or_default(),
                collector_id: target.collector_id.clone().unwrap_or_default(),
                health: device_health,
            }
        })
        .collect::<Vec<_>>();

    let all_statuses = read_subscription_statuses(state.store.db()).await?;
    let mut subscription_summary = SiteSubscriptionSummaryJson::default();
    for address in &device_addresses {
        for status in all_statuses.get(address).cloned().unwrap_or_default() {
            match status.status.as_str() {
                "observed" => subscription_summary.observed += 1,
                "pending" => subscription_summary.pending += 1,
                _ => subscription_summary.silent += 1,
            }
        }
    }

    let recent_detections = state
        .store
        .read_detections(100)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .into_iter()
        .filter(|row| device_addresses.contains(&row.device_address))
        .take(10)
        .collect();

    let child_site_count = all_sites
        .iter()
        .filter(|candidate| candidate.parent_id == site.id)
        .count();

    Ok(Json(SiteSummaryResponse {
        site: site_json(site),
        child_site_count,
        device_count: devices.len(),
        health,
        subscription_summary,
        devices,
        recent_detections,
    }))
}

async fn remove_site_handler(
    State(state): State<AppState>,
    Json(req): Json<RemoveSiteRequest>,
) -> Result<Json<SiteMutationResponse>, (StatusCode, String)> {
    let all_sites = state
        .store
        .list_sites()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let site = match all_sites.iter().find(|site| site.id == req.id).cloned() {
        Some(site) => site,
        None => {
            return Ok(Json(SiteMutationResponse {
                success: false,
                error: format!("site '{}' not found", req.id),
                site: None,
            }));
        }
    };
    if all_sites
        .iter()
        .any(|candidate| candidate.parent_id == site.id)
    {
        return Ok(Json(SiteMutationResponse {
            success: false,
            error: "cannot delete a site that still has child sites".to_string(),
            site: None,
        }));
    }

    let in_use = state
        .registry
        .list_all_targets()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?
        .into_iter()
        .filter(|target| target.site.as_deref() == Some(site.name.as_str()))
        .count();
    if in_use > 0 {
        return Ok(Json(SiteMutationResponse {
            success: false,
            error: format!(
                "cannot delete site '{}' while {} device(s) still reference it",
                site.name, in_use
            ),
            site: None,
        }));
    }

    let db = state.store.db();
    let site_id = site.id.clone();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("MATCH (s:Site {id: $id}) DETACH DELETE s")
            .map_err(|e| e.to_string())?;
        conn.execute(&mut stmt, vec![("id", Value::String(site_id))])
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(SiteMutationResponse {
        success: true,
        error: String::new(),
        site: Some(site_json(site)),
    }))
}

async fn remove_managed_device_handler(
    State(state): State<AppState>,
    Json(req): Json<RemoveManagedDeviceRequest>,
) -> Result<Json<MutationResponse>, (StatusCode, String)> {
    match state.registry.remove_device(&req.address) {
        Ok(Some(device)) => Ok(Json(MutationResponse {
            success: true,
            error: String::new(),
            device: Some(managed_device_json(
                device,
                &HashMap::new(),
                &state.registry.list_overrides().unwrap_or_default(),
            )),
        })),
        Ok(None) => Ok(Json(MutationResponse {
            success: false,
            error: format!("device '{}' not found", req.address),
            device: None,
        })),
        Err(error) => Ok(Json(MutationResponse {
            success: false,
            error: error.to_string(),
            device: None,
        })),
    }
}

async fn bulk_managed_device_action_handler(
    State(state): State<AppState>,
    Json(req): Json<BulkManagedDeviceActionRequest>,
) -> Result<Json<BulkManagedDeviceActionResponse>, (StatusCode, String)> {
    if req.addresses.is_empty() {
        return Ok(Json(BulkManagedDeviceActionResponse {
            success: false,
            error: "at least one address is required".to_string(),
            devices: Vec::new(),
        }));
    }

    let action = req.action.trim().to_ascii_lowercase();
    if !matches!(action.as_str(), "stop" | "start" | "restart") {
        return Ok(Json(BulkManagedDeviceActionResponse {
            success: false,
            error: "action must be one of: stop, start, restart".to_string(),
            devices: Vec::new(),
        }));
    }

    let statuses = read_subscription_statuses(state.store.db()).await?;
    let mut devices = Vec::new();
    let mut errors = Vec::new();
    for address in req.addresses {
        match state.registry.get_device(&address) {
            Ok(Some(mut target)) => {
                target.enabled = action != "stop";
                match state.registry.update_device_with_audit(
                    target,
                    "api",
                    &format!("api_bulk_{action}"),
                ) {
                    Ok(device) => devices.push(managed_device_json(
                        device,
                        &statuses,
                        &state.registry.list_overrides().unwrap_or_default(),
                    )),
                    Err(error) => errors.push(format!("{address}: {error:#}")),
                }
            }
            Ok(None) => errors.push(format!("{address}: device not found")),
            Err(error) => errors.push(format!("{address}: {error:#}")),
        }
    }

    Ok(Json(BulkManagedDeviceActionResponse {
        success: errors.is_empty(),
        error: errors.join("; "),
        devices,
    }))
}

async fn remove_impact_handler(
    State(state): State<AppState>,
    Json(req): Json<RemoveManagedDeviceRequest>,
) -> Result<Json<RemoveImpactResponse>, (StatusCode, String)> {
    let statuses = read_subscription_statuses(state.store.db()).await?;
    let device_statuses = statuses.get(&req.address).cloned().unwrap_or_default();
    let subscription_total = device_statuses.len();
    let subscription_observed = device_statuses
        .iter()
        .filter(|status| status.status == "observed")
        .count();
    let subscription_pending = device_statuses
        .iter()
        .filter(|status| status.status == "pending")
        .count();

    let (trust_marks_total, trust_marks_active) =
        read_trust_mark_impact(state.store.db(), req.address.clone()).await?;

    Ok(Json(RemoveImpactResponse {
        address: req.address,
        subscription_total,
        subscription_observed,
        subscription_pending,
        trust_marks_total,
        trust_marks_active,
    }))
}

async fn detections_handler(
    State(state): State<AppState>,
    Query(params): Query<DetectionsParams>,
) -> Result<Json<DetectionsResponse>, (StatusCode, String)> {
    let detections = state
        .store
        .read_detections(params.limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(DetectionsResponse {
        schema_version: API_SCHEMA_VERSION.to_string(),
        detections,
    }))
}

async fn trace_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TraceResponse>, (StatusCode, String)> {
    let steps = state
        .store
        .read_closed_loop_trace(id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(TraceResponse {
        schema_version: API_SCHEMA_VERSION.to_string(),
        steps,
    }))
}

async fn readiness_handler(
    State(state): State<AppState>,
) -> Result<Json<ReadinessResponse>, (StatusCode, String)> {
    let db = state.store.db();

    let readiness = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;

        let detection_rows = conn
            .query("MATCH (e:DetectionEvent) RETURN e.rule_id")
            .map_err(|e| e.to_string())?;
        let mut detection_events = 0usize;
        let mut rule_distribution: HashMap<String, usize> = HashMap::new();
        for row in detection_rows {
            detection_events += 1;
            let rule_id = read_str(&row[0]);
            if !rule_id.is_empty() {
                *rule_distribution.entry(rule_id).or_insert(0) += 1;
            }
        }

        let state_rows = conn
            .query("MATCH (e:StateChangeEvent) RETURN count(e)")
            .map_err(|e| e.to_string())?;
        let mut state_change_events = 0usize;
        for row in state_rows {
            state_change_events = read_i64(&row[0]).max(0) as usize;
        }

        let remediation_rows = conn
            .query(
                "MATCH (m:RemediationTrustMark)-[:TRUST_MARKS]->(r:Remediation) \
                 WHERE m.trustworthy = 1 \
                 RETURN r.action, r.status",
            )
            .map_err(|e| e.to_string())?;
        let mut remediation_rows_post_cutoff = 0usize;
        let mut action_distribution_post_cutoff: HashMap<String, usize> = HashMap::new();
        let mut status_distribution_post_cutoff: HashMap<String, usize> = HashMap::new();
        for row in remediation_rows {
            remediation_rows_post_cutoff += 1;

            let action = read_str(&row[0]);
            if !action.is_empty() {
                *action_distribution_post_cutoff.entry(action).or_insert(0) += 1;
            }

            let status = read_str(&row[1]);
            if !status.is_empty() {
                *status_distribution_post_cutoff.entry(status).or_insert(0) += 1;
            }
        }

        Ok::<_, String>(ReadinessResponse {
            schema_version: API_SCHEMA_VERSION.to_string(),
            detection_events,
            state_change_events,
            rule_distribution,
            cutoff_iso: REMEDIATION_TRUST_CUTOFF_ISO.to_string(),
            remediation_rows_post_cutoff,
            action_distribution_post_cutoff,
            status_distribution_post_cutoff,
        })
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(readiness))
}

async fn operations_handler(
    State(state): State<AppState>,
) -> Result<Json<OperationsResponse>, (StatusCode, String)> {
    let readiness = readiness_handler(State(state.clone())).await?.0;
    let targets = state
        .registry
        .list_all_targets()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let statuses = read_subscription_statuses(state.store.db()).await?;

    let mut observed_subscriptions = 0usize;
    let mut pending_subscriptions = 0usize;
    let mut silent_subscriptions = 0usize;
    for rows in statuses.values() {
        for status in rows {
            match status.status.as_str() {
                "observed" => observed_subscriptions += 1,
                "pending" => pending_subscriptions += 1,
                _ => silent_subscriptions += 1,
            }
        }
    }

    let collector_summary = state
        .collector_manager
        .as_ref()
        .map(|manager| manager.collector_status_summary())
        .unwrap_or_else(|| crate::assignment::CollectorStatusSummary {
            collectors: Vec::new(),
            unassigned_devices: Vec::new(),
        });
    let bus_snapshot = event_bus::InProcessBus::snapshot();
    let archive_snapshot = archive::snapshot();
    let mem_snapshot = memory_profile::snapshot();
    let disk_snapshot = disk_guard::snapshot(
        std::path::Path::new(&state.archive_path),
        std::path::Path::new(&state.graph_path),
        &state.storage_config,
    );
    // Use the governor's profile-derived budget when available; the hardcoded
    // constant is a last-resort fallback for non-governed modes.
    let effective_rss_budget = state
        .governor
        .as_ref()
        .map(|g| g.snapshot().memory_budget_mb * 1024 * 1024)
        .unwrap_or(RSS_BUDGET_BYTES);

    Ok(Json(OperationsResponse {
        schema_version: API_SCHEMA_VERSION.to_string(),
        detection_events: readiness.detection_events,
        state_change_events: readiness.state_change_events,
        remediation_rows_post_cutoff: readiness.remediation_rows_post_cutoff,
        rule_distribution: readiness.rule_distribution,
        action_distribution_post_cutoff: readiness.action_distribution_post_cutoff,
        status_distribution_post_cutoff: readiness.status_distribution_post_cutoff,
        device_count: targets.len(),
        enabled_device_count: targets.iter().filter(|target| target.enabled).count(),
        observed_subscriptions,
        pending_subscriptions,
        silent_subscriptions,
        collectors_connected: collector_summary
            .collectors
            .iter()
            .filter(|collector| collector.connected)
            .count(),
        collectors_total: collector_summary.collectors.len(),
        unassigned_devices: collector_summary.unassigned_devices.len(),
        event_bus_depth: bus_snapshot.depth,
        event_bus_receivers: bus_snapshot.receivers,
        archive_lag_millis: archive_snapshot.lag_millis,
        archive_buffer_rows: archive_snapshot.buffer_rows,
        archive_last_flush_millis: archive_snapshot.last_flush_millis,
        archive_last_compression_ppm: archive_snapshot.last_compression_ppm,
        cutoff_iso: readiness.cutoff_iso,
        rss_bytes: mem_snapshot.rss_bytes,
        archive_disk_bytes: disk_snapshot.archive_bytes,
        archive_disk_pct: disk_snapshot.archive_pct,
        graph_disk_bytes: disk_snapshot.graph_bytes,
        graph_disk_pct: disk_snapshot.graph_pct,
        memory_budget_bytes: effective_rss_budget,
        memory_rss_pct_of_budget: if effective_rss_budget > 0 {
            (mem_snapshot.rss_bytes as f64 / effective_rss_budget as f64) * 100.0
        } else {
            0.0
        },
        counter_mode: state.counter_mode.clone(),
        counter_window_secs: state.counter_window_secs,
        counter_debounce_secs: state.counter_debounce_secs,
    }))
}

async fn test_status_handler(
    State(state): State<AppState>,
) -> Result<Json<TestStatusResponse>, (StatusCode, String)> {
    let mem = memory_profile::snapshot();
    let disk = disk_guard::snapshot(
        std::path::Path::new(&state.archive_path),
        std::path::Path::new(&state.graph_path),
        &state.storage_config,
    );

    let external_path = std::path::Path::new(&state.runtime_dir).join("external_status.json");
    let external: serde_json::Value = tokio::fs::read_to_string(&external_path)
        .await
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);

    let driver_dir = std::path::Path::new(&state.runtime_dir).join("driver_results");
    let mut driver_results = serde_json::Map::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&driver_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if let Ok(s) = tokio::fs::read_to_string(&path).await
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&s)
            {
                driver_results.insert(stem.to_string(), v);
            }
        }
    }

    let ts_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let effective_rss_budget = state
        .governor
        .as_ref()
        .map(|g| g.snapshot().memory_budget_mb * 1024 * 1024)
        .unwrap_or(RSS_BUDGET_BYTES);

    let mut budget_breaches: Vec<BudgetBreach> = Vec::new();
    if mem.rss_bytes > effective_rss_budget {
        budget_breaches.push(BudgetBreach {
            name: "rss_budget",
            current: mem.rss_bytes as f64,
            budget: effective_rss_budget as f64,
            unit: "bytes",
        });
    }
    if mem.write_coordinator_queue_pct >= COORDINATOR_QUEUE_BUDGET_PCT {
        budget_breaches.push(BudgetBreach {
            name: "write_coordinator_queue_budget",
            current: mem.write_coordinator_queue_pct as f64,
            budget: COORDINATOR_QUEUE_BUDGET_PCT as f64,
            unit: "percent",
        });
    }

    Ok(Json(TestStatusResponse {
        ts_unix,
        memory: mem,
        disk: DiskStatusJson {
            archive_bytes: disk.archive_bytes,
            archive_max_bytes: disk.archive_max_bytes,
            archive_pct: disk.archive_pct,
            graph_bytes: disk.graph_bytes,
            graph_max_bytes: disk.graph_max_bytes,
            graph_pct: disk.graph_pct,
        },
        budget_breaches,
        external,
        driver_results: serde_json::Value::Object(driver_results),
    }))
}

#[derive(Serialize)]
struct DailyCheckResponse {
    ts_unix: u64,
    status: String,
    counts: DailyCheckCounts,
    checks: Vec<DailyCheckItem>,
}

#[derive(Serialize)]
struct DailyCheckCounts {
    pass: usize,
    fail: usize,
    skip: usize,
    prereq_missing: usize,
}

#[derive(Serialize)]
struct DailyCheckItem {
    name: String,
    status: String,
    summary: String,
}

async fn daily_check_handler(
    State(state): State<AppState>,
) -> Result<Json<DailyCheckResponse>, (StatusCode, String)> {
    let driver_dir = std::path::Path::new(&state.runtime_dir).join("driver_results");
    let mut checks: Vec<DailyCheckItem> = Vec::new();
    let mut counts = DailyCheckCounts {
        pass: 0,
        fail: 0,
        skip: 0,
        prereq_missing: 0,
    };

    if let Ok(mut entries) = tokio::fs::read_dir(&driver_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            // Exclude daily.json — it is a derived meta-file (self-referential in aggregation)
            if path.file_name().and_then(|n| n.to_str()) == Some("daily.json") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            let (status, summary) = if let Ok(s) = tokio::fs::read_to_string(&path).await {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                    let st = v
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let sm = v
                        .get("summary")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();
                    (st, sm)
                } else {
                    ("fail".to_string(), "invalid json".to_string())
                }
            } else {
                ("fail".to_string(), "unreadable".to_string())
            };

            match status.as_str() {
                "pass" => counts.pass += 1,
                "fail" => counts.fail += 1,
                "prereq_missing" => counts.prereq_missing += 1,
                _ => counts.skip += 1,
            }
            checks.push(DailyCheckItem {
                name,
                status,
                summary,
            });
        }
    }

    checks.sort_by(|a, b| a.name.cmp(&b.name));

    let overall = if counts.fail > 0 {
        "fail"
    } else if counts.prereq_missing > 0 {
        "pass_with_caveats"
    } else if counts.pass == 0 {
        "warn"
    } else {
        "pass"
    };

    let ts_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    Ok(Json(DailyCheckResponse {
        ts_unix,
        status: overall.to_string(),
        counts,
        checks,
    }))
}

// ── /api/_test/inject_detection ───────────────────────────────────────────────

#[derive(Deserialize)]
struct InjectDetectionRequest {
    device_address: String,
    rule_id: String,
    #[serde(default = "default_inject_severity")]
    severity: String,
}

fn default_inject_severity() -> String {
    "info".to_string()
}

#[derive(Serialize)]
struct InjectDetectionResponse {
    detection_id: String,
    fired_at_ns: i64,
}

#[derive(Deserialize)]
struct ParseSyslogFixtureRequest {
    raw: String,
    vendor: String,
    #[serde(default = "default_syslog_transport")]
    transport: String,
    #[serde(default = "default_syslog_peer_addr")]
    peer_addr: String,
}

fn default_syslog_transport() -> String {
    "udp".to_string()
}

fn default_syslog_peer_addr() -> String {
    "127.0.0.1:5514".to_string()
}

#[derive(Serialize)]
struct ParseSyslogFixtureResponse {
    event: SyslogEvent,
    facts: Vec<SyslogFact>,
    config_change_trigger: bool,
}

async fn inject_detection_handler(
    State(state): State<AppState>,
    Json(req): Json<InjectDetectionRequest>,
) -> Result<Json<InjectDetectionResponse>, (StatusCode, String)> {
    let fired_at_ns = crate::graph::common::now_ns();
    let detection_id = state
        .store
        .write_detection(
            req.device_address,
            req.rule_id,
            req.severity,
            "{}".to_string(),
            fired_at_ns,
            String::new(),
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(InjectDetectionResponse {
        detection_id,
        fired_at_ns,
    }))
}

async fn parse_syslog_fixture_handler(
    State(state): State<AppState>,
    Json(req): Json<ParseSyslogFixtureRequest>,
) -> Result<Json<ParseSyslogFixtureResponse>, (StatusCode, String)> {
    let raw = req.raw.trim().to_string();
    let vendor = req.vendor.trim().to_string();
    if raw.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "raw syslog line is required".to_string(),
        ));
    }
    if vendor.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "vendor is required".to_string()));
    }

    let timestamp_ns = crate::graph::common::now_ns();
    let (event, facts) = crate::signals::syslog::parse_syslog_fixture(
        &state.layered_ingestion.syslog_patterns_path,
        &raw,
        &vendor,
        &req.transport,
        &req.peer_addr,
        timestamp_ns,
    );
    let config_change_trigger = crate::signals::syslog::matches_syslog_config_change_trigger(
        &state.layered_ingestion.syslog_patterns_path,
        &vendor,
        &event.message,
    );

    Ok(Json(ParseSyslogFixtureResponse {
        event,
        facts,
        config_change_trigger,
    }))
}

// ── /api/operations/weekly-trend ─────────────────────────────────────────────

#[derive(Serialize)]
struct WeeklyTrendDay {
    date: String,
    status: String,
    pass: u32,
    fail: u32,
    skip: u32,
    prereq_missing: u32,
}

#[derive(Serialize)]
struct WeeklyTrendResponse {
    days: Vec<WeeklyTrendDay>,
}

async fn weekly_trend_handler(State(state): State<AppState>) -> Json<WeeklyTrendResponse> {
    let driver_dir = std::path::Path::new(&state.runtime_dir).join("driver_results");
    let mut days: Vec<WeeklyTrendDay> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&driver_dir) {
        let mut files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let s = name.to_string_lossy().into_owned();
                s.starts_with("daily-") && s.ends_with(".json")
            })
            .collect();
        files.sort_by_key(|e| e.file_name());
        // Take last 7, preserving chronological order
        let start = files.len().saturating_sub(7);
        for entry in &files[start..] {
            if let Ok(contents) = std::fs::read_to_string(entry.path())
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&contents)
            {
                let date = v["environment"]["date_utc"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let status = v["status"].as_str().unwrap_or("unknown").to_string();
                let mut pass = 0u32;
                let mut fail = 0u32;
                let mut skip = 0u32;
                let mut prereq_missing = 0u32;
                if let Some(checks) = v["checks"].as_array() {
                    for c in checks {
                        match c["status"].as_str().unwrap_or("") {
                            "pass" | "pass_with_caveats" => pass += 1,
                            "fail" => fail += 1,
                            "skip" => skip += 1,
                            "prereq_missing" => prereq_missing += 1,
                            _ => {}
                        }
                    }
                }
                days.push(WeeklyTrendDay {
                    date,
                    status,
                    pass,
                    fail,
                    skip,
                    prereq_missing,
                });
            }
        }
    }

    Json(WeeklyTrendResponse { days })
}

async fn events_handler(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.store.subscribe_events();
    let registry_rx = state.registry.subscribe_changes();

    let graph_stream = BroadcastStream::new(rx).map(|item| {
        let data = match item {
            Ok(ev) => serde_json::to_string(&SsePayload {
                device_address: ev.device_address,
                event_type: ev.event_type,
                detail_json: ev.detail_json,
                occurred_at_ns: ev.occurred_at_ns,
                state_change_event_id: ev.state_change_event_id,
            })
            .unwrap_or_default(),
            // Receiver lagged (broadcast buffer full); send a heartbeat comment.
            Err(_) => return Ok(Event::default().comment("lag")),
        };
        Ok(Event::default().data(data))
    });

    let registry_stream = ReceiverStream::new(registry_rx).map(|change| {
        let data = serde_json::to_string(&registry_change_payload(change)).unwrap_or_default();
        Ok(Event::default().data(data))
    });

    let stream = futures::stream::select(graph_stream, registry_stream);

    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn registry_change_payload(change: RegistryChange) -> SsePayload {
    match change {
        RegistryChange::Added(target) => registry_target_payload("registry_added", target),
        RegistryChange::Updated(target) => registry_target_payload("registry_updated", target),
        RegistryChange::Removed(address) => SsePayload {
            device_address: address.clone(),
            event_type: "registry_removed".to_string(),
            detail_json: serde_json::json!({ "address": address }).to_string(),
            occurred_at_ns: now_ns(),
            state_change_event_id: String::new(),
        },
    }
}

fn registry_target_payload(event_type: &str, target: TargetConfig) -> SsePayload {
    let address = target.address.clone();
    SsePayload {
        device_address: address.clone(),
        event_type: event_type.to_string(),
        detail_json: serde_json::json!({
            "address": address,
            "enabled": target.enabled,
            "hostname": target.hostname.unwrap_or_default(),
            "vendor": target.vendor.unwrap_or_default(),
            "role": target.role.unwrap_or_default(),
            "site": target.site.unwrap_or_default(),
            "credential_alias": target.credential_alias.unwrap_or_default(),
            "selected_path_count": target.selected_paths.len(),
        })
        .to_string(),
        occurred_at_ns: now_ns(),
        state_change_event_id: String::new(),
    }
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn compute_health(bgp: &[BgpJson]) -> String {
    if bgp.is_empty() {
        return "healthy".into();
    }
    let established = bgp.iter().filter(|n| n.state == "established").count();
    if established == bgp.len() {
        "healthy".into()
    } else if established > 0 {
        "warn".into()
    } else {
        "critical".into()
    }
}

async fn read_subscription_statuses(
    db: Arc<lbug::Database>,
) -> Result<HashMap<String, Vec<SubscriptionStatusJson>>, (StatusCode, String)> {
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let rows = conn
            .query(
                "MATCH (s:SubscriptionStatus) \
                 RETURN s.device_address, s.path, s.origin, s.mode, s.sample_interval_ns, \
                        s.status, s.first_observed_at, s.last_observed_at, s.updated_at \
                 ORDER BY s.device_address, s.path",
            )
            .map_err(|e| e.to_string())?;

        let mut by_device: HashMap<String, Vec<SubscriptionStatusJson>> = HashMap::new();
        for row in rows {
            by_device
                .entry(read_str(&row[0]))
                .or_default()
                .push(SubscriptionStatusJson {
                    path: read_str(&row[1]),
                    origin: read_str(&row[2]),
                    mode: read_str(&row[3]),
                    sample_interval_ns: read_i64(&row[4]),
                    status: read_str(&row[5]),
                    first_observed_at_ns: read_ts_ns(&row[6]),
                    last_observed_at_ns: read_ts_ns(&row[7]),
                    updated_at_ns: read_ts_ns(&row[8]),
                });
        }

        Ok::<_, String>(by_device)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn read_trust_mark_impact(
    db: Arc<lbug::Database>,
    address: String,
) -> Result<(usize, usize), (StatusCode, String)> {
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "MATCH (m:RemediationTrustMark)-[:TRUST_MARKS]->(r:Remediation)-[:RESOLVES]->(e:DetectionEvent) \
                 WHERE e.device_address = $addr \
                 RETURN m.trustworthy",
            )
            .map_err(|e| e.to_string())?;
        let rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(address))])
            .map_err(|e| e.to_string())?;

        let mut total = 0usize;
        let mut active = 0usize;
        for row in rows {
            total += 1;
            if read_i64(&row[0]) == 1 {
                active += 1;
            }
        }
        Ok::<_, String>((total, active))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

fn managed_device_json(
    target: TargetConfig,
    statuses: &HashMap<String, Vec<SubscriptionStatusJson>>,
    overrides: &[crate::registry::PathOverride],
) -> ManagedDeviceJson {
    let (_, audit) = crate::discovery::resolve_subscription_paths(&target, overrides);
    let address = target.address;
    ManagedDeviceJson {
        enabled: target.enabled,
        collector_id: target.collector_id.unwrap_or_default(),
        tls_domain: target.tls_domain.unwrap_or_default(),
        ca_cert: target.ca_cert.unwrap_or_default(),
        vendor: target.vendor.unwrap_or_default(),
        credential_alias: target.credential_alias.unwrap_or_default(),
        username_env: target.username_env.unwrap_or_default(),
        password_env: target.password_env.unwrap_or_default(),
        hostname: target.hostname.unwrap_or_default(),
        role: target.role.unwrap_or_default(),
        site: target.site.unwrap_or_default(),
        selected_paths: target.selected_paths,
        subscription_statuses: statuses.get(&address).cloned().unwrap_or_default(),
        address,
        resolution_audit: audit,
    }
}

fn target_from_request(req: ManagedDeviceRequest) -> Result<TargetConfig, (StatusCode, String)> {
    if req.address.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "device address is required".to_string(),
        ));
    }
    if !req.username_env.trim().is_empty() && std::env::var(req.username_env.trim()).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("username env var '{}' is not set", req.username_env.trim()),
        ));
    }
    if !req.password_env.trim().is_empty() && std::env::var(req.password_env.trim()).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("password env var '{}' is not set", req.password_env.trim()),
        ));
    }

    Ok(TargetConfig {
        address: req.address.trim().to_string(),
        enabled: req.enabled,
        tls_domain: option_string(req.tls_domain),
        ca_cert: option_string(req.ca_cert),
        vendor: option_string(req.vendor),
        credential_alias: option_string(req.credential_alias),
        username_env: option_string(req.username_env),
        password_env: option_string(req.password_env),
        username: None,
        password: None,
        hostname: option_string(req.hostname),
        role: option_string(req.role),
        site: option_string(req.site),
        collector_id: None,
        selected_paths: req
            .selected_paths
            .into_iter()
            .filter(|path| !path.path.trim().is_empty())
            .collect(),
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
        last_operator_action: String::new(),
    })
}

fn site_json(site: SiteRecord) -> SiteJson {
    SiteJson {
        id: site.id,
        name: site.name,
        parent_id: site.parent_id,
        kind: site.kind,
        lat: site.lat,
        lon: site.lon,
        metadata_json: site.metadata_json,
        environment_id: site.environment_id,
    }
}

fn site_record(site: SiteJson) -> SiteRecord {
    SiteRecord {
        id: site.id,
        name: site.name,
        parent_id: site.parent_id,
        kind: site.kind,
        lat: site.lat,
        lon: site.lon,
        metadata_json: site.metadata_json,
        environment_id: site.environment_id,
    }
}

fn credential_json(
    credential: CredentialSummary,
    device_counts: &HashMap<String, usize>,
) -> CredentialJson {
    CredentialJson {
        device_count: device_counts
            .get(&credential.alias)
            .copied()
            .unwrap_or_default(),
        alias: credential.alias,
        created_at_ns: credential.created_at_ns,
        updated_at_ns: credential.updated_at_ns,
        last_used_at_ns: credential.last_used_at_ns,
    }
}

fn credential_device_counts(registry: &ApiRegistry) -> anyhow::Result<HashMap<String, usize>> {
    let mut counts = HashMap::new();
    for target in registry.list_all_targets()? {
        if let Some(alias) = target.credential_alias {
            *counts.entry(alias).or_insert(0) += 1;
        }
    }
    Ok(counts)
}

fn resolve_request_credentials(
    credentials: &CredentialVault,
    credential_alias: Option<String>,
    username_env: Option<String>,
    password_env: Option<String>,
) -> anyhow::Result<Option<ResolvedCredential>> {
    if let Some(alias) = credential_alias {
        return credentials
            .resolve(&alias, ResolvePurpose::Discover)
            .map(Some);
    }

    let username = username_env
        .as_deref()
        .and_then(|key| std::env::var(key).ok());
    let password = password_env
        .as_deref()
        .and_then(|key| std::env::var(key).ok());
    Ok(match (username, password) {
        (Some(username), Some(password)) => Some(ResolvedCredential { username, password }),
        _ => None,
    })
}

fn option_string(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn site_subtree_ids(sites: &[SiteRecord], root_id: &str) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::from([root_id.to_string()]);
    let mut changed = true;
    while changed {
        changed = false;
        for site in sites {
            if !site.parent_id.is_empty()
                && ids.contains(&site.parent_id)
                && ids.insert(site.id.clone())
            {
                changed = true;
            }
        }
    }
    ids
}

fn build_site_path_by_id(sites: &[SiteRecord]) -> HashMap<String, String> {
    let by_id: HashMap<&str, &SiteRecord> =
        sites.iter().map(|site| (site.id.as_str(), site)).collect();
    let mut path_by_id = HashMap::new();

    for site in sites {
        let mut names = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut current_id = site.id.as_str();
        let mut depth = 0;

        while depth < 16 && seen.insert(current_id.to_string()) {
            let Some(current) = by_id.get(current_id) else {
                break;
            };
            names.push(current.name.clone());
            if current.parent_id.is_empty() {
                break;
            }
            current_id = current.parent_id.as_str();
            depth += 1;
        }

        names.reverse();
        path_by_id.insert(site.id.clone(), names.join(" / "));
    }

    path_by_id
}

fn resolve_site_metadata(
    site: &str,
    site_id_by_name: &HashMap<String, String>,
    site_path_by_id: &HashMap<String, String>,
) -> (String, String) {
    let site = site.trim();
    if site.is_empty() {
        return (String::new(), String::new());
    }

    let site_id = if site_path_by_id.contains_key(site) {
        site.to_string()
    } else {
        site_id_by_name.get(site).cloned().unwrap_or_default()
    };
    if site_id.is_empty() {
        return (String::new(), String::new());
    }

    let site_path = site_path_by_id.get(&site_id).cloned().unwrap_or_default();
    (site_id, site_path)
}

fn read_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

fn read_i64(v: &Value) -> i64 {
    match v {
        Value::Int64(n) => *n,
        _ => 0,
    }
}

fn read_ts_ns(v: &Value) -> i64 {
    match v {
        Value::TimestampNs(dt) => dt.unix_timestamp_nanos() as i64,
        _ => 0,
    }
}

// ── Device detail endpoint ────────────────────────────────────────────────────

async fn device_detail_handler(
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

// ── Device enrichment endpoint ────────────────────────────────────────────────

#[derive(Serialize)]
struct EnrichmentPropertyJson {
    key: String,
    value: String,
    source_name: String,
    updated_at_ns: i64,
    confidence: String,
    parser: String,
}

#[derive(Serialize)]
struct DeviceEnrichmentResponse {
    address: String,
    /// Properties grouped by source_name for display.
    properties: Vec<EnrichmentPropertyJson>,
}

async fn device_enrichment_handler(
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

#[derive(Serialize)]
struct DeviceConfigHistoryResponse {
    address: String,
    snapshots: Vec<change_detection::ConfigSnapshotSummary>,
    changes: Vec<change_detection::ConfigChangeSummary>,
}

#[derive(Serialize)]
struct DeviceGnmiReadinessResponse {
    address: String,
    report: discovery::GnmiReadinessReport,
}

#[derive(Serialize)]
struct DeviceStreamingReadinessResponse {
    address: String,
    report: StreamingReadinessReport,
}

async fn device_config_history_handler(
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

async fn device_gnmi_readiness_handler(
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

async fn device_streaming_readiness_handler(
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

async fn device_recommendations_handler(
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

async fn yang_modules_handler(
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

async fn yang_search_handler(
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

async fn apply_device_selected_paths_handler(
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

async fn persist_gnmi_readiness(
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

async fn persist_streaming_readiness(
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

#[derive(Deserialize)]
struct DeviceReparseRequest {
    #[serde(default)]
    reason: String,
}

#[derive(Serialize)]
struct DeviceReparseResponse {
    success: bool,
    message: String,
}

async fn device_reparse_handler(
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

fn resolve_target_credentials_for_discovery(
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

// ── Profiles ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ProfilesResponse {
    profiles: Vec<ProfileJson>,
    plugins: Vec<PluginJson>,
    load_errors: Vec<String>,
}

#[derive(Serialize)]
struct ProfileJson {
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
struct PluginJson {
    name: String,
    version: String,
    author: String,
    profile_count: usize,
    conflicts: Vec<String>,
}

async fn profiles_handler(State(state): State<AppState>) -> Json<ProfilesResponse> {
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

#[derive(Deserialize)]
struct SaveCustomProfileRequest {
    name: String,
    description: String,
    rationale: String,
    environment: Vec<String>,
    vendor_scope: Vec<String>,
    roles: Vec<String>,
    paths: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct SaveCustomProfileResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn save_custom_profile_handler(
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

// ── Enrichment handlers ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct EnricherEntry {
    config: crate::enrichment::EnricherConfig,
    state: crate::enrichment::EnricherRunState,
}

#[derive(Serialize)]
struct EnrichmentListResponse {
    enrichers: Vec<EnricherEntry>,
}

async fn enrichment_list_handler(State(state): State<AppState>) -> Json<EnrichmentListResponse> {
    let reg = state.enricher_registry.read().await;
    let enrichers = reg
        .list()
        .into_iter()
        .map(|(config, st)| EnricherEntry { config, state: st })
        .collect();
    Json(EnrichmentListResponse { enrichers })
}

#[derive(Deserialize)]
struct EnrichmentUpsertRequest {
    config: EnricherConfig,
}

#[derive(Serialize)]
struct EnrichmentMutationResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn enrichment_upsert_handler(
    State(state): State<AppState>,
    Json(req): Json<EnrichmentUpsertRequest>,
) -> Json<EnrichmentMutationResponse> {
    state.enricher_registry.write().await.upsert(req.config);
    Json(EnrichmentMutationResponse {
        success: true,
        error: None,
    })
}

#[derive(Deserialize)]
struct EnrichmentNameRequest {
    name: String,
}

async fn enrichment_remove_handler(
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

#[derive(Serialize)]
struct EnrichmentTestResponse {
    success: bool,
    message: String,
}

async fn enrichment_test_handler(
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

#[derive(Serialize)]
struct EnrichmentRunResponse {
    success: bool,
    message: String,
}

async fn enrichment_run_handler(
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

#[derive(Serialize)]
struct EnrichmentAuditResponse {
    entries: Vec<serde_json::Value>,
}

async fn enrichment_audit_handler(State(state): State<AppState>) -> Json<EnrichmentAuditResponse> {
    // Read audit log files and return the last 100 enrichment_run entries.
    let audit_dir = std::path::Path::new(&state.runtime_dir).join("audit");
    let entries = read_recent_enrichment_audit(&audit_dir, 100);
    Json(EnrichmentAuditResponse { entries })
}

fn read_recent_enrichment_audit(
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

// ── Human-in-the-loop remediation approvals (Sprint 4) ───────────────────────

#[derive(Deserialize)]
struct ApprovalsParams {
    #[serde(default = "default_proposal_status")]
    status: String,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_proposal_status() -> String {
    "pending".to_string()
}

#[derive(Serialize)]
struct TrustEntry {
    trust_key: String,
    record: crate::remediation::TrustRecord,
}

#[derive(Serialize)]
struct ApprovalsListResponse {
    proposals: Vec<RemediationProposalRow>,
    trust: Vec<TrustEntry>,
    graduation_hints: Vec<crate::remediation::GraduationHint>,
    active_rollbacks: Vec<crate::remediation::RollbackState>,
}

async fn approvals_list_handler(
    State(state): State<AppState>,
    Query(params): Query<ApprovalsParams>,
) -> Result<Json<ApprovalsListResponse>, (StatusCode, String)> {
    let proposals = state
        .store
        .read_remediation_proposals(Some(params.status), params.limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

    let (trust, graduation_hints) = {
        let store = state.trust_store.read().await;
        let records = store.list();
        let trust = records
            .iter()
            .map(|(trust_key, record)| TrustEntry {
                trust_key: trust_key.clone(),
                record: record.clone(),
            })
            .collect();
        let graduation_hints = records
            .iter()
            .filter_map(|(trust_key, record)| {
                check_graduation(
                    trust_key,
                    record,
                    state
                        .remediation_config
                        .graduation
                        .consecutive_approvals_required,
                )
            })
            .collect();
        (trust, graduation_hints)
    };

    let active_rollbacks = {
        let mut registry = state.rollback_registry.write().await;
        let now = now_ns();
        registry.prune(now);
        registry.active_windows(now).into_iter().cloned().collect()
    };

    Ok(Json(ApprovalsListResponse {
        proposals,
        trust,
        graduation_hints,
        active_rollbacks,
    }))
}

#[derive(Deserialize)]
struct CreateApprovalRequest {
    detection_id: String,
    playbook_id: String,
    #[serde(default)]
    rule_id: String,
    #[serde(default)]
    environment_archetype: String,
    #[serde(default)]
    site_id: String,
    #[serde(default)]
    trust_key: String,
    #[serde(default)]
    steps_json: String,
    #[serde(default)]
    rollback_steps_json: String,
}

#[derive(Serialize)]
struct CreateApprovalResponse {
    success: bool,
    error: String,
    proposal_id: String,
    trust_state: String,
}

async fn approvals_create_handler(
    State(state): State<AppState>,
    Json(req): Json<CreateApprovalRequest>,
) -> Json<CreateApprovalResponse> {
    let trust_key = if req.trust_key.trim().is_empty() {
        TrustKey::new(
            &req.rule_id,
            &req.environment_archetype,
            &req.site_id,
            &req.playbook_id,
        )
        .to_storage_key()
    } else {
        req.trust_key.clone()
    };

    let trust_state = {
        let mut store = state.trust_store.write().await;
        let key = trust_key_from_storage(&trust_key);
        store.get_or_default(&key).state.as_str().to_string()
    };

    match state
        .store
        .write_remediation_proposal(
            req.detection_id,
            req.playbook_id,
            trust_key,
            req.steps_json,
            req.rollback_steps_json,
            now_ns(),
        )
        .await
    {
        Ok(proposal_id) => Json(CreateApprovalResponse {
            success: true,
            error: String::new(),
            proposal_id,
            trust_state,
        }),
        Err(e) => Json(CreateApprovalResponse {
            success: false,
            error: format!("{e:#}"),
            proposal_id: String::new(),
            trust_state,
        }),
    }
}

#[derive(Deserialize)]
struct ApprovalDecisionRequest {
    #[serde(default)]
    operator_note: String,
}

#[derive(Serialize)]
struct ApprovalDecisionResponse {
    success: bool,
    error: String,
    remediation_id: String,
}

async fn approvals_approve_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ApprovalDecisionRequest>,
) -> Json<ApprovalDecisionResponse> {
    let proposal = match find_proposal(&state, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            return Json(ApprovalDecisionResponse {
                success: false,
                error: format!("proposal '{id}' not found"),
                remediation_id: String::new(),
            });
        }
        Err(e) => {
            return Json(ApprovalDecisionResponse {
                success: false,
                error: e,
                remediation_id: String::new(),
            });
        }
    };

    let now = now_ns();
    let execution =
        execute_proposal_steps(&state, &proposal.device_address, &proposal.steps_json).await;
    let (proposal_status, remediation_status, detail_json) = match execution {
        Ok(report) => (
            "approved".to_string(),
            "success".to_string(),
            serde_json::json!({
                "proposal_id": proposal.id,
                "operator_note": req.operator_note,
                "steps_executed": report.steps_executed,
                "steps": proposal.steps_json,
            })
            .to_string(),
        ),
        Err(e) => (
            "failed".to_string(),
            "failed".to_string(),
            serde_json::json!({
                "proposal_id": proposal.id,
                "operator_note": req.operator_note,
                "error": e,
                "steps": proposal.steps_json,
            })
            .to_string(),
        ),
    };

    if let Err(e) = state
        .store
        .decide_remediation_proposal(
            id.clone(),
            proposal_status.clone(),
            req.operator_note.clone(),
            now,
        )
        .await
    {
        return Json(ApprovalDecisionResponse {
            success: false,
            error: format!("{e:#}"),
            remediation_id: String::new(),
        });
    }

    let remediation_id = match state
        .store
        .write_remediation(
            proposal.detection_id.clone(),
            proposal.playbook_id.clone(),
            remediation_status.clone(),
            detail_json,
            now,
            now,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            return Json(ApprovalDecisionResponse {
                success: false,
                error: format!("{e:#}"),
                remediation_id: String::new(),
            });
        }
    };

    let trust_key = trust_key_from_storage(&proposal.trust_key);
    if remediation_status == "success" {
        state
            .trust_store
            .write()
            .await
            .record_approval(&trust_key, now);
    } else {
        state
            .trust_store
            .write()
            .await
            .record_failure(&trust_key, now);
    }
    let _ = audit::append_trust_operation(
        std::path::Path::new(&state.runtime_dir),
        now,
        &proposal.trust_key,
        if remediation_status == "success" {
            "approve"
        } else {
            "approve_failed"
        },
        &id,
        Some(&req.operator_note),
    );

    if remediation_status == "success" && !proposal.rollback_steps_json.trim().is_empty() {
        state
            .rollback_registry
            .write()
            .await
            .register(crate::remediation::RollbackState {
                proposal_id: id,
                remediation_id: remediation_id.clone(),
                executed_at_ns: now,
                window_secs: state.remediation_config.rollback_window_secs,
                snapshot_json: proposal.rollback_steps_json,
                rolled_back: false,
            });
    }

    Json(ApprovalDecisionResponse {
        success: remediation_status == "success",
        error: if remediation_status == "success" {
            String::new()
        } else {
            "proposal execution failed".to_string()
        },
        remediation_id,
    })
}

async fn approvals_reject_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ApprovalDecisionRequest>,
) -> Json<ApprovalDecisionResponse> {
    let proposal = match find_proposal(&state, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            return Json(ApprovalDecisionResponse {
                success: false,
                error: format!("proposal '{id}' not found"),
                remediation_id: String::new(),
            });
        }
        Err(e) => {
            return Json(ApprovalDecisionResponse {
                success: false,
                error: e,
                remediation_id: String::new(),
            });
        }
    };

    let now = now_ns();
    if let Err(e) = state
        .store
        .decide_remediation_proposal(
            id.clone(),
            "rejected".to_string(),
            req.operator_note.clone(),
            now,
        )
        .await
    {
        return Json(ApprovalDecisionResponse {
            success: false,
            error: format!("{e:#}"),
            remediation_id: String::new(),
        });
    }
    let trust_key = trust_key_from_storage(&proposal.trust_key);
    state
        .trust_store
        .write()
        .await
        .record_rejection(&trust_key, now);
    let _ = audit::append_trust_operation(
        std::path::Path::new(&state.runtime_dir),
        now,
        &proposal.trust_key,
        "reject",
        &id,
        Some(&req.operator_note),
    );

    Json(ApprovalDecisionResponse {
        success: true,
        error: String::new(),
        remediation_id: String::new(),
    })
}

async fn approvals_rollback_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ApprovalDecisionRequest>,
) -> Json<ApprovalDecisionResponse> {
    let proposal = match find_proposal(&state, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            return Json(ApprovalDecisionResponse {
                success: false,
                error: format!("proposal '{id}' not found"),
                remediation_id: String::new(),
            });
        }
        Err(e) => {
            return Json(ApprovalDecisionResponse {
                success: false,
                error: e,
                remediation_id: String::new(),
            });
        }
    };

    let now = now_ns();
    let rollback_state = {
        let registry = state.rollback_registry.read().await;
        registry.get(&id).cloned()
    };
    let Some(rollback_state) = rollback_state else {
        return Json(ApprovalDecisionResponse {
            success: false,
            error: "rollback window is not active".to_string(),
            remediation_id: String::new(),
        });
    };
    if rollback_state.is_expired(now) {
        return Json(ApprovalDecisionResponse {
            success: false,
            error: "rollback window expired".to_string(),
            remediation_id: String::new(),
        });
    }

    if let Err(e) = execute_proposal_steps(
        &state,
        &proposal.device_address,
        &rollback_state.snapshot_json,
    )
    .await
    {
        let trust_key = trust_key_from_storage(&proposal.trust_key);
        {
            let mut trust = state.trust_store.write().await;
            trust.record_failure(&trust_key, now);
            trust.set_state(&trust_key, TrustState::ApproveEach, now);
        }
        let _ = audit::append_trust_operation(
            std::path::Path::new(&state.runtime_dir),
            now,
            &proposal.trust_key,
            "rollback_failed",
            &id,
            Some(&format!("{}; error={e}", req.operator_note)),
        );
        return Json(ApprovalDecisionResponse {
            success: false,
            error: e,
            remediation_id: rollback_state.remediation_id,
        });
    }

    if let Err(e) = state
        .store
        .decide_remediation_proposal(
            id.clone(),
            "rolled_back".to_string(),
            req.operator_note.clone(),
            now,
        )
        .await
    {
        return Json(ApprovalDecisionResponse {
            success: false,
            error: format!("{e:#}"),
            remediation_id: String::new(),
        });
    }
    state.rollback_registry.write().await.mark_rolled_back(&id);
    let trust_key = trust_key_from_storage(&proposal.trust_key);
    {
        let mut trust = state.trust_store.write().await;
        trust.record_failure(&trust_key, now);
        trust.set_state(&trust_key, TrustState::ApproveEach, now);
    }
    let _ = audit::append_trust_operation(
        std::path::Path::new(&state.runtime_dir),
        now,
        &proposal.trust_key,
        "rollback",
        &id,
        Some(&req.operator_note),
    );

    Json(ApprovalDecisionResponse {
        success: true,
        error: String::new(),
        remediation_id: rollback_state.remediation_id,
    })
}

async fn trust_list_handler(State(state): State<AppState>) -> Json<ApprovalsListResponse> {
    let (trust, graduation_hints) = {
        let store = state.trust_store.read().await;
        let records = store.list();
        let trust = records
            .iter()
            .map(|(trust_key, record)| TrustEntry {
                trust_key: trust_key.clone(),
                record: record.clone(),
            })
            .collect();
        let graduation_hints = records
            .iter()
            .filter_map(|(trust_key, record)| {
                check_graduation(
                    trust_key,
                    record,
                    state
                        .remediation_config
                        .graduation
                        .consecutive_approvals_required,
                )
            })
            .collect();
        (trust, graduation_hints)
    };
    let active_rollbacks = {
        let registry = state.rollback_registry.read().await;
        registry
            .active_windows(now_ns())
            .into_iter()
            .cloned()
            .collect()
    };
    Json(ApprovalsListResponse {
        proposals: Vec::new(),
        trust,
        graduation_hints,
        active_rollbacks,
    })
}

#[derive(Deserialize)]
struct TrustGraduateRequest {
    trust_key: String,
    to_state: String,
    #[serde(default)]
    operator_note: String,
}

async fn trust_graduate_handler(
    State(state): State<AppState>,
    Json(req): Json<TrustGraduateRequest>,
) -> Json<ApprovalDecisionResponse> {
    let now = now_ns();
    let key = trust_key_from_storage(&req.trust_key);
    let state_to = TrustState::parse_state(&req.to_state);
    state
        .trust_store
        .write()
        .await
        .set_state(&key, state_to, now);
    let _ = audit::append_trust_operation(
        std::path::Path::new(&state.runtime_dir),
        now,
        &req.trust_key,
        "graduate",
        "",
        Some(&req.operator_note),
    );
    Json(ApprovalDecisionResponse {
        success: true,
        error: String::new(),
        remediation_id: String::new(),
    })
}

async fn find_proposal(
    state: &AppState,
    id: &str,
) -> Result<Option<RemediationProposalRow>, String> {
    state
        .store
        .read_remediation_proposals(Some("all".to_string()), 500)
        .await
        .map_err(|e| format!("{e:#}"))
        .map(|rows| rows.into_iter().find(|p| p.id == id))
}

fn trust_key_from_storage(key: &str) -> TrustKey {
    let mut parts = key.splitn(4, ':');
    TrustKey::new(
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    )
}

#[derive(Deserialize)]
struct ProposalGnmiSet {
    path: String,
    value: String,
}

#[derive(Deserialize)]
struct ProposalVerifyGraph {
    expected_graph_state: String,
    #[serde(default = "default_verify_wait_secs")]
    wait_seconds: u64,
}

fn default_verify_wait_secs() -> u64 {
    30
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ProposalStep {
    GnmiSet { gnmi_set: ProposalGnmiSet },
    Sleep { sleep: serde_json::Value },
    VerifyGraph { verify_graph: ProposalVerifyGraph },
}

struct ProposalExecutionReport {
    steps_executed: usize,
}

async fn execute_proposal_steps(
    state: &AppState,
    device_address: &str,
    steps_json: &str,
) -> Result<ProposalExecutionReport, String> {
    let steps: Vec<ProposalStep> = if steps_json.trim().is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(steps_json).map_err(|e| format!("invalid proposal steps_json: {e}"))?
    };

    if steps.is_empty() {
        return Ok(ProposalExecutionReport { steps_executed: 0 });
    }
    if device_address.trim().is_empty() {
        return Err("proposal is not linked to a device address".to_string());
    }

    let mut executed = 0usize;
    for step in steps {
        match step {
            ProposalStep::GnmiSet { gnmi_set: op } => {
                let conn = target_conn_info_for_http(state, device_address).await?;
                gnmi_set(
                    &conn.address,
                    conn.username.as_deref(),
                    conn.password.as_deref(),
                    conn.ca_cert_pem.as_deref(),
                    &conn.tls_domain,
                    &op.path,
                    &op.value,
                )
                .await
                .map_err(|e| format!("{e:#}"))?;
                executed += 1;
            }
            ProposalStep::Sleep { sleep } => {
                let secs = sleep
                    .as_f64()
                    .or_else(|| sleep.as_str().and_then(|s| s.parse::<f64>().ok()))
                    .unwrap_or(0.0)
                    .clamp(0.0, 300.0);
                if secs > 0.0 {
                    tokio::time::sleep(std::time::Duration::from_secs_f64(secs)).await;
                }
                executed += 1;
            }
            ProposalStep::VerifyGraph { verify_graph } => {
                verify_graph_state(
                    state,
                    &verify_graph.expected_graph_state,
                    verify_graph.wait_seconds,
                )
                .await?;
                executed += 1;
            }
        }
    }

    Ok(ProposalExecutionReport {
        steps_executed: executed,
    })
}

async fn verify_graph_state(
    state: &AppState,
    cypher: &str,
    wait_seconds: u64,
) -> Result<(), String> {
    if cypher.trim().is_empty() {
        return Ok(());
    }
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(wait_seconds.clamp(1, 300));
    loop {
        let db = state.store.db();
        let query = cypher.to_string();
        let verified = tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            let mut rows = conn.query(&query).map_err(|e| e.to_string())?;
            let Some(row) = rows.next() else {
                return Ok::<bool, String>(false);
            };
            Ok(row.first().map(value_truthy).unwrap_or(false))
        })
        .await
        .map_err(|e| format!("verification task failed: {e}"))??;
        if verified {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err("graph verification timed out".to_string());
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

fn value_truthy(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Int64(n) => *n > 0,
        Value::String(s) => s == "true" || s == "1",
        _ => false,
    }
}

struct HttpTargetConnInfo {
    address: String,
    username: Option<String>,
    password: Option<String>,
    ca_cert_pem: Option<Vec<u8>>,
    tls_domain: String,
}

async fn target_conn_info_for_http(
    state: &AppState,
    device_address: &str,
) -> Result<HttpTargetConnInfo, String> {
    let target = state
        .registry
        .get_device(device_address)
        .map_err(|e| format!("{e:#}"))?
        .ok_or_else(|| format!("unknown target '{device_address}'"))?;

    let ca_cert_pem = match &target.ca_cert {
        Some(path) if !path.is_empty() => Some(
            tokio::fs::read(path)
                .await
                .map_err(|e| format!("could not read CA cert from '{path}': {e}"))?,
        ),
        _ => None,
    };

    let resolved_credentials = resolve_http_target_credentials(&target, &state.credentials)
        .map_err(|e| format!("{e:#}"))?;
    let (username, password) = match resolved_credentials {
        Some(credentials) => (Some(credentials.username), Some(credentials.password)),
        None => (None, None),
    };

    Ok(HttpTargetConnInfo {
        address: target.address,
        username,
        password,
        ca_cert_pem,
        tls_domain: target.tls_domain.unwrap_or_default(),
    })
}

fn resolve_http_target_credentials(
    target: &TargetConfig,
    credentials: &CredentialVault,
) -> anyhow::Result<Option<ResolvedCredential>> {
    if let Some(alias) = target.credential_alias.as_deref()
        && !alias.is_empty()
    {
        return credentials
            .resolve(alias, ResolvePurpose::Remediate)
            .map(Some);
    }

    Ok(match (&target.username, &target.password) {
        (Some(username), Some(password)) if !username.is_empty() || !password.is_empty() => {
            Some(ResolvedCredential {
                username: username.clone(),
                password: password.clone(),
            })
        }
        _ => None,
    })
}

// ── ServiceNow integration test endpoint (T2-1) ───────────────────────────────

#[derive(Deserialize)]
struct SnowTestRequest {
    instance_url: String,
    credential_alias: String,
}

#[derive(Serialize)]
struct SnowTestResponse {
    success: bool,
    message: String,
}

#[derive(Serialize)]
struct SnowAiopsSyncResponse {
    success: bool,
    error: String,
    stats: crate::integrations::servicenow_aiops::SyncStats,
}

async fn snow_integration_test_handler(
    State(state): State<AppState>,
    Json(req): Json<SnowTestRequest>,
) -> Json<SnowTestResponse> {
    let instance_url = req.instance_url.trim_end_matches('/').to_string();

    let cred = match state.credentials.resolve(
        &req.credential_alias,
        crate::credentials::ResolvePurpose::ServiceNowAdmin,
    ) {
        Ok(c) => c,
        Err(e) => {
            return Json(SnowTestResponse {
                success: false,
                message: format!("credential resolve failed: {e:#}"),
            });
        }
    };

    let url = format!("{instance_url}/api/now/table/sys_properties?sysparm_limit=1");
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Json(SnowTestResponse {
                success: false,
                message: e.to_string(),
            });
        }
    };

    match client
        .get(&url)
        .basic_auth(&cred.username, Some(&cred.password))
        .send()
        .await
    {
        Err(e) => Json(SnowTestResponse {
            success: false,
            message: format!("{e:#}"),
        }),
        Ok(resp) if resp.status().is_success() => Json(SnowTestResponse {
            success: true,
            message: "ServiceNow connection successful".to_string(),
        }),
        Ok(resp) => Json(SnowTestResponse {
            success: false,
            message: format!("ServiceNow returned {}", resp.status()),
        }),
    }
}

async fn servicenow_aiops_sync_handler(
    State(state): State<AppState>,
) -> Json<SnowAiopsSyncResponse> {
    match crate::integrations::servicenow_aiops::run_sync_cycle(
        &state.servicenow_config,
        &state.store,
        &state.credentials,
    )
    .await
    {
        Ok(stats) => Json(SnowAiopsSyncResponse {
            success: true,
            error: String::new(),
            stats,
        }),
        Err(e) => Json(SnowAiopsSyncResponse {
            success: false,
            error: format!("{e:#}"),
            stats: crate::integrations::servicenow_aiops::SyncStats::default(),
        }),
    }
}

#[derive(serde::Deserialize)]
pub struct RemoveOverrideReq {
    pub scope: crate::registry::OverrideScope,
    pub path: String,
}

async fn list_overrides(State(state): State<AppState>) -> impl axum::response::IntoResponse {
    match state.registry.list_overrides() {
        Ok(overrides) => Json(overrides).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to list overrides: {}", e),
        )
            .into_response(),
    }
}

async fn add_override(
    State(state): State<AppState>,
    Json(mut req): Json<crate::registry::PathOverride>,
) -> impl axum::response::IntoResponse {
    let actor = std::env::var("BONSAI_OPERATOR")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default();

    req.created_at_ns = now;
    req.created_by = actor.clone();

    match state.registry.add_override(req.clone()) {
        Ok(_) => Json(req).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to add override: {}", e),
        )
            .into_response(),
    }
}

async fn remove_override(
    State(state): State<AppState>,
    Json(req): Json<RemoveOverrideReq>,
) -> impl axum::response::IntoResponse {
    match state.registry.remove_override(&req.scope, &req.path) {
        Ok(removed) => Json(serde_json::json!({ "removed": removed })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to remove override: {}", e),
        )
            .into_response(),
    }
}

// ── Output adapter management API (T6-6) ─────────────────────────────────────

#[derive(Serialize)]
struct AdapterEntry {
    config: OutputAdapterConfig,
    state: OutputAdapterRunState,
}

#[derive(Serialize)]
struct AdapterListResponse {
    adapters: Vec<AdapterEntry>,
}

async fn adapter_list_handler(State(state): State<AppState>) -> Json<AdapterListResponse> {
    let audit_dir = std::path::Path::new(&state.runtime_dir).join("audit");
    let latest_pushes = latest_adapter_push_state(&read_recent_adapter_audit(&audit_dir, 1000));

    let reg = state.adapter_registry.read().await;
    let adapters = reg
        .list()
        .into_iter()
        .map(|(config, mut st)| {
            if let Some(audit_state) = latest_pushes.get(&config.name) {
                st.last_push_at_ns = audit_state.last_push_at_ns;
                st.last_push_duration_ms = audit_state.last_push_duration_ms;
                st.last_push_events = audit_state.last_push_events;
                st.last_push_bytes = audit_state.last_push_bytes;
                st.last_push_warnings = audit_state.last_push_warnings.clone();
                st.last_push_error = audit_state.last_push_error.clone();
                st.total_events_pushed = audit_state.total_events_pushed;
                st.total_bytes_sent = audit_state.total_bytes_sent;
            }
            AdapterEntry { config, state: st }
        })
        .collect();
    Json(AdapterListResponse { adapters })
}

#[derive(Deserialize)]
struct AdapterUpsertRequest {
    config: OutputAdapterConfig,
}

#[derive(Serialize)]
struct AdapterMutationResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn adapter_upsert_handler(
    State(state): State<AppState>,
    Json(req): Json<AdapterUpsertRequest>,
) -> Json<AdapterMutationResponse> {
    if let Err(error) = crate::output::ensure_supported_adapter_type(&req.config) {
        return Json(AdapterMutationResponse {
            success: false,
            error: Some(error.to_string()),
        });
    }
    state.adapter_registry.write().await.upsert(req.config);
    Json(AdapterMutationResponse {
        success: true,
        error: None,
    })
}

#[derive(Deserialize)]
struct AdapterNameRequest {
    name: String,
}

async fn adapter_remove_handler(
    State(state): State<AppState>,
    Json(req): Json<AdapterNameRequest>,
) -> Json<AdapterMutationResponse> {
    let removed = state.adapter_registry.write().await.remove(&req.name);
    if removed {
        Json(AdapterMutationResponse {
            success: true,
            error: None,
        })
    } else {
        Json(AdapterMutationResponse {
            success: false,
            error: Some(format!("adapter '{}' not found", req.name)),
        })
    }
}

#[derive(Serialize)]
struct AdapterTestResponse {
    success: bool,
    message: String,
}

async fn adapter_test_handler(
    State(state): State<AppState>,
    Json(req): Json<AdapterNameRequest>,
) -> Json<AdapterTestResponse> {
    let config = {
        let reg = state.adapter_registry.read().await;
        reg.get(&req.name).cloned()
    };
    let Some(config) = config else {
        return Json(AdapterTestResponse {
            success: false,
            message: format!("adapter '{}' not found", req.name),
        });
    };

    let audit = crate::output::traits::OutputAdapterAuditLog::new(
        std::path::Path::new(&state.runtime_dir),
        &config.name,
    );

    let result = match crate::output::build_adapter(&config, state.store.db()) {
        Some(adapter) => {
            adapter
                .test_connection(Arc::clone(&state.credentials), &audit)
                .await
        }
        None => Err(anyhow::anyhow!(
            "unknown adapter type '{}'",
            config.adapter_type
        )),
    };

    match result {
        Ok(()) => Json(AdapterTestResponse {
            success: true,
            message: "connection ok".to_string(),
        }),
        Err(e) => Json(AdapterTestResponse {
            success: false,
            message: format!("{e:#}"),
        }),
    }
}

#[derive(Serialize)]
struct AdapterAuditResponse {
    entries: Vec<serde_json::Value>,
}

async fn adapter_audit_handler(State(state): State<AppState>) -> Json<AdapterAuditResponse> {
    let audit_dir = std::path::Path::new(&state.runtime_dir).join("audit");
    let entries = read_recent_adapter_audit(&audit_dir, 100);
    Json(AdapterAuditResponse { entries })
}

fn read_recent_adapter_audit(audit_dir: &std::path::Path, limit: usize) -> Vec<serde_json::Value> {
    if !audit_dir.exists() {
        return vec![];
    }
    let mut files: Vec<_> = std::fs::read_dir(audit_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "jsonl")
        })
        .collect();
    files.sort();

    let mut entries = Vec::new();
    for file in files.iter().rev() {
        if let Ok(content) = std::fs::read_to_string(file) {
            for line in content.lines().rev() {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
                    && val.get("event").and_then(|v| v.as_str()) == Some("adapter_push")
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

fn latest_adapter_push_state(
    entries: &[serde_json::Value],
) -> std::collections::HashMap<String, OutputAdapterRunState> {
    let mut by_adapter = std::collections::HashMap::new();
    for entry in entries {
        if entry.get("event").and_then(|v| v.as_str()) != Some("adapter_push") {
            continue;
        }
        let Some(name) = entry.get("adapter").and_then(|v| v.as_str()) else {
            continue;
        };
        let state = by_adapter
            .entry(name.to_string())
            .or_insert_with(OutputAdapterRunState::default);
        state.total_events_pushed += entry
            .get("events_pushed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        state.total_bytes_sent += entry
            .get("bytes_sent")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let ts = entry
            .get("timestamp_ns")
            .and_then(|v| v.as_i64())
            .unwrap_or_default();
        if state.last_push_at_ns.is_none_or(|cur| ts >= cur) {
            state.last_push_at_ns = Some(ts);
            state.last_push_events = Some(
                entry
                    .get("events_pushed")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize,
            );
            state.last_push_bytes = Some(
                entry
                    .get("bytes_sent")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            );
            state.last_push_error = entry
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            state.last_push_warnings = vec![];
        }
    }
    by_adapter
}

// ─── graph insights (T1-4) ───────────────────────────────────────────────────

async fn graph_insights_handler(
    State(state): State<AppState>,
) -> Result<Json<crate::graph::algorithms::GraphInsights>, (StatusCode, String)> {
    let db = state.store.db();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::algorithms::graph_insights(&conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

// ─── explorer (T1-5) ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ExplorerQueryBody {
    cypher: String,
    /// If set, record last_run_at and row count on this saved-query id.
    saved_query_id: Option<String>,
}

async fn explorer_query_handler(
    State(state): State<AppState>,
    Json(body): Json<ExplorerQueryBody>,
) -> Result<Json<crate::graph::explorer::ExplorerResult>, (StatusCode, String)> {
    let cypher = body.cypher.clone();
    let saved_query_id = body.saved_query_id.clone();
    let db = state.store.db();

    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::explorer::execute_query(&conn, &cypher).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    // Best-effort: update run metadata on the saved query if one was specified.
    if let Some(id) = saved_query_id {
        let count = result.row_count as i64;
        let _ = state.store.mark_saved_query_run(id, count).await;
    }

    Ok(Json(result))
}

// ─── saved queries CRUD (T1-6) ───────────────────────────────────────────────

async fn list_saved_queries_handler(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::graph::SavedQueryRecord>>, (StatusCode, String)> {
    state
        .store
        .list_saved_queries()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(Deserialize)]
struct CreateSavedQueryBody {
    name: String,
    #[serde(default)]
    description: String,
    cypher: String,
}

async fn create_saved_query_handler(
    State(state): State<AppState>,
    Json(body): Json<CreateSavedQueryBody>,
) -> Result<Json<crate::graph::SavedQueryRecord>, (StatusCode, String)> {
    state
        .store
        .create_saved_query(body.name, body.description, body.cypher)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

async fn delete_saved_query_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .store
        .delete_saved_query(id)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ── embedding handlers (T2-1) ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct UpsertEmbeddingsBody {
    records: Vec<crate::graph::EmbeddingRecord>,
}

async fn upsert_embeddings_handler(
    State(state): State<AppState>,
    Json(body): Json<UpsertEmbeddingsBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let count = body.records.len();
    state
        .store
        .write_device_embeddings(body.records)
        .await
        .map(|_| {
            tracing::info!(count, "embedding upsert accepted");
            StatusCode::NO_CONTENT
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(Serialize)]
struct EmbeddingsResponse {
    embeddings: Vec<crate::graph::EmbeddingRecord>,
}

async fn list_embeddings_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<EmbeddingsResponse>, (StatusCode, String)> {
    state
        .store
        .list_device_embeddings(address)
        .await
        .map(|embeddings| Json(EmbeddingsResponse { embeddings }))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ── investigation handlers (T3-1 / T3-2) ─────────────────────────────────────

#[derive(Deserialize)]
struct CreateInvestigationBody {
    detection_id: String,
    device_address: String,
    #[serde(default = "default_trigger")]
    trigger: String,
}
fn default_trigger() -> String {
    "operator".into()
}

#[derive(Deserialize)]
struct CompleteInvestigationBody {
    status: String,
    summary: String,
    #[serde(default)]
    proposal_json: String,
    #[serde(default)]
    tokens_used: i64,
    #[serde(default)]
    cost_usd: f64,
}

#[derive(Serialize)]
struct InvestigationDetailResponse {
    investigation: crate::graph::InvestigationRecord,
    tool_calls: Vec<crate::graph::ToolCallRecord>,
}

async fn list_investigations_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .store
        .list_investigations()
        .await
        .map(|inv| Json(serde_json::json!({ "investigations": inv })))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn create_investigation_handler(
    State(state): State<AppState>,
    Json(body): Json<CreateInvestigationBody>,
) -> Result<Json<crate::graph::InvestigationRecord>, (StatusCode, String)> {
    state
        .store
        .create_investigation(body.detection_id, body.device_address, body.trigger)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn get_investigation_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<InvestigationDetailResponse>, (StatusCode, String)> {
    let inv = state
        .store
        .get_investigation(id.clone())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("investigation {} not found", id),
            )
        })?;
    let tool_calls = state
        .store
        .list_tool_calls(id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(InvestigationDetailResponse {
        investigation: inv,
        tool_calls,
    }))
}

async fn list_tool_calls_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .store
        .list_tool_calls(id)
        .await
        .map(|tc| Json(serde_json::json!({ "tool_calls": tc })))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn complete_investigation_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<CompleteInvestigationBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .store
        .complete_investigation(
            id,
            body.status,
            body.summary,
            body.proposal_json,
            body.tokens_used,
            body.cost_usd,
        )
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ── T4-5 — Governance state endpoint ─────────────────────────────────────────

async fn governance_state_handler(State(state): State<AppState>) -> impl IntoResponse {
    match &state.governor {
        Some(g) => (StatusCode::OK, Json(serde_json::json!(g.snapshot()))).into_response(),
        None => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "governance_not_started"})),
        )
            .into_response(),
    }
}

// ── T5-2 — Grounded incident response ────────────────────────────────────────

async fn grounded_incident_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<crate::mcp_server::GroundedIncidentResponse>, (StatusCode, String)> {
    let detections = state
        .store
        .read_detections(500)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let det = detections.into_iter().find(|d| d.id == id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("DetectionEvent {id} not found"),
        )
    })?;

    let device_address = det.device_address.clone();
    let db = state.store.db();
    let blast = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::queries::blast_radius(&conn, &device_address, 2).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let meta = crate::mcp_server::rule_meta(&det.rule_id);
    let refs = crate::mcp_server::procedural_refs(&det.device_address, &det.rule_id);

    Ok(Json(crate::mcp_server::GroundedIncidentResponse {
        detection: crate::mcp_server::DetectionSummary {
            id: det.id,
            device_address: det.device_address,
            rule_id: det.rule_id,
            severity: det.severity,
            fired_at_ns: det.fired_at_ns,
            features_json: det.features_json,
            remediation_status: det.remediation_status,
            remediation_action: det.remediation_action,
        },
        blast_radius: blast,
        rule_description: meta.map(|m| m.description).unwrap_or(""),
        recurrence_indicators: meta.map(|m| m.recurrence_indicators).unwrap_or(&[]),
        procedural_references: refs,
    }))
}

// ── T5-3 — Self-describing OpenAPI schema endpoint ───────────────────────────

async fn schema_handler() -> Json<serde_json::Value> {
    Json(openapi_schema())
}

async fn openapi_json_handler() -> Json<serde_json::Value> {
    Json(openapi_schema())
}

async fn swagger_ui_handler() -> axum::response::Html<&'static str> {
    axum::response::Html(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8"/>
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Bonsai API — Swagger UI</title>
  <link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist@5/swagger-ui.css">
  <style>body { margin: 0; } .topbar { display: none; }</style>
</head>
<body>
<div id="swagger-ui"></div>
<script src="https://unpkg.com/swagger-ui-dist@5/swagger-ui-bundle.js"></script>
<script>
window.onload = () => {
  SwaggerUIBundle({
    url: "/api/openapi.json",
    dom_id: "#swagger-ui",
    presets: [SwaggerUIBundle.presets.apis, SwaggerUIBundle.SwaggerUIStandalonePreset],
    layout: "BaseLayout",
    deepLinking: true,
    tryItOutEnabled: true,
    filter: true,
    docExpansion: "none",
    defaultModelsExpandDepth: 2,
  });
};
</script>
</body>
</html>"##,
    )
}

fn openapi_schema() -> serde_json::Value {
    let topology_example = load_openapi_example("topology");
    let detections_example = load_openapi_example("detections");
    let incidents_example = load_openapi_example("incidents");
    let readiness_example = load_openapi_example("readiness");
    let operations_example = load_openapi_example("operations");
    let grounded_incident_example = load_openapi_example("grounded_incident");
    let managed_devices_example = load_openapi_example("managed_devices");
    let onboarding_discover_example = load_openapi_example("onboarding_discover");
    let device_detail_example = load_openapi_example("device_detail");
    let device_gnmi_readiness_example = load_openapi_example("device_gnmi_readiness");
    let device_streaming_readiness_example = load_openapi_example("device_streaming_readiness");
    let device_recommendations_example = load_openapi_example("device_recommendations");
    let apply_selected_paths_example = load_openapi_example("apply_selected_paths");
    let setup_status_example = load_openapi_example("setup_status");
    let yang_modules_example = load_openapi_example("yang_modules");
    let yang_search_example = load_openapi_example("yang_search");
    let profiles_example = load_openapi_example("profiles");
    let save_custom_profile_example = load_openapi_example("save_custom_profile");
    let servicenow_test_example = load_openapi_example("servicenow_test");
    let servicenow_sync_example = load_openapi_example("servicenow_aiops_sync");

    serde_json::json!({
        "openapi": "3.0.3",
        "info": {
            "title": "Bonsai Network State Engine API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "REST + SSE API for the Bonsai network state engine. Graph-native, gNMI-first, closed-loop detect-heal. Streaming telemetry from Nokia SR Linux, Cisco IOS-XRd, Juniper cRPD, and Arista cEOS. Browse endpoints by tag. Mutation endpoints require no auth in lab deployments; production deployments should sit behind a TLS-terminating reverse proxy.",
            "x-schema-version": env!("CARGO_PKG_VERSION"),
            "contact": { "name": "Bonsai", "url": "https://github.com/bonsai-network/bonsai" },
            "license": { "name": "MIT" }
        },
        "servers": [{ "url": "http://localhost:3000", "description": "Local lab instance" }],
        "tags": [
            { "name": "Observability", "description": "Topology snapshot, detection events, incidents, blast-radius, path tracing, and SSE live stream" },
            { "name": "Devices & Onboarding", "description": "Device lifecycle management — discovery, subscription path selection, gNMI readiness, enrichment" },
            { "name": "Sites & Environments", "description": "Site and environment archetype management for multi-site topologies" },
            { "name": "YANG & Path Profiles", "description": "YANG module discovery, path search, subscription path overrides, and profile management" },
            { "name": "Enrichment", "description": "NetBox, ServiceNow, and custom enrichment adapters that write business context into the graph" },
            { "name": "Output Adapters", "description": "Splunk HEC, Elasticsearch, ServiceNow EM, and Prometheus remote-write output adapters" },
            { "name": "Credentials", "description": "Device credential vault — all APIs accept alias names only; plaintext credentials never appear in requests or responses" },
            { "name": "Trust & Approvals", "description": "Graduated remediation trust model — human approval gates before autonomous gNMI Set execution" },
            { "name": "Collectors & Assignment", "description": "Distributed collector management and device-to-collector assignment rules" },
            { "name": "Graph Explorer", "description": "Direct Cypher query interface, graph insights, saved queries, and node embedding management" },
            { "name": "Investigations", "description": "AI-assisted investigation sessions with per-tool-call audit trail" },
            { "name": "Integrations", "description": "ServiceNow AIOps and EM integration connectors" },
            { "name": "Governance", "description": "Adaptive resource governor — memory pressure, write pressure, and load-shedding state" },
            { "name": "Operations", "description": "Operational health, daily check results, weekly trends, and readiness probes" },
            { "name": "Test & Verification", "description": "Internal endpoints for CI automation, chaos harness, and AI feedback loop" },
            { "name": "MCP", "description": "Model Context Protocol JSON-RPC 2.0 endpoint for AI agent tool use" },
            { "name": "Schema", "description": "API self-description, OpenAPI spec, and natural-language reference resolution" }
        ],
        "paths": {
            "/api/topology": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Network topology snapshot",
                    "description": "Returns all devices, LLDP fabric links, management-plane links, and BGP neighbour states. Link bytes_total is the sum of in/out octets on both ends for utilisation heatmap colouring.",
                    "responses": {
                        "200": {
                            "description": "Topology snapshot",
                            "content": { "application/json": {
                                "schema": { "$ref": "#/components/schemas/TopologyResponse" },
                                "example": topology_example
                            }}
                        }
                    }
                }
            },
            "/api/detections": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Recent detection events",
                    "description": "Returns the most recent DetectionEvents with associated remediation status. Each row includes severity, rule_id, device_address, features_json for ML inspection, and remediation outcome.",
                    "parameters": [{ "name": "limit", "in": "query", "schema": { "type": "integer", "default": 50 }, "description": "Maximum number of detections to return" }],
                    "responses": { "200": { "description": "Detection list", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/DetectionsResponse" },
                        "example": detections_example
                    }}}}
                }
            },
            "/api/incidents": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Detections grouped into incidents by time window",
                    "description": "Groups recent DetectionEvents into incidents using a sliding time window. Root detection is the highest-topology-degree device in the group. Provides the view ServiceNow EM receives as a correlated alert.",
                    "parameters": [
                        { "name": "window_secs", "in": "query", "schema": { "type": "integer", "default": 30 }, "description": "Time window in seconds for grouping co-occurring detections" },
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "default": 200 }, "description": "Maximum detections to consider before grouping" }
                    ],
                    "responses": { "200": { "description": "Incident list", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/IncidentsResponse" },
                        "example": incidents_example
                    }}}}
                }
            },
            "/api/incidents/grouped": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Detections grouped by rule and device (alternate view)",
                    "description": "Returns detections pre-grouped by rule_id and device for the dashboard aggregated view. Distinct from /api/incidents which uses a sliding time-window.",
                    "parameters": [
                        { "name": "window_secs", "in": "query", "schema": { "type": "integer", "default": 30 }},
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "default": 200 }}
                    ],
                    "responses": { "200": { "description": "Grouped incident list" }}
                }
            },
            "/api/incidents/{id}/grounded": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Grounded incident bundle",
                    "description": "Returns a detection event enriched with topological blast radius, rule documentation, recurrence indicators, and procedural references. Three-source grounding: topology (which nodes/services are impacted) + procedure (what the runbook says) + live state (current device telemetry). This is the unit of value bonsai delivers to a ServiceNow operator.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }, "description": "Root DetectionEvent UUID" }],
                    "responses": {
                        "200": { "description": "Grounded incident bundle", "content": { "application/json": {
                            "schema": { "type": "object" },
                            "example": grounded_incident_example
                        }}},
                        "404": { "description": "Detection not found" }
                    }
                }
            },
            "/api/blast-radius/{address}": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Blast radius from a device",
                    "description": "Returns all devices, applications, and active detections reachable within max_hops physical network hops from the origin device. Used to bound the service impact of a fault before executing remediation.",
                    "parameters": [
                        { "name": "address", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Management IP address of origin device" },
                        { "name": "max_hops", "in": "query", "schema": { "type": "integer", "default": 2, "minimum": 1, "maximum": 5 }, "description": "Maximum LLDP hops to traverse" }
                    ],
                    "responses": { "200": { "description": "Blast radius: affected devices, services, and active detections" }}
                }
            },
            "/api/trace/{id}": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Closed-loop trace for a detection",
                    "description": "Returns the ordered sequence of steps for a single DetectionEvent: trigger (gNMI state change) → rule evaluation → detection fired → remediation proposed → approval → gNMI Set executed → verification. Each step has a timestamp and outcome.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }, "description": "DetectionEvent UUID" }],
                    "responses": { "200": { "description": "Ordered trace steps" }}
                }
            },
            "/api/path": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Shortest topology path between two devices",
                    "description": "Returns the shortest LLDP-derived physical path between two devices. Useful for understanding propagation paths for link faults.",
                    "parameters": [
                        { "name": "src", "in": "query", "required": true, "schema": { "type": "string" }, "description": "Source device management IP" },
                        { "name": "dst", "in": "query", "required": true, "schema": { "type": "string" }, "description": "Destination device management IP" }
                    ],
                    "responses": { "200": { "description": "Hop list and link list along shortest path" }}
                }
            },
            "/api/events": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "SSE live event stream",
                    "description": "Server-Sent Events stream of all BonsaiEvents (StateChangeEvent, DetectionEvent, RemediationEvent). Each event is a JSON object with device_address, event_type, detail_json, and occurred_at_ns. Clients should reconnect on disconnect; the stream has no backfill.",
                    "responses": {
                        "200": {
                            "description": "text/event-stream — continuous SSE feed",
                            "content": { "text/event-stream": { "schema": { "type": "string" }}}
                        }
                    }
                }
            },
            "/api/readiness": {
                "get": {
                    "tags": ["Operations"],
                    "summary": "Readiness probe",
                    "description": "Returns HTTP 200 when the bonsai core is ready to serve traffic (graph DB open, registry loaded). Returns 503 during startup. Safe to use as a Kubernetes/Docker HEALTHCHECK target.",
                    "responses": {
                        "200": {
                            "description": "Core is ready",
                            "content": { "application/json": {
                                "schema": { "$ref": "#/components/schemas/ReadinessResponse" },
                                "example": readiness_example
                            }}
                        },
                        "503": { "description": "Core is starting up" }
                    }
                }
            },
            "/api/operations": {
                "get": {
                    "tags": ["Operations"],
                    "summary": "Operational health summary",
                    "description": "Returns current counts of detection events, state change events, remediations, device counts, event bus depth, archive stats (bytes, file count), RSS memory usage vs budget, and disk usage. This is the primary health dashboard endpoint.",
                    "responses": { "200": { "description": "Operations snapshot", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/OperationsResponse" },
                        "example": operations_example
                    }}}}
                }
            },
            "/api/operations/daily-check": {
                "get": {
                    "tags": ["Operations"],
                    "summary": "Latest daily check results",
                    "description": "Returns the most recent daily_check.sh result with pass/fail/skip/prereq_missing breakdowns per driver. Used by the AI feedback loop to surface operational regressions.",
                    "responses": { "200": { "description": "Daily check result JSON" }}
                }
            },
            "/api/operations/weekly-trend": {
                "get": {
                    "tags": ["Operations"],
                    "summary": "7-day operational trend",
                    "description": "Returns per-day aggregates of detection counts, remediation outcomes, archive growth, and chaos injection counts for the trailing 7 days. Drives the weekly trend sparklines in the Operations workspace.",
                    "responses": { "200": { "description": "7-day trend data" }}
                }
            },
            "/api/onboarding/devices": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "List managed devices",
                    "description": "Returns all devices currently in the device registry with their gNMI subscription status, collector assignment, health, and last-seen timestamps.",
                    "responses": { "200": { "description": "Managed device list", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/ManagedDevicesResponse" },
                        "example": managed_devices_example
                    }}}}
                },
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Add a managed device",
                    "description": "Adds a device to the registry with a credential alias and optional role hint. Bonsai will initiate a gNMI Capabilities exchange and subscribe to the paths appropriate for the device's role.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AddDeviceRequest" }}}
                    },
                    "responses": {
                        "200": { "description": "Device added" },
                        "400": { "description": "Invalid request or unreachable device" }
                    }
                }
            },
            "/api/onboarding/devices/with_paths": {
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Add device with explicit path selection",
                    "description": "Adds a device and immediately applies a specific set of subscription paths (from a prior /api/devices/{address}/recommendations response). Bypasses the auto-discovery step.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object" }}}
                    },
                    "responses": { "200": { "description": "Device added with selected paths" }}
                }
            },
            "/api/onboarding/devices/remove": {
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Remove a managed device",
                    "description": "Removes a device from the registry, cancels active gNMI subscriptions, and removes associated graph nodes. Does not delete historical StateChangeEvents.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "address": { "type": "string" }}, "required": ["address"] }}}
                    },
                    "responses": { "200": { "description": "Device removed" }}
                }
            },
            "/api/onboarding/devices/remove-impact": {
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Preview blast radius before removing a device",
                    "description": "Returns which graph nodes, detection rules, and enrichment linkages would be affected by removing a device. Use before /api/onboarding/devices/remove to understand impact.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "address": { "type": "string" }}, "required": ["address"] }}}
                    },
                    "responses": { "200": { "description": "Impact assessment" }}
                }
            },
            "/api/onboarding/devices/bulk": {
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Bulk device action",
                    "description": "Applies an action (add / remove / reparse) to multiple devices atomically. Returns per-device success/error results.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "action": { "type": "string", "enum": ["add", "remove", "reparse"] }, "addresses": { "type": "array", "items": { "type": "string" }}}}}}
                    },
                    "responses": { "200": { "description": "Per-device results" }}
                }
            },
            "/api/onboarding/discover": {
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Discovery wizard — connect and probe a device",
                    "description": "Connects to a device via gNMI, exchanges Capabilities, and returns vendor identification, available YANG modules, and recommended subscription paths. This is step 1 of the onboarding wizard; the result feeds into /api/devices/{address}/recommendations.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/DiscoverRequest" },
                            "example": {
                                "address": "172.100.103.31:57400",
                                "credential_alias": "lab-srlinux",
                                "tls_domain": "leaf1",
                                "role_hint": "leaf",
                                "environment_archetype": "data_center"
                            }
                        }}
                    },
                    "responses": {
                        "200": { "description": "Discovery report with vendor, modules, and recommended paths", "content": { "application/json": {
                            "schema": { "type": "object", "additionalProperties": true },
                            "example": onboarding_discover_example
                        }}},
                        "400": { "description": "Unreachable or unsupported device" }
                    }
                }
            },
            "/api/devices/{address}": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Device detail",
                    "description": "Returns full device detail: interfaces, BGP sessions, BFD sessions, IS-IS/OSPF adjacencies, subscription paths, health, enrichment linkages (site, environment, NetBox CI, ServiceNow CI).",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Management IP address" }],
                    "responses": {
                        "200": { "description": "Device detail", "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/DeviceDetailResponse" },
                            "example": device_detail_example
                        }}},
                        "404": { "description": "Device not found in graph" }
                    }
                }
            },
            "/api/devices/{address}/gnmi-readiness": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "gNMI subscription readiness per path",
                    "description": "Returns per-path subscription status for a device: which OpenConfig paths are being streamed, which are absent from Capabilities, and which have known issues (from config/gnmi_known_issues/).",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Per-path readiness report", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/DeviceGnmiReadinessResponse" },
                        "example": device_gnmi_readiness_example
                    }}}}
                }
            },
            "/api/devices/{address}/streaming-readiness": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Streaming readiness assessment",
                    "description": "Runs a live gNMI Capabilities exchange and returns a full streaming readiness report: vendor, supported paths, recommended profile, and any blocking issues.",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Streaming readiness report", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/DeviceStreamingReadinessResponse" },
                        "example": device_streaming_readiness_example
                    }}}}
                }
            },
            "/api/devices/{address}/recommendations": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Path recommendations for a device",
                    "description": "Returns the recommended gNMI subscription path set for this device based on its role, vendor, and discovered YANG capability set. Groups paths by category (interfaces, BGP, OSPF, IS-IS, LLDP, platform).",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Recommended path groups", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/DeviceRecommendationsResponse" },
                        "example": device_recommendations_example
                    }}}}
                }
            },
            "/api/devices/{address}/selected-paths": {
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Apply selected subscription paths",
                    "description": "Persists the operator-selected subscription paths for a device (from the onboarding wizard) and restarts the gNMI subscription with the new path set. This is the commit step of the onboarding wizard.",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/ApplySelectedPathsRequest" },
                            "example": {
                                "selected_paths": [
                                    { "path": "/interfaces/interface/state/counters", "origin": "openconfig-interfaces", "reason": "baseline_counters" },
                                    { "path": "/network-instances/network-instance/protocols/protocol/bgp/neighbors/neighbor/state/session-state", "origin": "openconfig-bgp", "reason": "bgp_state" }
                                ]
                            }
                        }}
                    },
                    "responses": { "200": { "description": "Paths applied and subscription restarted", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/ApplySelectedPathsResponse" },
                        "example": apply_selected_paths_example
                    }}}}
                }
            },
            "/api/devices/{address}/config-history": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "gNMI subscription path change history",
                    "description": "Returns the history of subscription path configuration changes for a device, including who changed what and when.",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Config history" }}
                }
            },
            "/api/devices/{address}/reparse": {
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Re-parse device state from archive",
                    "description": "Replays archived Parquet telemetry for a device through the ingest pipeline to rebuild graph state. Useful after a schema migration or rule change.",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Reparse initiated" }}
                }
            },
            "/api/devices/{address}/enrichment": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Enrichment data for a device",
                    "description": "Returns the business context enrichment for a device: NetBox device/interface records, ServiceNow CI linkages, site assignment, and environment membership.",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Enrichment data" }}
                }
            },
            "/api/setup/status": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Setup wizard completion status",
                    "description": "Returns the first-run status Bonsai uses to decide whether to route a user into the onboarding flow. Current fields reflect whether non-default environments exist and whether credentials or devices have been configured.",
                    "responses": { "200": { "description": "Setup status", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/SetupStatusResponse" },
                        "example": setup_status_example
                    }}}}
                }
            },
            "/api/sites": {
                "get": {
                    "tags": ["Sites & Environments"],
                    "summary": "List all sites",
                    "description": "Returns all site records with name, location, and associated device count. Sites are first-class graph entities used for multi-site topology segmentation.",
                    "responses": { "200": { "description": "Site list" }}
                },
                "post": {
                    "tags": ["Sites & Environments"],
                    "summary": "Create or update a site",
                    "description": "Upserts a site record. Site names must be unique. Sites can be assigned to environments.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SiteRecord" }}}
                    },
                    "responses": { "200": { "description": "Site upserted" }}
                }
            },
            "/api/sites/{id}": {
                "get": {
                    "tags": ["Sites & Environments"],
                    "summary": "Site summary with device detail",
                    "description": "Returns detailed site view: all devices assigned to this site with their health, role, vendor, and active detection counts.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Site name or UUID" }],
                    "responses": { "200": { "description": "Site summary" }}
                }
            },
            "/api/sites/remove": {
                "post": {
                    "tags": ["Sites & Environments"],
                    "summary": "Remove a site",
                    "description": "Removes a site record. Devices assigned to this site become unassigned; they are not removed from monitoring.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "id": { "type": "string" }}, "required": ["id"] }}}
                    },
                    "responses": { "200": { "description": "Site removed" }}
                }
            },
            "/api/environments": {
                "get": {
                    "tags": ["Sites & Environments"],
                    "summary": "List all environments",
                    "description": "Returns all environment records with their archetype (data_center, service_provider, home_lab), assigned sites, and resource governance profile.",
                    "responses": { "200": { "description": "Environment list" }}
                },
                "post": {
                    "tags": ["Sites & Environments"],
                    "summary": "Create an environment",
                    "description": "Creates a new environment with an archetype. The archetype determines default resource governance parameters and path profile selection.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/EnvironmentRecord" }}}
                    },
                    "responses": { "200": { "description": "Environment created" }}
                }
            },
            "/api/environments/update": {
                "post": {
                    "tags": ["Sites & Environments"],
                    "summary": "Update an environment",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/EnvironmentRecord" }}}
                    },
                    "responses": { "200": { "description": "Environment updated" }}
                }
            },
            "/api/environments/remove": {
                "post": {
                    "tags": ["Sites & Environments"],
                    "summary": "Remove an environment",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "id": { "type": "string" }}, "required": ["id"] }}}
                    },
                    "responses": { "200": { "description": "Environment removed" }}
                }
            },
            "/api/environments/assign-site": {
                "post": {
                    "tags": ["Sites & Environments"],
                    "summary": "Assign a site to an environment",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "site_id": { "type": "string" }, "environment_id": { "type": "string" }}, "required": ["site_id", "environment_id"] }}}
                    },
                    "responses": { "200": { "description": "Site assigned" }}
                }
            },
            "/api/yang/modules": {
                "get": {
                    "tags": ["YANG & Path Profiles"],
                    "summary": "List available YANG modules",
                    "description": "Returns all YANG modules available in the local YANG library, grouped by source (OpenConfig, vendor-native, universal). Modules are discovered by the discover_yang_paths.py tooling.",
                    "responses": { "200": { "description": "YANG module list", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/YangModulesResponse" },
                        "example": yang_modules_example
                    }}}}
                }
            },
            "/api/yang/search": {
                "get": {
                    "tags": ["YANG & Path Profiles"],
                    "summary": "Search YANG paths",
                    "description": "Full-text search across YANG path names, descriptions, and module names. Returns matching paths with their module, access type (read-only / read-write), and any known gNMI streaming issues.",
                    "parameters": [{ "name": "q", "in": "query", "required": true, "schema": { "type": "string" }, "description": "Search query, e.g. 'bgp neighbor state'" }],
                    "responses": { "200": { "description": "YANG path search results", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/YangSearchResponse" },
                        "example": yang_search_example
                    }}}}
                }
            },
            "/api/profiles": {
                "get": {
                    "tags": ["YANG & Path Profiles"],
                    "summary": "List path profiles",
                    "description": "Returns all built-in and custom path profiles (dc_leaf_minimal, dc_spine_standard, sp_pe_full, etc.). Each profile is a named collection of gNMI subscription paths for a device role.",
                    "responses": { "200": { "description": "Profile list", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/ProfilesResponse" },
                        "example": profiles_example
                    }}}}
                }
            },
            "/api/profiles/save-custom": {
                "post": {
                    "tags": ["YANG & Path Profiles"],
                    "summary": "Save a custom path profile",
                    "description": "Persists a custom path profile to the catalogue directory. Custom profiles are versioned alongside built-in profiles and appear in /api/profiles.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/SaveCustomProfileRequest" },
                            "example": {
                                "name": "dc_leaf_bgp_minimal",
                                "description": "Leaf profile with interface counters and BGP state only",
                                "rationale": "Lean onboarding profile for first-pass lab validation",
                                "environment": ["data_center"],
                                "vendor_scope": ["nokia", "cisco", "juniper", "arista"],
                                "roles": ["leaf"],
                                "paths": [
                                    { "path": "/interfaces/interface/state/counters", "origin": "openconfig-interfaces", "reason": "baseline_counters" },
                                    { "path": "/network-instances/network-instance/protocols/protocol/bgp/neighbors/neighbor/state/session-state", "origin": "openconfig-bgp", "reason": "bgp_state" }
                                ]
                            }
                        }}
                    },
                    "responses": { "200": { "description": "Profile saved", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/SaveCustomProfileResponse" },
                        "example": save_custom_profile_example
                    }}}}
                }
            },
            "/api/overrides": {
                "get": {
                    "tags": ["YANG & Path Profiles"],
                    "summary": "List gNMI path overrides",
                    "description": "Returns per-device gNMI path overrides — paths that are force-enabled or force-disabled relative to the device's base profile. Overrides survive profile updates.",
                    "responses": { "200": { "description": "Override list" }}
                },
                "post": {
                    "tags": ["YANG & Path Profiles"],
                    "summary": "Add a path override",
                    "description": "Adds a force-enable or force-disable override for a specific gNMI path on a specific device.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "address": { "type": "string" }, "path": { "type": "string" }, "mode": { "type": "string", "enum": ["enable", "disable"] }}, "required": ["address", "path", "mode"] }}}
                    },
                    "responses": { "200": { "description": "Override added" }}
                }
            },
            "/api/overrides/remove": {
                "post": {
                    "tags": ["YANG & Path Profiles"],
                    "summary": "Remove a path override",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "address": { "type": "string" }, "path": { "type": "string" }}, "required": ["address", "path"] }}}
                    },
                    "responses": { "200": { "description": "Override removed" }}
                }
            },
            "/api/enrichment": {
                "get": {
                    "tags": ["Enrichment"],
                    "summary": "List enrichment adapters",
                    "description": "Returns all configured enrichment adapters with their type (netbox, servicenow, custom), connection state, last run timestamp, and enrichment statistics (nodes enriched, errors).",
                    "responses": { "200": { "description": "Enricher list" }}
                },
                "post": {
                    "tags": ["Enrichment"],
                    "summary": "Add or update an enrichment adapter",
                    "description": "Upserts an enrichment adapter. Supported types: netbox (URL + token alias), servicenow (instance URL + credential alias), custom (Python module path).",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "config": { "type": "object", "properties": { "name": { "type": "string" }, "type": { "type": "string", "enum": ["netbox", "servicenow", "custom"] }, "url": { "type": "string" }, "credential_alias": { "type": "string" }}}}}}}
                    },
                    "responses": { "200": { "description": "Enricher upserted" }}
                }
            },
            "/api/enrichment/remove": {
                "post": {
                    "tags": ["Enrichment"],
                    "summary": "Remove an enrichment adapter",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }}, "required": ["name"] }}}
                    },
                    "responses": { "200": { "description": "Enricher removed" }}
                }
            },
            "/api/enrichment/test": {
                "post": {
                    "tags": ["Enrichment"],
                    "summary": "Test enrichment adapter connectivity",
                    "description": "Verifies that bonsai can reach the enrichment source and authenticate. Does not write any graph state.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }}, "required": ["name"] }}}
                    },
                    "responses": { "200": { "description": "Connection test result with success flag and message" }}
                }
            },
            "/api/enrichment/run": {
                "post": {
                    "tags": ["Enrichment"],
                    "summary": "Trigger an enrichment run",
                    "description": "Immediately runs an enrichment cycle for the named adapter outside of the scheduled interval. Returns a run report with nodes enriched and any errors.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }}, "required": ["name"] }}}
                    },
                    "responses": { "200": { "description": "Enrichment run report" }}
                }
            },
            "/api/enrichment/audit": {
                "get": {
                    "tags": ["Enrichment"],
                    "summary": "Enrichment audit log",
                    "description": "Returns the enrichment audit log: every enrichment run with timestamp, adapter name, outcome, nodes written, and any errors.",
                    "responses": { "200": { "description": "Audit log entries" }}
                }
            },
            "/api/adapters": {
                "get": {
                    "tags": ["Output Adapters"],
                    "summary": "List output adapters",
                    "description": "Returns all configured output adapters with their type (splunk_hec, elasticsearch, servicenow_em, prometheus), run state, cursor position, and last push statistics.",
                    "responses": { "200": { "description": "Adapter list" }}
                },
                "post": {
                    "tags": ["Output Adapters"],
                    "summary": "Add or update an output adapter",
                    "description": "Upserts an output adapter. Supported types: splunk_hec (HEC URL + token alias), elasticsearch (URL + credential alias), servicenow_em (instance URL + credential alias), prometheus (remote-write URL).",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object" }}}
                    },
                    "responses": { "200": { "description": "Adapter upserted" }}
                }
            },
            "/api/adapters/remove": {
                "post": {
                    "tags": ["Output Adapters"],
                    "summary": "Remove an output adapter",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }}, "required": ["name"] }}}
                    },
                    "responses": { "200": { "description": "Adapter removed" }}
                }
            },
            "/api/adapters/test": {
                "post": {
                    "tags": ["Output Adapters"],
                    "summary": "Test output adapter connectivity",
                    "description": "Verifies that bonsai can reach the output destination and authenticate. Sends a test payload; does not affect the cursor position.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }}, "required": ["name"] }}}
                    },
                    "responses": { "200": { "description": "Test result" }}
                }
            },
            "/api/adapters/audit": {
                "get": {
                    "tags": ["Output Adapters"],
                    "summary": "Output adapter audit log",
                    "description": "Returns push history per adapter: timestamp, records pushed, bytes sent, errors, and cursor position.",
                    "responses": { "200": { "description": "Audit log entries" }}
                }
            },
            "/api/credentials": {
                "get": {
                    "tags": ["Credentials"],
                    "summary": "List credential aliases",
                    "description": "Returns all credential aliases stored in the vault. Never returns plaintext credentials — only alias names, associated device count, and last-used timestamp.",
                    "responses": { "200": { "description": "Credential alias list" }}
                },
                "post": {
                    "tags": ["Credentials"],
                    "summary": "Add a credential",
                    "description": "Stores a new credential in the age-encrypted vault under an alias. The request body contains the alias name and env var names that hold the plaintext username/password — plaintext values must never appear in the request body.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "alias": { "type": "string" }, "username_env": { "type": "string", "description": "Env var name containing the username" }, "password_env": { "type": "string", "description": "Env var name containing the password" }}, "required": ["alias", "username_env", "password_env"] }}}
                    },
                    "responses": { "200": { "description": "Credential stored" }}
                }
            },
            "/api/credentials/update": {
                "post": {
                    "tags": ["Credentials"],
                    "summary": "Update an existing credential",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "alias": { "type": "string" }, "username_env": { "type": "string" }, "password_env": { "type": "string" }}, "required": ["alias"] }}}
                    },
                    "responses": { "200": { "description": "Credential updated" }}
                }
            },
            "/api/credentials/remove": {
                "post": {
                    "tags": ["Credentials"],
                    "summary": "Remove a credential alias",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "alias": { "type": "string" }}, "required": ["alias"] }}}
                    },
                    "responses": { "200": { "description": "Credential removed" }}
                }
            },
            "/api/credentials/test": {
                "post": {
                    "tags": ["Credentials"],
                    "summary": "Test a credential against a device",
                    "description": "Attempts a gNMI Capabilities RPC using the stored credential to verify it is valid for the target device.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "alias": { "type": "string" }, "address": { "type": "string" }}, "required": ["alias", "address"] }}}
                    },
                    "responses": { "200": { "description": "Test result with success flag" }}
                }
            },
            "/api/approvals": {
                "get": {
                    "tags": ["Trust & Approvals"],
                    "summary": "List pending remediation approvals",
                    "description": "Returns all pending RemediationProposals awaiting operator approval. Each proposal includes the proposed gNMI Set command, the triggering DetectionEvent, and the estimated blast radius.",
                    "responses": { "200": { "description": "Pending approval list" }}
                },
                "post": {
                    "tags": ["Trust & Approvals"],
                    "summary": "Create a manual remediation proposal",
                    "description": "Creates a manual remediation proposal for operator review. Used when an operator wants to test the approval workflow with a specific remediation action.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object" }}}
                    },
                    "responses": { "200": { "description": "Proposal created" }}
                }
            },
            "/api/approvals/{id}/approve": {
                "post": {
                    "tags": ["Trust & Approvals"],
                    "summary": "Approve a remediation proposal",
                    "description": "Approves a remediation proposal, triggering execution via gNMI Set. Each approval increments the consecutive-success counter toward graduated autonomous trust.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }}],
                    "responses": {
                        "200": { "description": "Approved and executing" },
                        "404": { "description": "Proposal not found" }
                    }
                }
            },
            "/api/approvals/{id}/reject": {
                "post": {
                    "tags": ["Trust & Approvals"],
                    "summary": "Reject a remediation proposal",
                    "description": "Rejects a remediation proposal and resets the consecutive-success counter for this rule+device, preventing graduation to autonomous trust.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }}],
                    "responses": { "200": { "description": "Rejected" }}
                }
            },
            "/api/approvals/{id}/rollback": {
                "post": {
                    "tags": ["Trust & Approvals"],
                    "summary": "Rollback an executed remediation",
                    "description": "Issues a gNMI Set to undo the effect of an already-executed remediation. Marks the rollback window as used; a given remediation can only be rolled back once.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }}],
                    "responses": {
                        "200": { "description": "Rollback initiated" },
                        "409": { "description": "Rollback window expired or already used" }
                    }
                }
            },
            "/api/trust": {
                "get": {
                    "tags": ["Trust & Approvals"],
                    "summary": "List trust records",
                    "description": "Returns the graduated trust state for every rule+device combination: current trust level (manual_only, auto_with_notification, auto_silent), consecutive success count, and graduation threshold.",
                    "responses": { "200": { "description": "Trust record list" }}
                }
            },
            "/api/trust/graduate": {
                "post": {
                    "tags": ["Trust & Approvals"],
                    "summary": "Manually graduate a trust record",
                    "description": "Forces a trust record to a higher trust level without waiting for the consecutive-success threshold. Requires operator intent to be explicit.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "rule_id": { "type": "string" }, "device_address": { "type": "string" }, "target_level": { "type": "string", "enum": ["manual_only", "auto_with_notification", "auto_silent"] }}, "required": ["rule_id", "device_address", "target_level"] }}}
                    },
                    "responses": { "200": { "description": "Trust record graduated" }}
                }
            },
            "/api/collectors": {
                "get": {
                    "tags": ["Collectors & Assignment"],
                    "summary": "List distributed collectors",
                    "description": "Returns all collector instances registered with the core: address, runtime mode, last heartbeat, device count, and queue statistics.",
                    "responses": { "200": { "description": "Collector list" }}
                }
            },
            "/api/assignment/rules": {
                "get": {
                    "tags": ["Collectors & Assignment"],
                    "summary": "List collector assignment rules",
                    "description": "Returns the ordered list of assignment rules that determine which collector handles which devices. Rules are evaluated in order; first match wins.",
                    "responses": { "200": { "description": "Assignment rule list" }}
                },
                "post": {
                    "tags": ["Collectors & Assignment"],
                    "summary": "Replace collector assignment rules",
                    "description": "Replaces the full assignment rule set atomically. All devices are re-evaluated against the new rules and reassigned if necessary.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "rules": { "type": "array", "items": { "type": "object" }}}}}}
                    },
                    "responses": { "200": { "description": "Rules replaced, reassignment triggered" }}
                }
            },
            "/api/assignment/status": {
                "get": {
                    "tags": ["Collectors & Assignment"],
                    "summary": "Current device-to-collector assignment status",
                    "description": "Returns per-device collector assignment: which collector is handling each device, the assignment rule that matched, and any assignment warnings.",
                    "responses": { "200": { "description": "Assignment status" }}
                }
            },
            "/api/assignment/override": {
                "post": {
                    "tags": ["Collectors & Assignment"],
                    "summary": "Override collector assignment for a device",
                    "description": "Forces a specific device to a specific collector regardless of assignment rules. Overrides are persisted and survive restarts.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "address": { "type": "string" }, "collector_id": { "type": "string" }}, "required": ["address", "collector_id"] }}}
                    },
                    "responses": { "200": { "description": "Override applied" }}
                }
            },
            "/api/graph/insights": {
                "get": {
                    "tags": ["Graph Explorer"],
                    "summary": "Graph structure insights",
                    "description": "Returns high-level graph statistics: node counts by label, edge counts by type, graph density, average degree, and any structural anomalies (isolated nodes, missing enrichment linkages).",
                    "responses": { "200": { "description": "Graph insights" }}
                }
            },
            "/api/explorer/query": {
                "post": {
                    "tags": ["Graph Explorer"],
                    "summary": "Execute a Cypher query",
                    "description": "Executes a read-only Cypher query against the LadybugDB graph. Mutations (CREATE, MERGE, SET, DELETE) are rejected. Returns rows as JSON arrays.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "query": { "type": "string", "description": "Cypher query string, e.g. MATCH (d:Device) RETURN d.address, d.hostname" }}, "required": ["query"] }}}
                    },
                    "responses": {
                        "200": { "description": "Query result rows" },
                        "400": { "description": "Invalid or disallowed Cypher" }
                    }
                }
            },
            "/api/explorer/saved-queries": {
                "get": {
                    "tags": ["Graph Explorer"],
                    "summary": "List saved Cypher queries",
                    "description": "Returns saved queries with their name, Cypher text, description, and last-run timestamp.",
                    "responses": { "200": { "description": "Saved query list" }}
                },
                "post": {
                    "tags": ["Graph Explorer"],
                    "summary": "Save a Cypher query",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }, "query": { "type": "string" }, "description": { "type": "string" }}, "required": ["name", "query"] }}}
                    },
                    "responses": { "200": { "description": "Query saved" }}
                }
            },
            "/api/explorer/saved-queries/{id}/delete": {
                "post": {
                    "tags": ["Graph Explorer"],
                    "summary": "Delete a saved query",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Query deleted" }}
                }
            },
            "/api/graph/embeddings/upsert": {
                "post": {
                    "tags": ["Graph Explorer"],
                    "summary": "Upsert node embeddings",
                    "description": "Stores vector embeddings for graph nodes (Device, Interface, BgpNeighbor). Used by the GNN training pipeline to persist learned representations.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object" }}}
                    },
                    "responses": { "200": { "description": "Embeddings stored" }}
                }
            },
            "/api/graph/embeddings/{address}": {
                "get": {
                    "tags": ["Graph Explorer"],
                    "summary": "Get embeddings for a device",
                    "description": "Returns stored vector embeddings for a device and its associated interface and BGP neighbor nodes.",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Embedding vectors" }}
                }
            },
            "/api/investigations": {
                "get": {
                    "tags": ["Investigations"],
                    "summary": "List investigations",
                    "description": "Returns all investigation sessions with their status (open, complete), associated detection IDs, and tool-call counts.",
                    "responses": { "200": { "description": "Investigation list" }}
                },
                "post": {
                    "tags": ["Investigations"],
                    "summary": "Create an investigation",
                    "description": "Opens a new AI-assisted investigation session anchored to a DetectionEvent or a natural-language problem statement. The session accumulates tool calls and findings.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "detection_id": { "type": "string" }, "problem": { "type": "string" }}}}}
                    },
                    "responses": { "200": { "description": "Investigation created with ID" }}
                }
            },
            "/api/investigations/{id}": {
                "get": {
                    "tags": ["Investigations"],
                    "summary": "Get investigation detail",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }}],
                    "responses": {
                        "200": { "description": "Investigation detail" },
                        "404": { "description": "Investigation not found" }
                    }
                }
            },
            "/api/investigations/{id}/tool-calls": {
                "get": {
                    "tags": ["Investigations"],
                    "summary": "List tool calls for an investigation",
                    "description": "Returns the ordered audit trail of tool calls made during an investigation session: tool name, input, output, and timestamp.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }}],
                    "responses": { "200": { "description": "Tool call audit trail" }}
                }
            },
            "/api/investigations/{id}/complete": {
                "post": {
                    "tags": ["Investigations"],
                    "summary": "Complete an investigation",
                    "description": "Closes an investigation session with a summary finding and optional recommended action.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }}],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "finding": { "type": "string" }, "recommended_action": { "type": "string" }}, "required": ["finding"] }}}
                    },
                    "responses": { "200": { "description": "Investigation closed" }}
                }
            },
            "/api/integrations/servicenow/test": {
                "post": {
                    "tags": ["Integrations"],
                    "summary": "Test ServiceNow connectivity",
                    "description": "Verifies bonsai can reach the ServiceNow instance and authenticate with the configured credential alias. Checks EM (Event Management) and CMDB table access.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/SnowTestRequest" },
                            "example": {
                                "instance_url": "https://dev394753.service-now.com",
                                "credential_alias": "servicenow-pdi"
                            }
                        }}
                    },
                    "responses": { "200": { "description": "Connectivity test result", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/SnowTestResponse" },
                        "example": servicenow_test_example
                    }}}}
                }
            },
            "/api/integrations/servicenow/aiops/sync": {
                "post": {
                    "tags": ["Integrations"],
                    "summary": "Sync topology to ServiceNow AIOps",
                    "description": "Pushes current bonsai topology graph to ServiceNow CMDB as CI records with CONNECTED_TO relationships. Idempotent — existing CIs are updated in place.",
                    "responses": { "200": { "description": "Sync report with CI counts", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/SnowAiopsSyncResponse" },
                        "example": servicenow_sync_example
                    }}}}
                }
            },
            "/api/governance/state": {
                "get": {
                    "tags": ["Governance"],
                    "summary": "Adaptive resource governance state",
                    "description": "Returns current governance profile, active policies (write_pressure_active, memory_pressure_active, load_shedding), shedding statistics, and recent governance actions. Memory at >90% of budget triggers memory_pressure_active.",
                    "responses": { "200": { "description": "Governance state" }}
                }
            },
            "/api/_test/status": {
                "get": {
                    "tags": ["Test & Verification"],
                    "summary": "Test driver health status",
                    "description": "Returns the results of all registered test drivers (api_driver, event_driver, ui_driver). Used by the Gemini AI feedback loop to surface test regressions. Each driver result includes pass/fail/skip counts and a last-run timestamp.",
                    "responses": { "200": { "description": "Test driver status aggregation" }}
                }
            },
            "/api/_test/inject_detection": {
                "post": {
                    "tags": ["Test & Verification"],
                    "summary": "Inject a synthetic detection event",
                    "description": "Publishes a synthetic DetectionEvent on the event bus for testing the remediation and output adapter pipelines. The event is written to the graph and flows through the full detect-heal loop. Do not use in production.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "rule_id": { "type": "string" }, "device_address": { "type": "string" }, "severity": { "type": "string", "enum": ["critical", "warn", "info"] }}, "required": ["rule_id", "device_address"] }}}
                    },
                    "responses": { "200": { "description": "Detection injected" }}
                }
            },
            "/api/_test/syslog/parse": {
                "post": {
                    "tags": ["Test & Verification"],
                    "summary": "Parse one syslog fixture",
                    "description": "Internal parser-validation endpoint used by the fixture-driven syslog smoke. Parses one raw syslog line, extracts SyslogFacts using the configured vendor pattern catalogue, and reports whether the line matches a config-change trigger pattern.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "raw": { "type": "string" }, "vendor": { "type": "string" }, "transport": { "type": "string" }, "peer_addr": { "type": "string" }}, "required": ["raw", "vendor"] }}}
                    },
                    "responses": { "200": { "description": "Parsed syslog event plus extracted facts" }}
                }
            },
            "/mcp": {
                "post": {
                    "tags": ["MCP"],
                    "summary": "MCP JSON-RPC 2.0 endpoint",
                    "description": "Model Context Protocol server for AI agent tool use. Supports initialize, tools/list, and tools/call. Available tools: get_incident (fetch grounded incident), query_devices (filter device list), get_device_blast_radius (impact assessment), list_active_detections (current anomalies), query_graph (read-only Cypher). Binds to localhost only; not exposed externally.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": {
                            "type": "object",
                            "properties": {
                                "jsonrpc": { "type": "string", "enum": ["2.0"] },
                                "id": { "description": "Request ID (integer, string, or null)" },
                                "method": { "type": "string", "enum": ["initialize", "tools/list", "tools/call"] },
                                "params": { "type": "object" }
                            },
                            "required": ["jsonrpc", "id", "method"]
                        }}}
                    },
                    "responses": { "200": { "description": "JSON-RPC 2.0 response object" }}
                }
            },
            "/api/schema": {
                "get": {
                    "tags": ["Schema"],
                    "summary": "OpenAPI 3 specification (legacy path)",
                    "description": "Returns the full OpenAPI 3 specification. Prefer /api/openapi.json which is the canonical path served by the Swagger UI infrastructure.",
                    "responses": { "200": { "description": "OpenAPI 3 JSON" }}
                }
            },
            "/api/openapi.json": {
                "get": {
                    "tags": ["Schema"],
                    "summary": "OpenAPI 3 specification",
                    "description": "Returns the full OpenAPI 3 specification for all bonsai endpoints. Served by utoipa-swagger-ui and consumed by /api/docs. Enables agents and tooling to introspect bonsai without prior knowledge.",
                    "responses": { "200": { "description": "OpenAPI 3 JSON" }}
                }
            },
            "/api/resolve": {
                "get": {
                    "tags": ["Schema"],
                    "summary": "Natural-language reference resolution",
                    "description": "Resolves a natural-language query to stable bonsai IDs. Returns candidate devices, detections, and rules ranked by match confidence. Designed for AI agent sessions to convert informal references (e.g. 'spine1', 'that BGP issue') to API-addressable UUIDs and addresses.",
                    "parameters": [{ "name": "q", "in": "query", "required": true, "schema": { "type": "string" }, "description": "Query string, e.g. 'spine1', 'BGP issue last night', 'bgp_session_down'" }],
                    "responses": { "200": { "description": "Resolution candidates with confidence scores" }}
                }
            }
        },
        "components": {
            "schemas": {
                "Device": {
                    "type": "object",
                    "properties": {
                        "address": { "type": "string", "description": "Management IP address (gNMI target)" },
                        "hostname": { "type": "string", "description": "Device hostname from gNMI telemetry" },
                        "vendor": { "type": "string", "enum": ["nokia", "cisco", "juniper", "arista", "frr", "holo", "unknown"] },
                        "role": { "type": "string", "description": "Topology role e.g. spine, leaf, pe, p, rr, super-spine" },
                        "site": { "type": "string", "description": "Site name from graph enrichment" },
                        "health": { "type": "string", "enum": ["healthy", "warn", "critical"] },
                        "bgp": { "type": "array", "items": { "$ref": "#/components/schemas/BgpSession" }},
                        "_schema_version": { "type": "string", "description": "Bonsai version that produced this record" }
                    }
                },
                "BgpSession": {
                    "type": "object",
                    "properties": {
                        "peer": { "type": "string", "description": "Peer IP address" },
                        "state": { "type": "string", "description": "BGP session state: Established, Active, Idle, etc." },
                        "peer_as": { "type": "integer", "description": "Peer autonomous system number" }
                    }
                },
                "Link": {
                    "type": "object",
                    "properties": {
                        "src_device": { "type": "string" },
                        "src_iface": { "type": "string" },
                        "dst_device": { "type": "string" },
                        "dst_iface": { "type": "string" },
                        "bytes_total": { "type": "integer", "description": "Sum of in_octets+out_octets on both ends — used for utilisation heatmap colouring" },
                        "is_mgmt": { "type": "boolean", "description": "True for out-of-band management-plane LLDP links" }
                    }
                },
                "DetectionEvent": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "format": "uuid" },
                        "device_address": { "type": "string" },
                        "rule_id": { "type": "string", "description": "Detection rule that fired, e.g. bgp_session_down" },
                        "severity": { "type": "string", "enum": ["critical", "warn", "info"] },
                        "features_json": { "type": "string", "description": "JSON-serialized Features struct — used for ML training and GNN input" },
                        "fired_at_ns": { "type": "integer", "description": "Unix timestamp in nanoseconds" },
                        "remediation_id": { "type": "string", "format": "uuid" },
                        "remediation_action": { "type": "string" },
                        "remediation_status": { "type": "string", "enum": ["pending", "approved", "rejected", "executed", "rolled_back"] },
                        "_schema_version": { "type": "string" }
                    }
                },
                "TopologyResponse": {
                    "type": "object",
                    "properties": {
                        "_schema_version": { "type": "string" },
                        "devices": { "type": "array", "items": { "$ref": "#/components/schemas/Device" }},
                        "links": { "type": "array", "items": { "$ref": "#/components/schemas/Link" }}
                    },
                    "required": ["_schema_version", "devices", "links"]
                },
                "DetectionsResponse": {
                    "type": "object",
                    "properties": {
                        "_schema_version": { "type": "string" },
                        "detections": { "type": "array", "items": { "$ref": "#/components/schemas/DetectionEvent" }}
                    },
                    "required": ["_schema_version", "detections"]
                },
                "Incident": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "format": "uuid" },
                        "root": { "$ref": "#/components/schemas/DetectionEvent" },
                        "cascading": { "type": "array", "items": { "$ref": "#/components/schemas/DetectionEvent" }},
                        "affected_devices": { "type": "array", "items": { "type": "string" }},
                        "severity": { "type": "string" },
                        "started_at_ns": { "type": "integer" },
                        "ended_at_ns": { "type": "integer" },
                        "remediation_status": { "type": "string" }
                    },
                    "required": ["id", "root", "cascading", "affected_devices", "severity", "started_at_ns", "ended_at_ns", "remediation_status"]
                },
                "IncidentsResponse": {
                    "type": "object",
                    "properties": {
                        "_schema_version": { "type": "string" },
                        "incidents": { "type": "array", "items": { "$ref": "#/components/schemas/Incident" }}
                    },
                    "required": ["_schema_version", "incidents"]
                },
                "ReadinessResponse": {
                    "type": "object",
                    "properties": {
                        "_schema_version": { "type": "string" },
                        "detection_events": { "type": "integer" },
                        "state_change_events": { "type": "integer" },
                        "rule_distribution": { "type": "object", "additionalProperties": { "type": "integer" }},
                        "cutoff_iso": { "type": "string" },
                        "remediation_rows_post_cutoff": { "type": "integer" },
                        "action_distribution_post_cutoff": { "type": "object", "additionalProperties": { "type": "integer" }},
                        "status_distribution_post_cutoff": { "type": "object", "additionalProperties": { "type": "integer" }}
                    },
                    "required": ["_schema_version", "detection_events", "state_change_events", "rule_distribution", "cutoff_iso", "remediation_rows_post_cutoff", "action_distribution_post_cutoff", "status_distribution_post_cutoff"]
                },
                "OperationsResponse": {
                    "type": "object",
                    "properties": {
                        "_schema_version": { "type": "string" },
                        "detection_events": { "type": "integer" },
                        "state_change_events": { "type": "integer" },
                        "remediation_rows_post_cutoff": { "type": "integer" },
                        "rule_distribution": { "type": "object", "additionalProperties": { "type": "integer" }},
                        "action_distribution_post_cutoff": { "type": "object", "additionalProperties": { "type": "integer" }},
                        "status_distribution_post_cutoff": { "type": "object", "additionalProperties": { "type": "integer" }},
                        "device_count": { "type": "integer" },
                        "enabled_device_count": { "type": "integer" },
                        "observed_subscriptions": { "type": "integer" },
                        "pending_subscriptions": { "type": "integer" },
                        "silent_subscriptions": { "type": "integer" },
                        "collectors_connected": { "type": "integer" },
                        "collectors_total": { "type": "integer" },
                        "unassigned_devices": { "type": "integer" },
                        "event_bus_depth": { "type": "integer" },
                        "event_bus_receivers": { "type": "integer" },
                        "archive_lag_millis": { "type": "integer" },
                        "archive_buffer_rows": { "type": "integer" },
                        "archive_last_flush_millis": { "type": "integer" },
                        "archive_last_compression_ppm": { "type": "integer" },
                        "cutoff_iso": { "type": "string" },
                        "rss_bytes": { "type": "integer" },
                        "archive_disk_bytes": { "type": "integer" },
                        "archive_disk_pct": { "type": "integer" },
                        "graph_disk_bytes": { "type": "integer" },
                        "graph_disk_pct": { "type": "integer" },
                        "memory_budget_bytes": { "type": "integer" },
                        "memory_rss_pct_of_budget": { "type": "number" },
                        "counter_mode": { "type": "string" },
                        "counter_window_secs": { "type": "integer" },
                        "counter_debounce_secs": { "type": "integer" }
                    },
                    "required": ["_schema_version", "detection_events", "state_change_events", "remediation_rows_post_cutoff", "rule_distribution", "action_distribution_post_cutoff", "status_distribution_post_cutoff", "device_count", "enabled_device_count", "observed_subscriptions", "pending_subscriptions", "silent_subscriptions", "collectors_connected", "collectors_total", "unassigned_devices", "event_bus_depth", "event_bus_receivers", "archive_lag_millis", "archive_buffer_rows", "archive_last_flush_millis", "archive_last_compression_ppm", "cutoff_iso", "rss_bytes", "archive_disk_bytes", "archive_disk_pct", "graph_disk_bytes", "graph_disk_pct", "memory_budget_bytes", "memory_rss_pct_of_budget", "counter_mode", "counter_window_secs", "counter_debounce_secs"]
                },
                "ManagedDevice": {
                    "type": "object",
                    "properties": {
                        "address": { "type": "string" },
                        "enabled": { "type": "boolean" },
                        "collector_id": { "type": "string" },
                        "tls_domain": { "type": "string" },
                        "ca_cert": { "type": "string" },
                        "vendor": { "type": "string" },
                        "credential_alias": { "type": "string" },
                        "username_env": { "type": "string" },
                        "password_env": { "type": "string" },
                        "hostname": { "type": "string" },
                        "role": { "type": "string" },
                        "site": { "type": "string" },
                        "selected_paths": { "type": "array", "items": { "type": "object", "additionalProperties": true }},
                        "subscription_statuses": { "type": "array", "items": { "$ref": "#/components/schemas/SubscriptionStatus" }},
                        "resolution_audit": { "type": "array", "items": { "type": "string" }}
                    },
                    "required": ["address", "enabled", "collector_id", "tls_domain", "ca_cert", "vendor", "credential_alias", "username_env", "password_env", "hostname", "role", "site", "selected_paths", "subscription_statuses", "resolution_audit"]
                },
                "ManagedDevicesResponse": {
                    "type": "object",
                    "properties": {
                        "devices": { "type": "array", "items": { "$ref": "#/components/schemas/ManagedDevice" }}
                    },
                    "required": ["devices"]
                },
                "SubscriptionStatus": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "origin": { "type": "string" },
                        "mode": { "type": "string" },
                        "sample_interval_ns": { "type": "integer" },
                        "status": { "type": "string" },
                        "first_observed_at_ns": { "type": "integer" },
                        "last_observed_at_ns": { "type": "integer" },
                        "updated_at_ns": { "type": "integer" }
                    },
                    "required": ["path", "origin", "mode", "sample_interval_ns", "status", "first_observed_at_ns", "last_observed_at_ns", "updated_at_ns"]
                },
                "SetupStatusResponse": {
                    "type": "object",
                    "properties": {
                        "is_first_run": { "type": "boolean" },
                        "has_environments": { "type": "boolean" },
                        "has_credentials": { "type": "boolean" },
                        "has_devices": { "type": "boolean" }
                    },
                    "required": ["is_first_run", "has_environments", "has_credentials", "has_devices"]
                },
                "InterfaceDetail": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "in_errors": { "type": "integer" },
                        "out_errors": { "type": "integer" },
                        "in_octets": { "type": "integer" },
                        "out_octets": { "type": "integer" },
                        "carrier_transitions": { "type": "integer" },
                        "updated_at_ns": { "type": "integer" }
                    },
                    "required": ["name", "in_errors", "out_errors", "in_octets", "out_octets", "carrier_transitions", "updated_at_ns"]
                },
                "LldpNeighbor": {
                    "type": "object",
                    "properties": {
                        "local_if": { "type": "string" },
                        "system_name": { "type": "string" },
                        "port_id": { "type": "string" },
                        "chassis_id": { "type": "string" }
                    },
                    "required": ["local_if", "system_name", "port_id", "chassis_id"]
                },
                "StateChange": {
                    "type": "object",
                    "properties": {
                        "event_type": { "type": "string" },
                        "detail": { "type": "string" },
                        "occurred_at_ns": { "type": "integer" }
                    },
                    "required": ["event_type", "detail", "occurred_at_ns"]
                },
                "DeviceDetailResponse": {
                    "type": "object",
                    "properties": {
                        "address": { "type": "string" },
                        "hostname": { "type": "string" },
                        "vendor": { "type": "string" },
                        "role": { "type": "string" },
                        "site": { "type": "string" },
                        "enabled": { "type": "boolean" },
                        "collector_id": { "type": "string" },
                        "credential_alias": { "type": "string" },
                        "health": { "type": "string" },
                        "interfaces": { "type": "array", "items": { "$ref": "#/components/schemas/InterfaceDetail" }},
                        "bgp_neighbors": { "type": "array", "items": { "$ref": "#/components/schemas/BgpSession" }},
                        "lldp_neighbors": { "type": "array", "items": { "$ref": "#/components/schemas/LldpNeighbor" }},
                        "recent_state_changes": { "type": "array", "items": { "$ref": "#/components/schemas/StateChange" }},
                        "recent_detections": { "type": "array", "items": { "$ref": "#/components/schemas/DetectionEvent" }},
                        "selected_paths": { "type": "array", "items": { "type": "object", "additionalProperties": true }},
                        "subscription_statuses": { "type": "array", "items": { "$ref": "#/components/schemas/SubscriptionStatus" }},
                        "resolution_audit": { "type": "array", "items": { "type": "string" }},
                        "created_at_ns": { "type": "integer" },
                        "updated_at_ns": { "type": "integer" },
                        "created_by": { "type": "string" },
                        "updated_by": { "type": "string" },
                        "last_operator_action": { "type": "string" }
                    },
                    "required": ["address", "hostname", "vendor", "role", "site", "enabled", "collector_id", "credential_alias", "health", "interfaces", "bgp_neighbors", "lldp_neighbors", "recent_state_changes", "recent_detections", "selected_paths", "subscription_statuses", "resolution_audit", "created_at_ns", "updated_at_ns", "created_by", "updated_by", "last_operator_action"]
                },
                "DeviceGnmiReadinessResponse": {
                    "type": "object",
                    "properties": {
                        "address": { "type": "string" },
                        "report": { "type": "object", "additionalProperties": true }
                    },
                    "required": ["address", "report"]
                },
                "DeviceStreamingReadinessResponse": {
                    "type": "object",
                    "properties": {
                        "address": { "type": "string" },
                        "report": { "type": "object", "additionalProperties": true }
                    },
                    "required": ["address", "report"]
                },
                "DeviceRecommendationsResponse": {
                    "type": "object",
                    "properties": {
                        "report": { "type": "object", "additionalProperties": true }
                    },
                    "required": ["report"]
                },
                "ApplySelectedPathsRequest": {
                    "type": "object",
                    "properties": {
                        "selected_paths": { "type": "array", "items": { "type": "object", "additionalProperties": true }}
                    },
                    "required": ["selected_paths"]
                },
                "ApplySelectedPathsResponse": {
                    "type": "object",
                    "properties": {
                        "success": { "type": "boolean" },
                        "error": { "type": "string" },
                        "selected_paths": { "type": "array", "items": { "type": "object", "additionalProperties": true }}
                    },
                    "required": ["success", "error", "selected_paths"]
                },
                "YangModulesResponse": {
                    "type": "object",
                    "properties": {
                        "modules": { "type": "array", "items": { "type": "object", "additionalProperties": true }}
                    },
                    "required": ["modules"]
                },
                "YangSearchResponse": {
                    "type": "object",
                    "properties": {
                        "result": { "type": "object", "additionalProperties": true }
                    },
                    "required": ["result"]
                },
                "Profile": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "environment": { "type": "array", "items": { "type": "string" }},
                        "vendor_scope": { "type": "array", "items": { "type": "string" }},
                        "roles": { "type": "array", "items": { "type": "string" }},
                        "description": { "type": "string" },
                        "rationale": { "type": "string" },
                        "path_count": { "type": "integer" },
                        "source": { "type": "string" }
                    },
                    "required": ["name", "environment", "vendor_scope", "roles", "description", "rationale", "path_count", "source"]
                },
                "Plugin": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "version": { "type": "string" },
                        "author": { "type": "string" },
                        "profile_count": { "type": "integer" },
                        "conflicts": { "type": "array", "items": { "type": "string" }}
                    },
                    "required": ["name", "version", "author", "profile_count", "conflicts"]
                },
                "ProfilesResponse": {
                    "type": "object",
                    "properties": {
                        "profiles": { "type": "array", "items": { "$ref": "#/components/schemas/Profile" }},
                        "plugins": { "type": "array", "items": { "$ref": "#/components/schemas/Plugin" }},
                        "load_errors": { "type": "array", "items": { "type": "string" }}
                    },
                    "required": ["profiles", "plugins", "load_errors"]
                },
                "SaveCustomProfileRequest": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "description": { "type": "string" },
                        "rationale": { "type": "string" },
                        "environment": { "type": "array", "items": { "type": "string" }},
                        "vendor_scope": { "type": "array", "items": { "type": "string" }},
                        "roles": { "type": "array", "items": { "type": "string" }},
                        "paths": { "type": "array", "items": { "type": "object", "additionalProperties": true }}
                    },
                    "required": ["name", "description", "rationale", "environment", "vendor_scope", "roles", "paths"]
                },
                "SaveCustomProfileResponse": {
                    "type": "object",
                    "properties": {
                        "success": { "type": "boolean" },
                        "error": { "type": "string", "nullable": true }
                    },
                    "required": ["success"]
                },
                "SnowTestRequest": {
                    "type": "object",
                    "properties": {
                        "instance_url": { "type": "string" },
                        "credential_alias": { "type": "string" }
                    },
                    "required": ["instance_url", "credential_alias"]
                },
                "SnowTestResponse": {
                    "type": "object",
                    "properties": {
                        "success": { "type": "boolean" },
                        "message": { "type": "string" }
                    },
                    "required": ["success", "message"]
                },
                "SnowAiopsSyncResponse": {
                    "type": "object",
                    "properties": {
                        "success": { "type": "boolean" },
                        "error": { "type": "string" },
                        "stats": { "type": "object", "additionalProperties": true }
                    },
                    "required": ["success", "error", "stats"]
                },
                "SiteRecord": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "name": { "type": "string" },
                        "location": { "type": "string" },
                        "description": { "type": "string" }
                    },
                    "required": ["name"]
                },
                "EnvironmentRecord": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "name": { "type": "string" },
                        "archetype": { "type": "string", "enum": ["data_center", "service_provider", "home_lab"] },
                        "description": { "type": "string" }
                    },
                    "required": ["name", "archetype"]
                },
                "AddDeviceRequest": {
                    "type": "object",
                    "properties": {
                        "address": { "type": "string", "description": "Management IP:port, e.g. 192.168.1.1:57400" },
                        "credential_alias": { "type": "string", "description": "Vault alias for gNMI credentials" },
                        "role_hint": { "type": "string", "description": "Optional topology role hint, e.g. spine, leaf, pe" },
                        "ca_cert_path": { "type": "string", "description": "Path to CA cert for TLS verification (defaults to lab CA)" },
                        "tls_domain": { "type": "string" }
                    },
                    "required": ["address", "credential_alias"]
                },
                "DiscoverRequest": {
                    "type": "object",
                    "properties": {
                        "address": { "type": "string", "description": "Management IP:port to probe" },
                        "credential_alias": { "type": "string" },
                        "username_env": { "type": "string", "description": "Env var holding username (alternative to alias)" },
                        "password_env": { "type": "string", "description": "Env var holding password (alternative to alias)" },
                        "ca_cert_path": { "type": "string" },
                        "tls_domain": { "type": "string" },
                        "role_hint": { "type": "string" },
                        "environment_archetype": { "type": "string" }
                    },
                    "required": ["address"]
                }
            }
        }
    })
}

fn load_openapi_example(name: &str) -> serde_json::Value {
    let live_path = std::path::Path::new("docs")
        .join("openapi")
        .join("examples")
        .join("live")
        .join(format!("{name}.json"));

    if let Ok(raw) = std::fs::read_to_string(&live_path)
        && let Ok(value) = serde_json::from_str(&raw)
    {
        return value;
    }

    let raw = match name {
        "topology" => include_str!("../docs/openapi/examples/topology.json"),
        "detections" => include_str!("../docs/openapi/examples/detections.json"),
        "incidents" => include_str!("../docs/openapi/examples/incidents.json"),
        "readiness" => include_str!("../docs/openapi/examples/readiness.json"),
        "operations" => include_str!("../docs/openapi/examples/operations.json"),
        "grounded_incident" => include_str!("../docs/openapi/examples/grounded_incident.json"),
        "managed_devices" => include_str!("../docs/openapi/examples/managed_devices.json"),
        "onboarding_discover" => include_str!("../docs/openapi/examples/onboarding_discover.json"),
        "device_detail" => include_str!("../docs/openapi/examples/device_detail.json"),
        "device_gnmi_readiness" => {
            include_str!("../docs/openapi/examples/device_gnmi_readiness.json")
        }
        "device_streaming_readiness" => {
            include_str!("../docs/openapi/examples/device_streaming_readiness.json")
        }
        "device_recommendations" => {
            include_str!("../docs/openapi/examples/device_recommendations.json")
        }
        "apply_selected_paths" => {
            include_str!("../docs/openapi/examples/apply_selected_paths.json")
        }
        "setup_status" => include_str!("../docs/openapi/examples/setup_status.json"),
        "yang_modules" => include_str!("../docs/openapi/examples/yang_modules.json"),
        "yang_search" => include_str!("../docs/openapi/examples/yang_search.json"),
        "profiles" => include_str!("../docs/openapi/examples/profiles.json"),
        "save_custom_profile" => include_str!("../docs/openapi/examples/save_custom_profile.json"),
        "servicenow_test" => include_str!("../docs/openapi/examples/servicenow_test.json"),
        "servicenow_aiops_sync" => {
            include_str!("../docs/openapi/examples/servicenow_aiops_sync.json")
        }
        _ => "{}",
    };

    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::json!({}))
}

// ── T5-5 — Reference resolution endpoint ─────────────────────────────────────

#[derive(Deserialize)]
struct ResolveParams {
    q: String,
}

async fn resolve_handler(
    State(state): State<AppState>,
    Query(params): Query<ResolveParams>,
) -> Result<Json<crate::mcp_server::ResolveResponse>, (StatusCode, String)> {
    let q = params.q.trim().to_string();
    if q.is_empty() {
        return Ok(Json(crate::mcp_server::ResolveResponse {
            query: q,
            candidates: vec![],
        }));
    }

    let mut candidates: Vec<crate::mcp_server::ResolveCandidate> = Vec::new();

    // 1. Device candidates — hostname and address substring match.
    let db = state.store.db();
    let q_clone = q.clone();
    let devices: Vec<(String, String)> = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let rows: Vec<(String, String)> = conn
            .query("MATCH (d:Device) RETURN d.address, d.hostname")
            .map_err(|e| e.to_string())?
            .map(|row| (read_str(&row[0]), read_str(&row[1])))
            .collect();
        Ok::<_, String>(rows)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    for (address, hostname) in devices {
        let score = crate::mcp_server::match_score(&hostname, &q_clone)
            .max(crate::mcp_server::match_score(&address, &q_clone));
        if score > 0.0 {
            candidates.push(crate::mcp_server::ResolveCandidate {
                kind: "device",
                id: address.clone(),
                label: if hostname.is_empty() {
                    address
                } else {
                    format!("{hostname} ({address})")
                },
                score,
            });
        }
    }

    // 2. Recent detection candidates — match against rule_id and device_address.
    let detections = state
        .store
        .read_detections(100)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    for det in &detections {
        let score = crate::mcp_server::match_score(&det.rule_id, &q)
            .max(crate::mcp_server::match_score(&det.id, &q))
            .max(crate::mcp_server::match_score(&det.device_address, &q));
        if score > 0.0 {
            candidates.push(crate::mcp_server::ResolveCandidate {
                kind: "detection",
                id: det.id.clone(),
                label: format!(
                    "{} on {} ({})",
                    det.rule_id, det.device_address, det.severity
                ),
                score,
            });
        }
    }

    // 3. Rule candidates — static catalogue, match against rule_id and description.
    for rule in crate::mcp_server::RULE_CATALOGUE {
        let score = crate::mcp_server::match_score(rule.rule_id, &q)
            .max(crate::mcp_server::match_score(rule.description, &q));
        if score > 0.0 {
            candidates.push(crate::mcp_server::ResolveCandidate {
                kind: "rule",
                id: rule.rule_id.to_string(),
                label: format!("{} — {}", rule.rule_id, rule.description),
                score,
            });
        }
    }

    // Sort by descending score, limit to top 20.
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates.truncate(20);

    Ok(Json(crate::mcp_server::ResolveResponse {
        query: q,
        candidates,
    }))
}

#[cfg(test)]
mod tests {
    use super::openapi_schema;

    #[test]
    fn openapi_schema_uses_envelope_shapes_for_primary_responses() {
        let spec = openapi_schema();
        let detections_ref = &spec["paths"]["/api/detections"]["get"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["$ref"];
        assert_eq!(
            detections_ref.as_str(),
            Some("#/components/schemas/DetectionsResponse")
        );

        let topology_ref = &spec["paths"]["/api/topology"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"];
        assert_eq!(
            topology_ref.as_str(),
            Some("#/components/schemas/TopologyResponse")
        );
    }

    #[test]
    fn openapi_schema_includes_examples_and_schema_version_fields() {
        let spec = openapi_schema();
        assert!(
            spec["paths"]["/api/operations"]["get"]["responses"]["200"]["content"]
                ["application/json"]["example"]
                .is_object()
        );
        assert!(
            spec["components"]["schemas"]["OperationsResponse"]["properties"]["_schema_version"]
                .is_object()
        );
        assert!(
            spec["components"]["schemas"]["DetectionsResponse"]["properties"]["_schema_version"]
                .is_object()
        );
        assert_eq!(
            spec["paths"]["/api/profiles"]["get"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["$ref"]
                .as_str(),
            Some("#/components/schemas/ProfilesResponse")
        );
        assert_eq!(
            spec["paths"]["/api/setup/status"]["get"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["$ref"]
                .as_str(),
            Some("#/components/schemas/SetupStatusResponse")
        );
    }
}
