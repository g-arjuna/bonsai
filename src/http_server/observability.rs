#![allow(unused_imports, dead_code, unused_variables)]
use super::*;

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn topology_handler(
    State(state): State<AppState>,
) -> Result<Json<TopologyResponse>, (StatusCode, String)> {
    let db = state.store.db();

    let (devices_raw, links_raw, bgp_raw) = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;

        // Devices
        let dev_rows = conn
            .query("MATCH (d:Device) RETURN d.address, d.vendor, d.hostname")
            .map_err(|e| e.to_string())?;
        let devices_raw: Vec<(String, String, String)> = dev_rows
            .map(|row| (read_str(&row[0]), read_str(&row[1]), read_str(&row[2])))
            .collect();

        // LLDP fabric links with interface counter totals for heatmap
        let link_rows = conn
            .query(
                "MATCH (a:Interface)-[:CONNECTED_TO]->(b:Interface) \
                 RETURN a.device_address, a.name, b.device_address, b.name, \
                        a.in_octets + a.out_octets + b.in_octets + b.out_octets",
            )
            .map_err(|e| e.to_string())?;
        let links_raw: Vec<(String, String, String, String, i64, bool)> = link_rows
            .map(|row| {
                (
                    read_str(&row[0]),
                    read_str(&row[1]),
                    read_str(&row[2]),
                    read_str(&row[3]),
                    read_i64(&row[4]),
                    false,
                )
            })
            .collect();

        // Management-plane LLDP links (out-of-band; hidden by default in UI)
        let mgmt_rows = conn
            .query(
                "MATCH (a:Interface)-[:MGMT_LINK]->(b:Interface) \
                 RETURN a.device_address, a.name, b.device_address, b.name",
            )
            .map_err(|e| e.to_string())?;
        let mgmt_raw: Vec<(String, String, String, String, i64, bool)> = mgmt_rows
            .map(|row| {
                (
                    read_str(&row[0]),
                    read_str(&row[1]),
                    read_str(&row[2]),
                    read_str(&row[3]),
                    0i64,
                    true,
                )
            })
            .collect();
        let links_raw: Vec<(String, String, String, String, i64, bool)> =
            links_raw.into_iter().chain(mgmt_raw).collect();

        // BGP neighbors
        let bgp_rows = conn
            .query(
                "MATCH (n:BgpNeighbor) \
                 RETURN n.device_address, n.peer_address, n.session_state, n.peer_as",
            )
            .map_err(|e| e.to_string())?;
        let bgp_raw: Vec<(String, String, String, i64)> = bgp_rows
            .map(|row| {
                (
                    read_str(&row[0]),
                    read_str(&row[1]),
                    read_str(&row[2]),
                    read_i64(&row[3]),
                )
            })
            .collect();

        Ok::<_, String>((devices_raw, links_raw, bgp_raw))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let all_sites = state.store.list_sites().await.unwrap_or_else(|error| {
        tracing::warn!(%error, "failed to list sites for topology metadata");
        Vec::new()
    });
    let site_id_by_name: HashMap<String, String> = all_sites
        .iter()
        .map(|site| (site.name.clone(), site.id.clone()))
        .collect();
    let site_path_by_id = build_site_path_by_id(&all_sites);

    // Build role + site map from registry
    let mut role_site: HashMap<String, (String, String, String, String)> = HashMap::new();
    if let Ok(targets) = state.registry.list_all_targets() {
        for t in targets {
            let site = t.site.unwrap_or_default();
            let (site_id, site_path) =
                resolve_site_metadata(&site, &site_id_by_name, &site_path_by_id);
            role_site.insert(
                t.address.clone(),
                (t.role.unwrap_or_default(), site, site_id, site_path),
            );
        }
    }

    // Group BGP by device
    let mut bgp_by_device: HashMap<String, Vec<BgpJson>> = HashMap::new();
    for (dev, peer, st, peer_as) in bgp_raw {
        bgp_by_device.entry(dev).or_default().push(BgpJson {
            peer,
            state: st,
            peer_as,
        });
    }

    // Build device list with computed health + registry metadata
    let devices: Vec<DeviceJson> = devices_raw
        .into_iter()
        .map(|(address, vendor, hostname)| {
            let bgp = bgp_by_device.remove(&address).unwrap_or_default();
            let health = compute_health(&bgp);
            let (role, site, site_id, site_path) = role_site.remove(&address).unwrap_or_default();
            DeviceJson {
                address,
                hostname,
                vendor,
                role,
                site,
                site_id,
                site_path,
                health,
                bgp,
            }
        })
        .collect();

    let links = links_raw
        .into_iter()
        .map(
            |(src_device, src_iface, dst_device, dst_iface, bytes_total, is_mgmt)| LinkJson {
                src_device,
                src_iface,
                dst_device,
                dst_iface,
                bytes_total,
                is_mgmt,
            },
        )
        .collect();

    Ok(Json(TopologyResponse {
        schema_version: API_SCHEMA_VERSION.to_string(),
        devices,
        links,
    }))
}

