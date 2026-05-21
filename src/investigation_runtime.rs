use std::sync::Arc;
use tracing::{info, warn};

use crate::ai_provider::{AiMessage, AiProvider, build_provider};
use crate::config::AiConfig;
use crate::graph::GraphStore;
use crate::http_server::AppState;

const MAX_ITERATIONS: usize = 15;

/// Spawn a background investigation task. Returns immediately; the investigation
/// runs to completion (or budget/iteration limit) as a tokio task.
pub fn spawn_investigation(
    investigation_id: String,
    device_address: String,
    detection_id: Option<String>,
    store: Arc<GraphStore>,
    state: AppState,
    ai_cfg: AiConfig,
) {
    tokio::spawn(async move {
        run_investigation(investigation_id, device_address, detection_id, store, state, ai_cfg).await;
    });
}

async fn run_investigation(
    investigation_id: String,
    device_address: String,
    detection_id: Option<String>,
    store: Arc<GraphStore>,
    state: AppState,
    ai_cfg: AiConfig,
) {
    let provider: Box<dyn AiProvider> = match build_provider(&ai_cfg) {
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

    let mut messages = vec![
        AiMessage::system(system_prompt(&device_address, detection_id.as_deref())),
        AiMessage::user(user_prompt(&device_address, detection_id.as_deref())),
    ];

    let mut total_cost = 0.0f64;
    let mut total_tokens = 0i64;

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
            let summary = resp.content.unwrap_or_else(|| "Investigation complete.".into());
            let _ = store.complete_investigation(
                investigation_id.clone(),
                "completed".to_string(),
                summary,
                "{}".to_string(),
                total_tokens,
                total_cost,
            ).await;
            info!(id = %investigation_id, iterations = iteration + 1, cost = total_cost, "investigation completed");
            return;
        }

        for tc in &resp.tool_calls {
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

    let fallback_summary = messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant" && m.content.is_some())
        .and_then(|m| m.content.clone())
        .unwrap_or_else(|| "Investigation terminated at iteration or budget limit.".into());

    let _ = store.complete_investigation(
        investigation_id.clone(),
        "completed".to_string(),
        fallback_summary,
        "{}".to_string(),
        total_tokens,
        total_cost,
    ).await;
    info!(id = %investigation_id, cost = total_cost, "investigation hit limit");
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
         4. Use query_graph for deeper analysis if needed.\n\
         \n\
         End with a structured summary: root cause, affected scope, recommended action.",
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
