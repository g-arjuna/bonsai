//! M5.2A remote assignment & security lifecycle — runtime verification
//! against a real (local, recording) gNMI server through the ACTUAL
//! collector assignment-update path (`ingest::handle_assignment_update`),
//! not direct subscriber cancellation. Asserts stream termination,
//! replacement Subscribe RPCs, generation fencing and fail-closed
//! behavior. No vendor interoperability is claimed; local contract
//! verification only.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bonsai::api::managed_device_from_target;
use bonsai::api::pb::{AssignmentUpdate, DeviceAssignment};
use bonsai::config::{AcquisitionMode, GnmiSecurityMode, SelectedSubscriptionPath, TargetConfig};
use bonsai::event_bus::InProcessBus;
use bonsai::gnmi_security::ConnSecurity;
use bonsai::ingest::{
    DISCONNECTED_ASSIGNMENT_LEASE, enforce_assignment_disconnect_lease, handle_assignment_update,
};
use bonsai::proto::gnmi::g_nmi_server::{GNmi, GNmiServer};
use bonsai::proto::gnmi::{
    CapabilityRequest, CapabilityResponse, Encoding, ModelData, Path, SubscribeRequest,
    SubscribeResponse, subscribe_request,
};
use bonsai::subscriber::SubscriberHandleMap;
use bonsai::subscription_status::SubscriptionPlan;
use tokio::sync::{Mutex, mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming, transport::Server};

// ── recording mock gNMI server (same contract as gnmi_selective_acquisition) ──

#[derive(Clone)]
struct MockGnmi {
    caps: CapabilityResponse,
    requests: Arc<Mutex<Vec<SubscribeRequest>>>,
    subscribe_calls: Arc<AtomicUsize>,
    /// Live Subscribe response streams; decremented only when the client
    /// cancels the RPC — proof the old stream really ended.
    active_streams: Arc<AtomicUsize>,
}

#[tonic::async_trait]
impl GNmi for MockGnmi {
    async fn capabilities(
        &self,
        _request: Request<CapabilityRequest>,
    ) -> Result<Response<CapabilityResponse>, Status> {
        Ok(Response::new(self.caps.clone()))
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
                requests.lock().await.push(msg);
            }
        });
        self.active_streams.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel::<Result<SubscribeResponse, Status>>(8);
        std::mem::forget(tx);
        Ok(Response::new(GuardedStream {
            inner: ReceiverStream::new(rx),
            _guard: StreamGuard(Arc::clone(&self.active_streams)),
        }))
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

struct Lab {
    addr: String,
    requests: Arc<Mutex<Vec<SubscribeRequest>>>,
    subscribe_calls: Arc<AtomicUsize>,
    active_streams: Arc<AtomicUsize>,
    _shutdown: watch::Sender<bool>,
    _handle: tokio::task::JoinHandle<()>,
}