#[derive(Deserialize)]
struct PathParams {
    src: String,
    dst: String,
}

/// Shortest path between two devices, computed in the graph database.
///
/// Replaces Rust-side BFS that loaded all CONNECTED_TO edges into a Vec.
/// The graph DB traverses HAS_INTERFACE|CONNECTED_TO edges with a variable-
/// length pattern and returns a single path; no edge-loading into Rust.
async fn path_handler(
    State(state): State<AppState>,
    Query(params): Query<PathParams>,
) -> Result<Json<PathResponse>, (StatusCode, String)> {
    let db = state.store.db();
    let (src, dst) = (params.src.clone(), params.dst.clone());

    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::queries::shortest_topology_path(&conn, &src, &dst).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    match result {
        None => Ok(Json(PathResponse {
            schema_version: API_SCHEMA_VERSION.to_string(),
            hops: vec![],
            links: vec![],
        })),
        Some(path) => Ok(Json(PathResponse {
            schema_version: API_SCHEMA_VERSION.to_string(),
            hops: path.hops,
            links: path.links,
        })),
    }
}

// ─── blast radius ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct BlastRadiusParams {
    #[serde(default = "default_max_hops")]
    max_hops: usize,
}

fn default_max_hops() -> usize {
    2
}

/// Devices, applications, and active detections reachable from `address` within
/// `max_hops` physical network hops.
///
/// Example: GET /api/blast-radius/10.0.0.1?max_hops=2
async fn blast_radius_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(params): Query<BlastRadiusParams>,
) -> Result<Json<crate::graph::queries::BlastRadiusResult>, (StatusCode, String)> {
    let db = state.store.db();
    let max_hops = params.max_hops.min(5); // cap at 5 to bound query time

    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::queries::blast_radius(&conn, &address, max_hops).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn managed_devices_handler(
    State(state): State<AppState>,
) -> Result<Json<ManagedDevicesResponse>, (StatusCode, String)> {
    let targets = state
        .registry
        .list_active()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let statuses = read_subscription_statuses(state.store.db()).await?;

    let overrides = state.registry.list_overrides().unwrap_or_default();
    let devices = targets
        .into_iter()
        .map(|target| managed_device_json(target, &statuses, &overrides))
        .collect();

    Ok(Json(ManagedDevicesResponse { devices }))
}

async fn discover_handler(
    State(state): State<AppState>,
    Json(req): Json<OnboardingDiscoveryRequest>,
) -> Result<Json<discovery::DiscoveryReport>, (StatusCode, String)> {
    let credentials = resolve_request_credentials(
        &state.credentials,
        option_string(req.credential_alias),
        option_string(req.username_env),
        option_string(req.password_env),
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    let (username, password) = match credentials {
        Some(credentials) => (Some(credentials.username), Some(credentials.password)),
        None => (None, None),
    };

    let report = discovery::discover_device(DiscoveryInput {
        address: req.address,
        username,
        password,
        username_env: None,
        password_env: None,
        ca_cert_path: option_string(req.ca_cert_path),
        tls_domain: option_string(req.tls_domain),
        role_hint: option_string(req.role_hint),
        environment_archetype: option_string(req.environment_archetype),
    })
    .await
    .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    Ok(Json(report))
}

async fn credentials_handler(
    State(state): State<AppState>,
) -> Result<Json<CredentialsResponse>, (StatusCode, String)> {
    let device_counts = credential_device_counts(&state.registry)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let credentials = state
        .credentials
        .list()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?
        .into_iter()
        .map(|credential| credential_json(credential, &device_counts))
        .collect();
    let unlocked = state
        .credentials
        .is_unlocked()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(CredentialsResponse {
        credentials,
        unlocked,
    }))
}

async fn add_credential_handler(
    State(state): State<AppState>,
    Json(req): Json<AddCredentialRequest>,
) -> Result<Json<CredentialMutationResponse>, (StatusCode, String)> {
    match state
        .credentials
        .add(&req.alias, &req.username, &req.password)
    {
        Ok(credential) => {
            let device_counts = credential_device_counts(&state.registry)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            Ok(Json(CredentialMutationResponse {
                success: true,
                error: String::new(),
                credential: Some(credential_json(credential, &device_counts)),
            }))
        }
        Err(error) => Ok(Json(CredentialMutationResponse {
            success: false,
            error: format!("{error:#}"),
            credential: None,
        })),
    }
}

async fn update_credential_handler(
    State(state): State<AppState>,
    Json(req): Json<AddCredentialRequest>,
) -> Result<Json<CredentialMutationResponse>, (StatusCode, String)> {
    let username = if req.username.trim().is_empty() {
        state
            .credentials
            .username_for_alias(&req.alias)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?
    } else {
        req.username.clone()
    };
    match state
        .credentials
        .update(&req.alias, &username, &req.password)
    {
        Ok(credential) => {
            let device_counts = credential_device_counts(&state.registry)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            Ok(Json(CredentialMutationResponse {
                success: true,
                error: String::new(),
                credential: Some(credential_json(credential, &device_counts)),
            }))
        }
        Err(error) => Ok(Json(CredentialMutationResponse {
            success: false,
            error: format!("{error:#}"),
            credential: None,
        })),
    }
}

async fn remove_credential_handler(
    State(state): State<AppState>,
    Json(req): Json<RemoveCredentialRequest>,
) -> Result<Json<CredentialMutationResponse>, (StatusCode, String)> {
    let device_counts = credential_device_counts(&state.registry)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    if device_counts
        .get(req.alias.trim())
        .copied()
        .unwrap_or_default()
        > 0
    {
        return Ok(Json(CredentialMutationResponse {
            success: false,
            error: format!(
                "credential alias '{}' is still referenced by {} device(s)",
                req.alias.trim(),
                device_counts
                    .get(req.alias.trim())
                    .copied()
                    .unwrap_or_default()
            ),
            credential: None,
        }));
    }
    match state.credentials.remove(&req.alias) {
        Ok(Some(credential)) => Ok(Json(CredentialMutationResponse {
            success: true,
            error: String::new(),
            credential: Some(credential_json(credential, &device_counts)),
        })),
        Ok(None) => Ok(Json(CredentialMutationResponse {
            success: false,
            error: format!("credential alias '{}' not found", req.alias),
            credential: None,
        })),
        Err(error) => Ok(Json(CredentialMutationResponse {
            success: false,
            error: format!("{error:#}"),
            credential: None,
        })),
    }
}

async fn test_credential_handler(
    State(state): State<AppState>,
    Json(req): Json<TestCredentialRequest>,
) -> Result<Json<discovery::DiscoveryReport>, (StatusCode, String)> {
    let credentials = state
        .credentials
        .resolve(&req.alias, ResolvePurpose::Test)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    let report = discovery::discover_device(DiscoveryInput {
        address: req.address,
        username: Some(credentials.username),
        password: Some(credentials.password),
        username_env: None,
        password_env: None,
        ca_cert_path: option_string(req.ca_cert_path),
        tls_domain: option_string(req.tls_domain),
        role_hint: option_string(req.role_hint),
        environment_archetype: None,
    })
    .await
    .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    Ok(Json(report))
}

async fn add_managed_device_handler(
    State(state): State<AppState>,
    Json(req): Json<ManagedDeviceRequest>,
) -> Result<Json<MutationResponse>, (StatusCode, String)> {
    save_managed_device(state, req).await
}

async fn add_managed_device_with_paths_handler(
    State(state): State<AppState>,
    Json(req): Json<ManagedDeviceRequest>,
) -> Result<Json<MutationResponse>, (StatusCode, String)> {
    if req.selected_paths.is_empty() {
        return Ok(Json(MutationResponse {
            success: false,
            error: "selected_paths is required for /api/onboarding/devices/with_paths".to_string(),
            device: None,
        }));
    }
    save_managed_device(state, req).await
}

async fn save_managed_device(
    state: AppState,
    req: ManagedDeviceRequest,
) -> Result<Json<MutationResponse>, (StatusCode, String)> {
    let mut target = target_from_request(req)?;
    if let Ok(Some(existing)) = state.registry.get_device(&target.address) {
        if target.credential_alias.is_none() {
            target.credential_alias = existing.credential_alias;
        }
        if target.username_env.is_none() {
            target.username_env = existing.username_env;
        }
        if target.password_env.is_none() {
            target.password_env = existing.password_env;
        }
        if target.username.is_none() {
            target.username = existing.username;
        }
        if target.password.is_none() {
            target.password = existing.password;
        }
        if target.selected_paths.is_empty() {
            target.selected_paths = existing.selected_paths;
        }
    }
    let address = target.address.clone();
    let result = match state
        .registry
        .add_device_with_audit(target.clone(), "api", "api_add_device")
    {
        Ok(device) => Ok(device),
        Err(add_error) => match state.registry.get_device(&address) {
            Ok(Some(_)) => state
                .registry
                .update_device_with_audit(target, "api", "api_update_device")
                .map_err(|update_error| {
                    format!("add failed: {add_error:#}; update failed: {update_error:#}")
                }),
            _ => Err(add_error.to_string()),
        },
    };

    match result {
        Ok(device) => {
            if let Err(error) = state
                .store
                .sync_sites_from_targets(vec![device.clone()])
                .await
            {
                return Ok(Json(MutationResponse {
                    success: false,
                    error: format!("device saved but site graph sync failed: {error:#}"),
                    device: Some(managed_device_json(
                        device,
                        &HashMap::new(),
                        &state.registry.list_overrides().unwrap_or_default(),
                    )),
                }));
            }
            let statuses = read_subscription_statuses(state.store.db()).await?;
            Ok(Json(MutationResponse {
                success: true,
                error: String::new(),
                device: Some(managed_device_json(
                    device,
                    &statuses,
                    &state.registry.list_overrides().unwrap_or_default(),
                )),
            }))
        }
        Err(error) => Ok(Json(MutationResponse {
            success: false,
            error,
            device: None,
        })),
    }
}

async fn sites_handler(
    State(state): State<AppState>,
) -> Result<Json<SitesResponse>, (StatusCode, String)> {
    let sites = state
        .store
        .list_sites()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?
        .into_iter()
        .map(site_json)
        .collect();
    Ok(Json(SitesResponse { sites }))
}

async fn upsert_site_handler(
    State(state): State<AppState>,
    Json(req): Json<SiteJson>,
) -> Result<Json<SiteMutationResponse>, (StatusCode, String)> {
    match state.store.upsert_site(site_record(req)).await {
        Ok(site) => Ok(Json(SiteMutationResponse {
            success: true,
            error: String::new(),
            site: Some(site_json(site)),
        })),
        Err(error) => Ok(Json(SiteMutationResponse {
            success: false,
            error: format!("{error:#}"),
            site: None,
        })),
    }
}

async fn site_summary_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SiteSummaryResponse>, (StatusCode, String)> {
    let all_sites = state
        .store
        .list_sites()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let site = all_sites
        .iter()
        .find(|site| site.id == id)
        .cloned()
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("site '{id}' not found")))?;

    let subtree_ids = site_subtree_ids(&all_sites, &site.id);
    let subtree_names: std::collections::HashSet<String> = all_sites
        .iter()
        .filter(|candidate| subtree_ids.contains(&candidate.id))
        .map(|candidate| candidate.name.clone())
        .collect();

    let targets = state
        .registry
        .list_all_targets()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let site_targets: Vec<TargetConfig> = targets
        .into_iter()
        .filter(|target| {
            target
                .site
                .as_ref()
                .map(|site_name| subtree_names.contains(site_name))
                .unwrap_or(false)
        })
        .collect();
    let device_addresses: std::collections::HashSet<String> = site_targets
        .iter()
        .map(|target| target.address.clone())
        .collect();

    let db = state.store.db();
    let bgp_rows = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let rows = conn
            .query(
                "MATCH (n:BgpNeighbor) \
                 RETURN n.device_address, n.session_state",
            )
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(
            rows.map(|row| (read_str(&row[0]), read_str(&row[1])))
                .collect::<Vec<_>>(),
        )
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut bgp_by_device: HashMap<String, Vec<BgpJson>> = HashMap::new();
    for (address, session_state) in bgp_rows {
        if !device_addresses.contains(&address) {
            continue;
        }
        bgp_by_device.entry(address).or_default().push(BgpJson {
            peer: String::new(),
            state: session_state,
            peer_as: 0,
        });
    }

    let mut health = SiteHealthJson::default();
    let devices = site_targets
        .iter()
        .map(|target| {
            let device_health = compute_health(
                bgp_by_device
                    .get(&target.address)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            );
            match device_health.as_str() {
                "healthy" => health.healthy += 1,
                "warn" => health.warn += 1,
                _ => health.critical += 1,
            }
            SiteDeviceJson {
                address: target.address.clone(),
                hostname: target.hostname.clone().unwrap_or_default(),
                vendor: target.vendor.clone().unwrap_or_default(),
                role: target.role.clone().unwrap_or_default(),
                collector_id: target.collector_id.clone().unwrap_or_default(),
                health: device_health,
            }
        })
        .collect::<Vec<_>>();

    let all_statuses = read_subscription_statuses(state.store.db()).await?;
    let mut subscription_summary = SiteSubscriptionSummaryJson::default();
    for address in &device_addresses {
        for status in all_statuses.get(address).cloned().unwrap_or_default() {
            match status.status.as_str() {
                "observed" => subscription_summary.observed += 1,
                "pending" => subscription_summary.pending += 1,
                _ => subscription_summary.silent += 1,
            }
        }
    }

    let recent_detections = state
        .store
        .read_detections(100)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .into_iter()
        .filter(|row| device_addresses.contains(&row.device_address))
        .take(10)
        .collect();

    let child_site_count = all_sites
        .iter()
        .filter(|candidate| candidate.parent_id == site.id)
        .count();

    Ok(Json(SiteSummaryResponse {
        site: site_json(site),
        child_site_count,
        device_count: devices.len(),
        health,
        subscription_summary,
        devices,
        recent_detections,
    }))
}

