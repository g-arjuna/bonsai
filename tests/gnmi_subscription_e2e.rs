//! M5.2B B4 — distributed subscription-evidence E2E: scripted gNMI
//! server → real GnmiSubscriber (collector runtime pieces) →
//! authenticated mTLS collector→core gRPC (SubscriptionLifecycle +
//! TelemetryIngest) → core session registry/authority validation →
//! bus → GraphStore persistence + subscription verifier → asserted
//! subscription status. Contract verification only; no vendor claim.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bonsai::api::pb::bonsai_graph_client::BonsaiGraphClient;
use bonsai::api::{BonsaiGraphServer, CoreService, PROTOCOL_VERSION_CURRENT};
use bonsai::assignment::CollectorManager;
use bonsai::collector_auth::AuthPolicy;
use bonsai::config::{
    AcquisitionMode, GnmiSecurityMode, RuntimeConfig, RuntimeTlsConfig, SelectedSubscriptionPath,
    TargetConfig,
};
use bonsai::credentials::CredentialVault;
use bonsai::event_bus::InProcessBus;
use bonsai::graph::GraphStore;
use bonsai::ingest::{
    handle_assignment_update, run_lifecycle_forwarder, telemetry_to_ingest_update,
};
use bonsai::proto::gnmi::g_nmi_server::{GNmi, GNmiServer};
use bonsai::proto::gnmi::{
    CapabilityRequest, CapabilityResponse, Encoding, ModelData, Path, SubscribeRequest,
    SubscribeResponse, subscribe_response, typed_value,
};
use bonsai::registry::ApiRegistry;
use bonsai::sidecar_registry::SidecarRegistry;
use bonsai::sqlite_store::SqliteStore;
use bonsai::store::BonsaiStore;
use bonsai::subscriber::{GnmiSubscriber, LifecycleHandle};
use bonsai::subscription_sessions::SubscriptionSessionRegistry;
use bonsai::subscription_status::run_subscription_verifier;
use bonsai::telemetry_identity::{
    BindingProvenance, BindingStatus, CanonicalDevice, TrustedBinding,
};
use rcgen::{CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc, watch};
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming, transport::Channel};

// ── Synthetic PKI (same convention as bn021g tests) ─────────────────────────

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
    let mut ca_params = CertificateParams::default();
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

// ── Scripted gNMI server that actually emits notifications ─────────────────

#[derive(Clone)]
struct MockGnmi {
    caps: CapabilityResponse,
    /// One response channel per open Subscribe stream; the test injects
    /// Update/SyncResponse notifications through the newest sender.
    streams: Arc<Mutex<Vec<mpsc::Sender<Result<SubscribeResponse, Status>>>>>,
    /// Every SubscribeRequest message received, for wire-level assertions.
    requests: Arc<std::sync::Mutex<Vec<SubscribeRequest>>>,
    subscribe_calls: Arc<AtomicUsize>,
}

#[tonic::async_trait]
impl GNmi for MockGnmi {
    async fn capabilities(
        &self,
        _request: Request<CapabilityRequest>,
    ) -> Result<Response<CapabilityResponse>, Status> {
        Ok(Response::new(self.caps.clone()))
    }

    type SubscribeStream = ReceiverStream<Result<SubscribeResponse, Status>>;

    async fn subscribe(
        &self,
        request: Request<Streaming<SubscribeRequest>>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        self.subscribe_calls.fetch_add(1, Ordering::SeqCst);
        let requests = Arc::clone(&self.requests);
        let mut inner = request.into_inner();
        tokio::spawn(async move {
            while let Ok(Some(msg)) = inner.message().await {
                requests.lock().unwrap().push(msg);
            }
        });
        let (tx, rx) = mpsc::channel::<Result<SubscribeResponse, Status>>(64);
        self.streams.lock().await.push(tx);
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

fn caps_response() -> CapabilityResponse {
    CapabilityResponse {
        supported_models: vec![ModelData {
            name: "openconfig-interfaces".to_string(),
            organization: "openconfig".to_string(),
            version: "1".to_string(),
        }],
        supported_encodings: vec![Encoding::JsonIetf as i32, Encoding::Proto as i32],
        g_nmi_version: "0.3.0".to_string(),
        ..Default::default()
    }
}

async fn start_mock() -> (
    String,
    Arc<Mutex<Vec<mpsc::Sender<Result<SubscribeResponse, Status>>>>>,
    Arc<std::sync::Mutex<Vec<SubscribeRequest>>>,
    Arc<AtomicUsize>,
    watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
) {
    let streams = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscribe_calls = Arc::new(AtomicUsize::new(0));
    let svc = MockGnmi {
        caps: caps_response(),
        streams: Arc::clone(&streams),
        requests: Arc::clone(&requests),
        subscribe_calls: Arc::clone(&subscribe_calls),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let (sd, mut srx) = watch::channel(false);
    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(GNmiServer::new(svc))
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async move {
                let _ = srx.changed().await;
            })
            .await
            .unwrap();
    });
    (addr, streams, requests, subscribe_calls, sd, handle)
}

fn leaf_path(name: &str) -> Path {
    Path {
        origin: "openconfig".to_string(),
        elem: vec![
            bonsai::proto::gnmi::PathElem {
                name: "interfaces".to_string(),
                key: Default::default(),
            },
            bonsai::proto::gnmi::PathElem {
                name: "interface".to_string(),
                key: [("name".to_string(), name.to_string())]
                    .into_iter()
                    .collect(),
            },
            bonsai::proto::gnmi::PathElem {
                name: "state".to_string(),
                key: Default::default(),
            },
            bonsai::proto::gnmi::PathElem {
                name: "counters".to_string(),
                key: Default::default(),
            },
            bonsai::proto::gnmi::PathElem {
                name: "in-octets".to_string(),
                key: Default::default(),
            },
        ],
        ..Default::default()
    }
}

fn update_notification(path: &Path, val: i64) -> SubscribeResponse {
    let mut tv = bonsai::proto::gnmi::TypedValue::default();
    tv.value = Some(typed_value::Value::IntVal(val));
    SubscribeResponse {
        response: Some(subscribe_response::Response::Update(
            bonsai::proto::gnmi::Notification {
                timestamp: 1_000,
                prefix: None,
                update: vec![bonsai::proto::gnmi::Update {
                    path: Some(path.clone()),
                    val: Some(tv),
                    ..Default::default()
                }],
                delete: vec![],
                atomic: false,
            },
        )),
        ..Default::default()
    }
}

fn sync_notification() -> SubscribeResponse {
    SubscribeResponse {
        response: Some(subscribe_response::Response::SyncResponse(true)),
        ..Default::default()
    }
}

// ── Core lab: real tonic server + real client + real graph path ────────────

/// `BonsaiStore` cannot be implemented for `Arc<GraphStore>` (orphan
/// rule), so the verifier gets a thin delegating newtype over the same
/// store the core service writes through.
struct SharedStore(Arc<GraphStore>);

#[tonic::async_trait]
impl BonsaiStore for SharedStore {
    fn db(&self) -> Arc<lbug::Database> {
        BonsaiStore::db(&*self.0)
    }
    fn write_lock(&self) -> Arc<std::sync::Mutex<()>> {
        BonsaiStore::write_lock(&*self.0)
    }
    fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<bonsai::graph::BonsaiEvent> {
        BonsaiStore::subscribe_events(&*self.0)
    }
    async fn write(&self, update: bonsai::telemetry::TelemetryUpdate) -> anyhow::Result<()> {
        BonsaiStore::write(&*self.0, update).await
    }
    async fn write_detection(
        &self,
        device_address: String,
        rule_id: String,
        severity: String,
        features_json: String,
        source_types_json: String,
        latency_ns: i64,
        fired_at_ns: i64,
        state_change_event_id: String,
        source_event_ids: Vec<String>,
    ) -> anyhow::Result<String> {
        BonsaiStore::write_detection(
            &*self.0,
            device_address,
            rule_id,
            severity,
            features_json,
            source_types_json,
            latency_ns,
            fired_at_ns,
            state_change_event_id,
            source_event_ids,
        )
        .await
    }
    async fn write_remediation(
        &self,
        detection_id: String,
        action: String,
        status: String,
        detail_json: String,
        attempted_at_ns: i64,
        completed_at_ns: i64,
    ) -> anyhow::Result<String> {
        BonsaiStore::write_remediation(
            &*self.0,
            detection_id,
            action,
            status,
            detail_json,
            attempted_at_ns,
            completed_at_ns,
        )
        .await
    }
    async fn sync_sites_from_targets(&self, targets: Vec<TargetConfig>) -> anyhow::Result<()> {
        BonsaiStore::sync_sites_from_targets(&*self.0, targets).await
    }
    async fn list_sites(&self) -> anyhow::Result<Vec<bonsai::graph::SiteRecord>> {
        BonsaiStore::list_sites(&*self.0).await
    }
    async fn upsert_site(
        &self,
        site: bonsai::graph::SiteRecord,
    ) -> anyhow::Result<bonsai::graph::SiteRecord> {
        BonsaiStore::upsert_site(&*self.0, site).await
    }
    async fn write_subscription_status(
        &self,
        status: bonsai::graph::SubscriptionStatusWrite,
    ) -> anyhow::Result<()> {
        BonsaiStore::write_subscription_status(&*self.0, status).await
    }
    async fn supersede_subscription_statuses_outside_plan(
        &self,
        device_address: &str,
        keep: &std::collections::HashSet<(String, String, String, i64)>,
    ) -> anyhow::Result<()> {
        BonsaiStore::supersede_subscription_statuses_outside_plan(&*self.0, device_address, keep)
            .await
    }
    fn publish_event(&self, event: bonsai::graph::BonsaiEvent) {
        BonsaiStore::publish_event(&*self.0, event)
    }
}

struct Lab {
    addr: std::net::SocketAddr,
    graph: Arc<GraphStore>,
    registry: Arc<ApiRegistry>,
    sqlite: Arc<SqliteStore>,
    manager: Arc<CollectorManager>,
    sessions: Arc<SubscriptionSessionRegistry>,
    _dir: TempDir,
    _server: tokio::task::JoinHandle<()>,
    _graph_writer: tokio::task::JoinHandle<()>,
    _verifier: tokio::task::JoinHandle<()>,
    /// Must outlive the verifier: dropping the sender makes
    /// `shutdown.changed()` return Err and the verifier exits.
    _verifier_shutdown: watch::Sender<bool>,
    /// The verifier exits when the plan channel closes; production
    /// holds this sender for the process lifetime.
    _plan_tx: mpsc::Sender<bonsai::subscription_status::SubscriptionPlan>,
}

