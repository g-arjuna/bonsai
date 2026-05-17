# 30-Day Archive Run — GNN Training Data Collection (DV2)

> **North star**: `models/gnn_v1.pt` — first trained GNN anomaly detector.  
> **Key invariant**: Bonsai is always a **native process**. Docker is only used by ContainerLab for SRL nodes.  
> **Cloud target**: Ubuntu 22.04 ARM Always Free OCI instance (`ubuntu@<cloud-vm-ip>`).

---

## Script map — the ONLY scripts you need

| Script | When |
|---|---|
| `bash scripts/ops/rebuild_and_validate.sh` | After every `git pull` |
| `bash scripts/lab/redeploy_dc.sh` | Once — fresh clab topology (Ubuntu laptop) |
| `bash scripts/lab/redeploy_cloud_dc.sh` | Once — fresh clab topology (cloud VM) |
| `bash scripts/ops/start_30day_run.sh` | **Start** bonsai + sidecar + chaos |
| `bash scripts/ops/run_status.sh` | **Check** — visual pass/warn/fail (11 checks) |
| `bash scripts/ops/run_status.sh --watch` | **Monitor** — live refresh every 10s |
| `bash scripts/ops/teardown.sh` | **Stop** everything cleanly |
| `bash scripts/chaos_runner.sh --status` | Chaos-only status |
| `bash scripts/ops/event_detection_retirement_gate.sh` | D2-1 T1 prerequisite gate |

**Never run for data collection**: `scripts/cloud/deploy.sh`, `docker compose up`,
`scripts/e2e_*`, `scripts/sprint*_preflight.sh`.

---

## Versioning

`Cargo.toml` version = `0.2.x` for DV2 sprint. Every binary bakes in git SHA + build timestamp.

- `/health` returns `{"version": "0.2.0", "git_sha": "abcd1234", ...}`
- Bonsai UI sidebar shows: `● v0.2.0 · abcd1234`
- Before starting a run: confirm UI SHA matches `git rev-parse --short=8 HEAD`

---

## Prerequisite — D2-1 T1 gate (run once, before the 30-day run)

The Rust detection fastpath must be retired so sidecar detections are correctly labelled for GNN training.

```bash
# On Ubuntu, lab up, bonsai NOT running:
bash scripts/ops/event_detection_retirement_gate.sh
# Required exit code: 0
```

If not done: `sidecar.detections_out_total` stays 0, producing unlabelled training data.

---

## Phase 0 — Pull + build (both machines)

```bash
git pull --rebase origin main
bash scripts/ops/rebuild_and_validate.sh
# Required: PASS>=14, FAIL=0
```

Confirm: UI sidebar SHA matches `git rev-parse --short=8 HEAD`.

---

## Phase 1 — Clean slate (Ubuntu laptop)

```bash
cd /home/arjuna/Desktop/bonsai

bash scripts/ops/teardown.sh
ss -tlnp | grep 3000 || echo "OK: port 3000 free"

rm -rf runtime/archive/ archive/
rm -f bonsai.db bonsai.db.wal bonsai.db-lock
rm -f runtime/bonsai.db runtime/bonsai.db.wal runtime/bonsai.db-lock
rm -rf runtime/signals/ runtime/streaming/ runtime/collector-queue/
rm -f logs/*.log runtime/*.pid runtime/chaos_runner.pid runtime/chaos_log.jsonl

find . -name "*.parquet" 2>/dev/null | grep -v target | wc -l   # Expected: 0
find . -name "*.db" 2>/dev/null | grep -v target                # Expected: nothing
```

---

## Phase 2 — Clean slate (cloud VM)

```bash
ssh ubuntu@<cloud-vm-ip>
cd /opt/bonsai

bash scripts/ops/teardown.sh 2>/dev/null || true
rm -rf runtime/archive/ archive/
rm -f bonsai.db bonsai.db.wal bonsai.db-lock
rm -f runtime/bonsai.db runtime/bonsai.db.wal runtime/bonsai.db-lock
rm -rf runtime/signals/ runtime/streaming/ runtime/collector-queue/
rm -f logs/*.log runtime/*.pid runtime/chaos_runner.pid runtime/chaos_log.jsonl

find . -name "*.parquet" 2>/dev/null | wc -l   # Expected: 0
```

---

## Phase 3 — Redeploy clab topology (Ubuntu laptop)

```bash
containerlab destroy -t lab/dc/dc-evpn-srv6.clab.yml --cleanup --graceful 2>/dev/null || true
rm -f lab/dc/ca.pem
docker ps --filter "name=clab-bonsai-dc" --format "{{.Names}}"  # Expected: empty

bash scripts/lab/redeploy_dc.sh
sleep 90 && bash scripts/lab/redeploy_dc.sh --check
```

Verify: 8 clab nodes shown, CA cert present at `lab/dc/clab-bonsai-dc/.tls/ca/ca.pem`.

---

## Phase 4 — Redeploy clab topology (cloud VM)

```bash
ssh ubuntu@<cloud-vm-ip>
cd /opt/bonsai
containerlab destroy -t lab/cloud-dc-6node.yml --cleanup --graceful 2>/dev/null || true
bash scripts/lab/redeploy_cloud_dc.sh
sleep 90 && bash scripts/lab/redeploy_cloud_dc.sh --check
```

Verify: 6 clab nodes shown.

---

## Phase 5 — bonsai.toml (both machines)

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
max_age_hours           = 720
max_state_change_events = 100000

[storage]
max_archive_bytes     = 107374182400
max_graph_bytes       = 10737418240
check_interval_secs   = 300
warn_threshold_pct    = 80

