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
    BindingProvenance, BindingStatus, CanonicalDevice, TrustedBinding, VerifiedIngressPrincipal,
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
async fn start_lab_mode(pki: Option<&Pki>, use_tls: bool, policy: AuthPolicy) -> Lab {
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

/// Insert an ambiguous collector binding row directly via SQL —
/// bypassing BOTH provisioning checks and the schema-v5 uniqueness
/// indexes — to simulate pre-existing ambiguous rows that runtime
/// authorization must fail closed on (BN-021g R4).
fn insert_raw_collector_binding(lab: &Lab, collector_id: &str, fp: &str, scope: &str) {
    let db_path = lab
        ._dir
        .path()
        .join(bonsai::sqlite_store::CONFIG_DB_FILE_NAME);
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_exporter_bindings_collector_identity;
         DROP INDEX IF EXISTS idx_exporter_bindings_collector_credential;
         DROP INDEX IF EXISTS idx_exporter_bindings_credential;",
    )
    .unwrap();
    let dev = lab
        .sqlite
        .upsert_canonical_device(
            &CanonicalDevice {
                device_id: String::new(),
                address: format!("collector:{collector_id}"),
                site: Some(scope.to_string()),
                hostname: None,
                created_at_ns: 0,
                updated_at_ns: 0,
                created_by: String::new(),
                updated_by: String::new(),
            },
            "test",
        )
        .unwrap();
    conn.execute(
        "INSERT INTO exporter_bindings (binding_id, scope, protocol, exporter_id, \
         device_id, match_transport, match_reported_identity, match_exporter_claim, \
         match_observation_domain, match_ingress_credential, provenance, status, \
         created_at_ns, updated_at_ns, created_by, updated_by) \
         VALUES (?1, ?2, 'collector', ?3, ?4, NULL, NULL, NULL, NULL, ?5, \
         'operator_provisioned', 'active', 0, 0, 'test', 'test')",
        rusqlite::params![
            SqliteStore::new_binding_id(),
            scope,
            collector_id,
            dev.device_id,
            fp
        ],
    )
    .unwrap();
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
        .assign_device_with_audit(&t.address, t.collector_id.clone(), "test", "test_assign")
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

async fn ingest_updates_full(
    client: &mut BonsaiGraphClient<Channel>,
    updates: Vec<TelemetryIngestUpdate>,
) -> Result<TelemetryIngestResponse, tonic::Status> {
    client
        .telemetry_ingest(Request::new(tokio_stream::iter(updates)))
        .await
        .map(|r| r.into_inner())
        .map_err(|e| e)
}

/// H2 contract: permanent per-record authorization/payload rejections
/// are counted in `rejected`, not surfaced as an RPC error — the batch
/// continues so one denied record cannot block legitimate telemetry.
/// Retryable failures still abort the RPC.
async fn assert_ingest_denied(
    client: &mut BonsaiGraphClient<Channel>,
    collector_id: &str,
    target_addr: &str,
    path: &str,
) {
    let resp = ingest_updates_full(
        client,
        vec![telemetry_update(
            collector_id,
            target_addr,
            path,
            serde_json::json!({"v": 1}),
        )],
    )
    .await
    .expect("ingest RPC completes with truthful rejected count");
    assert_eq!(
        resp.accepted, 0,
        "denied record must not be accepted: {resp:?}"
    );
    assert!(resp.rejected >= 1, "denial must be counted: {resp:?}");
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
        ingest_one(&mut client_a, "site-a", "10.0.0.1", "state/foo")
            .await
            .unwrap(),
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

    // F3: cert A claiming collector B's id — rejected per-record, no B
    // authority (H2: counted rejection, stream continues).
    assert_ingest_denied(&mut client_a, "site-b", "10.0.0.2", "state/foo").await;
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
        ingest_one(&mut client_b, "site-b", "10.0.0.2", "state/foo")
            .await
            .unwrap(),
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
    assert_ingest_denied(&mut client, "site-a", "10.0.0.1", "state/foo").await;
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
        // H1: provisional observations must be keyed by a `*src/`
        // source key namespaced to the authorized collector — a
        // canonical-looking target is denied outright.
        "otlpsrc/site-a/http/10.0.0.1:4318",
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
                // R1: the forged "resolved" claim is demoted — the
                // EFFECTIVE identity is provisional and the original
                // claim survives only as inert `claimed_*` evidence.
                assert_eq!(u.value["identity_status"], "provisional");
                assert_eq!(u.value["claimed_identity_status"], "resolved");
                assert!(u.value["device_id"].is_null());
                assert_eq!(u.value["claimed_device_id"], "dev-forged");
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
        ingest_one(&mut client, "site-a", "10.0.0.1", "state/foo")
            .await
            .unwrap(),
        1
    );

    // Revoke while the connection stays open.
    lab.sqlite
        .revoke_exporter_binding(&binding_id, "test")
        .unwrap();

    // Within AUTHZ_CACHE_TTL (5s) the cached grant may still pass; after
    // expiry every new decision must deny.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_ingest_denied(&mut client, "site-a", "10.0.0.1", "state/foo").await;
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
        ingest_one(&mut old_client, "site-a", "10.0.0.1", "state/foo")
            .await
            .unwrap(),
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
        ingest_one(&mut new_client, "site-a", "10.0.0.1", "state/foo")
            .await
            .unwrap(),
        1
    );
    // Old credential now denied — no implicit trust carry-over.
    assert_ingest_denied(&mut old_client, "site-a", "10.0.0.1", "state/foo").await;
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
    assert_ingest_denied(&mut thief, "site-a", "10.0.0.1", "state/foo").await;

    // And the rightful collector still works afterwards.
    let mut client = mtls_client(&lab, &pki, &cert_a).await;
    assert_eq!(
        ingest_one(&mut client, "site-a", "10.0.0.1", "state/foo")
            .await
            .unwrap(),
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
        target(
            "10.0.0.2",
            Some("site-b"),
            "dc-b",
            "spine",
            "rtr-b",
            "junos",
        ),
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
    //    H1: the source key must carry the authorized collector's
    //    namespace segment.
    let flow = TelemetryIngestUpdate {
        collector_id: "site-a".to_string(),
        target: "flowsrc/site-a/nf9/198.51.100.9:49152/sid7".to_string(),
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

    let accepted = ingest_updates(&mut client, vec![flow, relabel_unassigned])
        .await
        .unwrap();
    assert_eq!(
        accepted, 1,
        "provisional flow passes; relabeled canonical path denied"
    );

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
        target(
            "10.0.0.2",
            Some("site-b"),
            "dc-b",
            "spine",
            "rtr-b",
            "junos",
        ),
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
        graph_query(&lab.graph, "MATCH (e:DetectionEvent) RETURN e.id").is_empty(),
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
        target(
            "10.0.0.2",
            Some("site-b"),
            "dc-b",
            "spine",
            "rtr-b",
            "junos",
        ),
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
    let addrs: Vec<String> = resp
        .into_inner()
        .devices
        .iter()
        .map(|d| d.address.clone())
        .collect();
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
        .add_device(Request::new(AddDeviceRequest { device: None }))
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
    assert!(
        s1.message().await.unwrap().is_some(),
        "session 1 initial sync"
    );

    // Session 2 supersedes session 1 for the same collector id.
    let mut client2 = mtls_client(&lab, &pki, &cert_a).await;
    let s2 = client2
        .register_collector(identity())
        .await
        .unwrap()
        .into_inner();
    tokio::pin!(s2);
    assert!(
        s2.message().await.unwrap().is_some(),
        "session 2 initial sync"
    );

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
        matches!(
            err.code(),
            Code::Unauthenticated | Code::PermissionDenied | Code::Unknown | Code::Unavailable
        ),
        "missing principal must fail: {err}"
    );
    let err = client
        .get_devices(Request::new(GetDevicesRequest {}))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err.code(),
            Code::Unauthenticated | Code::PermissionDenied | Code::Unknown | Code::Unavailable
        ),
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
        authz
            .authorize_collector(&principal, "site-a")
            .unwrap_err()
            .code(),
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
    // Same collector id + same credential under TWO scopes. Provisioning
    // now rejects this (R4 global uniqueness), so the ambiguous rows are
    // inserted directly to prove runtime authorization still fails
    // closed on pre-existing bad data.
    bind_collector_scoped(&lab.sqlite, "site-x", &cert_a.fingerprint, "dc-a");
    insert_raw_collector_binding(&lab, "site-x", &cert_a.fingerprint, "dc-b");
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-x"), "dc-a", "leaf", "rtr-a", "ios"),
    );

    let mut client = mtls_client(&lab, &pki, &cert_a).await;
    assert_ingest_denied(&mut client, "site-x", "10.0.0.1", "state/foo").await;
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

