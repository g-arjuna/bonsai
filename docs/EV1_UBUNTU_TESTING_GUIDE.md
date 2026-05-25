# EV1 Ubuntu Testing Guide

> **Sprint**: EV1 — ML Intelligence, GNN Architecture & BonPy–Bonsai Unification
> **Generated**: auto-merged from ev1/ev1-testing-part1..3.md
> **Ubuntu ops box prerequisites**: Rust 1.95+, cmake, protoc, Docker 24+, ContainerLab ≥0.54, Python 3.12+
> **Key ports**: Bonsai API `:3000`, Sidecar health `:9200`, Sidecar Prometheus `:9201`, PyATS sidecar `:5000`

---

## Table of Contents

| Phase | Area |
|-------|------|
| 0 | Pre-Flight Checklist |
| 1 | Clean-Slate Bonsai Core Startup |
| 2 | Device Onboarding — PyATS (automated) + Manual |
| 3 | gNMI Telemetry Flow |
| 4 | Syslog Reception |
| 5 | SNMP Trap Reception |
| 6 | Multi-Source Correlation |
| 7 | Detection Firing Baseline |
| 8 | Remediation Proposal Flow |
| 9 | Python Sidecar Startup |
| 10 | ML Job Engine |
| 11 | Parquet Export Pipeline |
| 12 | STGNN Training |
| 13 | STGNN Live Inference |
| 14 | Semantic Embeddings |
| 15 | Parquet Store Management |
| 16 | Memory Management & Backpressure |
| 17 | BonPy MLOps Console UI |
| 18 | Rule Management (EV1-7) |
| 19 | Change Management Integration |
| 20 | End-to-End ML Fault Detection Cycle |
| 21 | NetBox Integration Test |
| 22 | Final Validation Scorecard |

---

> **Sprint**: EV1 (Embedded Vision 1)
> **Ubuntu ops box prerequisites**: Rust 1.95+, cmake, protoc, Docker 24+, ContainerLab, Python 3.12+
> **Build prerequisite**: `git pull && cargo build --release && cd ui && npm ci && npm run build && cd ../ui-bonpy && npm ci && npm run build`

---

## Phase 0 — Pre-Flight Checklist

Before any EV1 testing, complete this checklist. Every item must pass.

```bash
# 1. Pull latest code
cd ~/bonsai
git pull origin main

# 2. Full Rust build
cargo build --release
# Expected: compiles successfully. etcd-client and lbug warnings are OK. Zero errors.

# 3. UI build
cd ui && npm ci && npm run build
cd ../ui-bonpy && npm ci && npm run build
cd ..

# 4. Python ML deps
cd python
pip install -e ".[dev]"
pip install sentence-transformers torch torch-geometric apscheduler pyarrow scikit-learn hdbscan
cd ..

# 5. ContainerLab available
sudo clab version   # expect: >= 0.54

# 6. Docker running
docker ps           # expect: daemon running

# 7. Disk space
df -h               # expect: >= 20 GB free on runtime partition

# 8. Wipe any previous runtime
rm -rf runtime/
```

**EV1-specific Python ML dep check:**
```bash
python3 -c "import torch; import torch_geometric; import sentence_transformers; import apscheduler; import pyarrow; print('ALL OK')"
# Expected: ALL OK
```

**ServiceNow PDI check** (only if testing SNOW integration):
```
- Open https://developer.servicenow.com
- Wake PDI if hibernated (takes ~5 min)
- Note instance URL and credentials
```

---

## Phase 1 — Clean-Slate Bonsai Core Startup

```bash
# Create minimal config
cp bonsai.toml.example bonsai.toml

# Set vault passphrase
export BONSAI_VAULT_PASSPHRASE="test-passphrase-ev1-2026"

# Start in mode=all (core + collector + local sidecar in one process)
./target/release/bonsai --config bonsai.toml &
BONSAI_PID=$!

# Wait for health
sleep 5
curl -s http://localhost:3000/health | python3 -m json.tool
```

**Expected health response fields:**
```json
{
  "status": "ok",
  "version": "...",
  "mode": "all",
  "graph_node_count": 0,
  "sidecar_registered": false
}
```

**UI check:**
```
open http://localhost:3000
```
- [ ] Onboarding wizard appears on first boot
- [ ] No JS console errors
- [ ] BonPy console accessible at `http://localhost:3000/bonpy/`
- [ ] BonPy nav shows: Dashboard, Jobs, Models, Exports, GNN, Embeddings, Rules, Detections

---

## Phase 2 — Device Onboarding (Two Methods)

### Method A — PyATS Bootstrap (Automated)

Requires ContainerLab SRL lab running. Start it:

```bash
# Start 3-node SRL fast-iteration lab
cd lab/fast-iteration
sudo clab deploy -t 3node-srl.clab.yml
cd ../..

# Verify lab nodes are reachable
ping -c1 172.20.20.2   # srl1
ping -c1 172.20.20.3   # srl2
ping -c1 172.20.20.4   # srl3
```

Add device credential first:
```bash
curl -s -X POST http://localhost:3000/api/credentials \
  -H 'Content-Type: application/json' \
  -d '{"alias":"srl-lab","username":"admin","password":"NokiaSrl1!","type":"gnmi"}' | python3 -m json.tool
```

Bootstrap a device:
```bash
curl -s -X POST http://localhost:3000/api/devices/bootstrap \
  -H 'Content-Type: application/json' \
  -d '{
    "address": "172.20.20.2",
    "credential_alias": "srl-lab",
    "vendor": "nokia_srlinux",
    "profile": "dc_leaf"
  }' | python3 -m json.tool
```

