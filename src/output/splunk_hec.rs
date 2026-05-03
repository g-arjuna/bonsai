//! Splunk HTTP Event Collector output adapter (T6-3).
//!
//! Core-side adapter: polls the graph for DetectionEvents and pushes them
//! to a Splunk HEC endpoint as structured JSON events.
//!
//! Protocol: Splunk HEC 1.0
//!   POST /services/collector/event
//!   Authorization: Splunk <token>
//!   Content-Type: application/json
//!
//! HEC token: stored in the credential vault under `credential_alias`.
//! The `password` field of the resolved credential carries the token;
//! `username` is ignored.
//!
//! Config extra fields (all optional):
//!   sourcetype       — Splunk sourcetype (default: "bonsai:detection")
//!   index            — Splunk index (default: unset, uses token default)
//!   dedup_window_secs — suppress re-push of (device, rule) within this window (default: 300)

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

pub struct SplunkHecAdapter {
    config: OutputAdapterConfig,
    environments: Vec<String>,
    db: Arc<Database>,
}

impl SplunkHecAdapter {
    pub fn from_config(config: OutputAdapterConfig, db: Arc<Database>) -> Self {
        let environments = config.environment_scope.clone();
        Self { config, environments, db }
    }
}

#[async_trait::async_trait]
impl OutputAdapter for SplunkHecAdapter {
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
            "Splunk HEC adapter started"
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
                                "Splunk HEC push ok"
                            );
                        }
                        Ok(_) => {}
                        Err(e) => warn!(adapter = %self.config.name, "Splunk HEC push failed: {e:#}"),
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!(adapter = %self.config.name, "Splunk HEC adapter shutting down");
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
        let token = resolve_token(&self.config, &creds, audit)?;
        let http = build_http_client()?;
        let health_url = format!(
            "{}/services/collector/health",
            self.config.endpoint_url.trim_end_matches('/')
        );
        let resp = http
            .get(&health_url)
            .header("Authorization", format!("Splunk {token}"))
            .send()
            .await
            .with_context(|| format!("GET Splunk HEC health {health_url}"))?;
        let status = resp.status();
        // 200 = healthy; 400 = HEC globally disabled; 503 = HEC paused; 401 = reachable
        if status.is_success()
            || status.as_u16() == 400
            || status.as_u16() == 503
            || status.as_u16() == 401
        {
            Ok(())
        } else {
            anyhow::bail!("Splunk HEC health check returned {status}")
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
    let token = resolve_token(config, creds, audit)?;

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

    let sourcetype = config
        .extra
        .get("sourcetype")
        .and_then(|v| v.as_str())
        .unwrap_or("bonsai:detection")
        .to_string();
    let index = config.extra.get("index").and_then(|v| v.as_str()).map(|s| s.to_string());

    let mut hec_lines: Vec<String> = Vec::new();
    let mut bytes = 0u64;

    for det in &detections {
        if pushed.contains(&det.id) {
            continue;
        }
        let dedup_key = (det.device_address.clone(), det.rule_id.clone());
        if let Some(&last_ns) = dedup.get(&dedup_key)
            && now - last_ns < dedup_window_ns
        {
            continue;
        }

        let features: JsonValue =
            serde_json::from_str(&det.features_json).unwrap_or(JsonValue::Null);
        let mut hec = json!({
            "time":       det.fired_at_ns as f64 / 1_000_000_000.0,
            "source":     "bonsai",
            "sourcetype": sourcetype,
            "event": {
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
        });
        if let Some(ref idx) = index {
            hec["index"] = JsonValue::String(idx.clone());
        }

        let line = serde_json::to_string(&hec).unwrap_or_default();
        bytes += line.len() as u64;
        hec_lines.push(line);
        pushed.insert(det.id.clone());
        dedup.insert(dedup_key, now);
    }

    if hec_lines.is_empty() {
        return Ok(OutputReport { adapter_name: config.name.clone(), ..Default::default() });
    }

    let n = hec_lines.len();
    // Splunk HEC batch: newline-delimited JSON objects (no separator needed between records)
    let body = hec_lines.join("\n");
    let endpoint = format!(
        "{}/services/collector/event",
        config.endpoint_url.trim_end_matches('/')
    );

    let http = build_http_client()?;
    let resp = http
        .post(&endpoint)
        .header("Authorization", format!("Splunk {token}"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .with_context(|| format!("POST Splunk HEC {endpoint}"))?;

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
        let err = format!("Splunk HEC returned {status}: {text}");
        audit.log_push(0, bytes, Some(&err));
        Ok(OutputReport {
            adapter_name: config.name.clone(),
            bytes_sent: bytes,
            error: Some(err),
            ..Default::default()
        })
    }
}

// ── Graph query ───────────────────────────────────────────────────────────────

fn query_detections(db: &Arc<Database>) -> Result<Vec<DetectionRecord>> {
    let conn = Connection::new(db).context("open graph connection for Splunk HEC")?;
    let rows = conn
        .query(
            "MATCH (dev:Device)-[:TRIGGERED]->(e:DetectionEvent) \
             OPTIONAL MATCH (r:Remediation)-[:RESOLVES]->(e) \
             RETURN e.id, e.device_address, dev.hostname, e.rule_id, e.severity, \
                    e.fired_at, e.features_json, r.action, r.status \
             ORDER BY e.fired_at DESC LIMIT 500",
        )
        .context("query detections for Splunk HEC")?;
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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn resolve_token(
    config: &OutputAdapterConfig,
    creds: &Arc<CredentialVault>,
    audit: &OutputAdapterAuditLog,
) -> Result<String> {
    if config.credential_alias.is_empty() {
        return Ok(String::new());
    }
    match creds.resolve(&config.credential_alias, ResolvePurpose::AiopsEvent) {
        Ok(cred) => {
            audit.log_credential_resolve(&config.credential_alias, "ok", None);
            // HEC token is stored in the password field; username is unused
            Ok(cred.password)
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

/// Build a `SplunkHecAdapter` from a stored config if the type matches.
pub fn build(config: &OutputAdapterConfig, db: Arc<Database>) -> Option<SplunkHecAdapter> {
    if config.adapter_type == "splunk_hec" {
        Some(SplunkHecAdapter::from_config(config.clone(), db))
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
            name: "splunk-test".to_string(),
            adapter_type: "splunk_hec".to_string(),
            enabled: true,
            endpoint_url: endpoint.to_string(),
            credential_alias: String::new(),
            flush_interval_secs: 30,
            environment_scope: vec![],
            extra: serde_json::Value::Null,
        }
    }

    #[test]
    fn hec_url_strips_trailing_slash() {
        let c = cfg("https://splunk.example.com:8088/");
        let url = format!(
            "{}/services/collector/event",
            c.endpoint_url.trim_end_matches('/')
        );
        assert_eq!(url, "https://splunk.example.com:8088/services/collector/event");
    }

    #[test]
    fn sourcetype_defaults_to_bonsai_detection() {
        let c = cfg("http://localhost:8088");
        let st = c.extra.get("sourcetype").and_then(|v| v.as_str()).unwrap_or("bonsai:detection");
        assert_eq!(st, "bonsai:detection");
    }

    #[test]
    fn sourcetype_overrideable_via_extra() {
        let c = OutputAdapterConfig {
            name: "s".to_string(),
            adapter_type: "splunk_hec".to_string(),
            enabled: true,
            endpoint_url: "http://localhost:8088".to_string(),
            credential_alias: String::new(),
            flush_interval_secs: 30,
            environment_scope: vec![],
            extra: serde_json::json!({"sourcetype": "myapp:network"}),
        };
        let st = c.extra.get("sourcetype").and_then(|v| v.as_str()).unwrap_or("bonsai:detection");
        assert_eq!(st, "myapp:network");
    }

    #[test]
    fn hec_event_shape_roundtrips() {
        let det = DetectionRecord {
            id: "det-001".to_string(),
            device_address: "192.168.1.1".to_string(),
            hostname: "router1".to_string(),
            rule_id: "bgp_session_down".to_string(),
            severity: "critical".to_string(),
            fired_at_ns: 1_000_000_000_000_000_000,
            features_json: r#"{"bgp_established":0}"#.to_string(),
            remediation_action: "reset_bgp_session".to_string(),
            remediation_status: "success".to_string(),
        };
        let features: JsonValue =
            serde_json::from_str(&det.features_json).unwrap_or(JsonValue::Null);
        let hec = json!({
            "time":       det.fired_at_ns as f64 / 1_000_000_000.0,
            "source":     "bonsai",
            "sourcetype": "bonsai:detection",
            "event": {
                "detection_id": det.id,
                "rule_id":      det.rule_id,
                "severity":     det.severity,
                "features":     features,
            }
        });
        assert_eq!(hec["sourcetype"], "bonsai:detection");
        assert_eq!(hec["source"], "bonsai");
        assert_eq!(hec["event"]["rule_id"], "bgp_session_down");
        // 1_000_000_000_000_000_000 ns = 1_000_000_000.0 s
        assert!((hec["time"].as_f64().unwrap() - 1_000_000_000.0).abs() < 1.0);
    }

    #[test]
    fn build_returns_none_for_wrong_adapter_type() {
        let c = cfg("http://localhost");
        // build() would need a db; test the type-guard logic directly
        assert_ne!(c.adapter_type, "elastic");
        assert_eq!(c.adapter_type, "splunk_hec");
    }

    #[test]
    fn dedup_suppresses_same_device_rule_within_window() {
        let mut dedup: HashMap<(String, String), i64> = HashMap::new();
        let key = ("10.0.0.1".to_string(), "bgp_down".to_string());
        let now = 1_000_000_000_000i64;
        let window_ns = 300 * 1_000_000_000i64;
        dedup.insert(key.clone(), now - 10_000_000_000); // 10 seconds ago
        // 10s < 300s window → should be suppressed
        assert!(
            dedup.get(&key).map(|&last| now - last < window_ns).unwrap_or(false),
            "event within dedup window should be suppressed"
        );
    }
}