async fn start_lab() -> Lab {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let subscribe_calls = Arc::new(AtomicUsize::new(0));
    let active_streams = Arc::new(AtomicUsize::new(0));
    let svc = MockGnmi {
        caps: CapabilityResponse {
            supported_models: vec![
                ModelData {
                    name: "openconfig-interfaces".to_string(),
                    organization: "openconfig".to_string(),
                    version: "1.0".to_string(),
                },
                ModelData {
                    name: "openconfig-system".to_string(),
                    organization: "openconfig".to_string(),
                    version: "1.0".to_string(),
                },
            ],
            supported_encodings: vec![Encoding::JsonIetf as i32, Encoding::Proto as i32],
            g_nmi_version: "0.0.0".to_string(),
            ..Default::default()
        },
        requests: Arc::clone(&requests),
        subscribe_calls: Arc::clone(&subscribe_calls),
        active_streams: Arc::clone(&active_streams),
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
    Lab {
        addr,
        requests,
        subscribe_calls,
        active_streams,
        _shutdown: shutdown_tx,
        _handle: handle,
    }
}

// ── fixtures ─────────────────────────────────────────────────────────────────

fn selected(path: &str) -> SelectedSubscriptionPath {
    SelectedSubscriptionPath {
        path: path.to_string(),
        origin: "openconfig".to_string(),
        provenance: String::new(),
        mode: "sample".to_string(),
        sample_interval_ns: 10_000_000_000,
        rationale: String::new(),
        optional: false,
    }
}

/// Lab-authorized target: explicit `security="plaintext"` is the
/// deliberate isolated-laboratory opt-in (never implicit).
fn lab_target(lab: &Lab, paths: Vec<SelectedSubscriptionPath>) -> TargetConfig {
    TargetConfig {
        address: lab.addr.clone(),
        enabled: true,
        security: Some(GnmiSecurityMode::Plaintext),
        acquisition: AcquisitionMode::Explicit,
        selected_paths: paths,
        vendor: Some("openconfig".to_string()),
        ..Default::default()
    }
}

fn assignment_for(target: &TargetConfig, username: &str, password: &str) -> DeviceAssignment {
    DeviceAssignment {
        device: Some(managed_device_from_target(target)),
        username: username.to_string(),
        password: password.to_string(),
        rejected: false,
        reject_reason: String::new(),
    }
}

fn update(generation: u64, assignments: Vec<DeviceAssignment>) -> AssignmentUpdate {
    AssignmentUpdate {
        assignments,
        is_full_sync: true,
        generation,
        authority_epoch: 0,
        collector_session_generation: 0,
    }
}

fn epoch_update(
    epoch: u64,
    generation: u64,
    full_sync: bool,
    assignments: Vec<DeviceAssignment>,
) -> AssignmentUpdate {
    AssignmentUpdate {
        assignments,
        is_full_sync: full_sync,
        generation,
        authority_epoch: epoch,
        collector_session_generation: 0,
    }
}

struct Rig {
    bus: Arc<InProcessBus>,
    plan_tx: mpsc::Sender<SubscriptionPlan>,
    plan_rx: mpsc::Receiver<SubscriptionPlan>,
    subscribers: SubscriberHandleMap,
    last_epoch: u64,
    last_generation: u64,
}

impl Rig {
    fn new() -> Self {
        let (plan_tx, plan_rx) = mpsc::channel(64);
        Self {
            bus: InProcessBus::new(64),
            plan_tx,
            plan_rx,
            subscribers: SubscriberHandleMap::new(),
            last_epoch: 0,
            last_generation: 0,
        }
    }

    async fn apply(&mut self, update: AssignmentUpdate) {
        let plans = Some(self.plan_tx.clone());
        handle_assignment_update(
            update,
            &self.bus,
            &plans,
            &mut self.subscribers,
            &mut self.last_epoch,
            &mut self.last_generation,
            &None,
        )
        .await;
    }
}

fn path_string(path: &Path) -> String {
    let mut out = path.origin.clone();
    for elem in &path.elem {
        out.push('/');
        out.push_str(&elem.name);
        if !elem.key.is_empty() {
            let mut pairs: Vec<_> = elem.key.iter().collect();
            pairs.sort();
            out.push('[');
            for (i, (k, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&format!("{}={}", k, v));
            }
            out.push(']');
        }
    }
    out
}

/// All subscribed path strings across every recorded SubscribeRequest.
async fn recorded_paths(lab: &Lab) -> HashSet<String> {
    let reqs = lab.requests.lock().await;
    let mut out = HashSet::new();
    for req in reqs.iter() {
        if let Some(subscribe_request::Request::Subscribe(list)) = &req.request {
            for sub in &list.subscription {
                if let Some(path) = &sub.path {
                    out.insert(path_string(path));
                }
            }
        }
    }
    out
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

async fn wait_streams(lab: &Lab, n: usize) -> bool {
    wait_for(
        || lab.active_streams.load(Ordering::SeqCst) == n,
        Duration::from_secs(10),
    )
    .await
}

async fn wait_subscribes(lab: &Lab, n: usize) -> bool {
    wait_for(
        || lab.subscribe_calls.load(Ordering::SeqCst) >= n,
        Duration::from_secs(10),
    )
    .await
}

// ── A2 lifecycle cases ───────────────────────────────────────────────────────

/// 1. Unchanged full sync must NOT restart a healthy subscriber.
#[tokio::test]
async fn unchanged_full_sync_preserves_subscriber() {
    let lab = start_lab().await;
    let mut rig = Rig::new();
    let target = lab_target(&lab, vec![selected("/interfaces/interface[name=eth0]")]);

    rig.apply(update(1, vec![assignment_for(&target, "u", "p")]))
        .await;
    assert!(
        wait_subscribes(&lab, 1).await,
        "first subscribe never arrived"
    );
    assert!(wait_streams(&lab, 1).await);

    // Identical effective assignment under a NEW generation: fingerprint
    // match → preserved, no restart, no second Subscribe RPC.
    rig.apply(update(2, vec![assignment_for(&target, "u", "p")]))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 1);
    assert_eq!(lab.active_streams.load(Ordering::SeqCst), 1);
    assert_eq!(rig.subscribers.len(), 1);
}

/// 2. Changed selected paths: old stream ends, exactly the new paths are
/// requested on the replacement session.
#[tokio::test]
async fn changed_paths_replace_stream_and_request_new_paths() {
    let lab = start_lab().await;
    let mut rig = Rig::new();
    let t1 = lab_target(
        &lab,
        vec![
            selected("/interfaces/interface[name=eth0]"),
            selected("/system"),
        ],
    );

    rig.apply(update(1, vec![assignment_for(&t1, "u", "p")]))
        .await;
    assert!(wait_subscribes(&lab, 1).await);
    assert!(wait_streams(&lab, 1).await);
    lab.requests.lock().await.clear();

    // Same device, narrower explicit selection.
    let t2 = lab_target(&lab, vec![selected("/system")]);
    rig.apply(update(2, vec![assignment_for(&t2, "u", "p")]))
        .await;

    assert!(
        wait_subscribes(&lab, 2).await,
        "replacement subscribe missing"
    );
    assert!(wait_streams(&lab, 1).await, "old stream did not end");
    let paths = recorded_paths(&lab).await;
    assert!(
        paths.iter().any(|p| p.contains("/system")),
        "new path missing: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.contains("eth0")),
        "obsolete path still requested: {paths:?}"
    );
}

/// 3. Explicit empty selection ends the old stream and emits NO new
/// Subscribe RPC.
#[tokio::test]
async fn explicit_empty_selection_stops_without_new_subscribe() {
    let lab = start_lab().await;
    let mut rig = Rig::new();
    let t1 = lab_target(&lab, vec![selected("/system")]);

    rig.apply(update(1, vec![assignment_for(&t1, "u", "p")]))
        .await;
    assert!(wait_subscribes(&lab, 1).await);
    assert!(wait_streams(&lab, 1).await);

    let t2 = lab_target(&lab, Vec::new()); // explicit mode, empty set
    rig.apply(update(2, vec![assignment_for(&t2, "u", "p")]))
        .await;

    assert!(wait_streams(&lab, 0).await, "stream not stopped");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        lab.subscribe_calls.load(Ordering::SeqCst),
        1,
        "empty selection must not emit a broader replacement Subscribe RPC"
    );
    assert!(rig.subscribers.is_empty());
}

