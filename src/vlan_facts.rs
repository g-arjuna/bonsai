//! Typed payload contract for VLAN/bridge forwarding-domain observations.
//!
//! This module is pure data and validation. It does not acquire data, infer
//! VLANs from interface names, infer access/trunk mode from membership count,
//! or mutate topology. Empty/missing axes remain explicit. In particular, a
//! VLAN number is not a complete L2 forwarding-domain identity: scoped bridge
//! evidence is a separate payload axis.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::interface_facts::{FieldAvailability, ObservedField};
use crate::interface_relationships::InterfaceEntityKey;
use crate::typed_facts::FactError;

pub const FACT_KIND_VLAN_BRIDGE_DOMAIN: &str = "vlan.bridge_domain";

const MAX_TEXT: usize = 255;
const MAX_MEMBERS: usize = 512;
const MAX_ATTACHMENTS: usize = 128;

fn valid_text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && value.bytes().all(|b| !b.is_ascii_control())
}

fn check_field<T>(
    field: &ObservedField<T>,
    name: &'static str,
    valid: impl Fn(&T) -> bool,
) -> Result<(), FactError> {
    match field.state {
        FieldAvailability::Known => match &field.value {
            Some(value) if valid(value) => Ok(()),
            _ => Err(FactError::InvalidField(name)),
        },
        _ if field.value.is_none() => Ok(()),
        _ => Err(FactError::InvalidField(name)),
    }
}

/// Shape of the source observation. A VLAN-database entry is scoped VLAN
/// evidence; a port-membership row is member-scoped evidence and must not be
/// collapsed across bridges merely because the VLAN number matches.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum VlanObservationShape {
    VlanDatabaseEntry,
    PortMembership,
}

/// Operational VLAN state when the source explicitly exposes it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum VlanOperationalState {
    Active,
    Suspended,
}

/// Administrative shutdown state when explicitly exposed by the source.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum VlanAdminState {
    Shutdown,
    NoShutdown,
}

/// Evidence plane for membership. `SourceReported` is deliberately weaker
/// than either operational or configured truth: the command exposed the
/// membership but this producer cannot prove which truth plane it represents.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum MembershipPlane {
    Operational,
    Configured,
    SourceReported,
}

/// Interface mode only when independently exposed by source evidence. The
/// number of VLANs on an interface is never used to infer this value.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum VlanPortMode {
    Access,
    Trunk,
    Routed,
}

/// Evidence-based attachment classification. Name patterns are insufficient;
/// producers set a known value only when interface/type evidence establishes it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum VlanAttachmentKind {
    Physical,
    LogicalAggregate,
    Svi,
    Routed,
    Tunnel,
    CpuSpecial,
    Other,
}

/// Explicit availability of the membership collection itself. This is
/// independent of the per-member tagging/type axes: a syntactically valid
/// VLAN row can still lack authoritative membership evidence.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum MembershipAvailability {
    /// Source explicitly reported an empty membership collection.
    KnownEmpty,
    /// Source reported a complete non-empty membership collection.
    KnownPopulated,
    /// Some valid members were recovered, but part of the collection was absent/malformed.
    Partial,
    /// The membership field was absent where the qualified source shape exposes it.
    Missing,
    /// The membership field was present but not usable.
    Malformed,
    /// This source/profile does not expose membership.
    Unsupported,
    /// Legacy or externally supplied facts did not establish availability.
    #[default]
    Unknown,
}

/// One interface membership in a scoped VLAN/bridge domain. Tagging axes are
/// independent observed fields so unsupported sources remain unknown rather
/// than silently becoming tagged or untagged.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VlanMember {
    pub interface_name: String,
    /// Structured canonical key when independently established. It is absent
    /// for sources that provide only a native interface string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_key: Option<InterfaceEntityKey>,
    pub tagged: ObservedField<bool>,
    pub untagged: ObservedField<bool>,
    pub pvid: ObservedField<bool>,
    pub mode: ObservedField<VlanPortMode>,
    pub attachment_kind: ObservedField<VlanAttachmentKind>,
}

/// Typed `vlan.bridge_domain` payload. Device, tenant scope and network
/// namespace/ASIC live on the fact subject; acquisition stream, generation,
/// coverage and raw evidence live on the fact envelope/normalizer output.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VlanBridgePayload {
    pub observation_shape: VlanObservationShape,
    /// Scoped bridge/forwarding-domain identity when established. EOS/NXOS
    /// VLAN-database producers may use a qualified database-domain key; Linux
    /// port rows leave this unknown unless bridge/master evidence establishes it.
    pub bridge_domain: ObservedField<String>,
    pub vlan_id: u32,
    pub vlan_name: ObservedField<String>,
    pub operational_state: ObservedField<VlanOperationalState>,
    pub admin_state: ObservedField<VlanAdminState>,
    pub dynamic: ObservedField<bool>,
    pub membership_plane: ObservedField<MembershipPlane>,
    /// Availability of the member collection itself. The member vector is
    /// interpreted only through this axis; an empty vector alone never means
    /// a confirmed empty population.
    #[serde(default)]
    pub membership_availability: MembershipAvailability,
    /// VLAN-centric member population for database entries. `KnownEmpty`
    /// membership is represented by an empty Vec in a database-entry shape;
    /// port-membership facts normally contain one member.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<VlanMember>,
    /// Routed/SVI attachments when independently exposed. This is an
    /// `ObservedField` because absence of the field is not positive evidence
    /// that no routed attachment exists.
    pub routed_interfaces: ObservedField<Vec<String>>,
    /// VRF/network-instance association when independently established by
    /// source or typed interface evidence. It is never derived from a VRF or
    /// SVI name.
    pub vrf: ObservedField<String>,
    /// Stable scoped correlation handle. Device/scope/network namespace are
    /// supplied by the fact subject; this key covers only the domain/VLAN or
    /// membership occurrence axes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_entity_key: Option<String>,
}

