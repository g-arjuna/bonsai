use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};
use axum::{
    Json, extract::{Path, Query, State}, http::StatusCode,
    response::{IntoResponse, sse::{Event, KeepAlive, Sse}},
};
use serde::Deserialize;
use futures::stream::{Stream, StreamExt};
use lbug::Connection;
use serde_json;
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};

use super::AppState;
use super::{
    TopologyResponse, DeviceJson, BgpJson, IsisAdjJson, LinkJson, HostEndpointJson, PathResponse, PathParams,
    BlastRadiusParams, DetectionsResponse, DetectionsParams, TraceResponse,
    ReadinessResponse, OperationsResponse, BudgetBreach, TestStatusResponse, DiskStatusJson,
    DailyCheckResponse, DailyCheckCounts, DailyCheckItem,
    WeeklyTrendDay, WeeklyTrendResponse,
    IncidentJson, IncidentsResponse, IncidentsParams,
    CorrelationStep, BlastRadiusSummary, AffectedDeviceDetail,
    SsePayload, EmbeddingsResponse, UpsertEmbeddingsBody,
    ExplorerQueryBody,
    read_str, read_i64, read_ts_ns, read_subscription_statuses,
    now_ns, build_site_path_by_id, resolve_site_metadata, compute_health,
    CreateSavedQueryBody,
    EventsHistoryParams, EventsHistoryResponse, EventHistoryItem,
    API_SCHEMA_VERSION, RSS_BUDGET_BYTES, COORDINATOR_QUEUE_BUDGET_PCT,
};
use crate::http_server::device::InterfaceDetailJson;
use crate::graph::{DetectionRow, REMEDIATION_TRUST_CUTOFF_ISO};
use crate::registry::{DeviceRegistry, RegistryChange};
use crate::config::TargetConfig;
use crate::{event_bus, disk_guard, memory_profile, archive};

