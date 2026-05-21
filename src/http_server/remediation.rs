#[derive(Deserialize)]
pub(super) struct ApprovalsParams {
    #[serde(default = "default_proposal_status")]
    status: String,
    #[serde(default = "default_limit")]
    limit: u32,
}
#[derive(Serialize)]
pub(super) struct TrustEntry {
    trust_key: String,
    record: crate::remediation::TrustRecord,
}
#[derive(Serialize)]
pub(super) struct ApprovalsListResponse {
    proposals: Vec<RemediationProposalRow>,
    trust: Vec<TrustEntry>,
    graduation_hints: Vec<crate::remediation::GraduationHint>,
    active_rollbacks: Vec<crate::remediation::RollbackState>,
}
#[derive(Deserialize)]
pub(super) struct CreateApprovalRequest {
    detection_id: String,
    playbook_id: String,
    #[serde(default)]
    rule_id: String,
    #[serde(default)]
    environment_archetype: String,
    #[serde(default)]
    site_id: String,
    #[serde(default)]
    trust_key: String,
    #[serde(default)]
    steps_json: String,
    #[serde(default)]
    rollback_steps_json: String,
}
#[derive(Serialize)]
pub(super) struct CreateApprovalResponse {
    success: bool,
    error: String,
    proposal_id: String,
    trust_state: String,
}
#[derive(Deserialize)]
pub(super) struct ApprovalDecisionRequest {
    #[serde(default)]
    operator_note: String,
}
#[derive(Serialize)]
pub(super) struct ApprovalDecisionResponse {
    success: bool,
    error: String,
    remediation_id: String,
}
#[derive(Deserialize)]
pub(super) struct TrustGraduateRequest {
    trust_key: String,
    to_state: String,
    #[serde(default)]
    operator_note: String,
}
#[derive(Deserialize)]
pub(super) struct ProposalGnmiSet {
    path: String,
    value: String,
}
#[derive(Deserialize)]
pub(super) struct ProposalVerifyGraph {
    expected_graph_state: String,
    #[serde(default = "default_verify_wait_secs")]
    wait_seconds: u64,
}
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum ProposalStep {
    GnmiSet { gnmi_set: ProposalGnmiSet },
    Sleep { sleep: serde_json::Value },
    VerifyGraph { verify_graph: ProposalVerifyGraph },
}
pub(super) struct ProposalExecutionReport {
    steps_executed: usize,
}
pub(super) struct HttpTargetConnInfo {
    address: String,
    username: Option<String>,
    password: Option<String>,
    ca_cert_pem: Option<Vec<u8>>,
    tls_domain: String,
}
#[derive(Deserialize)]
pub(super) struct SnowTestRequest {
    instance_url: String,
    credential_alias: String,
}
#[derive(Serialize)]
pub(super) struct SnowTestResponse {
    success: bool,
    message: String,
}
#[derive(Serialize)]
pub(super) struct SnowAiopsSyncResponse {
    success: bool,
    error: String,
    stats: crate::integrations::servicenow_aiops::SyncStats,
}
#[derive(serde::Deserialize)]
pub struct RemoveOverrideReq {
    pub scope: crate::registry::OverrideScope,
    pub path: String,
}
#[derive(Deserialize)]
pub(super) struct CreateInvestigationBody {
    detection_id: String,
    device_address: String,
    #[serde(default = "default_trigger")]
    trigger: String,
}
#[derive(Deserialize)]
pub(super) struct InvestigationFeedbackBody {
    rating: String,
    #[serde(default)]
    comment: String,
    #[serde(default)]
    operator: String,
}
#[derive(Deserialize)]
pub(super) struct CompleteInvestigationBody {
    status: String,
    summary: String,
    #[serde(default)]
    proposal_json: String,
    #[serde(default)]
    tokens_used: i64,
    #[serde(default)]
    cost_usd: f64,
}
#[derive(Serialize)]
pub(super) struct InvestigationDetailResponse {
    investigation: crate::graph::InvestigationRecord,
    tool_calls: Vec<crate::graph::ToolCallRecord>,
}
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use axum::{Json, extract::{Path, Query, State}, http::StatusCode, response::IntoResponse};
use lbug::{Connection, Value};

