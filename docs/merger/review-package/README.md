# BN-022 Secure Core — Independent Review Package (2026-11-15)

Status: **prepared, awaiting qualified independent reviewer.**
This package is NOT a self-approval; ChatGPT source inspection and
implementer review do not constitute formal acceptance (WP2.9 open).

## Candidate under review

- PR #17 `feat/bn022-auth-storage` @ `c8f83fd` (includes PR #16 @589f06b)
- Base: `main` @ `e071cc7`
- Diff: `git diff e071cc7...c8f83fd` on g-arjuna/bonsai PR #17

## Index of evidence

| Topic | Location |
|---|---|
| Auth storage design & migration contract | `docs/auth_storage.md` |
| Backup/restore runbook (+live addendum §7) | `docs/auth_backup_restore_runbook.md` |
| Live recovery exercise (D1) | `docs/merger/evidence/2026-11-15-bn022-live-recovery/` |
| SEC-01..SEC-20 dispositions | PR #17 description + commit history; SEC-19/20 fixed in c8f83fd |
| Named suites | `tests/bn022_*.rs`, results below |
| Trusted gates L0–L3 | `/tmp/pr17-gates-c8f83fd.log` (running at package time) |
| Prior gate evidence PR#16 | `/tmp/pr16-gates-589f06b.log`, `docs/merger/evidence/2026-11-19-bn019-b6-fail-closed-ci/` |

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
  accept only strictly-verified migration tombstones.
- Destination authority (SEC-20): `routing_service_url` admin-only;
  credentialed requests never follow redirects; endpoint must be valid
  http(s) URL without userinfo.
- Migration: staged detect→conflict-check→import→verify→quarantine→
  scrub→manifest+marker (atomic); idempotent resume; tombstones bound
  to exact record identity + protected counterpart.
- Backup: `bonsai auth backup|verify-backup|rotate-emergency|
  set-service-token|list-service-tokens` — offline, manifest v2 with
  sha256 binding; graph-only backup is NOT complete DR.

## Authorization matrix (HTTP)

| Surface | anon | viewer | operator | admin | scoped key |
|---|---|---|---|---|---|
| GET reads | 401 | 200 | 200 | 200 | per-scope |
| POST /api/config-items ordinary | 401 | 403 | 204 | 204 | 403 |
| config-items protected names/classes | 401 | 403 | 403 | 204/GONE* | 403 |
| /api/auth/users, apikeys | 401 | 403 | 403 | 200 | 403 |
| routing_service_url write | 401 | 403 | 403 | 204 | 403 |

*retired credential classes → 410 GONE for everyone via REST.

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
6. WP2.8 live exercise executed (evidence above); negative matrix
   complete except invalid-permissions nuance (backup artifacts are
   0600 by construction; verify-backup does not re-check mode bits).

## Reproduction

```bash
# live recovery harness (disposable, loopback-only)
# fixture seeder: cargo build --bin wp28_fixture_seed
# see docs/merger/evidence/2026-11-15-bn022-live-recovery/README.md
cargo test --test bn022_sec19_sec20 --test bn022_auth_closure \
  --test bn022_auth_storage --test bn022_graph_read_boundary
scripts/ci/run-local.sh --trusted --shared-target l0 l1 l2 l3
```