pub(super) async fn topology_handler(
    State(state): State<AppState>,
) -> Result<Json<TopologyResponse>, (StatusCode, String)> {
    let db = state.store.db();

    let (devices_raw, interfaces_raw, links_raw, bgp_raw, bgp_d2d_raw, bfd_d2d_raw, isis_d2d_raw, host_endpoints_raw, isis_raw) = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;

        // Devices
        let dev_rows = conn
            .query("MATCH (d:Device) RETURN d.address, d.vendor, d.hostname")
            .map_err(|e| e.to_string())?;
        let devices_raw: Vec<(String, String, String)> = dev_rows
            .map(|row| (read_str(&row[0]), read_str(&row[1]), read_str(&row[2])))
            .collect();

        let iface_rows = conn
            .query(
                "MATCH (i:Interface) \
                 RETURN i.device_address, i.name, i.in_errors, i.out_errors, i.in_octets, i.out_octets, \
                        i.carrier_transitions, i.updated_at",
            )
            .map_err(|e| e.to_string())?;
        let interfaces_raw: Vec<(String, InterfaceDetailJson)> = iface_rows
            .map(|row| {
                (
                    read_str(&row[0]),
                    InterfaceDetailJson {
                        name: read_str(&row[1]),
                        in_errors: read_i64(&row[2]),
                        out_errors: read_i64(&row[3]),
                        in_octets: read_i64(&row[4]),
                        out_octets: read_i64(&row[5]),
                        carrier_transitions: read_i64(&row[6]),
                        updated_at_ns: read_ts_ns(&row[7]),
                    },
                )
            })
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

        // BGP neighbors (protocol-level BgpNeighbor nodes)
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

        // BGP Device-to-Device resolved sessions (peer_device lookup for L3 topology)
        let bgp_d2d_raw: Vec<(String, String, String)> = match conn.query(
            "MATCH (a:Device)-[r:BGP_SESSION_WITH]->(b:Device) \
             RETURN a.address, b.address, r.session_state",
        ) {
            Ok(rows) => rows.map(|row| (read_str(&row[0]), read_str(&row[1]), read_str(&row[2]))).collect(),
            Err(_) => Vec::new(),
        };

        // BFD Device-to-Device resolved sessions
        let bfd_d2d_raw: Vec<(String, String, String)> = match conn.query(
            "MATCH (a:Device)-[r:BFD_PEER_WITH]->(b:Device) \
             RETURN a.address, b.address, r.session_state",
        ) {
            Ok(rows) => rows.map(|row| (read_str(&row[0]), read_str(&row[1]), read_str(&row[2]))).collect(),
            Err(_) => Vec::new(),
        };

        // ISIS Device-to-Device resolved adjacencies
        let isis_d2d_raw: Vec<(String, String, String)> = match conn.query(
            "MATCH (a:Device)-[r:ISIS_NEIGHBOR_WITH]->(b:Device) \
             RETURN a.address, b.address, r.adjacency_state",
        ) {
            Ok(rows) => rows.map(|row| (read_str(&row[0]), read_str(&row[1]), read_str(&row[2]))).collect(),
            Err(_) => Vec::new(),
        };

        // HostEndpoints and their CONNECTED_TO interface links
        let he_rows = conn
            .query(
                "MATCH (h:HostEndpoint) \
                 OPTIONAL MATCH (h)-[:CONNECTED_TO]->(i:Interface) \
                 RETURN h.id, h.ip, h.mac, h.hostname, h.kind, i.device_address, i.name",
            )
            .map_err(|e| e.to_string())?;
        let host_endpoints_raw: Vec<(String, String, String, String, String, String, String)> =
            he_rows
                .map(|row| {
                    (
                        read_str(&row[0]),
                        read_str(&row[1]),
                        read_str(&row[2]),
                        read_str(&row[3]),
                        read_str(&row[4]),
                        read_str(&row[5]),
                        read_str(&row[6]),
                    )
                })
                .collect();

        // IS-IS adjacencies — gracefully absent before first gNMI or syslog IS-IS event
        let isis_raw: Vec<(String, String, String, String, String)> = match conn.query(
            "MATCH (a:IsisAdjacency) \
             RETURN a.device_address, a.system_id, a.if_name, a.adjacency_state, a.source_type",
        ) {
            Ok(rows) => rows
                .map(|row| {
                    (
                        read_str(&row[0]),
                        read_str(&row[1]),
                        read_str(&row[2]),
                        read_str(&row[3]),
                        read_str(&row[4]),
                    )
                })
                .collect(),
            Err(_) => Vec::new(),
        };

        Ok::<_, String>((devices_raw, interfaces_raw, links_raw, bgp_raw, bgp_d2d_raw, bfd_d2d_raw, isis_d2d_raw, host_endpoints_raw, isis_raw))
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
    let mut allowed_devices: HashSet<String> = HashSet::new();
    let mut role_site: HashMap<String, (String, String, String, String)> = HashMap::new();
    if let Ok(targets) = state.registry.list_all_targets() {
        for t in targets {
            allowed_devices.insert(t.address.clone());
            let site = t.site.unwrap_or_default();
            let (site_id, site_path) =
                resolve_site_metadata(&site, &site_id_by_name, &site_path_by_id);
            role_site.insert(
                t.address.clone(),
                (t.role.unwrap_or_default(), site, site_id, site_path),
            );
        }
    }

    let mut interfaces_by_device: HashMap<String, Vec<InterfaceDetailJson>> = HashMap::new();
    for (dev, iface) in interfaces_raw {
        if !allowed_devices.contains(&dev) {
            continue;
        }
        interfaces_by_device.entry(dev).or_default().push(iface);
    }
    for ifaces in interfaces_by_device.values_mut() {
        ifaces.sort_by(|a, b| a.name.cmp(&b.name));
    }

    // Group BGP by device, enriching with peer_device when resolved via BGP_SESSION_WITH
    let mut bgp_by_device: HashMap<String, Vec<BgpJson>> = HashMap::new();
    for (dev, peer, st, peer_as) in bgp_raw {
        if !allowed_devices.contains(&dev) {
            continue;
        }
        // peer_device: look for any D2D edge from this device where the resolved peer
        // matches a known device address (peer may equal resolved address, or peer IP
        // is a loopback that resolves to the device).
        let peer_device = bgp_d2d_raw.iter()
            .find(|(src, dst, _)| src == &dev && (dst == &peer || allowed_devices.contains(dst)))
            .map(|(_, dst, _)| dst.clone())
            .unwrap_or_default();
        bgp_by_device.entry(dev).or_default().push(BgpJson {
            peer,
            state: st,
            peer_as,
            peer_device,
        });
    }

    // Group IS-IS adjacencies by device
    let mut isis_by_device: HashMap<String, Vec<IsisAdjJson>> = HashMap::new();
    for (dev, system_id, if_name, adjacency_state, source_type) in isis_raw {
        if !allowed_devices.contains(&dev) {
            continue;
        }
        isis_by_device.entry(dev).or_default().push(IsisAdjJson {
            system_id,
            if_name,
            adjacency_state,
            source_type,
        });
    }

    // Build device list with computed health + registry metadata
    let devices: Vec<DeviceJson> = devices_raw
        .into_iter()
        .filter(|(address, _, _)| allowed_devices.contains(address))
        .map(|(address, vendor, hostname)| {
            let interfaces = interfaces_by_device.remove(&address).unwrap_or_default();
            let bgp = bgp_by_device.remove(&address).unwrap_or_default();
            let isis_adjacencies = isis_by_device.remove(&address).unwrap_or_default();
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
                interfaces,
                bgp,
                isis_adjacencies,
            }
        })
        .collect();

    let links = links_raw
        .into_iter()
        .filter(|(src_device, _, dst_device, _, _, _)| {
            allowed_devices.contains(src_device) && allowed_devices.contains(dst_device)
        })
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

    let host_endpoints: Vec<HostEndpointJson> = host_endpoints_raw
        .into_iter()
        .map(|(id, ip, mac, hostname, kind, connected_to_device, connected_to_iface)| {
            HostEndpointJson {
                id,
                ip,
                mac,
                hostname,
                kind,
                connected_to_device,
                connected_to_iface,
            }
        })
        .collect();

    // BFD Device-to-Device links (L3 layer)
    let bfd_links: Vec<LinkJson> = bfd_d2d_raw
        .into_iter()
        .filter(|(src, dst, _)| allowed_devices.contains(src) && allowed_devices.contains(dst))
        .map(|(src_device, dst_device, state)| LinkJson {
            src_device,
            src_iface: format!("BFD[{state}]"),
            dst_device,
            dst_iface: String::new(),
            bytes_total: 0,
            is_mgmt: false,
        })
        .collect();

    // ISIS Device-to-Device links (L3 layer)
    let isis_links: Vec<LinkJson> = isis_d2d_raw
        .into_iter()
        .filter(|(src, dst, _)| allowed_devices.contains(src) && allowed_devices.contains(dst))
        .map(|(src_device, dst_device, state)| LinkJson {
            src_device,
            src_iface: format!("ISIS[{state}]"),
            dst_device,
            dst_iface: String::new(),
            bytes_total: 0,
            is_mgmt: false,
        })
        .collect();

    Ok(Json(TopologyResponse {
        schema_version: API_SCHEMA_VERSION.to_string(),
        devices,
        bfd_links,
        isis_links,
        links,
        host_endpoints,
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
pub(super) async fn events_history_handler(
    State(state): State<AppState>,
    Query(params): Query<EventsHistoryParams>,
) -> Result<Json<EventsHistoryResponse>, (StatusCode, String)> {
    let rows = state
        .store
        .read_events_history(params.source, params.device, params.site, params.limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let events = rows
        .into_iter()
        .map(|r| EventHistoryItem {
            id: r.id,
            device_address: r.device_address,
            event_type: r.event_type,
            source_type: r.source_type,
            detail_json: r.detail_json,
            occurred_at_ns: r.occurred_at_ns,
        })
        .collect();
    Ok(Json(EventsHistoryResponse { events }))
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
                source_type: ev.source_type,
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
            source_type: "registry".to_string(),
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
        source_type: "registry".to_string(),
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

    // Single spawn_blocking: degree map + correlation chains + blast radius summaries.
    let db = state.store.db();
    let detection_ids: Vec<String> = detections.iter().map(|d| d.id.clone()).collect();
    let root_devices: Vec<String> = detections.iter().map(|d| d.device_address.clone()).collect();

    type ChainMap = HashMap<String, Vec<CorrelationStep>>;
    type BrMap = HashMap<String, BlastRadiusSummary>;

    let (degree_map, chain_map, br_map): (HashMap<String, usize>, ChainMap, BrMap) =
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;

            // Degree map
            let rows = conn
                .query(
                    "MATCH (a:Interface)-[:CONNECTED_TO]->(:Interface) \
                     RETURN a.device_address",
                )
                .map_err(|e| e.to_string())?;
            let mut degree_map: HashMap<String, usize> = HashMap::new();
            for row in rows {
                *degree_map.entry(read_str(&row[0])).or_insert(0) += 1;
            }

            // Correlation chains (one query per detection — bounded by params.limit)
            let mut chain_map: ChainMap = HashMap::new();
            for det_id in &detection_ids {
                let steps = crate::graph::GraphStore::read_triggered_by_chain_sync(&conn, det_id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(sid, etype, src, dev, ts)| CorrelationStep {
                        state_change_event_id: sid,
                        event_type: etype,
                        source_type: src,
                        device_address: dev,
                        occurred_at_ns: ts,
                    })
                    .collect();
                chain_map.insert(det_id.clone(), steps);
            }

            // Blast radius summary per unique root device (2-hop, capped)
            let mut br_map: BrMap = HashMap::new();
            let mut seen_devices: std::collections::HashSet<String> = std::collections::HashSet::new();
            for addr in &root_devices {
                if seen_devices.insert(addr.clone()) {
                    if let Ok(br) = crate::graph::queries::blast_radius(&conn, addr, 2) {
                        let app_count = br.direct_apps.len()
                            + br.neighbor_apps.iter()
                                .filter(|a| !br.direct_apps.contains(a))
                                .count();
                        br_map.insert(
                            addr.clone(),
                            BlastRadiusSummary {
                                device_count: br.reachable_devices.len(),
                                app_count,
                            },
                        );
                    }
                }
            }

            Ok::<_, String>((degree_map, chain_map, br_map))
        })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .unwrap_or_default();

    let incidents = group_into_incidents(detections, params.window_secs, &degree_map, &chain_map, &br_map);
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
    chain_map: &HashMap<String, Vec<CorrelationStep>>,
    br_map: &HashMap<String, BlastRadiusSummary>,
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

            let correlation_chain = chain_map
                .get(&id)
                .cloned()
                .unwrap_or_default();
            let blast_radius_summary = br_map
                .get(&root.device_address)
                .cloned();

            // D4-4 T2: Incident type taxonomy
            // config_caused: any rule_id contains "config" or "config_caused_fault"
            // cascading_failure: multi-device where root has highest degree and cascading have lower degree
            // multi_device_correlated: multiple devices but no clear upstream root
            // single_device: only one device involved
            let has_config_rule = rule_ids.iter().any(|r| r.contains("config"));
            let incident_type = if has_config_rule {
                "config_caused".to_string()
            } else if device_count > 1 {
                let root_degree = *degree_map.get(&root.device_address).unwrap_or(&0);
                let any_cascading_lower = group.iter().any(|d| {
                    *degree_map.get(&d.device_address).unwrap_or(&0) < root_degree
                });
                if any_cascading_lower || root_degree > 0 {
                    "cascading_failure".to_string()
                } else {
                    "multi_device_correlated".to_string()
                }
            } else {
                "single_device".to_string()
            };

            // D4-4 T3: Grouping rationale
            let multi_source = std::iter::once(&root)
                .chain(group.iter())
                .flat_map(|d| d.source_types.iter())
                .collect::<std::collections::HashSet<_>>()
                .len() > 1;
            let grouping_rationale = match incident_type.as_str() {
                "config_caused" => {
                    let lag_ms: Option<i64> = std::iter::once(&root)
                        .chain(group.iter())
                        .find(|d| d.rule_id.contains("config"))
                        .and_then(|d| serde_json::from_str::<serde_json::Value>(&d.features_json).ok())
                        .and_then(|v| v.get("detail").and_then(|d| d.get("config_lag_ms")).and_then(|l| l.as_i64()));
                    if let Some(ms) = lag_ms {
                        format!("Config change preceded this fault by {}ms — likely operator-caused.", ms)
                    } else {
                        "Config change correlated with this fault — likely operator-caused.".to_string()
                    }
                }
                "cascading_failure" => {
                    let span_s = (ended_at_ns - started_at_ns).max(0) / 1_000_000_000;
                    format!(
                        "{} lost uplink at T+0 → fault propagated to {} neighboring device{}. Temporal proximity + shared blast radius ({span_s}s window).",
                        root.device_address,
                        device_count - 1,
                        if device_count - 1 == 1 { "" } else { "s" },
                    )
                }
                "multi_device_correlated" => {
                    if multi_source {
                        format!(
                            "Same {} event confirmed by multiple signal sources — merged into one detection. {} devices affected within {}s.",
                            rule_ids.first().map(|s| s.as_str()).unwrap_or("fault"),
                            device_count,
                            window_secs_actual,
                        )
                    } else {
                        format!(
                            "{} devices fired the same rule type within {}s — grouped by temporal proximity.",
                            device_count,
                            window_secs_actual,
                        )
                    }
                }
                _ => {
                    if multi_source {
                        format!(
                            "Same fault confirmed by {} signal sources — merged into one detection.",
                            std::iter::once(&root)
                                .chain(group.iter())
                                .flat_map(|d| d.source_types.iter())
                                .collect::<std::collections::HashSet<_>>()
                                .len()
                        )
                    } else if event_count > 1 {
                        format!("{event_count} events from the same device within {window_secs_actual}s.")
                    } else {
                        format!("Single detection event on {}.", root.device_address)
                    }
                }
            };

            // D4-4 T6: Per-device breakdown
            let mut device_rule_map: HashMap<String, Vec<String>> = HashMap::new();
            for d in std::iter::once(&root).chain(group.iter()) {
                device_rule_map.entry(d.device_address.clone())
                    .or_default()
                    .push(d.rule_id.clone());
            }
            let mut affected_device_details: Vec<AffectedDeviceDetail> = device_rule_map
                .into_iter()
                .map(|(addr, rules)| {
                    let is_root_dev = addr == root.device_address;
                    let detected_at = std::iter::once(&root)
                        .chain(group.iter())
                        .filter(|d| d.device_address == addr)
                        .map(|d| d.fired_at_ns)
                        .min()
                        .unwrap_or(started_at_ns);
                    AffectedDeviceDetail {
                        address: addr,
                        rules,
                        is_root: is_root_dev,
                        detected_at_ns: detected_at,
                    }
                })
                .collect();
            affected_device_details.sort_by(|a, b| {
                b.is_root.cmp(&a.is_root).then(a.detected_at_ns.cmp(&b.detected_at_ns))
            });

            IncidentJson {
                id,
                root,
                cascading: group,
                affected_devices,
                affected_device_details,
                severity,
                started_at_ns,
                ended_at_ns,
                remediation_status,
                rule_ids,
                co_fire_signature,
                device_count,
                event_count,
                correlation_chain,
                blast_radius_summary,
                incident_type,
                grouping_rationale,
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
pub(super) async fn graph_quality_handler(
    State(state): State<AppState>,
) -> Result<Json<crate::graph::algorithms::GraphQuality>, (StatusCode, String)> {
    let db = state.store.db();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::algorithms::graph_quality(&conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// D4-5 T5: GET /api/flows/live
/// Returns AppFlow nodes updated in the last 60 seconds, along with aggregate
/// bytes_per_sec and packets_per_sec per exporter. Used by the UI liveliness indicator.
#[derive(serde::Serialize)]
pub struct LiveFlowSummary {
    pub window_secs: u64,
    pub total_flows: usize,
    pub exporters: Vec<FlowExporterSummary>,
    pub top_flows: Vec<LiveFlowEntry>,
}

#[derive(serde::Serialize)]
pub struct FlowExporterSummary {
    pub exporter_address: String,
    pub flow_count: usize,
    pub total_bytes_per_sec: f64,
    pub total_packets_per_sec: f64,
}

#[derive(serde::Serialize)]
pub struct LiveFlowEntry {
    pub exporter_address: String,
    pub src_address: String,
    pub dst_address: String,
    pub dst_port: i64,
    pub protocol: String,
    pub bytes_per_sec: f64,
    pub packets_per_sec: f64,
}

pub(super) async fn flows_live_handler(
    State(state): State<AppState>,
) -> Result<Json<LiveFlowSummary>, (StatusCode, String)> {
    let db = state.store.db();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let window_secs: u64 = 60;
        let cutoff_ns = now_ns() - (window_secs as i64 * 1_000_000_000);
        let cutoff_val = crate::graph::common::ts(cutoff_ns);

        let mut stmt = conn
            .prepare(
                "MATCH (f:AppFlow) \
                 WHERE f.updated_at >= $cutoff \
                 RETURN f.exporter_address, f.src_address, f.dst_address, \
                        f.dst_port, f.protocol, f.bytes_per_sec, f.packets_per_sec \
                 ORDER BY f.bytes_per_sec DESC \
                 LIMIT 200",
            )
            .map_err(|e| e.to_string())?;

        let rows = conn
            .execute(&mut stmt, vec![("cutoff", cutoff_val)])
            .map_err(|e| e.to_string())?;

        let mut flows: Vec<LiveFlowEntry> = Vec::new();
        let mut exporter_map: std::collections::HashMap<String, (usize, f64, f64)> =
            std::collections::HashMap::new();

        for row in rows {
            let exp  = read_str(&row[0]);
            let src  = read_str(&row[1]);
            let dst  = read_str(&row[2]);
            let port = read_i64(&row[3]);
            let proto = read_str(&row[4]);
            let bps  = match &row[5] { lbug::Value::Double(v) => *v, _ => 0.0 };
            let pps  = match &row[6] { lbug::Value::Double(v) => *v, _ => 0.0 };

            let e = exporter_map.entry(exp.clone()).or_insert((0, 0.0, 0.0));
            e.0 += 1;
            e.1 += bps;
            e.2 += pps;

            flows.push(LiveFlowEntry {
                exporter_address: exp,
                src_address: src,
                dst_address: dst,
                dst_port: port,
                protocol: proto,
                bytes_per_sec: bps,
                packets_per_sec: pps,
            });
        }

        let total = flows.len();
        let exporters: Vec<FlowExporterSummary> = exporter_map
            .into_iter()
            .map(|(addr, (cnt, bps, pps))| FlowExporterSummary {
                exporter_address: addr,
                flow_count: cnt,
                total_bytes_per_sec: bps,
                total_packets_per_sec: pps,
            })
            .collect();

        Ok::<_, String>(LiveFlowSummary {
            window_secs,
            total_flows: total,
            exporters,
            top_flows: flows,
        })
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
        crate::graph::explorer::execute_query(&conn, &cypher).map_err(|e| format!("{e:#}"))
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

#[derive(serde::Deserialize)]
pub(super) struct InjectChannelState {
    pub(super) name: String,
    pub(super) rx_power_dbm: f64,
    pub(super) tx_power_dbm: f64,
    pub(super) osnr_db: f64,
    pub(super) pre_fec_ber: f64,
    pub(super) laser_bias_ma: f64,
    pub(super) temperature_c: f64,
}

#[derive(serde::Deserialize)]
pub(super) struct InjectEventBody {
    pub(super) device_address: String,
    pub(super) event_type: String,
    pub(super) occurred_at_ns: i64,
    #[serde(default)]
    pub(super) channels: Vec<InjectChannelState>,
}

pub(super) async fn events_inject_handler(
    State(state): State<AppState>,
    Json(body): Json<InjectEventBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    match body.event_type.as_str() {
        "optical_channel_state" => {
            let InjectEventBody {
                device_address,
                occurred_at_ns,
                channels,
                ..
            } = body;
            let channel_count = channels.len();
            let db = state.store.db();
            let device_address_cl = device_address.clone();
            tokio::task::spawn_blocking(move || {
                let conn = lbug::Connection::new(&db).map_err(|e| e.to_string())?;
                for ch in &channels {
                    let id = format!("{}::{}", device_address_cl, ch.name);
                    crate::graph::common::upsert_optical_channel(
                        &conn,
                        &id,
                        &device_address_cl,
                        &ch.name,
                        ch.rx_power_dbm,
                        ch.tx_power_dbm,
                        ch.osnr_db,
                        ch.pre_fec_ber,
                        ch.laser_bias_ma,
                        ch.temperature_c,
                        occurred_at_ns,
                    )
                    .map_err(|e| e.to_string())?;
                }
                Ok::<_, String>(())
            })
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

            state.store.publish_event(crate::graph::BonsaiEvent {
                device_address: device_address.clone(),
                event_type: "optical_channel_state".to_string(),
                detail_json: serde_json::json!({
                    "device_address": device_address,
                    "channel_count": channel_count,
                })
                .to_string(),
                occurred_at_ns,
                state_change_event_id: String::new(),
                source_type: "gnmi".to_string(),
            });
            Ok(StatusCode::NO_CONTENT)
        }
        other => Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("unsupported event_type: {other}"),
        )),
    }
}

// ── GET /api/operations/gnn-calibration ──────────────────────────────────────

/// Return GNN score distribution stats for the last 24h.
/// Used by the Operations UI calibration panel.
pub async fn gnn_calibration_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .store
        .read_gnn_calibration_stats()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ── POST /api/gnn/score ───────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct GnnScoreBody {
    pub device_address: String,
    pub score: f64,
    #[serde(default)]
    pub model_version: String,
}

#[derive(serde::Serialize)]
pub struct GnnScoreResponse {
    pub id: String,
    pub fired_detection: bool,
    pub detection_id: Option<String>,
}

/// Called by the Python GNN sidecar with an anomaly score for a device.
/// In production mode (score >= threshold) fires a DetectionEvent with
/// rule_id="gnn_anomaly", which the auto-investigate gate will pick up.
pub async fn gnn_score_handler(
    State(state): State<AppState>,
    Json(body): Json<GnnScoreBody>,
) -> Result<Json<GnnScoreResponse>, (StatusCode, String)> {
    let gnn = &state.gnn_config;
    let is_production = gnn.inference_mode.to_ascii_lowercase() == "production";
    let fired = is_production && body.score >= gnn.threshold;

    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);

    let score_id = state
        .store
        .write_gnn_score(
            body.device_address.clone(),
            body.score,
            gnn.threshold,
            gnn.inference_mode.clone(),
            body.model_version.clone(),
            fired,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let detection_id = if fired {
        let features = serde_json::json!({
            "anomaly_score": body.score,
            "threshold": gnn.threshold,
            "model_version": body.model_version,
        })
        .to_string();
        match state
            .store
            .write_detection(
                body.device_address.clone(),
                "gnn_anomaly".to_string(),
                "warn".to_string(),
                features,
                "gnn".to_string(),
                0,
                now_ns,
                String::new(),
                vec![],
            )
            .await
        {
            Ok(id) => {
                tracing::info!(
                    device = %body.device_address,
                    score = body.score,
                    detection_id = %id,
                    "GNN anomaly detection fired"
                );
                Some(id)
            }
            Err(e) => {
                tracing::warn!(error = %e, "GNN: write_detection failed");
                None
            }
        }
    } else {
        None
    };

    Ok(Json(GnnScoreResponse {
        id: score_id,
        fired_detection: fired,
        detection_id,
    }))
}

// ── D4-13 T1: DB stats ───────────────────────────────────────────────────────

pub(super) async fn db_stats_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let db = state.store.db();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

        let node_tables = vec![
            "Device", "Interface", "BgpSession", "IsIsAdj", "BfdSession",
            "DetectionEvent", "Incident", "AppFlow", "Application",
            "RemediationProposal", "Investigation", "AgentToolCall",
            "InvestigationFeedback", "ConfigChange", "PlaybookCatalog",
            "Location", "Prefix", "HostEndpoint", "DeviceEmbedding",
            "GnnScore", "ShunRule", "ChangeRequest", "SubscriptionStatus",
            "DeviceAddress", "EntityIdentity",
        ];
        let rel_tables = vec![
            "CONNECTED_TO", "HAS_SESSION", "HAS_INTERFACE", "HAS_ISIS_ADJ",
            "HAS_BFD", "DETECTED_ON", "HAS_DETECTION", "HAS_PROPOSAL",
            "HAS_INCIDENT", "HAS_TOOL_CALL", "HAS_FEEDBACK",
            "CARRIES_FLOW", "RUNS_SERVICE", "HOST_RUNS_SERVICE",
            "SRC_HOST", "DST_HOST", "TRUST_MARKS",
            "AFFECTED_BY_CHANGE", "CHANGE_CAUSED_CONFIG",
            "CHANGE_CAUSED_DETECTION", "RELATED_TO_CHANGE",
            "CMDB_PARENT_OF", "LOC_PARENT_OF",
            "KNOWN_ADDRESS_OF", "HAS_IDENTITY",
        ];

        let mut node_counts = serde_json::Map::new();
        for table in &node_tables {
            let count = conn
                .query(&format!("MATCH (n:{table}) RETURN count(n)"))
                .and_then(|mut r| {
                    if let Some(row) = r.next() {
                        Ok(read_i64(&row[0]))
                    } else {
                        Ok(0)
                    }
                })
                .unwrap_or(0);
            if count > 0 {
                node_counts.insert(table.to_string(), serde_json::json!(count));
            }
        }

        let mut rel_counts = serde_json::Map::new();
        for table in &rel_tables {
            let count = conn
                .query(&format!("MATCH ()-[r:{table}]->() RETURN count(r)"))
                .and_then(|mut r| {
                    if let Some(row) = r.next() {
                        Ok(read_i64(&row[0]))
                    } else {
                        Ok(0)
                    }
                })
                .unwrap_or(0);
            if count > 0 {
                rel_counts.insert(table.to_string(), serde_json::json!(count));
            }
        }

        // DB file size
        let db_path = std::path::Path::new("runtime/bonsai.db");
        let db_size_bytes = if db_path.exists() {
            walkdir(db_path)
        } else {
            0
        };

        Ok::<_, (StatusCode, String)>(serde_json::json!({
            "node_counts": node_counts,
            "rel_counts": rel_counts,
            "db_size_bytes": db_size_bytes,
            "total_node_tables": node_tables.len(),
            "total_rel_tables": rel_tables.len(),
        }))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
}

fn walkdir(path: &std::path::Path) -> u64 {
    if path.is_file() {
        return path.metadata().map(|m| m.len()).unwrap_or(0);
    }
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            total += walkdir(&entry.path());
        }
    }
    total
}

// ── D4-13 T2: Schema viewer ─────────────────────────────────────────────────

pub(super) async fn db_schema_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let db = state.store.db();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

        let mut node_tables = Vec::new();
        let mut rel_tables = Vec::new();

        // Query node table info
        if let Ok(mut result) = conn.query(
            "CALL show_tables() RETURN * ORDER BY name"
        ) {
            while let Some(row) = result.next() {
                let name = read_str(&row[0]);
                let ttype = read_str(&row[1]);
                if ttype == "NODE" {
                    // Get columns for this node table
                    let columns = get_table_columns(&conn, &name);
                    node_tables.push(serde_json::json!({
                        "name": name,
                        "columns": columns,
                    }));
                } else if ttype == "REL" {
                    let columns = get_table_columns(&conn, &name);
                    rel_tables.push(serde_json::json!({
                        "name": name,
                        "columns": columns,
                    }));
                }
            }
        }

        Ok::<_, (StatusCode, String)>(serde_json::json!({
            "node_tables": node_tables,
            "rel_tables": rel_tables,
        }))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
}

// ── D4-13 T3: Safe data management operations ─────────────────────────────

#[derive(Deserialize)]
pub(super) struct PurgeParams {
    node_type: String,
    older_than_days: u64,
}

pub(super) async fn db_purge_handler(
    State(state): State<AppState>,
    Query(params): Query<PurgeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let allowed = [
        "DetectionEvent", "AppFlow", "AgentToolCall", "InvestigationFeedback",
        "GnnScore", "StateChangeEvent",
    ];
    if !allowed.contains(&params.node_type.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Purge not allowed for '{}'. Allowed: {:?}", params.node_type, allowed),
        ));
    }

    let ts_col = match params.node_type.as_str() {
        "DetectionEvent" => "fired_at",
        "AppFlow" => "updated_at",
        "AgentToolCall" => "called_at",
        "InvestigationFeedback" => "created_at",
        "GnnScore" => "scored_at",
        "StateChangeEvent" => "occurred_at",
        _ => return Err((StatusCode::BAD_REQUEST, "Unknown timestamp column".into())),
    };

    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64;
    let cutoff_ns = now_ns - (params.older_than_days as i64 * 86_400 * 1_000_000_000);

    let db = state.store.db();
    let node_type = params.node_type.clone();
    let deleted = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

        // Count first
        let count_q = format!(
            "MATCH (n:{node_type}) WHERE n.{ts_col} < timestamp_ns({cutoff_ns}) RETURN count(n)"
        );
        let count: i64 = conn
            .query(&count_q)
            .and_then(|mut r| {
                if let Some(row) = r.next() {
                    Ok(read_i64(&row[0]))
                } else {
                    Ok(0)
                }
            })
            .unwrap_or(0);

        if count > 0 {
            // Detach delete removes node and all connected edges
            let del_q = format!(
                "MATCH (n:{node_type}) WHERE n.{ts_col} < timestamp_ns({cutoff_ns}) DETACH DELETE n"
            );
            conn.query(&del_q)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }

        Ok::<_, (StatusCode, String)>(count)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    tracing::info!(
        node_type = params.node_type,
        older_than_days = params.older_than_days,
        deleted_count = deleted,
        "db purge completed"
    );

    Ok(Json(serde_json::json!({
        "node_type": params.node_type,
        "older_than_days": params.older_than_days,
        "deleted_count": deleted,
    })))
}

