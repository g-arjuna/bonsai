# Bonsai development acceleration — Devin and human agents

**11 November 2026. Status: adopted working policy + measured experiments proposed, not a claim of faster builds.**  
**Owner:** BN-019-B1…B8 in [canonical backlog](../BONSAI_BACKLOG.md).  
**Source reviewed:** Bonsai `main` at `f4243cd15da570fade44c69d67db18aefee151cd`, `AGENTS.md`, `docs/build-times/runs.csv`, `Cargo.toml`, `.cargo/config.toml`, `justfile`, `Makefile`, `docs/DEVELOPMENT.md`.  
**No benchmarks or Rust compilation were executed in this docs-only review.**

## Goal: shorter feedback without cheaper evidence

Bonsai already has **sccache**, **mold**, Rust 1.95, scoped `just` recipes, a Rust library + binaries, and build-time logs. The bottleneck is **compilation/codegen/linking the large `bonsai` package and duplicate target trees**—not the duration of most test bodies. We will optimize the *critical path and number of artifact builds*, not evade mandatory correctness/security/live acceptance.

The fastest workflow is **change-scope-aware verification, reused warmed artifacts, one controlled build owner, and small pure-Rust crates for future parsers**. Never skip a required gate because it is slow; collect and report fast feedback during iteration, run one definitive gate before merge, and reserve expensive release/lab runs for changes that need them.

### What the repository actually reports

Measurements in `docs/build-times/runs.csv` (various snapshots, 9–11 November; **not directly comparable without same profile/host/cache**):

| Work | Reported observation | Implication |
|---|---|---|
| Warm `cargo check -p bonsai --lib` | often ~7–35 seconds | Preserve the warmed workspace, use for first Rust typecheck |
| `cargo test -p bonsai --lib` focused test name | ~90–157 seconds on some warm-ish runs | A one-name filter can still compile and link a whole libtest executable |
| First `bonsai` integration target | ~9–15 minutes, sometimes much longer | A focused `--test` still builds heavy `bonsai` dependencies + test binary |
| First compile in an independent worktree | ~3,300 seconds with ~53% sccache hits | Separate `target` trees can cause C++/codegen rebuilds and disk exhaustion |
| Debug `bonsai-routing` build | ~285–1,115 seconds | Linking/codegen may dominate even with mold |
| Repeated Docker release rebuilds | ~360–570 seconds | Docker release builds should not be the default inner loop |
| Separate worktree out-of-space failure | recorded `No space left on device` | Resource policy is a correctness/availability issue |

The 23-minute “cold build” and other old figures in `docs/build_performance.md` predate current features/profiles; treat as historical, **not current promises**. Recent build logs include approximate `~` timings and occasional combined commands: never infer median/percentiles from these alone.

## 1. Before starting a task: select the cheapest truthful lane

Check `git status --short`, worktree ownership, free disk, configured compiler/profile/cache and whether another agent is already compiling. Do **not** touch the BN-009 soak CSV or any uncommitted task artifacts. If existing compile/test is running, **do not start another identical compile**; reuse its result once the commit/profile match.

Use the following **progressive verification ladder**. Gates are cumulative at review time, not substitutes.

| Lane | When | Command examples | What this proves / does not prove |
|---|---|---|---|
| **L0 — no-link local checks** | Every edit, immediate feedback | `cargo fmt --all -- --check`; `scripts/check_wiring.sh` if relevant; targeted Python `pytest`, Svelte type/lint checks when available | Formatting/wiring; **not** Rust compilation, routing correctness or live support |
| **L1 — cheap Rust type check** | After meaningful Rust edits | `cargo check -p rbmp-core`, `cargo check -p rbmp-rib`, or `cargo check -p bonsai --lib` | Typecheck; no test execution, final link or runtime proof |
| **L2 — compile and execute changed behavior** | Before opening PR | `cargo test -p rbmp-core --lib <filter>`; `cargo test -p bonsai --test <exact_target>`; Python focused tests | Specific behavior with assertions; exact target may still be costly first run |
| **L3 — targeted regression matrix** | Before PR merge, based on impact | Existing `just test-protocol` / `just test-replay` or relevant specific integration test names | Neighboring contracts; does not automatically prove full workspace or production |
| **L4 — broader/release/live gates** | CI/main-line promotion, routing/security change, release, lab/ops claim | `just verify-release`, selected full workspace `cargo test --workspace`, `cargo build --release --bin bonsai-routing`, isolated containerlab/browser | Required when named by backlog; do not relabel a mock as live |

