# BN-024-03 — bounded extension seam (first slice)

Status: contract-only slice landed on
`fix/bn024-parser-quality-and-sidecar-contract`. No collection, parsing,
graph mutation, or authority projection is wired.

## What exists

- `src/nos_features.rs` — `NosFeatureProfile` / `FeatureFlag` /
  `ProfileRegistry` / `ProfileQuery` / `Selection` /
  `FeatureStatus`.
- Selection precedence (deterministic, first-registration wins on ties):
  exact platform+version → exact platform+any → family+version →
  family+any. Unknown platform → `UnsupportedPlatform`; known platform
  with uncovered version → `UnsupportedVersion`; absent feature →
  `FeatureStatus::Absent` (never implicitly enabled); disabled →
  `Disabled`.
- `verified_namespaces` scopes ASIC/namespace coverage; empty means
  *unspecified* — multi-namespace devices must not be assumed covered.
- SONiC dual-ASIC fixture: `tests/fixtures/bn024/sonic-namespaces.fixture.json`
  (synthetic; `host` namespace intentionally unverified).
- Dummy future-NOS profile registers and selects without any core-code
  change (unit test `dummy_future_nos_registers_without_core_changes`).

## Design decisions recorded

- Reuses `platform_registry` canonical platform identity — no second
  platform namespace.
- Profiles are declarative data (serde JSON/TOML-friendly); selection is
  pure. No registry side-effects, no hot-reload yet.
- `verification` field is explicit (`untested`/`fixture_only`/...) —
  selection never upgrades verification status.
- Profile precedence is *platform/version only*; transport, role, ASIC
  and credential scoping remain future inputs (recorded as open).

## Outstanding architecture decisions (not invented defaults)

- Whether profiles live in config, a compiled bundle, or both.
- Interaction with `ParserChainConfig.priorities` (per-feature parser
  binding vs profile `parsers` allowlist) — needs BN-024-05 DSL work.
- Hot-disable/rollback semantics and per-plugin budgets.
- Whether `verified_namespaces` should bind to typed-fact `vrf`/namespace
  fields in BN-024-02 envelopes.

## Next bounded implementation (exact)

Wire `ProfileRegistry::select` into the CLI-enricher vendor resolution
path *behind a flag* (default off): given a `TargetConfig`, select the
profile and expose `feature_status` in capture diagnostics — still no
graph write. Requires: config schema for `[[nos_profiles]]`, profile
load/validate tests, and a decision on precedence vs existing
`platform_registry::resolve_target_platform`.
