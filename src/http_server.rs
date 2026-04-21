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
use tokio_stream::wrappers::BroadcastStream;
use tower_http::{cors::CorsLayer, services::ServeDir};

use crate::graph::{DetectionRow, GraphStore, REMEDIATION_TRUST_CUTOFF_ISO, TraceStep};
use crate::{
    config::TargetConfig,
    credentials::{CredentialSummary, CredentialVault, ResolvedCredential},
    discovery::{self, DiscoveryInput},
    registry::{ApiRegistry, DeviceRegistry},
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
struct ReadinessResponse {
    detection_events: usize,
    state_change_events: usize,
    rule_distribution: HashMap<String, usize>,
    cutoff_iso: String,
    remediation_rows_post_cutoff: usize,
    action_distribution_post_cutoff: HashMap<String, usize>,
    status_distribution_post_cutoff: HashMap<String, usize>,
}

/// Outbound SSE payload — mirrors BonsaiEvent but serialised as JSON.
#[derive(Serialize)]
struct ManagedDevicesResponse {
    devices: Vec<ManagedDeviceJson>,
}

#[derive(Serialize)]
struct ManagedDeviceJson {
    address: String,
    tls_domain: String,
    ca_cert: String,
    vendor: String,
    credential_alias: String,
    username_env: String,
    password_env: String,
    hostname: String,
    role: String,
    site: String,
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
}

#[derive(Deserialize)]
struct RemoveManagedDeviceRequest {
    address: String,
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

// ── App state ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<GraphStore>,
    pub registry: Arc<ApiRegistry>,
    pub credentials: Arc<CredentialVault>,
}

// ── Router ────────────────────────────────────────────────────────────────────

pub fn router(
    store: Arc<GraphStore>,
    registry: Arc<ApiRegistry>,
    credentials: Arc<CredentialVault>,
) -> Router {
    let state = AppState {
        store,
        registry,
        credentials,
    };

    // Serve the Svelte SPA from ui/dist/. Fall back to index.html so
    // client-side routing works (the SPA handles /events and /trace/:id paths).
    let spa = ServeDir::new("ui/dist")
        .not_found_service(tower_http::services::ServeFile::new("ui/dist/index.html"));

    Router::new()
        .route("/api/topology", get(topology_handler))
        .route(
            "/api/onboarding/devices",
            get(managed_devices_handler).post(add_managed_device_handler),
        )
        .route(
            "/api/onboarding/devices/remove",
            post(remove_managed_device_handler),
        )
        .route("/api/onboarding/discover", post(discover_handler))
        .route(
            "/api/credentials",
            get(credentials_handler).post(add_credential_handler),
        )
        .route("/api/credentials/remove", post(remove_credential_handler))
        .route("/api/detections", get(detections_handler))
        .route("/api/readiness", get(readiness_handler))
        .route("/api/trace/{id}", get(trace_handler))
        .route("/api/events", get(events_handler))
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

        // LLDP links (dedup by sorting both sides)
        let link_rows = conn
            .query(
                "MATCH (a:Interface)-[:CONNECTED_TO]->(b:Interface) \
                 RETURN a.device_address, a.name, b.device_address, b.name",
            )
            .map_err(|e| e.to_string())?;
        let links_raw: Vec<(String, String, String, String)> = link_rows
            .map(|row| {
                (
                    read_str(&row[0]),
                    read_str(&row[1]),
                    read_str(&row[2]),
                    read_str(&row[3]),
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

    // Group BGP by device
    let mut bgp_by_device: HashMap<String, Vec<BgpJson>> = HashMap::new();
    for (dev, peer, state, peer_as) in bgp_raw {
        bgp_by_device.entry(dev).or_default().push(BgpJson {
            peer,
            state,
            peer_as,
        });
    }

    // Build device list with computed health
    let devices: Vec<DeviceJson> = devices_raw
        .into_iter()
        .map(|(address, vendor, hostname)| {
            let bgp = bgp_by_device.remove(&address).unwrap_or_default();
            let health = compute_health(&bgp);
            DeviceJson {
                address,
                hostname,
                vendor,
                health,
                bgp,
            }
        })
        .collect();

    let links = links_raw
        .into_iter()
        .map(|(src_device, src_iface, dst_device, dst_iface)| LinkJson {
            src_device,
            src_iface,
            dst_device,
            dst_iface,
        })
        .collect();

    Ok(Json(TopologyResponse { devices, links }))
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
    let credentials = state
        .credentials
        .list()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?
        .into_iter()
        .map(credential_json)
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
        Ok(credential) => Ok(Json(CredentialMutationResponse {
            success: true,
            error: String::new(),
            credential: Some(credential_json(credential)),
        })),
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
    match state.credentials.remove(&req.alias) {
        Ok(Some(credential)) => Ok(Json(CredentialMutationResponse {
            success: true,
            error: String::new(),
            credential: Some(credential_json(credential)),
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

async fn add_managed_device_handler(
    State(state): State<AppState>,
    Json(req): Json<ManagedDeviceRequest>,
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
    }
    let address = target.address.clone();
    let result = match state.registry.add_device(target.clone()) {
        Ok(device) => Ok(device),
        Err(add_error) => match state.registry.get_device(&address) {
            Ok(Some(_)) => state
                .registry
                .update_device(target)
                .map_err(|update_error| {
                    format!("add failed: {add_error:#}; update failed: {update_error:#}")
                }),
            _ => Err(add_error.to_string()),
        },
    };

    match result {
        Ok(device) => {
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

async fn events_handler(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.store.subscribe_events();

    let stream = BroadcastStream::new(rx).map(|item| {
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

    Sse::new(stream).keep_alive(KeepAlive::default())
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

fn managed_device_json(
    target: TargetConfig,
    statuses: &HashMap<String, Vec<SubscriptionStatusJson>>,
) -> ManagedDeviceJson {
    let address = target.address;
    ManagedDeviceJson {
        tls_domain: target.tls_domain.unwrap_or_default(),
        ca_cert: target.ca_cert.unwrap_or_default(),
        vendor: target.vendor.unwrap_or_default(),
        credential_alias: target.credential_alias.unwrap_or_default(),
        username_env: target.username_env.unwrap_or_default(),
        password_env: target.password_env.unwrap_or_default(),
        hostname: target.hostname.unwrap_or_default(),
        role: target.role.unwrap_or_default(),
        site: target.site.unwrap_or_default(),
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
    })
}

fn credential_json(credential: CredentialSummary) -> CredentialJson {
    CredentialJson {
        alias: credential.alias,
        created_at_ns: credential.created_at_ns,
        updated_at_ns: credential.updated_at_ns,
        last_used_at_ns: credential.last_used_at_ns,
    }
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
