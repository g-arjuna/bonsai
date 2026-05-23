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

// ── D4-3 T2: RBAC model ─────────────────────────────────────────────────────
//
// Roles: admin, operator, viewer, api_readonly.
// Users stored as ConfigItem (config_class = "user").
// Sessions stored as ConfigItem (config_class = "session").

/// Roles from least to most privileged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)]
pub(super) enum Role {
    ApiReadonly,
    Viewer,
    Operator,
    Admin,
}

impl Role {
    fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "admin" => Role::Admin,
            "operator" => Role::Operator,
            "viewer" => Role::Viewer,
            _ => Role::ApiReadonly,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Operator => "operator",
            Role::Viewer => "viewer",
            Role::ApiReadonly => "api_readonly",
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct UserContent {
    password_hash: String,
    role: String,
    last_login_ns: i64,
}

#[derive(Serialize)]
pub(super) struct UserSummary {
    pub id: String,
    pub username: String,
    pub role: String,
    pub enabled: bool,
    pub last_login_ns: i64,
}

#[derive(Serialize)]
pub(super) struct UserListResponse {
    pub users: Vec<UserSummary>,
}

#[derive(Deserialize)]
pub(super) struct CreateUserRequest {
    pub username: String,
    pub password: String,
    #[serde(default = "default_role")]
    pub role: String,
}

fn default_role() -> String {
    "viewer".to_string()
}

#[derive(Deserialize)]
pub(super) struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub(super) struct LoginResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub token: String,
    pub username: String,
    pub role: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
}

#[derive(Serialize, Deserialize)]
struct SessionContent {
    username: String,
    role: String,
    token_hash: String,
    expires_at_ns: i64,
}

