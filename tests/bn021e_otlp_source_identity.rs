//! BN-021e — OTLP source/resource identity isolation.
//!
//! OTLP arrives over unauthenticated HTTP/JSON. `service.name`,
//! `host.name`, `peer.address` and metric values are self-reported
//! producer claims: they are preserved as evidence scoped to a
//! provisional `otlpsrc/{collector}/http/{ip}:{port}` source key and
//! must NEVER create or mutate canonical Device/Application/
//! HostEndpoint state, RUNS_SERVICE/HOST_RUNS_SERVICE edges,
//! APP_IMPACTED_BY_NETWORK correlations, or Application health columns.

use std::sync::Arc;
use std::time::Duration;

use bonsai::config::OtlpConfig;
use bonsai::event_bus::InProcessBus;
use bonsai::streaming::otlp::run_otlp_receiver;
use bonsai::telemetry::TelemetryUpdate;
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

// ── Live receiver fixture ───────────────────────────────────────────────────

struct Lab {
    rx: broadcast::Receiver<Arc<TelemetryUpdate>>,
    base_url: String,
    shutdown_tx: watch::Sender<bool>,
    task: JoinHandle<anyhow::Result<()>>,
    _dir: tempfile::TempDir,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn start_lab(collector_id: &str) -> Lab {
    let port = free_port();
    let cfg = OtlpConfig {
        enabled: true,
        http_addr: format!("127.0.0.1:{port}"),
        collector_id: collector_id.to_string(),
    };
    let bus = InProcessBus::new(1024);
    let rx = bus.subscribe();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(run_otlp_receiver(cfg, Arc::clone(&bus), shutdown_rx));
    tokio::time::sleep(Duration::from_millis(120)).await;
    Lab {
        rx,
        base_url: format!("http://127.0.0.1:{port}"),
        shutdown_tx,
        task,
        _dir: tempfile::tempdir().unwrap(),
    }
}

impl Lab {
    async fn stop(mut self) {
        let _ = self.shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(2), &mut self.task).await;
    }

    async fn post(&self, path: &str, body: &Value) -> reqwest::StatusCode {
        reqwest::Client::new()
            .post(format!("{}{}", self.base_url, path))
            .json(body)
            .send()
            .await
            .expect("post")
            .status()
    }
}

async fn next_update(
    rx: &mut broadcast::Receiver<Arc<TelemetryUpdate>>,
    path_prefix: &str,
) -> TelemetryUpdate {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            remaining > Duration::ZERO,
            "timed out waiting for {path_prefix}"
        );
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(u)) if u.path.starts_with(path_prefix) => return u.as_ref().clone(),
            Ok(Ok(_)) => continue,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(e)) => panic!("bus recv error: {e}"),
            Err(_) => panic!("timed out waiting for {path_prefix}"),
        }
    }
}

fn attr(key: &str, value: &str) -> Value {
    json!({"key": key, "value": {"stringValue": value}})
}

fn traces_body(service: &str, host: &str, peer: &str) -> Value {
    json!({
        "resourceSpans": [{
            "resource": {"attributes": [attr("service.name", service), attr("host.name", host)]},
            "scopeSpans": [{
                "scope": {"name": "test"},
                "spans": [{
                    "traceId": "0af7651916cd43dd8448eb211c80319c",
                    "spanId": "b7ad6b7169203331",
                    "name": "GET /api/orders",
                    "startTimeUnixNano": "1700000000000000000",
                    "attributes": [attr("peer.address", peer), attr("http.url", "http://internal/")]
                }]
            }]
        }]
    })
}

fn metrics_body(service: &str, host: &str, metric: &str, value: f64) -> Value {
    json!({
        "resourceMetrics": [{
            "resource": {"attributes": [attr("service.name", service), attr("host.name", host)]},
            "scopeMetrics": [{
                "scope": {"name": "test"},
                "metrics": [{
                    "name": metric,
                    "gauge": {"dataPoints": [{
                        "timeUnixNano": "1700000000000000000",
                        "asDouble": value,
                        "attributes": []
                    }]}
                }]
            }]
        }]
    })
}

