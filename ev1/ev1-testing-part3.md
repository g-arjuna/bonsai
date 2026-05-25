# EV1 Ubuntu Testing Guide — Part 3: BonPy UI, Rule Management & End-to-End Validation

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
