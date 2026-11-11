//! BN-021f hardening: TLS handshake availability, active-session
//! credential/binding revocation, certificate rotation, and production
//! deployment policy.
//!
//! Complements `bn021f_authenticated_ingress.rs` (tests A–I) with the
//! lifecycle and availability regressions required by the PR #2
//! security review.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use bonsai::routing_service::{
    build_bmp_mtls_acceptor, build_speakers_json, run_bmp_listener, BmpIdentityMode, BmpMtlsConfig,
    DeploymentSecurityOverrides, RoutingServiceConfig, RoutingServiceState,
};
use bonsai::sqlite_store::{SqliteStore, CONFIG_DB_FILE_NAME};
use bonsai::telemetry_identity::{
    BindingProvenance, BindingStatus, CanonicalDevice, TrustedBinding,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

// ── Listener tuning (process-wide; identical for every test here) ────────────
//
// These are read once per `run_bmp_listener` spawn. All tests in this
// binary use the SAME values so parallel execution cannot observe
// different limits.
const MAX_HANDSHAKES: usize = 2;
const HANDSHAKE_TIMEOUT_SECS: f64 = 1.0;
const RECHECK_INTERVAL_MS: u64 = 200;

fn set_listener_tuning() {
    // SAFETY: tests in this binary only ever set these variables to the
    // same constant values; no other thread reads them concurrently
    // before the listener spawn that consumes them.
    unsafe {
        std::env::set_var(
            "BONSAI_ROUTING_BMP_TLS_MAX_HANDSHAKES",
            MAX_HANDSHAKES.to_string(),
        );
        std::env::set_var(
            "BONSAI_ROUTING_BMP_TLS_HANDSHAKE_TIMEOUT_SECS",
            HANDSHAKE_TIMEOUT_SECS.to_string(),
        );
        std::env::set_var(
            "BONSAI_ROUTING_BMP_RECHECK_INTERVAL_MS",
            RECHECK_INTERVAL_MS.to_string(),
        );
    }
}

// ── Synthetic PKI (rcgen, test-only) ─────────────────────────────────────────

struct Pki {
    ca_cert_pem: String,
    ca_cert_der: Vec<u8>,
    ca_key: rcgen::KeyPair,
    ca_cert: rcgen::Certificate,
}

#[derive(Clone)]
struct LeafCert {
    cert_pem: String,
    key_pem: String,
    cert_der: Vec<u8>,
    fingerprint: String,
}

fn make_pki() -> Pki {
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    Pki {
        ca_cert_pem: ca_cert.pem(),
        ca_cert_der: ca_cert.der().to_vec(),
        ca_key,
        ca_cert,
    }
}

fn issue_leaf(pki: &Pki, cn: &str, server: bool) -> LeafCert {
    let mut params = if server {
        rcgen::CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .unwrap()
    } else {
        rcgen::CertificateParams::new(Vec::<String>::new()).unwrap()
    };
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, cn);
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.signed_by(&key, &pki.ca_cert, &pki.ca_key).unwrap();
    let der = cert.der().to_vec();
    LeafCert {
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
        fingerprint: format!("sha256:{:x}", Sha256::digest(&der)),
        cert_der: der,
    }
}

// ── Lab fixture ──────────────────────────────────────────────────────────────

const SYNC_ADDR: &str = "127.0.0.1";

struct Lab {
    dir: TempDir,
    state: Arc<RoutingServiceState>,
    bmp_addr: std::net::SocketAddr,
    pki: Pki,
    cert_a: LeafCert,
    cert_b: LeafCert,
    _server_cert: LeafCert,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    listener_handle: tokio::task::JoinHandle<()>,
}

struct Certs {
    a: LeafCert,
    b: LeafCert,
}

