# Historical canonical L0–L3 evidence for combined candidate 008b01f

This package preserves the successful canonical gate log that was produced
locally on 2026-11-27 and previously left outside the repository evidence
hierarchy. It is historical verification evidence only; it was not rerun for
this packaging commit.

## Exact tree and composition

- Validated candidate SHA: `008b01f3b7749d9000b073cfdc81d2674841a8e6`
- Published evidence-only head after the build-timing update:
  `cc33ba47445d085da6fd4d2fb4f7984f1a31880e`
- Integration base: `main` after PR #95 at
  `76c780115b468e6f0fbc56548c72716d1335a33d`
- Actual constituent ancestors present in `008b01f`:
  - PR #96 / native BGP and initial native route resolver:
    `57ceccbe9d9450dfe8ee6c6012b9e80e424c31c0`
  - PR #97 / operational LLDP:
    `fc2ad9a9dc11f1b65b9ce19f093c5d8e7951e677`
- Intended PR #98 / operational native-route query head:
  `0a6b92a4016a9f264c02669e9c2a7c7568b5430d`

Repository ancestry inspection during evidence packaging found that the PR #98
head is **not** an ancestor of `008b01f`; the `get_native_route_lookup` MCP
implementation is absent from that tree. Therefore this log is valid historical
evidence for the exact `008b01f` tree only, not evidence that the complete
three-PR scope was canonical-gate validated. The final post-correction combined
candidate still requires its own canonical cycle.

## Gate-registration additions in 008b01f

Commit `008b01f` added exactly three canonical target registrations beyond
the preceding integration tree:

- L2 integration target `bn006_native_routes`
- L2 integration target `bn024_lldp_operational`
- L3 integration target `bn006_collection_wiring`

## Execution and environment evidence preserved in the log

- Command recorded by the build-time log:
  `CARGO_BUILD_JOBS=2 scripts/ci/gates.sh l0 l1 l2 l3` through
  `scripts/ci/build-lane.sh --lbug-native /home/arjuna/.cache/bonsai-ci/lbug-native`
- Build lane: `CARGO_TARGET_DIR=/home/arjuna/.cache/bonsai-ci/build-lane`
- lbug mode: prebuilt native bundle `2b38794b29bd`
- Gate base used by L0 formatting:
  `76c780115b468e6f0fbc56548c72716d1335a33d`
- Wall time: 32:00.92 (1,921 seconds)
- Peak RSS: 7,088,128 KiB (approximately 6.76 GiB)
- Exit status: 0
- Result: L0 PASS, L1 PASS, L2 PASS, L3 PASS; `ALL REQUESTED GATES PASSED`
- Log SHA-256: `5cfdaf791cae9eede0c760e2e5bf6a04b348a74db3883b921a54e5d0ea171efa`

The original log does not contain a separate pre-execution CPU/memory/disk
preflight transcript. Publication-time host facts were therefore not inserted
into the historical result as if they had been measured before the run. The
inexpensive execution-environment facts above are limited to lines visible in
the preserved log and the existing `docs/build-times/runs.csv` row.

## Files

- `canonical-gates-l0l3-008b01f.log` — unmodified preserved canonical log.
- `composition.txt` — exact ancestry and diff inspection commands/results
  captured during packaging.
