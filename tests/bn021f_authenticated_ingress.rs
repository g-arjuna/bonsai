//! BN-021f: authenticated BMP ingress (mutual TLS) and authoritative
//! exporter identity.
//!
//! Covers the verified-ingress contract end to end over real TCP+TLS:
//! legitimate authenticated exporter, sequential/concurrent
//! impersonation rejection, shared-NAT separation, conflicting claims,
//! unauthenticated transport, revoked binding, reconnect, multi-
//! connection sharing, and restart/replay provenance.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use bonsai::routing_service::{
    BmpIdentityMode, BmpMtlsConfig, RoutingServiceConfig, RoutingServiceState,
    build_bmp_mtls_acceptor, build_speakers_json, run_bmp_listener,
};
use bonsai::sqlite_store::{CONFIG_DB_FILE_NAME, SqliteStore};
use bonsai::telemetry_identity::{
    BindingProvenance, BindingStatus, CanonicalDevice, TrustedBinding,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

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
}

impl Lab {
    fn db_path(&self) -> std::path::PathBuf {
        self.dir.path().join(CONFIG_DB_FILE_NAME)
    }
    fn store(&self) -> SqliteStore {
        SqliteStore::open(self.dir.path()).unwrap()
    }
}

/// Certificate bundle handed to the provisioning closure.
struct Certs {
    a: LeafCert,
    b: LeafCert,
}

/// Build the lab. `provision` runs against the identity DB BEFORE the
/// routing state opens it read-only — mirroring production, where the
/// management service provisions bindings before `bonsai-routing`
/// starts.
async fn new_lab(provision: impl FnOnce(&SqliteStore, &Certs)) -> Lab {
    // rustls cannot auto-pick a CryptoProvider when multiple providers
    // are enabled transitively; pin ring explicitly (same as main.rs).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_test_writer()
        .try_init();
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

    // Provision trusted bindings before the service opens the DB
    // read-only.
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
        internal_service_token: Some("bn021f-test-token".into()),
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
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let st = Arc::clone(&state);
    tokio::spawn(async move {
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
    }
}

/// Provision one canonical device + credential-pinned binding.
fn provision_binding(
    store: &SqliteStore,
    device_id: &str,
    exporter_id: &str,
    sys_name: &str,
    credential_fp: &str,
) {
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
            "bn021f-test",
        )
        .unwrap();
    store
        .upsert_exporter_binding(
            &TrustedBinding {
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
            },
            "bn021f-test",
        )
        .unwrap();
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

/// Connect with `cert`, send Initiation(sys_name) + PeerUp + one
/// RouteMonitoring announcement, keep the session open.
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

async fn wait_absent(state: &RoutingServiceState, needle: &str) {
    // Give the listener a moment, then assert the speaker never appears.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let map = speaker_map(state).await;
    assert!(
        speaker_entry(&map, needle).is_none(),
        "unexpected speaker containing '{needle}': {map}"
    );
}

// ── Tests A–I ────────────────────────────────────────────────────────────────

/// A — legitimate authenticated exporter resolves canonical identity.
#[tokio::test]
async fn test_a_legitimate_authenticated_exporter() {
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
    .expect("authenticated BMP session");

    let spk = wait_speaker(&lab.state, "vexp:").await;
    assert_eq!(spk["device_id"], "dev-frr-a");
    assert_eq!(spk["exporter_id"], "exp-frr-a");
    assert_eq!(spk["identity_status"], "resolved");
    assert_eq!(spk["total_routes"].as_u64().unwrap(), 1);
}

/// B — sequential impersonation: impostor copies sysName with a
/// DIFFERENT credential after the legit exporter disconnects. Must not
/// obtain canonical scope/device or contaminate last-known state.
#[tokio::test]
async fn test_b_sequential_impersonation_rejected() {
    let lab = new_lab(|s, c| {
        provision_binding(s, "dev-frr-a", "exp-frr-a", "frr-a", &c.a.fingerprint);
    })
    .await;
    provision_binding(
        &lab.store(),
        "dev-frr-b",
        "exp-frr-b",
        "frr-b",
        &lab.cert_b.fingerprint,
    );

    // Legit A connects, advertises, disconnects.
    let tls_a = connect_exporter(
        &lab,
        Some(&lab.cert_a),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 50),
        65000,
        [203, 0, 113],
    )
    .await
    .unwrap();
    let spk_a = wait_speaker(&lab.state, "vexp:").await;
    assert_eq!(spk_a["device_id"], "dev-frr-a");
    drop(tls_a); // clean TLS close -> bmp_tcp_eof gap, last-known retained
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Impostor B presents A's sysName with B's own credential.
    let _tls_b = connect_exporter(
        &lab,
        Some(&lab.cert_b),
        "frr-a", // copied claim
        Ipv4Addr::new(10, 240, 0, 51),
        65003,
        [192, 0, 2],
    )
    .await
    .unwrap();

    // B must be conn-isolated with NO device attribution.
    let spk_b = wait_speaker(&lab.state, "conn:").await;
    assert!(
        spk_b["device_id"].is_null(),
        "impostor must not get device_id"
    );
    assert_eq!(spk_b["identity_status"], "provisional");
    assert_ne!(
        spk_b["exporter_instance"].as_str().unwrap(),
        spk_a["exporter_instance"].as_str().unwrap()
    );

    // A's vexp speaker must be untouched: still dev-frr-a, still its own
    // routes only (impostor prefix must not appear under vexp).
    let map = speaker_map(&lab.state).await;
    let spk_a2 = speaker_entry(&map, "vexp:").unwrap();
    assert_eq!(spk_a2["device_id"], "dev-frr-a");
    assert_eq!(spk_a2["total_routes"], spk_a["total_routes"]);

    // No second vexp speaker may exist.
    let vexp_count = map["speakers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| {
            s["exporter_instance"]
                .as_str()
                .unwrap_or("")
                .starts_with("vexp:")
        })
        .count();
    assert_eq!(vexp_count, 1, "impostor must not create/join a vexp scope");
}

