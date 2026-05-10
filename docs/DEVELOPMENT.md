# Development Environment

## Python And Lab Workflow

Bonsai uses a native Ubuntu/Linux workflow for Rust, Python, and live lab operations.

- The source of truth for Python dependencies is [python/pyproject.toml](/home/arjuna/Desktop/bonsai/python/pyproject.toml:1).
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

## Daily Commands

From Linux:

```bash
cd /home/arjuna/Desktop/bonsai
source .venv/bin/activate
python scripts/chaos_runner.py chaos_plans/baseline_mix.yaml --duration-hours 0.03
python python/inject_fault.py bgp-flap srl-spine1 10.0.12.1 --hold 10
cargo build --release
cargo test --release
cargo clippy --release -- -D warnings
```

Set `BONSAI_CONFIG` to point a process at a non-default config file. This is
useful for distributed validation where a core and collector run side by side
with separate working directories and separate `bonsai.toml` files.

The repo-local `.cargo/config.toml` may use `sccache` as a Rust wrapper for
faster rebuilds. If `sccache` is unavailable in your environment, temporarily
clear it for a command with `RUSTC_WRAPPER= cargo ...`.

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
- The documented release build flow remains `cargo build --release`, `cargo test --release`, and `cargo clippy --release -- -D warnings`.