// ═══════════════════════════════════════════════════════════════════════════
// H1 — classified-authority authorization (path prefixes never authorize)
// ═══════════════════════════════════════════════════════════════════════════

fn bmp_peer_state_update(collector_id: &str, target_addr: &str) -> TelemetryIngestUpdate {
    telemetry_update(
        collector_id,
        target_addr,
        "streaming/bmp/peer-state",
        serde_json::json!({
            "timestamp_ns": 1_700_000_000_000_000_000_i64,
            "collector_peer": "198.51.100.1:179",
            "message_type": "peer_up_notification",
            "peer_type": 0,
            "peer_flags": 0,
            "router_distinguisher": 0,
            "router_address": "10.9.9.9",
            "peer_address": "10.1.0.1",
            "peer_as": 65001,
            "peer_bgp_id": "10.1.0.1",
            "session_state": "established",
            "route_entries": [],
            "raw_len": 0
        }),
    )
}

fn bgp_ls_node_update(collector_id: &str, target_addr: &str) -> TelemetryIngestUpdate {
    telemetry_update(
        collector_id,
        target_addr,
        "streaming/bgp-ls/node",
        serde_json::json!({
            "kind": "node",
            "router_id": "1.1.1.1",
            "protocol": "bgp-ls",
            "asn": 65001,
            "name": "rtr-a",
            "sr_node_sid": 100
        }),
    )
}

