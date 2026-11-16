//! EV1 ML pipeline HTTP handlers.
//!
//! Covers:
//! - EV1-2: Parquet catalog  (GET/POST /api/ml/exports, GET /api/ml/models, etc.)
//! - EV1-4: GNN persistence  (POST /api/gnn/inference-results, /api/gnn/attention)
//! - EV1-3: Embeddings       (GET/POST /api/events/unembedded, /api/events/embeddings,
//!                             GET/POST /api/devices/unembedded-config, /api/devices/{addr}/config-embedding)
//! - EV1-4 T1: SSE event bus (GET /api/ml/events/stream, POST /api/ml/events/publish)
//! - EV1-5: Job runs/schedules (GET/POST/PATCH /api/ml/jobs, /api/ml/schedules)

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use lbug::{Connection, Value};
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;
use tracing::{info, warn};
use uuid::Uuid;

use crate::ml_event_bus::MlEvent;

use super::AppState;

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

// ── SSE event bus ─────────────────────────────────────────────────────────────

/// GET /api/ml/events/stream — SSE stream of MlEvent messages.
pub async fn ml_events_stream_handler(
    State(state): State<AppState>,
) -> Sse<impl futures::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = state.ml_event_bus.subscribe();
    let stream = BroadcastStream::new(rx).map(|result| match result {
        Ok(event) => {
            let data = serde_json::to_string(&event).unwrap_or_default();
            Ok(Event::default().data(data))
        }
        Err(_) => Ok(Event::default().comment("lag")),
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// POST /api/ml/events/publish — Python sidecar publishes an MlEvent.
pub async fn ml_events_publish_handler(
    State(state): State<AppState>,
    Json(event): Json<MlEvent>,
) -> impl IntoResponse {
    state.ml_event_bus.publish(event);
    (StatusCode::OK, Json(serde_json::json!({"ok": true})))
}

// ── Parquet export catalog ────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ParquetExportRecord {
    pub id: String,
    pub export_type: String,
    pub output_path: String,
    pub started_at_ns: i64,
    pub completed_at_ns: i64,
    pub row_count: i64,
    pub anomaly_rows: i64,
    pub normal_rows: i64,
    pub since_ns: i64,
    pub until_ns: i64,
    pub schema_hash: String,
    pub status: String,
    pub error_message: String,
    pub quality_json: String,
}

fn parquet_from_row(r: &[Value]) -> ParquetExportRecord {
    let s = |v: &Value| match v {
        Value::String(s) => s.clone(),
        _ => String::new(),
    };
    let n = |v: &Value| match v {
        Value::Int64(n) => *n,
        _ => 0,
    };
    ParquetExportRecord {
        id: s(&r[0]),
        export_type: s(&r[1]),
        output_path: s(&r[2]),
        started_at_ns: n(&r[3]),
        completed_at_ns: n(&r[4]),
        row_count: n(&r[5]),
        anomaly_rows: n(&r[6]),
        normal_rows: n(&r[7]),
        since_ns: n(&r[8]),
        until_ns: n(&r[9]),
        schema_hash: s(&r[10]),
        status: s(&r[11]),
        error_message: s(&r[12]),
        quality_json: s(&r[13]),
    }
}

/// GET /api/ml/exports
pub async fn list_ml_exports_handler(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let rows = conn
            .query(
                "MATCH (e:ParquetExport) \
                 RETURN e.id, e.export_type, e.output_path, e.started_at_ns, \
                        e.completed_at_ns, e.row_count, e.anomaly_rows, e.normal_rows, \
                        e.since_ns, e.until_ns, e.schema_hash, e.status, \
                        e.error_message, e.quality_json \
                 ORDER BY e.started_at_ns DESC LIMIT 200",
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .map(|r| parquet_from_row(&r))
            .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>(rows)
    })
    .await;

    match result {
        Ok(Ok(rows)) => {
            (StatusCode::OK, Json(serde_json::json!({"exports": rows}))).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateExportRequest {
    pub export_type: String,
    pub since_ns: Option<i64>,
    pub until_ns: Option<i64>,
    pub model_version_trigger: Option<String>,
}

/// POST /api/ml/exports
pub async fn create_ml_export_handler(
    State(state): State<AppState>,
    Json(req): Json<CreateExportRequest>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        let id = Uuid::new_v4().to_string();
        let now = now_ns();
        let mut stmt = conn.prepare(
            "CREATE (:ParquetExport {id: $id, export_type: $et, output_path: '', \
             started_at_ns: $now, completed_at_ns: 0, row_count: 0, anomaly_rows: 0, \
             normal_rows: 0, since_ns: $since, until_ns: $until, schema_hash: '', \
             status: 'running', error_message: '', model_version_trigger: $mvt, quality_json: '{}'})",
        )?;
        conn.execute(&mut stmt, vec![
            ("id", Value::String(id.clone())),
            ("et", Value::String(req.export_type)),
            ("now", Value::Int64(now)),
            ("since", Value::Int64(req.since_ns.unwrap_or(0))),
            ("until", Value::Int64(req.until_ns.unwrap_or(0))),
            ("mvt", Value::String(req.model_version_trigger.unwrap_or_default())),
        ])?;
        Ok::<_, anyhow::Error>(id)
    })
    .await;

    match result {
        Ok(Ok(id)) => (StatusCode::CREATED, Json(serde_json::json!({"id": id}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct PatchExportRequest {
    pub status: Option<String>,
    pub row_count: Option<i64>,
    pub anomaly_rows: Option<i64>,
    pub normal_rows: Option<i64>,
    pub schema_hash: Option<String>,
    pub output_path: Option<String>,
    pub error_message: Option<String>,
    pub quality_json: Option<String>,
    pub completed_at_ns: Option<i64>,
}

/// PATCH /api/ml/exports/:id
pub async fn patch_ml_export_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PatchExportRequest>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        if let Some(s) = &req.status {
            let mut stmt = conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.status = $v")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("v", Value::String(s.clone())),
                ],
            )?;
        }
        if let Some(v) = req.row_count {
            let mut stmt =
                conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.row_count = $v")?;
            conn.execute(
                &mut stmt,
                vec![("id", Value::String(id.clone())), ("v", Value::Int64(v))],
            )?;
        }
        if let Some(v) = req.anomaly_rows {
            let mut stmt =
                conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.anomaly_rows = $v")?;
            conn.execute(
                &mut stmt,
                vec![("id", Value::String(id.clone())), ("v", Value::Int64(v))],
            )?;
        }
        if let Some(v) = req.normal_rows {
            let mut stmt =
                conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.normal_rows = $v")?;
            conn.execute(
                &mut stmt,
                vec![("id", Value::String(id.clone())), ("v", Value::Int64(v))],
            )?;
        }
        if let Some(v) = &req.schema_hash {
            let mut stmt =
                conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.schema_hash = $v")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("v", Value::String(v.clone())),
                ],
            )?;
        }
        if let Some(v) = &req.output_path {
            let mut stmt =
                conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.output_path = $v")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("v", Value::String(v.clone())),
                ],
            )?;
        }
        if let Some(v) = &req.error_message {
            let mut stmt =
                conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.error_message = $v")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("v", Value::String(v.clone())),
                ],
            )?;
        }
        if let Some(v) = &req.quality_json {
            let mut stmt =
                conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.quality_json = $v")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("v", Value::String(v.clone())),
                ],
            )?;
        }
        if let Some(v) = req.completed_at_ns {
            let mut stmt =
                conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.completed_at_ns = $v")?;
            conn.execute(
                &mut stmt,
                vec![("id", Value::String(id.clone())), ("v", Value::Int64(v))],
            )?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── Model registry ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModelArtifactRecord {
    pub id: String,
    pub model_type: String,
    pub version: String,
    pub path: String,
    pub feature_schema_hash: String,
    pub trained_at_ns: i64,
    pub val_auc: f64,
    pub val_f1: f64,
    pub val_precision: f64,
    pub val_recall: f64,
    pub threshold: f64,
    pub is_active: bool,
    pub retired_at_ns: i64,
    pub model_card_path: String,
}

fn model_from_row(r: &[Value]) -> ModelArtifactRecord {
    let s = |v: &Value| match v {
        Value::String(s) => s.clone(),
        _ => String::new(),
    };
    let n = |v: &Value| match v {
        Value::Int64(n) => *n,
        _ => 0,
    };
    let f = |v: &Value| match v {
        Value::Double(f) => *f,
        Value::Float(f) => *f as f64,
        Value::Int64(n) => *n as f64,
        _ => 0.0,
    };
    ModelArtifactRecord {
        id: s(&r[0]),
        model_type: s(&r[1]),
        version: s(&r[2]),
        path: s(&r[3]),
        feature_schema_hash: s(&r[4]),
        trained_at_ns: n(&r[5]),
        val_auc: f(&r[6]),
        val_f1: f(&r[7]),
        val_precision: f(&r[8]),
        val_recall: f(&r[9]),
        threshold: f(&r[10]),
        is_active: n(&r[11]) != 0,
        retired_at_ns: n(&r[12]),
        model_card_path: s(&r[13]),
    }
}

/// GET /api/ml/models
pub async fn list_ml_models_handler(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let rows = conn
            .query(
                "MATCH (m:ModelArtifact) \
                 RETURN m.id, m.model_type, m.version, m.path, m.feature_schema_hash, \
                        m.trained_at_ns, m.val_auc, m.val_f1, m.val_precision, m.val_recall, \
                        m.threshold, m.is_active, m.retired_at_ns, m.model_card_path \
                 ORDER BY m.trained_at_ns DESC LIMIT 100",
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .map(|r| model_from_row(&r))
            .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>(rows)
    })
    .await;

    match result {
        Ok(Ok(rows)) => (StatusCode::OK, Json(serde_json::json!({"models": rows}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/ml/models/active?type=stgnn
#[derive(Debug, Deserialize)]
pub struct ActiveModelQuery {
    #[serde(rename = "type")]
    pub model_type: Option<String>,
}

pub async fn active_ml_model_handler(
    State(state): State<AppState>,
    Query(q): Query<ActiveModelQuery>,
) -> impl IntoResponse {
    let db = state.store.db();
    let model_type = q.model_type.clone().unwrap_or_default();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let cypher = if model_type.is_empty() {
            "MATCH (m:ModelArtifact) WHERE m.is_active = 1 \
             RETURN m.id, m.model_type, m.version, m.path, m.feature_schema_hash, \
                    m.trained_at_ns, m.val_auc, m.val_f1, m.val_precision, m.val_recall, \
                    m.threshold, m.is_active, m.retired_at_ns, m.model_card_path \
             LIMIT 1"
                .to_string()
        } else {
            format!(
                "MATCH (m:ModelArtifact) WHERE m.is_active = 1 AND m.model_type = '{}' \
                 RETURN m.id, m.model_type, m.version, m.path, m.feature_schema_hash, \
                        m.trained_at_ns, m.val_auc, m.val_f1, m.val_precision, m.val_recall, \
                        m.threshold, m.is_active, m.retired_at_ns, m.model_card_path \
                 LIMIT 1",
                model_type.replace('\'', "")
            )
        };
        let model = conn
            .query(&cypher)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .next()
            .map(|r| model_from_row(&r));
        Ok::<_, anyhow::Error>(model)
    })
    .await;

    match result {
        Ok(Ok(Some(m))) => (StatusCode::OK, Json(m)).into_response(),
        Ok(Ok(None)) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "no active model"})),
        )
            .into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpsertModelRequest {
    pub model_type: String,
    pub version: String,
    pub path: String,
    pub feature_schema_hash: String,
    pub trained_at_ns: Option<i64>,
    pub val_auc: Option<f64>,
    pub val_f1: Option<f64>,
    pub val_precision: Option<f64>,
    pub val_recall: Option<f64>,
    pub threshold: Option<f64>,
    pub model_card_path: Option<String>,
    pub input_parquet_id: Option<String>,
}

/// POST /api/ml/models — register a new model artifact
pub async fn create_ml_model_handler(
    State(state): State<AppState>,
    Json(req): Json<UpsertModelRequest>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let ml_bus = state.ml_event_bus.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        let id = Uuid::new_v4().to_string();
        let now = now_ns();
        let mut stmt = conn.prepare(
            "CREATE (:ModelArtifact {id: $id, model_type: $mt, version: $ver, path: $path, \
             feature_schema_hash: $fsh, trained_at_ns: $tan, val_auc: $auc, val_f1: $f1, \
             val_precision: $prec, val_recall: $rec, threshold: $thr, is_active: 0, \
             retired_at_ns: 0, model_card_path: $mcp})",
        )?;
        conn.execute(
            &mut stmt,
            vec![
                ("id", Value::String(id.clone())),
                ("mt", Value::String(req.model_type.clone())),
                ("ver", Value::String(req.version)),
                ("path", Value::String(req.path)),
                ("fsh", Value::String(req.feature_schema_hash)),
                ("tan", Value::Int64(req.trained_at_ns.unwrap_or(now))),
                ("auc", Value::Double(req.val_auc.unwrap_or(0.0))),
                ("f1", Value::Double(req.val_f1.unwrap_or(0.0))),
                ("prec", Value::Double(req.val_precision.unwrap_or(0.0))),
                ("rec", Value::Double(req.val_recall.unwrap_or(0.0))),
                ("thr", Value::Double(req.threshold.unwrap_or(0.5))),
                (
                    "mcp",
                    Value::String(req.model_card_path.unwrap_or_default()),
                ),
            ],
        )?;
        Ok::<_, anyhow::Error>((id, req.model_type))
    })
    .await;

    match result {
        Ok(Ok((id, model_type))) => {
            ml_bus.publish(MlEvent::ModelActivated {
                model_id: id.clone(),
                model_type,
                val_auc: 0.0,
            });
            (StatusCode::CREATED, Json(serde_json::json!({"id": id}))).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// POST /api/ml/models/:id/activate
pub async fn activate_ml_model_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let ml_bus = state.ml_event_bus.clone();
    let event_model_id = id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        // Fetch model info
        let mut info_stmt = conn.prepare("MATCH (m:ModelArtifact {id: $id}) RETURN m.model_type, m.val_auc")?;
        let info = conn.execute(&mut info_stmt, vec![("id", Value::String(id.clone()))])?
            .next();
        let (model_type, val_auc) = match info {
            Some(r) => {
                let mt = match &r[0] { Value::String(s) => s.clone(), _ => String::new() };
                let auc = match &r[1] { Value::Double(f) => *f, _ => 0.0 };
                (mt, auc)
            }
            None => return Err(anyhow::anyhow!("model {id} not found")),
        };
        // Deactivate all models of same type
        let mut deact_stmt = conn.prepare(
            "MATCH (m:ModelArtifact) WHERE m.model_type = $mt AND m.is_active = 1 SET m.is_active = 0, m.retired_at_ns = $now",
        )?;
        conn.execute(&mut deact_stmt, vec![
            ("mt", Value::String(model_type.clone())),
            ("now", Value::Int64(now_ns())),
        ])?;
        // Activate this model
        let mut act_stmt = conn.prepare("MATCH (m:ModelArtifact {id: $id}) SET m.is_active = 1, m.retired_at_ns = 0")?;
        conn.execute(&mut act_stmt, vec![("id", Value::String(id.clone()))])?;
        Ok::<_, anyhow::Error>((model_type, val_auc))
    })
    .await;

    match result {
        Ok(Ok((model_type, val_auc))) => {
            ml_bus.publish(MlEvent::ModelActivated {
                model_id: event_model_id,
                model_type,
                val_auc,
            });
            (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
        }
        Ok(Err(e)) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── ML job runs ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MlJobRunRecord {
    pub id: String,
    pub job_type: String,
    pub started_at_ns: i64,
    pub completed_at_ns: i64,
    pub status: String,
    pub trigger: String,
    pub input_parquet_id: String,
    pub output_model_path: String,
    pub val_auc: f64,
    pub val_f1: f64,
    pub error_message: String,
    pub config_json: String,
    pub cancel_requested: bool,
}

fn job_from_row(r: &[Value]) -> MlJobRunRecord {
    let s = |v: &Value| match v {
        Value::String(s) => s.clone(),
        _ => String::new(),
    };
    let n = |v: &Value| match v {
        Value::Int64(n) => *n,
        _ => 0,
    };
    let f = |v: &Value| match v {
        Value::Double(f) => *f,
        Value::Float(f) => *f as f64,
        _ => 0.0,
    };
    MlJobRunRecord {
        id: s(&r[0]),
        job_type: s(&r[1]),
        started_at_ns: n(&r[2]),
        completed_at_ns: n(&r[3]),
        status: s(&r[4]),
        trigger: s(&r[5]),
        input_parquet_id: s(&r[6]),
        output_model_path: s(&r[7]),
        val_auc: f(&r[8]),
        val_f1: f(&r[9]),
        error_message: s(&r[10]),
        config_json: s(&r[11]),
        cancel_requested: n(&r[12]) != 0,
    }
}

/// GET /api/ml/jobs
pub async fn list_ml_jobs_handler(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let rows = conn
            .query(
                "MATCH (j:MlJobRun) \
             RETURN j.id, j.job_type, j.started_at_ns, j.completed_at_ns, j.status, \
                    j.trigger, j.input_parquet_id, j.output_model_path, \
                    j.val_auc, j.val_f1, j.error_message, j.config_json, j.cancel_requested \
             ORDER BY j.started_at_ns DESC LIMIT 500",
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .map(|r| job_from_row(&r))
            .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>(rows)
    })
    .await;

    match result {
        Ok(Ok(rows)) => (StatusCode::OK, Json(serde_json::json!({"jobs": rows}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateJobRequest {
    /// Primary field name used by the Rust API.
    #[serde(default)]
    pub job_type: String,
    /// Legacy alias: some callers (guide, older Python) send `job_id` instead of `job_type`.
    #[serde(default)]
    pub job_id: String,
    pub trigger: String,
    pub input_parquet_id: Option<String>,
    pub config_json: Option<String>,
}
impl CreateJobRequest {
    /// Return whichever of job_type / job_id is non-empty, preferring job_type.
    pub fn effective_job_type(&self) -> &str {
        if !self.job_type.is_empty() {
            &self.job_type
        } else {
            &self.job_id
        }
    }
}

/// POST /api/ml/jobs
pub async fn create_ml_job_handler(
    State(state): State<AppState>,
    Json(req): Json<CreateJobRequest>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let ml_bus = state.ml_event_bus.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        let id = Uuid::new_v4().to_string();
        let now = now_ns();
        let effective_type = req.effective_job_type().to_string();
        if effective_type.is_empty() {
            return Err(anyhow::anyhow!("job_type (or job_id) is required"));
        }
        let mut stmt = conn.prepare(
            "CREATE (:MlJobRun {id: $id, job_type: $jt, started_at_ns: $now, \
             completed_at_ns: 0, status: 'running', trigger: $trig, \
             input_parquet_id: $ipid, output_model_path: '', val_auc: 0.0, val_f1: 0.0, \
             error_message: '', config_json: $cfg, cancel_requested: 0})",
        )?;
        conn.execute(
            &mut stmt,
            vec![
                ("id", Value::String(id.clone())),
                ("jt", Value::String(effective_type.clone())),
                ("now", Value::Int64(now)),
                ("trig", Value::String(req.trigger)),
                (
                    "ipid",
                    Value::String(req.input_parquet_id.unwrap_or_default()),
                ),
                (
                    "cfg",
                    Value::String(req.config_json.unwrap_or_else(|| "{}".to_string())),
                ),
            ],
        )?;
        Ok::<_, anyhow::Error>((id, effective_type))
    })
    .await;

    match result {
        Ok(Ok((id, job_type))) => {
            ml_bus.publish(MlEvent::JobStarted {
                job_id: id.clone(),
                job_type,
                triggered_by: "api".to_string(),
            });
            (StatusCode::CREATED, Json(serde_json::json!({"id": id}))).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct PatchJobRequest {
    pub status: Option<String>,
    pub completed_at_ns: Option<i64>,
    pub val_auc: Option<f64>,
    pub val_f1: Option<f64>,
    pub output_model_path: Option<String>,
    pub error_message: Option<String>,
    pub cancel_requested: Option<bool>,
}

/// PATCH /api/ml/jobs/:id
pub async fn patch_ml_job_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PatchJobRequest>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let ml_bus = state.ml_event_bus.clone();
    let event_job_id = id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        // Fetch job type for events
        let mut info_stmt = conn.prepare("MATCH (j:MlJobRun {id: $id}) RETURN j.job_type")?;
        let job_type = conn
            .execute(&mut info_stmt, vec![("id", Value::String(id.clone()))])?
            .next()
            .map(|r| match &r[0] {
                Value::String(s) => s.clone(),
                _ => String::new(),
            })
            .unwrap_or_default();

        macro_rules! set_field {
            ($field:literal, $val:expr) => {{
                let mut stmt = conn.prepare(concat!(
                    "MATCH (j:MlJobRun {id: $id}) SET j.",
                    $field,
                    " = $v"
                ))?;
                conn.execute(
                    &mut stmt,
                    vec![("id", Value::String(id.clone())), ("v", $val)],
                )?;
            }};
        }
        if let Some(v) = &req.status {
            set_field!("status", Value::String(v.clone()));
        }
        if let Some(v) = req.completed_at_ns {
            set_field!("completed_at_ns", Value::Int64(v));
        }
        if let Some(v) = req.val_auc {
            set_field!("val_auc", Value::Double(v));
        }
        if let Some(v) = req.val_f1 {
            set_field!("val_f1", Value::Double(v));
        }
        if let Some(v) = &req.output_model_path {
            set_field!("output_model_path", Value::String(v.clone()));
        }
        if let Some(v) = &req.error_message {
            set_field!("error_message", Value::String(v.clone()));
        }
        if let Some(v) = req.cancel_requested {
            set_field!("cancel_requested", Value::Int64(if v { 1 } else { 0 }));
        }
        Ok::<_, anyhow::Error>((job_type, req.status.unwrap_or_default()))
    })
    .await;

    match result {
        Ok(Ok((job_type, status))) => {
            match status.as_str() {
                "succeeded" => ml_bus.publish(MlEvent::JobCompleted {
                    job_id: event_job_id.clone(),
                    job_type,
                    outcome: "succeeded".to_string(),
                    val_auc: 0.0,
                    val_f1: 0.0,
                    model_path: String::new(),
                }),
                "failed" => ml_bus.publish(MlEvent::JobFailed {
                    job_id: event_job_id,
                    job_type,
                    error: "see error_message".to_string(),
                }),
                _ => {}
            }
            (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// POST /api/ml/jobs/:id/cancel
pub async fn cancel_ml_job_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        let mut stmt = conn.prepare("MATCH (j:MlJobRun {id: $id}) SET j.cancel_requested = 1")?;
        conn.execute(&mut stmt, vec![("id", Value::String(id))])?;
        Ok::<_, anyhow::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// POST /api/ml/jobs/:id/retry — EV1-5 T6 dead-letter retry
pub async fn retry_ml_job_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let ml_bus = state.ml_event_bus.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        let now = now_ns();
        let mut check = conn.prepare("MATCH (j:MlJobRun {id: $id}) RETURN j.job_type")?;
        let row = conn
            .execute(&mut check, vec![("id", Value::String(id.clone()))])?
            .next();
        let job_type = match row {
            Some(r) => match &r[0] {
                Value::String(s) => s.clone(),
                _ => String::new(),
            },
            None => return Err(anyhow::anyhow!("job run {id} not found")),
        };
        let mut stmt = conn.prepare(
            "MATCH (j:MlJobRun {id: $id}) SET j.status = 'pending', j.completed_at_ns = 0, \
             j.error_message = '', j.started_at_ns = $now",
        )?;
        conn.execute(
            &mut stmt,
            vec![
                ("id", Value::String(id.clone())),
                ("now", Value::Int64(now)),
            ],
        )?;
        Ok::<_, anyhow::Error>((id, job_type))
    })
    .await;

    match result {
        Ok(Ok((id, job_type))) => {
            let _ = ml_bus.publish(MlEvent::JobRetryRequested {
                run_id: id.clone(),
                job_type,
            });
            (
                StatusCode::OK,
                Json(serde_json::json!({"ok": true, "id": id})),
            )
                .into_response()
        }
        Ok(Err(e)) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/ml/jobs/dead-letter — list MlJobRun records in dead_letter status (EV1-5 T6)
pub async fn list_dead_letter_handler(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let rows = conn
            .query(
                "MATCH (j:MlJobRun) WHERE j.status = 'dead_letter' \
             RETURN j.id, j.job_type, j.started_at_ns, j.error_message, j.config_json \
             ORDER BY j.started_at_ns DESC LIMIT 100",
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .map(|r| {
                let s = |v: &Value| match v {
                    Value::String(s) => s.clone(),
                    _ => String::new(),
                };
                let n = |v: &Value| match v {
                    Value::Int64(n) => *n,
                    _ => 0,
                };
                serde_json::json!({
                    "id": s(&r[0]),
                    "job_type": s(&r[1]),
                    "failed_at_ns": n(&r[2]),
                    "error_message": s(&r[3]),
                    "config_json": s(&r[4]),
                })
            })
            .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>(rows)
    })
    .await;

    match result {
        Ok(Ok(rows)) => (
            StatusCode::OK,
            Json(serde_json::json!({"dead_letter": rows})),
        )
            .into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── ML job schedules ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MlJobScheduleRecord {
    pub id: String,
    pub job_id: String,
    pub cron_expr: String,
    pub enabled: bool,
    pub last_modified_by: String,
    pub last_modified_at_ns: i64,
}

fn schedule_from_row(r: &[Value]) -> MlJobScheduleRecord {
    let s = |v: &Value| match v {
        Value::String(s) => s.clone(),
        _ => String::new(),
    };
    let n = |v: &Value| match v {
        Value::Int64(n) => *n,
        _ => 0,
    };
    MlJobScheduleRecord {
        id: s(&r[0]),
        job_id: s(&r[1]),
        cron_expr: s(&r[2]),
        enabled: n(&r[3]) != 0,
        last_modified_by: s(&r[4]),
        last_modified_at_ns: n(&r[5]),
    }
}

/// GET /api/ml/schedules
pub async fn list_ml_schedules_handler(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let rows = conn.query(
            "MATCH (s:MlJobSchedule) \
             RETURN s.id, s.job_id, s.cron_expr, s.enabled, s.last_modified_by, s.last_modified_at_ns \
             ORDER BY s.job_id",
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .map(|r| schedule_from_row(&r))
        .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>(rows)
    })
    .await;

    match result {
        Ok(Ok(rows)) => {
            (StatusCode::OK, Json(serde_json::json!({"schedules": rows}))).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpsertScheduleRequest {
    pub job_id: String,
    pub cron_expr: String,
    pub enabled: bool,
}

/// POST /api/ml/schedules
pub async fn upsert_ml_schedule_handler(
    State(state): State<AppState>,
    Json(req): Json<UpsertScheduleRequest>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        let id = format!("sched-{}", req.job_id);
        let now = now_ns();
        // Try merge
        let mut check_stmt = conn.prepare("MATCH (s:MlJobSchedule {id: $id}) RETURN s.id")?;
        let exists = conn
            .execute(&mut check_stmt, vec![("id", Value::String(id.clone()))])?
            .next()
            .is_some();
        if exists {
            let mut stmt = conn.prepare(
                "MATCH (s:MlJobSchedule {id: $id}) SET s.cron_expr = $ce, s.enabled = $en, \
                 s.last_modified_by = 'api', s.last_modified_at_ns = $now",
            )?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("ce", Value::String(req.cron_expr)),
                    ("en", Value::Int64(if req.enabled { 1 } else { 0 })),
                    ("now", Value::Int64(now)),
                ],
            )?;
        } else {
            let mut stmt = conn.prepare(
                "CREATE (:MlJobSchedule {id: $id, job_id: $jid, cron_expr: $ce, enabled: $en, \
                 last_modified_by: 'api', last_modified_at_ns: $now})",
            )?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("jid", Value::String(req.job_id)),
                    ("ce", Value::String(req.cron_expr)),
                    ("en", Value::Int64(if req.enabled { 1 } else { 0 })),
                    ("now", Value::Int64(now)),
                ],
            )?;
        }
        Ok::<_, anyhow::Error>(id)
    })
    .await;

    match result {
        Ok(Ok(id)) => (StatusCode::OK, Json(serde_json::json!({"id": id}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// DELETE /api/ml/schedules/:id
pub async fn delete_ml_schedule_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        let mut stmt = conn.prepare("MATCH (s:MlJobSchedule {id: $id}) DELETE s")?;
        conn.execute(&mut stmt, vec![("id", Value::String(id))])?;
        Ok::<_, anyhow::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── EV1-2 T4: Default schedule seeding + scheduler tick loop ──────────────────

/// Default `MlJobSchedule` records seeded at startup if not already present.
/// Each entry: (id, job_id, cron_expr, enabled).
/// cron_expr is a simplified "H H * * *" / "H H * * 0" string — the tick
/// loop interprets only the hour field for daily/weekly/N-hourly schedules.
const DEFAULT_SCHEDULES: &[(&str, &str, &str)] = &[
    ("sched-anomaly_export", "anomaly_export", "0 2 * * *"), // daily 02:00 UTC
    (
        "sched-remediation_export",
        "remediation_export",
        "0 2 * * 0",
    ), // weekly Sun 02:00 UTC
    ("sched-gnn_snapshot", "gnn_snapshot", "0 */4 * * *"),   // every 4h
    // EV1 ML pipeline schedules — expected by Python sidecar + EV1 guide
    ("sched-graph_snapshot", "graph_snapshot", "0 */1 * * *"), // every 1h
    ("sched-gnn_inference", "gnn_inference", "0 */4 * * *"),   // every 4h
    ("sched-syslog_embedding", "syslog_embedding", "0 */1 * * *"), // every 1h from Rust tick; Python APScheduler runs it every 60s
    ("sched-config_embedding", "config_embedding", "0 */6 * * *"), // every 6h
];

/// Seed the three default export schedules (idempotent — skips existing rows).
/// Also cleans up stale `running` MlJobRun records left from a previous crashed session.
pub fn seed_default_ml_schedules(db: Arc<lbug::Database>, write_lock: Arc<std::sync::Mutex<()>>) {
    let db = db.clone();
    let wl = write_lock.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let _g = wl.lock().expect("write lock poisoned");
        let conn = match Connection::new(&db) {
            Ok(c) => c,
            Err(e) => {
                warn!("seed_default_ml_schedules: cannot open connection: {e}");
                return;
            }
        };
        let now = now_ns();

        // Clean up stale `running` or `pending` jobs from a previous server session.
        // These will never be patched to completed because the Python worker that owned
        // them is gone. Mark them `interrupted` so the UI doesn't show perpetual spinners.
        let cleanup = conn.prepare(
            "MATCH (j:MlJobRun) WHERE j.status = 'running' OR j.status = 'pending' \
             SET j.status = 'interrupted', j.completed_at_ns = $now, \
                 j.error_message = 'server restarted before job completed'",
        );
        match cleanup {
            Ok(mut stmt) => match conn.execute(&mut stmt, vec![("now", Value::Int64(now))]) {
                Ok(_) => info!("seed_default_ml_schedules: cleaned up stale running/pending jobs"),
                Err(e) => warn!("seed_default_ml_schedules: stale-job cleanup failed: {e}"),
            },
            Err(e) => warn!("seed_default_ml_schedules: could not prepare stale-job cleanup: {e}"),
        }
        for (id, job_id, cron_expr) in DEFAULT_SCHEDULES {
            let mut check = match conn.prepare("MATCH (s:MlJobSchedule {id: $id}) RETURN s.id") {
                Ok(s) => s,
                Err(_) => continue,
            };
            let exists = conn
                .execute(&mut check, vec![("id", Value::String(id.to_string()))])
                .map(|mut r| r.next().is_some())
                .unwrap_or(false);
            if exists {
                continue;
            }
            let mut stmt = match conn.prepare(
                "CREATE (:MlJobSchedule {id: $id, job_id: $jid, cron_expr: $ce, enabled: 1, \
                 last_modified_by: 'system', last_modified_at_ns: $now})",
            ) {
                Ok(s) => s,
                Err(e) => {
                    warn!("seed_default_ml_schedules: prepare failed: {e}");
                    continue;
                }
            };
            if let Err(e) = conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.to_string())),
                    ("jid", Value::String(job_id.to_string())),
                    ("ce", Value::String(cron_expr.to_string())),
                    ("now", Value::Int64(now)),
                ],
            ) {
                warn!("seed_default_ml_schedules: insert {id} failed: {e}");
            } else {
                info!("seeded default ML schedule: id={id} cron={cron_expr}");
            }
        }
    });
}

/// Minimal cron-window check for the hourly tick.
/// Returns true if the schedule's next fire time has been reached since `last_run_ns`.
/// Supports: "H H * * *" (daily at H UTC), "H H * * W" (weekly), "H */N * * *" (every N hours).
fn cron_should_fire(cron_expr: &str, last_run_ns: i64, now_ns: i64) -> bool {
    let now_secs = now_ns / 1_000_000_000;
    let last_secs = last_run_ns / 1_000_000_000;
    let fields: Vec<&str> = cron_expr.split_whitespace().collect();
    if fields.len() < 5 {
        return false;
    }
    let hour_field = fields[1];
    // Every N hours: "*/N"
    if let Some(n_str) = hour_field.strip_prefix("*/") {
        if let Ok(n) = n_str.parse::<i64>() {
            let interval_secs = n * 3600;
            return now_secs - last_secs >= interval_secs;
        }
    }
    // Daily or weekly: specific hour
    if let Ok(target_hour) = hour_field.parse::<i64>() {
        let interval_secs = if fields[4] == "*" { 86400 } else { 86400 * 7 };
        let now_hour = (now_secs % 86400) / 3600;
        let elapsed = now_secs - last_secs;
        return elapsed >= interval_secs && now_hour == target_hour;
    }
    false
}

/// Background scheduler tick loop. Runs hourly; fires enabled ML schedules
/// whose cron window has elapsed by creating a `MlJobRun` record (status=pending)
/// and publishing an `MlScheduledJobFired` event on the ML event bus.
/// The Python export_job.py worker picks up `pending` jobs and executes them.
pub async fn run_ml_schedule_tick(
    db: std::sync::Arc<lbug::Database>,
    write_lock: std::sync::Arc<std::sync::Mutex<()>>,
    ml_bus: std::sync::Arc<crate::ml_event_bus::MlEventBus>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
    interval.tick().await; // consume immediate tick
    loop {
        tokio::select! {
            biased;
            _ = shutdown.changed() => {
                info!("ml_schedule_tick: shutdown received");
                return;
            }
            _ = interval.tick() => {}
        }
        let db2 = db.clone();
        let wl = write_lock.clone();
        let bus = ml_bus.clone();
        let now = now_ns();
        let _ = tokio::task::spawn_blocking(move || {
            let conn = match Connection::new(&db2) {
                Ok(c) => c,
                Err(e) => { warn!("ml_schedule_tick: open conn: {e}"); return; }
            };
            let rows: Vec<MlJobScheduleRecord> = conn.query(
                "MATCH (s:MlJobSchedule) WHERE s.enabled = 1 \
                 RETURN s.id, s.job_id, s.cron_expr, s.enabled, s.last_modified_by, s.last_modified_at_ns",
            )
            .map(|r| r.map(|row| schedule_from_row(&row)).collect())
            .unwrap_or_default();

            for sched in rows {
                // Get last run time for this schedule from MlJobRun records
                let last_run_ns: i64 = conn.query(&format!(
                    "MATCH (j:MlJobRun {{job_type: '{}'}}) \
                     RETURN j.started_at_ns ORDER BY j.started_at_ns DESC LIMIT 1",
                    sched.job_id
                ))
                .ok()
                .and_then(|mut r| r.next())
                .and_then(|row| if let Value::Int64(n) = &row[0] { Some(*n) } else { None })
                .unwrap_or(0);

                if !cron_should_fire(&sched.cron_expr, last_run_ns, now) {
                    continue;
                }

                // Create a pending MlJobRun
                let run_id = Uuid::new_v4().to_string();
                let _g = wl.lock().expect("write lock poisoned");
                let mut stmt = match conn.prepare(
                    "CREATE (:MlJobRun {id: $id, job_type: $jt, started_at_ns: $now, \
                     completed_at_ns: 0, status: 'pending', trigger: 'scheduler', \
                     input_parquet_id: '', output_model_path: '', \
                     val_auc: 0.0, val_f1: 0.0, error_message: '', config_json: '{}'})",
                ) {
                    Ok(s) => s,
                    Err(e) => { warn!("ml_schedule_tick: prepare MlJobRun: {e}"); continue; }
                };
                if let Err(e) = conn.execute(&mut stmt, vec![
                    ("id", Value::String(run_id.clone())),
                    ("jt", Value::String(sched.job_id.clone())),
                    ("now", Value::Int64(now)),
                ]) {
                    warn!("ml_schedule_tick: create MlJobRun for {}: {e}", sched.job_id);
                    continue;
                }

                info!("ml_schedule_tick: fired job_type={} run_id={run_id}", sched.job_id);
                let _ = bus.publish(crate::ml_event_bus::MlEvent::MlScheduledJobFired {
                    run_id: run_id.clone(),
                    job_type: sched.job_id.clone(),
                    schedule_id: sched.id.clone(),
                    fired_at_ns: now,
                });
            }
        }).await;
    }
}

// ── GNN inference results ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct GnnInferenceResultItem {
    pub device_address: String,
    pub anomaly_score: f64,
    pub uncertainty_margin: Option<f64>,
    pub threshold: f64,
    pub is_anomalous: bool,
    pub top_contributing_device_1: Option<String>,
    pub top_contributing_device_2: Option<String>,
    pub attention_weight_1: Option<f64>,
    pub attention_weight_2: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct GnnInferenceBatchRequest {
    pub snapshot_ns: i64,
    pub model_id: String,
    pub results: Vec<GnnInferenceResultItem>,
}

/// POST /api/gnn/inference-results
pub async fn gnn_inference_results_handler(
    State(state): State<AppState>,
    Json(req): Json<GnnInferenceBatchRequest>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let ml_bus = state.ml_event_bus.clone();
    let snapshot_ns = req.snapshot_ns;
    let model_id = req.model_id.clone();
    let event_model_id = model_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        let now = now_ns();
        let mut count = 0i64;
        let mut top_score = 0f64;
        let mut anomalous = 0i64;
        for item in &req.results {
            let id = Uuid::new_v4().to_string();
            let is_ano = if item.is_anomalous { 1i64 } else { 0 };
            let mut stmt = conn.prepare(
                "CREATE (:GnnInferenceResult {id: $id, snapshot_ns: $sns, model_id: $mid, \
                 device_address: $da, anomaly_score: $as_, uncertainty_margin: $um, \
                 threshold: $thr, is_anomalous: $ia, \
                 top_contributing_device_1: $tc1, top_contributing_device_2: $tc2, \
                 attention_weight_1: $aw1, attention_weight_2: $aw2, inferred_at_ns: $now})",
            )?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("sns", Value::Int64(snapshot_ns)),
                    ("mid", Value::String(model_id.clone())),
                    ("da", Value::String(item.device_address.clone())),
                    ("as_", Value::Double(item.anomaly_score)),
                    ("um", Value::Double(item.uncertainty_margin.unwrap_or(0.0))),
                    ("thr", Value::Double(item.threshold)),
                    ("ia", Value::Int64(is_ano)),
                    (
                        "tc1",
                        Value::String(item.top_contributing_device_1.clone().unwrap_or_default()),
                    ),
                    (
                        "tc2",
                        Value::String(item.top_contributing_device_2.clone().unwrap_or_default()),
                    ),
                    ("aw1", Value::Double(item.attention_weight_1.unwrap_or(0.0))),
                    ("aw2", Value::Double(item.attention_weight_2.unwrap_or(0.0))),
                    ("now", Value::Int64(now)),
                ],
            )?;
            // Link Device -> GnnInferenceResult
            let mut link_stmt = conn.prepare(
                "MATCH (d:Device {address: $da}), (r:GnnInferenceResult {id: $rid}) \
                 MERGE (d)-[:GNN_SCORED {inferred_at_ns: $now}]->(r)",
            )?;
            let _ = conn.execute(
                &mut link_stmt,
                vec![
                    ("da", Value::String(item.device_address.clone())),
                    ("rid", Value::String(id)),
                    ("now", Value::Int64(now)),
                ],
            );
            count += 1;
            if item.anomaly_score > top_score {
                top_score = item.anomaly_score;
            }
            if item.is_anomalous {
                anomalous += 1;
            }
        }
        Ok::<_, anyhow::Error>((count, anomalous, top_score))
    })
    .await;

    match result {
        Ok(Ok((count, anomalous, top_score))) => {
            ml_bus.publish(MlEvent::GnnInferenceCompleted {
                snapshot_ns,
                anomalous_device_count: anomalous,
                top_score,
                model_id: event_model_id,
            });
            (
                StatusCode::OK,
                Json(serde_json::json!({"written": count, "anomalous": anomalous})),
            )
                .into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct AttentionSnapshotItem {
    pub inference_result_id: String,
    pub source_device_address: String,
    pub neighbour_device_address: String,
    pub edge_type: String,
    pub attention_weight: f64,
    pub snapshot_ns: i64,
}

#[derive(Debug, Deserialize)]
pub struct GnnAttentionBatchRequest {
    pub snapshots: Vec<AttentionSnapshotItem>,
}

/// POST /api/gnn/attention
pub async fn gnn_attention_handler(
    State(state): State<AppState>,
    Json(req): Json<GnnAttentionBatchRequest>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        let mut count = 0i64;
        for item in &req.snapshots {
            let id = Uuid::new_v4().to_string();
            let mut stmt = conn.prepare(
                "CREATE (:GnnAttentionSnapshot {id: $id, inference_result_id: $irid, \
                 source_device_address: $src, neighbour_device_address: $nbr, \
                 edge_type: $et, attention_weight: $aw, snapshot_ns: $sns})",
            )?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("irid", Value::String(item.inference_result_id.clone())),
                    ("src", Value::String(item.source_device_address.clone())),
                    ("nbr", Value::String(item.neighbour_device_address.clone())),
                    ("et", Value::String(item.edge_type.clone())),
                    ("aw", Value::Double(item.attention_weight)),
                    ("sns", Value::Int64(item.snapshot_ns)),
                ],
            )?;
            // Link GnnInferenceResult -> GnnAttentionSnapshot
            let mut link_stmt = conn.prepare(
                "MATCH (r:GnnInferenceResult {id: $irid}), (a:GnnAttentionSnapshot {id: $aid}) \
                 MERGE (r)-[:HAS_ATTENTION]->(a)",
            )?;
            let _ = conn.execute(
                &mut link_stmt,
                vec![
                    ("irid", Value::String(item.inference_result_id.clone())),
                    ("aid", Value::String(id)),
                ],
            );
            count += 1;
        }
        Ok::<_, anyhow::Error>(count)
    })
    .await;

    match result {
        Ok(Ok(count)) => {
            (StatusCode::OK, Json(serde_json::json!({"written": count}))).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── Embedding lifecycle ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct UnembeddedQuery {
    #[serde(rename = "type")]
    pub event_type: Option<String>,
    pub limit: Option<i64>,
}

/// GET /api/events/unembedded
pub async fn events_unembedded_handler(
    State(state): State<AppState>,
    Query(q): Query<UnembeddedQuery>,
) -> impl IntoResponse {
    let db = state.store.db();
    let limit = q.limit.unwrap_or(200).min(1000);
    let event_type = q.event_type.clone().unwrap_or_default();
    let now_ns = embedding_now_ns();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        // WP65-14: eligible pending = needs_embedding AND not permanently
        // invalid AND retry deadline reached. Oldest-first with a stable
        // identity tie-breaker; deferred/backed-off events cannot starve
        // healthy newer events.
        let type_clause = if event_type.is_empty() {
            String::new()
        } else {
            format!(
                " AND e.event_type STARTS WITH '{}'",
                event_type.replace('\'', "")
            )
        };
        let cypher = format!(
            "MATCH (e:StateChangeEvent) WHERE e.needs_embedding = 1 \
             AND (e.embedding_invalid IS NULL OR e.embedding_invalid = 0) \
             AND (e.embedding_retry_after_ns IS NULL OR e.embedding_retry_after_ns <= $now){type_clause} \
             RETURN e.id, e.device_address, e.event_type, e.detail, e.occurred_at, \
             e.embedding_attempts, e.embedding_invalid \
             ORDER BY e.occurred_at ASC, e.id ASC LIMIT {limit}"
        );
        let mut stmt = conn.prepare(&cypher)?;
        let mut rows = conn
            .execute(&mut stmt, vec![("now", Value::Int64(now_ns))])
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .map(|r| {
                let s = |v: &Value| match v { Value::String(s) => s.clone(), _ => String::new() };
                let attempts = match &r[5] {
                    Value::Int64(n) => *n,
                    _ => 0,
                };
                let _invalid = &r[6];
                serde_json::json!({
                    "id": s(&r[0]),
                    "device_address": s(&r[1]),
                    "event_type": s(&r[2]),
                    "detail_json": s(&r[3]),
                    "claim_attempts": attempts,
                })
            })
            .collect::<Vec<_>>();
        // WP65-21 processing generation: this fetch ACQUIRES a unique
        // claim token per event — the newest fetch wins, so reports
        // from a superseded fetch are fenced out even when the attempt
        // count happens to match. The token is fencing metadata only;
        // it never changes eligibility and is overwritten by the next
        // fetch, so a crashed worker cannot wedge an event.
        let claim_token = format!(
            "{:x}-{:x}-{:x}",
            now_ns,
            std::process::id(),
            embedding_claim_seq()
        );
        if !rows.is_empty() {
            let mut claim = conn.prepare(
                "MATCH (e:StateChangeEvent {id: $eid}) \
                 SET e.embedding_claim_token = $tok",
            )?;
            for row in &rows {
                if let Some(id) = row.get("id").and_then(|v| v.as_str()) {
                    conn.execute(
                        &mut claim,
                        vec![
                            ("eid", Value::String(id.to_string())),
                            ("tok", Value::String(claim_token.clone())),
                        ],
                    )?;
                }
            }
            for row in rows.iter_mut() {
                row["claim_token"] = serde_json::json!(claim_token);
            }
        }
        Ok::<_, anyhow::Error>(rows)
    })
    .await;

    match result {
        Ok(Ok(rows)) => (StatusCode::OK, Json(serde_json::json!({"events": rows}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct EmbeddingItem {
    pub event_id: String,
    pub vector: Vec<f64>,
    pub model_name: String,
    /// Missing/empty revision is explicit UNKNOWN provenance — never
    /// fabricated into a known revision string.
    pub model_revision: Option<String>,
    /// Declared embedding dimension; when present it must equal
    /// `vector.len()` exactly.
    pub dim: Option<i64>,
    pub computed_at_ns: i64,
    /// Normalization contract that produced the embedded text
    /// (WP65-12). Missing/empty → explicit "unknown".
    pub normalization_version: Option<String>,
    /// Embedding record-schema identity. Missing/empty → "unknown".
    pub schema_hash: Option<String>,
    /// DEPRECATED: the authoritative event_type is read from the
    /// persisted StateChangeEvent; a client-supplied value is ignored
    /// and can never redefine the source event (WP65-11).
    pub event_type: Option<String>,
}

/// WP65-14: an event the worker resolved but could not embed this cycle
/// (template pending under another claim, provider backoff/failure).
/// Reported so the fetch path can apply a bounded retry deadline instead
/// of re-selecting the same stuck events ahead of healthy ones.
#[derive(Debug, Deserialize)]
pub struct EmbeddingDeferredItem {
    pub event_id: String,
    /// Optional client hint for the next eligible fetch (ns epoch).
    /// The server applies its own attempt-bounded backoff on top and
    /// clamps the hint to a bounded horizon — it can never schedule
    /// arbitrarily far into the future.
    pub retry_after_ns: Option<i64>,
    /// WP65-17 idempotency/processing-generation claim: the
    /// `embedding_attempts` value the worker observed when it fetched
    /// the event (returned as `claim_attempts` by the unembedded
    /// endpoint). When present, the report is applied only if the
    /// stored attempts still equal the claim — a duplicate delivery or
    /// a stale report cannot consume another attempt or alter state.
    pub claim_attempts: Option<i64>,
    /// WP65-21 processing generation: the `claim_token` returned by the
    /// unembedded fetch that acquired this event's processing claim.
    /// When present it is AUTHORITATIVE — the report applies only while
    /// the stored token still equals it, so a superseded fetch's stale
    /// reports are fenced out even when the attempt count matches.
    /// Absent → legacy `claim_attempts` compatibility behavior.
    pub claim_token: Option<String>,
    /// WP65-17 failure classification. "claim_contention" (template
    /// pending under another worker's claim) reschedules WITHOUT
    /// consuming an attempt; anything else (default) is a provider/
    /// processing failure that consumes one bounded attempt.
    pub kind: Option<String>,
}

/// WP65-14: an event that can never be enriched (e.g. no extractable
/// message text). Marked permanently ineligible — raw evidence is
/// retained, only the embedding obligation is closed.
#[derive(Debug, Deserialize)]
pub struct EmbeddingInvalidItem {
    pub event_id: String,
    pub reason: String,
    /// Same claim semantics as `EmbeddingDeferredItem::claim_attempts`:
    /// stale or duplicate invalid reports cannot re-close or alter an
    /// event whose lifecycle already moved on. `claim_token` (WP65-21)
    /// is the authoritative processing generation when present.
    pub claim_attempts: Option<i64>,
    /// WP65-21: see `EmbeddingDeferredItem::claim_token`.
    pub claim_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct EmbeddingBatchRequest {
    pub embeddings: Vec<EmbeddingItem>,
    #[serde(default)]
    pub deferred: Vec<EmbeddingDeferredItem>,
    #[serde(default)]
    pub invalid: Vec<EmbeddingInvalidItem>,
}

/// POST /api/events/embeddings
///
/// WP65-02/03/04 contract:
/// - Canonical embedding identity = SHA-256(event_id, model_name,
///   model_revision, schema_hash) → deterministic `emb-<hex>` node id.
///   `computed_at_ns` is metadata only — it is NOT part of identity, so
///   retries with a fresh timestamp converge on the same node and the
///   same single EMBEDDED_AS relationship.
/// - Existence is checked per item with an indexed primary-key lookup —
///   work per batch is bounded by the batch, never by total embedding
///   cardinality (no full-table scan).
/// - The source StateChangeEvent must exist or the item is rejected —
///   no orphan embeddings. All of an item's writes run in one transaction.
/// - Per-item outcomes are attributable: `accepted` (newly persisted),
///   `already_existing` (idempotent replay), `rejected` (validation or
///   storage failure with reason). The `items` array carries the same
///   per-item status so callers can attribute partial batches exactly.
/// - WP65-09: replay is a NO-OP. An existing embedding identity whose
///   stored vector and dim are identical is left untouched — retry
///   timestamps never rewrite computation provenance. A conflicting
///   vector or dimension under the same identity is REJECTED (the
///   documented repair path is a new model/normalization/schema
///   identity, never silent overwrite of historical semantic evidence).
/// - WP65-11: event eligibility and event_type are derived from the
///   persisted StateChangeEvent — the client cannot redefine the source
///   event or enrich a non-syslog event.
/// - WP65-14: `deferred`/`invalid` reports drive a bounded enrichment
///   lifecycle (retry deadline / permanent ineligibility) so failed
///   events cannot starve healthy ones in the oldest-first fetch.
/// - Batch size and vector dimension are bounded by
///   MAX_EMBEDDING_BATCH / MAX_EMBEDDING_DIM.
const MAX_EMBEDDING_BATCH: usize = 512;
const MAX_EMBEDDING_LIFECYCLE: usize = 1024;
const MAX_EMBEDDING_DIM: usize = 4096;
/// Deferred-retry ceiling: after this many reported deferrals an event
/// is marked permanently ineligible rather than retrying forever.
const MAX_EMBEDDING_ATTEMPTS: i64 = 100;
/// Upper bound for a client-supplied retry_after_ns hint (24h). The
/// server-side backoff stays the authority; the hint can only extend
/// the deadline within this horizon, never beyond it.
const MAX_LIFECYCLE_HINT_NS: i64 = 24 * 3600 * 1_000_000_000;
/// Default reschedule for claim-contention deferrals (template pending
/// under another worker): short, does not consume an attempt.
const CLAIM_CONTENTION_BACKOFF_NS: i64 = 60 * 1_000_000_000;
/// Explicit "unknown" provenance sentinel — distinct from any real
/// revision/normalization/schema string, never fabricated.
const PROVENANCE_UNKNOWN: &str = "unknown";

fn embedding_now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Monotonic per-process sequence for WP65-21 claim tokens (combined
/// with ns timestamp + pid → unique processing generation per fetch).
fn embedding_claim_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Bounded exponential backoff for deferred enrichment attempts:
/// 60s, 120s, 240s … capped at 1h.
fn embedding_retry_backoff_ns(attempts: i64) -> i64 {
    let shift = (attempts - 1).clamp(0, 6) as u32;
    (60i64 << shift).min(3600) * 1_000_000_000
}

fn embedding_node_id(
    event_id: &str,
    model_name: &str,
    model_revision: &str,
    normalization_version: &str,
    schema_hash: &str,
) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    for part in [
        event_id,
        model_name,
        model_revision,
        normalization_version,
        schema_hash,
    ] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    let digest = h.finalize();
    let mut s = String::with_capacity(4 + digest.len() * 2);
    s.push_str("emb-");
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn validate_embedding_item(item: &EmbeddingItem) -> Result<(), String> {
    if item.event_id.is_empty() {
        return Err("empty event_id".into());
    }
    if item.model_name.is_empty() {
        return Err("empty model_name".into());
    }
    if item.vector.is_empty() {
        return Err("empty vector".into());
    }
    if item.vector.len() > MAX_EMBEDDING_DIM {
        return Err(format!(
            "vector dim {} exceeds {MAX_EMBEDDING_DIM}",
            item.vector.len()
        ));
    }
    if item.vector.iter().any(|v| !v.is_finite()) {
        return Err("vector contains non-finite value".into());
    }
    // WP65-10: a zero-norm vector carries no semantic content — it must
    // never be persisted as successful enrichment (provider zero-vector
    // fallbacks are rejected here at the API boundary).
    let norm_sq: f64 = item.vector.iter().map(|v| v * v).sum();
    if !norm_sq.is_finite() || norm_sq == 0.0 {
        return Err("vector has zero or non-finite norm".into());
    }
    // Declared dim must match the actual vector exactly.
    if let Some(d) = item.dim {
        if d != item.vector.len() as i64 {
            return Err(format!(
                "declared dim {d} does not match vector len {}",
                item.vector.len()
            ));
        }
    }
    // Computation provenance is required — a zero/negative timestamp is
    // missing provenance, not a valid "unknown".
    if item.computed_at_ns <= 0 {
        return Err("missing computed_at_ns provenance".into());
    }
    Ok(())
}

pub async fn events_embeddings_handler(
    State(state): State<AppState>,
    Json(req): Json<EmbeddingBatchRequest>,
) -> impl IntoResponse {
    if req.embeddings.len() > MAX_EMBEDDING_BATCH {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "error": format!("batch size {} exceeds limit {MAX_EMBEDDING_BATCH}", req.embeddings.len())
            })),
        )
            .into_response();
    }
    if req.deferred.len() > MAX_EMBEDDING_LIFECYCLE || req.invalid.len() > MAX_EMBEDDING_LIFECYCLE {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "error": format!(
                    "lifecycle report size exceeds limit {MAX_EMBEDDING_LIFECYCLE}"
                )
            })),
        )
            .into_response();
    }
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let ml_bus = state.ml_event_bus.clone();
    let submitted = req.embeddings.len() as i64;
    let deferred_submitted = req.deferred.len() as i64;
    let invalid_submitted = req.invalid.len() as i64;
    let model_name = req
        .embeddings
        .first()
        .map(|e| e.model_name.clone())
        .unwrap_or_default();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        let now_ns = embedding_now_ns();

        let mut accepted = 0i64;
        let mut already_existing = 0i64;
        let mut rejected: Vec<serde_json::Value> = Vec::new();
        let mut items: Vec<serde_json::Value> = Vec::new();

        for (idx, item) in req.embeddings.iter().enumerate() {
            let item_status = |status: &str, reason: Option<&str>| {
                let mut v = serde_json::json!({
                    "index": idx,
                    "event_id": item.event_id,
                    "status": status,
                });
                if let Some(r) = reason {
                    v["reason"] = serde_json::Value::String(r.to_string());
                }
                v
            };
            if let Err(reason) = validate_embedding_item(item) {
                rejected.push(serde_json::json!({
                    "index": idx,
                    "event_id": item.event_id,
                    "reason": reason,
                }));
                items.push(item_status("rejected", Some(&reason)));
                continue;
            }
            // WP65-12: coherent versioned semantic identity —
            // (event, model, revision, normalization, schema). Missing
            // provenance is the explicit "unknown" sentinel.
            let model_revision = item
                .model_revision
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| PROVENANCE_UNKNOWN.to_string());
            let normalization_version = item
                .normalization_version
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| PROVENANCE_UNKNOWN.to_string());
            let schema_hash = item
                .schema_hash
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| PROVENANCE_UNKNOWN.to_string());
            let emb_id = embedding_node_id(
                &item.event_id,
                &item.model_name,
                &model_revision,
                &normalization_version,
                &schema_hash,
            );

            // Per-item transaction: embedding node + relationship +
            // enrichment flag commit or roll back together.
            conn.query("BEGIN TRANSACTION")?;
            // Returns (outcome, reject_reason): outcome is "created" or
            // "replayed"; reject_reason is Some only for pre-write
            // validation failures inside the transaction — those roll
            // back cleanly.
            let item_result: Result<(Outcome, Option<String>), anyhow::Error> = (|| {
                // WP65-11: derive authoritative identity from the persisted
                // event — existence AND enrichment eligibility AND the
                // event_type stored on the embedding all come from the
                // graph, never from the client payload.
                let mut ev = conn.prepare(
                    "MATCH (e:StateChangeEvent {id: $eid}) \
                     RETURN e.event_type",
                )?;
                let event_row = conn
                    .execute(&mut ev, vec![("eid", Value::String(item.event_id.clone()))])?
                    .next();
                let persisted_event_type = match event_row {
                    Some(r) => match &r[0] {
                        Value::String(s) => s.clone(),
                        _ => String::new(),
                    },
                    None => {
                        return Ok((
                            Outcome::Rejected,
                            Some("source event not found".to_string()),
                        ));
                    }
                };
                if !persisted_event_type.starts_with("syslog") {
                    return Ok((
                        Outcome::Rejected,
                        Some("event not eligible for embedding enrichment".to_string()),
                    ));
                }

                // Indexed primary-key existence check — bounded per item.
                let mut ex = conn.prepare(
                    "MATCH (emb:EventEmbedding {id: $id}) \
                     RETURN emb.vector_json, emb.dim",
                )?;
                let existing = conn
                    .execute(&mut ex, vec![("id", Value::String(emb_id.clone()))])?
                    .next()
                    .map(|r| {
                        let s = |v: &Value| match v {
                            Value::String(s) => s.clone(),
                            _ => String::new(),
                        };
                        let d = match &r[1] {
                            Value::Int64(n) => *n,
                            _ => 0,
                        };
                        (s(&r[0]), d)
                    });

                let dim = item.vector.len() as i64;
                let vector_json = serde_json::to_string(&item.vector)
                    .map_err(|e| anyhow::anyhow!("vector serialize: {e}"))?;

                if let Some((stored_json, stored_dim)) = existing {
                    // WP65-09: immutable replay. Identical vector+dim is a
                    // no-op — computed_at_ns and relationship timestamps
                    // keep their original provenance. A conflicting
                    // vector or dimension under the same identity is
                    // rejected; repair requires a new model/normalization/
                    // schema identity, never a silent overwrite.
                    let identical = stored_dim == dim
                        && serde_json::from_str::<Vec<f64>>(&stored_json)
                            .map(|v| v == item.vector)
                            .unwrap_or(false);
                    if !identical {
                        return Ok((
                            Outcome::Rejected,
                            Some(
                                "embedding identity conflict: stored vector/dim differs"
                                    .to_string(),
                            ),
                        ));
                    }
                    // Replay: ensure the relationship exists (covers
                    // pre-WP65-03 partial writes) without touching its
                    // timestamp, then close the enrichment flag.
                    ensure_embedded_as_rel(&conn, &item.event_id, &emb_id, item.computed_at_ns)?;
                    mark_embedding_done(&conn, &item.event_id)?;
                    return Ok((Outcome::Replayed, None));
                }

                let mut upsert = conn.prepare(
                    "MERGE (emb:EventEmbedding {id: $id}) \
                     ON CREATE SET emb.event_id = $eid, emb.event_type = $et, \
                       emb.model_name = $mn, emb.model_revision = $mr, \
                       emb.normalization_version = $nv, emb.dim = $dim, \
                       emb.vector_json = $vj, emb.computed_at_ns = $can, emb.schema_hash = $sh",
                )?;
                conn.execute(
                    &mut upsert,
                    vec![
                        ("id", Value::String(emb_id.clone())),
                        ("eid", Value::String(item.event_id.clone())),
                        ("et", Value::String(persisted_event_type.clone())),
                        ("mn", Value::String(item.model_name.clone())),
                        ("mr", Value::String(model_revision.clone())),
                        ("nv", Value::String(normalization_version.clone())),
                        ("dim", Value::Int64(dim)),
                        ("vj", Value::String(vector_json)),
                        ("can", Value::Int64(item.computed_at_ns)),
                        ("sh", Value::String(schema_hash.clone())),
                    ],
                )?;

                // EMBEDDED_AS identity is (event, embedding) — computed_at_ns
                // is metadata, never part of the MERGE key.
                ensure_embedded_as_rel(&conn, &item.event_id, &emb_id, item.computed_at_ns)?;

                // Enrichment flag — write failures propagate (no `let _ =`).
                mark_embedding_done(&conn, &item.event_id)?;

                Ok((Outcome::Created, None))
            })();

            match item_result {
                Ok((Outcome::Rejected, reason)) => {
                    conn.query("ROLLBACK")?;
                    let reason = reason.unwrap_or_else(|| "rejected".to_string());
                    rejected.push(serde_json::json!({
                        "index": idx,
                        "event_id": item.event_id,
                        "reason": reason,
                    }));
                    items.push(item_status("rejected", Some(&reason)));
                }
                Ok((Outcome::Created, _)) => {
                    conn.query("COMMIT")?;
                    accepted += 1;
                    items.push(item_status("accepted", None));
                }
                Ok((Outcome::Replayed, _)) => {
                    conn.query("COMMIT")?;
                    already_existing += 1;
                    items.push(item_status("already_existing", None));
                }
                Err(e) => {
                    let _ = conn.query("ROLLBACK");
                    let reason = format!("persistence failure: {e:#}");
                    rejected.push(serde_json::json!({
                        "index": idx,
                        "event_id": item.event_id,
                        "reason": reason,
                    }));
                    items.push(item_status("rejected", Some(&reason)));
                }
            }
        }

        // ── WP65-14/16/17: bounded enrichment lifecycle ───────────────
        // Deferred events keep needs_embedding=1 but get a retry
        // deadline so the oldest-first fetch cannot be starved by them;
        // attempts are bounded and exhaustion marks the event
        // permanently ineligible. Invalid events are closed immediately.
        // Raw event evidence is never touched — only the enrichment
        // obligation.
        //
        // WP65-16 allowed transitions: a lifecycle report applies ONLY
        // when the event still needs enrichment (needs_embedding=1), is
        // not already permanently invalid, has an authoritative eligible
        // syslog source type, was not completed earlier in THIS batch,
        // and — when the worker supplies claim_attempts — the stored
        // attempt count still equals the claim (processing-generation
        // idempotency). Stale deferred/invalid reports therefore cannot
        // alter a completed or already-closed event, and duplicate
        // deliveries cannot consume extra attempts.
        //
        // WP65-17 retry accounting: "claim_contention" deferrals
        // reschedule without consuming an attempt; other deferrals
        // consume exactly one attempt per distinct claim. Client retry
        // hints are clamped to MAX_LIFECYCLE_HINT_NS. Operator retry
        // contract: an event closed by 'embedding_retry_exhausted' (or
        // an invalid report) is recoverable by resetting
        // needs_embedding=1, embedding_invalid=0,
        // embedding_attempts=0, embedding_retry_after_ns=0 on the
        // StateChangeEvent — raw operational evidence is retained.
        let mut deferred_applied = 0i64;
        let mut deferred_skipped = 0i64;
        let mut invalid_applied = 0i64;
        let mut invalid_skipped = 0i64;

        // Events whose embedding was accepted/replayed in THIS batch —
        // lifecycle reports for them are stale by definition. `items`
        // is fully built by the embeddings loop above, so the set is
        // exact.
        let mut deferred_seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut invalid_seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut completed_now: std::collections::HashSet<String> = std::collections::HashSet::new();
        for it in &items {
            if let (Some(eid), Some(st)) = (
                it.get("event_id").and_then(|v| v.as_str()),
                it.get("status").and_then(|v| v.as_str()),
            ) {
                if st == "accepted" || st == "already_existing" {
                    completed_now.insert(eid.to_string());
                }
            }
        }

        for d in &req.deferred {
            if d.event_id.is_empty() || !deferred_seen.insert(d.event_id.as_str()) {
                deferred_skipped += 1;
                continue;
            }
            let mut st = conn.prepare(
                "MATCH (e:StateChangeEvent {id: $eid}) \
                 RETURN e.event_type, e.needs_embedding, e.embedding_invalid, \
                 e.embedding_attempts, e.embedding_claim_token",
            )?;
            let row = conn
                .execute(&mut st, vec![("eid", Value::String(d.event_id.clone()))])?
                .next();
            let Some(row) = row else {
                deferred_skipped += 1;
                continue;
            };
            let s = |v: &Value| match v {
                Value::String(s) => s.clone(),
                _ => String::new(),
            };
            let i64v = |v: &Value| match v {
                Value::Int64(n) => *n,
                _ => 0,
            };
            let event_type = s(&row[0]);
            let needs_embedding = i64v(&row[1]);
            let is_invalid = i64v(&row[2]) != 0;
            let attempts = i64v(&row[3]);
            let stored_token = s(&row[4]);
            // WP65-21/17 claim gate: a supplied claim_token is the
            // authoritative processing generation (newest fetch wins);
            // without one, the legacy claim_attempts snapshot applies;
            // with neither, the report is unconditional (compat).
            let claim_ok = match d.claim_token.as_deref() {
                Some(tok) if !tok.is_empty() => stored_token == tok,
                _ => match d.claim_attempts {
                    Some(c) => c == attempts,
                    None => true,
                },
            };
            if !event_type.starts_with("syslog")
                || needs_embedding != 1
                || is_invalid
                || completed_now.contains(&d.event_id)
                || !claim_ok
            {
                deferred_skipped += 1;
                continue;
            }
            // Bounded client hint: ignore non-future hints, clamp the
            // horizon — the hint can extend but never replace the
            // server-side backoff authority.
            let raw_hint = d.retry_after_ns.unwrap_or(0);
            let hint = if raw_hint > now_ns {
                raw_hint.min(now_ns + MAX_LIFECYCLE_HINT_NS)
            } else {
                0
            };
            let contention = d.kind.as_deref() == Some("claim_contention");
            // WP65-21 atomic claim-and-set: the eligibility + claim
            // predicate lives INSIDE the update's WHERE clause, so a
            // concurrent lifecycle transition (completion, invalid
            // close, attempt consume, newer fetch token) between the
            // read above and this write makes the update match zero
            // rows instead of double-applying. `RETURN e.id` lets us
            // distinguish applied from fenced.
            let token_clause = match d.claim_token.as_deref() {
                Some(tok) if !tok.is_empty() => {
                    "AND e.embedding_claim_token = $ctok"
                }
                _ => "",
            };
            let guard = format!(
                "MATCH (e:StateChangeEvent {{id: $eid}}) \
                 WHERE e.needs_embedding = 1 AND e.embedding_invalid = 0 \
                 AND e.embedding_attempts = $expected {token_clause}"
            );
            let apply = |conn: &lbug::Connection<'_>,
                         set_clause: &str,
                         extra: Vec<(&str, Value)>|
             -> Result<bool, anyhow::Error> {
                let mut params = vec![
                    ("eid", Value::String(d.event_id.clone())),
                    ("expected", Value::Int64(attempts)),
                ];
                if !token_clause.is_empty()
                    && let Some(tok) = d.claim_token.as_deref()
                {
                    params.push(("ctok", Value::String(tok.to_string())));
                }
                params.extend(extra);
                let mut up = conn.prepare(&format!("{guard} SET {set_clause} RETURN e.id"))?;
                let mut rows = conn.execute(&mut up, params)?;
                Ok(rows.next().is_some())
            };
            let applied = if contention {
                // Claim contention is not the event's fault: reschedule
                // without consuming an attempt (WP65-17).
                let deadline = (now_ns + CLAIM_CONTENTION_BACKOFF_NS).max(hint);
                apply(
                    &conn,
                    "e.embedding_retry_after_ns = $r",
                    vec![("r", Value::Int64(deadline))],
                )?
            } else {
                let new_attempts = attempts + 1;
                if new_attempts > MAX_EMBEDDING_ATTEMPTS {
                    apply(
                        &conn,
                        "e.needs_embedding = 0, e.embedding_invalid = 1, \
                         e.embedding_invalid_reason = 'embedding_retry_exhausted', \
                         e.embedding_retry_after_ns = 0",
                        vec![],
                    )?
                } else {
                    let backoff = embedding_retry_backoff_ns(new_attempts);
                    let deadline = (now_ns + backoff).max(hint);
                    apply(
                        &conn,
                        "e.embedding_attempts = $a, e.embedding_retry_after_ns = $r",
                        vec![
                            ("a", Value::Int64(new_attempts)),
                            ("r", Value::Int64(deadline)),
                        ],
                    )?
                }
            };
            if applied {
                deferred_applied += 1;
            } else {
                deferred_skipped += 1;
            }
        }
        for inv in &req.invalid {
            if inv.event_id.is_empty()
                || inv.reason.len() > 500
                || !invalid_seen.insert(inv.event_id.as_str())
                || deferred_seen.contains(inv.event_id.as_str())
            {
                invalid_skipped += 1;
                continue;
            }
            let mut chk = conn.prepare(
                "MATCH (e:StateChangeEvent {id: $eid}) \
                 RETURN e.event_type, e.needs_embedding, e.embedding_invalid, \
                 e.embedding_attempts, e.embedding_claim_token",
            )?;
            let row = conn
                .execute(&mut chk, vec![("eid", Value::String(inv.event_id.clone()))])?
                .next();
            let Some(row) = row else {
                invalid_skipped += 1;
                continue;
            };
            let s = |v: &Value| match v {
                Value::String(s) => s.clone(),
                _ => String::new(),
            };
            let i64v = |v: &Value| match v {
                Value::Int64(n) => *n,
                _ => 0,
            };
            let event_type = s(&row[0]);
            let needs_embedding = i64v(&row[1]);
            let is_invalid = i64v(&row[2]) != 0;
            let attempts = i64v(&row[3]);
            let stored_token = s(&row[4]);
            let claim_ok = match inv.claim_token.as_deref() {
                Some(tok) if !tok.is_empty() => stored_token == tok,
                _ => match inv.claim_attempts {
                    Some(c) => c == attempts,
                    None => true,
                },
            };
            if !event_type.starts_with("syslog")
                || needs_embedding != 1
                || is_invalid
                || completed_now.contains(&inv.event_id)
                || !claim_ok
            {
                invalid_skipped += 1;
                continue;
            }
            // Same atomic claim-and-set as the deferred path: the
            // eligibility + claim predicate is inside the update WHERE
            // so a concurrent transition fences the write.
            let token_clause = match inv.claim_token.as_deref() {
                Some(tok) if !tok.is_empty() => {
                    "AND e.embedding_claim_token = $ctok"
                }
                _ => "",
            };
            let mut params = vec![
                ("eid", Value::String(inv.event_id.clone())),
                ("expected", Value::Int64(attempts)),
                ("reason", Value::String(inv.reason.clone())),
            ];
            if !token_clause.is_empty()
                && let Some(tok) = inv.claim_token.as_deref()
            {
                params.push(("ctok", Value::String(tok.to_string())));
            }
            let mut up = conn.prepare(&format!(
                "MATCH (e:StateChangeEvent {{id: $eid}}) \
                 WHERE e.needs_embedding = 1 AND e.embedding_invalid = 0 \
                 AND e.embedding_attempts = $expected {token_clause} \
                 SET e.needs_embedding = 0, e.embedding_invalid = 1, \
                 e.embedding_invalid_reason = $reason, \
                 e.embedding_retry_after_ns = 0 RETURN e.id"
            ))?;
            let mut rows = conn.execute(&mut up, params)?;
            if rows.next().is_some() {
                invalid_applied += 1;
            } else {
                invalid_skipped += 1;
            }
        }
        Ok::<_, anyhow::Error>(serde_json::json!({
            "submitted": submitted,
            "accepted": accepted,
            "already_existing": already_existing,
            "persisted": accepted + already_existing,
            "rejected": rejected,
            "items": items,
            "deferred": deferred_applied,
            "deferred_skipped": deferred_skipped,
            "invalid": invalid_applied,
            "invalid_skipped": invalid_skipped,
            "deferred_submitted": deferred_submitted,
            "invalid_submitted": invalid_submitted,
        }))
    })
    .await;

    match result {
        Ok(Ok(body)) => {
            let persisted = body["persisted"].as_i64().unwrap_or(0);
            ml_bus.publish(MlEvent::EmbeddingBatchCompleted {
                events_embedded: persisted,
                model_name,
                embedding_type: "syslog".to_string(),
            });
            (StatusCode::OK, Json(body)).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Created,
    Replayed,
    Rejected,
}

/// Create the (event → embedding) EMBEDDED_AS relationship when absent.
/// Relationship identity is (event, embedding); `computed_at_ns` is
/// metadata refreshed only on creation — replay never rewrites the
/// original computation provenance.
fn ensure_embedded_as_rel(
    conn: &Connection,
    event_id: &str,
    emb_id: &str,
    computed_at_ns: i64,
) -> Result<(), anyhow::Error> {
    let mut rel_check = conn.prepare(
        "MATCH (e:StateChangeEvent {id: $eid})-[r:EMBEDDED_AS]->(emb:EventEmbedding {id: $embid}) \
         RETURN count(r)",
    )?;
    let rel_count = conn
        .execute(
            &mut rel_check,
            vec![
                ("eid", Value::String(event_id.to_string())),
                ("embid", Value::String(emb_id.to_string())),
            ],
        )?
        .next()
        .map(|r| match &r[0] {
            Value::Int64(n) => *n,
            _ => 0,
        })
        .unwrap_or(0);
    if rel_count == 0 {
        let mut rel = conn.prepare(
            "MATCH (e:StateChangeEvent {id: $eid}), (emb:EventEmbedding {id: $embid}) \
             CREATE (e)-[:EMBEDDED_AS {computed_at_ns: $can}]->(emb)",
        )?;
        conn.execute(
            &mut rel,
            vec![
                ("eid", Value::String(event_id.to_string())),
                ("embid", Value::String(emb_id.to_string())),
                ("can", Value::Int64(computed_at_ns)),
            ],
        )?;
    }
    Ok(())
}

/// Close the enrichment obligation on the source event. Idempotent —
/// replays and deferred-then-completed flows converge on the same state.
fn mark_embedding_done(conn: &Connection, event_id: &str) -> Result<(), anyhow::Error> {
    let mut mark = conn.prepare(
        "MATCH (e:StateChangeEvent {id: $eid}) SET e.needs_embedding = 0, \
         e.embedding_retry_after_ns = 0",
    )?;
    conn.execute(
        &mut mark,
        vec![("eid", Value::String(event_id.to_string()))],
    )?;
    Ok(())
}

/// GET /api/devices/unembedded-config?limit=N
pub async fn devices_unembedded_config_handler(
    State(state): State<AppState>,
    Query(q): Query<UnembeddedQuery>,
) -> impl IntoResponse {
    let db = state.store.db();
    let limit = q.limit.unwrap_or(50).min(200);
    let result =
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db)?;
            let rows = conn.query(&format!(
            "MATCH (d:Device) WHERE d.needs_config_embedding = 1 \
             RETURN d.address, d.hostname, d.vendor \
             LIMIT {limit}",
        ))
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .map(|r| {
            let s = |v: &Value| match v { Value::String(s) => s.clone(), _ => String::new() };
            serde_json::json!({"address": s(&r[0]), "hostname": s(&r[1]), "vendor": s(&r[2])})
        })
        .collect::<Vec<_>>();
            Ok::<_, anyhow::Error>(rows)
        })
        .await;

    match result {
        Ok(Ok(rows)) => {
            (StatusCode::OK, Json(serde_json::json!({"devices": rows}))).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct ConfigEmbeddingRequest {
    pub vector: Vec<f64>,
    pub model_name: String,
    pub computed_at_ns: i64,
    pub schema_hash: Option<String>,
}

/// POST /api/devices/:address/config-embedding
pub async fn device_config_embedding_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Json(req): Json<ConfigEmbeddingRequest>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        let id = format!("ce-{address}");
        let dim = req.vector.len() as i64;
        let vector_json = serde_json::to_string(&req.vector).unwrap_or_default();
        // Upsert config embedding
        let mut check_stmt =
            conn.prepare("MATCH (e:DeviceConfigEmbedding {id: $id}) RETURN e.id")?;
        let exists = conn
            .execute(&mut check_stmt, vec![("id", Value::String(id.clone()))])?
            .next()
            .is_some();
        if exists {
            let mut stmt = conn.prepare(
                "MATCH (e:DeviceConfigEmbedding {id: $id}) SET e.model_name = $mn, e.dim = $dim, \
                 e.vector_json = $vj, e.computed_at_ns = $can, e.schema_hash = $sh",
            )?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("mn", Value::String(req.model_name)),
                    ("dim", Value::Int64(dim)),
                    ("vj", Value::String(vector_json)),
                    ("can", Value::Int64(req.computed_at_ns)),
                    ("sh", Value::String(req.schema_hash.unwrap_or_default())),
                ],
            )?;
        } else {
            let mut stmt = conn.prepare(
                "CREATE (:DeviceConfigEmbedding {id: $id, device_address: $da, model_name: $mn, \
                 dim: $dim, vector_json: $vj, computed_at_ns: $can, schema_hash: $sh})",
            )?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("da", Value::String(address.clone())),
                    ("mn", Value::String(req.model_name)),
                    ("dim", Value::Int64(dim)),
                    ("vj", Value::String(vector_json)),
                    ("can", Value::Int64(req.computed_at_ns)),
                    ("sh", Value::String(req.schema_hash.unwrap_or_default())),
                ],
            )?;
            let mut link_stmt = conn.prepare(
                "MATCH (d:Device {address: $da}), (e:DeviceConfigEmbedding {id: $id}) \
                 MERGE (d)-[:CONFIG_EMBEDDED_AS {computed_at_ns: $can}]->(e)",
            )?;
            let _ = conn.execute(
                &mut link_stmt,
                vec![
                    ("da", Value::String(address.clone())),
                    ("id", Value::String(id)),
                    ("can", Value::Int64(req.computed_at_ns)),
                ],
            );
        }
        // Mark device as embedded
        let mut mark_stmt =
            conn.prepare("MATCH (d:Device {address: $da}) SET d.needs_config_embedding = 0")?;
        let _ = conn.execute(&mut mark_stmt, vec![("da", Value::String(address))]);
        Ok::<_, anyhow::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/ml/embeddings/stats
