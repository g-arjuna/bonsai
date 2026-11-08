# BN-019/BN-009 shared soak design — 8 November 2026

One soak design serves both backlog items (per BN-019 note: "soak design
should be shared with BN-009 rather than duplicated"). BN-009 owns the
sidecar restart/overload acceptance; BN-019 owns collector limits,
observability and packaged-deployment endurance.

## Scope split

| Layer | Owner | What is soaked |
|---|---|---|
| Routing service (`bonsai-routing` container / binary) | BN-019 | BMP listener under continuous genuine FRR feed, durable journal growth, container restart recovery, memory/CPU envelope |
| Bonsai app + sidecars (collector_engine, rules-sidecar, job_engine) | BN-009 | sidecar supervision/watchdog, heartbeat metrics, memory shedding (`BONSAI_SIDECAR_MAX_MEM_MB`), backlog visibility, restart/overload with no silent data loss |
| Packaged deployment as a whole | BN-019 | compose stack endurance, volume integrity, health endpoints |

## Executed bounded slice (this session): PASS

12-minute soak (24 samples @ 30 s) of the **packaged**
`bonsai-routing` container with the genuine r07vrf FRF feed attached
(host-network mode, real exporter IPs). `soak-metrics.csv`:

- `completeness_state=complete_for_observed_view`, `transport_gaps=0`,
  `peers_up=6`, `total_routes=7` — **24/24 samples identical**
- restart counts: 0 on both containers for the whole window
- memory stable: routing ~10.3 MiB, standalone ~495 MiB (no growth trend)
- journal grew continuously (genuine feed); no errors in container logs

Environment restored afterwards: override reverted to bridge +
`127.0.0.1:11019`, lab `bonsai-routing` restarted on
172.100.207.1:11019 (`complete_for_observed_view`, peers_up=6,
journal growing).

## Acceptance criteria (full soak — still open)

- **No silent data loss:** every BMP event journaled durably; on restart,
  replay reproduces identical RIB state (verified separately by BN-003
  journey 11 and BN-005 container-replacement gate).
- **Honest degradation:** transport gaps recorded as `gap`, never
  synthesized withdrawals; stale/lost sidecars visible via heartbeat.
- **Bounded resources:** RSS/CPU within configured limits; job_engine
  sheds under memory pressure rather than OOM.
- **Observability:** `/health` subsystem statuses + sidecar metrics
  (`status_json` cpu/rss/queue/uptime) scrape-able throughout.

## Not claimed

A 12-minute slice does not satisfy the full soak acceptance. A longer
window (hours–days) plus an overload phase (journal growth, sidecar
restart churn, BMP reconnect storms) remains open and is shared with
BN-009's restart/overload acceptance.
