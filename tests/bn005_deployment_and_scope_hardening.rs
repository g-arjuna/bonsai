//! BN-005 Integration and Acceptance Test:
//! Fail-closed deployment defaults, internal service token enforcement, identity-derived
//! tenant/exporter/VRF scoping, graph query read-only boundaries & execution timeout,
//! and untruncated event history pagination.

use std::net::Ipv4Addr;
use std::sync::Arc;

use bonsai::graph::{ConfigItemRecord, GraphStore};
use bonsai::http_server::{build_public_routing_web_router, mint_jwt};
use bonsai::routing_service::{
    RoutingServiceConfig, RoutingServiceState, build_internal_routing_router,
    sample_bmp_session_frames,
};
use rbmp_core::bmp::types::{EventKey, ObservationSource};
use rbmp_rib::event::{RibEvent, RibEventPayload};
use rbmp_store::RibEventJournal;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

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
            created_by: "bn005-test".to_string(),
        })
        .await
        .expect("upsert app_config");
}

async fn provision_test_key(store: &GraphStore, id: &str, key: &str, scope: &str) {
    store
        .upsert_config_item(ConfigItemRecord {
            id: id.into(),
            config_class: "api_key".into(),
            vendor: String::new(),
            name: id.into(),
            version: "1".into(),
            content_json:
                serde_json::json!({"key_hash":hex::encode(Sha256::digest(key.as_bytes())),
            "scope":scope, "expires_at_ns":0, "last_used_at_ns":0})
                .to_string(),
            enabled: true,
            created_by: "bn005-closeout".into(),
        })
        .await
        .unwrap();
}

/// 1. Deployment configuration fails closed for direct binary launch and service initialization.
#[test]
fn test_bn005_routing_config_deployment_validation() {
    // A default config with no internal token or persistence must fail closed.
    let default_cfg = RoutingServiceConfig::default();
    let res = default_cfg.validate_deployment();
    assert!(
        res.is_err(),
        "Default config must fail validate_deployment without explicit dev override"
    );
    let err_msg = res.unwrap_err();
    assert!(
        err_msg.contains("Missing internal_service_token"),
        "Expected missing internal_service_token error, got: {err_msg}"
    );

    // Supplying internal token but missing persistence must fail closed.
    let mut with_token = RoutingServiceConfig {
        internal_service_token: Some("secret-token-xyz".to_string()),
        identity_db_path: None,
        identity_scope: None,
        ..RoutingServiceConfig::default()
    };
    let res2 = with_token.validate_deployment();
    assert!(
        res2.is_err(),
        "Config with token but without durable journals must fail validate_deployment"
    );
    let err_msg2 = res2.unwrap_err();
    assert!(
        err_msg2.contains("Missing persistent journal paths"),
        "Expected missing persistent journal paths error, got: {err_msg2}"
    );

    // Non-loopback bind without token must be strictly refused under all circumstances.
    with_token.http_listen_addr = "0.0.0.0:8080".to_string();
    with_token.internal_service_token = None;
    let res3 = with_token.validate_deployment();
    assert!(
        res3.is_err(),
        "Non-loopback bind without token must always be rejected"
    );
}

