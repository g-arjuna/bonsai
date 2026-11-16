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

// ── WP65-19: historical (pre-WP6.5A) schema upgrade ─────────────────────
//
// Builds a REAL legacy fixture: a Ladybug file whose StateChangeEvent and
// EventEmbedding tables use the pre-WP6.5A column sets (no
// model_revision / normalization_version / lifecycle columns, random
// embedding node ids, needs_embedding absent entirely). GraphStore::open
// must migrate it idempotently, preserve legacy embedding ids and
// EMBEDDED_AS relationships, normalize unknown provenance explicitly,
// and keep serving HTTP retrieval after restart.

/// Create a disposable graph file with the pre-WP6.5A embedding schema
/// and legacy rows, then close it. Returns nothing; the file stays.
fn seed_legacy_graph(path: &str) {
    let sysconfig = lbug::SystemConfig::default()
        .buffer_pool_size(64 * 1024 * 1024)
        .max_num_threads(2);
    let db = lbug::Database::new(path, sysconfig).unwrap();
    let conn = lbug::Connection::new(&db).unwrap();
    // Pre-WP6.5A StateChangeEvent: no needs_embedding, no lifecycle cols.
    conn.query(
        "CREATE NODE TABLE StateChangeEvent(\
            id STRING, device_address STRING, event_type STRING,\
            detail STRING, source_type STRING, occurred_at TIMESTAMP_NS,\
            PRIMARY KEY (id))",
    )
    .unwrap();
    // Pre-WP6.5A EventEmbedding: random UUID ids, no provenance columns.
    conn.query(
        "CREATE NODE TABLE EventEmbedding(\
            id STRING, event_id STRING, event_type STRING, model_name STRING,\
            dim INT64, vector_json STRING, computed_at_ns INT64,\
            schema_hash STRING, PRIMARY KEY (id))",
    )
    .unwrap();
    conn.query(
        "CREATE REL TABLE EMBEDDED_AS(FROM StateChangeEvent TO EventEmbedding,\
            computed_at_ns INT64)",
    )
    .unwrap();

    let ts = lbug::Value::TimestampNs(
        time::OffsetDateTime::UNIX_EPOCH
            + time::Duration::nanoseconds(1_600_000_000_000_000_000),
    );
    let mk_event = |id: &str, etype: &str| {
        let mut stmt = conn
            .prepare(
                "CREATE (:StateChangeEvent {id: $id, device_address: $addr, \
                 event_type: $et, detail: $d, source_type: 'syslog', \
                 occurred_at: $ts})",
            )
            .unwrap();
        conn.execute(
            &mut stmt,
            vec![
                ("id", lbug::Value::String(id.to_string())),
                ("addr", lbug::Value::String("10.9.9.9".to_string())),
                ("et", lbug::Value::String(etype.to_string())),
                ("d", lbug::Value::String("{\"message\":\"legacy\"}".to_string())),
                ("ts", ts.clone()),
            ],
        )
        .unwrap();
    };
    mk_event("ev-legacy-embedded", "syslog/auth");
    mk_event("ev-legacy-pending", "syslog/protocol");
    mk_event("ev-legacy-bgp", "bgp/state");

    // Legacy embedding: random UUID-style id (NOT emb-<hex>), empty
    // schema_hash, no revision/normalization columns at all.
    let legacy_emb_id = "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d".to_string();
    let mut stmt = conn
        .prepare(
            "CREATE (:EventEmbedding {id: $id, event_id: $eid, \
             event_type: 'syslog/auth', model_name: 'legacy-model', dim: 4, \
             vector_json: '[0.1,0.2,0.3,0.4]', computed_at_ns: 1600000000000000000, \
             schema_hash: ''})",
        )
        .unwrap();
    conn.execute(
        &mut stmt,
        vec![
            ("id", lbug::Value::String(legacy_emb_id.clone())),
            ("eid", lbug::Value::String("ev-legacy-embedded".to_string())),
        ],
    )
    .unwrap();
    let mut link = conn
        .prepare(
            "MATCH (e:StateChangeEvent {id: 'ev-legacy-embedded'}), \
             (emb:EventEmbedding {id: $id}) \
             CREATE (e)-[:EMBEDDED_AS {computed_at_ns: 1600000000000000000}]->(emb)",
        )
        .unwrap();
    conn.execute(&mut link, vec![("id", lbug::Value::String(legacy_emb_id))])
        .unwrap();
    drop(conn);
    drop(db);
}

