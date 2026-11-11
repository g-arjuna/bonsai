//! BN-021e — SNMP trap source-identity isolation.
//!
//! An SNMP trap's transport endpoint, community string, v1 agent-addr,
//! v3 engine IDs and USM security name are ALL self-reported claims:
//! - the observed UDP sender endpoint is evidence only;
//! - a configured polling target is a *candidate hypothesis*, never the
//!   authenticated sender;
//! - a recognized SNMPv3 security name is NOT USM authentication (the
//!   receiver does not verify the auth digest — explicitly deferred);
//! - the graph projection fails closed: provisional `StateChangeEvent`s
//!   scoped to `snmpsrc/...` keys plus read-only candidate probes —
//!   no `Device` upsert, no canonical state mutation, no `REPORTED_BY`.

use std::sync::Arc;
use std::time::Duration;

use bonsai::config::{SnmpConfig, SnmpV3User, TargetConfig};
use bonsai::event_bus::InProcessBus;
use bonsai::signals::snmp::run_snmp_receiver;
use bonsai::telemetry::TelemetryUpdate;
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

// ── BER helpers (mirror the receiver's wire format) ─────────────────────────

fn encode_tag(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend(encode_length(value.len()));
    out.extend_from_slice(value);
    out
}

fn encode_length(len: usize) -> Vec<u8> {
    if len < 0x80 {
        return vec![len as u8];
    }
    if len <= 0xff {
        return vec![0x81, len as u8];
    }
    vec![0x82, ((len >> 8) & 0xff) as u8, (len & 0xff) as u8]
}

fn encode_sequence(parts: &[Vec<u8>]) -> Vec<u8> {
    encode_tag(0x30, &parts.concat())
}

fn encode_integer(value: i64) -> Vec<u8> {
    let mut bytes = value.to_be_bytes().to_vec();
    while bytes.len() > 1
        && ((bytes[0] == 0x00 && bytes[1] & 0x80 == 0)
            || (bytes[0] == 0xff && bytes[1] & 0x80 == 0x80))
    {
        bytes.remove(0);
    }
    encode_tag(0x02, &bytes)
}

fn encode_octet_string(value: &[u8]) -> Vec<u8> {
    encode_tag(0x04, value)
}

fn encode_oid(oid: &str) -> Vec<u8> {
    let parts: Vec<u32> = oid.split('.').map(|p| p.parse().unwrap()).collect();
    let mut encoded = vec![(parts[0] * 40 + parts[1]) as u8];
    for part in parts.iter().skip(2) {
        let mut stack = vec![(part & 0x7f) as u8];
        let mut value = *part >> 7;
        while value > 0 {
            stack.push(((value & 0x7f) as u8) | 0x80);
            value >>= 7;
        }
        stack.reverse();
        encoded.extend(stack);
    }
    encode_tag(0x06, &encoded)
}

fn encode_varbind(oid: &str, value_tlv: Vec<u8>) -> Vec<u8> {
    encode_sequence(&[encode_oid(oid), value_tlv])
}

const OID_SYS_UPTIME: &str = "1.3.6.1.2.1.1.3.0";
const OID_SNMP_TRAP: &str = "1.3.6.1.6.3.1.1.4.1.0";
const OID_COLD_START: &str = "1.3.6.1.6.3.1.1.5.1";
const OID_LINK_DOWN: &str = "1.3.6.1.6.3.1.1.5.3";
const OID_IF_INDEX: &str = "1.3.6.1.2.1.2.2.1.1";
const OID_IF_NAME: &str = "1.3.6.1.2.1.2.2.1.2";
const OID_IF_OPER: &str = "1.3.6.1.2.1.2.2.1.8";

/// SNMPv2-Trap-PDU content (request-id, error-status, error-index, varbinds).
fn v2_trap_pdu(varbinds: Vec<Vec<u8>>) -> Vec<u8> {
    encode_tag(
        0xA7,
        &[
            encode_integer(1),
            encode_integer(0),
            encode_integer(0),
            encode_sequence(&varbinds),
        ]
        .concat(),
    )
}

fn cold_start_varbinds() -> Vec<Vec<u8>> {
    vec![
        encode_varbind(OID_SYS_UPTIME, encode_tag(0x43, &[0, 0, 0, 100])),
        encode_varbind(OID_SNMP_TRAP, encode_oid(OID_COLD_START)),
    ]
}

