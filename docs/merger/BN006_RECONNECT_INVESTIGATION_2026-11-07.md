# BN-006 reconnect investigation — 7 November 2026

Status: reconnect replay fix implemented; broader BN-006 **Partial**. Base commit `ffb264e`; the regression and captured journal
below are pre-existing, uncommitted working-tree files. No live lab was run.

`env RUSTC_WRAPPER= cargo test --locked -p rbmp-store --test bn006_epoch_resync`
reproduces the existing assertion failure: expected four RIB entries, observed
five. The fixture's parsed observations show global Loc-RIB prefix
`10.77.1.0/24` announced with path ID 3 in epoch 1, an abrupt transport gap,
and an IPv4 Loc-RIB end-of-RIB marker in epoch 2 before a subsequent path ID 2
announcement. The test expects one path in each of the two VRFs and each of
the post-policy and Loc-RIB views. This is a captured journal claim; the live
capture provenance and independent router state must still be validated before
closing the acceptance gate.

The prior RIB manager marked end-of-RIB but did not retire paths absent from
the newer snapshot. An exploratory implementation tracking the last observed
epoch and emitting explicit retirement events made this replay assertion pass.
It was removed because it is not safe for existing durable history: historical
replay would emit additional events and shift later sequence numbers, while
`RoutingServiceState::new` currently identifies missing events by sequence only.
This can mismatch old history and projections. A retirement must also remain
distinct from an observed BGP withdrawal and must not occur merely on TCP EOF.

The working-tree fix tracks each observed path's epoch and retires older,
unrefreshed paths only in the peer/view/family completed by end-of-RIB. It
records the retired route identities in that existing EOR event, not as BGP
withdrawals. Historical replay reconstructs this metadata from the BMP journal
without rewriting existing event-journal bytes or shifting event sequences.
The graph projector rebuilds older checkpoints, and remote projection rejects
services that cannot attest to these semantics or whose stored event identities
are not covered by BMP replay.
Stale-epoch EORs are no longer emitted as current synchronization; their old
sequence slots remain reserved. A legacy event journal containing such an EOR
is therefore marked incompatible for projection rather than silently repaired;
its durable history is preserved and requires a separate migration/review.
Prefix history and snapshot comparison expose the retirement. Focused tests
cover the EOS fixture, identity isolation, interrupted checkpoint rebuilding,
legacy journal compatibility, remote compatibility, and recovery regressions.
For remote deployments, upgrade the routing service before the projector; an
older service is rejected until it advertises compatible event semantics.

The optional DuckDB writer retains the EOR record but does not materialize its
retired route identities; it was not tested under `duckdb-store`. Its legacy
projection requires separate BN-007 intake. The event journal plus BMP journal
remain the tested history path.

Next: rerun the dedicated EOS live lifecycle with preserved evidence and confirm
independent device state, and complete the remaining BN-006 second-NOS scope.

Original failing reproduction inputs (SHA-256, before the regression was expanded):

- `tests/fixtures/bn006_eos_4_36/reconnect.journal.jsonl`: `d4746fd8e3179f31a28512aa8417f781ae9971b6d02e2b594644ac2586f1fc97`
- `crates/rbmp-store/tests/bn006_epoch_resync.rs`: `28af4bb123f5d0c1abf0d3c2b8b7c2e7e8bd4a0346c76f67a4ccb7f4d13af7ef`

The exploratory implementation's checks are historical only. Current commands,
source hashes and results are recorded in [the evidence manifest](evidence/2026-11-07-bn006-reconnect/results.json).
