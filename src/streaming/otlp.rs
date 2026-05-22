use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::{Router, Json, extract::State, http::StatusCode, routing::post};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::info;

use crate::config::OtlpConfig;
use crate::event_bus::InProcessBus;
use crate::telemetry::TelemetryUpdate;

pub async fn run_otlp_receiver(
    cfg: OtlpConfig,
    bus: Arc<InProcessBus>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let listener = TcpListener::bind(&cfg.http_addr)
        .await
        .with_context(|| format!("bind OTLP HTTP listener on {}", cfg.http_addr))?;

    info!(addr = %cfg.http_addr, "OTLP HTTP receiver listening");

    let app = Router::new()
        .route("/v1/traces", post(traces_handler))
        .route("/v1/metrics", post(metrics_handler))
        .with_state(bus);

    axum::serve(listener, app)
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
    State(bus): State<Arc<InProcessBus>>,
    Json(body): Json<Value>,
) -> StatusCode {
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0);

    let Some(resource_spans) = body.get("resourceSpans").and_then(|v| v.as_array()) else {
        return StatusCode::OK;
    };

    for rs in resource_spans {
        let service_name = extract_attr_str(
            rs.get("resource")
                .and_then(|r| r.get("attributes"))
                .and_then(|a| a.as_array()),
            "service.name",
        );

        let Some(scope_spans) = rs.get("scopeSpans").and_then(|v| v.as_array()) else {
            continue;
        };

        for ss in scope_spans {
            let Some(spans) = ss.get("spans").and_then(|v| v.as_array()) else {
                continue;
            };

            for span in spans {
                let attrs = span.get("attributes").and_then(|a| a.as_array());
                let peer_address = extract_attr_str(attrs, "peer.address");
                let db_name = extract_attr_str(attrs, "db.name");
                let http_url = extract_attr_str(attrs, "http.url");

                let target = if !peer_address.is_empty() {
                    peer_address.clone()
                } else if !service_name.is_empty() {
                    service_name.clone()
                } else {
                    String::new()
                };

                let span_ns = span
                    .get("startTimeUnixNano")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(now_ns);

                let detail = serde_json::json!({
                    "service_name": service_name,
                    "peer_address": peer_address,
                    "db_name": db_name,
                    "http_url": http_url,
                    "span_name": span.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "trace_id": span.get("traceId").and_then(|v| v.as_str()).unwrap_or(""),
                    "span_id": span.get("spanId").and_then(|v| v.as_str()).unwrap_or(""),
                });

                bus.publish(TelemetryUpdate {
                    target,
                    vendor: String::new(),
                    hostname: service_name.clone(),
                    role: String::new(),
                    site: String::new(),
                    timestamp_ns: span_ns,
                    path: "streaming/otlp/span".to_string(),
                    value: detail,
                });
            }
        }
    }

    StatusCode::OK
}

/// D4-10 T2: OTLP /v1/metrics handler.
/// Parses ExportMetricsServiceRequest (JSON) — gauge, sum, and histogram metric
/// types — and publishes one TelemetryUpdate per data point on
/// "streaming/otlp/metrics".  The graph writer will upsert Application node
/// properties (cpu_pct, memory_mb, req_per_sec, error_rate) from these updates.
async fn metrics_handler(
    State(bus): State<Arc<InProcessBus>>,
    Json(body): Json<Value>,
) -> StatusCode {
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0);

    let Some(resource_metrics) = body.get("resourceMetrics").and_then(|v| v.as_array()) else {
        return StatusCode::OK;
    };

    for rm in resource_metrics {
        let service_name = extract_attr_str(
            rm.get("resource")
                .and_then(|r| r.get("attributes"))
                .and_then(|a| a.as_array()),
            "service.name",
        );
        let host_name = extract_attr_str(
            rm.get("resource")
                .and_then(|r| r.get("attributes"))
                .and_then(|a| a.as_array()),
            "host.name",
        );

        let Some(scope_metrics) = rm.get("scopeMetrics").and_then(|v| v.as_array()) else {
            continue;
        };

        for sm in scope_metrics {
            let Some(metrics) = sm.get("metrics").and_then(|v| v.as_array()) else {
                continue;
            };

            for metric in metrics {
                let metric_name = metric.get("name").and_then(|v| v.as_str()).unwrap_or("");
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

                for dp in dps {
                    let point_ns = dp
                        .get("timeUnixNano")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<i64>().ok())
                        .or_else(|| dp.get("timeUnixNano").and_then(|v| v.as_i64()))
                        .unwrap_or(now_ns);

                    let value = if kind == "histogram" {
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

                    let dp_attrs = dp.get("attributes").and_then(|v| v.as_array());
                    let peer_address = extract_attr_str(dp_attrs, "peer.address");

                    let target = if !peer_address.is_empty() {
                        peer_address.clone()
                    } else if !host_name.is_empty() {
                        host_name.clone()
                    } else if !service_name.is_empty() {
                        service_name.clone()
                    } else {
                        String::new()
                    };

                    bus.publish(TelemetryUpdate {
                        target,
                        vendor: String::new(),
                        hostname: service_name.clone(),
                        role: String::new(),
                        site: String::new(),
                        timestamp_ns: point_ns,
                        path: "streaming/otlp/metrics".to_string(),
                        value: serde_json::json!({
                            "service_name": service_name,
                            "metric_name": metric_name,
                            "metric_kind": kind,
                            "value": value,
                            "peer_address": peer_address,
                            "host_name": host_name,
                        }),
                    });
                }
            }
        }
    }

    StatusCode::OK
}

fn extract_attr_str(attrs: Option<&Vec<Value>>, key: &str) -> String {
    let Some(attrs) = attrs else {
        return String::new();
    };
    for attr in attrs {
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
