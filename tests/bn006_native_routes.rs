//! BN-006 follow-on: genuine FRR native route evidence and bounded recursive
//! next-hop semantics. Synthetic recursion controls are explicitly labelled;
//! they are not live qualification or verified traffic forwarding.

use bonsai::native_routes::{
    MAX_RESOLUTION_CANDIDATES, NativeRouteEntry, NativeRouteNextHop,
    parse_frr_native_route_capture, resolve_native_next_hop,
};

fn nh(address: Option<&str>, interface: &str, direct: bool) -> NativeRouteNextHop {
    NativeRouteNextHop {
        address: address.map(str::to_string),
        address_scope_id: None,
        interface_name: Some(interface.into()),
        interface_index: Some(1),
        active: Some(true),
        fib: Some(true),
        directly_connected: Some(direct),
        weight: Some(1),
        unreachable: None,
        reject: None,
        admin_prohibited: None,
        blackhole: None,
        missing_or_invalid_fields: Vec::new(),
    }
}

fn blackhole_nh() -> NativeRouteNextHop {
    NativeRouteNextHop {
        address: None,
        address_scope_id: None,
        interface_name: None,
        interface_index: None,
        active: Some(true),
        fib: Some(true),
        directly_connected: None,
        weight: Some(1),
        unreachable: Some(true),
        reject: None,
        admin_prohibited: None,
        blackhole: Some(true),
        missing_or_invalid_fields: Vec::new(),
    }
}

fn afi(prefix: &str) -> &'static str {
    if prefix.contains(':') {
        "ipv6-unicast"
    } else {
        "ipv4-unicast"
    }
}

fn route_with_selection(
    vrf: &str,
    prefix: &str,
    protocol: &str,
    next_hops: Vec<NativeRouteNextHop>,
    selected: Option<bool>,
    destination_selected: Option<bool>,
) -> NativeRouteEntry {
    NativeRouteEntry {
        vrf_name: vrf.into(),
        afi_safi: afi(prefix).into(),
        destination_prefix: prefix.into(),
        protocol: Some(protocol.into()),
        distance: Some(0),
        metric: Some(0),
        selected,
        destination_selected,
        installed: Some(true),
        table: Some(254),
        uptime: None,
        next_hops,
        missing_or_invalid_fields: Vec::new(),
        unparsed_fields: Vec::new(),
    }
}

fn route(
    vrf: &str,
    prefix: &str,
    protocol: &str,
    next_hops: Vec<NativeRouteNextHop>,
) -> NativeRouteEntry {
    route_with_selection(vrf, prefix, protocol, next_hops, Some(true), Some(true))
}

fn selected_fixture_routes() -> Vec<NativeRouteEntry> {
    vec![
        route(
            "default",
            "10.0.0.0/24",
            "bgp",
            vec![
                nh(Some("192.0.2.1"), "eth0", false),
                nh(Some("198.51.100.1"), "eth1", false),
            ],
        ),
        route(
            "default",
            "192.0.2.0/24",
            "connected",
            vec![nh(None, "eth0", true)],
        ),
        route(
            "default",
            "198.51.100.0/24",
            "connected",
            vec![nh(None, "eth1", true)],
        ),
    ]
}

#[test]
fn genuine_frr_route_capture_preserves_selection_installation_and_fib_distinctions() {
    let parsed = parse_frr_native_route_capture(
        include_str!("fixtures/bn006_frr_10_6_1/ip-route-vrf-all.json"),
        "172.100.208.11",
    )
    .unwrap();
    assert_eq!(parsed.parser_id, "frr-native-ip-route");
    assert_eq!(
        parsed.raw_sample_sha256,
        "8ef50939156e46a329bb27f96c891aaf565cad635f8b4338b9d2064695956238"
    );
    assert_eq!(parsed.routes.len(), 3);
    let default = parsed
        .routes
        .iter()
        .find(|route| route.destination_prefix == "0.0.0.0/0")
        .unwrap();
    assert_eq!(default.vrf_name, "default");
    assert_eq!(default.protocol.as_deref(), Some("kernel"));
    assert_eq!(default.selected, Some(true));
    assert_eq!(default.destination_selected, Some(true));
    assert_eq!(default.installed, Some(true));
    assert_eq!(default.distance, Some(0));
    assert_eq!(default.next_hops.len(), 1);
    assert_eq!(
        default.next_hops[0].address.as_deref(),
        Some("172.100.208.1")
    );
    assert_eq!(default.next_hops[0].interface_name.as_deref(), Some("eth0"));
    assert_eq!(default.next_hops[0].fib, Some(true));
    assert_eq!(default.next_hops[0].directly_connected, None);

    let connected = parsed
        .routes
        .iter()
        .find(|route| route.destination_prefix == "172.100.208.0/24")
        .unwrap();
    assert_eq!(connected.protocol.as_deref(), Some("connected"));
    assert_eq!(connected.next_hops[0].directly_connected, Some(true));
    assert!(parsed.missing_or_invalid_fields.is_empty());

    let resolved = resolve_native_next_hop(&parsed.routes, "default", "172.100.208.1");
    assert_eq!(resolved.resolution_state, "resolved");
    assert_eq!(resolved.terminal_interfaces, vec!["eth0"]);
    assert_eq!(
        resolved.forwarding_verification, "not_established_by_native_route_evidence",
        "installed/FIB indications remain evidence and never become verified traffic forwarding"
    );
}

