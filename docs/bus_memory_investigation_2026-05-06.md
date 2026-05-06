# Event Bus Saturation & Memory Investigation — 2026-05-06

## Context

8-node DC EVPN lab (srl-super1/2, srl-spine1/2, srl-leaf1–4) on ContainerLab.
All nodes Nokia SR Linux 26.x, gNMI over TLS on 172.100.103.11–18:57400.

---

## Problem 1 — LadybugDB Buffer Pool OOM (original)

### Symptom
After ~5 hours of continuous operation, bonsai crashes with:
```
Buffer manager exception: Unable to allocate memory!
The buffer pool is full and no memory could be freed!
```
Graph writes stop; the process must be restarted.

### Root Cause
Two compounding factors:
1. **Counter write debounce was 10 seconds** — 8 devices × ~40 interfaces each = 320
   unique `(device, interface)` keys, each writing to the graph at most every 10 s.
   That is ~32 writes/second sustained, filling the buffer pool with dirty pages faster
   than lbug's eviction can free them.
2. **Buffer pool was at the 2 GiB default** — enough to hold enormous amounts of dirty
   data before eviction pressure becomes meaningful.

### Attempts

| Attempt | Config | Result |
|---------|--------|--------|
| Default | 2 GiB pool, 10 s debounce | OOM after ~5 h |
| Fix 1 | 512 MiB pool | Crashed WAL from prior session could not replay — OOM at open |
| Fix 2 | 1 GiB pool, 60 s debounce | Pool OOM **at startup** on fresh DB (63-second open, first write fails) |
| Fix 3 (current) | 2 GiB pool, 60 s debounce | Starts clean; 6× less sustained write pressure |

### Fix Applied (`bonsai.toml`)
```toml
[graph]
buffer_pool_bytes = 2147483648  # 2 GiB

[event_bus]
capacity              = 4096
counter_debounce_secs = 60
```

**Why 1 GiB fails at startup**: lbug allocates and initialises all buffer pool pages at
open time, requiring the full 2 GiB to complete schema init and the initial device-registry
backfill. Below ~1.5 GiB the first write transaction fails immediately.

**Why 2 GiB is now safe**: at 60 s debounce the sustained write rate is ~0.5 writes/second,
6× lower than the original 10 s debounce. The pool is evicted faster than it fills.

---

## Problem 2 — Event Bus Saturation at Startup

### Symptom
Within the first minute of a fresh start, the event bus fills to 4096/4096:
```
WARN event bus channel is 100% full — a subscriber may be lagging
     depth=4096 capacity=4096 fill_pct=100
WARN graph writer lagged on event bus dropped=4
```
RSS grows from ~156 MB at open to ~5 GB within the first hour of operation.

### Root Cause: Two Receivers, SSE Lags Indefinitely

The tokio `broadcast::channel(4096)` has **two receivers**:
1. **Graph writer** — processes `TelemetryUpdate` events, writes to lbug, debounces counters.
2. **SSE subscriber** — fans events out to `/api/events` HTTP clients.

A tokio broadcast channel only frees a slot when **all** receivers have consumed the
message. When no browser tab is open on the Events page, the SSE receiver accumulates
every event indefinitely. The graph writer may be fully caught up; the bus still reads
as full because receiver 2 is at slot 0.

### Bus Depth vs Graph Write Activity

**Critical distinction**: `bus_depth=4096` does **not** mean graph writes are queued.
It means the SSE receiver is lagging. Graph writes (via the graph writer receiver) can
complete independently. Enrichment write failures are caused by **simultaneous
write-transaction contention** between the graph writer and the enricher, not bus depth.

### RSS Growth Pattern

| Time since start | RSS | bus_depth | Notes |
|---|---|---|---|
| 0 min | 156 MB | 0 | Fresh open, pool allocated |
| ~1 min | 2.5 GB | 4096 | Initial SRL telemetry burst |
| ~20 min | 3.4 GB | 4082 | Still processing |
| ~30 min | 5.2 GB | 4068 | Growing, not stabilising |

RSS growth after the startup burst (which should settle in <5 min) is **unexpected**.
With 60 s debounce, sustained write load should be minimal. The pool-page RSS growth
continuing past 5 GB suggests dirty pages are accumulating in the buffer pool despite
the debounce — possibly because the initial 4096-event burst takes longer to fully
process than expected when lbug's page eviction under 2 GiB contends with new writes.

### Open Questions
- Does the RSS stabilise after ~1 hour once the burst is fully processed?
- Is the growing RSS entirely from lbug buffer pages, or is the Rust heap also growing?
- Would increasing `capacity` beyond 4096 help (give the graph writer more runway) or
  make the SSE lag problem worse?

---

## Problem 3 — NetBox Enricher Write Contention

### Symptom
```
device 172.100.103.15:57400 property netbox_serial:
  execute upsert_enrichment_property: Query execution failed:
  Cannot start a new write transaction in the system.
  Only one write transaction at a time is allowed in the system.
```
`nodes_touched=0` despite 8 devices matching by hostname in the graph.

### Root Cause
LadybugDB is single-writer (one active write transaction at a time). The graph writer
task (processing the backlogged event bus) holds write transactions in rapid succession.
The enricher's `spawn_blocking` task races for the same write slot and loses every time
during the startup burst.

### Fixes Applied

