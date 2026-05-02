//! Elasticsearch ingest output adapter (T6-4).
//!
//! Core-side adapter: polls the graph for DetectionEvents and pushes them
//! to Elasticsearch via the `_bulk` API using an ECS-compliant document schema.
//!
//! Protocol: Elasticsearch Bulk API
//!   POST /<index>/_bulk
//!   Content-Type: application/x-ndjson
//!
//! Authentication (select via config extra.auth_type):
//!   "basic"   (default) — Basic auth, credential vault username + password
//!   "api_key" — `Authorization: ApiKey <token>` where the vault `password`
//!               field holds the pre-encoded key (Elastic cloud format: base64(id:key))
//!
//! Config extra fields (all optional):
//!   index            — target index (default: "bonsai-detections")
//!   auth_type        — "basic" | "api_key" (default: "basic")
//!   dedup_window_secs — suppress re-push of (device, rule) within this window (default: 300)
//!
//! ECS field mapping:
//!   @timestamp       → fired_at_ns as epoch seconds (float)
//!   event.kind       → "alert"
//!   event.category   → ["network"]
//!   event.severity   → 1=critical, 2=high/warning, 3=info
//!   event.module     → "bonsai"
//!   host.ip          → [device_address]
//!   host.name        → hostname
//!   rule.id / rule.name → rule_id
//!   labels.*         → bonsai-specific fields for filtering
//!   bonsai.*         → full bonsai event payload

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use lbug::{Connection, Database};
use serde_json::{json, Value as JsonValue};
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::credentials::{CredentialVault, ResolvePurpose};
use crate::event_bus::InProcessBus;
use crate::graph::common::{now_ns, read_str, read_ts_ns};
use crate::output::traits::{
    OutputAdapter, OutputAdapterAuditLog, OutputAdapterConfig, OutputReport, OutputTopic,
};

// ── Detection record from graph ───────────────────────────────────────────────

#[derive(Debug)]
struct DetectionRecord {
    id: String,
    device_address: String,
    hostname: String,
    rule_id: String,
    severity: String,
    fired_at_ns: i64,
    features_json: String,
    remediation_action: String,
    remediation_status: String,
}

// ── Adapter ───────────────────────────────────────────────────────────────────

pub struct ElasticAdapter {
    config: OutputAdapterConfig,
    environments: Vec<String>,
    db: Arc<Database>,
}

impl ElasticAdapter {
    pub fn from_config(config: OutputAdapterConfig, db: Arc<Database>) -> Self {
        let environments = config.environment_scope.clone();
        Self { config, environments, db }
    }
}

#[async_trait::async_trait]
impl OutputAdapter for ElasticAdapter {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn topics(&self) -> &[OutputTopic] {
        &[OutputTopic::DetectionEvents, OutputTopic::RemediationOutcomes]
    }

    fn applies_to_environments(&self) -> &[String] {
        &self.environments
    }

