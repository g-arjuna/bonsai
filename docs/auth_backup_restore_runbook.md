# Protected authentication backup & restore runbook (BN-022 / WP3)

Scope: disaster recovery for `bonsai_auth.db` — users, API-key
verifiers, JWT keys (active + retired), revocations, migration markers
and sealed quarantine records. See `docs/auth_storage.md` for the
storage design.

**Graph-only backup is NOT complete disaster recovery.** `/api/db/backup`
exports the graph database only. Authentication authority lives in the
separate protected store; losing it without this procedure loses all
accounts, API keys, JWT key material, revocations and migration state.

## 1. What a valid backup consists of

Two artifacts that MUST be managed together:

| Artifact | Contents | Sensitivity |
|---|---|---|
| `bonsai_auth.db` snapshot (+ `.manifest.json`) | verifiers, encrypted JWT keys, revocations, quarantine | owner-only 0600; contains no plaintext secrets |
| At-rest key material | `bonsai_auth.db.key` file **or** `BONSAI_VAULT_PASSPHRASE` | THE decryption key — store in the operator secret store, separately from the snapshot |

`AuthStore::backup_to(dest)` produces the snapshot + manifest
atomically: it refuses to overwrite an existing destination, stages the
`VACUUM INTO` output in an owner-only file from creation, computes the
manifest from the snapshot itself, and removes partial artifacts on
failure. Callers: the offline/privileged operational path (service
stopped or store quiesced is safest; `VACUUM INTO` is online-safe but a
quiet store gives the cleanest point-in-time).

## 2. Backup procedure

All commands are executable offline operations — run them on the host
with filesystem access while the service is stopped or quiesced. There
is deliberately no HTTP endpoint for auth-database backup/restore.

1. Quiesce or stop the Bonsai service (recommended).
2. Create the protected snapshot + manifest:

   ```bash
   bonsai auth backup \
     --db /var/lib/bonsai/bonsai_auth.db \
     --dest /backup/bonsai/$(date +%Y%m%d-%H%M%S)/bonsai_auth.db
   ```

   `backup_to` refuses to overwrite an existing snapshot OR manifest,
   stages `VACUUM INTO` output in an owner-only file, installs
   atomically, and removes only artifacts it created on failure.
3. Verify the manifest exists, is owner-only, and its counts match
   expectations (`user_count`, `api_key_count`, `jwt_key_count`,
   `jwt_revoked_count`, `quarantined_record_count`, `migration_state`).
4. Verify the backup is restorable BEFORE relying on it:

   ```bash
   bonsai auth verify-backup \
     --snapshot /backup/bonsai/<ts>/bonsai_auth.db \
     --keyfile /backup/bonsai/<ts>/bonsai_auth.db.key
   ```

   This opens a scratch COPY of the snapshot, fails closed on wrong or
   missing key material, and reports counts — it never prints secrets
   and never mutates the backup artifact.
5. Back up the at-rest key SEPARATELY:
   - keyfile mode: copy `bonsai_auth.db.key` into the secret store;
   - passphrase mode: ensure `BONSAI_VAULT_PASSPHRASE` is recorded in
     the operator secret store (never in the snapshot directory).
6. Test-restore periodically (section 4) — an untested backup is not a
   backup.

## 3. Restore procedure

1. STOP the service. Do not restore over a live store.
2. Choose a STAGING recovery directory — do NOT install into the live
   data dir yet. Restored authentication remains ISOLATED until the
   recovery checks in step 5 pass:

   ```bash
   install -m 0600 /backup/bonsai/<ts>/bonsai_auth.db \
     /recovery/bonsai_auth.db
   install -m 0600 /backup/bonsai/<ts>/bonsai_auth.db.key \
     /recovery/bonsai_auth.db.key   # keyfile mode only
   ```

3. Rehearse against the staging copy before touching production:

   ```bash
   bonsai auth verify-backup \
     --snapshot /recovery/bonsai_auth.db \
     --keyfile /recovery/bonsai_auth.db.key
   ```

4. **Emergency JWT rotation BEFORE reopening normal access (required
   after restoring any backup older than the incident window):**

   ```bash
   bonsai auth rotate-emergency --db /recovery/bonsai_auth.db
   ```

   This invalidates every token minted before the rotation — including
   any credential an old backup revived.
5. Reconcile account state against the incident timeline while still
   isolated:
   - `users`: re-disable accounts revoked/disabled after the backup,
     restore correct roles (`bonsai auth` has no user-edit command —
     use the admin API or SQL against the staging DB);
   - `api_keys`: revoke keys created-then-revoked or scope-reduced
     after the backup;
   - `jwt_revoked`: confirm post-backup revocations are re-applied.
6. Only after steps 3–5 pass, stop the service, install the staging DB
   into the live data dir, and start. `AuthStore::open` fails closed on
   wrong or missing key material — an auth store that cannot decrypt
   its JWT keys refuses to serve rather than silently re-keying.
7. Migration state travels with the snapshot (`migration_state`,
   `migration_report`, sealed `auth_quarantine`). A store restored
   mid-migration resumes idempotently: tombstones are re-validated
   against record identity AND protected-store counterparts, quarantine
   rows are content-verified before any graph scrub, the scrub only
   touches records byte-identical to the verified migration set, and
   the manifest + completion marker commit atomically.

**Unsupported recovery cases (explicit):** snapshots without a
manifest, snapshots whose at-rest key material is unavailable, and
stores whose `migration_state`/`migration_report` are inconsistent —
treat all of these as incomplete backups; do not restore.

## 4. Verification checklist after restore

- [ ] Service starts; `AuthStore::open` succeeds (no decrypt errors).
- [ ] Admin login works; a known-disabled account is still denied.
- [ ] An API key from the backup authenticates with its original scope.
- [ ] A JWT revoked BEFORE the backup is still denied; tokens minted
      after restore behave per the post-restore rotation policy.
- [ ] `migration_state` reads `complete` (or the expected in-progress
      stage) and `migration_report` parses.
- [ ] `quarantined_record_count` matches the manifest; sealed contents
      recoverable via `AuthStore::decrypt_quarantine_content(id)` with
      the same at-rest key. Quarantined credentials are NEVER
      reactivated automatically — recovery is a deliberate operator act.

## 5. Failure modes (all fail closed)

| Condition | Behavior |
|---|---|
| Wrong/missing key material | `AuthStore::open` / `jwt_secret()` error; no tokens minted or accepted |
| Snapshot without manifest | Treat as incomplete — do not restore; re-run backup |
| Destination or manifest already exists | `bonsai auth backup` refuses (no silent overwrite, no deletion of foreign artifacts) |
| Partial backup failure | staging + only self-created artifacts removed automatically |
| Conflicting quarantine record during migration | migration aborts; resolve manually — never silently accepted |

## 6. What is deliberately NOT provided

- No general-purpose HTTP download endpoint for auth credentials —
  backup/restore is an offline, privileged operational action.
- No automatic reactivation of quarantined credentials.
- No cross-node replication of revocation/key state (single-writer
  authority per node; see `docs/auth_storage.md` §6).
