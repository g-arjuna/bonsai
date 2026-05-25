/// DS-4: DDoS HTTP API handlers
///
/// Endpoints:
///   GET    /api/ddos/scope              — list all devices in DDoS scope
///   POST   /api/ddos/scope              — add device to DDoS scope
///   DELETE /api/ddos/scope/{address}    — remove device from DDoS scope
///   PATCH  /api/ddos/scope/{address}    — enable/disable without removing
///   GET    /api/ddos/config             — read current DdosConfig
///   GET    /api/ddos/events             — list DdosEvent nodes (most recent first)
///   GET    /api/ddos/baselines          — list TrafficBaseline nodes
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use lbug::{Connection, Value};
use serde::{Deserialize, Serialize};

use crate::config::DdosConfig;

use super::AppState;

// ── Request / response types ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct AddScopeRequest {
    pub device_address: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub added_by: String,
}

fn default_true() -> bool { true }

#[derive(Deserialize)]
pub(super) struct PatchScopeRequest {
    pub enabled: bool,
    #[serde(default)]
    pub reason: String,
}

#[derive(Serialize)]
pub(super) struct DdosScopeEntry {
    pub device_address: String,
    pub enabled: bool,
    pub reason: String,
    pub added_by: String,
    pub added_at_ns: i64,
    pub updated_at_ns: i64,
}

#[derive(Serialize)]
pub(super) struct DdosScopeListResponse {
    pub entries: Vec<DdosScopeEntry>,
}

#[derive(Serialize)]
pub(super) struct DdosEventRow {
    pub id: String,
    pub state: String,
    pub primary_vector: String,
    pub confidence: f64,
    pub max_observed_pps: f64,
    pub started_at_ns: i64,
    pub updated_at_ns: i64,
}

#[derive(Serialize)]
pub(super) struct DdosEventsResponse {
    pub events: Vec<DdosEventRow>,
}

#[derive(Serialize)]
pub(super) struct BaselineRow {
    pub id: String,
    pub device_address: String,
    pub protocol: String,
    pub p50_pps: f64,
    pub p95_pps: f64,
    pub p99_pps: f64,
    pub sample_count: i64,
    pub last_updated_ns: i64,
}

#[derive(Serialize)]
pub(super) struct BaselinesResponse {
    pub baselines: Vec<BaselineRow>,
}

// ── Helper ────────────────────────────────────────────────────────────────────

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn read_f64(v: &Value) -> f64 {
    match v { Value::Double(x) => *x, Value::Int64(x) => *x as f64, _ => 0.0 }
}

fn read_i64(v: &Value) -> i64 {
    match v { Value::Int64(x) => *x, Value::Double(x) => *x as i64, _ => 0 }
}

fn read_str(v: &Value) -> String {
    match v { Value::String(s) => s.clone(), _ => String::new() }
}

fn read_bool(v: &Value) -> bool {
    match v { Value::Bool(b) => *b, _ => false }
}

// ── Scope: GET /api/ddos/scope ────────────────────────────────────────────────

