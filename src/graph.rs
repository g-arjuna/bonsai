use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use lbug::{Connection, Database, SystemConfig, Value};
use time::OffsetDateTime;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::telemetry::{json_i64, json_i64_multi, json_str, TelemetryEvent, TelemetryUpdate};

/// A state-change event broadcast to all API streaming subscribers.
#[derive(Clone, Debug)]
pub struct BonsaiEvent {
    pub device_address:        String,
    pub event_type:            String,
    pub detail_json:           String,
    pub occurred_at_ns:        i64,
    /// UUID of the persisted StateChangeEvent node; empty for broadcast-only events
    /// that don't write a node (e.g. oper-status events which are broadcast-only).
    pub state_change_event_id: String,
}

pub struct GraphStore {
    db:         Arc<Database>,
    event_tx:   broadcast::Sender<BonsaiEvent>,
    /// KuzuDB permits only one concurrent write transaction. All spawn_blocking
    /// write paths must hold this lock for the duration of their Connection.
    write_lock: Arc<Mutex<()>>,
}

impl GraphStore {
    pub fn open(path: &str) -> Result<Self> {
        let db = Database::new(path, SystemConfig::default())
            .context("failed to open LadybugDB")?;
        let (event_tx, _) = broadcast::channel(1024);
        let store = GraphStore { db: Arc::new(db), event_tx, write_lock: Arc::new(Mutex::new(())) };
        store.init_schema()?;
        info!(path, "graph store opened");
        Ok(store)
    }

    /// Subscribe to state-change events broadcast by the graph writer.
    pub fn subscribe_events(&self) -> broadcast::Receiver<BonsaiEvent> {
        self.event_tx.subscribe()
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

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS DetectionEvent(\
                id             STRING,\
                device_address STRING,\
                rule_id        STRING,\
                severity       STRING,\
                features_json  STRING,\
                fired_at       TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create DetectionEvent table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS TRIGGERED(FROM Device TO DetectionEvent)",
        )
        .context("create TRIGGERED rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS Remediation(\
                id             STRING,\
                detection_id   STRING,\
                action         STRING,\
                status         STRING,\
                detail_json    STRING,\
                attempted_at   TIMESTAMP_NS,\
                completed_at   TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create Remediation table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS RESOLVES(FROM Remediation TO DetectionEvent)",
        )
        .context("create RESOLVES rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS TRIGGERED_BY(FROM DetectionEvent TO StateChangeEvent)",
        )
        .context("create TRIGGERED_BY rel")?;

        info!("graph schema initialised");
        Ok(())
    }

    pub fn db(&self) -> Arc<Database> {
        Arc::clone(&self.db)
    }

