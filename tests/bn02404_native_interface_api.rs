//! BN-024-04 operator API boundary for bounded native FRR interface evidence.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use bonsai::{
    config::TargetConfig,
    enrichment::frr_interfaces::{
        StoredFrrInterfaceShadowObservation, canonical_device_id,
        invalidate_device_native_interface_state, latest_observation, observation_id,
        persist_degraded_observation, persist_observation,
    },
    frr_interface_json::{
        FRR_JSON_ACQUISITION_CONTRACT, FrrInterfaceJsonObservation, normalize_frr_interface_json,
    },
    graph::{ConfigItemRecord, GraphStore},
    http_server::{AppState, mint_jwt, router_from_state},
    interface_normalizer::{CaptureOutcome, CommandCapture, NormalizerLimits, ObservationContext},
};
use serde_json::Value;
use sha2::Digest;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

const ARTIFACT: &str =
    "quay.io/frrouting/frr@sha256:813497af103e972600f6161012b3bf2a2a1c5ea10d09cd1eacd74f1dc510a57b";

struct Harness {
    _dir: TempDir,
    store: Arc<GraphStore>,
    registry: Arc<bonsai::registry::ApiRegistry>,
    router: axum::Router,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let graph_path = dir.path().join("graph").to_string_lossy().into_owned();
    let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("open graph"));
    for (username, role) in [("bn024-viewer", "viewer"), ("bn024-operator", "operator")] {
        store
            .auth_store
            .upsert_user(&bonsai::auth_store::AuthUser {
                username: username.into(),
                password_hash: "test-verifier".into(),
                role: role.into(),
                enabled: true,
                last_login_ns: 0,
                created_at_ns: 0,
                updated_at_ns: 0,
            })
            .expect("upsert user");
    }
    let state = AppState::new_minimal_with_runtime_dir(
        Arc::clone(&store),
        dir.path().to_str().expect("utf8 tempdir"),
    );
    let registry = Arc::clone(&state.registry);
    let router = router_from_state(state);
    Harness {
        _dir: dir,
        store,
        registry,
        router,
    }
}

async fn token(store: &GraphStore, user: &str, role: &str) -> String {
    mint_jwt(user, role, store).await.expect("mint jwt")
}