/// 2. Fail-closed internal routing endpoints require valid internal service token.
#[tokio::test]
async fn test_bn005_fail_closed_internal_endpoints() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token = "internal-routing-token-bn005";

    let routing_cfg = RoutingServiceConfig {
        bmp_listen_addr: "127.0.0.1:0".to_string(),
        http_listen_addr: "127.0.0.1:0".to_string(),
        bmp_journal_path: Some(dir.path().join("bmp.bin")),
        event_journal_path: Some(dir.path().join("events.jsonl")),
        internal_service_token: Some(token.to_string()),
        identity_db_path: None,
        identity_scope: None,
        ..RoutingServiceConfig::default()
    };
    assert!(routing_cfg.validate_deployment().is_ok());

    let state = RoutingServiceState::new(routing_cfg).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, build_internal_routing_router(state))
            .await
            .unwrap();
    });

    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // A. Unauthenticated calls to internal endpoints return 401 Unauthorized
    for endpoint in &[
        "/internal/routing/v1/health",
        "/internal/routing/v1/speakers",
        "/internal/routing/v1/peers",
        "/internal/routing/v1/routes",
        "/internal/routing/v1/events",
    ] {
        let resp = client
            .get(format!("{base}{endpoint}"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "Endpoint {endpoint} must fail closed with 401 when missing token"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "unauthorized_internal_client");
    }

    // B. Invalid token returns 401 Unauthorized
    let bad_token_resp = client
        .get(format!("{base}/internal/routing/v1/health"))
        .header("x-bonsai-routing-token", "wrong-bad-token")
        .send()
        .await
        .unwrap();
    assert_eq!(bad_token_resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // C. Correct token returns 200 OK
    let ok_resp = client
        .get(format!("{base}/internal/routing/v1/health"))
        .header("x-bonsai-routing-token", token)
        .send()
        .await
        .unwrap();
    assert_eq!(ok_resp.status(), reqwest::StatusCode::OK);
}

/// 3. Fail-closed public web server requires authentication by default.
#[tokio::test]
async fn test_bn005_fail_closed_public_web_auth() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph_auth.db").to_str().unwrap(),
            256 * 1024 * 1024,
        )
        .unwrap(),
    );
    // Preserve the original BN-005 acceptance contract in explicit strict mode.
    set_app_config(&store, "ownership_access_mode", "strict").await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let web_router = build_public_routing_web_router(Arc::clone(&store));
    tokio::spawn(async move {
        axum::serve(listener, web_router).await.unwrap();
    });

    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Health is public
    let health_resp = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(health_resp.status(), reqwest::StatusCode::OK);

    // API endpoints fail closed without credentials
    let routes_resp = client
        .get(format!("{base}/api/routing/v1/routes"))
        .send()
        .await
        .unwrap();
    assert_eq!(routes_resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    let invs_resp = client
        .get(format!("{base}/api/investigations"))
        .send()
        .await
        .unwrap();
    assert_eq!(invs_resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // With valid JWT, request is authenticated
    let token = mint_jwt("operator-user", "operator", &store).await.unwrap();
    let auth_invs = client
        .get(format!("{base}/api/investigations"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(auth_invs.status(), reqwest::StatusCode::OK);
}

/// 4. Identity-derived tenant, exporter, and VRF scoping on GET and POST requests.
#[tokio::test]
async fn test_bn005_identity_derived_tenant_and_exporter_scoping() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph_scope.db").to_str().unwrap(),
            256 * 1024 * 1024,
        )
        .unwrap(),
    );
    // Preserve the original BN-005 acceptance contract in explicit strict mode.
    set_app_config(&store, "ownership_access_mode", "strict").await;

    let internal_token = "scope-test-internal-token";
    let routing_cfg = RoutingServiceConfig {
        bmp_listen_addr: "127.0.0.1:0".to_string(),
        http_listen_addr: "127.0.0.1:0".to_string(),
        bmp_journal_path: Some(dir.path().join("bmp.bin")),
        event_journal_path: Some(dir.path().join("events.jsonl")),
        internal_service_token: Some(internal_token.to_string()),
        identity_db_path: None,
        identity_scope: None,
        ..RoutingServiceConfig::default()
    };
    let routing_state = RoutingServiceState::new(routing_cfg).await.unwrap();
    let routing_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let routing_addr = routing_listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(
            routing_listener,
            build_internal_routing_router(routing_state),
        )
        .await
        .unwrap();
    });

    set_app_config(
        &store,
        "routing_service_url",
        &format!("http://{routing_addr}"),
    )
    .await;
    set_app_config(&store, "routing_service_token", internal_token).await;

    // Ingest sample route into routing service
    let (init, _, _) =
        sample_bmp_session_frames(Ipv4Addr::new(192, 0, 2, 1), 65001, [192, 0, 2], 100);
    let client = reqwest::Client::new();
    client
        .post(format!(
            "http://{routing_addr}/internal/routing/v1/ingest-bmp"
        ))
        .header("x-bonsai-routing-token", internal_token)
        .json(&serde_json::json!({
            "speaker_addr": "192.0.2.1",
            "session_epoch": 1,
            "frame_hex": hex::encode(&init),
        }))
        .send()
        .await
        .unwrap();

    // Create 2 investigations in store: one for 192.0.2.1, one for 192.0.2.2
    let inv1 = store
        .create_investigation(
            "det-1".to_string(),
            "192.0.2.1".to_string(),
            "operator".to_string(),
        )
        .await
        .unwrap();
    let inv2 = store
        .create_investigation(
            "det-2".to_string(),
            "192.0.2.2".to_string(),
            "operator".to_string(),
        )
        .await
        .unwrap();

    // Create an API key scoped to speaker=192.0.2.1, vrf_rd=65000:100, tenant_id=acme
    let scoped_api_key = "bsk_scoped_token_alpha_12345678";
    let key_hash = hex::encode(Sha256::digest(scoped_api_key.as_bytes()));
    store
        .upsert_config_item(ConfigItemRecord {
            id: "apikey-scoped-alpha".to_string(),
            config_class: "api_key".to_string(),
            vendor: String::new(),
            name: "scoped-alpha".to_string(),
            version: "1".to_string(),
            content_json: serde_json::json!({
                "key_hash": key_hash,
                "scope": "routing:write:speaker=192.0.2.1,vrf_rd=65000:100,tenant_id=acme",
                "expires_at_ns": 0,
                "last_used_at_ns": 0,
            })
            .to_string(),
            enabled: true,
            created_by: "bn005-test".to_string(),
        })
        .await
        .unwrap();

    let routing_key = "bsk_supported_routing_scope";
    let device_key = "bsk_supported_device_scope";
    provision_test_key(
        &store,
        "supported-routing",
        routing_key,
        "routing:write:speaker=192.0.2.1,vrf_rd=65000:100",
    )
    .await;
    provision_test_key(
        &store,
        "supported-device",
        device_key,
        "routing:write:speaker=192.0.2.1",
    )
    .await;
    let web_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let web_addr = web_listener.local_addr().unwrap();
    let web_router = build_public_routing_web_router(Arc::clone(&store));
    tokio::spawn(async move {
        axum::serve(web_listener, web_router).await.unwrap();
    });

    let base = format!("http://{web_addr}");

    // A. Cross-speaker GET /api/routing/v1/routes is rejected
    let cross_spk = client
        .get(format!("{base}/api/routing/v1/routes?speaker=192.0.2.2"))
        .bearer_auth(scoped_api_key)
        .send()
        .await
        .unwrap();
    assert_eq!(cross_spk.status(), reqwest::StatusCode::FORBIDDEN);
    let cross_spk_body: serde_json::Value = cross_spk.json().await.unwrap();
    assert_eq!(cross_spk_body["error"], "routing_scope_violation");
    assert!(
        cross_spk_body["message"]
            .as_str()
            .unwrap()
            .contains("violates token scope '192.0.2.1'")
    );

    // B. Cross-VRF GET /api/routing/v1/routes is rejected
    let cross_vrf = client
        .get(format!("{base}/api/routing/v1/routes?vrf_rd=65000:200"))
        .bearer_auth(scoped_api_key)
        .send()
        .await
        .unwrap();
    assert_eq!(cross_vrf.status(), reqwest::StatusCode::FORBIDDEN);
    let cross_vrf_body: serde_json::Value = cross_vrf.json().await.unwrap();
    assert_eq!(cross_vrf_body["error"], "routing_scope_violation");
    assert!(
        cross_vrf_body["message"]
            .as_str()
            .unwrap()
            .contains("violates token scope '65000:100'")
    );

    // C. Cross-tenant GET /api/routing/v1/routes is rejected
    let cross_tenant = client
        .get(format!("{base}/api/routing/v1/routes?tenant_id=other"))
        .bearer_auth(scoped_api_key)
        .send()
        .await
        .unwrap();
    assert_eq!(cross_tenant.status(), reqwest::StatusCode::FORBIDDEN);

    // A matching tenant label is not an ownership predicate: deny the composite grant.
    let unsupported = client
        .get(format!("{base}/api/routing/v1/routes"))
        .bearer_auth(scoped_api_key)
        .send()
        .await
        .unwrap();
    assert_eq!(unsupported.status(), reqwest::StatusCode::FORBIDDEN);
    // D. Valid GET without parameters succeeds and automatically injects scoped filters
    let valid_get = client
        .get(format!("{base}/api/routing/v1/routes"))
        .bearer_auth(routing_key)
        .send()
        .await
        .unwrap();
    assert_eq!(valid_get.status(), reqwest::StatusCode::OK);

    // E. Cross-speaker POST /api/routing/v1/adjudicate-prefix is rejected
    let cross_adj = client
        .post(format!("{base}/api/routing/v1/adjudicate-prefix"))
        .bearer_auth(scoped_api_key)
        .json(&serde_json::json!({
            "prefix": "192.0.2.0/24",
            "speaker": "192.0.2.2",
            "diagnosis_verdict": "correct"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(cross_adj.status(), reqwest::StatusCode::FORBIDDEN);
    let adj_body: serde_json::Value = cross_adj.json().await.unwrap();
    assert_eq!(adj_body["error"], "routing_scope_violation");

    // F. Cross-speaker POST /api/investigations is rejected
    let cross_inv_create = client
        .post(format!("{base}/api/investigations"))
        .bearer_auth(device_key)
        .json(&serde_json::json!({
            "detection_id": "det-x",
            "device_address": "192.0.2.2",
            "trigger": "operator"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(cross_inv_create.status(), reqwest::StatusCode::FORBIDDEN);

    // G. Cross-speaker GET /api/investigations/{id} is rejected
    let cross_inv_get = client
        .get(format!("{base}/api/investigations/{}", inv2.id))
        .bearer_auth(device_key)
        .send()
        .await
        .unwrap();
    assert_eq!(cross_inv_get.status(), reqwest::StatusCode::FORBIDDEN);

    // H. Scoped speaker GET /api/investigations/{id} succeeds
    let ok_inv_get = client
        .get(format!("{base}/api/investigations/{}", inv1.id))
        .bearer_auth(device_key)
        .send()
        .await
        .unwrap();
    assert_eq!(ok_inv_get.status(), reqwest::StatusCode::OK);

    // I. GET /api/investigations only returns investigations matching the scoped speaker
    let list_invs = client
        .get(format!("{base}/api/investigations"))
        .bearer_auth(device_key)
        .send()
        .await
        .unwrap();
    assert_eq!(list_invs.status(), reqwest::StatusCode::OK);
    let list_body: serde_json::Value = list_invs.json().await.unwrap();
    let returned = list_body["investigations"].as_array().unwrap();
    assert_eq!(returned.len(), 1);
    assert_eq!(returned[0]["id"], inv1.id);
    assert_eq!(returned[0]["device_address"], "192.0.2.1");
}

/// 5. Graph explorer read-only boundaries, comment evasion rejection, and bounded execution.
#[test]
fn test_bn005_graph_explorer_readonly_and_evasion_checks() {
    use bonsai::graph::explorer::validate_query;

    // Direct mutation keywords rejected
    assert!(validate_query("CREATE (n:Device)").is_err());
    assert!(validate_query("MATCH (n) DELETE n").is_err());
    assert!(validate_query("MERGE (n:Device {address: '1.2.3.4'})").is_err());
    assert!(validate_query("MATCH (n) SET n.val = 1").is_err());
    assert!(validate_query("ALTER TABLE Device ADD col STRING").is_err());
    assert!(validate_query("COPY Device FROM 'file.csv'").is_err());
    assert!(validate_query("LOAD FROM 'data.parquet'").is_err());
    assert!(validate_query("FOREACH (x IN [1, 2] | CREATE (n))").is_err());

    // Comment evasion attempts rejected
    assert!(validate_query("/* innocuous */ CREATE (n:Device)").is_err());
    assert!(validate_query("// comment\nDROP TABLE Foo").is_err());
    assert!(validate_query("-- comment\nALTER TABLE Foo DROP col").is_err());
    assert!(validate_query("MATCH (n) /* delete it */ DETACH DELETE n").is_err());

    // Legitimate read queries with comments accepted
    assert!(validate_query("/* read query */ MATCH (d:Device) RETURN d.address").is_ok());
    assert!(validate_query("// get devices\nMATCH (d:Device) RETURN d.address").is_ok());
    assert!(validate_query("-- devices query\nMATCH (d:Device) RETURN d.address").is_ok());
}

/// 6. Event history queries retrieve latest evidence across sequence space without silent truncation.
#[tokio::test]
async fn test_bn005_rib_event_journal_latest_evidence_without_truncation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal_path = dir.path().join("events_scale.jsonl");

    let journal = RibEventJournal::open(Some(&journal_path)).await.unwrap();

    let obs = ObservationSource::new("192.0.2.1".parse().unwrap(), 1);

    // Write 1,200 events across sequence space
    let mut batch = Vec::with_capacity(1200);
    for i in 1..=1200 {
        batch.push(RibEvent {
            schema_version: 1,
            event_key: EventKey::new(obs.clone(), i),
            id: uuid::Uuid::new_v4(),
            occurred_at: chrono::Utc::now(),
            speaker: "192.0.2.1".parse().unwrap(),
            payload: RibEventPayload::SpeakerUp {
                sys_name: Some(format!("router-{i}")),
                sys_descr: Some("FRR".to_string()),
            },
        });
    }
    journal.append_batch(&batch).await.unwrap();

    // A. all_events() retrieves all 1,200 events without truncating at 1,000
    let all = journal.all_events().await;
    assert_eq!(
        all.len(),
        1200,
        "all_events() must return all 1,200 events without hardcoded 1000 limit"
    );

    // B. latest_events(50) retrieves newest events (sequences 1151..=1200)
    let latest_50 = journal.latest_events(50).await;
    assert_eq!(latest_50.len(), 50);
    assert_eq!(latest_50.last().unwrap().event_key.sequence, 1200);
    assert_eq!(latest_50.first().unwrap().event_key.sequence, 1151);

    // C. Internal routing API /internal/routing/v1/events with chunked scanning
    let internal_token = "event-test-token";
    let routing_cfg = RoutingServiceConfig {
        bmp_listen_addr: "127.0.0.1:0".to_string(),
        http_listen_addr: "127.0.0.1:0".to_string(),
        bmp_journal_path: Some(dir.path().join("bmp.bin")),
        event_journal_path: Some(journal_path),
        internal_service_token: Some(internal_token.to_string()),
        identity_db_path: None,
        identity_scope: None,
        ..RoutingServiceConfig::default()
    };
    let state = RoutingServiceState::new(routing_cfg).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, build_internal_routing_router(state))
            .await
            .unwrap();
    });

    let client = reqwest::Client::new();
    let resp = client
        .get(format!(
            "http://{addr}/internal/routing/v1/events?limit=2000"
        ))
        .header("x-bonsai-routing-token", internal_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body: serde_json::Value = resp.json().await.unwrap();
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 1200);
    assert_eq!(body["truncated"], false);
    assert_eq!(body["scanned_to_seq"], 1200);
}

/// 7. Unauthenticated MCP JSON-RPC requests are strictly rejected with HTTP 401 when auth is required.
#[tokio::test]
async fn test_bn005_anonymous_mcp_access_is_denied() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph.db").to_str().unwrap(),
            128 * 1024 * 1024,
        )
        .unwrap(),
    );
    // Preserve the original BN-005 acceptance contract in explicit strict mode.
    set_app_config(&store, "ownership_access_mode", "strict").await;
    set_app_config(&store, "require_auth", "\"1\"").await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let web_router = build_public_routing_web_router(Arc::clone(&store));
    tokio::spawn(async move {
        axum::serve(listener, web_router).await.unwrap();
    });

    let client = reqwest::Client::new();
    // 1. /api/routing/v1/health is protected
    let r1 = client
        .get(format!("http://{addr}/api/routing/v1/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), reqwest::StatusCode::UNAUTHORIZED);

    // 2. /mcp query_graph without credentials must be rejected with 401
    let r2 = client
        .post(format!("http://{addr}/mcp"))
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "query_graph",
                "arguments": {"cypher": "RETURN 42 AS answer"}
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), reqwest::StatusCode::UNAUTHORIZED);

    // 3. /mcp mutation probe without credentials must be rejected with 401
    let r3 = client
        .post(format!("http://{addr}/mcp"))
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "query_graph",
                "arguments": {
                    "cypher": "WITH '/*' AS marker CREATE (n:Device {address: 'bn005-anonymous-created'}) RETURN n.address"
                }
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r3.status(), reqwest::StatusCode::UNAUTHORIZED);

    // 4. Verify no node was created in the database
    let check = bonsai::mcp_server::call_tool_with_store_and_routing(
        &store,
        None,
        "query_graph",
        &serde_json::json!({"cypher": "MATCH (n:Device {address: 'bn005-anonymous-created'}) RETURN n.address"}),
     &bonsai::mcp_server::ToolExecutionContext::global(bonsai::http_server::auth::Role::Admin),)
    .await
    .unwrap();
    assert_eq!(check["row_count"], 0);
}