pub(super) async fn db_checkpoint_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let db = state.store.db();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        conn.query("CALL checkpoint()")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        Ok::<_, (StatusCode, String)>(())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    tracing::info!("db checkpoint completed");
    Ok(Json(serde_json::json!({"status": "checkpoint_complete"})))
}

#[derive(Deserialize)]
pub(super) struct ExportParams {
    node_type: String,
    #[serde(default = "default_export_limit")]
    limit: u32,
}

fn default_export_limit() -> u32 {
    10_000
}

pub(super) async fn db_export_handler(
    State(state): State<AppState>,
    Query(params): Query<ExportParams>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let allowed = [
        "Device", "Interface", "BgpSession", "IsIsAdj", "BfdSession",
        "DetectionEvent", "Incident", "AppFlow", "Application",
        "Investigation", "AgentToolCall", "ConfigChange", "ChangeRequest",
        "Location", "Prefix", "HostEndpoint", "ShunRule",
    ];
    if !allowed.contains(&params.node_type.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Export not allowed for '{}'. Allowed: {:?}", params.node_type, allowed),
        ));
    }

    let db = state.store.db();
    let node_type = params.node_type.clone();
    let limit = params.limit;
    let lines = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

        let q = format!("MATCH (n:{node_type}) RETURN n LIMIT {limit}");
        let mut result = conn.query(&q)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;

        let mut lines = Vec::new();
        while let Some(row) = result.next() {
            let val = read_str(&row[0]);
            lines.push(val);
        }
        Ok::<_, (StatusCode, String)>(lines)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    let body = lines.join("\n");
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/x-ndjson"),
    );
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_str(
            &format!("attachment; filename=\"{}.jsonl\"", params.node_type),
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    );
    Ok((StatusCode::OK, headers, body))
}

