# BN-019 build optimization — measured results (2026-11-21)

Host: 8 CPU / 46 GiB RAM, Rust/Cargo 1.95.0, mold + sccache,
prebuilt lbug bundle `2b38794b29bd…`, shared lane
`~/.cache/bonsai-ci/build-lane`. Benchmark:
`scripts/ci/build-benchmark.sh --profile ci --label bn019-warm`
(JSON: `benchmarks/20261121T172002Z-bn019-warm.json`; CSV rows in
`runs.csv`). All figures are warm-lane, first-run-in-this-worktree
(i.e. ci-profile deps were already warm from the YANG test build;
`cargo check` artifacts were cold for the ci profile).

## Warm-build timings (ci profile, `--test bmp_rfc_contract`)

| Phase | Wall (s) | Notes |
|---|---|---|
| env snapshot | 0.06 | toolchain/lbug/sccache/disk |
| `cargo check -p bonsai --lib` | 246.0 | cold ci-profile check artifacts |
| `cargo test --lib --no-run` | 319.8 | lib compile+link, peak RSS 5.3 GiB |
| `cargo test --lib` | 472.0 | 487 tests executed in 143 s |
| `--test bmp_rfc_contract --no-run` | 482.6 | integration compile+link |
| `cargo test --test bmp_rfc_contract` | 529.8 | 7 tests in 0.07 s |
| test-file-only edit → `--no-run` | 517.6 | mtime touch, full ci rebuild |

Disk: 49 GiB → 46 GiB avail across the run (ci check artifacts +
unrelated image pull). Peak process-tree RSS ≈ 8.1 GiB.

## Profile decision (A3)

Measured artifact sizes on the shared lane (`build-lane-gc.sh report`):

| Profile | Lane size | Executables | Notes |
|---|---|---|---|
| debug | 89.3 GB | 56.1 GB / 66 files (~850 MB each) | incremental 13.7 GB, .dwp 10.0 GB |
| ci | 6.1 GB | 1.9 GB / 60 files (~32 MB each) | what L0–L3 gates already warm |

YANG integration targets, ci profile compile+link each: 44.5–72.4 s
(cargo-timings report); debug-profile equivalents are ~860–900 MB
binaries plus ~165 MB `.dwp` each.

**Decision: keep `dev` for `cargo check`/debugging; use `--profile ci`
for focused tests and gates.** Rationale: ci executables are ~27×
smaller, links use far less memory, and the canonical gates already
warm that fingerprint family. Cost, measured: `incremental=false`
makes any bonsai-crate change a full ~5–8 min recompile — including
the 517 s test-file-only edit loop above. That is the accepted trade
for disk/link headroom on this host; dev+incremental remains the
right lane when actively iterating inside `src/` with debugger needs.
No Cargo.toml profile flags were changed — the ci profile already
existed and is gate-canonical.

## Test consolidation decision (A4)

The four YANG integration targets were measured individually under
the ci profile (per-unit compile+link 44.5–72.4 s; ~120 MB executables
total). Merging them into one binary would produce a single ~250 s+
link and a larger failure-isolation boundary, while saving at most
the parallel slack of ~3 link passes — not a meaningful win, and it
would couple four independently-owned suites' fixtures. **Decision:
keep the four targets separate.** `gates.sh` already compiles them in
one Cargo invocation, which captures the real saving (one dep graph,
one lib build) without merging test code.

## Artifact retention (A5)

`scripts/ci/build-lane-gc.sh` implements report/clean with the lane
flock, canonical-path checks, Cargo-probe `--stale-units` mode,
per-category opt-ins and JSONL manifests under
`~/.cache/bonsai-ci/gc-logs/`. Selftest
`scripts/ci/build_lane_gc_selftest.sh`: **25/25 PASS** (dry-run,
unexpected paths, lock contention, protected artifacts, profile
scoping, stale-generation, idempotence, undeletable-file evidence).

First real-lane `report`: 129.5 GB total — debug 89.3 GB, ci 6.1 GB.
Executed `clean --apply --intermediates` (debug profile; ci skipped
by default): removed 556 post-link objects/hardlink temps,
**3.94 GB reported / +3.7 GiB disk avail** (47.0 → 50.8 GiB),
0 failures, manifest `gc-20261121T180251Z.jsonl`. Larger recovery
(--dwp 10 GB, --incremental 13.7 GB, --stale-units probe) is
implemented but was not applied: dwp/incremental are lossy-by-design
opt-ins and the stale-units probe requires a full workspace
`--no-run` compile; left for an operator window.

## Before / after summary

| Metric | Before | After |
|---|---|---|
| Warm lib test compile+link (ci) | ~5.3 min (319.8 s) | same — no regression |
| Integration compile+link (ci) | ~8 min (482.6 s) | same |
| Test-file-only edit loop (ci) | n/a (new measurement) | 517.6 s — documented trade |
| Integration exe size (ci, yang_*) | n/a | 6.5–48 MB each |
| Integration exe size (debug, yang_*) | n/a | ~860–900 MB + ~165 MB .dwp each |
| Shared lane size | 129.5 GB | ~125.6 GB (+3.7 GiB avail) |
| Unsafe benchmark | cargo clean + touch + docker --no-cache | non-destructive warm benchmark |
| Lane GC | none | report/clean + 25-test selftest |
| Test count | unchanged | unchanged (no tests removed) |
