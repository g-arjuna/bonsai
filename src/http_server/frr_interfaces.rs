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
use sha2::{Digest, Sha256};

fn operator_allowed(extensions: &Extensions) -> bool {
    extensions
        .get::<Role>()
        .is_some_and(|r| *r >= Role::Operator)
}

fn scope_allows(extensions: &Extensions, target: &crate::config::TargetConfig) -> bool {
    // `speaker` is the legacy routing-scope key, but for this native device
    // boundary it must carry the canonical inventory device identity. A scoped
    // key naming merely a matching interface/management address is not device
    // authorization.
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

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

fn api_error(status: StatusCode, reason_code: &str, description: &str) -> ApiError {
    (
        status,
        Json(json!({
            "error": reason_code,
            "reason_code": reason_code,
            "description": description,
        })),
    )
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

fn lookup_target(state: &AppState, address: &str) -> Result<crate::config::TargetConfig, ApiError> {
    let targets = state.registry.list_all_targets().map_err(|_| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "inventory_unavailable",
            "inventory unavailable",
        )
    })?;
    let mut matching = targets
        .into_iter()
        .filter(|t| t.enabled && t.primary_ip() == address);
    let target = matching.next().ok_or_else(|| {
        api_error(
            StatusCode::NOT_FOUND,
            "native_device_not_found",
            "registered native device not found",
        )
    })?;
    if matching.next().is_some() {
        return Err(api_error(
            StatusCode::CONFLICT,
            "ambiguous_inventory_identity",
            "ambiguous inventory exporter identity",
        ));
    }
    Ok(target)
}

fn known_hosts_digest(path: &std::path::Path) -> Result<String, ApiError> {
    let bytes = std::fs::read(path).map_err(|_| {
        api_error(
            StatusCode::FAILED_DEPENDENCY,
            "host_key_trust_unavailable",
            "pinned SSH host-key database is unavailable",
        )
    })?;
    let mut material = path.as_os_str().as_encoded_bytes().to_vec();
    material.extend_from_slice(&bytes.len().to_be_bytes());
    material.extend_from_slice(&bytes);
    Ok(hex::encode(Sha256::digest(&material)))
}

struct AcquisitionAuthority {
    target: crate::config::TargetConfig,
    target_encoding: Vec<u8>,
    credentials: crate::credentials::ResolvedCredential,
    known_hosts: std::path::PathBuf,
    known_hosts_sha256: String,
    canonical_device_id: String,
}

impl AcquisitionAuthority {
    fn admission(
        target: crate::config::TargetConfig,
        credentials: crate::credentials::ResolvedCredential,
        known_hosts: std::path::PathBuf,
    ) -> Result<Self, ApiError> {
        let target_encoding = serde_json::to_vec(&target).map_err(|_| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "authority_encoding_failed",
                "native acquisition authority could not be encoded",
            )
        })?;
        let known_hosts_sha256 = known_hosts_digest(&known_hosts)?;
        let canonical_device_id = crate::enrichment::frr_interfaces::canonical_device_id(&target);
        Ok(Self {
            target,
            target_encoding,
            credentials,
            known_hosts,
            known_hosts_sha256,
            canonical_device_id,
        })
    }

    fn credentials_match(&self, other: &crate::credentials::ResolvedCredential) -> bool {
        self.credentials.username == other.username
            && self.credentials.password.as_str() == other.password.as_str()
    }

    /// Validate that the result still belongs to the same admitted device,
    /// acquisition configuration, credential resolution and pinned trust
    /// anchor. This is a narrow in-process revision fence, not a distributed
    /// transaction across inventory and graph storage; that limitation is
    /// explicit in the closeout evidence.
    fn validate_current(&self, state: &AppState) -> Result<(), ApiError> {
        let current = lookup_target(state, &self.target.primary_ip()).map_err(|_| {
            api_error(
                StatusCode::CONFLICT,
                "acquisition_authority_changed",
                "native interface acquisition authority changed during collection",
            )
        })?;
        let canonical = crate::enrichment::frr_interfaces::canonical_device_id(&current);
        let current_encoding = serde_json::to_vec(&current).unwrap_or_default();
        if !current.enabled
            || canonical != self.canonical_device_id
            || current_encoding != self.target_encoding
        {
            return Err(api_error(
                StatusCode::CONFLICT,
                "acquisition_authority_changed",
                "native interface acquisition authority changed during collection; result not admitted",
            ));
        }
        let credentials =
            super::device::resolve_target_credentials_for_discovery(&current, &state.credentials)
                .map_err(|_| {
                    api_error(
                        StatusCode::CONFLICT,
                        "acquisition_authority_changed",
                        "native credential authority changed during collection",
                    )
                })?
                .ok_or_else(|| {
                    api_error(
                        StatusCode::CONFLICT,
                        "acquisition_authority_changed",
                        "native device credentials are no longer available",
                    )
                })?;
        if !self.credentials_match(&credentials) {
            return Err(api_error(
                StatusCode::CONFLICT,
                "acquisition_authority_changed",
                "native credential authority changed during collection",
            ));
        }
        let current_digest = known_hosts_digest(&self.known_hosts).map_err(|_| {
            api_error(
                StatusCode::CONFLICT,
                "acquisition_authority_changed",
                "pinned SSH host-key authority changed during collection",
            )
        })?;
        if current_digest != self.known_hosts_sha256 {
            return Err(api_error(
                StatusCode::CONFLICT,
                "acquisition_authority_changed",
                "pinned SSH host-key authority changed during collection",
            ));
        }
        Ok(())
    }
}

