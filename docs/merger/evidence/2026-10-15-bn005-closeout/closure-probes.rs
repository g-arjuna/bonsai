use bonsai::graph::{ConfigItemRecord, GraphStore};
use bonsai::http_server::build_public_routing_web_router;

use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::net::TcpListener;
async fn config(store: &GraphStore, name: &str, value: &str) {
    store
        .upsert_config_item(ConfigItemRecord {
            id: format!("audit-{name}"),
            config_class: "app_config".into(),
            vendor: String::new(),
            name: name.into(),
            version: "1".into(),
            content_json: json!(value).to_string(),
            enabled: true,
            created_by: "audit".into(),
        })
        .await
        .unwrap();
}
fn store(dir: &tempfile::TempDir) -> Arc<GraphStore> {
    Arc::new(
        GraphStore::open(
            dir.path().join("graph.db").to_str().unwrap(),
            128 * 1024 * 1024,
        )
        .unwrap(),
    )
}
async fn web(store: Arc<GraphStore>) -> (String, tokio::task::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let h = tokio::spawn(async move {
        axum::serve(l, build_public_routing_web_router(store))
            .await
            .unwrap();
    });
    (url, h)
}

#[tokio::test]
async fn residual_scope_shapes_read_global_inventory() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    config(&store, "require_auth", "1").await;
    let db = store.db();
    {
        let conn = lbug::Connection::new(&db).unwrap();
        conn.query("CREATE (d:Device {address:'192.0.2.2', hostname:'outside-device'})")
            .unwrap();
    }
    let cases = [
        ("tenant", "routing:read:tenant_id=acme"),
        ("vrf", "routing:read:vrf_rd=65000:100"),
        ("malformed", "routing:read:speaker=192.0.2.1,unknown=value"),
    ];
    for (id, scope) in cases {
        let key = format!("bsk_audit_{id}_12345");
        store.upsert_config_item(ConfigItemRecord {id:format!("key-{id}"),config_class:"api_key".into(),vendor:String::new(),name:id.into(),version:"1".into(),content_json:json!({"key_hash":hex::encode(Sha256::digest(key.as_bytes())),"scope":scope,"expires_at_ns":0,"last_used_at_ns":0}).to_string(),enabled:true,created_by:"review".into()}).await.unwrap();
    }
    let (url, h) = web(store).await;
    let c = reqwest::Client::new();
    for (id, scope) in cases {
        let key = format!("bsk_audit_{id}_12345");
        let args = if id == "malformed" {
            json!({"cypher":"MATCH (d:Device) RETURN d.address"})
        } else {
            json!({})
        };
        let name = if id == "malformed" {
            "query_graph"
        } else {
            "query_devices"
        };
        let res=c.post(format!("{url}/mcp")).bearer_auth(key).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}})).send().await.unwrap();
        assert_eq!(res.status(), 200, "{scope}");
        let body = res.json::<serde_json::Value>().await.unwrap();
        assert!(body.get("result").is_some(), "{id}: {body}");
        assert!(body.to_string().contains("192.0.2.2"), "{id}: {body}");
        println!("Observed unrestricted inventory for scope kind {id}");
    }
    h.abort();
}

#[tokio::test]
async fn speaker_scoped_viewer_reads_unscoped_http_detections() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    config(&store, "require_auth", "1").await;
    let db = store.db();
    {
        let conn = lbug::Connection::new(&db).unwrap();
        conn.query("CREATE (d:Device {address:'192.0.2.2', hostname:'outside-device'})")
            .unwrap();
    }
    {
        let conn = lbug::Connection::new(&db).unwrap();
        conn.query("CREATE (de:DetectionEvent {id:'foreign-http-detection', device_address:'192.0.2.2', rule_id:'bgp_session_down', severity:'critical', features_json:'{}', source_types:'[]', latency_ns:0, fired_at:cast('2026-10-15 10:00:00', 'TIMESTAMP_NS')})").unwrap();
    }
    let key = "bsk_http_audit_12345";
    store
        .upsert_config_item(ConfigItemRecord {
            id: "http-audit-key".into(),
            config_class: "api_key".into(),
            vendor: String::new(),
            name: "http-audit".into(),
            version: "1".into(),
            content_json: json!({"key_hash":hex::encode(Sha256::digest(key.as_bytes())),
            "scope":"routing:read:speaker=192.0.2.1,vrf_rd=0:0,tenant_id=acme",
            "expires_at_ns":0,"last_used_at_ns":0})
            .to_string(),
            enabled: true,
            created_by: "review".into(),
        })
        .await
        .unwrap();
    let (url, handle) = web(store).await;
    let response = reqwest::Client::new()
        .get(format!("{url}/api/detections"))
        .bearer_auth(key)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert!(
        body.to_string().contains("foreign-http-detection"),
        "{body}"
    );
    handle.abort();
}

#[test]
fn installed_engine_readonly_transaction_rejects_write() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    conn.query("BEGIN TRANSACTION READ ONLY").unwrap();
    assert!(
        conn.query("CREATE (d:Device {address:'readonly-probe'})")
            .is_err()
    );
    let _ = conn.query("ROLLBACK");
    let mut rows = conn
        .query("MATCH (d:Device {address:'readonly-probe'}) RETURN count(d)")
        .unwrap();
    assert_eq!(format!("{:?}", rows.next().unwrap()[0]), "Int64(0)");
}
