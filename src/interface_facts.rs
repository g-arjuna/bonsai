//! Typed payload contract for `fact_kind = "interface.state"` (BN-024-04/G1).
//!
//! This is the first genuinely typed domain payload on the BN-024-02 fact
//! envelope: every observed attribute carries an explicit availability
//! state so "unknown", "unsupported", "invalid" and "absent" never collapse
//! into fabricated values (no unknown→down, no missing→0). The module is
//! pure data + validation — it performs no collection, parsing of device
//! output, or graph mutation.
//!
//! Truth-plane note (Stage K): this payload describes the *operational*
//! observation plane only. Configured/effective/startup interface state is
//! a separate fact kind and must never be merged into these fields.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::typed_facts::FactError;

/// Dotted fact kind for normalized interface operational state.
pub const FACT_KIND_INTERFACE_STATE: &str = "interface.state";

/// Explicit availability of one observed field. `Known` requires `value`;
/// every other state forbids it — a failed or unsupported read is never
/// represented as a value.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FieldAvailability {
    /// The source provided a usable value.
    Known,
    /// The source did not provide this field; semantics unknown.
    Unknown,
    /// The source/NOS does not expose this field in this observation.
    Unsupported,
    /// The source provided a value that failed validation.
    Invalid,
}

/// One observed attribute with explicit availability.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(bound(deserialize = "T: serde::Deserialize<'de>"))]
pub struct ObservedField<T> {
    pub state: FieldAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    /// Bounded, sanitized explanation for non-`known` states or caveats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl<T> ObservedField<T> {
    pub fn known(value: T) -> Self {
        Self {
            state: FieldAvailability::Known,
            value: Some(value),
            detail: None,
        }
    }

    pub fn unavailable(state: FieldAvailability, detail: Option<String>) -> Self {
        debug_assert_ne!(state, FieldAvailability::Known);
        Self {
            state,
            value: None,
            detail,
        }
    }
}

/// Administrative state as reported by the device (config intent plane
/// observed through operational output — NOT the configured plane).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdminState {
    Up,
    Down,
}

/// Operational (protocol/line) state. `LowerLayerDown`, `Dormant`,
/// `Testing` and a device-reported `Unknown` are distinct from `Down`:
/// they must not be collapsed into a boolean, and an administrative
/// down never implies an operational observation.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperState {
    Up,
    Down,
    LowerLayerDown,
    Dormant,
    Testing,
    /// The source explicitly reported an unknown operstate. Distinct
    /// from `FieldAvailability::Unknown` (field absent) — the device
    /// did report a state word; its value is "unknown".
    Unknown,
}

/// Coarse interface classification. `Unknown` is a first-class value —
/// an unrecognized type string maps here with detail, never to a guess.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceType {
    /// Physical port / front-panel interface.
    Physical,
    /// Routed port / L3 interface.
    Routed,
    /// Switched virtual interface.
    Svi,
    /// Loopback / virtual loopback.
    Loopback,
    /// Port-channel / LAG logical interface.
    PortChannel,
    /// VLAN / bridge / other virtual interface.
    Virtual,
    /// Tunnel interface.
    Tunnel,
    /// Classification could not be established.
    Unknown,
}

/// Speed value plus the unit the producing profile asserts for it.
/// `Unknown` is a first-class unit: several NOS CLI formats emit a bare
/// number whose scale is version- or vendor-dependent, and guessing a
/// unit would fabricate precision the observation does not carry.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpeedObservation {
    pub value: u64,
    pub unit: SpeedUnit,
}

/// Declared speed unit. Only units the profile can evidence may be used;
/// `Unknown` keeps the raw magnitude without a scale claim.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpeedUnit {
    BitsPerSecond,
    KilobitsPerSecond,
    MegabitsPerSecond,
    /// Scale not established for this source/format/release.
    Unknown,
}

/// How a reported master/parent relationship was classified. The
/// classification is only made when the target's own type is
/// independently observed in the same observation; a bare master name
/// is never assumed to be a LAG.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MasterKind {
    /// Target observed as a LAG/bond aggregate — `member_of` is set.
    Lag,
    /// Target observed as a bridge device.
    Bridge,
    /// Target observed as a VRF device.
    Vrf,
    /// Target observed as a physical/parent interface (e.g. macvlan
    /// parent, subinterface attachment).
    Parent,
    /// Master reported but the target's type/entity was not
    /// established — the raw target is preserved, nothing inferred.
    Unresolved,
}

