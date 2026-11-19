//! M5.1 secure gNMI connectivity — runtime verification against real
//! local gNMI servers (plaintext, TLS, mutual-TLS) that record RPCs.
//! Asserts the resolved ConnSecurity contract end-to-end: correct
//! handshake material, fail-closed behaviour on wrong/missing trust,
//! explicit plaintext lab mode, persistence and proto propagation.
//! No vendor interoperability is claimed.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bonsai::config::{AcquisitionMode, GnmiSecurityMode, SelectedSubscriptionPath, TargetConfig};
use bonsai::event_bus::InProcessBus;
use bonsai::gnmi_security::{ConnSecurity, EffectiveSecurity};
use bonsai::proto::gnmi::g_nmi_server::{GNmi, GNmiServer};
use bonsai::proto::gnmi::{
    CapabilityRequest, CapabilityResponse, Encoding, ModelData, SubscribeRequest, SubscribeResponse,
};
use bonsai::subscriber::GnmiSubscriber;
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Certificate, Identity, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming, transport::Server};

// ── recording mock gNMI server (same shape as the M4 harness) ────────────────

#[derive(Clone)]
struct MockGnmi {
    caps: CapabilityResponse,
    capability_calls: Arc<AtomicUsize>,
    subscribe_calls: Arc<AtomicUsize>,
}

#[tonic::async_trait]
impl GNmi for MockGnmi {
    async fn capabilities(
        &self,
        _request: Request<CapabilityRequest>,
    ) -> Result<Response<CapabilityResponse>, Status> {
        self.capability_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Response::new(self.caps.clone()))
    }

    type SubscribeStream = ReceiverStream<Result<SubscribeResponse, Status>>;

    async fn subscribe(
        &self,
        request: Request<Streaming<SubscribeRequest>>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        self.subscribe_calls.fetch_add(1, Ordering::SeqCst);
        let mut inner = request.into_inner();
        tokio::spawn(async move { while inner.message().await.ok().flatten().is_some() {} });
        let (tx, rx) = mpsc::channel::<Result<SubscribeResponse, Status>>(8);
        std::mem::forget(tx);
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn get(
        &self,
        _request: Request<bonsai::proto::gnmi::GetRequest>,
    ) -> Result<Response<bonsai::proto::gnmi::GetResponse>, Status> {
        Err(Status::unimplemented("get"))
    }

    async fn set(
        &self,
        _request: Request<bonsai::proto::gnmi::SetRequest>,
    ) -> Result<Response<bonsai::proto::gnmi::SetResponse>, Status> {
        Err(Status::unimplemented("set"))
    }
}

struct Lab {
    addr: String,
    capability_calls: Arc<AtomicUsize>,
    subscribe_calls: Arc<AtomicUsize>,
    _shutdown: watch::Sender<bool>,
    _handle: tokio::task::JoinHandle<()>,
}

async fn start_plain_lab() -> Lab {
    start_server(None).await
}