async fn remove_site_handler(
    State(state): State<AppState>,
    Json(req): Json<RemoveSiteRequest>,
) -> Result<Json<SiteMutationResponse>, (StatusCode, String)> {
    let all_sites = state
        .store
        .list_sites()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let site = match all_sites.iter().find(|site| site.id == req.id).cloned() {
        Some(site) => site,
        None => {
            return Ok(Json(SiteMutationResponse {
                success: false,
                error: format!("site '{}' not found", req.id),
                site: None,
            }));
        }
    };
    if all_sites
        .iter()
        .any(|candidate| candidate.parent_id == site.id)
    {
        return Ok(Json(SiteMutationResponse {
            success: false,
            error: "cannot delete a site that still has child sites".to_string(),
            site: None,
        }));
    }

    let in_use = state
        .registry
        .list_all_targets()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?
        .into_iter()
        .filter(|target| target.site.as_deref() == Some(site.name.as_str()))
        .count();
    if in_use > 0 {
        return Ok(Json(SiteMutationResponse {
            success: false,
            error: format!(
                "cannot delete site '{}' while {} device(s) still reference it",
                site.name, in_use
            ),
            site: None,
        }));
    }

    let db = state.store.db();
    let site_id = site.id.clone();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("MATCH (s:Site {id: $id}) DETACH DELETE s")
            .map_err(|e| e.to_string())?;
        conn.execute(&mut stmt, vec![("id", Value::String(site_id))])
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(SiteMutationResponse {
        success: true,
        error: String::new(),
        site: Some(site_json(site)),
    }))
}

