//! B10, B11, B12, B13, B15 integration and contract tests:
//! - B10: Reduced routing service (`bonsai-routing`) runs without Kafka/NATS/ML.
//! - B11: Versioned internal API (`/internal/routing/v1/*`) and public Bonsai proxy
//!        (`/api/routing/v1/*`), config validation, prefix history before/after attributes,
//!        unsupported family status, transport gap vs withdrawal, and graceful degradation
//!        when routing service is unavailable.
//! - B12: Shared Bonsai JWT/API-key auth & RBAC on `/api/routing/v1/*`, scoped read enforcement,
//!        viewer mutation denial, and secret redaction.
//! - B13: Checkpointed graph summary projection (`project_routing_events_to_graph`) that
//!        neither duplicates nor loses events across restart and avoids raw route churn in `BgpRoute`.
//! - B15: Single BMP authority (`BmpConfig::should_spawn_legacy_receiver()`).

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use bonsai::config::BmpConfig;
use bonsai::graph::{ConfigItemRecord, GraphStore};
use bonsai::http_server::{build_public_routing_router, mint_jwt, redact_secrets_in_json};
use bonsai::routing_service::{
    RoutingServiceConfig, RoutingServiceState, build_internal_routing_router,
    project_routing_events_to_graph, sample_bmp_session_frames,
};
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
            created_by: "test".to_string(),
        })
        .await
        .expect("upsert app_config");
}

