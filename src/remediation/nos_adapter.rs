//! Native SR Linux CLI-state projection. This is not BMP or FIB evidence.
use super::approval::ParseQualityState;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;
const MAX_NEIGHBORS: usize = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeNosBgpNeighbor {
    pub vrf_name: String,
    pub afi_safi: String,
    pub neighbor_address: Option<String>,
    pub peer_as: Option<u32>,
    pub session_state: Option<String>,
    pub received_routes: Option<u64>,
    pub active_routes: Option<u64>,
    pub import_policies: Vec<String>,
    pub export_policies: Vec<String>,
    pub parse_quality: ParseQualityState,
    pub missing_or_invalid_fields: Vec<String>,
    pub unparsed_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeNosBgpCapture {
    pub exporter_address: String,
    pub nos_version: String,
    pub afi_safi: String,
    pub parser_id: String,
    pub parser_version: String,
    pub raw_sample_sha256: String,
    pub parse_quality: ParseQualityState,
    pub missing_or_invalid_fields: Vec<String>,
    pub unparsed_fields: Vec<String>,
    pub neighbors: Vec<NativeNosBgpNeighbor>,
}

fn quality(missing: &[String], unknown: &[String]) -> ParseQualityState {
    if !missing.is_empty() {
        ParseQualityState::SchemaMismatch
    } else if !unknown.is_empty() {
        ParseQualityState::PartialUnknownFields
    } else {
        ParseQualityState::Complete
    }
}

fn extras(obj: &Map<String, Value>, known: &[&str], prefix: &str) -> Vec<String> {
    obj.keys()
        .filter(|key| !known.contains(&key.as_str()))
        .map(|key| format!("{prefix}/{key}"))
        .collect()
}

fn policies(value: Option<&Value>, path: &str, missing: &mut Vec<String>) -> Vec<String> {
    match value {
        None => Vec::new(),
        Some(Value::Array(values))
            if values
                .iter()
                .all(|v| v.as_str().is_some_and(|s| !s.is_empty())) =>
        {
            values
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        }
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => {
            missing.push(path.into());
            Vec::new()
        }
    }
}

/// Parses exact `info from state with-context network-instance ... protocols bgp neighbor *`
/// output. Caller supplies independently captured NOS version and exporter identity.
/// Counters are projected for one explicitly requested family, never summed across families.
pub fn parse_nokia_srl_native_bgp_neighbors(
    raw_json: &str,
    exporter_address: &str,
    nos_version: &str,
    afi_safi: &str,
) -> Result<NativeNosBgpCapture, String> {
    if raw_json.len() > MAX_CAPTURE_BYTES {
        return Err("NOS capture exceeds 4 MiB".into());
    }
    if exporter_address.parse::<std::net::IpAddr>().is_err() {
        return Err("native NOS capture requires an explicit exporter IP address".into());
    }
    if nos_version.trim().is_empty() || afi_safi.trim().is_empty() {
        return Err("native NOS capture requires version and AFI/SAFI context".into());
    }
    let value: Value =
        serde_json::from_str(raw_json).map_err(|e| format!("invalid native NOS JSON: {e}"))?;
    let root = value
        .as_object()
        .ok_or("native NOS capture must be an object")?;
    let mut capture = NativeNosBgpCapture {
        exporter_address: exporter_address.into(),
        nos_version: nos_version.into(),
        afi_safi: afi_safi.into(),
        parser_id: "nokia-srl-native-bgp-neighbor".into(),
        parser_version: "1.0.0".into(),
        raw_sample_sha256: hex::encode(Sha256::digest(raw_json.as_bytes())),
        parse_quality: ParseQualityState::Complete,
        missing_or_invalid_fields: Vec::new(),
        unparsed_fields: extras(root, &["network-instance"], ""),
        neighbors: Vec::new(),
    };
    if nos_version == "unknown" {
        capture.missing_or_invalid_fields.push("nos_version".into());
    }
    let Some(instances) = root.get("network-instance").and_then(Value::as_array) else {
        capture
            .missing_or_invalid_fields
            .push("/network-instance".into());
        capture.parse_quality = ParseQualityState::SchemaMismatch;
        return Ok(capture);
    };
    if instances.len() > MAX_NEIGHBORS {
        return Err("native NOS capture exceeds 10000 network instances".into());
    }
    let mut neighbor_count = 0usize;
    let mut identities = std::collections::HashSet::new();
    for (i, instance) in instances.iter().enumerate() {
        let path = format!("/network-instance/{i}");
        let Some(obj) = instance.as_object() else {
            capture.missing_or_invalid_fields.push(path);
            continue;
        };
        capture
            .unparsed_fields
            .extend(extras(obj, &["name", "protocols"], &path));
        let Some(vrf) = obj
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            capture
                .missing_or_invalid_fields
                .push(format!("{path}/name"));
            continue;
        };
        let Some(protocols) = obj.get("protocols").and_then(Value::as_object) else {
            capture
                .missing_or_invalid_fields
                .push(format!("{path}/protocols"));
            continue;
        };
        capture
            .unparsed_fields
            .extend(extras(protocols, &["bgp"], &format!("{path}/protocols")));
        let Some(bgp) = protocols.get("bgp").and_then(Value::as_object) else {
            capture
                .missing_or_invalid_fields
                .push(format!("{path}/protocols/bgp"));
            continue;
        };
        capture.unparsed_fields.extend(extras(
            bgp,
            &["neighbor"],
            &format!("{path}/protocols/bgp"),
        ));
        let Some(neighbors) = bgp.get("neighbor").and_then(Value::as_array) else {
            capture
                .missing_or_invalid_fields
                .push(format!("{path}/protocols/bgp/neighbor"));
            continue;
        };
        neighbor_count = neighbor_count
            .checked_add(neighbors.len())
            .ok_or("native NOS neighbor count overflow")?;
        if neighbor_count > MAX_NEIGHBORS {
            return Err("native NOS capture exceeds 10000 neighbors".into());
        }
        for (j, neighbor) in neighbors.iter().enumerate() {
            if capture.neighbors.len() >= MAX_NEIGHBORS {
                return Err("native NOS capture exceeds 10000 neighbors".into());
            }
            let npath = format!("{path}/protocols/bgp/neighbor/{j}");
            let Some(n) = neighbor.as_object() else {
                capture.missing_or_invalid_fields.push(npath);
                continue;
            };
            let mut missing = Vec::new();
            let mut unknown = extras(
                n,
                &[
                    "peer-address",
                    "peer-as",
                    "session-state",
                    "afi-safi",
                    "import-policy",
                    "export-policy",
                ],
                &npath,
            );
            let address = n
                .get("peer-address")
                .and_then(Value::as_str)
                .filter(|s| s.parse::<std::net::IpAddr>().is_ok())
                .map(str::to_string);
            if address.is_none() {
                missing.push(format!("{npath}/peer-address"));
            }
            if let Some(address) = &address {
                if !identities.insert((vrf.to_string(), address.clone())) {
                    return Err(format!(
                        "duplicate native NOS neighbor identity in VRF {vrf}"
                    ));
                }
            }
            let peer_as = n
                .get("peer-as")
                .and_then(Value::as_u64)
                .and_then(|asn| u32::try_from(asn).ok())
                .filter(|asn| *asn > 0);
            if peer_as.is_none() {
                missing.push(format!("{npath}/peer-as"));
            }
            let state = n
                .get("session-state")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            if state.is_none() {
                missing.push(format!("{npath}/session-state"));
            }
            let families: Vec<_> = n
                .get("afi-safi")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|f| f.get("afi-safi-name").and_then(Value::as_str) == Some(afi_safi))
                .collect();
            let family = if families.len() == 1 {
                families[0].as_object()
            } else {
                None
            };
            if family.is_none() {
                missing.push(format!("{npath}/afi-safi/{afi_safi}"));
            }
            let read_count =
                |field: &str| family.and_then(|f| f.get(field)).and_then(Value::as_u64);
            let received = read_count("received-routes");
            let active = read_count("active-routes");
            for (field, count) in [("received-routes", received), ("active-routes", active)] {
                if count.is_none() {
                    missing.push(format!("{npath}/afi-safi/{afi_safi}/{field}"));
                }
            }
            if let Some(f) = family {
                unknown.extend(extras(
                    f,
                    &["afi-safi-name", "received-routes", "active-routes"],
                    &format!("{npath}/afi-safi/{afi_safi}"),
                ));
            }
            let import = policies(
                n.get("import-policy"),
                &format!("{npath}/import-policy"),
                &mut missing,
            );
            let export = policies(
                n.get("export-policy"),
                &format!("{npath}/export-policy"),
                &mut missing,
            );
            capture
                .missing_or_invalid_fields
                .extend(missing.iter().cloned());
            capture.unparsed_fields.extend(unknown.iter().cloned());
            capture.neighbors.push(NativeNosBgpNeighbor {
                vrf_name: vrf.into(),
                afi_safi: afi_safi.into(),
                neighbor_address: address,
                peer_as,
                session_state: state,
                received_routes: received,
                active_routes: active,
                import_policies: import,
                export_policies: export,
                parse_quality: quality(&missing, &unknown),
                missing_or_invalid_fields: missing,
                unparsed_fields: unknown,
            });
        }
    }
    capture.missing_or_invalid_fields.sort();
    capture.unparsed_fields.sort();
    capture.parse_quality = quality(&capture.missing_or_invalid_fields, &capture.unparsed_fields);
    Ok(capture)
}
