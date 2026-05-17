# 30-Day Archive Run — GNN Training Data Collection

> Objective: accumulate 30 days of clean telemetry + chaos injection data
> across both labs (Ubuntu DC lab + cloud DC) to meet the D2-12 GNN trigger
> condition and produce the first GNN training dataset.
>
> Start date target: day after this doc is executed.
> End date target: T+30 days.
> Expected output: ≥ 500 chaos-labeled examples, ≥ 50 per rule_id.

---

## Phase 0 — Pull latest and verify build

Run on **Ubuntu** before anything else:

```bash
cd /home/arjuna/Desktop/bonsai
git pull --rebase origin main
bash scripts/ops/rebuild_and_validate.sh
# Expected: PASS=14+, FAIL=0
```

---

## Phase 1 — Full stop and clean slate

### 1a. Stop all running bonsai processes and containers

```bash
cd /home/arjuna/Desktop/bonsai

# Stop native bonsai (if running outside docker)
bash scripts/ops/teardown.sh 2>/dev/null || true

# Stop ALL docker compose profiles
docker compose --profile lab-dc stop 2>/dev/null || true
docker compose --profile cloud-dc stop 2>/dev/null || true
docker compose --profile all stop 2>/dev/null || true

# Confirm nothing is on port 3000
ss -tlnp | grep 3000 || echo "port 3000 free"
```

### 1b. Delete ALL archive data (parquet files)

> ⚠️ This is irreversible. It discards all previously accumulated archive data.
> The intent is a clean 30-day run with known-good, fully-labelled data.

```bash
# Archive directories used by native bonsai and docker lab-dc profile:
rm -rf /home/arjuna/Desktop/bonsai/runtime/archive/
rm -rf /home/arjuna/Desktop/bonsai/archive/

# Docker volume archive (lab-dc profile writes to a named volume):
docker volume ls | grep bonsai
# For each volume that looks like bonsai_archive or bonsai_runtime:
docker volume rm bonsai_archive 2>/dev/null || true
docker volume rm bonsai_lab-dc_archive 2>/dev/null || true

# Confirm empty:
find /home/arjuna/Desktop/bonsai -name "*.parquet" 2>/dev/null | wc -l
# Expected: 0
```

### 1c. Delete the graph database and WAL files

> This wipes all graph state: Device nodes, DetectionEvents, incidents,
> subscriptions, embeddings. Bonsai will rebuild from scratch on first boot.

```bash
# Native bonsai graph:
rm -f /home/arjuna/Desktop/bonsai/bonsai.db
rm -f /home/arjuna/Desktop/bonsai/bonsai.db.wal
rm -f /home/arjuna/Desktop/bonsai/bonsai.db-lock

# Runtime dir graph (used by docker lab-dc profile):
rm -f /home/arjuna/Desktop/bonsai/runtime/bonsai.db
rm -f /home/arjuna/Desktop/bonsai/runtime/bonsai.db.wal
rm -f /home/arjuna/Desktop/bonsai/runtime/bonsai.db-lock

# Docker named volume for graph (if present):
docker volume rm bonsai_graph 2>/dev/null || true

# Confirm:
find /home/arjuna/Desktop/bonsai -name "*.db" -o -name "*.db.wal" 2>/dev/null | grep -v target | grep -v ".git"
# Expected: nothing
```

### 1d. Delete syslog / SNMP / BMP JSONL archives

```bash
rm -f /home/arjuna/Desktop/bonsai/runtime/signals/syslog.jsonl
rm -f /home/arjuna/Desktop/bonsai/runtime/signals/snmp.jsonl
rm -f /home/arjuna/Desktop/bonsai/runtime/streaming/bmp.jsonl
rm -f /home/arjuna/Desktop/bonsai/runtime/streaming/bgp_ls.jsonl
rm -rf /home/arjuna/Desktop/bonsai/runtime/signals/
rm -rf /home/arjuna/Desktop/bonsai/runtime/streaming/
```

### 1e. Delete collector queue (if present)

```bash
rm -rf /home/arjuna/Desktop/bonsai/runtime/collector-queue/
```

### 1f. Delete stale logs and pidfiles

```bash
rm -f /home/arjuna/Desktop/bonsai/logs/*.log
rm -f /home/arjuna/Desktop/bonsai/runtime/*.pid
```

---

## Phase 2 — Clean lab teardown (Ubuntu DC lab)

### 2a. Destroy the DC topology completely

```bash
cd /home/arjuna/Desktop/bonsai
containerlab destroy -t lab/dc/dc-evpn-srv6.clab.yml --cleanup --graceful 2>/dev/null || true

# Confirm all clab-bonsai-dc-* containers gone:
docker ps --filter "name=clab-bonsai-dc" --format "{{.Names}}"
# Expected: (empty)

# Confirm .tls/ directory wiped (--cleanup removes it):
ls lab/dc/clab-bonsai-dc/ 2>/dev/null || echo "directory gone — good"
```

### 2b. Remove the stale CA cert

```bash
rm -f /home/arjuna/Desktop/bonsai/lab/dc/ca.pem
```

---

## Phase 3 — Clean lab teardown (cloud DC)

