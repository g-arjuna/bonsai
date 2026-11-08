# BN-009 intake reconciliation — 8 November 2026

Base commit `8017090` + uncommitted working tree. Scope: inventory existing
sidecar/BonPy operational-lifecycle functionality vs the BN-009 acceptance
surface, before any build work. Read-only reconciliation; no code changed.

## What already exists (verified in tree)

| BN-009 requirement | Existing implementation |
|---|---|
| Sidecar registry / heartbeat / stale / lost | `src/sidecar_registry.rs` (404 lines): register/heartbeat RPCs, 15s cadence, stale@45s, lost@120s, re-register replaces, `required_kinds`/`missing_required` in `GET /api/sidecars` |
| Health gate | `BONSAI_REQUIRE_SIDECAR` env + `/health` degraded state + 60s startup grace (server_startup/health_emitter) |
| One Bonsai UI entry | `/sidecars` route (`ui/src/routes/Sidecar.svelte`, 497 lines) + `/collectors` (`Collectors.svelte`) + Settings integration references |
| Export catalog / schedules / quality | `src/http_server/ml_jobs.rs`: `GET/POST/PATCH /api/ml/exports`, `GET /api/ml/exports/quality`, `GET/POST/DELETE /api/ml/schedules`, `GET/POST/PATCH /api/ml/jobs` with `cancel_requested` flag |
| Tracked jobs / retry / cancel | `ml_jobs.rs` job-run records (status, metrics, error_message, config_json, cancel_requested); `python/bonsai_ml/job_engine.py` — APScheduler 4.x with in-memory fallback, memory-pressure job shedding/pausing |
| Python sidecar processes | `python/collector_engine.py` (rules/detection), `python/bonsai_ml/` (job_engine, export_job, inference_loop, parquet_store), `python/bonsai_agent/` |
| Supervision (partial) | `deploy/systemd/bonsai-rules-sidecar.service` + `bonsai.service` + `bonsai-chaos.service`; docs mention `BindsTo=bonsai.service` pattern |

## Gaps vs BN-009 acceptance

1. **One event/DB contract** — detection events flow Python→Rust via gRPC
   `CreateDetection`; export catalog lives in bonsai DB via REST. Whether
   sidecar health/lifecycle events share one contract is unverified.
2. **Supervision/watchdog coverage** — systemd unit exists for the rules
   sidecar only; collector_engine/ml job engine restart policy and
   laptop-dev supervision (startup wrapper mentioned in docs) not verified.
3. **Backlog visibility** — job_engine sheds under memory pressure but no
   verified UI/API surface exposes queue depth/backlog.
4. **Observable resource limits** — memory-pressure shedding exists in
   job_engine; no verified per-sidecar CPU/mem metrics (`bonsai_sidecar_*`
   Prometheus metrics are "planned/CV8" per sidecars.md).
5. **Restart/overload soak acceptance** — not run; requires the soak gate.
6. **BonPy UI** — `docs/architecture/sidecars.md` describes a separate
   `ui-bonpy/` SPA; `ui/` has Sidecars/Collectors routes. Whether bonpy
   exists separately or was folded into main UI needs confirming
   (`ui-bonpy/` directory presence unchecked this pass).

## Disposition

Intake reconciliation done far enough to scope the build item: the registry,
health gate, catalog/schedules/jobs APIs, and a Sidecars UI already exist —
BN-009 build work should reconcile/extend these rather than create new
surfaces. Acceptance still requires the restart/overload soak with no
silent data loss.
