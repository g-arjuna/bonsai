//! BN-021d: NetFlow v9 / IPFIX exporter identity isolation and template
//! lifecycle.
//!
//! End-to-end coverage over real UDP sockets:
//! - NAT-shared exporters (same IP, different source ports) stay
//!   independently attributable as *provisional* observed sources.
//! - Template caches are isolated per (collector, transport endpoint,
//!   protocol, observation domain, template id).
//! - IPFIX enterprise IEs, UDP withdrawal semantics (RFC 7011 §8.4),
//!   expiry, restart detection, bounded eviction, malformed-packet
//!   safety, and downstream bus/graph separation.
//!
//! Trust boundary under test: plain UDP observations must NEVER acquire
//! canonical `device_id` — `identity_status` stays "provisional" even
//! when operator bindings would match on transport evidence.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bonsai::config::NetflowConfig;
use bonsai::event_bus::InProcessBus;
use bonsai::streaming::netflow::{resolve_flow_identity, run_netflow_receiver};
use bonsai::telemetry::TelemetryUpdate;
use bonsai::telemetry_identity::{
    BindingProvenance, BindingStatus, IdentityStatus, TelemetrySourceIdentity, TrustedBinding,
};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

// ── Packet builders ─────────────────────────────────────────────────────────

/// Standard test template: src/dst addr+port, protocol, bytes, packets,
/// first/last switched. Record length = 4+4+2+2+1+4+4+4+4 = 29 bytes.
const STD_FIELDS: &[(u16, u16)] = &[
    (8, 4),  // SOURCE_IPV4_ADDRESS
    (12, 4), // DESTINATION_IPV4_ADDRESS
    (7, 2),  // SOURCE_TRANSPORT_PORT
    (11, 2), // DESTINATION_TRANSPORT_PORT
    (4, 1),  // PROTOCOL_IDENTIFIER
    (1, 4),  // OCTET_DELTA_COUNT
    (2, 4),  // PACKET_DELTA_COUNT
    (22, 4), // FLOW_START_MILLISECOND (v9 uptime-relative)
    (21, 4), // FLOW_END_MILLISECOND
];

fn encode_std_record(
    src: [u8; 4],
    dst: [u8; 4],
    sport: u16,
    dport: u16,
    proto: u8,
    bytes: u64,
    pkts: u64,
    first_ms: u32,
    last_ms: u32,
) -> Vec<u8> {
    let mut r = Vec::with_capacity(29);
    r.extend_from_slice(&src);
    r.extend_from_slice(&dst);
    r.extend_from_slice(&sport.to_be_bytes());
    r.extend_from_slice(&dport.to_be_bytes());
    r.push(proto);
    r.extend_from_slice(&(bytes as u32).to_be_bytes());
    r.extend_from_slice(&(pkts as u32).to_be_bytes());
    r.extend_from_slice(&first_ms.to_be_bytes());
    r.extend_from_slice(&last_ms.to_be_bytes());
    r
}

fn v9_template_record(template_id: u16, fields: &[(u16, u16)]) -> Vec<u8> {
    let mut r = Vec::new();
    r.extend_from_slice(&template_id.to_be_bytes());
    r.extend_from_slice(&(fields.len() as u16).to_be_bytes());
    for (t, l) in fields {
        r.extend_from_slice(&t.to_be_bytes());
        r.extend_from_slice(&l.to_be_bytes());
    }
    r
}

fn v9_template_flowset(records: &[Vec<u8>]) -> Vec<u8> {
    let mut d = Vec::new();
    for rec in records {
        d.extend_from_slice(rec);
    }
    d
}

fn v9_packet(
    source_id: u32,
    uptime_ms: u32,
    unix_secs: u32,
    seq: u32,
    sets: &[(u16, Vec<u8>)],
) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&9u16.to_be_bytes());
    p.extend_from_slice(&(sets.len() as u16).to_be_bytes());
    p.extend_from_slice(&uptime_ms.to_be_bytes());
    p.extend_from_slice(&unix_secs.to_be_bytes());
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(&source_id.to_be_bytes());
    for (id, data) in sets {
        let len = (data.len() + 4) as u16;
        p.extend_from_slice(&id.to_be_bytes());
        p.extend_from_slice(&len.to_be_bytes());
        p.extend_from_slice(data);
    }
    p
}

/// Build an IPFIX field specifier. `pen = Some(n)` sets the enterprise
/// bit and appends the 4-byte enterprise number.
fn ipfix_field(ie: u16, len: u16, pen: Option<u32>) -> Vec<u8> {
    let mut f = Vec::new();
    let raw = if pen.is_some() { ie | 0x8000 } else { ie };
    f.extend_from_slice(&raw.to_be_bytes());
    f.extend_from_slice(&len.to_be_bytes());
    if let Some(p) = pen {
        f.extend_from_slice(&p.to_be_bytes());
    }
    f
}

fn ipfix_template_set(records: &[Vec<u8>]) -> Vec<u8> {
    let mut d = Vec::new();
    for rec in records {
        d.extend_from_slice(rec);
    }
    d
}

fn ipfix_template_record(template_id: u16, fields: &[Vec<u8>]) -> Vec<u8> {
    let mut r = Vec::new();
    r.extend_from_slice(&template_id.to_be_bytes());
    r.extend_from_slice(&(fields.len() as u16).to_be_bytes());
    for f in fields {
        r.extend_from_slice(f);
    }
    r
}

