use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use lbug::{Connection, Value};
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::mpsc;
use tracing::warn;
use uuid::Uuid;

use crate::config::{LayeredIngestionConfig, TargetConfig};
use crate::config_store::{ConfigStore, summarize_diff};
use crate::credentials::{CredentialVault, ResolvePurpose, ResolvedCredential};
use crate::enrichment::registry::MultiSourceEnricherRegistry;
use crate::event_bus::{BroadcastSubscriber, BusSubscriber, InProcessBus};
use crate::graph::GraphStore;
use crate::graph::common::{read_str, read_ts_ns, ts};
use crate::registry::{ApiRegistry, DeviceRegistry};
use crate::telemetry::TelemetryUpdate;

#[derive(Clone, Debug, Serialize)]
pub struct ConfigSnapshotSummary {
    pub id: String,
    pub source: String,
    pub trigger: String,
    pub snapshot_hash: String,
    pub summary: String,
    pub stored_path: String,
    pub bytes_len: i64,
    pub captured_at_ns: i64,
    pub confidence: String,
    pub parser: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigChangeSummary {
    pub id: String,
    pub source: String,
    pub trigger: String,
    pub previous_hash: String,
    pub current_hash: String,
    pub summary: String,
    pub added_lines: i64,
    pub removed_lines: i64,
    pub changed_at_ns: i64,
    pub confidence: String,
    pub parser: String,
}

#[derive(Clone, Debug)]
pub struct ChangeDetectionRequest {
    pub device_address: String,
    pub trigger: String,
    pub reason: String,
    pub requested_at_ns: i64,
}

#[derive(Clone)]
pub struct ChangeDetectionRuntime {
    tx: mpsc::Sender<ChangeDetectionRequest>,
    pub history_limit: usize,
}

impl ChangeDetectionRuntime {
    pub fn start(
        store: Arc<GraphStore>,
        registry: Arc<ApiRegistry>,
        credentials: Arc<CredentialVault>,
        bus: Arc<InProcessBus>,
        layered: LayeredIngestionConfig,
    ) -> Result<Arc<Self>> {
        let syslog_patterns = Arc::new(load_syslog_patterns(&layered.syslog_patterns_path));
        let config_store = Arc::new(ConfigStore::open(
            &layered.config_store_path,
            &layered.config_store_passphrase_env,
        )?);
        let (tx, rx) = mpsc::channel(128);
        let runtime = Arc::new(Self {
            tx,
            history_limit: layered.history_limit,
        });
        let capture_registry = Arc::new(MultiSourceEnricherRegistry::from_layered_config(&layered));

        tokio::spawn(run_worker(
            rx,
            Arc::clone(&store),
            Arc::clone(&registry),
            Arc::clone(&credentials),
            Arc::clone(&config_store),
            capture_registry,
        ));

        let (subscriber, mut subscriber_rx) =
            BroadcastSubscriber::new("change-detection", 2048);
        let trigger_runtime = Arc::clone(&runtime);
        let syslog_patterns_for_bus = Arc::clone(&syslog_patterns);
        tokio::spawn(async move {
            while let Ok(update) = subscriber_rx.recv().await {
                if let Some(trigger) = signal_trigger_for_update(&update, &syslog_patterns_for_bus)
                    && let Err(error) = trigger_runtime
                        .enqueue(ChangeDetectionRequest {
                            device_address: update.target.clone(),
                            trigger: trigger.trigger,
                            reason: trigger.reason,
                            requested_at_ns: now_ns(),
                        })
                        .await
                {
                    warn!(address = %update.target, %error, "failed to enqueue signal-driven re-parse");
                }
            }
        });
        tokio::spawn({
            let bus = Arc::clone(&bus);
            async move {
                bus.add_subscriber(subscriber as Arc<dyn BusSubscriber>)
                    .await;
            }
        });

        if layered.enabled {
            tokio::spawn(run_scheduler(
                Arc::clone(&runtime),
                Arc::clone(&registry),
                Arc::clone(&store),
                layered.change_detection_schedule_interval_secs,
                layered.change_detection_reparse_interval_secs,
            ));
        }

        Ok(runtime)
    }