#[test]
fn invalid_and_noncanonical_prefixes_are_not_canonical_resolution_evidence() {
    let routes = vec![
        route(
            "default",
            "10.0.0.1/24",
            "kernel",
            vec![nh(None, "eth-noncanonical", true)],
        ),
        route(
            "default",
            "not-a-prefix/24",
            "kernel",
            vec![nh(None, "eth-invalid", true)],
        ),
        route(
            "default",
            "0.0.0.0/0",
            "kernel",
            vec![nh(None, "eth-default", true)],
        ),
    ];
    let out = resolve_native_next_hop(&routes, "default", "10.0.0.9");
    assert_eq!(out.resolution_state, "resolved");
    assert_eq!(out.terminal_interfaces, vec!["eth-default"]);
    assert_eq!(out.resolution_chain, vec!["0.0.0.0/0"]);

    let raw = r#"{"default":{"10.0.0.1/24":[{"prefix":"10.0.0.1/24","protocol":"kernel","vrfName":"default","selected":true,"destSelected":true,"installed":true,"distance":0,"metric":0,"nexthops":[{"directlyConnected":true,"interfaceName":"eth0"}]}]}}"#;
    let parsed = parse_frr_native_route_capture(raw, "192.0.2.1").unwrap();
    assert!(
        parsed
            .missing_or_invalid_fields
            .iter()
            .any(|field| field.ends_with("/prefix:noncanonical-host-bits")),
        "host-bit-set prefix must remain noncanonical evidence: {parsed:?}"
    );
}

#[test]
fn recursive_next_hop_resolves_directly_connected_and_multiple_levels_same_vrf() {
    let routes = vec![
        route(
            "default",
            "10.0.0.0/24",
            "connected",
            vec![nh(None, "eth1", true)],
        ),
        route(
            "default",
            "0.0.0.0/0",
            "kernel",
            vec![nh(Some("10.0.0.1"), "eth0", false)],
        ),
        route(
            "BLUE",
            "10.0.0.1/32",
            "kernel",
            vec![nh(Some("192.0.2.1"), "eth9", false)],
        ),
    ];
    let resolved = resolve_native_next_hop(&routes, "default", "10.0.0.1");
    assert_eq!(resolved.resolution_state, "resolved");
    assert_eq!(resolved.terminal_interfaces, vec!["eth1"]);
    assert_eq!(resolved.resolution_chain, vec!["10.0.0.0/24"]);
    assert_eq!(resolved.depth, 0);

    let multilevel = vec![
        route(
            "default",
            "192.0.2.0/24",
            "bgp",
            vec![nh(Some("198.51.100.1"), "eth0", false)],
        ),
        route(
            "default",
            "198.51.100.0/24",
            "static",
            vec![nh(Some("203.0.113.1"), "eth0", false)],
        ),
        route(
            "default",
            "203.0.113.0/24",
            "connected",
            vec![nh(None, "eth8", true)],
        ),
    ];
    let out = resolve_native_next_hop(&multilevel, "default", "192.0.2.9");
    assert_eq!(out.resolution_state, "resolved");
    assert_eq!(out.terminal_interfaces, vec!["eth8"]);
    assert_eq!(out.depth, 2);
    assert_eq!(
        out.branches[0].resolution_chain,
        vec!["192.0.2.0/24", "198.51.100.0/24", "203.0.113.0/24"]
    );
}

