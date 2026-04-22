use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use lbug::{Connection, Database, SystemConfig, Value};
use serde::Serialize;
use time::OffsetDateTime;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::config::TargetConfig;
use crate::telemetry::{TelemetryEvent, TelemetryUpdate, json_i64, json_i64_multi, json_str};

pub const REMEDIATION_TRUST_CUTOFF_ISO: &str = "2026-04-20T09:32:50+00:00";
pub const REMEDIATION_TRUST_CUTOFF_NS: i64 = 1_776_677_570_000_000_000;
const REMEDIATION_TRUST_REASON_PRE_CUTOFF: &str = "pre_t0_2_verify_cutoff";
const REMEDIATION_TRUST_REASON_POST_CUTOFF: &str = "post_t0_2_verify_cutoff";
const MAX_SITE_HIERARCHY_DEPTH: usize = 10;

/// A state-change event broadcast to all API streaming subscribers.
#[derive(Clone, Debug)]
pub struct BonsaiEvent {
    pub device_address: String,
    pub event_type: String,
    pub detail_json: String,
    pub occurred_at_ns: i64,
    /// UUID of the persisted StateChangeEvent node; empty for broadcast-only events
    /// that don't write a node (e.g. oper-status events which are broadcast-only).
    pub state_change_event_id: String,
}

/// A detection + its linked remediation (if any). Used by the HTTP topology API.
#[derive(Debug, Clone, Serialize)]
pub struct DetectionRow {
    pub id: String,
    pub device_address: String,
    pub rule_id: String,
    pub severity: String,
    pub features_json: String,
    pub fired_at_ns: i64,
    pub remediation_id: String,
    pub remediation_action: String,
    pub remediation_status: String,
}

/// One step in a closed-loop trace: trigger → detection → remediation.
#[derive(Debug, Clone, Serialize)]
pub struct TraceStep {
    pub kind: String, // "trigger" | "detection" | "remediation"
    pub id: String,
    pub device_address: String,
    pub event_type: String,
    pub rule_id: String,
    pub severity: String,
    pub action: String,
    pub status: String,
    pub detail_json: String,
    pub occurred_at_ns: i64,
}

#[derive(Debug, Clone)]
pub struct SubscriptionStatusWrite {
    pub device_address: String,
    pub path: String,
    pub origin: String,
    pub mode: String,
    pub sample_interval_ns: i64,
    pub status: String,
    pub first_observed_at_ns: i64,
    pub last_observed_at_ns: i64,
    pub updated_at_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SiteRecord {
    pub id: String,
    pub name: String,
    pub parent_id: String,
    pub kind: String,
    pub lat: f64,
    pub lon: f64,
    pub metadata_json: String,
}

pub struct GraphStore {
    db: Arc<Database>,
    event_tx: broadcast::Sender<BonsaiEvent>,
    /// KuzuDB permits only one concurrent write transaction. All spawn_blocking
    /// write paths must hold this lock for the duration of their Connection.
    write_lock: Arc<Mutex<()>>,
}

impl GraphStore {
    pub fn open(path: &str) -> Result<Self> {
        let db =
            Database::new(path, SystemConfig::default()).context("failed to open LadybugDB")?;
        let (event_tx, _) = broadcast::channel(1024);
        let store = GraphStore {
            db: Arc::new(db),
            event_tx,
            write_lock: Arc::new(Mutex::new(())),
        };
        store.init_schema()?;
        store.backfill_remediation_trust_marks()?;
        info!(path, "graph store opened");
        Ok(store)
    }

    /// Subscribe to state-change events broadcast by the graph writer.
    pub fn subscribe_events(&self) -> broadcast::Receiver<BonsaiEvent> {
        self.event_tx.subscribe()
    }

    /// Publish a best-effort event to HTTP/SSE subscribers.
    pub fn publish_event(&self, event: BonsaiEvent) {
        if self.event_tx.send(event).is_err() {
            metrics::counter!("bonsai_broadcast_drops_total").increment(1);
        }
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
            "CREATE NODE TABLE IF NOT EXISTS Site(\
                id            STRING,\
                name          STRING,\
                parent_id     STRING,\
                kind          STRING,\
                lat           DOUBLE,\
                lon           DOUBLE,\
                metadata_json STRING,\
                updated_at    TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create Site table")?;

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
        )
        .context("create BfdSession table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS HAS_INTERFACE(FROM Device TO Interface)")
            .context("create HAS_INTERFACE rel")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS LOCATED_AT(FROM Device TO Site)")
            .context("create LOCATED_AT rel")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS PARENT_OF(FROM Site TO Site)")
            .context("create PARENT_OF rel")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS PEERS_WITH(FROM Device TO BgpNeighbor)")
            .context("create PEERS_WITH rel")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS HAS_BFD_SESSION(FROM Device TO BfdSession)")
            .context("create HAS_BFD_SESSION rel")?;

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

        conn.query("CREATE REL TABLE IF NOT EXISTS HAS_LLDP_NEIGHBOR(FROM Device TO LldpNeighbor)")
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

        conn.query("CREATE REL TABLE IF NOT EXISTS REPORTED_BY(FROM Device TO StateChangeEvent)")
            .context("create REPORTED_BY rel")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS CONNECTED_TO(FROM Interface TO Interface)")
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

        conn.query("CREATE REL TABLE IF NOT EXISTS TRIGGERED(FROM Device TO DetectionEvent)")
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

        conn.query("CREATE REL TABLE IF NOT EXISTS RESOLVES(FROM Remediation TO DetectionEvent)")
            .context("create RESOLVES rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS RemediationTrustMark(\
                remediation_id STRING,\
                trustworthy    INT64,\
                reason         STRING,\
                decided_at     TIMESTAMP_NS,\
                PRIMARY KEY (remediation_id))",
        )
        .context("create RemediationTrustMark table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS TRUST_MARKS(FROM RemediationTrustMark TO Remediation)",
        )
        .context("create TRUST_MARKS rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS TRIGGERED_BY(FROM DetectionEvent TO StateChangeEvent)",
        )
        .context("create TRIGGERED_BY rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS SubscriptionStatus(\
                id                 STRING,\
                device_address     STRING,\
                path               STRING,\
                origin             STRING,\
                mode               STRING,\
                sample_interval_ns INT64,\
                status             STRING,\
                first_observed_at  TIMESTAMP_NS,\
                last_observed_at   TIMESTAMP_NS,\
                updated_at         TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create SubscriptionStatus table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_SUBSCRIPTION_STATUS(FROM Device TO SubscriptionStatus)",
        )
        .context("create HAS_SUBSCRIPTION_STATUS rel")?;

