//! Administrative ownership rollout. Audit responses never change access decisions.
use super::{
    AppState,
    auth::{AuthenticatedSubject, Role},
};
use crate::graph::{
    ConfigItemRecord,
    ownership::{OwnershipAccessMode, OwnershipAssignment},
};
use axum::{
    Extension, Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

type ApiError = (StatusCode, String);

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/ownership/bindings", get(list).put(assign))
        .route("/api/ownership/audit", post(audit))
        .route("/api/ownership/policy", get(policy).put(update_policy))
}

async fn policy(
    State(state): State<AppState>,
    role: Option<Extension<Role>>,
) -> Result<Json<Value>, ApiError> {
    require_admin(role)?;
    Ok(Json(
        json!({"access_mode":state.store.ownership_access_mode().await,
        "mapping_enforcement_enabled":false}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyUpdate {
    access_mode: OwnershipAccessMode,
}

async fn update_policy(
    State(state): State<AppState>,
    role: Option<Extension<Role>>,
    subject: Option<Extension<AuthenticatedSubject>>,
    Json(update): Json<PolicyUpdate>,
) -> Result<Json<Value>, ApiError> {
    require_admin(role)?;
    let Some(Extension(subject)) = subject else {
        return Err((
            StatusCode::FORBIDDEN,
            "authenticated subject is required".into(),
        ));
    };
    state
        .store
        .upsert_config_item(ConfigItemRecord {
            id: "ownership-access-mode".into(),
            config_class: "app_config".into(),
            vendor: "bonsai".into(),
            name: "ownership_access_mode".into(),
            version: "1".into(),
            content_json: serde_json::to_string(&update.access_mode).map_err(internal)?,
            enabled: true,
            created_by: subject.0,
        })
        .await
        .map_err(internal)?;
    tracing::warn!(mode = ?update.access_mode, "ownership access policy updated");
    Ok(Json(
        json!({"access_mode":state.store.ownership_access_mode().await,
        "mapping_enforcement_enabled":false}),
    ))
}

// Also check here: an auth-disabled deployment must not silently allow anonymous
// changes to inventory ownership that may become authoritative in a future rollout.
fn require_admin(role: Option<Extension<Role>>) -> Result<(), ApiError> {
    match role {
        Some(Extension(Role::Admin)) => Ok(()),
        _ => Err((
            StatusCode::FORBIDDEN,
            "ownership administration requires an authenticated admin".into(),
        )),
    }
}

async fn assign(
    State(state): State<AppState>,
    role: Option<Extension<Role>>,
    subject: Option<Extension<AuthenticatedSubject>>,
    Json(assignment): Json<OwnershipAssignment>,
) -> Result<Json<Value>, ApiError> {
    require_admin(role)?;
    let Some(Extension(subject)) = subject else {
        return Err((
            StatusCode::FORBIDDEN,
            "authenticated subject is required".into(),
        ));
    };
    assignment.validate().map_err(bad_request)?;
    let binding = state
        .store
        .assign_ownership(assignment, subject.0)
        .await
        .map_err(internal)?;
    Ok(Json(
        json!({"mode":"audit_only", "enforcement_enabled":false, "binding":binding}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    #[serde(default)]
    after_key: String,
    limit: Option<usize>,
}

async fn list(
    State(state): State<AppState>,
    role: Option<Extension<Role>>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    require_admin(role)?;
    let limit = query.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) || query.after_key.len() > 2048 {
        return Err((
            StatusCode::BAD_REQUEST,
            "limit must be 1–500; cursor at most 2048 bytes".into(),
        ));
    }
    let mut rows = state
        .store
        .list_ownership(query.after_key, limit + 1)
        .await
        .map_err(internal)?;
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    let next_after_key = if truncated {
        rows.last().map(|(key, _)| key.clone())
    } else {
        None
    };
    let bindings: Vec<_> = rows.into_iter().map(|(_, value)| value).collect();
    Ok(Json(
        json!({"mode":"audit_only", "enforcement_enabled":false,
        "bindings":bindings, "truncated":truncated, "next_after_key":next_after_key}),
    ))
}

async fn audit(
    State(state): State<AppState>,
    role: Option<Extension<Role>>,
    Json(expected): Json<OwnershipAssignment>,
) -> Result<Json<Value>, ApiError> {
    require_admin(role)?;
    expected.validate().map_err(bad_request)?;
    let report = state
        .store
        .audit_ownership(expected)
        .await
        .map_err(internal)?;
    tracing::info!(status = ?report.status, mode = "audit_only", "ownership inventory audit");
    Ok(Json(serde_json::to_value(report).map_err(internal)?))
}

fn bad_request(error: impl std::fmt::Display) -> ApiError {
    (StatusCode::BAD_REQUEST, error.to_string())
}
fn internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(%error, "ownership storage failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "ownership storage failed".into(),
    )
}