fn ipfix_packet(domain_id: u32, export_time: u32, seq: u32, sets: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut p = Vec::new();
    let body_len: usize = sets.iter().map(|(_, d)| d.len() + 4).sum();
    let msg_len = 16 + body_len;
    p.extend_from_slice(&10u16.to_be_bytes());
    p.extend_from_slice(&(msg_len as u16).to_be_bytes());
    p.extend_from_slice(&export_time.to_be_bytes());
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(&domain_id.to_be_bytes());
    for (id, data) in sets {
        let len = (data.len() + 4) as u16;
        p.extend_from_slice(&id.to_be_bytes());
        p.extend_from_slice(&len.to_be_bytes());
        p.extend_from_slice(data);
    }
    p
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

async fn start_lab(cfg: NetflowConfig) -> Lab {
    let port = free_udp_port();
    let cfg = NetflowConfig {
        udp_addr: format!("127.0.0.1:{port}"),
        ..cfg
    };
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let bus = InProcessBus::new(1024);
    let rx = bus.subscribe();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let bus2 = Arc::clone(&bus);
    let task = tokio::spawn(run_netflow_receiver(cfg, bus2, shutdown_rx));
    // Give the receiver a moment to bind.
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

/// Bind a UDP socket that acts as one NAT'd exporter (fixed source port).
async fn exporter_socket() -> (UdpSocket, SocketAddr) {
    let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let local = s.local_addr().unwrap();
    (s, local)
}

async fn send(sock: &UdpSocket, lab: &Lab, pkt: &[u8]) {
    sock.send_to(pkt, lab.addr).await.unwrap();
}

/// Receive the next flow update, asserting path + returning it.
async fn next_flow(rx: &mut broadcast::Receiver<Arc<TelemetryUpdate>>) -> TelemetryUpdate {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            remaining > Duration::ZERO,
            "timed out waiting for flow update"
        );
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(u)) if u.path == "streaming/netflow/flow" => return u.as_ref().clone(),
            Ok(Ok(_)) => continue,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(e)) => panic!("bus recv error: {e}"),
            Err(_) => panic!("timed out waiting for flow update"),
        }
    }
}

/// Assert no flow update arrives within `dur`.
async fn expect_no_flow(rx: &mut broadcast::Receiver<Arc<TelemetryUpdate>>, dur: Duration) {
    let deadline = tokio::time::Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(u)) if u.path == "streaming/netflow/flow" => {
                panic!("unexpected flow update: target={}", u.target)
            }
            Ok(Ok(_)) => continue,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) => return,
            Err(_) => return,
        }
    }
}

fn test_cfg() -> NetflowConfig {
    NetflowConfig {
        enabled: true,
        collector_id: "bn021d-lab".to_string(),
        ..Default::default()
    }
}

fn std_data_v9(records: &[Vec<u8>]) -> Vec<u8> {
    let mut d = Vec::new();
    for r in records {
        d.extend_from_slice(r);
    }
    d
}

fn std_template_set_ipfix(tid: u16) -> Vec<u8> {
    let fields: Vec<Vec<u8>> = STD_FIELDS
        .iter()
        .map(|(ie, l)| ipfix_field(*ie, *l, None))
        .collect();
    ipfix_template_set(&[ipfix_template_record(tid, &fields)])
}

fn ipfix_std_record(
    src: [u8; 4],
    dst: [u8; 4],
    sport: u16,
    dport: u16,
    proto: u8,
    bytes: u64,
    pkts: u64,
) -> Vec<u8> {
    // IPFIX timestamps are absolute; our decoder only uses unix_secs for
    // IPFIX so first/last switched are not part of STD_FIELDS decoding —
    // but the template declares them, so the record must carry them.
    let mut r = Vec::with_capacity(29);
    r.extend_from_slice(&src);
    r.extend_from_slice(&dst);
    r.extend_from_slice(&sport.to_be_bytes());
    r.extend_from_slice(&dport.to_be_bytes());
    r.push(proto);
    r.extend_from_slice(&(bytes as u32).to_be_bytes());
    r.extend_from_slice(&(pkts as u32).to_be_bytes());
    r.extend_from_slice(&0u32.to_be_bytes());
    r.extend_from_slice(&0u32.to_be_bytes());
    r
}

// ── Tests ───────────────────────────────────────────────────────────────────

