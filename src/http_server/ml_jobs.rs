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
use uuid::Uuid;

use crate::ml_event_bus::{MlEvent, MlEventBus};

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
    let stream = BroadcastStream::new(rx).map(|result| {
        match result {
            Ok(event) => {
                let data = serde_json::to_string(&event).unwrap_or_default();
                Ok(Event::default().data(data))
            }
            Err(_) => Ok(Event::default().comment("lag")),
        }
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
pub async fn list_ml_exports_handler(
    State(state): State<AppState>,
) -> impl IntoResponse {
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
        Ok(Ok(rows)) => (StatusCode::OK, Json(serde_json::json!({"exports": rows}))).into_response(),
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
            conn.execute(&mut stmt, vec![("id", Value::String(id.clone())), ("v", Value::String(s.clone()))])?;
        }
        if let Some(v) = req.row_count {
            let mut stmt = conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.row_count = $v")?;
            conn.execute(&mut stmt, vec![("id", Value::String(id.clone())), ("v", Value::Int64(v))])?;
        }
        if let Some(v) = req.anomaly_rows {
            let mut stmt = conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.anomaly_rows = $v")?;
            conn.execute(&mut stmt, vec![("id", Value::String(id.clone())), ("v", Value::Int64(v))])?;
        }
        if let Some(v) = req.normal_rows {
            let mut stmt = conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.normal_rows = $v")?;
            conn.execute(&mut stmt, vec![("id", Value::String(id.clone())), ("v", Value::Int64(v))])?;
        }
        if let Some(v) = &req.schema_hash {
            let mut stmt = conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.schema_hash = $v")?;
            conn.execute(&mut stmt, vec![("id", Value::String(id.clone())), ("v", Value::String(v.clone()))])?;
        }
        if let Some(v) = &req.output_path {
            let mut stmt = conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.output_path = $v")?;
            conn.execute(&mut stmt, vec![("id", Value::String(id.clone())), ("v", Value::String(v.clone()))])?;
        }
        if let Some(v) = &req.error_message {
            let mut stmt = conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.error_message = $v")?;
            conn.execute(&mut stmt, vec![("id", Value::String(id.clone())), ("v", Value::String(v.clone()))])?;
        }
        if let Some(v) = &req.quality_json {
            let mut stmt = conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.quality_json = $v")?;
            conn.execute(&mut stmt, vec![("id", Value::String(id.clone())), ("v", Value::String(v.clone()))])?;
        }
        if let Some(v) = req.completed_at_ns {
            let mut stmt = conn.prepare("MATCH (e:ParquetExport {id: $id}) SET e.completed_at_ns = $v")?;
            conn.execute(&mut stmt, vec![("id", Value::String(id.clone())), ("v", Value::Int64(v))])?;
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
    let s = |v: &Value| match v { Value::String(s) => s.clone(), _ => String::new() };
    let n = |v: &Value| match v { Value::Int64(n) => *n, _ => 0 };
    let f = |v: &Value| match v { Value::Double(f) => *f, Value::Float(f) => *f as f64, Value::Int64(n) => *n as f64, _ => 0.0 };
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
pub async fn list_ml_models_handler(
    State(state): State<AppState>,
) -> impl IntoResponse {
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
             LIMIT 1".to_string()
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
        let model = conn.query(&cypher)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .next()
            .map(|r| model_from_row(&r));
        Ok::<_, anyhow::Error>(model)
    })
    .await;

    match result {
        Ok(Ok(Some(m))) => (StatusCode::OK, Json(m)).into_response(),
        Ok(Ok(None)) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "no active model"}))).into_response(),
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
        conn.execute(&mut stmt, vec![
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
            ("mcp", Value::String(req.model_card_path.unwrap_or_default())),
        ])?;
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
            ml_bus.publish(MlEvent::ModelActivated { model_id: id, model_type, val_auc });
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
    let s = |v: &Value| match v { Value::String(s) => s.clone(), _ => String::new() };
    let n = |v: &Value| match v { Value::Int64(n) => *n, _ => 0 };
    let f = |v: &Value| match v { Value::Double(f) => *f, Value::Float(f) => *f as f64, _ => 0.0 };
    MlJobRunRecord {
        id: s(&r[0]), job_type: s(&r[1]),
        started_at_ns: n(&r[2]), completed_at_ns: n(&r[3]),
        status: s(&r[4]), trigger: s(&r[5]),
        input_parquet_id: s(&r[6]), output_model_path: s(&r[7]),
        val_auc: f(&r[8]), val_f1: f(&r[9]),
        error_message: s(&r[10]), config_json: s(&r[11]),
        cancel_requested: n(&r[12]) != 0,
    }
}

