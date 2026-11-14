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
| 10 | `app_config.require_auth` | `app_config` | `"1"/"0"` flag | Safe operational metadata | Graph OK |
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
5. `quarantine` — legacy graph rows for `user`, `api_key`, `jwt_secret`,
   `jwt_revoked` **and** `session` are disabled AND their `content_json`
   is replaced with an audit tombstone (`{"migrated_to":"auth_store",…}`).
   Credential material is scrubbed from the live graph so it is not
   queryable through any graph API, including admin queries. Rows are
   never deleted; pre-migration backups/WAL remnants are unaffected
   (see §5 historical exposure).
6. `switch` — `migration_state = 'complete'` is written **last**, after
   quarantine succeeds. A crash mid-quarantine re-runs the whole
   idempotent migration and finishes scrubbing; no enabled credential
   row is ever left behind with the marker set. From this point auth
   reads **only** `AuthStore`. There is no fallback to graph records.
7. `verify_inert` — post-condition checked by tests: migrated graph rows
   cannot authorize (auth code contains no graph credential reads) and
   contain no credential material.

The marker makes re-runs no-ops; interruption at any stage resumes from
the marker. Real credential values never appear in migration evidence —
only counts, classes and record IDs.

## 5. Startup dependencies, backup, HA, recovery

- **Startup:** `GraphStore::open` also opens `bonsai_auth.db` beside the
  graph DB (failure to open it fails the graph open — auth never runs
  against missing protected storage). JWT-secret decryption needs either
  `BONSAI_VAULT_PASSPHRASE` or the auto-generated `bonsai_auth.db.key`
  keyfile; `BONSAI_JWT_SECRET` overrides the stored key entirely.
- **Backup:** `bonsai_auth.db` contains only verifiers + encrypted secret;
  it must be backed up **together with** its at-rest key — either the
  `BONSAI_VAULT_PASSPHRASE` (kept in the operator secret store) or the
  `bonsai_auth.db.key` keyfile. A backup of `bonsai_auth.db` alone cannot
  decrypt the JWT signing secret. `/api/db/backup` currently exports the
  graph DB only; including the auth DB in operator backup runbooks is
  required for recoverability (tracked as a residual item).
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

## 6. Revocation guarantee (actual, not aspirational)

- **Logout:** the presented JWT's `jti` is inserted into `jwt_revoked`
  with the token's `exp`; verification denies it immediately on this
  node. Revocation lookup errors deny the request (fail closed).
- **Restart:** `jwt_revoked` is durable in `bonsai_auth.db` — revocations
  survive restarts.
- **Disabled/deleted users:** disabling a user blocks *new* logins, but
  already-issued JWTs remain valid until their 24 h `exp` — there is no
  per-user token invalidation. This is an explicit limitation; a
  `not_before`/`user_epoch` claim is future work.
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
