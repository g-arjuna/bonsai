//! Initial native route and recursive next-hop semantic model.
//!
//! This module is deliberately evidence-first. It preserves FRR's separate
//! selected/destination-selected/installed/FIB signals instead of collapsing
//! them into one invented "route is forwarding" boolean. Recursive resolution
//! is bounded, VRF-scoped and multi-path aware; missing evidence is unresolved,
//! never a forwarding-failure verdict.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;
const MAX_ROUTES: usize = 100_000;
const MAX_NEXTHOPS_PER_ROUTE: usize = 256;
pub const MAX_RECURSION_DEPTH: usize = 8;

fn missing_or_invalid(capture_missing: &mut Vec<String>, path: String) {
    capture_missing.push(path);
}

fn optional_bool(object: &Map<String, Value>, key: &str) -> Option<bool> {
    object.get(key).and_then(Value::as_bool)
}

fn required_bool(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
    missing: &mut Vec<String>,
) -> Option<bool> {
    let value = optional_bool(object, key);
    if value.is_none() {
        missing.push(format!("{path}/{key}"));
    }
    value
}

fn optional_u64(object: &Map<String, Value>, key: &str) -> Option<u64> {
    object.get(key).and_then(Value::as_u64)
}

fn required_u32(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
    missing: &mut Vec<String>,
) -> Option<u32> {
    let value = optional_u64(object, key).and_then(|v| u32::try_from(v).ok());
    if object.contains_key(key) && value.is_none() {
        missing.push(format!("{path}/{key}"));
    }
    value
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeRouteNextHop {
    pub address: Option<String>,
    /// IPv6 zone/interface scope when the source embeds one in the address.
    /// Rust's IP type does not carry zones; this identity axis is preserved separately.
    pub address_scope_id: Option<String>,
    pub interface_name: Option<String>,
    pub interface_index: Option<u32>,
    pub active: Option<bool>,
    pub fib: Option<bool>,
    pub directly_connected: Option<bool>,
    pub weight: Option<u32>,
    /// FRR blackhole/discard evidence. These booleans are preserved separately;
    /// a discard terminal is not an interface forwarding resolution.
    pub unreachable: Option<bool>,
    pub reject: Option<bool>,
    pub admin_prohibited: Option<bool>,
    pub blackhole: Option<bool>,
    pub missing_or_invalid_fields: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeRouteEntry {
    pub vrf_name: String,
    pub afi_safi: String,
    pub destination_prefix: String,
    pub protocol: Option<String>,
    pub distance: Option<u32>,
    pub metric: Option<u32>,
    /// FRR `selected`: preserved exactly as route-selection evidence.
    pub selected: Option<bool>,
    /// FRR `destSelected`: preserved separately because it is not identical
    /// to generic route selection in every producer implementation.
    pub destination_selected: Option<bool>,
    /// FRR `installed`: installed-state evidence as emitted by the source.
    /// This is not silently equated with either BGP acceptance or forwarding.
    pub installed: Option<bool>,
    pub table: Option<u32>,
    pub uptime: Option<String>,
    pub next_hops: Vec<NativeRouteNextHop>,
    pub missing_or_invalid_fields: Vec<String>,
    pub unparsed_fields: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeRouteCapture {
    pub exporter_address: String,
    pub parser_id: String,
    pub parser_version: String,
    pub raw_sample_sha256: String,
    pub routes: Vec<NativeRouteEntry>,
    pub missing_or_invalid_fields: Vec<String>,
    pub unparsed_fields: Vec<String>,
}

fn route_key_extras(object: &Map<String, Value>, path: &str) -> Vec<String> {
    const KNOWN: &[&str] = &[
        "prefix",
        "prefixLen",
        "protocol",
        "vrfId",
        "vrfName",
        "selected",
        "destSelected",
        "distance",
        "metric",
        "installed",
        "table",
        "internalStatus",
        "internalFlags",
        "internalNextHopNum",
        "internalNextHopActiveNum",
        "internalNextHopFibInstalledNum",
        "nexthopGroupId",
        "installedNexthopGroupId",
        "receivedNexthopGroupId",
        "uptime",
        "nexthops",
    ];
    object
        .keys()
        .filter(|key| !KNOWN.contains(&key.as_str()))
        .map(|key| format!("{path}/{key}"))
        .collect()
}

fn nexthop_extras(object: &Map<String, Value>, path: &str) -> Vec<String> {
    const KNOWN: &[&str] = &[
        "flags",
        "fib",
        "ip",
        "afi",
        "interfaceIndex",
        "interfaceName",
        "active",
        "weight",
        "directlyConnected",
        "unreachable",
        "reject",
        "admin-prohibited",
        "blackhole",
    ];
    object
        .keys()
        .filter(|key| !KNOWN.contains(&key.as_str()))
        .map(|key| format!("{path}/{key}"))
        .collect()
}

fn parse_prefix(prefix: &str) -> Option<(IpAddr, u8)> {
    let (address, len) = prefix.split_once('/')?;
    let ip = address.parse().ok()?;
    let len = len.parse().ok()?;
    let max = match ip {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };
    (len <= max).then_some((ip, len))
}

fn prefix_host_bits_zero(ip: IpAddr, len: u8) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            if len > 32 {
                return false;
            }
            let host_mask = if len == 32 { 0 } else { u32::MAX >> len };
            u32::from(ip) & host_mask == 0
        }
        IpAddr::V6(ip) => {
            if len > 128 {
                return false;
            }
            let host_mask = if len == 128 { 0 } else { u128::MAX >> len };
            u128::from(ip) & host_mask == 0
        }
    }
}

