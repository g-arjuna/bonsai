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
        let mut count_stmt = conn
            .prepare("MATCH (e:StateChangeEvent) WHERE e.occurred_at < $cutoff RETURN count(e)")?;
        let cutoff_val = lbug::Value::TimestampNs(cutoff);
        let mut rows = conn.execute(&mut count_stmt, vec![("cutoff", cutoff_val.clone())])?;
        let n: u64 = rows
            .next()
            .and_then(|r| {
                if let lbug::Value::Int64(n) = r[0] {
                    Some(n as u64)
                } else {
                    None
                }
            })
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
    Ok(PruneStats {
        events_deleted: deleted,
    })
}

/// Delete the oldest StateChangeEvents if the total count exceeds `max_count`.
///
/// Keeps at most `max_count` most-recent events. No-op when `max_count` is 0.
pub async fn prune_events_by_count(store: Arc<GraphStore>, max_count: u64) -> Result<PruneStats> {
    if max_count == 0 {
        return Ok(PruneStats { events_deleted: 0 });
    }

    let db = store.db();
    let deleted = tokio::task::spawn_blocking(move || -> Result<u64> {
        let conn = lbug::Connection::new(&db)?;

        let mut count_stmt = conn.prepare("MATCH (e:StateChangeEvent) RETURN count(e)")?;
        let mut rows = conn.execute(&mut count_stmt, vec![])?;
        let total: u64 = rows
            .next()
            .and_then(|r| {
                if let lbug::Value::Int64(n) = r[0] {
                    Some(n as u64)
                } else {
                    None
                }
            })
            .unwrap_or(0);

        let excess = total.saturating_sub(max_count);
        if excess == 0 {
            return Ok(0);
        }

        // Delete the exact oldest IDs so timestamp ties do not over-delete.
        let mut ids_stmt = conn.prepare(
            "MATCH (e:StateChangeEvent) RETURN e.id ORDER BY e.occurred_at ASC LIMIT $n",
        )?;
        let limit_val = lbug::Value::Int64(excess as i64);
        let id_rows = conn.execute(&mut ids_stmt, vec![("n", limit_val)])?;
        let ids_to_delete: Vec<String> = id_rows
            .filter_map(|row| match &row[0] {
                lbug::Value::String(id) => Some(id.clone()),
                _ => None,
            })
            .collect();

        if ids_to_delete.is_empty() {
            return Ok(0);
        }

        let mut del_stmt = conn.prepare("MATCH (e:StateChangeEvent {id: $id}) DETACH DELETE e")?;
        for id in &ids_to_delete {
            conn.execute(&mut del_stmt, vec![("id", lbug::Value::String(id.clone()))])?;
        }
        Ok(ids_to_delete.len() as u64)
    })
    .await??;

    if deleted > 0 {
        info!(
            deleted,
            max_count, "retention: pruned StateChangeEvents by count"
        );
    }
    Ok(PruneStats {
        events_deleted: deleted,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use lbug::{Connection, Value};
    use uuid::Uuid;

    use super::*;
    use crate::graph::GraphStore;

    #[tokio::test]
    async fn prune_events_by_count_deletes_exact_number_when_timestamps_tie() -> Result<()> {
        let db_path = std::env::current_dir()?
            .join("target")
            .join(format!("retention-tie-test-{}.db", Uuid::new_v4()));
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let store = Arc::new(GraphStore::open(db_path.to_str().expect("valid db path"), 256 * 1024 * 1024)?);
        seed_state_change_events(Arc::clone(&store), 10)?;

        let stats = prune_events_by_count(Arc::clone(&store), 5).await?;
        assert_eq!(stats.events_deleted, 5);
        assert_eq!(count_state_change_events(Arc::clone(&store))?, 5);

        Ok(())
    }

    fn seed_state_change_events(store: Arc<GraphStore>, total: usize) -> Result<()> {
        let db = store.db();
        let conn = Connection::new(&db)?;
        let timestamp = OffsetDateTime::from_unix_timestamp_nanos(1_000_000_000)?;
        let mut insert = conn.prepare(
            "CREATE (e:StateChangeEvent {\
                id: $id, \
                device_address: $addr, \
                event_type: $etype, \
                detail: $detail, \
                occurred_at: $ts})",
        )?;

        for idx in 0..total {
            conn.execute(
                &mut insert,
                vec![
                    ("id", Value::String(format!("event-{idx}"))),
                    ("addr", Value::String("10.0.0.1".to_string())),
                    ("etype", Value::String("bgp_session_change".to_string())),
                    ("detail", Value::String("{}".to_string())),
                    ("ts", Value::TimestampNs(timestamp)),
                ],
            )?;
        }

        Ok(())
    }

    fn count_state_change_events(store: Arc<GraphStore>) -> Result<u64> {
        let db = store.db();
        let conn = Connection::new(&db)?;
        let mut stmt = conn.prepare("MATCH (e:StateChangeEvent) RETURN count(e)")?;
        let mut rows = conn.execute(&mut stmt, vec![])?;
        Ok(rows
            .next()
            .and_then(|row| match row[0] {
                Value::Int64(count) => Some(count as u64),
                _ => None,
            })
            .unwrap_or(0))
    }
}
