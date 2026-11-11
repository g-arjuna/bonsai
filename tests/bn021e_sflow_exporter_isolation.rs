//! BN-021e — sFlow exporter-identity isolation.
//!
//! Plain UDP sFlow is *evidence*, never canonical identity:
//! - the observed transport endpoint (IP + source port) separates
//!   exporters behind one NAT address;
//! - the self-reported agent address and sub-agent id are preserved as
//!   claims but can never merge observations or mint `device_id`;
//! - the graph writer fails closed: AppFlow + source-scoped events only —
//!   no `DeviceAddress` refresh, `CARRIES_FLOW`, `SRC_HOST`/`DST_HOST`,
//!   `Interface` counter mutation, or flow-derived device alerts.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bonsai::config::SflowConfig;
use bonsai::event_bus::InProcessBus;
use bonsai::streaming::sflow::run_sflow_receiver;
use bonsai::telemetry::TelemetryUpdate;
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

// ── sFlow v5 datagram builders ───────────────────────────────────────────────

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn push32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&be32(v));
}

/// One sampled-IPv4-header flow record (format 3, 32-byte payload).
fn ipv4_flow_record(
    src: [u8; 4],
    dst: [u8; 4],
    sport: u16,
    dport: u16,
    proto: u8,
    frame_len: u32,
) -> Vec<u8> {
    let mut rec = Vec::with_capacity(40);
    push32(&mut rec, 3); // enterprise_format: enterprise 0, format 3
    push32(&mut rec, 32); // record_len
    push32(&mut rec, frame_len);
    push32(&mut rec, proto as u32);
    rec.extend_from_slice(&src);
    rec.extend_from_slice(&dst);
    push32(&mut rec, sport as u32);
    push32(&mut rec, dport as u32);
    push32(&mut rec, 0x02); // tcp_flags word (SYN)
    push32(&mut rec, 0); // tos
    rec
}

/// One flow sample (format 1) carrying `records`.
fn flow_sample(records: &[Vec<u8>], sampling_rate: u32) -> Vec<u8> {
    let mut data = Vec::new();
    push32(&mut data, 1); // sequence_number
    push32(&mut data, 0); // source_id
    push32(&mut data, sampling_rate);
    push32(&mut data, 0); // sample_pool
    push32(&mut data, 0); // drops
    push32(&mut data, 1); // input_if
    push32(&mut data, 2); // output_if
    push32(&mut data, records.len() as u32); // num_records
    for r in records {
        data.extend_from_slice(r);
    }
    let mut sample = Vec::new();
    push32(&mut sample, 1); // enterprise_format: enterprise 0, format 1 (flow)
    push32(&mut sample, data.len() as u32);
    sample.extend_from_slice(&data);
    sample
}

/// One generic-interface counter sample (format 2) with a single record.
fn counter_sample(if_index: u32, in_octets: u64, out_octets: u64) -> Vec<u8> {
    let mut rec = vec![0u8; 88];
    rec[0..4].copy_from_slice(&be32(if_index));
    rec[8..16].copy_from_slice(&in_octets.to_be_bytes());
    rec[24..32].copy_from_slice(&in_octets.to_be_bytes());
    rec[32..40].copy_from_slice(&out_octets.to_be_bytes());
    rec[56..64].copy_from_slice(&out_octets.to_be_bytes());

    let mut data = Vec::new();
    push32(&mut data, 1); // sequence_number
    push32(&mut data, 0); // source_id
    push32(&mut data, 1); // num_records
    push32(&mut data, 1); // enterprise_format: enterprise 0, format 1 (counter data)
    push32(&mut data, 88);
    data.extend_from_slice(&rec);

    let mut sample = Vec::new();
    push32(&mut sample, 2); // enterprise_format: enterprise 0, format 2 (counter sample)
    push32(&mut sample, data.len() as u32);
    sample.extend_from_slice(&data);
    sample
}

