//! BN-014a — baseline/candidate state capture for change windows.
//!
//! Read-only pulls from the routing service (`/internal/routing/v1/routes`,
//! `/peers`) per affected CI, persisted as `ConfigItem` snapshots under
//! config_class `change_state_snapshot`. No device writes.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use lbug::{Connection, Value};
use serde::Deserialize;

use crate::graph::ConfigItemRecord;

use super::AppState;

const SNAPSHOT_CLASS: &str = "change_state_snapshot";
const MAX_DEVICES: usize = 20;
const MAX_PREFIXES: usize = 5000;

#[derive(Deserialize)]
pub(super) struct CaptureBody {
    /// "baseline" | "candidate"
    phase: String,
}

async fn routing_get(
    state: &AppState,
    path: &str,
) -> Result<serde_json::Value, String> {
    let (base, token) = super::resolve_routing_service_endpoint(&state.store).await;
    if base.is_empty() {
        return Err("routing service URL not configured".to_string());
    }
    let url = format!("{base}{path}");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.get(&url);
    if let Some(t) = token {
        req = req.header("x-bonsai-routing-token", t);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("routing service {}", resp.status()));
    }
    resp.json().await.map_err(|e| e.to_string())
}

fn extract_routes(json: &serde_json::Value) -> Vec<serde_json::Value> {
    if let Some(arr) = json["routes"].as_array() {
        return arr.clone();
    }
    if let Some(arr) = json.as_array() {
        return arr.clone();
    }
    Vec::new()
}

fn prefix_of(route: &serde_json::Value) -> Option<String> {
    for k in ["prefix", "nlri", "prefix_text", "destination"] {
        if let Some(s) = route[k].as_str() {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

async fn read_change_cis(
    state: &AppState,
    change_id: &str,
) -> Result<(String, String, Vec<String>), (StatusCode, String)> {
    let db = state.store.db();
    let cid = change_id.to_string();
    let row = tokio::task::spawn_blocking(move || -> Result<
        (String, String, Vec<String>),
        String,
    > {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "MATCH (c:ChangeRequest {id: $cid}) \
                 RETURN c.number, c.state, c.affected_cis_json",
            )
            .map_err(|e| e.to_string())?;
        let mut result = conn
            .execute(
                &mut stmt,
                vec![("cid", Value::String(cid.clone()))],
            )
            .map_err(|e| e.to_string())?;
        match result.next() {
            Some(row) => {
                let number = crate::graph::common::read_str(&row[0]);
                let st = crate::graph::common::read_str(&row[1]);
                let cis_json = crate::graph::common::read_str(&row[2]);
                let cis: Vec<String> =
                    serde_json::from_str(&cis_json).unwrap_or_default();
                Ok((number, st, cis))
            }
            _ => Err(format!("change request {cid} not found")),
        }
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    Ok(row)
}

/// POST /api/changes/{id}/capture — snapshot affected-device RIB/peer state.
pub(super) async fn capture_change_snapshot_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<CaptureBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let phase = body.phase.trim().to_lowercase();
    if phase != "baseline" && phase != "candidate" {
        return Err((
            StatusCode::BAD_REQUEST,
            "phase must be 'baseline' or 'candidate'".to_string(),
        ));
    }
    let (number, _state, cis) = read_change_cis(&state, &id).await?;
    if cis.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("change {number} has no affected_cis"),
        ));
    }

    let mut captured = Vec::new();
    let mut errors = Vec::new();
    for dev in cis.iter().take(MAX_DEVICES) {
        let dev = dev.trim();
        if dev.is_empty() {
            continue;
        }
        let routes_path = format!(
            "/internal/routing/v1/routes?speaker={}&limit=1000",
            urlencoding(dev)
        );
        let peers_path = format!(
            "/internal/routing/v1/peers?speaker={}&limit=500",
            urlencoding(dev)
        );
        let routes = routing_get(&state, &routes_path).await;
        let peers = routing_get(&state, &peers_path).await;

        let (route_count, prefixes, routes_ok, routes_err) = match &routes {
            Ok(v) => {
                let rs = extract_routes(v);
                let prefs: Vec<String> = rs
                    .iter()
                    .filter_map(prefix_of)
                    .take(MAX_PREFIXES)
                    .collect();
                (rs.len(), prefs, true, None)
            }
            Err(e) => (0, Vec::new(), false, Some(e.clone())),
        };
        let (peer_count, peer_states, peers_ok, peers_err) = match &peers {
            Ok(v) => {
                let arr = v["peers"].as_array().cloned().unwrap_or_else(|| {
                    v.as_array().cloned().unwrap_or_default()
                });
                let mut states: std::collections::BTreeMap<String, usize> =
                    std::collections::BTreeMap::new();
                for p in &arr {
                    let st = p["state"]
                        .as_str()
                        .or_else(|| p["session_state"].as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    *states.entry(st).or_default() += 1;
                }
                (arr.len(), states, true, None)
            }
            Err(e) => (
                0,
                std::collections::BTreeMap::new(),
                false,
                Some(e.clone()),
            ),
        };

        let snapshot = serde_json::json!({
            "change_id": id,
            "change_number": number,
            "device": dev,
            "phase": phase,
            "captured_at_ns": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0),
            "routes_ok": routes_ok,
            "peers_ok": peers_ok,
            "route_count": route_count,
            "prefixes": prefixes,
            "peer_count": peer_count,
            "peer_states": peer_states,
            "routes_error": routes_err,
            "peers_error": peers_err,
        });
        let safe_dev: String = dev
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == ':' { c } else { '_' })
            .collect();
        let item_id = format!("css-{}-{}-{}", id, safe_dev, phase);
        let item = ConfigItemRecord {
            id: item_id.clone(),
            config_class: SNAPSHOT_CLASS.to_string(),
            vendor: String::new(),
            name: format!("{}:{}:{}", id, dev, phase),
            version: String::new(),
            content_json: serde_json::to_string(&snapshot)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
            enabled: true,
            created_by: "bn014a".to_string(),
        };
        state
            .store
            .upsert_config_item(item)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        captured.push(serde_json::json!({
            "device": dev,
            "snapshot_id": item_id,
            "route_count": route_count,
            "peer_count": peer_count,
            "routes_ok": routes_ok,
            "peers_ok": peers_ok,
        }));
        if !routes_ok || !peers_ok {
            errors.push(serde_json::json!({
                "device": dev,
                "routes_error": routes_err,
                "peers_error": peers_err,
            }));
        }
    }

    Ok(Json(serde_json::json!({
        "change_id": id,
        "phase": phase,
        "captured": captured,
        "errors": errors,
    })))
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-._~".contains(c) {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}

