pub mod algorithms;
pub mod common;
pub mod explorer;
pub mod queries;
#[cfg(test)]
pub mod test_fixtures;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use lbug::{Connection, Database, SystemConfig, Value};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};
use uuid::Uuid;

use self::common::{
    link_device_address, link_host_endpoint_to_interface, now_ns, read_str, read_ts_ns,
    resolve_peer_to_device, ts,
    upsert_app_flow, upsert_device, upsert_device_address, upsert_device_with_endpoint,
    upsert_host_endpoint,
};
use crate::config::TargetConfig;
use crate::correlation_buffer::{CorrelationBuffer, CorrelationKey, semantic_key_for_event};
use crate::signals::syslog::SyslogFact;
use crate::signals::snmp::SnmpFact;
use crate::store::BonsaiStore;
use crate::streaming::bgp_ls::BgpLsEvent;
use crate::streaming::bmp::BmpEvent;
use crate::telemetry::{TelemetryEvent, TelemetryUpdate, json_f64, json_i64, json_i64_multi, json_str};

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
    /// Signal origin: "gnmi" | "syslog" | "snmp" | "netflow" | "otlp" | "bmp" | "bgp_ls" | "detection" | "registry"
    pub source_type: String,
}

/// A detection + its linked remediation (if any). Used by the HTTP topology API.
#[derive(Debug, Clone, Serialize)]
pub struct DetectionRow {
    pub id: String,
    pub device_address: String,
    pub rule_id: String,
    pub severity: String,
    pub features_json: String,
    pub source_types: Vec<String>,
    pub latency_ns: i64,
    pub fired_at_ns: i64,
    pub remediation_id: String,
    pub remediation_action: String,
    pub remediation_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemediationProposalRow {
    pub id: String,
    pub detection_id: String,
    pub device_address: String,
    pub rule_id: String,
    pub severity: String,
    pub playbook_id: String,
    pub trust_key: String,
    pub status: String,
    pub operator_note: String,
    pub steps_json: String,
    pub rollback_steps_json: String,
    pub proposed_at_ns: i64,
    pub decided_at_ns: i64,
    /// Raw features JSON from the linked DetectionEvent (empty string when no match).
    pub features_json: String,
}

/// A persisted StateChangeEvent node returned by the history query.
#[derive(Debug, Clone, Serialize)]
pub struct StateChangeEventRow {
    pub id: String,
    pub device_address: String,
    pub event_type: String,
    pub source_type: String,
    pub detail_json: String,
    pub occurred_at_ns: i64,
}

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

#[derive(Debug, Clone, Serialize)]
pub struct SavedQueryRecord {
    pub id: String,
    pub name: String,
    pub description: String,
    pub cypher: String,
    pub created_at_ns: i64,
    pub last_run_at_ns: i64,
    pub last_result_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvestigationRecord {
    pub id: String,
    pub detection_id: String,
    pub device_address: String,
    pub trigger: String, // "auto" | "operator"
    pub status: String,  // "running" | "complete" | "failed"
    pub summary: String,
    pub proposal_json: String, // serialised RemediationProposal request, or ""
    pub tokens_used: i64,
    pub cost_usd: f64,
    pub started_at_ns: i64,
    pub completed_at_ns: i64, // 0 while running
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: String,
    pub investigation_id: String,
    pub tool_name: String,
    pub input_json: String,
    pub output_json: String,
    pub called_at_ns: i64,
}

/// D4-8 T2: Operator feedback on investigation quality.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackRecord {
    pub id: String,
    pub investigation_id: String,
    pub rating: String, // "positive" | "negative"
    pub comment: String,
    pub operator: String,
    pub created_at_ns: i64,
}

/// D4-8 T2: Aggregate accuracy stats for the investigations/accuracy endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct InvestigationAccuracy {
    pub total_investigations: usize,
    pub total_feedback: usize,
    pub positive: usize,
    pub negative: usize,
    pub precision_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRecord {
    pub device_address: String,
    pub version: String,
    pub algorithm: String,
    pub dimension: i64,
    /// Dense embedding vector serialised as a JSON array.
    pub vector: Vec<f64>,
    pub computed_at_ns: i64,
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
    pub environment_id: String,
}

/// Archetype values for an Environment node.
pub const ARCHETYPE_DATA_CENTER: &str = "data_center";
pub const ARCHETYPE_CAMPUS_WIRED: &str = "campus_wired";
pub const ARCHETYPE_CAMPUS_WIRELESS: &str = "campus_wireless";
pub const ARCHETYPE_SERVICE_PROVIDER: &str = "service_provider";
pub const ARCHETYPE_HOME_LAB: &str = "home_lab";

pub const DEFAULT_ENVIRONMENT_ID: &str = "migrated-default";
pub const DEFAULT_ENVIRONMENT_NAME: &str = "Default (Migrated)";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EnvironmentRecord {
    pub id: String,
    pub name: String,
    pub archetype: String,
    pub created_at_ns: i64,
    pub metadata_json: String,
}

/// D4-7 T1: Config item stored in the graph DB (replaces on-disk YAML config files).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigItemRecord {
    pub id: String,
    pub config_class: String,
    pub vendor: String,
    pub name: String,
    pub version: String,
    pub content_json: String,
    pub enabled: bool,
    pub created_by: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentWithCounts {
    pub id: String,
    pub name: String,
    pub archetype: String,
    pub created_at_ns: i64,
    pub metadata_json: String,
    pub site_count: i64,
    pub device_count: i64,
}

pub struct GraphStore {
    db: Arc<Database>,
    event_tx: broadcast::Sender<BonsaiEvent>,
    /// KuzuDB permits only one concurrent write transaction. All spawn_blocking
    /// write paths must hold this lock for the duration of their Connection.
    write_lock: Arc<Mutex<()>>,
    /// Configured LadybugDB buffer pool cap — exposed for memory health reporting.
    buffer_pool_bytes: u64,
    /// Late-arrival multi-source correlation buffer. State-change events from
    /// BMP, gNMI, syslog and SNMP that describe the same physical event are
    /// absorbed into a single slot and flushed as a fused detection after the
    /// window expires.
    pub correlation_buffer: Arc<CorrelationBuffer>,
}

impl GraphStore {
    pub fn open(path: &str, buffer_pool_bytes: u64) -> Result<Self> {
        let sysconfig = SystemConfig::default()
            .buffer_pool_size(buffer_pool_bytes)
            .max_num_threads(2)
            .checkpoint_threshold(2 * 1024 * 1024); // 2 MiB: checkpoint aggressively to free buffer pool pages
        let db = Database::new(path, sysconfig).context("failed to open LadybugDB")?;
        info!(
            path,
            buffer_pool_mib = buffer_pool_bytes / 1024 / 1024,
            "LadybugDB opened"
        );
        let (event_tx, _) = broadcast::channel(1024);
        let store = GraphStore {
            db: Arc::new(db),
            event_tx,
            write_lock: Arc::new(Mutex::new(())),
            buffer_pool_bytes,
            correlation_buffer: Arc::new(CorrelationBuffer::new(45)),
        };

        let t = Instant::now();
        store.init_schema()?;
        info!(
            phase = "schema_init",
            elapsed_ms = t.elapsed().as_millis() as u64,
            "startup"
        );

        let t = Instant::now();
        store.backfill_remediation_trust_marks()?;
        info!(
            phase = "backfill",
            elapsed_ms = t.elapsed().as_millis() as u64,
            "startup"
        );

        info!(path, "graph store opened");
        Ok(store)
    }

    /// Subscribe to state-change events broadcast by the graph writer.
    pub fn subscribe_events(&self) -> broadcast::Receiver<BonsaiEvent> {
        self.event_tx.subscribe()
    }

    /// Clone the event sender so other components can publish workspace events
    /// onto the same SSE channel (e.g. CollectorManager for status changes).
    pub fn event_sender(&self) -> broadcast::Sender<BonsaiEvent> {
        self.event_tx.clone()
    }

    /// Configured LadybugDB buffer pool cap in bytes (for memory health reporting).
    pub fn buffer_pool_bytes(&self) -> u64 {
        self.buffer_pool_bytes
    }
}

#[tonic::async_trait]
impl BonsaiStore for GraphStore {
    fn db(&self) -> Arc<Database> {
        Arc::clone(&self.db)
    }

    fn write_lock(&self) -> Arc<std::sync::Mutex<()>> {
        Arc::clone(&self.write_lock)
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
        source_types_json: String,
        latency_ns: i64,
        fired_at_ns: i64,
        state_change_event_id: String,
        source_event_ids: Vec<String>,
    ) -> Result<String> {
        self.write_detection(
            device_address,
            rule_id,
            severity,
            features_json,
            source_types_json,
            latency_ns,
            fired_at_ns,
            state_change_event_id,
            source_event_ids,
        )
        .await
    }

    async fn write_remediation(
        &self,
        detection_id: String,
        action: String,
        status: String,
        detail_json: String,
        attempted_at_ns: i64,
        completed_at_ns: i64,
    ) -> Result<String> {
        self.write_remediation(
            detection_id,
            action,
            status,
            detail_json,
            attempted_at_ns,
            completed_at_ns,
        )
        .await
    }

    async fn sync_sites_from_targets(
        &self,
        targets: Vec<crate::config::TargetConfig>,
    ) -> Result<()> {
        self.sync_sites_from_targets(targets).await
    }

    async fn list_sites(&self) -> Result<Vec<SiteRecord>> {
        self.list_sites().await
    }

    async fn upsert_site(&self, site: SiteRecord) -> Result<SiteRecord> {
        self.upsert_site(site).await
    }

    async fn write_subscription_status(&self, status: SubscriptionStatusWrite) -> Result<()> {
        self.write_subscription_status(status).await
    }

