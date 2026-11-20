use bonsai::{
    enrichment::native_bgp::{StoredNativeBgpObservation, persist_observation},
    graph::{ConfigItemRecord, GraphStore},
    http_server::{
        AppState,
        auth::{Role, RoutingAccessScope},
        mint_jwt, router_from_state,
    },
    mcp_server::{ToolExecutionContext, call_tool_with_store_and_routing},
    remediation::parse_nokia_srl_native_bgp_neighbors,
};
use serde_json::{Value, json};
use std::sync::Arc;

async fn config(store: &GraphStore, name: &str, value: &str) {
    store
        .upsert_config_item(ConfigItemRecord {
            id: format!("bn006-{name}"),
            config_class: "app_config".into(),
            vendor: String::new(),
            name: name.into(),
            version: "1".into(),
            content_json: value.into(),
            enabled: true,
            created_by: "test".into(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn native_tool_reads_persisted_evidence_with_scope_and_truthful_freshness() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph").to_str().unwrap(),
            256 * 1024 * 1024,
        )
        .unwrap(),
    );
    config(&store, "ownership_access_mode", "strict").await;
    let raw = include_str!("fixtures/bn006_srl_26_3_1/neighbors-established.json");
    let capture =
        parse_nokia_srl_native_bgp_neighbors(raw, "192.0.2.1", "v26.3.1", "ipv4-unicast").unwrap();
    let mut observation = StoredNativeBgpObservation {
        captured_at_ns: bonsai::graph::common::now_ns(),
        capture_started_at_ns: bonsai::graph::common::now_ns(),
        transport: "synthetic_fixture_replay".into(),
        raw_neighbors: raw.into(),
        raw_system_information: "{}".into(),
        capture,
    };
    persist_observation(&store, &observation).await.unwrap();
    let context = ToolExecutionContext {
        role: Role::ApiReadonly,
        scope: Some(RoutingAccessScope {
            speaker: Some("192.0.2.1".into()),
            ..Default::default()
        }),
    };
    let result = call_tool_with_store_and_routing(
        &store,
        None,
        "get_native_bgp_neighbors",
        &json!({}),
        &context,
    )
    .await
    .unwrap();
    assert_eq!(result["freshness"], "fresh");
    assert_eq!(result["capture"]["exporter_address"], "192.0.2.1");
    assert_eq!(
        result["capture"]["neighbors"][0]["session_state"],
        "established"
    );
    assert!(
        call_tool_with_store_and_routing(
            &store,
            None,
            "get_native_bgp_neighbors",
            &json!({"device_address":"192.0.2.2"}),
            &context
        )
        .await
        .unwrap_err()
        .contains("routing_scope_violation")
    );
    observation.captured_at_ns -= 120_000_000_000;
    persist_observation(&store, &observation).await.unwrap();
    assert_eq!(
        call_tool_with_store_and_routing(
            &store,
            None,
            "get_native_bgp_neighbors",
            &json!({}),
            &context
        )
        .await
        .unwrap()["freshness"],
        "stale"
    );
    observation.captured_at_ns = bonsai::graph::common::now_ns() + 120_000_000_000;
    persist_observation(&store, &observation).await.unwrap();
    assert_eq!(
        call_tool_with_store_and_routing(
            &store,
            None,
            "get_native_bgp_neighbors",
            &json!({}),
            &context
        )
        .await
        .unwrap()["freshness"],
        "unknown"
    );
    let global = ToolExecutionContext::global(Role::Viewer);
    assert!(
        call_tool_with_store_and_routing(
            &store,
            None,
            "get_native_bgp_neighbors",
            &json!({"device_address":"192.0.2.3"}),
            &global
        )
        .await
        .unwrap_err()
        .contains("native_bgp_evidence_unavailable")
    );
}

#[tokio::test]
#[ignore = "explicit dedicated Containerlab SR Linux and pinned host-key file required"]
async fn live_native_collection_enters_operator_api_and_mcp() {
    let lab = std::env::var("BONSAI_BN006_LIVE_ROOT").expect("dedicated lab root");
    let address = std::env::var("BONSAI_BN006_SRL_ADDRESS").expect("dedicated SR Linux address");
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph").to_str().unwrap(),
            256 * 1024 * 1024,
        )
        .unwrap(),
    );
    config(&store, "require_auth", "true").await;
    config(&store, "ownership_access_mode", "strict").await;
    config(
        &store,
        "native_bgp_known_hosts",
        &std::env::var("BONSAI_BN006_KNOWN_HOSTS").unwrap_or_else(|_| format!("{lab}/known_hosts")),
    )
    .await;
    let state = AppState::new_minimal_with_runtime_dir(store.clone(), dir.path().to_str().unwrap());
    state
        .registry
        .add_device(bonsai::config::TargetConfig {
            address: format!("{address}:57400"),
            enabled: true,
            vendor: Some("nokia.srlinux".into()),
            username: Some("admin".into()),
            password: Some("NokiaSrl1!".into()),
            ..Default::default()
        })
        .unwrap();
    // AUTH-C2: src=local JWTs require live local accounts.
    for (u, r) in [("bn006-operator", "operator"), ("bn006-viewer", "viewer")] {
        store
            .auth_store
            .upsert_user(&bonsai::auth_store::AuthUser {
                username: u.into(),
                password_hash: "test-verifier".into(),
                role: r.into(),
                enabled: true,
                last_login_ns: 0,
                created_at_ns: 0,
                updated_at_ns: 0,
            })
            .unwrap();
    }
    let operator = mint_jwt("bn006-operator", "operator", &store)
        .await
        .unwrap();
    let viewer = mint_jwt("bn006-viewer", "viewer", &store).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = router_from_state(state);
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let request = json!({"device_address":address,"afi_safi":"ipv4-unicast"});
    let url = format!("{base}/api/routing/v1/native-bgp");
    assert_eq!(
        client
            .post(&url)
            .json(&request)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&viewer)
            .json(&request)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let response = client
        .post(&url)
        .bearer_auth(&operator)
        .json(&request)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, 200, "{text}");
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["neighbor_count"], 1);
    let rpc: Value = client
        .post(format!("{base}/mcp"))
        .bearer_auth(&operator)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"get_native_bgp_neighbors","arguments":{"device_address":address}}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let result: Value = serde_json::from_str(
        rpc["result"]["content"][0]["text"]
            .as_str()
            .expect("MCP result"),
    )
    .unwrap();
    assert_eq!(result["freshness"], "fresh");
    assert_eq!(result["transport"], "verified_ssh_read_only_cli");
    assert_eq!(result["capture"]["neighbors"][0]["peer_as"], 65002);
    assert_eq!(result["capture"]["neighbors"][0]["received_routes"], 1);
    assert_eq!(
        result["capture"]["neighbors"][0]["session_state"],
        "established"
    );
    let evidence_name = std::env::var("BONSAI_BN006_NATIVE_EVIDENCE")
        .unwrap_or_else(|_| "operator-native-evidence.json".into());
    assert_eq!(
        std::path::Path::new(&evidence_name).file_name().unwrap(),
        evidence_name.as_str()
    );
    let evidence_path = std::path::Path::new(&lab).join(evidence_name);
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(evidence_path)
        .unwrap();
    file.write_all(&serde_json::to_vec_pretty(&json!({"collection":body,"mcp":result})).unwrap())
        .unwrap();
    server.abort();
}
