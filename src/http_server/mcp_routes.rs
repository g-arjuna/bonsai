use axum::{Json, extract::{Query, State}, http::StatusCode};
use lbug::Connection;
use serde::Deserialize;

use super::{AppState, read_str};

#[derive(Deserialize)]
pub(super) struct ResolveParams {
    q: String,
}

pub(super) async fn schema_handler() -> Json<serde_json::Value> {
    Json(super::schema::openapi_schema())
}
pub(super) async fn openapi_json_handler() -> Json<serde_json::Value> {
    Json(super::schema::openapi_schema())
}
pub(super) async fn swagger_ui_handler() -> axum::response::Html<&'static str> {
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
pub(super) async fn resolve_handler(
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