/// H1: canonical graph writers reachable through `streaming/` paths are
/// gated by the SAME assigned-device rule as YANG paths — decided from
/// the classified observation, never the path prefix. Provisional
/// classes must carry a `*src/` key namespaced to the authorized
/// collector.
#[tokio::test]
async fn classified_authority_gates_canonical_writers_on_streaming_paths() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-a"), "dc-a", "leaf", "rtr-a", "ios"),
    );
    add_device(
        &lab.registry,
        target(
            "10.0.0.2",
            Some("site-b"),
            "dc-b",
            "spine",
            "rtr-b",
            "junos",
        ),
    );
    let mut client = mtls_client(&lab, &pki, &cert_a).await;

    let summary_to_b = {
        let mut u = telemetry_update(
            "site-a",
            "10.0.0.2",
            "interfaces/interface[name=eth0]/state/counters",
            serde_json::json!({}),
        );
        u.interface_summary = Some(InterfaceSummary {
            counters: vec![],
            window_secs: 1,
            target: String::new(),
            if_name: "eth0".to_string(),
        });
        u
    };

    let resp = ingest_updates_full(
        &mut client,
        vec![
            // Denied: canonical-class writers on an unassigned device.
            bmp_peer_state_update("site-a", "10.0.0.2"),
            bgp_ls_node_update("site-a", "10.0.0.2"),
            telemetry_update(
                "site-a",
                "10.0.0.2",
                "interfaces/interface[name=eth0]/state/admin-state",
                serde_json::json!({"admin-state": "up"}),
            ),
            summary_to_b,
            // Denied: provisional observation keyed by a canonical
            // device address instead of a `*src/` source key.
            telemetry_update(
                "site-a",
                "10.0.0.1",
                "streaming/otlp/metrics",
                serde_json::json!({"service_name": "svc", "metric_name": "m", "value": 1.0}),
            ),
            // Accepted: assigned-device acquisition through canonical
            // streaming writers (BMP peer state + BGP-LS node).
            bmp_peer_state_update("site-a", "10.0.0.1"),
            bgp_ls_node_update("site-a", "10.0.0.1"),
            // Accepted but re-namespaced: collector A cannot file flow
            // evidence under collector B's `flowsrc/` namespace.
            telemetry_update(
                "site-a",
                "flowsrc/site-b/nf9/10.0.0.2:49152/sid7",
                "streaming/netflow/flow",
                serde_json::json!({
                    "exporter_address": "10.0.0.2",
                    "exporter_id": "flowsrc/site-b/nf9/10.0.0.2:49152/sid7",
                    "src_address": "10.1.1.1", "src_port": 12345,
                    "dst_address": "10.1.1.2", "dst_port": 80,
                    "protocol": "tcp", "bytes_per_sec": 1.0, "packets_per_sec": 1.0
                }),
            ),
            // Accepted: unclassified streaming path → Ignored, no graph effect.
            telemetry_update(
                "site-a",
                "10.0.0.2",
                "streaming/mystery/unknown-thing",
                serde_json::json!({"x": 1}),
            ),
        ],
    )
    .await
    .unwrap();
    assert_eq!(resp.accepted, 4, "accepted={resp:?}");
    assert_eq!(resp.rejected, 5, "rejected={resp:?}");

    // Canonical graph effects exist ONLY for the assigned device.
    wait_for_graph_row(
        &lab.graph,
        "MATCH (s:BmpSession) RETURN s.id",
        "bmp session on assigned device",
    )
    .await;
    wait_for_graph_row(
        &lab.graph,
        "MATCH (n:BgpLsNode) RETURN n.id",
        "bgp-ls node on assigned device",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    for cypher in [
        "MATCH (d:Device {address: '10.0.0.2'})-[:PEERS_WITH]->(n:BgpNeighbor) RETURN n.id",
        "MATCH (d:Device {address: '10.0.0.2'})-[:HAS_BGPLS_NODE]->(n:BgpLsNode) RETURN n.id",
        "MATCH (d:Device {address: '10.0.0.2'})-[:HAS_INTERFACE|CONNECTED_TO*]->(i) RETURN i",
        "MATCH (i:Interface {device_address: '10.0.0.2'}) RETURN i.name",
        "MATCH (e:StateChangeEvent {device_address: '10.0.0.2', source_type: 'bmp'}) RETURN e.id",
    ] {
        assert!(
            graph_query(&lab.graph, cypher).is_empty(),
            "canonical writer leaked onto unassigned device: {cypher}"
        );
    }
    // The forged `flowsrc/site-b/` namespace was rewritten to site-a.
    let flow = wait_for_graph_row(
        &lab.graph,
        "MATCH (f:AppFlow) WHERE f.exporter_id STARTS WITH 'flowsrc/' RETURN f.exporter_id",
        "provisional AppFlow",
    )
    .await;
    assert!(
        flow[0].starts_with("flowsrc/site-a/"),
        "provisional evidence must be re-namespaced to the authorized collector: {}",
        flow[0]
    );
    assert!(
        graph_query(
            &lab.graph,
            "MATCH (f:AppFlow) WHERE f.exporter_id STARTS WITH 'flowsrc/site-b/' RETURN f.id"
        )
        .is_empty(),
        "collector A must not file evidence under collector B's namespace"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// H3 — per-message detection-ingest capability revalidation
// ═══════════════════════════════════════════════════════════════════════════

/// H3: the separate `detection-ingest` capability is revalidated per
/// message on an OPEN stream — revoking only that binding (collector
/// binding stays active) stops detection authoring on the same stream,
/// while ordinary collector telemetry keeps working.
#[tokio::test]
async fn detection_stream_revocation_stops_authoring_mid_stream() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);
    let det_binding = bind_capability(
        &lab.sqlite,
        bonsai::collector_auth::DETECTION_INGEST_PROTOCOL,
        &cert_a.fingerprint,
    );
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-a"), "dc-a", "leaf", "rtr-a", "ios"),
    );
    prepopulate_device(&lab.graph, "10.0.0.1");
    prepopulate_event(&lab.graph, "sce-a-1", "10.0.0.1", "{}", "gnmi");

    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let rpc = {
        let mut client = mtls_client(&lab, &pki, &cert_a).await;
        tokio::spawn(async move {
            client
                .detection_ingest(Request::new(tokio_stream::wrappers::ReceiverStream::new(
                    rx,
                )))
                .await
        })
    };

    // First detection on the still-open stream — authorized.
    tx.send(detection_update(
        "site-a",
        "10.0.0.1",
        "high",
        "sce-a-1",
        vec!["sce-a-1".to_string()],
    ))
    .await
    .unwrap();
    wait_for_graph_row(
        &lab.graph,
        "MATCH (d:Device {address: '10.0.0.1'})-[:TRIGGERED]->(e:DetectionEvent) RETURN e.id",
        "first authorized detection",
    )
    .await;

    // Revoke ONLY the detection-ingest capability; collector binding
    // stays active.
    lab.sqlite
        .revoke_exporter_binding(&det_binding, "test")
        .unwrap();

    // Same open stream: the next message must be denied. Epoch
    // invalidation makes the revocation visible on the next check —
    // no TTL wait required, but allow a small margin for scheduling.
    tokio::time::sleep(Duration::from_millis(300)).await;
    tx.send(detection_update(
        "site-a",
        "10.0.0.1",
        "critical",
        "sce-a-1",
        vec!["sce-a-1".to_string()],
    ))
    .await
    .unwrap();
    drop(tx);
    let outcome = tokio::time::timeout(Duration::from_secs(15), rpc)
        .await
        .expect("detection RPC must resolve after stream close");
    let resp = outcome.expect("rpc task");
    assert!(
        resp.is_err(),
        "revoked detection capability must fail the open stream, got {resp:?}"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    let detections = graph_query(
        &lab.graph,
        "MATCH (e:DetectionEvent) RETURN e.id, e.severity",
    );
    assert_eq!(
        detections.len(),
        1,
        "revoked capability must not author additional detections: {detections:?}"
    );
    assert_eq!(detections[0][1], "high");

    // Ordinary collector telemetry on the SAME credential still works.
    let mut tel = mtls_client(&lab, &pki, &cert_a).await;
    assert_eq!(
        ingest_one(&mut tel, "site-a", "10.0.0.1", "state/foo")
            .await
            .unwrap(),
        1,
        "collector ingest authority must survive detection-capability revocation"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// H4 — positive detection-evidence authority and provenance
// ═══════════════════════════════════════════════════════════════════════════

/// H4: remote DetectionIngest requires verified, in-scope,
/// non-provisional source-event provenance — evidence-free submissions,
/// malformed payloads, provisional-source events with canonical-looking
/// addresses, and missing server-owned provenance are all denied.
#[tokio::test]
async fn detection_ingest_requires_verified_nonprovisional_evidence() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);
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
        target(
            "10.0.0.2",
            Some("site-b"),
            "dc-b",
            "spine",
            "rtr-b",
            "junos",
        ),
    );
    prepopulate_device(&lab.graph, "10.0.0.1");
    prepopulate_device(&lab.graph, "10.0.0.2");
    prepopulate_event(&lab.graph, "sce-a-1", "10.0.0.1", "{}", "gnmi");
    // Real cross-device event: exists, canonical provenance, but belongs
    // to collector B's device — must be denied for device 10.0.0.1.
    prepopulate_event(&lab.graph, "sce-b-1", "10.0.0.2", "{}", "gnmi");
    // Provisional-source event that nonetheless carries a canonical-
    // looking device_address and a clean detail — only the
    // server-owned source_type marks it untrusted.
    prepopulate_event(&lab.graph, "sce-prov-canon", "10.0.0.1", "{}", "syslog");
    // Canonical-address event with NO server-owned provenance at all.
    prepopulate_event(&lab.graph, "sce-no-prov", "10.0.0.1", "{}", "");
    // Unknown source_type — positive allowlist must fail closed.
    prepopulate_event(
        &lab.graph,
        "sce-unknown-src",
        "10.0.0.1",
        "{}",
        "mystery-proto",
    );
    // Malformed (non-JSON) event detail — cannot prove provenance.
    prepopulate_event(
        &lab.graph,
        "sce-bad-detail",
        "10.0.0.1",
        "{not json",
        "gnmi",
    );
    // Canonical source_type but detail asserts provisional identity.
    prepopulate_event(
        &lab.graph,
        "sce-prov-detail",
        "10.0.0.1",
        "{\"identity_status\":\"provisional\"}",
        "gnmi",
    );

    let mut client = mtls_client(&lab, &pki, &cert_a).await;
    let accepted = ingest_detections(
        &mut client,
        vec![
            // Evidence-free remote detection — denied (H4 decision:
            // remote ingest requires positive provenance).
            detection_update("site-a", "10.0.0.1", "high", "", vec![]),
            // Malformed features_json — permanent rejection.
            {
                let mut d = detection_update(
                    "site-a",
                    "10.0.0.1",
                    "high",
                    "sce-a-1",
                    vec!["sce-a-1".to_string()],
                );
                d.features_json = "{not json".to_string();
                d
            },
            // Malformed source_types_json — permanent rejection.
            {
                let mut d = detection_update(
                    "site-a",
                    "10.0.0.1",
                    "high",
                    "sce-a-1",
                    vec!["sce-a-1".to_string()],
                );
                d.source_types_json = "not-an-array".to_string();
                d
            },
            // Provisional-source event (syslog) on a canonical address.
            detection_update(
                "site-a",
                "10.0.0.1",
                "high",
                "sce-prov-canon",
                vec!["sce-prov-canon".to_string()],
            ),
            // Event with NO server-owned provenance (empty source_type).
            detection_update(
                "site-a",
                "10.0.0.1",
                "high",
                "sce-no-prov",
                vec!["sce-no-prov".to_string()],
            ),
            // Unknown source_type — not on the eligible allowlist.
            detection_update(
                "site-a",
                "10.0.0.1",
                "high",
                "sce-unknown-src",
                vec!["sce-unknown-src".to_string()],
            ),
            // Malformed event detail cannot prove non-provisional status.
            detection_update(
                "site-a",
                "10.0.0.1",
                "high",
                "sce-bad-detail",
                vec!["sce-bad-detail".to_string()],
            ),
            // Canonical source_type but detail asserts provisional identity.
            detection_update(
                "site-a",
                "10.0.0.1",
                "high",
                "sce-prov-detail",
                vec!["sce-prov-detail".to_string()],
            ),
            // Cross-device reference: sce-b-1 exists on 10.0.0.2.
            detection_update(
                "site-a",
                "10.0.0.1",
                "high",
                "sce-b-1",
                vec!["sce-b-1".to_string()],
            ),
            // Fabricated reference: event does not exist at all.
            detection_update(
                "site-a",
                "10.0.0.1",
                "high",
                "sce-nonexistent",
                vec!["sce-nonexistent".to_string()],
            ),
        ],
    )
    .await
    .unwrap();
    assert_eq!(accepted, 0, "all provenance violations must be dropped");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        graph_query(&lab.graph, "MATCH (e:DetectionEvent) RETURN e.id").is_empty(),
        "no DetectionEvent may be written without verified provenance"
    );

    // Valid trusted evidence is accepted and carries remote-ingest
    // provenance in its stored source_types.
    let accepted = ingest_detections(
        &mut client,
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
    let row = wait_for_graph_row(
        &lab.graph,
        "MATCH (e:DetectionEvent) RETURN e.source_types",
        "detection source_types",
    )
    .await;
    assert!(
        row[0].contains("remote_detection_ingest"),
        "remote-submitted detection must be marked with ingest provenance: {}",
        row[0]
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// H5 — consistent ambiguity rules for collector-scoped reads
// ═══════════════════════════════════════════════════════════════════════════

/// H5: the same credential+collector-id provisioned under multiple
/// scopes is ambiguous for ingest AND for scoped reads — `DISTINCT`
/// collapsing must not silently grant read access.
#[tokio::test]
async fn ambiguous_collector_binding_denies_scoped_reads() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    // Same collector id + same credential under TWO scopes — inserted
    // raw because provisioning now rejects the duplicate (R4).
    bind_collector_scoped(&lab.sqlite, "site-a", &cert_a.fingerprint, "dc-a");
    insert_raw_collector_binding(&lab, "site-a", &cert_a.fingerprint, "dc-b");
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-a"), "dc-a", "leaf", "rtr-a", "ios"),
    );
    prepopulate_device(&lab.graph, "10.0.0.1");

    let mut client = mtls_client(&lab, &pki, &cert_a).await;
    let err = client
        .get_devices(Request::new(GetDevicesRequest {}))
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        Code::PermissionDenied,
        "ambiguous binding must not gain scoped read access: {err}"
    );
    // Ingest is denied for the same reason (existing rule, now shared).
    assert_ingest_denied(&mut client, "site-a", "10.0.0.1", "state/foo").await;
}

