// Named, typed multi-hop graph query functions.
// Every query here uses multi-hop Cypher patterns; no single-table relational
// lookups. All callers that previously fetched edges into Rust Vecs and walked
// them in-process should migrate to these functions over time.
//
// Naming: <thing>_<relationship>_<thing> or <thing>_<predicate>.
// Done-when test: every public fn has at least one test in tests:: below.

use std::collections::HashMap;

use anyhow::{Context, Result};
use lbug::{Connection, Value};
use serde::Serialize;

use super::common::read_str;

// ─── helpers ─────────────────────────────────────────────────────────────────

fn read_i64(v: &Value) -> i64 {
    match v {
        Value::Int64(n) => *n,
        Value::Int32(n) => *n as i64,
        _ => 0,
    }
}

fn read_ts(v: &Value) -> i64 {
    use time::OffsetDateTime;
    match v {
        Value::TimestampNs(dt) => dt.unix_timestamp_nanos() as i64,
        Value::TimestampTz(dt) => {
            let epoch = OffsetDateTime::UNIX_EPOCH;
            let dur = *dt - epoch;
            dur.whole_nanoseconds() as i64
        }
        _ => 0,
    }
}

// ─── result types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct NeighborRow {
    /// Address of the neighboring device.
    pub address: String,
    pub hostname: String,
    pub vendor: String,
    /// Interface on this device that connects to the neighbor.
    pub local_iface: String,
    /// Interface on the neighbor that this device connects to.
    pub remote_iface: String,
}