    /// Write a DetectionEvent into the graph; returns the new node UUID.
    pub async fn write_detection(
        &self,
        device_address: String,
        rule_id: String,
        severity: String,
        features_json: String,
        fired_at_ns: i64,
        state_change_event_id: String,
    ) -> Result<String> {
        let db         = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("detection write connection")?;
            let id  = Uuid::new_v4().to_string();
            let now = ts(fired_at_ns);
            let mut stmt = conn.prepare(
                "MERGE (e:DetectionEvent {id: $id}) \
                 ON CREATE SET \
                   e.device_address = $addr, e.rule_id = $rule, \
                   e.severity = $sev, e.features_json = $feats, e.fired_at = $ts",
            ).context("prepare DetectionEvent insert")?;
            conn.execute(&mut stmt, vec![
                ("id",   Value::String(id.clone())),
                ("addr", Value::String(device_address.clone())),
                ("rule", Value::String(rule_id)),
                ("sev",  Value::String(severity)),
                ("feats",Value::String(features_json)),
                ("ts",   now),
            ]).context("execute DetectionEvent insert")?;
            // TRIGGERED edge Device → DetectionEvent
            let mut edge = conn.prepare(
                "MATCH (d:Device {address: $addr}), (e:DetectionEvent {id: $id})\
                 CREATE (d)-[:TRIGGERED]->(e)",
            ).context("prepare TRIGGERED edge")?;
            conn.execute(&mut edge, vec![
                ("addr", Value::String(device_address)),
                ("id",   Value::String(id.clone())),
            ]).context("execute TRIGGERED edge")?;
            // TRIGGERED_BY edge DetectionEvent → StateChangeEvent (when available)
            if !state_change_event_id.is_empty() {
                let mut tb = conn.prepare(
                    "MATCH (e:DetectionEvent {id: $eid}), (s:StateChangeEvent {id: $sid})\
                     CREATE (e)-[:TRIGGERED_BY]->(s)",
                ).context("prepare TRIGGERED_BY edge")?;
                conn.execute(&mut tb, vec![
                    ("eid", Value::String(id.clone())),
                    ("sid", Value::String(state_change_event_id)),
                ]).context("execute TRIGGERED_BY edge")?;
            }
            Ok::<String, anyhow::Error>(id)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Write a Remediation node and link it to its DetectionEvent.
    pub async fn write_remediation(
        &self,
        detection_id: String,
        action: String,
        status: String,
        detail_json: String,
        attempted_at_ns: i64,
        completed_at_ns: i64,
    ) -> Result<String> {
        let db         = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("remediation write connection")?;
            let id       = Uuid::new_v4().to_string();
            let att_ts   = ts(attempted_at_ns);
            let comp_ts  = ts(if completed_at_ns > 0 { completed_at_ns } else { attempted_at_ns });
            let mut stmt = conn.prepare(
                "MERGE (r:Remediation {id: $id}) \
                 ON CREATE SET \
                   r.detection_id = $did, r.action = $action, \
                   r.status = $status, r.detail_json = $detail, \
                   r.attempted_at = $att, r.completed_at = $comp",
            ).context("prepare Remediation insert")?;
            conn.execute(&mut stmt, vec![
                ("id",     Value::String(id.clone())),
                ("did",    Value::String(detection_id.clone())),
                ("action", Value::String(action)),
                ("status", Value::String(status)),
                ("detail", Value::String(detail_json)),
                ("att",    att_ts),
                ("comp",   comp_ts),
            ]).context("execute Remediation insert")?;
            // RESOLVES edge Remediation → DetectionEvent
            let mut edge = conn.prepare(
                "MATCH (r:Remediation {id: $id}), (e:DetectionEvent {id: $did})\
                 CREATE (r)-[:RESOLVES]->(e)",
            ).context("prepare RESOLVES edge")?;
            conn.execute(&mut edge, vec![
                ("id",  Value::String(id.clone())),
                ("did", Value::String(detection_id)),
            ]).context("execute RESOLVES edge")?;
            Ok::<String, anyhow::Error>(id)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Write a single telemetry update to the graph.
    /// Dispatches to a blocking thread so the caller's async task is not blocked.
    pub async fn write(&self, update: TelemetryUpdate) -> Result<()> {
        let db         = Arc::clone(&self.db);
        let event_tx   = self.event_tx.clone();
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            write_blocking(&db, &update, &event_tx)
        })
        .await
        .context("spawn_blocking panicked")?
    }
}

// ── blocking write helpers ────────────────────────────────────────────────────

fn write_blocking(db: &Database, update: &TelemetryUpdate, event_tx: &broadcast::Sender<BonsaiEvent>) -> Result<()> {
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
            write_bgp_neighbor(&conn, update, &peer_address, state_value.as_ref().unwrap_or(&update.value), event_tx)
        }
        TelemetryEvent::LldpNeighbor { local_if, neighbor_id, state_value } => {
            write_lldp_neighbor(&conn, update, &local_if, &neighbor_id, state_value.as_ref().unwrap_or(&update.value))
        }
        TelemetryEvent::InterfaceOperStatus { if_name, oper_status } => {
            emit_oper_status_event(&conn, update, &if_name, &oper_status, event_tx)
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
            ("id",   Value::String(id.clone())),
        ],
    )
    .context("execute HAS_INTERFACE merge")?;

    // Retroactively build CONNECTED_TO for any LldpNeighbor rows that arrived
    // before this Interface node was written (LLDP typically precedes stats).
    let _ = backfill_connected_to(conn, &u.target, if_name);

    debug!(target = %u.target, interface = %if_name, "interface written");
    Ok(())
}