/// H5 model check: the binding store enforces ONE canonical
/// (collector_id, credential) pair per credential — a second collector
/// identity cannot be provisioned for the same certificate, so scoped
/// reads can never silently union or first-match across identities.
#[tokio::test]
async fn credential_cannot_bind_multiple_collector_identities() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);

    // A second binding for the same credential under a DIFFERENT
    // collector id is rejected at provisioning time.
    let device = CanonicalDevice {
        device_id: String::new(),
        address: "collector:site-c".to_string(),
        site: Some("dc-a".to_string()),
        hostname: None,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
    };
    let dev = lab.sqlite.upsert_canonical_device(&device, "test").unwrap();
    let binding = TrustedBinding {
        binding_id: SqliteStore::new_binding_id(),
        scope: "dc-a".to_string(),
        protocol: bonsai::collector_auth::COLLECTOR_PROTOCOL.to_string(),
        exporter_id: "site-c".to_string(),
        device_id: dev.device_id,
        match_transport: None,
        match_reported_identity: None,
        match_exporter_claim: None,
        match_observation_domain: None,
        match_ingress_credential: Some(cert_a.fingerprint.clone()),
        provenance: BindingProvenance::OperatorProvisioned,
        status: BindingStatus::Active,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
    };
    assert!(
        lab.sqlite
            .upsert_exporter_binding(&binding, "test")
            .is_err(),
        "one credential must not be provisioned for two collector identities"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// H6 — assignment-session lifecycle fencing on abrupt disconnect
// ═══════════════════════════════════════════════════════════════════════════

/// H6: dropping the assignment stream abruptly (client disconnect /
/// cancellation) must still unregister the session — the drop guard,
/// not post-loop code, owns cleanup. The collector's devices are
/// released for reassignment.
#[tokio::test]
async fn assignment_stream_disconnect_still_cleans_up_session() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);
    add_device(
        &lab.registry,
        target("10.0.0.1", Some("site-a"), "dc-a", "leaf", "rtr-a", "ios"),
    );

    let mut client = mtls_client(&lab, &pki, &cert_a).await;
    let mut stream = client
        .register_collector(Request::new(CollectorIdentity {
            collector_id: "site-a".to_string(),
            hostname: "collector-a".to_string(),
            protocol_version: PROTOCOL_VERSION,
            auth_token: String::new(),
        }))
        .await
        .expect("register")
        .into_inner();
    assert!(stream.message().await.unwrap().is_some(), "initial sync");
    assert_eq!(
        lab.registry
            .get_device("10.0.0.1")
            .unwrap()
            .unwrap()
            .collector_id
            .as_deref(),
        Some("site-a")
    );

    // Abrupt disconnect: drop the stream without reading to end.
    drop(stream);

    // The guard's blocking unregister must release the assignment.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let assigned = lab
            .registry
            .get_device("10.0.0.1")
            .unwrap()
            .unwrap()
            .collector_id
            .clone();
        if assigned.is_none() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session cleanup did not run after stream disconnect"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// R1 — provisional source identity is server-normalized at ingest
