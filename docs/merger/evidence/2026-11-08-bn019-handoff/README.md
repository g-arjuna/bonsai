# BN-019 handoff reconciliation — legacy clean-slate / 7-day criteria — 8 November 2026

Maps the archived CV5/DV4 release criteria onto the current product
boundary (integrated development prototype, read-only routing
observability; operational device writes disabled). Source criteria:
`docs/backlog_archive/BONSAI_CONSOLIDATED_BACKLOG_CV5.md` Tier 7
(T7-1..T7-4) and Tier 1 clean-slate, DV4 D4-19.

## Criterion-by-criterion disposition

| Legacy criterion | Status under current boundary | Where it lives now |
|---|---|---|
| Clean-slate teardown + rebuild (CV5 Tier 1) | **Satisfied in substance** — volume backup→restore drill, install-from-package + upgrade-in-place drills, project-scoped teardown all executed on the packaged deployment | BN-019 evidence 2026-11-08 |
| Packaged/reproducible build | **Satisfied** — `docker/Dockerfile.bonsai`, `bonsai:latest` image, boot gate healthy | bn019-packaged-build |
| Install / upgrade run | **Satisfied** — `scripts/install.sh` docker path, idempotent + forced-image upgrade verified | bn019-install |
| Backup / recovery | **Satisfied (volume level)** — `backup_volumes.sh`/`restore_volumes.sh` project-prefix fix + sentinel drill; `bonsai-migrate` SHA-verified drill | bn019-packaged-build, bn019-migrate |
| Browser/lab acceptance | **Satisfied** — BN-003 gate 12/12 on genuine FRR stream AND 12/12 against the packaged deployment | bn003-acceptance-genuine, bn019-packaged-browser |
| Hands-off 7-day proof (T7-2..T7-4: daily reports, sync branches, chaos uninterrupted, governor activation) | **Deferred / out of scope** — presumes the autonomous remediation + chaos + cron-sync product that the read-only prototype does not claim. The equivalent endurance evidence is the shared BN-009/BN-019 soak (restart/overload, no silent data loss), still open | BN-009, BN-019 soak |
| Daily-check scripts (`bv5_daily_check.sh`, `daily_sync.sh`, crontab) | **Historical** — scripts exist but target the retired CV-era deployment shape (git-sync branches, cloud instance); not part of the compose/standalone boundary | archived backlog |
| Archive bytes growing / chaos cycles / governor activation | **Deferred** — chaos runner and governor are development aids; no acceptance claim in the prototype boundary | BN-014/016 scope |

## Reconciliation statement

The CV5 Tier-7 "hands-off 7-day" gate was written for the full autonomous
product (cron-synced deployments, chaos injection, remediation governor,
GNN calibration). The current Bonsai boundary is a **read-only routing
observability prototype**: no autonomous remediation, no operational
device writes, no cron/cloud-sync deployment. The legacy criteria are
therefore reconciled as:

- Everything verifiable within the prototype boundary (install, upgrade,
  packaged build, backups, browser acceptance, migration) — **done and
  evidenced**.
- Everything requiring the deferred product surface (7-day unattended
  proof, chaos soak, governor activation, sync branches) — **explicitly
  deferred** to BN-009 (soak), BN-014 (shadow pilot), BN-016/017
  (telemetry/DDoS scope). They are not silently dropped; they remain in
  the unified queue under their BN IDs.

A future operational pilot (BN-014) is where a time-boxed unattended
window would be re-attempted, against the then-current deployment model.