/// C — two exporters sharing one NAT source IP with distinct valid
/// credentials stay fully separated.
#[tokio::test]
async fn test_c_shared_nat_distinct_credentials_isolated() {
    let lab = new_lab(|s, c| {
        provision_binding(s, "dev-frr-a", "exp-frr-a", "frr-a", &c.a.fingerprint);
    })
    .await;
    provision_binding(
        &lab.store(),
        "dev-frr-b",
        "exp-frr-b",
        "frr-b",
        &lab.cert_b.fingerprint,
    );

    let _a = connect_exporter(
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
        Ipv4Addr::new(10, 240, 0, 50),
        65000,
        [198, 18, 0],
    )
    .await
    .unwrap();

    let spk_a = wait_speaker(&lab.state, "vexp:exp-frr-a").await;
    let spk_b = wait_speaker(&lab.state, "vexp:exp-frr-b").await;
    assert_eq!(spk_a["device_id"], "dev-frr-a");
    assert_eq!(spk_b["device_id"], "dev-frr-b");
    assert_ne!(spk_a["speaker_key"], spk_b["speaker_key"]);
}

/// D — authenticated exporter B presents A's sysName: conflicting
/// evidence must fail closed, never claim A's identity.
#[tokio::test]
async fn test_d_conflicting_claim_fails_closed() {
    let lab = new_lab(|s, c| {
        provision_binding(s, "dev-frr-a", "exp-frr-a", "frr-a", &c.a.fingerprint);
    })
    .await;
    provision_binding(
        &lab.store(),
        "dev-frr-b",
        "exp-frr-b",
        "frr-b",
        &lab.cert_b.fingerprint,
    );

    // B's credential is valid, but it claims "frr-a" while its binding
    // pins reported identity "frr-b" -> contradiction -> conn:*.
    let _tls = connect_exporter(
        &lab,
        Some(&lab.cert_b),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 50),
        65000,
        [198, 18, 0],
    )
    .await
    .unwrap();

    let spk = wait_speaker(&lab.state, "conn:").await;
    assert!(spk["device_id"].is_null());
    assert_eq!(spk["identity_status"], "provisional");
    wait_absent(&lab.state, "vexp:").await;
}