use super::AppState;
use super::{default_proposal_status, default_verify_wait_secs, default_limit, default_trigger, now_ns};
use crate::audit;
use crate::config::TargetConfig;
use crate::credentials::{CredentialVault, ResolvePurpose, ResolvedCredential};
use crate::gnmi_set::gnmi_set;
use crate::graph::RemediationProposalRow;
use crate::remediation::{
    TrustKey, TrustState, check_graduation,
};


pub(super) async fn approvals_list_handler(
    State(state): State<AppState>,
    Query(params): Query<ApprovalsParams>,
) -> Result<Json<ApprovalsListResponse>, (StatusCode, String)> {
    let proposals = state
        .store
        .read_remediation_proposals(Some(params.status), params.limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

    let (trust, graduation_hints) = {
        let store = state.trust_store.read().await;
        let records = store.list();
        let trust = records
            .iter()
            .map(|(trust_key, record)| TrustEntry {
                trust_key: trust_key.clone(),
                record: record.clone(),
            })
            .collect();
        let graduation_hints = records
            .iter()
            .filter_map(|(trust_key, record)| {
                check_graduation(
                    trust_key,
                    record,
                    state
                        .remediation_config
                        .graduation
                        .consecutive_approvals_required,
                )
            })
            .collect();
        (trust, graduation_hints)
    };

    let active_rollbacks = {
        let mut registry = state.rollback_registry.write().await;
        let now = now_ns();
        registry.prune(now);
        registry.active_windows(now).into_iter().cloned().collect()
    };

    Ok(Json(ApprovalsListResponse {
        proposals,
        trust,
        graduation_hints,
        active_rollbacks,
    }))
}
pub(super) async fn approvals_create_handler(
    State(state): State<AppState>,
    Json(req): Json<CreateApprovalRequest>,
) -> Json<CreateApprovalResponse> {
    let trust_key = if req.trust_key.trim().is_empty() {
        TrustKey::new(
            &req.rule_id,
            &req.environment_archetype,
            &req.site_id,
            &req.playbook_id,
        )
        .to_storage_key()
    } else {
        req.trust_key.clone()
    };

    let trust_state = {
        let mut store = state.trust_store.write().await;
        let key = trust_key_from_storage(&trust_key);
        store.get_or_default(&key).state.as_str().to_string()
    };

    match state
        .store
        .write_remediation_proposal(
            req.detection_id,
            req.playbook_id,
            trust_key,
            req.steps_json,
            req.rollback_steps_json,
            now_ns(),
        )
        .await
    {
        Ok(proposal_id) => Json(CreateApprovalResponse {
            success: true,
            error: String::new(),
            proposal_id,
            trust_state,
        }),
        Err(e) => Json(CreateApprovalResponse {
            success: false,
            error: format!("{e:#}"),
            proposal_id: String::new(),
            trust_state,
        }),
    }
}
pub(super) async fn approvals_approve_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ApprovalDecisionRequest>,
) -> Json<ApprovalDecisionResponse> {
    let proposal = match find_proposal(&state, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            return Json(ApprovalDecisionResponse {
                success: false,
                error: format!("proposal '{id}' not found"),
                remediation_id: String::new(),
            });
        }
        Err(e) => {
            return Json(ApprovalDecisionResponse {
                success: false,
                error: e,
                remediation_id: String::new(),
            });
        }
    };

    let now = now_ns();
    let execution =
        execute_proposal_steps(&state, &proposal.device_address, &proposal.steps_json).await;
    let (proposal_status, remediation_status, detail_json) = match execution {
        Ok(report) => (
            "approved".to_string(),
            "success".to_string(),
            serde_json::json!({
                "proposal_id": proposal.id,
                "operator_note": req.operator_note,
                "steps_executed": report.steps_executed,
                "steps": proposal.steps_json,
            })
            .to_string(),
        ),
        Err(e) => (
            "failed".to_string(),
            "failed".to_string(),
            serde_json::json!({
                "proposal_id": proposal.id,
                "operator_note": req.operator_note,
                "error": e,
                "steps": proposal.steps_json,
            })
            .to_string(),
        ),
    };

    if let Err(e) = state
        .store
        .decide_remediation_proposal(
            id.clone(),
            proposal_status.clone(),
            req.operator_note.clone(),
            now,
        )
        .await
    {
        return Json(ApprovalDecisionResponse {
            success: false,
            error: format!("{e:#}"),
            remediation_id: String::new(),
        });
    }

    let remediation_id = match state
        .store
        .write_remediation(
            proposal.detection_id.clone(),
            proposal.playbook_id.clone(),
            remediation_status.clone(),
            detail_json,
            now,
            now,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            return Json(ApprovalDecisionResponse {
                success: false,
                error: format!("{e:#}"),
                remediation_id: String::new(),
            });
        }
    };

    let trust_key = trust_key_from_storage(&proposal.trust_key);
    if remediation_status == "success" {
        state
            .trust_store
            .write()
            .await
            .record_approval(&trust_key, now);
    } else {
        state
            .trust_store
            .write()
            .await
            .record_failure(&trust_key, now);
    }
    let _ = audit::append_trust_operation(
        std::path::Path::new(&state.runtime_dir),
        now,
        &proposal.trust_key,
        if remediation_status == "success" {
            "approve"
        } else {
            "approve_failed"
        },
        &id,
        Some(&req.operator_note),
    );

    if remediation_status == "success" && !proposal.rollback_steps_json.trim().is_empty() {
        state
            .rollback_registry
            .write()
            .await
            .register(crate::remediation::RollbackState {
                proposal_id: id,
                remediation_id: remediation_id.clone(),
                executed_at_ns: now,
                window_secs: state.remediation_config.rollback_window_secs,
                snapshot_json: proposal.rollback_steps_json,
                rolled_back: false,
            });
    }

    Json(ApprovalDecisionResponse {
        success: remediation_status == "success",
        error: if remediation_status == "success" {
            String::new()
        } else {
            "proposal execution failed".to_string()
        },
        remediation_id,
    })
}
pub(super) async fn approvals_reject_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ApprovalDecisionRequest>,
) -> Json<ApprovalDecisionResponse> {
    let proposal = match find_proposal(&state, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            return Json(ApprovalDecisionResponse {
                success: false,
                error: format!("proposal '{id}' not found"),
                remediation_id: String::new(),
            });
        }
        Err(e) => {
            return Json(ApprovalDecisionResponse {
                success: false,
                error: e,
                remediation_id: String::new(),
            });
        }
    };

    let now = now_ns();
    if let Err(e) = state
        .store
        .decide_remediation_proposal(
            id.clone(),
            "rejected".to_string(),
            req.operator_note.clone(),
            now,
        )
        .await
    {
        return Json(ApprovalDecisionResponse {
            success: false,
            error: format!("{e:#}"),
            remediation_id: String::new(),
        });
    }
    let trust_key = trust_key_from_storage(&proposal.trust_key);
    state
        .trust_store
        .write()
        .await
        .record_rejection(&trust_key, now);
    let _ = audit::append_trust_operation(
        std::path::Path::new(&state.runtime_dir),
        now,
        &proposal.trust_key,
        "reject",
        &id,
        Some(&req.operator_note),
    );

    Json(ApprovalDecisionResponse {
        success: true,
        error: String::new(),
        remediation_id: String::new(),
    })
}
pub(super) async fn approvals_rollback_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ApprovalDecisionRequest>,
) -> Json<ApprovalDecisionResponse> {
    let proposal = match find_proposal(&state, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            return Json(ApprovalDecisionResponse {
                success: false,
                error: format!("proposal '{id}' not found"),
                remediation_id: String::new(),
            });
        }
        Err(e) => {
            return Json(ApprovalDecisionResponse {
                success: false,
                error: e,
                remediation_id: String::new(),
            });
        }
    };

    let now = now_ns();
    let rollback_state = {
        let registry = state.rollback_registry.read().await;
        registry.get(&id).cloned()
    };
    let Some(rollback_state) = rollback_state else {
        return Json(ApprovalDecisionResponse {
            success: false,
            error: "rollback window is not active".to_string(),
            remediation_id: String::new(),
        });
    };
    if rollback_state.is_expired(now) {
        return Json(ApprovalDecisionResponse {
            success: false,
            error: "rollback window expired".to_string(),
            remediation_id: String::new(),
        });
    }

    if let Err(e) = execute_proposal_steps(
        &state,
        &proposal.device_address,
        &rollback_state.snapshot_json,
    )
    .await
    {
        let trust_key = trust_key_from_storage(&proposal.trust_key);
        {
            let mut trust = state.trust_store.write().await;
            trust.record_failure(&trust_key, now);
            trust.set_state(&trust_key, TrustState::ApproveEach, now);
        }
        let _ = audit::append_trust_operation(
            std::path::Path::new(&state.runtime_dir),
            now,
            &proposal.trust_key,
            "rollback_failed",
            &id,
            Some(&format!("{}; error={e}", req.operator_note)),
        );
        return Json(ApprovalDecisionResponse {
            success: false,
            error: e,
            remediation_id: rollback_state.remediation_id,
        });
    }

    if let Err(e) = state
        .store
        .decide_remediation_proposal(
            id.clone(),
            "rolled_back".to_string(),
            req.operator_note.clone(),
            now,
        )
        .await
    {
        return Json(ApprovalDecisionResponse {
            success: false,
            error: format!("{e:#}"),
            remediation_id: String::new(),
        });
    }
    state.rollback_registry.write().await.mark_rolled_back(&id);
    let trust_key = trust_key_from_storage(&proposal.trust_key);
    {
        let mut trust = state.trust_store.write().await;
        trust.record_failure(&trust_key, now);
        trust.set_state(&trust_key, TrustState::ApproveEach, now);
    }
    let _ = audit::append_trust_operation(
        std::path::Path::new(&state.runtime_dir),
        now,
        &proposal.trust_key,
        "rollback",
        &id,
        Some(&req.operator_note),
    );

    Json(ApprovalDecisionResponse {
        success: true,
        error: String::new(),
        remediation_id: rollback_state.remediation_id,
    })
}
pub(super) async fn trust_list_handler(State(state): State<AppState>) -> Json<ApprovalsListResponse> {
    let (trust, graduation_hints) = {
        let store = state.trust_store.read().await;
        let records = store.list();
        let trust = records
            .iter()
            .map(|(trust_key, record)| TrustEntry {
                trust_key: trust_key.clone(),
                record: record.clone(),
            })
            .collect();
        let graduation_hints = records
            .iter()
            .filter_map(|(trust_key, record)| {
                check_graduation(
                    trust_key,
                    record,
                    state
                        .remediation_config
                        .graduation
                        .consecutive_approvals_required,
                )
            })
            .collect();
        (trust, graduation_hints)
    };
    let active_rollbacks = {
        let registry = state.rollback_registry.read().await;
        registry
            .active_windows(now_ns())
            .into_iter()
            .cloned()
            .collect()
    };
    Json(ApprovalsListResponse {
        proposals: Vec::new(),
        trust,
        graduation_hints,
        active_rollbacks,
    })
}
pub(super) async fn trust_graduate_handler(
    State(state): State<AppState>,
    Json(req): Json<TrustGraduateRequest>,
) -> Json<ApprovalDecisionResponse> {
    let now = now_ns();
    let key = trust_key_from_storage(&req.trust_key);
    let state_to = TrustState::parse_state(&req.to_state);
    state
        .trust_store
        .write()
        .await
        .set_state(&key, state_to, now);
    let _ = audit::append_trust_operation(
        std::path::Path::new(&state.runtime_dir),
        now,
        &req.trust_key,
        "graduate",
        "",
        Some(&req.operator_note),
    );
    Json(ApprovalDecisionResponse {
        success: true,
        error: String::new(),
        remediation_id: String::new(),
    })
}
pub(super) async fn find_proposal(
    state: &AppState,
    id: &str,
) -> Result<Option<RemediationProposalRow>, String> {
    state
        .store
        .read_remediation_proposals(Some("all".to_string()), 500)
        .await
        .map_err(|e| format!("{e:#}"))
        .map(|rows| rows.into_iter().find(|p| p.id == id))
}
pub(super) fn trust_key_from_storage(key: &str) -> TrustKey {
    let mut parts = key.splitn(4, ':');
    TrustKey::new(
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    )
}
pub(super) async fn execute_proposal_steps(
    state: &AppState,
    device_address: &str,
    steps_json: &str,
) -> Result<ProposalExecutionReport, String> {
    let steps: Vec<ProposalStep> = if steps_json.trim().is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(steps_json).map_err(|e| format!("invalid proposal steps_json: {e}"))?
    };

    if steps.is_empty() {
        return Ok(ProposalExecutionReport { steps_executed: 0 });
    }
    if device_address.trim().is_empty() {
        return Err("proposal is not linked to a device address".to_string());
    }

    let mut executed = 0usize;
    for step in steps {
        match step {
            ProposalStep::GnmiSet { gnmi_set: op } => {
                let conn = target_conn_info_for_http(state, device_address).await?;
                gnmi_set(
                    &conn.address,
                    conn.username.as_deref(),
                    conn.password.as_deref(),
                    conn.ca_cert_pem.as_deref(),
                    &conn.tls_domain,
                    &op.path,
                    &op.value,
                )
                .await
                .map_err(|e| format!("{e:#}"))?;
                executed += 1;
            }
            ProposalStep::Sleep { sleep } => {
                let secs = sleep
                    .as_f64()
                    .or_else(|| sleep.as_str().and_then(|s| s.parse::<f64>().ok()))
                    .unwrap_or(0.0)
                    .clamp(0.0, 300.0);
                if secs > 0.0 {
                    tokio::time::sleep(std::time::Duration::from_secs_f64(secs)).await;
                }
                executed += 1;
            }
            ProposalStep::VerifyGraph { verify_graph } => {
                verify_graph_state(
                    state,
                    &verify_graph.expected_graph_state,
                    verify_graph.wait_seconds,
                )
                .await?;
                executed += 1;
            }
        }
    }

    Ok(ProposalExecutionReport {
        steps_executed: executed,
    })
}
pub(super) async fn verify_graph_state(
    state: &AppState,
    cypher: &str,
    wait_seconds: u64,
) -> Result<(), String> {
    if cypher.trim().is_empty() {
        return Ok(());
    }
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(wait_seconds.clamp(1, 300));
    loop {
        let db = state.store.db();
        let query = cypher.to_string();
        let verified = tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            let mut rows = conn.query(&query).map_err(|e| e.to_string())?;
            let Some(row) = rows.next() else {
                return Ok::<bool, String>(false);
            };
            Ok(row.first().map(value_truthy).unwrap_or(false))
        })
        .await
        .map_err(|e| format!("verification task failed: {e}"))??;
        if verified {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err("graph verification timed out".to_string());
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}
pub(super) fn value_truthy(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Int64(n) => *n > 0,
        Value::String(s) => s == "true" || s == "1",
        _ => false,
    }
}
pub(super) async fn target_conn_info_for_http(
    state: &AppState,
    device_address: &str,
) -> Result<HttpTargetConnInfo, String> {
    let target = state
        .registry
        .get_device(device_address)
        .map_err(|e| format!("{e:#}"))?
        .ok_or_else(|| format!("unknown target '{device_address}'"))?;

    let ca_cert_pem = match &target.ca_cert {
        Some(path) if !path.is_empty() => Some(
            tokio::fs::read(path)
                .await
                .map_err(|e| format!("could not read CA cert from '{path}': {e}"))?,
        ),
        _ => None,
    };

    let resolved_credentials = resolve_http_target_credentials(&target, &state.credentials)
        .map_err(|e| format!("{e:#}"))?;
    let (username, password) = match resolved_credentials {
        Some(credentials) => (Some(credentials.username), Some(credentials.password)),
        None => (None, None),
    };

    Ok(HttpTargetConnInfo {
        address: target.address,
        username,
        password,
        ca_cert_pem,
        tls_domain: target.tls_domain.unwrap_or_default(),
    })
}
pub(super) fn resolve_http_target_credentials(
    target: &TargetConfig,
    credentials: &CredentialVault,
) -> anyhow::Result<Option<ResolvedCredential>> {
    if let Some(alias) = target.credential_alias.as_deref()
        && !alias.is_empty()
    {
        return credentials
            .resolve(alias, ResolvePurpose::Remediate)
            .map(Some);
    }

    Ok(match (&target.username, &target.password) {
        (Some(username), Some(password)) if !username.is_empty() || !password.is_empty() => {
            Some(ResolvedCredential {
                username: username.clone(),
                password: password.clone(),
            })
        }
        _ => None,
    })
}
pub(super) async fn snow_integration_test_handler(
    State(state): State<AppState>,
    Json(req): Json<SnowTestRequest>,
) -> Json<SnowTestResponse> {
    let instance_url = req.instance_url.trim_end_matches('/').to_string();

    let cred = match state.credentials.resolve(
        &req.credential_alias,
        crate::credentials::ResolvePurpose::ServiceNowAdmin,
    ) {
        Ok(c) => c,
        Err(e) => {
            return Json(SnowTestResponse {
                success: false,
                message: format!("credential resolve failed: {e:#}"),
            });
        }
    };

    let url = format!("{instance_url}/api/now/table/sys_properties?sysparm_limit=1");
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Json(SnowTestResponse {
                success: false,
                message: e.to_string(),
            });
        }
    };

    match client
        .get(&url)
        .basic_auth(&cred.username, Some(&cred.password))
        .send()
        .await
    {
        Err(e) => Json(SnowTestResponse {
            success: false,
            message: format!("{e:#}"),
        }),
        Ok(resp) if resp.status().is_success() => Json(SnowTestResponse {
            success: true,
            message: "ServiceNow connection successful".to_string(),
        }),
        Ok(resp) => Json(SnowTestResponse {
            success: false,
            message: format!("ServiceNow returned {}", resp.status()),
        }),
    }
}
pub(super) async fn servicenow_aiops_sync_handler(
    State(state): State<AppState>,
) -> Json<SnowAiopsSyncResponse> {
    match crate::integrations::servicenow_aiops::run_sync_cycle(
        &state.servicenow_config,
        &state.store,
        &state.credentials,
    )
    .await
    {
        Ok(stats) => Json(SnowAiopsSyncResponse {
            success: true,
            error: String::new(),
            stats,
        }),
        Err(e) => Json(SnowAiopsSyncResponse {
            success: false,
            error: format!("{e:#}"),
            stats: crate::integrations::servicenow_aiops::SyncStats::default(),
        }),
    }
}
pub(super) async fn list_overrides(State(state): State<AppState>) -> impl axum::response::IntoResponse {
    match state.registry.list_overrides() {
        Ok(overrides) => Json(overrides).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to list overrides: {}", e),
        )
            .into_response(),
    }
}
pub(super) async fn add_override(
    State(state): State<AppState>,
    Json(mut req): Json<crate::registry::PathOverride>,
) -> impl axum::response::IntoResponse {
    let actor = std::env::var("BONSAI_OPERATOR")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default();

    req.created_at_ns = now;
    req.created_by = actor.clone();

    match state.registry.add_override(req.clone()) {
        Ok(_) => Json(req).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to add override: {}", e),
        )
            .into_response(),
    }
}
pub(super) async fn remove_override(
    State(state): State<AppState>,
    Json(req): Json<RemoveOverrideReq>,
) -> impl axum::response::IntoResponse {
    match state.registry.remove_override(&req.scope, &req.path) {
        Ok(removed) => Json(serde_json::json!({ "removed": removed })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to remove override: {}", e),
        )
            .into_response(),
    }
}
pub(super) async fn list_investigations_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .store
        .list_investigations()
        .await
        .map(|inv| Json(serde_json::json!({ "investigations": inv })))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
