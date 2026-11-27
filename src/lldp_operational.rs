//! Shared operational LLDP projection helpers.
//!
//! These helpers only decide how an already-classified telemetry event is
//! decomposed into stable per-neighbor evidence records. Acquisition,
//! typed CLI normalization, reconciliation and graph mutation remain in
//! their existing owning modules.

use serde_json::Value as JsonValue;

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
}

pub struct OperationalLldpEntry<'a> {
    /// Stable record key within the local interface. Legacy single-neighbor
    /// paths retain the path neighbor key; multi-entry native detail blobs use
    /// protocol identity evidence instead of array position.
    pub record_key: String,
    pub value: &'a JsonValue,
    /// True when the input carried an explicit multi-entry envelope.
    pub multi_entry_envelope: bool,
}

fn bounded_string(value: Option<&JsonValue>) -> Option<&str> {
    value
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 255)
}

fn json_field<'a>(value: &'a JsonValue, field: &str) -> Option<&'a str> {
    bounded_string(value.get(field))
}

fn discriminator(value: &JsonValue) -> Option<String> {
    let chassis = json_field(value, "chassis-id");
    let port = json_field(value, "port-id");
    match (chassis, port) {
        (Some(chassis), Some(port)) => Some(format!("{chassis}|{port}")),
        // Chassis and port are both protocol-mandatory identity. A partial or
        // system-name-only multi-entry blob is malformed evidence, not a
        // stable positional neighbor.
        _ => None,
    }
}

/// Decompose one classified LLDP event into persistable neighbor evidence.
/// Entries missing protocol-mandatory identity are not persisted under an
/// array-index-derived identity: positional identity would make lifecycle and
/// replacement semantics depend on source ordering rather than evidence.
pub fn projection_entries<'a>(
    path_neighbor_id: &str,
    value: &'a JsonValue,
) -> Vec<OperationalLldpEntry<'a>> {
    if let Some(entries) = value.get("neighbors").and_then(JsonValue::as_array) {
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries.iter() {
            let Some(discriminator) = discriminator(entry) else {
                continue;
            };
            out.push(OperationalLldpEntry {
                record_key: format!("{path_neighbor_id}#{discriminator}"),
                value: entry,
                multi_entry_envelope: true,
            });
        }
        return out;
    }
    vec![OperationalLldpEntry {
        record_key: path_neighbor_id.to_string(),
        value,
        multi_entry_envelope: false,
    }]
}

#[cfg(test)]
mod tests {
    use super::projection_entries;

    #[test]
    fn multi_neighbor_entries_use_protocol_identity_not_array_index() {
        let value = serde_json::json!({
            "neighbors": [
                {"chassis-id":"aa:01","port-id":"Gi0/1","system-name":"leaf1"},
                {"chassis-id":"aa:02","port-id":"Gi0/2","system-name":"leaf2"}
            ]
        });
        let entries = projection_entries("leaf1", &value);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].record_key.contains("aa:01|Gi0/1"));
        assert!(entries[1].record_key.contains("aa:02|Gi0/2"));
        assert!(!entries[0].record_key.contains("#0"));
    }

    #[test]
    fn missing_mandatory_identity_is_not_positionally_persisted() {
        let value = serde_json::json!({"neighbors":[{"system-name":"leaf1"}]});
        assert!(projection_entries("leaf1", &value).is_empty());
    }

    #[test]
    fn legacy_single_neighbor_paths_keep_existing_record_key() {
        let value = serde_json::json!({"chassis-id":"aa:01","port-id":"Gi0/1"});
        let entries = projection_entries("neighbor-key", &value);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].record_key, "neighbor-key");
        assert!(!entries[0].multi_entry_envelope);
    }
}