/// E — unauthenticated transport: missing client cert fails the
/// handshake; no state is created.
#[tokio::test]
async fn test_e_unauthenticated_transport_rejected() {
    let lab = new_lab(|s, c| {
        provision_binding(s, "dev-frr-a", "exp-frr-a", "frr-a", &c.a.fingerprint);
    })
    .await;

    // No client certificate -> the server rejects the handshake. In
    // TLS 1.3 the client's connect() can complete before the server's
    // certificate-request rejection arrives, so the failure surfaces on
    // first I/O: the write/read must fail and no state may appear.
    let mut no_cert = tls_connect(&lab, None).await.expect("tcp+tls connect");
    let write_result = no_cert.write_all(&initiation_frame("frr-a")).await;
    let read_result = async {
        let mut buf = [0u8; 1];
        no_cert.read(&mut buf).await
    }
    .await;
    assert!(
        write_result.is_err() || matches!(read_result, Ok(0) | Err(_)),
        "server must reject a client without a certificate"
    );
    drop(no_cert);

    // Client cert signed by an UNKNOWN CA -> handshake fails.
    let rogue_pki = make_pki();
    let rogue_cert = issue_leaf(&rogue_pki, "rogue", false);
    // Client trusts lab CA for the server but presents rogue cert —
    // server-side verification rejects it.
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
    .with_root_certificates(roots)
    .with_client_auth_cert(
        vec![rustls::pki_types::CertificateDer::from(
            rogue_cert.cert_der.clone(),
        )],
        rustls::pki_types::PrivateKeyDer::try_from(
            rustls_pemfile::private_key(&mut rogue_cert.key_pem.as_bytes())
                .unwrap()
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let connector = TlsConnector::from(Arc::new(builder));
    let tcp = TcpStream::connect(lab.bmp_addr).await.unwrap();
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    // Same TLS 1.3 caveat: rejection may surface on first I/O.
    let mut rogue = match connector.connect(name, tcp).await {
        Ok(s) => s,
        Err(_) => {
            tokio::time::sleep(Duration::from_millis(800)).await;
            let map = speaker_map(&lab.state).await;
            assert!(map["speakers"].as_array().unwrap().is_empty());
            return;
        }
    };
    let _ = rogue.write_all(&initiation_frame("frr-a")).await;
    let mut buf = [0u8; 1];
    let _ = rogue.read(&mut buf).await;

    // Plaintext BMP bytes on the TLS port -> handshake failure, no state.
    let mut plain = TcpStream::connect(lab.bmp_addr).await.unwrap();
    plain.write_all(&initiation_frame("frr-a")).await.unwrap();
    drop(plain);

    tokio::time::sleep(Duration::from_millis(800)).await;
    let map = speaker_map(&lab.state).await;
    assert!(
        map["speakers"].as_array().unwrap().is_empty(),
        "unauthenticated transports must create no speaker state: {map}"
    );
}

/// F — valid credential whose binding was revoked gets no authority.
#[tokio::test]
async fn test_f_revoked_binding_no_authority() {
    let lab = new_lab(|s, c| {
        provision_binding(s, "dev-frr-a", "exp-frr-a", "frr-a", &c.a.fingerprint);
    })
    .await;
    lab.store()
        .revoke_exporter_binding("bind-dev-frr-a", "bn021f-test")
        .unwrap();

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

    let spk = wait_speaker(&lab.state, "conn:").await;
    assert!(spk["device_id"].is_null());
    wait_absent(&lab.state, "vexp:").await;
}

/// G — legitimate reconnect with the same credential: same canonical
/// identity, new epoch, no inherited impostor state.
#[tokio::test]
async fn test_g_legitimate_reconnect_stable_identity() {
    let lab = new_lab(|s, c| {
        provision_binding(s, "dev-frr-a", "exp-frr-a", "frr-a", &c.a.fingerprint);
    })
    .await;
    provision_binding(
        &lab.store(),
        "dev-frr-b",
        "exp-frr-b",
        "frr-b",
        &lab.cert_b.fingerprint,
    );

    let tls1 = connect_exporter(
        &lab,
        Some(&lab.cert_a),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 50),
        65000,
        [203, 0, 113],
    )
    .await
    .unwrap();
    let spk1 = wait_speaker(&lab.state, "vexp:").await;
    let epoch1 = spk1["session_epoch"].as_u64().unwrap();
    drop(tls1);
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Impostor noise in between (conn-isolated).
    let _imp = connect_exporter(
        &lab,
        Some(&lab.cert_b),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 51),
        65003,
        [192, 0, 2],
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let tls2 = connect_exporter(
        &lab,
        Some(&lab.cert_a),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 50),
        65000,
        [203, 0, 114],
    )
    .await
    .unwrap();
    // Wait until the NEW session epoch is observable (the speaker entry
    // may briefly still reflect the retired session).
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let spk2 = loop {
        let map = speaker_map(&lab.state).await;
        let spk = speaker_entry(&map, "vexp:").expect("vexp speaker");
        if spk["session_epoch"].as_u64().unwrap() > epoch1 {
            break spk;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "new session epoch never appeared: {map}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(spk2["device_id"], "dev-frr-a");
    assert_eq!(spk2["exporter_instance"], spk1["exporter_instance"]);
    let _ = tls2;
}

/// H — two simultaneous connections with the SAME verified principal
/// legitimately share the exporter scope (FRR global+VRF pattern).
#[tokio::test]
async fn test_h_same_principal_multi_connection_shares_scope() {
    let lab = new_lab(|s, c| {
        provision_binding(s, "dev-frr-a", "exp-frr-a", "frr-a", &c.a.fingerprint);
    })
    .await;

    let _c1 = connect_exporter(
        &lab,
        Some(&lab.cert_a),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 50),
        65000,
        [203, 0, 113],
    )
    .await
    .unwrap();
    let _c2 = connect_exporter(
        &lab,
        Some(&lab.cert_a),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 51),
        65001,
        [198, 51, 100],
    )
    .await
    .unwrap();

    let map = speaker_map(&lab.state).await;
    let vexp: Vec<_> = map["speakers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| {
            s["exporter_instance"]
                .as_str()
                .unwrap_or("")
                .starts_with("vexp:")
        })
        .cloned()
        .collect();
    assert_eq!(
        vexp.len(),
        1,
        "same verified principal shares one vexp scope: {map}"
    );
    assert_eq!(vexp[0]["device_id"], "dev-frr-a");
    // Both connections' routes land in the shared speaker (one prefix
    // each, distinct peers) — poll until the second session's update
    // has been ingested.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let map = speaker_map(&lab.state).await;
        let spk = speaker_entry(&map, "vexp:").unwrap();
        if spk["total_routes"].as_u64().unwrap() >= 2 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "shared-scope routes never converged: {map}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// I — restart + replay: verified provenance survives, unverified