/// 4. Administrative disablement ends the old stream.
#[tokio::test]
async fn disabled_assignment_stops_stream() {
    let lab = start_lab().await;
    let mut rig = Rig::new();
    let t1 = lab_target(&lab, vec![selected("/system")]);
    rig.apply(update(1, vec![assignment_for(&t1, "u", "p")]))
        .await;
    assert!(wait_subscribes(&lab, 1).await);
    assert!(wait_streams(&lab, 1).await);

    let mut t2 = lab_target(&lab, vec![selected("/system")]);
    t2.enabled = false;
    rig.apply(update(2, vec![assignment_for(&t2, "u", "p")]))
        .await;

    assert!(
        wait_streams(&lab, 0).await,
        "disabled target kept streaming"
    );
    assert!(rig.subscribers.is_empty());
}

/// 5. plaintext → tls_verify policy change ends the insecure stream. The
/// replacement cannot complete a TLS handshake against the plaintext lab
/// server, but the OBSOLETE insecure stream must be gone — never
/// silently preserved.
#[tokio::test]
async fn plaintext_to_tls_change_ends_insecure_stream() {
    let lab = start_lab().await;
    let mut rig = Rig::new();
    let t1 = lab_target(&lab, vec![selected("/system")]);
    rig.apply(update(1, vec![assignment_for(&t1, "u", "p")]))
        .await;
    assert!(wait_subscribes(&lab, 1).await);
    assert!(wait_streams(&lab, 1).await);

    // Self-signed CA material (any PEM the resolver accepts); the TLS
    // dial will fail against the plaintext lab — that is expected and
    // irrelevant to the assertion: the plaintext stream must end.
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(Vec::new()).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let ca = params.self_signed(&key).unwrap();
    let dir = std::env::temp_dir().join(format!("m52a-ca-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ca_path = dir.join("ca.pem");
    std::fs::write(&ca_path, ca.pem()).unwrap();

    let mut t2 = lab_target(&lab, vec![selected("/system")]);
    t2.security = Some(GnmiSecurityMode::TlsVerify);
    t2.tls_domain = Some("localhost".to_string());
    t2.ca_cert = Some(ca_path.to_string_lossy().to_string());
    rig.apply(update(2, vec![assignment_for(&t2, "u", "p")]))
        .await;

    assert!(
        wait_streams(&lab, 0).await,
        "plaintext stream survived a TLS policy change"
    );
    // The replacement dials TLS and fails — it must NOT fall back to a
    // plaintext Subscribe RPC.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(lab.active_streams.load(Ordering::SeqCst), 0);
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 1);
}

/// 6. Invalid replacement security fails closed: the obsolete insecure
/// stream is stopped and an actionable rejected status is published.
#[tokio::test]
async fn invalid_replacement_security_fails_closed() {
    let lab = start_lab().await;
    let mut rig = Rig::new();
    let t1 = lab_target(&lab, vec![selected("/system")]);
    rig.apply(update(1, vec![assignment_for(&t1, "u", "p")]))
        .await;
    assert!(wait_subscribes(&lab, 1).await);
    assert!(wait_streams(&lab, 1).await);

    // mTLS without client cert/key material → resolution fails closed.
    let mut t2 = lab_target(&lab, vec![selected("/system")]);
    t2.security = Some(GnmiSecurityMode::Mtls);
    t2.tls_domain = Some("localhost".to_string());
    rig.apply(update(2, vec![assignment_for(&t2, "u", "p")]))
        .await;

    assert!(wait_streams(&lab, 0).await, "obsolete stream preserved");
    assert!(rig.subscribers.is_empty());
    // The rejected status may arrive after earlier queued plan updates —
    // scan a bounded window for it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut rejected = None;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rig.plan_rx.recv()).await {
            Ok(Some(plan)) if plan.state == "rejected" => {
                rejected = Some(plan);
                break;
            }
            Ok(Some(_)) => continue,
            _ => break,
        }
    }
    let plan = rejected.expect("rejected plan status never published");
    assert!(!plan.detail.is_empty());
}

