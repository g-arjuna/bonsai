use std::sync::{Arc, Mutex};
use anyhow::{Context, Result};
use lbug::{Connection, Database, SystemConfig, Value};
use tokio::sync::broadcast;
use tracing::info;
use uuid::Uuid;

use crate::graph::common::*;
use crate::graph::BonsaiEvent;
use crate::store::BonsaiStore;
use crate::telemetry::{TelemetryEvent, TelemetryUpdate, json_i64, json_i64_multi, json_str};

pub struct CollectorGraphStore {
    db: Arc<Database>,
    event_tx: broadcast::Sender<BonsaiEvent>,
    write_lock: Arc<Mutex<()>>,
}

impl CollectorGraphStore {
    pub fn open(path: &str, buffer_pool_bytes: u64) -> Result<Self> {
        let sysconfig = SystemConfig::default().buffer_pool_size(buffer_pool_bytes);
        let db = Database::new(path, sysconfig).context("failed to open collector LadybugDB")?;
        info!(
            path,
            buffer_pool_mib = buffer_pool_bytes / 1024 / 1024,
            "collector LadybugDB opened"
        );
        let (event_tx, _) = broadcast::channel(1024);
        let store = CollectorGraphStore {
            db: Arc::new(db),
            event_tx,
            write_lock: Arc::new(Mutex::new(())),
        };
        store.init_schema()?;
        info!(path, "collector graph store opened");
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = Connection::new(&self.db).context("collector schema connection")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS Device(\
                address    STRING,\
                vendor     STRING,\
                hostname   STRING,\
                updated_at TIMESTAMP_NS,\
                PRIMARY KEY (address))",
        ).context("create Device table")?;

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
        ).context("create Interface table")?;

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
        ).context("create BgpNeighbor table")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS BfdSession(\
                id                  STRING,\
                device_address      STRING,\
                if_name             STRING,\
                local_discriminator STRING,\
                local_address       STRING,\
                remote_address      STRING,\
                session_state       STRING,\
                updated_at          TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        ).context("create BfdSession table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS HAS_INTERFACE(FROM Device TO Interface)")?;
        conn.query("CREATE REL TABLE IF NOT EXISTS PEERS_WITH(FROM Device TO BgpNeighbor)")?;
        conn.query("CREATE REL TABLE IF NOT EXISTS HAS_BFD_SESSION(FROM Device TO BfdSession)")?;

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
        ).context("create LldpNeighbor table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS HAS_LLDP_NEIGHBOR(FROM Device TO LldpNeighbor)")?;

        // Collector still tracks state changes locally to trigger rules
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS StateChangeEvent(\
                id             STRING,\
                device_address STRING,\
                event_type     STRING,\
                detail         STRING,\
                occurred_at    TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        ).context("create StateChangeEvent table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS REPORTED_BY(FROM Device TO StateChangeEvent)")?;
        conn.query("CREATE REL TABLE IF NOT EXISTS CONNECTED_TO(FROM Interface TO Interface)")?;

        info!("collector graph schema initialised");
        Ok(())
    }

    pub async fn write(&self, update: TelemetryUpdate) -> Result<()> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("collector write lock poisoned");
            let conn = Connection::new(&db).context("collector graph write connection")?;
            write_blocking(&conn, &update)
        }).await.context("spawn_blocking panicked")?
    }

    pub async fn write_detection(
        &self,
        device_address: String,
        rule_id: String,
        severity: String,
        features_json: String,
        fired_at_ns: i64,
        state_change_event_id: String,
    ) -> Result<String> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        let event_tx = self.event_tx.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("collector write lock poisoned");
            let conn = Connection::new(&db).context("collector detection write connection")?;
            let id = Uuid::new_v4().to_string();
            let now = ts(fired_at_ns);

            let mut stmt = conn.prepare(
                "MERGE (e:DetectionEvent {id: $id}) \
                 ON CREATE SET \
                   e.device_address = $addr, e.rule_id = $rule, \
                   e.severity = $sev, e.features_json = $feats, e.fired_at = $ts",
            ).context("prepare collector DetectionEvent insert")?;

            conn.execute(&mut stmt, vec![
                ("id", Value::String(id.clone())),
                ("addr", Value::String(device_address.clone())),
                ("rule", Value::String(rule_id.clone())),
                ("sev", Value::String(severity.clone())),
                ("feats", Value::String(features_json.clone())),
                ("ts", now),
            ]).context("execute collector DetectionEvent insert")?;

            let mut edge = conn.prepare(
                "MATCH (d:Device {address: $addr}), (e:DetectionEvent {id: $id}) CREATE (d)-[:TRIGGERED]->(e)"
            ).context("prepare collector TRIGGERED edge")?;
            conn.execute(&mut edge, vec![
                ("addr", Value::String(device_address.clone())),
                ("id", Value::String(id.clone())),
            ]).context("execute collector TRIGGERED edge")?;

            if !state_change_event_id.is_empty() {
                let mut tb = conn.prepare(
                    "MATCH (e:DetectionEvent {id: $eid}), (s:StateChangeEvent {id: $sid}) CREATE (e)-[:TRIGGERED_BY]->(s)"
                ).context("prepare collector TRIGGERED_BY edge")?;
                conn.execute(&mut tb, vec![
                    ("eid", Value::String(id.clone())),
                    ("sid", Value::String(state_change_event_id.clone())),
                ]).context("execute collector TRIGGERED_BY edge")?;
            }

            let _ = event_tx.send(BonsaiEvent {
                device_address,
                event_type: format!("detection:{}", rule_id),
                detail_json: features_json,
                occurred_at_ns: fired_at_ns,
                state_change_event_id,
            });

            Ok(id)
        }).await.context("spawn_blocking panicked")?
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<BonsaiEvent> {
        self.event_tx.subscribe()
    }
}