/// SNMPv1 trap: version=0, community, Trap-PDU (tag A4) with agent-addr.
fn v1_trap_packet(community: &str, agent_ip: [u8; 4], generic: i64, specific: i64) -> Vec<u8> {
    encode_sequence(&[
        encode_integer(0),
        encode_octet_string(community.as_bytes()),
        encode_tag(
            0xA4,
            &[
                encode_oid("1.3.6.1.4.1.9"),
                encode_tag(0x40, &agent_ip),
                encode_integer(generic),
                encode_integer(specific),
                encode_tag(0x43, &[0, 0, 1, 0]),
                encode_sequence(&[]),
            ]
            .concat(),
        ),
    ])
}

/// SNMPv2c trap: version=1, community, v2-Trap-PDU.
fn v2c_trap_packet(community: &str, varbinds: Vec<Vec<u8>>) -> Vec<u8> {
    encode_sequence(&[
        encode_integer(1),
        encode_octet_string(community.as_bytes()),
        v2_trap_pdu(varbinds),
    ])
}

/// SNMPv3 trap (authNoPriv *claim* — the auth digest is not valid; the
/// receiver must treat the security name as a claim only).
fn v3_trap_packet(
    security_name: &str,
    usm_engine_id: &[u8],
    context_engine_id: &[u8],
    auth_flag: bool,
    varbinds: Vec<Vec<u8>>,
) -> Vec<u8> {
    // UsmSecurityParameters ::= SEQUENCE { engineID, boots, time, userName, authParams, privParams }
    let usm_params = encode_sequence(&[
        encode_octet_string(usm_engine_id),
        encode_integer(1),
        encode_integer(12345),
        encode_octet_string(security_name.as_bytes()),
        encode_octet_string(&[0xAA; 12]), // bogus auth digest — never verified
        encode_octet_string(&[]),
    ]);
    // msgGlobalData ::= SEQUENCE { msgID, msgMaxSize, msgFlags, msgSecurityModel }
    let msg_flags = if auth_flag { 0x01 } else { 0x00 };
    let header = encode_sequence(&[
        encode_integer(7),
        encode_integer(1500),
        encode_octet_string(&[msg_flags]),
        encode_integer(3), // USM
    ]);
    // plaintext scopedPDU ::= SEQUENCE { contextEngineID, contextName, data }
    let scoped_pdu = encode_sequence(&[
        encode_octet_string(context_engine_id),
        encode_octet_string(b""),
        v2_trap_pdu(varbinds),
    ]);
    encode_sequence(&[
        encode_integer(3),
        header,
        encode_octet_string(&usm_params),
        scoped_pdu,
    ])
}

// ── Lab fixture ─────────────────────────────────────────────────────────────

struct Lab {
    rx: broadcast::Receiver<Arc<TelemetryUpdate>>,
    udp_addr: String,
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

fn v3_user(name: &str) -> SnmpV3User {
    SnmpV3User {
        security_name: name.to_string(),
        auth_protocol: "sha".to_string(),
        auth_key_env: "SNMP_TEST_KEY".to_string(),
        priv_protocol: "aes128".to_string(),
        priv_key_env: String::new(),
    }
}

async fn start_lab(
    collector_id: &str,
    targets: Vec<TargetConfig>,
    community_allowlist: Vec<String>,
    v3_users: Vec<SnmpV3User>,
) -> Lab {
    let udp_port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let pattern_dir = dir.path().join("patterns");
    std::fs::create_dir_all(&pattern_dir).unwrap();
    let archive_path = dir.path().join("snmp.jsonl").to_string_lossy().into_owned();
    let cfg = SnmpConfig {
        enabled: true,
        udp_addr: format!("127.0.0.1:{udp_port}"),
        archive_path: archive_path.clone(),
        max_frame_bytes: 8192,
        oid_pattern_dir: Some("config/snmp_oid_patterns".to_string()),
        community_allowlist,
        v3_users,
        collector_id: collector_id.to_string(),
    };
    let bus = InProcessBus::new(1024);
    let rx = bus.subscribe();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(run_snmp_receiver(
        cfg,
        targets,
        Arc::clone(&bus),
        shutdown_rx,
        None,
        None,
        None,
    ));
    tokio::time::sleep(Duration::from_millis(80)).await;
    Lab {
        rx,
        udp_addr: format!("127.0.0.1:{udp_port}"),
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

    async fn send(&self, packet: &[u8]) -> std::net::SocketAddr {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sock.send_to(packet, &self.udp_addr).await.unwrap();
        sock.local_addr().unwrap()
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

/// Collect all updates matching a prefix within a bounded window.
async fn collect_updates(
    rx: &mut broadcast::Receiver<Arc<TelemetryUpdate>>,
    path_prefix: &str,
    window: Duration,
) -> Vec<TelemetryUpdate> {
    let deadline = tokio::time::Instant::now() + window;
    let mut out = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return out;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(u)) if u.path.starts_with(path_prefix) => out.push(u.as_ref().clone()),
            Ok(Ok(_)) => continue,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) => return out,
            Err(_) => return out,
        }
    }
}