**Expected response fields:**
```json
{
  "status": "seeded",
  "device_address": "172.20.20.2",
  "interfaces_seeded": 8,
  "bgp_neighbors_seeded": 2,
  "lldp_neighbors_seeded": 2,
  "ospf_neighbors_seeded": 0,
  "bfd_sessions_seeded": 0,
  "platform_detail": {"model": "7220 IXR-D2", ...}
}
```

**Verify in graph (Explorer → Cypher):**
```cypher
MATCH (d:Device {address: "172.20.20.2"})
RETURN d.address, d.hostname, d.vendor, d.model, d.cpu_util_pct, d.memory_used_mb
```
- [ ] Device node exists with model/CPU/memory populated
- [ ] `needs_config_embedding = true` on device node

```cypher
MATCH (d:Device {address: "172.20.20.2"})-[:HAS_INTERFACE]->(i:Interface)
RETURN i.name, i.if_index, i.is_in_lag
```
- [ ] Interfaces seeded (≥4 expected for SRL dc_leaf)

**Bulk bootstrap** (all 3 lab nodes):
```bash
curl -s -X POST http://localhost:3000/api/devices/bootstrap/bulk \
  -H 'Content-Type: application/json' \
  -d '{
    "devices": [
      {"address":"172.20.20.2","credential_alias":"srl-lab","vendor":"nokia_srlinux","profile":"dc_leaf"},
      {"address":"172.20.20.3","credential_alias":"srl-lab","vendor":"nokia_srlinux","profile":"dc_leaf"},
      {"address":"172.20.20.4","credential_alias":"srl-lab","vendor":"nokia_srlinux","profile":"dc_spine"}
    ],
    "parallel": 3
  }' | python3 -m json.tool
```
- [ ] All 3 devices seeded
- [ ] `seeded_count: 3` in response

### Method B — Manual Onboarding via UI

For environments without PyATS access:

1. Open `http://localhost:3000`
2. Navigate to **Devices** → **Add Device**
3. Fill form:
   - Address: `192.168.1.1`
   - Vendor: `Nokia SR Linux`
   - Credential: select `srl-lab` from vault
   - Profile: `dc_leaf`
4. Click **Add Device**

**Verify:**
```bash
curl -s http://localhost:3000/api/onboarding/devices | python3 -m json.tool
```
- [ ] Device appears in list with `status: "managed"`

**Manual seed via API** (if PyATS unavailable, seed known topology):
```bash
curl -s -X POST http://localhost:3000/api/devices/seed \
  -H 'Content-Type: application/json' \
  -d '{
    "address": "192.168.1.1",
    "hostname": "spine-1",
    "vendor": "nokia_srlinux",
    "interfaces": [
      {"name": "ethernet-1/1", "ip": "10.0.0.1", "is_up": true},
      {"name": "ethernet-1/2", "ip": "10.0.0.3", "is_up": true}
    ],
    "bgp_neighbors": [
      {"peer_address": "10.0.0.2", "local_as": 65001, "remote_as": 65002, "state": "established"}
    ],
    "lldp_neighbors": [
      {"local_interface": "ethernet-1/1", "neighbor_chassis_id": "aa:bb:cc:dd:ee:ff", "neighbor_hostname": "leaf-1"}
    ]
  }' | python3 -m json.tool
```
- [ ] `status: "seeded"` in response
- [ ] Device queryable in Explorer

---

## Phase 3 — gNMI Telemetry Flow

Start gNMI subscription to the lab device:
```bash
# Verify gNMI readiness
curl -s "http://localhost:3000/api/devices/172.20.20.2/gnmi-readiness" | python3 -m json.tool
# Expected: {"reachable": true, "gnmi_available": true}
```

In **Settings → Streaming**, confirm gNMI subscription is active. Then inject some traffic on the SRL nodes to generate interface counter updates.

**Verify interface counters flowing:**
```cypher
MATCH (d:Device {address:"172.20.20.2"})-[:HAS_INTERFACE]->(i:Interface)
WHERE i.in_octets IS NOT NULL
RETURN i.name, i.in_octets, i.out_octets, i.in_errors, i.out_errors
```
- [ ] At least one interface has non-null `in_octets`
- [ ] Counters updating every 30s (default SAMPLE interval)

**Verify BGP state flowing:**
```cypher
MATCH (d:Device {address:"172.20.20.2"})-[:HAS_BGP_NEIGHBOR]->(b:BgpNeighbor)
RETURN b.peer_address, b.state, b.adj_rib_in_routes, b.hold_time
```
- [ ] BGP neighbors exist with state populated

---

## Phase 4 — Syslog Reception

Configure SRL to send syslog to bonsai (port 5514 UDP):
```bash
# On SRL node via SSH
ssh admin@172.20.20.2
# In SRL CLI:
# /system logging remote 172.20.20.1 port 5514 facility local7
```

Or send test syslog directly:
```bash
echo '<14>May 25 10:00:00 172.20.20.2 bgpd: %BGP-5-ADJCHANGE: neighbor 10.0.0.2 Up' | \
  nc -u -w1 localhost 5514
```

**Verify syslog received:**
```bash
curl -s "http://localhost:3000/api/events/history?type=syslog&limit=5" | python3 -m json.tool
```
- [ ] Syslog events appear with `fact_type` populated
- [ ] Device address mapped correctly
- [ ] Events with `needs_embedding = true` field

```cypher
MATCH (e:StateChangeEvent {source: "syslog"})
WHERE e.needs_embedding = true
RETURN e.device_address, e.message, e.occurred_at
LIMIT 5
```
- [ ] Events tagged `needs_embedding = true` appear — these feed the syslog embedding worker