pub(super) async fn ddos_scope_list_handler(
    State(state): State<AppState>,
) -> Result<Json<DdosScopeListResponse>, StatusCode> {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut stmt = conn
            .prepare(
                "MATCH (ds:DdosScope) \
                 RETURN ds.device_address, ds.enabled, ds.reason, ds.added_by, \
                        ds.added_at_ns, ds.updated_at_ns \
                 ORDER BY ds.device_address",
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut rows = conn
            .execute(&mut stmt, vec![])
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let mut entries = Vec::new();
        while let Some(row) = rows.next() {
            entries.push(DdosScopeEntry {
                device_address: read_str(&row[0]),
                enabled:        read_bool(&row[1]),
                reason:         read_str(&row[2]),
                added_by:       read_str(&row[3]),
                added_at_ns:    read_i64(&row[4]),
                updated_at_ns:  read_i64(&row[5]),
            });
        }
        Ok(Json(DdosScopeListResponse { entries }))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    result
}

// ── Scope: POST /api/ddos/scope ───────────────────────────────────────────────

pub(super) async fn ddos_scope_add_handler(
    State(state): State<AppState>,
    Json(req): Json<AddScopeRequest>,
) -> Result<StatusCode, StatusCode> {
    let db = state.store.db();
    let now = now_ns();
    let write_lock = state.store.write_lock();
    tokio::task::spawn_blocking(move || {
        let _guard = write_lock.lock().expect("write lock");
        let conn = Connection::new(&db).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut stmt = conn
            .prepare(
                "MERGE (ds:DdosScope {device_address: $addr}) \
                 ON CREATE SET ds.enabled = $en, ds.reason = $reason, \
                               ds.added_by = $by, ds.added_at_ns = $now, ds.updated_at_ns = $now \
                 ON MATCH SET  ds.enabled = $en, ds.reason = $reason, ds.updated_at_ns = $now",
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        conn.execute(
            &mut stmt,
            vec![
                ("addr",   Value::String(req.device_address.clone())),
                ("en",     Value::Bool(req.enabled)),
                ("reason", Value::String(req.reason.clone())),
                ("by",     Value::String(req.added_by.clone())),
                ("now",    Value::Int64(now)),
            ],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(StatusCode::CREATED)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
}

// ── Scope: DELETE /api/ddos/scope/:address ────────────────────────────────────

pub(super) async fn ddos_scope_remove_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    tokio::task::spawn_blocking(move || {
        let _guard = write_lock.lock().expect("write lock");
        let conn = Connection::new(&db).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut stmt = conn
            .prepare("MATCH (ds:DdosScope {device_address: $addr}) DELETE ds")
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        conn.execute(&mut stmt, vec![("addr", Value::String(address))])
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
}

// ── Scope: PATCH /api/ddos/scope/:address ─────────────────────────────────────

pub(super) async fn ddos_scope_patch_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Json(req): Json<PatchScopeRequest>,
) -> Result<StatusCode, StatusCode> {
    let db = state.store.db();
    let now = now_ns();
    let write_lock = state.store.write_lock();
    tokio::task::spawn_blocking(move || {
        let _guard = write_lock.lock().expect("write lock");
        let conn = Connection::new(&db).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut stmt = conn
            .prepare(
                "MATCH (ds:DdosScope {device_address: $addr}) \
                 SET ds.enabled = $en, ds.reason = $reason, ds.updated_at_ns = $now",
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        conn.execute(
            &mut stmt,
            vec![
                ("addr",   Value::String(address)),
                ("en",     Value::Bool(req.enabled)),
                ("reason", Value::String(req.reason)),
                ("now",    Value::Int64(now)),
            ],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
}

// ── Config: GET /api/ddos/config ─────────────────────────────────────────────

pub(super) async fn ddos_config_handler(
    State(state): State<AppState>,
) -> Json<DdosConfig> {
    Json((*state.store.ddos_config).clone())
}

// ── Events: GET /api/ddos/events ─────────────────────────────────────────────

pub(super) async fn ddos_events_handler(
    State(state): State<AppState>,
) -> Result<Json<DdosEventsResponse>, StatusCode> {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut stmt = conn
            .prepare(
                "MATCH (e:DdosEvent) \
                 RETURN e.id, e.state, e.primary_vector, e.confidence, \
                        e.max_observed_pps, e.started_at_ns, e.updated_at_ns \
                 ORDER BY e.updated_at_ns DESC LIMIT 100",
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut rows = conn
            .execute(&mut stmt, vec![])
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let mut events = Vec::new();
        while let Some(row) = rows.next() {
            events.push(DdosEventRow {
                id:               read_str(&row[0]),
                state:            read_str(&row[1]),
                primary_vector:   read_str(&row[2]),
                confidence:       read_f64(&row[3]),
                max_observed_pps: read_f64(&row[4]),
                started_at_ns:    read_i64(&row[5]),
                updated_at_ns:    read_i64(&row[6]),
            });
        }
        Ok(Json(DdosEventsResponse { events }))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    result
}

// ── Baselines: GET /api/ddos/baselines ───────────────────────────────────────

pub(super) async fn ddos_baselines_handler(
    State(state): State<AppState>,
) -> Result<Json<BaselinesResponse>, StatusCode> {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut stmt = conn
            .prepare(
                "MATCH (b:TrafficBaseline) \
                 RETURN b.id, b.device_address, b.protocol, b.p50_pps, b.p95_pps, \
                        b.p99_pps, b.sample_count, b.last_updated_ns \
                 ORDER BY b.device_address, b.protocol",
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut rows = conn
            .execute(&mut stmt, vec![])
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let mut baselines = Vec::new();
        while let Some(row) = rows.next() {
            baselines.push(BaselineRow {
                id:              read_str(&row[0]),
                device_address:  read_str(&row[1]),
                protocol:        read_str(&row[2]),
                p50_pps:         read_f64(&row[3]),
                p95_pps:         read_f64(&row[4]),
                p99_pps:         read_f64(&row[5]),
                sample_count:    read_i64(&row[6]),
                last_updated_ns: read_i64(&row[7]),
            });
        }
        Ok(Json(BaselinesResponse { baselines }))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    result
}
