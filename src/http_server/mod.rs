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
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

use axum::{
    Router,
    http::StatusCode,
    routing::{delete, get, post},
};

use lbug::{Connection, Value};
use serde::{Deserialize, Serialize};
use tower_http::{cors::CorsLayer, services::ServeDir};

use crate::assignment::CollectorManager;
use crate::catalogue::CatalogueState;
use crate::enrichment::SharedEnricherRegistry;
use crate::graph::{
    DetectionRow, GraphStore, SiteRecord, TraceStep,
};
use crate::output::traits::SharedAdapterRegistry;
use crate::resource_governor::GovernorHandle;
use crate::{
    change_detection::ChangeDetectionRuntime,
    config::{
        LayeredIngestionConfig, RemediationConfig, SelectedSubscriptionPath,
        ServiceNowConfig, StorageConfig, StreamingConfig,
    },
    credentials::CredentialVault, memory_profile,
    registry::ApiRegistry,
    remediation::{
        SharedRollbackRegistry, SharedTrustStore,
    },
};

mod mcp_routes;
mod device;
mod managed_devices;
mod governance;
mod observability;
mod remediation;
mod outputs;
mod test_endpoints;
mod schema;
mod schema_components;
mod settings;
mod nl_query;
mod ha;
mod shun;

use mcp_routes::{openapi_json_handler, resolve_handler, schema_handler, swagger_ui_handler};
use remediation::{approvals_approve_handler, approvals_create_handler, approvals_list_handler, approvals_reject_handler, approvals_rollback_handler, trust_list_handler, trust_graduate_handler, snow_integration_test_handler, servicenow_aiops_sync_handler, list_overrides, add_override, remove_override, list_investigations_handler, create_investigation_handler, get_investigation_handler, list_tool_calls_handler, complete_investigation_handler, grounded_incident_handler, webhook_change_event_handler, change_context_handler, servicenow_change_sync_handler, list_changes_handler, playbooks_catalog_handler, audit_log_handler, investigation_feedback_handler, investigation_accuracy_handler, vault_rekey_handler, remediation_verify_handler, list_config_items_handler, upsert_config_item_handler};
use observability::{topology_handler, path_handler, blast_radius_handler, detections_handler, trace_handler, readiness_handler, operations_handler, test_status_handler, daily_check_handler, weekly_trend_handler, gnn_calibration_handler, gnn_score_handler, events_handler, events_history_handler, incidents_handler, graph_insights_handler, graph_quality_handler, flows_live_handler, explorer_query_handler, list_saved_queries_handler, create_saved_query_handler, delete_saved_query_handler, upsert_embeddings_handler, list_embeddings_handler, events_inject_handler, db_stats_handler, db_schema_handler, db_purge_handler, db_checkpoint_handler, db_export_handler, db_backup_handler, db_list_backups_handler, list_redundancy_groups_handler};
use device::{device_detail_handler, device_enrichment_handler, device_enrichment_conflicts_handler, device_cmdb_handler, device_sensors_handler, device_optics_handler, device_config_history_handler, device_gnmi_readiness_handler, device_streaming_readiness_handler, device_recommendations_handler, yang_modules_handler, yang_search_handler, apply_device_selected_paths_handler, device_reparse_handler, profiles_handler, save_custom_profile_handler, enrichment_list_handler, enrichment_upsert_handler, enrichment_remove_handler, enrichment_test_handler, enrichment_run_handler, enrichment_audit_handler, netbox_import_handler};
use device::InterfaceDetailJson;
use managed_devices::{managed_devices_handler, discover_handler, credentials_handler, add_credential_handler, update_credential_handler, remove_credential_handler, test_credential_handler, add_managed_device_handler, add_managed_device_with_paths_handler, sites_handler, upsert_site_handler, site_summary_handler, remove_site_handler, remove_managed_device_handler, bulk_managed_device_action_handler, remove_impact_handler, bulk_import_handler, bootstrap_device_handler, device_seed_handler, bulk_bootstrap_handler};
use governance::{assignment_override_handler, assignment_rules_handler, assignment_status_handler, collectors_handler, create_environment_handler, environments_handler, governance_state_handler, governance_history_handler, governance_profile_handler, health_handler, healthz_handler, readyz_handler, remove_environment_handler, assign_site_environment_handler, update_environment_handler, set_assignment_rules_handler, setup_status_handler, sidecars_handler, sidecar_status_handler};
use outputs::{adapter_audit_handler, adapter_list_handler, adapter_remove_handler, adapter_test_handler, adapter_upsert_handler};
use test_endpoints::{inject_detection_handler, parse_syslog_fixture_handler};
use settings::{get_streaming_settings_handler, patch_streaming_settings_handler, get_receiver_status_handler, get_ai_config_handler, post_ai_test_handler, list_ai_providers_handler, upsert_ai_provider_handler, remove_ai_provider_handler, test_ai_provider_handler};
use ha::{ha_status_handler, ha_settings_handler, ha_patch_settings_handler, restart_handler};
use nl_query::{explorer_ask_handler, nl_budget_handler};
use shun::{create_shun_rule_handler, delete_shun_rule_handler, disable_shun_rule_handler, list_shun_rules_handler, shun_stats_handler};

