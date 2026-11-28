//! Bounded normalization of directly acquired FRR OSPF and IS-IS operational
//! evidence.
//!
//! This module is an evidence boundary, not a topology or forwarding oracle.
//! Neighbor output is directly observed adjacency evidence. LSDB output is
//! advertised link-state evidence. Neither is physical connectivity evidence,
//! derived logical topology, or forwarding state. In particular, an IS-IS
//! `adj` value can be a dynamic hostname rather than a system identifier and
//! must not be silently canonicalized.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::net::Ipv4Addr;

pub const IGP_PARSER_ID: &str = "frr_igp_operational_normalizer";
pub const IGP_PARSER_REVISION: &str = "2026-11-28.initial-v1";
pub const MAX_IGP_INPUT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_JSON_DEPTH: usize = 64;
pub const MAX_IGP_ADJACENCIES: usize = 10_000;
pub const MAX_IGP_LSDB_ENTRIES: usize = 100_000;
pub const MAX_IGP_RELATIONSHIPS: usize = 200_000;
pub const MAX_IGP_PREFIXES: usize = 200_000;
pub const MAX_IGP_DIAGNOSTICS: usize = 256;
pub const MAX_VRF_CONTEXTS: usize = 256;
pub const MAX_AREA_CONTEXTS: usize = 512;
pub const IGP_EVIDENCE_BOUNDARY: &str =
    "not_physical_topology_not_derived_logical_topology_not_forwarding_state";

pub const FRR_OSPF_NEIGHBOR_COMMAND: &str = "vtysh -c 'show ip ospf vrf all neighbor detail json'";
pub const FRR_OSPF_LSDB_COMMAND: &str = "vtysh -c 'show ip ospf vrf all database detail json'";
pub const FRR_ISIS_NEIGHBOR_COMMAND: &str = "vtysh -c 'show isis vrf all neighbor detail json'";
pub const FRR_ISIS_LSDB_COMMAND: &str = "vtysh -c 'show isis vrf all database detail json'";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IgpProtocol {
    Ospfv2,
    Isis,
}