Run on the **cloud VM** (`ssh opc@150.136.208.16`):

```bash
cd /opt/bonsai
git pull --rebase origin main

# Stop bonsai
docker compose --profile cloud-dc stop 2>/dev/null || true
bash scripts/ops/teardown.sh 2>/dev/null || true

# Destroy cloud topology
containerlab destroy -t lab/cloud-dc-6node.yml --cleanup --graceful 2>/dev/null || true

# Confirm containers gone:
docker ps --filter "name=clab-bonsai-cloud-dc" --format "{{.Names}}"
# Expected: (empty)

# Wipe all data (same as Phase 1):
rm -rf /opt/bonsai/runtime/archive/
rm -rf /opt/bonsai/archive/
rm -f /opt/bonsai/runtime/bonsai.db
rm -f /opt/bonsai/runtime/bonsai.db.wal
rm -f /opt/bonsai/runtime/bonsai.db-lock
rm -f /opt/bonsai/bonsai.db
rm -f /opt/bonsai/bonsai.db.wal
rm -f /opt/bonsai/logs/*.log
rm -f /opt/bonsai/runtime/*.pid
rm -rf /opt/bonsai/runtime/signals/
rm -rf /opt/bonsai/runtime/streaming/
rm -rf /opt/bonsai/runtime/collector-queue/

# Remove stale CA cert:
rm -f /opt/bonsai/lab/cloud-dc-6node-ca.pem

# Confirm clean:
find /opt/bonsai -name "*.parquet" 2>/dev/null | wc -l  # Expected: 0
find /opt/bonsai -name "*.db" 2>/dev/null | grep -v target  # Expected: nothing
```

---

## Phase 4 — Fresh lab bringup (Ubuntu DC lab)

```bash
cd /home/arjuna/Desktop/bonsai

# Deploy fresh DC topology (generates new CA + certs, copies ca.pem):
bash scripts/lab/redeploy_dc.sh

# Wait for SRL nodes to fully boot (~90s), then verify:
bash scripts/lab/redeploy_dc.sh --check
# Expected: observed_subscriptions ≥ 8
```

Verify bonsai is ingesting:
```bash
curl -s http://localhost:3000/api/topology | python3 -c "
import json,sys; d=json.load(sys.stdin)
devs=d.get('devices',[])
print(f'devices={len(devs)}')
for dev in devs: print(f'  {dev[\"hostname\"]:20} vendor={dev[\"vendor\"]} health={dev[\"health\"]}')
"
```

Expected: 8 devices, all `nokia_srl`, health `healthy`.

---

## Phase 5 — Fresh lab bringup (cloud DC)

```bash
ssh opc@150.136.208.16 "cd /opt/bonsai && bash scripts/lab/redeploy_cloud_dc.sh"

# Verify from cloud VM:
ssh opc@150.136.208.16 "curl -s http://localhost:3000/api/topology | python3 -c \"
import json,sys; d=json.load(sys.stdin)
print(f'devices={len(d.get(\\\"devices\\\",[]))}')\""
```

Expected: 6 devices.

---

## Phase 6 — bonsai.toml tuning for 30-day archive run

Edit (or create) `/home/arjuna/Desktop/bonsai/bonsai.toml` with these settings:

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
max_age_hours           = 720        # 30 days (30 × 24)
max_state_change_events = 100000     # raise cap for 30-day volume

[storage]
max_archive_bytes     = 107374182400  # 100 GB — generous for 30-day parquet
max_graph_bytes       = 10737418240   # 10 GB graph cap
check_interval_secs   = 300
warn_threshold_pct    = 80

[gnn]
inference_mode            = "calibration"
threshold                 = 0.5
min_calibration_samples   = 1000

[lab]
topology     = "dc"
mgmt_subnet  = "172.100.103.0/24"
```

For the cloud DC, update `/opt/bonsai/docker/configs/cloud-dc.toml` (or the active config file) with the same `[archive]`, `[retention]`, and `[gnn]` stanzas.

---

## Phase 7 — Start bonsai (native, Ubuntu lab)

```bash
cd /home/arjuna/Desktop/bonsai
bash scripts/ops/rebuild_and_validate.sh --skip-build   # confirm clean start

# OR start directly:
BONSAI_REQUIRE_SIDECAR=rules \
  ./target/release/bonsai --config bonsai.toml \
  >> logs/bonsai.log 2>&1 &
echo $! > runtime/bonsai.pid

# Start sidecar:
.venv/bin/python python/collector_engine.py \
  >> logs/bonsai-sidecar.log 2>&1 &
echo $! > runtime/bonsai-sidecar.pid

# Confirm healthy:
sleep 5 && curl -s http://localhost:3000/health
# Expected: {"status":"ok"}
```

---

## Phase 8 — Start the chaos daemon

The chaos daemon runs continuously for 30 days, injecting faults and recovering them:

```bash
cd /home/arjuna/Desktop/bonsai

# Primary: always-on chaos against DC lab
nohup .venv/bin/python tests/chaos_harness/run.py \
  --plan chaos_plans/always_on_dc.yaml \
  --duration $((30 * 24 * 3600)) \
  >> logs/chaos-30day.log 2>&1 &