async fn start_lab(pki: &Pki, device: TargetConfig) -> Lab {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONSAI_E2E_VAULT_PASSPHRASE", "test-passphrase") };

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
        CredentialVault::open(dir.path().join("vault"), "BONSAI_E2E_VAULT_PASSPHRASE").unwrap(),
    );
    let bus = InProcessBus::new(1024);
    let mut graph_rx = bus.subscribe();
    let graph_for_writer = Arc::clone(&graph);
    let graph_writer = tokio::spawn(async move {
        while let Ok(update) = graph_rx.recv().await {
            let u = Arc::unwrap_or_clone(update);
            let _ = BonsaiStore::write(graph_for_writer.as_ref(), u).await;
        }
    });
    let sessions = Arc::new(SubscriptionSessionRegistry::new());
    let manager = Arc::new(
        CollectorManager::new(Arc::clone(&registry), Arc::clone(&credentials), vec![])
            // Production wiring (server_startup): assignment withdrawal and
            // collector disconnect retire session authority core-side.
            .with_session_registry(Arc::clone(&sessions)),
    );
    let sidecars = Arc::new(SidecarRegistry::new(vec![]));

    // Seed the verifier plan exactly as server_startup does for core
    // mode; the same channel also carries authenticated submitted
    // manifests forwarded by the lifecycle RPC (M5.2B B3).
    let (plan_tx, plan_rx) = mpsc::channel::<bonsai::subscription_status::SubscriptionPlan>(128);
    let svc = CoreService::new(
        Arc::clone(&graph),
        Arc::clone(&registry),
        credentials,
        bus.clone(),
        None,
        Some(Arc::clone(&manager)),
        sidecars,
        Some(Arc::clone(&sqlite)),
        None,
        None,
    )
    .with_auth_policy(AuthPolicy::Required)
    .with_session_registry(Arc::clone(&sessions))
    .with_subscription_plan_tx(Some(plan_tx.clone()));

    let plan = bonsai::subscriber::planned_subscription_plan_for_target(&device);
    plan_tx.send(plan).await.unwrap();
    let verifier_store = SharedStore(Arc::clone(&graph));
    let verifier_bus = Arc::clone(&bus);
    let verifier_sessions = Arc::clone(&sessions);
    let (vsd, vsrx) = watch::channel(false);
    // Readiness barrier (same contract as production server_startup):
    // the verifier signals once its bus subscriber is registered and
    // boot-time state is initialized — telemetry published before that
    // would be lost permanently. The sender drops on verifier exit, so
    // this await cannot hang.
    let (verifier_ready_tx, verifier_ready_rx) = tokio::sync::oneshot::channel();
    let verifier = tokio::spawn(run_subscription_verifier(
        verifier_store,
        verifier_bus,
        plan_rx,
        verifier_sessions,
        vsrx,
        Some(verifier_ready_tx),
        None,
    ));
    verifier_ready_rx
        .await
        .expect("subscription verifier exited before signaling readiness")
        .expect("subscription verifier boot initialization failed");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_cert = issue_leaf(pki, "bonsai-core", true);
    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(
            server_cert.cert_pem.clone(),
            server_cert.key_pem.clone(),
        ))
        .client_ca_root(Certificate::from_pem(pki.ca_cert_pem.clone().into_bytes()));
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
        graph,
        registry,
        sqlite,
        manager,
        sessions,
        _dir: dir,
        _server: server,
        _graph_writer: graph_writer,
        _verifier: verifier,
        _verifier_shutdown: vsd,
        _plan_tx: plan_tx,
    }
}

async fn connect_client(
    pki: &Pki,
    leaf: &LeafCert,
    addr: std::net::SocketAddr,
) -> BonsaiGraphClient<Channel> {
    let tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(pki.ca_cert_pem.clone().into_bytes()))
        .domain_name("localhost")
        .identity(Identity::from_pem(
            leaf.cert_pem.clone(),
            leaf.key_pem.clone(),
        ));
    let channel = Endpoint::from_shared(format!("https://{addr}"))
        .unwrap()
        .tls_config(tls)
        .unwrap()
        .connect()
        .await
        .unwrap();
    BonsaiGraphClient::new(channel)
}

/// The collector forwarder reads TLS material from files; write the
/// synthetic PKI into the lab tempdir.
fn write_tls_files(pki: &Pki, leaf: &LeafCert, dir: &std::path::Path) -> RuntimeTlsConfig {
    let ca = dir.join("ca.pem");
    let cert = dir.join("collector.pem");
    let key = dir.join("collector-key.pem");
    std::fs::write(&ca, &pki.ca_cert_pem).unwrap();
    std::fs::write(&cert, &leaf.cert_pem).unwrap();
    std::fs::write(&key, &leaf.key_pem).unwrap();
    RuntimeTlsConfig {
        enabled: true,
        ca_cert: Some(ca.to_string_lossy().into_owned()),
        cert: Some(cert.to_string_lossy().into_owned()),
        key: Some(key.to_string_lossy().into_owned()),
        server_name: Some("localhost".to_string()),
    }
}

fn device_target(address: &str) -> TargetConfig {
    TargetConfig {
        acquisition: AcquisitionMode::Explicit,
        extra_ips: Vec::new(),
        optional: false,
        paths: Vec::new(),
        address: address.to_string(),
        enabled: true,
        tls_domain: None,
        ca_cert: None,
        vendor: Some("openconfig".to_string()),
        credential_alias: None,
        username_env: None,
        password_env: None,
        username: None,
        password: None,
        software_build: None,
        hostname: Some("e2e-dut".to_string()),
        role: None,
        site: Some("dc-e2e".to_string()),
        collector_id: Some("e2e-col".to_string()),
        selected_paths: vec![
            SelectedSubscriptionPath {
                path: "interfaces/interface[name=Ethernet1/1]/state/counters".to_string(),
                origin: "openconfig".to_string(),
                provenance: String::new(),
                mode: "SAMPLE".to_string(),
                sample_interval_ns: 10_000_000_000,
                rationale: String::new(),
                optional: false,
            },
            // Path B: a second expectation that must stay unobserved —
            // no matching notification is ever injected for it.
            SelectedSubscriptionPath {
                path: "interfaces/interface[name=Ethernet1/2]/state/oper-status".to_string(),
                origin: "openconfig".to_string(),
                provenance: String::new(),
                mode: "ON_CHANGE".to_string(),
                sample_interval_ns: 0,
                rationale: String::new(),
                optional: false,
            },
            // Path C: third Subscription entry — exercises the gNMI
            // sync boundary: ONE SubscriptionList gets ONE
            // SyncResponse regardless of entry count (spec 3.5.1.4).
            SelectedSubscriptionPath {
                path: "interfaces/interface[name=Ethernet1/3]/state/oper-status".to_string(),
                origin: "openconfig".to_string(),
                provenance: String::new(),
                mode: "ON_CHANGE".to_string(),
                sample_interval_ns: 0,
                rationale: String::new(),
                optional: false,
            },
        ],
        external_id: None,
        inventory_only: false,
        inventory_platform: None,
        inventory_status: None,
        inventory_tenant: None,
        security: None,
        client_cert: None,
        client_key: None,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
        last_operator_action: String::new(),
        schema_binding: None,
    }
}

fn bind_collector(sqlite: &SqliteStore, collector_id: &str, fp: &str) {
    let device = CanonicalDevice {
        device_id: String::new(),
        address: format!("collector:{collector_id}"),
        site: Some("dc-e2e".to_string()),
        hostname: None,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
    };
    let dev = sqlite.upsert_canonical_device(&device, "test").unwrap();
    let binding = TrustedBinding {
        binding_id: SqliteStore::new_binding_id(),
        scope: "dc-e2e".to_string(),
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
    sqlite.upsert_exporter_binding(&binding, "test").unwrap();
}

/// Returns (device_address, path, origin, mode, status) rows.
#[allow(clippy::type_complexity)]
async fn query_subscription_statuses(
    graph: &GraphStore,
) -> Vec<(String, String, String, String, String)> {
    use lbug::Connection;
    let db = graph.db();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).unwrap();
        let mut out = Vec::new();
        let rows = conn
            .query(
                "MATCH (s:SubscriptionStatus)                  RETURN s.device_address, s.path, s.origin, s.mode, s.status",
            )
            .unwrap();
        for row in rows {
            let get = |i: usize| -> String {
                match &row[i] {
                    lbug::Value::String(v) => v.clone(),
                    other => other.to_string(),
                }
            };
            out.push((get(0), get(1), get(2), get(3), get(4)));
        }
        out
    })
    .await
    .unwrap()
}

async fn wait_for<F: FnMut() -> bool>(mut cond: F, timeout: Duration, what: &str) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    eprintln!("wait_for timed out: {what}");
    false
}