fn link_down_packet(community: &str) -> Vec<u8> {
    v2c_trap_packet(
        community,
        vec![
            encode_varbind(OID_SYS_UPTIME, encode_tag(0x43, &[0, 0, 0, 55])),
            encode_varbind(OID_SNMP_TRAP, encode_oid(OID_LINK_DOWN)),
            encode_varbind(OID_IF_INDEX, encode_integer(2)),
            encode_varbind(OID_IF_NAME, encode_octet_string(b"ethernet-1/1")),
            encode_varbind(OID_IF_OPER, encode_integer(2)),
        ],
    )
}

// ── A. Source-IP / configured-target impersonation ─────────────────────────

/// A trap sender whose IP matches a configured polling target gets only a
/// *candidate* hypothesis — the update target is the provisional source
/// key and no canonical identity is conferred.
#[tokio::test]
async fn source_ip_match_is_candidate_not_canonical() {
    let lab = start_lab(
        "site-a",
        vec![target("edge-rtr", "127.0.0.1:57400", "nokia_srl", "dc-a")],
        vec![],
        vec![],
    )
    .await;
    let mut rx = lab.rx.resubscribe();

    lab.send(&v2c_trap_packet("public", cold_start_varbinds()))
        .await;

    let u = next_update(&mut rx, "signals/snmp/").await;
    assert!(
        u.target.starts_with("snmpsrc/site-a/udp/127.0.0.1:"),
        "target={}",
        u.target
    );
    assert_eq!(u.value["identity_status"], "provisional");
    assert!(u.value["device_id"].is_null());
    assert_eq!(u.value["authority"], "unverified");
    assert_eq!(u.value["candidate"]["status"], "unique_target_match");
    assert_eq!(u.value["candidate"]["device_address"], "127.0.0.1");
    // Community string must not leak into the published value.
    assert_eq!(u.value["community"], "");
    assert_eq!(u.value["community_redacted"], true);
    assert_eq!(u.value["raw_hex"], "");
    assert_eq!(u.value["raw_hex_redacted"], true);
    // Fact extraction still works off the unique candidate.
    let f = next_update(&mut rx, "signals/snmp_fact/").await;
    assert_eq!(f.value["fact_type"], "cold_start");
    assert_eq!(f.value["identity_status"], "provisional");

    lab.stop().await;
}

// ── B. NAT-shared exporters with distinct claims ───────────────────────────

/// Two senders share the observed IP (different source ports = distinct
/// provisional keys) and report different v1 agent addresses — they must
/// not merge.
#[tokio::test]
async fn nat_shared_ip_senders_stay_separate() {
    let lab = start_lab("nat-site", vec![], vec![], vec![]).await;
    let mut rx = lab.rx.resubscribe();

    let sock_a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sock_b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port_a = sock_a.local_addr().unwrap().port();
    let port_b = sock_b.local_addr().unwrap().port();
    assert_ne!(port_a, port_b);

    sock_a
        .send_to(
            &v1_trap_packet("public", [10, 0, 0, 1], 6, 1),
            &lab.udp_addr,
        )
        .await
        .unwrap();
    sock_b
        .send_to(
            &v1_trap_packet("public", [10, 0, 0, 2], 6, 1),
            &lab.udp_addr,
        )
        .await
        .unwrap();

    let ua = next_update(&mut rx, "signals/snmp/").await;
    let ub = next_update(&mut rx, "signals/snmp/").await;
    assert_ne!(ua.target, ub.target, "distinct source ports must not merge");
    // Order of arrival is unspecified — compare as a set.
    let targets: std::collections::BTreeSet<&str> = [ua.target.as_str(), ub.target.as_str()]
        .into_iter()
        .collect();
    assert_eq!(targets.len(), 2);
    assert!(targets.iter().any(|t| t.contains(&format!(":{port_a}"))));
    assert!(targets.iter().any(|t| t.contains(&format!(":{port_b}"))));
    // Reported agent addresses stay distinct evidence.
    let agents: std::collections::BTreeSet<&str> = [
        ua.value["reported_agent_address"].as_str().unwrap(),
        ub.value["reported_agent_address"].as_str().unwrap(),
    ]
    .into_iter()
    .collect();
    assert_eq!(agents, ["10.0.0.1", "10.0.0.2"].into_iter().collect());
    for u in [&ua, &ub] {
        assert_eq!(u.value["identity_status"], "provisional");
        assert!(u.value["device_id"].is_null());
        assert_eq!(u.value["candidate"]["status"], "unresolved");
    }

    lab.stop().await;
}

