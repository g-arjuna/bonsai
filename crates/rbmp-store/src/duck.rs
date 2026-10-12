use std::path::Path;
use duckdb::Connection;
use anyhow::Result;
use tracing::info;
use crate::schema::CREATE_TABLES;

/// Persistent DuckDB-backed route store
pub struct RouteStore {
    conn: Connection,
}

impl RouteStore {
    /// Open (or create) the DuckDB database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path.as_ref())?;
        info!(path = %path.as_ref().display(), "Opened DuckDB route store");
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    /// Open an ephemeral in-memory database (for testing / --no-persist mode)
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(CREATE_TABLES)?;
        info!("DuckDB schema initialized");
        Ok(())
    }

    pub fn conn(&self) -> &Connection { &self.conn }

    /// Flush DuckDB WAL to disk
    pub fn checkpoint(&self) -> Result<()> {
        self.conn.execute_batch("CHECKPOINT;")?;
        Ok(())
    }

    /// Return (table_name, estimated_row_count) for each event table.
    /// Used for Prometheus gauge metrics (RV4-2 T3).
    pub fn table_row_counts(&self) -> Vec<(String, i64)> {
        const TABLES: &[&str] = &[
            "route_events", "peer_events", "speaker_events",
            "stats_events",  "evpn_events",
        ];
        TABLES.iter().filter_map(|t| {
            let sql = format!("SELECT COUNT(*) FROM {t}");
            self.conn.query_row(&sql, [], |r| r.get::<_, i64>(0))
                .ok()
                .map(|n| (t.to_string(), n))
        }).collect()
    }
}
