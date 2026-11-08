# BN-009 sidecar lifecycle — implementation slice — 8 November 2026

Base commit `8017090` + uncommitted working tree. Follows the intake
reconciliation in `2026-11-08-bn009-intake`.

## Implemented this slice

**Per-sidecar resource metrics + backlog visibility end-to-end:**

- `python/collector_engine.py`: new `_status_json()` populates the existing
  `SidecarHeartbeat.status_json` field (previously sent as `""`) with
  `uptime_secs`, `cpu_seconds` (utime+stime from /proc/self/stat),
  `rss_bytes` (VmRSS), `queue_depth`, `forward_queue_drops_total`,
  `priority_only_mode`, `detections_today`, `last_detection_at_ns`,
  `rules_loaded`. Stdlib /proc reads only, every field null-safe.
- `ui/src/routes/Sidecar.svelte`: status tab now also fetches
  `GET /api/sidecars` (the gRPC registry — previously not rendered anywhere
  in the UI) and shows a "gRPC Registry" panel per entry: name/kind/status,
  parsed metrics (RSS, CPU, queue depth, uptime), events/detections totals.

The wire path already existed: proto `status_json` → `sidecar_registry.rs`
→ `/api/sidecars` → UI. No proto or Rust changes needed.

## Verified existing (closes intake gaps without new code)

- **Supervision**: `deploy/systemd/bonsai-rules-sidecar.service` covers the
  single combined rules+ML sidecar (`collector_engine.py` runs rule engine,
  job engine, STGNN inference per unit description); `PartOf=bonsai.service`,
  SIGTERM graceful stop, `BONSAI_SIDECAR_MAX_MEM_MB=1800`.
- **Resource limits**: `python/bonsai_ml/memory_manager.py` enforces
  `BONSAI_SIDECAR_MAX_MEM_MB` (default 1800) with pressure shedding in
  `job_engine.py`.
- **Export catalog/schedules/jobs/cancel**: `/api/ml/{exports,schedules,
  jobs}` + `cancel_requested` in `src/http_server/ml_jobs.rs`.
- **Health/backlog**: `BONSAI_REQUIRE_SIDECAR` gate + stale/lost registry
  states; queue depth now visible via status_json.

## Verification

- `python3 -m py_compile python/collector_engine.py` — pass
- `/proc/self/stat` + `/proc/self/status` parsing verified standalone
  (cpu_seconds=0.01, VmRSS read OK)
- `cd ui && npm run build` — clean, zero warnings, all chunks < 500 kB
- Import of collector_engine requires project venv (grpc); syntax-checked
  via py_compile only — live heartbeat not exercised this session.

## Not done (honest remainder)

- Live sidecar restart/overload soak with no silent data loss — the BN-009
  acceptance gate; needs a running bonsai+sidecar soak run.
- `status_json` is opaque to bonsai by design; consumers must tolerate
  absent/legacy empty values (UI handles parse failure gracefully).