/// Full sFlow v5 datagram.
fn sflow_datagram(
    agent: [u8; 4],
    sub_agent: u32,
    seq: u32,
    uptime: u32,
    samples: &[Vec<u8>],
) -> Vec<u8> {
    let mut d = Vec::new();
    push32(&mut d, 5); // version
    push32(&mut d, 1); // agent address type = IPv4
    d.extend_from_slice(&agent);
    push32(&mut d, sub_agent);
    push32(&mut d, seq);
    push32(&mut d, uptime);
    push32(&mut d, samples.len() as u32);
    for s in samples {
        d.extend_from_slice(s);
    }
    d
}

fn flow_datagram(agent: [u8; 4], sub_agent: u32, seq: u32, records: &[Vec<u8>]) -> Vec<u8> {
    sflow_datagram(agent, sub_agent, seq, 1000, &[flow_sample(records, 100)])
}

// ── Lab fixture ─────────────────────────────────────────────────────────────

struct Lab {
    rx: broadcast::Receiver<Arc<TelemetryUpdate>>,
    addr: SocketAddr,
    shutdown_tx: watch::Sender<bool>,
    task: JoinHandle<anyhow::Result<()>>,
}

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn start_lab(cfg: SflowConfig) -> Lab {
    let port = free_udp_port();
    let cfg = SflowConfig {
        udp_addr: format!("127.0.0.1:{port}"),
        ..cfg
    };
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let bus = InProcessBus::new(1024);
    let rx = bus.subscribe();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let bus2 = Arc::clone(&bus);
    let task = tokio::spawn(run_sflow_receiver(cfg, bus2, shutdown_rx));
    tokio::time::sleep(Duration::from_millis(50)).await;
    Lab {
        rx,
        addr,
        shutdown_tx,
        task,
    }
}

impl Lab {
    async fn stop(mut self) {
        let _ = self.shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(2), &mut self.task).await;
    }
}

async fn exporter_socket() -> (UdpSocket, SocketAddr) {
    let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let local = s.local_addr().unwrap();
    (s, local)
}

async fn send(sock: &UdpSocket, lab: &Lab, pkt: &[u8]) {
    sock.send_to(pkt, lab.addr).await.unwrap();
}

async fn next_update(
    rx: &mut broadcast::Receiver<Arc<TelemetryUpdate>>,
    path: &str,
) -> TelemetryUpdate {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(remaining > Duration::ZERO, "timed out waiting for {path}");
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(u)) if u.path == path => return u.as_ref().clone(),
            Ok(Ok(_)) => continue,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(e)) => panic!("bus recv error: {e}"),
            Err(_) => panic!("timed out waiting for {path}"),
        }
    }
}

/// Counter samples are source-scoped too: the published update carries the
/// provisional `sflowsrc/...` key and identity evidence, never a canonical
/// device claim.
#[tokio::test]
async fn counter_samples_stay_source_scoped() {
    let lab = start_lab(test_cfg()).await;
    let mut rx = lab.rx.resubscribe();
    let (sock, _addr) = exporter_socket().await;

    send(
        &sock,
        &lab,
        &sflow_datagram([192, 0, 2, 10], 0, 1, 1000, &[counter_sample(2, 12345, 6789)]),
    )
    .await;

    let u = next_update(&mut rx, "streaming/sflow/counters").await;
    let key = u.value["exporter_id"].as_str().unwrap();
    assert!(key.starts_with("sflowsrc/bn021e-lab/"), "key={key}");
    assert_eq!(u.target, key);
    assert_eq!(u.value["exporter_address"], "127.0.0.1");
    assert_eq!(u.value["agent_address"], "192.0.2.10");
    assert_eq!(u.value["identity_status"], "provisional");
    assert!(u.value["device_id"].is_null());
    assert_eq!(u.value["if_index"], 2);
    assert_eq!(u.value["in_octets"], 12345);

    lab.stop().await;
}

