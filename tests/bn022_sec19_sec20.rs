//! SEC-19/SEC-20 regression tests — BN-022 secure-core boundary closure.
//!
//! SEC-19: startup graph certification must validate actual migration
//! evidence (strict tombstone schema + identity + disposition +
//! counterpart), and the structural graph-write boundary must cover
//! retired credential classes — not just protected names.
//!
//! SEC-20: `routing_service_url` (the credential DESTINATION for the
//! protected routing service token) is admin-only configuration,
//! enforced on the effective persisted identity.
//!
//! All credentials synthetic; all stores temporary.

use std::sync::Arc;

use anyhow::Result;
use bonsai::auth_store::{AuthUser, MIGRATION_MARKER};
use bonsai::graph::{ConfigItemRecord, GraphStore, RETIRED_CREDENTIAL_CLASSES};
use bonsai::http_server::build_public_routing_web_router;
use tempfile::TempDir;

fn clear_auth_env() {
    unsafe {
        std::env::remove_var("BONSAI_VAULT_PASSPHRASE");
        std::env::remove_var("BONSAI_JWT_SECRET");
    }
}

fn open_store(dir: &TempDir) -> Arc<GraphStore> {
    let path = dir.path().join("bonsai.db");
    Arc::new(GraphStore::open(&path.to_string_lossy(), 256 * 1024 * 1024).expect("open graph"))
}

async fn require_auth(store: &GraphStore) {
    store
        .upsert_config_item(ConfigItemRecord {
            id: "app-cfg-auth".into(),
            config_class: "app_config".into(),
            vendor: String::new(),
            name: "require_auth".into(),
            version: "1".into(),
            content_json: "\"1\"".into(),
            enabled: true,
            created_by: "test".into(),
        })
        .await
        .expect("require_auth");
}

