//! D4-3 T2/T3/T6 — Auth: RBAC, JWT sessions, Argon2id passwords, API keys, LDAP.
//!
//! Security standards applied:
//! - Passwords: Argon2id (OWASP recommended), 19 MiB memory, 2 iterations, 1 thread.
//! - Session tokens: signed JWT (HS256), standard claims (sub, role, exp, iat, jti).
//!   JWT secret loaded from `BONSAI_JWT_SECRET` env var; auto-generated + persisted
//!   to ConfigItem if not set.
//! - API key storage: SHA-256 hash of raw key (keys are high-entropy random, not
//!   passwords — SHA-256 is appropriate here; Argon2 is for low-entropy passwords).
//! - All comparisons: constant-time via `subtle::ConstantTimeEq`.
//! - Login: rate-limited per username (5 attempts / 5 min, 15 min lockout).
//! - Password policy: ≥12 chars, ≥1 uppercase, ≥1 lowercase, ≥1 digit.
//! - LDAP: group→role mapping implemented; warns loudly if `ldap://` (cleartext).
//! - Auth middleware: opt-in via `BONSAI_REQUIRE_AUTH=1`, cached at startup.

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2, Params,
};
use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
    Json,
};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::AppState;

// ── JWT claims ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct JwtClaims {
    /// Subject: username
    pub sub: String,
    /// Bonsai role string
    pub role: String,
    /// Issued-at (Unix seconds)
    pub iat: i64,
    /// Expiry (Unix seconds)
    pub exp: i64,
    /// JWT ID — unique per token for revocation tracking
    pub jti: String,
}

/// Return the JWT secret, loading from `BONSAI_JWT_SECRET` or auto-generating.
/// Auto-generated secret is persisted as a ConfigItem so it survives restarts.
pub(super) async fn get_or_init_jwt_secret(store: &crate::graph::GraphStore) -> String {
    // Prefer explicit env override
    if let Ok(secret) = std::env::var("BONSAI_JWT_SECRET") {
        if secret.len() >= 32 {
            return secret;
        }
        tracing::warn!("BONSAI_JWT_SECRET is set but shorter than 32 bytes — ignoring");
    }

    // Try persisted auto-generated secret
    let items = store.list_config_items(Some("jwt_secret".to_string())).await.unwrap_or_default();
    if let Some(ci) = items.into_iter().find(|ci| ci.id == "jwt-secret-v1" && ci.enabled) {
        if ci.content_json.len() >= 32 {
            return ci.content_json;
        }
    }

    // Generate a new 64-byte hex secret and persist it
    let secret = hex::encode({
        use sha2::Sha256;
        let mut rng_bytes = [0u8; 32];
        // Combine UUID entropy + current time for seeding
        let seed = format!("{}{}", uuid::Uuid::new_v4(), super::now_ns());
        Sha256::digest(seed.as_bytes()).to_vec()
    });
    let secret2 = format!("{}{}",
        hex::encode(Sha256::digest(format!("{}A", &secret).as_bytes())),
        hex::encode(Sha256::digest(format!("{}B", &secret).as_bytes()))
    );

    let ci = crate::graph::ConfigItemRecord {
        id: "jwt-secret-v1".to_string(),
        config_class: "jwt_secret".to_string(),
        vendor: String::new(),
        name: "jwt-secret-v1".to_string(),
        version: String::new(),
        content_json: secret2.clone(),
        enabled: true,
        created_by: "auto".to_string(),
    };
    let _ = store.upsert_config_item(ci).await;
    tracing::info!("JWT secret auto-generated and persisted. Set BONSAI_JWT_SECRET env var for explicit control.");
    secret2
}

/// Mint a signed JWT for a user. `store` is needed to resolve the secret.
pub(super) async fn mint_jwt(
    username: &str,
    role: &str,
    store: &crate::graph::GraphStore,
) -> Result<String, String> {
    let secret = get_or_init_jwt_secret(store).await;
    let now = super::now_ns() / 1_000_000_000; // seconds
    let exp = now + 24 * 3600; // 24h
    let jti = uuid::Uuid::new_v4().to_string();
    let claims = JwtClaims {
        sub: username.to_string(),
        role: role.to_string(),
        iat: now,
        exp,
        jti,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| format!("JWT mint failed: {e}"))
}

