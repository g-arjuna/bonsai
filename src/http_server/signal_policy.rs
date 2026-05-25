/// DS-5: Signal feature-gate HTTP API.
///
/// Endpoints:
///   GET    /api/signal-policy              — list all policies
///   POST   /api/signal-policy              — upsert a policy entry
///   DELETE /api/signal-policy/{id}         — remove a policy entry
///   GET    /api/signal-policy/signals      — list valid signal type tokens
///   GET    /api/signal-policy/summary      — matrix of (scope → signal → enabled) for UI
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use lbug::{Connection, Value};
use serde::{Deserialize, Serialize};
use crate::signal_filter::SIGNAL_TYPES;

use super::AppState;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

fn read_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

fn read_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        _ => true,
    }
}

fn read_i64(v: &Value) -> i64 {
    match v {
        Value::Int64(n) => *n,
        _ => 0,
    }
}

// ── Request / response types ──────────────────────────────────────────────────

/// Valid scope types.
const SCOPE_TYPES: &[&str] = &["device", "site", "role"];

#[derive(Deserialize)]
pub(super) struct UpsertPolicyRequest {
    /// "device" | "site" | "role"
    pub scope_type: String,
    /// Address, site label, or role label
    pub scope_value: String,
    /// One of SIGNAL_TYPES tokens
    pub signal_type: String,
    pub enabled: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default = "default_updated_by")]
    pub updated_by: String,
}

fn default_updated_by() -> String {
    "api".to_string()
}

#[derive(Serialize)]
pub(super) struct PolicyEntry {
    pub id: String,
    pub scope_type: String,
    pub scope_value: String,
    pub signal_type: String,
    pub enabled: bool,
    pub reason: String,
    pub updated_by: String,
    pub updated_at_ns: i64,
}

#[derive(Serialize)]
pub(super) struct PolicyListResponse {
    pub entries: Vec<PolicyEntry>,
}

#[derive(Serialize)]
pub(super) struct SignalTypesResponse {
    pub signal_types: Vec<&'static str>,
    pub scope_types: Vec<&'static str>,
}

/// Compact matrix for the UI: scope_type → scope_value → signal_type → enabled.
#[derive(Serialize)]
pub(super) struct SummaryResponse {
    /// All distinct scopes that have at least one policy.
    pub scopes: Vec<ScopeRow>,
}

#[derive(Serialize)]
pub(super) struct ScopeRow {
    pub scope_type: String,
    pub scope_value: String,
    /// Map from signal_type → enabled. Missing = default (true = allow).
    pub signals: std::collections::HashMap<String, bool>,
}

// ── GET /api/signal-policy ────────────────────────────────────────────────────

pub(super) async fn list_signal_policies_handler(
    State(state): State<AppState>,
) -> Result<Json<PolicyListResponse>, StatusCode> {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut stmt = conn
            .prepare(
                "MATCH (p:SignalPolicy) \
                 RETURN p.id, p.scope_type, p.scope_value, p.signal_type, \
                        p.enabled, p.reason, p.updated_by, p.updated_at_ns \
                 ORDER BY p.scope_type, p.scope_value, p.signal_type",
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut rows = conn
            .execute(&mut stmt, vec![])
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next() {
            entries.push(PolicyEntry {
                id:            read_str(&row[0]),
                scope_type:    read_str(&row[1]),
                scope_value:   read_str(&row[2]),
                signal_type:   read_str(&row[3]),
                enabled:       read_bool(&row[4]),
                reason:        read_str(&row[5]),
                updated_by:    read_str(&row[6]),
                updated_at_ns: read_i64(&row[7]),
            });
        }
        Ok(Json(PolicyListResponse { entries }))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    result
}

// ── POST /api/signal-policy ───────────────────────────────────────────────────

pub(super) async fn upsert_signal_policy_handler(
    State(state): State<AppState>,
    Json(req): Json<UpsertPolicyRequest>,
) -> Result<StatusCode, StatusCode> {
    if !SCOPE_TYPES.contains(&req.scope_type.as_str()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !SIGNAL_TYPES.contains(&req.signal_type.as_str()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req.scope_value.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let now = now_ns();
    tokio::task::spawn_blocking(move || {
        let _guard = write_lock.lock().expect("write lock");
        let conn = Connection::new(&db).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let id = format!("{}:{}:{}", req.scope_type, req.scope_value, req.signal_type);
        let mut stmt = conn
            .prepare(
                "MERGE (p:SignalPolicy {id: $id}) \
                 ON CREATE SET p.scope_type = $stype, p.scope_value = $sval, \
                               p.signal_type = $sigtype, p.enabled = $en, \
                               p.reason = $reason, p.updated_by = $by, p.updated_at_ns = $now \
                 ON MATCH SET  p.enabled = $en, p.reason = $reason, \
                               p.updated_by = $by, p.updated_at_ns = $now",
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        conn.execute(
            &mut stmt,
            vec![
                ("id", Value::String(id)),
                ("stype", Value::String(req.scope_type)),
                ("sval", Value::String(req.scope_value)),
                ("sigtype", Value::String(req.signal_type)),
                ("en", Value::Bool(req.enabled)),
                ("reason", Value::String(req.reason)),
                ("by", Value::String(req.updated_by)),
                ("now", Value::Int64(now)),
            ],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(StatusCode::OK)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
}

// ── DELETE /api/signal-policy/{id} ───────────────────────────────────────────

pub(super) async fn delete_signal_policy_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    tokio::task::spawn_blocking(move || {
        let _guard = write_lock.lock().expect("write lock");
        let conn = Connection::new(&db).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut stmt = conn
            .prepare("MATCH (p:SignalPolicy {id: $id}) DELETE p")
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        conn.execute(&mut stmt, vec![("id", Value::String(id))])
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
}

// ── GET /api/signal-policy/signals ───────────────────────────────────────────

pub(super) async fn signal_types_handler() -> Json<SignalTypesResponse> {
    Json(SignalTypesResponse {
        signal_types: SIGNAL_TYPES.to_vec(),
        scope_types: SCOPE_TYPES.to_vec(),
    })
}

// ── GET /api/signal-policy/summary ───────────────────────────────────────────

pub(super) async fn signal_policy_summary_handler(
    State(state): State<AppState>,
) -> Result<Json<SummaryResponse>, StatusCode> {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut stmt = conn
            .prepare(
                "MATCH (p:SignalPolicy) \
                 RETURN p.scope_type, p.scope_value, p.signal_type, p.enabled \
                 ORDER BY p.scope_type, p.scope_value, p.signal_type",
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut rows = conn
            .execute(&mut stmt, vec![])
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let mut scope_map: std::collections::HashMap<(String, String), std::collections::HashMap<String, bool>> =
            std::collections::HashMap::new();

        while let Some(row) = rows.next() {
            let scope_type  = read_str(&row[0]);
            let scope_value = read_str(&row[1]);
            let signal_type = read_str(&row[2]);
            let enabled     = read_bool(&row[3]);
            scope_map
                .entry((scope_type, scope_value))
                .or_default()
                .insert(signal_type, enabled);
        }

        let mut scopes: Vec<ScopeRow> = scope_map
            .into_iter()
            .map(|((scope_type, scope_value), signals)| ScopeRow {
                scope_type,
                scope_value,
                signals,
            })
            .collect();
        scopes.sort_by(|a, b| {
            a.scope_type
                .cmp(&b.scope_type)
                .then(a.scope_value.cmp(&b.scope_value))
        });

        Ok(Json(SummaryResponse { scopes }))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    result
}
