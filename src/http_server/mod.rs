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
    /// CV7 T4-2/T4-4: Sidecar registry. Shared with the gRPC service so both
    /// surfaces see the same data. See `src/sidecar_registry.rs`.
    pub sidecar_registry: Arc<crate::sidecar_registry::SidecarRegistry>,
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
    sidecar_registry: Arc<crate::sidecar_registry::SidecarRegistry>,
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
        sidecar_registry,
    };

    // Serve the Svelte SPA from ui/dist/. Fall back to index.html so
    // client-side routing works (the SPA handles /events and /trace/:id paths).
    let spa = ServeDir::new("ui/dist")
        .not_found_service(tower_http::services::ServeFile::new("ui/dist/index.html"));

    // CV7 T4-5: Bonpy — a separate Svelte SPA for Python/ML/AIOps surfaces,
    // mounted at /bonpy/ on the same Axum process. Distinct from bonsai UI;
    // see docs/architecture/sidecars.md. If `ui-bonpy/dist/` is missing (e.g.
    // a build that skipped the bonpy step), ServeDir returns 404 — bonsai UI
    // still works. Index fallback enables client-side routing within bonpy.
    let bonpy_spa = ServeDir::new("ui-bonpy/dist")
        .not_found_service(tower_http::services::ServeFile::new(
            "ui-bonpy/dist/index.html",
        ));

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
        // CV7 T4-4: Sidecar registry surface for bonpy UI and ops scripts.
        .route("/api/sidecars", get(sidecars_handler))
        // CV7 T4-6: Liveness gate. Returns "ok" normally; "degraded" with the
        // missing list when BONSAI_REQUIRE_SIDECAR is set and a required kind
        // has not registered within the startup grace window.
        .route("/health", get(health_handler))
        // CV7 T4-5: mount bonpy SPA at /bonpy/. Falls back to index.html for
        // client-side routing inside the bonpy app.
        .nest_service("/bonpy", bonpy_spa)
        .fallback_service(spa)
        .with_state(state)
        .layer(CorsLayer::permissive())
}

pub(crate) mod observability;
pub(crate) mod test_endpoints;
pub(crate) mod discovery;
pub(crate) mod config;
pub(crate) mod governance;
pub(crate) mod schema;
pub(crate) mod mcp_routes;
pub(crate) mod outputs;
pub(crate) mod swagger_ui;
pub(crate) mod openapi_schema;

#[allow(unused_imports)]
pub(crate) use observability::*;
#[allow(unused_imports)]
pub(crate) use test_endpoints::*;
#[allow(unused_imports)]
pub(crate) use discovery::*;
#[allow(unused_imports)]
pub(crate) use config::*;
#[allow(unused_imports)]
pub(crate) use governance::*;
#[allow(unused_imports)]
pub(crate) use schema::*;
#[allow(unused_imports)]
pub(crate) use mcp_routes::*;
#[allow(unused_imports)]
pub(crate) use outputs::*;
#[allow(unused_imports)]
pub(crate) use swagger_ui::*;
