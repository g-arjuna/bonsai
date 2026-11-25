# BN-024-03/03A — bounded extension seam (first slices)

Status: the first executable offline seam landed in PR #87 correction work.
Collection, authoritative graph mutation and promotion remain unwired. Native
normalization output is experimental or shadow-only.

## What exists

- `src/nos_features.rs` — `NosFeatureProfile` / `FeatureFlag` /
  `ProfileRegistry` / `ProfileQuery` / `Selection` /
  `FeatureStatus`, extended for BN-024-03A.
- Selection precedence (deterministic): exact platform+version → exact
  platform+any → family+version → family+any. Equally applicable
  overlapping profiles are `Ambiguous`, never first-registration-wins.
  A failed more-specific release/hardware constraint blocks lower-precedence
  wildcard fallback conservatively (`OutOfScope`). Unknown platform →
  `UnsupportedPlatform`; known platform with uncovered version →
  `UnsupportedVersion`; absent feature → `FeatureStatus::Absent` (never
  implicitly enabled); disabled → `Disabled`.
- `verified_namespaces` scopes ASIC/namespace coverage; empty means
  *unspecified and not universally applicable*. Executable dispatch enforces
  exact feature/namespace applicability. `None` or an unknown namespace is
  rejected for qualified dispatch.
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
- Platform queries are normalized through `platform_registry` canonical ids
  and aliases (trim/case normalization plus canonical alias mapping). Unknown
  future-NOS platform names remain explicit registration namespaces. Selection
  still describes feature applicability only — canonical device identity
  remains `platform_registry`'s responsibility (no second platform authority).

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
- `normalizer_registry` is the offline dispatch seam. Executable dispatch
  uses an immutable `SelectionReceipt` binding the selected profile,
  canonical platform, effective release representation, hardware axes and
  profile fingerprint. Dispatch verifies feature composition, exact
  normalizer/mapping revisions, acquisition contract shape, namespace
  applicability, resource budgets and binding between receipt metadata and
  the actual observation. A release-qualified `Selected` profile cannot
  execute when the observation release is absent or differs.
- Built-in wildcard SONiC/Cumulus profiles are recognized implementation
  profiles, not release-qualified runtime profiles. They cannot execute
  qualified dispatch merely because a nonempty version string exists. The
  historical Cumulus fixture remains available only through explicit
  `dispatch_experimental`; such output is machine-labelled
  `ExperimentalUnqualified` and shadow-only.
- No collection, I/O, parser-chain wiring or graph access — output stays
  `device_asserted`, `authority: none`, `shadow_only: true` even when
  profile data declares `PromotionState::Active`.

### Source-informed decisions (D6)

- SuzieQ pinned @ `75198b5`: `suzieq/config/interfaces.yml` declares
  the same `vtysh show int vrf all` + `ip -d address show` pair for
  `cumulus`/`sonic` (`sonic: copy: cumulus`) under `version: all`;
  `suzieq/poller/worker/services/service.py::_get_def_for_version`
  returns the first satisfying defn (`all` matches unknown versions,
  order-dependent). Bonsai's `ReleaseConstraint` is equality-only and
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

### BN-024-05 structured-source slice (2026-11-25, in progress)

- Genuine standalone FRR `show interface json` evidence was captured
  read-only from authorized lab container `clab-bonsai-bn003-vrf-frr2`
  running FRRouting `10.6.1_git`; provenance is in
  `tests/fixtures/bn024/interface-frr-json/PROVENANCE.md`. It is not
  SONiC or NVIDIA Cumulus compatibility evidence.
- `src/frr_interface_json.rs` implements exact typed decoding of the
  FRR object-map format and reuses the shared `interface.state` typed
  payload, evidence hashing, resource limits, diagnostics and canonical
  fact validation. Missing values remain explicit unknowns; zero speed
  is not fabricated as a bandwidth value.
- FRR 10.6.1 source inspection (`zebra/interface.c`) establishes that
  `index`, `mtu`, `speed`, `type`, `hardwareAddress`, `ipAddresses`,
  `parentInterface`/`parentIfindex` and `masterInterface`/`masterIfindex`
  are emitted by zebra. Speed is FRR's interface speed in Mbps; zero is
  used when no speed is established. Optional fields are decoded for
  schema enforcement, while genuinely unknown keys remain invalid input.
  Format strictness is deliberate for an exact-release-qualified adapter:
  FRR emits a fixed field set per build, so additive fields imply a
  different (unqualified) build and are rejected as typed `invalid_input`
  rather than silently ignored; missing optional fields within the schema
  remain forward-compatible explicit unknowns, and wrong types for known
  fields are format errors. Duplicate identity-bearing object keys are
  rejected during decoding before map semantics could drop a record, and
  contradictory explicit/numeric relationship references are preserved
  only as unresolved evidence — never resolved into edges.
- SuzieQ pinned @ `75198b5` has no standalone-FRR or FRR
  `show interface json` interface normalizer: Cumulus/SONiC interfaces
  use the text `vtysh show int vrf all` plus `ip -d address show` pair.
  Structured FRR coverage is therefore an upstream coverage gap, and no
  SuzieQ differential claim is made for this adapter.
- Registry dispatch now binds exact input variants to normalizer kind;
  a CLI-pair observation cannot execute the structured normalizer and
  structured JSON cannot execute the CLI-pair feature.
- Operator-facing coverage is represented by the normalization report:
  observed entities, explicit unknown expected denominator
  (`expected_interface_total: null`), per-field availability, capture
  outcome, diagnostics, evidence references and shadow-only status.
  `operator-presentation-cases.json` is a synthetic presentation contract
  fixture covering success, experimental, unsupported, unknown namespace,
  partial, conflict, failed/stale and unsupported-feature states; it is
  not device evidence.

### Deferred (explicit)

- Parser-chain/capture wiring behind a flag (needs common capture
  envelope — BN-024-05+).
- SONiC STATE_DB/COUNTERS_DB/APPL_DB/CONFIG_DB, Cumulus NVUE and
  kernel structured-state acquisition — declared `disabled`/
  `support: unknown`, never implied.
- Common capture/envelope and production acquisition wiring (BN-024-05+).
- Config-file profile loading, hot-disable/rollback, per-plugin
  sandboxing — profile data is code-registered for now. Configuration-file
  loading is not the next milestone; the next milestone is genuine
  structured native intelligence.