        info!("graph schema initialised");
        Ok(())
    }

    pub fn db(&self) -> Arc<Database> {
        Arc::clone(&self.db)
    }

    pub async fn list_sites(&self) -> Result<Vec<SiteRecord>> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("list sites connection")?;
            let rows = conn
                .query(
                    "MATCH (s:Site) \
                     RETURN s.id, s.name, s.parent_id, s.kind, s.lat, s.lon, s.metadata_json \
                     ORDER BY s.name",
                )
                .context("list sites query")?;
            Ok::<_, anyhow::Error>(rows.map(site_from_row).collect())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn upsert_site(&self, site: SiteRecord) -> Result<SiteRecord> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("site write connection")?;
            let site = normalize_site(site)?;
            upsert_site_record(&conn, &site, ts(now_ns()))?;
            Ok::<_, anyhow::Error>(site)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn sync_sites_from_targets(&self, targets: Vec<TargetConfig>) -> Result<()> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("site sync connection")?;
            let now = ts(now_ns());
            for target in targets {
                let Some(site_name) = target
                    .site
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                else {
                    continue;
                };
                let site = SiteRecord {
                    id: site_id_from_name(site_name),
                    name: site_name.to_string(),
                    parent_id: String::new(),
                    kind: "unknown".to_string(),
                    lat: 0.0,
                    lon: 0.0,
                    metadata_json: "{}".to_string(),
                };
                upsert_device(
                    &conn,
                    &target.address,
                    target.vendor.as_deref().unwrap_or_default(),
                    target.hostname.as_deref().unwrap_or_default(),
                    now.clone(),
                )?;
                upsert_site_record(&conn, &site, now.clone())?;
                link_device_to_site(&conn, &target.address, &site.id)?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    fn backfill_remediation_trust_marks(&self) -> Result<()> {
        let conn = Connection::new(&self.db).context("trust-mark backfill connection")?;
        let mut stmt = conn
            .prepare(
                "MATCH (r:Remediation) \
                 OPTIONAL MATCH (m:RemediationTrustMark {remediation_id: r.id}) \
                 RETURN r.id, r.attempted_at, m.remediation_id",
            )
            .context("prepare trust-mark backfill query")?;
        let rows = conn
            .execute(&mut stmt, Vec::new())
            .context("execute trust-mark backfill query")?;

        let mut created = 0usize;
        for row in rows {
            let remediation_id = read_str(&row[0]);
            if remediation_id.is_empty() || !read_str(&row[2]).is_empty() {
                continue;
            }
            write_remediation_trust_mark(&conn, &remediation_id, read_ts_ns(&row[1]))?;
            created += 1;
        }

        if created > 0 {
            info!(created, "backfilled remediation trust marks");
        }
        Ok(())
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
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("detection write connection")?;
            let id = Uuid::new_v4().to_string();
            let now = ts(fired_at_ns);
            let metric_rule_id = rule_id.clone();
            let metric_severity = severity.clone();
            let mut stmt = conn
                .prepare(
                    "MERGE (e:DetectionEvent {id: $id}) \
                 ON CREATE SET \
                   e.device_address = $addr, e.rule_id = $rule, \
                   e.severity = $sev, e.features_json = $feats, e.fired_at = $ts",
                )
                .context("prepare DetectionEvent insert")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("addr", Value::String(device_address.clone())),
                    ("rule", Value::String(rule_id)),
                    ("sev", Value::String(severity)),
                    ("feats", Value::String(features_json)),
                    ("ts", now),
                ],
            )
            .context("execute DetectionEvent insert")?;
            // TRIGGERED edge Device → DetectionEvent
            let mut edge = conn
                .prepare(
                    "MATCH (d:Device {address: $addr}), (e:DetectionEvent {id: $id})\
                 CREATE (d)-[:TRIGGERED]->(e)",
                )
                .context("prepare TRIGGERED edge")?;
            conn.execute(
                &mut edge,
                vec![
                    ("addr", Value::String(device_address)),
                    ("id", Value::String(id.clone())),
                ],
            )
            .context("execute TRIGGERED edge")?;
            // TRIGGERED_BY edge DetectionEvent → StateChangeEvent (when available)
            if !state_change_event_id.is_empty() {
                let mut tb = conn
                    .prepare(
                        "MATCH (e:DetectionEvent {id: $eid}), (s:StateChangeEvent {id: $sid})\
                     CREATE (e)-[:TRIGGERED_BY]->(s)",
                    )
                    .context("prepare TRIGGERED_BY edge")?;
                conn.execute(
                    &mut tb,
                    vec![
                        ("eid", Value::String(id.clone())),
                        ("sid", Value::String(state_change_event_id)),
                    ],
                )
                .context("execute TRIGGERED_BY edge")?;
            }
            metrics::counter!(
                "bonsai_rule_firings_total",
                "rule_id" => metric_rule_id,
                "severity" => metric_severity
            )
            .increment(1);
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
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("remediation write connection")?;
            let id = Uuid::new_v4().to_string();
            let att_ts = ts(attempted_at_ns);
            let comp_ts = ts(if completed_at_ns > 0 {
                completed_at_ns
            } else {
                attempted_at_ns
            });
            let mut stmt = conn
                .prepare(
                    "MERGE (r:Remediation {id: $id}) \
                 ON CREATE SET \
                   r.detection_id = $did, r.action = $action, \
                   r.status = $status, r.detail_json = $detail, \
                   r.attempted_at = $att, r.completed_at = $comp",
                )
                .context("prepare Remediation insert")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("did", Value::String(detection_id.clone())),
                    ("action", Value::String(action)),
                    ("status", Value::String(status)),
                    ("detail", Value::String(detail_json)),
                    ("att", att_ts),
                    ("comp", comp_ts),
                ],
            )
            .context("execute Remediation insert")?;
            // RESOLVES edge Remediation → DetectionEvent
            let mut edge = conn
                .prepare(
                    "MATCH (r:Remediation {id: $id}), (e:DetectionEvent {id: $did})\
                 CREATE (r)-[:RESOLVES]->(e)",
                )
                .context("prepare RESOLVES edge")?;
            conn.execute(
                &mut edge,
                vec![
                    ("id", Value::String(id.clone())),
                    ("did", Value::String(detection_id)),
                ],
            )
            .context("execute RESOLVES edge")?;
            write_remediation_trust_mark(&conn, &id, attempted_at_ns)?;
            Ok::<String, anyhow::Error>(id)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Return the most recent `limit` DetectionEvents joined with their Remediation.
    pub async fn read_detections(&self, limit: u32) -> Result<Vec<DetectionRow>> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("read_detections connection")?;
            let cypher = format!(
                "MATCH (e:DetectionEvent) \
                 OPTIONAL MATCH (r:Remediation)-[:RESOLVES]->(e) \
                 RETURN e.id, e.device_address, e.rule_id, e.severity, \
                        e.features_json, e.fired_at, r.id, r.action, r.status \
                 ORDER BY e.fired_at DESC LIMIT {limit}"
            );
            let rows = conn.query(&cypher).context("read_detections query")?;
            let mut out = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            for row in rows {
                let id = read_str(&row[0]);
                // OPTIONAL MATCH can produce duplicate detection rows when multiple
                // remediations exist for one detection — keep only the first.
                if seen.insert(id.clone()) {
                    out.push(DetectionRow {
                        id,
                        device_address: read_str(&row[1]),
                        rule_id: read_str(&row[2]),
                        severity: read_str(&row[3]),
                        features_json: read_str(&row[4]),
                        fired_at_ns: read_ts_ns(&row[5]),
                        remediation_id: read_str(&row[6]),
                        remediation_action: read_str(&row[7]),
                        remediation_status: read_str(&row[8]),
                    });
                }
            }
            Ok::<_, anyhow::Error>(out)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Return all steps in a closed-loop trace for a given DetectionEvent id.
    /// Steps are ordered: trigger → detection → remediation.
    pub async fn read_closed_loop_trace(&self, detection_id: String) -> Result<Vec<TraceStep>> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("read_trace connection")?;
            let mut stmt = conn
                .prepare(
                    "MATCH (e:DetectionEvent {id: $id}) \
                 OPTIONAL MATCH (e)-[:TRIGGERED_BY]->(s:StateChangeEvent) \
                 OPTIONAL MATCH (r:Remediation)-[:RESOLVES]->(e) \
                 RETURN e.id, e.device_address, e.rule_id, e.severity, e.fired_at, \
                        s.id, s.event_type, s.detail, s.occurred_at, s.device_address, \
                        r.id, r.action, r.status, r.detail_json, r.attempted_at",
                )
                .context("prepare trace query")?;
            let rows = conn
                .execute(&mut stmt, vec![("id", Value::String(detection_id))])
                .context("execute trace query")?;

            let mut steps: Vec<TraceStep> = Vec::new();
            let mut seen_det = false;
            let mut seen_trig: HashSet<String> = HashSet::new();
            let mut seen_rem: HashSet<String> = HashSet::new();

            for row in rows {
                if !seen_det {
                    seen_det = true;
                    steps.push(TraceStep {
                        kind: "detection".into(),
                        id: read_str(&row[0]),
                        device_address: read_str(&row[1]),
                        rule_id: read_str(&row[2]),
                        severity: read_str(&row[3]),
                        occurred_at_ns: read_ts_ns(&row[4]),
                        event_type: String::new(),
                        action: String::new(),
                        status: String::new(),
                        detail_json: String::new(),
                    });
                }
                let trig_id = read_str(&row[5]);
                if !trig_id.is_empty() && seen_trig.insert(trig_id.clone()) {
                    steps.push(TraceStep {
                        kind: "trigger".into(),
                        id: trig_id,
                        device_address: read_str(&row[9]),
                        event_type: read_str(&row[6]),
                        detail_json: read_str(&row[7]),
                        occurred_at_ns: read_ts_ns(&row[8]),
                        rule_id: String::new(),
                        severity: String::new(),
                        action: String::new(),
                        status: String::new(),
                    });
                }
                let rem_id = read_str(&row[10]);
                if !rem_id.is_empty() && seen_rem.insert(rem_id.clone()) {
                    steps.push(TraceStep {
                        kind: "remediation".into(),
                        id: rem_id,
                        action: read_str(&row[11]),
                        status: read_str(&row[12]),
                        detail_json: read_str(&row[13]),
                        occurred_at_ns: read_ts_ns(&row[14]),
                        device_address: String::new(),
                        event_type: String::new(),
                        rule_id: String::new(),
                        severity: String::new(),
                    });
                }
            }
            // Sort: trigger first, detection second, remediation last; within each kind by time.
            steps.sort_by_key(|s| {
                (
                    match s.kind.as_str() {
                        "trigger" => 0u8,
                        "detection" => 1,
                        _ => 2,
                    },
                    s.occurred_at_ns,
                )
            });
            Ok::<_, anyhow::Error>(steps)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Write a single telemetry update to the graph.
    /// Dispatches to a blocking thread so the caller's async task is not blocked.
    pub async fn write(&self, update: TelemetryUpdate) -> Result<()> {
        let db = Arc::clone(&self.db);
        let event_tx = self.event_tx.clone();
        let write_lock = Arc::clone(&self.write_lock);
        let target = update.target.clone();
        tokio::task::spawn_blocking(move || {
            metrics::counter!("bonsai_telemetry_updates_total", "target" => target.clone())
                .increment(1);
            let t0 = Instant::now();
            let _guard = write_lock.lock().expect("write lock poisoned");
            let result = write_blocking(&db, &update, &event_tx);
            metrics::histogram!("bonsai_graph_write_latency_seconds", "target" => target)
                .record(t0.elapsed().as_secs_f64());
            result
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn write_subscription_status(&self, status: SubscriptionStatusWrite) -> Result<()> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("subscription status write connection")?;
            write_subscription_status_blocking(&conn, status)
        })
        .await
        .context("spawn_blocking panicked")?
    }
}

