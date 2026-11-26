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
//! `parentIfindex` is lower-link parent evidence only. It is resolved into
//! a typed parent relationship only when the target identity is present and
//! uniquely identifiable in the same observation; it is never treated as
//! LAG membership. Numeric or explicit references that are missing,
//! duplicate, contradictory or self-referential remain explicitly
//! unresolved rather than becoming topology edges.
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
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

use crate::interface_facts::{
    FACT_KIND_INTERFACE_STATE, FieldAvailability, InterfaceStatePayload, MasterKind,
    MasterRelationship, ObservedField, SpeedObservation, SpeedUnit, valid_name,
};
use crate::interface_normalizer::{
    CaptureOutcome, CommandCapture, FieldCoverage, InterfaceDiagnostic, NormalizationOutput,
    NormalizationReport, NormalizerLimits, ObservationContext, SourceCaptureReport, addr_field,
    admin_state_of, interface_type_of, mac_of, parse_rfc3339, sanitize_bounded, sha256_hex,
    unavailable,
};
use crate::typed_facts::{
    Completeness, EvidenceRef, FactPayload, FactSource, FactSubject, Lifecycle, PayloadState,
    Quality, TrustClass, TypedFact,
};

pub const FRR_JSON_PARSER_REVISION: &str = "frr-iface-json-2026.11-r2";
pub const FRR_JSON_MAPPING_REVISION: &str = "bn024-iface-map-3";
pub const FRR_JSON_REQUIREMENT_REVISION: &str = "bn024-iface-requirements-r1";
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

const MAX_ADDRESSES_PER_INTERFACE: usize = 64;

/// Bounded decoded representation of one interface address array. Entries
/// past the stable retained boundary are consumed as ignored JSON values
/// and represented only by `total_entries`; interpretation must therefore
/// never fabricate family-wide absence.
#[derive(Debug)]
struct FrrAddressList {
    entries: Vec<FrrIpAddress>,
    total_entries: usize,
}