fn classify_collection_error(text: &str) -> (StatusCode, &'static str, &'static str) {
    if text.contains("shadow dispatch refused") || text.contains("version evidence") {
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
    } else if text.contains("stdout exceeds bounded retention")
        || text.contains("stderr exceeds bounded retention")
    {
        (
            StatusCode::FAILED_DEPENDENCY,
            "capture_output_bound_exceeded",
            "fixed read-only capture exceeded its bounded output retention limit",
        )
    } else {
        (
            StatusCode::FAILED_DEPENDENCY,
            "capture_transport_or_interpretation_failed",
            "fixed read-only capture or bounded interpretation failed",
        )
    }
}

fn collection_success_json(
    observation: &crate::enrichment::frr_interfaces::StoredFrrInterfaceShadowObservation,
    id: String,
) -> Value {
    let attempt_outcome = match observation.interpretation_outcome.as_str() {
        "partial_usable" => "partial_success",
        _ => "success",
    };
    json!({
        "observation_id": id,
        "outcome": attempt_outcome,
        "interpretation_outcome": observation.interpretation_outcome,
        "device_address": observation.device_address,
        "device_id": observation.device_id,
        "captured_at": observation.captured_at,
        "transport": observation.transport,
        "host_key_verified": true,
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
        "usable_entity_facts": observation.usable_entity_facts,
        "status_facts": observation.status_facts,
        "fact_retention_truncated": observation.fact_retention_truncated,
        "interface_output_sha256": observation.interface_output_sha256,
        "neighbor_output_sha256": observation.neighbor_output_sha256,
        "mac_output_sha256": observation.mac_output_sha256,
        "vlan_output_sha256": observation.vlan_output_sha256,
        "normalization_report": observation.normalization_report,
        "authoritative_projection": "none"
    })
}

