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
    response::{IntoResponse, Response},
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
/// callers stop iterating. Items never attempted are still counted as
/// rejected via `total_items - published` — drop accounting is exact,
/// not one-per-truncation-event.
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

/// Per-request accounting. `total_items` counts every span/data point
/// present in the request (including items under truncated resource
/// groups and unsupported metric kinds); `published` counts updates
/// actually emitted on the bus. `rejected = total - published` is the
/// exact number reported in the OTLP `partialSuccess` response.
#[derive(Default)]
struct RequestOutcome {
    total_items: usize,
    published: usize,
    /// Data points under metric kinds outside the supported subset
    /// (exponentialHistogram, summary, unknown).
    unsupported_items: usize,
    /// Items attempted but rejected because their numeric payload was
    /// missing, malformed or non-finite.
    invalid_items: usize,
}

impl RequestOutcome {
    fn rejected(&self) -> usize {
        self.total_items.saturating_sub(self.published)
    }
    fn over_budget(&self) -> usize {
        self.total_items
            .saturating_sub(self.published)
            .saturating_sub(self.unsupported_items)
            .saturating_sub(self.invalid_items)
    }
}

fn note_dropped(endpoint: &str, reason: &str, count: usize) {
    if count == 0 {
        return;
    }
    metrics::counter!(
        "bonsai_otlp_items_dropped_total",
        "endpoint" => endpoint.to_string(),
        "reason" => reason.to_string()
    )
    .increment(count as u64);
}

fn record_drops(endpoint: &str, outcome: &RequestOutcome) {
    note_dropped(endpoint, "invalid_value", outcome.invalid_items);
    note_dropped(endpoint, "unsupported_kind", outcome.unsupported_items);
    note_dropped(endpoint, "over_budget", outcome.over_budget());
}

/// OTLP/HTTP response: structurally valid requests always get 200 with
/// an `Export*ServiceResponse` body; rejected items are reported via
/// `partialSuccess.rejectedSpans`/`rejectedDataPoints`. Structurally
/// malformed requests (wrong container types, non-object body) get 400 —
/// they are never acknowledged as accepted.
fn otlp_response(outcome: &RequestOutcome, unit: &str) -> Response {
    let rejected = outcome.rejected();
    if rejected == 0 {
        return (StatusCode::OK, Json(json!({}))).into_response();
    }
    let field = match unit {
        "spans" => "rejectedSpans",
        _ => "rejectedDataPoints",
    };
    let mut partial = json!({ field: rejected });
    if let Value::Object(map) = &mut partial {
        let reasons = [
            ("invalid_value", outcome.invalid_items),
            ("unsupported_kind", outcome.unsupported_items),
            ("over_budget", outcome.over_budget()),
        ]
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(r, n)| format!("{r}={n}"))
        .collect::<Vec<_>>()
        .join(",");
        map.insert(
            "errorMessage".to_string(),
            Value::String(format!("{rejected} {unit} rejected ({reasons})")),
        );
    }
    (
        StatusCode::OK,
        Json(json!({ "partialSuccess": partial })),
    )
        .into_response()
}

fn bad_request(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": msg })),
    )
        .into_response()
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

