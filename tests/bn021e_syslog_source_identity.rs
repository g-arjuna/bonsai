//! BN-021e — syslog source-identity isolation.
//!
//! A syslog message's hostname is NOT authenticated device identity:
//! - the observed transport endpoint (UDP sender / TCP connection peer)
//!   is evidence only;
//! - the claimed hostname and structured-data fields are self-reported;
//! - hostname→target matching produces a *candidate hypothesis*, never
//!   an authoritative join;
//! - the graph projection fails closed: provisional `StateChangeEvent`s
//!   scoped to `syslogsrc/...` keys plus read-only candidate probes —
//!   no `Device` upsert, no canonical state mutation, no `REPORTED_BY`.

use std::sync::Arc;
use std::time::Duration;

use bonsai::config::{SyslogConfig, TargetConfig};
use bonsai::event_bus::InProcessBus;
use bonsai::signals::syslog::run_syslog_receiver;
use bonsai::telemetry::TelemetryUpdate;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

// ── Lab fixture ─────────────────────────────────────────────────────────────

struct Lab {
    rx: broadcast::Receiver<Arc<TelemetryUpdate>>,
    udp_addr: String,
    tcp_addr: String,
    archive_path: String,
    shutdown_tx: watch::Sender<bool>,
    task: JoinHandle<anyhow::Result<()>>,
    _dirs: Vec<tempfile::TempDir>,
}

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn target(hostname: &str, address: &str, vendor: &str, site: &str) -> TargetConfig {
    TargetConfig {
        extra_ips: Vec::new(),
        optional: false,
        paths: Vec::new(),
        address: address.to_string(),
        enabled: true,
        tls_domain: None,
        ca_cert: None,
        vendor: Some(vendor.to_string()),
        credential_alias: None,
        username_env: None,
        password_env: None,
        username: None,
        password: None,
        hostname: Some(hostname.to_string()),
        role: Some("leaf".to_string()),
        site: Some(site.to_string()),
        collector_id: None,
        selected_paths: Vec::new(),
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
        last_operator_action: String::new(),
    }
}

async fn start_lab(udp: bool, tcp: bool, collector_id: &str, targets: Vec<TargetConfig>) -> Lab {
    let udp_port = free_port();
    let tcp_port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let pattern_dir = dir.path().join("patterns");
    std::fs::create_dir_all(&pattern_dir).unwrap();
    let archive_path = dir
        .path()
        .join("syslog.jsonl")
        .to_string_lossy()
        .into_owned();
    let cfg = SyslogConfig {
        enabled: true,
        udp_addr: if udp {
            format!("127.0.0.1:{udp_port}")
        } else {
            String::new()
        },
        tcp_addr: if tcp {
            format!("127.0.0.1:{tcp_port}")
        } else {
            String::new()
        },
        archive_path: archive_path.clone(),
        max_frame_bytes: 8192,
        collector_id: collector_id.to_string(),
    };
    let bus = InProcessBus::new(1024);
    let rx = bus.subscribe();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(run_syslog_receiver(
        cfg,
        pattern_dir.to_string_lossy().into_owned(),
        targets,
        Arc::clone(&bus),
        shutdown_rx,
        None,
        None,
        None,
        None,
    ));
    tokio::time::sleep(Duration::from_millis(80)).await;
    Lab {
        rx,
        udp_addr: format!("127.0.0.1:{udp_port}"),
        tcp_addr: format!("127.0.0.1:{tcp_port}"),
        archive_path,
        shutdown_tx,
        task,
        _dirs: vec![dir],
    }
}

impl Lab {
    async fn stop(mut self) {
        let _ = self.shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(2), &mut self.task).await;
    }
}

async fn next_update(
    rx: &mut broadcast::Receiver<Arc<TelemetryUpdate>>,
    path_prefix: &str,
) -> TelemetryUpdate {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            remaining > Duration::ZERO,
            "timed out waiting for {path_prefix}"
        );
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(u)) if u.path.starts_with(path_prefix) => return u.as_ref().clone(),
            Ok(Ok(_)) => continue,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(e)) => panic!("bus recv error: {e}"),
            Err(_) => panic!("timed out waiting for {path_prefix}"),
        }
    }
}

