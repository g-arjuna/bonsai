use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::ai_provider::{AiMessage, AiProvider, build_provider_with_key};
use crate::config::AiConfig;
use crate::graph::GraphStore;
use crate::graph::algorithms::{GraphQuality, graph_quality};
use crate::http_server::AppState;
use crate::http_server::nl_query::GRAPH_SCHEMA;

const MAX_ITERATIONS: usize = 15;

/// Honest terminal states for an investigation run (A03).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationTerminalState {
    Completed,
    Partial,
    BudgetExhausted,
    Failed,
    Cancelled,
    AwaitingHuman,
}

impl InvestigationTerminalState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::BudgetExhausted => "budget_exhausted",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::AwaitingHuman => "awaiting_human",
        }
    }
}

/// Reason why the investigation runtime exited.
#[derive(Debug, Clone, PartialEq)]
pub enum LoopExitReason {
    ModelEndTurn,
    IterationLimitReached { max_iterations: usize },
    RepeatedToolCallDetected { tool_name: String },
    PerInvestigationBudgetExceeded { cost: f64, budget: f64 },
    DailyBudgetExceeded { spent: f64, daily_budget: f64 },
    DailyBudgetCheckFailed { error: String },
    ProviderUnavailable { error: String },
    ProviderCallFailed { error: String, iteration: usize },
    Cancelled { reason: String },
}

fn default_epistemic_state() -> String {
    "plausible".to_string()
}

fn default_true() -> bool {
    true
}

/// Validated structured RCA payload (A03).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypedInvestigationRca {
    pub root_cause_type: String,
    pub confidence: f64,
    #[serde(default = "default_true")]
    pub confidence_is_uncalibrated: bool,
    #[serde(default = "default_epistemic_state")]
    pub epistemic_state: String,
    #[serde(default)]
    pub affected_scope: Vec<String>,
    pub recommended_action: String,
    #[serde(default)]
    pub missing_data: Vec<String>,
    #[serde(default)]
    pub supporting_evidence_ids: Vec<String>,
    #[serde(default)]
    pub counterevidence_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_check: Option<String>,
}

/// Resolved terminal outcome for persistence in `GraphStore::complete_investigation`.
#[derive(Debug, Clone, PartialEq)]
pub struct InvestigationOutcome {
    pub state: InvestigationTerminalState,
    pub stop_reason: String,
    pub summary: String,
    pub proposal_json: String,
    pub validated_rca: Option<TypedInvestigationRca>,
}

/// Pre-fetch device and incident context before entering the agent loop.
/// Returns a markdown-formatted string injected into the first user message,
/// giving the AI situational awareness without spending iterations on basic lookups.
async fn build_context(
    device_address: &str,
    detection_id: Option<&str>,
    state: &AppState,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    // 1. Incident details if we have a detection ID
    if let Some(did) = detection_id {
        let args = serde_json::json!({ "id": did });
        if let Ok(v) = crate::mcp_server::call_tool(state, "get_incident", &args).await {
            parts.push(format!("## Incident\n```json\n{}\n```", serde_json::to_string_pretty(&v).unwrap_or_default()));
        }
    }

    // 2. Blast radius (2 hops)
    let args = serde_json::json!({ "address": device_address, "max_hops": 2 });
    if let Ok(v) = crate::mcp_server::call_tool(state, "get_device_blast_radius", &args).await {
        parts.push(format!("## Blast Radius\n```json\n{}\n```", serde_json::to_string_pretty(&v).unwrap_or_default()));
    }

    // 3. Recent detections on this device (last 10)
    let args = serde_json::json!({ "device_address": device_address, "limit": 10 });
    if let Ok(v) = crate::mcp_server::call_tool(state, "list_active_detections", &args).await {
        parts.push(format!("## Recent Detections on Device\n```json\n{}\n```", serde_json::to_string_pretty(&v).unwrap_or_default()));
    }

    if parts.is_empty() {
        return String::new();
    }
    format!("\n\n---\n## Pre-fetched Context\n{}", parts.join("\n\n"))
}