    fn publish_event(&self, event: BonsaiEvent) {
        self.publish_event(event)
    }
}

impl GraphStore {
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
                role       STRING,\
                site       STRING,\
                updated_at TIMESTAMP_NS,\
                PRIMARY KEY (address))",
        )
        .context("create Device table")?;

        // Migration: add role/site to Device table for DBs created before Sprint 2.
        // ALTER TABLE ADD is idempotent in lbug (ignored if column already exists).
        let _ = conn.query("ALTER TABLE Device ADD role STRING DEFAULT ''");
        let _ = conn.query("ALTER TABLE Device ADD site STRING DEFAULT ''");
        // D4-11 T4: BMP Initiation TLV identity columns
        let _ = conn.query("ALTER TABLE Device ADD bmp_sys_name STRING DEFAULT ''");
        let _ = conn.query("ALTER TABLE Device ADD bmp_sys_descr STRING DEFAULT ''");
        let _ = conn.query("ALTER TABLE Device ADD bmp_admin_string STRING DEFAULT ''");
        // Device identity refactor: address is now the bare IP (no port).
        // gnmi_endpoint stores the original host:port used for gNMI collection.
        let _ = conn.query("ALTER TABLE Device ADD gnmi_endpoint STRING DEFAULT ''");

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
                oper_status         STRING,\
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

        // Device-to-Device topology relationships — resolved from protocol peer IPs via
        // the DeviceAddress / EntityIdentity lookup chain. These power the topology UI
        // L3/L2 layer views and path-finding across all routing/switching protocols.
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS BGP_SESSION_WITH(\
                FROM Device TO Device, \
                session_state STRING, \
                peer_as INT64, \
                source STRING)",
        )
        .context("create BGP_SESSION_WITH rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS BFD_PEER_WITH(\
                FROM Device TO Device, \
                session_state STRING, \
                source STRING)",
        )
        .context("create BFD_PEER_WITH rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS LLDP_NEIGHBOR(\
                FROM Device TO Device, \
                local_if STRING, \
                remote_if STRING, \
                source STRING)",
        )
        .context("create LLDP_NEIGHBOR rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS ISIS_NEIGHBOR_WITH(\
                FROM Device TO Device, \
                adjacency_state STRING, \
                if_name STRING, \
                source STRING)",
        )
        .context("create ISIS_NEIGHBOR_WITH rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS MONITORS_BGP_ROUTER(\
                FROM Device TO Device, \
                source STRING)",
        )
        .context("create MONITORS_BGP_ROUTER rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS IsisAdjacency(\
                id               STRING,\
                device_address   STRING,\
                system_id        STRING,\
                if_name          STRING,\
                neighbor_id      STRING,\
                adjacency_state  STRING,\
                source_type      STRING,\
                updated_at       TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create IsisAdjacency table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_ISIS_ADJACENCY(FROM Device TO IsisAdjacency)",
        )
        .context("create HAS_ISIS_ADJACENCY rel")?;

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

        // EntityIdentity: canonical identity record linking a device's hostname,
        // chassis-id, and management IP. Prevents duplicate Device nodes when the
        // same physical device is seen via gNMI (by IP) and LLDP (by chassis-id).
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS EntityIdentity(\
                id         STRING,\
                hostname   STRING,\
                chassis_id STRING,\
                mgmt_ip    STRING,\
                source     STRING,\
                updated_at TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create EntityIdentity table")?;
        conn.query("CREATE REL TABLE IF NOT EXISTS HAS_IDENTITY(FROM Device TO EntityIdentity)")
            .context("create HAS_IDENTITY rel")?;

        // DeviceAddress: one node per IP/hostname known for a physical device.
        // A device may have a loopback (used for gNMI), a management IP (syslog/SNMP
        // source), and secondary data-plane IPs. All resolve back to the same Device.
        // Source: "gnmi" (primary), "extra_ip" (from TargetConfig.extra_ips or CLI),
        //         "bmp_peer" (BMP TCP source IP), "lldp" (LLDP mgmt-addr TLV),
        //         "netflow" (exporter IP), "sflow", "snmp", "syslog_peer".
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS DeviceAddress(\
                ip         STRING,\
                source     STRING,\
                updated_at TIMESTAMP_NS,\
                PRIMARY KEY (ip))",
        )
        .context("create DeviceAddress table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS KNOWN_ADDRESS_OF(FROM DeviceAddress TO Device)",
        )
        .context("create KNOWN_ADDRESS_OF rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS StateChangeEvent(\
                id             STRING,\
                device_address STRING,\
                event_type     STRING,\
                detail         STRING,\
                source_type    STRING,\
                occurred_at    TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create StateChangeEvent table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS REPORTED_BY(FROM Device TO StateChangeEvent)")
            .context("create REPORTED_BY rel")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS CONNECTED_TO(FROM Interface TO Interface)")
            .context("create CONNECTED_TO rel")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS MGMT_LINK(FROM Interface TO Interface)")
            .context("create MGMT_LINK rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS DetectionEvent(\
                id             STRING,\
                device_address STRING,\
                rule_id        STRING,\
                severity       STRING,\
                features_json  STRING,\
                source_types   STRING,\
                latency_ns     INT64,\
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

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS Environment(\
                id            STRING,\
                name          STRING,\
                archetype     STRING,\
                created_at    TIMESTAMP_NS,\
                metadata_json STRING,\
                PRIMARY KEY (id))",
        )
        .context("create Environment table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS BELONGS_TO_ENVIRONMENT(FROM Site TO Environment)",
        )
        .context("create BELONGS_TO_ENVIRONMENT rel")?;

        // ── Enrichment schema ─────────────────────────────────────────────────

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS EnrichmentProperty(\
                id           STRING,\
                device_address STRING,\
                key          STRING,\
                value        STRING,\
                source_name  STRING,\
                updated_at   TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create EnrichmentProperty table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_ENRICHMENT_PROPERTY(FROM Device TO EnrichmentProperty)",
        )
        .context("create HAS_ENRICHMENT_PROPERTY rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS VLAN(\
                id           STRING,\
                vid          INT64,\
                name         STRING,\
                source_name  STRING,\
                updated_at   TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create VLAN table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS ACCESS_VLAN(FROM Interface TO VLAN)")
            .context("create ACCESS_VLAN rel")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS TRUNK_VLAN(FROM Interface TO VLAN)")
            .context("create TRUNK_VLAN rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS Prefix(\
                id           STRING,\
                cidr         STRING,\
                prefix_role  STRING,\
                descr        STRING,\
                source_name  STRING,\
                updated_at   TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create Prefix table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS HAS_PREFIX(FROM Device TO Prefix)")
            .context("create HAS_PREFIX rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS ConfigSnapshot(\
                id                 STRING,\
                device_address     STRING,\
                source             STRING,\
                trigger            STRING,\
                reason             STRING,\
                requested_at       TIMESTAMP_NS,\
                snapshot_hash      STRING,\
                stored_path        STRING,\
                bytes_len          INT64,\
                captured_at        TIMESTAMP_NS,\
                summary            STRING,\
                changed            BOOL,\
                PRIMARY KEY (id))",
        )
        .context("create ConfigSnapshot table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_CONFIG_SNAPSHOT(FROM Device TO ConfigSnapshot)",
        )
        .context("create HAS_CONFIG_SNAPSHOT rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS ConfigChange(\
                id                   STRING,\
                device_address       STRING,\
                source               STRING,\
                trigger              STRING,\
                previous_snapshot_id STRING,\
                current_snapshot_id  STRING,\
                previous_hash        STRING,\
                current_hash         STRING,\
                summary              STRING,\
                added_lines          INT64,\
                removed_lines        INT64,\
                changed_at           TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create ConfigChange table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS HAS_CONFIG_CHANGE(FROM Device TO ConfigChange)")
            .context("create HAS_CONFIG_CHANGE rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS PropertyProvenance(\
                id           STRING,\
                owner_kind   STRING,\
                owner_id     STRING,\
                source       STRING,\
                parser       STRING,\
                confidence   STRING,\
                captured_at  TIMESTAMP_NS,\
                details_json STRING,\
                PRIMARY KEY (id))",
        )
        .context("create PropertyProvenance table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS CONFIG_SNAPSHOT_PROVENANCE(FROM ConfigSnapshot TO PropertyProvenance)",
        )
        .context("create CONFIG_SNAPSHOT_PROVENANCE rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS CONFIG_CHANGE_PROVENANCE(FROM ConfigChange TO PropertyProvenance)",
        )
        .context("create CONFIG_CHANGE_PROVENANCE rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS ENRICHMENT_PROPERTY_PROVENANCE(FROM EnrichmentProperty TO PropertyProvenance)",
        )
        .context("create ENRICHMENT_PROPERTY_PROVENANCE rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS GnmiReadiness(\
                id                  STRING,\
                device_address      STRING,\
                service_status      STRING,\
                tls_status          STRING,\
                encoding_support    STRING,\
                models_advertised   STRING,\
                known_issues        STRING,\
                blockers            STRING,\
                recommended_actions STRING,\
                checked_at          TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create GnmiReadiness table")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS StreamingReadiness(\
                id                         STRING,\
                device_address             STRING,\
                vendor                     STRING,\
                role                       STRING,\
                protocols_json             STRING,\
                recommended_protocols_json STRING,\
                checked_at                 TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create StreamingReadiness table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_STREAMING_READINESS(FROM Device TO StreamingReadiness)",
        )
        .context("create HAS_STREAMING_READINESS rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS BmpSession(\
                id                STRING,\
                device_address    STRING,\
                router_address    STRING,\
                peer_address      STRING,\
                peer_as           INT64,\
                peer_bgp_id       STRING,\
                session_state     STRING,\
                last_message_type STRING,\
                updated_at        TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create BmpSession table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS HAS_BMP_SESSION(FROM Device TO BmpSession)")
            .context("create HAS_BMP_SESSION rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS BgpRibEntry(\
                id               STRING,\
                device_address   STRING,\
                peer_address     STRING,\
                rib_type         STRING,\
                afi_safi         STRING,\
                prefix           STRING,\
                prefix_len       INT64,\
                action           STRING,\
                next_hop         STRING,\
                as_path_json     STRING,\
                communities_json STRING,\
                med              INT64,\
                local_pref       INT64,\
                updated_at       TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create BgpRibEntry table")?;

        // Migration: BMP route-monitoring writes now persist rib_type. Older DBs
        // need the column added in place so startup can continue using the same graph.
        let _ = conn.query("ALTER TABLE BgpRibEntry ADD rib_type STRING DEFAULT ''");

        conn.query("CREATE REL TABLE IF NOT EXISTS HAS_RIB_ENTRY(FROM Device TO BgpRibEntry)")
            .context("create HAS_RIB_ENTRY rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS BgpLsNode(\
                id            STRING,\
                device_address STRING,\
                router_id     STRING,\
                protocol      STRING,\
                asn           INT64,\
                name          STRING,\
                sr_node_sid   INT64,\
                updated_at    TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create BgpLsNode table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS HAS_BGPLS_NODE(FROM Device TO BgpLsNode)")
            .context("create HAS_BGPLS_NODE rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS BgpLsLink(\
                id                         STRING,\
                device_address             STRING,\
                local_router_id            STRING,\
                remote_router_id           STRING,\
                protocol                   STRING,\
                local_interface            STRING,\
                remote_interface           STRING,\
                igp_metric                 INT64,\
                te_metric                  INT64,\
                unreserved_bandwidth_bps   INT64,\
                admin_groups_json          STRING,\
                srlgs_json                 STRING,\
                updated_at                 TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create BgpLsLink table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS HAS_BGPLS_LINK(FROM Device TO BgpLsLink)")
            .context("create HAS_BGPLS_LINK rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS SrPolicy(\
                id            STRING,\
                device_address STRING,\
                name          STRING,\
                endpoint      STRING,\
                color         INT64,\
                preference    INT64,\
                binding_sid   INT64,\
                status        STRING,\
                updated_at    TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create SrPolicy table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS HAS_SR_POLICY(FROM Device TO SrPolicy)")
            .context("create HAS_SR_POLICY rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_GNMI_READINESS(FROM Device TO GnmiReadiness)",
        )
        .context("create HAS_GNMI_READINESS rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS Application(\
                id           STRING,\
                name         STRING,\
                criticality  STRING,\
                owner_group  STRING,\
                source_name  STRING,\
                updated_at   TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create Application table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS RUNS_SERVICE(FROM Device TO Application)")
            .context("create RUNS_SERVICE rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS CARRIES_APPLICATION(FROM Device TO Application)",
        )
        .context("create CARRIES_APPLICATION rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS Incident(\
                id               STRING,\
                snow_sys_id      STRING,\
                state            STRING,\
                assignment_group STRING,\
                opened_at_ns     INT64,\
                detection_id     STRING,\
                updated_at       TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create Incident table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS HAS_INCIDENT(FROM DetectionEvent TO Incident)")
            .context("create HAS_INCIDENT rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS RemediationProposal(\
                id                  STRING,\
                detection_id        STRING,\
                playbook_id         STRING,\
                trust_key           STRING,\
                status              STRING,\
                operator_note       STRING,\
                steps_json          STRING,\
                rollback_steps_json STRING,\
                proposed_at         TIMESTAMP_NS,\
                decided_at          TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create RemediationProposal table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_PROPOSAL(FROM DetectionEvent TO RemediationProposal)",
        )
        .context("create HAS_PROPOSAL rel")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS MigrationMarker(\
                id         STRING,\
                applied_at TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create MigrationMarker table")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS SavedQuery(\
                id                STRING,\
                name              STRING,\
                description       STRING,\
                cypher            STRING,\
                created_at        TIMESTAMP_NS,\
                last_run_at       TIMESTAMP_NS,\
                last_result_count INT64,\
                PRIMARY KEY (id))",
        )
        .context("create SavedQuery table")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS Investigation(\
                id              STRING,\
                detection_id    STRING,\
                device_address  STRING,\
                trigger         STRING,\
                status          STRING,\
                summary         STRING,\
                proposal_json   STRING,\
                tokens_used     INT64,\
                cost_usd        DOUBLE,\
                started_at      TIMESTAMP_NS,\
                completed_at    TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create Investigation table")?;

        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS AgentToolCall(\
                id               STRING,\
                investigation_id STRING,\
                tool_name        STRING,\
                input_json       STRING,\
                output_json      STRING,\
                called_at        TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create AgentToolCall table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_TOOL_CALL(\
                FROM Investigation TO AgentToolCall)",
        )
        .context("create HAS_TOOL_CALL rel table")?;

        // D4-8 T2: Operator feedback on investigation results (thumbs up/down + comment).
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS InvestigationFeedback(\
                id                STRING,\
                investigation_id  STRING,\
                rating            STRING,\
                comment           STRING,\
                operator          STRING,\
                created_at        TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create InvestigationFeedback table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_FEEDBACK(\
                FROM Investigation TO InvestigationFeedback)",
        )
        .context("create HAS_FEEDBACK rel table")?;

        // id = "{device_address}:{version}" to allow one embedding per (device, schema version).
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS DeviceEmbedding(\
                id             STRING,\
                device_address STRING,\
                version        STRING,\
                algorithm      STRING,\
                dimension      INT64,\
                vector_json    STRING,\
                computed_at    TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create DeviceEmbedding table")?;

        // ── GnnScore (D3-9 T3/T4) ────────────────────────────────────────────
        // One row per device per inference run. In calibration mode scores are
        // stored here but do NOT produce DetectionEvents. In production mode,
        // scores above [gnn] threshold produce a DetectionEvent (rule_id=gnn_anomaly).
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS GnnScore(\
                id             STRING,\
                device_address STRING,\
                score          DOUBLE,\
                threshold      DOUBLE,\
                inference_mode STRING,\
                model_version  STRING,\
                fired_detection BOOLEAN,\
                scored_at      TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create GnnScore table")?;

        // ── Location hierarchy (D3-4 / graph-strategy) ───────────────────────
        // Rack already written by common::upsert_rack; create table here so the
        // schema is self-contained regardless of whether the enricher has run.
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS Rack(\
                id         STRING,\
                name       STRING,\
                site       STRING,\
                row_id     STRING,\
                metadata   STRING,\
                updated_at TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create Rack table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS RACK_MEMBER(FROM Device TO Rack)")
            .context("create RACK_MEMBER rel")?;
        conn.query("CREATE REL TABLE IF NOT EXISTS RACK_IN_SITE(FROM Rack TO Site)")
            .context("create RACK_IN_SITE rel")?;

        // Location is the sub-site layer: AZ, pod, building, floor, room.
        // kind carries the semantic ("az" | "pod" | "building" | "floor" | "room" | "other").
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS Location(\
                id         STRING,\
                name       STRING,\
                kind       STRING,\
                site_id    STRING,\
                source     STRING,\
                updated_at TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create Location table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS IN_LOCATION(FROM Device TO Location)")
            .context("create IN_LOCATION rel")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS IN_SITE(FROM Location TO Site)")
            .context("create IN_SITE rel")?;

        // CMDB parent/child hierarchy edges (ServiceNow CMDB integration)
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS CMDB_PARENT_OF(\
                FROM Device TO Device, \
                rel_type    STRING, \
                source_name STRING, \
                updated_at  TIMESTAMP_NS)",
        )
        .context("create CMDB_PARENT_OF (Device→Device) rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS LOC_PARENT_OF(\
                FROM Location TO Location, \
                source_name STRING, \
                updated_at  TIMESTAMP_NS)",
        )
        .context("create LOC_PARENT_OF (Location→Location) rel")?;

        // Migration: add if_index + discards to Interface for sFlow counter writes.
        let _ = conn.query("ALTER TABLE Interface ADD if_index INT64 DEFAULT 0");
        let _ = conn.query("ALTER TABLE Interface ADD in_discards INT64 DEFAULT 0");
        let _ = conn.query("ALTER TABLE Interface ADD out_discards INT64 DEFAULT 0");

        // Migration: add full_address to Location for ServiceNow enrichment.
        let _ = conn.query("ALTER TABLE Location ADD full_address STRING DEFAULT ''");
        let _ = conn.query("ALTER TABLE Location ADD source_name STRING DEFAULT ''");

        // Migration: add id column to Rack if created by an older upsert_rack that
        // used name as PK only. Silently ignored on fresh DBs.
        let _ = conn.query("ALTER TABLE Rack ADD id STRING DEFAULT ''");

        // ── AppFlow (D3-11 / streaming audit) ────────────────────────────────
        // Represents a network flow record exported by a network device.
        // exporter_address = the router/switch that sent the NetFlow/IPFIX packet.
        // src_address / dst_address = endpoints of the observed traffic.
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS AppFlow(\
                id                STRING,\
                exporter_address  STRING,\
                src_address       STRING,\
                dst_address       STRING,\
                dst_port          INT64,\
                protocol          STRING,\
                bytes_per_sec     DOUBLE,\
                packets_per_sec   DOUBLE,\
                updated_at        TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create AppFlow table")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS CARRIES_FLOW(FROM Device TO AppFlow)")
            .context("create CARRIES_FLOW rel")?;

        // ── HostEndpoint (D3-11 / D14) ───────────────────────────────────────
        // Represents a non-network-device endpoint: server, AP client, phone,
        // CPE, IoT, printer. Always optional — absence is valid for SP deploys.
        // id = primary IP address of the endpoint.
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS HostEndpoint(\
                id         STRING,\
                ip         STRING,\
                kind       STRING,\
                hostname   STRING,\
                mac        STRING,\
                vendor     STRING,\
                rack_id    STRING,\
                site_id    STRING,\
                source     STRING,\
                updated_at TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create HostEndpoint table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS CONNECTED_TO(FROM HostEndpoint TO Interface)",
        )
        .context("create CONNECTED_TO rel")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS SRC_HOST(FROM AppFlow TO HostEndpoint)")
            .context("create SRC_HOST rel")?;

        conn.query("CREATE REL TABLE IF NOT EXISTS DST_HOST(FROM AppFlow TO HostEndpoint)")
            .context("create DST_HOST rel")?;

        // RUNS_SERVICE extended: HostEndpoint → Application (OTLP spans from servers).
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HOST_RUNS_SERVICE(FROM HostEndpoint TO Application)",
        )
        .context("create HOST_RUNS_SERVICE rel")?;

        // Migration: add exporter_address to AppFlow if upgrading from a DB
        // created before D3-11. Silently ignored on fresh installs.
        let _ = conn.query("ALTER TABLE AppFlow ADD exporter_address STRING DEFAULT ''");

        // ── D4-11 T2: BmpSession stats columns ───────────────────────────────
        let _ = conn.query("ALTER TABLE BmpSession ADD adj_rib_in_routes INT64 DEFAULT 0");
        let _ = conn.query("ALTER TABLE BmpSession ADD loc_rib_routes INT64 DEFAULT 0");
        let _ = conn.query("ALTER TABLE BmpSession ADD prefixes_rejected INT64 DEFAULT 0");
        let _ = conn.query("ALTER TABLE BmpSession ADD updates_invalid INT64 DEFAULT 0");
        let _ = conn.query("ALTER TABLE BmpSession ADD stats_updated_at TIMESTAMP_NS DEFAULT timestamp_ns('1970-01-01')");

        // ── D4-10 T2: Application metric columns (OTLP /v1/metrics) ──────────
        // D4-10 T3: APP_IMPACTED_BY_NETWORK edge — OTLP + network event correlation
        conn.query("CREATE REL TABLE IF NOT EXISTS APP_IMPACTED_BY_NETWORK(FROM Application TO DetectionEvent)")
            .context("create APP_IMPACTED_BY_NETWORK rel")?;
        let _ = conn.query("ALTER TABLE Application ADD source STRING DEFAULT ''");
        let _ = conn.query("ALTER TABLE Application ADD cpu_pct DOUBLE DEFAULT 0.0");
        let _ = conn.query("ALTER TABLE Application ADD memory_mb DOUBLE DEFAULT 0.0");
        let _ = conn.query("ALTER TABLE Application ADD req_per_sec DOUBLE DEFAULT 0.0");
        let _ = conn.query("ALTER TABLE Application ADD error_rate DOUBLE DEFAULT 0.0");
        let _ = conn.query("ALTER TABLE Application ADD metric_json STRING DEFAULT ''");

        // ── OpticalChannel (D3-8 T5) ─────────────────────────────────────────
        // One row per DWDM/coherent optical channel on a transponder or router
        // line-card. Populated by gNMI (openconfig-terminal-device) or SNMP.
        // tx_power_dbm / rx_power_dbm in dBm; snr_db in dB; frequency_ghz in GHz.
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS OpticalChannel(\
                id              STRING,\
                device_address  STRING,\
                channel_name    STRING,\
                operational_mode STRING,\
                frequency_ghz   DOUBLE,\
                tx_power_dbm    DOUBLE,\
                rx_power_dbm    DOUBLE,\
                snr_db          DOUBLE,\
                ber             DOUBLE,\
                admin_state     STRING,\
                oper_state      STRING,\
                updated_at      TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create OpticalChannel table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_OPTICAL_CHANNEL(FROM Device TO OpticalChannel)",
        )
        .context("create HAS_OPTICAL_CHANNEL rel")?;

        // ── PowerUnit (D3-8 T6) ───────────────────────────────────────────────
        // PDU / UPS / PSU visible via SNMP. One row per physical power unit.
        // outlet_count: total outlets; outlet_active: outlets currently carrying load.
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS PowerUnit(\
                id             STRING,\
                device_address STRING,\
                name           STRING,\
                kind           STRING,\
                outlet_count   INT64,\
                outlet_active  INT64,\
                load_watts     DOUBLE,\
                input_voltage  DOUBLE,\
                status         STRING,\
                updated_at     TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create PowerUnit table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS POWERED_BY(FROM Device TO PowerUnit)",
        )
        .context("create POWERED_BY rel")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS RACK_POWERED_BY(FROM Rack TO PowerUnit)",
        )
        .context("create RACK_POWERED_BY rel")?;

        // ── Environmental telemetry (D4-20 T1) ────────────────────────────────
        // SensorReading: temperature, voltage, power, current, fan speed from
        // gNMI platform/chassis paths or SNMP environmental MIBs.
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS SensorReading(\
                id                  STRING,\
                device_address      STRING,\
                component_name      STRING,\
                sensor_type         STRING,\
                value               DOUBLE,\
                unit                STRING,\
                threshold_warning   DOUBLE,\
                threshold_critical  DOUBLE,\
                updated_at          TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create SensorReading table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS REPORTED_BY(FROM SensorReading TO Device)",
        )
        .context("create REPORTED_BY rel")?;

        // OpticsTelemetry: per-interface Rx/Tx power, temperature, bias for
        // pluggable transceivers (SFP/QSFP). Distinct from OpticalChannel which
        // models coherent DWDM channels on line-cards.
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS OpticsTelemetry(\
                id              STRING,\
                device_address  STRING,\
                interface_name  STRING,\
                rx_power_dbm    DOUBLE,\
                tx_power_dbm    DOUBLE,\
                wavelength_nm   DOUBLE,\
                temperature_c   DOUBLE,\
                bias_current_ma DOUBLE,\
                updated_at      TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create OpticsTelemetry table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS OPTICS_ON(FROM OpticsTelemetry TO Interface)",
        )
        .context("create OPTICS_ON rel")?;

        // ── Change Management (ServiceNow CHG / AAP / manual) ────────────────
        // ChangeRequest represents a planned or in-progress change ticket.
        // source: "servicenow" | "aap" | "ansible_tower" | "manual" | "webhook"
        // state: "new" | "scheduled" | "implement" | "review" | "closed" | "cancelled"
        // change_type: "standard" | "normal" | "emergency"
        // risk: "high" | "moderate" | "low" | "none"
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS ChangeRequest(\
                id                  STRING,\
                number              STRING,\
                source              STRING,\
                snow_sys_id         STRING,\
                short_description   STRING,\
                state               STRING,\
                change_type         STRING,\
                risk                STRING,\
                assigned_to         STRING,\
                assignment_group    STRING,\
                affected_cis_json   STRING,\
                planned_start_ns    INT64,\
                planned_end_ns      INT64,\
                actual_start_ns     INT64,\
                actual_end_ns       INT64,\
                correlation_id      STRING,\
                external_ref        STRING,\
                updated_at          TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create ChangeRequest table")?;

        // Device affected by a planned change.
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS AFFECTED_BY_CHANGE(\
                FROM Device TO ChangeRequest, \
                role STRING, \
                updated_at TIMESTAMP_NS)",
        )
        .context("create AFFECTED_BY_CHANGE rel")?;

        // ConfigChange or DetectionEvent that occurred during an active change window.
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS CHANGE_CAUSED_CONFIG(\
                FROM ConfigChange TO ChangeRequest)",
        )
        .context("create CHANGE_CAUSED_CONFIG rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS CHANGE_CAUSED_DETECTION(\
                FROM DetectionEvent TO ChangeRequest)",
        )
        .context("create CHANGE_CAUSED_DETECTION rel")?;

        // Incident linked to the authorising change ticket.
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS RELATED_TO_CHANGE(\
                FROM Incident TO ChangeRequest)",
        )
        .context("create RELATED_TO_CHANGE rel")?;

        // ── ShunRule (D4-2 T1) ───────────────────────────────────────────────
        // Syslog shunning rules stored in graph for persistence + auditability.
        // scope_type: "device" | "global"
        // match_type: "substring" | "regex" | "fact_type"
        // action: "drop" | "rate_limit"
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS ShunRule(\
                id                STRING,\
                scope_type        STRING,\
                scope_value       STRING,\
                match_type        STRING,\
                match_value       STRING,\
                action            STRING,\
                rate_limit_per_min INT64,\
                expires_at_ns     INT64,\
                created_by        STRING,\
                created_at_ns     INT64,\
                enabled           BOOLEAN,\
                PRIMARY KEY (id))",
        )
        .context("create ShunRule table")?;

        // ── ComputeNode (D4-5 T3) ────────────────────────────────────────────
        // Represents a server, VM, or container node discovered via sFlow
        // counter samples or explicit provisioning (IPAM/CMDB enrichment).
        // kind: "server" | "vm" | "container" | "bare_metal" | "unknown"
        // source: "sflow" | "netbox" | "manual"
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS ComputeNode(\
                id              STRING,\
                ip              STRING,\
                hostname        STRING,\
                kind            STRING,\
                os              STRING,\
                vcpus           INT64,\
                memory_mb       INT64,\
                source          STRING,\
                rack_id         STRING,\
                site_id         STRING,\
                updated_at      TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create ComputeNode table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS COMPUTE_CONNECTED_TO(\
                FROM ComputeNode TO Interface, \
                link_speed_mbps INT64, \
                updated_at TIMESTAMP_NS)",
        )
        .context("create COMPUTE_CONNECTED_TO rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS CARRIES_COMPUTE(\
                FROM Device TO ComputeNode, \
                source  STRING, \
                updated_at TIMESTAMP_NS)",
        )
        .context("create CARRIES_COMPUTE rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS FLOW_SRC_COMPUTE(FROM AppFlow TO ComputeNode)",
        )
        .context("create FLOW_SRC_COMPUTE rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS FLOW_DST_COMPUTE(FROM AppFlow TO ComputeNode)",
        )
        .context("create FLOW_DST_COMPUTE rel")?;

        // ── D4-7 T1: ConfigItem table (all YAML config classes, DB-backed) ──────
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS ConfigItem(\
                id             STRING,\
                config_class   STRING,\
                vendor         STRING,\
                name           STRING,\
                version        STRING,\
                content_json   STRING,\
                enabled        BOOLEAN,\
                created_at     TIMESTAMP_NS,\
                updated_at     TIMESTAMP_NS,\
                created_by     STRING,\
                PRIMARY KEY (id))",
        )
        .context("create ConfigItem table")?;

        // ── RedundancyGroup (D4-12 T1) ───────────────────────────────────────
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS RedundancyGroup(\
                id              STRING,\
                name            STRING,\
                kind            STRING,\
                protocol        STRING,\
                site_id         STRING,\
                status          STRING,\
                member_count    INT32,\
                active_count    INT32,\
                source          STRING,\
                discovered_at   TIMESTAMP_NS,\
                updated_at      TIMESTAMP_NS,\
                PRIMARY KEY (id))",
        )
        .context("create RedundancyGroup table")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS MEMBER_OF(\
                FROM Device TO RedundancyGroup,\
                role        STRING,\
                priority    INT32,\
                state       STRING,\
                updated_at  TIMESTAMP_NS)",
        )
        .context("create MEMBER_OF rel")?;

        conn.query(
            "CREATE REL TABLE IF NOT EXISTS REDUNDANT_WITH(\
                FROM Device TO Device,\
                group_id    STRING,\
                protocol    STRING,\
                updated_at  TIMESTAMP_NS)",
        )
        .context("create REDUNDANT_WITH rel")?;

        // ── OspfNeighbor (batch23: expanded bootstrap parsing) ────────────────
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS OspfNeighbor(\
                id              STRING,\
                device_address  STRING,\
                neighbor_id     STRING,\
                interface       STRING,\
                state           STRING,\
                area            STRING,\
                dr              STRING,\
                bdr             STRING,\
                priority        INT64,\
                source          STRING,\
                updated_at_ns   INT64,\
                PRIMARY KEY (id))",
        )
        .context("create OspfNeighbor table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_OSPF_NEIGHBOR(FROM Device TO OspfNeighbor)",
        )
        .context("create HAS_OSPF_NEIGHBOR rel")?;

        // ── StpInstance (batch23) ────────────────────────────────────────────
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS StpInstance(\
                id                STRING,\
                device_address    STRING,\
                vlan_id           INT64,\
                instance          STRING,\
                root_bridge       STRING,\
                root_port         STRING,\
                bridge_priority   INT64,\
                is_root           BOOLEAN,\
                topology_changes  INT64,\
                protocol          STRING,\
                source            STRING,\
                updated_at_ns     INT64,\
                PRIMARY KEY (id))",
        )
        .context("create StpInstance table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_STP_INSTANCE(FROM Device TO StpInstance)",
        )
        .context("create HAS_STP_INSTANCE rel")?;

        // ── Vrf (batch23) ────────────────────────────────────────────────────
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS Vrf(\
                id                      STRING,\
                device_address          STRING,\
                name                    STRING,\
                rd                      STRING,\
                rt_import_json          STRING,\
                rt_export_json          STRING,\
                interfaces_json         STRING,\
                address_families_json   STRING,\
                source                  STRING,\
                updated_at_ns           INT64,\
                PRIMARY KEY (id))",
        )
        .context("create Vrf table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_VRF(FROM Device TO Vrf)",
        )
        .context("create HAS_VRF rel")?;

        // ── NtpPeer (batch23) ────────────────────────────────────────────────
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS NtpPeer(\
                id              STRING,\
                device_address  STRING,\
                peer_address    STRING,\
                stratum         INT64,\
                state           STRING,\
                offset_ms       DOUBLE,\
                reach           INT64,\
                ref_id          STRING,\
                is_synchronized BOOLEAN,\
                source          STRING,\
                updated_at_ns   INT64,\
                PRIMARY KEY (id))",
        )
        .context("create NtpPeer table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_NTP_PEER(FROM Device TO NtpPeer)",
        )
        .context("create HAS_NTP_PEER rel")?;

        // ── AclSummary (batch23) ─────────────────────────────────────────────
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS AclSummary(\
                id                      STRING,\
                device_address          STRING,\
                name                    STRING,\
                acl_type                STRING,\
                ace_count               INT64,\
                applied_interfaces_json STRING,\
                total_matches           INT64,\
                source                  STRING,\
                updated_at_ns           INT64,\
                PRIMARY KEY (id))",
        )
        .context("create AclSummary table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_ACL(FROM Device TO AclSummary)",
        )
        .context("create HAS_ACL rel")?;

        // ── MplsLsp (batch23) ────────────────────────────────────────────────
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS MplsLsp(\
                id              STRING,\
                device_address  STRING,\
                name            STRING,\
                destination     STRING,\
                state           STRING,\
                in_label        INT64,\
                out_label       INT64,\
                out_interface   STRING,\
                next_hop        STRING,\
                protocol        STRING,\
                source          STRING,\
                updated_at_ns   INT64,\
                PRIMARY KEY (id))",
        )
        .context("create MplsLsp table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_MPLS_LSP(FROM Device TO MplsLsp)",
        )
        .context("create HAS_MPLS_LSP rel")?;

        // ─── Performance Baseline Schema (Immediate Win) ────────────────────────
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS PerformanceBaseline(\
                id                    STRING,\
                device_address        STRING,\
                metric_type           STRING,\
                metric_key            STRING,\
                baseline_mean         DOUBLE,\
                baseline_stddev       DOUBLE,\
                baseline_min          DOUBLE,\
                baseline_max          DOUBLE,\
                sample_count          INT64,\
                computed_at_ns        INT64,\
                lookback_hours        INT32,\
                confidence_level      DOUBLE,\
                PRIMARY KEY (id))",
        )
        .context("create PerformanceBaseline table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_BASELINE(\
                FROM Device TO PerformanceBaseline,\
                updated_at INT64)",
        )
        .context("create HAS_BASELINE rel")?;

        // ─── Service Endpoint Schema (Service Mesh Discovery) ─────────────────
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS ServiceEndpoint(\
                id                    STRING,\
                device_address        STRING,\
                interface_name        STRING,\
                service_type          STRING,\
                service_name          STRING,\
                endpoint_type         STRING,\
                connection_count      INT64,\
                avg_throughput_mbps   DOUBLE,\
                discovered_via        STRING,\
                confidence_score      DOUBLE,\
                updated_at_ns         INT64,\
                PRIMARY KEY (id))",
        )
        .context("create ServiceEndpoint table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HOSTS_SERVICE(\
                FROM Device TO ServiceEndpoint,\
                role STRING,\
                updated_at INT64)",
        )
        .context("create HOSTS_SERVICE rel")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS COMMUNICATES_WITH(\
                FROM ServiceEndpoint TO ServiceEndpoint,\
                protocol STRING,\
                avg_flow_rate_mbps DOUBLE,\
                connection_count INT64,\
                updated_at INT64)",
        )
        .context("create COMMUNICATES_WITH rel")?;

        // ─── QoS Policy Schema ────────────────────────────────────────────────
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS QoSPolicy(\
                id                    STRING,\
                device_address        STRING,\
                policy_name           STRING,\
                traffic_class         STRING,\
                dscp_value            INT32,\
                queue_id              INT32,\
                min_bandwidth_mbps    DOUBLE,\
                max_bandwidth_mbps    DOUBLE,\
                priority_level        INT32,\
                policy_type           STRING,\
                updated_at_ns         INT64,\
                PRIMARY KEY (id))",
        )
        .context("create QoSPolicy table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS APPLIES_QOS(\
                FROM Interface TO QoSPolicy,\
                direction STRING,\
                updated_at INT64)",
        )
        .context("create APPLIES_QOS rel")?;

        // ─── Security Posture Schema ───────────────────────────────────────────
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS SecurityPosture(\
                id                    STRING,\
                device_address        STRING,\
                aaa_failure_count     INT64,\
                config_change_count   INT64,\
                process_crash_count   INT64,\
                last_auth_failure_ns  INT64,\
                last_config_change_ns INT64,\
                risk_score            DOUBLE,\
                updated_at_ns         INT64,\
                PRIMARY KEY (id))",
        )
        .context("create SecurityPosture table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_POSTURE(\
                FROM Device TO SecurityPosture,\
                updated_at INT64)",
        )
        .context("create HAS_POSTURE rel")?;

        // ─── Security Incident Schema ────────────────────────────────────────
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS SecurityIncident(\
                id                    STRING,\
                incident_type         STRING,\
                severity              STRING,\
                title                 STRING,\
                description           STRING,\
                source_ip             STRING,\
                target_device         STRING,\
                mitre_technique       STRING,\
                status                STRING,\
                detected_at_ns        INT64,\
                resolved_at_ns        INT64,\
                assigned_to           STRING,\
                PRIMARY KEY (id))",
        )
        .context("create SecurityIncident table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS HAS_SECURITY_INCIDENT(\
                FROM Device TO SecurityIncident,\
                role STRING,\
                updated_at INT64)",
        )
        .context("create HAS_SECURITY_INCIDENT rel")?;

        // ─── Vulnerability Schema ─────────────────────────────────────────────
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS Vulnerability(\
                id                    STRING,\
                cve_id                STRING,\
                title                 STRING,\
                description           STRING,\
                severity              STRING,\
                cvss_score            DOUBLE,\
                affected_component    STRING,\
                discovered_at_ns      INT64,\
                patched_at_ns         INT64,\
                PRIMARY KEY (id))",
        )
        .context("create Vulnerability table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS VULNERABLE_TO(\
                FROM Device TO Vulnerability,\
                affected_version STRING,\
                patch_available BOOL,\
                updated_at INT64)",
        )
        .context("create VULNERABLE_TO rel")?;

        // ─── Security Policy Schema ───────────────────────────────────────────
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS SecurityPolicy(\
                id                    STRING,\
                policy_name           STRING,\
                policy_type           STRING,\
                description           STRING,\
                compliance_framework  STRING,\
                enforcement_status    STRING,\
                created_at_ns         INT64,\
                updated_at_ns         INT64,\
                PRIMARY KEY (id))",
        )
        .context("create SecurityPolicy table")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS MITIGATED_BY(\
                FROM Vulnerability TO SecurityPolicy,\
                mitigation_type STRING,\
                effectiveness DOUBLE,\
                updated_at INT64)",
        )
        .context("create MITIGATED_BY rel")?;
        conn.query(
            "CREATE REL TABLE IF NOT EXISTS ENFORCES_POLICY(\
                FROM Device TO SecurityPolicy,\
                compliance_status STRING,\
                last_checked_ns INT64)",
        )
        .context("create ENFORCES_POLICY rel")?;

        // Migration: add columns used by expanded bootstrap to Device
        let _ = conn.query("ALTER TABLE Device ADD model STRING DEFAULT ''");
        let _ = conn.query("ALTER TABLE Device ADD serial_number STRING DEFAULT ''");
        let _ = conn.query("ALTER TABLE Device ADD cpu_util_pct DOUBLE DEFAULT 0.0");
        let _ = conn.query("ALTER TABLE Device ADD memory_used_mb DOUBLE DEFAULT 0.0");
        let _ = conn.query("ALTER TABLE Device ADD memory_total_mb DOUBLE DEFAULT 0.0");
        let _ = conn.query("ALTER TABLE Device ADD uptime_seconds INT64 DEFAULT 0");
        let _ = conn.query("ALTER TABLE Device ADD boot_image STRING DEFAULT ''");
        let _ = conn.query("ALTER TABLE Device ADD hardware_rev STRING DEFAULT ''");
        let _ = conn.query("ALTER TABLE Device ADD slot_inventory_json STRING DEFAULT ''");

        // Migration: add columns used by BFD bootstrap seeding
        let _ = conn.query("ALTER TABLE BfdSession ADD registered_protocols STRING DEFAULT ''");
        let _ = conn.query("ALTER TABLE BfdSession ADD local_diag STRING DEFAULT ''");
        let _ = conn.query("ALTER TABLE BfdSession ADD detect_multiplier INT64 DEFAULT 3");
        let _ = conn.query("ALTER TABLE BfdSession ADD interval_ms INT64 DEFAULT 0");
        let _ = conn.query("ALTER TABLE BfdSession ADD source STRING DEFAULT ''");
        let _ = conn.query("ALTER TABLE BfdSession ADD updated_at_ns INT64 DEFAULT 0");
        let _ = conn.query("ALTER TABLE BfdSession ADD peer_address STRING DEFAULT ''");

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
                     OPTIONAL MATCH (s)-[:BELONGS_TO_ENVIRONMENT]->(e:Environment) \
                     RETURN s.id, s.name, s.parent_id, s.kind, s.lat, s.lon, s.metadata_json, e.id \
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

    pub async fn list_environments(&self) -> Result<Vec<EnvironmentWithCounts>> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("list environments connection")?;
            let env_rows = conn
                .query(
                    "MATCH (e:Environment) \
                     RETURN e.id, e.name, e.archetype, e.created_at, e.metadata_json \
                     ORDER BY e.name",
                )
                .context("list environments query")?
                .map(environment_from_row)
                .collect::<Vec<_>>();

            let mut out = Vec::with_capacity(env_rows.len());
            for env in env_rows {
                let mut site_stmt = conn
                    .prepare(
                        "MATCH (s:Site)-[:BELONGS_TO_ENVIRONMENT]->(e:Environment {id: $id}) \
                         RETURN count(s)",
                    )
                    .context("prepare site count")?;
                let site_count = conn
                    .execute(&mut site_stmt, vec![("id", Value::String(env.id.clone()))])
                    .context("execute site count")?
                    .next()
                    .map(|r| match &r[0] {
                        Value::Int64(n) => *n,
                        _ => 0,
                    })
                    .unwrap_or(0);

                let mut dev_stmt = conn
                    .prepare(
                        "MATCH (s:Site)-[:BELONGS_TO_ENVIRONMENT]->(e:Environment {id: $id}) \
                         MATCH (d:Device)-[:LOCATED_AT]->(s) \
                         RETURN count(d)",
                    )
                    .context("prepare device count")?;
                let device_count = conn
                    .execute(&mut dev_stmt, vec![("id", Value::String(env.id.clone()))])
                    .context("execute device count")?
                    .next()
                    .map(|r| match &r[0] {
                        Value::Int64(n) => *n,
                        _ => 0,
                    })
                    .unwrap_or(0);

                out.push(EnvironmentWithCounts {
                    id: env.id,
                    name: env.name,
                    archetype: env.archetype,
                    created_at_ns: env.created_at_ns,
                    metadata_json: env.metadata_json,
                    site_count,
                    device_count,
                });
            }
            Ok::<_, anyhow::Error>(out)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn create_environment(&self, env: EnvironmentRecord) -> Result<EnvironmentRecord> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("create environment connection")?;
            let env = normalize_environment(env)?;
            upsert_environment_record(&conn, &env)?;
            Ok::<_, anyhow::Error>(env)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn update_environment(&self, env: EnvironmentRecord) -> Result<EnvironmentRecord> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("update environment connection")?;
            let env = normalize_environment(env)?;
            upsert_environment_record(&conn, &env)?;
            Ok::<_, anyhow::Error>(env)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn delete_environment(&self, id: String) -> Result<Result<(), String>> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("delete environment connection")?;

            let mut count_stmt = conn
                .prepare(
                    "MATCH (s:Site)-[:BELONGS_TO_ENVIRONMENT]->(e:Environment {id: $id}) \
                     RETURN count(s)",
                )
                .context("prepare site count for delete")?;
            let site_count = conn
                .execute(&mut count_stmt, vec![("id", Value::String(id.clone()))])
                .context("execute site count for delete")?
                .next()
                .map(|r| match &r[0] { Value::Int64(n) => *n, _ => 0 })
                .unwrap_or(0);

            if site_count > 0 {
                return Ok(Err(format!(
                    "environment '{id}' has {site_count} site(s) assigned — reassign them before deleting"
                )));
            }

            let mut del_stmt = conn
                .prepare("MATCH (e:Environment {id: $id}) DELETE e")
                .context("prepare environment delete")?;
            conn.execute(&mut del_stmt, vec![("id", Value::String(id))])
                .context("execute environment delete")?;

            Ok(Ok(()))
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn assign_site_to_environment(&self, site_id: String, env_id: String) -> Result<()> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("assign site environment connection")?;
            link_site_to_environment(&conn, &site_id, &env_id)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Idempotent startup migration: sites without an environment binding get
    /// assigned to the default "migrated" home-lab environment. Returns count of
    /// sites migrated (0 on subsequent calls).
    pub fn migrate_sites_to_default_environment(&self) -> Result<usize> {
        let conn = Connection::new(&self.db).context("migrate environments connection")?;
        let _guard = self.write_lock.lock().expect("write lock poisoned");

        let default_env = EnvironmentRecord {
            id: DEFAULT_ENVIRONMENT_ID.to_string(),
            name: DEFAULT_ENVIRONMENT_NAME.to_string(),
            archetype: ARCHETYPE_HOME_LAB.to_string(),
            created_at_ns: now_ns(),
            metadata_json: "{}".to_string(),
        };
        upsert_environment_record(&conn, &default_env)?;

        let unbound_rows = conn
            .query(
                "MATCH (s:Site) \
                 WHERE NOT (s)-[:BELONGS_TO_ENVIRONMENT]->() \
                 RETURN s.id",
            )
            .context("query unbound sites")?
            .map(|r| read_str(&r[0]))
            .collect::<Vec<_>>();

        let count = unbound_rows.len();
        for site_id in unbound_rows {
            link_site_to_environment(&conn, &site_id, DEFAULT_ENVIRONMENT_ID)?;
        }

        if count > 0 {
            info!(count, "migrated sites to default environment");
        }
        Ok(count)
    }

    pub async fn sync_sites_from_targets(&self, targets: Vec<TargetConfig>) -> Result<()> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("site sync connection")?;
            let now = ts(now_ns());
            for target in targets {
                let primary = target.primary_ip().to_string();
                let role = target.role.as_deref().unwrap_or_default();
                let site_str = target.site.as_deref().unwrap_or_default();
                upsert_device(
                    &conn,
                    &target.address,
                    target.vendor.as_deref().unwrap_or_default(),
                    target.hostname.as_deref().unwrap_or_default(),
                    role,
                    site_str,
                    now.clone(),
                )?;
                // Register extra_ips from onboarding discovery as DeviceAddress nodes.
                for ip in &target.extra_ips {
                    let ip = ip.trim();
                    if !ip.is_empty() {
                        let _ = upsert_device_address(&conn, ip, "extra_ip", now.clone());
                        let _ = link_device_address(&conn, ip, &primary);
                    }
                }
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
                    environment_id: String::new(),
                };
                upsert_site_record(&conn, &site, now.clone())?;
                link_device_to_site(&conn, &primary, &site.id)?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    fn backfill_remediation_trust_marks(&self) -> Result<()> {
        const MARKER_ID: &str = "backfill_trust_v1";
        let conn = Connection::new(&self.db).context("trust-mark backfill connection")?;

        // Skip if this one-shot migration has already been applied.
        let mut check = conn
            .prepare("MATCH (m:MigrationMarker {id: $id}) RETURN m.id")
            .context("prepare migration marker check")?;
        let rows = conn
            .execute(
                &mut check,
                vec![("id", Value::String(MARKER_ID.to_string()))],
            )
            .context("execute migration marker check")?;
        if rows.into_iter().next().is_some() {
            debug!("backfill_trust_v1 already applied — skipping");
            return Ok(());
        }

        let mut stmt = conn
            .prepare(
                "MATCH (r:Remediation) \
                 OPTIONAL MATCH (m:RemediationTrustMark {remediation_id: r.id}) \
                 RETURN r.id, r.attempted_at, m.remediation_id",
            )
            .context("prepare trust-mark backfill query")?;
        // Eagerly collect all rows before the write loop: lbug does not support
        // an open read cursor and a write on the same connection simultaneously.
        let rows: Vec<_> = conn
            .execute(&mut stmt, Vec::new())
            .context("execute trust-mark backfill query")?
            .collect();

        let mut created = 0usize;
        for row in &rows {
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

        // Only record the migration marker when there were remediations to examine.
        // If the DB is empty at startup the marker is withheld so that the next
        // startup (when remediations may exist) gets a chance to backfill them.
        if !rows.is_empty() {
            let mut mark = conn
                .prepare(
                    "MERGE (m:MigrationMarker {id: $id}) \
                     ON CREATE SET m.applied_at = $ts",
                )
                .context("prepare migration marker insert")?;
            conn.execute(
                &mut mark,
                vec![
                    ("id", Value::String(MARKER_ID.to_string())),
                    ("ts", ts(now_ns())),
                ],
            )
            .context("execute migration marker insert")?;
            info!("migration marker backfill_trust_v1 recorded");
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
        source_types_json: String,
        latency_ns: i64,
        fired_at_ns: i64,
        state_change_event_id: String,
        source_event_ids: Vec<String>,
    ) -> Result<String> {
        // G6: Emit metric for detection write
        metrics::counter!("bonsai_graph_detection_write_total", "severity" => severity.clone()).increment(1);

        let event_addr = device_address.clone();
        let event_rule = rule_id.clone();
        let event_sev = severity.clone();
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        let id = tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("detection write connection")?;
            conn.query("BEGIN TRANSACTION").context("detection begin transaction")?;
            let id = Uuid::new_v4().to_string();
            let now = ts(fired_at_ns);
            let metric_rule_id = rule_id.clone();
            let metric_severity = severity.clone();
            let mut trigger_event_ids = source_event_ids;
            if !state_change_event_id.is_empty() {
                trigger_event_ids.push(state_change_event_id);
            }
            trigger_event_ids.sort();
            trigger_event_ids.dedup();
            let mut stmt = conn
                .prepare(
                    "MERGE (e:DetectionEvent {id: $id}) \
                 ON CREATE SET \
                   e.device_address = $addr, e.rule_id = $rule, \
                   e.severity = $sev, e.features_json = $feats, \
                   e.source_types = $srctypes, e.latency_ns = $latency, \
                   e.fired_at = $ts",
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
                    ("srctypes", Value::String(source_types_json)),
                    ("latency", Value::Int64(latency_ns)),
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
            if !trigger_event_ids.is_empty() {
                let mut tb = conn
                    .prepare(
                        "MATCH (e:DetectionEvent {id: $eid}), (s:StateChangeEvent {id: $sid})\
                     CREATE (e)-[:TRIGGERED_BY]->(s)",
                    )
                    .context("prepare TRIGGERED_BY edge")?;
                for source_event_id in trigger_event_ids {
                    conn.execute(
                        &mut tb,
                        vec![
                            ("eid", Value::String(id.clone())),
                            ("sid", Value::String(source_event_id)),
                        ],
                    )
                    .context("execute TRIGGERED_BY edge")?;
                }
            }
            conn.query("COMMIT").context("detection commit transaction")?;
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
        .context("detection write")?;

        self.publish_event(BonsaiEvent {
            device_address: event_addr,
            event_type: "detection_fired".to_string(),
            detail_json: format!(
                r#"{{"id":"{}","rule_id":"{}","severity":"{}"}}"#,
                id, event_rule, event_sev
            ),
            occurred_at_ns: fired_at_ns,
            state_change_event_id: String::new(),
            source_type: "detection".to_string(),
        });

        Ok(id)
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
        // G6: Emit metric for remediation write
        metrics::counter!("bonsai_graph_remediation_write_total", "status" => status.clone()).increment(1);

        let event_detection_id = detection_id.clone();
        let event_action = action.clone();
        let event_status = status.clone();
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        let id = tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("remediation write connection")?;
            conn.query("BEGIN TRANSACTION").context("remediation begin transaction")?;
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
            conn.query("COMMIT").context("remediation commit transaction")?;
            Ok::<String, anyhow::Error>(id)
        })
        .await
        .context("spawn_blocking panicked")?
        .context("remediation write")?;

        self.publish_event(BonsaiEvent {
            device_address: String::new(),
            event_type: "remediation_outcome".to_string(),
            detail_json: format!(
                r#"{{"id":"{}","detection_id":"{}","action":"{}","status":"{}"}}"#,
                id, event_detection_id, event_action, event_status
            ),
            occurred_at_ns: attempted_at_ns,
            state_change_event_id: String::new(),
            source_type: "detection".to_string(),
        });

        Ok(id)
    }

    pub async fn write_remediation_proposal(
        &self,
        detection_id: String,
        playbook_id: String,
        trust_key: String,
        steps_json: String,
        rollback_steps_json: String,
        proposed_at_ns: i64,
    ) -> Result<String> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("proposal write connection")?;
            let id = Uuid::new_v4().to_string();
            let proposed_at = ts(proposed_at_ns);
            let mut stmt = conn
                .prepare(
                    "MERGE (p:RemediationProposal {id: $id}) \
                     ON CREATE SET \
                       p.detection_id = $did, p.playbook_id = $playbook, \
                       p.trust_key = $trust_key, p.status = 'pending', \
                       p.operator_note = '', p.steps_json = $steps, \
                       p.rollback_steps_json = $rollback_steps, \
                       p.proposed_at = $proposed_at, p.decided_at = $decided_at",
                )
                .context("prepare RemediationProposal insert")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("did", Value::String(detection_id.clone())),
                    ("playbook", Value::String(playbook_id)),
                    ("trust_key", Value::String(trust_key)),
                    ("steps", Value::String(steps_json)),
                    ("rollback_steps", Value::String(rollback_steps_json)),
                    ("proposed_at", proposed_at),
                    ("decided_at", ts(0)),
                ],
            )
            .context("execute RemediationProposal insert")?;

            let mut edge = conn
                .prepare(
                    "MATCH (e:DetectionEvent {id: $did}), (p:RemediationProposal {id: $id}) \
                     CREATE (e)-[:HAS_PROPOSAL]->(p)",
                )
                .context("prepare HAS_PROPOSAL edge")?;
            conn.execute(
                &mut edge,
                vec![
                    ("did", Value::String(detection_id)),
                    ("id", Value::String(id.clone())),
                ],
            )
            .context("execute HAS_PROPOSAL edge")?;
            Ok::<String, anyhow::Error>(id)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn decide_remediation_proposal(
        &self,
        proposal_id: String,
        status: String,
        operator_note: String,
        decided_at_ns: i64,
    ) -> Result<()> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("proposal decision connection")?;
            let mut stmt = conn
                .prepare(
                    "MATCH (p:RemediationProposal {id: $id}) \
                     SET p.status = $status, p.operator_note = $note, p.decided_at = $decided_at",
                )
                .context("prepare RemediationProposal decision")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(proposal_id)),
                    ("status", Value::String(status)),
                    ("note", Value::String(operator_note)),
                    ("decided_at", ts(decided_at_ns)),
                ],
            )
            .context("execute RemediationProposal decision")?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn read_remediation_proposals(
        &self,
        status_filter: Option<String>,
        limit: u32,
    ) -> Result<Vec<RemediationProposalRow>> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("proposal read connection")?;
            let where_clause = status_filter
                .as_ref()
                .filter(|s| !s.is_empty() && *s != "all")
                .map(|_| "WHERE p.status = $status ")
                .unwrap_or("");
            let cypher = format!(
                "MATCH (p:RemediationProposal) \
                 OPTIONAL MATCH (e:DetectionEvent {{id: p.detection_id}}) \
                 {where_clause}\
                 RETURN p.id, p.detection_id, e.device_address, e.rule_id, e.severity, \
                        p.playbook_id, p.trust_key, p.status, p.operator_note, \
                        p.steps_json, p.rollback_steps_json, p.proposed_at, p.decided_at, \
                        e.features_json \
                 ORDER BY p.proposed_at DESC LIMIT {limit}"
            );
            let mut stmt = conn.prepare(&cypher).context("prepare proposal read")?;
            let params = status_filter
                .filter(|s| !s.is_empty() && s != "all")
                .map(|status| vec![("status", Value::String(status))])
                .unwrap_or_default();
            let rows = conn
                .execute(&mut stmt, params)
                .context("execute proposal read")?;
            let mut out = Vec::new();
            for row in rows {
                out.push(RemediationProposalRow {
                    id: read_str(&row[0]),
                    detection_id: read_str(&row[1]),
                    device_address: read_str(&row[2]),
                    rule_id: read_str(&row[3]),
                    severity: read_str(&row[4]),
                    playbook_id: read_str(&row[5]),
                    trust_key: read_str(&row[6]),
                    status: read_str(&row[7]),
                    operator_note: read_str(&row[8]),
                    steps_json: read_str(&row[9]),
                    rollback_steps_json: read_str(&row[10]),
                    proposed_at_ns: read_ts_ns(&row[11]),
                    decided_at_ns: read_ts_ns(&row[12]),
                    features_json: read_str(&row[13]),
                });
            }
            Ok::<_, anyhow::Error>(out)
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
                        e.features_json, e.source_types, e.latency_ns, e.fired_at, \
                        r.id, r.action, r.status \
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
                    let src_json = read_str(&row[5]);
                    let source_types: Vec<String> = serde_json::from_str(&src_json).unwrap_or_default();
                    out.push(DetectionRow {
                        id,
                        device_address: read_str(&row[1]),
                        rule_id: read_str(&row[2]),
                        severity: read_str(&row[3]),
                        features_json: read_str(&row[4]),
                        source_types,
                        latency_ns: read_i64(&row[6]),
                        fired_at_ns: read_ts_ns(&row[7]),
                        remediation_id: read_str(&row[8]),
                        remediation_action: read_str(&row[9]),
                        remediation_status: read_str(&row[10]),
                    });
                }
            }
            Ok::<_, anyhow::Error>(out)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Compute and store performance baselines for all devices
    pub async fn compute_performance_baselines(&self, lookback_hours: i32) -> Result<Vec<crate::graph::algorithms::PerformanceBaselineRow>> {
        let db = Arc::clone(&self.db);
        let lookback = lookback_hours;
        
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("baseline computation connection")?;
            
            // Get all devices with interface data
            let device_rows = conn
                .query(
                    "MATCH (d:Device)-[:HAS_INTERFACE]->(i:Interface) \
                     WHERE i.in_octets > 0 OR i.out_octets > 0 \
                     RETURN DISTINCT d.address",
                )
                .context("get devices for baseline computation")?;
            
            let mut all_baselines = Vec::new();
            
            for row in device_rows {
                let device_address = read_str(&row[0]);
                
                // Compute baselines for this device
                let baselines = crate::graph::algorithms::compute_interface_utilization_baseline(
                    &conn,
                    &device_address,
                    lookback,
                )?;
                
                // Store baselines in graph
                for baseline in &baselines {
                    crate::graph::algorithms::store_performance_baseline(&conn, baseline)?;
                }
                
                all_baselines.extend(baselines);
            }
            
            Ok::<_, anyhow::Error>(all_baselines)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Discover service endpoints from interface descriptions
    pub async fn discover_service_endpoints(&self) -> Result<Vec<crate::service_discovery::ServiceEndpoint>> {
        let db = Arc::clone(&self.db);
        
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("service discovery connection")?;
            
            // Get all devices with interface descriptions
            let device_rows = conn
                .query(
                    "MATCH (d:Device)-[:HAS_INTERFACE]->(i:Interface) \
                     WHERE i.description IS NOT NULL AND i.description <> '' \
                     RETURN DISTINCT d.address",
                )
                .context("get devices for service discovery")?;
            
            let service_config = crate::service_discovery::ServiceDiscoveryConfig::default();
            let discovery = crate::service_discovery::ServiceDiscovery::new(service_config);
            let mut all_endpoints = Vec::new();
            
            for row in device_rows {
                let device_address = read_str(&row[0]);
                
                // Discover services from descriptions
                let endpoints = discovery.discover_from_descriptions(&conn, &device_address)?;
                
                // Store endpoints in graph
                for endpoint in &endpoints {
                    discovery.store_service_endpoint(&conn, endpoint)?;
                }
                
                all_endpoints.extend(endpoints);
            }
            
            Ok::<_, anyhow::Error>(all_endpoints)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Update security posture for devices based on recent events
    pub async fn update_security_posture(&self) -> Result<Vec<String>> {
        let db = Arc::clone(&self.db);
        
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("security posture connection")?;
            
            // Get recent security-related events
            let cutoff_ns = crate::graph::common::now_ns() - (15 * 60 * 1_000_000_000); // 15 minutes
            
            let mut device_stmt = conn
                .prepare(
                    "MATCH (d:Device) \
                     OPTIONAL MATCH (d)-[:HAS_EVENT]->(e:StateChangeEvent) \
                     WHERE e.fired_at_ns > $cutoff_ns AND (e.source_type = 'syslog' OR e.source_type = 'snmp') \
                     RETURN DISTINCT d.address",
                )
                .context("prepare devices for security posture")?;
            let device_rows = conn
                .execute(&mut device_stmt, vec![("cutoff_ns", Value::Int64(cutoff_ns))])
                .context("get devices for security posture")?;
            
            let mut updated_devices = Vec::new();
            
            for row in device_rows {
                let device_address = read_str(&row[0]);
                
                // Count AAA failures
                let aaa_failures = {
                    let mut stmt = conn.prepare(
                        "MATCH (d:Device {address: $device_address})-[:HAS_EVENT]->(e:StateChangeEvent) \
                         WHERE e.fired_at_ns > $cutoff_ns AND e.source_type = 'syslog' \
                         AND e.event_type = 'aaa_failure' \
                         RETURN count(e)"
                    ).context("prepare count aaa failures")?;
                    conn.execute(
                        &mut stmt,
                        vec![
                            ("device_address", Value::String(device_address.clone())),
                            ("cutoff_ns", Value::Int64(cutoff_ns)),
                        ],
                    )
                    .context("count aaa failures")?
                    .next()
                    .map(|r| read_i64(&r[0]))
                    .unwrap_or(0)
                };
                
                // Count config changes
                let config_changes = {
                    let mut stmt = conn.prepare(
                        "MATCH (d:Device {address: $device_address})-[:HAS_EVENT]->(e:StateChangeEvent) \
                         WHERE e.fired_at_ns > $cutoff_ns AND e.source_type = 'syslog' \
                         AND e.event_type = 'config_change_detail' \
                         RETURN count(e)"
                    ).context("prepare count config changes")?;
                    conn.execute(
                        &mut stmt,
                        vec![
                            ("device_address", Value::String(device_address.clone())),
                            ("cutoff_ns", Value::Int64(cutoff_ns)),
                        ],
                    )
                    .context("count config changes")?
                    .next()
                    .map(|r| read_i64(&r[0]))
                    .unwrap_or(0)
                };
                
                // Count process crashes
                let process_crashes = {
                    let mut stmt = conn.prepare(
                        "MATCH (d:Device {address: $device_address})-[:HAS_EVENT]->(e:StateChangeEvent) \
                         WHERE e.fired_at_ns > $cutoff_ns AND e.source_type = 'syslog' \
                         AND e.event_type = 'process_restart' \
                         RETURN count(e)"
                    ).context("prepare count process crashes")?;
                    conn.execute(
                        &mut stmt,
                        vec![
                            ("device_address", Value::String(device_address.clone())),
                            ("cutoff_ns", Value::Int64(cutoff_ns)),
                        ],
                    )
                    .context("count process crashes")?
                    .next()
                    .map(|r| read_i64(&r[0]))
                    .unwrap_or(0)
                };
                
                // Calculate risk score
                let risk_score = (aaa_failures as f64 * 0.4) 
                    + (config_changes as f64 * 0.3) 
                    + (process_crashes as f64 * 0.3);
                
                // Update or create SecurityPosture node
                let posture_id = format!("posture-{}", device_address);
                let mut stmt = conn.prepare(
                    "MERGE (sp:SecurityPosture {id: $posture_id}) \
                     SET sp.device_address = $device_address, \
                         sp.aaa_failure_count = $aaa_failures, \
                         sp.config_change_count = $config_changes, \
                         sp.process_crash_count = $process_crashes, \
                         sp.risk_score = $risk_score, \
                         sp.updated_at_ns = $updated_at_ns"
                ).context("prepare update security posture")?;
                conn.execute(&mut stmt, vec![
                    ("posture_id", Value::String(posture_id.clone())),
                    ("device_address", Value::String(device_address.clone())),
                    ("aaa_failures", Value::Int64(aaa_failures)),
                    ("config_changes", Value::Int64(config_changes)),
                    ("process_crashes", Value::Int64(process_crashes)),
                    ("risk_score", Value::Double(risk_score)),
                    ("updated_at_ns", Value::Int64(crate::graph::common::now_ns())),
                ])
                .context("update security posture")?;
                
                // Create relationship to device
                let posture_key = posture_id.clone();
                let mut stmt = conn.prepare(
                    "MATCH (d:Device {address: $device_address}), (sp:SecurityPosture {id: $posture_id}) \
                     MERGE (d)-[:HAS_POSTURE {updated_at: $updated_at}]->(sp)"
                ).context("prepare security posture relationship")?;
                conn.execute(&mut stmt, vec![
                    ("device_address", Value::String(device_address.clone())),
                    ("posture_id", Value::String(posture_key)),
                    ("updated_at", Value::Int64(crate::graph::common::now_ns())),
                ])
                .context("create security posture relationship")?;
                
                updated_devices.push(device_address);
            }
            
            Ok::<_, anyhow::Error>(updated_devices)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Query persisted StateChangeEvent nodes with optional filters.
    pub async fn read_events_history(
        &self,
        source: Option<String>,
        device: Option<String>,
        site: Option<String>,
        limit: u32,
    ) -> Result<Vec<StateChangeEventRow>> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("read_events_history connection")?;

            // Build WHERE clauses dynamically (all parts collected before assembly).
            let mut where_parts: Vec<String> = Vec::new();
            if let Some(ref s) = source {
                if !s.is_empty() {
                    where_parts.push(format!("e.source_type = '{}'", s.replace('\'', "''")));
                }
            }
            if let Some(ref d) = device {
                if !d.is_empty() {
                    where_parts.push(format!("e.device_address STARTS WITH '{}'", d.replace('\'', "''")));
                }
            }

            // Site filter requires an OPTIONAL MATCH on Device; added last so it can
            // reference the optional `d` binding.
            let need_site = site.as_deref().is_some_and(|s| !s.is_empty());
            let match_clause = if need_site {
                "MATCH (e:StateChangeEvent) OPTIONAL MATCH (d:Device {address: e.device_address})"
                    .to_string()
            } else {
                "MATCH (e:StateChangeEvent)".to_string()
            };
            if need_site {
                let escaped = site.as_deref().unwrap_or("").replace('\'', "''");
                where_parts.push(format!(
                    "(d IS NULL OR d.site = '{escaped}' OR d.site STARTS WITH '{escaped}')"
                ));
            }

            let where_clause = if where_parts.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", where_parts.join(" AND "))
            };

            let cypher = format!(
                "{match_clause} {where_clause} \
                 RETURN e.id, e.device_address, e.event_type, e.source_type, e.detail, e.occurred_at \
                 ORDER BY e.occurred_at DESC LIMIT {limit}"
            );

            let rows = conn.query(&cypher).context("read_events_history query")?;
            let mut out = Vec::new();
            for row in rows {
                out.push(StateChangeEventRow {
                    id: read_str(&row[0]),
                    device_address: read_str(&row[1]),
                    event_type: read_str(&row[2]),
                    source_type: read_str(&row[3]),
                    detail_json: read_str(&row[4]),
                    occurred_at_ns: read_ts_ns(&row[5]),
                });
            }
            Ok::<_, anyhow::Error>(out)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Return the TRIGGERED_BY StateChangeEvent chain for a DetectionEvent.
    /// Used to build the correlation_chain on IncidentJson.
    pub fn read_triggered_by_chain_sync(
        conn: &Connection<'_>,
        detection_id: &str,
    ) -> Result<Vec<(String, String, String, String, i64)>> {
        let mut stmt = conn
            .prepare(
                "MATCH (e:DetectionEvent {id: $id})-[:TRIGGERED_BY]->(s:StateChangeEvent) \
                 RETURN s.id, s.event_type, s.source_type, s.device_address, s.occurred_at \
                 ORDER BY s.occurred_at ASC",
            )
            .context("prepare triggered_by_chain")?;
        let rows = conn
            .execute(&mut stmt, vec![("id", Value::String(detection_id.to_string()))])
            .context("execute triggered_by_chain")?;
        Ok(rows
            .map(|row| {
                (
                    read_str(&row[0]),
                    read_str(&row[1]),
                    read_str(&row[2]),
                    read_str(&row[3]),
                    read_ts_ns(&row[4]),
                )
            })
            .collect())
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

    /// Return the shared write lock for external callers that need to serialize writes.
    pub fn write_lock(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.write_lock)
    }

    /// Write a single telemetry update to the graph.
    /// Dispatches to a blocking thread so the caller's async task is not blocked.
    pub async fn write(&self, update: TelemetryUpdate) -> Result<()> {
        let db = Arc::clone(&self.db);
        let event_tx = self.event_tx.clone();
        let write_lock = Arc::clone(&self.write_lock);
        let corr_buf = Arc::clone(&self.correlation_buffer);
        let target = update.target.clone();
        tokio::task::spawn_blocking(move || {
            metrics::counter!("bonsai_telemetry_updates_total", "target" => target.clone())
                .increment(1);
            let t0 = Instant::now();
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("single write connection")?;
            let result = write_blocking(&conn, &update, &event_tx, &corr_buf);
            metrics::histogram!("bonsai_graph_write_latency_seconds", "target" => target)
                .record(t0.elapsed().as_secs_f64());
            result
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Write a batch of telemetry updates to the graph using a single block.
    pub async fn write_batch(&self, updates: Vec<TelemetryUpdate>) -> Result<()> {
        let db = Arc::clone(&self.db);
        let event_tx = self.event_tx.clone();
        let write_lock = Arc::clone(&self.write_lock);
        let corr_buf = Arc::clone(&self.correlation_buffer);
        tokio::task::spawn_blocking(move || {
            let t0 = Instant::now();
            let _guard = write_lock.lock().expect("write lock poisoned");
            let batch_len = updates.len();
            let conn = Connection::new(&db).context("batch write connection")?;
            conn.query("BEGIN TRANSACTION").context("begin batch transaction")?;
            let mut errors = 0u32;
            for update in &updates {
                metrics::counter!("bonsai_telemetry_updates_total", "target" => update.target.clone())
                    .increment(1);
                if let Err(error) = write_blocking(&conn, &update, &event_tx, &corr_buf) {
                    // Log individual failures but keep processing — a single bad update
                    // from buggy device firmware must not poison the entire batch (C-1).
                    tracing::warn!(
                        target = %update.target,
                        path = %update.path,
                        %error,
                        "batched graph write failed (skipped)"
                    );
                    errors += 1;
                    metrics::counter!("bonsai_graph_write_errors_total", "target" => update.target.clone())
                        .increment(1);
                }
            }
            if let Err(error) = conn.query("COMMIT").context("commit batch transaction") {
                let _ = conn.query("ROLLBACK");
                tracing::warn!(
                    %error,
                    batch_len,
                    errors,
                    "batch transaction failed; retrying updates individually"
                );
                drop(conn);

                for update in updates {
                    let target = update.target.clone();
                    let path = update.path.clone();
                    let single_conn = Connection::new(&db).context("single-write fallback connection")?;
                    if let Err(single_error) =
                        write_blocking(&single_conn, &update, &event_tx, &corr_buf)
                    {
                        tracing::warn!(
                            target = %target,
                            path = %path,
                            error = %single_error,
                            "individual graph write failed after batch fallback"
                        );
                        metrics::counter!("bonsai_graph_write_errors_total", "target" => target)
                            .increment(1);
                    }
                }
            } else if errors > 0 {
                tracing::debug!(errors, "batch committed with partial write failures");
            }
            metrics::histogram!("bonsai_graph_batch_write_latency_seconds")
                .record(t0.elapsed().as_secs_f64());
            Ok::<(), anyhow::Error>(())
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

    // ── saved queries ─────────────────────────────────────────────────────────

    pub async fn list_saved_queries(&self) -> Result<Vec<SavedQueryRecord>> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("list_saved_queries connection")?;
            let rows = conn
                .query(
                    "MATCH (q:SavedQuery) \
                     RETURN q.id, q.name, q.description, q.cypher, \
                            q.created_at, q.last_run_at, q.last_result_count \
                     ORDER BY q.name",
                )
                .context("list_saved_queries query")?;
            Ok::<_, anyhow::Error>(
                rows.map(|r| SavedQueryRecord {
                    id: read_str(&r[0]),
                    name: read_str(&r[1]),
                    description: read_str(&r[2]),
                    cypher: read_str(&r[3]),
                    created_at_ns: read_ts_ns(&r[4]),
                    last_run_at_ns: read_ts_ns(&r[5]),
                    last_result_count: match &r[6] {
                        Value::Int64(n) => *n,
                        Value::Int32(n) => *n as i64,
                        _ => 0,
                    },
                })
                .collect(),
            )
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn create_saved_query(
        &self,
        name: String,
        description: String,
        cypher: String,
    ) -> Result<SavedQueryRecord> {
        // Validate before hitting the DB
        crate::graph::explorer::validate_query(&cypher).map_err(|e| anyhow::anyhow!("{}", e))?;

        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("create_saved_query connection")?;
            let id = Uuid::new_v4().to_string();
            let now = ts(now_ns());
            let mut stmt = conn
                .prepare(
                    "CREATE (q:SavedQuery {id: $id, name: $name, description: $desc, \
                     cypher: $cypher, created_at: $ts, last_run_at: $ts, \
                     last_result_count: 0})",
                )
                .context("create_saved_query prepare")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("name", Value::String(name.clone())),
                    ("desc", Value::String(description.clone())),
                    ("cypher", Value::String(cypher.clone())),
                    ("ts", now),
                ],
            )
            .context("create_saved_query execute")?;
            Ok::<_, anyhow::Error>(SavedQueryRecord {
                id,
                name,
                description,
                cypher,
                created_at_ns: now_ns(),
                last_run_at_ns: 0,
                last_result_count: 0,
            })
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn delete_saved_query(&self, id: String) -> Result<()> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("delete_saved_query connection")?;
            let mut stmt = conn
                .prepare("MATCH (q:SavedQuery {id: $id}) DELETE q")
                .context("delete_saved_query prepare")?;
            conn.execute(&mut stmt, vec![("id", Value::String(id))])
                .context("delete_saved_query execute")?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn mark_saved_query_run(&self, id: String, result_count: i64) -> Result<()> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("mark_saved_query_run connection")?;
            let now = ts(now_ns());
            let mut stmt = conn
                .prepare(
                    "MATCH (q:SavedQuery {id: $id}) \
                     SET q.last_run_at = $ts, q.last_result_count = $count",
                )
                .context("mark_saved_query_run prepare")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id)),
                    ("ts", now),
                    ("count", Value::Int64(result_count)),
                ],
            )
            .context("mark_saved_query_run execute")?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Upsert a batch of device embedding vectors. Each record is keyed by
    /// (device_address, version); re-inserting the same key overwrites the prior vector.
    pub async fn write_device_embeddings(&self, records: Vec<EmbeddingRecord>) -> Result<()> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock poisoned");
            let conn = Connection::new(&db).context("write_device_embeddings connection")?;
            let mut stmt = conn
                .prepare(
                    "MERGE (e:DeviceEmbedding {id: $id}) \
                     ON CREATE SET \
                       e.device_address = $addr, e.version = $ver, e.algorithm = $algo, \
                       e.dimension = $dim, e.vector_json = $vec, e.computed_at = $ts \
                     ON MATCH SET \
                       e.algorithm = $algo, e.dimension = $dim, \
                       e.vector_json = $vec, e.computed_at = $ts",
                )
                .context("write_device_embeddings prepare")?;
            for rec in &records {
                let vec_json =
                    serde_json::to_string(&rec.vector).context("serialise embedding vector")?;
                let id = format!("{}:{}", rec.device_address, rec.version);
                conn.execute(
                    &mut stmt,
                    vec![
                        ("id", Value::String(id)),
                        ("addr", Value::String(rec.device_address.clone())),
                        ("ver", Value::String(rec.version.clone())),
                        ("algo", Value::String(rec.algorithm.clone())),
                        ("dim", Value::Int64(rec.dimension)),
                        ("vec", Value::String(vec_json)),
                        ("ts", ts(rec.computed_at_ns)),
                    ],
                )
                .context("write_device_embeddings execute")?;
            }
            info!(count = records.len(), "device embeddings upserted");
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Return all stored embeddings for a device, newest first.
    pub async fn list_device_embeddings(
        &self,
        device_address: String,
    ) -> Result<Vec<EmbeddingRecord>> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("list_device_embeddings connection")?;
            let mut stmt = conn
                .prepare(
                    "MATCH (e:DeviceEmbedding {device_address: $addr}) \
                     RETURN e.device_address, e.version, e.algorithm, \
                            e.dimension, e.vector_json, e.computed_at \
                     ORDER BY e.computed_at DESC",
                )
                .context("list_device_embeddings prepare")?;
            let rows: Vec<_> = conn
                .execute(&mut stmt, vec![("addr", Value::String(device_address))])
                .context("list_device_embeddings execute")?
                .collect();
            rows.iter()
                .map(|r| {
                    let vec: Vec<f64> = serde_json::from_str(&read_str(&r[4])).unwrap_or_default();
                    Ok(EmbeddingRecord {
                        device_address: read_str(&r[0]),
                        version: read_str(&r[1]),
                        algorithm: read_str(&r[2]),
                        dimension: match &r[3] {
                            Value::Int64(n) => *n,
                            Value::Int32(n) => *n as i64,
                            _ => 0,
                        },
                        vector: vec,
                        computed_at_ns: read_ts_ns(&r[5]),
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .await
        .context("spawn_blocking panicked")?
    }

    // ── ShunRule CRUD (D4-2 T1) ───────────────────────────────────────────────

    /// Return all ShunRule nodes ordered by created_at descending.
    /// D4-7 T1: List all ConfigItems optionally filtered by config_class.
    pub async fn list_config_items(&self, config_class: Option<String>) -> Result<Vec<ConfigItemRecord>> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("config items connection")?;
            let rows = match &config_class {
                Some(cls) => {
                    let mut stmt = conn.prepare(
                        "MATCH (c:ConfigItem) WHERE c.config_class = $cls \
                         RETURN c.id, c.config_class, c.vendor, c.name, c.version, \
                                c.content_json, c.enabled, c.created_by",
                    )?;
                    conn.execute(&mut stmt, vec![("cls", Value::String(cls.clone()))])?
                }
                None => conn.query(
                    "MATCH (c:ConfigItem) \
                     RETURN c.id, c.config_class, c.vendor, c.name, c.version, \
                            c.content_json, c.enabled, c.created_by",
                )?,
            };
            let mut out = Vec::new();
            for row in rows {
                out.push(ConfigItemRecord {
                    id: read_str(&row[0]),
                    config_class: read_str(&row[1]),
                    vendor: read_str(&row[2]),
                    name: read_str(&row[3]),
                    version: read_str(&row[4]),
                    content_json: read_str(&row[5]),
                    enabled: match &row[6] { lbug::Value::Bool(b) => *b, _ => true },
                    created_by: read_str(&row[7]),
                });
            }
            Ok(out)
        })
        .await
        .context("list_config_items task")?
    }

    /// D4-7 T4: Load raw YAML content strings for a config_class.
    /// Returns `Vec<(name, yaml_content)>` for enabled items only.
    /// The content_json field is a JSON-escaped YAML string; this unwraps it.
    pub async fn load_config_yaml_by_class(&self, config_class: &str) -> Result<Vec<(String, String)>> {
        let items = self.list_config_items(Some(config_class.to_string())).await?;
        let mut out = Vec::new();
        for item in items {
            if !item.enabled { continue; }
            // content_json is a JSON string wrapping YAML text (via serde_json::to_string)
            let yaml: String = serde_json::from_str(&item.content_json)
                .unwrap_or(item.content_json.clone());
            out.push((item.name, yaml));
        }
        Ok(out)
    }

    /// D4-7 T1: Upsert a single ConfigItem (insert or update by id).
    pub async fn upsert_config_item(&self, item: ConfigItemRecord) -> Result<()> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("upsert config item connection")?;
            let now = ts(now_ns());
            let mut stmt = conn.prepare(
                "MERGE (c:ConfigItem {id: $id}) \
                 ON CREATE SET c.config_class = $cls, c.vendor = $vendor, c.name = $name, \
                   c.version = $ver, c.content_json = $content, c.enabled = $enabled, \
                   c.created_by = $created_by, c.created_at = $ts, c.updated_at = $ts \
                 ON MATCH SET c.content_json = $content, c.enabled = $enabled, \
                   c.version = $ver, c.updated_at = $ts",
            )?;
            conn.execute(&mut stmt, vec![
                ("id",         Value::String(item.id)),
                ("cls",        Value::String(item.config_class)),
                ("vendor",     Value::String(item.vendor)),
                ("name",       Value::String(item.name)),
                ("ver",        Value::String(item.version)),
                ("content",    Value::String(item.content_json)),
                ("enabled",    Value::Bool(item.enabled)),
                ("created_by", Value::String(item.created_by)),
                ("ts",         now),
            ])?;
            Ok(())
        })
        .await
        .context("upsert_config_item task")?
    }

    /// D4-7 T2: Boot-time YAML migration.
    /// Scans YAML config directories and upserts each file as a ConfigItem.
    /// Idempotent: re-running does not duplicate entries (MERGE by id).
    /// Called once at startup before serving requests.
    pub async fn migrate_yaml_config(&self, config_base: &str) -> Result<usize> {
        use std::fs;
        use std::path::Path;

        let dirs: &[(&str, &str)] = &[
            ("syslog_pattern",     "syslog_patterns"),
            ("snmp_oid_pattern",   "snmp_oid_patterns"),
            ("gnmi_path_profile",  "path_profiles"),
            ("synthesizer_rule",   "synthesizer_rules"),
            ("vendor_state_mapping", "vendor_state_mapping"),
            ("gnmi_known_issues",  "gnmi_known_issues"),
        ];

        let mut count = 0usize;
        for (config_class, subdir) in dirs {
            let dir_path = Path::new(config_base).join(subdir);
            let Ok(entries) = fs::read_dir(&dir_path) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("yaml") { continue; }
                let Ok(raw) = fs::read_to_string(&path) else { continue };
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
                let id = format!("{config_class}:{stem}");
                // Wrap raw YAML as JSON string for storage
                let content_json = serde_json::to_string(&raw).unwrap_or_default();
                let item = ConfigItemRecord {
                    id,
                    config_class: config_class.to_string(),
                    vendor: String::new(),
                    name: stem,
                    version: "1".to_string(),
                    content_json,
                    enabled: true,
                    created_by: "boot_migration".to_string(),
                };
                self.upsert_config_item(item).await?;
                count += 1;
            }
        }

        // Also migrate playbooks
        let playbook_dir = Path::new("playbooks").join("library");
        if let Ok(entries) = fs::read_dir(&playbook_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("yaml") { continue; }
                let Ok(raw) = fs::read_to_string(&path) else { continue };
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
                let id = format!("playbook:{stem}");
                let content_json = serde_json::to_string(&raw).unwrap_or_default();
                let item = ConfigItemRecord {
                    id,
                    config_class: "playbook".to_string(),
                    vendor: String::new(),
                    name: stem,
                    version: "1".to_string(),
                    content_json,
                    enabled: true,
                    created_by: "boot_migration".to_string(),
                };
                self.upsert_config_item(item).await?;
                count += 1;
            }
        }
        Ok(count)
    }

    pub async fn list_shun_rules(&self) -> Result<Vec<crate::shun::ShunRule>> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("shun rules connection")?;
            let rows = conn
                .query(
                    "MATCH (r:ShunRule) \
                     RETURN r.id, r.scope_type, r.scope_value, r.match_type, r.match_value, \
                            r.action, r.rate_limit_per_min, r.expires_at_ns, r.created_by, \
                            r.created_at_ns, r.enabled \
                     ORDER BY r.created_at_ns DESC",
                )
                .context("list shun rules query")?;
            rows.map(|r| {
                Ok::<_, anyhow::Error>(crate::shun::ShunRule {
                    id: read_str(&r[0]),
                    scope_type: read_str(&r[1]),
                    scope_value: read_str(&r[2]),
                    match_type: read_str(&r[3]),
                    match_value: read_str(&r[4]),
                    action: read_str(&r[5]),
                    rate_limit_per_min: match &r[6] {
                        Value::Int64(n) => *n,
                        Value::Int32(n) => *n as i64,
                        _ => 0,
                    },
                    expires_at_ns: match &r[7] {
                        Value::Int64(n) => *n,
                        Value::Int32(n) => *n as i64,
                        _ => 0,
                    },
                    created_by: read_str(&r[8]),
                    created_at_ns: match &r[9] {
                        Value::Int64(n) => *n,
                        Value::Int32(n) => *n as i64,
                        _ => 0,
                    },
                    enabled: match &r[10] {
                        Value::Bool(b) => *b,
                        _ => true,
                    },
                })
            })
            .collect::<Result<Vec<_>>>()
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Upsert (create or replace) a ShunRule node.
    pub async fn upsert_shun_rule(&self, rule: crate::shun::ShunRule) -> Result<()> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock");
            let conn = Connection::new(&db).context("shun rule write connection")?;
            let mut stmt = conn
                .prepare(
                    "MERGE (r:ShunRule {id: $id}) \
                     SET r.scope_type        = $scope_type, \
                         r.scope_value       = $scope_value, \
                         r.match_type        = $match_type, \
                         r.match_value       = $match_value, \
                         r.action            = $action, \
                         r.rate_limit_per_min = $rate_limit_per_min, \
                         r.expires_at_ns     = $expires_at_ns, \
                         r.created_by        = $created_by, \
                         r.created_at_ns     = $created_at_ns, \
                         r.enabled           = $enabled",
                )
                .context("prepare shun rule upsert")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(rule.id)),
                    ("scope_type", Value::String(rule.scope_type)),
                    ("scope_value", Value::String(rule.scope_value)),
                    ("match_type", Value::String(rule.match_type)),
                    ("match_value", Value::String(rule.match_value)),
                    ("action", Value::String(rule.action)),
                    ("rate_limit_per_min", Value::Int64(rule.rate_limit_per_min)),
                    ("expires_at_ns", Value::Int64(rule.expires_at_ns)),
                    ("created_by", Value::String(rule.created_by)),
                    ("created_at_ns", Value::Int64(rule.created_at_ns)),
                    ("enabled", Value::Bool(rule.enabled)),
                ],
            )
            .context("execute shun rule upsert")?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Delete a ShunRule node by id. Returns true if a row was deleted.
    pub async fn delete_shun_rule(&self, id: String) -> Result<bool> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock");
            let conn = Connection::new(&db).context("shun rule delete connection")?;
            let mut check = conn
                .prepare("MATCH (r:ShunRule {id: $id}) RETURN r.id")
                .context("prepare shun rule check")?;
            let found = conn
                .execute(&mut check, vec![("id", Value::String(id.clone()))])
                .context("execute shun rule check")?
                .next()
                .is_some();
            if found {
                let mut del = conn
                    .prepare("MATCH (r:ShunRule {id: $id}) DELETE r")
                    .context("prepare shun rule delete")?;
                conn.execute(&mut del, vec![("id", Value::String(id))])
                    .context("execute shun rule delete")?;
            }
            Ok::<_, anyhow::Error>(found)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    // ── D4-12 T1: RedundancyGroup helpers ────────────────────────────────────

    pub async fn upsert_redundancy_group(
        &self,
        id: String,
        name: String,
        kind: String,
        protocol: String,
        site_id: String,
        status: String,
        member_count: i32,
        active_count: i32,
        source: String,
    ) -> Result<()> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock");
            let conn = Connection::new(&db).context("upsert_redundancy_group conn")?;
            let now = now_ns();
            let mut stmt = conn.prepare(
                "MERGE (r:RedundancyGroup {id: $id}) \
                 SET r.name=$name, r.kind=$kind, r.protocol=$protocol, \
                     r.site_id=$site, r.status=$status, \
                     r.member_count=$mc, r.active_count=$ac, \
                     r.source=$src, r.discovered_at=coalesce(r.discovered_at,$ts), \
                     r.updated_at=$ts",
            ).context("prepare upsert_redundancy_group")?;
            conn.execute(&mut stmt, vec![
                ("id",     Value::String(id)),
                ("name",   Value::String(name)),
                ("kind",   Value::String(kind)),
                ("protocol", Value::String(protocol)),
                ("site",   Value::String(site_id)),
                ("status", Value::String(status)),
                ("mc",     Value::Int32(member_count)),
                ("ac",     Value::Int32(active_count)),
                ("src",    Value::String(source)),
                ("ts",     Value::Int64(now as i64)),
            ]).context("execute upsert_redundancy_group")?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn write_redundancy_member(
        &self,
        device_address: String,
        group_id: String,
        role: String,
        priority: i32,
        state: String,
    ) -> Result<()> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock");
            let conn = Connection::new(&db).context("write_redundancy_member conn")?;
            let now = now_ns();
            let mut stmt = conn.prepare(
                "MATCH (d:Device {address: $addr}), (g:RedundancyGroup {id: $gid}) \
                 MERGE (d)-[m:MEMBER_OF]->(g) \
                 SET m.role=$role, m.priority=$pri, m.state=$state, m.updated_at=$ts",
            ).context("prepare write_redundancy_member")?;
            conn.execute(&mut stmt, vec![
                ("addr",  Value::String(device_address)),
                ("gid",   Value::String(group_id)),
                ("role",  Value::String(role)),
                ("pri",   Value::Int32(priority)),
                ("state", Value::String(state)),
                ("ts",    Value::Int64(now as i64)),
            ]).context("execute write_redundancy_member")?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn list_redundancy_groups(&self) -> Result<Vec<serde_json::Value>> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("list_redundancy_groups conn")?;
            let rows = conn.query(
                "MATCH (g:RedundancyGroup) \
                 RETURN g.id, g.name, g.kind, g.protocol, g.site_id, \
                        g.status, g.member_count, g.active_count, g.source, g.updated_at \
                 ORDER BY g.name",
            ).context("list_redundancy_groups query")?;
            let mut out = Vec::new();
            for row in rows {
                out.push(serde_json::json!({
                    "id":           read_str(&row[0]),
                    "name":         read_str(&row[1]),
                    "kind":         read_str(&row[2]),
                    "protocol":     read_str(&row[3]),
                    "site_id":      read_str(&row[4]),
                    "status":       read_str(&row[5]),
                    "member_count": read_i64(&row[6]),
                    "active_count": read_i64(&row[7]),
                    "source":       read_str(&row[8]),
                }));
            }
            Ok::<_, anyhow::Error>(out)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Record a GNN inference score. In production mode with score >= threshold,
    /// also writes a DetectionEvent (rule_id="gnn_anomaly") via the write coordinator.
    /// Returns (score_id, fired_detection).
    pub async fn write_gnn_score(
        &self,
        device_address: String,
        score: f64,
        threshold: f64,
        inference_mode: String,
        model_version: String,
        fired_detection: bool,
    ) -> Result<String> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock");
            let conn = Connection::new(&db).context("write_gnn_score conn")?;
            let now = now_ns();
            let id = format!("gnn:{}:{}", device_address, now);
            let mut stmt = conn
                .prepare(
                    "CREATE (g:GnnScore {id: $id, device_address: $addr, score: $score, \
                     threshold: $thr, inference_mode: $mode, model_version: $ver, \
                     fired_detection: $fired, scored_at: $ts})",
                )
                .context("write_gnn_score prepare")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("addr", Value::String(device_address)),
                    ("score", Value::Double(score)),
                    ("thr", Value::Double(threshold)),
                    ("mode", Value::String(inference_mode)),
                    ("ver", Value::String(model_version)),
                    ("fired", Value::Bool(fired_detection)),
                    ("ts", ts(now)),
                ],
            )
            .context("write_gnn_score execute")?;
            Ok::<_, anyhow::Error>(id)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Return calibration stats for the last 24h: count, min, max, p50, p95 anomaly scores.
    pub async fn read_gnn_calibration_stats(&self) -> Result<serde_json::Value> {
        let day_ns: i64 = 86_400_000_000_000;
        let since_ns = (now_ns() / day_ns) * day_ns; // start of today UTC
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("read_gnn_calibration_stats conn")?;
            let cypher = format!(
                "MATCH (g:GnnScore) WHERE g.scored_at >= {since_ns} RETURN g.score, g.device_address, g.inference_mode, g.fired_detection, g.scored_at"
            );
            let rows = conn.query(&cypher).context("read_gnn_calibration_stats query")?;
            let mut scores: Vec<f64> = Vec::new();
            let mut fired_count: u64 = 0;
            let mut sample_rows: Vec<serde_json::Value> = Vec::new();
            for r in rows {
                let s = match &r[0] { Value::Float(f) => *f as f64, Value::Double(f) => *f, _ => 0.0 };
                let fired = matches!(&r[3], Value::Bool(true));
                scores.push(s);
                if fired { fired_count += 1; }
                if sample_rows.len() < 20 {
                    sample_rows.push(serde_json::json!({
                        "device_address": read_str(&r[1]),
                        "score": s,
                        "inference_mode": read_str(&r[2]),
                        "fired_detection": fired,
                        "scored_at_ns": read_ts_ns(&r[4]),
                    }));
                }
            }
            if scores.is_empty() {
                return Ok::<_, anyhow::Error>(serde_json::json!({
                    "count": 0, "min": null, "max": null,
                    "p50": null, "p95": null, "fired_count": 0, "samples": []
                }));
            }
            scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let n = scores.len();
            let p50 = scores[n / 2];
            let p95 = scores[(n as f64 * 0.95) as usize].min(scores[n - 1]);
            Ok::<_, anyhow::Error>(serde_json::json!({
                "count": n,
                "min": scores[0],
                "max": scores[n - 1],
                "p50": p50,
                "p95": p95,
                "fired_count": fired_count,
                "samples": sample_rows,
            }))
        })
        .await
        .context("spawn_blocking panicked")?
    }
}

