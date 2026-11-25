//! Authenticated, inventory-bound standalone-FRR interface shadow collection.
//! Device operations are fixed read-only commands over verified SSH.
use super::{
    AppState,
    auth::{Role, RoutingAccessScope},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{Extensions, StatusCode},
};
use serde_json::{Value, json};

fn operator_allowed(extensions: &Extensions) -> bool {
    extensions
        .get::<Role>()
        .is_some_and(|r| *r >= Role::Operator)
}

fn scope_allows(extensions: &Extensions, target: &crate::config::TargetConfig) -> bool {
    // `speaker` is the legacy routing-scope key, but for this native device
    // boundary it must carry the canonical inventory device identity
    // (external identity/hostname, with the inventory primary address only as
    // the model's documented fallback). A scoped key naming merely a matching
    // interface/management address is not device authorization.
    let canonical = crate::enrichment::frr_interfaces::canonical_device_id(target);
    extensions
        .get::<RoutingAccessScope>()
        .and_then(|s| s.speaker.as_ref())
        .is_none_or(|speaker| speaker == &canonical)
}

fn native_cli_scope_allows(extensions: &Extensions, target: &crate::config::TargetConfig) -> bool {
    if !scope_allows(extensions, target) {
        return false;
    }
    let scope = extensions.get::<RoutingAccessScope>();
    let mode = extensions.get::<crate::graph::ownership::OwnershipAccessMode>();
    // Native CLI observations have no verified tenant/RD mapping. Match the
    // existing native-BGP boundary: constrained routing scopes may use them
    // only in explicit audit-only mode, never as an ownership bypass.
    mode == Some(&crate::graph::ownership::OwnershipAccessMode::AuditOnly)
        || scope.is_none_or(|s| s.tenant_id.is_none() && s.vrf_rd.is_none())
}

fn collector_id(_state: &AppState) -> String {
    std::env::var("BONSAI_COLLECTOR_ID").unwrap_or_else(|_| "bonsai-local".into())
}

async fn known_hosts_path(state: &AppState) -> Option<std::path::PathBuf> {
    let configs = state
        .store
        .list_config_items(Some("app_config".into()))
        .await
        .ok()?;
    configs
        .iter()
        .find(|i| i.enabled && i.name == "native_cli_known_hosts")
        .or_else(|| {
            // Existing native-CLI deployments may already pin device keys
            // through the native-BGP configuration item. The generic item
            // takes precedence; both are operator-supplied trust anchors.
            configs
                .iter()
                .find(|i| i.enabled && i.name == "native_bgp_known_hosts")
        })
        .map(|i| std::path::PathBuf::from(i.content_json.trim().trim_matches('"')))
}

fn lookup_target(
    state: &AppState,
    address: &str,
) -> Result<crate::config::TargetConfig, (StatusCode, String)> {
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
    Ok(target)
}

