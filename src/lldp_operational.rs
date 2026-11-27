//! Shared operational LLDP projection helpers.
//!
//! These helpers decide how an already-classified telemetry event is
//! decomposed into stable per-neighbor evidence records and how lifecycle
//! events are ordered. Acquisition, typed CLI normalization, reconciliation
//! and graph mutation remain in their existing owning modules.

use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const MAX_OPERATIONAL_NEIGHBORS_PER_EVENT: usize = 64;

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct OperationalLldpNeighbor {
    pub id: String,
    pub local_if: String,
    pub neighbor_id: String,
    pub neighbor_key: String,
    pub chassis_id: String,
    pub system_name: String,
    pub port_id: String,
    pub lifecycle: String,
    pub updated_at_ns: i64,
    pub observed_at_ns: i64,
    pub lifecycle_changed_at_ns: i64,
    pub ordering_state: String,
    pub ordering_detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OperationalLldpPopulationAccounting {
    pub entries_encountered: usize,
    pub entries_accepted: usize,
    pub exact_duplicates: usize,
    pub conflicting_entries: usize,
    pub malformed_entries: usize,
    pub entries_omitted: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationalLldpPopulation<'a> {
    pub entries: Vec<OperationalLldpEntry<'a>>,
    pub accounting: OperationalLldpPopulationAccounting,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationalLldpEntry<'a> {
    /// Stable record key within the local interface. Legacy single-neighbor
    /// paths retain the path neighbor key; multi-entry native detail blobs use
    /// protocol identity evidence instead of array position.
    pub record_key: String,
    /// Domain-separated protocol identity digest used as part of persistent
    /// identity. The digest input carries explicit unknown subtype markers;
    /// it never treats an unknown subtype as a positive interface-name type.
    pub protocol_identity: String,
    pub value: &'a JsonValue,
    pub multi_entry_envelope: bool,
    pub chassis_id_subtype: String,
    pub port_id_subtype: String,
    pub identity_uncertainty: Option<String>,
}

fn bounded_string(value: Option<&JsonValue>) -> Option<&str> {
    value
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 255)
}

fn json_field<'a>(value: &'a JsonValue, field: &str) -> Option<&'a str> {
    bounded_string(value.get(field))
}

fn json_field_any<'a>(value: &'a JsonValue, fields: &[&str]) -> Option<&'a str> {
    fields.iter().find_map(|field| json_field(value, field))
}

fn push_len_prefixed_raw(buf: &mut Vec<u8>, domain: &str, value: &str) {
    buf.extend_from_slice(&(domain.len() as u32).to_le_bytes());
    buf.extend_from_slice(domain.as_bytes());
    buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
    buf.extend_from_slice(value.as_bytes());
}

fn digest_len_prefixed(fields: &[(&str, &str)]) -> String {
    let mut buf = Vec::new();
    for (domain, value) in fields {
        push_len_prefixed_raw(&mut buf, domain, value);
    }
    hex::encode(Sha256::digest(&buf))
}

/// Domain-separated persistent node identity for one operational LLDP record.
/// `device_address` is the existing inventory/transport key, not a claim of
/// canonical inventory identity. The unknown axes are explicit inputs to the
/// digest rather than delimiter-joined strings.
pub fn operational_lldp_node_id(
    device_address: &str,
    identity_scope: Option<&str>,
    network_namespace: Option<&str>,
    local_if: &str,
    record_key: &str,
) -> String {
    let digest = digest_len_prefixed(&[
        ("format", "bonsai-operational-lldp-node-v1"),
        ("device_address", device_address),
        ("identity_scope", identity_scope.unwrap_or("")),
        ("network_namespace", network_namespace.unwrap_or("")),
        ("local_interface", local_if),
        ("source_record_key", record_key),
    ]);
    format!("lldp-op:v1:{digest}")
}

fn protocol_identity(
    chassis: &str,
    chassis_subtype: Option<&str>,
    port: &str,
    port_subtype: Option<&str>,
) -> String {
    digest_len_prefixed(&[
        ("format", "bonsai-operational-lldp-neighbor-v1"),
        ("chassis_subtype", chassis_subtype.unwrap_or("unknown")),
        ("chassis_id", chassis),
        ("port_subtype", port_subtype.unwrap_or("unknown")),
        ("port_id", port),
    ])
}

pub fn lldp_port_subtype(value: &JsonValue) -> Option<&str> {
    json_field_any(
        value,
        &[
            "port-id-type",
            "port-id-subtype",
            "port_id_subtype",
            "interface-id-type",
            "interfaceIdType",
        ],
    )
}