/// Spawn a background investigation task. Returns immediately; the investigation
/// runs to completion (or budget/iteration limit) as a tokio task.
pub fn spawn_investigation(
    investigation_id: String,
    device_address: String,
    detection_id: Option<String>,
    store: Arc<GraphStore>,
    state: AppState,
    ai_cfg: AiConfig,
    api_key: String,
) {
    tokio::spawn(async move {
        run_investigation(investigation_id, device_address, detection_id, store, state, ai_cfg, api_key).await;
    });
}

async fn run_investigation(
    investigation_id: String,
    device_address: String,
    detection_id: Option<String>,
    store: Arc<GraphStore>,
    state: AppState,
    ai_cfg: AiConfig,
    api_key: String,
) {
    // Daily budget gate — fail closed if spend query fails or budget is exceeded (A03).
    let daily_budget = ai_cfg.daily_budget_usd;
    if daily_budget > 0.0 {
        match store.query_daily_investigation_cost().await {
            Ok(spent) if spent >= daily_budget => {
                warn!(
                    id = %investigation_id,
                    spent,
                    daily_budget,
                    "daily AI budget exceeded — investigation halted"
                );
                let outcome = resolve_investigation_outcome(
                    LoopExitReason::DailyBudgetExceeded { spent, daily_budget },
                    "",
                );
                let _ = store.complete_investigation(
                    investigation_id,
                    outcome.state.as_str().to_string(),
                    outcome.summary,
                    outcome.proposal_json,
                    0,
                    0.0,
                ).await;
                return;
            }
            Err(e) => {
                warn!(
                    id = %investigation_id,
                    error = %e,
                    "daily cost query failed — failing closed to prevent unbudgeted spend"
                );
                let outcome = resolve_investigation_outcome(
                    LoopExitReason::DailyBudgetCheckFailed { error: e.to_string() },
                    "",
                );
                let _ = store.complete_investigation(
                    investigation_id,
                    outcome.state.as_str().to_string(),
                    outcome.summary,
                    outcome.proposal_json,
                    0,
                    0.0,
                ).await;
                return;
            }
            _ => {}
        }
    }

    let provider: Box<dyn AiProvider> = match build_provider_with_key(&ai_cfg, api_key) {
        Ok(p) => p,
        Err(e) => {
            warn!(id = %investigation_id, error = %e, "AI provider unavailable");
            let outcome = resolve_investigation_outcome(
                LoopExitReason::ProviderUnavailable { error: e.to_string() },
                "",
            );
            let _ = store.complete_investigation(
                investigation_id,
                outcome.state.as_str().to_string(),
                outcome.summary,
                outcome.proposal_json,
                0,
                0.0,
            ).await;
            return;
        }
    };

    let tools = crate::mcp_server::ai_tool_definitions();
    let per_budget = ai_cfg.per_investigation_budget_usd;

    // D4-6 T4 & A03: Pre-flight quality check — warn if target device has missing signals or graph is sparse.
    let quality = preflight_quality(&store, &device_address).await;
    let quality_score = quality.as_ref().map(|q| q.overall_score).unwrap_or(-1.0);
    let quality_warning = evaluate_preflight_warning(&quality, &device_address);
    if let Some(ref w) = quality_warning {
        warn!(
            id = %investigation_id,
            score = quality_score,
            warning = %w,
            "evidence coverage gap detected for investigation device"
        );
    }

    let context = build_context(&device_address, detection_id.as_deref(), &state).await;
    let mut messages = vec![
        AiMessage::system(system_prompt(&device_address, detection_id.as_deref())),
        AiMessage::user(format!("{}{context}", user_prompt(&device_address, detection_id.as_deref()))),
    ];

    let mut total_cost = 0.0f64;
    let mut total_tokens = 0i64;
    let mut queried_paths: Vec<String> = Vec::new();
    let mut exit_reason = LoopExitReason::IterationLimitReached {
        max_iterations: MAX_ITERATIONS,
    };

    info!(id = %investigation_id, "investigation started");

    for iteration in 0..MAX_ITERATIONS {
        if per_budget > 0.0 && total_cost >= per_budget {
            warn!(id = %investigation_id, cost = total_cost, per_budget, "investigation budget exceeded");
            exit_reason = LoopExitReason::PerInvestigationBudgetExceeded {
                cost: total_cost,
                budget: per_budget,
            };
            break;
        }

        let resp = match provider.complete(messages.clone(), tools.clone()).await {
            Ok(r) => r,
            Err(e) => {
                warn!(id = %investigation_id, error = %e, iteration, "LLM call failed");
                exit_reason = LoopExitReason::ProviderCallFailed {
                    error: e.to_string(),
                    iteration,
                };
                break;
            }
        };

        total_tokens += resp.tokens_used as i64;
        total_cost += resp.cost_usd;

        messages.push(AiMessage {
            role: "assistant".into(),
            content: resp.content.clone(),
            tool_calls: resp.tool_calls.clone(),
            tool_call_id: None,
            raw_parts: resp.raw_parts.clone(),
        });

        if resp.tool_calls.is_empty() || resp.stop_reason == "end_turn" {
            exit_reason = LoopExitReason::ModelEndTurn;
            break;
        }

        for tc in &resp.tool_calls {
            queried_paths.push(tc.name.clone());

            let input_json = serde_json::to_string(&tc.arguments).unwrap_or_default();
            let tool_result = crate::mcp_server::call_tool(&state, &tc.name, &tc.arguments).await;
            let output_json = match &tool_result {
                Ok(v) => serde_json::to_string(v).unwrap_or_default(),
                Err(e) => serde_json::json!({ "error": e }).to_string(),
            };

            let _ = store.add_tool_call(
                investigation_id.clone(),
                tc.name.clone(),
                input_json,
                output_json.clone(),
            ).await;

            messages.push(AiMessage::tool_result(tc.id.clone(), output_json));
        }
    }

    let raw_assistant_text = messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant" && m.content.is_some())
        .and_then(|m| m.content.clone())
        .unwrap_or_default();

    let mut outcome = resolve_investigation_outcome(exit_reason, &raw_assistant_text);
    let gap_report = compute_coverage_gaps(&quality, &queried_paths, &device_address);
    if let Some(warning) = &quality_warning {
        outcome.summary = format!("{warning}\n\n{}", outcome.summary);
    }
    if !gap_report.is_empty() {
        outcome.summary.push_str("\n\n");
        outcome.summary.push_str(&gap_report);
    }

    let _ = store.complete_investigation(
        investigation_id.clone(),
        outcome.state.as_str().to_string(),
        outcome.summary,
        outcome.proposal_json,
        total_tokens,
        total_cost,
    ).await;
    info!(
        id = %investigation_id,
        status = outcome.state.as_str(),
        stop_reason = %outcome.stop_reason,
        cost = total_cost,
        quality_score,
        "investigation finished"
    );
}