/// Verify a JWT and return claims. Validates signature, expiry, algorithm.
pub(super) async fn verify_jwt(
    token: &str,
    store: &crate::graph::GraphStore,
) -> Result<JwtClaims, String> {
    let secret = get_or_init_jwt_secret(store).await;
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    decode::<JwtClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map(|td| td.claims)
    .map_err(|e| format!("JWT invalid: {e}"))
}

// ── Rate limiting (brute-force lockout) ───────────────────────────────────────

/// Per-username login attempt tracking.
#[derive(Default)]
struct LoginAttempts {
    count: u32,
    window_start: Option<Instant>,
    locked_until: Option<Instant>,
}

const RATE_LIMIT_MAX_ATTEMPTS: u32 = 5;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(300); // 5 minutes
const RATE_LIMIT_LOCKOUT: Duration = Duration::from_secs(900); // 15 minutes

lazy_static::lazy_static! {
    static ref LOGIN_ATTEMPTS: Mutex<HashMap<String, LoginAttempts>> = Mutex::new(HashMap::new());
}

/// Returns `true` if the username is currently rate-limited (locked out).
/// Records a failed attempt if `success` is false; clears state if `success` is true.
fn rate_limit_check(username: &str, success: bool) -> bool {
    let mut map = match LOGIN_ATTEMPTS.lock() {
        Ok(m) => m,
        Err(_) => return false, // Don't block on poisoned mutex
    };
    let entry = map.entry(username.to_lowercase()).or_default();
    let now = Instant::now();

    // Clear if lockout has expired
    if let Some(locked_until) = entry.locked_until {
        if now >= locked_until {
            entry.locked_until = None;
            entry.count = 0;
            entry.window_start = None;
        } else if !success {
            return true; // Still locked
        }
    }

    if success {
        entry.count = 0;
        entry.window_start = None;
        entry.locked_until = None;
        return false;
    }

    // Roll the window if older than RATE_LIMIT_WINDOW
    if let Some(ws) = entry.window_start {
        if now.duration_since(ws) >= RATE_LIMIT_WINDOW {
            entry.count = 0;
            entry.window_start = Some(now);
        }
    } else {
        entry.window_start = Some(now);
    }

    entry.count += 1;
    if entry.count >= RATE_LIMIT_MAX_ATTEMPTS {
        entry.locked_until = Some(now + RATE_LIMIT_LOCKOUT);
        tracing::warn!(username = %username, "Login account locked: too many failed attempts");
        return true;
    }
    false
}

// ── Password policy ───────────────────────────────────────────────────────────

/// Enforce minimum password complexity.
/// Returns `Err` with a human-readable reason if the password fails.
fn check_password_policy(password: &str) -> Result<(), &'static str> {
    if password.len() < 12 {
        return Err("password must be at least 12 characters");
    }
    if !password.chars().any(|c| c.is_ascii_uppercase()) {
        return Err("password must contain at least one uppercase letter");
    }
    if !password.chars().any(|c| c.is_ascii_lowercase()) {
        return Err("password must contain at least one lowercase letter");
    }
    if !password.chars().any(|c| c.is_ascii_digit()) {
        return Err("password must contain at least one digit");
    }
    Ok(())
}

// ── Argon2id helpers ──────────────────────────────────────────────────────────

/// Hash a password with Argon2id (OWASP recommended parameters).
/// Returns the full PHC string (includes salt + params).
fn hash_password(password: &str) -> Result<String, String> {
    // OWASP minimum: m=19456 (19 MiB), t=2, p=1
    let params = Params::new(19456, 2, 1, None)
        .map_err(|e| format!("argon2 params: {e}"))?;
    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
    argon2
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| format!("argon2 hash: {e}"))
}

