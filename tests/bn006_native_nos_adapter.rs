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
    assert_eq!((n.received_routes, n.active_routes), (Some(0), Some(0)));
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