    pub async fn enqueue_manual(&self, device_address: &str, reason: &str) -> Result<()> {
        self.enqueue(ChangeDetectionRequest {
            device_address: device_address.to_string(),
            trigger: "manual".to_string(),
            reason: reason.to_string(),
            requested_at_ns: now_ns(),
        })
        .await
    }

    async fn enqueue(&self, request: ChangeDetectionRequest) -> Result<()> {
        self.tx
            .send(request)
            .await
            .map_err(|_| anyhow!("change detection queue is unavailable"))
    }
}

#[derive(Clone)]
struct SignalTrigger {
    trigger: String,
    reason: String,
}

#[derive(Clone, Debug, Deserialize)]
struct SyslogPatternFile {
    #[serde(default)]
    vendor: String,
    #[serde(default)]
    patterns: Vec<String>,
}

fn load_syslog_patterns(dir: &str) -> Vec<SyslogPatternFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut loaded = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(pattern_file) = serde_yaml::from_str::<SyslogPatternFile>(&raw) {
            loaded.push(pattern_file);
        }
    }
    loaded
}

fn signal_trigger_for_update(
    update: &TelemetryUpdate,
    syslog_patterns: &[SyslogPatternFile],
) -> Option<SignalTrigger> {
    let path = update.path.to_ascii_lowercase();
    if path.contains("signals/syslog/") {
        let message = update
            .value
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let vendor = update.vendor.to_ascii_lowercase();
        for pattern_file in syslog_patterns {
            let vendor_match = pattern_file.vendor.is_empty()
                || vendor.contains(&pattern_file.vendor.to_ascii_lowercase());
            if vendor_match
                && pattern_file
                    .patterns
                    .iter()
                    .any(|pattern| message.contains(&pattern.to_ascii_lowercase()))
            {
                return Some(SignalTrigger {
                    trigger: "signal_syslog".to_string(),
                    reason: "matched syslog config-change pattern".to_string(),
                });
            }
        }
    }

    if path.contains("signals/snmp/") {
        let event_type = update
            .value
            .get("event_type")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let message = update
            .value
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if event_type.contains("config") || message.contains("config") {
            return Some(SignalTrigger {
                trigger: "signal_snmp".to_string(),
                reason: "matched SNMP config-change signal".to_string(),
            });
        }
    }

    if path.contains("last-changed") || path.contains("config-version") {
        return Some(SignalTrigger {
            trigger: "signal_gnmi".to_string(),
            reason: "matched gNMI config version change".to_string(),
        });
    }
    None
}

async fn run_worker(
    mut rx: mpsc::Receiver<ChangeDetectionRequest>,
    store: Arc<GraphStore>,
    registry: Arc<ApiRegistry>,
    credentials: Arc<CredentialVault>,
    config_store: Arc<ConfigStore>,
    capture_registry: Arc<MultiSourceEnricherRegistry>,
) {
    while let Some(request) = rx.recv().await {
        if let Err(error) = process_request(
            request,
            Arc::clone(&store),
            Arc::clone(&registry),
            Arc::clone(&credentials),
            Arc::clone(&config_store),
            Arc::clone(&capture_registry),
        )
        .await
        {
            warn!(%error, "change detection request failed");
        }
    }
}