/// Resolve the honest terminal state, stop reason, summary, and validated RCA JSON (A03).
pub fn resolve_investigation_outcome(
    exit_reason: LoopExitReason,
    raw_summary: &str,
) -> InvestigationOutcome {
    match exit_reason {
        LoopExitReason::DailyBudgetExceeded { spent, daily_budget } => InvestigationOutcome {
            state: InvestigationTerminalState::BudgetExhausted,
            stop_reason: "daily_budget_exceeded".to_string(),
            summary: format!(
                "STOP_REASON=daily_budget_exceeded: Daily AI budget ({daily_budget:.4} USD) exceeded; today spent {spent:.4} USD."
            ),
            proposal_json: "{}".to_string(),
            validated_rca: None,
        },
        LoopExitReason::DailyBudgetCheckFailed { error } => InvestigationOutcome {
            state: InvestigationTerminalState::Failed,
            stop_reason: "daily_budget_check_failed".to_string(),
            summary: format!(
                "STOP_REASON=daily_budget_check_failed: Failed to verify daily AI budget ({error}); halting model spend."
            ),
            proposal_json: "{}".to_string(),
            validated_rca: None,
        },
        LoopExitReason::ProviderUnavailable { error } => InvestigationOutcome {
            state: InvestigationTerminalState::Failed,
            stop_reason: "provider_unavailable".to_string(),
            summary: format!("STOP_REASON=provider_unavailable: AI provider unavailable: {error}"),
            proposal_json: "{}".to_string(),
            validated_rca: None,
        },
        LoopExitReason::PerInvestigationBudgetExceeded { cost, budget } => {
            let partial_rca = parse_and_validate_rca(raw_summary).ok();
            let proposal_json = partial_rca
                .as_ref()
                .and_then(|r| serde_json::to_string(r).ok())
                .unwrap_or_else(|| "{}".to_string());
            let body = if raw_summary.trim().is_empty() {
                format!("Investigation halted: per-investigation budget ({budget:.4} USD) exhausted at {cost:.4} USD.")
            } else {
                format!(
                    "STOP_REASON=investigation_budget_exceeded (spent {cost:.4}/{budget:.4} USD)\n\n{raw_summary}"
                )
            };
            InvestigationOutcome {
                state: InvestigationTerminalState::BudgetExhausted,
                stop_reason: "investigation_budget_exceeded".to_string(),
                summary: body,
                proposal_json,
                validated_rca: partial_rca,
            }
        }
        LoopExitReason::ProviderCallFailed { error, iteration } => InvestigationOutcome {
            state: InvestigationTerminalState::Failed,
            stop_reason: "provider_call_failed".to_string(),
            summary: if raw_summary.trim().is_empty() {
                format!("STOP_REASON=provider_call_failed at iteration {iteration}: {error}")
            } else {
                format!("STOP_REASON=provider_call_failed at iteration {iteration}: {error}\n\n{raw_summary}")
            },
            proposal_json: "{}".to_string(),
            validated_rca: None,
        },
        LoopExitReason::IterationLimitReached { max_iterations } => {
            let partial_rca = parse_and_validate_rca(raw_summary).ok();
            let proposal_json = partial_rca
                .as_ref()
                .and_then(|r| serde_json::to_string(r).ok())
                .unwrap_or_else(|| "{}".to_string());
            let body = if raw_summary.trim().is_empty() {
                format!("STOP_REASON=iteration_limit_reached: Exhausted {max_iterations} iterations without final RCA.")
            } else {
                format!("STOP_REASON=iteration_limit_reached ({max_iterations} iterations)\n\n{raw_summary}")
            };
            InvestigationOutcome {
                state: InvestigationTerminalState::Partial,
                stop_reason: "iteration_limit_reached".to_string(),
                summary: body,
                proposal_json,
                validated_rca: partial_rca,
            }
        }
        LoopExitReason::RepeatedToolCallDetected { tool_name } => InvestigationOutcome {
            state: InvestigationTerminalState::Partial,
            stop_reason: "repeated_tool_call_detected".to_string(),
            summary: if raw_summary.trim().is_empty() {
                format!("STOP_REASON=repeated_tool_call_detected ({tool_name})")
            } else {
                format!("STOP_REASON=repeated_tool_call_detected ({tool_name})\n\n{raw_summary}")
            },
            proposal_json: "{}".to_string(),
            validated_rca: None,
        },
        LoopExitReason::Cancelled { reason } => InvestigationOutcome {
            state: InvestigationTerminalState::Cancelled,
            stop_reason: "cancelled".to_string(),
            summary: format!("STOP_REASON=cancelled: {reason}"),
            proposal_json: "{}".to_string(),
            validated_rca: None,
        },
        LoopExitReason::ModelEndTurn => match parse_and_validate_rca(raw_summary) {
            Ok(rca) => {
                let proposal_json = serde_json::to_string(&rca).unwrap_or_else(|_| "{}".to_string());
                let (state, stop_reason) = match rca.epistemic_state.as_str() {
                    "awaiting_human" => (
                        InvestigationTerminalState::AwaitingHuman,
                        "awaiting_human_review".to_string(),
                    ),
                    "insufficient_evidence" | "contradicted" => (
                        InvestigationTerminalState::Partial,
                        format!("epistemic_{}", rca.epistemic_state),
                    ),
                    _ => (
                        InvestigationTerminalState::Completed,
                        "completed_valid_rca".to_string(),
                    ),
                };
                InvestigationOutcome {
                    state,
                    stop_reason,
                    summary: raw_summary.to_string(),
                    proposal_json,
                    validated_rca: Some(rca),
                }
            }
            Err(validation_err) => {
                let summary = if raw_summary.trim().is_empty() {
                    format!("STOP_REASON=invalid_or_missing_rca_schema: {validation_err}")
                } else {
                    format!(
                        "{raw_summary}\n\n[Validation Note: Output did not satisfy TypedInvestigationRca schema ({validation_err}); marked partial rather than completed.]"
                    )
                };
                InvestigationOutcome {
                    state: InvestigationTerminalState::Partial,
                    stop_reason: "invalid_or_missing_rca_schema".to_string(),
                    summary,
                    proposal_json: "{}".to_string(),
                    validated_rca: None,
                }
            }
        },
    }
}