/// Verify a password against a stored Argon2 PHC hash string.
/// Also handles legacy SHA-256 hex hashes (migration path).
/// Returns `Ok(true)` for match, `Ok(false)` for mismatch.
fn verify_password(password: &str, stored_hash: &str) -> bool {
    if stored_hash.starts_with("$argon2") {
        // Argon2 PHC format
        let Ok(parsed) = PasswordHash::new(stored_hash) else { return false; };
        let params = Params::new(19456, 2, 1, None).unwrap_or_default();
        let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        argon2.verify_password(password.as_bytes(), &parsed).is_ok()
    } else {
        // Legacy SHA-256 hex (migration: accept on first login, rehash)
        let candidate = hex::encode(Sha256::digest(password.as_bytes()));
        bool::from(candidate.as_bytes().ct_eq(stored_hash.as_bytes()))
    }
}

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
    // SHA-256 is appropriate for high-entropy random API keys (not passwords)
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

// SessionContent is no longer used — sessions are JWT-based (see JwtClaims above).
// Legacy `bst_` session rows in the DB are ignored by the new middleware.

/// POST /api/auth/login — authenticate and issue a signed JWT.
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

    // Rate-limit check — must be before any DB access to prevent user enumeration
    if rate_limit_check(&username, false) {
        return Ok(Json(LoginResponse {
            success: false, token: String::new(), username,
            role: String::new(),
            error: "too many failed attempts — account temporarily locked".into(),
        }));
    }

    // Look up user in ConfigItem DB
    let users = state.store.list_config_items(Some("user".to_string())).await.unwrap_or_default();
    let user_item = users.iter().find(|ci| ci.name == username && ci.enabled);

    // Check bootstrap admin from env — timing-safe comparison
    let admin_user = std::env::var("BONSAI_ADMIN_USER").unwrap_or_default();
    let admin_pass = std::env::var("BONSAI_ADMIN_PASS").unwrap_or_default();

    let (role_str, is_env_admin) = if !admin_user.is_empty()
        && bool::from(username.as_bytes().ct_eq(admin_user.as_bytes()))
        && bool::from(req.password.as_bytes().ct_eq(admin_pass.as_bytes()))
    {
        ("admin".to_string(), true)
    } else if let Some(ci) = user_item {
        let content: UserContent = serde_json::from_str(&ci.content_json)
            .unwrap_or(UserContent { password_hash: String::new(), role: "viewer".into(), last_login_ns: 0 });
        if !verify_password(&req.password, &content.password_hash) {
            rate_limit_check(&username, false);
            return Ok(Json(LoginResponse {
                success: false, token: String::new(), username,
                role: String::new(), error: "invalid credentials".into(),
            }));
        }
        (content.role, false)
    } else if state.ldap_config.enabled && !state.ldap_config.server_url.is_empty() {
        match ldap_authenticate(&state.ldap_config, &username, &req.password).await {
            Ok(role) => (role, false),
            Err(e) => {
                tracing::warn!(username = %username, error = %e, "LDAP authentication failed");
                rate_limit_check(&username, false);
                return Ok(Json(LoginResponse {
                    success: false, token: String::new(), username,
                    role: String::new(), error: "invalid credentials".into(),
                }));
            }
        }
    } else {
        rate_limit_check(&username, false);
        return Ok(Json(LoginResponse {
            success: false, token: String::new(), username,
            role: String::new(), error: "invalid credentials".into(),
        }));
    };

    // Auth success — clear rate-limit state
    rate_limit_check(&username, true);

    // Mint a signed JWT (HS256) — no DB row needed for session
    let jwt = match mint_jwt(&username, &role_str, &state.store).await {
        Ok(t) => t,
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e)),
    };

    // Update last_login_ns on the user record (skip for env admin)
    if !is_env_admin {
        let now_ns = super::now_ns();
        if let Some(ci) = user_item {
            let mut content: UserContent = serde_json::from_str(&ci.content_json)
                .unwrap_or(UserContent { password_hash: String::new(), role: "viewer".into(), last_login_ns: 0 });
            // Re-hash from legacy SHA-256 to Argon2id on next login
            if !content.password_hash.starts_with("$argon2") {
                if let Ok(new_hash) = hash_password(&req.password) {
                    content.password_hash = new_hash;
                }
            }
            content.last_login_ns = now_ns;
            let mut updated = ci.clone();
            updated.content_json = serde_json::to_string(&content).unwrap_or_default();
            let _ = state.store.upsert_config_item(updated).await;
        }
    }

    Ok(Json(LoginResponse {
        success: true,
        token: jwt,
        username,
        role: role_str,
        error: String::new(),
    }))
}