async fn new_lab(provision: impl FnOnce(&SqliteStore, &Certs)) -> Lab {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_test_writer()
        .try_init();
    set_listener_tuning();
    let dir = tempfile::tempdir().unwrap();
    let pki = make_pki();
    let server_cert = issue_leaf(&pki, "bonsai-bmp", true);
    let certs = Certs {
        a: issue_leaf(&pki, "exporter-a", false),
        b: issue_leaf(&pki, "exporter-b", false),
    };

    let cert_path = dir.path().join("server.crt");
    let key_path = dir.path().join("server.key");
    let ca_path = dir.path().join("ca.crt");
    std::fs::write(&cert_path, &server_cert.cert_pem).unwrap();
    std::fs::write(&key_path, &server_cert.key_pem).unwrap();
    std::fs::write(&ca_path, &pki.ca_cert_pem).unwrap();
    let mtls = BmpMtlsConfig {
        cert_path,
        key_path,
        client_ca_path: ca_path,
    };

    {
        let store = SqliteStore::open(dir.path()).unwrap();
        provision(&store, &certs);
    }

    let state = RoutingServiceState::new(RoutingServiceConfig {
        bmp_listen_addr: "127.0.0.1:0".into(),
        http_listen_addr: "127.0.0.1:0".into(),
        bmp_journal_path: Some(dir.path().join("bmp.jsonl")),
        event_journal_path: Some(dir.path().join("events.jsonl")),
        max_frame_bytes: rbmp_core::bmp::parser::DEFAULT_MAX_FRAME,
        internal_service_token: Some("bn021f-hardening-token".into()),
        identity_db_path: Some(dir.path().join(CONFIG_DB_FILE_NAME)),
        identity_scope: None,
        bmp_identity_mode: BmpIdentityMode::Verified,
        bmp_mtls: Some(mtls.clone()),
    })
    .await
    .unwrap();

    let acceptor: TlsAcceptor = build_bmp_mtls_acceptor(&mtls).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bmp_addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let st = Arc::clone(&state);
    let listener_handle = tokio::spawn(async move {
        run_bmp_listener(listener, Some(acceptor), st, shutdown_rx).await;
    });

    Lab {
        dir,
        state,
        bmp_addr,
        pki,
        cert_a: certs.a,
        cert_b: certs.b,
        _server_cert: server_cert,
        shutdown_tx,
        listener_handle,
    }
}

impl Lab {
    fn store(&self) -> SqliteStore {
        SqliteStore::open(self.dir.path()).unwrap()
    }
}

fn provision_binding(
    store: &SqliteStore,
    device_id: &str,
    exporter_id: &str,
    sys_name: &str,
    credential_fp: &str,
) -> String {
    store
        .upsert_canonical_device(
            &CanonicalDevice {
                device_id: device_id.to_string(),
                address: "10.240.0.200".into(),
                site: Some("lab".into()),
                hostname: Some(sys_name.to_string()),
                created_at_ns: 0,
                updated_at_ns: 0,
                created_by: String::new(),
                updated_by: String::new(),
            },
            "bn021f-hardening",
        )
        .unwrap();
    let binding = TrustedBinding {
        binding_id: format!("bind-{device_id}"),
        scope: "default".into(),
        protocol: "bmp".into(),
        exporter_id: exporter_id.to_string(),
        device_id: device_id.to_string(),
        match_transport: Some(SYNC_ADDR.to_string()),
        match_reported_identity: Some(sys_name.to_string()),
        match_exporter_claim: Some(format!("bmp:{sys_name}")),
        match_observation_domain: None,
        match_ingress_credential: Some(credential_fp.to_string()),
        provenance: BindingProvenance::OperatorProvisioned,
        status: BindingStatus::Active,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
    };
    store
        .upsert_exporter_binding(&binding, "bn021f-hardening")
        .unwrap();
    binding.binding_id
}

// ── BMP wire + TLS client helpers ────────────────────────────────────────────

fn bmp_tlv(t: u16, v: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + v.len());
    out.extend_from_slice(&t.to_be_bytes());
    out.extend_from_slice(&(v.len() as u16).to_be_bytes());
    out.extend_from_slice(v);
    out
}

