# Bonsai Ingestion Architecture

> Bv3 Sprint 1 — landed 2026-05-06. Addresses findings A-1, A-2, A-3 from
> `docs/bus_memory_investigation_2026-05-06.md`.

---

## Problem statement

Sprint 1 of Bv2-mod ran the 8-node DC EVPN lab and found three coupled failure
modes that caused OOM, bus saturation, and enrichment write failures:

- **A-1** — every `TelemetryUpdate` caused one `spawn_blocking` + one write
  transaction in lbug; 320 interface updates at startup = 320 transactions.
- **A-2** — `tokio::sync::broadcast` freed a slot only when every receiver
  consumed the message; the slowest subscriber stalled all others.
- **A-3** — enrichment opens its own write transaction; during a telemetry
  burst the write lock was always held, causing enrichment to exhaust retries.

---

## Architecture overview

```
gNMI stream
    │
    ▼
TelemetryDebouncer          ← ingest.rs — drops at source (T1-3, T1-4)
    │  • counter debounce window (cfg: ingest.counter_debounce_secs)
    │  • level-1 backpressure: drop INTERFACE_STATS when queue >75%
    │  • level-2 backpressure: drop rapid OPER_STATUS when queue >90%
    │
    ▼
InProcessBus::publish()     ← event_bus.rs
    │  • router mpsc queue (capacity = event_bus.capacity)
    │  • router task fans out to each subscriber's own mpsc queue
    │
    ├──► graph_writer subscriber (OverflowPolicy::BlockProducer)
    │        │  forwards WriteRequest::Telemetry to write coordinator
    │
    ├──► archive subscriber (OverflowPolicy::DropOldest, cap 4096)
    ├──► subscription_verifier subscriber (OverflowPolicy::DropOldest, cap 1024)
    ├──► prometheus adapter subscriber (OverflowPolicy::DropOldest, cap 1024)
    └──► collector_forwarder subscriber (OverflowPolicy::DropOldest, cap 4096)

WriteCoordinator            ← write_coordinator.rs (T1-1, A-3 fix)
    │  • single owner of all lbug write transactions
    │  • batches Telemetry writes: flush every 256 updates OR 1 second
    │  • runs Enrichment, Detection, Remediation, SubscriptionStatus writes
    │    between telemetry batches — never concurrent with each other
    │
    ▼
GraphStore::write_batch()   ← graph/mod.rs
    │  • one spawn_blocking + one write lock for up to 256 updates
    └──► lbug write transaction
```

---

## Write coordinator

**File**: `src/write_coordinator.rs`

The coordinator is the single point through which all lbug write transactions
are issued. No other component may call `Connection::new(&db).execute(...)` for
writes — all writes flow through `WriteRequest` submission.

```rust
enum WriteRequest {
    Telemetry(TelemetryUpdate),         // batchable, droppable under pressure
    SubscriptionStatus(...),             // low-frequency, not droppable
    Detection { ..., reply_to },        // critical, reply channel
    Remediation { ..., reply_to },      // critical, reply channel
}
```

The coordinator loop:

1. Accumulates `Telemetry` requests into a `Vec<TelemetryUpdate>`.
2. Flushes when `batch_size` (default 256) is reached OR the 1-second timer
   fires — whichever comes first.
3. For non-telemetry writes: flushes any pending telemetry first, then executes
   the write synchronously before returning to the loop.
4. Uses `GraphStore::write_batch()` which holds the write lock once for the
   entire batch, not once per update.

**Config** (`bonsai.toml`):
```toml
# These are the defaults; omit to accept them.
[write_coordinator]
batch_size = 256
flush_interval_secs = 1
queue_capacity = 4096
```

**Queue depth** is exposed as `bonsai_write_coordinator_queue_depth` (Prometheus
gauge) and in `GET /api/_test/status` under `memory.write_coordinator_queue_pct`.

---

## Per-subscriber bus queues

**File**: `src/event_bus.rs`

`InProcessBus` routes each published `TelemetryUpdate` to every registered
subscriber's own `mpsc::channel`. Subscribers register via `add_subscriber()`
with an `OverflowPolicy`:

| Policy | Effect on full queue |
|---|---|
| `DropOldest` | Oldest unread message discarded (archive, output adapters) |
| `DropNewest` | Incoming message discarded |
| `BlockProducer` | Router waits until space exists (graph_writer — must not lose) |

Each subscriber's queue depth is tracked as
`bonsai_subscriber_queue_depth{subscriber="<name>"}`.

A slow subscriber no longer stalls unrelated subscribers. The router task fans
out to all subscribers concurrently via `join_all`.

---

## Ingest-layer debounce and backpressure

**File**: `src/ingest.rs` — `TelemetryDebouncer`

Debounce and backpressure happen **before** the update is published to the bus.
An update that is dropped here consumes no bus capacity, no coordinator queue
slot, and no archive row.

**Debounce** (`cfg: ingest.counter_debounce_secs`, default 60s):  
Counter updates (INTERFACE_STATS) for the same `(target, interface)` pair
within the debounce window are dropped at ingest time. The window is per-device
per-interface, tracked in an LRU cache.

**Backpressure** (two graduated levels):

| Level | Threshold | Action |
|---|---|---|
| 1 | write_coordinator queue ≥ 75% | Drop all INTERFACE_STATS |
| 2 | write_coordinator queue ≥ 90% | Also drop rapidly-changing OPER_STATUS (< 6s between changes) |

BGP/IS-IS/BFD adjacency changes, DetectionEvents, and SubscriptionStatus
updates are never dropped by backpressure.

Drops are counted in `bonsai_ingest_backpressure_drops_total{reason}`.

---

## Memory budget assertions

**Prometheus alert rules**: `docker/prometheus/alerts/bonsai-memory.yml`  
**Grafana provisioning**: `docker/grafana/alerts/bonsai-memory.yml`

Three alerts are defined:

| Alert | Condition | Fire after |
|---|---|---|
| `BonsaiRssExceedsBudget` | RSS > 1.5 GiB | 1 min sustained |
| `BonsaiWriteCoordinatorQueueHigh` | queue > 75% full | 5 min sustained |
| `BonsaiEventBusDropping` | router drops > 0 in 5 min | immediate |

**Status endpoint**: `GET /api/_test/status` returns a `budget_breaches` array.
Each entry names the breached budget, its current value, and the threshold. An
empty array means all budgets are within spec.

```json
{
  "budget_breaches": [
    {
      "name": "rss_budget",
      "current": 1720000000,
      "budget": 1610612736,
      "unit": "bytes"
    }
  ]
}
```

---

## Sprint 1 success criterion

The architecture is complete when a 12-node lab sustains an 8-hour run with:

- RSS plateau visible within 15 minutes of fresh start
- RSS stable below 1.5 GiB; no OOM
- `budget_breaches` empty in `GET /api/_test/status` during steady-state
- `bonsai_ingest_backpressure_drops_total` > 0 only during known surge events,
  not continuously
- Enrichment runs succeed with zero "write transaction in the system" errors
- `bonsai_graph_write_latency_seconds` p99 below 100ms in steady state

---

## What this does NOT change

- The gNMI subscription path itself (subscriber.rs) is unchanged.
- The graph schema is unchanged.
- Read paths (query, API, HTTP) are unchanged.
- The archive Parquet format is unchanged.
- The detection/remediation flow is unchanged; Detection and Remediation writes
  now go through the coordinator but the calling API is identical.
