//! Prometheus remote-write output adapter (T6-2).
//!
//! Subscribes to raw telemetry on the bus, extracts numeric counter values,
//! and pushes them to a Prometheus-compatible remote-write endpoint every
//! `flush_interval_secs` seconds.
//!
//! Protocol: Prometheus Remote Write 1.0
//!   Content-Type: application/x-protobuf
//!   Content-Encoding: snappy
//!   X-Prometheus-Remote-Write-Version: 0.1.0
//!
//! Labels attached to every metric:
//!   job, device, instance (hostname or address), vendor
//!   Plus key-filter labels extracted from the gNMI path (e.g. interface, peer_address)
//!
//! Metric naming: `bonsai_<path_segments>_<field>` (lowercase, hyphens → underscores).
//! Numeric fields in JSON objects are emitted individually.
//! Non-numeric values are silently dropped.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use prost::Message as ProstMessage;
use serde_json::Value as JsonValue;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::credentials::{CredentialVault, ResolvePurpose};
use crate::event_bus::InProcessBus;
use crate::output::traits::{
    OutputAdapter, OutputAdapterAuditLog, OutputAdapterConfig, OutputReport, OutputTopic,
};

// ── Prometheus remote-write protobuf types (inline, no .proto file needed) ───

/// Prometheus WriteRequest (remote_write.proto)
#[derive(Clone, PartialEq, prost::Message)]
struct WriteRequest {
    #[prost(message, repeated, tag = "1")]
    timeseries: Vec<TimeSeries>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct TimeSeries {
    /// Must be sorted alphabetically by label name. `__name__` goes first.
    #[prost(message, repeated, tag = "1")]
    labels: Vec<Label>,
    #[prost(message, repeated, tag = "2")]
    samples: Vec<Sample>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct Label {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(string, tag = "2")]
    value: String,
}

#[derive(Clone, PartialEq, prost::Message)]
struct Sample {
    #[prost(double, tag = "1")]
    value: f64,
    /// Milliseconds since Unix epoch.
    #[prost(int64, tag = "2")]
    timestamp: i64,
}

// ── Metric batch ──────────────────────────────────────────────────────────────

#[derive(Debug)]
struct MetricPoint {
    name: String,
    labels: Vec<(String, String)>, // sorted, excluding __name__
    value: f64,
    timestamp_ms: i64,
}

// ── Adapter ───────────────────────────────────────────────────────────────────

pub struct PrometheusRemoteWriteAdapter {
    config: OutputAdapterConfig,
    environments: Vec<String>,
}

impl PrometheusRemoteWriteAdapter {
    pub fn from_config(config: OutputAdapterConfig) -> Self {
        let environments = config.environment_scope.clone();
        Self { config, environments }
    }

    fn job_label(&self) -> String {
        self.config
            .extra
            .get("job")
            .and_then(|v| v.as_str())
            .unwrap_or("bonsai")
            .to_string()
    }
}

#[async_trait::async_trait]
impl OutputAdapter for PrometheusRemoteWriteAdapter {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn topics(&self) -> &[OutputTopic] {
        &[OutputTopic::RawTelemetry]
    }

    fn applies_to_environments(&self) -> &[String] {
        &self.environments
    }

