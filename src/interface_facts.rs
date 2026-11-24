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

/// Operational (protocol/line) state. `LowerLayerDown` and `Testing` are
/// distinct from `Down`: they must not be collapsed into a boolean.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperState {
    Up,
    Down,
    LowerLayerDown,
    Testing,
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
    /// Port-channel/LAG this interface is a member of, if established.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_of: Option<String>,
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

fn valid_name(s: &str) -> bool {
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
/// Accepts `xx:xx:..`, `xx-xx-..`, `xxxx.xxxx.xxxx` (Cisco) and bare
/// 12-hex-digit forms. Returns `None` for anything else — callers must
/// surface `FieldAvailability::Invalid`, never a partial parse.
pub fn normalize_mac(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let hex: String = if trimmed.contains(':') || trimmed.contains('-') {
        trimmed.chars().filter(|c| *c != ':' && *c != '-').collect()
    } else if trimmed.contains('.') {
        // Cisco xxxx.xxxx.xxxx form.
        trimmed.chars().filter(|c| *c != '.').collect()
    } else {
        trimmed.to_string()
    };
    if hex.len() != 12 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
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
    for (field, name) in [
        (&p.ip_addresses, "ip_addresses"),
        (&p.ipv6_addresses, "ipv6_addresses"),
    ] {
        if let Some(f) = field {
            check_field(f, name, |addrs| {
                addrs.len() <= 64
                    && addrs.iter().all(|a| {
                        a.len() <= 64
                            && !a.chars().any(|c| c.is_control())
                            && a.contains('/')
                    })
            })?;
        }
    }
    Ok(())
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
    }

    #[test]
    fn scalar_fields_value_rejected() {
        // A bare scalar is not a valid interface.state payload.
        assert!(validate_payload(&json!(42)).is_err());
        assert!(validate_payload(&json!({"interface_name": "Eth0"})).is_err());
    }
}