echo $! > runtime/chaos-30day.pid

echo "Chaos daemon started: PID $(cat runtime/chaos-30day.pid)"
echo "Log: logs/chaos-30day.log"
echo "Tail: tail -f logs/chaos-30day.log"
```

On cloud (run from cloud VM):
```bash
cd /opt/bonsai
nohup .venv/bin/python tests/chaos_harness/run.py \
  --plan chaos_plans/always_on_cloud_dc.yaml \
  --duration $((30 * 24 * 3600)) \
  >> logs/chaos-cloud-30day.log 2>&1 &
echo $! > runtime/chaos-cloud-30day.pid
```

---

## Phase 9 — Day-1 verification checklist

Run 30 minutes after Phase 7–8 complete:

```bash
# 1. Devices healthy:
curl -s http://localhost:3000/api/topology | python3 -c "
import json,sys; d=json.load(sys.stdin); devs=d.get('devices',[])
print(f'devices={len(devs)} healthy={sum(1 for d in devs if d[\"health\"]==\"healthy\")}')"

# 2. Sidecar receiving events:
curl -s http://localhost:3000/api/sidecars | python3 -c "
import json,sys; sc=json.load(sys.stdin)['sidecars']
for s in sc: print(f'{s[\"name\"]} events_in={s[\"events_in_total\"]} dets_out={s[\"detections_out_total\"]}')"
# Expected: events_in > 0 and growing

# 3. Archive files being written:
find runtime/archive -name "*.parquet" | wc -l
# Expected: ≥ 1 after first flush (10s)

# 4. Detections firing from sidecar (not Rust fastpath):
# After first chaos injection, detections_out_total should increment.
# If it stays 0 while /api/detections shows detections — D2-1 T1 is blocking.

# 5. Archive rows present:
curl -s http://localhost:3000/api/operations | python3 -c "
import json,sys; d=json.load(sys.stdin)
print(f'archive_rows_buffered={d.get(\"archive_rows_buffered\",\"n/a\")}')
print(f'observed_subscriptions={d.get(\"observed_subscriptions\",\"n/a\")}')"
```

---

## Phase 10 — D2-12 GNN trigger check (run at T+30 days)

```bash
# 1. Archive depth check:
OLDEST=$(find runtime/archive -name "*.parquet" -printf "%T@\n" 2>/dev/null | sort -n | head -1)
NOW=$(date +%s)
DAYS=$(python3 -c "print(round(($NOW - $OLDEST) / 86400, 1))")
echo "Archive depth: ${DAYS} days"
# Required: ≥ 30

# 2. Chaos injection count:
grep -c "injected" logs/chaos-30day.log 2>/dev/null || echo "check chaos log"
# Required: ≥ 500

# 3. Per-rule detection counts:
curl -s http://localhost:3000/api/detections | python3 -c "
import json,sys,collections
dets = json.load(sys.stdin).get('detections', [])
counts = collections.Counter(d['rule_id'] for d in dets)
print('rule_id counts:')
for rule, cnt in sorted(counts.items()): print(f'  {rule}: {cnt}')
print()
qualifying = [r for r,c in counts.items() if c >= 50]
print(f'Rules with ≥50 examples: {qualifying}')
if len(qualifying) >= 3:
    print('✓ GNN trigger condition MET')
else:
    print('✗ GNN trigger condition NOT YET MET')
"
```

If trigger is met, proceed to D2-13:

```bash
# Run first GNN training cycle:
.venv/bin/python python/bonsai_ml/gnn/archive_to_training.py \
  --archive-path runtime/archive \
  --output python/bonsai_ml/model_cards/gnn_v1_dataset.json

# Training:
.venv/bin/python python/bonsai_ml/gnn/train.py \
  --dataset python/bonsai_ml/model_cards/gnn_v1_dataset.json \
  --output models/gnn_v1.pt

# Write model card:
# python/bonsai_ml/model_cards/gnn_v1.md
```

---

## Daily monitoring (optional, while run is in progress)

```bash
# Quick daily check — run or cron on Ubuntu:
bash scripts/bv5_daily_check.sh

# Check chaos is still alive:
kill -0 $(cat runtime/chaos-30day.pid 2>/dev/null) && echo "chaos OK" || echo "CHAOS DEAD — restart"

# Archive growth:
du -sh runtime/archive/
find runtime/archive -name "*.parquet" | wc -l

# Log tail:
tail -50 logs/chaos-30day.log
tail -50 logs/bonsai-sidecar.log | grep "detection\|error\|WARN"
```

---

## Summary: what a clean run produces

| Metric | Target |
|---|---|
| Archive depth | 30 calendar days |
| Parquet files | ~720 (hourly rotation) |
| Telemetry rows | ~10M+ (gNMI counter + state updates) |
| Chaos injections | ≥ 500 (BGP flap, interface down, BFD, link loss) |
| Detection examples (labeled) | ≥ 50 per active rule_id |
| GNN model output | `models/gnn_v1.pt` + model card |

This data forms the permanent training baseline for all future GNN versions.
