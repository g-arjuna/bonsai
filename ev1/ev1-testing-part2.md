# EV1 Ubuntu Testing Guide — Part 2: ML Pipeline, GNN, Embeddings

> **Prerequisites**: Part 1 complete. Bonsai running. Lab devices seeded. Python ML deps installed.
> **Key ports**: Bonsai API `:3000`, Sidecar health `:9200`, Sidecar Prometheus `:9201`

---

## Phase 9 — Python Sidecar Startup

### 9.1 Start the sidecar

```bash
# Set environment
export BONSAI_API_URL="http://localhost:3000"
export BONSAI_CORE_ADDR="localhost:50051"
export BONSAI_LOCAL_ADDR="localhost:50052"

# Start sidecar (non-blocking startup — all background threads launch immediately)
cd ~/bonsai
python python/collector_engine.py &
SIDECAR_PID=$!

sleep 8

# Verify health
curl -s http://localhost:9200/health | python3 -m json.tool
```

**Expected full health response:**
```json
{
  "status": "ok",
  "uptime_secs": 8,
  "rules_loaded": 47,
  "rules_enabled": 47,
  "rules_shadow": 0,
  "last_detection_at_ns": null,
  "detections_today": 0,
  "queue_depth": 0,
  "queue_drops_today": 0,
  "model_loaded": false,
  "model_id": null,
  "last_inference_at_ns": null,
  "snapshot_buffer_size": 0,
  "snapshot_buffer_stale": false,
  "embedding_pending_syslog": 0,
  "embedding_pending_config": 0,
  "job_engine_running": true,
  "next_job": {"id": "syslog_embedding", "in_seconds": 60},
  "memory_usage_mb": 120,
  "connected_to_core": true,
  "connected_to_local": true
}
```

- [ ] `job_engine_running: true`
- [ ] `connected_to_core: true`
- [ ] `rules_loaded >= 14` (14 rule modules)
- [ ] `memory_usage_mb < 512` at startup

### 9.2 Verify sidecar registered with Bonsai core

```bash
curl -s http://localhost:3000/api/sidecar/status | python3 -m json.tool
```
- [ ] `registered: true`
- [ ] Sidecar visible in **Bonsai UI → Collectors → Sidecars** tab

### 9.3 Verify Prometheus metrics endpoint

```bash
curl -s http://localhost:9201/metrics | grep bonsai_ml
```
- [ ] `bonsai_ml_job_runs_total` metric present
- [ ] `bonsai_ml_pending_embeddings` metric present
- [ ] `bonsai_sidecar_memory_mb` gauge present

### 9.4 Test graceful shutdown (then restart)

```bash
kill -SIGTERM $SIDECAR_PID
sleep 12
# Expected in sidecar logs: "received SIGTERM, shutting down gracefully"
# Expected: forward queue drains, snapshot buffer saved

# Restart
python python/collector_engine.py &
SIDECAR_PID=$!
sleep 5
curl -s http://localhost:9200/health | python3 -m json.tool
```
- [ ] Sidecar restarts cleanly, re-registers with core

---

## Phase 10 — ML Job Engine

### 10.1 Verify default job schedules

```bash
curl -s http://localhost:3000/api/ml/schedules | python3 -m json.tool
```

**Expected schedules (7 defaults):**
```
anomaly_export_daily      - cron(hour=2)
remediation_export_weekly - cron(day_of_week=0, hour=2)
gnn_inference             - interval(minutes=5)
syslog_embedding          - interval(seconds=60)
graph_snapshot            - interval(hours=4)
detection_clustering      - cron(day_of_week=0, hour=3)
config_embedding          - interval(hours=6)
```
- [ ] All 7 schedules present and `enabled: true`
- [ ] Schedules visible in BonPy UI at `http://localhost:3000/bonpy/jobs`

### 10.2 Manually trigger a job

```bash
# Trigger graph snapshot job (captures live graph state → STGNN buffer)
curl -s -X POST http://localhost:3000/api/ml/jobs \
  -H 'Content-Type: application/json' \
  -d '{"job_id":"graph_snapshot","trigger":"manual"}' | python3 -m json.tool
```