pub(super) async fn collect(
    State(state): State<AppState>,
    extensions: Extensions,
    Path(address): Path<String>,
) -> ApiResult {
    if !operator_allowed(&extensions) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "operator_role_required",
            "native interface collection requires an authenticated operator",
        ));
    }
    let target = lookup_target(&state, &address)?;
    if !native_cli_scope_allows(&extensions, &target) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "routing_scope_violation",
            "routing scope does not authorize native interface collection for this canonical device",
        ));
    }
    // Exact artifact qualification is admission metadata and is evaluated
    // before credential resolution or any network contact.
    if target.software_build.as_deref() != Some(crate::nos_features::FRR_1061_STRUCTURED_ARTIFACT) {
        return Err(api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_or_unqualified_release",
            "native interface release/build is not qualified for the structured FRR interface profile",
        ));
    }
    let credentials =
        super::device::resolve_target_credentials_for_discovery(&target, &state.credentials)
            .map_err(|_| {
                api_error(
                    StatusCode::FAILED_DEPENDENCY,
                    "credential_resolution_failed",
                    "native credential resolution failed",
                )
            })?
            .ok_or_else(|| {
                api_error(
                    StatusCode::FAILED_DEPENDENCY,
                    "credentials_unavailable",
                    "native device credentials are unavailable",
                )
            })?;
    let Some(known_hosts) = known_hosts_path(&state).await else {
        return Err(api_error(
            StatusCode::FAILED_DEPENDENCY,
            "host_key_trust_unavailable",
            "pinned SSH host-key configuration is required",
        ));
    };
    let authority = AcquisitionAuthority::admission(target, credentials, known_hosts)?;
    let admission = crate::enrichment::frr_interfaces::try_acquire_capture_admission(
        &authority.canonical_device_id,
    )
    .map_err(|_| {
        api_error(
            StatusCode::TOO_MANY_REQUESTS,
            "native_capture_admission_overloaded",
            "native interface capture admission limit reached; retry later",
        )
    })?;

    // Run the acquisition lifecycle in its own task. If the HTTP caller is
    // cancelled, the task continues to hold admission through capture,
    // blocking normalization and the persistence fence; a subsequent request
    // therefore cannot incorrectly reuse capacity or overwrite ordering while
    // the abandoned operation is still completing.
    let operation_state = state.clone();
    let operation = tokio::spawn(async move {
        let attempt_started_at_ns = crate::graph::common::now_ns();
        let acquisition_result =
            crate::enrichment::frr_interfaces::collect_normalize_frr_interfaces(
                &authority.target,
                &authority.credentials,
                Some(authority.known_hosts.as_path()),
                &collector_id(&operation_state),
                &admission,
            )
            .await;
        let observation = match acquisition_result {
            Ok(observation) => observation,
            Err(error) => {
                authority.validate_current(&operation_state)?;
                let text = error.to_string();
                let (status, class, detail) = classify_collection_error(&text);
                crate::enrichment::frr_interfaces::record_acquisition_failure(
                    &operation_state.store,
                    &authority.target,
                    attempt_started_at_ns,
                    class,
                    detail,
                )
                .await
                .map_err(|_| {
                    api_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "acquisition_state_persistence_failed",
                        "native interface acquisition state persistence failed",
                    )
                })?;
                return Err(api_error(status, class, detail));
            }
        };

        authority.validate_current(&operation_state)?;
        if matches!(
            observation.interpretation_outcome.as_str(),
            "complete_usable" | "partial_usable" | "genuine_complete_empty"
        ) {
            let id = crate::enrichment::frr_interfaces::persist_observation(
                &operation_state.store,
                &observation,
            )
            .await
            .map_err(|_| {
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "observation_persistence_failed",
                    "native interface observation persistence failed",
                )
            })?;
            let result = collection_success_json(&observation, id);
            let outcome = result
                .get("outcome")
                .cloned()
                .unwrap_or(Value::String("success".into()));
            Ok(Json(json!({
                "outcome": outcome,
                "result": result,
            })))
        } else {
            let outcome = observation.interpretation_outcome.clone();
            crate::enrichment::frr_interfaces::persist_degraded_observation(
                &operation_state.store,
                &observation,
                &outcome,
                "native capture completed but did not yield a usable interface interpretation",
            )
            .await
            .map_err(|_| {
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "degraded_evidence_persistence_failed",
                    "degraded native interface evidence persistence failed",
                )
            })?;
            Err(api_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                match outcome.as_str() {
                    "invalid_input" => "invalid_input",
                    "no_usable_records" => "no_usable_records",
                    "output_budget_exhausted" => "output_budget_exhausted",
                    "capture_failed" => "capture_failed",
                    "unsupported_release_or_format" => "unsupported_or_unqualified_release",
                    _ => "native_interpretation_degraded",
                },
                "native capture completed but did not yield a usable interface interpretation; last usable evidence was preserved",
            ))
        }
    });
    operation.await.map_err(|_| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "native_acquisition_task_failed",
            "native interface acquisition task failed",
        )
    })?
}

