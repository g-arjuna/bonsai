# 30-Day Archive Run — GNN Training Data Collection

> **Objective**: accumulate 30 days of clean telemetry + chaos injection data
> across both labs (Ubuntu DC lab + cloud DC) to meet the D2-12 GNN trigger
> condition and produce the first GNN training dataset.
>
> **North star**: `models/gnn_v1.pt` — the first trained GNN anomaly detector.
>
> **Key rule**: Bonsai runs as a **native process only**. Docker / docker compose
> are **never used for bonsai**. Docker is only used by ContainerLab to run SRL nodes.

---

## Script inventory (the only scripts you need)

| Script | Purpose |
|---|---|
| `bash scripts/ops/rebuild_and_validate.sh` | Build + validate bonsai before starting |
| `bash scripts/lab/redeploy_dc.sh` | Destroy + redeploy DC clab topology (Ubuntu) |
| `bash scripts/lab/redeploy_cloud_dc.sh` | Destroy + redeploy cloud DC topology (cloud VM) |
| `bash scripts/ops/start_30day_run.sh` | **Single entry point**: start bonsai + sidecar + chaos |
| `bash scripts/ops/start_30day_run.sh --status` | Check what is running |
| `bash scripts/ops/teardown.sh` | Stop everything cleanly |
| `bash scripts/ops/event_detection_retirement_gate.sh` | D2-1 T1 gate (run before the 30-day run) |

**Do not use**: `scripts/cloud/deploy.sh`, `scripts/cloud/daily_sync.sh`,
`scripts/e2e_compose_test.sh`, or any script that calls `docker compose` for bonsai.

---

## Before you start — D2-1 T1 gate (critical)

The Rust fastpath (`event_detection.rs`) must be retired before the 30-day run
so that all detections come from the Python sidecar and are correctly labelled.

```bash
# On Ubuntu, with lab up and bonsai NOT yet running:
bash scripts/ops/event_detection_retirement_gate.sh
# Expected exit code: 0 (gate closed, file deleted, committed)
```

If this gate is not closed, `sidecar.detections_out_total` stays at 0 while
`/api/detections` shows results — meaning the training data has unlabelled rows.

---

## Phase 0 — Pull latest + validate build

**Ubuntu laptop**:
```bash
cd /home/arjuna/Desktop/bonsai
git pull --rebase origin main
bash scripts/ops/rebuild_and_validate.sh
# Expected: PASS=14+, FAIL=0
```

**Cloud VM** (run via SSH or on the VM directly):
```bash
cd /opt/bonsai
git pull --rebase origin main
bash scripts/ops/rebuild_and_validate.sh
# Expected: PASS=14+, FAIL=0
```

---

## Phase 1 — Full stop + clean slate (Ubuntu laptop)

### 1a. Stop everything

```bash
cd /home/arjuna/Desktop/bonsai
bash scripts/ops/teardown.sh
# Confirm port 3000 is free:
ss -tlnp | grep 3000 || echo "port 3000 free"
```

### 1b. Delete all archive data (parquet)

> ⚠️ Irreversible — discards all prior archive data for a clean 30-day baseline.

```bash
rm -rf /home/arjuna/Desktop/bonsai/runtime/archive/
rm -rf /home/arjuna/Desktop/bonsai/archive/
find /home/arjuna/Desktop/bonsai -name "*.parquet" 2>/dev/null | wc -l
# Expected: 0
```

### 1c. Delete graph database and WAL files

> Wipes all graph state. Bonsai rebuilds from live telemetry on first boot.

```bash
rm -f /home/arjuna/Desktop/bonsai/bonsai.db
rm -f /home/arjuna/Desktop/bonsai/bonsai.db.wal
rm -f /home/arjuna/Desktop/bonsai/bonsai.db-lock
rm -f /home/arjuna/Desktop/bonsai/runtime/bonsai.db
rm -f /home/arjuna/Desktop/bonsai/runtime/bonsai.db.wal
rm -f /home/arjuna/Desktop/bonsai/runtime/bonsai.db-lock
find /home/arjuna/Desktop/bonsai -name "*.db" -o -name "*.db.wal" 2>/dev/null | grep -v target | grep -v ".git"
# Expected: nothing
```

### 1d. Delete signal archives, streaming logs, queue

```bash
rm -rf /home/arjuna/Desktop/bonsai/runtime/signals/
rm -rf /home/arjuna/Desktop/bonsai/runtime/streaming/
rm -rf /home/arjuna/Desktop/bonsai/runtime/collector-queue/
```

### 1e. Delete stale logs and pidfiles