// ── investigation methods (T3-1) ─────────────────────────────────────────────

impl GraphStore {
    pub async fn create_investigation(
        &self,
        detection_id: String,
        device_address: String,
        trigger: String,
    ) -> Result<InvestigationRecord> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock");
            let conn = Connection::new(&db).context("create_investigation conn")?;
            let id = Uuid::new_v4().to_string();
            let now = now_ns();
            let mut stmt = conn
                .prepare(
                    "CREATE (i:Investigation {id: $id, detection_id: $did, \
                     device_address: $addr, trigger: $trigger, status: 'running', \
                     summary: '', proposal_json: '', tokens_used: 0, cost_usd: 0.0, \
                     started_at: $ts, completed_at: $ts})",
                )
                .context("create_investigation prepare")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("did", Value::String(detection_id.clone())),
                    ("addr", Value::String(device_address.clone())),
                    ("trigger", Value::String(trigger.clone())),
                    ("ts", ts(now)),
                ],
            )
            .context("create_investigation execute")?;
            Ok::<_, anyhow::Error>(InvestigationRecord {
                id,
                detection_id,
                device_address,
                trigger,
                status: "running".into(),
                summary: String::new(),
                proposal_json: String::new(),
                tokens_used: 0,
                cost_usd: 0.0,
                started_at_ns: now,
                completed_at_ns: 0,
            })
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn complete_investigation(
        &self,
        id: String,
        status: String,
        summary: String,
        proposal_json: String,
        tokens_used: i64,
        cost_usd: f64,
    ) -> Result<()> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock");
            let conn = Connection::new(&db).context("complete_investigation conn")?;
            let mut stmt = conn
                .prepare(
                    "MATCH (i:Investigation {id: $id}) \
                     SET i.status = $status, i.summary = $summary, \
                         i.proposal_json = $prop, i.tokens_used = $tok, \
                         i.cost_usd = $cost, i.completed_at = $ts",
                )
                .context("complete_investigation prepare")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id)),
                    ("status", Value::String(status)),
                    ("summary", Value::String(summary)),
                    ("prop", Value::String(proposal_json)),
                    ("tok", Value::Int64(tokens_used)),
                    ("cost", Value::Float(cost_usd as f32)),
                    ("ts", ts(now_ns())),
                ],
            )
            .context("complete_investigation execute")?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    /// Sum cost_usd for all investigations started since UTC midnight today.
    pub async fn query_daily_investigation_cost(&self) -> Result<f64> {
        let day_ns: i64 = 86_400_000_000_000;
        let today_start_ns = (now_ns() / day_ns) * day_ns;
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("query_daily_cost conn")?;
            let cypher = format!(
                "MATCH (i:Investigation) \
                 WHERE i.started_at >= {today_start_ns} \
                 RETURN i.cost_usd"
            );
            let rows = conn.query(&cypher).context("query_daily_cost query")?;
            let total: f64 = rows
                .map(|r| match &r[0] {
                    Value::Float(f) => *f as f64,
                    Value::Double(f) => *f,
                    _ => 0.0,
                })
                .sum();
            Ok::<_, anyhow::Error>(total)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn list_investigations(&self) -> Result<Vec<InvestigationRecord>> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("list_investigations conn")?;
            let rows = conn
                .query(
                    "MATCH (i:Investigation) \
                     RETURN i.id, i.detection_id, i.device_address, i.trigger, i.status, \
                            i.summary, i.proposal_json, i.tokens_used, i.cost_usd, \
                            i.started_at, i.completed_at \
                     ORDER BY i.started_at DESC",
                )
                .context("list_investigations query")?;
            Ok::<_, anyhow::Error>(
                rows.map(|r| InvestigationRecord {
                    id: read_str(&r[0]),
                    detection_id: read_str(&r[1]),
                    device_address: read_str(&r[2]),
                    trigger: read_str(&r[3]),
                    status: read_str(&r[4]),
                    summary: read_str(&r[5]),
                    proposal_json: read_str(&r[6]),
                    tokens_used: match &r[7] {
                        Value::Int64(n) => *n,
                        Value::Int32(n) => *n as i64,
                        _ => 0,
                    },
                    cost_usd: match &r[8] {
                        Value::Float(f) => *f as f64,
                        _ => 0.0,
                    },
                    started_at_ns: read_ts_ns(&r[9]),
                    completed_at_ns: read_ts_ns(&r[10]),
                })
                .collect(),
            )
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn get_investigation(&self, id: String) -> Result<Option<InvestigationRecord>> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("get_investigation conn")?;
            let mut stmt = conn
                .prepare(
                    "MATCH (i:Investigation {id: $id}) \
                     RETURN i.id, i.detection_id, i.device_address, i.trigger, i.status, \
                            i.summary, i.proposal_json, i.tokens_used, i.cost_usd, \
                            i.started_at, i.completed_at",
                )
                .context("get_investigation prepare")?;
            let rows: Vec<_> = conn
                .execute(&mut stmt, vec![("id", Value::String(id))])
                .context("get_investigation execute")?
                .collect();
            Ok::<_, anyhow::Error>(rows.into_iter().next().map(|r| InvestigationRecord {
                id: read_str(&r[0]),
                detection_id: read_str(&r[1]),
                device_address: read_str(&r[2]),
                trigger: read_str(&r[3]),
                status: read_str(&r[4]),
                summary: read_str(&r[5]),
                proposal_json: read_str(&r[6]),
                tokens_used: match &r[7] {
                    Value::Int64(n) => *n,
                    Value::Int32(n) => *n as i64,
                    _ => 0,
                },
                cost_usd: match &r[8] {
                    Value::Float(f) => *f as f64,
                    _ => 0.0,
                },
                started_at_ns: read_ts_ns(&r[9]),
                completed_at_ns: read_ts_ns(&r[10]),
            }))
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn add_tool_call(
        &self,
        investigation_id: String,
        tool_name: String,
        input_json: String,
        output_json: String,
    ) -> Result<ToolCallRecord> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock");
            let conn = Connection::new(&db).context("add_tool_call conn")?;
            let id = Uuid::new_v4().to_string();
            let now = now_ns();
            let mut stmt = conn
                .prepare(
                    "CREATE (t:AgentToolCall {id: $id, investigation_id: $iid, \
                     tool_name: $name, input_json: $inp, output_json: $out, called_at: $ts})",
                )
                .context("add_tool_call prepare")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("iid", Value::String(investigation_id.clone())),
                    ("name", Value::String(tool_name.clone())),
                    ("inp", Value::String(input_json.clone())),
                    ("out", Value::String(output_json.clone())),
                    ("ts", ts(now)),
                ],
            )
            .context("add_tool_call execute")?;
            // Edge: Investigation -[:HAS_TOOL_CALL]-> AgentToolCall
            let mut edge_stmt = conn
                .prepare(
                    "MATCH (i:Investigation {id: $iid}), (t:AgentToolCall {id: $tid}) \
                     CREATE (i)-[:HAS_TOOL_CALL]->(t)",
                )
                .context("add_tool_call edge prepare")?;
            conn.execute(
                &mut edge_stmt,
                vec![
                    ("iid", Value::String(investigation_id.clone())),
                    ("tid", Value::String(id.clone())),
                ],
            )
            .context("add_tool_call edge execute")?;
            Ok::<_, anyhow::Error>(ToolCallRecord {
                id,
                investigation_id,
                tool_name,
                input_json,
                output_json,
                called_at_ns: now,
            })
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn list_tool_calls(&self, investigation_id: String) -> Result<Vec<ToolCallRecord>> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("list_tool_calls conn")?;
            let mut stmt = conn
                .prepare(
                    "MATCH (t:AgentToolCall {investigation_id: $iid}) \
                     RETURN t.id, t.investigation_id, t.tool_name, \
                            t.input_json, t.output_json, t.called_at \
                     ORDER BY t.called_at",
                )
                .context("list_tool_calls prepare")?;
            let rows: Vec<_> = conn
                .execute(&mut stmt, vec![("iid", Value::String(investigation_id))])
                .context("list_tool_calls execute")?
                .collect();
            Ok::<_, anyhow::Error>(
                rows.iter()
                    .map(|r| ToolCallRecord {
                        id: read_str(&r[0]),
                        investigation_id: read_str(&r[1]),
                        tool_name: read_str(&r[2]),
                        input_json: read_str(&r[3]),
                        output_json: read_str(&r[4]),
                        called_at_ns: read_ts_ns(&r[5]),
                    })
                    .collect(),
            )
        })
        .await
        .context("spawn_blocking panicked")?
    }

    // ── D4-8 T2: Feedback methods ────────────────────────────────────────────

    pub async fn add_investigation_feedback(
        &self,
        investigation_id: String,
        rating: String,
        comment: String,
        operator: String,
    ) -> Result<FeedbackRecord> {
        let db = Arc::clone(&self.db);
        let write_lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = write_lock.lock().expect("write lock");
            let conn = Connection::new(&db).context("add_feedback conn")?;
            let id = Uuid::new_v4().to_string();
            let now = now_ns();
            let mut stmt = conn
                .prepare(
                    "CREATE (f:InvestigationFeedback {id: $id, investigation_id: $iid, \
                     rating: $rating, comment: $comment, operator: $op, created_at: $ts})",
                )
                .context("add_feedback prepare")?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("iid", Value::String(investigation_id.clone())),
                    ("rating", Value::String(rating.clone())),
                    ("comment", Value::String(comment.clone())),
                    ("op", Value::String(operator.clone())),
                    ("ts", ts(now)),
                ],
            )
            .context("add_feedback execute")?;
            // Edge: Investigation -[:HAS_FEEDBACK]-> InvestigationFeedback
            let mut edge_stmt = conn
                .prepare(
                    "MATCH (i:Investigation {id: $iid}), (f:InvestigationFeedback {id: $fid}) \
                     CREATE (i)-[:HAS_FEEDBACK]->(f)",
                )
                .context("add_feedback edge prepare")?;
            conn.execute(
                &mut edge_stmt,
                vec![
                    ("iid", Value::String(investigation_id.clone())),
                    ("fid", Value::String(id.clone())),
                ],
            )
            .context("add_feedback edge execute")?;
            Ok::<_, anyhow::Error>(FeedbackRecord {
                id,
                investigation_id,
                rating,
                comment,
                operator,
                created_at_ns: now,
            })
        })
        .await
        .context("spawn_blocking panicked")?
    }

    pub async fn investigation_accuracy(&self) -> Result<InvestigationAccuracy> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).context("investigation_accuracy conn")?;
            let total_investigations: usize = conn
                .query(
                    "MATCH (i:Investigation) WHERE i.status = 'completed' RETURN count(i)",
                )
                .context("accuracy: total_inv")?
                .next()
                .map(|r| match &r[0] { Value::Int64(n) => *n as usize, _ => 0 })
                .unwrap_or(0);
            let feedback_rows: Vec<String> = conn
                .query("MATCH (f:InvestigationFeedback) RETURN f.rating")
                .context("accuracy: feedback")?
                .map(|r| read_str(&r[0]))
                .collect();
            let total_feedback = feedback_rows.len();
            let positive = feedback_rows.iter().filter(|r| r == &"positive").count();
            let negative = feedback_rows.iter().filter(|r| r == &"negative").count();
            let precision_pct = if total_feedback == 0 {
                0.0
            } else {
                (positive as f64 / total_feedback as f64 * 100.0 * 10.0).round() / 10.0
            };
            Ok::<_, anyhow::Error>(InvestigationAccuracy {
                total_investigations,
                total_feedback,
                positive,
                negative,
                precision_pct,
            })
        })
        .await
        .context("spawn_blocking panicked")?
    }
}

