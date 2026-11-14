//! BN-022: credential confidentiality / graph read boundary.
//!
//! Auth material (JWT signing secret, password verifiers, API-key hashes,
//! sessions) lives in graph `ConfigItem` records. Ad-hoc graph query
//! surfaces — HTTP Explorer, MCP `query_graph`, NL `run_cypher`, saved
//! verify queries — must fail closed for non-admin principals, and the
//! `/api/config-items` endpoints must not disclose or accept writes to
//! credential-bearing classes below Admin.
//!
//! All secrets used here are synthetic markers; no real credential material
//! is read, printed, or exfiltrated.

use std::sync::Arc;

use bonsai::graph::{ConfigItemRecord, GraphStore};
use bonsai::http_server::{build_public_routing_web_router, mint_jwt};
use tokio::net::TcpListener;

const SYNTHETIC_SECRET: &str = "BN022-SYNTHETIC-SECRET-MARKER-9f8e7d6c";
const SYNTHETIC_PASSWORD_HASH: &str = "bn022-synthetic-password-hash-marker";
const SYNTHETIC_KEY_HASH: &str = "bn022-synthetic-api-key-hash-marker";

async fn seed_credential_items(store: &GraphStore) {
    for (id, class, name, content) in [
        (
            "bn022-user-alice",
            "user",
            "alice",
            format!(
                r#"{{"password_hash":"{SYNTHETIC_PASSWORD_HASH}","role":"viewer","last_login_ns":0}}"#
            ),
        ),
        (
            "bn022-apikey-ci",
            "api_key",
            "ci-key",
            format!(
                r#"{{"key_hash":"{SYNTHETIC_KEY_HASH}","scope":"*","expires_at_ns":0,"last_used_at_ns":0}}"#
            ),
        ),
        (
            "bn022-jwt-secret",
            "jwt_secret",
            "jwt-secret-v1",
            SYNTHETIC_SECRET.to_string(),
        ),
        (
            "bn022-appcfg",
            "app_config",
            "require_auth",
            "\"1\"".to_string(),
        ),
    ] {
        store
            .upsert_config_item(ConfigItemRecord {
                id: id.to_string(),
                config_class: class.to_string(),
                vendor: String::new(),
                name: name.to_string(),
                version: "1".to_string(),
                content_json: content,
                enabled: true,
                created_by: "bn022-test".to_string(),
            })
            .await
            .expect("seed config item");
    }
    // A benign device row so legitimate diagnostic queries have data.
    let db = store.db();
    tokio::task::spawn_blocking(move || {
        let conn = lbug::Connection::new(&db).expect("conn");
        conn.query(
            "CREATE (d:Device {address: '198.51.100.10', hostname: 'bn022-leaf', vendor: 'test'})",
        )
        .expect("create device");
    })
    .await
    .expect("spawn");
}

struct TestBed {
    addr: std::net::SocketAddr,
    store: Arc<GraphStore>,
    client: reqwest::Client,
    // Keeps the graph DB directory alive for the life of the test.
    _dir: tempfile::TempDir,
}

async fn setup() -> TestBed {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph.db").to_str().unwrap(),
            128 * 1024 * 1024,
        )
        .expect("open graph store"),
    );
    seed_credential_items(&store).await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = build_public_routing_web_router(Arc::clone(&store));
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    TestBed {
        addr,
        store,
        client: reqwest::Client::new(),
        _dir: dir,
    }
}

impl TestBed {
    async fn token(&self, user: &str, role: &str) -> String {
        mint_jwt(user, role, &self.store).await.expect("mint jwt")
    }

    async fn explorer_query(&self, token: Option<&str>, cypher: &str) -> reqwest::Response {
        let mut req = self
            .client
            .post(format!("http://{}/api/explorer/query", self.addr))
            .json(&serde_json::json!({"cypher": cypher}));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        req.send().await.unwrap()
    }