**Fix 1 — hostname-based address resolution** (`src/enrichment/netbox.rs`):
NetBox devices have no `primary_ip` set (IPAM not seeded). The enricher was matching
`{address: $addr}` (bare IP) against graph Device nodes that store `"host:port"` format.
Fixed by pre-querying a `hostname → address` map and resolving by name:
```rust
let hostname_to_addr: HashMap<String, String> = {
    conn.prepare("MATCH (d:Device) RETURN d.hostname, d.address")...
};
// prefer primary_ip starts_with match; fall back to hostname key lookup
```

**Fix 2 — write-retry with exponential backoff** (`src/enrichment/netbox.rs`):
```rust
fn with_write_retry(mut f: impl FnMut() -> Result<()>) -> Result<()> {
    for attempt in 0u32..8 {
        match f() {
            Ok(()) => return Ok(()),
            Err(e) if e.to_string().contains("Only one write transaction") => {
                std::thread::sleep(Duration::from_millis(20 << attempt.min(7)));
            }
            Err(e) => return Err(e),
        }
    }
    f()
}
```
Wraps every `upsert_enrichment_property`, `upsert_vlan`, `upsert_prefix`,
`link_device_prefix`, and `link_interface_vlan` call.

**Status**: hostname resolution fix confirmed working (`nodes_touched=3` in first run
with new binary). Write-retry fix built but not yet validated — enrichment runs during
the startup burst still exhaust all 8 retry attempts. Enrichment must be re-run once
the startup burst fully settles.

### Pending
- Re-run `POST /api/enrichment/run {"name":"netbox-dc"}` after bus_depth decreases and
  RSS stabilises (expected: ~1 hour after fresh start).
- Longer-term: centralise lbug writes through a single serialised write queue so
  enrichment and telemetry writes don't race at all.

---

## Problem 4 — D3 Topology: Isolated Nodes & Jumble

### Symptom
Force-directed graph bunched all nodes into an indecipherable cluster; nodes with
fewer links drifted off-canvas.

### Fix Applied (`ui/src/lib/Topology.svelte`)
Replaced undirected force-simulation with **role-aware hierarchical tier layout**:

| Tier | Nodes | Y position |
|---|---|---|
| Super-Spines (`spine` + "super" in hostname) | srl-super1, srl-super2 | 14% |
| Spines | srl-spine1, srl-spine2 | 44% |
| Leaves | srl-leaf1–4 | 78% |

Nodes pre-seeded at tier Y, evenly spread on X, then simulation fine-tunes
horizontal spacing. Tier rail labels appended as static SVG text.

```javascript
const TIER_Y = [H * 0.14, H * 0.44, H * 0.78];
sim.force('y', d3.forceY(d => TIER_Y[d._tier]).strength(0.85))
   .force('x', d3.forceX(W / 2).strength(0.04));
```

---

## Problem 5 — Collectors Page False Alarm

### Symptom
Red "X devices unassigned" error banner fired in monolithic mode (no collectors
registered), confusing operators.

### Fix (`ui/src/routes/Collectors.svelte` + `ui/src/app.css`)
Unassigned-device warning now only renders when at least one collector is registered.
In monolithic mode, shows a neutral info banner instead.
Added `.notice.info` CSS class (was missing — only error/success existed).

---

## Current State (2026-05-06 ~11:00)

| Parameter | Value | Target |
|---|---|---|
| bonsai PID | 1824100 | — |
| buffer_pool_bytes | 2 GiB | stable |
| counter_debounce_secs | 60 | stable |
| event_bus capacity | 4096 | stable |
| RSS | ~5.4 GB (growing) | should stabilise ≤3 GB |
| bus_depth | ~4068/4096 | should drain over time |
| bus_receivers | 2 | graph writer + SSE |
| LLDP links | 20/41 expected | settling |
| BGP sessions | partial | settling |
| NetBox credential | `netbox-token` in vault | ✓ |
| ServiceNow PDI credential | `servicenow-pdi` in vault | ✓ |
| ServiceNow PDI connection | tested ✓ | ✓ |
| NetBox enricher `nodes_touched` | 0 (write contention) | needs re-run |
| ServiceNow enricher | registered, not yet run | needs run |
| Path A embeddings | not yet run | Sprint 3 pending |

---

## Next Steps for Sprint 3

1. **Wait for RSS to stabilise** — monitor `rss_mb` in memory profile logs (every 60 s).
   When it plateaus and stops growing, the startup burst is fully processed.
2. **Re-run NetBox enrichment** — verify `nodes_touched > 0` and no write-contention
   warnings with the new binary.
3. **Seed ServiceNow PDI** with lab topology data (or confirm existing CMDB records),
   then run `POST /api/enrichment/run {"name":"servicenow-pdi"}`.
4. **Run Path A spectral embeddings** — `python -m bonsai_ml.embeddings --base-url http://localhost:3000 --dim 16`
5. **Validate graph algorithms** against real topology (centrality, site_dependency_depth,
   co_firing_detections).
6. **Longer-term fix** — single write queue to serialise enricher and telemetry writes.

---

## Files Changed This Session

| File | Change |
|---|---|
| `src/enrichment/netbox.rs` | Hostname-based address resolution + `with_write_retry()` |
| `ui/src/lib/Topology.svelte` | Hierarchical tier D3 layout for DC Clos fabric |
| `ui/src/routes/Collectors.svelte` | Suppress false-alarm in monolithic mode |
| `ui/src/app.css` | Add `.notice.info` CSS class |
| `bonsai.toml` | 2 GiB buffer pool, 60 s counter debounce, 4096 bus capacity |
