//! BN-006: native NOS evidence and truthful adapter schema boundaries.
use bonsai::remediation::{ParseQualityState, parse_nokia_srl_bgp_neighbor_json};

fn complete_flat_sample() -> serde_json::Value {
    serde_json::json!({"software-version":"26.3.1-410", "network-instance":"default",
        "peer-address":"10.206.0.2", "peer-as":65002, "session-state":"established",
        "received-routes":0, "active-routes":0})
}

#[test]
fn bn006_incomplete_flat_records_are_not_reported_complete() {
    for field in [
        "peer-address",
        "peer-as",
        "session-state",
        "received-routes",
        "active-routes",
    ] {
        let mut sample = complete_flat_sample();
        sample.as_object_mut().unwrap().remove(field);
        let parsed = parse_nokia_srl_bgp_neighbor_json(&sample.to_string()).unwrap();
        assert_eq!(
            parsed.parse_quality,
            ParseQualityState::SchemaMismatch,
            "missing {field} must not fabricate complete telemetry"
        );
    }
}

#[test]
fn bn006_absent_version_is_unknown_not_assumed_24_7() {
    let mut sample = complete_flat_sample();
    sample.as_object_mut().unwrap().remove("software-version");
    let parsed = parse_nokia_srl_bgp_neighbor_json(&sample.to_string()).unwrap();
    assert_eq!(parsed.nos_version, "unknown");
}

#[test]
fn bn006_peer_as_overflow_is_not_wrapped_to_zero() {
    let mut sample = complete_flat_sample();
    sample["peer-as"] = serde_json::json!(4_294_967_296_u64);
    let parsed = parse_nokia_srl_bgp_neighbor_json(&sample.to_string()).unwrap();
    assert_eq!(parsed.parse_quality, ParseQualityState::SchemaMismatch);
}

use bonsai::remediation::parse_nokia_srl_native_bgp_neighbors;
const NATIVE: &str = include_str!("fixtures/bn006_srl_26_3_1/neighbors-established.json");
const STARTING: &str = include_str!("fixtures/bn006_srl_26_3_1/neighbors-starting.json");
const VERSION: &str = "v26.3.1-410-g98ed16c774c";

#[test]
fn bn006_live_native_capture_preserves_source_vrf_family_and_state() {
    let version: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/bn006_srl_26_3_1/system-information.json"
    ))
    .unwrap();
    assert_eq!(version["version"], VERSION);
    let parsed =
        parse_nokia_srl_native_bgp_neighbors(NATIVE, "172.31.216.3", VERSION, "ipv4-unicast")
            .unwrap();
    assert_eq!(
        parsed.raw_sample_sha256,
        "f1bae1632b3add6d2156d6f98754da9aa5907690c5e63171e26106659da8bf34"
    );
    assert_eq!(parsed.exporter_address, "172.31.216.3");
    assert_eq!(parsed.nos_version, VERSION);
    assert_eq!(parsed.neighbors.len(), 1);
    let n = &parsed.neighbors[0];
    assert_eq!(n.vrf_name, "default");
    assert_eq!(n.afi_safi, "ipv4-unicast");
    assert_eq!(n.neighbor_address.as_deref(), Some("10.206.0.2"));
    assert_eq!(n.peer_as, Some(65002));
    assert_eq!(n.session_state.as_deref(), Some("established"));
    assert_eq!(n.local_as, Some(65001));
    assert_eq!(n.peer_type.as_deref(), Some("ebgp"));
    assert_eq!(n.admin_state.as_deref(), Some("enable"));
    assert_eq!(n.peer_router_id.as_deref(), Some("192.0.2.2"));
    assert_eq!(
        n.last_established_at.as_deref(),
        Some("2026-10-30T05:50:00.800Z")
    );
    assert_eq!(n.established_transitions, Some(1));
    assert_eq!(
        n.advertised_capabilities,
        vec!["ROUTE_REFRESH", "4-OCTET_ASN", "MP_BGP", "GRACEFUL_RESTART"]
    );
    assert_eq!(n.received_capabilities, n.advertised_capabilities);
    assert_eq!(n.received_afi_safi, vec!["ipv4-unicast"]);
    assert_eq!(n.local_address.as_deref(), Some("10.206.0.1"));
    assert_eq!(n.family_admin_state.as_deref(), Some("enable"));
    assert_eq!(n.family_oper_state.as_deref(), Some("up"));
    assert_eq!((n.received_routes, n.active_routes), (Some(0), Some(0)));
    assert_eq!(
        (
            n.sent_routes,
            n.rejected_routes,
            n.suppressed_routes,
            n.withdrawn_error_routes
        ),
        (Some(0), Some(0), Some(0), Some(0))
    );
    assert_eq!(n.negotiated_hold_time_seconds, Some(90));
    assert_eq!(n.negotiated_keepalive_seconds, Some(30));
    assert_eq!(
        parsed.parse_quality,
        ParseQualityState::PartialUnknownFields
    );
    assert!(
        parsed
            .unparsed_fields
            .iter()
            .any(|path| path.ends_with("/peer-group"))
    );
    assert!(
        parsed
            .unparsed_fields
            .iter()
            .any(|path| path.ends_with("/transport/mtu-discovery")),
        "unmodelled nested transport evidence must remain explicit"
    );
    assert!(parsed.missing_or_invalid_fields.is_empty());
    let starting =
        parse_nokia_srl_native_bgp_neighbors(STARTING, "172.31.216.3", VERSION, "ipv4-unicast")
            .unwrap();
    assert_eq!(
        starting.neighbors[0].session_state.as_deref(),
        Some("active")
    );
    assert_eq!(
        starting.parse_quality,
        ParseQualityState::PartialUnknownFields
    );
}