// ── JSON response types ───────────────────────────────────────────────────────

#[derive(Serialize)]
pub(super) struct TopologyResponse {
    #[serde(rename = "_schema_version")]
    schema_version: String,
    devices: Vec<DeviceJson>,
    links: Vec<LinkJson>,
    host_endpoints: Vec<HostEndpointJson>,
}

/// A HostEndpoint node (server, VM, container endpoint learned via NetBox, LLDP, or NetFlow).
#[derive(Serialize)]
pub(super) struct HostEndpointJson {
    pub id: String,
    pub ip: String,
    pub mac: String,
    pub hostname: String,
    pub kind: String,
    /// Device address of the switch interface this host is connected to (via CONNECTED_TO).
    pub connected_to_device: String,
    pub connected_to_iface: String,
}

#[derive(Serialize)]
pub(super) struct DeviceJson {
    address: String,
    hostname: String,
    vendor: String,
    role: String,
    site: String,
    site_id: String,
    site_path: String,
    health: String, // "healthy" | "warn" | "critical"
    interfaces: Vec<InterfaceDetailJson>,
    bgp: Vec<BgpJson>,
    isis_adjacencies: Vec<IsisAdjJson>,
}

#[derive(Serialize)]
pub(super) struct IsisAdjJson {
    pub system_id: String,
    pub if_name: String,
    pub adjacency_state: String,
    pub source_type: String,
}

#[derive(Serialize)]
pub(super) struct BgpJson {
    peer: String,
    state: String,
    peer_as: i64,
}