/// POST /api/auth/login — authenticate and create a session token.
pub(super) async fn login_handler(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, String)> {
    let username = req.username.trim().to_string();
    if username.is_empty() || req.password.is_empty() {
        return Ok(Json(LoginResponse {
            success: false, token: String::new(), username,
            role: String::new(), error: "username and password required".into(),
        }));
    }

    // Look up user in ConfigItem DB
    let users = state.store.list_config_items(Some("user".to_string())).await.unwrap_or_default();
    let user_item = users.iter().find(|ci| ci.name == username && ci.enabled);

    // Also check bootstrap admin from env
    let admin_user = std::env::var("BONSAI_ADMIN_USER").unwrap_or_default();
    let admin_pass = std::env::var("BONSAI_ADMIN_PASS").unwrap_or_default();

    let (role_str, is_env_admin) = if !admin_user.is_empty() && username == admin_user && req.password == admin_pass {
        ("admin".to_string(), true)
    } else if let Some(ci) = user_item {
        let content: UserContent = serde_json::from_str(&ci.content_json)
            .unwrap_or(UserContent { password_hash: String::new(), role: "viewer".into(), last_login_ns: 0 });
        let pw_hash = hex::encode(Sha256::digest(req.password.as_bytes()));
        if pw_hash != content.password_hash {
            return Ok(Json(LoginResponse {
                success: false, token: String::new(), username,
                role: String::new(), error: "invalid credentials".into(),
            }));
        }
        (content.role, false)
    } else if state.ldap_config.enabled && !state.ldap_config.server_url.is_empty() {
        // D4-3 T3: LDAP/AD authentication fallback.
        // Attempt LDAP simple bind with the user's credentials.
        match ldap_authenticate(&state.ldap_config, &username, &req.password).await {
            Ok(role) => (role, false),
            Err(e) => {
                tracing::warn!(username = %username, error = %e, "LDAP authentication failed");
                return Ok(Json(LoginResponse {
                    success: false, token: String::new(), username,
                    role: String::new(), error: format!("LDAP auth failed: {e}"),
                }));
            }
        }
    } else {
        return Ok(Json(LoginResponse {
            success: false, token: String::new(), username,
            role: String::new(), error: "invalid credentials".into(),
        }));
    };

    // Generate session token
    let raw_token = format!("bst_{}_{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let token_hash = hex::encode(Sha256::digest(raw_token.as_bytes()));
    let now_ns = super::now_ns();
    let expires_at_ns = now_ns + (24 * 3600 * 1_000_000_000_i64); // 24h session

    let session_id = format!("session-{}", &token_hash[..16]);
    let session = SessionContent {
        username: username.clone(),
        role: role_str.clone(),
        token_hash,
        expires_at_ns,
    };
    let session_item = crate::graph::ConfigItemRecord {
        id: session_id,
        config_class: "session".to_string(),
        vendor: String::new(),
        name: username.clone(),
        version: String::new(),
        content_json: serde_json::to_string(&session).unwrap_or_default(),
        enabled: true,
        created_by: if is_env_admin { "env_bootstrap".into() } else { "login".into() },
    };
    let _ = state.store.upsert_config_item(session_item).await;

    // Update last_login_ns on the user record (skip for env admin)
    if !is_env_admin {
        if let Some(ci) = user_item {
            let mut content: UserContent = serde_json::from_str(&ci.content_json)
                .unwrap_or(UserContent { password_hash: String::new(), role: "viewer".into(), last_login_ns: 0 });
            content.last_login_ns = now_ns;
            let mut updated = ci.clone();
            updated.content_json = serde_json::to_string(&content).unwrap_or_default();
            let _ = state.store.upsert_config_item(updated).await;
        }
    }

    Ok(Json(LoginResponse {
        success: true,
        token: raw_token,
        username,
        role: role_str,
        error: String::new(),
    }))
}

/// POST /api/auth/logout — invalidate the current session.
pub(super) async fn logout_handler(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
) -> Result<StatusCode, (StatusCode, String)> {
    if let Some(bearer) = request.headers().get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    {
        let hash = hex::encode(Sha256::digest(bearer.as_bytes()));
        let sessions = state.store.list_config_items(Some("session".to_string())).await.unwrap_or_default();
        for mut s in sessions {
            if let Ok(sc) = serde_json::from_str::<SessionContent>(&s.content_json) {
                if sc.token_hash == hash {
                    s.enabled = false;
                    let _ = state.store.upsert_config_item(s).await;
                    break;
                }
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/auth/users — list all users (admin only, but enforcement is at middleware level).
pub(super) async fn list_users_handler(
    State(state): State<AppState>,
) -> Result<Json<UserListResponse>, (StatusCode, String)> {
    let items = state.store.list_config_items(Some("user".to_string())).await.unwrap_or_default();
    let users = items.iter().filter_map(|ci| {
        let content: UserContent = serde_json::from_str(&ci.content_json).ok()?;
        Some(UserSummary {
            id: ci.id.clone(),
            username: ci.name.clone(),
            role: content.role,
            enabled: ci.enabled,
            last_login_ns: content.last_login_ns,
        })
    }).collect();
    Ok(Json(UserListResponse { users }))
}

/// POST /api/auth/users — create or update a user.
pub(super) async fn create_user_handler(
    State(state): State<AppState>,
    Json(req): Json<CreateUserRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let username = req.username.trim().to_string();
    if username.is_empty() || req.password.is_empty() {
        return Ok(Json(serde_json::json!({"success": false, "error": "username and password required"})));
    }

    let valid_roles = ["admin", "operator", "viewer", "api_readonly"];
    let role = if valid_roles.contains(&req.role.as_str()) { req.role.clone() } else { "viewer".to_string() };

    let pw_hash = hex::encode(Sha256::digest(req.password.as_bytes()));
    let content = UserContent {
        password_hash: pw_hash,
        role: role.clone(),
        last_login_ns: 0,
    };
    let id = format!("user-{}", username.to_lowercase().replace(' ', "-"));
    let item = crate::graph::ConfigItemRecord {
        id,
        config_class: "user".to_string(),
        vendor: String::new(),
        name: username.clone(),
        version: String::new(),
        content_json: serde_json::to_string(&content).unwrap_or_default(),
        enabled: true,
        created_by: "admin".to_string(),
    };
    state.store.upsert_config_item(item).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

    Ok(Json(serde_json::json!({"success": true, "username": username, "role": role})))
}

/// DELETE /api/auth/users/{id} — disable a user.
pub(super) async fn delete_user_handler(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let items = state.store.list_config_items(Some("user".to_string())).await.unwrap_or_default();
    match items.into_iter().find(|ci| ci.id == id) {
        Some(mut item) => {
            item.enabled = false;
            state.store.upsert_config_item(item).await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            Ok(StatusCode::NO_CONTENT)
        }
        None => Err((StatusCode::NOT_FOUND, format!("User '{id}' not found"))),
    }
}

/// Minimum role required for a given HTTP method + path combination.
fn required_role(method: &str, path: &str) -> Role {
    // Health: no auth needed (handled before this)
    // Admin-only: vault, user management, DB purge, restart
    if path.starts_with("/api/auth/users")
        || path.starts_with("/api/vault")
        || path == "/api/db/purge"
        || path == "/api/restart"
    {
        return Role::Admin;
    }
    // Operator: write operations (POST/PUT/PATCH/DELETE on mutations)
    if path.starts_with("/api/remediations") && method != "GET"
        || path.starts_with("/api/shun") && method != "GET"
        || path.starts_with("/api/investigations") && method == "POST"
        || path.starts_with("/api/managed-devices") && method != "GET"
        || path.starts_with("/api/credentials") && method != "GET"
        || path.starts_with("/api/settings") && method == "PATCH"
    {
        return Role::Operator;
    }
    // Everything else: viewer for GET, operator for other methods
    if method == "GET" || method == "HEAD" || method == "OPTIONS" {
        return Role::Viewer;
    }
    Role::Operator
}

// ── Auth middleware ──────────────────────────────────────────────────────────

/// Axum middleware: if `BONSAI_REQUIRE_AUTH=1`, validate `Authorization: Bearer <key>`.
/// Supports both API keys (bsk_ prefix) and session tokens (bst_ prefix).
/// Enforces RBAC role checks based on method + path.
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

    // Always allow health + login endpoints
    let path = request.uri().path().to_string();
    if path == "/health" || path == "/healthz" || path == "/readyz" || path == "/api/auth/login" {
        return Ok(next.run(request).await);
    }
    // Allow static UI files
    if !path.starts_with("/api/") {
        return Ok(next.run(request).await);
    }

    let method = request.method().as_str().to_string();

    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let bearer = match auth_header.as_deref() {
        Some(h) if h.starts_with("Bearer ") => &h[7..],
        _ => return Err(StatusCode::UNAUTHORIZED),
    };

    let presented_hash = hex::encode(Sha256::digest(bearer.as_bytes()));

    // Try API key first
    let mut authenticated_role: Option<Role> = None;

    if bearer.starts_with("bsk_") {
        let items = state.store.list_config_items(Some("api_key".to_string())).await.unwrap_or_default();
        for ci in &items {
            if !ci.enabled { continue; }
            let Ok(content) = serde_json::from_str::<ApiKeyContent>(&ci.content_json) else { continue; };
            if content.key_hash != presented_hash { continue; }
            if content.expires_at_ns > 0 && super::now_ns() > content.expires_at_ns { continue; }
            // API key scope → role mapping
            let role = match content.scope.as_str() {
                "admin" | "*" => Role::Admin,
                "write" | "remediation" => Role::Operator,
                "webhook" => Role::Operator,
                _ => Role::ApiReadonly,
            };
            authenticated_role = Some(role);
            break;
        }
    }

    // Try session token
    if authenticated_role.is_none() && bearer.starts_with("bst_") {
        let sessions = state.store.list_config_items(Some("session".to_string())).await.unwrap_or_default();
        for ci in &sessions {
            if !ci.enabled { continue; }
            let Ok(sc) = serde_json::from_str::<SessionContent>(&ci.content_json) else { continue; };
            if sc.token_hash != presented_hash { continue; }
            if sc.expires_at_ns > 0 && super::now_ns() > sc.expires_at_ns { continue; }
            authenticated_role = Some(Role::from_str(&sc.role));
            break;
        }
    }

    // Fallback: try as raw API key (no prefix)
    if authenticated_role.is_none() {
        let items = state.store.list_config_items(Some("api_key".to_string())).await.unwrap_or_default();
        for ci in &items {
            if !ci.enabled { continue; }
            let Ok(content) = serde_json::from_str::<ApiKeyContent>(&ci.content_json) else { continue; };
            if content.key_hash != presented_hash { continue; }
            if content.expires_at_ns > 0 && super::now_ns() > content.expires_at_ns { continue; }
            authenticated_role = Some(Role::ApiReadonly);
            break;
        }
    }

    let Some(user_role) = authenticated_role else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    // RBAC enforcement
    let needed = required_role(&method, &path);
    if user_role < needed {
        return Err(StatusCode::FORBIDDEN);
    }

    Ok(next.run(request).await)
}

// ── D4-3 T3: LDAP/AD integration ───────────────────────────────────────────
//
// LDAP authentication via simple bind. Uses raw TCP socket with a minimal
// LDAP v3 simple bind implementation to avoid adding the ldap3 crate
// dependency. For production deployments with complex LDAP needs (StartTLS,
// SASL, paged search), the ldap3 crate should be added.

/// Authenticate a user via LDAP simple bind and determine their role from
/// group membership. Returns the Bonsai role string on success.
async fn ldap_authenticate(
    ldap: &crate::config::LdapConfig,
    username: &str,
    password: &str,
) -> Result<String, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    // Parse server URL to get host:port
    let url = &ldap.server_url;
    let (host, port, _use_tls) = if let Some(rest) = url.strip_prefix("ldaps://") {
        let (h, p) = parse_host_port(rest, 636);
        (h, p, true)
    } else if let Some(rest) = url.strip_prefix("ldap://") {
        let (h, p) = parse_host_port(rest, 389);
        (h, p, false)
    } else {
        return Err("invalid LDAP server_url (must start with ldap:// or ldaps://)".into());
    };

    // Build the user DN for simple bind.
    // For AD: user_search_filter with {username} replaced, prepended to user_search_base.
    let user_dn = if ldap.user_search_base.is_empty() {
        // Fall back to sAMAccountName-style: CN=username,<base>
        format!("CN={},{}", username, ldap.group_search_base)
    } else {
        format!("CN={},{}", username, ldap.user_search_base)
    };

    // Connect
    let addr = format!("{host}:{port}");
    let mut stream = TcpStream::connect(&addr)
        .await
        .map_err(|e| format!("LDAP connect to {addr}: {e}"))?;

    // LDAP v3 simple bind request (minimal BER encoding)
    let bind_request = build_ldap_simple_bind(&user_dn, password);
    stream.write_all(&bind_request).await
        .map_err(|e| format!("LDAP write: {e}"))?;

    // Read response
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await
        .map_err(|e| format!("LDAP read: {e}"))?;
    if n == 0 {
        return Err("LDAP connection closed without response".into());
    }

    // Parse bind response — check result code
    // The result code is at a known offset in the LDAP BindResponse BER structure.
    // For a successful bind, the resultCode is 0.
    let result_code = parse_ldap_bind_result(&buf[..n]);
    if result_code != 0 {
        return Err(format!("LDAP bind failed (result code {result_code})"));
    }

    // Bind succeeded. Now determine role via group membership.
    // For simplicity, we search the role_mapping for a match.
    // In a full implementation, we'd do an LDAP search for the user's groups.
    // For now, use the default_role since group search requires a separate
    // LDAP search operation with the bound connection.
    let role = ldap.default_role.clone();

    tracing::info!(username = %username, role = %role, "LDAP authentication successful");
    Ok(role)
}

fn parse_host_port(s: &str, default_port: u16) -> (String, u16) {
    if let Some(idx) = s.rfind(':') {
        let host = s[..idx].to_string();
        let port = s[idx + 1..].parse().unwrap_or(default_port);
        (host, port)
    } else {
        (s.to_string(), default_port)
    }
}

/// Build a minimal LDAP v3 BindRequest (simple auth) in BER encoding.
fn build_ldap_simple_bind(dn: &str, password: &str) -> Vec<u8> {
    // BindRequest ::= [APPLICATION 0] SEQUENCE {
    //   version  INTEGER (3),
    //   name     LDAPDN,
    //   authentication AuthenticationChoice (CHOICE { simple [0] OCTET STRING })
    // }
    let mut bind_req = Vec::new();
    // version = 3
    ber_encode_integer(&mut bind_req, 3);
    // name = dn
    ber_encode_octet_string(&mut bind_req, dn.as_bytes());
    // simple auth = [0] password
    let mut auth = vec![0x80]; // context tag 0, primitive
    ber_encode_length(&mut auth, password.len());
    auth.extend_from_slice(password.as_bytes());
    bind_req.extend_from_slice(&auth);

    // Wrap in APPLICATION 0 SEQUENCE
    let mut app0 = vec![0x60]; // APPLICATION 0, constructed
    ber_encode_length(&mut app0, bind_req.len());
    app0.extend_from_slice(&bind_req);

    // Wrap in LDAPMessage SEQUENCE { messageID INTEGER, protocolOp }
    let mut msg_body = Vec::new();
    ber_encode_integer(&mut msg_body, 1); // messageID = 1
    msg_body.extend_from_slice(&app0);

    let mut msg = vec![0x30]; // SEQUENCE
    ber_encode_length(&mut msg, msg_body.len());
    msg.extend_from_slice(&msg_body);

    msg
}

fn ber_encode_integer(out: &mut Vec<u8>, val: i64) {
    out.push(0x02); // INTEGER tag
    if val <= 127 {
        out.push(1);
        out.push(val as u8);
    } else {
        let bytes = val.to_be_bytes();
        let start = bytes.iter().position(|&b| b != 0).unwrap_or(7);
        let len = 8 - start;
        out.push(len as u8);
        out.extend_from_slice(&bytes[start..]);
    }
}

fn ber_encode_octet_string(out: &mut Vec<u8>, data: &[u8]) {
    out.push(0x04); // OCTET STRING tag
    ber_encode_length(out, data.len());
    out.extend_from_slice(data);
}

fn ber_encode_length(out: &mut Vec<u8>, len: usize) {
    if len < 128 {
        out.push(len as u8);
    } else if len < 256 {
        out.push(0x81);
        out.push(len as u8);
    } else {
        out.push(0x82);
        out.push((len >> 8) as u8);
        out.push(len as u8);
    }
}

/// Parse LDAP BindResponse and extract the resultCode (0 = success).
fn parse_ldap_bind_result(data: &[u8]) -> u8 {
    // LDAPMessage { messageID, BindResponse [APPLICATION 1] { resultCode, ... } }
    // Walk past: SEQUENCE tag+len, INTEGER (messageID), APPLICATION 1 tag+len,
    // then read ENUMERATED (resultCode).
    if data.len() < 10 { return 255; }
    let mut pos = 0;
    // Skip SEQUENCE tag + length
    if data[pos] != 0x30 { return 254; }
    pos += 1;
    pos += skip_ber_length(data, pos);
    // Skip messageID INTEGER
    if pos >= data.len() || data[pos] != 0x02 { return 253; }
    pos += 1;
    let id_len = data.get(pos).copied().unwrap_or(0) as usize;
    pos += 1 + id_len;
    // BindResponse is APPLICATION 1 (tag = 0x61)
    if pos >= data.len() || data[pos] != 0x61 { return 252; }
    pos += 1;
    pos += skip_ber_length(data, pos);
    // resultCode is ENUMERATED (tag 0x0A)
    if pos >= data.len() || data[pos] != 0x0A { return 251; }
    pos += 1;
    let rc_len = data.get(pos).copied().unwrap_or(0) as usize;
    pos += 1;
    if rc_len == 0 || pos >= data.len() { return 250; }
    data[pos]
}

fn skip_ber_length(data: &[u8], pos: usize) -> usize {
    if pos >= data.len() { return 1; }
    let first = data[pos];
    if first < 128 {
        1
    } else {
        let num_bytes = (first & 0x7F) as usize;
        1 + num_bytes
    }
}

/// GET /api/auth/ldap/config — return LDAP config (without passwords).
pub(super) async fn ldap_config_handler(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let ldap = &state.ldap_config;
    Json(serde_json::json!({
        "enabled": ldap.enabled,
        "server_url": ldap.server_url,
        "bind_dn": ldap.bind_dn,
        "user_search_base": ldap.user_search_base,
        "user_search_filter": ldap.user_search_filter,
        "group_search_base": ldap.group_search_base,
        "role_mapping": ldap.role_mapping,
        "default_role": ldap.default_role,
    }))
}

/// POST /api/auth/ldap/test — test LDAP connection by attempting a bind
/// with the configured service account.
pub(super) async fn ldap_test_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let ldap = &state.ldap_config;
    if !ldap.enabled {
        return Ok(Json(serde_json::json!({"success": false, "error": "LDAP not enabled"})));
    }
    let bind_password = if ldap.bind_password_env.is_empty() {
        String::new()
    } else {
        std::env::var(&ldap.bind_password_env).unwrap_or_default()
    };
    if bind_password.is_empty() {
        return Ok(Json(serde_json::json!({"success": false, "error": "bind password env not set"})));
    }
    match ldap_authenticate(ldap, &ldap.bind_dn, &bind_password).await {
        Ok(_) => Ok(Json(serde_json::json!({"success": true, "message": "LDAP bind successful"}))),
        Err(e) => Ok(Json(serde_json::json!({"success": false, "error": e}))),
    }
}
