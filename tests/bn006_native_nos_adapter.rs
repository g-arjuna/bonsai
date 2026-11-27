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

use bonsai::remediation::{
    parse_frr_native_bgp_summary_json, parse_nokia_srl_native_bgp_neighbors,
};
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
    assert_eq!(n.session_state_semantics, "established");
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
    assert_eq!(n.advertised_capabilities_availability, "present");
    assert_eq!(n.received_capabilities_availability, "present");
    assert_eq!(n.received_afi_safi_availability, "present");
    assert_eq!(n.requested_afi_safi_presence, "present");
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
        starting.neighbors[0].session_state_semantics,
        "recognized_not_established"
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
fn bn006_absent_empty_and_malformed_lists_remain_distinguishable() {
    let mut raw: serde_json::Value = serde_json::from_str(NATIVE).unwrap();
    let n = &mut raw["network-instance"][0]["protocols"]["bgp"]["neighbor"][0];
    n.as_object_mut().unwrap().remove("advertised-capabilities");
    n["received-capabilities"] = serde_json::json!([]);
    n["received-afi-safi"] = serde_json::json!(7);
    n["import-policy"] = serde_json::json!([]);
    n.as_object_mut().unwrap().remove("export-policy");
    let parsed = parse_nokia_srl_native_bgp_neighbors(
        &raw.to_string(),
        "172.31.216.3",
        VERSION,
        "ipv4-unicast",
    )
    .unwrap();
    let n = &parsed.neighbors[0];
    assert_eq!(n.advertised_capabilities_availability, "absent");
    assert_eq!(n.received_capabilities_availability, "explicitly_empty");
    assert_eq!(n.received_afi_safi_availability, "malformed");
    assert_eq!(n.import_policies_availability, "explicitly_empty");
    assert_eq!(n.export_policies_availability, "absent");
    assert_eq!(parsed.parse_quality, ParseQualityState::SchemaMismatch);
}

#[test]
fn bn006_duplicate_json_keys_and_duplicate_requested_family_are_rejected_or_marked() {
    let duplicate_key = NATIVE.replacen(
        "\"peer-as\": 65002",
        "\"peer-as\": 65002, \"peer-as\": 65002",
        1,
    );
    assert!(
        parse_nokia_srl_native_bgp_neighbors(
            &duplicate_key,
            "172.31.216.3",
            VERSION,
            "ipv4-unicast"
        )
        .unwrap_err()
        .contains("duplicate JSON key")
    );

    let mut raw: serde_json::Value = serde_json::from_str(NATIVE).unwrap();
    let neighbor = &mut raw["network-instance"][0]["protocols"]["bgp"]["neighbor"][0];
    let family = neighbor["afi-safi"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["afi-safi-name"] == "ipv4-unicast")
        .unwrap()
        .clone();
    neighbor["afi-safi"].as_array_mut().unwrap().push(family);
    let parsed = parse_nokia_srl_native_bgp_neighbors(
        &raw.to_string(),
        "172.31.216.3",
        VERSION,
        "ipv4-unicast",
    )
    .unwrap();
    assert_eq!(
        parsed.neighbors[0].requested_afi_safi_presence,
        "contradictory_duplicate"
    );
    assert_eq!(parsed.neighbors[0].family_oper_state, None);
    assert_eq!(parsed.parse_quality, ParseQualityState::SchemaMismatch);
}

#[test]
fn bn006_genuine_frr_vrf_all_summary_is_multi_vrf_session_evidence() {
    // Genuine read-only vtysh evidence acquired from clab-bonsai-bn003-vrf-frr1
    // (FRR 10.6.1) on 2026-11-27. It is genuine source evidence even though the
    // observed sessions were not established at capture time.
    let raw = include_str!("fixtures/bn006_frr_10_6_1/bgp-vrf-all-summary.json");
    let parsed =
        parse_frr_native_bgp_summary_json(raw, "172.100.208.11", "10.6.1_git", "ipv4-unicast")
            .unwrap();
    assert_eq!(parsed.parser_id, "frr-native-bgp-summary");
    assert_eq!(parsed.neighbors.len(), 2);
    let default = parsed
        .neighbors
        .iter()
        .find(|n| n.vrf_name == "default")
        .unwrap();
    assert_eq!(default.neighbor_address.as_deref(), Some("10.100.1.1"));
    assert_eq!(default.peer_as, Some(65002));
    assert_eq!(default.local_as, Some(65001));
    assert_eq!(default.session_state.as_deref(), Some("Active"));
    assert_eq!(
        default.session_state_semantics,
        "recognized_not_established"
    );
    assert_eq!(default.peer_uptime.as_deref(), Some("never"));
    assert_eq!(default.received_routes, Some(0));
    assert_eq!(default.sent_routes, Some(0));
    assert_eq!(
        default.requested_afi_safi_presence,
        "present_summary_visibility"
    );
    assert_eq!(default.family_oper_state, None);

    let blue = parsed
        .neighbors
        .iter()
        .find(|n| n.vrf_name == "BLUE")
        .unwrap();
    assert_eq!(blue.neighbor_address.as_deref(), Some("10.100.2.1"));
    assert_eq!(blue.session_state.as_deref(), Some("Idle"));
    assert_eq!(blue.session_state_semantics, "recognized_not_established");
    assert!(parsed.missing_or_invalid_fields.is_empty());
}