fn bmp_frame(msg_type: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + body.len());
    out.push(3);
    out.extend_from_slice(&((6 + body.len()) as u32).to_be_bytes());
    out.push(msg_type);
    out.extend_from_slice(body);
    out
}

fn initiation_frame(sys_name: &str) -> Vec<u8> {
    let mut body = bmp_tlv(1, b"FRR 10.6.1_lab");
    body.extend(bmp_tlv(2, sys_name.as_bytes()));
    bmp_frame(4, &body)
}

fn client_connector(lab: &Lab, client: Option<&LeafCert>) -> TlsConnector {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(
            lab.pki.ca_cert_der.clone(),
        ))
        .unwrap();
    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots);
    let config = match client {
        Some(c) => builder
            .with_client_auth_cert(
                vec![rustls::pki_types::CertificateDer::from(c.cert_der.clone())],
                rustls::pki_types::PrivateKeyDer::try_from(
                    rustls_pemfile::private_key(&mut c.key_pem.as_bytes())
                        .unwrap()
                        .unwrap(),
                )
                .unwrap(),
            )
            .unwrap(),
        None => builder.with_no_client_auth(),
    };
    TlsConnector::from(Arc::new(config))
}

type ClientTls = tokio_rustls::client::TlsStream<TcpStream>;

async fn tls_connect(lab: &Lab, client: Option<&LeafCert>) -> anyhow::Result<ClientTls> {
    let tcp = TcpStream::connect(lab.bmp_addr).await?;
    let connector = client_connector(lab, client);
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    connector.connect(name, tcp).await.map_err(Into::into)
}

async fn send_frames<S: tokio::io::AsyncWrite + Unpin>(stream: &mut S, frames: &[Vec<u8>]) {
    for f in frames {
        stream.write_all(f).await.unwrap();
    }
    stream.flush().await.unwrap();
}

async fn connect_exporter(
    lab: &Lab,
    cert: Option<&LeafCert>,
    sys_name: &str,
    peer_ip: Ipv4Addr,
    peer_as: u32,
    prefix: [u8; 3],
) -> anyhow::Result<ClientTls> {
    let mut tls = tls_connect(lab, cert).await?;
    let (_, peer_up, route) =
        bonsai::routing_service::sample_bmp_session_frames(peer_ip, peer_as, prefix, 100);
    send_frames(&mut tls, &[initiation_frame(sys_name), peer_up, route]).await;
    Ok(tls)
}

// ── Speaker assertions ───────────────────────────────────────────────────────

async fn speaker_map(state: &RoutingServiceState) -> serde_json::Value {
    build_speakers_json(state).await
}

fn speaker_entry(map: &serde_json::Value, needle: &str) -> Option<serde_json::Value> {
    map["speakers"]
        .as_array()?
        .iter()
        .find(|s| {
            s["exporter_instance"]
                .as_str()
                .unwrap_or("")
                .contains(needle)
        })
        .cloned()
}

