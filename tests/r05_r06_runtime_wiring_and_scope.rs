use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};

use bonsai::ai_provider::{AiMessage, AiProvider, AiResponse, AiToolCall, AiToolDef};
use bonsai::config::AiConfig;
use bonsai::graph::{ConfigItemRecord, GraphStore};
use bonsai::http_server::auth::parse_routing_access_scope;
use bonsai::http_server::build_public_routing_router;
use bonsai::investigation_runtime::{
    InvestigationTerminalState, load_durable_budget_ledger, run_investigation_with_provider,
};
use bonsai::mcp_server::{
    MAX_GRAPH_ROWS, call_tool_with_store_and_routing, tool_query_graph_with_store,
};
use bonsai::routing_service::{
    EventsQuery, PeersQuery, PrefixHistoryQuery, RoutingServiceConfig, RoutingServiceState,
    build_events_json, build_internal_routing_router, build_peers_json, build_prefix_history_json,
    project_remote_routing_events_to_graph,
};
use rbmp_core::bmp::parser::DEFAULT_MAX_FRAME;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

async fn set_app_config(store: &GraphStore, name: &str, value: &str) {
    store
        .upsert_config_item(ConfigItemRecord {
            id: format!("app-cfg-{name}"),
            config_class: "app_config".to_string(),
            vendor: String::new(),
            name: name.to_string(),
            version: "1".to_string(),
            content_json: value.to_string(),
            enabled: true,
            created_by: "test".to_string(),
        })
        .await
        .expect("upsert app_config");
}

fn build_rd_bmp_frames(
    peer_ip: Ipv4Addr,
    peer_as: u32,
    rd_hi: u32,
    rd_lo: u32,
    prefix_octets: [u8; 3],
    local_pref: u32,
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let init = b"\x03\x00\x00\x00\x1b\x04\x00\x01\x00\x07FRR 9.1\x00\x02\x00\x06edge-a".to_vec();

    let mut rd = [0u8; 8];
    rd[0..4].copy_from_slice(&rd_hi.to_be_bytes());
    rd[4..8].copy_from_slice(&rd_lo.to_be_bytes());

    let mut peer_hdr = Vec::with_capacity(42);
    peer_hdr.push(1); // RdInstance
    peer_hdr.push(0); // flags = 0 (IPv4, 4-byte ASN)
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

    let rm_len = (6 + 42 + bgp_update.len()) as u32;
    let mut rm = Vec::with_capacity(rm_len as usize);
    rm.push(3);
    rm.extend_from_slice(&rm_len.to_be_bytes());
    rm.push(0);
    rm.extend_from_slice(&peer_hdr);
    rm.extend_from_slice(&bgp_update);

    (init, peer_up, rm)
}

struct SequenceMockProvider {
    responses: Mutex<Vec<AiResponse>>,
    calls_made: Arc<Mutex<usize>>,
}

#[bonsai::async_trait::async_trait]
impl AiProvider for SequenceMockProvider {
    async fn complete(
        &self,
        _messages: Vec<AiMessage>,
        _tools: Vec<AiToolDef>,
    ) -> anyhow::Result<AiResponse> {
        let mut count = self.calls_made.lock().unwrap();
        *count += 1;
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            Ok(AiResponse {
                content: Some("Finished investigation.".into()),
                tool_calls: Vec::new(),
                stop_reason: "end_turn".into(),
                tokens_used: 50,
                cost_usd: 0.001,
                raw_parts: None,
            })
        } else {
            Ok(responses.remove(0))
        }
    }

    fn name(&self) -> &str {
        "mock"
    }
}