// ── D4-8 T3: Coverage gap reporter ─────────────────────────────────────────

/// Compare graph quality data and agent tool usage to produce a structured
/// "Missing Data" report appended to the investigation summary. This helps
/// the operator understand why the LLM's findings may be incomplete.
fn compute_coverage_gaps(
    quality: &Option<GraphQuality>,
    queried_paths: &[String],
    device_address: &str,
) -> String {
    let mut gaps: Vec<String> = Vec::new();

    // 1. Graph quality dimension gaps — flag any coverage < 40%.
    if let Some(q) = quality {
        if q.gnmi_coverage.pct < 40.0 {
            gaps.push(format!(
                "gNMI telemetry coverage is low ({:.0}%) — real-time interface and BGP state may be unavailable for {}.",
                q.gnmi_coverage.pct, device_address
            ));
        }
        if q.syslog_coverage.pct < 40.0 {
            gaps.push(format!(
                "Syslog coverage is low ({:.0}%) — fault events from device logs may be missing.",
                q.syslog_coverage.pct
            ));
        }
        if q.bmp_coverage.pct < 40.0 {
            gaps.push(format!(
                "BMP coverage is low ({:.0}%) — BGP route-level analysis is limited.",
                q.bmp_coverage.pct
            ));
        }
        if q.interface_counter_coverage.pct < 40.0 {
            gaps.push(format!(
                "Interface counter coverage is low ({:.0}%) — traffic utilization and error analysis may be incomplete.",
                q.interface_counter_coverage.pct
            ));
        }
        if q.topology_link_coverage.pct < 40.0 {
            gaps.push(format!(
                "Topology link coverage is low ({:.0}%) — blast radius calculation may miss physical paths.",
                q.topology_link_coverage.pct
            ));
        }
        if q.netbox_enrichment_coverage.pct < 20.0 {
            gaps.push("NetBox enrichment is absent — site, rack, and role context unavailable.".to_string());
        }

        // Check if this device is in the weak devices list.
        for weak in &q.weak_devices {
            if weak.address == device_address {
                for missing in &weak.missing {
                    gaps.push(format!(
                        "Device {} is missing {missing} signal data.",
                        device_address
                    ));
                }
            }
        }
    }

    // 2. Tool usage gaps — agent didn't query certain data that could help.
    let has = |name: &str| queried_paths.iter().any(|p| p == name);
    if !has("get_device_blast_radius") {
        gaps.push("Blast radius was not checked — nearby device impact is unknown.".to_string());
    }
    if !has("query_graph") {
        gaps.push("No direct graph queries were performed — deeper Cypher analysis may reveal additional context.".to_string());
    }
    if !has("check_change_context") {
        gaps.push("Change management context was not checked — this fault may be correlated with an active change window.".to_string());
    }

    if gaps.is_empty() {
        return String::new();
    }

    let mut report = String::from("---\n## Coverage Gaps (auto-generated)\nThe following data gaps may affect investigation accuracy:\n");
    for gap in &gaps {
        report.push_str(&format!("- {gap}\n"));
    }
    report
}