/// An observed master/parent relationship with its evidence-based
/// classification. `target` preserves the reported name exactly
/// (case-sensitive interface identifiers are not folded).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MasterRelationship {
    pub target: String,
    pub kind: MasterKind,
}

/// How the typed value was chosen when two source observations of the
/// same field disagreed.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictResolution {
    /// The command-order primary observation (first source) was kept.
    PrimaryWins,
    /// The alternate source's observation was preferred (e.g. the
    /// iproute2 linkinfo type over the vtysh type string).
    AlternatePreferred,
    /// No defensible selection; the field carries an explicit
    /// unknown/invalid state and both observations remain here.
    Unresolved,
}

/// A disagreement between source observations of the same entity,
/// preserved as evidence instead of being collapsed into a single
/// authoritative-looking value. Field names are the typed payload or
/// subject axes they affect (e.g. `"mtu"`, `"operational_state"`,
/// `"member_of"`, `"vrf"`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceFieldConflict {
    pub field: String,
    /// Observation that supplied the effective typed value.
    pub effective_source: String,
    pub effective_value: String,
    /// The disagreeing observation.
    pub alternate_source: String,
    pub alternate_value: String,
    pub resolution: ConflictResolution,
}

/// Typed `interface.state` payload. Identity axes (device, network
/// namespace, entity name, VRF) live on `TypedFact::subject`; this struct
/// carries only the observed attributes of that entity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct InterfaceStatePayload {
    /// Interface name exactly as observed (bounded, sanitized).
    pub interface_name: String,
    /// Canonical/normalized name when it differs from the observed form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_name: Option<String>,
    pub administrative_state: ObservedField<AdminState>,
    pub operational_state: ObservedField<OperState>,
    /// Interface index as reported by the source(s). Identity evidence
    /// for detecting contradictory observations of the same name;
    /// never a routing key by itself.
    pub ifindex: ObservedField<u64>,
    pub interface_type: ObservedField<InterfaceType>,
    /// MTU in bytes as reported; unit semantics are NOS-specific and
    /// recorded by the producing profile, not assumed here.
    pub mtu: ObservedField<u32>,
    /// Speed as reported by the source together with its declared unit.
    /// `unit: unknown` means the profile could not establish the scale —
    /// the raw value is preserved as evidence and is never presented as
    /// bits-per-second. Producers that can prove the unit must say so.
    pub speed: ObservedField<SpeedObservation>,
    /// MAC address normalized to lowercase `xx:xx:xx:xx:xx:xx`.
    pub mac_address: ObservedField<String>,
    /// Optional description; bounded and control-character stripped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Parent interface (e.g. physical member of a port-channel) — only
    /// populated when independently established by the observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_interface: Option<String>,
    /// Port-channel/LAG this interface is a member of. Populated ONLY
    /// when the master target is independently established as a LAG
    /// aggregate in the same observation (see `master_relationship`) —
    /// a bare master name is never assumed to be a LAG.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_of: Option<String>,
    /// Observed master/parent relationship, classified by evidence.
    /// Present whenever a source reported a master; `kind: unresolved`
    /// means the target was reported but its type was not established.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_relationship: Option<MasterRelationship>,
    /// Cross-source disagreements preserved as evidence. Empty means no
    /// conflicting observations were recorded for this entity's typed
    /// fields — it does NOT assert the sources agreed on everything.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_conflicts: Vec<SourceFieldConflict>,
    /// IPv4 addresses observed on the interface as `addr/prefix` strings,
    /// in source order. `Known` with an empty list means "observed, none";
    /// `Unknown` means the source did not report addressing at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_addresses: Option<ObservedField<Vec<String>>>,
    /// IPv6 addresses observed on the interface as `addr/prefix` strings.
    /// Link-local and other non-routable scopes are preserved verbatim —
    /// scope classification is a consumer concern, not silently dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv6_addresses: Option<ObservedField<Vec<String>>>,
}

const MAX_NAME_LEN: usize = 255;
const MAX_DETAIL_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 240;

pub(crate) fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_NAME_LEN && !s.chars().any(|c| c.is_control())
}

fn valid_detail(s: &Option<String>) -> bool {
    s.as_deref()
        .is_none_or(|d| d.len() <= MAX_DETAIL_LEN && !d.chars().any(|c| c.is_control()))
}

