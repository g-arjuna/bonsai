# BN-009 scheduled snapshot jobs — fix verification (9 November 2026)

Follow-up to `2026-11-09-bn009-graph-snapshot/`: fixes the two defects that
blocked scheduled acceptance of `graph_snapshot`/`gnn_snapshot`.

## Fixes

- `python/bonsai_ml/job_engine.py`
  - `_register_schedule` now passes `conflict_policy=ConflictPolicy.replace`
    (enum) instead of the string `"replace"`. APScheduler 4.0.0a6
    `SQLAlchemyDataStore.add_schedule` compares against the enum, so the
    previous string silently dropped every update to an existing schedule ID —
    API cron changes never reached the persisted triggers.
  - `_take_graph_snapshot` calls `store.get_buffer_health()` (the actual
    method name); `store.health()` raised AttributeError, which was swallowed
    by the inner try and logged as a store failure.
- `python/bonsai_ml/gnn/snapshot_store.py`
  - `_write_arrow` rewritten: Arrow IPC files cannot contain record batches
    with different schemas, so the old per-node-type/per-edge-type batches
    always failed (`Tried to write record batch with different schema`) and
    left corrupt 512-byte files. The snapshot is now encoded as a single JSON
    payload row under one uniform schema (`bonsai_hetero_snapshot_v2`), with
    `timestamp_ns` in file metadata. `_read_arrow` decodes the payload back
    into `HeteroData`; `_read_timestamp_ns` now uses `reader.schema.metadata`
    (`schema_arrow` does not exist on `RecordBatchFileReader`).

## Verification

- Round-trip unit check: `HeteroData` with node features + edge_index →
  `write_snapshot` → `load_buffer` returns identical shapes/indices;
  `_read_timestamp_ns` returns the written timestamp.
- Sidecar restarted on the fixed code (`scheduler_mode=apscheduler`,
  health ok).
- Scheduled acceptance: temporarily set `graph_snapshot` and `gnn_snapshot`
  API schedules to `* * * * *`, waited through the 60 s schedule sync + cron
  minute. Both fired via APScheduler and succeeded with real `MlJobRun`
  records:
  - `graph_snapshot` `fcb810d0-2f0d-4b39-a98c-5c0202fc5d26` (scheduler)
  - `gnn_snapshot` `6e2c2b08-3db4-41f3-9925-af0824a35b72` (scheduler)
  - Natural `0 */4` fires also succeeded (`02:30` snapshots on disk).
  - Schedules restored to `0 */4 * * *` afterwards
    (`schedules-restored.json`, `jobs-after-scheduled.json`).
- Sidecar log confirms `graph_snapshot: wrote snapshot to buffer
  (buffer_size=6/7, stale=False)` — the `get_buffer_health` path works.
- `snapshot-store-load.json`: 3 readable snapshots load via `load_buffer()`;
  4 legacy corrupt files from the pre-fix writer remain until evicted by the
  T=8 rolling buffer (they are skipped on read, not counted as usable).

## Remaining honest notes

- `buffer_size` in health counts files on disk, including the 4 unreadable
  legacy files; `load_buffer()` correctly returns only the 3 valid ones.
  Legacy files evict naturally as new snapshots arrive.
- Forward-queue overload and the full-length BN-009/BN-019 soak window
  remain open.