/// A topology path between two devices in the network graph.
#[derive(Debug, Clone, Serialize)]
pub struct TopologyPath {
    /// Device addresses in hop order, source first.
    pub hops: Vec<String>,
    /// (src_device, src_iface, dst_device, dst_iface) for each physical link.
    pub links: Vec<(String, String, String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlastRadiusResult {
    pub origin_address: String,
    pub origin_hostname: String,
    pub site_name: String,
    pub env_name: String,
    /// All device addresses reachable within max_hops.
    pub reachable_devices: Vec<DeviceRef>,
    /// Applications directly running on the origin device.
    pub direct_apps: Vec<String>,
    /// Applications on devices in the reachable set.
    pub neighbor_apps: Vec<String>,
    /// Active detection events on devices in the reachable set.
    pub active_detections: Vec<String>,
    /// D4-16 T5: BMP sessions on origin device (for FRR + BMP investigation).
    pub bmp_sessions: Vec<BmpSessionRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BmpSessionRef {
    pub session_id: String,
    pub peer_address: String,
    pub adj_rib_in_routes: i64,
    pub loc_rib_routes: i64,
    pub prefixes_rejected: i64,
    pub session_state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceRef {
    pub address: String,
    pub hostname: String,
    pub vendor: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInEnvRow {
    pub address: String,
    pub hostname: String,
    pub vendor: String,
    pub site_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DetectionInEnvRow {
    pub device_address: String,
    pub detection_id: String,
    pub rule_id: String,
    pub severity: String,
    pub fired_at_ns: i64,
    pub site_name: String,
    pub env_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppOnSiteRow {
    pub app_name: String,
    pub device_address: String,
    pub device_hostname: String,
    pub site_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnresolvedDetection {
    pub id: String,
    pub device_address: String,
    pub rule_id: String,
    pub severity: String,
    pub fired_at_ns: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubscriptionHealthRow {
    pub path: String,
    pub status: String,
    pub last_observed_at_ns: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoFireRow {
    pub device_address: String,
    pub rule_id: String,
    pub fire_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnrichmentRow {
    pub key: String,
    pub value: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TopologyEdge {
    pub src_device: String,
    pub src_iface: String,
    pub dst_device: String,
    pub dst_iface: String,
}

// ─── query 1: direct topology neighbors ──────────────────────────────────────

/// All devices directly connected to `address` via CONNECTED_TO interface links.
///
/// Pattern: Device -[:HAS_INTERFACE]-> Interface -[:CONNECTED_TO]-> Interface <-[:HAS_INTERFACE]- Device
pub fn neighbors_of_device(conn: &Connection<'_>, address: &str) -> Result<Vec<NeighborRow>> {
    let mut stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr})-[:HAS_INTERFACE]->(si:Interface) \
             -[:CONNECTED_TO]-(di:Interface)<-[:HAS_INTERFACE]-(neighbor:Device) \
             RETURN DISTINCT neighbor.address, neighbor.hostname, neighbor.vendor, \
                    si.name, di.name",
        )
        .context("prepare neighbors_of_device")?;

    let rows = conn
        .execute(
            &mut stmt,
            vec![("addr", Value::String(address.to_string()))],
        )
        .context("execute neighbors_of_device")?;

    Ok(rows
        .map(|row| NeighborRow {
            address: read_str(&row[0]),
            hostname: read_str(&row[1]),
            vendor: read_str(&row[2]),
            local_iface: read_str(&row[3]),
            remote_iface: read_str(&row[4]),
        })
        .collect())
}

// ─── query 2: shortest topology path (replaces Rust-side BFS) ────────────────

/// Shortest path between `src` and `dst` devices.
///
/// Implements BFS at the device level: each iteration queries one hop of
/// `neighbors_of_device` for all frontier devices. This avoids loading the
/// full CONNECTED_TO edge table into Rust (the old approach) while still
/// being correct and efficient — queries only the frontier, not all edges.
pub fn shortest_topology_path(
    conn: &Connection<'_>,
    src: &str,
    dst: &str,
) -> Result<Option<TopologyPath>> {
    if src == dst {
        return Ok(Some(TopologyPath {
            hops: vec![src.to_string()],
            links: vec![],
        }));
    }

    // parent[device] = (via_device, src_iface, dst_iface)
    let mut parent: HashMap<String, Option<(String, String, String)>> = HashMap::new();
    parent.insert(src.to_string(), None);

    let mut frontier = vec![src.to_string()];
    let mut found = false;

    'bfs: for _ in 0..30 {
        let mut next: Vec<String> = Vec::new();
        for device in &frontier {
            let neighbors = neighbors_of_device(conn, device)?;
            for nb in neighbors {
                if parent.contains_key(&nb.address) {
                    continue;
                }
                parent.insert(
                    nb.address.clone(),
                    Some((
                        device.clone(),
                        nb.local_iface.clone(),
                        nb.remote_iface.clone(),
                    )),
                );
                if nb.address == dst {
                    found = true;
                    break 'bfs;
                }
                next.push(nb.address.clone());
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }

    if !found {
        return Ok(None);
    }

    // Reconstruct path back from dst to src
    let mut hops = vec![dst.to_string()];
    let mut links: Vec<(String, String, String, String)> = Vec::new();
    let mut cur = dst.to_string();
    while let Some(Some((prev, src_if, dst_if))) = parent.get(&cur) {
        links.push((prev.clone(), src_if.clone(), cur.clone(), dst_if.clone()));
        hops.push(prev.clone());
        cur = prev.clone();
    }
    hops.reverse();
    links.reverse();

    Ok(Some(TopologyPath { hops, links }))
}

// ─── query 3: blast radius ────────────────────────────────────────────────────

/// Devices, applications, and active detections reachable from `address` within
/// `max_hops` physical network hops.
///
/// Each physical hop = 3 graph edges (HAS_INTERFACE + CONNECTED_TO + HAS_INTERFACE).
/// The upper bound for the variable-length traversal is `max_hops * 3`.
pub fn blast_radius(
    conn: &Connection<'_>,
    address: &str,
    max_hops: usize,
) -> Result<BlastRadiusResult> {
    // Origin device info + site + env
    let mut dev_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}) \
             OPTIONAL MATCH (d)-[:LOCATED_AT]->(s:Site)-[:BELONGS_TO_ENVIRONMENT]->(env:Environment) \
             RETURN d.address, d.hostname, s.name, env.name",
        )
        .context("prepare blast_radius device")?;
    let dev_rows: Vec<_> = conn
        .execute(
            &mut dev_stmt,
            vec![("addr", Value::String(address.to_string()))],
        )
        .context("execute blast_radius device")?
        .collect();

    let (origin_hostname, site_name, env_name) = dev_rows
        .first()
        .map(|row| (read_str(&row[1]), read_str(&row[2]), read_str(&row[3])))
        .unwrap_or_default();

    // Reachable devices via topology traversal
    let hop_depth = (max_hops * 3).clamp(3, 30); // lbug 0.15.3 caps variable-length upper bound at 30
    let reach_cypher = format!(
        "MATCH (d:Device {{address: $addr}})-[:HAS_INTERFACE|CONNECTED_TO*1..{}]-(n:Device) \
         WHERE n.address <> $addr \
         RETURN DISTINCT n.address, n.hostname, n.vendor",
        hop_depth
    );
    let mut reach_stmt = conn
        .prepare(&reach_cypher)
        .context("prepare blast_radius reachable")?;
    let reachable_devices: Vec<DeviceRef> = conn
        .execute(
            &mut reach_stmt,
            vec![("addr", Value::String(address.to_string()))],
        )
        .context("execute blast_radius reachable")?
        .map(|row| DeviceRef {
            address: read_str(&row[0]),
            hostname: read_str(&row[1]),
            vendor: read_str(&row[2]),
        })
        .collect();

    // Applications running directly on the origin
    let direct_apps = apps_on_device(conn, address)?;

    // Applications on neighbor devices
    let mut neighbor_apps: Vec<String> = Vec::new();
    for dev in &reachable_devices {
        neighbor_apps.extend(apps_on_device(conn, &dev.address)?);
    }
    neighbor_apps.sort();
    neighbor_apps.dedup();

    // Active detection events on reachable devices (most recent 20)
    let mut det_stmt = conn
        .prepare(
            "MATCH (d:Device)-[:TRIGGERED]->(de:DetectionEvent) \
             WHERE d.address = $addr \
             RETURN de.rule_id \
             ORDER BY de.fired_at DESC \
             LIMIT 5",
        )
        .context("prepare blast_radius detections")?;

    let mut active_detections: Vec<String> = Vec::new();
    let all_addresses: Vec<String> = std::iter::once(address.to_string())
        .chain(reachable_devices.iter().map(|d| d.address.clone()))
        .collect();
    for addr in &all_addresses {
        let dets: Vec<String> = conn
            .execute(&mut det_stmt, vec![("addr", Value::String(addr.clone()))])
            .context("execute blast_radius detections")?
            .map(|row| format!("{}:{}", addr, read_str(&row[0])))
            .collect();
        active_detections.extend(dets);
    }

    // D4-16 T5: BMP sessions on the origin device (for FRR + BMP investigation).
    let bmp_sessions: Vec<BmpSessionRef> = {
        let mut bmp_stmt = conn
            .prepare(
                "MATCH (d:Device {address: $addr})-[:HAS_BMP_SESSION]->(s:BmpSession) \
                 RETURN s.id, s.peer_address, s.adj_rib_in_routes, s.loc_rib_routes, \
                        s.prefixes_rejected, s.session_state",
            )
            .unwrap_or_else(|_| conn.prepare("RETURN 0, '', 0, 0, 0, ''").unwrap());
        conn.execute(&mut bmp_stmt, vec![("addr", Value::String(address.to_string()))])
            .map(|qr| {
                qr.map(|row| BmpSessionRef {
                    session_id: read_str(&row[0]),
                    peer_address: read_str(&row[1]),
                    adj_rib_in_routes: read_i64(&row[2]),
                    loc_rib_routes: read_i64(&row[3]),
                    prefixes_rejected: read_i64(&row[4]),
                    session_state: read_str(&row[5]),
                })
                .collect()
            })
            .unwrap_or_default()
    };

    Ok(BlastRadiusResult {
        origin_address: address.to_string(),
        origin_hostname,
        site_name,
        env_name,
        reachable_devices,
        direct_apps,
        neighbor_apps,
        active_detections,
        bmp_sessions,
    })
}

fn apps_on_device(conn: &Connection<'_>, address: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr})-[:RUNS_SERVICE|CARRIES_APPLICATION]->(a:Application) \
             RETURN DISTINCT a.name",
        )
        .context("prepare apps_on_device")?;
    Ok(conn
        .execute(
            &mut stmt,
            vec![("addr", Value::String(address.to_string()))],
        )
        .context("execute apps_on_device")?
        .map(|row| read_str(&row[0]))
        .collect())
}

// ─── query 4: devices in environment ─────────────────────────────────────────

/// All devices in an environment, traversing Environment ← Site ← Device.
pub fn devices_in_environment(conn: &Connection<'_>, env_id: &str) -> Result<Vec<DeviceInEnvRow>> {
    let mut stmt = conn
        .prepare(
            "MATCH (env:Environment {id: $eid})<-[:BELONGS_TO_ENVIRONMENT]-(s:Site) \
             <-[:LOCATED_AT]-(d:Device) \
             RETURN d.address, d.hostname, d.vendor, s.name",
        )
        .context("prepare devices_in_environment")?;

    Ok(conn
        .execute(&mut stmt, vec![("eid", Value::String(env_id.to_string()))])
        .context("execute devices_in_environment")?
        .map(|row| DeviceInEnvRow {
            address: read_str(&row[0]),
            hostname: read_str(&row[1]),
            vendor: read_str(&row[2]),
            site_name: read_str(&row[3]),
        })
        .collect())
}

// ─── query 5: detections in environment ──────────────────────────────────────

/// Detection events on devices in an environment, within a time window.
///
/// Pattern: Environment ← Site ← Device -[:TRIGGERED]-> DetectionEvent
pub fn detections_in_environment(
    conn: &Connection<'_>,
    env_id: &str,
    since_ns: i64,
) -> Result<Vec<DetectionInEnvRow>> {
    let since_ts = super::common::ts(since_ns);
    let mut stmt = conn
        .prepare(
            "MATCH (env:Environment {id: $eid})<-[:BELONGS_TO_ENVIRONMENT]-(s:Site) \
             <-[:LOCATED_AT]-(d:Device)-[:TRIGGERED]->(de:DetectionEvent) \
             WHERE de.fired_at > $since \
             RETURN d.address, de.id, de.rule_id, de.severity, de.fired_at, s.name, env.name \
             ORDER BY de.fired_at DESC",
        )
        .context("prepare detections_in_environment")?;

    Ok(conn
        .execute(
            &mut stmt,
            vec![
                ("eid", Value::String(env_id.to_string())),
                ("since", since_ts),
            ],
        )
        .context("execute detections_in_environment")?
        .map(|row| DetectionInEnvRow {
            device_address: read_str(&row[0]),
            detection_id: read_str(&row[1]),
            rule_id: read_str(&row[2]),
            severity: read_str(&row[3]),
            fired_at_ns: read_ts(&row[4]),
            site_name: read_str(&row[5]),
            env_name: read_str(&row[6]),
        })
        .collect())
}

// ─── query 6: applications on site ───────────────────────────────────────────

/// Applications running on devices located at a named site.
///
/// Pattern: Site ← Device -[:RUNS_SERVICE|CARRIES_APPLICATION]-> Application
pub fn applications_on_site(conn: &Connection<'_>, site_name: &str) -> Result<Vec<AppOnSiteRow>> {
    let mut stmt = conn
        .prepare(
            "MATCH (s:Site {name: $site})<-[:LOCATED_AT]-(d:Device) \
             -[:RUNS_SERVICE|CARRIES_APPLICATION]->(a:Application) \
             RETURN DISTINCT a.name, d.address, d.hostname, s.name",
        )
        .context("prepare applications_on_site")?;

    Ok(conn
        .execute(
            &mut stmt,
            vec![("site", Value::String(site_name.to_string()))],
        )
        .context("execute applications_on_site")?
        .map(|row| AppOnSiteRow {
            app_name: read_str(&row[0]),
            device_address: read_str(&row[1]),
            device_hostname: read_str(&row[2]),
            site_name: read_str(&row[3]),
        })
        .collect())
}

// ─── query 7: devices missing enrichment ─────────────────────────────────────

/// Devices that have no HAS_ENRICHMENT_PROPERTY edges — missing NetBox/CMDB context.
pub fn devices_missing_enrichment(conn: &Connection<'_>) -> Result<Vec<String>> {
    let rows = conn
        .query(
            "MATCH (d:Device) \
             OPTIONAL MATCH (d)-[:HAS_ENRICHMENT_PROPERTY]->(ep:EnrichmentProperty) \
             WITH d, count(ep) AS ep_count \
             WHERE ep_count = 0 \
             RETURN d.address \
             ORDER BY d.address",
        )
        .context("query devices_missing_enrichment")?;

    Ok(rows.map(|row| read_str(&row[0])).collect())
}

// ─── query 8: orphan devices ──────────────────────────────────────────────────

/// Devices with no topology neighbors — no CONNECTED_TO edges from any interface.
/// Likely indicates a subscription or onboarding issue.
pub fn orphan_devices(conn: &Connection<'_>) -> Result<Vec<String>> {
    let rows = conn
        .query(
            "MATCH (d:Device) \
             OPTIONAL MATCH (d)-[:HAS_INTERFACE]->(i:Interface)-[:CONNECTED_TO]-() \
             WITH d, count(i) AS link_count \
             WHERE link_count = 0 \
             RETURN d.address \
             ORDER BY d.address",
        )
        .context("query orphan_devices")?;

    Ok(rows.map(|row| read_str(&row[0])).collect())
}

// ─── query 9: detections without remediation ─────────────────────────────────

/// Detection events with no linked Remediation node (unresolved faults).
pub fn detections_without_remediation(
    conn: &Connection<'_>,
    limit: u32,
) -> Result<Vec<UnresolvedDetection>> {
    let mut stmt = conn
        .prepare(
            "MATCH (de:DetectionEvent) \
             OPTIONAL MATCH (r:Remediation)-[:RESOLVES]->(de) \
             WITH de, r \
             WHERE r IS NULL \
             RETURN de.id, de.device_address, de.rule_id, de.severity, de.fired_at \
             ORDER BY de.fired_at DESC \
             LIMIT $lim",
        )
        .context("prepare detections_without_remediation")?;

    Ok(conn
        .execute(&mut stmt, vec![("lim", Value::Int64(limit as i64))])
        .context("execute detections_without_remediation")?
        .map(|row| UnresolvedDetection {
            id: read_str(&row[0]),
            device_address: read_str(&row[1]),
            rule_id: read_str(&row[2]),
            severity: read_str(&row[3]),
            fired_at_ns: read_ts(&row[4]),
        })
        .collect())
}

// ─── query 10: subscription health for device ────────────────────────────────

/// All subscription paths and their status for a device.
///
/// Pattern: Device -[:HAS_SUBSCRIPTION_STATUS]-> SubscriptionStatus
pub fn subscription_health_for_device(
    conn: &Connection<'_>,
    address: &str,
) -> Result<Vec<SubscriptionHealthRow>> {
    let mut stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr})-[:HAS_SUBSCRIPTION_STATUS]->(ss:SubscriptionStatus) \
             RETURN ss.path, ss.status, ss.last_observed_at \
             ORDER BY ss.path",
        )
        .context("prepare subscription_health_for_device")?;

    Ok(conn
        .execute(
            &mut stmt,
            vec![("addr", Value::String(address.to_string()))],
        )
        .context("execute subscription_health_for_device")?
        .map(|row| SubscriptionHealthRow {
            path: read_str(&row[0]),
            status: read_str(&row[1]),
            last_observed_at_ns: read_ts(&row[2]),
        })
        .collect())
}

// ─── query 11: co-firing detections ──────────────────────────────────────────

/// Detection events grouped by (device, rule_id) that fired within `since_ns`.
/// Devices with multiple rule_ids show up as separate rows; callers can group
/// by device_address to find which devices have co-firing patterns.
pub fn co_firing_detections(conn: &Connection<'_>, since_ns: i64) -> Result<Vec<CoFireRow>> {
    let since_ts = super::common::ts(since_ns);
    let mut stmt = conn
        .prepare(
            "MATCH (d:Device)-[:TRIGGERED]->(de:DetectionEvent) \
             WHERE de.fired_at > $since \
             RETURN d.address, de.rule_id, count(*) AS fire_count \
             ORDER BY fire_count DESC",
        )
        .context("prepare co_firing_detections")?;

    Ok(conn
        .execute(&mut stmt, vec![("since", since_ts)])
        .context("execute co_firing_detections")?
        .map(|row| CoFireRow {
            device_address: read_str(&row[0]),
            rule_id: read_str(&row[1]),
            fire_count: read_i64(&row[2]),
        })
        .collect())
}

// ─── query 12: device enrichment context ─────────────────────────────────────

/// All enrichment properties on a device (NetBox/CMDB context).
///
/// Pattern: Device -[:HAS_ENRICHMENT_PROPERTY]-> EnrichmentProperty
pub fn device_enrichment_context(
    conn: &Connection<'_>,
    address: &str,
) -> Result<Vec<EnrichmentRow>> {
    let mut stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr})-[:HAS_ENRICHMENT_PROPERTY]->(ep:EnrichmentProperty) \
             RETURN ep.key, ep.value, ep.source_name \
             ORDER BY ep.source_name, ep.key",
        )
        .context("prepare device_enrichment_context")?;