// ── blocking write helpers ────────────────────────────────────────────────────

fn write_blocking(
    db: &Database,
    update: &TelemetryUpdate,
    event_tx: &broadcast::Sender<BonsaiEvent>,
) -> Result<()> {
    let conn = Connection::new(db).context("graph write connection")?;
    match update.classify() {
        TelemetryEvent::InterfaceStats { if_name } => {
            // Skip interfaces with no data (SR Linux sends empty {} for unconfigured ports)
            if update
                .value
                .as_object()
                .map(|o| o.is_empty())
                .unwrap_or(true)
            {
                return Ok(());
            }
            write_interface(&conn, update, &if_name)
        }
        TelemetryEvent::BgpNeighborState {
            peer_address,
            state_value,
        } => write_bgp_neighbor(
            &conn,
            update,
            &peer_address,
            state_value.as_ref().unwrap_or(&update.value),
            event_tx,
        ),
        TelemetryEvent::BfdSessionState {
            if_name,
            local_discriminator,
            state_value,
        } => write_bfd_session(
            &conn,
            update,
            &if_name,
            &local_discriminator,
            state_value.as_ref().unwrap_or(&update.value),
            event_tx,
        ),
        TelemetryEvent::LldpNeighbor {
            local_if,
            neighbor_id,
            state_value,
        } => write_lldp_neighbor(
            &conn,
            update,
            &local_if,
            &neighbor_id,
            state_value.as_ref().unwrap_or(&update.value),
        ),
        TelemetryEvent::InterfaceOperStatus {
            if_name,
            oper_status,
        } => emit_oper_status_event(&conn, update, &if_name, &oper_status, event_tx),
        TelemetryEvent::Ignored => Ok(()),
    }
}

