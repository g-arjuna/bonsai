# BN-009 forward-queue overload — live exercise (9 November 2026)

Closes the last unexercised BN-009 restart/overload path: forward-queue
backpressure in `python/collector_engine.py` (`queue.Queue(maxsize=1000)`,
`on_detection()` drop logic, `priority_only_mode` at >95%).

## Method (no device writes)

- Added an env-gated test hook to `collector_engine.py`:
  `_synthetic_detection_injector`, active only when
  `BONSAI_TEST_DETECTION_COUNT > 0`. It fires synthetic `Detection` objects
  through the real `on_detection()` path (local-graph write attempt +
  backpressure + `put_nowait`). Default off; no production behaviour change.
- Ran a dedicated sidecar instance with `BONSAI_LOCAL_ADDR`/`BONSAI_CORE_ADDR`
  pointed at unreachable `127.0.0.1:9` so the forwarder could not drain the
  queue, plus `BONSAI_TEST_DETECTION_COUNT=1500`,
  `BONSAI_TEST_DETECTION_SEVERITY=info`, collector id
  `bn009-overload-test`.

## Results

- `health-overloaded.json`: `queue_depth=946`, `queue_drops_today=549`,
  `priority_only_mode=true`, `detections_today=1500`,
  `connected_to_core=false`.
- `sidecar-log-excerpt.txt`: repeated
  `queue >95% full — dropping info bn009_overload_test` lines once the queue
  crossed 95%, then graceful shutdown reporting `drops=549` and
  `flushed 945 pending detections` (queue could not drain — core unreachable
  by design).
- Backpressure behaviour verified: >80% warning threshold, >95%
  priority-only mode dropping non-critical/high severities, `queue.Full`
  drops counted in `_forward_queue_drops_total`, all surfaced via `/health`
  (`queue_depth`, `queue_drops_today`, `priority_only_mode`) and
  `SidecarHeartbeat.status_json`.
- Sidecar then restarted with normal addresses: healthy, `queue_depth=0`,
  `queue_drops_today=0`, `priority_only_mode=false`, connected to core and
  local, job engine + APScheduler running. The overload queue was in-memory
  only and did not persist across restart (expected).

## Notes

- 1500 synthetic detections were fired; ~946 stayed queued, ~549 dropped
  (some dropped at >95% severity gate before `put_nowait`, rest on
  `queue.Full`). Numbers are consistent with maxsize=1000 minus drain.
- The injector is test instrumentation, env-gated and documented in code;
  it writes synthetic detections only to the unreachable local address, so
  no graph pollution occurred (`connected_to_local` flap is a stale flag
  from the reconnect loop).
- Remaining BN-009/BN-019 gate: full-length soak window. BN-013 next.