---

## Phase 5 — SNMP Trap Reception

Send test SNMP v2c trap:
```bash
# Send a BGP peer-down trap (OID 1.3.6.1.2.1.15.7)
snmptrap -v 2c -c public localhost:9162 "" 1.3.6.1.2.1.15.7

# Or via Python
python3 -c "
from pysnmp.hlapi import *
g = sendNotification(
    SnmpEngine(),
    CommunityData('public'),
    UdpTransportTarget(('localhost', 9162)),
    ContextData(),
    'trap',
    NotificationType(ObjectIdentity('1.3.6.1.2.1.15.7'))
)
for errorIndication, errorStatus, errorIndex, varBinds in g:
    print('Sent')
"
```

**Verify:**
```bash
curl -s "http://localhost:3000/api/events/history?type=snmp&limit=5" | python3 -m json.tool
```
- [ ] SNMP trap event present with `fact_type`

---

## Phase 6 — Multi-Source Correlation

Inject a BGP session down event via both syslog and SNMP within the 45-second correlation window:

```bash
# Syslog: BGP down
echo '<11>May 25 10:05:00 172.20.20.2 bgpd: %BGP-3-NOTIFICATION: neighbor 10.0.0.2 Down' | \
  nc -u -w1 localhost 5514

sleep 5

# Trigger a gNMI state change by admin-disabling interface on SRL
# (or send via SNMP trap)
```

**Verify single correlated detection:**
```bash
curl -s "http://localhost:3000/api/detections?limit=10" | python3 -m json.tool
```
- [ ] Detection appears with `source_event_ids` containing IDs from multiple sources
- [ ] `correlation_fused: true` on the detection (or single detection from multiple sources, not duplicates)

**Verify correlation buffer metric:**
```bash
curl -s http://localhost:3000/health | python3 -m json.tool | grep correlation
```
- [ ] `bonsai_correlation_multi_source_total` counter incrementing

---

## Phase 7 — Detection Firing Baseline

Inject a BGP all-peers-down scenario by flapping the BGP neighbor:

```bash
# Simulate BGP all-peers-down detection via syslog storm
for i in 1 2 3; do
  echo "<11>May 25 10:10:0${i} 172.20.20.2 bgpd: %BGP-3-NOTIFICATION: neighbor 10.0.0.2 Down" | \
    nc -u -w1 localhost 5514
  sleep 1
done
```

**Verify detection in UI and API:**
```bash
curl -s "http://localhost:3000/api/detections?limit=5" | python3 -m json.tool
```
- [ ] `bgp_session_down` or similar rule_id
- [ ] Severity: `high` or `critical`
- [ ] `reason` string is human-readable

**Verify blast radius:**
```bash
DEVICE="172.20.20.2"
curl -s "http://localhost:3000/api/blast-radius/${DEVICE}" | python3 -m json.tool
```
- [ ] Response includes `affected_interfaces`, `bgp_sessions`, `bfd_sessions`

---

## Phase 8 — Remediation Proposal Flow

```bash
# Check if a remediation proposal was auto-created
curl -s "http://localhost:3000/api/approvals?limit=5" | python3 -m json.tool
```
- [ ] Proposal appears with playbook steps
- [ ] Trust state = `suggest_only` initially

**Approve a proposal:**
```bash
PROPOSAL_ID="<id-from-above>"
curl -s -X POST "http://localhost:3000/api/approvals/${PROPOSAL_ID}/approve" | python3 -m json.tool
```
- [ ] Status changes to `approved` → `executing` → `executed`

**Verify via graph:**
```cypher
MATCH (r:RemediationProposal {id: "<proposal-id>"})
RETURN r.status, r.outcome, r.executed_at_ns
```
- [ ] `outcome: "success"` after execution

---

*Part 1 complete. Continue with Part 2 for ML pipeline testing.*


---

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


---

> **Prerequisites**: Parts 1 and 2 complete. STGNN model active. Embeddings flowing. Sidecar running.

---

## Phase 17 — BonPy MLOps Console UI

All BonPy routes are served from the same Bonsai origin at `/bonpy/`.

### 17.1 Dashboard (`/bonpy/`)

```
open http://localhost:3000/bonpy/
```

**Check each status card:**
- [ ] **Sidecars** strip: green ● with uptime (not red/amber)
- [ ] **Active Model** card: shows model_id, val_AUC, last inference timestamp
- [ ] **GNN Status** card: shows last inference time, anomalous device count
- [ ] **Parquet Freshness** card: age in hours, row count, PASS/FAIL/STALE badge
- [ ] **Next Scheduled Jobs** list: 5 upcoming jobs with countdown

**SSE live-update check:**
1. Open DevTools → Network → Filter by `events/stream`
2. Trigger a job: `curl -s -X POST http://localhost:3000/api/ml/jobs -H 'Content-Type: application/json' -d '{"job_id":"syslog_embedding","trigger":"manual"}'`
3. Watch for SSE events arriving in the DevTools stream
- [ ] `JobStarted` event received in browser
- [ ] Dashboard card updates without page reload

**Navigation check:**
- [ ] Left sidebar: Dashboard, Jobs, Models, Exports, GNN, Embeddings, Rules, Detections
- [ ] `← Network View` link navigates back to `http://localhost:3000/`
- [ ] SSE connection dot in header is green

### 17.2 Jobs Page (`/bonpy/jobs`)

```
open http://localhost:3000/bonpy/jobs
```

