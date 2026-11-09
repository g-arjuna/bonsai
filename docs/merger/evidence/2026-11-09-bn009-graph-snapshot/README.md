# BN-009 graph snapshot fix — packaged verification (9 November 2026)

Bounded item: fix `/api/graph/snapshot` and the DB purge path failing on an
invalid DuckDB `TIMESTAMP_NS(...)` call, rebuild the packaged deployment, and
verify the snapshot endpoint plus snapshot-backed ML jobs live.

## What changed

- `src/http_server/observability.rs`: bind timestamp values as Lbug
  `TimestampNs` parameters instead of emitting `TIMESTAMP_NS(...)` SQL
  function calls. Applied to both the snapshot query path and the purge
  `older_than_days` cutoff. Committed as `3c87220`.
- This directory: live verification evidence.

## Reproduction and fix verification

- Pre-fix live reproduction: `GET /api/graph/snapshot` returned HTTP 500
  (`before-snapshot.status`, `before-snapshot.json`).
- `cargo check -p bonsai --lib`: pass, 8.89 s wall (sccache active; logged in
  `docs/build-times/runs.csv`).
- Packaged image rebuild: `docker compose build bonsai-standalone
  bonsai-routing` → `bonsai:latest`, 485.50 s wall, digest
  `sha256:a3340bd6e91cbacd99e529354c4981f72e4ad3b2a7b0b3e500f2c097f8912381`
  (logged in `docs/build-times/runs.csv`).
- `docker compose -f docker-compose.yml -f /tmp/bn019-compose-override.yml
  --profile standalone up -d` recreated `bonsai-standalone` and
  `bonsai-routing`; both reached healthy on the new image.
- Post-fix: `GET /api/graph/snapshot` → HTTP 200
  (`after-snapshot.status`, `after-snapshot.json`) with real topology:
  `devices=3`, `bgp_sessions=11`, `snapshot_ns=1794187407683406241`.

## DB purge verification (no real data deleted)

- Created one disposable `GnnScore` node (`gnn-score-create.json`,
  `gnn-score-create.status` = 201).
- `DELETE /api/db/purge?node_type=GnnScore&older_than_days=0` → HTTP 200,
  `{"deleted_count":1}` (`db-purge.json`, `db-purge.status`).
- DB stats before/after (`db-stats-*.json`): `GnnScore` count 1 → absent;
  `DetectionEvent=11`, `Device=3`, `DeviceAddress=7`, `EntityIdentity=3`,
  `HAS_IDENTITY=3` unchanged.

## ML job verification — honest result

- Scheduler-path attempt: temporarily set `graph_snapshot` and `gnn_snapshot`
  API schedules to `* * * * *`, waited through the sidecar's 60 s schedule
  sync, then restored `0 */4 * * *` (`schedules-before-trigger.json`,
  `schedule-trigger-*.json`, `schedules-after-trigger.json`,
  `job-result-*.json` = `{"job": null}`).
- Finding: the API accepted the updates and APScheduler logged the new
  triggers, but the persisted `runtime/ml_jobs.db` schedules kept the original
  `0 */4 * * *` cron (`apscheduler-store.json`). Root cause identified:
  `job_engine._register_schedule` passes `conflict_policy="replace"` as a
  string; APScheduler 4.0.0a6 `SQLAlchemyDataStore.add_schedule` compares
  against the `ConflictPolicy` enum, so the replace branch never runs and
  updates to existing schedule IDs are silently dropped. Other jobs
  (`syslog_embedding`, `gnn_inference`) continued firing, so the scheduler
  itself is alive.
- Direct verification: invoked the registered job functions through the same
  engine `_run_job` path (creates real `MlJobRun` records). Both succeeded:
  - `graph_snapshot` run `6c1d60e6-c6f6-41df-940c-1c20cc7d8b17`
  - `gnn_snapshot` run `7b766ab1-6f8e-410b-ba78-492df87f1af0`
  (`direct-job-results.json`; `/api/ml/jobs` shows both `succeeded`).

## Newly found defect (not fixed in this slice)

- `_take_graph_snapshot` logs `could not build/store HeteroData: Tried to
  write record batch with different schema`. `SnapshotStore._write_arrow`
  writes heterogeneous node/edge record batches into a single-schema Arrow
  IPC file, producing unreadable 512-byte `.arrow` files.
  `snapshot-store-load.json`: `load_buffer()` returns 0 usable snapshots
  while `buffer_size=2`/`is_stale=false` — the health counter is misleading.
  The job still reports `succeeded` because the store failure is caught and
  only warned. This needs its own bounded fix (per-type files, unified
  schema, or pickle fallback) before `gnn_snapshot` can be called functionally
  accepted.

## Limitations

- Scheduled (cron-fired) `graph_snapshot`/`gnn_snapshot` acceptance is NOT
  proven: the schedule-update bug above blocked the temporary-trigger
  experiment, and the next natural fire is ~02:30 UTC.
- Forward-queue overload remains unexercised (no detection source without
  device writes).
- Full-length BN-009/BN-019 soak window remains the open acceptance gate.

## Next steps

1. Fix `conflict_policy` to `ConflictPolicy.replace` (and add a regression
   test that API schedule updates replace persisted triggers).
2. Fix `SnapshotStore._write_arrow` heterogeneous-schema serialization and
   make buffer health count only readable snapshots.
3. Re-run scheduled `graph_snapshot`/`gnn_snapshot` acceptance, then start/
   extend the full-length soak window.
4. BN-013 first live incident journey trace slice.