#[test]
fn bn006_optional_family_metrics_are_unknown_not_fabricated_zero() {
    let mut raw: serde_json::Value = serde_json::from_str(NATIVE).unwrap();
    let family = raw["network-instance"][0]["protocols"]["bgp"]["neighbor"][0]["afi-safi"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|f| f["afi-safi-name"] == "ipv4-unicast")
        .unwrap();
    for field in [
        "sent-routes",
        "rejected-routes",
        "suppressed-routes",
        "received-routes-withdrawn-due-to-error",
    ] {
        family.as_object_mut().unwrap().remove(field);
    }
    let parsed = parse_nokia_srl_native_bgp_neighbors(
        &raw.to_string(),
        "172.31.216.3",
        VERSION,
        "ipv4-unicast",
    )
    .unwrap();
    let n = &parsed.neighbors[0];
    assert_eq!(
        (
            n.sent_routes,
            n.rejected_routes,
            n.suppressed_routes,
            n.withdrawn_error_routes
        ),
        (None, None, None, None)
    );
    assert_eq!((n.received_routes, n.active_routes), (Some(0), Some(0)));
}

#[test]
fn bn006_interface_scoped_ipv6_peer_identity_is_preserved() {
    // Synthetic mutation of the genuine SR Linux capture: it exercises the
    // scoped-address representation without claiming live link-local evidence.
    let mut raw: serde_json::Value = serde_json::from_str(NATIVE).unwrap();
    let n = &mut raw["network-instance"][0]["protocols"]["bgp"]["neighbor"][0];
    n["peer-address"] = serde_json::json!("fe80::2%ethernet-1/1");
    n["transport"]["local-address"] = serde_json::json!("fe80::1%ethernet-1/1");
    let parsed = parse_nokia_srl_native_bgp_neighbors(
        &raw.to_string(),
        "172.31.216.3",
        VERSION,
        "ipv4-unicast",
    )
    .unwrap();
    let n = &parsed.neighbors[0];
    assert_eq!(n.neighbor_address.as_deref(), Some("fe80::2"));
    assert_eq!(n.neighbor_scope_id.as_deref(), Some("ethernet-1/1"));
    assert_eq!(n.local_address.as_deref(), Some("fe80::1"));
    assert_eq!(n.local_address_scope_id.as_deref(), Some("ethernet-1/1"));

    let mut invalid: serde_json::Value = serde_json::from_str(NATIVE).unwrap();
    invalid["network-instance"][0]["protocols"]["bgp"]["neighbor"][0]["peer-address"] =
        serde_json::json!("10.206.0.2%ethernet-1/1");
    let invalid = parse_nokia_srl_native_bgp_neighbors(
        &invalid.to_string(),
        "172.31.216.3",
        VERSION,
        "ipv4-unicast",
    )
    .unwrap();
    assert_eq!(invalid.neighbors[0].neighbor_address, None);
    assert_eq!(invalid.parse_quality, ParseQualityState::SchemaMismatch);
}

