use std::sync::Arc;

use anyhow::{Context, Result};
use lbug::{Connection, Database, SystemConfig, Value};
use time::OffsetDateTime;
use tracing::{debug, info, warn};

use crate::telemetry::{json_i64, json_str, TelemetryEvent, TelemetryUpdate};

pub struct GraphStore {
    db: Arc<Database>,
}

impl GraphStore {
    pub fn open(path: &str) -> Result<Self> {
        let db = Database::new(path, SystemConfig::default())
            .context("failed to open LadybugDB")?;
        let store = GraphStore { db: Arc::new(db) };
        store.init_schema()?;
        info!(path, "graph store opened");
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = Connection::new(&self.db).context("schema connection")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS Device(\
                address    STRING,\
                vendor     STRING,\
                updated_at TIMESTAMP_NS,\
                PRIMARY KEY (address))",
        )
        .context("create Device table")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS Interface(\
                id                  STRING,\
                device_address      STRING,\
                name                STRING,\
                in_pkts             INT64,\
                out_pkts            INT64,\
                in_octets           INT64,\
                out_octets          INT64,\
                in_errors           INT64,\
                out_errors          INT64,\
                carrier_transitions INT64,\
                updated_at          TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create Interface table")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS BgpNeighbor(\
                id                      STRING,\
                device_address          STRING,\
                peer_address            STRING,\
                peer_as                 INT64,\
                session_state           STRING,\
                established_transitions INT64,\
                updated_at              TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create BgpNeighbor table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_INTERFACE(FROM Device TO Interface)",
        )
        .context("create HAS_INTERFACE rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS PEERS_WITH(FROM Device TO BgpNeighbor)",
        )
        .context("create PEERS_WITH rel")?;

        info!("graph schema initialised");
        Ok(())
    }

    pub fn db(&self) -> Arc<Database> {
        Arc::clone(&self.db)
    }

    /// Write a single telemetry update to the graph.
    /// Dispatches to a blocking thread so the caller's async task is not blocked.
    pub async fn write(&self, update: TelemetryUpdate) -> Result<()> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || write_blocking(&db, &update))
            .await
            .context("spawn_blocking panicked")?
    }
}

// ── blocking write helpers ────────────────────────────────────────────────────

fn write_blocking(db: &Database, update: &TelemetryUpdate) -> Result<()> {
    let conn = Connection::new(db).context("graph write connection")?;
    match update.classify() {
        TelemetryEvent::InterfaceStats { if_name } => {
            // Skip interfaces with no data (SR Linux sends empty {} for unconfigured ports)
            if update.value.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                return Ok(());
            }
            write_interface(&conn, update, &if_name)
        }
        TelemetryEvent::BgpNeighborState { peer_address } => {
            write_bgp_neighbor(&conn, update, &peer_address)
        }
        TelemetryEvent::Ignored => Ok(()),
    }
}

fn ts(ns: i64) -> Value {
    let dt = OffsetDateTime::UNIX_EPOCH
        + time::Duration::nanoseconds(ns);
    Value::TimestampNs(dt)
}

fn write_interface(conn: &Connection<'_>, u: &TelemetryUpdate, if_name: &str) -> Result<()> {
    let id = format!("{}:{}", u.target, if_name);
    let now = ts(u.timestamp_ns);

    upsert_device(conn, &u.target, now.clone())?;

    let mut stmt = conn.prepare(
        "MERGE (i:Interface {id: $id}) \
         ON CREATE SET \
           i.device_address = $addr, i.name = $name, \
           i.in_pkts = $in_pkts, i.out_pkts = $out_pkts, \
           i.in_octets = $in_octets, i.out_octets = $out_octets, \
           i.in_errors = $in_errors, i.out_errors = $out_errors, \
           i.carrier_transitions = $carrier, i.updated_at = $ts \
         ON MATCH SET \
           i.in_pkts = $in_pkts, i.out_pkts = $out_pkts, \
           i.in_octets = $in_octets, i.out_octets = $out_octets, \
           i.in_errors = $in_errors, i.out_errors = $out_errors, \
           i.carrier_transitions = $carrier, i.updated_at = $ts",
    )
    .context("prepare interface upsert")?;

    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("addr", Value::String(u.target.clone())),
            ("name", Value::String(if_name.to_string())),
            ("in_pkts", Value::Int64(json_i64(&u.value, "in-packets"))),
            ("out_pkts", Value::Int64(json_i64(&u.value, "out-packets"))),
            ("in_octets", Value::Int64(json_i64(&u.value, "in-octets"))),
            ("out_octets", Value::Int64(json_i64(&u.value, "out-octets"))),
            ("in_errors", Value::Int64(json_i64(&u.value, "in-error-packets"))),
            ("out_errors", Value::Int64(json_i64(&u.value, "out-error-packets"))),
            ("carrier", Value::Int64(json_i64(&u.value, "carrier-transitions"))),
            ("ts", now.clone()),
        ],
    )
    .context("execute interface upsert")?;

    // Ensure the Device→Interface edge exists
    let mut edge_stmt = conn.prepare(
        "MATCH (d:Device {address: $addr}), (i:Interface {id: $id}) \
         MERGE (d)-[:HAS_INTERFACE]->(i)",
    )
    .context("prepare HAS_INTERFACE merge")?;

    conn.execute(
        &mut edge_stmt,
        vec![
            ("addr", Value::String(u.target.clone())),
            ("id", Value::String(id)),
        ],
    )
    .context("execute HAS_INTERFACE merge")?;

    debug!(target = %u.target, interface = %if_name, "interface written");
    Ok(())
}