fn check_field<T>(
    field: &ObservedField<T>,
    name: &'static str,
    extra: impl Fn(&T) -> bool,
) -> Result<(), FactError> {
    match field.state {
        FieldAvailability::Known => {
            let Some(v) = &field.value else {
                return Err(FactError::InvalidField(name));
            };
            if !extra(v) {
                return Err(FactError::InvalidField(name));
            }
        }
        _ => {
            if field.value.is_some() {
                return Err(FactError::InvalidField(name));
            }
        }
    }
    if !valid_detail(&field.detail) {
        return Err(FactError::InvalidField(name));
    }
    Ok(())
}

/// Normalize a MAC address to lowercase `xx:xx:xx:xx:xx:xx`.
/// Accepts exactly three well-formed layouts: six `xx` groups joined
/// by `:`, six `xx` groups joined by `-`, the Cisco `xxxx.xxxx.xxxx`
/// form, or a bare 12-hex-digit string. Mixed or malformed separator
/// layouts are rejected — stripping separators and counting hex digits
/// would accept inputs like `aa:bb-cc.dd:ee:ff` that no source emits.
/// Returns `None` for anything else — callers must surface
/// `FieldAvailability::Invalid`, never a partial parse.
pub fn normalize_mac(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let groups: Vec<&str> = if trimmed.contains(':') {
        trimmed.split(':').collect()
    } else if trimmed.contains('-') {
        trimmed.split('-').collect()
    } else if trimmed.contains('.') {
        trimmed.split('.').collect()
    } else {
        vec![trimmed]
    };
    let hex_ok = |g: &str, len: usize| g.len() == len && g.bytes().all(|b| b.is_ascii_hexdigit());
    let valid_layout = if trimmed.contains('.') {
        // Cisco xxxx.xxxx.xxxx: exactly three 4-hex groups.
        groups.len() == 3 && groups.iter().all(|g| hex_ok(g, 4))
    } else if trimmed.contains(':') || trimmed.contains('-') {
        // Six 2-hex groups with a single consistent separator (the
        // split above already guarantees consistency).
        groups.len() == 6 && groups.iter().all(|g| hex_ok(g, 2))
    } else {
        // Bare 12-hex-digit form.
        groups.len() == 1 && hex_ok(groups[0], 12)
    };
    if !valid_layout {
        return None;
    }
    let hex: String = trimmed.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let lower = hex.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut out = String::with_capacity(17);
    for i in 0..6 {
        if i > 0 {
            out.push(':');
        }
        out.push(bytes[i * 2] as char);
        out.push(bytes[i * 2 + 1] as char);
    }
    Some(out)
}

/// Validate a serialized `interface.state` payload. Called by
/// `TypedFact::validate` for this fact kind; other kinds keep the
/// generic object-shape contract only.
pub fn validate_payload(fields: &Value) -> Result<(), FactError> {
    let payload: InterfaceStatePayload = serde_json::from_value(fields.clone())
        .map_err(|_| FactError::InvalidField("payload.fields.interface.state"))?;
    validate_typed(&payload)
}

/// Semantic validation of an already-typed payload.
pub fn validate_typed(p: &InterfaceStatePayload) -> Result<(), FactError> {
    if !valid_name(&p.interface_name) {
        return Err(FactError::InvalidField("interface_name"));
    }
    if let Some(n) = &p.normalized_name
        && !valid_name(n)
    {
        return Err(FactError::InvalidField("normalized_name"));
    }
    check_field(&p.administrative_state, "administrative_state", |_| true)?;
    check_field(&p.operational_state, "operational_state", |_| true)?;
    check_field(&p.ifindex, "ifindex", |v| *v > 0)?;
    check_field(&p.interface_type, "interface_type", |_| true)?;
    check_field(&p.mtu, "mtu", |v| *v > 0 && *v <= u16::MAX as u32)?;
    check_field(&p.speed, "speed", |v| v.value > 0)?;
    check_field(&p.mac_address, "mac_address", |v| {
        // Stored value must already be in normalized form.
        normalize_mac(v).is_some_and(|n| n == *v)
    })?;
    if let Some(d) = &p.description
        && (d.len() > MAX_DESCRIPTION_LEN || d.chars().any(|c| c.is_control()))
    {
        return Err(FactError::InvalidField("description"));
    }
    for name in [&p.parent_interface, &p.member_of].into_iter().flatten() {
        if !valid_name(name) {
            return Err(FactError::InvalidField("interface reference"));
        }
    }
    if let Some(rel) = &p.master_relationship {
        if !valid_name(&rel.target) {
            return Err(FactError::InvalidField("master_relationship.target"));
        }
    }
    for c in &p.source_conflicts {
        if c.field.is_empty() || c.field.len() > 64 || c.field.chars().any(|ch| ch.is_control()) {
            return Err(FactError::InvalidField("source_conflicts.field"));
        }
        for (name, v) in [
            ("effective_source", &c.effective_source),
            ("alternate_source", &c.alternate_source),
            ("effective_value", &c.effective_value),
            ("alternate_value", &c.alternate_value),
        ] {
            if v.is_empty() || v.len() > 1024 || v.chars().any(|ch| ch.is_control()) {
                return Err(FactError::InvalidField(name));
            }
        }
    }
    for (field, name, want_v6) in [
        (&p.ip_addresses, "ip_addresses", false),
        (&p.ipv6_addresses, "ipv6_addresses", true),
    ] {
        if let Some(f) = field {
            check_field(f, name, |addrs| {
                addrs.len() <= 64 && addrs.iter().all(|a| valid_addr_entry(a, want_v6)) && {
                    let mut seen = std::collections::BTreeSet::new();
                    addrs.iter().all(|a| seen.insert(a.as_str()))
                }
            })?;
        }
    }
    Ok(())
}

