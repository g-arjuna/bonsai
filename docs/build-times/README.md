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
