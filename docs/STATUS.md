# Bonsai current state

BN-024-04 first authorized native acquisition vertical slice — **MERGED PR #89**
(merge commit `54d4cb65140ffbf202190f67b5c3b736345a5572`). This is the first end-to-end native
intelligence path, bounded to standalone FRR interface state: canonical inventory identity plus
independently enforced read/collection scopes → operator-authenticated production API →
pinned-host-key verified SSH → fixed read-only `vtysh -c 'show version json'` / `vtysh -c 'show
interface json'` → exact immutable artifact qualification
`quay.io/frrouting/frr@sha256:813497af103e972600f6161012b3bf2a2a1c5ea10d09cd1eacd74f1dc510a57b` →
`frr-iface-json-2026.11-r2` normalization with mapping `bn024-iface-map-3` → validated typed
`interface.state` facts → durable bounded shadow observation/acquisition state → authorized API and
DeviceDrawer evidence panel. Genuine authorized lab evidence on `clab-bonsai-bn003-vrf-frr2` (FRR
`10.6.1_git`) validated and retained 4/4 interface facts; negative SSH evidence covers
unknown/changed host key and authentication failure. The result remains `shadow_only`; authoritative
projection is explicitly `none`. Scope qualification: design+implementation (D/I) with normalization
(N) evidence for one exact software artifact in one default-namespace lab; no scale (S), production
(P), vendor-hardware (V), operational fleet (O), SONiC/Cumulus, multi-namespace, topology, or
authoritative projection qualification is claimed. Canonical L0–L3 gates passed once on exact
production SHA `d203e0000ee93c3439f925bb7cc99addeba640e2` (2,064.23 s; L2 1,298 Rust passes, L3 71
Rust/browser passes plus 60 root and 318 SDK Python passes; skips/ignores not counted); evidence is
in `docs/merger/evidence/2026-11-25-bn024-04-native-acquisition/`. L4 remains NOT_EXECUTED.
BN-024-04 is therefore a partial capture slice, not completion of the broader capture epic;
BN-024-08 SONiC and BN-024-08A Cumulus structured intelligence remain outstanding.

BN-024 XRd model-driven interface normalization — **MERGED PR #90** (merge commit
`4c100ba56a06dfe57a5aed65c3920eb5c894757e`; final production head
`be277f97be8500d43fa724e0352d207ea26746ff`). Genuine XRd 24.4.2 read-only gNMI
subscribe-once evidence now feeds an offline Rust OpenConfig interface normalizer with exact
revisioned registry/profile wiring. Canonical L0–L3 passed on `be277f9` after correction;
durable evidence is `docs/merger/evidence/2026-11-25-bn025-xrd-native-interfaces/canonical-l0-l3-be277f9.log`.
The retained log contains 1,409 Rust test executions passed / 0 failed / 10 ignored and 382 Python
tests passed / 1 skipped / 1 deselected when summing target executions across canonical tiers
(not unique-test counts); focused `xrd_openconfig` coverage was 32/32. L4 remains NOT_EXECUTED.
The capability is experimental offline/shadow only: production acquisition, durable shadow
lifecycle integration, structured non-zero subinterface identity, network-instance/VRF attachment,
LAG relationships, broader cross-NOS relationship coverage and operator/API integration remain
outstanding. Residual model-driven findings are explicitly assigned to the next shared increment in
`docs/bn024/xrd-residual-model-driven-findings.md`; they do not reopen PR #90.

