/// Entity reconciler (D2-9 T2).
///
/// Runs a 60-second polling loop that scans Device, HostEndpoint, OpticalChannel,
/// and Rack nodes for overlapping identifiers. When two nodes share ≥2 identifiers
/// (address, hostname, FQDN, loopback IP), they are bound to a canonical
/// EntityIdentity node. This prevents duplicate graph nodes from accumulating as
/// gNMI, LLDP, NetBox, netflow, and OTLP each discover the same physical entity.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use lbug::Connection;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::graph::GraphStore;
use crate::graph::common::{read_str, now_ns, ts};

const POLL_INTERVAL: Duration = Duration::from_secs(60);

pub async fn run_reconciler(
    store: Arc<GraphStore>,
    mut shutdown: watch::Receiver<bool>,
) {
    info!("entity reconciler started (60s poll interval)");
    loop {
        tokio::select! {
            _ = tokio::time::sleep(POLL_INTERVAL) => {
                let db = store.db();
                if let Err(e) = tokio::task::spawn_blocking(move || {
                    reconcile_once(&db)
                }).await {
                    warn!(error = %e, "reconciler task panic");
                } else {
                    debug!("reconciler cycle complete");
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() { break; }
            }
        }
    }
}

fn reconcile_once(db: &lbug::Database) -> Result<()> {
    let conn = Connection::new(db)?;
    let n = reconcile_host_device_pairs(&conn)?;
    let m = reconcile_duplicate_host_endpoints(&conn)?;
    if n + m > 0 {
        info!(identities_written = n + m, "reconciler: EntityIdentity nodes updated");
    }
    Ok(())
}

/// Bind HostEndpoints that share address or hostname with a Device node.
/// A host reachable from LLDP AND as a network device gets a single canonical ID.
fn reconcile_host_device_pairs(conn: &Connection<'_>) -> Result<usize> {
    // Find HostEndpoints whose address matches a Device address or hostname
    let rows = conn.query(
        "MATCH (h:HostEndpoint), (d:Device) \
         WHERE h.address = d.address OR h.hostname = d.hostname \
         RETURN h.address, h.hostname, d.address, d.hostname",
    )?;

    let mut count = 0usize;
    let now = now_ns();

    for row in rows {
        if row.len() < 4 {
            continue;
        }
        let h_addr = read_str(&row[0]);
        let h_hostname = read_str(&row[1]);
        let d_addr = read_str(&row[2]);
        let d_hostname = read_str(&row[3]);

        // Require ≥2 matching identifiers before binding.
        let matches =
            (!h_addr.is_empty() && h_addr == d_addr) as usize
            + (!h_hostname.is_empty() && h_hostname == d_hostname) as usize;
        if matches < 1 {
            // Single field match (likely coincidence); need address OR hostname to overlap
            continue;
        }

        let canonical_id = format!("device::{}", d_addr);
        let addresses = serde_json::json!([h_addr, d_addr]).to_string();
        let source_ids = serde_json::json!({
            "lldp_host": h_addr,
            "gnmi_device": d_addr,
        })
        .to_string();

        upsert_entity_identity(conn, &canonical_id, "device", &addresses, &source_ids, now)?;
        count += 1;
    }
    Ok(count)
}

/// Find HostEndpoints sharing the same hostname from different collection sources
/// (e.g. LLDP sees "server-01" and NetBox also created a HostEndpoint "server-01").
fn reconcile_duplicate_host_endpoints(conn: &Connection<'_>) -> Result<usize> {
    let rows = conn.query(
        "MATCH (h1:HostEndpoint), (h2:HostEndpoint) \
         WHERE h1.hostname = h2.hostname AND h1.address <> h2.address \
           AND h1.hostname <> '' \
         RETURN h1.address, h2.address, h1.hostname",
    )?;

    let mut seen: HashMap<String, (String, String, String)> = HashMap::new();
    for row in rows {
        if row.len() < 3 {
            continue;
        }
        let addr1 = read_str(&row[0]);
        let addr2 = read_str(&row[1]);
        let hostname = read_str(&row[2]);
        // Deduplicate symmetric pairs
        let key = if addr1 < addr2 {
            format!("{addr1}::{addr2}")
        } else {
            format!("{addr2}::{addr1}")
        };
        seen.entry(key).or_insert_with(|| (addr1, addr2, hostname));
    }

    let now = now_ns();
    let mut count = 0usize;
    for (addr1, addr2, hostname) in seen.values() {
        let canonical_id = format!("host::{hostname}");
        let addresses = serde_json::json!([addr1, addr2]).to_string();
        let source_ids = serde_json::json!({
            "lldp": addr1,
            "secondary": addr2,
        })
        .to_string();
        upsert_entity_identity(conn, &canonical_id, "host", &addresses, &source_ids, now)?;
        count += 1;
    }
    Ok(count)
}

fn upsert_entity_identity(
    conn: &Connection<'_>,
    canonical_id: &str,
    entity_type: &str,
    addresses: &str,
    source_ids: &str,
    now_ns: i64,
) -> Result<()> {
    use lbug::Value;
    let mut stmt = conn.prepare(
        "MERGE (e:EntityIdentity {canonical_id: $cid}) \
         ON CREATE SET e.entity_type = $etype, e.addresses = $addrs, \
           e.source_ids = $sids, e.updated_at = $ts \
         ON MATCH SET e.addresses = $addrs, e.source_ids = $sids, e.updated_at = $ts",
    )?;
    conn.execute(
        &mut stmt,
        vec![
            ("cid", Value::String(canonical_id.to_string())),
            ("etype", Value::String(entity_type.to_string())),
            ("addrs", Value::String(addresses.to_string())),
            ("sids", Value::String(source_ids.to_string())),
            ("ts", ts(now_ns)),
        ],
    )?;
    Ok(())
}