/// 7. Credential rotation under unchanged references replaces the
/// subscriber: the fingerprint covers effective credential material.
#[tokio::test]
async fn credential_rotation_replaces_session() {
    let lab = start_lab().await;
    let mut rig = Rig::new();
    let target = lab_target(&lab, vec![selected("/system")]);

    rig.apply(update(1, vec![assignment_for(&target, "u", "old-pass")]))
        .await;
    assert!(wait_subscribes(&lab, 1).await);
    assert!(wait_streams(&lab, 1).await);

    rig.apply(update(2, vec![assignment_for(&target, "u", "new-pass")]))
        .await;
    assert!(wait_subscribes(&lab, 2).await, "rotated session missing");
    assert!(wait_streams(&lab, 1).await);
    assert_eq!(rig.subscribers.len(), 1);
}

/// 8. Assignment removal terminates the subscriber.
#[tokio::test]
async fn assignment_removal_stops_subscriber() {
    let lab = start_lab().await;
    let mut rig = Rig::new();
    let target = lab_target(&lab, vec![selected("/system")]);
    rig.apply(update(1, vec![assignment_for(&target, "u", "p")]))
        .await;
    assert!(wait_subscribes(&lab, 1).await);
    assert!(wait_streams(&lab, 1).await);

    rig.apply(update(2, vec![])).await; // empty full sync = removal
    assert!(wait_streams(&lab, 0).await);
    assert!(rig.subscribers.is_empty());
}

