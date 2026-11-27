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
    #[serde(default)]
    pub neighbor_scope_id: Option<String>,
    pub peer_as: Option<u32>,
    #[serde(default)]
    pub local_as: Option<u32>,
    #[serde(default)]
    pub peer_type: Option<String>,
    #[serde(default)]
    pub admin_state: Option<String>,
    #[serde(default)]
    pub peer_router_id: Option<String>,
    pub session_state: Option<String>,
    #[serde(default)]
    pub last_established: Option<String>,
    #[serde(default)]
    pub last_established_at: Option<String>,
    #[serde(default)]
    pub established_transitions: Option<u64>,
    #[serde(default)]
    pub advertised_capabilities: Vec<String>,
    #[serde(default)]
    pub received_capabilities: Vec<String>,
    #[serde(default)]
    pub received_afi_safi: Vec<String>,
    #[serde(default)]
    pub local_address: Option<String>,
    #[serde(default)]
    pub local_address_scope_id: Option<String>,
    #[serde(default)]
    pub family_admin_state: Option<String>,
    #[serde(default)]
    pub family_oper_state: Option<String>,
    pub received_routes: Option<u64>,
    pub active_routes: Option<u64>,
    #[serde(default)]
    pub sent_routes: Option<u64>,
    #[serde(default)]
    pub rejected_routes: Option<u64>,
    #[serde(default)]
    pub suppressed_routes: Option<u64>,
    #[serde(default)]
    pub withdrawn_error_routes: Option<u64>,
    #[serde(default)]
    pub negotiated_hold_time_seconds: Option<u64>,
    #[serde(default)]
    pub negotiated_keepalive_seconds: Option<u64>,
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

fn numeric_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
    })
}

fn nested_extras(obj: &Map<String, Value>, field: &str, known: &[&str], path: &str) -> Vec<String> {
    obj.get(field)
        .and_then(Value::as_object)
        .map(|nested| extras(nested, known, path))
        .unwrap_or_default()
}

fn optional_string(obj: &Map<String, Value>, field: &str) -> Option<String> {
    obj.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn string_list(value: Option<&Value>, path: &str, invalid: &mut Vec<String>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        invalid.push(path.into());
        return Vec::new();
    };
    if items.len() > 64
        || items
            .iter()
            .any(|item| item.as_str().is_none_or(str::is_empty))
    {
        invalid.push(path.into());
        return Vec::new();
    }
    items
        .iter()
        .map(|item| item.as_str().expect("validated string").to_string())
        .collect()
}

fn scoped_ip(value: Option<&Value>) -> Option<(String, Option<String>)> {
    let raw = value
        .and_then(Value::as_str)
        .filter(|raw| !raw.is_empty())?;
    let (address, zone) = match raw.split_once('%') {
        Some((address, zone)) => {
            if zone.is_empty()
                || zone.len() > 64
                || zone.contains('%')
                || zone.chars().any(char::is_whitespace)
            {
                return None;
            }
            (address, Some(zone.to_string()))
        }
        None => (raw, None),
    };
    let ip = address.parse::<std::net::IpAddr>().ok()?;
    if zone.is_some() && !ip.is_ipv6() {
        return None;
    }
    Some((ip.to_string(), zone))
}