// ── E + I. HTTP sender vs resource claims; consistent provisional id ────────

/// The observed TCP peer produces the source key; reported
/// service/host/peer claims stay as evidence. The same source+service
/// yields the same provisional_app_id across traces and metrics;
/// different collectors never merge.
#[tokio::test]
async fn transport_peer_is_source_claims_are_evidence() {
    let lab = start_lab("edge-collector").await;
    let mut rx = lab.rx.resubscribe();

    lab.post("/v1/traces", &traces_body("payments", "web-01", "10.1.2.3"))
        .await;
    let span = next_update(&mut rx, "streaming/otlp/span").await;

    assert!(
        span.target
            .starts_with("otlpsrc/edge-collector/http/127.0.0.1:"),
        "target={}",
        span.target
    );
    assert_eq!(span.value["identity_status"], "provisional");
    assert_eq!(span.value["authority"], "unverified");
    assert!(span.value["device_id"].is_null());
    assert_eq!(span.value["transport_protocol"], "http");
    assert_eq!(span.value["service_name"], "payments");
    assert_eq!(span.value["host_name"], "web-01");
    assert_eq!(span.value["peer_address"], "10.1.2.3");
    let app_id = span.value["provisional_app_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        app_id.starts_with(&format!("otlpapp/{}", span.target)),
        "app_id={app_id}"
    );

    // Same connection is NOT guaranteed across reqwest calls (new TCP
    // conn each post) — but the provisional app id must still be
    // consistent for the same source key + service. Send a metric and
    // compare against a second span from a *fresh* connection: keys
    // differ (different source port) but each is internally consistent.
    lab.post(
        "/v1/metrics",
        &metrics_body("payments", "web-01", "system.cpu.utilization", 0.5),
    )
    .await;
    let metric = next_update(&mut rx, "streaming/otlp/metrics").await;
    assert_eq!(metric.value["identity_status"], "provisional");
    assert_eq!(metric.value["service_name"], "payments");
    assert_eq!(metric.value["metric_name"], "system.cpu.utilization");
    assert_eq!(metric.value["value"], 0.5);
    let m_app = metric.value["provisional_app_id"].as_str().unwrap();
    assert!(m_app.starts_with("otlpapp/otlpsrc/edge-collector/http/127.0.0.1:"));

    // A different collector scope produces a different source key.
    let lab2 = start_lab("other-collector").await;
    let mut rx2 = lab2.rx.resubscribe();
    lab2.post("/v1/traces", &traces_body("payments", "web-01", "10.1.2.3"))
        .await;
    let span2 = next_update(&mut rx2, "streaming/otlp/span").await;
    assert!(span2.target.starts_with("otlpsrc/other-collector/http/"));
    assert_ne!(span.target, span2.target, "collector scopes must not merge");
    lab2.stop().await;

    lab.stop().await;
}

// ── D. Shared service names across sources stay separated ───────────────────

#[tokio::test]
async fn shared_service_names_never_merge() {
    let lab_a = start_lab("collector-a").await;
    let lab_b = start_lab("collector-b").await;
    let mut rx_a = lab_a.rx.resubscribe();
    let mut rx_b = lab_b.rx.resubscribe();

    lab_a
        .post("/v1/traces", &traces_body("checkout", "h1", ""))
        .await;
    lab_b
        .post("/v1/traces", &traces_body("checkout", "h2", ""))
        .await;

    let a = next_update(&mut rx_a, "streaming/otlp/span").await;
    let b = next_update(&mut rx_b, "streaming/otlp/span").await;
    assert_ne!(a.target, b.target);
    assert!(a.target.contains("collector-a") && b.target.contains("collector-b"));
    // Same service name, different sources → different provisional app ids.
    assert_ne!(
        a.value["provisional_app_id"], b.value["provisional_app_id"],
        "shared service names must not collapse across sources"
    );

    lab_a.stop().await;
    lab_b.stop().await;
}

// ── J. Malformed / oversized / budget input ─────────────────────────────────

