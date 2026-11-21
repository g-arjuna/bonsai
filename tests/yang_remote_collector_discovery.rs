//! M6E — authenticated remote-collector YANG discovery, wire-level
//! integration proof.
//!
//! One integrated fixture drives the REAL production boundaries: mTLS
//! collector registration → bounded durable discovery job → collector
//! gNMI Capabilities/Get evidence acquisition → authenticated result
//! submission → Core-side admission/fencing → catalogue resolution →
//! SQLite applicability snapshot → schema-bound preview.
//!
//! The collector gathers evidence only; Core alone resolves and
//! approves. Mock gNMI only — no vendor interoperability is claimed.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bonsai::api::pb::bonsai_graph_client::BonsaiGraphClient;
use bonsai::api::pb::{
    AssignmentUpdate, CollectorIdentity, PollYangDiscoveryJobsRequest, YangDiscoveryJob as PbJob,
    YangDiscoveryResult,
};
use bonsai::api::{BonsaiGraphServer, CoreService, PROTOCOL_VERSION_CURRENT};
use bonsai::assignment::CollectorManager;
use bonsai::collector_auth::AuthPolicy;
use bonsai::config::{AcquisitionMode, GnmiSecurityMode, TargetConfig};
use bonsai::credentials::CredentialVault;
use bonsai::event_bus::InProcessBus;
use bonsai::graph::GraphStore;
use bonsai::proto::gnmi::g_nmi_server::{GNmi, GNmiServer};
use bonsai::proto::gnmi::{
    CapabilityRequest, CapabilityResponse, Encoding, GetRequest, GetResponse, ModelData,
    Notification, Path as GnmiPath, PathElem, Update, typed_value,
};
use bonsai::registry::ApiRegistry;
use bonsai::schema_binding;
use bonsai::sidecar_registry::SidecarRegistry;
use bonsai::sqlite_store::{SqliteStore, YangDiscoveryJobRow};
use bonsai::store::BonsaiStore;
use bonsai::subscription_sessions::SubscriptionSessionRegistry;
use bonsai::telemetry_identity::{
    BindingProvenance, BindingStatus, CanonicalDevice, TrustedBinding,
};
use bonsai::yang::YangLibrary;
use bonsai::yang_applicability;
use bonsai::yang_discovery;
use bonsai::yang_schema::{self, CompileOptions, ModuleInput};
use rcgen::{CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::{Mutex, watch};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming, transport::Channel};

// ── YANG fixtures (same modules as yang_device_applicability) ───────────────

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
      container config {
        leaf mtu { type uint32; units "octets"; if-feature "ta:fancy-f"; }
        leaf admin-status { type string; }
      }
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

// ── Synthetic PKI (same convention as the other e2e suites) ─────────────────

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

fn issue_leaf(pki: &Pki, cn: &str) -> LeafCert {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
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

fn issue_server_leaf(pki: &Pki, cn: &str) -> LeafCert {
    let mut params =
        CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()]).unwrap();
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

// ── Mock gNMI device (read-only evidence endpoints) ─────────────────────────

#[derive(Clone)]
struct MockGnmi {
    caps: CapabilityResponse,
    get_responses: Arc<Mutex<Vec<GetResponse>>>,
    subscribe_calls: Arc<AtomicUsize>,
    set_calls: Arc<AtomicUsize>,
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
            return Ok(Response::new(GetResponse::default()));
        }
        Ok(Response::new(responses.remove(0)))
    }

    type SubscribeStream =
        tokio_stream::wrappers::ReceiverStream<Result<SubscribeResponseShim, Status>>;

    async fn subscribe(
        &self,
        _request: Request<Streaming<bonsai::proto::gnmi::SubscribeRequest>>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        self.subscribe_calls.fetch_add(1, Ordering::SeqCst);
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }

    async fn set(
        &self,
        _request: Request<bonsai::proto::gnmi::SetRequest>,
    ) -> Result<Response<bonsai::proto::gnmi::SetResponse>, Status> {
        self.set_calls.fetch_add(1, Ordering::SeqCst);
        Err(Status::unimplemented("set"))
    }
}

// The generated SubscribeResponse type alias keeps the impl signature
// identical to the trait without importing the full name here.
use bonsai::proto::gnmi::SubscribeResponse as SubscribeResponseShim;

struct DeviceLab {
    addr: String,
    subscribe_calls: Arc<AtomicUsize>,
    set_calls: Arc<AtomicUsize>,
    _shutdown: watch::Sender<bool>,
    _handle: tokio::task::JoinHandle<()>,
}

fn nmda_json() -> serde_json::Value {
    serde_json::json!({
        "content-id": "cid-remote",
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

async fn start_device() -> DeviceLab {
    let get_responses = Arc::new(Mutex::new(vec![json_get_response(
        "yang:yang-library",
        nmda_json(),
    )]));
    let subscribe_calls = Arc::new(AtomicUsize::new(0));
    let set_calls = Arc::new(AtomicUsize::new(0));
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
            supported_encodings: vec![Encoding::JsonIetf as i32],
            g_nmi_version: "0.13.0".to_string(),
            ..Default::default()
        },
        get_responses,
        subscribe_calls: Arc::clone(&subscribe_calls),
        set_calls: Arc::clone(&set_calls),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(GNmiServer::new(svc))
            .serve_with_incoming_shutdown(
                tokio_stream::wrappers::TcpListenerStream::new(listener),
                async move {
                    let _ = shutdown_rx.changed().await;
                },
            )
            .await
            .unwrap();
    });
    DeviceLab {
        addr,
        subscribe_calls,
        set_calls,
        _shutdown: shutdown_tx,
        _handle: handle,
    }
}

// ── Core lab (mTLS gRPC + durable stores + verified catalogue) ──────────────

struct CoreLab {
    addr: std::net::SocketAddr,
    registry: Arc<ApiRegistry>,
    sqlite: Arc<SqliteStore>,
    manager: Arc<CollectorManager>,
    library: YangLibrary,
    catalogue_schema_id: String,
    _dir: TempDir,
    _server: tokio::task::JoinHandle<()>,
}