pub(super) async fn create_investigation_handler(
    State(state): State<AppState>,
    Json(body): Json<CreateInvestigationBody>,
) -> Result<Json<crate::graph::InvestigationRecord>, (StatusCode, String)> {
    let detection_id = body.detection_id.clone();
    let device_address = body.device_address.clone();
    let inv = state
        .store
        .create_investigation(body.detection_id, body.device_address, body.trigger)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Spawn AI investigation if provider is configured and API key is available.
    let ai_cfg = state.ai_config.clone();
    if std::env::var(&ai_cfg.api_key_env).is_ok() {
        let store_arc = std::sync::Arc::clone(&state.store);
        crate::investigation_runtime::spawn_investigation(
            inv.id.clone(),
            device_address,
            if detection_id.is_empty() { None } else { Some(detection_id) },
            store_arc,
            state,
            ai_cfg,
        );
    }

    Ok(Json(inv))
}
pub(super) async fn get_investigation_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<InvestigationDetailResponse>, (StatusCode, String)> {
    let inv = state
        .store
        .get_investigation(id.clone())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("investigation {} not found", id),
            )
        })?;
    let tool_calls = state
        .store
        .list_tool_calls(id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(InvestigationDetailResponse {
        investigation: inv,
        tool_calls,
    }))
}
pub(super) async fn list_tool_calls_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .store
        .list_tool_calls(id)
        .await
        .map(|tc| Json(serde_json::json!({ "tool_calls": tc })))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
