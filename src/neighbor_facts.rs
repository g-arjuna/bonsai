//! Typed payload contracts for ARP/IPv6-ND neighbor and MAC forwarding facts.
//!
//! This module is pure data + validation. It performs no acquisition, no
//! device identity resolution and no graph mutation. The payloads deliberately
//! preserve source uncertainty: an absent age/state/VLAN is explicit unknown,
//! not a fabricated zero/default, and a MAC forwarding entry is evidence of a
//! table observation — not independent proof of data-plane forwarding.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::IpAddr;

use crate::interface_facts::{FieldAvailability, ObservedField, normalize_mac};
use crate::typed_facts::FactError;

pub const FACT_KIND_ARPND_NEIGHBOR: &str = "arpnd.neighbor";
pub const FACT_KIND_MAC_FORWARDING: &str = "mac.forwarding";

const MAX_INTERFACE_LEN: usize = 255;
const MAX_DETAIL_LEN: usize = 200;
const MAX_FLAGS: usize = 32;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AddressFamily {
    Ipv4,
    Ipv6,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum NeighborResolutionState {
    /// Generic resolved/successful state when the source does not expose a
    /// more precise neighbor-cache state.
    Resolved,
    Reachable,
    Stale,
    Delay,
    Probe,
    Incomplete,
    Failed,
    Permanent,
    #[serde(rename = "noarp")]
    NoArp,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum MacEntryType {
    Static,
    Dynamic,
    Permanent,
    Unknown,
}

/// Typed forwarding-entry context evidence. Several contexts can legitimately
/// apply to one entry (for example master-table + externally-learned).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum MacEntryContext {
    Local,
    Remote,
    SelfTable,
    MasterTable,
    ExternallyLearned,
    Offloaded,
    Router,
    Special,
    Other,
    Unknown,
}

fn valid_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| !b.is_ascii_control())
}

fn check_field<T>(
    field: &ObservedField<T>,
    name: &'static str,
    valid: impl Fn(&T) -> bool,
) -> Result<(), FactError> {
    match field.state {
        FieldAvailability::Known => match &field.value {
            Some(v) if valid(v) => Ok(()),
            _ => Err(FactError::InvalidField(name)),
        },
        _ if field.value.is_none() => Ok(()),
        _ => Err(FactError::InvalidField(name)),
    }
}

/// One normalized ARP/ND neighbor observation. `outgoing_interface` is the
/// interface evidence exactly as selected by the qualified source profile;
/// `physical_interface` is additional source evidence (for example NXOS
/// `phy-intf`) and never a guessed resolution. `interface_vlan_id` is present
/// only when the qualified source contract establishes the VLAN interface
/// notation, not merely because a string contains a number.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ArpndNeighborPayload {
    pub ip_address: String,
    pub address_family: AddressFamily,
    pub mac_address: ObservedField<String>,
    pub resolution_state: ObservedField<NeighborResolutionState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_state: Option<String>,
    pub age_seconds: ObservedField<u64>,
    pub outgoing_interface: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physical_interface: Option<String>,
    pub interface_vlan_id: ObservedField<u32>,
    pub remote: ObservedField<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router: Option<bool>,
    /// Complete interface evidence before compound-interface decomposition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_interface_raw: Option<String>,
    /// Semantic neighbor-entry correlation key derived from scoped identity
    /// components. The components remain authoritative; this key is a stable
    /// correlation handle, not a flattened replacement for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub neighbor_entity_key: Option<String>,
    /// Bridge/L2 forwarding-domain association when independently established
    /// by source or normalization evidence. Unknown means no L2 join is made.
    pub forwarding_domain: ObservedField<String>,
}

/// One observed MAC forwarding-table entry. `outgoing_interface` is source
/// evidence. For remote tunnel evidence (for example NXOS `nve1(a.b.c.d)`),
/// the remote VTEP address is separated only when the syntax is explicit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MacForwardingPayload {
    pub mac_address: String,
    pub vlan_id: ObservedField<u32>,
    pub outgoing_interface: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_outgoing_interface: Option<String>,
    pub entry_type: ObservedField<MacEntryType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_vtep_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
    pub move_count: ObservedField<u64>,
    /// Source-reported last-move epoch seconds, preserved as source units.
    /// It is not converted into a validity interval without capture-time
    /// semantics.
    pub last_move_epoch_seconds: ObservedField<f64>,
    /// Bridge/forwarding domain established by source evidence (for example
    /// the Linux master bridge name). Unknown means VLAN alone is not promoted
    /// to a forwarding-domain identity.
    pub bridge_domain: ObservedField<String>,
    /// Typed source context evidence; free-form flags remain in `flags`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entry_contexts: Vec<MacEntryContext>,
    /// Semantic forwarding-entry correlation key derived from scoped identity
    /// components. The components remain authoritative; this key is a stable
    /// correlation handle, not a flattened replacement for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarding_entity_key: Option<String>,
}

