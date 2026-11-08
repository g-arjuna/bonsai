# BN-002 key lifecycle & assurance provenance evidence — 8 November 2026

Scope: approval-bound executor (`src/remediation/approval.rs`) signing-key
durability/lifecycle and assurance-report provenance. Base commit `0b89f00`;
implementation is uncommitted working-tree code.

## What was added

- **Durable key store** (`<journal>.keys.json`, atomic temp+rename, mode 0600):
  versioned JSON document holding per-key secret (hex), `active|revoked`
  status, creation timestamp. Loaded under the journal `flock` at
  `open_or_create` and reloaded on every journal-locked operation, so
  registrations/revocations by one executor instance are visible to all
  instances sharing the journal. Corrupt store, unsupported version, or a
  vanished store after keys existed all fail closed.
- **Lifecycle API**: `register_signing_key` (register/rotate with overlap —
  existing keys stay valid until explicitly revoked) and
  `revoke_signing_key` (durable, idempotent). `with_signing_key` /
  `with_revoked_key` / `with_signing_secret` builders now mark the store dirty
  and best-effort persist; journal-locked ops retry and surface failures as
  `JournalPersistenceError`. Re-registering a revoked `key_id` reactivates it
  explicitly.
- **Assurance provenance**: `ChangeAssuranceReport` gains optional
  `signature_algorithm`/`key_id`/`signature_hex` (serde-default, excluded from
  `canonical_bytes()`/`compute_report_hash()` — signing does not alter the
  report hash bound into approvals). `sign_hmac_sha256`/`verify_signature`
  mirror the approval API. Enforced mode now requires a valid report signature
  from a configured, non-revoked key (`MissingAssuranceSignature`,
  `RevokedSigningKey`, `UnknownKeyId`, `InvalidSignature`); simulation mode
  verifies signed reports when a keyring exists and still permits unsigned
  reports for developer workflows.

## Verified behavior (6 new tests)

- Registered keys and revocations survive process restart; enforced execution
  works on a fresh instance with no locally configured keys.
- Rotation overlap: old and new keys both verify; after revoking the old key,
  only the new key works — across restart.
- Corrupt key store fails `open_or_create`; vanished store fails subsequent
  operations with `JournalPersistenceError`.
- Enforced mode rejects unsigned reports, unknown-key signatures, and tampered
  report content; accepts properly signed reports. Revoked report-signing keys
  rejected. Simulation mode unchanged for unsigned reports.

One existing assertion was updated, not weakened: enforced replay on a journal
whose key store persists now succeeds via durable authority (previously it
assumed memory-only keys and expected `MissingSignatureAuthority`).
`MissingSignatureAuthority` on a keyless store remains covered by
`bn002_enforced_mode_requires_signing_authority_and_rejects_modeled_assurance`.

## Commands and results

- `cargo test --test a13_a20_change_assurance_and_executor --test bn001_executor_crash_recovery --test bn002_key_lifecycle_and_provenance` — 20 + 8 + 6 passed, 0 failed (`regressions.log`)
- `cargo fmt --check` — clean (`format.log`)
- `git diff --check` — clean (`diff-check.log`)

## Limits / remaining BN-002 scope

- HMAC-SHA256 symmetric keys retained per agreed scope: the verifier can also
  forge; asymmetric (Ed25519) non-repudiation deferred.
- Key custody is a local file store (0600), not vault/HSM; vault integration is
  BN-015/BN-019 scope.
- Approval and report signatures share one keyring; per-role key separation
  and the operational-boundary transport (mTLS/signed envelopes) remain for
  the BN-014 wiring scope.
- No `BONSAI-APPROVAL-V2` migration: canonical encodings are unchanged;
  new fields are additive `serde(default)` only.