    async fn mcp_query_graph(&self, token: Option<&str>, cypher: &str) -> reqwest::Response {
        let mut req =
            self.client
                .post(format!("http://{}/mcp", self.addr))
                .json(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {"name": "query_graph", "arguments": {"cypher": cypher}}
                }));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        req.send().await.unwrap()
    }
}

/// 1. Viewer cannot retrieve credential material through MCP query_graph.
#[tokio::test]
async fn bn022_viewer_cannot_query_configitem_via_mcp() {
    let bed = setup().await;
    let viewer = bed.token("v-user", "viewer").await;

    for cypher in [
        "MATCH (c:ConfigItem) RETURN c.content_json",
        "MATCH (c:ConfigItem {config_class: 'user'}) RETURN c",
        "MATCH (c) RETURN c",
        "MATCH (d:Device) RETURN d UNION MATCH (c:ConfigItem) RETURN c",
    ] {
        let resp = bed.mcp_query_graph(Some(&viewer), cypher).await;
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(
            body.get("error").is_some() || body["result"]["isError"] == true,
            "viewer query_graph must fail for '{cypher}': {body}"
        );
        assert!(
            !body.to_string().contains(SYNTHETIC_SECRET)
                && !body.to_string().contains(SYNTHETIC_PASSWORD_HASH)
                && !body.to_string().contains(SYNTHETIC_KEY_HASH),
            "credential marker leaked via MCP: {body}"
        );
    }

    // Legitimate diagnostic query still works for Viewer.
    let resp = bed
        .mcp_query_graph(Some(&viewer), "MATCH (d:Device) RETURN d.hostname")
        .await;
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["result"].is_object(),
        "viewer legitimate query_graph must succeed: {body}"
    );
}

/// 2. Operator cannot retrieve credential material through the HTTP Explorer.
#[tokio::test]
async fn bn022_operator_cannot_query_configitem_via_explorer() {
    let bed = setup().await;
    let operator = bed.token("op-user", "operator").await;

    for cypher in [
        "MATCH (c:ConfigItem) RETURN c.content_json",
        "MATCH (c:ConfigItem) RETURN c.config_class, c.name",
        "MATCH (n) RETURN n",
        "MATCH (d:Device)-[:HAS_INTERFACE]->(i) RETURN d, i",
    ] {
        let resp = bed.explorer_query(Some(&operator), cypher).await;
        let status = resp.status();
        let text = resp.text().await.unwrap();
        assert_eq!(
            status,
            reqwest::StatusCode::FORBIDDEN,
            "operator explorer query must be denied for '{cypher}': {text}"
        );
        assert!(
            !text.contains(SYNTHETIC_SECRET)
                && !text.contains(SYNTHETIC_PASSWORD_HASH)
                && !text.contains(SYNTHETIC_KEY_HASH),
            "credential marker leaked via Explorer: {text}"
        );
    }

    // Permitted diagnostic query still functions for Operator.
    let resp = bed
        .explorer_query(
            Some(&operator),
            "MATCH (d:Device) RETURN d.hostname, d.address",
        )
        .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let text = resp.text().await.unwrap();
    assert!(text.contains("bn022-leaf"));
}

/// 3. Admin retains Explorer access, but post-migration the seeded
/// legacy credential rows have been imported into AuthStore and scrubbed
/// to tombstones — even an admin graph query cannot recover credential
/// material. Mutation remains rejected even for admin.
#[tokio::test]
async fn bn022_admin_explorer_access_preserved() {
    let bed = setup().await;
    let admin = bed.token("admin-user", "admin").await;

    // First request triggers ensure_migrated → legacy rows quarantined
    // and scrubbed. A benign query confirms the surface works.
    let resp = bed
        .explorer_query(Some(&admin), "MATCH (d:Device) RETURN d.hostname")
        .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let resp = bed
        .explorer_query(
            Some(&admin),
            "MATCH (c:ConfigItem {config_class: 'jwt_secret'}) RETURN c.content_json",
        )
        .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let text = resp.text().await.unwrap();
    assert!(
        !text.contains(SYNTHETIC_SECRET),
        "credential material must not survive migration even for admin: {text}"
    );
    assert!(
        text.contains("migrated_to"),
        "scrubbed row should remain as an audit tombstone: {text}"
    );

    // Mutation remains rejected even for admin.
    let resp = bed
        .explorer_query(Some(&admin), "MATCH (c:ConfigItem) DELETE c")
        .await;
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
}