// ── D4-13 T4: Backup + restore ──────────────────────────────────────────────

pub(super) async fn db_backup_handler() -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let runtime_path = std::path::Path::new("runtime");
    if !runtime_path.exists() {
        return Err((StatusCode::BAD_REQUEST, "runtime/ directory not found".into()));
    }

    let backups_dir = std::path::Path::new("backups");
    std::fs::create_dir_all(backups_dir)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("create backups dir: {e}")))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let ts = now;
    let filename = format!("bonsai-{ts}.tar.gz");
    let backup_path = backups_dir.join(&filename);

    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::create(&backup_path)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("create backup: {e}")))?;
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut tar = tar::Builder::new(enc);
        tar.append_dir_all("runtime", "runtime")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("tar runtime: {e}")))?;
        tar.finish()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("finish tar: {e}")))?;
        Ok::<_, (StatusCode, String)>(())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    tracing::info!(filename = %filename, "db backup created");
    Ok(Json(serde_json::json!({
        "status": "backup_complete",
        "filename": filename,
    })))
}

pub(super) async fn db_list_backups_handler() -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let backups_dir = std::path::Path::new("backups");
    if !backups_dir.exists() {
        return Ok(Json(serde_json::json!({"backups": []})));
    }
    let mut backups: Vec<serde_json::Value> = Vec::new();
    let entries = std::fs::read_dir(backups_dir)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".tar.gz") {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            backups.push(serde_json::json!({"filename": name, "size_bytes": size}));
        }
    }
    backups.sort_by(|a, b| b["filename"].as_str().cmp(&a["filename"].as_str()));
    Ok(Json(serde_json::json!({"backups": backups})))
}