#[test]
fn longest_prefix_matching_covers_ipv4_ipv6_more_specific_and_default_fallback() {
    let routes = vec![
        route(
            "default",
            "0.0.0.0/0",
            "kernel",
            vec![nh(None, "eth4", true)],
        ),
        route(
            "default",
            "10.0.0.0/8",
            "kernel",
            vec![nh(None, "eth8", true)],
        ),
        route(
            "default",
            "10.0.1.0/24",
            "kernel",
            vec![nh(None, "eth24", true)],
        ),
        route("default", "::/0", "kernel", vec![nh(None, "ethv0", true)]),
        route(
            "default",
            "2001:db8::/32",
            "kernel",
            vec![nh(None, "ethv32", true)],
        ),
    ];
    let v4_more_specific = resolve_native_next_hop(&routes, "default", "10.0.1.9");
    assert_eq!(v4_more_specific.terminal_interfaces, vec!["eth24"]);
    let v4_less_specific = resolve_native_next_hop(&routes, "default", "10.1.1.1");
    assert_eq!(v4_less_specific.terminal_interfaces, vec!["eth8"]);
    let v4_default = resolve_native_next_hop(&routes, "default", "192.0.2.1");
    assert_eq!(v4_default.terminal_interfaces, vec!["eth4"]);
    let v6_more_specific = resolve_native_next_hop(&routes, "default", "2001:db8::1");
    assert_eq!(v6_more_specific.terminal_interfaces, vec!["ethv32"]);
    let v6_default = resolve_native_next_hop(&routes, "default", "2001:db9::1");
    assert_eq!(v6_default.terminal_interfaces, vec!["ethv0"]);
}

#[test]
fn unselected_or_unknown_longest_prefix_never_becomes_forwarding_candidate() {
    let explicit = vec![
        route_with_selection(
            "default",
            "10.0.0.0/24",
            "bgp",
            vec![nh(None, "eth-unselected", true)],
            Some(false),
            Some(true),
        ),
        route(
            "default",
            "0.0.0.0/0",
            "kernel",
            vec![nh(None, "eth-default", true)],
        ),
    ];
    let out = resolve_native_next_hop(&explicit, "default", "10.0.0.9");
    assert_eq!(out.resolution_state, "resolved");
    assert_eq!(out.terminal_interfaces, vec!["eth-default"]);

    let mut unknown = explicit.clone();
    unknown[0].selected = None;
    let out = resolve_native_next_hop(&unknown, "default", "10.0.0.9");
    assert_eq!(out.resolution_state, "unresolved");
    assert_eq!(
        out.unresolved_reason.as_deref(),
        Some("route_selection_unknown")
    );

    let mut destination_not_selected = explicit.clone();
    destination_not_selected[0].selected = Some(true);
    destination_not_selected[0].destination_selected = Some(false);
    let out = resolve_native_next_hop(&destination_not_selected, "default", "10.0.0.9");
    assert_eq!(out.resolution_state, "resolved");
    assert_eq!(out.terminal_interfaces, vec!["eth-default"]);
}

#[test]
fn multiple_selected_observations_of_one_prefix_are_retained_deterministically() {
    let mut first = route(
        "default",
        "10.0.0.0/24",
        "bgp",
        vec![nh(None, "eth0", true)],
    );
    first.distance = Some(20);
    let mut second = route(
        "default",
        "10.0.0.0/24",
        "static",
        vec![nh(None, "eth1", true)],
    );
    second.distance = Some(1);
    let routes = vec![first, second];
    let out = resolve_native_next_hop(&routes, "default", "10.0.0.9");
    assert_eq!(out.resolution_state, "resolved");
    assert_eq!(out.terminal_interfaces, vec!["eth0", "eth1"]);
    assert_eq!(out.branches.len(), 2);
}

#[test]
fn vrf_is_scoped_and_cross_vrf_recursion_is_not_invented() {
    let isolated = vec![route(
        "BLUE",
        "10.0.0.0/24",
        "connected",
        vec![nh(None, "eth1", true)],
    )];
    let unresolved = resolve_native_next_hop(&isolated, "default", "10.0.0.1");
    assert_eq!(unresolved.resolution_state, "unresolved");
    assert_eq!(
        unresolved.unresolved_reason.as_deref(),
        Some("no_same_vrf_longest_prefix_match")
    );
}

#[test]
fn ecmp_branches_resolve_independently_and_partial_failure_is_explicit() {
    let routes = selected_fixture_routes();
    let out = resolve_native_next_hop(&routes, "default", "10.0.0.9");
    assert_eq!(out.resolution_state, "resolved");
    assert_eq!(out.terminal_interfaces, vec!["eth0", "eth1"]);
    assert_eq!(out.branches.len(), 2);
    assert!(out.candidate_next_hops.is_empty());

    let missing_one = routes
        .iter()
        .filter(|route| route.destination_prefix != "198.51.100.0/24")
        .cloned()
        .collect::<Vec<_>>();
    let partial = resolve_native_next_hop(&missing_one, "default", "10.0.0.9");
    assert_eq!(partial.resolution_state, "partially_resolved");
    assert_eq!(partial.terminal_interfaces, vec!["eth0"]);
    assert_eq!(
        partial.unresolved_reason.as_deref(),
        Some("no_same_vrf_longest_prefix_match")
    );
    assert_eq!(partial.candidate_next_hops, vec!["198.51.100.1"]);
    assert!(
        partial
            .branches
            .iter()
            .any(|branch| branch.resolution_state == "unresolved")
    );
}

