# AI Feedback Protocol — Worked Examples

This document shows three concrete examples of how an AI session consumes the structured feedback output from bonsai's testing drivers to diagnose problems without operator narration. Read `docs/ai_feedback_protocol.md` first for the full schema.

---

## Example 1 — Diagnosing a BGP detection regression

**Scenario**: A developer changes `src/rules/bgp.rs` and opens a PR. CI runs the chaos harness. The AI session is asked "why did CI fail?".

**Step 1 — read the chaos matrix** from `runtime/driver_results/chaos.json` (or `/api/_test/status`):

```json
{
  "driver": "chaos",
  "passed": 7,
  "failed": 3,
  "total": 10,
  "matrix": [
    { "id": "dc-bgp-evpn-session-down-leaf3-super1", "passed": false, "latency_ms": 92000, "error": "" },
    { "id": "dc-bgp-evpn-all-sessions-down-leaf4",   "passed": false, "latency_ms": 92000, "error": "" },
    { "id": "sp-bgp-vpn-session-down-pe1-rr1",       "passed": false, "latency_ms": 92000, "error": "" },
    { "id": "dc-link-impairment-leaf1-spine1",        "passed": true,  "latency_ms": 45200, "error": "" },
    ...
  ]
}
```

**AI reads**: Three faults failed — all BGP-type. Link impairment (non-BGP) passed. The failure pattern is narrowly "BGP session down detection" across both topologies.

**Step 2 — check the detection output** from `/api/detections` or the event driver log:

```json
{
  "detections": []
}
```

No detections emitted even though the fault was injected and the BGP session was confirmed down (via `check_lab.sh` output showing `bgp_established_super1: 6` instead of `7`).

**Step 3 — inspect the git diff** for `src/rules/bgp.rs`:

```diff
-    if session.state == "idle" || session.state == "active" {
+    if session.state == "idle" {
```

**AI diagnosis**: The BGP rule now only fires on `"idle"` state, but ContainerLab SR Linux emits `"active"` when a session drops after `neighbor admin-state disable`. The regression narrowed the match condition. Fix: restore the `|| session.state == "active"` branch.

**What made this diagnosable without operator input**:
- The chaos matrix immediately showed which fault IDs regressed and their pattern (all BGP, none non-BGP)
- The event driver log confirmed zero detections emitted during the inject window
- `check_lab.sh` confirmed the fault was actually injected (lab side worked, bonsai side silent)
- The git diff was the only remaining variable

---

## Example 2 — Diagnosing a silent enricher failure

**Scenario**: NetBox enrichment is added to a PR. The AI session is asked "is enrichment working?".

**Step 1 — read the unified status endpoint**:

```bash
curl http://localhost:3000/api/_test/status | jq .external_health
```

```json
{
  "netbox": {
    "reachable": true,
    "seeded": true,
    "device_count": 8
  }
}
```

NetBox is up and seeded. Good.

**Step 2 — read the event driver timeline** from `runtime/driver_results/events.json`:

```json
{
  "timeline": [
    { "ts_offset_ms": 0,    "event": "TelemetryUpdate", "target": "srl-leaf1", "path": "/interface[name=ethernet-1/1]/oper-status" },
    { "ts_offset_ms": 1200, "event": "TelemetryUpdate", "target": "srl-leaf2", "path": "/interface[name=ethernet-1/1]/oper-status" },
    { "ts_offset_ms": 4800, "event": "EnrichmentRun",   "target": "srl-leaf1", "enricher": "netbox", "duration_ms": 312, "result": "ok", "edges_created": 0 },
    ...
  ]
}
```

**AI reads**: EnrichmentRun fired for srl-leaf1, result is `"ok"`, but `edges_created: 0`. NetBox has 8 devices seeded and the enricher ran without error — yet no graph edges were created.

**Step 3 — check the topology response** to see if any enrichment data reached the graph:

```bash
curl http://localhost:3000/api/topology | jq '.devices[] | select(.hostname=="srl-leaf1") | .netbox_site'
```

```
null
```

**Step 4 — check the NetBox seed data** against the bonsai hostname format:

```bash
curl http://localhost:8000/api/dcim/devices/?name=srl-leaf1 -H "Authorization: Token bonsai-dev-token" | jq '.count'
```

```
0
```

NetBox has `srl-leaf1` seeded as `"srl-leaf1"` but the ContainerLab topology creates it as `"clab-bonsai-dc-srl-leaf1"`. The enricher matched on hostname and found nothing.

