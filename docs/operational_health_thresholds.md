# Bonsai Operational Health Thresholds

These thresholds are the first-pass intervention points for the BV6 continuous
chaos and archive loop. They are intentionally conservative until the archive
has enough fault data to tune them from real distributions.

## Write Path

| Signal | Warning threshold | Critical threshold | First response |
|---|---:|---:|---|
| `write_coordinator_queue_pct` | `> 50%` for 5 minutes | `> 80%` for 5 minutes | Check graph write latency and recent graph errors. Reduce subscriber load if the queue does not drain. |
| `bonsai_graph_write_errors_total` | Any sustained increase | Any sustained increase with incident loss | Inspect `runtime/logs/` and the graph writer error labels before restarting Bonsai. |

## Event Bus

| Signal | Warning threshold | Critical threshold | First response |
|---|---:|---:|---|
| `event_bus_depth` | `> 50%` for 5 minutes | `> 80%` for 5 minutes | Identify slow subscribers or output adapters. Degrade failing adapters before touching ingestion. |
| `bonsai_event_bus_dropped_total` | Any sustained increase | Any increase during chaos injection | Check subscriber queue policy and adapter health. |

## Archive

| Signal | Warning threshold | Critical threshold | First response |
|---|---:|---:|---|
| `archive_lag_millis` | `> 30000` for 5 minutes | `> 120000` for 5 minutes | Check disk pressure, Parquet writer errors, and archive filesystem latency. |
| Archive verification | Any failed nightly check | Two failed nightly checks in a row | Stop training-data promotion until `scripts/verify_archive.sh` passes again. |

## Chaos Data Freshness

| Signal | Warning threshold | Critical threshold | First response |
|---|---:|---:|---|
| Last chaos injection age | `> 2h` | `> 24h` | Run `bash scripts/chaos_runner.sh --ensure-running` from WSL and inspect `runtime/chaos_runner.log`. |
| Daily chaos cycles | Lower than recent baseline | `0` cycles in 24h | Check cron, machine sleep/reboot history, and `runtime/chaos_log.jsonl` restart markers. |

The daily check report records chaos cycles and injections from
`chaos_runs/*/injections.csv`. Restart markers are appended to
`runtime/chaos_log.jsonl` by `scripts/chaos_runner.sh --ensure-running`.