async fn process_request(
    request: ChangeDetectionRequest,
    store: Arc<GraphStore>,
    registry: Arc<ApiRegistry>,
    credentials: Arc<CredentialVault>,
    config_store: Arc<ConfigStore>,
    capture_registry: Arc<MultiSourceEnricherRegistry>,
) -> Result<()> {
    let target = registry
        .get_device(&request.device_address)?
        .ok_or_else(|| anyhow!("managed device '{}' not found", request.device_address))?;

    let resolved_credentials = resolve_target_credentials(&target, &credentials)?;
    let capture = capture_registry
        .capture(&target, resolved_credentials.as_ref(), None)
        .await?;
    let snapshot_id = Uuid::new_v4().to_string();
    let captured_at_ns = now_ns();
    let stored = config_store.store_snapshot(&target.address, &snapshot_id, &capture.payload)?;
    let previous = latest_snapshot_meta(Arc::clone(&store), &target.address).await?;

    let (changed, added_lines, removed_lines, summary, previous_snapshot_id, previous_hash) =
        if let Some(previous) = previous {
            if previous.snapshot_hash == stored.sha256 {
                (
                    false,
                    0,
                    0,
                    "unchanged".to_string(),
                    Some(previous.id),
                    Some(previous.snapshot_hash),
                )
            } else {
                let previous_payload = config_store
                    .read_snapshot(&previous.stored_path)
                    .with_context(|| {
                        format!("failed to read previous snapshot for {}", target.address)
                    })?;
                let (added, removed, summary) = summarize_diff(&previous_payload, &capture.payload);
                (
                    true,
                    added,
                    removed,
                    summary,
                    Some(previous.id),
                    Some(previous.snapshot_hash),
                )
            }
        } else {
            (
                true,
                0,
                0,
                "baseline snapshot captured".to_string(),
                None,
                None,
            )
        };

    let details_json = serde_json::json!({
        "trigger": request.trigger.clone(),
        "reason": request.reason.clone(),
        "capture": capture.details.clone(),
        "summary": summary.clone(),
        "added_lines": added_lines,
        "removed_lines": removed_lines,
    })
    .to_string();

    write_snapshot_and_change(
        store,
        SnapshotWrite {
            snapshot_id,
            device_address: target.address,
            source: capture.source,
            trigger: request.trigger,
            reason: request.reason,
            requested_at_ns: request.requested_at_ns,
            snapshot_hash: stored.sha256,
            stored_path: stored.relative_path,
            bytes_len: stored.bytes_len as i64,
            captured_at_ns,
            summary,
            changed,
            added_lines,
            removed_lines,
            parser: capture.parser,
            confidence: capture.confidence,
            details_json,
            previous_snapshot_id,
            previous_hash,
        },
    )
    .await
}

async fn run_scheduler(
    runtime: Arc<ChangeDetectionRuntime>,
    registry: Arc<ApiRegistry>,
    store: Arc<GraphStore>,
    schedule_interval_secs: u64,
    reparse_interval_secs: u64,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(schedule_interval_secs.max(60)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;
        let now = now_ns();
        let targets = match registry.list_active() {
            Ok(targets) => targets,
            Err(error) => {
                warn!(%error, "scheduled change detection could not list devices");
                continue;
            }
        };

        for target in targets {
            match latest_snapshot_meta(Arc::clone(&store), &target.address).await {
                Ok(Some(snapshot))
                    if now - snapshot.captured_at_ns
                        < (reparse_interval_secs as i64 * 1_000_000_000) => {}
                Ok(_) => {
                    if let Err(error) = runtime
                        .enqueue(ChangeDetectionRequest {
                            device_address: target.address.clone(),
                            trigger: "scheduled".to_string(),
                            reason: "periodic config verification".to_string(),
                            requested_at_ns: now,
                        })
                        .await
                    {
                        warn!(address = %target.address, %error, "failed to enqueue scheduled config verification");
                    }
                }
                Err(error) => {
                    warn!(address = %target.address, %error, "failed to inspect latest snapshot timestamp")
                }
            }
        }
    }
}

#[derive(Clone)]
struct LatestSnapshotMeta {
    id: String,
    snapshot_hash: String,
    stored_path: String,
    captured_at_ns: i64,
}

