#![allow(unused_imports, dead_code, unused_variables)]
use super::*;
use super::openapi_schema::openapi_schema;

// ─── graph insights (T1-4) ───────────────────────────────────────────────────

async fn graph_insights_handler(
    State(state): State<AppState>,
) -> Result<Json<crate::graph::algorithms::GraphInsights>, (StatusCode, String)> {
    let db = state.store.db();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::algorithms::graph_insights(&conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

// ─── explorer (T1-5) ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ExplorerQueryBody {
    cypher: String,
    /// If set, record last_run_at and row count on this saved-query id.
    saved_query_id: Option<String>,
}

async fn explorer_query_handler(
    State(state): State<AppState>,
    Json(body): Json<ExplorerQueryBody>,
) -> Result<Json<crate::graph::explorer::ExplorerResult>, (StatusCode, String)> {
    let cypher = body.cypher.clone();
    let saved_query_id = body.saved_query_id.clone();
    let db = state.store.db();

    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::explorer::execute_query(&conn, &cypher).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    // Best-effort: update run metadata on the saved query if one was specified.
    if let Some(id) = saved_query_id {
        let count = result.row_count as i64;
        let _ = state.store.mark_saved_query_run(id, count).await;
    }

    Ok(Json(result))
}

// ─── saved queries CRUD (T1-6) ───────────────────────────────────────────────

async fn list_saved_queries_handler(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::graph::SavedQueryRecord>>, (StatusCode, String)> {
    state
        .store
        .list_saved_queries()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(Deserialize)]
struct CreateSavedQueryBody {
    name: String,
    #[serde(default)]
    description: String,
    cypher: String,
}

async fn create_saved_query_handler(
    State(state): State<AppState>,
    Json(body): Json<CreateSavedQueryBody>,
) -> Result<Json<crate::graph::SavedQueryRecord>, (StatusCode, String)> {
    state
        .store
        .create_saved_query(body.name, body.description, body.cypher)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

async fn delete_saved_query_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .store
        .delete_saved_query(id)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ── embedding handlers (T2-1) ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct UpsertEmbeddingsBody {
    records: Vec<crate::graph::EmbeddingRecord>,
}

async fn upsert_embeddings_handler(
    State(state): State<AppState>,
    Json(body): Json<UpsertEmbeddingsBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let count = body.records.len();
    state
        .store
        .write_device_embeddings(body.records)
        .await
        .map(|_| {
            tracing::info!(count, "embedding upsert accepted");
            StatusCode::NO_CONTENT
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(Serialize)]
struct EmbeddingsResponse {
    embeddings: Vec<crate::graph::EmbeddingRecord>,
}

async fn list_embeddings_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<EmbeddingsResponse>, (StatusCode, String)> {
    state
        .store
        .list_device_embeddings(address)
        .await
        .map(|embeddings| Json(EmbeddingsResponse { embeddings }))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ── investigation handlers (T3-1 / T3-2) ─────────────────────────────────────

#[derive(Deserialize)]
struct CreateInvestigationBody {
    detection_id: String,
    device_address: String,
    #[serde(default = "default_trigger")]
    trigger: String,
}
fn default_trigger() -> String {
    "operator".into()
}

#[derive(Deserialize)]
struct CompleteInvestigationBody {
    status: String,
    summary: String,
    #[serde(default)]
    proposal_json: String,
    #[serde(default)]
    tokens_used: i64,
    #[serde(default)]
    cost_usd: f64,
}

#[derive(Serialize)]
struct InvestigationDetailResponse {
    investigation: crate::graph::InvestigationRecord,
    tool_calls: Vec<crate::graph::ToolCallRecord>,
}

async fn list_investigations_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .store
        .list_investigations()
        .await
        .map(|inv| Json(serde_json::json!({ "investigations": inv })))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn create_investigation_handler(
    State(state): State<AppState>,
    Json(body): Json<CreateInvestigationBody>,
) -> Result<Json<crate::graph::InvestigationRecord>, (StatusCode, String)> {
    state
        .store
        .create_investigation(body.detection_id, body.device_address, body.trigger)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn get_investigation_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<InvestigationDetailResponse>, (StatusCode, String)> {
    let inv = state
        .store
        .get_investigation(id.clone())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("investigation {} not found", id),
            )
        })?;
    let tool_calls = state
        .store
        .list_tool_calls(id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(InvestigationDetailResponse {
        investigation: inv,
        tool_calls,
    }))
}

async fn list_tool_calls_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .store
        .list_tool_calls(id)
        .await
        .map(|tc| Json(serde_json::json!({ "tool_calls": tc })))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn complete_investigation_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<CompleteInvestigationBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .store
        .complete_investigation(
            id,
            body.status,
            body.summary,
            body.proposal_json,
            body.tokens_used,
            body.cost_usd,
        )
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ── T4-5 — Governance state endpoint ─────────────────────────────────────────

async fn governance_state_handler(State(state): State<AppState>) -> impl IntoResponse {
    match &state.governor {
        Some(g) => (StatusCode::OK, Json(serde_json::json!(g.snapshot()))).into_response(),
        None => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "governance_not_started"})),
        )
            .into_response(),
    }
}