// ── C. Duplicate IP across tenants ─────────────────────────────────────────

/// Two configured devices share one private IP. The match is ambiguous —
/// no arbitrary canonical selection.
#[tokio::test]
async fn duplicate_target_ip_is_ambiguous() {
    // Two configured devices share the loopback IP the test sender uses.
    let lab = start_lab(
        "tenant-site",
        vec![
            target("dev-a", "127.0.0.1:57400", "nokia_srl", "site-a"),
            target("dev-b", "127.0.0.1:57401", "cisco_iosxr", "site-b"),
        ],
        vec![],
        vec![],
    )
    .await;
    let mut rx = lab.rx.resubscribe();

    lab.send(&v2c_trap_packet("public", cold_start_varbinds()))
        .await;

    let u = next_update(&mut rx, "signals/snmp/").await;
    assert_eq!(u.value["candidate"]["status"], "ambiguous_target_match");
    assert!(u.value["candidate"]["device_address"].is_null());
    let alts = u.value["candidate"]["alternatives"].as_array().unwrap();
    assert_eq!(alts.len(), 2, "both tenant candidates preserved: {alts:?}");
    assert_eq!(u.value["identity_status"], "provisional");
    // Ambiguous matches must not steer vendor selection.
    assert_eq!(u.vendor, "");

    lab.stop().await;
}

// ── D + E. Forged SNMPv3 username and engine ID ────────────────────────────

/// An SNMPv3 packet carrying an *allowed* security name but no valid auth
/// digest gets no canonical attribution — username recognition is not
/// authentication.
#[tokio::test]
async fn forged_snmpv3_username_is_not_authentication() {
    let lab = start_lab(
        "v3-site",
        vec![target("v3-rtr", "127.0.0.1:57400", "nokia_srl", "dc-a")],
        vec![],
        vec![v3_user("allowed-user")],
    )
    .await;
    let mut rx = lab.rx.resubscribe();

    // Claims engine ID of "another device" + allowed username + authFlag.
    let packet = v3_trap_packet(
        "allowed-user",
        &[0x80, 0x00, 0x1F, 0x88, 0xDE, 0xAD], // forged authoritative engine ID
        &[0x80, 0x00, 0x1F, 0x88, 0xBE, 0xEF], // forged context engine ID
        true,
        cold_start_varbinds(),
    );
    lab.send(&packet).await;

    let u = next_update(&mut rx, "signals/snmp/").await;
    assert_eq!(u.value["identity_status"], "provisional");
    assert!(u.value["device_id"].is_null());
    assert_eq!(u.value["authority"], "unverified");
    // Claims preserved as evidence, explicitly unverified.
    assert_eq!(u.value["usm_security_name"], "allowed-user");
    assert_eq!(u.value["usm_auth_status"], "unverified");
    let engine_id = u.value["reported_engine_id"].as_str().unwrap();
    assert!(
        engine_id.contains("80001f88dead") && engine_id.contains("80001f88beef"),
        "engine_id={engine_id}"
    );
    // Engine/agent claims never select a canonical device.
    assert_eq!(u.value["candidate"]["status"], "unique_target_match");

    // Unknown security name IS dropped when v3_users is configured.
    let bad = v3_trap_packet(
        "intruder",
        &[1, 2, 3],
        &[4, 5, 6],
        true,
        cold_start_varbinds(),
    );
    lab.send(&bad).await;
    let extras = collect_updates(&mut rx, "signals/snmp/", Duration::from_millis(400)).await;
    assert!(
        extras
            .iter()
            .all(|u| u.value["usm_security_name"] != "intruder"),
        "unknown USM user must be dropped"
    );

    lab.stop().await;
}