fn test_cfg() -> SflowConfig {
    SflowConfig {
        enabled: true,
        collector_id: "bn021e-lab".to_string(),
        ..Default::default()
    }
}

// ── Receiver-level identity tests ───────────────────────────────────────────

/// Two agents behind one NAT address (distinct UDP source ports) must get
/// distinct provisional source keys; the self-reported agent address is
/// preserved as a claim, and `exporter_address` is the observed transport
/// IP — never the agent claim.
#[tokio::test]
async fn nat_agents_stay_separate_and_claims_are_evidence() {
    let lab = start_lab(test_cfg()).await;
    let mut rx = lab.rx.resubscribe();

    let (sock_a, _addr_a) = exporter_socket().await;
    let (sock_b, _addr_b) = exporter_socket().await;
    let rec_a = ipv4_flow_record([192, 0, 2, 1], [198, 51, 100, 1], 40000, 443, 6, 1500);
    let rec_b = ipv4_flow_record([192, 0, 2, 2], [198, 51, 100, 2], 40001, 443, 6, 1500);

    // Agent A claims 192.0.2.10; agent B claims a DIFFERENT address.
    send(&sock_a, &lab, &flow_datagram([192, 0, 2, 10], 0, 1, &[rec_a])).await;
    send(&sock_b, &lab, &flow_datagram([192, 0, 2, 20], 0, 1, &[rec_b])).await;

    let ua = next_update(&mut rx, "streaming/sflow/flow").await;
    let ub = next_update(&mut rx, "streaming/sflow/flow").await;

    let key_a = ua.value["exporter_id"].as_str().unwrap().to_string();
    let key_b = ub.value["exporter_id"].as_str().unwrap().to_string();
    assert_ne!(key_a, key_b, "NAT-shared agents must not merge");
    assert!(key_a.starts_with("sflowsrc/bn021e-lab/"), "key={key_a}");
    assert!(key_b.starts_with("sflowsrc/bn021e-lab/"), "key={key_b}");
    assert_eq!(ua.target, key_a);
    assert_eq!(ub.target, key_b);

    // exporter_address is the OBSERVED transport IP (127.0.0.1), not the
    // self-reported agent claim.
    assert_eq!(ua.value["exporter_address"], "127.0.0.1");
    assert_eq!(ub.value["exporter_address"], "127.0.0.1");
    assert_eq!(ua.value["agent_address"], "192.0.2.10");
    assert_eq!(ub.value["agent_address"], "192.0.2.20");

    // Both stay provisional with no canonical identity.
    for u in [&ua, &ub] {
        assert_eq!(u.value["identity_status"], "provisional");
        assert!(u.value["device_id"].is_null());
        assert!(u.value["canonical_exporter_id"].is_null());
        assert!(u.value["src_port"].is_u64());
    }

    lab.stop().await;
}

/// The SAME self-reported agent address arriving from different transport
/// endpoints (or different sub-agent ids) must still produce distinct
/// provisional keys — the claim cannot merge observations.
#[tokio::test]
async fn agent_claim_cannot_merge_distinct_sources() {
    let lab = start_lab(test_cfg()).await;
    let mut rx = lab.rx.resubscribe();
    let (sock, _addr) = exporter_socket().await;
    let rec = ipv4_flow_record([10, 0, 0, 1], [10, 0, 0, 2], 5000, 80, 6, 1000);

    // Same agent claim, different sub-agent ids.
    send(&sock, &lab, &flow_datagram([192, 0, 2, 10], 0, 1, &[rec.clone()])).await;
    send(&sock, &lab, &flow_datagram([192, 0, 2, 10], 9, 2, &[rec])).await;

    let u0 = next_update(&mut rx, "streaming/sflow/flow").await;
    let u9 = next_update(&mut rx, "streaming/sflow/flow").await;
    let k0 = u0.value["exporter_id"].as_str().unwrap();
    let k9 = u9.value["exporter_id"].as_str().unwrap();
    assert_ne!(k0, k9, "sub-agent id must separate the source key");
    assert!(k0.contains("/sub0"), "key={k0}");
    assert!(k9.contains("/sub9"), "key={k9}");
    assert_eq!(u0.value["agent_sub_id"], 0);
    assert_eq!(u9.value["agent_sub_id"], 9);

    lab.stop().await;
}