async fn start_server(tls: Option<ServerTlsConfig>) -> Lab {
    let capability_calls = Arc::new(AtomicUsize::new(0));
    let subscribe_calls = Arc::new(AtomicUsize::new(0));
    let svc = MockGnmi {
        caps: CapabilityResponse {
            supported_models: vec![ModelData {
                name: "openconfig-interfaces".to_string(),
                organization: "openconfig".to_string(),
                version: "1.0".to_string(),
            }],
            supported_encodings: vec![Encoding::JsonIetf as i32],
            g_nmi_version: "0.0.0".to_string(),
            ..Default::default()
        },
        capability_calls: Arc::clone(&capability_calls),
        subscribe_calls: Arc::clone(&subscribe_calls),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(async move {
        let mut builder = Server::builder();
        if let Some(tls) = tls {
            builder = builder.tls_config(tls).expect("server tls config");
        }
        builder
            .add_service(GNmiServer::new(svc))
            .serve_with_incoming_shutdown(
                tokio_stream::wrappers::TcpListenerStream::new(listener),
                async move {
                    let _ = shutdown_rx.changed().await;
                },
            )
            .await
            .ok();
    });
    Lab {
        addr,
        capability_calls,
        subscribe_calls,
        _shutdown: shutdown_tx,
        _handle: handle,
    }
}

// ── test PKI (rcgen) ─────────────────────────────────────────────────────────

struct TestPki {
    ca_pem: String,
    other_ca_pem: String,
    server_cert_pem: String,
    server_key_pem: String,
    client_cert_pem: String,
    client_key_pem: String,
}

fn make_ca() -> (rcgen::Certificate, rcgen::KeyPair, String) {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(Vec::new()).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let cert = params.self_signed(&key).unwrap();
    let pem = cert.pem();
    (cert, key, pem)
}

fn make_leaf(
    sans: Vec<String>,
    eku: rcgen::ExtendedKeyUsagePurpose,
    ca: &rcgen::Certificate,
    ca_key: &rcgen::KeyPair,
) -> (String, String) {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(sans).unwrap();
    params.is_ca = rcgen::IsCa::ExplicitNoCa;
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![eku];
    let cert = params.signed_by(&key, ca, ca_key).unwrap();
    (cert.pem(), key.serialize_pem())
}

fn test_pki() -> TestPki {
    let (ca, ca_key, ca_pem) = make_ca();
    let (_ca2, _ca2_key, other_ca_pem) = make_ca();
    let (server_cert_pem, server_key_pem) = make_leaf(
        vec!["localhost".to_string(), "127.0.0.1".to_string()],
        rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        &ca,
        &ca_key,
    );
    let (client_cert_pem, client_key_pem) = make_leaf(
        vec!["bonsai-client".to_string()],
        rcgen::ExtendedKeyUsagePurpose::ClientAuth,
        &ca,
        &ca_key,
    );
    TestPki {
        ca_pem,
        other_ca_pem,
        server_cert_pem,
        server_key_pem,
        client_cert_pem,
        client_key_pem,
    }
}

fn tls_server_config(pki: &TestPki, require_client_cert: bool) -> ServerTlsConfig {
    let mut tls = ServerTlsConfig::new().identity(Identity::from_pem(
        pki.server_cert_pem.clone().into_bytes(),
        pki.server_key_pem.clone().into_bytes(),
    ));
    if require_client_cert {
        tls = tls.client_ca_root(Certificate::from_pem(pki.ca_pem.clone().into_bytes()));
    }
    tls
}

fn conn(
    effective: EffectiveSecurity,
    ca_pem: Option<String>,
    client: Option<(&str, &str)>,
) -> ConnSecurity {
    ConnSecurity {
        effective,
        ca_cert_pem: ca_pem.map(String::into_bytes),
        tls_domain: "localhost".to_string(),
        client_cert_pem: client.map(|(c, _)| c.as_bytes().to_vec()),
        client_key_pem: client.map(|(_, k)| k.as_bytes().to_vec()),
    }
}

fn selected(path: &str) -> SelectedSubscriptionPath {
    SelectedSubscriptionPath {
        path: path.to_string(),
        origin: "openconfig".to_string(),
        provenance: String::new(),
        mode: "SAMPLE".to_string(),
        sample_interval_ns: 10_000_000_000,
        rationale: String::new(),
        optional: false,
    }
}

fn subscriber(lab: &Lab, conn_security: ConnSecurity) -> GnmiSubscriber {
    GnmiSubscriber::new(
        lab.addr.clone(),
        None,
        None,
        None,
        None,
        None,
        None,
        conn_security,
        InProcessBus::new(64),
        None,
        None,
        vec![selected(
            "interfaces/interface[name=Ethernet1/1]/state/counters",
        )],
        AcquisitionMode::Explicit,
        None,
        std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
    )
}

fn ensure_crypto_provider() {
    // Same pattern as bn021f/bn021g tests: with both rustls providers in
    // the tree the process-level provider must be pinned explicitly.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

async fn wait_for<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

// ── tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn tls_verify_connects_and_subscribes() {
    ensure_crypto_provider();
    let pki = test_pki();
    let lab = start_server(Some(tls_server_config(&pki, false))).await;
    let sub = subscriber(
        &lab,
        conn(EffectiveSecurity::TlsVerify, Some(pki.ca_pem.clone()), None),
    );
    let (_stx, mut srx) = watch::channel(false);
    let handle = tokio::spawn(async move { sub.subscribe_telemetry(&mut srx).await });

    assert!(
        wait_for(
            || lab.subscribe_calls.load(Ordering::SeqCst) > 0,
            Duration::from_secs(10)
        )
        .await,
        "TLS-verified subscriber must complete handshake and Subscribe"
    );
    assert!(lab.capability_calls.load(Ordering::SeqCst) > 0);
    _stx.send(true).unwrap();
    let _ = handle.await;
}

#[tokio::test]
async fn tls_verify_wrong_ca_fails_closed_no_downgrade() {
    ensure_crypto_provider();
    let pki = test_pki();
    let lab = start_server(Some(tls_server_config(&pki, false))).await;
    // Trust a DIFFERENT CA — verification must fail; there is no
    // plaintext fallback even though the lab also could serve http.
    let sub = subscriber(
        &lab,
        conn(
            EffectiveSecurity::TlsVerify,
            Some(pki.other_ca_pem.clone()),
            None,
        ),
    );
    let (_stx, mut srx) = watch::channel(false);
    let result = tokio::time::timeout(Duration::from_secs(15), sub.subscribe_telemetry(&mut srx))
        .await
        .expect("connect attempt must terminate, not hang");
    assert!(result.is_err(), "wrong CA must fail the connection");
    assert_eq!(lab.capability_calls.load(Ordering::SeqCst), 0);
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn mtls_client_identity_accepted_and_required() {
    ensure_crypto_provider();
    let pki = test_pki();
    let lab = start_server(Some(tls_server_config(&pki, true))).await;

    // Without client identity the server must reject the handshake.
    let sub_no_cert = subscriber(
        &lab,
        conn(EffectiveSecurity::TlsVerify, Some(pki.ca_pem.clone()), None),
    );
    let (_stx1, mut srx1) = watch::channel(false);
    let rejected = tokio::time::timeout(
        Duration::from_secs(15),
        sub_no_cert.subscribe_telemetry(&mut srx1),
    )
    .await
    .expect("handshake rejection must terminate");
    assert!(rejected.is_err(), "server requires a client certificate");

    // With the mTLS identity the same server accepts.
    let sub = subscriber(
        &lab,
        conn(
            EffectiveSecurity::MutualTls,
            Some(pki.ca_pem.clone()),
            Some((pki.client_cert_pem.as_str(), pki.client_key_pem.as_str())),
        ),
    );
    let (_stx2, mut srx2) = watch::channel(false);
    let handle = tokio::spawn(async move { sub.subscribe_telemetry(&mut srx2).await });
    assert!(
        wait_for(
            || lab.subscribe_calls.load(Ordering::SeqCst) > 0,
            Duration::from_secs(10)
        )
        .await,
        "mTLS subscriber must complete handshake and Subscribe"
    );
    _stx2.send(true).unwrap();
    let _ = handle.await;
}

#[tokio::test]
async fn explicit_plaintext_lab_mode_connects() {
    let lab = start_plain_lab().await;
    let sub = subscriber(&lab, ConnSecurity::plaintext(true));
    let (_stx, mut srx) = watch::channel(false);
    let handle = tokio::spawn(async move { sub.subscribe_telemetry(&mut srx).await });
    assert!(
        wait_for(
            || lab.subscribe_calls.load(Ordering::SeqCst) > 0,
            Duration::from_secs(10)
        )
        .await,
        "explicit plaintext lab mode must connect"
    );
    _stx.send(true).unwrap();
    let _ = handle.await;
}

#[tokio::test]
async fn resolve_conn_security_loads_mtls_material_from_fs() {
    let dir = tempfile::tempdir().unwrap();
    let pki = test_pki();
    let ca_path = dir.path().join("ca.pem");
    let cert_path = dir.path().join("client.pem");
    let key_path = dir.path().join("client.key");
    std::fs::write(&ca_path, &pki.ca_pem).unwrap();
    std::fs::write(&cert_path, &pki.client_cert_pem).unwrap();
    std::fs::write(&key_path, &pki.client_key_pem).unwrap();
    // Private-key material must be owner-only (M5.2A provisioning
    // contract) — the resolver refuses group/other-readable keys.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let mut target = TargetConfig {
        address: "127.0.0.1:443".to_string(),
        security: Some(GnmiSecurityMode::Mtls),
        ca_cert: Some(ca_path.to_string_lossy().to_string()),
        tls_domain: Some("localhost".to_string()),
        client_cert: Some(cert_path.to_string_lossy().to_string()),
        client_key: Some(key_path.to_string_lossy().to_string()),
        ..Default::default()
    };
    let resolved = bonsai::gnmi_security::resolve_conn_security(&target, None)
        .await
        .unwrap();
    assert_eq!(resolved.effective, EffectiveSecurity::MutualTls);
    assert!(resolved.client_cert_pem.is_some());
    assert!(resolved.client_key_pem.is_some());
    assert!(resolved.client_tls_config().unwrap().is_some());

    // Missing key material is a hard error — never a downgrade.
    target.client_key = Some(dir.path().join("missing.key").to_string_lossy().to_string());
    assert!(
        bonsai::gnmi_security::resolve_conn_security(&target, None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn security_and_acquisition_survive_sqlite_restart() {
    let dir = tempfile::tempdir().unwrap();
    let store = bonsai::sqlite_store::SqliteStore::open(dir.path()).unwrap();
    let device = TargetConfig {
        address: "192.0.2.10:443".to_string(),
        enabled: true,
        security: Some(GnmiSecurityMode::Mtls),
        ca_cert: Some("ca.pem".to_string()),
        tls_domain: Some("device.example".to_string()),
        client_cert: Some("client.pem".to_string()),
        client_key: Some("client.key".to_string()),
        acquisition: AcquisitionMode::Explicit,
        ..Default::default()
    };
    store.upsert_device(&device, "test", "test").unwrap();
    drop(store);

    let store = bonsai::sqlite_store::SqliteStore::open(dir.path()).unwrap();
    let loaded = store.get_device("192.0.2.10:443").unwrap().unwrap();
    assert_eq!(loaded.security, Some(GnmiSecurityMode::Mtls));
    assert_eq!(loaded.client_cert.as_deref(), Some("client.pem"));
    assert_eq!(loaded.client_key.as_deref(), Some("client.key"));
    assert_eq!(loaded.acquisition, AcquisitionMode::Explicit);
}

#[test]
fn proto_round_trip_preserves_security_contract() {
    use bonsai::api::pb::ManagedDevice;
    let target = TargetConfig {
        address: "192.0.2.20:443".to_string(),
        security: Some(GnmiSecurityMode::Mtls),
        client_cert: Some("vault:client-cert".to_string()),
        client_key: Some("vault:client-key".to_string()),
        ..Default::default()
    };
    let proto = bonsai::api::managed_device_from_target(&target);
    assert_eq!(proto.security, "mtls");
    assert_eq!(proto.client_cert, "vault:client-cert");
    let back = bonsai::api::target_from_managed_device(Some(proto)).unwrap();
    assert_eq!(back.security, Some(GnmiSecurityMode::Mtls));
    assert_eq!(back.client_cert.as_deref(), Some("vault:client-cert"));

    // Unknown security intent fails closed on the collector side.
    let bad = ManagedDevice {
        address: "192.0.2.21".to_string(),
        security: "carrier-pigeon".to_string(),
        ..Default::default()
    };
    assert!(bonsai::api::target_from_managed_device(Some(bad)).is_err());
}

// ── M5.2A focused negatives ─────────────────────────────────────────────────

/// TLS verify with a wrong server name must fail the handshake — the
/// cert is valid and CA-trusted, only the identity is wrong.
#[tokio::test]
async fn tls_verify_wrong_server_name_fails_closed() {
    ensure_crypto_provider();
    let pki = test_pki();
    let lab = start_server(Some(tls_server_config(&pki, false))).await;
    let mut security = conn(EffectiveSecurity::TlsVerify, Some(pki.ca_pem.clone()), None);
    security.tls_domain = "wrong.example".to_string();
    let sub = subscriber(&lab, security);
    let (_stx, mut srx) = watch::channel(false);
    let result = tokio::time::timeout(Duration::from_secs(15), sub.subscribe_telemetry(&mut srx))
        .await
        .expect("connect attempt must terminate, not hang");
    assert!(result.is_err(), "wrong server name must fail the handshake");
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 0);
}

/// An expired server certificate must fail verification — validity
/// window is part of trust, not just chain signature.
#[tokio::test]
async fn expired_server_certificate_fails_closed() {
    ensure_crypto_provider();
    // Dedicated expired leaf signed by a dedicated CA — only the
    // validity window is wrong.
    let (ca, ca_key, ca_pem) = make_ca();
    let expired_key = rcgen::KeyPair::generate().unwrap();
    let mut eparams =
        rcgen::CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .unwrap();
    eparams.is_ca = rcgen::IsCa::ExplicitNoCa;
    eparams.key_usages = vec![
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::KeyEncipherment,
    ];
    eparams.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    eparams.not_after = rcgen::date_time_ymd(2020, 1, 1);
    let expired_cert = eparams.signed_by(&expired_key, &ca, &ca_key).unwrap();
    let mut pki2 = test_pki();
    pki2.server_cert_pem = expired_cert.pem();
    pki2.server_key_pem = expired_key.serialize_pem();
    pki2.ca_pem = ca_pem;
    let lab = start_server(Some(tls_server_config(&pki2, false))).await;
    let sub = subscriber(
        &lab,
        conn(
            EffectiveSecurity::TlsVerify,
            Some(pki2.ca_pem.clone()),
            None,
        ),
    );
    let (_stx, mut srx) = watch::channel(false);
    let result = tokio::time::timeout(Duration::from_secs(15), sub.subscribe_telemetry(&mut srx))
        .await
        .expect("connect attempt must terminate, not hang");
    assert!(
        result.is_err(),
        "expired certificate must fail verification"
    );
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 0);
}

/// A client certificate the server does not trust must be rejected —
/// mTLS is mutual, not "any certificate".
#[tokio::test]
async fn untrusted_client_certificate_rejected() {
    ensure_crypto_provider();
    let pki = test_pki();
    let lab = start_server(Some(tls_server_config(&pki, true))).await;
    // Client identity signed by an UNRELATED CA.
    let (other_ca, other_ca_key, _) = make_ca();
    let (rogue_cert, rogue_key) = make_leaf(
        vec!["rogue".to_string()],
        rcgen::ExtendedKeyUsagePurpose::ClientAuth,
        &other_ca,
        &other_ca_key,
    );
    let sub = subscriber(
        &lab,
        conn(
            EffectiveSecurity::MutualTls,
            Some(pki.ca_pem.clone()),
            Some((rogue_cert.as_str(), rogue_key.as_str())),
        ),
    );
    let (_stx, mut srx) = watch::channel(false);
    let result = tokio::time::timeout(Duration::from_secs(15), sub.subscribe_telemetry(&mut srx))
        .await
        .expect("handshake rejection must terminate");
    assert!(result.is_err(), "untrusted client cert must be rejected");
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 0);
}

/// A client certificate/key mismatch must fail — mismatched material is
/// never silently repaired or downgraded.
#[tokio::test]
async fn client_cert_key_mismatch_fails() {
    ensure_crypto_provider();
    let pki = test_pki();
    let lab = start_server(Some(tls_server_config(&pki, true))).await;
    // Valid client cert paired with the SERVER's private key — the pair
    // does not match, so the handshake must fail.
    let sub = subscriber(
        &lab,
        conn(
            EffectiveSecurity::MutualTls,
            Some(pki.ca_pem.clone()),
            Some((pki.client_cert_pem.as_str(), pki.server_key_pem.as_str())),
        ),
    );
    let (_stx, mut srx) = watch::channel(false);
    let result = tokio::time::timeout(Duration::from_secs(15), sub.subscribe_telemetry(&mut srx))
        .await
        .expect("connect attempt must terminate, not hang");
    assert!(result.is_err(), "cert/key mismatch must fail");
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 0);
}
