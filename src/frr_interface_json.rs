//! BN-024-05 — native structured FRR interface observation adapter.
//!
//! Consumes the genuine FRR `show interface json` object map and emits the
//! same typed `interface.state` facts as the CLI adapter. This is a
//! standalone-FRR implementation only: it is NOT evidence of SONiC or
//! NVIDIA Cumulus compatibility, and it does not bypass their acquisition
//! or release-qualification contracts.
//!
//! Format semantics were derived from the genuine FRR 10.6.1 response in
//! `tests/fixtures/bn024/interface-frr-json/` and FRR's interface model:
//! `administrativeStatus`/`operationalStatus` are explicit state tokens,
//! `index` is the kernel ifindex, `mtu` is bytes, `speed` is the interface
//! speed in megabits per second when present (`0` means no established
//! speed), `type` is FRR's interface type vocabulary, `hardwareAddress` is
//! the MAC when present, `vrfName` is the VRF binding, and `ipAddresses[]`
//! entries carry address/prefix strings. Pseudo-interface records may omit
//! most operational attributes; those omissions remain explicit unknowns.
//! `parentIfindex` is preserved as diagnostic evidence only — it is never
//! converted into a named parent or LAG relationship without the target
//! identity in the same observation.
//!
//! Format strictness policy (exact-release qualification): every field of
//! the observed 10.6.1_git schema is decoded as optional, so a record
//! missing optional attributes still normalizes with explicit unknowns —
//! additive absence is forward-compatible. Unknown *additional* fields are
//! instead rejected as a format error. This is deliberate, not incidental:
//! dispatch is qualified only for the exact demonstrated release/format
//! envelope, so a response carrying fields outside that envelope is
//! evidence the emitting build differs from the qualified one (FRR emits
//! a fixed field set per build; new fields arrive with new versions, and
//! new versions are not qualified). Rejecting is device-visible as a typed
//! `invalid_input` outcome rather than a silent wrong interpretation; a
//! future qualified release may extend the schema deliberately with its
//! own fixture evidence. Wrong types for known fields are likewise format
//! errors; they are never coerced.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::interface_facts::{
    FACT_KIND_INTERFACE_STATE, FieldAvailability, InterfaceStatePayload, MasterKind,
    MasterRelationship, ObservedField, SpeedObservation, SpeedUnit, valid_name,
};
use crate::interface_normalizer::{
    CaptureOutcome, CommandCapture, FieldCoverage, InterfaceDiagnostic, NormalizationOutput,
    NormalizationReport, NormalizerLimits, ObservationContext, SourceCaptureReport, addr_field,
    admin_state_of, classify_master, interface_type_of, mac_of, mtu_of, parse_rfc3339,
    sanitize_bounded, sha256_hex, unavailable,
};
use crate::typed_facts::{
    Completeness, EvidenceRef, FactPayload, FactSource, FactSubject, Lifecycle, PayloadState,
    Quality, TrustClass, TypedFact,
};

pub const FRR_JSON_PARSER_REVISION: &str = "frr-iface-json-2026.11-r1";
pub const FRR_JSON_MAPPING_REVISION: &str = "bn024-iface-map-2";
pub const FRR_JSON_ACQUISITION_CONTRACT: &str = "frr_vtysh_show_interface_json";

/// Structured single-source observation. Unlike the two-command CLI
/// observation this is one command response; no atomicity claim is made
/// beyond that response's own capture time.
pub struct FrrInterfaceJsonObservation<'a> {
    pub capture: CommandCapture<'a>,
    pub context: ObservationContext<'a>,
    /// Explicit logical acquisition contract used to obtain this capture.
    /// The typed input variant proves the shape; this value proves the
    /// producer intended the qualified `show interface json` contract.
    pub acquisition_contract: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // decoded to enforce the exact source schema; semantic use is explicit below