// ── T5-2 — Grounded incident response ────────────────────────────────────────

async fn grounded_incident_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<crate::mcp_server::GroundedIncidentResponse>, (StatusCode, String)> {
    let detections = state
        .store
        .read_detections(500)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let det = detections.into_iter().find(|d| d.id == id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("DetectionEvent {id} not found"),
        )
    })?;

    let device_address = det.device_address.clone();
    let db = state.store.db();
    let blast = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::queries::blast_radius(&conn, &device_address, 2).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let meta = crate::mcp_server::rule_meta(&det.rule_id);
    let refs = crate::mcp_server::procedural_refs(&det.device_address, &det.rule_id);

    Ok(Json(crate::mcp_server::GroundedIncidentResponse {
        detection: crate::mcp_server::DetectionSummary {
            id: det.id,
            device_address: det.device_address,
            rule_id: det.rule_id,
            severity: det.severity,
            fired_at_ns: det.fired_at_ns,
            features_json: det.features_json,
            remediation_status: det.remediation_status,
            remediation_action: det.remediation_action,
        },
        blast_radius: blast,
        rule_description: meta.map(|m| m.description).unwrap_or(""),
        recurrence_indicators: meta.map(|m| m.recurrence_indicators).unwrap_or(&[]),
        procedural_references: refs,
    }))
}

// ── T5-3 — Self-describing OpenAPI schema endpoint ───────────────────────────

async fn schema_handler() -> Json<serde_json::Value> {
    Json(openapi_schema())
}

async fn openapi_json_handler() -> Json<serde_json::Value> {
    Json(openapi_schema())
}

async fn swagger_ui_handler() -> axum::response::Html<&'static str> {
    axum::response::Html(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8"/>
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Bonsai API — Swagger UI</title>
  <link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist@5/swagger-ui.css">
  <style>body { margin: 0; } .topbar { display: none; }</style>
</head>
<body>
<div id="swagger-ui"></div>
<script src="https://unpkg.com/swagger-ui-dist@5/swagger-ui-bundle.js"></script>
<script>
window.onload = () => {
  SwaggerUIBundle({
    url: "/api/openapi.json",
    dom_id: "#swagger-ui",
    presets: [SwaggerUIBundle.presets.apis, SwaggerUIBundle.SwaggerUIStandalonePreset],
    layout: "BaseLayout",
    deepLinking: true,
    tryItOutEnabled: true,
    filter: true,
    docExpansion: "none",
    defaultModelsExpandDepth: 2,
  });
};
</script>
</body>
</html>"##,
    )
}

// ── T5-5 — Reference resolution endpoint ─────────────────────────────────────

#[derive(Deserialize)]
struct ResolveParams {
    q: String,
}

async fn resolve_handler(
    State(state): State<AppState>,
    Query(params): Query<ResolveParams>,
) -> Result<Json<crate::mcp_server::ResolveResponse>, (StatusCode, String)> {
    let q = params.q.trim().to_string();
    if q.is_empty() {
        return Ok(Json(crate::mcp_server::ResolveResponse {
            query: q,
            candidates: vec![],
        }));
    }

    let mut candidates: Vec<crate::mcp_server::ResolveCandidate> = Vec::new();

    // 1. Device candidates — hostname and address substring match.
    let db = state.store.db();
    let q_clone = q.clone();
    let devices: Vec<(String, String)> = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let rows: Vec<(String, String)> = conn
            .query("MATCH (d:Device) RETURN d.address, d.hostname")
            .map_err(|e| e.to_string())?
            .map(|row| (read_str(&row[0]), read_str(&row[1])))
            .collect();
        Ok::<_, String>(rows)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    for (address, hostname) in devices {
        let score = crate::mcp_server::match_score(&hostname, &q_clone)
            .max(crate::mcp_server::match_score(&address, &q_clone));
        if score > 0.0 {
            candidates.push(crate::mcp_server::ResolveCandidate {
                kind: "device",
                id: address.clone(),
                label: if hostname.is_empty() {
                    address
                } else {
                    format!("{hostname} ({address})")
                },
                score,
            });
        }
    }

    // 2. Recent detection candidates — match against rule_id and device_address.
    let detections = state
        .store
        .read_detections(100)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    for det in &detections {
        let score = crate::mcp_server::match_score(&det.rule_id, &q)
            .max(crate::mcp_server::match_score(&det.id, &q))
            .max(crate::mcp_server::match_score(&det.device_address, &q));
        if score > 0.0 {
            candidates.push(crate::mcp_server::ResolveCandidate {
                kind: "detection",
                id: det.id.clone(),
                label: format!(
                    "{} on {} ({})",
                    det.rule_id, det.device_address, det.severity
                ),
                score,
            });
        }
    }

    // 3. Rule candidates — static catalogue, match against rule_id and description.
    for rule in crate::mcp_server::RULE_CATALOGUE {
        let score = crate::mcp_server::match_score(rule.rule_id, &q)
            .max(crate::mcp_server::match_score(rule.description, &q));
        if score > 0.0 {
            candidates.push(crate::mcp_server::ResolveCandidate {
                kind: "rule",
                id: rule.rule_id.to_string(),
                label: format!("{} — {}", rule.rule_id, rule.description),
                score,
            });
        }
    }

    // Sort by descending score, limit to top 20.
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates.truncate(20);

    Ok(Json(crate::mcp_server::ResolveResponse {
        query: q,
        candidates,
    }))
}

