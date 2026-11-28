# Corrected BGP/LLDP/native-route integration canonical evidence

Candidate SHA: `7dd49afe5076648b5eb4fcf587ad17638b35a460`  
Integration base: `76c780115b468e6f0fbc56548c72716d1335a33d` (`origin/main`)  
Combined PR: #99; constituent review history: #96, #97, #98

## Canonical result

Command:

```text
scripts/ci/build-lane.sh --lbug-native /home/arjuna/.cache/bonsai-ci/lbug-native -- \
  env CARGO_BUILD_JOBS=2 GATE_BASE=origin/main scripts/ci/gates.sh l0 l1 l2 l3
```

Result: **PASS** — L0 PASS, L1 PASS, L2 PASS, L3 PASS, exit status 0.

Wall time: 36m43.34s (2,203.34 s). Peak RSS: 6,684,772 KiB. Trusted shared build lane, warm prebuilt Ladybug, sccache/mold/ci profile, `CARGO_BUILD_JOBS=2`.

Canonical evidence log: `canonical-gates-l0l3-7dd49af.log`. Environment preflight: `environment-preflight.txt`.

Important contents include:

- L2 Bonsai library: 895 passed / 0 failed / 8 ignored.
- L2 required integration targets including `bn005_closeout` (11/11), `bn006_native_routes` (24/24), `bn02404_native_interface_api` (22 passed / 1 explicitly ignored live-gated), and `bn024_lldp_operational` (10/10): PASS.
- L3 cross-component Rust/browser targets including `bn006_collection_wiring` (3 passed / 1 explicitly ignored live-gated) and `bn006_native_nos_adapter` (15/15): PASS.
- Root Python suite: 55 passed / 24 skipped.
- Python SDK suite: 316 passed / 3 skipped / 1 deselected.

Skips/ignores are explicit and are not counted as passes. L4/live qualification is **NOT_EXECUTED**; green L0–L3 is not L4 lab acceptance.

## Focused correction evidence

- `focused-integration-final.log`: consolidated five-target integration verification, 74 passed / 0 failed / 2 explicitly ignored live-lab tests, 67.81 s, peak RSS 5,436,628 KiB.
- `focused-library.log`: filtered library regressions for BGP reconciliation, LLDP operational, FRR acquisition, MCP and typed facts: 82 passed / 0 failed / 3 ignored.
- `lldp-browser.log`: browser lifecycle acceptance, 5 passed.
- `ui-build.log`: production UI build passed.
- `provenance-pytest.log`: provenance tests, 20 passed.
- `playbook-schema.log` and `upstream-provenance-validate.log`: schema/provenance validation passed.
- `bn005-policy-correction.log`: demonstrated test-policy correction, 1 passed / 0 failed after adding `get_native_route_lookup` to the explicit device-tool matrix.

## Invalidated predecessor evidence retained append-only

- `invalidated-canonical-c3d7217-ownership-compile-failure.log`: L0 PASS, L1 compile failure in `ownership_compatibility` due missing explicit inventory-registry test context; corrected by `118846e`.
- `invalidated-canonical-118846e-bn005-policy-failure.log`: L0/L1 PASS, L2 FAIL solely because the BN-005 policy matrix omitted `get_native_route_lookup`; 895 library tests passed before that failure; corrected by `7dd49af`.

These predecessor logs are diagnostic history only and are not evidence of acceptance for the final candidate.

## Scope boundary

The validated implementation remains operational evidence intelligence, not forwarding-plane proof. Native route resolution uses selected same-VRF route evidence only; it does not establish RIB/FIB forwarding verification. LLDP evidence supports operational topology evidence within its provenance and lifecycle boundary. Live L4 qualification remains separately required.