- [ ] **Schedule table** shows 7 default jobs with cron expressions
- [ ] Each job has: cron_expr, enabled toggle, last_run time, next_run time, last_outcome badge
- [ ] **"Run Now"** button triggers the job and shows live progress SSE bar
- [ ] **Job run history table** shows completed runs with duration, outcome, metrics columns
- [ ] **Dead letter queue** section: empty (expected if no failures)
- [ ] **Schedule edit**: click edit on `syslog_embedding`, change to `interval(seconds=30)`, save → verify schedule updated via `GET /api/ml/schedules`
- [ ] **Revert**: restore to `interval(seconds=60)`

**Active job progress panel:**
1. Trigger a training job: `curl -s -X POST http://localhost:3000/api/ml/jobs -H 'Content-Type: application/json' -d '{"job_id":"anomaly_export_daily","trigger":"manual"}'`
2. Watch BonPy Jobs page
- [ ] Progress bar appears for the running job
- [ ] Step counter updates (e.g., `rows: 0/N`)
- [ ] Job completes and moves to history

### 17.3 Models Page (`/bonpy/models`)

```
open http://localhost:3000/bonpy/models
```

- [ ] Model registry table shows at least 1 model (trained in Phase 12)
- [ ] Active model has a green "Active" badge
- [ ] **"Activate"** button available on non-active models (disabled on active)
- [ ] **"View Card"** button opens the model card document
- [ ] **Version history chart** (Chart.js line chart) shows val_AUC over training runs
- [ ] **Lineage panel**: shows which Parquet exports the model was trained on, date range

**Model comparison (if 2+ models):**
- [ ] Select 2 models → side-by-side metrics table shows val_AUC, val_F1, threshold differences

### 17.4 Exports Page (`/bonpy/exports`)

```
open http://localhost:3000/bonpy/exports
```

- [ ] Export catalog table shows past runs with quality badges
- [ ] **Quality detail modal**: click "View Quality" → class balance bar, feature PSI table visible
- [ ] **Export schedule section**: shows next anomaly/remediation export times
- [ ] **"Export Now"** button triggers incremental export:
  - [ ] Progress bar appears via SSE
  - [ ] Catalog updates after completion
- [ ] **Parquet file browser**: lists files in `runtime/parquet/` with size, age

### 17.5 GNN Page (`/bonpy/gnn`)

```
open http://localhost:3000/bonpy/gnn
```

- [ ] **Inference timeline** Chart.js bar chart: shows last 24 inference runs (or fewer if less data)
- [ ] **Latest inference results table**: device_address, anomaly_score (colour-coded: green < 0.5, amber 0.5-0.75, red > 0.75), is_anomalous badge
- [ ] **Snapshot buffer health**: shows buffer_size/8 progress bar, stale warning absent
- [ ] **Inference settings** panel: model selector, threshold slider, interval field, "Run Now" button
- [ ] Click **"Run Now"** for inference: progress visible, results table refreshes after
- [ ] **Attention mini-viz**: click on an anomalous device → shows SVG/D3 force-directed graph of top-5 contributing neighbours with edge width proportional to attention weight
- [ ] If investigation was auto-triggered: "View Investigation" link present on anomalous device row

### 17.6 Embeddings Page (`/bonpy/embeddings`)

```
open http://localhost:3000/bonpy/embeddings
```

- [ ] **Health cards**: syslog, config — each shows embedded count, pending count, model name, last batch time
- [ ] Throughput gauge shows events/hour (may be 0 if no recent activity)
- [ ] **Syslog cluster explorer**: click a cluster → sample events shown in modal
- [ ] **Embedding drift monitor**: line chart of cluster sizes over time (if multiple clustering runs available)

### 17.7 Detections Page (`/bonpy/detections`)

```
open http://localhost:3000/bonpy/detections
```

- [ ] SSE-driven live detection stream (new detections appear without refresh)
- [ ] Filter by severity: select "high" → only high/critical detections shown
- [ ] Per-row: device badge, rule_id, severity chip, reason snippet, occurred_at, GNN score overlay
- [ ] **Expand row**: full features JSON, investigation link (if exists), remediation proposals
- [ ] **"Bulk acknowledge"** checkbox: select multiple → bulk action available

---

## Phase 18 — Rule Management (EV1-7)

### 18.1 DB-backed rule enable/disable

**List all Python rules with enabled state:**
```bash
curl -s http://localhost:3000/api/sidecar/rules | python3 -m json.tool
```
- [ ] Returns list of rules (≥14 expected)
- [ ] Each has `rule_id`, `enabled`, `parameters_json`

**Disable a non-critical rule:**
```bash
curl -s -X POST http://localhost:3000/api/sidecar/rules/interface_error_spike/toggle \
  -H 'Content-Type: application/json' \
  -d '{"enabled": false}'
```

Wait 65 seconds for the rule override poller to pick up the change, then verify:
```bash
curl -s http://localhost:9200/health | python3 -c "
import sys,json
h=json.load(sys.stdin)
print(f'Rules enabled: {h[\"rules_enabled\"]}')
# Should be 1 less than before
"
```
- [ ] `rules_enabled` count decreased by 1 (no restart required)

**Re-enable:**
```bash
curl -s -X POST http://localhost:3000/api/sidecar/rules/interface_error_spike/toggle \
  -H 'Content-Type: application/json' \
  -d '{"enabled": true}'
sleep 65
curl -s http://localhost:9200/health | python3 -c "import sys,json; h=json.load(sys.stdin); print(f'Rules enabled: {h[\"rules_enabled\"]}')"
```
- [ ] Count restored

### 18.2 Per-rule parameter overrides

Get current parameters for BGP flap rule:
```bash
curl -s http://localhost:3000/api/sidecar/rules/bgp_session_flap/parameters | python3 -m json.tool
```
- [ ] `flap_count_threshold: 3` (default)
- [ ] `flap_window_seconds: 300` (default)

