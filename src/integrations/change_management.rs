//! Change Management integration — ServiceNow change_request polling, external
//! webhook ingestion, and change-context correlation.
//!
//! ## Signal Flow
//!
//! ```text
//! ServiceNow change_request table ──poll──▶ ChangeRequest nodes in graph
//! AAP / Ansible Tower / external   ─webhook──▶ ChangeRequest nodes in graph
//! Manual (API)                     ──POST───▶ ChangeRequest nodes in graph
//!
//! On every config_change_event or detection_fired:
//!   → query active ChangeRequest windows for the device
//!   → if match: link DURING_CHANGE edge, tag detection change_correlated=true
//!   → if config_caused_fault + active change: annotate SNOW incident with CHG number
//! ```
//!
//! ## Design Decisions
//!
//! - ChangeRequest is a first-class graph node so NL queries, investigations,
//!   and the agent can reason about planned vs unplanned changes.
//! - The suppression_policy config controls whether detections during change
//!   windows are "annotated" (default — safer) or "suppressed" entirely.
//! - External webhooks use HMAC-SHA256 validation when a secret is configured.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use lbug::{Connection, Value};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::config::{ServiceNowChangeManagementConfig, ServiceNowConfig};
use crate::credentials::{CredentialVault, ResolvePurpose};
use crate::graph::common::{now_ns, read_str, ts};
use crate::graph::GraphStore;

// ── Data types ───────────────────────────────────────────────────────────────

/// A change request record written to the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeRequestRecord {
    pub id: String,
    pub number: String,
    pub source: String,
    pub snow_sys_id: String,
    pub short_description: String,
    pub state: String,
    pub change_type: String,
    pub risk: String,
    pub assigned_to: String,
    pub assignment_group: String,
    pub affected_cis: Vec<String>,
    pub planned_start_ns: i64,
    pub planned_end_ns: i64,
    pub actual_start_ns: i64,
    pub actual_end_ns: i64,
    pub correlation_id: String,
    pub external_ref: String,
}

/// Result of checking whether a device is in an active change window.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveChangeContext {
    pub in_change_window: bool,
    pub change_requests: Vec<ActiveChangeRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActiveChangeRef {
    pub id: String,
    pub number: String,
    pub short_description: String,
    pub change_type: String,
    pub risk: String,
    pub planned_start_ns: i64,
    pub planned_end_ns: i64,
    pub source: String,
}

/// Stats returned by a sync cycle.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ChangeSyncStats {
    pub changes_fetched: usize,
    pub changes_upserted: usize,
    pub device_links_created: usize,
}

// ── ServiceNow polling ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SnowChangeRecord {
    sys_id: String,
    #[serde(default)]
    number: String,
    #[serde(default)]
    short_description: String,
    #[serde(default)]
    state: String,
    #[serde(default, rename = "type")]
    change_type: String,
    #[serde(default)]
    risk: String,
    #[serde(default)]
    assigned_to: String,
    #[serde(default)]
    assignment_group: String,
    #[serde(default)]
    cmdb_ci: String,
    #[serde(default)]
    start_date: String,
    #[serde(default)]
    end_date: String,
    #[serde(default)]
    work_start: String,
    #[serde(default)]
    work_end: String,
    #[serde(default)]
    correlation_id: String,
}

#[derive(Debug, Deserialize)]
struct SnowChangeList {
    result: Vec<SnowChangeRecord>,
}

pub fn maybe_start(
    config: &ServiceNowConfig,
    store: Arc<GraphStore>,
    creds: Arc<CredentialVault>,
    mut shutdown: watch::Receiver<bool>,
) {
    if !(config.enabled && config.change_management.enabled) {
        return;
    }
    let config = config.clone();
    tokio::spawn(async move {
        info!("ServiceNow change management sync task started");
        let poll_secs = config.change_management.poll_interval_secs.max(30);
        let mut interval = tokio::time::interval(Duration::from_secs(poll_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = run_change_sync(&config, &store, &creds).await {
                        warn!("ServiceNow change management sync failed: {e:#}");
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!("ServiceNow change management sync shutting down");
                        break;
                    }
                }
            }
        }
    });
}