```bash
rm -f /home/arjuna/Desktop/bonsai/logs/*.log
rm -f /home/arjuna/Desktop/bonsai/runtime/*.pid
```

---

## Phase 2 — Full stop + clean slate (cloud VM)

```bash
ssh opc@150.136.208.16

cd /opt/bonsai
bash scripts/ops/teardown.sh 2>/dev/null || true

# Wipe data:
rm -rf /opt/bonsai/runtime/archive/
rm -rf /opt/bonsai/archive/
rm -f /opt/bonsai/bonsai.db /opt/bonsai/bonsai.db.wal /opt/bonsai/bonsai.db-lock
rm -f /opt/bonsai/runtime/bonsai.db /opt/bonsai/runtime/bonsai.db.wal /opt/bonsai/runtime/bonsai.db-lock
rm -rf /opt/bonsai/runtime/signals/ /opt/bonsai/runtime/streaming/ /opt/bonsai/runtime/collector-queue/
rm -f /opt/bonsai/logs/*.log /opt/bonsai/runtime/*.pid

# Confirm clean:
find /opt/bonsai -name "*.parquet" 2>/dev/null | wc -l   # Expected: 0
```

---

## Phase 3 — Destroy DC topology (Ubuntu laptop)

```bash
cd /home/arjuna/Desktop/bonsai
containerlab destroy -t lab/dc/dc-evpn-srv6.clab.yml --cleanup --graceful 2>/dev/null || true
rm -f lab/dc/ca.pem

# Confirm clab nodes gone:
docker ps --filter "name=clab-bonsai-dc" --format "{{.Names}}"
# Expected: (empty)
```

---

## Phase 4 — Destroy cloud DC topology (cloud VM)

```bash
ssh opc@150.136.208.16
cd /opt/bonsai
containerlab destroy -t lab/cloud-dc-6node.yml --cleanup --graceful 2>/dev/null || true

docker ps --filter "name=clab-bonsai-cloud-dc" --format "{{.Names}}"
# Expected: (empty)
```

---

## Phase 5 — bonsai.toml for 30-day run

Create/edit `/home/arjuna/Desktop/bonsai/bonsai.toml` (Ubuntu) and
`/opt/bonsai/bonsai.toml` (cloud VM) with these settings:

```toml
graph_path = "bonsai.db"

[archive]
enabled                = true
path                   = "runtime/archive"
flush_interval_seconds = 10
max_batch_rows         = 1000
compression_level      = 12
writer_max_idle_secs   = 7200
max_file_age_seconds   = 3600

[retention]
enabled                 = true
max_age_hours           = 720        # 30 days
max_state_change_events = 100000

[storage]
max_archive_bytes     = 107374182400  # 100 GB
max_graph_bytes       = 10737418240   # 10 GB
check_interval_secs   = 300
warn_threshold_pct    = 80

[gnn]
inference_mode            = "calibration"
threshold                 = 0.5
min_calibration_samples   = 1000
```

For DC lab topology add the `[[target]]` blocks from `docker/configs/lab-dc.toml`.
For cloud DC add the `[[target]]` blocks from `docker/configs/cloud-dc.toml`.

---

## Phase 6 — Fresh lab bringup (Ubuntu DC)

```bash
cd /home/arjuna/Desktop/bonsai

# Deploy clab topology only (no bonsai, no docker compose for bonsai):
bash scripts/lab/redeploy_dc.sh

# Verify TLS is ready (wait ~90s after deploy):
bash scripts/lab/redeploy_dc.sh --check
# Expected: all 8 clab nodes shown, CA cert present
```

---

## Phase 7 — Fresh lab bringup (cloud DC)

```bash
ssh opc@150.136.208.16 "cd /opt/bonsai && bash scripts/lab/redeploy_cloud_dc.sh"

# Verify:
ssh opc@150.136.208.16 "bash /opt/bonsai/scripts/lab/redeploy_cloud_dc.sh --check"
# Expected: all 6 clab nodes shown
```

---

## Phase 8 — Start the 30-day run

**Single command on Ubuntu laptop**:
```bash
cd /home/arjuna/Desktop/bonsai
bash scripts/ops/start_30day_run.sh
```

**Single command on cloud VM**:
```bash
cd /opt/bonsai
bash scripts/ops/start_30day_run.sh
```

The script:
1. Kills any existing bonsai / sidecar / chaos processes
2. Removes any docker bonsai containers that would conflict on port 3000
3. Starts `target/release/bonsai` as a native background process
4. Waits for `/health` to respond
5. Starts `python/collector_engine.py` (rules sidecar)
6. Waits for sidecar to register at `/api/sidecars`
7. Starts the chaos daemon against the appropriate plan for 30 days

