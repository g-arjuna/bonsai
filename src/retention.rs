use std::sync::Arc;

use anyhow::Result;
use time::OffsetDateTime;
use tracing::info;

use crate::graph::GraphStore;

pub struct PruneStats {
    pub events_deleted: u64,
}

/// Delete StateChangeEvents older than `cutoff` from the graph.
///
/// No-op when retention is disabled in config (`[retention] enabled = false`).
/// Phase 5.5 will implement: export to Parquet before delete, configurable
/// per-node-type retention, and a separate archive path.
pub async fn prune_events(store: Arc<GraphStore>, cutoff: OffsetDateTime) -> Result<PruneStats> {
    let db = store.db();
    let deleted = tokio::task::spawn_blocking(move || -> Result<u64> {
        let conn = lbug::Connection::new(&db)?;
        // Count first so we can log how many are pruned.
        let mut count_stmt = conn.prepare(
            "MATCH (e:StateChangeEvent) WHERE e.occurred_at < $cutoff RETURN count(e)",
        )?;
        let cutoff_val = lbug::Value::TimestampNs(cutoff);
        let mut rows = conn.execute(&mut count_stmt, vec![("cutoff", cutoff_val.clone())])?;
        let n: u64 = rows.next()
            .and_then(|r| if let lbug::Value::Int64(n) = r[0] { Some(n as u64) } else { None })
            .unwrap_or(0);
        if n > 0 {
            let mut del_stmt = conn.prepare(
                "MATCH (e:StateChangeEvent) WHERE e.occurred_at < $cutoff DETACH DELETE e",
            )?;
            conn.execute(&mut del_stmt, vec![("cutoff", cutoff_val)])?;
        }
        Ok(n)
    })
    .await??;

    info!(deleted, "retention: pruned StateChangeEvents");
    Ok(PruneStats { events_deleted: deleted })
}