pub(super) async fn complete_investigation_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<CompleteInvestigationBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .store
        .complete_investigation(
            id,
            body.status,
            body.summary,
            body.proposal_json,
            body.tokens_used,
            body.cost_usd,
        )
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
pub(super) async fn grounded_incident_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<crate::mcp_server::GroundedIncidentResponse>, (StatusCode, String)> {
    let detections = state
        .store
        .read_detections(500)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let det = detections.into_iter().find(|d| d.id == id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("DetectionEvent {id} not found"),
        )
    })?;

    let device_address = det.device_address.clone();
    let db = state.store.db();
    let blast = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::queries::blast_radius(&conn, &device_address, 2).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let meta = crate::mcp_server::rule_meta(&det.rule_id);
    let refs = crate::mcp_server::procedural_refs(&det.device_address, &det.rule_id);

    Ok(Json(crate::mcp_server::GroundedIncidentResponse {
        detection: crate::mcp_server::DetectionSummary {
            id: det.id,
            device_address: det.device_address,
            rule_id: det.rule_id,
            severity: det.severity,
            fired_at_ns: det.fired_at_ns,
            features_json: det.features_json,
            remediation_status: det.remediation_status,
            remediation_action: det.remediation_action,
        },
        blast_radius: blast,
        rule_description: meta.map(|m| m.description).unwrap_or(""),
        recurrence_indicators: meta.map(|m| m.recurrence_indicators).unwrap_or(&[]),
        procedural_references: refs,
    }))
}

