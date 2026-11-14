


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

**Required for Devin and other agents:** [Bonsai Build Acceleration Playbook](docs/BUILD_ACCELERATION_PLAYBOOK_2026-11-11.md) documents the source-measured bottlenecks, safe iteration lanes, cache/worktree ownership, performance experiments and BN-019 acceptance. It complements (does not relax) the correctness/test gates below.

### Fast feedback without false-green verification

- Start with `git status --short`, `git worktree list`, `df -h .`, `rustc -Vv`, and `sccache --show-stats`. **Never** modify the protected BN-009 soak metrics CSV or another agent's build directory; do not use `make build-time` on the shared workspace because it invokes `cargo clean`.
- For every edit: formatting/wiring and the **smallest owning crate** (`cargo check -p rbmp-core --offline`, or `cargo check -p bonsai --lib --offline`). For changed behavior: compile and execute the smallest *named* unit/integration test, then all risk-linked regressions **before merge**. A focused `cargo test --test X` still links a heavyweight Bonsai test binary the first time; a test-name filter does not skip its build.
- One **active heavy Cargo build** per shared `target`. Coordinate build ownership across worktrees. Separate targets avoid lock contention but consume massive disk and have caused ENOSPC; do not create new cold targets just to run another compiler concurrently. Use parallel agents for review/fixtures/docs while the single build owner compiles.
- Keep toolchain, features, profile, target-dir and `RUSTFLAGS` stable during an iteration; don't mix `dev` with `release` or flip `CARGO_INCREMENTAL` per invocation. sccache reuses cacheable crates, **not** incremental objects or the final linked Bonsai binary. Benchmark cache/profile changes in an isolated quota-bounded directory before proposing shared settings.
- Avoid building release Docker images on each Rust edit. Use scoped tests while coding; do a pinned SHA-based image/lab/replay run when named by the acceptance criteria. Never label `cargo check`, `--no-run`, or a cached build as executed tests.
- Capture actual wall time, build-versus-run time, sccache hits/misses, disk and peak RSS for expensive targets; append to `docs/build-times/runs.csv` and preserve extended evidence. Run `cargo ... --timings` to identify compiler/link critical paths before optimizing.
- The canonical fail-closed validation gates are `scripts/ci/gates.sh` tiers L0–L4 (`just gate <tier>` / `just gate-all` wrap them; `.github/workflows/ci.yml` runs the same script). A green gate means the named tests compiled, executed and passed — never infer L4 lab acceptance from L0–L3. See `docs/DEVELOPMENT.md` "Canonical Validation Gates".


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