async fn serve(store: Arc<GraphStore>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = build_public_routing_web_router(store);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

fn add_user(store: &GraphStore, username: &str, role: &str) {
    store
        .auth_store
        .upsert_user(&AuthUser {
            username: username.into(),
            password_hash: bonsai::http_server::hash_password("SecBoundaryPassw0rd!1").unwrap(),
            role: role.into(),
            enabled: true,
            last_login_ns: 0,
            created_at_ns: 0,
            updated_at_ns: 0,
        })
        .unwrap();
}

async fn login(client: &reqwest::Client, base: &str, user: &str) -> String {
    let resp = client
        .post(format!("{base}/api/auth/login"))
        .json(&serde_json::json!({
            "username": user, "password": "SecBoundaryPassw0rd!1"
        }))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    body["token"].as_str().unwrap().to_string()
}

fn cfg(id: &str, class: &str, name: &str, content: &str) -> ConfigItemRecord {
    ConfigItemRecord {
        id: id.into(),
        config_class: class.into(),
        vendor: String::new(),
        name: name.into(),
        version: "1".into(),
        content_json: content.into(),
        enabled: true,
        created_by: "test".into(),
    }
}

// ── SEC-19: structural boundary covers retired credential classes ──────

#[tokio::test]
async fn generic_upsert_cannot_create_retired_credential_classes() -> Result<()> {
    clear_auth_env();
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    require_auth(&store).await;

    for class in RETIRED_CREDENTIAL_CLASSES {
        let res = store
            .upsert_config_item(cfg(&format!("new-{class}"), class, "mallory", "{}"))
            .await;
        assert!(res.is_err(), "create in retired class '{class}' must fail");
    }
    // Ordinary classes still work.
    store
        .upsert_config_item(cfg("ok-1", "app_config", "probe", "\"x\""))
        .await?;
    Ok(())
}

#[tokio::test]
async fn generic_upsert_cannot_mutate_existing_credential_records() -> Result<()> {
    clear_auth_env();
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    require_auth(&store).await;

    // Fixture-only path recreates a legacy credential record.
    store
        .upsert_config_item_unchecked(cfg("u-1", "user", "legacy", "{\"password_hash\":\"x\"}"))
        .await?;

    // Generic write to the same id must fail — even with a harmless
    // incoming name (effective persisted class stays 'user').
    let res = store
        .upsert_config_item(cfg("u-1", "app_config", "innocent", "\"{}\""))
        .await;
    assert!(res.is_err(), "mutation of credential record must fail");
    Ok(())
}

#[tokio::test]
async fn fabricated_tombstone_without_counterpart_is_rejected() -> Result<()> {
    clear_auth_env();
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    require_auth(&store).await;

    // A strictly-shaped tombstone claiming 'imported' for a user that was
    // never migrated: schema passes but no AuthStore counterpart exists.
    let tomb = ConfigItemRecord {
        id: "u-tomb".into(),
        config_class: "user".into(),
        vendor: String::new(),
        name: "ghost".into(),
        version: "1".into(),
        content_json: serde_json::json!({
            "migrated_to": "auth_store",
            "migration": MIGRATION_MARKER,
            "record_class": "user",
            "item_id": "u-tomb",
            "disposition": "imported",
            "quarantined_at_ns": 1i64,
        })
        .to_string(),
        enabled: false,
        created_by: "test".into(),
    };
    // No existing row → cannot be a bound tombstone → rejected.
    assert!(store.upsert_config_item(tomb.clone()).await.is_err());
    Ok(())
}

#[tokio::test]
async fn tombstone_with_extra_credential_field_fails_certification() -> Result<()> {
    clear_auth_env();
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    require_auth(&store).await;
    store
        .auth_store
        .upsert_user(&AuthUser {
            username: "real".into(),
            password_hash: "verifier".into(),
            role: "viewer".into(),
            enabled: true,
            last_login_ns: 0,
            created_at_ns: 0,
            updated_at_ns: 0,
        })?;

    // Seed a legacy user then write a tombstone that smuggles an extra
    // credential-bearing field — strict schema must reject it.
    store
        .upsert_config_item_unchecked(cfg("u-2", "user", "real", "{\"password_hash\":\"x\"}"))
        .await?;
    let mut v = serde_json::json!({
        "migrated_to": "auth_store",
        "migration": MIGRATION_MARKER,
        "record_class": "user",
        "item_id": "u-2",
        "disposition": "imported",
        "quarantined_at_ns": 1i64,
        "password_hash": "smuggled-secret",
    });
    v.as_object_mut().unwrap().remove("password_hash"); // keep schema pure? no—
    // rebuild WITH the extra field deliberately:
    let content = serde_json::json!({
        "migrated_to": "auth_store",
        "migration": MIGRATION_MARKER,
        "record_class": "user",
        "item_id": "u-2",
        "disposition": "imported",
        "quarantined_at_ns": 1i64,
        "extra_secret": "smuggled",
    })
    .to_string();
    let _ = v;
    let tomb = ConfigItemRecord {
        id: "u-2".into(),
        config_class: "user".into(),
        vendor: String::new(),
        name: "real".into(),
        version: "1".into(),
        content_json: content,
        enabled: false,
        created_by: "test".into(),
    };
    // Store guard rejects: strict schema fails (extra field).
    assert!(store.upsert_config_item(tomb).await.is_err());
    Ok(())
}

// ── SEC-20: routing_service_url destination authority ──────────────────

#[tokio::test]
async fn operator_cannot_write_routing_service_destination() -> Result<()> {
    clear_auth_env();
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    require_auth(&store).await;
    add_user(&store, "op", "operator");
    add_user(&store, "view", "viewer");
    add_user(&store, "adm", "admin");
    let base = serve(store.clone()).await;
    let client = reqwest::Client::new();
    let op = login(&client, &base, "op").await;
    let view = login(&client, &base, "view").await;
    let adm = login(&client, &base, "adm").await;

    let body = |name: &str| {
        serde_json::json!({
            "id": "cfg-url", "config_class": "app_config", "vendor": "",
            "name": name, "version": "1",
            "content_json": "\"http://127.0.0.1:18099\"",
            "enabled": true, "created_by": "t"
        })
    };

    // Viewer and Operator cannot create the destination.
    for tok in [&view, &op] {
        let r = client
            .post(format!("{base}/api/config-items"))
            .bearer_auth(tok)
            .json(&body("routing_service_url"))
            .send()
            .await?;
        assert_eq!(r.status().as_u16(), 403, "non-admin must not set routing_service_url");
    }

    // Admin can establish the legitimate destination.
    let r = client
        .post(format!("{base}/api/config-items"))
        .bearer_auth(&adm)
        .json(&body("routing_service_url"))
        .send()
        .await?;
    assert_eq!(r.status().as_u16(), 204);

    // Effective-identity bypass: Operator reuses the protected record id
    // with an innocuous incoming name — must still be denied.
    let bypass = serde_json::json!({
        "id": "cfg-url", "config_class": "app_config", "vendor": "",
        "name": "innocuous", "version": "1",
        "content_json": "\"http://169.254.169.254:80\"",
        "enabled": true, "created_by": "t"
    });
    let r = client
        .post(format!("{base}/api/config-items"))
        .bearer_auth(&op)
        .json(&bypass)
        .send()
        .await?;
    assert_eq!(r.status().as_u16(), 403, "id-collision name laundering must fail");

    // And the stored destination is unchanged.
    let items = store.list_config_items(Some("app_config".into())).await?;
    let url_item = items.iter().find(|i| i.id == "cfg-url").unwrap();
    assert!(url_item.content_json.contains("127.0.0.1:18099"));
    Ok(())
}

#[test]
fn routing_service_url_validation_fails_closed() {
    use bonsai::routing_service::validate_routing_service_url;
    assert!(validate_routing_service_url("http://127.0.0.1:8081").is_ok());
    assert!(validate_routing_service_url("https://routing.internal:8443").is_ok());
    assert!(validate_routing_service_url("ftp://evil").is_err());
    assert!(validate_routing_service_url("http://user:pass@host").is_err());
    assert!(validate_routing_service_url("not a url").is_err());
    assert!(validate_routing_service_url("").is_err());
}
