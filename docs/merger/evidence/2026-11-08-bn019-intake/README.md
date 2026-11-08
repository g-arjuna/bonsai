# BN-019 intake triage — 8 November 2026

Base commit `8fd8cd2` (post BN-003 push). Inventory of existing
operational/release surfaces before building anything. Acceptance gates
themselves (install/upgrade run, packaged build, lab browser acceptance,
backups/recovery drill, soak) remain open.

## What already exists

- **Packaged build**: `docker/Dockerfile.bonsai` (multi-stage cargo-chef,
  rust 1.91-trixie, mold-friendly flags, `RUSTC_WRAPPER=` inside container).
  `docker-compose.yml` + `compose-external.yml` (Splunk/Elastic/Kibana/
  Grafana/Prometheus), `compose-netbox.yml`, `compose-signal-test.yml`,
  `docker/sidecars/`, `docker/grafana`, `docker/prometheus`.
- **Backups**: `scripts/backup_volumes.sh` snapshots bonsai Docker volumes
  to timestamped tar.gz; `scripts/restore_volumes.sh` referenced for
  restore. `bonsai-migrate` CLI (backup/migrate/rollback, SHA-verified,
  BN-007) covers graph DB migration. Journal backups covered via
  `bonsai_graph` volumes (BN-007 audit).
- **Release gate**: `just verify-release` = test-protocol + test-replay +
  5 Python suites + `ui npm run build` + `docker compose --profile
  standalone config -q` (config validation only, not a built-image gate).
- **Inner loop**: justfile `check-fast`/`test-protocol`/`test-replay`,
  `doctor`, `dev-up`/`dev-down`.
- **Lab acceptance**: BN-003 acceptance gate now runs the full browser
  journey against genuine FRR VRF BMP (this commit's evidence); BN-006
  bounded live profile (EOS + SR Linux) complete.
- **Observability/limits**: BN-009 sidecar lifecycle (heartbeat metrics,
  health gate, jobs API, memory shedding, systemd unit) — soak acceptance
  still open there too.

## Gaps vs BN-019 acceptance

1. No executed install-from-package or upgrade-in-place run recorded.
2. `verify-release` validates compose config but does not build/pull the
   packaged image or boot it.
3. `backup_volumes.sh`/`restore_volumes.sh` exist but no documented
   backup→restore drill evidence; `bonsai-migrate` rollback is unit-tested,
   not drilled on a live deployment.
4. No soak/endurance run (sidecar restart/overload, BMP stream longevity,
   journal growth) — overlaps BN-009's remaining soak.
5. Legacy clean-slate / seven-day handoff criteria need reconciling to the
   read-only prototype boundary (per backlog note).

## Suggested first bounded slice

Packaged-build + boot gate: `docker build -f docker/Dockerfile.bonsai` +
compose `--profile standalone` up + `/health` + routing proxy smoke,
recorded as evidence. Then backup/restore drill on that deployment.
Soak design should be shared with BN-009 rather than duplicated.
