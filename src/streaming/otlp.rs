//! BN-021e: OTLP HTTP receiver — provisional source identity only.
//!
//! OTLP telemetry arrives over unauthenticated HTTP/JSON (no gRPC, no
//! demonstrated authenticated-ingress principal). Every attribute inside
//! the payload — `service.name`, `host.name`, `peer.address`, metric
//! names and values — is a self-reported claim by the telemetry
//! producer. This receiver therefore:
//!
//! - keys every observation to a provisional
//!   `otlpsrc/{collector}/http/{peer_ip}:{peer_port}` source identity
//!   derived from the observed TCP peer, never from payload fields;
//! - preserves reported resource/span attributes as evidence inside the
//!   published value;
//! - never lets claims populate `TelemetryUpdate.target` or any
//!   canonical graph authority (the graph projection fails closed);
//! - enforces explicit per-request ingestion budgets so a hostile or
//!   buggy producer cannot drive unbounded work or high-cardinality
//!   graph writes.
//!
//! HTTP keep-alive means one TCP connection carries many requests; the
//! source key is per-connection (ip:port), which is bounded by live
//! connections and does not grow from request IDs or ephemeral labels.
//! A trusted reverse proxy / collector relay with real authentication
//! is the BN-021g verified-ingress path — not implemented here.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::{
    extract::{ConnectInfo, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::info;

use crate::config::OtlpConfig;
use crate::event_bus::InProcessBus;
use crate::streaming::netflow::encode_key_component;
use crate::telemetry::TelemetryUpdate;

// ── Ingestion budgets (Phase: resource & ingestion safety) ─────────────────
//
// Axum's `Json` extractor already caps the raw body (DefaultBodyLimit,
// 2 MiB), but a small JSON body can still contain very large arrays.
// These budgets bound per-request *work*: items beyond the cap are
// dropped with an observable metric — never silently, and never
// reported as accepted.
const MAX_RESOURCE_GROUPS: usize = 32;
const MAX_SCOPE_GROUPS_PER_RESOURCE: usize = 32;
const MAX_ITEMS_PER_SCOPE: usize = 1024;
const MAX_ITEMS_PER_REQUEST: usize = 4096;
const MAX_ATTRIBUTES_SCANNED: usize = 256;
const MAX_STRING_FIELD_LEN: usize = 512;
/// Reject timestamps more than one day in the future (clock skew
/// tolerance); non-positive or unparseable values fall back to now.
const MAX_FUTURE_SKEW_NS: i64 = 86_400_000_000_000;

#[derive(Clone)]
struct AppState {
    bus: Arc<InProcessBus>,
    collector_id: String,
}

/// Per-connection provisional source context. The observed TCP peer is
/// evidence only — HTTP headers, X-Forwarded-For and payload fields are
/// client-controlled and are NOT consulted for identity.
struct OtlpSource {
    source_key: String,
    peer_endpoint: String,
    collector_id: String,
}

impl OtlpSource {
    fn http(peer: SocketAddr, collector_id: &str) -> Self {
        Self {
            source_key: format!(
                "otlpsrc/{}/http/{}",
                encode_key_component(collector_id),
                peer
            ),
            peer_endpoint: peer.to_string(),
            collector_id: collector_id.to_string(),
        }
    }

    /// Source-level identity envelope shared by every update from this
    /// connection. `identity_status` is `provisional` by construction —
    /// plain OTLP/HTTP has no authenticated ingress, so
    /// `resolve_flow_identity` is never even needed to prove the
    /// negative: there is no verified principal to resolve.
    fn identity_json(&self) -> Value {
        json!({
            "source_key": self.source_key,
            "exporter_id": self.source_key,
            "identity_status": "provisional",
            "device_id": Value::Null,
            "canonical_exporter_id": Value::Null,
            "identity_resolution": Value::Null,
            "transport_source": self.peer_endpoint,
            "transport_protocol": "http",
            "collector_id": self.collector_id,
            "authority": "unverified",
        })
    }

    /// Consistent provisional application key for a reported service
    /// name: scoped to THIS source connection, so the same service name
    /// reported by different collectors/senders never merges, and raw
    /// service labels are never treated as global identifiers. Evidence
    /// only — no canonical Application node is created from it.
    fn provisional_app_id(&self, service_name: &str) -> String {
        let svc: String = service_name.chars().take(128).collect();
        format!("otlpapp/{}/{}", self.source_key, encode_key_component(&svc))
    }
}

/// Truncate a client-controlled string field before it reaches a bus
/// payload or graph property.
fn clip(s: &str) -> String {
    s.chars().take(MAX_STRING_FIELD_LEN).collect()
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// Sanitize a producer-supplied timestamp: unparseable, non-positive or
/// implausibly far-future values fall back to receive time.
fn sane_timestamp_ns(raw: Option<i64>, now: i64) -> i64 {
    match raw {
        Some(ns) if ns > 0 && ns <= now + MAX_FUTURE_SKEW_NS => ns,
        _ => now,
    }
}

/// Mutable per-request work budget. `take` returns false once exhausted;
/// callers stop iterating and record a single drop metric per endpoint.
struct Budget {
    items_left: usize,
    per_scope_left: usize,
    exhausted: bool,
}

impl Budget {
    fn new() -> Self {
        Self {
            items_left: MAX_ITEMS_PER_REQUEST,
            per_scope_left: MAX_ITEMS_PER_SCOPE,
            exhausted: false,
        }
    }
    fn take(&mut self) -> bool {
        if self.items_left == 0 || self.per_scope_left == 0 {
            self.exhausted = true;
            return false;
        }
        self.items_left -= 1;
        self.per_scope_left -= 1;
        true
    }
    fn reset_scope(&mut self) {
        self.per_scope_left = MAX_ITEMS_PER_SCOPE;
    }
}

fn note_dropped(endpoint: &str) {
    metrics::counter!("bonsai_otlp_items_dropped_total", "endpoint" => endpoint.to_string())
        .increment(1);
}

pub async fn run_otlp_receiver(
    cfg: OtlpConfig,
    bus: Arc<InProcessBus>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let listener = TcpListener::bind(&cfg.http_addr)
        .await
        .with_context(|| format!("bind OTLP HTTP listener on {}", cfg.http_addr))?;

    info!(addr = %cfg.http_addr, "OTLP HTTP receiver listening");

    let state = AppState {
        bus,
        collector_id: cfg.collector_id.clone(),
    };
    let app = Router::new()
        .route("/v1/traces", post(traces_handler))
        .route("/v1/metrics", post(metrics_handler))
        .with_state(state);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        loop {
            if shutdown.changed().await.is_err() || *shutdown.borrow() {
                break;
            }
        }
    })
    .await
    .context("OTLP HTTP server error")
}