fn system_prompt(device_address: &str, detection_id: Option<&str>) -> String {
    format!(
        "You are a network operations AI analyst for Bonsai, a network monitoring platform.\n\
         You have tools to query the live graph database.\n\
         \n\
         Investigate the issue on device {device_address}{}.\n\
         \n\
         Steps:\n\
         1. Get full incident context (get_incident) if a detection ID is provided.\n\
         2. Check blast radius (get_device_blast_radius) for affected scope.\n\
         3. Look for correlated detections on nearby devices (list_active_detections).\n\
         4. Use query_graph for deeper Cypher analysis if needed.\n\
         \n\
         When using query_graph, write read-only openCypher using the schema below.\n\
         \n\
         {GRAPH_SCHEMA}\n\
         \n\
         End with a JSON-formatted structured summary (inside a ```json code fence):\n\
         {{\n\
           \"root_cause_type\": \"<bgp_session_loss | interface_down | config_error | link_flap | insufficient_evidence | ...>\",\n\
           \"epistemic_state\": \"<supported | plausible | contradicted | insufficient_evidence | awaiting_human>\",\n\
           \"confidence\": <0.0-1.0>,\n\
           \"affected_scope\": [\"<device or service 1>\", \"<device or service 2>\"],\n\
           \"recommended_action\": \"<one-line remediation or next verification step>\",\n\
           \"missing_data\": [\"<signal or data that would increase confidence>\"]\n\
         }}",
        detection_id
            .map(|id| format!(" (detection: {id})"))
            .unwrap_or_default()
    )
}