/// 9/10. Duplicate delivery and stale generations cannot mutate state.
#[tokio::test]
async fn duplicate_and_stale_generations_are_fenced() {
    let lab = start_lab().await;
    let mut rig = Rig::new();
    let t1 = lab_target(&lab, vec![selected("/system")]);
    let u1 = update(5, vec![assignment_for(&t1, "u", "p")]);
    rig.apply(u1.clone()).await;
    assert!(wait_subscribes(&lab, 1).await);
    assert!(wait_streams(&lab, 1).await);

    // Exact duplicate (same generation, same content): ignored.
    rig.apply(u1).await;
    // Stale generation attempting a different (broader) plan: fenced.
    let t_broad = lab_target(
        &lab,
        vec![
            selected("/system"),
            selected("/interfaces/interface[name=eth0]"),
        ],
    );
    rig.apply(update(3, vec![assignment_for(&t_broad, "u", "p")]))
        .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 1);
    assert_eq!(lab.active_streams.load(Ordering::SeqCst), 1);
    let paths = recorded_paths(&lab).await;
    assert!(
        !paths.iter().any(|p| p.contains("eth0")),
        "stale generation restored broader authority: {paths:?}"
    );
}

/// 11. Bounded disconnect policy: subscribers held past the lease are
/// stopped; inside the lease they are preserved.
#[tokio::test]
async fn disconnect_lease_is_bounded() {
    let lab = start_lab().await;
    let mut rig = Rig::new();
    let target = lab_target(&lab, vec![selected("/system")]);
    rig.apply(update(1, vec![assignment_for(&target, "u", "p")]))
        .await;
    assert!(wait_subscribes(&lab, 1).await);
    assert!(wait_streams(&lab, 1).await);

    // Inside the lease: preserved.
    let mut since = Some(tokio::time::Instant::now());
    enforce_assignment_disconnect_lease(&mut rig.subscribers, &mut since).await;
    assert_eq!(rig.subscribers.len(), 1);

    // Past the lease: stopped.
    since =
        Some(tokio::time::Instant::now() - DISCONNECTED_ASSIGNMENT_LEASE - Duration::from_secs(1));
    enforce_assignment_disconnect_lease(&mut rig.subscribers, &mut since).await;
    assert!(rig.subscribers.is_empty());
    assert!(wait_streams(&lab, 0).await);
}

/// A rejected assignment from core produces no subscriber and publishes
/// the explicit rejected outcome (fail closed, never anonymous).
#[tokio::test]
async fn rejected_assignment_never_runs_subscriber() {
    let lab = start_lab().await;
    let mut rig = Rig::new();
    let target = lab_target(&lab, vec![selected("/system")]);
    let mut rejected = assignment_for(&target, "", "");
    rejected.rejected = true;
    rejected.reject_reason = "credential_unavailable: 'alias' (locked)".to_string();

    rig.apply(update(1, vec![rejected])).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 0);
    assert!(rig.subscribers.is_empty());
    let plan = tokio::time::timeout(Duration::from_secs(5), rig.plan_rx.recv())
        .await
        .expect("rejected plan status never published")
        .expect("plan channel closed");
    assert_eq!(plan.state, "rejected");
    assert!(plan.detail.contains("credential_unavailable"));
}