/// Count spans contained in one `resourceSpans` entry (all scopes).
fn count_resource_spans(rs: &Value) -> usize {
    rs.get("scopeSpans")
        .and_then(|v| v.as_array())
        .map(|scopes| {
            scopes
                .iter()
                .map(|ss| {
                    ss.get("spans")
                        .and_then(|v| v.as_array())
                        .map(|s| s.len())
                        .unwrap_or(0)
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Strict structural check for the supported traces subset: wherever a
/// container field is present it must be an array (null = unset). Any
/// violation makes the whole request malformed → 400.
fn valid_traces_structure(resource_spans: &[Value]) -> bool {
    resource_spans.iter().all(|rs| {
        let Some(rs_obj) = rs.as_object() else {
            return false;
        };
        match rs_obj.get("scopeSpans") {
            None | Some(Value::Null) => true,
            Some(Value::Array(scopes)) => scopes.iter().all(|ss| {
                let Some(ss_obj) = ss.as_object() else {
                    return false;
                };
                match ss_obj.get("spans") {
                    None | Some(Value::Null) => true,
                    Some(Value::Array(spans)) => spans.iter().all(|s| s.is_object()),
                    Some(_) => false,
                }
            }),
            Some(_) => false,
        }
    })
}

async fn traces_handler(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
    if !body.is_object() {
        return bad_request("request body must be a JSON object");
    }
    let Some(rs_val) = body.get("resourceSpans") else {
        // Legitimately empty request — distinct from a malformed one.
        return otlp_response(&RequestOutcome::default(), "spans");
    };
    if rs_val.is_null() {
        return otlp_response(&RequestOutcome::default(), "spans");
    }
    let Some(resource_spans) = rs_val.as_array() else {
        return bad_request("resourceSpans must be an array");
    };
    if !valid_traces_structure(resource_spans) {
        return bad_request("scopeSpans/spans must be arrays");
    }

    let src = OtlpSource::http(peer, &state.collector_id);
    let identity = src.identity_json();
    let now = now_ns();
    let mut budget = Budget::new();
    let mut outcome = RequestOutcome {
        total_items: resource_spans.iter().map(count_resource_spans).sum(),
        ..Default::default()
    };

    'outer: for rs in resource_spans.iter().take(MAX_RESOURCE_GROUPS) {
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
                outcome.published += 1;
            }
        }
    }

    record_drops("traces", &outcome);
    otlp_response(&outcome, "spans")
}

/// Count data points attributable to one `metrics` entry: every
/// sub-object carrying a `dataPoints` array (gauge, sum, histogram,
/// exponentialHistogram, summary, …). Used for exact accept/reject
/// accounting — items under unsupported kinds are reported rejected,
/// never silently acknowledged.
fn count_metric_data_points(metric: &Value) -> usize {
    metric
        .as_object()
        .map(|obj| {
            obj.values()
                .filter_map(|v| v.as_object())
                .filter_map(|o| o.get("dataPoints"))
                .filter_map(|dp| dp.as_array())
                .map(|a| a.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Data points under metric kinds outside the supported subset
/// (gauge/sum/histogram) — exponentialHistogram, summary, etc.
fn unsupported_data_points(metric: &Value) -> usize {
    const SUPPORTED: [&str; 3] = ["gauge", "sum", "histogram"];
    metric
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter(|(k, _)| !SUPPORTED.contains(&k.as_str()))
                .filter_map(|(_, v)| v.as_object())
                .filter_map(|o| o.get("dataPoints"))
                .filter_map(|dp| dp.as_array())
                .map(|a| a.len())
                .sum()
        })
        .unwrap_or(0)
}

fn count_scope_metrics(sm: &Value) -> usize {
    sm.get("metrics")
        .and_then(|v| v.as_array())
        .map(|ms| ms.iter().map(count_metric_data_points).sum())
        .unwrap_or(0)
}

fn count_resource_metrics(rm: &Value) -> usize {
    rm.get("scopeMetrics")
        .and_then(|v| v.as_array())
        .map(|sms| sms.iter().map(count_scope_metrics).sum())
        .unwrap_or(0)
}

/// Strict structural check for the supported metrics subset. Every
/// container level must be an object/array as appropriate, and any
/// metric-kind object carrying `dataPoints` must carry an array —
/// a present-but-malformed `dataPoints` is a 400, not a silent
/// empty kind.
fn valid_metrics_structure(resource_metrics: &[Value]) -> bool {
    resource_metrics.iter().all(|rm| {
        let Some(rm_obj) = rm.as_object() else {
            return false;
        };
        match rm_obj.get("scopeMetrics") {
            None | Some(Value::Null) => true,
            Some(Value::Array(sms)) => sms.iter().all(|sm| {
                let Some(sm_obj) = sm.as_object() else {
                    return false;
                };
                match sm_obj.get("metrics") {
                    None | Some(Value::Null) => true,
                    Some(Value::Array(metrics)) => metrics.iter().all(|m| {
                        let Some(m_obj) = m.as_object() else {
                            return false;
                        };
                        m_obj.iter().all(|(key, v)| match key.as_str() {
                            "name" | "description" | "unit" => true,
                            "metadata" => v.is_null() || v.is_array(),
                            _ => match v {
                                Value::Null => true,
                                Value::Object(kind) => kind
                                    .get("dataPoints")
                                    .is_none_or(|dp| dp.is_array()),
                                _ => false,
                            },
                        })
                    }),
                    Some(_) => false,
                }
            }),
            Some(_) => false,
        }
    })
}

/// OTLP `double` fields are JSON numbers in the proto3 JSON mapping.
/// A string or non-finite value is a malformed observation, not a zero.
fn parse_otlp_double(v: &Value) -> Option<f64> {
    v.as_f64().filter(|f| f.is_finite())
}

/// OTLP `int64`/`uint64`/`fixed64` fields are canonically JSON strings,
/// but numeric JSON values are also accepted. Anything else is invalid.
fn parse_otlp_uint(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_i64().and_then(|i| u64::try_from(i).ok())),
        Value::String(s) => s.parse::<u64>().ok(),
        _ => None,
    }
}

/// OTLP `int64` → f64 for `AnyValue.intValue`. Canonical encoding is a
/// JSON string; a bare JSON integer is also accepted.
fn parse_otlp_int(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .map(|i| i as f64)
            .or_else(|| n.as_u64().map(|u| u as f64)),
        Value::String(s) => s.parse::<i64>().ok().map(|i| i as f64),
        _ => None,
    }
}

/// Extract the scalar value of a gauge/sum data point.
///
/// Returns `Some(f64)` for a genuine reported number — including an
/// explicit reported `0`. Returns `None` when the value is absent,
/// malformed, of an unsupported AnyValue kind, or non-finite: those
/// observations are rejected, never fabricated into `0.0`.
fn scalar_data_point_value(dp: &Value) -> Option<f64> {
    if let Some(v) = dp.get("asDouble") {
        // Present but malformed → reject rather than fall through to
        // asInt (the fields are a proto oneof; both-present is already
        // ambiguous, and a bad asDouble must not be silently replaced).
        return parse_otlp_double(v);
    }
    if let Some(v) = dp.get("asInt") {
        return parse_otlp_int(v);
    }
    // Any other AnyValue kind (asString/asBool/asArray/asKvlist) or no
    // value field at all → no numeric observation to report.
    None
}

/// Extract histogram evidence WITHOUT conflating semantics: cumulative
/// `count`, optional `sum`/`min`/`max` and bucket counts are preserved
/// as distinct fields. No mean/rate/request-per-second is synthesized —
/// `value` stays null for histograms.
///
/// Returns `None` when `count` is missing or malformed (a histogram
/// without a count is not a usable observation). A reported `count: 0`
/// is genuine evidence and is preserved.
fn histogram_data_point_value(dp: &Value) -> Option<Value> {
    let count = dp.get("count").and_then(parse_otlp_uint)?;

    let opt_double = |key: &str| -> Option<f64> {
        match dp.get(key) {
            None | Some(Value::Null) => None,
            Some(v) => parse_otlp_double(v),
        }
    };
    // A present-but-malformed optional field invalidates the point.
    for key in ["sum", "min", "max"] {
        if dp.get(key).is_some_and(|v| !v.is_null() && parse_otlp_double(v).is_none()) {
            return None;
        }
    }

    let bucket_count = dp
        .get("bucketCounts")
        .or_else(|| dp.get("bucket_counts"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let explicit_bounds = dp
        .get("explicitBounds")
        .or_else(|| dp.get("explicit_bounds"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    Some(json!({
        "histogram_count": count,
        "histogram_sum": opt_double("sum"),
        "histogram_min": opt_double("min"),
        "histogram_max": opt_double("max"),
        "histogram_bucket_count": bucket_count,
        "histogram_explicit_bound_count": explicit_bounds,
        "value_semantics": "histogram_cumulative",
    }))
}

/// D4-10 T2 / BN-021e: OTLP /v1/metrics handler.
/// Parses ExportMetricsServiceRequest (JSON) — gauge, sum, and histogram
/// metric types — and publishes one provisional TelemetryUpdate per
/// data point on "streaming/otlp/metrics". Metric values are evidence
/// scoped to the source connection; they never overwrite canonical
/// Application health columns.
///
/// Acceptance contract: structurally valid requests get 200 with
/// `partialSuccess.rejectedDataPoints` reporting the exact number of
/// data points not accepted (invalid numerics, unsupported kinds,
/// budget truncation). Structurally malformed requests get 400.
async fn metrics_handler(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
    if !body.is_object() {
        return bad_request("request body must be a JSON object");
    }
    let Some(rm_val) = body.get("resourceMetrics") else {
        return otlp_response(&RequestOutcome::default(), "data points");
    };
    if rm_val.is_null() {
        return otlp_response(&RequestOutcome::default(), "data points");
    }
    let Some(resource_metrics) = rm_val.as_array() else {
        return bad_request("resourceMetrics must be an array");
    };
    if !valid_metrics_structure(resource_metrics) {
        return bad_request("scopeMetrics/metrics/dataPoints must be arrays/objects");
    }

    let src = OtlpSource::http(peer, &state.collector_id);
    let identity = src.identity_json();
    let now = now_ns();
    let mut budget = Budget::new();
    let mut outcome = RequestOutcome {
        total_items: resource_metrics.iter().map(count_resource_metrics).sum(),
        ..Default::default()
    };

    'outer: for rm in resource_metrics.iter().take(MAX_RESOURCE_GROUPS) {
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
                outcome.unsupported_items += unsupported_data_points(metric);
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
                let metric_name = clip(metric.get("name").and_then(|v| v.as_str()).unwrap_or(""));

                for dp in dps.iter().take(MAX_ITEMS_PER_SCOPE) {
                    if !budget.take() {
                        break 'outer;
                    }
                    let point_ns = dp
                        .get("timeUnixNano")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<i64>().ok())
                        .or_else(|| dp.get("timeUnixNano").and_then(|v| v.as_i64()))
                        .map(|ns| sane_timestamp_ns(Some(ns), now))
                        .unwrap_or(now);

                    // B1: distinguish a genuine reported value (including
                    // an explicit 0) from a missing/malformed one. Invalid
                    // observations are rejected and counted — never
                    // fabricated into 0.0. Histograms carry cumulative
                    // count/sum/min/max/bucket evidence as distinct fields;
                    // no mean/rate/rps is synthesized (`value` stays null).
                    let mut extra_fields = Value::Null;
                    let scalar_value = if kind == "histogram" {
                        match histogram_data_point_value(dp) {
                            Some(fields) => {
                                extra_fields = fields;
                                Value::Null
                            }
                            None => {
                                outcome.invalid_items += 1;
                                continue;
                            }
                        }
                    } else {
                        match scalar_data_point_value(dp) {
                            Some(f) if f.is_finite() => json!(f),
                            _ => {
                                outcome.invalid_items += 1;
                                continue;
                            }
                        }
                    };

                    let dp_attrs = dp.get("attributes").and_then(|v| v.as_array());
                    let peer_address = clip(&extract_attr_str(dp_attrs, "peer.address"));

                    let mut value = json!({
                        "service_name": service_name,
                        "host_name": host_name,
                        "metric_name": metric_name,
                        "metric_kind": kind,
                        "value": scalar_value,
                        "peer_address": peer_address,
                        "provisional_app_id": provisional_app_id,
                    });
                    if let (Value::Object(map), Value::Object(f)) = (&mut value, extra_fields) {
                        for (k, v) in f {
                            map.insert(k, v);
                        }
                    }
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
                    outcome.published += 1;
                }
            }
        }
    }

    record_drops("metrics", &outcome);
    otlp_response(&outcome, "data points")
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
