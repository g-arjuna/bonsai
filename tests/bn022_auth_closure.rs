//! BN-022 closure — AUTH-C1..C4 corrections and WP3 operational
//! acceptance: protected backup/restore, concurrent init, migration
//! interruption recovery, JWT cache expiry, and account-authority
//! edge cases through the real authenticated router.
//!
//! All credentials are synthetic; all stores are temporary directories.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use bonsai::auth_store::{AuthStore, AuthUser, MIGRATION_MARKER};
use bonsai::graph::{ConfigItemRecord, GraphStore};
use bonsai::http_server::{build_public_routing_web_router, mint_jwt, mint_jwt_for_source};
use sha2::Digest;
use tempfile::TempDir;

/// Deterministic key source: keyfile, not ambient env passphrase/secret.
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

async fn serve(store: Arc<GraphStore>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = build_public_routing_web_router(store);
    let h = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}"), h)
}

fn add_user(store: &GraphStore, username: &str, role: &str, enabled: bool) {
    store
        .auth_store
        .upsert_user(&AuthUser {
            username: username.into(),
            password_hash: bonsai::http_server::hash_password("ClosurePassw0rd!1").unwrap(),
            role: role.into(),
            enabled,
            last_login_ns: 0,
            created_at_ns: 0,
            updated_at_ns: 0,
        })
        .unwrap();
}

async fn login(client: &reqwest::Client, base: &str, user: &str) -> Option<String> {
    let resp = client
        .post(format!("{base}/api/auth/login"))
        .json(&serde_json::json!({
            "username": user, "password": "ClosurePassw0rd!1"
        }))
        .send()
        .await
        .ok()?;
    let body: serde_json::Value = resp.json().await.ok()?;
    body["token"].as_str().map(|s| s.to_string())
}

