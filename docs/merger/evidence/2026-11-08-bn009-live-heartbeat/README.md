# BN-009 live heartbeat exercise — 8 November 2026

Base commit `e8a1f55`. First live exercise of the sidecar
register/heartbeat path against the **packaged deployment**
(`bonsai:latest`, compose standalone, gRPC :50051).

## Result: PASS — register, heartbeat metrics, graceful stop observed

### Executed

```
BONSAI_LOCAL_ADDR=localhost:50051 BONSAI_CORE_ADDR=localhost:50051 \
BONSAI_API_URL=http://localhost:3000 BONSAI_COLLECTOR_ID=bn009-live-hb \
BONSAI_SIDECAR_HEALTH_PORT=9293 \
.venv/bin/python python/collector_engine.py
```

- `RegisterSidecar` over gRPC succeeded; sidecar
  `4011c5fb-92a4-486d-b47b-0557e02383b7` (kind `rules`, v0.1.0) visible at
  `GET /api/sidecars` (admin JWT) with 35 rule capabilities.
- `status: healthy`; `status_json` populated end-to-end and advancing:
  `uptime_secs` 15→30→90→120→165, `cpu_seconds`, `rss_bytes` (~139 MB),
  `queue_depth`, `forward_queue_drops_total`, `rules_loaded: 35`.
- Heartbeat cadence 15 s matches `src/sidecar_registry.rs`
  (`HEARTBEAT_INTERVAL`); stale/lost thresholds are 45 s / 120 s.

### SIGTERM / stale→lost observation

SIGTERM to the sidecar pid → registry should transition
healthy → stale (45 s) → lost (120 s). Timing samples in
`stale-lost-observation.txt` (see note below).

### Honest notes / defects observed

- First SIGTERM hit the wrong pid (nohup wrapper); re-sent to the real
  python pid — process exited gracefully (`graceful shutdown complete`
  in `/tmp/bn009-sidecar.log`).
- The sidecar's ML job engine made unauthenticated calls to
  `http://localhost:3000/api/*` and got repeated **401s**
  (`/api/graph/snapshot`, `/api/events/unembedded`) — the packaged
  deployment requires auth and the sidecar has no credential path.
  Sidecar core functions (register/heartbeat/rules) are unaffected, but
  ML/export jobs silently retry-fail. Recorded as a BN-009 gap:
  sidecar→API auth wiring is not exercised in this deployment mode.
- `last_heartbeat_at`/`registered_at` rendered `null` in the API
  snapshot while `status`/`status_json` were correct — field naming or
  population gap worth a look (not blocking).

### Remaining BN-009

Restart/overload soak acceptance with no silent data loss (shared design
with BN-019 soak, still open); sidecar→API authenticated job path.