async fn wait_speaker(state: &RoutingServiceState, needle: &str) -> serde_json::Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let map = speaker_map(state).await;
        if let Some(s) = speaker_entry(&map, needle) {
            return s;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for speaker containing '{needle}': {map}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Wait until the speaker carrying `needle` reports a transport gap
/// with `reason`, or time out.
async fn wait_gap_reason(
    state: &RoutingServiceState,
    needle: &str,
    reason: &str,
) -> serde_json::Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let map = speaker_map(state).await;
        if let Some(s) = speaker_entry(&map, needle) {
            if s["transport_gap"]["reason"].as_str() == Some(reason) {
                return s;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for gap reason '{reason}' on '{needle}': {map}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ── TLS handshake availability ───────────────────────────────────────────────

/// A stalled TLS handshake must not serialize legitimate exporters:
/// the accept loop keeps accepting while the stalled client holds its
/// handshake slot.
#[tokio::test]
async fn test_stalled_handshake_does_not_block_valid_client() {
    let lab = new_lab(|s, c| {
        provision_binding(s, "dev-frr-a", "exp-frr-a", "frr-a", &c.a.fingerprint);
    })
    .await;

    // Client A: TCP connect, then silence — never completes a handshake.
    let _stalled = TcpStream::connect(lab.bmp_addr).await.unwrap();

    // Client B: valid credential must authenticate and stream BMP
    // without waiting for A's (1s) handshake timeout.
    let start = std::time::Instant::now();
    let _tls = connect_exporter(
        &lab,
        Some(&lab.cert_a),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 50),
        65000,
        [203, 0, 113],
    )
    .await
    .expect("valid client must connect despite stalled handshake");
    assert!(
        start.elapsed() < Duration::from_millis(900),
        "valid client waited on stalled handshake: {:?}",
        start.elapsed()
    );
    let spk = wait_speaker(&lab.state, "vexp:").await;
    assert_eq!(spk["device_id"], "dev-frr-a");
}

/// Filling every handshake slot must bound (not queue) excess
/// connections; once stalled handshakes time out, capacity recovers.
#[tokio::test]
async fn test_handshake_limit_bounds_and_recovers() {
    let lab = new_lab(|_s, _c| {}).await;

    // Saturate all MAX_HANDSHAKES slots with stalled clients.
    let mut stalled = Vec::new();
    for _ in 0..MAX_HANDSHAKES {
        stalled.push(TcpStream::connect(lab.bmp_addr).await.unwrap());
    }
    // Give the listener a moment to spawn the handshake tasks.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // A further VALID client is rejected while saturated: TCP accepts
    // but the TLS handshake is dropped (no permit).
    let rejected = tls_connect(&lab, Some(&lab.cert_a)).await;
    assert!(
        rejected.is_err(),
        "valid client must be bounded while handshake slots are saturated"
    );

    // After the handshake timeout fires, permits are released and a
    // valid client connects again.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let tls = tls_connect(&lab, Some(&lab.cert_a))
        .await
        .expect("handshake capacity must recover after timeouts");
    drop(tls);
    drop(stalled);
}

/// Incomplete handshakes must not survive service shutdown: the
/// listener returns and pending handshake tasks are cancelled.
#[tokio::test]
async fn test_shutdown_cancels_pending_handshakes() {
    let lab = new_lab(|_s, _c| {}).await;

    let _stalled = TcpStream::connect(lab.bmp_addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    lab.shutdown_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), lab.listener_handle)
        .await
        .expect("listener must stop after shutdown")
        .expect("listener task must not panic");
}

// ── Active-session revocation ────────────────────────────────────────────────

/// Revoking the trusted binding must terminate the live verified
/// session within the revalidation bound — without a restart — while
/// leaving last-known state under an explicit transport gap.
#[tokio::test]
async fn test_active_binding_revocation_terminates_session() {
    let lab = new_lab(|s, c| {
        provision_binding(s, "dev-frr-a", "exp-frr-a", "frr-a", &c.a.fingerprint);
    })
    .await;

    let _tls = connect_exporter(
        &lab,
        Some(&lab.cert_a),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 50),
        65000,
        [203, 0, 113],
    )
    .await
    .unwrap();
    let spk = wait_speaker(&lab.state, "vexp:").await;
    assert_eq!(spk["device_id"], "dev-frr-a");

    // Revoke through the same provisioning API an operator uses.
    lab.store()
        .revoke_exporter_binding("bind-dev-frr-a", "bn021f-hardening")
        .unwrap();

    // The session must be terminated with an explicit revocation gap.
    let gapped = wait_gap_reason(&lab.state, "vexp:", "bmp_authorization_revoked").await;
    assert_eq!(gapped["device_id"], "dev-frr-a");
    // Last-known routes are preserved (BN-005): no fabricated withdrawal.
    assert!(
        gapped["total_routes"].as_u64().unwrap_or(0) >= 1,
        "revocation must preserve last-known routes: {gapped}"
    );

    // The revoked credential cannot reacquire authority on reconnect.
    let _tls2 = connect_exporter(
        &lab,
        Some(&lab.cert_a),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 50),
        65000,
        [203, 0, 114],
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    let map = speaker_map(&lab.state).await;
    // No NEW live vexp session: the speaker stays gapped and no second
    // resolved session appears.
    let live_vexp = map["speakers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| {
            s["exporter_instance"]
                .as_str()
                .unwrap_or("")
                .starts_with("vexp:")
                && s["completeness_state"].as_str() != Some("gap")
        })
        .count();
    assert_eq!(live_vexp, 0, "revoked credential regained authority: {map}");
}