pub async fn run_change_sync(
    config: &ServiceNowConfig,
    store: &Arc<GraphStore>,
    creds: &Arc<CredentialVault>,
) -> Result<ChangeSyncStats> {
    let start = std::time::Instant::now();
    let cred = creds
        .resolve(&config.credential_alias, ResolvePurpose::ServiceNowAdmin)
        .context("resolve ServiceNow credential for change management")?;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .context("build HTTP client for change management")?;

    let instance_url = config.instance_url.trim_end_matches('/');
    let chg_cfg = &config.change_management;

    // Fetch change_request records scheduled within the lookback window.
    let changes = fetch_change_requests(&http, instance_url, chg_cfg, &cred.username, &cred.password).await?;
    let mut stats = ChangeSyncStats {
        changes_fetched: changes.len(),
        ..Default::default()
    };

    // Load device address map for CI matching.
    let device_map = load_device_ci_map(store).await?;

    for change in &changes {
        let record = snow_to_record(change);
        upsert_change_request(store, &record).await?;
        stats.changes_upserted += 1;

        // Link affected devices.
        let affected = resolve_affected_devices(change, &device_map);
        for (device_address, role) in &affected {
            link_device_to_change(store, device_address, &record.id, role).await?;
            stats.device_links_created += 1;
        }
    }

    debug!(
        changes_fetched = stats.changes_fetched,
        changes_upserted = stats.changes_upserted,
        device_links = stats.device_links_created,
        "change management sync cycle complete"
    );

    let duration = start.elapsed().as_secs_f64();
    metrics::histogram!("bonsai_servicenow_sync_duration_seconds", "sync_type" => "change_management").record(duration);

    Ok(stats)
}

async fn fetch_change_requests(
    http: &reqwest::Client,
    instance_url: &str,
    cfg: &ServiceNowChangeManagementConfig,
    username: &str,
    password: &str,
) -> Result<Vec<SnowChangeRecord>> {
    let lookback_ns = (cfg.lookback_hours as i64) * 3_600_000_000_000;
    let cutoff_ns = now_ns() - lookback_ns;
    // ServiceNow datetime format: YYYY-MM-DD HH:MM:SS
    let cutoff_dt = format_snow_datetime(cutoff_ns);

    let url = format!("{instance_url}/api/now/table/{}", cfg.change_table);
    let query = format!(
        "start_date>={cutoff_dt}^ORwork_start>={cutoff_dt}^stateIN-5,-4,-3,-2,-1,1,2,3",
    );

    let resp: SnowChangeList = http
        .get(&url)
        .basic_auth(username, Some(password))
        .query(&[
            ("sysparm_query", query.as_str()),
            (
                "sysparm_fields",
                "sys_id,number,short_description,state,type,risk,assigned_to,assignment_group,cmdb_ci,start_date,end_date,work_start,work_end,correlation_id",
            ),
            ("sysparm_display_value", "false"),
            ("sysparm_limit", "500"),
        ])
        .send()
        .await
        .context("GET ServiceNow change_request")?
        .error_for_status()
        .context("ServiceNow change_request query returned error")?
        .json()
        .await
        .context("parse ServiceNow change_request response")?;

    Ok(resp.result)
}

/// Format nanosecond timestamp to ServiceNow datetime (UTC).
fn format_snow_datetime(ns: i64) -> String {
    let secs = ns / 1_000_000_000;
    let dt = time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        dt.year(),
        dt.month() as u8,
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
    )
}

