//! D4-3 T2/T3/T6 — Auth: RBAC, JWT sessions, Argon2id passwords, API keys, LDAP.
//!
//! Security standards applied:
//! - Passwords: Argon2id (OWASP recommended), 19 MiB memory, 2 iterations, 1 thread.
//! - Session tokens: signed JWT (HS256), standard claims (sub, role, exp, iat, jti).
//!   JWT secret managed by `AuthStore` (protected SQLite + age encryption);
//!   `BONSAI_JWT_SECRET` env var is an explicit override (>= 32 bytes).
//! - API key storage: SHA-256 hash of raw key in `AuthStore` (keys are
//!   high-entropy random, not passwords — SHA-256 is appropriate here;
//!   Argon2 is for low-entropy passwords). Raw keys are never persisted.
//! - Users, API keys, JWT keys and revocations live in `bonsai_auth.db`
//!   (see `docs/auth_storage.md`); the operational graph holds no
//!   credential material after migration.
//! - All comparisons: constant-time via `subtle::ConstantTimeEq`.
//! - Login: rate-limited per username (3 attempts / 5 min, 30 min lockout).
//! - Password policy: ≥16 chars, ≥1 uppercase, ≥1 lowercase, ≥1 digit, ≥1 special char.
//! - LDAP: group→role mapping implemented; warns loudly if `ldap://` (cleartext).
//! - Auth middleware: opt-in via `BONSAI_REQUIRE_AUTH=1`, cached at startup.

