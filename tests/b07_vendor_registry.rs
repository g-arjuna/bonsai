use bonsai::config::{BmpConfig, StreamingConfig, TargetConfig};
use bonsai::enrichment::parser_chain_enricher::{command_for_vendor, ssh_target_host};
use bonsai::platform_registry::{
    all_schema_vendor_enums, extract_endpoint_host, lookup_platform, normalize_platform_id,
    resolve_target_platform, suggest_platform_from_hostname,
};
use bonsai::streaming::build_streaming_readiness_report;

#[test]
fn b07_canonical_platform_aliases_and_no_cisco_fallback_for_unknown() {
    assert_eq!(normalize_platform_id("nokia_srl"), "nokia.srlinux");
    assert_eq!(normalize_platform_id("nokia-srlinux"), "nokia.srlinux");
    assert_eq!(normalize_platform_id("nokia.srlinux"), "nokia.srlinux");
    assert_eq!(normalize_platform_id("nokia_sros"), "nokia.sros");
    assert_eq!(normalize_platform_id("cisco_xrd"), "cisco.iosxr");
    assert_eq!(normalize_platform_id("cisco-iosxr"), "cisco.iosxr");
    assert_eq!(normalize_platform_id("arista_ceos"), "arista.eos");
    assert_eq!(normalize_platform_id("arista-eos"), "arista.eos");
    assert_eq!(normalize_platform_id("juniper_crpd"), "juniper.junos");
    assert_eq!(normalize_platform_id("juniper-junos"), "juniper.junos");
    assert_eq!(normalize_platform_id("frr"), "frr");

    // Unknown or unrecognized vendors must stay "unknown" and have no CLI fallback to Cisco.
    assert_eq!(normalize_platform_id("unknown"), "unknown");
    assert_eq!(normalize_platform_id("vyos"), "unknown");
    assert_eq!(normalize_platform_id(""), "unknown");
    assert_eq!(command_for_vendor("unknown"), None);
    assert_eq!(command_for_vendor("vyos"), None);
    assert_eq!(lookup_platform("unknown").adapter_id, None);
}

#[test]
fn b07_edge_a_hostname_works_without_vendor_prefix_and_hostname_is_advisory_only() {
    let edge_a = TargetConfig {
        address: "edge-a:57400".to_string(),
        hostname: Some("edge-a".to_string()),
        vendor: Some("frr".to_string()),
        role: Some("edge".to_string()),
        enabled: true,
        ..Default::default()
    };
    let resolved = resolve_target_platform(&edge_a);
    assert_eq!(resolved.platform_id, "frr");
    assert_eq!(
        command_for_vendor(&resolved.platform_id),
        Some("show running-config")
    );
    assert_eq!(ssh_target_host(&edge_a).unwrap(), "edge-a");

    let cfg = StreamingConfig {
        bmp: BmpConfig {
            enabled: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let report = build_streaming_readiness_report(&edge_a, None, &cfg);
    assert_eq!(report.vendor, "frr");
    let bmp_proto = report
        .protocols
        .iter()
        .find(|p| p.protocol == "bmp")
        .expect("bmp protocol readiness");
    assert_eq!(bmp_proto.status, "ready");
    assert!(bmp_proto.blockers.is_empty());

    // Without explicit vendor, hostname must NOT fabricate platform support,
    // though BMP remains conditionally supported via standards-based RFC 7854 export.
    let unprobed = TargetConfig {
        address: "172.20.0.15:57400".to_string(),
        hostname: Some("xrd-pe1".to_string()),
        vendor: None,
        role: Some("pe".to_string()),
        enabled: true,
        ..Default::default()
    };
    assert_eq!(resolve_target_platform(&unprobed).platform_id, "unknown");
    assert_eq!(
        suggest_platform_from_hostname(unprobed.hostname.as_deref()),
        Some("cisco.iosxr")
    );
    let unknown_report = build_streaming_readiness_report(&unprobed, None, &cfg);
    assert_eq!(unknown_report.vendor, "unknown");
    let unknown_bmp = unknown_report
        .protocols
        .iter()
        .find(|p| p.protocol == "bmp")
        .expect("bmp readiness on unknown platform");
    assert_eq!(unknown_bmp.support_level, "conditional");
    assert_eq!(unknown_bmp.status, "ready");
}

#[test]
fn b07_ipv6_and_hostname_endpoints_parse_cleanly() {
    assert_eq!(extract_endpoint_host("10.0.0.1:57400").unwrap(), "10.0.0.1");
    assert_eq!(extract_endpoint_host("10.0.0.1").unwrap(), "10.0.0.1");
    assert_eq!(
        extract_endpoint_host("[2001:db8::1]:57400").unwrap(),
        "2001:db8::1"
    );
    assert_eq!(
        extract_endpoint_host("[2001:db8::1]").unwrap(),
        "2001:db8::1"
    );
    assert_eq!(extract_endpoint_host("2001:db8::1").unwrap(), "2001:db8::1");
    assert_eq!(extract_endpoint_host("edge-a:57400").unwrap(), "edge-a");
    assert_eq!(extract_endpoint_host("edge-a").unwrap(), "edge-a");
    assert!(extract_endpoint_host("[2001:db8::1").is_err());
    assert!(extract_endpoint_host("").is_err());
}

#[test]
fn b07_ui_and_api_vendor_identifiers_agree() {
    let schema_enums = all_schema_vendor_enums();
    let onboarding_svelte =
        std::fs::read_to_string("ui/src/lib/Onboarding.svelte").expect("read Onboarding.svelte");
    for canonical in [
        "nokia.srlinux",
        "nokia.sros",
        "cisco.iosxr",
        "cisco.iosxe",
        "arista.eos",
        "juniper.junos",
        "frr",
    ] {
        assert!(
            schema_enums.contains(&canonical),
            "API schema enum missing canonical platform {canonical}"
        );
        assert!(
            onboarding_svelte.contains(&format!("value: '{canonical}'")),
            "Onboarding.svelte missing canonical platform {canonical}"
        );
    }
}