async fn remove_managed_device_handler(
    State(state): State<AppState>,
    Json(req): Json<RemoveManagedDeviceRequest>,
) -> Result<Json<MutationResponse>, (StatusCode, String)> {
    match state.registry.remove_device(&req.address) {
        Ok(Some(device)) => Ok(Json(MutationResponse {
            success: true,
            error: String::new(),
            device: Some(managed_device_json(
                device,
                &HashMap::new(),
                &state.registry.list_overrides().unwrap_or_default(),
            )),
        })),
        Ok(None) => Ok(Json(MutationResponse {
            success: false,
            error: format!("device '{}' not found", req.address),
            device: None,
        })),
        Err(error) => Ok(Json(MutationResponse {
            success: false,
            error: error.to_string(),
            device: None,
        })),
    }
}

async fn bulk_managed_device_action_handler(
    State(state): State<AppState>,
    Json(req): Json<BulkManagedDeviceActionRequest>,
) -> Result<Json<BulkManagedDeviceActionResponse>, (StatusCode, String)> {
    if req.addresses.is_empty() {
        return Ok(Json(BulkManagedDeviceActionResponse {
            success: false,
            error: "at least one address is required".to_string(),
            devices: Vec::new(),
        }));
    }

    let action = req.action.trim().to_ascii_lowercase();
    if !matches!(action.as_str(), "stop" | "start" | "restart") {
        return Ok(Json(BulkManagedDeviceActionResponse {
            success: false,
            error: "action must be one of: stop, start, restart".to_string(),
            devices: Vec::new(),
        }));
    }

    let statuses = read_subscription_statuses(state.store.db()).await?;
    let mut devices = Vec::new();
    let mut errors = Vec::new();
    for address in req.addresses {
        match state.registry.get_device(&address) {
            Ok(Some(mut target)) => {
                target.enabled = action != "stop";
                match state.registry.update_device_with_audit(
                    target,
                    "api",
                    &format!("api_bulk_{action}"),
                ) {
                    Ok(device) => devices.push(managed_device_json(
                        device,
                        &statuses,
                        &state.registry.list_overrides().unwrap_or_default(),
                    )),
                    Err(error) => errors.push(format!("{address}: {error:#}")),
                }
            }
            Ok(None) => errors.push(format!("{address}: device not found")),
            Err(error) => errors.push(format!("{address}: {error:#}")),
        }
    }

    Ok(Json(BulkManagedDeviceActionResponse {
        success: errors.is_empty(),
        error: errors.join("; "),
        devices,
    }))
}