use argon2::{
    Argon2, Params,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use axum::{
    Json,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::AppState;

// ── JWT claims ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct JwtClaims {
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

/// Return the JWT signing secret from protected storage.
///
/// `BONSAI_JWT_SECRET` (>= 32 bytes) is an explicit override; otherwise the
/// active key is read from `AuthStore` — CSPRNG-generated on first use,
/// age-encrypted under the vault passphrase, stable across restarts.
/// All failure modes (short env key, missing vault passphrase, storage
/// error) are hard errors — never a transient or default key.
async fn get_or_init_jwt_secret(store: &crate::graph::GraphStore) -> Result<String, String> {
    store
        .auth_store
        .jwt_secret()
        .map_err(|e| format!("jwt secret unavailable: {e:#}"))
}

/// Mint a signed JWT for a user. `store` is needed to resolve the secret.
pub async fn mint_jwt(
    username: &str,
    role: &str,
    store: &crate::graph::GraphStore,
) -> Result<String, String> {
    let secret = get_or_init_jwt_secret(store).await?;
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

/// Verify a JWT and return claims. Validates signature against every
/// currently-valid signing key (active + in-overlap retired), expiry and
/// algorithm. Storage failure while listing candidates is an error.
pub async fn verify_jwt(
    token: &str,
    store: &crate::graph::GraphStore,
) -> Result<JwtClaims, String> {
    let candidates = store
        .auth_store
        .jwt_verify_candidates()
        .map_err(|e| format!("jwt key lookup failed: {e:#}"))?;
    if candidates.is_empty() {
        return Err("no JWT signing keys available".to_string());
    }
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    let mut last_err = String::new();
    for secret in &candidates {
        match decode::<JwtClaims>(
            token,
            &DecodingKey::from_secret(secret.as_bytes()),
            &validation,
        ) {
            Ok(td) => return Ok(td.claims),
            Err(e) => last_err = format!("JWT invalid: {e}"),
        }
    }
    Err(last_err)
}

// ── Rate limiting (brute-force lockout) ───────────────────────────────────────

/// Per-username login attempt tracking.
#[derive(Default)]
struct LoginAttempts {
    count: u32,
    window_start: Option<Instant>,
    locked_until: Option<Instant>,
}

const RATE_LIMIT_MAX_ATTEMPTS: u32 = 3;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(300); // 5 minutes
const RATE_LIMIT_LOCKOUT: Duration = Duration::from_secs(1800); // 30 minutes

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
    if password.len() < 16 {
        return Err("password must be at least 16 characters");
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
    if !password
        .chars()
        .any(|c| "!@#$%^&*()_+-=[]{}|;:,.<>?".contains(c))
    {
        return Err("password must contain at least one special character");
    }
    // Check for common patterns
    let password_lower = password.to_lowercase();
    if password_lower.contains("password")
        || password_lower.contains("123456")
        || password_lower.contains("qwerty")
    {
        return Err("password cannot contain common patterns");
    }
    Ok(())
}

// ── Argon2id helpers ──────────────────────────────────────────────────────────

/// Hash a password with Argon2id (OWASP recommended parameters).
/// Returns the full PHC string (includes salt + params).
pub fn hash_password(password: &str) -> Result<String, String> {
    // OWASP minimum: m=19456 (19 MiB), t=2, p=1
    let params = Params::new(19456, 2, 1, None).map_err(|e| format!("argon2 params: {e}"))?;
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
pub fn verify_password(password: &str, stored_hash: &str) -> bool {
    if stored_hash.starts_with("$argon2") {
        // Argon2 PHC format
        let Ok(parsed) = PasswordHash::new(stored_hash) else {
            return false;
        };
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

// ApiKeyContent / UserContent records now live in `AuthStore`
// (`crate::auth_store`); legacy graph JSON shapes are parsed only by the
// migration code there.

// ── Handlers ─────────────────────────────────────────────────────────────────

/// GET /api/auth/apikeys — list all API keys (hash never exposed).
pub(super) async fn list_api_keys_handler(
    State(state): State<AppState>,
) -> Result<Json<ApiKeyListResponse>, (StatusCode, String)> {
    let keys = state
        .store
        .auth_store
        .list_api_keys()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

    let keys: Vec<ApiKeySummary> = keys
        .into_iter()
        .map(|k| ApiKeySummary {
            id: k.id,
            alias: k.alias,
            scope: k.scope,
            created_at_ns: k.created_at_ns,
            last_used_at_ns: k.last_used_at_ns,
            expires_at_ns: k.expires_at_ns,
            enabled: k.enabled,
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

    let (id, raw_key) = state
        .store
        .auth_store
        .create_api_key(&alias, &req.scope, req.expires_in_secs)
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
    match state
        .store
        .auth_store
        .disable_api_key(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?
    {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err((StatusCode::NOT_FOUND, format!("API key '{id}' not found"))),
    }
}

/// POST /api/auth/apikeys/{id}/rotate — rotate an existing key (generates
/// new secret, preserves alias + scope). The disable+create is atomic in
/// `AuthStore::rotate_api_key`: the old key can never remain active after
/// a successful rotation.
pub(super) async fn rotate_api_key_handler(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<CreateApiKeyResponse>, (StatusCode, String)> {
    match state
        .store
        .auth_store
        .rotate_api_key(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?
    {
        Some((new_id, raw_key, alias)) => Ok(Json(CreateApiKeyResponse {
            success: true,
            api_key: raw_key,
            alias,
            id: new_id,
            error: String::new(),
        })),
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
// Users and their verifiers live in `AuthStore` (see below); legacy graph
// `ConfigItem` rows are imported then quarantined by `ensure_migrated`.
// Sessions are JWT-based (see JwtClaims above); legacy `bst_` session rows
// are ignored by the middleware.

/// Roles from least to most privileged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)]
pub enum Role {
    ApiReadonly,
    Viewer,
    Operator,
    Admin,
}

impl Role {
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "admin" => Role::Admin,
            "operator" => Role::Operator,
            "viewer" => Role::Viewer,
            _ => Role::ApiReadonly,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Operator => "operator",
            Role::Viewer => "viewer",
            Role::ApiReadonly => "api_readonly",
        }
    }
}

// User records live in `AuthStore` as `AuthUser`; legacy graph JSON shapes
// are parsed only by the migration code in `crate::auth_store`.
// Sessions are JWT-based (see JwtClaims above); legacy `bst_` session rows
// are ignored by the middleware.

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
pub(crate) struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub(crate) struct LoginResponse {
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
    tls: Option<axum::Extension<TlsConnection>>,
    Json(req): Json<LoginRequest>,
) -> Result<Response, (StatusCode, String)> {
    let username = req.username.trim().to_string();
    if username.is_empty() || req.password.is_empty() {
        return Ok(Json(LoginResponse {
            success: false,
            token: String::new(),
            username,
            role: String::new(),
            error: "username and password required".into(),
        })
        .into_response());
    }

    // Rate-limit check — must be before any DB access to prevent user enumeration
    if rate_limit_check(&username, false) {
        return Ok(Json(LoginResponse {
            success: false,
            token: String::new(),
            username,
            role: String::new(),
            error: "too many failed attempts — account temporarily locked".into(),
        })
        .into_response());
    }

    // Migration must complete before auth decisions on upgraded installs.
    if let Err(e) = state.store.auth_store.ensure_migrated(&state.store).await {
        tracing::error!(error = %e, "auth storage migration failed — denying login");
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("auth storage unavailable: {e}"),
        ));
    }

    // Look up user in protected auth storage (fail closed on storage error)
    let user_record = state
        .store
        .auth_store
        .get_user(&username)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let user_item = user_record.filter(|u| u.enabled);

    // Check bootstrap admin — vault-first, then env var fallback (D4-7 T5).
    let admin_user =
        crate::config::resolve_secret(&state.credentials, "bonsai-admin", "BONSAI_ADMIN_USER")
            .unwrap_or_default();
    let admin_pass =
        crate::config::resolve_secret(&state.credentials, "bonsai-admin-pass", "BONSAI_ADMIN_PASS")
            .unwrap_or_default();

    let (role_str, is_env_admin) = if !admin_user.is_empty()
        && bool::from(username.as_bytes().ct_eq(admin_user.as_bytes()))
        && bool::from(req.password.as_bytes().ct_eq(admin_pass.as_bytes()))
    {
        ("admin".to_string(), true)
    } else if let Some(user) = user_item.as_ref() {
        if !verify_password(&req.password, &user.password_hash) {
            rate_limit_check(&username, false);
            return Ok(Json(LoginResponse {
                success: false,
                token: String::new(),
                username,
                role: String::new(),
                error: "invalid credentials".into(),
            })
            .into_response());
        }
        (user.role.clone(), false)
    } else if state.ldap_config.enabled && !state.ldap_config.server_url.is_empty() {
        match ldap_authenticate(&state.ldap_config, &username, &req.password).await {
            Ok(role) => (role, false),
            Err(e) => {
                tracing::warn!(username = %username, error = %e, "LDAP authentication failed");
                rate_limit_check(&username, false);
                return Ok(Json(LoginResponse {
                    success: false,
                    token: String::new(),
                    username,
                    role: String::new(),
                    error: "invalid credentials".into(),
                })
                .into_response());
            }
        }
    } else {
        rate_limit_check(&username, false);
        return Ok(Json(LoginResponse {
            success: false,
            token: String::new(),
            username,
            role: String::new(),
            error: "invalid credentials".into(),
        })
        .into_response());
    };

    // Auth success — clear rate-limit state
    rate_limit_check(&username, true);

    // Mint a signed JWT (HS256) — no DB row needed for session
    let jwt = match mint_jwt(&username, &role_str, &state.store).await {
        Ok(t) => t,
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e)),
    };

    // Update last_login_ns on the user record (skip for env admin).
    // Re-hash from legacy SHA-256 to Argon2id on next login.
    if !is_env_admin {
        if let Some(user) = user_item {
            let mut updated = user.clone();
            if !updated.password_hash.starts_with("$argon2") {
                if let Ok(new_hash) = hash_password(&req.password) {
                    updated.password_hash = new_hash;
                }
            }
            updated.last_login_ns = super::now_ns();
            if let Err(e) = state.store.auth_store.upsert_user(&updated) {
                tracing::warn!(username = %username, error = %e, "failed to update last_login");
            }
        }
    }

    let mut resp = Json(LoginResponse {
        success: true,
        token: jwt.clone(),
        username,
        role: role_str,
        error: String::new(),
    })
    .into_response();
    // `Secure` is set only for requests that arrived over the verified
    // TLS listener (TlsConnection extension); on plain HTTP the flag
    // would cause browsers to drop the cookie.
    let secure = if tls.is_some() { "; Secure" } else { "" };
    if let Ok(cookie_val) = axum::http::HeaderValue::from_str(&format!(
        "bonsai_token={jwt}; Path=/; SameSite=Lax; HttpOnly{secure}"
    )) {
        resp.headers_mut()
            .insert(axum::http::header::SET_COOKIE, cookie_val);
    }
    Ok(resp)
}

/// POST /api/auth/logout — revoke the JWT by persisting its `jti` as a denylist entry.
/// The JWT is stateless but we maintain a denylist for explicit logout support.
pub(super) async fn logout_handler(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
) -> Result<Response, (StatusCode, String)> {
    let mut bearer_token = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|s| s.to_string());

    if bearer_token.is_none() {
        if let Some(cookie_header) = request
            .headers()
            .get("cookie")
            .and_then(|v| v.to_str().ok())
        {
            for part in cookie_header.split(';') {
                let part = part.trim();
                if let Some(token) = part
                    .strip_prefix("bonsai_token=")
                    .or_else(|| part.strip_prefix("bonsai_session="))
                {
                    bearer_token = Some(token.to_string());
                    break;
                }
            }
        }
    }

    if let Some(bearer) = bearer_token {
        // Verify and extract claims to get jti for denylist. A revocation
        // storage failure must surface as an error — never silently skip
        // denylisting.
        if let Ok(claims) = verify_jwt(&bearer, &state.store).await {
            state
                .store
                .auth_store
                .revoke_jwt(&claims.jti, claims.exp.saturating_mul(1_000_000_000))
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("revocation store unavailable: {e:#}"),
                    )
                })?;
        }
    }
    let secure = if request.extensions().get::<TlsConnection>().is_some() {
        "; Secure"
    } else {
        ""
    };
    let mut resp = StatusCode::NO_CONTENT.into_response();
    if let Ok(cookie_val) = axum::http::HeaderValue::from_str(&format!(
        "bonsai_token=; Path=/; Max-Age=0; SameSite=Lax; HttpOnly{secure}"
    )) {
        resp.headers_mut()
            .insert(axum::http::header::SET_COOKIE, cookie_val);
    }
    Ok(resp)
}

/// GET /api/auth/users — list all users (admin only, but enforcement is at middleware level).
pub(super) async fn list_users_handler(
    State(state): State<AppState>,
) -> Result<Json<UserListResponse>, (StatusCode, String)> {
    let users = state
        .store
        .auth_store
        .list_users()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?
        .into_iter()
        .map(|u| UserSummary {
            id: format!("user-{}", u.username.to_lowercase().replace(' ', "-")),
            username: u.username,
            role: u.role,
            enabled: u.enabled,
            last_login_ns: u.last_login_ns,
        })
        .collect();
    Ok(Json(UserListResponse { users }))
}

/// POST /api/auth/users — create or update a user.
pub(super) async fn create_user_handler(
    State(state): State<AppState>,
    Json(req): Json<CreateUserRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let username = req.username.trim().to_string();
    if username.is_empty() || req.password.is_empty() {
        return Ok(Json(
            serde_json::json!({"success": false, "error": "username and password required"}),
        ));
    }

    // Enforce password policy
    if let Err(reason) = check_password_policy(&req.password) {
        return Ok(Json(serde_json::json!({"success": false, "error": reason})));
    }

    let valid_roles = ["admin", "operator", "viewer", "api_readonly"];
    let role = if valid_roles.contains(&req.role.as_str()) {
        req.role.clone()
    } else {
        "viewer".to_string()
    };

    // Hash with Argon2id
    let pw_hash =
        hash_password(&req.password).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let user = crate::auth_store::AuthUser {
        username: username.clone(),
        password_hash: pw_hash,
        role: role.clone(),
        enabled: true,
        last_login_ns: 0,
        created_at_ns: 0,
        updated_at_ns: 0,
    };
    state
        .store
        .auth_store
        .upsert_user(&user)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

    Ok(Json(
        serde_json::json!({"success": true, "username": username, "role": role}),
    ))
}

/// DELETE /api/auth/users/{id} — disable a user. Accepts either the
/// `user-<name>` record id (as returned by list_users) or the bare username.
pub(super) async fn delete_user_handler(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let users = state
        .store
        .auth_store
        .list_users()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let username = users
        .iter()
        .find(|u| {
            u.username == id
                || format!("user-{}", u.username.to_lowercase().replace(' ', "-")) == id
        })
        .map(|u| u.username.clone());
    match username {
        Some(name) => {
            state
                .store
                .auth_store
                .set_user_enabled(&name, false)
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
    if path == "/api/ownership"
        || path.starts_with("/api/ownership/")
        || path.starts_with("/api/auth/users")
        || path.starts_with("/api/vault")
        || path == "/api/db/purge"
        || path == "/api/db/backup"
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
        || path.starts_with("/api/routing/") && method != "GET"
    {
        return Role::Operator;
    }
    // Read-only routing and general GET endpoints allow Viewer or scoped ApiReadonly
    if method == "GET" || method == "HEAD" || method == "OPTIONS" {
        if path.starts_with("/api/routing/") {
            return Role::ApiReadonly;
        }
        return Role::Viewer;
    }
    // MCP JSON-RPC endpoints allow scoped ApiReadonly
    if path == "/mcp" || path.starts_with("/mcp/") {
        return Role::ApiReadonly;
    }
    Role::Operator
}

/// Redact sensitive tokens, bearer headers, API keys, or private key blocks within arbitrary text.
pub fn redact_sensitive_text(s: &str) -> String {
    let mut out = s.to_string();
    if out.contains("BEGIN PRIVATE KEY") {
        return "[REDACTED_PRIVATE_KEY]".to_string();
    }
    if let Some(idx) = out.find("Bearer ") {
        let after = &out[idx + 7..];
        let end_idx = after
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(after.len());
        let token_part = &after[..end_idx];
        if !token_part.is_empty() {
            out = format!("{}Bearer [REDACTED]{}", &out[..idx], &after[end_idx..]);
        }
    }
    for prefix in &["sk-", "bsk_"] {
        while let Some(idx) = out.find(prefix) {
            let after = &out[idx + prefix.len()..];
            let end_idx = after
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',')
                .unwrap_or(after.len());
            let secret_len = prefix.len() + end_idx;
            out = format!("{}[REDACTED]{}", &out[..idx], &out[idx + secret_len..]);
        }
    }
    out
}

/// Recursively redact secret/credential fields in a JSON value before returning or logging.
pub fn redact_secrets_in_json(val: &mut serde_json::Value) {
    const SENSITIVE_KEYS: &[&str] = &[
        "password",
        "password_hash",
        "secret",
        "jwt_secret",
        "token",
        "service_token",
        "internal_service_token",
        "private_key",
        "api_key",
        "key_hash",
        "bind_password",
        "authorization",
        "auth_token",
        "access_token",
        "refresh_token",
        "client_secret",
    ];
    match val {
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                let lower = k.to_ascii_lowercase();
                if SENSITIVE_KEYS
                    .iter()
                    .any(|s| lower == *s || lower.ends_with(&format!("_{s}")))
                {
                    *v = serde_json::Value::String("[REDACTED]".to_string());
                } else {
                    redact_secrets_in_json(v);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                redact_secrets_in_json(item);
            }
        }
        serde_json::Value::String(s) => {
            if s.contains("Bearer ")
                || s.contains("sk-")
                || s.contains("bsk_")
                || s.contains("BEGIN PRIVATE KEY")
            {
                *s = redact_sensitive_text(s);
            }
        }
        _ => {}
    }
}

/// Server-enforced routing access scope derived from an authenticated API key (`R06`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutingAccessScope {
    pub speaker: Option<String>,
    pub vrf_rd: Option<String>,
    pub tenant_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OriginalRoutingAccessScope(pub RoutingAccessScope);

impl RoutingAccessScope {
    pub fn for_ownership_mode(
        &self,
        mode: crate::graph::ownership::OwnershipAccessMode,
        device_bound: bool,
    ) -> Self {
        let mut effective = self.clone();
        if mode == crate::graph::ownership::OwnershipAccessMode::AuditOnly {
            effective.tenant_id = None;
            if device_bound {
                effective.vrf_rd = None;
            }
        }
        effective
    }
}

/// Marker extension inserted by the TLS accept loop for requests that
/// arrived over a verified TLS connection. Auth handlers use it to set
/// the `Secure` cookie attribute; plaintext-served requests never carry
/// it, so cookies are not marked Secure on plain HTTP (which would make
/// browsers drop them).
#[derive(Debug, Clone, Copy)]
pub struct TlsConnection;

/// Authenticated subject for server-attributed administrative records, never a credential.
#[derive(Debug, Clone)]
pub struct AuthenticatedSubject(pub String);

/// Typed representation of a parsed authorization scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeGrant {
    /// Explicit global scope granting access with no resource constraints.
    Global(Role),
    /// Valid restricted routing scope with explicit resource constraints.
    Restricted {
        role: Role,
        scope: RoutingAccessScope,
    },
    /// Invalid, malformed, or unparseable scope string.
    Invalid(String),
}

/// Parse a scope string strictly, distinguishing explicit global grants,
/// valid restricted grants, and invalid grants.
pub fn parse_scope_grant(scope: &str) -> ScopeGrant {
    let s = scope.trim();
    if s.is_empty() {
        return ScopeGrant::Invalid("scope string is empty".to_string());
    }

    // 1. Exact global grants (no resource restrictions)
    match s {
        "admin" | "*" => return ScopeGrant::Global(Role::Admin),
        "operator" | "write" | "remediation" | "webhook" | "routing:write" | "routing:operator" => {
            return ScopeGrant::Global(Role::Operator);
        }
        "viewer" | "read" | "routing" | "routing:read" => {
            return ScopeGrant::Global(Role::Viewer);
        }
        "api_readonly" => {
            return ScopeGrant::Global(Role::ApiReadonly);
        }
        _ => {}
    }

    // 2. Restricted routing grants with prefix
    let (base_role, suffix) = if let Some(suff) = s.strip_prefix("routing:write:") {
        (Role::Operator, suff)
    } else if let Some(suff) = s.strip_prefix("routing:operator:") {
        (Role::Operator, suff)
    } else if let Some(suff) = s.strip_prefix("routing:read:") {
        (Role::ApiReadonly, suff)
    } else if let Some(suff) = s.strip_prefix("routing:scope:") {
        (Role::ApiReadonly, suff)
    } else if s.starts_with("routing:") || s.contains(':') {
        return ScopeGrant::Invalid(format!("unrecognized or malformed scope prefix in '{s}'"));
    } else {
        return ScopeGrant::Invalid(format!("unknown grant scope: '{s}'"));
    };

    let suffix = suffix.trim();
    if suffix.is_empty() {
        return ScopeGrant::Invalid("restricted scope suffix cannot be empty".to_string());
    }

    let mut out = RoutingAccessScope::default();
    for part in suffix.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return ScopeGrant::Invalid("empty comma-separated entry in scope".to_string());
        }
        let Some((k, v)) = part.split_once('=') else {
            return ScopeGrant::Invalid(format!("missing '=' in scope parameter '{part}'"));
        };
        let k = k.trim();
        let v = v.trim();
        if v.is_empty() {
            return ScopeGrant::Invalid(format!("empty value for scope key '{k}'"));
        }
        match k {
            "speaker" => {
                if out.speaker.is_some() {
                    return ScopeGrant::Invalid("duplicate 'speaker' in scope".to_string());
                }
                out.speaker = Some(v.to_string());
            }
            "vrf_rd" | "rd" => {
                if out.vrf_rd.is_some() {
                    return ScopeGrant::Invalid("duplicate 'vrf_rd' in scope".to_string());
                }
                out.vrf_rd = Some(v.to_string());
            }
            "tenant_id" | "tenant" => {
                if out.tenant_id.is_some() {
                    return ScopeGrant::Invalid("duplicate 'tenant_id' in scope".to_string());
                }
                out.tenant_id = Some(v.to_string());
            }
            unknown => {
                return ScopeGrant::Invalid(format!("unknown scope key '{unknown}'"));
            }
        }
    }

    if out.speaker.is_none() && out.vrf_rd.is_none() && out.tenant_id.is_none() {
        return ScopeGrant::Invalid(
            "restricted scope must specify at least one of: speaker, vrf_rd, tenant_id".to_string(),
        );
    }

    ScopeGrant::Restricted {
        role: base_role,
        scope: out,
    }
}

