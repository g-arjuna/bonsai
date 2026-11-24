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
  `authoritative`) is authority *eligibility*, orthogonal to parse quality.
  `TrustClass::authority_eligible()` returns true only for `corroborated`
  and `authoritative`.
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

- `fact_id` = SHA-256 over canonical JSON of
  (device_id, namespace, vrf, fact_kind, payload_sha256, capture_id,
  collector_id) via `seal()`.
- `source` mirrors `ObservationSource` axes (collector_id, session_epoch,
  sequence) so BMP and CLI evidence join on the same identity.
- `evidence[]` carries SHA-256 digests + optional locators for raw
  captures, sidecar responses, fixtures.

## Temporal semantics

`observed_at` (RFC-3339) is when the capture happened; `valid_from`/
`valid_until` bound the fact's claimed validity. Freshness/staleness
policy is a consumer concern — the ABI only requires internal consistency.

## Tests

15 unit/property-style tests in `src/typed_facts.rs` cover: seal stability,
payload-state exclusivity, lifecycle terminality, trust boundary,
schema-version gating, roundtrip, committed-fixture validity.
