# BN-022 — Protected authentication storage

Status: design + implementation contract (WP2/WP3 of the secure-core
milestone). Supersedes the graph-`ConfigItem` credential storage described
in the BN-022 backlog entry.

## 1. Authentication-sensitive storage inventory

Every credential-bearing record currently persisted in the operational
graph (`ConfigItem` nodes in LadybugDB) or adjacent stores, classified by
sensitivity. Verified by code inspection on `fix/bn019-b6-fail-closed-ci`
(2026-11-14).

| # | Record | `config_class` / location | Content | Classification | Current exposure |
|---|--------|---------------------------|---------|----------------|------------------|
| 1 | JWT signing secret | `jwt_secret`, id `jwt-secret-v1` | 128-hex-char HS256 secret, auto-generated, **plaintext** | **Secret (recoverable)** | Graph-queryable by admin; in graph backups/exports |
| 2 | User password verifiers | `user` | `UserContent{password_hash (Argon2id PHC or legacy SHA-256 hex), role, last_login_ns}` | **Verifier (sensitive)** — offline-crackable, enables offline attack | Same |
| 3 | API-key verifiers + scopes | `api_key` | `ApiKeyContent{key_hash (SHA-256), scope, expires_at_ns, last_used_at_ns}` | **Verifier (sensitive)** — SHA-256 of high-entropy key; scope is authorization metadata | Same |
| 4 | JWT revocation (logout denylist) | `jwt_revoked` | `{jti → exp}` | Security state (not secret, but auth authority) | Same |
| 5 | Legacy session tokens | `session` (`bst_…`) | legacy opaque session rows | Secret-adjacent; already ignored by JWT middleware | Same |
| 6 | Certificate bindings | `cert_config` | `vault:<alias>` **references only** — key material lives in `CredentialVault` | Safe operational metadata (reveals vault alias names only) | Low; retained in graph |
| 7 | LDAP configuration | `bonsai.toml` `[ldap]` (`LdapConfig`) | server URL, bind DN, `bind_password_env` **name**, role map | Safe metadata (env var *name*, never the secret; secret resolved from env/vault at runtime) | Config file, not graph |
| 8 | LDAP bind password | env var named by `bind_password_env` / vault alias `ldap-bind` | cleartext secret | Secret — already outside graph (env/vault) | Vault-protected |
| 9 | Collector/device credentials | `CredentialVault` (`runtime/vault/vault.age`) | age-encrypted username/password pairs | Secret — already protected | Vault-protected |
| 10 | `app_config.require_auth` | `app_config` | `"1"/"0"` flag | Safe operational metadata | Graph OK — **ignored for auth decisions since F1** (deployment env policy only) |
| 10a | Internal/routing service tokens | `app_config`/`runtime_config` names `routing_service_token`, `internal_service_token`, `service_token`, `admin_password`, `bind_password` | cleartext bearer tokens/passwords | **Secret** | **Moved to `service_tokens` (schema v4, age-sealed) by `migrate_protected_config`; graph rows tombstoned; `/api/config-items` POST returns 410; resolver reads AuthStore → env fallback** |
| 10b | Stray `jwt_secret`-named ConfigItem | any class, name `jwt_secret` | ambiguous JWT material | Secret-adjacent | **Quarantined into `auth_quarantine` (sealed) — canonical keys live in `jwt_keys`** |
| 11 | Env admin bootstrap | `BONSAI_ADMIN_USER` / `BONSAI_ADMIN_PASS` env + vault aliases `bonsai-admin`, `bonsai-admin-pass` | cleartext password compared directly | Secret — env/vault only, never persisted to graph | OK |

**Conclusion:** records 1–5 must leave the graph. Record 6 stays (it is a
reference, not material). Records 7–11 are already outside the graph.

## 2. Protected storage design

`src/auth_store.rs` — `AuthStore`, a dedicated SQLite database
(`<graph-path-parent>/bonsai_auth.db`, mode `0600`) owned by the service
identity. Reuses `rusqlite` (already a dependency) for the verifier/metadata
tables and the existing `BONSAI_VAULT_PASSPHRASE` + `age` encryption used by
`CredentialVault` for the one recoverable secret (the JWT signing key).
No second credential system is introduced: this is the auth authority;
the vault remains the device-credential authority.

### Schema (v1)

- `users(username PK, password_hash, role, enabled, last_login_ns,
  created_at_ns, updated_at_ns)` — Argon2id PHC verifiers (legacy SHA-256
  hex accepted on first login and rehashed, preserving current semantics).