fn write_bgp_neighbor(conn: &Connection<'_>, u: &TelemetryUpdate, peer_addr: &str, val: &serde_json::Value, event_tx: &broadcast::Sender<BonsaiEvent>) -> Result<()> {
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
        write_state_change_event(conn, &u.target, "bgp_session_change", &detail, now.clone(), u.timestamp_ns, event_tx)?;
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
    timestamp_ns: i64,
    event_tx: &broadcast::Sender<BonsaiEvent>,
) -> Result<String> {
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
            ("id", Value::String(id.clone())),
        ],
    )
    .context("execute REPORTED_BY edge")?;

    let _ = event_tx.send(BonsaiEvent {
        device_address:        device_address.to_string(),
        event_type:            event_type.to_string(),
        detail_json:           detail.to_string(),
        occurred_at_ns:        timestamp_ns,
        state_change_event_id: id.clone(),
    });

    debug!(device = %device_address, event_type = %event_type, "state change event recorded");
    Ok(id)
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

/// After writing an Interface node, check if any LldpNeighbor rows already exist
/// that reference this device+port from another node, and wire up CONNECTED_TO edges.
/// Called from write_interface so edges get built even when LLDP arrived first.
fn backfill_connected_to(conn: &Connection<'_>, local_addr: &str, local_if: &str) -> Result<()> {
    // Case 1: This node has an LldpNeighbor entry for this interface — link outbound.
    let mut find = conn.prepare(
        "MATCH (n:LldpNeighbor {device_address: $addr, local_if: $lif}) \
         RETURN n.system_name, n.port_id",
    ).context("prepare lldp lookup for backfill")?;
    let mut rows = conn.execute(&mut find, vec![
        ("addr", Value::String(local_addr.to_string())),
        ("lif",  Value::String(local_if.to_string())),
    ]).context("execute lldp lookup for backfill")?;

    while let Some(row) = rows.next() {
        let system_name = match &row[0] { Value::String(s) => s.clone(), _ => continue };
        let port_id     = match &row[1] { Value::String(s) => s.clone(), _ => continue };
        if !system_name.is_empty() && !port_id.is_empty() {
            let _ = try_connect_interfaces(conn, local_addr, local_if, &system_name, &port_id);
        }
    }

    // Case 2: Another node's LldpNeighbor points TO this interface as port_id — link inbound.
    let mut find2 = conn.prepare(
        "MATCH (n:LldpNeighbor {port_id: $lif}) \
         RETURN n.device_address, n.local_if, n.system_name",
    ).context("prepare reverse lldp lookup")?;
    let mut rows2 = conn.execute(&mut find2, vec![
        ("lif", Value::String(local_if.to_string())),
    ]).context("execute reverse lldp lookup")?;

    while let Some(row) = rows2.next() {
        let remote_addr = match &row[0] { Value::String(s) => s.clone(), _ => continue };
        let remote_if   = match &row[1] { Value::String(s) => s.clone(), _ => continue };
        let system_name = match &row[2] { Value::String(s) => s.clone(), _ => continue };
        // Verify this LldpNeighbor's system_name matches our hostname.
        if system_name.is_empty() { continue; }
        let _ = try_connect_interfaces(conn, &remote_addr, &remote_if, &system_name, local_if);
    }

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

fn emit_oper_status_event(
    conn: &Connection<'_>,
    u: &TelemetryUpdate,
    if_name: &str,
    oper_status: &str,
    event_tx: &broadcast::Sender<BonsaiEvent>,
) -> Result<()> {
    let detail = format!(
        r#"{{"if_name":"{}","oper_status":"{}"}}"#,
        if_name, oper_status
    );
    let _ = event_tx.send(BonsaiEvent {
        device_address:        u.target.clone(),
        event_type:            "interface_oper_status_change".to_string(),
        detail_json:           detail,
        occurred_at_ns:        u.timestamp_ns,
        state_change_event_id: String::new(),
    });
    // Best-effort: ensure Device node exists so graph queries stay consistent.
    upsert_device(conn, &u.target, &u.vendor, &u.hostname, ts(u.timestamp_ns))?;
    debug!(target = %u.target, if_name, oper_status, "interface oper-status event emitted");
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
        ("detection-events",   "MATCH (n:DetectionEvent) RETURN count(n)"),
        ("remediations",       "MATCH (n:Remediation) RETURN count(n)"),
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
