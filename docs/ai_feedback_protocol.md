# Bonsai AI Feedback Protocol

This document describes the machine-readable endpoints and output files that an
AI coding assistant should use to assess lab health, test coverage, and runtime
correctness before proposing changes to bonsai.

---

## 1. Primary health endpoint: `GET /api/_test/status`

Returns an aggregated snapshot of memory, disk, external services, and the most
recent output of each test driver. Always check this endpoint first.

### Response schema

```json
{
  "ts_unix": 1714737600,
  "memory": {
    "rss_bytes": 52428800,
    "event_bus_depth": 3,
    "event_bus_receivers": 4,
    "archive_buffer_rows": 0,
    "archive_lag_millis": 12,
    "archive_last_compression_ppm": 120000
  },
  "disk": {
    "archive_bytes": 10485760,
    "archive_max_bytes": 10737418240,
    "archive_pct": 0,
    "graph_bytes": 524288,
    "graph_max_bytes": 5368709120,
    "graph_pct": 0
  },
  "external": { ... },
  "driver_results": {
    "api": { ... },
    "event": { ... },
    "ui": { ... }
  }
}
```

**Healthy baseline (no devices connected)**:
- `memory.rss_bytes` < 100 MB (100 × 1024 × 1024 = 104 857 600)
- `disk.archive_pct` < 80
- `disk.graph_pct` < 80
- `driver_results.api.failed` == 0

---

## 2. Operations endpoint: `GET /api/operations`

Full system summary used by the Operations UI. Contains all fields from the test
status endpoint plus graph-level counters.

### Key fields for AI assessment

| Field | Healthy value | Action if unhealthy |
|-------|--------------|---------------------|
| `rss_bytes` | < 500 MB steady-state | Check retention config; enable if disabled |
| `state_change_events` | < 10 000 (retention enabled) | Verify `[retention] enabled=true` |
| `archive_disk_pct` | < 80 | Check `[storage] max_archive_bytes` |
| `graph_disk_pct` | < 80 | Check `[storage] max_graph_bytes` |
| `event_bus_depth` | < 800 (< 40% of 2048 cap) | Look for slow subscribers |
| `observed_subscriptions` | > 0 when devices configured | Check device credentials and reachability |
| `pending_subscriptions` | 0 in steady state | Wait; alert if persistent > 5 min |

---

## 3. Driver result files

Each driver writes its result to `runtime/driver_results/<name>.json` after a run.
These files are also served via `/api/_test/status` under `driver_results`.

### `runtime/driver_results/api.json` — API contract driver

```json
{
  "driver": "api",
  "ts_unix": 1714737600,
  "base_url": "http://localhost:3000",
  "passed": 18,
  "failed": 0,
  "skipped": 1,
  "cases": [
    {
      "name": "topology",
      "method": "GET",
      "path": "/api/topology",
      "status": 200,
      "ok": true,
      "error": "",
      "response_keys": ["devices", "links"],
      "duration_ms": 4.2
    }
  ]
}
```

**Interpretation**:
- `failed > 0` → broken API contract; check `cases[].error` for the specific endpoint
- A `status: 500` usually indicates a graph query error — check `RUST_LOG` output
- `skipped` cases are non-fatal (e.g. trace endpoint requires a detection event)

### `runtime/driver_results/event.json` — SSE event stream driver

```json
{
  "driver": "event",
  "ts_unix": 1714737600,
  "base_url": "http://localhost:3000",
  "duration_secs": 30.0,
  "connected": true,
  "events_received": 12,
  "events_valid": 12,
  "events_invalid": 0,
  "unknown_types": [],
  "validation_errors": [],
  "ok": true,
  "error": ""
}
```

**Interpretation**:
- `connected: false` → HTTP server is not running or SSE handler returned an error
- `events_invalid > 0` → event schema regression; check `validation_errors[]`
- `unknown_types` → new event type added without updating this driver (non-fatal)
- `events_received == 0` → normal if no devices are connected; not a failure

### `runtime/driver_results/ui.json` — Playwright UI driver

Playwright JSON reporter format. The top-level fields to check:
- `stats.unexpected` — failing tests
- `stats.skipped` — intentionally skipped
- `suites[].specs[].tests[].results[].status` — `"passed"` | `"failed"` | `"skipped"`

---

## 4. External infrastructure status: `runtime/external_status.json`

Written by `scripts/check_external.sh`. Served under `external` in `/api/_test/status`.

```json
{
  "netbox":      { "reachable": true, "seeded": true, "device_count": 4 },
  "splunk":      { "reachable": false, "hec_token_valid": false },
  "elastic":     { "reachable": true, "cluster_status": "green", "index_present": true },
  "prometheus":  { "reachable": true, "scraping_bonsai": true, "bonsai_metric_series": 14 },
  "servicenow_pdi": { "reachable": false },
  "bonsai":      { "reachable": true, "topology_ok": true, "device_count": 4 }
}
```

**Note**: all fields may be absent if the script has not been run. `null` in
`/api/_test/status` means the file does not exist — not that services are down.

---

## 5. Prometheus metrics (for deeper analysis)

The metrics endpoint at `http://localhost:9090/metrics` (or configured
`metrics_addr`) exposes all internal gauges and counters. Useful ones for AI:

| Metric | Description |
|--------|-------------|
| `bonsai_memory_rss_bytes` | Process RSS, sampled every 60 s |
| `bonsai_archive_disk_bytes` | Archive directory size in bytes |
| `bonsai_graph_disk_bytes` | Graph DB directory size in bytes |
| `bonsai_archive_disk_use_pct` | Archive usage as % of cap |
| `bonsai_graph_disk_use_pct` | Graph usage as % of cap |
| `bonsai_event_bus_depth` | Current broadcast channel fill level |
| `bonsai_event_bus_slow_subscriber_warnings_total` | Lagging subscriber count |
| `bonsai_counter_summarizer_evictions_total` | CounterSummarizer evictions |
| `bonsai_disk_cap_reached_total{component="archive"|"graph"}` | Cap breach counter |

---

## 6. How to run the drivers

```bash
# API contract (requires bonsai running on :3000)
python tests/api_driver/run.py

# SSE event stream (listens for 30 seconds)
python tests/event_driver/run.py --duration 30

# Playwright UI tests (requires npm install in tests/ui_driver/ first)
cd tests/ui_driver && npm install && npm test
```

Results land in `runtime/driver_results/` and are served at `/api/_test/status`.

---

## 7. Decision heuristic for AI agents

Before suggesting a code change, evaluate:

1. `GET /api/_test/status` → check `driver_results.api.failed == 0`
2. `GET /api/operations` → verify memory and disk are within bounds
3. If `state_change_events > 10000` → confirm `[retention] enabled = true` in bonsai.toml
4. If `event_bus_depth > 1000` → look for a stalled output adapter or SSE client
5. If `archive_disk_pct >= 80` → warn before adding features that increase write volume

After a code change, re-run `python tests/api_driver/run.py` and confirm
`failed == 0` before declaring success.
