use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use lbug::{Connection, Value};
use std::collections::HashMap;

use super::AppState;
use super::{
    AddCredentialRequest, BgpJson, BulkImportResponse, BulkImportResult,
    BulkManagedDeviceActionRequest, BulkManagedDeviceActionResponse, CredentialJson,
    CredentialMutationResponse, CredentialsResponse, ManagedDeviceJson, ManagedDeviceRequest,
    ManagedDevicesResponse, MutationResponse, OnboardingDiscoveryRequest, RemoveCredentialRequest,
    RemoveImpactResponse, RemoveManagedDeviceRequest, RemoveSiteRequest, SiteDeviceJson,
    SiteHealthJson, SiteJson, SiteMutationResponse, SiteSubscriptionSummaryJson,
    SiteSummaryResponse, SitesResponse, SubscriptionStatusJson, TestCredentialRequest,
    compute_health, option_string, read_device_vendors, read_str, read_subscription_statuses,
    read_trust_mark_impact,
};
use crate::config::TargetConfig;
use crate::credentials::{CredentialSummary, CredentialVault, ResolvePurpose, ResolvedCredential};
use crate::discovery::{self, DiscoveryInput};
use crate::graph::SiteRecord;
use crate::registry::{ApiRegistry, DeviceRegistry};

pub(super) async fn managed_devices_handler(
    State(state): State<AppState>,
) -> Result<Json<ManagedDevicesResponse>, (StatusCode, String)> {
    let targets = state
        .registry
        .list_active()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let statuses = read_subscription_statuses(state.store.db()).await?;
    let graph_vendors = read_device_vendors(state.store.db())
        .await
        .unwrap_or_default();

    let overrides = state.registry.list_overrides().unwrap_or_default();
    let devices = targets
        .into_iter()
        .map(|mut target| {
            // D2-10 T3: back-fill vendor from the Device graph node when the
            // registry entry lacks it (devices onboarded without --vendor flag).
            if target.vendor.as_deref().unwrap_or_default().is_empty() {
                if let Some(v) = graph_vendors.get(&target.address) {
                    target.vendor = Some(v.clone());
                }
            }
            managed_device_json(target, &statuses, &overrides)
        })
        .collect();

    Ok(Json(ManagedDevicesResponse { devices }))
}
pub(super) async fn discover_handler(
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
        Some(credentials) => {
            let password = credentials.password_string();
            (Some(credentials.username), Some(password))
        }
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
        vault: Some(std::sync::Arc::clone(&state.credentials)),
    })
    .await
    .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    Ok(Json(report))
}
pub(super) async fn credentials_handler(
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
pub(super) async fn add_credential_handler(
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
pub(super) async fn update_credential_handler(
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
pub(super) async fn remove_credential_handler(
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
pub(super) async fn test_credential_handler(
    State(state): State<AppState>,
    Json(req): Json<TestCredentialRequest>,
) -> Result<Json<discovery::DiscoveryReport>, (StatusCode, String)> {
    let credentials = state
        .credentials
        .resolve(&req.alias, ResolvePurpose::Test)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    let password = credentials.password_string();

    let report = discovery::discover_device(DiscoveryInput {
        address: req.address,
        username: Some(credentials.username),
        password: Some(password),
        username_env: None,
        password_env: None,
        ca_cert_path: option_string(req.ca_cert_path),
        tls_domain: option_string(req.tls_domain),
        role_hint: option_string(req.role_hint),
        environment_archetype: None,
        vault: Some(std::sync::Arc::clone(&state.credentials)),
    })
    .await
    .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    Ok(Json(report))
}
pub(super) async fn add_managed_device_handler(
    State(state): State<AppState>,
    Json(req): Json<ManagedDeviceRequest>,
) -> Result<Json<MutationResponse>, (StatusCode, String)> {
    save_managed_device(state, req).await
}
pub(super) async fn add_managed_device_with_paths_handler(
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
pub(super) async fn save_managed_device(
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
        // Preserve TLS and identity fields — critical to avoid bootstrap re-registration
        // clobbering the ca_cert/tls_domain configured via bonsai.toml or manual onboarding.
        if target.ca_cert.is_none() {
            target.ca_cert = existing.ca_cert;
        }
        if target.tls_domain.is_none() {
            target.tls_domain = existing.tls_domain;
        }
        if target.hostname.is_none() {
            target.hostname = existing.hostname;
        }
        if target.role.is_none() {
            target.role = existing.role;
        }
        if target.site.is_none() {
            target.site = existing.site;
        }
        if target.vendor.is_none() {
            target.vendor = existing.vendor;
        }
        // BN-015-U1: pass the inventory-only marker through unchanged —
        // the registry owns the lifecycle transition. An inventory-only
        // asset stays inventory-only unless this update carries an
        // explicit collection plan (enabled + selected_paths); enabling
        // without a plan is rejected there. Never resurrect it for an
        // already-configured device.
        target.inventory_only = existing.inventory_only;
        if target.external_id.is_none() {
            target.external_id = existing.external_id.clone();
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
pub(super) async fn sites_handler(
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
pub(super) async fn upsert_site_handler(
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
pub(super) async fn site_summary_handler(
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
            peer_device: String::new(),
            source: String::new(),
            resolution: String::new(),
            reason: String::new(),
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
pub(super) async fn remove_site_handler(
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
pub(super) async fn remove_managed_device_handler(
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
pub(super) async fn bulk_managed_device_action_handler(
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
pub(super) async fn bulk_import_handler(
    State(state): State<AppState>,
    Json(reqs): Json<Vec<ManagedDeviceRequest>>,
) -> Result<Json<BulkImportResponse>, (StatusCode, String)> {
    let mut results = Vec::with_capacity(reqs.len());
    let mut imported = 0usize;
    let mut failed = 0usize;

    for req in reqs {
        let address = req.address.clone();
        match save_managed_device(state.clone(), req).await {
            Ok(Json(r)) if r.success => {
                imported += 1;
                results.push(BulkImportResult {
                    address,
                    success: true,
                    error: String::new(),
                });
            }
            Ok(Json(r)) => {
                failed += 1;
                results.push(BulkImportResult {
                    address,
                    success: false,
                    error: r.error,
                });
            }
            Err((_, e)) => {
                failed += 1;
                results.push(BulkImportResult {
                    address,
                    success: false,
                    error: e,
                });
            }
        }
    }

    Ok(Json(BulkImportResponse {
        imported,
        failed,
        results,
    }))
}

// ── BN-015-U1: inventory-only import ────────────────────────────────────────
//
// Registers declared inventory assets WITHOUT authorizing telemetry
// acquisition. Imported devices are persisted with `enabled = false`,
// `inventory_only = true`, no credential alias and no selected paths —
// the subscriber manager never spawns a collector for them. Activation
// is a separate explicit operator action (configure a collection plan
// and credentials via the managed-device update path).

#[derive(Debug, serde::Deserialize)]
pub(super) struct InventoryImportRequest {
    /// Source system label, e.g. "netbox". Recorded in the audit action.
    #[serde(default)]
    pub source: String,
    pub devices: Vec<InventoryImportDevice>,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct InventoryImportDevice {
    /// Stable external identity, e.g.
    /// `netbox:https:netbox.example.com:443:device:123`. Must be
    /// namespaced and unique across ALL source instances — a bare
    /// `netbox:device:123` is only unique within one NetBox deployment.
    pub external_id: String,
    #[serde(default)]
    pub name: String,
    /// Primary management address. Required — candidates without one are
    /// reported `invalid`, never silently discarded or fabricated.
    pub address: String,
    #[serde(default)]
    pub site: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
    /// Inventory-declared platform label. Persisted as declared metadata
    /// (`inventory_platform`); never treated as an operational capability.
    #[serde(default)]
    pub platform: Option<String>,
    /// Inventory-declared lifecycle status. `None` stays unknown — it is
    /// NOT Bonsai-observed health.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub tenant: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct InventoryImportRow {
    pub external_id: String,
    pub address: String,
    pub name: String,
    /// imported | already_imported | updated | conflict | invalid | failed
    pub outcome: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct InventoryImportResponse {
    pub results: Vec<InventoryImportRow>,
    pub imported: usize,
    pub already_present: usize,
    pub updated: usize,
    pub conflicts: usize,
    pub invalid: usize,
    pub failed: usize,
    /// "synced" when the graph site/device projection completed for all
    /// successfully registered rows, "pending" when registry persistence
    /// succeeded but the graph projection failed — retries reconcile it.
    pub graph_sync: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub graph_sync_error: String,
}

fn inventory_row(
    dev: &InventoryImportDevice,
    outcome: &str,
    reason: impl Into<String>,
) -> InventoryImportRow {
    InventoryImportRow {
        external_id: dev.external_id.trim().to_string(),
        address: crate::registry::strip_port(dev.address.trim()).to_string(),
        name: dev.name.trim().to_string(),
        outcome: outcome.to_string(),
        reason: reason.into(),
    }
}

pub(super) async fn inventory_import_handler(
    State(state): State<AppState>,
    Json(req): Json<InventoryImportRequest>,
) -> Result<Json<InventoryImportResponse>, (StatusCode, String)> {
    let mut results = Vec::with_capacity(req.devices.len());
    let mut imported = 0usize;
    let mut already = 0usize;
    let mut updated = 0usize;
    let mut conflicts = 0usize;
    let mut invalid = 0usize;
    let mut failed = 0usize;
    let mut touched: Vec<TargetConfig> = Vec::new();

    for dev in &req.devices {
        let address = crate::registry::strip_port(dev.address.trim()).to_string();
        let external_id =
            match crate::registry::normalize_external_id(Some(dev.external_id.clone())) {
                Ok(Some(id)) => id,
                Ok(None) => {
                    invalid += 1;
                    results.push(inventory_row(dev, "invalid", "missing external id"));
                    continue;
                }
                Err(e) => {
                    invalid += 1;
                    results.push(inventory_row(dev, "invalid", format!("{e:#}")));
                    continue;
                }
            };

        if address.is_empty() {
            invalid += 1;
            results.push(inventory_row(dev, "invalid", "missing primary IP address"));
            continue;
        }

        // 1. Same external object already registered?
        match state.registry.find_by_external_id(&external_id) {
            Ok(Some(existing)) => {
                let existing_addr = crate::registry::strip_port(&existing.address).to_string();
                if crate::registry::canonical_device_key(&existing_addr)
                    != crate::registry::canonical_device_key(&address)
                {
                    conflicts += 1;
                    results.push(inventory_row(
                        dev,
                        "conflict",
                        format!(
                            "external id '{external_id}' is already bound to {existing_addr}; \
                             refusing to silently re-address the imported object"
                        ),
                    ));
                    continue;
                }
                // Same object, same canonical address.
                if existing.inventory_only {
                    // Inventory asset: refresh importer-owned declared
                    // metadata. A missing incoming field never erases a
                    // stored value; credentials, enabled flag, selected
                    // paths, collector assignment and lifecycle state
                    // are NEVER touched.
                    let mut target = existing.clone();
                    let mut changed = false;
                    let mut set_if = |slot: &mut Option<String>, new: Option<&str>| {
                        let trimmed = new.map(str::trim).filter(|s| !s.is_empty());
                        if let Some(value) = trimmed {
                            if *slot != Some(value.to_string()) {
                                *slot = Some(value.to_string());
                                changed = true;
                            }
                        }
                    };
                    set_if(&mut target.hostname, Some(dev.name.as_str()));
                    set_if(&mut target.site, dev.site.as_deref());
                    set_if(&mut target.role, dev.role.as_deref());
                    set_if(&mut target.vendor, dev.vendor.as_deref());
                    set_if(&mut target.inventory_platform, dev.platform.as_deref());
                    set_if(&mut target.inventory_status, dev.status.as_deref());
                    set_if(&mut target.inventory_tenant, dev.tenant.as_deref());
                    if changed {
                        let action = format!("api_inventory_import_update:{}", req.source.trim());
                        match state.registry.update_device_with_audit(
                            target.clone(),
                            "api",
                            &action,
                        ) {
                            Ok(_) => {
                                updated += 1;
                                touched.push(target);
                                results.push(inventory_row(dev, "updated", "metadata refreshed"));
                            }
                            Err(e) => {
                                failed += 1;
                                results.push(inventory_row(dev, "failed", format!("{e:#}")));
                            }
                        }
                    } else {
                        already += 1;
                        touched.push(existing);
                        results.push(inventory_row(dev, "already_imported", "unchanged"));
                    }
                } else {
                    // Already-configured operational device: declared
                    // inventory metadata must not silently rewrite
                    // operator-managed fields. Drift is surfaced as a
                    // conflict; matching metadata refreshes only the
                    // inventory_* declared fields.
                    let drift: Vec<&str> = [
                        (
                            "hostname",
                            existing.hostname.as_deref(),
                            Some(dev.name.trim()),
                        ),
                        (
                            "site",
                            existing.site.as_deref(),
                            dev.site.as_deref().map(str::trim),
                        ),
                        (
                            "role",
                            existing.role.as_deref(),
                            dev.role.as_deref().map(str::trim),
                        ),
                        (
                            "vendor",
                            existing.vendor.as_deref(),
                            dev.vendor.as_deref().map(str::trim),
                        ),
                    ]
                    .into_iter()
                    .filter(|(_field, stored, incoming)| {
                        let stored = stored.unwrap_or_default();
                        let incoming = incoming.unwrap_or_default();
                        !incoming.is_empty() && stored != incoming
                    })
                    .map(|(field, _, _)| field)
                    .collect();
                    if !drift.is_empty() {
                        conflicts += 1;
                        results.push(inventory_row(
                            dev,
                            "conflict",
                            format!(
                                "inventory metadata differs for configured device \
                                 ({}); operational configuration preserved",
                                drift.join(", ")
                            ),
                        ));
                        continue;
                    }
                    let mut target = existing.clone();
                    let mut changed = false;
                    let mut set_if = |slot: &mut Option<String>, new: Option<&str>| {
                        let trimmed = new.map(str::trim).filter(|s| !s.is_empty());
                        if let Some(value) = trimmed {
                            if *slot != Some(value.to_string()) {
                                *slot = Some(value.to_string());
                                changed = true;
                            }
                        }
                    };
                    set_if(&mut target.inventory_platform, dev.platform.as_deref());
                    set_if(&mut target.inventory_status, dev.status.as_deref());
                    set_if(&mut target.inventory_tenant, dev.tenant.as_deref());
                    if changed {
                        let action = format!("api_inventory_import_update:{}", req.source.trim());
                        match state.registry.update_device_with_audit(
                            target.clone(),
                            "api",
                            &action,
                        ) {
                            Ok(_) => {
                                updated += 1;
                                touched.push(target);
                                results.push(inventory_row(dev, "updated", "metadata refreshed"));
                            }
                            Err(e) => {
                                failed += 1;
                                results.push(inventory_row(dev, "failed", format!("{e:#}")));
                            }
                        }
                    } else {
                        already += 1;
                        touched.push(existing);
                        results.push(inventory_row(dev, "already_imported", "unchanged"));
                    }
                }
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                failed += 1;
                results.push(inventory_row(
                    dev,
                    "failed",
                    format!("registry lookup: {e:#}"),
                ));
                continue;
            }
        }

        // 2. Canonical address claimed by a different identity? The
        //    graph keys Device nodes by port-stripped address, so an
        //    existing `host:port` entry conflicts with a bare `host`
        //    import — the schema cannot safely distinguish them.
        match state.registry.find_by_canonical_address(&address) {
            Ok(Some(existing)) => {
                conflicts += 1;
                let holder = existing
                    .external_id
                    .clone()
                    .unwrap_or_else(|| "a manually registered device".to_string());
                results.push(inventory_row(
                    dev,
                    "conflict",
                    format!(
                        "address {address} is already registered to {holder} \
                         (as '{}')",
                        existing.address
                    ),
                ));
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                failed += 1;
                results.push(inventory_row(
                    dev,
                    "failed",
                    format!("registry lookup: {e:#}"),
                ));
                continue;
            }
        }

        // 3. New inventory asset: declared only. No credential selection,
        //    no subscription paths, no connectivity attempt.
        let target = TargetConfig {
            address: address.clone(),
            enabled: false,
            inventory_only: true,
            external_id: Some(external_id.clone()),
            hostname: option_string(dev.name.trim().to_string()),
            role: dev
                .role
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            site: dev
                .site
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            vendor: dev
                .vendor
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            inventory_platform: dev
                .platform
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            inventory_status: dev
                .status
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            inventory_tenant: dev
                .tenant
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            ..Default::default()
        };
        match state.registry.add_device_with_audit(
            target.clone(),
            "api",
            &format!("api_inventory_import:{}", req.source.trim()),
        ) {
            Ok(_) => {
                imported += 1;
                touched.push(target);
                results.push(inventory_row(dev, "imported", "inventory only"));
            }
            Err(e) => {
                failed += 1;
                results.push(inventory_row(dev, "failed", format!("{e:#}")));
            }
        }
    }

    // Graph projection (Device nodes + LOCATED_AT site edges) for every
    // successfully registered or refreshed row — including unchanged
    // re-imports, which is what lets a retry reconcile a projection that
    // failed earlier. Registry persistence is already durable at this
    // point; a sync failure is reported explicitly as pending rather
    // than silently swallowed, and the next import retries it.
    let mut graph_sync = "synced".to_string();
    let mut graph_sync_error = String::new();
    if !touched.is_empty() {
        if let Err(e) = state.store.sync_sites_from_targets(touched).await {
            graph_sync = "pending".to_string();
            graph_sync_error = format!("{e:#}");
            tracing::warn!(error = %e, "inventory import graph projection pending");
        }
    }

    Ok(Json(InventoryImportResponse {
        results,
        imported,
        already_present: already,
        updated,
        conflicts,
        invalid,
        failed,
        graph_sync,
        graph_sync_error,
    }))
}

pub(super) async fn remove_impact_handler(
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
pub(super) fn managed_device_json(
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
        selected_paths: target.selected_paths.clone(),
        subscription_statuses: statuses.get(&address).cloned().unwrap_or_default(),
        address,
        resolution_audit: audit,
        external_id: target.external_id.clone().unwrap_or_default(),
        inventory_only: target.inventory_only,
        inventory_platform: target.inventory_platform.unwrap_or_default(),
        inventory_status: target.inventory_status.unwrap_or_default(),
        inventory_tenant: target.inventory_tenant.unwrap_or_default(),
    }
}
pub(super) fn target_from_request(
    req: ManagedDeviceRequest,
) -> Result<TargetConfig, (StatusCode, String)> {
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
        // Preserve an explicit empty string: it is the "clear site"
        // signal, distinct from an omitted field (None → inherit).
        site: req.site.map(|site| site.trim().to_string()),
        collector_id: None,
        selected_paths: req
            .selected_paths
            .into_iter()
            .filter(|path| !path.path.trim().is_empty())
            .collect(),
        extra_ips: Vec::new(),
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
        last_operator_action: String::new(),
        external_id: None,
        inventory_only: false,
        inventory_platform: None,
        inventory_status: None,
        inventory_tenant: None,
        paths: vec![],
        optional: false,
    })
}
pub(super) fn site_json(site: SiteRecord) -> SiteJson {
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
pub(super) fn site_record(site: SiteJson) -> SiteRecord {
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
pub(super) fn credential_json(
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
pub(super) fn credential_device_counts(
    registry: &ApiRegistry,
) -> anyhow::Result<HashMap<String, usize>> {
    let mut counts = HashMap::new();
    for target in registry.list_all_targets()? {
        if let Some(alias) = target.credential_alias {
            *counts.entry(alias).or_insert(0) += 1;
        }
    }
    Ok(counts)
}
pub(super) fn resolve_request_credentials(
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
        (Some(username), Some(password)) => Some(ResolvedCredential {
            username,
            password: zeroize::Zeroizing::new(password),
        }),
        _ => None,
    })
}
// ── D4-17 T2: Device bootstrap via PyATS agent ────────────────────────────────

#[derive(serde::Deserialize)]
pub(super) struct BootstrapRequest {
    address: String,
    #[serde(default)]
    credential_alias: String,
    #[serde(default)]
    vendor: String,
}

pub(super) async fn bootstrap_device_handler(
    State(state): State<AppState>,
    Json(req): Json<BootstrapRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if req.address.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "address is required".into()));
    }
    if req.credential_alias.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "credential_alias is required".into(),
        ));
    }

    // Resolve credentials from vault here in Rust — never ship them over HTTP.
    let cred = state
        .credentials
        .resolve(&req.credential_alias, ResolvePurpose::Discover)
        .map_err(|e| {
            (
                StatusCode::FAILED_DEPENDENCY,
                format!("credential resolve failed: {e}"),
            )
        })?;

    let api_url =
        std::env::var("BONSAI_API_URL").unwrap_or_else(|_| "http://127.0.0.1:3000".to_string());

    // Prefer .venv/bin/python (where PyATS/Genie/paramiko are installed) over system python3
    let python_bin = if std::path::Path::new(".venv/bin/python").exists() {
        ".venv/bin/python"
    } else {
        "python3"
    };
    let mut cmd = tokio::process::Command::new(python_bin);
    cmd.arg("python/bootstrap_agent.py")
        .arg("device")
        .arg("--address")
        .arg(&req.address)
        .arg("--api-url")
        .arg(&api_url)
        // Inject credentials as env vars — never as CLI args (visible in ps) or over HTTP.
        .env("BONSAI_BOOTSTRAP_USERNAME", &cred.username)
        .env("BONSAI_BOOTSTRAP_PASSWORD", &*cred.password);
    if !req.vendor.is_empty() {
        cmd.arg("--vendor").arg(&req.vendor);
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = cmd.output().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to spawn bootstrap agent: {e}"),
        )
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "bootstrap agent failed (exit {}): {}",
                output.status,
                stderr.trim()
            ),
        ));
    }

    let result: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
        serde_json::json!({
            "status": "ok",
            "stdout": stdout.trim(),
            "stderr": stderr.trim(),
        })
    });

    Ok(Json(result))
}

// ── D4-17 T2: Device seed — write pre-seeded graph data from bootstrap ────────

#[derive(serde::Deserialize)]
pub(super) struct DeviceSeedRequest {
    address: String,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    vendor: String,
    #[serde(default)]
    os_version: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    interfaces: Vec<SeedInterface>,
    #[serde(default)]
    bgp_neighbors: Vec<SeedBgpNeighbor>,
    #[serde(default)]
    lldp_neighbors: Vec<SeedLldpNeighbor>,
    #[serde(default)]
    isis_adjacencies: Vec<SeedIsisAdj>,
    #[serde(default)]
    lag_groups: Vec<SeedLagGroup>,
    #[serde(default)]
    vrrp_instances: Vec<SeedVrrpInstance>,
    #[serde(default)]
    routes: Vec<SeedRoute>,
    #[serde(default)]
    arp_entries: Vec<SeedArpEntry>,
    #[serde(default)]
    ospf_neighbors: Vec<SeedOspfNeighbor>,
    #[serde(default)]
    bfd_sessions: Vec<SeedBfdSession>,
    #[serde(default)]
    stp_instances: Vec<SeedStpInstance>,
    #[serde(default)]
    vlans: Vec<SeedVlan>,
    #[serde(default)]
    vrfs: Vec<SeedVrf>,
    #[serde(default)]
    ntp_peers: Vec<SeedNtpPeer>,
    #[serde(default)]
    platform_detail: Option<SeedPlatformDetail>,
    #[serde(default)]
    acl_summaries: Vec<SeedAclSummary>,
    #[serde(default)]
    mpls_lsps: Vec<SeedMplsLsp>,
}

#[derive(serde::Deserialize)]
struct SeedInterface {
    name: String,
    #[serde(default)]
    oper_status: String,
    #[serde(default)]
    admin_status: String,
    #[serde(default)]
    speed: i64,
    #[serde(default)]
    mac: String,
    #[serde(default)]
    description: String,
}

#[derive(serde::Deserialize)]
struct SeedBgpNeighbor {
    peer_address: String,
    #[serde(default)]
    peer_as: i64,
    #[serde(default)]
    state: String,
    #[serde(default)]
    vrf: String,
}

#[derive(serde::Deserialize)]
struct SeedLldpNeighbor {
    local_interface: String,
    #[serde(default)]
    remote_port: String,
    #[serde(default)]
    remote_device: String,
}

#[derive(serde::Deserialize)]
struct SeedIsisAdj {
    system_id: String,
    #[serde(default)]
    interface: String,
    #[serde(default)]
    state: String,
}

#[derive(serde::Deserialize)]
struct SeedLagGroup {
    name: String,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    oper_status: String,
    #[serde(default)]
    protocol: String,
    #[serde(default)]
    min_links: i64,
}

#[derive(serde::Deserialize)]
struct SeedVrrpInstance {
    #[serde(default)]
    group_id: i64,
    #[serde(default)]
    interface: String,
    #[serde(default)]
    virtual_ip: String,
    #[serde(default)]
    state: String,
    #[serde(default = "default_vrrp_priority")]
    priority: i64,
    #[serde(default)]
    protocol: String,
}

fn default_vrrp_priority() -> i64 {
    100
}

#[derive(serde::Deserialize)]
struct SeedRoute {
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    next_hops: Vec<String>,
    #[serde(default)]
    protocol: String,
    #[serde(default)]
    metric: i64,
    #[serde(default)]
    is_ecmp: bool,
}

#[derive(serde::Deserialize)]
struct SeedArpEntry {
    #[serde(default)]
    ip_address: String,
    #[serde(default)]
    mac_address: String,
    #[serde(default)]
    interface: String,
    #[serde(default)]
    state: String,
}

#[derive(serde::Deserialize)]
struct SeedOspfNeighbor {
    #[serde(default)]
    neighbor_id: String,
    #[serde(default)]
    interface: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    area: String,
    #[serde(default)]
    dr: String,
    #[serde(default)]
    bdr: String,
    #[serde(default)]
    priority: i64,
}

#[derive(serde::Deserialize)]
struct SeedBfdSession {
    #[serde(default)]
    peer_address: String,
    #[serde(default)]
    interface: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    protocol: String,
    #[serde(default)]
    local_diag: String,
    #[serde(default = "default_bfd_mult")]
    detect_multiplier: i64,
    #[serde(default)]
    interval_ms: i64,
}

fn default_bfd_mult() -> i64 {
    3
}

#[derive(serde::Deserialize)]
struct SeedStpInstance {
    #[serde(default)]
    vlan_id: i64,
    #[serde(default)]
    instance: String,
    #[serde(default)]
    root_bridge: String,
    #[serde(default)]
    root_port: String,
    #[serde(default = "default_stp_priority")]
    bridge_priority: i64,
    #[serde(default)]
    is_root: bool,
    #[serde(default)]
    topology_changes: i64,
    #[serde(default)]
    protocol: String,
}

fn default_stp_priority() -> i64 {
    32768
}

#[derive(serde::Deserialize)]
struct SeedVlan {
    #[serde(default)]
    vlan_id: i64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    state: String,
    #[serde(default)]
    interfaces: Vec<String>,
}

#[derive(serde::Deserialize)]
struct SeedVrf {
    #[serde(default)]
    name: String,
    #[serde(default)]
    rd: String,
    #[serde(default)]
    rt_import: Vec<String>,
    #[serde(default)]
    rt_export: Vec<String>,
    #[serde(default)]
    interfaces: Vec<String>,
    #[serde(default)]
    address_families: Vec<String>,
}

#[derive(serde::Deserialize)]
struct SeedNtpPeer {
    #[serde(default)]
    peer_address: String,
    #[serde(default = "default_ntp_stratum")]
    stratum: i64,
    #[serde(default)]
    state: String,
    #[serde(default)]
    offset_ms: f64,
    #[serde(default)]
    reach: i64,
    #[serde(default)]
    ref_id: String,
    #[serde(default)]
    is_synchronized: bool,
}

fn default_ntp_stratum() -> i64 {
    16
}

#[derive(serde::Deserialize)]
struct SeedPlatformDetail {
    #[serde(default)]
    model: String,
    #[serde(default)]
    serial: String,
    #[serde(default)]
    cpu_util_pct: f64,
    #[serde(default)]
    memory_used_mb: f64,
    #[serde(default)]
    memory_total_mb: f64,
    #[serde(default)]
    uptime_seconds: i64,
    #[serde(default)]
    boot_image: String,
    #[serde(default)]
    hardware_rev: String,
    #[serde(default)]
    slot_inventory: Vec<serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct SeedAclSummary {
    #[serde(default)]
    name: String,
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    ace_count: i64,
    #[serde(default)]
    applied_interfaces: Vec<String>,
    #[serde(default)]
    total_matches: i64,
}

#[derive(serde::Deserialize)]
struct SeedMplsLsp {
    #[serde(default)]
    name: String,
    #[serde(default)]
    destination: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    in_label: i64,
    #[serde(default)]
    out_label: i64,
    #[serde(default)]
    out_interface: String,
    #[serde(default)]
    next_hop: String,
    #[serde(default)]
    protocol: String,
}

pub(super) async fn device_seed_handler(
    State(state): State<AppState>,
    Json(req): Json<DeviceSeedRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if req.address.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "address is required".into()));
    }
    let db = state.store.db();
    let address = req.address.clone();
    let source = if req.source.is_empty() {
        "bootstrap".to_string()
    } else {
        req.source.clone()
    };
    let now = super::now_ns();

    let iface_count = req.interfaces.len();
    let bgp_count = req.bgp_neighbors.len();
    let lldp_count = req.lldp_neighbors.len();
    let isis_count = req.isis_adjacencies.len();
    let lag_count = req.lag_groups.len();
    let vrrp_count = req.vrrp_instances.len();
    let route_count = req.routes.len();
    let arp_count = req.arp_entries.len();
    let ospf_count = req.ospf_neighbors.len();
    let bfd_count = req.bfd_sessions.len();
    let stp_count = req.stp_instances.len();
    let vlan_count = req.vlans.len();
    let vrf_count = req.vrfs.len();
    let ntp_count = req.ntp_peers.len();
    let has_platform = req.platform_detail.is_some();
    let acl_count = req.acl_summaries.len();
    let mpls_count = req.mpls_lsps.len();

    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

        // Upsert Device node with bootstrap metadata
        conn.query(&format!(
            "MERGE (d:Device {{address: '{}'}}) \
             SET d.hostname = '{}', d.vendor = '{}', d.os_version = '{}', \
                 d.bootstrap_source = '{}', d.bootstrap_at_ns = {}",
            address, req.hostname, req.vendor, req.os_version, source, now,
        ))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

        // Seed interfaces
        // IMPORTANT: we set i.id = 'address:name' to match the key used by
        // emit_oper_status_event() in graph/mod.rs.  Without this the gNMI path
        // sees previous_oper_status = None for every interface on the first poll
        // and fires a false interface_down storm immediately after bootstrap.
        for iface in &req.interfaces {
            let iface_id = format!("{}:{}", address, iface.name);
            conn.query(&format!(
                "MERGE (i:Interface {{id: '{}'}}) \
                 ON CREATE SET \
                   i.device_address = '{}', i.name = '{}', \
                   i.oper_status = '{}', i.admin_status = '{}', i.speed = {}, \
                   i.mac = '{}', i.description = '{}', i.source = '{}', i.updated_at_ns = {} \
                 ON MATCH SET \
                   i.oper_status = '{}', i.admin_status = '{}', i.speed = {}, \
                   i.mac = '{}', i.description = '{}', i.source = '{}', i.updated_at_ns = {}",
                iface_id,
                address,
                iface.name,
                iface.oper_status,
                iface.admin_status,
                iface.speed,
                iface.mac,
                iface.description,
                source,
                now,
                iface.oper_status,
                iface.admin_status,
                iface.speed,
                iface.mac,
                iface.description,
                source,
                now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            // Ensure HAS_INTERFACE edge exists
            conn.query(&format!(
                "MATCH (d:Device {{address: '{}'}}), (i:Interface {{id: '{}'}}) \
                 MERGE (d)-[:HAS_INTERFACE]->(i)",
                address, iface_id,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        // Seed BGP neighbors
        for bgp in &req.bgp_neighbors {
            conn.query(&format!(
                "MERGE (b:BgpSession {{device_address: '{}', peer_address: '{}'}}) \
                 SET b.peer_as = {}, b.state = '{}', b.vrf = '{}', \
                     b.source = '{}', b.updated_at_ns = {}",
                address, bgp.peer_address, bgp.peer_as, bgp.state, bgp.vrf, source, now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        // Seed LLDP neighbors
        for lldp in &req.lldp_neighbors {
            conn.query(&format!(
                "MATCH (d:Device {{address: '{}'}}) \
                 MERGE (i:Interface {{device_address: '{}', name: '{}'}}) \
                 SET i.lldp_remote_port = '{}', i.lldp_remote_device = '{}', \
                     i.source = '{}', i.updated_at_ns = {}",
                address,
                address,
                lldp.local_interface,
                lldp.remote_port,
                lldp.remote_device,
                source,
                now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        // Seed IS-IS adjacencies
        for isis in &req.isis_adjacencies {
            conn.query(&format!(
                "MERGE (a:IsIsAdj {{device_address: '{}', system_id: '{}', interface: '{}'}}) \
                 SET a.state = '{}', a.source = '{}', a.updated_at_ns = {}",
                address, isis.system_id, isis.interface, isis.state, source, now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        // D4-12 T2: Seed LAG groups → RedundancyGroup(type=lag) + MEMBER_OF edges
        for lag in &req.lag_groups {
            let rg_id = format!("lag-{}-{}", address, lag.name);
            let member_count = lag.members.len() as i64;
            conn.query(&format!(
                "MERGE (rg:RedundancyGroup {{id: '{}'}}) \
                 SET rg.type = 'lag', rg.name = '{}', rg.member_count = {}, \
                     rg.original_member_count = {}, rg.oper_status = '{}', \
                     rg.protocol = '{}', rg.min_links = {}, \
                     rg.protects_node_id = '{}', rg.source = '{}', rg.updated_at_ns = {}",
                rg_id,
                lag.name,
                member_count,
                member_count,
                lag.oper_status,
                lag.protocol,
                lag.min_links,
                address,
                source,
                now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            // Link each member interface
            for member_iface in &lag.members {
                conn.query(&format!(
                    "MATCH (i:Interface {{device_address: '{}', name: '{}'}}) \
                     MATCH (rg:RedundancyGroup {{id: '{}'}}) \
                     MERGE (i)-[:MEMBER_OF {{role: 'member', updated_at_ns: {}}}]->(rg)",
                    address, member_iface, rg_id, now,
                ))
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            }
            // Link lag interface itself to device
            conn.query(&format!(
                "MERGE (i:Interface {{device_address: '{}', name: '{}'}}) \
                 SET i.is_lag = true, i.lag_members = '{}', i.source = '{}', i.updated_at_ns = {}",
                address,
                lag.name,
                lag.members.join(","),
                source,
                now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        // D4-12 T2: Seed VRRP instances → RedundancyGroup(type=vrrp|hsrp)
        for vrrp in &req.vrrp_instances {
            let rg_id = format!(
                "{}-{}-{}-{}",
                vrrp.protocol, address, vrrp.interface, vrrp.group_id
            );
            conn.query(&format!(
                "MERGE (rg:RedundancyGroup {{id: '{}'}}) \
                 SET rg.type = '{}', rg.name = '{} group {} on {}', \
                     rg.virtual_ip = '{}', rg.state = '{}', rg.priority = {}, \
                     rg.member_count = 1, rg.original_member_count = 1, \
                     rg.protects_node_id = '{}', rg.source = '{}', rg.updated_at_ns = {}",
                rg_id,
                vrrp.protocol,
                vrrp.protocol,
                vrrp.group_id,
                vrrp.interface,
                vrrp.virtual_ip,
                vrrp.state,
                vrrp.priority,
                address,
                source,
                now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            // Link device to this VRRP group
            conn.query(&format!(
                "MATCH (d:Device {{address: '{}'}}) \
                 MATCH (rg:RedundancyGroup {{id: '{}'}}) \
                 MERGE (d)-[:MEMBER_OF {{role: '{}', interface: '{}', updated_at_ns: {}}}]->(rg)",
                address, rg_id, vrrp.state, vrrp.interface, now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        // D4-12 T2: Seed ECMP routes → RedundancyGroup(type=ecmp) for multi-next-hop
        for route in &req.routes {
            if !route.is_ecmp || route.next_hops.len() < 2 {
                continue;
            }
            let rg_id = format!("ecmp-{}-{}", address, route.prefix);
            let nh_count = route.next_hops.len() as i64;
            conn.query(&format!(
                "MERGE (rg:RedundancyGroup {{id: '{}'}}) \
                 SET rg.type = 'ecmp', rg.name = 'ECMP {}', \
                     rg.prefix = '{}', rg.protocol = '{}', rg.metric = {}, \
                     rg.member_count = {}, rg.original_member_count = {}, \
                     rg.protects_node_id = '{}', rg.source = '{}', rg.updated_at_ns = {}",
                rg_id,
                route.prefix,
                route.prefix,
                route.protocol,
                route.metric,
                nh_count,
                nh_count,
                address,
                source,
                now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            // Link each next-hop device (if it exists in graph)
            for nh in &route.next_hops {
                conn.query(&format!(
                    "OPTIONAL MATCH (d:Device {{address: '{}'}}) \
                     WITH d WHERE d IS NOT NULL \
                     MATCH (rg:RedundancyGroup {{id: '{}'}}) \
                     MERGE (d)-[:MEMBER_OF {{role: 'next_hop', updated_at_ns: {}}}]->(rg)",
                    nh, rg_id, now,
                ))
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            }
        }

        // D4-12 T2: Seed ARP entries (for dual-homed host detection)
        for arp in &req.arp_entries {
            if arp.ip_address.is_empty() || arp.mac_address.is_empty() {
                continue;
            }
            conn.query(&format!(
                "MERGE (ae:ArpEntry {{device_address: '{}', ip_address: '{}'}}) \
                 SET ae.mac_address = '{}', ae.interface = '{}', ae.state = '{}', \
                     ae.source = '{}', ae.updated_at_ns = {}",
                address, arp.ip_address, arp.mac_address, arp.interface, arp.state, source, now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        // Seed OSPF neighbors
        for ospf in &req.ospf_neighbors {
            if ospf.neighbor_id.is_empty() {
                continue;
            }
            let ospf_id = format!("ospf-{}-{}-{}", address, ospf.neighbor_id, ospf.interface);
            conn.query(&format!(
                "MERGE (o:OspfNeighbor {{id: '{}'}}) \
                 SET o.device_address = '{}', o.neighbor_id = '{}', o.interface = '{}', \
                     o.state = '{}', o.area = '{}', o.dr = '{}', o.bdr = '{}', \
                     o.priority = {}, o.source = '{}', o.updated_at_ns = {}",
                ospf_id,
                address,
                ospf.neighbor_id,
                ospf.interface,
                ospf.state,
                ospf.area,
                ospf.dr,
                ospf.bdr,
                ospf.priority,
                source,
                now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        // Seed BFD sessions → BfdSession node (already exists in schema)
        for bfd in &req.bfd_sessions {
            if bfd.peer_address.is_empty() {
                continue;
            }
            let bfd_id = format!("bfd-{}-{}", address, bfd.peer_address);
            conn.query(&format!(
                "MERGE (b:BfdSession {{id: '{}'}}) \
                 SET b.device_address = '{}', b.peer_address = '{}', \
                     b.if_name = '{}', b.session_state = '{}', \
                     b.registered_protocols = '{}', b.local_diag = '{}', \
                     b.detect_multiplier = {}, b.interval_ms = {}, \
                     b.source = '{}', b.updated_at_ns = {}",
                bfd_id,
                address,
                bfd.peer_address,
                bfd.interface,
                bfd.state,
                bfd.protocol,
                bfd.local_diag,
                bfd.detect_multiplier,
                bfd.interval_ms,
                source,
                now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            // HAS_BFD_SESSION edge
            conn.query(&format!(
                "MATCH (d:Device {{address: '{}'}}) \
                 MATCH (b:BfdSession {{id: '{}'}}) \
                 MERGE (d)-[:HAS_BFD_SESSION]->(b)",
                address, bfd_id,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        // Seed STP instances
        for stp in &req.stp_instances {
            let stp_id = format!("stp-{}-{}", address, stp.instance);
            conn.query(&format!(
                "MERGE (s:StpInstance {{id: '{}'}}) \
                 SET s.device_address = '{}', s.vlan_id = {}, s.instance = '{}', \
                     s.root_bridge = '{}', s.root_port = '{}', \
                     s.bridge_priority = {}, s.is_root = {}, \
                     s.topology_changes = {}, s.protocol = '{}', \
                     s.source = '{}', s.updated_at_ns = {}",
                stp_id,
                address,
                stp.vlan_id,
                stp.instance,
                stp.root_bridge,
                stp.root_port,
                stp.bridge_priority,
                stp.is_root,
                stp.topology_changes,
                stp.protocol,
                source,
                now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        // Seed VLANs → VLAN node (already exists in schema) + ACCESS_VLAN edges
        for vlan in &req.vlans {
            if vlan.vlan_id == 0 {
                continue;
            }
            let vlan_id_str = format!("vlan-{}", vlan.vlan_id);
            conn.query(&format!(
                "MERGE (v:VLAN {{id: '{}'}}) \
                 SET v.vid = {}, v.name = '{}', v.source_name = '{}', v.updated_at_ns = {}",
                vlan_id_str, vlan.vlan_id, vlan.name, source, now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            // Link member interfaces
            for iface_name in &vlan.interfaces {
                conn.query(&format!(
                    "MATCH (i:Interface {{device_address: '{}', name: '{}'}}) \
                     MATCH (v:VLAN {{id: '{}'}}) \
                     MERGE (i)-[:ACCESS_VLAN]->(v)",
                    address, iface_name, vlan_id_str,
                ))
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            }
        }

        // Seed VRFs
        for vrf in &req.vrfs {
            if vrf.name.is_empty() {
                continue;
            }
            let vrf_id = format!("vrf-{}-{}", address, vrf.name);
            let rt_import_json = serde_json::to_string(&vrf.rt_import).unwrap_or_default();
            let rt_export_json = serde_json::to_string(&vrf.rt_export).unwrap_or_default();
            let ifaces_json = serde_json::to_string(&vrf.interfaces).unwrap_or_default();
            let afs_json = serde_json::to_string(&vrf.address_families).unwrap_or_default();
            conn.query(&format!(
                "MERGE (v:Vrf {{id: '{}'}}) \
                 SET v.device_address = '{}', v.name = '{}', v.rd = '{}', \
                     v.rt_import_json = '{}', v.rt_export_json = '{}', \
                     v.interfaces_json = '{}', v.address_families_json = '{}', \
                     v.source = '{}', v.updated_at_ns = {}",
                vrf_id,
                address,
                vrf.name,
                vrf.rd,
                rt_import_json.replace('\'', "\\'"),
                rt_export_json.replace('\'', "\\'"),
                ifaces_json.replace('\'', "\\'"),
                afs_json.replace('\'', "\\'"),
                source,
                now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        // Seed NTP peers
        for ntp in &req.ntp_peers {
            if ntp.peer_address.is_empty() {
                continue;
            }
            let ntp_id = format!("ntp-{}-{}", address, ntp.peer_address);
            conn.query(&format!(
                "MERGE (n:NtpPeer {{id: '{}'}}) \
                 SET n.device_address = '{}', n.peer_address = '{}', \
                     n.stratum = {}, n.state = '{}', n.offset_ms = {}, \
                     n.reach = {}, n.ref_id = '{}', n.is_synchronized = {}, \
                     n.source = '{}', n.updated_at_ns = {}",
                ntp_id,
                address,
                ntp.peer_address,
                ntp.stratum,
                ntp.state,
                ntp.offset_ms,
                ntp.reach,
                ntp.ref_id,
                ntp.is_synchronized,
                source,
                now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        // Seed platform detail → enrich Device node
        if let Some(ref plat) = req.platform_detail {
            let slots_json = serde_json::to_string(&plat.slot_inventory).unwrap_or_default();
            conn.query(&format!(
                "MERGE (d:Device {{address: '{}'}}) \
                 SET d.model = '{}', d.serial_number = '{}', \
                     d.cpu_util_pct = {}, d.memory_used_mb = {}, d.memory_total_mb = {}, \
                     d.uptime_seconds = {}, d.boot_image = '{}', \
                     d.hardware_rev = '{}', d.slot_inventory_json = '{}'",
                address,
                plat.model,
                plat.serial,
                plat.cpu_util_pct,
                plat.memory_used_mb,
                plat.memory_total_mb,
                plat.uptime_seconds,
                plat.boot_image,
                plat.hardware_rev,
                slots_json.replace('\'', "\\'"),
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        // Seed ACL summaries
        for acl in &req.acl_summaries {
            if acl.name.is_empty() {
                continue;
            }
            let acl_id = format!("acl-{}-{}", address, acl.name);
            let ifaces_json = serde_json::to_string(&acl.applied_interfaces).unwrap_or_default();
            conn.query(&format!(
                "MERGE (a:AclSummary {{id: '{}'}}) \
                 SET a.device_address = '{}', a.name = '{}', a.acl_type = '{}', \
                     a.ace_count = {}, a.applied_interfaces_json = '{}', \
                     a.total_matches = {}, a.source = '{}', a.updated_at_ns = {}",
                acl_id,
                address,
                acl.name,
                acl.r#type,
                acl.ace_count,
                ifaces_json.replace('\'', "\\'"),
                acl.total_matches,
                source,
                now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        // Seed MPLS LSPs
        for lsp in &req.mpls_lsps {
            if lsp.name.is_empty() && lsp.destination.is_empty() {
                continue;
            }
            let lsp_id = format!(
                "mpls-{}-{}",
                address,
                if lsp.name.is_empty() {
                    &lsp.destination
                } else {
                    &lsp.name
                }
            );
            conn.query(&format!(
                "MERGE (m:MplsLsp {{id: '{}'}}) \
                 SET m.device_address = '{}', m.name = '{}', m.destination = '{}', \
                     m.state = '{}', m.in_label = {}, m.out_label = {}, \
                     m.out_interface = '{}', m.next_hop = '{}', m.protocol = '{}', \
                     m.source = '{}', m.updated_at_ns = {}",
                lsp_id,
                address,
                lsp.name,
                lsp.destination,
                lsp.state,
                lsp.in_label,
                lsp.out_label,
                lsp.out_interface,
                lsp.next_hop,
                lsp.protocol,
                source,
                now,
            ))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        Ok::<_, (StatusCode, String)>(())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(Json(serde_json::json!({
        "success": true,
        "address": req.address,
        "seeded": {
            "interfaces": iface_count,
            "bgp_neighbors": bgp_count,
            "lldp_neighbors": lldp_count,
            "isis_adjacencies": isis_count,
            "lag_groups": lag_count,
            "vrrp_instances": vrrp_count,
            "ecmp_routes": route_count,
            "arp_entries": arp_count,
            "ospf_neighbors": ospf_count,
            "bfd_sessions": bfd_count,
            "stp_instances": stp_count,
            "vlans": vlan_count,
            "vrfs": vrf_count,
            "ntp_peers": ntp_count,
            "platform_detail": has_platform,
            "acl_summaries": acl_count,
            "mpls_lsps": mpls_count,
        }
    })))
}

// ── D4-17 T3: Bulk seed from file ────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub(super) struct BulkBootstrapRequest {
    devices: Vec<BootstrapRequest>,
    #[serde(default = "default_parallel")]
    parallel: usize,
}

fn default_parallel() -> usize {
    4
}

pub(super) async fn bulk_bootstrap_handler(
    State(state): State<AppState>,
    Json(req): Json<BulkBootstrapRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if req.devices.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "devices list is empty".into()));
    }

    let api_url =
        std::env::var("BONSAI_API_URL").unwrap_or_else(|_| "http://127.0.0.1:3000".to_string());

    // Resolve all credentials from vault here in Rust before writing the seed file.
    // Python reads username/password directly from the file — no HTTP resolve call needed.
    let mut seed_entries: Vec<serde_json::Value> = Vec::with_capacity(req.devices.len());
    for d in &req.devices {
        let (username, password) = if !d.credential_alias.is_empty() {
            match state
                .credentials
                .resolve(&d.credential_alias, ResolvePurpose::Discover)
            {
                Ok(c) => (c.username.clone(), c.password.to_string()),
                Err(e) => {
                    return Err((
                        StatusCode::FAILED_DEPENDENCY,
                        format!(
                            "credential resolve failed for '{}' (alias {}): {e}",
                            d.address, d.credential_alias
                        ),
                    ));
                }
            }
        } else {
            (String::new(), String::new())
        };
        seed_entries.push(serde_json::json!({
            "address": d.address,
            "username": username,
            "password": password,
            "vendor": d.vendor,
        }));
    }

    let tmp_file = format!("/tmp/bonsai_bulk_bootstrap_{}.yaml", super::now_ns());
    let yaml_content = serde_yaml::to_string(&serde_json::json!({ "devices": seed_entries }))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("yaml serialize: {e}"),
            )
        })?;
    tokio::fs::write(&tmp_file, &yaml_content)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("write temp seed file: {e}"),
            )
        })?;

    let python_bin = if std::path::Path::new(".venv/bin/python").exists() {
        ".venv/bin/python"
    } else {
        "python3"
    };
    let mut cmd = tokio::process::Command::new(python_bin);
    cmd.arg("python/bootstrap_agent.py")
        .arg("seed")
        .arg("--seed-file")
        .arg(&tmp_file)
        .arg("--api-url")
        .arg(&api_url)
        .arg("--parallel")
        .arg(req.parallel.min(8).to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = cmd.output().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to spawn bootstrap agent: {e}"),
        )
    })?;

    // Cleanup temp file (best-effort)
    let _ = tokio::fs::remove_file(&tmp_file).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "bulk bootstrap failed (exit {}): {}",
                output.status,
                stderr.trim()
            ),
        ));
    }

    let result: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
        serde_json::json!({
            "status": "ok",
            "stdout": stdout.trim(),
        })
    });

    Ok(Json(result))
}

/// GET /api/credentials/{alias}/resolve — resolve a credential alias to username+password.
/// Used by bootstrap_agent.py. Requires the vault to be unlocked.
/// Never called with a POST to avoid credentials appearing in request logs.
pub(super) async fn resolve_credential_handler(
    State(state): State<AppState>,
    axum::extract::Path(alias): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cred = state
        .credentials
        .resolve(&alias, ResolvePurpose::Discover)
        .map_err(|e| {
            (
                StatusCode::NOT_FOUND,
                format!("credential resolve failed: {e}"),
            )
        })?;
    Ok(Json(serde_json::json!({
        "alias": alias,
        "username": cred.username,
        "password": cred.password_string(),
    })))
}

pub(super) fn site_subtree_ids(
    sites: &[SiteRecord],
    root_id: &str,
) -> std::collections::HashSet<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{GraphStore, SiteRecord};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use lbug::{Connection, Value};
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_site(id: &str, name: &str) -> SiteRecord {
        SiteRecord {
            id: id.to_string(),
            name: name.to_string(),
            parent_id: String::new(),
            kind: "dc".to_string(),
            lat: 0.0,
            lon: 0.0,
            metadata_json: "{}".to_string(),
            environment_id: String::new(),
        }
    }

    fn located_at(store: &GraphStore, device_address: &str) -> Vec<String> {
        let db = store.db();
        let conn = Connection::new(&db).expect("graph connection");
        let mut stmt = conn
            .prepare("MATCH (d:Device {address: $addr})-[:LOCATED_AT]->(s:Site) RETURN s.id")
            .expect("prepare LOCATED_AT query");
        conn.execute(
            &mut stmt,
            vec![("addr", Value::String(device_address.to_string()))],
        )
        .expect("LOCATED_AT query")
        .into_iter()
        .map(|row| read_str(&row[0]))
        .collect()
    }

    async fn post_device(
        router: &axum::Router,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::post("/api/onboarding/devices")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("build request");
        let response = router
            .clone()
            .oneshot(request)
            .await
            .expect("oneshot response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    /// BN-015-U2/B3: exercise the REAL managed-device save/update API
    /// (axum route → serde → save_managed_device → registry →
    /// sync_sites_from_targets → graph) rather than GraphStore alone.
    #[tokio::test]
    async fn managed_device_api_site_claim_lifecycle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph_path = dir.path().join("graph").to_string_lossy().into_owned();
        let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("open"));
        let state = AppState::new_minimal_with_runtime_dir(
            Arc::clone(&store),
            dir.path().to_str().expect("utf8 tempdir"),
        );
        let router = crate::http_server::managed_device_routes().with_state(state);

        // Seed sites through the site admin surface (store.upsert_site is
        // the same path /api/sites uses).
        store
            .upsert_site(test_site("site-a", "Site A"))
            .await
            .expect("site a");
        for id in ["shared-1", "shared-2"] {
            store
                .upsert_site(test_site(id, "Shared"))
                .await
                .expect("shared site");
        }

        // 1. Create with a resolvable site claim.
        let (status, body) = post_device(
            &router,
            serde_json::json!({"address": "10.9.9.1", "hostname": "r1", "site": "Site A"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], true, "save failed: {body}");
        assert_eq!(located_at(&store, "10.9.9.1"), vec!["site-a"]);

        // 2. Partial update omitting `site` preserves the association.
        let (status, body) = post_device(
            &router,
            serde_json::json!({"address": "10.9.9.1", "hostname": "r1-renamed"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], true, "update failed: {body}");
        assert_eq!(
            located_at(&store, "10.9.9.1"),
            vec!["site-a"],
            "omitted site field must preserve the existing LOCATED_AT"
        );

        // 3. Move to an ambiguous site name: stale edge is dropped.
        let (status, body) = post_device(
            &router,
            serde_json::json!({"address": "10.9.9.1", "site": "Shared"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], true, "ambiguous save failed: {body}");
        assert!(
            located_at(&store, "10.9.9.1").is_empty(),
            "ambiguous claim must not leave the stale Site-A edge"
        );

        // 4. Corrected assignment resolves again through the API.
        let (status, body) = post_device(
            &router,
            serde_json::json!({"address": "10.9.9.1", "site": "Site A"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], true);
        assert_eq!(located_at(&store, "10.9.9.1"), vec!["site-a"]);

        // 5. Explicit clear via "site": "" drops edge and claim mirror.
        let (status, body) = post_device(
            &router,
            serde_json::json!({"address": "10.9.9.1", "site": ""}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], true, "clear failed: {body}");
        assert!(located_at(&store, "10.9.9.1").is_empty());
        let db = store.db();
        let conn = Connection::new(&db).expect("graph connection");
        let mut stmt = conn
            .prepare("MATCH (d:Device {address: $addr}) RETURN d.site")
            .expect("prepare d.site query");
        let rows: Vec<Vec<Value>> = conn
            .execute(
                &mut stmt,
                vec![("addr", Value::String("10.9.9.1".to_string()))],
            )
            .expect("d.site query")
            .into_iter()
            .collect();
        assert!(
            rows.iter().all(|row| read_str(&row[0]).is_empty()),
            "explicit clear must blank the mirrored d.site claim"
        );
    }

    // ── BN-015-U1: inventory-only NetBox import ─────────────────────────────

    async fn post_inventory(
        router: &axum::Router,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::post("/api/onboarding/inventory/import")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("build request");
        let response = router
            .clone()
            .oneshot(request)
            .await
            .expect("oneshot response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    fn nb_device(ext: &str, addr: &str, name: &str) -> serde_json::Value {
        serde_json::json!({
            "external_id": ext,
            "name": name,
            "address": addr,
            "site": "Site A",
            "role": "leaf",
            "vendor": "Nokia",
        })
    }

    #[tokio::test]
    async fn inventory_import_requires_operator_via_authenticated_router() {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph_path = dir.path().join("graph").to_string_lossy().into_owned();
        let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("open"));
        let state = AppState::new_minimal_with_runtime_dir(
            Arc::clone(&store),
            dir.path().to_str().expect("utf8 tempdir"),
        );
        // The REAL production router path: auth_middleware enforces
        // JWT/API-key RBAC on every /api/* route (fail-closed by default).
        let router = crate::http_server::router_from_state(state);

        let payload = serde_json::json!({
            "source": "netbox",
            "devices": [nb_device("netbox:https:nb:443:device:5", "10.40.0.1", "r-authz")],
        })
        .to_string();
        let post = |token: Option<&str>| {
            let mut req = Request::post("/api/onboarding/inventory/import")
                .header("content-type", "application/json");
            if let Some(t) = token {
                req = req.header("authorization", format!("Bearer {t}"));
            }
            req.body(Body::from(payload.clone())).expect("request")
        };
        let status_of = |req: Request<Body>| {
            let router = router.clone();
            async move {
                router
                    .oneshot(req)
                    .await
                    .expect("oneshot")
                    .status()
            }
        };

        // Unauthenticated → 401; viewer → 403 (import is an Operator write).
        assert_eq!(status_of(post(None)).await, StatusCode::UNAUTHORIZED);
        let viewer = crate::http_server::mint_jwt("inv-viewer", "viewer", &store)
            .await
            .expect("viewer jwt");
        assert_eq!(
            status_of(post(Some(&viewer))).await,
            StatusCode::FORBIDDEN
        );

        // Operator → 200 and the row lands inventory-only.
        let operator = crate::http_server::mint_jwt("inv-operator", "operator", &store)
            .await
            .expect("operator jwt");
        let response = router
            .clone()
            .oneshot(post(Some(&operator)))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(body["imported"], 1, "{body}");

        // The NetBox preview route is equally protected: a viewer token
        // must not reach the handler (which would contact NetBox).
        let preview_req = Request::post("/api/enrichment/netbox/import")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {viewer}"))
            .body(Body::from(
                serde_json::json!({"url": "http://127.0.0.1:1", "token": "sekrit"})
                    .to_string(),
            ))
            .expect("request");
        assert_eq!(
            router.clone().oneshot(preview_req).await.expect("oneshot").status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn inventory_import_creates_declared_only_assets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph_path = dir.path().join("graph").to_string_lossy().into_owned();
        let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("open"));
        let state = AppState::new_minimal_with_runtime_dir(
            Arc::clone(&store),
            dir.path().to_str().expect("utf8 tempdir"),
        );
        let router = crate::http_server::managed_device_routes().with_state(state);

        let (status, body) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [
                    nb_device("netbox:device:1", "10.20.0.1", "r1"),
                    nb_device("netbox:device:2", "10.20.0.2", "r2"),
                ],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["imported"], 2, "{body}");
        assert_eq!(body["results"][0]["outcome"], "imported");

        // Per-row idempotence: same external ids again → already_imported.
        let (_, body2) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:device:1", "10.20.0.1", "r1")],
            }),
        )
        .await;
        assert_eq!(body2["already_present"], 1, "{body2}");
        assert_eq!(body2["results"][0]["outcome"], "already_imported");

        // Conflict: same external id bound to a different address.
        let (_, body3) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:device:1", "10.20.0.9", "r1-moved")],
            }),
        )
        .await;
        assert_eq!(body3["conflicts"], 1, "{body3}");
        assert_eq!(body3["results"][0]["outcome"], "conflict");

        // Conflict: different external id claiming an imported address.
        let (_, body4) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:device:99", "10.20.0.1", "impostor")],
            }),
        )
        .await;
        assert_eq!(body4["conflicts"], 1, "{body4}");

        // Invalid: missing address / missing external id.
        let (_, body5) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [
                    {"external_id": "netbox:device:7", "name": "noip", "address": ""},
                    {"external_id": "", "name": "noid", "address": "10.20.0.7"},
                ],
            }),
        )
        .await;
        assert_eq!(body5["invalid"], 2, "{body5}");

        // Verify persisted state through the managed-devices API:
        // enabled=false, inventory_only=true, no credential, no paths.
        let request = Request::get("/api/onboarding/devices")
            .body(Body::empty())
            .expect("build request");
        let response = router.oneshot(request).await.expect("oneshot");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        let devices = json["devices"].as_array().expect("devices array");
        let r1 = devices
            .iter()
            .find(|d| d["address"] == "10.20.0.1")
            .expect("r1 listed");
        assert_eq!(r1["enabled"], false, "{r1}");
        assert_eq!(r1["inventory_only"], true, "{r1}");
        assert_eq!(r1["external_id"], "netbox:device:1", "{r1}");
        assert_eq!(r1["credential_alias"], "", "{r1}");
        assert_eq!(r1["selected_paths"].as_array().unwrap().len(), 0, "{r1}");
        assert_eq!(
            r1["subscription_statuses"].as_array().unwrap().len(),
            0,
            "{r1}"
        );
    }

    #[tokio::test]
    async fn inventory_import_preserves_existing_device_and_restarts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = dir.path().to_str().expect("utf8 tempdir").to_string();
        let graph_path = dir.path().join("graph").to_string_lossy().into_owned();
        let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("open"));

        // Existing ACTIVELY configured device at the same address.
        {
            let state = AppState::new_minimal_with_runtime_dir(Arc::clone(&store), &runtime);
            let router = crate::http_server::managed_device_routes().with_state(state);
            let (_, body) = post_device(
                &router,
                serde_json::json!({
                    "address": "10.20.1.1",
                    "hostname": "active-r1",
                    "enabled": true,
                    "vendor": "Cisco",
                }),
            )
            .await;
            assert_eq!(body["success"], true, "{body}");
        }

        // Import must conflict, not overwrite the active device.
        let state2 = AppState::new_minimal_with_runtime_dir(Arc::clone(&store), &runtime);
        let router2 = crate::http_server::managed_device_routes().with_state(state2);
        let (_, body) = post_inventory(
            &router2,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:device:5", "10.20.1.1", "nb-r1")],
            }),
        )
        .await;
        assert_eq!(body["conflicts"], 1, "{body}");

        // Import a fresh inventory device, then "restart" (new AppState on
        // the same runtime dir) and verify declared-only state persists
        // and does NOT become collection-enabled.
        let (_, body) = post_inventory(
            &router2,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:device:6", "10.20.1.2", "nb-r2")],
            }),
        )
        .await;
        assert_eq!(body["imported"], 1, "{body}");

        let state3 = AppState::new_minimal_with_runtime_dir(Arc::clone(&store), &runtime);
        let router3 = crate::http_server::managed_device_routes().with_state(state3);
        let request = Request::get("/api/onboarding/devices")
            .body(Body::empty())
            .expect("build request");
        let response = router3.oneshot(request).await.expect("oneshot");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        let devices = json["devices"].as_array().expect("devices array");
        let r2 = devices
            .iter()
            .find(|d| d["address"] == "10.20.1.2")
            .expect("r2 listed after restart");
        assert_eq!(r2["enabled"], false, "{r2}");
        assert_eq!(r2["inventory_only"], true, "{r2}");
        assert_eq!(r2["external_id"], "netbox:device:6", "{r2}");

        // The pre-existing active device is untouched by the import.
        let active = devices
            .iter()
            .find(|d| d["address"] == "10.20.1.1")
            .expect("active device listed");
        assert_eq!(active["enabled"], true, "{active}");
        assert!(active.get("inventory_only").is_none() || active["inventory_only"] == false);
    }

    // ── BN-015-U1 N1/N2/N3/N4 hardening ──────────────────────────────────

    #[tokio::test]
    async fn inventory_import_instance_scoped_identities() {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph_path = dir.path().join("graph").to_string_lossy().into_owned();
        let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("open"));
        let state = AppState::new_minimal_with_runtime_dir(
            Arc::clone(&store),
            dir.path().to_str().expect("utf8 tempdir"),
        );
        let router = crate::http_server::managed_device_routes().with_state(state);

        // Two NetBox instances each contain device id 123 — their
        // instance-scoped external identities are distinct.
        let (_, body) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [
                    nb_device("netbox:https:nb-a.example.com:443:device:123", "10.30.0.1", "nb-a-r1"),
                    nb_device("netbox:https:nb-b.example.com:443:device:123", "10.30.0.2", "nb-b-r1"),
                ],
            }),
        )
        .await;
        assert_eq!(body["imported"], 2, "{body}");
        assert_eq!(body["graph_sync"], "synced", "{body}");

        // The bare legacy form is still a valid namespaced id but is a
        // DIFFERENT identity — it must not collide with either instance.
        let (_, body2) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:device:123", "10.30.0.3", "legacy-r1")],
            }),
        )
        .await;
        assert_eq!(body2["imported"], 1, "{body2}");
    }

    #[tokio::test]
    async fn inventory_import_concurrent_same_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph_path = dir.path().join("graph").to_string_lossy().into_owned();
        let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("open"));
        let state = AppState::new_minimal_with_runtime_dir(
            Arc::clone(&store),
            dir.path().to_str().expect("utf8 tempdir"),
        );
        let router = crate::http_server::managed_device_routes().with_state(state.clone());

        let payload = serde_json::json!({
            "source": "netbox",
            "devices": [nb_device("netbox:https:nb:443:device:77", "10.30.1.1", "r1")],
        });
        let (a, b) = tokio::join!(
            post_inventory(&router, payload.clone()),
            post_inventory(&router, payload),
        );
        let outcomes: Vec<&str> = a
            .1["results"]
            .as_array()
            .unwrap()
            .iter()
            .chain(b.1["results"].as_array().unwrap().iter())
            .map(|r| r["outcome"].as_str().unwrap())
            .collect();
        // Exactly one import wins; the other sees already_imported or
        // fails closed — never a second device for the same identity.
        assert!(
            outcomes.iter().all(|o| matches!(*o, "imported" | "already_imported" | "failed")),
            "unexpected outcomes: {outcomes:?}"
        );
        assert_eq!(
            outcomes.iter().filter(|o| **o == "imported").count(),
            1,
            "exactly one concurrent import may win: {outcomes:?}"
        );

        let devices = state.registry.list_all_targets().expect("list");
        let matches: Vec<_> = devices
            .iter()
            .filter(|t| t.external_id.as_deref() == Some("netbox:https:nb:443:device:77"))
            .collect();
        assert_eq!(matches.len(), 1, "external identity must be unique");
    }

    #[tokio::test]
    async fn inventory_import_host_port_conflict_and_ipv6() {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph_path = dir.path().join("graph").to_string_lossy().into_owned();
        let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("open"));
        let state = AppState::new_minimal_with_runtime_dir(
            Arc::clone(&store),
            dir.path().to_str().expect("utf8 tempdir"),
        );
        let router = crate::http_server::managed_device_routes().with_state(state);

        // Existing device registered WITH a gNMI port.
        let (_, body) = post_device(
            &router,
            serde_json::json!({
                "address": "10.30.2.1:57400",
                "hostname": "port-dev",
                "enabled": false,
                "vendor": "Cisco",
            }),
        )
        .await;
        assert_eq!(body["success"], true, "{body}");

        // Bare-address import of the same canonical host must conflict —
        // the graph cannot distinguish 10.30.2.1 from 10.30.2.1:57400.
        let (_, body) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:https:nb:443:device:5", "10.30.2.1", "bare")],
            }),
        )
        .await;
        assert_eq!(body["conflicts"], 1, "{body}");
        assert_eq!(body["results"][0]["outcome"], "conflict");

        // IPv6 identity: canonical normalization treats expanded and
        // compressed forms as the same device.
        let (_, body) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:https:nb:443:device:6", "2001:db8::1", "v6")],
            }),
        )
        .await;
        assert_eq!(body["imported"], 1, "{body}");
        let (_, body) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:https:nb:443:device:6", "2001:0db8:0000:0000:0000:0000:0000:0001", "v6")],
            }),
        )
        .await;
        assert_eq!(body["already_present"], 1, "{body}");
    }

    #[tokio::test]
    async fn inventory_import_ambiguous_external_id_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Seed the config DB with two devices sharing one external id —
        // simulating ambiguity that predates uniqueness enforcement.
        {
            let store = crate::sqlite_store::SqliteStore::open(dir.path())
                .expect("open store");
            drop(store);
            let conn = rusqlite::Connection::open(dir.path().join("bonsai_config.db"))
                .expect("raw conn");
            conn.execute_batch("DROP INDEX IF EXISTS idx_devices_external_id")
                .expect("drop unique index");
            for addr in ["10.30.3.1", "10.30.3.2"] {
                conn.execute(
                    "INSERT INTO devices (address, enabled, hostname, tls_domain, vendor, \
                     credential_alias, username_env, role, site, collector_id, username, \
                     password, password_env, ca_cert, paths, optional, created_at_ns, \
                     updated_at_ns, created_by, updated_by, last_operator_action, external_id, \
                     inventory_only, selected_paths, extra_ips, inventory_platform, \
                     inventory_status, inventory_tenant) VALUES (?1, 0, NULL, NULL, NULL, \
                     NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, '[]', 0, 0, 0, \
                     '', '', '', 'netbox:https:nb:443:device:42', 1, NULL, NULL, NULL, NULL, NULL)",
                    rusqlite::params![addr],
                )
                .expect("seed ambiguous row");
            }
        }
        let graph_path = dir.path().join("graph").to_string_lossy().into_owned();
        let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("open"));
        let state = AppState::new_minimal_with_runtime_dir(
            Arc::clone(&store),
            dir.path().to_str().expect("utf8 tempdir"),
        );
        let router = crate::http_server::managed_device_routes().with_state(state);

        let (_, body) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:https:nb:443:device:42", "10.30.3.9", "amb")],
            }),
        )
        .await;
        assert_eq!(body["failed"], 1, "{body}");
        assert!(
            body["results"][0]["reason"]
                .as_str()
                .unwrap_or("")
                .contains("ambiguous"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn inventory_import_persistence_failure_is_truthful() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Pre-create the config DB and install triggers that reject all
        // device writes — simulates a failing persistence layer.
        {
            let store = crate::sqlite_store::SqliteStore::open(dir.path())
                .expect("open store");
            drop(store);
            let conn = rusqlite::Connection::open(dir.path().join("bonsai_config.db"))
                .expect("raw conn");
            conn.execute_batch(
                "CREATE TRIGGER fail_device_write BEFORE INSERT ON devices \
                 WHEN NEW.address LIKE '10.30.%' \
                 BEGIN SELECT RAISE(FAIL, 'injected write failure'); END;\
                 CREATE TRIGGER fail_device_update BEFORE UPDATE ON devices \
                 WHEN NEW.address LIKE '10.30.%' \
                 BEGIN SELECT RAISE(FAIL, 'injected write failure'); END;",
            )
            .expect("install triggers");
        }
        let graph_path = dir.path().join("graph").to_string_lossy().into_owned();
        let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("open"));
        let state = AppState::new_minimal_with_runtime_dir(
            Arc::clone(&store),
            dir.path().to_str().expect("utf8 tempdir"),
        );
        let router = crate::http_server::managed_device_routes().with_state(state.clone());

        let (_, body) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [
                    nb_device("netbox:https:nb:443:device:1", "10.30.4.1", "r1"),
                    nb_device("netbox:https:nb:443:device:2", "10.30.4.2", "r2"),
                ],
            }),
        )
        .await;
        assert_eq!(body["failed"], 2, "{body}");
        assert_eq!(body["imported"], 0, "{body}");
        // No successful response may claim unpersisted records.
        for row in body["results"].as_array().unwrap() {
            assert_eq!(row["outcome"], "failed", "{row}");
        }
        let devices = state.registry.list_all_targets().expect("list");
        assert!(
            devices
                .iter()
                .all(|t| t.external_id.is_none() || !t.external_id.as_deref().unwrap().contains("device:")),
            "unpersisted imports must not appear in the registry"
        );
    }

    #[tokio::test]
    async fn inventory_import_graph_pending_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph_path = dir.path().join("graph").to_string_lossy().into_owned();
        let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("open"));
        // Poison the graph write lock: sync_sites_from_targets takes it
        // inside spawn_blocking and panics on poison, so the projection
        // fails while registry persistence (SQLite) still succeeds.
        {
            let lock = store.write_lock();
            std::thread::spawn(move || {
                let _guard = lock.lock().expect("lock");
                panic!("intentional write-lock poison for test");
            })
            .join()
            .ok();
        }
        let state = AppState::new_minimal_with_runtime_dir(
            Arc::clone(&store),
            dir.path().to_str().expect("utf8 tempdir"),
        );
        let router = crate::http_server::managed_device_routes().with_state(state.clone());

        let (_, body) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:https:nb:443:device:8", "10.30.5.1", "r1")],
            }),
        )
        .await;
        assert_eq!(body["imported"], 1, "{body}");
        assert_eq!(body["graph_sync"], "pending", "{body}");
        assert!(!body["graph_sync_error"].as_str().unwrap_or("").is_empty());

        // Registry record IS durable despite the pending projection —
        // the row outcome is truthful about persistence, and the
        // response distinguishes it from graph synchronization.
        let devices = state.registry.list_all_targets().expect("list");
        assert!(devices
            .iter()
            .any(|t| t.external_id.as_deref() == Some("netbox:https:nb:443:device:8")));
    }

    #[tokio::test]
    async fn inventory_import_retry_reconciles_projection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph_path = dir.path().join("graph").to_string_lossy().into_owned();
        let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("open"));
        let state = AppState::new_minimal_with_runtime_dir(
            Arc::clone(&store),
            dir.path().to_str().expect("utf8 tempdir"),
        );
        let router = crate::http_server::managed_device_routes().with_state(state);

        let (_, body) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:https:nb:443:device:8", "10.30.5.1", "r1")],
            }),
        )
        .await;
        assert_eq!(body["graph_sync"], "synced", "{body}");
        assert!(!located_at(&store, "10.30.5.1").is_empty());

        // Simulate a lost projection: delete the Device node's site edge
        // directly, then re-import the unchanged object. The retry must
        // reconcile the missing graph association.
        {
            let db = store.db();
            let conn = Connection::new(&db).expect("graph connection");
            let mut stmt = conn
                .prepare("MATCH (d:Device {address: $addr})-[r:LOCATED_AT]->(s:Site) DELETE r")
                .expect("prepare delete edge");
            conn.execute(
                &mut stmt,
                vec![("addr", Value::String("10.30.5.1".to_string()))],
            )
            .expect("delete edge");
        }
        assert!(located_at(&store, "10.30.5.1").is_empty());

        let (_, body2) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:https:nb:443:device:8", "10.30.5.1", "r1")],
            }),
        )
        .await;
        assert_eq!(body2["already_present"], 1, "{body2}");
        assert_eq!(body2["graph_sync"], "synced", "{body2}");
        assert!(
            !located_at(&store, "10.30.5.1").is_empty(),
            "retry must restore the site projection"
        );
    }

    #[tokio::test]
    async fn inventory_lifecycle_requires_explicit_activation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph_path = dir.path().join("graph").to_string_lossy().into_owned();
        let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("open"));
        let state = AppState::new_minimal_with_runtime_dir(
            Arc::clone(&store),
            dir.path().to_str().expect("utf8 tempdir"),
        );
        let router = crate::http_server::managed_device_routes().with_state(state);

        let (_, body) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:https:nb:443:device:11", "10.30.6.1", "r1")],
            }),
        )
        .await;
        assert_eq!(body["imported"], 1, "{body}");

        // Enable request WITHOUT a collection plan → rejected.
        let (_, body) = post_device(
            &router,
            serde_json::json!({
                "address": "10.30.6.1",
                "hostname": "r1",
                "enabled": true,
                "vendor": "Nokia",
            }),
        )
        .await;
        assert_eq!(body["success"], false, "{body}");
        assert!(body["error"].as_str().unwrap().contains("collection plan"), "{body}");

        // Metadata-only update (enabled omitted→default true is rejected;
        // explicit enabled=false preserves inventory state).
        let (_, body) = post_device(
            &router,
            serde_json::json!({
                "address": "10.30.6.1",
                "hostname": "r1-renamed",
                "enabled": false,
                "vendor": "Nokia",
                "site": "Site B",
            }),
        )
        .await;
        assert_eq!(body["success"], true, "{body}");
        assert_eq!(body["device"]["inventory_only"], true, "{body}");
        assert_eq!(body["device"]["enabled"], false, "{body}");

        // Explicit activation: enabled + validated collection plan.
        let (_, body) = post_device(
            &router,
            serde_json::json!({
                "address": "10.30.6.1",
                "hostname": "r1-renamed",
                "enabled": true,
                "vendor": "Nokia",
                "site": "Site B",
                "selected_paths": [{
                    "path": "/interfaces/interface/state/counters",
                    "origin": "test",
                    "mode": "poll",
                    "sample_interval_ns": 0,
                    "rationale": "",
                    "optional": false
                }],
            }),
        )
        .await;
        assert_eq!(body["success"], true, "{body}");
        assert_eq!(body["device"]["inventory_only"], false, "{body}");
        assert_eq!(body["device"]["enabled"], true, "{body}");
        // External identity preserved through activation.
        assert!(body["device"]["external_id"]
            .as_str()
            .unwrap()
            .contains("device:11"), "{body}");

        // Bulk "start" on an inventory-only device must not bypass the
        // plan requirement. Re-import a fresh inventory asset for this.
        let (_, body) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:https:nb:443:device:12", "10.30.6.2", "r2")],
            }),
        )
        .await;
        assert_eq!(body["imported"], 1, "{body}");
        let request = Request::post("/api/onboarding/devices/bulk")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"addresses": ["10.30.6.2"], "action": "start"}).to_string(),
            ))
            .expect("build request");
        let response = router.clone().oneshot(request).await.expect("oneshot");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let bulk: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(bulk["success"], false, "{bulk}");
        assert!(bulk["error"].as_str().unwrap().contains("collection plan"), "{bulk}");
    }

    #[tokio::test]
    async fn inventory_reimport_preserves_operational_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph_path = dir.path().join("graph").to_string_lossy().into_owned();
        let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).expect("open"));
        let state = AppState::new_minimal_with_runtime_dir(
            Arc::clone(&store),
            dir.path().to_str().expect("utf8 tempdir"),
        );
        let router = crate::http_server::managed_device_routes().with_state(state.clone());

        // Import as inventory, then explicitly activate it.
        let (_, body) = post_inventory(
            &router,
            serde_json::json!({
                "source": "netbox",
                "devices": [nb_device("netbox:https:nb:443:device:20", "10.30.7.1", "r1")],
            }),
        )
        .await;
        assert_eq!(body["imported"], 1, "{body}");
        let (_, body) = post_device(
            &router,
            serde_json::json!({
                "address": "10.30.7.1",
                "hostname": "r1",
                "enabled": true,
                "vendor": "Cisco",
                "role": "spine",
                "credential_alias": "",
                "selected_paths": [{
                    "path": "/interfaces/interface/state/counters",
                    "origin": "test", "mode": "poll",
                    "sample_interval_ns": 0, "rationale": "", "optional": false
                }],
            }),
        )
        .await;
        assert_eq!(body["success"], true, "{body}");

        // Re-import with DIFFERENT declared metadata: the configured
        // device's operational fields must be preserved and the drift
        // surfaced as a conflict — never silently rewritten.
        let mut drifted = nb_device("netbox:https:nb:443:device:20", "10.30.7.1", "r1-renamed");
        drifted["vendor"] = serde_json::json!("Arista");
        let (_, body) = post_inventory(
            &router,
            serde_json::json!({"source": "netbox", "devices": [drifted]}),
        )
        .await;
        assert_eq!(body["conflicts"], 1, "{body}");
        assert_eq!(body["results"][0]["outcome"], "conflict");

        let devices = state.registry.list_all_targets().expect("list");
        let dev = devices
            .iter()
            .find(|t| t.address == "10.30.7.1")
            .expect("device");
        assert_eq!(dev.hostname.as_deref(), Some("r1"), "hostname preserved");
        assert_eq!(dev.vendor.as_deref(), Some("Cisco"), "vendor preserved");
        assert_eq!(dev.role.as_deref(), Some("spine"), "role preserved");
        assert!(dev.enabled, "lifecycle preserved");
        assert!(!dev.selected_paths.is_empty(), "acquisition plan preserved");
    }
}