fn operator_observation_json(
    observation: &crate::enrichment::frr_interfaces::StoredFrrInterfaceShadowObservation,
) -> Value {
    // Explicit bounded operator DTO. Raw device output is deliberately absent
    // even though the persistence model retains bounded raw evidence internally.
    json!({
        "observation_id": observation.observation_id,
        "device_address": observation.device_address,
        "device_id": observation.device_id,
        "captured_at_ns": observation.captured_at_ns,
        "capture_started_at_ns": observation.capture_started_at_ns,
        "captured_at": observation.captured_at,
        "transport": observation.transport,
        "acquisition_contract": observation.acquisition_contract,
        "interface_command": observation.interface_command,
        "host_key_verified": true,
        "platform": observation.platform,
        "release_raw": observation.release_raw,
        "release_build": observation.release_build,
        "network_namespace": observation.network_namespace,
        "network_namespace_evidence": observation.network_namespace_evidence,
        "device_hostname_reported": observation.device_hostname_reported,
        "interface_output_sha256": observation.interface_output_sha256,
        "interface_output_bytes": observation.interface_output_bytes,
        "neighbor_command": observation.neighbor_command,
        "mac_command": observation.mac_command,
        "vlan_command": observation.vlan_command,
        "neighbor_output_sha256": observation.neighbor_output_sha256,
        "neighbor_output_bytes": observation.neighbor_output_bytes,
        "mac_output_sha256": observation.mac_output_sha256,
        "mac_output_bytes": observation.mac_output_bytes,
        "vlan_output_sha256": observation.vlan_output_sha256,
        "vlan_output_bytes": observation.vlan_output_bytes,
        "raw_output_retained": observation.raw_interface_json.is_some(),
        "raw_neighbor_output_retained": observation.raw_neighbor_output.is_some(),
        "raw_neighbor_output_omitted_reason": observation.raw_neighbor_output_omitted_reason,
        "raw_mac_output_retained": observation.raw_mac_output.is_some(),
        "raw_mac_output_omitted_reason": observation.raw_mac_output_omitted_reason,
        "raw_vlan_output_retained": observation.raw_vlan_output.is_some(),
        "raw_vlan_output_omitted_reason": observation.raw_vlan_output_omitted_reason,
        "profile_id": observation.profile_id,
        "profile_fingerprint": observation.profile_fingerprint,
        "normalizer_id": observation.normalizer_id,
        "normalizer_revision": observation.normalizer_revision,
        "mapping_revision": observation.mapping_revision,
        "qualification": observation.qualification,
        "shadow_only": observation.shadow_only,
        "capture_id": observation.capture_id,
        "session_epoch": observation.session_epoch,
        "sequence": observation.sequence,
        "interpretation_outcome": observation.interpretation_outcome,
        "usable_entity_facts": observation.usable_entity_facts,
        "status_facts": observation.status_facts,
        "facts_emitted": observation.facts_emitted,
        "facts_validated": observation.facts_validated,
        "facts_retained": observation.facts_retained,
        "fact_retention_truncated": observation.fact_retention_truncated,
        "facts": observation.facts,
        "command_execution": observation.command_execution,
        "family_states": observation.family_states,
        "diagnostics_retained": observation.diagnostics_retained,
        "diagnostic_retention_truncated": observation.diagnostic_retention_truncated,
        "normalization_report": observation.normalization_report,
        "acquisition_classification": observation.acquisition_classification,
    })
}

fn operational_l2_assessment(
    observation: &crate::enrichment::frr_interfaces::StoredFrrInterfaceShadowObservation,
) -> Value {
    let mut facts = Vec::new();
    for value in &observation.facts {
        match serde_json::from_value::<crate::typed_facts::TypedFact>(value.clone()) {
            Ok(fact) => facts.push(fact),
            Err(error) => {
                return json!({
                    "state": "unresolved_prerequisites",
                    "reason": "retained typed facts are undecodable",
                    "detail": error.to_string(),
                });
            }
        }
    }
    let mut coverage = Vec::new();
    for family in &observation.family_states {
        if let Some(coverage_value) = family.retained.as_ref().and_then(|r| r.coverage.as_ref()) {
            match serde_json::from_value::<crate::semantic_evidence::ScopedCaptureCoverage>(
                coverage_value.clone(),
            ) {
                Ok(value) => coverage.push(value),
                Err(error) => {
                    return json!({
                        "state": "unresolved_prerequisites",
                        "reason": "retained scoped capture coverage is undecodable",
                        "detail": error.to_string(),
                    });
                }
            }
        }
    }
    let as_of = observation.captured_at.clone();
    let mut derivation_policy =
        crate::forwarding_domain_derivation::ForwardingDomainDerivationPolicy::default();
    derivation_policy.max_capture_skew_seconds = 60;
    let derivation = match crate::forwarding_domain_derivation::derive_forwarding_domains(
        &facts,
        derivation_policy,
        &as_of,
    ) {
        Ok(derivation) => derivation,
        Err(error) => {
            return json!({
                "state": "unresolved_prerequisites",
                "reason": "forwarding-domain derivation prerequisites were not satisfied",
                "detail": error,
            });
        }
    };
    let relationships = crate::interface_relationships::analyze_interface_relationships(&facts);
    let arpnd_facts = facts
        .iter()
        .filter(|fact| fact.fact_kind == crate::neighbor_facts::FACT_KIND_ARPND_NEIGHBOR)
        .count();
    let mac_facts = facts
        .iter()
        .filter(|fact| fact.fact_kind == crate::neighbor_facts::FACT_KIND_MAC_FORWARDING)
        .count();
    let vlan_facts = facts
        .iter()
        .filter(|fact| fact.fact_kind == crate::vlan_facts::FACT_KIND_VLAN_BRIDGE_DOMAIN)
        .count();
    let derivation_report = derivation.report.clone();
    let derived_domain_evidence = derivation.evidence.len();
    if derived_domain_evidence == 0 || arpnd_facts == 0 || mac_facts == 0 {
        return json!({
            "state": "unresolved_prerequisites",
            "reason": "persisted evidence does not establish the bridge-domain, ARP/ND and MAC prerequisites required for cross-table correlation",
            "as_of": as_of,
            "capture_skew_policy_seconds": 60,
            "input_facts": facts.len(),
            "coverage_objects": coverage.len(),
            "prerequisite_evidence": {
                "arpnd_facts": arpnd_facts,
                "mac_facts": mac_facts,
                "vlan_facts": vlan_facts,
                "derived_domain_evidence": derived_domain_evidence,
            },
            "forwarding_domain_derivation": {
                "report": derivation_report,
                "derived_domain_evidence": derived_domain_evidence,
            },
            "interface_relationships": relationships,
            "authoritative_projection": "none",
        });
    }
    let mut reconciliation_policy =
        crate::neighbor_reconciliation::NeighborReconciliationPolicy::default();
    reconciliation_policy.max_capture_skew_seconds = 60;
    let reconciliation = match crate::neighbor_reconciliation::reconcile_neighbor_tables(
        &facts,
        &coverage,
        &derivation.evidence,
        &reconciliation_policy,
        &as_of,
    ) {
        Ok(report) => {
            serde_json::to_value(report).unwrap_or_else(|_| json!({"serialization":"failed"}))
        }
        Err(error) => json!({
            "state": "unresolved_prerequisites",
            "reason": "neighbor reconciliation prerequisites were not satisfied",
            "detail": error,
        }),
    };
    json!({
        "state": "available",
        "as_of": as_of,
        "capture_skew_policy_seconds": 60,
        "input_facts": facts.len(),
        "coverage_objects": coverage.len(),
        "forwarding_domain_derivation": {
            "report": derivation_report,
            "derived_domain_evidence": derived_domain_evidence,
        },
        "neighbor_reconciliation": reconciliation,
        "interface_relationships": relationships,
        "authoritative_projection": "none",
    })
}