fn rfc5424(hostname: &str, msg: &str) -> String {
    format!("<134>1 2026-11-11T00:00:00Z {hostname} bgp 1 - - {msg}")
}

// ── Test A: claimed hostname impersonation ─────────────────────────────────

/// A sender at an unrelated transport address claims the hostname of a
/// registered device. The update must carry the provisional source key,
/// a unique-match *candidate* hypothesis — and no canonical identity.
#[tokio::test]
async fn claimed_hostname_never_becomes_canonical_identity() {
    let lab = start_lab(
        true,
        false,
        "site-a",
        vec![target("edge-rtr", "10.9.9.9:57400", "nokia_srl", "dc-a")],
    )
    .await;
    let mut rx = lab.rx.resubscribe();

    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    // Attacker/unrelated sender claims "edge-rtr".
    sock.send_to(
        rfc5424("edge-rtr", "BGP neighbor 10.1.0.1 changed state to down").as_bytes(),
        &lab.udp_addr,
    )
    .await
    .unwrap();

    let u = next_update(&mut rx, "signals/syslog/").await;
    let key = u.target.clone();
    assert!(
        key.starts_with("syslogsrc/site-a/udp/127.0.0.1:"),
        "target={key}"
    );
    assert_ne!(key, "10.9.9.9");
    assert_eq!(u.value["identity_status"], "provisional");
    assert!(u.value["device_id"].is_null());
    assert!(u.value["canonical_exporter_id"].is_null());
    assert_eq!(u.value["reported_hostname"], "edge-rtr");
    assert_eq!(u.value["transport_protocol"], "udp");
    // Candidate hypothesis preserved as evidence, clearly marked.
    assert_eq!(u.value["candidate"]["status"], "unique_hostname_match");
    assert_eq!(u.value["candidate"]["device_address"], "10.9.9.9");
    assert_eq!(u.value["authority"], "unverified");

    lab.stop().await;
}

// ── Test B: syslog relay preserves transport vs reported originators ───────

/// One relay forwards messages claiming two different originators. The
/// transport source key is shared; the reported hostnames stay distinct
/// evidence inside each payload.
#[tokio::test]
async fn relay_transport_and_reported_originators_stay_separate() {
    let lab = start_lab(true, false, "relay-site", vec![]).await;
    let mut rx = lab.rx.resubscribe();
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    sock.send_to(
        rfc5424("router-a", "interface Ethernet1 changed state to down").as_bytes(),
        &lab.udp_addr,
    )
    .await
    .unwrap();
    sock.send_to(
        rfc5424("router-b", "BFD session went down").as_bytes(),
        &lab.udp_addr,
    )
    .await
    .unwrap();

    let ua = next_update(&mut rx, "signals/syslog/").await;
    let ub = next_update(&mut rx, "signals/syslog/").await;
    assert_eq!(
        ua.target, ub.target,
        "same relay endpoint = same source key"
    );
    assert_eq!(ua.value["reported_hostname"], "router-a");
    assert_eq!(ub.value["reported_hostname"], "router-b");
    assert_eq!(ua.value["identity_status"], "provisional");
    assert_eq!(ub.value["identity_status"], "provisional");
    // No configured targets → both unresolved.
    assert_eq!(ua.value["candidate"]["status"], "unresolved");
    assert_eq!(ub.value["candidate"]["status"], "unresolved");

    lab.stop().await;
}

