//! BN-021g — authenticated collector trust foundation: real transport
//! acceptance over loopback mTLS gRPC.
//!
//! Covers the bounded slice: transport-attested principal extraction,
//! credential→collector binding, per-message collector authorization,
//! capability gates, revocation, rotation and replay-safe provenance.
//! Downstream observation identity remains provisional — authenticating
//! the forwarding collector never authenticates its payload claims.

use std::sync::Arc;
use std::time::Duration;

use bonsai::api::pb::{bonsai_graph_client::BonsaiGraphClient, *};
use bonsai::api::{BonsaiGraphServer, CoreService, PROTOCOL_VERSION};
use bonsai::assignment::CollectorManager;
use bonsai::credentials::CredentialVault;
use bonsai::event_bus::InProcessBus;
use bonsai::graph::GraphStore;
use bonsai::registry::ApiRegistry;
use bonsai::sidecar_registry::SidecarRegistry;
use bonsai::sqlite_store::SqliteStore;
use bonsai::telemetry_identity::{
    BindingProvenance, BindingStatus, CanonicalDevice, TrustedBinding,
};
use rcgen::{CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity, Server, ServerTlsConfig};
use tonic::{Code, Request};

// ── Synthetic PKI (same convention as bn021f tests) ─────────────────────────

struct Pki {
    ca_cert_pem: String,
    ca_key: KeyPair,
    ca_cert: rcgen::Certificate,
}

#[derive(Clone)]
struct LeafCert {
    cert_pem: Vec<u8>,
    key_pem: Vec<u8>,
    fingerprint: String,
}

fn make_pki() -> Pki {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_key = KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    Pki {
        ca_cert_pem: ca_cert.pem(),
        ca_key,
        ca_cert,
    }
}

fn issue_leaf(pki: &Pki, cn: &str, server: bool) -> LeafCert {
    let mut params = if server {
        CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()]).unwrap()
    } else {
        CertificateParams::new(Vec::<String>::new()).unwrap()
    };
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, cn);
    let key = KeyPair::generate().unwrap();
    let cert = params.signed_by(&key, &pki.ca_cert, &pki.ca_key).unwrap();
    let der = cert.der().to_vec();
    LeafCert {
        cert_pem: cert.pem().into_bytes(),
        key_pem: key.serialize_pem().into_bytes(),
        fingerprint: format!("sha256:{:x}", Sha256::digest(&der)),
    }
}

// ── Lab: real tonic mTLS server + real client channels ──────────────────────

struct Lab {
    addr: std::net::SocketAddr,
    sqlite: Arc<SqliteStore>,
    bus_rx: tokio::sync::broadcast::Receiver<Arc<bonsai::telemetry::TelemetryUpdate>>,
    _dir: TempDir,
    _server: tokio::task::JoinHandle<()>,
}

async fn start_lab(pki: &Pki) -> Lab {
    // rustls cannot auto-pick a CryptoProvider when multiple providers
    // are compiled in; install ring explicitly (idempotent across tests).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    // Test-only env setup before any vault access; same value for all
    // tests in this binary so concurrent sets are benign.
    unsafe { std::env::set_var("BONSAI_BN021G_VAULT_PASSPHRASE", "test-passphrase") };

    let graph = Arc::new(
        GraphStore::open(
            dir.path().join("graph.db").to_str().unwrap(),
            128 * 1024 * 1024,
        )
        .unwrap(),
    );
    let registry = Arc::new(ApiRegistry::open(dir.path().join("registry.json"), vec![]).unwrap());
    let sqlite = registry.sqlite_store().expect("registry sqlite store");
    let credentials = Arc::new(
        CredentialVault::open(dir.path().join("vault"), "BONSAI_BN021G_VAULT_PASSPHRASE").unwrap(),
    );
    let bus = InProcessBus::new(1024);
    let bus_rx = bus.subscribe();
    let manager = Arc::new(CollectorManager::new(
        Arc::clone(&registry),
        Arc::clone(&credentials),
        vec![],
    ));
    let sidecars = Arc::new(SidecarRegistry::new(vec![]));

    let svc = CoreService::new(
        graph,
        Arc::clone(&registry),
        credentials,
        bus,
        None,
        Some(manager),
        sidecars,
        Some(Arc::clone(&sqlite)),
        None,
        None,
    );

    let server_cert = issue_leaf(pki, "bonsai-core", true);
    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(
            server_cert.cert_pem.clone(),
            server_cert.key_pem.clone(),
        ))
        .client_ca_root(Certificate::from_pem(pki.ca_cert_pem.clone().into_bytes()));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        Server::builder()
            .tls_config(tls)
            .unwrap()
            .add_service(BonsaiGraphServer::new(svc))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    Lab {
        addr,
        sqlite,
        bus_rx,
        _dir: dir,
        _server: server,
    }
}