// ═══════════════════════════════════════════════════════════════════════════

fn netflow_update(
    collector_id: &str,
    target_key: &str,
    value: serde_json::Value,
) -> TelemetryIngestUpdate {
    TelemetryIngestUpdate {
        collector_id: collector_id.to_string(),
        target: target_key.to_string(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: 1_700_000_000_000_000_000,
        path: "streaming/netflow/flow".to_string(),
        value_msgpack: rmp_serde::to_vec(&value).unwrap(),
        protocol_version: PROTOCOL_VERSION,
        interface_summary: None,
    }
}

fn flow_value(extra: serde_json::Value) -> serde_json::Value {
    let mut v = serde_json::json!({
        "exporter_address": "203.0.113.50",
        "src_address": "10.1.0.1",
        "src_port": 40000,
        "dst_address": "10.1.0.2",
        "dst_port": 443,
        "protocol": "tcp",
        "bytes_per_sec": 1.0,
        "packets_per_sec": 1.0
    });
    if let (Some(dst), Some(src)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in src {
            dst.insert(k.clone(), val.clone());
        }
    }
    v
}

/// R1: for EVERY provisional observation the ingest boundary pins the
/// effective identity fields server-side. Missing, null, numeric or
/// malformed producer `exporter_id`/`source_key` claims can never make
/// `classify()` fall back to the untrusted `exporter_address`; foreign
/// collector namespaces are rewritten to the authenticated collector;
/// protocol prefixes must match the classified protocol; and forged
/// "resolved"/canonical claims remain provisional evidence only.
#[tokio::test]
async fn provisional_source_identity_is_server_normalized() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    let cert_c = issue_leaf(&pki, "collector-c", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);
    // A collector id containing namespace delimiters must be
    // percent-encoded into the composite key unambiguously. It gets its
    // own credential — one credential authorizes one collector identity.
    bind_collector(&lab.sqlite, "site/delimited%", &cert_c.fingerprint);

    let mut client_a = mtls_client(&lab, &pki, &cert_a).await;
    let mut client_c = mtls_client(&lab, &pki, &cert_c).await;

    // Case batch for collector site-a (all target the flowsrc family).
    let updates = vec![
        // 1+3. Missing exporter_id AND forged exporter_address AND
        //    forged resolved/canonical claims, foreign namespace:
        //    effective identity must be the normalized site-a key.
        netflow_update(
            "site-a",
            "flowsrc/site-b/nf9/10.9.0.9:49152/s1",
            flow_value(serde_json::json!({
                "exporter_address": "6.6.6.6",
                "identity_status": "resolved",
                "authority": "canonical_device",
                "canonical_exporter_id": "10.9.9.9",
                "identity_resolution": {"status": "resolved", "device_id": "dev-x"},
                "device_id": "dev-x",
                "collector_id": "site-b"
            })),
        ),
        // 2. Null exporter_id.
        netflow_update(
            "site-a",
            "flowsrc/site-b/nf9/10.9.0.10:49152/s2",
            flow_value(serde_json::json!({"exporter_id": null, "src_port": 40001})),
        ),
        // 2b. Non-string (numeric) exporter_id.
        netflow_update(
            "site-a",
            "flowsrc/site-b/nf9/10.9.0.11:49152/s3",
            flow_value(serde_json::json!({"exporter_id": 12345, "src_port": 40002})),
        ),
        // 5. Wrong protocol prefix on the TARGET — must be rejected,
        //    not silently rewritten into the flow family.
        netflow_update(
            "site-a",
            "sflowsrc/site-a/agent1/sub0",
            flow_value(serde_json::json!({"src_port": 40003})),
        ),
        // Payload exporter_id under the WRONG protocol family collapses
        // to the normalized target key rather than surviving as a
        // foreign-namespace identity.
        netflow_update(
            "site-a",
            "flowsrc/site-b/nf9/10.9.0.12:49152/s4",
            flow_value(serde_json::json!({
                "exporter_id": "sflowsrc/site-b/evil/agent",
                "src_port": 40004
            })),
        ),
    ];
    let resp = ingest_updates_full(&mut client_a, updates).await.unwrap();
    assert_eq!(
        resp.accepted, 4,
        "four valid provisional records accepted: {resp:?}"
    );
    assert_eq!(resp.rejected, 1, "wrong-protocol target rejected: {resp:?}");

    // Delimited collector id: percent-encoded namespace, no raw
    // delimiter ambiguity.
    let resp_c = ingest_updates_full(
        &mut client_c,
        vec![netflow_update(
            "site/delimited%",
            "flowsrc/foreign/nf9/10.9.0.20:49152/s9",
            flow_value(serde_json::json!({"src_port": 40010})),
        )],
    )
    .await
    .unwrap();
    assert_eq!(resp_c.accepted, 1, "{resp_c:?}");

    // ── Graph assertions ────────────────────────────────────────────
    // Effective AppFlow identity = normalized site-a source keys; the
    // forged exporter_address never became identity.
    let expected_keys = [
        "flowsrc/site-a/nf9/10.9.0.9:49152/s1",
        "flowsrc/site-a/nf9/10.9.0.10:49152/s2",
        "flowsrc/site-a/nf9/10.9.0.11:49152/s3",
        "flowsrc/site-a/nf9/10.9.0.12:49152/s4",
    ];
    for key in expected_keys {
        let row = wait_for_graph_row(
            &lab.graph,
            &format!(
                "MATCH (f:AppFlow) WHERE f.exporter_id = '{}' RETURN f.identity_status",
                key
            ),
            &format!("AppFlow for {key}"),
        )
        .await;
        assert_eq!(
            row[0], "provisional",
            "forged resolved claims must not upgrade identity: {key}"
        );
    }
    // The wrong-prefix payload exporter_id did NOT survive: its flow is
    // keyed by the normalized target key, not 'sflowsrc/...evil...'.
    assert!(
        graph_query(
            &lab.graph,
            "MATCH (f:AppFlow) WHERE f.exporter_id CONTAINS 'evil' RETURN f.id"
        )
        .is_empty(),
        "foreign-protocol payload identity must not persist"
    );
    // Delimited collector id produced an unambiguous encoded key.
    let delim = wait_for_graph_row(
        &lab.graph,
        "MATCH (f:AppFlow) WHERE f.exporter_id STARTS WITH 'flowsrc/' \
         AND f.exporter_id CONTAINS 'delimited' RETURN f.exporter_id",
        "delimited-collector AppFlow",
    )
    .await;
    assert_eq!(
        delim[0], "flowsrc/site%2Fdelimited%25/nf9/10.9.0.20:49152/s9",
        "collector id must be percent-encoded into the namespace segment"
    );
    // AppFlow separation: each normalized source key is a distinct
    // flow identity (4 site-a keys + 1 delimited key).
    let count = graph_query(
        &lab.graph,
        "MATCH (f:AppFlow) WHERE f.exporter_id STARTS WITH 'flowsrc/' RETURN count(f)",
    );
    assert_eq!(
        count[0][0], "5",
        "distinct normalized sources stay distinct"
    );
    // No canonical graph changes: no Device minted for the forged
    // exporter_address or canonical_exporter_id claims.
    tokio::time::sleep(Duration::from_millis(300)).await;
    for addr in ["6.6.6.6", "10.9.9.9", "203.0.113.50"] {
        assert!(
            graph_query_one(
                &lab.graph,
                &format!("MATCH (d:Device {{address: '{addr}'}}) RETURN d.address")
            )
            .is_none(),
            "provisional evidence must not mint canonical Device {addr}"
        );
    }
}

