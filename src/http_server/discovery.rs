#![allow(unused_imports, dead_code, unused_variables)]
use super::*;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn compute_health(bgp: &[BgpJson]) -> String {
    if bgp.is_empty() {
        return "healthy".into();
    }
    let established = bgp.iter().filter(|n| n.state == "established").count();
    if established == bgp.len() {
        "healthy".into()
    } else if established > 0 {
        "warn".into()
    } else {
        "critical".into()
    }
}

async fn read_subscription_statuses(
    db: Arc<lbug::Database>,
) -> Result<HashMap<String, Vec<SubscriptionStatusJson>>, (StatusCode, String)> {
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let rows = conn
            .query(
                "MATCH (s:SubscriptionStatus) \
                 RETURN s.device_address, s.path, s.origin, s.mode, s.sample_interval_ns, \
                        s.status, s.first_observed_at, s.last_observed_at, s.updated_at \
                 ORDER BY s.device_address, s.path",
            )
            .map_err(|e| e.to_string())?;

        let mut by_device: HashMap<String, Vec<SubscriptionStatusJson>> = HashMap::new();
        for row in rows {
            by_device
                .entry(read_str(&row[0]))
                .or_default()
                .push(SubscriptionStatusJson {
                    path: read_str(&row[1]),
                    origin: read_str(&row[2]),
                    mode: read_str(&row[3]),
                    sample_interval_ns: read_i64(&row[4]),
                    status: read_str(&row[5]),
                    first_observed_at_ns: read_ts_ns(&row[6]),
                    last_observed_at_ns: read_ts_ns(&row[7]),
                    updated_at_ns: read_ts_ns(&row[8]),
                });
        }

        Ok::<_, String>(by_device)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn read_trust_mark_impact(
    db: Arc<lbug::Database>,
    address: String,
) -> Result<(usize, usize), (StatusCode, String)> {
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "MATCH (m:RemediationTrustMark)-[:TRUST_MARKS]->(r:Remediation)-[:RESOLVES]->(e:DetectionEvent) \
                 WHERE e.device_address = $addr \
                 RETURN m.trustworthy",
            )
            .map_err(|e| e.to_string())?;
        let rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(address))])
            .map_err(|e| e.to_string())?;

        let mut total = 0usize;
        let mut active = 0usize;
        for row in rows {
            total += 1;
            if read_i64(&row[0]) == 1 {
                active += 1;
            }
        }
        Ok::<_, String>((total, active))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

fn managed_device_json(
    target: TargetConfig,
    statuses: &HashMap<String, Vec<SubscriptionStatusJson>>,
    overrides: &[crate::registry::PathOverride],
) -> ManagedDeviceJson {
    let (_, audit) = crate::discovery::resolve_subscription_paths(&target, overrides);
    let address = target.address;
    ManagedDeviceJson {
        enabled: target.enabled,
        collector_id: target.collector_id.unwrap_or_default(),
        tls_domain: target.tls_domain.unwrap_or_default(),
        ca_cert: target.ca_cert.unwrap_or_default(),
        vendor: target.vendor.unwrap_or_default(),
        credential_alias: target.credential_alias.unwrap_or_default(),
        username_env: target.username_env.unwrap_or_default(),
        password_env: target.password_env.unwrap_or_default(),
        hostname: target.hostname.unwrap_or_default(),
        role: target.role.unwrap_or_default(),
        site: target.site.unwrap_or_default(),
        selected_paths: target.selected_paths,
        subscription_statuses: statuses.get(&address).cloned().unwrap_or_default(),
        address,
        resolution_audit: audit,
    }
}

fn target_from_request(req: ManagedDeviceRequest) -> Result<TargetConfig, (StatusCode, String)> {
    if req.address.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "device address is required".to_string(),
        ));
    }
    if !req.username_env.trim().is_empty() && std::env::var(req.username_env.trim()).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("username env var '{}' is not set", req.username_env.trim()),
        ));
    }
    if !req.password_env.trim().is_empty() && std::env::var(req.password_env.trim()).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("password env var '{}' is not set", req.password_env.trim()),
        ));
    }

    Ok(TargetConfig {
        address: req.address.trim().to_string(),
        enabled: req.enabled,
        tls_domain: option_string(req.tls_domain),
        ca_cert: option_string(req.ca_cert),
        vendor: option_string(req.vendor),
        credential_alias: option_string(req.credential_alias),
        username_env: option_string(req.username_env),
        password_env: option_string(req.password_env),
        username: None,
        password: None,
        hostname: option_string(req.hostname),
        role: option_string(req.role),
        site: option_string(req.site),
        collector_id: None,
        selected_paths: req
            .selected_paths
            .into_iter()
            .filter(|path| !path.path.trim().is_empty())
            .collect(),
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
        last_operator_action: String::new(),
    })
}