fn snow_to_record(snow: &SnowChangeRecord) -> ChangeRequestRecord {
    let planned_start = parse_snow_datetime(&snow.start_date)
        .or_else(|| parse_snow_datetime(&snow.work_start))
        .unwrap_or(0);
    let planned_end = parse_snow_datetime(&snow.end_date)
        .or_else(|| parse_snow_datetime(&snow.work_end))
        .unwrap_or(0);
    let actual_start = parse_snow_datetime(&snow.work_start).unwrap_or(0);
    let actual_end = parse_snow_datetime(&snow.work_end).unwrap_or(0);

    ChangeRequestRecord {
        id: format!("snow:{}", snow.sys_id),
        number: snow.number.clone(),
        source: "servicenow".to_string(),
        snow_sys_id: snow.sys_id.clone(),
        short_description: snow.short_description.clone(),
        state: map_snow_change_state(&snow.state),
        change_type: map_snow_change_type(&snow.change_type),
        risk: snow.risk.clone(),
        assigned_to: snow.assigned_to.clone(),
        assignment_group: snow.assignment_group.clone(),
        affected_cis: if snow.cmdb_ci.is_empty() {
            vec![]
        } else {
            vec![snow.cmdb_ci.clone()]
        },
        planned_start_ns: planned_start,
        planned_end_ns: planned_end,
        actual_start_ns: actual_start,
        actual_end_ns: actual_end,
        correlation_id: snow.correlation_id.clone(),
        external_ref: snow.number.clone(),
    }
}

fn map_snow_change_state(state: &str) -> String {
    match state {
        "-5" => "new".to_string(),
        "-4" => "assess".to_string(),
        "-3" => "authorize".to_string(),
        "-2" => "scheduled".to_string(),
        "-1" => "implement".to_string(),
        "0" => "review".to_string(),
        "3" => "closed".to_string(),
        "4" => "cancelled".to_string(),
        other => other.to_string(),
    }
}

fn map_snow_change_type(change_type: &str) -> String {
    match change_type {
        "standard" | "Standard" => "standard".to_string(),
        "normal" | "Normal" => "normal".to_string(),
        "emergency" | "Emergency" => "emergency".to_string(),
        other => other.to_lowercase(),
    }
}

fn parse_snow_datetime(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    // ServiceNow format: "YYYY-MM-DD HH:MM:SS"
    let format = time::format_description::parse("[year]-[month]-[day] [hour]:[minute]:[second]").ok()?;
    let dt = time::PrimitiveDateTime::parse(s, &format).ok()?;
    let odt = dt.assume_utc();
    Some(odt.unix_timestamp_nanos() as i64)
}

// ── Graph operations ─────────────────────────────────────────────────────────

async fn upsert_change_request(store: &Arc<GraphStore>, record: &ChangeRequestRecord) -> Result<()> {
    let db = store.db();
    let write_lock = store.write_lock();
    let r = record.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = write_lock.lock().map_err(|_| anyhow::anyhow!("write lock poisoned"))?;
        let conn = Connection::new(&db).context("change request upsert connection")?;
        let mut stmt = conn
            .prepare(
                "MERGE (c:ChangeRequest {id: $id}) \
                 SET c.number = $number, c.source = $source, c.snow_sys_id = $sys_id, \
                     c.short_description = $desc, c.state = $state, c.change_type = $ctype, \
                     c.risk = $risk, c.assigned_to = $assigned_to, \
                     c.assignment_group = $ag, c.affected_cis_json = $cis, \
                     c.planned_start_ns = $pstart, c.planned_end_ns = $pend, \
                     c.actual_start_ns = $astart, c.actual_end_ns = $aend, \
                     c.correlation_id = $corr, c.external_ref = $ext_ref, \
                     c.updated_at = $updated_at",
            )
            .context("prepare ChangeRequest upsert")?;
        conn.execute(
            &mut stmt,
            vec![
                ("id", Value::String(r.id)),
                ("number", Value::String(r.number)),
                ("source", Value::String(r.source)),
                ("sys_id", Value::String(r.snow_sys_id)),
                ("desc", Value::String(r.short_description)),
                ("state", Value::String(r.state)),
                ("ctype", Value::String(r.change_type)),
                ("risk", Value::String(r.risk)),
                ("assigned_to", Value::String(r.assigned_to)),
                ("ag", Value::String(r.assignment_group)),
                ("cis", Value::String(serde_json::to_string(&r.affected_cis).unwrap_or_default())),
                ("pstart", Value::Int64(r.planned_start_ns)),
                ("pend", Value::Int64(r.planned_end_ns)),
                ("astart", Value::Int64(r.actual_start_ns)),
                ("aend", Value::Int64(r.actual_end_ns)),
                ("corr", Value::String(r.correlation_id)),
                ("ext_ref", Value::String(r.external_ref)),
                ("updated_at", ts(now_ns())),
            ],
        )
        .context("execute ChangeRequest upsert")?;
        Ok(())
    })
    .await
    .context("spawn_blocking panicked")?
}

