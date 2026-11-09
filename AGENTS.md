


# Development contract

- Product: Bonsai. Routing protocol/history components originate in RustyBMP (`crates/rbmp-core`, `crates/rbmp-rib`, `crates/rbmp-store`).
- Read `BONSAI_BACKLOG.md` and `docs/STATUS.md` (older planning documents are historical references); inspect current Git state before making changes.
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

## Build and test discipline

- Inner loop: prefer justfile recipes (`check-fast`, `test-protocol`,
  `test-replay`) or focused targets — `cargo test -p <crate>` /
  `cargo test --test <name>`. Never bare `cargo build`/`cargo test` on the
  `bonsai` package: it compiles ~600 deps + dev-deps + a ~1 GB dev binary.
- sccache is the configured `rustc-wrapper` (`.cargo/config.toml`) and its
  server persists across sessions — sccache is MANDATORY, not optional. Never
  prefix cargo with `RUSTC_WRAPPER=` and never bypass the wrapper: if sccache
  errors (e.g. `Operation not permitted (os error 1)`), fix sccache itself —
  run `sccache --show-stats` or `sccache rustc -vV` to start/verify the
  server, check `SCCACHE_DIR`/socket permissions, restart it with
  `sccache --stop-server && sccache --start-server` if needed — then retry
  cargo normally. Cargo caches the `rustc -vV` probe (including failures) in
  `target/.rustc_info.json`: if cargo keeps replaying an old sccache error
  without spawning rustc, delete that file and retry. Report persistent
  sccache failures to the user instead of working around them.
- mold linker + codegen-units are already configured; they speed linking,
  not the hundreds of crate compilations — scope the test target instead.
- For long compiles, run cargo in a background shell and poll rather than
  blocking; report test status honestly if still running at handoff.
- Record build/check wall-times: after any verification command that takes
  meaningful time (cargo check/test/build, just recipes, image builds,
  large pytest runs), append a row to `docs/build-times/runs.csv`
  (format in `docs/build-times/README.md`). This is how we verify sccache,
  mold and codegen-units are actually helping rather than compilation
  times silently growing.

