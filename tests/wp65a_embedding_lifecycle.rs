//! WP6.5A — embedding write-back runtime acceptance against a real
//! disposable Ladybug database and the real HTTP handler.
//!
//! Covers the D1 mandatory inventory: fresh schema, deterministic
//! identity, immutable replay, conflicting replay, per-item
//! transactions, source-event eligibility, vector validation,
//! relationship cardinality, partial batches, deferred/invalid
//! lifecycle, restart persistence and model-space similarity.

use std::sync::Arc;

use bonsai::graph::GraphStore;
use bonsai::http_server::{router_from_state, AppState};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::TcpListener;

struct Rig {
    _dir: TempDir,
    store: Arc<GraphStore>,
    client: reqwest::Client,
    base: String,
    _server: tokio::task::JoinHandle<()>,
}

fn env_once() {
    // SAFETY: test-only process env, set before any server task starts.
    unsafe {
        std::env::set_var("BONSAI_ALLOW_INSECURE_NO_AUTH", "1");
        std::env::set_var("BONSAI_VAULT_PASSPHRASE", "wp65a-embedding-tests");
    }
}

async fn rig() -> Rig {
    env_once();
    let dir = TempDir::new().unwrap();
    let graph_path = dir.path().join("graph.db").to_string_lossy().into_owned();
    let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).unwrap());
    let state = AppState::new_minimal_with_runtime_dir(
        Arc::clone(&store),
        dir.path().to_string_lossy().as_ref(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = router_from_state(state);
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Rig {
        _dir: dir,
        store,
        client: reqwest::Client::new(),
        base: format!("http://127.0.0.1:{port}"),
        _server: server,
    }
}

fn seed_event(store: &GraphStore, id: &str, event_type: &str, needs_embedding: i64) {
    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    let mut stmt = conn
        .prepare(
            "CREATE (e:StateChangeEvent {id: $id, device_address: $addr, \
             event_type: $etype, detail: $detail, source_type: 'syslog', \
             occurred_at: $ts, needs_embedding: $ne})",
        )
        .unwrap();
    conn.execute(
        &mut stmt,
        vec![
            ("id", lbug::Value::String(id.to_string())),
            ("addr", lbug::Value::String("10.0.0.1".to_string())),
            ("etype", lbug::Value::String(event_type.to_string())),
            ("detail", lbug::Value::String("{\"message\":\"test\"}".to_string())),
            (
                "ts",
                lbug::Value::TimestampNs(
                    time::OffsetDateTime::UNIX_EPOCH
                        + time::Duration::nanoseconds(1_700_000_000_000_000_000),
                ),
            ),
            ("ne", lbug::Value::Int64(needs_embedding)),
        ],
    )
    .unwrap();
}

fn embedding_item(event_id: &str, vector: Vec<f64>) -> Value {
    json!({
        "event_id": event_id,
        "vector": vector,
        "model_name": "test-model",
        "model_revision": "rev-a",
        "normalization_version": "syslog-tpl-v1",
        "schema_hash": "event-embedding-v1",
        "dim": vector.len() as i64,
        "computed_at_ns": 1_700_000_000_000_000_000i64,
    })
}

async fn post_embeddings(rig: &Rig, body: Value) -> (reqwest::StatusCode, Value) {
    let resp = rig
        .client
        .post(format!("{}/api/events/embeddings", rig.base))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(json!({"raw": text})))
}

fn query_count(store: &GraphStore, cypher: &str) -> i64 {
    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    conn.query(cypher)
        .unwrap()
        .next()
        .map(|r| match &r[0] {
            lbug::Value::Int64(n) => *n,
            _ => 0,
        })
        .unwrap_or(0)
}

fn emb_count(store: &GraphStore, event_id: &str) -> i64 {
    query_count(
        store,
        &format!(
            "MATCH (e:StateChangeEvent {{id: '{event_id}'}})-[:EMBEDDED_AS]->(emb:EventEmbedding) \
             RETURN count(emb)"
        ),
    )
}

fn needs_embedding(store: &GraphStore, event_id: &str) -> i64 {
    query_count(
        store,
        &format!(
            "MATCH (e:StateChangeEvent {{id: '{event_id}'}}) RETURN e.needs_embedding"
        ),
    )
}