#[tokio::test]
async fn distributed_subscription_evidence_reaches_verified_coverage() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .try_init();
    let pki = make_pki();
    let collector_leaf = issue_leaf(&pki, "e2e-col", false);

    // Mock gNMI device.
    let (gnmi_addr, streams, requests, subscribe_calls, gsd, ghandle) = start_mock().await;
    let device_addr = gnmi_addr.trim_start_matches("http://").to_string();
    let device_addr = device_addr.trim_end_matches('/').to_string();

    // Core lab with device assigned to e2e-col.
    let device = device_target(&device_addr);
    let lab = start_lab(&pki, device.clone()).await;
    lab.registry
        .add_device_with_audit(device.clone(), "test", "test_add")
        .unwrap();
    lab.registry
        .assign_device_with_audit(
            &device_addr,
            Some("e2e-col".to_string()),
            "test",
            "test_assign",
        )
        .unwrap();
    bind_collector(&lab.sqlite, "e2e-col", &collector_leaf.fingerprint);

    // Collector registration → assignment authority generation.
    let collector_client = connect_client(&pki, &collector_leaf, lab.addr).await;
    let epoch = lab.manager.authority_epoch();
    let (mut assign_rx, _gen) = lab
        .manager
        .register_collector("e2e-col".to_string(), PROTOCOL_VERSION_CURRENT, true)
        .await
        .unwrap();
    let initial_sync = assign_rx.recv().await.expect("initial sync");
    let assigned_gen = initial_sync.generation;
    let collector_session_gen = initial_sync.collector_session_generation;
    assert!(
        lab.manager
            .validate_session_authority("e2e-col", &device_addr, epoch, assigned_gen)
    );

    // Lifecycle channel → real forwarder → authenticated RPC.
    let tls_files = write_tls_files(&pki, &collector_leaf, lab._dir.path());
    let runtime_cfg = Arc::new(RuntimeConfig {
        collector_id: "e2e-col".to_string(),
        core_ingest_endpoint: format!("https://{}", lab.addr),
        tls: tls_files,
        ..Default::default()
    });
    let (lc_tx, lc_rx) = mpsc::channel::<bonsai::subscription_sessions::SessionLifecycleEvent>(256);
    let fwd_cfg = Arc::clone(&runtime_cfg);
    let fwd_id = "e2e-col".to_string();
    let (fsd, fsrx) = watch::channel(false);
    let lifecycle_fwd = tokio::spawn(run_lifecycle_forwarder(fwd_cfg, fwd_id, lc_rx, fsrx));

    // Collector bus → TelemetryIngest RPC (same wire path as production).
    let collector_bus = InProcessBus::new(256);
    let mut ingest_rx = collector_bus.subscribe();
    let ingest_client = connect_client(&pki, &collector_leaf, lab.addr).await;
    let ingest_cfg = Arc::clone(&runtime_cfg);
    let ingest_id = "e2e-col".to_string();
    let (isd, mut is_rx) = watch::channel(false);
    let ingest_fwd = tokio::spawn(async move {
        let client = ingest_client;
        loop {
            tokio::select! {
                _ = is_rx.changed() => break,
                update = ingest_rx.recv() => {
                    let Ok(update) = update else { break };
                    let proto = match telemetry_to_ingest_update(&ingest_id, &update) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    let mut c = client.clone();
                    tokio::spawn(async move {
                        let _ = c
                            .telemetry_ingest(Request::new(tokio_stream::iter(vec![proto])))
                            .await;
                    });
                }
            }
        }
    });
    let _ = ingest_cfg;

    // Real subscriber against the mock device.
    let lifecycle = LifecycleHandle {
        tx: lc_tx,
        authority_epoch: epoch,
        assignment_generation: assigned_gen,
        collector_session_generation: collector_session_gen,
    };
    let subscriber = GnmiSubscriber::new(
        device_addr.clone(),
        None,
        None,
        None,
        Some("e2e-dut".to_string()),
        None,
        Some("dc-e2e".to_string()),
        bonsai::gnmi_security::ConnSecurity::plaintext(true),
        Arc::clone(&collector_bus),
        None,
        None,
        device.selected_paths.clone(),
        AcquisitionMode::Explicit,
        Some(lifecycle),
        std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
    );
    let (ssd, mut ssrx) = watch::channel(false);
    let sub_handle = tokio::spawn(async move { subscriber.subscribe_telemetry(&mut ssrx).await });
    let _ = ssd;

    // Wait for the Subscribe RPC, then complete initial sync so the
    // session can register authoritatively; the covering observation is
    // injected only after registration so it is evaluated against the
    // current session.
    assert!(
        wait_for(
            || subscribe_calls.load(Ordering::SeqCst) >= 1,
            Duration::from_secs(10),
            "subscribe rpc"
        )
        .await
    );

    // Wire-level contract: exactly one Subscribe RPC carrying ONE
    // SubscriptionList with all three Subscription entries in STREAM
    // mode — the sync boundary the mock must honor.
    // The request drain task records messages asynchronously — wait
    // (bounded) for the wire record rather than racing it.
    assert!(
        wait_for(
            || !requests.lock().unwrap().is_empty(),
            Duration::from_secs(10),
            "subscribe request recorded"
        )
        .await
    );
    {
        let reqs = requests.lock().unwrap();
        assert_eq!(
            reqs.len(),
            1,
            "expected exactly one SubscribeRequest: {reqs:?}"
        );
        let Some(bonsai::proto::gnmi::subscribe_request::Request::Subscribe(list)) =
            &reqs[0].request
        else {
            panic!("expected Subscribe request, got {:?}", reqs[0].request);
        };
        assert_eq!(
            list.subscription.len(),
            3,
            "expected 3 Subscription entries"
        );
        assert_eq!(
            list.mode,
            bonsai::proto::gnmi::subscription_list::Mode::Stream as i32
        );
    }

    {
        let guard = streams.lock().await;
        let tx = guard.last().expect("subscribe stream").clone();
        // Initial-sync ordering, protocol-correct: matching telemetry
        // for path A plus an unrelated keyed instance are delivered
        // BEFORE the single SyncResponse(true). The gNMI sync boundary
        // is the SubscriptionList/RPC (spec 3.5.1.4/3.5.2.3) — one
        // list, one Subscribe RPC, one SyncResponse regardless of
        // entry count; a second sync would be protocol noise. The
        // early path-A update exercises the bounded pending buffer:
        // it must be released only once the session is authoritatively
        // current, never dropped into a false subscribed_but_silent.
        tx.send(Ok(update_notification(&leaf_path("Ethernet9/9"), 7)))
            .await
            .unwrap();
        tx.send(Ok(update_notification(&leaf_path("Ethernet1/1"), 42)))
            .await
            .unwrap();
        tx.send(Ok(sync_notification())).await.unwrap();
    }

    // Lifecycle synchronized must register the session authoritatively.
    assert!(
        wait_for(
            || {
                lab.sessions
                    .current_session(&device_addr)
                    .is_some_and(|(p, s)| !p.is_empty() && !s.is_empty())
            },
            Duration::from_secs(10),
            "session registered"
        )
        .await,
        "lifecycle registration never reached core registry"
    );

    // Path B and path C receive NO telemetry at all — the sync-first /
    // no-update case: synchronization alone must never produce coverage
    // (no updates_only semantics are claimed or required). Path A's
    // matching evidence arrived BEFORE the SyncResponse; the bounded
    // pending buffer releases it through the normal fenced path once
    // this session is current. The unrelated Ethernet9/9 update sent
    // pre-sync must never satisfy any configured path.
    let path_a = "interfaces/interface[name=Ethernet1/1]/state/counters";
    let path_b = "interfaces/interface[name=Ethernet1/2]/state/oper-status";
    let path_c = "interfaces/interface[name=Ethernet1/3]/state/oper-status";

    // Coverage: exact structured path observation → observed for path A
    // ONLY, with exact device/path/origin/mode identity.
    let mut last_rows = Vec::new();
    let mut a_observed = false;
    for _ in 0..100 {
        last_rows = query_subscription_statuses(&lab.graph).await;
        if last_rows
            .iter()
            .any(|r| r.0 == device_addr && r.1 == path_a && r.4 == "observed")
        {
            a_observed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(a_observed, "path A never observed: {last_rows:?}");
    let row_a = last_rows
        .iter()
        .find(|r| r.0 == device_addr && r.1 == path_a)
        .expect("path A row");
    assert_eq!(row_a.2, "openconfig", "path A origin");
    assert_eq!(row_a.3, "SAMPLE", "path A mode");
    assert!(
        last_rows
            .iter()
            .all(|r| !(r.0 == device_addr && r.1 == path_b && r.4 == "observed")),
        "path B must never be observed without matching evidence: {last_rows:?}"
    );
    assert!(
        last_rows
            .iter()
            .all(|r| !(r.0 == device_addr && r.1 == path_c && r.4 == "observed")),
        "path C must never be observed without matching evidence: {last_rows:?}"
    );

    fsd.send(true).unwrap();
    isd.send(true).unwrap();
    ssd.send(true).unwrap();
    gsd.send(true).unwrap();
    let _ = lifecycle_fwd.await;
    let _ = ingest_fwd.await;
    let _ = sub_handle.await;
    let _ = ghandle.await;
    let _ = collector_client;
}

/// M5.2B review B1 — trusted coverage provenance: two DISTINCT
/// authenticated collector principals deliberately reuse Device A's
/// exact target/plan_id/session_id. Collector B's observations —
/// recognized AND semantically unclassified, before AND after
/// synchronization, and after authority withdrawal — must never
/// satisfy Collector A's subscription. Only A's own authenticated
/// evidence establishes coverage.
#[tokio::test]
async fn foreign_collector_observations_never_satisfy_coverage() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let pki = make_pki();
    let leaf_a = issue_leaf(&pki, "e2e-col-a", false);
    let leaf_b = issue_leaf(&pki, "e2e-col-b", false);

    let (gnmi_addr, streams, _requests, subscribe_calls, gsd, ghandle) = start_mock().await;
    let device_addr = gnmi_addr
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();

    let mut device = device_target(&device_addr);
    device.collector_id = Some("e2e-col-a".to_string());
    let lab = start_lab(&pki, device.clone()).await;
    lab.registry
        .add_device_with_audit(device.clone(), "test", "test_add")
        .unwrap();
    lab.registry
        .assign_device_with_audit(
            &device_addr,
            Some("e2e-col-a".to_string()),
            "test",
            "test_assign",
        )
        .unwrap();
    bind_collector(&lab.sqlite, "e2e-col-a", &leaf_a.fingerprint);
    bind_collector(&lab.sqlite, "e2e-col-b", &leaf_b.fingerprint);

    // Collector A registration → assignment authority.
    let epoch = lab.manager.authority_epoch();
    let (mut assign_rx, _gen) = lab
        .manager
        .register_collector("e2e-col-a".to_string(), PROTOCOL_VERSION_CURRENT, true)
        .await
        .unwrap();
    let initial_sync = assign_rx.recv().await.expect("initial sync");
    let assigned_gen = initial_sync.generation;
    let collector_session_gen = initial_sync.collector_session_generation;
    assert!(
        lab.manager
            .validate_session_authority("e2e-col-a", &device_addr, epoch, assigned_gen)
    );

    // Tee the lifecycle channel so the test observes the minted
    // session identity, then forwards to the real RPC forwarder.
    let recorded = Arc::new(std::sync::Mutex::new(Vec::<
        bonsai::subscription_sessions::SessionLifecycleEvent,
    >::new()));
    let (sub_tx, mut sub_rx) =
        mpsc::channel::<bonsai::subscription_sessions::SessionLifecycleEvent>(256);
    let (lc_tx, lc_rx) = mpsc::channel::<bonsai::subscription_sessions::SessionLifecycleEvent>(256);
    {
        let recorded = Arc::clone(&recorded);
        let fwd_tx = lc_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = sub_rx.recv().await {
                recorded.lock().unwrap().push(ev.clone());
                if fwd_tx.send(ev).await.is_err() {
                    break;
                }
            }
        });
    }
    let tls_files = write_tls_files(&pki, &leaf_a, lab._dir.path());
    let runtime_cfg = Arc::new(RuntimeConfig {
        collector_id: "e2e-col-a".to_string(),
        core_ingest_endpoint: format!("https://{}", lab.addr),
        tls: tls_files,
        ..Default::default()
    });
    let (fsd, fsrx) = watch::channel(false);
    let lifecycle_fwd = tokio::spawn(run_lifecycle_forwarder(
        Arc::clone(&runtime_cfg),
        "e2e-col-a".to_string(),
        lc_rx,
        fsrx,
    ));

    // Collector B's authenticated clients — a DIFFERENT principal that
    // will deliberately claim A's identifiers.
    let client_b = connect_client(&pki, &leaf_b, lab.addr).await;
    let send_foreign = {
        let client = client_b.clone();
        let device_addr = device_addr.clone();
        move |plan_id: String, session_id: String, flattened: &str, gnmi: &str| {
            let client = client.clone();
            let device_addr = device_addr.clone();
            let flattened = flattened.to_string();
            let gnmi = gnmi.to_string();
            async move {
                let structured = bonsai::gnmi_path::parse_path("openconfig", &gnmi)
                    .expect("structured path parses");
                let update = bonsai::telemetry::TelemetryUpdate {
                    target: device_addr,
                    vendor: String::new(),
                    hostname: String::new(),
                    role: String::new(),
                    site: String::new(),
                    timestamp_ns: 1,
                    path: flattened,
                    value: serde_json::json!({"oper-status": "up"}),
                    plan_id,
                    session_id,
                    source_protocol: "gnmi".to_string(),
                    gnmi_path: vec![structured],
                    is_delete: false,
                    collector_id: String::new(),
                };
                let proto =
                    telemetry_to_ingest_update("e2e-col-b", &update).expect("proto conversion");
                let mut c = client;
                let _ = c
                    .telemetry_ingest(Request::new(tokio_stream::iter(vec![proto])))
                    .await;
            }
        }
    };

    // Collector A's bus → TelemetryIngest forwarder (same as prod).
    let collector_bus = InProcessBus::new(256);
    let mut ingest_rx = collector_bus.subscribe();
    let ingest_client = connect_client(&pki, &leaf_a, lab.addr).await;
    let (isd, mut is_rx) = watch::channel(false);
    let ingest_fwd = tokio::spawn(async move {
        let client = ingest_client;
        loop {
            tokio::select! {
                _ = is_rx.changed() => break,
                update = ingest_rx.recv() => {
                    let Ok(update) = update else { break };
                    let proto = match telemetry_to_ingest_update("e2e-col-a", &update) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    let mut c = client.clone();
                    tokio::spawn(async move {
                        let _ = c
                            .telemetry_ingest(Request::new(tokio_stream::iter(vec![proto])))
                            .await;
                    });
                }
            }
        }
    });

    // Real subscriber for Collector A (explicit plan, 3 paths).
    let lifecycle = LifecycleHandle {
        tx: sub_tx,
        authority_epoch: epoch,
        assignment_generation: assigned_gen,
        collector_session_generation: collector_session_gen,
    };
    let subscriber = GnmiSubscriber::new(
        device_addr.clone(),
        None,
        None,
        None,
        Some("e2e-dut".to_string()),
        None,
        Some("dc-e2e".to_string()),
        bonsai::gnmi_security::ConnSecurity::plaintext(true),
        Arc::clone(&collector_bus),
        None,
        None,
        device.selected_paths.clone(),
        AcquisitionMode::Explicit,
        Some(lifecycle),
        std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
    );
    let (ssd, mut ssrx) = watch::channel(false);
    let sub_handle = tokio::spawn(async move { subscriber.subscribe_telemetry(&mut ssrx).await });
    let _ = ssd;

    assert!(
        wait_for(
            || subscribe_calls.load(Ordering::SeqCst) >= 1,
            Duration::from_secs(10),
            "subscribe rpc"
        )
        .await
    );

    // Wait for the `submitted` transition to learn the minted session
    // identity, then fire Collector B's PRE-SYNCHRONIZATION foreign
    // observations — before any SyncResponse has been sent.
    let (plan_id, session_id) = {
        let mut found = None;
        for _ in 0..100 {
            let evs = recorded.lock().unwrap();
            if let Some(ev) = evs.iter().find(|e| e.state == "submitted") {
                found = Some((ev.plan_id.clone(), ev.session_id.clone()));
                break;
            }
            drop(evs);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        found.expect("submitted lifecycle event recorded")
    };

    let path_b_expectation = "interfaces/interface[name=Ethernet1/2]/state/oper-status";
    // (1) Unclassified flattened path + structured path matching an
    // expectation — the dangerous case: Ignored authority skips
    // assigned-device authorization, so only the collector stamp fences it.
    send_foreign(
        plan_id.clone(),
        session_id.clone(),
        "unclassified/foreign-leaf",
        path_b_expectation,
    )
    .await;
    // (2) Recognized flattened path (interface oper-status classifies as
    // device state) — denied even earlier at observation authorization.
    send_foreign(
        plan_id.clone(),
        session_id.clone(),
        "interfaces/interface[name=Ethernet1/2]/state/oper-status",
        path_b_expectation,
    )
    .await;

    // Now complete A's initial sync and deliver A's own legitimate
    // early evidence for path A (buffered → released post-sync).
    {
        let guard = streams.lock().await;
        let tx = guard.last().expect("subscribe stream").clone();
        tx.send(Ok(update_notification(&leaf_path("Ethernet1/1"), 42)))
            .await
            .unwrap();
        tx.send(Ok(sync_notification())).await.unwrap();
    }

    assert!(
        wait_for(
            || {
                lab.sessions
                    .current_session(&device_addr)
                    .is_some_and(|(p, s)| !p.is_empty() && !s.is_empty())
            },
            Duration::from_secs(10),
            "session registered"
        )
        .await,
        "lifecycle registration never reached core registry"
    );

    // Collector B POST-SYNCHRONIZATION foreign observations against
    // path B and path C — neither has any legitimate evidence.
    send_foreign(
        plan_id.clone(),
        session_id.clone(),
        "unclassified/foreign-leaf-2",
        path_b_expectation,
    )
    .await;
    send_foreign(
        plan_id.clone(),
        session_id.clone(),
        "interfaces/interface[name=Ethernet1/3]/state/oper-status",
        "interfaces/interface[name=Ethernet1/3]/state/oper-status",
    )
    .await;

    let path_a = "interfaces/interface[name=Ethernet1/1]/state/counters";
    let path_b = path_b_expectation;
    let path_c = "interfaces/interface[name=Ethernet1/3]/state/oper-status";

    // Positive control: A's own buffered evidence yields observed for
    // path A ONLY.
    let mut last_rows = Vec::new();
    let mut a_observed = false;
    for _ in 0..100 {
        last_rows = query_subscription_statuses(&lab.graph).await;
        if last_rows
            .iter()
            .any(|r| r.0 == device_addr && r.1 == path_a && r.4 == "observed")
        {
            a_observed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        a_observed,
        "legitimate path A coverage missing: {last_rows:?}"
    );
    // Foreign evidence must NEVER satisfy B or C — give the verifier
    // several sweep intervals to (incorrectly) do so before asserting.
    tokio::time::sleep(Duration::from_secs(12)).await;
    last_rows = query_subscription_statuses(&lab.graph).await;
    assert!(
        last_rows
            .iter()
            .all(|r| !(r.0 == device_addr && r.1 == path_b && r.4 == "observed")),
        "foreign collector satisfied path B: {last_rows:?}"
    );
    assert!(
        last_rows
            .iter()
            .all(|r| !(r.0 == device_addr && r.1 == path_c && r.4 == "observed")),
        "foreign collector satisfied path C: {last_rows:?}"
    );

    // Authority withdrawal: core-side removal retires the session
    // immediately; delayed evidence under the old identity must not
    // re-establish coverage, and observed history must reconcile to
    // stale rather than remain "observed".
    lab.registry
        .assign_device_with_audit(&device_addr, None, "test", "unassign")
        .unwrap();
    lab.manager.resync_collector("e2e-col-a").await;
    assert!(
        wait_for(
            || lab.sessions.current_session(&device_addr).is_none(),
            Duration::from_secs(10),
            "session retired"
        )
        .await,
        "withdrawal did not retire the session"
    );

    // Delayed lifecycle evidence stamped with the previously-valid
    // generation must be rejected at the authority gate — the same
    // check the authenticated RPC performs before registry mutation.
    assert!(
        !lab.manager
            .validate_session_authority("e2e-col-a", &device_addr, epoch, assigned_gen),
        "withdrawn generation still validates as current authority"
    );
    let stale_ev = bonsai::subscription_sessions::SessionLifecycleEvent {
        collector_id: "e2e-col-a".to_string(),
        target: device_addr.clone(),
        plan_id: plan_id.clone(),
        session_id: session_id.clone(),
        state: "synchronized".to_string(),
        transition_seq: 90,
        authority_epoch: epoch,
        assignment_generation: assigned_gen,
        authority_instance: 0,
        collector_session_generation: collector_session_gen,
        execution_generation: 1,
        occurred_at_ns: 0,
        detail: String::new(),
        submitted_paths: Vec::new(),
        acquisition: String::new(),
        schema_binding: None,
    };
    let outcome = lab.sessions.apply(&stale_ev, true);
    assert!(
        !matches!(
            outcome,
            bonsai::subscription_sessions::ApplyOutcome::Applied
        ),
        "delayed post-withdrawal lifecycle event was applied: {outcome:?}"
    );

    // Even the LEGITIMATE collector's delayed telemetry under the
    // retired session cannot restore coverage.
    {
        let guard = streams.lock().await;
        let tx = guard.last().expect("subscribe stream").clone();
        tx.send(Ok(update_notification(&leaf_path("Ethernet1/1"), 43)))
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_secs(12)).await;
    last_rows = query_subscription_statuses(&lab.graph).await;
    assert!(
        last_rows
            .iter()
            .all(|r| !(r.0 == device_addr && r.1 == path_a && r.4 == "observed")),
        "withdrawn session retained observed coverage: {last_rows:?}"
    );

    fsd.send(true).unwrap();
    isd.send(true).unwrap();
    ssd.send(true).unwrap();
    gsd.send(true).unwrap();
    let _ = lifecycle_fwd.await;
    let _ = ingest_fwd.await;
    let _ = sub_handle.await;
    let _ = ghandle.await;
    let _ = client_b;
}

// ── M5.2B review D: submitted-manifest authorization ────────────────────────

/// Forged-but-correctly-hashed manifests must still be rejected when they
/// exceed the authoritative assignment: explicit selections cannot broaden
/// authorized paths/modes/intervals, acquisition intent must agree with the
/// assignment, and a missing manifest never establishes a live execution.
#[tokio::test]
async fn lifecycle_manifest_must_agree_with_assignment() {
    use bonsai::api::pb::{SubscriptionLifecycleEvent, SubscriptionPath};

    let _ = rustls::crypto::ring::default_provider().install_default();
    let pki = make_pki();
    let leaf_a = issue_leaf(&pki, "e2e-col-a", false);
    let device_addr = "dut:57400".to_string();

    let mut device = device_target(&device_addr);
    device.collector_id = Some("e2e-col-a".to_string());
    let lab = start_lab(&pki, device.clone()).await;
    lab.registry
        .add_device_with_audit(device.clone(), "test", "test_add")
        .unwrap();
    lab.registry
        .assign_device_with_audit(
            &device_addr,
            Some("e2e-col-a".to_string()),
            "test",
            "test_assign",
        )
        .unwrap();
    bind_collector(&lab.sqlite, "e2e-col-a", &leaf_a.fingerprint);

    let epoch = lab.manager.authority_epoch();
    let (mut assign_rx, _gen) = lab
        .manager
        .register_collector("e2e-col-a".to_string(), PROTOCOL_VERSION_CURRENT, true)
        .await
        .unwrap();
    let initial_sync = assign_rx.recv().await.expect("initial sync");
    let assigned_gen = initial_sync.generation;
    let collector_session_gen = initial_sync.collector_session_generation;

    // The three authorized explicit selections (device_target).
    let auth_entries: Vec<(String, String, String, u64)> = vec![
        (
            "openconfig".to_string(),
            "interfaces/interface[name=Ethernet1/1]/state/counters".to_string(),
            "SAMPLE".to_string(),
            10_000_000_000,
        ),
        (
            "openconfig".to_string(),
            "interfaces/interface[name=Ethernet1/2]/state/oper-status".to_string(),
            "ON_CHANGE".to_string(),
            0,
        ),
        (
            "openconfig".to_string(),
            "interfaces/interface[name=Ethernet1/3]/state/oper-status".to_string(),
            "ON_CHANGE".to_string(),
            0,
        ),
    ];
    let to_pb = |entries: &[(String, String, String, u64)]| -> Vec<SubscriptionPath> {
        entries
            .iter()
            .map(|(origin, path, mode, interval)| SubscriptionPath {
                path: path.clone(),
                origin: origin.clone(),
                mode: mode.clone(),
                sample_interval_ns: *interval,
                ..Default::default()
            })
            .collect()
    };
    let ev = |plan_id: &str,
              session_id: &str,
              state: &str,
              seq: u64,
              acquisition: &str,
              paths: Vec<SubscriptionPath>| SubscriptionLifecycleEvent {
        collector_id: "e2e-col-a".to_string(),
        device_address: device_addr.clone(),
        plan_id: plan_id.to_string(),
        session_id: session_id.to_string(),
        state: state.to_string(),
        transition_seq: seq,
        authority_epoch: epoch,
        assignment_generation: assigned_gen,
        collector_session_generation: collector_session_gen,
        execution_generation: 1,
        occurred_at_ns: 1,
        detail: String::new(),
        protocol_version: PROTOCOL_VERSION_CURRENT,
        submitted_paths: paths,
        acquisition: acquisition.to_string(),
        schema_binding_mode: String::new(),
        schema_id: String::new(),
        evidence_fingerprint: String::new(),
        binding_fingerprint: String::new(),
        approval_revision: 0,
        snapshot_id: 0,
        datastore: String::new(),
        approved_by: String::new(),
        schema_binding_plan_id: String::new(),
    };

    // Broad manifest: the three authorized paths PLUS an unauthorized
    // fourth — plan_id is the CORRECT deterministic identity of the
    // broad manifest (integrity passes, authorization must not).
    let mut broad = auth_entries.clone();
    broad.push((
        "openconfig".to_string(),
        "network-instances/protocols/protocol/bgp".to_string(),
        "ON_CHANGE".to_string(),
        0,
    ));
    let broad_plan = bonsai::subscriber::acquisition_plan_id(AcquisitionMode::Explicit, &broad);
    let good_plan =
        bonsai::subscriber::acquisition_plan_id(AcquisitionMode::Explicit, &auth_entries);

    let (ev_tx, ev_rx) = mpsc::channel::<SubscriptionLifecycleEvent>(16);
    let mut client = connect_client(&pki, &leaf_a, lab.addr).await;
    let call = tokio::spawn(async move {
        client
            .subscription_lifecycle(Request::new(ReceiverStream::new(ev_rx)))
            .await
    });

    // 1. Correctly-hashed but unauthorized broader manifest → rejected.
    ev_tx
        .send(ev(
            &broad_plan,
            "sess-broad",
            "submitted",
            1,
            "explicit",
            to_pb(&broad),
        ))
        .await
        .unwrap();
    // 2. Missing manifest on a live execution state → rejected.
    ev_tx
        .send(ev(
            "whatever",
            "sess-nomanifest",
            "submitted",
            1,
            "explicit",
            vec![],
        ))
        .await
        .unwrap();
    // 3. Acquisition intent disagreeing with the explicit assignment →
    // rejected even though the manifest itself is authorized.
    ev_tx
        .send(ev(
            &good_plan,
            "sess-wrongacq",
            "submitted",
            1,
            "automatic",
            to_pb(&auth_entries),
        ))
        .await
        .unwrap();
    // 4. Positive control: exact authorized manifest → accepted and the
    // session becomes current.
    for (state, seq) in [("submitted", 1), ("synchronizing", 2), ("synchronized", 3)] {
        ev_tx
            .send(ev(
                &good_plan,
                "sess-good",
                state,
                seq,
                "explicit",
                to_pb(&auth_entries),
            ))
            .await
            .unwrap();
    }
    drop(ev_tx);

    let resp = call
        .await
        .expect("lifecycle rpc task")
        .expect("lifecycle rpc")
        .into_inner();
    assert_eq!(
        resp.rejected, 3,
        "broad/missing/mismatched manifests must each be rejected: {resp:?}"
    );
    assert_eq!(resp.accepted, 3, "authorized manifest lifecycle accepted");

    // Only the authorized session may hold current coverage authority.
    assert_eq!(
        lab.sessions.current_session(&device_addr),
        Some((good_plan.clone(), "sess-good".to_string())),
        "rejected manifests must never register coverage authority"
    );

    let _ = lab;
}

// ── M5.2B review C: authority renewal across core restart + reconnect ──────

/// A restarted core (new authority epoch, empty session registry) must
/// recover current coverage from the healthy long-lived device
/// subscription when the assignment configuration is unchanged — the
/// collector-side fingerprint must NOT preserve a subscriber whose
/// lifecycle authority belongs to the dead epoch. A same-authority
/// confirming resync must NOT restart it, and a collector
/// assignment-stream reconnect inside the session-generation fence
/// must re-establish execution identity promptly. Old authority can
/// never regain coverage on the new core.
///
/// Production-wired: real `handle_assignment_update` + subscriber
/// spawn/fingerprint path, real lifecycle forwarder over authenticated
/// mTLS, real core RPC authority validation, real session registry.
/// Two `start_lab` cores stand in for pre/post-restart boots (each has
/// its own epoch high-water, registry and session registry).
#[tokio::test]
async fn authority_renewal_recovers_coverage_across_core_restart() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let pki = make_pki();
    let collector_leaf = issue_leaf(&pki, "e2e-col", false);

    let (gnmi_addr, streams, _requests, subscribe_calls, gsd, ghandle) = start_mock().await;
    let device_addr = gnmi_addr
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();

    // Explicit plaintext is the deliberate isolated-lab opt-in required
    // for acquisition (same convention as the assignment-lifecycle rig).
    let mut device = device_target(&device_addr);
    device.security = Some(GnmiSecurityMode::Plaintext);
    device.collector_id = Some("e2e-col".to_string());

    // ── Phase 1: first core boot — establish verified coverage ──
    let lab1 = start_lab(&pki, device.clone()).await;
    lab1.registry
        .add_device_with_audit(device.clone(), "test", "test_add")
        .unwrap();
    lab1.registry
        .assign_device_with_audit(
            &device_addr,
            Some("e2e-col".to_string()),
            "test",
            "test_assign",
        )
        .unwrap();
    bind_collector(&lab1.sqlite, "e2e-col", &collector_leaf.fingerprint);

    let (mut assign_rx1, session_gen1) = lab1
        .manager
        .register_collector("e2e-col".to_string(), PROTOCOL_VERSION_CURRENT, true)
        .await
        .unwrap();
    let sync1 = assign_rx1.recv().await.expect("initial sync");
    let epoch1 = sync1.authority_epoch;
    let sync1_gen = sync1.generation;
    let sync1_sg = sync1.collector_session_generation;
    assert!(epoch1 > 0);
    assert_eq!(sync1_sg, 1);

    // Collector-side rig shared across all phases: the REAL assignment
    // update path, subscriber map and per-update lifecycle channel.
    let bus = InProcessBus::new(64);
    let mut subscribers = bonsai::subscriber::SubscriberHandleMap::new();

    // Collector bus → authenticated TelemetryIngest RPC against the
    // CURRENT core (the client slot is swapped across the restart).
    // This is the same wire path production collectors use, so the
    // persisted SubscriptionStatus rows asserted below are produced by
    // real authenticated telemetry ingestion — not registry identity
    // alone (M5.2B review B4).
    let telemetry_client: Arc<std::sync::Mutex<Option<BonsaiGraphClient<Channel>>>> =
        Arc::new(std::sync::Mutex::new(None));
    // Every telemetry_ingest RPC outcome is recorded — a silently
    // discarded transport/authorization failure must never be
    // indistinguishable from accepted coverage evidence.
    let telemetry_rpc_log: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut telemetry_rx = bus.subscribe();
    let telemetry_fwd = {
        let client_slot = Arc::clone(&telemetry_client);
        let rpc_log = Arc::clone(&telemetry_rpc_log);
        tokio::spawn(async move {
            while let Ok(update) = telemetry_rx.recv().await {
                let update = Arc::unwrap_or_clone(update);
                let proto = match telemetry_to_ingest_update("e2e-col", &update) {
                    Ok(p) => p,
                    Err(e) => {
                        rpc_log.lock().unwrap().push(format!("proto-error: {e}"));
                        continue;
                    }
                };
                let client = client_slot.lock().unwrap().clone();
                let Some(mut client) = client else { continue };
                let log = Arc::clone(&rpc_log);
                tokio::spawn(async move {
                    match client
                        .telemetry_ingest(Request::new(tokio_stream::iter(vec![proto])))
                        .await
                    {
                        Ok(resp) => {
                            let r = resp.into_inner();
                            log.lock().unwrap().push(format!(
                                "accepted={} rejected={} error={:?}",
                                r.accepted, r.rejected, r.error
                            ));
                        }
                        Err(status) => {
                            log.lock().unwrap().push(format!("rpc-error: {status}"));
                        }
                    }
                });
            }
        })
    };
    let (plan_tx, _plan_rx) = mpsc::channel::<bonsai::subscription_status::SubscriptionPlan>(64);
    let plans = Some(plan_tx);
    let mut last_epoch = 0u64;
    let mut last_gen = 0u64;

    *telemetry_client.lock().unwrap() =
        Some(connect_client(&pki, &collector_leaf, lab1.addr).await);
    let (lc1_tx, lc1_rx) =
        mpsc::channel::<bonsai::subscription_sessions::SessionLifecycleEvent>(256);
    let tls1 = write_tls_files(&pki, &collector_leaf, lab1._dir.path());
    let fwd1_cfg = Arc::new(RuntimeConfig {
        collector_id: "e2e-col".to_string(),
        core_ingest_endpoint: format!("https://{}", lab1.addr),
        tls: tls1,
        ..Default::default()
    });
    let (fwd1_sd, fwd1_rx) = watch::channel(false);
    let _fwd1 = tokio::spawn(run_lifecycle_forwarder(
        fwd1_cfg,
        "e2e-col".to_string(),
        lc1_rx,
        fwd1_rx,
    ));
    let _ = fwd1_sd;

    assert!(
        handle_assignment_update(
            sync1,
            &bus,
            &plans,
            &mut subscribers,
            &mut last_epoch,
            &mut last_gen,
            &Some(lc1_tx.clone()),
        )
        .await
    );

    // Complete the device-facing Subscribe → synchronized lifecycle.
    assert!(
        wait_for(
            || subscribe_calls.load(Ordering::SeqCst) >= 1,
            Duration::from_secs(10),
            "epoch1 subscribe rpc"
        )
        .await
    );
    assert!(
        wait_for(
            || streams.try_lock().map(|g| !g.is_empty()).unwrap_or(false),
            Duration::from_secs(10),
            "epoch1 stream"
        )
        .await
    );
    {
        let guard = streams.lock().await;
        let tx = guard.last().expect("subscribe stream").clone();
        tx.send(Ok(update_notification(&leaf_path("Ethernet1/1"), 7)))
            .await
            .unwrap();
        tx.send(Ok(sync_notification())).await.unwrap();
    }
    assert!(
        wait_for(
            || lab1.sessions.current_session(&device_addr).is_some(),
            Duration::from_secs(10),
            "epoch1 verified coverage"
        )
        .await,
        "first authority never established coverage"
    );
    let session1 = lab1
        .sessions
        .current_session(&device_addr)
        .expect("coverage")
        .1;
    let calls_phase1 = subscribe_calls.load(Ordering::SeqCst);

    // B4: exact persisted coverage on the FIRST authority — path A
    // observed via authenticated telemetry; paths B/C remain pending
    // (no matching evidence was ever injected for them).
    assert_exact_coverage(
        &lab1.graph,
        &device_addr,
        "epoch1-initial",
        &lab1.sessions,
        &telemetry_rpc_log,
    )
    .await;

    // ── Phase 2: core restart — new epoch, empty registry, SAME
    // assignment configuration. The subscriber must be re-established
    // under the new authority, not preserved with the dead epoch. ──
    let lab2 = start_lab(&pki, device.clone()).await;
    lab2.registry
        .add_device_with_audit(device.clone(), "test", "test_add")
        .unwrap();
    lab2.registry
        .assign_device_with_audit(
            &device_addr,
            Some("e2e-col".to_string()),
            "test",
            "test_assign",
        )
        .unwrap();
    bind_collector(&lab2.sqlite, "e2e-col", &collector_leaf.fingerprint);
    *telemetry_client.lock().unwrap() =
        Some(connect_client(&pki, &collector_leaf, lab2.addr).await);
    let epoch2 = lab2.manager.authority_epoch();
    assert!(
        epoch2 > epoch1,
        "restarted core must mint a strictly newer authority epoch"
    );

    let (mut assign_rx2, session_gen2) = lab2
        .manager
        .register_collector("e2e-col".to_string(), PROTOCOL_VERSION_CURRENT, true)
        .await
        .unwrap();
    let sync2 = assign_rx2.recv().await.expect("restart sync");
    let sync2_gen = sync2.generation;
    assert_eq!(sync2.authority_epoch, epoch2);

    // New lifecycle channel + forwarder bound to the restarted core.
    // The old forwarder keeps retrying the dead endpoint harmlessly.
    let (lc2_tx, lc2_rx) =
        mpsc::channel::<bonsai::subscription_sessions::SessionLifecycleEvent>(256);
    let tls2 = write_tls_files(&pki, &collector_leaf, lab2._dir.path());
    let fwd2_cfg = Arc::new(RuntimeConfig {
        collector_id: "e2e-col".to_string(),
        core_ingest_endpoint: format!("https://{}", lab2.addr),
        tls: tls2,
        ..Default::default()
    });
    let (_fwd2_sd, fwd2_rx) = watch::channel(false);
    let _fwd2 = tokio::spawn(run_lifecycle_forwarder(
        fwd2_cfg,
        "e2e-col".to_string(),
        lc2_rx,
        fwd2_rx,
    ));

    assert!(
        handle_assignment_update(
            sync2,
            &bus,
            &plans,
            &mut subscribers,
            &mut last_epoch,
            &mut last_gen,
            &Some(lc2_tx.clone()),
        )
        .await
    );

    // The epoch change must have replaced the subscriber: a NEW
    // Subscribe RPC — monitoring resumes promptly, it does not wait
    // for the device connection to fail.
    let streams_phase1 = streams.lock().await.len();
    assert!(
        wait_for(
            || subscribe_calls.load(Ordering::SeqCst) > calls_phase1,
            Duration::from_secs(10),
            "post-restart subscribe rpc"
        )
        .await,
        "authority renewal did not re-establish the device subscription"
    );
    assert!(
        wait_for(
            || streams
                .try_lock()
                .map(|g| g.len() > streams_phase1)
                .unwrap_or(false),
            Duration::from_secs(10),
            "post-restart stream"
        )
        .await
    );
    {
        let guard = streams.lock().await;
        let tx = guard.last().expect("new subscribe stream").clone();
        tx.send(Ok(update_notification(&leaf_path("Ethernet1/1"), 8)))
            .await
            .unwrap();
        tx.send(Ok(sync_notification())).await.unwrap();
    }
    assert!(
        wait_for(
            || lab2.sessions.current_session(&device_addr).is_some(),
            Duration::from_secs(10),
            "post-restart verified coverage"
        )
        .await,
        "restarted core never recovered coverage"
    );
    let session2 = lab2
        .sessions
        .current_session(&device_addr)
        .expect("coverage")
        .1;
    assert_ne!(
        session1, session2,
        "new authority must mint a fresh execution identity"
    );

    // B4: the renewed authority must reproduce the SAME exact
    // persisted coverage — registry identity alone is not proof that
    // observed path coverage survived the core restart.
    assert_exact_coverage(
        &lab2.graph,
        &device_addr,
        "post-restart-verified",
        &lab2.sessions,
        &telemetry_rpc_log,
    )
    .await;

    // Old authority cannot regain coverage on the new core: a delayed
    // epoch-1-stamped lifecycle event is fenced before the registry.
    {
        let (ev_tx, ev_rx) = mpsc::channel::<bonsai::api::pb::SubscriptionLifecycleEvent>(4);
        let mut stale_client = connect_client(&pki, &collector_leaf, lab2.addr).await;
        let stale_call = tokio::spawn(async move {
            stale_client
                .subscription_lifecycle(Request::new(ReceiverStream::new(ev_rx)))
                .await
        });
        let good_plan = bonsai::subscriber::acquisition_plan_id(
            AcquisitionMode::Explicit,
            &device
                .selected_paths
                .iter()
                .map(|p| {
                    (
                        p.origin.clone(),
                        p.path.clone(),
                        p.mode.clone(),
                        p.sample_interval_ns,
                    )
                })
                .collect::<Vec<_>>(),
        );
        ev_tx
            .send(bonsai::api::pb::SubscriptionLifecycleEvent {
                collector_id: "e2e-col".to_string(),
                device_address: device_addr.clone(),
                plan_id: good_plan,
                session_id: "stale-epoch-session".to_string(),
                state: "synchronized".to_string(),
                transition_seq: 1,
                authority_epoch: epoch1,
                assignment_generation: sync1_gen,
                collector_session_generation: sync1_sg,
                execution_generation: 1,
                occurred_at_ns: 1,
                detail: String::new(),
                protocol_version: PROTOCOL_VERSION_CURRENT,
                submitted_paths: vec![],
                acquisition: "explicit".to_string(),
                schema_binding_mode: String::new(),
                schema_id: String::new(),
                evidence_fingerprint: String::new(),
                binding_fingerprint: String::new(),
                approval_revision: 0,
                snapshot_id: 0,
                datastore: String::new(),
                approved_by: String::new(),
                schema_binding_plan_id: String::new(),
            })
            .await
            .unwrap();
        drop(ev_tx);
        let resp = stale_call
            .await
            .expect("stale rpc task")
            .expect("stale rpc")
            .into_inner();
        assert!(
            resp.rejected >= 1,
            "stale-epoch lifecycle event must be rejected: {resp:?}"
        );
        assert_eq!(
            lab2.sessions.current_session(&device_addr).map(|(_, s)| s),
            Some(session2.clone()),
            "stale authority must not disturb current coverage"
        );
    }

    // ── Phase 3: same-authority confirming resync must NOT restart the
    // healthy subscriber (fingerprint stable across generation bumps). ──
    lab2.manager.resync_collector("e2e-col").await;
    let sync3 = assign_rx2.recv().await.expect("resync update");
    assert_eq!(sync3.authority_epoch, epoch2);
    assert!(sync3.generation > sync2_gen);
    let calls_before_resync = subscribe_calls.load(Ordering::SeqCst);
    assert!(
        handle_assignment_update(
            sync3,
            &bus,
            &plans,
            &mut subscribers,
            &mut last_epoch,
            &mut last_gen,
            &Some(lc2_tx.clone()),
        )
        .await
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        subscribe_calls.load(Ordering::SeqCst),
        calls_before_resync,
        "confirming same-authority resync must preserve the healthy subscriber"
    );
    assert_eq!(
        lab2.sessions.current_session(&device_addr).map(|(_, s)| s),
        Some(session2.clone()),
        "resync must not disturb current coverage"
    );
    assert_exact_coverage(
        &lab2.graph,
        &device_addr,
        "post-resync",
        &lab2.sessions,
        &telemetry_rpc_log,
    )
    .await;

    // ── Phase 4: collector assignment-stream reconnect inside the
    // session-generation fence. New session registers first; the stale
    // stream's close is then fenced out — devices and coverage are
    // retained, and the new collector session re-establishes execution
    // identity promptly. ──
    let (mut assign_rx4, session_gen4) = lab2
        .manager
        .register_collector("e2e-col".to_string(), PROTOCOL_VERSION_CURRENT, true)
        .await
        .unwrap();
    assert!(
        session_gen4 > session_gen2,
        "reconnect must mint a newer collector session generation"
    );
    // The OLD stream's close arrives after the new session exists —
    // generation fencing makes it a no-op (devices stay assigned).
    assert!(
        !lab2.manager.unregister_collector("e2e-col", session_gen2),
        "stale stream close must be fenced once the new session exists"
    );
    let sync4 = assign_rx4.recv().await.expect("reconnect sync");
    assert_eq!(sync4.authority_epoch, epoch2);
    assert_eq!(sync4.collector_session_generation, session_gen4);
    let calls_before_reconnect = subscribe_calls.load(Ordering::SeqCst);
    let streams_before_reconnect = streams.lock().await.len();
    assert!(
        handle_assignment_update(
            sync4,
            &bus,
            &plans,
            &mut subscribers,
            &mut last_epoch,
            &mut last_gen,
            &Some(lc2_tx.clone()),
        )
        .await
    );
    // New collector session → new execution identity → subscriber
    // re-established WITHOUT waiting for the device connection to fail.
    assert!(
        wait_for(
            || subscribe_calls.load(Ordering::SeqCst) > calls_before_reconnect,
            Duration::from_secs(10),
            "reconnect subscribe rpc"
        )
        .await,
        "collector reconnect did not re-establish the subscription"
    );
    assert!(
        wait_for(
            || streams
                .try_lock()
                .map(|g| g.len() > streams_before_reconnect)
                .unwrap_or(false),
            Duration::from_secs(10),
            "reconnect stream"
        )
        .await
    );
    {
        let guard = streams.lock().await;
        let tx = guard.last().expect("reconnect stream").clone();
        tx.send(Ok(update_notification(&leaf_path("Ethernet1/1"), 9)))
            .await
            .unwrap();
        tx.send(Ok(sync_notification())).await.unwrap();
    }
    assert!(
        wait_for(
            || {
                lab2.sessions
                    .current_session(&device_addr)
                    .is_some_and(|(_, s)| s != session2)
            },
            Duration::from_secs(10),
            "post-reconnect coverage"
        )
        .await,
        "reconnect never restored current coverage"
    );
    let session3 = lab2
        .sessions
        .current_session(&device_addr)
        .expect("coverage")
        .1;
    assert_ne!(session2, session3, "reconnect must mint a new execution");
    assert_exact_coverage(
        &lab2.graph,
        &device_addr,
        "post-reconnect",
        &lab2.sessions,
        &telemetry_rpc_log,
    )
    .await;

    // ── Phase 5: a GENUINE current-generation disconnect retires session
    // authority immediately — old authority cannot linger. ──
    assert!(
        lab2.manager.unregister_collector("e2e-col", session_gen4),
        "current-generation unregister must run cleanup"
    );
    assert!(
        wait_for(
            || lab2.sessions.current_session(&device_addr).is_none(),
            Duration::from_secs(10),
            "disconnect retirement"
        )
        .await,
        "collector disconnect must retire session authority"
    );

    // Reassignment + resync restores monitoring promptly: new authority
    // window → new execution identity → coverage returns without any
    // device-side failure.
    lab2.registry
        .assign_device_with_audit(
            &device_addr,
            Some("e2e-col".to_string()),
            "test",
            "test_reassign",
        )
        .unwrap();
    let (mut assign_rx5, _session_gen5) = lab2
        .manager
        .register_collector("e2e-col".to_string(), PROTOCOL_VERSION_CURRENT, true)
        .await
        .unwrap();
    let sync5 = assign_rx5.recv().await.expect("reassign sync");
    assert!(
        handle_assignment_update(
            sync5,
            &bus,
            &plans,
            &mut subscribers,
            &mut last_epoch,
            &mut last_gen,
            &Some(lc2_tx.clone()),
        )
        .await
    );
    let streams_before_reassign = streams.lock().await.len();
    assert!(
        wait_for(
            || subscribe_calls.load(Ordering::SeqCst) > calls_before_reconnect + 1,
            Duration::from_secs(10),
            "post-reassign subscribe rpc"
        )
        .await,
        "reassignment did not restart acquisition"
    );
    assert!(
        wait_for(
            || streams
                .try_lock()
                .map(|g| g.len() > streams_before_reassign)
                .unwrap_or(false),
            Duration::from_secs(10),
            "post-reassign stream"
        )
        .await
    );
    {
        let guard = streams.lock().await;
        let tx = guard.last().expect("reassign stream").clone();
        tx.send(Ok(update_notification(&leaf_path("Ethernet1/1"), 10)))
            .await
            .unwrap();
        tx.send(Ok(sync_notification())).await.unwrap();
    }
    assert!(
        wait_for(
            || lab2.sessions.current_session(&device_addr).is_some(),
            Duration::from_secs(10),
            "post-reassign coverage"
        )
        .await,
        "monitoring did not resume after reassignment"
    );
    assert_exact_coverage(
        &lab2.graph,
        &device_addr,
        "post-reassign",
        &lab2.sessions,
        &telemetry_rpc_log,
    )
    .await;

    // The predecessor core legitimately loses coverage: the collector
    // renewed authority under the new epoch, so its epoch-1 session was
    // terminated during the subscriber replacement and can never be
    // resurrected there either.
    assert!(
        lab1.sessions.current_session(&device_addr).is_none(),
        "predecessor authority must not retain coverage after renewal"
    );

    let _ = gsd.send(true);
    let _ = ghandle.await;
    let _ = telemetry_fwd.abort();
    let _ = session_gen1;
}