#[test]
fn self_recursion_multi_route_cycles_and_depth_bound_terminate_explicitly() {
    let selfish = vec![route(
        "default",
        "192.0.2.0/24",
        "bgp",
        vec![nh(Some("192.0.2.1"), "eth0", false)],
    )];
    let out = resolve_native_next_hop(&selfish, "default", "192.0.2.1");
    assert_eq!(out.resolution_state, "unresolved");
    assert_eq!(out.unresolved_reason.as_deref(), Some("recursion_loop"));

    let looping = vec![
        route(
            "default",
            "192.0.2.1/32",
            "bgp",
            vec![nh(Some("198.51.100.1"), "eth0", false)],
        ),
        route(
            "default",
            "198.51.100.1/32",
            "bgp",
            vec![nh(Some("192.0.2.1"), "eth0", false)],
        ),
    ];
    let looped = resolve_native_next_hop(&looping, "default", "192.0.2.1");
    assert_eq!(looped.resolution_state, "unresolved");
    assert_eq!(looped.unresolved_reason.as_deref(), Some("recursion_loop"));
    assert!(looped.depth < bonsai::native_routes::MAX_RECURSION_DEPTH);

    let mut chain = Vec::new();
    for index in 0..12u8 {
        let next = format!("10.0.{}.1", index + 1);
        chain.push(route(
            "default",
            &format!("10.0.{index}.0/24"),
            "bgp",
            vec![nh(Some(&next), "eth0", false)],
        ));
    }
    let bounded = resolve_native_next_hop(&chain, "default", "10.0.0.1");
    assert_eq!(bounded.resolution_state, "unresolved");
    assert_eq!(
        bounded.unresolved_reason.as_deref(),
        Some("recursion_depth_bound_exceeded")
    );
    assert_eq!(bounded.depth, bonsai::native_routes::MAX_RECURSION_DEPTH);
}

#[test]
fn total_candidate_budget_is_bounded_deterministic_and_truthfully_reported() {
    let make_routes = || {
        let next_hops = (0..(MAX_RESOLUTION_CANDIDATES + 72))
            .map(|index| nh(None, &format!("eth{index}"), true))
            .collect();
        vec![route("default", "10.0.0.0/24", "kernel", next_hops)]
    };
    let out = resolve_native_next_hop(&make_routes(), "default", "10.0.0.9");
    assert_eq!(out.resolution_state, "partially_resolved");
    assert_eq!(
        out.candidate_next_hops_considered,
        MAX_RESOLUTION_CANDIDATES
    );
    assert_eq!(out.candidate_next_hops_omitted, 72);
    assert!(
        out.branches
            .iter()
            .any(|branch| branch.unresolved_reason.as_deref()
                == Some("resolution_candidate_budget_exceeded"))
    );

    let mut reversed = make_routes();
    reversed.reverse();
    for route in &mut reversed {
        route.next_hops.reverse();
    }
    let reversed_out = resolve_native_next_hop(&reversed, "default", "10.0.0.9");
    assert_eq!(
        out, reversed_out,
        "resource-limited retention must not depend on input order"
    );
}

#[test]
fn missing_interface_only_and_link_local_scope_evidence_remain_explicit() {
    let interface_only = vec![route(
        "default",
        "10.0.0.0/24",
        "static",
        vec![nh(None, "eth0", false)],
    )];
    let out = resolve_native_next_hop(&interface_only, "default", "10.0.0.9");
    assert_eq!(out.resolution_state, "resolved");
    assert_eq!(out.terminal_interfaces, vec!["eth0"]);

    let missing_nh = vec![route("default", "10.0.0.0/24", "bgp", Vec::new())];
    let out = resolve_native_next_hop(&missing_nh, "default", "10.0.0.9");
    assert_eq!(out.resolution_state, "unresolved");
    assert_eq!(
        out.unresolved_reason.as_deref(),
        Some("missing_next_hop_evidence")
    );

    let connected_v6 = vec![route(
        "default",
        "fe80::/64",
        "connected",
        vec![nh(None, "eth0", true)],
    )];
    let unscoped = resolve_native_next_hop(&connected_v6, "default", "fe80::1");
    assert_eq!(unscoped.resolution_state, "unresolved");
    assert_eq!(
        unscoped.unresolved_reason.as_deref(),
        Some("link_local_scope_missing")
    );
    let scoped = resolve_native_next_hop(&connected_v6, "default", "fe80::1%eth0");
    assert_eq!(scoped.resolution_state, "resolved");
    assert_eq!(scoped.terminal_interfaces, vec!["eth0"]);

    let conflicting = vec![route(
        "default",
        "fe80::/64",
        "connected",
        vec![nh(None, "eth1", true)],
    )];
    let scoped = resolve_native_next_hop(&conflicting, "default", "fe80::1%eth0");
    assert_eq!(scoped.resolution_state, "unresolved");
    assert_eq!(
        scoped.unresolved_reason.as_deref(),
        Some("link_local_scope_conflict")
    );
}

