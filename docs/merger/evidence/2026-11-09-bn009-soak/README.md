# BN-009 restart/overload soak slice — 9 November 2026

Extends the shared BN-019/BN-009 soak design
(`docs/merger/evidence/2026-11-08-bn019-soak/README.md`) — this slice covers
the **BN-009 sidecar restart acceptance** plus routing/app container
restart recovery on the packaged deployment. Samples: `soak-metrics.csv`
(30 s cadence).

Base: `cad48ce` (sidecar→API auth slice). Deployment: packaged
`bonsai:latest` compose standalone + `bonsai-bonsai-routing-1`;
sidecar `collector_engine.py` with `BONSAI_API_TOKEN` (bn009-sidecar key).

## Result: PASS — honest degradation, no silent data loss observed

### Phase 1 — sidecar SIGKILL → stale → lost → re-register

- SIGKILL to `collector_engine` pid at **23:51:00Z** (first attempt hit
  the nohup wrapper pid — same trap as the 8 Nov exercise; real python
  pid killed on second attempt, noted honestly).
- Registry (`GET /api/sidecars`, admin JWT): `bn009-api-auth`
  **healthy → stale at 23:52:01 (~61 s) → lost at 23:53:05 (~125 s)** —
  consistent with `STALE_AFTER=45 s` / `LOST_AFTER=120 s` in
  `src/sidecar_registry.rs` within 30 s sample granularity. Health
  endpoint `:9294/health` down throughout.
- Restart with identical env + token at ~23:53:2x → **new
  `sidecar_id 41711fcf…` registered healthy** on first heartbeat;
  **0 × 401** in the new log (auth slice holding under restart).
- Job engine resumed: APScheduler schedules re-registered, syslog /
  config embedding jobs cycling, `MlJobRun` records continuing in the
  durable store (no loss across sidecar restart — job state lives in
  `runtime/ml_jobs.db` + graph records, not sidecar memory).

### Phase 2 — Bonsai app container restart (registry is in-memory)

- `docker restart bonsai-bonsai-standalone-1` at **23:54:10Z**;
  healthy again ~25 s later.
- Sidecar process untouched (`up=` uptime kept counting). On the next
  heartbeat the registry no longer knew the sidecar →
  `reregister_required` → **re-registered with new id
  `05a57a57-…`**, status healthy within one heartbeat period.
- Pre-restart `lost` entries (`bn009-live-hb`, old ids) vanished with
  the in-memory registry — expected; registrations are ephemeral by
  design (`docs/architecture/sidecars.md`).
- ML job history, export catalog and schedules persisted through the
  app restart (graph store durable; verified via `/api/ml/jobs`,
  `/api/ml/exports`, `/api/ml/schedules` before/after).

### Phase 3 — routing container restart (journal replay)

- `docker restart bonsai-bonsai-routing-1` at **23:56:15Z**.
- Post-restart `/api/routing/v1/health`: `durable_sequence=297`,
  `graph_checkpoint_sequence=297`, `peers_up=6`, `total_routes=7`,
  `transport_gaps=1`, `completeness_state=gap` — **identical RIB/seq
  state to pre-restart baseline** (journal replay reproduced state; no
  synthesized withdrawals, gap count unchanged).
- `speakers_connected` read 2 at +30 s/+70 s (was 3 pre-restart): one
  BMP peer still reconnecting at last sample — honest in-progress
  reconnect, not data loss; `completeness_state=gap` correctly reflects
  it. (The `gap`/`transport_gaps=1` predates this exercise — carried
  over from earlier lab work, unchanged by restarts.)

### Resource/observability envelope

- Sidecar RSS ~99–139 MiB, queue_depth 0, `forward_queue_drops_total=0`
  throughout — no drops to count because detection volume was ~zero
  (labs idle; honest note: forward-queue overload path was **not**
  exercised — no device writes permitted to generate detections).
- Containers stable: standalone ~490–498 MiB, routing ~10.5 MiB;
  restart counts only the ones we issued.

## Not claimed / follow-ups

- Full-length (hours–days) soak window still open (shared with BN-019).
- Forward-queue overload (>80%/>95% backpressure, priority-only drops)
  not exercised — needs a detection source; device writes disabled.
- `/api/graph/snapshot` 500 (`TIMESTAMP_NS`) still blocks
  graph_snapshot/gnn jobs — tracked from the api-auth slice.
- `speakers_connected=2` — verify third BMP peer reconnected after
  lab-side reconnect window; if persistent, investigate exporter.
