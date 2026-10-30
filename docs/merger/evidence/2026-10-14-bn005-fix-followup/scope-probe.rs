use bonsai::graph::{ConfigItemRecord, GraphStore};
use bonsai::http_server::build_public_routing_web_router;
use bonsai::routing_service::{
    RoutingServiceConfig, RoutingServiceState, build_internal_routing_router,
    sample_bmp_session_frames,
};
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
async fn scoped_mcp_reads_foreign_graph_and_device_changes() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    config(&store, "require_auth", "1").await;
    let db = store.db();
    {
        let conn = lbug::Connection::new(&db).unwrap();
        conn.query("CREATE (d:Device {address:'192.0.2.2'})")
            .unwrap();
        conn.query("CREATE (c:ConfigSnapshot {id:'foreign-snapshot', device_address:'192.0.2.2',trigger:'audit',summary:'foreign-change'})").unwrap();
    }
    store.upsert_config_item(ConfigItemRecord {id:"scoped-review".into(),config_class:"api_key".into(),vendor:String::new(),name:"scoped-review".into(),version:"1".into(),content_json:json!({"key_hash":hex::encode(Sha256::digest(b"bsk_review_scope_12345")),"scope":"routing:read:speaker=192.0.2.1,vrf_rd=0:0,tenant_id=acme","expires_at_ns":0,"last_used_at_ns":0}).to_string(),enabled:true,created_by:"review".into()}).await.unwrap();
    let (url, h) = web(store).await;
    let c = reqwest::Client::new();
    let anonymous = c
        .post(format!("{url}/mcp"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
    for (name, args, expected) in [
        (
            "query_graph",
            json!({"cypher":"MATCH (d:Device {address:'192.0.2.2'}) RETURN d.address"}),
            "192.0.2.2",
        ),
        (
            "get_changes",
            json!({"device_address":"192.0.2.2"}),
            "foreign-change",
        ),
    ] {
        let r=c.post(format!("{url}/mcp")).bearer_auth("bsk_review_scope_12345").json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":args}})).send().await.unwrap();
        assert_eq!(r.status(), 200);
        let body = r.json::<serde_json::Value>().await.unwrap();
        assert!(body.get("result").is_some(), "{body}");
        assert!(body.to_string().contains(expected), "{body}");
    }
    h.abort();
}