/// Scenario A+B: two exporters behind one NAT address (same IP 127.0.0.1,
/// different source ports) use the SAME source-id and SAME template id
/// but DIFFERENT template definitions. Each must decode only its own
/// records and publish under a distinct provisional source key.
#[tokio::test]
async fn nat_shared_exporters_stay_separate() {
    let lab = start_lab(test_cfg()).await;
    let mut rx = lab.rx.resubscribe();

    let (sock_a, addr_a) = exporter_socket().await;
    let (sock_b, addr_b) = exporter_socket().await;
    assert_eq!(addr_a.ip(), addr_b.ip(), "test premise: same NAT IP");
    assert_ne!(addr_a.port(), addr_b.port());

    // Exporter A: standard template tid=300, sid=42.
    let tmpl_a = v9_template_record(300, STD_FIELDS);
    send(
        &sock_a,
        &lab,
        &v9_packet(
            42,
            1_000_000,
            1_700_000_000,
            1,
            &[(0, v9_template_flowset(&[tmpl_a]))],
        ),
    )
    .await;

    // Exporter B: SAME sid=42, SAME tid=300, but a different field order —
    // dst addr first, then src addr, proto only (record_len = 9).
    let fields_b: &[(u16, u16)] = &[(12, 4), (8, 4), (4, 1)];
    let tmpl_b = v9_template_record(300, fields_b);
    send(
        &sock_b,
        &lab,
        &v9_packet(
            42,
            1_000_000,
            1_700_000_000,
            1,
            &[(0, v9_template_flowset(&[tmpl_b]))],
        ),
    )
    .await;

    // A's data record (standard layout).
    let rec_a = encode_std_record(
        [10, 0, 0, 1],
        [10, 0, 0, 2],
        12345,
        179,
        6,
        1500,
        10,
        500,
        900,
    );
    send(
        &sock_a,
        &lab,
        &v9_packet(
            42,
            1_000_000,
            1_700_000_000,
            2,
            &[(300, std_data_v9(&[rec_a]))],
        ),
    )
    .await;

    // B's data record (its own layout: dst, src, proto).
    let mut rec_b = Vec::new();
    rec_b.extend_from_slice(&[192, 0, 2, 1]); // dst first per B's template
    rec_b.extend_from_slice(&[198, 51, 100, 7]); // src
    rec_b.push(17);
    send(
        &sock_b,
        &lab,
        &v9_packet(42, 1_000_000, 1_700_000_000, 2, &[(300, rec_b)]),
    )
    .await;

    let flow_a = next_flow(&mut rx).await;
    let flow_b = next_flow(&mut rx).await;

    // Distinct provisional source keys — same IP, different ports.
    assert_ne!(
        flow_a.target, flow_b.target,
        "NAT exporters must not collapse"
    );
    assert!(flow_a.target.contains("nf9"));
    assert!(flow_b.target.contains("nf9"));
    assert!(flow_a.target.contains("sid42"), "target={}", flow_a.target);
    assert_ne!(
        flow_a.value["exporter_id"].as_str().unwrap(),
        flow_b.value["exporter_id"].as_str().unwrap()
    );

    // Each decoded with its OWN template — no cross-contamination.
    let (fa, fb) = if flow_a.value["src_port"].as_u64() == Some(12345) {
        (&flow_a, &flow_b)
    } else {
        (&flow_b, &flow_a)
    };
    assert_eq!(fa.value["src_address"].as_str().unwrap(), "10.0.0.1");
    assert_eq!(fa.value["dst_address"].as_str().unwrap(), "10.0.0.2");
    assert_eq!(fa.value["src_port"].as_u64().unwrap(), 12345);
    assert_eq!(fa.value["protocol"].as_str().unwrap(), "tcp");
    // B's record: dst=192.0.2.1, src=198.51.100.7, proto=17 (udp).
    assert_eq!(fb.value["src_address"].as_str().unwrap(), "198.51.100.7");
    assert_eq!(fb.value["dst_address"].as_str().unwrap(), "192.0.2.1");
    assert_eq!(fb.value["protocol"].as_str().unwrap(), "udp");

    // Identity: provisional, never canonical.
    for f in [fa, fb] {
        assert_eq!(f.value["identity_status"].as_str().unwrap(), "provisional");
        assert!(f.value["device_id"].is_null());
        assert_eq!(f.value["exporter_address"].as_str().unwrap(), "127.0.0.1");
        assert_eq!(f.value["collector_id"].as_str().unwrap(), "bn021d-lab");
        assert_eq!(f.value["observation_domain_id"].as_u64().unwrap(), 42);
        assert_eq!(f.value["exporter_id"].as_str().unwrap(), f.target);
    }

    lab.stop().await;
}

/// Scenario C: NetFlow v9 and IPFIX reuse the same numeric domain and
/// template ids — protocol namespaces must be fully isolated.
#[tokio::test]
async fn v9_and_ipfix_namespaces_are_isolated() {
    let lab = start_lab(test_cfg()).await;
    let mut rx = lab.rx.resubscribe();
    let (sock, _addr) = exporter_socket().await;

    // v9: sid=7, template 300 with standard fields.
    send(
        &sock,
        &lab,
        &v9_packet(
            7,
            500_000,
            1_700_000_000,
            1,
            &[(
                0,
                v9_template_flowset(&[v9_template_record(300, STD_FIELDS)]),
            )],
        ),
    )
    .await;
    // IPFIX: odid=7, template 300 with DIFFERENT fields (dst,src,proto).
    let fields_b: Vec<Vec<u8>> = [(12u16, 4u16), (8, 4), (4, 1)]
        .iter()
        .map(|(ie, l)| ipfix_field(*ie, *l, None))
        .collect();
    send(
        &sock,
        &lab,
        &ipfix_packet(
            7,
            1_700_000_000,
            1,
            &[(
                2,
                ipfix_template_set(&[ipfix_template_record(300, &fields_b)]),
            )],
        ),
    )
    .await;

    // v9 data.
    let rec_v9 = encode_std_record([10, 1, 0, 1], [10, 1, 0, 2], 1111, 80, 6, 100, 2, 0, 0);
    send(
        &sock,
        &lab,
        &v9_packet(
            7,
            500_000,
            1_700_000_000,
            2,
            &[(300, std_data_v9(&[rec_v9]))],
        ),
    )
    .await;
    // IPFIX data (its own layout).
    let mut rec_fx = Vec::new();
    rec_fx.extend_from_slice(&[203, 0, 113, 9]); // dst
    rec_fx.extend_from_slice(&[192, 0, 2, 55]); // src
    rec_fx.push(17);
    send(
        &sock,
        &lab,
        &ipfix_packet(7, 1_700_000_000, 2, &[(300, rec_fx)]),
    )
    .await;

    let f1 = next_flow(&mut rx).await;
    let f2 = next_flow(&mut rx).await;
    assert_ne!(f1.target, f2.target);
    assert!(f1.target.contains("nf9") || f2.target.contains("nf9"));
    assert!(f1.target.contains("ipfix") || f2.target.contains("ipfix"));

    let (v9f, fxf) = if f1.target.contains("nf9") {
        (&f1, &f2)
    } else {
        (&f2, &f1)
    };
    assert_eq!(v9f.value["src_address"].as_str().unwrap(), "10.1.0.1");
    assert_eq!(v9f.value["src_port"].as_u64().unwrap(), 1111);
    assert_eq!(fxf.value["src_address"].as_str().unwrap(), "192.0.2.55");
    assert_eq!(fxf.value["dst_address"].as_str().unwrap(), "203.0.113.9");
    assert_eq!(fxf.value["protocol_version"].as_u64().unwrap(), 10);
    assert_eq!(v9f.value["protocol_version"].as_u64().unwrap(), 9);

    lab.stop().await;
}