Change threshold:
```bash
curl -s -X PATCH http://localhost:3000/api/sidecar/rules/bgp_session_flap/parameters \
  -H 'Content-Type: application/json' \
  -d '{"flap_count_threshold": 5, "flap_window_seconds": 600}'
```

Verify hot-reload:
```bash
sleep 65
curl -s http://localhost:3000/api/sidecar/rules/bgp_session_flap/parameters | python3 -m json.tool
```
- [ ] `flap_count_threshold: 5` returned (persisted in DB, applied by rule)

**Test the changed threshold:** inject 3 BGP flaps (below the new threshold of 5):
```bash
for i in 1 2 3; do
  echo "<11>May 25 13:0${i}:00 172.20.20.2 bgpd: %BGP-3-NOTIFICATION: neighbor 10.0.0.2 Down" | \
    nc -u -w1 localhost 5514
  sleep 2
done
sleep 5
```

Check no detection fired:
```bash
curl -s "http://localhost:3000/api/detections?limit=5" | python3 -c "
import sys,json
d=json.load(sys.stdin)
recent = [x for x in d if 'bgp_session_flap' in x.get('rule_id','')]
print(f'BGP flap detections after 3 events (threshold=5): {len(recent)} — expected: 0')
"
```
- [ ] No `bgp_session_flap` detection with only 3 events

**Restore default:**
```bash
curl -s -X PATCH http://localhost:3000/api/sidecar/rules/bgp_session_flap/parameters \
  -H 'Content-Type: application/json' \
  -d '{"flap_count_threshold": 3, "flap_window_seconds": 300}'
```

### 18.3 Shadow mode

Enable shadow mode on a rule:
```bash
curl -s -X POST http://localhost:3000/api/sidecar/rules/interface_high_utilization/shadow-mode \
  -H 'Content-Type: application/json' \
  -d '{"enabled": true}'
```

Inject interface utilization event:
```bash
echo "<14>May 25 13:30:00 172.20.20.2 interface: ethernet-1/1 utilization 85%" | \
  nc -u -w1 localhost 5514
sleep 5
```

Verify: shadow fired but NOT a real detection:
```bash
# Shadow firings (recorded internally)
curl -s "http://localhost:3000/api/sidecar/rules/interface_high_utilization/shadow-firings" | python3 -m json.tool
```
- [ ] Shadow firings > 0

```bash
# Real detections (should NOT have interface_high_utilization)
curl -s "http://localhost:3000/api/detections?limit=5" | python3 -c "
import sys,json
d=json.load(sys.stdin)
shadow_leaks = [x for x in d if 'interface_high_utilization' in x.get('rule_id','')]
print(f'Detections from shadow rule: {len(shadow_leaks)} — expected: 0')
"
```
- [ ] No real detections from shadow-mode rule

**Disable shadow mode:**
```bash
curl -s -X POST http://localhost:3000/api/sidecar/rules/interface_high_utilization/shadow-mode \
  -H 'Content-Type: application/json' \
  -d '{"enabled": false}'
```

### 18.4 Rule firing analytics

```bash
curl -s "http://localhost:3000/api/sidecar/rules/analytics?window_hours=24" | python3 -m json.tool
```
- [ ] Returns stats for all rules over past 24h
- [ ] `fire_count`, `shadow_fire_count` per rule

**Verify in BonPy UI at `/bonpy/rules`:**
- [ ] Rules tab shows firing rate charts
- [ ] Python rules: enable/disable toggle, parameter sliders for key rules
- [ ] Shadow mode toggle visible per rule

### 18.5 Syslog pattern rule from UI

Create a new syslog detection rule entirely from UI:
1. Open `http://localhost:3000/bonpy/rules`
2. Click **"New Syslog Rule"**
3. Fill form:
   - Name: `test_oom_rule`
   - Vendor: `nokia_srlinux`
   - Pattern: `OOM|out of memory|killed process`
   - Fact type: `memory_oom`
   - Severity: `critical`
   - Example message: `kernel: Out of memory: killed process bgpd (bgp)`
4. Click **Test** → verify match result shows `matched: true`
5. Click **Save**

Via API:
```bash
curl -s -X POST http://localhost:3000/api/syslog-rules \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "test_oom_rule",
    "vendor": "nokia_srlinux",
    "pattern_regex": "OOM|out of memory|killed process",
    "fact_type": "memory_oom",
    "severity": "critical",
    "description": "Kernel OOM killer detection",
    "enabled": true,
    "example_message": "kernel: Out of memory: killed process bgpd"
  }' | python3 -m json.tool
```

Test the pattern:
```bash
RULE_ID=$(curl -s http://localhost:3000/api/syslog-rules | \
  python3 -c "import sys,json; rules=json.load(sys.stdin); print(next(r['id'] for r in rules if r['name']=='test_oom_rule'))")

curl -s -X POST "http://localhost:3000/api/syslog-rules/${RULE_ID}/test" \
  -H 'Content-Type: application/json' \
  -d '{"example_message": "kernel: Out of memory: killed process bgpd (bgp)"}' | python3 -m json.tool
```
- [ ] `matched: true`
- [ ] `extracted_facts` contains `fact_type: "memory_oom"`

Activate by sending a matching syslog:
```bash
echo '<2>May 25 14:00:00 172.20.20.2 kernel: Out of memory: killed process bgpd (bgp) total-vm:512MB' | \
  nc -u -w1 localhost 5514
sleep 5
curl -s "http://localhost:3000/api/detections?limit=5" | python3 -c "
import sys,json
d=json.load(sys.stdin)
oom = [x for x in d if x.get('rule_id','') == 'memory_oom']
print(f'OOM detection: {len(oom)} — expected: 1')
"
```
- [ ] Detection fired from the new UI-created syslog rule