fn parse_canonical_prefix(prefix: &str) -> Option<(IpAddr, u8)> {
    let (ip, len) = parse_prefix(prefix)?;
    prefix_host_bits_zero(ip, len).then_some((ip, len))
}

fn canonical_prefix_text(prefix: &str) -> Option<String> {
    parse_canonical_prefix(prefix).map(|(ip, len)| format!("{ip}/{len}"))
}

fn parse_scoped_ip(value: &str) -> Option<(IpAddr, Option<String>)> {
    let (address, scope) = value
        .split_once('%')
        .map(|(address, scope)| (address, Some(scope)))
        .unwrap_or((value, None));
    if address.is_empty() || scope.is_some_and(str::is_empty) {
        return None;
    }
    let ip: IpAddr = address.parse().ok()?;
    if ip.is_ipv4() && scope.is_some() {
        return None;
    }
    Some((ip, scope.map(str::to_string)))
}

fn is_ipv6_link_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V6(ip) => ip.segments()[0] & 0xffc0 == 0xfe80,
        IpAddr::V4(_) => false,
    }
}

fn optional_bool_strict(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
    missing: &mut Vec<String>,
) -> Option<bool> {
    match object.get(key) {
        None => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => {
            missing.push(format!("{path}/{key}"));
            None
        }
    }
}