/// Two sessions sharing one verified principal must BOTH lose
/// authority on revocation; an unrelated exporter must be unaffected.
#[tokio::test]
async fn test_revocation_invalidates_all_sessions_of_principal() {
    let lab = new_lab(|s, c| {
        provision_binding(s, "dev-frr-a", "exp-frr-a", "frr-a", &c.a.fingerprint);
        provision_binding(s, "dev-frr-b", "exp-frr-b", "frr-b", &c.b.fingerprint);
    })
    .await;

    // Two legitimate sessions on exporter A (same credential -> shared
    // vexp scope) plus one on exporter B.
    let _a1 = connect_exporter(
        &lab,
        Some(&lab.cert_a),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 50),
        65000,
        [203, 0, 113],
    )
    .await
    .unwrap();
    let _a2 = connect_exporter(
        &lab,
        Some(&lab.cert_a),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 50),
        65000,
        [203, 0, 113],
    )
    .await
    .unwrap();
    let _b = connect_exporter(
        &lab,
        Some(&lab.cert_b),
        "frr-b",
        Ipv4Addr::new(10, 240, 0, 51),
        65002,
        [198, 51, 100],
    )
    .await
    .unwrap();

    wait_speaker(&lab.state, "vexp:exp-frr-a").await;
    wait_speaker(&lab.state, "vexp:exp-frr-b").await;

    lab.store()
        .revoke_exporter_binding("bind-dev-frr-a", "bn021f-hardening")
        .unwrap();

    // A's shared scope gaps with the revocation reason once BOTH
    // sessions have been terminated (last connection records it).
    let gapped = wait_gap_reason(&lab.state, "vexp:exp-frr-a", "bmp_authorization_revoked").await;
    assert_eq!(gapped["device_id"], "dev-frr-a");

    // B is untouched: still resolved, no gap.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let map = speaker_map(&lab.state).await;
    let b = speaker_entry(&map, "vexp:exp-frr-b").expect("B speaker missing");
    assert_eq!(b["device_id"], "dev-frr-b");
    assert!(
        b["transport_gap"].is_null(),
        "unaffected exporter must not gap: {b}"
    );
}

/// Re-pinning a binding to a different device must revoke the old
/// session: the canonical triple no longer matches.
#[tokio::test]
async fn test_binding_repin_revokes_session() {
    let lab = new_lab(|s, c| {
        provision_binding(s, "dev-frr-a", "exp-frr-a", "frr-a", &c.a.fingerprint);
    })
    .await;

    let _tls = connect_exporter(
        &lab,
        Some(&lab.cert_a),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 50),
        65000,
        [203, 0, 113],
    )
    .await
    .unwrap();
    wait_speaker(&lab.state, "vexp:").await;

    // Rebind the SAME exporter identity to a different canonical
    // device — the old session's (binding, exporter, device) triple
    // no longer resolves.
    let store = lab.store();
    let mut binding = store
        .get_exporter_binding("bind-dev-frr-a")
        .unwrap()
        .unwrap();
    store
        .upsert_canonical_device(
            &CanonicalDevice {
                device_id: "dev-frr-a2".into(),
                address: "10.240.0.200".into(),
                site: Some("lab".into()),
                hostname: Some("frr-a".into()),
                created_at_ns: 0,
                updated_at_ns: 0,
                created_by: String::new(),
                updated_by: String::new(),
            },
            "bn021f-hardening",
        )
        .unwrap();
    binding.device_id = "dev-frr-a2".into();
    store
        .upsert_exporter_binding(&binding, "bn021f-hardening")
        .unwrap();
    drop(store);

    wait_gap_reason(&lab.state, "vexp:", "bmp_authorization_revoked").await;
}