// ── Change Management handlers ──────────────────────────────────────────────

/// POST /api/webhooks/change-event — ingest a change event from AAP, Ansible
/// Tower, ServiceNow business rule, or any external system.
pub(super) async fn webhook_change_event_handler(
    State(state): State<AppState>,
    Json(body): Json<crate::integrations::change_management::WebhookChangeEvent>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !state.servicenow_config.change_management.webhook_enabled {
        return Err((
            StatusCode::FORBIDDEN,
            "change management webhook is not enabled".to_string(),
        ));
    }
    let record = crate::integrations::change_management::ingest_webhook_change(&state.store, body)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(serde_json::json!({
        "success": true,
        "change_request_id": record.id,
        "number": record.number,
    })))
}

/// GET /api/changes/context/{device_address} — check if a device is in an
/// active change window right now.
pub(super) async fn change_context_handler(
    State(state): State<AppState>,
    Path(device_address): Path<String>,
) -> Result<Json<crate::integrations::change_management::ActiveChangeContext>, (StatusCode, String)>
{
    crate::integrations::change_management::active_change_context(&state.store, &device_address)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
}

/// POST /api/integrations/servicenow/changes/sync — trigger a manual change
/// request sync cycle.
pub(super) async fn servicenow_change_sync_handler(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    match crate::integrations::change_management::run_change_sync(
        &state.servicenow_config,
        &state.store,
        &state.credentials,
    )
    .await
    {
        Ok(stats) => Json(serde_json::json!({
            "success": true,
            "stats": stats,
        })),
        Err(e) => Json(serde_json::json!({
            "success": false,
            "error": format!("{e:#}"),
        })),
    }
}