struct FrrIpAddress {
    address: Option<String>,
    secondary: Option<bool>,
    #[serde(rename = "noPrefixRoute")]
    no_prefix_route: Option<bool>,
    unnumbered: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // decoded to enforce the exact source schema; semantic use is explicit below
struct FrrInterfaceRecord {
    #[serde(rename = "administrativeStatus")]
    administrative_status: Option<String>,
    #[serde(rename = "operationalStatus")]
    operational_status: Option<String>,
    description: Option<String>,
    #[serde(rename = "vrfName")]
    vrf_name: Option<String>,
    index: Option<u64>,
    metric: Option<u64>,
    mtu: Option<u64>,
    speed: Option<u64>,
    txqlen: Option<u64>,
    flags: Option<String>,
    #[serde(rename = "type")]
    interface_kind: Option<String>,
    #[serde(rename = "hardwareAddress")]
    hardware_address: Option<String>,
    #[serde(rename = "ipAddresses")]
    ip_addresses: Option<Vec<FrrIpAddress>>,
    #[serde(rename = "interfaceType")]
    interface_type_detail: Option<String>,
    #[serde(rename = "interfaceSlaveType")]
    interface_slave_type: Option<String>,
    #[serde(rename = "parentIfindex")]
    parent_ifindex: Option<u64>,
    #[serde(rename = "pseudoInterface")]
    pseudo_interface: Option<bool>,
    // Present in the genuine FRR 10.6.1 response. They are decoded to
    // enforce the exact format contract but are not yet typed semantics.
    #[serde(rename = "linkDetection")]
    link_detection: Option<bool>,
    #[serde(rename = "linkUps")]
    link_ups: Option<u64>,
    #[serde(rename = "linkDowns")]
    link_downs: Option<u64>,
    #[serde(rename = "mplsEnabled")]
    mpls_enabled: Option<bool>,
    #[serde(rename = "linkDown")]
    link_down: Option<bool>,
    #[serde(rename = "linkDownV6")]
    link_down_v6: Option<bool>,
    #[serde(rename = "mcForwardingV4")]
    mc_forwarding_v4: Option<bool>,
    #[serde(rename = "mcForwardingV6")]
    mc_forwarding_v6: Option<bool>,
    #[serde(rename = "bgpRAConfigured")]
    bgp_ra_configured: Option<bool>,
    #[serde(rename = "multicastConfig")]
    multicast_config: Option<String>,
    #[serde(rename = "shutdownConfig")]
    shutdown_config: Option<String>,
    #[serde(rename = "mplsConfig")]
    mpls_config: Option<String>,
    #[serde(rename = "speedChecked")]
    speed_checked: Option<u64>,
    #[serde(rename = "lacpBypass")]
    lacp_bypass: Option<bool>,
    #[serde(rename = "evpnMh")]
    evpn_mh: Option<Value>,
    protodown: Option<String>,
    // Optional fields emitted by the FRR 10.6.1 zebra implementation for
    // particular interface kinds, states or build-time statistic backends.
    // They are decoded as schema evidence but are not interface.state typed
    // semantics until independently mapped and tested.
    #[serde(rename = "lastLinkUp")]
    last_link_up: Option<String>,
    #[serde(rename = "lastLinkDown")]
    last_link_down: Option<String>,
    #[serde(rename = "OsDescription")]
    os_description: Option<String>,
    mtu6: Option<u64>,
    bandwidth: Option<u64>,
    #[serde(rename = "bridgeVlanAware")]
    bridge_vlan_aware: Option<bool>,
    #[serde(rename = "vlanId")]
    vlan_id: Option<u64>,
    #[serde(rename = "vxlanId")]
    vxlan_id: Option<Value>,
    #[serde(rename = "vtepIp")]
    vtep_ip: Option<String>,
    #[serde(rename = "vtepRemoteIp")]
    vtep_remote_ip: Option<String>,
    #[serde(rename = "linkInterface")]
    link_interface: Option<String>,
    #[serde(rename = "masterInterface")]
    master_interface: Option<String>,
    #[serde(rename = "masterIfindex")]
    master_ifindex: Option<u64>,
    #[serde(rename = "parentInterface")]
    parent_interface: Option<String>,
    #[serde(rename = "protodownReason")]
    protodown_reason: Option<String>,
    #[serde(rename = "trafficEngineeringLinkParameters")]
    traffic_engineering_link_parameters: Option<Value>,
    #[serde(rename = "neighborIpAddresses")]
    neighbor_ip_addresses: Option<Value>,
    #[serde(rename = "inputPackets")]
    input_packets: Option<u64>,
    #[serde(rename = "inputBytes")]
    input_bytes: Option<u64>,
    #[serde(rename = "inputDropped")]
    input_dropped: Option<u64>,
    #[serde(rename = "inputDropd")]
    input_dropd: Option<u64>,
    #[serde(rename = "inputMulticastPackets")]
    input_multicast_packets: Option<u64>,
    #[serde(rename = "inputErrors")]
    input_errors: Option<u64>,
    #[serde(rename = "inputLengthErrors")]
    input_length_errors: Option<u64>,
    #[serde(rename = "inputOverrunErrors")]
    input_overrun_errors: Option<u64>,
    #[serde(rename = "inputCrcErrors")]
    input_crc_errors: Option<u64>,
    #[serde(rename = "inputFrameErrors")]
    input_frame_errors: Option<u64>,
    #[serde(rename = "inputFifoErrors")]
    input_fifo_errors: Option<u64>,
    #[serde(rename = "inputMissedErrors")]
    input_missed_errors: Option<u64>,
    #[serde(rename = "outputPackets")]
    output_packets: Option<u64>,
    #[serde(rename = "outputBytes")]
    output_bytes: Option<u64>,
    #[serde(rename = "outputDroppedPackets")]
    output_dropped_packets: Option<u64>,
    #[serde(rename = "outputMulticastPackets")]
    output_multicast_packets: Option<u64>,
    #[serde(rename = "outputErrors")]
    output_errors: Option<u64>,
    #[serde(rename = "outputAbortedErrors")]
    output_aborted_errors: Option<u64>,
    #[serde(rename = "outputCarrierErrors")]
    output_carrier_errors: Option<u64>,
    #[serde(rename = "outputFifoErrors")]
    output_fifo_errors: Option<u64>,
    #[serde(rename = "outputHeartbeatErrors")]
    output_heartbeat_errors: Option<u64>,
    #[serde(rename = "outputWindowErrors")]
    output_window_errors: Option<u64>,
    collisions: Option<u64>,
}

/// Top-level object map with duplicate interface-name detection. Serde's
/// ordinary `BTreeMap` deserialization silently keeps the last duplicate
/// key, which would destroy identity evidence; duplicates are therefore a
/// format error before any interface semantics run.
struct FrrInterfaceMap(BTreeMap<String, FrrInterfaceRecord>);

impl<'de> Deserialize<'de> for FrrInterfaceMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = FrrInterfaceMap;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a map of unique interface names to FRR interface records")
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut out = BTreeMap::new();
                while let Some(name) = access.next_key::<String>()? {
                    if out.contains_key(&name) {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate interface object key '{name}'"
                        )));
                    }
                    let record = access.next_value::<FrrInterfaceRecord>()?;
                    out.insert(name, record);
                }
                Ok(FrrInterfaceMap(out))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

fn context_error(context: &ObservationContext<'_>) -> Option<String> {
    for (name, value) in [
        ("device_id", Some(context.device_id)),
        ("collector_id", Some(context.collector_id)),
        ("capture_id", Some(context.capture_id)),
        ("observed_at", Some(context.observed_at)),
        ("scope", context.scope),
        ("network_namespace", context.network_namespace),
        ("release_raw", context.release_raw),
        ("release_train", context.release_train),
        ("release_build", context.release_build),
        ("platform_sku", context.platform_sku),
        ("asic", context.asic),
    ] {
        if let Some(v) = value
            && v.trim().is_empty()
        {
            return Some(format!("observation context '{name}' is empty"));
        }
    }
    if parse_rfc3339(context.observed_at).is_none() {
        return Some("observation context 'observed_at' is not RFC-3339".into());
    }
    None
}

fn status_fact(
    obs: &FrrInterfaceJsonObservation<'_>,
    state: PayloadState,
    note: &str,
    evidence: Vec<EvidenceRef>,
) -> TypedFact {
    let mut fact = TypedFact {
        schema_version: crate::typed_facts::SCHEMA_VERSION,
        fact_id: String::new(),
        fact_kind: FACT_KIND_INTERFACE_STATE.into(),
        subject: FactSubject {
            device_id: obs.context.device_id.into(),
            scope: obs.context.scope.map(str::to_string),
            namespace: "interface".into(),
            network_namespace: obs.context.network_namespace.map(str::to_string),
            entity: None,
            vrf: None,
            afi_safi: None,
            plane: Some("operational".into()),
        },
        source: FactSource {
            collector_id: obs.context.collector_id.into(),
            source_kind: "cli_ssh_structured".into(),
            capture_id: obs.context.capture_id.into(),
            session_epoch: obs.context.session_epoch,
            sequence: obs.context.sequence,
            exporter_id: None,
        },
        observed_at: obs.context.observed_at.into(),
        ingested_at: None,
        valid_from: None,
        valid_until: None,
        payload: FactPayload {
            produced_by: "native_frr_structured".into(),
            parser_revision: FRR_JSON_PARSER_REVISION.into(),
            mapping_revision: Some(FRR_JSON_MAPPING_REVISION.into()),
            fields: None,
            state: Some(state),
        },
        payload_sha256: String::new(),
        evidence_sha256: String::new(),
        quality: Quality {
            completeness: Completeness::Unknown,
            missing_fields: vec![],
            notes: vec![note.into()],
        },
        trust: TrustClass::DeviceAsserted,
        authority: None,
        lifecycle: Lifecycle::Active,
        withdrawn_reason: None,
        superseded_by: None,
        lifecycle_changed_at: None,
        evidence,
    };
    fact.seal();
    fact
}