**AI diagnosis**: Hostname mismatch between ContainerLab container names and `lab/seed/topology.yaml`. The seed uses short names (`srl-leaf1`) but bonsai reports the full ContainerLab FQDN `clab-bonsai-dc-srl-leaf1`. Either the seed needs to use full names, or the enricher needs hostname normalisation (strip `clab-bonsai-dc-` prefix).

**What made this diagnosable without operator input**:
- `external_health` confirmed NetBox was healthy and seeded — eliminated infrastructure as the cause
- The event timeline showed enrichment ran and returned `"ok"` — eliminated a crash
- `edges_created: 0` in the structured event narrowed to a data-match problem
- One API call to NetBox confirmed the hostname mismatch

---

## Example 3 — Diagnosing memory growth during a chaos run

**Scenario**: After running the full DC chaos suite (12 faults, inject+heal×12), an operator reports bonsai is using 2.3 GB RSS. The AI session is asked "is memory within budget?".

**Step 1 — read the memory metrics** from `/api/_test/status`:

```json
{
  "memory_metrics": {
    "rss_mb": 2340,
    "budget_mb": 1000,
    "over_budget": true,
    "breakdown": {
      "event_bus_queue_depth": 18420,
      "counter_summarizer_entries": 142,
      "archive_open_writers": 3,
      "lbug_cache_mb": 48
    }
  }
}
```

**AI reads**: RSS is 2.3× over the 1 GB budget. The breakdown shows `event_bus_queue_depth: 18420` — the broadcast channel has ~18k messages queued, which is far above the 2048 capacity per subscriber. This indicates a slow subscriber lagging and causing backpressure.

**Step 2 — check the SSE subscriber count** in the event driver log:

```json
{
  "sse_subscriber_count": 1,
  "sse_subscriber_lag_events": 17800
}
```

One SSE subscriber (the Playwright UI driver running during chaos testing) accumulated 17,800 event lag entries.

**Step 3 — check the config** for slow-subscriber threshold:

```toml
[event_bus]
slow_subscriber_threshold = 1024
```

The threshold is 1024 but the Playwright driver wasn't closing its SSE connection between fault injections — it accumulated lag across all 12 faults. At ~1500 events/fault × 12 faults = 18,000 events queued.

**AI diagnosis**: The SSE subscriber (UI driver) was not disconnecting between fault injections, causing the event bus to hold 18k events. The fix is either: (1) close the SSE connection between test runs in the Playwright driver, or (2) drop lagging SSE subscribers when they exceed the threshold. The memory budget violation is a test-harness artifact, not a production regression — but the slow-subscriber drop logic (T4-3 from the backlog) should be implemented to handle this automatically.

**Action items**:
1. In `tests/ui_driver/topology.spec.js`: close SSE connection between test steps
2. In `src/event_bus.rs`: implement slow-subscriber drop when lag > `slow_subscriber_threshold` for > 30s (T4-3)

**What made this diagnosable without operator input**:
- `/api/_test/status` returned the full breakdown including `event_bus_queue_depth`
- The specific number (18,420 ≈ 1500 × 12) immediately pointed to accumulated across fault iterations
- `sse_subscriber_lag_events` in the event driver output confirmed the specific subscriber
- No log scraping, no operator narration — three JSON fields told the complete story

---

## Key patterns these examples demonstrate

1. **Start with the matrix, not the logs.** Chaos matrix shows which fault IDs regressed and their pattern. Pattern recognition (all BGP? all enrichment? memory?) directs the next step.

2. **`edges_created: 0` with `result: "ok"` always means a data-match problem.** The enricher ran successfully but matched nothing. The data doesn't match — check hostnames, IDs, or field formats.

3. **Memory over budget + high queue depth = slow subscriber.** The queue depth breakdown (`event_bus_queue_depth`) is the fastest path to diagnosing memory growth.

4. **`check_lab.sh` is the ground truth for fault injection.** If a chaos fault fails but `check_lab.sh` shows the lab is healthy and the fault was injected, the problem is in bonsai's detection logic. If `check_lab.sh` shows nodes down, the lab itself failed to inject.

5. **The event timeline is a causal chain.** Events are time-sorted: `TelemetryUpdate → EnrichmentRun → DetectionEvent → RemediationProposal`. A gap in the chain (e.g., `TelemetryUpdate` present but no `DetectionEvent`) identifies exactly which stage broke.
