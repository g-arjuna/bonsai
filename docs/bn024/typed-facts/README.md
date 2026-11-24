# BN-024-02 — Canonical Typed-Fact ABI (v1)

Executable contract: `src/typed_facts.rs` (`bonsai::typed_facts`).
Machine-readable schema: `v1.schema.json`. Representative serialized
fixture: `v1.fixture.json` (regenerate via
`cargo test -p bonsai --lib typed_facts -- --ignored emit_fixture`).

## Scope and non-goals

This ABI defines how typed facts are *represented, validated, and
lifecycle-managed* before any authoritative projection. It deliberately
contains **no graph writer**: nothing in `typed_facts` references
`graph::`, `ladybug`, or any mutation path, and no consumer is wired to
treat `TypedFact` as authoritative store input yet. BN-024-03 may build an
extension seam on this stable schema.

## Schema versioning

- `schema_version` is a single `u32`; writers emit `SCHEMA_VERSION = 1`.
- `TypedFact::from_slice` rejects `schema_version >
  MAX_SUPPORTED_SCHEMA_VERSION` with `UnsupportedSchemaVersion` — newer
  payloads must pass through an explicit upgrade, never be mis-parsed.
- Evolution rule: additive optional fields may keep v1; any breaking
  change requires a new version plus upgrade path.

## Explicit states (never fabricated success)

- `payload.fields: None` requires `payload.state` ∈ {`empty`,
  `invalid_input`, `unsupported`, `unstructured_fallback`,
  `unknown_quality`}; `fields` and `state` are mutually exclusive.
- `quality.completeness` ∈ {`complete`, `partial`, `unknown`} — unknown is
  representable, not an error.
- `valid_from`/`valid_until` absent means *unknown validity*, not infinite.

## Trust vs quality vs authority

- `trust` (`unverified` → `device_asserted` → `corroborated` →
  `authoritative`) is an authority-*eligibility claim* carried by the
  fact, orthogonal to parse quality. `TrustClass::authority_eligible()`
  describes the claim class only — it is NOT an authorization check.
- **Authority requires a grant.** `TypedFact::authority_eligible()`
  returns true only when the trust claim is `corroborated`/`authoritative`
  AND `authority` holds an attested `AuthorityGrant`. Grants are minted
  solely by `AuthorityGrant::from_resolution()` from a trusted Core
  `IdentityResolution` (`IdentityStatus::Resolved`); the attestation flag
  is never serialized, so a fact restored from JSON/journal/API data is
  always unattested and must have authority re-derived by the consumer —
  the same boundary as `VerifiedIngressPrincipal`.
- A serialized `trust: "authoritative"` (with or without an `authority`
  block) is representable evidence but never self-authorizing; covered by
  `forged_trust_claim_is_not_authority` and
  `deserialized_grant_loses_attestation`.
- Validation rejects `authoritative` trust on `unstructured_fallback` or
  `unknown_quality` payloads.
- Structured parser output alone never confers `authoritative` — the
  enricher caps confidence at "medium" (`parser_chain_enricher.rs`).

## Lifecycle

`active` → `withdrawn` (requires `withdrawn_reason`) or `superseded`
(requires `superseded_by` fact id); `stale` is a non-terminal degraded
state. Withdrawn/superseded are terminal — `withdraw()`/`supersede()`
reject further transitions. Withdrawal annotates the same `fact_id`
(identity is content-derived, not lifecycle-derived).

## Identity & evidence

- `fact_id` = SHA-256 over canonical JSON (recursively key-sorted) of
  (schema_version, device_id, namespace, vrf, fact_kind, produced_by,
  payload_sha256, capture_id, collector_id) via `seal()`. Binding
  schema_version and parser identity means parser/schema evolution cannot
  silently collide with or reinterpret historical facts.
- `source` mirrors `ObservationSource` axes (collector_id, session_epoch,
  sequence) so BMP and CLI evidence join on the same identity.
- `evidence[]` carries SHA-256 digests + optional locators for raw
  captures, sidecar responses, fixtures.

## Temporal semantics

`observed_at` (RFC-3339) is capture/observation time; `ingested_at`
records when Bonsai recorded the fact (`None` = unknown);
`valid_from`/`valid_until` bound the fact's claimed validity. Device
event time and replay markers are fact-kind payload concerns or source
epoch/sequence — the ABI distinguishes capture time, ingestion time,
validity window, source epoch/cursor, completeness and lifecycle without
collapsing them. Freshness/staleness policy is a consumer concern — the
ABI only requires internal consistency.

## Tests

20 unit/property-style tests in `src/typed_facts.rs` cover: seal stability,
payload-state exclusivity, lifecycle terminality, trust boundary,
schema-version gating, roundtrip, committed-fixture validity, authority
grant derivation/attestation loss, forged-claim rejection, and
schema/producer-bound fact identity. `tests/test_bn024_typed_facts.py`
gates JSON-schema ↔ Rust-serialization drift (fixture validation, enum/
required-field rejection, lifecycle conditionals, forged-claim
representability).