Wait 10 seconds, then:
```bash
curl -s "http://localhost:3000/api/ml/jobs?limit=5" | python3 -m json.tool
```
- [ ] Job run record appears with `status: "succeeded"` or `"running"`
- [ ] `started_at_ns` populated

### 10.3 Verify SSE ML event stream

In a separate terminal:
```bash
curl -s -N http://localhost:3000/api/ml/events/stream
```
- [ ] SSE stream opens (no immediate close)
- [ ] When jobs run: `event: JobStarted`, `event: JobCompleted` events appear
- [ ] When syslog embedding runs: `event: EmbeddingBatchCompleted` appears

### 10.4 Test job cancel

```bash
# Start a long-running job
JOB_ID=$(curl -s -X POST http://localhost:3000/api/ml/jobs \
  -H 'Content-Type: application/json' \
  -d '{"job_id":"anomaly_export_daily","trigger":"manual"}' | python3 -m json.tool | python3 -c "import sys,json; print(json.load(sys.stdin)['id'])")

# Cancel it
curl -s -X POST "http://localhost:3000/api/ml/jobs/${JOB_ID}/cancel"

# Verify
curl -s "http://localhost:3000/api/ml/jobs/${JOB_ID}" | python3 -m json.tool
```
- [ ] `status: "cancelled"` on job record

---

## Phase 11 — Parquet Export Pipeline

### 11.1 Enable archive (prerequisite for export)

```bash
curl -s -X PATCH http://localhost:3000/api/settings/archive \
  -H 'Content-Type: application/json' \
  -d '{"enabled": true, "retention_days": 30}'
```

Run a few detections first (from Phase 7) so the archive has data to export.

### 11.2 Trigger incremental export

```bash
python3 -m bonsai_ml.export_job --type anomaly --incremental
```

**Expected output:**
```
[INFO] Creating catalog record...
[INFO] Fetching events from graph (since: epoch)...
[INFO] Exported N rows to runtime/parquet/anomaly/2026-05-25T02:00:00Z_v1_Nrows.parquet
[INFO] Running readiness check... PASS/FAIL
[INFO] PATCH catalog record: status=completed, rows=N
```

- [ ] Parquet file exists at `runtime/parquet/anomaly/latest`
- [ ] Catalog record in API:
  ```bash
  curl -s http://localhost:3000/api/ml/exports | python3 -m json.tool
  ```
  - [ ] Record present with `status: "completed"`
  - [ ] `row_count > 0`
  - [ ] `schema_hash` populated

### 11.3 Validate the Parquet file

```bash
python3 -c "
import pyarrow.parquet as pq
import glob, os

files = glob.glob('runtime/parquet/anomaly/*.parquet')
if not files:
    print('ERROR: No parquet file found')
    exit(1)

latest = max(files, key=os.path.getmtime)
print(f'File: {latest}')
tbl = pq.read_table(latest)
print(f'Rows: {tbl.num_rows}')
print(f'Columns: {tbl.schema.names}')
print(f'Label distribution:')
import pandas as pd
df = tbl.to_pandas()
print(df['label'].value_counts())
"
```
- [ ] File readable by pyarrow
- [ ] Column names match expected feature schema
- [ ] Label column present with binary values (0/1)

### 11.4 Quality dashboard

```bash
curl -s http://localhost:3000/api/ml/exports/quality | python3 -m json.tool
```
- [ ] `quality_passed` field present (true/false)
- [ ] `class_balance_pct` between 5-50%
- [ ] `label_drift_score` = null on first export (no previous to compare)

**BonPy UI Exports page:**
```
open http://localhost:3000/bonpy/exports
```
- [ ] Export catalog table shows the run
- [ ] Quality badge shows PASS or FAIL
- [ ] "Export Now" button triggers a new export and shows SSE progress

---

## Phase 12 — STGNN Training

> **Minimum data requirement**: Need ≥30 graph snapshots and ≥50 anomaly events in archive.
> **Fast path for testing**: use the chaos harness to inject faults.

