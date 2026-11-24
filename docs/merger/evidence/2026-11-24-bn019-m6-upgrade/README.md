# BN-019 — Packaged M6 upgrade & recovery slice (2026-11-24)

Bounded packaged-upgrade assurance for the M6 correctness closure
(PR #80, merge commit `18fa577`). Two legs, both executed against real
packaged images — no bare-binary shortcuts.

## Images

| Role | Tag | Image ID | Source |
|---|---|---|---|
| Old (pre-PR80 baseline) | `bonsai:bn019u2-cdfcac1` | `sha256:e1164c58b7af` | `origin/main` @ `cdfcac1` |
| New (corrected) | `bonsai:bn019u2-18fa577` | `sha256:39e71dd832dc` | `main` @ `18fa577` (PR #80 merged) |

New-image build: ~10 min wall (warm BuildKit cache; lbug CMake layer
cached, workspace crates rebuilt). Old-image build: ~85 min (cold
BuildKit cache, lbug CMake dominant). See `docs/build-times/runs.csv`.

## Leg A — packaged auth-migration upgrade (disposable runtime)

Runtime: `/tmp/bn019-u3/legacy-runtime` + `legacy-creds`, seeded by
`wp28_fixture_seed` (test-fixtures build, lane binary) into a FRESH
graph db while the server was stopped — the seeder's disposable-path
guard was respected (fresh dir, no pre-existing db).

Drill config `/tmp/bn019-u3/bonsai-drill.toml`: `auth_mode = "required"`,
synthetic vault passphrase + bootstrap admin (env-only, no real
credentials). Container ran `--user 1000:1000` to match host-owned
bind mounts (uid-ownership checks in AuthStore verified live — a
uid-10001-owned `bonsai_auth.db` was correctly refused).

### Old image boot (`u3-old-image-boot-migration.log`)

- `auth storage migration complete users=1 api_keys=1
  jwt_revocations=1 malformed=2 quarantined=7` — real
  `AuthStore::ensure_migrated` pipeline on packaged binary.
- `GET /api/config-items` (admin): all 12 legacy records are disabled
  tombstones `{"migrated_to":"auth_store", ...}` — zero plaintext
  credential material remains graph-readable
  (`u3-old-configitems-tombstones.json`).
- `legacy-viewer` (SHA-256 verifier) login → JWT issued; migrated
  `bsk_` API key authenticates (`api_key:cfg-key-legacy-read`, viewer);
  wrong key → 401 (`u3-old-legacy-apikey-me.json`).

### Upgrade to `18fa577` (`u3-upgraded-auth-verify.log`, `u3-new-image-boot.log`)

- Same mounted runtime + credentials volumes; container recreated on
  the new image.
- No re-migration (idempotent; `migration_state=complete` preserved).
- Post-upgrade verified live: admin login OK; `legacy-viewer` login OK
  (role viewer); legacy API key still authenticates; all 12 tombstones
  persist; `PRAGMA user_version` stays **15** — SQLite schema
  compatible across the upgrade.

## Leg B — live lab upgrade (bn019u2 compose project)

The U2 lab (SR Linux containerlab `clab-bonsai-bn019-srl1`,
`172.100.119.11:57400`, TLS `tls_verify` with lab CA) ran the OLD image
with: targeted compiled schema `ysc-0f4cc548…` (compiler v3, 13
modules / 420 paths, active), durable `schema_bindings` approval
rev 1, explicit device schema selection (ghost `192.0.2.10`), and an
observed SAMPLE subscription.

Compose recreated with `BN019_IMAGE=bonsai:bn019u2-18fa577` (same
named volumes — real packaged upgrade, not a fresh install).

### Fail-closed on stale authority (observed, `u3-live-boot-and-recovery.log`)

- `schema-bound subscriber blocked — approval is not current effective
  authority` for `172.100.119.11:57400`.
- `acquisition plan not submitted … detail=schema_binding_not_authoritative`.
- Applicability snapshot freshness=**stale**,
  `catalogue_integrity_failure` / `schema_version_changed` (v3→v4
  compiler bump detected).
- Binding preview correctly **rejected**: `snapshot_not_fresh`,
  `SchemaVersionChanged`, `schema_snapshot_mismatch`.
- Durable rows preserved (not deleted): `schema_bindings` rev 1 and
  `device_schema_selections` rev 1 intact in `bonsai_config.db`
  (user_version 15 unchanged).
- Ghost device explicit selection still resolves to stale
  `ysc-0f4cc548` → `catalogue_state: catalogue_integrity_failure`
  (explicit selections fail closed identically — CR-05 contract).

### Operator recovery (re-validation, not silent repair)

1. Recompiled identical 13-module scope under compiler v4 →
   `ysc-51aa768a66cde438a6fd39a8d4c6c2d2`, complete, activated
   (`bn019-u3-recompile.json`).
2. `POST …/yang-applicability/refresh` (operational) → snapshot 7,
   applicable, fresh (`bn019-u3-refresh.json`).
3. Binding preview → permitted; activate with plan_id +
   binding_fingerprint + expected_config_revision →
   **approval_revision 2, state active** (`bn019-u3-activate.json`).
4. Subscriber reconnected `tls_verify`, subscribed plan
   `bc6423936d0d7f39`; `interface[name=*]/subinterface[index=*]/statistics`
   SAMPLE → **observed** with fresh `last_observed_at_ns`
   (`u3-device-srl1-post-recovery.json`).
5. Ghost re-selection: PUT schema-selection → rev 2, effective schema
   `ysc-51aa768…`, catalogue_state cleared (`u3-ghost-reselect.json`).

## Honest caveats

- Ghost device `192.0.2.10` has no route; its post-upgrade acquisition
  rejection (`legacy plaintext gNMI transport … set security="plaintext"`)
  is the new rev's intentional fail-closed transport policy, unrelated
  to schema authority. Device record untouched.
- `hw-mac-address` SAMPLE remains `subscribed_but_silent` (path yields
  no data on this srlinux rev) — same as pre-upgrade.
- Broad 542-module SR Linux compile remains `partial` (1666
  diagnostics); the bounded slice used the targeted 13-module closure.
- L4 canonical lab tier was NOT run; this slice is targeted BN-019
  evidence, not full L4 acceptance.

## Acceptance matrix and classification corrections (addendum, 2026-11-24)

Corrections to the classifications above, after independent review. The
evidence files are unchanged; only the interpretation is refined.

| Requirement | Evidence | Result |
|---|---|---|
| Real packaged upgrade (old→new image, same volumes) | `u2-provenance.txt`, image IDs, compose recreate | PASS — PACKAGED |
| SQLite schema preservation v15 → v15 | `PRAGMA user_version` 15 before/after (`u2-sqlite-pre-summary.txt`, boot logs) | PASS — PACKAGED |
| SQLite v14 → v15 migration | Both images are schema v15; no packaged v14 fixture exercised. Unit-level migration chain exists (`src/sqlite_store.rs` migrate 1→15); no dedicated v14→v15 regression found — tracked as outstanding, see below | PARTIAL |
| Legacy AuthStore migration | `u3-old-image-boot-migration.log` (`users=1 api_keys=1 jwt_revocations=1 malformed=2 quarantined=7`), `u3-old-configitems-tombstones.json` | PASS — PACKAGED |
| AuthStore negative conditions (revoked JWT rejected, wrong key 401, tombstones cannot authorize) | Wrong-key 401 claimed in drill; committed artifact shows positive auth only. `tests/bn022_auth_storage.rs` (`migrated_graph_rows_cannot_authorize`, quarantine/conflict negatives) cover the semantics | PARTIAL — packaged positive evidence + regression coverage |
| Old-schema authority rejection (compiler v3 → v4) | `u3-live-boot-and-recovery.log` fail-closed block + stale applicability | PASS — PACKAGED |
| Explicit operator recovery | recompile v4 `ysc-51aa768`, refresh, activate rev 2, ghost reselect rev 2 | PASS — PACKAGED |
| Inherited (global catalogue) authority recovery | srl1 `172.100.119.11` has `selected_schema_id: null` (inherits global active schema). Pre-upgrade effective = v3 `ysc-0f4cc548`; post-recovery effective = v4 `ysc-51aa768` with observed SAMPLE telemetry (`bn019-u3-preview2.json`, `u3-device-srl1-post-recovery.json`) | PASS — PACKAGED (inherited mode) |
| CR-05 inherited global drift between two valid v4 bundles | Deterministic regressions on merged main (PR #80) | PASS — REGRESSION (packaged leg exercised compiler-version drift, not same-version bundle drift) |
| Collector convergence / unrelated device continuity | Only one live device (srl1) + ghost (unreachable, see below). No second independent valid device in the packaged leg | PARTIAL |
| Ghost device schema withdrawal | Ghost `192.0.2.10` is unreachable; post-upgrade rejection is the plaintext-transport policy (`migration_required`), which masks any schema-path behavior. NOT evidence for device-scoped withdrawal or multi-device isolation | NOT_EXECUTED (packaged); device-scoped withdrawal covered by `tests/gnmi_subscription_e2e.rs` multi-device regression — PASS — REGRESSION |
| Restart safety | Post-restore restart of both containers healthy, `phase="ready"`, auth enforced (401), device state intact (`u4-backup-restore.txt`) | PASS — LIVE LAB |
| Interrupted transition | Not fault-injected in packaged leg; commit-first fencing covered by `schema_binding_schema_mismatch` regression (M6-CR-03/05, PR #80) | PASS — REGRESSION |
| Backup/restore | `scripts/backup_volumes.sh` + `restore_volumes.sh` on bn019u2 volumes, post-restore healthy + state intact (`u4-backup-restore.txt`) | PASS — LIVE LAB (same-version tooling check, not corruption/cross-version drill) |
| Rollback (old binary + pre-upgrade data) | Old image `bonsai:bn019u2-cdfcac1` retained locally; pre-upgrade DB copies under `/tmp/bn019-u3/`; no packaged rollback drill executed | NOT_EXECUTED |
| Historical graph preservation | Graph volume backed up/restored byte-identical; tombstone audit rows preserved (`u3-old-configitems-tombstones.json`) | PASS — PACKAGED + LIVE LAB |
| Bounded soak | `u3-bounded-soak-stats.csv`: first sample 2026-11-24T08:40:28Z, last 08:54:56Z → ~14.5 min elapsed, 3 containers, docker stats 60 s cadence | PASS — bounded packaged smoke/soak observation (~14.5 min, not a duration certification) |
| Full-duration soak (BN-009/BN-019 release criterion) | Not run | NOT_EXECUTED — outstanding release condition |
| Canonical L4 | Not run; live-lab legs above are targeted BN-019 evidence only | NOT_EXECUTED |

### U1 vs U2 distinction

- **U2 (executed here):** genuine packaged upgrade `cdfcac1` → `18fa577`,
  both SQLite schema v15 — proves compiler-v3→v4 authority transition and
  v15 persistence across a real image/container/volume upgrade.
- **U1 (SQLite v14 → v15 packaged migration):** NOT executed — no faithful
  earlier-version packaged fixture was produced, and none is claimed.
  The v15 step (selection-epoch table + backfill) is small; a bounded
  v14→v15 persistent-database regression is being added separately rather
  than rebuilding a historical image for appearances.
- **AuthStore migration** is a separate mechanism (graph records →
  `bonsai_auth.db`), not SQLite schema migration; it is evidenced by Leg A.

### Persisted rows vs active authority

Pre-upgrade `schema_bindings` (rev 1) and `device_schema_selections`
(ghost, rev 1) rows were preserved, not deleted — correct behavior:
historical approval evidence is retained. Post-upgrade they are NOT
operational authority: the v3 binding fails closed
(`schema_binding_not_authoritative`), the ghost's stale v3 selection
resolves to `catalogue_integrity_failure`, and only explicit operator
re-validation produced rev-2 authority. No stale persisted row was
presented as an active executable plan (binding preview correctly
rejected with `snapshot_not_fresh`/`SchemaVersionChanged`).

### Soak/resource observations (bounded)

Host: 8 CPU / 46.7 GiB, compose project `bn019u2`, 2 bonsai containers +
1 SR Linux containerlab node; `docker stats` at 60 s cadence,
2026-11-24T08:40:28Z → 08:54:56Z (~14.5 min, 15 samples per container).
Standalone Core RSS 585–616 MiB (mem_pct ~1.2%), routing sidecar
~9.3 MiB; CPU 0.00–1.80% per sample (docker-stats instantaneous %, not
cumulative — treat as indicative only). No container restarts observed
during the window; SAMPLE telemetry continuity verified separately via
post-recovery observed paths. WAL/DB sizes: graph volume backup 2.4 MB,
creds volume 1.3 KB at backup time. This is a bounded packaged
smoke/soak observation on a single-device lab — NOT enterprise scale
certification. The full-duration BN-009/BN-019 soak remains an
outstanding release condition.

### SR Linux schema coverage limitation

Targeted closure used for the successful subscription: 13 modules /
420 paths, compiled `ysc-51aa768a66cde438a6fd39a8d4c6c2d2` (compiler v4).
The broad 542-module SR Linux 26.3.1 catalogue compilation remains
`partial` with 1,666 diagnostics (multiple diagnostics share root
causes; not 1,666 independent defects). Proven: the architecture
supports a genuine SR Linux acquisition workflow end-to-end. Not
proven: full native-model coverage of the SR Linux YANG surface.
Retained as a scoped YANG completeness / vendor-coverage backlog item.