/// GET /api/changes/{id}/snapshots — list captures + baseline↔candidate diff.
pub(super) async fn list_change_snapshots_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let items = state
        .store
        .list_config_items(Some(SNAPSHOT_CLASS.to_string()))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

    let mut snaps: Vec<serde_json::Value> = Vec::new();
    for it in items {
        if !it.name.starts_with(&format!("{}:", id)) {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&it.content_json) {
            snaps.push(v);
        }
    }

    // Pair baseline/candidate per device and diff.
    let mut devices: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    for s in &snaps {
        let dev = s["device"].as_str().unwrap_or("").to_string();
        let phase = s["phase"].as_str().unwrap_or("").to_string();
        let entry = devices.entry(dev.clone()).or_insert_with(|| {
            serde_json::json!({"device": dev, "baseline": null, "candidate": null})
        });
        entry[phase] = s.clone();
    }

    let mut diffs = Vec::new();
    for (_dev, d) in &devices {
        let (b, c) = (&d["baseline"], &d["candidate"]);
        let mut diff = serde_json::json!({"device": d["device"]});
        if b.is_null() || c.is_null() {
            diff["status"] = serde_json::json!("incomplete_pair");
        } else {
            let bp: std::collections::BTreeSet<String> = b["prefixes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let cp: std::collections::BTreeSet<String> = c["prefixes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let added: Vec<&String> = cp.difference(&bp).collect();
            let removed: Vec<&String> = bp.difference(&cp).collect();
            diff["status"] = serde_json::json!("diffed");
            diff["route_count_delta"] = serde_json::json!(
                c["route_count"].as_u64().unwrap_or(0) as i64
                    - b["route_count"].as_u64().unwrap_or(0) as i64
            );
            diff["prefixes_added"] = serde_json::json!(added);
            diff["prefixes_removed"] = serde_json::json!(removed);
            diff["peer_count_delta"] = serde_json::json!(
                c["peer_count"].as_u64().unwrap_or(0) as i64
                    - b["peer_count"].as_u64().unwrap_or(0) as i64
            );
        }
        diffs.push(diff);
    }

    Ok(Json(serde_json::json!({
        "change_id": id,
        "snapshots": snaps,
        "diffs": diffs,
    })))
}