[gnn]
inference_mode          = "calibration"
threshold               = 0.5
min_calibration_samples = 1000
```

Add `[[target]]` blocks from `docker/configs/lab-dc.toml` (Ubuntu) or `docker/configs/cloud-dc.toml` (cloud).

---

## Phase 6 — Start the run

**Same command on Ubuntu laptop and cloud VM:**

```bash
bash scripts/ops/start_30day_run.sh
```

What it does (in order):

1. Stops any existing bonsai/sidecar/chaos processes
2. Removes stale docker bonsai containers (port 3000 conflict prevention)
3. Pre-flight: binary + bonsai.toml existence check
4. Starts `target/release/bonsai` natively, waits for `/health`
5. Starts `python/collector_engine.py` (rules sidecar), waits for registration
6. Starts `scripts/chaos_runner.sh` (30-day duration)

To stop the run at any time:

```bash
bash scripts/ops/teardown.sh
```

---

## Phase 7 — Verify immediately + every day

```bash
bash scripts/ops/run_status.sh
```

Expected output (all green):

```
── 1. Binary ──────────────────────────────────────────
  ✓ PASS  target/release/bonsai exists (built Xh ago)
── 2. bonsai process ──────────────────────────────────
  ✓ PASS  running (pid XXXXX)
── 3. /health ─────────────────────────────────────────
  ✓ PASS  status=ok  version=0.2.0 abcd1234
── 4. Rules sidecar ───────────────────────────────────
  ✓ PASS  running (pid XXXXX)
          rules-local: events_in=NNN  dets_out=N  status=active
── 5. ContainerLab topology ───────────────────────────
  ✓ PASS  8 clab nodes running
── 6. gNMI subscriptions ──────────────────────────────
  ✓ PASS  devices=8  healthy=8  warn=0  critical=0
── 7. Detections ──────────────────────────────────────
  ✓ PASS  total=N  rules={...}
── 8. Archive ─────────────────────────────────────────
  ✓ PASS  N parquet files  size=X  newest=Nm ago
── 9. Chaos daemon ────────────────────────────────────
  ✓ PASS  running (pid XXXXX)
── 10. bonsai.toml ────────────────────────────────────
  ✓ PASS  archive enabled  max_age_hours=720  gnn.inference_mode=calibration
── 11. Port conflicts ─────────────────────────────────
  ✓ PASS  port 3000 in use by non-docker process
  PASS=11  WARN=0  FAIL=0
  All checks green — 30-day run is healthy.
```

**Expected WARNs (non-blocking on day 1):**
- `dets_out=0` in first 30 min before chaos injects — normal
- `archive depth: <1 day` — normal on day 1

**FAILs require action before the run is valid** — any FAIL in checks 1–6 means data is not being collected.

---

## Daily monitoring

```bash
# Visual check:
bash scripts/ops/run_status.sh

# Watch mode (stays open, refreshes every 10s):
bash scripts/ops/run_status.sh --watch

# Chaos only:
bash scripts/chaos_runner.sh --status
```

---

## Troubleshooting quick-reference

| Symptom | Root cause | Fix |
|---|---|---|
| Port 3000 blocked | Stale docker bonsai container | `docker rm -f bonsai-bonsai-lab-dc-1` |
| sidecar DEAD after start | Python crash | `tail -50 logs/bonsai-sidecar.log` |
| `dets_out=0` long-term | `vendor=""` in device lookup | check `python/bonsai_sdk/client.py` `device_vendor()` |
| Detections in API but `dets_out=0` | Rust fastpath not retired | run `event_detection_retirement_gate.sh` |
| Archive not writing | `[archive] enabled` missing | check `bonsai.toml` |
| Chaos not injecting | Plan not found or clab down | `bash scripts/chaos_runner.sh --status` |
| TLS errors on gNMI | Cert split-brain | re-run `scripts/lab/redeploy_dc.sh` |
| Build fails on cloud | Wrong OS (not Ubuntu ARM) | provision Ubuntu 22.04 ARM OCI instance |

---

## Phase 8 — GNN trigger check (T+30 days)

```bash
bash scripts/ops/run_status.sh   # must be all green

# 1. Archive depth (required >=30 days):
python3 -c "
import glob, os, time
files = glob.glob('runtime/archive/**/*.parquet', recursive=True)
oldest = min(os.path.getmtime(f) for f in files) if files else 0
print(f'{len(files)} files, depth={(time.time()-oldest)/86400:.1f}d')
"

# 2. Chaos injections (required >=500):
python3 -c "
import json
n = sum(1 for line in open('runtime/chaos_log.jsonl')
        if json.loads(line).get('event_type') == 'inject')
print(f'Injections: {n}')
" 2>/dev/null || echo "check runtime/chaos_log.jsonl"

# 3. Per-rule detection counts (required >=50 per rule for >=3 rules):
curl -s http://localhost:3000/api/detections | python3 -c "
import json,sys,collections
dets=json.load(sys.stdin).get('detections',[])
counts=collections.Counter(d['rule_id'] for d in dets)
q=[r for r,c in counts.items() if c>=50]
print('counts:', dict(counts))
print('TRIGGER MET' if len(q)>=3 else 'NOT YET MET', 'qualifying:', q)
"
```

If all three conditions met: proceed to D2-13 (GNN training). See `python/bonsai_ml/`.

---

## Summary: the 5 commands for a complete fresh run

```bash
# 0. Pull and build
git pull --rebase origin main && bash scripts/ops/rebuild_and_validate.sh

# 1–4. Wipe + redeploy (phases 1-4 above, one-time setup)

# 5. Start
bash scripts/ops/start_30day_run.sh

# 6. Verify (run now, then daily)
bash scripts/ops/run_status.sh

# 7. Stop when needed
bash scripts/ops/teardown.sh
```