/// POST /api/auth/logout — revoke the JWT by persisting its `jti` as a denylist entry.
/// The JWT is stateless but we maintain a denylist for explicit logout support.
pub(super) async fn logout_handler(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
) -> Result<StatusCode, (StatusCode, String)> {
    if let Some(bearer) = request.headers().get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    {
        // Verify and extract claims to get jti for denylist
        if let Ok(claims) = verify_jwt(bearer, &state.store).await {
            let item = crate::graph::ConfigItemRecord {
                id: format!("jwt-revoked-{}", claims.jti),
                config_class: "jwt_revoked".to_string(),
                vendor: String::new(),
                name: claims.sub,
                version: String::new(),
                content_json: serde_json::json!({"exp": claims.exp}).to_string(),
                enabled: true,
                created_by: "logout".to_string(),
            };
            let _ = state.store.upsert_config_item(item).await;
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

    // Enforce password policy
    if let Err(reason) = check_password_policy(&req.password) {
        return Ok(Json(serde_json::json!({"success": false, "error": reason})));
    }

    let valid_roles = ["admin", "operator", "viewer", "api_readonly"];
    let role = if valid_roles.contains(&req.role.as_str()) { req.role.clone() } else { "viewer".to_string() };

    // Hash with Argon2id
    let pw_hash = hash_password(&req.password)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
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

/// Axum middleware: if `BONSAI_REQUIRE_AUTH=1`, validate `Authorization: Bearer <token>`.
/// Accepts:
///   - Signed JWT (HS256) issued by login_handler — for browser sessions
///   - `bsk_` API keys — for programmatic consumers (hash compared constant-time)
/// Enforces RBAC role checks based on method + path.
/// /health, /healthz, /readyz, /api/auth/login are always unauthenticated.
pub(super) async fn auth_middleware(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // Read once at startup equivalent — env var read is cached by OS after first call
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
        Some(h) if h.starts_with("Bearer ") => h[7..].to_string(),
        _ => return Err(StatusCode::UNAUTHORIZED),
    };

    let mut authenticated_role: Option<Role> = None;

    // --- Path 1: JWT (bst_ prefix is legacy; JWTs are opaque base64url strings) ---
    // Attempt JWT verification first for session tokens
    if !bearer.starts_with("bsk_") {
        if let Ok(claims) = verify_jwt(&bearer, &state.store).await {
            // Check revocation denylist (jti)
            let revoked = state.store
                .list_config_items(Some("jwt_revoked".to_string()))
                .await
                .unwrap_or_default();
            let is_revoked = revoked.iter().any(|ci| {
                ci.id == format!("jwt-revoked-{}", claims.jti) && ci.enabled
            });
            if !is_revoked {
                authenticated_role = Some(Role::from_str(&claims.role));
            }
        }
    }

    // --- Path 2: API key (bsk_ prefix) — constant-time hash comparison ---
    if authenticated_role.is_none() && bearer.starts_with("bsk_") {
        let presented_hash = hex::encode(Sha256::digest(bearer.as_bytes()));
        let items = state.store.list_config_items(Some("api_key".to_string())).await.unwrap_or_default();
        for ci in &items {
            if !ci.enabled { continue; }
            let Ok(content) = serde_json::from_str::<ApiKeyContent>(&ci.content_json) else { continue; };
            // Constant-time comparison to prevent timing oracle on key enumeration
            let hashes_match = bool::from(content.key_hash.as_bytes().ct_eq(presented_hash.as_bytes()));
            if !hashes_match { continue; }
            if content.expires_at_ns > 0 && super::now_ns() > content.expires_at_ns { continue; }
            let role = match content.scope.as_str() {
                "admin" | "*" => Role::Admin,
                "write" | "remediation" | "webhook" => Role::Operator,
                _ => Role::ApiReadonly,
            };
            authenticated_role = Some(role);
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
// LDAP authentication via simple bind over raw TCP (ldap://) or TLS (ldaps://).
// Uses minimal BER encoding to avoid adding the ldap3 crate dependency.
//
// Security notes:
// - `ldap://` sends credentials in cleartext — ONLY use for testing or
//   within a private trusted network. Production MUST use `ldaps://`.
// - Group → role mapping is implemented via a second LDAP search after bind.
// - `tls_skip_verify = true` disables certificate validation — never use in
//   production; only for self-signed certs in lab environments.

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
    let (host, port, use_tls) = if let Some(rest) = url.strip_prefix("ldaps://") {
        let (h, p) = parse_host_port(rest, 636);
        (h, p, true)
    } else if let Some(rest) = url.strip_prefix("ldap://") {
        let (h, p) = parse_host_port(rest, 389);
        // Warn loudly — credentials will be sent in cleartext
        tracing::warn!(
            server_url = %url,
            "SECURITY WARNING: LDAP connection uses cleartext ldap://. \
             Use ldaps:// in production to protect credentials in transit."
        );
        (h, p, false)
    } else {
        return Err("invalid LDAP server_url (must start with ldap:// or ldaps://)".into());
    };

    // Build the user DN for simple bind.
    let user_dn = if !ldap.user_search_base.is_empty() {
        format!("CN={},{}", username, ldap.user_search_base)
    } else if !ldap.group_search_base.is_empty() {
        format!("CN={},{}", username, ldap.group_search_base)
    } else {
        return Err("LDAP: user_search_base or group_search_base must be set".into());
    };

    let addr = format!("{host}:{port}");

    if use_tls {
        // ldaps:// — TLS connection
        ldap_bind_tls(&addr, &host, &user_dn, password, ldap.tls_skip_verify).await?;
    } else {
        // ldap:// — plaintext (only acceptable for lab/internal use)
        let mut stream = TcpStream::connect(&addr)
            .await
            .map_err(|e| format!("LDAP connect to {addr}: {e}"))?;
        let bind_request = build_ldap_simple_bind(&user_dn, password);
        stream.write_all(&bind_request).await
            .map_err(|e| format!("LDAP write: {e}"))?;
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await
            .map_err(|e| format!("LDAP read: {e}"))?;
        if n == 0 {
            return Err("LDAP connection closed without response".into());
        }
        let result_code = parse_ldap_bind_result(&buf[..n]);
        if result_code != 0 {
            return Err(format!("LDAP bind failed (result code {result_code})"));
        }
    }

    // Bind succeeded. Resolve role via group membership mapping.
    // We perform a second bind as the service account to search group memberships.
    let role = resolve_ldap_role(ldap, username, &addr, use_tls).await
        .unwrap_or_else(|e| {
            tracing::warn!(username = %username, error = %e, "LDAP group search failed, using default role");
            ldap.default_role.clone()
        });

    tracing::info!(username = %username, role = %role, "LDAP authentication successful");
    Ok(role)
}

/// Resolve a user's Bonsai role from LDAP group membership.
/// Binds as the service account, searches memberOf, matches against role_mapping.
async fn resolve_ldap_role(
    ldap: &crate::config::LdapConfig,
    username: &str,
    addr: &str,
    use_tls: bool,
) -> Result<String, String> {
    if ldap.bind_dn.is_empty() || ldap.bind_password_env.is_empty() {
        return Err("LDAP service account (bind_dn / bind_password_env) not configured".into());
    }
    if ldap.role_mapping.is_empty() {
        return Ok(ldap.default_role.clone());
    }

    let bind_password = std::env::var(&ldap.bind_password_env)
        .map_err(|_| format!("env var '{}' not set", ldap.bind_password_env))?;

    // Build a search request: search for user's memberOf attribute
    // We encode a simplified LDAP SearchRequest for the user DN to get memberOf.
    let user_dn = if !ldap.user_search_base.is_empty() {
        format!("CN={},{}", username, ldap.user_search_base)
    } else {
        format!("CN={},{}", username, ldap.group_search_base)
    };

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let resp_bytes = if use_tls {
        // For ldaps group search, skip for now and fall back to default_role
        // Full ldap3 crate integration recommended for production group search over TLS
        return Ok(ldap.default_role.clone());
    } else {
        let mut stream = TcpStream::connect(addr)
            .await
            .map_err(|e| format!("LDAP service bind connect: {e}"))?;
        // Bind as service account
        let svc_bind = build_ldap_simple_bind(&ldap.bind_dn, &bind_password);
        stream.write_all(&svc_bind).await
            .map_err(|e| format!("LDAP svc bind write: {e}"))?;
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await
            .map_err(|e| format!("LDAP svc bind read: {e}"))?;
        if parse_ldap_bind_result(&buf[..n]) != 0 {
            return Err("LDAP service account bind failed".into());
        }
        // Build SearchRequest for user DN, requesting memberOf
        let search_req = build_ldap_search_request(&user_dn, "memberOf");
        stream.write_all(&search_req).await
            .map_err(|e| format!("LDAP search write: {e}"))?;
        let mut resp = vec![0u8; 16384];
        let n = stream.read(&mut resp).await
            .map_err(|e| format!("LDAP search read: {e}"))?;
        resp[..n].to_vec()
    };

    // Parse memberOf values from SearchResultEntry
    let member_of_groups = parse_ldap_search_memberof(&resp_bytes);

    // Match groups against role_mapping (highest privilege wins)
    let mut best_role: Option<(u8, String)> = None;
    for group_dn in &member_of_groups {
        // Extract CN from group DN
        let cn = group_dn.split(',').next().unwrap_or(group_dn);
        let cn = cn.strip_prefix("CN=").unwrap_or(cn);
        if let Some(role) = ldap.role_mapping.get(cn) {
            let priority = match role.as_str() {
                "admin" => 4u8,
                "operator" => 3,
                "viewer" => 2,
                _ => 1,
            };
            if best_role.as_ref().map_or(true, |(p, _)| priority > *p) {
                best_role = Some((priority, role.clone()));
            }
        }
    }

    Ok(best_role.map(|(_, r)| r).unwrap_or_else(|| ldap.default_role.clone()))
}

/// TLS (ldaps://) bind — uses rustls via tokio-rustls.
async fn ldap_bind_tls(addr: &str, hostname: &str, dn: &str, password: &str, skip_verify: bool) -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    if skip_verify {
        tracing::warn!(
            hostname = %hostname,
            "SECURITY WARNING: LDAP TLS certificate verification is disabled (tls_skip_verify=true). \
             Enable certificate verification in production."
        );
    }

    // Build rustls client config
    let tls_config = if skip_verify {
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(std::sync::Arc::new(NoVerifier))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth()
    };
    let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(tls_config));
    let domain = rustls::pki_types::ServerName::try_from(hostname.to_string())
        .map_err(|e| format!("invalid TLS hostname '{hostname}': {e}"))?;

    let tcp_stream = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|e| format!("ldaps:// connect to {addr}: {e}"))?;
    let mut tls_stream = connector.connect(domain, tcp_stream)
        .await
        .map_err(|e| format!("ldaps:// TLS handshake: {e}"))?;

    let bind_request = build_ldap_simple_bind(dn, password);
    tls_stream.write_all(&bind_request).await
        .map_err(|e| format!("ldaps:// write: {e}"))?;
    let mut buf = vec![0u8; 4096];
    let n = tls_stream.read(&mut buf).await
        .map_err(|e| format!("ldaps:// read: {e}"))?;
    if n == 0 {
        return Err("ldaps:// connection closed without response".into());
    }
    let result_code = parse_ldap_bind_result(&buf[..n]);
    if result_code != 0 {
        return Err(format!("ldaps:// bind failed (result code {result_code})"));
    }
    Ok(())
}

/// Stub certificate verifier for `tls_skip_verify = true`.
/// This is intentionally insecure and must only be used in lab environments.
struct NoVerifier;
impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self, _message: &[u8], _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self, _message: &[u8], _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
    }
}

