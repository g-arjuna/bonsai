#![allow(unused_imports, dead_code, unused_variables)]
use super::*;

// ── Human-in-the-loop remediation approvals (Sprint 4) ───────────────────────

#[derive(Deserialize)]
struct ApprovalsParams {
    #[serde(default = "default_proposal_status")]
    status: String,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_proposal_status() -> String {
    "pending".to_string()
}

#[derive(Serialize)]
struct TrustEntry {
    trust_key: String,
    record: crate::remediation::TrustRecord,
}

#[derive(Serialize)]
struct ApprovalsListResponse {
    proposals: Vec<RemediationProposalRow>,
    trust: Vec<TrustEntry>,
    graduation_hints: Vec<crate::remediation::GraduationHint>,
    active_rollbacks: Vec<crate::remediation::RollbackState>,
}

async fn approvals_list_handler(
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

#[derive(Deserialize)]
struct CreateApprovalRequest {
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
struct CreateApprovalResponse {
    success: bool,
    error: String,
    proposal_id: String,
    trust_state: String,
}

async fn approvals_create_handler(
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

#[derive(Deserialize)]
struct ApprovalDecisionRequest {
    #[serde(default)]
    operator_note: String,
}

#[derive(Serialize)]
struct ApprovalDecisionResponse {
    success: bool,
    error: String,
    remediation_id: String,
}

async fn approvals_approve_handler(
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

async fn approvals_reject_handler(
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

async fn approvals_rollback_handler(
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

async fn trust_list_handler(State(state): State<AppState>) -> Json<ApprovalsListResponse> {
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

#[derive(Deserialize)]
struct TrustGraduateRequest {
    trust_key: String,
    to_state: String,
    #[serde(default)]
    operator_note: String,
}

async fn trust_graduate_handler(
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

async fn find_proposal(
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

fn trust_key_from_storage(key: &str) -> TrustKey {
    let mut parts = key.splitn(4, ':');
    TrustKey::new(
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    )
}

#[derive(Deserialize)]
struct ProposalGnmiSet {
    path: String,
    value: String,
}

#[derive(Deserialize)]
struct ProposalVerifyGraph {
    expected_graph_state: String,
    #[serde(default = "default_verify_wait_secs")]
    wait_seconds: u64,
}

fn default_verify_wait_secs() -> u64 {
    30
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ProposalStep {
    GnmiSet { gnmi_set: ProposalGnmiSet },
    Sleep { sleep: serde_json::Value },
    VerifyGraph { verify_graph: ProposalVerifyGraph },
}

struct ProposalExecutionReport {
    steps_executed: usize,
}

async fn execute_proposal_steps(
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

async fn verify_graph_state(
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

fn value_truthy(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Int64(n) => *n > 0,
        Value::String(s) => s == "true" || s == "1",
        _ => false,
    }
}

struct HttpTargetConnInfo {
    address: String,
    username: Option<String>,
    password: Option<String>,
    ca_cert_pem: Option<Vec<u8>>,
    tls_domain: String,
}

async fn target_conn_info_for_http(
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

fn resolve_http_target_credentials(
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