fn user_prompt(device_address: &str, detection_id: Option<&str>) -> String {
    if let Some(id) = detection_id {
        format!(
            "Investigate detection {id} on device {device_address}. Start with get_incident."
        )
    } else {
        format!(
            "Investigate device {device_address}. List active detections and analyze blast radius."
        )
    }
}

// ── D4-6 T4 & A03: Pre-flight quality check ───────────────────────────────────

async fn preflight_quality(store: &Arc<GraphStore>, _device_address: &str) -> Option<GraphQuality> {
    let db = store.db();
    tokio::task::spawn_blocking(move || {
        let conn = lbug::Connection::new(&db).ok()?;
        graph_quality(&conn).ok()
    })
    .await
    .ok()
    .flatten()
}

/// Evaluate incident/device-specific pre-flight evidence coverage warning (A03).
///
/// Warns whenever the target `device_address` itself is missing signals in
/// `weak_devices` (even if the global graph `overall_score` is high because
/// other routers are healthy), or when the global graph quality is sparse (`< 40.0`).
pub fn evaluate_preflight_warning(
    quality: &Option<GraphQuality>,
    device_address: &str,
) -> Option<String> {
    let q = quality.as_ref()?;
    let device_missing: Vec<String> = q
        .weak_devices
        .iter()
        .filter(|w| w.address == device_address)
        .flat_map(|w| w.missing.clone())
        .collect();

    if !device_missing.is_empty() {
        return Some(format!(
            "WARNING: Target device {} is missing telemetry signals ({}) (global graph score {:.0}/100). \
             Findings for this device may be incomplete.",
            device_address,
            device_missing.join(", "),
            q.overall_score
        ));
    }

    if q.overall_score < 40.0 {
        return Some(format!(
            "WARNING: Graph data sparse for {} (global quality score {:.0}/100, missing: multiple signal sources). \
             Findings may be incomplete.",
            device_address, q.overall_score
        ));
    }

    None
}