/// GET /api/ml/jobs
pub async fn list_ml_jobs_handler(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let rows = conn.query(
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
    pub job_type: String,
    pub trigger: String,
    pub input_parquet_id: Option<String>,
    pub config_json: Option<String>,
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
        let mut stmt = conn.prepare(
            "CREATE (:MlJobRun {id: $id, job_type: $jt, started_at_ns: $now, \
             completed_at_ns: 0, status: 'running', trigger: $trig, \
             input_parquet_id: $ipid, output_model_path: '', val_auc: 0.0, val_f1: 0.0, \
             error_message: '', config_json: $cfg, cancel_requested: 0})",
        )?;
        conn.execute(&mut stmt, vec![
            ("id", Value::String(id.clone())),
            ("jt", Value::String(req.job_type.clone())),
            ("now", Value::Int64(now)),
            ("trig", Value::String(req.trigger)),
            ("ipid", Value::String(req.input_parquet_id.unwrap_or_default())),
            ("cfg", Value::String(req.config_json.unwrap_or_else(|| "{}".to_string()))),
        ])?;
        Ok::<_, anyhow::Error>((id, req.job_type))
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
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        // Fetch job type for events
        let mut info_stmt = conn.prepare("MATCH (j:MlJobRun {id: $id}) RETURN j.job_type")?;
        let job_type = conn.execute(&mut info_stmt, vec![("id", Value::String(id.clone()))])?
            .next()
            .map(|r| match &r[0] { Value::String(s) => s.clone(), _ => String::new() })
            .unwrap_or_default();

        macro_rules! set_field {
            ($field:literal, $val:expr) => {{
                let mut stmt = conn.prepare(concat!("MATCH (j:MlJobRun {id: $id}) SET j.", $field, " = $v"))?;
                conn.execute(&mut stmt, vec![("id", Value::String(id.clone())), ("v", $val)])?;
            }};
        }
        if let Some(v) = &req.status { set_field!("status", Value::String(v.clone())); }
        if let Some(v) = req.completed_at_ns { set_field!("completed_at_ns", Value::Int64(v)); }
        if let Some(v) = req.val_auc { set_field!("val_auc", Value::Double(v)); }
        if let Some(v) = req.val_f1 { set_field!("val_f1", Value::Double(v)); }
        if let Some(v) = &req.output_model_path { set_field!("output_model_path", Value::String(v.clone())); }
        if let Some(v) = &req.error_message { set_field!("error_message", Value::String(v.clone())); }
        if let Some(v) = req.cancel_requested { set_field!("cancel_requested", Value::Int64(if v { 1 } else { 0 })); }
        Ok::<_, anyhow::Error>((job_type, req.status.unwrap_or_default()))
    })
    .await;

    match result {
        Ok(Ok((job_type, status))) => {
            match status.as_str() {
                "succeeded" => ml_bus.publish(MlEvent::JobCompleted {
                    job_id: id, job_type, outcome: "succeeded".to_string(),
                    val_auc: 0.0, val_f1: 0.0, model_path: String::new(),
                }),
                "failed" => ml_bus.publish(MlEvent::JobFailed {
                    job_id: id, job_type, error: "see error_message".to_string(),
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
    let s = |v: &Value| match v { Value::String(s) => s.clone(), _ => String::new() };
    let n = |v: &Value| match v { Value::Int64(n) => *n, _ => 0 };
    MlJobScheduleRecord {
        id: s(&r[0]), job_id: s(&r[1]), cron_expr: s(&r[2]),
        enabled: n(&r[3]) != 0, last_modified_by: s(&r[4]),
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
        Ok(Ok(rows)) => (StatusCode::OK, Json(serde_json::json!({"schedules": rows}))).into_response(),
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
        let exists = conn.execute(&mut check_stmt, vec![("id", Value::String(id.clone()))])?
            .next()
            .is_some();
        if exists {
            let mut stmt = conn.prepare(
                "MATCH (s:MlJobSchedule {id: $id}) SET s.cron_expr = $ce, s.enabled = $en, \
                 s.last_modified_by = 'api', s.last_modified_at_ns = $now",
            )?;
            conn.execute(&mut stmt, vec![
                ("id", Value::String(id.clone())),
                ("ce", Value::String(req.cron_expr)),
                ("en", Value::Int64(if req.enabled { 1 } else { 0 })),
                ("now", Value::Int64(now)),
            ])?;
        } else {
            let mut stmt = conn.prepare(
                "CREATE (:MlJobSchedule {id: $id, job_id: $jid, cron_expr: $ce, enabled: $en, \
                 last_modified_by: 'api', last_modified_at_ns: $now})",
            )?;
            conn.execute(&mut stmt, vec![
                ("id", Value::String(id.clone())),
                ("jid", Value::String(req.job_id)),
                ("ce", Value::String(req.cron_expr)),
                ("en", Value::Int64(if req.enabled { 1 } else { 0 })),
                ("now", Value::Int64(now)),
            ])?;
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
            conn.execute(&mut stmt, vec![
                ("id", Value::String(id.clone())),
                ("sns", Value::Int64(snapshot_ns)),
                ("mid", Value::String(model_id.clone())),
                ("da", Value::String(item.device_address.clone())),
                ("as_", Value::Double(item.anomaly_score)),
                ("um", Value::Double(item.uncertainty_margin.unwrap_or(0.0))),
                ("thr", Value::Double(item.threshold)),
                ("ia", Value::Int64(is_ano)),
                ("tc1", Value::String(item.top_contributing_device_1.clone().unwrap_or_default())),
                ("tc2", Value::String(item.top_contributing_device_2.clone().unwrap_or_default())),
                ("aw1", Value::Double(item.attention_weight_1.unwrap_or(0.0))),
                ("aw2", Value::Double(item.attention_weight_2.unwrap_or(0.0))),
                ("now", Value::Int64(now)),
            ])?;
            // Link Device -> GnnInferenceResult
            let mut link_stmt = conn.prepare(
                "MATCH (d:Device {address: $da}), (r:GnnInferenceResult {id: $rid}) \
                 MERGE (d)-[:GNN_SCORED {inferred_at_ns: $now}]->(r)",
            )?;
            let _ = conn.execute(&mut link_stmt, vec![
                ("da", Value::String(item.device_address.clone())),
                ("rid", Value::String(id)),
                ("now", Value::Int64(now)),
            ]);
            count += 1;
            if item.anomaly_score > top_score { top_score = item.anomaly_score; }
            if item.is_anomalous { anomalous += 1; }
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
                model_id,
            });
            (StatusCode::OK, Json(serde_json::json!({"written": count, "anomalous": anomalous}))).into_response()
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
            conn.execute(&mut stmt, vec![
                ("id", Value::String(id.clone())),
                ("irid", Value::String(item.inference_result_id.clone())),
                ("src", Value::String(item.source_device_address.clone())),
                ("nbr", Value::String(item.neighbour_device_address.clone())),
                ("et", Value::String(item.edge_type.clone())),
                ("aw", Value::Double(item.attention_weight)),
                ("sns", Value::Int64(item.snapshot_ns)),
            ])?;
            // Link GnnInferenceResult -> GnnAttentionSnapshot
            let mut link_stmt = conn.prepare(
                "MATCH (r:GnnInferenceResult {id: $irid}), (a:GnnAttentionSnapshot {id: $aid}) \
                 MERGE (r)-[:HAS_ATTENTION]->(a)",
            )?;
            let _ = conn.execute(&mut link_stmt, vec![
                ("irid", Value::String(item.inference_result_id.clone())),
                ("aid", Value::String(id)),
            ]);
            count += 1;
        }
        Ok::<_, anyhow::Error>(count)
    })
    .await;

    match result {
        Ok(Ok(count)) => (StatusCode::OK, Json(serde_json::json!({"written": count}))).into_response(),
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
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let cypher = if event_type.is_empty() {
            format!(
                "MATCH (e:StateChangeEvent) WHERE e.needs_embedding = 1 \
                 RETURN e.id, e.device_address, e.event_type, e.detail_json, e.occurred_at \
                 ORDER BY e.occurred_at DESC LIMIT {limit}"
            )
        } else {
            format!(
                "MATCH (e:StateChangeEvent) WHERE e.needs_embedding = 1 AND e.event_type = '{}' \
                 RETURN e.id, e.device_address, e.event_type, e.detail_json, e.occurred_at \
                 ORDER BY e.occurred_at DESC LIMIT {limit}",
                event_type.replace('\'', "")
            )
        };
        let rows = conn.query(&cypher)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .map(|r| {
                let s = |v: &Value| match v { Value::String(s) => s.clone(), _ => String::new() };
                serde_json::json!({
                    "id": s(&r[0]),
                    "device_address": s(&r[1]),
                    "event_type": s(&r[2]),
                    "detail_json": s(&r[3]),
                })
            })
            .collect::<Vec<_>>();
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
    pub computed_at_ns: i64,
    pub schema_hash: Option<String>,
    pub event_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct EmbeddingBatchRequest {
    pub embeddings: Vec<EmbeddingItem>,
}

/// POST /api/events/embeddings
pub async fn events_embeddings_handler(
    State(state): State<AppState>,
    Json(req): Json<EmbeddingBatchRequest>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let ml_bus = state.ml_event_bus.clone();
    let count = req.embeddings.len() as i64;
    let model_name = req.embeddings.first().map(|e| e.model_name.clone()).unwrap_or_default();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        for item in &req.embeddings {
            let id = Uuid::new_v4().to_string();
            let dim = item.vector.len() as i64;
            let vector_json = serde_json::to_string(&item.vector).unwrap_or_default();
            let mut stmt = conn.prepare(
                "CREATE (:EventEmbedding {id: $id, event_id: $eid, event_type: $et, \
                 model_name: $mn, dim: $dim, vector_json: $vj, \
                 computed_at_ns: $can, schema_hash: $sh})",
            )?;
            conn.execute(&mut stmt, vec![
                ("id", Value::String(id.clone())),
                ("eid", Value::String(item.event_id.clone())),
                ("et", Value::String(item.event_type.clone().unwrap_or_default())),
                ("mn", Value::String(item.model_name.clone())),
                ("dim", Value::Int64(dim)),
                ("vj", Value::String(vector_json)),
                ("can", Value::Int64(item.computed_at_ns)),
                ("sh", Value::String(item.schema_hash.clone().unwrap_or_default())),
            ])?;
            // Mark source event as embedded
            let mut mark_stmt = conn.prepare(
                "MATCH (e:StateChangeEvent {id: $eid}) SET e.needs_embedding = 0",
            )?;
            let _ = conn.execute(&mut mark_stmt, vec![("eid", Value::String(item.event_id.clone()))]);
            // Link StateChangeEvent -> EventEmbedding
            let mut link_stmt = conn.prepare(
                "MATCH (e:StateChangeEvent {id: $eid}), (emb:EventEmbedding {id: $embid}) \
                 MERGE (e)-[:EMBEDDED_AS {computed_at_ns: $can}]->(emb)",
            )?;
            let _ = conn.execute(&mut link_stmt, vec![
                ("eid", Value::String(item.event_id.clone())),
                ("embid", Value::String(id)),
                ("can", Value::Int64(item.computed_at_ns)),
            ]);
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => {
            ml_bus.publish(MlEvent::EmbeddingBatchCompleted {
                events_embedded: count,
                model_name,
                embedding_type: "syslog".to_string(),
            });
            (StatusCode::OK, Json(serde_json::json!({"written": count}))).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/devices/unembedded-config?limit=N
pub async fn devices_unembedded_config_handler(
    State(state): State<AppState>,
    Query(q): Query<UnembeddedQuery>,
) -> impl IntoResponse {
    let db = state.store.db();
    let limit = q.limit.unwrap_or(50).min(200);
    let result = tokio::task::spawn_blocking(move || {
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
        Ok(Ok(rows)) => (StatusCode::OK, Json(serde_json::json!({"devices": rows}))).into_response(),
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
        let mut check_stmt = conn.prepare("MATCH (e:DeviceConfigEmbedding {id: $id}) RETURN e.id")?;
        let exists = conn.execute(&mut check_stmt, vec![("id", Value::String(id.clone()))])?
            .next().is_some();
        if exists {
            let mut stmt = conn.prepare(
                "MATCH (e:DeviceConfigEmbedding {id: $id}) SET e.model_name = $mn, e.dim = $dim, \
                 e.vector_json = $vj, e.computed_at_ns = $can, e.schema_hash = $sh",
            )?;
            conn.execute(&mut stmt, vec![
                ("id", Value::String(id.clone())),
                ("mn", Value::String(req.model_name)),
                ("dim", Value::Int64(dim)),
                ("vj", Value::String(vector_json)),
                ("can", Value::Int64(req.computed_at_ns)),
                ("sh", Value::String(req.schema_hash.unwrap_or_default())),
            ])?;
        } else {
            let mut stmt = conn.prepare(
                "CREATE (:DeviceConfigEmbedding {id: $id, device_address: $da, model_name: $mn, \
                 dim: $dim, vector_json: $vj, computed_at_ns: $can, schema_hash: $sh})",
            )?;
            conn.execute(&mut stmt, vec![
                ("id", Value::String(id.clone())),
                ("da", Value::String(address.clone())),
                ("mn", Value::String(req.model_name)),
                ("dim", Value::Int64(dim)),
                ("vj", Value::String(vector_json)),
                ("can", Value::Int64(req.computed_at_ns)),
                ("sh", Value::String(req.schema_hash.unwrap_or_default())),
            ])?;
            let mut link_stmt = conn.prepare(
                "MATCH (d:Device {address: $da}), (e:DeviceConfigEmbedding {id: $id}) \
                 MERGE (d)-[:CONFIG_EMBEDDED_AS {computed_at_ns: $can}]->(e)",
            )?;
            let _ = conn.execute(&mut link_stmt, vec![
                ("da", Value::String(address.clone())),
                ("id", Value::String(id)),
                ("can", Value::Int64(req.computed_at_ns)),
            ]);
        }
        // Mark device as embedded
        let mut mark_stmt = conn.prepare(
            "MATCH (d:Device {address: $da}) SET d.needs_config_embedding = 0",
        )?;
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
        let syslog_embedded = conn.query("MATCH (e:EventEmbedding) RETURN count(e)")
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .next().map(|r| match &r[0] { Value::Int64(n) => *n, _ => 0 }).unwrap_or(0);
        let syslog_pending = conn.query("MATCH (e:StateChangeEvent) WHERE e.needs_embedding = 1 RETURN count(e)")
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .next().map(|r| match &r[0] { Value::Int64(n) => *n, _ => 0 }).unwrap_or(0);
        let config_embedded = conn.query("MATCH (e:DeviceConfigEmbedding) RETURN count(e)")
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .next().map(|r| match &r[0] { Value::Int64(n) => *n, _ => 0 }).unwrap_or(0);
        let config_pending = conn.query("MATCH (d:Device) WHERE d.needs_config_embedding = 1 RETURN count(d)")
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .next().map(|r| match &r[0] { Value::Int64(n) => *n, _ => 0 }).unwrap_or(0);
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

/// GET /api/ml/exports/quality — summary across recent exports
pub async fn ml_export_quality_handler(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let rows = conn.query(
            "MATCH (e:ParquetExport) \
             RETURN e.export_type, e.started_at_ns, e.row_count, e.anomaly_rows, \
                    e.normal_rows, e.status, e.quality_json \
             ORDER BY e.started_at_ns DESC LIMIT 20",
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .map(|r| {
            let s = |v: &Value| match v { Value::String(s) => s.clone(), _ => String::new() };
            let n = |v: &Value| match v { Value::Int64(n) => *n, _ => 0 };
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
        Ok(Ok(rows)) => (StatusCode::OK, Json(serde_json::json!({"quality": rows}))).into_response(),
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
        let model = conn.execute(&mut mstmt, vec![("id", Value::String(model_id.clone()))])?
            .next()
            .map(|r| {
                let s = |v: &Value| match v { Value::String(s) => s.clone(), _ => String::new() };
                let n = |v: &Value| match v { Value::Int64(n) => *n, _ => 0 };
                let f = |v: &Value| match v { Value::Double(f) => *f, _ => 0.0 };
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
        let exports = conn.execute(&mut tstmt, vec![("id", Value::String(model_id))])?
            .map(|r| {
                let s = |v: &Value| match v { Value::String(s) => s.clone(), _ => String::new() };
                let n = |v: &Value| match v { Value::Int64(n) => *n, _ => 0 };
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
        let rows = conn.query(&cypher)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .map(|r| {
                let s = |v: &Value| match v { Value::String(s) => s.clone(), _ => String::new() };
                let n = |v: &Value| match v { Value::Int64(n) => *n, _ => 0 };
                let f = |v: &Value| match v { Value::Double(f) => *f, Value::Float(f) => *f as f64, _ => 0.0 };
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
        Ok(Ok(rows)) => (StatusCode::OK, Json(serde_json::json!({"results": rows}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