/// 8. Cypher comment stripping respects string literals and prevents mutation evasion via comments.
#[tokio::test]
async fn test_bn005_quote_comment_mutation_evasion_is_rejected_and_graph_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph.db").to_str().unwrap(),
            128 * 1024 * 1024,
        )
        .unwrap(),
    );
    // Preserve the original BN-005 acceptance contract in explicit strict mode.
    set_app_config(&store, "ownership_access_mode", "strict").await;

    let query =
        "WITH '/*' AS marker CREATE (n:Device {address: 'bn005-audit-created'}) RETURN n.address";
    assert!(bonsai::graph::explorer::validate_query(query).is_err());
    assert!(!bonsai::mcp_server::is_readonly_cypher(query));

    let res = bonsai::mcp_server::call_tool_with_store_and_routing(
        &store,
        None,
        "query_graph",
        &serde_json::json!({"cypher": query}),
        &bonsai::mcp_server::ToolExecutionContext::global(bonsai::http_server::auth::Role::Admin),
    )
    .await;
    assert!(
        res.is_err(),
        "Mutation via quoted comment evasion must be rejected"
    );

    let check = bonsai::mcp_server::call_tool_with_store_and_routing(
        &store,
        None,
        "query_graph",
        &serde_json::json!({"cypher": "MATCH (n:Device {address: 'bn005-audit-created'}) RETURN n.address"}),
     &bonsai::mcp_server::ToolExecutionContext::global(bonsai::http_server::auth::Role::Admin),)
    .await
    .unwrap();
    assert_eq!(
        check["row_count"], 0,
        "No device should have been created in the database"
    );
}