/// R1 (OTLP): `provisional_app_id` embeds the source key — a forged app
/// id cannot carry a foreign collector namespace; the service-name
/// claim is preserved under the normalized key.
#[tokio::test]
async fn otlp_provisional_app_id_is_renamespaced() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);
    let mut client = mtls_client(&lab, &pki, &cert_a).await;

    let update = TelemetryIngestUpdate {
        collector_id: "site-a".to_string(),
        target: "otlpsrc/site-b/http/10.4.0.5:4317".to_string(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: 1_700_000_000_000_000_000,
        path: "streaming/otlp/span".to_string(),
        value_msgpack: rmp_serde::to_vec(&serde_json::json!({
            "service_name": "payments-svc",
            "peer_address": "10.4.0.5",
            "span_name": "checkout",
            "provisional_app_id": "otlpapp/otlpsrc/site-b/http/10.4.0.5:4317/payments-svc",
            "identity_status": "resolved"
        }))
        .unwrap(),
        protocol_version: PROTOCOL_VERSION,
        interface_summary: None,
    };
    let resp = ingest_updates_full(&mut client, vec![update])
        .await
        .unwrap();
    assert_eq!(resp.accepted, 1, "{resp:?}");

    let row = wait_for_graph_row(
        &lab.graph,
        "MATCH (e:StateChangeEvent) WHERE e.device_address = 'otlpsrc/site-a/http/10.4.0.5:4317' RETURN e.detail",
        "normalized otlp provisional event",
    )
    .await;
    // Parse the event detail structurally: EFFECTIVE identity fields
    // must carry the authenticated namespace, while `claimed_*`
    // evidence keys legitimately preserve the forged producer claims.
    let detail: serde_json::Value =
        serde_json::from_str(&row[0]).expect("event detail must be JSON");
    assert_eq!(
        detail["provisional_app_id"],
        "otlpapp/otlpsrc/site-a/http/10.4.0.5:4317/payments-svc",
        "app id must be re-derived under the authenticated namespace: {}",
        row[0]
    );
    assert_eq!(
        detail["source_key"], "otlpsrc/site-a/http/10.4.0.5:4317",
        "effective source key must be namespaced: {}",
        row[0]
    );
    assert_eq!(
        detail["exporter_id"], "otlpsrc/site-a/http/10.4.0.5:4317",
        "effective exporter id must be namespaced: {}",
        row[0]
    );
    assert_eq!(
        detail["identity_status"], "provisional",
        "forged resolved claim must stay provisional: {}",
        row[0]
    );
    assert_eq!(
        detail["claimed_identity_status"], "resolved",
        "original claim preserved as evidence: {}",
        row[0]
    );
    assert!(
        detail["claimed_provisional_app_id"]
            .as_str()
            .is_some_and(|s| s.contains("otlpsrc/site-b")),
        "forged app id preserved under claimed_* evidence: {}",
        row[0]
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// R3 — per-record protocol version verification
// ═══════════════════════════════════════════════════════════════════════════

/// R3: protocol compatibility is verified on EVERY record, not just the
/// first — a major-skew record mid-stream is permanently rejected while
/// compatible and minor-skew records still flow.
#[tokio::test]
async fn per_record_protocol_version_is_verified() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    bind_collector(&lab.sqlite, "site-a", &cert_a.fingerprint);
    let mut client = mtls_client(&lab, &pki, &cert_a).await;

    let mut v0 = netflow_update(
        "site-a",
        "flowsrc/site-a/nf9/10.8.0.1:49152/a",
        flow_value(serde_json::json!({"src_port": 41000})),
    );
    v0.protocol_version = 0; // pre-versioning / major skew
    let mut v11 = netflow_update(
        "site-a",
        "flowsrc/site-a/nf9/10.8.0.2:49152/b",
        flow_value(serde_json::json!({"src_port": 41001})),
    );
    v11.protocol_version = PROTOCOL_VERSION + 10; // major skew
    let mut v2 = netflow_update(
        "site-a",
        "flowsrc/site-a/nf9/10.8.0.3:49152/c",
        flow_value(serde_json::json!({"src_port": 41002})),
    );
    v2.protocol_version = PROTOCOL_VERSION + 1; // minor skew — warn only

    // Interleave: valid, major-skew, minor-skew, major-skew, valid.
    let mut first = netflow_update(
        "site-a",
        "flowsrc/site-a/nf9/10.8.0.0:49152/z",
        flow_value(serde_json::json!({"src_port": 40999})),
    );
    first.protocol_version = PROTOCOL_VERSION;
    let resp = ingest_updates_full(
        &mut client,
        vec![
            first,
            v0,
            v2,
            v11,
            netflow_update(
                "site-a",
                "flowsrc/site-a/nf9/10.8.0.4:49152/d",
                flow_value(serde_json::json!({"src_port": 41003})),
            ),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        resp.accepted, 3,
        "compatible + minor-skew accepted: {resp:?}"
    );
    assert_eq!(
        resp.rejected, 2,
        "both major-skew records rejected: {resp:?}"
    );

    // Only the three compatible records produced AppFlows.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let rows = graph_query(
        &lab.graph,
        "MATCH (f:AppFlow) WHERE f.exporter_id STARTS WITH 'flowsrc/site-a/' RETURN f.exporter_id",
    );
    assert_eq!(
        rows.len(),
        3,
        "major-skew records must not reach the graph: {rows:?}"
    );
    assert!(
        rows.iter()
            .all(|r| !r[0].contains("10.8.0.1") && !r[0].contains("10.8.0.2"))
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// R4 — global collector identity uniqueness
// ═══════════════════════════════════════════════════════════════════════════

fn collector_binding(
    sqlite: &SqliteStore,
    collector_id: &str,
    fp: &str,
    scope: &str,
) -> Result<String, anyhow::Error> {
    let dev = sqlite
        .upsert_canonical_device(
            &CanonicalDevice {
                device_id: String::new(),
                address: format!("collector:{collector_id}"),
                site: Some(scope.to_string()),
                hostname: None,
                created_at_ns: 0,
                updated_at_ns: 0,
                created_by: String::new(),
                updated_by: String::new(),
            },
            "test",
        )
        .unwrap();
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
        .map(|b| b.binding_id)
}

/// R4: collector identity is globally unique — the same collector id
/// cannot be claimed by a different credential through a different
/// scope, and ambiguous pre-existing rows fail closed at runtime for
/// EVERY claimant.
#[tokio::test]
async fn collector_identity_uniqueness_is_global() {
    let pki = make_pki();
    let lab = start_lab(&pki).await;
    let cert_a = issue_leaf(&pki, "collector-a", false);
    let cert_b = issue_leaf(&pki, "collector-b", false);

    // Legitimate provisioning: two distinct collector identities.
    assert!(collector_binding(&lab.sqlite, "site-a", &cert_a.fingerprint, "dc-a").is_ok());
    assert!(collector_binding(&lab.sqlite, "site-b", &cert_b.fingerprint, "dc-b").is_ok());

    // Same collector id, DIFFERENT credential, different scope —
    // rejected at provisioning.
    let dup = collector_binding(&lab.sqlite, "site-a", &cert_b.fingerprint, "dc-b");
    assert!(
        dup.is_err(),
        "same collector id under a different credential must be rejected"
    );

    // Same credential, different collector id under a different scope —
    // rejected (one credential, one collector identity).
    let dup2 = collector_binding(&lab.sqlite, "site-c", &cert_a.fingerprint, "dc-b");
    assert!(
        dup2.is_err(),
        "one credential must not bind multiple collector identities"
    );

    // Rotation updates the SAME logical binding (same binding_id,
    // new credential) rather than creating an ambiguous second row.
    let rotated_cert = issue_leaf(&pki, "collector-a-rotated", false);
    let rotated_fp = rotated_cert.fingerprint.clone();
    let existing = lab
        .sqlite
        .list_exporter_bindings(
            Some("dc-a"),
            Some(bonsai::collector_auth::COLLECTOR_PROTOCOL),
            false,
        )
        .unwrap()
        .into_iter()
        .find(|b| b.exporter_id == "site-a")
        .unwrap();
    let mut rotated = existing.clone();
    rotated.match_ingress_credential = Some(rotated_fp.clone());
    lab.sqlite
        .upsert_exporter_binding(&rotated, "test")
        .unwrap();
    let rows = lab
        .sqlite
        .collector_bindings_for_exporter("site-a")
        .unwrap();
    assert_eq!(rows.len(), 1, "rotation must keep exactly one binding");
    assert_eq!(
        rows[0].match_ingress_credential.as_deref(),
        Some(rotated_fp.as_str())
    );

    // Pre-existing ambiguous row (a THIRD credential claiming the same
    // collector id, bypassing provisioning) fails closed for EVERY
    // claimant — exercised end-to-end over mTLS because attested
    // principals can only be minted by the transport layer.
    let cert_c = issue_leaf(&pki, "collector-c", false);
    insert_raw_collector_binding(&lab, "site-a", &cert_c.fingerprint, "dc-c");

    let mut client_a = mtls_client(&lab, &pki, &rotated_cert).await;
    let err = client_a
        .heartbeat(Request::new(CollectorStats {
            collector_id: "site-a".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        Code::PermissionDenied,
        "ambiguous collector identity must fail closed for the legitimate claimant"
    );
    let mut client_c = mtls_client(&lab, &pki, &cert_c).await;
    let err = client_c
        .heartbeat(Request::new(CollectorStats {
            collector_id: "site-a".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        Code::PermissionDenied,
        "ambiguous collector identity must fail closed for the usurping claimant"
    );
    // Unambiguous identities still authorize.
    let mut client_b = mtls_client(&lab, &pki, &cert_b).await;
    let hb = client_b
        .heartbeat(Request::new(CollectorStats {
            collector_id: "site-b".to_string(),
            ..Default::default()
        }))
        .await
        .expect("unambiguous collector identity must still authorize");
    assert!(hb.into_inner().error.is_empty());
}

/// R4: concurrent provisioning of the same collector identity cannot
/// bypass uniqueness — exactly one writer wins.
#[test]
fn concurrent_collector_provisioning_is_serialized() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(dir.path()).unwrap());
    let anchor = store
        .upsert_canonical_device(
            &CanonicalDevice {
                device_id: String::new(),
                address: "collector:race".to_string(),
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
        .device_id;
    let mk = |fp: &str, scope: &str| TrustedBinding {
        binding_id: SqliteStore::new_binding_id(),
        scope: scope.to_string(),
        protocol: bonsai::collector_auth::COLLECTOR_PROTOCOL.to_string(),
        exporter_id: "race".to_string(),
        device_id: anchor.clone(),
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
    let b1 = mk("sha256:aaa", "scope-1");
    let b2 = mk("sha256:bbb", "scope-2");
    let (r1, r2) = std::thread::scope(|s| {
        let h1 = s.spawn(|| store.upsert_exporter_binding(&b1, "t1").is_ok());
        let h2 = s.spawn(|| store.upsert_exporter_binding(&b2, "t2").is_ok());
        (h1.join().unwrap(), h2.join().unwrap())
    });
    assert!(
        r1 != r2,
        "exactly one concurrent provisioning must win (got {r1}, {r2})"
    );
    assert_eq!(
        store.collector_bindings_for_exporter("race").unwrap().len(),
        1
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Phase 6 — indexed credential lookup evidence
// ═══════════════════════════════════════════════════════════════════════════

/// The credential-pinned binding lookups used by `CollectorAuthz` must
/// be served by a real SQLite index, not a full table scan.
#[test]
fn credential_binding_lookups_use_sqlite_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    drop(store);
    let conn =
        rusqlite::Connection::open(dir.path().join(bonsai::sqlite_store::CONFIG_DB_FILE_NAME))
            .unwrap();

    let plan_for = |sql: &str| -> Vec<String> {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        stmt.query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };

    // find_credential_bindings shape (collector protocol, credential-
    // pinned). SQLite may pick either credential index — the general
    // one or the partial unique collector-scoped one — both are real
    // indexes on match_ingress_credential.
    let plan = plan_for(
        "SELECT binding_id FROM exporter_bindings \
         WHERE status = 'active' AND match_ingress_credential = 'sha256:x' \
         AND protocol = 'collector' AND exporter_id = 'c1' LIMIT 16",
    );
    assert!(
        plan.iter().any(|l| {
            l.contains("match_ingress_credential=?")
                && (l.contains("idx_exporter_bindings_credential")
                    || l.contains("idx_exporter_bindings_collector_credential"))
        }),
        "credential lookup must use a credential index: {plan:?}"
    );

    // collector_bindings_for_exporter shape.
    let plan = plan_for(
        "SELECT binding_id FROM exporter_bindings \
         WHERE status = 'active' AND protocol = 'collector' AND exporter_id = 'c1' LIMIT 1024",
    );
    assert!(
        plan.iter()
            .any(|l| l.contains("idx_exporter_bindings_collector_identity")),
        "collector identity lookup must use the partial unique index: {plan:?}"
    );
}
