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
use crate::graph::{DetectionRow, GraphStore, REMEDIATION_TRUST_CUTOFF_ISO, SiteRecord, TraceStep};
use crate::{
    archive,
    config::{AssignmentRule, SelectedSubscriptionPath, TargetConfig},
    credentials::{CredentialSummary, CredentialVault, ResolvedCredential},
    discovery::{self, DiscoveryInput},
    event_bus,
    registry::{ApiRegistry, DeviceRegistry, RegistryChange},
};

// ── JSON response types ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct TopologyResponse {
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
}

#[derive(Serialize)]
struct PathResponse {
    /// Device addresses in hop order, source first.
    hops: Vec<String>,
    /// (src_device, src_iface, dst_device, dst_iface) for each hop's link.
    links: Vec<(String, String, String, String)>,
}

#[derive(Serialize)]
struct DetectionsResponse {
    detections: Vec<DetectionRow>,
}

#[derive(Serialize)]
struct TraceResponse {
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
    incidents: Vec<IncidentJson>,
}

#[derive(Deserialize, Default)]
struct IncidentsParams {
    #[serde(default = "default_incident_window")]
    window_secs: u64,
    #[serde(default = "default_incident_limit")]
    limit: u32,
}

fn default_incident_window() -> u64 { 30 }
fn default_incident_limit() -> u32 { 200 }

#[derive(Serialize)]
struct ReadinessResponse {
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
    pub collector_manager: Option<Arc<CollectorManager>>,
}

// ── Router ────────────────────────────────────────────────────────────────────

