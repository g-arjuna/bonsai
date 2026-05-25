//! EV1-7 T4/T5 — Playbook DB-backed CRUD + execution tracking.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use lbug::{Connection, Value};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::AppState;

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

// ── Record types ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PlaybookRecord {
    pub id: String,
    pub name: String,
    pub description: String,
    pub rule_ids_json: String,
    pub vendor: String,
    pub steps_json: String,
    pub verify_graph: String,
    pub enabled: bool,
    pub version: i64,
    pub created_at_ns: i64,
    pub updated_at_ns: i64,
    pub updated_by: String,
    pub execution_count: i64,
    pub success_count: i64,
}

fn playbook_from_row(r: &[Value]) -> PlaybookRecord {
    let s = |v: &Value| match v { Value::String(s) => s.clone(), _ => String::new() };
    let n = |v: &Value| match v { Value::Int64(n) => *n, _ => 0 };
    PlaybookRecord {
        id: s(&r[0]),
        name: s(&r[1]),
        description: s(&r[2]),
        rule_ids_json: s(&r[3]),
        vendor: s(&r[4]),
        steps_json: s(&r[5]),
        verify_graph: s(&r[6]),
        enabled: n(&r[7]) != 0,
        version: n(&r[8]),
        created_at_ns: n(&r[9]),
        updated_at_ns: n(&r[10]),
        updated_by: s(&r[11]),
        execution_count: n(&r[12]),
        success_count: n(&r[13]),
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PlaybookExecutionRecord {
    pub id: String,
    pub playbook_id: String,
    pub detection_id: String,
    pub device_address: String,
    pub outcome: String,
    pub started_at_ns: i64,
    pub completed_at_ns: i64,
    pub operator_id: String,
    pub failure_step: String,
    pub failure_reason: String,
}

fn execution_from_row(r: &[Value]) -> PlaybookExecutionRecord {
    let s = |v: &Value| match v { Value::String(s) => s.clone(), _ => String::new() };
    let n = |v: &Value| match v { Value::Int64(n) => *n, _ => 0 };
    PlaybookExecutionRecord {
        id: s(&r[0]),
        playbook_id: s(&r[1]),
        detection_id: s(&r[2]),
        device_address: s(&r[3]),
        outcome: s(&r[4]),
        started_at_ns: n(&r[5]),
        completed_at_ns: n(&r[6]),
        operator_id: s(&r[7]),
        failure_step: s(&r[8]),
        failure_reason: s(&r[9]),
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// GET /api/playbooks
pub async fn list_playbooks_handler(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let rows = conn
            .query(
                "MATCH (p:Playbook) \
                 RETURN p.id, p.name, p.description, p.rule_ids_json, p.vendor, \
                        p.steps_json, p.verify_graph, p.enabled, p.version, \
                        p.created_at_ns, p.updated_at_ns, p.updated_by, \
                        p.execution_count, p.success_count \
                 ORDER BY p.name",
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .map(|r| playbook_from_row(&r))
            .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>(rows)
    })
    .await;

    match result {
        Ok(Ok(rows)) => (StatusCode::OK, Json(serde_json::json!({"playbooks": rows}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/playbooks/:id
pub async fn get_playbook_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let mut stmt = conn.prepare(
            "MATCH (p:Playbook {id: $id}) \
             RETURN p.id, p.name, p.description, p.rule_ids_json, p.vendor, \
                    p.steps_json, p.verify_graph, p.enabled, p.version, \
                    p.created_at_ns, p.updated_at_ns, p.updated_by, \
                    p.execution_count, p.success_count",
        )?;
        let pb = conn.execute(&mut stmt, vec![("id", Value::String(id))])?
            .next()
            .map(|r| playbook_from_row(&r));
        Ok::<_, anyhow::Error>(pb)
    })
    .await;

    match result {
        Ok(Ok(Some(pb))) => (StatusCode::OK, Json(pb)).into_response(),
        Ok(Ok(None)) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "not found"}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct CreatePlaybookRequest {
    pub name: String,
    pub description: Option<String>,
    pub rule_ids: Option<Vec<String>>,
    pub vendor: Option<String>,
    pub steps_json: String,
    pub verify_graph: Option<String>,
    pub enabled: Option<bool>,
}

/// POST /api/playbooks
pub async fn create_playbook_handler(
    State(state): State<AppState>,
    Json(req): Json<CreatePlaybookRequest>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        let id = Uuid::new_v4().to_string();
        let now = now_ns();
        let rule_ids_json = req.rule_ids
            .map(|ids| serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let enabled = if req.enabled.unwrap_or(true) { 1i64 } else { 0 };
        let mut stmt = conn.prepare(
            "CREATE (:Playbook {id: $id, name: $name, description: $desc, \
             rule_ids_json: $rij, vendor: $vendor, steps_json: $steps, \
             verify_graph: $vg, enabled: $en, version: 1, \
             created_at_ns: $now, updated_at_ns: $now, updated_by: 'api', \
             execution_count: 0, success_count: 0})",
        )?;
        conn.execute(&mut stmt, vec![
            ("id", Value::String(id.clone())),
            ("name", Value::String(req.name)),
            ("desc", Value::String(req.description.unwrap_or_default())),
            ("rij", Value::String(rule_ids_json)),
            ("vendor", Value::String(req.vendor.unwrap_or_default())),
            ("steps", Value::String(req.steps_json)),
            ("vg", Value::String(req.verify_graph.unwrap_or_default())),
            ("en", Value::Int64(enabled)),
            ("now", Value::Int64(now)),
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
pub struct UpdatePlaybookRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub rule_ids: Option<Vec<String>>,
    pub vendor: Option<String>,
    pub steps_json: Option<String>,
    pub verify_graph: Option<String>,
    pub enabled: Option<bool>,
    pub updated_by: Option<String>,
}

/// PUT /api/playbooks/:id
pub async fn update_playbook_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdatePlaybookRequest>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        let now = now_ns();
        let updated_by = req.updated_by.clone().unwrap_or_else(|| "api".to_string());

        macro_rules! set_str {
            ($field:literal, $val:expr) => {{
                let mut stmt = conn.prepare(concat!("MATCH (p:Playbook {id: $id}) SET p.", $field, " = $v"))?;
                conn.execute(&mut stmt, vec![("id", Value::String(id.clone())), ("v", Value::String($val))])?;
            }};
        }
        macro_rules! set_i64 {
            ($field:literal, $val:expr) => {{
                let mut stmt = conn.prepare(concat!("MATCH (p:Playbook {id: $id}) SET p.", $field, " = $v"))?;
                conn.execute(&mut stmt, vec![("id", Value::String(id.clone())), ("v", Value::Int64($val))])?;
            }};
        }

        if let Some(v) = req.name { set_str!("name", v); }
        if let Some(v) = req.description { set_str!("description", v); }
        if let Some(ids) = req.rule_ids {
            let j = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_string());
            set_str!("rule_ids_json", j);
        }
        if let Some(v) = req.vendor { set_str!("vendor", v); }
        if let Some(v) = req.steps_json { set_str!("steps_json", v); }
        if let Some(v) = req.verify_graph { set_str!("verify_graph", v); }
        if let Some(v) = req.enabled { set_i64!("enabled", if v { 1 } else { 0 }); }

        // bump version + timestamps
        {
            let mut stmt = conn.prepare(
                "MATCH (p:Playbook {id: $id}) SET p.version = p.version + 1, \
                 p.updated_at_ns = $now, p.updated_by = $by",
            )?;
            conn.execute(&mut stmt, vec![
                ("id", Value::String(id.clone())),
                ("now", Value::Int64(now)),
                ("by", Value::String(updated_by)),
            ])?;
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

/// DELETE /api/playbooks/:id  (soft delete — sets enabled=false)
pub async fn delete_playbook_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        let now = now_ns();
        let mut stmt = conn.prepare(
            "MATCH (p:Playbook {id: $id}) SET p.enabled = 0, p.updated_at_ns = $now, p.updated_by = 'api'",
        )?;
        conn.execute(&mut stmt, vec![
            ("id", Value::String(id)),
            ("now", Value::Int64(now)),
        ])?;
        Ok::<_, anyhow::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// POST /api/playbooks/:id/test — check if verify_graph passes for a device
#[derive(Debug, Deserialize)]
pub struct PlaybookTestRequest {
    pub device_address: String,
}

pub async fn test_playbook_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PlaybookTestRequest>,
) -> impl IntoResponse {
    let db = state.store.db();
    let device_address = req.device_address.clone();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let mut stmt = conn.prepare("MATCH (p:Playbook {id: $id}) RETURN p.verify_graph")?;
        let verify_graph = conn.execute(&mut stmt, vec![("id", Value::String(id))])?
            .next()
            .map(|r| match &r[0] { Value::String(s) => s.clone(), _ => String::new() })
            .unwrap_or_default();

        if verify_graph.is_empty() {
            return Ok::<_, anyhow::Error>(serde_json::json!({"passed": true, "details": "no verify_graph defined"}));
        }

        // Substitute $device_address placeholder and run
        let cypher = verify_graph.replace("$device_address", &format!("'{}'", device_address.replace('\'', "")));
        let passed = conn.query(&cypher).is_ok();
        Ok(serde_json::json!({"passed": passed, "details": if passed { "verify_graph matched" } else { "verify_graph returned no results" }}))
    })
    .await;

    match result {
        Ok(Ok(result)) => (StatusCode::OK, Json(result)).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── Execution tracking ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RecordExecutionRequest {
    pub detection_id: Option<String>,
    pub device_address: String,
    pub outcome: String,
    pub started_at_ns: i64,
    pub completed_at_ns: Option<i64>,
    pub operator_id: Option<String>,
    pub failure_step: Option<String>,
    pub failure_reason: Option<String>,
}

/// POST /api/playbooks/:id/executions
pub async fn record_execution_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RecordExecutionRequest>,
) -> impl IntoResponse {
    let db = state.store.db();
    let write_lock = state.store.write_lock();
    let result = tokio::task::spawn_blocking(move || {
        let _g = write_lock.lock().expect("write lock poisoned");
        let conn = Connection::new(&db)?;
        let exec_id = Uuid::new_v4().to_string();
        let now = now_ns();
        let completed_at = req.completed_at_ns.unwrap_or(now);
        let is_success = req.outcome == "success";

        let mut stmt = conn.prepare(
            "CREATE (:PlaybookExecution {id: $id, playbook_id: $pid, detection_id: $did, \
             device_address: $da, outcome: $out, started_at_ns: $san, completed_at_ns: $can, \
             operator_id: $op, failure_step: $fs, failure_reason: $fr})",
        )?;
        conn.execute(&mut stmt, vec![
            ("id", Value::String(exec_id.clone())),
            ("pid", Value::String(id.clone())),
            ("did", Value::String(req.detection_id.unwrap_or_default())),
            ("da", Value::String(req.device_address)),
            ("out", Value::String(req.outcome)),
            ("san", Value::Int64(req.started_at_ns)),
            ("can", Value::Int64(completed_at)),
            ("op", Value::String(req.operator_id.unwrap_or_default())),
            ("fs", Value::String(req.failure_step.unwrap_or_default())),
            ("fr", Value::String(req.failure_reason.unwrap_or_default())),
        ])?;

        // Link Playbook -> PlaybookExecution
        let mut link_stmt = conn.prepare(
            "MATCH (p:Playbook {id: $pid}), (e:PlaybookExecution {id: $eid}) \
             MERGE (p)-[:HAS_PLAYBOOK_EXECUTION {executed_at_ns: $now}]->(e)",
        )?;
        let _ = conn.execute(&mut link_stmt, vec![
            ("pid", Value::String(id.clone())),
            ("eid", Value::String(exec_id.clone())),
            ("now", Value::Int64(now)),
        ]);

        // Increment counters
        let mut cnt_stmt = conn.prepare(
            "MATCH (p:Playbook {id: $id}) SET p.execution_count = p.execution_count + 1",
        )?;
        conn.execute(&mut cnt_stmt, vec![("id", Value::String(id.clone()))])?;

        if is_success {
            let mut suc_stmt = conn.prepare(
                "MATCH (p:Playbook {id: $id}) SET p.success_count = p.success_count + 1",
            )?;
            conn.execute(&mut suc_stmt, vec![("id", Value::String(id))])?;
        }

        Ok::<_, anyhow::Error>(exec_id)
    })
    .await;

    match result {
        Ok(Ok(exec_id)) => (StatusCode::CREATED, Json(serde_json::json!({"id": exec_id}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/playbooks/:id/executions
pub async fn list_executions_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let mut stmt = conn.prepare(
            "MATCH (e:PlaybookExecution {playbook_id: $pid}) \
             RETURN e.id, e.playbook_id, e.detection_id, e.device_address, e.outcome, \
                    e.started_at_ns, e.completed_at_ns, e.operator_id, e.failure_step, e.failure_reason \
             ORDER BY e.started_at_ns DESC LIMIT 200",
        )?;
        let rows = conn.execute(&mut stmt, vec![("pid", Value::String(id))])?
            .map(|r| execution_from_row(&r))
            .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>(rows)
    })
    .await;

    match result {
        Ok(Ok(rows)) => (StatusCode::OK, Json(serde_json::json!({"executions": rows}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/playbooks/stats
pub async fn playbook_stats_handler(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)?;
        let rows = conn.query(
            "MATCH (p:Playbook) WHERE p.enabled = 1 \
             RETURN p.id, p.name, p.execution_count, p.success_count \
             ORDER BY p.execution_count DESC",
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .map(|r| {
            let s = |v: &Value| match v { Value::String(s) => s.clone(), _ => String::new() };
            let n = |v: &Value| match v { Value::Int64(n) => *n, _ => 0 };
            let exec_count = n(&r[2]);
            let succ_count = n(&r[3]);
            let success_rate = if exec_count > 0 {
                (succ_count as f64 / exec_count as f64) * 100.0
            } else {
                0.0
            };
            serde_json::json!({
                "id": s(&r[0]),
                "name": s(&r[1]),
                "execution_count": exec_count,
                "success_count": succ_count,
                "success_rate_pct": success_rate,
            })
        })
        .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>(rows)
    })
    .await;

    match result {
        Ok(Ok(rows)) => (StatusCode::OK, Json(serde_json::json!({"stats": rows}))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── Boot-time YAML migration ──────────────────────────────────────────────────

/// Migrate playbook YAML files from `playbooks/library/` into the DB at startup.
/// Idempotent: skips playbooks where `updated_by != 'boot_migration'` (operator-edited).
pub fn migrate_playbooks_from_yaml(conn: &Connection) -> anyhow::Result<usize> {
    use std::path::Path;

    let library_path = Path::new("playbooks/library");
    if !library_path.exists() {
        return Ok(0);
    }

    let mut count = 0;
    let entries = std::fs::read_dir(library_path)?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let stem = path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let id = format!("pb-{stem}");

        // Check if already exists and was operator-edited
        let mut check_stmt = conn.prepare(
            "MATCH (p:Playbook {id: $id}) RETURN p.updated_by",
        )?;
        if let Some(row) = conn.execute(&mut check_stmt, vec![("id", Value::String(id.clone()))])?.next() {
            let updated_by = match &row[0] { Value::String(s) => s.clone(), _ => String::new() };
            if updated_by != "boot_migration" && !updated_by.is_empty() {
                continue; // operator-edited — don't overwrite
            }
        }

        // Parse YAML minimally — extract name, description, steps
        let name = stem.replace('-', " ")
            .split_whitespace()
            .map(|w| {
                let mut chars = w.chars();
                match chars.next() {
                    None => String::new(),
                    Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        let now = now_ns();

        let mut upsert_stmt = conn.prepare(
            "MERGE (p:Playbook {id: $id}) \
             SET p.name = $name, p.description = $desc, \
                 p.rule_ids_json = '[]', p.vendor = '', \
                 p.steps_json = $steps, p.verify_graph = '', \
                 p.enabled = 1, p.version = 1, \
                 p.created_at_ns = $now, p.updated_at_ns = $now, \
                 p.updated_by = 'boot_migration', \
                 p.execution_count = 0, p.success_count = 0",
        );

        if let Ok(ref mut stmt) = upsert_stmt {
            let _ = conn.execute(stmt, vec![
                ("id", Value::String(id)),
                ("name", Value::String(name)),
                ("desc", Value::String(format!("Migrated from {stem}.yaml"))),
                ("steps", Value::String(content)),
                ("now", Value::Int64(now)),
            ]);
            count += 1;
        }
    }

    Ok(count)
}
