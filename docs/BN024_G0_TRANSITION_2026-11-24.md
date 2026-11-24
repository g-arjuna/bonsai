# BN-024 G0 transition note — 2026-11-24

Status: **superseded — BN-024 implementation has since landed.** This note
was written at G0 handoff; it is retained for history. Current state:

- **BN-024-00** provenance foundation MERGED (PR #82 `e578595`) and
  verifier hardened on `fix/bn024-parser-quality-and-sidecar-contract`
  (revision-identity checks, path containment, deterministic tree hashes;
  114/114 entries verified against pinned-revision git checkouts).
- **BN-024-01** parser contract MERGED (PR #83 `09c2d60`) and corrected on
  the same branch: structured-first ordering, canonical→backend vendor
  translation through the production `inferred_vendor()` path, sidecar
  identity/outcome validation, bounded native parse, explicit
  `suzieq_native` unsupported, genuine packaged-sidecar validation and a
  real differential oracle (see `docs/bn024/evidence/`).
- **BN-024-02** typed-fact ABI implemented on the same branch
  (`src/typed_facts.rs`, `docs/bn024/typed-facts/`): versioned schema,
  explicit unknown/absent/invalid states, trust/authority boundary,
  lifecycle/withdrawal, identity+evidence digests, schema-evolution gate.
  No graph writer enabled.
- Baseline: `main` @ `18fa577` + evidence branch
  `bn019/m6-packaged-upgrade` (`f55b4b1`, `c175cdb`).

The G0 requirements and verified-closed inputs below remain accurate as of
their writing; see `docs/STATUS.md` and `BONSAI_BACKLOG.md` for live state.

## What G0 requires (design doc §10)

> G0 — trust/correctness: fix BN-015-U1/U2, BN-016-U1 and required
> BN-008/021/022 boundaries; pin upstream, license and baselines.

## Verified-closed inputs

| G0 input | State | Evidence |
|---|---|---|
| BN-015-U1 NetBox inventory-only import | MERGED (PR #15 `e5373f0`) | backlog row; `bn015_u1_inventory_browser` |
| BN-015-U2 site hierarchy overwrite | MERGED (PR #15) | `graph::tests::site_sync_*` (6 regressions) |
| BN-016-U1 BGP false-edge | MERGED (PR #14 `25ce360`) | `bgp_session_*` regressions; fail-closed resolver |
| BN-022 credential/auth boundary | MERGED + packaged-verified | BN-019 leg A: real `ensure_migrated` on packaged images, tombstones, post-upgrade auth (`docs/merger/evidence/2026-11-24-bn019-m6-upgrade/`) |
| M6 YANG authority (compiled-schema identity, effective-schema authority, selection fencing) | MERGED (PRs #79/#80) + packaged upgrade verified | same evidence; `schema_version_changed` fail-closed observed live |
| Canonical gates on corrected tree | L0–L3 PASS on `a8e2810`/`18fa577` | `docs/build-times/runs.csv` |

## Open G0 items (honest gaps)

- **BN-021 production attribution** — still P0 Partial: two exporters
  behind one NAT must not merge RIB/graph identity; packaged
  production configuration OPEN (host-network lab proof only).
- **BN-008** — unified provenance/temporal graph: P1 Partial; gNMI
  delete/sync and projection-checkpoint defects tracked inside it.
- **L4 canonical lab tier** — never executed for the M6 corrective
  line; BN-019 slice was bounded, not L4.
- **Upstream/license pinning** — SuzieQ reference pin exists in the
  design doc (`75198b5`); a formal license/baseline sign-off record
  for BN-024-00 has not been produced.
- **BN-024-00…03 contracts** — typed fact serialization, capture
  queue placement, extraction DSL subset, first NOS/feature slice,
  policy-semantics depth, secret-redaction policy, in-process vs
  subprocess: all explicitly unresolved in the design doc §11. "No
  unresolved design decision is permission to invent a default."

## Proposed first bounded slice (G0 → G1 entry)

BN-024-00 contract doc only: pin the SuzieQ reference commit +
license disposition, name the first vendor/feature slice from
available fixtures (EOS/FRR interface+BGP+VRF per design §11), and
publish the failing golden-fixture test list — **no normalization
code**. Exit criteria: ADR-level decisions recorded, G0 residual
items (BN-021 NAT attribution, BN-008 projection checkpoint) either
closed or explicitly waived with owner sign-off.

## Explicit non-goals for this transition

- No SuzieQ runtime dependency; no PyATS/TextFSM replacement;
  no graph projection of normalized facts until G0/G1 gates pass.
- No new queue/store/registry infrastructure.
- L4 and full-length soak remain separate authorized scopes.
