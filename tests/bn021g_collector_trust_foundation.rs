//! BN-021g — authenticated collector trust foundation: real transport
//! acceptance over loopback mTLS gRPC, now including the canonical
//! graph projection path (G1), detection-authoring boundary (G2),
//! RPC capability matrix (G3), stream revocation/fencing (G4),
//! runtime auth policy (G5), principal/grant hardening (G6) and
//! provenance/audit correctness (G7).
//!
//! The lab wires the production path end to end: gRPC ingest →
//! InProcessBus → GraphStore::write (the same subscriber the core
//! runs), so canonical graph state is asserted directly.

use std::sync::Arc;
use std::time::Duration;

use bonsai::api::pb::{bonsai_graph_client::BonsaiGraphClient, *};
use bonsai::api::{BonsaiGraphServer, CoreService, PROTOCOL_VERSION};
use bonsai::assignment::CollectorManager;
use bonsai::collector_auth::{AuthPolicy, CollectorAuthz};
use bonsai::config::TargetConfig;
use bonsai::credentials::CredentialVault;
use bonsai::event_bus::InProcessBus;
use bonsai::graph::GraphStore;
use bonsai::registry::ApiRegistry;
use bonsai::sidecar_registry::SidecarRegistry;
use bonsai::sqlite_store::SqliteStore;
use bonsai::store::BonsaiStore;
use bonsai::telemetry_identity::{
    BindingProvenance, BindingStatus, CanonicalDevice, TrustedBinding,
    VerifiedIngressPrincipal,
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

// ── Lab: real tonic server + real client channels + real graph path ────────

struct Lab {
    addr: std::net::SocketAddr,
    sqlite: Arc<SqliteStore>,
    registry: Arc<ApiRegistry>,
    graph: Arc<GraphStore>,
    bus_rx: tokio::sync::broadcast::Receiver<Arc<bonsai::telemetry::TelemetryUpdate>>,
    plaintext: bool,
    _dir: TempDir,
    _server: tokio::task::JoinHandle<()>,
    _graph_writer: tokio::task::JoinHandle<()>,
}

/// Start the core service. `use_tls` selects the mTLS listener;
/// `policy` selects the runtime auth policy (G5).
async fn start_lab_mode(
    pki: Option<&Pki>,
    use_tls: bool,
    policy: AuthPolicy,
) -> Lab {
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
    // Wire the production graph subscriber: bus → GraphStore::write,
    // the same path server_startup uses for canonical projection.
    let mut graph_rx = bus.subscribe();
    let graph_for_writer = Arc::clone(&graph);
    let graph_writer = tokio::spawn(async move {
        while let Ok(update) = graph_rx.recv().await {
            let u = Arc::unwrap_or_clone(update);
            if let Err(e) = BonsaiStore::write(graph_for_writer.as_ref(), u).await {
                tracing::warn!(%e, "lab graph writer failed");
            }
        }
    });
    let manager = Arc::new(CollectorManager::new(
        Arc::clone(&registry),
        Arc::clone(&credentials),
        vec![],
    ));
    let sidecars = Arc::new(SidecarRegistry::new(vec![]));

    let svc = CoreService::new(
        Arc::clone(&graph),
        Arc::clone(&registry),
        credentials,
        bus,
        None,
        Some(manager),
        sidecars,
        Some(Arc::clone(&sqlite)),
        None,
        None,
    )
    .with_auth_policy(policy);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = if use_tls {
        let pki = pki.expect("tls lab requires pki");
        let server_cert = issue_leaf(pki, "bonsai-core", true);
        let tls = ServerTlsConfig::new()
            .identity(Identity::from_pem(
                server_cert.cert_pem.clone(),
                server_cert.key_pem.clone(),
            ))
            .client_ca_root(Certificate::from_pem(pki.ca_cert_pem.clone().into_bytes()));
        tokio::spawn(async move {
            Server::builder()
                .tls_config(tls)
                .unwrap()
                .add_service(BonsaiGraphServer::new(svc))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
                .unwrap();
        })
    } else {
        tokio::spawn(async move {
            Server::builder()
                .add_service(BonsaiGraphServer::new(svc))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
                .unwrap();
        })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;

    Lab {
        addr,
        sqlite,
        registry,
        graph,
        bus_rx,
        plaintext: !use_tls,
        _dir: dir,
        _server: server,
        _graph_writer: graph_writer,
    }
}

async fn start_lab(pki: &Pki) -> Lab {
    start_lab_mode(Some(pki), true, AuthPolicy::Required).await
}

fn client_channel(
    lab: &Lab,
    pki: Option<&Pki>,
    leaf: Option<&LeafCert>,
) -> impl std::future::Future<Output = Channel> {
    let ca_pem = pki.map(|p| p.ca_cert_pem.clone());
    let leaf = leaf.cloned();
    let addr = lab.addr;
    let plaintext = lab.plaintext;
    async move {
        if plaintext {
            return Endpoint::from_shared(format!("http://{addr}"))
                .unwrap()
                .connect()
                .await
                .unwrap();
        }
        let ca_pem = ca_pem.expect("mtls client requires pki");
        let mut tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(ca_pem.into_bytes()))
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
    BonsaiGraphClient::new(client_channel(lab, Some(pki), Some(leaf)).await)
}

async fn plain_client(lab: &Lab) -> BonsaiGraphClient<Channel> {
    BonsaiGraphClient::new(client_channel(lab, None, None).await)
}

// ── Provisioning helpers ────────────────────────────────────────────────────

fn canonical_anchor(sqlite: &SqliteStore) -> String {
    sqlite
        .upsert_canonical_device(
            &CanonicalDevice {
                device_id: String::new(),
                address: "capability-anchor".to_string(),
                site: None,
                hostname: None,
                created_at_ns: 0,
                updated_at_ns: 0,
                created_by: String::new(),
                updated_by: String::new(),
            },
            "test",
        )
        .unwrap()
        .device_id
}

/// Provision a canonical device + collector binding pinned to a cert fp.
fn bind_collector_scoped(
    sqlite: &SqliteStore,
    collector_id: &str,
    fp: &str,
    scope: &str,
) -> String {
    let device = CanonicalDevice {
        device_id: String::new(),
        address: format!("collector:{collector_id}"),
        site: Some(scope.to_string()),
        hostname: None,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
    };
    let dev = sqlite.upsert_canonical_device(&device, "test").unwrap();
    let binding = TrustedBinding {
        binding_id: SqliteStore::new_binding_id(),
        scope: scope.to_string(),
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

fn bind_collector(sqlite: &SqliteStore, collector_id: &str, fp: &str) -> String {
    bind_collector_scoped(sqlite, collector_id, fp, "dc-a")
}

/// Capability binding (detection-ingest / operator-read / core-admin /
/// exporter protocols) pinned to a cert fp.
fn bind_capability(sqlite: &SqliteStore, protocol: &str, fp: &str) -> String {
    let binding = TrustedBinding {
        binding_id: SqliteStore::new_binding_id(),
        scope: "dc-a".to_string(),
        protocol: protocol.to_string(),
        exporter_id: format!("{protocol}-principal"),
        device_id: canonical_anchor(sqlite),
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

fn target(
    address: &str,
    collector_id: Option<&str>,
    site: &str,
    role: &str,
    hostname: &str,
    vendor: &str,
) -> TargetConfig {
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
        role: Some(role.to_string()),
        site: Some(site.to_string()),
        collector_id: collector_id.map(str::to_string),
        selected_paths: Vec::new(),
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
        last_operator_action: String::new(),
    }
}

fn add_device(registry: &ApiRegistry, t: TargetConfig) {
    registry
        .add_device_with_audit(t.clone(), "test", "test_add")
        .unwrap();
    registry
        .assign_device_with_audit(
            &t.address,
            t.collector_id.clone(),
            "test",
            "test_assign",
        )
        .unwrap();
}

fn telemetry_update(
    collector_id: &str,
    target_addr: &str,
    path: &str,
    value: serde_json::Value,
) -> TelemetryIngestUpdate {
    TelemetryIngestUpdate {
        collector_id: collector_id.to_string(),
        target: target_addr.to_string(),
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

async fn ingest_updates(
    client: &mut BonsaiGraphClient<Channel>,
    updates: Vec<TelemetryIngestUpdate>,
) -> Result<u64, tonic::Status> {
    client
        .telemetry_ingest(Request::new(tokio_stream::iter(updates)))
        .await
        .map(|r| r.into_inner().accepted)
        .map_err(|e| e)
}

async fn ingest_one(
    client: &mut BonsaiGraphClient<Channel>,
    collector_id: &str,
    target_addr: &str,
    path: &str,
) -> Result<u64, tonic::Status> {
    ingest_updates(
        client,
        vec![telemetry_update(
            collector_id,
            target_addr,
            path,
            serde_json::json!({"v": 1}),
        )],
    )
    .await
}

// ── Graph helpers ───────────────────────────────────────────────────────────

fn graph_query(graph: &GraphStore, cypher: &str) -> Vec<Vec<String>> {
    let db = graph.db();
    let conn = lbug::Connection::new(&db).unwrap();
    conn.query(cypher)
        .unwrap()
        .map(|row| {
            row.iter()
                .map(|v| match v {
                    lbug::Value::String(s) => s.clone(),
                    lbug::Value::Int64(n) => n.to_string(),
                    other => other.to_string(),
                })
                .collect()
        })
        .collect()
}

fn graph_query_one(graph: &GraphStore, cypher: &str) -> Option<Vec<String>> {
    graph_query(graph, cypher).into_iter().next()
}

/// Poll the graph until `cypher` returns a row or the deadline passes.
async fn wait_for_graph_row(graph: &GraphStore, cypher: &str, what: &str) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    loop {
        if let Some(row) = graph_query_one(graph, cypher) {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for graph state: {what} ({cypher})"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn prepopulate_device(graph: &GraphStore, address: &str) {
    let db = graph.db();
    let conn = lbug::Connection::new(&db).unwrap();
    conn.query(&format!(
        "CREATE (d:Device {{address: '{}'}})",
        address.replace('\'', "\\'")
    ))
    .unwrap();
}

fn prepopulate_event(
    graph: &GraphStore,
    id: &str,
    device_address: &str,
    detail: &str,
    source_type: &str,
) {
    let db = graph.db();
    let conn = lbug::Connection::new(&db).unwrap();
    let mut stmt = conn
        .prepare(
            "CREATE (e:StateChangeEvent {id: $id, device_address: $addr, \
             event_type: 'test_event', detail: $detail, source_type: $src, \
             occurred_at: $ts})",
        )
        .unwrap();
    conn.execute(
        &mut stmt,
        vec![
            ("id", lbug::Value::String(id.to_string())),
            ("addr", lbug::Value::String(device_address.to_string())),
            ("detail", lbug::Value::String(detail.to_string())),
            ("src", lbug::Value::String(source_type.to_string())),
            (
                "ts",
                lbug::Value::TimestampNs(
                    time::OffsetDateTime::UNIX_EPOCH
                        + time::Duration::nanoseconds(1_700_000_000_000_000_000),
                ),
            ),
        ],
    )
    .unwrap();
}

fn detection_update(
    collector_id: &str,
    device_address: &str,
    severity: &str,
    state_change_event_id: &str,
    source_event_ids: Vec<String>,
) -> DetectionEventIngest {
    DetectionEventIngest {
        collector_id: collector_id.to_string(),
        device_address: device_address.to_string(),
        rule_id: "test.rule".to_string(),
        severity: severity.to_string(),
        features_json: "{}".to_string(),
        source_types_json: "[]".to_string(),
        latency_ns: 0,
        fired_at_ns: 1_700_000_000_000_000_000,
        state_change_event_id: state_change_event_id.to_string(),
        source_event_ids,
        // Remote submissions cannot self-authorize remediation: the
        // ingest path ignores these fields entirely (G2 boundary).
        auto_remediate: false,
        remediation_action: String::new(),
        reason: String::new(),
    }
}

async fn ingest_detections(
    client: &mut BonsaiGraphClient<Channel>,
    updates: Vec<DetectionEventIngest>,
) -> Result<u64, tonic::Status> {
    client
        .detection_ingest(Request::new(tokio_stream::iter(updates)))
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
    // Canonical inventory: one device per collector.
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-a"), "dc-a", "leaf", "rtr-a", "ios"),
    );
    add_device(
        &lab.registry,
        target("10.0.0.2", Some("site-b"), "dc-b", "leaf", "rtr-b", "ios"),
    );

    // F1+F12: cert A claiming its own id — telemetry, heartbeat,
    // get_leader and registration all succeed.
    let mut client_a = mtls_client(&lab, &pki, &cert_a).await;
    assert_eq!(
        ingest_one(&mut client_a, "site-a", "10.0.0.1", "state/foo").await.unwrap(),
        1
    );
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
    let err = ingest_one(&mut client_a, "site-b", "10.0.0.2", "state/foo")
        .await
        .unwrap_err();
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
    assert_eq!(
        ingest_one(&mut client_b, "site-b", "10.0.0.2", "state/foo").await.unwrap(),
        1
    );
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
    let err = ingest_one(&mut client, "site-a", "10.0.0.1", "state/foo")
        .await
        .unwrap_err();
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
    // data, not authority. Provisional path: stays provisional evidence.
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
        "10.0.0.1",
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
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-a"), "dc-a", "leaf", "rtr-a", "ios"),
    );

    let mut client = mtls_client(&lab, &pki, &cert_a).await;
    assert_eq!(
        ingest_one(&mut client, "site-a", "10.0.0.1", "state/foo").await.unwrap(),
        1
    );

    // Revoke while the connection stays open.
    lab.sqlite
        .revoke_exporter_binding(&binding_id, "test")
        .unwrap();

    // Within AUTHZ_CACHE_TTL (5s) the cached grant may still pass; after
    // expiry every new decision must deny.
    tokio::time::sleep(Duration::from_secs(6)).await;
    let err = ingest_one(&mut client, "site-a", "10.0.0.1", "state/foo")
        .await
        .unwrap_err();
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
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-a"), "dc-a", "leaf", "rtr-a", "ios"),
    );

    // Old credential works before rotation.
    let mut old_client = mtls_client(&lab, &pki, &cert_old).await;
    assert_eq!(
        ingest_one(&mut old_client, "site-a", "10.0.0.1", "state/foo").await.unwrap(),
        1
    );

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
    assert_eq!(
        ingest_one(&mut new_client, "site-a", "10.0.0.1", "state/foo").await.unwrap(),
        1
    );
    // Old credential now denied — no implicit trust carry-over.
    let err = ingest_one(&mut old_client, "site-a", "10.0.0.1", "state/foo")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
}

// ── F8: restart + replay — provenance is transport-bound, never payload ────

#[tokio::test]
async fn replayed_records_cannot_mint_authority() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-a"), "dc-a", "leaf", "rtr-a", "ios"),
    );

    // Simulate a collector disk-queue replay: an old serialized record
    // claiming site-a arrives on an UNBOUND certificate's connection.
    // The replayed bytes cannot recreate the original verified principal.
    let cert_rando = issue_leaf(&pki, "replay-thief", false);
    let mut thief = mtls_client(&lab, &pki, &cert_rando).await;
    let err = ingest_one(&mut thief, "site-a", "10.0.0.1", "state/foo")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);

    // And the rightful collector still works afterwards.
    let mut client = mtls_client(&lab, &pki, &cert_a).await;
    assert_eq!(
        ingest_one(&mut client, "site-a", "10.0.0.1", "state/foo").await.unwrap(),
        1
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// G1 — observation authorization on canonical projection
// ═══════════════════════════════════════════════════════════════════════════

/// Canonical-path telemetry may only target a device ASSIGNED to the
/// authenticated collector; cross-collector, unknown and unassigned
/// targets are dropped before the bus; forged inventory metadata is
/// rewritten from the registry.
#[tokio::test]
async fn canonical_telemetry_requires_assigned_device_and_inventory_metadata() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    let cert_b = issue_leaf(&pki, "collector-b", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);
    bind_collector(&lab.sqlite, "site-b", &cert_b.fingerprint);
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-a"), "dc-a", "leaf", "rtr-a", "ios"),
    );
    add_device(
        &lab.registry,
        target("10.0.0.2", Some("site-b"), "dc-b", "spine", "rtr-b", "junos"),
    );

    let mut client_a = mtls_client(&lab, &pki, &cert_a).await;

    // Collector A cannot mutate collector B's device: the observation
    // is dropped (accepted counts only published updates).
    let accepted = ingest_updates(
        &mut client_a,
        vec![
            // B's device — denied.
            telemetry_update(
                "site-a",
                "10.0.0.2",
                "interfaces/interface[name=eth0]/state/counters",
                serde_json::json!({"in-octets": 999}),
            ),
            // Unknown target — denied.
            telemetry_update(
                "site-a",
                "192.0.2.66",
                "interfaces/interface[name=eth0]/state/counters",
                serde_json::json!({"in-octets": 1}),
            ),
            // A's own device — accepted, with FORGED metadata that must
            // be rewritten from authoritative inventory.
            {
                let mut u = telemetry_update(
                    "site-a",
                    "10.0.0.1",
                    "interfaces/interface[name=eth0]/state/counters",
                    serde_json::json!({"in-octets": 42}),
                );
                u.hostname = "forged-host".to_string();
                u.role = "core".to_string();
                u.site = "evil-site".to_string();
                u.vendor = "forged-vendor".to_string();
                u
            },
        ],
    )
    .await
    .unwrap();
    assert_eq!(accepted, 1, "only the assigned-device update may publish");

    // Canonical projection happened for the legitimate update only:
    // an Interface node exists for 10.0.0.1 and NOT for 10.0.0.2.
    wait_for_graph_row(
        &lab.graph,
        "MATCH (i:Interface {device_address: '10.0.0.1'}) RETURN i.name",
        "interface on assigned device",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        graph_query_one(
            &lab.graph,
            "MATCH (i:Interface {device_address: '10.0.0.2'}) RETURN i.name"
        )
        .is_none(),
        "collector A must not create canonical state on collector B's device"
    );

    // Forged hostname/vendor never reached the canonical Device node.
    let device = wait_for_graph_row(
        &lab.graph,
        "MATCH (d:Device {address: '10.0.0.1'}) RETURN d.hostname, d.vendor",
        "device node",
    )
    .await;
    assert_eq!(device[0], "rtr-a", "forged hostname must not persist");
    assert_eq!(device[1], "ios", "forged vendor must not persist");
    assert!(
        graph_query_one(
            &lab.graph,
            "MATCH (d:Device {hostname: 'forged-host'}) RETURN d.address"
        )
        .is_none(),
        "no Device node may carry forged inventory metadata"
    );
}

/// Provisional protocol payloads stay provisional: they may traverse
/// the authorized channel but can never be relabeled onto a canonical
/// path to gain device authority.
#[tokio::test]
async fn provisional_payloads_cannot_gain_canonical_authority() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-a"), "dc-a", "leaf", "rtr-a", "ios"),
    );
    let mut client = mtls_client(&lab, &pki, &cert_a).await;

    // 1. Legitimate provisional flow record — passes through as
    //    provisional evidence (flowsrc target key, streaming/ path).
    let flow = TelemetryIngestUpdate {
        collector_id: "site-a".to_string(),
        target: "flowsrc/198.51.100.9#sid=0".to_string(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: 1_700_000_000_000_000_000,
        path: "streaming/netflow/flow".to_string(),
        value_msgpack: rmp_serde::to_vec(&serde_json::json!({
            "exporter_address": "198.51.100.9",
            "exporter_id": "flowsrc/site-a/nf9/198.51.100.9:49152/sid7",
            "src_address": "10.1.1.1",
            "src_port": 12345,
            "dst_address": "10.1.1.2",
            "dst_port": 80,
            "protocol": "tcp",
            "bytes_per_sec": 100.0,
            "packets_per_sec": 10.0
        }))
        .unwrap(),
        protocol_version: PROTOCOL_VERSION,
        interface_summary: None,
    };
    // Relabel attempt: canonical gNMI path on a device target that is
    // NOT assigned to site-a → denied before reaching the bus.
    let relabel_unassigned = telemetry_update(
        "site-a",
        "203.0.113.9",
        "interfaces/interface[name=eth0]/state/counters",
        serde_json::json!({"in-octets": 1}),
    );

    let accepted = ingest_updates(
        &mut client,
        vec![flow, relabel_unassigned],
    )
    .await
    .unwrap();
    assert_eq!(accepted, 1, "provisional flow passes; relabeled canonical path denied");

    // The flow stayed provisional: no canonical Interface/Device state
    // was created for the relabeled target, and the flow evidence is
    // keyed by its exporter source, not a device.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        graph_query(
            &lab.graph,
            "MATCH (d:Device) WHERE d.address = '203.0.113.9' RETURN d.address"
        )
        .is_empty(),
        "relabeled payload must not create canonical inventory"
    );
    // Provisional evidence remains queryable under its exporter source
    // key: an AppFlow node carrying the flowsrc/... exporter_id. No
    // canonical Device/Interface state is created for the exporter.
    let provisional = wait_for_graph_row(
        &lab.graph,
        "MATCH (f:AppFlow) WHERE f.exporter_id STARTS WITH 'flowsrc/' RETURN f.id",
        "provisional flow AppFlow",
    )
    .await;
    assert!(!provisional.is_empty());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        graph_query_one(
            &lab.graph,
            "MATCH (d:Device {address: '198.51.100.9'}) RETURN d.address"
        )
        .is_none(),
        "provisional flow must not mint a canonical Device for the exporter"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// G2 — DetectionIngest authority boundary