### 12.1 Inject chaos to build training data

```bash
# Run chaos harness (requires lab)
cd tests/chaos_harness
python3 run.py --duration 600 --topology signal-test
cd ../..

# Or manually inject via API to simulate fault archive
python3 -c "
import requests, time, random

base = 'http://localhost:3000'
devices = ['172.20.20.2', '172.20.20.3', '172.20.20.4']

for i in range(60):
    # Simulate periodic detection
    device = random.choice(devices)
    requests.post(f'{base}/api/detections', json={
        'rule_id': 'bgp_session_flap',
        'device_address': device,
        'severity': 'high',
        'reason': f'BGP session flap #{i}',
        'is_fault': i % 5 == 0  # 20% fault rate
    })
    time.sleep(1)
print('Done injecting test data')
"
```

### 12.2 Check training readiness

```bash
python3 -c "
from bonsai_ml.parquet_validator import validate_parquet
from bonsai_sdk.training_readiness import ReadinessCheck
import glob, os

files = glob.glob('runtime/parquet/anomaly/*.parquet')
if not files:
    print('No parquet file — run export first')
    exit(1)
latest = max(files, key=os.path.getmtime)
result = validate_parquet(latest, 'DEVICE_V2_SCHEMA')
print(f'Valid: {result.valid}')
print(f'Rows: {result.row_count}')
print(f'Issues: {result.issues}')
"
```
- [ ] `valid: True`
- [ ] Row count sufficient for training (≥50 anomaly, ≥200 normal)

### 12.3 Run STGNN training

```bash
python python/train_stgnn.py --model-type stgnn --register
```

**Expected output (abbreviated):**
```
[INFO] Phase 1: NCT pre-training
[INFO]   Snapshots available: 30+
[INFO]   Epoch 1/50: NCT loss = 0.842
[INFO]   ...
[INFO]   NCT pre-training complete. Saved: models/nct_pretrain.pt
[INFO] Phase 2: Supervised fine-tuning
[INFO]   Epoch 1/50: loss=0.621, val_auc=0.531
[INFO]   ...
[INFO]   Final: val_auc=0.702, val_f1=0.448
[INFO] Quality gate: AUC=0.702 >= 0.65 ✓, F1=0.448 >= 0.40 ✓
[INFO] Registering model via POST /api/ml/models...
[INFO] Model registered: stgnn_v1_20260525_123456
[INFO] Saved: models/stgnn_v1_20260525_123456.pt
```

- [ ] NCT pretrain completes without error
- [ ] Final val_AUC ≥ 0.65
- [ ] Final val_F1 ≥ 0.40
- [ ] Model registered in API:
  ```bash
  curl -s http://localhost:3000/api/ml/models | python3 -m json.tool
  ```
  - [ ] Model record present with `val_auc`, `val_f1`, `feature_schema_hash`

### 12.4 Activate the model

```bash
MODEL_ID=$(curl -s http://localhost:3000/api/ml/models | python3 -m json.tool | \
  python3 -c "import sys,json; models=json.load(sys.stdin); print(models[0]['id'])")

curl -s -X POST "http://localhost:3000/api/ml/models/${MODEL_ID}/activate" | python3 -m json.tool
```
- [ ] `is_active: true` on the model
- [ ] Active model visible in BonPy UI at `/bonpy/models`

### 12.5 Verify conformal calibration

```bash
ls -la models/conformal_qhat_alpha0.1.json
python3 -c "
import json
with open('models/conformal_qhat_alpha0.1.json') as f:
    d = json.load(f)
print(f'q_hat: {d[\"q_hat\"]}')
print(f'coverage: {d[\"coverage\"]}')
print(f'efficiency: {d[\"efficiency\"]}')
"
```
- [ ] `q_hat` between 0.1 and 0.9 (reasonable conformal threshold)
- [ ] `coverage` ≥ 0.90 (90% target)
- [ ] `efficiency` > 0 (some singleton prediction sets)

### 12.6 Model card check

