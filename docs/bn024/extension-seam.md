# BN-024-03/03A — bounded extension seam (first slices)

Status: contract-only slices landed on
`fix/bn024-parser-quality-and-sidecar-contract`. No collection, parsing,
graph mutation, or authority projection is wired.

## What exists

- `src/nos_features.rs` — `NosFeatureProfile` / `FeatureFlag` /
  `ProfileRegistry` / `ProfileQuery` / `Selection` /
  `FeatureStatus`, extended for BN-024-03A.
- Selection precedence (deterministic, first-registration wins on ties):
  exact platform+version → exact platform+any → family+version →
  family+any. Unknown platform → `UnsupportedPlatform`; known platform
  with uncovered version → `UnsupportedVersion`; release/hardware
  constraint rejection → `OutOfScope`; absent feature →
  `FeatureStatus::Absent` (never implicitly enabled); disabled →
  `Disabled`.
- `verified_namespaces` scopes ASIC/namespace coverage; empty means
  *unspecified* — multi-namespace devices must not be assumed covered.
- SONiC dual-ASIC fixture: `tests/fixtures/bn024/sonic-namespaces.fixture.json`
  (synthetic; `host` namespace intentionally unverified).
- Cumulus Linux metadata fixture:
  `tests/fixtures/bn024/cumulus-metadata.fixture.json` (synthetic;
  release/NVUE/FRR/kernel/ASIC + applied-vs-startup config planes).
- Dummy future-NOS profile registers and selects without any core-code
  change (unit test `dummy_future_nos_registers_without_core_changes`).

## BN-024-03A contract additions

- `SoftwareRelease` — opaque `raw` vendor version plus optional
  `train`/`build` and `components` (daemon versions such as `frr`,
  `nvue`, `kernel`). No ordering semantics.
- `ReleaseConstraint` — **equality-only** matching on
  raw/train/build/components. Every specified field must equal the
  device's reported value; unknown or partially reported releases fail
  closed. No lexical/semver comparison — NOS release formats differ.
- `HardwareScope` — platform_sku/asic/namespace + vendor `attributes`;
  used both as device description (query) and constraint (profile).
- `FeatureFlag` gains `requires` (prerequisites), `conflicts`,
  `support` (`Supported`/`Unsupported`/`Unknown`, default `Unknown`)
  and `evidence` (`ApplicabilityEvidence`: kind/source/revision/scope/
  claim — advisory only, never grants authority per BN-028-13).
- `ProfileRegistry::validate_feature_set` checks a requested feature
  *set* as a composition: declared+enabled, prerequisites present, no
  conflicts — returning all violations.
- `ProfileQuery` gains `release`/`hardware`; the legacy `version` string
  becomes the release `raw` when `release` is absent.
- Platform queries are case-normalized; selection still describes feature
  applicability only — canonical device identity remains
  `platform_registry`'s responsibility (no second platform authority).

## SONiC vs Cumulus distinction (contract-level)

- SONiC: distribution/release/HWSKU + `asic-N`/`host` namespaces +
  per-namespace CONFIG_DB/APPL_DB/STATE_DB/COUNTERS_DB + native gNMI/FRR/
  Linux observation planes.
- Cumulus: NOS release + NVUE capability/version + FRR version +
  kernel/networking source + ASIC identity + applied/startup
  configuration planes — modeled via `SoftwareRelease.components`
  (`frr`, `nvue`) and `config_planes` in the fixture, **not** as a
  standalone FRR profile.

## Design decisions recorded

- Reuses `platform_registry` canonical platform identity — no second
  platform namespace.
- Profiles are declarative data (serde JSON/TOML-friendly); selection is
  pure. No registry side-effects, no hot-reload yet.
- `verification` field is explicit (`untested`/`fixture_only`/...) —
  selection never upgrades verification status.
- `version_match` (lexical) is retained for existing profiles;
  `release_constraint` is the preferred NOS-aware path.
- Transport, role, license and credential scoping remain future inputs
  (recorded as open).

## Outstanding architecture decisions (not invented defaults)

- Whether profiles live in config, a compiled bundle, or both.
- Interaction with `ParserChainConfig.priorities` (per-feature parser
  binding vs profile `parsers` allowlist) — needs BN-024-05 DSL work.
- Hot-disable/rollback semantics and per-plugin budgets.
- Whether `verified_namespaces` should bind to typed-fact `vrf`/namespace
  fields in BN-024-02 envelopes.
- Feature `support`/`evidence` currently declarative; wiring to BN-028-13
  retrieved claims needs the document-ingestion slice (advisory only).

## Next bounded implementation (exact)

Wire `ProfileRegistry::select` into the CLI-enricher vendor resolution
path *behind a flag* (default off): given a `TargetConfig`, select the
profile and expose `feature_status` in capture diagnostics — still no
graph write. Requires: config schema for `[[nos_profiles]]`, profile
load/validate tests, and a decision on precedence vs existing
`platform_registry::resolve_target_platform`.
