//! B10–B13: Reduced Bonsai routing service, versioned internal API (`/internal/routing/v1/*`),
//! public proxy handlers (`/api/routing/v1/*`), and checkpointed graph summary projector.
//!
//! Runs without Kafka, NATS, or external ML sidecars, using `rbmp-core`, `rbmp-rib`,
//! and `rbmp-store` as the single canonical BMP/BGP protocol and history engine.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use rbmp_core::bgp::types::{AfiSafi, BgpDecodeContext, FamilySupportStatus};
use rbmp_core::bmp::parser::{
    DEFAULT_MAX_FRAME, parse_bmp_message_with_context, parse_peer_header, read_bmp_frame,
};
use rbmp_core::bmp::types::{BmpMessage, BmpPayload, PeerKey, RibType};
use rbmp_rib::RibManager;
use rbmp_rib::event::{RibEventPayload, RouteAction};
use rbmp_rib::session::PeerState;
use rbmp_store::{BmpJournal, PrefixHistoryItem, RibEventJournal};
use serde::{Deserialize, Serialize};
use tokio::io::BufReader;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use uuid::Uuid;

pub const ROUTING_API_VERSION: &str = "v1";
pub const ROUTING_CHECKPOINT_CONFIG_CLASS: &str = "routing_checkpoint";
pub const ROUTING_CHECKPOINT_ID: &str = "routing-projection-checkpoint-v1";

/// Configuration for the reduced routing process (`bonsai-routing`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingServiceConfig {
    pub bmp_listen_addr: String,
    pub http_listen_addr: String,
    pub bmp_journal_path: Option<PathBuf>,
    pub event_journal_path: Option<PathBuf>,
    pub max_frame_bytes: u32,
    #[serde(skip_serializing)]
    pub internal_service_token: Option<String>,
}

impl Default for RoutingServiceConfig {
    fn default() -> Self {
        Self {
            bmp_listen_addr: std::env::var("BONSAI_ROUTING_BMP_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:11019".to_string()),
            http_listen_addr: std::env::var("BONSAI_ROUTING_HTTP_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:8081".to_string()),
            bmp_journal_path: std::env::var("BONSAI_ROUTING_BMP_JOURNAL")
                .ok()
                .map(PathBuf::from),
            event_journal_path: std::env::var("BONSAI_ROUTING_EVENT_JOURNAL")
                .ok()
                .map(PathBuf::from),
            max_frame_bytes: DEFAULT_MAX_FRAME,
            internal_service_token: std::env::var("BONSAI_ROUTING_SERVICE_TOKEN")
                .ok()
                .filter(|s| !s.trim().is_empty()),
        }
    }
}

impl RoutingServiceConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.bmp_listen_addr.trim().is_empty() {
            return Err("bmp_listen_addr must not be empty".to_string());
        }
        if self.http_listen_addr.trim().is_empty() {
            return Err("http_listen_addr must not be empty".to_string());
        }
        if self.max_frame_bytes < 6 || self.max_frame_bytes > 16 * 1024 * 1024 {
            return Err(format!(
                "max_frame_bytes ({}) must be between 6 and 16777216",
                self.max_frame_bytes
            ));
        }
        Ok(())
    }
}

/// Transport loss metadata when a BMP TCP connection drops without an explicit BGP withdrawal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransportGapRecord {
    pub speaker_addr: IpAddr,
    #[serde(default)]
    pub session_epoch: Option<u64>,
    pub disconnected_at: DateTime<Utc>,
    pub reason: String,
}

/// Shared state of the reduced routing service.
pub struct RoutingServiceState {
    pub config: RoutingServiceConfig,
    pub rib: RwLock<RibManager>,
    pub bmp_journal: BmpJournal,
    pub event_journal: RibEventJournal,
    pub decode_contexts: RwLock<HashMap<PeerKey, BgpDecodeContext>>,
    pub transport_gaps: RwLock<HashMap<IpAddr, TransportGapRecord>>,
}

impl RoutingServiceState {
    pub async fn new(config: RoutingServiceConfig) -> anyhow::Result<Arc<Self>> {
        config
            .validate()
            .map_err(|e| anyhow::anyhow!("Invalid RoutingServiceConfig: {e}"))?;

        let event_journal = RibEventJournal::open(config.event_journal_path.as_ref()).await?;
        let (mut rib_mgr, _rx) = RibManager::new(4096);
        let mut recovered_contexts = HashMap::new();
        let mut recovered_gaps = HashMap::new();

        if let Some(ref bmp_path) = config.bmp_journal_path {
            if bmp_path.exists() {
                let outcome = BmpJournal::replay_into_with_state(bmp_path, &mut rib_mgr)?;
                recovered_contexts = outcome.decode_contexts;
                recovered_gaps = outcome
                    .transport_gaps
                    .into_iter()
                    .map(|(ip, g)| {
                        (
                            ip,
                            TransportGapRecord {
                                speaker_addr: g.speaker_addr,
                                session_epoch: g.session_epoch,
                                disconnected_at: g.disconnected_at,
                                reason: g.reason,
                            },
                        )
                    })
                    .collect();

                let existing_seqs = event_journal.existing_sequences().await;
                let missing_events: Vec<_> = outcome
                    .emitted_events
                    .into_iter()
                    .filter(|ev| !existing_seqs.contains(&ev.event_key.sequence))
                    .collect();
                if !missing_events.is_empty() {
                    event_journal.append_batch(&missing_events).await?;
                }
            }
        }

        let latest_seq = event_journal.latest_seq().await;
        rib_mgr.ensure_min_event_seq(latest_seq + 1);

        let bmp_journal = BmpJournal::open(config.bmp_journal_path.as_ref()).await?;

        Ok(Arc::new(Self {
            config,
            rib: RwLock::new(rib_mgr),
            bmp_journal,
            event_journal,
            decode_contexts: RwLock::new(recovered_contexts),
            transport_gaps: RwLock::new(recovered_gaps),
        }))
    }

    /// Decode a raw BMP wire frame using session-aware capabilities when available,
    /// journal the accepted observation (including wire bytes and connection epoch),
    /// apply to `RibManager`, and journal emitted `RibEvent`s.
    pub async fn ingest_bmp_bytes(
        &self,
        speaker_addr: IpAddr,
        session_epoch: Option<u64>,
        frame: &[u8],
    ) -> anyhow::Result<BmpMessage> {
        // Determine session-aware decode context if this is a RouteMonitoring (type 0) frame
        let ctx = if frame.len() >= 48 && frame[0] == 3 && frame[5] == 0 {
            if let Ok((peer_hdr, _)) = parse_peer_header(&frame[6..]) {
                let peer_key = PeerKey::from_header(speaker_addr, &peer_hdr);
                let contexts = self.decode_contexts.read().await;
                contexts
                    .get(&peer_key)
                    .cloned()
                    .unwrap_or_else(|| BgpDecodeContext::new(peer_hdr.peer_flags.four_byte_asn()))
            } else {
                BgpDecodeContext::default()
            }
        } else {
            BgpDecodeContext::default()
        };

        let payload =
            parse_bmp_message_with_context(frame, speaker_addr, self.config.max_frame_bytes, &ctx)?;
        let msg = BmpMessage {
            id: Uuid::new_v4(),
            received_at: Utc::now(),
            speaker_addr,
            payload,
        };
        self.ingest_bmp_message_internal(msg, session_epoch, Some(frame))
            .await
    }