#[derive(Serialize)]
pub(super) struct LinkJson {
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
pub(super) struct PathResponse {
    #[serde(rename = "_schema_version")]
    schema_version: String,
    /// Device addresses in hop order, source first.
    hops: Vec<String>,
    /// (src_device, src_iface, dst_device, dst_iface) for each hop's link.
    links: Vec<(String, String, String, String)>,
}

#[derive(Serialize)]
pub(super) struct DetectionsResponse {
    #[serde(rename = "_schema_version")]
    schema_version: String,
    detections: Vec<DetectionRow>,
}

#[derive(Serialize)]
pub(super) struct TraceResponse {
    #[serde(rename = "_schema_version")]
    schema_version: String,
    steps: Vec<TraceStep>,
}

/// One signal in the correlation chain: a StateChangeEvent that triggered the root detection.
#[derive(Serialize, Clone)]
pub(super) struct CorrelationStep {
    pub state_change_event_id: String,
    pub event_type: String,
    pub source_type: String,
    pub device_address: String,
    pub occurred_at_ns: i64,
}

/// Lightweight blast-radius summary attached to each incident.
#[derive(Serialize, Clone)]
pub(super) struct BlastRadiusSummary {
    pub device_count: usize,
    pub app_count: usize,
}

/// Per-device detail for multi-device incidents (D4-4 T6).
#[derive(Serialize, Clone)]
pub(super) struct AffectedDeviceDetail {
    pub address: String,
    pub rules: Vec<String>,
    pub is_root: bool,
    pub detected_at_ns: i64,
}

#[derive(Serialize)]
pub(super) struct IncidentJson {
    id: String,
    root: DetectionRow,
    cascading: Vec<DetectionRow>,
    affected_devices: Vec<String>,
    /// Per-device breakdown for multi-device incidents (D4-4 T6).
    affected_device_details: Vec<AffectedDeviceDetail>,
    severity: String,
    started_at_ns: i64,
    ended_at_ns: i64,
    remediation_status: String,
    /// Deduplicated sorted list of rule_ids that fired in this incident.
    rule_ids: Vec<String>,
    /// Human-readable clubbing rationale: "2 rule types, 3 devices, 5s window".
    co_fire_signature: String,
    /// Number of distinct devices involved.
    device_count: usize,
    /// Total detection event count (root + cascading).
    event_count: usize,
    /// Ordered chain of StateChangeEvents that triggered the root detection.
    correlation_chain: Vec<CorrelationStep>,
    /// Lightweight blast-radius for the root device (reachable devices + apps).
    blast_radius_summary: Option<BlastRadiusSummary>,
    /// D4-4 T2: incident type taxonomy.
    /// One of: single_device | cascading_failure | multi_device_correlated | config_caused
    incident_type: String,
    /// D4-4 T3: human-readable explanation of why events were grouped.
    grouping_rationale: String,
}

#[derive(Serialize)]
pub(super) struct IncidentsResponse {
    #[serde(rename = "_schema_version")]
    schema_version: String,
    incidents: Vec<IncidentJson>,
}

#[derive(Deserialize, Default)]
pub(super) struct IncidentsParams {
    #[serde(default = "default_incident_window")]
    window_secs: u64,
    #[serde(default = "default_incident_limit")]
    limit: u32,
}

pub(super) fn default_incident_window() -> u64 {
    30
}
pub(super) fn default_incident_limit() -> u32 {
    200
}

#[derive(Serialize)]
pub(super) struct ReadinessResponse {
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
pub(super) struct OperationsResponse {
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

pub(super) const RSS_BUDGET_BYTES: u64 = 1_610_612_736; // 1.5 GiB
pub(super) const COORDINATOR_QUEUE_BUDGET_PCT: u64 = 75;
pub(super) const API_SCHEMA_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Serialize)]
pub(super) struct BudgetBreach {
    name: &'static str,
    current: f64,
    budget: f64,
    unit: &'static str,
}

#[derive(Serialize)]
pub(super) struct TestStatusResponse {
    ts_unix: u64,
    memory: memory_profile::MemorySnapshot,
    disk: DiskStatusJson,
    budget_breaches: Vec<BudgetBreach>,
    external: serde_json::Value,
    driver_results: serde_json::Value,
}

#[derive(Serialize)]
pub(super) struct DiskStatusJson {
    archive_bytes: u64,
    archive_max_bytes: u64,
    archive_pct: u8,
    graph_bytes: u64,
    graph_max_bytes: u64,
    graph_pct: u8,
}

/// Outbound SSE payload — mirrors BonsaiEvent but serialised as JSON.
#[derive(Serialize)]
pub(super) struct ManagedDevicesResponse {
    devices: Vec<ManagedDeviceJson>,
}

#[derive(Serialize)]
pub(super) struct ManagedDeviceJson {
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
pub(super) struct SubscriptionStatusJson {
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
pub(super) struct OnboardingDiscoveryRequest {
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
pub(super) struct ManagedDeviceRequest {
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
pub(super) struct RemoveManagedDeviceRequest {
    address: String,
}

#[derive(Deserialize)]
pub(super) struct BulkManagedDeviceActionRequest {
    addresses: Vec<String>,
    action: String,
}

#[derive(Serialize)]
pub(super) struct BulkManagedDeviceActionResponse {
    success: bool,
    error: String,
    devices: Vec<ManagedDeviceJson>,
}

#[derive(Serialize)]
pub(super) struct BulkImportResult {
    address: String,
    success: bool,
    error: String,
}

#[derive(Serialize)]
pub(super) struct BulkImportResponse {
    imported: usize,
    failed: usize,
    results: Vec<BulkImportResult>,
}

#[derive(Serialize)]
pub(super) struct RemoveImpactResponse {
    address: String,
    subscription_total: usize,
    subscription_observed: usize,
    subscription_pending: usize,
    trust_marks_total: usize,
    trust_marks_active: usize,
}

#[derive(Serialize)]
pub(super) struct SitesResponse {
    sites: Vec<SiteJson>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct SiteJson {
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
pub(super) struct SiteMutationResponse {
    success: bool,
    error: String,
    site: Option<SiteJson>,
}

#[derive(Deserialize)]
pub(super) struct RemoveSiteRequest {
    id: String,
}

#[derive(Serialize)]
pub(super) struct SiteSummaryResponse {
    site: SiteJson,
    child_site_count: usize,
    device_count: usize,
    health: SiteHealthJson,
    subscription_summary: SiteSubscriptionSummaryJson,
    devices: Vec<SiteDeviceJson>,
    recent_detections: Vec<DetectionRow>,
}

#[derive(Serialize, Default)]
pub(super) struct SiteHealthJson {
    healthy: usize,
    warn: usize,
    critical: usize,
}

#[derive(Serialize, Default)]
pub(super) struct SiteSubscriptionSummaryJson {
    observed: usize,
    pending: usize,
    silent: usize,
}

#[derive(Serialize)]
pub(super) struct SiteDeviceJson {
    address: String,
    hostname: String,
    vendor: String,
    role: String,
    collector_id: String,
    health: String,
}

#[derive(Serialize)]
pub(super) struct CredentialsResponse {
    credentials: Vec<CredentialJson>,
    unlocked: bool,
}

#[derive(Serialize)]
pub(super) struct CredentialJson {
    alias: String,
    created_at_ns: i64,
    updated_at_ns: i64,
    last_used_at_ns: i64,
    device_count: usize,
}

#[derive(Deserialize)]
pub(super) struct AddCredentialRequest {
    alias: String,
    username: String,
    password: String,
}

#[derive(Deserialize)]
pub(super) struct RemoveCredentialRequest {
    alias: String,
}

#[derive(Deserialize)]
pub(super) struct TestCredentialRequest {
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
pub(super) struct CredentialMutationResponse {
    success: bool,
    error: String,
    credential: Option<CredentialJson>,
}

#[derive(Serialize)]
pub(super) struct MutationResponse {
    success: bool,
    error: String,
    device: Option<ManagedDeviceJson>,
}

#[derive(Serialize)]
pub(super) struct SsePayload {
    device_address: String,
    event_type: String,
    detail_json: String,
    occurred_at_ns: i64,
    state_change_event_id: String,
    source_type: String,
}

// ── Query params ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct DetectionsParams {
    #[serde(default = "default_limit")]
    limit: u32,
}

#[derive(Deserialize)]
pub(super) struct EventsHistoryParams {
    pub source: Option<String>,
    pub device: Option<String>,
    pub site: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

#[derive(Serialize)]
pub(super) struct EventsHistoryResponse {
    pub events: Vec<EventHistoryItem>,
}

#[derive(Serialize)]
pub(super) struct EventHistoryItem {
    pub id: String,
    pub device_address: String,
    pub event_type: String,
    pub source_type: String,
    pub detail_json: String,
    pub occurred_at_ns: i64,
}

pub(super) fn default_limit() -> u32 {
    50
}

pub(super) fn default_enabled() -> bool {
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
    pub signals: crate::config::SignalsConfig,
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
    pub receiver_supervisor: crate::receiver_supervisor::SharedReceiverSupervisor,
    /// G3: HA coordinator for leader election and config replication
    pub ha_coordinator: Option<Arc<crate::ha_coordinator::HACoordinator>>,
    pub event_bus: std::sync::Arc<crate::event_bus::InProcessBus>,
    /// K3: Static target list for hot-restarting syslog/snmp receivers.
    pub targets: Vec<crate::config::TargetConfig>,
    /// D3-6: AI config for investigation runtime.
    pub ai_config: crate::config::AiConfig,
    /// D3-9: GNN inference config for score thresholding and anomaly detection.
    pub gnn_config: crate::config::GnnConfig,
    /// D4-2: Syslog shunning engine. None if not enabled.
    pub shun_engine: Option<Arc<crate::shun::ShunEngine>>,
}

impl axum::extract::FromRef<AppState> for Option<Arc<crate::ha_coordinator::HACoordinator>> {
    fn from_ref(state: &AppState) -> Self {
        state.ha_coordinator.clone()
    }
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
    signals: crate::config::SignalsConfig,
    yang_library_root: String,
    yang_cache_root: String,
    yang_bundle_key_env: String,
    counter_mode: String,
    counter_window_secs: u64,
    counter_debounce_secs: u64,
    governor: Option<GovernorHandle>,
    sidecar_registry: Arc<crate::sidecar_registry::SidecarRegistry>,
    receiver_supervisor: crate::receiver_supervisor::SharedReceiverSupervisor,
    event_bus: std::sync::Arc<crate::event_bus::InProcessBus>,
    ha_coordinator: Option<Arc<crate::ha_coordinator::HACoordinator>>,
    targets: Vec<crate::config::TargetConfig>,
    ai_config: crate::config::AiConfig,
    gnn_config: crate::config::GnnConfig,
    investigation_rx: Option<tokio::sync::mpsc::Receiver<crate::write_coordinator::AutoInvestigateRequest>>,
    shun_engine: Option<Arc<crate::shun::ShunEngine>>,
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
        signals,
        yang_library_root,
        yang_cache_root,
        yang_bundle_key_env,
        counter_mode,
        counter_window_secs,
        counter_debounce_secs,
        governor,
        sidecar_registry,
        receiver_supervisor,
        ha_coordinator,
        event_bus,
        targets,
        ai_config,
        gnn_config,
        shun_engine,
    };

    // D3-6 T6: consume auto-investigate requests from the write coordinator.
    // Runs only when auto_investigate_unmatched is enabled and the channel was wired.
    if let Some(mut rx) = investigation_rx {
        let inv_state = state.clone();
        tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let store = Arc::clone(&inv_state.store);
                match store.create_investigation(
                    req.detection_id.clone(),
                    req.device_address.clone(),
                    "auto".to_string(),
                ).await {
                    Ok(inv) => {
                        crate::investigation_runtime::spawn_investigation(
                            inv.id,
                            req.device_address,
                            Some(req.detection_id),
                            store,
                            inv_state.clone(),
                            inv_state.ai_config.clone(),
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            rule_id = %req.rule_id,
                            "auto-investigate: create_investigation failed"
                        );
                    }
                }
            }
        });
    }

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
        .merge(observability_routes())
        .merge(device_routes())
        .merge(managed_device_routes())
        .merge(governance_routes())
        .merge(remediation_routes())
        .merge(adapter_and_schema_routes())
        .merge(settings_routes())
        .merge(shun_routes())
        .route("/mcp", post(crate::mcp_server::mcp_handler))
        .nest_service("/bonpy", bonpy_spa)
        .fallback_service(spa)
        .with_state(state)
        .layer(CorsLayer::permissive())
}

fn observability_routes() -> Router<AppState> {
    Router::new()
        .route("/api/topology", get(topology_handler))
        .route("/api/path", get(path_handler))
        .route("/api/blast-radius/{address}", get(blast_radius_handler))
        .route("/api/incidents/grouped", get(incidents_handler))
        .route("/api/incidents", get(incidents_handler))
        .route("/api/incidents/{id}/grounded", get(grounded_incident_handler))
        .route("/api/detections", get(detections_handler))
        .route("/api/readiness", get(readiness_handler))
        .route("/api/operations", get(operations_handler))
        .route("/api/operations/daily-check", get(daily_check_handler))
        .route("/api/operations/weekly-trend", get(weekly_trend_handler))
        .route("/api/operations/gnn-calibration", get(gnn_calibration_handler))
        .route("/api/gnn/score", post(gnn_score_handler))
        .route("/api/trace/{id}", get(trace_handler))
        .route("/api/events", get(events_handler))
        .route("/api/events/history", get(events_history_handler))
        .route("/api/events/inject", post(events_inject_handler))
        .route("/api/graph/insights", get(graph_insights_handler))
        .route("/api/graph/quality", get(graph_quality_handler))
        .route("/api/flows/live", get(flows_live_handler))
        .route("/api/redundancy/groups", get(list_redundancy_groups_handler))
        .route("/api/explorer/query", post(explorer_query_handler))
        .route("/api/explorer/saved-queries", get(list_saved_queries_handler).post(create_saved_query_handler))
        .route("/api/explorer/saved-queries/{id}/delete", post(delete_saved_query_handler))
        .route("/api/graph/embeddings/upsert", post(upsert_embeddings_handler))
        .route("/api/graph/embeddings/{address}", get(list_embeddings_handler))
        .route("/api/db/stats", get(db_stats_handler))
        .route("/api/db/schema", get(db_schema_handler))
        .route("/api/db/purge", delete(db_purge_handler))
        .route("/api/db/checkpoint", post(db_checkpoint_handler))
        .route("/api/db/export", get(db_export_handler))
        .route("/api/db/backup", post(db_backup_handler))
        .route("/api/db/backups", get(db_list_backups_handler))
}

fn device_routes() -> Router<AppState> {
    Router::new()
        .route("/api/yang/modules", get(yang_modules_handler))
        .route("/api/yang/search", get(yang_search_handler))
        .route("/api/overrides", get(list_overrides).post(add_override))
        .route("/api/overrides/remove", post(remove_override))
        .route("/api/devices/{address}", get(device_detail_handler))
        .route("/api/devices/{address}/enrichment", get(device_enrichment_handler))
        .route("/api/devices/{address}/enrichment/conflicts", get(device_enrichment_conflicts_handler))
        .route("/api/devices/{address}/cmdb", get(device_cmdb_handler))
        .route("/api/devices/{address}/sensors", get(device_sensors_handler))
        .route("/api/devices/{address}/optics", get(device_optics_handler))
        .route("/api/devices/{address}/gnmi-readiness", get(device_gnmi_readiness_handler))
        .route("/api/devices/{address}/streaming-readiness", get(device_streaming_readiness_handler))
        .route("/api/devices/{address}/recommendations", get(device_recommendations_handler))
        .route("/api/devices/{address}/selected-paths", post(apply_device_selected_paths_handler))
        .route("/api/devices/{address}/config-history", get(device_config_history_handler))
        .route("/api/devices/{address}/reparse", post(device_reparse_handler))
        .route("/api/profiles", get(profiles_handler))
        .route("/api/profiles/save-custom", post(save_custom_profile_handler))
        .route("/api/enrichment", get(enrichment_list_handler).post(enrichment_upsert_handler))
        .route("/api/enrichment/remove", post(enrichment_remove_handler))
        .route("/api/enrichment/test", post(enrichment_test_handler))
        .route("/api/enrichment/run", post(enrichment_run_handler))
        .route("/api/enrichment/audit", get(enrichment_audit_handler))
        .route("/api/enrichment/netbox/import", post(netbox_import_handler))
        .route("/api/explorer/ask", post(explorer_ask_handler))
        .route("/api/explorer/nl-budget", get(nl_budget_handler))
}

fn managed_device_routes() -> Router<AppState> {
    Router::new()
        .route("/api/onboarding/devices", get(managed_devices_handler).post(add_managed_device_handler))
        .route("/api/onboarding/devices/with_paths", post(add_managed_device_with_paths_handler))
        .route("/api/onboarding/devices/remove", post(remove_managed_device_handler))
        .route("/api/onboarding/devices/remove-impact", post(remove_impact_handler))
        .route("/api/onboarding/devices/bulk", post(bulk_managed_device_action_handler))
        .route("/api/onboarding/import", post(bulk_import_handler))
        .route("/api/onboarding/discover", post(discover_handler))
        .route("/api/devices/bootstrap", post(bootstrap_device_handler))
        .route("/api/devices/bootstrap/bulk", post(bulk_bootstrap_handler))
        .route("/api/devices/seed", post(device_seed_handler))
        .route("/api/sites", get(sites_handler).post(upsert_site_handler))
        .route("/api/sites/{id}", get(site_summary_handler))
        .route("/api/sites/remove", post(remove_site_handler))
        .route("/api/credentials", get(credentials_handler).post(add_credential_handler))
        .route("/api/credentials/update", post(update_credential_handler))
        .route("/api/credentials/remove", post(remove_credential_handler))
        .route("/api/credentials/test", post(test_credential_handler))
        .route("/api/vault/rekey", post(vault_rekey_handler))
}

fn governance_routes() -> Router<AppState> {
    Router::new()
        .route("/api/environments", get(environments_handler).post(create_environment_handler))
        .route("/api/environments/update", post(update_environment_handler))
        .route("/api/environments/remove", post(remove_environment_handler))
        .route("/api/environments/assign-site", post(assign_site_environment_handler))
        .route("/api/setup/status", get(setup_status_handler))
        .route("/api/collectors", get(collectors_handler))
        .route("/api/assignment/rules", get(assignment_rules_handler).post(set_assignment_rules_handler))
        .route("/api/assignment/status", get(assignment_status_handler))
        .route("/api/assignment/override", post(assignment_override_handler))
        .route("/api/governance/state", get(governance_state_handler))
        .route("/api/governance/history", get(governance_history_handler))
        .route("/api/governance/profile", post(governance_profile_handler))
}

fn remediation_routes() -> Router<AppState> {
    Router::new()
        .route("/api/playbooks", get(playbooks_catalog_handler))
        .route("/api/audit", get(audit_log_handler))
        .route("/api/approvals", get(approvals_list_handler).post(approvals_create_handler))
        .route("/api/approvals/{id}/approve", post(approvals_approve_handler))
        .route("/api/approvals/{id}/reject", post(approvals_reject_handler))
        .route("/api/approvals/{id}/rollback", post(approvals_rollback_handler))
        .route("/api/trust", get(trust_list_handler))
        .route("/api/trust/graduate", post(trust_graduate_handler))
        .route("/api/integrations/servicenow/test", post(snow_integration_test_handler))
        .route("/api/integrations/servicenow/aiops/sync", post(servicenow_aiops_sync_handler))
        .route("/api/investigations", get(list_investigations_handler).post(create_investigation_handler))
        .route("/api/investigations/{id}", get(get_investigation_handler))
        .route("/api/investigations/{id}/tool-calls", get(list_tool_calls_handler))
        .route("/api/investigations/{id}/complete", post(complete_investigation_handler))
        .route("/api/investigations/{id}/feedback", post(investigation_feedback_handler))
        .route("/api/investigations/accuracy", get(investigation_accuracy_handler))
        .route("/api/webhooks/change-event", post(webhook_change_event_handler))
        .route("/api/changes", get(list_changes_handler))
        .route("/api/changes/context/{device_address}", get(change_context_handler))
        .route("/api/integrations/servicenow/changes/sync", post(servicenow_change_sync_handler))
        // D4-15 T3: Remediation outcome verification
        .route("/api/remediations/{id}/verify", post(remediation_verify_handler))
        // D4-7 T1: Config item CRUD
        .route("/api/config-items", get(list_config_items_handler).post(upsert_config_item_handler))
}

fn adapter_and_schema_routes() -> Router<AppState> {
    Router::new()
        .route("/api/adapters", get(adapter_list_handler).post(adapter_upsert_handler))
        .route("/api/adapters/remove", post(adapter_remove_handler))
        .route("/api/adapters/test", post(adapter_test_handler))
        .route("/api/adapters/audit", get(adapter_audit_handler))
        .route("/api/schema", get(schema_handler))
        .route("/api/resolve", get(resolve_handler))
        .route("/api/docs", get(swagger_ui_handler))
        .route("/api/openapi.json", get(openapi_json_handler))
        .route("/api/sidecars", get(sidecars_handler))
        .route("/api/sidecar/status", get(sidecar_status_handler))
        .route("/health", get(health_handler))
        .route("/healthz", get(healthz_handler))
        .route("/readyz", get(readyz_handler))
        .route("/api/_test/status", get(test_status_handler))
        .route("/api/_test/inject_detection", post(inject_detection_handler))
        .route("/api/_test/syslog/parse", post(parse_syslog_fixture_handler))
}

fn settings_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/settings/streaming",
            get(get_streaming_settings_handler).patch(patch_streaming_settings_handler),
        )
        .route("/api/receivers/status", get(get_receiver_status_handler))
        .route("/api/ai/config", get(get_ai_config_handler))
        .route("/api/ai/test", post(post_ai_test_handler))
        .route("/api/ai/providers", get(list_ai_providers_handler).post(upsert_ai_provider_handler))
        .route("/api/ai/providers/remove", post(remove_ai_provider_handler))
        .route("/api/ai/providers/test", post(test_ai_provider_handler))
        .route("/api/ha/status", get(ha_status_handler))
        .route("/api/ha/settings", get(ha_settings_handler).patch(ha_patch_settings_handler))
        .route("/api/restart", post(restart_handler))
}

fn shun_routes() -> Router<AppState> {
    Router::new()
        .route("/api/shun/rules", get(list_shun_rules_handler).post(create_shun_rule_handler))
        .route("/api/shun/rules/{id}/disable", post(disable_shun_rule_handler))
        .route("/api/shun/rules/{id}/delete", post(delete_shun_rule_handler))
        .route("/api/shun/stats", get(shun_stats_handler))
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct PathParams {
    src: String,
    dst: String,
}

// ─── blast radius ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct BlastRadiusParams {
    #[serde(default = "default_max_hops")]
    max_hops: usize,
}

pub(super) fn default_max_hops() -> usize {
    2
}

#[derive(Serialize)]
pub(super) struct DailyCheckResponse {
    ts_unix: u64,
    status: String,
    counts: DailyCheckCounts,
    checks: Vec<DailyCheckItem>,
}

#[derive(Serialize)]
pub(super) struct DailyCheckCounts {
    pass: usize,
    fail: usize,
    skip: usize,
    prereq_missing: usize,
}

#[derive(Serialize)]
pub(super) struct DailyCheckItem {
    name: String,
    status: String,
    summary: String,
}

// ── /api/_test/inject_detection ───────────────────────────────────────────────


pub(super) fn default_inject_severity() -> String {
    "info".to_string()
}



pub(super) fn default_syslog_transport() -> String {
    "udp".to_string()
}

pub(super) fn default_syslog_peer_addr() -> String {
    "127.0.0.1:5514".to_string()
}


// ── /api/operations/weekly-trend ─────────────────────────────────────────────

#[derive(Serialize)]
pub(super) struct WeeklyTrendDay {
    date: String,
    status: String,
    pass: u32,
    fail: u32,
    skip: u32,
    prereq_missing: u32,
}

#[derive(Serialize)]
pub(super) struct WeeklyTrendResponse {
    days: Vec<WeeklyTrendDay>,
}

pub(super) fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub(super) fn compute_health(bgp: &[BgpJson]) -> String {
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

pub(super) async fn read_subscription_statuses(
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

pub(super) async fn read_device_vendors(
    db: Arc<lbug::Database>,
) -> Result<HashMap<String, String>, (StatusCode, String)> {
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let rows = conn
            .query("MATCH (d:Device) WHERE d.vendor <> '' RETURN d.address, d.vendor")
            .map_err(|e| e.to_string())?;
        let mut map: HashMap<String, String> = HashMap::new();
        for row in rows {
            let addr = read_str(&row[0]);
            let vendor = read_str(&row[1]);
            if !addr.is_empty() && !vendor.is_empty() {
                map.insert(addr, vendor);
            }
        }
        Ok::<_, String>(map)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

pub(super) async fn read_trust_mark_impact(
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

pub(super) fn option_string(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(super) fn build_site_path_by_id(sites: &[SiteRecord]) -> HashMap<String, String> {
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

pub(super) fn resolve_site_metadata(
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

pub(super) fn read_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

pub(super) fn read_i64(v: &Value) -> i64 {
    match v {
        Value::Int64(n) => *n,
        _ => 0,
    }
}

pub(super) fn read_ts_ns(v: &Value) -> i64 {
    match v {
        Value::TimestampNs(dt) => dt.unix_timestamp_nanos() as i64,
        _ => 0,
    }
}

// ── Device detail endpoint ────────────────────────────────────────────────────

// ── Device enrichment endpoint ────────────────────────────────────────────────








// ── Incidents endpoint ────────────────────────────────────────────────────────

// ── Assignment rule endpoints ─────────────────────────────────────────────────

// ── Device detail types ───────────────────────────────────────────────────────











// ── Assignment types ──────────────────────────────────────────────────────────








// ── Environment handlers ──────────────────────────────────────────────────────

// ── Profiles ──────────────────────────────────────────────────────────────────






// ── Enrichment handlers ───────────────────────────────────────────────────────









// ── Human-in-the-loop remediation approvals (Sprint 4) ───────────────────────


pub(super) fn default_proposal_status() -> String {
    "pending".to_string()
}










pub(super) fn default_verify_wait_secs() -> u64 {
    30
}




// ── ServiceNow integration test endpoint (T2-1) ───────────────────────────────





// ── Output adapter management API (T6-6) ─────────────────────────────────────








// ─── graph insights (T1-4) ───────────────────────────────────────────────────

// ─── explorer (T1-5) ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct ExplorerQueryBody {
    cypher: String,
    /// If set, record last_run_at and row count on this saved-query id.
    saved_query_id: Option<String>,
}

// ─── saved queries CRUD (T1-6) ───────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct CreateSavedQueryBody {
    name: String,
    #[serde(default)]
    description: String,
    cypher: String,
}

// ── embedding handlers (T2-1) ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct UpsertEmbeddingsBody {
    records: Vec<crate::graph::EmbeddingRecord>,
}

#[derive(Serialize)]
pub(super) struct EmbeddingsResponse {
    embeddings: Vec<crate::graph::EmbeddingRecord>,
}

// ── investigation handlers (T3-1 / T3-2) ─────────────────────────────────────

pub(super) fn default_trigger() -> String {
    "operator".into()
}



// ── T4-5 — Governance state endpoint ─────────────────────────────────────────

// ── T5-2 — Grounded incident response ────────────────────────────────────────

// ── T5-3 — Self-describing OpenAPI schema endpoint ───────────────────────────

// ── CV7 T4-4: GET /api/sidecars ───────────────────────────────────────────────
// Surfaces the in-memory sidecar registry as JSON. Consumed by the bonpy UI
// and ops scripts. See `src/sidecar_registry.rs` and `docs/architecture/sidecars.md`.


// ── CV7 T4-6: GET /health ─────────────────────────────────────────────────────
// Returns 200 + JSON `{ "status": "ok" }` by default. When a required sidecar
// is missing past the startup grace window, returns 503 + `{ "status":
// "degraded", "missing_required_sidecars": [...] }`. This is the operational
// "loud" surface that prevents the CV6-era "Detections: 0" silent gap.


#[cfg(test)]
mod tests {
    use super::schema::openapi_schema;

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
