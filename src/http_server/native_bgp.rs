//! Authenticated, inventory-bound native collection; device operations are fixed read-only commands.
use super::{
    AppState,
    auth::{Role, RoutingAccessScope},
};
use axum::{
    Json,
    extract::State,
    http::{Extensions, StatusCode},
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CollectRequest {
    device_address: Option<String>,
    #[serde(default = "family")]
    afi_safi: String,
}
fn family() -> String {
    "ipv4-unicast".into()
}

pub(super) async fn collect(
    State(state): State<AppState>,
    extensions: Extensions,
    Json(req): Json<CollectRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if extensions.get::<Role>().is_none_or(|r| *r < Role::Operator) {
        return Err((
            StatusCode::FORBIDDEN,
            "native collection requires an authenticated operator".into(),
        ));
    }
    let scope = extensions.get::<RoutingAccessScope>();
    let address = req
        .device_address
        .or_else(|| scope.and_then(|s| s.speaker.clone()))
        .ok_or((StatusCode::BAD_REQUEST, "device_address is required".into()))?;
    if scope
        .and_then(|s| s.speaker.as_ref())
        .is_some_and(|s| s != &address)
    {
        return Err((StatusCode::FORBIDDEN, "routing_scope_violation".into()));
    }
    let mode = extensions.get::<crate::graph::ownership::OwnershipAccessMode>();
    if mode != Some(&crate::graph::ownership::OwnershipAccessMode::AuditOnly)
        && scope.is_some_and(|s| s.tenant_id.is_some() || s.vrf_rd.is_some())
    {
        return Err((
            StatusCode::FORBIDDEN,
            "routing_scope_violation: native CLI has no verified tenant/RD mapping".into(),
        ));
    }
    crate::enrichment::native_bgp::validate_family(&req.afi_safi)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let targets = state.registry.list_all_targets().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "inventory unavailable".into(),
        )
    })?;
    let mut matching = targets
        .into_iter()
        .filter(|t| t.enabled && t.primary_ip() == address);
    let target = matching.next().ok_or((
        StatusCode::NOT_FOUND,
        "registered native device not found".into(),
    ))?;
    if matching.next().is_some() {
        return Err((
            StatusCode::CONFLICT,
            "ambiguous inventory exporter identity".into(),
        ));
    }
    let credentials =
        super::device::resolve_target_credentials_for_discovery(&target, &state.credentials)
            .map_err(|_| {
                (
                    StatusCode::FAILED_DEPENDENCY,
                    "native credential resolution failed".into(),
                )
            })?
            .ok_or((
                StatusCode::FAILED_DEPENDENCY,
                "native device credentials are unavailable".into(),
            ))?;
    let configs = state
        .store
        .list_config_items(Some("app_config".into()))
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "native collector configuration unavailable".into(),
            )
        })?;
    let known_hosts = configs
        .iter()
        .find(|i| i.enabled && i.name == "native_bgp_known_hosts")
        .map(|i| std::path::PathBuf::from(i.content_json.trim().trim_matches('"')));
    let observation = crate::enrichment::native_bgp::collect_native_bgp(
        &target,
        &credentials,
        &req.afi_safi,
        known_hosts.as_deref(),
    )
    .await
    .map_err(|_| {
        (
            StatusCode::FAILED_DEPENDENCY,
            "native capture failed; verify registered vendor, credentials and SSH host keys".into(),
        )
    })?;
    let id = crate::enrichment::native_bgp::persist_observation(&state.store, &observation)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "native observation persistence failed".into(),
            )
        })?;
    Ok(Json(
        json!({"observation_id":id,"exporter_address":address,"captured_at_ns":observation.captured_at_ns,
        "parse_quality":observation.capture.parse_quality,"neighbor_count":observation.capture.neighbors.len(),
        "raw_sample_sha256":observation.capture.raw_sample_sha256,"transport":observation.transport}),
    ))
}