BN-024 native network intelligence — **G0 landed 2026-11-24** on main
`e578595`/`09c2d60` (PRs #82/#83): BN-024-00 pinned SuzieQ OSS provenance
manifest + offline validator; BN-024-01 parser-chain contract repair.
**Corrective pass (merged through PR #83 and later descendants):** independent review of
the merged BN-024-01 found residual defects — fixed: structured-first
ordering no longer lets `bonsai_native` shadow Genie/TextFSM, canonical
platform ids (`cisco.iosxr`) are translated to backend vendor ids
(`cisco_iosxr`) at the sidecar boundary (regression-tested through the
production `inferred_vendor()` path), sidecar `parser`/`backend_used`/
`outcome` identity+quality are validated (missing backend = `unknown`,
never fabricated structured), consensus compares structured outputs only,
unsupported `suzieq_native` fails explicitly without HTTP, sidecar calls
are bounded (10s) with per-attempt fallthrough errors, native line-split
is capped (`MAX_NATIVE_LINES`). **Genuine packaged validation:** rebuilt
`bonsai-pyats-sidecar:bn024` (genie 26.10 / pyats 26.10 /
ntc-templates 9.4.0 / textfsm 2.1.1) exercised over HTTP — real Genie and
TextFSM parses, structured_empty vs invalid_input vs fallback all
distinguished; Rust `ParserChain`↔live-sidecar test passes. **Genuine
differential oracle** (`scripts/bn024/differential_baseline.py` v2):
pinned SuzieQ .tfsm templates executed by pinned TextFSM source vs the
live sidecar on pinned nossim fixtures — per-record typed-field diffs
with denominators, no aggregate parity claim. **BN-024-02 ABI**:
`src/typed_facts.rs` — versioned typed-fact contract (schema_version,
explicit unknown/absent/invalid payload states, completeness, trust/
authority boundary, lifecycle+withdrawal, identity/evidence digests,
schema-evolution gate) with 15 unit/property tests, JSON schema +
serialized fixture under `docs/bn024/typed-facts/`; no graph writer.
**BN-024-00 verifier hardened:** `upstream_provenance.py verify` now
checks actual git checkout revisions (not just content hashes), enforces
path containment, and uses deterministic tree hashing; all 114 manifest
entries verified against fresh pinned-revision git checkouts.
**Reconciled onto main `3347207` (owner's SONiC/Cumulus + source-port +
reference-knowledge commits preserved; BN-024-00A/03A/08A/12A and
BN-028-13 intact). Second review pass:** `typed_facts` authority is no
longer self-asserted — `AuthorityGrant` mints only from a trusted
`IdentityResolution`, attestation never survives serde, fact_id binds
schema_version+produced_by, `ingested_at` added, JSON-schema drift gate
added; `nos_features` extended for BN-024-03A (equality-only
`ReleaseConstraint`, `HardwareScope`, feature requires/conflicts/support/
evidence, `validate_feature_set`, `OutOfScope`) with a Cumulus metadata
fixture and IS-IS/SR composition tests; BN-024-00A first crosswalk
increment at `docs/bn024/source-crosswalk/`; parser-contract known
limitations documented (unbounded Python worker post-timeout, sidecar
`/learn` iosxe default, AutoAddPolicy posture, structured-empty
ambiguity). Focused tests: 30 lib + 31 pytest pass. Canonical L0–L3 subsequently
passed on descendant production validation heads; no separate gate was run on the
historical reconciled SHA itself. L4 not applicable. No
authoritative graph projection; BN-021/BN-008 gates still precede it.
Evidence: `docs/bn024/evidence/2026-11-24-bn024-01*/`,
`docs/bn024/parser-contract.md`, `docs/bn024/typed-facts/`,
`docs/bn024/extension-seam.md`, `docs/bn024/source-crosswalk/`.


BN-024 SuzieQ capability ledger — **merge-ready implementation candidate** (2026-11-26):
`docs/bn024/source-crosswalk/` now contains a source-derived inventory of the
pinned upstream revision and a machine/human capability delivery ledger. Counts:
27 upstream object families (19 polled service families + 8 derived/virtual
objects), 66 ledger capabilities (64 implemented upstream, 2 enterprise/Bonsai
improvements absent upstream). Conservative Bonsai dispositions: 1 narrow native
port, 1 existing equivalent, 1 intentional improvement, 20 partial and 43
missing after the first narrow LAG-consistency assertion slice moved
`interfaces.assert` to partial.
This is a delivery map, not a claim of feature parity or production readiness.
The cross-NOS interface qualification matrix now also records the roadmap-only
status of Nokia SR OS, MikroTik RouterOS and IP Infusion OcNOS without implying
implementation. Canonical gate evidence is reported with the implementation
slice below.

BN-024 structured interface identity and first relationship-assessment slice —
**merge-ready implementation candidate** (2026-11-26): additive optional v1
`subject.interface_identity` identity refinement implemented in
`src/typed_facts.rs` with JSON-schema/Python compatibility regressions. Legacy
facts omitting the refinement retain their serialized shape and fact identity;
when present, structured parent/subinterface/vendor-native axes participate in
`fact_id` and `AuthorityGrant` binding. Producers still emit no refinement
unless positive structured evidence exists; current genuine FRR/XRd evidence
does not contain non-zero subinterfaces, so non-zero cases are synthetic
structural regressions only. First capability-led implementation:
`src/interface_relationships.rs` assesses LAG aggregate/member consistency and
reports bridge/parent/VRF/unresolved relationships from typed interface facts,
verified against the genuine Cumulus fixture. It is offline analysis only —
no graph writer, live acquisition, API/UI exposure or cross-NOS qualification.
Focused verification through the trusted ci lane after the final corrections:
typed-facts 42 passed/2 ignored, interface relationships 17 passed,
Cumulus/SONiC CLI normalizer 29 passed/2 ignored, FRR structured JSON 20
passed, XRd OpenConfig 34 passed (142 passed total, 0 failed, 4 ignored);
typed-fact pytest schema compatibility 9 passed. Canonical L0–L3 gates passed
on frozen implementation candidate
`89382018f3c4e14886c7a55fd28fb236c62ac9cd` against base
`4c100ba56a06dfe57a5aed65c3920eb5c894757e` in 28m21.78s with maximum RSS
6,589,888 KiB: L2 1362 Rust tests passed/0 failed/10 ignored across 45 suites;
L3 71 Rust integration/browser tests passed/0 failed across 13 targets, root
Python 66 passed, SDK Python 318 passed with 1 explicit skip and 1 deselected
test. L4 remains NOT_EXECUTED because this offline slice has no live lab or
operator-surface acceptance boundary. Evidence:
`docs/bn024/evidence/2026-11-26-pr91-corrective/canonical-l0-l3-8938201.log`.

BN-024 first native intelligence slice — **PR #86**
(`feat/bn024-native-interface-state`): native Rust normalizer for the
Cumulus/SONiC FRR interface observation pair emitting typed
`interface.state` facts. Review pass on `1130ef1` (L0 failed at
rustfmt; L1–L3 not executed) produced confirmed findings A1–A7 —
fixed on `ca6d86c`: per-source capture outcomes with honest
completeness, resource-limit accounting, preserved cross-source
conflicts, LAG-vs-parent relationship classification, admin/oper
separation, per-family address validation, and pre-return fact
validation; plus applicability reporting, fixture provenance manifest
entry, typed-payload oracle comparison, hash-once perf fix, and
`AuthorityGrant::for_fact` source-axis corroboration. Independent
review of `ba2ce9c` then produced bounded findings B1–B7 — fixed on
the merge candidate: `capture_failed` added to the published ABI
schema with a Rust-emitted per-state fixture drift gate; attestation
now cryptographically binds the contents it attests (mutated
resolutions/grants/facts fail closed); input bounds and status-fact
validation enforced before hashing; completeness/temporal alignment
corrected (incl. a real MTU-65536 defect the new oracle caught);
differential oracle split into typed-payload vs raw-extraction
parity with mutation tests; upstream Apache-2.0 LICENSE vendored and
provenance manifest made compliant. Offline normalization only —
fixture-qualified, no live collector, no graph projection. Canonical
L0–L3 gate run recorded in
`docs/bn024/evidence/2026-11-25-bn024-native-iface/README.md`.
Evidence: `docs/bn024/evidence/2026-11-25-bn024-native-iface/`.
**Merged to main as `03b6ccd` (2026-11-25).**

BN-024-03/03A extension-seam slice — `feat/bn024-native-profiles`:
canonical `sonic` and `nvidia.cumulus` entries in `platform_registry`
(distinct from standalone `frr`; no guessed parser-backend ids);
`nos_features` profiles extended with typed `AcquisitionContract`/
`AcquisitionKind` (declarative planes, never runnable commands),
`NormalizerRef` (id+revision resolved by registry, never a code
path), `PromotionState` (shadow default), `ResourceBudget` and
`device_roles`; `builtin_profiles()`/`builtin_registry()` ship
declarative SONiC/Cumulus/FRR profiles where `version_match: "*"`
records upstream `version: all` as Candidate-only for unknown
releases; new `normalizer_registry` is the offline dispatch seam —
Selected+Enabled+bound feature on a qualified platform invokes the
PR #86 interface normalizer and returns `shadow_only` output, with
explicit `DispatchError` fail-closed paths and no collection, I/O,
parser-chain wiring or graph/authority access. SONiC STATE_DB/
COUNTERS_DB and Cumulus NVUE planes are declared disabled/unknown —
no unsupported capability claims. 18 focused lib tests + 11
integration tests (`tests/bn024_extension_seam.rs`) pass; canonical
gate pending on the PR head. Deferred: config-file profile loading,
hot-disable/rollback mechanics, parser-chain/capture-envelope
integration (BN-024-05+), all structured-source acquisition slices.
Evidence: `docs/bn024/extension-seam.md`.

**Correction pass (2026-11-25, commit `d897530`):** review of the
published seam confirmed and fixed applicability/dispatch defects —
wildcard `version_match` is now discovery/candidate-only evidence and
can never execute qualified dispatch; qualified dispatch requires an
exact equality release constraint plus verification evidence and
validates the actual observation's canonical platform, release,
namespace and acquisition contract against the selection receipt;
namespace-scoped applicability replaces device-level applicability at
dispatch; registry binds exact normalizer+mapping revisions;
declarative `PromotionState::Active` never relaxes shadow-only output;
profile resource budgets tighten but never relax implementation
limits; structured release metadata is the single effective version
representation with explicit legacy-conflict detection; input-contract
mismatches are reported before applicability qualification. The
historical Cumulus CLI fixture path is explicitly experimental and
machine-labelled unqualified. Lane-validated: 636 lib tests + 22/22
integration tests pass on the correction tree.

**BN-024-05 structured-source slice (same day, stacked PR):** first
genuine structured-source native normalizer — exact typed decoding of
standalone FRR `show interface json` captured read-only from
authorized lab container `clab-bonsai-bn003-vrf-frr2` (FRRouting
`10.6.1_git`, immutable image digest recorded in fixture provenance;
fixture byte-identical to the live container response). Emits the same
typed `interface.state` facts as the CLI adapter. Exact-release scoped
qualification only: not a SONiC/Cumulus claim, not other FRR versions,
not collection authorization. Strict schema policy documented:
optional-field absence is forward-compatible explicit unknowns;
additive fields/wrong types/duplicate identity keys are typed
invalid-input because qualification is exact-release. Relationships
resolve numeric references only within the same observation;
contradictory evidence stays unresolved. Registry input-shape binding:
CLI pair cannot execute the structured normalizer and vice versa.
SuzieQ @75198b5 has no standalone-FRR structured normalizer — no
differential parity claimed; expected values independently read from
the genuine capture. Offline/shadow only; no live producer yet.
Evidence: `tests/fixtures/bn024/interface-frr-json/PROVENANCE.md`,
`docs/bn024/extension-seam.md`.

BN-019 packaged upgrade slice — **executed 2026-11-24** on merged
main `18fa577` (PR #80): real packaged images `bonsai:bn019u2-cdfcac1`
(old baseline) → `bonsai:bn019u2-18fa577` (corrected). Leg A: disposable
runtime seeded via `wp28_fixture_seed` — old image ran the real
`AuthStore::ensure_migrated` pipeline (users=1 api_keys=1
jwt_revocations=1 malformed=2 quarantined=7, 12 graph tombstones), and
the new image booted on the same mounted runtime/credentials with all
migrated auth still working and SQLite `user_version` unchanged at 15.
Leg B: live SR Linux lab compose recreated on the new image — the
pre-upgrade compiler-v3 schema binding and explicit device selection
failed closed (`schema_binding_not_authoritative`,
`catalogue_integrity_failure`, `schema_version_changed`), durable rows
preserved; operator re-validation (v4 recompile `ysc-51aa768…`,
applicability refresh, approval rev 2) restored observed telemetry.
Evidence: `docs/merger/evidence/2026-11-24-bn019-m6-upgrade/`. L4
canonical lab tier remains NOT_EXECUTED; bounded slice only.

M6 final correctness review update — **PR #80**
(`fix/m6g-deviation-preconditions`, on reviewed main `cdfcac1`)
corrects defects found by the independent post-merge review; canonical
L0–L3 gate results recorded in `docs/build-times/runs.csv`, L4
live-NOS acceptance remains NOT_EXECUTED:

- M6-CR-01: RFC 7950 §7.20.3.2 deviation preconditions — `deviate
  delete` requires keyword+argument match and removes only the
  matching occurrence of repeated properties; `deviate replace`
  requires an existing property; `deviate add` respects singleton
  cardinality; deviation properties outside the RFC set are
  diagnosed; deviation bodies without a `deviate` statement are
  non-authoritative. `COMPILED_SCHEMA_VERSION` is now **v4** —
  v3-era bundles are rejected fail-closed and require explicit
  recompile + reselection (see `docs/YANG_CATALOGUE.md`).
- M6-CR-02: default NMDA resolution previously unioned config/state
  datastore axes across ALL declared datastores while module
  membership came from only the selected datastore's schema — a state
  path could be `applicable` via an unrelated datastore. Per-module
  axes are now proven by each datastore's own schema at the compiled
  revision; explicit datastore requests are unchanged/strict.
- M6-CR-03: commit-first selection ordering could leave an active
  schema-bound approval authorizing a superseded schema if the
  process was interrupted before revocation. Assignment authorization
  now fails closed (`schema_binding_schema_mismatch`) whenever a
  durable selection postdates the approval's schema.
- M6-CR-04: the schema-selection UI explains a 409 conflict,
  refreshes the durable epoch and never silently resubmits.

M6G update — **standards-correctness review and hardening of the
YANG architecture** (integration PR #79 `m6g/integration`; focused
PRs #73–#78; canonical L0–L3 PASS on the gate-tested tree, L4
live-NOS acceptance NOT_EXECUTED). Adversarial review of the M6F-F
pipeline corrected only confirmed defects:

- RFC 7950 deviation semantics: `deviate not-supported` removes the
  target node and complete subtree; `deviate delete`/`replace`/`add`
  operate at property level with diagnostics for unsupported forms —
  unsupported deviation semantics can no longer produce a falsely
  complete catalogue (catalogue stays `partial`). Applied-deviation
  provenance is tracked per compiled module.
- Compiled-schema identity now embeds `COMPILED_SCHEMA_VERSION`
  (v4 after the M6 final-review deviation-semantics correction —
  see the update above): pre-upgrade bundles fail verification
  closed and require explicit recompilation plus reselection — no
  silent reinterpretation of old semantics, and devices never lose
  authority silently (selections fail closed until re-approved).
- RFC 8342 datastore interpretation corrected: `running` is
  configuration-only, `operational` is the config+state superset,
  `candidate`/`startup`/`intended` are configuration-only. Default
  evaluation unions coverage across declared datastores; explicit
  `requested_datastore` is strict; unrecognized datastore identities
  (e.g. XRd's misspelled `ietf-datastores:candiate`) yield `unknown`
  axes and `datastore_semantics_unverified` — never positive
  applicability.
- RFC 8525 YANG Library: revision conflicts are scoped to each
  schema's module-set union (different datastores may legitimately
  pin different revisions; same-schema conflicts still rejected);
  `import-only-module` entries never conflict.
- YANG 1.1 feature `if-feature` dependencies are evaluated
  device-side with three-valued logic — unproven dependencies cap
  applicability at `unknown`, never positive. Circular dependency
  graphs (direct, multi-node, cross-module) are invalid per
  RFC 7950 §7.20.1 and diagnosed at compile time. Feature atoms
  resolve ambiguity-aware across all modules carrying a lexical
  prefix.
- Schema selection mutations are commit-first with a durable
  selection epoch (schema v15): stale `expected_selection_revision`
  requests return 409 with zero authority side effects (no approval
  revocation, no audit entry, no collector withdrawal), DELETE
  requires the precondition, and select→clear→reselect cannot reset
  the observable revision (ABA closed). Concurrent writers have a
  single winner.
- Device-scoped withdrawal verified across two devices with
  independent schema bundles on one core (`multi_device_withdrawal_
  is_device_scoped`).

M6F-F vendor acceptance evidence is unchanged — M6G changed
deterministic semantics only and was validated by unit/integration
suites plus canonical gates; no live NOS rerun was performed, so the
`lab/acceptance/m6f/MATRIX.md` statuses stand as recorded. Upgrade
note: existing compiled bundles and device selections from pre-M6G
builds fail closed on schema-identity mismatch and require explicit
recompilation/reselection — see `docs/YANG_CATALOGUE.md`.

M6F-F update — **enterprise multi-version YANG architecture and
XRd production closeout** (PRs #69 `ede70c9`…hardening `871b1a7`,
#70 deviation-semantics, #71 scope-load-boundary, closeout fix
`ecab60d`; canonical L0–L3 PASS on gate-tested SHAs).
Per-device schema authority is now durable and revision-aware:
`device_schema_selections` (schema v14) selects one verified complete
bundle per device, `resolve_effective_catalogue` fails closed on
missing/corrupt/partial selections (no silent global fallback),
scoped compiles are bounded by `module_scope`/`module_revisions`
recorded as provenance (never auto-activated), and retention
protects selected/active/active-pointer bundles while refusing to
evict on unreadable reference state. Scoped-compile defects found
live were fixed and covered by regression tests (scope applied
before module parse; revision-dedup downgraded to notice so pinned
compiles stay `complete`). Hardening: malformed binding/policy JSON
can no longer hide a referenced bundle from retention; selection
PUT/DELETE runs a post-commit re-sweep so an approval racing the
mutation cannot survive inconsistent with the committed selection;
deleted devices cannot resurrect selection authority.
XRd 24.4.2 production acceptance is now PASS end-to-end
(`docs/merger/evidence/2026-11-23-m6ff-closeout/`): a 9-module
XRd-revision-pinned scoped bundle (`ysc-9d38b26e…`, 120 paths)
selected per-device → discovery → `applicable` (content-id
`IOS-XR 7.5.1`) → schema-bound preview/activate (rev 1/2) →
observed SAMPLE telemetry with advancing timestamps → revoke → all
seven withdrawal predicates verified incl. old-session rejection.
Simultaneous SR Linux (`ysc-d7254b6e…`) + XRd selections coexist on
one core with independent lifecycle and restart-persistent
selections (`coexistence.json`). Cisco DevNet IOS XE remains
deferred (sandbox backend unavailable); Junos deferred.

M6F-D/E update — **M6F-D recovery merged; multi-vendor
interoperability extended** (PRs #61 `7192c27`, #62 `a208d43`,
#63 `8b37839`; Junos deferred). Stage 0: the three interrupted
M6F-D commits were recovered with provenance — `d803570`→`b833872`
(bounded collector lifecycle termination + stream-health
reliability), `d9266c8`→`5ff02a4` (gNMI origin provenance for
non-echoing NOS notifications, uniquely-covered authorized
subscriptions only), `9fb9a2f`→`d50afd4` (YANG Library-backed
organization mismatch advisory; capabilities-only stays strict).
The `src/subscriber.rs` overlap was resolved manually keeping both
test suites; follow-up `af6d57d` added org-mismatch boundary tests
and fixed a caps-only fixture org. Canonical gates L0–L3 PASS on
the merged SHA. Stage 6 partial: automatic acquisition now refuses
to subscribe when the Capabilities RPC fails — hint-derived fallback
can no longer manufacture model support (`capabilities_verified`
flag; explicit operator-authorized acquisition unaffected; mock
wire-level tests in `tests/gnmi_selective_acquisition.rs`).
IOS XE: remote-endpoint harness mode added (verified-TLS-only,
no containerlab, env-var credentials only); live probe of the
Catalyst 9000v DevNet sandbox recorded connection-reset on all
services — reservation backend inactive, live gNMI acceptance
NOT_EXECUTED pending fresh reservation + refreshed credentials
(`docs/merger/evidence/2026-11-22-m6f-iosxe/`). FRR: reusable
version-pinned two-router topology under `lab/acceptance/m6f-frr/`;
BMP peer/route/withdrawal evidence already exists
(bn003/bn021c/bn021f); gNMI UNSUPPORTED; fresh live run 2026-11-22 PASS (peer/route/withdraw/BMP reconnect, `docs/merger/evidence/2026-11-22-m6f-frr/`), BGP-LS UNSUPPORTED on the 10.6.2 build.
Matrix updated (`lab/acceptance/m6f/MATRIX.md`).

M6F-C update — **merged: coverage integrity, XRd/cEOS lab evidence,
SR Linux regression PASS** (PRs #55 `b40223d`, #56 `9907d7d`,
#57 `993d4f8`, #58 `f789ed8`). Stage A:
coverage matching is now schema-aware — observed gNMI module
qualifiers (`prefix:local`) must resolve through the plan's verified
compiled-catalogue identity (`src/schema_coverage.rs`); arbitrary or
unverifiable qualifiers fail closed instead of establishing
coverage, while unqualified structural matching is unchanged.
Withdrawal verification in `lab/acceptance/m6f/tools/m6f_e2e.py` now
requires authoritative evidence (binding revoked, acquisition
disabled, plan-state/terminal statuses, live collector ack with zero
subscriptions, and a stability window spanning ≥2 sample intervals)
— a frozen timestamp alone no longer passes, and a noncompliant
collector that keeps streaming fails. A new `provenance` stage
records source SHA/dirty flag, binary SHA256s, image identities, NOS
version, model-source pin, schema identity and acquisition/plan
revisions; dirty trees are marked ineligible for final-SHA
acceptance. Stage B: XRd 24.4.2 boots after the owner raised host
inotify limits; probe-level Capabilities, RFC 8525 + legacy YANG
Library and subscribe-once on OpenConfig interface counters all PASS
(gNMI 0.10.0 plaintext on TCP/9339 via the containerlab-injected
config; 1064 advertised models). XRd production-path ran end-to-end
through Core-issued discovery (snapshot 3) but plan preview correctly
REFUSED activation — applicability `conflicted`: XRd declares no
`operational` datastore (running/candidate only), capabilities
organization strings conflict with catalogue YANG org statements on
the IETF modules, and device OpenConfig revisions (2022-era) do not
match the pinned 2026 catalogue. Documented in
`docs/merger/evidence/2026-11-22-m6f-xrd-e2e/BLOCKED-FINDINGS.md`.
Stage C: pinned Arista `aristanetworks/yang@f7fc5de` EOS-4.37.0F
audited — `release/openconfig` is the correct supported closure,
`experimental/` must stay out of the authoritative catalogue,
`pathreport.csv` is advisory-only
(`docs/merger/evidence/2026-11-23-m6fc-arista-yang-audit/`).
Stage D: cEOS 4.37.0F explicit selective acquisition slice PASS on
the production path — bounded operator-selected path, `observed`
coverage with advancing timestamps, interface-counter projection,
and verified withdrawal; schema-bound remains impossible (no YANG
Library). Finding: cEOS does not echo gNMI `Path.origin`, so
explicit plans must leave `origin` empty
(`docs/merger/evidence/2026-11-22-m6f-ceos-e2e/`,
`docs/M6F_CEOS_EXPLICIT_ACQUISITION.md`).
Stage E/regression: the merged code was revalidated live against SR
Linux 26.3.1-410 — full production path PASS
(`docs/merger/evidence/2026-11-23-m6fc-srlinux-r8/`), including
module-prefixed coverage under the new schema verification and all
seven authoritative withdrawal checks, with
`final_sha_acceptance_eligible=true`. Telemetry granularity confirmed:
coverage is container-granular by design (a container blob never
fabricates child-leaf coverage) and the mgmt0 counter chain
(device Get → collector → Core → API projection) verified with
advancing values. Remaining open items: XRd production-path needs a
revision-pinned OpenConfig/IETF source closure plus a schema-authority
policy decision on capabilities `organization` free-text conflicts;
Junos awaits a gNMI-capable image. Stage G resilience executed
PASS on the live SR Linux production chain
(`docs/merger/evidence/2026-11-23-m6fc-srlinux-g1/`): collector
restart (reconnect + resubscribe, new session), device
disconnect/reconnect (truthful `stale` degradation, bounded
retries, fresh session), Core restart (durable registry/vault,
collector re-established, new session) and post-recovery
revocation verified through all seven authoritative withdrawal
checks including old-session rejection. Not executed: assignment
withdrawal/reassignment mid-stream, catalogue change during an
active subscription, expired-evidence aging, revocation during
the reconnect window.

M6F-B update — **real Nokia SR Linux production-path
interoperability validated** (branch `feat/m6fb-gnmi-interop`).
The complete production chain ran against a live SR Linux 26.3.1
device twice with identical results: an isolated Core+collector
topology proved Core→device gNMI unreachability while the
authenticated Rust collector (protocol 6, mTLS) reached both Core
and the device; a Core-issued remote-discovery job retrieved live
Capabilities + RFC 8525 YANG Library evidence; Core admitted and
persisted applicability (snapshot `applicable`, schema
`ysc-d7254b6e840b0f0041c4b2458bb509a1`, 11 modules / 419 paths);
Admin previewed and explicitly activated an exact schema-bound
SAMPLE plan (`/interface[name=mgmt0]/statistics`, 10 s interval);
the collector submitted exactly the approved SubscribeRequest; real
telemetry was received, bound to the current authenticated session
and projected as `observed` coverage with advancing observation
timestamps; revocation withdrew acquisition and telemetry ceased.
No gNMI Set was issued. Production fixes landed alongside: canonical
`ietf-yang-library:yang-library` discovery path, Nokia YANG dialect
parsing (unquoted revision dates, prefixed extension
statements/instances, string and bare `presence`, bare and
`/`-bearing enum names, type arguments with `error-message` blocks,
and a block-statement skip fix that previously swallowed sibling
statements), module-prefixed notification element tolerance in
coverage matching, deterministic archive image selection, explicit
probe TLS modes, truthful matrix outcome states, and collector
resync on assignment override. Live finding: SR Linux reports
statistics at container granularity with module-prefixed element
names, so leaf-level plan paths cannot be covered — the accepted
plan selects the applicable container path. Evidence:
`docs/merger/evidence/2026-11-22-m6fb-srlinux-r6/` and `-r7/`;
matrix: `lab/acceptance/m6f/MATRIX.md`. Vendor expansion (same
branch): cEOS 4.37.0F probed live — Capabilities/SAMPLE/list-key/
initial-sync PASS at probe level, but gNMI YANG Library is
UNSUPPORTED so the production schema-bound path cannot reach
applicability (evidence `2026-11-22-m6f-ceos-probe/`); Junos
cJunosEVO 26.2R1.7-EVO boots but this cPTX image exposes no gNMI
service (evidence `2026-11-22-m6f-junos-probe/`); XRd 24.4.2 image
loads but the node aborts on host inotify limits pending an owner
sysctl change. All three remain NOT_EXECUTED for the production
path.

M6E update — **authenticated remote-collector YANG discovery
implemented** (branch `feat/bn015-m6e-remote-yang-discovery`).
An Admin can now request applicability refresh for a device Core
cannot reach directly: Core validates the device and its assigned
collector, queues a durable bounded discovery job (schema v12,
`yang_discovery_jobs`), the collector picks it up over the existing
authenticated registration channel (`PollYangDiscoveryJobs`, gated on
protocol ≥ 6 + the `yang_discovery_v1` feature), executes read-only
gNMI Capabilities + RFC 8525 YANG Library Get in an isolated bounded
worker, and submits bounded evidence (`SubmitYangDiscoveryResult`).
Core independently re-validates principal, job, session generation,
authority epoch, assignment generation, device identity, datastore,
payload bounds and observation times, then resolves applicability
through the same M6C-1 resolver and persists through the same snapshot
path — the collector never approves plans or declares paths
authoritative. Snapshots carry explicit evidence provenance
(`core_direct` / `authenticated_collector` / `legacy_unknown` plus
observing collector, job id, session and epoch references). Refresh
offers `core_direct` (synchronous), `assigned_collector` (202 +
job-status polling) and documented `auto` selection; the schema
workspace shows executor, live job state, failure reasons and
provenance, preserves drafts across async responses, and confirms
before refreshing a schema-bound device. M6D corrections landed with
it: fingerprint+recency-guarded approval invalidation at the registry
mutation boundary, segment-qualified nested-list keys, async response
fencing and truthful revocation messaging. Integration proof:
`tests/yang_remote_collector_discovery.rs` (T1–T16 wire-level
scenarios) plus M6E Playwright coverage in
`tests/ui_driver/yang_schema_workspace.spec.js`.

21 November update — **M6D device-effective YANG browser and
schema-bound collection workspace implemented** (branch
`feat/bn015-m6d-schema-workspace`).
Operators can now move through the full schema-bound workflow in the
existing UI: open a device, inspect the compiled-catalogue and
tri-state applicability evidence with freshness, browse/filter/paginate
schema paths server-side, instantiate list keys explicitly, choose
SAMPLE/ON_CHANGE modes and intervals, keep a persistent draft across
navigation, preview the exact server-evaluated plan (read-only,
Viewer-accessible), and — as Admin — activate with the previewed plan
id, binding fingerprint and `expected_config_revision`, or revoke the
binding with a recorded reason. The workspace renders staged truth —
desired → approved → assigned → submitted → synchronized → observed —
and never presents intended or authorized paths as received telemetry.
It is mounted as the device drawer's schema tab and reused from the
Devices, Profiles and Onboarding surfaces via one
`SchemaCollectionWorkspace.svelte`; `/api/auth/me` drives role-aware
controls while the backend remains the authorization boundary.
Four integration defects were corrected alongside: distinct instantiated
list instances no longer collapse under template-level dedup
(structured canonical identity preserves origin, key names/values, mode
and interval; same-path parameter conflicts still reject), activation
enforces optimistic concurrency on the device configuration revision
(`expected_config_revision` required; `config_revision_mismatch` 409;
atomic registry commit), gNMI wire origin is separated from selection
provenance end-to-end, and schema-binding preview is Viewer-readable
while activate/revoke/refresh require Admin. Evidence:
`docs/merger/evidence/2026-11-21-m6d-schema-workspace/`.
Remaining on this line: authenticated remote-collector discovery and
real-NOS interoperability acceptance (mock-gNMI only so far).

Earlier merged milestones on this line: **M6C-2 schema-bound
acquisition merged** (PR #42 @ 183da07 — deterministic evaluator over
the verified compiled catalogue plus freshest persisted M6C-1
applicability evidence; durable approval (`schema_bindings` +
`devices.schema_binding_json`, schema v11); additive wire fields;
collector-side fail-closed enforcement; Core-side lifecycle
verification; two-layer 24 h evidence expiry with a 60 s reconciler;
wire-level proof `tests/yang_schema_bound_acquisition.rs` 6/6;
evidence `docs/merger/evidence/2026-11-20-m6c2-schema-bound-acquisition/`),
**M6C-1 device-effective YANG applicability merged**
(`GET /api/devices/{address}/yang-applicability` + Admin refresh,
tri-state resolver, durable snapshots; evidence
`docs/merger/evidence/2026-11-20-m6c1-device-applicability/`),
M6A/M6B effective YANG catalogue (PR #40 @ e7cb22c), BN-022 protected
auth storage (PR #17), selective gNMI acquisition contract M5.1–M5.2B.

**20 November research intake (no implementation):** [Enterprise knowledge, dark-site assurance and diagnostic-runner design](architecture/ENTERPRISE_KNOWLEDGE_OUTAGE_AND_DIAGNOSTIC_RUNNERS_2026-11-20.md) is recorded as [BN-028](../BONSAI_BACKLOG.md#bn-028--enterprise-knowledge-and-dark-site-assurance-20-november-2026). The proposed enterprise-document facts, circuit/power/OOB map, site outage engine and optional read-only diagnostic runner are **not shipped**. This does not change the M6C-1/M6C-2 verified status or production gate claims.

20 November update — **M6C-1 device-effective YANG applicability merged.**
Bonsai now answers, per device, which compiled YANG catalogue paths are
evidence-justified as applicable: complete gNMI Capabilities evidence
(name/org/version per `ModelData`, encodings, security provenance),
bounded RFC 8525 YANG Library discovery over gNMI `Get` (NMDA
module-set/schema/datastore model plus legacy `/modules-state`
fallback, explicit outcome taxonomy), a deterministic tri-state
resolver (`src/yang_schema/applicability.rs`) honouring exact
revision/namespace/organization identity, feature `if-feature`
expressions, deviations, import-only modules and datastore linkage,
durable SQLite snapshots (schema v9, freshness + invalidation), and
read-only API (`GET /api/devices/{address}/yang-applicability`,
Admin `POST .../refresh`). Capabilities-only evidence never yields
`applicable`; unsupported/denied/malformed discovery stays explicit.
Applicability is schema-level only — not transport-probed, subscribed
or projected; acquisition enforcement is M6C-2, UI is M6D.
Evidence: `docs/merger/evidence/2026-11-20-m6c1-device-applicability/`.

Earlier merged milestones on this line: BN-022 protected auth storage
(PR #17; deployment limits noted in backlog), selective gNMI
acquisition contract M5.1–M5.2B (assignment lifecycle, subscription
truth), M6A/M6B effective YANG catalogue (PR #40 @ e7cb22c — pinned
source manifest, content-addressed compiled bundles, deterministic
schema paths, Admin-gated compile/activate).

The detailed snapshot below remains the 30 October–8 November baseline
and is historical context, not the current development stage.

8 November repository review: BN-006 bounded live acceptance is complete;
the detailed snapshot below is the 30 October baseline. BN-006 code and fixtures
were committed in `0b89f00`. The EOS reconnect replay regression is fixed, including legacy
event and checkpoint recovery. Live EOS lifecycle, offline policy update, virtual FIB,
and SR Linux native collection evidence is in [BN-006 evidence](merger/evidence/2026-11-07-bn006-live/README.md).

Baseline snapshot: 30 October 2026 (Asia/Kolkata). Base Git commit: `e4d8c44`.
The BN-001–005 implementation is uncommitted working-tree code. Source hashes
and fresh results identify exactly what this review inspected.

**Standing: integrated development prototype.** Routing history/recovery is
substantially improved, dual live FRR streams and the browser workflow have
reproducible coverage. All three BN-001/002/003 items still have acceptance gaps.

Work from [BONSAI_BACKLOG.md](../BONSAI_BACKLOG.md). Latest review:
[30 October code/environment analysis and BN-006 native slice](merger/CURRENT_STATE_REVIEW_2026-10-30.md).
[Current evidence](merger/evidence/2026-10-30-bn006/results.json).
Earlier reviews and counts below are historical snapshots, not new live acceptance.

30 October revalidation: **46 focused Rust + 132 imported-crate tests** pass
(2 ignored tests excluded), plus **16 Python tests**. UI build and format/diff
checks pass; existing UI warnings remain. The new live slice is SR Linux CLI
capture only, not fresh FRR/browser or full second-NOS route acceptance.

| Area | Verified subcriteria | Remaining acceptance |
|---|---|---|
| BN-001 Partial | Durable scope/intent/completion records, direct failure/reopen tests, replay binding, open-time corruption/tail checks, cross-process scope ownership confirmed; runtime reload fails closed on corrupted/empty journal on disk (JournalPersistenceError); off-to-the-side atomic parsing under lock | Phase-specific process-kill and crash-phase recovery tests; actual device writes remain disabled |
| BN-002 Partial | Length-prefixed approval signing, mandatory enforced backing/identity/authority, report-fidelity binding, simulation-replay rejection; length-prefixed canonical report encoding eliminating identity delimiter collisions and binding limitations; enforced replay requires signing authority | Operational key lifecycle/migration policy and trusted live-lab assurance provenance (BN-014) |
| BN-003 Partial | Fresh 12-browser-journey run passes; submitted/fresh capture has zero exact fixture matches; native cookie and token-revocation checks exercised. 8 Nov: acceptance gate rewired to the genuine VRF stream (no `build_rd_bmp_frames` injection) — 12/12 Playwright journeys pass on real FRR 10.6.1 dual-session BMP (global + VRF BLUE, RD 65000:100, 10.99.1.0/24); `run_bmp_listener` epoch-sharing fix verified live (`complete_for_observed_view` + journal restart recovery) | Live-router VRF browser acceptance now evidenced; remaining gates are operational (BN-014/019 soak/pilot) |
| Routing history R01–R04 | 7 current regressions pass | Production scale/fault/soak remains release work |
| Wiring/scope R05–R06 / BN-004 | 2 wiring/scope regressions pass; provider clients have mock integration coverage | BN-004 real model comparison pending; mock scenario token/cost/conclusion is not model evidence; deployment/query scope under BN-005 |
| BN-005 Partial — temporary exception | Prior strict closure and current strict-mode regression assertions remain; default `audit_only` now relaxes tenant/unsupported-VRF denials at user request. Mappings persist; admin policy endpoint can restore strict denials | Tenant isolation is not enforced in compatibility mode. Authentication, roles, speaker and supported RD restrictions remain. See [policy contract](architecture/ownership_mapping.md) |
| BN-006 Complete (bounded live profile) | EOS 4.36.0F BMP lifecycle and offline startup-policy change across default and BLUE/RD 65001:100; cEOS software-FIB route/withdrawal probes; live SR Linux 26.3.1 collection through operator API and MCP; reconnect replay regression | No periodic scheduler, physical ASIC claim, IPv6, extra release/vendor coverage, or DuckDB EOR retirement parity. See [evidence and support limits](merger/evidence/2026-11-07-bn006-live/README.md) |
| ML/research | Metrics/alignment/prototype smoke evidence remains | Trained held-out models, actual forecasting/provider comparisons remain separate gates |
| Release hygiene | Format, strict selected-core Clippy and UI build pass; UI build warning-free with route-level code splitting and live-doc links verified (8 Nov, [UI](merger/evidence/2026-11-08-bn019-ui-warnings/README.md), [links](merger/evidence/2026-11-08-bn019-link-debt/README.md)) | Full-product release gates remain |

BN-005 closeout snapshot verification: **62 focused Rust tests**, **19 explorer unit tests**, **132 imported-crate tests**
(**2 older FRR tests remain ignored and are not counted as passes**), and **12 live
FRR Chromium journeys** (plus both Rust browser orchestrator tests) pass. The real
Docker named-volume/container replacement gate and **16 Python tests** pass.
Formatting, strict selected-core Clippy and UI build pass; existing UI warnings remain.
The library test target compiles after repairing stale test fixtures; the other
249 library tests and a full workspace release/soak run are not claimed as executed.

The strict BN-005 profile remains tested. The current default has a user-requested
temporary exception: tenant and unsupported-VRF ownership restrictions no longer
block access. Speaker and supported routing RD filters remain. This default is
not a tenant isolation guarantee. Use the admin policy API to restore `strict`.
The original closeout manifest identifies its earlier tested source snapshot.

**Next action:** BN-019 operational/release proof is effectively complete —
packaged build/boot, volume backup→restore, install/upgrade, `bonsai-migrate`
drill, packaged-deployment browser acceptance (12/12 journeys on genuine
FRR BMP), bounded packaged soak, and handoff reconciliation all evidenced.
Only the full-length soak window remains open (shared with BN-009).
UI warning and link/provenance debt are cleared. BN-009/BN-013 intake triage remains doable
without external deps.
BN-003 genuine router VRF/deployed browser-graph parity, and BN-004 real
provider evaluation remain open. Operational device writes remain disabled.

BN-007 intake (8 November): legacy/duplicate-authority inventory completed;
fixed a settings-API path that could spawn a second BMP authority, a dead
`[bmp]` config block, and license-gate failures (`cargo deny` passes;
135 imported-crate tests pass). Added `bonsai-migrate` operator CLI
(backup/migrate/rollback, SHA-verified end-to-end), `authority` typo warning,
and LICENSE/NOTICE RustyBMP attribution. Journal backups confirmed covered
via `bonsai_graph` volumes. Leftovers disposed: rbmp crates versioned to
0.2.0, `duckdb-store` kept optional-disabled pending EOR parity, legacy
receiver retirement still blocked on projection parity, CI/install gates
deferred to BN-019. Evidence: [BN-007 audit](merger/evidence/2026-11-08-bn007-audit/README.md).

BN-009 live heartbeat (8 November): `collector_engine.py` registered with
the packaged deployment over gRPC — `/api/sidecars` shows healthy with
advancing `status_json` (uptime/cpu/rss/queue/rules_loaded=35); SIGTERM →
registry transitions stale (45s) → lost (120s) matching
`sidecar_registry.rs` thresholds. Gap found: sidecar ML/export jobs hit
unauthenticated 401s on the JWT API (no credential path); heartbeat
timestamp fields render null. Evidence:
[live heartbeat](merger/evidence/2026-11-08-bn009-live-heartbeat/README.md).

BN-009 sidecar→API auth (9 November): `BONSAI_API_TOKEN` (`bsk_` API key)
support added via `bonsai_ml/api_auth.py` and injected into all sidecar
HTTP call sites (job engine aiohttp, requests workers, urllib probes,
`BonsaiClient` env fallback, snapshot/inference clients). Verified live
against the packaged deployment: minted `bn009-sidecar` key (operator
scope), restarted `collector_engine` — zero 401s (was 21), ML jobs
succeed via scheduler with real `MlJobRun` records, schedules/exports
writes land. Also fixed `export_job` `quality_json` type bug (PATCH 422 →
records stuck `running`). `last_heartbeat_at` null was a field-name
expectation, not an API defect (`*_ns` fields populated). New findings:
`/api/graph/snapshot` 500s on missing DuckDB `TIMESTAMP_NS` (pre-existing,
affects JWT too); pandas absent in venv → empty parquet exports.
Evidence: [api-auth](merger/evidence/2026-11-09-bn009-api-auth/README.md).

BN-009 restart/overload slice (9 November): sidecar SIGKILL → registry
stale (~61 s) → lost (~125 s) → restart re-registered healthy with zero
401s; `docker restart` of the app container exercised the
`reregister_required` heartbeat path (new sidecar_id, job/export/schedule
state durable); routing container restart replayed the journal to
identical RIB state (`durable_sequence=297`, `peers_up=6`,
`transport_gaps=1` unchanged — no synthesized withdrawals). Forward-queue
overload exercised same day via an env-gated synthetic detection injector
(`BONSAI_TEST_DETECTION_COUNT`, off by default): with core unreachable the
queue reached 946/1000, `queue_drops_today=549`, `priority_only_mode=true`,
and graceful shutdown reported `drops=549` — no silent loss, drops are
counted and surfaced. Full-length soak window STARTED 9 Nov 05:48Z —
detached 60 s sampler writing
`docs/merger/evidence/2026-11-09-bn009-soak-full/soak-metrics.csv`
(routing health, sidecar health, registry, container memory); finalize by
reviewing the CSV for gaps/drops/memory growth.
Evidence:
[soak](merger/evidence/2026-11-09-bn009-soak/README.md),
[overload](merger/evidence/2026-11-09-bn009-forward-queue-overload/README.md).

BN-009 graph snapshot slice (9 November): `/api/graph/snapshot` and the DB
purge path were fixed by binding timestamps as Lbug `TimestampNs` parameters
instead of the unsupported DuckDB `TIMESTAMP_NS(...)` call. Verified on a
rebuilt packaged deployment (`bonsai:latest`, image build 485.50 s): snapshot
endpoint 500→200 with real topology (3 devices, 11 BGP sessions), and a
disposable `GnnScore` purge deleted exactly 1 row while real counts stayed
unchanged. Follow-up same day: two defects found during scheduled-trigger
verification were fixed — `_register_schedule` passed
`conflict_policy="replace"` as a string while APScheduler 4.0.0a6 compares
against the `ConflictPolicy` enum (schedule updates silently dropped), and
`SnapshotStore._write_arrow` wrote heterogeneous record batches into a
single-schema Arrow IPC file (corrupt files, `store.health()` also called a
nonexistent method). After fixes, scheduled `graph_snapshot`/`gnn_snapshot`
fired via APScheduler test crons and succeeded with real `MlJobRun` records;
schedules restored to `0 */4 * * *`. Evidence:
[graph snapshot](merger/evidence/2026-11-09-bn009-graph-snapshot/README.md),
[scheduled jobs](merger/evidence/2026-11-09-bn009-scheduled-snapshot-jobs/README.md).

BN-009 lifecycle slice (8 November): `collector_engine.py` heartbeat
`status_json` now reports uptime/cpu/rss/queue-depth/drops/rules-loaded
(stdlib /proc, null-safe); Sidecars UI gained a gRPC-registry panel rendering
`/api/sidecars` with parsed metrics — the registry was previously not
surfaced in the UI. Verified: py_compile, /proc parsing, clean UI build.
Supervision/resource-limit gaps closed by inspection (single combined
sidecar unit + `BONSAI_SIDECAR_MAX_MEM_MB` enforcement). Live heartbeat
exercise done same day (see above); remaining: restart/overload soak
acceptance.
Evidence: [BN-009 lifecycle](merger/evidence/2026-11-08-bn009-lifecycle/README.md).

BN-020 first slice (9 November): UI login page added — `Login.svelte` +
auth gate in `App.svelte` probing `/api/setup/status` (401 → login screen,
HttpOnly `bonsai_token` JWT cookie, no JS token storage). `admin` DB user
created with policy-compliant lab password (literal admin/admin rejected
by password policy). Verified live on rebuilt image: unauth 401 → login
success (role=admin) → authed 200; wrong password rejected; brute-force
Gemini provider fixed same day: Interactions API implemented as primary
(`POST /v1beta/interactions`, store=false, typed function steps) with
generateContent preserved as user-selectable legacy (`api_style` per
provider in vault + Settings UI); provider timeout raised to 120s for
thinking models. Verified live: interactions ok 13.8s, legacy ok 39.5s
on gemini-3.8-flash. BN-014 intake (9 November): change-management machinery inventoried —
change_management.rs (ServiceNow sync, webhook ingest, event/incident
linking), /api/changes + context endpoints, governance.rs,
scenario_pack.rs, approval.rs. Gaps: baseline/candidate capture,
independent invariants, failure rehearsal, episode utility/failure
taxonomy. Proposed slices BN-014a–d; first slice is read-only
baseline/candidate capture on containerlab NOS instances. Evidence:
[intake](merger/evidence/2026-11-09-bn014-intake/README.md).

BN-013b (9 November): adjudication + feedback + real-provider
investigation verified live — test-openapi (gpt-6-luna) ran the full
agentic loop (8 tool calls, 61k tokens, terminal `budget_exhausted` at
$0.10 cap); feedback persists FeedbackRecord, accuracy endpoint
reflects it, adjudicate-prefix persists adjudication + accuracy.
Provider fixes: Gemini Interactions API primary + legacy fallback +
per-provider api_style, 300s timeout, function_result name fix, OpenAI
reasoning_effort=none for tool turns. Gaps: no `complete` status yet
(Gemini 503s + budget cap); Interactions tool-calling needs schema
work. Evidence: [bn013 adjudication](merger/evidence/2026-11-09-bn013-adjudication-feedback/README.md).

GitHub Actions workflows removed — account
billing-locked, every job failed in ~2s and emailed on each push
(evidence: [gemini interactions](merger/evidence/2026-11-09-bn020-gemini-interactions/README.md)). rate-limit lockout observed live (3 attempts/5min → 30min). Same day:
logout added (sidebar Sign out → `POST /api/auth/logout` → 401 after);
post-lockout re-verified — admin login success, cookie-session provider
save `has_api_key:true`, logout→401. Evidence:
[ui login](merger/evidence/2026-11-09-bn020-ui-login/README.md).

BN-013 first slice (9 November): traced one live incident journey on the
packaged deployment. `/api/incidents` returns 2 grouped incidents with
populated correlation chain/blast radius; `POST /api/investigations`
creates a record (HTTP 200) but the AI investigation remains
`status=running` with `tokens_used=0`/`tool_calls=[]` after ~55 s — likely
no AI provider configured or a stalled spawn, and there is no
failure/timeout transition. `completed_at_ns` equals `started_at_ns` while
running; no incident→investigation automation exists. Break points
recorded; next: adjudication/feedback endpoints and investigation
lifecycle states. Evidence:
[incident trace](merger/evidence/2026-11-09-bn013-incident-trace/README.md).
BN-013a (9 November): fixed — investigations created with no AI provider
configured now reach terminal `failed` with
`STOP_REASON=no_ai_provider` and a real `completed_at_ns` instead of
sticking `running` forever; verified live on rebuilt `bonsai:latest`.
This deployment has no AI provider configured, so grounded-RCA quality
remains BN-004-blocked. Evidence:
[investigation lifecycle](merger/evidence/2026-11-09-bn013-investigation-lifecycle/README.md).

BN-014a baseline/candidate capture (9 November): the change webhook and
capture path had three latent defects, all fixed and verified live on the
packaged deployment. lbug's Cypher lexer reserves `DESC`, so the
`$desc` query parameter failed to parse (`expected rule oC_SingleQuery`)
— renamed to `$description` in the ChangeRequest upsert plus the
identically broken SavedQuery create and both Playbook writes.
`load_device_ci_map` selected nonexistent `Device.snow_sys_id`; CI
sys_ids actually live on `EnrichmentProperty` (`key='snow_ci_id'`) and
the map now resolves hostname→address from those. `change_capture`'s
routing-service client used `Authorization: Bearer` while the service
requires the `x-bonsai-routing-token` header (same as the
`/api/routing/v1/*` proxy). Regression test
`change_request_upsert_creates_then_matches` covers upsert create+match
and full `ingest_webhook_change`. Live: webhook ingest 200 (twice —
ON CREATE and ON MATCH), baseline+candidate captures returned real
counts (route `10.10.1.0/24`, 2 peers up on 172.100.207.12), and
`/api/changes/{id}/snapshots` reports `status=diffed` with honest zero
deltas (no routing change was made between phases). Evidence:
[bn014a lbug upsert](merger/evidence/2026-11-09-bn014a-lbug-upsert/README.md).

BN-014b invariant checker (9 November): `POST
/api/changes/{id}/verify-invariants` evaluates four independent
invariants — `capture_complete`, `no_new_peer_downs`, `prefix_retention`,
`expected_prefixes_present` (operator-supplied, `skipped` when absent) —
purely from the persisted baseline/candidate snapshots, persisting
per-device verdicts as `change_invariant_verdict` ConfigItems readable
via `GET /api/changes/{id}/invariants`. Verified live: real snapshot
pair → overall `pass`; bogus expected prefix → overall `fail` with the
missing prefix named; 3 unit tests pass. Evidence:
[bn014b invariants](merger/evidence/2026-11-09-bn014b-invariants/README.md).

BN-014c rehearsal (9 November): ran a controlled failure during an open
change window on the packaged deployment — webhook change
`manual:bn014c-rehearsal-001`, baseline capture, admin-down of frr2's
eBGP session to frr1 via vtysh, candidate capture, invariant
verification, restore, recovery re-capture (pass). Honest gap recorded:
the admin shutdown did not surface in the BMP-fed routing view within
~6 minutes (peer stayed `up`, no new `/events`) — peer-down/withdrawal
propagation needs investigation before claiming detection coverage for
this fault class. Evidence:
[bn014c rehearsal](merger/evidence/2026-11-09-bn014c-rehearsal/README.md).
BMP peer-down investigation same day: rbmp-rib PeerDown handling verified
correct in code; across admin shutdown AND hard `clear bgp` resets,
bonsai-routing logged zero peer events and peers stayed `up` while FRR's
`show bmp` MonSent counter incremented ~2/flap — strong indication FRR
10.6.2 does not emit BMP PeerUp/PeerDown on these transitions, but no
wire capture was possible (no CAP_NET_RAW, no FRR bmp debug cmd).
Documented next steps incl. privileged tcpdump and a hold-time/staleness
fallback. Evidence:
[bmp peerdown investigation](merger/evidence/2026-11-09-bmp-peerdown-investigation/README.md).

Frame-level tracing (10 November): `BONSAI_BMP_TRACE=1` added to
`run_bmp_listener` (commit f4f0363) logging every reconstructed BMP
frame's type/peer header. Three controlled flaps (admin shutdown + hard
clear, 45–60s windows) produced **zero BMP frames of any type** —
no PeerDown (type 2), no PeerUp (type 3), not even route-monitoring —
while the single verified BMP TCP session stayed ESTAB and the trace
itself proved live on reconnect. Peer identity confirmed as the real
type-0 neighbor (not a Loc-RIB virtual peer). Conclusion: the
notification disappears **upstream of Bonsai's decoder** — no frame
reaches the listener. Wire capture remains blocked (no CAP_NET_RAW); a
TCP-proxy or privileged capture container is the recommended next step,
plus FRR-side `debug bgp neighbor-events` logging. Consolidated record:
[bmp peerdown full investigation](merger/evidence/2026-11-10-bmp-peerdown-full-investigation/README.md).

**ROOT CAUSE (10 Nov ~04:05, FIXED ~04:10):** two `bonsai-routing`
instances — a stray `nohup ./target/debug/bonsai-routing` on the host
(PID 2633113, since 8 Nov) owned `172.100.207.1:11019` and received ALL
live BMP; the Docker container only had `127.0.0.1:11019` published,
unreachable from clab nodes, serving stale replayed state to the app.
Stray process stopped; container now publishes
`172.100.207.1:11019:11019`. **PeerDown verified end-to-end**: admin
shutdown → `mtype=2` frames → `peer down LocalSystemClosed2` → API shows
`10.100.1.0 down`/`10.100.1.1 down`; restore → PeerUp → `up`. FRR,
transport, decoder, manager, and API were never defective. New
secondary finding: docker-proxy masquerades clab BMP sources to
`172.20.0.1`, collapsing frr1+frr2 under one speaker identity —
follow-up: host-network mode or speaker disambiguation.

BN-014 completed on live data (10 November): after the deployment fix,
the full change-assurance cycle ran against real BMP state — webhook
`manual:bn014-live-001` → baseline (5 peers, 6 routes under speaker
`172.20.0.1`) → frr2 neighbor admin-down → candidate (4 routes) →
`verify-invariants` returned a **genuine `fail`**
(`no_new_peer_downs`: down peers 0→2) → restore → re-verify `pass`.
First nonzero-diff, failing-invariant result on live data; all four
BN-014 slices (capture, invariants, rehearsal, episode record) now
verified end-to-end. Evidence:
[bn014 live verification](merger/evidence/2026-11-10-bn014-live-verification/README.md).

BN-009 intake reconciliation (8 November): existing sidecar lifecycle
surfaces inventoried — registry/heartbeat/stale-lost, `BONSAI_REQUIRE_SIDECAR`
health gate, `/api/ml/{exports,schedules,jobs}` incl. cancel, job_engine
memory-pressure shedding, `/sidecars` UI route, rules-sidecar systemd unit.
Build should extend these; gaps are unified event contract, watchdog
coverage, backlog visibility, per-sidecar resource metrics, soak acceptance.
Evidence: [BN-009 intake](merger/evidence/2026-11-08-bn009-intake/README.md).

BN-003 genuine VRF BMP — WIRE PROOF PASSED (8 November): rebuilt the lab
per FRR 10.6.2 research — kernel VRF BLUE provisioned as root before FRR
start (`bootstrap.sh` via containerlab `cmd:`), `rd vpn export 65000:100` in
the VRF BGP instance, real eBGP route `10.99.1.0/24` from frr2. FRR emitted
genuine BMP Peer Up + Route Monitoring with peer type=1 RD-instance
distinguisher 65000:100 — verified on the wire by `verify_bmp.py`, 5 frames
captured. Earlier failure root-caused: FRR does not create Linux VRFs and
never needed the capability workaround. Lab `bonsai-bn003-vrf` left running.
Remaining: deployed
browser/graph restart journey and acceptance-test fixture replacement.
Phase 2 same day: `bonsai-routing` ingested the genuine stream — RdInstance
peer 65000:100 up, `10.99.1.0/24` in LocRib + AdjRibIn pre/post-policy views
with VRF key/exporter identity preserved; restart gap honestly recorded.
Evidence:
[genuine VRF BMP](merger/evidence/2026-11-08-bn003-genuine-vrf-bmp/README.md).
Earlier blocked attempt:
[BN-003 VRF lab](merger/evidence/2026-11-08-bn003-vrf-lab/README.md). NetBox restarted (healthy, 8 seeded
devices); Splunk/Elastic/Kibana/Grafana/Prometheus confirmed up; stale clab
containers removed. Evidence:
[BN-003 VRF lab](merger/evidence/2026-11-08-bn003-vrf-lab/README.md).

BN-019 packaged build + boot gate (8 November): `docker/Dockerfile.bonsai`
now builds a working `bonsai:latest` image (added `protobuf-compiler`,
`mold`, `sccache`; `CARGO_PROFILE_RELEASE_*` env pins so cargo-chef deps
are reused — cook 12 s, bin step 9 m vs prior 15–47 min double compiles).
Fixed container HEALTHCHECK (`/readyz`, was auth-gated `/api/readiness`)
and gave `bonsai-routing` a token-authenticated probe. Standalone compose
profile boots healthy; `/health` ok, UI served, all `/api/routing/v1/*`
proxy endpoints 200 via JWT. Volume backup→restore drill passed after
fixing project-prefix resolution in `backup_volumes.sh`/
`restore_volumes.sh`. Install/upgrade, lab browser acceptance, soak and
handoff reconciliation remain. Evidence:
[packaged build](merger/evidence/2026-11-08-bn019-packaged-build/README.md).

BN-019 install + upgrade drill (8 November): `scripts/install.sh` executed
end-to-end in an isolated Compose project (pristine clone @ `ec50410`,
project `bn019inst`, ports remapped to 13000/25051/127.0.0.1:11020 via
`COMPOSE_FILE`/`COMPOSE_PROJECT_NAME` — running deployment and lab
`bonsai-routing` untouched). Install-from-package PASS: build → both
containers healthy → `/health` ok, JWT login, all `/api/routing/v1/*`
200 (`prefix-journey` requires `?prefix=`). Idempotent re-run is a no-op
(byte-identical image, no container recreation). Upgrade-in-place PASS:
forced new image (`sha256:e5c32496c77f`) recreated both containers; graph
volume sentinel, admin login and routing proxy verified post-upgrade.
Not exercised: interactive passphrase prompt, `--source`/`--uninstall`/
`--verify`. Evidence:
[install drill](merger/evidence/2026-11-08-bn019-install/README.md).

BN-019 bonsai-migrate drill (8 November): host-side run of the BN-007 CLI
against real data — 2,890-record genuine FRR lab journal snapshot (all
schema v2; migrate rewrites bytes but preserves semantics — rollback
restores exact SHA), synthetic v1 fixture (2 migrations flagged
`legacy_identity_incomplete`, 1 corrupt line skipped), legacy TOML vendor
alias migration, and corrupted-backup rollback fails closed. Binary is
not shipped in the image; quiesced-copy operator procedure documented.
Evidence: [migrate drill](merger/evidence/2026-11-08-bn019-migrate/README.md).

BN-019 packaged browser acceptance + soak + handoff (8 November): all 12
BN-003 Playwright journeys passed against the packaged `bonsai:latest`
compose deployment fed by genuine FRR BMP — required host-network
`bonsai-routing` (bridge SNAT collapses exporter IPs to the docker
gateway; documented as a packaging finding) and clearing the first-run
onboarding gate via admin API. A control shim drove vtysh +
`docker restart` for the journey hooks. 12-minute packaged soak: 24/24
samples `complete_for_observed_view`, 0 gaps/restarts, stable memory;
shared BN-009/BN-009 soak design recorded, full-length window still open.
Legacy CV5 clean-slate/7-day criteria reconciled to the read-only
prototype boundary (unattended-proof/chaos/governor deferred to
BN-009/014/016). Evidence:
[browser](merger/evidence/2026-11-08-bn019-packaged-browser/README.md),
[soak](merger/evidence/2026-11-08-bn019-soak/README.md),
[handoff](merger/evidence/2026-11-08-bn019-handoff/README.md).

BN-019 link/provenance debt (8 November): scripted check of 164 relative links
across 84 live docs found 4 dead links (absolute `file://`/machine paths, two
pre-consolidation backlog paths, and a never-committed CV7 sprint doc in
`sidecars.md`); all fixed, re-check clean. Archived backlogs excluded per their
freeze policy; `B10_IMPORT_MANIFEST.md`, `LICENSE`, `NOTICE` verified present.
Evidence: [link debt](merger/evidence/2026-11-08-bn019-link-debt/README.md).

BN-019 progress (8 November): UI accessibility/bundle warning debt cleared —
zero-warning `vite build`, route-level code splitting (entry 637 kB → 98 kB),
proper label/listbox/dialog semantics and programmatic palette focus. Browser
smoke over 15 hash routes passed on `vite preview`. Install/upgrade, lab
acceptance, backup/recovery and soak gates remain. Evidence:
[BN-019 UI warnings](merger/evidence/2026-11-08-bn019-ui-warnings/README.md).

BN-002 progress (8 November): signing keys and revocations are now durable in a
versioned key store beside the executor journal — register/rotate-overlap/revoke
survive restart and propagate across executor instances; corrupt or vanished
stores fail closed. Assurance reports carry optional HMAC signatures that
enforced mode now requires (`MissingAssuranceSignature`). 34 focused tests pass
(20 + 8 + 6). Remaining: vault custody, per-role key separation, boundary
transport, asymmetric crypto. Evidence:
[BN-002 key lifecycle](merger/evidence/2026-11-08-bn002-key-lifecycle/README.md).

BN-001 closeout (8 November): phase-specific process-kill recovery is verified
at all five durable executor journal phases using real child-process aborts, and
interrupted intents reconcile via the operator-driven
`abort_interrupted_execution` API (durable `ExecutionAborted` + scoped lock
release). 28 focused tests pass (20 existing + 8 new). Limits and evidence:
[BN-001 crash recovery](merger/evidence/2026-11-08-bn001-crash-recovery/README.md).

Ownership mapping follow-up (BN-015): explicit device and exporter/RD → tenant
bindings persist in the graph, with admin assignment/list/audit endpoints. Missing
and mismatched mappings add no access denials. Tenant/unsupported-VRF denials are
now temporarily relaxed; authentication, roles and supported speaker/RD controls remain.
This is an inventory/audit foundation, not completed tenant isolation. See the
[rollout contract](architecture/ownership_mapping.md).

Ownership follow-up verification: **3 ownership tests + 32 existing regressions**
pass; formatting and diff checks pass. The ownership suite includes JWT and
API-key administrator attribution. [Evidence](merger/evidence/2026-10-15-ownership-audit/results.json).
No operational owner assignments have been populated.

Temporary access-policy follow-up: **38 distinct Rust tests pass** (35 existing
mapping/regression tests plus 3 compatibility tests). Strict acceptance fixtures
retain all original assertions; separate tests verify the requested relaxation,
retained speaker/RD/role constraints and persisted admin restore switch.
[Current evidence](merger/evidence/2026-10-15-ownership-compatibility/results.json).


8 November BN-006 closeout: the live evidence validates only the bounded support
profile in the linked report. Native collection is operator-triggered; no scheduled
polling is present. The test used isolated task-owned cEOS/SR Linux nodes and made
no live configuration writes. BN-007 still owns the optional DuckDB EOR lifecycle gap.

BN-021 source-identity workstream opened (10 November): architecture
review extracted to backlog — canonical device/exporter/transport
identity model, per-protocol attribution rules, P0–P2 stages. P0
listener audit done ([audit](merger/evidence/2026-11-10-bn021-listener-audit/README.md)):
sFlow already uses payload agent_address (reference pattern); BMP
speaker still transport-IP keyed (Initiation sysName parsed but unused);
NetFlow template cache keyed by socket peer; syslog/SNMP use transport
peer_addr. Lab networking fixed via host-mode routing (speakers split
to real IPs). Also landed: change captures + invariant verdicts are now
append-only (`css-…-{phase}-{ts}`, `civ-…-{ts}`) — verified live, 3
verdict records preserved where 1 previously overwrote; pairing selects
newest per phase. Next BN-021 slices: (1) canonical exporter/device identity
contract + inventory bindings; (2) BMP connection-scoped exporter
resolution via Initiation; (3) NetFlow/IPFIX exporter provenance +
protocol-scoped template-cache hardening; (4) syslog/SNMP/OTLP
attribution + trust; (5) production acceptance, alias reconciliation,
monitoring-health semantics. NetFlow template caches already separate
by SocketAddr + source/domain ID — no NAT template collision
demonstrated; confirmed gap is downstream `exporter_address` using
bare transport IP.

BN-021 slice 1 verified live (10 November): peers API now returns
`speaker_sys_name`/`speaker_sys_descr` from BMP Initiation — confirmed
`frr1`/`frr2` per speaker on the packaged deployment. Slice 2
(NetFlow/IPFIX) analyzed: template cache is already collision-safe
(SocketAddr key incl. port); residual gap is downstream
`exporter_address` identity collapse — deferred to the canonical
exporter-identity layer rather than a format-breaking patch.

BN-021b identity-resolver slice (10 November): the canonical identity
contract now has a trusted-binding resolver and durable storage.
`telemetry_identity.rs` gained protocol/scope/collector/session-epoch
fields on the evidence envelope, `TrustedBinding` + `CanonicalDevice`
types, and a stateless `IdentityResolver` (resolved / provisional /
ambiguous / unknown / conflicting; only Resolved may drive
authoritative state). SQLite schema v3 adds `canonical_devices`
(opaque `dev-<uuid>` id stable across address changes) and
`exporter_bindings` (scope+protocol+exporter → device, optional
evidence pins, provenance, revoke-only invalidation) — additive, the
address-keyed `devices` table is untouched. Bindings require an
existing canonical device and reject overlapping claims at
registration; admin-only `/api/identity/*` endpoints provision them
(telemetry cannot mint bindings). `identity_from_bmp_observation`
adapts `ObservationSource`+sysName into the envelope — NOT yet wired
into ingestion; PeerKey/RIB/journal remain transport-IP keyed (that is
BN-021c). Verified: `cargo check --lib` clean; 13/13 tests in
`tests/bn021b_trusted_identity.rs` pass incl. v2→v3 migration on a
populated DB; BN-022 read-boundary suite re-run as regression.

BN-021c exporter-isolation slice (10 November): BMP ingestion, session,
RIB, journal, replay and projection state are now keyed by logical
exporter identity, not transport IP. `SpeakerKey{speaker_addr,
speaker_instance}` partitions `RibManager.speakers`; `PeerKey` gained a
`speaker_instance` discriminator (FNV-1a of the exporter scope, `Copy`
preserved); `ObservationSource`/`BmpMessage` carry
`exporter_instance`/`exporter_id`/`device_id` (serde-defaulted, legacy
records map to instance 0). The listener now requires a BMP Initiation
before creating state, resolves sysName evidence against the trusted
binding store (`bonsai_config.db` beside the journals or
`BONSAI_ROUTING_IDENTITY_DB`), and assigns namespace-encoded `exp:*`
scopes (binding scope+protocol+canonical id — identical canonical
exporter ids in different sites cannot collide) to resolved exporters
(shared across connections = FRR global+VRF association preserved) or
isolated `conn:{id}` scopes otherwise — unresolved/ambiguous/
conflicting evidence fails closed and is never promoted to canonical
device state. Disconnects, transport gaps, EOR clearing, decode
contexts and `conn:*` retirement are all fenced per `SpeakerKey`.
Journals/replay carry the scope end-to-end; `JournalTransportGap`/
`PrefixHistoryItem` gained `exporter_instance`; projection/checkpoint/
graph ids use `ip|instance`.

BN-021c hardening pass (10 November): audit found and fixed five
defects — (1) replay recorded `conn:*` disconnect gaps but never
retired the speakers, diverging from live state (now retires on the
disconnect record and prunes gaps/decode contexts); (2)
`process_for_epoch` fenced only stale Termination/EOR, letting stale
PeerUp/PeerDown/RouteMonitoring mutate newer sessions (now all
payloads fenced; stale EOR still reserves its event sequence for
deterministic replay); (3) `exp:{canonical}` dropped the
(scope, protocol) namespace — latent cross-site collision (now
namespace-encoded); (4) routes/prefix-history/events APIs lacked
exporter-instance selectors (added); (5) `conn:*` retirement was not
gated on the disconnect actually being accepted (now gated).
`tests/bn021c_exporter_isolation.rs` grew to 7 tests: cross-scope
duplicate canonical ids, trusted bindings behind one NAT IP, stale
epoch fencing, journal replay equivalence, and a real-TCP loopback
listener test with two simultaneous exporters. Verified:
`cargo check -p bonsai --all-targets` clean; bn021c 7/7, bn021b 13/13,
bn005_closeout 11/11, rbmp-rib 42/42, rbmp-store 3/3,
bn006_reconnect_projection, r01, r05, ownership and read-boundary
suites pass. Known limitation: `conn:*` speakers with no disconnect
record (SIGKILL) replay as live last-known state — the journal cannot
distinguish them from live connections; deferred to BN-021f alias
reconciliation.

BN-021c review-findings pass + NAT acceptance (10–11 November): the
published-commit review findings were reproduced and dispositioned.
P0-A: `identity_from_bmp_observation` resolved only the default scope
and `exp:{canonical}` dropped the (scope, protocol) namespace — fixed
via `RoutingServiceConfig.identity_scope` and namespace-encoded
`exp:{scope}:{canonical}` instances (default scope keeps legacy
`exp:{canonical}` for journal continuity). P0-B: an invalid BMP frame
broke the read loop without a disconnect reason — fixed; rejections
record distinct `bmp_frame_rejected:*`/transport-error reasons and
still run conn-scope retirement. P0-C: BMP Termination removed the
whole `SpeakerKey` session, so one FRR connection's Termination could
kill sibling state — fixed by deferring termination per sending
connection. P0-D: identical self-reported sysName claims coalesce —
confirmed as a trust assumption (lab-verified: an impostor claiming
`frr-a` merged into `exp:exp-frr-a`); ambiguous/conflicting claims
fail closed into `conn:*` isolation; authenticated ingress identity
remains future work. P1: the routing service now opens the identity
DB read-only (`open_file_readonly`) — no schema/WAL mutation. P2:
FNV speaker-instance hashing documented as a bounded collision risk.
`tests/bn021c_exporter_isolation.rs` is now 12 tests including real-TCP
loopback coverage of invalid-frame lifecycle and Termination sibling
isolation. Live containerlab NAT acceptance is DONE — three FRR
exporters (AS 65001/2/3) behind one iptables SNAT source
(10.240.0.200 → bonsai 10.240.0.20:11019) with a BGP route-server;
scenarios A–G (isolation, withdrawal, PeerDown, disconnect/reconnect,
unknown exporter, conflicting claims, SIGKILL replay) all verified
against the deployed HTTP API — evidence in
`docs/merger/evidence/2026-11-10-bn021c-nat-acceptance/`. BN-005
kill/restart last-known-state semantics preserved: recorded
disconnects replay as gaps, `conn:*` without a disconnect replays as
live last-known (documented limitation).

BN-021c independent PR review + hardening (11 November): a second
review pass over PR #2 (`d19394c`) confirmed and fixed three defect
classes. P0-1: concurrent same-claim connections could merge into one
`exp:*` speaker — the listener now admits at most ONE live connection
per resolved scope; additional simultaneous same-claim connections are
`conn:*`-isolated with attribution preserved. Sequential same-claim
rejoins and claim-matched `device_id` attribution remain spoofable —
P0 production attribution gate stays OPEN pending authenticated
ingress (BN-021f). P0-2: `record_transport_disconnect_for_key`
ignored journal append failures and still retired `conn:*` state —
now returns `TransportDisconnectOutcome` (Stale/NoActiveSession/
Recorded/PersistenceFailed), appends durably BEFORE mutating memory,
and retirement is gated on durable success; failed persistence is
logged and leaves live state identical to replay. P0-3: a connection
that sends BMP Termination no longer accepts further frames; a clean
Termination + EOF no longer leaves a meaningless transport gap
(`NoActiveSession`). P1-2: `exp:{scope}:{canonical}` components are
percent-encoded (separator injection impossible); FNV-64 speaker
partitioning documented honestly as a hash, not the scope string.
`tests/bn021c_exporter_isolation.rs` is now 17 tests. NAT Scenario F
reclassified FAIL/OPEN in the acceptance README; `runtime/` journal
artifacts confirmed local-only (gitignored), `up.sh` now derives paths
and refuses silent lab destruction without `BN021C_RESET=1`. Review
evidence: `docs/merger/evidence/2026-11-11-bn021c-pr2-review/`.
Merge recommendation: Outcome B — mergeable as restricted partial
capability with explicit acceptance of the same-claim limitation.

BN-021f authenticated BMP ingress (11 November): the BN-021c P0
identity-attribution gate is now closed for the default deployment
mode. `bonsai-routing` can terminate mutual TLS on the BMP listener
(`bmp_mtls`; env `BONSAI_ROUTING_BMP_TLS_{CERT,KEY,CLIENT_CA}`); the
verified principal is the SHA-256 of the client-certificate DER,
produced only by the TLS layer — never from BMP payload fields.
Trusted bindings gained `match_ingress_credential` (schema v4,
additive migration); `IdentityResolver::resolve_verified_ingress`
authorizes only credential-pinned bindings, with the existing
evidence/contradiction rules. Verified resolutions get the
provenance-fenced `vexp:*` scope namespace — legacy claim-based `exp:*`
journal records stay readable but can never be joined by new
connections, and replay never upgrades unverified provenance.
`BmpIdentityMode::Verified` is the default; plaintext/unauthenticated
or unbound-credential connections always isolate into `conn:*` with
`device_id = None` (also fixing the BN-021c defect where an isolated
same-claim sibling retained `device_id`). `ClaimCompatible` remains an
explicit, unsafe opt-in. Live NAT lab (`lab/acceptance/bn021c-nat`)
now runs FRR → per-exporter mTLS shim → shared SNAT source → Bonsai:
sequential same-claim impersonation (the exact BN-021c P0 repro) is
rejected, legitimate reconnects keep canonical identity, SIGKILL
replay preserves verified provenance. Verified: bn021f suite 9/9,
bn021c 17/17, bn021b 13/13, bn005_closeout 11/11, rbmp-rib/rbmp-store,
bn006, r01_r04, r05_r06, bn022 — 0 failures; clippy warnings only
plus one pre-existing `never_loop` error in untouched
`src/http_server/device.rs` (reproduced on baseline). Remaining:
production exporter mTLS story; simultaneous old+new credential
authorization during rotation (sequential re-pin documented).
Evidence:
`docs/merger/evidence/2026-11-11-bn021f-authenticated-bmp/`.

BN-021f hardening pass (11 November): the four remaining security/
lifecycle findings from the PR #2 review are resolved. TLS
handshakes no longer serialize the BMP accept loop — they run in
per-connection tasks bounded by a semaphore (default 64 concurrent,
10 s timeout, excess dropped, shutdown-aware). An active-session
registry plus periodic batched binding revalidation
(`BONSAI_ROUTING_BMP_RECHECK_INTERVAL_MS`, default 5000) revokes live
sessions when a binding/credential/device mapping changes: ingestion
stops, the transport closes through the durable disconnect path with
a `bmp_authorization_revoked` gap, last-known routes are preserved,
and unauthorized reconnects isolate to `conn:*`. Multiple sessions
sharing one verified principal are all invalidated by one revocation.
Credential rotation is a documented sequential re-pin that preserves
canonical exporter/device identity and `vexp:*` scope; simultaneous
overlap authorization is deferred (schema v5 candidate). Production
deployment policy now refuses non-loopback plaintext BMP and
claim-compatible mode unless explicit dev overrides are set, and
fully validates mTLS material at startup; compose exposes the TLS env
contract. `VerifiedIngressPrincipal` can no longer be minted outside
the transport layer — private fields, `pub(crate)` attestation
constructor, and deserialization always yields an unattested value
that fails closed. Live NAT R1–R5 verified revocation, multi-session
revocation, rotation, TLS saturation/recovery, and SIGKILL
restart/replay. Verified: bn021f_hardening 9/9,
bn021f_authenticated_ingress 9/9, all prior regression suites 0
failures. Evidence:
`docs/merger/evidence/2026-11-11-bn021f-revocation-hardening/`.

BN-021d NetFlow/IPFIX identity isolation (11 November): the flow
receiver now separates observed transport evidence from provisional
exporter identity and verified ingress. `TelemetryUpdate.target`, the
bus event and the AppFlow node id all carry a collision-resistant
`flowsrc/{collector}/{proto}/{ip}:{port}/{sid|odid}{id}` key, so
NAT-shared exporters no longer collapse downstream; `exporter_address`
remains as bare-IP evidence. The IPFIX template parser is strict
(per-specifier bounds, enterprise PEN preserved, varlen fields marked
non-decodable, RFC 7011 message-length/padding validation) and the
template cache is bounded on every dimension (contexts, domains,
templates, fields, total count, bytes) with TTL expiry, idle eviction,
v9/IPFIX restart clearing and source-port-change isolation; UDP template
withdrawals are ignored per RFC 7011 §8.4. Packet handling is
transactional: every flowset/set boundary is validated before any cache
mutation, so a rejected packet cannot alter template or restart state.
Independent-review hardening: NetFlow graph projection fails closed
for every flow update — no `DeviceAddress` freshness updates, no
`CARRIES_FLOW`, and no `SRC_HOST`/`DST_HOST` endpoint edges — because
no authenticated flow ingress exists yet and even a serialized
`identity_resolution` is self-asserted msgpack evidence, not proof of
a transport-attested resolver; flow-derived utilization/DDoS device
alerts are disabled for the same reason. The AppFlow id is the full
5-tuple including `src_port`. Plain UDP flows stay `provisional` with
`device_id = null` — a matching operator binding or transport address
can never confer canonical device identity. `resolve_flow_identity` is
the extension point for a future authenticated flow ingress. Verified: bn021d suite
15/15 + 3 unit tests, all BN-005/BN-021b/c/f and routing regressions
0 failures, and a
live docker NAT lab (3 exporters SNAT'd to one source) producing 7
distinct provisional AppFlow identities with zero Device attribution
and clean malformed-packet rejection. Evidence:
`docs/merger/evidence/2026-11-11-bn021d-flow-exporter-isolation/`;
lab `lab/acceptance/bn021d-flow-nat/`.

BN-021e sFlow identity isolation (11 November, first adapter slice):
the sFlow receiver no longer treats the self-reported `agent_address`
as exporter identity. Every datagram now yields a provisional
`sflowsrc/{collector}/{transport}/{agent}/{sub}` source key covering
the observed UDP endpoint (IP + source port), collector namespace,
agent claim and sub-agent id; `agent_address`, `agent_sub_id`,
`sflow_sequence` and `sflow_uptime` are preserved as self-reported
evidence and `exporter_address` is the observed transport IP.
`sFlowConfig.collector_id` added (default `default`). The graph writer
fails closed exactly like NetFlow: AppFlow (full 5-tuple id) +
source-scoped `app_flow_event`/`sflow_counters_event` only — no
`DeviceAddress` refresh, `CARRIES_FLOW`, `SRC_HOST`/`DST_HOST`,
canonical `Interface` counter mutation, or flow-derived device alerts
(the now-unused utilization/DDoS emitters were removed). Plain UDP
sFlow stays `provisional` with `device_id = null`; canonical
attribution awaits a transport-attested ingress path (BN-021g).
Verified: `tests/bn021e_sflow_exporter_isolation.rs` 7/7 + 2 unit
tests. Syslog/SNMP/OTLP adapters remain open under BN-021e.

BN-022 containment slice (10 November): ad-hoc graph reads are now
denied-by-default for non-admin principals. `src/graph/explorer.rs`
gained `GraphReadAccess` + `validate_query_for_access`: a positive
node-label allowlist (all schema tables except `ConfigItem`,
`ConfigSnapshot`, `ConfigChange`) enforced before execution; unlabeled
node patterns rejected unless they are bound-variable references;
UNION resets binding scope. Wired into MCP `query_graph` (role-aware),
HTTP Explorer (role extension; missing role ⇒ Standard), NL
`run_cypher` (always Standard — model output is untrusted), and
`verify_graph_state` proposal queries. `/api/config-items` GET filters
and POST rejects credential classes (`user`, `session`, `api_key`,
`jwt_secret`, `jwt_revoked`, `cert_config`) below Admin;
`/api/db/backup` is now Admin-only. Verified: 5 new integration tests
(`tests/bn022_graph_read_boundary.rs`, synthetic markers only), 8 new
explorer/nl_query unit tests, bn005 suites 24/24 — all pass. Storage
migration (JWT secret/verifiers out of the graph) remains open.

BN-021e syslog identity isolation (11 November, second adapter slice):
the syslog receiver no longer treats a message's claimed hostname as
device identity. Every frame now yields a provisional
`syslogsrc/{collector}/{transport}/{peer}[/conn{id}]` source key —
UDP sender endpoint, TCP connection peer plus per-connection session
scope, and collector namespace (`SyslogConfig.collector_id`, default
`default`). The hostname→target match is demoted to an explicit
candidate hypothesis: exactly one configured hostname match →
`unique_hostname_match`; duplicates across sites/tenants →
`ambiguous_hostname_match` with all candidates preserved (no arbitrary
selection, and ambiguous claims no longer steer vendor pattern
selection); none → `unresolved`. Relay vs originator stays separated:
the source key identifies the sender while `reported_hostname`,
`peer_addr`, `transport` and raw bytes remain evidence. The graph
projection fails closed for every syslog update: provisional
`StateChangeEvent`s scoped to the source key (no `REPORTED_BY`, no
`Device` upsert) plus read-only candidate probes against
BgpNeighbor/Interface/BfdSession/OspfNeighbor/IsisAdjacency state.
Fact events are now `syslog_fact_candidate`/`syslog_fact_unresolved`
(the mutating joiners were removed); forged serialized
`identity_resolution`/`device_id`/`verified_ingress` fields cannot
authorize anything. Category events keep `syslog_{category}` names
scoped to the source key. Verified:
`tests/bn021e_syslog_source_identity.rs` 10/10 + 13 lib tests incl.
the 44-fixture corpus. SNMP/OTLP adapters remain open under BN-021e;
canonical attribution awaits BN-021g verified ingress.

BN-021e SNMP trap identity isolation (11 November, third adapter
slice): the SNMP trap receiver no longer treats the transport source
IP as device identity. Every accepted trap now yields a provisional
`snmpsrc/{collector}/udp/{peer_ip}:{peer_port}` source key covering
the observed UDP endpoint and collector namespace
(`SnmpConfig.collector_id`, default `default`). The configured polling
target match is demoted to an explicit candidate hypothesis:
`unique_target_match` / `ambiguous_target_match` (duplicate IPs across
tenants — no arbitrary selection, no vendor steering) /
`unresolved`. All self-reported claims stay separate evidence:
v1 `agent-addr` → `reported_agent_address`, v3 USM engine/context
engine IDs → `reported_engine_id`, `usm_security_name` with
`usm_auth_status: "unverified"`, community presence only
(`community_present`; the string itself and `raw_hex` are redacted
from published values and retained solely in the restricted forensic
archive, which now flushes before publish like the syslog fix).
SNMPv3 message authentication is NOT verified — a recognized security
name is not authentication; encrypted scoped PDUs are rejected with an
explicit parse error. Community/USM filtering now runs BEFORE dedup
(re-keyed to `(trust domain, source key, trap OID)`, bounded), so a
rejected packet cannot suppress a later accepted observation; a
configured allowlist rejects empty/missing/malformed communities.
The graph projection fails closed: provisional `StateChangeEvent`s
scoped to the source key plus read-only candidate probes
(Interface/BgpNeighbor/OspfNeighbor/IsisAdjacency) — no `Device`
upsert, `DeviceAddress` refresh, `REPORTED_BY`, or canonical
Interface/BGP/BFD/ISIS/OSPF/service mutation from trap claims, and no
detection/remediation under trap-claimed identity. Fact events are
now `snmp_fact_candidate`/`snmp_fact_unresolved` (the mutating
`join_snmp_fact` was removed); `correlation_buffer` accepts the new
type names so SNMP facts keep contributing correlation keys. Forged
serialized `identity_resolution`/`device_id`/`verified_ingress`
fields cannot authorize anything. Verified:
`tests/bn021e_snmp_source_identity.rs` 11/11 + lib SNMP parser/target
tests + BN-021/022 regression batch. OTLP remains the open BN-021e
adapter; canonical SNMP attribution awaits BN-021g verified ingress.

BN-021e OTLP source identity isolation (12 November, fourth adapter
slice — completes the BN-021e unauthenticated-adapter set): the OTLP
HTTP/JSON receiver no longer treats `service.name`, `host.name` or
`peer.address` as identity. Every observation is keyed to a
provisional `otlpsrc/{collector}/http/{peer_ip}:{peer_port}` source
identity derived from the observed TCP peer (`ConnectInfo`) —
`X-Forwarded-For`, forwarding headers and payload fields are never
consulted for identity. Reported resource/span attributes are
preserved as evidence; a consistent source-scoped
`otlpapp/{source_key}/{service}` provisional id replaces the old
inconsistent `app:{service}` / `{peer}:{service}` schemes, and no
canonical Application node is created per unverified service name.
The graph projection fails closed: provisional
`StateChangeEvent`s (`otlp_span_event`/`otlp_metric_event`) scoped to
the source key plus read-only exact-match candidate probes against
Device/HostEndpoint — the `address STARTS WITH` prefix joins,
`RUNS_SERVICE`/`HOST_RUNS_SERVICE`/`APP_IMPACTED_BY_NETWORK` edges
and Application health-column writes (cpu_pct, memory_mb,
req_per_sec, error_rate, metric_json) from untrusted claims are
removed. Ingestion budgets bound body size (axum default 2 MiB),
resource groups (32), scopes (32), items per request (4096) and per
scope (1024), attributes scanned (256), string fields (512 chars)
and timestamps (non-positive/>+24h fall back to receive time);
non-finite metric values are dropped, never coerced; drops are
counted via `bonsai_otlp_items_dropped_total`. Evidence is durable
through the parquet archiver and graph `StateChangeEvent`s. OTLP
canonical Application metrics therefore no longer update from
unverified producers — a deliberate coverage change pending
BN-021g verified ingress; HTTP/JSON only (no gRPC claimed).
Verified: `tests/bn021e_otlp_source_identity.rs` 8/8 + BN-021/022
regression batch. All BN-021e unauthenticated adapters (NetFlow/
IPFIX, sFlow, syslog, SNMP, OTLP) are now isolated; BN-021g
production acceptance is next.

PR #11 review resolution + merge (12 November): independent review of
the OTLP slice confirmed and fixed four findings before merge.
B1 — strict numeric parsing: absent/malformed/non-finite
`asDouble`/`asInt`, missing histogram `count` and unsupported value
shapes are rejected and counted (`partialSuccess.rejectedDataPoints`),
never fabricated into `0.0`; a genuine reported `0` stays valid, and
histograms preserve cumulative count/sum/min/max/bucket evidence
without synthesizing mean/rate/rps. B2 — honest HTTP acceptance:
structurally malformed requests get 400, partial acceptance reports
exact per-item `rejectedSpans`/`rejectedDataPoints`, legitimately
empty requests stay 200; drop accounting is per item, not per
truncation event. B3 — `probe_otlp_candidate` dropped `LIMIT 1`:
bounded multi-match collection reports `unresolved`/`ambiguous`/
`candidate` explicitly, always `authority: "unverified"`,
`scope: "unscoped"`. B4 — verified provisional `StateChangeEvent`s
are durable (Kuzu) and bounded by request budgets plus existing
age/count retention; no new storage. Verified: OTLP suite 14/14
(incl. new `candidate_probe_reports_ambiguity`), BN-021/022
regression batch 109/109, `cargo test --lib` 290 pass / 5 fail —
the same 5 pre-existing baseline failures (archive overflow,
site_sync, subscription_status, registry, service_discovery),
tracked under BN-019-B6. Merged as `9b80cd4`.

BN-021g-1 collector trust foundation (12 November): the collector→core
gRPC channel now authenticates and authorizes the FORWARDING
collector without granting it authority over downstream observations.
New `src/collector_auth.rs`: the peer principal is extracted from
tonic `TlsConnectInfo` peer certificates (populated only after the
TLS handshake verified the client cert against the configured client
CA — payload and metadata are never consulted) and hashed as
`sha256:<hex>` of the DER leaf, the same convention as BN-021f.
Authorization reuses `exporter_bindings`: an ACTIVE binding with
`protocol="collector"`, `exporter_id` = claimed `collector_id` and
`match_ingress_credential` = the connection fingerprint must exist —
a valid certificate cannot claim another collector's id, an unbound
CA-issued cert gains nothing, and revocation/rotation take effect
within a bounded 5s decision-cache window (4096-entry cap). Every
relevant RPC is gated by capability: ingest plane per claimed
`collector_id` (telemetry/detection streams, register, heartbeat),
`core-admin` for device/site/credential mutation, query and
remediation writes, `sidecar` for sidecar registration, and
any-active-pinned-binding for the read plane; `get_leader` stays
reachable for provisioned collectors. Durable provenance is recorded
once per (credential, collector) per stream in `audit_log`.
Plaintext deployments remain the explicit legacy dev mode. The
authorized collector's payload claims remain provisional evidence —
this slice changes nothing downstream (BN-021g-2+ owns verified
original-source projection). `server_startup` now passes the shared
registry SQLite store into the gRPC service (previously `None`, which
would have left authz without bindings). Verified: real loopback mTLS
acceptance `tests/bn021g_collector_trust_foundation.rs` 6/6 —
authorized ingest/heartbeat/register/get_leader, collector-id
impersonation, unbound certificate, forged serialized principal,
active revocation, credential rotation and replayed-record denial.
Evidence: `docs/merger/evidence/2026-11-12-bn021g-collector-trust-foundation/`.

BN-021g-1 review hardening (12 November, same branch): independent
review found the first slice authorized the CONNECTION but not the
OBSERVATION — an authenticated collector could submit canonical-path
telemetry for any target, forge inventory metadata, submit arbitrary
DetectionEvents, read global topology, and keep revoked assignment
streams alive. All findings G1–G7 are now closed:

- **G1 observation boundary** — `telemetry_ingest` authorizes each
  update: provisional paths (`streaming/`, `signals/`) stay
  provisional; canonical paths require the target to be an enabled
  registry device assigned to the authenticated collector, and
  vendor/hostname/role/site are rewritten from authoritative inventory.
  Relabeling a flow/OTLP payload onto a YANG path cannot bypass the
  assigned-device check.
- **G2 detection authority** — `DetectionIngest` requires the separate
  `detection-ingest` capability plus verification that the target is
  assigned and every `source_event_ids` reference exists, belongs to
  the same canonical device, and is not provisional evidence. Remote
  `auto_remediate`/`remediation_action` fields are ignored.
- **G3 RPC matrix** — explicit capabilities: `collector` (ingest,
  heartbeat, register stream), `detection-ingest`, `core-admin`
  (mutations, `query`, remediation writes), `sidecar`, `operator-read`
  (global reads incl. topology/sites/event stream); device-scoped
  reads return only assigned devices; `get_leader` needs any pinned
  binding. Exporter-protocol bindings grant no core RPC access.
- **G4 stream fencing** — assignment and event streams revalidate
  authorization per delivery and periodically while idle (revoked
  credentials stop receiving device usernames/passwords within ~5s);
  collector sessions are generation-fenced so a stale stream cannot
  unregister or displace a reconnect.
- **G5 auth policy** — `runtime.auth_mode` = `required` (default;
  missing/unattested principal fails closed) or `development-compat`
  (explicit opt-in, loud startup warning); unknown values fail config
  parsing.
- **G6 grant hardening** — every authorization path requires a
  transport-attested principal; ambiguous bindings fail closed.
- **G7 provenance** — registration audit records the real certificate
  fingerprint; audit-write failures are logged and metric'd rather
  than claimed durable; cache stays bounded (4096, 5s TTL) with
  indexed lookups per miss.

Trust contract documented: assignment authorizes the collector's
acquisition process for the configured device — it is NOT cryptographic
proof of original device identity (BN-021g-2+). Revocation cannot
recall credentials already delivered over a stream; rotate device
credentials when revoking a collector. Verified:
`tests/bn021g_collector_trust_foundation.rs` 19/19 (real mTLS + real
GraphStore graph-writer wiring — cross-collector/unknown-target/
forged-metadata denial, provisional-flow isolation, detection
provenance denial, capability matrix, active-stream revocation,
generation fencing, auth-mode enforcement). `cargo test --lib`
290 pass / 5 fail = unchanged BN-019-B6 baseline. Evidence:
`docs/merger/evidence/2026-11-12-bn021g-collector-trust-foundation/`.

BN-021g-1 final hardening (12 November, same branch): a second
independent review confirmed six residual defects (H1–H6), all now
closed on the same architecture:

- **H1 classified authority** — the `streaming/`/`signals/` path-prefix
  bypass is gone. Every record is decoded, classified
  (`TelemetryEvent::authority`: CanonicalDevice / Provisional /
  Ignored) and authorized by class: canonical writers reachable through
  `streaming/` paths (BMP peer-state/route-monitoring/initiation/
  statistics, BGP-LS node/link/policy, interface summaries,
  config-state leaves) now require the same enabled+assigned-device
  check as YANG paths; provisional classes require a `*src/` source
  key whose collector namespace is rewritten to the authenticated
  collector id (cross-collector evidence-key forgery impossible);
  unclassified paths are ignored. Serialized BMP/BGP-LS payloads via
  generic ingest cannot acquire original-receiver authority.
- **H2 queue acks** — `TelemetryIngestResponse.rejected` added; the
  core consumes the whole stream before responding so
  accepted+rejected==submitted and the collector retires the whole
  batch. Permanent rejections (authz denial, malformed payload,
  protocol major skew) never replay; RPC errors retain everything.
  Verified with the real `drain_queue_to_core` + real core server:
  interleaved rejection, fully-rejected batch, retryable core outage.
- **H3 per-message capability** — `detection-ingest` is revalidated on
  every message of an open stream; authz decisions are stamped with a
  process-local epoch bumped on every binding upsert/revoke, so
  revocation is visible on the next check (no TTL wait) — verified by
  revoking only the detection binding mid-stream.
- **H4 detection provenance** — remote detections now require
  non-empty source-event references that exist, belong to the
  authorized device, and carry server-owned non-provisional
  `source_type`; malformed JSON permanently rejected; stored
  `source_types` marked `remote_detection_ingest`; graph-write
  failures abort as retryable errors.
- **H5 read ambiguity** — `collector_ids_for_credential` no longer
  collapses with `DISTINCT`; scoped reads apply the same ambiguity
  rules as ingest; the store rejects a second collector identity per
  credential.
- **H6 session lifecycle** — assignment-stream cleanup is owned by a
  drop guard (fires on disconnect/cancellation, even before first
  poll); generation check + all cleanup run under one
  `active_collectors` lock so stale cleanup cannot touch a newer
  session. Worst-case idle-stream revocation bound ≈5s poll interval.

Verified: `tests/bn021g_collector_trust_foundation.rs` **25/25** +
3 real-server queue-drain unit tests + focused lib modules 15/15.
Evidence: `docs/merger/evidence/2026-11-12-bn021g-collector-trust-foundation/`.

BN-021g-1 residual round (13 November, same branch): a third
independent review confirmed four residual defects (R1–R4), all now
closed:

- **R1 provisional normalization** — `authorize_provisional_source`
  unconditionally pins effective `source_key`/`exporter_id` to the
  authenticated collector namespace and classified protocol prefix;
  `identity_status`/`authority`/canonical fields are forced
  provisional/unverified regardless of producer claims; missing, null,
  numeric or malformed identity fields can no longer trigger
  `classify()`'s untrusted `exporter_address` fallback; `otlpapp/`
  provisional ids are re-derived under the normalized key; every
  original producer claim is preserved under `claimed_*` evidence
  keys.
- **R2 detection provenance** — `verify_detection_scope` is now a
  positive allowlist of server-established `source_type` values with
  structured detail parsing (missing/null/empty/unknown provenance
  and malformed details permanently rejected; store failures
  retryable). Correction: the earlier
  `sce-no-prov` test never actually submitted a referencing
  detection — it does now, along with unknown/provisional/missing/
  cross-device/malformed/db-failure cases.
- **R3 queue ack contract** — `drain_queue_to_core` refuses to ack
  inconsistent or impossible `accepted + rejected` accounting
  (checked arithmetic) and major-skewed response protocol versions;
  violations retain the batch for replay. Core verifies
  `protocol_version` on every record — major skew is a permanent
  per-record rejection even mid-stream. `InProcessBus::publish`
  returns `PublishOutcome`; router-queue saturation surfaces as a
  retryable drain failure instead of a silent drop.
- **R4 global collector uniqueness** — schema v5 partial unique
  indexes make active `protocol="collector"` `exporter_id` and
  `match_ingress_credential` globally unique; provisioning rejects
  cross-scope/cross-credential conflicts (concurrent provisioning
  serialized, one winner); authorization fails closed on
  pre-existing ambiguous rows for every claimant; rotation keeps a
  single `binding_id`. Authz decisions are epoch-stamped with the
  epoch captured before the store read and the epoch bumps before
  audit persistence, so a pre-revocation lookup cannot cache a stale
  grant and an audit failure cannot strand one. Indexed credential
  lookup proven via `EXPLAIN QUERY PLAN`.

Verified: `tests/bn021g_collector_trust_foundation.rs` **31/31** +
16/16 ingest drain tests + lib 299 pass / 5 documented BN-019-B6
baseline failures + 110/110 BN-021b–f / BN-022 / syslog / snmp /
otlp / sflow regressions. Revocation guarantee is process-local
(epoch) with cross-process changes bounded by the 5s TTL — recorded
under BN-021g-6 production acceptance. Evidence:
`docs/merger/evidence/2026-11-12-bn021g-collector-trust-foundation/`.

BN-015-U2 site-hierarchy preservation (13 November,
`fix/bn015-u2-preserve-site-hierarchy`): `save_managed_device` →
`sync_sites_from_targets` previously built a blank root `SiteRecord`
(name-derived id, empty parent, `kind="unknown"`, zeroed
coords/metadata) and ran it through `upsert_site_record`, whose
`ON MATCH SET` overwrote every field and deleted all `PARENT_OF`
edges — any device save flattened Region→DC→Building→Rack
hierarchies and could merge distinct same-named sites. Device saves
now resolve the free-text `site` claim via `ensure_target_site`:
name-derived id match, else UNIQUE exact-name match (explicit-id and
imported sites), else create-only insert (same shape as the NetBox
backfill — `ON MATCH` touches nothing). Ambiguous display names
produce no link and no duplicate; only the device's `LOCATED_AT`
association is written. Site admin `upsert_site` +
`validate_site_hierarchy` are unchanged and retain full authority.
Verified: 6 new `graph::tests::site_sync_*` regressions + the stale
`site_sync_creates_site_and_located_at_edge` baseline failure fixed
(queried port-full address instead of canonical `Device.address`);
lib 305 pass / 4 documented BN-019-B6 baseline failures (was 5).
Residual: duplicate display names across branches still cannot be
distinguished by a free-text device.site claim — canonical site_id
migration remains BN-015-U3. Evidence:
`docs/merger/evidence/2026-11-13-bn015-u2-site-hierarchy/`.

BN-016-U1 BGP false-edge protection (13 November,
`fix/bn016-u1-bgp-session-identity`): the topology API derived
`peer_device` by scanning every `BGP_SESSION_WITH` edge out of a
device and accepting any onboarded destination; the shared
`resolve_peer_to_device` returned the first `LIMIT 1` row across three
evidence tiers; edges were keyed only by `{source}` per device pair so
multiple sessions overwrote each other and stale resolutions stayed
authoritative forever; BMP's global-session id collided with the gNMI
`{device}:{peer}` `BgpNeighbor` id; and `Topology.svelte` re-resolved
peers client-side. Now `resolve_peer_typed` collects ALL evidence
(Device.address / DeviceAddress→KNOWN_ADDRESS_OF /
EntityIdentity.mgmt_ip) and returns `Resolved` only on a single
consistent claimant — `Unresolved`/`Ambiguous`/`Conflicted` are
explicit non-results. `BGP_SESSION_WITH` edges are keyed per session
`(source, peer_address)`, created only on unique resolution and
transactionally reconciled on every observation; `BgpNeighbor` carries
`source`, `resolved_device`, `resolution_status`, `resolution_reason`
with BMP sessions under `bmp:`-prefixed ids. API exposes
`peer_device`/`source`/`resolution`/`reason`; the UI draws only
backend-resolved peers and badges unresolved/ambiguous/conflict.
Verified: 7 new `graph::tests::bgp_session_*` regressions + graph::
suite 87 pass / 1 documented BN-019-B6 baseline failure. Residual:
globally address-keyed schema fails closed on overlapping private
peer addresses (no cross-tenant isolation claim); gNMI path carries
no VRF discriminator. Evidence:
`docs/merger/evidence/2026-11-13-bn016-u1-bgp-session-identity/`.

BN-016-U1 Phase B P0 closure (13 November, merged PR #14 at
`25ce360e86b79a5d7d0478297bcd9a9e5a833a90`): the remaining correctness
defects are fixed. `reconcile_bgp_session_edge` opened a nested
transaction inside `write_batch`'s active transaction — lbug rejects
nested write transactions, so every BGP/BMP write inside production
batches failed; one transaction owner per write op now, with
dead-transaction tracking and per-update fallback retry (batching
stays enabled, failures surfaced). `BGP_SESSION_WITH` is keyed by
`session_id` plus `peer_type`/`route_distinguisher`/
`network_instance`/`source`; gNMI `network-instance[name=X]` joins
session identity for non-default instances. `resolve_peer_typed` fails
closed with an explicit `Unavailable` outcome when evidence tiers
cannot be examined. Ownership mutations re-resolve affected edges
immediately; a startup migration re-keys or invalidates legacy unkeyed
edges; schema ALTERs distinguish already-present columns from real
failures. Effective persisted peer-as is projected; duplicate BMP
reconcile removed; topology resolution cached per peer per request.
Verified: `graph::tests::bgp_session` 18/18, `graph::` 98 pass / 1
documented BN-019-B6 baseline failure, `bn016_u1_topology_browser`
1/1 (real Chromium, canonical router), ui build clean.

BN-015-U1 NetBox inventory-only onboarding (13 November,
`fix/bn015-u1-netbox-inventory-onboarding`): the NetBox import UI posted
`selected_paths: []` to the subscription-oriented
`/api/onboarding/devices/with_paths` (always rejected) while silently
binding `credentials[0]` and defaulting unknown roles to `leaf`.
New `POST /api/onboarding/inventory/import` registers declared assets
only (`enabled=false`, `inventory_only=true`, stable
`external_id=netbox:device:<id>`, no credentials, no paths) — the
subscriber manager never activates collection. Per-row outcomes cover
imported/already-imported/updated/conflict/invalid/failed; identity
conflicts never overwrite existing devices or re-address imported
objects. Preview returns NetBox object IDs and explicit eligibility;
no-IP candidates are surfaced, not dropped. sqlite schema v6 persists
the new fields plus previously-dropped selected_paths/extra_ips.
Verified: 3 managed_devices tests incl. restart persistence, ui build.
Evidence: `docs/merger/evidence/2026-11-13-bn015-u1-netbox-inventory-onboarding/`.

BN-015-U1 P0 review hardening + merge (13 November, PR #15 merged at
`e5373f0`): external identities are now instance-scoped
(`netbox:{scheme}:{host}:{port}:device:{id}` — credentials/tokens never
participate) with persistence-boundary uniqueness via partial unique
index `idx_devices_external_id` (schema v7); ambiguous identities and
canonical-address conflicts (host vs host:port, IPv6) fail closed.
Registry mutations are transactional — memory/SQLite/audit commit or
roll back together, errors propagate, and notifications publish only
after durable commit. Graph projection is reported separately as
`synced`/`pending` and reconciled by unchanged re-imports.
Inventory-only assets are skipped by subscriber startup, seed-plan
merge and collector assignment even if `enabled` is set; activation
requires an explicit validated collection plan. Re-imports refresh
importer-owned declared metadata only — operator configuration is
preserved and drift surfaces as conflict. sqlite v6/v7 migrations
distinguish duplicate-column partial runs from real failures and
verify columns before advancing `user_version` (fail closed on
duplicate external ids). UI selects by `external_id`, disables
ineligible candidates and shows per-object results.
Verified: lib suite 355 pass / 3 documented BN-019-B6 baseline
failures (`api_registry_persists` baseline now passes); http_server
24/24 incl. authenticated-router RBAC; sqlite_store 6/6 migration
tests; `bn015_u1_inventory_browser` Playwright/Chromium acceptance
against the real router + mocked paginated NetBox (duplicate-address
candidates, ineligible rows, per-object conflict, inventory-only
badge). U3/U4/U6 remain open.

BN-019-B6 fail-closed CI (13 November, branch
`fix/bn019-b6-fail-closed-ci`): canonical tiered gates landed in
`scripts/ci/gates.sh` (L0 source / L1 compile / L2 required behavioral /
L3 cross-component / L4 lab-manual), wrapped by `just gate` and consumed
by the new `.github/workflows/ci.yml`. Makefile false-green paths
(`|| cargo test` runner fallback, `|| echo` on integration failures,
`|| true` on pytest, fake UI-smoke success) removed. Baseline failures
root-caused and fixed: archive test i64 overflow, subscription-status
edge keyed on raw host:port vs port-stripped Device node (real defect),
service-pattern space-separated descriptions. Lib suite now 363/363.
Rust gates job requires a self-hosted `bonsai-ci` runner (>100 GB
target/, >16 GB link RAM); hosted jobs cover L0/python/UI. CI configured
but not yet executed end-to-end — no self-hosted runner registered.
Evidence: [BN-019-B6](merger/evidence/2026-11-13-bn019-b6-fail-closed-ci/README.md).

Secure-core milestone progress (14 November, stacked branches):

- **WP1 CI closure (PR #16, `fix/bn019-b6-fail-closed-ci` @ 27cfa85):**
  CI-R1–R6 implemented — `rust-gates` now `needs: ui-build`; all actions
  pinned to commit SHAs; `gates.sh` emits distinct PASS/FAIL/
  NOT_EXECUTED outcomes (l4 and unknown tiers exit nonzero); `make
  test-ui` executes the real Playwright gate; `make test-all` runs the
  actual suites; L0 rustfmt distinguishes new drift from pre-existing
  base drift; L1/L3 resolve `.venv/bin/python` and validate
  pytest/playwright; explicit L2 (22) / L3 (13) target manifests
  verified before cargo runs. GitHub run 43515987209 was blocked by an
  account billing lock before any job started — hosted-job failures are
  infrastructure, not test results; Rust gates still await an isolated
  `bonsai-ci` runner.

  15 November update — **owner-directed local-first CI policy**: zero
  paid remote CI consumption; GitHub remains source-control/review only.
  `.github/workflows/ci.yml` converted to manual `workflow_dispatch`
  only (all job definitions preserved, no automatic triggers remain);
  the canonical acceptance command is `scripts/ci/run-local.sh
  --trusted --shared-target l0 l1 l2 l3` inside the pinned
  `bonsai-ci:local` image. Remote CI activation is deferred to a future
  project-maturity workstream. No branch protection or rulesets are
  configured, so no required-check adjustment blocks promotion; remote
  CI is never reported as passed.
- **WP2/WP3 BN-022 protected auth storage (PR #17,
  `feat/bn022-auth-storage` @ 409aaba, stacked on PR #16):** new
  `AuthStore` (`src/auth_store.rs`) owns users, API-key verifiers,
  JWT signing keys (age-encrypted CSPRNG, 0600 `bonsai_auth.db`),
  revocations, config and audit — credential material leaves the
  queryable graph. Staged idempotent migration imports legacy records
  then disables+scrubs graph rows to tombstones before the completion
  marker; bounded 26h JWT rotation overlap; transactional API-key
  rotation preserving disabled state; fail-closed revocation lookups.
  Verified: 8 auth_store unit + 371 lib + 5 `bn022_auth_storage`
  integration tests (real LadybugDB + SQLite). Design contract:
  `docs/auth_storage.md`. **Not merged — pending independent review.**
- **WP4 northbound hardening (same PR #17):** `/api/config-items`
  rejects retired credential classes with 410 (auth material managed
  only via `/api/auth/*`); `Secure` cookie attribute set on
  verified-TLS requests via `TlsConnection` extension. Residual:
  outstanding JWTs of disabled users live until `exp`; auth DB not yet
  in `/api/db/backup` runbook; Swagger UI still loads assets from unpkg
  CDN (page itself is auth-gated).
- **WP5 BN-021g-2 contract (PR #18,
  `docs/bn021g2-otlp-contract` @ 1dda9de):** verified OTLP
  original-source implementation contract + 15-case acceptance matrix
  at `docs/bn021g2-otlp-verified-source-contract.md`. Docs only.
