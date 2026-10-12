use std::sync::{Arc, Mutex};
use duckdb::Row;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::duck::RouteStore;

/// A flattened route row returned by queries
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRow {
    pub occurred_at:  String,
    pub speaker_addr: String,
    pub peer_addr:    String,
    pub peer_as:      u32,
    pub rib_type:     String,
    pub action:       String,
    pub prefix:       String,
    pub afi:          String,
    pub origin:       Option<String>,
    pub as_path:      Option<String>,
    pub as_path_len:  Option<u16>,
    pub next_hop:     Option<String>,
    pub local_pref:   Option<u32>,
    pub med:          Option<u32>,
    pub communities:  Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerEventRow {
    pub occurred_at:  String,
    pub speaker_addr: String,
    pub peer_addr:    String,
    pub peer_as:      Option<u32>,
    pub event_type:   String,
    pub hold_time:    Option<u16>,
    pub reason:       Option<String>,
}

pub struct QueryEngine {
    store: Arc<Mutex<RouteStore>>,
}

fn now_minus(interval: &str) -> String {
    format!("CAST(NOW() AS TIMESTAMP) - INTERVAL '{interval}'")
}

impl QueryEngine {
    pub fn new(store: Arc<Mutex<RouteStore>>) -> Self { Self { store } }

    /// Latest announced routes for a peer (current RIB snapshot)
    pub fn current_rib(&self, peer_addr: &str, rib_type: Option<&str>, limit: usize) -> Result<Vec<RouteRow>> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let rib_filter = rib_type.map(|r| format!("AND rib_type = '{}'", r)).unwrap_or_default();
        let sql = format!(
            r#"SELECT occurred_at, speaker_addr, peer_addr, peer_as, rib_type, action,
                      prefix, afi, origin, as_path, as_path_len, next_hop, local_pref, med, communities
               FROM (
                 SELECT *, ROW_NUMBER() OVER (PARTITION BY prefix ORDER BY occurred_at DESC) AS rn
                 FROM route_events
                 WHERE peer_addr = '{}' {}
               ) t
               WHERE rn = 1 AND action = 'announce'
               ORDER BY prefix
               LIMIT {}"#,
            peer_addr, rib_filter, limit
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_route_row)?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// Route history for a specific prefix
    pub fn prefix_history(&self, prefix: &str, limit: usize) -> Result<Vec<RouteRow>> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let sql = format!(
            "SELECT occurred_at, speaker_addr, peer_addr, peer_as, rib_type, action,
                    prefix, afi, origin, as_path, as_path_len, next_hop, local_pref, med, communities
             FROM route_events WHERE prefix = '{prefix}'
             ORDER BY occurred_at DESC LIMIT {limit}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_route_row)?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// Route changes in a time window
    pub fn route_changes(&self, since: &str, until: Option<&str>, limit: usize) -> Result<Vec<RouteRow>> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let until_clause = until.map(|u| format!("AND occurred_at <= '{u}'")).unwrap_or_default();
        let sql = format!(
            "SELECT occurred_at, speaker_addr, peer_addr, peer_as, rib_type, action,
                    prefix, afi, origin, as_path, as_path_len, next_hop, local_pref, med, communities
             FROM route_events
             WHERE occurred_at >= '{since}' {until_clause}
             ORDER BY occurred_at DESC LIMIT {limit}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_route_row)?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// Top N most active prefixes by churn (announce+withdraw count)
    pub fn top_churning_prefixes(&self, limit: usize) -> Result<Vec<(String, u64)>> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let sql = format!(
            "SELECT prefix, COUNT(*) AS churn FROM route_events
             GROUP BY prefix ORDER BY churn DESC LIMIT {limit}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            let prefix: String = row.get(0)?;
            let churn: u64     = row.get(1)?;
            Ok((prefix, churn))
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// AS path popularity (how many times each AS appears in paths)
    pub fn as_origin_counts(&self, limit: usize) -> Result<Vec<(u32, u64)>> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let sql = format!(
            r#"SELECT CAST(trim(last) AS UINTEGER) AS origin_asn, COUNT(*) AS cnt
               FROM (
                 SELECT string_split(as_path, ' ')[-1] AS last
                 FROM route_events
                 WHERE action = 'announce' AND as_path IS NOT NULL AND as_path <> ''
               ) sub
               WHERE last IS NOT NULL AND last <> ''
               GROUP BY origin_asn
               ORDER BY cnt DESC
               LIMIT {limit}"#
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            let asn: u32 = row.get(0)?;
            let cnt: u64 = row.get(1)?;
            Ok((asn, cnt))
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// Prefix timeline — announce/withdraw events bucketed by hour for the last N days
    pub fn prefix_timeline(&self, prefix: &str, days: u32) -> Result<Vec<serde_json::Value>> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let sql = format!(
            r#"SELECT
                time_bucket(INTERVAL '1 hour', occurred_at) AS bucket,
                action,
                COUNT(*) AS event_count
               FROM route_events
               WHERE prefix = '{prefix}'
                 AND occurred_at >= {since_expr}
               GROUP BY bucket, action
               ORDER BY bucket"#,
            since_expr = now_minus(&format!("{days} days"))
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            let bucket: String    = row.get(0)?;
            let action: String    = row.get(1)?;
            let count:  u64       = row.get(2)?;
            Ok(serde_json::json!({ "bucket": bucket, "action": action, "count": count }))
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// Which peers currently see a prefix and their most recent AS_PATH
    pub fn prefix_peers(&self, prefix: &str) -> Result<Vec<serde_json::Value>> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let sql = format!(
            r#"SELECT peer_addr, peer_as, as_path, next_hop, local_pref, communities,
                      occurred_at
               FROM (
                 SELECT *, ROW_NUMBER() OVER (PARTITION BY peer_addr ORDER BY occurred_at DESC) AS rn
                 FROM route_events
                 WHERE prefix = '{prefix}' AND action = 'announce'
               ) t WHERE rn = 1
               ORDER BY peer_addr"#
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            let peer_addr:  String         = row.get(0)?;
            let peer_as:    u32            = row.get(1)?;
            let as_path:    Option<String> = row.get(2)?;
            let next_hop:   Option<String> = row.get(3)?;
            let local_pref: Option<u32>    = row.get(4)?;
            let communities: Option<String> = row.get(5)?;
            let occurred_at: String        = row.get(6)?;
            Ok(serde_json::json!({
                "peer_addr": peer_addr, "peer_as": peer_as,
                "as_path": as_path, "next_hop": next_hop,
                "local_pref": local_pref, "communities": communities,
                "last_seen": occurred_at
            }))
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// Convergence times for the last N events on a prefix
    /// Returns the gap in seconds between sequential announce events per peer
    pub fn prefix_convergence(&self, prefix: &str, limit: usize) -> Result<Vec<serde_json::Value>> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let sql = format!(
            r#"SELECT
                peer_addr,
                occurred_at,
                action,
                epoch(occurred_at) - lag(epoch(occurred_at)) OVER (PARTITION BY peer_addr ORDER BY occurred_at) AS gap_secs
               FROM route_events
               WHERE prefix = '{prefix}'
               ORDER BY occurred_at DESC
               LIMIT {limit}"#
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            let peer_addr:  String     = row.get(0)?;
            let occurred_at: String    = row.get(1)?;
            let action:     String     = row.get(2)?;
            let gap_secs:   Option<f64> = row.get(3)?;
            Ok(serde_json::json!({
                "peer_addr": peer_addr,
                "occurred_at": occurred_at,
                "action": action,
                "gap_secs": gap_secs
            }))
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// Peer session timeline — up/down events in last N days with duration
    pub fn peer_session_timeline(&self, peer_addr: &str, days: u32) -> Result<Vec<serde_json::Value>> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let sql = format!(
            r#"SELECT occurred_at, event_type, reason,
                      epoch(lead(occurred_at) OVER (ORDER BY occurred_at)) - epoch(occurred_at) AS duration_secs
               FROM peer_events
               WHERE peer_addr = '{peer_addr}'
                 AND occurred_at >= {since_expr}
               ORDER BY occurred_at DESC"#,
            since_expr = now_minus(&format!("{days} days"))
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            let occurred_at:   String     = row.get(0)?;
            let event_type:    String     = row.get(1)?;
            let reason:        Option<String> = row.get(2)?;
            let duration_secs: Option<f64>   = row.get(3)?;
            Ok(serde_json::json!({
                "occurred_at": occurred_at,
                "event_type": event_type,
                "reason": reason,
                "duration_secs": duration_secs
            }))
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// RPKI analysis — breakdown by validity + per-peer invalid rates
    pub fn rpki_analysis(&self) -> Result<serde_json::Value> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let has_rpki_validity: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM information_schema.columns WHERE table_name = 'route_events' AND column_name = 'rpki_validity'",
            [], |row| row.get(0),
        ).unwrap_or(false);
        if !has_rpki_validity {
            return Ok(serde_json::json!({ "breakdown": [], "per_peer": [] }));
        }

        // Overall breakdown
        let breakdown_sql = r#"SELECT rpki_validity, COUNT(DISTINCT prefix) AS cnt
            FROM (
              SELECT prefix, rpki_validity,
                     ROW_NUMBER() OVER (PARTITION BY prefix ORDER BY occurred_at DESC) AS rn
              FROM route_events WHERE action = 'announce'
            ) t WHERE rn = 1
            GROUP BY rpki_validity ORDER BY cnt DESC"#;
        let mut stmt = conn.prepare(breakdown_sql)?;
        let breakdown: Vec<serde_json::Value> = stmt.query_map([], |row| {
            let validity: Option<String> = row.get(0)?;
            let cnt: u64 = row.get(1)?;
            Ok(serde_json::json!({ "validity": validity.unwrap_or_else(|| "unknown".into()), "count": cnt }))
        })?.filter_map(|r| r.ok()).collect();

        // Per-peer invalid rate
        let peer_sql = r#"SELECT peer_addr, peer_as,
                SUM(CASE WHEN rpki_validity='invalid' THEN 1 ELSE 0 END)::FLOAT / COUNT(*) AS invalid_rate,
                COUNT(*) AS total
            FROM route_events WHERE action = 'announce'
            GROUP BY peer_addr, peer_as ORDER BY invalid_rate DESC LIMIT 20"#;
        let mut stmt2 = conn.prepare(peer_sql)?;
        let per_peer: Vec<serde_json::Value> = stmt2.query_map([], |row| {
            let peer_addr:    String = row.get(0)?;
            let peer_as:      u32    = row.get(1)?;
            let invalid_rate: f64    = row.get(2)?;
            let total:        u64    = row.get(3)?;
            Ok(serde_json::json!({ "peer_addr": peer_addr, "peer_as": peer_as, "invalid_rate": invalid_rate, "total": total }))
        })?.filter_map(|r| r.ok()).collect();

        Ok(serde_json::json!({ "breakdown": breakdown, "per_peer": per_peer }))
    }

    /// Policy analysis — pre vs post-policy diff for a peer
    pub fn policy_delta(&self, peer_addr: &str) -> Result<serde_json::Value> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let sql = format!(
            r#"SELECT rib_type, COUNT(DISTINCT prefix) AS prefix_count
               FROM (
                 SELECT prefix, rib_type,
                        ROW_NUMBER() OVER (PARTITION BY prefix, rib_type ORDER BY occurred_at DESC) AS rn
                 FROM route_events WHERE peer_addr = '{peer_addr}' AND action = 'announce'
               ) t WHERE rn = 1
               GROUP BY rib_type"#
        );
        let mut stmt = conn.prepare(&sql)?;
        let by_rib: Vec<serde_json::Value> = stmt.query_map([], |row| {
            let rib_type:      String = row.get(0)?;
            let prefix_count:  u64   = row.get(1)?;
            Ok(serde_json::json!({ "rib_type": rib_type, "prefix_count": prefix_count }))
        })?.filter_map(|r| r.ok()).collect();

        Ok(serde_json::json!({ "peer_addr": peer_addr, "by_rib_type": by_rib }))
    }

    /// ML anomaly detections from the Python pipeline (most recent first)
    pub fn ml_anomalies(&self, limit: usize, kind: Option<&str>) -> Result<Vec<Value>> {
        let locked = self.store.lock().unwrap();
        let conn   = locked.conn();
        let kind_filter = kind.map(|k| format!("AND kind = '{k}'")).unwrap_or_default();
        let sql = format!(
            r#"SELECT detected_at, kind, prefix, peer_addr, score, description, severity
               FROM ml_anomalies
               WHERE 1=1 {kind_filter}
               ORDER BY detected_at DESC
               LIMIT {limit}"#
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            let detected_at: String    = row.get(0)?;
            let kind:        String    = row.get(1)?;
            let prefix:      Option<String> = row.get(2)?;
            let peer_addr:   Option<String> = row.get(3)?;
            let score:       Option<f64>    = row.get(4)?;
            let description: Option<String> = row.get(5)?;
            let severity:    Option<String> = row.get(6)?;
            Ok(serde_json::json!({
                "detected_at": detected_at, "kind": kind,
                "prefix": prefix, "peer_addr": peer_addr,
                "score": score, "description": description,
                "severity": severity
            }))
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    // ─── RV6-5 new query methods ──────────────────────────────────────────────

    /// SR Policy list (RV6-5) — reads from srpolicy_events table
    pub fn srpolicy_list(&self, limit: usize) -> Result<Vec<serde_json::Value>> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let table_exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM information_schema.tables WHERE table_name = 'srpolicy_events'",
            [], |row| row.get(0),
        ).unwrap_or(false);
        if !table_exists { return Ok(Vec::new()); }
        let sql = format!(
            r#"SELECT occurred_at, speaker_addr, peer_addr, peer_as, action,
                      endpoint, color, preference, bsid, segment_list, distinguisher
               FROM srpolicy_events
               ORDER BY occurred_at DESC
               LIMIT {limit}"#
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(serde_json::json!({
                "occurred_at":  row.get::<_,String>(0)?,
                "speaker_addr": row.get::<_,String>(1)?,
                "peer_addr":    row.get::<_,String>(2)?,
                "peer_as":      row.get::<_,u32>(3)?,
                "action":       row.get::<_,String>(4)?,
                "endpoint":     row.get::<_,Option<String>>(5)?,
                "color":        row.get::<_,Option<u32>>(6)?,
                "preference":   row.get::<_,Option<u32>>(7)?,
                "bsid":         row.get::<_,Option<String>>(8)?,
                "segment_list": row.get::<_,Option<String>>(9)?,
                "distinguisher": row.get::<_,Option<u32>>(10)?,
            }))
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// SR Policy list filtered by peer address
    pub fn srpolicy_by_peer(&self, peer_addr: &str, limit: usize) -> Result<Vec<serde_json::Value>> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let table_exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM information_schema.tables WHERE table_name = 'srpolicy_events'",
            [], |row| row.get(0),
        ).unwrap_or(false);
        if !table_exists { return Ok(Vec::new()); }
        let sql = format!(
            r#"SELECT occurred_at, speaker_addr, peer_addr, peer_as, action,
                      endpoint, color, preference, bsid, segment_list, distinguisher
               FROM srpolicy_events
               WHERE peer_addr = '{peer_addr}'
               ORDER BY occurred_at DESC
               LIMIT {limit}"#
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(serde_json::json!({
                "occurred_at":  row.get::<_,String>(0)?,
                "speaker_addr": row.get::<_,String>(1)?,
                "peer_addr":    row.get::<_,String>(2)?,
                "peer_as":      row.get::<_,u32>(3)?,
                "action":       row.get::<_,String>(4)?,
                "endpoint":     row.get::<_,Option<String>>(5)?,
                "color":        row.get::<_,Option<u32>>(6)?,
                "preference":   row.get::<_,Option<u32>>(7)?,
                "bsid":         row.get::<_,Option<String>>(8)?,
                "segment_list": row.get::<_,Option<String>>(9)?,
                "distinguisher": row.get::<_,Option<u32>>(10)?,
            }))
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// AS-path graph data for Sankey rendering (RV6-5)
    /// Returns { nodes: [{id, label}], links: [{source, target, value}] }
    pub fn aspath_graph(&self, filter_asn: Option<u32>, peer: Option<&str>, limit: usize) -> Result<serde_json::Value> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let peer_clause = peer.map(|p| format!("AND peer_addr = '{p}'")).unwrap_or_default();
        let asn_clause  = filter_asn.map(|a| format!("AND as_path LIKE '%{a}%'")).unwrap_or_default();
        let sql = format!(
            r#"SELECT as_path FROM route_events
               WHERE action = 'announce' AND as_path IS NOT NULL AND as_path <> ''
               {peer_clause} {asn_clause}
               ORDER BY occurred_at DESC
               LIMIT {limit}"#
        );
        let mut stmt = conn.prepare(&sql)?;
        // Build edge-count map from consecutive ASN pairs
        let mut edge_counts: std::collections::HashMap<(u32, u32), u64> = std::collections::HashMap::new();
        let _ = stmt.query_map([], |row| {
            let path: String = row.get(0)?;
            let asns: Vec<u32> = path.split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect();
            for pair in asns.windows(2) {
                *edge_counts.entry((pair[0], pair[1])).or_insert(0) += 1;
            }
            Ok(())
        })?.for_each(|_| {});

        let mut asn_set: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for (src, dst) in edge_counts.keys() {
            asn_set.insert(*src);
            asn_set.insert(*dst);
        }
        let nodes: Vec<serde_json::Value> = asn_set.iter()
            .map(|asn| serde_json::json!({ "id": asn.to_string(), "label": format!("AS{asn}") }))
            .collect();
        let links: Vec<serde_json::Value> = edge_counts.iter()
            .map(|((src, dst), count)| serde_json::json!({
                "source": src.to_string(),
                "target": dst.to_string(),
                "value":  count,
            }))
            .collect();
        Ok(serde_json::json!({ "nodes": nodes, "links": links }))
    }

    /// BMP statistics counter history (RV6-5)
    pub fn bmp_stats_history(&self, peer_addr: Option<&str>, limit: usize) -> Result<Vec<serde_json::Value>> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let peer_clause = peer_addr.map(|p| format!("WHERE peer_addr = '{p}'")).unwrap_or_default();
        let sql = format!(
            r#"SELECT
                   CAST(occurred_at AS VARCHAR) AS occurred_at,
                   speaker_addr,
                   peer_addr,
                   counter_name,
                   counter_value,
                   CAST(stat_type AS INTEGER) AS stat_type,
                   CAST(afi AS INTEGER) AS afi,
                   CAST(safi AS INTEGER) AS safi
               FROM stats_events
               {peer_clause}
               ORDER BY occurred_at DESC
               LIMIT {limit}"#
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            let stat_type: Option<i64> = row.get(5)?;
            let afi: Option<i64> = row.get(6)?;
            let safi: Option<i64> = row.get(7)?;
            Ok(serde_json::json!({
                "occurred_at":   row.get::<_,String>(0)?,
                "speaker_addr":  row.get::<_,String>(1)?,
                "peer_addr":     row.get::<_,String>(2)?,
                "counter_name":  row.get::<_,String>(3)?,
                "counter_value": row.get::<_,u64>(4)?,
                "stat_type":     stat_type.and_then(|v| u16::try_from(v).ok()),
                "afi":           afi.and_then(|v| u16::try_from(v).ok()),
                "safi":          safi.and_then(|v| u8::try_from(v).ok()),
            }))
        })?.collect::<duckdb::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// RPKI coverage analysis (RV6-5)
    /// Returns what % of announced prefixes have a matching ROA.
    pub fn rpki_coverage(&self) -> Result<serde_json::Value> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let has_rpki_validity: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM information_schema.columns WHERE table_name = 'route_events' AND column_name = 'rpki_validity'",
            [], |row| row.get(0),
        ).unwrap_or(false);
        if !has_rpki_validity {
            return Ok(serde_json::json!({
                "total_prefixes": 0,
                "covered": 0,
                "not_covered": 0,
                "valid": 0,
                "invalid": 0,
                "coverage_pct": 0.0,
            }));
        }
        let sql = r#"
            SELECT
                COUNT(DISTINCT prefix) AS total_prefixes,
                SUM(CASE WHEN rpki_validity IN ('valid','invalid') THEN 1 ELSE 0 END) AS covered,
                SUM(CASE WHEN rpki_validity = 'valid'     THEN 1 ELSE 0 END) AS valid,
                SUM(CASE WHEN rpki_validity = 'invalid'   THEN 1 ELSE 0 END) AS invalid,
                SUM(CASE WHEN rpki_validity = 'not-found' OR rpki_validity IS NULL THEN 1 ELSE 0 END) AS not_covered
            FROM (
                SELECT prefix, rpki_validity,
                       ROW_NUMBER() OVER (PARTITION BY prefix ORDER BY occurred_at DESC) AS rn
                FROM route_events WHERE action = 'announce'
            ) t WHERE rn = 1
        "#;
        let mut stmt = conn.prepare(sql)?;
        let row = stmt.query_row([], |row| {
            let total:       u64 = row.get(0)?;
            let covered:     u64 = row.get(1)?;
            let valid:       u64 = row.get(2)?;
            let invalid:     u64 = row.get(3)?;
            let not_covered: u64 = row.get(4)?;
            Ok((total, covered, valid, invalid, not_covered))
        })?;
        let (total, covered, valid, invalid, not_covered) = row;
        let coverage_pct = if total > 0 { covered as f64 / total as f64 * 100.0 } else { 0.0 };
        Ok(serde_json::json!({
            "total_prefixes":  total,
            "covered":         covered,
            "not_covered":     not_covered,
            "valid":           valid,
            "invalid":         invalid,
            "coverage_pct":    (coverage_pct * 10.0).round() / 10.0,
        }))
    }

    /// Peer session events (up/down history)
    pub fn peer_history(&self, peer_addr: &str) -> Result<Vec<PeerEventRow>> {
        let locked = self.store.lock().unwrap();
        let conn = locked.conn();
        let sql = format!(
            "SELECT occurred_at, speaker_addr, peer_addr, peer_as, event_type, hold_time, reason
             FROM peer_events WHERE peer_addr = '{peer_addr}'
             ORDER BY occurred_at DESC LIMIT 200"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(PeerEventRow {
                occurred_at:  row.get(0)?,
                speaker_addr: row.get(1)?,
                peer_addr:    row.get(2)?,
                peer_as:      row.get(3)?,
                event_type:   row.get(4)?,
                hold_time:    row.get(5)?,
                reason:       row.get(6)?,
            })
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }
}

// ─── Convergence events (RV7-UI6) ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergenceEventRow {
    pub event_id:           String,
    pub started_at:         String,
    pub eor_at:             Option<String>,
    pub convergence_ms:     Option<f64>,
    pub speaker_addr:       String,
    pub peer_addr:          String,
    pub trigger_type:       Option<String>,
    pub affected_prefixes:  Option<u32>,
    pub recovered_prefixes: Option<u32>,
    pub unreachable_after:  Option<u32>,
}