async fn traces_handler(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> StatusCode {
    let src = OtlpSource::http(peer, &state.collector_id);
    let identity = src.identity_json();
    let now = now_ns();
    let mut budget = Budget::new();
    let mut groups = 0usize;

    let Some(resource_spans) = body.get("resourceSpans").and_then(|v| v.as_array()) else {
        return StatusCode::OK;
    };

    'outer: for rs in resource_spans.iter().take(MAX_RESOURCE_GROUPS) {
        groups += 1;
        let resource_attrs = rs
            .get("resource")
            .and_then(|r| r.get("attributes"))
            .and_then(|a| a.as_array());
        let service_name = clip(&extract_attr_str(resource_attrs, "service.name"));
        let host_name = clip(&extract_attr_str(resource_attrs, "host.name"));
        let provisional_app_id = if service_name.is_empty() {
            String::new()
        } else {
            src.provisional_app_id(&service_name)
        };

        let Some(scope_spans) = rs.get("scopeSpans").and_then(|v| v.as_array()) else {
            continue;
        };

        for ss in scope_spans.iter().take(MAX_SCOPE_GROUPS_PER_RESOURCE) {
            budget.reset_scope();
            let Some(spans) = ss.get("spans").and_then(|v| v.as_array()) else {
                continue;
            };
            for span in spans.iter().take(MAX_ITEMS_PER_SCOPE) {
                if !budget.take() {
                    note_dropped("traces");
                    break 'outer;
                }
                let attrs = span.get("attributes").and_then(|a| a.as_array());
                let peer_address = clip(&extract_attr_str(attrs, "peer.address"));
                let db_name = clip(&extract_attr_str(attrs, "db.name"));
                let http_url = clip(&extract_attr_str(attrs, "http.url"));

                let span_ns = span
                    .get("startTimeUnixNano")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<i64>().ok())
                    .or_else(|| span.get("startTimeUnixNano").and_then(|v| v.as_i64()))
                    .map(|ns| sane_timestamp_ns(Some(ns), now))
                    .unwrap_or(now);

                let mut value = json!({
                    "service_name": service_name,
                    "host_name": host_name,
                    "peer_address": peer_address,
                    "db_name": db_name,
                    "http_url": http_url,
                    "span_name": clip(span.get("name").and_then(|v| v.as_str()).unwrap_or("")),
                    "trace_id": clip(span.get("traceId").and_then(|v| v.as_str()).unwrap_or("")),
                    "span_id": clip(span.get("spanId").and_then(|v| v.as_str()).unwrap_or("")),
                    "provisional_app_id": provisional_app_id,
                });
                merge_identity(&mut value, &identity);

                state.bus.publish(TelemetryUpdate {
                    target: src.source_key.clone(),
                    vendor: String::new(),
                    hostname: String::new(),
                    role: String::new(),
                    site: String::new(),
                    timestamp_ns: span_ns,
                    path: "streaming/otlp/span".to_string(),
                    value,
                });
            }
        }
    }
    if groups >= MAX_RESOURCE_GROUPS || resource_spans.len() > MAX_RESOURCE_GROUPS {
        note_dropped("traces");
    }

    StatusCode::OK
}