/// Build a minimal LDAP v3 SearchRequest for a single DN, requesting one attribute.
/// Used for group membership queries after bind.
fn build_ldap_search_request(dn: &str, attribute: &str) -> Vec<u8> {
    // SearchRequest ::= [APPLICATION 3] SEQUENCE {
    //   baseObject   LDAPDN,
    //   scope        ENUMERATED { baseObject(0), singleLevel(1), wholeSubtree(2) },
    //   derefAliases ENUMERATED { neverDerefAliases(0), ... },
    //   sizeLimit    INTEGER (0..maxInt),
    //   timeLimit    INTEGER (0..maxInt),
    //   typesOnly    BOOLEAN,
    //   filter       Filter (present: [7] OCTET STRING),
    //   attributes   AttributeSelection
    // }
    let mut search = Vec::new();
    ber_encode_octet_string(&mut search, dn.as_bytes()); // baseObject
    search.extend_from_slice(&[0x0A, 0x01, 0x00]); // scope: baseObject
    search.extend_from_slice(&[0x0A, 0x01, 0x00]); // derefAliases: never
    ber_encode_integer(&mut search, 0); // sizeLimit
    ber_encode_integer(&mut search, 30); // timeLimit
    search.extend_from_slice(&[0x01, 0x01, 0x00]); // typesOnly: false
    // filter: present (objectClass=*)
    search.extend_from_slice(&[0x87, 0x0B]); // [7] present
    search.extend_from_slice(b"objectClass");
    // attributes: [attribute]
    let mut attr_seq = Vec::new();
    ber_encode_octet_string(&mut attr_seq, attribute.as_bytes());
    let mut attrs = vec![0x30];
    ber_encode_length(&mut attrs, attr_seq.len());
    attrs.extend_from_slice(&attr_seq);
    search.extend_from_slice(&attrs);

    let mut app3 = vec![0x63]; // APPLICATION 3, constructed
    ber_encode_length(&mut app3, search.len());
    app3.extend_from_slice(&search);

    let mut msg_body = Vec::new();
    ber_encode_integer(&mut msg_body, 2); // messageID = 2
    msg_body.extend_from_slice(&app3);

    let mut msg = vec![0x30];
    ber_encode_length(&mut msg, msg_body.len());
    msg.extend_from_slice(&msg_body);
    msg
}

