# Development Environment

## Python And Lab Workflow

Bonsai uses a native Ubuntu/Linux workflow for Rust, Python, and live lab operations.

- The source of truth for Python dependencies is [python/pyproject.toml](../python/pyproject.toml).
- Create a project-local virtual environment at `.venv/`.
- Run `scripts/chaos_runner.py`, `python/inject_fault.py`, and `clab` commands directly on Linux.
- Keep `bonsai.toml` in the repo root so Rust, Python, and the live lab use the same target inventory.

## First-Time Setup

From Linux:

```bash
cd /home/arjuna/Desktop/bonsai
python3 -m venv .venv
source .venv/bin/activate
python -m pip install --upgrade pip
python -m pip install -e './python[dev,ml]'
```

This installs:

- core SDK/runtime dependencies from `python/pyproject.toml`
- `dev` extras for test tooling
- `ml` extras for Parquet export and training scripts

## Daily Commands — fast feedback first

The **current development contract** is [AGENTS.md](../AGENTS.md), with a
measured, task-by-task [build acceleration playbook](BUILD_ACCELERATION_PLAYBOOK_2026-11-11.md).
The historical release-build commands below are **not** the daily edit loop.
Existing Rust 1.95, sccache and mold are already configured.

From the primary Linux checkout, after verifying no other agent owns a
heavy build and preserving any local/soak work:

```bash
cd /home/arjuna/Desktop/bonsai
git status --short
git worktree list
df -h .
sccache --show-stats

# First feedback: no full link.
cargo fmt --all -- --check
cargo check -p rbmp-core --offline        # when editing the protocol crate
cargo check -p bonsai --lib --offline     # when editing the main application

# Execute a specific owning test; first invocation may link the full lib/test binary.
cargo test -p rbmp-core --lib --offline
cargo test -p bonsai --test bn021f_hardening --offline

# Existing verification recipes; select based on changed semantics.
just check-fast
just test-protocol
just test-replay
```

Use the **smallest correct command**, not every line each time. Running a
`cargo test` filter narrows *execution*, but a first test binary compile/link
can still take many minutes. Keep the same `target`, toolchain, profile and
compile flags for incremental edits. Use the playbook's L0–L4 ladder to
escalate verification; don't confuse `cargo check` with a passing test.

Set `BONSAI_CONFIG` to point a process at a non-default config file. This is
useful for distributed validation where a core and collector run side by side
with separate working directories and separate `bonsai.toml` files.

The checked-in `.cargo/config.toml` **does configure** `sccache` as the Rust
wrapper. Do not bypass it with `RUSTC_WRAPPER= cargo ...`: that contradicts
`AGENTS.md`, changes compiler fingerprints and may cause another expensive
build. If sccache is broken, diagnose and repair it per the agent contract,
then report if unresolved. Its cache is complementary to Cargo incremental
objects; neither skips final binary linking.

For release/promotion evidence, run the full **relevant** release/profile,
integration and UI/lab checks from `justfile` and the owning BN acceptance.
Examples, **not** every-day edit commands:

```bash
just verify-release
cargo build --release --bin bonsai-routing --locked
# cargo test --release --workspace --locked  # only when this exact expensive gate is required
```

Do not blindly invoke `make build-time` in the shared checkout: that target
calls `cargo clean`. Do not treat `make test-all` as a trustworthy hard
gate: it currently has `|| true` paths masking failures. The playbook
documents the planned BN-019 fixes and safer measurement procedure.

## Canonical Validation Gates (BN-019-B6)

`scripts/ci/gates.sh` is the single fail-closed definition of the tiered
gates; `just gate <tier>` and `just gate-all` are thin wrappers, and
`.github/workflows/ci.yml` runs the same script in CI.

| Tier | Scope | Command |
|------|-------|---------|
| L0 | Source checks: rustfmt on changed files (new drift only), python compile, cargo metadata | `scripts/ci/gates.sh l0` |
| L1 | Compile/static: `cargo check --workspace --all-targets --profile ci`, UI build, SDK import | `scripts/ci/gates.sh l1` |
| L2 | Required behavioral: bonsai lib tests, rbmp crates, 22 core integration targets | `scripts/ci/gates.sh l2` |
| L3 | Cross-component: 13 workflow/browser integration targets + python suites | `scripts/ci/gates.sh l3` |
| L4 | Lab/deployment acceptance (containerlab, live NOS, soak) — never implied by L0–L3 | manual, see `gates.sh l4` |