#[derive(Clone)]
struct SnapshotWrite {
    snapshot_id: String,
    device_address: String,
    source: String,
    trigger: String,
    reason: String,
    requested_at_ns: i64,
    snapshot_hash: String,
    stored_path: String,
    bytes_len: i64,
    captured_at_ns: i64,
    summary: String,
    changed: bool,
    added_lines: i64,
    removed_lines: i64,
    parser: String,
    confidence: String,
    details_json: String,
    previous_snapshot_id: Option<String>,
    previous_hash: Option<String>,
}

async fn latest_snapshot_meta(
    store: Arc<GraphStore>,
    address: &str,
) -> Result<Option<LatestSnapshotMeta>> {
    let db = store.db();
    let address = address.to_string();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).context("latest config snapshot connection")?;
        let mut stmt = conn
            .prepare(
                "MATCH (s:ConfigSnapshot {device_address: $addr}) \
                 RETURN s.id, s.snapshot_hash, s.stored_path, s.captured_at \
                 ORDER BY s.captured_at DESC LIMIT 1",
            )
            .context("prepare latest config snapshot query")?;
        let mut rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(address))])
            .context("execute latest config snapshot query")?;
        if let Some(row) = rows.next() {
            Ok(Some(LatestSnapshotMeta {
                id: read_str(&row[0]),
                snapshot_hash: read_str(&row[1]),
                stored_path: read_str(&row[2]),
                captured_at_ns: read_ts_ns(&row[3]),
            }))
        } else {
            Ok(None)
        }
    })
    .await
    .context("latest config snapshot task panicked")?
}

pub async fn config_history(
    store: Arc<GraphStore>,
    address: String,
    limit: usize,
) -> Result<(Vec<ConfigSnapshotSummary>, Vec<ConfigChangeSummary>)> {
    let db = store.db();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).context("config history connection")?;

        let mut snapshot_stmt = conn
            .prepare(
                "MATCH (s:ConfigSnapshot {device_address: $addr}) \
                 OPTIONAL MATCH (s)-[:CONFIG_SNAPSHOT_PROVENANCE]->(p:PropertyProvenance) \
                 RETURN s.id, s.source, s.trigger, s.snapshot_hash, s.summary, s.stored_path, \
                        s.bytes_len, s.captured_at, p.confidence, p.parser \
                 ORDER BY s.captured_at DESC LIMIT $limit",
            )
            .context("prepare config snapshot history query")?;
        let snapshots = conn
            .execute(
                &mut snapshot_stmt,
                vec![
                    ("addr", Value::String(address.clone())),
                    ("limit", Value::Int64(limit as i64)),
                ],
            )
            .context("execute config snapshot history query")?
            .map(|row| ConfigSnapshotSummary {
                id: read_str(&row[0]),
                source: read_str(&row[1]),
                trigger: read_str(&row[2]),
                snapshot_hash: read_str(&row[3]),
                summary: read_str(&row[4]),
                stored_path: read_str(&row[5]),
                bytes_len: read_i64(&row[6]),
                captured_at_ns: read_ts_ns(&row[7]),
                confidence: read_str(&row[8]),
                parser: read_str(&row[9]),
            })
            .collect::<Vec<_>>();

        let mut change_stmt = conn
            .prepare(
                "MATCH (c:ConfigChange {device_address: $addr}) \
                 OPTIONAL MATCH (c)-[:CONFIG_CHANGE_PROVENANCE]->(p:PropertyProvenance) \
                 RETURN c.id, c.source, c.trigger, c.previous_hash, c.current_hash, c.summary, \
                        c.added_lines, c.removed_lines, c.changed_at, p.confidence, p.parser \
                 ORDER BY c.changed_at DESC LIMIT $limit",
            )
            .context("prepare config change history query")?;
        let changes = conn
            .execute(
                &mut change_stmt,
                vec![
                    ("addr", Value::String(address)),
                    ("limit", Value::Int64(limit as i64)),
                ],
            )
            .context("execute config change history query")?
            .map(|row| ConfigChangeSummary {
                id: read_str(&row[0]),
                source: read_str(&row[1]),
                trigger: read_str(&row[2]),
                previous_hash: read_str(&row[3]),
                current_hash: read_str(&row[4]),
                summary: read_str(&row[5]),
                added_lines: read_i64(&row[6]),
                removed_lines: read_i64(&row[7]),
                changed_at_ns: read_ts_ns(&row[8]),
                confidence: read_str(&row[9]),
                parser: read_str(&row[10]),
            })
            .collect::<Vec<_>>();

        Ok((snapshots, changes))
    })
    .await
    .context("config history task panicked")?
}