// ── CV7 T4-4: GET /api/sidecars ───────────────────────────────────────────────
// Surfaces the in-memory sidecar registry as JSON. Consumed by the bonpy UI
// and ops scripts. See `src/sidecar_registry.rs` and `docs/architecture/sidecars.md`.

#[derive(Serialize)]
struct SidecarsResponse {
    sidecars: Vec<crate::sidecar_registry::SidecarSnapshot>,
    required_kinds: Vec<String>,
    /// `None` while no kinds are required OR while still in the startup grace
    /// window. `Some([])` means all required kinds present. `Some([...])`
    /// means those kinds are missing or lost.
    missing_required: Option<Vec<String>>,
}

async fn sidecars_handler(State(state): State<AppState>) -> Json<SidecarsResponse> {
    let sidecars = state.sidecar_registry.snapshot().await;
    let required_kinds = state.sidecar_registry.required_kinds().await;
    let missing_required = state.sidecar_registry.missing_required().await;
    Json(SidecarsResponse {
        sidecars,
        required_kinds,
        missing_required,
    })
}

// ── CV7 T4-6: GET /health ─────────────────────────────────────────────────────
// Returns 200 + JSON `{ "status": "ok" }` by default. When a required sidecar
// is missing past the startup grace window, returns 503 + `{ "status":
// "degraded", "missing_required_sidecars": [...] }`. This is the operational
// "loud" surface that prevents the CV6-era "Detections: 0" silent gap.

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    missing_required_sidecars: Option<Vec<String>>,
}

async fn health_handler(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    match state.sidecar_registry.missing_required().await {
        Some(missing) if !missing.is_empty() => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResponse {
                status: "degraded",
                missing_required_sidecars: Some(missing),
            }),
        ),
        _ => (
            StatusCode::OK,
            Json(HealthResponse {
                status: "ok",
                missing_required_sidecars: None,
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::openapi_schema;

    #[test]
    fn openapi_schema_uses_envelope_shapes_for_primary_responses() {
        let spec = openapi_schema();
        let detections_ref = &spec["paths"]["/api/detections"]["get"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["$ref"];
        assert_eq!(
            detections_ref.as_str(),
            Some("#/components/schemas/DetectionsResponse")
        );

        let topology_ref = &spec["paths"]["/api/topology"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"];
        assert_eq!(
            topology_ref.as_str(),
            Some("#/components/schemas/TopologyResponse")
        );
    }

    #[test]
    fn openapi_schema_includes_examples_and_schema_version_fields() {
        let spec = openapi_schema();
        assert!(
            spec["paths"]["/api/operations"]["get"]["responses"]["200"]["content"]
                ["application/json"]["example"]
                .is_object()
        );
        assert!(
            spec["components"]["schemas"]["OperationsResponse"]["properties"]["_schema_version"]
                .is_object()
        );
        assert!(
            spec["components"]["schemas"]["DetectionsResponse"]["properties"]["_schema_version"]
                .is_object()
        );
        assert_eq!(
            spec["paths"]["/api/profiles"]["get"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["$ref"]
                .as_str(),
            Some("#/components/schemas/ProfilesResponse")
        );
        assert_eq!(
            spec["paths"]["/api/setup/status"]["get"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["$ref"]
                .as_str(),
            Some("#/components/schemas/SetupStatusResponse")
        );
    }
}