async fn link_device_to_change(
    store: &Arc<GraphStore>,
    device_address: &str,
    change_id: &str,
    role: &str,
) -> Result<()> {
    let db = store.db();
    let write_lock = store.write_lock();
    let addr = device_address.to_string();
    let cid = change_id.to_string();
    let role = role.to_string();
    tokio::task::spawn_blocking(move || {
        let _guard = write_lock.lock().map_err(|_| anyhow::anyhow!("write lock poisoned"))?;
        let conn = Connection::new(&db).context("device-change link connection")?;
        let mut stmt = conn
            .prepare(
                "MATCH (d:Device {address: $addr}), (c:ChangeRequest {id: $cid}) \
                 MERGE (d)-[:AFFECTED_BY_CHANGE {role: $role, updated_at: $ts}]->(c)",
            )
            .context("prepare AFFECTED_BY_CHANGE merge")?;
        conn.execute(
            &mut stmt,
            vec![
                ("addr", Value::String(addr)),
                ("cid", Value::String(cid)),
                ("role", Value::String(role)),
                ("ts", ts(now_ns())),
            ],
        )
        .context("execute AFFECTED_BY_CHANGE merge")?;
        Ok(())
    })
    .await
    .context("spawn_blocking panicked")?
}

/// Load a map of CMDB CI sys_id → device_address for matching affected CIs.
/// Also maps hostname → address and address → address for direct matches.
async fn load_device_ci_map(store: &Arc<GraphStore>) -> Result<HashMap<String, String>> {
    let db = store.db();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).context("device CI map connection")?;
        let rows = conn
            .query(
                "MATCH (d:Device) RETURN d.address, d.hostname, d.snow_sys_id",
            )
            .context("query device CI map")?;
        let mut map = HashMap::new();
        for row in rows {
            let addr = read_str(&row[0]);
            let hostname = read_str(&row[1]);
            let sys_id = read_str(&row[2]);
            if !addr.is_empty() {
                map.insert(addr.clone(), addr.clone());
            }
            if !hostname.is_empty() {
                map.insert(hostname.to_lowercase(), addr.clone());
            }
            if !sys_id.is_empty() {
                map.insert(sys_id, addr.clone());
            }
        }
        Ok(map)
    })
    .await
    .context("spawn_blocking panicked")?
}

/// Match affected CIs from the ServiceNow change to Bonsai device addresses.
fn resolve_affected_devices(
    snow: &SnowChangeRecord,
    device_map: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut result = Vec::new();
    if !snow.cmdb_ci.is_empty() {
        if let Some(addr) = device_map.get(&snow.cmdb_ci) {
            result.push((addr.clone(), "primary_ci".to_string()));
        }
    }
    result
}

// ── Change context queries (used by detection rules + investigation agent) ───

