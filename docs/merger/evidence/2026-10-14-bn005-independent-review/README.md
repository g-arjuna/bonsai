# Independent BN-005 evidence — 14 October 2026

[Security review](../../BN005_INDEPENDENT_SECURITY_REVIEW_2026-10-14.md) is the
current acceptance decision. [results.json](results.json) records exact commands,
source and log hashes, counts and proven blockers.

- `regressions.log`: all 6 BN-005 and 32 related regression tests pass.
- `browser.log`: 2 Rust tests and 12 Playwright journeys pass.
- `security-probes.rs` / `security-probes.log`: four undesirable behaviors
  reproduced, including an unauthenticated HTTP graph write against an isolated
  temporary database. These passing observations are not security acceptance.
- `submitted-provenance.json` / `fresh-provenance.json`: zero exact fixture matches.
- `fresh-browser-bundle/`: fresh lab/screenshots/captures; submitted bundle restored.
- `fmt.log`, `python.log`, `ui.log`, `clippy.log`: focused checks; UI warnings retained.

To reproduce the observations, copy `security-probes.rs` into a scratch target
`tests/bn005_independent_security_probes.rs`, run the command in results.json and
remove that scratch target afterwards. Its servers bind only loopback and its
writes are confined to new temporary stores. Future fixes must assert denial,
unchanged graph/RIB state and successful permitted operations, rather than retain
these observed-gap assertions as passing acceptance tests.

No implementation fixes, commits or pushes were made by this review.