/// 9. Conflicting speaker aliases ("speaker" vs "speaker_addr") are rejected, preventing unauthorized exporter ingestion.
#[tokio::test]
async fn test_bn005_conflicting_speaker_aliases_are_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph.db").to_str().unwrap(),
            128 * 1024 * 1024,
        )
        .unwrap(),
    );
    // Preserve the original BN-005 acceptance contract in explicit strict mode.
    set_app_config(&store, "ownership_access_mode", "strict").await;
    set_app_config(&store, "require_auth", "\"1\"").await;

    let routing_cfg = RoutingServiceConfig {
        bmp_listen_addr: "127.0.0.1:0".to_string(),
        http_listen_addr: "127.0.0.1:0".to_string(),
        bmp_journal_path: Some(dir.path().join("bmp.journal")),
        event_journal_path: Some(dir.path().join("events.journal")),
        internal_service_token: Some("audit-internal".to_string()),
        identity_db_path: None,
        identity_scope: None,
        ..RoutingServiceConfig::default()
    };
    let state = RoutingServiceState::new(routing_cfg).await.unwrap();
    let internal_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_url = format!("http://{}", internal_listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(internal_listener, build_internal_routing_router(state))
            .await
            .unwrap();
    });

    set_app_config(&store, "routing_service_url", &format!("\"{backend_url}\"")).await;
    set_app_config(&store, "routing_service_token", "\"audit-internal\"").await;

    let key = "bsk_audit_alias_key";
    let key_hash = hex::encode(Sha256::digest(key.as_bytes()));
    store
        .upsert_config_item(ConfigItemRecord {
            id: "api-key-alias".to_string(),
            config_class: "api_key".to_string(),
            vendor: String::new(),
            name: "test-alias-key".to_string(),
            version: "1".to_string(),
            content_json: serde_json::json!({
                "key_hash": key_hash,
                "scope": "routing:write:speaker=192.0.2.1,vrf_rd=0:0,tenant_id=acme",
                "expires_at_ns": 0,
                "last_used_at_ns": 0,
            })
            .to_string(),
            enabled: true,
            created_by: "bn005-test".to_string(),
        })
        .await
        .unwrap();

    let public_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let public_addr = public_listener.local_addr().unwrap();
    let web_router = build_public_routing_web_router(Arc::clone(&store));
    tokio::spawn(async move {
        axum::serve(public_listener, web_router).await.unwrap();
    });

    let client = reqwest::Client::new();
    let (init, _, _) =
        sample_bmp_session_frames(Ipv4Addr::new(192, 0, 2, 1), 65001, [192, 0, 2], 100);

    // Attempt ingest with conflicting aliases: speaker=192.0.2.1, speaker_addr=192.0.2.2
    let resp = client
        .post(format!("http://{public_addr}/api/routing/v1/ingest-bmp"))
        .bearer_auth(key)
        .json(&serde_json::json!({
            "speaker": "192.0.2.1",
            "speaker_addr": "192.0.2.2",
            "vrf_rd": "0:0",
            "session_epoch": 1,
            "frame_hex": hex::encode(init),
        }))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error(),
        "Conflicting speaker aliases must be rejected with client error, got: {}",
        resp.status()
    );

    // Verify backend internal state: exporter 192.0.2.2 must NOT have been ingested
    let speakers_resp = client
        .get(format!("{backend_url}/internal/routing/v1/speakers"))
        .header("x-bonsai-routing-token", "audit-internal")
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(
        !speakers_resp.to_string().contains("192.0.2.2"),
        "Exporter 192.0.2.2 must not exist in internal speakers state: {speakers_resp}"
    );
}

