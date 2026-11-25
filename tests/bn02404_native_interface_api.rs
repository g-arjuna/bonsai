//! BN-024-04 operator API boundary for bounded native FRR interface evidence.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use bonsai::{
    config::TargetConfig,
    enrichment::frr_interfaces::{StoredFrrInterfaceShadowObservation, persist_observation},
    graph::{ConfigItemRecord, GraphStore},
    http_server::{AppState, mint_jwt, router_from_state},
};
use serde_json::Value;
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
    serde_json::from_str(include_str!(
        "../docs/merger/evidence/2026-11-25-bn024-04-native-acquisition/live-shadow-observation.json"
    ))
    .expect("committed live evidence shape")
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
    bonsai::enrichment::frr_interfaces::record_acquisition_success(&h.store, &observation)
        .await
        .expect("record success");

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
        bonsai::enrichment::frr_interfaces::record_acquisition_success(&store, &observation)
            .await
            .expect("record acquisition state");
    }

    let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("reopen graph"));
    let retained = bonsai::enrichment::frr_interfaces::latest_observation(&store, &address)
        .await
        .expect("shadow observation must survive restart");
    assert_eq!(retained.captured_at_ns, observation.captured_at_ns);
    assert_eq!(retained.facts_validated, observation.facts_validated);
    let acquisition = bonsai::enrichment::frr_interfaces::acquisition_state(&store, &address)
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
    assert_eq!(body["facts_emitted"], 4);
    assert_eq!(body["facts_validated"], 4);
    assert_eq!(body["interface_command"], "vtysh -c 'show interface json'");
    assert_eq!(
        body["interface_output_sha256"],
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