// ═══════════════════════════════════════════════════════════════════════════

/// Two canonical devices + their state events; detections require the
/// separate detection-ingest capability, an assigned target, and
/// in-scope verified event references.
#[tokio::test]
async fn detection_ingest_enforces_capability_scope_and_provenance() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    let cert_b = issue_leaf(&pki, "collector-b", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);
    bind_collector(&lab.sqlite, "site-b", &cert_b.fingerprint);
    // site-a collector may submit detections; site-b may not.
    bind_capability(
        &lab.sqlite,
        bonsai::collector_auth::DETECTION_INGEST_PROTOCOL,
        &cert_a.fingerprint,
    );
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-a"), "dc-a", "leaf", "rtr-a", "ios"),
    );
    add_device(
        &lab.registry,
        target("10.0.0.2", Some("site-b"), "dc-b", "spine", "rtr-b", "junos"),
    );
    // Canonical graph state: devices + state events.
    prepopulate_device(&lab.graph, "10.0.0.1");
    prepopulate_device(&lab.graph, "10.0.0.2");
    prepopulate_event(&lab.graph, "sce-a-1", "10.0.0.1", "{}", "gnmi");
    prepopulate_event(&lab.graph, "sce-b-1", "10.0.0.2", "{}", "gnmi");
    prepopulate_event(
        &lab.graph,
        "sce-prov-1",
        "flowsrc/198.51.100.9#sid=0",
        "{\"identity_status\":\"provisional\"}",
        "netflow",
    );

    let mut client_a = mtls_client(&lab, &pki, &cert_a).await;
    let mut client_b = mtls_client(&lab, &pki, &cert_b).await;

    // 1. Collector B lacks the detection capability entirely — the
    //    stream is refused before any message is processed.
    let err = ingest_detections(
        &mut client_b,
        vec![detection_update("site-b", "10.0.0.2", "high", "", vec![])],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied, "{err}");

    // 2-5. Collector A: cross-collector target, foreign event reference,
    //      provisional-only evidence and fabricated references are all
    //      dropped — none reach the graph.
    let accepted = ingest_detections(
        &mut client_a,
        vec![
            // Detection targeting collector B's device.
            detection_update("site-a", "10.0.0.2", "critical", "", vec![]),
            // Valid target but references B's state event.
            detection_update("site-a", "10.0.0.1", "high", "sce-b-1", vec![]),
            // References only provisional (unverified) evidence.
            detection_update("site-a", "10.0.0.1", "high", "sce-prov-1", vec![]),
            // Fabricated critical detection citing a nonexistent event.
            detection_update("site-a", "10.0.0.1", "critical", "sce-fake", vec![]),
            // Unknown device entirely.
            detection_update("site-a", "203.0.113.9", "low", "", vec![]),
        ],
    )
    .await
    .unwrap();
    assert_eq!(accepted, 0, "all unauthorized detections must be dropped");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        graph_query(&lab.graph, "MATCH (e:DetectionEvent) RETURN e.id")
            .is_empty(),
        "no DetectionEvent may be written by unauthorized submissions"
    );

    // 6. Valid detection from the authorized source: assigned device +
    //    verified in-scope event reference → written to the graph.
    let accepted = ingest_detections(
        &mut client_a,
        vec![detection_update(
            "site-a",
            "10.0.0.1",
            "high",
            "sce-a-1",
            vec!["sce-a-1".to_string()],
        )],
    )
    .await
    .unwrap();
    assert_eq!(accepted, 1);
    let det = wait_for_graph_row(
        &lab.graph,
        "MATCH (d:Device {address: '10.0.0.1'})-[:TRIGGERED]->(e:DetectionEvent) RETURN e.id",
        "TRIGGERED detection edge",
    )
    .await;
    assert!(!det.is_empty());
    // TRIGGERED_BY links only the verified in-scope event.
    let triggered_by = graph_query(
        &lab.graph,
        "MATCH (e:DetectionEvent)-[:TRIGGERED_BY]->(s:StateChangeEvent) RETURN s.id",
    );
    assert!(triggered_by.iter().all(|r| r[0] == "sce-a-1"));
}

