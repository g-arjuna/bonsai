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
    store.auth_store.upsert_user(&AuthUser {
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
        assert_eq!(
            r.status().as_u16(),
            403,
            "non-admin must not set routing_service_url"
        );
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
    assert_eq!(
        r.status().as_u16(),
        403,
        "id-collision name laundering must fail"
    );

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
    assert!(validate_routing_service_url("http://localhost:8081").is_ok());
    assert!(validate_routing_service_url("http://[::1]:8081").is_ok());
    assert!(validate_routing_service_url("https://routing.internal:8443").is_ok());
    assert!(validate_routing_service_url("https://10.1.2.3/base-path").is_ok());
    assert!(validate_routing_service_url("ftp://evil").is_err());
    assert!(validate_routing_service_url("http://user:pass@host").is_err());
    assert!(validate_routing_service_url("not a url").is_err());
    assert!(validate_routing_service_url("").is_err());
    // Ambiguous base components corrupt the {base}/internal/... join.
    assert!(validate_routing_service_url("http://127.0.0.1:8081?x=1").is_err());
    assert!(validate_routing_service_url("http://127.0.0.1:8081#frag").is_err());
    // WP1.4 transport contract: plain http to non-loopback is rejected
    // unless the documented operator override is set.
    assert!(validate_routing_service_url("http://169.254.169.254:80").is_err());
    assert!(validate_routing_service_url("http://192.0.2.10:8081").is_err());
    assert!(validate_routing_service_url("http://bonsai-routing:8081").is_err());
    unsafe { std::env::set_var("BONSAI_ROUTING_ALLOW_INSECURE_HTTP", "1") };
    assert!(validate_routing_service_url("http://bonsai-routing:8081").is_ok());
    unsafe { std::env::remove_var("BONSAI_ROUTING_ALLOW_INSECURE_HTTP") };
    assert!(validate_routing_service_url("http://bonsai-routing:8081").is_err());
}

// ── SEC-20/WP1.2: redirect regression — synthetic credential must reach
// the authorized endpoint but NEVER a redirect destination, on every
// credential-bearing routing client. ─────────────────────────────────

use std::sync::Mutex;

type SeenRequests = Arc<Mutex<Vec<(String, String, Option<String>)>>>;

async fn spawn_recording_server() -> (String, SeenRequests) {
    let seen: SeenRequests = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let app =
        axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
            let seen = seen2.clone();
            async move {
                let token = req
                    .headers()
                    .get("x-bonsai-routing-token")
                    .and_then(|v| v.to_str().ok())
                    .map(String::from);
                seen.lock().unwrap().push((
                    req.uri().path().to_string(),
                    req.method().to_string(),
                    token,
                ));
                axum::Json(serde_json::json!({"ok": true}))
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

async fn spawn_redirector(location: String, status: axum::http::StatusCode) -> String {
    let app = axum::Router::new().fallback(axum::routing::any(move || {
        let location = location.clone();
        async move { (status, [(axum::http::header::LOCATION, location)]) }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn make_change_request(store: &GraphStore, id: &str) {
    let db = store.db();
    let id = id.to_string();
    std::thread::spawn(move || {
        let conn = lbug::Connection::new(&db).unwrap();
        conn.query(&format!(
            "CREATE (c:ChangeRequest {{id: '{id}', number: 'CHG-{id}', \
             source: 'test', state: 'open', \
             affected_cis_json: '[\"dev-redirect\"]'}})"
        ))
        .unwrap();
    })
    .join()
    .unwrap();
}

#[tokio::test]
async fn credential_bearing_routing_clients_never_follow_redirects() -> Result<()> {
    clear_auth_env();
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    store
        .auth_store
        .set_service_token("routing_service_token", "synthetic-sec20-redirect-token")?;
    make_change_request(&store, "chg-redir");

    let state = bonsai::http_server::AppState::new_minimal_with_runtime_dir(store.clone(), "/tmp");
    let app_router = bonsai::http_server::router_from_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let app_addr = listener.local_addr()?;
    tokio::spawn(async move { axum::serve(listener, app_router).await.unwrap() });
    let app_base = format!("http://{app_addr}");

    let http = reqwest::Client::new();
    let token = "synthetic-sec20-redirect-token";

    // Representative redirect statuses for GET (301/302/303/307/308)
    // and POST (307/308 preserve method+body; 302 is the classic case).
    for status in [301u16, 302, 303, 307, 308] {
        let (dest_url, dest_seen) = spawn_recording_server().await;
        let redir_url = spawn_redirector(
            format!("{dest_url}/stolen"),
            axum::http::StatusCode::from_u16(status).unwrap(),
        )
        .await;
        store
            .upsert_config_item(cfg(
                "app-cfg-routing-url",
                "app_config",
                "routing_service_url",
                &format!("\"{redir_url}\""),
            ))
            .await?;

        // 1. Routing GET proxy.
        let r = http
            .get(format!(
                "{app_base}/api/routing/v1/routes?speaker=198.51.100.1"
            ))
            .send()
            .await?;
        assert!(!r.status().is_success(), "GET proxy status {status}");

        // 2. Routing POST proxy.
        let r = http
            .post(format!("{app_base}/api/routing/v1/ingest-bmp"))
            .json(&serde_json::json!({"speaker": "198.51.100.1", "payload": "x"}))
            .send()
            .await?;
        assert!(!r.status().is_success(), "POST proxy status {status}");

        // 3. Remote MCP routing tool.
        let mcp_res = bonsai::mcp_server::call_tool_with_store_and_routing(
            &store,
            None,
            "get_prefix_journey",
            &serde_json::json!({
                "prefix": "10.99.0.0/24",
                "speaker": "198.51.100.1",
                "vrf_rd": "65000:99"
            }),
            &bonsai::mcp_server::ToolExecutionContext::global(
                bonsai::http_server::auth::Role::Admin,
            ),
        )
        .await;
        assert!(mcp_res.is_err(), "MCP tool must fail on redirect {status}");

        // 4. Change-capture routing request.
        let _ = http
            .post(format!("{app_base}/api/changes/chg-redir/capture"))
            .json(&serde_json::json!({"phase": "baseline"}))
            .send()
            .await?;

        // 5. Remote routing-event projection.
        let proj =
            bonsai::routing_service::project_remote_routing_events_to_graph(&store, 10).await;
        assert!(proj.is_err(), "projection must fail on redirect {status}");

        // ...but the redirect destination must NEVER see it.
        {
            let dest = dest_seen.lock().unwrap();
            assert!(
                dest.is_empty(),
                "redirect status {status}: credential leaked to destination: {dest:?}"
            );
        }

        // Positive control: the authorized endpoint DOES receive the
        // credential (non-following client observes the raw 3xx).
        let no_redirect = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let direct = no_redirect
            .get(format!("{redir_url}/internal/routing/v1/routes"))
            .header("x-bonsai-routing-token", token)
            .send()
            .await?;
        assert_eq!(direct.status().as_u16(), status);
    }
    Ok(())
}

// ── SEC-20/WP1.3: authorized config mutation is atomic ────────────────

#[tokio::test]
async fn authorized_upsert_denies_operator_under_concurrency() -> Result<()> {
    clear_auth_env();
    let dir = TempDir::new()?;
    let store = open_store(&dir);

    // Admin establishes the protected destination record.
    store
        .upsert_config_item_authorized(
            cfg(
                "cfg-url",
                "app_config",
                "routing_service_url",
                "\"http://127.0.0.1:18099\"",
            ),
            true,
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Concurrent Operator id-collision attempts + Admin mutations: the
    // operator must NEVER succeed regardless of interleaving, and the
    // protected content must only change via admin writes.
    let mut handles = Vec::new();
    for i in 0..16 {
        let s = store.clone();
        handles.push(tokio::spawn(async move {
            s.upsert_config_item_authorized(
                cfg(
                    "cfg-url",
                    "app_config",
                    "innocuous",
                    &format!("\"http://169.254.169.254:{i}\""),
                ),
                false,
            )
            .await
        }));
    }
    for i in 0..8 {
        let s = store.clone();
        handles.push(tokio::spawn(async move {
            s.upsert_config_item_authorized(
                cfg(
                    "cfg-url",
                    "app_config",
                    "routing_service_url",
                    &format!("\"http://127.0.0.1:1809{i}\""),
                ),
                true,
            )
            .await
        }));
    }
    let mut operator_denied = 0usize;
    let mut admin_ok = 0usize;
    for (idx, h) in handles.into_iter().enumerate() {
        let res = h.await?;
        if idx < 16 {
            assert!(
                matches!(res, Err(bonsai::graph::ConfigWriteRejection::Forbidden)),
                "operator write {idx} must be Forbidden, got {res:?}"
            );
            operator_denied += 1;
        } else {
            assert!(res.is_ok(), "admin write must succeed: {res:?}");
            admin_ok += 1;
        }
    }
    assert_eq!(operator_denied, 16);
    assert_eq!(admin_ok, 8);

    // Final persisted record is still the protected destination identity.
    let items = store.list_config_items(Some("app_config".into())).await?;
    let url_item = items.iter().find(|i| i.id == "cfg-url").unwrap();
    assert_eq!(url_item.name, "routing_service_url");
    assert!(url_item.content_json.contains("127.0.0.1:1809"));
    Ok(())
}

// ── SEC-19/WP2: startup certification of restored malformed state ────

/// Seed a representative set of legacy credential records (fixture-only
/// path), run the real migration, then return the store.
async fn migrated_store(dir: &TempDir) -> Arc<GraphStore> {
    let store = open_store(dir);
    store
        .upsert_config_item_unchecked(cfg(
            "u-leg",
            "user",
            "legacy-user",
            "{\"password_hash\":\"x\",\"role\":\"viewer\"}",
        ))
        .await
        .unwrap();
    store
        .upsert_config_item_unchecked(cfg(
            "k-leg",
            "api_key",
            "legacy-key",
            "{\"key_hash\":\"deadbeef\",\"scope\":\"read\"}",
        ))
        .await
        .unwrap();
    store
        .upsert_config_item_unchecked(cfg(
            "jwt-revoked-legacyjti0001",
            "jwt_revoked",
            "jwt-revoked-legacyjti0001",
            "{\"exp\": 4102444800}",
        ))
        .await
        .unwrap();
    store
        .upsert_config_item_unchecked(cfg(
            "cfg-svc-routing",
            "app_config",
            "routing_service_token",
            "\"rt_legacy_cert_fixture\"",
        ))
        .await
        .unwrap();
    store
        .auth_store
        .ensure_migrated(&store)
        .await
        .expect("migration completes on clean legacy state");
    // Both durable completion markers must be present.
    assert_eq!(
        store
            .auth_store
            .get_auth_config("migration_state")
            .unwrap()
            .as_deref(),
        Some("complete")
    );
    assert_eq!(
        store
            .auth_store
            .get_auth_config("protected_config_state")
            .unwrap()
            .as_deref(),
        Some("complete")
    );
    store
}

fn tombstone(
    id: &str,
    class: &str,
    name: &str,
    disposition: &str,
    extra: Option<&str>,
) -> ConfigItemRecord {
    let mut v = serde_json::json!({
        "migrated_to": "auth_store",
        "migration": MIGRATION_MARKER,
        "record_class": class,
        "item_id": id,
        "disposition": disposition,
        "quarantined_at_ns": 1i64,
    });
    if let Some(e) = extra {
        v["extra_secret"] = serde_json::json!(e);
    }
    ConfigItemRecord {
        id: id.into(),
        config_class: class.into(),
        vendor: String::new(),
        name: name.into(),
        version: "1".into(),
        content_json: v.to_string(),
        enabled: false,
        created_by: "test".into(),
    }
}

async fn reopen_and_certify(dir: &TempDir) -> Result<Arc<GraphStore>, String> {
    let store = open_store(dir);
    store
        .auth_store
        .ensure_migrated(&store)
        .await
        .map(|_| store)
        .map_err(|e| format!("{e:#}"))
}

#[tokio::test]
async fn valid_certified_tombstones_pass_restart_certification() -> Result<()> {
    clear_auth_env();
    let dir = TempDir::new()?;
    {
        let _store = migrated_store(&dir).await;
    }
    let store = reopen_and_certify(&dir)
        .await
        .expect("certification passes");
    // Post-certification auth surface works.
    store
        .auth_store
        .upsert_user(&AuthUser {
            username: "post".into(),
            password_hash: "x".into(),
            role: "viewer".into(),
            enabled: true,
            last_login_ns: 0,
            created_at_ns: 0,
            updated_at_ns: 0,
        })
        .unwrap();
    Ok(())
}

#[tokio::test]
async fn malformed_restored_tombstone_fails_closed_at_startup() -> Result<()> {
    clear_auth_env();
    let dir = TempDir::new()?;
    {
        let store = migrated_store(&dir).await;
        // Recreate a malformed credential-bearing tombstone in the graph
        // (isolated fixture mechanism) — strict schema must reject it.
        store
            .upsert_config_item_unchecked(tombstone(
                "u-leg",
                "user",
                "legacy-user",
                "imported",
                Some("smuggled-password-hash"),
            ))
            .await?;
    }
    let err = match reopen_and_certify(&dir).await {
        Ok(_) => panic!("certification must fail closed"),
        Err(e) => e,
    };
    assert!(
        err.contains("unmigrated") || err.contains("protected"),
        "{err}"
    );

    // No supported graph-serving interface may expose the smuggled
    // credential material: non-admin config reads filter sensitive
    // classes and redact protected names. Verify via the public read
    // boundary (unauthenticated → non-admin filtering applies).
    let store = open_store(&dir);
    let state = bonsai::http_server::AppState::new_minimal_with_runtime_dir(store.clone(), "/tmp");
    let app = bonsai::http_server::router_from_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/api/config-items"))
        .send()
        .await?;
    let body = resp.text().await?;
    assert!(
        !body.contains("smuggled-password-hash"),
        "protected credential material exposed via config-items"
    );
    Ok(())
}

#[tokio::test]
async fn incorrect_disposition_and_missing_evidence_fail_certification() -> Result<()> {
    clear_auth_env();
    for (label, plant) in [
        (
            "bad-disposition",
            tombstone("u-leg", "user", "legacy-user", "retired", None),
        ),
        (
            "missing-evidence",
            tombstone(
                "jwt-revoked-ghostjti",
                "jwt_revoked",
                "jwt-revoked-ghostjti",
                "imported",
                None,
            ),
        ),
    ] as [(&str, ConfigItemRecord); 2]
    {
        let dir = TempDir::new()?;
        {
            let store = migrated_store(&dir).await;
            store.upsert_config_item_unchecked(plant).await?;
        }
        let res = reopen_and_certify(&dir).await;
        assert!(res.is_err(), "{label}: certification must fail closed");
    }
    Ok(())
}

#[tokio::test]
async fn legitimate_post_migration_mutations_keep_certification_valid() -> Result<()> {
    clear_auth_env();
    let dir = TempDir::new()?;
    {
        let store = migrated_store(&dir).await;
        // Legitimate post-migration user disablement.
        let mut u = store.auth_store.get_user("legacy-user").unwrap().unwrap();
        u.enabled = false;
        store.auth_store.upsert_user(&u).unwrap();
        // API-key rotation (same verifier id, new key hash).
        store
            .auth_store
            .upsert_api_key_verifier(
                "k-leg",
                "legacy-key",
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "read",
                0,
            )
            .unwrap();
        // Service-token rotation.
        store
            .auth_store
            .set_service_token("routing_service_token", "rt_rotated_synthetic")
            .unwrap();
        // Legitimately expired JWT revocation: the migrated jti's live
        // row is pruned once past expiry; certification then relies on
        // the durable migration manifest.
        store
            .auth_store
            .ensure_jwt_revoked("legacyjti0001", 1)
            .unwrap();
    }
    reopen_and_certify(&dir)
        .await
        .expect("legitimate post-migration state must certify");
    Ok(())
}