pub async fn ml_embedding_stats_handler(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let syslog_embedded = conn
            .query("MATCH (e:EventEmbedding) RETURN count(e)")
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .next()
            .map(|r| match &r[0] {
                Value::Int64(n) => *n,
                _ => 0,
            })
            .unwrap_or(0);
        let syslog_pending = conn
            .query("MATCH (e:StateChangeEvent) WHERE e.needs_embedding = 1 RETURN count(e)")
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .next()
            .map(|r| match &r[0] {
                Value::Int64(n) => *n,
                _ => 0,
            })
            .unwrap_or(0);
        let config_embedded = conn
            .query("MATCH (e:DeviceConfigEmbedding) RETURN count(e)")
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .next()
            .map(|r| match &r[0] {
                Value::Int64(n) => *n,
                _ => 0,
            })
            .unwrap_or(0);
        let config_pending = conn
            .query("MATCH (d:Device) WHERE d.needs_config_embedding = 1 RETURN count(d)")
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .next()
            .map(|r| match &r[0] {
                Value::Int64(n) => *n,
                _ => 0,
            })
            .unwrap_or(0);
        Ok::<_, anyhow::Error>(serde_json::json!({
            "syslog_embedded": syslog_embedded,
            "syslog_pending": syslog_pending,
            "config_embedded": config_embedded,
            "config_pending": config_pending,
        }))
    })
    .await;

    match result {
        Ok(Ok(stats)) => (StatusCode::OK, Json(stats)).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/ml/syslog-clusters — list SyslogCluster nodes created by detection_clustering job
pub async fn ml_syslog_clusters_handler(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let rows = conn.query(
            "MATCH (c:SyslogCluster) \
             RETURN c.id, c.label, c.event_count, c.centroid_event_id, c.top_event_types, c.created_at_ns \
             ORDER BY c.event_count DESC LIMIT 200",
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .map(|r| {
            let s = |v: &Value| match v { Value::String(s) => s.clone(), _ => String::new() };
            let n = |v: &Value| match v { Value::Int64(n) => *n, _ => 0 };
            let top_types: Vec<String> = match &r[4] {
                Value::String(s) => serde_json::from_str(s).unwrap_or_default(),
                _ => vec![],
            };
            serde_json::json!({
                "id": s(&r[0]),
                "label": s(&r[1]),
                "event_count": n(&r[2]),
                "centroid_event_id": s(&r[3]),
                "top_event_types": top_types,
                "created_at_ns": n(&r[5]),
            })
        })
        .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>(rows)
    })
    .await;

    match result {
        Ok(Ok(rows)) => {
            (StatusCode::OK, Json(serde_json::json!({"clusters": rows}))).into_response()
        }
        Ok(Err(_)) | Err(_) => {
            (StatusCode::OK, Json(serde_json::json!({"clusters": []}))).into_response()
        }
    }
}

