# BN-009 sidecar→API auth wiring — 9 November 2026

Base: working tree on `main` @ `414ef57` + this slice's changes.
Deployment: packaged `bonsai:latest` compose standalone
(`bonsai-bonsai-standalone-1` :3000/:50051, `bonsai-bonsai-routing-1`
127.0.0.1:11019), `require_auth` enforced (401s observed pre-fix).

## Result: PASS — authenticated sidecar path verified live

### Defect (reproduced baseline)

`docs/merger/evidence/2026-11-08-bn009-live-heartbeat/` recorded repeated
`401 Unauthorized` from the sidecar's ML job engine, export jobs, embedding
workers and inference loop against `http://localhost:3000/api/*`. Baseline
log preserved at `before-unauthenticated-401s.log` (21 × 401 across
`/api/graph/snapshot`, `/api/events/unembedded`, `/api/sidecar/rules`).

### Fix

- New `python/bonsai_ml/api_auth.py`: `api_token()` /
  `auth_headers()` / `merge_headers()` reading `BONSAI_API_TOKEN`
  (a `bsk_` API key). Unset → empty headers (legacy no-auth dev mode).
- Headers injected at every Bonsai HTTP call site:
  - `bonsai_ml/job_engine.py` — all 6 aiohttp calls (schedules GET/POST,
    governance pressure, dead-letter, job run create/patch) +
    `requests` events publish; headers cached on the engine instance.
  - `bonsai_ml/job_progress.py`, `export_job.py` (3 calls),
    `syslog_embedding_worker.py` (2), `config_embedding_worker.py` (3),
    `syslog_cluster.py` (3), `detection_clustering.py` (urllib GET+PATCH),
    `inference_loop.py` (aiohttp snapshot/results/attention + 2 events
    publishes), `gnn/embedding_pca.py`, `gnn/archive_to_training.py`
    (urllib GETs), `snapshot_client.py` + `inference_client.py`
    (requests.Session headers).
  - `bonsai_sdk/client.py`: `http_token` now defaults to
    `BONSAI_API_TOKEN` env — covers `_http_json` users (RuleEngine
    `/api/sidecar/rules` override poll, `/api/changes/context/*`,
    approvals, embeddings upsert).
  - `train_stgnn.py`, `bootstrap_agent.py`, `rca_harness.py`,
    `collector_engine.py` health probe (`/api/ml/embedding-stats`).
- `export_job.py` bug fixed in passing: `quality_json` was sent as a JSON
  object but the API expects a string → every catalog PATCH 422'd and
  export records stayed `status:"running"`. Now `json.dumps(...)`.
  Verified: `bc7d50ad-…` export record reached `completed` with
  `quality_json` populated.

### Live verification

1. Admin JWT → `POST /api/auth/apikeys {"alias":"bn009-sidecar",
   "scope":"operator"}` → `bsk_…` key (`apikey-62796f9529ff0036`).
   Scope `operator` → `Global(Operator)`: covers the sidecar's reads
   (Viewer-level GETs) and writes (Operator-level POST/PATCH on
   `/api/ml/*`, `/api/events/*`, `/api/gnn/*`, `/api/devices/*`).
2. Manual probes with the key: `/api/events/unembedded` 200,
   `/api/sidecar/rules` 200, `/api/ml/schedules` 200,
   `PATCH /api/ml/exports/{id}` 200.
3. Restarted collector_engine with `BONSAI_API_TOKEN=<key>`
   (`BONSAI_COLLECTOR_ID=bn009-api-auth`, health :9294), log at
   `after-authenticated-sidecar.log`:
   - **0 × 401** (vs 21 in baseline; the single "401" grep hit is a
     timestamp substring).
   - `syslog_embedding` + `config_embedding` jobs **succeeded** via
     APScheduler — visible in `GET /api/ml/jobs` with real run records
     (`trigger:"scheduler"`, `status:"succeeded"`), proving authed
     POST `/api/ml/jobs` + PATCH `/api/ml/jobs/{id}` round-trip.
   - Default schedules POSTed to `/api/ml/schedules`
     (`last_modified_by:"api"`), cron entries live in the DB.
   - `anomaly_export` created a real catalog record
     (`ParquetExport` UUID, not `local-*`) — authed POST
     `/api/ml/exports` works.
   - RuleEngine DB-override poll (`/api/sidecar/rules`) no longer 401s —
     `rules_enabled:34` vs `rules_loaded:35` shows overrides applied via
     the `BonsaiClient` env-token fallback.
   - Sidecar registered healthy at `/api/sidecars`
     (`1daf9aa0-e804-4460-aa7a-12d6b1ffdb76`), heartbeats advancing.

### Honest notes / remaining defects (not auth)

- `GET /api/graph/snapshot` returns **500** for both the API key and the
  admin JWT: `Catalog exception: function TIMESTAMP_NS does not exist`
  (DuckDB catalog query). Pre-existing server bug — `graph_snapshot` /
  `gnn_snapshot` jobs still fail (now with 500, honestly recorded in
  `MlJobRun.error_message` and retried per policy). Filed as a BN-009
  follow-up; the inference loop also skips (no STGNN model + stale
  snapshot buffer).
- `bonsai_sdk.training` import fails in this venv (`No module named
  'pandas'`) → export jobs write empty parquet and
  `quality_passed=False`. Environment gap, pre-existing.
- `last_heartbeat_at`/`registered_at` "null" from the heartbeat exercise:
  **not reproducible as an API defect.** `/api/sidecars` serializes
  `registered_at_ns`/`last_heartbeat_ns` (u64, always populated —
  verified live: `1794181350453324799` / advancing heartbeat). No
  `last_heartbeat_at`/`registered_at` field exists in Rust or UI source;
  the earlier null was a field-name expectation mismatch, not missing
  data. No code change needed.
- API key `bn009-sidecar` left enabled for the soak exercise; raw key
  intentionally not committed (stored only in `/tmp/bn009-apikey.json`).

### Checks executed

- `py_compile` on all 18 touched files — OK.
- Import + `auth_headers()`/`BonsaiClient._http_token` fallback unit
  sanity — OK.
- `pytest tests/test_gnn_scaffolding.py test_embeddings.py
  test_gnn_data_loader.py` — 56 passed, 1 skipped, **2 pre-existing
  failures** (`test_default_feature_names_count` 23 vs 36,
  `test_bonsai_gnn_config_defaults`) — stale expectations vs
  `data_loader.py` feature list; untouched by this diff.
- Live sidecar run ~4 min: jobs, schedules, exports, overrides all
  authenticated; evidence logs in this directory.