### 18.6 DB-backed playbook management

Verify playbooks migrated to DB at boot:
```bash
curl -s http://localhost:3000/api/playbooks-v2 | python3 -c "
import sys,json
p=json.load(sys.stdin)
print(f'Playbooks in DB: {len(p)}')
for pb in p[:5]:
    print(f'  {pb[\"name\"]} - {pb[\"vendor\"]} - enabled={pb[\"enabled\"]}')
"
```
- [ ] ≥9 playbooks loaded from `playbooks/library/`

Create a new playbook from UI (`/bonpy/rules` → Playbooks tab → "New Playbook"):
```bash
curl -s -X POST http://localhost:3000/api/playbooks-v2 \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "test_bgp_soft_reset",
    "description": "EV1 test playbook — soft reset BGP peer",
    "rule_ids": ["bgp_session_flap"],
    "vendor": "nokia_srlinux",
    "steps": [
      {"type": "gnmi_set", "path": "/network-instance[name=default]/protocols/bgp/neighbor[peer-address={{peer_address}}]/soft-reset", "value": true, "risk": "safe"}
    ],
    "enabled": true
  }' | python3 -m json.tool
```
- [ ] Playbook created with `id` in response
- [ ] `version: 1`

Verify playbook test endpoint:
```bash
PB_ID="<id-from-above>"
curl -s -X POST "http://localhost:3000/api/playbooks-v2/${PB_ID}/test" \
  -H 'Content-Type: application/json' \
  -d '{"device_address": "172.20.20.2"}' | python3 -m json.tool
```
- [ ] `passed: true` or `passed: false` (depends on verify_graph state) — no 500 error

---

## Phase 19 — Change Management Integration

### 19.1 Change context detection during fault

Create a test change request:
```bash
curl -s -X POST http://localhost:3000/api/changes \
  -H 'Content-Type: application/json' \
  -d '{
    "number": "CHG0010001",
    "source": "manual",
    "state": "implement",
    "change_type": "normal",
    "risk": "low",
    "planned_start_ns": '"$(date -d 'now - 5 minutes' +%s%N)"',
    "planned_end_ns": '"$(date -d 'now + 55 minutes' +%s%N)"',
    "affected_devices": ["172.20.20.2"]
  }' | python3 -m json.tool
```

Now inject a fault on the same device:
```bash
echo '<11>May 25 14:30:00 172.20.20.2 bgpd: %BGP-3-NOTIFICATION: neighbor 10.0.0.2 Down' | \
  nc -u -w1 localhost 5514
sleep 5
```

**Check detection is change-correlated:**
```bash
curl -s "http://localhost:3000/api/detections?limit=5" | python3 -c "
import sys,json
d=json.load(sys.stdin)
for det in d[:3]:
    print(f'change_correlated={det.get(\"change_correlated\",False)}, reason_prefix={det.get(\"reason\",\"\")[:40]}')
"
```
- [ ] `change_correlated: true` on detection occurring during CHG0010001 window
- [ ] Reason prefixed with `[DURING CHANGE CHG0010001]`

### 19.2 Change context API

```bash
curl -s "http://localhost:3000/api/changes/context/172.20.20.2" | python3 -m json.tool
```
- [ ] `in_change_window: true`
- [ ] `active_changes[0].number: "CHG0010001"`

### 19.3 GNN CW-GNN training with change data (informational)

The control-weighted loss function uses `change_weight = 0.0` for fault labels during active change windows. To verify this is wired in training:
```bash
python3 -c "
from bonsai_ml.gnn.loss import FocalControlWeightedLoss
import torch

loss_fn = FocalControlWeightedLoss(gamma=2.0)
logits = torch.tensor([[0.1, 0.9], [0.8, 0.2]])  # 2 samples
labels = torch.tensor([1, 0])  # fault, normal
change_weights = torch.tensor([0.0, 1.0])  # first sample in change window

loss = loss_fn(logits, labels, change_weights)
print(f'CW loss: {loss.item():.4f}')

# Compare without change weighting (weight=1.0 for both)
loss_no_cw = loss_fn(logits, labels, torch.ones(2))
print(f'No-CW loss: {loss_no_cw.item():.4f}')
print(f'CW loss < No-CW loss: {loss.item() < loss_no_cw.item()} (expected: True)')
"
```
- [ ] CW loss < standard loss (change-window sample gradient suppressed)

---

## Phase 20 — End-to-End ML Fault Detection Cycle

This phase validates the complete EV1 loop end-to-end:

```
Fault injected → syslog/gNMI event → Detection fired → Syslog embedding queued
→ Embedding worker embeds → Graph snapshot updated → GNN inference → Anomaly scored
→ Investigation auto-triggered (if score high + uncertainty low) → AI RCA with attention context
→ Remediation proposed → Human approves → Playbook executes → verify_graph passes
```

### 20.1 Inject a realistic fault scenario

```bash
# Step 1: BGP peer down (syslog)
echo '<11>May 25 15:00:00 172.20.20.2 bgpd: %BGP-3-NOTIFICATION: neighbor 10.0.0.2 Down' | \
  nc -u -w1 localhost 5514

# Step 2: Interface down (syslog)
echo '<11>May 25 15:00:01 172.20.20.2 intf: ethernet-1/1 link-state changed to down' | \
  nc -u -w1 localhost 5514

# Step 3: BFD session down (syslog)
echo '<11>May 25 15:00:02 172.20.20.2 bfdd: BFD session to 10.0.0.2 went Down' | \
  nc -u -w1 localhost 5514

sleep 5
```