```bash
cat python/bonsai_ml/model_cards/stgnn_v1.md
```
- [ ] Architecture section present (STGNN, GATv2-GRU, 8-snapshot)
- [ ] Validation metrics populated
- [ ] Threshold guidance (0.5 investigation trigger, 0.7 production)
- [ ] Known limitations section

---

## Phase 13 — STGNN Live Inference

### 13.1 Verify snapshot buffer populating

The `graph_snapshot` job runs every 4h. Trigger manually:
```bash
curl -s -X POST http://localhost:3000/api/ml/jobs \
  -H 'Content-Type: application/json' \
  -d '{"job_id":"graph_snapshot","trigger":"manual"}'

sleep 10
curl -s http://localhost:9200/health | python3 -c "
import sys,json
h=json.load(sys.stdin)
print(f'Snapshot buffer size: {h[\"snapshot_buffer_size\"]} / 8')
print(f'Stale: {h[\"snapshot_buffer_stale\"]}')
"
```
- [ ] `snapshot_buffer_size >= 1` after first manual trigger
- [ ] `snapshot_buffer_stale: false`

### 13.2 Trigger GNN inference

```bash
curl -s -X POST http://localhost:3000/api/ml/jobs \
  -H 'Content-Type: application/json' \
  -d '{"job_id":"gnn_inference","trigger":"manual"}'

sleep 30

# Check results
curl -s "http://localhost:3000/api/gnn/inference-results?limit=10" | python3 -m json.tool
```
- [ ] Inference results present with `anomaly_score`, `threshold`, `is_anomalous`
- [ ] `uncertainty_margin` field populated
- [ ] `top_contributing_device_1` may be populated if anomalous

**Verify in graph:**
```cypher
MATCH (d:Device)-[:GNN_SCORED]->(r:GnnInferenceResult)
RETURN d.address, r.anomaly_score, r.is_anomalous, r.uncertainty_margin, r.inferred_at_ns
ORDER BY r.inferred_at_ns DESC LIMIT 10
```
- [ ] GnnInferenceResult nodes exist
- [ ] Scores in [0.0, 1.0] range

### 13.3 Attention weight verification

```bash
curl -s "http://localhost:3000/api/gnn/inference-results?limit=1" | python3 -c "
import sys,json
results=json.load(sys.stdin)
if results and results[0]['is_anomalous']:
    print('Anomalous device detected — attention weights should be present')
else:
    print('No anomalous device — inject a fault and re-run')
"
```

If anomalous device detected:
```cypher
MATCH (r:GnnInferenceResult)-[:HAS_ATTENTION]->(a:GnnAttentionSnapshot)
WHERE r.device_address IS NOT NULL
RETURN r.device_address, a.neighbour_device_address, a.edge_type, a.attention_weight
ORDER BY a.attention_weight DESC LIMIT 5
```
- [ ] Attention snapshots present for anomalous devices
- [ ] `attention_weight` in (0.0, 1.0] range

### 13.4 Uncertainty-gated investigation trigger

Set the uncertainty gate threshold lower to force a trigger during testing:
```bash
curl -s -X PATCH http://localhost:3000/api/settings/gnn \
  -H 'Content-Type: application/json' \
  -d '{"gnn_trigger_threshold": 0.5, "gnn_uncertainty_gate": 0.9}'
```

Now inject a fault and watch:
```bash
# Inject BGP down via syslog
echo '<11>May 25 12:00:00 172.20.20.2 bgpd: %BGP-3-NOTIFICATION: neighbor 10.0.0.2 Down' | \
  nc -u -w1 localhost 5514

# Wait for inference cycle (or trigger manually)
sleep 10
curl -s -X POST http://localhost:3000/api/ml/jobs \
  -H 'Content-Type: application/json' \
  -d '{"job_id":"gnn_inference","trigger":"manual"}'
sleep 30
```

**Check for auto-triggered investigation:**
```bash
curl -s "http://localhost:3000/api/investigations?limit=5" | python3 -m json.tool
```
- [ ] Investigation auto-created for device `172.20.20.2`
- [ ] `triggered_by: "gnn_anomaly"` in investigation record
- [ ] Investigation prompt contains attention context ("GNN attention context: device X contributed...")