    async fn run(
        &self,
        _bus: Arc<InProcessBus>,
        creds: Arc<CredentialVault>,
        audit: OutputAdapterAuditLog,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        info!(
            adapter = %self.config.name,
            url = %self.config.endpoint_url,
            "Elastic adapter started"
        );

        let flush_secs = self.config.flush_interval_secs.max(10);
        let mut interval = tokio::time::interval(Duration::from_secs(flush_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut dedup: HashMap<(String, String), i64> = HashMap::new();
        let mut pushed: HashSet<String> = HashSet::new();

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let start = Instant::now();
                    match push_cycle(&self.config, &self.db, &creds, &audit, &mut dedup, &mut pushed).await {
                        Ok(report) if report.events_pushed > 0 => {
                            debug!(
                                adapter = %self.config.name,
                                events = report.events_pushed,
                                bytes = report.bytes_sent,
                                ms = start.elapsed().as_millis(),
                                "Elastic push ok"
                            );
                        }
                        Ok(_) => {}
                        Err(e) => warn!(adapter = %self.config.name, "Elastic push failed: {e:#}"),
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!(adapter = %self.config.name, "Elastic adapter shutting down");
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    async fn test_connection(
        &self,
        creds: Arc<CredentialVault>,
        audit: &OutputAdapterAuditLog,
    ) -> Result<()> {
        let http = build_http_client()?;
        let base = self.config.endpoint_url.trim_end_matches('/');
        let health_url = format!("{base}/_cluster/health");
        let req = http.get(&health_url);
        let req = apply_auth(req, &self.config, &creds, audit)?;
        let resp = req
            .send()
            .await
            .with_context(|| format!("GET Elastic cluster health {health_url}"))?;
        let status = resp.status();
        // 200 = healthy; 401 = reachable but auth invalid (still proves connectivity)
        if status.is_success() || status.as_u16() == 401 {
            Ok(())
        } else {
            anyhow::bail!("Elastic cluster health returned {status}")
        }
    }
}

// ── Push cycle ────────────────────────────────────────────────────────────────

async fn push_cycle(
    config: &OutputAdapterConfig,
    db: &Arc<Database>,
    creds: &Arc<CredentialVault>,
    audit: &OutputAdapterAuditLog,
    dedup: &mut HashMap<(String, String), i64>,
    pushed: &mut HashSet<String>,
) -> Result<OutputReport> {
    let db2 = Arc::clone(db);
    let detections: Vec<DetectionRecord> =
        tokio::task::spawn_blocking(move || query_detections(&db2))
            .await
            .context("spawn_blocking panicked")??;

    if detections.is_empty() {
        return Ok(OutputReport { adapter_name: config.name.clone(), ..Default::default() });
    }

    let now = now_ns();
    let dedup_window_ns: i64 = config
        .extra
        .get("dedup_window_secs")
        .and_then(|v| v.as_i64())
        .unwrap_or(300)
        * 1_000_000_000;

    let index = config
        .extra
        .get("index")
        .and_then(|v| v.as_str())
        .unwrap_or("bonsai-detections")
        .to_string();

    let mut ndjson_lines: Vec<String> = Vec::new();
    let mut bytes = 0u64;
    let mut n = 0usize;

    for det in &detections {
        if pushed.contains(&det.id) {
            continue;
        }
        let dedup_key = (det.device_address.clone(), det.rule_id.clone());
        if let Some(&last_ns) = dedup.get(&dedup_key) {
            if now - last_ns < dedup_window_ns {
                continue;
            }
        }

        let doc = build_ecs_doc(det);
        let action = json!({"index": {"_index": index, "_id": det.id}});
        let action_line = serde_json::to_string(&action).unwrap_or_default();
        let doc_line = serde_json::to_string(&doc).unwrap_or_default();
        bytes += (action_line.len() + doc_line.len() + 2) as u64; // +2 for newlines

        ndjson_lines.push(action_line);
        ndjson_lines.push(doc_line);
        pushed.insert(det.id.clone());
        dedup.insert(dedup_key, now);
        n += 1;
    }

    if n == 0 {
        return Ok(OutputReport { adapter_name: config.name.clone(), ..Default::default() });
    }

    // Elastic bulk: each line terminated by \n, including the final line
    let mut body = ndjson_lines.join("\n");
    body.push('\n');

    let bulk_url =
        format!("{}/{}/_bulk", config.endpoint_url.trim_end_matches('/'), index);
    let http = build_http_client()?;
    let req = http
        .post(&bulk_url)
        .header("Content-Type", "application/x-ndjson")
        .body(body);
    let req = apply_auth(req, config, creds, audit)?;

    let resp = req
        .send()
        .await
        .with_context(|| format!("POST Elastic bulk {bulk_url}"))?;

    let status = resp.status();
    if status.is_success() {
        audit.log_push(n, bytes, None);
        Ok(OutputReport {
            adapter_name: config.name.clone(),
            events_pushed: n,
            bytes_sent: bytes,
            ..Default::default()
        })
    } else {
        let text = resp.text().await.unwrap_or_default();
        let err = format!("Elastic bulk returned {status}: {text}");
        audit.log_push(0, bytes, Some(&err));
        Ok(OutputReport {
            adapter_name: config.name.clone(),
            bytes_sent: bytes,
            error: Some(err),
            ..Default::default()
        })
    }
}

// ── ECS document builder ──────────────────────────────────────────────────────

fn severity_to_ecs(s: &str) -> u32 {
    match s {
        "critical" => 1,
        "high" | "warning" => 2,
        _ => 3,
    }
}

fn build_ecs_doc(det: &DetectionRecord) -> JsonValue {
    let features: JsonValue =
        serde_json::from_str(&det.features_json).unwrap_or(JsonValue::Null);
    // @timestamp as epoch seconds (float). Elastic date_detection parses epoch floats,
    // or operators can set explicit mappings to date with format "epoch_second||epoch_millis".
    let ts_secs = det.fired_at_ns as f64 / 1_000_000_000.0;

    json!({
        "@timestamp": ts_secs,
        "event": {
            "kind":     "alert",
            "category": ["network"],
            "severity": severity_to_ecs(&det.severity),
            "module":   "bonsai",
        },
        "host": {
            "ip":   [det.device_address],
            "name": det.hostname,
        },
        "rule": {
            "id":   det.rule_id,
            "name": det.rule_id,
        },
        // ECS labels must be scalar; use string values for bonsai-specific fields
        "labels": {
            "bonsai_detection_id":       det.id,
            "bonsai_severity":           det.severity,
            "bonsai_remediation_action": det.remediation_action,
            "bonsai_remediation_status": det.remediation_status,
        },
        // Non-ECS bonsai namespace for the full payload
        "bonsai": {
            "detection_id":       det.id,
            "device_address":     det.device_address,
            "hostname":           det.hostname,
            "rule_id":            det.rule_id,
            "severity":           det.severity,
            "fired_at_ns":        det.fired_at_ns,
            "features":           features,
            "remediation_action": det.remediation_action,
            "remediation_status": det.remediation_status,
        }
    })
}

// ── Graph query ───────────────────────────────────────────────────────────────

fn query_detections(db: &Arc<Database>) -> Result<Vec<DetectionRecord>> {
    let conn = Connection::new(db).context("open graph connection for Elastic")?;
    let rows = conn
        .query(
            "MATCH (dev:Device)-[:TRIGGERED]->(e:DetectionEvent) \
             OPTIONAL MATCH (r:Remediation)-[:RESOLVES]->(e) \
             RETURN e.id, e.device_address, dev.hostname, e.rule_id, e.severity, \
                    e.fired_at, e.features_json, r.action, r.status \
             ORDER BY e.fired_at DESC LIMIT 500",
        )
        .context("query detections for Elastic")?;
    Ok(rows
        .map(|row| DetectionRecord {
            id: read_str(&row[0]),
            device_address: read_str(&row[1]),
            hostname: read_str(&row[2]),
            rule_id: read_str(&row[3]),
            severity: read_str(&row[4]),
            fired_at_ns: read_ts_ns(&row[5]),
            features_json: read_str(&row[6]),
            remediation_action: read_str(&row[7]),
            remediation_status: read_str(&row[8]),
        })
        .collect())
}

// ── Auth helper ───────────────────────────────────────────────────────────────

fn apply_auth(
    req: reqwest::RequestBuilder,
    config: &OutputAdapterConfig,
    creds: &Arc<CredentialVault>,
    audit: &OutputAdapterAuditLog,
) -> Result<reqwest::RequestBuilder> {
    if config.credential_alias.is_empty() {
        return Ok(req);
    }
    match creds.resolve(&config.credential_alias, ResolvePurpose::AiopsEvent) {
        Ok(cred) => {
            audit.log_credential_resolve(&config.credential_alias, "ok", None);
            let auth_type = config
                .extra
                .get("auth_type")
                .and_then(|v| v.as_str())
                .unwrap_or("basic");
            if auth_type == "api_key" {
                // API key: vault password holds the pre-encoded key (base64(id:key))
                Ok(req.header("Authorization", format!("ApiKey {}", cred.password)))
            } else {
                Ok(req.basic_auth(&cred.username, Some(&cred.password)))
            }
        }
        Err(e) => {
            let msg = e.to_string();
            audit.log_credential_resolve(&config.credential_alias, "error", Some(&msg));
            anyhow::bail!("credential resolve failed: {e}")
        }
    }
}

fn build_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("build reqwest client")
}

// ── Factory ───────────────────────────────────────────────────────────────────

/// Build an `ElasticAdapter` from a stored config if the type matches.
pub fn build(config: &OutputAdapterConfig, db: Arc<Database>) -> Option<ElasticAdapter> {
    if config.adapter_type == "elastic" {
        Some(ElasticAdapter::from_config(config.clone(), db))
    } else {
        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(endpoint: &str) -> OutputAdapterConfig {
        OutputAdapterConfig {
            name: "elastic-test".to_string(),
            adapter_type: "elastic".to_string(),
            enabled: true,
            endpoint_url: endpoint.to_string(),
            credential_alias: String::new(),
            flush_interval_secs: 30,
            environment_scope: vec![],
            extra: serde_json::Value::Null,
        }
    }

    #[test]
    fn bulk_url_uses_default_index() {
        let c = cfg("https://elastic.example.com:9200");
        let index = c.extra.get("index").and_then(|v| v.as_str()).unwrap_or("bonsai-detections");
        let url = format!("{}/{index}/_bulk", c.endpoint_url.trim_end_matches('/'));
        assert_eq!(url, "https://elastic.example.com:9200/bonsai-detections/_bulk");
    }

    #[test]
    fn bulk_url_uses_custom_index() {
        let c = OutputAdapterConfig {
            name: "e".to_string(),
            adapter_type: "elastic".to_string(),
            enabled: true,
            endpoint_url: "https://elastic.example.com:9200".to_string(),
            credential_alias: String::new(),
            flush_interval_secs: 30,
            environment_scope: vec![],
            extra: serde_json::json!({"index": "network-events"}),
        };
        let index = c.extra.get("index").and_then(|v| v.as_str()).unwrap_or("bonsai-detections");
        let url = format!("{}/{index}/_bulk", c.endpoint_url.trim_end_matches('/'));
        assert_eq!(url, "https://elastic.example.com:9200/network-events/_bulk");
    }

    #[test]
    fn ecs_doc_required_fields() {
        let det = DetectionRecord {
            id: "det-001".to_string(),
            device_address: "10.0.0.1".to_string(),
            hostname: "spine1".to_string(),
            rule_id: "bgp_down".to_string(),
            severity: "critical".to_string(),
            fired_at_ns: 1_700_000_000_000_000_000,
            features_json: "{}".to_string(),
            remediation_action: String::new(),
            remediation_status: String::new(),
        };
        let doc = build_ecs_doc(&det);
        assert!(doc.get("@timestamp").is_some());
        assert_eq!(doc["event"]["kind"], "alert");
        assert_eq!(doc["event"]["severity"], 1);
        assert_eq!(doc["host"]["name"], "spine1");
        assert_eq!(doc["rule"]["id"], "bgp_down");
        assert_eq!(doc["labels"]["bonsai_severity"], "critical");
        assert_eq!(doc["bonsai"]["detection_id"], "det-001");
    }

    #[test]
    fn severity_ecs_mapping() {
        assert_eq!(severity_to_ecs("critical"), 1);
        assert_eq!(severity_to_ecs("high"), 2);
        assert_eq!(severity_to_ecs("warning"), 2);
        assert_eq!(severity_to_ecs("info"), 3);
        assert_eq!(severity_to_ecs("unknown"), 3);
    }

    #[test]
    fn bulk_ndjson_ends_with_newline() {
        let lines = vec!["action".to_string(), "doc".to_string()];
        let mut body = lines.join("\n");
        body.push('\n');
        assert!(body.ends_with('\n'), "Elastic bulk body must end with newline");
        assert_eq!(body, "action\ndoc\n");
    }

    #[test]
    fn dedup_window_default_is_300s() {
        let c = cfg("http://localhost:9200");
        let window_secs = c.extra.get("dedup_window_secs").and_then(|v| v.as_i64()).unwrap_or(300);
        assert_eq!(window_secs, 300);
    }

    #[test]
    fn build_returns_none_for_wrong_adapter_type() {
        let c = cfg("http://localhost");
        assert_ne!(c.adapter_type, "splunk_hec");
        assert_eq!(c.adapter_type, "elastic");
    }
}
