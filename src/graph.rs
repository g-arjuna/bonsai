use std::sync::Arc;

use anyhow::{Context, Result};
use lbug::{Connection, Database, SystemConfig, Value};
use time::OffsetDateTime;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::telemetry::{json_i64, json_i64_multi, json_str, TelemetryEvent, TelemetryUpdate};

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
                hostname   STRING,\
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

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS LldpNeighbor(\
                id             STRING,\
                device_address STRING,\
                local_if       STRING,\
                neighbor_id    STRING,\
                chassis_id     STRING,\
                system_name    STRING,\
                port_id        STRING,\
                updated_at     TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create LldpNeighbor table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_LLDP_NEIGHBOR(FROM Device TO LldpNeighbor)",
        )
        .context("create HAS_LLDP_NEIGHBOR rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS StateChangeEvent(\
                id             STRING,\
                device_address STRING,\
                event_type     STRING,\
                detail         STRING,\
                occurred_at    TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create StateChangeEvent table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS REPORTED_BY(FROM Device TO StateChangeEvent)",
        )
        .context("create REPORTED_BY rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS CONNECTED_TO(FROM Interface TO Interface)",
        )
        .context("create CONNECTED_TO rel")?;

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
        TelemetryEvent::BgpNeighborState { peer_address, state_value } => {
            write_bgp_neighbor(&conn, update, &peer_address, state_value.as_ref().unwrap_or(&update.value))
        }
        TelemetryEvent::LldpNeighbor { local_if, neighbor_id, state_value } => {
            write_lldp_neighbor(&conn, update, &local_if, &neighbor_id, state_value.as_ref().unwrap_or(&update.value))
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

    upsert_device(conn, &u.target, &u.vendor, &u.hostname, now.clone())?;

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
            // Field name priority: SRL native → XR native → Junos native → OC
            ("in_pkts",    Value::Int64(json_i64_multi(&u.value, &[
                "in-packets",        // SRL native
                "packets-received",  // XR native (generic-counters)
                "input-packets",     // Junos native
                "in-pkts",           // OC
            ]))),
            ("out_pkts",   Value::Int64(json_i64_multi(&u.value, &[
                "out-packets",       // SRL native
                "packets-sent",      // XR native
                "output-packets",    // Junos native
                "out-pkts",          // OC
            ]))),
            ("in_octets",  Value::Int64(json_i64_multi(&u.value, &[
                "in-octets",         // SRL native & OC
                "bytes-received",    // XR native
                "input-bytes",       // Junos native
            ]))),
            ("out_octets", Value::Int64(json_i64_multi(&u.value, &[
                "out-octets",        // SRL native & OC
                "bytes-sent",        // XR native
                "output-bytes",      // Junos native
            ]))),
            ("in_errors",  Value::Int64(json_i64_multi(&u.value, &[
                "in-error-packets",  // SRL native
                "input-total-errors",// XR native
                "input-errors",      // Junos native
                "in-errors",         // OC
            ]))),
            ("out_errors", Value::Int64(json_i64_multi(&u.value, &[
                "out-error-packets", // SRL native
                "output-total-errors",// XR native
                "output-errors",     // Junos native
                "out-errors",        // OC
            ]))),
            ("carrier",    Value::Int64(json_i64(&u.value, "carrier-transitions"))),
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

fn write_bgp_neighbor(conn: &Connection<'_>, u: &TelemetryUpdate, peer_addr: &str, val: &serde_json::Value) -> Result<()> {
    let id = format!("{}:{}", u.target, peer_addr);
    let now = ts(u.timestamp_ns);
    let new_state = json_str(val, "session-state").to_lowercase();

    upsert_device(conn, &u.target, &u.vendor, &u.hostname, now.clone())?;

    // Read current state before upserting so we can detect transitions.
    let old_state = get_bgp_state(conn, &id)?;

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
            ("peer_as", Value::Int64(json_i64(val, "peer-as"))),
            ("state", Value::String(new_state.clone())),
            ("estab", Value::Int64(json_i64(val, "established-transitions"))),
            ("ts", now.clone()),
        ],
    )
    .context("execute BgpNeighbor upsert")?;

    // Emit a StateChangeEvent when session state transitions (or on first observation).
    if old_state.as_deref() != Some(new_state.as_str()) {
        let detail = format!(
            r#"{{"peer":"{}","old_state":"{}","new_state":"{}"}}"#,
            peer_addr,
            old_state.as_deref().unwrap_or("none"),
            new_state
        );
        write_state_change_event(conn, &u.target, "bgp_session_change", &detail, now.clone())?;
    }

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
        state = %new_state,
        "BGP neighbor written"
    );
    Ok(())
}

