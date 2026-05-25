// Graph algorithm library for the /api/graph/insights endpoint.
//
// All functions accept a &Connection<'_> and must run inside spawn_blocking.
// Algorithms: degree centrality, site dependency depth, detection correlation,
// subscription health by topology tier, orphan count.

use std::collections::HashMap;

use anyhow::{Context, Result};
use lbug::{Connection, Value};
use serde::Serialize;

use super::common::{now_ns, read_str, ts};

fn read_i64(v: &Value) -> i64 {
    match v {
        Value::Int64(n) => *n,
        Value::Int32(n) => *n as i64,
        _ => 0,
    }
}

// ─── result types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DeviceCentralityRow {
    pub address: String,
    pub hostname: String,
    /// Number of physical CONNECTED_TO links on this device (undirected degree).
    pub degree: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SiteDependencyRow {
    pub site_name: String,
    /// Devices located directly at this site.
    pub local_device_count: i64,
    /// Distinct devices in other sites reachable via topology links from this site.
    pub reachable_cross_site: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorrelationPair {
    pub rule_a: String,
    pub rule_b: String,
    /// Number of devices where both rules co-fired.
    pub co_fire_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TierHealthRow {
    /// "spine" (degree ≥ 4), "aggregation" (degree 2–3), "leaf" (degree 1), "isolated" (degree 0)
    pub tier: String,
    pub device_count: i64,
    pub active_subscriptions: i64,
    /// Devices with no subscriptions at all (not monitoring).
    pub unmonitored_devices: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphInsights {
    pub device_centrality: Vec<DeviceCentralityRow>,
    pub site_dependencies: Vec<SiteDependencyRow>,
    pub detection_correlations: Vec<CorrelationPair>,
    pub tier_health: Vec<TierHealthRow>,
    pub orphan_count: i64,
}

// ─── algorithms ───────────────────────────────────────────────────────────────

/// Device degree centrality — number of physical links (undirected) per device.
/// Spines and core PEs surface naturally at the top.
pub fn device_centrality(conn: &Connection<'_>) -> Result<Vec<DeviceCentralityRow>> {
    let rows = conn
        .query(
            "MATCH (d:Device)-[:HAS_INTERFACE]->(i:Interface)-[:CONNECTED_TO]-() \
             RETURN d.address, d.hostname, count(*) AS degree \
             ORDER BY degree DESC",
        )
        .context("device_centrality query")?;

    Ok(rows
        .map(|row| DeviceCentralityRow {
            address: read_str(&row[0]),
            hostname: read_str(&row[1]),
            degree: read_i64(&row[2]),
        })
        .collect())
}

/// For each site: local device count and distinct cross-site devices reachable
/// via topology links. A site with high cross-site reach is a critical hub.
pub fn site_dependency_depth(conn: &Connection<'_>) -> Result<Vec<SiteDependencyRow>> {
    // Local device count per site
    let local: Vec<(String, i64)> = conn
        .query(
            "MATCH (s:Site)<-[:LOCATED_AT]-(d:Device) \
             RETURN s.name, count(DISTINCT d) AS device_count \
             ORDER BY device_count DESC",
        )
        .context("site_dependency_depth local")?
        .map(|row| (read_str(&row[0]), read_i64(&row[1])))
        .collect();

    // Cross-site reachability: devices in OTHER sites reachable via direct LLDP links.
    let cross: HashMap<String, i64> = conn
        .query(
            "MATCH (s:Site)<-[:LOCATED_AT]-(d:Device)-[:HAS_INTERFACE]->(i:Interface)-[:CONNECTED_TO]-(ri:Interface)<-[:HAS_INTERFACE]-(n:Device)-[:LOCATED_AT]->(other_s:Site) \
             WHERE other_s.name <> s.name \
             RETURN s.name, count(DISTINCT n) AS reach",
        )
        .context("site_dependency_depth cross")?
        .map(|row| (read_str(&row[0]), read_i64(&row[1])))
        .collect();

    Ok(local
        .into_iter()
        .map(|(name, local_count)| SiteDependencyRow {
            reachable_cross_site: cross.get(&name).copied().unwrap_or(0),
            site_name: name,
            local_device_count: local_count,
        })
        .collect())
}

/// Detection rule pairs that co-fire on the same device within `since_ns`.
/// Ordered by co-fire frequency descending. Only different rule pairs (rule_a < rule_b).
pub fn detection_correlation(conn: &Connection<'_>, since_ns: i64) -> Result<Vec<CorrelationPair>> {
    let since_ts = ts(since_ns);
    let mut stmt = conn
        .prepare(
            "MATCH (d:Device)-[:TRIGGERED]->(e1:DetectionEvent) \
             MATCH (d)-[:TRIGGERED]->(e2:DetectionEvent) \
             WHERE e1.rule_id < e2.rule_id \
               AND e1.fired_at > $since \
               AND e2.fired_at > $since \
             RETURN e1.rule_id, e2.rule_id, count(DISTINCT d.address) AS co_count \
             ORDER BY co_count DESC \
             LIMIT 50",
        )
        .context("detection_correlation prepare")?;

    Ok(conn
        .execute(&mut stmt, vec![("since", since_ts)])
        .context("detection_correlation execute")?
        .map(|row| CorrelationPair {
            rule_a: read_str(&row[0]),
            rule_b: read_str(&row[1]),
            co_fire_count: read_i64(&row[2]),
        })
        .collect())
}

/// Subscription health grouped by topology tier.
/// Tier is derived from the device `role` field when set; falls back to undirected degree
/// (spine ≥4, aggregation 2–3, leaf 1, isolated 0) for devices with no role.
pub fn subscription_health_by_tier(conn: &Connection<'_>) -> Result<Vec<TierHealthRow>> {
    // Step 1: role and degree per device
    let role_map: HashMap<String, String> = conn
        .query("MATCH (d:Device) RETURN d.address, coalesce(d.role, '')")
        .context("tier health — roles")?
        .map(|row| (read_str(&row[0]), read_str(&row[1])))
        .collect();

    let degree_map: HashMap<String, i64> = conn
        .query(
            "MATCH (d:Device)-[:HAS_INTERFACE]->(i:Interface)-[:CONNECTED_TO]-() \
             RETURN d.address, count(*) AS degree",
        )
        .context("tier health — degree")?
        .map(|row| (read_str(&row[0]), read_i64(&row[1])))
        .collect();

    // Step 2: all devices
    let all_devices: Vec<String> = conn
        .query("MATCH (d:Device) RETURN d.address")
        .context("tier health — all devices")?
        .map(|row| read_str(&row[0]))
        .collect();

    // Step 3: active subscription count per device
    let active_map: HashMap<String, i64> = conn
        .query(
            "MATCH (d:Device)-[:HAS_SUBSCRIPTION_STATUS]->(ss:SubscriptionStatus) \
             WHERE ss.status = 'active' \
             RETURN d.address, count(*) AS active_count",
        )
        .context("tier health — active subscriptions")?
        .map(|row| (read_str(&row[0]), read_i64(&row[1])))
        .collect();

    // Step 4: bucket by tier — role wins over degree when set
    let tier_label = |addr: &str| -> &'static str {
        let role = role_map.get(addr).map(String::as_str).unwrap_or("");
        match role {
            "spine" | "super-spine" | "pe" | "route-reflector" => "spine",
            "leaf" | "vtep" | "access" | "ce" => "leaf",
            "aggregation" | "distribution" | "border" => "aggregation",
            _ => match degree_map.get(addr).copied().unwrap_or(0) {
                d if d >= 4 => "spine",
                d if d >= 2 => "aggregation",
                d if d >= 1 => "leaf",
                _ => "isolated",
            },
        }
    };

    // (device_count, active_subs, unmonitored_devices) per tier
    let mut buckets: HashMap<&'static str, (i64, i64, i64)> = [
        ("spine", (0, 0, 0)),
        ("aggregation", (0, 0, 0)),
        ("leaf", (0, 0, 0)),
        ("isolated", (0, 0, 0)),
    ]
    .into_iter()
    .collect();

    for addr in &all_devices {
        let tier = tier_label(addr);
        let entry = buckets.get_mut(tier).unwrap();
        entry.0 += 1; // device count
        let active = active_map.get(addr.as_str()).copied().unwrap_or(0);
        entry.1 += active;
        if active == 0 {
            entry.2 += 1; // unmonitored
        }
    }

    let order = ["spine", "aggregation", "leaf", "isolated"];
    Ok(order
        .iter()
        .map(|&tier| {
            let (devices, active, unmonitored) = buckets[tier];
            TierHealthRow {
                tier: tier.to_string(),
                device_count: devices,
                active_subscriptions: active,
                unmonitored_devices: unmonitored,
            }
        })
        .collect())
}

/// All algorithm results bundled for a single /api/graph/insights response.
/// Uses `since_ns = 0` (all time) for correlation so the lab has something to show.
pub fn graph_insights(conn: &Connection<'_>) -> Result<GraphInsights> {
    let centrality = device_centrality(conn)?;
    let site_deps = site_dependency_depth(conn)?;
    let correlations = detection_correlation(conn, 0)?;
    let tier_health = subscription_health_by_tier(conn)?;
    let orphan_count = super::queries::orphan_devices(conn)?.len() as i64;

    Ok(GraphInsights {
        device_centrality: centrality,
        site_dependencies: site_deps,
        detection_correlations: correlations,
        tier_health,
        orphan_count,
    })
}

// ─── graph quality ────────────────────────────────────────────────────────────

/// Coverage breakdown for a single quality dimension.
#[derive(Debug, Clone, Serialize)]
pub struct QualityCoverage {
    pub total: i64,
    pub covered: i64,
    /// 0.0–100.0
    pub pct: f64,
}

impl QualityCoverage {
    fn new(total: i64, covered: i64) -> Self {
        let pct = if total == 0 { 100.0 } else { (covered as f64 / total as f64 * 100.0 * 10.0).round() / 10.0 };
        Self { total, covered, pct }
    }
}

/// Devices below the quality threshold (for the weak-devices table).
#[derive(Debug, Clone, Serialize)]
pub struct WeakDevice {
    pub address: String,
    pub hostname: String,
    /// Which dimensions are missing: e.g. ["gnmi", "syslog", "bgp"]
    pub missing: Vec<String>,
}

/// Full graph quality report returned by GET /api/graph/quality.
#[derive(Debug, Clone, Serialize)]
pub struct GraphQuality {
    /// Weighted overall score 0–100.
    pub overall_score: f64,
    /// % of managed devices with an active gNMI subscription.
    pub gnmi_coverage: QualityCoverage,
    /// % of managed devices that sent syslog in the last 24 h.
    pub syslog_coverage: QualityCoverage,
    /// % of managed devices with BMP session active.
    pub bmp_coverage: QualityCoverage,
    /// % of interfaces with in/out counters populated and updated < 5 min ago.
    pub interface_counter_coverage: QualityCoverage,
    /// % of device-pair links confirmed by LLDP discovery.
    pub topology_link_coverage: QualityCoverage,
    /// % of devices with at least one BGP session mapped.
    pub bgp_mapped_coverage: QualityCoverage,
    /// % of devices with NetBox enrichment (`netbox_site` property set).
    pub netbox_enrichment_coverage: QualityCoverage,
    /// Devices missing one or more key signals.
    pub weak_devices: Vec<WeakDevice>,
    /// Unix ns when this snapshot was computed.
    pub computed_at_ns: i64,
}

/// Compute a point-in-time graph quality snapshot.
/// All queries run synchronously inside a spawn_blocking closure.
pub fn graph_quality(conn: &Connection<'_>) -> Result<GraphQuality> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0);

    // ── Total managed device count ─────────────────────────────────────────
    let total_devices: i64 = conn
        .query("MATCH (d:Device) RETURN count(d)")
        .context("quality: total_devices")?
        .next()
        .map(|r| read_i64(&r[0]))
        .unwrap_or(0);

    // ── gNMI active subscriptions ──────────────────────────────────────────
    let gnmi_active: i64 = conn
        .query(
            "MATCH (d:Device)-[:HAS_SUBSCRIPTION_STATUS]->(ss:SubscriptionStatus) \
             WHERE ss.status = 'observed' \
             RETURN count(DISTINCT d.address)",
        )
        .context("quality: gnmi_active")?
        .next()
        .map(|r| read_i64(&r[0]))
        .unwrap_or(0);

    // ── Syslog recently received (StateChangeEvent from syslog within 24 h) ─
    let syslog_cutoff = ts(now_ns - 86_400_000_000_000_i64);
    let mut syslog_stmt = conn
        .prepare(
            "MATCH (d:Device)-[:REPORTED_BY]->(e:StateChangeEvent) \
             WHERE e.source_type = 'syslog' AND e.occurred_at > $cutoff \
             RETURN count(DISTINCT d.address)",
        )
        .context("quality: syslog prepare")?;
    let syslog_active: i64 = conn
        .execute(&mut syslog_stmt, vec![("cutoff", syslog_cutoff.clone())])
        .context("quality: syslog execute")?
        .next()
        .map(|r| read_i64(&r[0]))
        .unwrap_or(0);

    // ── BMP sessions active ────────────────────────────────────────────────
    let bmp_active: i64 = conn
        .query(
            "MATCH (d:Device)-[:HAS_BMP_SESSION]->(b:BmpSession) \
             WHERE b.session_state = 'up' \
             RETURN count(DISTINCT d.address)",
        )
        .context("quality: bmp_active")?
        .next()
        .map(|r| read_i64(&r[0]))
        .unwrap_or(0);

    // ── Interface counter coverage (updated in last 5 min) ────────────────
    let iface_cutoff = ts(now_ns - 300_000_000_000_i64);
    let total_ifaces: i64 = conn
        .query("MATCH (:Device)-[:HAS_INTERFACE]->(i:Interface) RETURN count(i)")
        .context("quality: total_ifaces")?
        .next()
        .map(|r| read_i64(&r[0]))
        .unwrap_or(0);
    let mut iface_stmt = conn
        .prepare(
            "MATCH (:Device)-[:HAS_INTERFACE]->(i:Interface) \
             WHERE i.in_octets > 0 AND i.updated_at > $cutoff \
             RETURN count(i)",
        )
        .context("quality: iface_covered prepare")?;
    let ifaces_with_counters: i64 = conn
        .execute(&mut iface_stmt, vec![("cutoff", iface_cutoff)])
        .context("quality: iface_covered execute")?
        .next()
        .map(|r| read_i64(&r[0]))
        .unwrap_or(0);

    // ── Topology link coverage (LLDP-confirmed CONNECTED_TO edges) ────────
    let lldp_links: i64 = conn
        .query(
            "MATCH (:Interface)-[c:CONNECTED_TO]->(:Interface) \
             RETURN count(c)",
        )
        .context("quality: lldp_links")?
        .next()
        .map(|r| read_i64(&r[0]))
        .unwrap_or(0);
    // Expected: each device-to-device link appears as two directed edges
    let expected_links: i64 = conn
        .query(
            "MATCH (d:Device)-[:HAS_INTERFACE]->(i:Interface)-[:CONNECTED_TO]->(:Interface) \
             RETURN count(i)",
        )
        .context("quality: expected_links")?
        .next()
        .map(|r| read_i64(&r[0]))
        .unwrap_or(0);

    // ── BGP mapped (at least one BgpSession node per device) ──────────────
    let bgp_mapped: i64 = conn
        .query(
            "MATCH (d:Device)-[:PEERS_WITH]->(:BgpNeighbor) \
             RETURN count(DISTINCT d.address)",
        )
        .context("quality: bgp_mapped")?
        .next()
        .map(|r| read_i64(&r[0]))
        .unwrap_or(0);

    // ── NetBox enrichment (devices with at least one netbox EnrichmentProperty) ─
    let netbox_enriched: i64 = conn
        .query(
            "MATCH (d:Device)-[:HAS_ENRICHMENT_PROPERTY]->(ep:EnrichmentProperty) \
             WHERE ep.source_name = 'netbox' \
             RETURN count(DISTINCT d.address)",
        )
        .context("quality: netbox_enriched")?
        .next()
        .map(|r| read_i64(&r[0]))
        .unwrap_or(0);

    // ── Weak devices (missing >= 1 key signal) ────────────────────────────
    let gnmi_set: std::collections::HashSet<String> = conn
        .query(
            "MATCH (d:Device)-[:HAS_SUBSCRIPTION_STATUS]->(ss:SubscriptionStatus) \
             WHERE ss.status = 'observed' RETURN DISTINCT d.address",
        )
        .context("quality: gnmi_set")?
        .map(|r| read_str(&r[0]))
        .collect();

    let mut syslog_stmt2 = conn
        .prepare(
            "MATCH (d:Device)-[:REPORTED_BY]->(e:StateChangeEvent) \
             WHERE e.source_type = 'syslog' AND e.occurred_at > $cutoff \
             RETURN DISTINCT d.address",
        )
        .context("quality: syslog_set prepare")?;
    let syslog_set: std::collections::HashSet<String> = conn
        .execute(&mut syslog_stmt2, vec![("cutoff", syslog_cutoff.clone())])
        .context("quality: syslog_set execute")?
        .map(|r| read_str(&r[0]))
        .collect();

    let bgp_set: std::collections::HashSet<String> = conn
        .query(
            "MATCH (d:Device)-[:PEERS_WITH]->(:BgpNeighbor) RETURN DISTINCT d.address",
        )
        .context("quality: bgp_set")?
        .map(|r| read_str(&r[0]))
        .collect();

    let all_devs: Vec<(String, String)> = conn
        .query("MATCH (d:Device) RETURN d.address, coalesce(d.hostname, '')")
        .context("quality: all_devs")?
        .map(|r| (read_str(&r[0]), read_str(&r[1])))
        .collect();

    let mut weak_devices: Vec<WeakDevice> = Vec::new();
    for (addr, hostname) in &all_devs {
        let mut missing = Vec::new();
        if !gnmi_set.contains(addr.as_str()) { missing.push("gnmi".to_string()); }
        if !syslog_set.contains(addr.as_str()) { missing.push("syslog".to_string()); }
        if !bgp_set.contains(addr.as_str()) { missing.push("bgp".to_string()); }
        if !missing.is_empty() {
            weak_devices.push(WeakDevice {
                address: addr.clone(),
                hostname: hostname.clone(),
                missing,
            });
        }
    }
    weak_devices.sort_by(|a, b| b.missing.len().cmp(&a.missing.len()).then(a.address.cmp(&b.address)));

    // ── Coverage structs ───────────────────────────────────────────────────
    let gnmi_coverage = QualityCoverage::new(total_devices, gnmi_active);
    let syslog_coverage = QualityCoverage::new(total_devices, syslog_active);
    let bmp_coverage = QualityCoverage::new(total_devices, bmp_active);
    let interface_counter_coverage = QualityCoverage::new(total_ifaces, ifaces_with_counters);
    let topology_link_coverage = QualityCoverage::new(expected_links, lldp_links);
    let bgp_mapped_coverage = QualityCoverage::new(total_devices, bgp_mapped);
    let netbox_enrichment_coverage = QualityCoverage::new(total_devices, netbox_enriched);

    // ── Overall score (weighted average of key dimensions) ────────────────
    // Weights: gNMI 30, syslog 20, interface counters 20, topology 15, BGP 15
    let overall_score = (
        gnmi_coverage.pct * 0.30
        + syslog_coverage.pct * 0.20
        + interface_counter_coverage.pct * 0.20
        + topology_link_coverage.pct * 0.15
        + bgp_mapped_coverage.pct * 0.15
    ).min(100.0).max(0.0);
    let overall_score = (overall_score * 10.0).round() / 10.0;

    Ok(GraphQuality {
        overall_score,
        gnmi_coverage,
        syslog_coverage,
        bmp_coverage,
        interface_counter_coverage,
        topology_link_coverage,
        bgp_mapped_coverage,
        netbox_enrichment_coverage,
        weak_devices,
        computed_at_ns: now_ns,
    })
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::test_fixtures::TestGraph;

    #[test]
    fn centrality_spines_have_highest_degree() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let rows = device_centrality(&conn).unwrap();
        assert!(!rows.is_empty(), "centrality should return results");
        // Spine1 and spine2 each have 2 links; leaves have 1 each
        let spine1 = rows.iter().find(|r| r.address == "10.0.0.1").unwrap();
        let leaf1 = rows.iter().find(|r| r.address == "10.0.0.3").unwrap();
        assert!(
            spine1.degree >= leaf1.degree,
            "spines should have >= degree than leaves; spine={} leaf={}",
            spine1.degree,
            leaf1.degree,
        );
    }

    #[test]
    fn centrality_isolated_device_not_in_results() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let rows = device_centrality(&conn).unwrap();
        // Isolated device (10.0.0.9) has no interfaces/links — should not appear
        assert!(
            !rows.iter().any(|r| r.address == "10.0.0.9"),
            "isolated device should not appear in centrality (no links)",
        );
    }

    #[test]
    fn site_dependency_returns_dc_and_sp_sites() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let rows = site_dependency_depth(&conn).unwrap();
        let names: Vec<&str> = rows.iter().map(|r| r.site_name.as_str()).collect();
        assert!(names.contains(&"dc-site-1"), "DC site should appear");
        assert!(names.contains(&"sp-site-1"), "SP site should appear");
    }

    #[test]
    fn site_dependency_dc_has_correct_local_count() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let rows = site_dependency_depth(&conn).unwrap();
        // DC site has: spine1, spine2, leaf1, leaf2, leaf3, leaf4, isolated = 7 devices
        let dc = rows.iter().find(|r| r.site_name == "dc-site-1").unwrap();
        assert_eq!(dc.local_device_count, 7, "DC should have 7 devices");
    }

    #[test]
    fn detection_correlation_finds_cofiring_pair() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let pairs = detection_correlation(&conn, 0).unwrap();
        // leaf1 has both bgp_session_down and interface_down detections
        let pair = pairs.iter().find(|p| {
            (p.rule_a == "bgp_session_down" && p.rule_b == "interface_down")
                || (p.rule_a == "interface_down" && p.rule_b == "bgp_session_down")
        });
        assert!(
            pair.is_some(),
            "should find bgp_session_down/interface_down co-fire pair; got {:?}",
            pairs,
        );
    }

    #[test]
    fn subscription_health_by_tier_has_four_tiers() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let rows = subscription_health_by_tier(&conn).unwrap();
        assert_eq!(rows.len(), 4, "should always return all four tiers");
        let tiers: Vec<&str> = rows.iter().map(|r| r.tier.as_str()).collect();
        assert!(tiers.contains(&"spine"));
        assert!(tiers.contains(&"isolated"));
    }

    #[test]
    fn subscription_health_active_subscription_counted() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let rows = subscription_health_by_tier(&conn).unwrap();
        // In the 2-spine/4-leaf fixture every connected device has degree 2 (undirected),
        // so spine1 and spine2 land in the "aggregation" tier.
        // Spine1 has one active subscription, so aggregation must have >= 1.
        let agg = rows.iter().find(|r| r.tier == "aggregation").unwrap();
        assert!(
            agg.active_subscriptions >= 1,
            "aggregation tier should have at least 1 active subscription (spine1 degree=2)",
        );
    }

    #[test]
    fn graph_insights_runs_without_error() {
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let insights = graph_insights(&conn).unwrap();
        assert!(!insights.device_centrality.is_empty());
        assert!(
            insights.orphan_count >= 1,
            "isolated device should count as orphan"
        );
    }
}