/// D4-10 T2 / BN-021e: OTLP /v1/metrics handler.
/// Parses ExportMetricsServiceRequest (JSON) — gauge, sum, and histogram
/// metric types — and publishes one provisional TelemetryUpdate per
/// data point on "streaming/otlp/metrics". Metric values are evidence
/// scoped to the source connection; they never overwrite canonical
/// Application health columns.
async fn metrics_handler(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> StatusCode {
    let src = OtlpSource::http(peer, &state.collector_id);
    let identity = src.identity_json();
    let now = now_ns();
    let mut budget = Budget::new();
    let mut groups = 0usize;

    let Some(resource_metrics) = body.get("resourceMetrics").and_then(|v| v.as_array()) else {
        return StatusCode::OK;
    };

    'outer: for rm in resource_metrics.iter().take(MAX_RESOURCE_GROUPS) {
        groups += 1;
        let resource_attrs = rm
            .get("resource")
            .and_then(|r| r.get("attributes"))
            .and_then(|a| a.as_array());
        let service_name = clip(&extract_attr_str(resource_attrs, "service.name"));
        let host_name = clip(&extract_attr_str(resource_attrs, "host.name"));
        let provisional_app_id = if service_name.is_empty() {
            String::new()
        } else {
            src.provisional_app_id(&service_name)
        };

        let Some(scope_metrics) = rm.get("scopeMetrics").and_then(|v| v.as_array()) else {
            continue;
        };

        for sm in scope_metrics.iter().take(MAX_SCOPE_GROUPS_PER_RESOURCE) {
            budget.reset_scope();
            let Some(metrics) = sm.get("metrics").and_then(|v| v.as_array()) else {
                continue;
            };

            for metric in metrics.iter().take(MAX_ITEMS_PER_SCOPE) {
                let metric_name = clip(metric.get("name").and_then(|v| v.as_str()).unwrap_or(""));
                let (data_points, kind) = if let Some(g) = metric.get("gauge") {
                    (g.get("dataPoints").and_then(|v| v.as_array()), "gauge")
                } else if let Some(s) = metric.get("sum") {
                    (s.get("dataPoints").and_then(|v| v.as_array()), "sum")
                } else if let Some(h) = metric.get("histogram") {
                    (h.get("dataPoints").and_then(|v| v.as_array()), "histogram")
                } else {
                    continue;
                };

                let Some(dps) = data_points else { continue };

                for dp in dps.iter().take(MAX_ITEMS_PER_SCOPE) {
                    if !budget.take() {
                        note_dropped("metrics");
                        break 'outer;
                    }
                    let point_ns = dp
                        .get("timeUnixNano")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<i64>().ok())
                        .or_else(|| dp.get("timeUnixNano").and_then(|v| v.as_i64()))
                        .map(|ns| sane_timestamp_ns(Some(ns), now))
                        .unwrap_or(now);

                    let value_f = if kind == "histogram" {
                        // Use sum/count as rate estimate
                        let sum = dp.get("sum").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let count = dp.get("count").and_then(|v| v.as_u64()).unwrap_or(1).max(1);
                        sum / count as f64
                    } else {
                        dp.get("asDouble")
                            .and_then(|v| v.as_f64())
                            .or_else(|| dp.get("asInt").and_then(|v| v.as_i64()).map(|i| i as f64))
                            .unwrap_or(0.0)
                    };
                    // JSON cannot express NaN/Infinity, but a histogram
                    // sum/count estimate can still overflow to inf.
                    // A non-finite value is malformed evidence — drop the
                    // point rather than fabricate a metric.
                    let value_f = match value_f {
                        v if v.is_finite() => v,
                        _ => {
                            note_dropped("metrics");
                            continue;
                        }
                    };

                    let dp_attrs = dp.get("attributes").and_then(|v| v.as_array());
                    let peer_address = clip(&extract_attr_str(dp_attrs, "peer.address"));

                    let mut value = json!({
                        "service_name": service_name,
                        "host_name": host_name,
                        "metric_name": metric_name,
                        "metric_kind": kind,
                        "value": value_f,
                        "peer_address": peer_address,
                        "provisional_app_id": provisional_app_id,
                    });
                    merge_identity(&mut value, &identity);

                    state.bus.publish(TelemetryUpdate {
                        target: src.source_key.clone(),
                        vendor: String::new(),
                        hostname: String::new(),
                        role: String::new(),
                        site: String::new(),
                        timestamp_ns: point_ns,
                        path: "streaming/otlp/metrics".to_string(),
                        value,
                    });
                }
            }
        }
    }
    if groups >= MAX_RESOURCE_GROUPS || resource_metrics.len() > MAX_RESOURCE_GROUPS {
        note_dropped("metrics");
    }

    StatusCode::OK
}

/// Merge the source identity envelope into a published value. All
/// identity fields are provisional evidence — the graph projection
/// treats them as hints for read-only probes only.
fn merge_identity(value: &mut Value, identity: &Value) {
    if let (Value::Object(map), Value::Object(id_map)) = (value, identity) {
        for (k, v) in id_map {
            map.insert(k.clone(), v.clone());
        }
    }
}

fn extract_attr_str(attrs: Option<&Vec<Value>>, key: &str) -> String {
    let Some(attrs) = attrs else {
        return String::new();
    };
    for attr in attrs.iter().take(MAX_ATTRIBUTES_SCANNED) {
        let Some(k) = attr.get("key").and_then(|v| v.as_str()) else {
            continue;
        };
        if k != key {
            continue;
        }
        if let Some(sv) = attr
            .get("value")
            .and_then(|v| v.get("stringValue"))
            .and_then(|v| v.as_str())
        {
            return sv.to_string();
        }
    }
    String::new()
}