// ── D4-8 T1 & A03: Strict Typed RCA Extraction & Validation ───────────────────

const VALID_EPISTEMIC_STATES: &[&str] = &[
    "supported",
    "plausible",
    "contradicted",
    "insufficient_evidence",
    "awaiting_human",
];

fn extract_json_candidate(summary: &str) -> Option<&str> {
    if let Some(start) = summary.find("```json") {
        let after = &summary[start + 7..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim());
        }
    }
    if let Some(start) = summary.find("```") {
        let after = &summary[start + 3..];
        if let Some(end) = after.find("```") {
            let candidate = after[..end].trim();
            if candidate.starts_with('{') && candidate.ends_with('}') {
                return Some(candidate);
            }
        }
    }
    if let (Some(s), Some(e)) = (summary.find('{'), summary.rfind('}')) {
        if e > s {
            return Some(&summary[s..=e]);
        }
    }
    None
}

/// Parse and strictly validate a `TypedInvestigationRca` from the LLM summary (A03).
pub fn parse_and_validate_rca(summary: &str) -> Result<TypedInvestigationRca, String> {
    let candidate = extract_json_candidate(summary)
        .ok_or_else(|| "no JSON block found in summary".to_string())?;
    let mut rca: TypedInvestigationRca = serde_json::from_str(candidate)
        .map_err(|e| format!("invalid RCA JSON structure: {e}"))?;

    rca.root_cause_type = rca.root_cause_type.trim().to_string();
    if rca.root_cause_type.is_empty() {
        return Err("root_cause_type must not be empty".to_string());
    }

    if !rca.confidence.is_finite() || !(0.0..=1.0).contains(&rca.confidence) {
        return Err(format!(
            "confidence must be a finite number in [0.0, 1.0], got {}",
            rca.confidence
        ));
    }

    rca.recommended_action = rca.recommended_action.trim().to_string();
    if rca.recommended_action.is_empty() {
        return Err("recommended_action must not be empty".to_string());
    }

    rca.epistemic_state = rca.epistemic_state.trim().to_lowercase();
    if !VALID_EPISTEMIC_STATES.contains(&rca.epistemic_state.as_str()) {
        return Err(format!(
            "epistemic_state must be one of {:?}, got {:?}",
            VALID_EPISTEMIC_STATES, rca.epistemic_state
        ));
    }

    // Model-reported confidence is always marked as uncalibrated judgment per Strategy §5.
    rca.confidence_is_uncalibrated = true;
    Ok(rca)
}

/// Extract a validated RCA JSON string from `summary`, or return `"{}"` if validation fails.
pub fn extract_rca_json(summary: &str) -> String {
    match parse_and_validate_rca(summary) {
        Ok(rca) => serde_json::to_string(&rca).unwrap_or_else(|_| "{}".to_string()),
        Err(_) => "{}".to_string(),
    }
}

// ── A08: Pre-Call Budget Reservation Ledger & Tool-Call Deduplicator ──────────

/// Pre-call budget reservation ledger (Strategy §10, A08).
///
/// Reserves estimated model spend BEFORE invoking the provider so concurrent or
/// sequential iterations cannot overshoot `per_investigation_budget_usd` or
/// `daily_budget_usd`. Reconciles actual spend on completion or releases on error/cancel.
#[derive(Debug, Clone, Default)]
pub struct BudgetReservationLedger {
    daily_committed_usd: f64,
    committed_by_investigation: std::collections::HashMap<String, f64>,
    /// Maps `reservation_id -> (investigation_id, reserved_usd)`.
    active_reservations: std::collections::HashMap<String, (String, f64)>,
}