    async fn run(
        &self,
        bus: Arc<InProcessBus>,
        creds: Arc<CredentialVault>,
        audit: OutputAdapterAuditLog,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        info!(adapter = %self.config.name, url = %self.config.endpoint_url, "Prometheus remote-write adapter started");

        let flush_secs = self.config.flush_interval_secs.max(5);
        let mut flush_interval = tokio::time::interval(Duration::from_secs(flush_secs));
        flush_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut rx = bus.subscribe();
        let mut buffer: Vec<MetricPoint> = Vec::new();
        let job = self.job_label();

        loop {
            tokio::select! {
                res = rx.recv() => {
                    match res {
                        Ok(update) => {
                            let ts_ms = update.timestamp_ns / 1_000_000;
                            let base_labels = base_labels_for(&update.target, &update.hostname, &update.vendor, &job, &update.role, &update.site);
                            extract_metrics(&update.path, &update.value, ts_ms, base_labels, &mut buffer);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(adapter = %self.config.name, skipped = n, "bus lagged — metrics dropped");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = flush_interval.tick() => {
                    if !buffer.is_empty() {
                        let points = std::mem::take(&mut buffer);
                        let start = Instant::now();
                        match push_batch(&self.config, &creds, &audit, points).await {
                            Ok(report) => {
                                let elapsed = start.elapsed().as_millis() as u64;
                                debug!(
                                    adapter = %self.config.name,
                                    events = report.events_pushed,
                                    bytes = report.bytes_sent,
                                    ms = elapsed,
                                    "remote-write push ok"
                                );
                            }
                            Err(e) => {
                                warn!(adapter = %self.config.name, "remote-write push failed: {e:#}");
                            }
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!(adapter = %self.config.name, "Prometheus adapter shutting down");
                        // Final flush
                        if !buffer.is_empty() {
                            let points = std::mem::take(&mut buffer);
                            let _ = push_batch(&self.config, &creds, &audit, points).await;
                        }
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
        // Send an empty WriteRequest — Prometheus returns 204 or 400 (both mean reachable).
        let empty = WriteRequest { timeseries: vec![] };
        let encoded = encode_write_request(&empty)?;
        let (status, _) = do_http_push(&self.config, &creds, audit, encoded).await?;
        // 204 = success, 400 = bad request but server is up, both are "reachable"
        if status.is_success() || status.as_u16() == 400 {
            Ok(())
        } else {
            anyhow::bail!("remote-write endpoint returned {status}")
        }
    }
}

// ── Path → metric name + labels ───────────────────────────────────────────────

/// Parse a gNMI path into a metric name prefix and extracted key-filter labels.
///
/// Input:  `/interface[name=eth0]/statistics`
/// Output: prefix = "interface_statistics", labels = [("interface", "eth0")]
fn parse_path(path: &str) -> (String, Vec<(String, String)>) {
    let mut labels: Vec<(String, String)> = Vec::new();
    let mut name_parts: Vec<String> = Vec::new();

    for segment in path.trim_start_matches('/').split('/') {
        if segment.is_empty() {
            continue;
        }
        if let Some(bracket) = segment.find('[') {
            let base = &segment[..bracket];
            let rest = &segment[bracket + 1..segment.len().saturating_sub(1)];
            // May have multiple key predicates: [name=eth0 peer-address=1.2.3.4]
            for kv in rest.split_whitespace() {
                if let Some((k, v)) = kv.split_once('=') {
                    let label_key = k.replace('-', "_");
                    labels.push((label_key, v.to_string()));
                }
            }
            if !base.is_empty() {
                name_parts.push(base.replace('-', "_"));
            }
        } else {
            name_parts.push(segment.replace('-', "_"));
        }
    }

    (name_parts.join("_").to_lowercase(), labels)
}

fn base_labels_for(target: &str, hostname: &str, vendor: &str, job: &str, role: &str, site: &str) -> Vec<(String, String)> {
    let instance = if hostname.is_empty() { target } else { hostname };
    let mut labels = vec![
        ("device".to_string(), target.to_string()),
        ("instance".to_string(), instance.to_string()),
        ("job".to_string(), job.to_string()),
        ("vendor".to_string(), vendor.to_string()),
    ];
    if !role.is_empty() {
        labels.push(("role".to_string(), role.to_string()));
    }
    if !site.is_empty() {
        labels.push(("site".to_string(), site.to_string()));
    }
    labels
}

/// Extract numeric metrics from a TelemetryUpdate value. Appends to `out`.
fn extract_metrics(
    path: &str,
    value: &JsonValue,
    timestamp_ms: i64,
    mut base_labels: Vec<(String, String)>,
    out: &mut Vec<MetricPoint>,
) {
    let (path_prefix, path_labels) = parse_path(path);

    // Merge path-extracted labels into base labels, then sort
    base_labels.extend(path_labels);
    base_labels.sort_by(|a, b| a.0.cmp(&b.0));
    base_labels.dedup_by(|a, b| a.0 == b.0);

    match value {
        JsonValue::Number(n) => {
            if let Some(f) = n.as_f64() {
                let name = format!("bonsai_{path_prefix}");
                out.push(MetricPoint {
                    name,
                    labels: base_labels,
                    value: f,
                    timestamp_ms,
                });
            }
        }
        JsonValue::Object(map) => {
            for (field, v) in map {
                let Some(f) = json_to_f64(v) else { continue };
                let field_clean = field.replace('-', "_").to_lowercase();
                let name = if path_prefix.is_empty() {
                    format!("bonsai_{field_clean}")
                } else {
                    format!("bonsai_{path_prefix}_{field_clean}")
                };
                out.push(MetricPoint {
                    name: name.clone(),
                    labels: base_labels.clone(),
                    value: f,
                    timestamp_ms,
                });
            }
        }
        // Strings, booleans, arrays — not emittable as Prometheus metrics
        _ => {}
    }
}

fn json_to_f64(v: &JsonValue) -> Option<f64> {
    match v {
        JsonValue::Number(n) => n.as_f64(),
        JsonValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

// ── Encoding + HTTP push ──────────────────────────────────────────────────────

fn encode_write_request(req: &WriteRequest) -> Result<Vec<u8>> {
    let proto_bytes = req.encode_to_vec();
    snap::raw::Encoder::new()
        .compress_vec(&proto_bytes)
        .context("snappy compress WriteRequest")
}

fn build_write_request(points: &[MetricPoint]) -> WriteRequest {
    // Deduplicate: group by (name, sorted labels) → take last value if multiple
    // For simplicity, emit each point as its own TimeSeries with one Sample.
    let timeseries: Vec<TimeSeries> = points
        .iter()
        .map(|p| {
            let mut labels: Vec<Label> = std::iter::once(Label {
                name: "__name__".to_string(),
                value: p.name.clone(),
            })
            .chain(p.labels.iter().map(|(k, v)| Label {
                name: k.clone(),
                value: v.clone(),
            }))
            .collect();
            // Labels must be sorted alphabetically; __name__ sorts first naturally.
            labels.sort_by(|a, b| a.name.cmp(&b.name));

            TimeSeries {
                labels,
                samples: vec![Sample {
                    value: p.value,
                    timestamp: p.timestamp_ms,
                }],
            }
        })
        .collect();
    WriteRequest { timeseries }
}

async fn push_batch(
    config: &OutputAdapterConfig,
    creds: &Arc<CredentialVault>,
    audit: &OutputAdapterAuditLog,
    points: Vec<MetricPoint>,
) -> Result<OutputReport> {
    let n = points.len();
    let write_req = build_write_request(&points);
    let body = encode_write_request(&write_req)?;
    let bytes = body.len() as u64;

    let (status, _) = do_http_push(config, creds, audit, body).await?;

    if !status.is_success() {
        let report = OutputReport {
            adapter_name: config.name.clone(),
            events_pushed: 0,
            bytes_sent: bytes,
            error: Some(format!("remote-write returned {status}")),
            ..Default::default()
        };
        audit.log_push(0, bytes, report.error.as_deref());
        return Ok(report);
    }

    let report = OutputReport {
        adapter_name: config.name.clone(),
        events_pushed: n,
        bytes_sent: bytes,
        ..Default::default()
    };
    audit.log_push(n, bytes, None);
    Ok(report)
}

async fn do_http_push(
    config: &OutputAdapterConfig,
    creds: &Arc<CredentialVault>,
    audit: &OutputAdapterAuditLog,
    body: Vec<u8>,
) -> Result<(reqwest::StatusCode, String)> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("build reqwest client")?;

    let mut req = http
        .post(&config.endpoint_url)
        .header("Content-Type", "application/x-protobuf")
        .header("Content-Encoding", "snappy")
        .header("X-Prometheus-Remote-Write-Version", "0.1.0")
        .body(body);

    // Resolve credentials if configured
    if !config.credential_alias.is_empty() {
        match creds.resolve(&config.credential_alias, ResolvePurpose::AiopsEvent) {
            Ok(cred) => {
                audit.log_credential_resolve(&config.credential_alias, "ok", None);
                req = req.basic_auth(&cred.username, Some(&cred.password));
            }
            Err(e) => {
                let msg = e.to_string();
                audit.log_credential_resolve(&config.credential_alias, "error", Some(&msg));
                anyhow::bail!("credential resolve failed: {e}");
            }
        }
    }

    let resp = req.send().await.context("send remote-write request")?;
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    Ok((status, body_text))
}

// ── Factory ───────────────────────────────────────────────────────────────────

/// Build a `PrometheusRemoteWriteAdapter` from a stored config.
pub fn build(config: &OutputAdapterConfig) -> Option<PrometheusRemoteWriteAdapter> {
    if config.adapter_type == "prometheus_remote_write" {
        Some(PrometheusRemoteWriteAdapter::from_config(config.clone()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_path_no_filters() {
        let (name, labels) = parse_path("/interfaces");
        assert_eq!(name, "interfaces");
        assert!(labels.is_empty());
    }

    #[test]
    fn parse_path_with_list_key() {
        let (name, labels) = parse_path("/interface[name=eth0]/statistics");
        assert_eq!(name, "interface_statistics");
        assert_eq!(labels, vec![("name".to_string(), "eth0".to_string())]);
    }

    #[test]
    fn parse_path_hyphen_in_segment() {
        let (name, labels) = parse_path("/network-instance[name=default]/protocols");
        assert_eq!(name, "network_instance_protocols");
        assert_eq!(labels, vec![("name".to_string(), "default".to_string())]);
    }

    #[test]
    fn parse_path_multiple_keys() {
        let (name, labels) = parse_path("/bgp/neighbor[peer-address=1.2.3.4]/afi-safi[afi-safi-name=IPV4_UNICAST]");
        assert_eq!(name, "bgp_neighbor_afi_safi");
        assert!(labels.iter().any(|(k, _)| k == "peer_address"));
        assert!(labels.iter().any(|(k, _)| k == "afi_safi_name"));
    }

    #[test]
    fn extract_single_number() {
        let mut out = Vec::new();
        extract_metrics(
            "/interface[name=eth0]/statistics/in-octets",
            &JsonValue::Number(serde_json::Number::from(12345u64)),
            1000,
            vec![("job".to_string(), "bonsai".to_string())],
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "bonsai_interface_statistics_in_octets");
        assert!((out[0].value - 12345.0).abs() < 1e-9);
    }

    #[test]
    fn extract_object_fields() {
        let mut out = Vec::new();
        let value = serde_json::json!({ "in-octets": 100, "out-octets": 200, "label": "eth0" });
        extract_metrics(
            "/interface[name=eth0]/statistics",
            &value,
            1000,
            vec![],
            &mut out,
        );
        // "label" is a string, should be skipped
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|m| m.name == "bonsai_interface_statistics_in_octets"));
        assert!(out.iter().any(|m| m.name == "bonsai_interface_statistics_out_octets"));
    }

    #[test]
    fn encode_empty_request_is_valid_snappy() {
        let req = WriteRequest { timeseries: vec![] };
        let encoded = encode_write_request(&req).unwrap();
        // Should be decodable
        let decoded = snap::raw::Decoder::new().decompress_vec(&encoded).unwrap();
        let roundtrip = WriteRequest::decode(decoded.as_slice()).unwrap();
        assert!(roundtrip.timeseries.is_empty());
    }
}