fn write_subscription_status_blocking(
    conn: &Connection<'_>,
    status: SubscriptionStatusWrite,
) -> Result<()> {
    let id = subscription_status_id(
        &status.device_address,
        &status.path,
        &status.origin,
        &status.mode,
        status.sample_interval_ns,
    );
    let updated_at = ts(status.updated_at_ns);
    let first_observed_at = ts(status.first_observed_at_ns);
    let last_observed_at = ts(status.last_observed_at_ns);

    upsert_device(conn, &status.device_address, "", "", updated_at.clone())?;

    let mut stmt = conn
        .prepare(
            "MERGE (s:SubscriptionStatus {id: $id}) \
             ON CREATE SET \
               s.device_address = $addr, s.path = $path, s.origin = $origin, \
               s.mode = $mode, s.sample_interval_ns = $interval, s.status = $status, \
               s.first_observed_at = $first, s.last_observed_at = $last, s.updated_at = $updated \
             ON MATCH SET \
               s.device_address = $addr, s.path = $path, s.origin = $origin, \
               s.mode = $mode, s.sample_interval_ns = $interval, s.status = $status, \
               s.first_observed_at = $first, s.last_observed_at = $last, s.updated_at = $updated",
        )
        .context("prepare SubscriptionStatus upsert")?;

    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("addr", Value::String(status.device_address.clone())),
            ("path", Value::String(status.path)),
            ("origin", Value::String(status.origin)),
            ("mode", Value::String(status.mode)),
            ("interval", Value::Int64(status.sample_interval_ns)),
            ("status", Value::String(status.status)),
            ("first", first_observed_at),
            ("last", last_observed_at),
            ("updated", updated_at),
        ],
    )
    .context("execute SubscriptionStatus upsert")?;

    let mut edge_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (s:SubscriptionStatus {id: $id}) \
             MERGE (d)-[:HAS_SUBSCRIPTION_STATUS]->(s)",
        )
        .context("prepare HAS_SUBSCRIPTION_STATUS merge")?;
    conn.execute(
        &mut edge_stmt,
        vec![
            ("addr", Value::String(status.device_address)),
            ("id", Value::String(id)),
        ],
    )
    .context("execute HAS_SUBSCRIPTION_STATUS merge")?;

    Ok(())
}

fn subscription_status_id(
    device_address: &str,
    path: &str,
    origin: &str,
    mode: &str,
    sample_interval_ns: i64,
) -> String {
    format!("{device_address}|{origin}|{mode}|{sample_interval_ns}|{path}")
}

/// Read helpers for query result rows — used by the read_* methods above.
fn read_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

fn read_f64(v: &Value) -> f64 {
    match v {
        Value::Double(n) => *n,
        Value::Float(n) => (*n).into(),
        _ => 0.0,
    }
}

#[cfg(test)]
fn read_i64(v: &Value) -> i64 {
    match v {
        Value::Int64(n) => *n,
        _ => 0,
    }
}

fn read_ts_ns(v: &Value) -> i64 {
    match v {
        Value::TimestampNs(dt) => dt.unix_timestamp_nanos() as i64,
        _ => 0,
    }
}

fn write_remediation_trust_mark(
    conn: &Connection<'_>,
    remediation_id: &str,
    attempted_at_ns: i64,
) -> Result<()> {
    let trustworthy = if attempted_at_ns > REMEDIATION_TRUST_CUTOFF_NS {
        1
    } else {
        0
    };
    let reason = if trustworthy == 1 {
        REMEDIATION_TRUST_REASON_POST_CUTOFF
    } else {
        REMEDIATION_TRUST_REASON_PRE_CUTOFF
    };

    let mut stmt = conn
        .prepare(
            "MERGE (m:RemediationTrustMark {remediation_id: $rid}) \
             ON CREATE SET \
               m.trustworthy = $trustworthy, m.reason = $reason, m.decided_at = $decided_at \
             ON MATCH SET \
               m.trustworthy = $trustworthy, m.reason = $reason, m.decided_at = $decided_at",
        )
        .context("prepare RemediationTrustMark upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("rid", Value::String(remediation_id.to_string())),
            ("trustworthy", Value::Int64(trustworthy)),
            ("reason", Value::String(reason.to_string())),
            (
                "decided_at",
                ts(attempted_at_ns.max(REMEDIATION_TRUST_CUTOFF_NS)),
            ),
        ],
    )
    .context("execute RemediationTrustMark upsert")?;

    let mut edge_stmt = conn
        .prepare(
            "MATCH (m:RemediationTrustMark {remediation_id: $rid}), (r:Remediation {id: $rid}) \
             MERGE (m)-[:TRUST_MARKS]->(r)",
        )
        .context("prepare TRUST_MARKS edge")?;
    conn.execute(
        &mut edge_stmt,
        vec![("rid", Value::String(remediation_id.to_string()))],
    )
    .context("execute TRUST_MARKS edge")?;

    Ok(())
}

fn ts(ns: i64) -> Value {
    let dt = OffsetDateTime::UNIX_EPOCH + time::Duration::nanoseconds(ns);
    Value::TimestampNs(dt)
}

