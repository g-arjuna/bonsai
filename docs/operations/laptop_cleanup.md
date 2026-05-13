# Laptop Cleanup Runbook — CV5 T1-1

Brings the laptop WSL environment from "current possibly-messy state" to "ready to start CV5 fresh."
Run from WSL where ContainerLab and Docker run.

## Prerequisites

- Running from WSL (not Windows)
- `sudo` available (needed for `containerlab destroy`)
- Current directory: repo root or anywhere (script auto-detects repo root)

## Quick Start

```bash
# Dry run first — see what needs cleaning
bash scripts/cleanup_laptop.sh --verify

# Full teardown + backup
bash scripts/cleanup_laptop.sh
```

## What the Script Does

### Step 1 — Cron removal
Calls `scripts/install_cron.sh --remove` to remove the `bonsai-daily-check` crontab entry.

### Step 2 — Stop bonsai processes
```bash
sudo systemctl stop bonsai          # in case it's installed as a service
pkill -9 -f "target/release/bonsai" # direct binary
pkill -9 -f chaos_runner            # chaos daemon
rm -f runtime/chaos_runner.pid      # stale PID file
```

### Step 3 — Destroy ContainerLab labs
Runs `sudo containerlab destroy -t <topology>` for every known topology:
- `lab/dc/dc-evpn-srv6.clab.yml`
- `lab/sp/sp-mpls-srte.clab.yml`
- `lab/fast-iteration/multivendor.clab.yml`
- `lab/fast-iteration/bonsai-phase4.clab.yml`
- `lab/fast-iteration/3node-srl.clab.yml`

Non-zero exit from destroy is tolerated (lab may already be down).

### Step 4 — Tear down Docker Compose stacks
```bash
docker compose -f docker/compose-external.yml down -v
docker compose -f docker-compose.yml down -v
docker compose -f docker/compose-netbox.yml down -v
```
The `-v` flag removes named volumes. This is intentional — CV5 starts with clean volumes.

### Step 5 — Back up runtime state
Moves runtime dirs/files to dated backups (never deletes):
```
runtime/archive       → runtime/archive.precv5-<timestamp>
runtime/logs          → runtime/logs.precv5-<timestamp>
runtime/driver_results → runtime/driver_results.precv5-<timestamp>
runtime/chaos_log.jsonl → runtime/chaos_log.jsonl.precv5-<timestamp>
runtime/chaos_runner.log → runtime/chaos_runner.log.precv5-<timestamp>
runtime/bonsai.db.local  → (if present) runtime/bonsai.db.local.precv5-<timestamp>
```

The pre-CV5 archive data is preserved. Review `runtime/archive.precv5-*/` for any labeled
injection data worth keeping before the next cleanup.

### Step 6 — Verification (always runs)
Prints state summary:
- Docker containers matching bonsai/clab/netbox/splunk/elastic/grafana/prometheus
- ContainerLab inspect output
- ps check for bonsai/chaos_runner processes
- runtime/ directory listing
- Remaining crontab entries

## Done Criteria

The cleanup is complete when verification shows:
- [ ] `No bonsai/chaos_runner processes`
- [ ] `docker ps` shows no bonsai/clab containers
- [ ] `containerlab inspect` reports no active labs
- [ ] `crontab -l` shows no bonsai entries
- [ ] `runtime/` contains only `.precv5-*` backup dirs (no active logs/archive/driver_results)

## Rollback / Restore Notes

The backup dirs created by this script (`*.precv5-<timestamp>`) are the restore point.
To restore runtime state:

```bash
# Example restore (adjust timestamp):
TS=1747045200
mv runtime/archive.precv5-${TS} runtime/archive
mv runtime/logs.precv5-${TS} runtime/logs
mv runtime/driver_results.precv5-${TS} runtime/driver_results
```

The pre-CV5 freeze (`scripts/precv5_freeze.sh`, see `docs/operations/precv5_freeze.md`)
also tarballs these backup dirs for a single-file restoration point.

## Notes

- The primary bonsai.db on Windows lives outside WSL at the path configured in bonsai.toml.
  This script only moves the `.local` variant if present in WSL's runtime/.
- `containerlab destroy` requires `sudo`. If sudo is unavailable, destroy containers manually:
  `docker ps -a | grep clab | awk '{print $1}' | xargs docker rm -f`
- Docker named volumes removed by `down -v` cannot be recovered from this script.
  If you need the Splunk/Elastic data from a previous cycle, back up volumes first with
  `scripts/backup_volumes.sh` before running cleanup.
