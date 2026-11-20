//! M6C-2 schema-bound acquisition — wire-level integration proof.
//!
//! One integrated fixture drives the REAL production boundaries end to
//! end: integrity-verified compiled catalogue → mock-gNMI applicability
//! refresh → deterministic schema-plan preview/activation → durable
//! approval → Core collector assignment → collector assignment apply
//! (`ingest::handle_assignment_update`) → device-facing gNMI
//! SubscribeRequest → authenticated lifecycle → telemetry ingest →
//! coverage verifier → revocation/expiry withdrawal.
//!
//! No parallel authority model is constructed: every hop uses the same
//! functions production wiring calls. Mock gNMI only — no vendor
//! interoperability is claimed.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::Duration;

use bonsai::api::pb::bonsai_graph_client::BonsaiGraphClient;
use bonsai::api::pb::{AssignmentUpdate, SubscriptionLifecycleEvent as PbLifecycleEvent};
use bonsai::api::{BonsaiGraphServer, CoreService, PROTOCOL_VERSION_CURRENT};
use bonsai::assignment::CollectorManager;
use bonsai::collector_auth::AuthPolicy;
use bonsai::config::{
    AcquisitionMode, GnmiSecurityMode, RuntimeConfig, RuntimeTlsConfig, SelectedSubscriptionPath,
    TargetConfig,
};
use bonsai::credentials::CredentialVault;
use bonsai::discovery::DiscoveryInput;
use bonsai::event_bus::InProcessBus;
use bonsai::graph::GraphStore;
use bonsai::ingest::{
    handle_assignment_update, run_lifecycle_forwarder, telemetry_to_ingest_update,
};
use bonsai::proto::gnmi::g_nmi_server::{GNmi, GNmiServer};
use bonsai::proto::gnmi::{
    CapabilityRequest, CapabilityResponse, Encoding, GetRequest, GetResponse, ModelData,
    Notification, Path as GnmiPath, PathElem, SubscribeRequest, SubscribeResponse, Update,
    subscribe_request, subscribe_response, typed_value,
};
use bonsai::registry::ApiRegistry;
use bonsai::schema_binding::{self, SchemaBindingClock};
use bonsai::sidecar_registry::SidecarRegistry;
use bonsai::sqlite_store::SqliteStore;
use bonsai::store::BonsaiStore;
use bonsai::subscriber::SubscriberHandleMap;
use bonsai::subscription_sessions::{SessionLifecycleEvent, SubscriptionSessionRegistry};
use bonsai::subscription_status::{SubscriptionPlan, run_subscription_verifier};
use bonsai::telemetry_identity::{
    BindingProvenance, BindingStatus, CanonicalDevice, TrustedBinding,
};
use bonsai::yang::YangLibrary;
use bonsai::yang_applicability;
use bonsai::yang_schema::{self, CompileOptions, ModuleInput};
use rcgen::{CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::{Mutex, mpsc, watch};
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming, transport::Channel};

// ── Synthetic PKI (same convention as gnmi_subscription_e2e) ────────────────

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

// ── Mock gNMI: capabilities + Get (yang-library probe) + Subscribe ─────────

#[derive(Clone)]
struct MockGnmi {
    caps: CapabilityResponse,
    get_responses: Arc<Mutex<Vec<GetResponse>>>,
    repeat_get: Arc<Mutex<Option<GetResponse>>>,
    streams: Arc<Mutex<Vec<mpsc::Sender<Result<SubscribeResponse, Status>>>>>,
    requests: Arc<std::sync::Mutex<Vec<SubscribeRequest>>>,
    subscribe_calls: Arc<AtomicUsize>,
    active_streams: Arc<AtomicUsize>,
    set_calls: Arc<AtomicUsize>,
}

struct GuardedStream {
    inner: ReceiverStream<Result<SubscribeResponse, Status>>,
    _guard: StreamGuard,
}

struct StreamGuard(Arc<AtomicUsize>);

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl tokio_stream::Stream for GuardedStream {
    type Item = Result<SubscribeResponse, Status>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.inner).poll_next(cx)
    }
}

#[tonic::async_trait]
impl GNmi for MockGnmi {
    async fn capabilities(
        &self,
        _request: Request<CapabilityRequest>,
    ) -> Result<Response<CapabilityResponse>, Status> {
        Ok(Response::new(self.caps.clone()))
    }

    async fn get(&self, _request: Request<GetRequest>) -> Result<Response<GetResponse>, Status> {
        let mut responses = self.get_responses.lock().await;
        if responses.is_empty() {
            // Applicability refreshes may probe more than once (initial
            // evidence + post-expiry reapproval); repeat the final
            // scripted response rather than draining to an empty Get.
            return Ok(Response::new(
                self.repeat_get.lock().await.clone().unwrap_or_default(),
            ));
        }
        let response = responses.remove(0);
        if responses.is_empty() {
            self.repeat_get
                .lock()
                .await
                .replace(response.clone());
        }
        Ok(Response::new(response))
    }

    type SubscribeStream = GuardedStream;

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
        self.active_streams.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel::<Result<SubscribeResponse, Status>>(64);
        self.streams.lock().await.push(tx);
        Ok(Response::new(GuardedStream {
            inner: ReceiverStream::new(rx),
            _guard: StreamGuard(Arc::clone(&self.active_streams)),
        }))
    }

    async fn set(
        &self,
        _request: Request<bonsai::proto::gnmi::SetRequest>,
    ) -> Result<Response<bonsai::proto::gnmi::SetResponse>, Status> {
        self.set_calls.fetch_add(1, Ordering::SeqCst);
        Err(Status::unimplemented("set"))
    }
}

struct MockLab {
    /// Bare `host:port` — the device address used by registry,
    /// applicability discovery and the subscriber alike.
    addr: String,
    streams: Arc<Mutex<Vec<mpsc::Sender<Result<SubscribeResponse, Status>>>>>,
    requests: Arc<std::sync::Mutex<Vec<SubscribeRequest>>>,
    subscribe_calls: Arc<AtomicUsize>,
    active_streams: Arc<AtomicUsize>,
    set_calls: Arc<AtomicUsize>,
    _shutdown: watch::Sender<bool>,
    _handle: tokio::task::JoinHandle<()>,
}

async fn start_mock(get_responses: Vec<GetResponse>) -> MockLab {
    let svc = MockGnmi {
        caps: CapabilityResponse {
            supported_models: vec![
                ModelData {
                    name: "test-app".to_string(),
                    organization: "example".to_string(),
                    version: "1".to_string(),
                },
                ModelData {
                    name: "test-baselib".to_string(),
                    organization: "example".to_string(),
                    version: "1".to_string(),
                },
            ],
            supported_encodings: vec![Encoding::JsonIetf as i32, Encoding::Proto as i32],
            g_nmi_version: "0.3.0".to_string(),
            ..Default::default()
        },
        get_responses: Arc::new(Mutex::new(get_responses)),
        repeat_get: Arc::new(Mutex::new(None)),
        streams: Arc::new(Mutex::new(Vec::new())),
        requests: Arc::new(std::sync::Mutex::new(Vec::new())),
        subscribe_calls: Arc::new(AtomicUsize::new(0)),
        active_streams: Arc::new(AtomicUsize::new(0)),
        set_calls: Arc::new(AtomicUsize::new(0)),
    };
    let streams = Arc::clone(&svc.streams);
    let requests = Arc::clone(&svc.requests);
    let subscribe_calls = Arc::clone(&svc.subscribe_calls);
    let active_streams = Arc::clone(&svc.active_streams);
    let set_calls = Arc::clone(&svc.set_calls);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(GNmiServer::new(svc))
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async move {
                let _ = shutdown_rx.changed().await;
            })
            .await
            .unwrap();
    });
    MockLab {
        addr,
        streams,
        requests,
        subscribe_calls,
        active_streams,
        set_calls,
        _shutdown: shutdown_tx,
        _handle: handle,
    }
}

