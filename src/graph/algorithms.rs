// Graph algorithm library for the /api/graph/insights endpoint.
//
// All functions accept a &Connection<'_> and must run inside spawn_blocking.
// Algorithms: degree centrality, site dependency depth, detection correlation,
// subscription health by topology tier, orphan count.

use std::collections::HashMap;

use anyhow::{Context, Result};
use lbug::{Connection, Value};
use serde::Serialize;

use super::common::{read_str, ts};

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

    // Cross-site reachability: devices in OTHER sites reachable via topology.
    // Two separate MATCH clauses so lbug handles the variable-length segment first.
    let cross: HashMap<String, i64> = conn
        .query(
            "MATCH (s:Site)<-[:LOCATED_AT]-(d:Device) \
             MATCH (d)-[:HAS_INTERFACE|CONNECTED_TO*1..10]-(n:Device) \
             WHERE n.address <> d.address \
             MATCH (n)-[:LOCATED_AT]->(other_s:Site) \
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
pub fn detection_correlation(
    conn: &Connection<'_>,
    since_ns: i64,
) -> Result<Vec<CorrelationPair>> {
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
        let pair = pairs
            .iter()
            .find(|p| {
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
        assert!(insights.orphan_count >= 1, "isolated device should count as orphan");
    }
}
