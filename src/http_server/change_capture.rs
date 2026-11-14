//! BN-014a — baseline/candidate state capture for change windows.
//!
//! Read-only pulls from the routing service (`/internal/routing/v1/routes`,
//! `/peers`) per affected CI, persisted as `ConfigItem` snapshots under
//! config_class `change_state_snapshot`. No device writes.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use lbug::{Connection, Value};
use serde::Deserialize;

use crate::graph::ConfigItemRecord;

use super::AppState;

const SNAPSHOT_CLASS: &str = "change_state_snapshot";
const VERDICT_CLASS: &str = "change_invariant_verdict";
const MAX_DEVICES: usize = 20;
const MAX_PREFIXES: usize = 5000;

#[derive(Deserialize)]
pub(super) struct CaptureBody {
    /// "baseline" | "candidate"
    phase: String,
}

async fn routing_get(state: &AppState, path: &str) -> Result<serde_json::Value, String> {
    let (base, token) = super::resolve_routing_service_endpoint(&state.store).await?;
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
    let row =
        tokio::task::spawn_blocking(move || -> Result<(String, String, Vec<String>), String> {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            let mut stmt = conn
                .prepare(
                    "MATCH (c:ChangeRequest {id: $cid}) \
                 RETURN c.number, c.state, c.affected_cis_json",
                )
                .map_err(|e| e.to_string())?;
            let mut result = conn
                .execute(&mut stmt, vec![("cid", Value::String(cid.clone()))])
                .map_err(|e| e.to_string())?;
            match result.next() {
                Some(row) => {
                    let number = crate::graph::common::read_str(&row[0]);
                    let st = crate::graph::common::read_str(&row[1]);
                    let cis_json = crate::graph::common::read_str(&row[2]);
                    let cis: Vec<String> = serde_json::from_str(&cis_json).unwrap_or_default();
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
                let prefs: Vec<String> =
                    rs.iter().filter_map(prefix_of).take(MAX_PREFIXES).collect();
                (rs.len(), prefs, true, None)
            }
            Err(e) => (0, Vec::new(), false, Some(e.clone())),
        };
        let (peer_count, peer_states, peers_ok, peers_err) = match &peers {
            Ok(v) => {
                let arr = v["peers"]
                    .as_array()
                    .cloned()
                    .unwrap_or_else(|| v.as_array().cloned().unwrap_or_default());
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
            Err(e) => (0, std::collections::BTreeMap::new(), false, Some(e.clone())),
        };

        let captured_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let snapshot = serde_json::json!({
            "change_id": id,
            "change_number": number,
            "device": dev,
            "phase": phase,
            "captured_at_ns": captured_at,
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
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == ':' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        // Append-only: each capture gets a unique id so a later capture
        // (e.g. post-recovery) never overwrites an earlier one.
        let item_id = format!("css-{}-{}-{}-{}", id, safe_dev, phase, captured_at);
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

/// Load persisted `change_state_snapshot` ConfigItems for a change and
/// return their parsed snapshot JSON values.
async fn load_change_snapshots(
    state: &AppState,
    change_id: &str,
) -> Result<Vec<serde_json::Value>, (StatusCode, String)> {
    let items = state
        .store
        .list_config_items(Some(SNAPSHOT_CLASS.to_string()))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let prefix = format!("{}:", change_id);
    let mut snaps = Vec::new();
    for it in items {
        if !it.name.starts_with(&prefix) {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&it.content_json) {
            snaps.push(v);
        }
    }
    Ok(snaps)
}

/// GET /api/changes/{id}/snapshots — list captures + baseline↔candidate diff.
pub(super) async fn list_change_snapshots_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let snaps = load_change_snapshots(&state, &id).await?;

    // Pair baseline/candidate per device and diff.
    let mut devices: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    for s in &snaps {
        let dev = s["device"].as_str().unwrap_or("").to_string();
        let phase = s["phase"].as_str().unwrap_or("").to_string();
        let entry = devices.entry(dev.clone()).or_insert_with(
            || serde_json::json!({"device": dev, "baseline": null, "candidate": null}),
        );
        // Keep the newest snapshot per phase (append-only history).
        let cur_ts = entry[phase.as_str()]["captured_at_ns"]
            .as_i64()
            .unwrap_or(0);
        if entry[phase.as_str()].is_null() || cur_ts < s["captured_at_ns"].as_i64().unwrap_or(0) {
            entry[phase.as_str()] = s.clone();
        }
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

// ── BN-014b: independent invariant verification ─────────────────────────────
//
// Verdicts are computed ONLY from the persisted baseline/candidate snapshot
// payloads — never from the change system's own report. Each invariant is
// explicit: pass / fail / unknown / skipped, with a human-readable detail.

#[derive(Deserialize)]
pub(super) struct VerifyBody {
    /// Prefixes that must still be present in every candidate snapshot.
    #[serde(default)]
    expected_prefixes: Vec<String>,
}

fn snapshot_prefixes(snap: &serde_json::Value) -> std::collections::BTreeSet<String> {
    snap["prefixes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Count peers whose state string contains `needle` (case-insensitive).
fn peer_state_count(snap: &serde_json::Value, needle: &str) -> usize {
    snap["peer_states"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter(|(k, _)| k.to_lowercase().contains(needle))
                .map(|(_, v)| v.as_u64().unwrap_or(0) as usize)
                .sum()
        })
        .unwrap_or(0)
}

fn verdict(invariant: &str, verdict: &str, detail: String) -> serde_json::Value {
    serde_json::json!({"invariant": invariant, "verdict": verdict, "detail": detail})
}

/// Evaluate the independent post-change invariants for one device from its
/// baseline and candidate snapshots. `expected_prefixes` is operator-supplied;
/// when empty the expected-prefix invariant is reported as `skipped`.
fn evaluate_device_invariants(
    baseline: &serde_json::Value,
    candidate: &serde_json::Value,
    expected_prefixes: &[String],
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();

    // 1. Capture completeness — both phases must have succeeded end to end.
    let complete = baseline["routes_ok"].as_bool().unwrap_or(false)
        && baseline["peers_ok"].as_bool().unwrap_or(false)
        && candidate["routes_ok"].as_bool().unwrap_or(false)
        && candidate["peers_ok"].as_bool().unwrap_or(false);
    out.push(if complete {
        verdict(
            "capture_complete",
            "pass",
            "routes and peers captured ok in both phases".into(),
        )
    } else {
        verdict(
            "capture_complete",
            "fail",
            format!(
                "baseline routes_ok={} peers_ok={}, candidate routes_ok={} peers_ok={}",
                baseline["routes_ok"],
                baseline["peers_ok"],
                candidate["routes_ok"],
                candidate["peers_ok"]
            ),
        )
    });

    // 2. No new peer-down states in candidate vs baseline.
    let b_down = peer_state_count(baseline, "down");
    let c_down = peer_state_count(candidate, "down");
    out.push(if c_down <= b_down {
        verdict(
            "no_new_peer_downs",
            "pass",
            format!("down peers baseline={b_down} candidate={c_down}"),
        )
    } else {
        verdict(
            "no_new_peer_downs",
            "fail",
            format!("down peers increased baseline={b_down} candidate={c_down}"),
        )
    });

    // 3. Prefix retention — every baseline prefix must survive in candidate.
    let bp = snapshot_prefixes(baseline);
    let cp = snapshot_prefixes(candidate);
    let removed: Vec<&String> = bp.difference(&cp).collect();
    out.push(if removed.is_empty() {
        verdict(
            "prefix_retention",
            "pass",
            format!("all {} baseline prefixes retained", bp.len()),
        )
    } else {
        let shown: Vec<String> = removed.iter().take(10).map(|s| s.to_string()).collect();
        verdict(
            "prefix_retention",
            "fail",
            format!(
                "{} baseline prefixes missing in candidate: {}",
                removed.len(),
                shown.join(", ")
            ),
        )
    });

    // 4. Operator-declared expected prefixes must be present in candidate.
    if expected_prefixes.is_empty() {
        out.push(verdict(
            "expected_prefixes_present",
            "skipped",
            "no expected_prefixes supplied".into(),
        ));
    } else {
        let missing: Vec<&String> = expected_prefixes
            .iter()
            .filter(|p| !cp.contains(p.as_str()))
            .collect();
        out.push(if missing.is_empty() {
            verdict(
                "expected_prefixes_present",
                "pass",
                format!("all {} expected prefixes present", expected_prefixes.len()),
            )
        } else {
            let shown: Vec<String> = missing.iter().take(10).map(|s| s.to_string()).collect();
            verdict(
                "expected_prefixes_present",
                "fail",
                format!(
                    "{} expected prefixes missing: {}",
                    missing.len(),
                    shown.join(", ")
                ),
            )
        });
    }

    out
}

fn overall_verdict(verdicts: &[serde_json::Value]) -> &'static str {
    let mut saw_unknown = false;
    for v in verdicts {
        match v["verdict"].as_str().unwrap_or("") {
            "fail" => return "fail",
            "unknown" => saw_unknown = true,
            _ => {}
        }
    }
    if saw_unknown { "unknown" } else { "pass" }
}

fn safe_device(dev: &str) -> String {
    dev.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == ':' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// POST /api/changes/{id}/verify-invariants — evaluate independent
/// post-change invariants over persisted baseline/candidate snapshots and
/// persist per-device verdicts as `change_invariant_verdict` ConfigItems.
/// Optional body: {"expected_prefixes": ["10.0.0.0/24", ...]}.
pub(super) async fn verify_change_invariants_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<VerifyBody>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let expected = body.map(|Json(b)| b.expected_prefixes).unwrap_or_default();
    let snaps = load_change_snapshots(&state, &id).await?;
    if snaps.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no snapshots found for change {id}"),
        ));
    }

    // Pair baseline/candidate per device (same shape as the diff view).
    let mut devices: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    for s in &snaps {
        let dev = s["device"].as_str().unwrap_or("").to_string();
        let phase = s["phase"].as_str().unwrap_or("").to_string();
        let entry = devices.entry(dev.clone()).or_insert_with(
            || serde_json::json!({"device": dev, "baseline": null, "candidate": null}),
        );
        // Keep the newest snapshot per phase (append-only history).
        let cur_ts = entry[phase.as_str()]["captured_at_ns"]
            .as_i64()
            .unwrap_or(0);
        if entry[phase.as_str()].is_null() || cur_ts < s["captured_at_ns"].as_i64().unwrap_or(0) {
            entry[phase.as_str()] = s.clone();
        }
    }

    let mut results = Vec::new();
    let mut overall = "pass";
    for (dev, d) in &devices {
        let (b, c) = (&d["baseline"], &d["candidate"]);
        let (verdicts, pair_status) = if b.is_null() || c.is_null() {
            (
                vec![verdict(
                    "snapshot_pair",
                    "unknown",
                    "incomplete baseline/candidate pair".into(),
                )],
                "incomplete_pair",
            )
        } else {
            (evaluate_device_invariants(b, c, &expected), "evaluated")
        };
        let dev_overall = if pair_status == "incomplete_pair" {
            "unknown"
        } else {
            overall_verdict(&verdicts)
        };
        if dev_overall == "fail" {
            overall = "fail";
        } else if dev_overall == "unknown" && overall != "fail" {
            overall = "unknown";
        }

        let evaluated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let record = serde_json::json!({
            "change_id": id,
            "device": dev,
            "pair_status": pair_status,
            "overall": dev_overall,
            "expected_prefixes": expected,
            "verdicts": verdicts,
            "evaluated_at_ns": evaluated_at,
        });
        let safe = safe_device(dev);
        // Append-only: each evaluation gets a unique id so a later pass
        // never overwrites an earlier fail verdict.
        let item = ConfigItemRecord {
            id: format!("civ-{}-{}-{}", id, safe, evaluated_at),
            config_class: VERDICT_CLASS.to_string(),
            vendor: String::new(),
            name: format!("{}:{}", id, dev),
            version: String::new(),
            content_json: serde_json::to_string(&record)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
            enabled: true,
            created_by: "bn014b".to_string(),
        };
        state
            .store
            .upsert_config_item(item)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

        results.push(serde_json::json!({
            "device": dev,
            "pair_status": pair_status,
            "overall": dev_overall,
            "verdicts": record["verdicts"],
        }));
    }

    Ok(Json(serde_json::json!({
        "change_id": id,
        "overall": overall,
        "devices": results,
    })))
}

/// GET /api/changes/{id}/invariants — return persisted verdict records.
pub(super) async fn list_change_invariants_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let items = state
        .store
        .list_config_items(Some(VERDICT_CLASS.to_string()))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let prefix = format!("{}:", id);
    let mut verdicts = Vec::new();
    for it in items {
        if !it.name.starts_with(&prefix) {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&it.content_json) {
            verdicts.push(v);
        }
    }
    Ok(Json(serde_json::json!({
        "change_id": id,
        "verdicts": verdicts,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(
        phase: &str,
        routes_ok: bool,
        peers_ok: bool,
        prefixes: &[&str],
        peer_states: &[(&str, u64)],
    ) -> serde_json::Value {
        serde_json::json!({
            "phase": phase,
            "device": "10.0.0.1",
            "routes_ok": routes_ok,
            "peers_ok": peers_ok,
            "route_count": prefixes.len(),
            "peer_count": peer_states.iter().map(|(_, n)| n).sum::<u64>(),
            "prefixes": prefixes,
            "peer_states": peer_states.iter().map(|(k, v)| (k.to_string(), serde_json::json!(v))).collect::<serde_json::Map<_, _>>(),
        })
    }

    fn verdicts_map(v: &[serde_json::Value]) -> std::collections::HashMap<String, String> {
        v.iter()
            .map(|x| {
                (
                    x["invariant"].as_str().unwrap_or("").to_string(),
                    x["verdict"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn identical_snapshots_pass_all() {
        let b = snap("baseline", true, true, &["10.0.0.0/24"], &[("up", 2)]);
        let c = snap("candidate", true, true, &["10.0.0.0/24"], &[("up", 2)]);
        let v = evaluate_device_invariants(&b, &c, &[]);
        let m = verdicts_map(&v);
        assert_eq!(m["capture_complete"], "pass");
        assert_eq!(m["no_new_peer_downs"], "pass");
        assert_eq!(m["prefix_retention"], "pass");
        assert_eq!(m["expected_prefixes_present"], "skipped");
        assert_eq!(overall_verdict(&v), "pass");
    }

    #[test]
    fn prefix_loss_and_new_down_fail() {
        let b = snap(
            "baseline",
            true,
            true,
            &["10.0.0.0/24", "10.1.0.0/24"],
            &[("up", 2)],
        );
        let c = snap(
            "candidate",
            true,
            true,
            &["10.0.0.0/24"],
            &[("up", 1), ("down", 1)],
        );
        let v = evaluate_device_invariants(&b, &c, &["10.9.9.0/24".to_string()]);
        let m = verdicts_map(&v);
        assert_eq!(m["prefix_retention"], "fail");
        assert_eq!(m["no_new_peer_downs"], "fail");
        assert_eq!(m["expected_prefixes_present"], "fail");
        assert_eq!(overall_verdict(&v), "fail");
    }

    #[test]
    fn incomplete_capture_fails() {
        let b = snap("baseline", true, true, &["10.0.0.0/24"], &[("up", 1)]);
        let c = snap("candidate", false, true, &["10.0.0.0/24"], &[("up", 1)]);
        let v = evaluate_device_invariants(&b, &c, &[]);
        let m = verdicts_map(&v);
        assert_eq!(m["capture_complete"], "fail");
        assert_eq!(overall_verdict(&v), "fail");
    }
}
