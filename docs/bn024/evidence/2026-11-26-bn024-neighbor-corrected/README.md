# BN-024 ARP/ND + MAC corrected candidate evidence

This directory contains the focused verification evidence for the corrected
ARP/ND and MAC semantic-reconciliation candidate. It is REVIEW_READY evidence,
not canonical merge-validation evidence.

Executed on the corrected worktree:

```text
scripts/ci/build-lane.sh --lbug-native ~/.cache/bonsai-ci/lbug-native -- \
  cargo test -p bonsai --lib --profile ci --offline
```

Result: `816 passed; 0 failed; 8 ignored` in `108.84s` after warm reuse.
See `bonsai-lib-full.log`.

```text
scripts/ci/build-lane.sh --lbug-native ~/.cache/bonsai-ci/lbug-native -- \
  cargo test -p bonsai --lib --profile ci --offline neighbor_ -- --nocapture
```

Result: `47 passed; 0 failed; 1 ignored` in `2.56s`. See `neighbor-focused.log`.
This filter includes the scoped device/tenant/ASIC/bridge-domain isolation,
VRF/IPv6/interface identity, temporal compatibility, coverage selection,
malformed-input/state, structured-subinterface and concentrated-candidate
regressions.

```text
scripts/ci/build-lane.sh --lbug-native ~/.cache/bonsai-ci/lbug-native -- \
  cargo test -p bonsai --lib --profile ci --offline \
  synthetic_cross_table_scaling_100_1000_10000 -- --ignored --nocapture
```

Result: pass. 100 reconciliation pairs in 16 ms, 1,000 in 185 ms and 10,000
in 1,842 ms. See `neighbor-scaling.log`. The concentrated-key regression in
`neighbor-focused.log` evaluated at most 64 of 256 concentrated candidates in
17.147 ms and returned an explicit indeterminate truncation result.

```text
/home/arjuna/Desktop/bonsai-netintel/.venv/bin/python -m pytest \
  tests/test_bn024_typed_facts.py -q
```

Result: `17 passed`. See `typed-facts-pytest.log`.

`ledger-provenance-validation.log` records upstream provenance validation,
presence of all six pinned neighbor fixtures, 66 capability entries and the
unchanged `arpnd.acquisition`/`macs.acquisition` `EXISTING_PARTIAL`
classification. `fixture-sha256.txt` records the local pinned fixture hashes.
The two Cumulus fixtures are intentionally different-device captures and are
not represented as genuine same-device cross-table evidence.

Canonical L0-L3/L4 gates: NOT_EXECUTED for this corrected candidate by design;
independent review is required before committing another canonical cycle.