#[tonic::async_trait]
impl BonsaiStore for CollectorGraphStore {
    fn db(&self) -> Arc<Database> {
        Arc::clone(&self.db)
    }

    fn subscribe_events(&self) -> broadcast::Receiver<BonsaiEvent> {
        self.event_tx.subscribe()
    }

    async fn write(&self, update: TelemetryUpdate) -> Result<()> {
        self.write(update).await
    }

    async fn write_detection(
        &self,
        device_address: String,
        rule_id: String,
        severity: String,
        features_json: String,
        fired_at_ns: i64,
        state_change_event_id: String,
    ) -> Result<String> {
        self.write_detection(
            device_address,
            rule_id,
            severity,
            features_json,
            fired_at_ns,
            state_change_event_id,
        ).await
    }

    async fn write_remediation(
        &self,
        _detection_id: String,
        _action: String,
        _status: String,
        _detail_json: String,
        _attempted_at_ns: i64,
        _completed_at_ns: i64,
    ) -> Result<String> {
        // Collector remediation persistence is deferred
        Ok(String::new())
    }

    async fn sync_sites_from_targets(&self, _targets: Vec<crate::config::TargetConfig>) -> Result<()> {
        // Collector does not maintain site hierarchy graph
        Ok(())
    }

    async fn list_sites(&self) -> Result<Vec<crate::graph::SiteRecord>> {
        Ok(Vec::new())
    }

    async fn upsert_site(&self, site: crate::graph::SiteRecord) -> Result<crate::graph::SiteRecord> {
        Ok(site)
    }

    async fn write_subscription_status(
        &self,
        _status: crate::graph::SubscriptionStatusWrite,
    ) -> Result<()> {
        // Collector doesn't track subscription status in graph yet
        Ok(())
    }

    fn publish_event(&self, event: BonsaiEvent) {
        let _ = self.event_tx.send(event);
    }
}

fn write_blocking(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
) -> Result<()> {
    match update.classify() {
        TelemetryEvent::InterfaceStats { if_name } => {
            if update.value.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                return Ok(());
            }
            write_interface(conn, update, &if_name)
        }
        TelemetryEvent::BgpNeighborState { peer_address, state_value } => {
            write_bgp_neighbor(conn, update, &peer_address, state_value.as_ref().unwrap_or(&update.value))
        }
        TelemetryEvent::BfdSessionState { if_name, local_discriminator, state_value } => {
            write_bfd_session(conn, update, &if_name, &local_discriminator, state_value.as_ref().unwrap_or(&update.value))
        }
        TelemetryEvent::LldpNeighbor { local_if, neighbor_id, state_value } => {
            write_lldp_neighbor(conn, update, &local_if, &neighbor_id, state_value.as_ref().unwrap_or(&update.value))
        }
        _ => Ok(()),
    }
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
    )?;

    conn.execute(&mut stmt, vec![
        ("id", Value::String(id.clone())),
        ("addr", Value::String(u.target.clone())),
        ("name", Value::String(if_name.to_string())),
        ("in_pkts", Value::Int64(json_i64_multi(&u.value, &["in-packets", "packets-received", "input-packets", "in-pkts"]))),
        ("out_pkts", Value::Int64(json_i64_multi(&u.value, &["out-packets", "packets-sent", "output-packets", "out-pkts"]))),
        ("in_octets", Value::Int64(json_i64_multi(&u.value, &["in-octets", "bytes-received", "input-bytes"]))),
        ("out_octets", Value::Int64(json_i64_multi(&u.value, &["out-octets", "bytes-sent", "output-bytes"]))),
        ("in_errors", Value::Int64(json_i64_multi(&u.value, &["in-error-packets", "input-total-errors", "input-errors", "in-errors"]))),
        ("out_errors", Value::Int64(json_i64_multi(&u.value, &["out-error-packets", "output-total-errors", "output-errors", "out-errors"]))),
        ("carrier", Value::Int64(json_i64(&u.value, "carrier-transitions"))),
        ("ts", now),
    ])?;

    let mut edge_stmt = conn.prepare("MATCH (d:Device {address: $addr}), (i:Interface {id: $id}) MERGE (d)-[:HAS_INTERFACE]->(i)")?;
    conn.execute(&mut edge_stmt, vec![("addr", Value::String(u.target.clone())), ("id", Value::String(id))])?;
    Ok(())
}