/// TCP connections get a per-connection session scope: two sequential
/// connections from the same peer never share a source key.
#[tokio::test]
async fn tcp_connections_get_distinct_session_scope() {
    let lab = start_lab(false, true, "tcp-site", vec![]).await;
    let mut rx = lab.rx.resubscribe();

    for _ in 0..2 {
        let mut stream = TcpStream::connect(&lab.tcp_addr).await.unwrap();
        stream
            .write_all(rfc5424("sw1", "license will expire soon").as_bytes())
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();
        stream.flush().await.unwrap();
        // Keep the connection open briefly so framing is newline-delimited.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let u0 = next_update(&mut rx, "signals/syslog/").await;
    let u1 = next_update(&mut rx, "signals/syslog/").await;
    assert!(u0.target.contains("/tcp/"), "key={}", u0.target);
    assert!(u0.target.contains("/conn"), "key={}", u0.target);
    assert_ne!(u0.target, u1.target, "distinct TCP sessions must not merge");
    assert_eq!(u0.value["transport_protocol"], "tcp");
    assert_eq!(u0.value["identity_status"], "provisional");

    lab.stop().await;
}

// ── Test C: NAT collision ───────────────────────────────────────────────────

/// Two senders behind one NAT address use different transport endpoints —
/// distinct provisional keys, no canonical collapse.
#[tokio::test]
async fn nat_collision_never_collapses_sources() {
    let lab = start_lab(true, false, "nat-site", vec![]).await;
    let mut rx = lab.rx.resubscribe();

    let sock_a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sock_b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port_a = sock_a.local_addr().unwrap().port();
    let port_b = sock_b.local_addr().unwrap().port();
    assert_ne!(port_a, port_b);

    let msg = rfc5424("claimed-same", "custom event one");
    sock_a.send_to(msg.as_bytes(), &lab.udp_addr).await.unwrap();
    sock_b.send_to(msg.as_bytes(), &lab.udp_addr).await.unwrap();

    let ua = next_update(&mut rx, "signals/syslog/").await;
    let ub = next_update(&mut rx, "signals/syslog/").await;
    assert_ne!(ua.target, ub.target, "NAT endpoints must not merge");
    assert!(
        ua.target.contains(&format!(":{port_a}")),
        "key={}",
        ua.target
    );
    assert!(
        ub.target.contains(&format!(":{port_b}")),
        "key={}",
        ub.target
    );

    lab.stop().await;
}

// ── Test D: duplicate hostname across tenants ───────────────────────────────

/// Two configured devices share a hostname in different sites. The claim
/// is ambiguous — no arbitrary canonical selection, both candidates kept
/// as evidence.
#[tokio::test]
async fn duplicate_hostname_across_tenants_is_ambiguous() {
    let lab = start_lab(
        true,
        false,
        "tenant-site",
        vec![
            target("dup-host", "10.1.0.1:57400", "nokia_srl", "site-a"),
            target("dup-host", "10.2.0.1:57400", "cisco_iosxr", "site-b"),
        ],
    )
    .await;
    let mut rx = lab.rx.resubscribe();

    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(
        rfc5424("dup-host", "BGP neighbor 10.9.9.1 down").as_bytes(),
        &lab.udp_addr,
    )
    .await
    .unwrap();

    let u = next_update(&mut rx, "signals/syslog/").await;
    assert_eq!(u.value["candidate"]["status"], "ambiguous_hostname_match");
    assert!(u.value["candidate"]["device_address"].is_null());
    let alts = u.value["candidate"]["alternatives"].as_array().unwrap();
    assert_eq!(alts.len(), 2, "both tenant candidates preserved: {alts:?}");
    assert_eq!(u.value["identity_status"], "provisional");
    // Ambiguous claims must not steer vendor pattern selection either.
    assert_eq!(u.vendor, "");

    lab.stop().await;
}

// ── Test I: unresolved origin ───────────────────────────────────────────────

/// An unknown hostname produces an explicit unresolved candidate status.
#[tokio::test]
async fn unknown_hostname_is_explicitly_unresolved() {
    let lab = start_lab(true, false, "iso-site", vec![]).await;
    let mut rx = lab.rx.resubscribe();

    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(
        rfc5424("no-such-device", "fan tray 1 failed").as_bytes(),
        &lab.udp_addr,
    )
    .await
    .unwrap();

    let u = next_update(&mut rx, "signals/syslog/").await;
    assert_eq!(u.value["candidate"]["status"], "unresolved");
    assert_eq!(u.value["reported_hostname"], "no-such-device");
    assert_eq!(u.value["identity_status"], "provisional");

    lab.stop().await;
}

// ── Test G: raw evidence retention ──────────────────────────────────────────

/// The raw archive keeps the verbatim message with provenance, and the
/// published update preserves transport details and extraction context.
#[tokio::test]
async fn raw_evidence_is_retained_with_provenance() {
    let lab = start_lab(true, false, "evidence-site", vec![]).await;
    let mut rx = lab.rx.resubscribe();

    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let raw_msg = rfc5424("evidence-host", "custom audit event XYZ-123");
    sock.send_to(raw_msg.as_bytes(), &lab.udp_addr)
        .await
        .unwrap();

    let u = next_update(&mut rx, "signals/syslog/").await;
    assert_eq!(u.value["raw"], raw_msg);
    assert_eq!(u.value["hostname"], "evidence-host");
    assert!(
        u.value["peer_addr"]
            .as_str()
            .unwrap()
            .starts_with("127.0.0.1:")
    );
    assert_eq!(u.value["transport"], "udp");
    assert_eq!(u.value["collector_id"], "evidence-site");
    assert!(
        u.value["source_key"]
            .as_str()
            .unwrap()
            .contains("evidence-site")
    );

    // JSONL archive retains the same evidence while the lab is alive.
    let contents = std::fs::read_to_string(&lab.archive_path).unwrap();
    assert!(contents.contains("evidence-host"), "archive={contents}");
    assert!(contents.contains("XYZ-123"), "archive={contents}");
    assert!(contents.contains("\"peer_addr\""), "archive={contents}");

    lab.stop().await;
}

// ── Test H: restart/replay cannot gain authority ────────────────────────────

/// Restarting the receiver and replaying the identical datagram yields
/// the same provisional status — replay never acquires authority.
#[tokio::test]
async fn replayed_message_stays_provisional_across_restart() {
    let targets = vec![target("stable-rtr", "10.7.7.7:57400", "nokia_srl", "dc-a")];
    let lab1 = start_lab(true, false, "replay-site", targets.clone()).await;
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let msg = rfc5424("stable-rtr", "BGP neighbor 10.1.0.1 down");
    sock.send_to(msg.as_bytes(), &lab1.udp_addr).await.unwrap();
    let mut rx1 = lab1.rx.resubscribe();
    let u1 = next_update(&mut rx1, "signals/syslog/").await;
    lab1.stop().await;

    let lab2 = start_lab(true, false, "replay-site", targets).await;
    let mut rx2 = lab2.rx.resubscribe();
    sock.send_to(msg.as_bytes(), &lab2.udp_addr).await.unwrap();
    let u2 = next_update(&mut rx2, "signals/syslog/").await;
    lab2.stop().await;

    for u in [&u1, &u2] {
        assert_eq!(u.value["identity_status"], "provisional");
        assert!(u.value["device_id"].is_null());
        assert_eq!(u.value["authority"], "unverified");
        assert!(u.target.starts_with("syslogsrc/replay-site/udp/"));
    }
}

// ── Tests E & F: graph-authority boundary ───────────────────────────────────

fn syslog_event_update(
    source_key: &str,
    category_path: &str,
    value_extra: serde_json::Value,
) -> TelemetryUpdate {
    let mut value = serde_json::json!({
        "timestamp_ns": 1_700_000_000_000_000_000i64,
        "priority": Some(134),
        "facility": Some(13),
        "severity": Some(6),
        "hostname": "edge-rtr",
        "app_name": "bgp",
        "proc_id": "1",
        "msg_id": "",
        "category": "protocol",
        "message": "BGP neighbor 10.1.0.1 changed state to down",
        "raw": "<134>1 2026-11-11T00:00:00Z edge-rtr bgp 1 - - BGP neighbor 10.1.0.1 changed state to down",
        "transport": "udp",
        "peer_addr": "127.0.0.1:5514",
        "source_key": source_key,
        "exporter_id": source_key,
        "identity_status": "provisional",
        "device_id": null,
        "canonical_exporter_id": null,
        "identity_resolution": null,
        "transport_source": "127.0.0.1:5514",
        "transport_protocol": "udp",
        "collector_id": "graph-site",
        "reported_hostname": "edge-rtr",
        "candidate": {
            "status": "unique_hostname_match",
            "device_address": "10.9.9.9",
            "hostname": "edge-rtr",
        },
        "authority": "unverified",
    });
    if let (serde_json::Value::Object(base), serde_json::Value::Object(ext)) =
        (&mut value, value_extra)
    {
        for (k, v) in ext {
            base.insert(k, v);
        }
    }
    TelemetryUpdate {
        target: source_key.to_string(),
        vendor: "nokia_srl".to_string(),
        hostname: "edge-rtr".to_string(),
        role: "leaf".to_string(),
        site: "dc-a".to_string(),
        timestamp_ns: 1_700_000_000_000_000_000,
        path: category_path.to_string(),
        value,
    }
}

fn syslog_fact_update(
    source_key: &str,
    fact_type: &str,
    fields: serde_json::Value,
    extra: serde_json::Value,
) -> TelemetryUpdate {
    let mut fact = serde_json::json!({
        "timestamp_ns": 1_700_000_000_000_000_000i64,
        "fact_type": fact_type,
        "category": "protocol",
        "hostname": "edge-rtr",
        "source_vendor": "nokia_srl",
        "message": "BGP neighbor 10.1.0.1 changed state to down",
        "raw": "raw",
        "transport": "udp",
        "peer_addr": "127.0.0.1:5514",
        "field_schema": {},
        "fields": fields,
        "source_key": source_key,
        "exporter_id": source_key,
        "identity_status": "provisional",
        "device_id": null,
        "canonical_exporter_id": null,
        "identity_resolution": null,
        "transport_source": "127.0.0.1:5514",
        "transport_protocol": "udp",
        "collector_id": "graph-site",
        "reported_hostname": "edge-rtr",
        "candidate": {
            "status": "unique_hostname_match",
            "device_address": "10.9.9.9",
            "hostname": "edge-rtr",
        },
        "authority": "unverified",
    });
    if let (serde_json::Value::Object(base), serde_json::Value::Object(ext)) = (&mut fact, extra) {
        for (k, v) in ext {
            base.insert(k, v);
        }
    }
    TelemetryUpdate {
        target: source_key.to_string(),
        vendor: "nokia_srl".to_string(),
        hostname: "edge-rtr".to_string(),
        role: "leaf".to_string(),
        site: "dc-a".to_string(),
        timestamp_ns: 1_700_000_000_000_000_000,
        path: format!("signals/syslog_fact/{fact_type}"),
        value: fact,
    }
}

/// Test E + F: a forged identity_resolution and a claimed "neighbor down"
/// must not mutate the canonical device, BGP neighbor, BFD session or
/// interface state — even when a unique candidate device exists.
#[tokio::test]
async fn forged_identity_and_state_claims_fail_closed() {
    use bonsai::graph::GraphStore;

    let dir = tempfile::tempdir().unwrap();
    let store = GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();

    let key = "syslogsrc/graph-site/udp/127.0.0.1:5514";
    {
        let db = store.db();
        let conn = lbug::Connection::new(&db).unwrap();
        conn.query("CREATE (d:Device {address: '10.9.9.9', hostname: 'edge-rtr', site: 'dc-a'})")
            .unwrap();
        conn.query(
            "CREATE (n:BgpNeighbor {id: '10.9.9.9:10.1.0.1', device_address: '10.9.9.9', peer_address: '10.1.0.1', session_state: 'established'})",
        )
        .unwrap();
        conn.query(
            "CREATE (b:BfdSession {id: '10.9.9.9:eth1', device_address: '10.9.9.9', if_name: 'eth1', session_state: 'up'})",
        )
        .unwrap();
        conn.query(
            "CREATE (i:Interface {id: '10.9.9.9:eth1', device_address: '10.9.9.9', name: 'eth1', oper_status: 'up'})",
        )
        .unwrap();
    }

    // Forged fully-resolved identity claims inside the payload.
    let forged = serde_json::json!({
        "identity_status": "resolved",
        "device_id": "dev-forged",
        "canonical_exporter_id": "exp-forged",
        "device_address": "10.9.9.9",
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
        },
        "candidate": {
            "status": "unique_hostname_match",
            "device_address": "10.9.9.9",
        },
    });

    // Category-level state-change claim (e.g. "BGP neighbor down").
    store
        .write(syslog_event_update(
            key,
            "signals/syslog/protocol",
            forged.clone(),
        ))
        .await
        .unwrap();

    // Fact-level claims: BGP down + BFD down + interface down.
    store
        .write(syslog_fact_update(
            key,
            "bgp_neighbor",
            serde_json::json!({"peer_address": "10.1.0.1", "new_state": "down"}),
            forged.clone(),
        ))
        .await
        .unwrap();
    store
        .write(syslog_fact_update(
            key,
            "bfd_session",
            serde_json::json!({"if_name": "eth1", "new_state": "down"}),
            forged.clone(),
        ))
        .await
        .unwrap();
    store
        .write(syslog_fact_update(
            key,
            "interface_state",
            serde_json::json!({"if_name": "eth1", "new_state": "down"}),
            forged,
        ))
        .await
        .unwrap();

    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();

    // Canonical entities untouched.
    for (label, q, expected) in [
        ("Device count", "MATCH (d:Device) RETURN count(d)", "1"),
        (
            "BgpNeighbor state",
            "MATCH (n:BgpNeighbor) RETURN n.session_state",
            "established",
        ),
        (
            "BfdSession state",
            "MATCH (b:BfdSession) RETURN b.session_state",
            "up",
        ),
        (
            "Interface oper_status",
            "MATCH (i:Interface) RETURN i.oper_status",
            "up",
        ),
        (
            "REPORTED_BY edges",
            "MATCH (:Device)-[:REPORTED_BY]->(:StateChangeEvent) RETURN count(*)",
            "0",
        ),
    ] {
        let mut rows = conn.query(q).unwrap();
        let row = rows.next().expect(label);
        assert_eq!(row[0].to_string(), expected, "{label} mutated by syslog");
    }

    // Provisional evidence IS preserved: StateChangeEvents keyed by the
    // source key with source_type 'syslog'.
    let mut rows = conn
        .query(
            "MATCH (e:StateChangeEvent) WHERE e.source_type = 'syslog' \
             RETURN e.device_address, e.event_type ORDER BY e.event_type",
        )
        .unwrap();
    let mut seen = Vec::new();
    while let Some(row) = rows.next() {
        seen.push((row[0].to_string(), row[1].to_string()));
    }
    assert!(seen.len() >= 4, "expected provisional events, got {seen:?}");
    for (addr, _) in &seen {
        assert_eq!(addr, key, "event must be scoped to source key");
    }
    assert!(seen.iter().any(|(_, t)| t == "syslog_protocol"));
    assert!(seen.iter().any(|(_, t)| t == "syslog_fact_candidate"));
}