/// GET /api/ml/exports/quality — summary across recent exports
pub async fn ml_export_quality_handler(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let rows = conn
            .query(
                "MATCH (e:ParquetExport) \
             RETURN e.export_type, e.started_at_ns, e.row_count, e.anomaly_rows, \
                    e.normal_rows, e.status, e.quality_json \
             ORDER BY e.started_at_ns DESC LIMIT 20",
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .map(|r| {
                let s = |v: &Value| match v {
                    Value::String(s) => s.clone(),
                    _ => String::new(),
                };
                let n = |v: &Value| match v {
                    Value::Int64(n) => *n,
                    _ => 0,
                };
                serde_json::json!({
                    "export_type": s(&r[0]),
                    "last_export_at": n(&r[1]),
                    "row_count": n(&r[2]),
                    "anomaly_rows": n(&r[3]),
                    "normal_rows": n(&r[4]),
                    "status": s(&r[5]),
                    "quality_json": s(&r[6]),
                })
            })
            .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>(rows)
    })
    .await;

    match result {
        Ok(Ok(rows)) => {
            (StatusCode::OK, Json(serde_json::json!({"quality": rows}))).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/ml/lineage/:model_id
pub async fn ml_lineage_handler(
    State(state): State<AppState>,
    Path(model_id): Path<String>,
) -> impl IntoResponse {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        // Model info
        let mut mstmt = conn.prepare(
            "MATCH (m:ModelArtifact {id: $id}) \
             RETURN m.id, m.model_type, m.version, m.trained_at_ns, m.val_auc, m.is_active",
        )?;
        let model = conn
            .execute(&mut mstmt, vec![("id", Value::String(model_id.clone()))])?
            .next()
            .map(|r| {
                let s = |v: &Value| match v {
                    Value::String(s) => s.clone(),
                    _ => String::new(),
                };
                let n = |v: &Value| match v {
                    Value::Int64(n) => *n,
                    _ => 0,
                };
                let f = |v: &Value| match v {
                    Value::Double(f) => *f,
                    _ => 0.0,
                };
                serde_json::json!({
                    "id": s(&r[0]), "model_type": s(&r[1]), "version": s(&r[2]),
                    "trained_at_ns": n(&r[3]), "val_auc": f(&r[4]), "is_active": n(&r[5]) != 0,
                })
            });
        // Training data lineage
        let mut tstmt = conn.prepare(
            "MATCH (m:ModelArtifact {id: $id})-[:TRAINED_ON]->(e:ParquetExport) \
             RETURN e.id, e.export_type, e.row_count, e.since_ns, e.until_ns, e.status",
        )?;
        let exports = conn
            .execute(&mut tstmt, vec![("id", Value::String(model_id))])?
            .map(|r| {
                let s = |v: &Value| match v {
                    Value::String(s) => s.clone(),
                    _ => String::new(),
                };
                let n = |v: &Value| match v {
                    Value::Int64(n) => *n,
                    _ => 0,
                };
                serde_json::json!({
                    "id": s(&r[0]), "export_type": s(&r[1]),
                    "row_count": n(&r[2]), "since_ns": n(&r[3]),
                    "until_ns": n(&r[4]), "status": s(&r[5]),
                })
            })
            .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>(serde_json::json!({"model": model, "training_exports": exports}))
    })
    .await;

    match result {
        Ok(Ok(lineage)) => (StatusCode::OK, Json(lineage)).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── Semantic similarity search ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SimilarEventsQuery {
    pub event_id: String,
    pub limit: Option<usize>,
    pub min_similarity: Option<f64>,
}

/// GET /api/ml/similar-events?event_id=X&limit=N&min_similarity=0.7
///
/// Returns the top-K most similar events to the given event_id using cosine
/// similarity over EventEmbedding vector_json fields.  Runs in-process (no
/// external vector DB required).
pub async fn ml_similar_events_handler(
    State(state): State<AppState>,
    Query(q): Query<SimilarEventsQuery>,
) -> impl IntoResponse {
    let db = state.store.db();
    let event_id = q.event_id.clone();
    let limit = q.limit.unwrap_or(10).min(50);
    let min_sim = q.min_similarity.unwrap_or(0.0);

    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;

        // 1. Fetch the query embedding. An event can carry embeddings
        //    from several model spaces; pick deterministically (most
        //    recent, then lowest node id) and compare ONLY within that
        //    exact model space — model_name + model_revision +
        //    normalization_version + dim + schema_hash must all match,
        //    or vectors are incomparable (WP65-12).
        let mut qstmt = conn.prepare(
            "MATCH (emb:EventEmbedding {event_id: $eid}) \
             RETURN emb.vector_json, emb.dim, emb.model_name, emb.model_revision, \
                    emb.normalization_version, emb.schema_hash, emb.id \
             ORDER BY emb.computed_at_ns DESC, emb.id ASC \
             LIMIT 1",
        )?;
        let query_row = conn
            .execute(&mut qstmt, vec![("eid", Value::String(event_id.clone()))])?
            .next();

        let query_row = match query_row {
            Some(r) => r,
            None => {
                return Ok::<_, anyhow::Error>(serde_json::json!({
                    "error": "event not found or not embedded",
                    "similar": []
                }));
            }
        };

        let s = |v: &Value| match v {
            Value::String(s) => s.clone(),
            _ => String::new(),
        };
        // WP65-12: empty/NULL provenance reads as the explicit "unknown"
        // sentinel — the same normalization the schema migration applies
        // — so legacy rows and new writes compare in one consistent space.
        let prov = |v: &Value| {
            let raw = s(v);
            if raw.is_empty() {
                PROVENANCE_UNKNOWN.to_string()
            } else {
                raw
            }
        };
        let query_vec_json = s(&query_row[0]);
        let model_name = s(&query_row[2]);
        let model_revision = prov(&query_row[3]);
        let normalization_version = prov(&query_row[4]);
        let schema_hash = prov(&query_row[5]);
        let query_dim = match &query_row[1] {
            Value::Int64(n) => *n,
            _ => 0,
        };

        let query_vec: Vec<f64> = serde_json::from_str(&query_vec_json)
            .map_err(|e| anyhow::anyhow!("invalid query vector json: {e}"))?;
        let q_norm = cosine_norm(&query_vec);

        if q_norm == 0.0 {
            return Ok(serde_json::json!({
                "similar": [],
                "provenance_verified": false,
            }));
        }

        // 2. Fetch candidates from the SAME model space. Fetch one extra
        //    row to detect truncation of the bounded candidate scan.
        let mut cstmt = conn.prepare(
            "MATCH (emb:EventEmbedding) \
             WHERE emb.event_id <> $eid AND emb.model_name = $mn \
               AND emb.model_revision = $mr AND emb.normalization_version = $nv \
               AND emb.dim = $dim AND emb.schema_hash = $sh \
             RETURN emb.event_id, emb.vector_json, emb.event_type, emb.computed_at_ns \
             ORDER BY emb.computed_at_ns DESC, emb.id ASC \
             LIMIT 5001",
        )?;
        let cand_rows: Vec<_> = conn
            .execute(
                &mut cstmt,
                vec![
                    ("eid", Value::String(event_id.clone())),
                    ("mn", Value::String(model_name.clone())),
                    ("mr", Value::String(model_revision.clone())),
                    ("nv", Value::String(normalization_version.clone())),
                    ("dim", Value::Int64(query_dim)),
                    ("sh", Value::String(schema_hash.clone())),
                ],
            )?
            .collect();
        let scan_truncated = cand_rows.len() > 5000;
        let all_rows = &cand_rows[..cand_rows.len().min(5000)];

        let s2 = |v: &Value| match v {
            Value::String(s) => s.clone(),
            _ => String::new(),
        };
        let n2 = |v: &Value| match v {
            Value::Int64(n) => *n,
            _ => 0,
        };

        // WP65-24: unknown model provenance must never establish
        // VERIFIED semantic compatibility. Candidates are already
        // constrained to the query's exact (model_name, revision,
        // normalization, dim, schema) space, so they share its
        // provenance status — when any provenance field is the
        // explicit "unknown" sentinel the whole comparison is
        // UNVERIFIED and the response says so. Similarity remains a
        // retrieval aid, never proof of model-space equivalence.
        let provenance_verified = model_revision != PROVENANCE_UNKNOWN
            && normalization_version != PROVENANCE_UNKNOWN
            && schema_hash != PROVENANCE_UNKNOWN;

        let mut scored: Vec<(f64, String, String, i64)> = Vec::new();
        for row in all_rows {
            let cand_event_id = s2(&row[0]);
            let cand_vec_json = s2(&row[1]);
            let event_type = s2(&row[2]);
            let computed_at = n2(&row[3]);
            if let Ok(cand_vec) = serde_json::from_str::<Vec<f64>>(&cand_vec_json) {
                let sim = cosine_similarity(&query_vec, q_norm, &cand_vec);
                if sim >= min_sim {
                    scored.push((sim, cand_event_id, event_type, computed_at));
                }
            }
        }

        // 3. Sort descending, take top-K
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        let similar: Vec<_> = scored
            .into_iter()
            .map(|(sim, eid, etype, cat)| {
                serde_json::json!({
                    "event_id": eid,
                    "event_type": etype,
                    "similarity": (sim * 10000.0).round() / 10000.0,
                    "computed_at_ns": cat,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "query_event_id": event_id,
            "model_name": model_name,
            "model_revision": model_revision,
            "normalization_version": normalization_version,
            "dim": query_dim,
            "schema_hash": schema_hash,
            // Bounded recall contract: candidates are the most recent
            // <=5000 embeddings in the SAME model space — this is NOT a
            // global nearest-neighbor search; scan_truncated=true means
            // older same-space embeddings were not considered.
            "candidate_scan_limit": 5000,
            "scan_truncated": scan_truncated,
            // WP65-24: false when the query model space carries
            // "unknown" provenance — results are retrieval candidates
            // only and must not be treated as verified-compatible
            // embeddings.
            "provenance_verified": provenance_verified,
            "similar": similar,
        }))
    })
    .await;

    match result {
        Ok(Ok(body)) => (StatusCode::OK, Json(body)).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn cosine_norm(v: &[f64]) -> f64 {
    v.iter().map(|x| x * x).sum::<f64>().sqrt()
}

fn cosine_similarity(a: &[f64], a_norm: f64, b: &[f64]) -> f64 {
    if a.len() != b.len() || a_norm == 0.0 {
        return 0.0;
    }
    let b_norm = cosine_norm(b);
    if b_norm == 0.0 {
        return 0.0;
    }
    let dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    dot / (a_norm * b_norm)
}

/// GET /api/gnn/inference-results?device_address=X&since_ns=N
#[derive(Debug, Deserialize)]
pub struct GnnResultsQuery {
    pub device_address: Option<String>,
    pub since_ns: Option<i64>,
    pub limit: Option<i64>,
}

pub async fn gnn_results_query_handler(
    State(state): State<AppState>,
    Query(q): Query<GnnResultsQuery>,
) -> impl IntoResponse {
    let db = state.store.db();
    let limit = q.limit.unwrap_or(100).min(500);
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let mut conditions = Vec::new();
        if let Some(addr) = &q.device_address {
            conditions.push(format!("r.device_address = '{}'", addr.replace('\'', "")));
        }
        if let Some(since) = q.since_ns {
            conditions.push(format!("r.inferred_at_ns >= {since}"));
        }
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };
        let cypher = format!(
            "MATCH (r:GnnInferenceResult){where_clause} \
             RETURN r.id, r.snapshot_ns, r.model_id, r.device_address, r.anomaly_score, \
                    r.uncertainty_margin, r.threshold, r.is_anomalous, \
                    r.top_contributing_device_1, r.attention_weight_1, r.inferred_at_ns \
             ORDER BY r.inferred_at_ns DESC LIMIT {limit}"
        );
        let rows = conn
            .query(&cypher)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .map(|r| {
                let s = |v: &Value| match v {
                    Value::String(s) => s.clone(),
                    _ => String::new(),
                };
                let n = |v: &Value| match v {
                    Value::Int64(n) => *n,
                    _ => 0,
                };
                let f = |v: &Value| match v {
                    Value::Double(f) => *f,
                    Value::Float(f) => *f as f64,
                    _ => 0.0,
                };
                serde_json::json!({
                    "id": s(&r[0]), "snapshot_ns": n(&r[1]), "model_id": s(&r[2]),
                    "device_address": s(&r[3]), "anomaly_score": f(&r[4]),
                    "uncertainty_margin": f(&r[5]), "threshold": f(&r[6]),
                    "is_anomalous": n(&r[7]) != 0,
                    "top_contributing_device": s(&r[8]), "attention_weight": f(&r[9]),
                    "inferred_at_ns": n(&r[10]),
                })
            })
            .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>(rows)
    })
    .await;

    match result {
        Ok(Ok(rows)) => {
            (StatusCode::OK, Json(serde_json::json!({"results": rows}))).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