async fn remove_impact_handler(
    State(state): State<AppState>,
    Json(req): Json<RemoveManagedDeviceRequest>,
) -> Result<Json<RemoveImpactResponse>, (StatusCode, String)> {
    let statuses = read_subscription_statuses(state.store.db()).await?;
    let device_statuses = statuses.get(&req.address).cloned().unwrap_or_default();
    let subscription_total = device_statuses.len();
    let subscription_observed = device_statuses
        .iter()
        .filter(|status| status.status == "observed")
        .count();
    let subscription_pending = device_statuses
        .iter()
        .filter(|status| status.status == "pending")
        .count();

    let (trust_marks_total, trust_marks_active) =
        read_trust_mark_impact(state.store.db(), req.address.clone()).await?;

    Ok(Json(RemoveImpactResponse {
        address: req.address,
        subscription_total,
        subscription_observed,
        subscription_pending,
        trust_marks_total,
        trust_marks_active,
    }))
}

async fn detections_handler(
    State(state): State<AppState>,
    Query(params): Query<DetectionsParams>,
) -> Result<Json<DetectionsResponse>, (StatusCode, String)> {
    let detections = state
        .store
        .read_detections(params.limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(DetectionsResponse {
        schema_version: API_SCHEMA_VERSION.to_string(),
        detections,
    }))
}

