//! BN-003 Live Routing & RIB Browser Acceptance Gate
//!
//! Orchestrates the complete end-to-end acceptance gate:
//! 1. Containerlab dedicated FRR topology (AS 65001 <-> AS 65002)
//! 2. Two real BMP exporters (frr1: 172.100.207.11 and frr2: 172.100.207.12)
//! 3. Global RD (0:0) and VRF RD (65000:100) isolation
//! 4. Real RoutingServiceState with durable BmpJournal and RibEventJournal
//! 5. Bonsai authenticated HTTP web server with ui/dist SPA, JWT cookies, and routing proxy
//! 6. Playwright automated headless Chromium browser testing all 11 user journeys in real DOM
//! 7. Collection and preservation of screenshots, vtysh logs, and test evidence in
//!    `docs/merger/evidence/2026-10-13-bn003/`

use std::fs;
use std::net::{IpAddr, Ipv4Addr};
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
use rbmp_core::bmp::parser::{DEFAULT_MAX_FRAME, read_bmp_frame};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};

const LAB_TOPO_PATH: &str = "lab/fast-iteration/frr-bmp-r07.clab.yml";
const FRR_IMAGE: &str = "quay.io/frrouting/frr:10.6.1";
const BMP_PORT: u16 = 11019;
const FRR1_IP: &str = "172.100.207.11";
const FRR2_IP: &str = "172.100.207.12";
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
    let mut args = vec!["exec", "clab-bonsai-frr-r07-frr1", "vtysh"];
    for c in cmds {
        args.push("-c");
        args.push(c);
    }
    run_cmd("docker", &args)
}

fn vtysh_frr2(cmds: &[&str]) -> Result<String> {
    let mut args = vec!["exec", "clab-bonsai-frr-r07-frr2", "vtysh"];
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

fn build_rd_bmp_frames(
    peer_ip: Ipv4Addr,
    peer_as: u32,
    rd_hi: u32,
    rd_lo: u32,
    prefix_octets: [u8; 3],
    local_pref: u32,
) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let init = b"\x03\x00\x00\x00\x1e\x04\x00\x01\x00\x08FRR 10.6\x00\x02\x00\x08edge-vrf".to_vec();

    let mut rd = [0u8; 8];
    rd[0..4].copy_from_slice(&rd_hi.to_be_bytes());
    rd[4..8].copy_from_slice(&rd_lo.to_be_bytes());

    let mut peer_hdr = Vec::with_capacity(42);
    peer_hdr.push(1); // RdInstance
    peer_hdr.push(0); // flags = 0
    peer_hdr.extend_from_slice(&rd);
    peer_hdr.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    peer_hdr.extend_from_slice(&peer_ip.octets());
    peer_hdr.extend_from_slice(&peer_as.to_be_bytes());
    peer_hdr.extend_from_slice(&peer_ip.octets());
    peer_hdr.extend_from_slice(&1700000000u32.to_be_bytes());
    peer_hdr.extend_from_slice(&0u32.to_be_bytes());

    let mut open = Vec::with_capacity(29);
    open.extend_from_slice(&[0xff; 16]);
    open.extend_from_slice(&29u16.to_be_bytes());
    open.push(1);
    open.push(4);
    open.extend_from_slice(&(peer_as as u16).to_be_bytes());
    open.extend_from_slice(&90u16.to_be_bytes());
    open.extend_from_slice(&peer_ip.octets());
    open.push(0);

    let mut peer_up = Vec::with_capacity(126);
    peer_up.push(3);
    peer_up.extend_from_slice(&126u32.to_be_bytes());
    peer_up.push(3);
    peer_up.extend_from_slice(&peer_hdr);
    peer_up.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 0, 2, 254]);
    peer_up.extend_from_slice(&179u16.to_be_bytes());
    peer_up.extend_from_slice(&50000u16.to_be_bytes());
    peer_up.extend_from_slice(&open);
    peer_up.extend_from_slice(&open);

    let mut attrs = Vec::new();
    attrs.extend_from_slice(&[0x40, 1, 1, 0]);
    attrs.extend_from_slice(&[0x40, 2, 6, 2, 1]);
    attrs.extend_from_slice(&peer_as.to_be_bytes());
    attrs.extend_from_slice(&[0x40, 3, 4]);
    attrs.extend_from_slice(&peer_ip.octets());
    attrs.extend_from_slice(&[0x40, 5, 4]);
    attrs.extend_from_slice(&local_pref.to_be_bytes());

    let nlri = [24, prefix_octets[0], prefix_octets[1], prefix_octets[2]];
    let mut bgp_update = Vec::new();
    bgp_update.extend_from_slice(&[0xff; 16]);
    let bgp_len = (19 + 2 + 2 + attrs.len() + nlri.len()) as u16;
    bgp_update.extend_from_slice(&bgp_len.to_be_bytes());
    bgp_update.push(2);
    bgp_update.extend_from_slice(&0u16.to_be_bytes());
    bgp_update.extend_from_slice(&(attrs.len() as u16).to_be_bytes());
    bgp_update.extend_from_slice(&attrs);
    bgp_update.extend_from_slice(&nlri);

    let total_rm_len = (6 + peer_hdr.len() + bgp_update.len()) as u32;
    let mut rm = Vec::with_capacity(total_rm_len as usize);
    rm.push(3);
    rm.extend_from_slice(&total_rm_len.to_be_bytes());
    rm.push(0);
    rm.extend_from_slice(&peer_hdr);
    rm.extend_from_slice(&bgp_update);

    let mut eor_bgp = Vec::new();
    eor_bgp.extend_from_slice(&[0xff; 16]);
    eor_bgp.extend_from_slice(&23u16.to_be_bytes());
    eor_bgp.push(2);
    eor_bgp.extend_from_slice(&0u16.to_be_bytes());
    eor_bgp.extend_from_slice(&0u16.to_be_bytes());

    let total_eor_len = (6 + peer_hdr.len() + eor_bgp.len()) as u32;
    let mut eor = Vec::with_capacity(total_eor_len as usize);
    eor.push(3);
    eor.extend_from_slice(&total_eor_len.to_be_bytes());
    eor.push(0);
    eor.extend_from_slice(&peer_hdr);
    eor.extend_from_slice(&eor_bgp);

    (init, peer_up, rm, eor)
}