impl BudgetReservationLedger {
    pub fn new(initial_daily_committed_usd: f64) -> Self {
        Self {
            daily_committed_usd: initial_daily_committed_usd.max(0.0),
            committed_by_investigation: std::collections::HashMap::new(),
            active_reservations: std::collections::HashMap::new(),
        }
    }

    pub fn total_reserved_usd(&self) -> f64 {
        self.active_reservations.values().map(|(_, amt)| *amt).sum()
    }

    pub fn investigation_reserved_usd(&self, investigation_id: &str) -> f64 {
        self.active_reservations
            .values()
            .filter(|(inv_id, _)| inv_id == investigation_id)
            .map(|(_, amt)| *amt)
            .sum()
    }

    pub fn investigation_committed_usd(&self, investigation_id: &str) -> f64 {
        self.committed_by_investigation
            .get(investigation_id)
            .copied()
            .unwrap_or(0.0)
    }

    pub fn daily_committed_usd(&self) -> f64 {
        self.daily_committed_usd
    }

    /// Reserve `estimated_usd` prior to an AI provider call.
    /// Fails closed if either the per-investigation limit or the daily limit would be exceeded.
    pub fn reserve(
        &mut self,
        reservation_id: &str,
        investigation_id: &str,
        estimated_usd: f64,
        per_investigation_limit_usd: f64,
        daily_limit_usd: f64,
    ) -> Result<(), LoopExitReason> {
        let est = estimated_usd.max(0.0);
        let inv_committed = self.investigation_committed_usd(investigation_id);
        let inv_reserved = self.investigation_reserved_usd(investigation_id);
        if per_investigation_limit_usd > 0.0
            && (inv_committed + inv_reserved + est) > per_investigation_limit_usd
        {
            return Err(LoopExitReason::PerInvestigationBudgetExceeded {
                cost: inv_committed + inv_reserved + est,
                budget: per_investigation_limit_usd,
            });
        }

        let total_reserved = self.total_reserved_usd();
        if daily_limit_usd > 0.0
            && (self.daily_committed_usd + total_reserved + est) > daily_limit_usd
        {
            return Err(LoopExitReason::DailyBudgetExceeded {
                spent: self.daily_committed_usd + total_reserved + est,
                daily_budget: daily_limit_usd,
            });
        }

        self.active_reservations.insert(
            reservation_id.to_string(),
            (investigation_id.to_string(), est),
        );
        Ok(())
    }

    /// Reconcile a reservation with actual provider spend.
    pub fn commit(&mut self, reservation_id: &str, actual_usd: f64) -> Result<f64, String> {
        let (inv_id, _) = self
            .active_reservations
            .remove(reservation_id)
            .ok_or_else(|| format!("unknown reservation_id: {reservation_id}"))?;
        let actual = actual_usd.max(0.0);
        self.daily_committed_usd += actual;
        *self.committed_by_investigation.entry(inv_id).or_insert(0.0) += actual;
        Ok(actual)
    }

    /// Release an uncommitted reservation on provider error or cancellation.
    pub fn release(&mut self, reservation_id: &str) -> Option<f64> {
        self.active_reservations
            .remove(reservation_id)
            .map(|(_, amt)| amt)
    }
}

/// Detects repeated identical `(tool_name, canonical_args_json)` calls within an investigation (Strategy §5, A08).
#[derive(Debug, Clone, Default)]
pub struct ToolCallDeduplicator {
    seen_calls: std::collections::HashSet<String>,
}

impl ToolCallDeduplicator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if this exact `(tool_name, args)` call was already executed in this investigation.
    pub fn record_or_detect_repeat(
        &mut self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> bool {
        let key = format!("{}::{}", tool_name.trim(), args);
        !self.seen_calls.insert(key)
    }
}


