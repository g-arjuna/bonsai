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
    CapabilityRequest, CapabilityResponse, Encoding, ModelData, Path, SubscribeRequest,
    SubscribeResponse, Subscription, subscribe_request,
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
        let requests = Arc::clone(&self.requests);
        let mut inner = request.into_inner();
        tokio::spawn(async move {
            while let Ok(Some(msg)) = inner.message().await {
                requests.lock().await.push(msg);
            }
        });
        // Hold the response stream open so the client stays subscribed
        // until the test shuts it down.
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
    requests: Arc<Mutex<Vec<SubscribeRequest>>>,
    capability_calls: Arc<AtomicUsize>,
    subscribe_calls: Arc<AtomicUsize>,
    _shutdown: watch::Sender<bool>,
    _handle: tokio::task::JoinHandle<()>,
}

async fn start_lab(models: Vec<&str>) -> Lab {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let capability_calls = Arc::new(AtomicUsize::new(0));
    let subscribe_calls = Arc::new(AtomicUsize::new(0));
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
    GnmiSubscriber::new(
        lab.addr.clone(),
        None,
        None,
        None,
        None,
        None,
        None,
        String::new(),
        None,
        InProcessBus::new(64),
        None,
        Some(plan_tx),
        selected_paths,
        acquisition,
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
    let verifier = tokio::spawn(run_subscription_verifier(store, bus, plan_rx, srx));

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
            state: "disabled".to_string(),
            acquisition: "disabled".to_string(),
            detail: "acquisition disabled".to_string(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let statuses = query_subscription_statuses(&db).await;
    assert!(
        statuses.iter().all(|(_, _, s)| s == "superseded"),
        "disabled plan must supersede all tracked paths: {:?}",
        statuses
    );
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
