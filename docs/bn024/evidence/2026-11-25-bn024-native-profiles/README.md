# BN-024-03/03A correction + BN-024-05 structured FRR slice — validation evidence (2026-11-25)

## Commits

- PR #87 correction: `d897530` on `feat/bn024-native-profiles`
  (fast-forward over published `5035a43`).
- Structured adapter: stacked commit on `feat/bn024-frr-structured-interfaces`
  (see PR for final SHA).

## Lane validation (exact trees, `ci` profile, lbug prebuilt lane)

| Tree | Suite | Result | Wall | Peak RSS |
|---|---|---|---|---|
| pre-split combined | lib 651 + integration 22/22 | PASS | 592.59s | 6.73 GB |
| D1 correction-only (`d897530` content) | lib 636 + integration 22/22 | PASS | 595.38s | 6.88 GB |
| D2 full tree | lib 651/652 first pass (presentation-fixture assertion corrected: fixture semantics right, test wrong) then 13/13 adapter tests green after test-only assertion correction | PASS-after-fix | 585.76s | 7.04 GB |

Rows also in `docs/build-times/runs.csv`. Python schema/provenance/oracle
suites: 37 passed, 7 skipped (`test_bn024_provenance/typed_facts/
parser_contract/oracle`).

## Key behavioral evidence

- `wrong_observation_input_shape_fails_closed`: CLI pair cannot execute
  `interfaces.frr_json`; structured JSON cannot execute `interfaces.frr_cli`.
- `standalone_frr_json_exact_release_dispatch_is_qualified_and_shadow_only`:
  genuine fixture dispatches qualified+shadow-only with exact release,
  namespace `default`, contract `frr_vtysh_show_interface_json`.
- `standalone_frr_json_rejects_wrong_release_and_namespace`: `10.7.0`,
  `asic-0` and missing namespace all fail closed.
- `duplicate_object_keys_and_wrong_known_field_types_are_format_errors`,
  `missing_optional_fields_are_explicit_unknowns_not_failures`,
  `contradictory_explicit_and_numeric_references_never_resolve_to_edges`.
- Perf smoke (printed in test output, `--nocapture`): genuine 4-interface
  fixture ~811 µs/observation ci-profile; synthetic 128/1024-interface
  scaling printed by `synthetic_scale_workloads_report_scaling_and_budget_refusal`
  (synthetic scale data, not device evidence).

## Genuine fixture

`tests/fixtures/bn024/interface-frr-json/` — provenance, immutable image
digest, sha256 hashes and live re-verification recorded in
`PROVENANCE.md`. Canonical L0–L3 gate logs land under
`target/ci/local-run-*` on the gate runner host and are referenced from
the final report; they are not duplicated here.
