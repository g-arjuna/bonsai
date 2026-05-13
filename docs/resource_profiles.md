# Resource Profiles

Bonsai probes the runtime environment at startup and selects a **ResourceProfile** that sets safe defaults for every internal subsystem. Operator config always takes precedence; the profile fills in the gaps for un-configured deployments.

## How the profile is selected

At startup, `resource_profile::probe()` reads:

| Signal | Source |
|--------|--------|
| Total RAM | `/proc/meminfo` (Linux) |
| cgroup memory cap | `/sys/fs/cgroup/memory.max` |
| CPU cores | `std::thread::available_parallelism()` |
| Disk free at archive path | `statvfs(2)` |
| Container detection | presence of `/.dockerenv` |

The **effective RAM** is `min(physical RAM, cgroup cap)`. The profile is chosen from that:

| Profile | Effective RAM | Typical deployment |
|---------|---------------|--------------------|
| `tiny`  | < 2 GB        | CI, edge appliance, tiny VM |
| `small` | 2–5 GB        | Small cloud instance, laptop dev |
| `medium`| 6–13 GB       | Mid-size server, workstation |
| `large` | 14–29 GB      | Dedicated server |
| `xlarge`| ≥ 30 GB       | Production server, large workstation |

The selected profile is logged at `INFO` level on every startup:

```
INFO bonsai: resource profile selected profile=medium ram_gb=16 effective_ram_gb=16 cpu_cores=8 disk_free_gb=120 in_container=false memory_budget_mb=1024
```

## Profile defaults

| Setting | tiny | small | medium | large | xlarge |
|---------|------|-------|--------|-------|--------|
| Memory budget (RSS) | 256 MB | 512 MB | 1 GB | 2 GB | 4 GB |
| LRU cache budget | 8 MB | 32 MB | 128 MB | 512 MB | 1 GB |
| Write coordinator batch | 64 | 128 | 256 | 512 | 1024 |
| Event bus capacity | 512 | 2048 | 8192 | 16384 | 32768 |
| Archive flush interval | 30 s | 20 s | 10 s | 10 s | 10 s |
| Inbound rate budget | 500 eps | 2 K eps | 10 K eps | 50 K eps | 200 K eps |

## Adaptive resource governance

Three background loops run once the governor is started (core mode only):

### Memory pressure (T4-3)

Samples RSS every **5 seconds**. Two thresholds:

- **Soft (80% of budget)**: emits `bonsai_governance_action_total{action="memory_soft"}`, logs at INFO, sets `memory_pressure_active=true`. Ingest shedding becomes available (callers check `governor.memory_pressure_active()`).
- **Hard (95% of budget)**: emits `bonsai_governance_action_total{action="memory_hard"}`, logs at WARN. Signals archive rotation and increments `memory_flush_count`.

When RSS retreats below the soft threshold, the flag clears and governance relaxes.

### Write pressure (T4-4)

Samples `write_coordinator_queue_pct` every **10 seconds**. When `queue_pct > 50%` is sustained for **60 seconds**, emits `bonsai_governance_action_total{action="write_batch_expand"}` and logs at WARN. When the queue retreats below 50%, the flag clears.

### Inbound rate (T4-2)

Measures aggregate events/second over a **10-second window** across all ingest sources. When the window total exceeds `rate_budget_events_per_sec × 10`:

- Increments `bonsai_rate_shed_total{source="all"}` by the excess event count
- Emits `bonsai_governance_action_total{action="rate_shed"}`
- Sets `rate_shedding_active=true` — ingest paths check `governor.is_shedding()` and may drop low-priority messages (BMP statistics, counter noise within debounce)

## Governance observability

### Prometheus metrics

| Metric | Labels | Description |
|--------|--------|-------------|
| `bonsai_governance_action_total` | `action`, `reason`, `profile` | Counter incremented on every governance action |
| `bonsai_rate_shed_total` | `source`, `profile` | Excess events dropped by rate governor |
| `bonsai_inbound_eps` | — | Current aggregate events/second (gauge) |
| `bonsai_rss_bytes` | — | Current RSS in bytes (gauge) |
| `bonsai_write_queue_pct` | — | Write coordinator queue fill % (gauge) |

### REST endpoint

```
GET /api/governance/state
```

Returns a JSON snapshot:

```json
{
  "profile": "medium",
  "memory_budget_mb": 1024,
  "rate_budget_eps": 10000,
  "memory_pressure_active": false,
  "write_pressure_active": false,
  "rate_shedding_active": false,
  "memory_shrink_count": 0,
  "memory_flush_count": 0,
  "write_batch_expand_count": 0,
  "rate_shed_count": 0
}
```

## Overriding profile defaults

All profile defaults are overridden by explicit operator config in `bonsai.toml`. To force a specific budget regardless of what the probe detects, set:

```toml
[ingest]
debounce_memory_bytes = 134217728   # 128 MB — overrides LRU budget

[archive]
flush_interval_seconds = 30         # overrides archive flush

[event_bus]
capacity = 4096                     # overrides bus capacity
```

The probe defaults are only applied when the corresponding config key is absent or at its default value.

## Operational health thresholds

See `docs/operational_health_thresholds.md` for the full list of health check thresholds. Governance fires **before** the kill-switch RSS budget is hit, giving the system time to recover without operator intervention.

The kill-switch (OOM protection via Bv4 budget assertion) remains active as the final backstop. Governance is the graduated degradation curve that fires first.

## Boundary Behavior (C4-N4)

The profile is selected by flooring effective RAM to GB thresholds. This produces a step function, not a continuous one. A VM with 1.9 GB effective RAM selects `tiny` (256 MB memory budget); a VM with 2.0 GB selects `small` (512 MB budget). The gap at the boundary is 256 MB.

| Boundary | Just below | Just above | Budget jump |
|----------|------------|------------|-------------|
| 2 GB | `tiny` — 256 MB | `small` — 512 MB | +256 MB |
| 6 GB | `small` — 512 MB | `medium` — 1 GB | +512 MB |
| 14 GB | `medium` — 1 GB | `large` — 2 GB | +1 GB |
| 30 GB | `large` — 2 GB | `xlarge` — 4 GB | +2 GB |

**If your VM is within 200 MB of a boundary, pin the profile manually** to avoid surprising behavior after memory pressure causes the effective RAM to dip below the threshold:

```toml
[resource]
profile = "small"   # pin explicitly rather than relying on auto-probe
```

The explicit override takes precedence over the probe result and will not shift across restarts as cgroup or memory pressure changes.

**Why there is no hysteresis**: the probe runs once at startup and does not re-probe during runtime. Boundary oscillation during a single run is not possible. The concern is strictly across restarts on VMs that hover near a threshold (e.g., OCI Ampere free-tier VMs with 24 GB nominal RAM but significant cgroup caps from OS overhead).