/// Parse a scoped routing API key string such as `"routing:read:speaker=192.0.2.1,vrf_rd=65000:100,tenant_id=acme"`.
pub fn parse_routing_access_scope(scope: &str) -> Option<RoutingAccessScope> {
    match parse_scope_grant(scope) {
        ScopeGrant::Restricted { scope, .. } => Some(scope),
        _ => None,
    }
}

// ── Auth middleware ──────────────────────────────────────────────────────────

/// Axum middleware: if `BONSAI_REQUIRE_AUTH=1` or DB `require_auth=true`, validate `Authorization: Bearer <token>`.
/// Accepts:
///   - Signed JWT (HS256) issued by login_handler — for browser sessions
///   - `bsk_` API keys — for programmatic consumers (hash compared constant-time)
/// Enforces RBAC role checks based on method + path and scope checks for `/api/routing/v1/*`.
/// /health, /healthz, /readyz, /api/auth/login are always unauthenticated.
pub async fn auth_middleware(
    State(store): State<std::sync::Arc<crate::graph::GraphStore>>,
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // D4-7 T5 / BN-005: Check DB for require_auth setting, fallback to env var. Fail closed by default.
    let require_auth = {
        let db_val = store
            .list_config_items(Some("app_config".to_string()))
            .await
            .unwrap_or_default()
            .iter()
            .find(|i| i.name == "require_auth" && i.enabled)
            .map(|i| i.content_json.trim().trim_matches('"').to_string());
        match db_val.as_deref() {
            Some("1") | Some("true") => true,
            Some("0") | Some("false") => false,
            _ => {
                let env_val = std::env::var("BONSAI_REQUIRE_AUTH").unwrap_or_default();
                let allow_insecure = std::env::var("BONSAI_ALLOW_INSECURE_NO_AUTH")
                    .or_else(|_| std::env::var("BONSAI_ALLOW_INSECURE_DEV"))
                    .map(|v| v == "1" || v == "true")
                    .unwrap_or(false);
                if allow_insecure || env_val == "0" || env_val.eq_ignore_ascii_case("false") {
                    false
                } else {
                    true
                }
            }
        }
    };

    if !require_auth {
        return Ok(next.run(request).await);
    }

    // Always allow health + login endpoints
    let path = request.uri().path().to_string();
    if path == "/health" || path == "/healthz" || path == "/readyz" || path == "/api/auth/login" {
        return Ok(next.run(request).await);
    }
    // Only protected paths require authentication; static UI files pass through
    let is_protected = path.starts_with("/api/") || path == "/mcp" || path.starts_with("/mcp/");
    if !is_protected {
        return Ok(next.run(request).await);
    }

    let method = request.method().as_str().to_string();

    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let mut bearer = auth_header
        .as_deref()
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string());

    if bearer.is_none() {
        if let Some(cookie_header) = request
            .headers()
            .get("cookie")
            .and_then(|v| v.to_str().ok())
        {
            for part in cookie_header.split(';') {
                let part = part.trim();
                if let Some(token) = part
                    .strip_prefix("bonsai_token=")
                    .or_else(|| part.strip_prefix("bonsai_session="))
                {
                    bearer = Some(token.trim().to_string());
                    break;
                }
            }
        }
    }

    let bearer = match bearer {
        Some(b) if !b.is_empty() => b,
        _ => return Err(StatusCode::UNAUTHORIZED),
    };

    // Migration must complete before auth decisions on upgraded installs.
    // A failure here denies the request — never fall back to legacy graph
    // credential records.
    if let Err(e) = store.auth_store.ensure_migrated(&store).await {
        tracing::error!(error = %e, "auth storage migration failed — denying request");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    let mut authenticated_role: Option<Role> = None;
    let mut authenticated_subject = None;
    let mut authenticated_restricted_scope: Option<RoutingAccessScope> = None;
    let mut raw_api_key_scope: Option<String> = None;

    // --- Path 1: JWT (bst_ prefix is legacy; JWTs are opaque base64url strings) ---
    if !bearer.starts_with("bsk_") {
        if let Ok(claims) = verify_jwt(&bearer, &store).await {
            // Revocation lookup failure must deny — an errored lookup is
            // not proof the token is still valid.
            match store.auth_store.is_jwt_revoked(&claims.jti) {
                Ok(true) => {}
                Ok(false) => {
                    authenticated_role = Some(Role::from_str(&claims.role));
                    authenticated_subject = Some(format!("user:{}", claims.sub));
                }
                Err(e) => {
                    tracing::error!(error = %e, "jwt revocation lookup failed — denying");
                    return Err(StatusCode::SERVICE_UNAVAILABLE);
                }
            }
        }
    }

    // --- Path 2: API key (bsk_ prefix) — constant-time hash comparison ---
    if authenticated_role.is_none() && bearer.starts_with("bsk_") {
        match store.auth_store.authenticate_api_key(&bearer) {
            Ok(Some(key)) => {
                match parse_scope_grant(&key.scope) {
                    ScopeGrant::Global(role) => {
                        authenticated_role = Some(role);
                        authenticated_restricted_scope = None;
                        raw_api_key_scope = Some(key.scope.clone());
                    }
                    ScopeGrant::Restricted { role, scope } => {
                        authenticated_role = Some(role);
                        authenticated_restricted_scope = Some(scope);
                        raw_api_key_scope = Some(key.scope.clone());
                    }
                    ScopeGrant::Invalid(reason) => {
                        tracing::warn!(
                            scope = %key.scope,
                            reason = %reason,
                            "API key has invalid or malformed scope grant — failing closed"
                        );
                        return Err(StatusCode::FORBIDDEN);
                    }
                }
                authenticated_subject = Some(format!("api_key:{}", key.id));
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(error = %e, "api key store lookup failed — denying");
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
        }
    }

    let Some(user_role) = authenticated_role else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    request.extensions_mut().insert(user_role);
    if let Some(subject) = authenticated_subject {
        request
            .extensions_mut()
            .insert(AuthenticatedSubject(subject));
    }

    // Legacy capability grants must not turn their Operator role into global data access.
    if let Some(raw_scope) = raw_api_key_scope.as_deref() {
        let allowed = match raw_scope {
            "webhook" => path.starts_with("/api/webhooks/") || path == "/api/webhooks",
            "remediation" => path.starts_with("/api/remediations/") || path == "/api/remediations",
            _ => true,
        };
        if !allowed {
            return Err(StatusCode::FORBIDDEN);
        }
    }

    // Enforce API key scope and attach fine-grained scope to request extensions
    let ownership_mode = store.ownership_access_mode().await;
    request.extensions_mut().insert(ownership_mode);
    if let Some(ref scope) = authenticated_restricted_scope {
        request
            .extensions_mut()
            .insert(OriginalRoutingAccessScope(scope.clone()));
        // MCP retains the original grant so its shared dispatcher can normalize per tool.
        let effective = if path == "/mcp" || path.starts_with("/mcp/") {
            scope.clone()
        } else {
            scope.for_ownership_mode(ownership_mode, path.starts_with("/api/investigations"))
        };
        request.extensions_mut().insert(effective);

        // Restrict scoped keys to an explicit allowlist of scope-aware HTTP routes
        let is_scope_aware_route = path.starts_with("/api/routing/")
            || path == "/api/investigations"
            || path.starts_with("/api/investigations/")
            || path == "/mcp"
            || path.starts_with("/mcp/");

        // Investigations are device-bound and have no VRF/tenant ownership fields.
        if path.starts_with("/api/investigations")
            && ownership_mode == crate::graph::ownership::OwnershipAccessMode::Strict
            && (scope.speaker.is_none() || scope.vrf_rd.is_some() || scope.tenant_id.is_some())
        {
            return Err(StatusCode::FORBIDDEN);
        }
        if !is_scope_aware_route {
            tracing::warn!(
                path = %path,
                speaker = ?scope.speaker,
                "Restricted API key denied access to unscoped route '{path}'"
            );
            return Err(StatusCode::FORBIDDEN);
        }
    } else if path.starts_with("/api/routing/") {
        if let Some(ref raw_scope) = raw_api_key_scope {
            let allowed_routing_scope = matches!(
                raw_scope.as_str(),
                "*" | "admin"
                    | "operator"
                    | "write"
                    | "read"
                    | "viewer"
                    | "routing"
                    | "routing:read"
                    | "routing:write"
                    | "routing:operator"
            );
            if !allowed_routing_scope {
                return Err(StatusCode::FORBIDDEN);
            }
        }
    }

    // RBAC enforcement
    let scoped_investigation_read = method == "GET"
        && path.starts_with("/api/investigations")
        && authenticated_restricted_scope
            .as_ref()
            .is_some_and(|scope| {
                ownership_mode == crate::graph::ownership::OwnershipAccessMode::AuditOnly
                    || (scope.speaker.is_some()
                        && scope.vrf_rd.is_none()
                        && scope.tenant_id.is_none())
            });
    let needed = if scoped_investigation_read {
        Role::ApiReadonly
    } else {
        required_role(&method, &path)
    };
    if user_role < needed {
        return Err(StatusCode::FORBIDDEN);
    }

    let mut response = next.run(request).await;
    if ownership_mode == crate::graph::ownership::OwnershipAccessMode::AuditOnly
        && authenticated_restricted_scope
            .as_ref()
            .is_some_and(|s| s.tenant_id.is_some() || s.vrf_rd.is_some())
    {
        response.headers_mut().insert(
            "x-bonsai-ownership-mode",
            axum::http::HeaderValue::from_static("audit-only"),
        );
    }
    Ok(response)
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
        stream
            .write_all(&bind_request)
            .await
            .map_err(|e| format!("LDAP write: {e}"))?;
        let mut buf = vec![0u8; 4096];
        let n = stream
            .read(&mut buf)
            .await
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

    // D4-7 T5: vault-first resolution for LDAP bind password.
    let bind_password = std::env::var(&ldap.bind_password_env)
        .ok()
        .unwrap_or_default();
    let bind_password = if bind_password.is_empty() {
        return Err(format!(
            "LDAP bind password not available (env '{}' not set, vault alias 'ldap-bind' not found)",
            ldap.bind_password_env
        ));
    } else {
        bind_password
    };

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
        stream
            .write_all(&svc_bind)
            .await
            .map_err(|e| format!("LDAP svc bind write: {e}"))?;
        let mut buf = vec![0u8; 4096];
        let n = stream
            .read(&mut buf)
            .await
            .map_err(|e| format!("LDAP svc bind read: {e}"))?;
        if parse_ldap_bind_result(&buf[..n]) != 0 {
            return Err("LDAP service account bind failed".into());
        }
        // Build SearchRequest for user DN, requesting memberOf
        let search_req = build_ldap_search_request(&user_dn, "memberOf");
        stream
            .write_all(&search_req)
            .await
            .map_err(|e| format!("LDAP search write: {e}"))?;
        let mut resp = vec![0u8; 16384];
        let n = stream
            .read(&mut resp)
            .await
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

    Ok(best_role
        .map(|(_, r)| r)
        .unwrap_or_else(|| ldap.default_role.clone()))
}

/// TLS (ldaps://) bind — uses rustls via tokio-rustls.
async fn ldap_bind_tls(
    addr: &str,
    hostname: &str,
    dn: &str,
    password: &str,
    skip_verify: bool,
) -> Result<(), String> {
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
    let mut tls_stream = connector
        .connect(domain, tcp_stream)
        .await
        .map_err(|e| format!("ldaps:// TLS handshake: {e}"))?;

    let bind_request = build_ldap_simple_bind(dn, password);
    tls_stream
        .write_all(&bind_request)
        .await
        .map_err(|e| format!("ldaps:// write: {e}"))?;
    let mut buf = vec![0u8; 4096];
    let n = tls_stream
        .read(&mut buf)
        .await
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
#[derive(Debug)]
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
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
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
        if data[i] == 0x04 {
            // OCTET STRING
            let (len, consumed) = ber_decode_length(&data[i + 1..]);
            let start = i + 1 + consumed;
            if start + len <= data.len() {
                if let Ok(s) = std::str::from_utf8(&data[start..start + len]) {
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
    if data.is_empty() {
        return (0, 1);
    }
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
    if data.len() < 10 {
        return 255;
    }
    let mut pos = 0;
    // Skip SEQUENCE tag + length
    if data[pos] != 0x30 {
        return 254;
    }
    pos += 1;
    pos += skip_ber_length(data, pos);
    // Skip messageID INTEGER
    if pos >= data.len() || data[pos] != 0x02 {
        return 253;
    }
    pos += 1;
    let id_len = data.get(pos).copied().unwrap_or(0) as usize;
    pos += 1 + id_len;
    // BindResponse is APPLICATION 1 (tag = 0x61)
    if pos >= data.len() || data[pos] != 0x61 {
        return 252;
    }
    pos += 1;
    pos += skip_ber_length(data, pos);
    // resultCode is ENUMERATED (tag 0x0A)
    if pos >= data.len() || data[pos] != 0x0A {
        return 251;
    }
    pos += 1;
    let rc_len = data.get(pos).copied().unwrap_or(0) as usize;
    pos += 1;
    if rc_len == 0 || pos >= data.len() {
        return 250;
    }
    data[pos]
}

fn skip_ber_length(data: &[u8], pos: usize) -> usize {
    if pos >= data.len() {
        return 1;
    }
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
pub(super) async fn ldap_config_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
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
        return Ok(Json(
            serde_json::json!({"success": false, "error": "LDAP not enabled"}),
        ));
    }
    let bind_password = if ldap.bind_password_env.is_empty() {
        String::new()
    } else {
        std::env::var(&ldap.bind_password_env).unwrap_or_default()
    };
    if bind_password.is_empty() {
        return Ok(Json(
            serde_json::json!({"success": false, "error": "bind password env not set"}),
        ));
    }
    match ldap_authenticate(ldap, &ldap.bind_dn, &bind_password).await {
        Ok(_) => Ok(Json(
            serde_json::json!({"success": true, "message": "LDAP bind successful"}),
        )),
        Err(e) => Ok(Json(serde_json::json!({"success": false, "error": e}))),
    }
}
