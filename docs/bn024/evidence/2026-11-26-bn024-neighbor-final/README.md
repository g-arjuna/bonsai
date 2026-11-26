# BN-024 ARP/ND + MAC final canonical gate evidence

Frozen implementation candidate: `2d27c7ff043d8e472a08e61ec44c305283ee784d`
(branch `feat/bn024-arpnd-mac`, PR #93).
Integration base: `83d30fd68b4008e423932a31ac01426b203e9d9a` (`origin/main`,
also the merge-base used for changed-file detection).

## Command

```text
scripts/ci/build-lane.sh --lbug-native ~/.cache/bonsai-ci/lbug-native -- \
  env CARGO_BUILD_JOBS=2 PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=/usr/bin/google-chrome \
  GATE_BASE=83d30fd68b4008e423932a31ac01426b203e9d9a \
  scripts/ci/gates.sh l0 l1 l2 l3
```

## Result (see canonical-l0-l3-2d27c7f.log)

```text
l0 PASS / l1 PASS / l2 PASS / l3 PASS — ALL REQUESTED GATES PASSED
```

- Full bonsai library: `835 passed; 0 failed; 8 ignored` in 108.55s
  (ci profile, shared trusted lane).
- Python suites: `74 passed` (root tests) and `318 passed, 1 skipped,
  1 deselected` (python/tests).
- L4: NOT_EXECUTED — offline feature, no lab acceptance boundary.

A first run of the same cycle reached L0/L1/L2 PASS but reported L3 FAIL
because pytest was not provisioned in this worktree (environment defect, no
test executed). After provisioning `.venv`, the complete cycle above was
re-executed end-to-end against the unchanged frozen tree; all tiers passed.

## Focused regression summary (pre-gate, same tree)

- `neighbor_reconciliation`: 34 passed, 0 failed, 1 ignored — includes
  same-domain VLAN contradiction order-independence, structured subinterface
  non-equality, stale domain binding exclusion, timestamp/ordering coverage
  determinism, future-capture exclusion, noncomparable-interface outcome,
  failed-neighbor residual-MAC exclusion and concentrated-candidate bounds.
- `semantic_evidence` 8, `interface_relationships` 17, `lldp` 46,
  `typed_facts` 42, `neighbor_normalizer` 20 — all passed.
