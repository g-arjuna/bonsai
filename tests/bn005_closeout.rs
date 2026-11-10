//! Remaining BN-005 acceptance: encoded bounds, actual subprocesses and late history.
use bonsai::mcp_server::{MAX_TOOL_OUTPUT_BYTES, enforce_tool_output_bounds};

#[test]
fn encoded_output_bound_includes_unicode_and_json_escaping() {
    for value in ["🦀".repeat(70_000), "\"\\\n".repeat(70_000)] {
        let result = enforce_tool_output_bounds(serde_json::json!({"data": value}));
        assert_eq!(result["truncated"], true);
        assert!(serde_json::to_vec(&result).unwrap().len() <= MAX_TOOL_OUTPUT_BYTES);
    }
}

#[test]
fn conflicting_journals_rejected_before_open() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = bonsai::routing_service::RoutingServiceConfig {
        internal_service_token: Some("closure-token".into()),
        identity_db_path: None,
        bmp_journal_path: Some(dir.path().join("shared")),
        event_journal_path: Some(dir.path().join("shared")),
        ..Default::default()
    };
    assert!(cfg.validate_deployment().is_err());
}

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

struct ServiceProcess {
    child: Child,
    log: PathBuf,
}
impl Drop for ServiceProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn launch(dir: &Path, label: &str, vars: &[(&str, &str)]) -> ServiceProcess {
    let log = dir.join(format!("{label}.log"));
    let file = std::fs::File::create(&log).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bonsai-routing"));
    for name in [
        "BONSAI_ROUTING_SERVICE_TOKEN",
        "BONSAI_ROUTING_BMP_JOURNAL",
        "BONSAI_ROUTING_EVENT_JOURNAL",
        "BONSAI_ALLOW_INSECURE_DEV",
        "BONSAI_ROUTING_ALLOW_INSECURE_NO_TOKEN",
        "BONSAI_ROUTING_ALLOW_EPHEMERAL",
    ] {
        cmd.env_remove(name);
    }
    cmd.env("BONSAI_ROUTING_BMP_ADDR", "127.0.0.1:0")
        .env("BONSAI_ROUTING_HTTP_ADDR", "127.0.0.1:0")
        .env("RUST_LOG", "info")
        .envs(vars.iter().copied())
        .stdout(Stdio::from(file.try_clone().unwrap()))
        .stderr(Stdio::from(file));
    ServiceProcess {
        child: cmd.spawn().unwrap(),
        log,
    }
}
async fn ready(process: &mut ServiceProcess) -> (String, String) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let log = std::fs::read_to_string(&process.log).unwrap();
        if let Some(line) = log.lines().find(|line| line.contains("Started reduced")) {
            let addr = |field: &str| {
                line.split_once(field)
                    .unwrap()
                    .1
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .to_string()
            };
            return (addr("bmp_addr="), addr("http_addr="));
        }
        assert!(
            process.child.try_wait().unwrap().is_none(),
            "startup exited: {log}"
        );
        assert!(Instant::now() < deadline, "startup deadline: {log}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn actual_binary_fail_closed_startup_matrix() {
    let dir = tempfile::tempdir().unwrap();
    let bmp = dir.path().join("bmp").to_str().unwrap().to_string();
    let events = dir.path().join("events").to_str().unwrap().to_string();
    let token = ("BONSAI_ROUTING_SERVICE_TOKEN", "process-token");
    let journals = [
        ("BONSAI_ROUTING_BMP_JOURNAL", bmp.as_str()),
        ("BONSAI_ROUTING_EVENT_JOURNAL", events.as_str()),
    ];
    let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("defaults", vec![]),
        ("missing-token", journals.to_vec()),
        (
            "blank-token",
            vec![journals[0], journals[1], (token.0, "   ")],
        ),
        (
            "invalid-token",
            vec![journals[0], journals[1], (token.0, "bad\ntoken")],
        ),
        ("missing-journals", vec![token]),
        (
            "same-journal",
            vec![token, journals[0], (journals[1].0, bmp.as_str())],
        ),
        (
            "unwritable",
            vec![
                token,
                (journals[0].0, "/proc/bonsai-bn005/bmp"),
                journals[1],
            ],
        ),
        (
            "directory-journal",
            vec![
                token,
                (journals[0].0, dir.path().to_str().unwrap()),
                journals[1],
            ],
        ),
        (
            "fake-loopback",
            vec![
                ("BONSAI_ALLOW_INSECURE_DEV", "1"),
                ("BONSAI_ROUTING_HTTP_ADDR", "127.attacker.example:8081"),
            ],
        ),
        (
            "insecure-public",
            vec![
                ("BONSAI_ALLOW_INSECURE_DEV", "1"),
                ("BONSAI_ROUTING_HTTP_ADDR", "0.0.0.0:0"),
            ],
        ),
    ];
    for (label, vars) in cases {
        let mut process = launch(dir.path(), label, &vars);
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = process.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "{label} did not fail closed");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let log = std::fs::read_to_string(&process.log).unwrap();
        assert!(!status.success(), "{label} started: {log}");
        assert!(
            !log.contains("Started reduced"),
            "{label} served before rejection"
        );
    }
    // Explicit loopback-only ephemeral development mode really starts.
    let mut dev = launch(
        dir.path(),
        "explicit-dev",
        &[("BONSAI_ALLOW_INSECURE_DEV", "1")],
    );
    let (_, http) = ready(&mut dev).await;
    assert!(
        reqwest::get(format!("http://{http}/internal/routing/v1/health"))
            .await
            .unwrap()
            .status()
            .is_success()
    );
    // Production non-loopback binding requires a token and persistent paths.
    let mut public = launch(
        dir.path(),
        "authenticated-public",
        &[
            token,
            journals[0],
            journals[1],
            ("BONSAI_ROUTING_HTTP_ADDR", "0.0.0.0:0"),
        ],
    );
    let (_, http) = ready(&mut public).await;
    let http = http.replace("0.0.0.0", "127.0.0.1");
    assert_eq!(
        reqwest::get(format!("http://{http}/internal/routing/v1/health"))
            .await
            .unwrap()
            .status(),
        401
    );
}

