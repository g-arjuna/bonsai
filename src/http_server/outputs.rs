#[derive(Serialize)]
pub(super) struct AdapterEntry {
    config: OutputAdapterConfig,
    state: OutputAdapterRunState,
}
#[derive(Serialize)]
pub(super) struct AdapterListResponse {
    adapters: Vec<AdapterEntry>,
}
#[derive(Deserialize)]
pub(super) struct AdapterUpsertRequest {
    config: OutputAdapterConfig,
}
#[derive(Serialize)]
pub(super) struct AdapterMutationResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
#[derive(Deserialize)]
pub(super) struct AdapterNameRequest {
    name: String,
}
#[derive(Serialize)]
pub(super) struct AdapterTestResponse {
    success: bool,
    message: String,
}
#[derive(Serialize)]
pub(super) struct AdapterAuditResponse {
    entries: Vec<serde_json::Value>,
}
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use axum::{Json, extract::State, http::StatusCode};
use serde_json::Value;

use super::AppState;
use crate::output::traits::{OutputAdapterConfig, OutputAdapterRunState, SharedAdapterRegistry};

pub(super) async fn adapter_list_handler(State(state): State<AppState>) -> Json<AdapterListResponse> {
    let audit_dir = std::path::Path::new(&state.runtime_dir).join("audit");
    let latest_pushes = latest_adapter_push_state(&read_recent_adapter_audit(&audit_dir, 1000));

    let reg = state.adapter_registry.read().await;
    let adapters = reg
        .list()
        .into_iter()
        .map(|(config, mut st)| {
            if let Some(audit_state) = latest_pushes.get(&config.name) {
                st.last_push_at_ns = audit_state.last_push_at_ns;
                st.last_push_duration_ms = audit_state.last_push_duration_ms;
                st.last_push_events = audit_state.last_push_events;
                st.last_push_bytes = audit_state.last_push_bytes;
                st.last_push_warnings = audit_state.last_push_warnings.clone();
                st.last_push_error = audit_state.last_push_error.clone();
                st.total_events_pushed = audit_state.total_events_pushed;
                st.total_bytes_sent = audit_state.total_bytes_sent;
            }
            AdapterEntry { config, state: st }
        })
        .collect();
    Json(AdapterListResponse { adapters })
}
pub(super) async fn adapter_upsert_handler(
    State(state): State<AppState>,
    Json(req): Json<AdapterUpsertRequest>,
) -> Json<AdapterMutationResponse> {
    if let Err(error) = crate::output::ensure_supported_adapter_type(&req.config) {
        return Json(AdapterMutationResponse {
            success: false,
            error: Some(error.to_string()),
        });
    }
    state.adapter_registry.write().await.upsert(req.config);
    Json(AdapterMutationResponse {
        success: true,
        error: None,
    })
}
pub(super) async fn adapter_remove_handler(
    State(state): State<AppState>,
    Json(req): Json<AdapterNameRequest>,
) -> Json<AdapterMutationResponse> {
    let removed = state.adapter_registry.write().await.remove(&req.name);
    if removed {
        Json(AdapterMutationResponse {
            success: true,
            error: None,
        })
    } else {
        Json(AdapterMutationResponse {
            success: false,
            error: Some(format!("adapter '{}' not found", req.name)),
        })
    }
}
pub(super) async fn adapter_test_handler(
    State(state): State<AppState>,
    Json(req): Json<AdapterNameRequest>,
) -> Json<AdapterTestResponse> {
    let config = {
        let reg = state.adapter_registry.read().await;
        reg.get(&req.name).cloned()
    };
    let Some(config) = config else {
        return Json(AdapterTestResponse {
            success: false,
            message: format!("adapter '{}' not found", req.name),
        });
    };

    let audit = crate::output::traits::OutputAdapterAuditLog::new(
        std::path::Path::new(&state.runtime_dir),
        &config.name,
    );

    let result = match crate::output::build_adapter(&config, state.store.db()) {
        Some(adapter) => {
            adapter
                .test_connection(Arc::clone(&state.credentials), &audit)
                .await
        }
        None => Err(anyhow::anyhow!(
            "unknown adapter type '{}'",
            config.adapter_type
        )),
    };

    match result {
        Ok(()) => Json(AdapterTestResponse {
            success: true,
            message: "connection ok".to_string(),
        }),
        Err(e) => Json(AdapterTestResponse {
            success: false,
            message: format!("{e:#}"),
        }),
    }
}
pub(super) async fn adapter_audit_handler(State(state): State<AppState>) -> Json<AdapterAuditResponse> {
    let audit_dir = std::path::Path::new(&state.runtime_dir).join("audit");
    let entries = read_recent_adapter_audit(&audit_dir, 100);
    Json(AdapterAuditResponse { entries })
}
pub(super) fn read_recent_adapter_audit(audit_dir: &std::path::Path, limit: usize) -> Vec<serde_json::Value> {
    if !audit_dir.exists() {
        return vec![];
    }
    let mut files: Vec<_> = std::fs::read_dir(audit_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "jsonl")
        })
        .collect();
    files.sort();

    let mut entries = Vec::new();
    for file in files.iter().rev() {
        if let Ok(content) = std::fs::read_to_string(file) {
            for line in content.lines().rev() {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
                    && val.get("event").and_then(|v| v.as_str()) == Some("adapter_push")
                {
                    entries.push(val);
                    if entries.len() >= limit {
                        return entries;
                    }
                }
            }
        }
    }
    entries
}
pub(super) fn latest_adapter_push_state(
    entries: &[serde_json::Value],
) -> std::collections::HashMap<String, OutputAdapterRunState> {
    let mut by_adapter = std::collections::HashMap::new();
    for entry in entries {
        if entry.get("event").and_then(|v| v.as_str()) != Some("adapter_push") {
            continue;
        }
        let Some(name) = entry.get("adapter").and_then(|v| v.as_str()) else {
            continue;
        };
        let state = by_adapter
            .entry(name.to_string())
            .or_insert_with(OutputAdapterRunState::default);
        state.total_events_pushed += entry
            .get("events_pushed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        state.total_bytes_sent += entry
            .get("bytes_sent")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let ts = entry
            .get("timestamp_ns")
            .and_then(|v| v.as_i64())
            .unwrap_or_default();
        if state.last_push_at_ns.is_none_or(|cur| ts >= cur) {
            state.last_push_at_ns = Some(ts);
            state.last_push_events = Some(
                entry
                    .get("events_pushed")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize,
            );
            state.last_push_bytes = Some(
                entry
                    .get("bytes_sent")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            );
            state.last_push_error = entry
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            state.last_push_warnings = vec![];
        }
    }
    by_adapter
}
