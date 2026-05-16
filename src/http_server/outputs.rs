#![allow(unused_imports, dead_code, unused_variables)]
use super::*;

// ── ServiceNow integration test endpoint (T2-1) ───────────────────────────────

#[derive(Deserialize)]
struct SnowTestRequest {
    instance_url: String,
    credential_alias: String,
}

#[derive(Serialize)]
struct SnowTestResponse {
    success: bool,
    message: String,
}

#[derive(Serialize)]
struct SnowAiopsSyncResponse {
    success: bool,
    error: String,
    stats: crate::integrations::servicenow_aiops::SyncStats,
}

async fn snow_integration_test_handler(
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

async fn servicenow_aiops_sync_handler(
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

#[derive(serde::Deserialize)]
pub struct RemoveOverrideReq {
    pub scope: crate::registry::OverrideScope,
    pub path: String,
}

async fn list_overrides(State(state): State<AppState>) -> impl axum::response::IntoResponse {
    match state.registry.list_overrides() {
        Ok(overrides) => Json(overrides).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to list overrides: {}", e),
        )
            .into_response(),
    }
}

async fn add_override(
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

async fn remove_override(
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

// ── Output adapter management API (T6-6) ─────────────────────────────────────

#[derive(Serialize)]
struct AdapterEntry {
    config: OutputAdapterConfig,
    state: OutputAdapterRunState,
}

#[derive(Serialize)]
struct AdapterListResponse {
    adapters: Vec<AdapterEntry>,
}

async fn adapter_list_handler(State(state): State<AppState>) -> Json<AdapterListResponse> {
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

#[derive(Deserialize)]
struct AdapterUpsertRequest {
    config: OutputAdapterConfig,
}

#[derive(Serialize)]
struct AdapterMutationResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn adapter_upsert_handler(
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

#[derive(Deserialize)]
struct AdapterNameRequest {
    name: String,
}

async fn adapter_remove_handler(
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

#[derive(Serialize)]
struct AdapterTestResponse {
    success: bool,
    message: String,
}

async fn adapter_test_handler(
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

#[derive(Serialize)]
struct AdapterAuditResponse {
    entries: Vec<serde_json::Value>,
}

async fn adapter_audit_handler(State(state): State<AppState>) -> Json<AdapterAuditResponse> {
    let audit_dir = std::path::Path::new(&state.runtime_dir).join("audit");
    let entries = read_recent_adapter_audit(&audit_dir, 100);
    Json(AdapterAuditResponse { entries })
}

fn read_recent_adapter_audit(audit_dir: &std::path::Path, limit: usize) -> Vec<serde_json::Value> {
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

fn latest_adapter_push_state(
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