fn site_json(site: SiteRecord) -> SiteJson {
    SiteJson {
        id: site.id,
        name: site.name,
        parent_id: site.parent_id,
        kind: site.kind,
        lat: site.lat,
        lon: site.lon,
        metadata_json: site.metadata_json,
        environment_id: site.environment_id,
    }
}

fn site_record(site: SiteJson) -> SiteRecord {
    SiteRecord {
        id: site.id,
        name: site.name,
        parent_id: site.parent_id,
        kind: site.kind,
        lat: site.lat,
        lon: site.lon,
        metadata_json: site.metadata_json,
        environment_id: site.environment_id,
    }
}

fn credential_json(
    credential: CredentialSummary,
    device_counts: &HashMap<String, usize>,
) -> CredentialJson {
    CredentialJson {
        device_count: device_counts
            .get(&credential.alias)
            .copied()
            .unwrap_or_default(),
        alias: credential.alias,
        created_at_ns: credential.created_at_ns,
        updated_at_ns: credential.updated_at_ns,
        last_used_at_ns: credential.last_used_at_ns,
    }
}

fn credential_device_counts(registry: &ApiRegistry) -> anyhow::Result<HashMap<String, usize>> {
    let mut counts = HashMap::new();
    for target in registry.list_all_targets()? {
        if let Some(alias) = target.credential_alias {
            *counts.entry(alias).or_insert(0) += 1;
        }
    }
    Ok(counts)
}

fn resolve_request_credentials(
    credentials: &CredentialVault,
    credential_alias: Option<String>,
    username_env: Option<String>,
    password_env: Option<String>,
) -> anyhow::Result<Option<ResolvedCredential>> {
    if let Some(alias) = credential_alias {
        return credentials
            .resolve(&alias, ResolvePurpose::Discover)
            .map(Some);
    }

    let username = username_env
        .as_deref()
        .and_then(|key| std::env::var(key).ok());
    let password = password_env
        .as_deref()
        .and_then(|key| std::env::var(key).ok());
    Ok(match (username, password) {
        (Some(username), Some(password)) => Some(ResolvedCredential { username, password }),
        _ => None,
    })
}

fn option_string(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn site_subtree_ids(sites: &[SiteRecord], root_id: &str) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::from([root_id.to_string()]);
    let mut changed = true;
    while changed {
        changed = false;
        for site in sites {
            if !site.parent_id.is_empty()
                && ids.contains(&site.parent_id)
                && ids.insert(site.id.clone())
            {
                changed = true;
            }
        }
    }
    ids
}

fn build_site_path_by_id(sites: &[SiteRecord]) -> HashMap<String, String> {
    let by_id: HashMap<&str, &SiteRecord> =
        sites.iter().map(|site| (site.id.as_str(), site)).collect();
    let mut path_by_id = HashMap::new();

    for site in sites {
        let mut names = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut current_id = site.id.as_str();
        let mut depth = 0;

        while depth < 16 && seen.insert(current_id.to_string()) {
            let Some(current) = by_id.get(current_id) else {
                break;
            };
            names.push(current.name.clone());
            if current.parent_id.is_empty() {
                break;
            }
            current_id = current.parent_id.as_str();
            depth += 1;
        }

        names.reverse();
        path_by_id.insert(site.id.clone(), names.join(" / "));
    }

    path_by_id
}

fn resolve_site_metadata(
    site: &str,
    site_id_by_name: &HashMap<String, String>,
    site_path_by_id: &HashMap<String, String>,
) -> (String, String) {
    let site = site.trim();
    if site.is_empty() {
        return (String::new(), String::new());
    }

    let site_id = if site_path_by_id.contains_key(site) {
        site.to_string()
    } else {
        site_id_by_name.get(site).cloned().unwrap_or_default()
    };
    if site_id.is_empty() {
        return (String::new(), String::new());
    }

    let site_path = site_path_by_id.get(&site_id).cloned().unwrap_or_default();
    (site_id, site_path)
}

fn read_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

fn read_i64(v: &Value) -> i64 {
    match v {
        Value::Int64(n) => *n,
        _ => 0,
    }
}

fn read_ts_ns(v: &Value) -> i64 {
    match v {
        Value::TimestampNs(dt) => dt.unix_timestamp_nanos() as i64,
        _ => 0,
    }
}