// ── F. Community allowlist ─────────────────────────────────────────────────

/// A configured allowlist rejects unknown, empty, missing and malformed
/// communities; accepted communities still confer no canonical identity.
#[tokio::test]
async fn community_allowlist_semantics() {
    let lab = start_lab(
        "comm-site",
        vec![],
        vec!["good-community".to_string()],
        vec![],
    )
    .await;
    let mut rx = lab.rx.resubscribe();

    // Accepted community → published (provisional).
    lab.send(&v2c_trap_packet("good-community", cold_start_varbinds()))
        .await;
    let ok = next_update(&mut rx, "signals/snmp/").await;
    assert_eq!(ok.value["identity_status"], "provisional");
    assert_eq!(ok.value["community_present"], true);

    // Disallowed community → blocked.
    lab.send(&v2c_trap_packet("bad-community", cold_start_varbinds()))
        .await;
    // Empty community → blocked (previously bypassed the allowlist).
    lab.send(&v2c_trap_packet("", cold_start_varbinds())).await;
    // Malformed packet → parse error, empty community → blocked.
    lab.send(b"\x30\x82\xff\xffgarbage").await;

    let blocked = collect_updates(&mut rx, "signals/snmp/", Duration::from_millis(500)).await;
    assert!(
        blocked.is_empty(),
        "allowlist must reject bad/empty/malformed communities: {blocked:?}"
    );

    // Blocked traps are still archived for forensic review.
    let archive = std::fs::read_to_string(&lab.archive_path).unwrap();
    assert!(archive.contains("bad-community"), "archive={archive}");

    lab.stop().await;
}

// ── G. Dedup ordering ──────────────────────────────────────────────────────