/// Check whether a device is currently within an active change window.
/// Returns all overlapping ChangeRequests.
pub async fn active_change_context(
    store: &Arc<GraphStore>,
    device_address: &str,
) -> Result<ActiveChangeContext> {
    let db = store.db();
    let addr = device_address.to_string();
    let now = now_ns();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).context("active change context connection")?;
        let mut stmt = conn
            .prepare(
                "MATCH (d:Device {address: $addr})-[:AFFECTED_BY_CHANGE]->(c:ChangeRequest) \
                 WHERE c.planned_start_ns <= $now AND c.planned_end_ns >= $now \
                   AND c.state <> 'closed' AND c.state <> 'cancelled' \
                 RETURN c.id, c.number, c.short_description, c.change_type, c.risk, \
                        c.planned_start_ns, c.planned_end_ns, c.source",
            )
            .context("prepare active change context query")?;
        let rows = conn
            .execute(
                &mut stmt,
                vec![
                    ("addr", Value::String(addr)),
                    ("now", Value::Int64(now)),
                ],
            )
            .context("execute active change context query")?;

        let changes: Vec<ActiveChangeRef> = rows
            .map(|row| ActiveChangeRef {
                id: read_str(&row[0]),
                number: read_str(&row[1]),
                short_description: read_str(&row[2]),
                change_type: read_str(&row[3]),
                risk: read_str(&row[4]),
                planned_start_ns: match &row[5] {
                    Value::Int64(v) => *v,
                    _ => 0,
                },
                planned_end_ns: match &row[6] {
                    Value::Int64(v) => *v,
                    _ => 0,
                },
                source: read_str(&row[7]),
            })
            .collect();

        Ok(ActiveChangeContext {
            in_change_window: !changes.is_empty(),
            change_requests: changes,
        })
    })
    .await
    .context("spawn_blocking panicked")?
}

/// Link a ConfigChange or DetectionEvent to a ChangeRequest via DURING_CHANGE.
pub async fn link_event_to_change(
    store: &Arc<GraphStore>,
    event_kind: &str,
    event_id: &str,
    change_id: &str,
) -> Result<()> {
    let db = store.db();
    let write_lock = store.write_lock();
    let kind = event_kind.to_string();
    let eid = event_id.to_string();
    let cid = change_id.to_string();
    tokio::task::spawn_blocking(move || {
        let _guard = write_lock.lock().map_err(|_| anyhow::anyhow!("write lock poisoned"))?;
        let conn = Connection::new(&db).context("event-change link connection")?;
        let query = match kind.as_str() {
            "ConfigChange" => {
                "MATCH (e:ConfigChange {id: $eid}), (c:ChangeRequest {id: $cid}) \
                 MERGE (e)-[:CHANGE_CAUSED_CONFIG]->(c)"
            }
            "DetectionEvent" => {
                "MATCH (e:DetectionEvent {id: $eid}), (c:ChangeRequest {id: $cid}) \
                 MERGE (e)-[:CHANGE_CAUSED_DETECTION]->(c)"
            }
            _ => return Ok(()),
        };
        let mut stmt = conn.prepare(query).context("prepare DURING_CHANGE merge")?;
        conn.execute(
            &mut stmt,
            vec![
                ("eid", Value::String(eid)),
                ("cid", Value::String(cid)),
            ],
        )
        .context("execute DURING_CHANGE merge")?;
        Ok(())
    })
    .await
    .context("spawn_blocking panicked")?
}

/// Link an Incident to a ChangeRequest via RELATED_TO_CHANGE.
pub async fn link_incident_to_change(
    store: &Arc<GraphStore>,
    incident_id: &str,
    change_id: &str,
) -> Result<()> {
    let db = store.db();
    let write_lock = store.write_lock();
    let iid = incident_id.to_string();
    let cid = change_id.to_string();
    tokio::task::spawn_blocking(move || {
        let _guard = write_lock.lock().map_err(|_| anyhow::anyhow!("write lock poisoned"))?;
        let conn = Connection::new(&db).context("incident-change link connection")?;
        let mut stmt = conn
            .prepare(
                "MATCH (i:Incident {id: $iid}), (c:ChangeRequest {id: $cid}) \
                 MERGE (i)-[:RELATED_TO_CHANGE]->(c)",
            )
            .context("prepare RELATED_TO_CHANGE merge")?;
        conn.execute(
            &mut stmt,
            vec![
                ("iid", Value::String(iid)),
                ("cid", Value::String(cid)),
            ],
        )
        .context("execute RELATED_TO_CHANGE merge")?;
        Ok(())
    })
    .await
    .context("spawn_blocking panicked")?
}

