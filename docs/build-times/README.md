# Build / check time log

Purpose: verify that the configured build optimizations (sccache
`rustc-wrapper`, mold linker, codegen-units, scoped test targets) are
actually reducing wall-clock times — or whether compilation times are
quietly growing as the workspace does.

## How to record

After every focused verification command that takes meaningful time
(`cargo check`, `cargo test -p <crate>`, `cargo build`, image builds,
`just` recipes, large pytest runs), append one row to `runs.csv`:

```
date,command,scope,wall_seconds,result,notes
```

- `wall_seconds`: measure with `time` (or `/usr/bin/time -f %e`) around
  the command — report the **wall** time, not CPU time.
- `scope`: the crate/test/package targeted (e.g. `bonsai --lib`,
  `rbmp-core`, `pytest python/tests`).
- `result`: `pass` / `fail` / `running` (update the row when it
  finishes if you logged it early).
- `notes`: cache state (`sccache hit/miss`, cold vs warm), whether the
  `bonsai` package dev-binary was involved, anything anomalous.

Run a cold-ish baseline occasionally (e.g. after `cargo clean` of a
single crate's artifacts is NOT required — just note sccache stats via
`sccache --show-stats` before/after if investigating a regression).

## Reading the log

Compare like-for-like rows over time. A growing trend on the same
scope/command means an optimization regressed (sccache misses, mold
not linking, codegen-units changed, dep graph growth). Occasional cold
runs are expected after toolchain or config changes — note them.

## November 2026 profiling protocol for Devin

For practical instructions and bounded experiments see
[Build Acceleration Playbook](../BUILD_ACCELERATION_PLAYBOOK_2026-11-11.md)
and canonical backlog **BN-019-B1…B8**. The existing `runs.csv`
schema remains intact; **do not overwrite or "correct" historical rows**.
The entries were collected on differing source revisions, targets, cache
conditions and available disk. Do not compute a single "Bonsai build time"
from that mixed population.

For each decision-making experiment:

1. Capture SHA, Rust/Cargo version, CPU cores, RAM/swap, free disk, OS,
   Cargo profile, target-dir, build jobs, enabled features, linker/rustflags,
   worktree, `CARGO_INCREMENTAL` override (if any) and `sccache --show-stats`
   before/after.
2. Use `cargo check -p bonsai --lib --timings`, or a specifically selected
   `cargo test -p bonsai --test <name> --no-run --timings`, in a
   **single owned target directory**. Rust Cargo emits HTML under
   `target/cargo-timings/` (or your custom target directory). For a
   run+build breakdown use `/usr/bin/time -v` around the actual command;
   the timing report's compiler units are **not test execution time**.
3. Record `wall_seconds` and `result` here. In a dated evidence report,
   keep compile/link versus execution time, critical-path crates,
   sccache hits/misses, peak RSS, disk delta, cache warmth, and the
   precise command. For already warmed binaries, report first-compilation
   and reuse separately.
4. Compare the **same command** at the **same source revision** and
   comparable machine/cache state before claiming improvement. Use repeated
   warm measurements; cold tests must use an explicitly approved,
   capacity-bounded separate directory rather than `cargo clean`
   or `make build-time` on the shared workspace.
5. A cached or `--no-run` compile is **not a passed regression**. Record
   unit, integration, genuine lab and browser evidence separately. Any
   failed/missing command remains failed/not-run, never a fabricated pass.

### Change proposal template

```text
BN-019-Bx / base SHA / candidate SHA:
Build: exact command + profile/features/target dir, toolchain, linker, jobs
Environment: CPU/RAM/swap/disk + concurrent Cargo ownership
Baseline: cold vs warm wall, compile/link/runtime, peak RSS, cache stats
Candidate: same metrics, number of repeated runs
Savings: absolute seconds + relative %, disk cost, regressions
Correctness: exact checks executed/pass/fail/skip, live vs simulated
Decision: keep/revert/conditional + rollback procedure
```

**Known source-level caveats:** `[profile.dev.package."*"]` currently
optimizes *all* dependency crates at level 3, while the main dev profile
has incremental compilation enabled. sccache may cache dependency builds
but not incremental crates or the final linked executable. These are
hypotheses to benchmark, **not** permission to change flags and invalidate
a working cache during a production-shaped task.

Sources: Rust Cargo [timing reports](https://doc.rust-lang.org/cargo/reference/timings.html),
[profiles](https://doc.rust-lang.org/cargo/reference/profiles.html),
[test --no-run](https://doc.rust-lang.org/cargo/commands/cargo-test.html),
[build cache](https://doc.rust-lang.org/cargo/reference/build-cache.html),
and [sccache Rust behavior](https://github.com/mozilla/sccache/blob/main/docs/Rust.md).