pub fn lldp_chassis_subtype(value: &JsonValue) -> Option<&str> {
    json_field_any(
        value,
        &[
            "chassis-id-type",
            "chassis-id-subtype",
            "chassis_id_subtype",
            "chassisIdType",
        ],
    )
}

pub fn port_subtype_is_interface_name(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "interface_name" | "interface-name" | "interfacename" | "ifname" | "if_name"
    )
}

fn discriminator(value: &JsonValue) -> Option<(String, String, String, String)> {
    let chassis = json_field(value, "chassis-id")?;
    let port = json_field(value, "port-id")?;
    let chassis_subtype = lldp_chassis_subtype(value).unwrap_or("unknown");
    let port_subtype = lldp_port_subtype(value).unwrap_or("unknown");
    Some((
        chassis.to_string(),
        chassis_subtype.to_string(),
        port.to_string(),
        port_subtype.to_string(),
    ))
}

/// Decompose one classified LLDP event into persistable neighbor evidence.
/// Exact replay is deduplicated; contradictory entries with the same semantic
/// key are counted and none of that conflicting group is accepted. Array order
/// is never an identity axis.
pub fn decompose_projection_entries<'a>(
    path_neighbor_id: &str,
    value: &'a JsonValue,
) -> OperationalLldpPopulation<'a> {
    let mut accounting = OperationalLldpPopulationAccounting::default();
    if let Some(entries) = value.get("neighbors").and_then(JsonValue::as_array) {
        accounting.entries_encountered = entries.len();
        let mut groups: BTreeMap<String, Vec<(&JsonValue, String, String)>> = BTreeMap::new();
        for entry in entries.iter() {
            let Some((chassis, chassis_subtype, port, port_subtype)) = discriminator(entry) else {
                accounting.malformed_entries += 1;
                continue;
            };
            let protocol =
                protocol_identity(&chassis, Some(&chassis_subtype), &port, Some(&port_subtype));
            let record_key = format!("{}:proto:{}", path_neighbor_id, &protocol[..32]);
            groups
                .entry(record_key)
                .or_default()
                .push((entry, protocol, chassis_subtype));
        }
        let mut accepted = Vec::new();
        for (record_key, group) in groups {
            let first = group[0].0;
            if group.iter().all(|(value, _, _)| *value == first) {
                accounting.exact_duplicates += group.len().saturating_sub(1);
                let (_, protocol, chassis_subtype) = &group[0];
                accepted.push((record_key, chassis_subtype.clone(), first, protocol.clone()));
            } else {
                accounting.conflicting_entries += group.len();
            }
        }
        let omitted = accepted
            .len()
            .saturating_sub(MAX_OPERATIONAL_NEIGHBORS_PER_EVENT);
        accounting.entries_omitted = omitted;
        accounting.entries_accepted = accepted.len().saturating_sub(omitted);
        let entries =
            accepted
                .into_iter()
                .take(MAX_OPERATIONAL_NEIGHBORS_PER_EVENT)
                .map(|(record_key, chassis_subtype, value, protocol)| {
                    OperationalLldpEntry {
                record_key,
                protocol_identity: protocol,
                value,
                multi_entry_envelope: true,
                chassis_id_subtype: chassis_subtype,
                port_id_subtype: lldp_port_subtype(value).unwrap_or("unknown").to_string(),
                identity_uncertainty: (lldp_chassis_subtype(value).is_none()
                    || lldp_port_subtype(value).is_none())
                .then(|| {
                    "model-driven extraction does not positively establish LLDP identity subtypes"
                        .to_string()
                }),
            }
                })
                .collect();
        return OperationalLldpPopulation {
            entries,
            accounting,
        };
    }

    accounting.entries_encountered = 1;
    accounting.entries_accepted = 1;
    let chassis_subtype = lldp_chassis_subtype(value).unwrap_or("unknown").to_string();
    let port_subtype = lldp_port_subtype(value).unwrap_or("unknown").to_string();
    let protocol = match (
        json_field(value, "chassis-id"),
        json_field(value, "port-id"),
    ) {
        (Some(chassis), Some(port)) => protocol_identity(
            chassis,
            lldp_chassis_subtype(value),
            port,
            lldp_port_subtype(value),
        ),
        _ => digest_len_prefixed(&[
            ("format", "bonsai-operational-lldp-neighbor-v1"),
            ("identity_state", "partial-source-occurrence"),
            ("source_neighbor_id", path_neighbor_id),
        ]),
    };
    OperationalLldpPopulation {
        entries: vec![OperationalLldpEntry {
            record_key: path_neighbor_id.to_string(),
            protocol_identity: protocol,
            value,
            multi_entry_envelope: false,
            chassis_id_subtype: chassis_subtype,
            port_id_subtype: port_subtype,
            identity_uncertainty: (lldp_chassis_subtype(value).is_none()
                || lldp_port_subtype(value).is_none())
            .then(|| {
                "model-driven extraction does not positively establish LLDP identity subtypes"
                    .to_string()
            }),
        }],
        accounting,
    }
}