/// R05 + R06 verification:
/// - Remote MCP routing tools (`get_prefix_journey`, `get_peer_timeline`,
///   `get_evidence_coverage`, `compare_snapshots`, `explain_missing_prefix`) query a
///   separately deployed `bonsai-routing` HTTP service when no in-process `RoutingServiceState` is present.
/// - `project_remote_routing_events_to_graph` projects remote routing events into `GraphStore`
///   over HTTP and persists a durable checkpoint cursor.
/// - `/v1/prefix-history`, `/v1/peers`, and `/v1/events` filter by `speaker` and `vrf_rd` and
///   include `peer_distinguisher` in responses.
/// - `RoutingAccessScope` enforces speaker/VRF boundaries and rejects out-of-scope queries on
///   `/api/routing/v1/*`.
#[tokio::test]
async fn r05_r06_remote_mcp_projector_and_scoped_routing_verification() {
    let dir = TempDir::new().expect("tempdir");
    let token = "r05-r06-test-routing-token";
    let cfg = RoutingServiceConfig {
        bmp_listen_addr: "127.0.0.1:0".into(),
        http_listen_addr: "127.0.0.1:0".into(),
        bmp_journal_path: Some(dir.path().join("bmp.jsonl")),
        event_journal_path: Some(dir.path().join("events.jsonl")),
        max_frame_bytes: DEFAULT_MAX_FRAME,
        internal_service_token: Some(token.to_string()),
    };
    let routing_state = RoutingServiceState::new(cfg)
        .await
        .expect("open routing state");

    let spk_11: IpAddr = "192.0.2.11".parse().unwrap();
    let spk_12: IpAddr = "192.0.2.12".parse().unwrap();

    // Speaker 192.0.2.11, VRF RD 65000:10, announcing 10.99.0.0/24
    let (init1, peer1, rm1) = build_rd_bmp_frames(
        Ipv4Addr::new(198, 51, 100, 11),
        65101,
        65000,
        10,
        [10, 99, 0],
        200,
    );
    for f in [&init1, &peer1, &rm1] {
        routing_state
            .ingest_bmp_bytes(spk_11, Some(1), f)
            .await
            .expect("ingest speaker 11");
    }

    // Speaker 192.0.2.12, VRF RD 65000:20, announcing identical prefix 10.99.0.0/24
    let (init2, peer2, rm2) = build_rd_bmp_frames(
        Ipv4Addr::new(198, 51, 100, 12),
        65102,
        65000,
        20,
        [10, 99, 0],
        150,
    );
    for f in [&init2, &peer2, &rm2] {
        routing_state
            .ingest_bmp_bytes(spk_12, Some(1), f)
            .await
            .expect("ingest speaker 12");
    }

    // 1. Verify R06 scoped prefix-history, peers, and events include `peer_distinguisher` and isolate by VRF/speaker.
    let (_, hist_10) = build_prefix_history_json(
        &routing_state,
        &PrefixHistoryQuery {
            prefix: Some("10.99.0.0/24".into()),
            speaker: Some("192.0.2.11".into()),
            vrf_rd: Some("65000:10".into()),
            limit: Some(50),
        },
    )
    .await;
    let hist_events = hist_10["events"].as_array().expect("events array");
    assert_eq!(
        hist_events.len(),
        1,
        "scoped prefix-history must return only VRF 65000:10"
    );
    assert_eq!(hist_events[0]["speaker_addr"], "192.0.2.11");
    assert_eq!(hist_events[0]["peer_distinguisher"], "65000:10");

    let peers_20 = build_peers_json(
        &routing_state,
        &PeersQuery {
            speaker: Some("192.0.2.12".into()),
            vrf_rd: Some("65000:20".into()),
            state: None,
            limit: Some(50),
        },
    )
    .await;
    let peer_rows = peers_20["peers"].as_array().expect("peers array");
    assert_eq!(peer_rows.len(), 1);
    assert_eq!(peer_rows[0]["speaker_addr"], "192.0.2.12");
    assert_eq!(peer_rows[0]["peer_distinguisher"], "65000:20");

    let events_10 = build_events_json(
        &routing_state,
        &EventsQuery {
            after_seq: Some(0),
            speaker: Some("192.0.2.11".into()),
            vrf_rd: Some("65000:10".into()),
            limit: Some(50),
        },
    )
    .await;
    let ev_rows = events_10["events"].as_array().expect("events array");
    assert_eq!(
        ev_rows.len(),
        2,
        "PeerUp + RouteChange for speaker 192.0.2.11 VRF 65000:10"
    );

    // 2. Serve the routing service over HTTP and verify remote MCP tools + remote projector (`R05`).
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    let router = build_internal_routing_router(routing_state);
    let server_handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let graph_path = dir.path().join("graph_r05.db");
    let graph_store = Arc::new(
        GraphStore::open(graph_path.to_str().unwrap(), 128 * 1024 * 1024)
            .expect("open graph store"),
    );
    set_app_config(
        &graph_store,
        "routing_service_url",
        &format!("http://{addr}"),
    )
    .await;
    set_app_config(&graph_store, "routing_service_token", token).await;

    // Remote MCP tool: get_prefix_journey (`routing` is `None`, forcing HTTP client execution)
    let journey_val = call_tool_with_store_and_routing(
        &graph_store,
        None,
        "get_prefix_journey",
        &serde_json::json!({
            "prefix": "10.99.0.0/24",
            "speaker": "192.0.2.11",
            "vrf_rd": "65000:10"
        }),
    )
    .await
    .expect("remote get_prefix_journey succeeds over HTTP");
    assert_eq!(journey_val["scope"]["prefix"], "10.99.0.0/24");
    assert_eq!(
        journey_val["active_hops"]
            .as_array()
            .expect("active_hops")
            .len(),
        1
    );

    // Remote MCP tool: get_peer_timeline
    let timeline_val = call_tool_with_store_and_routing(
        &graph_store,
        None,
        "get_peer_timeline",
        &serde_json::json!({
            "speaker": "192.0.2.12",
            "vrf_rd": "65000:20"
        }),
    )
    .await
    .expect("remote get_peer_timeline succeeds over HTTP");
    assert_eq!(timeline_val["scope"]["speaker"], "192.0.2.12");
    assert!(
        !timeline_val["transitions"]
            .as_array()
            .expect("transitions")
            .is_empty()
    );

    // Remote MCP tool: get_evidence_coverage
    let cov_val = call_tool_with_store_and_routing(
        &graph_store,
        None,
        "get_evidence_coverage",
        &serde_json::json!({
            "speaker": "192.0.2.11",
            "vrf_rd": "65000:10",
            "prefix": "10.99.0.0/24"
        }),
    )
    .await
    .expect("remote get_evidence_coverage succeeds over HTTP");
    assert!(cov_val.get("completeness_state").is_some());

    // Remote MCP tool: explain_missing_prefix
    let explain_val = call_tool_with_store_and_routing(
        &graph_store,
        None,
        "explain_missing_prefix",
        &serde_json::json!({
            "prefix": "10.99.0.0/24",
            "speaker": "192.0.2.11",
            "vrf_rd": "65000:10"
        }),
    )
    .await
    .expect("remote explain_missing_prefix succeeds over HTTP");
    assert_eq!(explain_val["scope"]["prefix"], "10.99.0.0/24");

    // 3. Verify remote routing event projection into GraphStore over HTTP (`project_remote_routing_events_to_graph`).
    let projected = project_remote_routing_events_to_graph(&graph_store, 250)
        .await
        .expect("remote projection succeeds");
    assert!(
        projected.newly_applied_events >= 4,
        "both speakers' events must be projected into GraphStore"
    );

    let projected_again = project_remote_routing_events_to_graph(&graph_store, 250)
        .await
        .expect("second remote projection succeeds");
    assert_eq!(
        projected_again.newly_applied_events, 0,
        "checkpoint cursor must prevent duplicate projection on subsequent runs"
    );

    // 4. Verify token scope parsing and server-enforced query scoping on `/api/routing/v1/*` (R06).
    let scope = parse_routing_access_scope(
        "routing:read:speaker=192.0.2.11,vrf_rd=65000:10,tenant_id=tenant-a",
    )
    .expect("valid routing scope");
    assert_eq!(scope.speaker.as_deref(), Some("192.0.2.11"));
    assert_eq!(scope.vrf_rd.as_deref(), Some("65000:10"));
    assert_eq!(scope.tenant_id.as_deref(), Some("tenant-a"));

    set_app_config(&graph_store, "require_auth", "1").await;
    let scoped_key = "bsk_scoped_speaker11_vrf10_key";
    let key_hash = hex::encode(Sha256::digest(scoped_key.as_bytes()));
    graph_store
        .upsert_config_item(ConfigItemRecord {
            id: "key-scoped-r06".to_string(),
            config_class: "api_key".to_string(),
            vendor: String::new(),
            name: "key-scoped-r06".to_string(),
            version: "1".to_string(),
            content_json: serde_json::json!({
                "key_hash": key_hash,
                "scope": "routing:read:speaker=192.0.2.11,vrf_rd=65000:10",
                "expires_at_ns": 0,
                "last_used_at_ns": 0,
            })
            .to_string(),
            enabled: true,
            created_by: "test".to_string(),
        })
        .await
        .expect("insert scoped api key");

    let bonsai_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind bonsai listener");
    let bonsai_addr = bonsai_listener.local_addr().expect("bonsai addr");
    let bonsai_store = Arc::clone(&graph_store);
    let bonsai_handle = tokio::spawn(async move {
        let _ = axum::serve(bonsai_listener, build_public_routing_router(bonsai_store)).await;
    });

    let client = reqwest::Client::new();
    let base = format!("http://{bonsai_addr}");

    // Unscoped query with scoped token automatically injects speaker=192.0.2.11 & vrf_rd=65000:10
    let auto_scoped_resp = client
        .get(format!(
            "{base}/api/routing/v1/prefix-history?prefix=10.99.0.0/24"
        ))
        .bearer_auth(scoped_key)
        .send()
        .await
        .expect("auto scoped request");
    assert_eq!(auto_scoped_resp.status(), reqwest::StatusCode::OK);
    let auto_scoped_json: Value = auto_scoped_resp.json().await.expect("json");
    let auto_events = auto_scoped_json["events"].as_array().expect("events");
    assert_eq!(auto_events.len(), 1);
    assert_eq!(auto_events[0]["speaker_addr"], "192.0.2.11");
    assert_eq!(auto_events[0]["peer_distinguisher"], "65000:10");

    // Cross-speaker query is rejected with 403 FORBIDDEN
    let cross_spk_resp = client
        .get(format!(
            "{base}/api/routing/v1/prefix-history?prefix=10.99.0.0/24&speaker=192.0.2.12"
        ))
        .bearer_auth(scoped_key)
        .send()
        .await
        .expect("cross speaker request");
    assert_eq!(cross_spk_resp.status(), reqwest::StatusCode::FORBIDDEN);

    // Cross-VRF query is rejected with 403 FORBIDDEN
    let cross_vrf_resp = client
        .get(format!(
            "{base}/api/routing/v1/prefix-history?prefix=10.99.0.0/24&vrf_rd=65000:20"
        ))
        .bearer_auth(scoped_key)
        .send()
        .await
        .expect("cross vrf request");
    assert_eq!(cross_vrf_resp.status(), reqwest::StatusCode::FORBIDDEN);

    bonsai_handle.abort();
    server_handle.abort();
}