A green gate means the named tests compiled, executed and passed. Missing
tools, missing `--test` targets, build errors and test failures all exit
nonzero. Optional lab suites are reported as skipped, never as passed.

Environment knobs: `GATE_BASE` (fmt-diff base ref, default `origin/main`),
`CARGO_PROFILE` (default `ci`), `SKIP_UI_BUILD=1` (reuse `ui/dist`).

### CI runners

GitHub-hosted `ubuntu-latest` runners cannot build the full Bonsai test
suite: `target/` exceeds 100 GB and parallel test-binary links exceed 16 GB
RAM. The `rust-gates` job therefore requires a **self-hosted runner**
labelled `[self-hosted, linux, x64, bonsai-ci]` with: Rust 1.95.0
(`rust-toolchain.toml`), `sccache`, `mold`, Node 20, a warm cargo registry,
and ~150 GB free disk. The `source-checks`, `python-tests` and `ui-build`
jobs run on hosted runners. Until a self-hosted runner is registered the
rust job stays queued — that is intentional fail-closed behaviour, not a
pass.

#### Isolated runner requirements (CI-R2)

The `bonsai-ci` runner must be a **dedicated disposable or single-purpose
VM** — never the developer workstation, the containerlab host, or any
production-connected machine. Required isolation properties:

- **No production credentials**: no cloud keys, no NetBox/prod tokens, no
  VPN profiles, no SSH keys to network devices.
- **No developer home directory access**: the runner workdir is a fresh
  ephemeral checkout; `$HOME` contains only runner + toolchain state.
- **No production network-device connectivity**: e2e/L3 tests are
  loopback/tempdir/synthetic-cert only; the VM must not route to lab or
  production addressing.
- **No access to the protected soak evidence**
  (`docs/merger/evidence/2026-11-09-bn009-soak-full/soak-metrics.csv`) —
  irrelevant on a clean VM, but reinforces that this host must never be the
  dev machine.
- **No unrestricted host Docker socket**: tests that need Docker are L4 and
  do not run in CI; do not mount `/var/run/docker.sock` into the runner.
- **Minimal GitHub permissions**: runner registration token is single-use,
  short-lived, and must never appear in logs, artifacts, or the repo.
  `GITHUB_TOKEN` keeps default read-only job permissions.
- **Controlled cache reuse**: sccache/GHA cache only; no shared mutable
  cargo registry credentials.
- **Explicit resource limits**: VM sized ≥16 GB RAM (links are the peak),
  ≥150 GB disk for `target/`, CPU quota documented; ephemeral disk is
  wiped between jobs or the VM is rebuilt per CI window.

#### Hosted larger runners vs self-hosted (evaluation)

Measured requirements (BN-019-B6 evidence,
`docs/merger/evidence/2026-11-13-bn019-b6-fail-closed-ci/`):

- `target/` for the ci profile across workspace + all `--test` targets:
  well over 100 GB during L2/L3 (ci-profile test binaries are 34–90 MB
  each but there are ~35 of them plus deps).
- Peak memory during parallel test-binary links exceeds the 16 GB
  GitHub-hosted runner limit; disk exceeds the ~14 GB hosted SSD.

GitHub-hosted **larger runners** (4-core/16 GB, or custom) still cap disk
near hosted levels and cost per-minute on private repos; for a public repo
hosted minutes are free but larger-runner SKUs are billed. A dedicated
self-hosted VM on existing infrastructure is cheaper and satisfies the
isolation list above. **Decision: self-hosted isolated VM.** Provisioning
requires owner approval (new VM, runner registration token); until then the
rust job stays queued — the correct fail-closed state.

#### Known infrastructure blocker (2026-11-14)