To check status at any time:
```bash
bash scripts/ops/start_30day_run.sh --status
```

To stop everything:
```bash
bash scripts/ops/teardown.sh
```

---

## Phase 9 — Day-1 verification checklist

Run ~30 minutes after Phase 8:

```bash
# 1. Health:
curl -s http://localhost:3000/health
# Expected: {"status":"ok"}

# 2. Devices ingesting:
curl -s http://localhost:3000/api/topology | python3 -c "
import json,sys; d=json.load(sys.stdin); devs=d.get('devices',[])
print(f'devices={len(devs)} healthy={sum(1 for d in devs if d[\"health\"]==\"healthy\")}')"
# Expected: devices=8, healthy=8 (Ubuntu) or devices=6, healthy=6 (cloud)

# 3. Sidecar stats:
curl -s http://localhost:3000/api/sidecars | python3 -c "
import json,sys; sc=json.load(sys.stdin)['sidecars']
for s in sc: print(f'{s[\"name\"]} events_in={s[\"events_in_total\"]} dets_out={s[\"detections_out_total\"]}')"
# Expected: events_in > 0 and growing. After chaos injection: dets_out > 0.

# 4. Archive writing:
find runtime/archive -name "*.parquet" | wc -l
# Expected: ≥ 1 after 10s flush

# 5. Archive + subscriptions:
curl -s http://localhost:3000/api/operations | python3 -c "
import json,sys; d=json.load(sys.stdin)
print(f'archive_rows_buffered={d.get(\"archive_rows_buffered\",\"n/a\")}')
print(f'observed_subscriptions={d.get(\"observed_subscriptions\",\"n/a\")}')"

# 6. Chaos alive:
kill -0 $(cat runtime/chaos-30day.pid 2>/dev/null) && echo "chaos OK" || echo "CHAOS DEAD"
```

---

## Daily monitoring

```bash
# Daily check script (records to docs/test_results/daily_runs/YYYY-MM-DD.md):
bash scripts/bv5_daily_check.sh

# Quick status:
bash scripts/ops/start_30day_run.sh --status

# Archive growth:
du -sh runtime/archive/
find runtime/archive -name "*.parquet" | wc -l

# Log tails:
tail -50 logs/chaos-30day.log
tail -20 logs/bonsai-sidecar.log | grep -i "detect\|error\|warn"
```

---

## Phase 10 — D2-12 GNN trigger check (T+30 days)

```bash
# 1. Archive depth:
OLDEST=$(find runtime/archive -name "*.parquet" -printf "%T@\n" 2>/dev/null | sort -n | head -1)
NOW=$(date +%s)
python3 -c "print(f'Archive depth: {round(($NOW - $OLDEST) / 86400, 1)} days')"
# Required: ≥ 30

# 2. Chaos injection count:
grep -c "injected" logs/chaos-30day.log 2>/dev/null
# Required: ≥ 500

# 3. Per-rule detection counts (from archive, not live API):
bash scripts/check_training_readiness.py 2>/dev/null || \
curl -s http://localhost:3000/api/detections | python3 -c "
import json,sys,collections
dets = json.load(sys.stdin).get('detections', [])
counts = collections.Counter(d['rule_id'] for d in dets)
qualifying = [r for r,c in counts.items() if c >= 50]
print(f'Rules with >=50 examples: {qualifying}')
print('GNN trigger MET' if len(qualifying) >= 3 else 'GNN trigger NOT YET MET')"
```

If all three conditions are met, proceed to D2-13:

```bash
.venv/bin/python python/bonsai_ml/gnn/archive_to_training.py \
  --archive-path runtime/archive \
  --output python/bonsai_ml/model_cards/gnn_v1_dataset.json

.venv/bin/python python/bonsai_ml/gnn/train.py \
  --dataset python/bonsai_ml/model_cards/gnn_v1_dataset.json \
  --output models/gnn_v1.pt
```

---

## Summary

| Metric | Target |
|---|---|
| Archive depth | 30 calendar days |
| Parquet files | ~720 (hourly rotation) |
| Telemetry rows | ~10M+ |
| Chaos injections | ≥ 500 |
| Labeled detection examples | ≥ 50 per active rule_id |
| GNN model output | `models/gnn_v1.pt` + model card |

**The single script for everything after lab bringup**:
```bash
bash scripts/ops/start_30day_run.sh          # start
bash scripts/ops/start_30day_run.sh --status  # check
bash scripts/ops/teardown.sh                  # stop
```
