//! BN-003 Live Routing & RIB Browser Acceptance Gate
//!
//! Orchestrates the complete end-to-end acceptance gate:
//! 1. Containerlab dedicated FRR topology (AS 65001 <-> AS 65002) with kernel-VRF
//!    BLUE provisioned pre-FRR-start (`frr-bmp-r07-vrf.clab.yml`, `configs-r07vrf/`)
//! 2. Genuine BMP exporters: frr1 (172.100.207.11) exports TWO concurrent sessions —
//!    global instance plus VRF instance with `rd vpn export 65000:100` — and frr2
//!    (172.100.207.12) exports the global instance. No synthetic BMP frames are injected.
//! 3. Global RD (0:0) and VRF RD (65000:100) isolation on genuine wire data
//!    (eBGP route 10.99.1.0/24 received by frr1 inside VRF BLUE)
//! 4. Real RoutingServiceState with durable BmpJournal and RibEventJournal
//! 5. Bonsai authenticated HTTP web server with ui/dist SPA, JWT cookies, and routing proxy
//! 6. Playwright automated headless Chromium browser testing all 11 user journeys in real DOM
//! 7. Collection and preservation of screenshots, vtysh logs, and test evidence in
//!    `docs/merger/evidence/2026-10-13-bn003/` (override with BONSAI_EVIDENCE_DIR)

use std::collections::HashMap;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::routing::post;
use bonsai::graph::{ConfigItemRecord, GraphStore};
use bonsai::http_server::build_public_routing_web_router;
use bonsai::routing_service::{
    RoutingServiceConfig, RoutingServiceState, build_internal_routing_router,
};
use chrono::Utc;
use rbmp_core::bmp::parser::DEFAULT_MAX_FRAME;
use rbmp_core::bmp::parser::read_bmp_frame;
use rbmp_core::bmp::types::{PeerType, RibType, SpeakerKey};
use rbmp_rib::session::PeerState;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};

const LAB_TOPO_PATH: &str = "lab/fast-iteration/frr-bmp-r07-vrf.clab.yml";
const FRR_IMAGE: &str = "quay.io/frrouting/frr:10.6.1";
const BMP_PORT: u16 = 11019;
const FRR1_IP: &str = "172.100.207.11";
const FRR2_IP: &str = "172.100.207.12";
const VRF_RD: &str = "65000:100";
const VRF_PREFIX: &str = "10.99.1.0/24";
const GLOBAL_PREFIX: &str = "10.10.1.0/24";
const DEFAULT_EVIDENCE_DIR: &str = "docs/merger/evidence/2026-10-13-bn003";

fn run_cmd(cmd: &str, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("Failed to run {cmd} with {args:?}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        anyhow::bail!(
            "Command {cmd} {:?} failed: status={}, stdout={stdout}, stderr={stderr}",
            args,
            out.status
        );
    }
    Ok(stdout)
}

fn vtysh_frr1(cmds: &[&str]) -> Result<String> {
    let mut args = vec!["exec", "clab-bonsai-bn003-r07vrf-frr1", "vtysh"];
    for c in cmds {
        args.push("-c");
        args.push(c);
    }
    run_cmd("docker", &args)
}

fn vtysh_frr2(cmds: &[&str]) -> Result<String> {
    let mut args = vec!["exec", "clab-bonsai-bn003-r07vrf-frr2", "vtysh"];
    for c in cmds {
        args.push("-c");
        args.push(c);
    }
    run_cmd("docker", &args)
}

fn check_prerequisites() -> Result<()> {
    let img_out = run_cmd("docker", &["image", "inspect", FRR_IMAGE])
        .context("FRR image not available on host")?;
    assert!(
        img_out.contains("Architecture"),
        "Docker inspect output invalid"
    );

    let clab_out = run_cmd("containerlab", &["version"]).context("Containerlab not available")?;
    assert!(
        clab_out.contains("version:"),
        "Containerlab version check failed"
    );

    let node_out = run_cmd("node", &["--version"]).context("Node.js not available")?;
    assert!(node_out.starts_with("v"), "Node version check failed");

    Ok(())
}

fn cleanup_lab() {
    let _ = run_cmd(
        "containerlab",
        &["destroy", "-t", LAB_TOPO_PATH, "--cleanup"],
    );
}

struct LabCleanupGuard;
impl Drop for LabCleanupGuard {
    fn drop(&mut self) {
        cleanup_lab();
    }
}

/// Tracks live BMP TCP connections per speaker IP so that concurrent sessions
/// (FRR exports one session per BGP instance: global + each VRF) share the speaker's
/// session epoch, mirroring `run_bmp_listener`. A fresh epoch is opened only when the
/// speaker's last connection closes, preserving stale-session fencing on reconnect.
#[derive(Default)]
struct BmpConnTracker {
    /// speaker_addr -> (session_epoch, live connection count)
    live: HashMap<IpAddr, (u64, usize)>,
    /// When armed by `/bmp/reconnect`, the next newly opened speaker epoch is recorded
    /// so the first ingested frame on that epoch fires `reconnect_epoch_established`.
    armed_reconnect: bool,
    reconnect_epoch: Option<u64>,
}