    /// Persist a parsed `BmpMessage` to the raw journal, process through `RibManager`,
    /// and persist all emitted `RibEvent`s directly to `RibEventJournal`.
    pub async fn ingest_bmp_message(
        &self,
        msg: BmpMessage,
        session_epoch: Option<u64>,
    ) -> anyhow::Result<BmpMessage> {
        self.ingest_bmp_message_internal(msg, session_epoch, None)
            .await
    }

    async fn ingest_bmp_message_internal(
        &self,
        msg: BmpMessage,
        session_epoch: Option<u64>,
        raw_frame: Option<&[u8]>,
    ) -> anyhow::Result<BmpMessage> {
        self.bmp_journal
            .append_observation(&msg, session_epoch, raw_frame)
            .await?;

        // Update session-aware decode context on PeerUp per RFC 7911 Section 5
        if let BmpPayload::PeerUp(ref pu) = msg.payload {
            let peer_key = PeerKey::from_header(msg.speaker_addr, &pu.peer_header);
            let ctx = BgpDecodeContext::from_negotiated_open(
                pu.peer_header.peer_flags.four_byte_asn(),
                &pu.sent_open.capabilities,
                &pu.recv_open.capabilities,
            );
            self.decode_contexts.write().await.insert(peer_key, ctx);
        }

        let (emitted, effective_epoch) = {
            let mut rib = self.rib.write().await;
            let current_ep = session_epoch.or_else(|| {
                rib.speaker(msg.speaker_addr)
                    .map(|s| s.source.session_epoch)
            });
            let events = rib.process_for_epoch(msg.clone(), session_epoch);
            let post_ep = current_ep.or_else(|| {
                rib.speaker(msg.speaker_addr)
                    .map(|s| s.source.session_epoch)
            });
            (events, post_ep)
        };

        // Epoch-aware transport gap clearing (R04): only clear a speaker's gap when a newer
        // session epoch establishes synchronization (Initiation / PeerUp) or on EndOfRib / Termination.
        {
            let mut gaps = self.transport_gaps.write().await;
            if let Some(existing_gap) = gaps.get(&msg.speaker_addr) {
                if rbmp_store::should_clear_transport_gap(
                    existing_gap.session_epoch,
                    effective_epoch,
                    &msg.payload,
                ) {
                    gaps.remove(&msg.speaker_addr);
                }
            }
        }

        self.event_journal.append_batch(&emitted).await?;

        Ok(msg)
    }

    /// Record BMP transport loss without fabricating BGP route withdrawals.
    /// Last-known routes remain in the RIB marked under a transport gap.
    pub async fn record_transport_disconnect(
        &self,
        speaker_addr: IpAddr,
        reason: impl Into<String>,
    ) {
        self.record_transport_disconnect_for_epoch(speaker_addr, None, reason)
            .await;
    }

    /// Epoch-fenced BMP transport disconnect recorder (R04).
    /// Stale disconnects from an older TCP connection epoch (< active speaker epoch) are ignored,
    /// while active disconnects clear EoR markers and are persisted durably to `BmpJournal`.
    pub async fn record_transport_disconnect_for_epoch(
        &self,
        speaker_addr: IpAddr,
        session_epoch: Option<u64>,
        reason: impl Into<String>,
    ) {
        let reason_str = reason.into();
        let effective_epoch = {
            let mut rib = self.rib.write().await;
            let active_epoch = rib.speaker(speaker_addr).map(|s| s.source.session_epoch);
            if let (Some(disc_ep), Some(act_ep)) = (session_epoch, active_epoch) {
                if disc_ep < act_ep {
                    tracing::debug!(
                        %speaker_addr,
                        disc_ep,
                        act_ep,
                        "Ignoring stale transport disconnect from older session epoch"
                    );
                    return;
                }
            }
            rib.clear_speaker_eor(speaker_addr);
            session_epoch.or(active_epoch)
        };

        let disconnected_at = Utc::now();
        let gap = TransportGapRecord {
            speaker_addr,
            session_epoch: effective_epoch,
            disconnected_at,
            reason: reason_str.clone(),
        };
        let journal_gap = rbmp_store::JournalTransportGap {
            speaker_addr,
            session_epoch: effective_epoch,
            disconnected_at,
            reason: reason_str,
        };
        let _ = self
            .bmp_journal
            .append_transport_disconnect(&journal_gap)
            .await;
        self.transport_gaps.write().await.insert(speaker_addr, gap);
    }

    /// Overall data-quality completeness state across observed speakers/peers.
    pub async fn overall_completeness(&self) -> &'static str {
        let gaps = self.transport_gaps.read().await;
        if !gaps.is_empty() {
            return "gap";
        }
        let rib = self.rib.read().await;
        let speakers = rib.speakers();
        if speakers.is_empty() {
            return "syncing";
        }
        let mut any_peer = false;
        let mut all_complete = true;
        for spk in speakers {
            if spk.peers.is_empty() {
                all_complete = false;
                continue;
            }
            for peer in spk.peers.values() {
                any_peer = true;
                if peer.state != PeerState::Up {
                    return "gap";
                }
                if peer.eor_families.is_empty() {
                    all_complete = false;
                }
            }
        }
        if !any_peer {
            "syncing"
        } else if all_complete {
            "complete_for_observed_view"
        } else {
            "syncing"
        }
    }
}

/// Spawn a bounded TCP BMP listener that feeds accepted frames into `RoutingServiceState`.
pub async fn run_bmp_listener(
    listener: TcpListener,
    state: Arc<RoutingServiceState>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    break;
                }
            }
            accept_res = listener.accept() => {
                let Ok((stream, peer_sock)) = accept_res else { continue; };
                let speaker_ip = peer_sock.ip();
                let st = Arc::clone(&state);
                tokio::spawn(async move {
                    let epoch = {
                        let mut rib = st.rib.write().await;
                        rib.open_speaker_session(speaker_ip, Utc::now())
                    };
                    let mut reader = BufReader::new(stream);
                    let hdr_timeout = Duration::from_secs(30);
                    let body_timeout = Duration::from_secs(10);
                    loop {
                        match read_bmp_frame(&mut reader, st.config.max_frame_bytes, hdr_timeout, body_timeout).await {
                            Ok(Some(frame)) => {
                                if let Err(e) = st.ingest_bmp_bytes(speaker_ip, Some(epoch), &frame).await {
                                    tracing::warn!(%speaker_ip, error = %e, "Rejected invalid BMP frame");
                                    break;
                                }
                            }
                            Ok(None) => {
                                // TCP EOF without BMP Termination = observation gap, not BGP withdraw
                                st.record_transport_disconnect_for_epoch(speaker_ip, Some(epoch), "bmp_tcp_eof").await;
                                break;
                            }
                            Err(e) => {
                                st.record_transport_disconnect_for_epoch(
                                    speaker_ip,
                                    Some(epoch),
                                    format!("bmp_transport_error: {e}"),
                                )
                                .await;
                                break;
                            }
                        }
                    }
                });
            }
        }
    }
}

// ── Internal Routing API (`/internal/routing/v1/*`) ──────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct PeersQuery {
    pub speaker: Option<String>,
    pub vrf_rd: Option<String>,
    pub state: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