async fn api(http: &str, path: &str) -> serde_json::Value {
    let res = reqwest::Client::new()
        .get(format!("http://{http}/internal/routing/v1/{path}"))
        .header("x-bonsai-routing-token", "process-token")
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success(), "{path}: {}", res.status());
    res.json().await.unwrap()
}
async fn send_route(bmp: &str, lp: u32) -> tokio::net::TcpStream {
    let mut socket = tokio::net::TcpStream::connect(bmp).await.unwrap();
    let (init, up, route) = bonsai::routing_service::sample_bmp_session_frames(
        std::net::Ipv4Addr::new(192, 0, 2, 2),
        65002,
        [10, 77, 9],
        lp,
    );
    for frame in [init, up, route] {
        socket.write_all(&frame).await.unwrap();
    }
    socket
}
async fn wait_history(http: &str, lp: u64) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let body = api(http, "prefix-history?prefix=10.77.9.0/24").await;
        if body["events"][0]["after_attributes"]["local_pref"] == lp {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "durable history local_pref={lp}: {body}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
async fn wait_route(http: &str, lp: u64) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let body = api(http, "routes?prefix=10.77.9.0/24").await;
        if body["routes"]
            .as_array()
            .is_some_and(|a| a.iter().any(|r| r["local_pref"] == lp))
        {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "expected local_pref={lp}: {body}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn actual_binary_kill_restart_preserves_history_and_accepts_new_ingest() {
    let dir = tempfile::tempdir().unwrap();
    let bmp_path = dir.path().join("bmp.bin");
    let event_path = dir.path().join("events.jsonl");
    let vars = [
        ("BONSAI_ROUTING_SERVICE_TOKEN", "process-token"),
        ("BONSAI_ROUTING_BMP_JOURNAL", bmp_path.to_str().unwrap()),
        ("BONSAI_ROUTING_EVENT_JOURNAL", event_path.to_str().unwrap()),
    ];
    let mut first = launch(dir.path(), "first", &vars);
    let (bmp, http) = ready(&mut first).await;
    let mut contender = launch(dir.path(), "conflicting-owner", &vars);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = contender.child.try_wait().unwrap() {
            assert!(!status.success());
            let log = std::fs::read_to_string(&contender.log).unwrap();
            assert!(log.contains("Cannot lock routing journal"), "{log}");
            assert!(!log.contains("Started reduced"));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "two processes owned one journal pair"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let socket = send_route(&bmp, 100).await;
    let original = wait_route(&http, 100).await;
    let seq = original["durable_sequence"].as_u64().unwrap();
    let epoch = original["routes"][0]["source"]["session_epoch"]
        .as_u64()
        .unwrap();
    let routes = original["routes"].clone();
    let history = wait_history(&http, 100).await;
    assert_eq!(history["events"][0]["after_attributes"]["local_pref"], 100);
    // Every internal GET and POST rejects absent and incorrect tokens in a real process.
    let client = reqwest::Client::new();
    for path in [
        "health",
        "speakers",
        "peers",
        "routes",
        "prefix-history?prefix=10.77.9.0/24",
        "coverage",
        "evidence-coverage",
        "prefix-journey?prefix=10.77.9.0/24",
        "peer-timeline",
        "compare-snapshots",
        "explain-prefix?prefix=10.77.9.0/24",
        "events",
        "adjudications",
    ] {
        for token in [None, Some("incorrect")] {
            let mut req = client.get(format!("http://{http}/internal/routing/v1/{path}"));
            if let Some(token) = token {
                req = req.header("x-bonsai-routing-token", token);
            }
            assert_eq!(req.send().await.unwrap().status(), 401, "{path}");
        }
        assert!(api(&http, path).await.is_object());
    }
    for token in [None, Some("incorrect")] {
        let mut req = client
            .post(format!("http://{http}/internal/routing/v1/ingest-bmp"))
            .json(&serde_json::json!({"frame_hex":"00"}));
        if let Some(token) = token {
            req = req.header("x-bonsai-routing-token", token);
        }
        assert_eq!(req.send().await.unwrap().status(), 401);
    }
    let adjudication = serde_json::json!({"prefix":"10.77.9.0/24", "diagnosis_verdict":"unknown", "adjudicated_by":"bn005", "rationale":"token boundary"});
    for token in [None, Some("incorrect")] {
        let mut req = client
            .post(format!(
                "http://{http}/internal/routing/v1/adjudicate-prefix"
            ))
            .json(&adjudication);
        if let Some(token) = token {
            req = req.header("x-bonsai-routing-token", token);
        }
        assert_eq!(req.send().await.unwrap().status(), 401);
    }
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    drop(socket);
    let mut second = launch(dir.path(), "restarted", &vars);
    let (bmp2, http2) = ready(&mut second).await;
    let recovered = wait_route(&http2, 100).await;
    assert_eq!(recovered["routes"], routes);
    assert_eq!(
        api(&http2, "prefix-history?prefix=10.77.9.0/24").await["events"],
        history["events"]
    );
    let socket2 = send_route(&bmp2, 250).await;
    let updated = wait_route(&http2, 250).await;
    assert!(updated["durable_sequence"].as_u64().unwrap() > seq);
    assert!(
        updated["routes"][0]["source"]["session_epoch"]
            .as_u64()
            .unwrap()
            > epoch
    );
    let history2 = wait_history(&http2, 250).await;
    assert_eq!(history2["events"][0]["after_attributes"]["local_pref"], 250);
    assert!(
        history2["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["after_attributes"]["local_pref"] == 100)
    );
    drop(socket2);
}

#[tokio::test]
async fn query_deadline_and_request_cancellation_stop_engine_workers() {
    use bonsai::graph::{
        GraphStore,
        explorer::{active_diagnostic_queries, execute_query_with_deadline},
    };
    let dir = tempfile::tempdir().unwrap();
    let store = GraphStore::open(
        dir.path().join("graph").to_str().unwrap(),
        256 * 1024 * 1024,
    )
    .unwrap();
    // Large RANGE expressions are folded before native cancellation checkpoints.
    assert!(
        bonsai::graph::explorer::validate_query("UNWIND RANGE(1,1000000) AS x RETURN x").is_err()
    );
    assert!(bonsai::graph::explorer::validate_query("UNWIND RANGE(1, $n) AS x RETURN x").is_err());
    {
        let db = store.db();
        let conn = lbug::Connection::new(&db).unwrap();
        conn.query("UNWIND RANGE(1,500) AS x CREATE (:Device {address: CAST(x AS STRING)})")
            .unwrap();
    }
    let expensive = "MATCH (a:Device), (b:Device), (c:Device), (d:Device) RETURN SUM(CAST(a.address AS INT64)*CAST(b.address AS INT64)*CAST(c.address AS INT64)*CAST(d.address AS INT64))";
    let start = Instant::now();
    let result =
        execute_query_with_deadline(store.db(), expensive.into(), Duration::from_millis(50)).await;
    assert!(result.is_err(), "expensive query must be interrupted");
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "watchdog must stop engine work: elapsed={:?}, result={result:?}",
        start.elapsed()
    );
    assert_eq!(active_diagnostic_queries(), 0);
    let db = store.db();
    let handle = tokio::spawn(execute_query_with_deadline(
        db,
        expensive.into(),
        Duration::from_secs(5),
    ));
    let deadline = Instant::now() + Duration::from_secs(2);
    while active_diagnostic_queries() == 0 {
        assert!(Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(30)).await;
    handle.abort();
    let _ = handle.await;
    let deadline = Instant::now() + Duration::from_secs(3);
    while active_diagnostic_queries() != 0 {
        assert!(
            Instant::now() < deadline,
            "cancelled request left a blocking query worker"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // A subsequent write and read prove there is no abandoned read transaction.
    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    conn.query("CREATE (:Device {address: 'after-cancel'})")
        .unwrap();
    assert_eq!(
        conn.query("MATCH (d:Device) RETURN count(d)")
            .unwrap()
            .next()
            .unwrap()[0]
            .to_string(),
        "501"
    );
}

#[tokio::test]
async fn sparse_late_scoped_history_is_bounded_pageable_and_restart_stable() {
    use bonsai::routing_service::*;
    use rbmp_core::bmp::types::{EventKey, ObservationSource};
    use rbmp_rib::event::{RibEvent, RibEventPayload};
    let dir = tempfile::tempdir().unwrap();
    let cfg = RoutingServiceConfig {
        bmp_journal_path: Some(dir.path().join("bmp.bin")),
        event_journal_path: Some(dir.path().join("events.jsonl")),
        internal_service_token: Some("late-history".into()),
        identity_db_path: None,
        ..Default::default()
    };
    let state = RoutingServiceState::new(cfg.clone()).await.unwrap();
    let own: std::net::IpAddr = "192.0.2.1".parse().unwrap();
    let foreign: std::net::IpAddr = "192.0.2.2".parse().unwrap();
    // Build a canonical valid route event, then independently set sequence, source and RD.
    let (init, up, route) =
        sample_bmp_session_frames("192.0.2.9".parse().unwrap(), 65009, [10, 88, 1], 100);
    for frame in [init, up, route] {
        state.ingest_bmp_bytes(own, Some(1), &frame).await.unwrap();
    }
    let base = state
        .event_journal
        .all_events()
        .await
        .into_iter()
        .find(|e| matches!(e.payload, RibEventPayload::RouteChange(_)))
        .unwrap();
    let mut batch = Vec::new();
    for seq in 4..=12_503 {
        let is_own = seq >= 12_500;
        let speaker = if is_own { own } else { foreign };
        let mut ev: RibEvent = base.clone();
        ev.event_key = EventKey::new(ObservationSource::new(speaker, 1), seq);
        ev.id = uuid::Uuid::new_v4();
        ev.speaker = speaker;
        if let RibEventPayload::RouteChange(ref mut rc) = ev.payload {
            rc.peer_header.peer_distinguisher = if seq % 2 == 0 {
                [0, 0, 0xfd, 0xe8, 0, 0, 0, 100]
            } else {
                [0; 8]
            };
            rc.peer_key = rbmp_core::bmp::types::PeerKey::from_header(speaker, "", &rc.peer_header);
            rc.view_key.peer = rc.peer_key.clone();
            rc.route_key.view = rc.view_key.clone();
            rc.attributes.as_mut().unwrap().local_pref = Some(seq as u32);
        }
        batch.push(ev);
    }
    state.event_journal.append_batch(&batch).await.unwrap();
    let query = EventsQuery {
        speaker: Some(own.to_string()),
        vrf_rd: Some("65000:100".into()),
        limit: Some(1),
        after_seq: Some(3),
    };
    let first = build_events_json(&state, &query).await;
    assert_eq!(first["events"].as_array().unwrap().len(), 0);
    assert_eq!(first["scanned_events"], 10_000);
    assert_eq!(first["next_after_seq"], 10_003);
    assert_eq!(first["history_coverage"], "partial");
    let second = build_events_json(
        &state,
        &EventsQuery {
            after_seq: first["next_after_seq"].as_u64(),
            ..query
        },
    )
    .await;
    assert_eq!(second["events"][0]["event_key"]["sequence"], 12_500);
    let third_q = EventsQuery {
        speaker: Some(own.to_string()),
        vrf_rd: Some("65000:100".into()),
        limit: Some(1),
        after_seq: second["next_after_seq"].as_u64(),
    };
    let third = build_events_json(&state, &third_q).await;
    assert_eq!(third["events"][0]["event_key"]["sequence"], 12_502);
    let done = build_events_json(
        &state,
        &EventsQuery {
            after_seq: Some(12_502),
            ..third_q
        },
    )
    .await;
    assert_eq!(done["history_coverage"], "complete");
    assert!(done["events"].as_array().unwrap().is_empty());
    let journey_q = PrefixJourneyQuery {
        speaker: Some(own.to_string()),
        vrf_rd: Some("65000:100".into()),
        prefix: Some("10.88.1.0/24".into()),
        ..Default::default()
    };
    let (_, journey) = build_prefix_journey_json(&state, &journey_q).await;
    assert_eq!(journey["history_coverage"], "partial");
    assert_eq!(journey["history_window"]["scanned_events"], 10_000);
    assert_eq!(journey["observations"].as_array().unwrap().len(), 2);
    let (_, history) = build_prefix_history_json(
        &state,
        &PrefixHistoryQuery {
            speaker: Some(own.to_string()),
            vrf_rd: Some("65000:100".into()),
            prefix: Some("10.88.1.0/24".into()),
            limit: Some(1),
        },
    )
    .await;
    assert_eq!(history["history_coverage"], "partial");
    assert_eq!(history["events"][0]["sequence"], 12_502);
    let diff = build_compare_snapshots_json(&state, &CompareSnapshotsQuery::default()).await;
    assert_eq!(diff["history_coverage"], "partial");
    assert_eq!(diff["status"], "partial");
    drop(state);
    let recovered = RoutingServiceState::new(cfg).await.unwrap();
    let restored = build_events_json(
        &recovered,
        &EventsQuery {
            speaker: Some(own.to_string()),
            vrf_rd: Some("65000:100".into()),
            limit: Some(1),
            after_seq: Some(12_500),
        },
    )
    .await;
    assert_eq!(restored["events"], third["events"]);
}

struct ScopeProbeProvider;
#[bonsai::async_trait::async_trait]
impl bonsai::ai_provider::AiProvider for ScopeProbeProvider {
    async fn complete(
        &self,
        messages: Vec<bonsai::ai_provider::AiMessage>,
        _: Vec<bonsai::ai_provider::AiToolDef>,
    ) -> anyhow::Result<bonsai::ai_provider::AiResponse> {
        use bonsai::ai_provider::{AiResponse, AiToolCall};
        assert!(
            !messages.iter().any(|m| m
                .content
                .as_deref()
                .unwrap_or("")
                .contains("FOREIGN_SECRET")),
            "prefetch or model tool result leaked a foreign neighbour"
        );
        let done = messages.iter().any(|m| m.role == "tool");
        Ok(AiResponse {
            content: Some("No grounded conclusion".into()),
            tool_calls: if done {
                vec![]
            } else {
                vec![
                    AiToolCall {
                        id: "foreign".into(),
                        name: "get_changes".into(),
                        arguments: serde_json::json!({"device_address":"192.0.2.2"}),
                    },
                    AiToolCall {
                        id: "cypher".into(),
                        name: "query_graph".into(),
                        arguments: serde_json::json!({"cypher":"MATCH (d:Device) RETURN d.address"}),
                    },
                    AiToolCall {
                        id: "incident".into(),
                        name: "get_incident".into(),
                        arguments: serde_json::json!({"id":"own-probe"}),
                    },
                    AiToolCall {
                        id: "own".into(),
                        name: "query_devices".into(),
                        arguments: serde_json::json!({"_scoped_speaker":"192.0.2.2"}),
                    },
                ]
            },
            stop_reason: if done { "end_turn" } else { "tool_use" }.into(),
            tokens_used: 10,
            cost_usd: 0.0,
            raw_parts: None,
        })
    }
    fn name(&self) -> &str {
        "scope-probe-mock"
    }
}

#[tokio::test]
async fn investigator_inherits_principal_for_prefetch_and_model_tools() {
    use bonsai::{
        graph::GraphStore,
        investigation_runtime::{InvestigationRunOptions, run_investigation_with_options},
        mcp_server::ToolExecutionContext,
    };
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(
        GraphStore::open(
            dir.path().join("graph").to_str().unwrap(),
            256 * 1024 * 1024,
        )
        .unwrap(),
    );
    {
        let db = store.db();
        let conn = lbug::Connection::new(&db).unwrap();
        conn.query("CREATE (:Device {address:'192.0.2.1', hostname:'own'}), (:Device {address:'192.0.2.2', hostname:'FOREIGN_SECRET'})").unwrap();
        conn.query("CREATE (:Interface {id:'own-if', device_address:'192.0.2.1'}), (:Interface {id:'foreign-if', device_address:'192.0.2.2'})").unwrap();
        conn.query("MATCH (d:Device), (i:Interface) WHERE d.address = i.device_address CREATE (d)-[:HAS_INTERFACE]->(i)").unwrap();
        conn.query("MATCH (i:Interface {id:'own-if'}), (j:Interface {id:'foreign-if'}) CREATE (i)-[:CONNECTED_TO]->(j)").unwrap();
        conn.query("CREATE (:DetectionEvent {id:'own-probe', device_address:'192.0.2.1', rule_id:'bgp_session_down', severity:'critical', fired_at:cast('2026-10-15 10:00:00','TIMESTAMP_NS')})").unwrap();
    }
    let inv = store
        .create_investigation("".into(), "192.0.2.1".into(), "scope-probe".into())
        .await
        .unwrap();
    run_investigation_with_options(
        inv.id.clone(),
        "192.0.2.1".into(),
        None,
        store.clone(),
        Default::default(),
        Box::new(ScopeProbeProvider),
        InvestigationRunOptions {
            tool_context: Some(ToolExecutionContext::for_device("192.0.2.1")),
            ..Default::default()
        },
    )
    .await;
    let calls = store.list_tool_calls(inv.id).await.unwrap();
    assert_eq!(calls.len(), 4);
    let mut denials = 0;
    for call in calls {
        if call.tool_name == "query_devices" {
            let result: serde_json::Value = serde_json::from_str(&call.output_json).unwrap();
            assert_eq!(result["devices"].as_array().unwrap().len(), 1);
            assert_eq!(result["devices"][0]["address"], "192.0.2.1");
        } else if call.tool_name == "get_incident" {
            assert!(!call.output_json.contains("FOREIGN_SECRET"));
            let result: serde_json::Value = serde_json::from_str(&call.output_json).unwrap();
            assert_eq!(result["device_address"], "192.0.2.1");
            assert!(
                result["blast_radius"]["reachable_devices"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        } else {
            assert!(call.output_json.contains("routing_scope_violation"));
            denials += 1;
        }
    }
    assert_eq!(denials, 2);
}

#[test]
fn advertised_tools_have_an_explicit_role_and_scope_support_matrix() {
    use bonsai::http_server::auth::{Role, RoutingAccessScope};
    use bonsai::mcp_server::{authorize_and_scope_tool, tool_schemas};
    let schemas = tool_schemas();
    let device = [
        "query_devices",
        "get_native_bgp_neighbors",
        "get_changes",
        "get_device_blast_radius",
        "list_active_detections",
        "get_incident",
    ];
    let routing = [
        "get_prefix_journey",
        "get_peer_timeline",
        "get_evidence_coverage",
        "compare_snapshots",
        "explain_missing_prefix",
    ];
    for tool in schemas["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        assert!(
            device.contains(&name)
                || routing.contains(&name)
                || ["query_graph", "evaluate_change_plan"].contains(&name),
            "new tool needs a policy classification: {name}"
        );
        for role in [Role::ApiReadonly, Role::Viewer, Role::Operator, Role::Admin] {
            for mask in 0..8 {
                let scope = RoutingAccessScope {
                    speaker: (mask & 1 != 0).then(|| "192.0.2.1".into()),
                    vrf_rd: (mask & 2 != 0).then(|| "65000:100".into()),
                    tenant_id: (mask & 4 != 0).then(|| "acme".into()),
                };
                let minimum = match name {
                    "query_graph" => Role::Viewer,
                    "evaluate_change_plan" => Role::Operator,
                    _ => Role::ApiReadonly,
                };
                let supported = mask == 0
                    || if name == "evaluate_change_plan" {
                        true
                    } else if device.contains(&name) {
                        mask == 1
                    } else if routing.contains(&name) {
                        mask & 4 == 0 && (mask & 1 != 0 || name == "compare_snapshots")
                    } else {
                        false
                    };
                let mut args = serde_json::json!({});
                let result = authorize_and_scope_tool(
                    name,
                    &mut args,
                    (mask != 0).then_some(&scope),
                    Some(&role),
                );
                assert_eq!(
                    result.is_ok(),
                    supported && role >= minimum,
                    "name={name} role={role:?} mask={mask}: {result:?}"
                );
            }
        }
    }
    let scope = RoutingAccessScope {
        speaker: Some("192.0.2.1".into()),
        ..Default::default()
    };
    assert!(
        authorize_and_scope_tool(
            "future_unclassified_tool",
            &mut serde_json::json!({}),
            Some(&scope),
            Some(&Role::Admin)
        )
        .is_err()
    );
}

#[test]
fn compose_requires_token_and_persists_both_journals_on_the_runtime_volume() {
    let compose: serde_yaml::Value =
        serde_yaml::from_str(include_str!("../docker-compose.yml")).unwrap();
    let service = &compose["services"]["bonsai-routing"];
    assert_eq!(
        service["entrypoint"][0].as_str(),
        Some("/usr/local/bin/bonsai-routing")
    );
    let env: Vec<_> = service["environment"]
        .as_sequence()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(env.iter().any(|v| v.starts_with("BONSAI_ROUTING_SERVICE_TOKEN=${BONSAI_ROUTING_SERVICE_TOKEN:?")));
    assert!(env.contains(&"BONSAI_ROUTING_BMP_JOURNAL=/app/runtime/routing-bmp.journal"));
    assert!(env.contains(&"BONSAI_ROUTING_EVENT_JOURNAL=/app/runtime/routing-events.jsonl"));
    assert!(
        service["volumes"]
            .as_sequence()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("bonsai_graph_dev:/app/runtime"))
    );
    assert!(
        !service["ports"]
            .as_sequence()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("8081"))
    );
}

#[tokio::test]
async fn concurrent_ingestion_never_publishes_a_cursor_past_unpersisted_events() {
    use bonsai::routing_service::*;
    let dir = tempfile::tempdir().unwrap();
    let state = RoutingServiceState::new(RoutingServiceConfig {
        bmp_journal_path: Some(dir.path().join("bmp")),
        event_journal_path: Some(dir.path().join("events")),
        ..Default::default()
    })
    .await
    .unwrap();
    let mut writers = Vec::new();
    for id in 1..=10u8 {
        let state = state.clone();
        writers.push(tokio::spawn(async move {
            let speaker = std::net::IpAddr::from([192, 0, 2, id]);
            let (init, up, _) = sample_bmp_session_frames(
                [198, 51, 100, id].into(),
                65000 + u32::from(id),
                [10, id, 1],
                100,
            );
            for frame in [init, up] {
                state
                    .ingest_bmp_bytes(speaker, Some(1), &frame)
                    .await
                    .unwrap();
            }
            for lp in 100..150 {
                let (_, _, route) = sample_bmp_session_frames(
                    [198, 51, 100, id].into(),
                    65000 + u32::from(id),
                    [10, id, 1],
                    lp,
                );
                state
                    .ingest_bmp_bytes(speaker, Some(1), &route)
                    .await
                    .unwrap();
            }
        }));
    }
    let mut cursor = 0;
    let mut observed = std::collections::BTreeSet::new();
    loop {
        let page = build_events_json(
            &state,
            &EventsQuery {
                after_seq: Some(cursor),
                limit: Some(17),
                ..Default::default()
            },
        )
        .await;
        for event in page["events"].as_array().unwrap() {
            assert!(
                observed.insert(event["event_key"]["sequence"].as_u64().unwrap()),
                "duplicate event"
            );
        }
        cursor = page["next_after_seq"].as_u64().unwrap();
        if writers.iter().all(tokio::task::JoinHandle::is_finished) && page["truncated"] == false {
            break;
        }
        tokio::task::yield_now().await;
    }
    for writer in writers {
        writer.await.unwrap();
    }
    assert_eq!(observed, (1..=520u64).collect());
}

#[tokio::test]
async fn shared_explorer_runner_bounds_serialized_unicode_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = bonsai::graph::GraphStore::open(
        dir.path().join("graph").to_str().unwrap(),
        256 * 1024 * 1024,
    )
    .unwrap();
    let query = format!(
        "UNWIND RANGE(1,500) AS x RETURN '{}' AS value",
        "🦀".repeat(2000)
    );
    let result = bonsai::graph::explorer::execute_query_with_deadline(
        store.db(),
        query,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert!(serde_json::to_vec(&result).unwrap().len() <= 65_536);
    assert!(result.truncated);
    let large_metadata = format!("RETURN '{}' AS value", ",".repeat(15_000));
    let result = bonsai::graph::explorer::execute_query_with_deadline(
        store.db(),
        large_metadata,
        Duration::from_secs(5),
    )
    .await;
    assert!(
        result
            .unwrap_err()
            .contains("metadata exceeds output byte cap")
    );
}