/// Backwards-compatible entry-only view used by older callers. New writers
/// must use [`decompose_projection_entries`] so malformed/conflicting/omitted
/// population evidence is not silently lost.
pub fn projection_entries<'a>(
    path_neighbor_id: &str,
    value: &'a JsonValue,
) -> Vec<OperationalLldpEntry<'a>> {
    decompose_projection_entries(path_neighbor_id, value).entries
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LldpDeletionScope {
    NotDelete,
    NeighborOccurrence,
    OptionalField,
    InterfaceNeighborList,
    Collection,
    PartialNotification,
    Unknown,
}

pub fn deletion_scope(path: &str, is_delete: bool) -> LldpDeletionScope {
    if !is_delete {
        return LldpDeletionScope::NotDelete;
    }
    let keyed_neighbor = (path.contains("/neighbor[id=")
        || path.contains("/neighbors/neighbor[id=")
        || path.contains("/detail[interface-name="))
        && path.contains(']');
    if keyed_neighbor {
        if path.ends_with(']') {
            return LldpDeletionScope::NeighborOccurrence;
        }
        if path.ends_with("/state") {
            return LldpDeletionScope::Unknown;
        }
        return LldpDeletionScope::OptionalField;
    }
    if path.ends_with("/neighbors") || path.ends_with("/neighbor") || path.ends_with("/details") {
        return LldpDeletionScope::InterfaceNeighborList;
    }
    if path.ends_with("lldp") || path.ends_with("/lldp") {
        return LldpDeletionScope::Collection;
    }
    if path.ends_with("/state") || path.ends_with("/details") {
        return LldpDeletionScope::PartialNotification;
    }
    LldpDeletionScope::Unknown
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleEventKind {
    Observation,
    Withdrawal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleOrderDecision {
    Apply,
    CompatibleReplay,
    ExactReplay,
    RejectStale,
    EqualTimeConflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExistingLifecycleOrder {
    pub lifecycle_active: bool,
    pub lifecycle_withdrawn: bool,
    pub lifecycle_superseded: bool,
    pub latest_observation_ns: i64,
    pub lifecycle_changed_at_ns: i64,
}

pub fn decide_lifecycle_order(
    existing: ExistingLifecycleOrder,
    incoming_ns: i64,
    kind: LifecycleEventKind,
    compatible_same_time_observation: bool,
) -> LifecycleOrderDecision {
    if existing.lifecycle_superseded {
        return LifecycleOrderDecision::RejectStale;
    }
    let reference = existing
        .latest_observation_ns
        .max(existing.lifecycle_changed_at_ns);
    if reference > 0 && incoming_ns > reference {
        return LifecycleOrderDecision::Apply;
    }
    if reference > 0 && incoming_ns < reference {
        return LifecycleOrderDecision::RejectStale;
    }
    if incoming_ns <= 0 {
        return LifecycleOrderDecision::EqualTimeConflict;
    }
    match kind {
        LifecycleEventKind::Observation => {
            if existing.lifecycle_active {
                if compatible_same_time_observation {
                    LifecycleOrderDecision::CompatibleReplay
                } else {
                    LifecycleOrderDecision::EqualTimeConflict
                }
            } else {
                LifecycleOrderDecision::EqualTimeConflict
            }
        }
        LifecycleEventKind::Withdrawal => {
            if existing.lifecycle_withdrawn {
                LifecycleOrderDecision::ExactReplay
            } else {
                LifecycleOrderDecision::EqualTimeConflict
            }
        }
    }
}

/// True when two partial observations have no contradictory overlapping
/// identity/value field. Missing fields are not negative evidence in the
/// model-driven leaf-grouping contract.
pub fn compatible_partial_observation(incoming: &JsonValue, existing: &JsonValue) -> bool {
    for field in ["chassis-id", "system-name", "port-id"] {
        let Some(incoming_value) = incoming.get(field).and_then(JsonValue::as_str) else {
            continue;
        };
        if incoming_value.is_empty() {
            continue;
        }
        if let Some(existing_value) = existing.get(field).and_then(JsonValue::as_str)
            && !existing_value.is_empty()
            && existing_value != incoming_value
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{
        ExistingLifecycleOrder, LifecycleEventKind, LifecycleOrderDecision, LldpDeletionScope,
        decide_lifecycle_order, decompose_projection_entries, deletion_scope,
        operational_lldp_node_id,
    };

    #[test]
    fn multi_neighbor_entries_use_protocol_identity_not_array_index() {
        let value = serde_json::json!({
            "neighbors": [
                {"chassis-id":"aa:01","port-id":"Gi0/1","system-name":"leaf1"},
                {"chassis-id":"aa:02","port-id":"Gi0/2","system-name":"leaf2"}
            ]
        });
        let population = decompose_projection_entries("leaf1", &value);
        assert_eq!(population.entries.len(), 2);
        assert!(
            population
                .entries
                .iter()
                .all(|entry| !entry.record_key.contains("#0"))
        );
        assert_ne!(
            population.entries[0].protocol_identity,
            population.entries[1].protocol_identity
        );
        assert_eq!(population.accounting.entries_accepted, 2);
    }

    #[test]
    fn missing_mandatory_identity_is_counted_not_positionally_persisted() {
        let value = serde_json::json!({"neighbors":[{"system-name":"leaf1"}]});
        let population = decompose_projection_entries("leaf1", &value);
        assert!(population.entries.is_empty());
        assert_eq!(population.accounting.malformed_entries, 1);
    }

    #[test]
    fn exact_replay_dedupes_and_conflicting_entries_are_not_overwritten() {
        let value = serde_json::json!({"neighbors":[
            {"chassis-id":"aa:01","port-id":"Gi0/1","system-name":"leaf1"},
            {"chassis-id":"aa:01","port-id":"Gi0/1","system-name":"leaf1"},
            {"chassis-id":"aa:02","port-id":"Gi0/2","system-name":"leaf2a"},
            {"chassis-id":"aa:02","port-id":"Gi0/2","system-name":"leaf2b"}
        ]});
        let population = decompose_projection_entries("path", &value);
        assert_eq!(population.entries.len(), 1);
        assert_eq!(population.accounting.exact_duplicates, 1);
        assert_eq!(population.accounting.conflicting_entries, 2);
    }

    #[test]
    fn concentrated_input_is_bounded_and_counted() {
        let neighbors: Vec<_> = (0..80)
            .map(|i| serde_json::json!({"chassis-id":format!("chassis-{i}"),"port-id":format!("eth{i}")}))
            .collect();
        let envelope = serde_json::json!({"neighbors":neighbors});
        let population = decompose_projection_entries("path", &envelope);
        assert_eq!(
            population.entries.len(),
            super::MAX_OPERATIONAL_NEIGHBORS_PER_EVENT
        );
        assert_eq!(population.accounting.entries_encountered, 80);
        assert_eq!(population.accounting.entries_omitted, 16);
    }

    #[test]
    fn legacy_single_neighbor_paths_keep_existing_record_key() {
        let value = serde_json::json!({"chassis-id":"aa:01","port-id":"Gi0/1"});
        let entries = super::projection_entries("neighbor-key", &value);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].record_key, "neighbor-key");
        assert!(!entries[0].multi_entry_envelope);
    }

    #[test]
    fn node_identity_is_domain_separated_and_delimiter_safe() {
        let a = operational_lldp_node_id("dev", None, None, "ab:c", "x");
        let b = operational_lldp_node_id("dev", None, None, "ab", "c:x");
        assert_ne!(a, b);
        assert!(a.starts_with("lldp-op:v1:"));
    }

    #[test]
    fn leaf_delete_is_not_neighbor_occurrence_delete() {
        assert_eq!(
            deletion_scope(
                "lldp/interfaces/interface[name=eth1]/neighbors/neighbor[id=x]/state/chassis-id",
                true
            ),
            LldpDeletionScope::OptionalField
        );
        assert_eq!(
            deletion_scope(
                "lldp/interfaces/interface[name=eth1]/neighbors/neighbor[id=x]",
                true
            ),
            LldpDeletionScope::NeighborOccurrence
        );
    }

    #[test]
    fn stale_equal_and_replay_ordering_is_explicit() {
        let existing = ExistingLifecycleOrder {
            lifecycle_active: false,
            lifecycle_withdrawn: true,
            lifecycle_superseded: false,
            latest_observation_ns: 100,
            lifecycle_changed_at_ns: 200,
        };
        assert_eq!(
            decide_lifecycle_order(existing, 150, LifecycleEventKind::Observation, true),
            LifecycleOrderDecision::RejectStale
        );
        assert_eq!(
            decide_lifecycle_order(existing, 200, LifecycleEventKind::Observation, true),
            LifecycleOrderDecision::EqualTimeConflict
        );
        assert_eq!(
            decide_lifecycle_order(existing, 200, LifecycleEventKind::Withdrawal, true),
            LifecycleOrderDecision::ExactReplay
        );
        assert_eq!(
            decide_lifecycle_order(existing, 250, LifecycleEventKind::Observation, true),
            LifecycleOrderDecision::Apply
        );
    }
}