fn invalid_output(
    obs: &FrrInterfaceJsonObservation<'_>,
    reason: &str,
    state: PayloadState,
    bytes: usize,
    limits: NormalizerLimits,
) -> NormalizationOutput {
    let oversized = bytes > limits.max_input_bytes;
    let (kind, sha) = if oversized {
        // Evidence truthfully describes the refusal. Do not claim a full
        // content digest for an oversized buffer this bounded implementation
        // deliberately never hashed.
        (
            "capture_refused_input_bound",
            sha256_hex(
                format!("bn024-input-bound-exceeded:frr_show_interface_json={bytes}").as_bytes(),
            ),
        )
    } else {
        (
            "raw_output_frr_show_interface_json",
            sha256_hex(obs.capture.output.as_bytes()),
        )
    };
    let evidence = vec![EvidenceRef {
        kind: kind.into(),
        sha256: sha,
        locator: None,
    }];
    let mut facts = vec![];
    let f = status_fact(obs, state, reason, evidence);
    if f.validate().is_ok() {
        facts.push(f);
    }
    let emitted = facts.len();
    NormalizationOutput {
        facts,
        interfaces: vec![],
        report: NormalizationReport {
            nos: "frr",
            applicability: "frr structured show-interface-json",
            interfaces_observed: 0,
            expected_interface_total: None,
            facts_emitted: emitted,
            duplicate_ifnames: 0,
            merge_conflicts: 0,
            unparsed_lines: 0,
            field_coverage: BTreeMap::new(),
            sources: vec![SourceCaptureReport {
                source: FRR_JSON_ACQUISITION_CONTRACT,
                outcome: obs.capture.outcome,
                parsed: false,
                rejected: oversized,
                bytes,
                captured_at: obs.capture.captured_at.map(str::to_string),
            }],
            entities_omitted: 0,
            addresses_omitted: 0,
            invalid_entities: 0,
            facts_rejected: usize::from(emitted == 0),
            output_facts_omitted: 0,
            conflicts_omitted: 0,
            context_error: None,
            capture_skew_seconds: None,
            notes: vec![reason.into()],
        },
    }
}

fn speed_mbps(raw: Option<u64>) -> ObservedField<SpeedObservation> {
    match raw {
        None => unavailable(FieldAvailability::Unknown, Some("speed not observed")),
        Some(0) => unavailable(FieldAvailability::Unknown, Some("device reported no speed")),
        Some(v) => ObservedField::known(SpeedObservation {
            value: v,
            unit: SpeedUnit::MegabitsPerSecond,
        }),
    }
}

fn ifindex_json(raw: Option<u64>) -> ObservedField<u64> {
    match raw {
        None => unavailable(FieldAvailability::Unknown, Some("ifindex not observed")),
        Some(0) => unavailable(
            FieldAvailability::Invalid,
            Some("ifindex 0 failed validation"),
        ),
        Some(v) => ObservedField::known(v),
    }
}

fn oper_json(raw: Option<&str>) -> ObservedField<crate::interface_facts::OperState> {
    match raw.and_then(crate::interface_normalizer::oper_token) {
        Some(v) => ObservedField::known(v),
        None if raw.is_some() => unavailable(
            FieldAvailability::Invalid,
            Some(format!("unrecognized operationalStatus '{}'", raw.unwrap())),
        ),
        None => unavailable(
            FieldAvailability::Unknown,
            Some("operationalStatus not observed"),
        ),
    }
}

