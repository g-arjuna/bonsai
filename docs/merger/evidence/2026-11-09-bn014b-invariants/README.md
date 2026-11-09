# BN-014b — independent invariant checker (9 Nov 2026)

Slice 2 of BN-014 (real lab change assurance). Evaluates independent
post-change assertions over the persisted baseline/candidate snapshots
from BN-014a — verdicts are computed only from captured state, never
from the change system's own report.

## Implementation

`src/http_server/change_capture.rs`:

- `evaluate_device_invariants(baseline, candidate, expected_prefixes)` —
  pure function, four invariants per device:
  - `capture_complete` — routes_ok && peers_ok in both phases
  - `no_new_peer_downs` — candidate down-peer count <= baseline
  - `prefix_retention` — every baseline prefix present in candidate
  - `expected_prefixes_present` — operator-supplied prefixes present in
    candidate (`skipped` when none supplied)
- Verdicts: `pass` / `fail` / `unknown` / `skipped` with detail strings;
  overall = fail > unknown > pass.
- `POST /api/changes/{id}/verify-invariants` — pairs snapshots per
  device, evaluates, persists one `change_invariant_verdict` ConfigItem
  per device (`civ-{change}-{device}`), returns per-device + overall.
- `GET /api/changes/{id}/invariants` — returns persisted verdicts.
- `load_change_snapshots` helper extracted (shared with snapshots GET).

## Verification

- Unit tests (`cargo test -p bonsai --lib change_capture`): 3 passed —
  identical snapshots → all pass; prefix loss + new peer down + missing
  expected prefix → fail; incomplete capture → fail.
- Live on packaged `bonsai:latest` against change
  `manual:bn014a-test-001` (device 172.100.207.12, baseline==candidate):
  - `POST verify-invariants {}` → overall `pass`
    (capture_complete/no_new_peer_downs/prefix_retention pass,
    expected_prefixes_present skipped)
  - `POST verify-invariants {"expected_prefixes":["10.10.1.0/24",
    "203.0.113.0/24"]}` → overall `fail`,
    `expected_prefixes_present` fail with
    "1 expected prefixes missing: 203.0.113.0/24"
  - `GET /api/changes/{id}/invariants` → persisted verdict returned
- `cargo check -p bonsai --lib` pass; image rebuilt + recreated,
  `/health` ok.

Raw responses: `verify-pass.json`, `verify-fail.json`,
`invariants-get.json`.

## Limits

- Invariants are intentionally minimal and honest: they assert captured
  state relationships, not semantic correctness of the change. Deeper
  invariants (RPKI, adj-rib policy, FIB consistency) are future slices.
- Verdict records upsert by id — re-verifying overwrites the previous
  verdict for the same change+device (latest evaluation wins).
- An incomplete baseline/candidate pair yields `pair_status:
  incomplete_pair` + overall `unknown`, not a fabricated pass.
