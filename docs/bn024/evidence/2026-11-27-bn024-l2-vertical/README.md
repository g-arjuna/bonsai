# BN-024 Linux L2 vertical-slice focused evidence

Date: 2026-11-27. Branch/worktree: `feat/bn024-l2-vertical` /
`bonsai-netintel-l2slice`. Status: REVIEW_READY candidate evidence; not a
merge-ready canonical gate claim.

## Scope

Extends the existing authorized standalone-FRR shadow acquisition seam from
interface-only evidence to one bounded fixed-command Linux L2 snapshot:

- `vtysh -c 'show version json'`
- `vtysh -c 'show interface json'`
- `ip neigh show`
- `bridge fdb show`
- `bridge -c vlan show`

The acquisition remains verified-SSH, read-only, exact-artifact qualified and
shadow-only. No authoritative graph projection is added.

## Genuine fixture provenance

`tests/fixtures/bn024/l2-vertical/PROVENANCE.md` records genuine read-only
output captured from `clab-bonsai-bn003-vrf-frr2`. The empty neighbor output is
a positively established empty population for a successful complete scoped
command, not missing collection. The header-only VLAN output is likewise
positive-empty after the Linux `port vlan-id` header correction. The FDB output
contains two genuine permanent multicast/self entries on `eth0`.

## Focused verification

- Python capture helper: 5/5 passed, including exact fixed multifamily command
  order/envelope shape and bounded stream behavior.
- Native Linux normalizer fixtures: 2/2 passed (`genuine_linux_frr` selection):
  positive-empty ARP/ND, two validated MAC forwarding facts, and positive-empty
  Linux bridge VLAN population.
- Production API integration target: 9 passed, 0 failed, 1 explicitly ignored
  live-gated test. This includes
  `operator_surface_exposes_multifamily_coverage_and_bounded_l2_evidence`;
  it validates authorized query shape, output digests/retention flags,
  per-family coverage, positive-empty ARP/VLAN semantics, MAC facts, and no raw
  output exposure.
- Browser acceptance: 6/6 Chromium tests passed against the freshly built UI
  served locally with API route fixtures, including per-family coverage and L2
  MAC display without fabricated empty neighbor rows.
- `cargo fmt --check`: passed; `git diff --check`: passed. One intermediate
  full integration run failed because an interface-only pre-existing assertion
  had been incorrectly broadened to the multifamily count; the assertion was
  corrected and the full target then passed. No implementation assertion was
  weakened.

Build/test timing rows were appended to `docs/build-times/runs.csv`.

## Honest boundaries

- Canonical L0-L3: NOT_EXECUTED for this candidate. This evidence is for
  independent review/REVIEW_READY only, not merge readiness.
- Live multifamily SSH re-acquisition: NOT_EXECUTED. The prior interface-only
  lab credential was deliberately rotated to an unknown random password after
  PR #89 evidence collection, and this slice did not perform any live device
  write or credential reset. The production path is therefore verified here by
  hermetic API/persistence tests, genuine captured command-shape fixtures, and
  prior authorized SSH evidence for the same seam; it is not claimed as a new
  live end-to-end multifamily capture.
- L4/live deployment acceptance: NOT_EXECUTED and not implied.
- The FRR interface normalizer does not yet emit structured subinterface
  identity; cross-family interface corroboration remains conservative and does
  not invent parent/subinterface identity from display strings.