impl QueryEngine {
    /// Query convergence events for a peer within the last `hours` hours.
    pub fn convergence_events(
        &self,
        peer_addr: Option<&str>,
        hours:     u32,
        limit:     u32,
    ) -> Result<Vec<ConvergenceEventRow>> {
        let locked  = self.store.lock().unwrap();
        let conn    = locked.conn();
        let peer_filter = peer_addr
            .map(|p| format!("AND peer_addr = '{p}'"))
            .unwrap_or_default();
        let sql = format!(
            r#"SELECT event_id, started_at, eor_at, convergence_ms,
                      speaker_addr, peer_addr, trigger_type,
                      affected_prefixes, recovered_prefixes, unreachable_after
               FROM convergence_events
               WHERE started_at >= {since_expr}
               {peer_filter}
               ORDER BY started_at DESC
               LIMIT {limit}"#,
            since_expr = now_minus(&format!("{hours} hours"))
        );
        let rows = conn.prepare(&sql)?
            .query_map([], |row| {
                Ok(ConvergenceEventRow {
                    event_id:           row.get(0)?,
                    started_at:         row.get(1)?,
                    eor_at:             row.get(2)?,
                    convergence_ms:     row.get(3)?,
                    speaker_addr:       row.get(4)?,
                    peer_addr:          row.get(5)?,
                    trigger_type:       row.get(6)?,
                    affected_prefixes:  row.get(7)?,
                    recovered_prefixes: row.get(8)?,
                    unreachable_after:  row.get(9)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }
}

// ─── Max-prefix capacity analytics (RV7-B4) ──────────────────────────────────

/// Live prefix count + configured limit per (speaker, peer, afi_safi).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaxPrefixRow {
    pub speaker_addr: String,
    pub peer_addr:    String,
    pub peer_as:      u32,
    pub afi_safi:     String,
    pub live_count:   u64,
    pub max_prefix:   u32,
    pub used_pct:     f64,
    pub warning_pct:  u16,
    pub trend_per_day: Option<f64>,   // None when insufficient history
    pub eta_days:      Option<f64>,   // days until max_prefix exhausted at current trend
}

/// One stored policy config row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyConfigRow {
    pub fetched_at:   String,
    pub peer_addr:    String,
    pub speaker_addr: String,
    pub policy_name:  String,
    pub direction:    String,
    pub vendor:       String,
    pub clauses_json: String,
    pub source:       String,
    pub confidence:   f64,
}

impl QueryEngine {
    /// Max-prefix capacity dashboard data.
    ///
    /// Joins `peer_max_prefix` (configured limits) with the current live prefix
    /// count from `route_events` to produce fuel-gauge data for every peer.
    ///
    /// The 7-day trend (routes/day) and ETA to exhaustion are computed from the
    /// `stats_events` table counter `Prefixes/Prefixes Received` when available,
    /// falling back to 0.
    pub fn max_prefix_capacity(&self) -> Result<Vec<MaxPrefixRow>> {
        let locked = self.store.lock().unwrap();
        let conn   = locked.conn();

        let sql = r#"
            WITH live AS (
                SELECT speaker_addr, peer_addr, peer_as,
                       CASE
                           WHEN afi IN ('1', 'ipv4') THEN 'ipv4-unicast'
                           WHEN afi IN ('2', 'ipv6') THEN 'ipv6-unicast'
                           ELSE afi
                       END AS afi_safi_norm,
                       COUNT(DISTINCT prefix) AS live_count
                FROM (
                    SELECT speaker_addr, peer_addr, peer_as, prefix, afi, action,
                           ROW_NUMBER() OVER (PARTITION BY speaker_addr, peer_addr, prefix ORDER BY occurred_at DESC) AS rn
                    FROM route_events
                ) t
                WHERE rn = 1 AND action = 'announce'
                GROUP BY speaker_addr, peer_addr, peer_as, afi_safi_norm
            ),
            trend AS (
                SELECT speaker_addr, peer_addr,
                       REGR_SLOPE(counter_value, epoch(occurred_at)) * 86400 AS slope_per_day
                FROM stats_events
                WHERE counter_name = 'Prefixes'
                  AND occurred_at >= CAST(NOW() AS TIMESTAMP) - INTERVAL '7 days'
                GROUP BY speaker_addr, peer_addr
            )
            SELECT
                m.speaker_addr, m.peer_addr, m.peer_as,
                m.afi_safi, l.live_count,
                m.max_prefix, m.warning_pct,
                COALESCE(t.slope_per_day, 0.0) AS trend_per_day
            FROM peer_max_prefix m
            JOIN live l
              ON m.speaker_addr = l.speaker_addr
             AND m.peer_addr    = l.peer_addr
             AND m.afi_safi     = l.afi_safi_norm
            LEFT JOIN trend t
              ON m.speaker_addr = t.speaker_addr
             AND m.peer_addr    = t.peer_addr
            ORDER BY (l.live_count::DOUBLE / m.max_prefix) DESC
        "#;

        let rows: Vec<MaxPrefixRow> = conn.prepare(sql)?
            .query_map([], |row| {
                let live_count: i64 = row.get(4)?;
                let max_prefix: u32 = row.get(5)?;
                let warning_pct: u16 = row.get(6)?;
                let trend: f64 = row.get(7)?;
                let live  = live_count as u64;
                let used_pct = if max_prefix > 0 {
                    (live as f64 / max_prefix as f64) * 100.0
                } else { 0.0 };
                let eta_days = if trend > 0.01 && max_prefix > live as u32 {
                    Some((max_prefix as f64 - live as f64) / trend)
                } else { None };
                Ok(MaxPrefixRow {
                    speaker_addr: row.get(0)?,
                    peer_addr:    row.get(1)?,
                    peer_as:      row.get(2)?,
                    afi_safi:     row.get(3)?,
                    live_count:   live,
                    max_prefix,
                    used_pct,
                    warning_pct,
                    trend_per_day: if trend.abs() < 1e-6 { None } else { Some(trend) },
                    eta_days,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// List policy configs, optionally filtered by peer.
    pub fn policy_configs(&self, peer_addr: Option<&str>) -> Result<Vec<PolicyConfigRow>> {
        let locked = self.store.lock().unwrap();
        let conn   = locked.conn();
        let filter = peer_addr
            .map(|p| format!("WHERE peer_addr = '{p}'"))
            .unwrap_or_default();
        let sql = format!(
            r#"SELECT fetched_at, peer_addr, speaker_addr, policy_name, direction,
                      vendor, clauses_json, source, confidence
               FROM policy_configs
               {filter}
               ORDER BY fetched_at DESC
               LIMIT 500"#
        );
        let rows = conn.prepare(&sql)?
            .query_map([], |row| {
                Ok(PolicyConfigRow {
                    fetched_at:   row.get(0)?,
                    peer_addr:    row.get(1)?,
                    speaker_addr: row.get(2)?,
                    policy_name:  row.get(3)?,
                    direction:    row.get(4)?,
                    vendor:       row.get(5)?,
                    clauses_json: row.get(6)?,
                    source:       row.get(7)?,
                    confidence:   row.get(8)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Upsert a max-prefix limit for a peer/afi-safi.
    pub fn upsert_max_prefix(
        &self,
        speaker_addr: &str,
        peer_addr:    &str,
        peer_as:      u32,
        afi_safi:     &str,
        max_prefix:   u32,
        warning_pct:  u16,
    ) -> Result<()> {
        let locked = self.store.lock().unwrap();
        let conn   = locked.conn();
        conn.execute(
            r#"INSERT OR REPLACE INTO peer_max_prefix
               VALUES (NOW(), ?, ?, ?, ?, ?, ?)"#,
            duckdb::params![speaker_addr, peer_addr, peer_as, afi_safi, max_prefix, warning_pct],
        )?;
        Ok(())
    }
}

// ─── Path Status TLV query types (RV7-P3) ────────────────────────────────────

/// One row from the path_markings table — latest status per prefix+peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathMarkingRow {
    pub occurred_at:  String,
    pub speaker_addr: String,
    pub peer_addr:    String,
    pub peer_as:      u32,
    pub prefix:       String,
    pub afi:          String,
    pub path_status:  u32,
    pub path_reason:  u16,
    pub status_label: String,
    pub reason_label: String,
}

impl QueryEngine {
    /// Redundancy matrix: latest path status per (prefix, peer) for the given AFI.
    /// Returns at most `limit` prefix rows, ordered by prefix.
    ///
    /// When `min_active_paths` is Some(n), only returns prefixes with fewer
    /// than n active (best or primary) paths — the "show me where I'm unprotected" view.
    pub fn path_status_matrix(
        &self,
        afi:               Option<&str>,
        min_active_paths:  Option<u32>,
        limit:             usize,
    ) -> Result<Vec<PathMarkingRow>> {
        let locked = self.store.lock().unwrap();
        let conn   = locked.conn();

        let afi_filter = afi
            .map(|a| format!("AND afi = '{a}'"))
            .unwrap_or_default();

        let having_clause = min_active_paths
            .map(|n| format!(
                "HAVING SUM(CASE WHEN (path_status & 2 != 0) OR (path_status & 8 != 0) THEN 1 ELSE 0 END) < {n}"
            ))
            .unwrap_or_default();

        let sql = format!(
            r#"WITH latest AS (
                SELECT occurred_at, speaker_addr, peer_addr, peer_as, prefix, afi,
                       path_status, path_reason, status_label, reason_label,
                       ROW_NUMBER() OVER (PARTITION BY prefix, peer_addr ORDER BY occurred_at DESC) AS rn
                FROM path_markings
                WHERE occurred_at >= {since_5m}
                  {afi_filter}
            ),
            filtered_prefixes AS (
                SELECT prefix
                FROM latest
                WHERE rn = 1
                GROUP BY prefix
                {having_clause}
            )
            SELECT l.occurred_at, l.speaker_addr, l.peer_addr, l.peer_as,
                   l.prefix, l.afi, l.path_status, l.path_reason,
                   l.status_label, l.reason_label
            FROM latest l
            JOIN filtered_prefixes fp ON l.prefix = fp.prefix
            WHERE l.rn = 1
            ORDER BY l.prefix, l.peer_addr
            LIMIT {limit}"#,
            since_5m = now_minus("5 minutes")
        );

        let rows = conn.prepare(&sql)?.query_map([], |row| {
            Ok(PathMarkingRow {
                occurred_at:  row.get(0)?,
                speaker_addr: row.get(1)?,
                peer_addr:    row.get(2)?,
                peer_as:      row.get(3)?,
                prefix:       row.get(4)?,
                afi:          row.get(5)?,
                path_status:  row.get(6)?,
                path_reason:  row.get(7)?,
                status_label: row.get(8)?,
                reason_label: row.get(9)?,
            })
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// Timeline of path status events for a specific prefix+peer (last N hours).
    pub fn path_status_history(
        &self,
        prefix:    &str,
        peer_addr: &str,
        hours:     u32,
        limit:     usize,
    ) -> Result<Vec<PathMarkingRow>> {
        let locked = self.store.lock().unwrap();
        let conn   = locked.conn();
        let sql = format!(
            r#"SELECT occurred_at, speaker_addr, peer_addr, peer_as, prefix, afi,
                      path_status, path_reason, status_label, reason_label
               FROM path_markings
               WHERE prefix = '{prefix}'
                 AND peer_addr = '{peer_addr}'
                 AND occurred_at >= {since_expr}
               ORDER BY occurred_at DESC
               LIMIT {limit}"#,
            since_expr = now_minus(&format!("{hours} hours"))
        );
        let rows = conn.prepare(&sql)?.query_map([], |row| {
            Ok(PathMarkingRow {
                occurred_at:  row.get(0)?,
                speaker_addr: row.get(1)?,
                peer_addr:    row.get(2)?,
                peer_as:      row.get(3)?,
                prefix:       row.get(4)?,
                afi:          row.get(5)?,
                path_status:  row.get(6)?,
                path_reason:  row.get(7)?,
                status_label: row.get(8)?,
                reason_label: row.get(9)?,
            })
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }
}

// ─── RV8-MC1: Additional query methods for MCP tools ─────────────────────────

impl QueryEngine {
    /// Peers that have had a peer_down event in the last `hours` hours (flap detection).
    pub fn peer_flap_events(&self, hours: u32, limit: usize) -> Result<Vec<serde_json::Value>> {
        let locked = self.store.lock().unwrap();
        let conn   = locked.conn();
        let sql = format!(
            r#"SELECT peer_addr, speaker_addr, COUNT(*) AS down_count,
                      MAX(occurred_at) AS last_down
               FROM peer_events
               WHERE event_type = 'peer_down'
                 AND occurred_at >= {since_expr}
               GROUP BY peer_addr, speaker_addr
               ORDER BY down_count DESC
               LIMIT {limit}"#,
            since_expr = now_minus(&format!("{hours} hours"))
        );
        let rows = conn.prepare(&sql)?.query_map([], |row| {
            Ok(serde_json::json!({
                "peer_addr":    row.get::<_, String>(0)?,
                "speaker_addr": row.get::<_, String>(1)?,
                "down_count":   row.get::<_, u64>(2)?,
                "last_down":    row.get::<_, String>(3)?,
            }))
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// Routes with RPKI Invalid status, optionally filtered by speaker.
    pub fn rpki_invalids(&self, speaker: Option<&str>, limit: usize) -> Result<Vec<serde_json::Value>> {
        let locked = self.store.lock().unwrap();
        let conn   = locked.conn();
        let has_rpki_validity: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM information_schema.columns WHERE table_name = 'route_events' AND column_name = 'rpki_validity'",
            [], |row| row.get(0),
        ).unwrap_or(false);
        if !has_rpki_validity {
            return Ok(Vec::new());
        }
        let speaker_filter = speaker
            .map(|s| format!("AND speaker_addr = '{s}'"))
            .unwrap_or_default();
        let sql = format!(
            r#"SELECT prefix, peer_addr, speaker_addr, as_path, occurred_at
               FROM route_events
               WHERE action = 'announce'
                 AND rpki_validity = 'invalid'
                 {speaker_filter}
               ORDER BY occurred_at DESC
               LIMIT {limit}"#
        );
        let rows = conn.prepare(&sql)?.query_map([], |row| {
            Ok(serde_json::json!({
                "prefix":       row.get::<_, String>(0)?,
                "peer_addr":    row.get::<_, String>(1)?,
                "speaker_addr": row.get::<_, String>(2)?,
                "as_path":      row.get::<_, Option<String>>(3)?,
                "occurred_at":  row.get::<_, String>(4)?,
            }))
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// Top BGP communities by route count.
    pub fn community_summary(&self, limit: usize) -> Result<Vec<serde_json::Value>> {
        let locked = self.store.lock().unwrap();
        let conn   = locked.conn();
        let sql = format!(
            r#"SELECT communities, COUNT(*) AS route_count
               FROM route_events
               WHERE communities IS NOT NULL AND communities <> ''
               GROUP BY communities
               ORDER BY route_count DESC
               LIMIT {limit}"#
        );
        let rows = conn.prepare(&sql)?.query_map([], |row| {
            Ok(serde_json::json!({
                "communities": row.get::<_, String>(0)?,
                "route_count": row.get::<_, u64>(1)?,
            }))
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// Execute an arbitrary read-only SQL query and return rows as JSON.
    /// Used by the NL→SQL MCP tool. Only SELECT statements are accepted.
    pub fn raw_query(&self, sql: &str) -> Result<serde_json::Value> {
        let trimmed = sql.trim_start().to_uppercase();
        if !trimmed.starts_with("SELECT") {
            return Err(anyhow::anyhow!("Only SELECT statements are allowed in raw_query"));
        }
        let locked = self.store.lock().unwrap();
        let conn   = locked.conn();
        let mut stmt = conn.prepare(sql)?;
        let col_names: Vec<String> = stmt.column_names();
        let rows: Vec<serde_json::Value> = stmt.query_map([], |row| {
            let mut obj = serde_json::Map::new();
            for (i, name) in col_names.iter().enumerate() {
                let val: Option<String> = row.get(i).ok();
                obj.insert(name.clone(), val.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null));
            }
            Ok(serde_json::Value::Object(obj))
        })?.filter_map(|r| r.ok()).collect();
        Ok(serde_json::Value::Array(rows))
    }
}

fn map_route_row(row: &Row) -> duckdb::Result<RouteRow> {
    Ok(RouteRow {
        occurred_at:  row.get(0)?,
        speaker_addr: row.get(1)?,
        peer_addr:    row.get(2)?,
        peer_as:      row.get(3)?,
        rib_type:     row.get(4)?,
        action:       row.get(5)?,
        prefix:       row.get(6)?,
        afi:          row.get(7)?,
        origin:       row.get(8)?,
        as_path:      row.get(9)?,
        as_path_len:  row.get(10)?,
        next_hop:     row.get(11)?,
        local_pref:   row.get(12)?,
        med:          row.get(13)?,
        communities:  row.get(14)?,
    })
}

#[cfg(test)]
mod tests {
    use super::QueryEngine;
    use crate::duck::RouteStore;
    use std::sync::{Arc, Mutex};

    #[test]
    fn bmp_stats_history_returns_rows_with_afi_safi() {
        let store = Arc::new(Mutex::new(RouteStore::in_memory().unwrap()));
        {
            let locked = store.lock().unwrap();
            locked.conn().execute(
                "INSERT INTO stats_events (
                    id, occurred_at, speaker_addr, peer_addr, counter_name,
                    counter_value, stat_type, afi, safi
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    "00000000-0000-0000-0000-000000000001",
                    "2026-06-23T15:03:07Z",
                    "172.20.21.2",
                    "10.30.1.1",
                    "per-afi-safi-pre-route-limit-adj-rib-in",
                    42u64,
                    30u16,
                    1u16,
                    1u8,
                ],
            ).unwrap();
        }

        let rows = QueryEngine::new(store)
            .bmp_stats_history(None, 10)
            .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["stat_type"], 30);
        assert_eq!(rows[0]["afi"], 1);
        assert_eq!(rows[0]["safi"], 1);
        assert_eq!(rows[0]["counter_value"], 42);
    }
}