async fn trace_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TraceResponse>, (StatusCode, String)> {
    let steps = state
        .store
        .read_closed_loop_trace(id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(TraceResponse {
        schema_version: API_SCHEMA_VERSION.to_string(),
        steps,
    }))
}

async fn readiness_handler(
    State(state): State<AppState>,
) -> Result<Json<ReadinessResponse>, (StatusCode, String)> {
    let db = state.store.db();

    let readiness = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;

        let detection_rows = conn
            .query("MATCH (e:DetectionEvent) RETURN e.rule_id")
            .map_err(|e| e.to_string())?;
        let mut detection_events = 0usize;
        let mut rule_distribution: HashMap<String, usize> = HashMap::new();
        for row in detection_rows {
            detection_events += 1;
            let rule_id = read_str(&row[0]);
            if !rule_id.is_empty() {
                *rule_distribution.entry(rule_id).or_insert(0) += 1;
            }
        }

        let state_rows = conn
            .query("MATCH (e:StateChangeEvent) RETURN count(e)")
            .map_err(|e| e.to_string())?;
        let mut state_change_events = 0usize;
        for row in state_rows {
            state_change_events = read_i64(&row[0]).max(0) as usize;
        }

        let remediation_rows = conn
            .query(
                "MATCH (m:RemediationTrustMark)-[:TRUST_MARKS]->(r:Remediation) \
                 WHERE m.trustworthy = 1 \
                 RETURN r.action, r.status",
            )
            .map_err(|e| e.to_string())?;
        let mut remediation_rows_post_cutoff = 0usize;
        let mut action_distribution_post_cutoff: HashMap<String, usize> = HashMap::new();
        let mut status_distribution_post_cutoff: HashMap<String, usize> = HashMap::new();
        for row in remediation_rows {
            remediation_rows_post_cutoff += 1;

            let action = read_str(&row[0]);
            if !action.is_empty() {
                *action_distribution_post_cutoff.entry(action).or_insert(0) += 1;
            }

            let status = read_str(&row[1]);
            if !status.is_empty() {
                *status_distribution_post_cutoff.entry(status).or_insert(0) += 1;
            }
        }

        Ok::<_, String>(ReadinessResponse {
            schema_version: API_SCHEMA_VERSION.to_string(),
            detection_events,
            state_change_events,
            rule_distribution,
            cutoff_iso: REMEDIATION_TRUST_CUTOFF_ISO.to_string(),
            remediation_rows_post_cutoff,
            action_distribution_post_cutoff,
            status_distribution_post_cutoff,
        })
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(readiness))
}