// ── blocking write helpers ────────────────────────────────────────────────────

fn write_blocking(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
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
            write_interface(conn, update, &if_name)
        }
        TelemetryEvent::InterfaceSummary { if_name } => {
            write_interface_summary(conn, update, &if_name)
        }
        TelemetryEvent::BgpNeighborState {
            peer_address,
            state_value,
        } => write_bgp_neighbor(
            conn,
            update,
            &peer_address,
            state_value.as_ref().unwrap_or(&update.value),
            event_tx,
            corr_buf,
        ),
        TelemetryEvent::BfdSessionState {
            if_name,
            local_discriminator,
            state_value,
        } => write_bfd_session(
            conn,
            update,
            &if_name,
            &local_discriminator,
            state_value.as_ref().unwrap_or(&update.value),
            event_tx,
            corr_buf,
        ),
        TelemetryEvent::IsisAdjacencyState { system_id, if_name } => {
            write_isis_adjacency(conn, update, &system_id, &if_name, &update.value, "gnmi", event_tx, corr_buf)
        }
        TelemetryEvent::LldpNeighbor {
            local_if,
            neighbor_id,
            state_value,
        } => write_lldp_neighbor(
            conn,
            update,
            &local_if,
            &neighbor_id,
            state_value.as_ref().unwrap_or(&update.value),
        ),
        TelemetryEvent::InterfaceOperStatus {
            if_name,
            oper_status,
        } => emit_oper_status_event(conn, update, &if_name, &oper_status, event_tx, corr_buf),
        TelemetryEvent::SyslogEvent { category } => {
            write_syslog_state_change_event(conn, update, &category, event_tx, corr_buf)
        }
        TelemetryEvent::SyslogFact { fact_type } => {
            write_syslog_fact_event(conn, update, &fact_type, event_tx, corr_buf)
        }
        TelemetryEvent::SnmpTrap { event_type } => {
            write_signal_state_change_event(conn, update, &event_type, "snmp", event_tx, corr_buf)
        }
        TelemetryEvent::SnmpFact { fact_type } => {
            write_snmp_fact_event(conn, update, &fact_type, event_tx, corr_buf)
        }
        TelemetryEvent::ConfigChange { yang_path, new_value } => {
            write_config_change_event(conn, update, &yang_path, &new_value, event_tx)
        }
        TelemetryEvent::BmpPeerState => write_bmp_peer_state(conn, update, event_tx, corr_buf),
        TelemetryEvent::BmpRouteMonitoring => write_bmp_route_monitoring(conn, update, event_tx, corr_buf),
        TelemetryEvent::BmpInitiation => write_bmp_initiation(conn, update),
        TelemetryEvent::BmpStatisticsReport => write_bmp_statistics_report(conn, update, event_tx, corr_buf),
        TelemetryEvent::BgpLsState => write_bgp_ls_state(conn, update, event_tx, corr_buf),
        TelemetryEvent::EnvSensor { component_name, sensor_type } =>
            write_env_sensor(conn, update, &component_name, &sensor_type, event_tx, corr_buf),
        TelemetryEvent::OpticsDiagnostics { if_name } =>
            write_optics_diagnostics(conn, update, &if_name),
        TelemetryEvent::InterfaceDescription { if_name, description } =>
            write_interface_description(conn, update, &if_name, &description),
        TelemetryEvent::ServiceEndpoint { if_name, service_type, service_name, confidence } =>
            write_service_endpoint(conn, update, &if_name, &service_type, &service_name, confidence),
        TelemetryEvent::QoSPolicyChange { policy_name, action, interface_name } =>
            write_qos_policy_change(conn, update, &policy_name, &action, interface_name.as_deref()),
        TelemetryEvent::OtlpSpan {
            service_name,
            peer_address,
        } => write_otlp_span(conn, update, &service_name, &peer_address, event_tx),
        TelemetryEvent::OtlpMetrics {
            service_name,
            metric_name,
            value,
            peer_address,
        } => write_otlp_metrics(conn, update, &service_name, &metric_name, value, &peer_address),
        TelemetryEvent::NetflowRecord {
            exporter_address,
            src_address,
            dst_address,
            dst_port,
            protocol,
            bytes_per_sec,
            packets_per_sec,
        } => write_netflow_record(
            conn,
            update,
            &exporter_address,
            &src_address,
            &dst_address,
            dst_port,
            &protocol,
            bytes_per_sec,
            packets_per_sec,
            event_tx,
            corr_buf,
        ),
        TelemetryEvent::SflowRecord {
            exporter_address,
            src_address,
            dst_address,
            dst_port,
            protocol,
            bytes_per_sec,
            packets_per_sec,
            sampling_rate,
        } => write_sflow_record(
            conn,
            update,
            &exporter_address,
            &src_address,
            &dst_address,
            dst_port,
            &protocol,
            bytes_per_sec,
            packets_per_sec,
            sampling_rate,
            event_tx,
            corr_buf,
        ),
        TelemetryEvent::SflowCounters {
            exporter_address,
            if_index,
            if_speed,
            in_octets,
            out_octets,
            in_errors,
            out_errors,
            in_discards,
            out_discards,
        } => write_sflow_counters(
            conn,
            update,
            &exporter_address,
            if_index,
            if_speed,
            in_octets,
            out_octets,
            in_errors,
            out_errors,
            in_discards,
            out_discards,
        ),
        TelemetryEvent::OpticalChannel { channel_name } => {
            write_optical_channel(conn, update, &channel_name)
        }
        TelemetryEvent::PowerUnit { unit_name, kind } => {
            write_power_unit(conn, update, &unit_name, &kind)
        }
        TelemetryEvent::Ignored => Ok(()),
    }
}

fn write_optical_channel(
    conn: &Connection<'_>,
    u: &TelemetryUpdate,
    channel_name: &str,
) -> Result<()> {
    let bare = crate::registry::strip_port(&u.target);
    let id = format!("{}:{}", bare, channel_name);
    let now = ts(u.timestamp_ns);
    upsert_device(conn, &u.target, &u.vendor, &u.hostname, "", "", now.clone())?;

    let v = &u.value;
    let freq = json_f64(v, "frequency") as f64
        / 1_000_000.0  // Hz → GHz
        + json_f64(v, "frequency-ghz") as f64;
    let tx = json_f64(v, "output-power") + json_f64(v, "tx-power-dbm");
    let rx = json_f64(v, "input-power") + json_f64(v, "rx-power-dbm");
    let snr = json_f64(v, "snr") + json_f64(v, "snr-db");
    let ber = json_f64(v, "post-fec-ber") + json_f64(v, "ber");
    let admin = json_str(v, "admin-state").to_string();
    let oper = json_str(v, "oper-state").to_string();
    let op_mode = json_str(v, "operational-mode").to_string();

    let mut stmt = conn
        .prepare(
            "MERGE (c:OpticalChannel {id: $id}) \
             ON CREATE SET c.device_address = $addr, c.channel_name = $name, \
               c.operational_mode = $mode, c.frequency_ghz = $freq, \
               c.tx_power_dbm = $tx, c.rx_power_dbm = $rx, c.snr_db = $snr, \
               c.ber = $ber, c.admin_state = $admin, c.oper_state = $oper, \
               c.updated_at = $ts \
             ON MATCH SET c.frequency_ghz = $freq, c.tx_power_dbm = $tx, \
               c.rx_power_dbm = $rx, c.snr_db = $snr, c.ber = $ber, \
               c.oper_state = CASE WHEN $oper <> '' THEN $oper ELSE c.oper_state END, \
               c.updated_at = $ts",
        )
        .context("prepare OpticalChannel upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("addr", Value::String(bare.to_string())),
            ("name", Value::String(channel_name.to_string())),
            ("mode", Value::String(op_mode)),
            ("freq", Value::Double(freq)),
            ("tx", Value::Double(tx)),
            ("rx", Value::Double(rx)),
            ("snr", Value::Double(snr)),
            ("ber", Value::Double(ber)),
            ("admin", Value::String(admin)),
            ("oper", Value::String(oper)),
            ("ts", now),
        ],
    )
    .context("execute OpticalChannel upsert")?;

    let mut edge = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (c:OpticalChannel {id: $id}) \
             MERGE (d)-[:HAS_OPTICAL_CHANNEL]->(c)",
        )
        .context("prepare HAS_OPTICAL_CHANNEL merge")?;
    conn.execute(
        &mut edge,
        vec![
            ("addr", Value::String(bare.to_string())),
            ("id", Value::String(id)),
        ],
    )
    .context("execute HAS_OPTICAL_CHANNEL merge")?;
    Ok(())
}

fn write_power_unit(
    conn: &Connection<'_>,
    u: &TelemetryUpdate,
    unit_name: &str,
    kind: &str,
) -> Result<()> {
    let bare = crate::registry::strip_port(&u.target);
    let id = format!("pdu:{}:{}", bare, unit_name);
    let now = ts(u.timestamp_ns);
    upsert_device(conn, &u.target, &u.vendor, &u.hostname, "", "", now.clone())?;

    let v = &u.value;
    let outlet_count = json_i64(v, "outlet-count") + json_i64(v, "outlets-total");
    let outlet_active = json_i64(v, "outlets-active") + json_i64(v, "active-outlets");
    let load = json_f64(v, "load-watts") + json_f64(v, "active-power");
    let voltage = json_f64(v, "input-voltage") + json_f64(v, "voltage");
    let status = json_str(v, "status").to_string();

    let mut stmt = conn
        .prepare(
            "MERGE (p:PowerUnit {id: $id}) \
             ON CREATE SET p.device_address = $addr, p.name = $name, p.kind = $kind, \
               p.outlet_count = $oc, p.outlet_active = $oa, \
               p.load_watts = $load, p.input_voltage = $volt, \
               p.status = $status, p.updated_at = $ts \
             ON MATCH SET p.outlet_active = $oa, p.load_watts = $load, \
               p.input_voltage = $volt, p.status = $status, p.updated_at = $ts",
        )
        .context("prepare PowerUnit upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("addr", Value::String(bare.to_string())),
            ("name", Value::String(unit_name.to_string())),
            ("kind", Value::String(kind.to_string())),
            ("oc", Value::Int64(outlet_count)),
            ("oa", Value::Int64(outlet_active)),
            ("load", Value::Double(load)),
            ("volt", Value::Double(voltage)),
            ("status", Value::String(status)),
            ("ts", now),
        ],
    )
    .context("execute PowerUnit upsert")?;

    let mut edge = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (p:PowerUnit {id: $id}) \
             MERGE (d)-[:POWERED_BY]->(p)",
        )
        .context("prepare POWERED_BY merge")?;
    conn.execute(
        &mut edge,
        vec![
            ("addr", Value::String(bare.to_string())),
            ("id", Value::String(id)),
        ],
    )
    .context("execute POWERED_BY merge")?;
    Ok(())
}

fn write_config_change_event(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    yang_path: &str,
    new_value: &serde_json::Value,
    event_tx: &broadcast::Sender<BonsaiEvent>,
) -> Result<()> {
    let bare = crate::registry::strip_port(&update.target);
    let now = ts(update.timestamp_ns);
    upsert_device(conn, &update.target, &update.vendor, &update.hostname, "", "", now.clone())?;

    let id = format!(
        "{}::{}::{}",
        bare,
        yang_path,
        update.timestamp_ns
    );
    let new_val_str = new_value.to_string();
    let mut stmt = conn
        .prepare(
            "MERGE (c:ConfigChange {id: $id}) \
             ON CREATE SET c.device_address = $addr, c.source = 'gnmi_realtime', \
               c.trigger = $yang_path, c.summary = $new_val, \
               c.previous_snapshot_id = '', c.current_snapshot_id = '', \
               c.previous_hash = '', c.current_hash = '', \
               c.added_lines = 0, c.removed_lines = 0, c.changed_at = $ts \
             ON MATCH SET c.summary = $new_val, c.changed_at = $ts",
        )
        .context("prepare ConfigChange upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("addr", Value::String(bare.to_string())),
            ("yang_path", Value::String(yang_path.to_string())),
            ("new_val", Value::String(new_val_str.clone())),
            ("ts", now),
        ],
    )
    .context("execute ConfigChange upsert")?;

    let mut edge = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (c:ConfigChange {id: $id}) \
             MERGE (d)-[:HAS_CONFIG_CHANGE]->(c)",
        )
        .context("prepare HAS_CONFIG_CHANGE merge")?;
    conn.execute(
        &mut edge,
        vec![
            ("addr", Value::String(bare.to_string())),
            ("id", Value::String(id)),
        ],
    )
    .context("execute HAS_CONFIG_CHANGE merge")?;

    let evt = BonsaiEvent {
        device_address: bare.to_string(),
        event_type: "config_change_event".to_string(),
        detail_json: serde_json::json!({
            "yang_path": yang_path,
            "new_value": new_value,
            "previous_value": null,
        })
        .to_string(),
        occurred_at_ns: update.timestamp_ns,
        state_change_event_id: String::new(),
        source_type: "gnmi".to_string(),
    };
    let _ = event_tx.send(evt);
    Ok(())
}

fn write_otlp_span(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    service_name: &str,
    peer_address: &str,
    event_tx: &broadcast::Sender<BonsaiEvent>,
) -> Result<()> {
    tracing::debug!(
        target = %update.target,
        service_name,
        peer_address,
        timestamp_ns = update.timestamp_ns,
        "write_otlp_span invoked"
    );
    if service_name.is_empty() {
        let _ = event_tx.send(BonsaiEvent {
            device_address: crate::registry::strip_port(&update.target).to_string(),
            event_type: "otlp_span_event".to_string(),
            detail_json: serde_json::json!({
                "service_name": service_name,
                "peer_address": peer_address,
            })
            .to_string(),
            occurred_at_ns: update.timestamp_ns,
            state_change_event_id: String::new(),
            source_type: "otlp".to_string(),
        });
        return Ok(());
    }

    let now = ts(update.timestamp_ns);
    let app_id = format!("app:{service_name}");

    // Track F1: Upsert Application node.
    let mut app_stmt = conn
        .prepare(
            "MERGE (a:Application {id: $id}) \
             ON CREATE SET a.name = $name, a.criticality = 'unknown', \
               a.owner_group = '', a.source_name = $src, a.updated_at = $ts \
             ON MATCH SET a.updated_at = $ts",
        )
        .context("prepare Application upsert")?;
    conn.execute(
        &mut app_stmt,
        vec![
            ("id",   Value::String(app_id.clone())),
            ("name", Value::String(service_name.to_string())),
            ("src",  Value::String("otlp".to_string())),
            ("ts",   now),
        ],
    )
    .context("execute Application upsert")?;
    tracing::debug!(app_id = %app_id, "write_otlp_span upserted application");

    // RUNS_SERVICE: Device → Application (if peer_address matches a Device).
    if !peer_address.is_empty() {
        let mut dev_edge = conn
            .prepare(
                "MATCH (d:Device) WHERE d.address STARTS WITH $pfx \
                 MATCH (a:Application {id: $aid}) \
                 MERGE (d)-[:RUNS_SERVICE]->(a)",
            )
            .context("prepare RUNS_SERVICE merge")?;
        conn.execute(
            &mut dev_edge,
            vec![
                ("pfx", Value::String(peer_address.to_string())),
                ("aid", Value::String(app_id.clone())),
            ],
        )
        .context("execute RUNS_SERVICE merge")?;
        tracing::debug!(peer_address, app_id = %app_id, "write_otlp_span attempted RUNS_SERVICE merge");

        // HOST_RUNS_SERVICE: HostEndpoint → Application (if peer_address matches a HostEndpoint).
        let mut host_edge = conn
            .prepare(
                "MATCH (h:HostEndpoint {ip: $ip}) \
                 MATCH (a:Application {id: $aid}) \
                 MERGE (h)-[:HOST_RUNS_SERVICE]->(a)",
            )
            .context("prepare HOST_RUNS_SERVICE merge")?;
        conn.execute(
            &mut host_edge,
            vec![
                ("ip",  Value::String(peer_address.to_string())),
                ("aid", Value::String(app_id.clone())),
            ],
        )
        .context("execute HOST_RUNS_SERVICE merge")?;
        tracing::debug!(peer_address, app_id = %app_id, "write_otlp_span attempted HOST_RUNS_SERVICE merge");
    }

    // D4-10 T3: OTLP trace + network event temporal correlation.
    // If the peer device (or any device hosting this application) has an active
    // DetectionEvent within ±30s, create an APP_IMPACTED_BY_NETWORK edge.
    if !peer_address.is_empty() {
        let window_ns = 30_000_000_000_i64;
        let ts_lo = update.timestamp_ns - window_ns;
        let ts_hi = update.timestamp_ns + window_ns;
        let mut corr_select = conn.prepare(
            "MATCH (d:Device)-[:TRIGGERED]->(de:DetectionEvent) \
             WHERE d.address STARTS WITH $pfx \
               AND de.severity IN ['critical', 'high'] \
               AND de.fired_at > $lo AND de.fired_at < $hi \
             RETURN de.id"
        ).context("prepare APP_IMPACTED_BY_NETWORK correlation select")?;
        let mut rows = conn.execute(
            &mut corr_select,
            vec![
                ("pfx", Value::String(peer_address.to_string())),
                ("lo", ts(ts_lo)),
                ("hi", ts(ts_hi)),
            ],
        )
        .context("execute APP_IMPACTED_BY_NETWORK correlation select")?;
        let mut detection_ids = Vec::new();
        while let Some(row) = rows.next() {
            if let Value::String(id) = &row[0] {
                detection_ids.push(id.clone());
            }
        }
        if !detection_ids.is_empty() {
            let mut attach_stmt = conn.prepare(
                "MATCH (a:Application {id: $aid}), (de:DetectionEvent {id: $did}) \
                 MERGE (a)-[:APP_IMPACTED_BY_NETWORK]->(de)"
            ).context("prepare APP_IMPACTED_BY_NETWORK attach")?;
            for did in detection_ids {
                conn.execute(
                    &mut attach_stmt,
                    vec![
                        ("aid", Value::String(app_id.clone())),
                        ("did", Value::String(did)),
                    ],
                )
                .context("execute APP_IMPACTED_BY_NETWORK attach")?;
            }
        }
    }

    let _ = event_tx.send(BonsaiEvent {
        device_address: crate::registry::strip_port(&update.target).to_string(),
        event_type: "otlp_span_event".to_string(),
        detail_json: serde_json::json!({
            "service_name": service_name,
            "peer_address": peer_address,
            "app_id": app_id,
        })
        .to_string(),
        occurred_at_ns: update.timestamp_ns,
        state_change_event_id: String::new(),
        source_type: "otlp".to_string(),
    });
    Ok(())
}