/// 4. /api/config-items does not disclose credential classes below Admin.
#[tokio::test]
async fn bn022_config_items_endpoint_filters_sensitive_classes() {
    let bed = setup().await;
    let viewer = bed.token("v-user", "viewer").await;
    let operator = bed.token("op-user", "operator").await;

    // Viewer asking for a sensitive class directly → 403.
    let resp = bed
        .client
        .get(format!("http://{}/api/config-items?class=user", bed.addr))
        .bearer_auth(&viewer)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);

    // Operator listing everything → 200 but no credential classes/markers.
    let resp = bed
        .client
        .get(format!("http://{}/api/config-items", bed.addr))
        .bearer_auth(&operator)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let text = resp.text().await.unwrap();
    for marker in [
        "user",
        "api_key",
        "jwt_secret",
        SYNTHETIC_SECRET,
        SYNTHETIC_PASSWORD_HASH,
        SYNTHETIC_KEY_HASH,
    ] {
        // 'user'/'api_key' appear as config_class values; ensure none of the
        // sensitive class rows leaked (app_config require_auth must remain).
        assert!(
            !text.contains(&format!("\"config_class\":\"{marker}\"")) && !text.contains(marker),
            "sensitive marker '{marker}' leaked in config-items: {text}"
        );
    }
    assert!(
        text.contains("require_auth"),
        "benign class must remain: {text}"
    );

    // Operator writing a credential class → 403.
    let resp = bed
        .client
        .post(format!("http://{}/api/config-items", bed.addr))
        .bearer_auth(&operator)
        .json(&serde_json::json!({
            "id": "bn022-forged",
            "config_class": "user",
            "vendor": "",
            "name": "mallory",
            "version": "1",
            "content_json": "{\"password_hash\":\"forged\",\"role\":\"admin\",\"last_login_ns\":0}",
            "enabled": true,
            "created_by": "attacker"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);

    // Admin writing a retired credential class → 410 GONE: auth material
    // is managed exclusively through /api/auth/* + AuthStore.
    let admin = bed.token("admin-user", "admin").await;
    let resp = bed
        .client
        .post(format!("http://{}/api/config-items", bed.addr))
        .bearer_auth(&admin)
        .json(&serde_json::json!({
            "id": "bn022-forged-admin",
            "config_class": "api_key",
            "vendor": "",
            "name": "forged",
            "version": "1",
            "content_json": "{\"key_hash\":\"forged\"}",
            "enabled": true,
            "created_by": "attacker"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::GONE,
        "admin credential-class write must be retired: {}",
        resp.status()
    );

    // Operator writing a benign class still works.
    let resp = bed
        .client
        .post(format!("http://{}/api/config-items", bed.addr))
        .bearer_auth(&operator)
        .json(&serde_json::json!({
            "id": "bn022-pattern",
            "config_class": "syslog_pattern",
            "vendor": "",
            "name": "bn022-test-pattern",
            "version": "1",
            "content_json": "[]",
            "enabled": true,
            "created_by": "bn022-test"
        }))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "benign config class write must succeed: {}",
        resp.status()
    );
}

/// 5. Unauthenticated callers get 401 on the protected surfaces.
#[tokio::test]
async fn bn022_unauthenticated_denied() {
    let bed = setup().await;
    let resp = bed
        .explorer_query(None, "MATCH (c:ConfigItem) RETURN c.content_json")
        .await;
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}