**Reset gate to conservative default:**
```bash
curl -s -X PATCH http://localhost:3000/api/settings/gnn \
  -H 'Content-Type: application/json' \
  -d '{"gnn_trigger_threshold": 0.75, "gnn_uncertainty_gate": 0.3}'
```

---

## Phase 14 — Semantic Embeddings

### 14.1 Verify embedding model loads

The syslog embedding worker runs every 60s. Watch logs:
```bash
# Check if sentence-transformers loaded
curl -s http://localhost:9200/health | python3 -c "
import sys,json
h=json.load(sys.stdin)
print(f'Pending syslog embeddings: {h.get(\"embedding_pending_syslog\", 0)}')
print(f'Pending config embeddings: {h.get(\"embedding_pending_config\", 0)}')
"
```

### 14.2 Send syslog events and verify embedding queued

```bash
# Send multiple syslog messages to build embedding corpus
for msg in \
  "BGP session to 10.0.0.2 went Down — hold timer expired" \
  "Interface ethernet-1/1 link-state changed to down" \
  "ISIS adjacency to spine-1 lost — BFD session down" \
  "BGP NOTIFICATION received from 10.0.0.3 error=4 subcode=0" \
  "SRL cpu-util critical: 95% sustained for 5 minutes"; do
  echo "<14>May 25 12:00:00 172.20.20.2 syslog: $msg" | nc -u -w1 localhost 5514
  sleep 0.5
done

# Wait for embedding worker cycle (60s)
echo "Waiting 65s for embedding worker..."
sleep 65
```

**Verify embeddings written to graph:**
```bash
curl -s "http://localhost:3000/api/ml/embeddings/stats" | python3 -m json.tool
```
- [ ] `syslog_embedded > 0`
- [ ] `model_name: "all-MiniLM-L6-v2"` (or configured model)
- [ ] `syslog_pending` decreasing

```cypher
MATCH (e:StateChangeEvent)-[:EMBEDDED_AS]->(emb:EventEmbedding)
RETURN e.message, emb.model_name, emb.computed_at_ns
LIMIT 5
```
- [ ] EventEmbedding nodes exist linked to StateChangeEvents
- [ ] `vector_json` is non-null (full 384-dim vector)
- [ ] `needs_embedding = false` on embedded events

### 14.3 Config embedding pipeline

```bash
# Trigger config embedding worker cycle
curl -s -X POST http://localhost:3000/api/ml/jobs \
  -H 'Content-Type: application/json' \
  -d '{"job_id":"config_embedding","trigger":"manual"}'

sleep 30
```

**Verify config embeddings:**
```cypher
MATCH (d:Device)-[:CONFIG_EMBEDDED_AS]->(emb:DeviceConfigEmbedding)
RETURN d.address, emb.model_name, emb.schema_hash
```
- [ ] At least one device has a `DeviceConfigEmbedding` node

### 14.4 Semantic similarity search

```bash
# Get an event ID from the graph
EVENT_ID=$(curl -s "http://localhost:3000/api/events/history?type=syslog&limit=1" | \
  python3 -c "import sys,json; evts=json.load(sys.stdin); print(evts[0]['id'])" 2>/dev/null || echo "test-id")

curl -s "http://localhost:3000/api/ml/similar-events?event_id=${EVENT_ID}&limit=5" | python3 -m json.tool
```
- [ ] Returns list of similar events with `similarity_score` field
- [ ] Similar events have semantically related messages (e.g., BGP down events returned for a BGP down query event)

### 14.5 Syslog clustering

Run the clustering job:
```bash
curl -s -X POST http://localhost:3000/api/ml/jobs \
  -H 'Content-Type: application/json' \
  -d '{"job_id":"detection_clustering","trigger":"manual"}'

sleep 30
```

