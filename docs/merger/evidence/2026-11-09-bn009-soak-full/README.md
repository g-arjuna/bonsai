# BN-009/BN-019 full-length soak window — started 9 November 2026

Full-length soak sampler for the shared BN-009/BN-019 acceptance gate,
following the design in `docs/merger/evidence/2026-11-08-bn019-soak/` and the
slice pattern in `docs/merger/evidence/2026-11-09-bn009-soak/`.

## Setup

- `sampler.py` runs detached (`nohup`), appending one row per 60 s to
  `soak-metrics.csv`. Started **2026-11-09T05:48:01Z**.
- Columns: routing health (peers/routes/gaps/seq/completeness/speakers),
  sidecar `/health` (queue depth, drops, priority-only, job engine, snapshot
  buffer, core/local connectivity, uptime), `/api/sidecars` registry status,
  and docker stats memory for `bonsai-bonsai-standalone-1` /
  `bonsai-bonsai-routing-1`.
- Auth: admin JWT via `/api/auth/login` from `.env`, refreshed on failure.
- First row verified: `peers_up=6 routes=7 gaps=1 seq=297 complete=gap
  speakers=2`, sidecar `queue=0 drops=0 jobs=True snap_buf=7 conn=True/True`,
  `bn009-api-auth=healthy`.

## Scope notes

- Covers the packaged `bonsai:latest` standalone deployment + running
  `collector_engine.py` sidecar + containerlab labs
  (`bonsai-bn003-vrf`, `bonsai-bn003-r07vrf`) — same environment as the
  9 Nov restart/overload slice.
- Forward-queue overload and sidecar/app/routing restart recovery were
  already exercised in bounded slices; this window is the sustained
  no-silent-data-loss observation period.
- To finalize: stop the sampler (`pkill -f bn009-soak-full/sampler.py`),
  review `soak-metrics.csv` for gaps/restarts/drops/memory growth, and
  record the window length + verdict here and in `docs/STATUS.md`.
- Process is a plain background python process — it survives this session
  but not a host reboot; check `pgrep -f bn009-soak-full/sampler.py`.
