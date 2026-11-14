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

1. Quiesce or stop the Bonsai service (recommended).
2. Run the privileged backup path against the runtime auth DB:
   `AuthStore::backup_to(<backup-dir>/bonsai_auth.db)`.
3. Verify `<backup-dir>/bonsai_auth.db.manifest.json` exists, is
   owner-only, and its counts match expectations
   (`user_count`, `api_key_count`, `jwt_key_count`,
   `jwt_revoked_count`, `quarantined_record_count`,
   `migration_state`).
4. Back up the at-rest key SEPARATELY:
   - keyfile mode: copy `bonsai_auth.db.key` into the secret store;
   - passphrase mode: ensure `BONSAI_VAULT_PASSPHRASE` is recorded in
     the operator secret store (never in the snapshot directory).
5. Test-restore periodically (section 4) — an untested backup is not a
   backup.

## 3. Restore procedure

1. STOP the service. Do not restore over a live store.
2. Choose the recovery directory (the runtime data dir that contains
   `bonsai_auth.db`).
3. Install the snapshot AS `bonsai_auth.db` (plain file copy; it is a
   complete SQLite database). Install the matching key material:
   - keyfile mode: place `bonsai_auth.db.key` beside it, mode 0600; or
   - passphrase mode: export `BONSAI_VAULT_PASSPHRASE` before start.
4. Start the service. `AuthStore::open` fails closed on wrong or
   missing key material — an auth store that cannot decrypt its JWT
   keys refuses to serve rather than silently re-keying.
5. **Post-restore revocation policy (required reading):** an older
   backup cannot know revocations, disables, role changes or new
   accounts created after it was taken. Restoring revives that older
   state — credentials revoked after the backup timestamp become
   valid again. After ANY restore from a backup older than the incident
   window:
   - run an emergency JWT rotation
     (`AuthStore::rotate_jwt_key_emergency`) to invalidate every
     pre-restore token at once;
   - audit `users` (enabled flags, roles) and `jwt_revoked` against the
     incident timeline before declaring recovery;
   - re-issue or re-revoke API keys as required.
6. Migration state travels with the snapshot (`migration_state`,
   `migration_report`, sealed `auth_quarantine`). A store restored
   mid-migration resumes idempotently: validated tombstones are
   recognized, quarantine rows are content-verified before any graph
   scrub, and the manifest + completion marker commit atomically.

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
| Destination already exists | `backup_to` refuses (no silent overwrite) |
| Partial backup failure | staging + partial artifacts removed automatically |
| Conflicting quarantine record during migration | migration aborts; resolve manually — never silently accepted |

## 6. What is deliberately NOT provided

- No general-purpose HTTP download endpoint for auth credentials —
  backup/restore is an offline, privileged operational action.
- No automatic reactivation of quarantined credentials.
- No cross-node replication of revocation/key state (single-writer
  authority per node; see `docs/auth_storage.md` §6).