/// ConnSecurity helper retained for fingerprint-path compile coverage of
/// the explicit-lab authorization boundary used across these tests.
#[allow(dead_code)]
fn _assert_lab_transport_authorized() {
    assert!(
        ConnSecurity::plaintext(true)
            .effective
            .authorized_for_acquisition()
    );
    assert!(
        !ConnSecurity::plaintext(false)
            .effective
            .authorized_for_acquisition()
    );
}

// ── authority epoch fencing (restart-safe contract) ─────────────────────────

/// A newer authority epoch is honoured ONLY via full sync; an older
/// epoch is always rejected — a core restart cannot be impersonated by
/// delayed previous-authority updates.
#[tokio::test]
async fn authority_epoch_fencing_restart_safe() {
    let lab = start_lab().await;
    let mut rig = Rig::new();
    let target = lab_target(&lab, vec![selected("/system")]);

    // Epoch 100, generation 5, full sync → establishes authority.
    rig.apply(epoch_update(
        100,
        5,
        true,
        vec![assignment_for(&target, "u", "p")],
    ))
    .await;
    assert!(wait_subscribes(&lab, 1).await);
    assert!(wait_streams(&lab, 1).await);

    // Same epoch, older generation → fenced.
    rig.apply(epoch_update(100, 3, true, vec![])).await;
    // Older epoch with a HIGH generation → still fenced (epoch wins).
    rig.apply(epoch_update(99, 999, true, vec![])).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lab.active_streams.load(Ordering::SeqCst), 1);

    // New epoch 200 but INCREMENTAL (not full sync) → rejected: new
    // authority must be established by authoritative synchronization.
    rig.apply(epoch_update(200, 1, false, vec![])).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lab.active_streams.load(Ordering::SeqCst), 1);

    // New epoch 200 full sync → accepted; stream reconciles (empty =
    // removal, so the subscriber stops).
    rig.apply(epoch_update(200, 1, true, vec![])).await;
    assert!(
        wait_streams(&lab, 0).await,
        "new-epoch full sync not applied"
    );

    // Now epoch 100 again (delayed previous authority) → rejected even
    // though it carries a full sync.
    rig.apply(epoch_update(
        100,
        50,
        true,
        vec![assignment_for(&target, "u", "p")],
    ))
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 1);
    assert!(rig.subscribers.is_empty());
}

/// A malformed/unconvertible assignment device must stop the obsolete
/// subscriber — it cannot preserve stale acquisition authority even
/// though its address stays in the full-sync set.
#[tokio::test]
async fn malformed_assignment_stops_obsolete_subscriber() {
    let lab = start_lab().await;
    let mut rig = Rig::new();
    let target = lab_target(&lab, vec![selected("/system")]);
    rig.apply(update(1, vec![assignment_for(&target, "u", "p")]))
        .await;
    assert!(wait_subscribes(&lab, 1).await);
    assert!(wait_streams(&lab, 1).await);

    // Same address, but an unsupported security value the collector
    // cannot convert → fail closed.
    let mut bad_device = managed_device_from_target(&target);
    bad_device.security = "carrier-pigeon".to_string();
    let bad = DeviceAssignment {
        device: Some(bad_device),
        username: "u".to_string(),
        password: "p".to_string(),
        rejected: false,
        reject_reason: String::new(),
    };
    rig.apply(update(2, vec![bad])).await;

    assert!(wait_streams(&lab, 0).await, "obsolete stream preserved");
    assert!(rig.subscribers.is_empty());
    // And no replacement Subscribe RPC was emitted.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 1);
}