/// A packet rejected by the community filter must not poison dedup for a
/// later accepted packet with the same OID; genuine duplicates within the
/// window are still suppressed.
#[tokio::test]
async fn filtered_packets_cannot_poison_dedup() {
    let lab = start_lab("dedup-site", vec![], vec!["good".to_string()], vec![]).await;
    let mut rx = lab.rx.resubscribe();

    // Steps 1–3 must share ONE transport endpoint (same ip:port ⇒ same
    // provisional source key) or dedup cannot engage.
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // 1. Disallowed community, same trap OID — filtered out BEFORE dedup.
    sock.send_to(
        &v2c_trap_packet("evil", cold_start_varbinds()),
        &lab.udp_addr,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 2. Allowed community, same OID, same endpoint — must NOT be
    //    dedup-suppressed by the filtered packet.
    sock.send_to(
        &v2c_trap_packet("good", cold_start_varbinds()),
        &lab.udp_addr,
    )
    .await
    .unwrap();
    let accepted = next_update(&mut rx, "signals/snmp/").await;
    assert_eq!(accepted.value["identity_status"], "provisional");

    // 3. Identical allowed packet again, same endpoint → dedup suppresses.
    sock.send_to(
        &v2c_trap_packet("good", cold_start_varbinds()),
        &lab.udp_addr,
    )
    .await
    .unwrap();
    let repeats = collect_updates(&mut rx, "signals/snmp/", Duration::from_millis(500)).await;
    assert!(
        repeats.is_empty(),
        "in-window duplicate must be suppressed: {repeats:?}"
    );

    // 4. Different source port → different source key → not suppressed.
    lab.send(&v2c_trap_packet("good", cold_start_varbinds()))
        .await;
    let _ = next_update(&mut rx, "signals/snmp/").await;

    lab.stop().await;
}

// ── H + I. Graph isolation and forged identity ─────────────────────────────

fn snmp_trap_update(
    source_key: &str,
    event_type: &str,
    value_extra: serde_json::Value,
) -> TelemetryUpdate {
    let mut value = serde_json::json!({
        "timestamp_ns": 1_700_000_000_000_000_000i64,
        "peer_addr": "127.0.0.1:55555",
        "version": "v2c",
        "community": "",
        "community_redacted": true,
        "event_type": event_type,
        "trap_oid": OID_LINK_DOWN,
        "category": "interface",
        "message": "linkDown trap received",
        "enterprise_oid": "1.3.6.1.6.3.1.1.5",
        "generic_trap": null,
        "specific_trap": null,
        "uptime_ticks": 100,
        "varbinds": [],
        "parse_error": "",
        "raw_hex": "",
        "raw_hex_redacted": true,
        "raw_len": 42,
        "usm_security_name": "",
        "usm_auth_protocol": "",
        "engine_id": "",
        "agent_address": "",
        "source_key": source_key,
        "exporter_id": source_key,
        "identity_status": "provisional",
        "device_id": null,
        "canonical_exporter_id": null,
        "identity_resolution": null,
        "transport_source": "127.0.0.1:55555",
        "transport_protocol": "udp",
        "collector_id": "graph-site",
        "usm_auth_status": "unverified",
        "community_present": true,
        "candidate": {
            "status": "unique_target_match",
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
        hostname: String::new(),
        role: "leaf".to_string(),
        site: "dc-a".to_string(),
        timestamp_ns: 1_700_000_000_000_000_000,
        path: format!("signals/snmp/{event_type}"),
        value,
    }
}

fn snmp_fact_update(
    source_key: &str,
    fact_type: &str,
    fields: serde_json::Value,
    extra: serde_json::Value,
) -> TelemetryUpdate {
    let mut fact = serde_json::json!({
        "timestamp_ns": 1_700_000_000_000_000_000i64,
        "fact_type": fact_type,
        "trap_oid": OID_LINK_DOWN,
        "peer_addr": "127.0.0.1:55555",
        "enterprise_oid": "1.3.6.1.6.3.1.1.5",
        "field_schema": {},
        "fields": fields,
        "source_key": source_key,
        "exporter_id": source_key,
        "identity_status": "provisional",
        "device_id": null,
        "canonical_exporter_id": null,
        "identity_resolution": null,
        "transport_source": "127.0.0.1:55555",
        "transport_protocol": "udp",
        "collector_id": "graph-site",
        "candidate": {
            "status": "unique_target_match",
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
        hostname: String::new(),
        role: "leaf".to_string(),
        site: "dc-a".to_string(),
        timestamp_ns: 1_700_000_000_000_000_000,
        path: format!("signals/snmp_fact/{fact_type}"),
        value: fact,
    }
}

/// Prepopulate canonical Device/DeviceAddress/Interface/BgpNeighbor state,
/// then submit provisional SNMP traps + facts (including a fully forged
/// identity_resolution). Authoritative entities must remain unchanged.
#[tokio::test]
async fn graph_fails_closed_for_provisional_snmp() {
    use bonsai::graph::GraphStore;

    let dir = tempfile::tempdir().unwrap();
    let store = GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();

    let key = "snmpsrc/graph-site/udp/127.0.0.1:55555";
    {
        let db = store.db();
        let conn = lbug::Connection::new(&db).unwrap();
        conn.query("CREATE (d:Device {address: '10.9.9.9', hostname: 'edge-rtr', site: 'dc-a'})")
            .unwrap();
        conn.query("CREATE (a:DeviceAddress {ip: '10.9.9.9', source: 'gnmi'})")
            .unwrap();
        conn.query(
            "CREATE (n:BgpNeighbor {id: '10.9.9.9:10.1.0.1', device_address: '10.9.9.9', peer_address: '10.1.0.1', session_state: 'established'})",
        )
        .unwrap();
        conn.query(
            "CREATE (i:Interface {id: '10.9.9.9:ethernet-1/1', device_address: '10.9.9.9', name: 'ethernet-1/1', oper_status: 'up', in_octets: 1000})",
        )
        .unwrap();
    }

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
            "status": "unique_target_match",
            "device_address": "10.9.9.9",
        },
    });

    // Category-level trap event.
    store
        .write(snmp_trap_update(key, "snmp_link_down", forged.clone()))
        .await
        .unwrap();
    // Fact-level claims: interface down + BGP down.
    store
        .write(snmp_fact_update(
            key,
            "link_down",
            serde_json::json!({"interface_name": "ethernet-1/1", "oper_status": "2"}),
            forged.clone(),
        ))
        .await
        .unwrap();
    store
        .write(snmp_fact_update(
            key,
            "bgp_peer_state",
            serde_json::json!({"peer_address": "10.1.0.1", "peer_state": "1"}),
            forged,
        ))
        .await
        .unwrap();

    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();

    for (label, q, expected) in [
        ("Device count", "MATCH (d:Device) RETURN count(d)", "1"),
        (
            "DeviceAddress count",
            "MATCH (a:DeviceAddress) RETURN count(a)",
            "1",
        ),
        (
            "BgpNeighbor state",
            "MATCH (n:BgpNeighbor) RETURN n.session_state",
            "established",
        ),
        (
            "Interface oper_status",
            "MATCH (i:Interface) RETURN i.oper_status",
            "up",
        ),
        (
            "Interface counters",
            "MATCH (i:Interface) RETURN i.in_octets",
            "1000",
        ),
        (
            "REPORTED_BY edges",
            "MATCH (:Device)-[:REPORTED_BY]->(:StateChangeEvent) RETURN count(*)",
            "0",
        ),
    ] {
        let mut rows = conn.query(q).unwrap();
        let row = rows.next().expect(label);
        assert_eq!(row[0].to_string(), expected, "{label} mutated by snmp");
    }

    // Provisional evidence IS preserved.
    let mut rows = conn
        .query(
            "MATCH (e:StateChangeEvent) WHERE e.source_type = 'snmp' \
             RETURN e.device_address, e.event_type ORDER BY e.event_type",
        )
        .unwrap();
    let mut seen = Vec::new();
    while let Some(row) = rows.next() {
        seen.push((row[0].to_string(), row[1].to_string()));
    }
    assert!(seen.len() >= 3, "expected provisional events, got {seen:?}");
    for (addr, _) in &seen {
        assert_eq!(addr, key, "event must be scoped to source key");
    }
    assert!(seen.iter().any(|(_, t)| t == "snmp_link_down"));
    assert!(seen.iter().any(|(_, t)| t == "snmp_fact_candidate"));
}

/// A unique candidate yields read-only probe correlation — canonical
/// neighbor state is reported as evidence but never touched.
#[tokio::test]
async fn candidate_probe_is_read_only() {
    use bonsai::graph::GraphStore;

    let dir = tempfile::tempdir().unwrap();
    let store = GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();
    let key = "snmpsrc/graph-site/udp/127.0.0.1:55555";
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
        .write(snmp_fact_update(
            key,
            "bgp_peer_state",
            serde_json::json!({"peer_address": "10.1.0.1", "peer_state": "1"}),
            serde_json::json!({}),
        ))
        .await
        .unwrap();

    let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("event timeout")
        .expect("recv event");
    assert_eq!(event.event_type, "snmp_fact_candidate");
    let detail: serde_json::Value = serde_json::from_str(&event.detail_json).unwrap();
    assert_eq!(detail["join"]["status"], "candidate");
    assert_eq!(detail["join"]["authority"], "unverified");
    assert_eq!(detail["join"]["graph_probe"]["matched"], true);
    assert_eq!(
        detail["join"]["graph_probe"]["graph_state"]["session_state"],
        "established"
    );

    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    let mut rows = conn
        .query("MATCH (n:BgpNeighbor) RETURN n.session_state")
        .unwrap();
    assert_eq!(rows.next().unwrap()[0].to_string(), "established");
}

// ── J. Parsing compatibility ───────────────────────────────────────────────

/// v1, v2c and v3 traps all parse and publish with explicit version and
/// claim metadata; unsupported encrypted scoped PDUs report a parse error.
#[tokio::test]
async fn parsing_compatibility_across_versions() {
    let lab = start_lab("parse-site", vec![], vec![], vec![v3_user("u1")]).await;
    let mut rx = lab.rx.resubscribe();

    // v1 trap with agent address claim.
    lab.send(&v1_trap_packet("public", [192, 0, 2, 99], 0, 0))
        .await;
    let v1 = next_update(&mut rx, "signals/snmp/").await;
    assert_eq!(v1.value["version"], "v1");
    assert_eq!(v1.value["reported_agent_address"], "192.0.2.99");
    assert_eq!(v1.value["event_type"], "snmp_cold_start");

    // v2c linkDown → fact extraction.
    lab.send(&link_down_packet("public")).await;
    let v2 = next_update(&mut rx, "signals/snmp/").await;
    assert_eq!(v2.value["version"], "v2c");
    assert_eq!(v2.value["event_type"], "snmp_link_down");
    let fact = next_update(&mut rx, "signals/snmp_fact/link_down").await;
    assert_eq!(fact.value["fields"]["interface_name"], "ethernet-1/1");

    // v3 trap.
    lab.send(&v3_trap_packet(
        "u1",
        &[9, 9],
        &[8, 8],
        false,
        cold_start_varbinds(),
    ))
    .await;
    let v3 = next_update(&mut rx, "signals/snmp/").await;
    assert_eq!(v3.value["version"], "v3");
    assert_eq!(v3.value["usm_security_name"], "u1");

    // Encrypted scopedPDU (msgData as OCTET STRING) → explicit parse error.
    let enc = encode_sequence(&[
        encode_integer(3),
        encode_sequence(&[
            encode_integer(7),
            encode_integer(1500),
            encode_octet_string(&[0x03]), // auth+priv flags
            encode_integer(3),
        ]),
        encode_octet_string(&encode_sequence(&[
            encode_octet_string(&[1]),
            encode_integer(1),
            encode_integer(1),
            encode_octet_string(b"u1"),
            encode_octet_string(&[0; 12]),
            encode_octet_string(&[0; 8]),
        ])),
        encode_octet_string(&[0xDE, 0xAD]), // encrypted scopedPDU
    ]);
    lab.send(&enc).await;
    let raw = next_update(&mut rx, "signals/snmp/snmp_raw").await;
    assert!(
        !raw.value["parse_error"].as_str().unwrap().is_empty(),
        "encrypted scoped PDU must surface explicit parse_error"
    );

    lab.stop().await;
}

// ── K. Archive and restart ─────────────────────────────────────────────────

/// Raw forensic archive retains verbatim evidence (including the
/// community string — restricted storage); replay after restart stays
/// provisional.
#[tokio::test]
async fn archive_retains_evidence_and_restart_stays_provisional() {
    let targets = vec![];
    let lab1 = start_lab("arch-site", targets.clone(), vec![], vec![]).await;
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let packet = v2c_trap_packet("secret-community", cold_start_varbinds());
    sock.send_to(&packet, &lab1.udp_addr).await.unwrap();
    let mut rx1 = lab1.rx.resubscribe();
    let u1 = next_update(&mut rx1, "signals/snmp/").await;
    // Read the archive BEFORE stopping: Lab owns the TempDir and dropping
    // it deletes the archive file.
    let contents = std::fs::read_to_string(&lab1.archive_path).unwrap();
    lab1.stop().await;
    assert!(contents.contains("secret-community"), "archive={contents}");
    assert!(contents.contains("snmp_cold_start"), "archive={contents}");

    let lab2 = start_lab("arch-site", targets, vec![], vec![]).await;
    let mut rx2 = lab2.rx.resubscribe();
    sock.send_to(&packet, &lab2.udp_addr).await.unwrap();
    let u2 = next_update(&mut rx2, "signals/snmp/").await;
    lab2.stop().await;

    for u in [&u1, &u2] {
        assert_eq!(u.value["identity_status"], "provisional");
        assert!(u.value["device_id"].is_null());
        assert_eq!(u.value["authority"], "unverified");
        assert!(u.target.starts_with("snmpsrc/arch-site/udp/"));
    }
}

// ── L. Malformed input ─────────────────────────────────────────────────────

/// Malformed BER, truncated and oversized messages must not panic or
/// allocate unboundedly; they surface as parse_error/raw events.
#[tokio::test]
async fn malformed_input_is_safe() {
    let lab = start_lab("mal-site", vec![], vec![], vec![]).await;
    let mut rx = lab.rx.resubscribe();

    // Garbage bytes.
    lab.send(b"not an snmp packet at all").await;
    // BER length claims more than buffer holds.
    lab.send(&[0x30, 0x82, 0xFF, 0xFF, 0x02, 0x01]).await;
    // Truncated sequence.
    lab.send(&[0x30, 0x10, 0x02, 0x01]).await;
    // Unsupported version (v4).
    lab.send(&encode_sequence(&[encode_integer(4)])).await;

    // Each produces at most a raw/parse-error event; none may panic.
    let mut got = 0;
    for _ in 0..4 {
        if let Ok(Ok(u)) = tokio::time::timeout(Duration::from_millis(700), rx.recv()).await {
            let _ = u;
            got += 1;
        }
    }
    assert!(got >= 1, "expected at least one raw/parse-error event");

    lab.stop().await;
}