/// D4-10 T2: Write OTLP metric data point to Application node.
/// Maps well-known OpenTelemetry metric names to typed columns:
///   system.cpu.utilization / process.cpu.utilization → cpu_pct
///   system.memory.usage / process.memory.usage      → memory_mb (bytes→MB)
///   http.server.request_count / rpc.server.requests  → req_per_sec
///   http.server.error_count                          → error_rate
/// Unknown metric names are stored in the generic `metric_json` column.
fn write_otlp_metrics(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    service_name: &str,
    metric_name: &str,
    value: f64,
    peer_address: &str,
) -> Result<()> {
    if service_name.is_empty() {
        return Ok(());
    }
    let app_id = if !peer_address.is_empty() {
        format!("{}:{}", peer_address, service_name)
    } else {
        service_name.to_string()
    };
    let now = ts(update.timestamp_ns);

    // Upsert Application node (CREATE if new, no-op ON MATCH for identity fields)
    let mut stmt = conn
        .prepare(
            "MERGE (a:Application {id: $id}) \
             ON CREATE SET a.name = $name, a.source = $src, a.updated_at = $ts \
             ON MATCH SET a.updated_at = $ts",
        )
        .context("prepare Application upsert (metrics)")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id",   Value::String(app_id.clone())),
            ("name", Value::String(service_name.to_string())),
            ("src",  Value::String("otlp".to_string())),
            ("ts",   now.clone()),
        ],
    )
    .context("execute Application upsert (metrics)")?;

    // Map metric name to Application column
    let set_clause = match metric_name {
        "system.cpu.utilization" | "process.cpu.utilization" => {
            let pct = (value * 100.0).clamp(0.0, 100.0);
            let mut s = conn.prepare(
                "MATCH (a:Application {id: $id}) SET a.cpu_pct = $v, a.updated_at = $ts",
            )?;
            conn.execute(&mut s, vec![
                ("id", Value::String(app_id.clone())),
                ("v",  Value::Double(pct)),
                ("ts", now.clone()),
            ])?;
            None::<()>
        }
        "system.memory.usage" | "process.memory.usage" => {
            let mb = (value / 1_048_576.0).max(0.0);
            let mut s = conn.prepare(
                "MATCH (a:Application {id: $id}) SET a.memory_mb = $v, a.updated_at = $ts",
            )?;
            conn.execute(&mut s, vec![
                ("id", Value::String(app_id.clone())),
                ("v",  Value::Double(mb)),
                ("ts", now.clone()),
            ])?;
            None
        }
        "http.server.request_count" | "rpc.server.requests" | "http.server.requests_total" => {
            let mut s = conn.prepare(
                "MATCH (a:Application {id: $id}) SET a.req_per_sec = $v, a.updated_at = $ts",
            )?;
            conn.execute(&mut s, vec![
                ("id", Value::String(app_id.clone())),
                ("v",  Value::Double(value)),
                ("ts", now.clone()),
            ])?;
            None
        }
        "http.server.error_count" | "http.server.errors_total" => {
            let mut s = conn.prepare(
                "MATCH (a:Application {id: $id}) SET a.error_rate = $v, a.updated_at = $ts",
            )?;
            conn.execute(&mut s, vec![
                ("id", Value::String(app_id.clone())),
                ("v",  Value::Double(value)),
                ("ts", now.clone()),
            ])?;
            None
        }
        _ => {
            // Store unknown metric as JSON blob in metric_json column
            let blob = serde_json::json!({"metric": metric_name, "value": value}).to_string();
            let mut s = conn.prepare(
                "MATCH (a:Application {id: $id}) SET a.metric_json = $v, a.updated_at = $ts",
            )?;
            conn.execute(&mut s, vec![
                ("id", Value::String(app_id.clone())),
                ("v",  Value::String(blob)),
                ("ts", now),
            ])
            .context("execute Application metric_json update")?;
            None
        }
    };
    let _ = set_clause;
    Ok(())
}

fn write_netflow_record(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    exporter_address: &str,
    src_address: &str,
    dst_address: &str,
    dst_port: i64,
    protocol: &str,
    bytes_per_sec: f64,
    packets_per_sec: f64,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
    let id = format!("{src_address}:{dst_address}:{dst_port}:{protocol}");
    upsert_app_flow(
        conn,
        &id,
        exporter_address,
        src_address,
        dst_address,
        dst_port,
        protocol,
        bytes_per_sec,
        packets_per_sec,
        update.timestamp_ns,
    )?;

    // Register exporter IP as DeviceAddress so it resolves to any Device node
    // whose primary bare IP matches — covers gNMI-registered devices.
    let exporter_bare = crate::registry::strip_port(exporter_address);
    let _ = upsert_device_address(conn, exporter_bare, "netflow", ts(update.timestamp_ns));

    // Track A2: CARRIES_FLOW — link the exporting Device to this AppFlow.
    // Now uses exact bare-IP match since Device.address is always the bare IP.
    // Silent no-op if the Device node doesn't exist yet (collector-only mode).
    let mut carries = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (f:AppFlow {id: $fid}) \
             MERGE (d)-[:CARRIES_FLOW]->(f)",
        )
        .context("prepare CARRIES_FLOW merge")?;
    conn.execute(
        &mut carries,
        vec![
            ("addr", Value::String(exporter_bare.to_string())),
            ("fid", Value::String(id.clone())),
        ],
    )
    .context("execute CARRIES_FLOW merge")?;

    // Track C1: SRC_HOST / DST_HOST — link AppFlow to HostEndpoints if they exist.
    // Completely silent when no HostEndpoint nodes are present (SP deployments).
    let mut src_host = conn
        .prepare(
            "MATCH (h:HostEndpoint {ip: $ip}), (f:AppFlow {id: $fid}) \
             MERGE (f)-[:SRC_HOST]->(h)",
        )
        .context("prepare SRC_HOST merge")?;
    conn.execute(
        &mut src_host,
        vec![
            ("ip", Value::String(src_address.to_string())),
            ("fid", Value::String(id.clone())),
        ],
    )
    .context("execute SRC_HOST merge")?;

    let mut dst_host = conn
        .prepare(
            "MATCH (h:HostEndpoint {ip: $ip}), (f:AppFlow {id: $fid}) \
             MERGE (f)-[:DST_HOST]->(h)",
        )
        .context("prepare DST_HOST merge")?;
    conn.execute(
        &mut dst_host,
        vec![
            ("ip", Value::String(dst_address.to_string())),
            ("fid", Value::String(id.clone())),
        ],
    )
    .context("execute DST_HOST merge")?;

    emit_flow_utilization_event(conn, update, exporter_address, &id, bytes_per_sec, event_tx, corr_buf);

    let evt = BonsaiEvent {
        device_address: exporter_address.to_string(),
        event_type: "app_flow_event".to_string(),
        detail_json: serde_json::json!({
            "flow_id": id,
            "exporter_address": exporter_address,
            "src_address": src_address,
            "dst_address": dst_address,
            "dst_port": dst_port,
            "protocol": protocol,
            "bytes_per_sec": bytes_per_sec,
            "packets_per_sec": packets_per_sec,
        })
        .to_string(),
        occurred_at_ns: update.timestamp_ns,
        state_change_event_id: String::new(),
        source_type: "netflow".to_string(),
    };
    let _ = event_tx.send(evt);
    Ok(())
}

/// D4-5 T1: sFlow sampled flow record — same AppFlow/CARRIES_FLOW path as NetFlow.
/// Sampling rate is recorded in the BonsaiEvent detail for downstream rate scaling.
#[allow(clippy::too_many_arguments)]
fn write_sflow_record(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    exporter_address: &str,
    src_address: &str,
    dst_address: &str,
    dst_port: i64,
    protocol: &str,
    bytes_per_sec: f64,
    packets_per_sec: f64,
    sampling_rate: u32,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
    let id = format!("{src_address}:{dst_address}:{dst_port}:{protocol}");
    upsert_app_flow(
        conn,
        &id,
        exporter_address,
        src_address,
        dst_address,
        dst_port,
        protocol,
        bytes_per_sec,
        packets_per_sec,
        update.timestamp_ns,
    )?;

    // Register exporter IP as DeviceAddress for sFlow.
    let exporter_bare = crate::registry::strip_port(exporter_address);
    let _ = upsert_device_address(conn, exporter_bare, "sflow", ts(update.timestamp_ns));

    let mut carries = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (f:AppFlow {id: $fid}) \
             MERGE (d)-[:CARRIES_FLOW]->(f)",
        )
        .context("prepare CARRIES_FLOW merge (sflow)")?;
    conn.execute(
        &mut carries,
        vec![
            ("addr", Value::String(exporter_bare.to_string())),
            ("fid", Value::String(id.clone())),
        ],
    )
    .context("execute CARRIES_FLOW merge (sflow)")?;

    let mut src_host = conn
        .prepare(
            "MATCH (h:HostEndpoint {ip: $ip}), (f:AppFlow {id: $fid}) \
             MERGE (f)-[:SRC_HOST]->(h)",
        )
        .context("prepare SRC_HOST merge (sflow)")?;
    conn.execute(
        &mut src_host,
        vec![
            ("ip", Value::String(src_address.to_string())),
            ("fid", Value::String(id.clone())),
        ],
    )
    .context("execute SRC_HOST merge (sflow)")?;

    let mut dst_host = conn
        .prepare(
            "MATCH (h:HostEndpoint {ip: $ip}), (f:AppFlow {id: $fid}) \
             MERGE (f)-[:DST_HOST]->(h)",
        )
        .context("prepare DST_HOST merge (sflow)")?;
    conn.execute(
        &mut dst_host,
        vec![
            ("ip", Value::String(dst_address.to_string())),
            ("fid", Value::String(id.clone())),
        ],
    )
    .context("execute DST_HOST merge (sflow)")?;

    emit_flow_utilization_event(conn, update, exporter_address, &id, bytes_per_sec, event_tx, corr_buf);

    let evt = BonsaiEvent {
        device_address: exporter_address.to_string(),
        event_type: "app_flow_event".to_string(),
        detail_json: serde_json::json!({
            "flow_id": id,
            "exporter_address": exporter_address,
            "src_address": src_address,
            "dst_address": dst_address,
            "dst_port": dst_port,
            "protocol": protocol,
            "bytes_per_sec": bytes_per_sec,
            "packets_per_sec": packets_per_sec,
            "sampling_rate": sampling_rate,
        })
        .to_string(),
        occurred_at_ns: update.timestamp_ns,
        state_change_event_id: String::new(),
        source_type: "sflow".to_string(),
    };
    let _ = event_tx.send(evt);
    Ok(())
}

/// D4-5 T1: sFlow counter sample — update Interface node counters by if_index lookup.
/// Silently no-ops when no Interface with matching device+if_index exists.
#[allow(clippy::too_many_arguments)]
fn write_sflow_counters(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    exporter_address: &str,
    if_index: u32,
    _if_speed: u64,
    in_octets: u64,
    out_octets: u64,
    in_errors: u32,
    out_errors: u32,
    in_discards: u32,
    out_discards: u32,
) -> Result<()> {
    let now = ts(update.timestamp_ns);
    let mut stmt = conn
        .prepare(
            "MATCH (i:Interface) \
             WHERE i.device_address = $addr AND i.if_index = $idx \
             SET i.in_octets = $in_oct, i.out_octets = $out_oct, \
                 i.in_errors = $in_err, i.out_errors = $out_err, \
                 i.in_discards = $in_dis, i.out_discards = $out_dis, \
                 i.updated_at = $ts",
        )
        .context("prepare sflow counter update")?;
    conn.execute(
        &mut stmt,
        vec![
            ("addr", Value::String(exporter_address.to_string())),
            ("idx", Value::Int64(if_index as i64)),
            ("in_oct", Value::Int64(in_octets.min(i64::MAX as u64) as i64)),
            ("out_oct", Value::Int64(out_octets.min(i64::MAX as u64) as i64)),
            ("in_err", Value::Int64(in_errors as i64)),
            ("out_err", Value::Int64(out_errors as i64)),
            ("in_dis", Value::Int64(in_discards as i64)),
            ("out_dis", Value::Int64(out_discards as i64)),
            ("ts", now),
        ],
    )
    .context("execute sflow counter update")?;
    Ok(())
}

/// D4-10 T1: Check if a new AppFlow record implies interface utilization > 90%.
/// Queries the exporter device's max interface speed from the graph and compares
/// `bytes_per_sec * 8` against that speed.  Fires an `app_flow_high_utilization`
/// StateChangeEvent if the threshold is breached.  Silent no-op when no Interface
/// speed is known (avoids false positives for unregistered exporters).
#[allow(clippy::too_many_arguments)]
fn emit_flow_utilization_event(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    exporter_address: &str,
    flow_id: &str,
    bytes_per_sec: f64,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) {
    const UTIL_THRESHOLD: f64 = 0.90;
    let mut stmt = match conn.prepare(
        "MATCH (d:Device)-[:HAS_INTERFACE]->(i:Interface) \
         WHERE (d.address = $addr OR d.address STARTS WITH $addr_pfx) AND i.speed > 0 \
         RETURN i.speed ORDER BY i.speed DESC LIMIT 1",
    ) {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut rows = match conn.execute(
        &mut stmt,
        vec![
            ("addr", Value::String(exporter_address.to_string())),
            ("addr_pfx", Value::String(format!("{exporter_address}:"))),
        ],
    ) {
        Ok(r) => r,
        Err(_) => return,
    };
    let speed_bps = match rows.next() {
        Some(row) => match &row[0] {
            Value::Int64(s) => *s as f64,
            Value::Double(s) => *s,
            _ => return,
        },
        None => return,
    };
    if speed_bps <= 0.0 {
        return;
    }
    let utilization = (bytes_per_sec * 8.0) / speed_bps;
    if utilization < UTIL_THRESHOLD {
        return;
    }
    let detail = serde_json::json!({
        "exporter_address": exporter_address,
        "flow_id": flow_id,
        "bytes_per_sec": bytes_per_sec,
        "interface_speed_bps": speed_bps,
        "utilization_pct": (utilization * 100.0).round() as i64,
    })
    .to_string();
    let _ = write_state_change_event(
        conn,
        exporter_address,
        "app_flow_high_utilization",
        &detail,
        "flow",
        ts(update.timestamp_ns),
        update.timestamp_ns,
        event_tx,
        corr_buf,
    );
}

fn write_interface_summary(
    conn: &Connection<'_>,
    u: &TelemetryUpdate,
    if_name: &str,
) -> Result<()> {
    let bare = crate::registry::strip_port(&u.target);
    let id = format!("{}:{}", bare, if_name);
    let now = ts(u.timestamp_ns);

    upsert_device(conn, &u.target, &u.vendor, &u.hostname, "", "", now.clone())?;

    let summary = u
        .value
        .get("interface_summary")
        .context("missing interface_summary")?;
    let counters = summary
        .get("counters")
        .context("missing counters in summary")?;

    let get_max = |aliases: &[&str]| -> i64 {
        for &alias in aliases {
            if let Some(c) = counters.get(alias)
                && let Some(max) = c.get("max").and_then(|v| v.as_i64())
            {
                return max;
            }
        }
        0
    };

    let in_pkts = get_max(&["in-packets", "input-packets", "in-pkts"]);
    let out_pkts = get_max(&["out-packets", "packets-sent", "output-packets", "out-pkts"]);
    let in_octets = get_max(&["in-octets", "bytes-received", "input-bytes"]);
    let out_octets = get_max(&["out-octets", "bytes-sent", "output-bytes"]);
    let in_errors = get_max(&[
        "in-error-packets",
        "input-total-errors",
        "input-errors",
        "in-errors",
    ]);
    let out_errors = get_max(&[
        "out-error-packets",
        "output-total-errors",
        "output-errors",
        "out-errors",
    ]);
    let carrier = get_max(&["carrier-transitions"]);

    let mut stmt = conn.prepare(
        "MERGE (n:Interface {id: $id}) \
         ON CREATE SET \
           n.device_address = $addr, n.name = $name, \
           n.in_pkts = $in_p, n.out_pkts = $out_p, \
           n.in_octets = $in_o, n.out_octets = $out_o, \
           n.in_errors = $in_e, n.out_errors = $out_e, \
           n.carrier_transitions = $carrier, \
           n.updated_at = $ts \
         ON MATCH SET \
           n.in_pkts = $in_p, n.out_pkts = $out_p, \
           n.in_octets = $in_o, n.out_octets = $out_o, \
           n.in_errors = $in_e, n.out_errors = $out_e, \
           n.carrier_transitions = $carrier, \
           n.updated_at = $ts",
    )?;

    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("addr", Value::String(bare.to_string())),
            ("name", Value::String(if_name.to_string())),
            ("in_p", Value::Int64(in_pkts)),
            ("out_p", Value::Int64(out_pkts)),
            ("in_o", Value::Int64(in_octets)),
            ("out_o", Value::Int64(out_octets)),
            ("in_e", Value::Int64(in_errors)),
            ("out_e", Value::Int64(out_errors)),
            ("carrier", Value::Int64(carrier)),
            ("ts", now),
        ],
    )
    .context("execute interface summary upsert")?;

    // Ensure the Device→Interface edge exists
    let mut edge_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (i:Interface {id: $id}) \
         MERGE (d)-[:HAS_INTERFACE]->(i)",
        )
        .context("prepare HAS_INTERFACE merge for summary")?;

    conn.execute(
        &mut edge_stmt,
        vec![
            ("addr", Value::String(bare.to_string())),
            ("id", Value::String(id.clone())),
        ],
    )
    .context("execute HAS_INTERFACE merge for summary")?;

    // Distributed collectors forward counter summaries instead of every raw
    // counter update. Those summaries still create Interface nodes, so they
    // must trigger the same LLDP backfill as the raw interface writer.
    let _ = backfill_connected_to(conn, &u.target, if_name);

    Ok(())
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

    upsert_device(
        conn,
        &status.device_address,
        "",
        "",
        "",
        "",
        updated_at.clone(),
    )?;

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
fn read_f64(v: &Value) -> f64 {
    match v {
        Value::Double(n) => *n,
        Value::Float(n) => (*n).into(),
        _ => 0.0,
    }
}

fn read_i64(v: &Value) -> i64 {
    match v {
        Value::Int64(n) => *n,
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

fn write_interface(conn: &Connection<'_>, u: &TelemetryUpdate, if_name: &str) -> Result<()> {
    let bare = crate::registry::strip_port(&u.target);
    let id = format!("{}:{}", bare, if_name);
    let now = ts(u.timestamp_ns);

    upsert_device(conn, &u.target, &u.vendor, &u.hostname, "", "", now.clone())?;

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
            ("addr", Value::String(bare.to_string())),
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
            ("addr", Value::String(bare.to_string())),
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
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
    let bare = crate::registry::strip_port(&u.target);
    let id = format!("{}:{}", bare, peer_addr);
    let now = ts(u.timestamp_ns);
    let new_state = json_str(val, "session-state").to_lowercase();

    upsert_device(conn, &u.target, &u.vendor, &u.hostname, "", "", now.clone())?;

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
            ("addr", Value::String(bare.to_string())),
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
            "gnmi",
            now.clone(),
            u.timestamp_ns,
            event_tx,
            corr_buf,
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
            ("addr", Value::String(bare.to_string())),
            ("id", Value::String(id)),
        ],
    )
    .context("execute PEERS_WITH merge")?;

    // Register peer_address as a DeviceAddress candidate. When the peer device is onboarded
    // (or its loopback is added via extra_ips), KNOWN_ADDRESS_OF will link it back to a Device.
    // This lets resolve_peer_to_device() succeed on a subsequent write even if the peer
    // was not yet known at the time of this write.
    let _ = crate::graph::common::upsert_device_address(conn, peer_addr, "bgp_peer", now.clone());

    // Attempt to resolve peer_address → Device and create a BGP_SESSION_WITH edge.
    // Best-effort: silently skipped when the peer is not yet in the graph.
    if let Some(peer_device) = resolve_peer_to_device(conn, peer_addr) {
        let mut bgp_d2d = conn
            .prepare(
                "MATCH (a:Device {address: $src}), (b:Device {address: $dst}) \
                 MERGE (a)-[r:BGP_SESSION_WITH {source: 'gnmi'}]->(b) \
                 SET r.session_state = $state, r.peer_as = $peer_as",
            )
            .context("prepare BGP_SESSION_WITH merge")?;
        let _ = conn.execute(
            &mut bgp_d2d,
            vec![
                ("src", Value::String(bare.to_string())),
                ("dst", Value::String(peer_device.clone())),
                ("state", Value::String(new_state.clone())),
                ("peer_as", Value::Int64(peer_as)),
            ],
        );
        debug!(src = %bare, dst = %peer_device, state = %new_state, "BGP_SESSION_WITH resolved");
    }

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
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
    let bare = crate::registry::strip_port(&u.target);
    let id = format!("{}:{}:{}", bare, if_name, local_discriminator);
    let now = ts(u.timestamp_ns);
    let new_state = json_str(val, "session-state").to_lowercase();
    let remote_address = json_str(val, "remote-address").to_string();
    let local_address = json_str(val, "local-address").to_string();

    if new_state.is_empty() {
        return Ok(());
    }

    upsert_device(conn, &u.target, &u.vendor, &u.hostname, "", "", now.clone())?;

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
            ("addr", Value::String(bare.to_string())),
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
            "gnmi",
            now.clone(),
            u.timestamp_ns,
            event_tx,
            corr_buf,
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
            ("addr", Value::String(bare.to_string())),
            ("id", Value::String(id)),
        ],
    )
    .context("execute HAS_BFD_SESSION merge")?;

    // Register remote_address as a DeviceAddress candidate so it can be resolved later.
    if !remote_address.is_empty() {
        let _ = crate::graph::common::upsert_device_address(conn, &remote_address, "bfd_peer", now.clone());
    }

    // Attempt to resolve remote_address → Device and create BFD_PEER_WITH(Device→Device).
    // BFD transport IPs are often loopbacks or interface addresses different from the mgmt IP.
    if !remote_address.is_empty() {
        if let Some(peer_device) = resolve_peer_to_device(conn, &remote_address) {
            let mut bfd_d2d = conn
                .prepare(
                    "MATCH (a:Device {address: $src}), (b:Device {address: $dst}) \
                     MERGE (a)-[r:BFD_PEER_WITH {source: 'gnmi'}]->(b) \
                     SET r.session_state = $state",
                )
                .context("prepare BFD_PEER_WITH merge")?;
            let _ = conn.execute(
                &mut bfd_d2d,
                vec![
                    ("src", Value::String(bare.to_string())),
                    ("dst", Value::String(peer_device.clone())),
                    ("state", Value::String(new_state.clone())),
                ],
            );
            debug!(src = %bare, dst = %peer_device, state = %new_state, "BFD_PEER_WITH resolved");
        }
    }

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

fn write_isis_adjacency(
    conn: &Connection<'_>,
    u: &TelemetryUpdate,
    system_id: &str,
    if_name: &str,
    val: &serde_json::Value,
    source_type: &str,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
    let bare = crate::registry::strip_port(&u.target);
    let id = format!("{}:{}:{}", bare, system_id, if_name);
    let now = ts(u.timestamp_ns);
    // SRL's ON_CHANGE sync sends adjacency list entries WITHOUT adjacency-state in the
    // initial response — the entry's existence implies the adjacency is established (up).
    // adjacency-state is only streamed on subsequent change events.
    let new_state = {
        let s = json_str(val, "adjacency-state").to_lowercase();
        if s.is_empty() { "up".to_string() } else { s }
    };

    upsert_device(conn, &u.target, &u.vendor, &u.hostname, "", "", now.clone())?;

    let old_state = get_isis_adjacency_state(conn, &id)?;

    let mut stmt = conn
        .prepare(
            "MERGE (a:IsisAdjacency {id: $id}) \
         ON CREATE SET \
           a.device_address = $addr, a.system_id = $sid, a.if_name = $if_name, \
           a.neighbor_id = $sid, a.adjacency_state = $state, \
           a.source_type = $src, a.updated_at = $ts \
         ON MATCH SET \
           a.adjacency_state = $state, a.source_type = $src, a.updated_at = $ts",
        )
        .context("prepare IsisAdjacency upsert")?;

    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("addr", Value::String(bare.to_string())),
            ("sid", Value::String(system_id.to_string())),
            ("if_name", Value::String(if_name.to_string())),
            ("state", Value::String(new_state.clone())),
            ("src", Value::String(source_type.to_string())),
            ("ts", now.clone()),
        ],
    )
    .context("execute IsisAdjacency upsert")?;

    if old_state.as_deref() != Some(new_state.as_str()) {
        let detail = format!(
            r#"{{"system_id":"{}","if_name":"{}","old_state":"{}","new_state":"{}","source_type":"{}"}}"#,
            system_id,
            if_name,
            old_state.as_deref().unwrap_or("none"),
            new_state,
            source_type,
        );
        write_state_change_event(
            conn,
            &u.target,
            "isis_adjacency_change",
            &detail,
            source_type,
            now.clone(),
            u.timestamp_ns,
            event_tx,
            corr_buf,
        )?;
    }

    let mut edge_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (a:IsisAdjacency {id: $id}) \
         MERGE (d)-[:HAS_ISIS_ADJACENCY]->(a)",
        )
        .context("prepare HAS_ISIS_ADJACENCY merge")?;

    conn.execute(
        &mut edge_stmt,
        vec![
            ("addr", Value::String(bare.to_string())),
            ("id", Value::String(id)),
        ],
    )
    .context("execute HAS_ISIS_ADJACENCY merge")?;

    // ISIS system_id is a dotted NET (e.g. "0100.0000.0002.00"). Vendors ALSO advertise
    // the same system ID via LLDP chassis-id TLV, which lands in EntityIdentity.chassis_id.
    // We try that path first, then fall back to resolving via DeviceAddress (rare, but
    // some implementations use a loopback IP derived from the system ID).
    let isis_peer_device: Option<String> = {
        let mut found = None;
        // Path 1: EntityIdentity.chassis_id (Cisco/Juniper/Nokia/Arista via LLDP)
        if let Ok(mut stmt) = conn.prepare(
            "MATCH (e:EntityIdentity {chassis_id: $cid})<-[:HAS_IDENTITY]-(d:Device) \
             RETURN d.address LIMIT 1",
        ) {
            if let Ok(mut rows) = conn.execute(&mut stmt, vec![("cid", Value::String(system_id.to_string()))]) {
                if let Some(row) = rows.next() {
                    if let Value::String(addr) = &row[0] {
                        if !addr.is_empty() { found = Some(addr.clone()); }
                    }
                }
            }
        }
        // Path 2: resolve_peer_to_device (loopback IP derived from system-id, if registered)
        if found.is_none() {
            found = resolve_peer_to_device(conn, system_id);
        }
        found
    };

    if let Some(peer_device) = isis_peer_device {
        let mut isis_d2d = conn
            .prepare(
                "MATCH (a:Device {address: $src}), (b:Device {address: $dst}) \
                 MERGE (a)-[r:ISIS_NEIGHBOR_WITH {source: $src_type, if_name: $ifn}]->(b) \
                 SET r.adjacency_state = $state",
            )
            .context("prepare ISIS_NEIGHBOR_WITH merge")?;
        let _ = conn.execute(
            &mut isis_d2d,
            vec![
                ("src", Value::String(bare.to_string())),
                ("dst", Value::String(peer_device.clone())),
                ("state", Value::String(new_state.clone())),
                ("ifn", Value::String(if_name.to_string())),
                ("src_type", Value::String(source_type.to_string())),
            ],
        );
        debug!(src = %bare, dst = %peer_device, state = %new_state, "ISIS_NEIGHBOR_WITH resolved");
    }

    info!(
        target = %u.target,
        system_id = %system_id,
        if_name = %if_name,
        state = %new_state,
        source = %source_type,
        "IS-IS adjacency written"
    );
    Ok(())
}

fn get_isis_adjacency_state(conn: &Connection<'_>, id: &str) -> Result<Option<String>> {
    let mut stmt = match conn.prepare("MATCH (a:IsisAdjacency {id: $id}) RETURN a.adjacency_state") {
        Ok(s) => s,
        Err(e) if e.to_string().contains("does not exist") => return Ok(None),
        Err(e) => return Err(e).context("prepare IsisAdjacency state lookup"),
    };
    let mut result = conn
        .execute(&mut stmt, vec![("id", Value::String(id.to_string()))])
        .context("execute IsisAdjacency state lookup")?;
    Ok(result.next().and_then(|row| {
        if let Value::String(s) = &row[0] {
            Some(s.clone())
        } else {
            None
        }
    }))
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
    source_type: &str,
    now: Value,
    timestamp_ns: i64,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<String> {
    // Normalise to bare IP at entry — callers may pass ip:port (gNMI target).
    // This ensures StateChangeEvent.device_address matches Device.address (bare IP PK).
    let device_address = crate::registry::strip_port(device_address);

    // G6: Emit metric for state change event write
    metrics::counter!("bonsai_graph_state_change_write_total", "event_type" => event_type.to_string(), "source" => source_type.to_string()).increment(1);

    let id = Uuid::new_v4().to_string();

    let mut stmt = conn
        .prepare(
            "CREATE (e:StateChangeEvent {\
                id: $id, device_address: $addr, event_type: $etype, \
                detail: $detail, source_type: $src, occurred_at: $ts})",
        )
        .context("prepare StateChangeEvent insert")?;

    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("addr", Value::String(device_address.to_string())),
            ("etype", Value::String(event_type.to_string())),
            ("detail", Value::String(detail.to_string())),
            ("src", Value::String(source_type.to_string())),
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
            source_type: source_type.to_string(),
        })
        .is_err()
    {
        metrics::counter!("bonsai_broadcast_drops_total").increment(1);
    }

    if let Some((semantic_type, sub_key)) = semantic_key_for_event(event_type, detail) {
        // Always use bare IP as the correlation key so events from gNMI (ip:port),
        // syslog, SNMP, BMP, and NetFlow all land in the same slot for the same device.
        let bare_addr = crate::registry::strip_port(device_address);
        let key = CorrelationKey::new(bare_addr, semantic_type, sub_key);
        corr_buf.record(key, id.clone(), source_type.to_string(), timestamp_ns, detail.to_string());
    }

    debug!(device = %device_address, event_type = %event_type, "state change event recorded");
    Ok(id)
}

fn write_syslog_state_change_event(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    category: &str,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
    write_signal_state_change_event(conn, update, &format!("syslog_{category}"), "syslog", event_tx, corr_buf)
}

#[derive(Serialize)]
struct SyslogFactEventDetail {
    fact_type: String,
    category: String,
    hostname: String,
    message: String,
    raw: String,
    transport: String,
    peer_addr: String,
    field_schema: std::collections::BTreeMap<String, String>,
    fields: std::collections::BTreeMap<String, String>,
    device_context: JsonValue,
    join: JsonValue,
}

fn write_syslog_fact_event(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    fact_type: &str,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
    let fact: SyslogFact =
        serde_json::from_value(update.value.clone()).context("parse syslog fact event")?;
    upsert_device(
        conn,
        &update.target,
        &update.vendor,
        &update.hostname,
        &update.role,
        &update.site,
        ts(update.timestamp_ns),
    )?;
    let join = join_syslog_fact(conn, update, &fact, event_tx, corr_buf)?;
    let join_status = join
        .get("status")
        .and_then(JsonValue::as_str)
        .unwrap_or("orphan");
    let event_type = if join_status == "joined" {
        "syslog_fact_joined"
    } else {
        "syslog_fact_orphan"
    };
    metrics::counter!(
        "bonsai_syslog_fact_join_total",
        "fact_type" => fact_type.to_string(),
        "status" => join_status.to_string(),
    )
    .increment(1);
    let bare_target = crate::registry::strip_port(&update.target).to_string();
    let detail = SyslogFactEventDetail {
        fact_type: fact_type.to_string(),
        category: fact.category,
        hostname: fact.hostname,
        message: fact.message,
        raw: fact.raw,
        transport: fact.transport,
        peer_addr: fact.peer_addr,
        field_schema: fact.field_schema,
        fields: fact.fields,
        device_context: json!({
            "device_address": bare_target,
            "vendor": update.vendor,
            "hostname": update.hostname,
            "role": update.role,
            "site": update.site,
        }),
        join,
    };
    let detail_json = serde_json::to_string(&detail).context("serialize syslog fact detail")?;
    let _ = write_state_change_event(
        conn,
        &bare_target,
        event_type,
        &detail_json,
        "syslog",
        ts(update.timestamp_ns),
        update.timestamp_ns,
        event_tx,
        corr_buf,
    )?;
    Ok(())
}