/// 10. Wire BMP frame RD mismatch with token scope is rejected before ingestion into RIB.
#[tokio::test]
async fn test_bn005_decoded_bmp_vrf_mismatch_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph.db").to_str().unwrap(),
            128 * 1024 * 1024,
        )
        .unwrap(),
    );
    // Preserve the original BN-005 acceptance contract in explicit strict mode.
    set_app_config(&store, "ownership_access_mode", "strict").await;
    set_app_config(&store, "require_auth", "\"1\"").await;

    let routing_cfg = RoutingServiceConfig {
        bmp_listen_addr: "127.0.0.1:0".to_string(),
        http_listen_addr: "127.0.0.1:0".to_string(),
        bmp_journal_path: Some(dir.path().join("bmp.journal")),
        event_journal_path: Some(dir.path().join("events.journal")),
        internal_service_token: Some("audit-internal".to_string()),
        identity_db_path: None,
        identity_scope: None,
        ..RoutingServiceConfig::default()
    };
    let state = RoutingServiceState::new(routing_cfg).await.unwrap();
    let internal_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_url = format!("http://{}", internal_listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(internal_listener, build_internal_routing_router(state))
            .await
            .unwrap();
    });

    set_app_config(&store, "routing_service_url", &format!("\"{backend_url}\"")).await;
    set_app_config(&store, "routing_service_token", "\"audit-internal\"").await;

    let key = "bsk_audit_vrf_key";
    let key_hash = hex::encode(Sha256::digest(key.as_bytes()));
    store
        .upsert_config_item(ConfigItemRecord {
            id: "api-key-vrf".to_string(),
            config_class: "api_key".to_string(),
            vendor: String::new(),
            name: "test-vrf-key".to_string(),
            version: "1".to_string(),
            content_json: serde_json::json!({
                "key_hash": key_hash,
                "scope": "routing:write:speaker=192.0.2.1,vrf_rd=0:0,tenant_id=acme",
                "expires_at_ns": 0,
                "last_used_at_ns": 0,
            })
            .to_string(),
            enabled: true,
            created_by: "bn005-test".to_string(),
        })
        .await
        .unwrap();

    let global_key = "bsk_trusted_exporter_ingest";
    provision_test_key(&store, "global-ingest", global_key, "routing:write").await;
    let public_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let public_addr = public_listener.local_addr().unwrap();
    let web_router = build_public_routing_web_router(Arc::clone(&store));
    tokio::spawn(async move {
        axum::serve(public_listener, web_router).await.unwrap();
    });

    let client = reqwest::Client::new();
    let frames = build_rd_bmp_frames(
        Ipv4Addr::new(10, 100, 1, 1),
        65002,
        65000,
        100,
        [10, 99, 1],
        100,
    );

    // Global ingestion authority is explicit; restricted keys cannot reset an exporter.
    let r_init = client
        .post(format!("http://{public_addr}/api/routing/v1/ingest-bmp"))
        .bearer_auth(global_key)
        .json(&serde_json::json!({
            "speaker_addr": "192.0.2.1",
            "vrf_rd": "0:0",
            "frame_hex": hex::encode(&frames.0),
            "session_epoch": 1,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r_init.status(), reqwest::StatusCode::OK);

    // Attempt to ingest PeerUp, RouteMonitoring, EndOfRib carrying RD 65000:100 while token scope is vrf_rd=0:0
    for frame in [&frames.1, &frames.2, &frames.3] {
        let resp = client
            .post(format!("http://{public_addr}/api/routing/v1/ingest-bmp"))
            .bearer_auth(key)
            .json(&serde_json::json!({
                "speaker_addr": "192.0.2.1",
                "vrf_rd": "0:0",
                "frame_hex": hex::encode(frame),
                "session_epoch": 1,
            }))
            .send()
            .await
            .unwrap();
        assert!(
            resp.status().is_client_error(),
            "Frame with RD 65000:100 must be rejected under vrf_rd=0:0 token scope"
        );
    }

    // Verify backend internal state: routes query for 65000:100 must NOT find 10.99.1.0/24
    let routes_resp = client
        .get(format!(
            "{backend_url}/internal/routing/v1/routes?vrf_rd=65000:100"
        ))
        .header("x-bonsai-routing-token", "audit-internal")
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(
        !routes_resp.to_string().contains("10.99.1.0/24"),
        "Route 10.99.1.0/24 must not exist in VRF 65000:100: {routes_resp}"
    );

    let denied_own = client
        .post(format!("http://{public_addr}/api/routing/v1/ingest-bmp"))
        .bearer_auth(key)
        .json(
            &serde_json::json!({"speaker_addr":"192.0.2.1", "vrf_rd":"0:0",
            "frame_hex":hex::encode(&frames.0), "session_epoch":1}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(denied_own.status(), reqwest::StatusCode::FORBIDDEN);
    // Also verify permitted ingest: frames carrying permitted RD 0:0 succeed
    let (_p_init, p_up, _p_rm) =
        sample_bmp_session_frames(Ipv4Addr::new(192, 0, 2, 1), 65001, [192, 0, 2], 100);
    let r_perm = client
        .post(format!("http://{public_addr}/api/routing/v1/ingest-bmp"))
        .bearer_auth(global_key)
        .json(&serde_json::json!({
            "speaker_addr": "192.0.2.1",
            "vrf_rd": "0:0",
            "frame_hex": hex::encode(p_up),
            "session_epoch": 1,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r_perm.status(),
        reqwest::StatusCode::OK,
        "Explicit global ingestion authority must succeed"
    );
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

/// 11. Authenticated MCP tool scope enforcement:
/// - Scoped key cannot run arbitrary Cypher via `query_graph`
/// - Scoped key cannot read foreign device snapshots via `get_changes` (neither by specifying foreign address nor omitting filter)
/// - Scoped key can read own device snapshots via `get_changes`
/// - Scoped key cannot query foreign device blast radius via `get_device_blast_radius`
/// - Scoped key cannot read foreign incident details via `get_incident`
/// - Scoped key querying devices via `query_devices` only sees own device
/// - Scoped key listing detections via `list_active_detections` only sees own device
/// - Global viewer key can query graph
/// - ApiReadonly key cannot evaluate change plan or query graph
/// - Scoped operator key cannot evaluate plan targeting foreign device/vrf/tenant
#[tokio::test]
async fn test_bn005_authenticated_mcp_scoped_tool_authorization_and_denial() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph.db").to_str().unwrap(),
            128 * 1024 * 1024,
        )
        .expect("open GraphStore"),
    );
    // Preserve the original BN-005 acceptance contract in explicit strict mode.
    set_app_config(&store, "ownership_access_mode", "strict").await;
    set_app_config(&store, "require_auth", "1").await;

    // Seed database with devices, config snapshots, and detections
    let db = store.db();
    {
        let conn = lbug::Connection::new(&db).unwrap();
        conn.query("CREATE (d:Device {address:'192.0.2.1', hostname:'edge-1', vendor:'arista'})")
            .unwrap();
        conn.query("CREATE (d:Device {address:'192.0.2.2', hostname:'edge-2', vendor:'cisco'})")
            .unwrap();
        conn.query("CREATE (c:ConfigSnapshot {id:'snap-own', device_address:'192.0.2.1', trigger:'manual', summary:'own-change', captured_at:'2026-10-14T10:00:00Z'})")
            .unwrap();
        conn.query("CREATE (c:ConfigSnapshot {id:'snap-foreign', device_address:'192.0.2.2', trigger:'audit', summary:'foreign-change', captured_at:'2026-10-14T10:05:00Z'})")
            .unwrap();
        conn.query("CREATE (de:DetectionEvent {id:'det-own-001', device_address:'192.0.2.1', rule_id:'bgp_session_down', severity:'critical', features_json:'{}', source_types:'[]', latency_ns:0, fired_at:cast('2026-10-14 10:00:00', 'TIMESTAMP_NS')})")
            .unwrap();
        conn.query("CREATE (de:DetectionEvent {id:'det-foreign-002', device_address:'192.0.2.2', rule_id:'bgp_session_down', severity:'critical', features_json:'{}', source_types:'[]', latency_ns:0, fired_at:cast('2026-10-14 10:05:00', 'TIMESTAMP_NS')})")
            .unwrap();
    }

    // 1. Supported device scope: speaker-only. Composite grants are denied by the matrix test.
    let scoped_key = "bsk_scoped_test_12345";
    store
        .upsert_config_item(ConfigItemRecord {
            id: "apikey-scoped".to_string(),
            config_class: "api_key".to_string(),
            vendor: String::new(),
            name: "scoped-key".to_string(),
            version: "1".to_string(),
            content_json: serde_json::json!({
                "key_hash": hex::encode(Sha256::digest(scoped_key.as_bytes())),
                "scope": "routing:read:speaker=192.0.2.1",
                "expires_at_ns": 0,
                "last_used_at_ns": 0,
            })
            .to_string(),
            enabled: true,
            created_by: "test".to_string(),
        })
        .await
        .unwrap();

    // 2. Global viewer key
    let viewer_key = "bsk_viewer_global_12345";
    store
        .upsert_config_item(ConfigItemRecord {
            id: "apikey-viewer".to_string(),
            config_class: "api_key".to_string(),
            vendor: String::new(),
            name: "viewer-key".to_string(),
            version: "1".to_string(),
            content_json: serde_json::json!({
                "key_hash": hex::encode(Sha256::digest(viewer_key.as_bytes())),
                "scope": "viewer",
                "expires_at_ns": 0,
                "last_used_at_ns": 0,
            })
            .to_string(),
            enabled: true,
            created_by: "test".to_string(),
        })
        .await
        .unwrap();

    // 3. ApiReadonly key
    let readonly_key = "bsk_readonly_api_12345";
    store
        .upsert_config_item(ConfigItemRecord {
            id: "apikey-readonly".to_string(),
            config_class: "api_key".to_string(),
            vendor: String::new(),
            name: "readonly-key".to_string(),
            version: "1".to_string(),
            content_json: serde_json::json!({
                "key_hash": hex::encode(Sha256::digest(readonly_key.as_bytes())),
                "scope": "api_readonly",
                "expires_at_ns": 0,
                "last_used_at_ns": 0,
            })
            .to_string(),
            enabled: true,
            created_by: "test".to_string(),
        })
        .await
        .unwrap();

    // 4. Scoped operator key: speaker=192.0.2.1, vrf_rd=0:0, tenant_id=acme
    let operator_key = "bsk_operator_scoped_12345";
    store
        .upsert_config_item(ConfigItemRecord {
            id: "apikey-operator".to_string(),
            config_class: "api_key".to_string(),
            vendor: String::new(),
            name: "operator-key".to_string(),
            version: "1".to_string(),
            content_json: serde_json::json!({
                "key_hash": hex::encode(Sha256::digest(operator_key.as_bytes())),
                "scope": "routing:operator:speaker=192.0.2.1,vrf_rd=0:0,tenant_id=acme",
                "expires_at_ns": 0,
                "last_used_at_ns": 0,
            })
            .to_string(),
            enabled: true,
            created_by: "test".to_string(),
        })
        .await
        .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let web_router = build_public_routing_web_router(Arc::clone(&store));
    tokio::spawn(async move {
        axum::serve(listener, web_router).await.unwrap();
    });

    let client = reqwest::Client::new();
    let mcp_url = format!("http://{addr}/mcp");

    // Helper closure for calling MCP
    let call_mcp = |key: &str, tool: &str, args: serde_json::Value| {
        let client = client.clone();
        let url = mcp_url.clone();
        let key = key.to_string();
        let tool = tool.to_string();
        async move {
            let resp = client
                .post(&url)
                .bearer_auth(&key)
                .json(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": tool,
                        "arguments": args
                    }
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), reqwest::StatusCode::OK);
            resp.json::<serde_json::Value>().await.unwrap()
        }
    };

    // A. query_graph is DENIED for resource-scoped key
    let res_qg = call_mcp(
        scoped_key,
        "query_graph",
        serde_json::json!({"cypher": "MATCH (d:Device {address:'192.0.2.2'}) RETURN d.address"}),
    )
    .await;
    assert!(
        res_qg.get("error").is_some(),
        "query_graph must return JSON-RPC error for scoped key: {res_qg}"
    );
    assert_eq!(res_qg["error"]["code"], -32000);
    assert!(
        res_qg["error"]["message"]
            .as_str()
            .unwrap()
            .contains("routing_scope_violation"),
        "Expected routing_scope_violation error, got: {res_qg}"
    );
    assert!(res_qg.get("result").is_none());

    // B1. get_changes for foreign device is DENIED
    let res_gc_foreign = call_mcp(
        scoped_key,
        "get_changes",
        serde_json::json!({"device_address": "192.0.2.2"}),
    )
    .await;
    assert!(
        res_gc_foreign.get("error").is_some(),
        "get_changes for 192.0.2.2 must be denied: {res_gc_foreign}"
    );
    assert_eq!(res_gc_foreign["error"]["code"], -32000);
    assert!(
        res_gc_foreign["error"]["message"]
            .as_str()
            .unwrap()
            .contains("routing_scope_violation"),
        "Expected routing_scope_violation error, got: {res_gc_foreign}"
    );

    // B2. get_changes for own device succeeds and only returns own-change
    let res_gc_own = call_mcp(
        scoped_key,
        "get_changes",
        serde_json::json!({"device_address": "192.0.2.1"}),
    )
    .await;
    assert!(
        res_gc_own.get("result").is_some(),
        "get_changes for own device must succeed: {res_gc_own}"
    );
    let text_own = res_gc_own.to_string();
    assert!(
        text_own.contains("own-change"),
        "Expected own-change in: {text_own}"
    );
    assert!(
        !text_own.contains("foreign-change"),
        "Must not leak foreign-change in: {text_own}"
    );

    // B3. get_changes with omitted filter defaults to own device
    let res_gc_default = call_mcp(scoped_key, "get_changes", serde_json::json!({})).await;
    assert!(
        res_gc_default.get("result").is_some(),
        "get_changes with empty args must succeed: {res_gc_default}"
    );
    let text_default = res_gc_default.to_string();
    assert!(
        text_default.contains("own-change"),
        "Expected own-change in: {text_default}"
    );
    assert!(
        !text_default.contains("foreign-change"),
        "Must not leak foreign-change in: {text_default}"
    );

    // C1. get_device_blast_radius for foreign device is DENIED
    let res_br_foreign = call_mcp(
        scoped_key,
        "get_device_blast_radius",
        serde_json::json!({"address": "192.0.2.2"}),
    )
    .await;
    assert!(
        res_br_foreign.get("error").is_some(),
        "blast radius for 192.0.2.2 must be denied: {res_br_foreign}"
    );
    assert!(
        res_br_foreign["error"]["message"]
            .as_str()
            .unwrap()
            .contains("routing_scope_violation")
    );

    // C2. get_device_blast_radius for own device SUCCEEDS
    let res_br_own = call_mcp(
        scoped_key,
        "get_device_blast_radius",
        serde_json::json!({"address": "192.0.2.1"}),
    )
    .await;
    assert!(
        res_br_own.get("result").is_some(),
        "blast radius for own device must succeed: {res_br_own}"
    );

    // D1. get_incident for foreign detection is DENIED
    let res_inc_foreign = call_mcp(
        scoped_key,
        "get_incident",
        serde_json::json!({"id": "det-foreign-002"}),
    )
    .await;
    assert!(
        res_inc_foreign.get("error").is_some(),
        "incident for foreign device must be denied: {res_inc_foreign}"
    );
    assert!(
        res_inc_foreign["error"]["message"]
            .as_str()
            .unwrap()
            .contains("routing_scope_violation")
    );

    // D2. get_incident for own detection SUCCEEDS
    let res_inc_own = call_mcp(
        scoped_key,
        "get_incident",
        serde_json::json!({"id": "det-own-001"}),
    )
    .await;
    assert!(
        res_inc_own.get("result").is_some(),
        "incident for own device must succeed: {res_inc_own}"
    );
    assert!(res_inc_own.to_string().contains("192.0.2.1"));

    // E. query_devices only returns own device
    let res_qd = call_mcp(scoped_key, "query_devices", serde_json::json!({})).await;
    assert!(
        res_qd.get("result").is_some(),
        "query_devices must succeed: {res_qd}"
    );
    let text_qd = res_qd.to_string();
    assert!(
        text_qd.contains("192.0.2.1"),
        "Expected own device in: {text_qd}"
    );
    assert!(
        !text_qd.contains("192.0.2.2"),
        "Must not leak foreign device in: {text_qd}"
    );

    // F. list_active_detections only returns own detections
    let res_ad = call_mcp(scoped_key, "list_active_detections", serde_json::json!({})).await;
    assert!(
        res_ad.get("result").is_some(),
        "list_active_detections must succeed: {res_ad}"
    );
    let text_ad = res_ad.to_string();
    assert!(
        text_ad.contains("det-own-001"),
        "Expected det-own-001 in: {text_ad}"
    );
    assert!(
        !text_ad.contains("det-foreign-002"),
        "Must not leak foreign detection in: {text_ad}"
    );

    // G. Global viewer key can query graph
    let res_qg_viewer = call_mcp(
        viewer_key,
        "query_graph",
        serde_json::json!({"cypher": "MATCH (d:Device) RETURN d.address ORDER BY d.address"}),
    )
    .await;
    assert!(
        res_qg_viewer.get("result").is_some(),
        "global viewer must be allowed to run query_graph: {res_qg_viewer}"
    );
    let text_qg = res_qg_viewer.to_string();
    assert!(text_qg.contains("192.0.2.1") && text_qg.contains("192.0.2.2"));

    // H1. ApiReadonly key calling query_graph is DENIED with insufficient_permissions
    let res_ro_qg = call_mcp(
        readonly_key,
        "query_graph",
        serde_json::json!({"cypher": "MATCH (d:Device) RETURN d.address"}),
    )
    .await;
    assert!(
        res_ro_qg.get("error").is_some(),
        "ApiReadonly must be denied query_graph: {res_ro_qg}"
    );
    assert!(
        res_ro_qg["error"]["message"]
            .as_str()
            .unwrap()
            .contains("insufficient_permissions")
    );

    // H2. ApiReadonly key calling evaluate_change_plan is DENIED with insufficient_permissions
    let res_ro_plan = call_mcp(
        readonly_key,
        "evaluate_change_plan",
        serde_json::json!({"plan": {}, "digital_twin_snapshot": {}}),
    )
    .await;
    assert!(
        res_ro_plan.get("error").is_some(),
        "ApiReadonly must be denied evaluate_change_plan: {res_ro_plan}"
    );
    assert!(
        res_ro_plan["error"]["message"]
            .as_str()
            .unwrap()
            .contains("insufficient_permissions")
    );

    // I1. Scoped operator key calling evaluate_change_plan with foreign device plan is DENIED
    let sample_twin = serde_json::json!({
        "lab_topology_id": "lab-topo-1",
        "nos_image_ref": "cisco-8000:latest",
        "target_prefix_reachable_before": true,
        "target_prefix_reachable_after": true,
        "ebgp_peers_up_before": 2,
        "ebgp_peers_up_after": 2,
        "vrf_isolation_intact": true,
        "max_allowed_blast_radius_devices": 5,
        "unsupported_vendor_constructs": []
    });
    let foreign_plan = serde_json::json!({
        "plan_id": "plan-foreign",
        "tenant_id": "acme",
        "device_address": "192.0.2.2",
        "vrf_rd": "0:0",
        "nos_vendor": "cisco",
        "baseline_config_revision": "rev1",
        "candidate_diff": "+interface Loopback10\n",
        "intended_behavior": "test",
        "blast_radius": {
            "affected_devices": ["192.0.2.2"],
            "affected_vrfs": ["0:0"],
            "affected_ebgp_peers": [],
            "estimated_affected_prefixes": 1,
            "expected_disruption": "none_expected"
        },
        "invariants": [],
        "prechecks": [],
        "postchecks": [],
        "recovery_policy": "explicit_rollback_steps",
        "rollback_steps": [],
        "created_at_epoch_secs": 1700000000,
        "expires_at_epoch_secs": 1700003600
    });
    let res_op_foreign = call_mcp(
        operator_key,
        "evaluate_change_plan",
        serde_json::json!({
            "plan": foreign_plan,
            "digital_twin_snapshot": sample_twin
        }),
    )
    .await;
    assert!(
        res_op_foreign.get("error").is_some(),
        "operator plan with foreign device must be denied: {res_op_foreign}"
    );
    assert!(
        res_op_foreign["error"]["message"]
            .as_str()
            .unwrap()
            .contains("routing_scope_violation")
    );

    // I2. Scoped operator key calling evaluate_change_plan with own device plan SUCCEEDS
    let mut own_plan = foreign_plan.clone();
    own_plan["device_address"] = serde_json::json!("192.0.2.1");
    let res_op_own = call_mcp(
        operator_key,
        "evaluate_change_plan",
        serde_json::json!({
            "plan": own_plan,
            "digital_twin_snapshot": sample_twin
        }),
    )
    .await;
    assert!(
        res_op_own.get("result").is_some(),
        "operator plan with own device must succeed: {res_op_own}"
    );
}

/// 12. Strict scope grant parsing, default-deny on unsupported tool/scope combinations,
/// unscoped HTTP route escape prevention, and engine-level read-only transaction enforcement.
#[tokio::test]
async fn test_bn005_strict_scope_parsing_default_deny_and_readonly_transactions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph_scope.db").to_str().unwrap(),
            256 * 1024 * 1024,
        )
        .unwrap(),
    );
    // Preserve the original BN-005 acceptance contract in explicit strict mode.
    set_app_config(&store, "ownership_access_mode", "strict").await;

    set_app_config(&store, "require_auth", "1").await;

    // Seed devices and detections in GraphStore
    let seed_devices = vec![
        ("192.0.2.1", "leaf-1", "arista"),
        ("192.0.2.2", "leaf-2", "cisco"),
    ];
    for (addr, host, vendor) in seed_devices {
        store
            .upsert_config_item(ConfigItemRecord {
                id: format!("device-{addr}"),
                config_class: "device".to_string(),
                vendor: vendor.to_string(),
                name: host.to_string(),
                version: "1".to_string(),
                content_json: serde_json::json!({
                    "address": addr,
                    "hostname": host,
                    "vendor": vendor,
                })
                .to_string(),
                enabled: true,
                created_by: "bn005-test".to_string(),
            })
            .await
            .unwrap();
    }

    // Provision test API keys:
    // 1. Malformed scope: unknown field
    let malformed_key_1 = "bsk_malformed_scope_unknown_12345678";
    let hash_m1 = hex::encode(Sha256::digest(malformed_key_1.as_bytes()));
    store
        .upsert_config_item(ConfigItemRecord {
            id: "apikey-m1".to_string(),
            config_class: "api_key".to_string(),
            vendor: String::new(),
            name: "malformed-1".to_string(),
            version: "1".to_string(),
            content_json: serde_json::json!({
                "key_hash": hash_m1,
                "scope": "routing:read:speaker=192.0.2.1,unknown=value",
                "expires_at_ns": 0,
                "last_used_at_ns": 0,
            })
            .to_string(),
            enabled: true,
            created_by: "bn005-test".to_string(),
        })
        .await
        .unwrap();

    // 2. Malformed scope: empty value
    let malformed_key_2 = "bsk_malformed_scope_empty_12345678";
    let hash_m2 = hex::encode(Sha256::digest(malformed_key_2.as_bytes()));
    store
        .upsert_config_item(ConfigItemRecord {
            id: "apikey-m2".to_string(),
            config_class: "api_key".to_string(),
            vendor: String::new(),
            name: "malformed-2".to_string(),
            version: "1".to_string(),
            content_json: serde_json::json!({
                "key_hash": hash_m2,
                "scope": "routing:read:speaker=",
                "expires_at_ns": 0,
                "last_used_at_ns": 0,
            })
            .to_string(),
            enabled: true,
            created_by: "bn005-test".to_string(),
        })
        .await
        .unwrap();

    // 3. Tenant-only scoped key
    let tenant_only_key = "bsk_tenant_only_scoped_12345678";
    let hash_to = hex::encode(Sha256::digest(tenant_only_key.as_bytes()));
    store
        .upsert_config_item(ConfigItemRecord {
            id: "apikey-to".to_string(),
            config_class: "api_key".to_string(),
            vendor: String::new(),
            name: "tenant-only".to_string(),
            version: "1".to_string(),
            content_json: serde_json::json!({
                "key_hash": hash_to,
                "scope": "routing:read:tenant_id=acme",
                "expires_at_ns": 0,
                "last_used_at_ns": 0,
            })
            .to_string(),
            enabled: true,
            created_by: "bn005-test".to_string(),
        })
        .await
        .unwrap();

    // 4. VRF-only scoped key
    let vrf_only_key = "bsk_vrf_only_scoped_12345678";
    let hash_vo = hex::encode(Sha256::digest(vrf_only_key.as_bytes()));
    store
        .upsert_config_item(ConfigItemRecord {
            id: "apikey-vo".to_string(),
            config_class: "api_key".to_string(),
            vendor: String::new(),
            name: "vrf-only".to_string(),
            version: "1".to_string(),
            content_json: serde_json::json!({
                "key_hash": hash_vo,
                "scope": "routing:read:vrf_rd=65000:100",
                "expires_at_ns": 0,
                "last_used_at_ns": 0,
            })
            .to_string(),
            enabled: true,
            created_by: "bn005-test".to_string(),
        })
        .await
        .unwrap();

    // 5. Speaker-only scoped key
    let speaker_scoped_key = "bsk_speaker_scoped_12345678";
    let hash_spk = hex::encode(Sha256::digest(speaker_scoped_key.as_bytes()));
    store
        .upsert_config_item(ConfigItemRecord {
            id: "apikey-spk".to_string(),
            config_class: "api_key".to_string(),
            vendor: String::new(),
            name: "speaker-scoped".to_string(),
            version: "1".to_string(),
            content_json: serde_json::json!({
                "key_hash": hash_spk,
                "scope": "routing:read:speaker=192.0.2.1",
                "expires_at_ns": 0,
                "last_used_at_ns": 0,
            })
            .to_string(),
            enabled: true,
            created_by: "bn005-test".to_string(),
        })
        .await
        .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let web_router = bonsai::http_server::build_public_routing_web_router(Arc::clone(&store));
    tokio::spawn(async move {
        axum::serve(listener, web_router).await.unwrap();
    });

    let client = reqwest::Client::new();
    let mcp_url = format!("http://{addr}/mcp");

    let call_mcp_raw = |key: &str, tool: &str, args: serde_json::Value| {
        let client = client.clone();
        let url = mcp_url.clone();
        let key = key.to_string();
        let tool = tool.to_string();
        async move {
            client
                .post(&url)
                .bearer_auth(&key)
                .json(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": tool,
                        "arguments": args
                    }
                }))
                .send()
                .await
                .unwrap()
        }
    };

    // --- Subaction 1 & 2: Malformed scope grants fail closed with HTTP 403 ---
    let resp_m1 = call_mcp_raw(malformed_key_1, "query_devices", serde_json::json!({})).await;
    assert_eq!(
        resp_m1.status(),
        reqwest::StatusCode::FORBIDDEN,
        "Malformed scope with unknown key must be rejected with HTTP 403"
    );

    let resp_m2 = call_mcp_raw(malformed_key_2, "query_devices", serde_json::json!({})).await;
    assert_eq!(
        resp_m2.status(),
        reqwest::StatusCode::FORBIDDEN,
        "Malformed scope with empty value must be rejected with HTTP 403"
    );

    // --- Subaction 2: Tenant-only and VRF-only keys fail closed on device-bound tools ---
    for (key_name, key) in &[("tenant_only", tenant_only_key), ("vrf_only", vrf_only_key)] {
        for tool in &[
            "query_devices",
            "get_changes",
            "get_device_blast_radius",
            "list_active_detections",
            "get_incident",
        ] {
            let resp = call_mcp_raw(key, tool, serde_json::json!({})).await;
            assert_eq!(resp.status(), reqwest::StatusCode::OK);
            let body: serde_json::Value = resp.json().await.unwrap();
            assert!(
                body.get("error").is_some(),
                "{key_name} key calling {tool} must return JSON-RPC error: {body}"
            );
            assert_eq!(body["error"]["code"], -32000);
            let msg = body["error"]["message"].as_str().unwrap();
            assert!(
                msg.contains("routing_scope_violation")
                    && msg.contains("cannot establish ownership"),
                "Expected ownership failure message for {key_name} on {tool}, got: {msg}"
            );
        }
    }

    // Tenant-only key calling compare_snapshots fails closed (compare_snapshots does not support tenant_id)
    let resp_cs_to =
        call_mcp_raw(tenant_only_key, "compare_snapshots", serde_json::json!({})).await;
    assert_eq!(resp_cs_to.status(), reqwest::StatusCode::OK);
    let body_cs_to: serde_json::Value = resp_cs_to.json().await.unwrap();
    assert!(body_cs_to.get("error").is_some());
    assert_eq!(body_cs_to["error"]["code"], -32000);
    assert!(
        body_cs_to["error"]["message"]
            .as_str()
            .unwrap()
            .contains("does not support tenant-only scoping")
    );

    // Unknown tool called with restricted key fails closed
    let resp_unk = call_mcp_raw(
        speaker_scoped_key,
        "unknown_tool_xyz",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(resp_unk.status(), reqwest::StatusCode::OK);
    let body_unk: serde_json::Value = resp_unk.json().await.unwrap();
    assert!(body_unk.get("error").is_some());
    assert_eq!(body_unk["error"]["code"], -32000);
    assert!(
        body_unk["error"]["message"]
            .as_str()
            .unwrap()
            .contains("routing_scope_violation")
    );

    // --- Subaction 2: Client-supplied _scoped_* arguments are stripped ---
    let resp_spoof = call_mcp_raw(
        speaker_scoped_key,
        "query_devices",
        serde_json::json!({ "_scoped_speaker": "192.0.2.2" }),
    )
    .await;
    assert_eq!(resp_spoof.status(), reqwest::StatusCode::OK);
    let body_spoof: serde_json::Value = resp_spoof.json().await.unwrap();
    assert!(body_spoof.get("result").is_some());
    let text = body_spoof["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    // Must contain 192.0.2.1, must NOT contain 192.0.2.2
    let devs = parsed["devices"].as_array().unwrap();
    for d in devs {
        assert_ne!(
            d["address"], "192.0.2.2",
            "Spoofed _scoped_speaker must be stripped; foreign device must not be returned"
        );
    }

    // --- Subaction 1: Restricted API key calling unscoped HTTP routes fails closed ---
    let detections_resp = client
        .get(format!("http://{addr}/api/detections"))
        .bearer_auth(speaker_scoped_key)
        .send()
        .await
        .unwrap();
    assert_eq!(
        detections_resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "Restricted API key must be denied access to unscoped /api/detections"
    );

    let topology_resp = client
        .get(format!("http://{addr}/api/topology"))
        .bearer_auth(speaker_scoped_key)
        .send()
        .await
        .unwrap();
    assert_eq!(
        topology_resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "Restricted API key must be denied access to unscoped /api/topology"
    );

    // --- Subaction 4: Native engine-level read-only transaction execution ---
    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    conn.query("BEGIN TRANSACTION READ ONLY").unwrap();
    assert!(
        conn.query("CREATE (d:Device {address:'engine-ro-probe'})")
            .is_err(),
        "Engine must reject writes within BEGIN TRANSACTION READ ONLY"
    );
    let _ = conn.query("ROLLBACK");

    // Verify state unchanged
    let mut rows = conn
        .query("MATCH (d:Device {address:'engine-ro-probe'}) RETURN count(d)")
        .unwrap();
    assert_eq!(format!("{:?}", rows.next().unwrap()[0]), "Int64(0)");
}

/// Unsupported ownership dimensions and unscoped routing builders deny by default.
#[tokio::test]
async fn test_bn005_http_scope_support_matrix_and_encoded_parameters() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph").to_str().unwrap(),
            256 * 1024 * 1024,
        )
        .unwrap(),
    );
    // Preserve the original BN-005 acceptance contract in explicit strict mode.
    set_app_config(&store, "ownership_access_mode", "strict").await;
    let routing = RoutingServiceState::new(RoutingServiceConfig {
        internal_service_token: Some("matrix-token".into()),
        bmp_journal_path: None,
        event_journal_path: None,
        ..Default::default()
    })
    .await
    .unwrap();
    let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
    set_app_config(
        &store,
        "routing_service_url",
        &format!("http://{}", backend.local_addr().unwrap()),
    )
    .await;
    set_app_config(&store, "routing_service_token", "matrix-token").await;
    tokio::spawn(async move {
        axum::serve(backend, build_internal_routing_router(routing))
            .await
            .unwrap();
    });
    store
        .create_investigation("".into(), "192.0.2.1".into(), "matrix".into())
        .await
        .unwrap();
    store
        .create_investigation("".into(), "192.0.2.2".into(), "matrix".into())
        .await
        .unwrap();
    for mask in 1..8 {
        let mut dimensions = Vec::new();
        if mask & 1 != 0 {
            dimensions.push("speaker=192.0.2.1");
        }
        if mask & 2 != 0 {
            dimensions.push("vrf_rd=65000:100");
        }
        if mask & 4 != 0 {
            dimensions.push("tenant_id=acme");
        }
        provision_test_key(
            &store,
            &format!("matrix-{mask}"),
            &format!("bsk_matrix_{mask}"),
            &format!("routing:operator:{}", dimensions.join(",")),
        )
        .await;
    }
    for grant in ["viewer", "webhook", "remediation"] {
        provision_test_key(
            &store,
            &format!("matrix-{grant}"),
            &format!("bsk_matrix_{grant}"),
            grant,
        )
        .await;
    }
    provision_test_key(
        &store,
        "speaker-reader",
        "bsk_speaker_reader",
        "routing:read:speaker=192.0.2.1",
    )
    .await;
    provision_test_key(
        &store,
        "global-api-readonly",
        "bsk_global_api_readonly",
        "api_readonly",
    )
    .await;
    let web = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", web.local_addr().unwrap());
    let router = build_public_routing_web_router(store);
    tokio::spawn(async move {
        axum::serve(web, router).await.unwrap();
    });
    let client = reqwest::Client::new();
    let scoped_reader = client
        .get(format!("{base}/api/investigations"))
        .bearer_auth("bsk_speaker_reader")
        .send()
        .await
        .unwrap();
    assert_eq!(scoped_reader.status(), 200);
    let reader_body: serde_json::Value = scoped_reader.json().await.unwrap();
    assert_eq!(reader_body["investigations"].as_array().unwrap().len(), 1);
    assert_eq!(
        reader_body["investigations"][0]["device_address"],
        "192.0.2.1"
    );
    assert_eq!(
        client
            .get(format!("{base}/api/investigations"))
            .bearer_auth("bsk_global_api_readonly")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    for mask in 1..8 {
        let key = format!("bsk_matrix_{mask}");
        let response = client
            .get(format!("{base}/api/routing/v1/routes"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            if mask & 4 == 0 { 200 } else { 403 },
            "routing mask={mask}"
        );
        let response = client
            .get(format!("{base}/api/investigations"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            if mask == 1 { 200 } else { 403 },
            "investigations mask={mask}"
        );
        if mask == 1 {
            let body: serde_json::Value = response.json().await.unwrap();
            assert_eq!(body["investigations"].as_array().unwrap().len(), 1);
            assert_eq!(body["investigations"][0]["device_address"], "192.0.2.1");
        }
        for path in [
            "evidence-coverage",
            "prefix-journey?prefix=10.77.9.0/24",
            "peer-timeline",
            "explain-prefix?prefix=10.77.9.0/24",
        ] {
            assert_eq!(
                client
                    .get(format!("{base}/api/routing/v1/{path}"))
                    .bearer_auth(&key)
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .as_u16(),
                if mask == 1 || mask == 3 { 200 } else { 403 },
                "transport evidence {path}, mask={mask}"
            );
        }
        for path in ["health", "speakers", "coverage", "adjudications"] {
            assert_eq!(
                client
                    .get(format!("{base}/api/routing/v1/{path}"))
                    .bearer_auth(&key)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                403,
                "unscoped builder {path}, mask={mask}"
            );
        }
    }
    for path in [
        "routes?%73peaker=192.0.2.2",
        "routes?speaker=192.0.2.2&speaker=192.0.2.1",
        "routes?%76rf_rd=65000%3A200",
    ] {
        assert_eq!(
            client
                .get(format!("{base}/api/routing/v1/{path}"))
                .bearer_auth("bsk_matrix_3")
                .send()
                .await
                .unwrap()
                .status(),
            403,
            "{path}"
        );
    }
    assert_eq!(
        client
            .get(format!(
                "{base}/api/routing/v1/routes?speaker=192.0.2.1&vrf_rd=65000%3A100"
            ))
            .bearer_auth("bsk_matrix_3")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    for grant in ["webhook", "remediation"] {
        let key = format!("bsk_matrix_{grant}");
        assert_eq!(
            client
                .get(format!("{base}/api/detections"))
                .bearer_auth(&key)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(client.post(format!("{base}/mcp")).bearer_auth(&key).json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"query_graph","arguments":{"cypher":"RETURN 1"}}})).send().await.unwrap().status(), 403);
    }
    assert_eq!(
        client
            .get(format!("{base}/api/routing/v1/health"))
            .bearer_auth("bsk_matrix_viewer")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
}