#[derive(Deserialize)]
pub(super) struct ChangeListParams {
    #[serde(default = "default_change_state")]
    state: String,
    #[serde(default = "default_change_limit")]
    limit: u32,
}
fn default_change_state() -> String {
    String::new()
}
fn default_change_limit() -> u32 {
    100
}

/// GET /api/changes — list change requests from the graph.
pub(super) async fn list_changes_handler(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<ChangeListParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let db = state.store.db();
    let filter_state = params.state.clone();
    let limit = params.limit.min(500) as i64;
    let rows = tokio::task::spawn_blocking(move || -> Result<Vec<serde_json::Value>, String> {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let query = if filter_state.is_empty() {
            "MATCH (c:ChangeRequest) \
             RETURN c.id, c.number, c.source, c.short_description, c.state, c.change_type, \
                    c.risk, c.planned_start_ns, c.planned_end_ns, c.affected_cis_json, \
                    c.assigned_to, c.assignment_group, c.correlation_id, c.external_ref \
             ORDER BY c.planned_start_ns DESC \
             LIMIT $limit"
                .to_string()
        } else {
            format!(
                "MATCH (c:ChangeRequest) WHERE c.state = '{}' \
                 RETURN c.id, c.number, c.source, c.short_description, c.state, c.change_type, \
                        c.risk, c.planned_start_ns, c.planned_end_ns, c.affected_cis_json, \
                        c.assigned_to, c.assignment_group, c.correlation_id, c.external_ref \
                 ORDER BY c.planned_start_ns DESC \
                 LIMIT $limit",
                filter_state
            )
        };
        let mut stmt = conn.prepare(&query).map_err(|e| e.to_string())?;
        let result = conn
            .execute(&mut stmt, vec![("limit", Value::Int64(limit))])
            .map_err(|e| e.to_string())?;
        let rows: Vec<serde_json::Value> = result
            .map(|row| {
                serde_json::json!({
                    "id": crate::graph::common::read_str(&row[0]),
                    "number": crate::graph::common::read_str(&row[1]),
                    "source": crate::graph::common::read_str(&row[2]),
                    "short_description": crate::graph::common::read_str(&row[3]),
                    "state": crate::graph::common::read_str(&row[4]),
                    "change_type": crate::graph::common::read_str(&row[5]),
                    "risk": crate::graph::common::read_str(&row[6]),
                    "planned_start_ns": match &row[7] { Value::Int64(v) => *v, _ => 0 },
                    "planned_end_ns": match &row[8] { Value::Int64(v) => *v, _ => 0 },
                    "affected_cis_json": crate::graph::common::read_str(&row[9]),
                    "assigned_to": crate::graph::common::read_str(&row[10]),
                    "assignment_group": crate::graph::common::read_str(&row[11]),
                    "correlation_id": crate::graph::common::read_str(&row[12]),
                    "external_ref": crate::graph::common::read_str(&row[13]),
                })
            })
            .collect();
        Ok(rows)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(serde_json::json!({ "changes": rows })))
}

// ── GET /api/playbooks ────────────────────────────────────────────────────────

pub async fn playbooks_catalog_handler(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let lib = crate::playbook::PlaybookLibrary::load_dir(&state.remediation_config.playbook_library_dir);
    let entries: Vec<serde_json::Value> = lib
        .catalog()
        .iter()
        .flat_map(|(rule_id, pbs)| {
            let rid = rule_id.clone();
            pbs.iter().map(move |pb| serde_json::json!({
                "rule_id": rid,
                "name": pb.name,
                "vendor": pb.vendor,
                "operation": pb.operation,
                "description": pb.description,
                "risk_tier": pb.risk_tier,
            }))
        })
        .collect();
    Json(serde_json::json!({ "playbooks": entries }))
}

// ── GET /api/audit ────────────────────────────────────────────────────────────

#[derive(serde::Deserialize, Default)]
pub struct AuditQuery {
    #[serde(default = "default_audit_limit")]
    limit: usize,
}
fn default_audit_limit() -> usize { 200 }

pub async fn audit_log_handler(
    State(state): State<AppState>,
    Query(q): Query<AuditQuery>,
) -> Json<serde_json::Value> {
    let root = std::path::Path::new(&state.runtime_dir);
    let entries = crate::audit::read_recent(root, q.limit.min(1000));
    Json(serde_json::json!({ "entries": entries }))
}

// ── D4-14 T4: Vault rekey ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct VaultRekeyBody {
    new_passphrase_env: String,
}

