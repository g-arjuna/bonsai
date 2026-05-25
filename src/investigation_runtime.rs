use std::sync::Arc;
use tracing::{info, warn};

use crate::ai_provider::{AiMessage, AiProvider, build_provider_with_key};
use crate::config::AiConfig;
use crate::graph::GraphStore;
use crate::graph::algorithms::{GraphQuality, graph_quality};
use crate::http_server::AppState;
use crate::http_server::nl_query::GRAPH_SCHEMA;

const MAX_ITERATIONS: usize = 15;

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
    // Daily budget gate — abort before building the provider if today's spend is over limit.
    let daily_budget = ai_cfg.daily_budget_usd;
    if daily_budget > 0.0 {
        match store.query_daily_investigation_cost().await {
            Ok(spent) if spent >= daily_budget => {
                warn!(
                    id = %investigation_id,
                    spent,
                    daily_budget,
                    "daily AI budget exceeded — investigation skipped"
                );
                let _ = store.complete_investigation(
                    investigation_id,
                    "skipped".to_string(),
                    format!("Daily AI budget ({daily_budget:.4} USD) exceeded; today spent {spent:.4} USD."),
                    "{}".to_string(),
                    0,
                    0.0,
                ).await;
                return;
            }
            Err(e) => {
                warn!(id = %investigation_id, error = %e, "daily cost query failed — proceeding");
            }
            _ => {}
        }
    }

    let provider: Box<dyn AiProvider> = match build_provider_with_key(&ai_cfg, api_key) {
        Ok(p) => p,
        Err(e) => {
            warn!(id = %investigation_id, error = %e, "AI provider unavailable");
            let _ = store.complete_investigation(
                investigation_id,
                "failed".to_string(),
                format!("AI provider unavailable: {e}"),
                "{}".to_string(),
                0,
                0.0,
            ).await;
            return;
        }
    };

    let tools = crate::mcp_server::ai_tool_definitions();
    let per_budget = ai_cfg.per_investigation_budget_usd;

    // D4-6 T4: Pre-flight quality check — warn if graph data is sparse.
    let quality = preflight_quality(&store, &device_address).await;
    let quality_score = quality.as_ref().map(|q| q.overall_score).unwrap_or(-1.0);
    let quality_warning = match &quality {
        Some(q) if q.overall_score < 40.0 => {
            let missing: Vec<String> = q.weak_devices.iter()
                .filter(|w| w.address == device_address)
                .flat_map(|w| w.missing.clone())
                .collect();
            let missing_str = if missing.is_empty() {
                "multiple signal sources".to_string()
            } else {
                missing.join(", ")
            };
            warn!(
                id = %investigation_id,
                score = q.overall_score,
                missing = %missing_str,
                "graph data sparse for investigation device"
            );
            Some(format!(
                "WARNING: Graph data sparse for {} (quality score {:.0}/100, missing: {}). \
                 Findings may be incomplete.",
                device_address, q.overall_score, missing_str
            ))
        }
        _ => None,
    };

    let context = build_context(&device_address, detection_id.as_deref(), &state).await;
    // D4-8 T5: Inject graph schema into system prompt so the agent can write valid Cypher.
    let mut messages = vec![
        AiMessage::system(system_prompt(&device_address, detection_id.as_deref())),
        AiMessage::user(format!("{}{context}", user_prompt(&device_address, detection_id.as_deref()))),
    ];

    let mut total_cost = 0.0f64;
    let mut total_tokens = 0i64;

    // D4-8 T3: Track which tool calls the agent used (for coverage gap analysis).
    let mut queried_paths: Vec<String> = Vec::new();

    info!(id = %investigation_id, "investigation started");

    for iteration in 0..MAX_ITERATIONS {
        if total_cost >= per_budget {
            warn!(id = %investigation_id, cost = total_cost, "investigation budget exceeded");
            break;
        }

        let resp = match provider.complete(messages.clone(), tools.clone()).await {
            Ok(r) => r,
            Err(e) => {
                warn!(id = %investigation_id, error = %e, "LLM call failed");
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
        });

        if resp.tool_calls.is_empty() || resp.stop_reason == "end_turn" {
            let raw_summary = resp.content.unwrap_or_else(|| "Investigation complete.".into());
            // D4-8 T1: Structured RCA extraction pass.
            let result_json = extract_rca_json(&raw_summary);
            // D4-8 T3: Append coverage gap report.
            let gap_report = compute_coverage_gaps(&quality, &queried_paths, &device_address);
            // D4-6 T4: Prefix quality warning if graph was sparse.
            let mut summary = match &quality_warning {
                Some(warning) => format!("{warning}\n\n{raw_summary}"),
                None => raw_summary,
            };
            if !gap_report.is_empty() {
                summary.push_str("\n\n");
                summary.push_str(&gap_report);
            }
            let _ = store.complete_investigation(
                investigation_id.clone(),
                "completed".to_string(),
                summary,
                result_json,
                total_tokens,
                total_cost,
            ).await;
            info!(id = %investigation_id, iterations = iteration + 1, cost = total_cost, quality_score, "investigation completed");
            return;
        }

        for tc in &resp.tool_calls {
            // D4-8 T3: Record tool call names for coverage gap analysis.
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

    let raw_fallback = messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant" && m.content.is_some())
        .and_then(|m| m.content.clone())
        .unwrap_or_else(|| "Investigation terminated at iteration or budget limit.".into());

    let result_json = extract_rca_json(&raw_fallback);
    let gap_report = compute_coverage_gaps(&quality, &queried_paths, &device_address);
    let mut fallback_summary = match &quality_warning {
        Some(warning) => format!("{warning}\n\n{raw_fallback}"),
        None => raw_fallback,
    };
    if !gap_report.is_empty() {
        fallback_summary.push_str("\n\n");
        fallback_summary.push_str(&gap_report);
    }

    let _ = store.complete_investigation(
        investigation_id.clone(),
        "completed".to_string(),
        fallback_summary,
        result_json,
        total_tokens,
        total_cost,
    ).await;
    info!(id = %investigation_id, cost = total_cost, quality_score, "investigation hit limit");
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
    // D4-8 T5: Include graph schema so the agent can write valid Cypher in query_graph calls.
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
           \"root_cause_type\": \"<bgp_session_loss | interface_down | config_error | link_flap | ...>\",\n\
           \"confidence\": <0.0-1.0>,\n\
           \"affected_scope\": [\"<device or service 1>\", \"<device or service 2>\"],\n\
           \"recommended_action\": \"<one-line remediation step>\",\n\
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

// ── D4-6 T4: Pre-flight quality check ─────────────────────────────────────────

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

// ── D4-8 T1: Structured RCA extraction ────────────────────────────────────────

/// Try to extract a JSON object with root_cause_type, confidence, etc. from the
/// LLM's final summary. Falls back to `"{}"` if no valid JSON is found.
fn extract_rca_json(summary: &str) -> String {
    // 1. Try ```json ... ``` fenced block
    if let Some(start) = summary.find("```json") {
        let after = &summary[start + 7..];
        if let Some(end) = after.find("```") {
            let candidate = after[..end].trim();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(candidate) {
                if v.get("root_cause_type").is_some() {
                    return serde_json::to_string(&v).unwrap_or_else(|_| "{}".into());
                }
            }
        }
    }
    // 2. Try ``` ... ``` fenced block
    if let Some(start) = summary.find("```") {
        let after = &summary[start + 3..];
        if let Some(end) = after.find("```") {
            let candidate = after[..end].trim();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(candidate) {
                if v.get("root_cause_type").is_some() {
                    return serde_json::to_string(&v).unwrap_or_else(|_| "{}".into());
                }
            }
        }
    }
    // 3. Try to find a JSON object with root_cause_type anywhere in the text
    if let (Some(s), Some(e)) = (summary.find('{'), summary.rfind('}')) {
        if e > s {
            let candidate = &summary[s..=e];
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(candidate) {
                if v.get("root_cause_type").is_some() {
                    return serde_json::to_string(&v).unwrap_or_else(|_| "{}".into());
                }
            }
        }
    }
    "{}".to_string()
}