async fn request(
    router: &axum::Router,
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(token) = bearer {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let body = if let Some(body) = body {
        req = req.header("content-type", "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    let response = router
        .clone()
        .oneshot(req.body(body).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body bytes");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn evidence() -> StoredFrrInterfaceShadowObservation {
    let mut observation: StoredFrrInterfaceShadowObservation = serde_json::from_str(include_str!(
        "../docs/merger/evidence/2026-11-25-bn024-04-native-acquisition/live-shadow-observation.json"
    ))
    .expect("committed live evidence shape");
    // The committed artifact is genuine pre-correction evidence. Tests re-key
    // the same observed facts into the corrected canonical v2 slot and add
    // metadata that the collector now emits at acquisition time; no fact or
    // device output is rewritten.
    observation.collector_id = observation
        .facts
        .first()
        .and_then(|fact| fact.get("source"))
        .and_then(|source| source.get("collector_id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("bn02404-live-test")
        .to_string();
    observation.observation_id =
        observation_id(&observation.device_id, &observation.network_namespace);
    observation.interpretation_outcome = "complete_usable".into();
    observation.usable_entity_facts = observation
        .facts
        .iter()
        .filter(|fact| {
            fact.get("subject")
                .and_then(|subject| subject.get("entity"))
                .is_some()
        })
        .count();
    observation.status_facts = observation
        .facts
        .iter()
        .filter(|fact| {
            fact.get("subject")
                .and_then(|subject| subject.get("entity"))
                .is_none()
        })
        .count();
    observation
}

fn rewrite_fact_provenance(observation: &mut StoredFrrInterfaceShadowObservation) {
    // Tests sometimes simulate a later acquisition using the genuine fixture's
    // interface payloads. Typed facts are content-addressed, so provenance
    // cannot merely be patched in JSON; reseal the fact after changing the
    // acquisition axes. This is explicit test adaptation, not live evidence.
    for value in &mut observation.facts {
        let mut fact: bonsai::typed_facts::TypedFact =
            serde_json::from_value(value.clone()).expect("typed fact fixture");
        fact.source.capture_id = observation.capture_id.clone();
        fact.source.session_epoch = observation.session_epoch;
        fact.source.sequence = observation.sequence;
        fact.observed_at = observation.captured_at.clone();
        fact.seal();
        *value = serde_json::to_value(fact).expect("resealed fact json");
    }
}

fn fresh_evidence() -> StoredFrrInterfaceShadowObservation {
    let mut observation = evidence();
    let now = bonsai::graph::common::now_ns();
    observation.capture_started_at_ns = now - 1;
    observation.captured_at_ns = now;
    observation.captured_at =
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    observation.session_epoch = observation.capture_started_at_ns as u64;
    observation.sequence = 1;
    rewrite_fact_provenance(&mut observation);
    observation
}

fn target(address: &str, username: Option<String>, password: Option<String>) -> TargetConfig {
    TargetConfig {
        address: address.into(),
        enabled: true,
        vendor: Some("frr".into()),
        username,
        password,
        hostname: Some("frr2".into()),
        external_id: Some("containerlab:clab-bonsai-bn003-vrf-frr2".into()),
        software_build: Some(ARTIFACT.into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn authorized_device_surface_exposes_bounded_shadow_evidence_and_staleness() {
    let h = harness().await;
    let mut observation = fresh_evidence();
    let address = observation.device_address.clone();
    h.registry
        .add_device_with_audit(target(&address, None, None), "bn02404-test", "test_add")
        .expect("register target");
    persist_observation(&h.store, &observation)
        .await
        .expect("persist genuine evidence shape");
    let viewer = token(&h.store, "bn024-viewer", "viewer").await;
    let uri = format!("/api/devices/{address}/native-interfaces/frr");

    let (status, _) = request(&h.router, "GET", &uri, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = request(&h.router, "GET", &uri, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["promotion"], "shadow_only");
    assert_eq!(body["authoritative_projection"], "none");
    assert_eq!(body["freshness"], "fresh");
    assert_eq!(body["evidence"]["facts_emitted"], 4);
    assert_eq!(body["evidence"]["facts_validated"], 4);
    assert_eq!(
        body["evidence"]["interface_output_sha256"],
        "b61b12df7142a9fcfacb4718eb9fc2b3210cd91a68122a87254dd093855c489a"
    );
    assert!(body["evidence"].get("raw_interface_json").is_none());
    assert!(body["evidence"].get("interface_diagnostics").is_none());
    assert!(body["evidence"].get("host_key").is_none());
    assert_eq!(
        body["evidence"]["interpretation_outcome"],
        "complete_usable"
    );
    assert_eq!(body["evidence"]["usable_entity_facts"], 4);
    let replay_error = persist_observation(&h.store, &observation)
        .await
        .expect_err("exact replay must not overwrite newer retained evidence");
    assert!(replay_error.to_string().contains("replayed"));

    // Viewer mutation is denied before any credential/device access. This is
    // the operator-facing denied state; the server remains the authority.
    let collect_uri = format!("{uri}/collect");
    let (status, _) = request(
        &h.router,
        "POST",
        &collect_uri,
        Some(&viewer),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    observation.captured_at_ns -= 2 * 3600 * 1_000_000_000;
    observation.session_epoch += 1;
    rewrite_fact_provenance(&mut observation);
    persist_observation(&h.store, &observation)
        .await
        .expect("persist aged evidence");
    let (status, body) = request(&h.router, "GET", &uri, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["freshness"], "stale");
    assert!(body["age_seconds"].as_i64().unwrap() >= 7200);
}

#[tokio::test]
async fn scoped_authorization_and_failed_attempt_visibility_are_enforced() {
    let h = harness().await;
    let observation = fresh_evidence();
    let address = observation.device_address.clone();
    let device = target(&address, None, None);
    h.registry
        .add_device_with_audit(device.clone(), "bn02404-test", "test_add")
        .expect("register target");
    persist_observation(&h.store, &observation)
        .await
        .expect("persist successful observation");

    let (_, wrong_write_key) = h
        .store
        .auth_store
        .create_api_key(
            "bn024-wrong-device-writer",
            "routing:write:speaker=192.0.2.254",
            0,
        )
        .expect("create wrong-device write key");
    let (_, write_key) = h
        .store
        .auth_store
        .create_api_key(
            "bn024-device-writer",
            &format!(
                "routing:write:speaker={}",
                device
                    .external_id
                    .as_deref()
                    .expect("canonical external id")
            ),
            0,
        )
        .expect("create device write key");
    let (_, address_scoped_write_key) = h
        .store
        .auth_store
        .create_api_key(
            "bn024-address-aliased-writer",
            &format!("routing:write:speaker={address}"),
            0,
        )
        .expect("create address-aliased write key");

    let (_, wrong_read_key) = h
        .store
        .auth_store
        .create_api_key(
            "bn024-wrong-device-reader",
            "routing:read:speaker=192.0.2.254",
            0,
        )
        .expect("create wrong-device read key");
    let (_, read_key) = h
        .store
        .auth_store
        .create_api_key(
            "bn024-device-reader",
            &format!(
                "routing:read:speaker={}",
                device
                    .external_id
                    .as_deref()
                    .expect("canonical external id")
            ),
            0,
        )
        .expect("create device read key");

    let base = format!("/api/devices/{address}/native-interfaces/frr");
    let collect = format!("{base}/collect");
    let (status, _) = request(&h.router, "GET", &base, Some(&wrong_read_key), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "wrong-device read scope");
    let (status, body) = request(&h.router, "GET", &base, Some(&read_key), None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "device-scoped read must pass: {body}"
    );
    assert_eq!(body["acquisition"]["last_attempt_outcome"], "success");

    let (status, _) = request(
        &h.router,
        "POST",
        &collect,
        Some(&wrong_write_key),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "wrong-device collection scope"
    );
    let (status, _) = request(
        &h.router,
        "POST",
        &collect,
        Some(&address_scoped_write_key),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an inventory address is not a substitute for canonical device identity"
    );
    // The correctly scoped writer reaches inventory/credential admission. This
    // fixture deliberately has no device credential, so it is refused before
    // any network contact; importantly the scope itself was accepted.
    let (status, _) = request(
        &h.router,
        "POST",
        &collect,
        Some(&write_key),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::FAILED_DEPENDENCY);

    // A failed acquisition is explicit additive state: it neither replaces
    // nor refreshes the last successful shadow observation.
    bonsai::enrichment::frr_interfaces::record_acquisition_failure(
        &h.store,
        &device,
        observation.capture_started_at_ns + 10,
        "capture_transport_or_interpretation_failed",
        "fixed read-only capture or bounded interpretation failed",
    )
    .await
    .expect("record failure");
    let (status, body) = request(&h.router, "GET", &base, Some(&read_key), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "available");
    assert_eq!(body["acquisition"]["last_attempt_outcome"], "failed");
    assert_eq!(
        body["acquisition"]["last_successful_observation_id"],
        observation.observation_id
    );
    assert_eq!(
        body["evidence"]["captured_at_ns"],
        observation.captured_at_ns
    );

    // Ordering is acquisition-start based: an older result cannot overwrite a
    // newer retained observation merely because processing completed later.
    let mut older = observation.clone();
    older.session_epoch = observation.session_epoch.saturating_sub(1);
    older.sequence = observation.sequence;
    rewrite_fact_provenance(&mut older);
    let err = persist_observation(&h.store, &older)
        .await
        .expect_err("older acquisition-start result must be rejected");
    assert!(err.to_string().contains("stale or replayed"));

    // If no successful observation exists, the failed attempt is still an
    // explicit operator-visible state rather than an empty successful table.
    let failed_only = "192.0.2.99";
    let mut failed_target = target(failed_only, None, None);
    failed_target.external_id = None;
    failed_target.hostname = None;
    h.registry
        .add_device_with_audit(failed_target.clone(), "bn02404-test", "test_add")
        .expect("register failed-only target");
    bonsai::enrichment::frr_interfaces::record_acquisition_failure(
        &h.store,
        &failed_target,
        observation.capture_started_at_ns + 20,
        "capture_deadline",
        "fixed read-only capture exceeded its absolute deadline",
    )
    .await
    .expect("record failed-only attempt");
    let viewer = token(&h.store, "bn024-viewer", "viewer").await;
    let failed_uri = format!("/api/devices/{failed_only}/native-interfaces/frr");
    let (status, body) = request(&h.router, "GET", &failed_uri, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "acquisition_failed");
    assert_eq!(body["evidence"], serde_json::Value::Null);
    assert_eq!(
        body["acquisition"]["last_failure_class"],
        "capture_deadline"
    );
}

#[tokio::test]
async fn shadow_observation_and_acquisition_state_survive_store_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let graph_path = dir.path().join("graph").to_string_lossy().into_owned();
    let observation = fresh_evidence();
    let address = observation.device_address.clone();
    {
        let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("open graph"));
        persist_observation(&store, &observation)
            .await
            .expect("persist observation");
    }

    let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("reopen graph"));
    let retained = bonsai::enrichment::frr_interfaces::latest_observation(
        &store,
        &target(&address, None, None),
    )
    .await
    .expect("shadow observation must survive restart");
    assert_eq!(retained.captured_at_ns, observation.captured_at_ns);
    assert_eq!(retained.facts_validated, observation.facts_validated);
    let acquisition = bonsai::enrichment::frr_interfaces::acquisition_state(
        &store,
        &target(&address, None, None),
    )
    .await
    .expect("decode acquisition state")
    .expect("acquisition state must survive restart");
    assert_eq!(acquisition.last_attempt_outcome, "success");
    assert_eq!(
        acquisition.last_successful_observation_id.as_deref(),
        Some(observation.observation_id.as_str())
    );
    let replay_error = persist_observation(&store, &observation)
        .await
        .expect_err("restart must not make replayed evidence current");
    assert!(replay_error.to_string().contains("replayed"));
}

#[tokio::test]
async fn canonical_identity_prevents_address_reuse_or_fact_transfer() {
    let h = harness().await;
    let observation = fresh_evidence();
    let address = observation.device_address.clone();
    let device_a = target(&address, None, None);
    persist_observation(&h.store, &observation)
        .await
        .expect("persist canonical evidence");

    let mut device_b = device_a.clone();
    device_b.external_id = Some("containerlab:replacement-device".into());
    assert_ne!(
        canonical_device_id(&device_a),
        canonical_device_id(&device_b)
    );
    let err = latest_observation(&h.store, &device_b)
        .await
        .expect_err("evidence for device A must not decode as device B evidence");
    assert!(err.to_string().contains("shadow evidence unavailable"));

    let retained = latest_observation(&h.store, &device_a)
        .await
        .expect("canonical owner evidence remains readable");
    assert_eq!(retained.device_id, canonical_device_id(&device_a));

    // A corrupted record whose top-level identity was rewritten to device B
    // still carries device A typed-fact subjects and must fail admission.
    let mut corrupted = observation.clone();
    corrupted.device_id = canonical_device_id(&device_b);
    corrupted.observation_id = observation_id(&corrupted.device_id, &corrupted.network_namespace);
    corrupted.session_epoch += 10;
    let err = persist_observation(&h.store, &corrupted)
        .await
        .expect_err("cross-device typed facts must be rejected");
    assert!(
        err.to_string()
            .contains("typed-fact subject/source identity mismatch")
    );
}

#[tokio::test]
async fn concurrent_persistence_admits_newer_epoch_regardless_of_completion_order() {
    let h = harness().await;
    let base = fresh_evidence();
    let mut older = base.clone();
    older.session_epoch = base.session_epoch.saturating_sub(1);
    rewrite_fact_provenance(&mut older);
    let mut newer = base;
    newer.session_epoch += 1;
    rewrite_fact_provenance(&mut newer);

    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let mut tasks = Vec::new();
    for observation in [older.clone(), newer.clone()] {
        let store = Arc::clone(&h.store);
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            persist_observation(&store, &observation).await
        }));
    }
    barrier.wait().await;
    let mut outcomes = Vec::new();
    for task in tasks {
        outcomes.push(task.await.expect("persistence task"));
    }
    assert!(
        outcomes[1].is_ok(),
        "newer epoch must always be admitted: {outcomes:?}"
    );
    // Whether the older writer won or lost the lock race, it can never remove
    // the newer epoch once that epoch has been admitted.
    let device = target(&newer.device_address, None, None);
    let retained = latest_observation(&h.store, &device)
        .await
        .expect("retained observation");
    assert_eq!(retained.session_epoch, newer.session_epoch);
}

#[tokio::test]
async fn stale_failure_success_and_exact_replay_are_fenced_at_persistence_boundary() {
    let h = harness().await;
    let mut success = fresh_evidence();
    success.capture_started_at_ns -= 100;
    success.captured_at_ns -= 90;
    success.session_epoch = success.capture_started_at_ns as u64;
    rewrite_fact_provenance(&mut success);
    let device = target(&success.device_address, None, None);
    persist_observation(&h.store, &success)
        .await
        .expect("initial successful observation");

    // A failure from an acquisition that started before the admitted success
    // may finish later, but cannot overwrite the newer successful attempt.
    let err = bonsai::enrichment::frr_interfaces::record_acquisition_failure(
        &h.store,
        &device,
        success.capture_started_at_ns - 10,
        "capture_failed",
        "older capture finished after newer success",
    )
    .await
    .expect_err("older failure must be rejected at the persistence boundary");
    assert!(err.to_string().contains("stale or replayed"));
    let state = bonsai::enrichment::frr_interfaces::acquisition_state(&h.store, &device)
        .await
        .expect("decode state")
        .expect("state");
    assert_eq!(state.last_attempt_outcome, "success");

    // A newer failure is operator-visible while preserving the last usable
    // observation pointer.
    let newer_failure_started = success.capture_started_at_ns + 10;
    bonsai::enrichment::frr_interfaces::record_acquisition_failure(
        &h.store,
        &device,
        newer_failure_started,
        "capture_failed",
        "newer capture failed",
    )
    .await
    .expect("newer failure admitted");
    let state = bonsai::enrichment::frr_interfaces::acquisition_state(&h.store, &device)
        .await
        .expect("decode state")
        .expect("state");
    assert_eq!(state.last_attempt_outcome, "failed");
    assert_eq!(
        state.last_successful_observation_id.as_deref(),
        Some(success.observation_id.as_str())
    );

    // An older successful capture completing after that newer failure may be
    // retained as older usable evidence, but it must not erase the newer
    // latest-attempt failure.
    let mut older_success = success.clone();
    older_success.capture_started_at_ns = success.capture_started_at_ns + 5;
    older_success.captured_at_ns = newer_failure_started + 20;
    older_success.session_epoch = older_success.capture_started_at_ns as u64;
    rewrite_fact_provenance(&mut older_success);
    persist_observation(&h.store, &older_success)
        .await
        .expect("older usable evidence may be retained without altering latest failure");
    let state = bonsai::enrichment::frr_interfaces::acquisition_state(&h.store, &device)
        .await
        .expect("decode state")
        .expect("state");
    assert_eq!(state.last_attempt_outcome, "failed");
    assert_eq!(
        state.last_successful_capture_started_at_ns,
        Some(older_success.capture_started_at_ns)
    );

    // Repeating the exact same failure does not refresh freshness or attempt
    // metadata.
    let before = bonsai::enrichment::frr_interfaces::acquisition_state(&h.store, &device)
        .await
        .expect("decode state")
        .expect("state");
    let err = bonsai::enrichment::frr_interfaces::record_acquisition_failure(
        &h.store,
        &device,
        newer_failure_started,
        "capture_failed",
        "exact replay",
    )
    .await
    .expect_err("exact replayed failure must be rejected");
    assert!(err.to_string().contains("stale or replayed"));
    let after = bonsai::enrichment::frr_interfaces::acquisition_state(&h.store, &device)
        .await
        .expect("decode state")
        .expect("state");
    assert_eq!(
        after.last_attempt_started_at_ns,
        before.last_attempt_started_at_ns
    );
    assert_eq!(
        after.last_attempt_finished_at_ns,
        before.last_attempt_finished_at_ns
    );
}

#[tokio::test]
async fn degraded_interpretation_preserves_last_usable_evidence() {
    let h = harness().await;
    let observation = fresh_evidence();
    let device = target(&observation.device_address, None, None);
    persist_observation(&h.store, &observation)
        .await
        .expect("persist usable evidence");

    let raw = "{\"unexpected-schema\":true}";
    let capture =
        CommandCapture::new(raw, CaptureOutcome::Complete).captured_at("2026-11-25T15:00:00.000Z");
    let mut context = ObservationContext::minimal(
        &observation.device_id,
        &observation.collector_id,
        &observation.capture_id,
        "2026-11-25T15:00:00.000Z",
    );
    context.network_namespace = Some(&observation.network_namespace);
    context.platform = Some("frr");
    context.session_epoch = observation.session_epoch + 1;
    context.sequence = observation.sequence;
    let input = FrrInterfaceJsonObservation {
        capture,
        context,
        acquisition_contract: FRR_JSON_ACQUISITION_CONTRACT,
    };
    let normalized = normalize_frr_interface_json(&input, NormalizerLimits::default());
    assert_eq!(normalized.facts.len(), 1);
    assert!(normalized.facts[0].subject.entity.is_none());
    assert!(normalized.facts[0].payload.state.is_some());

    let mut degraded = observation.clone();
    degraded.session_epoch += 1;
    degraded.capture_started_at_ns += 1;
    degraded.captured_at_ns += 1;
    degraded.raw_interface_json = Some(raw.into());
    degraded.interface_output_bytes = raw.len();
    degraded.interface_output_sha256 = format!("{:x}", sha2::Sha256::digest(raw.as_bytes()));
    degraded.facts = normalized
        .facts
        .iter()
        .map(|fact| serde_json::to_value(fact).expect("status fact json"))
        .collect();
    degraded.facts_emitted = degraded.facts.len();
    degraded.facts_validated = degraded.facts.len();
    degraded.facts_retained = degraded.facts.len();
    degraded.usable_entity_facts = 0;
    degraded.status_facts = degraded.facts.len();
    degraded.interpretation_outcome = "invalid_input".into();
    degraded.normalization_report =
        serde_json::to_value(&normalized.report).expect("normalization report json");

    let err = persist_observation(&h.store, &degraded)
        .await
        .expect_err("degraded payload must not enter the usable observation slot");
    assert!(
        err.to_string()
            .contains("cannot be admitted as usable evidence")
    );
    persist_degraded_observation(
        &h.store,
        &degraded,
        "invalid_structured_input",
        "structured input did not match the qualified schema",
    )
    .await
    .expect("persist degraded attempt evidence");

    let retained = latest_observation(&h.store, &device)
        .await
        .expect("last usable evidence must survive degraded interpretation");
    assert_eq!(retained.session_epoch, observation.session_epoch);
    assert_eq!(retained.usable_entity_facts, 4);
    let state = bonsai::enrichment::frr_interfaces::acquisition_state(&h.store, &device)
        .await
        .expect("decode state")
        .expect("state");
    assert_eq!(state.last_attempt_outcome, "degraded");
    assert_eq!(
        state.last_interpretation_outcome.as_deref(),
        Some("invalid_input")
    );
    assert_eq!(
        state.last_successful_observation_id.as_deref(),
        Some(observation.observation_id.as_str())
    );
}

#[tokio::test]
async fn removal_invalidates_evidence_without_permanently_poisoning_identity() {
    let h = harness().await;
    let mut observation = fresh_evidence();
    let device = target(&observation.device_address, None, None);
    persist_observation(&h.store, &observation)
        .await
        .expect("persist evidence");
    let legacy_id = format!(
        "native-frr-interface-{}",
        hex::encode(sha2::Sha256::digest(device.primary_ip().as_bytes()))
    );
    h.store
        .upsert_config_item(ConfigItemRecord {
            id: legacy_id.clone(),
            config_class: "native_interface_shadow_observation".into(),
            vendor: "frr".into(),
            name: device.primary_ip().to_string(),
            version: "1".into(),
            content_json: format!("{{\"legacy\":\"{}\"}}", "x".repeat(160)),
            enabled: true,
            created_by: "bn02404-test".into(),
        })
        .await
        .expect("seed legacy address-keyed evidence");
    invalidate_device_native_interface_state(&h.store, &device)
        .await
        .expect("invalidate device native evidence");
    let legacy = h
        .store
        .get_config_item(&legacy_id)
        .await
        .expect("legacy point lookup")
        .expect("legacy tombstone");
    assert!(
        !legacy.enabled,
        "legacy address-keyed evidence is tombstoned"
    );
    let err = latest_observation(&h.store, &device)
        .await
        .expect_err("invalidated evidence must not be served");
    assert!(err.to_string().contains("shadow evidence unavailable"));

    // If the same canonical identity is deliberately re-registered later, a
    // new acquisition can write fresh evidence. The invalidated payload is not
    // resurrected; it is atomically replaced only by a newer validated result.
    observation.session_epoch += 1;
    observation.capture_started_at_ns += 1;
    observation.captured_at_ns += 1;
    rewrite_fact_provenance(&mut observation);
    persist_observation(&h.store, &observation)
        .await
        .expect("new validated acquisition after explicit re-registration");
    let retained = latest_observation(&h.store, &device)
        .await
        .expect("fresh evidence readable");
    assert_eq!(retained.session_epoch, observation.session_epoch);
}

/// Genuine end-to-end operator acceptance: production router auth and
/// inventory boundary, fixed verified SSH capture, exact-artifact registry
/// dispatch, typed-fact validation, persistence and authorized retrieval.
#[tokio::test]
#[ignore = "requires authorized live FRR lab, credential env and pinned SSH host key"]
async fn operator_collects_live_frr_shadow_evidence_through_production_api() {
    let host = std::env::var("BONSAI_FRR_LIVE_HOST").expect("live host");
    let username = std::env::var("BONSAI_FRR_LIVE_USERNAME").expect("live username");
    let password = std::env::var("BONSAI_FRR_LIVE_PASSWORD").expect("live password");
    let known_hosts = std::env::var("BONSAI_FRR_LIVE_KNOWN_HOSTS").expect("known hosts");
    let h = harness().await;
    h.store
        .upsert_config_item(ConfigItemRecord {
            id: "bn02404-native-cli-known-hosts".into(),
            config_class: "app_config".into(),
            vendor: String::new(),
            name: "native_cli_known_hosts".into(),
            version: "1".into(),
            content_json: known_hosts,
            enabled: true,
            created_by: "bn02404-test".into(),
        })
        .await
        .expect("pin known hosts");
    h.registry
        .add_device_with_audit(
            target(&host, Some(username), Some(password)),
            "bn02404-test",
            "test_add",
        )
        .expect("register live target");
    let operator = token(&h.store, "bn024-operator", "operator").await;
    let base = format!("/api/devices/{host}/native-interfaces/frr");
    let collect = format!("{base}/collect");
    let (status, body) = request(
        &h.router,
        "POST",
        &collect,
        Some(&operator),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["qualification"], "Qualified");
    assert_eq!(body["shadow_only"], true);
    assert_eq!(body["result"]["facts_emitted"], 4);
    assert_eq!(body["result"]["facts_validated"], 4);
    assert_eq!(body["result"]["usable_entity_facts"], 4);
    assert_eq!(body["result"]["interpretation_outcome"], "complete_usable");
    assert_eq!(
        body["result"]["interface_command"],
        "vtysh -c 'show interface json'"
    );
    assert_eq!(
        body["result"]["interface_output_sha256"],
        "b61b12df7142a9fcfacb4718eb9fc2b3210cd91a68122a87254dd093855c489a"
    );

    let viewer = token(&h.store, "bn024-viewer", "viewer").await;
    let (status, body) = request(&h.router, "GET", &base, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["promotion"], "shadow_only");
    assert_eq!(body["evidence"]["facts_validated"], 4);
    assert_eq!(body["evidence"]["network_namespace"], "default");
    assert_eq!(
        body["evidence"]["network_namespace_evidence"],
        "fixed verified SSH session in device default network namespace; no namespace override"
    );
}