pub fn validate_arpnd_payload(fields: &Value) -> Result<(), FactError> {
    let payload: ArpndNeighborPayload = serde_json::from_value(fields.clone())
        .map_err(|_| FactError::InvalidField("payload.fields.arpnd.neighbor"))?;
    validate_arpnd_typed(&payload)
}

pub fn validate_mac_payload(fields: &Value) -> Result<(), FactError> {
    let payload: MacForwardingPayload = serde_json::from_value(fields.clone())
        .map_err(|_| FactError::InvalidField("payload.fields.mac.forwarding"))?;
    validate_mac_typed(&payload)
}

pub fn validate_arpnd_typed(p: &ArpndNeighborPayload) -> Result<(), FactError> {
    let ip: IpAddr = p
        .ip_address
        .parse()
        .map_err(|_| FactError::InvalidField("ip_address"))?;
    match (ip, p.address_family) {
        (IpAddr::V4(_), AddressFamily::Ipv4) | (IpAddr::V6(_), AddressFamily::Ipv6) => {}
        _ => return Err(FactError::InvalidField("address_family")),
    }
    if !valid_text(&p.outgoing_interface, MAX_INTERFACE_LEN) {
        return Err(FactError::InvalidField("outgoing_interface"));
    }
    if let Some(i) = &p.physical_interface
        && !valid_text(i, MAX_INTERFACE_LEN)
    {
        return Err(FactError::InvalidField("physical_interface"));
    }
    if let Some(i) = &p.source_interface_raw
        && !valid_text(i, MAX_INTERFACE_LEN)
    {
        return Err(FactError::InvalidField("source_interface_raw"));
    }
    check_field(&p.mac_address, "mac_address", |v| {
        normalize_mac(v).is_some_and(|n| n == *v)
    })?;
    check_field(&p.resolution_state, "resolution_state", |_| true)?;
    check_field(&p.age_seconds, "age_seconds", |_| true)?;
    check_field(&p.interface_vlan_id, "interface_vlan_id", |v| *v <= 4095)?;
    check_field(&p.remote, "remote", |_| true)?;
    if let Some(raw) = &p.raw_state
        && !valid_text(raw, 64)
    {
        return Err(FactError::InvalidField("raw_state"));
    }
    if let Some(protocol) = &p.protocol
        && !valid_text(protocol, 64)
    {
        return Err(FactError::InvalidField("protocol"));
    }
    check_field(&p.forwarding_domain, "forwarding_domain", |v| {
        valid_text(v, MAX_DETAIL_LEN)
    })?;
    if let Some(key) = &p.neighbor_entity_key
        && !valid_text(key, 512)
    {
        return Err(FactError::InvalidField("neighbor_entity_key"));
    }
    Ok(())
}

pub fn validate_mac_typed(p: &MacForwardingPayload) -> Result<(), FactError> {
    if normalize_mac(&p.mac_address).is_none_or(|n| n != p.mac_address) {
        return Err(FactError::InvalidField("mac_address"));
    }
    if !valid_text(&p.outgoing_interface, MAX_INTERFACE_LEN) {
        return Err(FactError::InvalidField("outgoing_interface"));
    }
    if let Some(i) = &p.normalized_outgoing_interface
        && !valid_text(i, MAX_INTERFACE_LEN)
    {
        return Err(FactError::InvalidField("normalized_outgoing_interface"));
    }
    check_field(&p.vlan_id, "vlan_id", |v| *v <= 4095)?;
    check_field(&p.entry_type, "entry_type", |_| true)?;
    check_field(&p.move_count, "move_count", |_| true)?;
    check_field(&p.last_move_epoch_seconds, "last_move_epoch_seconds", |v| {
        v.is_finite() && *v >= 0.0
    })?;
    if let Some(ip) = &p.remote_vtep_ip
        && ip.parse::<IpAddr>().is_err()
    {
        return Err(FactError::InvalidField("remote_vtep_ip"));
    }
    if p.flags.len() > MAX_FLAGS
        || p.flags
            .iter()
            .any(|f| !valid_text(f, 64) || f.len() > MAX_DETAIL_LEN)
    {
        return Err(FactError::InvalidField("flags"));
    }
    check_field(&p.bridge_domain, "bridge_domain", |v| {
        valid_text(v, MAX_DETAIL_LEN)
    })?;
    if p.entry_contexts.len() > MAX_FLAGS {
        return Err(FactError::InvalidField("entry_contexts"));
    }
    if let Some(key) = &p.forwarding_entity_key
        && !valid_text(key, 512)
    {
        return Err(FactError::InvalidField("forwarding_entity_key"));
    }
    Ok(())
}