/// Scenario D: an exporter reconnecting with a different UDP source port
/// gets a fresh transport context — old templates are NOT reused.
#[tokio::test]
async fn source_port_change_does_not_inherit_templates() {
    let lab = start_lab(test_cfg()).await;
    let mut rx = lab.rx.resubscribe();

    let (sock_a, _a) = exporter_socket().await;
    let (sock_b, _b) = exporter_socket().await;

    // Port A: template + data → flow.
    send(
        &sock_a,
        &lab,
        &v9_packet(
            9,
            100_000,
            1_700_000_000,
            1,
            &[(
                0,
                v9_template_flowset(&[v9_template_record(400, STD_FIELDS)]),
            )],
        ),
    )
    .await;
    let rec = encode_std_record([10, 9, 0, 1], [10, 9, 0, 2], 5000, 443, 6, 10, 1, 0, 0);
    send(
        &sock_a,
        &lab,
        &v9_packet(
            9,
            100_000,
            1_700_000_000,
            2,
            &[(400, std_data_v9(&[rec.clone()]))],
        ),
    )
    .await;
    let _ = next_flow(&mut rx).await;

    // Port B (same IP): data referencing the same template id but NO
    // template — must be dropped as unknown-template, not decoded with
    // port A's template.
    send(
        &sock_b,
        &lab,
        &v9_packet(
            9,
            100_000,
            1_700_000_000,
            1,
            &[(400, std_data_v9(&[rec.clone()]))],
        ),
    )
    .await;
    expect_no_flow(&mut rx, Duration::from_millis(400)).await;

    // Port B then receives its own template → decodes correctly.
    send(
        &sock_b,
        &lab,
        &v9_packet(
            9,
            100_000,
            1_700_000_000,
            2,
            &[(
                0,
                v9_template_flowset(&[v9_template_record(400, STD_FIELDS)]),
            )],
        ),
    )
    .await;
    send(
        &sock_b,
        &lab,
        &v9_packet(
            9,
            100_000,
            1_700_000_000,
            3,
            &[(400, std_data_v9(&[rec.clone()]))],
        ),
    )
    .await;
    let f = next_flow(&mut rx).await;
    assert!(
        f.value["exporter_endpoint"]
            .as_str()
            .unwrap()
            .ends_with(&_b.port().to_string())
    );

    lab.stop().await;
}

/// Scenario E (v9): exporter restart detected via sys_uptime regression
/// clears cached templates — stale definitions cannot decode new data.
#[tokio::test]
async fn v9_exporter_restart_clears_templates() {
    let lab = start_lab(test_cfg()).await;
    let mut rx = lab.rx.resubscribe();
    let (sock, _addr) = exporter_socket().await;

    // uptime = 500_000 ms.
    send(
        &sock,
        &lab,
        &v9_packet(
            5,
            500_000,
            1_700_000_000,
            1,
            &[(
                0,
                v9_template_flowset(&[v9_template_record(500, STD_FIELDS)]),
            )],
        ),
    )
    .await;
    let rec = encode_std_record([10, 5, 0, 1], [10, 5, 0, 2], 6000, 22, 6, 5, 1, 0, 0);
    send(
        &sock,
        &lab,
        &v9_packet(
            5,
            500_000,
            1_700_000_000,
            2,
            &[(500, std_data_v9(&[rec.clone()]))],
        ),
    )
    .await;
    let _ = next_flow(&mut rx).await;

    // "Restart": uptime drops to 1_000 ms. Data with the old template id
    // must now be rejected until a fresh template arrives.
    send(
        &sock,
        &lab,
        &v9_packet(
            5,
            1_000,
            1_700_000_100,
            1,
            &[(500, std_data_v9(&[rec.clone()]))],
        ),
    )
    .await;
    expect_no_flow(&mut rx, Duration::from_millis(400)).await;

    // Re-template after restart → decoding resumes, generation bumped.
    send(
        &sock,
        &lab,
        &v9_packet(
            5,
            1_000,
            1_700_000_100,
            2,
            &[(
                0,
                v9_template_flowset(&[v9_template_record(500, STD_FIELDS)]),
            )],
        ),
    )
    .await;
    send(
        &sock,
        &lab,
        &v9_packet(
            5,
            1_000,
            1_700_000_100,
            3,
            &[(500, std_data_v9(&[rec.clone()]))],
        ),
    )
    .await;
    let f = next_flow(&mut rx).await;
    assert!(f.value["session_epoch"].as_u64().unwrap() >= 1);

    lab.stop().await;
}