fn sr_linux_timestamp(raw: Option<&str>) -> Option<String> {
    let first = raw?.split_whitespace().next()?;
    let parsed = chrono::DateTime::parse_from_rfc3339(first).ok()?;
    Some(parsed.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn optional_counter(
    family: Option<&Map<String, Value>>,
    field: &str,
    path: String,
    missing: &mut Vec<String>,
) -> Option<u64> {
    let value = family.and_then(|f| f.get(field));
    let parsed = numeric_u64(value);
    if value.is_some() && parsed.is_none() {
        missing.push(path);
    }
    parsed
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
        parser_version: "1.1.0".into(),
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
                    "local-as",
                    "peer-type",
                    "admin-state",
                    "peer-router-id",
                    "session-state",
                    "last-established",
                    "established-transitions",
                    "advertised-capabilities",
                    "received-capabilities",
                    "received-afi-safi",
                    "transport",
                    "timers",
                    "afi-safi",
                    "import-policy",
                    "export-policy",
                ],
                &npath,
            );
            let scoped_neighbor = scoped_ip(n.get("peer-address"));
            if scoped_neighbor.is_none() {
                missing.push(format!("{npath}/peer-address"));
            }
            let address = scoped_neighbor.as_ref().map(|(address, _)| address.clone());
            let neighbor_scope_id = scoped_neighbor.and_then(|(_, zone)| zone);
            if let Some(address) = &address {
                if !identities.insert((
                    vrf.to_string(),
                    address.clone(),
                    neighbor_scope_id.clone().unwrap_or_default(),
                )) {
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
            let local_as_value = n
                .get("local-as")
                .and_then(Value::as_object)
                .and_then(|local| local.get("as-number"));
            let local_as = numeric_u64(local_as_value)
                .and_then(|asn| u32::try_from(asn).ok())
                .filter(|asn| *asn > 0);
            if n.contains_key("local-as") && !n.get("local-as").is_some_and(Value::is_object) {
                missing.push(format!("{npath}/local-as"));
            }
            if local_as_value.is_some() && local_as.is_none() {
                missing.push(format!("{npath}/local-as/as-number"));
            }
            unknown.extend(nested_extras(
                n,
                "local-as",
                &["as-number"],
                &format!("{npath}/local-as"),
            ));
            let peer_type = optional_string(n, "peer-type");
            let admin_state = optional_string(n, "admin-state");
            let peer_router_id = n
                .get("peer-router-id")
                .and_then(Value::as_str)
                .filter(|value| value.parse::<std::net::Ipv4Addr>().is_ok())
                .map(str::to_string);
            if n.contains_key("peer-router-id") && peer_router_id.is_none() {
                missing.push(format!("{npath}/peer-router-id"));
            }
            let last_established = optional_string(n, "last-established");
            let last_established_at = sr_linux_timestamp(last_established.as_deref());
            if last_established.is_some() && last_established_at.is_none() {
                missing.push(format!("{npath}/last-established"));
            }
            let established_transitions = numeric_u64(n.get("established-transitions"));
            if n.contains_key("established-transitions") && established_transitions.is_none() {
                missing.push(format!("{npath}/established-transitions"));
            }
            let advertised_capabilities = string_list(
                n.get("advertised-capabilities"),
                &format!("{npath}/advertised-capabilities"),
                &mut missing,
            );
            let received_capabilities = string_list(
                n.get("received-capabilities"),
                &format!("{npath}/received-capabilities"),
                &mut missing,
            );
            let received_afi_safi = string_list(
                n.get("received-afi-safi"),
                &format!("{npath}/received-afi-safi"),
                &mut missing,
            );
            let transport = n.get("transport").and_then(Value::as_object);
            if n.contains_key("transport") && transport.is_none() {
                missing.push(format!("{npath}/transport"));
            }
            unknown.extend(nested_extras(
                n,
                "transport",
                &["local-address"],
                &format!("{npath}/transport"),
            ));
            let local_scoped = scoped_ip(transport.and_then(|t| t.get("local-address")));
            if transport.is_some_and(|t| t.contains_key("local-address")) && local_scoped.is_none()
            {
                missing.push(format!("{npath}/transport/local-address"));
            }
            let local_address = local_scoped.as_ref().map(|(address, _)| address.clone());
            let local_address_scope_id = local_scoped.and_then(|(_, zone)| zone);
            let timers = n.get("timers").and_then(Value::as_object);
            if n.contains_key("timers") && timers.is_none() {
                missing.push(format!("{npath}/timers"));
            }
            unknown.extend(nested_extras(
                n,
                "timers",
                &["negotiated-hold-time", "negotiated-keepalive-interval"],
                &format!("{npath}/timers"),
            ));
            let negotiated_hold_time_seconds =
                numeric_u64(timers.and_then(|t| t.get("negotiated-hold-time")))
                    .and_then(|seconds| u64::try_from(seconds).ok());
            let negotiated_keepalive_seconds =
                numeric_u64(timers.and_then(|t| t.get("negotiated-keepalive-interval")));
            if timers.is_some_and(|t| t.contains_key("negotiated-hold-time"))
                && negotiated_hold_time_seconds.is_none()
            {
                missing.push(format!("{npath}/timers/negotiated-hold-time"));
            }
            if timers.is_some_and(|t| t.contains_key("negotiated-keepalive-interval"))
                && negotiated_keepalive_seconds.is_none()
            {
                missing.push(format!("{npath}/timers/negotiated-keepalive-interval"));
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
            let family_path = format!("{npath}/afi-safi/{afi_safi}");
            let read_count =
                |field: &str| family.and_then(|f| f.get(field)).and_then(Value::as_u64);
            let received = read_count("received-routes");
            let active = read_count("active-routes");
            for (field, count) in [("received-routes", received), ("active-routes", active)] {
                if count.is_none() {
                    missing.push(format!("{family_path}/{field}"));
                }
            }
            let family_admin_state = family.and_then(|f| optional_string(f, "admin-state"));
            let family_oper_state = family.and_then(|f| optional_string(f, "oper-state"));
            let sent = optional_counter(
                family,
                "sent-routes",
                format!("{family_path}/sent-routes"),
                &mut missing,
            );
            let rejected = optional_counter(
                family,
                "rejected-routes",
                format!("{family_path}/rejected-routes"),
                &mut missing,
            );
            let suppressed = optional_counter(
                family,
                "suppressed-routes",
                format!("{family_path}/suppressed-routes"),
                &mut missing,
            );
            let withdrawn_error = optional_counter(
                family,
                "received-routes-withdrawn-due-to-error",
                format!("{family_path}/received-routes-withdrawn-due-to-error"),
                &mut missing,
            );
            if let Some(f) = family {
                unknown.extend(extras(
                    f,
                    &[
                        "afi-safi-name",
                        "admin-state",
                        "oper-state",
                        "received-routes",
                        "active-routes",
                        "sent-routes",
                        "rejected-routes",
                        "suppressed-routes",
                        "received-routes-withdrawn-due-to-error",
                    ],
                    &family_path,
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
                neighbor_scope_id,
                peer_as,
                local_as,
                peer_type,
                admin_state,
                peer_router_id,
                session_state: state,
                last_established,
                last_established_at,
                established_transitions,
                advertised_capabilities,
                received_capabilities,
                received_afi_safi,
                local_address,
                local_address_scope_id,
                family_admin_state,
                family_oper_state,
                received_routes: received,
                active_routes: active,
                sent_routes: sent,
                rejected_routes: rejected,
                suppressed_routes: suppressed,
                withdrawn_error_routes: withdrawn_error,
                negotiated_hold_time_seconds,
                negotiated_keepalive_seconds,
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