/// B4 helper: poll the persisted graph until the exact expected
/// SubscriptionStatus rows are present — path A `observed` with exact
/// device/path/origin/mode identity, paths B/C `pending` (registered
/// but never evidenced). `phase`/`ctx` identify WHICH recovery phase
/// failed and carry the authoritative session/execution identity and
/// telemetry RPC outcomes observed up to that point.
async fn assert_exact_coverage(
    graph: &GraphStore,
    device_addr: &str,
    phase: &str,
    sessions: &bonsai::subscription_sessions::SubscriptionSessionRegistry,
    telemetry_rpc_log: &std::sync::Mutex<Vec<String>>,
) {
    let path_a = "interfaces/interface[name=Ethernet1/1]/state/counters";
    let path_b = "interfaces/interface[name=Ethernet1/2]/state/oper-status";
    let path_c = "interfaces/interface[name=Ethernet1/3]/state/oper-status";
    let mut last_rows = Vec::new();
    for _ in 0..150 {
        last_rows = query_subscription_statuses(graph).await;
        let status_of = |path: &str| -> Option<&str> {
            last_rows
                .iter()
                .find(|r| r.0 == device_addr && r.1 == path)
                .map(|r| r.4.as_str())
        };
        if status_of(path_a) == Some("observed")
            && status_of(path_b) == Some("pending")
            && status_of(path_c) == Some("pending")
        {
            let row_a = last_rows
                .iter()
                .find(|r| r.0 == device_addr && r.1 == path_a)
                .expect("path A row");
            assert_eq!(row_a.2, "openconfig", "path A origin");
            assert_eq!(row_a.3, "SAMPLE", "path A mode");
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let provenance = sessions
        .current_session_provenance(device_addr)
        .map(|(p, s, c)| format!("plan={p} session={s} collector={c}"))
        .unwrap_or_else(|| "none".to_string());
    let rpc_tail: Vec<String> = telemetry_rpc_log
        .lock()
        .unwrap()
        .iter()
        .rev()
        .take(8)
        .rev()
        .cloned()
        .collect();
    panic!(
        "[{phase}] exact persisted coverage never reached\n  authority: {provenance}\n  telemetry rpc tail: {rpc_tail:?}\n  persisted rows: {last_rows:?}"
    );
}

// ── M5.2B review B3: collector registration generation fencing ───────────────

/// A delayed lifecycle event stamped with a SUPERSEDED collector
/// session generation must never establish authority — even before the
/// new registration's first device execution arrives. Generation zero
/// and post-disconnect events are fenced the same way.
#[tokio::test]
async fn stale_collector_session_generation_cannot_establish_authority() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let pki = make_pki();
    let collector_leaf = issue_leaf(&pki, "e2e-col", false);
    let (_gnmi_addr, _streams, _requests, _subscribe_calls, _gsd, _ghandle) = start_mock().await;

    let mut device = device_target("127.0.0.1:1");
    device.security = Some(GnmiSecurityMode::Plaintext);
    device.collector_id = Some("e2e-col".to_string());
    let device_addr = device.address.clone();

    let lab = start_lab(&pki, device.clone()).await;
    lab.registry
        .add_device_with_audit(device.clone(), "test", "test_add")
        .unwrap();
    lab.registry
        .assign_device_with_audit(
            &device_addr,
            Some("e2e-col".to_string()),
            "test",
            "test_assign",
        )
        .unwrap();
    bind_collector(&lab.sqlite, "e2e-col", &collector_leaf.fingerprint);

    // Explicit authorized manifest (same construction as the manifest
    // authorization test): plan_id is the deterministic identity.
    let entries: Vec<(String, String, String, u64)> = device
        .selected_paths
        .iter()
        .map(|p| {
            (
                p.origin.clone(),
                p.path.clone(),
                p.mode.clone(),
                p.sample_interval_ns,
            )
        })
        .collect();
    let good_plan = bonsai::subscriber::acquisition_plan_id(AcquisitionMode::Explicit, &entries);
    let to_pb =
        |entries: &[(String, String, String, u64)]| -> Vec<bonsai::api::pb::SubscriptionPath> {
            entries
                .iter()
                .map(
                    |(origin, path, mode, interval)| bonsai::api::pb::SubscriptionPath {
                        path: path.clone(),
                        origin: origin.clone(),
                        mode: mode.clone(),
                        sample_interval_ns: *interval,
                        ..Default::default()
                    },
                )
                .collect()
        };
    let manifest = to_pb(&entries);

    let epoch = lab.manager.authority_epoch();

    // Registration generation 1.
    let (mut assign_rx1, _sg1) = lab
        .manager
        .register_collector("e2e-col".to_string(), PROTOCOL_VERSION_CURRENT, true)
        .await
        .unwrap();
    let sync1 = assign_rx1.recv().await.expect("sync1");
    let assigned_gen1 = sync1.generation;
    let session_gen1 = sync1.collector_session_generation;

    let ev = |session_gen: u64,
              session: &str,
              exec_gen: u64,
              assigned_gen: u64,
              state: &str,
              seq: u64| bonsai::api::pb::SubscriptionLifecycleEvent {
        collector_id: "e2e-col".to_string(),
        device_address: device_addr.clone(),
        plan_id: good_plan.clone(),
        session_id: session.to_string(),
        state: state.to_string(),
        transition_seq: seq,
        authority_epoch: epoch,
        assignment_generation: assigned_gen,
        collector_session_generation: session_gen,
        execution_generation: exec_gen,
        occurred_at_ns: 1,
        detail: String::new(),
        protocol_version: PROTOCOL_VERSION_CURRENT,
        submitted_paths: manifest.clone(),
        acquisition: "explicit".to_string(),
        schema_binding_mode: String::new(),
        schema_id: String::new(),
        evidence_fingerprint: String::new(),
        binding_fingerprint: String::new(),
        approval_revision: 0,
        snapshot_id: 0,
        datastore: String::new(),
        approved_by: String::new(),
        schema_binding_plan_id: String::new(),
    };

    // Baseline: a gen-1-stamped synchronized execution IS admitted
    // while generation 1 is current.
    let (ev_tx, ev_rx) = mpsc::channel::<bonsai::api::pb::SubscriptionLifecycleEvent>(8);
    let mut client = connect_client(&pki, &collector_leaf, lab.addr).await;
    let call = tokio::spawn(async move {
        client
            .subscription_lifecycle(Request::new(ReceiverStream::new(ev_rx)))
            .await
    });
    for (state, seq) in [("submitted", 1), ("synchronized", 2)] {
        ev_tx
            .send(ev(session_gen1, "sess-gen1", 1, assigned_gen1, state, seq))
            .await
            .unwrap();
    }
    drop(ev_tx);
    let resp = call.await.expect("rpc").expect("resp").into_inner();
    assert_eq!(
        resp.rejected, 0,
        "gen-1 baseline must be accepted: {resp:?}"
    );
    assert!(
        wait_for(
            || lab.sessions.current_session(&device_addr).is_some(),
            Duration::from_secs(10),
            "gen-1 coverage"
        )
        .await
    );

    // Registration generation 2 — the collector reconnects before any
    // gen-2 device lifecycle event arrives.
    let (mut assign_rx2, session_gen2_reg) = lab
        .manager
        .register_collector("e2e-col".to_string(), PROTOCOL_VERSION_CURRENT, true)
        .await
        .unwrap();
    let sync2 = assign_rx2.recv().await.expect("sync2");
    let session_gen2 = sync2.collector_session_generation;
    assert!(session_gen2 > session_gen1);

    // A DELAYED gen-1 event attempting to register a different
    // execution must be fenced at the authenticated boundary.
    let (ev_tx2, ev_rx2) = mpsc::channel::<bonsai::api::pb::SubscriptionLifecycleEvent>(8);
    let mut client2 = connect_client(&pki, &collector_leaf, lab.addr).await;
    let call2 = tokio::spawn(async move {
        client2
            .subscription_lifecycle(Request::new(ReceiverStream::new(ev_rx2)))
            .await
    });
    for (state, seq) in [("submitted", 1), ("synchronized", 2)] {
        ev_tx2
            .send(ev(
                session_gen1,
                "sess-stale-gen1",
                50,
                sync2.generation,
                state,
                seq,
            ))
            .await
            .unwrap();
    }
    // Generation zero — never a valid registration.
    ev_tx2
        .send(ev(0, "sess-gen0", 1, sync2.generation, "synchronized", 1))
        .await
        .unwrap();
    drop(ev_tx2);
    let resp2 = call2.await.expect("rpc2").expect("resp2").into_inner();
    assert!(
        resp2.rejected >= 3,
        "stale/zero-generation events must be rejected: {resp2:?}"
    );
    assert_eq!(
        lab.sessions.current_session(&device_addr),
        Some((good_plan.clone(), "sess-gen1".to_string())),
        "stale registration must not disturb or replace current authority"
    );

    // Disconnected collector: after unregister + resync withdrawal,
    // even a current-generation-stamped event cannot establish
    // authority.
    assert!(
        lab.manager
            .unregister_collector("e2e-col", session_gen2_reg)
    );
    lab.registry
        .assign_device_with_audit(&device_addr, None, "test", "test_unassign")
        .unwrap();
    lab.manager.resync_collector("e2e-col").await;
    let _ = assign_rx2.recv().await;

    let (ev_tx3, ev_rx3) = mpsc::channel::<bonsai::api::pb::SubscriptionLifecycleEvent>(4);
    let mut client3 = connect_client(&pki, &collector_leaf, lab.addr).await;
    let call3 = tokio::spawn(async move {
        client3
            .subscription_lifecycle(Request::new(ReceiverStream::new(ev_rx3)))
            .await
    });
    ev_tx3
        .send(ev(
            session_gen2,
            "sess-post-disconnect",
            1,
            sync2.generation,
            "synchronized",
            1,
        ))
        .await
        .unwrap();
    drop(ev_tx3);
    let resp3 = call3.await.expect("rpc3").expect("resp3").into_inner();
    assert!(
        resp3.rejected >= 1,
        "post-disconnect event must be rejected: {resp3:?}"
    );
    assert_eq!(
        lab.sessions.current_session(&device_addr),
        None,
        "disconnected collector must not hold coverage authority"
    );
}

/// M5.2B review B3 residual: lifecycle admission must require the
/// collector registration to be CURRENTLY ACTIVE — not merely that
/// its session generation equals the issued high-water mark. After
/// `unregister_collector` retires the registration, a device whose
/// assignment still names the dead collector (the deactivation-to-
/// cleanup gap, reproduced deterministically by re-assigning without
/// re-registering) must NOT be able to establish session authority,
/// even though the authority window, epoch and issued-generation
/// fences all still validate.
#[tokio::test]
async fn lifecycle_admission_requires_active_registration() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let pki = make_pki();
    let collector_leaf = issue_leaf(&pki, "e2e-col", false);
    let (_gnmi_addr, _streams, _requests, _subscribe_calls, _gsd, _ghandle) = start_mock().await;

    let mut device = device_target("127.0.0.1:1");
    device.security = Some(GnmiSecurityMode::Plaintext);
    device.collector_id = Some("e2e-col".to_string());
    let device_addr = device.address.clone();

    let lab = start_lab(&pki, device.clone()).await;
    lab.registry
        .add_device_with_audit(device.clone(), "test", "test_add")
        .unwrap();
    lab.registry
        .assign_device_with_audit(
            &device_addr,
            Some("e2e-col".to_string()),
            "test",
            "test_assign",
        )
        .unwrap();
    bind_collector(&lab.sqlite, "e2e-col", &collector_leaf.fingerprint);

    let entries: Vec<(String, String, String, u64)> = device
        .selected_paths
        .iter()
        .map(|p| {
            (
                p.origin.clone(),
                p.path.clone(),
                p.mode.clone(),
                p.sample_interval_ns,
            )
        })
        .collect();
    let good_plan = bonsai::subscriber::acquisition_plan_id(AcquisitionMode::Explicit, &entries);
    let manifest: Vec<bonsai::api::pb::SubscriptionPath> = entries
        .iter()
        .map(
            |(origin, path, mode, interval)| bonsai::api::pb::SubscriptionPath {
                path: path.clone(),
                origin: origin.clone(),
                mode: mode.clone(),
                sample_interval_ns: *interval,
                ..Default::default()
            },
        )
        .collect();

    let epoch = lab.manager.authority_epoch();

    let (mut assign_rx, registration_gen) = lab
        .manager
        .register_collector("e2e-col".to_string(), PROTOCOL_VERSION_CURRENT, true)
        .await
        .unwrap();
    let sync = assign_rx.recv().await.expect("sync");
    let assigned_gen = sync.generation;
    let session_gen = sync.collector_session_generation;
    assert_eq!(session_gen, registration_gen);

    let ev = |session: &str, exec_gen: u64, state: &str, seq: u64| {
        bonsai::api::pb::SubscriptionLifecycleEvent {
            collector_id: "e2e-col".to_string(),
            device_address: device_addr.clone(),
            plan_id: good_plan.clone(),
            session_id: session.to_string(),
            state: state.to_string(),
            transition_seq: seq,
            authority_epoch: epoch,
            assignment_generation: assigned_gen,
            collector_session_generation: session_gen,
            execution_generation: exec_gen,
            occurred_at_ns: 1,
            detail: String::new(),
            protocol_version: PROTOCOL_VERSION_CURRENT,
            submitted_paths: manifest.clone(),
            acquisition: "explicit".to_string(),
            schema_binding_mode: String::new(),
            schema_id: String::new(),
            evidence_fingerprint: String::new(),
            binding_fingerprint: String::new(),
            approval_revision: 0,
            snapshot_id: 0,
            datastore: String::new(),
            approved_by: String::new(),
            schema_binding_plan_id: String::new(),
        }
    };

    // Baseline: while registered, the execution is admitted.
    let (ev_tx, ev_rx) = mpsc::channel::<bonsai::api::pb::SubscriptionLifecycleEvent>(8);
    let mut client = connect_client(&pki, &collector_leaf, lab.addr).await;
    let call = tokio::spawn(async move {
        client
            .subscription_lifecycle(Request::new(ReceiverStream::new(ev_rx)))
            .await
    });
    for (state, seq) in [("submitted", 1), ("synchronized", 2)] {
        ev_tx.send(ev("sess-active", 1, state, seq)).await.unwrap();
    }
    drop(ev_tx);
    let resp = call.await.expect("rpc").expect("resp").into_inner();
    assert_eq!(resp.rejected, 0, "baseline must be accepted: {resp:?}");
    assert!(
        wait_for(
            || lab.sessions.current_session(&device_addr).is_some(),
            Duration::from_secs(10),
            "baseline coverage"
        )
        .await
    );

    // Deactivate the registration. unregister_collector also runs
    // device cleanup, so restore the mid-cleanup state deterministically:
    // the device still names the dead collector while no registration
    // is active. Every OTHER fence still validates.
    assert!(
        lab.manager
            .unregister_collector("e2e-col", registration_gen)
    );
    lab.registry
        .assign_device_with_audit(
            &device_addr,
            Some("e2e-col".to_string()),
            "test",
            "test_reassign_dead_collector",
        )
        .unwrap();

    // Precondition proof: the authority window, epoch and issued
    // session-generation bound ALL still pass — only the inactive
    // registration can reject this event.
    assert!(
        lab.manager
            .validate_session_authority("e2e-col", &device_addr, epoch, assigned_gen),
        "authority window must still validate for the boundary test"
    );
    assert_eq!(lab.manager.active_session_generation("e2e-col"), None);
    assert_eq!(
        lab.manager.latest_session_generation("e2e-col"),
        session_gen,
        "issued high-water mark is retained after unregister"
    );

    // A lifecycle event stamped with the still-issued generation must
    // be rejected: the registration it claims is no longer active.
    let (ev_tx2, ev_rx2) = mpsc::channel::<bonsai::api::pb::SubscriptionLifecycleEvent>(8);
    let mut client2 = connect_client(&pki, &collector_leaf, lab.addr).await;
    let call2 = tokio::spawn(async move {
        client2
            .subscription_lifecycle(Request::new(ReceiverStream::new(ev_rx2)))
            .await
    });
    for (state, seq) in [("submitted", 1), ("synchronized", 2)] {
        ev_tx2
            .send(ev("sess-post-unregister", 2, state, seq))
            .await
            .unwrap();
    }
    drop(ev_tx2);
    let resp2 = call2.await.expect("rpc2").expect("resp2").into_inner();
    assert!(
        resp2.rejected >= 2,
        "inactive-registration events must be rejected: {resp2:?}"
    );
    assert_eq!(
        lab.sessions.current_session(&device_addr),
        None,
        "deactivated registration must not establish coverage authority"
    );
}