fn join_syslog_fact(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    fact: &SyslogFact,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<JsonValue> {
    // Route by fact_type before reaching generic field-based branches to prevent
    // ospf/isis/bfd facts from accidentally joining to Interface or BgpNeighbor nodes.
    if fact.fact_type == "bfd_session" {
        return join_bfd_fact(conn, update, fact);
    }
    if fact.fact_type == "ospf_neighbor" {
        return join_ospf_fact(conn, update, fact);
    }
    if fact.fact_type == "isis_adjacency" {
        return join_isis_fact(conn, update, fact, event_tx, corr_buf);
    }
    
    // Service process syslog facts - create ServiceEndpoint nodes
    if fact.fact_type == "service_process" {
        return join_service_process_fact(conn, update, fact);
    }
    
    // QoS policy change syslog facts - create QoSPolicy nodes
    if fact.fact_type == "qos_policy_change" {
        return join_qos_policy_fact(conn, update, fact);
    }
    
    // Connection anomaly syslog facts - update ServiceEndpoint health
    if fact.fact_type == "connection_anomaly" {
        return join_connection_anomaly_fact(conn, update, fact);
    }

    if let Some(peer_address) = fact
        .fields
        .get("peer_address")
        .or_else(|| fact.fields.get("peer"))
        && let Some(graph_state) = lookup_bgp_neighbor_state(conn, &update.target, peer_address)?
    {
        return Ok(json!({
            "status": "joined",
            "kind": "bgp_neighbor",
            "key": peer_address,
            "graph_state": graph_state,
        }));
    }

    if let Some(if_name) = fact
        .fields
        .get("if_name")
        .or_else(|| fact.fields.get("interface"))
        .or_else(|| fact.fields.get("interface_name"))
    {
        if let Some(graph_state) = lookup_interface_state(conn, &update.target, if_name)? {
            return Ok(json!({
                "status": "joined",
                "kind": "interface",
                "key": if_name,
                "graph_state": graph_state,
            }));
        }
        return Ok(json!({
            "status": "orphan",
            "kind": "interface",
            "key": if_name,
            "reason": "no_interface_match",
        }));
    }

    if fact.fields.contains_key("peer_address") || fact.fields.contains_key("peer") {
        return Ok(json!({
            "status": "orphan",
            "kind": "bgp_neighbor",
            "key": fact
                .fields
                .get("peer_address")
                .or_else(|| fact.fields.get("peer"))
                .cloned()
                .unwrap_or_default(),
            "reason": "no_bgp_neighbor_match",
        }));
    }

    Ok(json!({
        "status": "orphan",
        "kind": "unknown",
        "reason": "no_cross_source_match_key",
    }))
}

// ── SNMP OID Fact pipeline ────────────────────────────────────────────────────

fn write_snmp_fact_event(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    fact_type: &str,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
    let fact: SnmpFact =
        serde_json::from_value(update.value.clone()).context("parse snmp fact")?;
    upsert_device(
        conn,
        &update.target,
        &update.vendor,
        &update.hostname,
        &update.role,
        &update.site,
        ts(update.timestamp_ns),
    )?;
    let join = join_snmp_fact(conn, update, &fact)?;
    let join_status = join
        .get("status")
        .and_then(JsonValue::as_str)
        .unwrap_or("orphan");
    let event_type = if join_status == "joined" {
        "snmp_fact_joined"
    } else {
        "snmp_fact_orphan"
    };
    metrics::counter!(
        "bonsai_snmp_fact_join_total",
        "fact_type" => fact_type.to_string(),
        "status" => join_status.to_string(),
    )
    .increment(1);
    let bare_target = crate::registry::strip_port(&update.target).to_string();
    let detail_json = serde_json::to_string(&serde_json::json!({
        "fact_type": fact_type,
        "trap_oid": fact.trap_oid,
        "peer_addr": fact.peer_addr,
        "enterprise_oid": fact.enterprise_oid,
        "field_schema": fact.field_schema,
        "fields": fact.fields,
        "device_context": {
            "device_address": bare_target,
            "vendor": update.vendor,
            "hostname": update.hostname,
            "role": update.role,
            "site": update.site,
        },
        "join": join,
    }))
    .context("serialize snmp fact detail")?;
    let _ = write_state_change_event(
        conn,
        &bare_target,
        event_type,
        &detail_json,
        "snmp",
        ts(update.timestamp_ns),
        update.timestamp_ns,
        event_tx,
        corr_buf,
    )?;
    Ok(())
}

/// Join an SNMP fact against graph context.
/// - link_down / link_up → correlate with Interface oper_status via `interface_name` or `interface_index`
/// - bgp_peer_state / bgp_peer_backward_transition → correlate with BgpNeighbor via `peer_address`
/// - ospf / isis → correlate with existing StateChangeEvents on the same device
/// - everything else → orphan (still written as a state-change event)
fn join_snmp_fact(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    fact: &SnmpFact,
) -> Result<JsonValue> {
    // Interface facts: link_down / link_up
    if matches!(fact.fact_type.as_str(), "link_down" | "link_up") {
        if let Some(iface_name) = fact.fields.get("interface_name").filter(|v| !v.is_empty()) {
            let if_id = format!("{}:{}", update.target, iface_name);
            let mut stmt = conn
                .prepare("MATCH (i:Interface {id: $id}) RETURN i.oper_status")
                .context("prepare interface join")?;
            let rows: Vec<_> = conn
                .execute(&mut stmt, vec![("id", Value::String(if_id.clone()))])
                .context("execute interface join")?
                .collect();
            if let Some(row) = rows.first() {
                return Ok(serde_json::json!({
                    "status": "joined",
                    "join_type": "interface",
                    "interface_id": if_id,
                    "current_oper_status": read_str(&row[0]),
                }));
            }
        }
    }

    // BGP facts: bgp_peer_state / bgp_peer_backward_transition
    if matches!(fact.fact_type.as_str(), "bgp_peer_state" | "bgp_peer_backward_transition") {
        if let Some(peer_addr) = fact.fields.get("peer_address").filter(|v| !v.is_empty()) {
            let mut stmt = conn
                .prepare(
                    "MATCH (n:BgpNeighbor {device_address: $dev, peer_address: $peer}) \
                     RETURN n.session_state, n.peer_as",
                )
                .context("prepare bgp neighbor join")?;
            let rows: Vec<_> = conn
                .execute(
                    &mut stmt,
                    vec![
                        ("dev", Value::String(crate::registry::strip_port(&update.target).to_string())),
                        ("peer", Value::String(peer_addr.clone())),
                    ],
                )
                .context("execute bgp neighbor join")?
                .collect();
            if let Some(row) = rows.first() {
                return Ok(serde_json::json!({
                    "status": "joined",
                    "join_type": "bgp_neighbor",
                    "peer_address": peer_addr,
                    "current_session_state": read_str(&row[0]),
                    "peer_as": read_str(&row[1]),
                }));
            }
        }
    }

    Ok(serde_json::json!({
        "status": "orphan",
        "reason": "no_graph_entity_matched",
        "fact_type": fact.fact_type,
    }))
}

fn join_bfd_fact(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    fact: &SyslogFact,
) -> Result<JsonValue> {
    let if_name = fact.fields.get("if_name");
    let remote_address = fact.fields.get("remote_address");

    if let Some(if_name) = if_name
        && let Some(graph_state) = lookup_bfd_session_by_interface(conn, &update.target, if_name)?
    {
        return Ok(json!({
            "status": "joined",
            "kind": "bfd_session",
            "key": if_name,
            "graph_state": graph_state,
        }));
    }

    if let Some(remote_addr) = remote_address
        && let Some(graph_state) = lookup_bfd_session_by_remote(conn, &update.target, remote_addr)?
    {
        return Ok(json!({
            "status": "joined",
            "kind": "bfd_session",
            "key": remote_addr,
            "graph_state": graph_state,
        }));
    }

    let key = if_name.or(remote_address).cloned().unwrap_or_default();
    Ok(json!({
        "status": "orphan",
        "kind": "bfd_session",
        "key": key,
        "reason": "no_bfd_session_match",
    }))
}

fn lookup_bgp_neighbor_state(
    conn: &Connection<'_>,
    device_address: &str,
    peer_address: &str,
) -> Result<Option<JsonValue>> {
    let id = format!("{device_address}:{peer_address}");
    let mut stmt = conn
        .prepare(
            "MATCH (n:BgpNeighbor {id: $id}) \
             RETURN n.peer_address, n.peer_as, n.session_state, n.established_transitions",
        )
        .context("prepare BGP neighbor fact join lookup")?;
    let mut rows = conn
        .execute(&mut stmt, vec![("id", Value::String(id))])
        .context("execute BGP neighbor fact join lookup")?;
    let Some(row) = rows.next() else {
        return Ok(None);
    };
    Ok(Some(json!({
        "peer_address": read_str(&row[0]),
        "peer_as": value_i64(&row[1]),
        "session_state": read_str(&row[2]),
        "established_transitions": value_i64(&row[3]),
    })))
}

fn lookup_interface_state(
    conn: &Connection<'_>,
    device_address: &str,
    if_name: &str,
) -> Result<Option<JsonValue>> {
    let id = format!("{device_address}:{if_name}");
    let mut stmt = conn
        .prepare(
            "MATCH (i:Interface {id: $id}) \
             RETURN i.name, i.in_errors, i.out_errors, i.in_octets, i.out_octets, i.in_pkts, i.out_pkts",
        )
        .context("prepare interface fact join lookup")?;
    let mut rows = conn
        .execute(&mut stmt, vec![("id", Value::String(id))])
        .context("execute interface fact join lookup")?;
    let Some(row) = rows.next() else {
        return Ok(None);
    };
    Ok(Some(json!({
        "if_name": read_str(&row[0]),
        "in_errors": value_i64(&row[1]),
        "out_errors": value_i64(&row[2]),
        "in_octets": value_i64(&row[3]),
        "out_octets": value_i64(&row[4]),
        "in_pkts": value_i64(&row[5]),
        "out_pkts": value_i64(&row[6]),
    })))
}

fn lookup_bfd_session_by_interface(
    conn: &Connection<'_>,
    device_address: &str,
    if_name: &str,
) -> Result<Option<JsonValue>> {
    let mut stmt = conn
        .prepare(
            "MATCH (b:BfdSession) \
             WHERE b.device_address = $addr AND b.if_name = $if_name \
             RETURN b.session_state, b.remote_address, b.local_discriminator \
             LIMIT 1",
        )
        .context("prepare BFD session by interface join lookup")?;
    let mut rows = conn
        .execute(
            &mut stmt,
            vec![
                ("addr", Value::String(device_address.to_string())),
                ("if_name", Value::String(if_name.to_string())),
            ],
        )
        .context("execute BFD session by interface join lookup")?;
    let Some(row) = rows.next() else {
        return Ok(None);
    };
    Ok(Some(json!({
        "session_state": read_str(&row[0]),
        "remote_address": read_str(&row[1]),
        "local_discriminator": read_str(&row[2]),
    })))
}

fn lookup_bfd_session_by_remote(
    conn: &Connection<'_>,
    device_address: &str,
    remote_address: &str,
) -> Result<Option<JsonValue>> {
    let mut stmt = conn
        .prepare(
            "MATCH (b:BfdSession) \
             WHERE b.device_address = $addr AND b.remote_address = $remote_addr \
             RETURN b.session_state, b.if_name, b.local_discriminator \
             LIMIT 1",
        )
        .context("prepare BFD session by remote address join lookup")?;
    let mut rows = conn
        .execute(
            &mut stmt,
            vec![
                ("addr", Value::String(device_address.to_string())),
                ("remote_addr", Value::String(remote_address.to_string())),
            ],
        )
        .context("execute BFD session by remote address join lookup")?;
    let Some(row) = rows.next() else {
        return Ok(None);
    };
    Ok(Some(json!({
        "session_state": read_str(&row[0]),
        "if_name": read_str(&row[1]),
        "local_discriminator": read_str(&row[2]),
    })))
}

fn join_ospf_fact(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    fact: &SyslogFact,
) -> Result<JsonValue> {
    let neighbor_address = fact
        .fields
        .get("neighbor_address")
        .or_else(|| fact.fields.get("neighbor"));
    let if_name = fact.fields.get("if_name");

    if let Some(addr) = neighbor_address
        && let Some(graph_state) = lookup_ospf_neighbor_state(conn, &update.target, addr)?
    {
        return Ok(json!({
            "status": "joined",
            "kind": "ospf_neighbor",
            "key": addr,
            "graph_state": graph_state,
        }));
    }

    let key = neighbor_address.or(if_name).cloned().unwrap_or_default();
    Ok(json!({
        "status": "orphan",
        "kind": "ospf_neighbor",
        "key": key,
        "reason": "no_ospf_neighbor_match",
    }))
}

fn join_isis_fact(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    fact: &SyslogFact,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<JsonValue> {
    let neighbor_id = fact
        .fields
        .get("neighbor_id")
        .or_else(|| fact.fields.get("system_id"));
    let if_name = fact.fields.get("if_name");
    let new_state = fact.fields.get("new_state");

    // Write the adjacency node from syslog if we have enough fields.
    // Any source writes — gNMI is preferred but syslog is equally valid.
    if let (Some(nid), Some(iface), Some(state)) = (neighbor_id, if_name, new_state) {
        let val = serde_json::json!({ "adjacency-state": state });
        let _ = write_isis_adjacency(conn, update, nid, iface, &val, "syslog", event_tx, corr_buf);
    }

    if let Some(nid) = neighbor_id
        && let Some(graph_state) = lookup_isis_adjacency_state(conn, &update.target, nid)?
    {
        return Ok(json!({
            "status": "joined",
            "kind": "isis_adjacency",
            "key": nid,
            "graph_state": graph_state,
        }));
    }

    let key = neighbor_id.or(if_name).cloned().unwrap_or_default();
    Ok(json!({
        "status": "orphan",
        "kind": "isis_adjacency",
        "key": key,
        "reason": "no_isis_adjacency_match",
    }))
}

fn lookup_ospf_neighbor_state(
    conn: &Connection<'_>,
    device_address: &str,
    neighbor_address: &str,
) -> Result<Option<JsonValue>> {
    let id = format!("{device_address}:{neighbor_address}");
    let mut stmt = match conn.prepare(
        "MATCH (n:OspfNeighbor {id: $id}) \
         RETURN n.neighbor_address, n.adjacency_state, n.if_name",
    ) {
        Ok(s) => s,
        // Node label doesn't exist yet — no OSPF telemetry has been written.
        Err(e) if e.to_string().contains("does not exist") => return Ok(None),
        Err(e) => return Err(e).context("prepare OSPF neighbor fact join lookup"),
    };
    let mut rows = conn
        .execute(&mut stmt, vec![("id", Value::String(id))])
        .context("execute OSPF neighbor fact join lookup")?;
    let Some(row) = rows.next() else {
        return Ok(None);
    };
    Ok(Some(json!({
        "neighbor_address": read_str(&row[0]),
        "adjacency_state": read_str(&row[1]),
        "if_name": read_str(&row[2]),
    })))
}

fn lookup_isis_adjacency_state(
    conn: &Connection<'_>,
    device_address: &str,
    neighbor_id: &str,
) -> Result<Option<JsonValue>> {
    let mut stmt = match conn.prepare(
        "MATCH (a:IsisAdjacency) \
         WHERE a.device_address = $addr AND a.neighbor_id = $nid \
         RETURN a.neighbor_id, a.adjacency_state, a.if_name \
         LIMIT 1",
    ) {
        Ok(s) => s,
        // Node label doesn't exist yet — no IS-IS telemetry has been written.
        Err(e) if e.to_string().contains("does not exist") => return Ok(None),
        Err(e) => return Err(e).context("prepare IS-IS adjacency fact join lookup"),
    };
    let mut rows = conn
        .execute(
            &mut stmt,
            vec![
                ("addr", Value::String(device_address.to_string())),
                ("nid", Value::String(neighbor_id.to_string())),
            ],
        )
        .context("execute IS-IS adjacency fact join lookup")?;
    let Some(row) = rows.next() else {
        return Ok(None);
    };
    Ok(Some(json!({
        "neighbor_id": read_str(&row[0]),
        "adjacency_state": read_str(&row[1]),
        "if_name": read_str(&row[2]),
    })))
}

fn value_i64(value: &Value) -> i64 {
    match value {
        Value::Int64(number) => *number,
        _ => 0,
    }
}

fn write_signal_state_change_event(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    event_type: &str,
    source_type: &str,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
    let bare = crate::registry::strip_port(&update.target).to_string();
    let now = ts(update.timestamp_ns);
    upsert_device(
        conn,
        &update.target,
        &update.vendor,
        &update.hostname,
        &update.role,
        &update.site,
        now.clone(),
    )?;
    let detail = serde_json::to_string(&update.value).context("serialize syslog event detail")?;
    let _ = write_state_change_event(
        conn,
        &bare,
        event_type,
        &detail,
        source_type,
        now,
        update.timestamp_ns,
        event_tx,
        corr_buf,
    )?;
    Ok(())
}

fn write_bmp_peer_state(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
    let event: BmpEvent =
        serde_json::from_value(update.value.clone()).context("parse BMP peer-state event")?;
    upsert_device(
        conn,
        &update.target,
        &update.vendor,
        &update.hostname,
        &update.role,
        &update.site,
        ts(update.timestamp_ns),
    )?;
    upsert_bmp_session(conn, update, &event)?;

    // Mirror peer state into BgpNeighbor so the topology API shows BMP-only
    // devices (e.g. frr-rr which has no gNMI) alongside gNMI-sourced peers.
    if !event.peer_address.is_empty() {
        let bmp_bare = crate::registry::strip_port(&update.target);
        let bgp_id = format!("{}:{}", bmp_bare, event.peer_address);
        let mut stmt = conn
            .prepare(
                "MERGE (n:BgpNeighbor {id: $id}) \
                 ON CREATE SET \
                   n.device_address = $addr, n.peer_address = $peer, \
                   n.peer_as = $peer_as, n.session_state = $state, \
                   n.established_transitions = 0, n.updated_at = $ts \
                 ON MATCH SET \
                   n.session_state = $state, n.updated_at = $ts",
            )
            .context("prepare BgpNeighbor upsert (bmp)")?;
        conn.execute(
            &mut stmt,
            vec![
                ("id", Value::String(bgp_id.clone())),
                ("addr", Value::String(bmp_bare.to_string())),
                ("peer", Value::String(event.peer_address.clone())),
                ("peer_as", Value::Int64(event.peer_as as i64)),
                ("state", Value::String(event.session_state.clone())),
                ("ts", ts(update.timestamp_ns)),
            ],
        )
        .context("execute BgpNeighbor upsert (bmp)")?;

        let mut edge = conn
            .prepare(
                "MATCH (d:Device {address: $addr}), (n:BgpNeighbor {id: $id}) \
                 MERGE (d)-[:PEERS_WITH]->(n)",
            )
            .context("prepare PEERS_WITH merge (bmp)")?;
        conn.execute(
            &mut edge,
            vec![
                ("addr", Value::String(bmp_bare.to_string())),
                ("id", Value::String(bgp_id)),
            ],
        )
        .context("execute PEERS_WITH merge (bmp)")?;
    }

    let bare = crate::registry::strip_port(&update.target);
    let id = format!("{}:{}", bare, event.peer_address);
    let old_state = get_bmp_session_state(conn, &id)?;
    if old_state.as_deref() != Some(event.session_state.as_str()) {
        let detail = serde_json::to_string(&event).context("serialize BMP peer-state detail")?;
        let _ = write_state_change_event(
            conn,
            &update.target,
            "bmp_session_change",
            &detail,
            "bmp",
            ts(update.timestamp_ns),
            update.timestamp_ns,
            event_tx,
            corr_buf,
        )?;
    }
    Ok(())
}

fn write_bmp_route_monitoring(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
    let event: BmpEvent =
        serde_json::from_value(update.value.clone()).context("parse BMP route-monitoring event")?;
    upsert_device(
        conn,
        &update.target,
        &update.vendor,
        &update.hostname,
        &update.role,
        &update.site,
        ts(update.timestamp_ns),
    )?;
    upsert_bmp_session(conn, update, &event)?;
    let now = ts(update.timestamp_ns);
    let rib_type = event.rib_type.as_deref().unwrap_or("adj-rib-in-pre-policy");
    let rib_bare = crate::registry::strip_port(&update.target);
    for route in &event.route_entries {
        let id = format!(
            "{}:{}:{}:{}:{}/{}",
            rib_bare, event.peer_address, rib_type, route.afi_safi, route.prefix, route.prefix_len
        );
        let mut stmt = conn.prepare(
            "MERGE (r:BgpRibEntry {id: $id}) \
             ON CREATE SET \
               r.device_address = $addr, r.peer_address = $peer, r.afi_safi = $afi_safi, \
               r.prefix = $prefix, r.prefix_len = $prefix_len, r.action = $action, \
               r.next_hop = $next_hop, r.as_path_json = $as_path_json, \
               r.communities_json = $communities_json, r.med = $med, r.local_pref = $local_pref, \
               r.rib_type = $rib_type, r.updated_at = $ts \
             ON MATCH SET \
               r.action = $action, r.next_hop = $next_hop, r.as_path_json = $as_path_json, \
               r.communities_json = $communities_json, r.med = $med, r.local_pref = $local_pref, \
               r.rib_type = $rib_type, r.updated_at = $ts",
        )?;
        conn.execute(
            &mut stmt,
            vec![
                ("id", Value::String(id.clone())),
                ("addr", Value::String(rib_bare.to_string())),
                ("peer", Value::String(event.peer_address.clone())),
                ("afi_safi", Value::String(route.afi_safi.clone())),
                ("prefix", Value::String(route.prefix.clone())),
                ("prefix_len", Value::Int64(route.prefix_len as i64)),
                ("action", Value::String(route.action.clone())),
                ("next_hop", Value::String(route.next_hop.clone())),
                (
                    "as_path_json",
                    Value::String(serde_json::to_string(&route.as_path)?),
                ),
                (
                    "communities_json",
                    Value::String(serde_json::to_string(&route.communities)?),
                ),
                ("med", Value::Int64(route.med.unwrap_or_default() as i64)),
                (
                    "local_pref",
                    Value::Int64(route.local_pref.unwrap_or_default() as i64),
                ),
                ("rib_type", Value::String(rib_type.to_string())),
                ("ts", now.clone()),
            ],
        )?;
        let mut edge_stmt = conn.prepare(
            "MATCH (d:Device {address: $addr}), (r:BgpRibEntry {id: $id}) \
             MERGE (d)-[:HAS_RIB_ENTRY]->(r)",
        )?;
        conn.execute(
            &mut edge_stmt,
            vec![
                ("addr", Value::String(rib_bare.to_string())),
                ("id", Value::String(id)),
            ],
        )?;
    }
    let detail = serde_json::json!({
        "peer_address": event.peer_address,
        "router_address": event.router_address,
        "route_count": event.route_entries.len(),
        "route_entries": event.route_entries,
    });
    let _ = write_state_change_event(
        conn,
        &update.target,
        "bmp_route_change",
        &serde_json::to_string(&detail)?,
        "bmp",
        now,
        update.timestamp_ns,
        event_tx,
        corr_buf,
    )?;
    Ok(())
}

/// D4-11 T4: Write BMP Initiation TLVs (sysDescr, sysName) to Device node.
/// Critical for FRR nodes which have BMP only and no gNMI — this is their
/// only source of system identity.
fn write_bmp_initiation(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
) -> Result<()> {
    let event: BmpEvent =
        serde_json::from_value(update.value.clone()).context("parse BMP initiation event")?;
    upsert_device(
        conn,
        &update.target,
        &update.vendor,
        &update.hostname,
        &update.role,
        &update.site,
        ts(update.timestamp_ns),
    )?;
    // Write BMP identity properties onto Device node
    let sys_name = event.sys_name.unwrap_or_default();
    let sys_descr = event.sys_descr.unwrap_or_default();
    let admin_string = event.init_admin_string.unwrap_or_default();
    if !sys_name.is_empty() || !sys_descr.is_empty() {
        let init_bare = crate::registry::strip_port(&update.target);
        let mut stmt = conn.prepare(
            "MATCH (d:Device {address: $addr}) \
             SET d.bmp_sys_name = $sys_name, \
                 d.bmp_sys_descr = $sys_descr, \
                 d.bmp_admin_string = $admin, \
                 d.updated_at = $ts",
        ).context("prepare BMP initiation Device update")?;
        conn.execute(
            &mut stmt,
            vec![
                ("addr", Value::String(init_bare.to_string())),
                ("sys_name", Value::String(sys_name)),
                ("sys_descr", Value::String(sys_descr)),
                ("admin", Value::String(admin_string)),
                ("ts", ts(update.timestamp_ns)),
            ],
        ).context("execute BMP initiation Device update")?;
    }
    Ok(())
}

/// D4-11 T2: Write BMP STATISTICS_REPORT counters to BmpSession node.
/// Updates adj_rib_in_routes, loc_rib_routes, prefixes_rejected, updates_invalid.
/// Fires a `bgp_rib_prefix_spike` StateChangeEvent when adj_rib_in_routes > 100 000,
/// which enters the CorrelationBuffer and may produce a detection.
fn write_bmp_statistics_report(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
    use crate::streaming::bmp::BmpEvent;
    let event: BmpEvent =
        serde_json::from_value(update.value.clone()).context("parse BMP statistics event")?;
    upsert_device(
        conn,
        &update.target,
        &update.vendor,
        &update.hostname,
        &update.role,
        &update.site,
        ts(update.timestamp_ns),
    )?;
    upsert_bmp_session(conn, update, &event)?;

    // Aggregate the stats entries into named counters
    let mut adj_rib_in: i64 = 0;
    let mut loc_rib: i64 = 0;
    let mut prefixes_rejected: i64 = 0;
    let mut updates_invalid: i64 = 0;
    for s in &event.stats {
        match s.stat_type {
            0 => prefixes_rejected += s.value as i64,
            3 | 4 | 5 | 6 => updates_invalid += s.value as i64,
            7 => adj_rib_in = s.value.min(i64::MAX as u64) as i64,
            8 => loc_rib = s.value.min(i64::MAX as u64) as i64,
            _ => {}
        }
    }

    let session_id = format!("{}:{}", update.target, event.peer_address);
    // D4-16 T3: Read previous adj_rib_in BEFORE we overwrite it (for policy filter spike detection).
    let prev_adj: i64 = if adj_rib_in > 0 {
        let mut q = conn
            .prepare("MATCH (s:BmpSession {id: $id}) RETURN s.adj_rib_in_routes")
            .unwrap_or_else(|_| conn.prepare("RETURN 0").unwrap());
        conn.execute(&mut q, vec![("id", Value::String(session_id.clone()))])
            .and_then(|mut qr| Ok(qr.next().map(|row| read_i64(&row[0])).unwrap_or(0)))
            .unwrap_or(0)
    } else {
        0
    };

    let stats_query =
        "MATCH (s:BmpSession {id: $id}) \
         SET s.adj_rib_in_routes = $adj, s.loc_rib_routes = $loc, \
             s.prefixes_rejected = $rej, s.updates_invalid = $inv";
    let mut stmt = match conn.prepare(stats_query) {
        Ok(stmt) => stmt,
        Err(initial_error) => {
            // Older runtime DBs may be missing the D4-11 BMP stats columns.
            // Attempt the additive migration inline and retry once so one BMP
            // report cannot poison unrelated telemetry batch commits.
            let _ = conn.query("ALTER TABLE BmpSession ADD adj_rib_in_routes INT64 DEFAULT 0");
            let _ = conn.query("ALTER TABLE BmpSession ADD loc_rib_routes INT64 DEFAULT 0");
            let _ = conn.query("ALTER TABLE BmpSession ADD prefixes_rejected INT64 DEFAULT 0");
            let _ = conn.query("ALTER TABLE BmpSession ADD updates_invalid INT64 DEFAULT 0");
            let _ = conn.query(
                "ALTER TABLE BmpSession ADD stats_updated_at TIMESTAMP_NS DEFAULT timestamp_ns('1970-01-01')",
            );
            conn.prepare(stats_query).with_context(|| {
                format!("prepare BmpSession stats update after migration retry: {initial_error}")
            })?
        }
    };
    conn.execute(
        &mut stmt,
        vec![
            ("id",  Value::String(session_id.clone())),
            ("adj", Value::Int64(adj_rib_in)),
            ("loc", Value::Int64(loc_rib)),
            ("rej", Value::Int64(prefixes_rejected)),
            ("inv", Value::Int64(updates_invalid)),
        ],
    )
    .context("execute BmpSession stats update")?;

    // D4-16 T3: bgp_policy_filter_spike — Adj-RIB-In drops >20% without session going down
    if adj_rib_in > 0 {
        // Fire if previous was meaningful and current dropped >20%
        if prev_adj > 10 && adj_rib_in < (prev_adj * 80 / 100) {
            let drop_pct = ((prev_adj - adj_rib_in) as f64 / prev_adj as f64 * 100.0) as i64;
            // Check if there's a recent ConfigChange within ±60s
            let config_change_corr: String = {
                let window_ns = 60_000_000_000_i64;
                let ts_val = update.timestamp_ns;
                let mut cq = conn
                    .prepare(
                        "MATCH (c:ConfigChange)-[:CHANGED_ON]->(d:Device {address: $addr}) \
                         WHERE c.timestamp > $lo AND c.timestamp < $hi \
                         RETURN c.change_description LIMIT 1"
                    )
                    .unwrap_or_else(|_| conn.prepare("RETURN ''").unwrap());
                conn.execute(&mut cq, vec![
                    ("addr", Value::String(crate::registry::strip_port(&update.target).to_string())),
                    ("lo", Value::Int64(ts_val - window_ns)),
                    ("hi", Value::Int64(ts_val + window_ns)),
                ])
                .ok()
                .and_then(|mut qr| qr.next().map(|row| read_str(&row[0])))
                .unwrap_or_default()
            };
            let detail = serde_json::json!({
                "peer_address": event.peer_address,
                "session_id": session_id,
                "previous_adj_rib_in": prev_adj,
                "current_adj_rib_in": adj_rib_in,
                "drop_percent": drop_pct,
                "config_change_correlated": !config_change_corr.is_empty(),
                "config_change_description": config_change_corr,
            })
            .to_string();
            let _ = write_state_change_event(
                conn,
                &update.target,
                "bgp_policy_filter_spike",
                &detail,
                "bmp",
                ts(update.timestamp_ns),
                update.timestamp_ns,
                event_tx,
                corr_buf,
            );
        }
    }

    // bgp_rib_prefix_spike: fire when adj_rib_in > 100 000 prefixes
    const PREFIX_SPIKE_THRESHOLD: i64 = 100_000;
    if adj_rib_in > PREFIX_SPIKE_THRESHOLD {
        let detail = serde_json::json!({
            "peer_address": event.peer_address,
            "session_id": session_id,
            "adj_rib_in_routes": adj_rib_in,
            "loc_rib_routes": loc_rib,
            "threshold": PREFIX_SPIKE_THRESHOLD,
        })
        .to_string();
        let _ = write_state_change_event(
            conn,
            &update.target,
            "bgp_rib_prefix_spike",
            &detail,
            "bmp",
            ts(update.timestamp_ns),
            update.timestamp_ns,
            event_tx,
            corr_buf,
        );
    }
    Ok(())
}

/// D4-20 T3: Write environmental sensor reading to SensorReading node + REPORTED_BY(→Device).
/// D4-20 T4: Fire thermal_sensor_critical or thermal_sensor_warning state change event
///           when temperature exceeds 85°C (critical) or 75°C (warning) thresholds.
fn write_env_sensor(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    component_name: &str,
    sensor_type: &str,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
    use crate::telemetry::json_f64;
    upsert_device(
        conn,
        &update.target,
        &update.vendor,
        &update.hostname,
        &update.role,
        &update.site,
        ts(update.timestamp_ns),
    )?;

    let value_raw = &update.value;

    let reading_value = if sensor_type == "fan" {
        json_f64(value_raw, "fan-speed")
    } else if sensor_type == "voltage" {
        json_f64(value_raw, "voltage")
    } else {
        // temperature: pick best non-zero across multiple key names
        let t1 = json_f64(value_raw, "temperature");
        let t2 = json_f64(value_raw, "current-temperature");
        let t3 = json_f64(value_raw, "temperature-input");
        [t1, t2, t3].into_iter().find(|&v| v > 0.0).unwrap_or(0.0)
    };

    let unit = match sensor_type {
        "temperature" => "celsius",
        "voltage"     => "volt",
        "fan"         => "rpm",
        _             => "",
    };

    let sensor_id = format!("{}:{}:{}", update.target, component_name, sensor_type);
    let now = ts(update.timestamp_ns);

    let mut stmt = conn.prepare(
        "MERGE (s:SensorReading {id: $id}) \
         ON CREATE SET s.device_address = $dev, s.component_name = $comp, \
           s.sensor_type = $stype, s.unit = $unit, \
           s.threshold_warning = 75.0, s.threshold_critical = 85.0, \
           s.value = $val, s.updated_at = $ts \
         ON MATCH SET s.value = $val, s.updated_at = $ts",
    )
    .context("prepare SensorReading upsert")?;
    let sensor_bare = crate::registry::strip_port(&update.target);
    conn.execute(&mut stmt, vec![
        ("id",   Value::String(sensor_id.clone())),
        ("dev",  Value::String(sensor_bare.to_string())),
        ("comp", Value::String(component_name.to_string())),
        ("stype",Value::String(sensor_type.to_string())),
        ("unit", Value::String(unit.to_string())),
        ("val",  Value::Double(reading_value)),
        ("ts",   now.clone()),
    ])
    .context("execute SensorReading upsert")?;

    // REPORTED_BY edge: SensorReading → Device
    let mut edge_stmt = conn.prepare(
        "MATCH (s:SensorReading {id: $id}), (d:Device {address: $dev}) \
         MERGE (s)-[:REPORTED_BY]->(d)",
    )
    .context("prepare REPORTED_BY edge")?;
    conn.execute(&mut edge_stmt, vec![
        ("id",  Value::String(sensor_id.clone())),
        ("dev", Value::String(sensor_bare.to_string())),
    ])
    .context("execute REPORTED_BY edge")?

    // D4-20 T4: Thermal detection rules
    if sensor_type == "temperature" && reading_value > 0.0 {
        const WARN_THRESHOLD: f64 = 75.0;
        const CRIT_THRESHOLD: f64 = 85.0;
        let (event_type, severity_label) = if reading_value >= CRIT_THRESHOLD {
            ("thermal_sensor_critical", "critical")
        } else if reading_value >= WARN_THRESHOLD {
            ("thermal_sensor_warning", "warning")
        } else {
            return Ok(());
        };
        let detail = serde_json::json!({
            "component_name": component_name,
            "sensor_id": sensor_id,
            "temperature_c": reading_value,
            "threshold": if reading_value >= CRIT_THRESHOLD { CRIT_THRESHOLD } else { WARN_THRESHOLD },
            "severity": severity_label,
        })
        .to_string();
        let _ = write_state_change_event(
            conn,
            &update.target,
            event_type,
            &detail,
            "gnmi",
            ts(update.timestamp_ns),
            update.timestamp_ns,
            event_tx,
            corr_buf,
        );
    }
    Ok(())
}

/// D4-20 T3: Write optical transceiver diagnostics to OpticsTelemetry node + OPTICS_ON(→Interface).
fn write_optics_diagnostics(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    if_name: &str,
) -> Result<()> {
    use crate::telemetry::json_f64;
    upsert_device(
        conn,
        &update.target,
        &update.vendor,
        &update.hostname,
        &update.role,
        &update.site,
        ts(update.timestamp_ns),
    )?;

    let v = &update.value;
    let rx_power = json_f64(v, "rx-power")
        .max(json_f64(v, "rx-optical-power"))
        .max(json_f64(v, "receive-power"));
    let tx_power = json_f64(v, "tx-power")
        .max(json_f64(v, "transmit-power"))
        .max(json_f64(v, "output-power"));
    let temperature = json_f64(v, "temperature").max(json_f64(v, "laser-temp"));
    let bias_current = json_f64(v, "bias-current").max(json_f64(v, "laser-bias-current"));
    let wavelength = json_f64(v, "wavelength");

    let optics_id = format!("{}:{}", update.target, if_name);
    let now = ts(update.timestamp_ns);

    let mut stmt = conn.prepare(
        "MERGE (o:OpticsTelemetry {id: $id}) \
         ON CREATE SET o.device_address = $dev, o.interface_name = $iface, \
           o.rx_power_dbm = $rx, o.tx_power_dbm = $tx, o.temperature_c = $temp, \
           o.bias_current_ma = $bias, o.wavelength_nm = $wave, o.updated_at = $ts \
         ON MATCH SET o.rx_power_dbm = $rx, o.tx_power_dbm = $tx, o.temperature_c = $temp, \
           o.bias_current_ma = $bias, o.wavelength_nm = $wave, o.updated_at = $ts",
    )
    .context("prepare OpticsTelemetry upsert")?;
    let optics_bare = crate::registry::strip_port(&update.target);
    conn.execute(&mut stmt, vec![
        ("id",   Value::String(optics_id.clone())),
        ("dev",  Value::String(optics_bare.to_string())),
        ("iface",Value::String(if_name.to_string())),
        ("rx",   Value::Double(rx_power)),
        ("tx",   Value::Double(tx_power)),
        ("temp", Value::Double(temperature)),
        ("bias", Value::Double(bias_current)),
        ("wave", Value::Double(wavelength)),
        ("ts",   now),
    ])
    .context("execute OpticsTelemetry upsert")?;

    // OPTICS_ON edge: OpticsTelemetry → Interface
    let iface_id = format!("{}:{}", optics_bare, if_name);
    let mut edge_stmt = conn.prepare(
        "MATCH (o:OpticsTelemetry {id: $oid}), (i:Interface {id: $iid}) \
         MERGE (o)-[:OPTICS_ON]->(i)",
    )
    .context("prepare OPTICS_ON edge")?;
    conn.execute(&mut edge_stmt, vec![
        ("oid", Value::String(optics_id)),
        ("iid", Value::String(iface_id)),
    ])
    .context("execute OPTICS_ON edge")?;

    Ok(())
}

fn write_bgp_ls_state(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
    let event: BgpLsEvent =
        serde_json::from_value(update.value.clone()).context("parse BGP-LS event")?;
    upsert_device(
        conn,
        &update.target,
        &update.vendor,
        &update.hostname,
        &update.role,
        &update.site,
        ts(update.timestamp_ns),
    )?;
    match &event {
        BgpLsEvent::Node {
            router_id,
            protocol,
            asn,
            name,
            sr_node_sid,
            ..
        } => {
            let bare = crate::registry::strip_port(&update.target);
            let id = format!("{}:{router_id}", bare);
            let mut stmt = conn.prepare(
                "MERGE (n:BgpLsNode {id: $id}) \
                 ON CREATE SET \
                   n.device_address = $addr, n.router_id = $router_id, n.protocol = $protocol, \
                   n.asn = $asn, n.name = $name, n.sr_node_sid = $sr_node_sid, n.updated_at = $ts \
                 ON MATCH SET \
                   n.protocol = $protocol, n.asn = $asn, n.name = $name, \
                   n.sr_node_sid = $sr_node_sid, n.updated_at = $ts",
            )?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("addr", Value::String(bare.to_string())),
                    ("router_id", Value::String(router_id.clone())),
                    ("protocol", Value::String(protocol.clone())),
                    ("asn", Value::Int64(asn.unwrap_or_default() as i64)),
                    ("name", Value::String(name.clone().unwrap_or_default())),
                    (
                        "sr_node_sid",
                        Value::Int64(sr_node_sid.unwrap_or_default() as i64),
                    ),
                    ("ts", ts(update.timestamp_ns)),
                ],
            )?;
            let mut edge_stmt = conn.prepare(
                "MATCH (d:Device {address: $addr}), (n:BgpLsNode {id: $id}) \
                 MERGE (d)-[:HAS_BGPLS_NODE]->(n)",
            )?;
            conn.execute(
                &mut edge_stmt,
                vec![
                    ("addr", Value::String(bare.to_string())),
                    ("id", Value::String(id)),
                ],
            )?;
        }
        BgpLsEvent::Link {
            local_router_id,
            remote_router_id,
            protocol,
            local_interface,
            remote_interface,
            igp_metric,
            te_metric,
            unreserved_bandwidth_bps,
            admin_groups,
            srlgs,
            ..
        } => {
            let bare = crate::registry::strip_port(&update.target);
            let id = format!("{}:{local_router_id}->{remote_router_id}", bare);
            let mut stmt = conn.prepare(
                "MERGE (l:BgpLsLink {id: $id}) \
                 ON CREATE SET \
                   l.device_address = $addr, l.local_router_id = $local_router_id, \
                   l.remote_router_id = $remote_router_id, l.protocol = $protocol, \
                   l.local_interface = $local_interface, l.remote_interface = $remote_interface, \
                   l.igp_metric = $igp_metric, l.te_metric = $te_metric, \
                   l.unreserved_bandwidth_bps = $unreserved_bandwidth_bps, \
                   l.admin_groups_json = $admin_groups_json, l.srlgs_json = $srlgs_json, \
                   l.updated_at = $ts \
                 ON MATCH SET \
                   l.protocol = $protocol, l.local_interface = $local_interface, \
                   l.remote_interface = $remote_interface, l.igp_metric = $igp_metric, \
                   l.te_metric = $te_metric, l.unreserved_bandwidth_bps = $unreserved_bandwidth_bps, \
                   l.admin_groups_json = $admin_groups_json, l.srlgs_json = $srlgs_json, \
                   l.updated_at = $ts",
            )?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("addr", Value::String(bare.to_string())),
                    ("local_router_id", Value::String(local_router_id.clone())),
                    ("remote_router_id", Value::String(remote_router_id.clone())),
                    ("protocol", Value::String(protocol.clone())),
                    (
                        "local_interface",
                        Value::String(local_interface.clone().unwrap_or_default()),
                    ),
                    (
                        "remote_interface",
                        Value::String(remote_interface.clone().unwrap_or_default()),
                    ),
                    (
                        "igp_metric",
                        Value::Int64(igp_metric.unwrap_or_default() as i64),
                    ),
                    (
                        "te_metric",
                        Value::Int64(te_metric.unwrap_or_default() as i64),
                    ),
                    (
                        "unreserved_bandwidth_bps",
                        Value::Int64(unreserved_bandwidth_bps.unwrap_or_default() as i64),
                    ),
                    (
                        "admin_groups_json",
                        Value::String(serde_json::to_string(
                            &admin_groups.clone().unwrap_or_default(),
                        )?),
                    ),
                    (
                        "srlgs_json",
                        Value::String(serde_json::to_string(&srlgs.clone().unwrap_or_default())?),
                    ),
                    ("ts", ts(update.timestamp_ns)),
                ],
            )?;
            let mut edge_stmt = conn.prepare(
                "MATCH (d:Device {address: $addr}), (l:BgpLsLink {id: $id}) \
                 MERGE (d)-[:HAS_BGPLS_LINK]->(l)",
            )?;
            conn.execute(
                &mut edge_stmt,
                vec![
                    ("addr", Value::String(bare.to_string())),
                    ("id", Value::String(id)),
                ],
            )?;
        }
        BgpLsEvent::SrPolicy {
            name,
            endpoint,
            color,
            preference,
            binding_sid,
            status,
            ..
        } => {
            let bare = crate::registry::strip_port(&update.target);
            let id = format!("{}:{}:{}", bare, color, endpoint);
            let old_status = get_sr_policy_status(conn, &id)?;
            let new_status = status.clone().unwrap_or_else(|| "unknown".to_string());
            let mut stmt = conn.prepare(
                "MERGE (p:SrPolicy {id: $id}) \
                 ON CREATE SET \
                   p.device_address = $addr, p.name = $name, p.endpoint = $endpoint, \
                   p.color = $color, p.preference = $preference, p.binding_sid = $binding_sid, \
                   p.status = $status, p.updated_at = $ts \
                 ON MATCH SET \
                   p.name = $name, p.preference = $preference, p.binding_sid = $binding_sid, \
                   p.status = $status, p.updated_at = $ts",
            )?;
            conn.execute(
                &mut stmt,
                vec![
                    ("id", Value::String(id.clone())),
                    ("addr", Value::String(bare.to_string())),
                    ("name", Value::String(name.clone())),
                    ("endpoint", Value::String(endpoint.clone())),
                    ("color", Value::Int64(*color as i64)),
                    (
                        "preference",
                        Value::Int64(preference.unwrap_or_default() as i64),
                    ),
                    (
                        "binding_sid",
                        Value::Int64(binding_sid.unwrap_or_default() as i64),
                    ),
                    ("status", Value::String(new_status.clone())),
                    ("ts", ts(update.timestamp_ns)),
                ],
            )?;
            let mut edge_stmt = conn.prepare(
                "MATCH (d:Device {address: $addr}), (p:SrPolicy {id: $id}) \
                 MERGE (d)-[:HAS_SR_POLICY]->(p)",
            )?;
            conn.execute(
                &mut edge_stmt,
                vec![
                    ("addr", Value::String(bare.to_string())),
                    ("id", Value::String(id.clone())),
                ],
            )?;
            if old_status.as_deref() != Some(new_status.as_str()) {
                let detail = serde_json::to_string(&event)?;
                let _ = write_state_change_event(
                    conn,
                    &update.target,
                    "sr_policy_change",
                    &detail,
                    "bgp_ls",
                    ts(update.timestamp_ns),
                    update.timestamp_ns,
                    event_tx,
                    corr_buf,
                )?;
            }
        }
    }
    Ok(())
}