impl<'de> Deserialize<'de> for FrrAddressList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct AddressVisitor;

        impl<'de> Visitor<'de> for AddressVisitor {
            type Value = FrrAddressList;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a bounded array of FRR IP address objects")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut entries = Vec::with_capacity(MAX_ADDRESSES_PER_INTERFACE.min(64));
                let mut total_entries = 0usize;
                while total_entries < MAX_ADDRESSES_PER_INTERFACE {
                    let Some(entry) = seq.next_element::<FrrIpAddress>()? else {
                        return Ok(FrrAddressList {
                            entries,
                            total_entries,
                        });
                    };
                    total_entries += 1;
                    entries.push(entry);
                }
                while seq.next_element::<IgnoredAny>()?.is_some() {
                    total_entries += 1;
                    if total_entries > 100_000 {
                        return Err(serde::de::Error::custom(
                            "unbounded FRR interface address array",
                        ));
                    }
                }
                Ok(FrrAddressList {
                    entries,
                    total_entries,
                })
            }
        }

        deserializer.deserialize_seq(AddressVisitor)
    }
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
    ip_addresses: Option<FrrAddressList>,
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
        struct InterfaceMapVisitor;
        impl<'de> Visitor<'de> for InterfaceMapVisitor {
            type Value = FrrInterfaceMap;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a map of unique interface names to FRR interface records")
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
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
        deserializer.deserialize_map(InterfaceMapVisitor)
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

const MAX_REPORT_NOTES: usize = 24;
const MAX_REPORT_NOTE_BYTES: usize = 4096;
const MAX_FACT_QUALITY_NOTES: usize = 12;
const MAX_FACT_QUALITY_NOTE_BYTES: usize = 1024;

fn bounded_note(text: impl AsRef<str>, max_chars: usize) -> String {
    sanitize_bounded(text.as_ref(), max_chars)
}

fn append_bounded_note(notes: &mut Vec<String>, text: impl AsRef<str>) {
    if notes.len() >= MAX_REPORT_NOTES {
        return;
    }
    notes.push(bounded_note(text, 240));
}

fn finish_bounded_notes(notes: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = notes.len() > MAX_REPORT_NOTES;
    for note in notes.into_iter().take(MAX_REPORT_NOTES) {
        let note = bounded_note(note, 240);
        let next = bytes.saturating_add(note.len());
        if next > MAX_REPORT_NOTE_BYTES {
            truncated = true;
            break;
        }
        bytes = next;
        out.push(note);
    }
    if truncated && out.len() < MAX_REPORT_NOTES {
        let marker = "diagnostic output truncated at bounded budget";
        if bytes + marker.len() <= MAX_REPORT_NOTE_BYTES {
            out.push(marker.into());
        }
    }
    out
}

fn finish_fact_quality_notes(notes: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = notes.len() > MAX_FACT_QUALITY_NOTES;
    for note in notes.into_iter().take(MAX_FACT_QUALITY_NOTES) {
        let note = bounded_note(note, 160);
        let next = bytes.saturating_add(note.len());
        if next > MAX_FACT_QUALITY_NOTE_BYTES {
            truncated = true;
            break;
        }
        bytes = next;
        out.push(note);
    }
    if truncated && out.len() < MAX_FACT_QUALITY_NOTES {
        let marker = "fact quality notes truncated";
        if bytes + marker.len() <= MAX_FACT_QUALITY_NOTE_BYTES {
            out.push(marker.into());
        }
    }
    out
}

#[derive(Clone, Debug)]
enum IndexResolution {
    Unique(String),
    Ambiguous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReferenceStatus {
    Resolved,
    Missing,
    Ambiguous,
    Invalid,
    Contradictory,
    SelfReference,
}

impl ReferenceStatus {
    fn is_resolved(self) -> bool {
        self == Self::Resolved
    }
}

#[derive(Clone, Debug)]
struct ReferenceResolution {
    target: String,
    status: ReferenceStatus,
}

fn sanitize_reference(raw: Option<&String>) -> Option<Option<String>> {
    raw.map(|value| {
        let value = sanitize_bounded(value, 255);
        valid_name(&value).then_some(value)
    })
}

fn resolve_reference(
    explicit: Option<&String>,
    ifindex: Option<u64>,
    name_by_index: &BTreeMap<u64, IndexResolution>,
    eligible_names: &BTreeSet<String>,
    self_name: &str,
) -> Option<ReferenceResolution> {
    let explicit_sanitized = sanitize_reference(explicit);
    let explicit_target = explicit_sanitized.as_ref().and_then(|v| v.clone());
    let explicit_invalid = explicit_sanitized.as_ref().is_some_and(Option::is_none);
    let by_index = ifindex.map(|index| {
        if index == 0 {
            (None, ReferenceStatus::Invalid)
        } else {
            match name_by_index.get(&index) {
                Some(IndexResolution::Unique(name)) => {
                    (Some(name.clone()), ReferenceStatus::Resolved)
                }
                Some(IndexResolution::Ambiguous) => {
                    (Some(format!("ifindex:{index}")), ReferenceStatus::Ambiguous)
                }
                None => (Some(format!("ifindex:{index}")), ReferenceStatus::Missing),
            }
        }
    });

    let (target, status) = match (explicit.is_some(), by_index) {
        (true, Some((index_target, index_status))) => {
            if explicit_invalid {
                ("invalid-reference".to_string(), ReferenceStatus::Invalid)
            } else if index_status == ReferenceStatus::Resolved
                && explicit_target.as_deref() == index_target.as_deref()
            {
                (explicit_target.clone().unwrap(), ReferenceStatus::Resolved)
            } else {
                (
                    explicit_target
                        .clone()
                        .or(index_target)
                        .unwrap_or_else(|| "invalid-reference".to_string()),
                    ReferenceStatus::Contradictory,
                )
            }
        }
        (true, None) => {
            if explicit_invalid {
                ("invalid-reference".to_string(), ReferenceStatus::Invalid)
            } else {
                let target = explicit_target.clone().unwrap();
                if eligible_names.contains(&target) {
                    (target, ReferenceStatus::Resolved)
                } else {
                    (target, ReferenceStatus::Missing)
                }
            }
        }
        (false, Some((index_target, index_status))) => (
            index_target.unwrap_or_else(|| "invalid-reference".to_string()),
            index_status,
        ),
        (false, None) => return None,
    };

    let status = if status == ReferenceStatus::Resolved && target == self_name {
        ReferenceStatus::SelfReference
    } else {
        status
    };
    Some(ReferenceResolution { target, status })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetTypeEvidence {
    Absent,
    Unrecognized,
    Kind(MasterKind),
}

fn target_type_evidence(
    target: &str,
    zif_type_by_name: &BTreeMap<String, String>,
) -> TargetTypeEvidence {
    let Some(raw) = zif_type_by_name.get(target) else {
        return TargetTypeEvidence::Absent;
    };
    match raw.to_ascii_lowercase().as_str() {
        "bond" => TargetTypeEvidence::Kind(MasterKind::Lag),
        "bridge" => TargetTypeEvidence::Kind(MasterKind::Bridge),
        "vrf" => TargetTypeEvidence::Kind(MasterKind::Vrf),
        _ => TargetTypeEvidence::Unrecognized,
    }
}

fn classify_master_relationship(
    rec: &FrrInterfaceRecord,
    resolution: &ReferenceResolution,
    zif_type_by_name: &BTreeMap<String, String>,
) -> MasterKind {
    if !resolution.status.is_resolved() {
        return MasterKind::Unresolved;
    }
    let slave_kind = rec
        .interface_slave_type
        .as_deref()
        .map(|v| v.to_ascii_lowercase());
    let explicit_kind = match slave_kind.as_deref() {
        Some("bond") => Some(MasterKind::Lag),
        Some("bridge") => Some(MasterKind::Bridge),
        // A VRF enslavement flag contradicts the strict bridge/bond-slave
        // semantics of masterInterface/masterIfindex. VRF binding remains
        // represented on the subject and must never become a master edge.
        Some("vrf") | Some("other") | Some("none") => None,
        Some(_) => None,
        None => None,
    };
    let evidence = target_type_evidence(&resolution.target, zif_type_by_name);
    match (explicit_kind, evidence) {
        (Some(kind), TargetTypeEvidence::Absent) => kind,
        (Some(kind), TargetTypeEvidence::Kind(seen)) if kind == seen => kind,
        (Some(_), TargetTypeEvidence::Kind(_)) => MasterKind::Unresolved,
        (Some(_), TargetTypeEvidence::Unrecognized) => MasterKind::Unresolved,
        (None, TargetTypeEvidence::Kind(MasterKind::Lag)) => MasterKind::Lag,
        (None, TargetTypeEvidence::Kind(MasterKind::Bridge)) => MasterKind::Bridge,
        // VRF target evidence is not a master-relationship class.
        // VRF/parent/unrecognized target evidence is never a valid master
        // relationship class for FRR's strict bridge/bond-slave field.
        (None, TargetTypeEvidence::Kind(_)) => MasterKind::Unresolved,
        (None, TargetTypeEvidence::Absent | TargetTypeEvidence::Unrecognized) => {
            MasterKind::Unresolved
        }
    }
}

fn record_field_state(
    coverage: &mut BTreeMap<String, FieldCoverage>,
    name: &str,
    state: FieldAvailability,
) {
    let entry = coverage.entry(name.into()).or_default();
    match state {
        FieldAvailability::Known => entry.known += 1,
        FieldAvailability::Unknown => entry.unknown += 1,
        FieldAvailability::Unsupported => entry.unsupported += 1,
        FieldAvailability::Invalid => entry.invalid += 1,
    }
}

fn status_fact(
    obs: &FrrInterfaceJsonObservation<'_>,
    state: PayloadState,
    completeness: Completeness,
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
            interface_identity: None,
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
            completeness,
            missing_fields: vec![],
            notes: finish_fact_quality_notes(vec![note.into()]),
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
    // Pre-parse refusals/capture failures have no established coverage;
    // never claim partial success for bytes that were not interpreted.
    let f = status_fact(obs, state, Completeness::Unknown, reason, evidence);
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
            records_decoded: None,
            entities_encountered: None,
            entities_accepted: Some(0),
            status_facts_emitted: Some(emitted),
            validated_field_coverage: Some(BTreeMap::new()),
            context_error: None,
            capture_skew_seconds: None,
            notes: finish_bounded_notes(vec![reason.into()]),
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

/// MTU is a 32-bit quantity. In FRR JSON a present zero is invalid source
/// data, unlike an absent optional field which is unknown.
fn mtu_json(raw: Option<u64>) -> ObservedField<u32> {
    match raw {
        None => unavailable(FieldAvailability::Unknown, Some("mtu not observed")),
        Some(0) => unavailable(FieldAvailability::Invalid, Some("mtu 0 failed validation")),
        Some(v) if v <= u32::MAX as u64 => ObservedField::known(v as u32),
        Some(v) => unavailable(
            FieldAvailability::Invalid,
            Some(format!("mtu {v} out of range")),
        ),
    }
}

/// Normalize the structured observation. Every emitted fact is canonical-
/// validated before return; malformed JSON, unexpected fields, failed
/// captures, malformed capture metadata, oversized input and invalid
/// observation context are explicit non-success outcomes, never empty
/// success.
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
        out.report.status_facts_emitted = Some(0);
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
    if let Some(captured_at) = obs.capture.captured_at {
        if parse_rfc3339(captured_at).is_none() {
            return invalid_output(
                obs,
                "capture metadata 'captured_at' is not RFC-3339; observation rejected",
                PayloadState::InvalidInput,
                bytes,
                limits,
            );
        }
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
    let records_decoded = decoded.len();

    let sha = sha256_hex(obs.capture.output.as_bytes());
    let evidence = || {
        vec![EvidenceRef {
            kind: "raw_output_frr_show_interface_json".into(),
            sha256: sha.clone(),
            locator: None,
        }]
    };

    // Relationship targets may only be resolved against validated entities
    // inside the entity-resource budget. Invalid or budget-omitted records
    // are visible through counters but are never relationship authorities.
    let mut eligible_names = BTreeSet::new();
    for (entity_index, (name, _rec)) in decoded.iter().enumerate() {
        if entity_index < limits.max_entities && valid_name(name) {
            eligible_names.insert(name.clone());
        }
    }

    let mut name_by_index: BTreeMap<u64, IndexResolution> = BTreeMap::new();
    let mut zif_type_by_name: BTreeMap<String, String> = BTreeMap::new();
    for (entity_index, (name, rec)) in decoded.iter().enumerate() {
        if entity_index >= limits.max_entities || !valid_name(name) {
            continue;
        }
        if let Some(index) = rec.index.filter(|index| *index > 0) {
            match name_by_index.get_mut(&index) {
                None => {
                    name_by_index.insert(index, IndexResolution::Unique(name.clone()));
                }
                // Duplicate kernel ifindices are contradictory evidence.
                // Three or more claimants leave the entry in Ambiguous; the
                // first contradiction never becomes a "winner" and a later
                // claimant never removes the ambiguity.
                Some(slot) => *slot = IndexResolution::Ambiguous,
            }
        }
        if let Some(detail) = rec.interface_type_detail.as_ref() {
            zif_type_by_name.insert(name.clone(), detail.clone());
        }
    }

    let mut facts = Vec::new();
    let mut diagnostics = Vec::new();
    let mut coverage: BTreeMap<String, FieldCoverage> = BTreeMap::new();
    let mut validated_coverage: BTreeMap<String, FieldCoverage> = BTreeMap::new();
    let mut entities_omitted = 0usize;
    let mut invalid_entities = 0usize;
    let mut output_facts_omitted = 0usize;
    let mut facts_rejected = 0usize;
    let mut rejected_samples = Vec::new();
    let mut addresses_omitted_total = 0usize;
    let mut entities_accepted = 0usize;

    for (entity_index, (name, rec)) in decoded.iter().enumerate() {
        if entity_index >= limits.max_entities {
            entities_omitted += 1;
            continue;
        }
        if !valid_name(name) {
            invalid_entities += 1;
            continue;
        }
        if facts.len() >= limits.max_output_facts {
            output_facts_omitted += 1;
            continue;
        }

        let admin = admin_state_of(rec.administrative_status.as_deref());
        let oper = oper_json(rec.operational_status.as_deref());
        let ifindex = ifindex_json(rec.index);
        let itype = interface_type_of(rec.interface_kind.as_deref(), name);
        let mtu = mtu_json(rec.mtu);
        let speed = speed_mbps(rec.speed);
        let mac = mac_of(rec.hardware_address.as_deref());
        let description = rec.description.as_deref().map(|d| sanitize_bounded(d, 240));

        let mut v4 = vec![];
        let mut v6 = vec![];
        let mut malformed_addrs = 0usize;
        let mut address_entries_omitted = 0usize;
        if let Some(addrs) = &rec.ip_addresses {
            address_entries_omitted = addrs.total_entries.saturating_sub(addrs.entries.len());
            for a in &addrs.entries {
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
            } else if address_entries_omitted > 0 && values.is_empty() {
                // Entries beyond the retained boundary may have belonged to
                // this family. An empty retained family is therefore unknown,
                // never fabricated as observed-none.
                unavailable(
                    FieldAvailability::Unknown,
                    Some("address entries omitted at bounded budget; family coverage incomplete"),
                )
            } else if rec.ip_addresses.is_some() && values.is_empty() {
                // `ipAddresses` was present and complete for this family:
                // observed-none is Known([]), distinct from field absence.
                ObservedField::known(Vec::new())
            } else {
                addr_field(values, want_v6, address_entries_omitted, empty_detail)
            }
        };
        let ip4 = address_list(&v4, false, "no ipv4 observed");
        let ip6 = address_list(&v6, true, "no ipv6 observed");

        let parent_resolution = resolve_reference(
            rec.parent_interface.as_ref(),
            rec.parent_ifindex,
            &name_by_index,
            &eligible_names,
            name,
        );
        let master_resolution = resolve_reference(
            rec.master_interface.as_ref(),
            rec.master_ifindex,
            &name_by_index,
            &eligible_names,
            name,
        );
        let parent_relationship = parent_resolution
            .as_ref()
            .map(|resolution| MasterRelationship {
                target: resolution.target.clone(),
                kind: if resolution.status.is_resolved() {
                    MasterKind::Parent
                } else {
                    MasterKind::Unresolved
                },
            });
        let master_relationship = master_resolution
            .as_ref()
            .map(|resolution| MasterRelationship {
                target: resolution.target.clone(),
                kind: classify_master_relationship(rec, resolution, &zif_type_by_name),
            });
        let parent_interface = parent_relationship
            .as_ref()
            .and_then(|rel| (rel.kind == MasterKind::Parent).then_some(rel.target.clone()));
        let member_of = master_relationship
            .as_ref()
            .and_then(|rel| (rel.kind == MasterKind::Lag).then_some(rel.target.clone()));

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
            parent_relationship: parent_relationship.clone(),
            master_relationship: master_relationship.clone(),
            source_conflicts: vec![],
            ip_addresses: Some(ip4.clone()),
            ipv6_addresses: Some(ip6.clone()),
        };

        let mut local_coverage: BTreeMap<String, FieldCoverage> = BTreeMap::new();
        let observed_fields: [(&str, FieldAvailability); 9] = [
            ("administrative_state", admin.state),
            ("operational_state", oper.state),
            ("ifindex", ifindex.state),
            ("interface_type", itype.state),
            ("mtu", mtu.state),
            ("speed", speed.state),
            ("mac_address", mac.state),
            ("ip_addresses", ip4.state),
            ("ipv6_addresses", ip6.state),
        ];
        for (fname, state) in observed_fields {
            record_field_state(&mut coverage, fname, state);
            record_field_state(&mut local_coverage, fname, state);
        }

        let pseudo = rec.pseudo_interface == Some(true);
        let mut required_missing = vec![];
        if !pseudo {
            for (fname, state) in [
                ("administrative_state", admin.state),
                ("operational_state", oper.state),
                ("ifindex", ifindex.state),
                ("interface_type", itype.state),
                ("mtu", mtu.state),
            ] {
                if state != FieldAvailability::Known {
                    required_missing.push(fname.to_string());
                }
            }
        }
        let mut missing_fields = required_missing.clone();
        if malformed_addrs > 0 && missing_fields.len() < 32 {
            missing_fields.push("ip_addresses".into());
            missing_fields.push("ipv6_addresses".into());
        } else {
            if ip4.state == FieldAvailability::Invalid && missing_fields.len() < 32 {
                missing_fields.push("ip_addresses".into());
            }
            if ip6.state == FieldAvailability::Invalid && missing_fields.len() < 32 {
                missing_fields.push("ipv6_addresses".into());
            }
        }
        missing_fields.truncate(32);

        let mut qnotes = vec![];
        if pseudo {
            qnotes.push("pseudo-interface record has partial operational detail".into());
        }
        for (label, resolution) in [
            ("parent", parent_resolution.as_ref()),
            ("master", master_resolution.as_ref()),
        ] {
            let Some(resolution) = resolution else {
                continue;
            };
            match resolution.status {
                ReferenceStatus::Resolved => {}
                ReferenceStatus::Missing => qnotes.push(format!(
                    "{label} reference target '{}' is not an eligible interface in this observation; relationship unresolved",
                    resolution.target
                )),
                ReferenceStatus::Ambiguous => qnotes.push(format!(
                    "{label} reference uses duplicate kernel ifindex evidence; relationship unresolved"
                )),
                ReferenceStatus::Invalid => qnotes.push(format!(
                    "{label} reference is invalid; relationship unresolved"
                )),
                ReferenceStatus::Contradictory => qnotes.push(format!(
                    "explicit and numeric {label} references contradict; relationship unresolved"
                )),
                ReferenceStatus::SelfReference => qnotes.push(format!(
                    "{label} reference points to the interface itself; relationship unresolved"
                )),
            }
        }
        if let Some(rel) = &master_relationship {
            if rel.kind == MasterKind::Unresolved
                && master_resolution
                    .as_ref()
                    .is_some_and(|r| r.status.is_resolved())
            {
                qnotes.push(
                    "master target type or slave-type evidence is insufficient/contradictory; relationship unresolved"
                        .into(),
                );
            }
        }
        if rec
            .interface_slave_type
            .as_deref()
            .is_some_and(|v| v.eq_ignore_ascii_case("vrf"))
        {
            qnotes.push(
                "interfaceSlaveType 'Vrf' is represented by the subject VRF binding, not a master edge"
                    .into(),
            );
        }
        if let Some(detail) = rec.interface_type_detail.as_deref()
            && detail != "None"
        {
            qnotes.push(format!(
                "interfaceType detail '{}' decoded as zebra interface detail; canonical type uses the FRR type field",
                sanitize_bounded(detail, 80)
            ));
        }

        let relationship_unresolved = parent_relationship
            .iter()
            .chain(master_relationship.iter())
            .any(|rel| rel.kind == MasterKind::Unresolved);
        let semantic_gap = !required_missing.is_empty()
            || malformed_addrs > 0
            || address_entries_omitted > 0
            || relationship_unresolved;
        let completeness = if obs.capture.outcome == CaptureOutcome::Unknown {
            Completeness::Unknown
        } else if obs.capture.outcome == CaptureOutcome::Complete && !semantic_gap {
            Completeness::Complete
        } else {
            Completeness::Partial
        };

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
                interface_identity: None,
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
                notes: finish_fact_quality_notes(qnotes),
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
                .map(|rel| format!("master {} ({:?})", rel.target, rel.kind)),
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
        for (fname, counts) in local_coverage {
            let aggregate = validated_coverage.entry(fname).or_default();
            aggregate.known += counts.known;
            aggregate.unknown += counts.unknown;
            aggregate.unsupported += counts.unsupported;
            aggregate.invalid += counts.invalid;
        }
        entities_accepted += 1;
        facts.push(fact);
    }

    let mut status_facts_emitted = 0usize;
    if facts.is_empty() {
        let (state, note) = if decoded.is_empty() {
            (
                PayloadState::Empty,
                "structured observation decoded successfully and contained no interface records"
                    .to_string(),
            )
        } else if output_facts_omitted > 0
            && invalid_entities == 0
            && entities_omitted == 0
            && facts_rejected == 0
        {
            (
                PayloadState::OutputBudgetExhausted,
                format!(
                    "structured observation decoded {records_decoded} interface records, but the effective output-fact budget exhausted before any fact was emitted ({output_facts_omitted} omitted)"
                ),
            )
        } else {
            (
                PayloadState::NoUsableRecords,
                format!(
                    "structured observation decoded {records_decoded} interface records but produced no usable validated entity fact (invalid entities={invalid_entities}, entities omitted={entities_omitted}, validation rejections={facts_rejected}, output-budget omissions={output_facts_omitted})"
                ),
            )
        };
        let status_completeness =
            if state == PayloadState::Empty && obs.capture.outcome == CaptureOutcome::Complete {
                Completeness::Complete
            } else if state == PayloadState::OutputBudgetExhausted {
                Completeness::Partial
            } else {
                Completeness::Unknown
            };
        let f = status_fact(obs, state, status_completeness, &note, evidence());
        if f.validate().is_ok() {
            status_facts_emitted = 1;
            facts.push(f);
        } else {
            facts_rejected += 1;
        }
    }

    let mut notes = Vec::new();
    append_bounded_note(
        &mut notes,
        "standalone FRR structured evidence; not SONiC or Cumulus qualification evidence",
    );
    append_bounded_note(
        &mut notes,
        "speed interpreted as Mbps according to the FRR interface model",
    );
    append_bounded_note(
        &mut notes,
        "FRR parent references are lower-link evidence; master references are bridge/bond slave evidence only",
    );
    append_bounded_note(
        &mut notes,
        format!(
            "requirements revision {} exempts pseudo interfaces from absent operational core fields",
            FRR_JSON_REQUIREMENT_REVISION
        ),
    );
    if entities_omitted > 0 {
        append_bounded_note(
            &mut notes,
            format!("{entities_omitted} entities omitted at resource limit"),
        );
    }
    if output_facts_omitted > 0 {
        append_bounded_note(
            &mut notes,
            format!("{output_facts_omitted} facts omitted at effective output-fact budget"),
        );
    }
    if facts_rejected > 0 {
        append_bounded_note(
            &mut notes,
            format!("{facts_rejected} facts rejected by canonical validation"),
        );
        for sample in rejected_samples.into_iter().take(8) {
            append_bounded_note(&mut notes, format!("rejected: {sample}"));
        }
    }
    if addresses_omitted_total > 0 {
        append_bounded_note(
            &mut notes,
            format!("{addresses_omitted_total} address entries omitted at bounded budget"),
        );
    }

    let facts_emitted = facts.len();
    NormalizationOutput {
        facts,
        interfaces: diagnostics,
        report: NormalizationReport {
            nos: "frr",
            applicability: "frr structured show-interface-json",
            interfaces_observed: entities_accepted,
            records_decoded: Some(records_decoded),
            entities_encountered: Some(records_decoded),
            entities_accepted: Some(entities_accepted),
            status_facts_emitted: Some(status_facts_emitted),
            validated_field_coverage: Some(validated_coverage),
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
            notes: finish_bounded_notes(notes),
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
        let eth0_parent = eth0
            .parent_relationship
            .as_ref()
            .expect("parentIfindex evidence must be preserved");
        assert_eq!(eth0_parent.target, "ifindex:21");
        assert_eq!(eth0_parent.kind, MasterKind::Unresolved);
        assert_eq!(eth0.master_relationship, None);
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
    fn same_observation_master_ifindex_resolution_establishes_lag_membership() {
        let json = r#"{
          "bond0": {
            "administrativeStatus": "up",
            "operationalStatus": "up",
            "vrfName": "default",
            "index": 10,
            "mtu": 1500,
            "speed": 20000,
            "type": "Bond",
            "interfaceType": "bond"
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
            "masterIfindex": 10
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
            .parent_relationship
            .expect("unresolved parent evidence retained");
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
        // Coverage honesty: a complete decoded object map has a real
        // population denominator; partial/failed acquisitions remain unknown.
        let cov = &success["entity_coverage"];
        assert_eq!(cov["observed"].as_u64().unwrap(), 4);
        assert_eq!(cov["expected_total"].as_u64().unwrap(), 4);
        assert_eq!(
            cov["denominator"].as_str().unwrap(),
            "complete decoded object map"
        );
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
    fn bn02404_repro_all_rejected_population_is_not_empty_table() {
        let json = r#"{
          "bad\nname": {"administrativeStatus":"up","operationalStatus":"up","index":2}
        }"#;
        let out = normalize_frr_interface_json(&obs(json), NormalizerLimits::default());
        assert_ne!(
            out.facts[0].payload.state,
            Some(PayloadState::Empty),
            "a nonempty population rejected for invalid identity must not masquerade as an empty table"
        );
        assert_eq!(out.report.invalid_entities, 1);
    }

    #[test]
    fn bn02404_repro_three_duplicate_indices_never_resolve() {
        let json = r#"{
          "bond0": {"administrativeStatus":"up","operationalStatus":"up","index":10,"type":"Bond"},
          "eth1": {"administrativeStatus":"up","operationalStatus":"up","index":7,"type":"Ethernet"},
          "eth2": {"administrativeStatus":"up","operationalStatus":"up","index":7,"type":"Ethernet"},
          "eth3": {"administrativeStatus":"up","operationalStatus":"up","index":7,"type":"Ethernet"},
          "eth4": {"administrativeStatus":"up","operationalStatus":"up","index":8,"type":"Ethernet","parentIfindex":7}
        }"#;
        // Three duplicate claimants must never collapse to the last one.
        let out = normalize_frr_interface_json(&obs(json), NormalizerLimits::default());
        let eth4 = out
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("eth4"))
            .map(payload)
            .expect("eth4 payload");
        let rel = eth4
            .parent_relationship
            .expect("parent relationship evidence");
        assert_eq!(rel.kind, MasterKind::Unresolved);
        assert_eq!(eth4.parent_interface, None);
    }

    #[test]
    fn bn02404_repro_truncated_family_is_not_observed_none() {
        let addrs: Vec<_> = (0..64)
            .map(|i| serde_json::json!({"address": format!("10.0.0.{i}/32")}))
            .chain(std::iter::once(
                serde_json::json!({"address": "2001:db8::1/64"}),
            ))
            .collect();
        let json = serde_json::json!({
            "eth0": {
                "administrativeStatus":"up",
                "operationalStatus":"up",
                "index":2,
                "type":"Ethernet",
                "ipAddresses": addrs
            }
        })
        .to_string();
        let out = normalize_frr_interface_json(&obs(&json), NormalizerLimits::default());
        let eth0 = payload(&out.facts[0]);
        let v6 = eth0.ipv6_addresses.as_ref().expect("ipv6 field");
        assert_ne!(
            v6.state,
            FieldAvailability::Known,
            "an IPv6 entry beyond the retained boundary must not fabricate an empty IPv6 family"
        );
        assert_eq!(out.report.addresses_omitted, 1);
    }

    #[test]
    fn bn02404_repro_malformed_capture_timestamp_is_rejected() {
        let mut capture = obs("{}");
        capture.capture =
            CommandCapture::new("{}", CaptureOutcome::Complete).captured_at("not-a-timestamp");
        let out = normalize_frr_interface_json(&capture, NormalizerLimits::default());
        assert_eq!(
            out.facts[0].payload.state,
            Some(PayloadState::InvalidInput),
            "invalid capture metadata must fail closed"
        );
        assert!(out.facts[0].validate().is_ok());
    }

    #[test]
    fn bn02404_repro_bond_parent_is_lower_layer_not_lag_membership() {
        let json = r#"{
          "bond0": {"administrativeStatus":"up","operationalStatus":"up","index":10,"type":"Bond"},
          "bond0.100": {"administrativeStatus":"up","operationalStatus":"up","index":11,"type":"Vlan","parentIfindex":10}
        }"#;
        let out = normalize_frr_interface_json(&obs(json), NormalizerLimits::default());
        let vlan = out
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("bond0.100"))
            .map(payload)
            .expect("vlan payload");
        assert_eq!(vlan.member_of, None);
        assert_eq!(vlan.parent_interface.as_deref(), Some("bond0"));
        assert_eq!(
            vlan.parent_relationship.map(|r| r.kind),
            Some(MasterKind::Parent)
        );
        assert_eq!(vlan.master_relationship, None);
    }

    #[test]
    fn simultaneous_parent_and_master_relationships_remain_distinct() {
        let json = r#"{
          "bond0": {"administrativeStatus":"up","operationalStatus":"up","index":10,"type":"Bond"},
          "br0": {"administrativeStatus":"up","operationalStatus":"up","index":20,"type":"Bridge","interfaceType":"bridge"},
          "eth1": {
            "administrativeStatus":"up","operationalStatus":"up","index":11,"type":"Ethernet",
            "interfaceSlaveType":"Bridge","masterIfindex":20,"parentIfindex":10
          }
        }"#;
        let out = normalize_frr_interface_json(&obs(json), NormalizerLimits::default());
        let eth1 = out
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("eth1"))
            .map(payload)
            .expect("eth1 payload");
        assert_eq!(eth1.parent_interface.as_deref(), Some("bond0"));
        let parent = eth1.parent_relationship.expect("parent relationship");
        assert_eq!(parent.target, "bond0");
        assert_eq!(parent.kind, MasterKind::Parent);
        let master = eth1.master_relationship.expect("master relationship");
        assert_eq!(master.target, "br0");
        assert_eq!(master.kind, MasterKind::Bridge);
        assert_eq!(eth1.member_of, None);
    }

    #[test]
    fn bn02404_repro_contradictory_slave_and_target_type_is_unresolved() {
        let json = r#"{
          "br0": {"administrativeStatus":"up","operationalStatus":"up","index":10,"type":"Bridge","interfaceType":"bridge"},
          "eth1": {"administrativeStatus":"up","operationalStatus":"up","index":11,"type":"Ethernet","interfaceSlaveType":"Bond","masterIfindex":10}
        }"#;
        let out = normalize_frr_interface_json(&obs(json), NormalizerLimits::default());
        let eth1 = out
            .facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some("eth1"))
            .map(payload)
            .expect("eth1 payload");
        assert_eq!(eth1.member_of, None);
        assert_eq!(
            eth1.master_relationship.map(|r| r.kind),
            Some(MasterKind::Unresolved)
        );
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