/// Normalize the structured observation. Every emitted fact is canonical-
/// validated before return; malformed JSON, unexpected fields, failed
/// captures, oversized input and invalid observation context are explicit
/// non-success outcomes, never empty success.
pub fn normalize_frr_interface_json(
    obs: &FrrInterfaceJsonObservation<'_>,
    limits: NormalizerLimits,
) -> NormalizationOutput {
    let bytes = obs.capture.output.len();
    if let Some(err) = context_error(&obs.context) {
        let mut out = invalid_output(
            obs,
            "observation context invalid",
            PayloadState::InvalidInput,
            bytes,
            limits,
        );
        out.report.context_error = Some(err);
        out.facts.clear();
        out.report.facts_emitted = 0;
        out.report.facts_rejected = 0;
        return out;
    }
    if bytes > limits.max_input_bytes {
        return invalid_output(
            obs,
            "observation exceeded input byte bound; refused",
            PayloadState::InvalidInput,
            bytes,
            limits,
        );
    }
    if !obs.capture.outcome.usable() {
        return invalid_output(
            obs,
            "structured capture failed/unavailable/timed out; no usable observation",
            PayloadState::CaptureFailed,
            bytes,
            limits,
        );
    }
    let decoded: BTreeMap<String, FrrInterfaceRecord> =
        match serde_json::from_str::<FrrInterfaceMap>(obs.capture.output) {
            Ok(v) => v.0,
            Err(e) => {
                return invalid_output(
                    obs,
                    &format!("invalid or unexpected FRR interface JSON: {e}"),
                    PayloadState::InvalidInput,
                    bytes,
                    limits,
                );
            }
        };

    let sha = sha256_hex(obs.capture.output.as_bytes());
    let evidence = || {
        vec![EvidenceRef {
            kind: "raw_output_frr_show_interface_json".into(),
            sha256: sha.clone(),
            locator: None,
        }]
    };
    let mut facts = Vec::new();
    let mut diagnostics = Vec::new();
    let mut coverage: BTreeMap<String, FieldCoverage> = BTreeMap::new();
    let mut entities_omitted = 0usize;
    let mut invalid_entities = 0usize;
    let mut output_facts_omitted = 0usize;
    let mut facts_rejected = 0usize;
    let mut rejected_samples = Vec::new();
    let mut addresses_omitted_total = 0usize;
    // parentIfindex is numeric evidence. It may be resolved only when the
    // target interface is present in this same observation; duplicate or
    // absent indices remain unresolved rather than becoming guessed names.
    let mut name_by_index: BTreeMap<u64, String> = BTreeMap::new();
    let mut type_by_name: BTreeMap<String, String> = BTreeMap::new();
    for (candidate_name, candidate) in &decoded {
        if let Some(index) = candidate.index {
            match name_by_index.insert(index, candidate_name.clone()) {
                None => {}
                Some(existing) if existing == *candidate_name => {}
                Some(_) => {
                    // Contradictory duplicate kernel indices are not safe
                    // relationship evidence for either interface.
                    name_by_index.remove(&index);
                }
            }
        }
        if let Some(kind) = &candidate.interface_kind {
            type_by_name.insert(candidate_name.clone(), kind.clone());
        }
    }
    for (entity_index, (name, rec)) in decoded.iter().enumerate() {
        if entity_index >= limits.max_entities {
            entities_omitted += 1;
            continue;
        }
        if facts.len() >= limits.max_output_facts {
            output_facts_omitted += 1;
            continue;
        }
        if !valid_name(name) {
            invalid_entities += 1;
            continue;
        }
        let admin = admin_state_of(rec.administrative_status.as_deref());
        let oper = oper_json(rec.operational_status.as_deref());
        let ifindex = ifindex_json(rec.index);
        let itype = interface_type_of(rec.interface_kind.as_deref(), name);
        let mtu_s = rec.mtu.map(|v| v.to_string());
        let mtu = mtu_of(mtu_s.as_deref());
        let speed = speed_mbps(rec.speed);
        let mac = mac_of(rec.hardware_address.as_deref());
        let description = rec.description.as_deref().map(|d| sanitize_bounded(d, 240));
        let mut v4 = vec![];
        let mut v6 = vec![];
        let mut malformed_addrs = 0usize;
        let mut address_entries_omitted = 0usize;
        if let Some(addrs) = &rec.ip_addresses {
            for (idx, a) in addrs.iter().enumerate() {
                if idx >= 64 {
                    address_entries_omitted += 1;
                    continue;
                }
                if let Some(s) = &a.address {
                    if s.contains(':') {
                        v6.push(s.clone())
                    } else {
                        v4.push(s.clone())
                    }
                } else {
                    malformed_addrs += 1;
                }
            }
        }
        addresses_omitted_total += address_entries_omitted;
        let address_list = |values: &[String], want_v6: bool, empty_detail: &str| {
            if malformed_addrs > 0 {
                unavailable(
                    FieldAvailability::Invalid,
                    Some("ipAddresses entry missing address"),
                )
            } else if rec.ip_addresses.is_some() && values.is_empty() {
                // `ipAddresses` was present and empty for this family:
                // observed-none is Known([]), distinct from the field
                // being absent entirely.
                ObservedField::known(Vec::new())
            } else {
                addr_field(values, want_v6, address_entries_omitted, empty_detail)
            }
        };
        let ip4 = address_list(&v4, false, "no ipv4 observed");
        let ip6 = address_list(&v6, true, "no ipv6 observed");
        let slave_kind = match rec.interface_slave_type.as_deref() {
            Some("Bond") => MasterKind::Lag,
            Some("Bridge") => MasterKind::Bridge,
            _ => MasterKind::Unresolved,
        };
        let explicit_numeric_contradiction = |explicit: Option<&String>, index: Option<u64>| match (
            explicit,
            index.and_then(|i| name_by_index.get(&i)),
        ) {
            (Some(name), Some(indexed_name)) => indexed_name != name,
            _ => false,
        };
        let parent_ref_contradicts =
            explicit_numeric_contradiction(rec.parent_interface.as_ref(), rec.parent_ifindex);
        let master_ref_contradicts =
            explicit_numeric_contradiction(rec.master_interface.as_ref(), rec.master_ifindex);
        let self_or = |target: &str, kind: MasterKind| {
            if target == name.as_str() {
                MasterKind::Unresolved
            } else {
                kind
            }
        };
        let (master_relationship, relationship_target_is_name) = if parent_ref_contradicts {
            // Explicit and numeric parent evidence disagree. Preserve the
            // explicit target as unresolved evidence but do not populate a
            // parent edge or infer a type from either side.
            (
                rec.parent_interface
                    .as_ref()
                    .map(|target| MasterRelationship {
                        target: target.clone(),
                        kind: MasterKind::Unresolved,
                    }),
                true,
            )
        } else if master_ref_contradicts {
            (
                rec.master_interface
                    .as_ref()
                    .map(|target| MasterRelationship {
                        target: target.clone(),
                        kind: MasterKind::Unresolved,
                    }),
                true,
            )
        } else if let Some(target) = rec.parent_interface.as_ref() {
            // `parentInterface` is FRR's explicit lower-link parent name,
            // not an inference from a numeric index.
            (
                Some(MasterRelationship {
                    target: target.clone(),
                    kind: self_or(target, MasterKind::Parent),
                }),
                true,
            )
        } else if let Some(target) = rec.master_interface.as_ref() {
            // `masterInterface` is emitted only for bridge/bond slave
            // records. Prefer the explicit slave kind; use the target's
            // own type as corroboration when present.
            let classified = if slave_kind != MasterKind::Unresolved {
                slave_kind
            } else {
                classify_master(target, &type_by_name)
            };
            (
                Some(MasterRelationship {
                    target: target.clone(),
                    kind: self_or(target, classified),
                }),
                true,
            )
        } else if let Some(master_index) = rec.master_ifindex {
            let resolved = name_by_index.get(&master_index);
            let target = resolved
                .cloned()
                .unwrap_or_else(|| format!("ifindex:{master_index}"));
            let classified = if slave_kind != MasterKind::Unresolved {
                slave_kind
            } else if resolved.is_some() {
                classify_master(&target, &type_by_name)
            } else {
                MasterKind::Unresolved
            };
            let kind = if resolved.is_some() {
                self_or(&target, classified)
            } else {
                classified
            };
            (
                Some(MasterRelationship { target, kind }),
                resolved.is_some(),
            )
        } else if let Some(parent_index) = rec.parent_ifindex {
            let resolved = name_by_index.get(&parent_index);
            let target = resolved
                .cloned()
                .unwrap_or_else(|| format!("ifindex:{parent_index}"));
            let classified = if resolved.is_some() {
                classify_master(&target, &type_by_name)
            } else {
                MasterKind::Unresolved
            };
            let kind = if resolved.is_some() {
                self_or(&target, classified)
            } else {
                classified
            };
            (
                Some(MasterRelationship { target, kind }),
                resolved.is_some(),
            )
        } else {
            (None, false)
        };
        let (parent_interface, member_of) = match master_relationship.as_ref() {
            Some(rel) if rel.kind == MasterKind::Parent && relationship_target_is_name => {
                (Some(rel.target.clone()), None)
            }
            Some(rel) if rel.kind == MasterKind::Lag && relationship_target_is_name => {
                (None, Some(rel.target.clone()))
            }
            _ => (None, None),
        };
        let payload = InterfaceStatePayload {
            interface_name: name.clone(),
            normalized_name: None,
            administrative_state: admin.clone(),
            operational_state: oper.clone(),
            ifindex: ifindex.clone(),
            interface_type: itype.clone(),
            mtu: mtu.clone(),
            speed: speed.clone(),
            mac_address: mac.clone(),
            description,
            parent_interface,
            member_of,
            master_relationship: master_relationship.clone(),
            source_conflicts: vec![],
            ip_addresses: Some(ip4.clone()),
            ipv6_addresses: Some(ip6.clone()),
        };
        let observed_fields: [(&str, FieldAvailability); 7] = [
            ("administrative_state", admin.state),
            ("operational_state", oper.state),
            ("ifindex", ifindex.state),
            ("interface_type", itype.state),
            ("mtu", mtu.state),
            ("speed", speed.state),
            ("mac_address", mac.state),
        ];
        let mut missing_fields = vec![];
        for (fname, st) in observed_fields {
            let e = coverage.entry(fname.to_string()).or_default();
            match st {
                FieldAvailability::Known => e.known += 1,
                FieldAvailability::Unknown => e.unknown += 1,
                FieldAvailability::Unsupported => e.unsupported += 1,
                FieldAvailability::Invalid => e.invalid += 1,
            }
            if st != FieldAvailability::Known && missing_fields.len() < 32 {
                missing_fields.push(fname.to_string());
            }
        }
        for (fname, field) in [("ip_addresses", &ip4), ("ipv6_addresses", &ip6)] {
            coverage.entry(fname.to_string()).or_default().record(field);
            if field.state != FieldAvailability::Known && missing_fields.len() < 32 {
                missing_fields.push(fname.to_string());
            }
        }
        let mut qnotes = vec![];
        if rec.pseudo_interface == Some(true) {
            qnotes.push("pseudo-interface record has partial operational detail".into());
        }
        if parent_ref_contradicts || master_ref_contradicts {
            qnotes.push(
                "explicit and numeric parent/master references contradict; relationship unresolved"
                    .into(),
            );
        }
        if let Some(rel) = &master_relationship {
            if rel.kind == MasterKind::Unresolved {
                qnotes.push(
                    "parentIfindex target/type not established in this observation; relationship unresolved"
                        .into(),
                );
            } else {
                qnotes.push(format!(
                    "parentIfindex resolved within the same observation as {:?}",
                    rel.kind
                ));
            }
        }
        if rec
            .interface_slave_type
            .as_deref()
            .is_some_and(|v| v != "None")
        {
            qnotes.push(format!(
                "interfaceSlaveType '{}' decoded as evidence; relationship classification uses the resolved target type",
                rec.interface_slave_type.as_deref().unwrap_or_default()
            ));
        }
        if rec
            .interface_type_detail
            .as_deref()
            .is_some_and(|v| v != "None")
        {
            qnotes.push(format!(
                "interfaceType detail '{}' decoded as kernel detail; canonical type uses the FRR type field",
                rec.interface_type_detail.as_deref().unwrap_or_default()
            ));
        }
        let fields = match serde_json::to_value(&payload) {
            Ok(v) => v,
            Err(e) => {
                facts_rejected += 1;
                if rejected_samples.len() < 8 {
                    rejected_samples.push(format!("{name}: payload serialization failed: {e}"));
                }
                continue;
            }
        };
        let completeness = if obs.capture.outcome == CaptureOutcome::Complete
            && missing_fields.is_empty()
            && malformed_addrs == 0
        {
            Completeness::Complete
        } else if obs.capture.outcome == CaptureOutcome::Unknown {
            Completeness::Unknown
        } else {
            Completeness::Partial
        };
        let mut fact = TypedFact {
            schema_version: crate::typed_facts::SCHEMA_VERSION,
            fact_id: String::new(),
            fact_kind: FACT_KIND_INTERFACE_STATE.into(),
            subject: FactSubject {
                device_id: obs.context.device_id.into(),
                scope: obs.context.scope.map(str::to_string),
                namespace: "interface".into(),
                network_namespace: obs.context.network_namespace.map(str::to_string),
                entity: Some(name.clone()),
                vrf: rec.vrf_name.clone(),
                afi_safi: None,
                plane: Some("operational".into()),
            },
            source: FactSource {
                collector_id: obs.context.collector_id.into(),
                source_kind: "cli_ssh_structured".into(),
                capture_id: obs.context.capture_id.into(),
                session_epoch: obs.context.session_epoch,
                sequence: obs.context.sequence,
                exporter_id: None,
            },
            observed_at: obs.context.observed_at.into(),
            ingested_at: None,
            valid_from: None,
            valid_until: None,
            payload: FactPayload {
                produced_by: "native_frr_structured".into(),
                parser_revision: FRR_JSON_PARSER_REVISION.into(),
                mapping_revision: Some(FRR_JSON_MAPPING_REVISION.into()),
                fields: Some(fields),
                state: None,
            },
            payload_sha256: String::new(),
            evidence_sha256: String::new(),
            quality: Quality {
                completeness,
                missing_fields,
                notes: qnotes,
            },
            trust: TrustClass::DeviceAsserted,
            authority: None,
            lifecycle: Lifecycle::Active,
            withdrawn_reason: None,
            superseded_by: None,
            lifecycle_changed_at: None,
            evidence: evidence(),
        };
        fact.seal();
        let diagnostic = InterfaceDiagnostic {
            ifname: name.clone(),
            ifname_raw: None,
            vrf: rec.vrf_name.clone(),
            ifindex: rec.index,
            admin_state_raw: rec.administrative_status.clone(),
            oper_state_raw: rec.operational_status.clone(),
            type_raw: rec.interface_kind.clone(),
            mtu_raw: rec.mtu.map(|v| v.to_string()),
            speed_raw: rec.speed.map(|v| format!("{v} Mbps")),
            mac_raw: rec.hardware_address.clone(),
            master_raw: master_relationship
                .as_ref()
                .map(|rel| format!("{} ({:?})", rel.target, rel.kind)),
            vlan_raw: None,
            ip_addresses: v4,
            ipv6_addresses: v6,
            addresses_omitted: address_entries_omitted,
            conflict_fields: vec![],
        };
        diagnostics.push(diagnostic);
        if let Err(e) = fact.validate() {
            facts_rejected += 1;
            if rejected_samples.len() < 8 {
                rejected_samples.push(format!("{name}: {e:?}"));
            }
            continue;
        }
        facts.push(fact);
    }

    if facts.is_empty() {
        let f = status_fact(
            obs,
            PayloadState::Empty,
            "no interface records in structured observation",
            evidence(),
        );
        if f.validate().is_ok() {
            facts.push(f);
        } else {
            facts_rejected += 1;
        }
    }
    let mut notes = vec![
        "standalone FRR structured evidence; not SONiC or Cumulus qualification evidence".into(),
        "speed interpreted as Mbps according to the FRR interface model".into(),
        "parentIfindex is diagnostic evidence only; no relationship is inferred without a named target".into(),
    ];
    if entities_omitted > 0 {
        notes.push(format!(
            "{entities_omitted} entities omitted at resource limit"
        ));
    }
    if output_facts_omitted > 0 {
        notes.push(format!(
            "{output_facts_omitted} facts omitted at effective output-fact budget"
        ));
    }
    if facts_rejected > 0 {
        notes.push(format!(
            "{facts_rejected} facts rejected by canonical validation"
        ));
        notes.extend(
            rejected_samples
                .into_iter()
                .map(|s| format!("rejected: {s}")),
        );
    }
    let interfaces_observed = diagnostics.len();
    let facts_emitted = facts.len();
    NormalizationOutput {
        facts,
        interfaces: diagnostics,
        report: NormalizationReport {
            nos: "frr",
            applicability: "frr structured show-interface-json",
            interfaces_observed,
            expected_interface_total: None,
            facts_emitted,
            duplicate_ifnames: 0,
            merge_conflicts: 0,
            unparsed_lines: 0,
            field_coverage: coverage,
            sources: vec![SourceCaptureReport {
                source: FRR_JSON_ACQUISITION_CONTRACT,
                outcome: obs.capture.outcome,
                parsed: true,
                rejected: false,
                bytes,
                captured_at: obs.capture.captured_at.map(str::to_string),
            }],
            entities_omitted,
            addresses_omitted: addresses_omitted_total,
            invalid_entities,
            facts_rejected,
            output_facts_omitted,
            conflicts_omitted: 0,
            context_error: None,
            capture_skew_seconds: None,
            notes,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_facts::{AdminState, InterfaceType, OperState};

    const GENUINE: &str = include_str!(
        "../tests/fixtures/bn024/interface-frr-json/show-interface-json-frr-10.6.1.json"
    );

    fn obs<'a>(json: &'a str) -> FrrInterfaceJsonObservation<'a> {
        let mut context = ObservationContext::minimal(
            "frr2",
            "collector-test",
            "capture-frr-json-1",
            "2026-11-25T13:50:00Z",
        );
        context.session_epoch = 7;
        context.sequence = 42;
        context.network_namespace = Some("default");
        context.platform = Some("frr");
        context.release_raw = Some("10.6.1_git");
        FrrInterfaceJsonObservation {
            capture: CommandCapture::new(json, CaptureOutcome::Complete)
                .captured_at("2026-11-25T13:50:00Z"),
            context,
            acquisition_contract: FRR_JSON_ACQUISITION_CONTRACT,
        }
    }

    fn payload(fact: &TypedFact) -> InterfaceStatePayload {
        serde_json::from_value(
            fact.payload
                .fields
                .clone()
                .expect("interface payload fields"),
        )
        .expect("payload must decode as interface.state")
    }

    #[test]
    fn genuine_frr_1061_fixture_has_independent_expected_values() {
        let out = normalize_frr_interface_json(&obs(GENUINE), NormalizerLimits::default());
        assert_eq!(out.report.interfaces_observed, 4);
        assert_eq!(out.report.facts_rejected, 0);
        assert_eq!(out.facts.len(), 4);
        assert!(out.facts.iter().all(|f| f.validate().is_ok()));
        assert!(out.facts.iter().all(|f| f.authority.is_none()));

        let by_name: BTreeMap<_, _> = out
            .facts
            .iter()
            .map(|f| {
                (
                    f.subject.entity.clone().expect("interface entity"),
                    payload(f),
                )
            })
            .collect();
        let eth0 = &by_name["eth0"];
        assert_eq!(eth0.administrative_state.value, Some(AdminState::Up));
        assert_eq!(eth0.operational_state.value, Some(OperState::Up));
        assert_eq!(eth0.ifindex.value, Some(2));
        assert_eq!(eth0.interface_type.value, Some(InterfaceType::Physical));
        assert_eq!(eth0.mtu.value, Some(1500));
        assert_eq!(
            eth0.speed.value,
            Some(SpeedObservation {
                value: 10000,
                unit: SpeedUnit::MegabitsPerSecond
            })
        );
        assert_eq!(eth0.mac_address.value.as_deref(), Some("6e:5b:f7:50:04:e3"));
        assert_eq!(
            eth0.ip_addresses.as_ref().unwrap().value.as_deref(),
            Some([("172.100.208.12/24").to_string()].as_slice())
        );
        assert_eq!(
            eth0.ipv6_addresses.as_ref().unwrap().value.as_deref(),
            Some(Vec::<String>::new().as_slice())
        );
        let eth0_master = eth0
            .master_relationship
            .as_ref()
            .expect("parentIfindex evidence must be preserved");
        assert_eq!(eth0_master.target, "ifindex:21");
        assert_eq!(eth0_master.kind, MasterKind::Unresolved);
        assert_eq!(eth0.member_of, None);
        assert_eq!(eth0.parent_interface, None);
        assert_eq!(out.report.expected_interface_total, None);

        let lo = &by_name["lo"];
        assert_eq!(lo.interface_type.value, Some(InterfaceType::Loopback));
        assert_eq!(lo.mtu.value, Some(65536));
        assert_eq!(lo.speed.state, FieldAvailability::Unknown);
        assert_eq!(
            lo.ip_addresses.as_ref().unwrap().value.as_deref(),
            Some(Vec::<String>::new().as_slice()),
            "explicit empty ipAddresses is observed-none, not unknown"
        );

        let eth1 = &by_name["eth1"];
        assert_eq!(eth1.administrative_state.value, Some(AdminState::Down));
        assert_eq!(eth1.operational_state.state, FieldAvailability::Unknown);
        assert_eq!(eth1.interface_type.state, FieldAvailability::Unknown);
        assert_eq!(eth1.description.as_deref(), Some("to-frr1-global"));
        assert!(
            out.report
                .field_coverage
                .get("operational_state")
                .is_some_and(|c| c.unknown >= 2)
        );
    }

    #[test]
    fn structured_identity_preserves_namespace_release_and_source_axes() {
        let out = normalize_frr_interface_json(&obs(GENUINE), NormalizerLimits::default());
        let f = out
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("eth0"))
            .unwrap();
        assert_eq!(f.subject.device_id, "frr2");
        assert_eq!(f.subject.network_namespace.as_deref(), Some("default"));
        assert_eq!(f.subject.vrf.as_deref(), Some("default"));
        assert_eq!(f.subject.plane.as_deref(), Some("operational"));
        assert_eq!(f.source.collector_id, "collector-test");
        assert_eq!(f.source.capture_id, "capture-frr-json-1");
        assert_eq!(f.source.session_epoch, 7);
        assert_eq!(f.source.sequence, 42);
        assert_eq!(f.source.source_kind, "cli_ssh_structured");
        assert_eq!(f.payload.parser_revision, FRR_JSON_PARSER_REVISION);
        assert_eq!(
            f.payload.mapping_revision.as_deref(),
            Some(FRR_JSON_MAPPING_REVISION)
        );
    }

    #[test]
    fn invalid_json_unexpected_fields_oversize_and_failed_capture_are_explicit() {
        let malformed = normalize_frr_interface_json(&obs("{"), NormalizerLimits::default());
        assert_eq!(
            malformed.facts[0].payload.state,
            Some(PayloadState::InvalidInput)
        );
        assert!(malformed.facts[0].validate().is_ok());

        let mut value: serde_json::Value = serde_json::from_str(GENUINE).unwrap();
        value["eth0"]["unknownField"] = serde_json::Value::Bool(true);
        let unexpected = serde_json::to_string(&value).unwrap();
        let out = normalize_frr_interface_json(&obs(&unexpected), NormalizerLimits::default());
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));

        let tiny = NormalizerLimits {
            max_input_bytes: 8,
            ..Default::default()
        };
        let out = normalize_frr_interface_json(&obs(GENUINE), tiny);
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
        assert!(out.report.sources[0].rejected);

        let mut failed = obs(GENUINE);
        failed.capture.outcome = CaptureOutcome::Failed;
        let out = normalize_frr_interface_json(&failed, NormalizerLimits::default());
        assert_eq!(
            out.facts[0].payload.state,
            Some(PayloadState::CaptureFailed)
        );
        assert!(out.facts.iter().all(|f| f.authority.is_none()));
    }

    #[test]
    fn invalid_address_and_missing_required_values_do_not_fabricate_success() {
        let mut value: serde_json::Value = serde_json::from_str(GENUINE).unwrap();
        value["eth0"]["ipAddresses"][0]["address"] =
            serde_json::Value::String("999.1.1.1/99".into());
        value["lo"]["mtu"] = serde_json::Value::Null;
        let json = serde_json::to_string(&value).unwrap();
        let out = normalize_frr_interface_json(&obs(&json), NormalizerLimits::default());
        let eth0_fact = out
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("eth0"))
            .unwrap();
        let eth0 = payload(eth0_fact);
        assert_eq!(
            eth0.ip_addresses.as_ref().unwrap().state,
            FieldAvailability::Invalid
        );
        assert!(
            eth0_fact
                .quality
                .missing_fields
                .contains(&"ip_addresses".to_string())
        );
        assert!(out.facts.iter().all(|f| f.validate().is_ok()));
    }

    #[test]
    fn same_observation_parent_ifindex_resolution_establishes_lag_membership() {
        let json = r#"{
          "bond0": {
            "administrativeStatus": "up",
            "operationalStatus": "up",
            "vrfName": "default",
            "index": 10,
            "mtu": 1500,
            "speed": 20000,
            "type": "Bond"
          },
          "eth9": {
            "administrativeStatus": "up",
            "operationalStatus": "up",
            "vrfName": "default",
            "index": 11,
            "mtu": 1500,
            "speed": 10000,
            "type": "Ethernet",
            "interfaceSlaveType": "Bond",
            "parentIfindex": 10
          }
        }"#;
        let out = normalize_frr_interface_json(&obs(json), NormalizerLimits::default());
        assert_eq!(out.report.facts_rejected, 0);
        let eth9 = out
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("eth9"))
            .map(payload)
            .expect("eth9 payload");
        assert_eq!(eth9.member_of.as_deref(), Some("bond0"));
        assert_eq!(eth9.parent_interface, None);
        let rel = eth9.master_relationship.expect("master relationship");
        assert_eq!(rel.target, "bond0");
        assert_eq!(rel.kind, MasterKind::Lag);
        let bond0 = out
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("bond0"))
            .map(payload)
            .expect("bond0 payload");
        assert_eq!(bond0.interface_type.value, Some(InterfaceType::PortChannel));
    }

    #[test]
    fn entity_output_and_address_budgets_are_explicitly_enforced() {
        let mut value: serde_json::Value = serde_json::from_str(GENUINE).unwrap();
        value["eth0"]["ipAddresses"] = (0..70)
            .map(|i| {
                serde_json::json!({
                    "address": format!("10.0.0.{i}/32"),
                    "secondary": false,
                    "noPrefixRoute": false,
                    "unnumbered": false
                })
            })
            .collect();
        let json = serde_json::to_string(&value).unwrap();
        let limits = NormalizerLimits {
            max_entities: 2,
            max_output_facts: 1,
            ..Default::default()
        };
        let out = normalize_frr_interface_json(&obs(&json), limits);
        assert_eq!(out.report.entities_omitted, 2);
        assert_eq!(out.report.output_facts_omitted, 1);
        assert_eq!(out.report.addresses_omitted, 6);
        assert_eq!(out.report.expected_interface_total, None);
        assert!(out.report.notes.iter().any(|n| n.contains("omitted")));
        assert!(out.facts.iter().all(|f| f.validate().is_ok()));
    }

    #[test]
    fn duplicate_interface_names_in_different_network_namespaces_remain_distinct() {
        let first = normalize_frr_interface_json(&obs(GENUINE), NormalizerLimits::default());
        let mut second_obs = obs(GENUINE);
        second_obs.context.network_namespace = Some("asic-0");
        second_obs.context.capture_id = "capture-frr-json-2";
        let second = normalize_frr_interface_json(&second_obs, NormalizerLimits::default());
        let f1 = first
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("eth0"))
            .unwrap();
        let f2 = second
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("eth0"))
            .unwrap();
        assert_eq!(f1.subject.network_namespace.as_deref(), Some("default"));
        assert_eq!(f2.subject.network_namespace.as_deref(), Some("asic-0"));
        assert_ne!(f1.fact_id, f2.fact_id);
        assert!(f1.validate().is_ok() && f2.validate().is_ok());
    }

    #[test]
    fn duplicate_object_keys_and_wrong_known_field_types_are_format_errors() {
        // A duplicated identity-bearing object key must be detected before
        // map decoding could silently drop one of the two records.
        let duplicated = r#"{
          "eth0": {"administrativeStatus": "up", "operationalStatus": "up", "index": 2},
          "eth0": {"administrativeStatus": "down", "operationalStatus": "down", "index": 3}
        }"#;
        let out = normalize_frr_interface_json(&obs(duplicated), NormalizerLimits::default());
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
        assert!(out.report.notes.iter().any(|n| n.contains("duplicate")));
        assert!(out.facts[0].validate().is_ok());

        // A wrong type for a known field is a format error, not an
        // silently-unknown attribute.
        let mut value: serde_json::Value = serde_json::from_str(GENUINE).unwrap();
        value["eth0"]["mtu"] = serde_json::Value::String("1500".into());
        let json = serde_json::to_string(&value).unwrap();
        let out = normalize_frr_interface_json(&obs(&json), NormalizerLimits::default());
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
    }

    #[test]
    fn missing_optional_fields_are_explicit_unknowns_not_failures() {
        // Forward-compatible absence: optional attributes missing from the
        // record yield explicit unknown fields while the interface fact is
        // still emitted for the exact qualified format.
        let mut value: serde_json::Value = serde_json::from_str(GENUINE).unwrap();
        let obj = value["eth0"].as_object_mut().unwrap();
        obj.remove("speed");
        obj.remove("mtu");
        obj.remove("hardwareAddress");
        let json = serde_json::to_string(&value).unwrap();
        let out = normalize_frr_interface_json(&obs(&json), NormalizerLimits::default());
        let eth0 = out
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("eth0"))
            .map(payload)
            .expect("eth0 fact must still be emitted");
        assert_eq!(eth0.mtu.state, FieldAvailability::Unknown);
        assert_eq!(eth0.speed.state, FieldAvailability::Unknown);
        assert_eq!(eth0.mac_address.state, FieldAvailability::Unknown);
        assert!(out.facts.iter().all(|f| f.validate().is_ok()));
    }

    #[test]
    fn contradictory_explicit_and_numeric_references_never_resolve_to_edges() {
        // `parentInterface` names bond0 explicitly while `parentIfindex`
        // resolves to a different interface. The explicit target survives
        // only as unresolved evidence; no parent/LAG edge may be inferred.
        let json = r#"{
          "bond0": {
            "administrativeStatus": "up", "operationalStatus": "up",
            "index": 10, "mtu": 1500, "type": "Bond"
          },
          "eth8": {
            "administrativeStatus": "up", "operationalStatus": "up",
            "index": 12, "mtu": 1500, "type": "Ethernet"
          },
          "eth9": {
            "administrativeStatus": "up", "operationalStatus": "up",
            "index": 11, "mtu": 1500, "type": "Ethernet",
            "parentInterface": "bond0", "parentIfindex": 12
          }
        }"#;
        let out = normalize_frr_interface_json(&obs(json), NormalizerLimits::default());
        let eth9 = out
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("eth9"))
            .map(payload)
            .expect("eth9 payload");
        let rel = eth9
            .master_relationship
            .expect("unresolved evidence retained");
        assert_eq!(rel.target, "bond0");
        assert_eq!(rel.kind, MasterKind::Unresolved);
        assert_eq!(eth9.member_of, None);
        assert_eq!(eth9.parent_interface, None);

        // A numeric reference whose target is absent from this observation
        // stays an unresolved ifindex reference — never a fabricated name.
        let json2 = r#"{
          "eth9": {
            "administrativeStatus": "up", "operationalStatus": "up",
            "index": 11, "mtu": 1500, "type": "Ethernet",
            "interfaceSlaveType": "Bond", "masterIfindex": 99
          }
        }"#;
        let out2 = normalize_frr_interface_json(&obs(json2), NormalizerLimits::default());
        let eth9b = payload(&out2.facts[0]);
        let rel2 = eth9b
            .master_relationship
            .expect("unresolved numeric evidence");
        assert_eq!(rel2.target, "ifindex:99");
        assert_eq!(eth9b.member_of, None);
    }

    #[test]
    fn synthetic_scale_workloads_report_scaling_and_budget_refusal() {
        // Clearly-labelled synthetic scale data derived from the qualified
        // format — NOT device evidence. Measures extraction+normalization
        // latency scaling beyond the 4-interface genuine baseline.
        let synth = |n: usize| {
            let entries: Vec<String> = (0..n)
                .map(|i| {
                    format!(
                        r#""eth{i}": {{"administrativeStatus":"up","operationalStatus":"up","vrfName":"default","index":{},"mtu":1500,"speed":10000,"type":"Ethernet","hardwareAddress":"02:00:00:00:{:02x}:{:02x}","ipAddresses":[{{"address":"10.240.{}.{}/31","secondary":false,"noPrefixRoute":false,"unnumbered":false}}]}}"#,
                        i + 2,
                        (i >> 8) & 0xff,
                        i & 0xff,
                        (i >> 8) & 0xff,
                        (i & 0xff) * 2 % 256,
                    )
                })
                .collect();
            format!("{{{}}}", entries.join(","))
        };
        for n in [128usize, 1024] {
            let json = synth(n);
            let input_bytes = json.len();
            let started = std::time::Instant::now();
            let out = normalize_frr_interface_json(&obs(&json), NormalizerLimits::default());
            let elapsed = started.elapsed();
            assert_eq!(out.facts.len(), n);
            assert!(out.facts.iter().all(|f| f.validate().is_ok()));
            println!(
                "frr_json_scale interfaces={} input_bytes={} facts={} total_ms={:.3} us_per_iface={:.2}",
                n,
                input_bytes,
                out.report.facts_emitted,
                elapsed.as_secs_f64() * 1000.0,
                elapsed.as_secs_f64() * 1_000_000.0 / n as f64,
            );
        }
        // Explicit refusal boundary: input beyond the byte budget is a
        // typed refusal, never a partial silent success.
        let json = synth(1024);
        let tiny = NormalizerLimits {
            max_input_bytes: 1024,
            ..Default::default()
        };
        let out = normalize_frr_interface_json(&obs(&json), tiny);
        assert_eq!(out.facts[0].payload.state, Some(PayloadState::InvalidInput));
        // Entity budget truncation at scale stays explicit.
        let capped = NormalizerLimits {
            max_entities: 100,
            ..Default::default()
        };
        let out = normalize_frr_interface_json(&obs(&json), capped);
        assert_eq!(out.report.entities_omitted, 924);
        assert!(out.report.notes.iter().any(|n| n.contains("omitted")));
    }

    #[test]
    fn operator_presentation_fixture_matches_implementation_contract() {
        // Drift prevention: the synthetic presentation fixture must agree
        // with the actual implementation constants and remain labelled
        // synthetic — it never becomes device evidence by being consumed.
        let raw = include_str!(
            "../tests/fixtures/bn024/interface-frr-json/operator-presentation-cases.json"
        );
        let value: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert!(
            value["classification"]
                .as_str()
                .unwrap()
                .contains("synthetic")
        );
        assert_eq!(
            value["source_feature"].as_str().unwrap(),
            "interfaces.frr_json"
        );
        let cases = value["cases"].as_array().unwrap();
        assert!(
            cases.len() >= 6,
            "success/experimental/unsupported/partial/conflict/failed coverage"
        );
        let success = cases
            .iter()
            .find(|c| c["case"].as_str().unwrap() == "qualified_successful_observation")
            .expect("qualified success case");
        assert_eq!(
            success["implementation"]["parser_revision"]
                .as_str()
                .unwrap(),
            FRR_JSON_PARSER_REVISION
        );
        assert_eq!(
            success["implementation"]["mapping_revision"]
                .as_str()
                .unwrap(),
            FRR_JSON_MAPPING_REVISION
        );
        assert_eq!(
            success["acquisition"]["contract"].as_str().unwrap(),
            FRR_JSON_ACQUISITION_CONTRACT
        );
        // Coverage honesty: the success case reports the genuine observed
        // entity count with an explicitly unknown denominator — never a
        // fabricated total derived from parsed rows.
        let cov = &success["entity_coverage"];
        assert_eq!(cov["observed"].as_u64().unwrap(), 4);
        assert!(cov["expected_total"].is_null());
        assert_eq!(cov["denominator"].as_str().unwrap(), "unknown");
        // No case may claim authoritative projection or baseline evaluation.
        // Cases that never reach normalization correctly present no
        // projection at all ("none"/absent) rather than a fake shadow result.
        for case in cases {
            if let Some(p) = case.get("projection").and_then(|p| p.as_str()) {
                assert!(
                    p == "shadow_only" || p == "none",
                    "projection must never be authoritative: {p}"
                );
            }
            if let Some(b) = case.get("baseline").and_then(|b| b.as_str()) {
                assert_eq!(b, "not_evaluated");
            }
        }
    }

    #[test]
    fn structured_fixture_performance_smoke_reports_bounded_native_latency() {
        let iterations = 100usize;
        let started = std::time::Instant::now();
        let mut facts = 0usize;
        for _ in 0..iterations {
            let out = normalize_frr_interface_json(&obs(GENUINE), NormalizerLimits::default());
            facts = std::hint::black_box(out.facts.len());
        }
        let elapsed = started.elapsed();
        assert_eq!(facts, 4);
        println!(
            "frr_json_perf iterations={} total_ms={:.3} avg_us={:.1} observations_per_sec={:.1}",
            iterations,
            elapsed.as_secs_f64() * 1000.0,
            elapsed.as_secs_f64() * 1_000_000.0 / iterations as f64,
            iterations as f64 / elapsed.as_secs_f64().max(f64::EPSILON)
        );
    }
}