pub struct RoutesQuery {
    pub speaker: Option<String>,
    pub peer: Option<String>,
    pub view: Option<String>,
    pub vrf_rd: Option<String>,
    pub prefix: Option<String>,
    pub family: Option<String>,
    pub limit: Option<usize>,
    pub cursor: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
pub struct PrefixHistoryQuery {
    pub prefix: Option<String>,
    pub speaker: Option<String>,
    pub vrf_rd: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
pub struct EventsQuery {
    pub after_seq: Option<u64>,
    pub speaker: Option<String>,
    pub vrf_rd: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct IngestBmpRequest {
    pub speaker_addr: String,
    pub session_epoch: Option<u64>,
    pub frame_hex: String,
}

fn check_internal_token(headers: &HeaderMap, state: &RoutingServiceState) -> Result<(), Response> {
    use subtle::ConstantTimeEq;
    if let Some(ref expected) = state.config.internal_service_token {
        let provided = headers
            .get("x-bonsai-routing-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let matches = provided.len() == expected.len()
            && bool::from(provided.as_bytes().ct_eq(expected.as_bytes()));
        if !matches {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "unauthorized_internal_client",
                    "message": "Missing or invalid x-bonsai-routing-token"
                })),
            )
                .into_response());
        }
    }
    Ok(())
}

pub fn build_internal_routing_router(state: Arc<RoutingServiceState>) -> Router {
    Router::new()
        .route("/internal/routing/v1/health", get(internal_health_handler))
        .route(
            "/internal/routing/v1/speakers",
            get(internal_speakers_handler),
        )
        .route("/internal/routing/v1/peers", get(internal_peers_handler))
        .route("/internal/routing/v1/routes", get(internal_routes_handler))
        .route(
            "/internal/routing/v1/prefix-history",
            get(internal_prefix_history_handler),
        )
        .route(
            "/internal/routing/v1/coverage",
            get(internal_coverage_handler),
        )
        .route(
            "/internal/routing/v1/evidence-coverage",
            get(internal_evidence_coverage_handler),
        )
        .route(
            "/internal/routing/v1/prefix-journey",
            get(internal_prefix_journey_handler),
        )
        .route(
            "/internal/routing/v1/peer-timeline",
            get(internal_peer_timeline_handler),
        )
        .route(
            "/internal/routing/v1/compare-snapshots",
            get(internal_compare_snapshots_handler),
        )
        .route(
            "/internal/routing/v1/explain-prefix",
            get(internal_explain_prefix_handler),
        )
        .route(
            "/internal/routing/v1/adjudicate-prefix",
            post(internal_adjudicate_prefix_handler),
        )
        .route(
            "/internal/routing/v1/adjudications",
            get(internal_adjudications_handler),
        )
        .route("/internal/routing/v1/events", get(internal_events_handler))
        .route(
            "/internal/routing/v1/ingest-bmp",
            post(internal_ingest_bmp_handler),
        )
        .with_state(state)
}

async fn internal_health_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_health_json(&state).await).into_response()
}

async fn internal_speakers_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_speakers_json(&state).await).into_response()
}

async fn internal_peers_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<PeersQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_peers_json(&state, &q).await).into_response()
}

async fn internal_routes_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<RoutesQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    let (status, val) = build_routes_json(&state, &q).await;
    (status, Json(val)).into_response()
}

async fn internal_prefix_history_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<PrefixHistoryQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    let (status, val) = build_prefix_history_json(&state, &q).await;
    (status, Json(val)).into_response()
}

async fn internal_coverage_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_coverage_json(&state).await).into_response()
}

async fn internal_evidence_coverage_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<EvidenceCoverageQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_evidence_coverage_json(&state, &q).await).into_response()
}

async fn internal_prefix_journey_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<PrefixJourneyQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    let (status, val) = build_prefix_journey_json(&state, &q).await;
    (status, Json(val)).into_response()
}

async fn internal_peer_timeline_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<PeerTimelineQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_peer_timeline_json(&state, &q).await).into_response()
}

async fn internal_compare_snapshots_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<CompareSnapshotsQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_compare_snapshots_json(&state, &q).await).into_response()
}

async fn internal_explain_prefix_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<ExplainPrefixQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    let (status, val) = build_explain_prefix_json(&state, &q).await;
    (status, Json(val)).into_response()
}

async fn internal_adjudicate_prefix_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Json(req): Json<OperatorAdjudicationRequest>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    let (status, val) = build_adjudicate_prefix_json(req);
    (status, Json(val)).into_response()
}

async fn internal_adjudications_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_adjudications_json()).into_response()
}

async fn internal_events_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_events_json(&state, &q).await).into_response()
}

async fn internal_ingest_bmp_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Json(req): Json<IngestBmpRequest>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    let Ok(speaker_addr) = req.speaker_addr.parse::<IpAddr>() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_speaker_addr",
                "message": format!("Invalid speaker IP address: {}", req.speaker_addr)
            })),
        )
            .into_response();
    };
    let Ok(bytes) = hex::decode(req.frame_hex.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_frame_hex",
                "message": "frame_hex must be valid hex-encoded BMP PDU bytes"
            })),
        )
            .into_response();
    };
    match state
        .ingest_bmp_bytes(speaker_addr, req.session_epoch, &bytes)
        .await
    {
        Ok(msg) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "accepted": true,
                "message_id": msg.id,
                "speaker_addr": msg.speaker_addr,
                "durable_sequence": state.event_journal.latest_seq().await,
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "accepted": false,
                "error": "bmp_parse_or_journal_failed",
                "message": e.to_string(),
            })),
        )
            .into_response(),
    }
}

// ── Shared JSON Builders (used by internal API and in-process Bonsai proxy) ──

pub async fn build_health_json(state: &RoutingServiceState) -> serde_json::Value {
    let rib = state.rib.read().await;
    let speakers_connected = rib.speakers().len();
    let peers_up = rib.total_peers_up();
    let total_routes = rib.total_routes();
    drop(rib);
    let latest_sequence = state.event_journal.latest_seq().await;
    let completeness = state.overall_completeness().await;
    let gaps = state.transport_gaps.read().await.len();

    serde_json::json!({
        "status": "ok",
        "service": "bonsai-routing",
        "api_version": ROUTING_API_VERSION,
        "speakers_connected": speakers_connected,
        "peers_up": peers_up,
        "total_routes": total_routes,
        "durable_sequence": latest_sequence,
        "transport_gaps": gaps,
        "completeness_state": completeness,
        "receive_time": Utc::now().to_rfc3339(),
    })
}

