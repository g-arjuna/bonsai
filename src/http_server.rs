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
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::get,
    Json,
};
use futures::stream::{Stream, StreamExt};
use lbug::{Connection, Value};
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;
use tower_http::{cors::CorsLayer, services::ServeDir};

use crate::graph::{DetectionRow, GraphStore, TraceStep};

// ── JSON response types ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct TopologyResponse {
    devices: Vec<DeviceJson>,
    links:   Vec<LinkJson>,
}

#[derive(Serialize)]
struct DeviceJson {
    address:  String,
    hostname: String,
    vendor:   String,
    health:   String,  // "healthy" | "warn" | "critical"
    bgp:      Vec<BgpJson>,
}

#[derive(Serialize)]
struct BgpJson {
    peer:      String,
    state:     String,
    peer_as:   i64,
}

#[derive(Serialize)]
struct LinkJson {
    src_device: String,
    src_iface:  String,
    dst_device: String,
    dst_iface:  String,
}

#[derive(Serialize)]
struct DetectionsResponse {
    detections: Vec<DetectionRow>,
}

#[derive(Serialize)]
struct TraceResponse {
    steps: Vec<TraceStep>,
}

/// Outbound SSE payload — mirrors BonsaiEvent but serialised as JSON.
#[derive(Serialize)]
struct SsePayload {
    device_address:        String,
    event_type:            String,
    detail_json:           String,
    occurred_at_ns:        i64,
    state_change_event_id: String,
}

// ── Query params ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct DetectionsParams {
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 { 50 }

// ── App state ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<GraphStore>,
}

// ── Router ────────────────────────────────────────────────────────────────────

pub fn router(store: Arc<GraphStore>) -> Router {
    let state = AppState { store };

    // Serve the Svelte SPA from ui/dist/. Fall back to index.html so
    // client-side routing works (the SPA handles /events and /trace/:id paths).
    let spa = ServeDir::new("ui/dist")
        .not_found_service(tower_http::services::ServeFile::new("ui/dist/index.html"));

    Router::new()
        .route("/api/topology",        get(topology_handler))
        .route("/api/detections",      get(detections_handler))
        .route("/api/trace/{id}",      get(trace_handler))
        .route("/api/events",          get(events_handler))
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
            .map(|row| (read_str(&row[0]), read_str(&row[1]), read_str(&row[2]), read_str(&row[3])))
            .collect();

        // BGP neighbors
        let bgp_rows = conn
            .query(
                "MATCH (n:BgpNeighbor) \
                 RETURN n.device_address, n.peer_address, n.session_state, n.peer_as",
            )
            .map_err(|e| e.to_string())?;
        let bgp_raw: Vec<(String, String, String, i64)> = bgp_rows
            .map(|row| (read_str(&row[0]), read_str(&row[1]), read_str(&row[2]), read_i64(&row[3])))
            .collect();

        Ok::<_, String>((devices_raw, links_raw, bgp_raw))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Group BGP by device
    let mut bgp_by_device: HashMap<String, Vec<BgpJson>> = HashMap::new();
    for (dev, peer, state, peer_as) in bgp_raw {
        bgp_by_device.entry(dev).or_default().push(BgpJson { peer, state, peer_as });
    }

    // Build device list with computed health
    let devices: Vec<DeviceJson> = devices_raw
        .into_iter()
        .map(|(address, vendor, hostname)| {
            let bgp = bgp_by_device.remove(&address).unwrap_or_default();
            let health = compute_health(&bgp);
            DeviceJson { address, hostname, vendor, health, bgp }
        })
        .collect();

    let links = links_raw
        .into_iter()
        .map(|(src_device, src_iface, dst_device, dst_iface)| LinkJson {
            src_device, src_iface, dst_device, dst_iface,
        })
        .collect();

    Ok(Json(TopologyResponse { devices, links }))
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

async fn events_handler(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.store.subscribe_events();

    let stream = BroadcastStream::new(rx).map(|item| {
        let data = match item {
            Ok(ev) => serde_json::to_string(&SsePayload {
                device_address:        ev.device_address,
                event_type:            ev.event_type,
                detail_json:           ev.detail_json,
                occurred_at_ns:        ev.occurred_at_ns,
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

fn read_str(v: &Value) -> String {
    match v { Value::String(s) => s.clone(), _ => String::new() }
}

fn read_i64(v: &Value) -> i64 {
    match v { Value::Int64(n) => *n, _ => 0 }
}