/// Scenario E/F (IPFIX): template replacement, UDP withdrawal MUST be
/// ignored (RFC 7011 §8.4), export_time regression restarts, and expiry.
#[tokio::test]
async fn ipfix_template_replacement_withdrawal_and_expiry() {
    let lab = start_lab(NetflowConfig {
        flow_template_ttl_secs: 1,
        ..test_cfg()
    })
    .await;
    let mut rx = lab.rx.resubscribe();
    let (sock, _addr) = exporter_socket().await;

    let tmpl_a = std_template_set_ipfix(600);
    send(
        &sock,
        &lab,
        &ipfix_packet(11, 1_700_000_000, 1, &[(2, tmpl_a)]),
    )
    .await;

    // Withdrawal record (field count = 0) MUST be ignored over UDP.
    let mut withdrawal = Vec::new();
    withdrawal.extend_from_slice(&600u16.to_be_bytes());
    withdrawal.extend_from_slice(&0u16.to_be_bytes());
    send(
        &sock,
        &lab,
        &ipfix_packet(
            11,
            1_700_000_001,
            2,
            &[(2, ipfix_template_set(&[withdrawal]))],
        ),
    )
    .await;

    // Data still decodes — template survived the "withdrawal".
    let rec = ipfix_std_record([10, 6, 0, 1], [10, 6, 0, 2], 7000, 443, 6, 42, 3);
    send(
        &sock,
        &lab,
        &ipfix_packet(11, 1_700_000_001, 3, &[(600, rec.clone())]),
    )
    .await;
    let f = next_flow(&mut rx).await;
    assert_eq!(f.value["src_port"].as_u64().unwrap(), 7000);

    // Replacement: same template id, different definition (dst,src,proto).
    let fields_b: Vec<Vec<u8>> = [(12u16, 4u16), (8, 4), (4, 1)]
        .iter()
        .map(|(ie, l)| ipfix_field(*ie, *l, None))
        .collect();
    send(
        &sock,
        &lab,
        &ipfix_packet(
            11,
            1_700_000_002,
            4,
            &[(
                2,
                ipfix_template_set(&[ipfix_template_record(600, &fields_b)]),
            )],
        ),
    )
    .await;
    let mut rec_b = Vec::new();
    rec_b.extend_from_slice(&[198, 51, 100, 20]); // dst per B layout
    rec_b.extend_from_slice(&[203, 0, 113, 30]); // src
    rec_b.push(17);
    send(
        &sock,
        &lab,
        &ipfix_packet(11, 1_700_000_002, 5, &[(600, rec_b)]),
    )
    .await;
    let f2 = next_flow(&mut rx).await;
    assert_eq!(f2.value["src_address"].as_str().unwrap(), "203.0.113.30");
    assert_eq!(f2.value["protocol"].as_str().unwrap(), "udp");

    // Export-time regression → probable restart → templates cleared.
    // (B's template was just installed; a backwards export_time must
    // invalidate it.)
    send(
        &sock,
        &lab,
        &ipfix_packet(11, 1_600_000_000, 6, &[(600, rec)]),
    )
    .await;
    expect_no_flow(&mut rx, Duration::from_millis(400)).await;

    // Expiry: re-install template, wait past ttl, data must drop.
    send(
        &sock,
        &lab,
        &ipfix_packet(11, 1_700_000_100, 7, &[(2, std_template_set_ipfix(601))]),
    )
    .await;
    let rec601 = ipfix_std_record([10, 6, 0, 9], [10, 6, 0, 10], 8000, 443, 6, 1, 1);
    send(
        &sock,
        &lab,
        &ipfix_packet(11, 1_700_000_100, 8, &[(601, rec601.clone())]),
    )
    .await;
    let _ = next_flow(&mut rx).await; // fresh template works
    tokio::time::sleep(Duration::from_millis(1200)).await;
    send(
        &sock,
        &lab,
        &ipfix_packet(11, 1_700_000_101, 9, &[(601, rec601)]),
    )
    .await;
    expect_no_flow(&mut rx, Duration::from_millis(400)).await;

    lab.stop().await;
}

/// Enterprise-specific IPFIX Information Elements: mixed standard +
/// enterprise fields must parse safely and never be decoded with the
/// wrong standard definition.
#[tokio::test]
async fn ipfix_enterprise_information_elements() {
    let lab = start_lab(test_cfg()).await;
    let mut rx = lab.rx.resubscribe();
    let (sock, _addr) = exporter_socket().await;

    // Template: ENTERPRISE field FIRST with ie=8 (which would alias
    // SOURCE_IPV4_ADDRESS if the PEN were ignored), then standard fields.
    let fields = vec![
        ipfix_field(8, 4, Some(293)), // enterprise IANA-PEN? use 293 (some vendor)
        ipfix_field(8, 4, None),      // real SOURCE_IPV4_ADDRESS
        ipfix_field(12, 4, None),     // DESTINATION_IPV4_ADDRESS
        ipfix_field(4, 1, None),      // PROTOCOL
        ipfix_field(10, 4, Some(0)),  // another enterprise field (PEN 0 edge)
    ];
    let mut rec = Vec::new();
    rec.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // enterprise field bytes
    rec.extend_from_slice(&[10, 7, 0, 1]); // real src
    rec.extend_from_slice(&[10, 7, 0, 2]); // dst
    rec.push(6);
    rec.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]); // second enterprise field
    send(
        &sock,
        &lab,
        &ipfix_packet(
            3,
            1_700_000_000,
            1,
            &[(
                2,
                ipfix_template_set(&[ipfix_template_record(700, &fields)]),
            )],
        ),
    )
    .await;
    send(
        &sock,
        &lab,
        &ipfix_packet(3, 1_700_000_000, 2, &[(700, rec)]),
    )
    .await;

    let f = next_flow(&mut rx).await;
    // If the enterprise field were misdecoded as SOURCE_IPV4_ADDRESS,
    // src_address would be 222.173.190.239 (0xDEADBEEF) — it must not be.
    assert_eq!(f.value["src_address"].as_str().unwrap(), "10.7.0.1");
    assert_eq!(f.value["dst_address"].as_str().unwrap(), "10.7.0.2");
    assert_eq!(f.value["protocol"].as_str().unwrap(), "tcp");

    lab.stop().await;
}