// ── YANG fixtures (same modules as yang_device_applicability) ──────────────

const MODULE_APP: &str = r#"
module test-app {
  yang-version 1.1;
  namespace "urn:example:test-app";
  prefix ta;
  revision 2024-01-01;

  feature base-f {
    description "base feature";
  }
  feature fancy-f {
    if-feature "ta:base-f";
  }

  container system {
    leaf hostname { type string; config false; }
    leaf domain { type string; }
  }

  container interfaces {
    list interface {
      key "name";
      leaf name { type string; }
      leaf oper-status { type string; config false; }
    }
  }
}
"#;

const MODULE_BASELIB: &str = r#"
module test-baselib {
  yang-version 1.1;
  namespace "urn:example:test-baselib";
  prefix tbl;
  revision 2023-06-01;

  container platform {
    leaf os-name { type string; config false; }
    leaf version { type string; config false; }
  }
}
"#;

fn module_input(name: &str, yang: &str) -> ModuleInput {
    let module = yang_schema::parse_module(yang).expect("fixture parses");
    ModuleInput {
        module,
        vendor_scope: "test".to_string(),
        source_kind: "inline".to_string(),
        source_ref: name.to_string(),
        trust: "trusted".to_string(),
        checksum_sha256: format!("inline-{}", name),
        approved: false,
        license: String::new(),
        platforms: vec!["*".to_string()],
        pinned_ref: String::new(),
        source_id: String::new(),
    }
}

fn compile_app() -> bonsai::yang_schema::CompiledCatalogue {
    yang_schema::compile_parsed(
        vec![
            module_input("test-app", MODULE_APP),
            module_input("test-baselib", MODULE_BASELIB),
        ],
        &CompileOptions::default(),
    )
}

/// RFC 8525 NMDA `yang-library` container content (same evidence the
/// M6C-1 probe tests use).
fn nmda_json() -> serde_json::Value {
    serde_json::json!({
        "content-id": "cid-m6c2",
        "module-set": [{
            "name": "ms1",
            "module": [
                {"name": "test-app", "revision": "2024-01-01",
                 "namespace": "urn:example:test-app",
                 "feature": [{"name": "base-f"}, {"name": "fancy-f"}]},
                {"name": "test-baselib", "revision": "2023-06-01",
                 "namespace": "urn:example:test-baselib"}
            ]
        }],
        "schema": [{"name": "s1", "module-set": ["ms1"]}],
        "datastore": [{"name": "ietf-datastores:running", "schema": "s1"}]
    })
}

