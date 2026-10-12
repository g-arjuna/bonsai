# Development contract

- Product: Bonsai. Routing protocol/history components originate in RustyBMP (`crates/rbmp-core`, `crates/rbmp-rib`, `crates/rbmp-store`).
- Read `docs/merger/STATUS.md` and `docs/merger/BACKLOG.md`; inspect current Git state before making changes.
- Work on one task at a time. Preserve unrelated local changes.
- Reproduce correctness bugs before fixing them; use independent expected values.
- Protocol parsers must reject malformed input without panic or unbounded allocation (`validate_bmp_header`, `read_bmp_frame`).
- Preserve exporter (`ObservationSource`), instance/VRF (`PeerKey`), view (`ViewKey`), family (`CanonicalNlri`), path (`path_id`) and session epoch identity end to end.
- Unknown/unsupported/stale/gap must remain explicit (`completeness_state`, `FamilySupportStatus`, `BgpsecVerdict::Unverified`); never fabricate a success state or synthesize BGP withdrawals on BMP TCP EOF.
- Do not use disposable broadcast as the sole path to durable history (`BmpJournal`, `RibEventJournal`, `RoutingProjectionCheckpoint`).
- No live device writes, broad lab teardown, data deletion or force-push.
- Use only the task's dedicated containerlab resources.
- Do not weaken assertions, hide failures, change goldens blindly or count skips as passes.
- Run focused checks, then the task's required gate. Report unexecuted checks honestly.
- No broad dependency updates, cosmetic rewrites or new infrastructure without task need.
- Finish with changed files, executed commands/results, evidence path and next step.

