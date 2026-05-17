use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use axum::{
    Json, extract::{Path, Query, State}, http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
};
use futures::stream::{Stream, StreamExt};
use lbug::{Connection, Value};
use serde_json;
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};

use super::AppState;
use super::{
    TopologyResponse, DeviceJson, BgpJson, LinkJson, PathResponse, PathParams,
    BlastRadiusParams, DetectionsResponse, DetectionsParams, TraceResponse,
    ReadinessResponse, OperationsResponse, BudgetBreach, TestStatusResponse, DiskStatusJson,
    DailyCheckResponse, DailyCheckCounts, DailyCheckItem,
    WeeklyTrendDay, WeeklyTrendResponse,
    IncidentJson, IncidentsResponse, IncidentsParams,
    SsePayload, EmbeddingsResponse, UpsertEmbeddingsBody,
    ExplorerQueryBody,
    read_str, read_i64, read_ts_ns, read_subscription_statuses, read_trust_mark_impact,
    option_string, now_ns, build_site_path_by_id, resolve_site_metadata, compute_health,
    CreateSavedQueryBody,
    API_SCHEMA_VERSION, RSS_BUDGET_BYTES, COORDINATOR_QUEUE_BUDGET_PCT,
};
use crate::graph::{DetectionRow, GraphStore, SiteRecord, TraceStep, REMEDIATION_TRUST_CUTOFF_ISO};
use crate::registry::{ApiRegistry, DeviceRegistry, RegistryChange};
use crate::config::{StorageConfig, TargetConfig};
use crate::{event_bus, disk_guard, memory_profile, archive, streaming};