fn get_bgp_state(conn: &Connection<'_>, id: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("MATCH (n:BgpNeighbor {id: $id}) RETURN n.session_state")
        .context("prepare BGP state lookup")?;
    let mut result = conn
        .execute(&mut stmt, vec![("id", Value::String(id.to_string()))])
        .context("execute BGP state lookup")?;
    Ok(result.next().and_then(|row| {
        if let Value::String(s) = &row[0] { Some(s.clone()) } else { None }
    }))
}

fn write_state_change_event(
    conn: &Connection<'_>,
    device_address: &str,
    event_type: &str,
    detail: &str,
    now: Value,
) -> Result<()> {
    let id = Uuid::new_v4().to_string();

    let mut stmt = conn
        .prepare(
            "CREATE (e:StateChangeEvent {\
                id: $id, device_address: $addr, event_type: $etype, \
                detail: $detail, occurred_at: $ts})",
        )
        .context("prepare StateChangeEvent insert")?;

    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("addr", Value::String(device_address.to_string())),
            ("etype", Value::String(event_type.to_string())),
            ("detail", Value::String(detail.to_string())),
            ("ts", now.clone()),
        ],
    )
    .context("execute StateChangeEvent insert")?;

    let mut edge_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (e:StateChangeEvent {id: $id}) \
             CREATE (d)-[:REPORTED_BY]->(e)",
        )
        .context("prepare REPORTED_BY edge")?;

    conn.execute(
        &mut edge_stmt,
        vec![
            ("addr", Value::String(device_address.to_string())),
            ("id", Value::String(id)),
        ],
    )
    .context("execute REPORTED_BY edge")?;

    debug!(device = %device_address, event_type = %event_type, "state change event recorded");
    Ok(())
}

fn write_lldp_neighbor(
    conn: &Connection<'_>,
    u: &TelemetryUpdate,
    local_if: &str,
    neighbor_id: &str,
    val: &serde_json::Value,
) -> Result<()> {
    let id = format!("{}:{}:{}", u.target, local_if, neighbor_id);
    let now = ts(u.timestamp_ns);

    upsert_device(conn, &u.target, &u.vendor, &u.hostname, now.clone())?;

    // cEOS sends chassis-id and system-name/port-id in separate notifications.
    // Use CASE WHEN to preserve existing non-empty values on partial updates.
    let mut stmt = conn
        .prepare(
            "MERGE (n:LldpNeighbor {id: $id}) \
             ON CREATE SET \
               n.device_address = $addr, n.local_if = $local_if, n.neighbor_id = $nid, \
               n.chassis_id = $chassis, n.system_name = $sysname, n.port_id = $port, \
               n.updated_at = $ts \
             ON MATCH SET \
               n.chassis_id  = CASE WHEN $chassis  <> '' THEN $chassis  ELSE n.chassis_id  END, \
               n.system_name = CASE WHEN $sysname  <> '' THEN $sysname  ELSE n.system_name END, \
               n.port_id     = CASE WHEN $port     <> '' THEN $port     ELSE n.port_id     END, \
               n.updated_at  = $ts",
        )
        .context("prepare LldpNeighbor upsert")?;

    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("addr", Value::String(u.target.clone())),
            ("local_if", Value::String(local_if.to_string())),
            ("nid", Value::String(neighbor_id.to_string())),
            ("chassis", Value::String(json_str(val, "chassis-id").to_string())),
            ("sysname", Value::String(json_str(val, "system-name").to_string())),
            ("port", Value::String(json_str(val, "port-id").to_string())),
            ("ts", now.clone()),
        ],
    )
    .context("execute LldpNeighbor upsert")?;

    let mut edge_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (n:LldpNeighbor {id: $id}) \
             MERGE (d)-[:HAS_LLDP_NEIGHBOR]->(n)",
        )
        .context("prepare HAS_LLDP_NEIGHBOR merge")?;

    conn.execute(
        &mut edge_stmt,
        vec![
            ("addr", Value::String(u.target.clone())),
            ("id", Value::String(id)),
        ],
    )
    .context("execute HAS_LLDP_NEIGHBOR merge")?;

    // Best-effort: link the local Interface to the remote Interface via LLDP data.
    let system_name = json_str(val, "system-name").to_string();
    let port_id     = json_str(val, "port-id").to_string();
    if !system_name.is_empty() && !port_id.is_empty() {
        if let Err(e) = try_connect_interfaces(conn, &u.target, local_if, &system_name, &port_id) {
            debug!(error = %e, local_if, system_name, port_id, "CONNECTED_TO skipped");
        }
    }

    info!(
        target = %u.target,
        local_if = %local_if,
        chassis_id = %json_str(val, "chassis-id"),
        system_name = %json_str(val, "system-name"),
        "LLDP neighbor written"
    );
    Ok(())
}