/// Malformed packet battery: nothing may panic the receiver, corrupt
/// template state, or emit bogus flows. The receiver must still decode
/// a valid template afterwards.
#[tokio::test]
async fn malformed_packets_are_safe() {
    let lab = start_lab(test_cfg()).await;
    let mut rx = lab.rx.resubscribe();
    let (sock, _addr) = exporter_socket().await;

    let malformed: Vec<(&str, Vec<u8>)> = vec![
        // Truncated enterprise field specifier (enterprise bit set, PEN missing).
        (
            "truncated_enterprise",
            ipfix_packet(
                1,
                1_700_000_000,
                1,
                &[(2, {
                    let mut d = Vec::new();
                    // template record: tid=800, count=1, field = ie|0x8000,len=4 — NO PEN bytes
                    d.extend_from_slice(&800u16.to_be_bytes());
                    d.extend_from_slice(&1u16.to_be_bytes());
                    d.extend_from_slice(&(8u16 | 0x8000).to_be_bytes());
                    d.extend_from_slice(&4u16.to_be_bytes());
                    d
                })],
            ),
        ),
        // Oversized field count.
        (
            "oversized_field_count",
            ipfix_packet(
                1,
                1_700_000_000,
                1,
                &[(2, {
                    let mut d = Vec::new();
                    d.extend_from_slice(&801u16.to_be_bytes());
                    d.extend_from_slice(&0xFFFFu16.to_be_bytes());
                    d
                })],
            ),
        ),
        // Invalid set length (length exceeds message).
        ("invalid_set_length", {
            let mut p = Vec::new();
            p.extend_from_slice(&10u16.to_be_bytes());
            p.extend_from_slice(&30u16.to_be_bytes()); // declared len 30
            p.extend_from_slice(&1_700_000_000u32.to_be_bytes());
            p.extend_from_slice(&1u32.to_be_bytes());
            p.extend_from_slice(&1u32.to_be_bytes());
            p.extend_from_slice(&2u16.to_be_bytes()); // template set
            p.extend_from_slice(&0xFFF0u16.to_be_bytes()); // absurd length
            p
        }),
        // Invalid message length (declared > datagram).
        ("invalid_message_length", {
            let mut p = Vec::new();
            p.extend_from_slice(&10u16.to_be_bytes());
            p.extend_from_slice(&60000u16.to_be_bytes());
            p.extend_from_slice(&1_700_000_000u32.to_be_bytes());
            p.extend_from_slice(&1u32.to_be_bytes());
            p.extend_from_slice(&1u32.to_be_bytes());
            p
        }),
        // Non-zero trailing bytes beyond declared message length.
        ("nonzero_trailing", {
            let mut p = ipfix_packet(1, 1_700_000_000, 1, &[]);
            p.extend_from_slice(&[0xFF, 0xFF]);
            p
        }),
        // Zero-length / nonsensical template set content.
        (
            "empty_template_set",
            ipfix_packet(1, 1_700_000_000, 1, &[(2, vec![])]),
        ),
        // Unsupported variable-length field (len = 0xFFFF).
        (
            "variable_length_field",
            ipfix_packet(
                1,
                1_700_000_000,
                1,
                &[(
                    2,
                    ipfix_template_set(&[ipfix_template_record(
                        802,
                        &[ipfix_field(8, 4, None), ipfix_field(70, 0xFFFF, None)],
                    )]),
                )],
            ),
        ),
        // Malformed v9: flowset length exceeds packet.
        ("v9_bad_flowset", {
            let mut p = Vec::new();
            p.extend_from_slice(&9u16.to_be_bytes());
            p.extend_from_slice(&1u16.to_be_bytes());
            p.extend_from_slice(&1u32.to_be_bytes());
            p.extend_from_slice(&1_700_000_000u32.to_be_bytes());
            p.extend_from_slice(&1u32.to_be_bytes());
            p.extend_from_slice(&7u32.to_be_bytes());
            p.extend_from_slice(&0u16.to_be_bytes());
            p.extend_from_slice(&0xFFF0u16.to_be_bytes());
            p
        }),
        // Truncated v9 header.
        ("v9_tiny", vec![0x00, 0x09, 0x00]),
        // Garbage version.
        ("bad_version", vec![0x13, 0x37, 0x00, 0x00]),
    ];

    for (name, pkt) in &malformed {
        send(&sock, &lab, pkt).await;
        let _ = name;
    }

    // Receiver must still be alive and functional: valid template+data.
    send(
        &sock,
        &lab,
        &ipfix_packet(1, 1_700_000_000, 10, &[(2, std_template_set_ipfix(900))]),
    )
    .await;
    let rec = ipfix_std_record([10, 8, 0, 1], [10, 8, 0, 2], 9999, 443, 6, 1, 1);
    send(
        &sock,
        &lab,
        &ipfix_packet(1, 1_700_000_000, 11, &[(900, rec.clone())]),
    )
    .await;
    let f = next_flow(&mut rx).await;
    assert_eq!(f.value["src_port"].as_u64().unwrap(), 9999);

    // Data referencing a template id that was never (validly) installed
    // must not produce a flow.
    send(
        &sock,
        &lab,
        &ipfix_packet(1, 1_700_000_000, 12, &[(950, rec)]),
    )
    .await;
    expect_no_flow(&mut rx, Duration::from_millis(300)).await;

    lab.stop().await;
}

