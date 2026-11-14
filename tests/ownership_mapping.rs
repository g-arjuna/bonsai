//! Ownership rollout remains audit-only; it never authorizes a data request.
use bonsai::graph::{
    ConfigItemRecord, GraphStore,
    ownership::{OwnershipAssignment, OwnershipResource, OwnershipStatus},
};
use bonsai::http_server::{build_public_routing_web_router, mint_jwt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::future::IntoFuture;
use std::sync::Arc;

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

fn assignment(speaker: &str, rd: Option<&str>, tenant: &str) -> OwnershipAssignment {
    OwnershipAssignment {
        resource: OwnershipResource {
            speaker: speaker.into(),
            vrf_rd: rd.map(str::to_owned),
        },
        tenant_id: tenant.into(),
    }
}

#[tokio::test]
async fn durable_exact_ownership_distinguishes_device_exporter_and_rd() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ownership.db");
    {
        let store = GraphStore::open(path.to_str().unwrap(), 256 * 1024 * 1024).unwrap();
        for input in [
            assignment("192.0.2.1", None, "infrastructure"),
            assignment("192.0.2.1", Some("65000:100"), "acme"),
            assignment("192.0.2.1", Some("65000:200"), "beta"),
            assignment("192.0.2.2", Some("65000:100"), "beta"),
        ] {
            store
                .assign_ownership(input, "user:alice".into())
                .await
                .unwrap();
        }
        let report = store
            .audit_ownership(assignment("192.0.2.1", Some("65000:100"), "beta"))
            .await
            .unwrap();
        assert_eq!(report.status, OwnershipStatus::Mismatched);
        assert!(!report.enforcement_enabled);
        // A device mapping does not implicitly assign all its VRFs to the device's tenant.
        let missing = store
            .audit_ownership(assignment("192.0.2.1", Some("65000:300"), "infrastructure"))
            .await
            .unwrap();
        assert_eq!(missing.status, OwnershipStatus::Unmapped);
        assert!(!missing.enforcement_enabled);
    }
    let store = GraphStore::open(path.to_str().unwrap(), 256 * 1024 * 1024).unwrap();
    let report = store
        .audit_ownership(assignment("192.0.2.1", Some("65000:100"), "acme"))
        .await
        .unwrap();
    assert_eq!(report.status, OwnershipStatus::Matched);
    assert_eq!(report.binding.unwrap().updated_by, "user:alice");
    let page1 = store.list_ownership(String::new(), 2).await.unwrap();
    let page2 = store
        .list_ownership(page1.last().unwrap().0.clone(), 2)
        .await
        .unwrap();
    assert_eq!(page1.len(), 2);
    assert_eq!(page2.len(), 2);
    assert!(page1.iter().all(|a| page2.iter().all(|b| a.0 != b.0)));
    store
        .assign_ownership(
            assignment("192.0.2.1", Some("65000:100"), "beta"),
            "user:bob".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .list_ownership(String::new(), 100)
            .await
            .unwrap()
            .len(),
        4
    );
    let changed = store
        .audit_ownership(assignment("192.0.2.1", Some("65000:100"), "beta"))
        .await
        .unwrap();
    assert_eq!(changed.status, OwnershipStatus::Matched);
    assert_eq!(changed.binding.unwrap().updated_by, "user:bob");
}