/// Parse FRR `show ip route vrf all json` structured output.
pub fn parse_frr_native_route_capture(
    raw_json: &str,
    exporter_address: &str,
) -> Result<NativeRouteCapture, String> {
    if raw_json.len() > MAX_CAPTURE_BYTES {
        return Err("FRR route capture exceeds 8 MiB".into());
    }
    if exporter_address.parse::<IpAddr>().is_err() {
        return Err("FRR route capture requires an explicit exporter IP address".into());
    }
    crate::xrd_openconfig_interfaces::reject_duplicate_json_keys(raw_json.as_bytes())
        .map_err(|e| format!("duplicate JSON key in FRR route capture: {e}"))?;
    let root: Value =
        serde_json::from_str(raw_json).map_err(|e| format!("invalid FRR route JSON: {e}"))?;
    let root = root
        .as_object()
        .ok_or("FRR route capture must be an object")?;
    let mut capture = NativeRouteCapture {
        exporter_address: exporter_address.into(),
        parser_id: "frr-native-ip-route".into(),
        parser_version: "0.1.0".into(),
        raw_sample_sha256: hex::encode(sha2::Sha256::digest(raw_json.as_bytes())),
        routes: Vec::new(),
        missing_or_invalid_fields: Vec::new(),
        unparsed_fields: Vec::new(),
    };
    let mut identities = BTreeSet::new();
    for (vrf_key, vrf_value) in root {
        let Some(vrf) = vrf_value.as_object() else {
            missing_or_invalid(
                &mut capture.missing_or_invalid_fields,
                format!("/{vrf_key}"),
            );
            continue;
        };
        for (prefix_key, routes_value) in vrf {
            let route_path = format!("/{vrf_key}/{prefix_key}");
            let Some(entries) = routes_value.as_array() else {
                capture.missing_or_invalid_fields.push(route_path);
                continue;
            };
            if capture.routes.len().saturating_add(entries.len()) > MAX_ROUTES {
                return Err("FRR route capture exceeds 100000 routes".into());
            }
            for (entry_index, entry_value) in entries.iter().enumerate() {
                let epath = format!("{route_path}/{entry_index}");
                let Some(entry) = entry_value.as_object() else {
                    capture.missing_or_invalid_fields.push(epath);
                    continue;
                };
                let mut missing = Vec::new();
                let prefix = optional_string(entry, "prefix").unwrap_or_else(|| prefix_key.clone());
                if parse_prefix(&prefix).is_none() {
                    missing.push(format!("{epath}/prefix"));
                } else if parse_canonical_prefix(&prefix).is_none() {
                    missing.push(format!("{epath}/prefix:noncanonical-host-bits"));
                }
                let vrf_name = optional_string(entry, "vrfName").unwrap_or_else(|| vrf_key.clone());
                let protocol = optional_string(entry, "protocol");
                if protocol.is_none() {
                    missing.push(format!("{epath}/protocol"));
                }
                let selected = required_bool(entry, "selected", &epath, &mut missing);
                let destination_selected =
                    required_bool(entry, "destSelected", &epath, &mut missing);
                let installed = required_bool(entry, "installed", &epath, &mut missing);
                let distance = required_u32(entry, "distance", &epath, &mut missing);
                let metric = required_u32(entry, "metric", &epath, &mut missing);
                let table = optional_u64(entry, "table").and_then(|v| u32::try_from(v).ok());
                let afi_safi = match parse_prefix(&prefix).map(|(ip, _)| ip) {
                    Some(IpAddr::V4(_)) => "ipv4-unicast",
                    Some(IpAddr::V6(_)) => "ipv6-unicast",
                    None => "unknown",
                }
                .to_string();
                let prefix_identity =
                    canonical_prefix_text(&prefix).unwrap_or_else(|| format!("invalid:{prefix}"));
                if !identities.insert((
                    vrf_name.clone(),
                    prefix_identity,
                    protocol.clone().unwrap_or_default(),
                    distance,
                    metric,
                )) {
                    return Err(format!("duplicate FRR route identity in VRF {vrf_name}"));
                }
                let mut next_hops = Vec::new();
                match entry.get("nexthops").and_then(Value::as_array) {
                    Some(values) => {
                        if values.len() > MAX_NEXTHOPS_PER_ROUTE {
                            missing.push(format!("{epath}/nexthops:omitted-after-256"));
                        }
                        for (nh_index, nh_value) in
                            values.iter().take(MAX_NEXTHOPS_PER_ROUTE).enumerate()
                        {
                            let npath = format!("{epath}/nexthops/{nh_index}");
                            let mut nh_missing = Vec::new();
                            let Some(nh) = nh_value.as_object() else {
                                nh_missing.push(npath.clone());
                                next_hops.push(NativeRouteNextHop {
                                    address: None,
                                    address_scope_id: None,
                                    interface_name: None,
                                    interface_index: None,
                                    active: None,
                                    fib: None,
                                    directly_connected: None,
                                    weight: None,
                                    unreachable: None,
                                    reject: None,
                                    admin_prohibited: None,
                                    blackhole: None,
                                    missing_or_invalid_fields: nh_missing,
                                });
                                continue;
                            };
                            let raw_address = optional_string(nh, "ip");
                            let parsed_address = raw_address.as_deref().and_then(parse_scoped_ip);
                            if raw_address.is_some() && parsed_address.is_none() {
                                nh_missing.push(format!("{npath}/ip"));
                            }
                            let (address, address_scope_id) = match (raw_address, parsed_address) {
                                (Some(_), Some((ip, scope))) => (Some(ip.to_string()), scope),
                                (Some(raw), None) => (Some(raw), None),
                                (None, _) => (None, None),
                            };
                            let interface_name = optional_string(nh, "interfaceName");
                            let active =
                                optional_bool_strict(nh, "active", &npath, &mut nh_missing);
                            let fib = optional_bool_strict(nh, "fib", &npath, &mut nh_missing);
                            let directly_connected = optional_bool_strict(
                                nh,
                                "directlyConnected",
                                &npath,
                                &mut nh_missing,
                            );
                            let unreachable =
                                optional_bool_strict(nh, "unreachable", &npath, &mut nh_missing);
                            let reject =
                                optional_bool_strict(nh, "reject", &npath, &mut nh_missing);
                            let admin_prohibited = optional_bool_strict(
                                nh,
                                "admin-prohibited",
                                &npath,
                                &mut nh_missing,
                            );
                            let blackhole =
                                optional_bool_strict(nh, "blackhole", &npath, &mut nh_missing);
                            let has_discard = [unreachable, reject, admin_prohibited, blackhole]
                                .into_iter()
                                .any(|value| value == Some(true));
                            if address.is_none()
                                && directly_connected != Some(true)
                                && !has_discard
                                && interface_name.is_none()
                            {
                                nh_missing.push(format!("{npath}/ip-or-directlyConnected"));
                            }
                            if let Some(ip) = address
                                .as_deref()
                                .and_then(parse_scoped_ip)
                                .map(|(ip, _)| ip)
                                && is_ipv6_link_local(&ip)
                            {
                                if address_scope_id.is_none() && interface_name.is_none() {
                                    nh_missing.push(format!("{npath}/ip:interface-scope"));
                                }
                                if let (Some(scope), Some(interface)) =
                                    (&address_scope_id, &interface_name)
                                    && scope != interface
                                {
                                    nh_missing.push(format!("{npath}/ip:interface-scope-conflict"));
                                }
                            }
                            let next_hop = NativeRouteNextHop {
                                address,
                                address_scope_id,
                                interface_name,
                                interface_index: optional_u64(nh, "interfaceIndex")
                                    .and_then(|v| u32::try_from(v).ok()),
                                active,
                                fib,
                                directly_connected,
                                weight: optional_u64(nh, "weight")
                                    .and_then(|v| u32::try_from(v).ok()),
                                unreachable,
                                reject,
                                admin_prohibited,
                                blackhole,
                                missing_or_invalid_fields: nh_missing.clone(),
                            };
                            missing.extend(nh_missing);
                            capture.unparsed_fields.extend(nexthop_extras(nh, &npath));
                            next_hops.push(next_hop);
                        }
                    }
                    None => missing.push(format!("{epath}/nexthops")),
                }
                capture
                    .missing_or_invalid_fields
                    .extend(missing.iter().cloned());
                let unparsed = route_key_extras(entry, &epath);
                capture.unparsed_fields.extend(unparsed.iter().cloned());
                capture.routes.push(NativeRouteEntry {
                    vrf_name,
                    afi_safi,
                    destination_prefix: prefix,
                    protocol,
                    distance,
                    metric,
                    selected,
                    destination_selected,
                    installed,
                    table,
                    uptime: optional_string(entry, "uptime"),
                    next_hops,
                    missing_or_invalid_fields: missing,
                    unparsed_fields: unparsed,
                });
            }
        }
    }
    capture.missing_or_invalid_fields.sort();
    capture.unparsed_fields.sort();
    Ok(capture)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeNextHopBranchResolution {
    /// Ordered recursive next-hop addresses traversed by this branch. The first
    /// element is the original next hop; subsequent elements are evidence-derived
    /// recursive candidates, never fabricated interfaces or forwarding results.
    pub branch_path: Vec<String>,
    /// Ordered distinct longest-prefix route identities used by this branch.
    pub resolution_chain: Vec<String>,
    pub terminal_interface: Option<String>,
    /// Explicit discard terminal such as blackhole/reject. This is deliberately
    /// not represented as an interface or as verified traffic forwarding.
    pub terminal_action: Option<String>,
    pub depth: usize,
    pub resolution_state: String,
    pub unresolved_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedNativeNextHop {
    pub original_next_hop: String,
    pub terminal_interfaces: Vec<String>,
    pub terminal_actions: Vec<String>,
    /// Recursive candidates present on unresolved branches. This is diagnostic
    /// evidence, not a selected forwarding population.
    pub candidate_next_hops: Vec<String>,
    /// Ordered union of route prefixes used across retained branches. Per-branch
    /// ordering is preserved in `branches[].resolution_chain`.
    pub resolution_chain: Vec<String>,
    pub depth: usize,
    /// `resolved` means every retained branch terminated at interface evidence.
    /// `resolved_discard` means every retained branch terminated at explicit
    /// discard evidence. `partially_resolved` preserves mixed branch outcomes.
    /// None of these states asserts verified end-to-end traffic forwarding.
    pub resolution_state: String,
    pub unresolved_reason: Option<String>,
    pub branches: Vec<NativeNextHopBranchResolution>,
    pub targets_expanded: usize,
    pub candidate_next_hops_considered: usize,
    pub candidate_next_hops_omitted: usize,
    pub route_candidates_omitted: usize,
    pub max_recursion_depth: usize,
    pub max_resolution_expansions: usize,
    pub max_resolution_candidates: usize,
    pub resolution_basis: String,
    pub forwarding_verification: String,
}

pub const MAX_RESOLUTION_EXPANSIONS: usize = 64;
pub const MAX_RESOLUTION_CANDIDATES: usize = 128;
const MAX_LPM_ROUTE_ENTRIES: usize = 32;
const RESOLUTION_BASIS: &str =
    "same_vrf_selected_route_evidence_only_not_rib_or_forwarding_database";
const FORWARDING_VERIFICATION: &str = "not_established_by_native_route_evidence";

fn unresolved_output(original: &str, reason: &str) -> ResolvedNativeNextHop {
    let branch = NativeNextHopBranchResolution {
        branch_path: vec![original.to_string()],
        resolution_chain: Vec::new(),
        terminal_interface: None,
        terminal_action: None,
        depth: 0,
        resolution_state: "unresolved".into(),
        unresolved_reason: Some(reason.into()),
    };
    ResolvedNativeNextHop {
        original_next_hop: original.into(),
        terminal_interfaces: Vec::new(),
        terminal_actions: Vec::new(),
        candidate_next_hops: Vec::new(),
        resolution_chain: Vec::new(),
        depth: 0,
        resolution_state: "unresolved".into(),
        unresolved_reason: Some(reason.into()),
        branches: vec![branch],
        targets_expanded: 0,
        candidate_next_hops_considered: 0,
        candidate_next_hops_omitted: 0,
        route_candidates_omitted: 0,
        max_recursion_depth: MAX_RECURSION_DEPTH,
        max_resolution_expansions: MAX_RESOLUTION_EXPANSIONS,
        max_resolution_candidates: MAX_RESOLUTION_CANDIDATES,
        resolution_basis: RESOLUTION_BASIS.into(),
        forwarding_verification: FORWARDING_VERIFICATION.into(),
    }
}

fn prefix_contains(prefix: &str, address: &IpAddr) -> bool {
    let Some((network, len)) = parse_canonical_prefix(prefix) else {
        return false;
    };
    match (network, address) {
        (IpAddr::V4(network), IpAddr::V4(address)) => {
            let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
            (u32::from(*address) & mask) == u32::from(network)
        }
        (IpAddr::V6(network), IpAddr::V6(address)) => {
            let mask = if len == 0 {
                0
            } else {
                u128::MAX << (128 - len)
            };
            (u128::from(*address) & mask) == u128::from(network)
        }
        _ => false,
    }
}

fn bool_key(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "unknown",
    }
}

fn nexthop_sort_key(nh: &NativeRouteNextHop) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        nh.address.as_deref().unwrap_or(""),
        nh.address_scope_id.as_deref().unwrap_or(""),
        nh.interface_name.as_deref().unwrap_or(""),
        nh.interface_index.unwrap_or(u32::MAX),
        bool_key(nh.active),
        bool_key(nh.fib),
        bool_key(nh.directly_connected),
        nh.weight.unwrap_or(u32::MAX),
        bool_key(nh.unreachable),
        bool_key(nh.reject),
        bool_key(nh.admin_prohibited),
        bool_key(nh.blackhole),
    )
}

