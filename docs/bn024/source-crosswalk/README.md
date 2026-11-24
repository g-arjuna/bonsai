# BN-024-00A — SuzieQ upstream engineering crosswalk (first increment)

Pinned source: SuzieQ OSS @ `75198b5585ebef91f0659b6b1ec3c964cc2cd1ad`
(Apache-2.0; provenance manifest `docs/bn024/provenance/upstream-sources.json`,
114/114 entries verified 2026-11-24). Machine-readable per-module map:
`suzieq-modules.v1.json` in this directory.

This is the *first bounded increment* of the crosswalk — it covers the
pipeline skeleton and the services named in the backlog. Per-feature
traces deepen it before each native port; it is not a claim of complete
coverage.

## Architecture in one paragraph

SuzieQ's poller is a per-device service pipeline: `nodes/node.py` detects
NOS+version from `show version`, `config/<service>.yml` selects commands,
TextFSM templates or `normalize` templates per NOS/version, outputs are
merged on service keys, per-NOS `_clean_<nos>_data` functions reshape
records, Avro schemas + `sqobjects` add defaults/query/assertion, and
`get_diff`/`commit_data` handle lifecycle. Version selection uses
`packaging.version` comparisons over ordered defn lists; the first match
wins and `all` is the wildcard — including for unknown versions.

## Findings that shape Bonsai's design

1. **Defaults are claims, not truth.** The `normalize` DSL's `?|default`
   syntax and cleaner fallbacks (BFD `unknown`→`disabled`, Linux interface
   `state unknown`→`up`, Cumulus prefix counters→`0`) silently fabricate
   values. Bonsai's typed ABI must carry `missing_fields`/explicit unknown
   states instead of inheriting these.
2. **SONiC ≠ Linux ≠ Cumulus upstream.** SONiC delegates to Linux *or*
   Cumulus cleaners depending on the service (bgp→linux, interfaces→
   cumulus-style entry cleaner), and neither models ASIC/host namespace
   separation. Bonsai's namespace-aware identity is an intentional
   improvement, not parity.
3. **Protocol flags are lossy upstream.** `routes.py` maps
   `protocol[0]` through `proto_map`, discarding `*`/`%`/`E2` suffixes —
   this explains the single route-protocol differential finding
   (`S*` vs `S`): SuzieQ's own cleaner would reduce both to `static`.
   Bonsai can preserve flags as separate fields.
4. **devconfig is not a config compiler.** Raw text + regex secret filter
   + whole-string `hash()` diff (process-seeded — unstable). Confirms the
   Batfish-inspired seam must be built natively; see Stage I contract.
5. **`eval_expr` exists upstream** (`svcparser.py`) — Bonsai must not port
   arbitrary expression evaluation; closed-form mappings only.
6. **First-match version selection is order-dependent.** `_get_def_for_version`
   returns the first satisfying defn; `all` matches unknown versions.
   Bonsai's `ReleaseConstraint` is equality-based and fails closed on
   unknown releases — a deliberate deviation.

## Per-module status

| Module | Lines | Role | Rust equivalent (planned) |
|---|---|---|---|
| nodes/node.py | 2296 | detection/dispatch/envelope | platform_registry + capture envelope |
| services/service.py | 947 | defn select/merge/clean/diff/commit | BN-024-05 normalizer pipeline |
| services/svcparser.py | 552 | nested-JSON normalize DSL | typed extraction specs (no eval) |
| services/interfaces.py | 1087 | interface state per-NOS | first native slice candidate |
| services/bgp.py | 708 | BGP per-AFI/SAFI expansion | later slice |
| services/routes.py | 414 | protocol/vrf normalization | later slice |
| services/devconfig.py | 62 | raw config + hash diff | BN-024-10..13 config compiler |
| config/*.yml + schema | 19+28 | commands/extraction/schemas | FeatureFlag + typed specs |

## Batfish boundary (BN-024-00A second half)

Batfish OSS is established as the *configuration-intelligence* reference
(SuzieQ = operational "what is it doing"; Batfish = "what should it do
under effective config"). High-level map recorded in
`suzieq-modules.v1.json::batfish_reference`. No port started, no Java
dependency, no pinned revision chosen yet — that is BN-024-11/12 work.
License review required before any reuse.

## Licensing

All studied files are Apache-2.0 at the pinned revision. Classification
per entry in the JSON (`study_and_reimplement`, `study_only`,
`study_only_eval_rejected`). Per-asset `approved_reuse` sign-off for
verbatim fixture/template reuse remains pending under BN-024-00 — nothing
here asserts it.