/// Resolve the remote Interface by hostname+port_id and MERGE a CONNECTED_TO edge.
/// Returns Ok(()) if the remote is not yet in the graph — caller treats that as a no-op.
fn try_connect_interfaces(
    conn: &Connection<'_>,
    local_addr: &str,
    local_if: &str,
    remote_hostname: &str,
    remote_port_id: &str,
) -> Result<()> {
    // Find the remote device's address via its configured hostname.
    let mut find_stmt = conn
        .prepare("MATCH (d:Device {hostname: $hn}) RETURN d.address")
        .context("prepare remote device lookup")?;
    let mut result = conn
        .execute(&mut find_stmt, vec![("hn", Value::String(remote_hostname.to_string()))])
        .context("execute remote device lookup")?;

    let remote_addr = match result.next() {
        Some(row) => match &row[0] {
            Value::String(s) if !s.is_empty() => s.clone(),
            _ => return Ok(()),
        },
        None => return Ok(()),
    };

    let local_if_id  = format!("{}:{}", local_addr, local_if);
    let remote_if_id = format!("{}:{}", remote_addr, remote_port_id);

    let mut edge_stmt = conn
        .prepare(
            "MATCH (li:Interface {id: $lid}), (ri:Interface {id: $rid}) \
             MERGE (li)-[:CONNECTED_TO]->(ri)",
        )
        .context("prepare CONNECTED_TO merge")?;
    conn.execute(
        &mut edge_stmt,
        vec![
            ("lid", Value::String(local_if_id)),
            ("rid", Value::String(remote_if_id)),
        ],
    )
    .context("execute CONNECTED_TO merge")?;

    Ok(())
}

fn upsert_device(conn: &Connection<'_>, address: &str, vendor: &str, hostname: &str, now: Value) -> Result<()> {
    let mut stmt = conn.prepare(
        "MERGE (d:Device {address: $addr}) \
         ON CREATE SET d.vendor = $vendor, d.hostname = $hn, d.updated_at = $ts \
         ON MATCH SET d.hostname = $hn, d.updated_at = $ts",
    )
    .context("prepare Device upsert")?;

    conn.execute(
        &mut stmt,
        vec![
            ("addr", Value::String(address.to_string())),
            ("vendor", Value::String(vendor.to_string())),
            ("hn", Value::String(hostname.to_string())),
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
        ("devices",            "MATCH (n:Device) RETURN count(n)"),
        ("interfaces",         "MATCH (n:Interface) RETURN count(n)"),
        ("bgp-neighbors",      "MATCH (n:BgpNeighbor) RETURN count(n)"),
        ("lldp-neighbors",     "MATCH (n:LldpNeighbor) RETURN count(n)"),
        ("connected-to",       "MATCH ()-[r:CONNECTED_TO]->() RETURN count(r)"),
        ("state-change-events","MATCH (n:StateChangeEvent) RETURN count(n)"),
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
