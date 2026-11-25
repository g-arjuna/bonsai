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

## BN-024-03/03A implementation slice (2026-11-25)

First executable slice of the seam, built on the contract above:

- `platform_registry` gained canonical `sonic` and `nvidia.cumulus`
  platform profiles (aliases resolve; standalone `frr` unchanged).
  No CLI adapter or parser-backend id is claimed for either —
  `parser_backend_id: None` keeps them out of parser-sidecar vendor
  resolution until a verified mapping exists.
- `nos_features` profiles are now the versioned feature contract:
  `AcquisitionContract`/`AcquisitionKind` (typed acquisition *planes* —
  `cli_observation`, `structured_state_db`, `api_rest`, `gnmi`,
  `kernel_netlink`, `unknown`; `contracts` holds logical identifiers,
  never runnable commands), `NormalizerRef` (id + normalizer/mapping
  revision resolved by the offline registry, never a code path),
  `PromotionState` (`shadow` default; `active` reserved for qualified
  profiles — reversible profile-data change), `ResourceBudget`
  (per-profile input/entity/output bounds) and `device_roles`.
- `builtin_profiles()` / `builtin_registry()` ship declarative SONiC,
  Cumulus and standalone-FRR profiles. `version_match: "*"` records
  the upstream `version: all` selector — it is NOT tested-release
  support: unknown releases select only as `Candidate`.
- `normalizer_registry` is the offline dispatch seam: a `Selected`
  profile + `Enabled` feature + declared `NormalizerRef` + matching
  observation NOS invokes the bound normalizer and returns
  `NormalizerRun { output, shadow_only }`. Candidate/disabled/
  unqualified/mismatched dispatch fails closed with explicit
  `DispatchError`. No collection, I/O, parser-chain wiring or graph
  access — output stays `device_asserted`, `authority: none`.

### Source-informed decisions (D6)

- SuzieQ pinned @ `75198b5`: `suzieq/config/interfaces.yml` declares
  the same `vtysh show int vrf all` + `ip -d address show` pair for
  `cumulus`/`sonic` under `version: all`; `service.py` selects the
  first matching defn (order-dependent, `all` matches unknown
  versions). Bonsai's `ReleaseConstraint` is equality-only and
  `Candidate`-gates unknown releases — deliberate deviation recorded
  in `source-crosswalk/README.md` finding 6.
- SONiC namespace model: upstream collapses host/ASIC namespaces
  (crosswalk finding 2); the profile keeps `verified_namespaces`
  empty until the structured STATE_DB slice exists, and the FRR CLI
  observation path is host-level only.
- Cumulus is modeled as `nvidia.cumulus` (vendor.os) with NVUE/FRR/
  kernel components — NOT the standalone `frr` platform; its
  `interfaces.frr_cli` feature binds the same normalizer under a
  distinct profile id.
- M6 `yang_applicability`/`schema_binding` infrastructure is
  untouched: profiles describe *feature applicability for native
  normalization*, a different axis from YANG schema binding. No
  competing registry was created — selection reuses
  `platform_registry` canonical ids via `canonical_platform_id`.

### Deferred (explicit)

- Parser-chain/capture wiring behind a flag (needs common capture
  envelope — BN-024-05+).
- SONiC STATE_DB/COUNTERS_DB/APPL_DB/CONFIG_DB, Cumulus NVUE and
  kernel structured-state acquisition — declared `disabled`/
  `support: unknown`, never implied.
- Namespace-scoped dispatch (`feature_status_in_namespace`) is
  contract-ready but unused until structured sources land.
- Config-file profile loading, hot-disable/rollback, per-plugin
  sandboxing — profile data is code-registered for now.