pub async fn build_speakers_json(state: &RoutingServiceState) -> serde_json::Value {
    let rib = state.rib.read().await;
    let gaps = state.transport_gaps.read().await;
    let latest_seq = state.event_journal.latest_seq().await;

    let mut list = Vec::new();
    for spk in rib.speakers() {
        let gap = gaps.get(&spk.speaker_addr);
        let completeness = if gap.is_some() {
            "gap"
        } else if spk
            .peers
            .values()
            .any(|p| p.state == PeerState::Up && !p.eor_families.is_empty())
        {
            "complete_for_observed_view"
        } else {
            "syncing"
        };
        list.push(serde_json::json!({
            "speaker_addr": spk.speaker_addr,
            "source": spk.source,
            "sys_name": spk.sys_name,
            "sys_descr": spk.sys_descr,
            "connected_at": spk.connected_at,
            "peer_count": spk.peer_count(),
            "up_peer_count": spk.up_peer_count(),
            "total_routes": spk.total_routes(),
            "completeness_state": completeness,
            "transport_gap": gap,
        }));
    }

    serde_json::json!({
        "api_version": ROUTING_API_VERSION,
        "durable_sequence": latest_seq,
        "receive_time": Utc::now().to_rfc3339(),
        "completeness_state": state.overall_completeness().await,
        "speakers": list,
    })
}

pub async fn build_peers_json(state: &RoutingServiceState, q: &PeersQuery) -> serde_json::Value {
    let rib = state.rib.read().await;
    let gaps = state.transport_gaps.read().await;
    let latest_seq = state.event_journal.latest_seq().await;
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);

    let mut peers = Vec::new();
    for spk in rib.speakers() {
        if let Some(ref spk_filter) = q.speaker {
            if spk.speaker_addr.to_string() != *spk_filter {
                continue;
            }
        }
        let has_transport_gap = gaps.contains_key(&spk.speaker_addr);
        for (peer_key, peer) in &spk.peers {
            if let Some(ref rd_filter) = q.vrf_rd {
                if peer_key.distinguisher_string() != *rd_filter {
                    continue;
                }
            }
            let state_str = match peer.state {
                PeerState::Up => "up",
                PeerState::Down => "down",
                PeerState::Unknown => "unknown",
            };
            if let Some(ref st_filter) = q.state {
                if state_str != st_filter.to_lowercase() {
                    continue;
                }
            }
            let completeness = if has_transport_gap {
                "gap"
            } else if peer.state == PeerState::Down {
                if peer.llgr_active { "stale" } else { "gap" }
            } else if !peer.eor_families.is_empty() {
                "complete_for_observed_view"
            } else {
                "syncing"
            };
            peers.push(serde_json::json!({
                "speaker_addr": spk.speaker_addr,
                "peer_key": peer_key,
                "peer_type": peer.peer_type,
                "peer_distinguisher": peer_key.distinguisher_string(),
                "peer_address": peer.peer_address,
                "peer_as": peer.peer_as,
                "peer_bgp_id": peer.peer_bgp_id,
                "local_as": peer.local_as,
                "hold_time": peer.hold_time,
                "state": state_str,
                "up_at": peer.up_at,
                "down_at": peer.down_at,
                "flap_count": peer.flap_count,
                "llgr_active": peer.llgr_active,
                "route_counts": peer.route_counts,
                "eor_families": peer.eor_families,
                "completeness_state": completeness,
            }));
            if peers.len() >= limit {
                break;
            }
        }
    }

    serde_json::json!({
        "api_version": ROUTING_API_VERSION,
        "durable_sequence": latest_seq,
        "receive_time": Utc::now().to_rfc3339(),
        "completeness_state": state.overall_completeness().await,
        "peers": peers,
    })
}

pub async fn build_routes_json(
    state: &RoutingServiceState,
    q: &RoutesQuery,
) -> (StatusCode, serde_json::Value) {
    let latest_seq = state.event_journal.latest_seq().await;

    // Check explicit family filter against FamilySupportStatus
    if let Some(ref fam) = q.family {
        let fam_lower = fam.trim().to_lowercase();
        let afi_safi = match fam_lower.as_str() {
            "ipv4-unicast" | "ipv4" | "1/1" => AfiSafi::ipv4_unicast(),
            "ipv6-unicast" | "ipv6" | "2/1" => AfiSafi::ipv6_unicast(),
            "ipv4-vpn" | "vpnv4" | "1/128" => AfiSafi::ipv4_vpn(),
            "ipv6-vpn" | "vpnv6" | "2/128" => AfiSafi::ipv6_vpn(),
            "l2vpn-evpn" | "evpn" | "25/70" => AfiSafi::evpn(),
            "bgp-ls" | "bgpls" | "16388/71" => AfiSafi::new(16388, 71),
            _ => AfiSafi::new(65535, 255),
        };
        let support = afi_safi.support_status();
        if support != FamilySupportStatus::Supported {
            return (
                StatusCode::OK,
                serde_json::json!({
                    "api_version": ROUTING_API_VERSION,
                    "family": fam,
                    "support_status": support,
                    "completeness_state": "unsupported",
                    "durable_sequence": latest_seq,
                    "receive_time": Utc::now().to_rfc3339(),
                    "routes": [],
                    "message": format!("Address family '{}' has support status {:?} and is not projected as a complete RIB view.", fam, support),
                }),
            );
        }
    }

    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.cursor.unwrap_or(0);
    let rib = state.rib.read().await;
    let gaps = state.transport_gaps.read().await;

    let mut all_matching = Vec::new();
    for spk in rib.speakers() {
        if let Some(ref spk_filter) = q.speaker {
            if spk.speaker_addr.to_string() != *spk_filter {
                continue;
            }
        }
        let spk_gap = gaps.contains_key(&spk.speaker_addr);
        for (peer_key, table) in rib.ribs_for_speaker(spk.speaker_addr) {
            if let Some(ref peer_filter) = q.peer {
                if peer_key.peer_address.to_string() != *peer_filter {
                    continue;
                }
            }
            if let Some(ref rd_filter) = q.vrf_rd {
                if peer_key.distinguisher_string() != *rd_filter {
                    continue;
                }
            }
            let peer_sess = spk.peers.get(peer_key);
            for rib_type in [
                RibType::AdjRibInPrePolicy,
                RibType::AdjRibInPostPolicy,
                RibType::LocRib,
                RibType::LocRibFiltered,
                RibType::AdjRibOutPrePolicy,
                RibType::AdjRibOutPostPolicy,
            ] {
                let view_str = format!("{:?}", rib_type);
                if let Some(ref view_filter) = q.view {
                    if !view_str.eq_ignore_ascii_case(view_filter) {
                        continue;
                    }
                }
                for entry in table.iter_rib(rib_type) {
                    let pfx_str = entry.prefix.to_string();
                    if let Some(ref pfx_filter) = q.prefix {
                        if pfx_str != *pfx_filter {
                            continue;
                        }
                    }
                    let afi_safi = entry.prefix.afi_safi();
                    let eor_done = peer_sess
                        .map(|ps| ps.has_eor(rib_type, afi_safi))
                        .unwrap_or(false);
                    let completeness = if spk_gap {
                        "gap"
                    } else if entry.llgr_state != rbmp_core::bgp::types::LlgrState::Normal {
                        "stale"
                    } else if eor_done {
                        "complete_for_observed_view"
                    } else {
                        "syncing"
                    };

                    all_matching.push(serde_json::json!({
                        "source": spk.source,
                        "speaker_addr": spk.speaker_addr,
                        "peer_key": peer_key,
                        "peer_addr": entry.peer_addr,
                        "peer_as": entry.peer_as,
                        "peer_distinguisher": peer_key.distinguisher_string(),
                        "view": view_str,
                        "prefix": pfx_str,
                        "afi_safi": afi_safi.to_string(),
                        "path_id": entry.path_id,
                        "is_best": entry.is_best,
                        "llgr_state": entry.llgr_state,
                        "observation_time": entry.received_at,
                        "completeness_state": completeness,
                        "as_path": entry.attributes.as_path.as_ref().map(|p| p.to_string()),
                        "next_hop": entry.attributes.next_hop,
                        "local_pref": entry.attributes.local_pref,
                        "med": entry.attributes.multi_exit_disc,
                        "origin": entry.attributes.origin.map(|o| o.to_string()),
                        "communities": entry.attributes.communities.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
                    }));
                }
            }
        }
    }

    let total_matching = all_matching.len();
    let page: Vec<_> = all_matching.into_iter().skip(offset).take(limit).collect();
    let next_cursor = if offset + page.len() < total_matching {
        Some(offset + page.len())
    } else {
        None
    };

    (
        StatusCode::OK,
        serde_json::json!({
            "api_version": ROUTING_API_VERSION,
            "support_status": "supported",
            "completeness_state": state.overall_completeness().await,
            "durable_sequence": latest_seq,
            "receive_time": Utc::now().to_rfc3339(),
            "total_matching": total_matching,
            "next_cursor": next_cursor,
            "routes": page,
        }),
    )
}