// ── Webhook ingest ──────────────────────────────────────────────────────────

/// Payload accepted by POST /api/webhooks/change-event.
/// Works with AAP/Ansible Tower callbacks, ServiceNow business rules, or any
/// external change orchestrator.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WebhookChangeEvent {
    /// Unique change identifier from the source system.
    pub change_id: String,
    /// Human-readable change number (e.g. "CHG0012345" or AAP job ID).
    #[serde(default)]
    pub number: String,
    /// Source system identifier.
    pub source: String,
    /// Brief description of the change.
    #[serde(default)]
    pub description: String,
    /// Change state: "scheduled" | "implement" | "review" | "closed".
    #[serde(default = "default_webhook_state")]
    pub state: String,
    /// Change type: "standard" | "normal" | "emergency".
    #[serde(default = "default_webhook_type")]
    pub change_type: String,
    /// Risk level.
    #[serde(default)]
    pub risk: String,
    /// Device addresses or hostnames affected by this change.
    #[serde(default)]
    pub affected_devices: Vec<String>,
    /// Planned start time (Unix epoch seconds).
    #[serde(default)]
    pub planned_start_epoch: i64,
    /// Planned end time (Unix epoch seconds).
    #[serde(default)]
    pub planned_end_epoch: i64,
    /// Optional correlation ID for linking back to external systems.
    #[serde(default)]
    pub correlation_id: String,
    /// Optional: Ansible playbook name, AAP template name, etc.
    #[serde(default)]
    pub external_ref: String,
    /// Extra details (job variables, playbook contents, etc.)
    #[serde(default)]
    pub extra_json: String,
}

fn default_webhook_state() -> String {
    "implement".to_string()
}

fn default_webhook_type() -> String {
    "standard".to_string()
}

/// Process an incoming webhook change event: write to graph and link devices.
pub async fn ingest_webhook_change(
    store: &Arc<GraphStore>,
    event: WebhookChangeEvent,
) -> Result<ChangeRequestRecord> {
    let record = ChangeRequestRecord {
        id: format!("{}:{}", event.source, event.change_id),
        number: if event.number.is_empty() {
            event.change_id.clone()
        } else {
            event.number
        },
        source: event.source,
        snow_sys_id: String::new(),
        short_description: event.description,
        state: event.state,
        change_type: event.change_type,
        risk: event.risk,
        assigned_to: String::new(),
        assignment_group: String::new(),
        affected_cis: event.affected_devices.clone(),
        planned_start_ns: event.planned_start_epoch.saturating_mul(1_000_000_000),
        planned_end_ns: event.planned_end_epoch.saturating_mul(1_000_000_000),
        actual_start_ns: 0,
        actual_end_ns: 0,
        correlation_id: event.correlation_id,
        external_ref: event.external_ref,
    };

    upsert_change_request(store, &record).await?;

    // Link affected devices.
    let device_map = load_device_ci_map(store).await?;
    for device_ref in &event.affected_devices {
        let addr = device_map
            .get(device_ref)
            .or_else(|| device_map.get(&device_ref.to_lowercase()))
            .cloned()
            .unwrap_or_else(|| device_ref.clone());
        link_device_to_change(store, &addr, &record.id, "webhook_target").await?;
    }

    info!(
        change_id = %record.id,
        source = %record.source,
        affected = event.affected_devices.len(),
        "webhook change event ingested"
    );

    Ok(record)
}
