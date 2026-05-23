//! D4-3 T6 — Scoped API keys for external consumers.
//!
//! Keys are stored as `ConfigItem` rows with `config_class = "api_key"`.
//! The `content_json` field holds a JSON object with `key_hash`, `scope`,
//! `expires_at_ns`, and `last_used_at_ns`.
//!
//! Auth middleware is opt-in: set `BONSAI_REQUIRE_AUTH=1` to enforce bearer
//! token validation on all API requests.

use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
    Json,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::AppState;

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub(super) struct ApiKeyListResponse {
    pub keys: Vec<ApiKeySummary>,
}

#[derive(Serialize)]
pub(super) struct ApiKeySummary {
    pub id: String,
    pub alias: String,
    pub scope: String,
    pub created_at_ns: i64,
    pub last_used_at_ns: i64,
    pub expires_at_ns: i64,
    pub enabled: bool,
}

#[derive(Deserialize)]
pub(super) struct CreateApiKeyRequest {
    pub alias: String,
    /// Comma-separated scopes: "read", "write", "remediation", "webhook", "admin", or "*" for all.
    #[serde(default = "default_scope")]
    pub scope: String,
    /// Optional expiry in seconds from now. 0 = no expiry.
    #[serde(default)]
    pub expires_in_secs: i64,
}

fn default_scope() -> String {
    "read".to_string()
}

#[derive(Serialize)]
pub(super) struct CreateApiKeyResponse {
    pub success: bool,
    /// The raw API key — shown only once.
    pub api_key: String,
    pub alias: String,
    pub id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
}

