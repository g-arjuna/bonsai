# Laptop Setup (Ubuntu ops environment)

> CV7 T2-1. The laptop runs **bonsai + the rules sidecar as two foreground/background
> processes** under a single startup wrapper. This page is the operator quickstart.

## Prerequisites

- Ubuntu 22.04+ (host) with containerlab installed (for the network lab itself, not bonsai).
- Rust toolchain (interim — pre Tier 6 CI/CD). `rustup install stable` is enough.
- Python 3.11+ with a repo-local `.venv` containing `bonsai_sdk` deps.
- Docker (for external infra: Splunk, Elastic, NetBox, Prometheus). NOT for bonsai itself.

## First-time bring-up

```bash
git clone <repo> && cd bonsai
# 1. Build bonsai (interim — Tier 6 CI/CD will replace this with a binary install)
cargo build --release

# 2. Python deps for the sidecar
python3 -m venv .venv && source .venv/bin/activate
pip install -e python/

# 3. Bring up the network lab + external infra
sudo containerlab deploy -t lab/dc/bonsai.clab.yml
docker compose -f docker/compose.yml up -d   # Splunk/Elastic/NetBox/Prometheus

# 4. Start bonsai + sidecar via the wrapper
bash scripts/ops/start_bonsai_with_sidecar.sh
```

Open:
- bonsai UI: `http://localhost:3000/` (live network graph, topology, events)
- bonpy UI:  `http://localhost:3000/bonpy/` (sidecar status, rule firing, ML model panel)
- sidecar registry JSON: `http://localhost:3000/api/sidecars`
- liveness: `http://localhost:3000/health` (returns `degraded` if `BONSAI_REQUIRE_SIDECAR=rules` and no rules sidecar)

## Day-to-day

```bash
# Start (foreground — Ctrl-C tears down both)
bash scripts/ops/start_bonsai_with_sidecar.sh --foreground

# Start (background — returns immediately)
bash scripts/ops/start_bonsai_with_sidecar.sh

# Stop both, leave lab and external infra running
bash scripts/ops/teardown.sh

# Stop EVERYTHING (also destroys containerlab + docker compose)
bash scripts/ops/teardown.sh --full
```

Logs:
- `logs/bonsai.log` — Rust core output (tracing).
- `logs/bonsai-sidecar.log` — Python sidecar output.

PID files: `runtime/bonsai.pid`, `runtime/bonsai-sidecar.pid`.

## Verifying the sidecar is bound

```bash
curl -s http://localhost:3000/api/sidecars | jq
```

You should see one entry with `"kind": "rules"`, `"status": "healthy"`, and
`last_heartbeat_ns` within the last 15 seconds. If nothing is registered the
banner on the bonpy UI is red and `/health` returns 503 (when
`BONSAI_REQUIRE_SIDECAR=rules` is set, which the startup wrapper sets by default).

## Rebuild + validate cycle

The recommended iteration loop is:

```bash
bash scripts/ops/teardown.sh
git pull origin main
bash scripts/ops/rebuild_and_validate.sh
```

`rebuild_and_validate.sh` rebuilds, runs the smoke suite, captures results into
`docs/test_results/cv7-validation-<date>.md`, and pushes them back to `main` so
Mac-side iteration sees them.

## What this replaces

- The older `scripts/start_bonsai.sh` / docker-compose-for-bonsai paths used in
  earlier CVs. CV7 commits to **bare process on laptop, systemd on cloud**;
  containers are for the network lab and external infra only.
- The dual-mode systemd detection block in `scripts/chaos_runner.sh` (now
  laptop-only per T2-3).

See [`dev_vs_ops_boundary.md`](dev_vs_ops_boundary.md) for the broader
environment rules and the 2026-05-14 ADR in `DECISIONS.md` for the sidecar
visibility rationale.
