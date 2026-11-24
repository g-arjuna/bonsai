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