async fn operations_handler(
    State(state): State<AppState>,
) -> Result<Json<OperationsResponse>, (StatusCode, String)> {
    let readiness = readiness_handler(State(state.clone())).await?.0;
    let targets = state
        .registry
        .list_all_targets()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let statuses = read_subscription_statuses(state.store.db()).await?;

    let mut observed_subscriptions = 0usize;
    let mut pending_subscriptions = 0usize;
    let mut silent_subscriptions = 0usize;
    for rows in statuses.values() {
        for status in rows {
            match status.status.as_str() {
                "observed" => observed_subscriptions += 1,
                "pending" => pending_subscriptions += 1,
                _ => silent_subscriptions += 1,
            }
        }
    }

    let collector_summary = state
        .collector_manager
        .as_ref()
        .map(|manager| manager.collector_status_summary())
        .unwrap_or_else(|| crate::assignment::CollectorStatusSummary {
            collectors: Vec::new(),
            unassigned_devices: Vec::new(),
        });
    let bus_snapshot = event_bus::InProcessBus::snapshot();
    let archive_snapshot = archive::snapshot();
    let mem_snapshot = memory_profile::snapshot();
    let disk_snapshot = disk_guard::snapshot(
        std::path::Path::new(&state.archive_path),
        std::path::Path::new(&state.graph_path),
        &state.storage_config,
    );
    // Use the governor's profile-derived budget when available; the hardcoded
    // constant is a last-resort fallback for non-governed modes.
    let effective_rss_budget = state
        .governor
        .as_ref()
        .map(|g| g.snapshot().memory_budget_mb * 1024 * 1024)
        .unwrap_or(RSS_BUDGET_BYTES);

    Ok(Json(OperationsResponse {
        schema_version: API_SCHEMA_VERSION.to_string(),
        detection_events: readiness.detection_events,
        state_change_events: readiness.state_change_events,
        remediation_rows_post_cutoff: readiness.remediation_rows_post_cutoff,
        rule_distribution: readiness.rule_distribution,
        action_distribution_post_cutoff: readiness.action_distribution_post_cutoff,
        status_distribution_post_cutoff: readiness.status_distribution_post_cutoff,
        device_count: targets.len(),
        enabled_device_count: targets.iter().filter(|target| target.enabled).count(),
        observed_subscriptions,
        pending_subscriptions,
        silent_subscriptions,
        collectors_connected: collector_summary
            .collectors
            .iter()
            .filter(|collector| collector.connected)
            .count(),
        collectors_total: collector_summary.collectors.len(),
        unassigned_devices: collector_summary.unassigned_devices.len(),
        event_bus_depth: bus_snapshot.depth,
        event_bus_receivers: bus_snapshot.receivers,
        archive_lag_millis: archive_snapshot.lag_millis,
        archive_buffer_rows: archive_snapshot.buffer_rows,
        archive_last_flush_millis: archive_snapshot.last_flush_millis,
        archive_last_compression_ppm: archive_snapshot.last_compression_ppm,
        cutoff_iso: readiness.cutoff_iso,
        rss_bytes: mem_snapshot.rss_bytes,
        archive_disk_bytes: disk_snapshot.archive_bytes,
        archive_disk_pct: disk_snapshot.archive_pct,
        graph_disk_bytes: disk_snapshot.graph_bytes,
        graph_disk_pct: disk_snapshot.graph_pct,
        memory_budget_bytes: effective_rss_budget,
        memory_rss_pct_of_budget: if effective_rss_budget > 0 {
            (mem_snapshot.rss_bytes as f64 / effective_rss_budget as f64) * 100.0
        } else {
            0.0
        },
        counter_mode: state.counter_mode.clone(),
        counter_window_secs: state.counter_window_secs,
        counter_debounce_secs: state.counter_debounce_secs,
    }))
}