fn upsert_bmp_session(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    event: &BmpEvent,
) -> Result<()> {
    let bare = crate::registry::strip_port(&update.target);
    let id = format!("{}:{}", bare, event.peer_address);
    let mut stmt = conn.prepare(
        "MERGE (s:BmpSession {id: $id}) \
         ON CREATE SET \
           s.device_address = $addr, s.router_address = $router_address, \
           s.peer_address = $peer_address, s.peer_as = $peer_as, s.peer_bgp_id = $peer_bgp_id, \
           s.session_state = $session_state, s.last_message_type = $last_message_type, s.updated_at = $ts \
         ON MATCH SET \
           s.router_address = $router_address, s.peer_as = $peer_as, s.peer_bgp_id = $peer_bgp_id, \
           s.session_state = $session_state, s.last_message_type = $last_message_type, s.updated_at = $ts",
    )?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("addr", Value::String(bare.to_string())),
            (
                "router_address",
                Value::String(event.router_address.clone()),
            ),
            ("peer_address", Value::String(event.peer_address.clone())),
            ("peer_as", Value::Int64(event.peer_as as i64)),
            ("peer_bgp_id", Value::String(event.peer_bgp_id.clone())),
            ("session_state", Value::String(event.session_state.clone())),
            (
                "last_message_type",
                Value::String(event.message_type.clone()),
            ),
            ("ts", ts(update.timestamp_ns)),
        ],
    )?;
    let mut edge_stmt = conn.prepare(
        "MATCH (d:Device {address: $addr}), (s:BmpSession {id: $id}) \
         MERGE (d)-[:HAS_BMP_SESSION]->(s)",
    )?;
    conn.execute(
        &mut edge_stmt,
        vec![
            ("addr", Value::String(bare.to_string())),
            ("id", Value::String(id)),
        ],
    )?;

    // Register both addresses as DeviceAddress candidates so resolve_peer_to_device()
    // can find them even if the remote device is only known via BMP (no gNMI).
    if !event.peer_address.is_empty() {
        let _ = crate::graph::common::upsert_device_address(
            conn, &event.peer_address, "bmp_peer", ts(update.timestamp_ns),
        );
    }
    if !event.router_address.is_empty() {
        let _ = crate::graph::common::upsert_device_address(
            conn, &event.router_address, "bmp_router", ts(update.timestamp_ns),
        );
    }

    // MONITORS_BGP_ROUTER: collector Device → monitored router Device (via router_address).
    // router_address is the BGP router-id of the monitored device, which typically equals
    // its loopback0 IP. Resolves when the router is also a gNMI-onboarded device or its
    // loopback is registered via extra_ips.
    if !event.router_address.is_empty() {
        if let Some(router_device) = resolve_peer_to_device(conn, &event.router_address) {
            let mut mon = conn.prepare(
                "MATCH (a:Device {address: $src}), (b:Device {address: $dst}) \
                 MERGE (a)-[r:MONITORS_BGP_ROUTER {source: 'bmp'}]->(b)",
            )?;
            let _ = conn.execute(
                &mut mon,
                vec![
                    ("src", Value::String(bare.to_string())),
                    ("dst", Value::String(router_device.clone())),
                ],
            );
            debug!(collector = %bare, router = %router_device, "MONITORS_BGP_ROUTER resolved");
        }
    }

    // BGP_SESSION_WITH from BMP perspective: the BMP-reported device (update.target) has
    // a BGP session with the peer (event.peer_address). Both may be known devices.
    if !event.peer_address.is_empty() {
        if let Some(peer_device) = resolve_peer_to_device(conn, &event.peer_address) {
            let mut bgp_d2d = conn.prepare(
                "MATCH (a:Device {address: $src}), (b:Device {address: $dst}) \
                 MERGE (a)-[r:BGP_SESSION_WITH {source: 'bmp'}]->(b) \
                 SET r.session_state = $state, r.peer_as = $peer_as",
            )?;
            let _ = conn.execute(
                &mut bgp_d2d,
                vec![
                    ("src", Value::String(bare.to_string())),
                    ("dst", Value::String(peer_device.clone())),
                    ("state", Value::String(event.session_state.clone())),
                    ("peer_as", Value::Int64(event.peer_as as i64)),
                ],
            );
            debug!(src = %bare, dst = %peer_device, "BGP_SESSION_WITH (bmp) resolved");
        }
    }

    Ok(())
}

fn get_bmp_session_state(conn: &Connection<'_>, id: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("MATCH (s:BmpSession {id: $id}) RETURN s.session_state")
        .context("prepare BMP session state lookup")?;
    let mut result = conn
        .execute(&mut stmt, vec![("id", Value::String(id.to_string()))])
        .context("execute BMP session state lookup")?;
    Ok(result.next().and_then(|row| {
        if let Value::String(s) = &row[0] {
            Some(s.clone())
        } else {
            None
        }
    }))
}

fn get_sr_policy_status(conn: &Connection<'_>, id: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("MATCH (p:SrPolicy {id: $id}) RETURN p.status")
        .context("prepare SR policy status lookup")?;
    let mut result = conn
        .execute(&mut stmt, vec![("id", Value::String(id.to_string()))])
        .context("execute SR policy status lookup")?;
    Ok(result.next().and_then(|row| {
        if let Value::String(s) = &row[0] {
            Some(s.clone())
        } else {
            None
        }
    }))
}

fn write_lldp_neighbor(
    conn: &Connection<'_>,
    u: &TelemetryUpdate,
    local_if: &str,
    neighbor_id: &str,
    val: &serde_json::Value,
) -> Result<()> {
    let bare = crate::registry::strip_port(&u.target);
    let id = format!("{}:{}:{}", bare, local_if, neighbor_id);
    let now = ts(u.timestamp_ns);

    upsert_device(conn, &u.target, &u.vendor, &u.hostname, "", "", now.clone())?;

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
            ("addr", Value::String(bare.to_string())),
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
            ("addr", Value::String(bare.to_string())),
            ("id", Value::String(id)),
        ],
    )
    .context("execute HAS_LLDP_NEIGHBOR merge")?;

    // Best-effort: link the local Interface to the remote Interface via LLDP data.
    let system_name = json_str(val, "system-name").to_string();
    let port_id = json_str(val, "port-id").to_string();
    let chassis_id = json_str(val, "chassis-id").to_string();
    if !system_name.is_empty()
        && !port_id.is_empty()
        && let Err(e) = try_connect_interfaces(
            conn,
            &u.target,
            local_if,
            &system_name,
            &port_id,
            is_mgmt_interface(local_if),
        )
    {
        debug!(error = %e, local_if, system_name, port_id, "interface link skipped");
    }

    // Track B4: HostEndpoint fallback.
    // If the LLDP peer does not match any Device (by hostname or address), create a
    // HostEndpoint so the endpoint is at least visible in the graph topology.
    // This is arch-agnostic: works for campus workstations, printers, phones, etc.
    // A HostEndpoint with kind="unknown" is a placeholder that NetBox enrichment or
    // operator data can promote to a richer record later.
    if !chassis_id.is_empty() {
        // Register chassis_id as a DeviceAddress candidate (Cisco/Junos put mgmt IP here;
        // even when it's a MAC it's harmless — resolve_peer_to_device will reject non-IPs).
        let _ = crate::graph::common::upsert_device_address(conn, &chassis_id, "lldp", now.clone());

        // Resolve the peer to a known Device using the 3-tier lookup:
        //   1. Device.address exact match (chassis_id == mgmt IP — Cisco IOS-XR, Arista)
        //   2. DeviceAddress.ip (loopback registered as extra_ip)
        //   3. EntityIdentity.mgmt_ip
        // Also try by hostname in case chassis_id is a MAC but system_name matches Device.hostname.
        let peer_device_addr: Option<String> = resolve_peer_to_device(conn, &chassis_id)
            .or_else(|| {
                // Hostname fallback — Nokia SRL, Arista, FRR all set system-name = hostname
                if system_name.is_empty() {
                    return None;
                }
                let mut chk = conn
                    .prepare("MATCH (d:Device {hostname: $hn}) RETURN d.address LIMIT 1")
                    .ok()?;
                let mut rows = conn
                    .execute(&mut chk, vec![("hn", Value::String(system_name.clone()))])
                    .ok()?;
                if let Some(row) = rows.next() {
                    if let Value::String(addr) = &row[0] {
                        if !addr.is_empty() {
                            return Some(addr.clone());
                        }
                    }
                }
                None
            });

        let peer_is_known_device = peer_device_addr.is_some();

        // When peer is a known Device, create a direct LLDP_NEIGHBOR(Device→Device) edge
        // so the topology view can render L2 adjacencies without relying on Interface nodes.
        if let Some(ref peer_addr) = peer_device_addr {
            let mut lldp_d2d = conn
                .prepare(
                    "MATCH (a:Device {address: $src}), (b:Device {address: $dst}) \
                     MERGE (a)-[r:LLDP_NEIGHBOR {local_if: $lif, source: 'lldp'}]->(b) \
                     SET r.remote_if = $rif",
                )
                .context("prepare LLDP_NEIGHBOR merge")?;
            let _ = conn.execute(
                &mut lldp_d2d,
                vec![
                    ("src", Value::String(bare.to_string())),
                    ("dst", Value::String(peer_addr.clone())),
                    ("lif", Value::String(local_if.to_string())),
                    ("rif", Value::String(port_id.clone())),
                ],
            );
            debug!(src = %bare, dst = %peer_addr, local_if, "LLDP_NEIGHBOR D2D resolved");
        }

        if !peer_is_known_device {
            let now_ns = u.timestamp_ns;
            if let Err(e) = upsert_host_endpoint(
                conn,
                &chassis_id,
                "unknown",
                &system_name,
                &chassis_id,
                "",
                "",
                "",
                "lldp",
                now_ns,
            ) {
                debug!(error = %e, chassis_id, "HostEndpoint upsert from LLDP skipped");
            } else {
                // Wire CONNECTED_TO: HostEndpoint → local Interface of the observing device
                let local_iface_id = format!("{}:{local_if}", bare);
                let _ = link_host_endpoint_to_interface(conn, &chassis_id, &local_iface_id);
                debug!(chassis_id, system_name, "HostEndpoint created from LLDP");
            }
        }
    }

    // EntityIdentity: update the identity record for the LLDP peer using chassis_id.
    // If the peer is already a known Device (same hostname in Device table), the
    // HAS_IDENTITY edge will link the existing Device to this identity record.
    if !chassis_id.is_empty() || !system_name.is_empty() {
        // Use system_name as the key when available (matches Device.hostname).
        // device_address here is the LLDP peer's address (chassis_id as proxy).
        let peer_addr = if !chassis_id.is_empty() { &chassis_id } else { "" };
        if let Err(e) = crate::graph::common::upsert_entity_identity(
            conn,
            peer_addr,
            &system_name,
            &chassis_id,
            "",
            "lldp",
            u.timestamp_ns,
        ) {
            debug!(error = %e, chassis_id, "EntityIdentity update from LLDP skipped");
        }
    }

    info!(
        target = %u.target,
        local_if = %local_if,
        chassis_id = %chassis_id,
        system_name = %system_name,
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
            let _ = try_connect_interfaces(
                conn,
                local_addr,
                local_if,
                &system_name,
                &port_id,
                is_mgmt_interface(local_if),
            );
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
        let _ = try_connect_interfaces(
            conn,
            &remote_addr,
            &remote_if,
            &system_name,
            local_if,
            is_mgmt_interface(&remote_if),
        );
    }

    Ok(())
}

/// Returns true when `if_name` is a management-plane interface that should produce
/// MGMT_LINK edges instead of fabric CONNECTED_TO edges.
fn is_mgmt_interface(if_name: &str) -> bool {
    let lo = if_name.to_lowercase();
    lo.starts_with("mgmt")          // Nokia SR Linux mgmt0, Cisco Mgmt0/...
        || lo.starts_with("management") // Arista Management1, Juniper
        || lo == "eth0"                 // FRR / generic Linux
        || lo.starts_with("fxp0")       // Juniper fxp0
        || lo.starts_with("me0")        // Juniper me0
        || lo.starts_with("em0") // Juniper em0
}

/// Resolve the remote Interface by hostname+port_id and MERGE either a CONNECTED_TO
/// (fabric) or MGMT_LINK (management-plane) edge depending on `is_mgmt`.
/// Returns Ok(()) if the remote is not yet in the graph — caller treats that as a no-op.
fn try_connect_interfaces(
    conn: &Connection<'_>,
    local_addr: &str,
    local_if: &str,
    remote_hostname: &str,
    remote_port_id: &str,
    is_mgmt: bool,
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

    let cypher = if is_mgmt {
        "MATCH (li:Interface {id: $lid}), (ri:Interface {id: $rid}) \
         MERGE (li)-[:MGMT_LINK]->(ri)"
    } else {
        "MATCH (li:Interface {id: $lid}), (ri:Interface {id: $rid}) \
         MERGE (li)-[:CONNECTED_TO]->(ri)"
    };

    let mut edge_stmt = conn
        .prepare(cypher)
        .context("prepare interface link merge")?;
    conn.execute(
        &mut edge_stmt,
        vec![
            ("lid", Value::String(local_if_id)),
            ("rid", Value::String(remote_if_id)),
        ],
    )
    .context("execute interface link merge")?;

    Ok(())
}

const VALID_ARCHETYPES: &[&str] = &[
    ARCHETYPE_DATA_CENTER,
    ARCHETYPE_CAMPUS_WIRED,
    ARCHETYPE_CAMPUS_WIRELESS,
    ARCHETYPE_SERVICE_PROVIDER,
    ARCHETYPE_HOME_LAB,
];

fn normalize_environment(mut env: EnvironmentRecord) -> Result<EnvironmentRecord> {
    env.name = env.name.trim().to_string();
    if env.name.is_empty() {
        anyhow::bail!("environment name is required");
    }
    env.id = env.id.trim().to_string();
    if env.id.is_empty() {
        env.id = site_id_from_name(&env.name);
    }
    env.archetype = env.archetype.trim().to_ascii_lowercase();
    if !VALID_ARCHETYPES.contains(&env.archetype.as_str()) {
        anyhow::bail!(
            "invalid archetype '{}'; valid values: {}",
            env.archetype,
            VALID_ARCHETYPES.join(", ")
        );
    }
    if env.metadata_json.trim().is_empty() {
        env.metadata_json = "{}".to_string();
    }
    if env.created_at_ns == 0 {
        env.created_at_ns = now_ns();
    }
    Ok(env)
}

fn upsert_environment_record(conn: &Connection<'_>, env: &EnvironmentRecord) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "MERGE (e:Environment {id: $id}) \
             ON CREATE SET \
               e.name = $name, e.archetype = $archetype, \
               e.created_at = $ts, e.metadata_json = $metadata_json \
             ON MATCH SET \
               e.name = $name, e.archetype = $archetype, \
               e.metadata_json = $metadata_json",
        )
        .context("prepare Environment upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(env.id.clone())),
            ("name", Value::String(env.name.clone())),
            ("archetype", Value::String(env.archetype.clone())),
            ("ts", ts(env.created_at_ns)),
            ("metadata_json", Value::String(env.metadata_json.clone())),
        ],
    )
    .context("execute Environment upsert")?;
    Ok(())
}

fn link_site_to_environment(conn: &Connection<'_>, site_id: &str, env_id: &str) -> Result<()> {
    let mut clear = conn
        .prepare("MATCH (s:Site {id: $sid})-[r:BELONGS_TO_ENVIRONMENT]->(:Environment) DELETE r")
        .context("prepare BELONGS_TO_ENVIRONMENT clear")?;
    conn.execute(
        &mut clear,
        vec![("sid", Value::String(site_id.to_string()))],
    )
    .context("execute BELONGS_TO_ENVIRONMENT clear")?;

    let mut link = conn
        .prepare(
            "MATCH (s:Site {id: $sid}), (e:Environment {id: $eid}) \
             MERGE (s)-[:BELONGS_TO_ENVIRONMENT]->(e)",
        )
        .context("prepare BELONGS_TO_ENVIRONMENT edge")?;
    conn.execute(
        &mut link,
        vec![
            ("sid", Value::String(site_id.to_string())),
            ("eid", Value::String(env_id.to_string())),
        ],
    )
    .context("execute BELONGS_TO_ENVIRONMENT edge")?;
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
    site.environment_id = site.environment_id.trim().to_string();
    Ok(site)
}

pub fn site_id_from_name(name: &str) -> String {
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

    if !site.environment_id.is_empty() {
        link_site_to_environment(conn, &site.id, &site.environment_id)?;
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
        environment_id: if row.len() > 7 {
            read_str(&row[7])
        } else {
            String::new()
        },
    }
}

fn environment_from_row(row: Vec<Value>) -> EnvironmentRecord {
    EnvironmentRecord {
        id: read_str(&row[0]),
        name: read_str(&row[1]),
        archetype: read_str(&row[2]),
        created_at_ns: read_ts_ns(&row[3]),
        metadata_json: read_str(&row[4]),
    }
}

fn emit_oper_status_event(
    conn: &Connection<'_>,
    u: &TelemetryUpdate,
    if_name: &str,
    oper_status: &str,
    event_tx: &broadcast::Sender<BonsaiEvent>,
    corr_buf: &CorrelationBuffer,
) -> Result<()> {
    let bare = crate::registry::strip_port(&u.target);
    let id = format!("{}:{}", bare, if_name);
    let normalized_oper_status = oper_status.to_ascii_lowercase();
    let previous_oper_status = read_interface_oper_status(conn, &id)?;

    upsert_device(
        conn,
        &u.target,
        &u.vendor,
        &u.hostname,
        "",
        "",
        ts(u.timestamp_ns),
    )?;

    let mut stmt = conn
        .prepare(
            "MERGE (i:Interface {id: $id}) \
         ON CREATE SET \
           i.device_address = $addr, i.name = $name, \
           i.oper_status = $oper_status, i.updated_at = $ts \
         ON MATCH SET \
           i.oper_status = $oper_status, i.updated_at = $ts",
        )
        .context("prepare interface oper-status upsert")?;
    conn.execute(
        &mut stmt,
        vec![
            ("id", Value::String(id.clone())),
            ("addr", Value::String(bare.to_string())),
            ("name", Value::String(if_name.to_string())),
            ("oper_status", Value::String(normalized_oper_status.clone())),
            ("ts", ts(u.timestamp_ns)),
        ],
    )
    .context("execute interface oper-status upsert")?;

    let mut edge_stmt = conn
        .prepare(
            "MATCH (d:Device {address: $addr}), (i:Interface {id: $id}) \
         MERGE (d)-[:HAS_INTERFACE]->(i)",
        )
        .context("prepare device-interface edge")?;
    conn.execute(
        &mut edge_stmt,
        vec![
            ("addr", Value::String(bare.to_string())),
            ("id", Value::String(id)),
        ],
    )
    .context("execute device-interface edge")?;

    if previous_oper_status.as_deref() == Some(normalized_oper_status.as_str()) {
        debug!(
            target = %u.target,
            if_name,
            oper_status = %normalized_oper_status,
            "interface oper-status unchanged; event suppressed"
        );
        return Ok(());
    }

    let event_type = match normalized_oper_status.as_str() {
        "down" => "interface_down",
        "up"   => "interface_up",
        _      => "interface_oper_status_change",
    };
    let detail = serde_json::json!({
        "if_name": if_name,
        "interface_name": if_name,
        "old_state": previous_oper_status.clone().unwrap_or_default(),
        "new_state": normalized_oper_status,
        "oper_status": normalized_oper_status,
    })
    .to_string();
    debug!(target = %u.target, if_name, oper_status, "interface oper-status event emitted");
    write_state_change_event(
        conn,
        &u.target,
        event_type,
        &detail,
        "gnmi",
        ts(u.timestamp_ns),
        u.timestamp_ns,
        event_tx,
        corr_buf,
    )?;
    Ok(())
}

