use bonsai::graph::{ConfigItemRecord, GraphStore};
use bonsai::http_server::build_public_routing_web_router;
use bonsai::routing_service::{
    RoutingServiceConfig, RoutingServiceState, build_internal_routing_router,
    sample_bmp_session_frames,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{net::Ipv4Addr, sync::Arc};
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
async fn anonymous_mcp_query_succeeds_while_api_requires_auth() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    config(&store, "require_auth", "1").await;
    let (url, h) = web(store).await;
    let c = reqwest::Client::new();
    assert_eq!(
        c.get(format!("{url}/api/routing/v1/health"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let r=c.post(format!("{url}/mcp")).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"query_graph","arguments":{"cypher":"RETURN 42 AS answer"}}})).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let body = r.json::<serde_json::Value>().await.unwrap();
    assert!(body.get("result").is_some(), "{body}");
    assert!(body.to_string().contains("42"));
    let r = c.post(format!("{url}/mcp")).json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"query_graph","arguments":{"cypher":"WITH '/*' AS marker CREATE (n:Device {address: 'bn005-anonymous-created'}) RETURN n.address"}}})).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let body = r.json::<serde_json::Value>().await.unwrap();
    assert!(body.get("result").is_some(), "{body}");
    assert!(
        body.to_string().contains("bn005-anonymous-created"),
        "{body}"
    );
    h.abort();
}
#[tokio::test]
async fn quote_comment_string_hides_mutation_and_mcp_writes_temp_graph() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let query =
        "WITH '/*' AS marker CREATE (n:Device {address: 'bn005-audit-created'}) RETURN n.address";
    assert!(bonsai::graph::explorer::validate_query(query).is_ok());
    assert!(bonsai::mcp_server::is_readonly_cypher(query));
    let result = bonsai::mcp_server::tool_query_graph_with_store(&store, &json!({"cypher":query}))
        .await
        .unwrap();
    assert!(
        result.to_string().contains("bn005-audit-created"),
        "{result}"
    );
    let check = bonsai::mcp_server::tool_query_graph_with_store(
        &store,
        &json!({"cypher":"MATCH (n:Device {address:'bn005-audit-created'}) RETURN n.address"}),
    )
    .await
    .unwrap();
    assert_eq!(check["row_count"], 1);
}
async fn setup_routing(
    dir: &tempfile::TempDir,
) -> (
    String,
    String,
    reqwest::Client,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
) {
    let store = store(dir);
    config(&store, "require_auth", "1").await;
    let state = RoutingServiceState::new(RoutingServiceConfig {
        bmp_listen_addr: "127.0.0.1:0".into(),
        http_listen_addr: "127.0.0.1:0".into(),
        bmp_journal_path: Some(dir.path().join("bmp.journal")),
        event_journal_path: Some(dir.path().join("events.journal")),
        internal_service_token: Some("audit-internal".into()),
        ..Default::default()
    })
    .await
    .unwrap();
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = format!("http://{}", l.local_addr().unwrap());
    let h = tokio::spawn(async move {
        axum::serve(l, build_internal_routing_router(state))
            .await
            .unwrap();
    });
    config(&store, "routing_service_url", &backend).await;
    config(&store, "routing_service_token", "audit-internal").await;
    store.upsert_config_item(ConfigItemRecord{id:"audit-key".into(),config_class:"api_key".into(),vendor:String::new(),name:"audit-key".into(),version:"1".into(),content_json:json!({"key_hash":hex::encode(Sha256::digest(b"bsk_audit_key_12345678")),"scope":"routing:write:speaker=192.0.2.1,vrf_rd=0:0,tenant_id=acme","expires_at_ns":0,"last_used_at_ns":0}).to_string(),enabled:true,created_by:"audit".into()}).await.unwrap();
    let (url, w) = web(store).await;
    (url, backend, reqwest::Client::new(), h, w)
}
#[tokio::test]
async fn conflicting_speaker_alias_ingests_outside_token_scope() {
    let dir = tempfile::tempdir().unwrap();
    let (url, backend, c, h, w) = setup_routing(&dir).await;
    let (init, _, _) =
        sample_bmp_session_frames(Ipv4Addr::new(192, 0, 2, 1), 65001, [192, 0, 2], 100);
    let r=c.post(format!("{url}/api/routing/v1/ingest-bmp")).bearer_auth("bsk_audit_key_12345678").json(&json!({"speaker":"192.0.2.1","speaker_addr":"192.0.2.2","vrf_rd":"0:0","frame_hex":hex::encode(init),"session_epoch":1})).send().await.unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    let body = c
        .get(format!("{backend}/internal/routing/v1/speakers"))
        .header("x-bonsai-routing-token", "audit-internal")
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(body.to_string().contains("192.0.2.2"), "{body}");
    h.abort();
    w.abort();
}
#[tokio::test]
async fn decoded_vrf_ingest_ignores_top_level_scope_label() {
    let dir = tempfile::tempdir().unwrap();
    let (url, backend, c, h, w) = setup_routing(&dir).await;
    let frames = build_rd_bmp_frames(
        Ipv4Addr::new(10, 100, 1, 1),
        65002,
        65000,
        100,
        [10, 99, 1],
        100,
    );
    for frame in [&frames.0, &frames.1, &frames.2, &frames.3] {
        let r=c.post(format!("{url}/api/routing/v1/ingest-bmp")).bearer_auth("bsk_audit_key_12345678").json(&json!({"speaker_addr":"192.0.2.1","vrf_rd":"0:0","frame_hex":hex::encode(frame),"session_epoch":1})).send().await.unwrap();
        assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    }
    let body = c
        .get(format!(
            "{backend}/internal/routing/v1/routes?vrf_rd=65000:100"
        ))
        .header("x-bonsai-routing-token", "audit-internal")
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(body.to_string().contains("10.99.1.0/24"), "{body}");
    h.abort();
    w.abort();
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