#[test]
fn explicit_blackhole_is_a_discard_terminal_not_interface_or_forwarding_verification() {
    let routes = vec![route(
        "default",
        "203.0.113.0/24",
        "static",
        vec![blackhole_nh()],
    )];
    let out = resolve_native_next_hop(&routes, "default", "203.0.113.9");
    assert_eq!(out.resolution_state, "resolved_discard");
    assert_eq!(out.terminal_interfaces, Vec::<String>::new());
    assert_eq!(out.terminal_actions, vec!["discard_blackhole"]);
    assert_eq!(
        out.forwarding_verification,
        "not_established_by_native_route_evidence"
    );
}

#[test]
fn input_route_and_nexthop_reordering_does_not_change_resolution_outcome() {
    let normal = selected_fixture_routes();
    let forward = resolve_native_next_hop(&normal, "default", "10.0.0.9");
    let mut reversed: Vec<_> = normal.iter().rev().cloned().collect();
    for route in &mut reversed {
        route.next_hops.reverse();
    }
    let reverse = resolve_native_next_hop(&reversed, "default", "10.0.0.9");
    assert_eq!(forward, reverse);
    assert_eq!(
        forward.resolution_basis,
        "same_vrf_selected_route_evidence_only_not_rib_or_forwarding_database"
    );
}

#[test]
fn competing_unknown_selection_at_same_prefix_makes_resolution_partial() {
    let selected = route(
        "default",
        "10.0.0.0/24",
        "bgp",
        vec![nh(None, "eth-selected", true)],
    );
    let unknown = route_with_selection(
        "default",
        "10.0.0.0/24",
        "static",
        vec![nh(None, "eth-unknown", true)],
        None,
        None,
    );
    let out = resolve_native_next_hop(&[selected, unknown], "default", "10.0.0.9");
    assert_eq!(out.resolution_state, "partially_resolved");
    assert_eq!(out.terminal_interfaces, vec!["eth-selected"]);
    assert!(out.branches.iter().any(|branch| {
        branch.unresolved_reason.as_deref() == Some("competing_route_selection_unknown")
    }));
}

#[test]
fn inactive_noninstalled_and_unknown_nexthop_states_do_not_become_active_candidates() {
    for (active, fib, reason) in [
        (
            Some(false),
            Some(true),
            "inactive_next_hop_not_active_candidate",
        ),
        (Some(true), Some(false), "next_hop_not_fib_installed"),
        (None, Some(true), "next_hop_active_state_unknown"),
        (Some(true), None, "next_hop_fib_state_unknown"),
    ] {
        let mut hop = nh(None, "eth0", true);
        hop.active = active;
        hop.fib = fib;
        let routes = vec![route("default", "10.0.0.0/24", "connected", vec![hop])];
        let out = resolve_native_next_hop(&routes, "default", "10.0.0.9");
        assert_eq!(out.resolution_state, "unresolved");
        assert_eq!(out.unresolved_reason.as_deref(), Some(reason));
        assert!(out.terminal_interfaces.is_empty());
    }
}

#[test]
fn genuine_local_route_is_local_delivery_not_interface_egress() {
    let parsed = parse_frr_native_route_capture(
        include_str!("fixtures/bn006_frr_10_6_1/ip-route-vrf-all.json"),
        "172.100.208.11",
    )
    .unwrap();
    let out = resolve_native_next_hop(&parsed.routes, "default", "172.100.208.11");
    assert_eq!(out.resolution_state, "resolved_local_delivery");
    assert!(out.terminal_interfaces.is_empty());
    assert_eq!(out.terminal_actions, vec!["local_delivery"]);
}

#[test]
fn ipv6_link_local_loop_identity_preserves_interface_scope() {
    let mut scoped_hop = nh(Some("fe80::1%eth1"), "eth1", false);
    scoped_hop.address_scope_id = Some("eth1".into());
    let routes = vec![
        route("default", "fe80::1/128", "bgp", vec![scoped_hop]),
        route(
            "default",
            "fe80::/64",
            "connected",
            vec![nh(None, "eth1", true)],
        ),
    ];
    let out = resolve_native_next_hop(&routes, "default", "fe80::1%eth0");
    assert_eq!(out.resolution_state, "unresolved");
    assert_eq!(out.unresolved_reason.as_deref(), Some("recursion_loop"));
    assert_eq!(out.branches[0].depth, 1);
}