**Use the existing tests before creating more integration-test binaries.** A new `tests/foo.rs` file is a separate executable and can force another substantial `bonsai` link. Prefer a well-scoped test in an already relevant crate or integration target when isolation stays clear; do not crowd unrelated tests to game build time.

**Important:** `cargo test -p bonsai --lib my_test` does **not** avoid building the large libtest binary. Test filtering reduces **test execution**, not the first compile/link. `cargo nextest` likewise accelerates/distributes execution but **does not eliminate compilation**.

## 2. Repeatable fast commands (do not change global build flags casually)

~~~bash
# Inspect current environment/cache before experimenting.
git status --short
git worktree list
df -h .
df -i .
nproc
free -h
rustc -Vv
cargo -V
sccache --show-stats

# Fastest affected-package feedback (examples; choose real affected package).
cargo fmt --all -- --check
cargo check -p rbmp-core --offline
cargo test -p rbmp-core --lib --offline

# For main Bonsai code: typecheck first; one explicitly named integration target.
cargo check -p bonsai --lib --offline
cargo test -p bonsai --test bn021f_hardening --no-run --timings --offline
cargo test -p bonsai --test bn021f_hardening --offline

# For a code change touching several regression suites, batch *only* those suites
# in ONE cargo invocation to avoid multiple Cargo startup/build graph passes.
cargo test -p bonsai \
  --test bn021b_trusted_identity \
  --test bn021c_exporter_isolation \
  --test bn021f_hardening --offline

# Only when actual release/lab evidence is required:
cargo build --release --bin bonsai-routing --locked
~~~

The first pass may require registry/network or a missing fixture; `--offline` is appropriate **only with pre-fetched dependencies**. A failed “offline missing crate” is a dependency/cache issue, not passing test evidence. Keep the existing Rust toolchain/profile/`RUSTFLAGS` stable across ordinary builds; each material change can invalidate Cargo artifacts.

**Precompile `--no-run`** only when it helps separate build-vs-test wall time or intentionally prepares a later test session; it does **not reduce total compilation**. `--timings` writes an HTML report at `target/cargo-timings/cargo-timing.html` for the selected target directory. Track CPU/RSS separately with `/usr/bin/time -v`. Do not append an invented test pass on `--no-run`.

## 3. Cache and worktrees: stop paying for “clean” compilation repeatedly

1. **One warmed primary target tree for routine dev.** The existing repo `target/` is costly to regenerate. Never `cargo clean`, delete all `target` directories, or `docker builder prune -af` during another agent's work or without inspecting artifact ownership/space.
2. **One active Rust compiler lane per shared target.** Concurrent Cargo processes using the same target directory contend on build locks. Multiple worktrees with individual targets may run concurrently, but duplicate the ~600-dependency/link footprint and caused real ENOSPC. Coordinate an owner/queue for heavy compiles; other agents can edit/docs/review/use lightweight crate tests.
3. **Avoid path/flag churn.** Keep pinned `rust-toolchain.toml`, `Cargo.lock`, enabled Cargo features, compile profile and linker options fixed for the sprint. Running tests with `--release` after a day of `dev` compiles pays for a **separate** profile.
4. **Cross-worktree reuse must be tested, not assumed.** Cargo's `CARGO_TARGET_DIR` can point to a common target, but shared output induces build-lock contention; profile/cfg/path differences still cause recompiles. Experiment with **one deliberately shared build owner** only when inputs/toolchain match. Default for independent or untrusted worktree changes: isolate then merge and test on warmed primary workspace. Avoid symlinking/deleting live target trees.
5. **sccache is not magic for local incremental crates.** Configured as `rustc-wrapper="sccache"`. Incrementally compiled crates are generally **not sccache-cacheable**; ordinary Cargo incremental compilation may speed edits to the main crate, while sccache accelerates cacheable dependencies across worktrees/clean builds. Switching `CARGO_INCREMENTAL`/`SCCACHE_ALLOW_INCREMENTAL` modes or `SCCACHE_BASEDIRS` changes cache behavior and may incur a one-time rebuild. **Benchmark in an isolated experiment**, never flip these repeatedly per command or turn off the mandated wrapper.
6. **Measure sccache misses/rebuilds.** Record `sccache --show-stats` before/after, compile profile, `RUSTFLAGS`/linker, Rust version, target dir, changed crate, cache warmth and disk. Diagnose expensive link as link; cache hits on dependencies do not cache the final `bonsai`/`bonsai-routing` executable.
7. **Avoid destructive “cold benchmark” recipes.** `make build-time` currently runs `cargo clean`. Do **not** use it on the production-shaped primary worktree. For an approved cold benchmark, use a new **quota-bounded temporary build directory** without touching other agents' targets; reserve sufficient disk, include cleanup approval and record conditions.