// ─── Performance Baseline Algorithms ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct PerformanceBaselineRow {
    pub id: String,
    pub device_address: String,
    pub metric_type: String,
    pub metric_key: String,
    pub baseline_mean: f64,
    pub baseline_stddev: f64,
    pub baseline_min: f64,
    pub baseline_max: f64,
    pub sample_count: i64,
    pub computed_at_ns: i64,
    pub lookback_hours: i32,
    pub confidence_level: f64,
}

/// Compute interface utilization baseline for a device over lookback period
pub fn compute_interface_utilization_baseline(
    conn: &Connection<'_>,
    device_address: &str,
    lookback_hours: i32,
) -> Result<Vec<PerformanceBaselineRow>> {
    let _cutoff_ns = now_ns() - (lookback_hours as i64 * 3600 * 1_000_000_000);
    
    let rows = conn
        .query(
            "MATCH (d:Device {address: $device_address})-[:HAS_INTERFACE]->(i:Interface) \
             WHERE i.updated_at_ns > $cutoff_ns \
             RETURN i.name, i.in_octets, i.out_octets, i.speed, i.updated_at_ns \
             ORDER BY i.updated_at_ns",
        )
        .context("interface baseline query")?;

    // Group by interface and compute statistics
    let mut interface_data: HashMap<String, Vec<(f64, i64)>> = HashMap::new();
    
    for row in rows {
        let if_name = read_str(&row[0]);
        let in_octets = read_i64(&row[1]) as f64;
        let out_octets = read_i64(&row[2]) as f64;
        let speed = read_i64(&row[3]) as f64;
        let timestamp = read_i64(&row[4]);
        
        if speed > 0.0 {
            let utilization = ((in_octets + out_octets) * 8.0 / speed) * 100.0; // Percentage
            interface_data.entry(if_name).or_insert_with(Vec::new).push((utilization, timestamp));
        }
    }
    
    let mut baselines = Vec::new();
    
    for (if_name, samples) in interface_data {
        if samples.len() < 10 {
            continue; // Need sufficient samples for meaningful baseline
        }
        
        let values: Vec<f64> = samples.iter().map(|(v, _)| *v).collect();
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
        let stddev = variance.sqrt();
        let min_val = values.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        let max_val = values.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
        
        let baseline = PerformanceBaselineRow {
            id: format!("baseline-{}-{}-{}", device_address, "interface_utilization", if_name),
            device_address: device_address.to_string(),
            metric_type: "interface_utilization".to_string(),
            metric_key: if_name,
            baseline_mean: mean,
            baseline_stddev: stddev,
            baseline_min: min_val,
            baseline_max: max_val,
            sample_count: samples.len() as i64,
            computed_at_ns: now_ns(),
            lookback_hours,
            confidence_level: 0.95,
        };
        
        baselines.push(baseline);
    }
    
    Ok(baselines)
}