    Ok(conn
        .execute(
            &mut stmt,
            vec![("addr", Value::String(address.to_string()))],
        )
        .context("execute device_enrichment_context")?
        .map(|row| EnrichmentRow {
            key: read_str(&row[0]),
            value: read_str(&row[1]),
            source: read_str(&row[2]), // source_name column
        })
        .collect())
}

// ─── query 13: full topology edge list ───────────────────────────────────────

/// All CONNECTED_TO edges in the topology graph, annotated with device addresses.
///
/// Pattern: Device -[:HAS_INTERFACE]-> Interface -[:CONNECTED_TO]-> Interface <-[:HAS_INTERFACE]- Device
///
/// Used for bulk topology export; for path finding, prefer `shortest_topology_path`.
pub fn topology_edges(conn: &Connection<'_>) -> Result<Vec<TopologyEdge>> {
    let rows = conn
        .query(
            "MATCH (a:Interface)-[:CONNECTED_TO]->(b:Interface) \
             RETURN a.device_address, a.name, b.device_address, b.name",
        )
        .context("query topology_edges")?;

    Ok(rows
        .map(|row| TopologyEdge {
            src_device: read_str(&row[0]),
            src_iface: read_str(&row[1]),
            dst_device: read_str(&row[2]),
            dst_iface: read_str(&row[3]),
        })
        .collect())
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::test_fixtures::{TEST_ENV_DC_ID, TEST_SITE_DC_NAME, TestGraph};

    // ── q1: neighbors ──

    #[test]
    fn neighbors_of_spine1_returns_both_leaves() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let rows = neighbors_of_device(&conn, "10.0.0.1").unwrap();
        let addrs: Vec<&str> = rows.iter().map(|r| r.address.as_str()).collect();
        assert!(
            addrs.contains(&"10.0.0.3"),
            "leaf1 should be a neighbor of spine1"
        );
        assert!(
            addrs.contains(&"10.0.0.4"),
            "leaf2 should be a neighbor of spine1"
        );
    }

    // ── q2: shortest path ──

    #[test]
    fn shortest_path_same_device_returns_single_hop() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let path = shortest_topology_path(&conn, "10.0.0.1", "10.0.0.1")
            .unwrap()
            .unwrap();
        assert_eq!(path.hops, vec!["10.0.0.1"]);
        assert!(path.links.is_empty());
    }

    #[test]
    fn shortest_path_direct_neighbors_has_two_hops() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let path = shortest_topology_path(&conn, "10.0.0.1", "10.0.0.3")
            .unwrap()
            .unwrap();
        // spine1 → leaf1: exactly 2 device hops
        assert_eq!(path.hops.len(), 2);
        assert_eq!(path.hops[0], "10.0.0.1");
        assert_eq!(path.hops[1], "10.0.0.3");
        assert_eq!(
            path.links.len(),
            1,
            "one physical link for a direct neighbor"
        );
    }

    #[test]
    fn shortest_path_two_hops_traverses_spine() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        // leaf1 → leaf2 must go through a spine
        let path = shortest_topology_path(&conn, "10.0.0.3", "10.0.0.4")
            .unwrap()
            .unwrap();
        assert!(
            path.hops.len() >= 3,
            "leaf-to-leaf path must traverse at least one spine"
        );
        assert_eq!(path.links.len(), path.hops.len() - 1);
    }

    #[test]
    fn shortest_path_unreachable_returns_none() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        // 10.0.0.99 doesn't exist in the fixture
        let path = shortest_topology_path(&conn, "10.0.0.1", "10.0.0.99").unwrap();
        assert!(path.is_none(), "unknown dst should return None");
    }

    // ── q3: blast radius ──

    #[test]
    fn blast_radius_1hop_from_spine1_includes_both_leaves() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let br = blast_radius(&conn, "10.0.0.1", 1).unwrap();
        let addrs: Vec<&str> = br
            .reachable_devices
            .iter()
            .map(|d| d.address.as_str())
            .collect();
        assert!(addrs.contains(&"10.0.0.3"));
        assert!(addrs.contains(&"10.0.0.4"));
    }

    #[test]
    fn blast_radius_includes_apps_on_origin() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let br = blast_radius(&conn, "10.0.0.3", 1).unwrap();
        assert!(
            br.direct_apps.contains(&"app-web".to_string()),
            "leaf1 should have app-web"
        );
    }

    // ── q4: devices in environment ──

    #[test]
    fn devices_in_environment_returns_dc_devices() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let rows = devices_in_environment(&conn, TEST_ENV_DC_ID).unwrap();
        assert!(!rows.is_empty(), "DC environment should have devices");
        for row in &rows {
            assert_eq!(row.site_name, TEST_SITE_DC_NAME);
        }
    }

    // ── q5: detections in environment ──

    #[test]
    fn detections_in_environment_returns_rows_in_window() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let since = 0i64; // all time
        let rows = detections_in_environment(&conn, TEST_ENV_DC_ID, since).unwrap();
        assert!(!rows.is_empty(), "should find detections in DC environment");
    }

    // ── q6: applications on site ──

    #[test]
    fn applications_on_site_returns_apps() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let rows = applications_on_site(&conn, TEST_SITE_DC_NAME).unwrap();
        let names: Vec<&str> = rows.iter().map(|r| r.app_name.as_str()).collect();
        assert!(names.contains(&"app-web"));
    }

    // ── q7: missing enrichment ──

    #[test]
    fn devices_missing_enrichment_finds_unenriched_devices() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let missing = devices_missing_enrichment(&conn).unwrap();
        // spine2 (10.0.0.2) has no enrichment in the fixture
        assert!(
            missing.contains(&"10.0.0.2".to_string()),
            "spine2 should be missing enrichment"
        );
    }

    // ── q8: orphan devices ──

    #[test]
    fn orphan_devices_returns_isolated_device() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let orphans = orphan_devices(&conn).unwrap();
        // 10.0.0.9 is an isolated device in the fixture with no interfaces
        assert!(
            orphans.contains(&"10.0.0.9".to_string()),
            "isolated device should appear as orphan"
        );
    }

    // ── q9: detections without remediation ──

    #[test]
    fn detections_without_remediation_excludes_resolved() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let unresolved = detections_without_remediation(&conn, 50).unwrap();
        let ids: Vec<&str> = unresolved.iter().map(|r| r.id.as_str()).collect();
        // det-resolved is linked to a Remediation in the fixture
        assert!(
            !ids.contains(&"det-resolved"),
            "resolved detection should not appear"
        );
        // det-open has no remediation
        assert!(ids.contains(&"det-open"), "open detection should appear");
    }

    // ── q10: subscription health ──

    #[test]
    fn subscription_health_returns_paths_for_device() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let rows = subscription_health_for_device(&conn, "10.0.0.1").unwrap();
        assert!(!rows.is_empty(), "spine1 should have subscription paths");
    }

    // ── q11: co-firing detections ──

    #[test]
    fn co_firing_detections_returns_rows() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let rows = co_firing_detections(&conn, 0).unwrap();
        assert!(!rows.is_empty());
    }

    // ── q12: enrichment context ──

    #[test]
    fn device_enrichment_context_returns_properties() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let rows = device_enrichment_context(&conn, "10.0.0.1").unwrap();
        assert!(!rows.is_empty(), "spine1 should have enrichment properties");
        assert!(rows.iter().any(|r| r.source == "netbox"));
    }

    // ── q13: topology edges ──

    #[test]
    fn topology_edges_returns_all_links() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let edges = topology_edges(&conn).unwrap();
        // fixture has spine1-leaf1, spine1-leaf2, spine2-leaf1, spine2-leaf2
        assert!(edges.len() >= 4, "expected at least 4 topology edges");
    }
}