fn split_bmp_frames(data: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut offset = 0;
    while offset + 6 <= data.len() {
        let length = u32::from_be_bytes([
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
            data[offset + 4],
        ]) as usize;
        if data[offset] != 3 || length < 6 || offset + length > data.len() {
            break;
        }
        frames.push(data[offset..offset + length].to_vec());
        offset += length;
    }
    frames
}

struct ControlContext {
    routing_state: Arc<tokio::sync::RwLock<Arc<RoutingServiceState>>>,
    reconnect_trigger: Arc<tokio::sync::Notify>,
    reconnect_epoch_established: Arc<tokio::sync::Notify>,
    bmp_journal_path: PathBuf,
    event_journal_path: PathBuf,
    internal_routing_port: u16,
    active_routing_server_abort: Mutex<Option<tokio::task::AbortHandle>>,
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
    // Genuine router-driven disconnect: disable BMP target in frr1 so it closes the TCP session
    let res = vtysh_frr1(&["conf t", "router bgp 65001", "no bmp targets bonsai", "end"]);
    if res.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    // Wait until reader loop records transport gap from TCP EOF
    for _ in 0..40 {
        let state = ctx.routing_state.read().await.clone();
        if !state.transport_gaps.read().await.is_empty() {
            return StatusCode::OK;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    StatusCode::OK
}

async fn handle_bmp_reconnect(AxumState(ctx): AxumState<Arc<ControlContext>>) -> StatusCode {
    ctx.reconnect_trigger.notify_one();
    // Genuine router-driven reconnect: re-enable BMP target in frr1
    let res = vtysh_frr1(&[
        "conf t",
        "router bgp 65001",
        "bmp targets bonsai",
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
        Duration::from_secs(15),
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

    // 5. Setup single BMP TCP listener on port 11019 shared across all sessions
    let bmp_listener = Arc::new(
        TcpListener::bind(format!("0.0.0.0:{BMP_PORT}"))
            .await
            .context("bind TCP listener for BMP on port 11019")?,
    );

    // 6. Setup Control Server for Playwright interaction
    let reconnect_notify = Arc::new(tokio::sync::Notify::new());
    let reconnect_epoch_established = Arc::new(tokio::sync::Notify::new());

    let shared_routing_state = Arc::new(tokio::sync::RwLock::new(Arc::clone(&routing_state)));

    let ctrl_ctx = Arc::new(ControlContext {
        routing_state: Arc::clone(&shared_routing_state),
        reconnect_trigger: Arc::clone(&reconnect_notify),
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

    // 8. Accept BMP connection from frr1 (172.100.207.11)
    println!("Waiting for BMP connection from frr1...");
    let (mut stream1, sock1) = timeout(Duration::from_secs(25), bmp_listener.accept())
        .await
        .context("Timeout waiting for BMP from frr1")?
        .context("Accept error frr1")?;
    let spk1_addr = sock1.ip();
    println!("Accepted BMP connection from frr1: {spk1_addr}");

    let raw_bytes_log = Arc::new(Mutex::new(Vec::<u8>::new()));
    let raw_rec1 = Arc::clone(&raw_bytes_log);
    let state_holder1 = Arc::clone(&shared_routing_state);

    let reader_handle_epoch1 = tokio::spawn(async move {
        loop {
            let res = read_bmp_frame(
                &mut stream1,
                DEFAULT_MAX_FRAME,
                Duration::from_secs(60),
                Duration::from_secs(30),
            )
            .await;
            let cur_state = state_holder1.read().await.clone();
            match res {
                Ok(Some(frame)) => {
                    raw_rec1.lock().await.extend_from_slice(&frame);
                    let _ = cur_state.ingest_bmp_bytes(spk1_addr, Some(1), &frame).await;
                }
                Ok(None) => {
                    // Production collector EOF detection path: TCP EOF = gap, never synthetic BGP withdraw
                    cur_state
                        .record_transport_disconnect_for_epoch(spk1_addr, Some(1), "bmp_tcp_eof")
                        .await;
                    break;
                }
                Err(e) => {
                    cur_state
                        .record_transport_disconnect_for_epoch(
                            spk1_addr,
                            Some(1),
                            format!("bmp_transport_error: {e}"),
                        )
                        .await;
                    break;
                }
            }
        }
    });

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

    // Accept BMP connection from frr2
    let bmp_listener_frr2 = Arc::clone(&bmp_listener);
    let state_holder2 = Arc::clone(&shared_routing_state);
    let raw_rec2 = Arc::clone(&raw_bytes_log);
    let frr2_connected = Arc::new(tokio::sync::Notify::new());
    let frr2_connected_notify = Arc::clone(&frr2_connected);
    tokio::spawn(async move {
        if let Ok(Ok((mut stream2, sock2))) =
            timeout(Duration::from_secs(15), bmp_listener_frr2.accept()).await
        {
            let spk2_addr = sock2.ip();
            println!("Accepted BMP connection from frr2: {spk2_addr}");
            frr2_connected_notify.notify_waiters();
            loop {
                let cur_state = state_holder2.read().await.clone();
                match read_bmp_frame(
                    &mut stream2,
                    DEFAULT_MAX_FRAME,
                    Duration::from_secs(60),
                    Duration::from_secs(30),
                )
                .await
                {
                    Ok(Some(frame)) => {
                        raw_rec2.lock().await.extend_from_slice(&frame);
                        let _ = cur_state.ingest_bmp_bytes(spk2_addr, Some(1), &frame).await;
                    }
                    Ok(None) => {
                        cur_state
                            .record_transport_disconnect_for_epoch(
                                spk2_addr,
                                Some(1),
                                "bmp_tcp_eof",
                            )
                            .await;
                        break;
                    }
                    Err(e) => {
                        cur_state
                            .record_transport_disconnect_for_epoch(
                                spk2_addr,
                                Some(1),
                                format!("bmp_transport_error: {e}"),
                            )
                            .await;
                        break;
                    }
                }
            }
        }
    });

    // Await BMP connection from frr2 before proceeding
    let _ = timeout(Duration::from_secs(15), frr2_connected.notified()).await;

    // 10. Ingest VRF RD 65000:100 route (10.99.1.0/24) and End-of-RIB to verify VRF RD isolation
    let spk1_ip_parsed: IpAddr = FRR1_IP.parse().unwrap();
    let (vrf_init, vrf_peer_up, vrf_rm, vrf_eor) = build_rd_bmp_frames(
        Ipv4Addr::new(10, 100, 1, 1),
        65002,
        65000,
        100,
        [10, 99, 1],
        100,
    );
    let mut synthetic_vrf_fixture_bytes = Vec::<u8>::new();
    synthetic_vrf_fixture_bytes.extend_from_slice(&vrf_init);
    synthetic_vrf_fixture_bytes.extend_from_slice(&vrf_peer_up);
    synthetic_vrf_fixture_bytes.extend_from_slice(&vrf_rm);
    synthetic_vrf_fixture_bytes.extend_from_slice(&vrf_eor);

    routing_state
        .ingest_bmp_bytes(spk1_ip_parsed, Some(1), &vrf_init)
        .await?;
    routing_state
        .ingest_bmp_bytes(spk1_ip_parsed, Some(1), &vrf_peer_up)
        .await?;
    routing_state
        .ingest_bmp_bytes(spk1_ip_parsed, Some(1), &vrf_rm)
        .await?;
    routing_state
        .ingest_bmp_bytes(spk1_ip_parsed, Some(1), &vrf_eor)
        .await?;

    // 11. Wait for BGP session between frr1 and frr2 to establish
    println!("Waiting for BGP neighbor session between frr1 and frr2...");
    let mut bgp_up = false;
    for _ in 0..40 {
        if let Ok(out) = vtysh_frr1(&["show ip bgp summary"]) {
            if out.contains("frr2(10.100.1.1)")
                && !out.contains("Active")
                && !out.contains("Connect")
                && !out.contains("Idle")
            {
                bgp_up = true;
                break;
            }
        }
        sleep(Duration::from_millis(500)).await;
    }
    assert!(
        bgp_up,
        "BGP session between frr1 and frr2 failed to establish"
    );

    // 12. Announce initial route 10.10.1.0/24 from frr2
    println!("Announcing 10.10.1.0/24 from frr2...");
    vtysh_frr2(&[
        "conf t",
        "ip route 10.10.1.0/24 Null0",
        "router bgp 65002",
        "address-family ipv4 unicast",
        "network 10.10.1.0/24",
        "end",
    ])?;

    // Wait until route appears in RIB
    let mut announced = false;
    for _ in 0..40 {
        let rib = routing_state.rib.read().await;
        if rib.total_routes() >= 1 {
            announced = true;
            break;
        }
        drop(rib);
        sleep(Duration::from_millis(300)).await;
    }
    assert!(announced, "Route 10.10.1.0/24 not received in RIB");
    println!("Route 10.10.1.0/24 active in RIB.");

    // Background listener for Epoch 2 Reconnect on bmp_listener
    let bmp_listener_epoch2 = Arc::clone(&bmp_listener);
    let state_holder_epoch2 = Arc::clone(&shared_routing_state);
    let reconnect_signal = Arc::clone(&reconnect_notify);
    let reconnect_epoch_established_listener = Arc::clone(&reconnect_epoch_established);
    let raw_rec_epoch2 = Arc::clone(&raw_bytes_log);

    tokio::spawn(async move {
        reconnect_signal.notified().await;
        println!("Control signal received: waiting for epoch 2 reconnect on port 11019...");
        if let Ok(Ok((mut stream2, sock2))) =
            timeout(Duration::from_secs(20), bmp_listener_epoch2.accept()).await
        {
            let spk2 = sock2.ip();
            println!("Accepted epoch 2 BMP connection from {spk2}");
            let epoch = {
                let cur_state = state_holder_epoch2.read().await.clone();
                let mut rib = cur_state.rib.write().await;
                rib.open_speaker_session(spk2, Utc::now())
            };
            println!("Advanced session epoch to {epoch} for {spk2}");

            // Re-inject VRF RD route and End-of-RIB for epoch 2 into routing state
            // NOTE: Do NOT append synthetic fixture frames to raw_rec_epoch2 (frr_live_network_capture.bin)
            let (_vrf_init2, vrf_peer_up2, vrf_rm2, vrf_eor2) = build_rd_bmp_frames(
                Ipv4Addr::new(10, 100, 1, 1),
                65002,
                65000,
                100,
                [10, 99, 1],
                100,
            );

            let cur_state = state_holder_epoch2.read().await.clone();
            let _ = cur_state
                .ingest_bmp_bytes(spk2, Some(epoch), &vrf_peer_up2)
                .await;
            let _ = cur_state
                .ingest_bmp_bytes(spk2, Some(epoch), &vrf_rm2)
                .await;
            let _ = cur_state
                .ingest_bmp_bytes(spk2, Some(epoch), &vrf_eor2)
                .await;

            let mut notified_epoch_established = false;
            loop {
                let cur_state = state_holder_epoch2.read().await.clone();
                match read_bmp_frame(
                    &mut stream2,
                    DEFAULT_MAX_FRAME,
                    Duration::from_secs(60),
                    Duration::from_secs(30),
                )
                .await
                {
                    Ok(Some(frame)) => {
                        raw_rec_epoch2.lock().await.extend_from_slice(&frame);
                        let _ = cur_state.ingest_bmp_bytes(spk2, Some(epoch), &frame).await;
                        if !notified_epoch_established {
                            notified_epoch_established = true;
                            reconnect_epoch_established_listener.notify_waiters();
                        }
                    }
                    Ok(None) => {
                        cur_state
                            .record_transport_disconnect_for_epoch(spk2, Some(epoch), "bmp_tcp_eof")
                            .await;
                        break;
                    }
                    Err(e) => {
                        cur_state
                            .record_transport_disconnect_for_epoch(
                                spk2,
                                Some(epoch),
                                format!("bmp_transport_error: {e}"),
                            )
                            .await;
                        break;
                    }
                }
            }
        }
    });

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

    let vrf_fixture_sha256 = hex::encode(Sha256::digest(&synthetic_vrf_fixture_bytes));
    fs::write(
        Path::new(&evidence_dir).join("synthetic_vrf_fixture.bin"),
        &synthetic_vrf_fixture_bytes,
    )?;
    fs::write(
        Path::new(&evidence_dir).join("synthetic_vrf_fixture.sha256"),
        format!("{vrf_fixture_sha256}  synthetic_vrf_fixture.bin\n"),
    )?;

    // Preserve captured_bmp_stream.bin pointing to pure live network stream for compatibility
    fs::write(
        Path::new(&evidence_dir).join("captured_bmp_stream.bin"),
        &live_captured_bytes,
    )?;
    fs::write(
        Path::new(&evidence_dir).join("captured_bmp_stream.sha256"),
        format!("{live_sha256}  captured_bmp_stream.bin\n"),
    )?;

    // Verify capture provenance: pure live capture must not contain synthetic fixture frames
    let live_frames = split_bmp_frames(&live_captured_bytes);
    let fixture_frames = split_bmp_frames(&synthetic_vrf_fixture_bytes);
    let contaminated_frames: Vec<_> = live_frames
        .iter()
        .filter(|frame| fixture_frames.contains(frame))
        .collect();
    assert!(
        contaminated_frames.is_empty(),
        "Contamination detected: {} fixture frames (total {} bytes) found in live capture!",
        contaminated_frames.len(),
        contaminated_frames.iter().map(|f| f.len()).sum::<usize>()
    );

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
        "synthetic_vrf_fixture_file": "synthetic_vrf_fixture.bin",
        "synthetic_vrf_fixture_sha256": vrf_fixture_sha256,
        "synthetic_vrf_fixture_bytes_total": synthetic_vrf_fixture_bytes.len(),
        "fixture_provenance": "Deterministic BMP frame fixture injection for VRF RD 65000:100 stored separately in synthetic_vrf_fixture.bin; live FRR 10.6.1 BMP stream for global RD 0:0 and speakers 172.100.207.11 / 172.100.207.12 stored in frr_live_network_capture.bin",
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
            "10. Reconnect on session epoch 2 resolves transport gap",
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
    reader_handle_epoch1.abort();

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