- `api_keys(id PK, alias, key_hash, scope, expires_at_ns, last_used_at_ns,
  enabled, created_at_ns, updated_at_ns)` — SHA-256 verifiers for
  high-entropy keys; raw keys are never persisted.
- `jwt_keys(kid PK, is_active, algorithm, secret_enc BLOB, source,
  created_at_ns, retired_at_ns, verify_until_ns)` — `secret_enc` is an
  age ciphertext of the raw secret. `source` ∈ `generated | imported`.
  A partial unique index guarantees at most one `is_active = 1` row.
- `jwt_revoked(jti PK, expires_at_ns, revoked_at_ns)` — logout denylist.
- `auth_config(key PK, value)` — durable migration marker and auth config.
- `auth_audit(ts_ns, actor, action, subject, outcome)` — authentication
  audit metadata; **never** contains secret material.
- `service_tokens(name PK, token_enc, created_at_ns, updated_at_ns)` —
  age-sealed internal service / routing bearer tokens (schema v4).
  Populated by `migrate_protected_config` from graph ConfigItems and by
  the offline `bonsai auth set-service-token` CLI; raw values are never
  listed or audited.

### Access rules

- Typed API only (`AuthStore::{list_users, upsert_user, …}`); no generic
  record access. Callers cannot reach verifier rows through graph APIs.
- All fallible operations return `Result`; storage errors propagate —
  they never degrade to "empty = allowed".
- File created `0600`; parent directory must already exist (runtime dir).
  WAL is not enabled (single-writer process; default journal is atomic).

## 3. JWT key lifecycle

- **Generation:** 32 bytes from `rand::rngs::OsRng` (CSPRNG), hex-encoded,
  age-encrypted under the resolved secret key, inserted as the single
  `is_active = 1` row. Initialization is serialized by the store mutex plus
  the SQLite unique index — concurrent first starts cannot mint two
  different live keys; the loser re-reads the winner's row.
- **At-rest key:** `BONSAI_VAULT_PASSPHRASE` when set; otherwise an
  auto-generated `<auth.db>.key` file (mode 0600) beside the database.
  The keyfile protects the secret when `bonsai_auth.db` is copied without
  it (partial backups); against an attacker who already owns the runtime
  directory it is equivalent to the 0600 file permission. Set
  `BONSAI_VAULT_PASSPHRASE` for external key ownership.
- **Restart stability:** the active row is re-read and decrypted on every
  process start; the key survives restarts.
- **Failure modes (all fail closed):**
  - `BONSAI_JWT_SECRET` set but < 32 bytes → explicit configuration error
    (previously warn-and-ignore — fixed).
  - Stored key present but the at-rest key is wrong/unavailable → decrypt
    error; no token is minted or accepted.
  - Protected DB unreadable/corrupt → error, never an empty-key fallback.
- **Rotation:** `rotate_jwt_key()` inserts a new active key, retires the
  old one with `verify_until_ns = now + max_token_ttl + margin`. Tokens
  signed by a retired key verify until `verify_until_ns`, bounding the
  overlap; after that only the new key verifies. No mass logout required.
- **Revocation:** logout inserts `(jti, exp)` into `jwt_revoked`; expired
  entries are pruned lazily. A failed revocation lookup returns an error →
  request denied (previously `unwrap_or_default()` silently treated the
  token as unrevoked — fixed).

## 4. Migration contract (WP3)

`AuthStore::migrate_from_graph(&GraphStore)` — idempotent, resumable,
explicitly staged, guarded by `auth_config.migration_state`:

1. `detect` — scan graph `ConfigItem` classes `user`, `api_key`,
   `jwt_secret`, `jwt_revoked`, `session`.
2. `validate` — parse each record; malformed records are counted and
   reported, never silently imported or invented.
3. `import` — `INSERT OR IGNORE` into auth tables (PK-conflict = already
   migrated; content conflict = error, not overwrite).
4. `verify` — row counts re-validated; a smaller auth-store count than
   the imported count means import loss ⇒ bail before switching.
5. `quarantine` — every unresolved credential-bearing record (malformed
   users/API keys, non-canonical or disabled `jwt_secret` rows, bad
   `jwt_revoked` ids) is sealed into `auth_quarantine`: the original
   `content_json` is age-encrypted under the at-rest key into
   `content_enc`, `content_sha256` binds the original bytes, and
   `content_json` retains only a non-secret descriptor (AUTH-C4 —
   recoverable secrets are never stored as unrestricted plaintext).
   A same-identity row with different content is a hard conflict, never
   `INSERT OR IGNORE`-accepted; every quarantined record is verified
   present and content-bound BEFORE any graph row is scrubbed.
   Verified-equivalent records live authoritatively in `AuthStore`.