### Proposed **experiments**, not immediate production settings

The current `Cargo.toml` has `[profile.dev] opt-level=1, incremental=true, debug=1, split-debuginfo="packed"`, while `[profile.dev.package."*"] opt-level=3, codegen-units=256` optimizes **every dependency** at level 3 even in dev/test. That can make cold builds expensive; it may also be justified for lbug/Arrow/Parquet runtime/lab speed. Measure before changing it:

- **Experiment A (profile):** compare current baseline versus a separate dev-only profile with dependencies at `opt-level=0` or `1` and selectively retain `opt-level=2/3` for performance-sensitive crates *only if runtime fidelity requires it*. Measure cold/time, warm/time, disk, test runtime and functional equivalence. Do not casually change the global profile; it will invalidate the existing warmed cache.
- **Experiment B (codegen/link):** benchmark `codegen-units` and `debug` settings for the large lib/bin against current mold settings. `LBUG_SHARED=1` is a documented optional shared-lbug mode in `.cargo/config.toml`; only benchmark in an isolated lab with verified library path/runtime loader, ABI, licensing/package and security parity. Never deploy a debug-only dynamic-link workaround as the release default.
- **Experiment C (concurrency):** measure `cargo -j <N> ...` at 2–3 bounded job limits against default, with peak RSS, CPU pressure, page cache, swap/IO and ENOSPC. More compilation processes can make slow builds **slower** on memory-constrained hosts.
- **Experiment D (dependency/feature):** inspect Cargo timings for LBUG C++/CMake, proc macros, codegen and long serial dependency edges; use `cargo tree -d` and feature analysis to propose *targeted*, correctness-preserving dependency reductions. No broad dependency upgrades or unsupported “disable default features” claims.

**Optimization criteria:** compare like-for-like host/toolchain/feature/profile/revision, run repeated warm builds and a controlled isolated cold baseline. Set numerical budgets **from measured results** before implementing a global change. Reject changes that improve an easy command but regress real security/runtime tests or inflate disk enough to cause ENOSPC.

## 4. High leverage architecture change: cheap independently testable Rust crates

The most durable acceleration for BN-024 is not another linker: **keep new pure algorithms outside the giant `bonsai` dependency graph.** The current workspace has three small `rbmp-*` crates but `bonsai` itself brings graph DB, Arrow/Parquet, HTTP, TLS, auth, ML/AI and test dependencies.

For each future native-normalization/config-semantics slice, propose a **small pure-data crate with minimal dependencies**, e.g. a future `crates/bonsai-net-model` or `crates/bonsai-config-semantics` only after actual BN-024 interface review. The crate should depend on stable typed input/evidence interfaces and not `lbug`/`axum`/`parquet`/full `bonsai`. Feature conformance tests, property tests and fuzzing should run within this crate; Bonsai retains **a small number of integrated graph/identity/LLM boundary tests** against the full product.

**Proof gate:** edit one small parser or policy file → run crate-local compile+test without linking `bonsai` or rebuilding `liblbug`; demonstrate cargo timings and compared runtime vs the same test embedded in Bonsai. No circular dependency, forked schema, duplicate canonical identity or separate graph writer. The crate split is not justified until measured; do not break shared types or backend correctness to make CI green.

## 5. CI/test architecture: compile once, verify many targets

There is **no committed `.github/workflows/`** in the inspected tree. `justfile` has focused test and release recipes; `Makefile`'s `test-all` currently permits failures through `|| true`, and `test-integration` can report success despite missing tests. **These are not a trustworthy green CI gate.**