fn write_interface(conn: &Connection<'_>, u: &TelemetryUpdate, if_name: &str) -> Result<()> {
    let id = format!("{}:{}", u.target, if_name);
    let now = ts(u.timestamp_ns);

    upsert_device(conn, &u.target, &u.vendor, &u.hostname, now.clone())?;

    let mut stmt = conn
        .prepare(
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
            (
                "in_pkts",
                Value::Int64(json_i64_multi(
                    &u.value,
                    &[
                        "in-packets",       // SRL native
                        "packets-received", // XR native (generic-counters)
                        "input-packets",    // Junos native
                        "in-pkts",          // OC
                    ],
                )),
            ),
            (
                "out_pkts",
                Value::Int64(json_i64_multi(
                    &u.value,
                    &[
                        "out-packets",    // SRL native
                        "packets-sent",   // XR native
                        "output-packets", // Junos native
                        "out-pkts",       // OC
                    ],
                )),
            ),
            (
                "in_octets",
                Value::Int64(json_i64_multi(
                    &u.value,
                    &[
                        "in-octets",      // SRL native & OC
                        "bytes-received", // XR native
                        "input-bytes",    // Junos native
                    ],
                )),
            ),
            (
                "out_octets",
                Value::Int64(json_i64_multi(
                    &u.value,
                    &[
                        "out-octets",   // SRL native & OC
                        "bytes-sent",   // XR native
                        "output-bytes", // Junos native
                    ],
                )),
            ),
            (
                "in_errors",
                Value::Int64(json_i64_multi(
                    &u.value,
                    &[
                        "in-error-packets",   // SRL native
                        "input-total-errors", // XR native
                        "input-errors",       // Junos native
                        "in-errors",          // OC
                    ],
                )),
            ),
            (
                "out_errors",
                Value::Int64(json_i64_multi(
                    &u.value,
                    &[
                        "out-error-packets",   // SRL native
                        "output-total-errors", // XR native
                        "output-errors",       // Junos native
                        "out-errors",          // OC
                    ],
                )),
            ),
            (
                "carrier",
                Value::Int64(json_i64(&u.value, "carrier-transitions")),
            ),
            ("ts", now.clone()),
        ],
    )
    .context("execute interface upsert")?;

    // Ensure the Device→Interface edge exists
    let mut edge_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (i:Interface {id: $id}) \
         MERGE (d)-[:HAS_INTERFACE]->(i)",
        )
        .context("prepare HAS_INTERFACE merge")?;

    conn.execute(
        &mut edge_stmt,
        vec![
            ("addr", Value::String(u.target.clone())),
            ("id", Value::String(id.clone())),
        ],
    )
    .context("execute HAS_INTERFACE merge")?;

    // Retroactively build CONNECTED_TO for any LldpNeighbor rows that arrived
    // before this Interface node was written (LLDP typically precedes stats).
    let _ = backfill_connected_to(conn, &u.target, if_name);

    debug!(target = %u.target, interface = %if_name, "interface written");
    Ok(())
}

fn write_bgp_neighbor(
    conn: &Connection<'_>,
    u: &TelemetryUpdate,
    peer_addr: &str,
    val: &serde_json::Value,
    event_tx: &broadcast::Sender<BonsaiEvent>,
) -> Result<()> {
    let id = format!("{}:{}", u.target, peer_addr);
    let now = ts(u.timestamp_ns);
    let new_state = json_str(val, "session-state").to_lowercase();

    upsert_device(conn, &u.target, &u.vendor, &u.hostname, now.clone())?;

    // Read current state before upserting so we can detect transitions.
    let old_state = get_bgp_state(conn, &id)?;

    let peer_as = json_i64(val, "peer-as");

    // ON MATCH: only overwrite peer_as when the notification actually carries it
    // (non-zero). ON_CHANGE updates for session-state transitions omit peer-as,
    // which would clobber the stored value with 0.
    let on_match_peer_as = if peer_as != 0 {
        "n.peer_as = $peer_as, "
    } else {
        ""
    };
    let cypher = format!(
        "MERGE (n:BgpNeighbor {{id: $id}}) \
         ON CREATE SET \
           n.device_address = $addr, n.peer_address = $peer, \
           n.peer_as = $peer_as, n.session_state = $state, \
           n.established_transitions = $estab, n.updated_at = $ts \
         ON MATCH SET \
           {on_match_peer_as}n.session_state = $state, \
           n.established_transitions = $estab, n.updated_at = $ts"
    );
    let mut stmt = conn
        .prepare(&cypher)
        .context("prepare BgpNeighbor upsert")?;

    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("addr", Value::String(u.target.clone())),
            ("peer", Value::String(peer_addr.to_string())),
            ("peer_as", Value::Int64(peer_as)),
            ("state", Value::String(new_state.clone())),
            (
                "estab",
                Value::Int64(json_i64(val, "established-transitions")),
            ),
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
        write_state_change_event(
            conn,
            &u.target,
            "bgp_session_change",
            &detail,
            now.clone(),
            u.timestamp_ns,
            event_tx,
        )?;
    }

    // Ensure the Device→BgpNeighbor edge exists
    let mut edge_stmt = conn
        .prepare(
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

fn write_bfd_session(
    conn: &Connection<'_>,
    u: &TelemetryUpdate,
    if_name: &str,
    local_discriminator: &str,
    val: &serde_json::Value,
    event_tx: &broadcast::Sender<BonsaiEvent>,
) -> Result<()> {
    let id = format!("{}:{}:{}", u.target, if_name, local_discriminator);
    let now = ts(u.timestamp_ns);
    let new_state = json_str(val, "session-state").to_lowercase();
    let remote_address = json_str(val, "remote-address").to_string();
    let local_address = json_str(val, "local-address").to_string();

    if new_state.is_empty() {
        return Ok(());
    }

    upsert_device(conn, &u.target, &u.vendor, &u.hostname, now.clone())?;

    let old_state = get_bfd_state(conn, &id)?;

    let mut stmt = conn
        .prepare(
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
        )
        .context("prepare BfdSession upsert")?;

    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("addr", Value::String(u.target.clone())),
            ("if_name", Value::String(if_name.to_string())),
            ("disc", Value::String(local_discriminator.to_string())),
            ("local_addr", Value::String(local_address.clone())),
            ("remote_addr", Value::String(remote_address.clone())),
            ("state", Value::String(new_state.clone())),
            ("ts", now.clone()),
        ],
    )
    .context("execute BfdSession upsert")?;

    if old_state.as_deref() != Some(new_state.as_str()) {
        let detail = format!(
            r#"{{"if_name":"{}","peer":"{}","local_address":"{}","local_discriminator":"{}","old_state":"{}","new_state":"{}"}}"#,
            if_name,
            remote_address,
            local_address,
            local_discriminator,
            old_state.as_deref().unwrap_or("none"),
            new_state
        );
        write_state_change_event(
            conn,
            &u.target,
            "bfd_session_change",
            &detail,
            now.clone(),
            u.timestamp_ns,
            event_tx,
        )?;
    }

    let mut edge_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (b:BfdSession {id: $id}) \
         MERGE (d)-[:HAS_BFD_SESSION]->(b)",
        )
        .context("prepare HAS_BFD_SESSION merge")?;

    conn.execute(
        &mut edge_stmt,
        vec![
            ("addr", Value::String(u.target.clone())),
            ("id", Value::String(id)),
        ],
    )
    .context("execute HAS_BFD_SESSION merge")?;

    info!(
        target = %u.target,
        if_name = %if_name,
        local_discriminator = %local_discriminator,
        remote_address = %remote_address,
        state = %new_state,
        "BFD session written"
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
        if let Value::String(s) = &row[0] {
            Some(s.clone())
        } else {
            None
        }
    }))
}