async fn test_status_handler(
    State(state): State<AppState>,
) -> Result<Json<TestStatusResponse>, (StatusCode, String)> {
    let mem = memory_profile::snapshot();
    let disk = disk_guard::snapshot(
        std::path::Path::new(&state.archive_path),
        std::path::Path::new(&state.graph_path),
        &state.storage_config,
    );

    let external_path = std::path::Path::new(&state.runtime_dir).join("external_status.json");
    let external: serde_json::Value = tokio::fs::read_to_string(&external_path)
        .await
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);

    let driver_dir = std::path::Path::new(&state.runtime_dir).join("driver_results");
    let mut driver_results = serde_json::Map::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&driver_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if let Ok(s) = tokio::fs::read_to_string(&path).await
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&s)
            {
                driver_results.insert(stem.to_string(), v);
            }
        }
    }

    let ts_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let effective_rss_budget = state
        .governor
        .as_ref()
        .map(|g| g.snapshot().memory_budget_mb * 1024 * 1024)
        .unwrap_or(RSS_BUDGET_BYTES);

    let mut budget_breaches: Vec<BudgetBreach> = Vec::new();
    if mem.rss_bytes > effective_rss_budget {
        budget_breaches.push(BudgetBreach {
            name: "rss_budget",
            current: mem.rss_bytes as f64,
            budget: effective_rss_budget as f64,
            unit: "bytes",
        });
    }
    if mem.write_coordinator_queue_pct >= COORDINATOR_QUEUE_BUDGET_PCT {
        budget_breaches.push(BudgetBreach {
            name: "write_coordinator_queue_budget",
            current: mem.write_coordinator_queue_pct as f64,
            budget: COORDINATOR_QUEUE_BUDGET_PCT as f64,
            unit: "percent",
        });
    }

    Ok(Json(TestStatusResponse {
        ts_unix,
        memory: mem,
        disk: DiskStatusJson {
            archive_bytes: disk.archive_bytes,
            archive_max_bytes: disk.archive_max_bytes,
            archive_pct: disk.archive_pct,
            graph_bytes: disk.graph_bytes,
            graph_max_bytes: disk.graph_max_bytes,
            graph_pct: disk.graph_pct,
        },
        budget_breaches,
        external,
        driver_results: serde_json::Value::Object(driver_results),
    }))
}

#[derive(Serialize)]
struct DailyCheckResponse {
    ts_unix: u64,
    status: String,
    counts: DailyCheckCounts,
    checks: Vec<DailyCheckItem>,
}

#[derive(Serialize)]
struct DailyCheckCounts {
    pass: usize,
    fail: usize,
    skip: usize,
    prereq_missing: usize,
}

#[derive(Serialize)]
struct DailyCheckItem {
    name: String,
    status: String,
    summary: String,
}

async fn daily_check_handler(
    State(state): State<AppState>,
) -> Result<Json<DailyCheckResponse>, (StatusCode, String)> {
    let driver_dir = std::path::Path::new(&state.runtime_dir).join("driver_results");
    let mut checks: Vec<DailyCheckItem> = Vec::new();
    let mut counts = DailyCheckCounts {
        pass: 0,
        fail: 0,
        skip: 0,
        prereq_missing: 0,
    };

    if let Ok(mut entries) = tokio::fs::read_dir(&driver_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            // Exclude daily.json — it is a derived meta-file (self-referential in aggregation)
            if path.file_name().and_then(|n| n.to_str()) == Some("daily.json") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            let (status, summary) = if let Ok(s) = tokio::fs::read_to_string(&path).await {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                    let st = v
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let sm = v
                        .get("summary")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();
                    (st, sm)
                } else {
                    ("fail".to_string(), "invalid json".to_string())
                }
            } else {
                ("fail".to_string(), "unreadable".to_string())
            };

            match status.as_str() {
                "pass" => counts.pass += 1,
                "fail" => counts.fail += 1,
                "prereq_missing" => counts.prereq_missing += 1,
                _ => counts.skip += 1,
            }
            checks.push(DailyCheckItem {
                name,
                status,
                summary,
            });
        }
    }

    checks.sort_by(|a, b| a.name.cmp(&b.name));

    let overall = if counts.fail > 0 {
        "fail"
    } else if counts.prereq_missing > 0 {
        "pass_with_caveats"
    } else if counts.pass == 0 {
        "warn"
    } else {
        "pass"
    };

    let ts_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    Ok(Json(DailyCheckResponse {
        ts_unix,
        status: overall.to_string(),
        counts,
        checks,
    }))
}