fn write_bgp_neighbor(conn: &Connection<'_>, u: &TelemetryUpdate, peer_addr: &str, val: &serde_json::Value) -> Result<()> {
    let id = format!("{}:{}", u.target, peer_addr);
    let now = ts(u.timestamp_ns);
    upsert_device(conn, &u.target, &u.vendor, &u.hostname, now.clone())?;

    let mut stmt = conn.prepare(
        "MERGE (n:BgpNeighbor {id: $id}) \
         ON CREATE SET \
           n.device_address = $addr, n.peer_address = $peer, \
           n.peer_as = $peer_as, n.session_state = $state, \
           n.established_transitions = $estab, n.updated_at = $ts \
         ON MATCH SET \
           n.session_state = $state, \
           n.established_transitions = $estab, n.updated_at = $ts",
    )?;

    conn.execute(&mut stmt, vec![
        ("id", Value::String(id.clone())),
        ("addr", Value::String(u.target.clone())),
        ("peer", Value::String(peer_addr.to_string())),
        ("peer_as", Value::Int64(json_i64(val, "peer-as"))),
        ("state", Value::String(json_str(val, "session-state").to_lowercase())),
        ("estab", Value::Int64(json_i64(val, "established-transitions"))),
        ("ts", now),
    ])?;

    let mut edge_stmt = conn.prepare("MATCH (d:Device {address: $addr}), (n:BgpNeighbor {id: $id}) MERGE (d)-[:PEERS_WITH]->(n)")?;
    conn.execute(&mut edge_stmt, vec![("addr", Value::String(u.target.clone())), ("id", Value::String(id))])?;
    Ok(())
}

fn write_bfd_session(conn: &Connection<'_>, u: &TelemetryUpdate, if_name: &str, local_discriminator: &str, val: &serde_json::Value) -> Result<()> {
    let id = format!("{}:{}:{}", u.target, if_name, local_discriminator);
    let now = ts(u.timestamp_ns);
    upsert_device(conn, &u.target, &u.vendor, &u.hostname, now.clone())?;

    let mut stmt = conn.prepare(
        "MERGE (b:BfdSession {id: $id}) \
         ON CREATE SET \
           b.device_address = $addr, b.if_name = $if_name, \
           b.local_discriminator = $disc, b.local_address = $local_addr, \
           b.remote_address = $remote_addr, b.session_state = $state, \
           b.updated_at = $ts \
         ON MATCH SET \
           b.if_name = $if_name, b.local_address = $local_addr, \
           b.remote_address = $remote_addr, b.session_state = $state, \
           b.updated_at = $ts",
    )?;

    conn.execute(&mut stmt, vec![
        ("id", Value::String(id.clone())),
        ("addr", Value::String(u.target.clone())),
        ("if_name", Value::String(if_name.to_string())),
        ("disc", Value::String(local_discriminator.to_string())),
        ("local_addr", Value::String(json_str(val, "local-address").to_string())),
        ("remote_addr", Value::String(json_str(val, "remote-address").to_string())),
        ("state", Value::String(json_str(val, "session-state").to_lowercase())),
        ("ts", now),
    ])?;

    let mut edge_stmt = conn.prepare("MATCH (d:Device {address: $addr}), (b:BfdSession {id: $id}) MERGE (d)-[:HAS_BFD_SESSION]->(b)")?;
    conn.execute(&mut edge_stmt, vec![("addr", Value::String(u.target.clone())), ("id", Value::String(id))])?;
    Ok(())
}

fn write_lldp_neighbor(conn: &Connection<'_>, u: &TelemetryUpdate, local_if: &str, neighbor_id: &str, val: &serde_json::Value) -> Result<()> {
    let id = format!("{}:{}:{}", u.target, local_if, neighbor_id);
    let now = ts(u.timestamp_ns);
    upsert_device(conn, &u.target, &u.vendor, &u.hostname, now.clone())?;

    let mut stmt = conn.prepare(
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
    )?;

    conn.execute(&mut stmt, vec![
        ("id", Value::String(id.clone())),
        ("addr", Value::String(u.target.clone())),
        ("local_if", Value::String(local_if.to_string())),
        ("nid", Value::String(neighbor_id.to_string())),
        ("chassis", Value::String(json_str(val, "chassis-id").to_string())),
        ("sysname", Value::String(json_str(val, "system-name").to_string())),
        ("port", Value::String(json_str(val, "port-id").to_string())),
        ("ts", now),
    ])?;

    let mut edge_stmt = conn.prepare("MATCH (d:Device {address: $addr}), (n:LldpNeighbor {id: $id}) MERGE (d)-[:HAS_LLDP_NEIGHBOR]->(n)")?;
    conn.execute(&mut edge_stmt, vec![("addr", Value::String(u.target.clone())), ("id", Value::String(id))])?;
    Ok(())
}