impl IgpProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ospfv2 => "ospfv2",
            Self::Isis => "isis",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IgpEvidenceClass {
    DirectlyObservedAdjacency,
    AdvertisedLinkStateRelationship,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IgpSourceProvenance {
    pub source_provenance: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub software_release: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquired_at_ns: Option<i64>,
    pub raw_sha256: String,
    pub parser_id: String,
    pub parser_revision: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IgpCaptureContext {
    pub canonical_device: Option<String>,
    pub identity_scope: String,
    pub network_namespace: String,
    pub network_instance: Option<String>,
    pub protocol_instance: Option<String>,
    pub source_provenance: String,
    pub command: String,
    pub software_release: Option<String>,
    pub artifact_identity: Option<String>,
    pub acquired_at_ns: Option<i64>,
}

impl IgpCaptureContext {
    pub fn new(command: impl Into<String>, source_provenance: impl Into<String>) -> Self {
        Self {
            canonical_device: None,
            identity_scope: "inventory".into(),
            network_namespace: "default".into(),
            network_instance: None,
            protocol_instance: None,
            source_provenance: source_provenance.into(),
            command: command.into(),
            software_release: None,
            artifact_identity: None,
            acquired_at_ns: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IgpAdjacencyEvidence {
    pub protocol: IgpProtocol,
    pub evidence_class: IgpEvidenceClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_device: Option<String>,
    pub identity_scope: String,
    pub network_namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_instance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_instance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_interface_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_interface_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_interface_address: Option<String>,
    /// Identity exactly as reported by the source for the adjacency. The kind
    /// is explicit because FRR can render IS-IS neighbors by dynamic hostname.
    pub remote_adjacency_identity: String,
    pub remote_identity_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub convergence_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at_ns: Option<i64>,
    pub lifecycle: String,
    pub acquisition_coverage: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_or_invalid_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ospf: Option<OspfAdjacencyDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isis: Option<IsisAdjacencyDetails>,
    pub provenance: IgpSourceProvenance,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OspfAdjacencyDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub neighbor_router_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hello_timer_msec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead_timer_msec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_msec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_change_counter: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IsisAdjacencyDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub neighbor_system_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub circuit_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub circuit_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holdtime_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_or_last_change: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topologies: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocols: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snpa: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkStateRelationship {
    pub remote_identity: String,
    pub identity_kind: String,
    pub relationship_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_interface_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_interface_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology_id: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extensions: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkStatePrefix {
    pub prefix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology_id: Option<String>,
    #[serde(default)]
    pub external: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address_family: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extensions: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkStateEvidence {
    pub protocol: IgpProtocol,
    pub evidence_class: IgpEvidenceClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_device: Option<String>,
    pub identity_scope: String,
    pub network_namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_instance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_instance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    pub advertising_router_or_system: String,
    pub lsa_lsp_identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_lifetime_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lsa_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub advertised_relationships: Vec<LinkStateRelationship>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reachability_prefixes: Vec<LinkStatePrefix>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology_id: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extensions: Map<String, Value>,
    pub evidence_boundary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_or_invalid_fields: Vec<String>,
    pub provenance: IgpSourceProvenance,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IgpNormalization<T> {
    pub entries: Vec<T>,
    pub diagnostics: Vec<String>,
    pub input_bytes: usize,
    pub input_sha256: String,
    pub input_record_limit_reached: bool,
    /// A complete acquisition of an active protocol scope that genuinely
    /// contains zero records. This is never inferred from `{}` when the
    /// protocol instance itself is absent or inactive.
    pub positive_empty_population: bool,
    /// The command completed but the source reported no active/configured
    /// protocol scope to assess (for example OSPF not running or no IS-IS areas).
    pub scope_absent_or_inactive: bool,
}

impl<T> IgpNormalization<T> {
    fn new(input: &str) -> Self {
        Self {
            entries: Vec::new(),
            diagnostics: Vec::new(),
            input_bytes: input.len(),
            input_sha256: sha256_hex(input.as_bytes()),
            input_record_limit_reached: false,
            positive_empty_population: false,
            scope_absent_or_inactive: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IgpFamilyNormalizationSummary {
    pub family: String,
    pub command: String,
    pub outcome: String,
    #[serde(default)]
    pub entries: usize,
    #[serde(default)]
    pub positive_empty_population: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IgpCaptureNormalization {
    pub adjacencies: Vec<IgpAdjacencyEvidence>,
    pub link_state_entries: Vec<LinkStateEvidence>,
    pub families: Vec<IgpFamilyNormalizationSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn provenance(context: &IgpCaptureContext, raw: &str) -> IgpSourceProvenance {
    IgpSourceProvenance {
        source_provenance: context.source_provenance.clone(),
        command: context.command.clone(),
        software_release: context.software_release.clone(),
        artifact_identity: context.artifact_identity.clone(),
        acquired_at_ns: context.acquired_at_ns,
        raw_sha256: sha256_hex(raw.as_bytes()),
        parser_id: IGP_PARSER_ID.into(),
        parser_revision: IGP_PARSER_REVISION.into(),
    }
}

fn base_adjacency(
    protocol: IgpProtocol,
    context: &IgpCaptureContext,
    raw: &str,
    remote_identity: String,
    remote_identity_kind: &'static str,
) -> IgpAdjacencyEvidence {
    IgpAdjacencyEvidence {
        protocol,
        evidence_class: IgpEvidenceClass::DirectlyObservedAdjacency,
        canonical_device: context.canonical_device.clone(),
        identity_scope: context.identity_scope.clone(),
        network_namespace: context.network_namespace.clone(),
        network_instance: context.network_instance.clone(),
        protocol_instance: context.protocol_instance.clone(),
        local_interface_name: None,
        local_interface_address: None,
        remote_interface_address: None,
        remote_adjacency_identity: remote_identity,
        remote_identity_kind: remote_identity_kind.into(),
        state: None,
        convergence_state: None,
        observed_at_ns: context.acquired_at_ns,
        lifecycle: "directly_observed".into(),
        acquisition_coverage: "single_fixed_command_bounded".into(),
        missing_or_invalid_fields: Vec::new(),
        ospf: None,
        isis: None,
        provenance: provenance(context, raw),
    }
}

fn base_lsdb(
    protocol: IgpProtocol,
    context: &IgpCaptureContext,
    raw: &str,
    advertising: String,
    identity: String,
) -> LinkStateEvidence {
    LinkStateEvidence {
        protocol,
        evidence_class: IgpEvidenceClass::AdvertisedLinkStateRelationship,
        canonical_device: context.canonical_device.clone(),
        identity_scope: context.identity_scope.clone(),
        network_namespace: context.network_namespace.clone(),
        network_instance: context.network_instance.clone(),
        protocol_instance: context.protocol_instance.clone(),
        area: None,
        level: None,
        advertising_router_or_system: advertising,
        lsa_lsp_identity: identity,
        sequence: None,
        age_seconds: None,
        remaining_lifetime_seconds: None,
        lsa_type: None,
        advertised_relationships: Vec::new(),
        reachability_prefixes: Vec::new(),
        topology_id: None,
        extensions: Map::new(),
        evidence_boundary: IGP_EVIDENCE_BOUNDARY.into(),
        missing_or_invalid_fields: Vec::new(),
        provenance: provenance(context, raw),
    }
}

fn obj<'a>(value: &'a Value, key: &str) -> Option<&'a Map<String, Value>> {
    value.get(key).and_then(Value::as_object)
}

fn arr<'a>(value: &'a Value, key: &str) -> Option<&'a Vec<Value>> {
    value.get(key).and_then(Value::as_array)
}

fn s(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn u64_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(|v| match v {
        Value::Number(n) => n.as_u64(),
        Value::String(text) => text.parse::<u64>().ok(),
        _ => None,
    })
}

fn push_diag(out: &mut Vec<String>, diagnostic: impl Into<String>) {
    if out.len() < MAX_IGP_DIAGNOSTICS {
        out.push(diagnostic.into());
    } else if out.len() == MAX_IGP_DIAGNOSTICS {
        out.push("diagnostic_limit_reached".into());
    }
}

fn json_depth_exceeds(input: &str, max_depth: usize) -> Result<bool> {
    let mut depth = 0usize;
    let mut deepest = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for byte in input.as_bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match *byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth = depth.saturating_add(1);
                deepest = deepest.max(depth);
                if deepest > max_depth {
                    return Ok(true);
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(false)
}

fn parse_bounded_json(input: &str) -> Result<Value> {
    if input.len() > MAX_IGP_INPUT_BYTES {
        bail!("igp input exceeds {} byte bound", MAX_IGP_INPUT_BYTES);
    }
    if json_depth_exceeds(input, MAX_JSON_DEPTH)? {
        bail!("igp input exceeds JSON nesting depth bound");
    }
    serde_json::from_str(input).context("igp operational output is not valid JSON")
}

fn vrf_root_objects<'a>(root: &'a Value, key: &str) -> Vec<(String, &'a Value)> {
    if root.get(key).is_some() {
        return vec![("default".into(), root)];
    }
    let object = match root.as_object() {
        Some(object) => object,
        None => return Vec::new(),
    };
    object
        .iter()
        .take(MAX_VRF_CONTEXTS)
        .filter_map(|(name, value)| {
            if value.get(key).is_some() {
                let vrf = s(value, "vrfName").unwrap_or_else(|| name.clone());
                Some((vrf, value))
            } else {
                None
            }
        })
        .collect()
}

fn ospf_lsdb_vrf_roots<'a>(root: &'a Value) -> Vec<(String, &'a Value)> {
    const LSA_KEYS: [&str; 7] = [
        "routerLinkStates",
        "networkLinkStates",
        "summaryLinkStates",
        "asbrSummaryLinkStates",
        "nssaExternalLinkStates",
        "asExternalLinkStates",
        "areas",
    ];
    let has_lsa_or_identity = |value: &Value| {
        value.get("routerId").is_some()
            || value.get("vrfName").is_some()
            || LSA_KEYS.iter().any(|key| value.get(key).is_some())
    };
    if has_lsa_or_identity(root) {
        return vec![("default".into(), root)];
    }
    let object = match root.as_object() {
        Some(object) => object,
        None => return Vec::new(),
    };
    object
        .iter()
        .take(MAX_VRF_CONTEXTS)
        .filter_map(|(name, value)| {
            if value.is_object() && has_lsa_or_identity(value) {
                let vrf = s(value, "vrfName").unwrap_or_else(|| name.clone());
                Some((vrf, value))
            } else {
                None
            }
        })
        .collect()
}

fn missing(entry: &mut IgpAdjacencyEvidence, field: &str) {
    if entry.missing_or_invalid_fields.len() < 32 {
        entry.missing_or_invalid_fields.push(field.into());
    }
}

pub fn parse_frr_ospf_neighbors(
    context: &IgpCaptureContext,
    input: &str,
) -> Result<IgpNormalization<IgpAdjacencyEvidence>> {
    let root = parse_bounded_json(input)?;
    let mut out = IgpNormalization::new(input);
    let roots = vrf_root_objects(&root, "neighbors");
    if roots.is_empty() {
        out.scope_absent_or_inactive = true;
        push_diag(
            &mut out.diagnostics,
            "ospf instance absent or not running in neighbor output",
        );
        return Ok(out);
    }
    let mut invalid_population_evidence = false;
    for (vrf, vrf_root) in roots {
        let Some(neighbors) = obj(vrf_root, "neighbors") else {
            continue;
        };
        for (router_id, neighbor_list) in neighbors.iter() {
            if out.entries.len() >= MAX_IGP_ADJACENCIES {
                out.input_record_limit_reached = true;
                push_diag(&mut out.diagnostics, "ospf_adjacency_limit_reached");
                return Ok(out);
            }
            let Some(records) = neighbor_list.as_array() else {
                invalid_population_evidence = true;
                push_diag(
                    &mut out.diagnostics,
                    format!("ospf neighbor list for {router_id} is not an array"),
                );
                continue;
            };
            for record in records.iter().take(MAX_IGP_ADJACENCIES) {
                if out.entries.len() >= MAX_IGP_ADJACENCIES {
                    out.input_record_limit_reached = true;
                    push_diag(&mut out.diagnostics, "ospf_adjacency_limit_reached");
                    break;
                }
                let (remote_identity, identity_kind, neighbor_router_id) = if router_id == "noNbrId"
                {
                    (
                        router_id.clone(),
                        "ospf_neighbor_source_key_without_router_id",
                        None,
                    )
                } else {
                    (
                        router_id.clone(),
                        "ospf_neighbor_router_id",
                        Some(router_id.clone()),
                    )
                };
                let neighbor_router_id_missing = neighbor_router_id.is_none();
                let mut entry = base_adjacency(
                    IgpProtocol::Ospfv2,
                    context,
                    input,
                    remote_identity,
                    identity_kind,
                );
                entry.network_instance = Some(vrf.clone());
                entry.remote_interface_address = s(record, "ifaceAddress");
                entry.local_interface_address = s(record, "localIfaceAddress");
                entry.local_interface_name = s(record, "ifaceName");
                entry.state = s(record, "nbrState");
                entry.convergence_state = s(record, "converged");
                let details = OspfAdjacencyDetails {
                    router_id: s(record, "routerId"),
                    neighbor_router_id,
                    area_id: s(record, "areaId").or_else(|| s(record, "area")),
                    network_type: s(record, "networkType"),
                    role: s(record, "role"),
                    priority: u64_field(record, "nbrPriority"),
                    cost: u64_field(record, "cost").or_else(|| u64_field(record, "linkCost")),
                    hello_timer_msec: u64_field(record, "helloTimerMsecs")
                        .or_else(|| u64_field(record, "helloTimerMsec")),
                    dead_timer_msec: u64_field(record, "deadTimeMsecs")
                        .or_else(|| u64_field(record, "deadTimerMsecs"))
                        .or_else(|| u64_field(record, "routerDeadIntervalTimerDueMsec")),
                    uptime_msec: u64_field(record, "upTimeInMsec")
                        .or_else(|| u64_field(record, "upTimeInMsecs")),
                    state_change_counter: u64_field(record, "stateChangeCounter"),
                };
                if entry.local_interface_name.is_none() {
                    missing(&mut entry, "ifaceName");
                }
                if entry.state.is_none() {
                    missing(&mut entry, "nbrState");
                }
                if neighbor_router_id_missing {
                    missing(&mut entry, "neighborRouterId");
                }
                entry.ospf = Some(details);
                out.entries.push(entry);
            }
        }
    }
    out.positive_empty_population = out.entries.is_empty() && !invalid_population_evidence;
    Ok(out)
}

fn parse_isis_holdtime(record: &Value) -> Option<u64> {
    if let Some(value) = u64_field(record, "holdtime") {
        return Some(value);
    }
    let expires = s(record, "expires-in")?;
    // FRR renders HH:MM:SS in the detail path. Keep the exact string separately
    // and expose seconds only when the shape is unambiguous.
    let mut parts = expires.split(':');
    let hours = parts.next()?.parse::<u64>().ok()?;
    let minutes = parts.next()?.parse::<u64>().ok()?;
    let seconds = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() || minutes > 59 || seconds > 59 {
        return None;
    }
    Some(
        hours
            .saturating_mul(3600)
            .saturating_add(minutes * 60)
            .saturating_add(seconds),
    )
}

pub fn parse_frr_isis_neighbors(
    context: &IgpCaptureContext,
    input: &str,
) -> Result<IgpNormalization<IgpAdjacencyEvidence>> {
    let root = parse_bounded_json(input)?;
    let mut out = IgpNormalization::new(input);
    let Some(areas) = arr(&root, "areas") else {
        push_diag(
            &mut out.diagnostics,
            "isis neighbor output has no areas array",
        );
        return Ok(out);
    };
    if areas.is_empty() {
        out.scope_absent_or_inactive = true;
        push_diag(
            &mut out.diagnostics,
            "isis has no configured areas in neighbor output",
        );
        return Ok(out);
    }
    for area in areas.iter().take(MAX_VRF_CONTEXTS) {
        let area_name = obj(area, "area")
            .and_then(|a| s(&Value::Object(a.clone()), "name"))
            .or_else(|| s(area, "area"));
        let Some(circuits) = arr(area, "circuits") else {
            continue;
        };
        for circuit in circuits {
            if out.entries.len() >= MAX_IGP_ADJACENCIES {
                out.input_record_limit_reached = true;
                push_diag(&mut out.diagnostics, "isis_adjacency_limit_reached");
                return Ok(out);
            }
            let detail_interface = obj(circuit, "interface");
            let interface_name = detail_interface
                .and_then(|iface| iface.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .or_else(|| s(circuit, "interface"));
            let adjacency_identity = match s(circuit, "adj") {
                Some(identity) if !identity.is_empty() => identity,
                _ => {
                    push_diag(&mut out.diagnostics, "isis adjacency missing adj identity");
                    continue;
                }
            };
            let mut entry = base_adjacency(
                IgpProtocol::Isis,
                context,
                input,
                adjacency_identity,
                "frr_isis_adjacency_label_hostname_or_system_id",
            );
            entry.protocol_instance = area_name.clone();
            entry.local_interface_name = interface_name;
            let state_value = detail_interface
                .and_then(|iface| iface.get("state").and_then(Value::as_str))
                .map(str::to_owned)
                .or_else(|| s(circuit, "state"));
            entry.state = state_value;
            let level = detail_interface
                .and_then(|iface| iface.get("level").and_then(Value::as_u64))
                .or_else(|| u64_field(circuit, "level"))
                .and_then(|level| u8::try_from(level).ok());
            let mut topologies = Vec::new();
            if let Some(topos) = detail_interface
                .and_then(|iface| iface.get("topologies"))
                .and_then(Value::as_object)
            {
                for value in topos.values().take(64) {
                    if let Some(topology) = value.as_str() {
                        topologies.push(topology.to_owned());
                    }
                }
            }
            let mut protocols = Vec::new();
            if let Some(speaks) = detail_interface
                .and_then(|iface| iface.get("speaks"))
                .and_then(Value::as_str)
            {
                protocols = speaks
                    .split(',')
                    .map(|p| p.trim().to_owned())
                    .filter(|p| !p.is_empty())
                    .collect();
            }
            entry.isis = Some(IsisAdjacencyDetails {
                neighbor_system_id: None,
                circuit_identity: detail_interface
                    .and_then(|iface| iface.get("lan-id").and_then(Value::as_str))
                    .map(str::to_owned),
                level,
                circuit_type: detail_interface
                    .and_then(|iface| iface.get("circuit-type").and_then(Value::as_str))
                    .map(str::to_owned),
                metric: u64_field(circuit, "metric"),
                holdtime_seconds: detail_interface
                    .and_then(|iface| {
                        iface
                            .get("expires-in")
                            .map(|_| Value::Object(iface.clone()))
                    })
                    .and_then(|iface| parse_isis_holdtime(&iface))
                    .or_else(|| parse_isis_holdtime(circuit)),
                uptime_or_last_change: detail_interface
                    .and_then(|iface| iface.get("last-ago").and_then(Value::as_str))
                    .map(str::to_owned),
                topologies,
                protocols,
                snpa: detail_interface
                    .and_then(|iface| iface.get("snpa").and_then(Value::as_str))
                    .map(str::to_owned)
                    .or_else(|| s(circuit, "snpa")),
            });
            if entry.local_interface_name.is_none() {
                missing(&mut entry, "interface");
            }
            if entry.state.is_none() {
                missing(&mut entry, "state");
            }
            out.entries.push(entry);
        }
    }
    out.positive_empty_population = out.entries.is_empty();
    Ok(out)
}

fn copy_extension_fields(source: &Map<String, Value>, consumed: &[&str]) -> Map<String, Value> {
    source
        .iter()
        .filter(|(key, _)| !consumed.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn contiguous_ipv4_mask_len(mask: &str) -> Option<u64> {
    let bits = u32::from(mask.parse::<Ipv4Addr>().ok()?);
    let len = bits.count_ones();
    let expected = if len == 0 {
        0
    } else {
        u32::MAX.checked_shl(32 - len).unwrap_or(0)
    };
    (bits == expected).then_some(len as u64)
}

fn ipv4_prefix_from_network_mask(network: &str, mask: &str) -> Option<String> {
    contiguous_ipv4_mask_len(mask).map(|len| format!("{network}/{len}"))
}

fn parse_ospf_network_prefix(link_state_id: &str, record: &Value) -> Option<String> {
    if let Some(prefix) = s(record, "prefix").or_else(|| s(record, "summaryAddress")) {
        return Some(prefix);
    }
    let mask = u64_field(record, "networkMask").or_else(|| {
        s(record, "networkMask").and_then(|dotted| contiguous_ipv4_mask_len(&dotted))
    })?;
    if mask > 32 {
        return None;
    }
    let address = u32::from(link_state_id.parse::<Ipv4Addr>().ok()?);
    let network = if mask == 0 {
        0
    } else {
        address & u32::MAX.checked_shl(32 - mask as u32).unwrap_or(0)
    };
    Some(format!("{}/{}", Ipv4Addr::from(network), mask))
}

fn ospf_link_semantics(item: &Value) -> (String, Option<String>, Option<String>, Option<u64>) {
    let raw_type = s(item, "linkType").unwrap_or_else(|| "unknown".into());
    let lower = raw_type.to_ascii_lowercase();
    let canonical = if lower.contains("point-to-point") || raw_type == "pointToPoint" {
        "point_to_point"
    } else if lower.contains("virtual") || raw_type == "virtualLink" {
        "virtual_link"
    } else if lower.contains("transit") || raw_type == "transitNetwork" {
        "transit_network"
    } else if lower.contains("stub") || raw_type == "stubNetwork" {
        "stub_network"
    } else {
        "unknown"
    };
    let link_id = match canonical {
        "point_to_point" | "virtual_link" => s(item, "neighborRouterId")
            .or_else(|| s(item, "linkId"))
            .or_else(|| s(item, "linkID")),
        "transit_network" => s(item, "designatedRouterAddress")
            .or_else(|| s(item, "linkId"))
            .or_else(|| s(item, "linkID")),
        "stub_network" => s(item, "networkAddress")
            .or_else(|| s(item, "linkId"))
            .or_else(|| s(item, "linkID")),
        _ => s(item, "linkId").or_else(|| s(item, "linkID")),
    };
    let link_data = match canonical {
        "stub_network" => s(item, "networkMask").or_else(|| s(item, "linkData")),
        "point_to_point" | "virtual_link" | "transit_network" => {
            s(item, "routerInterfaceAddress").or_else(|| s(item, "linkData"))
        }
        _ => s(item, "linkData"),
    };
    let metric = u64_field(item, "tos0Metric")
        .or_else(|| u64_field(item, "metric"))
        .or_else(|| u64_field(item, "linkMetric"));
    (canonical.into(), link_id, link_data, metric)
}

fn parse_one_ospf_router_link(
    item: &Value,
    out: &mut Vec<LinkStateRelationship>,
    prefixes: &mut Vec<LinkStatePrefix>,
    diagnostics: &mut Vec<String>,
) {
    if out.len() >= MAX_IGP_RELATIONSHIPS {
        push_diag(diagnostics, "ospf_relationship_limit_reached");
        return;
    }
    let (link_type, link_id, link_data, metric) = ospf_link_semantics(item);
    let Some(remote) = link_id.clone() else {
        push_diag(diagnostics, "ospf router link lacks typed link identity");
        return;
    };
    let identity_kind = match link_type.as_str() {
        "point_to_point" | "virtual_link" => "ospf_router_id",
        "transit_network" => "ospf_designated_router_interface_address",
        "stub_network" => "ospf_network_address",
        _ => "ospf_link_identifier_uninterpreted",
    };
    if link_type == "stub_network" && prefixes.len() < MAX_IGP_PREFIXES {
        if let (Some(network), Some(mask)) = (link_id.as_deref(), link_data.as_deref()) {
            if let Some(prefix) = ipv4_prefix_from_network_mask(network, mask) {
                prefixes.push(LinkStatePrefix {
                    prefix,
                    metric,
                    topology_id: None,
                    external: false,
                    address_family: Some("ipv4-unicast".into()),
                    extensions: Map::new(),
                });
            } else {
                push_diag(
                    diagnostics,
                    "ospf stub network has non-contiguous or invalid IPv4 mask",
                );
            }
        }
    }
    let mut extensions = item
        .as_object()
        .map(|object| {
            copy_extension_fields(
                object,
                &[
                    "linkType",
                    "linkId",
                    "linkID",
                    "linkData",
                    "neighborRouterId",
                    "designatedRouterAddress",
                    "networkAddress",
                    "networkMask",
                    "routerInterfaceAddress",
                    "metric",
                    "linkMetric",
                    "tos0Metric",
                ],
            )
        })
        .unwrap_or_default();
    if let Some(raw_type) = s(item, "linkType") {
        extensions.insert("rawLinkType".into(), Value::String(raw_type));
    }
    out.push(LinkStateRelationship {
        remote_identity: remote,
        identity_kind: identity_kind.into(),
        relationship_type: link_type.clone(),
        metric,
        local_interface_address: if matches!(
            link_type.as_str(),
            "point_to_point" | "transit_network" | "virtual_link"
        ) {
            link_data
        } else {
            None
        },
        remote_interface_address: if identity_kind == "ospf_designated_router_interface_address" {
            link_id
        } else {
            None
        },
        topology_id: None,
        extensions,
    });
}

fn parse_ospf_relationships(
    record: &Value,
    out: &mut Vec<LinkStateRelationship>,
    prefixes: &mut Vec<LinkStatePrefix>,
    diagnostics: &mut Vec<String>,
) {
    let Some(links) = obj(record, "routerLinks") else {
        return;
    };
    for (_link_key, value) in links.iter().take(MAX_IGP_RELATIONSHIPS) {
        if let Some(items) = value.as_array() {
            for item in items {
                parse_one_ospf_router_link(item, out, prefixes, diagnostics);
            }
        } else if value.is_object() {
            parse_one_ospf_router_link(value, out, prefixes, diagnostics);
        } else {
            push_diag(
                diagnostics,
                "ospf routerLinks member is not an object or array",
            );
        }
    }
}

fn parse_ospf_lsa(
    context: &IgpCaptureContext,
    input: &str,
    network_instance: &str,
    area: Option<String>,
    lsa_type: &str,
    record: &Value,
    diagnostics: &mut Vec<String>,
) -> Option<LinkStateEvidence> {
    let identity = s(record, "linkStateId").or_else(|| s(record, "linkStateID"));
    let advertising = s(record, "advertisingRouter");
    let (identity, advertising) = match (identity, advertising) {
        (Some(identity), Some(advertising)) => (identity, advertising),
        _ => {
            push_diag(
                diagnostics,
                format!("ospf {lsa_type} LSA lacks linkStateId or advertisingRouter"),
            );
            return None;
        }
    };
    let mut entry = base_lsdb(
        IgpProtocol::Ospfv2,
        context,
        input,
        advertising,
        identity.clone(),
    );
    entry.network_instance = Some(network_instance.into());
    entry.area = area;
    entry.lsa_type = Some(lsa_type.into());
    entry.age_seconds = u64_field(record, "lsaAge");
    entry.sequence = s(record, "lsaSeqNumber")
        .or_else(|| u64_field(record, "lsaSeqNumber").map(|v| v.to_string()));
    parse_ospf_relationships(
        record,
        &mut entry.advertised_relationships,
        &mut entry.reachability_prefixes,
        diagnostics,
    );

    if lsa_type == "network" {
        let attached = obj(record, "attchedRouters").or_else(|| obj(record, "attachedRouters"));
        if let Some(attached) = attached {
            for router in attached.values().take(MAX_IGP_RELATIONSHIPS) {
                if entry.advertised_relationships.len() >= MAX_IGP_RELATIONSHIPS {
                    push_diag(diagnostics, "ospf_relationship_limit_reached");
                    break;
                }
                if let Some(remote) = s(router, "attachedRouterId") {
                    entry.advertised_relationships.push(LinkStateRelationship {
                        remote_identity: remote,
                        identity_kind: "ospf_router_id".into(),
                        relationship_type: "attachedRouter".into(),
                        metric: None,
                        local_interface_address: None,
                        remote_interface_address: None,
                        topology_id: None,
                        extensions: Map::new(),
                    });
                }
            }
        }
    }

    if let Some(prefix) = parse_ospf_network_prefix(&identity, record) {
        entry.reachability_prefixes.push(LinkStatePrefix {
            prefix,
            metric: u64_field(record, "metric")
                .or_else(|| u64_field(record, "lsaMetric"))
                .or_else(|| u64_field(record, "tos0Metric")),
            topology_id: None,
            external: lsa_type.contains("external"),
            address_family: Some("ipv4-unicast".into()),
            extensions: Map::new(),
        });
    }
    if entry.reachability_prefixes.len() > MAX_IGP_PREFIXES {
        push_diag(diagnostics, "ospf_prefix_limit_reached");
        entry.reachability_prefixes.truncate(MAX_IGP_PREFIXES);
    }
    let consumed = [
        "linkStateId",
        "linkStateID",
        "advertisingRouter",
        "lsaAge",
        "lsaSeqNumber",
        "routerLinks",
        "attchedRouters",
        "attachedRouters",
        "prefix",
        "summaryAddress",
        "networkMask",
        "metric",
        "lsaMetric",
    ];
    let object = record.as_object()?;
    entry.extensions = copy_extension_fields(object, &consumed);
    Some(entry)
}

pub fn parse_frr_ospf_lsdb(
    context: &IgpCaptureContext,
    input: &str,
) -> Result<IgpNormalization<LinkStateEvidence>> {
    let root = parse_bounded_json(input)?;
    let mut out = IgpNormalization::new(input);
    let lsa_groups = [
        ("routerLinkStates", "router"),
        ("networkLinkStates", "network"),
        ("summaryLinkStates", "summary_network"),
        ("asbrSummaryLinkStates", "summary_asbr"),
        ("nssaExternalLinkStates", "nssa_external"),
        ("asExternalLinkStates", "as_external"),
        ("groupMembershipLsa", "group_membership"),
        ("type8Lsa", "type8"),
        ("linkLocalOpaqueLsa", "opaque_link_local"),
        ("areaLocalOpaqueLsa", "opaque_area_local"),
        ("asExternalOpaqueLsa", "opaque_as_external"),
    ];
    let roots = ospf_lsdb_vrf_roots(&root);
    if roots.is_empty() {
        out.scope_absent_or_inactive = true;
        push_diag(
            &mut out.diagnostics,
            "ospf instance absent or not running in database output",
        );
        return Ok(out);
    }
    let mut lsdb_records_observed = false;
    let mut invalid_population_evidence = false;
    for (vrf, vrf_root) in roots {
        // Summary output nests LSA-type arrays under areas; detail output nests
        // per-area arrays below each LSA-type object. Support both genuine FRR
        // shapes rather than prescribing one rendering.
        if let Some(summary_areas) = vrf_root.get("areas").and_then(Value::as_object) {
            for (area, area_root) in summary_areas.iter().take(MAX_AREA_CONTEXTS) {
                let Some(area_root) = area_root.as_object() else {
                    continue;
                };
                for (group_key, lsa_type) in lsa_groups {
                    let Some(records) = area_root.get(group_key).and_then(Value::as_array) else {
                        continue;
                    };
                    for record in records {
                        lsdb_records_observed = true;
                        if out.entries.len() >= MAX_IGP_LSDB_ENTRIES {
                            out.input_record_limit_reached = true;
                            push_diag(&mut out.diagnostics, "ospf_lsdb_entry_limit_reached");
                            return Ok(out);
                        }
                        if let Some(entry) = parse_ospf_lsa(
                            context,
                            input,
                            &vrf,
                            Some(area.clone()),
                            lsa_type,
                            record,
                            &mut out.diagnostics,
                        ) {
                            out.entries.push(entry);
                        }
                    }
                }
            }
        }
        for (group_key, lsa_type) in lsa_groups {
            let Some(group) = vrf_root.get(group_key) else {
                continue;
            };
            if let Some(areas) = group.get("areas").and_then(Value::as_object) {
                for (area, records) in areas.iter().take(MAX_AREA_CONTEXTS) {
                    let Some(records) = records.as_array() else {
                        invalid_population_evidence = true;
                        push_diag(
                            &mut out.diagnostics,
                            format!("ospf {group_key} area {area} is not an array"),
                        );
                        continue;
                    };
                    for record in records {
                        lsdb_records_observed = true;
                        if out.entries.len() >= MAX_IGP_LSDB_ENTRIES {
                            out.input_record_limit_reached = true;
                            push_diag(&mut out.diagnostics, "ospf_lsdb_entry_limit_reached");
                            return Ok(out);
                        }
                        if let Some(entry) = parse_ospf_lsa(
                            context,
                            input,
                            &vrf,
                            Some(area.clone()),
                            lsa_type,
                            record,
                            &mut out.diagnostics,
                        ) {
                            out.entries.push(entry);
                        }
                    }
                }
            } else if let Some(records) = group.as_array() {
                for record in records {
                    lsdb_records_observed = true;
                    if out.entries.len() >= MAX_IGP_LSDB_ENTRIES {
                        out.input_record_limit_reached = true;
                        push_diag(&mut out.diagnostics, "ospf_lsdb_entry_limit_reached");
                        return Ok(out);
                    }
                    if let Some(entry) = parse_ospf_lsa(
                        context,
                        input,
                        &vrf,
                        None,
                        lsa_type,
                        record,
                        &mut out.diagnostics,
                    ) {
                        out.entries.push(entry);
                    }
                }
            }
        }
    }
    out.positive_empty_population =
        out.entries.is_empty() && !lsdb_records_observed && !invalid_population_evidence;
    Ok(out)
}

fn value_map(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn parse_isis_lsp(
    context: &IgpCaptureContext,
    input: &str,
    area_name: Option<String>,
    level: Option<u8>,
    record: &Value,
    diagnostics: &mut Vec<String>,
) -> Option<LinkStateEvidence> {
    let lsp_object = obj(record, "lsp")?;
    let identity = lsp_object.get("id").and_then(Value::as_str)?.to_owned();
    // Do not infer a system identifier from a dynamic hostname. When the
    // rendered LSP identity has the canonical hex system-id shape, preserve
    // its first three system-id components; otherwise retain the complete
    // rendered LSP identity as the advertisement identity.
    let advertising = {
        let before_fragment = identity.split('-').next().unwrap_or(identity.as_str());
        let components: Vec<&str> = before_fragment.split('.').collect();
        if components.len() >= 4
            && components[..3]
                .iter()
                .all(|part| part.len() == 4 && part.chars().all(|c| c.is_ascii_hexdigit()))
        {
            components[..3].join(".")
        } else {
            identity.clone()
        }
    };
    let mut entry = base_lsdb(IgpProtocol::Isis, context, input, advertising, identity);
    entry.protocol_instance = area_name;
    entry.level = level;
    entry.sequence = s(record, "seqNumber")
        .or_else(|| u64_field(record, "seqNumber").map(|v| format!("0x{v:08x}")));
    entry.remaining_lifetime_seconds = u64_field(record, "holdtime");

    let mut add_relationship =
        |item: &Value, relationship_type: &str, identity_key: &str, metric_key: &str| {
            if entry.advertised_relationships.len() >= MAX_IGP_RELATIONSHIPS {
                push_diag(diagnostics, "isis_relationship_limit_reached");
                return;
            }
            let Some(remote) = s(item, identity_key) else {
                push_diag(
                    diagnostics,
                    format!("isis {relationship_type} lacks remote identity"),
                );
                return;
            };
            let mut extensions = value_map(item);
            extensions.remove(identity_key);
            extensions.remove(metric_key);
            extensions.remove("mtId");
            let topology_id = s(item, "mtName").or_else(|| s(item, "mtId"));
            entry.advertised_relationships.push(LinkStateRelationship {
                remote_identity: remote,
                identity_kind: "isis_pseudonode_or_system_identifier".into(),
                relationship_type: relationship_type.into(),
                metric: u64_field(item, metric_key),
                local_interface_address: s(item, "localIfaceIp")
                    .or_else(|| s(item, "localIfaceIpv6")),
                remote_interface_address: s(item, "remoteIfaceIp")
                    .or_else(|| s(item, "remoteIfaceIpv6")),
                topology_id,
                extensions,
            });
        };
    for item in arr(record, "extReach").into_iter().flatten() {
        add_relationship(item, "extended_is_reachability", "id", "metric");
    }
    for item in arr(record, "oldReachStyle").into_iter().flatten() {
        add_relationship(item, "old_style_is_reachability", "isReach", "metric");
    }
    let mut add_prefix = |item: &Value, prefix_key: &str, metric_key: &str, family: &str| {
        if entry.reachability_prefixes.len() >= MAX_IGP_PREFIXES {
            push_diag(diagnostics, "isis_prefix_limit_reached");
            return;
        }
        let Some(prefix) = s(item, prefix_key) else {
            push_diag(diagnostics, "isis reachability item lacks prefix");
            return;
        };
        let mut extensions = value_map(item);
        extensions.remove(prefix_key);
        extensions.remove(metric_key);
        extensions.remove("mtId");
        entry.reachability_prefixes.push(LinkStatePrefix {
            prefix,
            metric: u64_field(item, metric_key),
            topology_id: s(item, "mtName").or_else(|| s(item, "mtId")),
            external: item
                .get("external")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            address_family: Some(family.into()),
            extensions,
        });
    };
    for item in arr(record, "extIpReach").into_iter().flatten() {
        add_prefix(item, "ipReach", "ipReachMetric", "ipv4-unicast");
    }
    for item in arr(record, "ipv6Reach").into_iter().flatten() {
        add_prefix(item, "prefix", "metric", "ipv6-unicast");
    }
    for item in arr(record, "oldIpReachStyle").into_iter().flatten() {
        add_prefix(item, "prefix", "metric", "ipv4-unicast");
    }

    if entry.advertised_relationships.is_empty() && entry.reachability_prefixes.is_empty() {
        entry
            .missing_or_invalid_fields
            .push("no_decoded_adjacency_or_prefix_tlvs".into());
    }
    let consumed = [
        "lsp",
        "pduLen",
        "seqNumber",
        "chksum",
        "holdtime",
        "attPOl",
        "extReach",
        "oldReachStyle",
        "extIpReach",
        "ipv6Reach",
        "oldIpReachStyle",
    ];
    entry.extensions = copy_extension_fields(record.as_object()?, &consumed);
    if let Some(te_router_id) = s(record, "teRouterId") {
        entry
            .extensions
            .insert("teRouterId".into(), Value::String(te_router_id));
    }
    Some(entry)
}

pub fn parse_frr_isis_lsdb(
    context: &IgpCaptureContext,
    input: &str,
) -> Result<IgpNormalization<LinkStateEvidence>> {
    let root = parse_bounded_json(input)?;
    let mut out = IgpNormalization::new(input);
    let Some(areas) = arr(&root, "areas") else {
        push_diag(
            &mut out.diagnostics,
            "isis database output has no areas array",
        );
        return Ok(out);
    };
    if areas.is_empty() {
        out.scope_absent_or_inactive = true;
        push_diag(
            &mut out.diagnostics,
            "isis has no configured areas in database output",
        );
        return Ok(out);
    }
    let mut lsp_records_observed = false;
    for area in areas.iter().take(MAX_VRF_CONTEXTS) {
        let area_name = obj(area, "area")
            .and_then(|a| a.get("name").and_then(Value::as_str))
            .map(str::to_owned)
            .or_else(|| s(area, "area"));
        let Some(levels) = arr(area, "levels") else {
            continue;
        };
        for level_object in levels {
            let level = u64_field(level_object, "id").and_then(|v| u8::try_from(v).ok());
            let Some(lsps) = arr(level_object, "lsps") else {
                continue;
            };
            for record in lsps {
                lsp_records_observed = true;
                if out.entries.len() >= MAX_IGP_LSDB_ENTRIES {
                    out.input_record_limit_reached = true;
                    push_diag(&mut out.diagnostics, "isis_lsdb_entry_limit_reached");
                    return Ok(out);
                }
                match parse_isis_lsp(
                    context,
                    input,
                    area_name.clone(),
                    level,
                    record,
                    &mut out.diagnostics,
                ) {
                    Some(entry) => out.entries.push(entry),
                    None => push_diag(&mut out.diagnostics, "isis LSP lacks lsp.id identity"),
                }
            }
        }
    }
    out.positive_empty_population = out.entries.is_empty() && !lsp_records_observed;
    Ok(out)
}

fn command_output<'a>(
    envelope: &'a Value,
    command_key: &str,
) -> Option<(String, &'a Map<String, Value>)> {
    let command = obj(envelope, "commands")?.get(command_key)?.as_object()?;
    let outcome = command
        .get("outcome")
        .and_then(Value::as_str)
        .unwrap_or("missing");
    if outcome != "complete" {
        return Some((String::new(), command));
    }
    let output = command.get("output").and_then(Value::as_str)?.to_owned();
    if let Some(expected) = command.get("output_sha256").and_then(Value::as_str) {
        if expected != sha256_hex(output.as_bytes()) {
            return None;
        }
    }
    Some((output, command))
}

fn family_summary(
    family: &str,
    command: &str,
    outcome: &str,
    entries: usize,
    diagnostics: Vec<String>,
) -> IgpFamilyNormalizationSummary {
    IgpFamilyNormalizationSummary {
        family: family.into(),
        command: command.into(),
        outcome: outcome.into(),
        entries,
        positive_empty_population: outcome == "complete" && entries == 0,
        diagnostics,
    }
}

/// Normalize the fixed four-command `frr_igp_capture.py` envelope. Digest
/// inconsistency or malformed JSON prevents use of the affected command; it is
/// never converted into an empty successful population.
pub fn parse_frr_igp_capture_envelope(
    base_context: &IgpCaptureContext,
    input: &str,
) -> Result<IgpCaptureNormalization> {
    let envelope = parse_bounded_json(input)?;
    let software_release = s(&envelope, "release_raw").or(base_context.software_release.clone());
    let batch = obj(&envelope, "acquisition_batch");
    let acquired_at_ns = batch
        .and_then(|b| b.get("completed_at_ns"))
        .and_then(Value::as_i64);
    let mut normalization = IgpCaptureNormalization {
        adjacencies: Vec::new(),
        link_state_entries: Vec::new(),
        families: Vec::new(),
        diagnostics: Vec::new(),
    };
    let families = [
        ("ospf_neighbors", FRR_OSPF_NEIGHBOR_COMMAND),
        ("ospf_lsdb", FRR_OSPF_LSDB_COMMAND),
        ("isis_neighbors", FRR_ISIS_NEIGHBOR_COMMAND),
        ("isis_lsdb", FRR_ISIS_LSDB_COMMAND),
    ];
    for (family, command) in families {
        let mut context = base_context.clone();
        context.command = command.into();
        context.software_release = software_release.clone();
        context.acquired_at_ns = acquired_at_ns.or(base_context.acquired_at_ns);
        if context.artifact_identity.is_none() {
            context.artifact_identity = Some(format!(
                "frr-igp-envelope-sha256:{}",
                sha256_hex(input.as_bytes())
            ));
        }
        let Some((output, metadata)) = command_output(&envelope, family) else {
            push_diag(
                &mut normalization.diagnostics,
                format!("{family} output missing or digest invalid"),
            );
            normalization.families.push(family_summary(
                family,
                command,
                "evidence_invalid",
                0,
                vec!["command output missing or digest mismatch".into()],
            ));
            continue;
        };
        let outcome = metadata
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("missing");
        if outcome != "complete" {
            normalization
                .families
                .push(family_summary(family, command, outcome, 0, Vec::new()));
            continue;
        }
        let result = match family {
            "ospf_neighbors" => parse_frr_ospf_neighbors(&context, &output).map(|parsed| {
                let entries = parsed.entries.len();
                normalization.adjacencies.extend(parsed.entries);
                (
                    entries,
                    parsed.diagnostics,
                    parsed.positive_empty_population,
                )
            }),
            "ospf_lsdb" => parse_frr_ospf_lsdb(&context, &output).map(|parsed| {
                let entries = parsed.entries.len();
                normalization.link_state_entries.extend(parsed.entries);
                (
                    entries,
                    parsed.diagnostics,
                    parsed.positive_empty_population,
                )
            }),
            "isis_neighbors" => parse_frr_isis_neighbors(&context, &output).map(|parsed| {
                let entries = parsed.entries.len();
                normalization.adjacencies.extend(parsed.entries);
                (
                    entries,
                    parsed.diagnostics,
                    parsed.positive_empty_population,
                )
            }),
            "isis_lsdb" => parse_frr_isis_lsdb(&context, &output).map(|parsed| {
                let entries = parsed.entries.len();
                normalization.link_state_entries.extend(parsed.entries);
                (
                    entries,
                    parsed.diagnostics,
                    parsed.positive_empty_population,
                )
            }),
            _ => unreachable!("fixed IGP family set"),
        };
        match result {
            Ok((entries, diagnostics, positive_empty)) => {
                let mut summary = family_summary(family, command, outcome, entries, diagnostics);
                summary.positive_empty_population = positive_empty;
                normalization.families.push(summary);
            }
            Err(error) => {
                push_diag(
                    &mut normalization.diagnostics,
                    format!("{family} normalization failed: {error}"),
                );
                normalization.families.push(family_summary(
                    family,
                    command,
                    "evidence_invalid",
                    0,
                    vec![error.to_string()],
                ));
            }
        }
    }
    Ok(normalization)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn context(command: &str) -> IgpCaptureContext {
        let mut context = IgpCaptureContext::new(command, "synthetic_integration_control");
        context.canonical_device = Some("fixture-device".into());
        context.artifact_identity = Some("synthetic-fixture-v1".into());
        context.acquired_at_ns = Some(1_700_000_000_000_000_000);
        context
    }

    #[test]
    fn ospf_neighbors_preserve_two_way_and_full_states() {
        let input = r#"{"default":{"vrfName":"default","neighbors":{"192.0.2.2":[{"ifaceAddress":"10.0.0.1","ifaceName":"eth1","nbrState":"Full/-","converged":"Full","role":"DR","nbrPriority":1,"deadTimeMsecs":40000,"upTimeInMsec":1000}],"192.0.2.3":[{"ifaceAddress":"10.0.0.2","ifaceName":"eth1","nbrState":"2-Way/-","converged":"2-Way","role":"DROther","nbrPriority":1}]}}}"#;
        let parsed = parse_frr_ospf_neighbors(&context(FRR_OSPF_NEIGHBOR_COMMAND), input).unwrap();
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0].state.as_deref(), Some("Full/-"));
        assert_eq!(parsed.entries[1].state.as_deref(), Some("2-Way/-"));
        assert_eq!(
            parsed.entries[1].ospf.as_ref().unwrap().role.as_deref(),
            Some("DROther")
        );
        assert_eq!(
            parsed.entries[0].evidence_class,
            IgpEvidenceClass::DirectlyObservedAdjacency
        );
    }

    #[test]
    fn isis_neighbor_identity_is_not_reclassified_as_system_id() {
        let input = r#"{"areas":[{"area":{"name":"core"},"circuits":[{"adj":"leaf-1","interface":{"name":"eth1","state":"Up","circuit-type":"level-2-only","snpa":"2020.2020.2020.03","topologies":{"topo-0":"ipv4-unicast"},"speaks":"IPv4"},"level":2,"expires-in":"00:00:09"}]}]}"#;
        let parsed = parse_frr_isis_neighbors(&context(FRR_ISIS_NEIGHBOR_COMMAND), input).unwrap();
        assert_eq!(parsed.entries.len(), 1);
        let entry = &parsed.entries[0];
        assert_eq!(entry.remote_adjacency_identity, "leaf-1");
        assert_eq!(
            entry.remote_identity_kind,
            "frr_isis_adjacency_label_hostname_or_system_id"
        );
        assert_eq!(entry.isis.as_ref().unwrap().neighbor_system_id, None);
        assert_eq!(entry.isis.as_ref().unwrap().level, Some(2));
        assert_eq!(entry.isis.as_ref().unwrap().holdtime_seconds, Some(9));
    }

    #[test]
    fn malformed_and_deep_json_are_rejected_without_panic() {
        assert!(parse_frr_ospf_neighbors(&context(FRR_OSPF_NEIGHBOR_COMMAND), "{").is_err());
        let deep = "[".repeat(MAX_JSON_DEPTH + 2) + &"]".repeat(MAX_JSON_DEPTH + 2);
        assert!(parse_frr_ospf_neighbors(&context(FRR_OSPF_NEIGHBOR_COMMAND), &deep).is_err());
    }

    #[test]
    fn ospf_lsdb_preserves_advertised_links_and_boundary() {
        let input = r#"{"default":{"vrfName":"default","routerLinkStates":{"areas":{"0.0.0.0":[{"lsaAge":42,"lsaType":"router","linkStateId":"192.0.2.1","advertisingRouter":"192.0.2.1","lsaSeqNumber":"0x80000001","routerLinks":{"link0":[{"linkType":"pointToPoint","linkId":"192.0.2.2","linkData":"10.0.0.1","metric":10}]}}]}}}}"#;
        let parsed = parse_frr_ospf_lsdb(&context(FRR_OSPF_LSDB_COMMAND), input).unwrap();
        assert_eq!(parsed.entries.len(), 1);
        let entry = &parsed.entries[0];
        assert_eq!(entry.evidence_boundary, IGP_EVIDENCE_BOUNDARY);
        assert_eq!(entry.area.as_deref(), Some("0.0.0.0"));
        assert_eq!(entry.age_seconds, Some(42));
        assert_eq!(entry.advertised_relationships.len(), 1);
        assert_eq!(entry.advertised_relationships[0].metric, Some(10));
        assert_eq!(
            entry.advertised_relationships[0].relationship_type,
            "point_to_point"
        );
    }

    #[test]
    fn isis_lsdb_preserves_level_sequence_metric_prefix_and_extensions() {
        let input = r#"{"areas":[{"area":{"name":"core"},"levels":[{"id":2,"lsps":[{"lsp":{"id":"0000.0000.0002.00-00","ownLSP":true},"pduLen":120,"seqNumber":"0x00000004","chksum":"0xaaaa","holdtime":1100,"attPOl":"0/0/0","extReach":[{"mtId":"Extended","id":"0000.0000.0001.00","metric":10,"adjSid":[{"sid":24001,"weight":0}]}],"extIpReach":[{"mtId":"Extended","ipReach":"192.0.2.2/32","ipReachMetric":10,"subtlvs":{"sr":[{"index":2,"alg":0}]}}]}]}]}]}"#;
        let parsed = parse_frr_isis_lsdb(&context(FRR_ISIS_LSDB_COMMAND), input).unwrap();
        assert_eq!(parsed.entries.len(), 1);
        let entry = &parsed.entries[0];
        assert_eq!(entry.level, Some(2));
        assert_eq!(entry.sequence.as_deref(), Some("0x00000004"));
        assert_eq!(entry.remaining_lifetime_seconds, Some(1100));
        assert_eq!(entry.advertised_relationships[0].metric, Some(10));
        assert!(
            entry.advertised_relationships[0]
                .extensions
                .contains_key("adjSid")
        );
        assert_eq!(entry.reachability_prefixes[0].prefix, "192.0.2.2/32");
        assert!(
            entry.reachability_prefixes[0]
                .extensions
                .contains_key("subtlvs")
        );
    }

    #[test]
    fn capture_envelope_preserves_positive_empty_and_rejects_digest_mismatch() {
        let empty = r#"{"default":{"neighbors":{}}}"#;
        let digest = sha256_hex(empty.as_bytes());
        let envelope = format!(
            r#"{{"release_raw":"FRRouting 10.1","acquisition_batch":{{"completed_at_ns":123}},"commands":{{"ospf_neighbors":{{"outcome":"complete","output":{empty:?},"output_sha256":"{digest}"}},"ospf_lsdb":{{"outcome":"unsupported","output":null}},"isis_neighbors":{{"outcome":"complete","output":"{{}}","output_sha256":"bad"}},"isis_lsdb":{{"outcome":"not_attempted","output":null}}}}}}"#
        );
        let parsed = parse_frr_igp_capture_envelope(&context("envelope"), &envelope).unwrap();
        assert!(parsed.families[0].positive_empty_population);
        assert_eq!(parsed.families[1].outcome, "unsupported");
        assert_eq!(parsed.families[2].outcome, "evidence_invalid");
    }
    #[test]
    fn synthetic_fixture_files_parse_with_explicit_control_provenance() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/igp");
        let ospf_neighbors =
            std::fs::read_to_string(root.join("frr_ospf_neighbors.synthetic.json")).unwrap();
        let isis_neighbors =
            std::fs::read_to_string(root.join("frr_isis_neighbors.synthetic.json")).unwrap();
        let ospf_lsdb = std::fs::read_to_string(root.join("frr_ospf_lsdb.synthetic.json")).unwrap();
        let isis_lsdb = std::fs::read_to_string(root.join("frr_isis_lsdb.synthetic.json")).unwrap();

        let ospf_adj =
            parse_frr_ospf_neighbors(&context(FRR_OSPF_NEIGHBOR_COMMAND), &ospf_neighbors).unwrap();
        let isis_adj =
            parse_frr_isis_neighbors(&context(FRR_ISIS_NEIGHBOR_COMMAND), &isis_neighbors).unwrap();
        let ospf_db = parse_frr_ospf_lsdb(&context(FRR_OSPF_LSDB_COMMAND), &ospf_lsdb).unwrap();
        let isis_db = parse_frr_isis_lsdb(&context(FRR_ISIS_LSDB_COMMAND), &isis_lsdb).unwrap();

        assert_eq!(ospf_adj.entries.len(), 2);
        assert_eq!(isis_adj.entries.len(), 1);
        assert_eq!(ospf_db.entries.len(), 3);
        assert_eq!(isis_db.entries.len(), 1);
        assert!(
            ospf_adj
                .entries
                .iter()
                .all(|entry| entry.provenance.source_provenance == "synthetic_integration_control")
        );
        assert!(
            ospf_db
                .entries
                .iter()
                .all(|entry| entry.evidence_boundary == IGP_EVIDENCE_BOUNDARY)
        );
        assert_eq!(
            isis_db.entries[0]
                .extensions
                .get("teRouterId")
                .and_then(Value::as_str),
            Some("192.0.2.2")
        );
    }

    #[test]
    fn genuine_frr_detail_shape_preserves_typed_links_and_canonical_prefixes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/igp");
        let input = std::fs::read_to_string(root.join("frr_ospf_lsdb.synthetic.json")).unwrap();
        let parsed = parse_frr_ospf_lsdb(&context(FRR_OSPF_LSDB_COMMAND), &input).unwrap();
        let router = parsed
            .entries
            .iter()
            .find(|entry| entry.lsa_type.as_deref() == Some("router"))
            .unwrap();
        assert_eq!(
            router.advertised_relationships[0].relationship_type,
            "point_to_point"
        );
        assert_eq!(
            router.advertised_relationships[0].remote_identity,
            "192.0.2.2"
        );
        assert_eq!(
            router.advertised_relationships[0]
                .local_interface_address
                .as_deref(),
            Some("10.0.0.1")
        );
        assert!(
            router
                .reachability_prefixes
                .iter()
                .any(|prefix| prefix.prefix == "10.0.0.0/30")
        );
        let network = parsed
            .entries
            .iter()
            .find(|entry| entry.lsa_type.as_deref() == Some("network"))
            .unwrap();
        assert_eq!(network.reachability_prefixes[0].prefix, "10.0.0.0/30");
        assert!(
            network
                .advertised_relationships
                .iter()
                .any(|relationship| relationship.remote_identity == "192.0.2.1")
        );
    }

    #[test]
    fn invalid_lsa_records_are_not_positive_empty_population() {
        let input = r#"{"default":{"vrfName":"default","routerLinkStates":{"areas":{"0.0.0.0":[{"lsaAge":1}]}}}}"#;
        let parsed = parse_frr_ospf_lsdb(&context(FRR_OSPF_LSDB_COMMAND), input).unwrap();
        assert!(parsed.entries.is_empty());
        assert!(!parsed.positive_empty_population);
        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("linkStateId"))
        );
    }

    #[test]
    fn empty_shapes_distinguish_positive_empty_from_inactive_protocol_scope() {
        let ospf_empty = r#"{"default":{"vrfName":"default","neighbors":{}}}"#;
        let isis_inactive = r#"{"areas":[]}"#;
        let ospf =
            parse_frr_ospf_neighbors(&context(FRR_OSPF_NEIGHBOR_COMMAND), ospf_empty).unwrap();
        assert!(ospf.entries.is_empty());
        assert!(ospf.positive_empty_population);
        assert!(!ospf.scope_absent_or_inactive);

        let isis =
            parse_frr_isis_neighbors(&context(FRR_ISIS_NEIGHBOR_COMMAND), isis_inactive).unwrap();
        assert!(isis.entries.is_empty());
        assert!(!isis.positive_empty_population);
        assert!(isis.scope_absent_or_inactive);

        let ospf_db_empty = r#"{"default":{"vrfName":"default","routerId":"192.0.2.1"}}"#;
        let lsdb = parse_frr_ospf_lsdb(&context(FRR_OSPF_LSDB_COMMAND), ospf_db_empty).unwrap();
        assert!(lsdb.entries.is_empty());
        assert!(lsdb.positive_empty_population);
        assert!(!lsdb.scope_absent_or_inactive);
    }
}