Propose an explicit CI DAG in a separate bounded implementation PR (BN-019-B6): metadata/format/wiring and UI/Python checks in parallel; Rust small-crate tests fast; one build of the heavyweight Bonsai test binaries using pinned Rust/profile/flags; partition *execution* without recompiling on every shard; then named security/replay/integration and release/lab acceptance gates. Use `cargo-nextest` only if installed/version-pinned and test semantics fit; it can archive test binaries and distribute execution, but compilation and dynamic dependencies still need validation.

- **Never use `|| true` or suppressed stderr** to create a green mandatory test job.
- **No unverified test selection.** Maintain path-to-target mapping *and fallback conservative scope* when shared identity/graph/protocol modules change. CI must fail for unexecuted required gates, and report not-run separately.
- **Do not make PRs wait for a full release Docker image** for pure parser unit tests. Build heavy images once for release/lab promotion on the same source SHA, capture digest and provenance.
- **Remote caches and distributed compilation are conditional.** sccache supports remote storage/distributed builds; requires credentials, trusted cache isolation, tamper protections and measured ROI. Do not expose secrets in cache keys/artifacts or spin up public build nodes by default. Start with local profiling + artifact reuse.

## 6. Decision and evidence record for Devin

For each optimized command or profile change, attach to the BN-019 evidence directory:

~~~text
commit | host CPU/RAM/disk | rustc/cargo | profile | features | linker
target dir | worktree | jobs | RUSTFLAGS | sccache before/after
command | wall | peak RSS | delta disk | compilation vs execution | result
cargo timing HTML path | test count / skipped / failures | lab provenance
~~~

Add one normal row to `docs/build-times/runs.csv` using the current schema. Store extended measurements in a new dated evidence README/CSV, not by changing historical timings. For a PR, include a short **slowest three build units**, critical path, cache misses, peak RSS, before/after comparison, correctness gates and rollback plan.

### Practical scheduling policy

- **Per edit:** L0 + relevant L1; run only relevant small-crate unit tests while coding.
- **At stable checkpoint:** compile required test binary **once**, run multiple test cases from it without touching sources or build flags; capture issues.
- **Before requesting review:** L2 + risk-based L3, static source audit and all named acceptance gates; do not hide failures.
- **Before merging/promotion:** authoritative CI/routing/security/soak gates appropriate to changed code; if not run or blocked, leave PR open or explicitly label incomplete.
- **Avoid serializing *thinking* behind compiling:** when one bounded build is running, another agent can prepare fixtures, docs, code review or next task **without launching a second heavyweight Cargo job or editing the under-test source**.
- **No unattended destructive cleanup:** never delete protected `soak-metrics.csv`, running lab evidence, other worktrees, actively used `target`, volumes, or Docker build cache without owner approval.

## 7. Research and source references (verified 11 November 2026)

- Rust Cargo [timing reports](https://doc.rust-lang.org/cargo/reference/timings.html): stable `cargo ... --timings` and compilation critical path.
- Rust Cargo [profile options](https://doc.rust-lang.org/cargo/reference/profiles.html): dev/test profile inheritance, optimization/codegen/incremental trade-offs.
- Rust Cargo [build cache and target directory](https://doc.rust-lang.org/cargo/reference/build-cache.html): artifact reuse and `CARGO_TARGET_DIR`.
- Rust Cargo [test selection / no-run](https://doc.rust-lang.org/cargo/commands/cargo-test.html): filtered execution does not skip building selected test binaries; `--no-run` and `--timings`.
- Mozilla [sccache Rust caveats](https://github.com/mozilla/sccache/blob/main/docs/Rust.md): incremental builds and linkable targets cannot be fully cached.
- Mozilla [sccache repository and configuration](https://github.com/mozilla/sccache/blob/main/README.md): supported local/remote caching, path normalization, distributed-build tradeoffs.
- [cargo-nextest archive/reuse](https://nexte.st/docs/ci-features/archiving/), [execution partitioning](https://nexte.st/docs/ci-features/partitioning/): compile once, run in multiple partitions where appropriate.

**Invariants:** the purpose is to reduce unproductive waiting **without reducing test integrity, user/data security, authoritativeness, or release evidence**. Every change must report what was measured, what was actually executed and what was deliberately deferred.