/// Bounded cache: tiny per-domain template cap evicts oldest; data for
/// evicted templates is rejected while fresh ones still decode.
#[tokio::test]
async fn template_cache_is_bounded() {
    let lab = start_lab(NetflowConfig {
        max_flow_templates_per_domain: 2,
        max_flow_domains_per_context: 4,
        ..test_cfg()
    })
    .await;
    let mut rx = lab.rx.resubscribe();
    let (sock, _addr) = exporter_socket().await;

    // Install 3 templates (cap = 2) — tid 700 should be evicted.
    let t700 = ipfix_template_record(700, &[ipfix_field(8, 4, None), ipfix_field(4, 1, None)]);
    let t701 = ipfix_template_record(701, &[ipfix_field(8, 4, None), ipfix_field(4, 1, None)]);
    let t702 = ipfix_template_record(702, &[ipfix_field(8, 4, None), ipfix_field(4, 1, None)]);
    send(
        &sock,
        &lab,
        &ipfix_packet(
            2,
            1_700_000_000,
            1,
            &[(2, ipfix_template_set(&[t700, t701, t702]))],
        ),
    )
    .await;

    // Records for the small templates: src(4) + proto(1) = 5 bytes.
    let rec = |b: u8| {
        let mut r = Vec::new();
        r.extend_from_slice(&[10, 9, 0, b]);
        r.push(6);
        r
    };
    // tid 700 evicted → dropped; 701/702 live. The small template is
    // (src_ipv4, protocol) so the last octet of src_address identifies
    // which template id produced the flow.
    send(
        &sock,
        &lab,
        &ipfix_packet(2, 1_700_000_000, 2, &[(700, rec(1))]),
    )
    .await;
    send(
        &sock,
        &lab,
        &ipfix_packet(2, 1_700_000_000, 3, &[(701, rec(2))]),
    )
    .await;
    send(
        &sock,
        &lab,
        &ipfix_packet(2, 1_700_000_000, 4, &[(702, rec(3))]),
    )
    .await;

    let f1 = next_flow(&mut rx).await;
    let f2 = next_flow(&mut rx).await;
    let last_octets: Vec<u8> = [f1, f2]
        .iter()
        .map(|f| {
            f.value["src_address"]
                .as_str()
                .unwrap()
                .rsplit('.')
                .next()
                .unwrap()
                .parse()
                .unwrap()
        })
        .collect();
    assert!(
        !last_octets.contains(&1),
        "evicted template 700 must not decode: {last_octets:?}"
    );
    assert!(last_octets.contains(&2) && last_octets.contains(&3));

    lab.stop().await;
}

/// Provisional vs verified: a trusted binding that WOULD match on
/// transport/exporter evidence still cannot confer device_id over plain
/// UDP — resolution stays provisional.
#[tokio::test]
async fn udp_observations_never_resolve_canonical_identity() {
    let observed = TelemetrySourceIdentity::observed("127.0.0.1:49152")
        .with_protocol("netflow9")
        .with_exporter("flowsrc/bn021d-lab/nf9/127.0.0.1:49152/sid42")
        .with_domain(42)
        .with_collector("bn021d-lab");

    // A binding that pins every observable dimension — transport,
    // exporter claim, domain — would satisfy IdentityResolver::resolve,
    // but the flow path must never use it without verified ingress.
    let binding = TrustedBinding {
        binding_id: "b1".into(),
        scope: "default".into(),
        protocol: "netflow9".into(),
        exporter_id: "exp-rtr-a".into(),
        device_id: "dev-rtr-a".into(),
        match_transport: Some("127.0.0.1:49152".into()),
        match_reported_identity: None,
        match_exporter_claim: Some("flowsrc/bn021d-lab/nf9/127.0.0.1:49152/sid42".into()),
        match_observation_domain: Some(42),
        match_ingress_credential: None,
        provenance: BindingProvenance::OperatorProvisioned,
        status: BindingStatus::Active,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: "test".into(),
        updated_by: "test".into(),
    };

    let res = resolve_flow_identity(&observed, &[binding.clone()], None);
    assert_eq!(res.status, IdentityStatus::Provisional);
    assert!(res.device_id.is_none());
    assert!(res.canonical_exporter_id.is_none());
    assert!(res.binding_id.is_none());

    // Sanity: the claim-based resolver WOULD resolve this binding —
    // proving the flow path deliberately does not use it for UDP.
    let claim_res = bonsai::telemetry_identity::IdentityResolver::resolve(&observed, &[binding]);
    assert_eq!(claim_res.status, IdentityStatus::Resolved);
}