pub(super) async fn collect(
    State(state): State<AppState>,
    extensions: Extensions,
    Path(address): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if !operator_allowed(&extensions) {
        return Err((
            StatusCode::FORBIDDEN,
            "native interface collection requires an authenticated operator".into(),
        ));
    }
    let target = lookup_target(&state, &address)?;
    if !native_cli_scope_allows(&extensions, &target) {
        return Err((StatusCode::FORBIDDEN, "routing_scope_violation".into()));
    }
    // Exact artifact qualification is admission metadata and is evaluated
    // before credential resolution or any network contact. The acquisition
    // function repeats this check defense-in-depth for non-HTTP callers.
    if target.software_build.as_deref() != Some(crate::nos_features::FRR_1061_STRUCTURED_ARTIFACT) {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "native interface release/build is not qualified for the structured FRR interface profile".into(),
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
    let known_hosts = known_hosts_path(&state).await;
    let attempt_started_at_ns = crate::graph::common::now_ns();
    let acquisition_result = crate::enrichment::frr_interfaces::collect_normalize_frr_interfaces(
        &target,
        &credentials,
        known_hosts.as_deref(),
        &collector_id(&state),
    )
    .await;
    let observation = match acquisition_result {
        Ok(observation) => observation,
        Err(e) => {
            let text = e.to_string();
            if text.contains("frr_interface_admission_overloaded") {
                // Admission was refused before the capture started; this is
                // overload, not a failed device observation attempt.
                return Err((
                    StatusCode::TOO_MANY_REQUESTS,
                    "native interface capture admission limit reached; retry later".into(),
                ));
            }
            let (status, class, detail) = if text.contains("shadow dispatch refused")
                || text.contains("version evidence")
            {
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "unsupported_or_unqualified_release",
                    "device release/build evidence did not satisfy the qualified native interface profile",
                )
            } else if text.contains("deadline") {
                (
                    StatusCode::FAILED_DEPENDENCY,
                    "capture_deadline",
                    "fixed read-only capture exceeded its absolute deadline",
                )
            } else {
                (
                    StatusCode::FAILED_DEPENDENCY,
                    "capture_transport_or_interpretation_failed",
                    "fixed read-only capture or bounded interpretation failed",
                )
            };
            // Preserve the failure as a bounded acquisition attempt without
            // erasing the last successful shadow observation. Details are
            // static classifications, never device output or credentials.
            let _ = crate::enrichment::frr_interfaces::record_acquisition_failure(
                &state.store,
                &target,
                attempt_started_at_ns,
                class,
                detail,
            )
            .await;
            return Err((status, text));
        }
    };
    // Acquisition authority is evaluated at admission and again before
    // admission of the result. Disabling the device or changing its
    // acquisition/profile revision while SSH/normalization is in flight
    // must not make an obsolete result current.
    let current_target = lookup_target(&state, &address).map_err(|_| {
        (
            StatusCode::CONFLICT,
            "native interface acquisition authority changed during collection".into(),
        )
    })?;
    if !current_target.enabled || current_target.updated_at_ns != target.updated_at_ns {
        return Err((
            StatusCode::CONFLICT,
            "native interface acquisition authority changed during collection; result not admitted"
                .into(),
        ));
    }
    let id = crate::enrichment::frr_interfaces::persist_observation(&state.store, &observation)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "native interface observation persistence failed".into(),
            )
        })?;
    crate::enrichment::frr_interfaces::record_acquisition_success(&state.store, &observation)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "native interface acquisition state persistence failed".into(),
            )
        })?;
    Ok(Json(json!({
        "observation_id": id,
        "device_address": observation.device_address,
        "device_id": observation.device_id,
        "captured_at": observation.captured_at,
        "transport": observation.transport,
        "host_key": observation.host_key,
        "platform": observation.platform,
        "release_raw": observation.release_raw,
        "release_build": observation.release_build,
        "network_namespace": observation.network_namespace,
        "acquisition_contract": observation.acquisition_contract,
        "interface_command": observation.interface_command,
        "profile_id": observation.profile_id,
        "normalizer_revision": observation.normalizer_revision,
        "mapping_revision": observation.mapping_revision,
        "qualification": observation.qualification,
        "shadow_only": observation.shadow_only,
        "facts_emitted": observation.facts_emitted,
        "facts_validated": observation.facts_validated,
        "facts_retained": observation.facts_retained,
        "fact_retention_truncated": observation.fact_retention_truncated,
        "interface_output_sha256": observation.interface_output_sha256,
        "normalization_report": observation.normalization_report,
        "authoritative_projection": "none"
    })))
}

pub(super) async fn latest(
    State(state): State<AppState>,
    extensions: Extensions,
    Path(address): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if extensions.get::<Role>().is_none() {
        return Err((StatusCode::UNAUTHORIZED, "authentication required".into()));
    }
    let target = lookup_target(&state, &address)?;
    if !native_cli_scope_allows(&extensions, &target) {
        return Err((StatusCode::FORBIDDEN, "routing_scope_violation".into()));
    }
    let acquisition = crate::enrichment::frr_interfaces::acquisition_state(&state.store, &address)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "native interface acquisition state is undecodable".into(),
            )
        })?;
    let observation = crate::enrichment::frr_interfaces::latest_observation(&state.store, &address)
        .await
        .ok();
    let Some(observation) = observation else {
        return if let Some(acquisition) = acquisition {
            Ok(Json(json!({
                "state": "acquisition_failed",
                "evidence": null,
                "acquisition": acquisition,
                "authoritative_projection": "none",
                "promotion": "shadow_only"
            })))
        } else {
            Err((
                StatusCode::NOT_FOUND,
                "native interface shadow evidence unavailable; collect authorized device state first".into(),
            ))
        };
    };
    let now = crate::graph::common::now_ns();
    let age_seconds = (now - observation.captured_at_ns).max(0) / 1_000_000_000;
    let freshness = if age_seconds > 3600 { "stale" } else { "fresh" };
    Ok(Json(json!({
        "state": "available",
        "freshness": freshness,
        "age_seconds": age_seconds,
        "stale_after_seconds": 3600,
        "evidence": observation,
        "acquisition": acquisition,
        "authoritative_projection": "none",
        "promotion": "shadow_only"
    })))
}