fn json_get_response(elem_name: &str, content: serde_json::Value) -> GetResponse {
    GetResponse {
        notification: vec![Notification {
            timestamp: 0,
            prefix: None,
            update: vec![Update {
                path: Some(GnmiPath {
                    origin: String::new(),
                    elem: vec![PathElem {
                        name: elem_name.to_string(),
                        key: Default::default(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                val: Some(bonsai::proto::gnmi::TypedValue {
                    value: Some(typed_value::Value::JsonIetfVal(
                        serde_json::to_vec(&content).unwrap(),
                    )),
                }),
                ..Default::default()
            }],
            delete: vec![],
            atomic: false,
        }],
        ..Default::default()
    }
}

// ── Core lab (same wiring as gnmi_subscription_e2e, plus injected clock) ───

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

struct CoreLab {
    addr: std::net::SocketAddr,
    graph: Arc<GraphStore>,
    registry: Arc<ApiRegistry>,
    sqlite: Arc<SqliteStore>,
    manager: Arc<CollectorManager>,
    sessions: Arc<SubscriptionSessionRegistry>,
    credentials: Arc<CredentialVault>,
    /// Verifier plan channel — also receives assignment plan status and
    /// authenticated submitted manifests, exactly as production wires it.
    plan_tx: mpsc::Sender<SubscriptionPlan>,
    _dir: TempDir,
    _server: tokio::task::JoinHandle<()>,
    _graph_writer: tokio::task::JoinHandle<()>,
    _verifier: tokio::task::JoinHandle<()>,
    _verifier_shutdown: watch::Sender<bool>,
}

async fn start_core_lab(
    pki: &Pki,
    device: TargetConfig,
    clock: SchemaBindingClock,
) -> CoreLab {
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
            .with_session_registry(Arc::clone(&sessions))
            .with_schema_binding_clock(clock.clone()),
    );
    let sidecars = Arc::new(SidecarRegistry::new(vec![]));

    let (plan_tx, plan_rx) = mpsc::channel::<SubscriptionPlan>(128);
    let svc = CoreService::new(
        Arc::clone(&graph),
        Arc::clone(&registry),
        Arc::clone(&credentials),
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
    .with_subscription_plan_tx(Some(plan_tx.clone()))
    .with_schema_binding_clock(clock);

    let plan = bonsai::subscriber::planned_subscription_plan_for_target(&device);
    plan_tx.send(plan).await.unwrap();
    let verifier_store = SharedStore(Arc::clone(&graph));
    let verifier_bus = Arc::clone(&bus);
    let verifier_sessions = Arc::clone(&sessions);
    let (vsd, vsrx) = watch::channel(false);
    let (verifier_ready_tx, verifier_ready_rx) = tokio::sync::oneshot::channel();
    let verifier = tokio::spawn(run_subscription_verifier(
        verifier_store,
        verifier_bus,
        plan_rx,
        verifier_sessions,
        vsrx,
        Some(verifier_ready_tx),
    ));
    verifier_ready_rx
        .await
        .expect("subscription verifier exited before signaling readiness")
        .expect("subscription verifier boot initialization failed");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
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

    CoreLab {
        addr,
        graph,
        registry,
        sqlite,
        manager,
        sessions,
        credentials,
        plan_tx,
        _dir: dir,
        _server: server,
        _graph_writer: graph_writer,
        _verifier: verifier,
        _verifier_shutdown: vsd,
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

// ── fixtures/helpers ────────────────────────────────────────────────────────

/// Pre-activation device: assigned to the collector but carrying no
/// acquisition authority until the schema-bound approval lands.
fn device_target(address: &str) -> TargetConfig {
    TargetConfig {
        address: address.to_string(),
        enabled: true,
        security: Some(GnmiSecurityMode::Plaintext),
        collector_id: Some("e2e-col".to_string()),
        acquisition: AcquisitionMode::Disabled,
        inventory_only: false,
        vendor: Some("test".to_string()),
        hostname: Some("m6c2-dut".to_string()),
        site: Some("dc-e2e".to_string()),
        ..Default::default()
    }
}

/// The explicit operator selection submitted to the real evaluator:
/// two operational (config-false) schema paths — one SAMPLE leaf, one
/// ON_CHANGE keyed list leaf.
fn selected_paths() -> Vec<SelectedSubscriptionPath> {
    vec![
        SelectedSubscriptionPath {
            path: "/system/hostname".to_string(),
            origin: "ta".to_string(),
            provenance: String::new(),
            mode: "SAMPLE".to_string(),
            sample_interval_ns: 10_000_000_000,
            rationale: "approved hostname sample".to_string(),
            optional: false,
        },
        SelectedSubscriptionPath {
            path: "/interfaces/interface[name=eth0]/oper-status".to_string(),
            origin: "ta".to_string(),
            provenance: String::new(),
            mode: "ON_CHANGE".to_string(),
            sample_interval_ns: 0,
            rationale: "approved oper-status".to_string(),
            optional: false,
        },
    ]
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

fn discovery_input(addr: &str) -> DiscoveryInput {
    DiscoveryInput {
        address: addr.to_string(),
        username: None,
        password: None,
        username_env: None,
        password_env: None,
        ca_cert_path: None,
        tls_domain: None,
        security: Some(GnmiSecurityMode::Plaintext),
        client_cert_path: None,
        client_key_path: None,
        role_hint: None,
        vault: None,
        environment_archetype: None,
    }
}

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

fn update_notification(origin: &str, elems: Vec<(&str, Option<(&str, &str)>)>, val: &str) -> SubscribeResponse {
    let path = GnmiPath {
        origin: origin.to_string(),
        elem: elems
            .into_iter()
            .map(|(name, key)| PathElem {
                name: name.to_string(),
                key: key
                    .map(|(k, v)| [(k.to_string(), v.to_string())].into_iter().collect())
                    .unwrap_or_default(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    let mut tv = bonsai::proto::gnmi::TypedValue::default();
    tv.value = Some(typed_value::Value::StringVal(val.to_string()));
    SubscribeResponse {
        response: Some(subscribe_response::Response::Update(
            bonsai::proto::gnmi::Notification {
                timestamp: 1_000,
                prefix: None,
                update: vec![bonsai::proto::gnmi::Update {
                    path: Some(path),
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

fn hostname_notification() -> SubscribeResponse {
    update_notification("ta", vec![("system", None), ("hostname", None)], "m6c2-dut")
}

fn oper_status_notification() -> SubscribeResponse {
    update_notification(
        "ta",
        vec![
            ("interfaces", None),
            ("interface", Some(("name", "eth0"))),
            ("oper-status", None),
        ],
        "up",
    )
}

/// Convert an internal lifecycle event to the wire proto exactly as the
/// production forwarder does (mirrors `ingest::lifecycle_event_to_proto`).
fn lifecycle_event_to_proto(
    collector_id: &str,
    ev: &SessionLifecycleEvent,
) -> PbLifecycleEvent {
    let binding = ev.schema_binding.as_ref();
    PbLifecycleEvent {
        collector_id: collector_id.to_string(),
        device_address: ev.target.clone(),
        plan_id: ev.plan_id.clone(),
        session_id: ev.session_id.clone(),
        state: ev.state.clone(),
        transition_seq: ev.transition_seq,
        authority_epoch: ev.authority_epoch,
        assignment_generation: ev.assignment_generation,
        collector_session_generation: ev.collector_session_generation,
        execution_generation: ev.execution_generation,
        occurred_at_ns: ev.occurred_at_ns,
        detail: ev.detail.clone(),
        protocol_version: bonsai::api::PROTOCOL_VERSION,
        submitted_paths: ev
            .submitted_paths
            .iter()
            .map(|e| bonsai::api::pb::SubscriptionPath {
                path: e.path.clone(),
                origin: e.origin.clone(),
                mode: e.mode.clone(),
                sample_interval_ns: e.sample_interval_ns,
                ..Default::default()
            })
            .collect(),
        acquisition: ev.acquisition.clone(),
        schema_binding_mode: binding.map(|b| b.mode.clone()).unwrap_or_default(),
        schema_id: binding.map(|b| b.schema_id.clone()).unwrap_or_default(),
        evidence_fingerprint: binding
            .map(|b| b.evidence_fingerprint.clone())
            .unwrap_or_default(),
        binding_fingerprint: binding
            .map(|b| b.binding_fingerprint.clone())
            .unwrap_or_default(),
        approval_revision: binding.map(|b| b.approval_revision).unwrap_or(0),
        snapshot_id: binding.map(|b| b.snapshot_id).unwrap_or(0),
        datastore: binding.and_then(|b| b.datastore.clone()).unwrap_or_default(),
        approved_by: binding.map(|b| b.approved_by.clone()).unwrap_or_default(),
        schema_binding_plan_id: binding.map(|b| b.plan_id.clone()).unwrap_or_default(),
    }
}

/// Send lifecycle events over a direct authenticated RPC stream and
/// return Core's terminal accept/reject counts — the same boundary the
/// collector forwarder uses, minus the subscriber.
async fn send_lifecycle_events(
    client: &mut BonsaiGraphClient<Channel>,
    collector_id: &str,
    events: Vec<SessionLifecycleEvent>,
) -> (u64, u64) {
    let protos: Vec<PbLifecycleEvent> = events
        .iter()
        .map(|e| lifecycle_event_to_proto(collector_id, e))
        .collect();
    let resp = client
        .subscription_lifecycle(Request::new(tokio_stream::iter(protos)))
        .await
        .expect("lifecycle rpc")
        .into_inner();
    (resp.accepted, resp.rejected)
}

// ── integrated fixture ──────────────────────────────────────────────────────

struct BoundFixture {
    mock: MockLab,
    lab: CoreLab,
    library: YangLibrary,
    device_addr: String,
    activated_device: TargetConfig,
    collector_client: BonsaiGraphClient<Channel>,
    assign_rx: mpsc::Receiver<AssignmentUpdate>,
    authority_epoch: u64,
    assigned_gen: u64,
    collector_session_gen: u64,
    subscribers: SubscriberHandleMap,
    last_epoch: u64,
    last_gen: u64,
    recorded: Arc<std::sync::Mutex<Vec<SessionLifecycleEvent>>>,
    sub_tx: mpsc::Sender<SessionLifecycleEvent>,
    collector_bus: Arc<InProcessBus>,
    clock_cell: Arc<AtomicI64>,
    _pki: Pki,
    _collector_leaf: LeafCert,
    _fsd: watch::Sender<bool>,
    _isd: watch::Sender<bool>,
    _lifecycle_fwd: tokio::task::JoinHandle<Result<(), anyhow::Error>>,
    _ingest_fwd: tokio::task::JoinHandle<()>,
}

/// Drive steps 1–15 of the required positive path: catalogue →
/// applicability → preview → activate → assignment → Subscribe →
/// lifecycle → telemetry → coverage. Returns with coverage established.
async fn setup_bound_fixture() -> BoundFixture {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .try_init();
    let pki = make_pki();
    let collector_leaf = issue_leaf(&pki, "e2e-col", false);

    // 1–2. Mock gNMI serving capabilities, the yang-library Get probe and
    // the Subscribe stream — one device endpoint for all evidence.
    let get_responses = vec![json_get_response("yang:yang-library", nmda_json())];
    let mock = start_mock(get_responses).await;
    let device_addr = mock.addr.clone();

    let clock_cell = Arc::new(AtomicI64::new(bonsai::graph::common::now_ns()));
    let clock = SchemaBindingClock::shared(Arc::clone(&clock_cell));
    let device = device_target(&device_addr);
    let lab = start_core_lab(&pki, device.clone(), clock).await;
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

    // 1 (cont). Integrity-verified, complete compiled catalogue.
    let yang_dir = lab._dir.path().join("yanglib");
    let library =
        YangLibrary::open(yang_dir.join("lib"), yang_dir.join("cache"), "K").unwrap();
    let catalogue = compile_app();
    yang_schema::persist_compiled(&library, &catalogue).unwrap();
    yang_schema::activate_compiled(&library, &catalogue.schema_id).unwrap();

    // 3. Establish device applicability through the mock gNMI device and
    // persist the snapshot (real discovery + RFC 8525 probe + resolve).
    let target = lab.registry.get_device(&device_addr).expect("get").expect("device");
    let snapshot = yang_applicability::refresh_device_applicability(
        &yang_applicability::RefreshInput {
            target: &target,
            library: &library,
            discovery_input: &discovery_input(&device_addr),
            identity_ambiguous: false,
            requested_datastore: None,
            store: &lab.sqlite,
        },
    )
    .await
    .expect("applicability refresh");
    assert_eq!(snapshot.overall_status, "applicable");

    // 4–6. Explicit selection → real evaluator preview → real activation
    // persistence path.
    let req = schema_binding::SchemaPlanRequest {
        selected_paths: Some(selected_paths()),
        datastore: None,
        plan_id: None,
        binding_fingerprint: None,
    };
    let now = bonsai::graph::common::now_ns();
    let preview =
        schema_binding::evaluate_preview(&library, &lab.sqlite, &target, &req, now).unwrap();
    assert!(
        preview.activation_permitted,
        "preview must permit activation: {:?}",
        preview.evaluation.as_ref().map(|e| &e.rejection_reasons)
    );
    let eval = preview.evaluation.clone().unwrap();
    let act_req = schema_binding::SchemaPlanRequest {
        selected_paths: req.selected_paths.clone(),
        datastore: None,
        plan_id: Some(eval.plan_id.clone()),
        binding_fingerprint: Some(eval.binding_fingerprint.clone()),
    };
    let activation =
        schema_binding::activate(&library, &lab.sqlite, &target, &act_req, "admin", now)
            .expect("schema-bound activation");
    let approval_revision = activation.record.approval_revision;
    let activated_device = lab
        .registry
        .activate_schema_binding(
            activation.device.clone(),
            activation.record,
            approval_revision as i64 - 1,
            "admin",
            "test_activate",
        )
        .expect("durable activation");

    // 7–9. Collector registration → assignment sync carries the exact
    // approved binding → applied through the real assignment path.
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

    // Wire-level assignment assertions: the exact approved binding.
    let md = initial_sync
        .assignments
        .iter()
        .find(|a| {
            a.device
                .as_ref()
                .is_some_and(|d| d.address == device_addr)
        })
        .expect("device assignment present");
    assert!(!md.rejected, "assignment must not be rejected");
    let wire_device = md.device.as_ref().unwrap();
    assert_eq!(wire_device.schema_binding_mode, "schema_bound");
    assert_eq!(wire_device.schema_id, catalogue.schema_id);
    assert_eq!(wire_device.binding_fingerprint, eval.binding_fingerprint);
    assert_eq!(
        wire_device.approval_revision,
        approval_revision as u64
    );
    assert_eq!(wire_device.snapshot_id, snapshot.snapshot_id as i64);
    assert_eq!(
        wire_device.plan_id, eval.plan_id,
        "dedicated schema-plan identity on the wire"
    );

    // Lifecycle channel → tee (record) → real forwarder → authenticated RPC.
    let recorded = Arc::new(std::sync::Mutex::new(Vec::<SessionLifecycleEvent>::new()));
    let (sub_tx, mut sub_rx) = mpsc::channel::<SessionLifecycleEvent>(256);
    let (lc_tx, lc_rx) = mpsc::channel::<SessionLifecycleEvent>(256);
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
    let tls_files = write_tls_files(&pki, &collector_leaf, lab._dir.path());
    let runtime_cfg = Arc::new(RuntimeConfig {
        collector_id: "e2e-col".to_string(),
        core_ingest_endpoint: format!("https://{}", lab.addr),
        tls: tls_files,
        ..Default::default()
    });
    let (fsd, fsrx) = watch::channel(false);
    let lifecycle_fwd = tokio::spawn(run_lifecycle_forwarder(
        Arc::clone(&runtime_cfg),
        "e2e-col".to_string(),
        lc_rx,
        fsrx,
    ));

    // Collector bus → TelemetryIngest RPC (same wire path as production).
    let collector_bus = InProcessBus::new(256);
    let mut ingest_rx = collector_bus.subscribe();
    let ingest_client = connect_client(&pki, &collector_leaf, lab.addr).await;
    let (isd, mut is_rx) = watch::channel(false);
    let ingest_fwd = tokio::spawn(async move {
        let client = ingest_client;
        loop {
            tokio::select! {
                _ = is_rx.changed() => break,
                update = ingest_rx.recv() => {
                    let Ok(update) = update else { break };
                    let proto = match telemetry_to_ingest_update("e2e-col", &update) {
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

    // 9 (cont). Apply the assignment through the REAL collector path.
    let mut subscribers = SubscriberHandleMap::new();
    let mut last_epoch = 0u64;
    let mut last_gen = 0u64;
    let plans = Some(lab.plan_tx.clone());
    let lifecycle = Some(sub_tx.clone());
    handle_assignment_update(
        initial_sync,
        &collector_bus,
        &plans,
        &mut subscribers,
        &mut last_epoch,
        &mut last_gen,
        &lifecycle,
    )
    .await;

    // 10–11. Capture the actual device-facing SubscribeRequest.
    assert!(
        wait_for(
            || mock.subscribe_calls.load(Ordering::SeqCst) >= 1,
            Duration::from_secs(10),
            "subscribe rpc"
        )
        .await
    );
    assert!(
        wait_for(
            || !mock.requests.lock().unwrap().is_empty(),
            Duration::from_secs(10),
            "subscribe request recorded"
        )
        .await
    );
    {
        let reqs = mock.requests.lock().unwrap();
        assert_eq!(reqs.len(), 1, "expected exactly one SubscribeRequest");
        let Some(subscribe_request::Request::Subscribe(list)) = &reqs[0].request else {
            panic!("expected Subscribe request, got {:?}", reqs[0].request);
        };
        assert_eq!(list.subscription.len(), 2, "two approved paths only");
        // Order is canonical, not selection order — match by mode.
        let sample = list
            .subscription
            .iter()
            .find(|s| s.mode == bonsai::proto::gnmi::SubscriptionMode::Sample as i32)
            .expect("SAMPLE subscription");
        let on_change = list
            .subscription
            .iter()
            .find(|s| s.mode == bonsai::proto::gnmi::SubscriptionMode::OnChange as i32)
            .expect("ON_CHANGE subscription");
        // Path 1: /system/hostname — SAMPLE, origin ta, 10s interval.
        let p0 = sample.path.as_ref().expect("path");
        assert_eq!(p0.origin, "ta");
        assert_eq!(p0.elem.len(), 2);
        assert_eq!(p0.elem[0].name, "system");
        assert_eq!(p0.elem[1].name, "hostname");
        assert_eq!(sample.sample_interval, 10_000_000_000);
        // Path 2: /interfaces/interface[name=eth0]/oper-status — ON_CHANGE.
        let p1 = on_change.path.as_ref().expect("path");
        assert_eq!(p1.origin, "ta");
        assert_eq!(p1.elem.len(), 3);
        assert_eq!(p1.elem[0].name, "interfaces");
        assert_eq!(p1.elem[1].name, "interface");
        assert_eq!(p1.elem[1].key.get("name").map(String::as_str), Some("eth0"));
        assert_eq!(p1.elem[2].name, "oper-status");
        assert_eq!(on_change.sample_interval, 0);
    }

    // 12–15. Complete initial sync with real telemetry, then verify Core
    // accepts the authenticated binding+manifest and coverage lands.
    {
        let guard = mock.streams.lock().await;
        let tx = guard.last().expect("subscribe stream").clone();
        tx.send(Ok(hostname_notification())).await.unwrap();
        tx.send(Ok(oper_status_notification())).await.unwrap();
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

    let path_a = "system/hostname";
    let path_b = "interfaces/interface[name=eth0]/oper-status";
    let mut a_observed = false;
    let mut b_observed = false;
    for _ in 0..100 {
        let rows = query_subscription_statuses(&lab.graph).await;
        a_observed = rows
            .iter()
            .any(|r| r.0 == device_addr && r.1 == path_a && r.4 == "observed");
        b_observed = rows
            .iter()
            .any(|r| r.0 == device_addr && r.1 == path_b && r.4 == "observed");
        if a_observed && b_observed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(a_observed, "hostname path never observed");
    assert!(b_observed, "oper-status path never observed");

    // The submitted lifecycle manifest must echo the approved binding.
    let submitted = {
        let evs = recorded.lock().unwrap();
        evs.iter()
            .find(|e| e.state == "submitted")
            .cloned()
            .expect("submitted lifecycle event recorded")
    };
    let binding_echo = submitted.schema_binding.clone().expect("binding echo");
    assert_eq!(binding_echo.binding_fingerprint, eval.binding_fingerprint);
    assert_eq!(binding_echo.plan_id, eval.plan_id);
    assert_eq!(binding_echo.mode, "schema_bound");

    BoundFixture {
        mock,
        lab,
        library,
        device_addr,
        activated_device,
        collector_client,
        assign_rx,
        authority_epoch: epoch,
        assigned_gen,
        collector_session_gen,
        subscribers,
        last_epoch,
        last_gen,
        recorded,
        sub_tx,
        collector_bus,
        clock_cell,
        _pki: pki,
        _collector_leaf: collector_leaf,
        _fsd: fsd,
        _isd: isd,
        _lifecycle_fwd: lifecycle_fwd,
        _ingest_fwd: ingest_fwd,
    }
}

/// Apply the next assignment update the manager delivers (resync,
/// revocation, reconnect) through the real collector path.
async fn apply_next_update(fx: &mut BoundFixture) {
    let update = fx
        .assign_rx
        .recv()
        .await
        .expect("assignment update");
    let plans = Some(fx.lab.plan_tx.clone());
    let lifecycle = Some(fx.sub_tx.clone());
    handle_assignment_update(
        update,
        &fx.collector_bus,
        &plans,
        &mut fx.subscribers,
        &mut fx.last_epoch,
        &mut fx.last_gen,
        &lifecycle,
    )
    .await;
}

// ── tests ───────────────────────────────────────────────────────────────────

/// Full positive path plus the negative path: revoke → withdrawal →
/// stale replay/forged manifest rejection → no coverage resurrection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn schema_bound_activation_to_revocation_e2e() {
    let mut fx = setup_bound_fixture().await;

    // ── Negative path A: forged manifests while the binding is ACTIVE ──
    // Mutating one approved manifest element must be rejected by Core's
    // authorized-manifest equality check — with a self-consistent
    // plan_id so plan-integrity is not what rejects it.
    let recorded_evs: Vec<SessionLifecycleEvent> = fx.recorded.lock().unwrap().clone();
    let synchronized = recorded_evs
        .iter()
        .find(|e| e.state == "synchronized")
        .cloned()
        .expect("synchronized event recorded");
    let approved_binding = synchronized.schema_binding.clone().expect("binding echo");

    let forged_base = |session: &str| SessionLifecycleEvent {
        collector_id: String::new(),
        target: fx.device_addr.clone(),
        plan_id: String::new(),
        session_id: session.to_string(),
        state: "submitted".to_string(),
        transition_seq: 1,
        authority_epoch: fx.authority_epoch,
        assignment_generation: fx.last_gen,
        authority_instance: 0,
        collector_session_generation: fx.collector_session_gen,
        execution_generation: synchronized.execution_generation + 1,
        occurred_at_ns: bonsai::graph::common::now_ns(),
        detail: String::new(),
        submitted_paths: synchronized.submitted_paths.clone(),
        acquisition: "explicit".to_string(),
        schema_binding: Some(approved_binding.clone()),
    };
    let plan_id_for = |paths: &[bonsai::subscription_sessions::LifecyclePathEntry]| {
        let entries: Vec<(String, String, String, u64)> = paths
            .iter()
            .map(|e| (e.origin.clone(), e.path.clone(), e.mode.clone(), e.sample_interval_ns))
            .collect();
        bonsai::subscriber::acquisition_plan_id(AcquisitionMode::Explicit, &entries)
    };

    // Variant 1: an additional path broadens the approved manifest.
    let mut forged_extra = forged_base("forged-session-extra");
    forged_extra.submitted_paths.push(
        bonsai::subscription_sessions::LifecyclePathEntry {
            path: "system/domain".to_string(),
            origin: "ta".to_string(),
            mode: "ON_CHANGE".to_string(),
            sample_interval_ns: 0,
        },
    );
    forged_extra.plan_id = plan_id_for(&forged_extra.submitted_paths);

    // Variant 2: a changed mode on an approved path — flip the SAMPLE
    // entry to ON_CHANGE (submitted order is canonical, not selection
    // order, so locate it by mode rather than index).
    let mut forged_mode = forged_base("forged-session-mode");
    let sample_entry = forged_mode
        .submitted_paths
        .iter_mut()
        .find(|e| e.mode == "SAMPLE")
        .expect("approved SAMPLE entry");
    sample_entry.mode = "ON_CHANGE".to_string();
    sample_entry.sample_interval_ns = 0;
    forged_mode.plan_id = plan_id_for(&forged_mode.submitted_paths);

    let (accepted, rejected) = send_lifecycle_events(
        &mut fx.collector_client,
        "e2e-col",
        vec![forged_extra, forged_mode],
    )
    .await;
    assert_eq!(accepted, 0, "forged manifests must not be accepted");
    assert_eq!(rejected, 2, "both forged manifests must be rejected");

    // ── Negative path B: revoke through the actual revocation path ──
    let revoked = fx
        .lab
        .registry
        .revoke_schema_binding(&fx.device_addr, "admin", "operator_revoked", "test_revoke")
        .expect("revoke")
        .expect("revocation returns updated device");
    assert!(revoked.schema_binding.is_none());
    assert_eq!(revoked.acquisition, AcquisitionMode::Disabled);
    fx.lab.manager.resync_collector("e2e-col").await;
    apply_next_update(&mut fx).await;

    // Obsolete subscriber stopped; no replacement Subscribe RPC.
    assert!(
        wait_for(
            || fx.subscribers.is_empty(),
            Duration::from_secs(10),
            "subscriber stopped after revocation"
        )
        .await
    );
    assert!(
        wait_for(
            || fx.mock.active_streams.load(Ordering::SeqCst) == 0,
            Duration::from_secs(10),
            "device stream closed"
        )
        .await
    );
    assert!(
        wait_for(
            || fx
                .lab
                .sessions
                .current_session(&fx.device_addr)
                .is_none(),
            Duration::from_secs(10),
            "session authority retired"
        )
        .await
    );
    assert_eq!(
        fx.mock.subscribe_calls.load(Ordering::SeqCst),
        1,
        "revocation must not create a broader replacement subscription"
    );
    assert_eq!(fx.mock.requests.lock().unwrap().len(), 1);

    // Coverage can no longer be satisfied: statuses superseded.
    let path_a = "system/hostname";
    let path_b = "interfaces/interface[name=eth0]/oper-status";
    let mut superseded = false;
    for _ in 0..100 {
        let rows = query_subscription_statuses(&fx.lab.graph).await;
        let a = rows
            .iter()
            .find(|r| r.0 == fx.device_addr && r.1 == path_a);
        let b = rows
            .iter()
            .find(|r| r.0 == fx.device_addr && r.1 == path_b);
        if a.is_some_and(|r| r.4 != "observed") && b.is_some_and(|r| r.4 != "observed") {
            superseded = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(superseded, "withdrawn coverage must be superseded");

    // Stale telemetry carrying the PREVIOUS session's real plan/session
    // identity: received bytes, but no current authorized coverage.
    let stale_update = bonsai::telemetry::TelemetryUpdate {
        target: fx.device_addr.clone(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: 2,
        path: path_a.to_string(),
        value: serde_json::json!("stale-replay"),
        plan_id: synchronized.plan_id.clone(),
        session_id: synchronized.session_id.clone(),
        source_protocol: "gnmi".to_string(),
        gnmi_path: vec![bonsai::gnmi_path::parse_path("ta", "/system/hostname").unwrap()],
        is_delete: false,
        collector_id: String::new(),
    };
    let mut c = fx.collector_client.clone();
    let proto = telemetry_to_ingest_update("e2e-col", &stale_update).unwrap();
    let _ = c
        .telemetry_ingest(Request::new(tokio_stream::iter(vec![proto])))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let rows = query_subscription_statuses(&fx.lab.graph).await;
    assert!(
        rows.iter()
            .all(|r| !(r.0 == fx.device_addr && r.1 == path_a && r.4 == "observed")),
        "stale telemetry must not restore coverage: {rows:?}"
    );

    // Replay the old authenticated lifecycle event — must be rejected.
    let mut replay = synchronized.clone();
    replay.transition_seq += 100;
    let (accepted, rejected) = send_lifecycle_events(
        &mut fx.collector_client,
        "e2e-col",
        vec![replay],
    )
    .await;
    assert_eq!(accepted, 0, "revoked execution replay must not be accepted");
    assert!(rejected >= 1, "replay must be rejected");

    // Zero device Set requests across the entire run.
    assert_eq!(fx.mock.set_calls.load(Ordering::SeqCst), 0);

    // Binding evidence carries no secrets.
    let binding_json =
        serde_json::to_string(&fx.activated_device.schema_binding).unwrap();
    for forbidden in ["password", "secret", "token", "private_key", "credential"] {
        assert!(
            !binding_json.contains(forbidden),
            "binding must not contain '{forbidden}'"
        );
    }
}

/// A pre-v5 collector must never receive a schema-bound assignment —
/// the assignment is withheld and the device fails closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn schema_bound_assignment_blocked_for_old_collector() {
    let fx = setup_bound_fixture().await;
    let (mut old_rx, _old_gen) = fx
        .lab
        .manager
        .register_collector("e2e-col-old".to_string(), 4, true)
        .await
        .unwrap();
    let old_sync = old_rx.recv().await.expect("old collector sync");
    assert!(
        old_sync
            .assignments
            .iter()
            .all(|a| a
                .device
                .as_ref()
                .map(|d| d.address.as_str())
                != Some(fx.device_addr.as_str())),
        "schema-bound assignment must never reach a pre-v5 collector"
    );
}

/// Time-driven expiry during an ACTIVE subscription: authorization
/// rejects expired authority immediately; the bounded reconciler
/// revokes durably and withdraws through the established assignment
/// mechanism; reapproval with fresh evidence restores authority.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn schema_bound_expiry_withdraws_active_execution() {
    let mut fx = setup_bound_fixture().await;
    let device_addr = fx.device_addr.clone();

    // Advance past the 24h evidence window deterministically.
    // Evidence was observed seconds after the cell's initial value, so
    // anchor expiry to real now with a wide margin — never a bare +1ns.
    fx.clock_cell.store(
        bonsai::graph::common::now_ns()
            + bonsai::yang_applicability::EVIDENCE_MAX_AGE_NS
            + 60_000_000_000,
        Ordering::SeqCst,
    );

    // Layer 1 — authorization: a well-formed lifecycle event carrying
    // the CURRENT durable binding echo is still rejected because the
    // approved evidence is expired. No reconciler has run yet.
    let recorded_evs: Vec<SessionLifecycleEvent> = fx.recorded.lock().unwrap().clone();
    let synchronized = recorded_evs
        .iter()
        .find(|e| e.state == "synchronized")
        .cloned()
        .expect("synchronized event");
    let mut probe = synchronized.clone();
    probe.transition_seq += 300;
    // Stamp CURRENT post-sync authority so only the expiry check can
    // reject it.
    probe.authority_epoch = fx.authority_epoch;
    probe.assignment_generation = fx.last_gen;
    probe.collector_session_generation = fx.collector_session_gen;
    let (accepted, rejected) = send_lifecycle_events(
        &mut fx.collector_client,
        "e2e-col",
        vec![probe],
    )
    .await;
    assert_eq!(accepted, 0, "expired binding must not authorize lifecycle");
    assert!(rejected >= 1);

    // Layer 1 — assignment: a resync now delivers an explicit rejection
    // rather than the bound plan.
    fx.lab.manager.resync_collector("e2e-col").await;
    let update = fx.assign_rx.recv().await.expect("resync update");
    let md = update
        .assignments
        .iter()
        .find(|a| {
            a.device
                .as_ref()
                .is_some_and(|d| d.address == device_addr)
        })
        .expect("device still assigned");
    assert!(md.rejected, "expired evidence must reject the assignment");
    assert!(
        md.reject_reason.contains("schema_binding_evidence_expired"),
        "unexpected reject reason: {}",
        md.reject_reason
    );
    let plans = Some(fx.lab.plan_tx.clone());
    let lifecycle = Some(fx.sub_tx.clone());
    handle_assignment_update(
        update,
        &fx.collector_bus,
        &plans,
        &mut fx.subscribers,
        &mut fx.last_epoch,
        &mut fx.last_gen,
        &lifecycle,
    )
    .await;
    assert!(
        wait_for(
            || fx.subscribers.is_empty(),
            Duration::from_secs(10),
            "expired execution stopped"
        )
        .await
    );

    // Layer 2 — bounded reconciliation: durable revocation + resync.
    // (The device was already stopped above; reconcile must still mark
    // policy revoked and clear runtime authority.)
    let report = schema_binding::reconcile_schema_binding_expiry(
        &fx.lab.registry,
        Some(&fx.lab.manager),
        fx.clock_cell.load(Ordering::SeqCst),
    )
    .await
    .expect("reconcile");
    assert!(report.expired.contains(&device_addr));
    assert!(report.revoked.contains(&device_addr));
    let device = fx.lab.registry.get_device(&device_addr).expect("get").expect("device");
    assert!(device.schema_binding.is_none(), "runtime binding cleared");
    assert_eq!(device.acquisition, AcquisitionMode::Disabled);
    let record = fx
        .lab
        .sqlite
        .get_schema_binding(&bonsai::registry::device_identity_key(&device))
        .unwrap()
        .expect("policy record");
    assert_eq!(record.state, "revoked");
    assert_eq!(record.revoke_reason.as_deref(), Some("evidence_expired"));

    // Drain the resync the reconciler triggered and apply it.
    apply_next_update(&mut fx).await;

    // Reapproval after refresh: new evidence → new approval → authority
    // restored through the same path.
    let target = fx.lab.registry.get_device(&device_addr).unwrap().unwrap();
    let snapshot2 = yang_applicability::refresh_device_applicability(
        &yang_applicability::RefreshInput {
            target: &target,
            library: &fx.library,
            discovery_input: &discovery_input(&device_addr),
            identity_ambiguous: false,
            requested_datastore: None,
            store: &fx.lab.sqlite,
        },
    )
    .await
    .expect("refresh");
    // The refresh evidence is timestamped at real now — inside the
    // injected clock's window only if the clock is near real time.
    // Rewind the injected clock to real now for the reapproval.
    fx.clock_cell
        .store(bonsai::graph::common::now_ns(), Ordering::SeqCst);
    let _ = snapshot2;

    let req = schema_binding::SchemaPlanRequest {
        selected_paths: Some(selected_paths()),
        datastore: None,
        plan_id: None,
        binding_fingerprint: None,
    };
    let now = bonsai::graph::common::now_ns();
    let preview = schema_binding::evaluate_preview(
        &fx.library,
        &fx.lab.sqlite,
        &target,
        &req,
        now,
    )
    .unwrap();
    assert!(preview.activation_permitted);
    let eval2 = preview.evaluation.unwrap();
    let act_req = schema_binding::SchemaPlanRequest {
        plan_id: Some(eval2.plan_id.clone()),
        binding_fingerprint: Some(eval2.binding_fingerprint.clone()),
        ..req
    };
    let activation2 = schema_binding::activate(
        &fx.library,
        &fx.lab.sqlite,
        &target,
        &act_req,
        "admin",
        now,
    )
    .expect("reapproval");
    let rev2 = activation2.record.approval_revision;
    fx.lab
        .registry
        .activate_schema_binding(
            activation2.device,
            activation2.record,
            rev2 as i64 - 1,
            "admin",
            "test_reapprove",
        )
        .unwrap();
    fx.lab.manager.resync_collector("e2e-col").await;
    apply_next_update(&mut fx).await;

    // New Subscribe RPC with the reapproved manifest.
    assert!(
        wait_for(
            || fx.mock.subscribe_calls.load(Ordering::SeqCst) >= 2,
            Duration::from_secs(10),
            "reapproved subscribe"
        )
        .await
    );

    // The OLD binding fingerprint can never echo authority again.
    let old_binding = fx.activated_device.schema_binding.clone().unwrap();
    let mut stale_echo = synchronized.clone();
    stale_echo.transition_seq += 400;
    stale_echo.schema_binding = Some(old_binding);
    stale_echo.authority_epoch = fx.authority_epoch;
    stale_echo.assignment_generation = fx.last_gen;
    stale_echo.collector_session_generation = fx.collector_session_gen;
    let (accepted, rejected) = send_lifecycle_events(
        &mut fx.collector_client,
        "e2e-col",
        vec![stale_echo],
    )
    .await;
    assert_eq!(accepted, 0, "superseded approval echo must be rejected");
    assert!(rejected >= 1);
}

/// Expiry while the collector is DISCONNECTED: reconciliation still
/// revokes durable authority; the next assignment sync fails closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn schema_bound_expiry_disconnected_collector() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let pki = make_pki();
    let collector_leaf = issue_leaf(&pki, "e2e-col", false);
    let get_responses = vec![json_get_response("yang:yang-library", nmda_json())];
    let mock = start_mock(get_responses).await;
    let device_addr = mock.addr.clone();
    let clock_cell = Arc::new(AtomicI64::new(bonsai::graph::common::now_ns()));
    let clock = SchemaBindingClock::shared(Arc::clone(&clock_cell));
    let device = device_target(&device_addr);
    let lab = start_core_lab(&pki, device.clone(), clock).await;
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

    let yang_dir = lab._dir.path().join("yanglib");
    let library = YangLibrary::open(yang_dir.join("lib"), yang_dir.join("cache"), "K").unwrap();
    let catalogue = compile_app();
    yang_schema::persist_compiled(&library, &catalogue).unwrap();
    yang_schema::activate_compiled(&library, &catalogue.schema_id).unwrap();
    let target = lab.registry.get_device(&device_addr).unwrap().unwrap();
    yang_applicability::refresh_device_applicability(
        &yang_applicability::RefreshInput {
            target: &target,
            library: &library,
            discovery_input: &discovery_input(&device_addr),
            identity_ambiguous: false,
            requested_datastore: None,
            store: &lab.sqlite,
        },
    )
    .await
    .unwrap();
    let req = schema_binding::SchemaPlanRequest {
        selected_paths: Some(selected_paths()),
        datastore: None,
        plan_id: None,
        binding_fingerprint: None,
    };
    let now = bonsai::graph::common::now_ns();
    let preview = schema_binding::evaluate_preview(&library, &lab.sqlite, &target, &req, now).unwrap();
    let eval = preview.evaluation.unwrap();
    let act_req = schema_binding::SchemaPlanRequest {
        plan_id: Some(eval.plan_id.clone()),
        binding_fingerprint: Some(eval.binding_fingerprint.clone()),
        ..req
    };
    let activation =
        schema_binding::activate(&library, &lab.sqlite, &target, &act_req, "admin", now).unwrap();
    let rev = activation.record.approval_revision;
    lab.registry
        .activate_schema_binding(
            activation.device,
            activation.record,
            rev as i64 - 1,
            "admin",
            "test_activate",
        )
        .unwrap();

    // Expire while NO collector is registered.
    clock_cell.store(
        bonsai::graph::common::now_ns()
            + bonsai::yang_applicability::EVIDENCE_MAX_AGE_NS
            + 60_000_000_000,
        Ordering::SeqCst,
    );
    let report = schema_binding::reconcile_schema_binding_expiry(
        &lab.registry,
        Some(&lab.manager),
        clock_cell.load(Ordering::SeqCst),
    )
    .await
    .unwrap();
    assert!(report.revoked.contains(&device_addr));

    // Collector connects AFTER expiry: the sync must fail closed — the
    // device is disabled, no Subscribe RPC is ever emitted.
    let (_rx, _g) = lab
        .manager
        .register_collector("e2e-col".to_string(), PROTOCOL_VERSION_CURRENT, true)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(mock.subscribe_calls.load(Ordering::SeqCst), 0);
    let device = lab.registry.get_device(&device_addr).unwrap().unwrap();
    assert!(device.schema_binding.is_none());
    assert_eq!(device.acquisition, AcquisitionMode::Disabled);
}

/// Expired evidence blocks NEW activation (pre-activation expiry) and a
/// core restart after expiry fails closed on the next assignment sync.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn schema_bound_expired_evidence_blocks_activation_and_restart() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let pki = make_pki();
    let collector_leaf = issue_leaf(&pki, "e2e-col", false);
    let get_responses = vec![json_get_response("yang:yang-library", nmda_json())];
    let mock = start_mock(get_responses).await;
    let device_addr = mock.addr.clone();
    let clock_cell = Arc::new(AtomicI64::new(bonsai::graph::common::now_ns()));
    let clock = SchemaBindingClock::shared(Arc::clone(&clock_cell));
    let device = device_target(&device_addr);
    let lab = start_core_lab(&pki, device.clone(), clock).await;
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

    let yang_dir = lab._dir.path().join("yanglib");
    let library = YangLibrary::open(yang_dir.join("lib"), yang_dir.join("cache"), "K").unwrap();
    let catalogue = compile_app();
    yang_schema::persist_compiled(&library, &catalogue).unwrap();
    yang_schema::activate_compiled(&library, &catalogue.schema_id).unwrap();
    let target = lab.registry.get_device(&device_addr).unwrap().unwrap();
    let snapshot = yang_applicability::refresh_device_applicability(
        &yang_applicability::RefreshInput {
            target: &target,
            library: &library,
            discovery_input: &discovery_input(&device_addr),
            identity_ambiguous: false,
            requested_datastore: None,
            store: &lab.sqlite,
        },
    )
    .await
    .unwrap();

    // Persist an ADDITIONAL snapshot row whose evidence is already older
    // than the validity window — it becomes the latest view, so preview
    // and activation must fail closed before any approval exists.
    let mut stale_snap = snapshot.clone();
    stale_snap.observed_at_ns = Some(
        bonsai::graph::common::now_ns()
            - bonsai::yang_applicability::EVIDENCE_MAX_AGE_NS
            - 1,
    );
    stale_snap.evaluated_at_ns = bonsai::graph::common::now_ns();
    stale_snap.created_at_ns = bonsai::graph::common::now_ns();
    let row = yang_applicability::snapshot_to_row(&stale_snap, 0).unwrap();
    lab.sqlite.save_yang_applicability_snapshot(&row).unwrap();

    let req = schema_binding::SchemaPlanRequest {
        selected_paths: Some(selected_paths()),
        datastore: None,
        plan_id: None,
        binding_fingerprint: None,
    };
    let now = bonsai::graph::common::now_ns();
    let preview = schema_binding::evaluate_preview(&library, &lab.sqlite, &target, &req, now).unwrap();
    assert!(
        !preview.activation_permitted,
        "expired evidence must block activation"
    );
    assert!(
        preview
            .evaluation
            .as_ref()
            .map(|e| !e.activation_permitted)
            .unwrap_or(true)
    );

    // Now activate a FRESH approval first (restore freshness by using the
    // earlier fresh snapshot — remove the stale latest by writing an even
    // newer fresh row is complex; instead activate against the fresh
    // snapshot id directly is not supported, so: refresh again).
    let snapshot_fresh = yang_applicability::refresh_device_applicability(
        &yang_applicability::RefreshInput {
            target: &target,
            library: &library,
            discovery_input: &discovery_input(&device_addr),
            identity_ambiguous: false,
            requested_datastore: None,
            store: &lab.sqlite,
        },
    )
    .await
    .unwrap();
    let preview2 = schema_binding::evaluate_preview(&library, &lab.sqlite, &target, &req, now).unwrap();
    assert!(preview2.activation_permitted, "fresh evidence permits activation");
    let eval = preview2.evaluation.unwrap();
    let act_req = schema_binding::SchemaPlanRequest {
        plan_id: Some(eval.plan_id.clone()),
        binding_fingerprint: Some(eval.binding_fingerprint.clone()),
        ..req
    };
    let activation =
        schema_binding::activate(&library, &lab.sqlite, &target, &act_req, "admin", now).unwrap();
    let rev = activation.record.approval_revision;
    lab.registry
        .activate_schema_binding(
            activation.device,
            activation.record,
            rev as i64 - 1,
            "admin",
            "test_activate",
        )
        .unwrap();
    let _ = snapshot_fresh;

    // Restart-after-expiry: a NEW CollectorManager (new authority epoch)
    // on the same registry must still fail closed once evidence expires.
    clock_cell.store(
        bonsai::graph::common::now_ns()
            + bonsai::yang_applicability::EVIDENCE_MAX_AGE_NS
            + 60_000_000_000,
        Ordering::SeqCst,
    );
    let restarted_manager = Arc::new(
        CollectorManager::new(
            Arc::clone(&lab.registry),
            Arc::clone(&lab.credentials),
            vec![],
        )
        .with_session_registry(Arc::clone(&lab.sessions))
        .with_schema_binding_clock(SchemaBindingClock::shared(Arc::clone(&clock_cell))),
    );
    let (mut rx2, _g2) = restarted_manager
        .register_collector("e2e-col".to_string(), PROTOCOL_VERSION_CURRENT, true)
        .await
        .unwrap();
    let sync2 = rx2.recv().await.expect("restart sync");
    let md = sync2
        .assignments
        .iter()
        .find(|a| {
            a.device
                .as_ref()
                .is_some_and(|d| d.address == device_addr)
        })
        .expect("device present");
    assert!(md.rejected, "post-restart sync must reject expired binding");
    assert!(md.reject_reason.contains("schema_binding_evidence_expired"));

    // And the durable reconciler revokes it on the restarted core too.
    let report = schema_binding::reconcile_schema_binding_expiry(
        &lab.registry,
        Some(&restarted_manager),
        clock_cell.load(Ordering::SeqCst),
    )
    .await
    .unwrap();
    assert!(report.revoked.contains(&device_addr));
}

/// Failed reconciliation delivery followed by recovery: the collector's
/// assignment stream is gone when expiry hits; durable revocation still
/// lands and the reconnecting collector receives fail-closed state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn schema_bound_expiry_failed_delivery_then_recovery() {
    let mut fx = setup_bound_fixture().await;
    let device_addr = fx.device_addr.clone();

    // Simulate delivery failure: drop the assignment stream receiver so
    // resync_collector cannot reach the collector.
    drop(fx.assign_rx);
    let (_dead_tx, dead_rx) = mpsc::channel::<AssignmentUpdate>(1);
    fx.assign_rx = dead_rx;

    fx.clock_cell.store(
        bonsai::graph::common::now_ns()
            + bonsai::yang_applicability::EVIDENCE_MAX_AGE_NS
            + 60_000_000_000,
        Ordering::SeqCst,
    );
    let report = schema_binding::reconcile_schema_binding_expiry(
        &fx.lab.registry,
        Some(&fx.lab.manager),
        fx.clock_cell.load(Ordering::SeqCst),
    )
    .await
    .unwrap();
    assert!(report.revoked.contains(&device_addr));

    // Durable authority is gone even though delivery failed.
    let device = fx.lab.registry.get_device(&device_addr).unwrap().unwrap();
    assert!(device.schema_binding.is_none());
    assert_eq!(device.acquisition, AcquisitionMode::Disabled);

    // Recovery: the collector reconnects (new session) and receives the
    // current fail-closed state — no bound assignment, no Subscribe.
    let calls_before = fx.mock.subscribe_calls.load(Ordering::SeqCst);
    let (mut rx3, _g3) = fx
        .lab
        .manager
        .register_collector("e2e-col".to_string(), PROTOCOL_VERSION_CURRENT, true)
        .await
        .unwrap();
    let sync3 = rx3.recv().await.expect("reconnect sync");
    let md = sync3
        .assignments
        .iter()
        .find(|a| {
            a.device
                .as_ref()
                .is_some_and(|d| d.address == device_addr)
        });
    // Either absent or present-but-disabled; never schema-bound.
    if let Some(md) = md {
        assert!(
            md.device
                .as_ref()
                .is_none_or(|d| d.schema_binding_mode != "schema_bound"),
            "reconnect must not restore expired binding"
        );
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        fx.mock.subscribe_calls.load(Ordering::SeqCst),
        calls_before,
        "no new Subscribe after failed-delivery expiry"
    );
}