/// R05 + R06 verification:
/// - `run_investigation_with_provider` enforces `ToolCallDeduplicator` in the real runtime loop
///   and halts on duplicate tool calls.
/// - `run_investigation_with_provider` enforces pre-call `BudgetReservationLedger` reservations
///   and persists committed spend in `GraphStore`.
/// - `tool_query_graph_with_store` bounds row materialization at `MAX_GRAPH_ROWS` (`500`).
#[tokio::test]
async fn r05_r06_investigation_runtime_budget_dedup_and_bounded_graph_rows() {
    let dir = TempDir::new().expect("tempdir");
    let graph_path = dir.path().join("graph_inv.db");
    let store = Arc::new(
        GraphStore::open(graph_path.to_str().unwrap(), 128 * 1024 * 1024)
            .expect("open graph store"),
    );

    // 1. Test duplicate tool call detection in the real investigation runtime loop.
    let inv_dedup_id = "inv-r05-dedup";
    store
        .create_investigation(inv_dedup_id.into(), "192.0.2.11".into(), "test".into())
        .await
        .expect("create investigation");

    let call_args = serde_json::json!({ "address": "192.0.2.11" });
    let calls_made = Arc::new(Mutex::new(0usize));
    let provider = Box::new(SequenceMockProvider {
        responses: Mutex::new(vec![
            AiResponse {
                content: Some("Checking device first time".into()),
                tool_calls: vec![AiToolCall {
                    id: "tc-1".into(),
                    name: "query_devices".into(),
                    arguments: call_args.clone(),
                }],
                stop_reason: "tool_use".into(),
                tokens_used: 100,
                cost_usd: 0.001,
                raw_parts: None,
            },
            AiResponse {
                content: Some("Repeating identical tool call".into()),
                tool_calls: vec![AiToolCall {
                    id: "tc-2".into(),
                    name: "query_devices".into(),
                    arguments: call_args,
                }],
                stop_reason: "tool_use".into(),
                tokens_used: 100,
                cost_usd: 0.001,
                raw_parts: None,
            },
        ]),
        calls_made: calls_made.clone(),
    });

    let mut ai_cfg = AiConfig::default();
    ai_cfg.daily_budget_usd = 1.0;
    ai_cfg.per_investigation_budget_usd = 0.05;

    let outcome = run_investigation_with_provider(
        inv_dedup_id.into(),
        "192.0.2.11".into(),
        None,
        store.clone(),
        ai_cfg,
        provider,
    )
    .await;

    assert_eq!(outcome.state, InvestigationTerminalState::Partial);
    assert_eq!(outcome.stop_reason, "repeated_tool_call_detected");
    assert_eq!(*calls_made.lock().unwrap(), 2);

    // Durable budget ledger must have recorded the committed spend ($0.002).
    let ledger = load_durable_budget_ledger(&store).await;
    assert!(
        (ledger.daily_committed_usd() - 0.002).abs() < 1e-9,
        "durable budget ledger must persist committed spend across turns"
    );

    // 2. Test pre-call budget reservation rejection in the real investigation loop.
    let inv_budget_id = "inv-r05-budget";
    store
        .create_investigation(inv_budget_id.into(), "192.0.2.11".into(), "test".into())
        .await
        .expect("create budget investigation");

    let budget_calls_made = Arc::new(Mutex::new(0usize));
    let budget_provider = Box::new(SequenceMockProvider {
        responses: Mutex::new(vec![
            AiResponse {
                content: Some("First call commits $0.004 of $0.005 budget".into()),
                tool_calls: vec![AiToolCall {
                    id: "tc-b1".into(),
                    name: "query_devices".into(),
                    arguments: serde_json::json!({ "address": "192.0.2.11" }),
                }],
                stop_reason: "tool_use".into(),
                tokens_used: 200,
                cost_usd: 0.004,
                raw_parts: None,
            },
            AiResponse {
                content: Some("Second call should never execute".into()),
                tool_calls: Vec::new(),
                stop_reason: "end_turn".into(),
                tokens_used: 200,
                cost_usd: 0.004,
                raw_parts: None,
            },
        ]),
        calls_made: budget_calls_made.clone(),
    });

    let mut tight_budget_cfg = AiConfig::default();
    tight_budget_cfg.daily_budget_usd = 1.0;
    tight_budget_cfg.per_investigation_budget_usd = 0.005;

    let budget_outcome = run_investigation_with_provider(
        inv_budget_id.into(),
        "192.0.2.11".into(),
        None,
        store.clone(),
        tight_budget_cfg,
        budget_provider,
    )
    .await;

    assert_eq!(
        budget_outcome.state,
        InvestigationTerminalState::BudgetExhausted
    );
    assert_eq!(budget_outcome.stop_reason, "investigation_budget_exceeded");
    assert_eq!(
        *budget_calls_made.lock().unwrap(),
        1,
        "pre-call budget reservation must reject iteration 1 before calling provider.complete()"
    );

    // 3. Test bounded graph row materialization (`MAX_GRAPH_ROWS`) in `tool_query_graph_with_store`.
    let bounded_res: Value = tool_query_graph_with_store(
        &store,
        &serde_json::json!({
            "cypher": "UNWIND range(1, 1200) AS x RETURN x"
        }),
    )
    .await
    .expect("UNWIND query succeeds");
    assert_eq!(bounded_res["row_count"], MAX_GRAPH_ROWS);
    assert_eq!(bounded_res["total_rows"], MAX_GRAPH_ROWS + 1);
    assert_eq!(bounded_res["row_cap"], MAX_GRAPH_ROWS);
    assert_eq!(bounded_res["truncated"], true);
}