**Verify cluster assignments:**
```cypher
MATCH (e:StateChangeEvent)
WHERE e.incident_cluster_id IS NOT NULL
RETURN e.incident_cluster_id, count(*) as event_count
ORDER BY event_count DESC
```
- [ ] Events have `incident_cluster_id` assigned
- [ ] Multiple clusters present (not all in cluster 0)

**Verify SyslogCluster nodes:**
```cypher
MATCH (c:SyslogCluster)
RETURN c.id, c.event_count, c.top_event_types
```
- [ ] `SyslogCluster` nodes created with centroids

---

## Phase 15 — Parquet Store Management

### 15.1 Verify versioned archive layout

```bash
tree runtime/parquet/
```
**Expected structure:**
```
runtime/parquet/
  anomaly/
    2026-05-25T...Z_v1_Nrows.parquet
    latest -> 2026-05-25T...Z_v1_Nrows.parquet
  remediation/
    (empty or one file)
  gnn_snapshots/
    2026-05-25T...Z_T8_snapshots.pkl (or .arrow)
    latest -> ...
```
- [ ] `latest` symlink exists in each directory
- [ ] File naming includes timestamp, version, row count

### 15.2 Remediation export

```bash
python3 -m bonsai_ml.export_job --type remediation
```
- [ ] Remediation parquet file created (may be 0 rows if no remediations ran — that's OK, `status: "completed"` expected)

### 15.3 Lineage tracking

```bash
# Get the active model ID
MODEL_ID=$(curl -s "http://localhost:3000/api/ml/models/active?type=stgnn" | \
  python3 -c "import sys,json; print(json.load(sys.stdin)['id'])")

curl -s "http://localhost:3000/api/ml/lineage/${MODEL_ID}" | python3 -m json.tool
```
- [ ] `parquet_exports` list shows which exports the model was trained on
- [ ] `time_window` shows data coverage period
- [ ] `is_active: true`

---

## Phase 16 — Memory Management & Backpressure

### 16.1 Memory bounds check

```bash
# Check sidecar memory report
curl -s http://localhost:9200/health | python3 -c "
import sys,json
h=json.load(sys.stdin)
print(f'RSS: {h[\"memory_usage_mb\"]} MB')
print(f'Job engine running: {h[\"job_engine_running\"]}')
"

# Get memory component breakdown from Prometheus
curl -s http://localhost:9201/metrics | grep memory
```
- [ ] RSS < 2048 MB (systemd MemoryMax limit)
- [ ] No `bonsai_sidecar_memory_oom_evictions_total` counter increasing

### 16.2 Forward queue backpressure test

```bash
# Flood the queue with synthetic detections
python3 -c "
import requests, time

base = 'http://localhost:3000'
for i in range(200):
    requests.post(f'{base}/api/detections', json={
        'rule_id': 'bgp_session_flap',
        'device_address': f'10.0.{i//256}.{i%256}',
        'severity': 'info',
        'reason': f'Synthetic detection {i}'
    })
print('Done')
"

sleep 2
curl -s http://localhost:9200/health | python3 -c "
import sys,json
h=json.load(sys.stdin)
print(f'Queue depth: {h[\"queue_depth\"]}')
print(f'Queue drops today: {h[\"queue_drops_today\"]}')
"
```

Monitor Prometheus:
```bash
curl -s http://localhost:9201/metrics | grep forward_queue
```
- [ ] Queue depth stays < 1000
- [ ] `bonsai_forward_queue_drops_total` may increment under heavy load — expected

### 16.3 Resource governor integration

Check if bonsai core is applying memory pressure:
```bash
curl -s http://localhost:3000/api/governance/pressure | python3 -m json.tool
```
- [ ] Response contains `write_pressure`, `rate_shedding`, `memory_hard_limit` fields

When `write_pressure: true`, verify sidecar pauses heavy jobs:
```bash
# Simulate: check sidecar logs for "pausing heavy jobs due to memory pressure"
curl -s http://localhost:9200/health | python3 -c "
import sys,json
h=json.load(sys.stdin)
print(f'Next job: {h.get(\"next_job\")}')
"
```

---

*Part 2 complete. Continue with Part 3 for BonPy UI, rule management, and end-to-end validation.*