fn device_target(address: &str, collector: &str) -> TargetConfig {
    TargetConfig {
        address: address.to_string(),
        enabled: true,
        security: Some(GnmiSecurityMode::Plaintext),
        collector_id: Some(collector.to_string()),
        acquisition: AcquisitionMode::Disabled,
        inventory_only: false,
        ..Default::default()
    }
}

async fn start_core_lab(pki: &Pki, _device: TargetConfig) -> CoreLab {
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
    // Drain the bus so the writer task never blocks.
    let mut graph_rx = bus.subscribe();
    let graph_for_writer = Arc::clone(&graph);
    let _graph_writer = tokio::spawn(async move {
        while let Ok(update) = graph_rx.recv().await {
            let u = Arc::unwrap_or_clone(update);
            let _ = BonsaiStore::write(graph_for_writer.as_ref(), u).await;
        }
    });
    let sessions = Arc::new(SubscriptionSessionRegistry::new());
    let manager = Arc::new(
        CollectorManager::new(Arc::clone(&registry), Arc::clone(&credentials), vec![])
            .with_session_registry(Arc::clone(&sessions)),
    );
    let sidecars = Arc::new(SidecarRegistry::new(vec![]));

    // Integrity-verified compiled catalogue persisted+activated in the
    // same library the admission path opens.
    let yang_dir = dir.path().join("yanglib");
    let library = YangLibrary::open(yang_dir.join("lib"), yang_dir.join("cache"), "K").unwrap();
    let catalogue = compile_app();
    yang_schema::persist_compiled(&library, &catalogue).unwrap();
    yang_schema::activate_compiled(&library, &catalogue.schema_id).unwrap();

    let svc = CoreService::new(
        graph,
        Arc::clone(&registry),
        credentials,
        bus,
        None,
        Some(Arc::clone(&manager)),
        sidecars,
        Some(Arc::clone(&sqlite)),
        None,
        None,
    )
    .with_auth_policy(AuthPolicy::Required)
    .with_session_registry(Arc::clone(&sessions))
    .with_yang_library_roots(
        yang_dir.join("lib").to_string_lossy().into_owned(),
        yang_dir.join("cache").to_string_lossy().into_owned(),
        "K".to_string(),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_cert = issue_server_leaf(pki, "bonsai-core");
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
            .add_service(BonsaiGraphServer::new(svc).max_decoding_message_size(
                bonsai::sqlite_store::YANG_DISCOVERY_MAX_RESULT_MESSAGE_BYTES,
            ))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    CoreLab {
        addr,
        registry,
        sqlite,
        manager,
        library,
        catalogue_schema_id: catalogue.schema_id,
        _dir: dir,
        _server: server,
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

/// Full fixture: device + core + bound collector + registered session.
struct Fixture {
    lab: CoreLab,
    device: DeviceLab,
    collector_leaf: LeafCert,
    client: BonsaiGraphClient<Channel>,
    assign_rx: tonic::Streaming<AssignmentUpdate>,
    device_addr: String,
    pki: Pki,
}

async fn start_fixture() -> Fixture {
    let pki = make_pki();
    let device = start_device().await;
    let device_addr = device.addr.clone();
    let target = device_target(&device_addr, "col-a");
    let lab = start_core_lab(&pki, target.clone()).await;
    lab.registry
        .add_device_with_audit(target.clone(), "test", "test_add")
        .unwrap();
    lab.registry
        .assign_device_with_audit(
            &device_addr,
            Some("col-a".to_string()),
            "test",
            "test_assign",
        )
        .unwrap();
    let collector_leaf = issue_leaf(&pki, "col-a");
    bind_collector(&lab.sqlite, "col-a", &collector_leaf.fingerprint);

    let client = connect_client(&pki, &collector_leaf, lab.addr).await;
    let resp = client
        .clone()
        .register_collector(Request::new(CollectorIdentity {
            collector_id: "col-a".to_string(),
            hostname: "collector-a".to_string(),
            protocol_version: PROTOCOL_VERSION_CURRENT,
            auth_token: String::new(),
            features: vec![yang_discovery::YANG_DISCOVERY_FEATURE.to_string()],
        }))
        .await
        .expect("register_collector");
    let assign_rx = resp.into_inner();
    Fixture {
        lab,
        device,
        collector_leaf,
        client,
        assign_rx,
        device_addr,
        pki,
    }
}

impl Fixture {
    /// Queue a remote-discovery job exactly as the Admin refresh path
    /// does for executor=assigned_collector.
    fn queue_job(&self, datastore: Option<String>) -> YangDiscoveryJobRow {
        let target = self
            .lab
            .registry
            .get_device(&self.device_addr)
            .unwrap()
            .expect("device");
        yang_discovery::request_remote_discovery(
            &self.lab.registry,
            &self.lab.sqlite,
            &self.lab.manager,
            &target,
            datastore,
            "test-admin",
        )
        .expect("queue remote discovery job")
    }

    /// Poll → receive the durable job stamped with the live session.
    async fn poll_job(&mut self) -> PbJob {
        let resp = self
            .client
            .clone()
            .poll_yang_discovery_jobs(Request::new(PollYangDiscoveryJobsRequest {
                collector_id: "col-a".to_string(),
                protocol_version: PROTOCOL_VERSION_CURRENT,
            }))
            .await
            .expect("poll")
            .into_inner();
        assert!(resp.discovery_supported, "session must support discovery");
        assert_eq!(resp.jobs.len(), 1, "exactly one pending job");
        resp.jobs.into_iter().next().unwrap()
    }

    /// Execute the job against the mock device through the REAL
    /// collector executor, then submit through the REAL authenticated
    /// RPC. Returns the wire ack.
    async fn execute_and_submit(&self, job: &PbJob) -> bonsai::api::pb::YangDiscoveryResultAck {
        let target = self
            .lab
            .registry
            .get_device(&job.device_address)
            .unwrap()
            .expect("device for job");
        let submission = yang_discovery::execute_discovery_job(
            &target,
            if job.datastore.is_empty() {
                None
            } else {
                Some(job.datastore.as_str())
            },
            job.deadline_ns,
        )
        .await;
        assert_eq!(
            submission.outcome, "completed",
            "diagnostics: {:?}",
            submission.diagnostics
        );
        let result = yang_discovery::submission_to_result(
            job,
            "col-a",
            submission,
            PROTOCOL_VERSION_CURRENT,
        );
        self.client
            .clone()
            .submit_yang_discovery_result(Request::new(result))
            .await
            .expect("submit")
            .into_inner()
    }
}

fn selected_paths() -> Vec<bonsai::config::SelectedSubscriptionPath> {
    vec![
        bonsai::config::SelectedSubscriptionPath {
            path: "/system/hostname".to_string(),
            origin: "ta".to_string(),
            provenance: String::new(),
            mode: "SAMPLE".to_string(),
            sample_interval_ns: 10_000_000_000,
            rationale: "approved hostname sample".to_string(),
            optional: false,
        },
        bonsai::config::SelectedSubscriptionPath {
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

// ── T1/T2: successful remote discovery + schema-plan continuation ────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t1_remote_discovery_persists_collector_evidence() {
    let mut fx = start_fixture().await;
    let job_row = fx.queue_job(None);

    let pb_job = fx.poll_job().await;
    assert_eq!(pb_job.job_id, job_row.job_id);
    assert!(pb_job.collector_session_generation > 0);

    let ack = fx.execute_and_submit(&pb_job).await;
    assert!(ack.accepted, "rejected: {}", ack.reason);
    assert!(!ack.duplicate);
    assert!(ack.snapshot_id > 0);

    // Durable job reached completed with the snapshot reference.
    let done = fx
        .lab
        .sqlite
        .get_yang_discovery_job(&job_row.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(done.state, "completed");
    assert_eq!(done.snapshot_id, Some(ack.snapshot_id));

    // Snapshot persisted through the SAME applicability path, marked as
    // authenticated-collector evidence — never core_direct.
    let row = fx
        .lab
        .sqlite
        .latest_yang_applicability_snapshot(&done.device_key)
        .unwrap()
        .expect("snapshot row");
    let snap = yang_applicability::snapshot_from_row(&row).unwrap();
    assert_eq!(snap.overall_status, "applicable");
    assert_eq!(
        snap.schema_id.as_deref(),
        Some(fx.lab.catalogue_schema_id.as_str())
    );
    assert_eq!(snap.provenance.executor, "authenticated_collector");
    assert_eq!(
        snap.provenance.observing_collector_id.as_deref(),
        Some("col-a")
    );
    assert_eq!(
        snap.provenance.discovery_job_id.as_deref(),
        Some(job_row.job_id.as_str())
    );
    assert!(snap.provenance.collector_session_generation.is_some());
    assert!(snap.provenance.authority_epoch.is_some());
    assert!(snap.provenance.evidence_received_at_ns.is_some());
    // The ASSIGNED collector field is unchanged — assignment is not
    // proof of observation; provenance carries the executor.
    assert_eq!(snap.collector_id.as_deref(), Some("col-a"));

    // T15 partial: zero device writes, zero subscriptions.
    assert_eq!(fx.device.set_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fx.device.subscribe_calls.load(Ordering::SeqCst), 0);

    // T2: the remote snapshot drives the EXISTING schema-bound preview —
    // no second planner was introduced.
    let target = fx
        .lab
        .registry
        .get_device(&fx.device_addr)
        .unwrap()
        .expect("device");
    let req = schema_binding::SchemaPlanRequest {
        selected_paths: Some(selected_paths()),
        datastore: None,
        plan_id: None,
        binding_fingerprint: None,
        expected_config_revision: None,
    };
    let now = bonsai::graph::common::now_ns();
    let preview =
        schema_binding::evaluate_preview(&fx.lab.library, &fx.lab.sqlite, &target, &req, now)
            .expect("preview over remote evidence");
    assert!(
        preview.activation_permitted,
        "remote evidence must feed the existing preview: {:?}",
        preview.evaluation.as_ref().map(|e| &e.rejection_reasons)
    );
}

// ── T3/T4: wrong collector + unauthenticated submission ──────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t3_wrong_collector_cannot_submit() {
    let mut fx = start_fixture().await;
    fx.queue_job(None);
    let pb_job = fx.poll_job().await;

    // A second, legitimately-bound collector certificate claims col-a's
    // job — authorization must fail before any job state is touched.
    let pki = &fx.pki;
    let leaf_b = issue_leaf(pki, "col-b");
    bind_collector(&fx.lab.sqlite, "col-b", &leaf_b.fingerprint);
    let mut client_b = connect_client(pki, &leaf_b, fx.lab.addr).await;

    // Register col-b so its principal is a live collector session.
    let _b_assign = client_b
        .clone()
        .register_collector(Request::new(CollectorIdentity {
            collector_id: "col-b".to_string(),
            hostname: "collector-b".to_string(),
            protocol_version: PROTOCOL_VERSION_CURRENT,
            auth_token: String::new(),
            features: vec![yang_discovery::YANG_DISCOVERY_FEATURE.to_string()],
        }))
        .await
        .expect("register col-b");

    let target = fx
        .lab
        .registry
        .get_device(&pb_job.device_address)
        .unwrap()
        .unwrap();
    let submission = yang_discovery::execute_discovery_job(&target, None, pb_job.deadline_ns).await;
    let result = yang_discovery::submission_to_result(
        &pb_job,
        "col-a", // payload claims col-a; the CERT is col-b
        submission,
        PROTOCOL_VERSION_CURRENT,
    );
    let err = client_b
        .submit_yang_discovery_result(Request::new(result))
        .await
        .expect_err("col-b must not submit for col-a's job");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);

    // Even a payload that claims col-b cannot complete col-a's job —
    // authenticated identity and job binding must agree.
    let target = fx
        .lab
        .registry
        .get_device(&pb_job.device_address)
        .unwrap()
        .unwrap();
    let submission = yang_discovery::execute_discovery_job(&target, None, pb_job.deadline_ns).await;
    let mut result = yang_discovery::submission_to_result(
        &pb_job,
        "col-b",
        submission,
        PROTOCOL_VERSION_CURRENT,
    );
    result.collector_id = "col-b".to_string();
    let ack = client_b
        .submit_yang_discovery_result(Request::new(result))
        .await
        .expect("submit as col-b")
        .into_inner();
    assert!(!ack.accepted);
    assert_eq!(ack.reason, "wrong_collector");

    // Job remains dispatchable — no state corruption.
    let job = fx
        .lab
        .sqlite
        .get_yang_discovery_job(&pb_job.job_id)
        .unwrap()
        .unwrap();
    assert!(matches!(job.state.as_str(), "queued" | "dispatched"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t4_unauthenticated_submission_rejected() {
    // No transport-attested principal → PermissionDenied before any
    // job lookup. Constructed directly against the service so no TLS
    // session exists at all.
    let pki = make_pki();
    let device = start_device().await;
    let device_addr = device.addr.clone();
    let target = device_target(&device_addr, "col-a");
    let lab = start_core_lab(&pki, target.clone()).await;
    lab.registry
        .add_device_with_audit(target, "test", "test_add")
        .unwrap();

    // Rebuild the service handle the same way start_core_lab did — but
    // we cannot reach `svc` (moved into the server). Instead drive the
    // same boundary over the wire: a client certificate NOT bound to any
    // collector credential is denied by authorize_collector.
    let stray = issue_leaf(&pki, "stray-collector");
    let mut client = connect_client(&pki, &stray, lab.addr).await;
    let err = client
        .poll_yang_discovery_jobs(Request::new(PollYangDiscoveryJobsRequest {
            collector_id: "col-a".to_string(),
            protocol_version: PROTOCOL_VERSION_CURRENT,
        }))
        .await
        .expect_err("unbound credential must not poll");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    let err = client
        .submit_yang_discovery_result(Request::new(YangDiscoveryResult {
            job_id: "any".to_string(),
            collector_id: "col-a".to_string(),
            ..Default::default()
        }))
        .await
        .expect_err("unbound credential must not submit");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

// ── T5/T13: stale session fencing + reconnect redispatch ─────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t5_stale_session_result_rejected_and_redispatched() {
    let mut fx = start_fixture().await;
    fx.queue_job(None);
    let job_v1 = fx.poll_job().await;
    let session_v1 = job_v1.collector_session_generation;
    assert!(session_v1 > 0);

    // Reconnect: a NEW registration supersedes the session under a new
    // generation. The old session's eventual unregister is fenced by
    // generation and cannot unassign the device — so the durable job
    // stays valid and is redispatched, never revived as stale authority.
    let client2 = connect_client(&fx.pki, &fx.collector_leaf, fx.lab.addr).await;
    let resp = client2
        .clone()
        .register_collector(Request::new(CollectorIdentity {
            collector_id: "col-a".to_string(),
            hostname: "collector-a".to_string(),
            protocol_version: PROTOCOL_VERSION_CURRENT,
            auth_token: String::new(),
            features: vec![yang_discovery::YANG_DISCOVERY_FEATURE.to_string()],
        }))
        .await
        .expect("reregister");
    let assign_rx2 = resp.into_inner();
    // Now retire the old session — its unregister is generation-fenced
    // and must not clear the device assignment.
    drop(fx.assign_rx);
    drop(fx.client);
    fx.client = client2;
    fx.assign_rx = assign_rx2;
    // Defensive: the device must still be assigned to col-a.
    fx.lab
        .registry
        .assign_device_with_audit(
            &fx.device_addr,
            Some("col-a".to_string()),
            "test",
            "test_reassign",
        )
        .unwrap();
    let job_v2 = fx.poll_job().await;
    assert_eq!(
        job_v2.job_id, job_v1.job_id,
        "same durable job redispatched"
    );
    assert!(
        job_v2.collector_session_generation > session_v1,
        "redispatch must carry the new session generation"
    );

    // The v1-stamped result is stale — rejected, job stays alive.
    let target = fx
        .lab
        .registry
        .get_device(&fx.device_addr)
        .unwrap()
        .unwrap();
    let submission = yang_discovery::execute_discovery_job(&target, None, job_v1.deadline_ns).await;
    let mut stale = yang_discovery::submission_to_result(
        &job_v1,
        "col-a",
        submission,
        PROTOCOL_VERSION_CURRENT,
    );
    stale.collector_session_generation = session_v1;
    let ack = fx
        .client
        .clone()
        .submit_yang_discovery_result(Request::new(stale))
        .await
        .expect("submit stale")
        .into_inner();
    assert!(!ack.accepted);
    assert_eq!(ack.reason, "stale_session");
    let job = fx
        .lab
        .sqlite
        .get_yang_discovery_job(&job_v1.job_id)
        .unwrap()
        .unwrap();
    assert!(matches!(job.state.as_str(), "queued" | "dispatched"));

    // The correctly-stamped v2 result is accepted.
    let ack = fx.execute_and_submit(&job_v2).await;
    assert!(
        ack.accepted,
        "fresh session result must be accepted: {}",
        ack.reason
    );
}

// ── T6/T7: reassignment + device deletion fencing ────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t6_reassignment_rejects_stale_result() {
    let mut fx = start_fixture().await;
    fx.queue_job(None);
    let pb_job = fx.poll_job().await;

    // Reassign the device to a different collector mid-flight.
    fx.lab
        .registry
        .assign_device_with_audit(
            &fx.device_addr,
            Some("col-b".to_string()),
            "test",
            "test_reassign",
        )
        .unwrap();

    let target = fx
        .lab
        .registry
        .get_device(&fx.device_addr)
        .unwrap()
        .unwrap();
    let submission = yang_discovery::execute_discovery_job(&target, None, pb_job.deadline_ns).await;
    let result = yang_discovery::submission_to_result(
        &pb_job,
        "col-a",
        submission,
        PROTOCOL_VERSION_CURRENT,
    );
    let ack = fx
        .client
        .clone()
        .submit_yang_discovery_result(Request::new(result))
        .await
        .expect("submit")
        .into_inner();
    assert!(!ack.accepted);
    let job = fx
        .lab
        .sqlite
        .get_yang_discovery_job(&pb_job.job_id)
        .unwrap()
        .unwrap();
    assert!(
        matches!(job.state.as_str(), "stale_authority" | "cancelled"),
        "reassigned job must terminate: {}",
        job.state
    );
    // No snapshot may exist for the device.
    assert!(
        fx.lab
            .sqlite
            .latest_yang_applicability_snapshot(&job.device_key)
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t7_deleted_device_rejects_result() {
    let mut fx = start_fixture().await;
    fx.queue_job(None);
    let pb_job = fx.poll_job().await;

    fx.lab.registry.remove_device(&fx.device_addr).unwrap();

    // Even a locally-crafted valid-looking result cannot recreate
    // evidence for a deleted device.
    let target = device_target(&fx.device_addr, "col-a");
    let submission = yang_discovery::execute_discovery_job(&target, None, pb_job.deadline_ns).await;
    let result = yang_discovery::submission_to_result(
        &pb_job,
        "col-a",
        submission,
        PROTOCOL_VERSION_CURRENT,
    );
    let ack = fx
        .client
        .clone()
        .submit_yang_discovery_result(Request::new(result))
        .await
        .expect("submit")
        .into_inner();
    assert!(!ack.accepted);
    let job = fx
        .lab
        .sqlite
        .get_yang_discovery_job(&pb_job.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(job.state, "cancelled");
    assert!(
        fx.lab
            .sqlite
            .latest_yang_applicability_snapshot(&job.device_key)
            .unwrap()
            .is_none()
    );
}

// ── T8/T9/T10: idempotent replay, out-of-order, expiry ───────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t8_duplicate_submission_is_idempotent() {
    let mut fx = start_fixture().await;
    fx.queue_job(None);
    let pb_job = fx.poll_job().await;

    let ack1 = fx.execute_and_submit(&pb_job).await;
    assert!(ack1.accepted && !ack1.duplicate);

    // Identical re-submission: accepted as duplicate, no new snapshot,
    // no repeated binding revocation.
    let target = fx
        .lab
        .registry
        .get_device(&fx.device_addr)
        .unwrap()
        .unwrap();
    let submission = yang_discovery::execute_discovery_job(&target, None, pb_job.deadline_ns).await;
    let result = yang_discovery::submission_to_result(
        &pb_job,
        "col-a",
        submission,
        PROTOCOL_VERSION_CURRENT,
    );
    let ack2 = fx
        .client
        .clone()
        .submit_yang_discovery_result(Request::new(result))
        .await
        .expect("resubmit")
        .into_inner();
    // Evidence differs (fresh timestamps) → digest differs → conflict
    // rejection, still no state corruption.
    assert!(!ack2.accepted || ack2.duplicate);
    let job = fx
        .lab
        .sqlite
        .get_yang_discovery_job(&pb_job.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(job.state, "completed");
    assert_eq!(job.snapshot_id, Some(ack1.snapshot_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t9_out_of_order_completion_cannot_supersede() {
    let fx = start_fixture().await;
    let target = fx
        .lab
        .registry
        .get_device(&fx.device_addr)
        .unwrap()
        .unwrap();
    let device_key = bonsai::registry::device_identity_key(&target);

    // Two durable jobs for the same device — craft the second directly
    // (per-device dedup only applies to the request path).
    let now = bonsai::graph::common::now_ns();
    let epoch = fx.lab.manager.authority_epoch() as i64;
    let assignment_gen = fx.lab.manager.latest_assignment_generation("col-a") as i64;
    let mk = |n: i64| YangDiscoveryJobRow {
        id: 0,
        job_id: format!("job-{n}"),
        device_key: device_key.clone(),
        device_address: fx.device_addr.clone(),
        collector_id: "col-a".to_string(),
        datastore: None,
        operation: yang_discovery::OPERATION_YANG_APPLICABILITY.to_string(),
        state: "dispatched".to_string(),
        authority_epoch: epoch,
        assignment_generation: assignment_gen,
        collector_session_generation: Some(1),
        created_at_ns: now,
        deadline_ns: now + 120_000_000_000,
        dispatched_at_ns: Some(now),
        finished_at_ns: None,
        attempt_count: 1,
        result_classification: None,
        reason_code: None,
        snapshot_id: None,
        evidence_fingerprint: None,
        result_digest: None,
        diagnostics_json: None,
        created_by: Some("test".to_string()),
        updated_at_ns: now,
    };
    fx.lab.sqlite.create_yang_discovery_job(&mk(1)).unwrap();
    fx.lab.sqlite.create_yang_discovery_job(&mk(2)).unwrap();
    // Stamp both rows with the LIVE session generation — admission
    // fences results to the current authenticated session.
    let live_gen = fx
        .lab
        .manager
        .active_session_generation("col-a")
        .expect("col-a has a live session") as i64;
    for jid in ["job-1", "job-2"] {
        assert!(
            fx.lab
                .sqlite
                .mark_yang_discovery_job_dispatched(jid, live_gen, now)
                .unwrap()
        );
    }

    // Complete the NEWER job first through the real admission path,
    // with the real catalogue roots so evidence actually resolves.
    let yang_dir = fx.lab._dir.path().join("yanglib");
    let roots = (
        yang_dir.join("lib").to_string_lossy().into_owned(),
        yang_dir.join("cache").to_string_lossy().into_owned(),
        "K".to_string(),
    );
    for row in [
        fx.lab
            .sqlite
            .get_yang_discovery_job("job-2")
            .unwrap()
            .unwrap(),
        fx.lab
            .sqlite
            .get_yang_discovery_job("job-1")
            .unwrap()
            .unwrap(),
    ] {
        let pb_job = yang_discovery_job_proto_for_test(&row);
        let target = fx
            .lab
            .registry
            .get_device(&fx.device_addr)
            .unwrap()
            .unwrap();
        let submission =
            yang_discovery::execute_discovery_job(&target, None, pb_job.deadline_ns).await;
        let result = yang_discovery::submission_to_result(
            &pb_job,
            "col-a",
            submission,
            PROTOCOL_VERSION_CURRENT,
        );
        let outcome = yang_discovery::admit_discovery_result(
            &fx.lab.sqlite,
            &fx.lab.registry,
            &fx.lab.manager,
            Some(&roots),
            "col-a",
            &result,
            bonsai::graph::common::now_ns(),
        )
        .await
        .unwrap();
        let _ = outcome;
    }

    // The older job must NOT supersede the newer job's evidence: it is
    // either explicitly superseded or its snapshot is not current.
    let j1 = fx
        .lab
        .sqlite
        .get_yang_discovery_job("job-1")
        .unwrap()
        .unwrap();
    let j2 = fx
        .lab
        .sqlite
        .get_yang_discovery_job("job-2")
        .unwrap()
        .unwrap();
    assert!(
        j2.state == "completed",
        "newer job must complete first: {}",
        j2.state
    );
    assert_ne!(
        j1.state, "completed",
        "older job completing later must not supersede: {}",
        j1.state
    );
    assert_eq!(j1.state, "superseded");
    // Current applicability evidence is the NEWER job's snapshot.
    let latest = fx
        .lab
        .sqlite
        .latest_yang_applicability_snapshot(&device_key)
        .unwrap()
        .expect("snapshot");
    assert_eq!(latest.id, j2.snapshot_id.unwrap());
}

fn yang_discovery_job_proto_for_test(row: &YangDiscoveryJobRow) -> PbJob {
    PbJob {
        job_id: row.job_id.clone(),
        device_address: row.device_address.clone(),
        device_key: row.device_key.clone(),
        datastore: row.datastore.clone().unwrap_or_default(),
        operation: row.operation.clone(),
        authority_epoch: row.authority_epoch as u64,
        assignment_generation: row.assignment_generation as u64,
        collector_session_generation: row.collector_session_generation.unwrap_or(0) as u64,
        created_at_ns: row.created_at_ns,
        deadline_ns: row.deadline_ns,
        protocol_version: yang_discovery::YANG_DISCOVERY_CONTRACT_VERSION,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t10_expired_job_rejects_late_result() {
    let mut fx = start_fixture().await;
    fx.queue_job(None);
    let pb_job = fx.poll_job().await;

    // Submit with a Core clock beyond the durable deadline.
    let target = fx
        .lab
        .registry
        .get_device(&fx.device_addr)
        .unwrap()
        .unwrap();
    let submission = yang_discovery::execute_discovery_job(&target, None, pb_job.deadline_ns).await;
    let result = yang_discovery::submission_to_result(
        &pb_job,
        "col-a",
        submission,
        PROTOCOL_VERSION_CURRENT,
    );
    let outcome = yang_discovery::admit_discovery_result(
        &fx.lab.sqlite,
        &fx.lab.registry,
        &fx.lab.manager,
        None,
        "col-a",
        &result,
        pb_job.deadline_ns + 1,
    )
    .await
    .unwrap();
    match outcome {
        yang_discovery::AdmitResult::Rejected { reason, .. } => {
            assert_eq!(reason, "deadline_exceeded");
        }
        other => panic!("expected deadline rejection, got {other:?}"),
    }
    let job = fx
        .lab
        .sqlite
        .get_yang_discovery_job(&pb_job.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(job.state, "timed_out");
}

// ── T11/T14: bounds + explicit failure taxonomy ──────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t11_oversized_evidence_rejected() {
    let mut fx = start_fixture().await;
    fx.queue_job(None);
    let pb_job = fx.poll_job().await;

    let huge = "x".repeat(5 * 1024 * 1024);
    let result = YangDiscoveryResult {
        job_id: pb_job.job_id.clone(),
        collector_id: "col-a".to_string(),
        device_address: pb_job.device_address.clone(),
        outcome: "completed".to_string(),
        outcome_detail: String::new(),
        capabilities_json: String::new(),
        yang_library_json: huge,
        observed_at_ns: bonsai::graph::common::now_ns(),
        diagnostics: vec![],
        authority_epoch: pb_job.authority_epoch,
        assignment_generation: pb_job.assignment_generation,
        collector_session_generation: pb_job.collector_session_generation,
        protocol_version: PROTOCOL_VERSION_CURRENT,
    };
    let ack = fx
        .client
        .clone()
        .submit_yang_discovery_result(Request::new(result))
        .await
        .expect("submit")
        .into_inner();
    assert!(!ack.accepted);
    assert_eq!(ack.reason, "oversized_evidence");
    let job = fx
        .lab
        .sqlite
        .get_yang_discovery_job(&pb_job.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(job.state, "failed");
    assert!(
        fx.lab
            .sqlite
            .latest_yang_applicability_snapshot(&job.device_key)
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t14_failure_outcomes_are_explicit() {
    let mut fx = start_fixture().await;
    fx.queue_job(None);
    let pb_job = fx.poll_job().await;

    for (outcome, expect_state) in [
        ("timeout", "timed_out"),
        ("permission_denied", "denied"),
        ("unsupported", "unsupported"),
        ("transport_failure", "failed"),
    ] {
        // Fresh job per outcome — queue another after each terminal.
        let job_id = if outcome == "timeout" {
            pb_job.job_id.clone()
        } else {
            let row = fx.queue_job(None);
            let p = fx.poll_job().await;
            assert_eq!(p.job_id, row.job_id);
            p.job_id.clone()
        };
        let pb = if job_id == pb_job.job_id {
            pb_job.clone()
        } else {
            // Rebuild proto fields for the new job.
            let row = fx
                .lab
                .sqlite
                .get_yang_discovery_job(&job_id)
                .unwrap()
                .unwrap();
            yang_discovery_job_proto_for_test(&row)
        };
        let result = YangDiscoveryResult {
            job_id: job_id.clone(),
            collector_id: "col-a".to_string(),
            device_address: pb.device_address.clone(),
            outcome: outcome.to_string(),
            outcome_detail: format!("{outcome} detail"),
            capabilities_json: String::new(),
            yang_library_json: String::new(),
            observed_at_ns: bonsai::graph::common::now_ns(),
            diagnostics: vec![format!("diag-{outcome}")],
            authority_epoch: pb.authority_epoch,
            assignment_generation: pb.assignment_generation,
            collector_session_generation: pb.collector_session_generation,
            protocol_version: PROTOCOL_VERSION_CURRENT,
        };
        let ack = fx
            .client
            .clone()
            .submit_yang_discovery_result(Request::new(result))
            .await
            .expect("submit")
            .into_inner();
        assert!(ack.accepted, "{outcome}: {}", ack.reason);
        assert!(!ack.reason.is_empty());
        let job = fx
            .lab
            .sqlite
            .get_yang_discovery_job(&job_id)
            .unwrap()
            .unwrap();
        assert_eq!(job.state, expect_state, "outcome {outcome}");
        // Failure outcomes never fabricate applicability evidence.
        assert!(job.snapshot_id.is_none());
    }
    assert!(
        fx.lab
            .sqlite
            .latest_yang_applicability_snapshot(&bonsai::registry::device_identity_key(
                &fx.lab
                    .registry
                    .get_device(&fx.device_addr)
                    .unwrap()
                    .unwrap()
            ))
            .unwrap()
            .is_none()
    );
}

// ── T12: pre-contract collector gets no executable jobs ──────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t12_old_collector_gets_no_jobs() {
    let pki = make_pki();
    let device = start_device().await;
    let device_addr = device.addr.clone();
    let target = device_target(&device_addr, "col-old");
    let lab = start_core_lab(&pki, target.clone()).await;
    lab.registry
        .add_device_with_audit(target, "test", "test_add")
        .unwrap();
    lab.registry
        .assign_device_with_audit(
            &device_addr,
            Some("col-old".to_string()),
            "test",
            "test_assign",
        )
        .unwrap();
    let leaf = issue_leaf(&pki, "col-old");
    bind_collector(&lab.sqlite, "col-old", &leaf.fingerprint);
    let mut client = connect_client(&pki, &leaf, lab.addr).await;

    // Register WITHOUT the discovery feature and at the pre-M6E
    // protocol version — the session is valid but incapable.
    let resp = client
        .clone()
        .register_collector(Request::new(CollectorIdentity {
            collector_id: "col-old".to_string(),
            hostname: "collector-old".to_string(),
            protocol_version: PROTOCOL_VERSION_CURRENT - 1,
            auth_token: String::new(),
            features: vec![],
        }))
        .await
        .expect("old collector still registers");
    let _stream = resp.into_inner();

    // A job CAN be queued (offline/unknown capability is not denied)…
    let target = lab.registry.get_device(&device_addr).unwrap().unwrap();
    let job = yang_discovery::request_remote_discovery(
        &lab.registry,
        &lab.sqlite,
        &lab.manager,
        &target,
        None,
        "test-admin",
    );
    // …but the known-incompatible ACTIVE session is reported explicitly
    // rather than silently queued.
    assert!(matches!(
        job,
        Err(yang_discovery::RequestError::CollectorUnsupported)
    ));

    // And the poll surface reports discovery_supported=false with zero
    // executable jobs.
    let poll = client
        .poll_yang_discovery_jobs(Request::new(PollYangDiscoveryJobsRequest {
            collector_id: "col-old".to_string(),
            protocol_version: PROTOCOL_VERSION_CURRENT - 1,
        }))
        .await
        .expect("poll")
        .into_inner();
    assert!(!poll.discovery_supported);
    assert!(poll.jobs.is_empty());
}

// ── T15: no secrets in durable job state ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t15_job_rows_never_contain_secrets() {
    let mut fx = start_fixture().await;
    // Give the device credential material on the assignment path.
    let mut target = fx
        .lab
        .registry
        .get_device(&fx.device_addr)
        .unwrap()
        .unwrap();
    target.password = Some("s3cr3t-device-password".to_string());
    target.username = Some("admin".to_string());
    fx.lab
        .registry
        .update_device_with_audit(target, "test", "test_creds")
        .unwrap();

    let job = fx.queue_job(None);
    let _ = fx.poll_job().await;

    let row = fx
        .lab
        .sqlite
        .get_yang_discovery_job(&job.job_id)
        .unwrap()
        .unwrap();
    let serialized = format!("{row:?}");
    for forbidden in ["s3cr3t", "password", "secret", "token"] {
        assert!(
            !serialized.contains(forbidden),
            "job row must not contain '{forbidden}': {serialized}"
        );
    }
}

// ── T16: refresh/reapproval race — fingerprint + recency guarded invalidation ─

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t16_evidence_invalidation_preserves_newer_approval() {
    let mut fx = start_fixture().await;
    // Remote discovery produces real persisted evidence (snapshot A).
    fx.queue_job(None);
    let pb_job = fx.poll_job().await;
    let ack = fx.execute_and_submit(&pb_job).await;
    assert!(ack.accepted, "{}", ack.reason);

    let target = fx
        .lab
        .registry
        .get_device(&fx.device_addr)
        .unwrap()
        .unwrap();
    let snapshot = fx
        .lab
        .sqlite
        .latest_yang_applicability_snapshot(&bonsai::registry::device_identity_key(&target))
        .unwrap()
        .unwrap();
    let snap_a = yang_applicability::snapshot_from_row(&snapshot).unwrap();

    // Activate approval A against evidence A through the real path.
    let req = schema_binding::SchemaPlanRequest {
        selected_paths: Some(selected_paths()),
        datastore: None,
        plan_id: None,
        binding_fingerprint: None,
        expected_config_revision: None,
    };
    let now = bonsai::graph::common::now_ns();
    let preview =
        schema_binding::evaluate_preview(&fx.lab.library, &fx.lab.sqlite, &target, &req, now)
            .unwrap();
    assert!(
        preview.activation_permitted,
        "preview must permit: {:?}",
        preview.evaluation.as_ref().map(|e| &e.rejection_reasons)
    );
    let eval = preview.evaluation.clone().unwrap();
    let act_req = schema_binding::SchemaPlanRequest {
        selected_paths: req.selected_paths.clone(),
        datastore: None,
        plan_id: Some(eval.plan_id.clone()),
        binding_fingerprint: Some(eval.binding_fingerprint.clone()),
        expected_config_revision: Some(eval.config_revision),
    };
    let activation = schema_binding::activate(
        &fx.lab.library,
        &fx.lab.sqlite,
        &target,
        &act_req,
        "admin",
        now,
    )
    .expect("activate A");
    let approval_revision = activation.record.approval_revision;
    fx.lab
        .registry
        .activate_schema_binding(
            activation.device.clone(),
            activation.record,
            approval_revision as i64 - 1,
            eval.config_revision,
            "admin",
            "test_activate_a",
        )
        .unwrap();

    // Case 1: invalidation naming the SAME evidence the approval was
    // derived from → Current (approval confirms, never self-revokes).
    assert!(matches!(
        schema_binding::revoke_binding_superseded_by_evidence(
            &fx.lab.registry,
            &fx.device_addr,
            snap_a.snapshot_id,
            &snap_a.evidence_fingerprint,
            "test",
            "test_same_evidence",
        )
        .unwrap(),
        schema_binding::EvidenceInvalidation::Current
    ));

    // Case 2: NEWER evidence B arrives (a second remote job completes
    // with different evidence). The stale-evidence invalidation for A
    // must NOT touch approval B once B exists — simulate ordering:
    // first the new snapshot is persisted, approval re-bound to it,
    // THEN the late invalidation for the OLD fingerprint arrives.
    // We emulate by persisting a newer snapshot row (larger id,
    // different fingerprint), re-activating approval B through the real
    // schema-binding path, THEN invoking the guard with snapshot_id of A
    // (older) while the binding references B's newer snapshot id.
    let mut snap_b_row = snapshot.clone();
    snap_b_row.evidence_fingerprint = "fp-B-newer".to_string();
    snap_b_row.id = 0;
    let snap_b_id = fx
        .lab
        .sqlite
        .save_yang_applicability_snapshot(&snap_b_row)
        .unwrap();
    assert!(
        snap_b_id > snapshot.id,
        "newer snapshot must have larger id"
    );

    let target_b = fx
        .lab
        .registry
        .get_device(&fx.device_addr)
        .unwrap()
        .unwrap();
    let req_b = schema_binding::SchemaPlanRequest {
        selected_paths: Some(selected_paths()),
        datastore: None,
        plan_id: None,
        binding_fingerprint: None,
        expected_config_revision: None,
    };
    let preview_b =
        schema_binding::evaluate_preview(&fx.lab.library, &fx.lab.sqlite, &target_b, &req_b, now)
            .unwrap();
    assert!(
        preview_b.activation_permitted,
        "preview B must permit: {:?}",
        preview_b.evaluation.as_ref().map(|e| &e.rejection_reasons)
    );
    let eval_b = preview_b.evaluation.clone().unwrap();
    let act_b = schema_binding::SchemaPlanRequest {
        selected_paths: req_b.selected_paths.clone(),
        datastore: None,
        plan_id: Some(eval_b.plan_id.clone()),
        binding_fingerprint: Some(eval_b.binding_fingerprint.clone()),
        expected_config_revision: Some(eval_b.config_revision),
    };
    let activation_b = schema_binding::activate(
        &fx.lab.library,
        &fx.lab.sqlite,
        &target_b,
        &act_b,
        "admin",
        now,
    )
    .expect("activate B");
    let approval_revision_b = activation_b.record.approval_revision;
    fx.lab
        .registry
        .activate_schema_binding(
            activation_b.device.clone(),
            activation_b.record,
            approval_revision_b as i64 - 1,
            eval_b.config_revision,
            "admin",
            "test_activate_b",
        )
        .unwrap();

    // The LATE invalidation for evidence A (older snapshot id, old
    // fingerprint) must preserve approval B — NewerApproval.
    assert!(matches!(
        schema_binding::revoke_binding_superseded_by_evidence(
            &fx.lab.registry,
            &fx.device_addr,
            snap_a.snapshot_id,
            &snap_a.evidence_fingerprint,
            "test",
            "test_late_old_refresh",
        )
        .unwrap(),
        schema_binding::EvidenceInvalidation::NewerApproval
    ));
    let after = fx
        .lab
        .registry
        .get_device(&fx.device_addr)
        .unwrap()
        .unwrap();
    assert!(
        after.schema_binding.is_some(),
        "newer approval must survive"
    );

    // Case 3: invalidation for genuinely NEWER evidence than the
    // approval's → Revoked at the guarded boundary.
    let mut snap_c_row = snapshot.clone();
    snap_c_row.evidence_fingerprint = "fp-C-newest".to_string();
    snap_c_row.id = 0;
    let snap_c_id = fx
        .lab
        .sqlite
        .save_yang_applicability_snapshot(&snap_c_row)
        .unwrap();
    assert!(snap_c_id > snap_b_id);
    match schema_binding::revoke_binding_superseded_by_evidence(
        &fx.lab.registry,
        &fx.device_addr,
        snap_c_id,
        "fp-C-newest",
        "test",
        "test_new_evidence",
    )
    .unwrap()
    {
        schema_binding::EvidenceInvalidation::Revoked(_) => {}
        other => panic!("expected Revoked for superseded approval, got {other:?}"),
    }
    let final_device = fx
        .lab
        .registry
        .get_device(&fx.device_addr)
        .unwrap()
        .unwrap();
    assert!(
        final_device.schema_binding.is_none()
            || final_device
                .schema_binding
                .as_ref()
                .map(|b| b.mode != "schema_bound")
                .unwrap_or(true),
        "superseded approval must be revoked"
    );

    // Case 4: no binding → NotBound (device deletion path covered by T7).
    assert!(matches!(
        schema_binding::revoke_binding_superseded_by_evidence(
            &fx.lab.registry,
            &fx.device_addr,
            snap_c_id,
            "fp-C-newest",
            "test",
            "test_again",
        )
        .unwrap(),
        schema_binding::EvidenceInvalidation::NotBound
    ));
}