#[tokio::test]
async fn labels_are_bound_parameters_and_empty_values_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        256 * 1024 * 1024,
    )
    .unwrap();
    for bad in [
        assignment("", None, "acme"),
        assignment("x", Some(""), "acme"),
        assignment("x", None, " acme"),
        assignment("x\n", None, "acme"),
    ] {
        assert!(
            store
                .assign_ownership(bad, "user:test".into())
                .await
                .is_err()
        );
    }
    // Delimiters and query syntax cannot collide keys or inject statements.
    for input in [
        assignment("a:b", Some("c"), "acme' SET x='bad"),
        assignment("a", Some("b:c"), "beta"),
    ] {
        store
            .assign_ownership(input.clone(), "user:test".into())
            .await
            .unwrap();
        assert_eq!(
            store.audit_ownership(input).await.unwrap().status,
            OwnershipStatus::Matched
        );
    }
    assert_eq!(
        store
            .list_ownership(String::new(), 100)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn admin_http_inventory_and_audit_do_not_change_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph.db").to_str().unwrap(),
            256 * 1024 * 1024,
        )
        .unwrap(),
    );
    // AUTH-C2: src=local JWTs require live local accounts.
    for (u, r) in [
        ("alice", "admin"),
        ("bob", "viewer"),
        ("charlie", "operator"),
    ] {
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
    let admin = mint_jwt("alice", "admin", &store).await.unwrap();
    let viewer = mint_jwt("bob", "viewer", &store).await.unwrap();
    let operator = mint_jwt("charlie", "operator", &store).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(
        axum::serve(listener, build_public_routing_web_router(store.clone())).into_future(),
    );
    let client = reqwest::Client::new();
    let url = format!("{base}/api/ownership/bindings");
    let input = json!({"resource":{"speaker":"192.0.2.1","vrf_rd":"65000:100"},"tenant_id":"acme"});
    assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
    for token in [&viewer, &operator] {
        assert_eq!(
            client
                .put(&url)
                .bearer_auth(token)
                .json(&input)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            client
                .get(&url)
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    let response = client
        .put(&url)
        .bearer_auth(&admin)
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let assigned: Value = response.json().await.unwrap();
    assert_eq!(assigned["binding"]["updated_by"], "user:alice");
    assert_eq!(assigned["enforcement_enabled"], false);
    for (tenant, expected) in [("acme", "matched"), ("beta", "mismatched")] {
        let report = client.post(format!("{base}/api/ownership/audit")).bearer_auth(&admin)
            .json(&json!({"resource":{"speaker":"192.0.2.1","vrf_rd":"65000:100"},"tenant_id":tenant})).send().await.unwrap();
        assert_eq!(report.status(), 200);
        let report: Value = report.json().await.unwrap();
        assert_eq!(report["status"], expected);
        assert_eq!(report["enforcement_enabled"], false);
    }
    let report = client
        .post(format!("{base}/api/ownership/audit"))
        .bearer_auth(&admin)
        .json(&json!({"resource":{"speaker":"192.0.2.2"},"tenant_id":"acme"}))
        .send()
        .await
        .unwrap();
    assert_eq!(report.status(), 200);
    assert_eq!(report.json::<Value>().await.unwrap()["status"], "unmapped");
    let spoofed =
        json!({"resource":{"speaker":"x"},"tenant_id":"acme","updated_by":"user:mallory"});
    assert_eq!(
        client
            .put(&url)
            .bearer_auth(&admin)
            .json(&spoofed)
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    let listed: Value = client
        .get(&url)
        .bearer_auth(&admin)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["bindings"].as_array().unwrap().len(), 1);
    assert_eq!(listed["truncated"], false);
    let api_key = "bsk_ownership_test_admin_only";
    seed_api_key(
        &store,
        ConfigItemRecord {
            id: "ownership-admin-key".into(),
            config_class: "api_key".into(),
            vendor: String::new(),
            name: "ownership admin".into(),
            version: "1".into(),
            content_json: json!({"key_hash":hex::encode(Sha256::digest(api_key.as_bytes())),
                "scope":"admin", "expires_at_ns":0, "last_used_at_ns":0})
            .to_string(),
            enabled: true,
            created_by: "test".into(),
        },
    );
    let reassigned: Value = client
        .put(&url)
        .bearer_auth(api_key)
        .json(&input)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        reassigned["binding"]["updated_by"],
        "api_key:ownership-admin-key"
    );
    // Mapping mismatches did not revoke the viewer's pre-existing topology permission.
    assert_eq!(
        client
            .get(format!("{base}/api/topology"))
            .bearer_auth(&viewer)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    server.abort();
}
