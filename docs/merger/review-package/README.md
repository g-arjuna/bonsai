# BN-022 Secure Core — Independent Review Package (2026-11-16)

Status: **prepared, awaiting qualified independent reviewer.**
This package is NOT a self-approval; implementer review, ChatGPT static
source inspection and CI green runs do not constitute formal acceptance
(WP2.9 remains open).

## Candidate under review

- PR #17 `feat/bn022-auth-storage` @ `19c6be7` (contains an equivalent
  cherry-pick of PR #16's local-first CI change as `549568b`; the
  `ci.yml` content is byte-identical to PR #16 @`95cd0f8`). The trusted
  L0–L3 gate was executed at `5570585`; `621955f`, `f86ea26` and
  `19c6be7` are test-fixture/docs/evidence-only successors — no
  production `src/` change after the tested SHA.
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
| Durable sanitized gate summary @5570585 | `docs/merger/evidence/2026-11-16-bn022-l0-l3-gate/gate-summary.txt` |
| Build-time log | `docs/build-times/runs.csv` |

## Gate record (exact tested SHAs)

| SHA | Result | Notes |
|---|---|---|
| 7fb3d53 | L0/L1 PASS, **L2 FAIL**, L3 not run | `bn022_config_items_endpoint_filters_sensitive_classes` expected 403/410, got 400 — ordering regression from 2f32b3d (allowlist check preempted credential-class rejection). Log: `/tmp/final-gates-7fb3d53.log` |
| df30cb7 | L0 FAIL (rustfmt on remediation.rs) | fixed in 29d1497 |
| 29d1497 | L0/L1/L2 PASS, **L3 FAIL** | `b11_..._graceful_degradation` fixture planted `routing_service_token` as plaintext app_config — rejected by SEC-19 structural guard (correct behavior); fixture fixed in 621955f+5570585 |
| 621955f | L0 FAIL (rustfmt on b11 fixture) | fixed in 5570585 |
| 5570585 | **L0/L1/L2/L3 ALL PASS** | `scripts/ci/run-local.sh --trusted --shared-target l0 l1 l2 l3`; 48 test-result lines, 0 failures; durable sanitized summary `docs/merger/evidence/2026-11-16-bn022-l0-l3-gate/gate-summary.txt`; ephemeral full log `/tmp/final-gates-5570585.log`, run dir `target/ci/local-run-20261116-092619` |

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
4. Routing transport: plain `http` `routing_service_url` is permitted
   only for loopback destinations; non-loopback plain HTTP is rejected
   unless the operator explicitly sets
   `BONSAI_ROUTING_ALLOW_INSECURE_HTTP=1`
   (`src/routing_service/mod.rs` `insecure_http_transport_permitted`).
   HTTPS is therefore effectively required for remote deployments —
   enforced at the transport contract, not merely recommended.
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
# NOTE: wp28_fixture_seed requires the test-fixtures feature; build it
# separately so the bonsai binary stays feature-identical to the
# gate-tested build.
cargo build --bin bonsai
cargo build --features test-fixtures --bin wp28_fixture_seed
KEEP_WORK=1 scripts/recovery/final_candidate_recovery.sh target/debug/bonsai
```

---

## 2026-11-20 reconciliation update (main @ d3dd771)

PR #17 was 111 commits behind `main`. Reconciliation merge
`2ce3b2c57520558cdce3187de0dd9e10d433c76e` merges `origin/main` @
`d3dd771` into `feat/bn022-auth-storage` @ `85f4c29` (history-preserving,
no force-push). A docs/evidence-only successor commit may follow; no
production `src/` change after `2ce3b2c`.

### Conflicts resolved (4 files)

| File | Resolution |
|---|---|
| `scripts/ci/gates.sh` | Union: kept BN-022 L2 targets (`bn022_auth_storage`, `bn022_auth_closure`, `bn022_sec19_sec20`) AND new main targets (`m3_syslog_tcp_availability`, `gnmi_selective_acquisition`, `gnmi_security_connectivity`, `gnmi_assignment_lifecycle`, `wp65a_embedding_lifecycle`). |
| `src/http_server/change_capture.rs` | Kept branch side: `resolve_routing_service_endpoint` returns `Result` (fail-closed credential resolution); main's tuple signature was superseded by the merged `mod.rs`. |
| `src/graph/mod.rs` | Test `TargetConfig` literal: kept main's `security`/`client_cert`/`client_key` fields (M5.1 gNMI security contract still present in `config.rs`). |
| `src/server_startup.rs` | Union: `with_auth_policy` (branch) + `with_dev_open_credential_assignments`, `with_session_registry`, `with_subscription_plan_tx` (main M5.2B subscription lifecycle) on `CoreService`; CollectorService keeps `with_auth_policy` + `with_dev_open_credential_assignments` per main. |

All other both-sides-modified files (`cli.rs`, `lib.rs`,
`http_server/mod.rs`, `managed_devices.rs`, `remediation.rs`,
`routing_service/mod.rs`, `AGENTS.md`, `BONSAI_BACKLOG.md`,
`docs/DEVELOPMENT.md`, `docs/build-times/runs.csv`) auto-merged;
verified the effective diff vs `origin/main` is exactly the BN-022
surface (60 files, +13677/−644) with no main functionality dropped —
M5.2B trusted subscription lifecycle, collector registration/authority
fencing, verifier readiness/coverage, and remediation persistence
hardening all present.

### New evidence on `2ce3b2c`

- Focused suites: `bn022_auth_closure` 49, `bn022_auth_storage` 12,
  `bn022_graph_read_boundary` 5, `bn022_sec19_sec20` 13,
  `b11_b12_b13_routing_workflow` 3 — **82/82 PASS**.
- Recovery harness: **79 PASS / 0 FAIL** —
  `docs/merger/evidence/2026-11-20-bn022-reconcile-recovery/`.
- Canonical trusted-local gates **L0/L1/L2/L3 ALL PASS** —
  `docs/merger/evidence/2026-11-20-bn022-reconcile-gate/`
  (`run-local.sh --trusted --shared-target-dir --lbug-native`,
  sha=2ce3b2c, exit 0).

### Security review notes (implementer pass — NOT WP2.9)

Re-verified on the integrated tree: `AuthStore` remains sole credential
authority; graph `ConfigItem`s hold only certified migration tombstones;
`upsert_config_item_authorized` enforces effective-identity checks under
the write lock; retired classes/secret names fail closed (403/410);
`routing_service_url` is admin-only destination authority, credentialed
clients never follow redirects, non-loopback plain HTTP rejected unless
explicit override; migration markers are bound to the attached graph
state (`ensure_migrated` SEC-11 scan) and fail closed on mismatch;
staged detect→import→verify→quarantine→scrub→manifest ordering with
idempotent resume preserved. No new defects found; this is implementer
review only.

### Outstanding

- **WP2.9 qualified independent security review remains OPEN.** This
  package is preparation, not approval. Items needing reviewer sign-off:
  the reconciliation conflict resolutions above, JWT key-cache
  cross-process policy (limitation 1), `jwt_revoked` tombstone
  certification via durable manifest (limitation 2), and manifest
  digest authenticity scope (limitation 7).
- PR #17 must not merge before WP2.9 approval and explicit owner
  authorization.
