#![allow(unused_imports,dead_code,unused_variables)]
use super::*;

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

