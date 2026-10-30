# BN-005 closure analysis evidence — 15 October 2026

[Finite closeout plan](../../BN005_CLOSEOUT_PLAN_2026-10-15.md) explains the findings,
recommended design and exact acceptance. [results.json](results.json) records
source identity, commands, log hashes and what was not run.

`regressions.log` records 11 hardening, 20 executor, 6 mock investigator and 3
workflow tests. Other routine logs cover Python, format, selected-core Clippy
and UI build. These pass; UI warnings remain.

`closure-probes.rs` and `closure-probes.log` contain three review checks:
three MCP scope-shape observations, a speaker-scoped HTTP detection read, and a
positive native read-only transaction feasibility check. To rerun, copy source
to `tests/bn005_closure_scope_probe.rs` and execute the command in results.json.
Remove that scratch target afterwards. The source seeds only temporary databases;
passing observed-gap assertions are not desired-denial security acceptance.

`scope-probe.log`, `http-probe.log` and `readonly-feasibility.log` retain focused
runs. `http-graph-snapshot-attempt.log` is a failed exploratory probe (HTTP 500).
It did not prove snapshot exposure; the separate successful detections probe
established the HTTP data-access finding. It is not hidden or counted as a
passing security gate.

No product implementation, external-provider, lab, commit or push action was
performed. Read-only transaction feasibility is tested locally; application
wrapping, transaction-control denial, rollback and cancellation remain to implement.