#[test]
fn bn006_frr_summary_missing_state_and_counter_remain_unknown() {
    let mut raw: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/bn006_frr_10_6_1/bgp-vrf-all-summary.json"
    ))
    .unwrap();
    let peer = &mut raw["default"]["ipv4Unicast"]["peers"]["10.100.1.1"];
    peer.as_object_mut().unwrap().remove("state");
    peer.as_object_mut().unwrap().remove("pfxRcd");
    let parsed = parse_frr_native_bgp_summary_json(
        &raw.to_string(),
        "172.100.208.11",
        "10.6.1_git",
        "ipv4-unicast",
    )
    .unwrap();
    let neighbor = parsed
        .neighbors
        .iter()
        .find(|n| n.vrf_name == "default")
        .unwrap();
    assert_eq!(neighbor.session_state, None);
    assert_eq!(neighbor.session_state_semantics, "absent");
    assert_eq!(neighbor.received_routes, None);
    assert_eq!(parsed.parse_quality, ParseQualityState::SchemaMismatch);
}

#[test]
fn bn006_frr_native_sessions_use_shared_bmp_reconciliation_contract() {
    use bonsai::bgp_reconciliation::{
        BmpPeerEvidence, NativeBmpAgreement, NativeBmpReconciliationPolicy,
        reconcile_native_bgp_with_bmp,
    };
    let capture = parse_frr_native_bgp_summary_json(
        include_str!("fixtures/bn006_frr_10_6_1/bgp-vrf-all-summary.json"),
        "172.100.208.11",
        "10.6.1_git",
        "ipv4-unicast",
    )
    .unwrap();
    let now = bonsai::graph::common::now_ns();
    // Synthetic BMP control evidence: this validates semantic interoperability,
    // not live corroboration of the captured lab session.
    let row = BmpPeerEvidence {
        session_id: "bmp:frr-control".into(),
        device_address: "172.100.208.11".into(),
        peer_address: "10.100.1.1".into(),
        peer_as: Some(65002),
        session_state: "Active".into(),
        network_instance: String::new(),
        route_distinguisher: "0".into(),
        peer_type: Some(0),
        updated_at_ns: now - 1_000_000_000,
        evidence_received_at_ns: now - 1_000_000_000,
    };
    let out = reconcile_native_bgp_with_bmp(
        &capture,
        now - 1_000_000_000,
        &[row],
        now,
        NativeBmpReconciliationPolicy {
            max_observation_age_seconds: 300,
            max_capture_skew_seconds: 60,
        },
    );
    let default = out
        .assessments
        .iter()
        .find(|assessment| assessment.vrf_name == "default")
        .unwrap();
    assert_eq!(
        default.agreement,
        NativeBmpAgreement::CorroboratedNotEstablished
    );
    assert_eq!(out.corroborated_not_established, 1);
    assert!(
        out.assessments
            .iter()
            .any(|assessment| assessment.vrf_name == "BLUE"
                && assessment.agreement == NativeBmpAgreement::MissingBmpVisibility)
    );
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
    let now = bonsai::graph::common::now_ns();
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
    assert_eq!(rows.total_matching_rows, 1);
    assert_eq!(rows.returned_rows, 1);
    assert_eq!(rows.retrieval_omitted_rows, 0);
    assert_eq!(rows.rows[0].peer_address, "10.206.0.2");
    assert_eq!(rows.rows[0].peer_as, Some(65002));

    let native =
        parse_nokia_srl_native_bgp_neighbors(NATIVE, "172.31.216.3", VERSION, "ipv4-unicast")
            .unwrap();
    // as_of must be sampled after evidence receipt: comparing as-of a time
    // before the BMP evidence even arrived is correctly a future (non-current)
    // observation, never corroboration.
    let as_of = bonsai::graph::common::now_ns();
    let reconciliation = reconcile_native_bgp_with_bmp(
        &native,
        now,
        &rows.rows,
        as_of,
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