pub fn vlan_database_entity_key(vlan_id: u32) -> String {
    format!("vlan/{vlan_id}")
}

pub fn bridge_vlan_entity_key(bridge_domain: &str, vlan_id: u32) -> String {
    format!("bridge/{bridge_domain}/vlan/{vlan_id}")
}

pub fn port_membership_entity_key(interface_name: &str, vlan_id: u32) -> String {
    format!("port/{interface_name}/vlan/{vlan_id}")
}

pub fn validate_vlan_payload(fields: &Value) -> Result<(), FactError> {
    let payload: VlanBridgePayload = serde_json::from_value(fields.clone())
        .map_err(|_| FactError::InvalidField("payload.fields.vlan.bridge_domain"))?;
    validate_vlan_typed(&payload)
}

pub fn validate_vlan_typed(payload: &VlanBridgePayload) -> Result<(), FactError> {
    if payload.vlan_id == 0 || payload.vlan_id > 4095 {
        return Err(FactError::InvalidField("vlan_id"));
    }
    check_field(&payload.bridge_domain, "bridge_domain", |v| {
        valid_text(v, MAX_TEXT)
    })?;
    check_field(&payload.vlan_name, "vlan_name", |v| valid_text(v, MAX_TEXT))?;
    check_field(&payload.operational_state, "operational_state", |_| true)?;
    check_field(&payload.admin_state, "admin_state", |_| true)?;
    check_field(&payload.dynamic, "dynamic", |_| true)?;
    check_field(&payload.membership_plane, "membership_plane", |_| true)?;
    check_field(
        &payload.routed_interfaces,
        "routed_interfaces",
        |interfaces| {
            interfaces.len() <= MAX_ATTACHMENTS
                && interfaces.iter().all(|i| valid_text(i, MAX_TEXT))
                && interfaces
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == interfaces.len()
        },
    )?;
    check_field(&payload.vrf, "vrf", |v| valid_text(v, MAX_TEXT))?;
    if payload.members.len() > MAX_MEMBERS {
        return Err(FactError::InvalidField("members"));
    }
    let mut member_names = std::collections::BTreeSet::new();
    for member in &payload.members {
        if !valid_text(&member.interface_name, MAX_TEXT) {
            return Err(FactError::InvalidField("members.interface_name"));
        }
        if !member_names.insert(member.interface_name.as_str()) {
            return Err(FactError::InvalidField("members duplicate interface"));
        }
        check_field(&member.tagged, "members.tagged", |_| true)?;
        check_field(&member.untagged, "members.untagged", |_| true)?;
        check_field(&member.pvid, "members.pvid", |_| true)?;
        check_field(&member.mode, "members.mode", |_| true)?;
        check_field(&member.attachment_kind, "members.attachment_kind", |_| true)?;
        if member.tagged.state == FieldAvailability::Known
            && member.untagged.state == FieldAvailability::Known
            && member.tagged.value == Some(true)
            && member.untagged.value == Some(true)
        {
            return Err(FactError::InvalidField(
                "members cannot be both tagged and untagged for one VLAN",
            ));
        }
    }
    match payload.membership_availability {
        MembershipAvailability::KnownEmpty if !payload.members.is_empty() => {
            return Err(FactError::InvalidField(
                "known-empty membership cannot carry members",
            ));
        }
        MembershipAvailability::KnownPopulated | MembershipAvailability::Partial
            if payload.members.is_empty() =>
        {
            return Err(FactError::InvalidField(
                "populated/partial membership requires recovered members",
            ));
        }
        MembershipAvailability::Missing | MembershipAvailability::Malformed
            if !payload.members.is_empty() =>
        {
            return Err(FactError::InvalidField(
                "missing/malformed membership cannot silently carry a complete member vector",
            ));
        }
        _ => {}
    }
    match payload.observation_shape {
        VlanObservationShape::VlanDatabaseEntry => {
            if payload.bridge_domain.state != FieldAvailability::Known {
                return Err(FactError::InvalidField(
                    "vlan database entry requires scoped bridge_domain",
                ));
            }
        }
        VlanObservationShape::PortMembership => {
            if payload.members.len() != 1 {
                return Err(FactError::InvalidField(
                    "port membership requires exactly one member",
                ));
            }
        }
    }
    if let Some(key) = &payload.domain_entity_key
        && !valid_text(key, 512)
    {
        return Err(FactError::InvalidField("domain_entity_key"));
    }
    Ok(())
}