/// Collector namespace is part of the provisional key: identical datagrams
/// received by differently-scoped collectors stay separated.
#[tokio::test]
async fn collector_scope_separates_source_keys() {
    let lab_a = start_lab(test_cfg()).await;
    let cfg_b = SflowConfig {
        collector_id: "other-collector".to_string(),
        ..test_cfg()
    };
    let lab_b = start_lab(cfg_b).await;
    let mut rx_a = lab_a.rx.resubscribe();
    let mut rx_b = lab_b.rx.resubscribe();
    let (sock_a, _a) = exporter_socket().await;
    let (sock_b, _b) = exporter_socket().await;
    let rec = ipv4_flow_record([10, 1, 1, 1], [10, 1, 1, 2], 6000, 443, 6, 1000);
    let pkt = flow_datagram([192, 0, 2, 10], 0, 1, &[rec]);

    send(&sock_a, &lab_a, &pkt).await;
    send(&sock_b, &lab_b, &pkt).await;

    let ua = next_update(&mut rx_a, "streaming/sflow/flow").await;
    let ub = next_update(&mut rx_b, "streaming/sflow/flow").await;
    let ka = ua.value["exporter_id"].as_str().unwrap();
    let kb = ub.value["exporter_id"].as_str().unwrap();
    assert!(ka.contains("bn021e-lab"), "key={ka}");
    assert!(kb.contains("other-collector"), "key={kb}");
    assert_ne!(ka, kb);

    lab_a.stop().await;
    lab_b.stop().await;
}

// ── Graph-level fail-closed tests ───────────────────────────────────────────

