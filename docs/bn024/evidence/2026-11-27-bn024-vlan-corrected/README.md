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
