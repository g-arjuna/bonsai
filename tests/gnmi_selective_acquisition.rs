//! M4 selective gNMI acquisition — runtime verification against a real
//! (local, recording) gNMI server. Asserts the exact SubscribeRequest
//! protobuf messages emitted for each acquisition intent. No vendor
//! interoperability is claimed; this is contract verification only.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bonsai::config::{AcquisitionMode, SelectedSubscriptionPath, TargetConfig};
use bonsai::event_bus::InProcessBus;
use bonsai::proto::gnmi::g_nmi_server::{GNmi, GNmiServer};
use bonsai::proto::gnmi::{
    CapabilityRequest, CapabilityResponse, Encoding, ModelData, Notification, Path, PathElem,
    SubscribeRequest, SubscribeResponse, Subscription, Update, subscribe_request,
    subscribe_response, typed_value,
};
use bonsai::subscriber::GnmiSubscriber;
use bonsai::subscription_status::{
    SubscriptionPathExpectation, SubscriptionPlan, run_subscription_verifier,
};
use tokio::sync::{Mutex, mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming, transport::Server};

// ── recording mock gNMI server ────────────────────────────────────────────────

#[derive(Clone)]
struct MockGnmi {
    caps: CapabilityResponse,
    requests: Arc<Mutex<Vec<SubscribeRequest>>>,
    capability_calls: Arc<AtomicUsize>,
    subscribe_calls: Arc<AtomicUsize>,
    /// Live Subscribe response streams: incremented when a Subscribe RPC
    /// opens a stream, decremented when the server drops it — which only
    /// happens when the client cancels the RPC.
    active_streams: Arc<AtomicUsize>,
    /// Response senders for open Subscribe streams — tests inject
    /// Update/SyncResponse notifications through the newest sender.
    streams: Arc<Mutex<Vec<mpsc::Sender<Result<SubscribeResponse, Status>>>>>,
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
        // Hold the response stream open so the client stays subscribed
        // until the test shuts it down. The drop guard proves real RPC
        // cancellation: tonic drops this stream only when the client
        // cancels or the RPC ends. A clone of the sender is retained so
        // tests can inject Update/SyncResponse notifications.
        self.active_streams.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel::<Result<SubscribeResponse, Status>>(8);
        self.streams.lock().await.push(tx.clone());
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

/// Response stream that decrements `active_streams` on drop — proof the
/// client cancelled the Subscribe RPC rather than merely going idle.
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
    capability_calls: Arc<AtomicUsize>,
    subscribe_calls: Arc<AtomicUsize>,
    active_streams: Arc<AtomicUsize>,
    streams: Arc<Mutex<Vec<mpsc::Sender<Result<SubscribeResponse, Status>>>>>,
    _shutdown: watch::Sender<bool>,
    _handle: tokio::task::JoinHandle<()>,
}

async fn start_lab(models: Vec<&str>) -> Lab {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let capability_calls = Arc::new(AtomicUsize::new(0));
    let subscribe_calls = Arc::new(AtomicUsize::new(0));
    let active_streams = Arc::new(AtomicUsize::new(0));
    let streams = Arc::new(Mutex::new(Vec::new()));
    let svc = MockGnmi {
        caps: CapabilityResponse {
            supported_models: models
                .into_iter()
                .map(|name| ModelData {
                    name: name.to_string(),
                    organization: "openconfig".to_string(),
                    version: "1.0".to_string(),
                })
                .collect(),
            supported_encodings: vec![Encoding::JsonIetf as i32, Encoding::Proto as i32],
            g_nmi_version: "0.0.0".to_string(),
            ..Default::default()
        },
        requests: Arc::clone(&requests),
        capability_calls: Arc::clone(&capability_calls),
        subscribe_calls: Arc::clone(&subscribe_calls),
        active_streams: Arc::clone(&active_streams),
        streams: Arc::clone(&streams),
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
        capability_calls,
        subscribe_calls,
        active_streams,
        streams,
        _shutdown: shutdown_tx,
        _handle: handle,
    }
}

fn selected(path: &str, mode: &str) -> SelectedSubscriptionPath {
    SelectedSubscriptionPath {
        path: path.to_string(),
        origin: "openconfig".to_string(),
        provenance: String::new(),
        mode: mode.to_string(),
        sample_interval_ns: 10_000_000_000,
        rationale: String::new(),
        optional: false,
    }
}

fn subscriber(
    lab: &Lab,
    selected_paths: Vec<SelectedSubscriptionPath>,
    acquisition: AcquisitionMode,
    plan_tx: mpsc::Sender<bonsai::subscription_status::SubscriptionPlan>,
) -> GnmiSubscriber {
    // Production wiring (server_startup, RuntimeMode::All): the
    // subscriber's lifecycle events are drained into the authoritative
    // session registry and the APPLIED manifest is forwarded to the
    // coverage verifier — the same post-apply contract as the remote
    // lifecycle RPC. plan_tx also carries plan-level outcomes.
    let sessions = Arc::new(bonsai::subscription_sessions::SubscriptionSessionRegistry::new());
    let (lc_tx, mut lc_rx) =
        mpsc::channel::<bonsai::subscription_sessions::SessionLifecycleEvent>(64);
    let drain_plan_tx = plan_tx.clone();
    let drain_sessions = Arc::clone(&sessions);
    tokio::spawn(async move {
        while let Some(event) = lc_rx.recv().await {
            bonsai::subscription_status::apply_local_lifecycle_event(
                &drain_sessions,
                Some(&drain_plan_tx),
                event,
            )
            .await;
        }
    });
    let lifecycle = bonsai::subscriber::LifecycleHandle {
        tx: lc_tx,
        authority_epoch: 0,
        assignment_generation: 0,
        collector_session_generation: 0,
    };
    GnmiSubscriber::new(
        lab.addr.clone(),
        None,
        None,
        None,
        None,
        None,
        None,
        // Explicit isolated-laboratory plaintext opt-in (M5.2A): legacy-inferred
        // plaintext is no longer authorized for device sessions.
        bonsai::gnmi_security::ConnSecurity::plaintext(true),
        InProcessBus::new(64),
        None,
        Some(plan_tx),
        selected_paths,
        acquisition,
        Some(lifecycle),
        std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
    )
}