6. `scrub` — legacy graph rows for `user`, `api_key`, `jwt_secret`,
   `jwt_revoked` **and** `session` are disabled AND their `content_json`
   is replaced with a *validated* migration tombstone
   (`{"migrated_to":"auth_store","migration":"bn022-auth-migration-v1",
   "record_class":…, "item_id":…, "disposition":…}`). Tombstones are
   recognized only by full JSON validation — an arbitrary substring or
   lookalike record is treated as an original record (fail-safe).
   Credential material is scrubbed from the live graph so it is not
   queryable through any graph API, including admin queries. Rows are
   never deleted; pre-migration backups/WAL remnants are unaffected
   (see §5 historical exposure).
7. `switch` — the durable per-record migration manifest
   (`auth_config.migration_report`: counts + every record's
   class/id/disposition — imported, quarantined, retired or
   env-skipped) and `migration_state = 'complete'` are committed in ONE
   transaction, so an interruption can never leave a completion marker
   without its manifest. A crash mid-scrub re-runs the whole idempotent
   migration: validated tombstones contribute their recorded
   dispositions, remaining rows are re-validated and finished. From this
   point auth reads **only** `AuthStore`. There is no fallback to graph
   records.

The marker makes re-runs no-ops; interruption at any stage resumes from
the marker. Real credential values never appear in migration evidence —
only counts, classes and record IDs.

## 5. Startup dependencies, backup, HA, recovery

- **Startup:** `GraphStore::open` also opens `bonsai_auth.db` beside the
  graph DB (failure to open it fails the graph open — auth never runs
  against missing protected storage). JWT-secret decryption needs either
  `BONSAI_VAULT_PASSPHRASE` or the auto-generated `bonsai_auth.db.key`
  keyfile; `BONSAI_JWT_SECRET` overrides the stored key entirely.
- **Backup (AUTH-C1):** `AuthStore::backup_to(dest)` produces a
  consistent `VACUUM INTO` snapshot staged in an owner-only file
  (0600 from creation), installs it atomically without overwriting an
  existing destination, and writes a `<dest>.manifest.json` computed
  from the snapshot itself (users/keys/JWT keys/revocations/quarantine
  counts + migration state — never secret material). The at-rest key is
  NOT included: recovery needs `BONSAI_VAULT_PASSPHRASE` or the
  `bonsai_auth.db.key` keyfile backed up separately. `/api/db/backup`
  exports the graph DB only and is NOT complete disaster recovery.
- **Restore (offline/privileged — WP3):** stop the service, place the
  snapshot as `bonsai_auth.db` in the runtime data directory together
  with the matching keyfile (or set `BONSAI_VAULT_PASSPHRASE`), then
  start. `AuthStore::open` fails closed on wrong/missing key material.
  **Rollback caveat:** an older backup cannot know revocations or
  account changes made after it was taken — restoring it revives that
  older state. After any restore from a backup older than the incident
  window, run an emergency JWT rotation
  (`rotate_jwt_key_emergency`) to invalidate all pre-restore tokens,
  and re-check `jwt_revoked`/`users` state before declaring recovery.
  Quarantined records are never reactivated automatically; operator
  recovery uses `AuthStore::decrypt_quarantine_content(id)` with the
  same at-rest key, per the runbook in
  `docs/auth_backup_restore_runbook.md`.
- **HA:** single-writer SQLite; cross-process HA is not claimed. Revocation
  and key state are per-node until a replication design exists.
- **Recovery:** losing `bonsai_auth.db` while keeping the vault passphrase
  allows re-migration only if legacy graph rows still exist; otherwise
  users/keys must be re-enrolled (documented operator procedure). JWT
  signing key loss invalidates all outstanding tokens (acceptable: users
  re-login).
- **Historical exposure:** pre-migration graph DBs, WAL remnants, backups
  and exports may still contain the old plaintext JWT secret and verifiers.
  Operators must rotate the JWT signing key (`rotate_jwt_key`) and
  re-issue credentials on upgraded deployments; this milestone does not
  perform production rotation.

## 6. Revocation and account-authority guarantee (actual, not aspirational)

- **Logout:** the presented JWT's `jti` is inserted into `jwt_revoked`
  with the token's `exp`; verification denies it immediately on this
  node. Revocation lookup errors deny the request (fail closed).
- **Restart:** `jwt_revoked` is durable in `bonsai_auth.db` — revocations
  survive restarts.