/// Validate one `addr/prefix` (optionally `addr%zone/prefix`) entry.
/// The address must parse as the declared family and the prefix must
/// be in range. Zone identifiers (e.g. `fe80::1%eth0/64`) are
/// preserved and validated syntactically, never stripped into a
/// different address. Host addresses are NOT rewritten to network
/// prefixes — the observed interface address is kept verbatim.
pub(crate) fn valid_addr_entry(addr: &str, want_v6: bool) -> bool {
    if addr.is_empty() || addr.len() > 64 || addr.chars().any(|c| c.is_control()) {
        return false;
    }
    let Some((host, prefix)) = addr.rsplit_once('/') else {
        return false;
    };
    if prefix.is_empty() || !prefix.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let Ok(p) = prefix.parse::<u32>() else {
        return false;
    };
    // Optional zone id (IPv6 link-local scope, `fe80::1%eth0/64`) —
    // validated separately from the address proper. Zone ids are an
    // IPv6-only concept.
    let (ip, zone) = match host.split_once('%') {
        Some((h, z)) => (h, Some(z)),
        None => (host, None),
    };
    if let Some(z) = zone
        && (z.is_empty() || z.len() > 48 || z.chars().any(|c| c.is_control()))
    {
        return false;
    }
    if want_v6 {
        p <= 128 && ip.parse::<std::net::Ipv6Addr>().is_ok()
    } else {
        p <= 32 && zone.is_none() && ip.parse::<std::net::Ipv4Addr>().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn known_payload() -> InterfaceStatePayload {
        InterfaceStatePayload {
            interface_name: "Ethernet0".into(),
            normalized_name: None,
            administrative_state: ObservedField::known(AdminState::Up),
            operational_state: ObservedField::known(OperState::Up),
            ifindex: ObservedField::known(3),
            interface_type: ObservedField::known(InterfaceType::Physical),
            mtu: ObservedField::known(9216),
            speed: ObservedField::known(SpeedObservation {
                value: 100_000,
                unit: SpeedUnit::MegabitsPerSecond,
            }),
            mac_address: ObservedField::known("aa:bb:cc:dd:ee:ff".into()),
            description: Some("server-facing".into()),
            parent_interface: None,
            member_of: None,
            master_relationship: None,
            source_conflicts: vec![],
            ip_addresses: Some(ObservedField::known(vec!["10.0.0.2/24".into()])),
            ipv6_addresses: Some(ObservedField::known(vec![])),
        }
    }

    #[test]
    fn valid_payload_roundtrips() {
        let p = known_payload();
        let v = serde_json::to_value(&p).unwrap();
        validate_payload(&v).unwrap();
    }

    #[test]
    fn unknown_is_not_a_value() {
        let mut p = known_payload();
        p.speed = ObservedField {
            state: FieldAvailability::Unknown,
            value: Some(SpeedObservation {
                value: 0,
                unit: SpeedUnit::Unknown,
            }), // must be rejected: value on non-known state
            detail: None,
        };
        assert!(validate_typed(&p).is_err());
        p.speed = ObservedField::unavailable(FieldAvailability::Unknown, None);
        assert!(validate_typed(&p).is_ok());
    }

    #[test]
    fn invalid_mac_and_bad_mtu_rejected() {
        let mut p = known_payload();
        p.mac_address = ObservedField::known("not-a-mac".into());
        assert!(validate_typed(&p).is_err());
        let mut p = known_payload();
        p.mtu = ObservedField::known(0);
        assert!(validate_typed(&p).is_err());
    }

    #[test]
    fn mac_normalization_accepts_vendor_forms() {
        assert_eq!(
            normalize_mac("aabb.ccdd.eeff").as_deref(),
            Some("aa:bb:cc:dd:ee:ff")
        );
        assert_eq!(
            normalize_mac("AA-BB-CC-DD-EE-FF").as_deref(),
            Some("aa:bb:cc:dd:ee:ff")
        );
        assert_eq!(
            normalize_mac("aabbccddeeff").as_deref(),
            Some("aa:bb:cc:dd:ee:ff")
        );
        assert_eq!(normalize_mac("zz:bb:cc:dd:ee:ff"), None);
        assert_eq!(normalize_mac("aa:bb"), None);
        // Malformed separator layouts: 12 hex digits after stripping is
        // not sufficient — group structure must be well-formed.
        assert_eq!(normalize_mac("aa:bb-cc.dd:ee:ff"), None);
        assert_eq!(normalize_mac("aabb:ccdd:eeff"), None);
        assert_eq!(normalize_mac("aa:bb:cc:dd:ee:ff:00"), None);
        assert_eq!(normalize_mac("aa.bb:cc.dd:ee:ff"), None);
        assert_eq!(
            normalize_mac("aabb.ccdd.eeff").as_deref(),
            Some("aa:bb:cc:dd:ee:ff")
        );
    }

    #[test]
    fn scalar_fields_value_rejected() {
        // A bare scalar is not a valid interface.state payload.
        assert!(validate_payload(&json!(42)).is_err());
        assert!(validate_payload(&json!({"interface_name": "Eth0"})).is_err());
    }

    #[test]
    fn address_entries_must_be_valid_cidr_of_declared_family() {
        let mut p = known_payload();
        for bad in [
            "garbage/24",
            "999.999.999.999/24",
            "10.0.0.1/99",
            "10.0.0.1",
            "10.0.0.1/24/extra",
            "2001:db8::1/200",
            "::1", // no prefix
        ] {
            p.ip_addresses = Some(ObservedField::known(vec![bad.into()]));
            assert!(validate_typed(&p).is_err(), "accepted bad v4 entry {bad}");
            p.ipv6_addresses = Some(ObservedField::known(vec![bad.into()]));
            assert!(validate_typed(&p).is_err(), "accepted bad v6 entry {bad}");
        }
        // Family separation: v6 address in the v4 list and vice versa.
        p.ip_addresses = Some(ObservedField::known(vec!["2001:db8::1/64".into()]));
        assert!(validate_typed(&p).is_err());
        p.ip_addresses = Some(ObservedField::known(vec!["10.0.0.1/24".into()]));
        p.ipv6_addresses = Some(ObservedField::known(vec!["10.0.0.1/24".into()]));
        assert!(validate_typed(&p).is_err());
        // Duplicates rejected.
        p.ipv6_addresses = Some(ObservedField::known(vec![
            "2001:db8::1/64".into(),
            "2001:db8::1/64".into(),
        ]));
        assert!(validate_typed(&p).is_err());
        // Valid entries pass, including loopback and a zoned link-local.
        p.ip_addresses = Some(ObservedField::known(vec![
            "10.0.0.2/24".into(),
            "127.0.0.1/32".into(),
        ]));
        p.ipv6_addresses = Some(ObservedField::known(vec![
            "::1/128".into(),
            "fe80::1%eth0/64".into(),
            "2001:db8::1/64".into(),
        ]));
        assert!(validate_typed(&p).is_ok());
    }

    #[test]
    fn source_conflicts_and_relationship_are_validated() {
        let mut p = known_payload();
        p.master_relationship = Some(MasterRelationship {
            target: "bond01".into(),
            kind: MasterKind::Lag,
        });
        p.member_of = Some("bond01".into());
        p.source_conflicts = vec![SourceFieldConflict {
            field: "mtu".into(),
            effective_source: "vtysh_show_int".into(),
            effective_value: "9000".into(),
            alternate_source: "ip_addr_show".into(),
            alternate_value: "1500".into(),
            resolution: ConflictResolution::PrimaryWins,
        }];
        assert!(validate_typed(&p).is_ok());
        p.master_relationship = Some(MasterRelationship {
            target: "bad\nname".into(),
            kind: MasterKind::Unresolved,
        });
        assert!(validate_typed(&p).is_err());
    }
}