pub async fn build_prefix_history_json(
    state: &RoutingServiceState,
    q: &PrefixHistoryQuery,
) -> (StatusCode, serde_json::Value) {
    let Some(ref prefix) = q.prefix else {
        return (
            StatusCode::BAD_REQUEST,
            serde_json::json!({
                "error": "missing_prefix_parameter",
                "message": "Query parameter 'prefix' is required for /prefix-history"
            }),
        );
    };
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let history: Vec<PrefixHistoryItem> = state
        .event_journal
        .prefix_history_filtered(prefix, q.speaker.as_deref(), q.vrf_rd.as_deref(), limit)
        .await;
    let latest_seq = state.event_journal.latest_seq().await;
    let completeness = state.overall_completeness().await;

    (
        StatusCode::OK,
        serde_json::json!({
            "api_version": ROUTING_API_VERSION,
            "prefix": prefix,
            "speaker": q.speaker,
            "vrf_rd": q.vrf_rd,
            "support_status": "supported",
            "completeness_state": completeness,
            "durable_sequence": latest_seq,
            "receive_time": Utc::now().to_rfc3339(),
            "events": history,
        }),
    )
}

pub async fn build_coverage_json(state: &RoutingServiceState) -> serde_json::Value {
    let families = [
        ("ipv4-unicast", AfiSafi::ipv4_unicast()),
        ("ipv6-unicast", AfiSafi::ipv6_unicast()),
        ("ipv4-vpn", AfiSafi::ipv4_vpn()),
        ("ipv6-vpn", AfiSafi::ipv6_vpn()),
        ("l2vpn-evpn", AfiSafi::evpn()),
        ("bgp-ls", AfiSafi::new(16388, 71)),
    ];
    let matrix: Vec<_> = families
        .iter()
        .map(|(name, af)| {
            serde_json::json!({
                "family": name,
                "afi": af.afi.as_u16(),
                "safi": af.safi.as_u8(),
                "support_status": af.support_status(),
            })
        })
        .collect();

    serde_json::json!({
        "api_version": ROUTING_API_VERSION,
        "durable_sequence": state.event_journal.latest_seq().await,
        "completeness_state": state.overall_completeness().await,
        "receive_time": Utc::now().to_rfc3339(),
        "families": matrix,
    })
}

pub async fn build_events_json(state: &RoutingServiceState, q: &EventsQuery) -> serde_json::Value {
    let after_seq = q.after_seq.unwrap_or(0);
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let fetch_cap = if q.speaker.is_some() || q.vrf_rd.is_some() {
        10_000
    } else {
        limit
    };
    let raw_events = state
        .event_journal
        .events_after_seq(after_seq, fetch_cap)
        .await;
    let events: Vec<_> = raw_events
        .into_iter()
        .filter(|ev| {
            if let Some(ref spk) = q.speaker {
                if ev.speaker.to_string() != *spk {
                    return false;
                }
            }
            if let Some(ref rd) = q.vrf_rd {
                let ev_rd = match &ev.payload {
                    RibEventPayload::RouteChange(rc) => Some(rc.peer_key.distinguisher_string()),
                    RibEventPayload::PeerUp { peer_header, .. }
                    | RibEventPayload::PeerDown { peer_header, .. }
                    | RibEventPayload::EndOfRib { peer_header, .. }
                    | RibEventPayload::Stats { peer_header, .. } => {
                        Some(PeerKey::from_header(ev.speaker, peer_header).distinguisher_string())
                    }
                    RibEventPayload::SpeakerUp { .. } | RibEventPayload::SpeakerDown { .. } => None,
                };
                if ev_rd.as_deref() != Some(rd.as_str()) {
                    return false;
                }
            }
            true
        })
        .take(limit)
        .collect();
    let latest_seq = state.event_journal.latest_seq().await;
    let next_after_seq = events
        .last()
        .map(|e| e.event_key.sequence)
        .unwrap_or(after_seq);

    serde_json::json!({
        "api_version": ROUTING_API_VERSION,
        "durable_sequence": latest_seq,
        "after_seq": after_seq,
        "next_after_seq": next_after_seq,
        "completeness_state": state.overall_completeness().await,
        "receive_time": Utc::now().to_rfc3339(),
        "events": events,
    })
}

