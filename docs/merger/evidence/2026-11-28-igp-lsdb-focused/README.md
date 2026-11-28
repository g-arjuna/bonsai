# BN-026/BN-024 initial IGP focused verification evidence

Candidate branch: `feat/bn026-igp-lsdb`

Executed focused evidence:

- `igp-focused-rust.log`: trusted shared-lane `cargo test -p bonsai --profile ci --lib igp::tests --offline`; 10 passed, 0 failed; compile 6m35s, execution 0.00s, wall 6m44.11s, peak RSS 6,036,084 KiB.
- `igp-capture-python.log`: `pytest tests/test_frr_igp_capture.py -q`; 4 passed.
- `igp-artifact-validation.log`: structural ledger consistency validation; 68 entries, 64 upstream-implemented, 4 improvement-only entries; partial IGP entries receive no completion credit.
- `SHA256SUMS`: digests of the preserved logs.

Scope boundary: synthetic control and upstream-shape parser normalization only. This evidence does not establish live/populated device qualification, production collection wiring, coherent snapshot replay, derived topology, SPF, physical topology or forwarding correctness. Canonical L0-L3 and L4 were not executed for this review candidate.