fn write_bgp_neighbor(conn: &Connection<'_>, u: &TelemetryUpdate, peer_addr: &str) -> Result<()> {
    let id = format!("{}:{}", u.target, peer_addr);
    let now = ts(u.timestamp_ns);

    upsert_device(conn, &u.target, now.clone())?;

    let mut stmt = conn.prepare(
        "MERGE (n:BgpNeighbor {id: $id}) \
         ON CREATE SET \
           n.device_address = $addr, n.peer_address = $peer, \
           n.peer_as = $peer_as, n.session_state = $state, \
           n.established_transitions = $estab, n.updated_at = $ts \
         ON MATCH SET \
           n.peer_as = $peer_as, n.session_state = $state, \
           n.established_transitions = $estab, n.updated_at = $ts",
    )
    .context("prepare BgpNeighbor upsert")?;

    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("addr", Value::String(u.target.clone())),
            ("peer", Value::String(peer_addr.to_string())),
            ("peer_as", Value::Int64(json_i64(&u.value, "peer-as"))),
            ("state", Value::String(json_str(&u.value, "session-state").to_string())),
            ("estab", Value::Int64(json_i64(&u.value, "established-transitions"))),
            ("ts", now.clone()),
        ],
    )
    .context("execute BgpNeighbor upsert")?;

    // Ensure the Device→BgpNeighbor edge exists
    let mut edge_stmt = conn.prepare(
        "MATCH (d:Device {address: $addr}), (n:BgpNeighbor {id: $id}) \
         MERGE (d)-[:PEERS_WITH]->(n)",
    )
    .context("prepare PEERS_WITH merge")?;

    conn.execute(
        &mut edge_stmt,
        vec![
            ("addr", Value::String(u.target.clone())),
            ("id", Value::String(id)),
        ],
    )
    .context("execute PEERS_WITH merge")?;

    info!(
        target = %u.target,
        peer = %peer_addr,
        state = %json_str(&u.value, "session-state"),
        "BGP neighbor written"
    );
    Ok(())
}

fn upsert_device(conn: &Connection<'_>, address: &str, now: Value) -> Result<()> {
    let mut stmt = conn.prepare(
        "MERGE (d:Device {address: $addr}) \
         ON CREATE SET d.vendor = $vendor, d.updated_at = $ts \
         ON MATCH SET d.updated_at = $ts",
    )
    .context("prepare Device upsert")?;

    conn.execute(
        &mut stmt,
        vec![
            ("addr", Value::String(address.to_string())),
            ("vendor", Value::String("nokia_srl".to_string())),
            ("ts", now),
        ],
    )
    .context("execute Device upsert")?;

    Ok(())
}

// ── diagnostic query (callable from main after startup) ──────────────────────

pub fn log_graph_summary(db: &Database) {
    let Ok(conn) = Connection::new(db) else { return };
    for (label, q) in [
        ("devices", "MATCH (n:Device) RETURN count(n)"),
        ("interfaces", "MATCH (n:Interface) RETURN count(n)"),
        ("bgp-neighbors", "MATCH (n:BgpNeighbor) RETURN count(n)"),
    ] {
        match conn.query(q) {
            Ok(mut r) => {
                if let Some(row) = r.next() {
                    info!(label, count = ?row[0], "graph summary");
                }
            }
            Err(e) => warn!(label, error = %e, "summary query failed"),
        }
    }
}