// ── B13: Checkpointed Graph Summary Projector ────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpeakerSummaryProjection {
    pub speaker_addr: String,
    pub sys_name: Option<String>,
    pub sys_descr: Option<String>,
    pub session_epoch: u64,
    pub is_up: bool,
    pub last_event_sequence: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerSummaryProjection {
    pub peer_key_id: String,
    pub speaker_addr: String,
    pub peer_addr: String,
    pub peer_as: u32,
    pub peer_type: u8,
    pub peer_distinguisher: String,
    pub state: String,
    pub active_prefixes: HashSet<String>,
    pub announce_events_count: u64,
    pub withdraw_events_count: u64,
    pub eor_families: HashSet<String>,
    pub last_event_sequence: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingProjectionCheckpoint {
    pub last_applied_sequence: u64,
    pub last_event_id: String,
    pub updated_at_ns: i64,
    pub applied_event_count: u64,
    pub skipped_raw_route_churn_count: u64,
    pub speaker_summaries: HashMap<String, SpeakerSummaryProjection>,
    pub peer_summaries: HashMap<String, PeerSummaryProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectionStepReport {
    pub newly_applied_events: usize,
    pub journal_latest_sequence: u64,
    pub graph_checkpoint_sequence: u64,
    pub sequence_lag: u64,
    pub checkpoint: RoutingProjectionCheckpoint,
}

/// Load the durable routing projection checkpoint from `GraphStore`.
pub async fn load_projection_checkpoint(
    store: &crate::graph::GraphStore,
) -> RoutingProjectionCheckpoint {
    let items = store
        .list_config_items(Some(ROUTING_CHECKPOINT_CONFIG_CLASS.to_string()))
        .await
        .unwrap_or_default();
    if let Some(ci) = items
        .into_iter()
        .find(|i| i.id == ROUTING_CHECKPOINT_ID && i.enabled)
    {
        if let Ok(cp) = serde_json::from_str::<RoutingProjectionCheckpoint>(&ci.content_json) {
            return cp;
        }
    }
    RoutingProjectionCheckpoint::default()
}

/// Resolve the private routing service `(base_url, token, has_explicit_db_url)` from `GraphStore` (`app_config`) or environment (`R05`/`R06`).
pub async fn resolve_routing_service_endpoint_config(
    store: &crate::graph::GraphStore,
) -> (String, Option<String>, bool) {
    let items = store
        .list_config_items(Some("app_config".to_string()))
        .await
        .unwrap_or_default();
    let db_url = items
        .iter()
        .find(|i| i.name == "routing_service_url" && i.enabled)
        .map(|i| i.content_json.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty());
    let has_explicit_db_url = db_url.is_some();
    let url = db_url.unwrap_or_else(|| {
        std::env::var("BONSAI_ROUTING_SERVICE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8081".to_string())
    });
    let token = items
        .iter()
        .find(|i| i.name == "routing_service_token" && i.enabled)
        .map(|i| i.content_json.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("BONSAI_ROUTING_SERVICE_TOKEN")
                .ok()
                .filter(|s| !s.trim().is_empty())
        });
    (
        url.trim_end_matches('/').to_string(),
        token,
        has_explicit_db_url,
    )
}

/// Project new durable `RibEvent`s after `checkpoint.last_applied_sequence` into `GraphStore`.
///
/// Session/lifecycle events (`SpeakerUp`, `SpeakerDown`, `PeerUp`, `PeerDown`, `EndOfRib`)
/// update graph provenance and `BmpSession` state, while `RouteChange` events update bounded
/// per-peer route counts/summaries without duplicating raw route churn into the `BgpRoute` graph table.
pub async fn project_routing_events_to_graph(
    event_journal: &RibEventJournal,
    store: &crate::graph::GraphStore,
    batch_limit: usize,
) -> anyhow::Result<ProjectionStepReport> {
    let cp = load_projection_checkpoint(store).await;
    let pending = event_journal
        .events_after_seq(cp.last_applied_sequence, batch_limit.max(1))
        .await;
    let latest_seq = event_journal.latest_seq().await;
    apply_routing_events_to_graph(cp, pending, latest_seq, store).await
}

/// Project new durable `RibEvent`s from either a configured remote `bonsai-routing` HTTP service
/// (`/internal/routing/v1/events`) or an in-process `RoutingServiceState` into `GraphStore` (`R05`).
pub async fn project_remote_routing_events_to_graph(
    store: &crate::graph::GraphStore,
    batch_limit: usize,
) -> anyhow::Result<ProjectionStepReport> {
    let (base_url, token, has_explicit_db_url) =
        resolve_routing_service_endpoint_config(store).await;
    if !has_explicit_db_url {
        if let Some(in_proc) = default_routing_service() {
            return project_routing_events_to_graph(&in_proc.event_journal, store, batch_limit)
                .await;
        }
    }

    let cp = load_projection_checkpoint(store).await;
    let limit = batch_limit.clamp(1, 1000);
    let url = format!(
        "{base_url}/internal/routing/v1/events?after_seq={}&limit={limit}",
        cp.last_applied_sequence
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()?;
    let mut req = client.get(&url);
    if let Some(t) = token {
        req = req.header("x-bonsai-routing-token", t);
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "remote routing events endpoint returned HTTP {}",
            resp.status()
        );
    }
    let body: serde_json::Value = resp.json().await?;
    let latest_seq = body
        .get("durable_sequence")
        .and_then(|v| v.as_u64())
        .unwrap_or(cp.last_applied_sequence);
    let pending: Vec<rbmp_rib::event::RibEvent> = body
        .get("events")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();

    apply_routing_events_to_graph(cp, pending, latest_seq, store).await
}

async fn apply_routing_events_to_graph(
    mut cp: RoutingProjectionCheckpoint,
    pending: Vec<rbmp_rib::event::RibEvent>,
    latest_seq: u64,
    store: &crate::graph::GraphStore,
) -> anyhow::Result<ProjectionStepReport> {
    let mut newly_applied = 0usize;

    for ev in pending {
        if ev.event_key.sequence <= cp.last_applied_sequence {
            continue;
        }
        let seq = ev.event_key.sequence;
        let spk_str = ev.speaker.to_string();

        match &ev.payload {
            RibEventPayload::SpeakerUp {
                sys_name,
                sys_descr,
            } => {
                let entry = cp.speaker_summaries.entry(spk_str.clone()).or_default();
                entry.speaker_addr = spk_str;
                entry.sys_name = sys_name.clone();
                entry.sys_descr = sys_descr.clone();
                entry.session_epoch = ev.event_key.source.session_epoch;
                entry.is_up = true;
                entry.last_event_sequence = seq;
            }
            RibEventPayload::SpeakerDown { .. } => {
                let entry = cp.speaker_summaries.entry(spk_str.clone()).or_default();
                entry.speaker_addr = spk_str;
                entry.session_epoch = ev.event_key.source.session_epoch;
                entry.is_up = false;
                entry.last_event_sequence = seq;
            }
            RibEventPayload::PeerUp {
                peer_header,
                local_asn,
                remote_asn,
                ..
            } => {
                let peer_key = PeerKey::from_header(ev.speaker, peer_header);
                let pk_id = format!(
                    "{}|{}|{}|{}",
                    ev.speaker,
                    peer_header.peer_type as u8,
                    peer_key.distinguisher_string(),
                    peer_header.peer_address
                );
                let ps = cp.peer_summaries.entry(pk_id.clone()).or_default();
                ps.peer_key_id = pk_id;
                ps.speaker_addr = spk_str.clone();
                ps.peer_addr = peer_header.peer_address.to_string();
                ps.peer_as = *remote_asn;
                ps.peer_type = peer_header.peer_type as u8;
                ps.peer_distinguisher = peer_key.distinguisher_string();
                ps.state = "UP".to_string();
                ps.last_event_sequence = seq;

                // Project session transition into GraphStore without raw route churn
                let ts_ns = ev.occurred_at.timestamp_nanos_opt().unwrap_or(0);
                let rd_u64 = u64::from_be_bytes(peer_header.peer_distinguisher);
                let bmp_ev = crate::streaming::bmp::BmpEvent {
                    timestamp_ns: ts_ns,
                    collector_peer: format!("{}:11019", spk_str),
                    message_type: "peer_up".to_string(),
                    peer_type: peer_header.peer_type as u8,
                    peer_flags: peer_header.peer_flags.0,
                    router_distinguisher: rd_u64,
                    router_address: spk_str.clone(),
                    peer_address: ps.peer_addr.clone(),
                    peer_as: *remote_asn,
                    peer_bgp_id: peer_header.peer_bgp_id.to_string(),
                    session_state: "Established".to_string(),
                    route_entries: Vec::new(),
                    raw_len: 48,
                    peer_down_reason: None,
                    peer_down_reason_name: None,
                    stats: Vec::new(),
                    sys_name: None,
                    sys_descr: None,
                    termination_reason: None,
                    termination_reason_name: None,
                    rib_type: Some(format!("{:?}", peer_header.rib_type)),
                    peer_up_info: None,
                    init_admin_string: None,
                    termination_message: None,
                };
                let _ = store
                    .write(crate::telemetry::TelemetryUpdate {
                        timestamp_ns: ts_ns,
                        target: spk_str,
                        vendor: "unknown".to_string(),
                        hostname: String::new(),
                        role: "router".to_string(),
                        site: String::new(),
                        path: "streaming/bmp/peer-up".to_string(),
                        value: serde_json::to_value(&bmp_ev).unwrap_or_default(),
                    })
                    .await;
                let _ = local_asn;
            }
            RibEventPayload::PeerDown {
                peer_header,
                reason,
            } => {
                let peer_key = PeerKey::from_header(ev.speaker, peer_header);
                let pk_id = format!(
                    "{}|{}|{}|{}",
                    ev.speaker,
                    peer_header.peer_type as u8,
                    peer_key.distinguisher_string(),
                    peer_header.peer_address
                );
                let ps = cp.peer_summaries.entry(pk_id.clone()).or_default();
                ps.peer_key_id = pk_id;
                ps.speaker_addr = spk_str.clone();
                ps.peer_addr = peer_header.peer_address.to_string();
                ps.peer_as = peer_header.peer_as;
                ps.peer_type = peer_header.peer_type as u8;
                ps.peer_distinguisher = peer_key.distinguisher_string();
                ps.state = "DOWN".to_string();
                ps.active_prefixes.clear();
                ps.eor_families.clear();
                ps.last_event_sequence = seq;

                let ts_ns = ev.occurred_at.timestamp_nanos_opt().unwrap_or(0);
                let rd_u64 = u64::from_be_bytes(peer_header.peer_distinguisher);
                let bmp_ev = crate::streaming::bmp::BmpEvent {
                    timestamp_ns: ts_ns,
                    collector_peer: format!("{}:11019", spk_str),
                    message_type: "peer_down".to_string(),
                    peer_type: peer_header.peer_type as u8,
                    peer_flags: peer_header.peer_flags.0,
                    router_distinguisher: rd_u64,
                    router_address: spk_str.clone(),
                    peer_address: ps.peer_addr.clone(),
                    peer_as: peer_header.peer_as,
                    peer_bgp_id: peer_header.peer_bgp_id.to_string(),
                    session_state: "Down".to_string(),
                    route_entries: Vec::new(),
                    raw_len: 48,
                    peer_down_reason: Some(1),
                    peer_down_reason_name: Some(reason.clone()),
                    stats: Vec::new(),
                    sys_name: None,
                    sys_descr: None,
                    termination_reason: None,
                    termination_reason_name: None,
                    rib_type: Some(format!("{:?}", peer_header.rib_type)),
                    peer_up_info: None,
                    init_admin_string: None,
                    termination_message: None,
                };
                let _ = store
                    .write(crate::telemetry::TelemetryUpdate {
                        timestamp_ns: ts_ns,
                        target: spk_str,
                        vendor: "unknown".to_string(),
                        hostname: String::new(),
                        role: "router".to_string(),
                        site: String::new(),
                        path: "streaming/bmp/peer-down".to_string(),
                        value: serde_json::to_value(&bmp_ev).unwrap_or_default(),
                    })
                    .await;
            }
            RibEventPayload::EndOfRib {
                peer_header,
                afi_safi,
            } => {
                let peer_key = PeerKey::from_header(ev.speaker, peer_header);
                let pk_id = format!(
                    "{}|{}|{}|{}",
                    ev.speaker,
                    peer_header.peer_type as u8,
                    peer_key.distinguisher_string(),
                    peer_header.peer_address
                );
                let ps = cp.peer_summaries.entry(pk_id.clone()).or_default();
                ps.peer_key_id = pk_id;
                ps.speaker_addr = spk_str;
                ps.peer_addr = peer_header.peer_address.to_string();
                ps.peer_as = peer_header.peer_as;
                ps.peer_type = peer_header.peer_type as u8;
                ps.peer_distinguisher = peer_key.distinguisher_string();
                ps.eor_families.insert(afi_safi.clone());
                ps.last_event_sequence = seq;
            }
            RibEventPayload::RouteChange(rc) => {
                let pk_id = format!(
                    "{}|{}|{}|{}",
                    ev.speaker,
                    rc.peer_header.peer_type as u8,
                    rc.peer_key.distinguisher_string(),
                    rc.peer_header.peer_address
                );
                let ps = cp.peer_summaries.entry(pk_id.clone()).or_default();
                ps.peer_key_id = pk_id;
                ps.speaker_addr = spk_str;
                ps.peer_addr = rc.peer_header.peer_address.to_string();
                ps.peer_as = rc.peer_header.peer_as;
                ps.peer_type = rc.peer_header.peer_type as u8;
                ps.peer_distinguisher = rc.peer_key.distinguisher_string();
                let route_id = format!("{:?}|{}|{:?}", rc.rib_type, rc.prefix, rc.path_id);
                match rc.action {
                    RouteAction::Announce => {
                        ps.active_prefixes.insert(route_id);
                        ps.announce_events_count += 1;
                    }
                    RouteAction::Withdraw => {
                        ps.active_prefixes.remove(&route_id);
                        ps.withdraw_events_count += 1;
                    }
                }
                ps.last_event_sequence = seq;
                // Intentionally do NOT write individual BgpRoute rows into GraphStore!
                cp.skipped_raw_route_churn_count += 1;
            }
            RibEventPayload::Stats { .. } => {}
        }

        cp.last_applied_sequence = seq;
        cp.last_event_id = ev.id.to_string();
        cp.applied_event_count += 1;
        cp.updated_at_ns = Utc::now().timestamp_nanos_opt().unwrap_or(0);
        newly_applied += 1;
    }

    if newly_applied > 0 {
        let record = crate::graph::ConfigItemRecord {
            id: ROUTING_CHECKPOINT_ID.to_string(),
            config_class: ROUTING_CHECKPOINT_CONFIG_CLASS.to_string(),
            vendor: String::new(),
            name: "routing_projection_checkpoint".to_string(),
            version: ROUTING_API_VERSION.to_string(),
            content_json: serde_json::to_string(&cp)?,
            enabled: true,
            created_by: "routing_projector".to_string(),
        };
        store.upsert_config_item(record).await?;
    }

    let lag = latest_seq.saturating_sub(cp.last_applied_sequence);

    Ok(ProjectionStepReport {
        newly_applied_events: newly_applied,
        journal_latest_sequence: latest_seq,
        graph_checkpoint_sequence: cp.last_applied_sequence,
        sequence_lag: lag,
        checkpoint: cp,
    })
}

/// Helper to build a synthetic BMP Initiation + PeerUp + RouteMonitoring sequence for testing.
pub fn sample_bmp_session_frames(
    peer_ip: Ipv4Addr,
    peer_as: u32,
    prefix_octets: [u8; 3],
    local_pref: u32,
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    // 1. Initiation: sysDescr="FRR 9.1", sysName="edge-a"
    let init = b"\x03\x00\x00\x00\x1b\x04\x00\x01\x00\x07FRR 9.1\x00\x02\x00\x06edge-a".to_vec();

    // 2. PeerUp (type 3): 6-byte BMP + 42-byte PeerHeader + 20-byte local addr/ports + 29-byte sent OPEN + 29-byte recv OPEN = 126 bytes
    let mut peer_hdr = Vec::with_capacity(42);
    peer_hdr.push(0); // GlobalInstance
    peer_hdr.push(0); // flags = 0 (IPv4, 4-byte ASN)
    peer_hdr.extend_from_slice(&[0u8; 8]); // RD = 0
    peer_hdr.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    peer_hdr.extend_from_slice(&peer_ip.octets());
    peer_hdr.extend_from_slice(&peer_as.to_be_bytes());
    peer_hdr.extend_from_slice(&peer_ip.octets()); // BGP ID
    peer_hdr.extend_from_slice(&1700000000u32.to_be_bytes());
    peer_hdr.extend_from_slice(&0u32.to_be_bytes());

    let mut open = Vec::with_capacity(29);
    open.extend_from_slice(&[0xff; 16]);
    open.extend_from_slice(&29u16.to_be_bytes());
    open.push(1); // OPEN
    open.push(4); // version 4
    open.extend_from_slice(&(peer_as as u16).to_be_bytes());
    open.extend_from_slice(&90u16.to_be_bytes());
    open.extend_from_slice(&peer_ip.octets());
    open.push(0); // opt_params_len = 0

    let mut peer_up = Vec::with_capacity(126);
    peer_up.push(3);
    peer_up.extend_from_slice(&126u32.to_be_bytes());
    peer_up.push(3); // PeerUp
    peer_up.extend_from_slice(&peer_hdr);
    peer_up.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 0, 2, 254]); // local_addr
    peer_up.extend_from_slice(&179u16.to_be_bytes());
    peer_up.extend_from_slice(&50000u16.to_be_bytes());
    peer_up.extend_from_slice(&open);
    peer_up.extend_from_slice(&open);

    // 3. RouteMonitoring (type 0) announcing prefix_octets/24
    let mut attrs = Vec::new();
    attrs.extend_from_slice(&[0x40, 1, 1, 0]); // ORIGIN = IGP
    attrs.extend_from_slice(&[0x40, 2, 6, 2, 1]); // AS_PATH Sequence(1)
    attrs.extend_from_slice(&peer_as.to_be_bytes());
    attrs.extend_from_slice(&[0x40, 3, 4]); // NEXT_HOP
    attrs.extend_from_slice(&peer_ip.octets());
    attrs.extend_from_slice(&[0x40, 5, 4]); // LOCAL_PREF
    attrs.extend_from_slice(&local_pref.to_be_bytes());

    let nlri = [24, prefix_octets[0], prefix_octets[1], prefix_octets[2]];
    let mut bgp_update = Vec::new();
    bgp_update.extend_from_slice(&[0xff; 16]);
    let bgp_len = (19 + 2 + 2 + attrs.len() + nlri.len()) as u16;
    bgp_update.extend_from_slice(&bgp_len.to_be_bytes());
    bgp_update.push(2); // UPDATE
    bgp_update.extend_from_slice(&0u16.to_be_bytes());
    bgp_update.extend_from_slice(&(attrs.len() as u16).to_be_bytes());
    bgp_update.extend_from_slice(&attrs);
    bgp_update.extend_from_slice(&nlri);

    let rm_len = (6 + 42 + bgp_update.len()) as u32;
    let mut rm = Vec::with_capacity(rm_len as usize);
    rm.push(3);
    rm.extend_from_slice(&rm_len.to_be_bytes());
    rm.push(0); // RouteMonitoring
    rm.extend_from_slice(&peer_hdr);
    rm.extend_from_slice(&bgp_update);

    (init, peer_up, rm)
}