pub fn router(
    store: Arc<GraphStore>,
    registry: Arc<ApiRegistry>,
    credentials: Arc<CredentialVault>,
    collector_manager: Option<Arc<CollectorManager>>,
) -> Router {
    let state = AppState {
        store,
        registry,
        credentials,
        collector_manager,
    };

    // Serve the Svelte SPA from ui/dist/. Fall back to index.html so
    // client-side routing works (the SPA handles /events and /trace/:id paths).
    let spa = ServeDir::new("ui/dist")
        .not_found_service(tower_http::services::ServeFile::new("ui/dist/index.html"));

    Router::new()
        .route("/api/topology", get(topology_handler))
        .route("/api/path", get(path_handler))
        .route("/api/incidents/grouped", get(incidents_handler))
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
        .route("/api/trace/{id}", get(trace_handler))
        .route("/api/events", get(events_handler))
        .route("/api/devices/{address}", get(device_detail_handler))
        .route("/api/collectors", get(collectors_handler))
        .route(
            "/api/assignment/rules",
            get(assignment_rules_handler).post(set_assignment_rules_handler),
        )
        .route("/api/assignment/status", get(assignment_status_handler))
        .route("/api/assignment/override", post(assignment_override_handler))
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

        // LLDP links with interface counter totals for heatmap
        let link_rows = conn
            .query(
                "MATCH (a:Interface)-[:CONNECTED_TO]->(b:Interface) \
                 RETURN a.device_address, a.name, b.device_address, b.name, \
                        a.in_octets + a.out_octets + b.in_octets + b.out_octets",
            )
            .map_err(|e| e.to_string())?;
        let links_raw: Vec<(String, String, String, String, i64)> = link_rows
            .map(|row| {
                (
                    read_str(&row[0]),
                    read_str(&row[1]),
                    read_str(&row[2]),
                    read_str(&row[3]),
                    read_i64(&row[4]),
                )
            })
            .collect();

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

    // Build role + site map from registry
    let mut role_site: HashMap<String, (String, String)> = HashMap::new();
    if let Ok(targets) = state.registry.list_all_targets() {
        for t in targets {
            role_site.insert(
                t.address.clone(),
                (t.role.unwrap_or_default(), t.site.unwrap_or_default()),
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
            let (role, site) = role_site.remove(&address).unwrap_or_default();
            DeviceJson {
                address,
                hostname,
                vendor,
                role,
                site,
                health,
                bgp,
            }
        })
        .collect();

    let links = links_raw
        .into_iter()
        .map(|(src_device, src_iface, dst_device, dst_iface, bytes_total)| LinkJson {
            src_device,
            src_iface,
            dst_device,
            dst_iface,
            bytes_total,
        })
        .collect();

    Ok(Json(TopologyResponse { devices, links }))
}

#[derive(Deserialize)]
struct PathParams {
    src: String,
    dst: String,
}

/// BFS shortest-path between two devices over LLDP links.
/// Returns the device-address hop list and the link segments traversed.
async fn path_handler(
    State(state): State<AppState>,
    Query(params): Query<PathParams>,
) -> Result<Json<PathResponse>, (StatusCode, String)> {
    let db = state.store.db();
    let (src, dst) = (params.src.clone(), params.dst.clone());

    let all_links = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let rows = conn
            .query(
                "MATCH (a:Interface)-[:CONNECTED_TO]->(b:Interface) \
                 RETURN a.device_address, a.name, b.device_address, b.name",
            )
            .map_err(|e| e.to_string())?;
        Ok::<Vec<(String, String, String, String)>, String>(
            rows.map(|row| {
                (read_str(&row[0]), read_str(&row[1]), read_str(&row[2]), read_str(&row[3]))
            })
            .collect(),
        )
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Build undirected adjacency: device → Vec<(neighbour, src_iface, dst_iface)>
    let mut adj: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
    for (a_dev, a_if, b_dev, b_if) in &all_links {
        adj.entry(a_dev.clone()).or_default().push((b_dev.clone(), a_if.clone(), b_if.clone()));
        adj.entry(b_dev.clone()).or_default().push((a_dev.clone(), b_if.clone(), a_if.clone()));
    }

    if src == dst {
        return Ok(Json(PathResponse { hops: vec![src], links: vec![] }));
    }

    // BFS
    use std::collections::VecDeque;
    let mut visited: HashMap<String, Option<(String, String, String)>> = HashMap::new(); // device → (via_device, via_src_if, via_dst_if)
    visited.insert(src.clone(), None);
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(src.clone());

    'bfs: while let Some(current) = queue.pop_front() {
        if let Some(neighbours) = adj.get(&current) {
            for (nb, src_if, dst_if) in neighbours {
                if visited.contains_key(nb.as_str()) { continue; }
                visited.insert(nb.clone(), Some((current.clone(), src_if.clone(), dst_if.clone())));
                if nb == &dst { break 'bfs; }
                queue.push_back(nb.clone());
            }
        }
    }

    if !visited.contains_key(dst.as_str()) {
        return Ok(Json(PathResponse { hops: vec![], links: vec![] }));
    }

    // Reconstruct path backwards
    let mut hops = vec![dst.clone()];
    let mut link_segs: Vec<(String, String, String, String)> = Vec::new();
    let mut cur = dst.clone();
    while let Some(Some((prev, src_if, dst_if))) = visited.get(&cur) {
        link_segs.push((prev.clone(), src_if.clone(), cur.clone(), dst_if.clone()));
        hops.push(prev.clone());
        cur = prev.clone();
    }
    hops.reverse();
    link_segs.reverse();

    Ok(Json(PathResponse { hops, links: link_segs }))
}

async fn managed_devices_handler(
    State(state): State<AppState>,
) -> Result<Json<ManagedDevicesResponse>, (StatusCode, String)> {
    let targets = state
        .registry
        .list_active()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let statuses = read_subscription_statuses(state.store.db()).await?;

    let devices = targets
        .into_iter()
        .map(|target| managed_device_json(target, &statuses))
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
    if device_counts.get(req.alias.trim()).copied().unwrap_or_default() > 0 {
        return Ok(Json(CredentialMutationResponse {
            success: false,
            error: format!(
                "credential alias '{}' is still referenced by {} device(s)",
                req.alias.trim(),
                device_counts.get(req.alias.trim()).copied().unwrap_or_default()
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
        .resolve(&req.alias)
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
                    device: Some(managed_device_json(device, &HashMap::new())),
                }));
            }
            let statuses = read_subscription_statuses(state.store.db()).await?;
            Ok(Json(MutationResponse {
                success: true,
                error: String::new(),
                device: Some(managed_device_json(device, &statuses)),
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
    if all_sites.iter().any(|candidate| candidate.parent_id == site.id) {
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
            error: format!("cannot delete site '{}' while {} device(s) still reference it", site.name, in_use),
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
            device: Some(managed_device_json(device, &HashMap::new())),
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
                match state
                    .registry
                    .update_device_with_audit(target, "api", &format!("api_bulk_{action}"))
                {
                    Ok(device) => devices.push(managed_device_json(device, &statuses)),
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

    Ok(Json(DetectionsResponse { detections }))
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

    Ok(Json(TraceResponse { steps }))
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

    Ok(Json(OperationsResponse {
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
    }))
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
) -> ManagedDeviceJson {
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
    }
}

fn credential_json(
    credential: CredentialSummary,
    device_counts: &HashMap<String, usize>,
) -> CredentialJson {
    CredentialJson {
        device_count: device_counts.get(&credential.alias).copied().unwrap_or_default(),
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
        return credentials.resolve(&alias).map(Some);
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
            if !site.parent_id.is_empty() && ids.contains(&site.parent_id) && ids.insert(site.id.clone()) {
                changed = true;
            }
        }
    }
    ids
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
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("device '{address}' not found")))?;

    let db = state.store.db();
    let addr_clone = address.clone();

    let (ifaces, bgp, lldp, state_changes, detections) =
        tokio::task::spawn_blocking(move || {
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
        subscription_statuses,
        created_at_ns: target.created_at_ns,
        updated_at_ns: target.updated_at_ns,
        created_by: target.created_by,
        updated_by: target.updated_by,
        last_operator_action: target.last_operator_action,
    }))
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
    Ok(Json(IncidentsResponse { incidents }))
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
        let joined = groups.iter_mut().rev().find(|g| {
            det.fired_at_ns - g[0].fired_at_ns <= window_ns
        });
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
                    (*degree_map.get(&d.device_address).unwrap_or(&0), -(d.fired_at_ns))
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

    incidents.sort_by(|a, b| b.started_at_ns.cmp(&a.started_at_ns));
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
    subscription_statuses: Vec<SubscriptionStatusJson>,
    created_at_ns: i64,
    updated_at_ns: i64,
    created_by: String,
    updated_by: String,
    last_operator_action: String,
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
    let manager = state
        .collector_manager
        .as_ref()
        .ok_or_else(|| (StatusCode::NOT_IMPLEMENTED, "assignment not enabled on this node".to_string()))?;
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
        Ok(_) => Ok(Json(AssignmentOverrideResponse { success: true, error: String::new() })),
        Err(e) => Ok(Json(AssignmentOverrideResponse { success: false, error: format!("{e:#}") })),
    }
}