/// A unique candidate yields read-only probe correlation: the fact
/// reports `candidate` status with graph state as evidence — but the
/// canonical neighbor state is not touched.
#[tokio::test]
async fn candidate_probe_is_read_only() {
    use bonsai::graph::GraphStore;

    let dir = tempfile::tempdir().unwrap();
    let store = GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();
    let key = "syslogsrc/graph-site/tcp/10.0.0.1:6514/conn3";
    {
        let db = store.db();
        let conn = lbug::Connection::new(&db).unwrap();
        conn.query("CREATE (d:Device {address: '10.9.9.9', hostname: 'edge-rtr'})")
            .unwrap();
        conn.query(
            "CREATE (n:BgpNeighbor {id: '10.9.9.9:10.1.0.1', device_address: '10.9.9.9', peer_address: '10.1.0.1', session_state: 'established'})",
        )
        .unwrap();
    }

    let mut rx = store.subscribe_events();
    store
        .write(syslog_fact_update(
            key,
            "bgp_neighbor",
            serde_json::json!({"peer_address": "10.1.0.1", "new_state": "down"}),
            serde_json::json!({}),
        ))
        .await
        .unwrap();

    let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("event timeout")
        .expect("recv event");
    assert_eq!(event.event_type, "syslog_fact_candidate");
    let detail: serde_json::Value = serde_json::from_str(&event.detail_json).unwrap();
    assert_eq!(detail["join"]["status"], "candidate");
    assert_eq!(detail["join"]["authority"], "unverified");
    assert_eq!(detail["join"]["graph_probe"]["matched"], true);
    assert_eq!(
        detail["join"]["graph_probe"]["graph_state"]["session_state"],
        "established"
    );

    // State untouched.
    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    let mut rows = conn
        .query("MATCH (n:BgpNeighbor) RETURN n.session_state")
        .unwrap();
    assert_eq!(rows.next().unwrap()[0].to_string(), "established");
}