#[derive(Serialize, Deserialize)]
struct ApiKeyContent {
    key_hash: String,
    scope: String,
    expires_at_ns: i64,
    last_used_at_ns: i64,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// GET /api/auth/apikeys — list all API keys (hash never exposed).
pub(super) async fn list_api_keys_handler(
    State(state): State<AppState>,
) -> Result<Json<ApiKeyListResponse>, (StatusCode, String)> {
    let items = state
        .store
        .list_config_items(Some("api_key".to_string()))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

    let keys: Vec<ApiKeySummary> = items
        .iter()
        .filter_map(|ci| {
            let content: ApiKeyContent = serde_json::from_str(&ci.content_json).ok()?;
            Some(ApiKeySummary {
                id: ci.id.clone(),
                alias: ci.name.clone(),
                scope: content.scope,
                created_at_ns: 0, // DB timestamps handled by upsert
                last_used_at_ns: content.last_used_at_ns,
                expires_at_ns: content.expires_at_ns,
                enabled: ci.enabled,
            })
        })
        .collect();

    Ok(Json(ApiKeyListResponse { keys }))
}

/// POST /api/auth/apikeys — generate a new scoped API key.
pub(super) async fn create_api_key_handler(
    State(state): State<AppState>,
    Json(req): Json<CreateApiKeyRequest>,
) -> Result<Json<CreateApiKeyResponse>, (StatusCode, String)> {
    let alias = req.alias.trim().to_string();
    if alias.is_empty() {
        return Ok(Json(CreateApiKeyResponse {
            success: false,
            api_key: String::new(),
            alias,
            id: String::new(),
            error: "alias is required".to_string(),
        }));
    }

    // Generate a random key from two UUIDs (256 bits of entropy)
    let raw_key = format!("bsk_{}_{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());

    // Hash for storage
    let key_hash = hex::encode(Sha256::digest(raw_key.as_bytes()));

    let now_ns = super::now_ns();
    let expires_at_ns = if req.expires_in_secs > 0 {
        now_ns + (req.expires_in_secs * 1_000_000_000)
    } else {
        0 // no expiry
    };

    let id = format!("apikey-{}", &key_hash[..16]);
    let content = ApiKeyContent {
        key_hash,
        scope: req.scope.clone(),
        expires_at_ns,
        last_used_at_ns: 0,
    };

    let item = crate::graph::ConfigItemRecord {
        id: id.clone(),
        config_class: "api_key".to_string(),
        vendor: String::new(),
        name: alias.clone(),
        version: String::new(),
        content_json: serde_json::to_string(&content).unwrap_or_default(),
        enabled: true,
        created_by: "ui".to_string(),
    };

    state
        .store
        .upsert_config_item(item)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

    Ok(Json(CreateApiKeyResponse {
        success: true,
        api_key: raw_key,
        alias,
        id,
        error: String::new(),
    }))
}

/// DELETE /api/auth/apikeys/{id} — revoke an API key.
pub(super) async fn delete_api_key_handler(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    // Disable rather than hard-delete so audit trail is preserved
    let items = state
        .store
        .list_config_items(Some("api_key".to_string()))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

    let existing = items.into_iter().find(|ci| ci.id == id);
    match existing {
        Some(mut item) => {
            item.enabled = false;
            state
                .store
                .upsert_config_item(item)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            Ok(StatusCode::NO_CONTENT)
        }
        None => Err((StatusCode::NOT_FOUND, format!("API key '{id}' not found"))),
    }
}

/// POST /api/auth/apikeys/{id}/rotate — rotate an existing key (generates
/// new secret, preserves alias + scope).
pub(super) async fn rotate_api_key_handler(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<CreateApiKeyResponse>, (StatusCode, String)> {
    let items = state
        .store
        .list_config_items(Some("api_key".to_string()))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

    let existing = items.into_iter().find(|ci| ci.id == id);
    match existing {
        Some(item) => {
            let old_content: ApiKeyContent =
                serde_json::from_str(&item.content_json).unwrap_or(ApiKeyContent {
                    key_hash: String::new(),
                    scope: "read".to_string(),
                    expires_at_ns: 0,
                    last_used_at_ns: 0,
                });

            let raw_key = format!("bsk_{}_{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
            let key_hash = hex::encode(Sha256::digest(raw_key.as_bytes()));

            let new_id = format!("apikey-{}", &key_hash[..16]);
            let content = ApiKeyContent {
                key_hash,
                scope: old_content.scope,
                expires_at_ns: old_content.expires_at_ns,
                last_used_at_ns: 0,
            };

            // Disable old key
            let mut old = item.clone();
            old.enabled = false;
            let _ = state.store.upsert_config_item(old).await;

            // Create new key
            let new_item = crate::graph::ConfigItemRecord {
                id: new_id.clone(),
                config_class: "api_key".to_string(),
                vendor: String::new(),
                name: item.name.clone(),
                version: String::new(),
                content_json: serde_json::to_string(&content).unwrap_or_default(),
                enabled: true,
                created_by: "ui".to_string(),
            };
            state
                .store
                .upsert_config_item(new_item)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

            Ok(Json(CreateApiKeyResponse {
                success: true,
                api_key: raw_key,
                alias: item.name,
                id: new_id,
                error: String::new(),
            }))
        }
        None => Ok(Json(CreateApiKeyResponse {
            success: false,
            api_key: String::new(),
            alias: String::new(),
            id,
            error: "API key not found".to_string(),
        })),
    }
}

// ── Auth middleware ──────────────────────────────────────────────────────────

/// Axum middleware: if `BONSAI_REQUIRE_AUTH=1`, validate `Authorization: Bearer <key>`.
/// Unauthenticated requests to /health, /healthz, /readyz are always allowed.
pub(super) async fn auth_middleware(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let require_auth = std::env::var("BONSAI_REQUIRE_AUTH")
        .unwrap_or_default()
        .trim()
        == "1";

    if !require_auth {
        return Ok(next.run(request).await);
    }

    // Always allow health endpoints
    let path = request.uri().path();
    if path == "/health" || path == "/healthz" || path == "/readyz" {
        return Ok(next.run(request).await);
    }

    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    let bearer = match auth_header {
        Some(h) if h.starts_with("Bearer ") => &h[7..],
        _ => return Err(StatusCode::UNAUTHORIZED),
    };

    // Hash the presented key and look up in DB
    let presented_hash = hex::encode(Sha256::digest(bearer.as_bytes()));

    let items = state
        .store
        .list_config_items(Some("api_key".to_string()))
        .await
        .unwrap_or_default();

    let matched = items.iter().find(|ci| {
        if !ci.enabled {
            return false;
        }
        let Ok(content) = serde_json::from_str::<ApiKeyContent>(&ci.content_json) else {
            return false;
        };
        if content.key_hash != presented_hash {
            return false;
        }
        // Check expiry
        if content.expires_at_ns > 0 {
            let now = super::now_ns();
            if now > content.expires_at_ns {
                return false;
            }
        }
        true
    });

    match matched {
        Some(_key_item) => {
            // TODO: update last_used_at_ns (batched, not per-request)
            Ok(next.run(request).await)
        }
        None => Err(StatusCode::UNAUTHORIZED),
    }
}