pub(super) async fn latest(
    State(state): State<AppState>,
    extensions: Extensions,
    Path(address): Path<String>,
) -> ApiResult {
    if extensions.get::<Role>().is_none() {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "authentication required",
        ));
    }
    let target = lookup_target(&state, &address)?;
    if !native_cli_scope_allows(&extensions, &target) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "routing_scope_violation",
            "routing scope does not authorize native interface evidence for this canonical device",
        ));
    }
    let acquisition = crate::enrichment::frr_interfaces::acquisition_state(&state.store, &target)
        .await
        .map_err(|_| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "acquisition_state_undecodable",
                "native interface acquisition state is undecodable or belongs to another identity",
            )
        })?;
    let latest_attempt =
        crate::enrichment::frr_interfaces::latest_attempt_evidence(&state.store, &target)
            .await
            .map_err(|_| {
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "latest_attempt_undecodable",
                    "native interface latest-attempt evidence is undecodable or belongs to another identity",
                )
            })?;
    let observation_result =
        crate::enrichment::frr_interfaces::latest_observation(&state.store, &target).await;
    let observation = match observation_result {
        Ok(observation) => Some(observation),
        Err(error) if error.to_string().contains("shadow evidence unavailable") => None,
        Err(_) => {
            return Err(api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "stored_shadow_observation_invalid",
                "retained native interface shadow evidence is invalid or undecodable; acquisition state is preserved separately",
            ));
        }
    };
    let Some(observation) = observation else {
        return if let Some(acquisition) = acquisition {
            Ok(Json(json!({
                "state": if acquisition.last_attempt_outcome == "degraded" {
                    "interpretation_degraded"
                } else {
                    "acquisition_failed"
                },
                "evidence": null,
                "acquisition": acquisition,
                "latest_attempt": latest_attempt,
                "authoritative_projection": "none",
                "promotion": "shadow_only"
            })))
        } else {
            Err(api_error(
                StatusCode::NOT_FOUND,
                "native_shadow_evidence_unavailable",
                "native interface shadow evidence unavailable; collect authorized device state first",
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
        "interpretation_outcome": observation.interpretation_outcome,
        "evidence": operator_observation_json(&observation),
        "l2_assessment": operational_l2_assessment(&observation),
        "acquisition": acquisition,
        "latest_attempt": latest_attempt,
        "authoritative_projection": "none",
        "promotion": "shadow_only"
    })))
}