fn read_interface_oper_status(conn: &Connection<'_>, id: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("MATCH (i:Interface {id: $id}) RETURN i.oper_status")
        .context("prepare interface oper-status lookup")?;
    let rows = conn
        .execute(&mut stmt, vec![("id", Value::String(id.to_string()))])
        .context("execute interface oper-status lookup")?
        .collect::<Vec<_>>();
    Ok(rows
        .first()
        .map(|row| read_str(&row[0]))
        .filter(|v| !v.is_empty()))
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
    use crate::telemetry::TelemetryUpdate;
    use tokio::time::{Duration, timeout};

    fn temp_graph_path(label: &str) -> String {
        std::env::temp_dir()
            .join(format!("bonsai-{}-{}", label, Uuid::new_v4()))
            .to_string_lossy()
            .into_owned()
    }

    #[tokio::test]
    async fn syslog_fact_joined_event_is_emitted_for_known_bgp_neighbor() {
        let path = temp_graph_path("syslog-fact-bgp");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open graph store");
        store
            .write(TelemetryUpdate {
                target: "leaf1".to_string(),
                vendor: "nokia_srl".to_string(),
                hostname: "leaf1".to_string(),
                role: "leaf".to_string(),
                site: "dc-a".to_string(),
                timestamp_ns: 10,
                path:
                    "network-instance[name=default]/protocols/bgp/neighbor[peer-address=10.1.0.1]"
                        .to_string(),
                value: serde_json::json!({
                    "session-state": "established",
                    "peer-as": 65101,
                    "established-transitions": 4,
                }),
            })
            .await
            .expect("seed BGP neighbor");

        let mut rx = store.subscribe_events();
        store
            .write(TelemetryUpdate {
                target: "leaf1".to_string(),
                vendor: "nokia_srl".to_string(),
                hostname: "leaf1".to_string(),
                role: "leaf".to_string(),
                site: "dc-a".to_string(),
                timestamp_ns: 11,
                path: "signals/syslog_fact/bgp_neighbor".to_string(),
                value: serde_json::to_value(SyslogFact {
                    timestamp_ns: 11,
                    fact_type: "bgp_neighbor".to_string(),
                    category: "protocol".to_string(),
                    hostname: "leaf1".to_string(),
                    source_vendor: "nokia_srl".to_string(),
                    message: "BGP neighbor 10.1.0.1 down".to_string(),
                    raw: "raw".to_string(),
                    transport: "udp".to_string(),
                    peer_addr: "127.0.0.1:5514".to_string(),
                    field_schema: std::collections::BTreeMap::from([
                        ("peer_address".to_string(), "string".to_string()),
                        ("new_state".to_string(), "string".to_string()),
                    ]),
                    fields: std::collections::BTreeMap::from([
                        ("peer_address".to_string(), "10.1.0.1".to_string()),
                        ("new_state".to_string(), "down".to_string()),
                    ]),
                })
                .expect("serialize syslog fact"),
            })
            .await
            .expect("write syslog fact");

        let event = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for joined syslog fact")
            .expect("receive joined syslog fact event");
        assert_eq!(event.event_type, "syslog_fact_joined");
        let detail: serde_json::Value =
            serde_json::from_str(&event.detail_json).expect("detail json");
        assert_eq!(detail["fact_type"], "bgp_neighbor");
        assert_eq!(detail["join"]["status"], "joined");
        assert_eq!(detail["join"]["kind"], "bgp_neighbor");
        assert_eq!(
            detail["join"]["graph_state"]["session_state"],
            "established"
        );
    }

    #[tokio::test]
    async fn syslog_fact_orphan_event_is_emitted_for_unknown_interface() {
        let path = temp_graph_path("syslog-fact-orphan");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open graph store");
        let mut rx = store.subscribe_events();
        store
            .write(TelemetryUpdate {
                target: "leaf2".to_string(),
                vendor: "nokia_srl".to_string(),
                hostname: "leaf2".to_string(),
                role: "leaf".to_string(),
                site: "dc-a".to_string(),
                timestamp_ns: 12,
                path: "signals/syslog_fact/interface_state".to_string(),
                value: serde_json::to_value(SyslogFact {
                    timestamp_ns: 12,
                    fact_type: "interface_state".to_string(),
                    category: "protocol".to_string(),
                    hostname: "leaf2".to_string(),
                    source_vendor: "nokia_srl".to_string(),
                    message: "Interface ethernet-1/99 changed state to down".to_string(),
                    raw: "raw".to_string(),
                    transport: "udp".to_string(),
                    peer_addr: "127.0.0.1:5514".to_string(),
                    field_schema: std::collections::BTreeMap::from([
                        ("if_name".to_string(), "string".to_string()),
                        ("new_state".to_string(), "string".to_string()),
                    ]),
                    fields: std::collections::BTreeMap::from([
                        ("if_name".to_string(), "ethernet-1/99".to_string()),
                        ("new_state".to_string(), "down".to_string()),
                    ]),
                })
                .expect("serialize syslog fact"),
            })
            .await
            .expect("write syslog fact");

        let event = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for orphan syslog fact")
            .expect("receive orphan syslog fact event");
        assert_eq!(event.event_type, "syslog_fact_orphan");
        let detail: serde_json::Value =
            serde_json::from_str(&event.detail_json).expect("detail json");
        assert_eq!(detail["fact_type"], "interface_state");
        assert_eq!(detail["join"]["status"], "orphan");
        assert_eq!(detail["join"]["reason"], "no_interface_match");
    }

    #[tokio::test]
    async fn syslog_bfd_fact_joins_to_known_bfd_session_by_interface() {
        let path = temp_graph_path("syslog-fact-bfd-join");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open graph store");

        // Seed a BFD session via OpenConfig path: interface id = "ethernet-1/1"
        store
            .write(TelemetryUpdate {
                target: "leaf3".to_string(),
                vendor: "nokia_srl".to_string(),
                hostname: "leaf3".to_string(),
                role: "leaf".to_string(),
                site: "dc-a".to_string(),
                timestamp_ns: 20,
                path: "bfd/interfaces/interface[id=ethernet-1/1]/peers/peer[local-discriminator=5001]/state".to_string(),
                value: serde_json::json!({
                    "session-state": "up",
                    "remote-address": "10.2.0.1",
                    "local-address": "10.2.0.2",
                }),
            })
            .await
            .expect("seed BFD session");

        let mut rx = store.subscribe_events();
        store
            .write(TelemetryUpdate {
                target: "leaf3".to_string(),
                vendor: "nokia_srl".to_string(),
                hostname: "leaf3".to_string(),
                role: "leaf".to_string(),
                site: "dc-a".to_string(),
                timestamp_ns: 21,
                path: "signals/syslog_fact/bfd_session".to_string(),
                value: serde_json::to_value(SyslogFact {
                    timestamp_ns: 21,
                    fact_type: "bfd_session".to_string(),
                    category: "protocol".to_string(),
                    hostname: "leaf3".to_string(),
                    source_vendor: "nokia_srl".to_string(),
                    message: "BFD session on interface ethernet-1/1 went to down".to_string(),
                    raw: "raw".to_string(),
                    transport: "udp".to_string(),
                    peer_addr: "127.0.0.1:5514".to_string(),
                    field_schema: std::collections::BTreeMap::from([
                        ("if_name".to_string(), "string".to_string()),
                        ("new_state".to_string(), "string".to_string()),
                    ]),
                    fields: std::collections::BTreeMap::from([
                        ("if_name".to_string(), "ethernet-1/1".to_string()),
                        ("new_state".to_string(), "down".to_string()),
                    ]),
                })
                .expect("serialize bfd syslog fact"),
            })
            .await
            .expect("write bfd syslog fact");

        let event = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for BFD syslog fact event")
            .expect("receive BFD syslog fact event");
        assert_eq!(event.event_type, "syslog_fact_joined");
        let detail: serde_json::Value =
            serde_json::from_str(&event.detail_json).expect("detail json");
        assert_eq!(detail["fact_type"], "bfd_session");
        assert_eq!(detail["join"]["status"], "joined");
        assert_eq!(detail["join"]["kind"], "bfd_session");
        assert_eq!(detail["join"]["graph_state"]["session_state"], "up");
        assert_eq!(detail["join"]["graph_state"]["remote_address"], "10.2.0.1");
    }

    #[tokio::test]
    async fn syslog_bfd_fact_orphan_for_unknown_session() {
        let path = temp_graph_path("syslog-fact-bfd-orphan");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open graph store");
        let mut rx = store.subscribe_events();
        store
            .write(TelemetryUpdate {
                target: "leaf4".to_string(),
                vendor: "nokia_srl".to_string(),
                hostname: "leaf4".to_string(),
                role: "leaf".to_string(),
                site: "dc-a".to_string(),
                timestamp_ns: 30,
                path: "signals/syslog_fact/bfd_session".to_string(),
                value: serde_json::to_value(SyslogFact {
                    timestamp_ns: 30,
                    fact_type: "bfd_session".to_string(),
                    category: "protocol".to_string(),
                    hostname: "leaf4".to_string(),
                    source_vendor: "arista".to_string(),
                    message: "BFD peer 10.99.0.1 changed state to down".to_string(),
                    raw: "raw".to_string(),
                    transport: "udp".to_string(),
                    peer_addr: "127.0.0.1:5514".to_string(),
                    field_schema: std::collections::BTreeMap::from([
                        ("remote_address".to_string(), "string".to_string()),
                        ("new_state".to_string(), "string".to_string()),
                    ]),
                    fields: std::collections::BTreeMap::from([
                        ("remote_address".to_string(), "10.99.0.1".to_string()),
                        ("new_state".to_string(), "down".to_string()),
                    ]),
                })
                .expect("serialize bfd orphan syslog fact"),
            })
            .await
            .expect("write bfd orphan syslog fact");

        let event = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for BFD orphan event")
            .expect("receive BFD orphan event");
        assert_eq!(event.event_type, "syslog_fact_orphan");
        let detail: serde_json::Value =
            serde_json::from_str(&event.detail_json).expect("detail json");
        assert_eq!(detail["fact_type"], "bfd_session");
        assert_eq!(detail["join"]["status"], "orphan");
        assert_eq!(detail["join"]["kind"], "bfd_session");
        assert_eq!(detail["join"]["reason"], "no_bfd_session_match");
    }

    #[tokio::test]
    async fn syslog_ospf_fact_orphans_when_no_ospf_neighbor_in_graph() {
        let path = temp_graph_path("syslog-fact-ospf-orphan");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open graph store");
        let mut rx = store.subscribe_events();
        store
            .write(TelemetryUpdate {
                target: "spine1".to_string(),
                vendor: "cisco_iosxr".to_string(),
                hostname: "spine1".to_string(),
                role: "spine".to_string(),
                site: "dc-a".to_string(),
                timestamp_ns: 40,
                path: "signals/syslog_fact/ospf_neighbor".to_string(),
                value: serde_json::to_value(SyslogFact {
                    timestamp_ns: 40,
                    fact_type: "ospf_neighbor".to_string(),
                    category: "protocol".to_string(),
                    hostname: "spine1".to_string(),
                    source_vendor: "cisco_iosxr".to_string(),
                    message:
                        "OSPF neighbor 10.0.0.2 on interface GigabitEthernet0/0/0 changed to down"
                            .to_string(),
                    raw: "raw".to_string(),
                    transport: "udp".to_string(),
                    peer_addr: "127.0.0.1:5514".to_string(),
                    field_schema: std::collections::BTreeMap::from([
                        ("neighbor_address".to_string(), "string".to_string()),
                        ("if_name".to_string(), "string".to_string()),
                        ("new_state".to_string(), "string".to_string()),
                    ]),
                    fields: std::collections::BTreeMap::from([
                        ("neighbor_address".to_string(), "10.0.0.2".to_string()),
                        ("if_name".to_string(), "GigabitEthernet0/0/0".to_string()),
                        ("new_state".to_string(), "down".to_string()),
                    ]),
                })
                .expect("serialize ospf syslog fact"),
            })
            .await
            .expect("write ospf syslog fact");

        let event = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for OSPF orphan event")
            .expect("receive OSPF orphan event");
        assert_eq!(event.event_type, "syslog_fact_orphan");
        let detail: serde_json::Value =
            serde_json::from_str(&event.detail_json).expect("detail json");
        assert_eq!(detail["fact_type"], "ospf_neighbor");
        assert_eq!(detail["join"]["status"], "orphan");
        assert_eq!(detail["join"]["kind"], "ospf_neighbor");
        assert_eq!(detail["join"]["reason"], "no_ospf_neighbor_match");
        // Confirm if_name did NOT trigger an Interface join — ospf is routed before generic if_name branch
        assert_ne!(detail["join"]["kind"], "interface");
    }

    #[tokio::test]
    async fn syslog_isis_fact_orphans_when_no_isis_adjacency_in_graph() {
        let path = temp_graph_path("syslog-fact-isis-orphan");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open graph store");
        let mut rx = store.subscribe_events();
        store
            .write(TelemetryUpdate {
                target: "spine2".to_string(),
                vendor: "juniper".to_string(),
                hostname: "spine2".to_string(),
                role: "spine".to_string(),
                site: "dc-a".to_string(),
                timestamp_ns: 50,
                path: "signals/syslog_fact/isis_adjacency".to_string(),
                value: serde_json::to_value(SyslogFact {
                    timestamp_ns: 50,
                    fact_type: "isis_adjacency".to_string(),
                    category: "protocol".to_string(),
                    hostname: "spine2".to_string(),
                    source_vendor: "juniper".to_string(),
                    message: "IS-IS adjacency with 0000.0000.0001 on ge-0/0/0 went down"
                        .to_string(),
                    raw: "raw".to_string(),
                    transport: "udp".to_string(),
                    peer_addr: "127.0.0.1:5514".to_string(),
                    field_schema: std::collections::BTreeMap::from([
                        ("neighbor_id".to_string(), "string".to_string()),
                        ("if_name".to_string(), "string".to_string()),
                        ("new_state".to_string(), "string".to_string()),
                    ]),
                    fields: std::collections::BTreeMap::from([
                        ("neighbor_id".to_string(), "0000.0000.0001".to_string()),
                        ("if_name".to_string(), "ge-0/0/0".to_string()),
                        ("new_state".to_string(), "down".to_string()),
                    ]),
                })
                .expect("serialize isis syslog fact"),
            })
            .await
            .expect("write isis syslog fact");

        let event = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for IS-IS orphan event")
            .expect("receive IS-IS orphan event");
        assert_eq!(event.event_type, "syslog_fact_orphan");
        let detail: serde_json::Value =
            serde_json::from_str(&event.detail_json).expect("detail json");
        assert_eq!(detail["fact_type"], "isis_adjacency");
        assert_eq!(detail["join"]["status"], "orphan");
        assert_eq!(detail["join"]["kind"], "isis_adjacency");
        assert_eq!(detail["join"]["reason"], "no_isis_adjacency_match");
        // Confirm if_name did NOT trigger an Interface join
        assert_ne!(detail["join"]["kind"], "interface");
    }

    #[test]
    fn backfill_remediation_trust_marks_marks_legacy_rows() {
        let path = temp_graph_path("trust-backfill");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open graph store");

        // Insert test data in a scoped block so the connection is dropped (and its
        // writes fully committed to lbug) before backfill opens its own connection.
        // lbug 0.15.x does not expose auto-committed writes to a second concurrent
        // connection until the first connection is closed.
        {
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
        } // conn dropped — writes now visible to a new connection

        store
            .backfill_remediation_trust_marks()
            .expect("backfill trust marks");

        // Re-open connection for verification queries.
        let conn = Connection::new(&store.db).expect("verify connection");
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
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open graph store");
        let conn = Connection::new(&store.db).expect("graph connection");

        upsert_device(
            &conn,
            "dut:57400",
            "nokia_srl",
            "dut1",
            "",
            "",
            ts(1_000_000_000),
        )
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
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open graph store");

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
                collector_id: None,
                created_at_ns: 0,
                updated_at_ns: 0,
                created_by: String::new(),
                updated_by: String::new(),
                last_operator_action: String::new(),
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
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open graph store");

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
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open graph store");

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
            environment_id: String::new(),
        }
    }

    // ── Investigation tests (T3-1) ────────────────────────────────────────────

    #[tokio::test]
    async fn create_and_get_investigation() {
        let path = temp_graph_path("investigation-create");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open");
        let inv = store
            .create_investigation("det-1".into(), "10.0.0.1:57400".into(), "operator".into())
            .await
            .expect("create");
        assert!(!inv.id.is_empty());
        assert_eq!(inv.status, "running");

        let got = store
            .get_investigation(inv.id.clone())
            .await
            .expect("get")
            .expect("should exist");
        assert_eq!(got.detection_id, "det-1");
        assert_eq!(got.trigger, "operator");
    }

    #[tokio::test]
    async fn complete_investigation_updates_status() {
        let path = temp_graph_path("investigation-complete");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open");
        let inv = store
            .create_investigation("det-2".into(), "10.0.0.2:57400".into(), "auto".into())
            .await
            .expect("create");

        store
            .complete_investigation(
                inv.id.clone(),
                "complete".into(),
                "BGP session restored after interface reset.".into(),
                "".into(),
                1234,
                0.001,
            )
            .await
            .expect("complete");

        let got = store
            .get_investigation(inv.id)
            .await
            .expect("get")
            .expect("exists");
        assert_eq!(got.status, "complete");
        assert_eq!(got.tokens_used, 1234);
        assert!(got.summary.contains("BGP"));
    }

    #[tokio::test]
    async fn list_investigations_returns_newest_first() {
        let path = temp_graph_path("investigation-list");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open");
        let _ = store
            .create_investigation("d1".into(), "10.0.0.1".into(), "auto".into())
            .await
            .expect("c1");
        let _ = store
            .create_investigation("d2".into(), "10.0.0.2".into(), "auto".into())
            .await
            .expect("c2");
        let list = store.list_investigations().await.expect("list");
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn add_and_list_tool_calls() {
        let path = temp_graph_path("tool-calls");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open");
        let inv = store
            .create_investigation("det-3".into(), "10.0.0.3:57400".into(), "operator".into())
            .await
            .expect("create");

        store
            .add_tool_call(
                inv.id.clone(),
                "get_blast_radius".into(),
                r#"{"device_address":"10.0.0.3:57400"}"#.into(),
                r#"{"devices":["10.0.0.4"]}"#.into(),
            )
            .await
            .expect("add tool call 1");

        store
            .add_tool_call(
                inv.id.clone(),
                "summarise".into(),
                r#"{"text":"BGP session down."}"#.into(),
                r#"{"summary":"BGP session down."}"#.into(),
            )
            .await
            .expect("add tool call 2");

        let calls = store.list_tool_calls(inv.id.clone()).await.expect("list");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].tool_name, "get_blast_radius");
        assert_eq!(calls[1].tool_name, "summarise");
    }

    #[tokio::test]
    async fn get_investigation_returns_none_for_unknown() {
        let path = temp_graph_path("investigation-missing");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open");
        let result = store
            .get_investigation("nonexistent-id".into())
            .await
            .expect("no error");
        assert!(result.is_none());
    }

    // ── DeviceEmbedding tests (T2-1 / T5-3) ──────────────────────────────────

    #[tokio::test]
    async fn write_and_read_device_embedding_roundtrip() {
        let path = temp_graph_path("embedding-roundtrip");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open store");
        let conn = Connection::new(&store.db).expect("conn");
        upsert_device(
            &conn,
            "10.0.0.1:57400",
            "nokia_srl",
            "spine1",
            "",
            "",
            ts(1_000_000_000),
        )
        .expect("seed device");
        drop(conn);

        let rec = EmbeddingRecord {
            device_address: "10.0.0.1:57400".to_string(),
            version: "spectral_v1".to_string(),
            algorithm: "spectral".to_string(),
            dimension: 4,
            vector: vec![0.1, 0.2, 0.3, 0.4],
            computed_at_ns: 1_000_000_000,
        };
        store
            .write_device_embeddings(vec![rec.clone()])
            .await
            .expect("write embedding");

        let results = store
            .list_device_embeddings("10.0.0.1:57400".to_string())
            .await
            .expect("list embeddings");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].version, "spectral_v1");
        assert_eq!(results[0].algorithm, "spectral");
        assert_eq!(results[0].dimension, 4);
        let diff: f64 = results[0]
            .vector
            .iter()
            .zip(&rec.vector)
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(
            diff < 1e-9,
            "vector roundtrip should be lossless; diff={}",
            diff
        );
    }

    #[tokio::test]
    async fn write_device_embedding_multiple_versions() {
        let path = temp_graph_path("embedding-versions");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open store");
        let conn = Connection::new(&store.db).expect("conn");
        upsert_device(
            &conn,
            "10.0.0.2:57400",
            "nokia_srl",
            "leaf1",
            "",
            "",
            ts(1_000_000_000),
        )
        .expect("seed device");
        drop(conn);

        let v1 = EmbeddingRecord {
            device_address: "10.0.0.2:57400".to_string(),
            version: "spectral_v1".to_string(),
            algorithm: "spectral".to_string(),
            dimension: 4,
            vector: vec![0.1, 0.2, 0.3, 0.4],
            computed_at_ns: 1_000_000_000,
        };
        let v2 = EmbeddingRecord {
            device_address: "10.0.0.2:57400".to_string(),
            version: "spectral_v2".to_string(),
            algorithm: "spectral".to_string(),
            dimension: 8,
            vector: vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8],
            computed_at_ns: 2_000_000_000,
        };
        store
            .write_device_embeddings(vec![v1, v2])
            .await
            .expect("write embeddings");

        let results = store
            .list_device_embeddings("10.0.0.2:57400".to_string())
            .await
            .expect("list embeddings");

        assert_eq!(results.len(), 2, "both versions should be stored");
        // newest first (computed_at DESC)
        assert_eq!(results[0].version, "spectral_v2");
        assert_eq!(results[0].dimension, 8);
        assert_eq!(results[1].version, "spectral_v1");
        assert_eq!(results[1].dimension, 4);
    }

    #[tokio::test]
    async fn write_device_embedding_upsert_overwrites_same_version() {
        let path = temp_graph_path("embedding-upsert");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open store");
        let conn = Connection::new(&store.db).expect("conn");
        upsert_device(
            &conn,
            "10.0.0.3:57400",
            "nokia_srl",
            "leaf2",
            "",
            "",
            ts(1_000_000_000),
        )
        .expect("seed device");
        drop(conn);

        let original = EmbeddingRecord {
            device_address: "10.0.0.3:57400".to_string(),
            version: "spectral_v1".to_string(),
            algorithm: "spectral".to_string(),
            dimension: 4,
            vector: vec![0.1, 0.2, 0.3, 0.4],
            computed_at_ns: 1_000_000_000,
        };
        store
            .write_device_embeddings(vec![original])
            .await
            .expect("first write");

        let updated = EmbeddingRecord {
            device_address: "10.0.0.3:57400".to_string(),
            version: "spectral_v1".to_string(),
            algorithm: "spectral_updated".to_string(),
            dimension: 4,
            vector: vec![0.9, 0.8, 0.7, 0.6],
            computed_at_ns: 2_000_000_000,
        };
        store
            .write_device_embeddings(vec![updated])
            .await
            .expect("upsert write");

        let results = store
            .list_device_embeddings("10.0.0.3:57400".to_string())
            .await
            .expect("list embeddings");

        assert_eq!(results.len(), 1, "upsert should not duplicate");
        assert_eq!(results[0].algorithm, "spectral_updated");
        assert!((results[0].vector[0] - 0.9).abs() < 1e-9);
    }

    #[tokio::test]
    async fn list_device_embeddings_returns_empty_for_unknown_device() {
        let path = temp_graph_path("embedding-empty");
        let store = GraphStore::open(&path, 256 * 1024 * 1024).expect("open store");
        let results = store
            .list_device_embeddings("does.not.exist:57400".to_string())
            .await
            .expect("list should not error");
        assert!(results.is_empty());
    }
}

// ─── New Telemetry Event Handlers for Immediate Wins ────────────────────────

/// Write interface description for service discovery
fn write_interface_description(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    if_name: &str,
    description: &str,
) -> Result<()> {
    let bare = crate::registry::strip_port(&update.target);
    let now = ts(update.timestamp_ns);
    
    // Update Interface node with description
    let mut stmt = conn.prepare(
        "MATCH (d:Device {address: $device_address})-[:HAS_INTERFACE]->(i:Interface {name: $if_name}) \
         SET i.description = $description, i.updated_at_ns = $timestamp_ns"
    ).context("prepare update interface description")?;
    conn.execute(&mut stmt, vec![
        ("device_address", Value::String(bare.to_string())),
        ("if_name", Value::String(if_name.to_string())),
        ("description", Value::String(description.to_string())),
        ("timestamp_ns", Value::Int64(update.timestamp_ns)),
    ])
    .context("update interface description")?
    
    // Trigger service discovery if description contains service indicators
    if description.to_lowercase().contains("api") 
        || description.to_lowercase().contains("database")
        || description.to_lowercase().contains("cache")
        || description.to_lowercase().contains("gateway")
        || description.to_lowercase().contains("load")
        || description.to_lowercase().contains("service")
    {
        // Create a state change event for service discovery
        let _ = write_state_change_event(
            conn,
            &update.target,
            "service_discovery_candidate",
            &serde_json::json!({
                "interface": if_name,
                "description": description,
                "discovery_method": "interface_description"
            }).to_string(),
            "gNMI",
            now,
            update.timestamp_ns,
            &broadcast::Sender::new(1),
            &crate::correlation_buffer::CorrelationBuffer::new(45),
        );
    }
    
    Ok(())
}

/// Write service endpoint from telemetry
fn write_service_endpoint(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    if_name: &str,
    service_type: &str,
    service_name: &str,
    confidence: f64,
) -> Result<()> {
    let bare = crate::registry::strip_port(&update.target);
    let endpoint_id = format!("service-{}-{}-{}", bare, if_name, service_type);
    
    // Create or update ServiceEndpoint node
    let mut stmt = conn.prepare(
        "MERGE (se:ServiceEndpoint {id: $endpoint_id}) \
         SET se.device_address = $device_address, \
             se.interface_name = $interface_name, \
             se.service_type = $service_type, \
             se.service_name = $service_name, \
             se.endpoint_type = $endpoint_type, \
             se.discovered_via = $discovered_via, \
             se.confidence_score = $confidence_score, \
             se.updated_at_ns = $updated_at_ns"
    ).context("prepare create service endpoint")?;
    conn.execute(&mut stmt, vec![
        ("endpoint_id", Value::String(endpoint_id.clone())),
        ("device_address", Value::String(bare.to_string())),
        ("interface_name", Value::String(if_name.to_string())),
        ("service_type", Value::String(service_type.to_string())),
        ("service_name", Value::String(service_name.to_string())),
        ("endpoint_type", Value::String("internal".to_string())),
        ("discovered_via", Value::String("telemetry".to_string())),
        ("confidence_score", Value::Double(confidence)),
        ("updated_at_ns", Value::Int64(update.timestamp_ns)),
    ])
    .context("create service endpoint")?;
    
    // Create relationship to device
    let mut stmt = conn.prepare(
        "MATCH (d:Device {address: $device_address}), (se:ServiceEndpoint {id: $endpoint_id}) \
         MERGE (d)-[:HOSTS_SERVICE {role: $endpoint_type, updated_at: $updated_at}]->(se)"
    ).context("prepare service endpoint relationship")?;
    conn.execute(&mut stmt, vec![
        ("device_address", Value::String(bare.to_string())),
        ("endpoint_id", Value::String(endpoint_id)),
        ("endpoint_type", Value::String("internal".to_string())),
        ("updated_at", Value::Int64(update.timestamp_ns)),
    ])
    .context("create service endpoint relationship")?;
    
    Ok(())
}

/// Write QoS policy change
fn write_qos_policy_change(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    policy_name: &str,
    _action: &str,
    interface_name: Option<&str>,
) -> Result<()> {
    let bare = crate::registry::strip_port(&update.target);
    let policy_id = format!("qos-{}-{}", bare, policy_name);
    
    // Create or update QoSPolicy node
    let mut stmt = conn.prepare(
        "MERGE (qp:QoSPolicy {id: $policy_id}) \
         SET qp.device_address = $device_address, \
             qp.policy_name = $policy_name, \
             qp.policy_type = $policy_type, \
             qp.updated_at_ns = $updated_at_ns"
    ).context("prepare create qos policy")?;
    conn.execute(&mut stmt, vec![
        ("policy_id", Value::String(policy_id.clone())),
        ("device_address", Value::String(bare.to_string())),
        ("policy_name", Value::String(policy_name.to_string())),
        ("policy_type", Value::String("dynamic".to_string())),
        ("updated_at_ns", Value::Int64(update.timestamp_ns)),
    ])
    .context("create qos policy")?;
    
    // If interface is specified, create relationship
    if let Some(if_name) = interface_name {
        let mut stmt = conn.prepare(
            "MATCH (i:Interface {name: $if_name}), (qp:QoSPolicy {id: $policy_id}) \
             WHERE i.device_address = $device_address \
             MERGE (i)-[:APPLIES_QOS {direction: $direction, updated_at: $updated_at}]->(qp)"
        ).context("prepare qos interface relationship")?;
        conn.execute(&mut stmt, vec![
            ("if_name", Value::String(if_name.to_string())),
            ("policy_id", Value::String(policy_id)),
            ("device_address", Value::String(bare.to_string())),
            ("direction", Value::String("ingress".to_string())),
            ("updated_at", Value::Int64(update.timestamp_ns)),
        ])
        .context("create qos interface relationship")?;
    }
    
    Ok(())
}

// ─── Syslog Fact Join Handlers for Immediate Wins ─────────────────────────────

/// Join service process syslog facts to create ServiceEndpoint nodes
fn join_service_process_fact(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    fact: &SyslogFact,
) -> Result<JsonValue> {
    let service_name = fact.fields.get("service_name").map(String::as_str).unwrap_or("unknown");
    let action = fact.fields.get("action").map(String::as_str).unwrap_or("unknown");
    
    // Determine service type from service name
    let service_type = if service_name.contains("nginx") || service_name.contains("haproxy") || service_name.contains("envoy") {
        "load_balancer"
    } else if service_name.contains("postgres") || service_name.contains("mysql") || service_name.contains("mongodb") {
        "database"
    } else if service_name.contains("redis") || service_name.contains("memcached") {
        "cache"
    } else if service_name.contains("kafka") || service_name.contains("rabbitmq") {
        "message_queue"
    } else if service_name.contains("elasticsearch") || service_name.contains("solr") {
        "search_engine"
    } else if service_name.contains("istio") || service_name.contains("consul") {
        "service_mesh"
    } else {
        "application_server"
    };
    
    // Create ServiceEndpoint node
    let svc_bare = crate::registry::strip_port(&update.target);
    let endpoint_id = format!("service-{}-{}-{}", svc_bare, service_name, service_type);
    
    let mut stmt = conn.prepare(
        "MERGE (se:ServiceEndpoint {id: $endpoint_id}) \
         SET se.device_address = $device_address, \
             se.service_name = $service_name, \
             se.service_type = $service_type, \
             se.endpoint_type = $endpoint_type, \
             se.discovered_via = $discovered_via, \
             se.confidence_score = $confidence_score, \
             se.updated_at_ns = $updated_at_ns"
    ).context("prepare create service endpoint from syslog")?;
    conn.execute(&mut stmt, vec![
        ("endpoint_id", Value::String(endpoint_id.clone())),
        ("device_address", Value::String(svc_bare.to_string())),
        ("service_name", Value::String(service_name.to_string())),
        ("service_type", Value::String(service_type.to_string())),
        ("endpoint_type", Value::String("internal".to_string())),
        ("discovered_via", Value::String("syslog".to_string())),
        ("confidence_score", Value::Double(0.8)),
        ("updated_at_ns", Value::Int64(update.timestamp_ns)),
    ])
    .context("create service endpoint from syslog")?;
    
    // Create relationship to device
    let mut stmt = conn.prepare(
        "MATCH (d:Device {address: $device_address}), (se:ServiceEndpoint {id: $endpoint_id}) \
         MERGE (d)-[:HOSTS_SERVICE {role: $endpoint_type, updated_at: $updated_at}]->(se)"
    ).context("prepare service endpoint relationship")?;
    conn.execute(&mut stmt, vec![
        ("device_address", Value::String(svc_bare.to_string())),
        ("endpoint_id", Value::String(endpoint_id)),
        ("endpoint_type", Value::String("internal".to_string())),
        ("updated_at", Value::Int64(update.timestamp_ns)),
    ])
    .context("create service endpoint relationship")?;
    
    Ok(json!({
        "status": "joined",
        "kind": "service_endpoint",
        "service_name": service_name,
        "service_type": service_type,
        "action": action
    }))
}

/// Join QoS policy change syslog facts to create QoSPolicy nodes
fn join_qos_policy_fact(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    fact: &SyslogFact,
) -> Result<JsonValue> {
    let policy_name = fact.fields.get("policy_name").map(String::as_str).unwrap_or("unknown");
    let action = fact.fields.get("action").map(String::as_str).unwrap_or("unknown");
    let interface_name = fact.fields.get("interface_name").cloned();
    
    // Create QoSPolicy node
    let qos_bare = crate::registry::strip_port(&update.target);
    let policy_id = format!("qos-{}-{}", qos_bare, policy_name);
    
    let mut stmt = conn.prepare(
        "MERGE (qp:QoSPolicy {id: $policy_id}) \
         SET qp.device_address = $device_address, \
             qp.policy_name = $policy_name, \
             qp.policy_type = $policy_type, \
             qp.updated_at_ns = $updated_at_ns"
    ).context("prepare create qos policy from syslog")?;
    conn.execute(&mut stmt, vec![
        ("policy_id", Value::String(policy_id.clone())),
        ("device_address", Value::String(qos_bare.to_string())),
        ("policy_name", Value::String(policy_name.to_string())),
        ("policy_type", Value::String("syslog_triggered".to_string())),
        ("updated_at_ns", Value::Int64(update.timestamp_ns)),
    ])
    .context("create qos policy from syslog")?;
    
    // If interface is specified, create relationship
    if let Some(if_name) = &interface_name {
        let mut stmt = conn.prepare(
            "MATCH (i:Interface {name: $if_name}), (qp:QoSPolicy {id: $policy_id}) \
             WHERE i.device_address = $device_address \
             MERGE (i)-[:APPLIES_QOS {direction: $direction, updated_at: $updated_at}]->(qp)"
        ).context("prepare qos interface relationship")?;
        conn.execute(&mut stmt, vec![
            ("if_name", Value::String(if_name.clone())),
            ("policy_id", Value::String(policy_id)),
            ("device_address", Value::String(qos_bare.to_string())),
            ("direction", Value::String("ingress".to_string())),
            ("updated_at", Value::Int64(update.timestamp_ns)),
        ])
        .context("create qos interface relationship")?;
    }
    
    Ok(json!({
        "status": "joined",
        "kind": "qos_policy",
        "policy_name": policy_name,
        "action": action,
        "interface_name": interface_name
    }))
}

/// Join connection anomaly syslog facts to update ServiceEndpoint health
fn join_connection_anomaly_fact(
    conn: &Connection<'_>,
    update: &TelemetryUpdate,
    fact: &SyslogFact,
) -> Result<JsonValue> {
    let service_type = fact.fields.get("service_type").map(String::as_str).unwrap_or("unknown");
    let anomaly_type = fact.fields.get("anomaly_type").map(String::as_str).unwrap_or("unknown");
    
    // Look for existing ServiceEndpoint nodes of this type on the device
    let mut stmt = conn.prepare(
        "MATCH (d:Device {address: $device_address})-[:HOSTS_SERVICE]->(se:ServiceEndpoint) \
         WHERE se.service_type = $service_type \
         RETURN se.id, se.service_name"
    ).context("prepare find service endpoints for anomaly")?;
    let rows: Vec<Vec<Value>> = conn.execute(
        &mut stmt,
        vec![
            ("device_address", Value::String(crate::registry::strip_port(&update.target).to_string())),
            ("service_type", Value::String(service_type.to_string())),
        ],
    )
    .context("find service endpoints for anomaly")?
    .collect();
    let affected_count = rows.len();
    
    for row in rows {
        let endpoint_id = read_str(&row[0]);
        
        // Update ServiceEndpoint with anomaly information
        let mut stmt = conn.prepare(
            "MATCH (se:ServiceEndpoint {id: $endpoint_id}) \
             SET se.last_anomaly_type = $anomaly_type, \
                 se.last_anomaly_at_ns = $timestamp_ns, \
                 se.health_status = $health_status, \
                 se.updated_at_ns = $updated_at_ns"
        ).context("prepare update service endpoint health")?;
        conn.execute(&mut stmt, vec![
            ("endpoint_id", Value::String(endpoint_id)),
            ("anomaly_type", Value::String(anomaly_type.to_string())),
            ("timestamp_ns", Value::Int64(update.timestamp_ns)),
            ("health_status", Value::String("degraded".to_string())),
            ("updated_at_ns", Value::Int64(update.timestamp_ns)),
        ])
        .context("update service endpoint health")?;
    }
    
    Ok(json!({
        "status": "joined",
        "kind": "connection_anomaly",
        "service_type": service_type,
        "anomaly_type": anomaly_type,
        "affected_endpoints": affected_count
    }))
}
