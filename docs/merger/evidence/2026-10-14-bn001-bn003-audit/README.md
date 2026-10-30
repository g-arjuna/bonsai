# BN-001/002/003 audit evidence — 14 October 2026

[Comprehensive review](../../BN001_BN002_BN003_COMPREHENSIVE_REVIEW_2026-10-14.md)
is the acceptance decision. [results.json](results.json) records source hashes,
working-tree/base-commit identity, counts, dispositions and log hashes.

- `regressions.log`: 16 executor/assurance, 7 recovery and 2 wiring/scope tests.
- `imported.log`: 132 passed and 2 ignored, including imported regression targets.
- `browser.log`: 1 Rust orchestrator and 12 Playwright journeys.
- `python.log`, `fmt.log`, `clippy.log`, `ui.log`: executed focused checks.
- `fresh-browser-bundle/`: fresh screenshots/router output/raw bytes/manifest.
  Raw bytes include injected VRF fixtures and live FRR observations; the supplied
  binary hash matches its manifest. The original submitted bundle was restored.
- `review-probes.rs` and `probes.log`: four observed-gap probes plus one independent
  parent/child-process scope confirmation. Passing observed-gap probes reproduces
  undesirable behavior; it is not safety acceptance. The child worker's output
  is nested in the log and is not an additional product gate.

To rerun the review probes, copy their source to a scratch target named
`tests/bn001_003_comprehensive_review_probes.rs` and run:

```sh
cargo test --locked --offline --test bn001_003_comprehensive_review_probes -- --skip process_scope_worker
```

Remove only that scratch target afterwards. All its failure-injection journals
are created under temporary directories. Desired-behavior regressions for fixes
must invert the four observed-gap assertions. No implementation fixes were made
by this audit. Full workspace Clippy, live second NOS, real process-crash phases,
BN-004 provider evaluation and actual operational execution were not performed.