/// Store computed baseline in the graph database
pub fn store_performance_baseline(
    conn: &Connection<'_>,
    baseline: &PerformanceBaselineRow,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "MERGE (pb:PerformanceBaseline {id: $id}) \
             SET pb.device_address = $device_address, \
                 pb.metric_type = $metric_type, \
                 pb.metric_key = $metric_key, \
                 pb.baseline_mean = $baseline_mean, \
                 pb.baseline_stddev = $baseline_stddev, \
                 pb.baseline_min = $baseline_min, \
                 pb.baseline_max = $baseline_max, \
                 pb.sample_count = $sample_count, \
                 pb.computed_at_ns = $computed_at_ns, \
                 pb.lookback_hours = $lookback_hours, \
                 pb.confidence_level = $confidence_level",
        )
        .context("prepare store performance baseline")?;
    conn.execute(&mut stmt, vec![
        ("id", Value::String(baseline.id.clone())),
        ("device_address", Value::String(baseline.device_address.clone())),
        ("metric_type", Value::String(baseline.metric_type.clone())),
        ("metric_key", Value::String(baseline.metric_key.clone())),
        ("baseline_mean", Value::Double(baseline.baseline_mean)),
        ("baseline_stddev", Value::Double(baseline.baseline_stddev)),
        ("baseline_min", Value::Double(baseline.baseline_min)),
        ("baseline_max", Value::Double(baseline.baseline_max)),
        ("sample_count", Value::Int64(baseline.sample_count)),
        ("computed_at_ns", Value::Int64(baseline.computed_at_ns)),
        ("lookback_hours", Value::Int32(baseline.lookback_hours)),
        ("confidence_level", Value::Double(baseline.confidence_level)),
    ])
    .context("store performance baseline")?;
    
    // Create relationship to device
    let mut stmt = conn
        .prepare(
            "MATCH (d:Device {address: $device_address}), (pb:PerformanceBaseline {id: $id}) \
             MERGE (d)-[:HAS_BASELINE {updated_at: $updated_at}]->(pb)",
        )
        .context("prepare baseline relationship")?;
    conn.execute(&mut stmt, vec![
        ("device_address", Value::String(baseline.device_address.clone())),
        ("id", Value::String(baseline.id.clone())),
        ("updated_at", Value::Int64(now_ns())),
    ])
    .context("create baseline relationship")?;
    
    Ok(())
}