/// D4-12 T1: GET /api/redundancy/groups
pub(super) async fn list_redundancy_groups_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let groups = state.store
        .list_redundancy_groups()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({"groups": groups})))
}

// ── GET /api/devices/{address}/flows ─────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct DeviceFlowSummary {
    pub device_address: String,
    pub window_secs: u64,
    pub total_flows: usize,
    pub total_bytes_per_sec: f64,
    pub total_packets_per_sec: f64,
    pub top_flows: Vec<LiveFlowEntry>,
}

/// Returns AppFlow nodes exported by the given device in the last 60 seconds.
/// Uses the `CARRIES_FLOW(Device→AppFlow)` edge to scope the result to a single device.
pub(super) async fn device_flows_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceFlowSummary>, (StatusCode, String)> {
    let db = state.store.db();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let window_secs: u64 = 60;
        let cutoff_ns = now_ns() - (window_secs as i64 * 1_000_000_000);
        let cutoff_val = crate::graph::common::ts(cutoff_ns);
        let bare_addr = crate::registry::strip_port(&address).to_string();
        let addr_val = lbug::Value::String(bare_addr);

        let mut stmt = conn
            .prepare(
                "MATCH (d:Device {address: $addr})-[:CARRIES_FLOW]->(f:AppFlow) \
                 WHERE f.updated_at >= $cutoff \
                 RETURN f.exporter_address, f.src_address, f.dst_address, \
                        f.dst_port, f.protocol, f.bytes_per_sec, f.packets_per_sec \
                 ORDER BY f.bytes_per_sec DESC \
                 LIMIT 100",
            )
            .map_err(|e| e.to_string())?;

        let rows = conn
            .execute(&mut stmt, vec![
                ("addr", addr_val),
                ("cutoff", cutoff_val),
            ])
            .map_err(|e| e.to_string())?;

        let mut flows: Vec<LiveFlowEntry> = Vec::new();
        let mut total_bps = 0.0f64;
        let mut total_pps = 0.0f64;

        for row in rows {
            let exp   = read_str(&row[0]);
            let src   = read_str(&row[1]);
            let dst   = read_str(&row[2]);
            let port  = read_i64(&row[3]);
            let proto = read_str(&row[4]);
            let bps   = match &row[5] { lbug::Value::Double(v) => *v, _ => 0.0 };
            let pps   = match &row[6] { lbug::Value::Double(v) => *v, _ => 0.0 };
            total_bps += bps;
            total_pps += pps;
            flows.push(LiveFlowEntry {
                exporter_address: exp,
                src_address: src,
                dst_address: dst,
                dst_port: port,
                protocol: proto,
                bytes_per_sec: bps,
                packets_per_sec: pps,
            });
        }

        let total = flows.len();
        Ok::<_, String>(DeviceFlowSummary {
            device_address: address,
            window_secs,
            total_flows: total,
            total_bytes_per_sec: total_bps,
            total_packets_per_sec: total_pps,
            top_flows: flows,
        })
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

// ── GET /api/endpoints ────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct EndpointEntry {
    pub id: String,
    pub ip: String,
    pub kind: String,
    pub hostname: String,
    pub mac: String,
    pub vendor: String,
    pub source: String,
    pub site_id: String,
    pub rack_id: String,
    pub connected_to_device: String,
    pub connected_to_iface: String,
    pub recent_flow_count: i64,
}

