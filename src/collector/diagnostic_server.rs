/// Narrowly-scoped diagnostic HTTP server for collector nodes.
///
/// Disabled by default. Enable by setting `collector.diagnostic_port` in bonsai.toml.
/// Exposes only three endpoints for operator troubleshooting:
///
///   GET /health              — liveness (200 OK always while process is up)
///   GET /api/readiness       — registered with core successfully?
///   GET /api/collector/status — queue depth, subscriptions, last heartbeat, assigned devices
///
/// All other paths return 404. Optional basic auth via BONSAI_COLLECTOR_DIAG_PASSWORD.
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::get,
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::watch;
use tracing::{info, warn};

#[derive(Clone, Default)]
pub struct DiagnosticState {
    inner: Arc<Mutex<DiagnosticStateInner>>,
}

#[derive(Default)]
struct DiagnosticStateInner {
    registered_with_core: bool,
    collector_id: String,
    queue_depth: u64,
    subscription_count: u32,
    assigned_device_addresses: Vec<String>,
    last_heartbeat_unix_secs: u64,
    uptime_start_unix_secs: u64,
    receiver_statuses: Vec<ReceiverStatusEntry>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ReceiverStatusEntry {
    pub name: String,
    pub state: String,
    pub addr: String,
    pub packet_count: u64,
    pub error_count: u64,
    pub last_error: Option<String>,
}

impl DiagnosticState {
    pub fn new(collector_id: &str) -> Self {
        let s = Self::default();
        let mut inner = s.inner.lock().unwrap();
        inner.collector_id = collector_id.to_string();
        inner.uptime_start_unix_secs = now_unix_secs();
        drop(inner);
        s
    }

    pub fn mark_registered(&self) {
        self.inner.lock().unwrap().registered_with_core = true;
    }

    pub fn update_stats(&self, queue_depth: u64, subscription_count: u32, assigned: Vec<String>) {
        let mut inner = self.inner.lock().unwrap();
        inner.queue_depth = queue_depth;
        inner.subscription_count = subscription_count;
        inner.assigned_device_addresses = assigned;
        inner.last_heartbeat_unix_secs = now_unix_secs();
    }

    pub fn update_queue_depth(&self, queue_depth: u64) {
        self.inner.lock().unwrap().queue_depth = queue_depth;
    }

    pub fn update_receiver_statuses(&self, statuses: Vec<ReceiverStatusEntry>) {
        self.inner.lock().unwrap().receiver_statuses = statuses;
    }

    pub fn receiver_statuses(&self) -> Vec<ReceiverStatusEntry> {
        self.inner.lock().unwrap().receiver_statuses.clone()
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Serialize)]
struct ReadinessResponse {
    ready: bool,
    reason: String,
}

#[derive(Serialize)]
struct CollectorStatusResponse {
    collector_id: String,
    registered_with_core: bool,
    queue_depth: u64,
    subscription_count: u32,
    assigned_devices: Vec<String>,
    last_heartbeat_unix_secs: u64,
    uptime_secs: u64,
    receiver_statuses: Vec<ReceiverStatusEntry>,
}

async fn health_handler() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn readiness_handler(
    State(state): State<DiagnosticState>,
) -> (StatusCode, Json<ReadinessResponse>) {
    let inner = state.inner.lock().unwrap();
    if inner.registered_with_core {
        (
            StatusCode::OK,
            Json(ReadinessResponse {
                ready: true,
                reason: String::new(),
            }),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadinessResponse {
                ready: false,
                reason: "not yet registered with core".to_string(),
            }),
        )
    }
}

async fn status_handler(
    State(state): State<DiagnosticState>,
    headers: HeaderMap,
) -> Result<Json<CollectorStatusResponse>, StatusCode> {
    // Optional auth — check BONSAI_COLLECTOR_DIAG_PASSWORD if set.
    // Timing-safe comparison prevents password oracle attacks.
    if let Ok(required) = std::env::var("BONSAI_COLLECTOR_DIAG_PASSWORD")
        && !required.is_empty()
    {
        let provided = headers
            .get("x-diag-password")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        // Pad to equal length before comparing to prevent length-based timing leak
        let req_bytes = required.as_bytes();
        let prov_bytes = provided.as_bytes();
        let len_ok = req_bytes.len() == prov_bytes.len();
        // Always run ct_eq even on length mismatch to avoid timing short-circuit
        let content_ok = req_bytes.ct_eq(prov_bytes).unwrap_u8() == 1;
        if !(len_ok && content_ok) {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    let inner = state.inner.lock().unwrap();
    let uptime_secs = now_unix_secs().saturating_sub(inner.uptime_start_unix_secs);
    Ok(Json(CollectorStatusResponse {
        collector_id: inner.collector_id.clone(),
        registered_with_core: inner.registered_with_core,
        queue_depth: inner.queue_depth,
        subscription_count: inner.subscription_count,
        assigned_devices: inner.assigned_device_addresses.clone(),
        last_heartbeat_unix_secs: inner.last_heartbeat_unix_secs,
        uptime_secs,
        receiver_statuses: inner.receiver_statuses.clone(),
    }))
}

async fn fallback_handler() -> StatusCode {
    StatusCode::NOT_FOUND
}

/// Starts the diagnostic HTTP server on `port`. Returns immediately; the server
/// runs in a background task and honours `shutdown`.
pub async fn start(port: u16, state: DiagnosticState, mut shutdown: watch::Receiver<bool>) {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            warn!(%port, %e, "failed to bind collector diagnostic server");
            return;
        }
    };

    // Security advisory: this server runs plain HTTP; the x-diag-password header
    // travels in cleartext. Restrict diagnostic_port to loopback-only (127.x.x.x)
    // or place behind a TLS-terminating proxy in any non-isolated environment.
    let has_password = std::env::var("BONSAI_COLLECTOR_DIAG_PASSWORD")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    if has_password {
        warn!(
            %addr,
            "SECURITY: collector diagnostic server is running plain HTTP — \
             BONSAI_COLLECTOR_DIAG_PASSWORD will be transmitted in cleartext. \
             Bind to 127.0.0.1 or use a TLS-terminating proxy."
        );
    }
    info!(%addr, "collector diagnostic server listening");

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/api/readiness", get(readiness_handler))
        .route("/api/collector/status", get(status_handler))
        .fallback(fallback_handler)
        .with_state(state);

    tokio::select! {
        result = axum::serve(listener, app) => {
            if let Err(e) = result {
                warn!(%e, "collector diagnostic server error");
            }
        }
        _ = shutdown.changed() => {
            info!("collector diagnostic server shutting down");
        }
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
