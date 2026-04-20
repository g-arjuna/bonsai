use std::sync::Arc;

use anyhow::Result;
use time::OffsetDateTime;
use tracing::info;

use crate::graph::GraphStore;

pub struct PruneStats {
    pub events_deleted: u64,
}

/// Delete StateChangeEvents older than `cutoff` from the graph.
pub async fn prune_events(store: Arc<GraphStore>, cutoff: OffsetDateTime) -> Result<PruneStats> {
    let db = store.db();
    let deleted = tokio::task::spawn_blocking(move || -> Result<u64> {
        let conn = lbug::Connection::new(&db)?;
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

    info!(deleted, "retention: pruned StateChangeEvents by age");
    Ok(PruneStats { events_deleted: deleted })
}

/// Delete the oldest StateChangeEvents if the total count exceeds `max_count`.
///
/// Keeps at most `max_count` most-recent events. No-op when `max_count` is 0.
pub async fn prune_events_by_count(
    store: Arc<GraphStore>,
    max_count: u64,
) -> Result<PruneStats> {
    if max_count == 0 {
        return Ok(PruneStats { events_deleted: 0 });
    }

    let db = store.db();
    let deleted = tokio::task::spawn_blocking(move || -> Result<u64> {
        let conn = lbug::Connection::new(&db)?;

        let mut count_stmt = conn.prepare("MATCH (e:StateChangeEvent) RETURN count(e)")?;
        let mut rows = conn.execute(&mut count_stmt, vec![])?;
        let total: u64 = rows.next()
            .and_then(|r| if let lbug::Value::Int64(n) = r[0] { Some(n as u64) } else { None })
            .unwrap_or(0);

        let excess = total.saturating_sub(max_count);
        if excess == 0 {
            return Ok(0);
        }

        // Identify the occurred_at of the excess-th oldest event, then delete
        // everything at or before that timestamp.
        let mut cutoff_stmt = conn.prepare(
            "MATCH (e:StateChangeEvent) RETURN e.occurred_at ORDER BY e.occurred_at ASC LIMIT $n",
        )?;
        let limit_val = lbug::Value::Int64(excess as i64);
        let mut cutoff_rows = conn.execute(&mut cutoff_stmt, vec![("n", limit_val)])?;
        let mut last_cutoff: Option<lbug::Value> = None;
        for row in cutoff_rows.by_ref() {
            last_cutoff = Some(row[0].clone());
        }

        if let Some(cutoff) = last_cutoff {
            let mut del_stmt = conn.prepare(
                "MATCH (e:StateChangeEvent) WHERE e.occurred_at <= $cutoff DETACH DELETE e",
            )?;
            conn.execute(&mut del_stmt, vec![("cutoff", cutoff)])?;
        }
        Ok(excess)
    })
    .await??;

    if deleted > 0 {
        info!(deleted, max_count, "retention: pruned StateChangeEvents by count");
    }
    Ok(PruneStats { events_deleted: deleted })
}