/// conn state does not contaminate, no fabricated withdrawals.
#[tokio::test]
async fn test_i_restart_replay_preserves_provenance() {
    let lab = new_lab(|s, c| {
        provision_binding(s, "dev-frr-a", "exp-frr-a", "frr-a", &c.a.fingerprint);
    })
    .await;
    provision_binding(
        &lab.store(),
        "dev-frr-b",
        "exp-frr-b",
        "frr-b",
        &lab.cert_b.fingerprint,
    );

    // Verified A + unverified impostor (cert_b claiming frr-a -> conn).
    let tls_a = connect_exporter(
        &lab,
        Some(&lab.cert_a),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 50),
        65000,
        [203, 0, 113],
    )
    .await
    .unwrap();
    let imp = connect_exporter(
        &lab,
        Some(&lab.cert_b),
        "frr-a",
        Ipv4Addr::new(10, 240, 0, 51),
        65003,
        [192, 0, 2],
    )
    .await
    .unwrap();
    wait_speaker(&lab.state, "vexp:").await;
    wait_speaker(&lab.state, "conn:").await;
    drop(tls_a);
    drop(imp); // clean TLS close -> durable disconnect for BOTH sessions
    tokio::time::sleep(Duration::from_millis(700)).await;

    // "SIGKILL": drop the whole service state without graceful shutdown,
    // then replay journals into a fresh state on the same paths.
    let bmp_path = lab.dir.path().join("bmp.jsonl");
    let events_path = lab.dir.path().join("events.jsonl");
    let db_path = lab.db_path();
    drop(lab.state);

    let replayed = RoutingServiceState::new(RoutingServiceConfig {
        bmp_listen_addr: "127.0.0.1:0".into(),
        http_listen_addr: "127.0.0.1:0".into(),
        bmp_journal_path: Some(bmp_path),
        event_journal_path: Some(events_path),
        max_frame_bytes: rbmp_core::bmp::parser::DEFAULT_MAX_FRAME,
        internal_service_token: Some("bn021f-test-token".into()),
        identity_db_path: Some(db_path),
        identity_scope: None,
        bmp_identity_mode: BmpIdentityMode::Verified,
        bmp_mtls: None,
    })
    .await
    .unwrap();

    let map = speaker_map(&replayed).await;
    // vexp speaker replays with verified provenance intact.
    let vexp = speaker_entry(&map, "vexp:").expect("vexp speaker must replay");
    assert_eq!(vexp["device_id"], "dev-frr-a");
    assert_eq!(vexp["exporter_id"], "exp-frr-a");
    // The conn:* impostor had a clean TLS close -> disconnect record ->
    // retired on replay (conn scopes never persist).
    assert!(
        speaker_entry(&map, "conn:").is_none(),
        "conn:* impostor must not survive replay: {map}"
    );
    // And no exp:* (claim-era) speaker was synthesized from claims.
    // NB: "vexp:" legitimately contains the substring "exp:", so match
    // the namespace prefix exactly.
    let legacy_exp = map["speakers"].as_array().unwrap().iter().any(|s| {
        s["exporter_instance"]
            .as_str()
            .unwrap_or("")
            .starts_with("exp:")
    });
    assert!(!legacy_exp, "no claim-era exp:* speaker may appear: {map}");
}