struct ControlContext {
    routing_state: Arc<tokio::sync::RwLock<Arc<RoutingServiceState>>>,
    conn_tracker: Arc<Mutex<BmpConnTracker>>,
    reconnect_epoch_established: Arc<tokio::sync::Notify>,
    bmp_journal_path: PathBuf,
    event_journal_path: PathBuf,
    internal_routing_port: u16,
    active_routing_server_abort: Mutex<Option<tokio::task::AbortHandle>>,
}

/// Returns true once every live BMP connection from `speaker` has drained.
async fn speaker_connections_drained(tracker: &Mutex<BmpConnTracker>, speaker: IpAddr) -> bool {
    let tr = tracker.lock().await;
    match tr.live.get(&speaker) {
        Some(&(_, count)) => count == 0,
        None => true,
    }
}

async fn handle_frr_modify() -> StatusCode {
    let res = vtysh_frr2(&[
        "conf t",
        "route-map RM_OUT permit 10",
        "set as-path prepend 65002 65002",
        "set metric 150",
        "exit",
        "router bgp 65002",
        "address-family ipv4 unicast",
        "neighbor 10.100.1.0 route-map RM_OUT out",
        "end",
        "clear bgp 10.100.1.0 soft out",
    ]);
    if res.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

async fn handle_frr_withdraw() -> StatusCode {
    let res = vtysh_frr2(&[
        "conf t",
        "router bgp 65002",
        "address-family ipv4 unicast",
        "no network 10.10.1.0/24",
        "end",
    ]);
    if res.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

async fn handle_frr_announce() -> StatusCode {
    let res = vtysh_frr2(&[
        "conf t",
        "router bgp 65002",
        "address-family ipv4 unicast",
        "network 10.10.1.0/24",
        "end",
    ]);
    if res.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

async fn handle_bmp_disconnect(AxumState(ctx): AxumState<Arc<ControlContext>>) -> StatusCode {
    // Genuine router-driven disconnect: disable BOTH frr1 BMP targets (global instance
    // and VRF BLUE instance) so FRR closes both TCP sessions
    let res = vtysh_frr1(&[
        "conf t",
        "router bgp 65001",
        "no bmp targets bonsai",
        "exit",
        "router bgp 65001 vrf BLUE",
        "no bmp targets bonsai-vrf",
        "end",
    ]);
    if res.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    // Wait until reader loops record transport gap from TCP EOF and every frr1 BMP
    // connection has drained, so the reconnect opens a fresh speaker epoch
    let frr1_ip: IpAddr = FRR1_IP.parse().unwrap();
    for _ in 0..60 {
        let state = ctx.routing_state.read().await.clone();
        let has_gap = !state.transport_gaps.read().await.is_empty();
        if has_gap && speaker_connections_drained(&ctx.conn_tracker, frr1_ip).await {
            return StatusCode::OK;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    StatusCode::OK
}

async fn handle_bmp_reconnect(AxumState(ctx): AxumState<Arc<ControlContext>>) -> StatusCode {
    // Arm reconnect notification before re-enabling targets so the next speaker epoch
    // opened by the accept loop is recognized as the reconnect epoch
    {
        let mut tr = ctx.conn_tracker.lock().await;
        tr.armed_reconnect = true;
    }
    // Genuine router-driven reconnect: re-enable BOTH frr1 BMP targets
    let res = vtysh_frr1(&[
        "conf t",
        "router bgp 65001",
        "bmp targets bonsai",
        "bmp stats interval 30000",
        "bmp monitor ipv4 unicast pre-policy",
        "bmp monitor ipv4 unicast post-policy",
        "bmp monitor ipv4 unicast loc-rib",
        "bmp connect 172.100.207.1 port 11019 min-retry 1000 max-retry 4000",
        "exit",
        "router bgp 65001 vrf BLUE",
        "bmp targets bonsai-vrf",
        "bmp stats interval 30000",
        "bmp monitor ipv4 unicast pre-policy",
        "bmp monitor ipv4 unicast post-policy",
        "bmp monitor ipv4 unicast loc-rib",
        "bmp connect 172.100.207.1 port 11019 min-retry 1000 max-retry 4000",
        "end",
    ]);
    if res.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    // Await reconnect epoch connection acceptance and initial frame ingestion
    match timeout(
        Duration::from_secs(20),
        ctx.reconnect_epoch_established.notified(),
    )
    .await
    {
        Ok(_) => StatusCode::OK,
        Err(_) => {
            eprintln!("Timeout waiting for reconnect epoch to establish");
            StatusCode::GATEWAY_TIMEOUT
        }
    }
}

async fn handle_routing_restart(AxumState(ctx): AxumState<Arc<ControlContext>>) -> StatusCode {
    let mut abort_guard = ctx.active_routing_server_abort.lock().await;
    if let Some(h) = abort_guard.take() {
        h.abort();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    let config = RoutingServiceConfig {
        bmp_listen_addr: format!("0.0.0.0:{BMP_PORT}"),
        http_listen_addr: format!("127.0.0.1:{}", ctx.internal_routing_port),
        bmp_journal_path: Some(ctx.bmp_journal_path.clone()),
        event_journal_path: Some(ctx.event_journal_path.clone()),
        max_frame_bytes: DEFAULT_MAX_FRAME,
        internal_service_token: Some("bn003-token".to_string()),
        identity_db_path: None,
        identity_scope: None,
        bmp_identity_mode: bonsai::routing_service::BmpIdentityMode::Verified,
        bmp_mtls: None,
    };

    let new_state = match RoutingServiceState::new(config).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to restart RoutingServiceState: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };
    *ctx.routing_state.write().await = Arc::clone(&new_state);

    let mut internal_listener = None;
    for _ in 0..20 {
        match TcpListener::bind(format!("127.0.0.1:{}", ctx.internal_routing_port)).await {
            Ok(l) => {
                internal_listener = Some(l);
                break;
            }
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    let internal_listener = match internal_listener {
        Some(l) => l,
        None => {
            eprintln!("Failed to bind internal listener on restart after retries");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    let new_router = build_internal_routing_router(new_state);
    let handle = tokio::spawn(async move {
        let _ = axum::serve(internal_listener, new_router).await;
    });
    *abort_guard = Some(handle.abort_handle());

    StatusCode::OK
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_bn003_live_routing_browser_acceptance() -> Result<()> {
    let evidence_dir =
        std::env::var("BONSAI_EVIDENCE_DIR").unwrap_or_else(|_| DEFAULT_EVIDENCE_DIR.to_string());
    println!("\n=======================================================");
    println!("=== BN-003 Live Routing & RIB Browser Acceptance Gate ===");
    println!("=======================================================\n");

    let _cleanup_guard = LabCleanupGuard;

    check_prerequisites()?;
    fs::create_dir_all(&evidence_dir)?;

    // 1. Clean lab and setup temp directories
    cleanup_lab();
    sleep(Duration::from_millis(500)).await;

    let temp_dir = TempDir::new().context("create tempdir")?;
    let bmp_journal_path = temp_dir.path().join("bmp.journal");
    let event_journal_path = temp_dir.path().join("events.jsonl");
    let graph_db_path = temp_dir.path().join("graph.db");

    let graph_store = Arc::new(
        GraphStore::open(graph_db_path.to_str().unwrap(), 128 * 1024 * 1024)
            .context("open graph store")?,
    );

    // 2. Setup initial RoutingServiceState on dynamic port
    let internal_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind internal listener")?;
    let internal_port = internal_listener.local_addr()?.port();

    let routing_config = RoutingServiceConfig {
        bmp_listen_addr: format!("0.0.0.0:{BMP_PORT}"),
        http_listen_addr: format!("127.0.0.1:{internal_port}"),
        bmp_journal_path: Some(bmp_journal_path.clone()),
        event_journal_path: Some(event_journal_path.clone()),
        max_frame_bytes: DEFAULT_MAX_FRAME,
        internal_service_token: Some("bn003-token".to_string()),
        identity_db_path: None,
        identity_scope: None,
        bmp_identity_mode: bonsai::routing_service::BmpIdentityMode::Verified,
        bmp_mtls: None,
    };

    let routing_state = RoutingServiceState::new(routing_config)
        .await
        .context("create RoutingServiceState")?;

    let internal_router = build_internal_routing_router(Arc::clone(&routing_state));
    let internal_server_handle = tokio::spawn(async move {
        let _ = axum::serve(internal_listener, internal_router).await;
    });

    // 3. Configure GraphStore app_config & scoped API keys
    graph_store
        .upsert_config_item(ConfigItemRecord {
            id: "app-cfg-routing-url".to_string(),
            config_class: "app_config".to_string(),
            vendor: String::new(),
            name: "routing_service_url".to_string(),
            version: "1".to_string(),
            content_json: format!("\"http://127.0.0.1:{internal_port}\""),
            enabled: true,
            created_by: "bn003-test".to_string(),
        })
        .await?;

    graph_store
        .upsert_config_item(ConfigItemRecord {
            id: "app-cfg-routing-token".to_string(),
            config_class: "app_config".to_string(),
            vendor: String::new(),
            name: "routing_service_token".to_string(),
            version: "1".to_string(),
            content_json: "\"bn003-token\"".to_string(),
            enabled: true,
            created_by: "bn003-test".to_string(),
        })
        .await?;

    graph_store
        .upsert_config_item(ConfigItemRecord {
            id: "app-cfg-require-auth".to_string(),
            config_class: "app_config".to_string(),
            vendor: String::new(),
            name: "require_auth".to_string(),
            version: "1".to_string(),
            content_json: "\"1\"".to_string(),
            enabled: true,
            created_by: "bn003-test".to_string(),
        })
        .await?;

    let scoped_key = "bsk_scoped_key_for_testing_12345";
    let key_hash = hex::encode(Sha256::digest(scoped_key.as_bytes()));
    graph_store
        .upsert_config_item(ConfigItemRecord {
            id: "apikey-scoped-spk1".to_string(),
            config_class: "api_key".to_string(),
            vendor: String::new(),
            name: "scoped-spk1".to_string(),
            version: "1".to_string(),
            content_json: serde_json::json!({
                "key_hash": key_hash,
                "scope": "routing:read:speaker=172.100.207.11,vrf_rd=0:0",
                "expires_at_ns": 0,
                "last_used_at_ns": 0,
            })
            .to_string(),
            enabled: true,
            created_by: "bn003-test".to_string(),
        })
        .await?;

    let admin_pass = "BonsaiAdmin123!@#$";
    unsafe {
        std::env::set_var("BONSAI_ADMIN_USER", "admin");
        std::env::set_var("BONSAI_ADMIN_PASS", admin_pass);
        std::env::set_var("BONSAI_REQUIRE_AUTH", "1");
        std::env::set_var("BONSAI_ROUTING_SERVICE_TOKEN", "bn003-token");
    }

    // 4. Start Bonsai web server serving ui/dist and routing proxy
    let bonsai_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind bonsai web listener")?;
    let bonsai_port = bonsai_listener.local_addr()?.port();
    let bonsai_url = format!("http://127.0.0.1:{bonsai_port}");

    let bonsai_web_router = build_public_routing_web_router(Arc::clone(&graph_store));
    let bonsai_server_handle = tokio::spawn(async move {
        let _ = axum::serve(bonsai_listener, bonsai_web_router).await;
    });

    // 5. Setup single BMP TCP listener on port 11019 shared across all sessions,
    //    plus the unified connection manager. frr1 exports TWO concurrent BMP
    //    sessions (global BGP instance + VRF BLUE instance); frr2 exports one.
    //    Concurrent connections from the same speaker IP share the speaker's
    //    session epoch (mirroring run_bmp_listener); a fresh epoch opens only
    //    after the speaker's last connection closes.
    let bmp_listener = Arc::new(
        TcpListener::bind(format!("0.0.0.0:{BMP_PORT}"))
            .await
            .context("bind TCP listener for BMP on port 11019")?,
    );

    let raw_bytes_log = Arc::new(Mutex::new(Vec::<u8>::new()));
    let conn_tracker = Arc::new(Mutex::new(BmpConnTracker::default()));
    let reconnect_epoch_established = Arc::new(tokio::sync::Notify::new());
    let shared_routing_state = Arc::new(tokio::sync::RwLock::new(Arc::clone(&routing_state)));

    {
        let bmp_listener = Arc::clone(&bmp_listener);
        let state_holder = Arc::clone(&shared_routing_state);
        let tracker = Arc::clone(&conn_tracker);
        let raw_log = Arc::clone(&raw_bytes_log);
        let epoch_established = Arc::clone(&reconnect_epoch_established);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, sock)) = bmp_listener.accept().await else {
                    break;
                };
                let speaker = sock.ip();
                println!("Accepted BMP connection from {speaker}");
                let epoch = {
                    let mut tr = tracker.lock().await;
                    match tr.live.get(&speaker) {
                        Some(&(ep, count)) if count > 0 => {
                            if let Some(entry) = tr.live.get_mut(&speaker) {
                                entry.1 += 1;
                            }
                            ep
                        }
                        _ => {
                            let ep = {
                                let cur_state = state_holder.read().await.clone();
                                let mut rib = cur_state.rib.write().await;
                                rib.open_speaker_session(speaker, "", None, None, Utc::now())
                            };
                            if tr.armed_reconnect {
                                tr.armed_reconnect = false;
                                tr.reconnect_epoch = Some(ep);
                            }
                            tr.live.insert(speaker, (ep, 1));
                            ep
                        }
                    }
                };
                let reader_state = Arc::clone(&state_holder);
                let reader_tracker = Arc::clone(&tracker);
                let reader_raw = Arc::clone(&raw_log);
                let reader_established = Arc::clone(&epoch_established);
                tokio::spawn(async move {
                    let mut first_frame = true;
                    let disconnect_reason = loop {
                        let cur_state = reader_state.read().await.clone();
                        match read_bmp_frame(
                            &mut stream,
                            DEFAULT_MAX_FRAME,
                            Duration::from_secs(60),
                            Duration::from_secs(30),
                        )
                        .await
                        {
                            Ok(Some(frame)) => {
                                reader_raw.lock().await.extend_from_slice(&frame);
                                let _ = cur_state
                                    .ingest_bmp_bytes(speaker, Some(epoch), &frame)
                                    .await;
                                if first_frame {
                                    first_frame = false;
                                    let is_reconnect_epoch = {
                                        let tr = reader_tracker.lock().await;
                                        tr.reconnect_epoch == Some(epoch)
                                    };
                                    if is_reconnect_epoch {
                                        {
                                            let mut tr = reader_tracker.lock().await;
                                            tr.reconnect_epoch = None;
                                        }
                                        reader_established.notify_waiters();
                                    }
                                }
                            }
                            Ok(None) => {
                                // Production collector EOF detection path: TCP EOF = gap,
                                // never synthetic BGP withdraw
                                break Some("bmp_tcp_eof".to_string());
                            }
                            Err(e) => {
                                break Some(format!("bmp_transport_error: {e}"));
                            }
                        }
                    };
                    // Release this connection's share of the speaker epoch before
                    // recording the disconnect so a fast reconnect opens a fresh epoch.
                    {
                        let mut tr = reader_tracker.lock().await;
                        if let Some(entry) = tr.live.get_mut(&speaker) {
                            entry.1 = entry.1.saturating_sub(1);
                            if entry.1 == 0 {
                                tr.live.remove(&speaker);
                            }
                        }
                    }
                    if let Some(reason) = disconnect_reason {
                        let cur_state = reader_state.read().await.clone();
                        cur_state
                            .record_transport_disconnect_for_epoch(speaker, Some(epoch), reason)
                            .await;
                    }
                });
            }
        });
    }

    // 6. Setup Control Server for Playwright interaction
    let ctrl_ctx = Arc::new(ControlContext {
        routing_state: Arc::clone(&shared_routing_state),
        conn_tracker: Arc::clone(&conn_tracker),
        reconnect_epoch_established: Arc::clone(&reconnect_epoch_established),
        bmp_journal_path: bmp_journal_path.clone(),
        event_journal_path: event_journal_path.clone(),
        internal_routing_port: internal_port,
        active_routing_server_abort: Mutex::new(Some(internal_server_handle.abort_handle())),
    });

    let ctrl_router = Router::new()
        .route("/frr/modify", post(handle_frr_modify))
        .route("/frr/withdraw", post(handle_frr_withdraw))
        .route("/frr/announce", post(handle_frr_announce))
        .route("/bmp/disconnect", post(handle_bmp_disconnect))
        .route("/bmp/reconnect", post(handle_bmp_reconnect))
        .route("/routing/restart", post(handle_routing_restart))
        .with_state(ctrl_ctx);

    let ctrl_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind control listener")?;
    let ctrl_port = ctrl_listener.local_addr()?.port();
    let ctrl_url = format!("http://127.0.0.1:{ctrl_port}");

    let ctrl_server_handle = tokio::spawn(async move {
        let _ = axum::serve(ctrl_listener, ctrl_router).await;
    });

    // 7. Deploy Containerlab topology
    println!("Deploying Containerlab topology: {LAB_TOPO_PATH}...");
    let deploy_out =
        run_cmd("containerlab", &["deploy", "-t", LAB_TOPO_PATH]).context("deploy containerlab")?;
    println!("Containerlab deployed successfully: {deploy_out}");

    // 8. Wait for genuine BMP sessions: frr1 global + frr1 VRF BLUE + frr2 global.
    //    The connection manager (spawned above) accepts every TCP session, shares
    //    the speaker epoch across frr1's concurrent connections, ingests frames,
    //    and records transport gaps on EOF.
    println!("Waiting for genuine BMP sessions from frr1 (global + VRF)...");
    let frr1_ip: IpAddr = FRR1_IP.parse().unwrap();
    let frr2_ip: IpAddr = FRR2_IP.parse().unwrap();
    let mut frr1_sessions_ready = false;
    for _ in 0..120 {
        let tr = conn_tracker.lock().await;
        let frr1_conns = tr.live.get(&frr1_ip).map(|e| e.1).unwrap_or(0);
        drop(tr);
        if frr1_conns >= 2 {
            frr1_sessions_ready = true;
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }
    assert!(
        frr1_sessions_ready,
        "Timed out waiting for frr1 genuine BMP sessions (global + VRF BLUE)"
    );
    println!("Genuine frr1 BMP sessions established: global instance + VRF BLUE.");

    // 9. Configure BMP on frr2 (172.100.207.12) to create second genuine exporter
    println!("Configuring BMP exporter on frr2 (172.100.207.12)...");
    let _ = vtysh_frr2(&[
        "conf t",
        "router bgp 65002",
        "bmp targets bonsai",
        "bmp stats interval 30000",
        "bmp monitor ipv4 unicast pre-policy",
        "bmp monitor ipv4 unicast post-policy",
        "bmp monitor ipv4 unicast loc-rib",
        "bmp connect 172.100.207.1 port 11019 min-retry 1000 max-retry 4000",
        "exit",
        "end",
    ]);

    // Await BMP connection from frr2 (up to 15s; the loop above already tolerates it)
    let mut frr2_seen = false;
    for _ in 0..30 {
        let tr = conn_tracker.lock().await;
        frr2_seen = tr.live.get(&frr2_ip).map(|e| e.1).unwrap_or(0) >= 1;
        drop(tr);
        if frr2_seen {
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }
    assert!(frr2_seen, "frr2 BMP session never connected");
    println!("Accepted BMP connection from frr2: {frr2_ip}");

    // 10. Wait for the GENUINE VRF BMP stream: frr1's VRF BLUE BGP instance exports
    //     peer 10.100.2.1 with RdInstance distinguisher 65000:100 and the eBGP-learned
    //     route 10.99.1.0/24. No synthetic frames are injected anywhere.
    println!("Waiting for genuine VRF RD-instance BMP stream ({VRF_RD}, {VRF_PREFIX})...");
    let mut vrf_stream_ready = false;
    for _ in 0..120 {
        let rib = routing_state.rib.read().await;
        let peer_ready = rib
            .speaker(SpeakerKey::new(frr1_ip, ""))
            .map(|s| {
                s.peers.iter().any(|(pk, p)| {
                    pk.distinguisher_string() == VRF_RD
                        && pk.peer_type == PeerType::RdInstance
                        && p.state == PeerState::Up
                })
            })
            .unwrap_or(false);
        let route_ready = rib
            .ribs_for_speaker(SpeakerKey::new(frr1_ip, ""))
            .any(|(pk, table)| {
                pk.distinguisher_string() == VRF_RD
                    && [
                        RibType::AdjRibInPrePolicy,
                        RibType::AdjRibInPostPolicy,
                        RibType::LocRib,
                    ]
                    .iter()
                    .any(|rt| {
                        table
                            .iter_rib(*rt)
                            .any(|e| e.prefix.to_string() == VRF_PREFIX)
                    })
            });
        drop(rib);
        if peer_ready && route_ready {
            vrf_stream_ready = true;
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }
    assert!(
        vrf_stream_ready,
        "Genuine VRF BMP stream did not populate RdInstance {VRF_RD} peer + {VRF_PREFIX} in RIB"
    );
    println!("Genuine VRF stream verified: RdInstance {VRF_RD} peer up, {VRF_PREFIX} in RIB.");

    // 11. Wait for BGP sessions to establish (global on eth1, VRF BLUE on eth2)
    println!("Waiting for BGP neighbor sessions between frr1 and frr2...");
    let mut bgp_up = false;
    let mut vrf_bgp_up = false;
    for _ in 0..60 {
        if let Ok(out) = vtysh_frr1(&["show ip bgp summary"]) {
            if out.contains("frr2(10.100.1.1)")
                && !out.contains("Active")
                && !out.contains("Connect")
                && !out.contains("Idle")
            {
                bgp_up = true;
            }
        }
        if let Ok(out) = vtysh_frr1(&["show bgp vrf BLUE ipv4 unicast summary"]) {
            if out.contains("10.100.2.1")
                && !out.contains("Active")
                && !out.contains("Connect")
                && !out.contains("Idle")
            {
                vrf_bgp_up = true;
            }
        }
        if bgp_up && vrf_bgp_up {
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }
    assert!(
        bgp_up,
        "BGP session between frr1 and frr2 failed to establish"
    );
    assert!(
        vrf_bgp_up,
        "VRF BLUE BGP session between frr1 and frr2 failed to establish"
    );

    // 12. Announce initial route 10.10.1.0/24 from frr2 (global instance)
    println!("Announcing {GLOBAL_PREFIX} from frr2...");
    vtysh_frr2(&[
        "conf t",
        "ip route 10.10.1.0/24 Null0",
        "router bgp 65002",
        "address-family ipv4 unicast",
        "network 10.10.1.0/24",
        "end",
    ])?;

    // Wait until the global route appears in RIB (VRF route may already be present)
    let mut announced = false;
    for _ in 0..60 {
        let rib = routing_state.rib.read().await;
        let found = rib
            .ribs_for_speaker(SpeakerKey::new(frr1_ip, ""))
            .any(|(pk, table)| {
                pk.distinguisher_string() == "0:0"
                    && table
                        .iter_rib(RibType::LocRib)
                        .any(|e| e.prefix.to_string() == GLOBAL_PREFIX)
            });
        drop(rib);
        if found {
            announced = true;
            break;
        }
        sleep(Duration::from_millis(300)).await;
    }
    assert!(announced, "Route 10.10.1.0/24 not received in RIB");
    println!("Route {GLOBAL_PREFIX} active in RIB.");

    // 13. Run Playwright Browser Acceptance Test Suite
    println!("\n--- Launching Playwright Chromium Acceptance Suite ---");
    println!("Bonsai URL:   {bonsai_url}");
    println!("Control URL:  {ctrl_url}");
    println!("Evidence Dir: {evidence_dir}\n");

    let chrome_bin = std::env::var("PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH")
        .ok()
        .or_else(|| {
            let user_cache = format!(
                "{}/.cache/ms-playwright/chromium-1228/chrome-linux64/chrome",
                std::env::var("HOME").unwrap_or_else(|_| "/home/arjuna".to_string())
            );
            if Path::new(&user_cache).exists() {
                Some(user_cache)
            } else if Path::new("/usr/bin/google-chrome").exists() {
                Some("/usr/bin/google-chrome".to_string())
            } else if Path::new("/usr/bin/chromium").exists() {
                Some("/usr/bin/chromium".to_string())
            } else {
                None
            }
        });

    let mut pw_cmd = tokio::process::Command::new("npx");
    pw_cmd
        .args(&[
            "--prefix",
            "tests/ui_driver",
            "playwright",
            "test",
            "--config",
            "tests/ui_driver/playwright.config.js",
            "tests/ui_driver/routing_live_acceptance.spec.js",
            "--reporter=list",
        ])
        .current_dir(Path::new("."))
        .env("BONSAI_URL", &bonsai_url)
        .env("BONSAI_CTRL_URL", &ctrl_url)
        .env("BONSAI_SCOPED_KEY", scoped_key)
        .env("BONSAI_ADMIN_PASS", admin_pass)
        .env("BONSAI_EVIDENCE_DIR", &evidence_dir);
    if let Some(ref bin) = chrome_bin {
        pw_cmd.env("PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH", bin);
    }
    let pw_status = pw_cmd
        .status()
        .await
        .context("execute Playwright test runner")?;

    println!("\nPlaywright test execution finished with status: {pw_status}");
    assert!(
        pw_status.success(),
        "Playwright browser acceptance suite failed!"
    );

    // 14. Save router vtysh state and evidence manifest
    println!("\nSaving router vtysh states and evidence bundle to {evidence_dir}...");
    let bgp_summary = vtysh_frr1(&["show ip bgp summary"]).unwrap_or_default();
    let bgp_routes = vtysh_frr1(&["show ip bgp"]).unwrap_or_default();
    let bmp_summary = vtysh_frr1(&["show bmp"]).unwrap_or_default();
    let frr2_bgp = vtysh_frr2(&["show ip bgp summary"]).unwrap_or_default();

    fs::write(
        Path::new(&evidence_dir).join("frr1_show_ip_bgp_summary.txt"),
        &bgp_summary,
    )?;
    fs::write(
        Path::new(&evidence_dir).join("frr1_show_ip_bgp.txt"),
        &bgp_routes,
    )?;
    fs::write(
        Path::new(&evidence_dir).join("frr1_show_bmp.txt"),
        &bmp_summary,
    )?;
    fs::write(
        Path::new(&evidence_dir).join("frr2_show_ip_bgp_summary.txt"),
        &frr2_bgp,
    )?;

    let live_captured_bytes = raw_bytes_log.lock().await.clone();
    let live_sha256 = hex::encode(Sha256::digest(&live_captured_bytes));
    fs::write(
        Path::new(&evidence_dir).join("frr_live_network_capture.bin"),
        &live_captured_bytes,
    )?;
    fs::write(
        Path::new(&evidence_dir).join("frr_live_network_capture.sha256"),
        format!("{live_sha256}  frr_live_network_capture.bin\n"),
    )?;

    // Preserve captured_bmp_stream.bin pointing to the pure live network stream
    fs::write(
        Path::new(&evidence_dir).join("captured_bmp_stream.bin"),
        &live_captured_bytes,
    )?;
    fs::write(
        Path::new(&evidence_dir).join("captured_bmp_stream.sha256"),
        format!("{live_sha256}  captured_bmp_stream.bin\n"),
    )?;

    // Positive wire proof: the live capture must contain the genuine VRF RD
    // distinguisher 65000:100 (0x0000FDE8_00000064) emitted by frr1's VRF BGP
    // instance — there is no synthetic fixture anywhere in this gate.
    let rd_65000_100: [u8; 8] = [0x00, 0x00, 0xfd, 0xe8, 0x00, 0x00, 0x00, 0x64];
    let rd_frame_count = live_captured_bytes
        .windows(rd_65000_100.len())
        .filter(|w| *w == rd_65000_100)
        .count();
    assert!(
        rd_frame_count > 0,
        "Live BMP capture contains no RD-instance distinguisher 65000:100 frames"
    );
    println!("Wire proof: {rd_frame_count} occurrences of RD 65000:100 in live capture.");

    let git_commit = run_cmd("git", &["rev-parse", "HEAD"])
        .unwrap_or_default()
        .trim()
        .to_string();
    let manifest = serde_json::json!({
        "gate": "BN-003",
        "task": "Live /routing browser acceptance through Bonsai login/API/proxy against dedicated FRR streams",
        "status": "Verified",
        "git_commit": git_commit,
        "playwright_success": true,
        "browser": "Chromium (headless)",
        "routers": [
            { "name": "frr1", "ip": FRR1_IP, "as": 65001, "role": "bmp-exporter" },
            { "name": "frr2", "ip": FRR2_IP, "as": 65002, "role": "bgp-peer / second exporter" }
        ],
        "bmp_stream_sha256": live_sha256,
        "bmp_bytes_total": live_captured_bytes.len(),
        "live_network_capture_file": "frr_live_network_capture.bin",
        "live_network_capture_sha256": live_sha256,
        "live_network_bytes_total": live_captured_bytes.len(),
        "vrf_rd_distinguisher_wire_occurrences": rd_frame_count,
        "capture_provenance": "100% genuine FRR 10.6.1 BMP wire capture: frr1 exports two concurrent sessions (global RD 0:0 + VRF BLUE instance with rd vpn export 65000:100, peer 10.100.2.1, eBGP route 10.99.1.0/24) and frr2 exports the global session. No synthetic BMP frames are injected; build_rd_bmp_frames fixture removed.",
        "verified_requirements": [
            "1. Unauthenticated access denied with HTTP 401 in API and UI",
            "2. Cross-scope access denied with HTTP 403 (speaker and VRF scope)",
            "3. Authenticated login via POST /api/auth/login setting signed JWT cookie bonsai_token",
            "4. 3 RIB views: AdjRibInPrePolicy, AdjRibInPostPolicy, and LocRib",
            "5. Two exporters (172.100.207.11 and 172.100.207.12) and VRF RD isolation (0:0 and 65000:100)",
            "6. Route announcement 10.10.1.0/24 visible with attributes in browser DOM",
            "7. Route attribute modification (MED 150 / AS-path prepend) reflected in browser DOM",
            "8. Route withdrawal removes prefix from active table in browser DOM",
            "9. Re-announcement and BMP TCP EOF disconnect preserves routes and updates completeness badge to gap",
            "10. Reconnect on a fresh speaker session epoch resolves transport gap",
            "11. Prefix History Timeline renders events with before/after path attributes",
            "12. Truthful empty (198.51.100.99/32) and unsupported (bgp-ls) states",
            "13. Graph projection lag 0 and sequence tracking",
            "14. Routing service restart from durable journals preserves complete state",
            "15. Logout revokes browser session and subsequent navigation is denied"
        ],
        "screenshots": [
            "00_unauthenticated_denied.png",
            "01_routing_dashboard_initial.png",
            "02_route_attribute_modified.png",
            "03_route_withdrawn.png",
            "04_transport_gap_preservation.png",
            "05_reconnect_epoch2.png",
            "06_prefix_history_timeline.png",
            "07_empty_and_unsupported_states.png",
            "08_after_restart_preserved.png",
            "09_logged_out_denied.png"
        ]
    });

    fs::write(
        Path::new(&evidence_dir).join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;

    // 15. Teardown
    println!("Tearing down test servers and Containerlab resources...");
    cleanup_lab();
    bonsai_server_handle.abort();
    ctrl_server_handle.abort();

    println!("\n=======================================================");
    println!("=== BN-003 Acceptance Gate Passed Successfully! ===");
    println!("=======================================================\n");

    Ok(())
}

#[tokio::test]
async fn test_web_router_sanity() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let graph_db_path = temp_dir.path().join("graph_sanity.db");
    let store = Arc::new(GraphStore::open(
        graph_db_path.to_str().unwrap(),
        128 * 1024 * 1024,
    )?);
    store
        .upsert_config_item(ConfigItemRecord {
            id: "app-cfg-require-auth".to_string(),
            config_class: "app_config".to_string(),
            vendor: String::new(),
            name: "require_auth".to_string(),
            version: "1".to_string(),
            content_json: "\"1\"".to_string(),
            enabled: true,
            created_by: "test".to_string(),
        })
        .await?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = build_public_routing_web_router(store);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let client = reqwest::Client::new();
    let res = client.get(format!("http://{addr}/health")).send().await?;
    println!("health status: {}", res.status());
    assert_eq!(res.status(), reqwest::StatusCode::OK);

    let res401 = client
        .get(format!("http://{addr}/api/routing/v1/health"))
        .send()
        .await?;
    println!("api 401 status: {}", res401.status());
    assert_eq!(res401.status(), reqwest::StatusCode::UNAUTHORIZED);
    Ok(())
}