pub(super) async fn vault_rekey_handler(
    State(state): State<AppState>,
    Json(body): Json<VaultRekeyBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let env_var = body.new_passphrase_env.trim().to_string();
    if env_var.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "new_passphrase_env is required".to_string(),
        ));
    }
    let creds = Arc::clone(&state.credentials);
    tokio::task::spawn_blocking(move || {
        creds.rekey(&env_var)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

    let now = now_ns();
    let _ = audit::append_trust_operation(
        std::path::Path::new(&state.runtime_dir),
        now,
        "vault",
        "rekey",
        "",
        Some(&format!("rekey via env var {}", body.new_passphrase_env)),
    );

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Vault re-keyed successfully. Restart bonsai with the new passphrase."
    })))
}

// ── D4-8 T2: Investigation feedback ──────────────────────────────────────────

pub(super) async fn investigation_feedback_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<InvestigationFeedbackBody>,
) -> Result<Json<crate::graph::FeedbackRecord>, (StatusCode, String)> {
    let rating = body.rating.trim().to_lowercase();
    if rating != "positive" && rating != "negative" {
        return Err((
            StatusCode::BAD_REQUEST,
            "rating must be 'positive' or 'negative'".to_string(),
        ));
    }
    let operator = if body.operator.is_empty() {
        std::env::var("BONSAI_OPERATOR").unwrap_or_else(|_| "unknown".to_string())
    } else {
        body.operator
    };
    state
        .store
        .add_investigation_feedback(id, rating, body.comment, operator)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
}

pub(super) async fn investigation_accuracy_handler(
    State(state): State<AppState>,
) -> Result<Json<crate::graph::InvestigationAccuracy>, (StatusCode, String)> {
    state
        .store
        .investigation_accuracy()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
}