async fn write_snapshot_and_change(store: Arc<GraphStore>, write: SnapshotWrite) -> Result<()> {
    let db = store.db();
    let write_lock = store.write_lock();
    tokio::task::spawn_blocking(move || {
        let _guard = write_lock
            .lock()
            .map_err(|_| anyhow!("graph write lock poisoned"))?;
        let conn = Connection::new(&db).context("config snapshot write connection")?;

        let mut snapshot_stmt = conn
            .prepare(
                "MERGE (s:ConfigSnapshot {id: $id}) \
                 SET s.device_address = $addr, s.source = $source, s.trigger = $trigger, \
                     s.reason = $reason, s.requested_at = $requested_at, \
                     s.snapshot_hash = $snapshot_hash, s.stored_path = $stored_path, \
                     s.bytes_len = $bytes_len, s.captured_at = $captured_at, s.summary = $summary, \
                     s.changed = $changed \
                 RETURN s.id",
            )
            .context("prepare config snapshot write")?;
        conn.execute(
            &mut snapshot_stmt,
            vec![
                ("id", Value::String(write.snapshot_id.clone())),
                ("addr", Value::String(write.device_address.clone())),
                ("source", Value::String(write.source.clone())),
                ("trigger", Value::String(write.trigger.clone())),
                ("reason", Value::String(write.reason.clone())),
                ("requested_at", ts(write.requested_at_ns)),
                ("snapshot_hash", Value::String(write.snapshot_hash.clone())),
                ("stored_path", Value::String(write.stored_path.clone())),
                ("bytes_len", Value::Int64(write.bytes_len)),
                ("captured_at", ts(write.captured_at_ns)),
                ("summary", Value::String(write.summary.clone())),
                ("changed", Value::Bool(write.changed)),
            ],
        )
        .context("execute config snapshot write")?;

        let mut link_stmt = conn
            .prepare(
                "MATCH (d:Device {address: $addr}), (s:ConfigSnapshot {id: $id}) \
                 MERGE (d)-[:HAS_CONFIG_SNAPSHOT]->(s)",
            )
            .context("prepare config snapshot link")?;
        conn.execute(
            &mut link_stmt,
            vec![
                ("addr", Value::String(write.device_address.clone())),
                ("id", Value::String(write.snapshot_id.clone())),
            ],
        )
        .context("execute config snapshot link")?;
        write_provenance(
            &conn,
            ProvenanceWrite {
                owner_kind: "ConfigSnapshot",
                owner_id: &write.snapshot_id,
                source: &write.source,
                parser: &write.parser,
                confidence: &write.confidence,
                captured_at_ns: write.captured_at_ns,
                details_json: &write.details_json,
            },
        )?;

        if let (Some(previous_snapshot_id), Some(previous_hash)) = (
            write.previous_snapshot_id.clone(),
            write.previous_hash.clone(),
        ) && write.changed
        {
            let change_id = Uuid::new_v4().to_string();
            let mut change_stmt = conn
                .prepare(
                    "MERGE (c:ConfigChange {id: $id}) \
                     SET c.device_address = $addr, c.source = $source, c.trigger = $trigger, \
                         c.previous_snapshot_id = $previous_snapshot_id, \
                         c.current_snapshot_id = $current_snapshot_id, \
                         c.previous_hash = $previous_hash, c.current_hash = $current_hash, \
                         c.summary = $summary, c.added_lines = $added_lines, \
                         c.removed_lines = $removed_lines, c.changed_at = $changed_at \
                     RETURN c.id",
                )
                .context("prepare config change write")?;
            conn.execute(
                &mut change_stmt,
                vec![
                    ("id", Value::String(change_id.clone())),
                    ("addr", Value::String(write.device_address.clone())),
                    ("source", Value::String(write.source.clone())),
                    ("trigger", Value::String(write.trigger.clone())),
                    (
                        "previous_snapshot_id",
                        Value::String(previous_snapshot_id.clone()),
                    ),
                    (
                        "current_snapshot_id",
                        Value::String(write.snapshot_id.clone()),
                    ),
                    ("previous_hash", Value::String(previous_hash)),
                    ("current_hash", Value::String(write.snapshot_hash.clone())),
                    ("summary", Value::String(write.summary.clone())),
                    ("added_lines", Value::Int64(write.added_lines)),
                    ("removed_lines", Value::Int64(write.removed_lines)),
                    ("changed_at", ts(write.captured_at_ns)),
                ],
            )
            .context("execute config change write")?;

            let mut change_link_stmt = conn
                .prepare(
                    "MATCH (d:Device {address: $addr}), (c:ConfigChange {id: $id}) \
                     MERGE (d)-[:HAS_CONFIG_CHANGE]->(c)",
                )
                .context("prepare config change link")?;
            conn.execute(
                &mut change_link_stmt,
                vec![
                    ("addr", Value::String(write.device_address.clone())),
                    ("id", Value::String(change_id.clone())),
                ],
            )
            .context("execute config change link")?;
            write_provenance(
                &conn,
                ProvenanceWrite {
                    owner_kind: "ConfigChange",
                    owner_id: &change_id,
                    source: &write.source,
                    parser: &write.parser,
                    confidence: &write.confidence,
                    captured_at_ns: write.captured_at_ns,
                    details_json: &write.details_json,
                },
            )?;
        }

        let mut enrichment_stmt = conn
            .prepare(
                "MERGE (p:EnrichmentProperty {id: $id}) \
                 SET p.device_address = $addr, p.key = $key, p.value = $value, \
                     p.source_name = $source_name, p.updated_at = $updated_at \
                 RETURN p.id",
            )
            .context("prepare config enrichment property write")?;
        let enrichment_values = [
            ("last_capture_hash", write.snapshot_hash.clone()),
            ("last_capture_summary", write.summary.clone()),
            ("last_capture_trigger", write.trigger.clone()),
            ("last_capture_source", write.source.clone()),
        ];
        for (key, value) in enrichment_values {
            let property_id = format!("{}:multisource:{key}", write.device_address);
            conn.execute(
                &mut enrichment_stmt,
                vec![
                    ("id", Value::String(property_id.clone())),
                    ("addr", Value::String(write.device_address.clone())),
                    ("key", Value::String(format!("multisource_{key}"))),
                    ("value", Value::String(value)),
                    ("source_name", Value::String("multi_source".to_string())),
                    ("updated_at", ts(write.captured_at_ns)),
                ],
            )
            .context("execute config enrichment property write")?;

            let mut enrichment_link_stmt = conn
                .prepare(
                    "MATCH (d:Device {address: $addr}), (p:EnrichmentProperty {id: $id}) \
                     MERGE (d)-[:HAS_ENRICHMENT_PROPERTY]->(p)",
                )
                .context("prepare config enrichment property link")?;
            conn.execute(
                &mut enrichment_link_stmt,
                vec![
                    ("addr", Value::String(write.device_address.clone())),
                    ("id", Value::String(property_id)),
                ],
            )
            .context("execute config enrichment property link")?;
            write_provenance(
                &conn,
                ProvenanceWrite {
                    owner_kind: "EnrichmentProperty",
                    owner_id: &format!("{}:multisource:{key}", write.device_address),
                    source: &write.source,
                    parser: "gnmi_json",
                    confidence: "medium",
                    captured_at_ns: write.captured_at_ns,
                    details_json: &serde_json::json!({
                        "key": key,
                    })
                    .to_string(),
                },
            )?;
        }

        Ok(())
    })
    .await
    .context("config snapshot write task panicked")?
}