fn route_sort_key(route: &NativeRouteEntry) -> (String, String, u32, u32, u32, String) {
    let mut next_hop_keys: Vec<_> = route.next_hops.iter().map(nexthop_sort_key).collect();
    next_hop_keys.sort();
    (
        canonical_prefix_text(&route.destination_prefix)
            .unwrap_or_else(|| format!("invalid:{}", route.destination_prefix)),
        route.protocol.clone().unwrap_or_default(),
        route.distance.unwrap_or(u32::MAX),
        route.metric.unwrap_or(u32::MAX),
        route.table.unwrap_or(u32::MAX),
        next_hop_keys.join("\u{1f}"),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RouteSelectionClass {
    Selected,
    ExplicitlyNotSelected,
    Unknown,
}

fn route_selection_class(route: &NativeRouteEntry) -> RouteSelectionClass {
    if route.selected == Some(true) && route.destination_selected == Some(true) {
        RouteSelectionClass::Selected
    } else if route.selected == Some(false) || route.destination_selected == Some(false) {
        RouteSelectionClass::ExplicitlyNotSelected
    } else {
        RouteSelectionClass::Unknown
    }
}

enum LpmLookup<'a> {
    Candidates {
        routes: Vec<&'a NativeRouteEntry>,
        omitted: usize,
    },
    NoMatch,
    SelectionUnknown,
}

/// Find the longest-prefix selected route evidence usable for forwarding-candidate
/// resolution. Explicitly unselected entries at a prefix do not shadow less-specific
/// selected evidence because they are not a forwarding candidate; unknown selection
/// at the longest matching prefix is resolution-blocking evidence instead.
fn lookup_selected_lpm<'a>(
    routes: &'a [NativeRouteEntry],
    vrf_name: &str,
    afi: &str,
    target: &IpAddr,
) -> LpmLookup<'a> {
    let mut by_len: BTreeMap<u8, Vec<&NativeRouteEntry>> = BTreeMap::new();
    for route in routes {
        if route.vrf_name != vrf_name || route.afi_safi != afi {
            continue;
        }
        let Some((_, len)) = parse_canonical_prefix(&route.destination_prefix) else {
            continue;
        };
        if prefix_contains(&route.destination_prefix, target) {
            by_len.entry(len).or_default().push(route);
        }
    }
    for group in by_len.values().rev() {
        let mut selected = Vec::new();
        let mut unknown = false;
        for route in group {
            match route_selection_class(route) {
                RouteSelectionClass::Selected => selected.push(*route),
                RouteSelectionClass::ExplicitlyNotSelected => {}
                RouteSelectionClass::Unknown => unknown = true,
            }
        }
        if !selected.is_empty() {
            selected.sort_by_key(|route| route_sort_key(route));
            let mut seen = BTreeSet::new();
            selected.retain(|route| seen.insert(route_sort_key(route)));
            let omitted = selected.len().saturating_sub(MAX_LPM_ROUTE_ENTRIES);
            selected.truncate(MAX_LPM_ROUTE_ENTRIES);
            return LpmLookup::Candidates {
                routes: selected,
                omitted,
            };
        }
        if unknown {
            return LpmLookup::SelectionUnknown;
        }
    }
    LpmLookup::NoMatch
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NextHopCandidate {
    TerminalInterface(String),
    TerminalDiscard(String),
    Recurse(IpAddr, Option<String>),
    Unresolved(&'static str),
}

fn candidate_key(candidate: &NextHopCandidate) -> String {
    match candidate {
        NextHopCandidate::TerminalInterface(interface) => format!("interface:{interface}"),
        NextHopCandidate::TerminalDiscard(action) => format!("discard:{action}"),
        NextHopCandidate::Recurse(address, scope) => {
            format!("recurse:{address}:{}", scope.as_deref().unwrap_or(""))
        }
        NextHopCandidate::Unresolved(reason) => format!("unresolved:{reason}"),
    }
}

fn classify_next_hop(nh: &NativeRouteNextHop) -> NextHopCandidate {
    let subtype_count = [nh.reject, nh.admin_prohibited, nh.blackhole]
        .into_iter()
        .filter(|value| *value == Some(true))
        .count();
    if subtype_count > 1 {
        return NextHopCandidate::Unresolved("contradictory_discard_evidence");
    }
    if nh.reject == Some(true) {
        return NextHopCandidate::TerminalDiscard("discard_reject".into());
    }
    if nh.admin_prohibited == Some(true) {
        return NextHopCandidate::TerminalDiscard("discard_admin_prohibited".into());
    }
    if nh.blackhole == Some(true) {
        return NextHopCandidate::TerminalDiscard("discard_blackhole".into());
    }
    if nh.unreachable == Some(true) {
        return NextHopCandidate::TerminalDiscard("discard_unreachable".into());
    }

    if nh.directly_connected == Some(true) || nh.address.is_none() {
        return match nh.interface_name.clone() {
            Some(interface) if !interface.is_empty() => {
                NextHopCandidate::TerminalInterface(interface)
            }
            _ => NextHopCandidate::Unresolved("missing_interface_next_hop_evidence"),
        };
    }

    let Some(raw_address) = nh.address.as_deref() else {
        return NextHopCandidate::Unresolved("missing_next_hop_evidence");
    };
    let Some((address, embedded_scope)) = parse_scoped_ip(raw_address) else {
        return NextHopCandidate::Unresolved("invalid_next_hop_address");
    };
    let scope = match (embedded_scope, nh.address_scope_id.clone()) {
        (Some(embedded), Some(recorded)) if embedded != recorded => {
            return NextHopCandidate::Unresolved("link_local_scope_conflict");
        }
        (Some(embedded), _) => Some(embedded),
        (None, recorded) => recorded,
    };
    let scope = match (scope, nh.interface_name.clone()) {
        (Some(scope), Some(interface)) if scope != interface => {
            return NextHopCandidate::Unresolved("link_local_scope_conflict");
        }
        (Some(scope), _) => Some(scope),
        (None, interface) => interface,
    };
    if is_ipv6_link_local(&address) && scope.is_none() {
        return NextHopCandidate::Unresolved("link_local_scope_missing");
    }
    NextHopCandidate::Recurse(
        address,
        if is_ipv6_link_local(&address) {
            scope
        } else {
            None
        },
    )
}

#[derive(Default)]
struct ResolutionWorkspace {
    targets_expanded: usize,
    candidate_next_hops_considered: usize,
    candidate_next_hops_omitted: usize,
    route_candidates_omitted: usize,
}

fn terminal_branch(
    branch_path: Vec<String>,
    resolution_chain: Vec<String>,
    terminal_interface: Option<String>,
    terminal_action: Option<String>,
    depth: usize,
    state: &str,
    reason: Option<&str>,
) -> NativeNextHopBranchResolution {
    NativeNextHopBranchResolution {
        branch_path,
        resolution_chain,
        terminal_interface,
        terminal_action,
        depth,
        resolution_state: state.into(),
        unresolved_reason: reason.map(str::to_string),
    }
}

#[allow(clippy::too_many_arguments)]
fn expand_target(
    routes: &[NativeRouteEntry],
    vrf_name: &str,
    afi: &str,
    target: IpAddr,
    target_scope: Option<String>,
    depth: usize,
    branch_path: Vec<String>,
    visited: BTreeSet<IpAddr>,
    workspace: &mut ResolutionWorkspace,
) -> Vec<NativeNextHopBranchResolution> {
    if depth >= MAX_RECURSION_DEPTH {
        return vec![terminal_branch(
            branch_path,
            Vec::new(),
            None,
            None,
            depth,
            "unresolved",
            Some("recursion_depth_bound_exceeded"),
        )];
    }
    if workspace.targets_expanded >= MAX_RESOLUTION_EXPANSIONS {
        return vec![terminal_branch(
            branch_path,
            Vec::new(),
            None,
            None,
            depth,
            "unresolved",
            Some("resolution_traversal_budget_exceeded"),
        )];
    }
    workspace.targets_expanded += 1;

    let (candidates, omitted_routes) = match lookup_selected_lpm(routes, vrf_name, afi, &target) {
        LpmLookup::Candidates { routes, omitted } => (routes, omitted),
        LpmLookup::NoMatch => {
            return vec![terminal_branch(
                branch_path,
                Vec::new(),
                None,
                None,
                depth,
                "unresolved",
                Some("no_same_vrf_longest_prefix_match"),
            )];
        }
        LpmLookup::SelectionUnknown => {
            return vec![terminal_branch(
                branch_path,
                Vec::new(),
                None,
                None,
                depth,
                "unresolved",
                Some("route_selection_unknown"),
            )];
        }
    };
    workspace.route_candidates_omitted += omitted_routes;
    let chain_prefixes: Vec<String> = candidates
        .iter()
        .filter_map(|route| canonical_prefix_text(&route.destination_prefix))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut specs = Vec::new();
    let mut seen_specs = BTreeSet::new();
    let mut empty_route_evidence = false;
    let omitted_next_hops_before = workspace.candidate_next_hops_omitted;
    if omitted_routes > 0
        && seen_specs.insert("unresolved:resolution_route_budget_exceeded".to_string())
    {
        specs.push(NextHopCandidate::Unresolved(
            "resolution_route_budget_exceeded",
        ));
    }
    for route in candidates {
        if route.next_hops.is_empty() {
            empty_route_evidence = true;
            continue;
        }
        let mut next_hops: Vec<_> = route.next_hops.iter().collect();
        next_hops.sort_by_key(|nh| nexthop_sort_key(nh));
        for nh in next_hops {
            if workspace.candidate_next_hops_considered >= MAX_RESOLUTION_CANDIDATES {
                workspace.candidate_next_hops_omitted += 1;
                continue;
            }
            workspace.candidate_next_hops_considered += 1;
            let spec = classify_next_hop(nh);
            if seen_specs.insert(candidate_key(&spec)) {
                specs.push(spec);
            }
        }
    }
    if empty_route_evidence && seen_specs.insert("unresolved:missing_next_hop_evidence".to_string())
    {
        specs.push(NextHopCandidate::Unresolved("missing_next_hop_evidence"));
    }
    if workspace.candidate_next_hops_omitted > omitted_next_hops_before
        && seen_specs.insert("unresolved:resolution_candidate_budget_exceeded".to_string())
    {
        specs.push(NextHopCandidate::Unresolved(
            "resolution_candidate_budget_exceeded",
        ));
    }
    if specs.is_empty() {
        let reason = if workspace.candidate_next_hops_omitted > 0 {
            "resolution_candidate_budget_exceeded"
        } else {
            "missing_next_hop_evidence"
        };
        return vec![terminal_branch(
            branch_path,
            chain_prefixes,
            None,
            None,
            depth,
            "unresolved",
            Some(reason),
        )];
    }

    let mut branches = Vec::new();
    for spec in specs {
        match spec {
            NextHopCandidate::TerminalInterface(interface) => {
                if let (Some(scope), terminal) = (&target_scope, &interface)
                    && scope != terminal
                {
                    branches.push(terminal_branch(
                        branch_path.clone(),
                        chain_prefixes.clone(),
                        None,
                        None,
                        depth,
                        "unresolved",
                        Some("link_local_scope_conflict"),
                    ));
                } else {
                    branches.push(terminal_branch(
                        branch_path.clone(),
                        chain_prefixes.clone(),
                        Some(interface),
                        None,
                        depth,
                        "resolved_interface",
                        None,
                    ));
                }
            }
            NextHopCandidate::TerminalDiscard(action) => branches.push(terminal_branch(
                branch_path.clone(),
                chain_prefixes.clone(),
                None,
                Some(action),
                depth,
                "resolved_discard",
                None,
            )),
            NextHopCandidate::Unresolved(reason) => branches.push(terminal_branch(
                branch_path.clone(),
                chain_prefixes.clone(),
                None,
                None,
                depth,
                "unresolved",
                Some(reason),
            )),
            NextHopCandidate::Recurse(address, scope) => {
                if address == target || visited.contains(&address) {
                    branches.push(terminal_branch(
                        branch_path.clone(),
                        chain_prefixes.clone(),
                        None,
                        None,
                        depth,
                        "unresolved",
                        Some("recursion_loop"),
                    ));
                    continue;
                }
                let mut child_path = branch_path.clone();
                child_path.push(address.to_string());
                let mut child_visited = visited.clone();
                child_visited.insert(target);
                let children = expand_target(
                    routes,
                    vrf_name,
                    afi,
                    address,
                    scope,
                    depth + 1,
                    child_path,
                    child_visited,
                    workspace,
                );
                for mut child in children {
                    let mut chain = chain_prefixes.clone();
                    for prefix in child.resolution_chain {
                        if !chain.contains(&prefix) {
                            chain.push(prefix);
                        }
                    }
                    child.resolution_chain = chain;
                    branches.push(child);
                }
            }
        }
    }
    branches
}

fn ordered_unique(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for value in values {
        if seen.insert(value.clone()) {
            out.push(value);
        }
    }
    out
}

/// Resolve one next-hop address through longest-prefix matches inside the same
/// VRF/AFI. This is selected-route evidence resolution, neither a RIB browser
/// nor a data-plane/forwarding correctness verdict.
pub fn resolve_native_next_hop(
    routes: &[NativeRouteEntry],
    vrf_name: &str,
    next_hop: &str,
) -> ResolvedNativeNextHop {
    let Some((target, input_scope)) = parse_scoped_ip(next_hop) else {
        return unresolved_output(next_hop, "invalid_next_hop_address");
    };
    if is_ipv6_link_local(&target) && input_scope.is_none() {
        return unresolved_output(next_hop, "link_local_scope_missing");
    }
    let afi = match target {
        IpAddr::V4(_) => "ipv4-unicast",
        IpAddr::V6(_) => "ipv6-unicast",
    };
    let mut workspace = ResolutionWorkspace::default();
    let mut branches = expand_target(
        routes,
        vrf_name,
        afi,
        target,
        input_scope,
        0,
        vec![next_hop.to_string()],
        BTreeSet::new(),
        &mut workspace,
    );
    branches.sort_by_key(|branch| {
        (
            branch.branch_path.clone(),
            branch.terminal_interface.clone().unwrap_or_default(),
            branch.terminal_action.clone().unwrap_or_default(),
            branch.resolution_state.clone(),
            branch.unresolved_reason.clone().unwrap_or_default(),
        )
    });

    let terminal_interfaces: Vec<String> = branches
        .iter()
        .filter_map(|branch| branch.terminal_interface.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let terminal_actions: Vec<String> = branches
        .iter()
        .filter_map(|branch| branch.terminal_action.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let unresolved: Vec<_> = branches
        .iter()
        .filter(|branch| branch.resolution_state == "unresolved")
        .collect();
    let has_terminal = branches.iter().any(|branch| {
        branch.resolution_state == "resolved_interface"
            || branch.resolution_state == "resolved_discard"
    });
    let resolution_state = if unresolved.is_empty() {
        if !terminal_interfaces.is_empty() && terminal_actions.is_empty() {
            "resolved"
        } else if terminal_interfaces.is_empty() && !terminal_actions.is_empty() {
            "resolved_discard"
        } else {
            "resolved_mixed_terminal"
        }
    } else if has_terminal {
        "partially_resolved"
    } else {
        "unresolved"
    }
    .to_string();
    let reasons: Vec<String> = unresolved
        .iter()
        .filter_map(|branch| branch.unresolved_reason.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let unresolved_reason = if unresolved.is_empty() {
        None
    } else if reasons.len() == 1 {
        reasons.first().cloned()
    } else {
        Some("mixed_unresolved_branches".into())
    };
    let candidate_next_hops: Vec<String> = unresolved
        .iter()
        .flat_map(|branch| branch.branch_path.iter().skip(1).cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let resolution_chain = ordered_unique(
        branches
            .iter()
            .flat_map(|branch| branch.resolution_chain.iter().cloned()),
    );
    let depth = branches
        .iter()
        .map(|branch| branch.depth)
        .max()
        .unwrap_or(0);

    ResolvedNativeNextHop {
        original_next_hop: next_hop.into(),
        terminal_interfaces,
        terminal_actions,
        candidate_next_hops,
        resolution_chain,
        depth,
        resolution_state,
        unresolved_reason,
        branches,
        targets_expanded: workspace.targets_expanded,
        candidate_next_hops_considered: workspace.candidate_next_hops_considered,
        candidate_next_hops_omitted: workspace.candidate_next_hops_omitted,
        route_candidates_omitted: workspace.route_candidates_omitted,
        max_recursion_depth: MAX_RECURSION_DEPTH,
        max_resolution_expansions: MAX_RESOLUTION_EXPANSIONS,
        max_resolution_candidates: MAX_RESOLUTION_CANDIDATES,
        resolution_basis: RESOLUTION_BASIS.into(),
        forwarding_verification: FORWARDING_VERIFICATION.into(),
    }
}