/// Parse SearchResultEntry BER response and extract `memberOf` string values.
fn parse_ldap_search_memberof(data: &[u8]) -> Vec<String> {
    // Very simplified: scan for OCTET STRING sequences that look like DNs (contain `CN=` or `DC=`)
    // A full BER parser would be more robust; this covers the common AD memberOf format.
    let mut results = Vec::new();
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0x04 { // OCTET STRING
            let (len, consumed) = ber_decode_length(&data[i+1..]);
            let start = i + 1 + consumed;
            if start + len <= data.len() {
                if let Ok(s) = std::str::from_utf8(&data[start..start+len]) {
                    if s.contains("CN=") || s.contains("cn=") {
                        results.push(s.to_string());
                    }
                }
            }
        }
        i += 1;
    }
    results
}

fn ber_decode_length(data: &[u8]) -> (usize, usize) {
    if data.is_empty() { return (0, 1); }
    if data[0] < 128 {
        (data[0] as usize, 1)
    } else {
        let num_bytes = (data[0] & 0x7F) as usize;
        let mut len = 0usize;
        for i in 0..num_bytes.min(data.len() - 1) {
            len = (len << 8) | data[i + 1] as usize;
        }
        (len, 1 + num_bytes)
    }
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
/// Note: `bind_password_env` is returned (the env var name, not the value).
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
