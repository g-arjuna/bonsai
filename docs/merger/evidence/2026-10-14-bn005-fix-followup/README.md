# BN-005 fix follow-up evidence

[Current review](../../BN005_FIX_FOLLOWUP_REVIEW_2026-10-14.md) and
[results.json](results.json) record findings, source identity and exact log hashes.
`regressions.log` covers 10 BN-005, 20 executor, 7 recovery, 3 workflow and 6
mock-provider wiring tests. `browser.log` records 2 Rust tests and 12 browser
journeys. Other logs record formatting, selected-core lint, Python and UI build.
Submitted browser evidence was restored after saving the fresh bundle.

`scope-probe.rs` reproduces authenticated foreign-device access with a restricted
key while confirming anonymous MCP is denied. A passing observation proves a gap,
not acceptance. To reproduce, copy to `tests/bn005_scope_followup_probe.rs`, run
`cargo test --locked --offline --test bn005_scope_followup_probe`, then remove
that scratch target. All records and credentials are created in an isolated
temporary database. Future fixes should assert denial and allowed own-device reads.
No implementation changes, commits or pushes were made by this review.
