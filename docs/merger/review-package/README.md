# BN-022 Secure Core — Independent Review Package (2026-11-16)

Status: **prepared, awaiting qualified independent reviewer.**
This package is NOT a self-approval; implementer review, ChatGPT static
source inspection and CI green runs do not constitute formal acceptance
(WP2.9 remains open).

## Candidate under review

- PR #17 `feat/bn022-auth-storage` @ `5570585` (contains an equivalent
  cherry-pick of PR #16's local-first CI change as `549568b`; the
  `ci.yml` content is byte-identical to PR #16 @`95cd0f8`).
- PR #16 `fix/bn019-b6-fail-closed-ci` @ `95cd0f8` — merge first.
- PR #18 `docs/bn021g2-otlp-contract` @ `9e2459d` — docs-only; its branch
  still carries the OLD auto-trigger `ci.yml` and must absorb the
  `workflow_dispatch`-only version (merge main after PR #16) before merge.
- Base: `main` @ `e071cc7`. All three PRs verified merge-clean against
  main via `git merge-tree` (GitHub `mergeable=false` was stale state,
  not a real conflict).
- Promotion order: PR #16 → PR #18 (after branch update) → PR #17.

## Index of evidence

| Topic | Location |
|---|---|
| Auth storage design & migration contract | `docs/auth_storage.md` |
| Backup/restore runbook (+live addendum §7, offline rotation contract) | `docs/auth_backup_restore_runbook.md` |
| Recovery exercise @549568b (70 PASS, pre-harness-fix) | `docs/merger/evidence/2026-11-15-bn022-final-recovery/` (incl. failed-run-before-fix log) |
| Recovery confirmation @621955f (**79 PASS / 0 FAIL**) | `docs/merger/evidence/2026-11-16-bn022-final-recovery-confirmation/` |
| Earlier live recovery (D1) | `docs/merger/evidence/2026-11-15-bn022-live-recovery/` |
| SEC-01..SEC-20 dispositions | PR #17 description + commit history; SEC-19/20 in c8f83fd, residuals in 2f32b3d |
| Named suites | `tests/bn022_*.rs`, `tests/bn022_sec19_sec20.rs`, `tests/b11_b12_b13_routing_workflow.rs` |
| Trusted gates L0–L3 | `scripts/ci/run-local.sh --trusted --shared-target l0 l1 l2 l3` — see "Gate record" below |
| Build-time log | `docs/build-times/runs.csv` |

## Gate record (exact tested SHAs)

| SHA | Result | Notes |
|---|---|---|
| 7fb3d53 | L0/L1 PASS, **L2 FAIL**, L3 not run | `bn022_config_items_endpoint_filters_sensitive_classes` expected 403/410, got 400 — ordering regression from 2f32b3d (allowlist check preempted credential-class rejection). Log: `/tmp/final-gates-7fb3d53.log` |
| df30cb7 | L0 FAIL (rustfmt on remediation.rs) | fixed in 29d1497 |
| 29d1497 | L0/L1/L2 PASS, **L3 FAIL** | `b11_..._graceful_degradation` fixture planted `routing_service_token` as plaintext app_config — rejected by SEC-19 structural guard (correct behavior); fixture fixed in 621955f+5570585 |
| 621955f | L0 FAIL (rustfmt on b11 fixture) | fixed in 5570585 |
| 5570585 | **in flight** at package time | `scripts/ci/run-local.sh --trusted --shared-target l0 l1 l2 l3`; result to be recorded on completion |

Focused verification already executed at 5570585-equivalent source:
`cargo test --test bn022_graph_read_boundary` 5/5 PASS;
`cargo test --test b11_b12_b13_routing_workflow` 3/3 PASS;
recovery harness 79 PASS / 0 FAIL.

## Security architecture summary

- Credential authority: `AuthStore` (`bonsai_auth.db`, sqlite, age/scrypt
  sealed under vault passphrase or 0600 keyfile) — users, API-key
  verifiers, JWT keys (active+retired), revocations, service tokens,
  sealed quarantine. Graph `ConfigItem`s are audit tombstones only.
- AuthN/Z: JWT (HS256, rotation overlap + emergency) + scoped `bsk_`
  API keys + env bootstrap admin; middleware fail-closed; local account
  record authoritative for local/LDAP identities.
- Structural boundary: `upsert_config_item` guards effective persisted
  identity — retired credential classes and protected secret names
  accept only strictly-verified migration tombstones. The REST handler
  restores correct rejection statuses (403 sensitive/non-admin,
  410 retired/secret-name) via a targeted id lookup; the store write
  boundary remains the authoritative check under the write lock.
- Destination authority (SEC-20): `routing_service_url` admin-only;
  credentialed requests never follow redirects; endpoint must be valid
  http(s) URL without userinfo.
- Migration: staged detect→conflict-check→import→verify→quarantine→
  scrub→manifest+marker (atomic); idempotent resume; tombstones bound
  to exact record identity + protected counterpart; SEC-19 strict
  tombstone certification at startup.
- Backup: `bonsai auth backup|verify-backup|rotate-emergency|
  set-service-token|list-service-tokens` — offline, manifest v2 with
  sha256 binding + per-record age decrypt-verify; graph-only backup is
  NOT complete DR. Offline/quiesced emergency rotation contract in
  runbook §7.

## Authorization matrix (HTTP)

| Surface | anon | viewer | operator | admin | scoped key |
|---|---|---|---|---|---|
| GET reads | 401 | 200 | 200 | 200 | per-scope |
| POST /api/config-items ordinary | 401 | 403 | 204 | 204 | 403 |
| config-items protected names/classes | 401 | 403 | 403 | 204/GONE* | 403 |
| /api/auth/users, apikeys | 401 | 403 | 403 | 200 | 403 |
| routing_service_url write | 401 | 403 | 403 | 204 | 403 |

*retired credential classes → 410 GONE for everyone via REST;
secret-bearing names → 410 GONE; sensitive classes → 403 non-admin.

## Recovery acceptance coverage (79-check harness)

Live positive roundtrip; backup/verify/loss/restore/restart; pre-backup
JWT validity after restore; API-key auth + revoked-key denial after
restore; post-restore scoped routing credential consumption through the
configured downstream (matching speaker proxied with sealed internal
token, wrong speaker denied pre-proxy); historical-JWT restoration +
offline emergency rotation reconciliation; legacy migration
(tombstoned graph, no plaintext credential readable); graph/AuthStore
mismatch both directions with EXPLICIT certification failure (HTTP 503
+ log evidence, or proven-live auth with successful env-admin login);
negative matrix: wrong passphrase, missing key material, corrupt /
truncated / tampered-manifest / missing-manifest / unsupported-format
snapshots, wrong-key startup fail-closed, permissive-mode DB tightened
to 0600 on open, corrupt-ciphertext decrypt-verify rejection.

## Known limitations / reviewer questions

1. JWT key cache is process-local: cross-process rotation requires
   restart (documented runbook §7). Acceptable for single-writer
   authority; confirm policy.
2. `jwt_revoked` tombstone certification accepts durable manifest
   evidence because revocations legitimately expire/prune — confirm
   this does not weaken mismatch detection.
3. Arbitrary secret names outside PROTECTED_CONFIG_NAMES/retired
   classes are operator responsibility (no enumeration possible).
4. Plain-http routing endpoints permitted for loopback/dev; remote
   deployments should use https (documented, not enforced).
5. D2.4 residuals: auth-cancellation failure accounting and
   quarantine-seal edge cases were reviewed; existing suites cover the
   required regressions — no new defect confirmed, but dedicated
   cancellation-race test not added this session.
6. verify-backup does not gate on file mode bits (backup artifacts are
   0600 by construction; the live-open path enforces/tightens 0600 —
   now covered by an explicit harness negative).
7. Manifest digest proves integrity against corruption, NOT
   authenticity against an attacker who can rewrite both snapshot and
   manifest (documented in auth_storage.md; authenticated manifests are
   future work).

## Local-first CI policy

Owner policy (2026-11-15): ZERO PAID REMOTE CI. `ci.yml` on PR #16/#17
branches is `workflow_dispatch`-only; definitions preserved for future
activation. Remote CI is deferred, not failed — canonical acceptance is
`scripts/ci/run-local.sh --trusted --shared-target l0 l1 l2 l3`.
PR #18's branch still carries the auto-trigger variant and must not be
merged before absorbing the deferred version.

## Remaining gates

- L4 lab acceptance is NOT claimed by L0–L3; separate authorized lab
  scope required.
- WP2.9 qualified independent review outstanding — this package is
  preparation, not approval.

## Reproduction

```bash
# focused suites
cargo test --test bn022_sec19_sec20 --test bn022_auth_closure \
  --test bn022_auth_storage --test bn022_graph_read_boundary \
  --test b11_b12_b13_routing_workflow
# trusted local gates (canonical)
scripts/ci/run-local.sh --trusted --shared-target l0 l1 l2 l3
# recovery harness (disposable, loopback-only, synthetic creds)
cargo build --bin bonsai --bin wp28_fixture_seed
KEEP_WORK=1 scripts/recovery/final_candidate_recovery.sh target/debug/bonsai
```