### 20.2 Verify detection fired

```bash
curl -s "http://localhost:3000/api/detections?limit=5" | python3 -c "
import sys,json
d=json.load(sys.stdin)
if d:
    print(f'Detection: rule={d[0][\"rule_id\"]}, severity={d[0][\"severity\"]}')
    print(f'  Reason: {d[0].get(\"reason\",\"\")[:80]}')
    print(f'  Source events: {len(d[0].get(\"source_event_ids\",[]))}')
else:
    print('No detection — check rule engine')
"
```
- [ ] Detection present for `172.20.20.2`
- [ ] Multiple source event IDs (correlated)

### 20.3 Trigger full ML cycle

```bash
# Snapshot + inference
curl -s -X POST http://localhost:3000/api/ml/jobs -H 'Content-Type: application/json' \
  -d '{"job_id":"graph_snapshot","trigger":"manual"}'
sleep 10

curl -s -X POST http://localhost:3000/api/ml/jobs -H 'Content-Type: application/json' \
  -d '{"job_id":"gnn_inference","trigger":"manual"}'
sleep 30
```

### 20.4 Check GNN scored the fault device

```bash
curl -s "http://localhost:3000/api/gnn/inference-results?device_address=172.20.20.2&limit=3" | \
  python3 -m json.tool
```
- [ ] `anomaly_score > 0.5` for the fault device
- [ ] `uncertainty_margin` present

### 20.5 Check investigation auto-triggered

```bash
curl -s "http://localhost:3000/api/investigations?limit=3" | python3 -c "
import sys,json
invs=json.load(sys.stdin)
for inv in invs[:2]:
    print(f'ID: {inv[\"id\"]}')
    print(f'  Device: {inv.get(\"device_address\")}')
    print(f'  Triggered by: {inv.get(\"triggered_by\")}')
    print(f'  Status: {inv.get(\"status\")}')
"
```
- [ ] Investigation for `172.20.20.2` present
- [ ] `triggered_by` is `"gnn_anomaly"` or `"detection_fired"` (depending on which triggered first)

### 20.6 Verify attention context in AI investigation

```bash
INV_ID=$(curl -s "http://localhost:3000/api/investigations?limit=1" | \
  python3 -c "import sys,json; invs=json.load(sys.stdin); print(invs[0]['id'])")

curl -s "http://localhost:3000/api/investigations/${INV_ID}" | python3 -c "
import sys,json
inv=json.load(sys.stdin)
summary=inv.get('summary','')
if 'GNN attention' in summary or 'attention context' in summary.lower():
    print('✓ GNN attention context present in investigation prompt')
else:
    print('⚠ GNN attention context NOT found in investigation — check investigation_trigger.rs')
print(f'Summary (first 200 chars): {summary[:200]}')
"
```
- [ ] Investigation summary contains GNN attention context

### 20.7 Operator feedback on investigation

```bash
curl -s -X POST "http://localhost:3000/api/investigations/${INV_ID}/feedback" \
  -H 'Content-Type: application/json' \
  -d '{"rating": "positive", "comment": "EV1 E2E test - correct root cause identified"}'
```
- [ ] Feedback accepted (200 OK)

```bash
curl -s "http://localhost:3000/api/investigations/accuracy" | python3 -m json.tool
```
- [ ] `positive_count: 1`, `total_count: 1`, `precision: 1.0`

### 20.8 Remediation proposal and execution

```bash
# Check if auto-proposal was created
curl -s "http://localhost:3000/api/approvals?limit=5" | python3 -c "
import sys,json
p=json.load(sys.stdin)
print(f'Proposals: {len(p)}')
if p:
    print(f'  Playbook: {p[0].get(\"playbook_name\",\"unknown\")}')
    print(f'  Trust: {p[0].get(\"risk_level\",\"unknown\")}')
    print(f'  Status: {p[0].get(\"status\",\"unknown\")}')
"
```
- [ ] Proposal exists for `172.20.20.2`

Execute the safe remediation:
```bash
APPROVAL_ID=$(curl -s "http://localhost:3000/api/approvals?limit=1" | \
  python3 -c "import sys,json; p=json.load(sys.stdin); print(p[0]['id'])")
curl -s -X POST "http://localhost:3000/api/approvals/${APPROVAL_ID}/approve"
sleep 10

curl -s "http://localhost:3000/api/approvals/${APPROVAL_ID}" | python3 -c "
import sys,json
p=json.load(sys.stdin)
print(f'Status: {p.get(\"status\")}')
print(f'Outcome: {p.get(\"outcome\")}')
"
```
- [ ] `status: "executed"` or `"completed"`
- [ ] `outcome: "success"` (or `"verify_failed"` if lab device didn't actually recover — that's OK for test)

---

## Phase 21 — NetBox Integration Test

> **Requires**: `docker compose -f docker/compose-netbox.yml up -d` (wait ~2 min for NetBox to initialise)

```bash
cd ~/bonsai
docker compose -f docker/compose-netbox.yml up -d
sleep 120  # NetBox startup
```

Configure NetBox enrichment:
```bash
curl -s -X PATCH http://localhost:3000/api/settings/integrations \
  -H 'Content-Type: application/json' \
  -d '{
    "netbox": {
      "enabled": true,
      "base_url": "http://localhost:8000",
      "token_env": "NETBOX_TOKEN"
    }
  }'
export NETBOX_TOKEN="your-netbox-token"
```

