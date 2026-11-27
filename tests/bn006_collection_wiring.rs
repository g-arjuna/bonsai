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
        inventory_identity: Some("registered-target:192.0.2.1".into()),
        acquisition_profile: Some("nokia.srlinux.fixed-readonly-native-bgp-v1".into()),
    };
    persist_observation(&store, &observation).await.unwrap();
    let bmp_now = bonsai::graph::common::now_ns();
    let bmp_event = bonsai::streaming::bmp::BmpEvent {
        timestamp_ns: bmp_now,
        collector_peer: "192.0.2.99:11019".into(),
        message_type: "peer_up".into(),
        peer_type: 0,
        peer_flags: 0,
        router_distinguisher: 0,
        router_address: "192.0.2.1".into(),
        peer_address: "10.206.0.2".into(),
        peer_as: 65002,
        peer_bgp_id: "192.0.2.2".into(),
        session_state: "established".into(),
        route_entries: Vec::new(),
        raw_len: 48,
        peer_down_reason: None,
        peer_down_reason_name: None,
        stats: Vec::new(),
        sys_name: None,
        sys_descr: None,
        termination_reason: None,
        termination_reason_name: None,
        rib_type: None,
        peer_up_info: None,
        init_admin_string: None,
        termination_message: None,
    };
    store
        .write(bonsai::telemetry::TelemetryUpdate {
            plan_id: String::new(),
            session_id: String::new(),
            source_protocol: "bmp".into(),
            gnmi_path: Vec::new(),
            is_delete: false,
            collector_id: String::new(),
            timestamp_ns: bmp_now,
            target: "192.0.2.1:57400".into(),
            vendor: "nokia.srlinux".into(),
            hostname: "srl-native".into(),
            role: "pe".into(),
            site: "lab".into(),
            path: "streaming/bmp/peer-up".into(),
            value: serde_json::to_value(&bmp_event).unwrap(),
        })
        .await
        .unwrap();
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
        result["native_identity_status"],
        "registered_inventory_target"
    );
    assert_eq!(result["reconciliation_status"], "transport_session_only");
    assert_eq!(
        result["bmp_reconciliation"]["corroborated_established"], 1,
        "actual graph BMP row must reconcile through the MCP response: {result}"
    );
    assert_eq!(
        result["bmp_reconciliation"]["corroboration_level"],
        "transport_session_only_not_afi_safi"
    );
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
    let stale_write = persist_observation(&store, &observation).await;
    assert!(
        stale_write
            .as_ref()
            .err()
            .is_some_and(|err| err.to_string().contains("stale native BGP observation")),
        "delayed older collection must not overwrite newer evidence: {stale_write:?}"
    );
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
        "fresh"
    );

    let mut old_observation = observation.clone();
    old_observation.capture.exporter_address = "192.0.2.4".into();
    old_observation.inventory_identity = Some("registered-target:192.0.2.4".into());
    // 600 s exceeds the explicit 300 s freshness policy constant; 120 s
    // would still be fresh under that contract and cannot prove staleness.
    old_observation.captured_at_ns = bonsai::graph::common::now_ns() - 600_000_000_000;
    persist_observation(&store, &old_observation).await.unwrap();
    assert_eq!(
        call_tool_with_store_and_routing(
            &store,
            None,
            "get_native_bgp_neighbors",
            &json!({"device_address":"192.0.2.4"}),
            &ToolExecutionContext::global(Role::Viewer)
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
async fn native_persistence_exact_replay_and_equal_time_conflict_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let store = GraphStore::open(
        dir.path().join("graph").to_str().unwrap(),
        256 * 1024 * 1024,
    )
    .unwrap();
    let raw = include_str!("fixtures/bn006_srl_26_3_1/neighbors-established.json");
    let capture =
        parse_nokia_srl_native_bgp_neighbors(raw, "192.0.2.8", "v26.3.1", "ipv4-unicast").unwrap();
    let observation = StoredNativeBgpObservation {
        captured_at_ns: 1_769_000_000_000_000_000,
        capture_started_at_ns: 1_769_000_000_000_000_000,
        transport: "synthetic_fixture_replay".into(),
        raw_neighbors: raw.into(),
        raw_system_information: "{}".into(),
        capture,
        inventory_identity: Some("registered-target:192.0.2.8".into()),
        acquisition_profile: Some("nokia.srlinux.fixed-readonly-native-bgp-v1".into()),
    };
    persist_observation(&store, &observation).await.unwrap();
    persist_observation(&store, &observation)
        .await
        .expect("byte-identical exact replay must be idempotent");
    let mut conflict = observation.clone();
    conflict.transport = "synthetic_conflicting_transport".into();
    let err = persist_observation(&store, &conflict)
        .await
        .expect_err("equal-time differing evidence must not silently overwrite");
    assert!(err.to_string().contains("equal-time conflicting"));
}

#[tokio::test]
async fn legacy_address_keyed_identity_is_not_definitively_reconciled() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph").to_str().unwrap(),
            256 * 1024 * 1024,
        )
        .unwrap(),
    );
    let raw = include_str!("fixtures/bn006_srl_26_3_1/neighbors-established.json");
    let capture =
        parse_nokia_srl_native_bgp_neighbors(raw, "192.0.2.9", "v26.3.1", "ipv4-unicast").unwrap();
    let observation = StoredNativeBgpObservation {
        captured_at_ns: bonsai::graph::common::now_ns(),
        capture_started_at_ns: bonsai::graph::common::now_ns(),
        transport: "legacy_address_keyed_synthetic".into(),
        raw_neighbors: raw.into(),
        raw_system_information: "{}".into(),
        capture,
        inventory_identity: None,
        acquisition_profile: None,
    };
    persist_observation(&store, &observation).await.unwrap();
    let result = call_tool_with_store_and_routing(
        &store,
        None,
        "get_native_bgp_neighbors",
        &json!({"device_address":"192.0.2.9"}),
        &ToolExecutionContext::global(Role::Viewer),
    )
    .await
    .unwrap();
    assert_eq!(
        result["native_identity_status"],
        "legacy_address_keyed_unverified"
    );
    assert_eq!(
        result["reconciliation_status"],
        "not_run_unverified_legacy_identity"
    );
    assert!(result["bmp_reconciliation"].is_null());
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
