/// REST API for syslog shun rule management (D4-2 T3).
///
/// Endpoints:
///   GET  /api/shun/rules                — list all rules
///   POST /api/shun/rules                — create a rule
///   POST /api/shun/rules/{id}/disable   — disable without deleting
///   POST /api/shun/rules/{id}/delete    — permanently remove
///   GET  /api/shun/stats                — per-rule suppression counts
use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use crate::shun::ShunRule;

use super::AppState;

// ── Request / response types ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct CreateShunRuleRequest {
    #[serde(default = "default_scope_type")]
    pub scope_type: String,
    #[serde(default)]
    pub scope_value: String,
    #[serde(default = "default_match_type")]
    pub match_type: String,
    pub match_value: String,
    #[serde(default = "default_action")]
    pub action: String,
    #[serde(default)]
    pub rate_limit_per_min: i64,
    /// Unix ns timestamp; 0 = never expires.
    #[serde(default)]
    pub expires_at_ns: i64,
    #[serde(default = "default_created_by")]
    pub created_by: String,
}

fn default_scope_type() -> String { "global".to_string() }
fn default_match_type() -> String { "substring".to_string() }
fn default_action() -> String { "drop".to_string() }
fn default_created_by() -> String { "api".to_string() }

#[derive(Serialize)]
pub(super) struct ShunRulesResponse {
    rules: Vec<ShunRule>,
}

#[derive(Serialize)]
pub(super) struct ShunStatsResponse {
    stats: std::collections::HashMap<String, u64>,
}

#[derive(Serialize)]
pub(super) struct ShunActionResponse {
    success: bool,
    error: String,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub(super) async fn list_shun_rules_handler(
    State(state): State<AppState>,
) -> Result<Json<ShunRulesResponse>, StatusCode> {
    let rules = state
        .store
        .list_shun_rules()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ShunRulesResponse { rules }))
}

pub(super) async fn create_shun_rule_handler(
    State(state): State<AppState>,
    Json(req): Json<CreateShunRuleRequest>,
) -> Result<Json<ShunRule>, StatusCode> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default();

    let rule = ShunRule {
        id: uuid::Uuid::new_v4().to_string(),
        scope_type: req.scope_type,
        scope_value: req.scope_value,
        match_type: req.match_type,
        match_value: req.match_value,
        action: req.action,
        rate_limit_per_min: req.rate_limit_per_min,
        expires_at_ns: req.expires_at_ns,
        created_by: req.created_by,
        created_at_ns: now_ns,
        enabled: true,
    };

    state
        .store
        .upsert_shun_rule(rule.clone())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Reload the in-memory engine.
    if let Some(engine) = &state.shun_engine {
        match state.store.list_shun_rules().await {
            Ok(rules) => engine.reload(rules),
            Err(e) => tracing::warn!(error = %e, "shun engine reload after create failed"),
        }
    }

    Ok(Json(rule))
}

pub(super) async fn disable_shun_rule_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<ShunActionResponse> {
    let rules = match state.store.list_shun_rules().await {
        Ok(r) => r,
        Err(e) => {
            return Json(ShunActionResponse { success: false, error: e.to_string() });
        }
    };
    let Some(mut rule) = rules.into_iter().find(|r| r.id == id) else {
        return Json(ShunActionResponse {
            success: false,
            error: format!("rule {id} not found"),
        });
    };
    rule.enabled = false;
    if let Err(e) = state.store.upsert_shun_rule(rule).await {
        return Json(ShunActionResponse { success: false, error: e.to_string() });
    }
    if let Some(engine) = &state.shun_engine {
        match state.store.list_shun_rules().await {
            Ok(rules) => engine.reload(rules),
            Err(e) => tracing::warn!(error = %e, "shun engine reload after disable failed"),
        }
    }
    Json(ShunActionResponse { success: true, error: String::new() })
}

pub(super) async fn delete_shun_rule_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<ShunActionResponse> {
    match state.store.delete_shun_rule(id).await {
        Ok(found) => {
            if !found {
                return Json(ShunActionResponse {
                    success: false,
                    error: "rule not found".to_string(),
                });
            }
            if let Some(engine) = &state.shun_engine {
                match state.store.list_shun_rules().await {
                    Ok(rules) => engine.reload(rules),
                    Err(e) => tracing::warn!(error = %e, "shun engine reload after delete failed"),
                }
            }
            Json(ShunActionResponse { success: true, error: String::new() })
        }
        Err(e) => Json(ShunActionResponse { success: false, error: e.to_string() }),
    }
}

pub(super) async fn shun_stats_handler(
    State(state): State<AppState>,
) -> Json<ShunStatsResponse> {
    let stats = state
        .shun_engine
        .as_ref()
        .map(|e| e.stats())
        .unwrap_or_default();
    Json(ShunStatsResponse { stats })
}