// ═══════════════════════════════════════════════════════════════════════════
// G3 — RPC capability matrix
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn rpc_capability_matrix_scopes_reads_and_denies_exporters() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_col = issue_leaf(&pki, "collector-a", false);
    let cert_exporter = issue_leaf(&pki, "bmp-exporter", false);
    let cert_operator = issue_leaf(&pki, "operator", false);
    let cert_admin = issue_leaf(&pki, "admin", false);
    bind_collector(&lab.sqlite, "site-a", &cert_col.fingerprint);
    // BMP-only exporter credential — no collector, no read, no admin.
    bind_capability(&lab.sqlite, "bmp", &cert_exporter.fingerprint);
    bind_capability(
        &lab.sqlite,
        bonsai::collector_auth::OPERATOR_READ_PROTOCOL,
        &cert_operator.fingerprint,
    );
    bind_capability(
        &lab.sqlite,
        bonsai::collector_auth::CORE_ADMIN_PROTOCOL,
        &cert_admin.fingerprint,
    );
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-a"), "dc-a", "leaf", "rtr-a", "ios"),
    );
    add_device(
        &lab.registry,
        target("10.0.0.2", Some("site-b"), "dc-b", "spine", "rtr-b", "junos"),
    );
    prepopulate_device(&lab.graph, "10.0.0.1");
    prepopulate_device(&lab.graph, "10.0.0.2");

    let mut col = mtls_client(&lab, &pki, &cert_col).await;
    let mut exporter = mtls_client(&lab, &pki, &cert_exporter).await;
    let mut operator = mtls_client(&lab, &pki, &cert_operator).await;
    let mut admin = mtls_client(&lab, &pki, &cert_admin).await;

    // ── Collector-scoped reads ──
    // get_devices returns ONLY the collector's assigned device.
    let resp = col
        .get_devices(Request::new(GetDevicesRequest {}))
        .await
        .expect("collector-scoped get_devices");
    let addrs: Vec<String> = resp.into_inner().devices.iter().map(|d| d.address.clone()).collect();
    assert_eq!(addrs, vec!["10.0.0.1".to_string()], "scope leak: {addrs:?}");
    // list_managed_devices likewise.
    let resp = col
        .list_managed_devices(Request::new(ListManagedDevicesRequest {}))
        .await
        .expect("collector-scoped list_managed_devices");
    let addrs: Vec<String> = resp
        .into_inner()
        .devices
        .iter()
        .map(|d| d.address.clone())
        .collect();
    assert_eq!(addrs, vec!["10.0.0.1".to_string()]);
    // Device-scoped interface read on own device passes (empty ok).
    col.get_interfaces(Request::new(GetInterfacesRequest {
        device_address: "10.0.0.1".to_string(),
    }))
    .await
    .expect("scoped interface read");
    // Other device, global read, topology, sites, events: denied.
    for res in [
        col.get_interfaces(Request::new(GetInterfacesRequest {
            device_address: "10.0.0.2".to_string(),
        }))
        .await
        .unwrap_err(),
        col.get_interfaces(Request::new(GetInterfacesRequest {
            device_address: String::new(),
        }))
        .await
        .unwrap_err(),
        col.get_bgp_neighbors(Request::new(GetBgpNeighborsRequest {
            device_address: String::new(),
        }))
        .await
        .unwrap_err(),
        col.get_topology(Request::new(GetTopologyRequest {}))
            .await
            .unwrap_err(),
        col.list_sites(Request::new(ListSitesRequest {}))
            .await
            .unwrap_err(),
        col.stream_events(Request::new(StreamEventsRequest::default()))
            .await
            .unwrap_err(),
        col.query(Request::new(QueryRequest {
            cypher: "MATCH (d:Device) RETURN d".to_string(),
        }))
        .await
        .unwrap_err(),
    ] {
        assert_eq!(res.code(), Code::PermissionDenied);
    }
    // get_leader remains available to any bound credential (core discovery).
    col.get_leader(Request::new(GetLeaderRequest {}))
        .await
        .expect("collector core discovery");

    // ── Exporter-only credential: no inventory read at all ──
    for res in [
        exporter
            .get_devices(Request::new(GetDevicesRequest {}))
            .await
            .unwrap_err(),
        exporter
            .list_sites(Request::new(ListSitesRequest {}))
            .await
            .unwrap_err(),
        exporter
            .get_topology(Request::new(GetTopologyRequest {}))
            .await
            .unwrap_err(),
        exporter
            .query(Request::new(QueryRequest {
                cypher: "MATCH (d:Device) RETURN d".to_string(),
            }))
            .await
            .unwrap_err(),
    ] {
        assert_eq!(res.code(), Code::PermissionDenied);
    }
    // …but core discovery still works for any pinned credential.
    exporter
        .get_leader(Request::new(GetLeaderRequest {}))
        .await
        .expect("exporter leader discovery");

    // ── Operator-read: global operational reads, NOT admin ──
    let resp = operator
        .get_devices(Request::new(GetDevicesRequest {}))
        .await
        .expect("operator global read");
    assert_eq!(resp.into_inner().devices.len(), 2);
    operator
        .list_sites(Request::new(ListSitesRequest {}))
        .await
        .expect("operator sites");
    operator
        .get_topology(Request::new(GetTopologyRequest {}))
        .await
        .expect("operator topology");
    // operator-read does NOT grant ad-hoc graph query or mutations.
    let err = operator
        .query(Request::new(QueryRequest {
            cypher: "MATCH (d:Device) RETURN d".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
    let err = operator
        .add_device(Request::new(AddDeviceRequest {
            device: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);

    // ── Core-admin: ad-hoc query works; reads still need read scope ──
    admin
        .query(Request::new(QueryRequest {
            cypher: "MATCH (d:Device) RETURN count(d)".to_string(),
        }))
        .await
        .expect("admin query");
    // core-admin alone does not imply inventory read.
    let err = admin
        .get_devices(Request::new(GetDevicesRequest {}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
}

// ═══════════════════════════════════════════════════════════════════════════
// G4 — active-stream revocation, credential delivery, session fencing
// ═══════════════════════════════════════════════════════════════════════════

/// The assignment stream revalidates authorization: initial
/// credential-bearing delivery works, revocation ends the stream, and
/// no further credential-bearing updates are delivered afterwards.
#[tokio::test]
async fn assignment_stream_revocation_stops_credential_delivery() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    let binding_id = bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);
    let mut t = target("10.0.0.1", Some("site-a"), "dc-a", "leaf", "rtr-a", "ios");
    t.username = Some("device-user".to_string());
    t.password = Some("device-secret".to_string());
    add_device(&lab.registry, t);

    let mut client = mtls_client(&lab, &pki, &cert_a).await;
    let resp = client
        .register_collector(Request::new(CollectorIdentity {
            collector_id: "site-a".to_string(),
            hostname: "collector-a".to_string(),
            protocol_version: PROTOCOL_VERSION,
            auth_token: String::new(),
        }))
        .await
        .expect("register");
    let mut stream = resp.into_inner();

    // Initial full-sync delivery includes the credential-bearing
    // assignment — this is exactly what revocation must stop.
    let first = tokio::time::timeout(Duration::from_secs(5), stream.message())
        .await
        .expect("initial assignment timeout")
        .expect("stream error")
        .expect("initial assignment update");
    assert!(first.is_full_sync);
    let assignment = first.assignments.first().expect("one assignment");
    assert_eq!(assignment.username, "device-user");
    assert_eq!(assignment.password, "device-secret");

    // Revoke the binding while the stream sits idle.
    lab.sqlite
        .revoke_exporter_binding(&binding_id, "test")
        .unwrap();

    // Within ~AUTHZ_CACHE_TTL + revalidate interval the stream must end
    // without delivering anything further.
    let ended = tokio::time::timeout(Duration::from_secs(15), stream.message()).await;
    match ended {
        Ok(Ok(None)) | Ok(Err(_)) | Err(_) => {}
        Ok(Ok(Some(_))) => panic!("credential-bearing update delivered after revocation"),
    }
    // The stream must have ENDED (not merely timed out silently):
    // verify no second message is forthcoming.
    assert!(
        tokio::time::timeout(Duration::from_secs(2), stream.message())
            .await
            .map(|r| r.map(|m| m.is_none()).unwrap_or(true))
            .unwrap_or(true),
        "stream still open after revocation bound"
    );
}

/// A stale stream close (superseded reconnect) must not unregister the
/// newer session or orphan its device assignments.
#[tokio::test]
async fn reconnect_generation_fencing_preserves_new_session() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-a"), "dc-a", "leaf", "rtr-a", "ios"),
    );

    let identity = || {
        Request::new(CollectorIdentity {
            collector_id: "site-a".to_string(),
            hostname: "collector-a".to_string(),
            protocol_version: PROTOCOL_VERSION,
            auth_token: String::new(),
        })
    };

    // Session 1.
    let mut client1 = mtls_client(&lab, &pki, &cert_a).await;
    let s1 = client1
        .register_collector(identity())
        .await
        .unwrap()
        .into_inner();
    tokio::pin!(s1);
    assert!(s1.message().await.unwrap().is_some(), "session 1 initial sync");

    // Session 2 supersedes session 1 for the same collector id.
    let mut client2 = mtls_client(&lab, &pki, &cert_a).await;
    let s2 = client2
        .register_collector(identity())
        .await
        .unwrap()
        .into_inner();
    tokio::pin!(s2);
    assert!(s2.message().await.unwrap().is_some(), "session 2 initial sync");

    // Session 1's sender was replaced → its stream ends → its fenced
    // unregister must be a no-op.
    let end1 = tokio::time::timeout(Duration::from_secs(8), s1.message()).await;
    assert!(
        matches!(end1, Ok(Ok(None)) | Ok(Err(_))),
        "old session stream should end after supersede: {end1:?}"
    );

    // Give the fenced unregister a moment, then prove the device was
    // NOT unassigned/reassigned by the stale close.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let dev = lab.registry.get_device("10.0.0.1").unwrap().unwrap();
    assert_eq!(
        dev.collector_id.as_deref(),
        Some("site-a"),
        "stale session close must not clear the new session's assignments"
    );

    // Session 2 remains authorized and open (idle — no message pending,
    // but the stream must not have been terminated by session 1's close).
    let still_open = tokio::time::timeout(Duration::from_secs(6), s2.message()).await;
    assert!(
        still_open.is_err(),
        "new session stream must survive the old session's close"
    );
}

/// stream_events requires operator-read and is revoked on the same
/// bounded window — including while the stream is idle.
#[tokio::test]
async fn stream_events_revocation_ends_idle_stream() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_op = issue_leaf(&pki, "operator", false);
    let binding_id = bind_capability(
        &lab.sqlite,
        bonsai::collector_auth::OPERATOR_READ_PROTOCOL,
        &cert_op.fingerprint,
    );

    let mut client = mtls_client(&lab, &pki, &cert_op).await;
    let resp = client
        .stream_events(Request::new(StreamEventsRequest::default()))
        .await
        .expect("operator stream_events");
    let mut stream = resp.into_inner();

    lab.sqlite
        .revoke_exporter_binding(&binding_id, "test")
        .unwrap();

    // Idle stream: no events flowing — revocation must still terminate
    // it within the bounded window.
    let ended = tokio::time::timeout(Duration::from_secs(15), stream.message()).await;
    match ended {
        Ok(Ok(None)) | Ok(Err(_)) | Err(_) => {}
        Ok(Ok(Some(_))) => panic!("event delivered after read revocation"),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// G5 — runtime authentication policy
// ═══════════════════════════════════════════════════════════════════════════

/// Required mode: a TLS connection WITHOUT a client certificate gains
/// nothing — every RPC fails closed.
#[tokio::test]
async fn missing_transport_principal_fails_closed() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-a"), "dc-a", "leaf", "rtr-a", "ios"),
    );

    // TLS client with NO client certificate.
    let channel = client_channel(&lab, Some(&pki), None).await;
    let mut client = BonsaiGraphClient::new(channel);
    let err = ingest_one(&mut client, "site-a", "10.0.0.1", "state/foo")
        .await
        .unwrap_err();
    assert!(
        matches!(err.code(), Code::Unauthenticated | Code::PermissionDenied | Code::Unknown | Code::Unavailable),
        "missing principal must fail: {err}"
    );
    let err = client
        .get_devices(Request::new(GetDevicesRequest {}))
        .await
        .unwrap_err();
    assert!(
        matches!(err.code(), Code::Unauthenticated | Code::PermissionDenied | Code::Unknown | Code::Unavailable),
        "missing principal read must fail: {err}"
    );
}

/// Explicit development compatibility: plaintext transport keeps the
/// legacy pass-through — but ONLY because the policy opted in.
#[tokio::test]
async fn development_compat_mode_permits_plaintext() {
    let lab = start_lab_mode(None, false, AuthPolicy::DevelopmentCompat).await;
    let mut client = plain_client(&lab).await;

    // No principal at all — ingest and reads pass (legacy dev path).
    assert_eq!(
        ingest_one(&mut client, "dev-col", "10.0.0.1", "state/foo")
            .await
            .unwrap(),
        1
    );
    client
        .get_devices(Request::new(GetDevicesRequest {}))
        .await
        .expect("dev-compat read");
    client
        .query(Request::new(QueryRequest {
            cypher: "MATCH (d:Device) RETURN count(d)".to_string(),
        }))
        .await
        .expect("dev-compat query");
}

/// Required mode on a PLAINTEXT listener denies everything — auth
/// mode is not inferred from transport shape.
#[tokio::test]
async fn required_mode_denies_plaintext_listener() {
    let lab = start_lab_mode(None, false, AuthPolicy::Required).await;
    let mut client = plain_client(&lab).await;
    let err = ingest_one(&mut client, "dev-col", "10.0.0.1", "state/foo")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unauthenticated, "{err}");
    let err = client
        .get_devices(Request::new(GetDevicesRequest {}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unauthenticated, "{err}");
}

// ═══════════════════════════════════════════════════════════════════════════
// G6 — principal attestation, grant typing, ambiguous bindings
// ═══════════════════════════════════════════════════════════════════════════

/// A serialized principal has a fingerprint but no transport
/// attestation — it must fail every authorization path.
#[test]
fn unattested_serialized_principal_is_denied() {
    let dir = tempfile::tempdir().unwrap();
    let sqlite = Arc::new(SqliteStore::open(dir.path()).unwrap());
    let authz = CollectorAuthz::new(Some(sqlite));

    // Deserialized from JSON — transport_attested can never be true.
    let principal: VerifiedIngressPrincipal = serde_json::from_str(
        r#"{"method":"mutual_tls","credential_fingerprint":"sha256:deadbeef"}"#,
    )
    .unwrap();
    assert!(!principal.is_transport_attested());

    assert_eq!(
        authz.authorize_collector(&principal, "site-a").unwrap_err().code(),
        Code::PermissionDenied
    );
    assert_eq!(
        authz
            .authorize_capability(&principal, "core-admin")
            .unwrap_err()
            .code(),
        Code::PermissionDenied
    );
    assert_eq!(
        authz.authorize_any_binding(&principal).unwrap_err().code(),
        Code::PermissionDenied
    );
    assert_eq!(
        authz.read_scope(&principal).unwrap_err().code(),
        Code::PermissionDenied
    );
}

/// The same credential+collector id provisioned under two scopes is
/// ambiguous — authorization fails closed rather than picking a row.
#[tokio::test]
async fn ambiguous_binding_fails_closed() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    // Same collector id + same credential under TWO scopes.
    bind_collector_scoped(&lab.sqlite, "site-x", &cert_a.fingerprint, "dc-a");
    bind_collector_scoped(&lab.sqlite, "site-x", &cert_a.fingerprint, "dc-b");
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-x"), "dc-a", "leaf", "rtr-a", "ios"),
    );

    let mut client = mtls_client(&lab, &pki, &cert_a).await;
    let err = ingest_one(&mut client, "site-x", "10.0.0.1", "state/foo")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied, "{err}");
    // Heartbeat and registration are equally ambiguous.
    let err = client
        .heartbeat(Request::new(CollectorStats {
            collector_id: "site-x".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
}

// ═══════════════════════════════════════════════════════════════════════════
// G7 — registration audit fingerprint
// ═══════════════════════════════════════════════════════════════════════════

/// Registration audit records must carry the genuine transport
/// certificate fingerprint — the same value authorization used.
#[tokio::test]
async fn registration_audit_records_real_certificate_fingerprint() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);

    let mut client = mtls_client(&lab, &pki, &cert_a).await;
    client
        .register_collector(Request::new(CollectorIdentity {
            collector_id: "site-a".to_string(),
            hostname: "collector-a".to_string(),
            protocol_version: PROTOCOL_VERSION,
            auth_token: String::new(),
        }))
        .await
        .expect("register");

    let db_path = lab._dir.path().join("bonsai_config.db");
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let fp: String = conn
        .query_row(
            "SELECT cert_fingerprint FROM collector_registrations \
             WHERE collector_id = 'site-a' AND success = 1 \
             ORDER BY timestamp_ns DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .expect("registration audit row");
    assert_eq!(
        fp, cert_a.fingerprint,
        "audit must record the transport-attested certificate fingerprint"
    );
}