/// Deterministic scoped subject key for a neighbor fact. The VRF axis remains
/// on `FactSubject::vrf`; this key keeps the address itself unambiguous when
/// the producing source could not establish a VRF.
pub fn arpnd_entity_key(vrf: Option<&str>, ip: &str) -> String {
    format!("ip/{}/{}", vrf.unwrap_or("<unknown>"), ip)
}

/// Neighbor attachment identity used for correlation. Interface is included
/// for every family because even non-link-local addresses can recur on
/// distinct attachments inside one VRF in malformed or transitional evidence.
pub fn arpnd_attachment_entity_key(
    vrf: Option<&str>,
    family: AddressFamily,
    ip: &str,
    interface: &str,
    subinterface_index: Option<u32>,
) -> String {
    serde_json::json!({
        "neighbor": arpnd_entity_key(vrf, ip),
        "address_family": family,
        "interface_attachment": interface,
        "subinterface_index": subinterface_index,
    })
    .to_string()
}

/// Deterministic scoped subject key for a MAC table entry. VLAN absence is
/// explicit rather than silently treating the entry as VLAN zero.
pub fn mac_entity_key(vlan: Option<u32>, mac: &str) -> String {
    match vlan {
        Some(v) => format!("mac/vlan/{v}/{mac}"),
        None => format!("mac/vlan/<unknown>/{mac}"),
    }
}

/// Forwarding-entry identity scoped by bridge/forwarding-domain evidence.
pub fn mac_forwarding_entity_key(
    bridge_domain: Option<&str>,
    vlan: Option<u32>,
    mac: &str,
) -> String {
    serde_json::json!({
        "bridge_domain": bridge_domain,
        "entry": mac_entity_key(vlan, mac),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn observed_mac() -> ObservedField<String> {
        ObservedField::known("44:38:39:e3:19:2e".to_string())
    }

    #[test]
    fn arpnd_valid_payload_accepted() {
        let p = ArpndNeighborPayload {
            ip_address: "172.16.3.202".into(),
            address_family: AddressFamily::Ipv4,
            mac_address: observed_mac(),
            resolution_state: ObservedField::known(NeighborResolutionState::Resolved),
            raw_state: None,
            age_seconds: ObservedField::unavailable(FieldAvailability::Unknown, None),
            outgoing_interface: "Vlan30".into(),
            physical_interface: Some("Vxlan1".into()),
            interface_vlan_id: ObservedField::known(30),
            remote: ObservedField::known(true),
            protocol: None,
            router: None,
            source_interface_raw: Some("Vlan30, Vxlan1".into()),
            neighbor_entity_key: None,
            forwarding_domain: ObservedField::unavailable(
                FieldAvailability::Unknown,
                Some("test domain unknown".into()),
            ),
        };
        validate_arpnd_typed(&p).unwrap();
        validate_arpnd_payload(&serde_json::to_value(&p).unwrap()).unwrap();
    }

    #[test]
    fn mac_validation_rejects_nonnormalized_mac() {
        let p = MacForwardingPayload {
            mac_address: "4438.39e3.192e".into(),
            vlan_id: ObservedField::known(30),
            outgoing_interface: "Port-Channel4".into(),
            normalized_outgoing_interface: None,
            entry_type: ObservedField::known(MacEntryType::Dynamic),
            remote_vtep_ip: None,
            flags: vec![],
            move_count: ObservedField::unavailable(FieldAvailability::Unknown, None),
            last_move_epoch_seconds: ObservedField::unavailable(FieldAvailability::Unknown, None),
            bridge_domain: ObservedField::unavailable(FieldAvailability::Unknown, None),
            entry_contexts: vec![],
            forwarding_entity_key: None,
        };
        assert!(validate_mac_typed(&p).is_err());
    }

    #[test]
    fn availability_states_forbid_values() {
        let v = json!({
            "ip_address":"10.0.0.1",
            "address_family":"ipv4",
            "mac_address":{"state":"unknown","value":"00:00:00:00:00:00"},
            "resolution_state":{"state":"known","value":"resolved"},
            "age_seconds":{"state":"unknown"},
            "outgoing_interface":"Ethernet1",
            "interface_vlan_id":{"state":"unknown"},
            "remote":{"state":"unknown"}
        });
        assert!(validate_arpnd_payload(&v).is_err());
    }
}