#[tokio::test]
async fn embedding_writeback_full_lifecycle() {
    let rig = rig().await;
    seed_event(&rig.store, "ev1", "syslog/auth", 1);
    seed_event(&rig.store, "ev2", "syslog/protocol", 1);
    seed_event(&rig.store, "ev-non", "bgp/state", 0);

    let vec = vec![0.1, 0.2, 0.3, 0.4];
    let (status, body) = post_embeddings(
        &rig,
        json!({"embeddings": [
            embedding_item("ev1", vec.clone()),
            embedding_item("ev2", vec![0.5, 0.6, 0.7, 0.8]),
            // non-syslog event must be rejected as ineligible
            embedding_item("ev-non", vec.clone()),
            // missing source event
            embedding_item("ev-missing", vec.clone()),
        ]}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["submitted"], 4);
    assert_eq!(body["accepted"], 2);
    assert_eq!(body["already_existing"], 0);
    assert_eq!(body["persisted"], 2);
    assert_eq!(body["rejected"].as_array().unwrap().len(), 2);
    assert_eq!(body["items"].as_array().unwrap().len(), 4);
    assert_eq!(needs_embedding(&rig.store, "ev1"), 0);
    assert_eq!(emb_count(&rig.store, "ev1"), 1);
    assert_eq!(emb_count(&rig.store, "ev-non"), 0);

    // ── WP65-09: timestamp-independent immutable replay ──
    let mut replay = embedding_item("ev1", vec.clone());
    replay["computed_at_ns"] = json!(1_800_000_000_000_000_000i64);
    let (status, body) = post_embeddings(&rig, json!({"embeddings": [replay]})).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["already_existing"], 1);
    assert_eq!(body["accepted"], 0);
    assert_eq!(emb_count(&rig.store, "ev1"), 1);
    // provenance preserved: computed_at_ns unchanged on node and rel
    let db = rig.store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    let row = conn
        .query(
            "MATCH (e:StateChangeEvent {id: 'ev1'})-[r:EMBEDDED_AS]->(emb:EventEmbedding) \
             RETURN emb.computed_at_ns, r.computed_at_ns",
        )
        .unwrap()
        .next()
        .unwrap();
    let n = |v: &lbug::Value| match v {
        lbug::Value::Int64(n) => *n,
        _ => 0,
    };
    assert_eq!(n(&row[0]), 1_700_000_000_000_000_000);
    assert_eq!(n(&row[1]), 1_700_000_000_000_000_000);

    // ── conflicting replay: same identity, different vector → rejected ──
    let (status, body) = post_embeddings(
        &rig,
        json!({"embeddings": [embedding_item("ev1", vec![9.9, 9.9, 9.9, 9.9])]}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["rejected"].as_array().unwrap().len(), 1);
    assert!(body["rejected"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("conflict"));
    assert_eq!(emb_count(&rig.store, "ev1"), 1);

    // conflicting dimension under same model identity → rejected
    let (status, body) = post_embeddings(
        &rig,
        json!({"embeddings": [embedding_item("ev1", vec![0.1, 0.2])]}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["rejected"].as_array().unwrap().len(), 1);

    // ── WP65-10: invalid vectors rejected, pending state preserved ──
    seed_event(&rig.store, "ev-bad", "syslog/auth", 1);
    for bad in [
        vec![],
        vec![0.0, 0.0, 0.0, 0.0],
        // norm overflows to non-finite — JSON cannot carry NaN/inf
        vec![1e308, 1e308, 1e308, 1e308],
        vec![0.1, 0.2, 0.3], // declared dim 4 != vector len 3
    ] {
        let mut item = embedding_item("ev-bad", bad);
        item["dim"] = json!(4); // declared dim != vector len for last case
        let (_s, b) = post_embeddings(&rig, json!({"embeddings": [item]})).await;
        assert_eq!(b["rejected"].as_array().unwrap().len(), 1, "{b}");
    }
    assert_eq!(needs_embedding(&rig.store, "ev-bad"), 1);
    assert_eq!(emb_count(&rig.store, "ev-bad"), 0);

    // missing computed_at_ns provenance
    let mut no_ts = embedding_item("ev-bad", vec![0.1, 0.2, 0.3, 0.4]);
    no_ts["computed_at_ns"] = json!(0);
    let (_s, b) = post_embeddings(&rig, json!({"embeddings": [no_ts]})).await;
    assert_eq!(b["rejected"].as_array().unwrap().len(), 1);

    // ── new normalization identity -> distinct embedding node ──
    let mut new_norm = embedding_item("ev1", vec![0.7, 0.7, 0.7, 0.7]);
    new_norm["normalization_version"] = json!("syslog-tpl-v2");
    let (_s, b) = post_embeddings(&rig, json!({"embeddings": [new_norm]})).await;
    assert_eq!(b["accepted"], 1);
    assert_eq!(emb_count(&rig.store, "ev1"), 2);

    // ── WP65-14: deferred retry deadline + invalid lifecycle ──
    seed_event(&rig.store, "ev-def", "syslog/auth", 1);
    seed_event(&rig.store, "ev-inv", "syslog/auth", 1);
    let (_s, b) = post_embeddings(
        &rig,
        json!({
            "embeddings": [],
            "deferred": [{"event_id": "ev-def"}],
            "invalid": [{"event_id": "ev-inv", "reason": "no_extractable_text"}],
        }),
    )
    .await;
    assert_eq!(b["deferred"], 1);
    assert_eq!(b["invalid"], 1);
    assert_eq!(needs_embedding(&rig.store, "ev-inv"), 0);
    let db = rig.store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    let inv = conn
        .query(
            "MATCH (e:StateChangeEvent {id: 'ev-inv'}) \
             RETURN e.embedding_invalid, e.embedding_invalid_reason",
        )
        .unwrap()
        .next()
        .unwrap();
    assert!(matches!(inv[0], lbug::Value::Int64(1)));
    assert!(matches!(&inv[1], lbug::Value::String(s) if s == "no_extractable_text"));

    // deferred event still pending but not immediately re-fetchable:
    // fetch must exclude it until the retry deadline.
    let resp = rig
        .client
        .get(format!("{}/api/events/unembedded?type=syslog&limit=50", rig.base))
        .send()
        .await
        .unwrap();
    let fetched: Value = resp.json().await.unwrap();
    let ids: Vec<&str> = fetched["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["id"].as_str())
        .collect();
    assert!(!ids.contains(&"ev-def"), "deferred event must not be re-fetched: {ids:?}");
    assert!(!ids.contains(&"ev-inv"));
    assert!(needs_embedding(&rig.store, "ev-def") == 1);

    // ── batch cap ──
    let big: Vec<Value> = (0..513)
        .map(|i| embedding_item(&format!("ev{i}"), vec![0.1, 0.2, 0.3, 0.4]))
        .collect();
    let (status, _b) = post_embeddings(&rig, json!({"embeddings": big})).await;
    assert_eq!(status, reqwest::StatusCode::PAYLOAD_TOO_LARGE);

    // ── similarity: same model space only ──
    let resp = rig
        .client
        .get(format!(
            "{}/api/ml/similar-events?event_id=ev1&limit=10",
            rig.base
        ))
        .send()
        .await
        .unwrap();
    let sim: Value = resp.json().await.unwrap();
    // ev1 has two embeddings (two normalization spaces); the most recent
    // deterministic pick compares only within that space — ev2 shares
    // the v1 space and must appear, nothing from v2 leaks in.
    assert!(sim["similar"].as_array().unwrap().iter().any(|s| s["event_id"] == "ev2"));
    let _ = conn;
}

#[tokio::test]
async fn embedding_writeback_restart_preserves() {
    let dir = TempDir::new().unwrap();
    let graph_path = dir.path().join("graph.db").to_string_lossy().into_owned();
    {
        let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).unwrap());
        seed_event(&store, "ev-r", "syslog/auth", 1);
        env_once();
        let state = AppState::new_minimal_with_runtime_dir(
            Arc::clone(&store),
            dir.path().to_string_lossy().as_ref(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let router = router_from_state(state);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/api/events/embeddings"))
            .json(&json!({"embeddings": [embedding_item("ev-r", vec![0.1, 0.2, 0.3, 0.4])]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        server.abort();
    }
    // Reopen the same graph file — embedding + relationship persist.
    let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).unwrap());
    assert_eq!(emb_count(&store, "ev-r"), 1);
    assert_eq!(needs_embedding(&store, "ev-r"), 0);
}