/// Scenario I: collector restart discards template state — post-restart
/// data is not decoded by pre-restart templates.
#[tokio::test]
async fn collector_restart_discards_template_state() {
    // First collector instance: install template, then stop it.
    // Template state is in-memory only, so a fresh instance starts empty.
    let lab1 = start_lab(test_cfg()).await;
    let (sock, _a) = exporter_socket().await;
    send(
        &sock,
        &lab1,
        &v9_packet(
            3,
            100_000,
            1_700_000_000,
            1,
            &[(
                0,
                v9_template_flowset(&[v9_template_record(310, STD_FIELDS)]),
            )],
        ),
    )
    .await;
    lab1.stop().await;

    // Second collector instance (new process state = empty cache).
    let lab2 = start_lab(test_cfg()).await;
    let mut rx = lab2.rx.resubscribe();
    let rec = encode_std_record([10, 3, 0, 1], [10, 3, 0, 2], 4444, 443, 6, 9, 1, 0, 0);
    // Data referencing the old template id — no template in new cache.
    send(
        &sock,
        &lab2,
        &v9_packet(
            3,
            100_000,
            1_700_000_000,
            2,
            &[(310, std_data_v9(&[rec.clone()]))],
        ),
    )
    .await;
    expect_no_flow(&mut rx, Duration::from_millis(400)).await;
    // Fresh template → decodes.
    send(
        &sock,
        &lab2,
        &v9_packet(
            3,
            100_000,
            1_700_000_000,
            3,
            &[(
                0,
                v9_template_flowset(&[v9_template_record(310, STD_FIELDS)]),
            )],
        ),
    )
    .await;
    send(
        &sock,
        &lab2,
        &v9_packet(3, 100_000, 1_700_000_000, 4, &[(310, std_data_v9(&[rec]))]),
    )
    .await;
    let f = next_flow(&mut rx).await;
    assert_eq!(f.value["src_port"].as_u64().unwrap(), 4444);

    lab2.stop().await;
}

/// Scenario J: downstream graph isolation — flows from two NAT-shared
/// exporters produce DISTINCT AppFlow nodes with per-source exporter_id,
/// and no canonical Device/CARRIES_FLOW merge for provisional sources.
#[tokio::test]
async fn graph_isolates_provisional_exporters() {
    use bonsai::graph::GraphStore;

    let lab = start_lab(test_cfg()).await;
    let mut rx = lab.rx.resubscribe();
    let (sock_a, _a) = exporter_socket().await;
    let (sock_b, _b) = exporter_socket().await;

    // Both exporters: same sid, same tid, same tuple — only port differs.
    for (sock, seq) in [(&sock_a, 1u32), (&sock_b, 1u32)] {
        send(
            sock,
            &lab,
            &v9_packet(
                77,
                100_000,
                1_700_000_000,
                seq,
                &[(
                    0,
                    v9_template_flowset(&[v9_template_record(320, STD_FIELDS)]),
                )],
            ),
        )
        .await;
    }
    let rec = encode_std_record([10, 20, 0, 1], [10, 20, 0, 2], 8080, 443, 6, 100, 5, 0, 0);
    for sock in [&sock_a, &sock_b] {
        send(
            sock,
            &lab,
            &v9_packet(
                77,
                100_000,
                1_700_000_000,
                2,
                &[(320, std_data_v9(&[rec.clone()]))],
            ),
        )
        .await;
    }

    let f1 = next_flow(&mut rx).await;
    let f2 = next_flow(&mut rx).await;
    assert_ne!(f1.target, f2.target);

    // Write both updates through the real graph store.
    let dir = tempfile::tempdir().unwrap();
    let store = GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();
    store.write(f1.clone()).await.unwrap();
    store.write(f2.clone()).await.unwrap();

    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();

    // Two distinct AppFlow nodes.
    let mut rows = conn
        .query(
            "MATCH (f:AppFlow) RETURN f.id, f.exporter_id, f.exporter_address, f.identity_status",
        )
        .unwrap();
    let mut flows = Vec::new();
    while let Some(row) = rows.next() {
        flows.push((
            row[0].to_string(),
            row[1].to_string(),
            row[2].to_string(),
            row[3].to_string(),
        ));
    }
    assert_eq!(
        flows.len(),
        2,
        "expected 2 distinct AppFlow nodes, got {flows:?}"
    );
    let eids: Vec<&str> = flows.iter().map(|f| f.1.as_str()).collect();
    assert_ne!(eids[0], eids[1], "exporter_ids must differ");
    for f in &flows {
        assert_eq!(f.3, "provisional");
        assert_eq!(f.2, "127.0.0.1");
    }

    // No CARRIES_FLOW edges — provisional sources must not merge into
    // canonical device inventory.
    let mut rows = conn
        .query("MATCH (:Device)-[:CARRIES_FLOW]->(f:AppFlow) RETURN count(f)")
        .unwrap();
    let row = rows.next().expect("count row");
    assert_eq!(
        row[0].to_string(),
        "0",
        "provisional flows must not link to Device nodes"
    );

    // DeviceAddress evidence for the observed transport IP still exists.
    let mut rows = conn
        .query("MATCH (a:DeviceAddress {ip: '127.0.0.1'}) RETURN a.source")
        .unwrap();
    assert!(
        rows.next().is_some(),
        "transport IP evidence should be registered"
    );

    lab.stop().await;
}