// ---------------------------------------------------------------------------
// Operational route-query integration: retained shadow evidence is persisted
// monotonically and consumed through the real MCP tool path with explicit
// authorization, freshness and unresolved-branch semantics. These are
// synthetic replay controls — not live acquisition or verified forwarding.
// ---------------------------------------------------------------------------

mod mcp_integration {
    use super::*;
    use bonsai::enrichment::native_routes::{StoredNativeRouteObservation, persist_observation};
    use bonsai::enrichment::native_routing::{
        NativeRoutingIdentity, PROVENANCE_SYNTHETIC_CONTROL, canonical_device_id,
    };
    use bonsai::graph::GraphStore;
    use bonsai::http_server::auth::{Role, RoutingAccessScope};
    use bonsai::mcp_server::{ToolExecutionContext, call_tool_with_store_and_routing};
    use bonsai::registry::ApiRegistry;
    use serde_json::json;

    fn capture(
        address: &str,
        routes: Vec<NativeRouteEntry>,
    ) -> bonsai::native_routes::NativeRouteCapture {
        bonsai::native_routes::NativeRouteCapture {
            exporter_address: address.into(),
            parser_id: "frr-native-ip-route".into(),
            parser_version: "0.1.0".into(),
            raw_sample_sha256: "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
                .into(),
            routes,
            missing_or_invalid_fields: Vec::new(),
            unparsed_fields: Vec::new(),
        }
    }

    fn target_for(address: &str) -> bonsai::config::TargetConfig {
        bonsai::config::TargetConfig {
            address: format!("{address}:57400"),
            enabled: true,
            external_id: Some(format!("test-inventory:{address}")),
            ..Default::default()
        }
    }

    fn inventory_registry(
        dir: &tempfile::TempDir,
        addresses: &[&str],
    ) -> std::sync::Arc<ApiRegistry> {
        std::sync::Arc::new(
            ApiRegistry::open(
                dir.path().join("inventory-registry.json"),
                addresses
                    .iter()
                    .map(|address| target_for(address))
                    .collect(),
            )
            .unwrap(),
        )
    }

    fn observation(address: &str, captured_at_ns: i64) -> StoredNativeRouteObservation {
        let target = target_for(address);
        StoredNativeRouteObservation {
            captured_at_ns,
            capture_started_at_ns: captured_at_ns,
            transport: "synthetic_fixture_replay".into(),
            raw_routes: "{}".into(),
            capture: capture(address, selected_fixture_routes()),
            afi_safi: "ipv4-unicast".into(),
            inventory_identity: Some(canonical_device_id(&target)),
            acquisition_profile: Some("frr.fixed-readonly-native-routes-v1".into()),
            routing_identity: Some(NativeRoutingIdentity::new(
                &target,
                "ipv4-unicast",
                "frr.fixed-readonly-native-routes-v1",
                PROVENANCE_SYNTHETIC_CONTROL,
                captured_at_ns,
                address,
            )),
            source_observed_at_ns: None,
            ingested_at_ns: None,
            source_ordering_token: None,
        }
    }

    fn scoped_context(
        speaker: &str,
        registry: std::sync::Arc<ApiRegistry>,
    ) -> ToolExecutionContext {
        ToolExecutionContext {
            role: Role::ApiReadonly,
            scope: Some(RoutingAccessScope {
                speaker: Some(speaker.into()),
                ..Default::default()
            }),
            inventory_registry: Some(registry),
        }
    }

    #[tokio::test]
    async fn mcp_route_lookup_resolves_through_persisted_shadow_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(
            GraphStore::open(
                dir.path().join("graph").to_str().unwrap(),
                256 * 1024 * 1024,
            )
            .unwrap(),
        );
        let registry = inventory_registry(&dir, &["192.0.2.1", "192.0.2.5", "192.0.2.9"]);
        let now = bonsai::graph::common::now_ns();
        persist_observation(&store, &observation("192.0.2.1", now))
            .await
            .unwrap();