pub mod diagnostics;
pub use diagnostics::*;
pub mod migration;
pub mod scenario_pack;
pub use scenario_pack::*;

/// Build a canonical 6-byte BMP RouteMonitoring (type 0) withdrawal frame for `prefix_octets/24`.
pub fn sample_bmp_withdraw_frame(
    peer_ip: Ipv4Addr,
    peer_as: u32,
    prefix_octets: [u8; 3],
) -> Vec<u8> {
    let mut peer_hdr = Vec::with_capacity(42);
    peer_hdr.push(0); // GlobalInstance
    peer_hdr.push(0); // flags = 0 (IPv4, 4-byte ASN)
    peer_hdr.extend_from_slice(&[0u8; 8]);
    peer_hdr.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    peer_hdr.extend_from_slice(&peer_ip.octets());
    peer_hdr.extend_from_slice(&peer_as.to_be_bytes());
    peer_hdr.extend_from_slice(&peer_ip.octets());
    peer_hdr.extend_from_slice(&1700000010u32.to_be_bytes());
    peer_hdr.extend_from_slice(&0u32.to_be_bytes());

    let withdrawn = [24, prefix_octets[0], prefix_octets[1], prefix_octets[2]];
    let mut bgp_update = Vec::new();
    bgp_update.extend_from_slice(&[0xff; 16]);
    let bgp_len = (19 + 2 + withdrawn.len() + 2) as u16;
    bgp_update.extend_from_slice(&bgp_len.to_be_bytes());
    bgp_update.push(2); // UPDATE
    bgp_update.extend_from_slice(&(withdrawn.len() as u16).to_be_bytes());
    bgp_update.extend_from_slice(&withdrawn);
    bgp_update.extend_from_slice(&0u16.to_be_bytes()); // total_path_attr_len = 0

    let rm_len = (6 + 42 + bgp_update.len()) as u32;
    let mut rm = Vec::with_capacity(rm_len as usize);
    rm.push(3);
    rm.extend_from_slice(&rm_len.to_be_bytes());
    rm.push(0); // RouteMonitoring
    rm.extend_from_slice(&peer_hdr);
    rm.extend_from_slice(&bgp_update);
    rm
}