pub(super) async fn topology_handler(
    State(state): State<AppState>,
) -> Result<Json<TopologyResponse>, (StatusCode, String)> {
    let db = state.store.db();

    let (devices_raw, links_raw, bgp_raw) = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;

        // Devices
        let dev_rows = conn
            .query("MATCH (d:Device) RETURN d.address, d.vendor, d.hostname")
            .map_err(|e| e.to_string())?;
        let devices_raw: Vec<(String, String, String)> = dev_rows
            .map(|row| (read_str(&row[0]), read_str(&row[1]), read_str(&row[2])))
            .collect();

        // LLDP fabric links with interface counter totals for heatmap
        let link_rows = conn
            .query(
                "MATCH (a:Interface)-[:CONNECTED_TO]->(b:Interface) \
                 RETURN a.device_address, a.name, b.device_address, b.name, \
                        a.in_octets + a.out_octets + b.in_octets + b.out_octets",
            )
            .map_err(|e| e.to_string())?;
        let links_raw: Vec<(String, String, String, String, i64, bool)> = link_rows
            .map(|row| {
                (
                    read_str(&row[0]),
                    read_str(&row[1]),
                    read_str(&row[2]),
                    read_str(&row[3]),
                    read_i64(&row[4]),
                    false,
                )
            })
            .collect();

        // Management-plane LLDP links (out-of-band; hidden by default in UI)
        let mgmt_rows = conn
            .query(
                "MATCH (a:Interface)-[:MGMT_LINK]->(b:Interface) \
                 RETURN a.device_address, a.name, b.device_address, b.name",
            )
            .map_err(|e| e.to_string())?;
        let mgmt_raw: Vec<(String, String, String, String, i64, bool)> = mgmt_rows
            .map(|row| {
                (
                    read_str(&row[0]),
                    read_str(&row[1]),
                    read_str(&row[2]),
                    read_str(&row[3]),
                    0i64,
                    true,
                )
            })
            .collect();
        let links_raw: Vec<(String, String, String, String, i64, bool)> =
            links_raw.into_iter().chain(mgmt_raw).collect();

        // BGP neighbors
        let bgp_rows = conn
            .query(
                "MATCH (n:BgpNeighbor) \
                 RETURN n.device_address, n.peer_address, n.session_state, n.peer_as",
            )
            .map_err(|e| e.to_string())?;
        let bgp_raw: Vec<(String, String, String, i64)> = bgp_rows
            .map(|row| {
                (
                    read_str(&row[0]),
                    read_str(&row[1]),
                    read_str(&row[2]),
                    read_i64(&row[3]),
                )
            })
            .collect();

        Ok::<_, String>((devices_raw, links_raw, bgp_raw))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let all_sites = state.store.list_sites().await.unwrap_or_else(|error| {
        tracing::warn!(%error, "failed to list sites for topology metadata");
        Vec::new()
    });
    let site_id_by_name: HashMap<String, String> = all_sites
        .iter()
        .map(|site| (site.name.clone(), site.id.clone()))
        .collect();
    let site_path_by_id = build_site_path_by_id(&all_sites);

    // Build role + site map from registry
    let mut role_site: HashMap<String, (String, String, String, String)> = HashMap::new();
    if let Ok(targets) = state.registry.list_all_targets() {
        for t in targets {
            let site = t.site.unwrap_or_default();
            let (site_id, site_path) =
                resolve_site_metadata(&site, &site_id_by_name, &site_path_by_id);
            role_site.insert(
                t.address.clone(),
                (t.role.unwrap_or_default(), site, site_id, site_path),
            );
        }
    }

    // Group BGP by device
    let mut bgp_by_device: HashMap<String, Vec<BgpJson>> = HashMap::new();
    for (dev, peer, st, peer_as) in bgp_raw {
        bgp_by_device.entry(dev).or_default().push(BgpJson {
            peer,
            state: st,
            peer_as,
        });
    }

    // Build device list with computed health + registry metadata
    let devices: Vec<DeviceJson> = devices_raw
        .into_iter()
        .map(|(address, vendor, hostname)| {
            let bgp = bgp_by_device.remove(&address).unwrap_or_default();
            let health = compute_health(&bgp);
            let (role, site, site_id, site_path) = role_site.remove(&address).unwrap_or_default();
            DeviceJson {
                address,
                hostname,
                vendor,
                role,
                site,
                site_id,
                site_path,
                health,
                bgp,
            }
        })
        .collect();

    let links = links_raw
        .into_iter()
        .map(
            |(src_device, src_iface, dst_device, dst_iface, bytes_total, is_mgmt)| LinkJson {
                src_device,
                src_iface,
                dst_device,
                dst_iface,
                bytes_total,
                is_mgmt,
            },
        )
        .collect();

    Ok(Json(TopologyResponse {
        schema_version: API_SCHEMA_VERSION.to_string(),
        devices,
        links,
    }))
}
/// Shortest path between two devices, computed in the graph database.
///
/// Replaces Rust-side BFS that loaded all CONNECTED_TO edges into a Vec.
/// The graph DB traverses HAS_INTERFACE|CONNECTED_TO edges with a variable-
/// length pattern and returns a single path; no edge-loading into Rust.
pub(super) async fn path_handler(
    State(state): State<AppState>,
    Query(params): Query<PathParams>,
) -> Result<Json<PathResponse>, (StatusCode, String)> {
    let db = state.store.db();
    let (src, dst) = (params.src.clone(), params.dst.clone());

    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::queries::shortest_topology_path(&conn, &src, &dst).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    match result {
        None => Ok(Json(PathResponse {
            schema_version: API_SCHEMA_VERSION.to_string(),
            hops: vec![],
            links: vec![],
        })),
        Some(path) => Ok(Json(PathResponse {
            schema_version: API_SCHEMA_VERSION.to_string(),
            hops: path.hops,
            links: path.links,
        })),
    }
}
/// Devices, applications, and active detections reachable from `address` within
/// `max_hops` physical network hops.
///
/// Example: GET /api/blast-radius/10.0.0.1?max_hops=2
pub(super) async fn blast_radius_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(params): Query<BlastRadiusParams>,
) -> Result<Json<crate::graph::queries::BlastRadiusResult>, (StatusCode, String)> {
    let db = state.store.db();
    let max_hops = params.max_hops.min(5); // cap at 5 to bound query time

    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::queries::blast_radius(&conn, &address, max_hops).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}
