//! BN-024-04 operator API boundary for bounded native FRR interface evidence.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use bonsai::{
    config::TargetConfig,
    enrichment::frr_interfaces::{
        CaptureEnvelopeAssemblyInput, FRR_INTERFACE_COMMAND, FRR_MAC_COMMAND, FRR_NEIGHBOR_COMMAND,
        FRR_VERSION_COMMAND, FRR_VLAN_COMMAND, RawAcquisitionBatchEnvelope, RawCaptureEnvelope,
        RawCommandCaptureEnvelope, StoredFrrInterfaceShadowObservation, VerifiedHostKey,
        canonical_device_id, invalidate_device_native_interface_state, latest_observation,
        observation_from_capture_envelope, observation_id, persist_degraded_observation,
        persist_observation,
    },
    graph::{ConfigItemRecord, GraphStore},
    http_server::{AppState, mint_jwt, router_from_state},
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

fn rfc3339_ns(ns: i64) -> String {
    chrono::DateTime::from_timestamp_nanos(ns).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn command_capture(
    key: &str,
    command: &str,
    output: &str,
    started_ns: i64,
    duration_ns: i64,
) -> RawCommandCaptureEnvelope {
    RawCommandCaptureEnvelope {
        command_key: key.into(),
        command: command.into(),
        outcome: "complete".into(),
        started_at: Some(rfc3339_ns(started_ns)),
        completed_at: Some(rfc3339_ns(started_ns + duration_ns)),
        started_at_ns: Some(started_ns),
        completed_at_ns: Some(started_ns + duration_ns),
        output: Some(output.into()),
        output_bytes: Some(output.len()),
        output_sha256: Some(sha256_text(output)),
        detail: None,
        stderr_bytes: None,
    }
}

/// Genuine replay envelope assembled in the exact shape emitted by the fixed
/// SSH helper. Command bytes are genuine read-only fixtures; the execution
/// timestamps are synthetic replay metadata and the resulting observation is
/// explicitly classified as replay, never live SSH acquisition.
fn production_raw_envelope(
    started_ns: i64,
    neighbor_output: &str,
    mac_output: &str,
    vlan_output: &str,
) -> RawCaptureEnvelope {
    let interface_output = include_str!("fixtures/bn024/l2-vertical/frr2-show-interface.json");
    let version_output =
        include_str!("fixtures/bn024/interface-frr-json/show-version-json-frr-10.6.1.json");
    let mut commands = std::collections::BTreeMap::new();
    commands.insert(
        "version".to_string(),
        command_capture(
            "version",
            FRR_VERSION_COMMAND,
            version_output,
            started_ns,
            100_000_000,
        ),
    );
    commands.insert(
        "interfaces".to_string(),
        command_capture(
            "interfaces",
            FRR_INTERFACE_COMMAND,
            interface_output,
            started_ns + 150_000_000,
            200_000_000,
        ),
    );
    commands.insert(
        "arpnd".to_string(),
        command_capture(
            "arpnd",
            FRR_NEIGHBOR_COMMAND,
            neighbor_output,
            started_ns + 400_000_000,
            100_000_000,
        ),
    );
    commands.insert(
        "mac".to_string(),
        command_capture(
            "mac",
            FRR_MAC_COMMAND,
            mac_output,
            started_ns + 550_000_000,
            100_000_000,
        ),
    );
    commands.insert(
        "vlan".to_string(),
        command_capture(
            "vlan",
            FRR_VLAN_COMMAND,
            vlan_output,
            started_ns + 700_000_000,
            100_000_000,
        ),
    );
    RawCaptureEnvelope {
        host: "172.100.208.12".into(),
        port: 22,
        transport: "verified_ssh_read_only_cli".into(),
        host_key: VerifiedHostKey {
            algorithm: "ssh-ed25519".into(),
            sha256: "SHA256:w/HyErTm6v1GknV7Mcae7K5xUMVnIVDvlFE5TfSoT1E".into(),
        },
        acquisition_batch: RawAcquisitionBatchEnvelope {
            started_at: rfc3339_ns(started_ns),
            started_at_ns: started_ns,
            completed_at: rfc3339_ns(started_ns + 900_000_000),
            completed_at_ns: started_ns + 900_000_000,
            deadline_seconds: 40,
            atomic_snapshot: false,
        },
        commands,
        release_raw: Some("10.6.1_git".into()),
        device_hostname: Some("frr2".into()),
    }
}

fn production_evidence_from_raw(
    raw: RawCaptureEnvelope,
    started_ns: i64,
) -> StoredFrrInterfaceShadowObservation {
    observation_from_capture_envelope(
        raw,
        CaptureEnvelopeAssemblyInput {
            device_id: "containerlab:clab-bonsai-bn003-vrf-frr2".into(),
            device_address: "172.100.208.12".into(),
            collector_id: "bn02404-replay-test".into(),
            release_build: Some(ARTIFACT.into()),
            acquisition_started_at_ns: started_ns,
            acquisition_classification: "genuine_fixture_replay_not_live_ssh".into(),
        },
    )
    .expect("production envelope assembly must produce persistable evidence")
}

fn production_evidence_with_outputs(
    started_ns: i64,
    neighbor_output: &str,
    mac_output: &str,
    vlan_output: &str,
) -> StoredFrrInterfaceShadowObservation {
    production_evidence_from_raw(
        production_raw_envelope(started_ns, neighbor_output, mac_output, vlan_output),
        started_ns,
    )
}

fn empty_l2_outputs() -> (&'static str, &'static str, &'static str) {
    (
        include_str!("fixtures/bn024/l2-vertical/frr2-ip-neigh-show.txt"),
        include_str!("fixtures/bn024/l2-vertical/frr2-bridge-fdb-show.txt"),
        include_str!("fixtures/bn024/l2-vertical/frr2-bridge-c-vlan-show.txt"),
    )
}

fn populated_l2_outputs() -> (&'static str, &'static str, &'static str) {
    (
        include_str!("fixtures/bn024/neighbor/cumulus-spine02-ip-neigh-show.txt"),
        include_str!("fixtures/bn024/neighbor/cumulus-leaf01-bridge-fdb-show.txt"),
        // No populated genuine Linux `bridge -c vlan show` fixture is currently
        // available. This small Linux-shaped populated VLAN context is
        // explicitly synthetic integration input; it is never represented as
        // live or genuine device evidence in the final qualification.
        "port              vlan-id  \neth0              10 PVID Egress Untagged\neth1              20\n",
    )
}

fn evidence() -> StoredFrrInterfaceShadowObservation {
    let (neighbor, mac, vlan) = empty_l2_outputs();
    production_evidence_with_outputs(bonsai::graph::common::now_ns(), neighbor, mac, vlan)
}

fn fresh_evidence() -> StoredFrrInterfaceShadowObservation {
    evidence()
}

fn resealed_fact_with_namespace(fact: &mut Value, namespace: &str) {
    let mut typed: bonsai::typed_facts::TypedFact =
        serde_json::from_value(fact.clone()).expect("decode typed fact");
    typed.subject.namespace = namespace.into();
    typed.seal();
    *fact = serde_json::to_value(typed).expect("encode resealed typed fact");
}

fn sha256_text(text: &str) -> String {
    hex::encode(sha2::Sha256::digest(text.as_bytes()))
}

/// Offline vertical-slice evidence shape built by running the real native
/// normalizers over the genuine captured Linux command outputs in
/// `tests/fixtures/bn024/l2-vertical`. The interface facts come from the
/// committed authorized-lab observation; the added family facts are generated
/// here rather than hand-written so schema validation and provenance remain
/// executable.
fn l2_vertical_evidence() -> StoredFrrInterfaceShadowObservation {
    evidence()
}

fn populated_l2_evidence() -> StoredFrrInterfaceShadowObservation {
    let (neighbor, mac, vlan) = populated_l2_outputs();
    production_evidence_with_outputs(bonsai::graph::common::now_ns(), neighbor, mac, vlan)
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
    let observation = fresh_evidence();
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
    assert_eq!(body["evidence"]["facts_emitted"], 6);
    assert_eq!(body["evidence"]["facts_validated"], 6);
    assert_eq!(
        body["evidence"]["interface_output_sha256"],
        "2909f0cbc8247d0ca94a2d7b844c8d5f3676f51371a9583b1314dc98a358f44f"
    );
    assert!(body["evidence"].get("raw_interface_json").is_none());
    assert!(body["evidence"].get("interface_diagnostics").is_none());
    assert!(body["evidence"].get("host_key").is_none());
    assert_eq!(
        body["evidence"]["interpretation_outcome"],
        "complete_usable"
    );
    assert_eq!(body["evidence"]["usable_entity_facts"], 6);
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

    let (neighbor, mac, vlan) = empty_l2_outputs();
    let mut aged = production_evidence_with_outputs(
        observation.capture_started_at_ns + 10,
        neighbor,
        mac,
        vlan,
    );
    // Operator freshness is deliberately evaluated from the aggregate retained
    // observation timestamp. This test only ages that presentation clock; the
    // typed facts and per-family acquisition times remain unchanged evidence.
    aged.captured_at_ns -= 2 * 3600 * 1_000_000_000;
    persist_observation(&h.store, &aged)
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
    let (neighbor, mac, vlan) = empty_l2_outputs();
    let older = production_evidence_with_outputs(
        observation.capture_started_at_ns.saturating_sub(1),
        neighbor,
        mac,
        vlan,
    );
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
async fn invalid_stored_observation_is_not_presented_as_missing_evidence() {
    let h = harness().await;
    let observation = fresh_evidence();
    let address = observation.device_address.clone();
    let device = target(&address, None, None);
    h.registry
        .add_device_with_audit(device.clone(), "bn02404-test", "test_add")
        .expect("register target");
    bonsai::enrichment::frr_interfaces::record_acquisition_failure(
        &h.store,
        &device,
        observation.capture_started_at_ns,
        "capture_failed",
        "prior failed attempt",
    )
    .await
    .expect("record acquisition state");
    h.store
        .upsert_config_item(ConfigItemRecord {
            id: observation.observation_id.clone(),
            config_class: "native_interface_shadow_observation".into(),
            vendor: "frr".into(),
            name: observation.device_id.clone(),
            version: "2".into(),
            content_json: "{\"corrupted\":true}".into(),
            enabled: true,
            created_by: "bn02404-test".into(),
        })
        .await
        .expect("seed corrupted stored observation");
    let viewer = token(&h.store, "bn024-viewer", "viewer").await;
    let uri = format!("/api/devices/{address}/native-interfaces/frr");
    let (status, body) = request(&h.router, "GET", &uri, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["reason_code"], "stored_shadow_observation_invalid");
}

#[tokio::test]
async fn operator_surface_exposes_multifamily_coverage_and_bounded_l2_evidence() {
    let h = harness().await;
    let observation = l2_vertical_evidence();
    let address = observation.device_address.clone();
    h.registry
        .add_device_with_audit(target(&address, None, None), "bn02404-test", "test_add")
        .expect("register target");
    persist_observation(&h.store, &observation)
        .await
        .expect("persist multi-family evidence");
    let viewer = token(&h.store, "bn024-viewer", "viewer").await;
    let uri = format!("/api/devices/{address}/native-interfaces/frr");
    let (status, body) = request(&h.router, "GET", &uri, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(body["evidence"]["facts_emitted"], 6);
    assert_eq!(body["evidence"]["facts_validated"], 6);
    assert!(body["evidence"]["neighbor_output_sha256"].is_string());
    assert!(body["evidence"]["mac_output_sha256"].is_string());
    assert!(body["evidence"]["vlan_output_sha256"].is_string());
    assert_eq!(body["evidence"]["raw_neighbor_output_retained"], true);
    assert_eq!(body["evidence"]["raw_mac_output_retained"], true);
    assert_eq!(body["evidence"]["raw_vlan_output_retained"], true);
    assert!(body["evidence"].get("raw_neighbor_output").is_none());
    assert!(body["evidence"].get("raw_mac_output").is_none());
    assert!(body["evidence"].get("raw_vlan_output").is_none());

    let families = body["evidence"]["normalization_report"]["feature_families"]
        .as_array()
        .expect("feature-family coverage array");
    assert_eq!(families.len(), 4);
    let family = |name: &str| {
        families
            .iter()
            .find(|entry| entry["feature"] == name)
            .unwrap_or_else(|| panic!("missing feature family {name}"))
    };
    let arp = family("arpnd.linux_ip_neigh");
    assert_eq!(arp["facts_emitted"], 0);
    assert_eq!(arp["coverage"]["covered_fact_kind"], "arpnd.neighbor");
    assert_eq!(arp["coverage"]["positive_empty_population"], true);
    assert_eq!(arp["coverage"]["population_coverage"], "complete");
    let mac = family("mac.linux_bridge_fdb");
    assert_eq!(mac["facts_emitted"], 2);
    assert_eq!(mac["coverage"]["covered_fact_kind"], "mac.forwarding");
    assert_eq!(mac["coverage"]["positive_empty_population"], false);
    let vlan = family("vlan.linux_bridge_vlan");
    assert_eq!(vlan["facts_emitted"], 0);
    assert_eq!(vlan["coverage"]["covered_fact_kind"], "vlan.bridge_domain");
    assert_eq!(vlan["coverage"]["positive_empty_population"], true);

    let fact_kinds: Vec<_> = body["evidence"]["facts"]
        .as_array()
        .expect("facts array")
        .iter()
        .filter_map(|fact| fact["fact_kind"].as_str())
        .collect();
    assert!(fact_kinds.contains(&"interface.state"));
    assert!(fact_kinds.contains(&"mac.forwarding"));
    // Positive-empty ARP/VLAN populations are explicit coverage, not fabricated
    // empty entity rows and not evidence about each other's tables.
    assert!(!fact_kinds.contains(&"arpnd.neighbor"));
    assert!(!fact_kinds.contains(&"vlan.bridge_domain"));

    // The bounded operator assessment invokes the existing forwarding-domain
    // and interface-relationship analyzers, but the genuine Linux fixture is
    // ARP/VLAN positive-empty and does not establish a bridge domain. It must
    // therefore expose explicit unresolved prerequisites rather than invent a
    // positive neighbor correlation.
    let assessment = &body["l2_assessment"];
    assert_eq!(assessment["state"], "unresolved_prerequisites");
    assert_eq!(assessment["prerequisite_evidence"]["arpnd_facts"], 0);
    assert_eq!(assessment["prerequisite_evidence"]["mac_facts"], 2);
    assert_eq!(
        assessment["prerequisite_evidence"]["derived_domain_evidence"],
        0
    );
    assert!(assessment["forwarding_domain_derivation"]["report"].is_object());
    assert!(assessment["interface_relationships"].is_object());
    assert_eq!(assessment["authoritative_projection"], "none");
}

#[tokio::test]
async fn nonempty_vlan_facts_survive_production_construction_and_persistence() {
    let h = harness().await;
    let observation = populated_l2_evidence();
    let vlan_facts = observation
        .facts
        .iter()
        .filter(|fact| fact["fact_kind"] == "vlan.bridge_domain")
        .count();
    assert!(
        vlan_facts > 0,
        "populated Linux VLAN fixture must emit typed facts"
    );
    persist_observation(&h.store, &observation)
        .await
        .expect("nonempty multifamily VLAN evidence must pass persistence validation");
    let retained = latest_observation(&h.store, &target(&observation.device_address, None, None))
        .await
        .expect("retained populated observation");
    assert!(
        retained
            .facts
            .iter()
            .any(|fact| fact["fact_kind"] == "vlan.bridge_domain"
                && fact["subject"]["namespace"] == "forwarding")
    );

    let mut wrong_vlan_namespace = observation.clone();
    for fact in &mut wrong_vlan_namespace.facts {
        if fact["fact_kind"] == "vlan.bridge_domain" {
            resealed_fact_with_namespace(fact, "interface");
        }
    }
    let err = persist_observation(&h.store, &wrong_vlan_namespace)
        .await
        .expect_err("VLAN facts must not be admitted in the interface namespace");
    assert!(err.to_string().contains("identity mismatch"), "{err}");

    let mut wrong_interface_namespace = observation.clone();
    for fact in &mut wrong_interface_namespace.facts {
        if fact["fact_kind"] == "interface.state" {
            resealed_fact_with_namespace(fact, "forwarding");
        }
    }
    let err = persist_observation(&h.store, &wrong_interface_namespace)
        .await
        .expect_err("interface facts must not be admitted in the forwarding namespace");
    assert!(err.to_string().contains("identity mismatch"), "{err}");
}

#[tokio::test]
async fn tampered_family_coverage_and_raw_output_are_rejected() {
    let h = harness().await;
    let observation = populated_l2_evidence();

    let mut wrong_device_coverage = observation.clone();
    let vlan = wrong_device_coverage
        .family_states
        .iter_mut()
        .find(|state| state.fact_kind == "vlan.bridge_domain")
        .expect("vlan state");
    vlan.retained
        .as_mut()
        .expect("retained vlan")
        .coverage
        .as_mut()
        .expect("coverage")["device_id"] = serde_json::Value::String("other-device".into());
    let err = persist_observation(&h.store, &wrong_device_coverage)
        .await
        .expect_err("coverage bound to another device must be rejected");
    assert!(err.to_string().contains("coverage identity"), "{err}");

    let mut wrong_digest = observation.clone();
    wrong_digest.vlan_output_sha256 = Some("0".repeat(64));
    let err = persist_observation(&h.store, &wrong_digest)
        .await
        .expect_err("raw evidence digest tampering must be rejected");
    assert!(
        err.to_string().contains("raw-output identity mismatch"),
        "{err}"
    );

    let mut false_complete = observation.clone();
    let mac = false_complete
        .family_states
        .iter_mut()
        .find(|state| state.fact_kind == "mac.forwarding")
        .expect("mac state");
    let retained = mac.retained.as_mut().expect("retained mac");
    retained.retention_truncated = true;
    retained.facts_omitted_by_retention = 1;
    let err = persist_observation(&h.store, &false_complete)
        .await
        .expect_err("retention-truncated evidence cannot keep complete coverage");
    assert!(err.to_string().contains("cannot claim a complete"), "{err}");
}

#[tokio::test]
async fn positive_empty_interface_population_is_coverage_not_a_fabricated_fact() {
    let h = harness().await;
    let (neighbor, mac, vlan) = empty_l2_outputs();
    let started = bonsai::graph::common::now_ns();
    let mut raw = production_raw_envelope(started, neighbor, mac, vlan);
    // Synthetic integration input in the exact FRR object shape: an empty
    // top-level interface map. It is not represented as genuine device
    // evidence; it exercises the positive-empty lifecycle boundary.
    let empty_interfaces = "{}";
    let interface = raw.commands.get_mut("interfaces").expect("interface slot");
    interface.output = Some(empty_interfaces.into());
    interface.output_bytes = Some(empty_interfaces.len());
    interface.output_sha256 = Some(sha256_text(empty_interfaces));
    let observation = production_evidence_from_raw(raw, started);
    let interface_state = observation
        .family_states
        .iter()
        .find(|state| state.fact_kind == "interface.state")
        .expect("interface state");
    let retained = interface_state
        .retained
        .as_ref()
        .expect("positive-empty interface evidence");
    assert!(retained.positive_empty_population);
    assert_eq!(retained.facts_retained, 0);
    assert!(
        !observation
            .facts
            .iter()
            .any(|fact| fact["fact_kind"] == "interface.state"),
        "positive-empty interface coverage must not fabricate an entity/status fact"
    );
    persist_observation(&h.store, &observation)
        .await
        .expect("positive-empty interface evidence must remain persistable bounded evidence");
}

#[tokio::test]
async fn failed_family_preserves_unrelated_last_good_and_positive_empty_can_replace_populated_vlan()
{
    let h = harness().await;
    let (neighbor, mac, vlan) = populated_l2_outputs();
    let started = bonsai::graph::common::now_ns();
    let first = production_evidence_with_outputs(started, neighbor, mac, vlan);
    let initial_arp_facts = first
        .facts
        .iter()
        .filter(|fact| fact["fact_kind"] == "arpnd.neighbor")
        .count();
    assert!(initial_arp_facts > 0);
    persist_observation(&h.store, &first)
        .await
        .expect("persist initial populated multifamily evidence");

    // ARP fails in the newer batch while interface/MAC/VLAN remain valid.
    let (empty_neighbor, empty_mac, empty_vlan) = empty_l2_outputs();
    let _ = (empty_neighbor, empty_mac);
    let mut raw = production_raw_envelope(started + 1_000, neighbor, mac, vlan);
    let arp = raw.commands.get_mut("arpnd").expect("arp slot");
    arp.outcome = "failed".into();
    arp.output = None;
    arp.output_bytes = None;
    arp.output_sha256 = None;
    arp.detail = Some("fixed read-only command failed".into());
    let second = production_evidence_from_raw(raw, started + 1_000);
    persist_observation(&h.store, &second)
        .await
        .expect("partial multifamily batch remains bounded usable evidence");
    let retained = latest_observation(&h.store, &target(&first.device_address, None, None))
        .await
        .expect("merged observation");
    let retained_arp_facts = retained
        .facts
        .iter()
        .filter(|fact| fact["fact_kind"] == "arpnd.neighbor")
        .count();
    assert_eq!(retained_arp_facts, initial_arp_facts);
    let arp_state = retained
        .family_states
        .iter()
        .find(|state| state.fact_kind == "arpnd.neighbor")
        .expect("arp family state");
    assert_eq!(arp_state.latest_attempt.outcome, "failed");
    assert_eq!(
        arp_state
            .retained
            .as_ref()
            .expect("last-good arp")
            .evidence_state,
        "stale_last_usable"
    );
    assert!(
        retained
            .facts
            .iter()
            .any(|fact| fact["fact_kind"] == "mac.forwarding")
    );

    // A later genuinely complete empty VLAN capture is valid replacement
    // evidence within the same scope and can supersede the populated VLAN set.
    let third = production_evidence_with_outputs(started + 2_000, neighbor, mac, empty_vlan);
    persist_observation(&h.store, &third)
        .await
        .expect("positive empty VLAN replacement must persist");
    let retained = latest_observation(&h.store, &target(&first.device_address, None, None))
        .await
        .expect("merged positive-empty observation");
    assert!(
        !retained
            .facts
            .iter()
            .any(|fact| fact["fact_kind"] == "vlan.bridge_domain")
    );
    let vlan_state = retained
        .family_states
        .iter()
        .find(|state| state.fact_kind == "vlan.bridge_domain")
        .expect("vlan family state");
    let vlan_retained = vlan_state
        .retained
        .as_ref()
        .expect("positive empty vlan state");
    assert_eq!(vlan_retained.evidence_state, "current");
    assert!(vlan_retained.positive_empty_population);
    assert_eq!(vlan_retained.facts_retained, 0);
}

#[tokio::test]
async fn per_family_retention_truncation_is_explicit_and_does_not_starve_l2_families() {
    let h = harness().await;
    let base: serde_json::Map<String, Value> = serde_json::from_str(include_str!(
        "fixtures/bn024/l2-vertical/frr2-show-interface.json"
    ))
    .expect("interface fixture object");
    let template = base.get("eth0").expect("eth0 template").clone();
    let mut large = serde_json::Map::new();
    for index in 0..100u64 {
        let mut row = template.clone();
        row["index"] = serde_json::json!(index + 2);
        row["hardwareAddress"] = serde_json::json!(format!("02:00:00:00:00:{index:02x}"));
        row["ipAddresses"] = serde_json::json!([]);
        large.insert(format!("eth{index}"), row);
    }
    let large_interface = Value::Object(large).to_string();
    let (neighbor, mac, vlan) = populated_l2_outputs();
    let started = bonsai::graph::common::now_ns();
    let mut raw = production_raw_envelope(started, neighbor, mac, vlan);
    let interface = raw.commands.get_mut("interfaces").expect("interface slot");
    interface.output = Some(large_interface.clone());
    interface.output_bytes = Some(large_interface.len());
    interface.output_sha256 = Some(sha256_text(&large_interface));
    let observation = production_evidence_from_raw(raw, started);

    let interface_state = observation
        .family_states
        .iter()
        .find(|state| state.fact_kind == "interface.state")
        .expect("interface family");
    let interface_retained = interface_state
        .retained
        .as_ref()
        .expect("interface retained");
    assert_eq!(interface_retained.facts_retained, 64);
    assert!(interface_retained.retention_truncated);
    assert!(interface_retained.facts_omitted_by_retention > 0);
    let coverage = interface_retained
        .coverage
        .as_ref()
        .expect("interface coverage");
    assert_eq!(coverage["population_coverage"], "partial");
    assert!(observation.fact_retention_truncated);
    assert!(
        observation
            .facts
            .iter()
            .any(|fact| fact["fact_kind"] == "mac.forwarding"),
        "per-family allocation must prevent interfaces starving MAC evidence"
    );
    assert!(
        observation
            .facts
            .iter()
            .any(|fact| fact["fact_kind"] == "vlan.bridge_domain"),
        "per-family allocation must prevent interfaces starving VLAN evidence"
    );
    persist_observation(&h.store, &observation)
        .await
        .expect("legitimately truncated multifamily evidence remains valid bounded evidence");
}

#[tokio::test]
async fn interface_failure_does_not_discard_independently_valid_l2_families() {
    let h = harness().await;
    let (neighbor, mac, vlan) = populated_l2_outputs();
    let started = bonsai::graph::common::now_ns();
    let mut raw = production_raw_envelope(started, neighbor, mac, vlan);
    let interface = raw.commands.get_mut("interfaces").expect("interface slot");
    interface.outcome = "unsupported".into();
    interface.output = None;
    interface.output_bytes = None;
    interface.output_sha256 = None;
    interface.detail = Some("interface command unsupported".into());
    let observation = production_evidence_from_raw(raw, started);
    assert_eq!(observation.interpretation_outcome, "partial_usable");
    persist_observation(&h.store, &observation)
        .await
        .expect("valid independent L2 families must persist despite interface failure");
    let retained = latest_observation(&h.store, &target(&observation.device_address, None, None))
        .await
        .expect("retained independent family evidence");
    assert!(
        !retained
            .facts
            .iter()
            .any(|fact| fact["fact_kind"] == "interface.state")
    );
    assert!(
        retained
            .facts
            .iter()
            .any(|fact| fact["fact_kind"] == "mac.forwarding")
    );
    assert!(
        retained
            .family_states
            .iter()
            .any(|state| state.fact_kind == "interface.state" && state.retained.is_none())
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
    assert!(err.to_string().contains("identity mismatch"), "{err}");
}

#[tokio::test]
async fn concurrent_persistence_admits_newer_epoch_regardless_of_completion_order() {
    let h = harness().await;
    let base_started = bonsai::graph::common::now_ns();
    let (neighbor, mac, vlan) = empty_l2_outputs();
    let older = production_evidence_with_outputs(base_started, neighbor, mac, vlan);
    let newer = production_evidence_with_outputs(base_started + 1_000, neighbor, mac, vlan);

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
    let (neighbor, mac, vlan) = empty_l2_outputs();
    let success = production_evidence_with_outputs(
        bonsai::graph::common::now_ns() - 100,
        neighbor,
        mac,
        vlan,
    );
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
    let mut older_success =
        production_evidence_with_outputs(success.capture_started_at_ns + 5, neighbor, mac, vlan);
    older_success.captured_at_ns = newer_failure_started + 20;
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

    // A later batch whose independent family commands all failed is genuine
    // acquisition-attempt evidence, but it has no replacement populations and
    // therefore cannot erase the retained last-usable observation.
    let started = observation.capture_started_at_ns + 1_000;
    let (neighbor, mac, vlan) = empty_l2_outputs();
    let mut raw = production_raw_envelope(started, neighbor, mac, vlan);
    for key in ["interfaces", "arpnd", "mac", "vlan"] {
        let command = raw.commands.get_mut(key).expect("command slot");
        command.outcome = "failed".into();
        command.output = None;
        command.output_bytes = None;
        command.output_sha256 = None;
        command.detail = Some("fixed read-only command failed".into());
    }
    let degraded = production_evidence_from_raw(raw, started);
    assert_eq!(degraded.interpretation_outcome, "no_usable_records");
    assert_eq!(degraded.facts.len(), 0);

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
        "capture_failed",
        "independent family command execution failed",
    )
    .await
    .expect("persist degraded attempt evidence");

    let retained = latest_observation(&h.store, &device)
        .await
        .expect("last usable evidence must survive degraded interpretation");
    assert_eq!(retained.session_epoch, observation.session_epoch);
    assert_eq!(retained.usable_entity_facts, 6);
    let state = bonsai::enrichment::frr_interfaces::acquisition_state(&h.store, &device)
        .await
        .expect("decode state")
        .expect("state");
    assert_eq!(state.last_attempt_outcome, "degraded");
    assert_eq!(
        state.last_interpretation_outcome.as_deref(),
        Some("no_usable_records")
    );
    assert_eq!(
        state.last_successful_observation_id.as_deref(),
        Some(observation.observation_id.as_str())
    );
}

#[tokio::test]
async fn removal_invalidates_evidence_without_permanently_poisoning_identity() {
    let h = harness().await;
    let observation = fresh_evidence();
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
    let (neighbor, mac, vlan) = empty_l2_outputs();
    let observation = production_evidence_with_outputs(
        observation.capture_started_at_ns + 1,
        neighbor,
        mac,
        vlan,
    );
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
    assert_eq!(body["result"]["facts_emitted"], 6);
    assert_eq!(body["result"]["facts_validated"], 6);
    assert_eq!(body["result"]["usable_entity_facts"], 6);
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
    assert_eq!(body["evidence"]["facts_validated"], 6);
    assert_eq!(body["evidence"]["network_namespace"], "default");
    assert_eq!(
        body["evidence"]["network_namespace_evidence"],
        "fixed verified SSH session in device default network namespace; no namespace override"
    );
}