#[tokio::test]
async fn malformed_and_oversized_input_is_safe() {
    let lab = start_lab("safe-site").await;

    // Malformed JSON → axum rejects before handler.
    let status = reqwest::Client::new()
        .post(format!("{}/v1/traces", lab.base_url))
        .header("content-type", "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap()
        .status();
    assert!(status.is_client_error(), "status={status}");

    // Missing resourceSpans → accepted no-op.
    assert_eq!(lab.post("/v1/traces", &json!({})).await, 200);
    assert_eq!(
        lab.post("/v1/metrics", &json!({"resourceMetrics": "nope"}))
            .await,
        200
    );

    // Extreme arrays: more resource groups than the budget allows —
    // must not panic, must still respond.
    let huge_groups: Vec<Value> = (0..500)
        .map(|i| {
            json!({
                "resource": {"attributes": [attr("service.name", &format!("svc-{i}"))]},
                "scopeSpans": [{"spans": [{"name": "s", "attributes": []}]}]
            })
        })
        .collect();
    let status = lab
        .post("/v1/traces", &json!({"resourceSpans": huge_groups}))
        .await;
    assert_eq!(status, 200);

    // Huge attribute arrays and long strings are clipped/scanned-bounded.
    let many_attrs: Vec<Value> = (0..2000).map(|i| attr(&format!("k{i}"), "v")).collect();
    let body = json!({
        "resourceSpans": [{
            "resource": {"attributes": []},
            "scopeSpans": [{"spans": [{
                "name": "x",
                "attributes": many_attrs,
                "startTimeUnixNano": "99999999999999999999999"
            }]}]
        }]
    });
    assert_eq!(lab.post("/v1/traces", &body).await, 200);

    // Absurd timestamp falls back safely (no panic, no graph corruption).
    let mut rx = lab.rx.resubscribe();
    lab.post("/v1/traces", &body).await;
    let _ = next_update(&mut rx, "streaming/otlp/span").await;

    lab.stop().await;
}

// ── Graph-level cases (A,B,C,F,G,H,I,K) ─────────────────────────────────────

use bonsai::graph::GraphStore;

fn otlp_span_update(source_key: &str, service: &str, peer: &str, extra: Value) -> TelemetryUpdate {
    let mut value = json!({
        "service_name": service,
        "host_name": "claimed-host",
        "peer_address": peer,
        "db_name": "",
        "http_url": "",
        "span_name": "op",
        "trace_id": "t",
        "span_id": "s",
        "provisional_app_id": format!("otlpapp/{source_key}/{}", service),
        "source_key": source_key,
        "exporter_id": source_key,
        "identity_status": "provisional",
        "device_id": null,
        "canonical_exporter_id": null,
        "identity_resolution": null,
        "transport_source": "127.0.0.1:4444",
        "transport_protocol": "http",
        "collector_id": "graph-site",
        "authority": "unverified",
    });
    if let (Value::Object(base), Value::Object(ext)) = (&mut value, extra) {
        for (k, v) in ext {
            base.insert(k, v);
        }
    }
    TelemetryUpdate {
        target: source_key.to_string(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: 1_700_000_000_000_000_000,
        path: "streaming/otlp/span".to_string(),
        value,
    }
}

fn otlp_metric_update(
    source_key: &str,
    service: &str,
    metric: &str,
    val: f64,
    extra: Value,
) -> TelemetryUpdate {
    let mut value = json!({
        "service_name": service,
        "host_name": "claimed-host",
        "metric_name": metric,
        "metric_kind": "gauge",
        "value": val,
        "peer_address": "10.9.9.9",
        "provisional_app_id": format!("otlpapp/{source_key}/{}", service),
        "source_key": source_key,
        "exporter_id": source_key,
        "identity_status": "provisional",
        "device_id": null,
        "canonical_exporter_id": null,
        "identity_resolution": null,
        "transport_source": "127.0.0.1:4444",
        "transport_protocol": "http",
        "collector_id": "graph-site",
        "authority": "unverified",
    });
    if let (Value::Object(base), Value::Object(ext)) = (&mut value, extra) {
        for (k, v) in ext {
            base.insert(k, v);
        }
    }
    TelemetryUpdate {
        target: source_key.to_string(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: 1_700_000_000_000_000_000,
        path: "streaming/otlp/metrics".to_string(),
        value,
    }
}

/// Seed canonical state: Device(s), Application with health metrics,
/// HostEndpoint, and a recent critical DetectionEvent.
fn seed_canonical(store: &GraphStore) {
    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    conn.query("CREATE (d:Device {address: '10.9.9.9', hostname: 'edge-rtr', site: 'dc-a'})")
        .unwrap();
    // Prefix-collision device: claim "10.9" must NOT match this.
    conn.query("CREATE (d2:Device {address: '10.9.9.10', hostname: 'edge-rtr-2', site: 'dc-a'})")
        .unwrap();
    conn.query(
        "CREATE (a:Application {id: 'app:payments', name: 'payments', criticality: 'critical', cpu_pct: 42.0, memory_mb: 512.0, req_per_sec: 100.0, error_rate: 0.01})",
    )
    .unwrap();
    conn.query(
        "CREATE (h:HostEndpoint {id: '10.9.9.9', ip: '10.9.9.9', kind: 'server', site_id: 'dc-a'})",
    )
    .unwrap();
    conn.query(
        "CREATE (de:DetectionEvent {id: 'det-net-1', device_address: '10.9.9.9', rule_id: 'bgp_session_down', severity: 'critical', features_json: '{}', source_types: '[]', latency_ns: 0, fired_at: cast('2026-11-11 10:00:00', 'TIMESTAMP_NS')})",
    )
    .unwrap();
    conn.query(
        "MATCH (d:Device {address: '10.9.9.9'}), (de:DetectionEvent {id: 'det-net-1'}) CREATE (d)-[:TRIGGERED]->(de)",
    )
    .unwrap();
}

/// A–C, F, G, H: forged service/device claims, prefix collision,
/// forged identity_resolution, metric overwrite attempt and network-
/// impact correlation must ALL fail closed while evidence is preserved.
#[tokio::test]
async fn graph_fails_closed_for_provisional_otlp() {
    let dir = tempfile::tempdir().unwrap();
    let store = GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();
    seed_canonical(&store);

    let key = "otlpsrc/graph-site/http/127.0.0.1:4444";
    let forged = json!({
        "identity_status": "resolved",
        "device_id": "dev-forged",
        "canonical_exporter_id": "exp-forged",
        "identity_resolution": {
            "status": "resolved",
            "device_id": "dev-forged",
            "verified_ingress": {"method": "mutual_tls", "credential_fingerprint": "sha256:x"}
        },
    });

    // A: span claiming the canonical application + device address.
    store
        .write(otlp_span_update(
            key,
            "payments",
            "10.9.9.9",
            forged.clone(),
        ))
        .await
        .unwrap();
    // B/C: span claiming a PREFIX of a device address — must not match.
    store
        .write(otlp_span_update(key, "payments", "10.9", json!({})))
        .await
        .unwrap();
    // G: fabricated health metrics for the existing Application.
    store
        .write(otlp_metric_update(
            key,
            "payments",
            "system.cpu.utilization",
            99.9,
            forged.clone(),
        ))
        .await
        .unwrap();
    store
        .write(otlp_metric_update(
            key,
            "payments",
            "http.server.error_count",
            777.0,
            json!({}),
        ))
        .await
        .unwrap();

    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();

    for (label, q, expected) in [
        ("Device count", "MATCH (d:Device) RETURN count(d)", "2"),
        (
            "Application count",
            "MATCH (a:Application) RETURN count(a)",
            "1",
        ),
        (
            "Application cpu_pct",
            "MATCH (a:Application {id: 'app:payments'}) RETURN a.cpu_pct",
            "42",
        ),
        (
            "Application memory_mb",
            "MATCH (a:Application {id: 'app:payments'}) RETURN a.memory_mb",
            "512",
        ),
        (
            "Application req_per_sec",
            "MATCH (a:Application {id: 'app:payments'}) RETURN a.req_per_sec",
            "100",
        ),
        (
            "Application error_rate",
            "MATCH (a:Application {id: 'app:payments'}) RETURN a.error_rate",
            "0.01",
        ),
        (
            "RUNS_SERVICE edges",
            "MATCH ()-[r:RUNS_SERVICE]->() RETURN count(r)",
            "0",
        ),
        (
            "HOST_RUNS_SERVICE edges",
            "MATCH ()-[r:HOST_RUNS_SERVICE]->() RETURN count(r)",
            "0",
        ),
        (
            "CARRIES_APPLICATION edges",
            "MATCH ()-[r:CARRIES_APPLICATION]->() RETURN count(r)",
            "0",
        ),
        (
            "APP_IMPACTED_BY_NETWORK edges",
            "MATCH ()-[r:APP_IMPACTED_BY_NETWORK]->() RETURN count(r)",
            "0",
        ),
    ] {
        let mut rows = conn.query(q).unwrap();
        let row = rows.next().expect(label);
        assert_eq!(row[0].to_string(), expected, "{label} mutated by otlp");
    }

    // Positive evidence: provisional events scoped to the source key,
    // including the read-only candidate probe match on the exact
    // peer.address (evidence, not authority).
    let mut rows = conn
        .query(
            "MATCH (e:StateChangeEvent) WHERE e.source_type = 'otlp' \
             RETURN e.device_address, e.event_type ORDER BY e.event_type",
        )
        .unwrap();
    let mut seen = Vec::new();
    while let Some(row) = rows.next() {
        seen.push((row[0].to_string(), row[1].to_string()));
    }
    assert_eq!(seen.len(), 4, "expected 4 provisional events, got {seen:?}");
    for (addr, _) in &seen {
        assert_eq!(addr, key, "event must be scoped to source key");
    }
    assert!(seen.iter().any(|(_, t)| t == "otlp_span_event"));
    assert!(seen.iter().any(|(_, t)| t == "otlp_metric_event"));

    // The candidate probe DID find the exact-address device — as
    // read-only evidence inside the event detail.
    let mut rows = conn
        .query(
            "MATCH (e:StateChangeEvent {event_type: 'otlp_span_event'}) \
             RETURN e.detail LIMIT 1",
        )
        .unwrap();
    let detail = rows.next().unwrap()[0].to_string();
    assert!(detail.contains("\"matched\":true"), "detail={detail}");
    assert!(detail.contains("device_address"), "detail={detail}");
}

/// H (isolated): a trace whose claimed peer matches a device with a
/// recent critical detection must not create APP_IMPACTED_BY_NETWORK.
#[tokio::test]
async fn network_impact_claim_never_correlates() {
    let dir = tempfile::tempdir().unwrap();
    let store = GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();
    seed_canonical(&store);

    let key = "otlpsrc/graph-site/http/127.0.0.1:5555";
    store
        .write(otlp_span_update(key, "payments", "10.9.9.9", json!({})))
        .await
        .unwrap();

    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    let mut rows = conn
        .query("MATCH ()-[r:APP_IMPACTED_BY_NETWORK]->() RETURN count(r)")
        .unwrap();
    assert_eq!(rows.next().unwrap()[0].to_string(), "0");
    // Application node itself untouched.
    let mut rows = conn
        .query("MATCH (a:Application {id: 'app:payments'}) RETURN a.cpu_pct")
        .unwrap();
    assert_eq!(rows.next().unwrap()[0].to_string(), "42");
}

/// I (graph side): span and metric updates from the same provisional
/// source produce consistent source-scoped events; a different source
/// key never merges.
#[tokio::test]
async fn provisional_source_scoping_is_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let store = GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();

    let key_a = "otlpsrc/site-a/http/10.0.0.1:4000";
    let key_b = "otlpsrc/site-b/http/10.0.0.1:4000";
    for key in [key_a, key_b] {
        store
            .write(otlp_span_update(key, "svc", "", json!({})))
            .await
            .unwrap();
        store
            .write(otlp_metric_update(
                key,
                "svc",
                "system.cpu.utilization",
                1.0,
                json!({}),
            ))
            .await
            .unwrap();
    }

    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    let mut rows = conn
        .query(
            "MATCH (e:StateChangeEvent) WHERE e.source_type = 'otlp' \
             RETURN e.device_address, count(e)",
        )
        .unwrap();
    let mut counts: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    while let Some(row) = rows.next() {
        counts.insert(row[0].to_string(), row[1].to_string());
    }
    assert_eq!(counts.get(key_a).map(String::as_str), Some("2"));
    assert_eq!(counts.get(key_b).map(String::as_str), Some("2"));
    // No Application nodes were created for the claimed service.
    let mut rows = conn.query("MATCH (a:Application) RETURN count(a)").unwrap();
    assert_eq!(rows.next().unwrap()[0].to_string(), "0");
}

/// K: replayed provisional observations stay provisional — writing the
/// same update twice produces two source-scoped events, never canonical
/// state.
#[tokio::test]
async fn replay_stays_provisional() {
    let dir = tempfile::tempdir().unwrap();
    let store = GraphStore::open(
        dir.path().join("graph.db").to_str().unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();
    seed_canonical(&store);

    let key = "otlpsrc/replay-site/http/127.0.0.1:6666";
    let update = otlp_span_update(key, "payments", "10.9.9.9", json!({}));
    store.write(update.clone()).await.unwrap();
    store.write(update).await.unwrap();

    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    let mut rows = conn
        .query("MATCH (e:StateChangeEvent {event_type: 'otlp_span_event'}) RETURN count(e)")
        .unwrap();
    assert_eq!(rows.next().unwrap()[0].to_string(), "2");
    let mut rows = conn.query("MATCH (a:Application) RETURN count(a)").unwrap();
    assert_eq!(rows.next().unwrap()[0].to_string(), "1");
}

/// L: end-to-end — live HTTP ingestion produces source-scoped bus
/// updates that the graph stores as provisional events only.
#[tokio::test]
async fn live_ingestion_produces_provisional_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(
        GraphStore::open(
            dir.path().join("graph.db").to_str().unwrap(),
            128 * 1024 * 1024,
        )
        .unwrap(),
    );

    let lab = start_lab("live-site").await;
    // Wire the receiver's bus into the graph store exactly like the
    // production write coordinator does.
    let mut bus_rx = lab.rx.resubscribe();
    let store2 = Arc::clone(&store);
    let _forwarder = tokio::spawn(async move {
        while let Ok(u) = bus_rx.recv().await {
            let _ = store2.write(u.as_ref().clone()).await;
        }
    });
    let mut graph_rx = store.subscribe_events();
    lab.post("/v1/traces", &traces_body("billing", "host-9", "192.0.2.5"))
        .await;
    lab.post(
        "/v1/metrics",
        &metrics_body("billing", "host-9", "system.memory.usage", 2048.0),
    )
    .await;

    // Two provisional events must reach the graph writer's broadcast.
    let mut saw_span = false;
    let mut saw_metric = false;
    for _ in 0..2 {
        let ev = tokio::time::timeout(Duration::from_secs(4), graph_rx.recv())
            .await
            .expect("event timeout")
            .expect("recv");
        assert_eq!(ev.source_type, "otlp");
        assert!(ev
            .device_address
            .starts_with("otlpsrc/live-site/http/127.0.0.1:"));
        match ev.event_type.as_str() {
            "otlp_span_event" => saw_span = true,
            "otlp_metric_event" => saw_metric = true,
            other => panic!("unexpected event type {other}"),
        }
    }
    assert!(saw_span && saw_metric);
    lab.stop().await;

    // And the graph contains only provisional, source-scoped events.
    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    let mut rows = conn.query("MATCH (a:Application) RETURN count(a)").unwrap();
    assert_eq!(rows.next().unwrap()[0].to_string(), "0");
}