Trigger enrichment:
```bash
curl -s -X POST http://localhost:3000/api/integrations/netbox/sync | python3 -m json.tool
```
- [ ] Sync completes without error

Verify enrichment in graph:
```cypher
MATCH (d:Device {address:"172.20.20.2"})
RETURN d.netbox_name, d.netbox_site, d.netbox_role, d.netbox_rack
```
- [ ] NetBox properties present on device node

**Enrichment conflict check** (if device has different hostname in NetBox vs CLI):
1. Navigate to **Bonsai UI → Devices → device detail drawer**
2. Open **Enrichment** tab
- [ ] Conflict banner visible (if hostname differs between CLI and NetBox)
- [ ] Winner/loser fields shown with source badges

---

## Phase 22 — Final Validation Scorecard

Run this verification script to produce a pass/fail summary:

```bash
python3 -c "
import requests, json, sys

base = 'http://localhost:3000'
sidecar = 'http://localhost:9200'
prometheus = 'http://localhost:9201'

checks = []

def chk(name, fn):
    try:
        result = fn()
        checks.append((name, result, None))
    except Exception as e:
        checks.append((name, False, str(e)))

# Core health
chk('Bonsai core running', lambda: requests.get(f'{base}/health', timeout=5).json()['status'] == 'ok')
chk('Sidecar running', lambda: requests.get(f'{sidecar}/health', timeout=5).json()['status'] == 'ok')
chk('Sidecar job engine running', lambda: requests.get(f'{sidecar}/health', timeout=5).json()['job_engine_running'] == True)
chk('Sidecar connected to core', lambda: requests.get(f'{sidecar}/health', timeout=5).json()['connected_to_core'] == True)
chk('Prometheus metrics', lambda: 'bonsai_ml' in requests.get(f'{prometheus}/metrics', timeout=5).text)

# ML state
chk('ML schedules present', lambda: len(requests.get(f'{base}/api/ml/schedules', timeout=5).json()) >= 7)
chk('Active STGNN model', lambda: 'id' in requests.get(f'{base}/api/ml/models/active?type=stgnn', timeout=5).json())
chk('Parquet exports exist', lambda: len(requests.get(f'{base}/api/ml/exports', timeout=5).json()) >= 1)
chk('GNN inference results exist', lambda: len(requests.get(f'{base}/api/gnn/inference-results', timeout=5).json()) >= 1)

# Embeddings
chk('Syslog embeddings created', lambda: requests.get(f'{base}/api/ml/embeddings/stats', timeout=5).json()['syslog_embedded'] > 0)

# Rules
chk('Python rules loaded', lambda: len(requests.get(f'{base}/api/sidecar/rules', timeout=5).json()) >= 14)
chk('DB-backed playbooks exist', lambda: len(requests.get(f'{base}/api/playbooks-v2', timeout=5).json()) >= 5)

# Graph
chk('Devices in graph', lambda: len(requests.get(f'{base}/api/devices', timeout=5).json()) >= 1)
chk('Detections fired', lambda: len(requests.get(f'{base}/api/detections?limit=5', timeout=5).json()) >= 1)

# BonPy UI
chk('BonPy UI reachable', lambda: requests.get(f'{base}/bonpy/', timeout=5).status_code == 200)

passed = [(n,r,e) for n,r,e in checks if r]
failed = [(n,r,e) for n,r,e in checks if not r]

print()
print('='*60)
print('EV1 Ubuntu Testing — Final Scorecard')
print('='*60)
for name, result, err in checks:
    status = '✓' if result else '✗'
    err_str = f'  ({err})' if err else ''
    print(f'  {status} {name}{err_str}')
print()
print(f'PASSED: {len(passed)}/{len(checks)}')
print(f'FAILED: {len(failed)}/{len(checks)}')
print()
if failed:
    print('FAILED CHECKS:')
    for name, _, err in failed:
        print(f'  - {name}: {err}')
    sys.exit(1)
else:
    print('ALL CHECKS PASSED ✓')
"
```

**Expected output (all checks passing):**
```
============================================================
EV1 Ubuntu Testing — Final Scorecard
============================================================
  ✓ Bonsai core running
  ✓ Sidecar running
  ✓ Sidecar job engine running
  ✓ Sidecar connected to core
  ✓ Prometheus metrics
  ✓ ML schedules present
  ✓ Active STGNN model
  ✓ Parquet exports exist
  ✓ GNN inference results exist
  ✓ Syslog embeddings created
  ✓ Python rules loaded
  ✓ DB-backed playbooks exist
  ✓ Devices in graph
  ✓ Detections fired
  ✓ BonPy UI reachable

PASSED: 15/15
ALL CHECKS PASSED ✓
```

---

## EV1 Testing Summary

| Phase | Area | Result |
|-------|------|--------|
| 0 | Pre-flight | |
| 1 | Core startup | |
| 2 | Device onboarding (PyATS + manual) | |
| 3 | gNMI telemetry | |
| 4 | Syslog reception | |
| 5 | SNMP trap | |
| 6 | Multi-source correlation | |
| 7 | Detection firing | |
| 8 | Remediation proposal | |
| 9 | Sidecar startup | |
| 10 | ML job engine | |
| 11 | Parquet export pipeline | |
| 12 | STGNN training | |
| 13 | STGNN live inference | |
| 14 | Semantic embeddings | |
| 15 | Parquet store management | |
| 16 | Memory & backpressure | |
| 17 | BonPy UI all pages | |
| 18 | Rule management (DB-backed) | |
| 19 | Change management integration | |
| 20 | End-to-end ML fault cycle | |
| 21 | NetBox integration | |
| 22 | Final scorecard | |

Fill result column with PASS / FAIL / SKIP before committing.