fn get_bfd_state(conn: &Connection<'_>, id: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("MATCH (b:BfdSession {id: $id}) RETURN b.session_state")
        .context("prepare BFD state lookup")?;
    let mut result = conn
        .execute(&mut stmt, vec![("id", Value::String(id.to_string()))])
        .context("execute BFD state lookup")?;
    Ok(result.next().and_then(|row| {
        if let Value::String(s) = &row[0] {
            Some(s.clone())
        } else {
            None
        }
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

    if event_tx
        .send(BonsaiEvent {
            device_address: device_address.to_string(),
            event_type: event_type.to_string(),
            detail_json: detail.to_string(),
            occurred_at_ns: timestamp_ns,
            state_change_event_id: id.clone(),
        })
        .is_err()
    {
        metrics::counter!("bonsai_broadcast_drops_total").increment(1);
    }

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
            (
                "chassis",
                Value::String(json_str(val, "chassis-id").to_string()),
            ),
            (
                "sysname",
                Value::String(json_str(val, "system-name").to_string()),
            ),
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
    let port_id = json_str(val, "port-id").to_string();
    if !system_name.is_empty()
        && !port_id.is_empty()
        && let Err(e) = try_connect_interfaces(conn, &u.target, local_if, &system_name, &port_id)
    {
        debug!(error = %e, local_if, system_name, port_id, "CONNECTED_TO skipped");
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
    let mut find = conn
        .prepare(
            "MATCH (n:LldpNeighbor {device_address: $addr, local_if: $lif}) \
         RETURN n.system_name, n.port_id",
        )
        .context("prepare lldp lookup for backfill")?;
    let rows = conn
        .execute(
            &mut find,
            vec![
                ("addr", Value::String(local_addr.to_string())),
                ("lif", Value::String(local_if.to_string())),
            ],
        )
        .context("execute lldp lookup for backfill")?;

    for row in rows {
        let system_name = match &row[0] {
            Value::String(s) => s.clone(),
            _ => continue,
        };
        let port_id = match &row[1] {
            Value::String(s) => s.clone(),
            _ => continue,
        };
        if !system_name.is_empty() && !port_id.is_empty() {
            let _ = try_connect_interfaces(conn, local_addr, local_if, &system_name, &port_id);
        }
    }

    // Case 2: Another node's LldpNeighbor points TO this interface as port_id — link inbound.
    let mut find2 = conn
        .prepare(
            "MATCH (n:LldpNeighbor {port_id: $lif}) \
         RETURN n.device_address, n.local_if, n.system_name",
        )
        .context("prepare reverse lldp lookup")?;
    let rows2 = conn
        .execute(
            &mut find2,
            vec![("lif", Value::String(local_if.to_string()))],
        )
        .context("execute reverse lldp lookup")?;

    for row in rows2 {
        let remote_addr = match &row[0] {
            Value::String(s) => s.clone(),
            _ => continue,
        };
        let remote_if = match &row[1] {
            Value::String(s) => s.clone(),
            _ => continue,
        };
        let system_name = match &row[2] {
            Value::String(s) => s.clone(),
            _ => continue,
        };
        // Verify this LldpNeighbor's system_name matches our hostname.
        if system_name.is_empty() {
            continue;
        }
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
        .execute(
            &mut find_stmt,
            vec![("hn", Value::String(remote_hostname.to_string()))],
        )
        .context("execute remote device lookup")?;

    let remote_addr = match result.next() {
        Some(row) => match &row[0] {
            Value::String(s) if !s.is_empty() => s.clone(),
            _ => return Ok(()),
        },
        None => return Ok(()),
    };

    let local_if_id = format!("{}:{}", local_addr, local_if);
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

fn upsert_device(
    conn: &Connection<'_>,
    address: &str,
    vendor: &str,
    hostname: &str,
    now: Value,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "MERGE (d:Device {address: $addr}) \
         ON CREATE SET d.vendor = $vendor, d.hostname = $hn, d.updated_at = $ts \
         ON MATCH SET \
           d.vendor = CASE WHEN $vendor <> '' THEN $vendor ELSE d.vendor END, \
           d.hostname = CASE WHEN $hn <> '' THEN $hn ELSE d.hostname END, \
           d.updated_at = $ts",
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

fn normalize_site(mut site: SiteRecord) -> Result<SiteRecord> {
    site.name = site.name.trim().to_string();
    if site.name.is_empty() {
        anyhow::bail!("site name is required");
    }
    site.id = site.id.trim().to_string();
    if site.id.is_empty() {
        site.id = site_id_from_name(&site.name);
    }
    site.parent_id = site.parent_id.trim().to_string();
    site.kind = site.kind.trim().to_ascii_lowercase();
    if site.kind.is_empty() {
        site.kind = "unknown".to_string();
    }
    if site.metadata_json.trim().is_empty() {
        site.metadata_json = "{}".to_string();
    }
    Ok(site)
}

fn site_id_from_name(name: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in name.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "site".to_string()
    } else {
        slug.to_string()
    }
}

fn upsert_site_record(conn: &Connection<'_>, site: &SiteRecord, now: Value) -> Result<()> {
    validate_site_hierarchy(conn, site)?;

    let mut stmt = conn
        .prepare(
            "MERGE (s:Site {id: $id}) \
         ON CREATE SET \
           s.name = $name, s.parent_id = $parent_id, s.kind = $kind, \
           s.lat = $lat, s.lon = $lon, s.metadata_json = $metadata_json, s.updated_at = $ts \
         ON MATCH SET \
           s.name = $name, s.parent_id = $parent_id, s.kind = $kind, \
           s.lat = $lat, s.lon = $lon, s.metadata_json = $metadata_json, s.updated_at = $ts",
        )
        .context("prepare Site upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(site.id.clone())),
            ("name", Value::String(site.name.clone())),
            ("parent_id", Value::String(site.parent_id.clone())),
            ("kind", Value::String(site.kind.clone())),
            ("lat", Value::Double(site.lat)),
            ("lon", Value::Double(site.lon)),
            ("metadata_json", Value::String(site.metadata_json.clone())),
            ("ts", now),
        ],
    )
    .context("execute Site upsert")?;

    let mut clear_parent = conn
        .prepare("MATCH (:Site)-[r:PARENT_OF]->(s:Site {id: $id}) DELETE r")
        .context("prepare PARENT_OF clear")?;
    conn.execute(
        &mut clear_parent,
        vec![("id", Value::String(site.id.clone()))],
    )
    .context("execute PARENT_OF clear")?;

    if !site.parent_id.is_empty() && site.parent_id != site.id {
        let mut parent_edge = conn
            .prepare(
                "MATCH (p:Site {id: $parent_id}), (s:Site {id: $id}) \
             MERGE (p)-[:PARENT_OF]->(s)",
            )
            .context("prepare PARENT_OF edge")?;
        conn.execute(
            &mut parent_edge,
            vec![
                ("parent_id", Value::String(site.parent_id.clone())),
                ("id", Value::String(site.id.clone())),
            ],
        )
        .context("execute PARENT_OF edge")?;
    }

    Ok(())
}

fn validate_site_hierarchy(conn: &Connection<'_>, site: &SiteRecord) -> Result<()> {
    if site.parent_id.is_empty() {
        return Ok(());
    }
    if site.parent_id == site.id {
        anyhow::bail!("site parent_id cannot reference itself");
    }

    let mut seen = HashSet::from([site.id.clone()]);
    let mut current = site.parent_id.clone();
    let mut depth = 0usize;

    while !current.is_empty() {
        if !seen.insert(current.clone()) {
            anyhow::bail!("site hierarchy contains a cycle at '{current}'");
        }
        depth += 1;
        if depth > MAX_SITE_HIERARCHY_DEPTH {
            anyhow::bail!("site hierarchy depth exceeds {MAX_SITE_HIERARCHY_DEPTH}");
        }

        let Some(parent_id) = read_site_parent_id(conn, &current)? else {
            break;
        };
        current = parent_id;
    }

    Ok(())
}

fn read_site_parent_id(conn: &Connection<'_>, site_id: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("MATCH (s:Site {id: $id}) RETURN s.parent_id")
        .context("prepare Site parent lookup")?;
    let rows = conn
        .execute(&mut stmt, vec![("id", Value::String(site_id.to_string()))])
        .context("execute Site parent lookup")?
        .collect::<Vec<_>>();
    Ok(rows.first().map(|row| read_str(&row[0])))
}

fn link_device_to_site(conn: &Connection<'_>, device_address: &str, site_id: &str) -> Result<()> {
    let mut clear = conn
        .prepare("MATCH (d:Device {address: $addr})-[r:LOCATED_AT]->(:Site) DELETE r")
        .context("prepare LOCATED_AT clear")?;
    conn.execute(
        &mut clear,
        vec![("addr", Value::String(device_address.to_string()))],
    )
    .context("execute LOCATED_AT clear")?;

    let mut link = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (s:Site {id: $site_id}) \
         MERGE (d)-[:LOCATED_AT]->(s)",
        )
        .context("prepare LOCATED_AT edge")?;
    conn.execute(
        &mut link,
        vec![
            ("addr", Value::String(device_address.to_string())),
            ("site_id", Value::String(site_id.to_string())),
        ],
    )
    .context("execute LOCATED_AT edge")?;
    Ok(())
}

fn site_from_row(row: Vec<Value>) -> SiteRecord {
    SiteRecord {
        id: read_str(&row[0]),
        name: read_str(&row[1]),
        parent_id: read_str(&row[2]),
        kind: read_str(&row[3]),
        lat: read_f64(&row[4]),
        lon: read_f64(&row[5]),
        metadata_json: read_str(&row[6]),
    }
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
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
        device_address: u.target.clone(),
        event_type: "interface_oper_status_change".to_string(),
        detail_json: detail,
        occurred_at_ns: u.timestamp_ns,
        state_change_event_id: String::new(),
    });
    // Best-effort: ensure Device node exists so graph queries stay consistent.
    upsert_device(conn, &u.target, &u.vendor, &u.hostname, ts(u.timestamp_ns))?;
    debug!(target = %u.target, if_name, oper_status, "interface oper-status event emitted");
    Ok(())
}

// ── diagnostic query (callable from main after startup) ──────────────────────

pub fn log_graph_summary(db: &Database) {
    let Ok(conn) = Connection::new(db) else {
        return;
    };
    for (label, q) in [
        ("devices", "MATCH (n:Device) RETURN count(n)"),
        ("interfaces", "MATCH (n:Interface) RETURN count(n)"),
        ("bgp-neighbors", "MATCH (n:BgpNeighbor) RETURN count(n)"),
        ("bfd-sessions", "MATCH (n:BfdSession) RETURN count(n)"),
        ("lldp-neighbors", "MATCH (n:LldpNeighbor) RETURN count(n)"),
        (
            "connected-to",
            "MATCH ()-[r:CONNECTED_TO]->() RETURN count(r)",
        ),
        (
            "state-change-events",
            "MATCH (n:StateChangeEvent) RETURN count(n)",
        ),
        (
            "detection-events",
            "MATCH (n:DetectionEvent) RETURN count(n)",
        ),
        ("remediations", "MATCH (n:Remediation) RETURN count(n)"),
        (
            "remediation-trust-marks",
            "MATCH (n:RemediationTrustMark) RETURN count(n)",
        ),
        (
            "subscription-status",
            "MATCH (n:SubscriptionStatus) RETURN count(n)",
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_graph_path(label: &str) -> String {
        std::env::temp_dir()
            .join(format!("bonsai-{}-{}", label, Uuid::new_v4()))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn backfill_remediation_trust_marks_marks_legacy_rows() {
        let path = temp_graph_path("trust-backfill");
        let store = GraphStore::open(&path).expect("open graph store");
        let conn = Connection::new(&store.db).expect("graph connection");

        let mut old_stmt = conn
            .prepare(
                "CREATE (r:Remediation {\
                    id: $id, detection_id: $did, action: $action, status: $status, \
                    detail_json: $detail, attempted_at: $att, completed_at: $comp})",
            )
            .expect("prepare old remediation");
        conn.execute(
            &mut old_stmt,
            vec![
                ("id", Value::String("legacy-old".to_string())),
                ("did", Value::String("det-1".to_string())),
                ("action", Value::String("log_only".to_string())),
                ("status", Value::String("success".to_string())),
                ("detail", Value::String("{}".to_string())),
                ("att", ts(REMEDIATION_TRUST_CUTOFF_NS - 1)),
                ("comp", ts(REMEDIATION_TRUST_CUTOFF_NS - 1)),
            ],
        )
        .expect("insert old remediation");

        let mut new_stmt = conn
            .prepare(
                "CREATE (r:Remediation {\
                    id: $id, detection_id: $did, action: $action, status: $status, \
                    detail_json: $detail, attempted_at: $att, completed_at: $comp})",
            )
            .expect("prepare new remediation");
        conn.execute(
            &mut new_stmt,
            vec![
                ("id", Value::String("legacy-new".to_string())),
                ("did", Value::String("det-2".to_string())),
                ("action", Value::String("log_only".to_string())),
                ("status", Value::String("success".to_string())),
                ("detail", Value::String("{}".to_string())),
                ("att", ts(REMEDIATION_TRUST_CUTOFF_NS + 1)),
                ("comp", ts(REMEDIATION_TRUST_CUTOFF_NS + 1)),
            ],
        )
        .expect("insert new remediation");

        store
            .backfill_remediation_trust_marks()
            .expect("backfill trust marks");

        let mut query = conn
            .prepare(
                "MATCH (m:RemediationTrustMark) \
                 RETURN m.remediation_id, m.trustworthy, m.reason \
                 ORDER BY m.remediation_id",
            )
            .expect("prepare trust-mark query");
        let rows = conn
            .execute(&mut query, Vec::new())
            .expect("query trust marks")
            .collect::<Vec<_>>();

        assert_eq!(rows.len(), 2);
        assert_eq!(read_str(&rows[0][0]), "legacy-new");
        assert_eq!(read_i64(&rows[0][1]), 1);
        assert_eq!(read_str(&rows[0][2]), REMEDIATION_TRUST_REASON_POST_CUTOFF);
        assert_eq!(read_str(&rows[1][0]), "legacy-old");
        assert_eq!(read_i64(&rows[1][1]), 0);
        assert_eq!(read_str(&rows[1][2]), REMEDIATION_TRUST_REASON_PRE_CUTOFF);
    }

    #[tokio::test]
    async fn subscription_status_write_preserves_device_metadata() {
        let path = temp_graph_path("subscription-status");
        let store = GraphStore::open(&path).expect("open graph store");
        let conn = Connection::new(&store.db).expect("graph connection");

        upsert_device(&conn, "dut:57400", "nokia_srl", "dut1", ts(1_000_000_000))
            .expect("seed device");

        store
            .write_subscription_status(SubscriptionStatusWrite {
                device_address: "dut:57400".to_string(),
                path: "interface[name=*]/statistics".to_string(),
                origin: String::new(),
                mode: "SAMPLE".to_string(),
                sample_interval_ns: 10_000_000_000,
                status: "subscribed_but_silent".to_string(),
                first_observed_at_ns: 0,
                last_observed_at_ns: 0,
                updated_at_ns: 2_000_000_000,
            })
            .await
            .expect("write subscription status");

        let mut status_query = conn
            .prepare(
                "MATCH (d:Device {address: $addr})-[:HAS_SUBSCRIPTION_STATUS]->(s:SubscriptionStatus) \
                 RETURN d.vendor, d.hostname, s.path, s.status",
            )
            .expect("prepare status query");
        let rows = conn
            .execute(
                &mut status_query,
                vec![("addr", Value::String("dut:57400".to_string()))],
            )
            .expect("query status")
            .collect::<Vec<_>>();

        assert_eq!(rows.len(), 1);
        assert_eq!(read_str(&rows[0][0]), "nokia_srl");
        assert_eq!(read_str(&rows[0][1]), "dut1");
        assert_eq!(read_str(&rows[0][2]), "interface[name=*]/statistics");
        assert_eq!(read_str(&rows[0][3]), "subscribed_but_silent");
    }

    #[tokio::test]
    async fn site_sync_creates_site_and_located_at_edge() {
        let path = temp_graph_path("site-sync");
        let store = GraphStore::open(&path).expect("open graph store");

        store
            .sync_sites_from_targets(vec![TargetConfig {
                address: "dut:57400".to_string(),
                enabled: true,
                tls_domain: None,
                ca_cert: None,
                vendor: Some("nokia_srl".to_string()),
                credential_alias: None,
                username_env: None,
                password_env: None,
                username: None,
                password: None,
                hostname: Some("dut1".to_string()),
                role: Some("leaf".to_string()),
                site: Some("lab-london".to_string()),
                selected_paths: Vec::new(),
            }])
            .await
            .expect("sync sites");

        let conn = Connection::new(&store.db).expect("graph connection");
        let mut site_query = conn
            .prepare(
                "MATCH (d:Device {address: $addr})-[:LOCATED_AT]->(s:Site) \
                 RETURN d.hostname, s.id, s.name, s.kind",
            )
            .expect("prepare site query");
        let rows = conn
            .execute(
                &mut site_query,
                vec![("addr", Value::String("dut:57400".to_string()))],
            )
            .expect("query site edge")
            .collect::<Vec<_>>();

        assert_eq!(rows.len(), 1);
        assert_eq!(read_str(&rows[0][0]), "dut1");
        assert_eq!(read_str(&rows[0][1]), "lab-london");
        assert_eq!(read_str(&rows[0][2]), "lab-london");
        assert_eq!(read_str(&rows[0][3]), "unknown");
    }

    #[tokio::test]
    async fn site_upsert_rejects_self_parent_and_cycles() {
        let path = temp_graph_path("site-cycle");
        let store = GraphStore::open(&path).expect("open graph store");

        let self_parent = store.upsert_site(test_site("lab", "lab")).await;
        assert!(
            self_parent
                .expect_err("self parent should fail")
                .to_string()
                .contains("parent_id cannot reference itself")
        );

        store
            .upsert_site(test_site("region", ""))
            .await
            .expect("insert region");
        store
            .upsert_site(test_site("dc", "region"))
            .await
            .expect("insert dc");
        let cycle = store.upsert_site(test_site("region", "dc")).await;
        assert!(
            cycle
                .expect_err("cycle should fail")
                .to_string()
                .contains("site hierarchy contains a cycle")
        );
    }

    #[tokio::test]
    async fn site_upsert_rejects_parent_chain_deeper_than_ten() {
        let path = temp_graph_path("site-depth");
        let store = GraphStore::open(&path).expect("open graph store");

        store
            .upsert_site(test_site("site-0", ""))
            .await
            .expect("insert root");
        for index in 1..=10 {
            store
                .upsert_site(test_site(
                    &format!("site-{index}"),
                    &format!("site-{}", index - 1),
                ))
                .await
                .expect("insert allowed depth");
        }

        let too_deep = store.upsert_site(test_site("site-11", "site-10")).await;
        assert!(
            too_deep
                .expect_err("deep chain should fail")
                .to_string()
                .contains("site hierarchy depth exceeds 10")
        );
    }

    fn test_site(id: &str, parent_id: &str) -> SiteRecord {
        SiteRecord {
            id: id.to_string(),
            name: id.to_string(),
            parent_id: parent_id.to_string(),
            kind: "dc".to_string(),
            lat: 0.0,
            lon: 0.0,
            metadata_json: "{}".to_string(),
        }
    }
}