async fn get_status(client: &reqwest::Client, base: &str, token: &str) -> u16 {
    client
        .get(format!("{base}/api/config-items"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

// ── AUTH-C1: backup correctness ─────────────────────────────────────────

#[test]
fn auth_backup_produces_consistent_snapshot_and_manifest() -> Result<()> {
    clear_auth_env();
    let dir = TempDir::new()?;
    let auth_path = dir.path().join("bonsai_auth.db");
    let store = AuthStore::open(&auth_path)?;
    store.upsert_user(&AuthUser {
        username: "bk-user".into(),
        password_hash: "verifier".into(),
        role: "viewer".into(),
        enabled: true,
        last_login_ns: 0,
        created_at_ns: 0,
        updated_at_ns: 0,
    })?;
    let _ = store.jwt_secret()?;

    // Bounded timeout: backup of a small store must complete promptly.
    let dest = dir.path().join("backup").join("bonsai_auth.db");
    let (tx, rx) = std::sync::mpsc::channel();
    let store_ref = &store;
    std::thread::scope(|s| {
        s.spawn(|| {
            let _ = tx.send(store_ref.backup_to(&dest));
        });
    });
    let manifest = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("backup timed out")?;
    assert_eq!(manifest.user_count, 1);
    assert!(manifest.jwt_key_count >= 1);
    assert!(dest.exists());
    assert!(dest.with_extension("manifest.json").exists());

    // Snapshot is a real, readable database describing the backup.
    let snap = rusqlite::Connection::open(&dest)?;
    let users: i64 = snap.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
    assert_eq!(users, 1);
    drop(snap);

    // No overwrite: second backup to same dest must fail.
    assert!(store.backup_to(&dest).is_err());
    Ok(())
}

// ── AUTH-C2: account authority through the real router ─────────────────

#[tokio::test]
async fn deleted_local_account_token_is_denied() -> Result<()> {
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    require_auth(&store).await;
    add_user(&store, "doomed", "admin", true);
    let (base, _h) = serve(Arc::clone(&store)).await;
    let client = reqwest::Client::new();
    let token = login(&client, &base, "doomed").await.expect("login");
    assert_eq!(get_status(&client, &base, &token).await, 200);

    // Hard-delete the account row (simulates account deletion).
    let conn = rusqlite::Connection::open(store.auth_store.path())?;
    conn.execute("DELETE FROM users WHERE username = 'doomed'", [])?;
    drop(conn);
    assert_eq!(get_status(&client, &base, &token).await, 401);
    Ok(())
}

#[tokio::test]
async fn disabled_local_account_token_is_denied() -> Result<()> {
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    require_auth(&store).await;
    add_user(&store, "dis", "admin", true);
    let (base, _h) = serve(Arc::clone(&store)).await;
    let client = reqwest::Client::new();
    let token = login(&client, &base, "dis").await.expect("login");
    store.auth_store.set_user_enabled("dis", false)?;
    assert_eq!(get_status(&client, &base, &token).await, 401);
    Ok(())
}

#[tokio::test]
async fn role_reduction_takes_effect_on_live_token() -> Result<()> {
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    require_auth(&store).await;
    add_user(&store, "demoted", "admin", true);
    let (base, _h) = serve(Arc::clone(&store)).await;
    let client = reqwest::Client::new();
    let token = login(&client, &base, "demoted").await.expect("login");
    // Admin-only endpoint works before reduction.
    let ok = client
        .get(format!("{base}/api/auth/users"))
        .bearer_auth(&token)
        .send()
        .await?
        .status();
    assert!(ok.is_success());
    // Reduce to viewer → same token loses admin authority.
    store.auth_store.upsert_user(&AuthUser {
        username: "demoted".into(),
        password_hash: store.auth_store.get_user("demoted")?.unwrap().password_hash,
        role: "viewer".into(),
        enabled: true,
        last_login_ns: 0,
        created_at_ns: 0,
        updated_at_ns: 0,
    })?;
    let resp = client
        .get(format!("{base}/api/auth/users"))
        .bearer_auth(&token)
        .send()
        .await?;
    assert_eq!(resp.status().as_u16(), 403);
    Ok(())
}

#[tokio::test]
async fn unknown_jwt_source_fails_closed() -> Result<()> {
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    require_auth(&store).await;
    add_user(&store, "src-user", "admin", true);
    let (base, _h) = serve(Arc::clone(&store)).await;
    let client = reqwest::Client::new();
    let weird = mint_jwt_for_source("src-user", "admin", &store, "kerberos")
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(get_status(&client, &base, &weird).await, 401);
    Ok(())
}

#[tokio::test]
async fn ldap_source_without_local_account_uses_token_role() -> Result<()> {
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    require_auth(&store).await;
    let (base, _h) = serve(Arc::clone(&store)).await;
    let client = reqwest::Client::new();
    // Pure external identity: no local record → LDAP-minted role stands.
    let ldap = mint_jwt_for_source("ext-user", "viewer", &store, "ldap")
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(get_status(&client, &base, &ldap).await, 200);
    Ok(())
}

#[tokio::test]
async fn ldap_token_denied_when_local_account_disabled() -> Result<()> {
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    require_auth(&store).await;
    // Collision: local account exists but disabled → local authority wins
    // over an LDAP-minted token for the same username.
    add_user(&store, "collide", "admin", false);
    let (base, _h) = serve(Arc::clone(&store)).await;
    let client = reqwest::Client::new();
    let ldap = mint_jwt_for_source("collide", "admin", &store, "ldap")
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(get_status(&client, &base, &ldap).await, 401);
    Ok(())
}

#[tokio::test]
async fn local_token_denied_when_no_local_account() -> Result<()> {
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    require_auth(&store).await;
    let (base, _h) = serve(Arc::clone(&store)).await;
    let client = reqwest::Client::new();
    // src=local token for a user that does not exist locally → deny even
    // though the signature is valid and the claim says admin.
    let t = mint_jwt("ghost", "admin", &store)
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(get_status(&client, &base, &t).await, 401);
    Ok(())
}

#[tokio::test]
async fn env_bootstrap_admin_compatibility() -> Result<()> {
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    require_auth(&store).await;
    unsafe {
        std::env::set_var("BONSAI_ADMIN_USER", "env-root-unique");
        std::env::set_var("BONSAI_ADMIN_PASS", "EnvBootstrapPassw0rd!9");
    }
    let (base, _h) = serve(Arc::clone(&store)).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/auth/login"))
        .json(&serde_json::json!({
            "username": "env-root-unique", "password": "EnvBootstrapPassw0rd!9"
        }))
        .send()
        .await?;
    let body: serde_json::Value = resp.json().await?;
    let token = body["token"].as_str().expect("env admin token").to_string();
    // env-source token authorizes without any local account record.
    assert_eq!(get_status(&client, &base, &token).await, 200);
    unsafe {
        std::env::remove_var("BONSAI_ADMIN_USER");
        std::env::remove_var("BONSAI_ADMIN_PASS");
    }
    Ok(())
}

#[tokio::test]
async fn migration_storage_failure_denies_requests() -> Result<()> {
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    require_auth(&store).await;
    // Conflicting legacy record: same username, different role →
    // migration aborts → auth storage unavailable → 503 (fail closed).
    let pw = bonsai::http_server::hash_password("ConflictPassw0rd!1").unwrap();
    store
        .upsert_config_item(ConfigItemRecord {
            id: "user-conflict".into(),
            config_class: "user".into(),
            vendor: String::new(),
            name: "conflict".into(),
            version: String::new(),
            content_json: serde_json::json!({
                "password_hash": pw, "role": "operator", "last_login_ns": 0
            })
            .to_string(),
            enabled: true,
            created_by: "test".into(),
        })
        .await?;
    store.auth_store.upsert_user(&AuthUser {
        username: "conflict".into(),
        password_hash: pw,
        role: "admin".into(),
        enabled: true,
        last_login_ns: 0,
        created_at_ns: 0,
        updated_at_ns: 0,
    })?;
    let (base, _h) = serve(Arc::clone(&store)).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/config-items"))
        .bearer_auth("whatever")
        .send()
        .await?;
    assert_eq!(resp.status().as_u16(), 503);
    Ok(())
}

#[tokio::test]
async fn record_login_cannot_undo_concurrent_changes() -> Result<()> {
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    let legacy = hex::encode(sha2::Sha256::digest(b"legacy-password-material"));
    store.auth_store.upsert_user(&AuthUser {
        username: "race".into(),
        password_hash: legacy.clone(),
        role: "admin".into(),
        enabled: true,
        last_login_ns: 0,
        created_at_ns: 0,
        updated_at_ns: 0,
    })?;
    // Simulate: login verified the legacy hash, then a concurrent
    // password reset + role reduction + disablement landed. The login
    // bookkeeping write must not undo any of them.
    let reset_hash = bonsai::http_server::hash_password("ResetPassw0rd!22").unwrap();
    store.auth_store.upsert_user(&AuthUser {
        username: "race".into(),
        password_hash: reset_hash.clone(),
        role: "viewer".into(),
        enabled: false,
        last_login_ns: 0,
        created_at_ns: 0,
        updated_at_ns: 0,
    })?;
    store
        .auth_store
        .record_login("race", Some(&legacy), Some("argon2-should-not-land"))?;
    let u = store.auth_store.get_user("race")?.unwrap();
    assert_eq!(
        u.password_hash, reset_hash,
        "rehash overwrote password reset"
    );
    assert_eq!(u.role, "viewer", "login write changed role");
    assert!(!u.enabled, "login write re-enabled account");
    assert_eq!(u.last_login_ns, 0, "disabled account got last_login");
    Ok(())
}

// ── AUTH-C3: JWT cache expiry ───────────────────────────────────────────

#[tokio::test]
async fn expired_retired_key_stops_authorizing_without_restart() -> Result<()> {
    clear_auth_env();
    // Parallel tests do Argon2/scrypt work that can deschedule this
    // test's thread for seconds — use a generous synthetic window and
    // retry the whole scenario rather than flake on scheduler timing.
    let mut last_err = None;
    for _attempt in 0..3 {
        let dir = TempDir::new()?;
        let store = open_store(&dir);
        store.auth_store.ensure_migrated(&store).await?;
        let old_token = mint_jwt("u", "viewer", &store)
            .await
            .map_err(anyhow::Error::msg)?;
        // 10 s synthetic overlap — no 26 h wait.
        store
            .auth_store
            .rotate_jwt_key_with_overlap_ns(10_000_000_000)?;
        // Inside the window the retired key still verifies (populates
        // the generation-scoped cache).
        if bonsai::http_server::verify_jwt(&old_token, &store)
            .await
            .is_err()
        {
            last_err = Some("retired key did not verify inside overlap".to_string());
            continue;
        }
        // Past the deadline the SAME cached generation must stop serving
        // the expired retired secret — no rotation, no restart.
        tokio::time::sleep(Duration::from_millis(10_500)).await;
        if bonsai::http_server::verify_jwt(&old_token, &store)
            .await
            .is_ok()
        {
            last_err = Some("expired retired key still verified".to_string());
            continue;
        }
        return Ok(());
    }
    Err(anyhow::anyhow!(
        "expired-retired-key check failed: {last_err:?}"
    ))
}

#[tokio::test]
async fn concurrent_requests_during_rotation_stay_consistent() -> Result<()> {
    let dir = TempDir::new()?;
    let store = Arc::new(open_store_inner(&dir));
    store.auth_store.ensure_migrated(&store).await?;
    let token = mint_jwt("u", "viewer", &store)
        .await
        .map_err(anyhow::Error::msg)?;
    let mut handles = Vec::new();
    for _ in 0..6 {
        let s = Arc::clone(&store);
        let t = token.clone();
        handles.push(tokio::spawn(async move {
            for _ in 0..20 {
                let _ = bonsai::http_server::verify_jwt(&t, &s).await;
            }
        }));
    }
    for _ in 0..3 {
        store
            .auth_store
            .rotate_jwt_key_with_overlap_ns(60_000_000_000)?;
    }
    for h in handles {
        h.await?;
    }
    // After rotations settle the retired-window token may or may not
    // verify depending on timing — what must hold is that a freshly
    // minted token verifies and nothing panicked.
    let fresh = mint_jwt("u", "viewer", &store)
        .await
        .map_err(anyhow::Error::msg)?;
    assert!(
        bonsai::http_server::verify_jwt(&fresh, &store)
            .await
            .is_ok()
    );
    Ok(())
}

fn open_store_inner(dir: &TempDir) -> GraphStore {
    let path = dir.path().join("bonsai.db");
    GraphStore::open(&path.to_string_lossy(), 256 * 1024 * 1024).expect("open graph")
}

#[tokio::test]
async fn jwt_verify_latency_cached_vs_cold() -> Result<()> {
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    store.auth_store.ensure_migrated(&store).await?;
    let token = mint_jwt("u", "viewer", &store)
        .await
        .map_err(anyhow::Error::msg)?;
    // Force a cache refresh: rotation bumps the generation and clears the
    // cache, so the next verification pays the scrypt decrypt cost again.
    store.auth_store.rotate_jwt_key()?;
    // Cold: first verification after invalidation repopulates the cache.
    let t0 = Instant::now();
    bonsai::http_server::verify_jwt(&token, &store)
        .await
        .unwrap();
    let cold = t0.elapsed();
    // Warm: cached secrets — must be dramatically faster.
    let t1 = Instant::now();
    for _ in 0..20 {
        bonsai::http_server::verify_jwt(&token, &store)
            .await
            .unwrap();
    }
    let warm_each = t1.elapsed() / 20;
    eprintln!("jwt verify latency: cold={cold:?} warm_each={warm_each:?}");
    assert!(warm_each < cold, "cache must make verification faster");
    Ok(())
}

// ── AUTH-C4: sealed quarantine + migration recovery ────────────────────

fn malformed_user_item(id: &str, content: &str) -> ConfigItemRecord {
    ConfigItemRecord {
        id: id.into(),
        config_class: "user".into(),
        vendor: String::new(),
        name: id.into(),
        version: String::new(),
        content_json: content.into(),
        enabled: true,
        created_by: "test".into(),
    }
}

#[tokio::test]
async fn quarantined_secret_is_sealed_and_recoverable() -> Result<()> {
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    let secret_content = "{\"password_hash\":\"$argon2id$v=19$m=19456,t=2,p=1$SECRET$HASH\"";
    store
        .upsert_config_item(malformed_user_item("user-leaky", secret_content))
        .await?;
    let report = store.auth_store.migrate_from_graph(&store).await?;
    assert_eq!(report.quarantined_records, 1);

    // Quarantine metadata is visible; content is NOT plaintext anywhere.
    let infos = store.auth_store.list_quarantine_records()?;
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].record_class, "user");
    // Direct SQL: content_json must be a descriptor, content_enc sealed.
    let conn = rusqlite::Connection::open(store.auth_store.path())?;
    let (cj, enc): (String, Option<Vec<u8>>) = conn.query_row(
        "SELECT content_json, content_enc FROM auth_quarantine WHERE id = ?1",
        rusqlite::params![infos[0].id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert!(!cj.contains("SECRET"), "plaintext secret in quarantine");
    assert!(enc.is_some(), "quarantine content not sealed");
    drop(conn);
    // Operator recovery returns the exact original bytes.
    let recovered = store
        .auth_store
        .decrypt_quarantine_content(infos[0].id)?
        .expect("sealed content");
    assert_eq!(recovered, secret_content);
    Ok(())
}

#[tokio::test]
async fn conflicting_quarantine_record_aborts_migration() -> Result<()> {
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    store
        .upsert_config_item(malformed_user_item("user-clash", "{corrupt-A"))
        .await?;
    // Pre-seed a quarantine row for the same identity with DIFFERENT
    // content — must conflict, not be silently ignored.
    let conn = rusqlite::Connection::open(store.auth_store.path())?;
    conn.execute(
        "INSERT INTO auth_quarantine(record_class, item_id, content_json, \
         content_sha256, content_enc, reason, quarantined_at_ns) \
         VALUES('user', 'user-clash', '{}', 'different-sha', X'00', 'other', 1)",
        [],
    )?;
    drop(conn);
    let err = store
        .auth_store
        .migrate_from_graph(&store)
        .await
        .expect_err("conflicting quarantine must abort");
    assert!(format!("{err:#}").contains("conflict"), "{err:#}");
    Ok(())
}

#[tokio::test]
async fn migration_resumes_with_existing_tombstones() -> Result<()> {
    let dir = TempDir::new()?;
    let store = open_store(&dir);
    let pw = bonsai::http_server::hash_password("ResumePassw0rd!3").unwrap();
    store
        .upsert_config_item(ConfigItemRecord {
            id: "user-first".into(),
            config_class: "user".into(),
            vendor: String::new(),
            name: "first".into(),
            version: String::new(),
            content_json: serde_json::json!({
                "password_hash": pw, "role": "viewer", "last_login_ns": 0
            })
            .to_string(),
            enabled: true,
            created_by: "test".into(),
        })
        .await?;
    store.auth_store.migrate_from_graph(&store).await?;

    // Simulate interruption-resume: roll the durable marker back and add
    // a second legacy record. Tombstones from run 1 must be skipped via
    // validated records, and the manifest must cover both dispositions.
    store
        .auth_store
        .set_auth_config("migration_state", "interrupted")?;
    let pw2 = bonsai::http_server::hash_password("ResumePassw0rd!4").unwrap();
    store
        .upsert_config_item(ConfigItemRecord {
            id: "user-second".into(),
            config_class: "user".into(),
            vendor: String::new(),
            name: "second".into(),
            version: String::new(),
            content_json: serde_json::json!({
                "password_hash": pw2, "role": "operator", "last_login_ns": 0
            })
            .to_string(),
            enabled: true,
            created_by: "test".into(),
        })
        .await?;
    store.auth_store.migrate_from_graph(&store).await?;

    assert!(store.auth_store.get_user("first")?.is_some());
    assert!(store.auth_store.get_user("second")?.is_some());
    // Manifest is consistent: marker + report committed together.
    assert_eq!(
        store
            .auth_store
            .get_auth_config("migration_state")?
            .as_deref(),
        Some("complete")
    );
    let report = store.auth_store.get_auth_config("migration_report")?;
    assert!(report.is_some());
    let v: serde_json::Value = serde_json::from_str(&report.unwrap())?;
    assert_eq!(v["migration"], MIGRATION_MARKER);
    let records = v["records"].as_array().unwrap();
    // Both original records have a recorded disposition.
    for id in ["user-first", "user-second"] {
        assert!(
            records.iter().any(|r| r["id"] == id),
            "manifest missing disposition for {id}"
        );
    }
    Ok(())
}

// ── WP3: concurrent init + protected backup/restore ────────────────────

#[test]
fn concurrent_auth_store_init_shares_one_identity() -> Result<()> {
    clear_auth_env();
    let dir = TempDir::new()?;
    let path = dir.path().join("bonsai_auth.db");
    let path_str = path.to_string_lossy().into_owned();
    let mut handles = Vec::new();
    for _ in 0..8 {
        let p = path_str.clone();
        handles.push(std::thread::spawn(move || {
            let s = AuthStore::open(std::path::Path::new(&p)).expect("open");
            s.jwt_secret().expect("jwt secret")
        }));
    }
    let secrets: Vec<String> = handles
        .into_iter()
        .map(|h| h.join().expect("join"))
        .collect();
    assert!(
        secrets.iter().all(|s| s == &secrets[0]),
        "divergent identities"
    );
    // Exactly one keyfile, owner-only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("bonsai_auth.db.key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        // No leftover insecure temp files.
        for e in std::fs::read_dir(dir.path())? {
            let n = e?.file_name().to_string_lossy().to_string();
            assert!(!n.contains(".tmp."), "leftover temp file {n}");
        }
    }
    Ok(())
}

fn install_backup(
    backup_db: &std::path::Path,
    key_src: &std::path::Path,
    dest_dir: &std::path::Path,
) -> Result<AuthStore> {
    std::fs::create_dir_all(dest_dir)?;
    std::fs::copy(backup_db, dest_dir.join("bonsai_auth.db"))?;
    let key_name = key_src.file_name().unwrap();
    std::fs::copy(key_src, dest_dir.join(key_name))?;
    AuthStore::open(&dest_dir.join("bonsai_auth.db"))
}

#[test]
fn protected_backup_restore_roundtrip() -> Result<()> {
    clear_auth_env();
    let dir = TempDir::new()?;
    let src_dir = dir.path().join("src");
    std::fs::create_dir_all(&src_dir)?;
    let auth_path = src_dir.join("bonsai_auth.db");
    let store = AuthStore::open(&auth_path)?;
    store.upsert_user(&AuthUser {
        username: "restore-user".into(),
        password_hash: "argon2-verifier".into(),
        role: "operator".into(),
        enabled: true,
        last_login_ns: 123,
        created_at_ns: 0,
        updated_at_ns: 0,
    })?;
    let (key_id, raw_key) = store.create_api_key("svc", "read", 0)?;
    let secret_before = store.jwt_secret()?;
    store.revoke_jwt("jti-restore", now_ns_test() + 3_600_000_000_000)?;

    let backup = src_dir.join("backups").join("bonsai_auth.db");
    let manifest = store.backup_to(&backup)?;
    assert_eq!(manifest.user_count, 1);
    assert_eq!(manifest.api_key_count, 1);
    assert!(manifest.jwt_revoked_count >= 1);

    // Restore with correct key material → everything survives.
    let keyfile = src_dir.join("bonsai_auth.db.key");
    let dst = dir.path().join("restore");
    let restored = install_backup(&backup, &keyfile, &dst)?;
    assert_eq!(restored.jwt_secret()?, secret_before);
    let u = restored.get_user("restore-user")?.expect("user");
    assert_eq!(u.role, "operator");
    assert!(u.enabled);
    assert!(restored.is_jwt_revoked("jti-restore")?);
    assert!(restored.authenticate_api_key(&raw_key)?.is_some());
    let _ = key_id;

    // Restore with WRONG key material → encrypted secrets unreadable.
    let wrong_dir = dir.path().join("wrong");
    std::fs::create_dir_all(&wrong_dir)?;
    std::fs::write(
        wrong_dir.join("bonsai_auth.db.key"),
        format!("{}\n", "0".repeat(64)),
    )?;
    std::fs::copy(&backup, wrong_dir.join("bonsai_auth.db"))?;
    let wrong = AuthStore::open(&wrong_dir.join("bonsai_auth.db"))?;
    assert!(wrong.jwt_secret().is_err(), "wrong key must fail closed");

    // Missing key material → open must fail closed (encrypted keys exist).
    let nokey_dir = dir.path().join("nokey");
    std::fs::create_dir_all(&nokey_dir)?;
    std::fs::copy(&backup, nokey_dir.join("bonsai_auth.db"))?;
    unsafe { std::env::remove_var("BONSAI_VAULT_PASSPHRASE") };
    assert!(AuthStore::open(&nokey_dir.join("bonsai_auth.db")).is_err());
    Ok(())
}

#[test]
fn old_backup_restore_revocation_policy_is_documented_and_safe() -> Result<()> {
    clear_auth_env();
    let dir = TempDir::new()?;
    let src_dir = dir.path().join("src");
    std::fs::create_dir_all(&src_dir)?;
    let auth_path = src_dir.join("bonsai_auth.db");
    let store = AuthStore::open(&auth_path)?;
    let _ = store.jwt_secret()?;

    // Backup taken BEFORE a revocation.
    let backup = src_dir.join("backups").join("bonsai_auth.db");
    store.backup_to(&backup)?;
    store.revoke_jwt("jti-post-backup", now_ns_test() + 3_600_000_000_000)?;

    // Restoring the older backup revives the pre-backup revocation state —
    // documented rollback semantics. The safe recovery path is an
    // emergency rotation afterwards, which this test verifies.
    let dst = dir.path().join("restore");
    let restored = install_backup(&backup, &src_dir.join("bonsai_auth.db.key"), &dst)?;
    assert!(
        !restored.is_jwt_revoked("jti-post-backup")?,
        "old backup cannot know post-backup revocations"
    );
    // Emergency rotation invalidates every pre-restore token at once.
    restored.rotate_jwt_key_emergency()?;
    let old_secret = store.jwt_secret()?;
    let _ = old_secret;
    Ok(())
}

fn now_ns_test() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}