fn flow_update(
    exporter_key: &str,
    exporter_ip: &str,
    agent: &str,
    sub_agent: i64,
    src: &str,
    src_port: i64,
    dst: &str,
    dst_port: i64,
    extra: serde_json::Value,
) -> TelemetryUpdate {
    let mut value = serde_json::json!({
        "exporter_address": exporter_ip,
        "exporter_id": exporter_key,
        "exporter_endpoint": "127.0.0.1:49152",
        "transport_source": "127.0.0.1:49152",
        "collector_id": "bn021e-lab",
        "agent_address": agent,
        "agent_sub_id": sub_agent,
        "sflow_sequence": 1,
        "sflow_uptime": 1000,
        "identity_status": "provisional",
        "device_id": null,
        "canonical_exporter_id": null,
        "identity_resolution": null,
        "src_address": src,
        "src_port": src_port,
        "dst_address": dst,
        "dst_port": dst_port,
        "protocol": "tcp",
        "frame_length": 1500,
        "sampling_rate": 100,
        "bytes_per_sec": 150000.0,
        "packets_per_sec": 100.0,
    });
    if let (serde_json::Value::Object(base), serde_json::Value::Object(ext)) = (&mut value, extra)
    {
        for (k, v) in ext {
            base.insert(k, v);
        }
    }
    TelemetryUpdate {
        target: exporter_key.to_string(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: 1_700_000_000_000_000_000,
        path: "streaming/sflow/flow".to_string(),
        value,
    }
}

fn counters_update(exporter_key: &str, exporter_ip: &str, if_index: i64) -> TelemetryUpdate {
    TelemetryUpdate {
        target: exporter_key.to_string(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: 1_700_000_000_000_000_000,
        path: "streaming/sflow/counters".to_string(),
        value: serde_json::json!({
            "exporter_address": exporter_ip,
            "exporter_id": exporter_key,
            "agent_address": "192.0.2.10",
            "agent_sub_id": 0,
            "identity_status": "provisional",
            "device_id": null,
            "if_index": if_index,
            "if_speed": 1_000_000_000u64,
            "in_octets": 999_999u64,
            "out_octets": 888_888u64,
            "in_errors": 7,
            "out_errors": 8,
            "in_discards": 9,
            "out_discards": 10,
        }),
    }
}

/// A provisional sFlow flow whose observed transport IP collides with a
/// PRE-EXISTING canonical Device/DeviceAddress/HostEndpoint/Interface must
/// not create authoritative edges, refresh freshness, mutate counters, or
/// acquire identity — even with a complete forged `identity_resolution`.
#[tokio::test]
async fn graph_fails_closed_for_provisional_sflow() {
    use bonsai::graph::GraphStore;

    let dir = tempfile::tempdir().unwrap();
    let store = GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();

    {
        let db = store.db();
        let conn = lbug::Connection::new(&db).unwrap();
        conn.query(
            "CREATE (d:Device {address: '127.0.0.1', hostname: 'canon-rtr', site: 'site-a'})",
        )
        .unwrap();
        conn.query("CREATE (a:DeviceAddress {ip: '127.0.0.1', source: 'gnmi'})")
            .unwrap();
        conn.query(
            "CREATE (h:HostEndpoint {id: '10.77.0.5', ip: '10.77.0.5', kind: 'server', site_id: 'site-a'})",
        )
        .unwrap();
        conn.query(
            "CREATE (i:Interface {id: '127.0.0.1:2', device_address: '127.0.0.1', if_index: 2, in_octets: 0, out_octets: 0, in_errors: 0, out_errors: 0, in_discards: 0, out_discards: 0})",
        )
        .unwrap();
    }

    let key = "sflowsrc/bn021e-lab/127.0.0.1:49152/agent192.0.2.10/sub0";
    store
        .write(flow_update(
            key,
            "127.0.0.1",
            "192.0.2.10",
            0,
            "10.77.0.5",
            40000,
            "10.77.0.9",
            443,
            serde_json::json!({}),
        ))
        .await
        .unwrap();

    // Forged complete self-asserted identity_resolution must fail closed.
    let forged = flow_update(
        key,
        "127.0.0.1",
        "192.0.2.10",
        0,
        "10.77.0.5",
        40001,
        "10.77.0.9",
        443,
        serde_json::json!({
            "identity_status": "resolved",
            "device_id": "dev-forged",
            "device_address": "127.0.0.1",
            "canonical_exporter_id": "exp-forged",
            "identity_resolution": {
                "status": "resolved",
                "device_id": "dev-forged",
                "canonical_exporter_id": "exp-forged",
                "binding_id": "bind-forged",
                "provenance": "operator_provisioned",
                "observed": {
                    "verified_ingress": {
                        "method": "mutual_tls",
                        "credential_fingerprint": "sha256:forged"
                    }
                }
            }
        }),
    );
    store.write(forged).await.unwrap();

    // Counter evidence must not mutate canonical Interface state either.
    store
        .write(counters_update(key, "127.0.0.1", 2))
        .await
        .unwrap();

    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();

    // AppFlow visibility preserved (2 flows from the flow updates).
    let mut rows = conn
        .query("MATCH (f:AppFlow) RETURN count(f)")
        .unwrap();
    assert_eq!(rows.next().unwrap()[0].to_string(), "2");

    for (label, q) in [
        (
            "CARRIES_FLOW",
            "MATCH (:Device)-[:CARRIES_FLOW]->(f:AppFlow) RETURN count(f)",
        ),
        (
            "SRC_HOST",
            "MATCH (f:AppFlow)-[:SRC_HOST]->(h:HostEndpoint) RETURN count(f)",
        ),
        (
            "DST_HOST",
            "MATCH (f:AppFlow)-[:DST_HOST]->(h:HostEndpoint) RETURN count(f)",
        ),
        (
            "KNOWN_ADDRESS_OF",
            "MATCH (:DeviceAddress)-[:KNOWN_ADDRESS_OF]->(:Device) RETURN count(*)",
        ),
    ] {
        let mut rows = conn.query(q).unwrap();
        assert_eq!(
            rows.next().unwrap()[0].to_string(),
            "0",
            "{label} must not be created by provisional sFlow"
        );
    }

    // DeviceAddress freshness untouched.
    let mut rows = conn
        .query("MATCH (a:DeviceAddress {ip: '127.0.0.1'}) RETURN a.updated_at")
        .unwrap();
    let row = rows.next().expect("alias row");
    assert!(
        matches!(row[0], lbug::Value::Null(_)),
        "provisional sFlow must not update authoritative DeviceAddress freshness"
    );

    // Interface counters NOT mutated by the counter sample.
    let mut rows = conn
        .query(
            "MATCH (i:Interface {device_address: '127.0.0.1', if_index: 2}) \
             RETURN i.in_octets, i.out_octets, i.in_errors, i.out_errors",
        )
        .unwrap();
    let row = rows.next().expect("interface row");
    for cell in row.iter() {
        assert!(
            matches!(cell, lbug::Value::Int64(0) | lbug::Value::Double(0.0)),
            "Interface counters must stay untouched, got {cell:?}"
        );
    }
}

/// AppFlow id must distinguish flows differing only in source port, and
/// `src_port` must be a queryable property.
#[tokio::test]
async fn appflow_id_includes_source_port() {
    use bonsai::graph::GraphStore;

    let dir = tempfile::tempdir().unwrap();
    let store = GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();

    let key = "sflowsrc/bn021e-lab/127.0.0.1:49152/agent192.0.2.10/sub0";
    for sport in [40000i64, 40001] {
        store
            .write(flow_update(
                key,
                "127.0.0.1",
                "192.0.2.10",
                0,
                "10.0.0.1",
                sport,
                "10.0.0.2",
                443,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
    }

    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    let mut rows = conn
        .query("MATCH (f:AppFlow) RETURN f.id, f.src_port ORDER BY f.src_port")
        .unwrap();
    let mut ids = Vec::new();
    while let Some(row) = rows.next() {
        ids.push((row[0].to_string(), row[1].to_string()));
    }
    assert_eq!(ids.len(), 2, "distinct src_ports must be distinct flows");
    assert!(ids[0].0.contains(":40000:"), "id={}", ids[0].0);
    assert!(ids[1].0.contains(":40001:"), "id={}", ids[1].0);
    assert_eq!(ids[0].1, "40000");
    assert_eq!(ids[1].1, "40001");
}

/// Two sFlow agents reporting the SAME self-claimed agent address from
/// different transport endpoints must produce distinct AppFlow identities —
/// the claim never merges flow records.
#[tokio::test]
async fn same_agent_claim_never_merges_appflows() {
    use bonsai::graph::GraphStore;

    let dir = tempfile::tempdir().unwrap();
    let store = GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();

    // Identical flow tuple, identical agent claim — only the transport
    // endpoint inside exporter_id differs.
    for key in [
        "sflowsrc/bn021e-lab/127.0.0.1:49152/agent192.0.2.10/sub0",
        "sflowsrc/bn021e-lab/127.0.0.1:49153/agent192.0.2.10/sub0",
    ] {
        store
            .write(flow_update(
                key,
                "127.0.0.1",
                "192.0.2.10",
                0,
                "10.0.0.1",
                40000,
                "10.0.0.2",
                443,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
    }

    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    let mut rows = conn
        .query("MATCH (f:AppFlow) RETURN count(f)")
        .unwrap();
    assert_eq!(
        rows.next().unwrap()[0].to_string(),
        "2",
        "same agent claim from distinct transport endpoints must not merge"
    );
}
