# BN-022 reconciliation gate evidence (2026-11-20)

Canonical trusted-local gate run on the PR #17 reconciliation merge.

- Candidate SHA: `2ce3b2c57520558cdce3187de0dd9e10d433c76e`
  (merge of `origin/main` @ `d3dd771` into `feat/bn022-auth-storage` @ `85f4c29`)
- Command: `scripts/ci/run-local.sh --trusted --shared-target-dir
  ~/.cache/bonsai-ci/build-lane --lbug-native ~/.cache/bonsai-ci/lbug-native l0 l1 l2 l3`
- Result: **L0/L1/L2/L3 ALL PASS**, exit 0
- Full gate log: `gate.log` (sha256
  `0aac179154c8407dacca9250476cca4d5f3ec31cfec3e52c8159b9994937b6e5`)
- L0 note: 2 files carry pre-existing fmt drift at base (warn-only,
  unchanged policy).
- Recovery harness on same SHA: 79 PASS / 0 FAIL
  (`/tmp/bonsai-final-recovery.5TUHMx/results.txt`).
- Focused suites on same SHA: bn022_auth_closure 49, bn022_auth_storage
  12, bn022_graph_read_boundary 5, bn022_sec19_sec20 13,
  b11_b12_b13_routing_workflow 3 — 82/82 PASS.