- **Disabled/deleted users and role changes (AUTH-C2):** every JWT
  carries a `src` claim (`local` | `ldap` | `env`; absent ⇒ `local`).
  The middleware re-reads the `users` row on EVERY request:
  - `env` bootstrap identities keep their token role (explicit operator
    configuration, independent of the users table).
  - `local` and `ldap` tokens: a local `users` row is authoritative for
    BOTH sources (the collision policy for shared usernames) — enabled
    ⇒ the LIVE role is enforced (Admin→Viewer reductions take effect
    immediately), disabled ⇒ deny. With NO local row, `local` tokens are
    denied (missing/deleted account) while `ldap` tokens keep their
    bind-time role (pure external identity).
  - Unknown `src` values fail closed.
  - Login bookkeeping uses field-scoped conditional writes
    (`AuthStore::record_login`): `last_login` updates only while the
    account is enabled, and a legacy-verifier rehash applies only via
    compare-and-swap on the exact hash verified — it cannot undo a
    concurrent disable, delete, role reduction or password reset.
  - A disabled local account cannot fall through to LDAP at login.
- **JWT key cache expiry (AUTH-C3):** the process-local decrypted-secret
  cache is valid only until the earliest retired key's
  `verify_until_ns`; expiry forces a refresh without rotation or
  restart. Retired keys are bounded (at most 8 retained). Emergency
  rotation clears the cache and zeroes every retired verification
  window. The cache is per-process; cross-process consistency is NOT
  claimed (single-writer SQLite authority).
- **API keys:** disable/rotate take effect on the next request (the key
  hash lookup is per-request). Rotation is a single transaction — the
  old key can never remain active after a successful rotation.
- **HA:** revocation and key state are per-node; no cross-process
  propagation is claimed.

## 7. Residual limitations (explicit)

- `cert_config` graph records retained (references only).
- LDAP group search over `ldaps://` still falls back to `default_role`
  (pre-existing; unchanged).
- Process-wide env vars (`BONSAI_JWT_SECRET`, `BONSAI_VAULT_PASSPHRASE`)
  remain process-scoped; tests must serialize via an env lock.
- No distributed revocation/HA claim.

## 6. F1–F6 closure notes (WP2, 14 November)

- **F1 auth policy:** `auth_middleware` no longer reads `app_config.require_auth`
  (or any graph ConfigItem). `GraphStore::auth_required()` resolves a
  per-store test override, else `auth_store::auth_required_by_deployment()`:
  auth required by default; `BONSAI_REQUIRE_AUTH=1|true` forces it on;
  `BONSAI_REQUIRE_AUTH=0|false` is honored ONLY with
  `BONSAI_ALLOW_INSECURE_NO_AUTH=1|true` or `BONSAI_ALLOW_INSECURE_DEV=1|true`;
  unset → required unless an insecure flag is set. A stale
  `require_auth=false` graph record cannot weaken enforcement.
- **F2:** `/api/auth/apikeys*` and `/api/auth/ldap/*` are Administrator-only
  in `required_role`; scope strings are validated at creation.
- **F3:** `bonsai auth verify-backup` uses `AuthStore::verify_backup` —
  manifest v2 (`bonsai-auth-backup-v2`) with snapshot SHA-256+size, read-only
  `integrity_check`/`foreign_key_check`, count/migration-state comparison,
  and decrypt-verification of every `jwt_keys.secret_enc` without generating
  material. v1 manifests are rejected explicitly. Digest proves integrity,
  not authenticity against an attacker rewriting both artifacts.
- **F4:** backup stages with fsync, installs via hard-link, writes+fsyncs the
  manifest, then fsyncs the parent directory. Incomplete pairs fail verify.
- **F5:** `AuthStore::open` rejects symlink/non-regular/foreign-owned DB files
  and world-writable non-sticky parents; opens with `SQLITE_OPEN_NOFOLLOW`.
  Trusted-directory assumption: the parent dir must be operator-controlled;
  intermediate symlinked ancestors are out of scope.
- **F6:** login rate limiting split into `login_is_locked` /
  `record_login_failure` / `record_login_success` — one count per failed
  auth, poisoned mutex recovers the guard (never fails open), tracker is
  bounded (8192 usernames). Eviction is **stale-only**: entries with a
  live window or active lockout are never evicted, so username flooding
  cannot flush a victim's lockout or pre-lockout failure count. Residual:
  when all 8192 entries are live, new usernames are untracked (documented
  trade-off; rate limiting is process-local — see §6 HA note).