GitHub Actions run
[43515987209](https://github.com/g-arjuna/bonsai/actions/runs/43515987209)
at `a911573`: all hosted jobs failed in ~2–3 s with **zero steps and zero
billable minutes**; job annotations read *"The job was not started because
your account is locked due to a billing issue."* This is an account-level
billing lock, not a workflow or test failure — owner action required. The
self-hosted job remains queued (no `bonsai-ci` runner registered).

### Local containerized CI (GitHub-billing workaround)

While hosted runners are unavailable, `scripts/ci/run-local.sh` executes
the SAME canonical `scripts/ci/gates.sh` tiers inside the development-only
image `docker/ci/Dockerfile` (`bonsai-ci:local`):

```bash
scripts/ci/run-local.sh l0 l1 l2 l3   # or: scripts/ci/run-local.sh --build l0
```

The runner snapshots the exact committed HEAD via `git worktree`
(uncommitted changes and protected soak evidence are excluded), then runs
the container with `--memory=24g --cpus=4`, `CARGO_BUILD_JOBS=2`, non-root,
no docker socket and no home-directory mounts. Host toolchain caches
(`~/.cargo`, `~/.rustup`, sccache, Playwright browsers, `target/`) are
bind-mounted at identical paths so fingerprints stay valid and no second
~140 GB target tree is created — verify `df -h` first. Resource metrics and
per-tier results are written under `target/ci/`. This validates the gate
commands and code, NOT GitHub billing, runner registration, hosted
permissions or branch protection — remote CI status must still be reported
truthfully. `actionlint` (or equivalent) should be run against
`.github/workflows/ci.yml` before relying on workflow changes; the workflow
itself is unchanged by this path.

## Canonical Local Helpers

Use these scripts instead of ad hoc PATH-dependent commands:

```bash
# Search the repo quickly.
rg DiscoverDevice src proto

# Regenerate committed Python gRPC stubs after editing proto/bonsai_service.proto.
python -m grpc_tools.protoc -I proto --python_out=python/generated --grpc_python_out=python/generated proto/bonsai_service.proto
```

## Runtime Modes

Bonsai now has the first T1-2 distributed collector seam. The default remains single-process:

```toml
[runtime]
mode = "all"        # subscribes to devices and runs graph/API/UI
collector_id = "local"
core_ingest_endpoint = "http://[::1]:50051"
```

Use `mode = "core"` for a graph/API/UI process that accepts `TelemetryIngest` streams and
does not start local gNMI subscribers. Use `mode = "collector"` for a lab-side process that
subscribes to local gNMI targets and forwards decoded telemetry to `core_ingest_endpoint`.
Collector mode persists decoded telemetry to `[collector.queue]` before forwarding, so a
core outage does not silently drop updates. Defaults write to `runtime/collector-queue`,
retain up to 1 GiB or 24 hours, and log queue size every 30 seconds.
Set `[runtime.tls].enabled = true` on both core and collector to require mTLS
for `TelemetryIngest`; see `docs/distributed_tls.md` for the lab CA flow.

Current T1-2 boundary:

- `all` is the normal local Linux workflow for this machine.
- `collector` should run wherever the gNMI targets are reachable.
- collector-local archive is supported when `[archive].enabled = true`; it writes
  one Parquet file per target per hour during normal operation, closing files at
  hour rollover or graceful shutdown.
- gRPC zstd compression, the disk-backed outage queue, and optional mTLS are
  enabled for collector-to-core ingest.

## Parser Sidecars

CV1 Sprint 2 adds optional parser sidecars for layered ingestion. Native Linux
development can keep the default localhost URLs from `bonsai.toml.example`:

```bash
docker compose --profile parsers up -d
curl http://127.0.0.1:9101/healthz
curl http://127.0.0.1:9102/healthz
```

This profile is intended for the native Linux workflow where Bonsai runs on the
host and the sidecars run in containers. If you later run Bonsai itself inside
Docker, set the sidecar URLs explicitly in that container's `bonsai.toml`
instead of assuming `127.0.0.1` will cross container boundaries.

## Why This Setup Exists

- Native Linux keeps `clab`, `netem`, Rust, and Python in one environment with fewer path and socket surprises.
- A repo-local `.venv/` keeps Python packages reproducible and isolated from machine-global interpreters.
- Full release builds and applicable lint/test gates remain separate, explicit promotion work. Normal development uses the focused, cache-aware commands above; see `AGENTS.md` and `docs/BUILD_ACCELERATION_PLAYBOOK_2026-11-11.md`.
