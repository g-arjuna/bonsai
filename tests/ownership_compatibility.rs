//! Explicitly requested temporary relaxation: ownership labels are audit-only.
use bonsai::{
    graph::{ConfigItemRecord, GraphStore, ownership::OwnershipAccessMode},
    http_server::{
        auth::{Role, RoutingAccessScope},
        build_public_routing_web_router, mint_jwt,
    },
    mcp_server::{
        ToolExecutionContext, authorize_and_scope_tool_with_mode, call_tool_with_store_and_routing,
    },
    routing_service::{
        RoutingServiceConfig, RoutingServiceState, build_internal_routing_router,
        sample_bmp_session_frames,
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{future::IntoFuture, net::Ipv4Addr, sync::Arc};

/// BN-022: provision an API key through protected AuthStore. The legacy
/// graph `api_key` ConfigItem path is retired — post-migration graph rows
/// cannot authorize, so tests must seed the auth store directly.
fn seed_api_key(store: &bonsai::graph::GraphStore, item: bonsai::graph::ConfigItemRecord) {
    let v: serde_json::Value =
        serde_json::from_str(&item.content_json).expect("api_key content_json");
    store
        .auth_store
        .upsert_api_key_verifier(
            &item.id,
            &item.name,
            v["key_hash"].as_str().expect("key_hash"),
            v["scope"].as_str().unwrap_or("read"),
            v["expires_at_ns"].as_i64().unwrap_or(0),
        )
        .expect("seed api key");
}

async fn config(store: &GraphStore, name: &str, value: &str) {
    store
        .upsert_config_item(ConfigItemRecord {
            id: if name == "ownership_access_mode" {
                "ownership-access-mode".into()
            } else {
                format!("config-{name}")
            },
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
async fn key(store: &GraphStore, id: &str, grant: &str) -> String {
    let key = format!("bsk_ownership_compat_{id}");
    seed_api_key(
        &store,
        ConfigItemRecord {
            id: id.into(),
            config_class: "api_key".into(),
            vendor: String::new(),
            name: id.into(),
            version: "1".into(),
            content_json:
                json!({"key_hash":hex::encode(Sha256::digest(key.as_bytes())), "scope":grant,
            "expires_at_ns":0,"last_used_at_ns":0})
                .to_string(),
            enabled: true,
            created_by: "test".into(),
        },
    );
    key
}

#[test]
fn temporary_tool_policy_relaxes_ownership_dimensions_but_retains_other_boundaries() {
    for mask in 1..8 {
        let scope = RoutingAccessScope {
            speaker: (mask & 1 != 0).then(|| "192.0.2.1".into()),
            vrf_rd: (mask & 2 != 0).then(|| "65000:100".into()),
            tenant_id: (mask & 4 != 0).then(|| "acme".into()),
        };
        for name in [
            "query_devices",
            "get_changes",
            "get_device_blast_radius",
            "list_active_detections",
            "get_incident",
            "get_prefix_journey",
            "get_peer_timeline",
            "get_evidence_coverage",
            "compare_snapshots",
            "explain_missing_prefix",
        ] {
            let mut args = json!({"_scoped_speaker":"spoofed"});
            authorize_and_scope_tool_with_mode(
                name,
                &mut args,
                Some(&scope),
                Some(&Role::ApiReadonly),
                OwnershipAccessMode::AuditOnly,
            )
            .unwrap();
            assert_ne!(args["_scoped_speaker"], "spoofed");
            if scope.speaker.is_some() {
                let mut foreign = json!({"speaker":"192.0.2.2", "device_address":"192.0.2.2", "address":"192.0.2.2"});
                assert!(
                    authorize_and_scope_tool_with_mode(
                        name,
                        &mut foreign,
                        Some(&scope),
                        Some(&Role::Operator),
                        OwnershipAccessMode::AuditOnly
                    )
                    .is_err()
                );
            }
        }
        for name in ["query_graph", "unclassified_future_tool"] {
            assert!(
                authorize_and_scope_tool_with_mode(
                    name,
                    &mut json!({}),
                    Some(&scope),
                    Some(&Role::Admin),
                    OwnershipAccessMode::AuditOnly
                )
                .is_err()
            );
        }
        assert!(
            authorize_and_scope_tool_with_mode(
                "evaluate_change_plan",
                &mut json!({}),
                Some(&scope),
                Some(&Role::ApiReadonly),
                OwnershipAccessMode::AuditOnly
            )
            .is_err()
        );
    }
    let scope = RoutingAccessScope {
        speaker: None,
        vrf_rd: Some("65000:100".into()),
        tenant_id: Some("acme".into()),
    };
    assert!(
        authorize_and_scope_tool_with_mode(
            "compare_snapshots",
            &mut json!({"vrf_rd":"65000:200"}),
            Some(&scope),
            Some(&Role::Operator),
            OwnershipAccessMode::AuditOnly
        )
        .is_err()
    );
    assert!(
        authorize_and_scope_tool_with_mode(
            "evaluate_change_plan",
            &mut json!({"plan":{"tenant_id":"beta"}}),
            Some(&scope),
            Some(&Role::Operator),
            OwnershipAccessMode::AuditOnly
        )
        .is_err()
    );
}

#[tokio::test]
async fn shared_dispatch_and_persisted_strict_restore() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph");
    {
        let store = Arc::new(GraphStore::open(path.to_str().unwrap(), 256 * 1024 * 1024).unwrap());
        assert_eq!(
            store.ownership_access_mode().await,
            OwnershipAccessMode::AuditOnly
        );
        {
            let db = store.db();
            lbug::Connection::new(&db).unwrap().query("CREATE (:Device {address:'192.0.2.1',hostname:'own'}), (:Device {address:'192.0.2.2',hostname:'other'})").unwrap();
        }
        let combined = ToolExecutionContext {
            role: Role::ApiReadonly,
            scope: Some(RoutingAccessScope {
                speaker: Some("192.0.2.1".into()),
                vrf_rd: Some("65000:100".into()),
                tenant_id: Some("acme".into()),
            }),
        };
        let result =
            call_tool_with_store_and_routing(&store, None, "query_devices", &json!({}), &combined)
                .await
                .unwrap();
        let text = result.to_string();
        assert!(text.contains("192.0.2.1"));
        assert!(!text.contains("192.0.2.2"));
        assert_eq!(result["ownership_access"]["mode"], "audit_only");
        let tenant = ToolExecutionContext {
            role: Role::ApiReadonly,
            scope: Some(RoutingAccessScope {
                speaker: None,
                vrf_rd: None,
                tenant_id: Some("acme".into()),
            }),
        };
        let broad =
            call_tool_with_store_and_routing(&store, None, "query_devices", &json!({}), &tenant)
                .await
                .unwrap()
                .to_string();
        assert!(broad.contains("192.0.2.1") && broad.contains("192.0.2.2"));
        config(&store, "ownership_access_mode", "strict").await;
        assert!(
            call_tool_with_store_and_routing(&store, None, "query_devices", &json!({}), &tenant)
                .await
                .unwrap_err()
                .contains("routing_scope_violation")
        );
        assert!(
            call_tool_with_store_and_routing(&store, None, "query_devices", &json!({}), &combined)
                .await
                .is_err()
        );
    }
    let store = GraphStore::open(path.to_str().unwrap(), 256 * 1024 * 1024).unwrap();
    assert_eq!(
        store.ownership_access_mode().await,
        OwnershipAccessMode::Strict
    );
    config(&store, "ownership_access_mode", "invalid").await;
    assert_eq!(
        store.ownership_access_mode().await,
        OwnershipAccessMode::Strict
    );
    config(&store, "ownership_access_mode", "audit_only").await;
    assert_eq!(
        store.ownership_access_mode().await,
        OwnershipAccessMode::AuditOnly
    );
}

#[tokio::test]
async fn http_compatibility_and_admin_policy_switch_retain_speaker_and_supported_rd_filters() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph").to_str().unwrap(),
            256 * 1024 * 1024,
        )
        .unwrap(),
    );
    config(&store, "require_auth", "true").await;
    let routing = RoutingServiceState::new(RoutingServiceConfig {
        internal_service_token: Some("ownership-internal".into()),
        bmp_journal_path: None,
        event_journal_path: None,
        ..Default::default()
    })
    .await
    .unwrap();
    for speaker in [Ipv4Addr::new(192, 0, 2, 1), Ipv4Addr::new(192, 0, 2, 2)] {
        let (init, peer, route) = sample_bmp_session_frames(speaker, 65001, [10, 0, 0], 100);
        for frame in [&init, &peer, &route] {
            routing
                .ingest_bmp_bytes(speaker.into(), Some(1), frame)
                .await
                .unwrap();
        }
    }
    let backend_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    config(
        &store,
        "routing_service_url",
        &format!("http://{}", backend_listener.local_addr().unwrap()),
    )
    .await;
    config(&store, "routing_service_token", "ownership-internal").await;
    let backend = tokio::spawn(
        axum::serve(backend_listener, build_internal_routing_router(routing)).into_future(),
    );
    let own = store
        .create_investigation("".into(), "192.0.2.1".into(), "test".into())
        .await
        .unwrap();
    let other = store
        .create_investigation("".into(), "192.0.2.2".into(), "test".into())
        .await
        .unwrap();
    let tenant = key(&store, "tenant", "routing:read:tenant_id=acme").await;
    let vrf = key(&store, "vrf", "routing:read:vrf_rd=0:0").await;
    let combined = key(
        &store,
        "combined",
        "routing:read:speaker=192.0.2.1,vrf_rd=0:0,tenant_id=acme",
    )
    .await;
    // AUTH-C2: src=local JWTs require a live local account.
    store
        .auth_store
        .upsert_user(&bonsai::auth_store::AuthUser {
            username: "admin".into(),
            password_hash: "test-verifier".into(),
            role: "admin".into(),
            enabled: true,
            last_login_ns: 0,
            created_at_ns: 0,
            updated_at_ns: 0,
        })
        .unwrap();
    let admin = mint_jwt("admin", "admin", &store).await.unwrap();
    let web_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", web_listener.local_addr().unwrap());
    let web = tokio::spawn(
        axum::serve(web_listener, build_public_routing_web_router(store.clone())).into_future(),
    );
    let client = reqwest::Client::new();
    for (name, succeeds) in [("query_devices", true), ("query_graph", false)] {
        let rpc: Value = client
            .post(format!("{base}/mcp"))
            .bearer_auth(&tenant)
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":name,"arguments":{"query":"MATCH (d:Device) RETURN d.address"}}}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        if succeeds {
            let payload: Value =
                serde_json::from_str(rpc["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(payload["ownership_access"]["mode"], "audit_only");
        } else {
            assert!(
                rpc["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("routing_scope_violation")
            );
        }
    }
    for token in [&tenant, &vrf, &combined] {
        let response = client
            .get(format!("{base}/api/routing/v1/routes"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["x-bonsai-ownership-mode"], "audit-only");
        let body: Value = response.json().await.unwrap();
        let rows = body["routes"].as_array().unwrap();
        assert_eq!(rows.len(), if token == &combined { 1 } else { 2 });
        if token == &combined {
            assert_eq!(rows[0]["speaker_addr"], "192.0.2.1");
        }
        let invs: Value = client
            .get(format!("{base}/api/investigations"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            invs["investigations"].as_array().unwrap().len(),
            if token == &combined { 1 } else { 2 }
        );
    }
    assert_eq!(
        client
            .get(format!("{base}/api/investigations/{}", own.id))
            .bearer_auth(&combined)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("{base}/api/investigations/{}", other.id))
            .bearer_auth(&combined)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    for (token, query) in [(&combined, "speaker=192.0.2.2"), (&vrf, "vrf_rd=65000:200")] {
        assert_eq!(
            client
                .get(format!("{base}/api/routing/v1/routes?{query}"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    // Tenant-only summaries are readable in compatibility mode; speaker-bound summaries still deny.
    assert_eq!(
        client
            .get(format!("{base}/api/routing/v1/speakers"))
            .bearer_auth(&tenant)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("{base}/api/routing/v1/speakers"))
            .bearer_auth(&combined)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(format!("{base}/api/routing/v1/routes"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .put(format!("{base}/api/ownership/policy"))
            .bearer_auth(&tenant)
            .json(&json!({"access_mode":"strict"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    for mode in ["strict", "audit_only"] {
        let switched = client
            .put(format!("{base}/api/ownership/policy"))
            .bearer_auth(&admin)
            .json(&json!({"access_mode":mode}))
            .send()
            .await
            .unwrap();
        assert_eq!(switched.status(), 200);
        let response = client
            .get(format!("{base}/api/routing/v1/routes"))
            .bearer_auth(&tenant)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), if mode == "strict" { 403 } else { 200 });
    }
    backend.abort();
    web.abort();
}
