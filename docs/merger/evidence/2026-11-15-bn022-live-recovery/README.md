# BN-022 WP2.8 — Live operational backup & recovery evidence

Date: 2026-11-15. Candidate: `feat/bn022-auth-storage` @ `8e02a59`
(binary `target/debug/bonsai`, built from this worktree).

All credentials below are synthetic; all listeners were loopback-only
(127.0.0.1:13000/13001/13002, gRPC 15051–15055). No production
connectivity, no production credential material.

## Disposable environments

| Instance | Graph | AuthStore | HTTP | Purpose |
|---|---|---|---|---|
| live | `live/data/bonsai.db` | env passphrase `BONSAI_VAULT_PASSPHRASE` | 127.0.0.1:13000 | positive roundtrip + negatives |
| legacy | `legacy/data/bonsai.db` | same | 127.0.0.1:13001 | pre-migration graph → real migration |
| pregraph | `pregraph/data/bonsai.db` | migrated auth store copy | 127.0.0.1:13002 | older-graph/newer-authstore mismatch |

Fixture seeder: `src/bin/wp28_fixture_seed.rs` writes legacy credential
`ConfigItem`s via `upsert_config_item_unchecked` (fixture-only path,
server stopped) so the real `ensure_migrated` pipeline runs at boot.

## D1.1 — Positive roundtrip (live instance)

1. `BONSAI_REQUIRE_AUTH=1`, env vault passphrase, env bootstrap admin.
   Server started; `AuthStore` auto-created `bonsai_auth.db` (schema v4).
2. Created via `POST /api/auth/users`: `recovery-op` (operator),
   `recovery-view` (viewer), `recovery-adm2` (admin).
3. Created via `POST /api/auth/apikeys`: `recovery-read-key` (scope
   `read`), `recovery-scoped-key` (scope `routing:read:speaker=10.0.0.1`),
   `recovery-doomed-key` (scope `write`, then `DELETE` → revoked).
4. Service tokens via `bonsai auth set-service-token`:
   `routing_service_token`, `internal_service_token` (sealed).
5. Baseline authz verified live: anon→401; viewer/operator/admin JWT
   login OK; `POST /api/auth/users` admin-only (viewer/operator 403);
   `POST /api/config-items` viewer 403 / operator 204; read-scoped key
   GET 200 / POST 403; scoped routing key 403 on non-routing path;
   revoked key 401.
6. `bonsai auth backup --db … --dest backup/run1/bonsai_auth.db` →
   manifest v2: users=3 api_keys=3 jwt_keys=1 jwt_revoked=0
   quarantined=0 service_token_count=2 migration_state=complete.
7. `bonsai auth verify-backup` → OK; all 3 encrypted records decrypt.
8. Stopped server, moved `bonsai_auth.db` aside (simulated loss),
   `install -m 0600` snapshot back, restarted.
9. Post-restore verified: anon 401; all four logins succeed; pre-backup
   JWT still valid (key material recovered); API-key scope enforcement
   intact; revoked key still 401; `list-service-tokens` shows both
   tokens; role authorization unchanged.

## D1.1 — Migration + quarantine (legacy instance)

Seeder wrote: legacy user (SHA-256 verifier), malformed user, legacy
api_key, canonical `jwt-secret-v1`, extra `jwt_secret` row, legacy
`jwt_revoked`, `session` row, and app_config protected names
(`routing_service_token` enabled, `internal_service_token` DISABLED,
`service_token`, `admin_password`, `jwt_secret`).

Server boot ran real migration (log: `legacy/evidence/stdout.log`):
`users=1 api_keys=1 jwt_revocations=1 malformed=2 quarantined=7`.

Verified over HTTP/CLI:
- `legacy-viewer` login OK (legacy SHA-256 verifier accepted, upgraded);
  legacy `bsk_` API key authenticates (scope `read`).
- Graph `/api/config-items` (admin): all 12 records are disabled
  tombstones `{"migrated_to":"auth_store","migration":"bn022-auth-migration-v1",…}`
  — zero plaintext credential material remains graph-readable.
- `list-service-tokens`: ONLY `routing_service_token` — the DISABLED
  `internal_service_token` was quarantined, NOT activated (SEC-01
  disposition contract holds).
- Non-admin reads of protected names → `"[REDACTED]"`.
- Backup manifest: quarantined=6; verify-backup decrypts all 8 sealed
  records OK.

## D1.2 — Negative / historical cases (all fail closed, observed)

| Case | Command/setup | Observed |
|---|---|---|
| Wrong passphrase | `verify-backup` with wrong `BONSAI_VAULT_PASSPHRASE` | `stored content passphrase rejected` — exit 1 |
| Missing key | `verify-backup` no env, no keyfile | `keyfile … is missing` — exit 1 |
| Wrong key at startup | server start, wrong passphrase | `AuthStore::open` fails, process exit 1, no serve |
| Corrupt snapshot | 1-byte flip in snapshot | manifest sha256/size mismatch — exit 1 |
| Truncated snapshot (interrupted install) | `head -c 40000` copy + server start | `database disk image is malformed` — exit 1 |
| Tampered manifest | `user_count` 3→99 | `manifest count mismatch` — exit 1 |
| Unsupported schema | manifest `format=v1`, `schema_version=1` | `unsupported backup manifest format` — exit 1 |
| Missing manifest | snapshot without sidecar | `backup manifest … is missing` — exit 1 |
| Corrupt ciphertext | `service_tokens.token_enc` overwritten + manifest re-digested | `service token 'routing_service_token' does not decrypt` — exit 1 |
| Emergency rotation | `bonsai auth rotate-emergency` on live db | old JWTs invalid **after restart**; fresh login OK |
| Restore predating rotation | restored run1 (pre-rotation) snapshot | pre-rotation JWT accepted again (resurrection confirmed) → `rotate-emergency` + restart → JWT 401, login OK |
| Older graph + newer AuthStore | pre-migration graph + migrated auth db | startup certification bails: `migration_state=complete but the attached graph still contains unmigrated credential records`; all auth 401/503 |
| Older AuthStore + newer graph | tombstoned graph + fresh unmigrated auth db | migration aborts: `tombstone 'user/cfg-user-legacy-viewer' claims 'imported' but no protected counterpart exists`; auth denied |

## Findings recorded during the exercise

- **F-R1 (documented, by-design boundary):** `rotate-emergency` executed
  by the offline CLI while the server runs is NOT visible to the running
  server's process-local JWT key cache (`jwt_generation` is
  process-local; cache freshness also depends on `verify_deadline_ns`).
  A live-rotated old JWT kept verifying (200) until restart; after
  restart it correctly failed (401). Operator procedure must be:
  rotate while quiesced, then start. Runbook updated accordingly.
- **SEC-20 confirmed live:** `POST /api/config-items` with Operator role
  wrote `app_config/routing_service_url = http://169.254.169.254:80`
  (204); `GET /api/routing/v1/health` then proxied toward that
  destination (503 unreachable — the credential-bearing request path
  was exercised). Viewer write → 403. This is the release-blocking gap
  corrected under D2.3.

## Artifact notes

- `live/backup/run1/`, `legacy/backup/run1/`: v2 manifests + snapshots
  retained under `/tmp/bn022-recovery/**` (contain no plaintext secrets;
  snapshots are ciphertext-only but are NOT committed — they are
  disposable exercise artifacts).
- Logs under `*/evidence/` contain no tokens, keys or passphrases
  (verified by grep); JWTs appear only in `/tmp` scratch files, not here.
