# Bonsai Resource Contract

This document states the bounded-resource guarantees for bonsai and explains where to
verify them at runtime.

## Memory (RSS)

| Scenario | Expected RSS ceiling | Notes |
|----------|---------------------|-------|
| Idle (no devices) | < 100 MB | Base runtime |
| 4-device lab (steady state) | < 500 MB | After 15-min warm-up |
| 12-device lab (steady state) | < 1.5 GB | Scales with device × path count |

**What bounds memory**:
- `[retention] enabled = true` (default) limits `StateChangeEvent` accumulation in lbug.
- `[retention] max_age_hours` (default 24) rolls old events out of the graph.
- `[retention] max_state_change_events` (default 10000) hard-caps total event count.
- `[event_bus] capacity` (default 2048) caps broadcast channel queue depth.
- `[archive] max_batch_rows` (default 1000) caps in-flight archive buffer.
- `CounterSummarizer.max_entries` (default 1024) caps summarizer state.

**Observable**: `bonsai_memory_rss_bytes` (Prometheus gauge, sampled every 60s).

**CI assertion**: `.github/workflows/memory-budget.yml` — bonsai with synthetic load for
10 minutes; fails if peak RSS exceeds 1 GB.

---

## Archive (Parquet on disk)

| Config | Default | Behaviour when exceeded |
|--------|---------|------------------------|
| `[storage] max_archive_bytes` | 10 GB | Log warning at `warn_threshold_pct`%; log error at 100% |
| `[archive] writer_max_idle_secs` | 7200 (2h) | Idle partition writers force-closed |

**Observable**: `bonsai_archive_disk_bytes`, `bonsai_archive_disk_use_pct`.

**Compression**: ZSTD level 12 with dictionary encoding for low-cardinality columns.
Typical compression ratio: 8-15× over raw telemetry JSON, depending on value diversity.
Observable: `bonsai_archive_last_compression_ppm` (parts-per-million; divide by 1e6 for ratio).

---

## Graph database (lbug)

| Config | Default | Notes |
|--------|---------|-------|
| `[storage] max_graph_bytes` | 5 GB | Logged when exceeded; does not auto-delete |
| `[retention] max_age_hours` | 24 | StateChangeEvents older than N hours are pruned |
| `[retention] max_state_change_events` | 10000 | Prune oldest events past this count |

**Observable**: `bonsai_graph_disk_bytes`, `bonsai_graph_disk_use_pct`.

---

## Event bus

| Config | Default | Notes |
|--------|---------|-------|
| `[event_bus] capacity` | 2048 | `tokio::sync::broadcast` channel depth |

When any subscriber lags and the channel is >50% full, bonsai logs a warning and increments
`bonsai_event_bus_slow_subscriber_warnings_total`.

When the channel is 100% full, the broadcast channel drops the oldest message for lagging
receivers (they receive `RecvError::Lagged`). This is logged by each consumer.

**Observable**: `bonsai_event_bus_depth`, `bonsai_event_bus_receivers`,
`bonsai_event_bus_slow_subscriber_warnings_total`.

---

## Disk usage summary (Operations UI)

The Operations workspace in the UI shows:
- Current RSS (MB) with 24h trendline
- Archive size on disk + percent of cap
- Graph DB size + percent of cap
- Archive compression ratio (last batch)

All panels use data from `/api/_test/status` (T3-5, Sprint 3).

---

## Tuning for larger deployments

For labs with more than 12 devices or longer retention windows:

```toml
[retention]
enabled = true
max_age_hours = 48          # extend to 48h history
max_state_change_events = 50000

[event_bus]
capacity = 4096             # headroom for more subscribers

[storage]
max_archive_bytes = 53687091200   # 50 GB
max_graph_bytes = 10737418240     # 10 GB

[archive]
compression_level = 12      # keep at 12; level 22 not worth CPU cost
```

Memory footprint scales approximately as:
`RSS ≈ 100 MB base + N_devices × 30 MB + N_retained_events / 10000 × 200 MB`