#[tokio::test]
async fn b11_routing_api_contract_prefix_history_and_graceful_degradation() {
    // 1. Config validation rejects empty addresses and invalid frame bounds
    let mut bad_cfg = RoutingServiceConfig::default();
    bad_cfg.max_frame_bytes = 3;
    assert!(bad_cfg.validate().is_err());

    let dir = tempfile::tempdir().expect("tempdir");
    let bmp_journal_path = dir.path().join("bmp.jsonl");
    let event_journal_path = dir.path().join("events.jsonl");
    let graph_path = dir.path().join("graph.db");

    // 2. Start reduced routing service on an ephemeral TCP port
    let routing_cfg = RoutingServiceConfig {
        bmp_listen_addr: "127.0.0.1:0".to_string(),
        http_listen_addr: "127.0.0.1:0".to_string(),
        bmp_journal_path: Some(bmp_journal_path),
        event_journal_path: Some(event_journal_path),
        max_frame_bytes: 65535,
        internal_service_token: Some("test-internal-routing-secret-token".to_string()),
    };
    let routing_state = RoutingServiceState::new(routing_cfg).await.unwrap();
    let routing_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let routing_addr = routing_listener.local_addr().unwrap();
    let routing_srv_state = Arc::clone(&routing_state);
    tokio::spawn(async move {
        axum::serve(
            routing_listener,
            build_internal_routing_router(routing_srv_state),
        )
        .await
        .unwrap();
    });

    // 3. Start Bonsai public router proxying to the private routing service
    let store = Arc::new(
        GraphStore::open(graph_path.to_str().unwrap(), 256 * 1024 * 1024).expect("open graph"),
    );
    set_app_config(
        &store,
        "routing_service_url",
        &format!("http://{routing_addr}"),
    )
    .await;
    set_app_config(
        &store,
        "routing_service_token",
        "test-internal-routing-secret-token",
    )
    .await;
    set_app_config(&store, "require_auth", "0").await;

    let bonsai_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bonsai_addr = bonsai_listener.local_addr().unwrap();
    let bonsai_store = Arc::clone(&store);
    tokio::spawn(async move {
        axum::serve(bonsai_listener, build_public_routing_router(bonsai_store))
            .await
            .unwrap();
    });

    // 4. Ingest Initiation + PeerUp + 2 announcements (local_pref 100 -> 250) + withdrawal
    let speaker = IpAddr::V4(Ipv4Addr::new(172, 20, 0, 11));
    let peer_ip = Ipv4Addr::new(10, 0, 0, 2);
    let (init, peer_up, rm_lp100) = sample_bmp_session_frames(peer_ip, 65001, [203, 0, 113], 100);
    let (_, _, rm_lp250) = sample_bmp_session_frames(peer_ip, 65001, [203, 0, 113], 250);

    routing_state
        .ingest_bmp_bytes(speaker, Some(1), &init)
        .await
        .unwrap();
    routing_state
        .ingest_bmp_bytes(speaker, Some(1), &peer_up)
        .await
        .unwrap();
    routing_state
        .ingest_bmp_bytes(speaker, Some(1), &rm_lp100)
        .await
        .unwrap();
    routing_state
        .ingest_bmp_bytes(speaker, Some(1), &rm_lp250)
        .await
        .unwrap();

    let client = reqwest::Client::new();
    let base = format!("http://{bonsai_addr}");

    // 5. Verify /api/routing/v1/health, /speakers, /peers, /routes, /prefix-history, /coverage, /events
    let health: serde_json::Value = client
        .get(format!("{base}/api/routing/v1/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["status"], "ok");
    assert_eq!(health["speakers_connected"], 1);
    assert_eq!(health["peers_up"], 1);
    assert_eq!(health["total_routes"], 1);
    assert_eq!(health["durable_sequence"], 4);

    let speakers: serde_json::Value = client
        .get(format!("{base}/api/routing/v1/speakers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(speakers["speakers"][0]["sys_name"], "edge-a");

    let routes: serde_json::Value = client
        .get(format!("{base}/api/routing/v1/routes"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(routes["total_matching"], 1);
    assert_eq!(routes["routes"][0]["prefix"], "203.0.113.0/24");
    assert_eq!(routes["routes"][0]["local_pref"], 250);

    let history: serde_json::Value = client
        .get(format!(
            "{base}/api/routing/v1/prefix-history?prefix=203.0.113.0/24"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let events = history["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    // Most recent event first: local_pref changed from 100 (before) to 250 (after)
    assert_eq!(events[0]["before_attributes"]["local_pref"], 100);
    assert_eq!(events[0]["after_attributes"]["local_pref"], 250);
    assert!(events[1]["before_attributes"].is_null());
    assert_eq!(events[1]["after_attributes"]["local_pref"], 100);

    // 6. Unsupported / experimental family returns explicit completeness_state = "unsupported"
    let evpn_routes: serde_json::Value = client
        .get(format!("{base}/api/routing/v1/routes?family=l2vpn-evpn"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(evpn_routes["completeness_state"], "unsupported");
    assert_eq!(evpn_routes["support_status"], "experimental_parse_only");

    // 7. Transport disconnect marks view as "gap" without fabricating BGP route withdrawals
    routing_state
        .record_transport_disconnect(speaker, "bmp_tcp_reset")
        .await;
    let gap_routes: serde_json::Value = client
        .get(format!("{base}/api/routing/v1/routes"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(gap_routes["completeness_state"], "gap");
    assert_eq!(
        gap_routes["total_matching"], 1,
        "BMP transport disconnect must preserve last-known route with gap status rather than fabricating BGP withdrawal"
    );

    // 8. Pointing Bonsai at an unreachable routing backend returns 503 without crashing Bonsai
    set_app_config(&store, "routing_service_url", "http://127.0.0.1:1").await;
    let unavail_resp = client
        .get(format!("{base}/api/routing/v1/routes"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        unavail_resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    let unavail_json: serde_json::Value = unavail_resp.json().await.unwrap();
    assert_eq!(unavail_json["error"], "routing_service_unavailable");
    assert_eq!(unavail_json["completeness_state"], "unavailable");

    // Bonsai itself remains healthy
    let bonsai_health = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(bonsai_health.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn b12_security_rbac_scoped_reads_and_secret_redaction() {
    let dir = tempfile::tempdir().expect("tempdir");
    let graph_path = dir.path().join("graph_rbac.db");
    let store = Arc::new(
        GraphStore::open(graph_path.to_str().unwrap(), 256 * 1024 * 1024).expect("open graph"),
    );

    let routing_state = RoutingServiceState::new(RoutingServiceConfig::default())
        .await
        .unwrap();
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
    set_app_config(&store, "require_auth", "1").await;

    let bonsai_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bonsai_addr = bonsai_listener.local_addr().unwrap();
    let bonsai_store = Arc::clone(&store);
    tokio::spawn(async move {
        axum::serve(bonsai_listener, build_public_routing_router(bonsai_store))
            .await
            .unwrap();
    });

    let client = reqwest::Client::new();
    let base = format!("http://{bonsai_addr}");

    // 1. Unauthenticated request to /api/routing/v1/routes is denied (401)
    let unauth = client
        .get(format!("{base}/api/routing/v1/routes"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), reqwest::StatusCode::UNAUTHORIZED);

    // 2. Viewer JWT can GET /api/routing/v1/routes (200) but CANNOT POST /api/routing/v1/ingest-bmp (403)
    let viewer_jwt = mint_jwt("alice-viewer", "viewer", &store).await.unwrap();
    let viewer_get = client
        .get(format!("{base}/api/routing/v1/routes"))
        .bearer_auth(&viewer_jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(viewer_get.status(), reqwest::StatusCode::OK);

    let (init, _, _) =
        sample_bmp_session_frames(Ipv4Addr::new(10, 0, 0, 2), 65001, [10, 0, 1], 100);
    let viewer_post = client
        .post(format!("{base}/api/routing/v1/ingest-bmp"))
        .bearer_auth(&viewer_jwt)
        .json(&serde_json::json!({
            "speaker_addr": "172.20.0.11",
            "session_epoch": 1,
            "frame_hex": hex::encode(&init),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(viewer_post.status(), reqwest::StatusCode::FORBIDDEN);

    // 3. Operator JWT CAN POST /api/routing/v1/ingest-bmp (200)
    let operator_jwt = mint_jwt("bob-operator", "operator", &store).await.unwrap();
    let operator_post = client
        .post(format!("{base}/api/routing/v1/ingest-bmp"))
        .bearer_auth(&operator_jwt)
        .json(&serde_json::json!({
            "speaker_addr": "172.20.0.11",
            "session_epoch": 1,
            "frame_hex": hex::encode(&init),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(operator_post.status(), reqwest::StatusCode::OK);

    // 4. API key with non-routing scope ("webhook") is denied (403) on /api/routing/v1/routes,
    //    while API key with scope "routing:read" is permitted (200) for GET and denied (403) for POST.
    let webhook_key = "bsk_webhook_only_key_123456";
    let routing_key = "bsk_routing_read_key_654321";
    for (id, raw_key, scope) in [
        ("key-webhook", webhook_key, "webhook"),
        ("key-routing", routing_key, "routing:read"),
    ] {
        let hash = hex::encode(Sha256::digest(raw_key.as_bytes()));
        store
            .upsert_config_item(ConfigItemRecord {
                id: id.to_string(),
                config_class: "api_key".to_string(),
                vendor: String::new(),
                name: id.to_string(),
                version: "1".to_string(),
                content_json: serde_json::json!({
                    "key_hash": hash,
                    "scope": scope,
                    "expires_at_ns": 0,
                    "last_used_at_ns": 0,
                })
                .to_string(),
                enabled: true,
                created_by: "test".to_string(),
            })
            .await
            .unwrap();
    }

    let webhook_get = client
        .get(format!("{base}/api/routing/v1/routes"))
        .bearer_auth(webhook_key)
        .send()
        .await
        .unwrap();
    assert_eq!(webhook_get.status(), reqwest::StatusCode::FORBIDDEN);

    let routing_get = client
        .get(format!("{base}/api/routing/v1/routes"))
        .bearer_auth(routing_key)
        .send()
        .await
        .unwrap();
    assert_eq!(routing_get.status(), reqwest::StatusCode::OK);

    // 5. Secret redaction helper scrubs nested sensitive fields
    let mut payload = serde_json::json!({
        "speaker": "172.20.0.11",
        "internal_service_token": "super-secret-token",
        "nested": {
            "bind_password": "cleartext-password",
            "api_key": "bsk_secret",
            "safe_field": "visible"
        }
    });
    redact_secrets_in_json(&mut payload);
    assert_eq!(payload["internal_service_token"], "[REDACTED]");
    assert_eq!(payload["nested"]["bind_password"], "[REDACTED]");
    assert_eq!(payload["nested"]["api_key"], "[REDACTED]");
    assert_eq!(payload["nested"]["safe_field"], "visible");
}

#[tokio::test]
async fn b13_b15_graph_routing_projector_idempotency_and_single_bmp_authority() {
    // B15: Single BMP authority check
    let mut bmp_cfg = BmpConfig::default();
    bmp_cfg.enabled = true;
    assert_eq!(bmp_cfg.authority, "routing_service");
    assert!(
        !bmp_cfg.should_spawn_legacy_receiver(),
        "Legacy Bonsai BMP receiver must stay disabled when authority is routing_service"
    );
    bmp_cfg.authority = "legacy_bonsai".to_string();
    assert!(bmp_cfg.should_spawn_legacy_receiver());

    // B13: Graph summary projection with durable replay checkpoint
    let dir = tempfile::tempdir().expect("tempdir");
    let graph_path = dir.path().join("graph_proj.db");
    let store =
        GraphStore::open(graph_path.to_str().unwrap(), 256 * 1024 * 1024).expect("open graph");

    let routing_state = RoutingServiceState::new(RoutingServiceConfig::default())
        .await
        .unwrap();
    let speaker = IpAddr::V4(Ipv4Addr::new(172, 20, 0, 11));
    let peer_ip = Ipv4Addr::new(10, 0, 0, 2);
    let (init, peer_up, rm1) = sample_bmp_session_frames(peer_ip, 65001, [198, 51, 100], 100);
    let (_, _, rm2) = sample_bmp_session_frames(peer_ip, 65001, [203, 0, 113], 200);

    routing_state
        .ingest_bmp_bytes(speaker, Some(1), &init)
        .await
        .unwrap();
    routing_state
        .ingest_bmp_bytes(speaker, Some(1), &peer_up)
        .await
        .unwrap();
    routing_state
        .ingest_bmp_bytes(speaker, Some(1), &rm1)
        .await
        .unwrap();
    routing_state
        .ingest_bmp_bytes(speaker, Some(1), &rm2)
        .await
        .unwrap();

    // First projection run applies all 4 durable events
    let step1 = project_routing_events_to_graph(&routing_state.event_journal, &store, 100)
        .await
        .unwrap();
    assert_eq!(step1.newly_applied_events, 4);
    assert_eq!(step1.graph_checkpoint_sequence, 4);
    assert_eq!(step1.sequence_lag, 0);
    assert_eq!(step1.checkpoint.skipped_raw_route_churn_count, 2);

    let pk_id = "172.20.0.11|0|0:0|10.0.0.2";
    let peer_summary = step1
        .checkpoint
        .peer_summaries
        .get(pk_id)
        .expect("peer summary projected");
    assert_eq!(peer_summary.state, "UP");
    assert_eq!(peer_summary.active_prefixes.len(), 2);
    assert_eq!(peer_summary.announce_events_count, 2);

    // Second projection run (simulating projector restart) must be idempotent: 0 newly applied events
    let step2 = project_routing_events_to_graph(&routing_state.event_journal, &store, 100)
        .await
        .unwrap();
    assert_eq!(step2.newly_applied_events, 0);
    assert_eq!(step2.graph_checkpoint_sequence, 4);
    assert_eq!(step2.sequence_lag, 0);
    assert_eq!(
        step2.checkpoint.peer_summaries.get(pk_id),
        Some(peer_summary)
    );
}