#[test]
fn bn006_missing_native_metrics_and_overflow_remain_unknown() {
    let mut raw: serde_json::Value = serde_json::from_str(NATIVE).unwrap();
    let n = &mut raw["network-instance"][0]["protocols"]["bgp"]["neighbor"][0];
    n["peer-as"] = serde_json::json!(4_294_967_296_u64);
    let family = n["afi-safi"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|f| f["afi-safi-name"] == "ipv4-unicast")
        .unwrap();
    family.as_object_mut().unwrap().remove("active-routes");
    let parsed = parse_nokia_srl_native_bgp_neighbors(
        &raw.to_string(),
        "172.31.216.3",
        VERSION,
        "ipv4-unicast",
    )
    .unwrap();
    assert_eq!(parsed.parse_quality, ParseQualityState::SchemaMismatch);
    assert_eq!(parsed.neighbors[0].peer_as, None);
    assert_eq!(parsed.neighbors[0].active_routes, None);
    assert_eq!(
        parsed.neighbors[0].session_state.as_deref(),
        Some("established")
    );
    assert!(
        parsed
            .missing_or_invalid_fields
            .iter()
            .any(|f| f.ends_with("/active-routes"))
    );
}

#[test]
fn bn006_native_identity_policy_and_family_projection_do_not_collapse_context() {
    // Derived mutations of the live fixture are synthetic regression cases, not live VRF evidence.
    let mut raw: serde_json::Value = serde_json::from_str(NATIVE).unwrap();
    let mut blue = raw["network-instance"][0].clone();
    blue["name"] = serde_json::json!("blue");
    let n = &mut blue["protocols"]["bgp"]["neighbor"][0];
    n["import-policy"] = serde_json::json!(["IN-A", "IN-B"]);
    n["export-policy"] = serde_json::json!(["OUT"]);
    raw["network-instance"].as_array_mut().unwrap().push(blue);
    let parsed = parse_nokia_srl_native_bgp_neighbors(
        &raw.to_string(),
        "172.31.216.3",
        VERSION,
        "ipv4-unicast",
    )
    .unwrap();
    assert_eq!(parsed.neighbors.len(), 2);
    assert_eq!(parsed.neighbors[1].vrf_name, "blue");
    assert_eq!(parsed.neighbors[1].import_policies, vec!["IN-A", "IN-B"]);
    assert_eq!(parsed.neighbors[1].export_policies, vec!["OUT"]);
    let duplicate = raw["network-instance"][0]["protocols"]["bgp"]["neighbor"][0].clone();
    raw["network-instance"][0]["protocols"]["bgp"]["neighbor"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(
        parse_nokia_srl_native_bgp_neighbors(
            &raw.to_string(),
            "172.31.216.3",
            VERSION,
            "ipv4-unicast"
        )
        .unwrap_err()
        .contains("duplicate")
    );
    let unsupported =
        parse_nokia_srl_native_bgp_neighbors(NATIVE, "172.31.216.3", VERSION, "not-a-family")
            .unwrap();
    assert_eq!(unsupported.parse_quality, ParseQualityState::SchemaMismatch);
    assert_eq!(unsupported.neighbors[0].received_routes, None);
}

#[test]
fn bn006_native_empty_schema_mismatch_and_input_bounds_are_distinct() {
    let empty = r#"{"network-instance":[{"name":"default","protocols":{"bgp":{"neighbor":[]}}}]}"#;
    let parsed =
        parse_nokia_srl_native_bgp_neighbors(empty, "172.31.216.3", VERSION, "ipv4-unicast")
            .unwrap();
    assert_eq!(parsed.parse_quality, ParseQualityState::Complete);
    assert!(parsed.neighbors.is_empty());
    let malformed =
        parse_nokia_srl_native_bgp_neighbors("{}", "172.31.216.3", VERSION, "ipv4-unicast")
            .unwrap();
    assert_eq!(malformed.parse_quality, ParseQualityState::SchemaMismatch);
    assert!(
        parse_nokia_srl_native_bgp_neighbors("[", "172.31.216.3", VERSION, "ipv4-unicast").is_err()
    );
    assert!(parse_nokia_srl_native_bgp_neighbors(NATIVE, "", VERSION, "ipv4-unicast").is_err());
    assert!(
        parse_nokia_srl_native_bgp_neighbors(
            &" ".repeat(4 * 1024 * 1024 + 1),
            "172.31.216.3",
            VERSION,
            "ipv4-unicast"
        )
        .is_err()
    );
}

#[tokio::test]
async fn bn006_graph_bmp_rows_feed_bounded_native_reconciliation() {
    use bonsai::bgp_reconciliation::{
        NativeBmpAgreement, NativeBmpReconciliationPolicy, reconcile_native_bgp_with_bmp,
    };
    use bonsai::graph::GraphStore;
    use bonsai::streaming::bmp::BmpEvent;
    use bonsai::telemetry::TelemetryUpdate;

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("graph.db");
    let store = GraphStore::open(db_path.to_str().unwrap(), 128 * 1024 * 1024).expect("open graph");
    let now = 1_769_000_000_000_000_000_i64;
    let event = BmpEvent {
        timestamp_ns: now,
        collector_peer: "172.31.216.99:41234".into(),
        message_type: "peer_up".into(),
        peer_type: 0,
        peer_flags: 0,
        router_distinguisher: 0,
        router_address: "172.31.216.3".into(),
        peer_address: "10.206.0.2".into(),
        peer_as: 65002,
        peer_bgp_id: "192.0.2.2".into(),
        session_state: "established".into(),
        route_entries: Vec::new(),
        raw_len: 48,
        peer_down_reason: None,
        peer_down_reason_name: None,
        stats: Vec::new(),
        sys_name: None,
        sys_descr: None,
        termination_reason: None,
        termination_reason_name: None,
        rib_type: None,
        peer_up_info: None,
        init_admin_string: None,
        termination_message: None,
    };
    store
        .write(TelemetryUpdate {
            plan_id: String::new(),
            session_id: String::new(),
            source_protocol: String::new(),
            gnmi_path: Vec::new(),
            is_delete: false,
            collector_id: String::new(),
            timestamp_ns: now,
            target: "172.31.216.3:57400".into(),
            vendor: "nokia".into(),
            hostname: "srl-native".into(),
            role: "pe".into(),
            site: "lab".into(),
            path: "streaming/bmp/peer-up".into(),
            value: serde_json::to_value(&event).unwrap(),
        })
        .await
        .expect("write BMP peer evidence");

    let rows = store
        .bmp_bgp_sessions_for_device("172.31.216.3")
        .await
        .expect("read BMP evidence");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].peer_address, "10.206.0.2");
    assert_eq!(rows[0].peer_as, Some(65002));

    let native =
        parse_nokia_srl_native_bgp_neighbors(NATIVE, "172.31.216.3", VERSION, "ipv4-unicast")
            .unwrap();
    let reconciliation = reconcile_native_bgp_with_bmp(
        &native,
        now,
        &rows,
        now,
        NativeBmpReconciliationPolicy {
            max_observation_age_seconds: 300,
            max_capture_skew_seconds: 60,
        },
    );
    assert_eq!(reconciliation.corroborated_established, 1);
    assert_eq!(
        reconciliation.assessments[0].agreement,
        NativeBmpAgreement::CorroboratedEstablished
    );
}