// ── Certificate rotation ─────────────────────────────────────────────────────

/// Planned rotation: re-pin the binding's credential to the
/// replacement certificate. The old session is revoked; the new
/// certificate reconnects to the SAME canonical exporter/device and
/// `vexp:*` scope — canonical identity is preserved.
#[tokio::test]
async fn test_certificate_rotation_preserves_canonical_identity() {
    let lab = new_lab(|s, c| {
        provision_binding(s, "dev-frr-a", "exp-frr-a", "frr-a", &c.a.fingerprint);
    })
    .await;

    let tls_old = connect_exporter(
        &lab,
        Some(&lab.cert_a),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 50),
        65000,
        [203, 0, 113],
    )
    .await
    .unwrap();
    wait_speaker(&lab.state, "vexp:exp-frr-a").await;

    // Rotate: same binding, new credential fingerprint (cert_b is
    // unbound otherwise in this fixture).
    let store = lab.store();
    let mut binding = store
        .get_exporter_binding("bind-dev-frr-a")
        .unwrap()
        .unwrap();
    binding.match_ingress_credential = Some(lab.cert_b.fingerprint.clone());
    store
        .upsert_exporter_binding(&binding, "bn021f-hardening")
        .unwrap();
    drop(store);

    // Old session loses authority.
    wait_gap_reason(&lab.state, "vexp:exp-frr-a", "bmp_authorization_revoked").await;
    drop(tls_old);

    // Replacement credential connects: same canonical exporter, same
    // device, same vexp scope — new session epoch.
    let _tls_new = connect_exporter(
        &lab,
        Some(&lab.cert_b),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 50),
        65000,
        [203, 0, 115],
    )
    .await
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let map = speaker_map(&lab.state).await;
        if let Some(s) = speaker_entry(&map, "vexp:exp-frr-a") {
            if s["completeness_state"].as_str() != Some("gap") {
                assert_eq!(s["device_id"], "dev-frr-a");
                assert_eq!(s["exporter_id"], "exp-frr-a");
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "rotated credential did not restore verified session: {map}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ── Production deployment policy ─────────────────────────────────────────────

fn deployment_base(dir: &std::path::Path) -> RoutingServiceConfig {
    RoutingServiceConfig {
        bmp_listen_addr: "0.0.0.0:11019".into(),
        http_listen_addr: "0.0.0.0:8081".into(),
        bmp_journal_path: Some(dir.join("bmp.jsonl")),
        event_journal_path: Some(dir.join("events.jsonl")),
        max_frame_bytes: rbmp_core::bmp::parser::DEFAULT_MAX_FRAME,
        internal_service_token: Some("tok".into()),
        identity_db_path: None,
        identity_scope: None,
        bmp_identity_mode: BmpIdentityMode::Verified,
        bmp_mtls: None,
    }
}

fn strict() -> DeploymentSecurityOverrides {
    DeploymentSecurityOverrides::default()
}

fn dev_all() -> DeploymentSecurityOverrides {
    DeploymentSecurityOverrides {
        allow_insecure_no_token: true,
        allow_ephemeral: true,
        allow_insecure_dev: true,
        allow_bmp_plaintext: true,
        allow_claim_compatible: true,
    }
}

#[test]
fn test_deployment_policy_matrix() {
    let dir = tempfile::tempdir().unwrap();

    // 1. Non-loopback BMP without mTLS: refused in production.
    let cfg = deployment_base(dir.path());
    let err = cfg.validate_deployment_with(&strict()).unwrap_err();
    assert!(err.contains("mutual-TLS"), "unexpected: {err}");

    // 2. Same config with explicit plaintext opt-in: accepted
    //    (all connections remain non-authoritative under Verified).
    let mut o = strict();
    o.allow_bmp_plaintext = true;
    assert!(cfg.validate_deployment_with(&o).is_ok());

    // 3. Loopback plaintext BMP: dev-acceptable without override.
    let mut loop_cfg = deployment_base(dir.path());
    loop_cfg.bmp_listen_addr = "127.0.0.1:11019".into();
    assert!(loop_cfg.validate_deployment_with(&strict()).is_ok());

    // 4. ClaimCompatible refused without explicit unsafe opt-in.
    let mut cc = deployment_base(dir.path());
    cc.bmp_identity_mode = BmpIdentityMode::ClaimCompatible;
    cc.bmp_listen_addr = "127.0.0.1:11019".into();
    let err = cc.validate_deployment_with(&strict()).unwrap_err();
    assert!(err.contains("claim_compat"), "unexpected: {err}");
    let mut o = strict();
    o.allow_claim_compatible = true;
    assert!(cc.validate_deployment_with(&o).is_ok());

    // 5. Complete mTLS config validates (real cert material).
    let pki = make_pki();
    let server = issue_leaf(&pki, "srv", true);
    let cert_path = dir.path().join("s.crt");
    let key_path = dir.path().join("s.key");
    let ca_path = dir.path().join("ca.crt");
    std::fs::write(&cert_path, &server.cert_pem).unwrap();
    std::fs::write(&key_path, &server.key_pem).unwrap();
    std::fs::write(&ca_path, &pki.ca_cert_pem).unwrap();
    let mut mtls_cfg = deployment_base(dir.path());
    mtls_cfg.bmp_mtls = Some(BmpMtlsConfig {
        cert_path: cert_path.clone(),
        key_path: key_path.clone(),
        client_ca_path: ca_path.clone(),
    });
    assert!(mtls_cfg.validate_deployment_with(&strict()).is_ok());

    // 6. Missing/unreadable TLS files fail fast at validation.
    let mut bad = deployment_base(dir.path());
    bad.bmp_mtls = Some(BmpMtlsConfig {
        cert_path: dir.path().join("nonexistent.crt"),
        key_path: key_path.clone(),
        client_ca_path: ca_path.clone(),
    });
    assert!(bad.validate_deployment_with(&strict()).is_err());

    // 7. Dev umbrella enables every override.
    let mut cc2 = deployment_base(dir.path());
    cc2.bmp_identity_mode = BmpIdentityMode::ClaimCompatible;
    assert!(cc2.validate_deployment_with(&dev_all()).is_ok());
}

/// A deserialized VerifiedIngressPrincipal must never authorize:
/// journal/API-restored principals are evidence, not attestation.
#[test]
fn test_unattested_principal_cannot_resolve() {
    use bonsai::telemetry_identity::{
        IdentityResolver, TelemetrySourceIdentity, VerifiedIngressPrincipal,
    };

    // Simulate a principal round-tripped through JSON (as a journal or
    // API payload would carry it).
    let json = serde_json::json!({
        "method": "mutual_tls",
        "credential_fingerprint": "sha256:deadbeef",
    });
    let principal: VerifiedIngressPrincipal = serde_json::from_value(json).unwrap();
    assert!(!principal.is_transport_attested());

    let observed = TelemetrySourceIdentity::observed("127.0.0.1")
        .with_protocol("bmp")
        .with_verified_ingress(principal.clone());
    let resolution = IdentityResolver::resolve_verified_ingress(&observed, &[], &principal);
    assert_ne!(
        resolution.status,
        bonsai::telemetry_identity::IdentityStatus::Resolved,
        "deserialized principal must never resolve authoritatively"
    );
}