struct ProvenanceWrite<'a> {
    owner_kind: &'a str,
    owner_id: &'a str,
    source: &'a str,
    parser: &'a str,
    confidence: &'a str,
    captured_at_ns: i64,
    details_json: &'a str,
}

fn write_provenance(conn: &Connection<'_>, write: ProvenanceWrite<'_>) -> Result<()> {
    let provenance_id = format!("{}:{}:provenance", write.owner_kind, write.owner_id);
    let mut prov_stmt = conn
        .prepare(
            "MERGE (p:PropertyProvenance {id: $id}) \
             SET p.owner_kind = $owner_kind, p.owner_id = $owner_id, p.source = $source, \
                 p.parser = $parser, p.confidence = $confidence, p.captured_at = $captured_at, \
                 p.details_json = $details_json",
        )
        .context("prepare provenance write")?;
    conn.execute(
        &mut prov_stmt,
        vec![
            ("id", Value::String(provenance_id.clone())),
            ("owner_kind", Value::String(write.owner_kind.to_string())),
            ("owner_id", Value::String(write.owner_id.to_string())),
            ("source", Value::String(write.source.to_string())),
            ("parser", Value::String(write.parser.to_string())),
            ("confidence", Value::String(write.confidence.to_string())),
            ("captured_at", ts(write.captured_at_ns)),
            (
                "details_json",
                Value::String(write.details_json.to_string()),
            ),
        ],
    )
    .context("execute provenance write")?;

    let rel_query = match write.owner_kind {
        "ConfigSnapshot" => {
            "MATCH (o:ConfigSnapshot {id: $owner_id}), (p:PropertyProvenance {id: $prov_id}) \
             MERGE (o)-[:CONFIG_SNAPSHOT_PROVENANCE]->(p)"
        }
        "ConfigChange" => {
            "MATCH (o:ConfigChange {id: $owner_id}), (p:PropertyProvenance {id: $prov_id}) \
             MERGE (o)-[:CONFIG_CHANGE_PROVENANCE]->(p)"
        }
        "EnrichmentProperty" => {
            "MATCH (o:EnrichmentProperty {id: $owner_id}), (p:PropertyProvenance {id: $prov_id}) \
             MERGE (o)-[:ENRICHMENT_PROPERTY_PROVENANCE]->(p)"
        }
        _ => bail!("unsupported provenance owner kind '{}'", write.owner_kind),
    };
    let mut rel_stmt = conn.prepare(rel_query).context("prepare provenance link")?;
    conn.execute(
        &mut rel_stmt,
        vec![
            ("owner_id", Value::String(write.owner_id.to_string())),
            ("prov_id", Value::String(provenance_id)),
        ],
    )
    .context("execute provenance link")?;
    Ok(())
}

fn resolve_target_credentials(
    target: &TargetConfig,
    credentials: &CredentialVault,
) -> Result<Option<ResolvedCredential>> {
    if let Some(alias) = target.credential_alias.as_deref() {
        return credentials.resolve(alias, ResolvePurpose::Enrich).map(Some);
    }
    Ok(
        match (target.resolved_username(), target.resolved_password()) {
            (Some(username), Some(password)) => Some(ResolvedCredential { username, password: zeroize::Zeroizing::new(password) }),
            _ => None,
        },
    )
}

fn read_i64(value: &Value) -> i64 {
    match value {
        Value::Int64(inner) => *inner,
        Value::Int32(inner) => i64::from(*inner),
        Value::Int16(inner) => i64::from(*inner),
        Value::Int8(inner) => i64::from(*inner),
        _ => 0,
    }
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as i64)
        .unwrap_or(0)
}
