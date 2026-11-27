# BN-024 VLAN/bridge corrective focused evidence

This directory preserves the pre-gate focused validation for the corrected
`feat/bn024-vlan-bridge` implementation. Canonical L0–L3 validation is
recorded separately after the candidate is frozen.

## Focused results

- `cargo check -p bonsai --lib --offline`: PASS (15.40s dev-profile compile,
  trusted shared lane).
- `cargo test -p bonsai --lib --profile ci --offline vlan`: PASS — 20 passed,
  0 failed (genuine EOS/NXOS/Cumulus fixtures, membership-availability and
  malformed-shape coverage).
- `cargo test -p bonsai --lib --profile ci --offline forwarding_domain`:
  PASS — 24 passed, 0 failed (deterministic interface selection, temporal
  compatibility, Linux bridge-master evidence, genuine EOS/NXOS integration,
  bounded query order independence).
- `cargo test -p bonsai --lib --profile ci --offline neighbor_reconciliation`:
  PASS — 36 passed, 0 failed, 1 ignored.
- `cargo test -p bonsai --lib --profile ci --offline interface_relationships`:
  PASS — 17 passed, 0 failed.
- `cargo test -p bonsai --lib --profile ci --offline typed_facts`: PASS —
  42 passed, 0 failed, 2 ignored.
- `PYTHONPATH=.:python .venv/bin/python -m pytest -q
  tests/test_bn024_typed_facts.py tests/test_bn024_provenance.py`: PASS —
  40 passed in 24.32s.
- `scripts/bn024/upstream_provenance.py validate docs/bn024/provenance/upstream-sources.json`: PASS (`OK`).
- `scripts/bn024/upstream_provenance.py verify docs/bn024/provenance/upstream-sources.json /tmp/bn024-upstream-checkouts`: PASS (`OK (126 entries verified)`).

## Failure handling

An intermediate forwarding-domain test run failed closed because a newly added
LAG test payload itself violated the typed member/master consistency
validator. The fixture was corrected to use the same explicit LAG master target;
the implementation was not weakened, and the final run above passed.

## Canonical L0-L3 result

Frozen tested implementation candidate: `3d5aaf28d1d9289a94be3084a67996d6668f2987`.
Integration base: `91240469a16106fe183e27ef03d9cf8afc00364b`.

Command:

```text
scripts/ci/build-lane.sh --dir ~/.cache/bonsai-ci/build-lane   --lbug-native ~/.cache/bonsai-ci/lbug-native --   env CARGO_BUILD_JOBS=2 PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=/usr/bin/google-chrome   GATE_BASE=91240469a16106fe183e27ef03d9cf8afc00364b   scripts/ci/gates.sh l0 l1 l2 l3
```

Result recorded in `canonical-l0-l3-3d5aaf2.log`:

```text
l0 PASS / l1 PASS / l2 PASS / l3 PASS — ALL REQUESTED GATES PASSED
```

- Full bonsai library: 874 tests executed in L2.
- Root Python suite: 77 passed.
- SDK Python suite: 318 passed, 1 skipped, 1 deselected.
- Wall time: 1,831 seconds (04:06:30Z-04:37:01Z).
- L4: NOT_EXECUTED — this offline/normalization slice has no lab acceptance
  boundary and L4 is never inferred from L0-L3.

During the browser/workflow portions, historical benchmark JSON artifacts were
regenerated as test side effects. They were restored byte-for-byte from `HEAD`
before publication and are not part of this evidence change.