/// Check if current value exceeds baseline threshold (mean + 2*stddev)
pub fn check_baseline_drift(
    conn: &Connection<'_>,
    device_address: &str,
    metric_type: &str,
    metric_key: &str,
    current_value: f64,
) -> Result<bool> {
    let mut stmt = conn
        .prepare(
            "MATCH (d:Device {address: $device_address})-[:HAS_BASELINE]->(pb:PerformanceBaseline) \
             WHERE pb.metric_type = $metric_type AND pb.metric_key = $metric_key \
             RETURN pb.baseline_mean, pb.baseline_stddev",
        )
        .context("prepare baseline drift check query")?;
    let mut rows = conn
        .execute(
            &mut stmt,
            vec![
                ("device_address", Value::String(device_address.to_string())),
                ("metric_type", Value::String(metric_type.to_string())),
                ("metric_key", Value::String(metric_key.to_string())),
            ],
        )
        .context("baseline drift check query")?;
    
    if let Some(row) = rows.next() {
        let baseline_mean = read_f64(&row[0]);
        let baseline_stddev = read_f64(&row[1]);
        let threshold = baseline_mean + 2.0 * baseline_stddev;
        Ok(current_value > threshold)
    } else {
        Ok(false) // No baseline available
    }
}

fn read_f64(v: &Value) -> f64 {
    match v {
        Value::Double(n) => *n,
        Value::Float(n) => *n as f64,
        Value::Int64(n) => *n as f64,
        Value::Int32(n) => *n as f64,
        _ => 0.0,
    }
}