#[derive(serde::Serialize)]
pub struct EndpointsResponse {
    pub endpoints: Vec<EndpointEntry>,
}

/// GET /api/endpoints — list all HostEndpoint nodes with connectivity and recent flow activity.
pub(super) async fn endpoints_handler(
    State(state): State<AppState>,
) -> Result<Json<EndpointsResponse>, (StatusCode, String)> {
    let db = state.store.db();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;

        // Fetch host endpoints with optional connected device/iface from CONNECTED_TO edge
        let mut stmt = conn.prepare(
            "MATCH (h:HostEndpoint) \
             OPTIONAL MATCH (h)-[:CONNECTED_TO]->(i:Interface)<-[:HAS_INTERFACE]-(d:Device) \
             RETURN h.id, h.ip, h.kind, h.hostname, h.mac, h.vendor, h.source, \
                    h.site_id, h.rack_id, \
                    coalesce(d.address, ''), coalesce(i.name, '') \
             ORDER BY h.ip",
        ).map_err(|e| e.to_string())?;

        let rows = conn.execute(&mut stmt, vec![]).map_err(|e| e.to_string())?;

        // Count recent flows per endpoint (src or dst in last 60s)
        let cutoff_ns = now_ns() - 60_000_000_000i64;
        let cutoff_val = crate::graph::common::ts(cutoff_ns);
        let mut flow_counts: HashMap<String, i64> = HashMap::new();
        if let Ok(mut fc_stmt) = conn.prepare(
            "MATCH (h:HostEndpoint)-[:SRC_HOST|DST_HOST]-(f:AppFlow) \
             WHERE f.updated_at >= $cutoff \
             RETURN h.id, count(f)",
        ) {
            if let Ok(fc_rows) = conn.execute(&mut fc_stmt, vec![("cutoff", cutoff_val)]) {
                for row in fc_rows {
                    let id = read_str(&row[0]);
                    let cnt = read_i64(&row[1]);
                    flow_counts.insert(id, cnt);
                }
            }
        }

        let mut endpoints: Vec<EndpointEntry> = Vec::new();
        for row in rows {
            let id = read_str(&row[0]);
            let recent_flow_count = *flow_counts.get(&id).unwrap_or(&0);
            endpoints.push(EndpointEntry {
                id: id.clone(),
                ip: read_str(&row[1]),
                kind: read_str(&row[2]),
                hostname: read_str(&row[3]),
                mac: read_str(&row[4]),
                vendor: read_str(&row[5]),
                source: read_str(&row[6]),
                site_id: read_str(&row[7]),
                rack_id: read_str(&row[8]),
                connected_to_device: read_str(&row[9]),
                connected_to_iface: read_str(&row[10]),
                recent_flow_count,
            });
        }

        Ok::<_, String>(EndpointsResponse { endpoints })
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

fn get_table_columns(conn: &Connection<'_>, table_name: &str) -> Vec<serde_json::Value> {
    let mut columns = Vec::new();
    if let Ok(mut result) = conn.query(&format!(
        "CALL table_info('{}') RETURN *", table_name
    )) {
        while let Some(row) = result.next() {
            let col_name = read_str(&row[1]);
            let col_type = read_str(&row[2]);
            columns.push(serde_json::json!({
                "name": col_name,
                "type": col_type,
            }));
        }
    }
    columns
}