pub(super) async fn detections_handler(
    State(state): State<AppState>,
    Query(params): Query<DetectionsParams>,
) -> Result<Json<DetectionsResponse>, (StatusCode, String)> {
    let detections = state
        .store
        .read_detections(params.limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(DetectionsResponse {
        schema_version: API_SCHEMA_VERSION.to_string(),
        detections,
    }))
}
pub(super) async fn trace_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TraceResponse>, (StatusCode, String)> {
    let steps = state
        .store
        .read_closed_loop_trace(id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(TraceResponse {
        schema_version: API_SCHEMA_VERSION.to_string(),
        steps,
    }))
}
pub(super) async fn readiness_handler(
    State(state): State<AppState>,
) -> Result<Json<ReadinessResponse>, (StatusCode, String)> {
    let db = state.store.db();

    let readiness = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;

        let detection_rows = conn
            .query("MATCH (e:DetectionEvent) RETURN e.rule_id")
            .map_err(|e| e.to_string())?;
        let mut detection_events = 0usize;
        let mut rule_distribution: HashMap<String, usize> = HashMap::new();
        for row in detection_rows {
            detection_events += 1;
            let rule_id = read_str(&row[0]);
            if !rule_id.is_empty() {
                *rule_distribution.entry(rule_id).or_insert(0) += 1;
            }
        }

        let state_rows = conn
            .query("MATCH (e:StateChangeEvent) RETURN count(e)")
            .map_err(|e| e.to_string())?;
        let mut state_change_events = 0usize;
        for row in state_rows {
            state_change_events = read_i64(&row[0]).max(0) as usize;
        }

        let remediation_rows = conn
            .query(
                "MATCH (m:RemediationTrustMark)-[:TRUST_MARKS]->(r:Remediation) \
                 WHERE m.trustworthy = 1 \
                 RETURN r.action, r.status",
            )
            .map_err(|e| e.to_string())?;
        let mut remediation_rows_post_cutoff = 0usize;
        let mut action_distribution_post_cutoff: HashMap<String, usize> = HashMap::new();
        let mut status_distribution_post_cutoff: HashMap<String, usize> = HashMap::new();
        for row in remediation_rows {
            remediation_rows_post_cutoff += 1;

            let action = read_str(&row[0]);
            if !action.is_empty() {
                *action_distribution_post_cutoff.entry(action).or_insert(0) += 1;
            }

            let status = read_str(&row[1]);
            if !status.is_empty() {
                *status_distribution_post_cutoff.entry(status).or_insert(0) += 1;
            }
        }

        Ok::<_, String>(ReadinessResponse {
            schema_version: API_SCHEMA_VERSION.to_string(),
            detection_events,
            state_change_events,
            rule_distribution,
            cutoff_iso: REMEDIATION_TRUST_CUTOFF_ISO.to_string(),
            remediation_rows_post_cutoff,
            action_distribution_post_cutoff,
            status_distribution_post_cutoff,
        })
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(readiness))
}
pub(super) async fn operations_handler(
    State(state): State<AppState>,
) -> Result<Json<OperationsResponse>, (StatusCode, String)> {
    let readiness = readiness_handler(State(state.clone())).await?.0;
    let targets = state
        .registry
        .list_all_targets()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let statuses = read_subscription_statuses(state.store.db()).await?;

    let mut observed_subscriptions = 0usize;
    let mut pending_subscriptions = 0usize;
    let mut silent_subscriptions = 0usize;
    for rows in statuses.values() {
        for status in rows {
            match status.status.as_str() {
                "observed" => observed_subscriptions += 1,
                "pending" => pending_subscriptions += 1,
                _ => silent_subscriptions += 1,
            }
        }
    }

    let collector_summary = state
        .collector_manager
        .as_ref()
        .map(|manager| manager.collector_status_summary())
        .unwrap_or_else(|| crate::assignment::CollectorStatusSummary {
            collectors: Vec::new(),
            unassigned_devices: Vec::new(),
        });
    let bus_snapshot = event_bus::InProcessBus::snapshot();
    let archive_snapshot = archive::snapshot();
    let mem_snapshot = memory_profile::snapshot();
    let disk_snapshot = disk_guard::snapshot(
        std::path::Path::new(&state.archive_path),
        std::path::Path::new(&state.graph_path),
        &state.storage_config,
    );
    // Use the governor's profile-derived budget when available; the hardcoded
    // constant is a last-resort fallback for non-governed modes.
    let effective_rss_budget = state
        .governor
        .as_ref()
        .map(|g| g.snapshot().memory_budget_mb * 1024 * 1024)
        .unwrap_or(RSS_BUDGET_BYTES);

    Ok(Json(OperationsResponse {
        schema_version: API_SCHEMA_VERSION.to_string(),
        detection_events: readiness.detection_events,
        state_change_events: readiness.state_change_events,
        remediation_rows_post_cutoff: readiness.remediation_rows_post_cutoff,
        rule_distribution: readiness.rule_distribution,
        action_distribution_post_cutoff: readiness.action_distribution_post_cutoff,
        status_distribution_post_cutoff: readiness.status_distribution_post_cutoff,
        device_count: targets.len(),
        enabled_device_count: targets.iter().filter(|target| target.enabled).count(),
        observed_subscriptions,
        pending_subscriptions,
        silent_subscriptions,
        collectors_connected: collector_summary
            .collectors
            .iter()
            .filter(|collector| collector.connected)
            .count(),
        collectors_total: collector_summary.collectors.len(),
        unassigned_devices: collector_summary.unassigned_devices.len(),
        event_bus_depth: bus_snapshot.depth,
        event_bus_receivers: bus_snapshot.receivers,
        archive_lag_millis: archive_snapshot.lag_millis,
        archive_buffer_rows: archive_snapshot.buffer_rows,
        archive_last_flush_millis: archive_snapshot.last_flush_millis,
        archive_last_compression_ppm: archive_snapshot.last_compression_ppm,
        cutoff_iso: readiness.cutoff_iso,
        rss_bytes: mem_snapshot.rss_bytes,
        archive_disk_bytes: disk_snapshot.archive_bytes,
        archive_disk_pct: disk_snapshot.archive_pct,
        graph_disk_bytes: disk_snapshot.graph_bytes,
        graph_disk_pct: disk_snapshot.graph_pct,
        memory_budget_bytes: effective_rss_budget,
        memory_rss_pct_of_budget: if effective_rss_budget > 0 {
            (mem_snapshot.rss_bytes as f64 / effective_rss_budget as f64) * 100.0
        } else {
            0.0
        },
        counter_mode: state.counter_mode.clone(),
        counter_window_secs: state.counter_window_secs,
        counter_debounce_secs: state.counter_debounce_secs,
    }))
}
pub(super) async fn test_status_handler(
    State(state): State<AppState>,
) -> Result<Json<TestStatusResponse>, (StatusCode, String)> {
    let mem = memory_profile::snapshot();
    let disk = disk_guard::snapshot(
        std::path::Path::new(&state.archive_path),
        std::path::Path::new(&state.graph_path),
        &state.storage_config,
    );

    let external_path = std::path::Path::new(&state.runtime_dir).join("external_status.json");
    let external: serde_json::Value = tokio::fs::read_to_string(&external_path)
        .await
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);

    let driver_dir = std::path::Path::new(&state.runtime_dir).join("driver_results");
    let mut driver_results = serde_json::Map::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&driver_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if let Ok(s) = tokio::fs::read_to_string(&path).await
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&s)
            {
                driver_results.insert(stem.to_string(), v);
            }
        }
    }

    let ts_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let effective_rss_budget = state
        .governor
        .as_ref()
        .map(|g| g.snapshot().memory_budget_mb * 1024 * 1024)
        .unwrap_or(RSS_BUDGET_BYTES);

    let mut budget_breaches: Vec<BudgetBreach> = Vec::new();
    if mem.rss_bytes > effective_rss_budget {
        budget_breaches.push(BudgetBreach {
            name: "rss_budget",
            current: mem.rss_bytes as f64,
            budget: effective_rss_budget as f64,
            unit: "bytes",
        });
    }
    if mem.write_coordinator_queue_pct >= COORDINATOR_QUEUE_BUDGET_PCT {
        budget_breaches.push(BudgetBreach {
            name: "write_coordinator_queue_budget",
            current: mem.write_coordinator_queue_pct as f64,
            budget: COORDINATOR_QUEUE_BUDGET_PCT as f64,
            unit: "percent",
        });
    }

    Ok(Json(TestStatusResponse {
        ts_unix,
        memory: mem,
        disk: DiskStatusJson {
            archive_bytes: disk.archive_bytes,
            archive_max_bytes: disk.archive_max_bytes,
            archive_pct: disk.archive_pct,
            graph_bytes: disk.graph_bytes,
            graph_max_bytes: disk.graph_max_bytes,
            graph_pct: disk.graph_pct,
        },
        budget_breaches,
        external,
        driver_results: serde_json::Value::Object(driver_results),
    }))
}
pub(super) async fn daily_check_handler(
    State(state): State<AppState>,
) -> Result<Json<DailyCheckResponse>, (StatusCode, String)> {
    let driver_dir = std::path::Path::new(&state.runtime_dir).join("driver_results");
    let mut checks: Vec<DailyCheckItem> = Vec::new();
    let mut counts = DailyCheckCounts {
        pass: 0,
        fail: 0,
        skip: 0,
        prereq_missing: 0,
    };

    if let Ok(mut entries) = tokio::fs::read_dir(&driver_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            // Exclude daily.json and dated copies (daily-YYYY-MM-DD.json) — they are derived
            // meta-aggregates, not individual driver results. Individual drivers write their
            // own <driver_name>.json files alongside daily.json.
            let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if fname == "daily.json" || fname.starts_with("daily-") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            let (status, summary) = if let Ok(s) = tokio::fs::read_to_string(&path).await {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                    let st = v
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let sm = v
                        .get("summary")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();
                    (st, sm)
                } else {
                    ("fail".to_string(), "invalid json".to_string())
                }
            } else {
                ("fail".to_string(), "unreadable".to_string())
            };

            match status.as_str() {
                "pass" => counts.pass += 1,
                "fail" => counts.fail += 1,
                "prereq_missing" => counts.prereq_missing += 1,
                _ => counts.skip += 1,
            }
            checks.push(DailyCheckItem {
                name,
                status,
                summary,
            });
        }
    }

    checks.sort_by(|a, b| a.name.cmp(&b.name));

    let overall = if counts.fail > 0 {
        "fail"
    } else if counts.prereq_missing > 0 {
        "pass_with_caveats"
    } else if counts.pass == 0 {
        "warn"
    } else {
        "pass"
    };

    let ts_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    Ok(Json(DailyCheckResponse {
        ts_unix,
        status: overall.to_string(),
        counts,
        checks,
    }))
}
pub(super) async fn weekly_trend_handler(State(state): State<AppState>) -> Json<WeeklyTrendResponse> {
    let driver_dir = std::path::Path::new(&state.runtime_dir).join("driver_results");
    let mut days: Vec<WeeklyTrendDay> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&driver_dir) {
        let mut files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let s = name.to_string_lossy().into_owned();
                s.starts_with("daily-") && s.ends_with(".json")
            })
            .collect();
        files.sort_by_key(|e| e.file_name());
        // Take last 7, preserving chronological order
        let start = files.len().saturating_sub(7);
        for entry in &files[start..] {
            if let Ok(contents) = std::fs::read_to_string(entry.path())
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&contents)
            {
                let date = v["environment"]["date_utc"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let status = v["status"].as_str().unwrap_or("unknown").to_string();
                let mut pass = 0u32;
                let mut fail = 0u32;
                let mut skip = 0u32;
                let mut prereq_missing = 0u32;
                if let Some(checks) = v["checks"].as_array() {
                    for c in checks {
                        match c["status"].as_str().unwrap_or("") {
                            "pass" | "pass_with_caveats" => pass += 1,
                            "fail" => fail += 1,
                            "skip" => skip += 1,
                            "prereq_missing" => prereq_missing += 1,
                            _ => {}
                        }
                    }
                }
                days.push(WeeklyTrendDay {
                    date,
                    status,
                    pass,
                    fail,
                    skip,
                    prereq_missing,
                });
            }
        }
    }

    Json(WeeklyTrendResponse { days })
}
pub(super) async fn events_handler(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.store.subscribe_events();
    let registry_rx = state.registry.subscribe_changes();

    let graph_stream = BroadcastStream::new(rx).map(|item| {
        let data = match item {
            Ok(ev) => serde_json::to_string(&SsePayload {
                device_address: ev.device_address,
                event_type: ev.event_type,
                detail_json: ev.detail_json,
                occurred_at_ns: ev.occurred_at_ns,
                state_change_event_id: ev.state_change_event_id,
            })
            .unwrap_or_default(),
            // Receiver lagged (broadcast buffer full); send a heartbeat comment.
            Err(_) => return Ok(Event::default().comment("lag")),
        };
        Ok(Event::default().data(data))
    });

    let registry_stream = ReceiverStream::new(registry_rx).map(|change| {
        let data = serde_json::to_string(&registry_change_payload(change)).unwrap_or_default();
        Ok(Event::default().data(data))
    });

    let stream = futures::stream::select(graph_stream, registry_stream);

    Sse::new(stream).keep_alive(KeepAlive::default())
}
pub(super) fn registry_change_payload(change: RegistryChange) -> SsePayload {
    match change {
        RegistryChange::Added(target) => registry_target_payload("registry_added", target),
        RegistryChange::Updated(target) => registry_target_payload("registry_updated", target),
        RegistryChange::Removed(address) => SsePayload {
            device_address: address.clone(),
            event_type: "registry_removed".to_string(),
            detail_json: serde_json::json!({ "address": address }).to_string(),
            occurred_at_ns: now_ns(),
            state_change_event_id: String::new(),
        },
    }
}
pub(super) fn registry_target_payload(event_type: &str, target: TargetConfig) -> SsePayload {
    let address = target.address.clone();
    SsePayload {
        device_address: address.clone(),
        event_type: event_type.to_string(),
        detail_json: serde_json::json!({
            "address": address,
            "enabled": target.enabled,
            "hostname": target.hostname.unwrap_or_default(),
            "vendor": target.vendor.unwrap_or_default(),
            "role": target.role.unwrap_or_default(),
            "site": target.site.unwrap_or_default(),
            "credential_alias": target.credential_alias.unwrap_or_default(),
            "selected_path_count": target.selected_paths.len(),
        })
        .to_string(),
        occurred_at_ns: now_ns(),
        state_change_event_id: String::new(),
    }
}
pub(super) async fn incidents_handler(
    State(state): State<AppState>,
    Query(params): Query<IncidentsParams>,
) -> Result<Json<IncidentsResponse>, (StatusCode, String)> {
    let detections = state
        .store
        .read_detections(params.limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Build a device-degree map from LLDP topology. Higher-degree devices are treated as
    // more "upstream" when selecting the root detection within a grouped incident.
    let db = state.store.db();
    let degree_map: HashMap<String, usize> = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let rows = conn
            .query(
                "MATCH (a:Interface)-[:CONNECTED_TO]->(:Interface) \
                 RETURN a.device_address",
            )
            .map_err(|e| e.to_string())?;
        let mut map: HashMap<String, usize> = HashMap::new();
        for row in rows {
            *map.entry(read_str(&row[0])).or_insert(0) += 1;
        }
        Ok::<_, String>(map)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .unwrap_or_default();

    let incidents = group_into_incidents(detections, params.window_secs, &degree_map);
    Ok(Json(IncidentsResponse {
        schema_version: API_SCHEMA_VERSION.to_string(),
        incidents,
    }))
}
/// Groups a list of detections into incidents by time window.
/// Root = highest-degree device (most upstream in topology) among the group;
/// tie-breaks by earliest fired_at_ns. Incidents are returned newest-first.
pub(super) fn group_into_incidents(
    mut detections: Vec<DetectionRow>,
    window_secs: u64,
    degree_map: &HashMap<String, usize>,
) -> Vec<IncidentJson> {
    detections.sort_by_key(|d| d.fired_at_ns);
    let window_ns = (window_secs as i64).saturating_mul(1_000_000_000);

    let mut groups: Vec<Vec<DetectionRow>> = Vec::new();

    for det in detections {
        let joined = groups
            .iter_mut()
            .rev()
            .find(|g| det.fired_at_ns - g[0].fired_at_ns <= window_ns);
        if let Some(group) = joined {
            group.push(det);
        } else {
            groups.push(vec![det]);
        }
    }

    let severity_rank = |s: &str| match s {
        "critical" => 3,
        "high" => 2,
        "warn" | "warning" => 1,
        _ => 0,
    };

    let mut incidents: Vec<IncidentJson> = groups
        .into_iter()
        .map(|mut group| {
            group.sort_by_key(|d| d.fired_at_ns);
            let started_at_ns = group[0].fired_at_ns;
            let ended_at_ns = group.last().map_or(started_at_ns, |d| d.fired_at_ns);

            // Pick root: highest topology degree (most upstream), then earliest time.
            let root_idx = group
                .iter()
                .enumerate()
                .max_by_key(|(_, d)| {
                    (
                        *degree_map.get(&d.device_address).unwrap_or(&0),
                        -(d.fired_at_ns),
                    )
                })
                .map(|(i, _)| i)
                .unwrap_or(0);
            let root = group.remove(root_idx);
            let id = root.id.clone();

            let severity = std::iter::once(&root)
                .chain(group.iter())
                .max_by_key(|d| severity_rank(&d.severity))
                .map_or("info".to_string(), |d| d.severity.clone());
            let remediation_status = std::iter::once(&root)
                .chain(group.iter())
                .find(|d| !d.remediation_status.is_empty())
                .map_or("none".to_string(), |d| d.remediation_status.clone());
            let mut affected_devices: Vec<String> = std::iter::once(&root)
                .chain(group.iter())
                .map(|d| d.device_address.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            affected_devices.sort();

            let event_count = 1 + group.len();
            let device_count = affected_devices.len();

            let mut rule_ids: Vec<String> = std::iter::once(&root)
                .chain(group.iter())
                .map(|d| d.rule_id.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            rule_ids.sort();

            let window_secs_actual = if event_count > 1 {
                (ended_at_ns - started_at_ns).max(0) / 1_000_000_000
            } else {
                0
            };
            let co_fire_signature = if rule_ids.len() > 1 {
                format!(
                    "{} rule types, {} device{}, {}s window",
                    rule_ids.len(),
                    device_count,
                    if device_count == 1 { "" } else { "s" },
                    window_secs_actual,
                )
            } else {
                format!(
                    "{}, {} event{}",
                    rule_ids.first().map(|s| s.as_str()).unwrap_or("unknown"),
                    event_count,
                    if event_count == 1 { "" } else { "s" },
                )
            };

            IncidentJson {
                id,
                root,
                cascading: group,
                affected_devices,
                severity,
                started_at_ns,
                ended_at_ns,
                remediation_status,
                rule_ids,
                co_fire_signature,
                device_count,
                event_count,
            }
        })
        .collect();

    incidents.sort_by_key(|incident| std::cmp::Reverse(incident.started_at_ns));
    incidents
}
pub(super) async fn graph_insights_handler(
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
pub(super) async fn explorer_query_handler(
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
pub(super) async fn list_saved_queries_handler(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::graph::SavedQueryRecord>>, (StatusCode, String)> {
    state
        .store
        .list_saved_queries()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
pub(super) async fn create_saved_query_handler(
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
pub(super) async fn delete_saved_query_handler(
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
pub(super) async fn upsert_embeddings_handler(
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
pub(super) async fn list_embeddings_handler(
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
