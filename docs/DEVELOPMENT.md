# Development Environment

## Python And Lab Workflow

Bonsai uses a WSL-first workflow for Python tooling and live lab operations.

- The source of truth for Python dependencies is [python/pyproject.toml](/C:/Users/arjun/Desktop/bonsai/python/pyproject.toml:1).
- Create a project-local virtual environment at `.venv/` from inside WSL.
- Run `scripts/chaos_runner.py`, `python/inject_fault.py`, and any `clab tools netem` commands from WSL, because the ContainerLab topology and `clab` binary live there.
- Keep `bonsai.toml` in the repo root so both Rust on Windows and Python in WSL read the same target inventory.

## First-Time Setup

From WSL:

```bash
cd /mnt/c/Users/arjun/Desktop/bonsai
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

From WSL:

```bash
cd /mnt/c/Users/arjun/Desktop/bonsai
source .venv/bin/activate
python scripts/chaos_runner.py chaos_plans/baseline_mix.yaml --duration-hours 0.03
python python/inject_fault.py bgp-flap srl-spine1 10.0.12.1 --hold 10
```

From Windows PowerShell:

```powershell
cargo build --release
cargo test --release
cargo clippy --release -- -D warnings
```

## Canonical Local Helpers

Use these scripts instead of ad hoc PATH-dependent commands:

```powershell
# Verify Windows Python, real ripgrep, Cargo, WSL .venv, clab, and Bonsai readiness.
powershell.exe -ExecutionPolicy Bypass -File scripts\check_dev_env.ps1

# Search the repo without hitting the broken Chocolatey rg shim.
powershell.exe -ExecutionPolicy Bypass -File scripts\search_repo.ps1 DiscoverDevice src proto

# Regenerate committed Python gRPC stubs after editing proto/bonsai_service.proto.
powershell.exe -ExecutionPolicy Bypass -File scripts\regenerate_python_stubs.ps1

# Start/stop Bonsai from a normal Windows PowerShell.
powershell.exe -ExecutionPolicy Bypass -File scripts\start_bonsai_windows.ps1
powershell.exe -ExecutionPolicy Bypass -File scripts\stop_bonsai_windows.ps1
```

`scripts\start_bonsai_windows.ps1` is intended for a normal user PowerShell when Bonsai
needs to stay running. Codex shell commands can use it for short smoke tests, but the
desktop sandbox may clean up child processes after a tool call returns.

## Windows vs WSL Boundary

- Windows owns the Rust core process: `cargo build --release`, `cargo test --release`,
  `cargo clippy --release -- -D warnings`, and the long-running `target\release\bonsai.exe`.
- WSL owns the live lab: `clab`, `netem`, chaos plans, and lab-side fault injection.
- Python SDK/lab dependencies live in WSL `.venv/`; however, protobuf stub generation can
  use the Windows Python fallback via `scripts\regenerate_python_stubs.ps1`.
- WSL clients may call Bonsai on Windows at `127.0.0.1:50051` / `127.0.0.1:3000` when the
  Windows process is running and reachable. If a call fails, first check Windows Bonsai
  readiness before debugging Python code.

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

Current T1-2 boundary:

- `all` is still the normal Windows workflow for this machine.
- `collector` should run wherever the gNMI targets are reachable.
- collector-local archive is supported when `[archive].enabled = true`.
- gRPC zstd compression, disk-backed outage queue, and collector/core mTLS remain follow-up slices.

## Why This Split Exists

- `clab` and the live ContainerLab lab run inside WSL, so Windows-hosted Python cannot reliably drive `netem` or other lab-side tooling.
- A repo-local `.venv/` keeps Python packages reproducible and isolated from Codex runtime bundles or machine-global interpreters.
- Rust stays on the existing Windows `--release` workflow because that is already the documented and validated path for this machine.