/// Compute a deterministic SHA-256 digest of all active routes in `RoutingServiceState`
/// sorted by `(speaker_addr, peer_address, peer_distinguisher, view, prefix, path_id, next_hop, local_pref, as_path)`.
pub async fn compute_rib_digest(state: &RoutingServiceState) -> (usize, String) {
    use sha2::{Digest, Sha256};
    let rib = state.rib.read().await;
    let mut canonical_rows = Vec::new();
    for spk in rib.speakers() {
        for (peer_key, table) in rib.ribs_for_speaker(spk.speaker_addr) {
            for rib_type in [
                RibType::AdjRibInPrePolicy,
                RibType::AdjRibInPostPolicy,
                RibType::LocRib,
                RibType::LocRibFiltered,
                RibType::AdjRibOutPrePolicy,
                RibType::AdjRibOutPostPolicy,
            ] {
                for entry in table.iter_rib(rib_type) {
                    canonical_rows.push(format!(
                        "{}|{}|{}|{:?}|{}|{:?}|{:?}|{:?}|{:?}",
                        spk.speaker_addr,
                        peer_key.peer_address,
                        peer_key.distinguisher_string(),
                        rib_type,
                        entry.prefix,
                        entry.path_id,
                        entry.attributes.next_hop,
                        entry.attributes.local_pref,
                        entry.attributes.as_path.as_ref().map(|p| p.to_string()),
                    ));
                }
            }
        }
    }
    canonical_rows.sort();
    let count = canonical_rows.len();
    let joined = canonical_rows.join("\n");
    let digest = hex::encode(Sha256::digest(joined.as_bytes()));
    (count, digest)
}