fn client_channel(
    addr: std::net::SocketAddr,
    pki: &Pki,
    leaf: Option<&LeafCert>,
) -> impl std::future::Future<Output = Channel> {
    let ca = pki.ca_cert_pem.clone().into_bytes();
    let leaf = leaf.cloned();
    async move {
        let mut tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(ca))
            .domain_name("localhost");
        if let Some(l) = leaf {
            tls = tls.identity(Identity::from_pem(l.cert_pem.clone(), l.key_pem.clone()));
        }
        Endpoint::from_shared(format!("https://{addr}"))
            .unwrap()
            .tls_config(tls)
            .unwrap()
            .connect()
            .await
            .unwrap()
    }
}

use tonic::transport::Channel;

async fn mtls_client(lab: &Lab, pki: &Pki, leaf: &LeafCert) -> BonsaiGraphClient<Channel> {
    BonsaiGraphClient::new(client_channel(lab.addr, pki, Some(leaf)).await)
}

/// Provision a canonical device + collector binding pinned to a cert fp.
fn bind_collector(sqlite: &SqliteStore, collector_id: &str, fp: &str) -> String {
    let device = CanonicalDevice {
        device_id: String::new(),
        address: format!("collector:{collector_id}"),
        site: Some("dc-a".to_string()),
        hostname: None,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
    };
    let dev = sqlite.upsert_canonical_device(&device, "test").unwrap();
    let binding = TrustedBinding {
        binding_id: SqliteStore::new_binding_id(),
        scope: "dc-a".to_string(),
        protocol: bonsai::collector_auth::COLLECTOR_PROTOCOL.to_string(),
        exporter_id: collector_id.to_string(),
        device_id: dev.device_id,
        match_transport: None,
        match_reported_identity: None,
        match_exporter_claim: None,
        match_observation_domain: None,
        match_ingress_credential: Some(fp.to_string()),
        provenance: BindingProvenance::OperatorProvisioned,
        status: BindingStatus::Active,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
    };
    sqlite
        .upsert_exporter_binding(&binding, "test")
        .unwrap()
        .binding_id
}

