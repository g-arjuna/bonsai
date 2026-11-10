//! BN-021b: trusted exporter-identity administration.
//!
//! Canonical device registration and exporter→device bindings are
//! inventory authority: admin-only, audited, and persisted in the
//! SQLite config store. Telemetry content can never create or modify
//! these records — there is intentionally no endpoint that derives a
//! binding from observed data.

use super::{
    AppState,
    auth::{AuthenticatedSubject, Role},
};
use crate::telemetry_identity::{
    BindingProvenance, CanonicalDevice, IdentityResolver, TelemetrySourceIdentity, TrustedBinding,
};
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

type ApiError = (StatusCode, String);

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/identity/devices",
            get(list_devices).post(upsert_device),
        )
        .route(
            "/api/identity/bindings",
            get(list_bindings).post(create_binding),
        )
        .route(
            "/api/identity/bindings/{binding_id}/revoke",
            post(revoke_binding),
        )
        .route("/api/identity/resolve", post(resolve_identity))
}

fn store(state: &AppState) -> Result<std::sync::Arc<crate::sqlite_store::SqliteStore>, ApiError> {
    state.registry.sqlite_store().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "identity store unavailable".into(),
    ))
}

fn require_admin(role: Option<Extension<Role>>) -> Result<(), ApiError> {
    match role {
        Some(Extension(Role::Admin)) => Ok(()),
        _ => Err((
            StatusCode::FORBIDDEN,
            "identity administration requires an authenticated admin".into(),
        )),
    }
}

fn actor(subject: Option<Extension<AuthenticatedSubject>>) -> Result<String, ApiError> {
    subject.map(|Extension(s)| s.0).ok_or((
        StatusCode::FORBIDDEN,
        "authenticated subject is required".into(),
    ))
}

fn bad_request(error: impl std::fmt::Display) -> ApiError {
    (StatusCode::BAD_REQUEST, error.to_string())
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(%error, "identity store operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "identity store operation failed".into(),
    )
}

async fn list_devices(
    State(state): State<AppState>,
    role: Option<Extension<Role>>,
) -> Result<Json<Value>, ApiError> {
    require_admin(role)?;
    let devices = store(&state)?.list_canonical_devices().map_err(internal)?;
    Ok(Json(json!({"devices": devices})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceUpsert {
    /// Existing canonical id to update (e.g. address change); omit or
    /// empty to register a new device.
    #[serde(default)]
    device_id: String,
    address: String,
    #[serde(default)]
    site: Option<String>,
    #[serde(default)]
    hostname: Option<String>,
}

async fn upsert_device(
    State(state): State<AppState>,
    role: Option<Extension<Role>>,
    subject: Option<Extension<AuthenticatedSubject>>,
    Json(req): Json<DeviceUpsert>,
) -> Result<Json<Value>, ApiError> {
    require_admin(role)?;
    let who = actor(subject)?;
    if req.address.trim().is_empty() {
        return Err(bad_request("address must not be empty"));
    }
    let device = CanonicalDevice {
        device_id: req.device_id,
        address: req.address,
        site: req.site,
        hostname: req.hostname,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
    };
    let stored = store(&state)?
        .upsert_canonical_device(&device, &who)
        .map_err(|e| {
            if e.to_string().contains("must not be empty") {
                bad_request(e)
            } else {
                internal(e)
            }
        })?;
    Ok(Json(json!({"device": stored})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingsQuery {
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    include_revoked: bool,
}

async fn list_bindings(
    State(state): State<AppState>,
    role: Option<Extension<Role>>,
    Query(q): Query<BindingsQuery>,
) -> Result<Json<Value>, ApiError> {
    require_admin(role)?;
    let bindings = store(&state)?
        .list_exporter_bindings(q.scope.as_deref(), q.protocol.as_deref(), q.include_revoked)
        .map_err(internal)?;
    Ok(Json(json!({"bindings": bindings})))
}

/// Operator-provisioned binding request. `provenance` is NOT accepted
/// from the client — bindings created here are always recorded as
/// `operator_provisioned` with the authenticated actor.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingCreate {
    #[serde(default)]
    binding_id: String,
    scope: String,
    protocol: String,
    /// Canonical logical exporter id, unique per (scope, protocol).
    exporter_id: String,
    /// Canonical inventory device id.
    device_id: String,
    #[serde(default)]
    match_transport: Option<String>,
    #[serde(default)]
    match_reported_identity: Option<String>,
    #[serde(default)]
    match_exporter_claim: Option<String>,
    #[serde(default)]
    match_observation_domain: Option<u32>,
}

async fn create_binding(
    State(state): State<AppState>,
    role: Option<Extension<Role>>,
    subject: Option<Extension<AuthenticatedSubject>>,
    Json(req): Json<BindingCreate>,
) -> Result<Json<Value>, ApiError> {
    require_admin(role)?;
    let who = actor(subject)?;
    let binding = TrustedBinding {
        binding_id: if req.binding_id.is_empty() {
            crate::sqlite_store::SqliteStore::new_binding_id()
        } else {
            req.binding_id
        },
        scope: req.scope,
        protocol: req.protocol,
        exporter_id: req.exporter_id,
        device_id: req.device_id,
        match_transport: req.match_transport,
        match_reported_identity: req.match_reported_identity,
        match_exporter_claim: req.match_exporter_claim,
        match_observation_domain: req.match_observation_domain,
        provenance: BindingProvenance::OperatorProvisioned,
        status: crate::telemetry_identity::BindingStatus::Active,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
    };
    let stored = store(&state)?
        .upsert_exporter_binding(&binding, &who)
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("already bound")
                || msg.contains("overlaps")
                || msg.contains("must not be empty")
                || msg.contains("unknown canonical device")
            {
                bad_request(e)
            } else {
                internal(e)
            }
        })?;
    Ok(Json(json!({"binding": stored})))
}

async fn revoke_binding(
    State(state): State<AppState>,
    role: Option<Extension<Role>>,
    subject: Option<Extension<AuthenticatedSubject>>,
    Path(binding_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_admin(role)?;
    let who = actor(subject)?;
    let binding = store(&state)?
        .revoke_exporter_binding(&binding_id, &who)
        .map_err(|e| {
            if e.to_string().contains("does not exist") {
                bad_request(e)
            } else {
                internal(e)
            }
        })?;
    Ok(Json(json!({"binding": binding})))
}

/// Evaluate an observed identity envelope against the active trusted
/// bindings. Read-only diagnostic — performs no mutation and does not
/// feed authoritative state by itself.
async fn resolve_identity(
    State(state): State<AppState>,
    role: Option<Extension<Role>>,
    Json(observed): Json<TelemetrySourceIdentity>,
) -> Result<Json<Value>, ApiError> {
    require_admin(role)?;
    let store = store(&state)?;
    let bindings = store
        .list_exporter_bindings(None, None, false)
        .map_err(internal)?;
    let resolution = IdentityResolver::resolve(&observed, &bindings);
    Ok(Json(json!({"resolution": resolution})))
}
