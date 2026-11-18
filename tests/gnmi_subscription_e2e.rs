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
    AcquisitionMode, RuntimeConfig, RuntimeTlsConfig, SelectedSubscriptionPath, TargetConfig,
};
use bonsai::credentials::CredentialVault;
use bonsai::event_bus::InProcessBus;
use bonsai::graph::GraphStore;
use bonsai::ingest::{run_lifecycle_forwarder, telemetry_to_ingest_update};
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
    let manager = Arc::new(CollectorManager::new(
        Arc::clone(&registry),
        Arc::clone(&credentials),
        vec![],
    ));
    let sessions = Arc::new(SubscriptionSessionRegistry::new());
    let sidecars = Arc::new(SidecarRegistry::new(vec![]));

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
    .with_session_registry(Arc::clone(&sessions));

    // Seed the verifier plan exactly as server_startup does for core mode.
    let (plan_tx, plan_rx) = mpsc::channel::<bonsai::subscription_status::SubscriptionPlan>(128);
    let plan = bonsai::subscriber::planned_subscription_plan_for_target(&device);
    plan_tx.send(plan).await.unwrap();
    let verifier_store = SharedStore(Arc::clone(&graph));
    let verifier_bus = Arc::clone(&bus);
    let verifier_sessions = Arc::clone(&sessions);
    let (vsd, vsrx) = watch::channel(false);
    let verifier = tokio::spawn(run_subscription_verifier(
        verifier_store,
        verifier_bus,
        plan_rx,
        verifier_sessions,
        vsrx,
    ));

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
    let assigned_gen = assign_rx.recv().await.expect("initial sync").generation;
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