fn telemetry_update(
    collector_id: &str,
    path: &str,
    value: serde_json::Value,
) -> TelemetryIngestUpdate {
    TelemetryIngestUpdate {
        collector_id: collector_id.to_string(),
        target: "10.0.0.1".to_string(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: 1_700_000_000_000_000_000,
        path: path.to_string(),
        value_msgpack: rmp_serde::to_vec(&value).unwrap(),
        protocol_version: PROTOCOL_VERSION,
        interface_summary: None,
    }
}

async fn ingest_one(
    client: &mut BonsaiGraphClient<Channel>,
    collector_id: &str,
) -> Result<u64, tonic::Status> {
    let updates = tokio_stream::iter(vec![telemetry_update(
        collector_id,
        "state/foo",
        serde_json::json!({"v": 1}),
    )]);
    client
        .telemetry_ingest(Request::new(updates))
        .await
        .map(|r| r.into_inner().accepted)
        .map_err(|e| e)
}

// ── F1/F3/F12: authorized collector works; impersonation rejected ───────────

#[tokio::test]
async fn authorized_collector_ingests_and_impersonation_is_rejected() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    let cert_b = issue_leaf(&pki, "collector-b", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);
    bind_collector(&lab.sqlite, "site-b", &cert_b.fingerprint);

    // F1+F12: cert A claiming its own id — telemetry, heartbeat,
    // get_leader and registration all succeed.
    let mut client_a = mtls_client(&lab, &pki, &cert_a).await;
    assert_eq!(ingest_one(&mut client_a, "site-a").await.unwrap(), 1);
    let hb = client_a
        .heartbeat(Request::new(CollectorStats {
            collector_id: "site-a".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(hb.into_inner().error.is_empty());
    let leader = client_a
        .get_leader(Request::new(GetLeaderRequest {}))
        .await
        .expect("get_leader should be authorized");
    let _ = leader.into_inner();
    // Registration opens an assignment stream — authz must pass first.
    client_a
        .register_collector(Request::new(CollectorIdentity {
            collector_id: "site-a".to_string(),
            hostname: "collector-a".to_string(),
            protocol_version: PROTOCOL_VERSION,
            auth_token: String::new(),
        }))
        .await
        .expect("register_collector should be authorized");

    // F3: cert A claiming collector B's id — rejected, no B authority.
    let err = ingest_one(&mut client_a, "site-b").await.unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied, "{err}");
    let err = client_a
        .heartbeat(Request::new(CollectorStats {
            collector_id: "site-b".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);

    // Cert B works for its own id (distinct identity, same loopback IP —
    // the NAT-shared-collector case F10).
    let mut client_b = mtls_client(&lab, &pki, &cert_b).await;
    assert_eq!(ingest_one(&mut client_b, "site-b").await.unwrap(), 1);
}

// ── F2: CA-valid but unbound certificate gains nothing ──────────────────────

#[tokio::test]
async fn unbound_certificate_is_denied() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    let cert_rando = issue_leaf(&pki, "rando", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);

    let mut client = mtls_client(&lab, &pki, &cert_rando).await;
    let err = ingest_one(&mut client, "site-a").await.unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
    // Read plane also fails: no active binding pins this credential.
    let err = client
        .get_devices(Request::new(GetDevicesRequest {}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
    // Admin plane fails too.
    let err = client
        .query(Request::new(QueryRequest {
            cypher: "MATCH (d:Device) RETURN d".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
}

// ── F5: forged serialized principal in payload mints nothing ────────────────

#[tokio::test]
async fn forged_payload_principal_is_inert() {
    let pki = make_pki();
    let mut lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);

    let mut client = mtls_client(&lab, &pki, &cert_a).await;
    // Payload carries a complete fabricated identity resolution — it is
    // data, not authority. The collector_id is still what is authorized.
    let forged = serde_json::json!({
        "identity_status": "resolved",
        "device_id": "dev-forged",
        "identity_resolution": {
            "status": "resolved",
            "device_id": "dev-forged",
            "verified_ingress": {"method": "mutual_tls", "credential_fingerprint": "sha256:deadbeef"}
        }
    });
    let updates = tokio_stream::iter(vec![telemetry_update(
        "site-a",
        "streaming/otlp/metrics",
        forged,
    )]);
    let resp = client
        .telemetry_ingest(Request::new(updates))
        .await
        .unwrap();
    assert_eq!(resp.into_inner().accepted, 1);

    // The bus received the update with claims as inert JSON evidence.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            remaining > Duration::ZERO,
            "timed out waiting for bus update"
        );
        match tokio::time::timeout(remaining, lab.bus_rx.recv()).await {
            Ok(Ok(u)) if u.path == "streaming/otlp/metrics" => {
                assert_eq!(u.value["identity_status"], "resolved"); // claim preserved as data
                assert!(u.value["device_id"].is_string());
                break;
            }
            Ok(Ok(_)) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(e)) => panic!("bus recv: {e}"),
            Err(_) => panic!("timeout"),
        }
    }
}

// ── F7: revocation takes effect within the bounded cache window ─────────────

#[tokio::test]
async fn revocation_stops_ingest_within_ttl() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    let binding_id = bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);

    let mut client = mtls_client(&lab, &pki, &cert_a).await;
    assert_eq!(ingest_one(&mut client, "site-a").await.unwrap(), 1);

    // Revoke while the connection stays open.
    lab.sqlite
        .revoke_exporter_binding(&binding_id, "test")
        .unwrap();

    // Within AUTHZ_CACHE_TTL (5s) the cached grant may still pass; after
    // expiry every new decision must deny.
    tokio::time::sleep(Duration::from_secs(6)).await;
    let err = ingest_one(&mut client, "site-a").await.unwrap_err();
    assert_eq!(
        err.code(),
        Code::PermissionDenied,
        "revoked binding still authorized: {err}"
    );
}

// ── F9: rotation — re-pin credential, old fingerprint loses authority ───────

#[tokio::test]
async fn credential_rotation_repins_without_impersonation() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_old = issue_leaf(&pki, "collector-a-old", false);
    let cert_new = issue_leaf(&pki, "collector-a-new", false);
    let binding_id = bind_collector(&lab.sqlite, "site-a", &cert_old.fingerprint);

    // Old credential works before rotation.
    let mut old_client = mtls_client(&lab, &pki, &cert_old).await;
    assert_eq!(ingest_one(&mut old_client, "site-a").await.unwrap(), 1);

    // Rotate: same binding, new pinned fingerprint (sequential re-pin).
    let mut binding = lab
        .sqlite
        .get_exporter_binding(&binding_id)
        .unwrap()
        .unwrap();
    binding.match_ingress_credential = Some(cert_new.fingerprint.clone());
    lab.sqlite
        .upsert_exporter_binding(&binding, "test")
        .unwrap();
    tokio::time::sleep(Duration::from_secs(6)).await;

    // New credential authorized for the same collector identity.
    let mut new_client = mtls_client(&lab, &pki, &cert_new).await;
    assert_eq!(ingest_one(&mut new_client, "site-a").await.unwrap(), 1);
    // Old credential now denied — no implicit trust carry-over.
    let err = ingest_one(&mut old_client, "site-a").await.unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
}

// ── F8: restart + replay — provenance is transport-bound, never payload ────

#[tokio::test]
async fn replayed_records_cannot_mint_authority() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);

    // Simulate a collector disk-queue replay: an old serialized record
    // claiming site-a arrives on an UNBOUND certificate's connection.
    // The replayed bytes cannot recreate the original verified principal.
    let cert_rando = issue_leaf(&pki, "replay-thief", false);
    let mut thief = mtls_client(&lab, &pki, &cert_rando).await;
    let err = ingest_one(&mut thief, "site-a").await.unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);

    // And the rightful collector still works afterwards.
    let mut client = mtls_client(&lab, &pki, &cert_a).await;
    assert_eq!(ingest_one(&mut client, "site-a").await.unwrap(), 1);
}