        let result = call_tool_with_store_and_routing(
            &store,
            None,
            "get_native_route_lookup",
            &json!({"target": "10.0.0.9"}),
            &scoped_context("192.0.2.1", registry.clone()),
        )
        .await
        .unwrap();
        assert_eq!(result["freshness"], "fresh");
        assert_eq!(result["evidence_class"], "synthetic_integration_control");
        assert_eq!(result["transport"], "synthetic_fixture_replay");
        assert_eq!(result["total_retained_routes"], 3);
        assert_eq!(result["resolution"]["resolution_state"], "resolved");
        assert_eq!(
            result["resolution"]["terminal_interfaces"],
            json!(["eth0", "eth1"])
        );
        assert_eq!(
            result["resolution"]["forwarding_verification"],
            "not_established_by_native_route_evidence"
        );
    }

    #[tokio::test]
    async fn mcp_route_lookup_preserves_unresolved_branches_explicitly() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(
            GraphStore::open(
                dir.path().join("graph").to_str().unwrap(),
                256 * 1024 * 1024,
            )
            .unwrap(),
        );
        let registry = inventory_registry(&dir, &["192.0.2.1", "192.0.2.5", "192.0.2.9"]);
        let now = bonsai::graph::common::now_ns();
        persist_observation(&store, &observation("192.0.2.1", now))
            .await
            .unwrap();

        // No route covers 203.0.113.9: the lookup must be explicitly
        // unresolved rather than fabricating a path.
        let result = call_tool_with_store_and_routing(
            &store,
            None,
            "get_native_route_lookup",
            &json!({"target": "203.0.113.9"}),
            &scoped_context("192.0.2.1", registry.clone()),
        )
        .await
        .unwrap();
        assert_eq!(result["resolution"]["resolution_state"], "unresolved");
        assert!(
            result["resolution"]["unresolved_reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "unresolved lookups require an explicit reason: {result}"
        );
    }

    #[tokio::test]
    async fn mcp_route_lookup_enforces_speaker_scope_and_identity() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(
            GraphStore::open(
                dir.path().join("graph").to_str().unwrap(),
                256 * 1024 * 1024,
            )
            .unwrap(),
        );
        let registry = inventory_registry(&dir, &["192.0.2.1", "192.0.2.5", "192.0.2.9"]);
        let now = bonsai::graph::common::now_ns();
        persist_observation(&store, &observation("192.0.2.1", now))
            .await
            .unwrap();

        let err = call_tool_with_store_and_routing(
            &store,
            None,
            "get_native_route_lookup",
            &json!({"device_address": "192.0.2.2", "target": "10.0.0.9"}),
            &scoped_context("192.0.2.1", registry.clone()),
        )
        .await
        .unwrap_err();
        assert!(err.contains("routing_scope_violation"), "{err}");

        let err = call_tool_with_store_and_routing(
            &store,
            None,
            "get_native_route_lookup",
            &json!({"target": "10.0.0.9"}),
            &scoped_context("192.0.2.9", registry.clone()),
        )
        .await
        .unwrap_err();
        assert!(err.contains("native_route_evidence_unavailable"), "{err}");

        let err = call_tool_with_store_and_routing(
            &store,
            None,
            "get_native_route_lookup",
            &json!({"target": "10.0.0.9", "afi_safi": "ipv6-unicast"}),
            &scoped_context("192.0.2.1", registry.clone()),
        )
        .await
        .unwrap_err();
        assert!(err.contains("afi_safi"), "{err}");
    }

    #[tokio::test]
    async fn native_route_persistence_is_monotonic_and_truthful_about_freshness() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(
            GraphStore::open(
                dir.path().join("graph").to_str().unwrap(),
                256 * 1024 * 1024,
            )
            .unwrap(),
        );
        let registry = inventory_registry(&dir, &["192.0.2.1", "192.0.2.5", "192.0.2.9"]);
        let now = bonsai::graph::common::now_ns();
        let obs = observation("192.0.2.1", now);
        persist_observation(&store, &obs).await.unwrap();
        // Exact replay is idempotent.
        persist_observation(&store, &obs).await.unwrap();

        // Older observations never overwrite newer evidence.
        let mut older = obs.clone();
        older.captured_at_ns = now - 1_000_000_000;
        older.capture_started_at_ns = now - 1_000_000_000;
        let stale = persist_observation(&store, &older).await;
        assert!(
            stale.as_ref().err().is_some_and(|err| err
                .to_string()
                .contains("older-start native route acquisition")),
            "{stale:?}"
        );

        // Equal-time conflicting evidence fails explicitly.
        let mut conflict = obs.clone();
        conflict.transport = "synthetic_conflicting_transport".into();
        let conflicted = persist_observation(&store, &conflict).await;
        assert!(
            conflicted
                .as_ref()
                .err()
                .is_some_and(|err| err.to_string().contains("equal-position conflicting")),
            "{conflicted:?}"
        );

        // An observation older than the freshness policy is stale, not fresh.
        let stale_obs = observation("192.0.2.5", now - 600_000_000_000);
        persist_observation(&store, &stale_obs).await.unwrap();
        let result = call_tool_with_store_and_routing(
            &store,
            None,
            "get_native_route_lookup",
            &json!({"target": "10.0.0.9"}),
            &scoped_context("192.0.2.5", registry.clone()),
        )
        .await
        .unwrap();
        assert_eq!(result["freshness"], "stale");
        assert_eq!(result["analysis_completed"], false);
        assert!(result["resolution"].is_null());
        assert_eq!(result["analysis_outcome"], "unresolved_prerequisites");
    }

    #[tokio::test]
    async fn management_address_reassignment_cannot_claim_prior_canonical_route_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(
            GraphStore::open(
                dir.path().join("graph").to_str().unwrap(),
                256 * 1024 * 1024,
            )
            .unwrap(),
        );
        let registry = inventory_registry(&dir, &["192.0.2.1"]);
        let now = bonsai::graph::common::now_ns();
        persist_observation(&store, &observation("192.0.2.1", now))
            .await
            .unwrap();
        let first = call_tool_with_store_and_routing(
            &store,
            None,
            "get_native_route_lookup",
            &json!({"target": "10.0.0.9"}),
            &scoped_context("192.0.2.1", registry.clone()),
        )
        .await
        .unwrap();
        assert_eq!(first["analysis_outcome"], "current_evidence_analysis");

        registry.remove_device("192.0.2.1:57400").unwrap();
        let mut replacement = target_for("192.0.2.1");
        replacement.external_id = Some("test-inventory:replacement-device".into());
        registry.add_device(replacement).unwrap();
        let err = call_tool_with_store_and_routing(
            &store,
            None,
            "get_native_route_lookup",
            &json!({"target": "10.0.0.9"}),
            &scoped_context("192.0.2.1", registry.clone()),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("native_route_evidence_unavailable"),
            "replacement device must not inherit prior canonical evidence: {err}"
        );

        registry.remove_device("192.0.2.1:57400").unwrap();
        let mut moved_original = target_for("192.0.2.2");
        moved_original.external_id = Some("test-inventory:192.0.2.1".into());
        registry.add_device(moved_original).unwrap();
        let moved = call_tool_with_store_and_routing(
            &store,
            None,
            "get_native_route_lookup",
            &json!({"device_address":"192.0.2.2", "target": "10.0.0.9"}),
            &ToolExecutionContext::global(Role::Viewer).with_inventory_registry(registry.clone()),
        )
        .await
        .unwrap();
        assert_eq!(
            moved["native_identity_status"],
            "registered_inventory_target"
        );
        assert_eq!(moved["analysis_outcome"], "current_evidence_analysis");
    }

    #[tokio::test]
    async fn arbitrary_inventory_string_and_legacy_route_rows_are_not_currently_eligible() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(
            GraphStore::open(
                dir.path().join("graph").to_str().unwrap(),
                256 * 1024 * 1024,
            )
            .unwrap(),
        );
        let registry = inventory_registry(&dir, &["192.0.2.1"]);
        let now = bonsai::graph::common::now_ns();
        let mut legacy = observation("192.0.2.1", now);
        legacy.inventory_identity = Some("arbitrary-nonempty-string".into());
        legacy.routing_identity = None;
        persist_observation(&store, &legacy).await.unwrap();
        let result = call_tool_with_store_and_routing(
            &store,
            None,
            "get_native_route_lookup",
            &json!({"target": "10.0.0.9"}),
            &scoped_context("192.0.2.1", registry.clone()),
        )
        .await
        .unwrap();
        assert_eq!(
            result["storage_identity_class"],
            "legacy_address_keyed_unverified"
        );
        assert_eq!(
            result["native_identity_status"],
            "unverified_or_stale_inventory_binding"
        );
        assert_eq!(result["analysis_completed"], false);
        assert!(result["resolution"].is_null());
    }

    #[tokio::test]
    async fn delayed_older_acquisition_cannot_overwrite_newer_completed_capture() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(
            GraphStore::open(
                dir.path().join("graph").to_str().unwrap(),
                256 * 1024 * 1024,
            )
            .unwrap(),
        );
        let base = 1_769_000_000_000_000_000i64;
        let mut older_capture_later_completion = observation("192.0.2.1", base + 900);
        older_capture_later_completion.capture_started_at_ns = base + 100;
        let mut newer_capture_earlier_completion = observation("192.0.2.1", base + 300);
        newer_capture_earlier_completion.capture_started_at_ns = base + 200;
        persist_observation(&store, &newer_capture_earlier_completion)
            .await
            .unwrap();
        let err = persist_observation(&store, &older_capture_later_completion)
            .await
            .expect_err("completion time must not outrank acquisition start order");
        assert!(
            err.to_string()
                .contains("older-start native route acquisition"),
            "{err}"
        );
    }
}