/// Extract the SubscriptionList from a recorded SubscribeRequest.
fn subscription_list(req: &SubscribeRequest) -> &[Subscription] {
    match &req.request {
        Some(subscribe_request::Request::Subscribe(list)) => &list.subscription,
        _ => &[],
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
async fn automatic_mode_emits_capability_derived_plan() {
    let lab = start_lab(vec!["openconfig-interfaces", "openconfig-bfd"]).await;
    let (plan_tx, mut plan_rx) = mpsc::channel(8);
    let sub = subscriber(&lab, vec![], AcquisitionMode::Automatic, plan_tx);
    let (_stx, mut srx) = watch::channel(false);
    let handle = tokio::spawn(async move { sub.subscribe_telemetry(&mut srx).await });

    assert!(
        wait_for(
            || lab.subscribe_calls.load(Ordering::SeqCst) > 0,
            Duration::from_secs(10)
        )
        .await,
        "expected a Subscribe RPC for automatic mode"
    );
    // Give the recorder task a moment to drain the request stream.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let requests = lab.requests.lock().await;
    assert_eq!(requests.len(), 1, "exactly one Subscribe request expected");
    let subs = subscription_list(&requests[0]);
    assert!(
        !subs.is_empty(),
        "capability-derived plan must be non-empty"
    );
    assert!(
        subs.iter()
            .all(|s| !path_string(s.path.as_ref().unwrap()).is_empty()),
        "every subscription must carry a structured path"
    );
    let subs_len = subs.len();
    drop(requests);

    let plan = tokio::time::timeout(Duration::from_secs(5), plan_rx.recv())
        .await
        .expect("plan published")
        .expect("plan channel open");
    assert_eq!(plan.state, "submitted");
    assert_eq!(plan.acquisition, "automatic");
    assert!(!plan.plan_id.is_empty());
    assert_eq!(plan.paths.len(), subs_len);

    _stx.send(true).unwrap();
    let _ = handle.await;
}

#[tokio::test]
async fn explicit_mode_emits_only_requested_paths() {
    let lab = start_lab(vec!["openconfig-interfaces"]).await;
    let (plan_tx, mut plan_rx) = mpsc::channel(8);
    let paths = vec![
        selected(
            "interfaces/interface[name=Ethernet1/1]/state/counters",
            "SAMPLE",
        ),
        selected(
            "network-instances/network-instance[name=default]/protocols",
            "ON_CHANGE",
        ),
    ];
    let sub = subscriber(&lab, paths, AcquisitionMode::Explicit, plan_tx);
    let (_stx, mut srx) = watch::channel(false);
    let handle = tokio::spawn(async move { sub.subscribe_telemetry(&mut srx).await });

    assert!(
        wait_for(
            || lab.subscribe_calls.load(Ordering::SeqCst) > 0,
            Duration::from_secs(10)
        )
        .await
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let requests = lab.requests.lock().await;
    assert_eq!(requests.len(), 1);
    let subs = subscription_list(&requests[0]);
    assert_eq!(
        subs.len(),
        2,
        "explicit plan must contain exactly the requested paths"
    );
    let rendered: Vec<String> = subs
        .iter()
        .map(|s| path_string(s.path.as_ref().unwrap()))
        .collect();
    assert!(rendered.iter().any(|p| p.contains("Ethernet1/1")));
    assert!(rendered.iter().any(|p| p.contains("network-instance")));
    // No capability-derived extras may be appended.
    assert!(
        !rendered
            .iter()
            .any(|p| p.contains("lldp") || p.contains("bfd")),
        "explicit mode must not append automatic defaults: {:?}",
        rendered
    );
    drop(requests);

    let plan = tokio::time::timeout(Duration::from_secs(5), plan_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(plan.state, "submitted");
    assert_eq!(plan.acquisition, "explicit");
    assert_eq!(plan.paths.len(), 2);

    _stx.send(true).unwrap();
    let _ = handle.await;
}

#[tokio::test]
async fn explicit_empty_selection_emits_no_request() {
    let lab = start_lab(vec!["openconfig-interfaces"]).await;
    let (_plan_tx, _plan_rx) = mpsc::channel(8);
    let sub = subscriber(&lab, vec![], AcquisitionMode::Explicit, _plan_tx);
    let (stx, mut srx) = watch::channel(false);
    let handle = tokio::spawn(async move { sub.subscribe_telemetry(&mut srx).await });

    // Wait well past any connect timeout, then verify silence.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 0);
    assert_eq!(lab.capability_calls.load(Ordering::SeqCst), 0);
    assert!(lab.requests.lock().await.is_empty());

    stx.send(true).unwrap();
    let _ = handle.await;
}

#[tokio::test]
async fn invalid_explicit_plan_emits_no_request_and_reports_rejected() {
    let lab = start_lab(vec!["openconfig-interfaces"]).await;
    let (plan_tx, mut plan_rx) = mpsc::channel(8);
    let paths = vec![
        selected("interfaces/interface[name=Ethernet1/1]/state", "SAMPLE"),
        selected("bad[path", "SAMPLE"),
        selected("interfaces/interface[name=x]", "BOGUS_MODE"),
    ];
    let sub = subscriber(&lab, paths, AcquisitionMode::Explicit, plan_tx);
    let (stx, mut srx) = watch::channel(false);
    let handle = tokio::spawn(async move { sub.subscribe_telemetry(&mut srx).await });

    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 0);
    assert_eq!(lab.capability_calls.load(Ordering::SeqCst), 0);

    let plan = tokio::time::timeout(Duration::from_secs(5), plan_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(plan.state, "rejected");
    assert_eq!(plan.acquisition, "explicit");
    assert!(plan.detail.contains("bad[path") || plan.detail.contains("BOGUS_MODE"));

    stx.send(true).unwrap();
    let _ = handle.await;
}

#[tokio::test]
async fn disabled_acquisition_emits_no_request() {
    let lab = start_lab(vec!["openconfig-interfaces"]).await;
    let paths = vec![selected("interfaces/interface[name=Ethernet1/1]", "SAMPLE")];
    let (plan_tx, mut plan_rx) = mpsc::channel(8);
    let sub = subscriber(&lab, paths, AcquisitionMode::Disabled, plan_tx);
    let (stx, mut srx) = watch::channel(false);
    let handle = tokio::spawn(async move { sub.subscribe_telemetry(&mut srx).await });

    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 0);
    assert_eq!(lab.capability_calls.load(Ordering::SeqCst), 0);

    let plan = tokio::time::timeout(Duration::from_secs(5), plan_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(plan.state, "disabled");
    assert_eq!(plan.acquisition, "disabled");

    stx.send(true).unwrap();
    let _ = handle.await;
}

#[tokio::test]
async fn keyed_paths_multiple_keys_and_origins_survive_wire_encoding() {
    let lab = start_lab(vec![]).await;
    let (_plan_tx, _plan_rx) = mpsc::channel(8);
    let paths = vec![
        selected(
            "interfaces/interface[name=xe-0/0/0]/subinterfaces/subinterface[index=0]/state",
            "SAMPLE",
        ),
        SelectedSubscriptionPath {
            path: "foo[name=a][slot=1]/bar".to_string(),
            origin: "openconfig".to_string(),
            provenance: String::new(),
            mode: "ON_CHANGE".to_string(),
            sample_interval_ns: 0,
            rationale: String::new(),
            optional: false,
        },
    ];
    let sub = subscriber(&lab, paths, AcquisitionMode::Explicit, _plan_tx);
    let (_stx, mut srx) = watch::channel(false);
    let handle = tokio::spawn(async move { sub.subscribe_telemetry(&mut srx).await });

    assert!(
        wait_for(
            || lab.subscribe_calls.load(Ordering::SeqCst) > 0,
            Duration::from_secs(10)
        )
        .await
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let requests = lab.requests.lock().await;
    let subs = subscription_list(&requests[0]);
    assert_eq!(subs.len(), 2);

    let p0 = subs[0].path.as_ref().unwrap();
    assert_eq!(p0.elem.len(), 5);
    assert_eq!(p0.elem[1].name, "interface");
    assert_eq!(
        p0.elem[1].key.get("name").map(String::as_str),
        Some("xe-0/0/0")
    );
    assert_eq!(p0.elem[3].key.get("index").map(String::as_str), Some("0"));
    assert_eq!(p0.origin, "openconfig");

    let p1 = subs[1].path.as_ref().unwrap();
    assert_eq!(p1.elem[0].key.len(), 2);
    assert_eq!(p1.elem[0].key.get("name").map(String::as_str), Some("a"));
    assert_eq!(p1.elem[0].key.get("slot").map(String::as_str), Some("1"));
    drop(requests);

    _stx.send(true).unwrap();
    let _ = handle.await;
}

#[tokio::test]
async fn removing_last_path_via_override_cannot_activate_automatic() {
    // Simulate the assignment-time resolution: explicit selection of one
    // path, then a device-scoped Drop override removes it. The resolved set
    // is empty — the subscriber must treat explicit+empty as disabled.
    let mut target = TargetConfig {
        address: "dut".to_string(),
        enabled: true,
        acquisition: AcquisitionMode::Explicit,
        selected_paths: vec![selected("interfaces/interface[name=Ethernet1/1]", "SAMPLE")],
        ..Default::default()
    };
    let overrides = vec![bonsai::registry::PathOverride {
        scope: bonsai::registry::OverrideScope::Device("dut".to_string()),
        path: "interfaces/interface[name=Ethernet1/1]".to_string(),
        action: bonsai::registry::OverrideAction::Drop,
        sample_interval_s: None,
        optional: None,
        created_at_ns: 0,
        created_by: String::new(),
    }];
    let (resolved, audit) = bonsai::discovery::resolve_subscription_paths(&target, &overrides);
    assert!(
        resolved.is_empty(),
        "override must drop the last path: {:?}",
        audit
    );
    target.selected_paths = resolved;

    let lab = start_lab(vec!["openconfig-interfaces"]).await;
    let (plan_tx, mut plan_rx) = mpsc::channel(8);
    let sub = subscriber(&lab, target.selected_paths, target.acquisition, plan_tx);
    let (stx, mut srx) = watch::channel(false);
    let handle = tokio::spawn(async move { sub.subscribe_telemetry(&mut srx).await });

    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(lab.subscribe_calls.load(Ordering::SeqCst), 0);
    assert_eq!(lab.capability_calls.load(Ordering::SeqCst), 0);

    let plan = tokio::time::timeout(Duration::from_secs(5), plan_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(plan.state, "disabled");

    stx.send(true).unwrap();
    let _ = handle.await;
}

#[tokio::test]
async fn restart_preserves_acquisition_intent_and_plan_identity() {
    let lab = start_lab(vec!["openconfig-interfaces"]).await;
    let paths = vec![selected(
        "interfaces/interface[name=Ethernet1/1]/state",
        "SAMPLE",
    )];

    let plan_ids = {
        let (plan_tx, mut plan_rx) = mpsc::channel(8);
        let sub = subscriber(&lab, paths.clone(), AcquisitionMode::Explicit, plan_tx);
        let (_stx, mut srx) = watch::channel(false);
        let handle = tokio::spawn(async move { sub.subscribe_telemetry(&mut srx).await });
        assert!(
            wait_for(
                || lab.subscribe_calls.load(Ordering::SeqCst) > 0,
                Duration::from_secs(10)
            )
            .await
        );
        let plan = tokio::time::timeout(Duration::from_secs(5), plan_rx.recv())
            .await
            .unwrap()
            .unwrap();
        _stx.send(true).unwrap();
        let _ = handle.await;
        plan.plan_id
    };

    // "Restart": identical configuration produces the identical plan_id.
    let (plan_tx2, mut plan_rx2) = mpsc::channel(8);
    let sub2 = subscriber(&lab, paths, AcquisitionMode::Explicit, plan_tx2);
    let (_stx2, mut srx2) = watch::channel(false);
    let handle2 = tokio::spawn(async move { sub2.subscribe_telemetry(&mut srx2).await });
    assert!(
        wait_for(
            || lab.subscribe_calls.load(Ordering::SeqCst) >= 2,
            Duration::from_secs(10)
        )
        .await
    );
    let plan2 = tokio::time::timeout(Duration::from_secs(5), plan_rx2.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        plan2.plan_id, plan_ids,
        "plan identity must be stable across restart"
    );
    assert_eq!(plan2.state, "submitted");

    _stx2.send(true).unwrap();
    let _ = handle2.await;
}

#[tokio::test]
async fn core_to_collector_assignment_preserves_acquisition_intent() {
    // Proto round-trip is the core→collector boundary: intent, paths,
    // origins and provenance must survive unchanged.
    let target = TargetConfig {
        address: "dut:57400".to_string(),
        enabled: true,
        acquisition: AcquisitionMode::Explicit,
        selected_paths: vec![SelectedSubscriptionPath {
            path: "interfaces/interface[name=Ethernet1/1]/state".to_string(),
            origin: "openconfig".to_string(),
            provenance: "override: device-override(dut:57400)".to_string(),
            mode: "SAMPLE".to_string(),
            sample_interval_ns: 5_000_000_000,
            rationale: "test".to_string(),
            optional: true,
        }],
        ..Default::default()
    };
    let proto = bonsai::api::managed_device_from_target(&target);
    assert_eq!(proto.acquisition, "explicit");
    assert_eq!(proto.selected_paths.len(), 1);
    assert_eq!(
        proto.selected_paths[0].provenance,
        "override: device-override(dut:57400)"
    );

    let back = bonsai::api::target_from_managed_device(Some(proto)).unwrap();
    assert_eq!(back.acquisition, AcquisitionMode::Explicit);
    assert_eq!(back.selected_paths.len(), 1);
    let p = &back.selected_paths[0];
    assert_eq!(p.path, "interfaces/interface[name=Ethernet1/1]/state");
    assert_eq!(p.origin, "openconfig");
    assert_eq!(p.provenance, "override: device-override(dut:57400)");
    assert_eq!(p.mode, "SAMPLE");
    assert_eq!(p.sample_interval_ns, 5_000_000_000);
    assert!(p.optional);

    // Disabled and invalid intents must not degrade silently.
    let mut disabled = bonsai::api::managed_device_from_target(&TargetConfig {
        address: "d2".to_string(),
        acquisition: AcquisitionMode::Disabled,
        ..Default::default()
    });
    assert_eq!(disabled.acquisition, "disabled");
    disabled.acquisition = "bogus".to_string();
    assert!(bonsai::api::target_from_managed_device(Some(disabled)).is_err());
}

#[tokio::test]
async fn plan_change_supersedes_stale_paths_without_false_active() {
    // Verifier-level: a new plan must mark paths absent from it as
    // superseded; a disabled plan supersedes everything.
    let dir = tempfile::tempdir().unwrap();
    let store = bonsai::graph::GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        256 * 1024 * 1024,
    )
    .expect("open graph store");
    let db = store.db();
    let bus = InProcessBus::new(64);
    let (plan_tx, plan_rx) = mpsc::channel(8);
    let (_stx, srx) = watch::channel(false);
    let verifier = tokio::spawn(run_subscription_verifier(
        store,
        bus,
        plan_rx,
        Arc::new(bonsai::subscription_sessions::SubscriptionSessionRegistry::new()),
        srx,
        None,
    ));

    let expectation = |path: &str| SubscriptionPathExpectation {
        path: path.to_string(),
        origin: "openconfig".to_string(),
        mode: "SAMPLE".to_string(),
        sample_interval_ns: 10_000_000_000,
    };
    let target = "dut:57400".to_string();

    // Plan 1: two submitted paths.
    plan_tx
        .send(SubscriptionPlan {
            target: target.clone(),
            paths: vec![
                expectation("interfaces/interface[name=Ethernet1/1]/state"),
                expectation("network-instances"),
            ],
            plan_id: "plan-1".to_string(),
            session_id: String::new(),
            state: "submitted".to_string(),
            acquisition: "explicit".to_string(),
            detail: String::new(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Plan 2: only the first path remains — second must be superseded.
    plan_tx
        .send(SubscriptionPlan {
            target: target.clone(),
            paths: vec![expectation("interfaces/interface[name=Ethernet1/1]/state")],
            plan_id: "plan-2".to_string(),
            session_id: String::new(),
            state: "submitted".to_string(),
            acquisition: "explicit".to_string(),
            detail: String::new(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Disabled plan: everything superseded.
    plan_tx
        .send(SubscriptionPlan {
            target: target.clone(),
            paths: vec![],
            plan_id: "plan-3".to_string(),
            session_id: String::new(),
            state: "disabled".to_string(),
            acquisition: "disabled".to_string(),
            detail: "acquisition disabled".to_string(),
        })
        .await
        .unwrap();
    // Bounded deterministic wait: the disabled plan's persisted
    // supersede is asynchronous relative to the channel send — poll
    // until every row reports superseded instead of assuming the
    // 300ms sleep covered it under parallel test load.
    let mut statuses;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        statuses = query_subscription_statuses(&db).await;
        if !statuses.is_empty() && statuses.iter().all(|(_, _, s)| s == "superseded") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "disabled plan must supersede all tracked paths: {:?}",
            statuses
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(statuses.iter().any(|(t, _, _)| t == &target));

    _stx.send(true).unwrap();
    let _ = verifier.await;
}

/// Returns (device_address, path, status) rows.
async fn query_subscription_statuses(db: &Arc<lbug::Database>) -> Vec<(String, String, String)> {
    use lbug::Connection;
    let db = Arc::clone(db);
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).unwrap();
        let mut out = Vec::new();
        let rows = conn
            .query(
                "MATCH (s:SubscriptionStatus) \
                 RETURN s.device_address, s.path, s.status",
            )
            .unwrap();
        for row in rows {
            let get = |i: usize| -> String {
                match &row[i] {
                    lbug::Value::String(v) => v.clone(),
                    other => other.to_string(),
                }
            };
            out.push((get(0), get(1), get(2)));
        }
        out
    })
    .await
    .unwrap()
}

/// Live plan supersession: an active Subscribe stream must be actually
/// cancelled when the acquisition plan is withdrawn, an explicit empty
/// selection must emit no further Subscribe RPC, and a changed selection
/// must produce exactly one new request carrying only the new paths.
#[tokio::test]
async fn plan_change_cancels_stream_and_empty_emits_no_request() {
    let lab = start_lab(vec!["openconfig-interfaces"]).await;

    // Phase 1 — explicit selection A is live.
    let (plan_tx, _plan_rx) = mpsc::channel(8);
    let sub_a = subscriber(
        &lab,
        vec![selected(
            "interfaces/interface[name=Ethernet1/1]/state/counters",
            "ON_CHANGE",
        )],
        AcquisitionMode::Explicit,
        plan_tx,
    );
    let (_stx_a, mut srx_a) = watch::channel(false);
    let handle_a = tokio::spawn(async move { sub_a.subscribe_telemetry(&mut srx_a).await });
    assert!(
        wait_for(
            || lab.subscribe_calls.load(Ordering::SeqCst) == 1,
            Duration::from_secs(10)
        )
        .await,
        "expected initial Subscribe RPC"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Phase 2 — withdraw plan A: the previous stream must be cancelled
    // for real (server drops the response stream), not just idle.
    assert_eq!(lab.active_streams.load(Ordering::SeqCst), 1);
    _stx_a.send(true).unwrap();
    let _ = handle_a.await;
    assert!(
        wait_for(
            || lab.active_streams.load(Ordering::SeqCst) == 0,
            Duration::from_secs(5)
        )
        .await,
        "previous Subscribe stream was not cancelled"
    );

    // Phase 3 — explicit empty selection: disabled plan, no new RPC.
    let (plan_tx_b, mut plan_rx_b) = mpsc::channel(8);
    let sub_b = subscriber(&lab, vec![], AcquisitionMode::Explicit, plan_tx_b);
    let (_stx_b, mut srx_b) = watch::channel(false);
    let handle_b = tokio::spawn(async move { sub_b.subscribe_telemetry(&mut srx_b).await });
    let plan_b = tokio::time::timeout(Duration::from_secs(5), plan_rx_b.recv())
        .await
        .expect("disabled plan published")
        .expect("plan channel open");
    assert_eq!(plan_b.state, "disabled");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        lab.subscribe_calls.load(Ordering::SeqCst),
        1,
        "explicit empty selection must not emit a Subscribe RPC"
    );
    _stx_b.send(true).unwrap();
    let _ = handle_b.await;

    // Phase 4 — changed explicit selection: exactly one new request with
    // only the new path; the old plan's stream stays cancelled.
    let (plan_tx_c, _plan_rx_c) = mpsc::channel(8);
    let sub_c = subscriber(
        &lab,
        vec![selected(
            "network-instances/network-instance[name=default]/protocols",
            "SAMPLE",
        )],
        AcquisitionMode::Explicit,
        plan_tx_c,
    );
    let (_stx_c, mut srx_c) = watch::channel(false);
    let handle_c = tokio::spawn(async move { sub_c.subscribe_telemetry(&mut srx_c).await });
    assert!(
        wait_for(
            || lab.subscribe_calls.load(Ordering::SeqCst) == 2,
            Duration::from_secs(10)
        )
        .await,
        "expected a second Subscribe RPC for the changed plan"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let requests = lab.requests.lock().await;
    assert_eq!(requests.len(), 2, "exactly two Subscribe requests expected");
    let subs = subscription_list(&requests[1]);
    assert_eq!(subs.len(), 1, "new plan must carry only its own path");
    let emitted = path_string(subs[0].path.as_ref().unwrap());
    assert!(
        emitted.contains("network-instances"),
        "unexpected path in new plan: {emitted}"
    );
    assert!(
        !emitted.contains("Ethernet1/1"),
        "obsolete path leaked into new plan: {emitted}"
    );
    drop(requests);
    assert_eq!(
        lab.active_streams.load(Ordering::SeqCst),
        1,
        "exactly the new plan's stream may be live"
    );

    _stx_c.send(true).unwrap();
    let _ = handle_c.await;
    assert!(
        wait_for(
            || lab.active_streams.load(Ordering::SeqCst) == 0,
            Duration::from_secs(5)
        )
        .await,
        "final stream was not cancelled"
    );
}

#[tokio::test]
async fn stale_session_observation_never_projects_coverage() {
    // M5.2B B2d regression: a superseded session's observation — even one
    // whose structured gNMI path exactly covers a tracked expectation —
    // must never flip subscription status to "observed". Only the
    // current registered (plan_id, session_id) pair grants coverage, and
    // only through exact structured path evidence.
    use bonsai::subscription_sessions::{
        SessionLifecycleEvent, SubscriptionSessionRegistry, state,
    };
    use bonsai::telemetry::TelemetryUpdate;

    let dir = tempfile::tempdir().unwrap();
    let store = bonsai::graph::GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        256 * 1024 * 1024,
    )
    .expect("open graph store");
    let db = store.db();
    let bus = InProcessBus::new(64);
    let publish_bus = bus.clone();
    let (plan_tx, plan_rx) = mpsc::channel(8);
    let (_stx, srx) = watch::channel(false);
    let sessions = Arc::new(SubscriptionSessionRegistry::new());
    let verifier_sessions = Arc::clone(&sessions);
    let verifier = tokio::spawn(run_subscription_verifier(
        store,
        bus,
        plan_rx,
        verifier_sessions,
        srx,
        None,
    ));

    let target = "dut:57400".to_string();
    let expectation = SubscriptionPathExpectation {
        path: "interfaces/interface[name=Ethernet1/1]/state/counters".to_string(),
        origin: "openconfig".to_string(),
        mode: "SAMPLE".to_string(),
        sample_interval_ns: 10_000_000_000,
    };
    plan_tx
        .send(SubscriptionPlan {
            target: target.clone(),
            paths: vec![expectation],
            plan_id: "plan-1".to_string(),
            session_id: "sess-b".to_string(),
            state: "submitted".to_string(),
            acquisition: "explicit".to_string(),
            detail: String::new(),
        })
        .await
        .unwrap();

    // Register sess-a synchronized, then sess-b — sess-b supersedes.
    for (session_id, seq_state) in [
        ("sess-a", [(state::SUBMITTED, 1), (state::SYNCHRONIZED, 2)]),
        ("sess-b", [(state::SUBMITTED, 1), (state::SYNCHRONIZED, 2)]),
    ] {
        for (st, seq) in seq_state {
            sessions.apply(
                &SessionLifecycleEvent {
                    collector_id: String::new(),
                    target: target.clone(),
                    plan_id: "plan-1".to_string(),
                    session_id: session_id.to_string(),
                    state: st.to_string(),
                    transition_seq: seq,
                    authority_epoch: 0,
                    assignment_generation: 0,
                    authority_instance: 0,
                    collector_session_generation: 0,
                    execution_generation: if session_id == "sess-a" { 1 } else { 2 },
                    occurred_at_ns: 0,
                    detail: String::new(),
                    submitted_paths: Vec::new(),
                    acquisition: String::new(),
                },
                true,
            );
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let covering_path = bonsai::gnmi_path::parse_path(
        "openconfig",
        "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
    )
    .unwrap();
    let stale_update = TelemetryUpdate {
        plan_id: "plan-1".to_string(),
        session_id: "sess-a".to_string(),
        source_protocol: "gnmi".to_string(),
        gnmi_path: vec![covering_path.clone()],
        is_delete: false,
        collector_id: String::new(),
        target: target.clone(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: 1,
        path: "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets".to_string(),
        value: serde_json::json!({"in-octets": 42}),
    };
    publish_bus.publish(stale_update);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let statuses = query_subscription_statuses(&db).await;
    assert!(
        statuses.iter().all(|(_, _, s)| s != "observed"),
        "superseded session observation must not project coverage: {:?}",
        statuses
    );

    // The current session's identical-path observation DOES cover.
    let mut current = TelemetryUpdate {
        plan_id: "plan-1".to_string(),
        session_id: "sess-b".to_string(),
        source_protocol: "gnmi".to_string(),
        gnmi_path: vec![covering_path],
        is_delete: false,
        collector_id: String::new(),
        target: target.clone(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: 2,
        path: "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets".to_string(),
        value: serde_json::json!({"in-octets": 43}),
    };
    publish_bus.publish(current.clone());
    // A same-session observation on an unrelated keyed instance must not
    // satisfy this expectation even though the session is current.
    current.gnmi_path = bonsai::gnmi_path::parse_path(
        "openconfig",
        "interfaces/interface[name=Ethernet1/2]/state/counters/in-octets",
    )
    .map(|p| vec![p])
    .unwrap_or_default();
    publish_bus.publish(current);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let statuses = query_subscription_statuses(&db).await;
    assert!(
        statuses.iter().any(|(_, _, s)| s == "observed"),
        "current session covering observation must project coverage: {:?}",
        statuses
    );

    _stx.send(true).unwrap();
    let _ = verifier.await;
}

/// M5.2B B3: delete/tombstone evidence invalidates coverage for the
/// current session only, and an observed SAMPLE path goes stale after
/// its freshness bound while quiet ON_CHANGE stays valid.
#[tokio::test]
async fn delete_evidence_and_sample_freshness_invalidate_coverage() {
    use bonsai::subscription_sessions::{
        SessionLifecycleEvent, SubscriptionSessionRegistry, state,
    };
    use bonsai::subscription_status::run_subscription_verifier_with_windows;
    use bonsai::telemetry::TelemetryUpdate;

    let dir = tempfile::tempdir().unwrap();
    let store = bonsai::graph::GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        256 * 1024 * 1024,
    )
    .expect("open graph store");
    let db = store.db();
    let bus = InProcessBus::new(64);
    let publish_bus = bus.clone();
    let (plan_tx, plan_rx) = mpsc::channel(8);
    let (_stx, srx) = watch::channel(false);
    let sessions = Arc::new(SubscriptionSessionRegistry::new());
    let verifier_sessions = Arc::clone(&sessions);
    // Tight windows so the test exercises stale transitions quickly:
    // 50ms sweep tick, 200ms pending/freshness floor.
    let verifier = tokio::spawn(run_subscription_verifier_with_windows(
        store,
        bus,
        plan_rx,
        verifier_sessions,
        srx,
        Duration::from_millis(50),
        Duration::from_millis(200),
        None,
    ));

    let target = "dut:57400".to_string();
    let leaf = "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets";
    plan_tx
        .send(SubscriptionPlan {
            target: target.clone(),
            paths: vec![SubscriptionPathExpectation {
                path: "interfaces/interface[name=Ethernet1/1]/state/counters".to_string(),
                origin: "openconfig".to_string(),
                mode: "SAMPLE".to_string(),
                sample_interval_ns: 0,
            }],
            plan_id: "plan-1".to_string(),
            session_id: "sess-b".to_string(),
            state: "submitted".to_string(),
            acquisition: "explicit".to_string(),
            detail: String::new(),
        })
        .await
        .unwrap();

    // sess-a synchronized then superseded by sess-b.
    for (session_id, seq_state) in [
        ("sess-a", [(state::SUBMITTED, 1), (state::SYNCHRONIZED, 2)]),
        ("sess-b", [(state::SUBMITTED, 1), (state::SYNCHRONIZED, 2)]),
    ] {
        for (st, seq) in seq_state {
            sessions.apply(
                &SessionLifecycleEvent {
                    collector_id: String::new(),
                    target: target.clone(),
                    plan_id: "plan-1".to_string(),
                    session_id: session_id.to_string(),
                    state: st.to_string(),
                    transition_seq: seq,
                    authority_epoch: 0,
                    assignment_generation: 0,
                    authority_instance: 0,
                    collector_session_generation: 0,
                    execution_generation: if session_id == "sess-a" { 1 } else { 2 },
                    occurred_at_ns: 0,
                    detail: String::new(),
                    submitted_paths: Vec::new(),
                    acquisition: String::new(),
                },
                true,
            );
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mk =
        |session_id: &str, is_delete: bool, ts: i64| mk_at(session_id, is_delete, ts, leaf, leaf);
    // `gnmi`/`path_str` may differ: a delete notification can carry a
    // different (ancestor or descendant) structured path than the
    // covering leaf observation.
    fn mk_at(
        session_id: &str,
        is_delete: bool,
        ts: i64,
        gnmi: &str,
        path_str: &str,
    ) -> TelemetryUpdate {
        TelemetryUpdate {
            plan_id: "plan-1".to_string(),
            session_id: session_id.to_string(),
            source_protocol: "gnmi".to_string(),
            gnmi_path: bonsai::gnmi_path::parse_path("openconfig", gnmi)
                .map(|p| vec![p])
                .unwrap_or_default(),
            is_delete,
            collector_id: String::new(),
            target: "dut:57400".to_string(),
            vendor: String::new(),
            hostname: String::new(),
            role: String::new(),
            site: String::new(),
            timestamp_ns: ts,
            path: path_str.to_string(),
            value: if is_delete {
                serde_json::Value::Null
            } else {
                serde_json::json!({"in-octets": ts})
            },
        }
    }

    // Wait until the plan is actually registered (pending row
    // persisted) — under parallel gate load register_plan can lag the
    // fixed sleeps, and an update consumed before tracking is dropped.
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let rows = query_subscription_statuses(&db).await;
            if rows.iter().any(|(t, _, s)| t == &target && s == "pending") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "plan never registered as pending: {rows:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    // 1. Current-session observation -> observed.
    publish_bus.publish(mk("sess-b", false, 1));
    wait_for_status(&db, &target, "observed").await;

    // 2. Current-session delete of the expectation path itself ->
    // deleted (complete absence evidence).
    publish_bus.publish(mk_at(
        "sess-b",
        true,
        2,
        "interfaces/interface[name=Ethernet1/1]/state/counters",
        "interfaces/interface[name=Ethernet1/1]/state/counters",
    ));
    wait_for_status(&db, &target, "deleted").await;

    // 3. Stale-session observation and delete are both fenced.
    publish_bus.publish(mk("sess-a", false, 3));
    publish_bus.publish(mk("sess-a", true, 4));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let statuses = query_subscription_statuses(&db).await;
    assert!(
        statuses.iter().all(|(_, _, s)| s == "deleted"),
        "stale session must not revive or alter coverage: {:?}",
        statuses
    );

    // 4. Fresh current-session observation revives coverage.
    publish_bus.publish(mk("sess-b", false, 5));
    wait_for_status(&db, &target, "observed").await;

    // 5. SAMPLE freshness: quiet past the bound -> stale. Poll: the
    // stale transition depends on verifier scheduling, not wall time.
    wait_for_status(&db, &target, "stale").await;

    // 6. A new observation clears stale back to observed.
    publish_bus.publish(mk("sess-b", false, 6));
    wait_for_status(&db, &target, "observed").await;

    _stx.send(true).unwrap();
    let _ = verifier.await;
}

// ── M5.2B ordering/fencing/persistence regressions ──────────────────────────

fn lifecycle_ev(
    target: &str,
    plan_id: &str,
    session_id: &str,
    st: &str,
    seq: u64,
    exec_gen: u64,
) -> bonsai::subscription_sessions::SessionLifecycleEvent {
    bonsai::subscription_sessions::SessionLifecycleEvent {
        collector_id: String::new(),
        target: target.to_string(),
        plan_id: plan_id.to_string(),
        session_id: session_id.to_string(),
        state: st.to_string(),
        transition_seq: seq,
        authority_epoch: 0,
        assignment_generation: 0,
        authority_instance: 0,
        collector_session_generation: 0,
        execution_generation: exec_gen,
        occurred_at_ns: 0,
        detail: String::new(),
        submitted_paths: Vec::new(),
        acquisition: String::new(),
    }
}

fn sync_session(
    sessions: &bonsai::subscription_sessions::SubscriptionSessionRegistry,
    target: &str,
    plan_id: &str,
    session_id: &str,
    exec_gen: u64,
) {
    use bonsai::subscription_sessions::state;
    sessions.apply(
        &lifecycle_ev(target, plan_id, session_id, state::SUBMITTED, 1, exec_gen),
        true,
    );
    sessions.apply(
        &lifecycle_ev(
            target,
            plan_id,
            session_id,
            state::SYNCHRONIZING,
            2,
            exec_gen,
        ),
        true,
    );
    sessions.apply(
        &lifecycle_ev(
            target,
            plan_id,
            session_id,
            state::SYNCHRONIZED,
            3,
            exec_gen,
        ),
        true,
    );
}

/// Bounded wait until the verifier has registered `target`'s plan as a
/// pending row — plan_rx processing is asynchronous and can lag fixed
/// sleeps under parallel gate load.
async fn wait_pending_row(db: &Arc<lbug::Database>, target: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let rows = query_subscription_statuses(db).await;
        if rows.iter().any(|(t, _, s)| t == target && s == "pending") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "plan never registered as pending for {target}: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn gnmi_obs(
    target: &str,
    plan_id: &str,
    session_id: &str,
    protocol: &str,
    path: &str,
    gnmi: &str,
    is_delete: bool,
) -> bonsai::telemetry::TelemetryUpdate {
    bonsai::telemetry::TelemetryUpdate {
        plan_id: plan_id.to_string(),
        session_id: session_id.to_string(),
        source_protocol: protocol.to_string(),
        gnmi_path: bonsai::gnmi_path::parse_path("openconfig", gnmi)
            .map(|p| vec![p])
            .unwrap_or_default(),
        is_delete,
        collector_id: String::new(),
        target: target.to_string(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: 1,
        path: path.to_string(),
        value: serde_json::json!({"leaf": 1}),
    }
}

/// Bounded wait until `target` has a persisted row in `want` status.
/// Fixed sleeps are unsafe here: under parallel gate load the verifier
/// can lag, and a single-shot query can observe an intermediate state
/// (e.g. subscribed_but_silent) that a queued update will still
/// correctly overwrite on the next drain.
async fn wait_for_status(
    db: &Arc<lbug::Database>,
    target: &str,
    want: &str,
) -> Vec<(String, String, String)> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let rows = query_subscription_statuses(db).await;
        if rows.iter().any(|(t, _, s)| t == target && s == want) {
            return rows;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for status {want}: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// E: an observation that arrives BEFORE its session's synchronized
/// lifecycle transition is buffered (bounded) and released once the
/// exact (plan_id, session_id) becomes current — initial-sync updates
/// are never silently dropped into a false subscribed_but_silent.
#[tokio::test]
async fn early_sync_observation_is_buffered_until_session_current() {
    use bonsai::subscription_status::run_subscription_verifier_with_windows;

    let dir = tempfile::tempdir().unwrap();
    let store = bonsai::graph::GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        256 * 1024 * 1024,
    )
    .expect("open graph store");
    let db = store.db();
    let bus = InProcessBus::new(64);
    let publish_bus = bus.clone();
    let (plan_tx, plan_rx) = mpsc::channel(8);
    let (_stx, srx) = watch::channel(false);
    let sessions = Arc::new(bonsai::subscription_sessions::SubscriptionSessionRegistry::new());
    let verifier = tokio::spawn(run_subscription_verifier_with_windows(
        store,
        bus,
        plan_rx,
        Arc::clone(&sessions),
        srx,
        Duration::from_millis(50),
        Duration::from_millis(10_000),
        None,
    ));

    let target = "dut:57400".to_string();
    plan_tx
        .send(SubscriptionPlan {
            target: target.clone(),
            paths: vec![SubscriptionPathExpectation {
                path: "interfaces/interface[name=Ethernet1/1]/state/counters".to_string(),
                origin: "openconfig".to_string(),
                mode: "ON_CHANGE".to_string(),
                sample_interval_ns: 0,
            }],
            plan_id: "plan-1".to_string(),
            session_id: String::new(),
            state: "submitted".to_string(),
            acquisition: "explicit".to_string(),
            detail: String::new(),
        })
        .await
        .unwrap();
    wait_pending_row(&db, &target).await;
    // The authoritative submitted manifest binds the session identity —
    // production always forwards it (api.rs on submitted/synchronized).
    plan_tx
        .send(SubscriptionPlan {
            target: target.clone(),
            paths: vec![SubscriptionPathExpectation {
                path: "interfaces/interface[name=Ethernet1/1]/state/counters".to_string(),
                origin: "openconfig".to_string(),
                mode: "ON_CHANGE".to_string(),
                sample_interval_ns: 0,
            }],
            plan_id: "plan-1".to_string(),
            session_id: "sess-a".to_string(),
            state: "submitted".to_string(),
            acquisition: "explicit".to_string(),
            detail: String::new(),
        })
        .await
        .unwrap();

    // Telemetry arrives BEFORE the synchronized lifecycle registration —
    // exactly the production initial-sync ordering race.
    publish_bus.publish(gnmi_obs(
        &target,
        "plan-1",
        "sess-a",
        "gnmi",
        "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
        "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
        false,
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let statuses = query_subscription_statuses(&db).await;
    assert!(
        statuses.iter().all(|(_, _, s)| s != "observed"),
        "pre-registration observation must not be authoritative yet: {:?}",
        statuses
    );

    // Session becomes synchronized; the buffered observation must now
    // establish coverage on the next sweep.
    sync_session(&sessions, &target, "plan-1", "sess-a", 1);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut observed = false;
    while std::time::Instant::now() < deadline {
        let statuses = query_subscription_statuses(&db).await;
        if statuses.iter().any(|(_, _, s)| s == "observed") {
            observed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(observed, "buffered early-sync observation never released");

    _stx.send(true).unwrap();
    let _ = verifier.await;
}

/// E negative: observations stamped with a session identity that is
/// never authoritatively registered can never establish coverage, no
/// matter how often they arrive or how exact their path evidence is.
#[tokio::test]
async fn unregistered_session_observation_never_covers() {
    use bonsai::subscription_status::run_subscription_verifier_with_windows;

    let dir = tempfile::tempdir().unwrap();
    let store = bonsai::graph::GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        256 * 1024 * 1024,
    )
    .expect("open graph store");
    let db = store.db();
    let bus = InProcessBus::new(64);
    let publish_bus = bus.clone();
    let (plan_tx, plan_rx) = mpsc::channel(8);
    let (_stx, srx) = watch::channel(false);
    let sessions = Arc::new(bonsai::subscription_sessions::SubscriptionSessionRegistry::new());
    let verifier = tokio::spawn(run_subscription_verifier_with_windows(
        store,
        bus,
        plan_rx,
        Arc::clone(&sessions),
        srx,
        Duration::from_millis(50),
        Duration::from_millis(10_000),
        None,
    ));

    let target = "dut:57400".to_string();
    plan_tx
        .send(SubscriptionPlan {
            target: target.clone(),
            paths: vec![SubscriptionPathExpectation {
                path: "interfaces/interface[name=Ethernet1/1]/state/counters".to_string(),
                origin: "openconfig".to_string(),
                mode: "ON_CHANGE".to_string(),
                sample_interval_ns: 0,
            }],
            plan_id: "plan-1".to_string(),
            session_id: String::new(),
            state: "submitted".to_string(),
            acquisition: "explicit".to_string(),
            detail: String::new(),
        })
        .await
        .unwrap();
    wait_pending_row(&db, &target).await;

    // A different, legitimately registered session exists — the ghost
    // session's evidence must still never cover.
    sync_session(&sessions, &target, "plan-1", "sess-real", 1);
    for _ in 0..3 {
        publish_bus.publish(gnmi_obs(
            &target,
            "plan-1",
            "sess-ghost",
            "gnmi",
            "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
            "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
            false,
        ));
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    let statuses = query_subscription_statuses(&db).await;
    assert!(
        statuses.iter().all(|(_, _, s)| s != "observed"),
        "unregistered session evidence must never cover: {:?}",
        statuses
    );

    _stx.send(true).unwrap();
    let _ = verifier.await;
}

/// PendingObservations authority isolation: buffered early-sync
/// evidence is bound to the exact verified (plan_id, session_id) the
/// authenticated lifecycle stream authorized. A foreign collector
/// claiming the same identifiers without verified authority, a
/// different authorized session for the same plan, and withdrawal
/// before release can never turn buffered evidence into coverage.
#[tokio::test]
async fn pending_evidence_requires_the_authorized_session_identity() {
    use bonsai::subscription_sessions::state;
    use bonsai::subscription_status::run_subscription_verifier_with_windows;

    let dir = tempfile::tempdir().unwrap();
    let store = bonsai::graph::GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        256 * 1024 * 1024,
    )
    .expect("open graph store");
    let db = store.db();
    let bus = InProcessBus::new(64);
    let publish_bus = bus.clone();
    let (plan_tx, plan_rx) = mpsc::channel(8);
    let (_stx, srx) = watch::channel(false);
    let sessions = Arc::new(bonsai::subscription_sessions::SubscriptionSessionRegistry::new());
    let verifier = tokio::spawn(run_subscription_verifier_with_windows(
        store,
        bus,
        plan_rx,
        Arc::clone(&sessions),
        srx,
        Duration::from_millis(50),
        Duration::from_millis(10_000),
        None,
    ));

    let target = "dut:57400".to_string();
    plan_tx
        .send(SubscriptionPlan {
            target: target.clone(),
            paths: vec![SubscriptionPathExpectation {
                path: "interfaces/interface[name=Ethernet1/1]/state/counters".to_string(),
                origin: "openconfig".to_string(),
                mode: "ON_CHANGE".to_string(),
                sample_interval_ns: 0,
            }],
            plan_id: "plan-1".to_string(),
            session_id: String::new(),
            state: "submitted".to_string(),
            acquisition: "explicit".to_string(),
            detail: String::new(),
        })
        .await
        .unwrap();
    wait_pending_row(&db, &target).await;

    // Buffer evidence for sess-old before any registration.
    publish_bus.publish(gnmi_obs(
        &target,
        "plan-1",
        "sess-old",
        "gnmi",
        "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
        "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
        false,
    ));
    tokio::time::sleep(Duration::from_millis(150)).await;

    // A foreign collector claims the SAME plan/session identifiers, but
    // unverified lifecycle can never authorize coverage (in production
    // `verified` is only set after authenticated authority validation).
    let mut foreign = lifecycle_ev(&target, "plan-1", "sess-old", state::SUBMITTED, 1, 2);
    foreign.collector_id = "other-collector".to_string();
    sessions.apply(&foreign, false);

    // The legitimately authorized session is a DIFFERENT session id.
    sync_session(&sessions, &target, "plan-1", "sess-new", 1);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let statuses = query_subscription_statuses(&db).await;
    assert!(
        statuses.iter().all(|(_, _, s)| s != "observed"),
        "foreign claim or sibling session must not release buffered evidence: {:?}",
        statuses
    );

    // Withdrawal before release: the authorized session disconnects and
    // the pending bucket's session goes terminal — evidence is dropped.
    sessions.apply(
        &lifecycle_ev(&target, "plan-1", "sess-new", state::DISCONNECTED, 4, 1),
        true,
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let statuses = query_subscription_statuses(&db).await;
    assert!(
        statuses.iter().all(|(_, _, s)| s != "observed"),
        "withdrawn session must not leave coverage: {:?}",
        statuses
    );

    _stx.send(true).unwrap();
    let _ = verifier.await;
}

/// D: acquisition coverage is independent of semantic classification.
/// A gNMI observation whose path matches NO Bonsai classifier still
/// establishes subscription coverage (gnmi_path evidence only); and a
/// non-gNMI producer with identical path/session stamps never does.
#[tokio::test]
async fn coverage_is_acquisition_not_classification() {
    use bonsai::subscription_status::run_subscription_verifier_with_windows;

    let dir = tempfile::tempdir().unwrap();
    let store = bonsai::graph::GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        256 * 1024 * 1024,
    )
    .expect("open graph store");
    let db = store.db();
    let bus = InProcessBus::new(64);
    let publish_bus = bus.clone();
    let (plan_tx, plan_rx) = mpsc::channel(8);
    let (_stx, srx) = watch::channel(false);
    let sessions = Arc::new(bonsai::subscription_sessions::SubscriptionSessionRegistry::new());
    let verifier = tokio::spawn(run_subscription_verifier_with_windows(
        store,
        bus,
        plan_rx,
        Arc::clone(&sessions),
        srx,
        Duration::from_millis(50),
        Duration::from_millis(10_000),
        None,
    ));

    let target = "dut:57400".to_string();
    // An expectation on a path no classifier recognizes.
    plan_tx
        .send(SubscriptionPlan {
            target: target.clone(),
            paths: vec![SubscriptionPathExpectation {
                path: "foo/bar[name=x]/baz".to_string(),
                origin: "openconfig".to_string(),
                mode: "ON_CHANGE".to_string(),
                sample_interval_ns: 0,
            }],
            plan_id: "plan-1".to_string(),
            session_id: String::new(),
            state: "submitted".to_string(),
            acquisition: "explicit".to_string(),
            detail: String::new(),
        })
        .await
        .unwrap();
    wait_pending_row(&db, &target).await;
    // Session-bound manifest — the authoritative execution identity.
    plan_tx
        .send(SubscriptionPlan {
            target: target.clone(),
            paths: vec![SubscriptionPathExpectation {
                path: "foo/bar[name=x]/baz".to_string(),
                origin: "openconfig".to_string(),
                mode: "ON_CHANGE".to_string(),
                sample_interval_ns: 0,
            }],
            plan_id: "plan-1".to_string(),
            session_id: "sess-a".to_string(),
            state: "submitted".to_string(),
            acquisition: "explicit".to_string(),
            detail: String::new(),
        })
        .await
        .unwrap();
    sync_session(&sessions, &target, "plan-1", "sess-a", 1);

    // Negative first: identical stamps from a non-gNMI producer.
    for proto in ["bmp", "syslog", "snmp", "sflow", "otlp", "netflow"] {
        publish_bus.publish(gnmi_obs(
            &target,
            "plan-1",
            "sess-a",
            proto,
            "foo/bar[name=x]/baz/qux",
            "foo/bar[name=x]/baz/qux",
            false,
        ));
    }
    // And a legacy/empty-provenance gNMI update.
    let mut legacy = gnmi_obs(
        &target,
        "",
        "",
        "gnmi",
        "foo/bar[name=x]/baz/qux",
        "foo/bar[name=x]/baz/qux",
        false,
    );
    legacy.source_protocol = "gnmi".to_string();
    publish_bus.publish(legacy);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let statuses = query_subscription_statuses(&db).await;
    assert!(
        statuses.iter().all(|(_, _, s)| s != "observed"),
        "non-gNMI/legacy evidence must not cover: {:?}",
        statuses
    );

    // Positive: the same session's genuine gNMI observation on the
    // unclassified path establishes coverage.
    publish_bus.publish(gnmi_obs(
        &target,
        "plan-1",
        "sess-a",
        "gnmi",
        "foo/bar[name=x]/baz/qux",
        "foo/bar[name=x]/baz/qux",
        false,
    ));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut observed = false;
    while std::time::Instant::now() < deadline {
        let statuses = query_subscription_statuses(&db).await;
        if statuses.iter().any(|(_, _, s)| s == "observed") {
            observed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        observed,
        "unclassified gNMI leaf must still establish coverage"
    );

    _stx.send(true).unwrap();
    let _ = verifier.await;
}

/// G: a failed status persistence must not be claimed in memory — the
/// transition is retried and eventually lands durably, and no success
/// event is published for the failed write.
#[tokio::test]
async fn failed_status_write_is_retried_not_claimed() {
    use bonsai::store::BonsaiStore;
    use bonsai::subscription_status::run_subscription_verifier_with_windows;
    use std::sync::atomic::AtomicBool;

    /// Delegates to GraphStore but fails the next N subscription-status
    /// writes — models a transient LadybugDB write failure.
    struct FlakyStore {
        inner: Arc<bonsai::graph::GraphStore>,
        fail_writes: Arc<AtomicBool>,
    }
    #[tonic::async_trait]
    impl BonsaiStore for FlakyStore {
        fn db(&self) -> Arc<lbug::Database> {
            BonsaiStore::db(&*self.inner)
        }
        fn write_lock(&self) -> Arc<std::sync::Mutex<()>> {
            BonsaiStore::write_lock(&*self.inner)
        }
        fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<bonsai::graph::BonsaiEvent> {
            BonsaiStore::subscribe_events(&*self.inner)
        }
        async fn write(&self, u: bonsai::telemetry::TelemetryUpdate) -> anyhow::Result<()> {
            BonsaiStore::write(&*self.inner, u).await
        }
        async fn write_detection(
            &self,
            a: String,
            b: String,
            c: String,
            d: String,
            e: String,
            f: i64,
            g: i64,
            h: String,
            i: Vec<String>,
        ) -> anyhow::Result<String> {
            BonsaiStore::write_detection(&*self.inner, a, b, c, d, e, f, g, h, i).await
        }
        async fn write_remediation(
            &self,
            a: String,
            b: String,
            c: String,
            d: String,
            e: i64,
            f: i64,
        ) -> anyhow::Result<String> {
            BonsaiStore::write_remediation(&*self.inner, a, b, c, d, e, f).await
        }
        async fn sync_sites_from_targets(&self, t: Vec<TargetConfig>) -> anyhow::Result<()> {
            BonsaiStore::sync_sites_from_targets(&*self.inner, t).await
        }
        async fn list_sites(&self) -> anyhow::Result<Vec<bonsai::graph::SiteRecord>> {
            BonsaiStore::list_sites(&*self.inner).await
        }
        async fn upsert_site(
            &self,
            s: bonsai::graph::SiteRecord,
        ) -> anyhow::Result<bonsai::graph::SiteRecord> {
            BonsaiStore::upsert_site(&*self.inner, s).await
        }
        async fn write_subscription_status(
            &self,
            s: bonsai::graph::SubscriptionStatusWrite,
        ) -> anyhow::Result<()> {
            // Fail every status write while armed — a single-shot
            // failure would be legitimately healed by the bounded
            // durable-retry queue before the assertion runs.
            if self.fail_writes.load(Ordering::SeqCst) {
                anyhow::bail!("injected subscription-status write failure");
            }
            BonsaiStore::write_subscription_status(&*self.inner, s).await
        }
        async fn supersede_subscription_statuses_outside_plan(
            &self,
            d: &str,
            k: &std::collections::HashSet<(String, String, String, i64)>,
        ) -> anyhow::Result<()> {
            BonsaiStore::supersede_subscription_statuses_outside_plan(&*self.inner, d, k).await
        }
        fn publish_event(&self, e: bonsai::graph::BonsaiEvent) {
            BonsaiStore::publish_event(&*self.inner, e)
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let graph = Arc::new(
        bonsai::graph::GraphStore::open(
            dir.path().join("graph.db").to_str().unwrap(),
            256 * 1024 * 1024,
        )
        .expect("open graph store"),
    );
    let db = graph.db();
    let bus = InProcessBus::new(64);
    let publish_bus = bus.clone();
    let fail_writes = Arc::new(AtomicBool::new(false));
    let (plan_tx, plan_rx) = mpsc::channel(8);
    let (_stx, srx) = watch::channel(false);
    let sessions = Arc::new(bonsai::subscription_sessions::SubscriptionSessionRegistry::new());
    let verifier = tokio::spawn(run_subscription_verifier_with_windows(
        FlakyStore {
            inner: Arc::clone(&graph),
            fail_writes: Arc::clone(&fail_writes),
        },
        bus,
        plan_rx,
        Arc::clone(&sessions),
        srx,
        Duration::from_millis(50),
        Duration::from_millis(10_000),
        None,
    ));

    let target = "dut:57400".to_string();
    plan_tx
        .send(SubscriptionPlan {
            target: target.clone(),
            paths: vec![SubscriptionPathExpectation {
                path: "interfaces/interface[name=Ethernet1/1]/state/counters".to_string(),
                origin: "openconfig".to_string(),
                mode: "ON_CHANGE".to_string(),
                sample_interval_ns: 0,
            }],
            plan_id: "plan-1".to_string(),
            session_id: String::new(),
            state: "submitted".to_string(),
            acquisition: "explicit".to_string(),
            detail: String::new(),
        })
        .await
        .unwrap();
    wait_pending_row(&db, &target).await;
    // Session-bound manifest — the authoritative execution identity.
    plan_tx
        .send(SubscriptionPlan {
            target: target.clone(),
            paths: vec![SubscriptionPathExpectation {
                path: "interfaces/interface[name=Ethernet1/1]/state/counters".to_string(),
                origin: "openconfig".to_string(),
                mode: "ON_CHANGE".to_string(),
                sample_interval_ns: 0,
            }],
            plan_id: "plan-1".to_string(),
            session_id: "sess-a".to_string(),
            state: "submitted".to_string(),
            acquisition: "explicit".to_string(),
            detail: String::new(),
        })
        .await
        .unwrap();
    sync_session(&sessions, &target, "plan-1", "sess-a", 1);

    // Arm the failure: every status write fails while armed. The
    // in-memory desired status commits, but no durable row may claim
    // observed, and the bounded retry queue keeps failing.
    fail_writes.store(true, Ordering::SeqCst);
    publish_bus.publish(gnmi_obs(
        &target,
        "plan-1",
        "sess-a",
        "gnmi",
        "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
        "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
        false,
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let statuses = query_subscription_statuses(&db).await;
    assert!(
        statuses.iter().all(|(_, _, s)| s != "observed"),
        "failed write must not surface as observed: {:?}",
        statuses
    );

    // Storage restored: the bounded durable-retry queue converges the
    // transition WITHOUT any further device telemetry — a one-shot
    // observation survives a transient persistence failure.
    fail_writes.store(false, Ordering::SeqCst);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut observed = false;
    while std::time::Instant::now() < deadline {
        let statuses = query_subscription_statuses(&db).await;
        if statuses.iter().any(|(_, _, s)| s == "observed") {
            observed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        observed,
        "transition was not retried after storage recovery"
    );

    _stx.send(true).unwrap();
    let _ = verifier.await;
}

// ── M5.2B local-acquisition manifest ordering (production path) ─────────────

fn leaf_update_path(interface: &str) -> Path {
    Path {
        origin: "openconfig".to_string(),
        elem: vec![
            PathElem {
                name: "interfaces".to_string(),
                key: Default::default(),
            },
            PathElem {
                name: "interface".to_string(),
                key: [("name".to_string(), interface.to_string())]
                    .into_iter()
                    .collect(),
            },
            PathElem {
                name: "state".to_string(),
                key: Default::default(),
            },
            PathElem {
                name: "counters".to_string(),
                key: Default::default(),
            },
            PathElem {
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
        response: Some(subscribe_response::Response::Update(Notification {
            timestamp: 1_000,
            prefix: None,
            update: vec![Update {
                path: Some(path.clone()),
                val: Some(tv),
                ..Default::default()
            }],
            delete: vec![],
            atomic: false,
        })),
        ..Default::default()
    }
}

fn sync_notification() -> SubscribeResponse {
    SubscribeResponse {
        response: Some(subscribe_response::Response::SyncResponse(true)),
        ..Default::default()
    }
}

/// Deterministic production-path regression for local (RuntimeMode::All)
/// acquisition: the subscriber's lifecycle events flow through the same
/// drain → registry apply → manifest-forward contract as the remote
/// lifecycle RPC, and the coverage verifier registers expectations only
/// after the execution's lifecycle commit. A same-plan reconnect must
/// re-establish observed coverage from its own initial sync evidence.
#[tokio::test]
async fn local_all_mode_manifest_orders_coverage_across_reconnect() {
    use bonsai::subscription_sessions::{SessionLifecycleEvent, SubscriptionSessionRegistry};

    let lab = start_lab(vec!["openconfig-interfaces"]).await;
    let dir = tempfile::tempdir().unwrap();
    let store = bonsai::graph::GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        256 * 1024 * 1024,
    )
    .expect("open graph store");
    let db = store.db();
    let bus = InProcessBus::new(64);
    let sessions = Arc::new(SubscriptionSessionRegistry::new());
    let (plan_tx, plan_rx) = mpsc::channel::<bonsai::subscription_status::SubscriptionPlan>(16);

    // Production drain (server_startup): apply lifecycle events to the
    // authoritative registry and forward APPLIED manifests to the
    // verifier — identical code path via apply_local_lifecycle_event.
    let (lc_tx, mut lc_rx) = mpsc::channel::<SessionLifecycleEvent>(64);
    {
        let drain_sessions = Arc::clone(&sessions);
        let drain_plan_tx = plan_tx.clone();
        tokio::spawn(async move {
            while let Some(event) = lc_rx.recv().await {
                bonsai::subscription_status::apply_local_lifecycle_event(
                    &drain_sessions,
                    Some(&drain_plan_tx),
                    event,
                )
                .await;
            }
        });
    }

    // Verifier with the shared readiness contract.
    let (vsd, vsrx) = watch::channel(false);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let verifier = tokio::spawn(run_subscription_verifier(
        store,
        bus.clone(),
        plan_rx,
        Arc::clone(&sessions),
        vsrx,
        Some(ready_tx),
    ));
    ready_rx
        .await
        .expect("verifier exited before readiness")
        .expect("verifier boot initialization failed");

    let iface_path = "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets";
    let selected = vec![SelectedSubscriptionPath {
        path: iface_path.to_string(),
        origin: "openconfig".to_string(),
        provenance: String::new(),
        mode: "ON_CHANGE".to_string(),
        sample_interval_ns: 0,
        rationale: String::new(),
        optional: false,
    }];
    // One execution counter book shared across recreations — mirrors
    // production SubscriberHandleMap behavior (M5.2B B1).
    let execution_seq = Arc::new(std::sync::atomic::AtomicU64::new(0));

    let spawn_subscriber = |lc_tx: &mpsc::Sender<SessionLifecycleEvent>| {
        let lifecycle = bonsai::subscriber::LifecycleHandle {
            tx: lc_tx.clone(),
            authority_epoch: 0,
            assignment_generation: 0,
            collector_session_generation: 0,
        };
        GnmiSubscriber::new(
            lab.addr.clone(),
            None,
            None,
            None,
            None,
            None,
            None,
            bonsai::gnmi_security::ConnSecurity::plaintext(true),
            bus.clone(),
            None,
            Some(plan_tx.clone()),
            selected.clone(),
            AcquisitionMode::Explicit,
            Some(lifecycle),
            Arc::clone(&execution_seq),
        )
    };

    async fn drive_initial_sync(lab: &Lab, iface: &str, tag: i64) {
        // Initial-sync ordering: the covering Update legitimately arrives
        // BEFORE the SyncResponse — it must be buffered and bound to the
        // exact session once the manifest registers.
        let update = update_notification(&leaf_update_path(iface), tag);
        let sync = sync_notification();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let streams = lab.streams.lock().await;
            if let Some(tx) = streams.last() {
                if tx.send(Ok(update.clone())).await.is_ok()
                    && tx.send(Ok(sync.clone())).await.is_ok()
                {
                    return;
                }
            }
            drop(streams);
            assert!(
                std::time::Instant::now() < deadline,
                "no open subscribe stream for initial sync"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_observed(db: &Arc<lbug::Database>, target: &str, path: &str, what: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let statuses = query_subscription_statuses(db).await;
            if statuses
                .iter()
                .any(|(dev, p, st)| dev == target && p == path && st == "observed")
            {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{what}: coverage never became observed: {statuses:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // ── first acquisition ──
    let sub1 = spawn_subscriber(&lc_tx);
    let (sd1, mut srx1) = watch::channel(false);
    let h1 = tokio::spawn(async move { sub1.subscribe_telemetry(&mut srx1).await });
    assert!(
        wait_for(
            || lab.subscribe_calls.load(Ordering::SeqCst) >= 1,
            Duration::from_secs(10)
        )
        .await,
        "first subscribe rpc"
    );
    drive_initial_sync(&lab, "Ethernet1/1", 111).await;
    wait_observed(&db, &lab.addr, iface_path, "first acquisition").await;

    // ── same-plan reconnect: new session, shared execution counter ──
    let _ = sd1.send(true);
    let _ = h1.await;
    let sub2 = spawn_subscriber(&lc_tx);
    let (sd2, mut srx2) = watch::channel(false);
    let _h2 = tokio::spawn(async move { sub2.subscribe_telemetry(&mut srx2).await });
    assert!(
        wait_for(
            || lab.subscribe_calls.load(Ordering::SeqCst) >= 2,
            Duration::from_secs(10)
        )
        .await,
        "reconnect subscribe rpc"
    );
    drive_initial_sync(&lab, "Ethernet1/1", 222).await;
    wait_observed(&db, &lab.addr, iface_path, "same-plan reconnect").await;

    // The reconnect's own initial-sync evidence — no additional
    // telemetry — restored observed coverage under the new session.
    let _ = sd2.send(true);
    let _ = vsd.send(true);
    let _ = verifier.await;
}