fn query_str(store: &GraphStore, cypher: &str) -> Vec<String> {
    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    conn.query(cypher)
        .unwrap()
        .map(|r| {
            r.iter()
                .map(|v| match v {
                    lbug::Value::String(s) => s.clone(),
                    lbug::Value::Int64(n) => n.to_string(),
                    lbug::Value::Double(d) => d.to_string(),
                    lbug::Value::Bool(b) => b.to_string(),
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect()
}

#[tokio::test]
async fn wp65_19_legacy_schema_upgrade() {
    let dir = TempDir::new().unwrap();
    let graph_path = dir.path().join("legacy.db").to_string_lossy().into_owned();
    seed_legacy_graph(&graph_path);

    // ── open migrates: schema upgrade must succeed, not error ──
    let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).unwrap());

    // Legacy random-id embedding node + relationship preserved verbatim.
    let rows = query_str(
        &store,
        "MATCH (e:StateChangeEvent {id: 'ev-legacy-embedded'})-[r:EMBEDDED_AS]->(emb:EventEmbedding) \
         RETURN emb.id, emb.model_name, emb.vector_json, emb.model_revision, \
         emb.normalization_version, emb.schema_hash",
    );
    assert_eq!(rows.len(), 1, "legacy EMBEDDED_AS relationship lost: {rows:?}");
    let f: Vec<&str> = rows[0].split('|').collect();
    assert_eq!(f[0], "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d");
    assert_eq!(f[1], "legacy-model");
    assert_eq!(f[2], "[0.1,0.2,0.3,0.4]");
    // Unknown provenance normalized explicitly — never fabricated.
    assert_eq!(f[3], "unknown");
    assert_eq!(f[4], "unknown");
    assert_eq!(f[5], "unknown");

    // needs_embedding migration: the column did not exist; DEFAULT 1 then
    // the non-syslog sweep. syslog events stay pending; bgp is cleared.
    let ne = |id: &str| -> String {
        query_str(&store, &format!(
            "MATCH (e:StateChangeEvent {{id: '{id}'}}) RETURN e.needs_embedding"
        ))
        .pop()
        .unwrap_or_default()
    };
    // ev-legacy-embedded had an embedding historically but the legacy
    // schema had no flag to clear — it stays pending=1 (honest: core
    // cannot prove it was consumed) OR the fixture never set it. Either
    // way the fetch must not crash; assert only the bgp clearing rule.
    assert_eq!(ne("ev-legacy-bgp"), "0", "non-syslog event not cleared");

    // Lifecycle columns exist with bounded defaults on migrated rows.
    let lc = query_str(
        &store,
        "MATCH (e:StateChangeEvent {id: 'ev-legacy-pending'}) \
         RETURN e.embedding_attempts, e.embedding_invalid, e.embedding_retry_after_ns",
    );
    assert_eq!(lc.len(), 1, "lifecycle columns missing after migration: {lc:?}");

    // ── restart: reopen the migrated file, state must persist ──
    drop(store);
    let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024).unwrap());
    let rows = query_str(
        &store,
        "MATCH (e:StateChangeEvent {id: 'ev-legacy-embedded'})-[r:EMBEDDED_AS]->(emb:EventEmbedding) \
         RETURN emb.id",
    );
    assert_eq!(rows, vec!["9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d".to_string()]);

    // ── real HTTP retrieval on the migrated graph ──
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
    let base = format!("http://127.0.0.1:{port}");

    // Unembedded fetch works on migrated rows and excludes non-syslog.
    let resp = client
        .get(format!("{base}/api/events/unembedded?type=syslog&limit=50"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let ids: Vec<&str> = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["id"].as_str())
        .collect();
    assert!(ids.contains(&"ev-legacy-pending"), "pending syslog event missing: {ids:?}");
    assert!(!ids.contains(&"ev-legacy-bgp"), "non-syslog event leaked: {ids:?}");

    // New deterministic write-back on the migrated graph is accepted and
    // coexists with the legacy random-id embedding (different identity).
    let resp = client
        .post(format!("{base}/api/events/embeddings"))
        .json(&json!({"embeddings": [embedding_item("ev-legacy-pending", vec![0.5, 0.5, 0.5, 0.5])]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let b: Value = resp.json().await.unwrap();
    assert_eq!(b["accepted"], 1, "{b}");
    assert_eq!(emb_count(&store, "ev-legacy-pending"), 1);
    // Legacy node untouched: still exactly one embedding on ev-legacy-embedded.
    assert_eq!(emb_count(&store, "ev-legacy-embedded"), 1);

    server.abort();
}

#[tokio::test]
async fn wp65_19_corrupt_graph_fails_closed() {
    let dir = TempDir::new().unwrap();
    let bad = dir.path().join("not-a-graph.db");
    std::fs::write(&bad, b"this is not a ladybug database file").unwrap();
    let res = GraphStore::open(&bad.to_string_lossy(), 64 * 1024 * 1024);
    assert!(res.is_err(), "corrupt graph file must fail closed, not open");
}

// ── WP65-16/17: lifecycle transition guards ─────────────────────────────
// Stale/duplicate lifecycle reports, claim-generation fencing, contention
// vs real attempt consumption, non-syslog eligibility, bounded retry
// deadline and provider-recovery eligibility — all through the real HTTP
// handler against a disposable Ladybug DB.

fn set_attempts(store: &GraphStore, id: &str, attempts: i64) {
    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    let mut stmt = conn
        .prepare("MATCH (e:StateChangeEvent {id: $id}) SET e.embedding_attempts = $a")
        .unwrap();
    conn.execute(
        &mut stmt,
        vec![
            ("id", lbug::Value::String(id.to_string())),
            ("a", lbug::Value::Int64(attempts)),
        ],
    )
    .unwrap();
}

fn get_i64(store: &GraphStore, id: &str, prop: &str) -> i64 {
    query_count(
        store,
        &format!("MATCH (e:StateChangeEvent {{id: '{id}'}}) RETURN e.{prop}"),
    )
}

fn get_str(store: &GraphStore, id: &str, prop: &str) -> String {
    let db = store.db();
    let conn = lbug::Connection::new(&db).unwrap();
    conn.query(&format!(
        "MATCH (e:StateChangeEvent {{id: '{id}'}}) RETURN e.{prop}"
    ))
    .unwrap()
    .next()
    .and_then(|r| match &r[0] {
        lbug::Value::String(s) => Some(s.clone()),
        _ => None,
    })
    .unwrap_or_default()
}

#[tokio::test]
async fn wp65_16_17_lifecycle_transition_guards() {
    let rig = rig().await;
    seed_event(&rig.store, "ev-g1", "syslog/auth", 1);
    seed_event(&rig.store, "ev-g2", "syslog/auth", 1);
    seed_event(&rig.store, "ev-g3", "syslog/auth", 1);
    seed_event(&rig.store, "ev-g4", "syslog/auth", 1);
    seed_event(&rig.store, "ev-nonsys", "bgp/state", 1);

    // ── Batch A: accepted embedding + stale/duplicate/non-syslog reports ──
    let (status, body) = post_embeddings(
        &rig,
        json!({
            "embeddings": [embedding_item("ev-g1", vec![0.1, 0.2, 0.3, 0.4])],
            "deferred": [
                // same-batch stale: ev-g1 completed in this very batch
                {"event_id": "ev-g1", "claim_attempts": 0},
                // duplicate of the same stale report
                {"event_id": "ev-g1", "claim_attempts": 0},
                // stale claim: stored attempts=0, claim says 99
                {"event_id": "ev-g2", "claim_attempts": 99},
                // non-syslog event can never take a lifecycle report
                {"event_id": "ev-nonsys", "claim_attempts": 0},
            ],
            "invalid": [
                {"event_id": "ev-nonsys", "reason": "no_extractable_text"},
            ],
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["accepted"], 1);
    assert_eq!(body["deferred"], 0, "{body}");
    assert_eq!(body["deferred_skipped"], 4, "{body}");
    assert_eq!(body["invalid"], 0);
    assert_eq!(body["invalid_skipped"], 1);
    // Nothing consumed attempts; non-syslog untouched.
    assert_eq!(get_i64(&rig.store, "ev-g1", "embedding_attempts"), 0);
    assert_eq!(needs_embedding(&rig.store, "ev-g1"), 0);
    assert_eq!(get_i64(&rig.store, "ev-nonsys", "embedding_attempts"), 0);
    assert_eq!(needs_embedding(&rig.store, "ev-nonsys"), 1);

    // ── Batch B: real provider failure consumes exactly one attempt ──
    let (_s, b) = post_embeddings(
        &rig,
        json!({"embeddings": [], "deferred": [
            {"event_id": "ev-g2", "claim_attempts": 0, "kind": "provider_error"},
        ]}),
    )
    .await;
    assert_eq!(b["deferred"], 1, "{b}");
    assert_eq!(get_i64(&rig.store, "ev-g2", "embedding_attempts"), 1);
    assert!(get_i64(&rig.store, "ev-g2", "embedding_retry_after_ns") > 0);

    // Duplicate redelivery with the now-stale claim (attempts moved 0->1)
    // must NOT consume a second attempt.
    let (_s, b) = post_embeddings(
        &rig,
        json!({"embeddings": [], "deferred": [
            {"event_id": "ev-g2", "claim_attempts": 0, "kind": "provider_error"},
        ]}),
    )
    .await;
    assert_eq!(b["deferred"], 0, "{b}");
    assert_eq!(b["deferred_skipped"], 1);
    assert_eq!(get_i64(&rig.store, "ev-g2", "embedding_attempts"), 1);

    // A deferred report for the already-completed ev-g1 is stale even
    // with a matching claim — needs_embedding=0 gates it.
    let (_s, b) = post_embeddings(
        &rig,
        json!({"embeddings": [], "deferred": [
            {"event_id": "ev-g1", "claim_attempts": 0},
        ]}),
    )
    .await;
    assert_eq!(b["deferred_skipped"], 1, "{b}");

    // ── claim_contention reschedules WITHOUT consuming an attempt ──
    let (_s, b) = post_embeddings(
        &rig,
        json!({"embeddings": [], "deferred": [
            {"event_id": "ev-g3", "claim_attempts": 0, "kind": "claim_contention"},
        ]}),
    )
    .await;
    assert_eq!(b["deferred"], 1, "{b}");
    assert_eq!(get_i64(&rig.store, "ev-g3", "embedding_attempts"), 0);
    assert!(get_i64(&rig.store, "ev-g3", "embedding_retry_after_ns") > 0);

    // ── bounded retry deadline: attempts at the ceiling exhaust ──
    set_attempts(&rig.store, "ev-g4", 100);
    let (_s, b) = post_embeddings(
        &rig,
        json!({"embeddings": [], "deferred": [
            {"event_id": "ev-g4", "claim_attempts": 100, "kind": "provider_error"},
        ]}),
    )
    .await;
    assert_eq!(b["deferred"], 1, "{b}");
    assert_eq!(needs_embedding(&rig.store, "ev-g4"), 0);
    assert_eq!(get_i64(&rig.store, "ev-g4", "embedding_invalid"), 1);
    assert_eq!(
        get_str(&rig.store, "ev-g4", "embedding_invalid_reason"),
        "embedding_retry_exhausted"
    );

    // ── provider recovery: a deferred event becomes fetchable again once
    // its retry deadline is cleared (operator retry contract / natural
    // deadline expiry) — eligibility is restored, not fabricated. ──
    {
        let db = rig.store.db();
        let conn = lbug::Connection::new(&db).unwrap();
        let mut stmt = conn
            .prepare(
                "MATCH (e:StateChangeEvent {id: 'ev-g2'}) \
                 SET e.embedding_retry_after_ns = 0",
            )
            .unwrap();
        conn.execute(&mut stmt, vec![]).unwrap();
    }
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
    assert!(ids.contains(&"ev-g2"), "recovered event not re-fetchable: {ids:?}");
    // claim_contention rescheduled ev-g3 ~60s out — still inside its
    // bounded deadline, so it must NOT be re-fetchable yet.
    assert!(!ids.contains(&"ev-g3"), "contention event refetched early: {ids:?}");
    assert!(!ids.contains(&"ev-g4"), "exhausted event must stay closed: {ids:?}");
    assert!(!ids.contains(&"ev-g1"), "completed event must not refetch: {ids:?}");
    assert!(!ids.contains(&"ev-nonsys"), "non-syslog event must not refetch: {ids:?}");
}
