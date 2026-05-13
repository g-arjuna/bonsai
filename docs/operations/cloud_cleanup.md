# Cloud VM Cleanup Runbook — CV5 T1-2

Brings the OCI cloud VM from "current possibly-messy state" to "ready to start CV5 fresh."

**Run on the cloud VM** (SSH in first). The script lives in the bonsai repo at `scripts/cloud/cleanup.sh`.

## Prerequisites

- SSH access to the cloud VM
- `sudo` available on the VM (needed for `containerlab destroy` and `systemctl`)
- Bonsai installed at `INSTALL_DIR` (default: `/opt/bonsai`)
- The bonsai repo is checked out on the VM (deploy was done via `scripts/cloud/deploy.sh`)

## Quick Start

```bash
# SSH to cloud VM
ssh ubuntu@<cloud-vm-ip>   # or configured SSH alias

# Dry run first
INSTALL_DIR=/opt/bonsai bash /opt/bonsai/scripts/cloud/cleanup.sh --verify

# Full teardown + backup
INSTALL_DIR=/opt/bonsai bash /opt/bonsai/scripts/cloud/cleanup.sh
```

If `INSTALL_DIR` is `/opt/bonsai` (the default), the env var is optional.

## What the Script Does

### Step 1 — Cron removal
Calls `scripts/cloud/install_cron.sh --remove` to remove `bonsai-cloud-sync` and `bonsai-cloud-check` crontab entries.

### Step 2 — Stop bonsai processes
```bash
sudo systemctl stop bonsai
pkill -9 -f bonsai
pkill -9 -f chaos_runner
```

### Step 3 — Destroy ContainerLab labs
- `lab/cloud-dc-6node.yml`
- `lab/sp/sp-mpls-srte.clab.yml` (if present)

### Step 4 — Tear down Docker Compose stacks
```bash
docker compose -f docker/compose-external.yml down -v
docker compose -f docker-compose.yml down -v
docker compose -f docker/compose-netbox.yml down -v
```

### Step 5 — Back up runtime state
Moves to dated backups under `$INSTALL_DIR/runtime/`:
```
runtime/archive        → runtime/archive.precv5-<timestamp>
runtime/logs           → runtime/logs.precv5-<timestamp>
runtime/driver_results → runtime/driver_results.precv5-<timestamp>
runtime/bonsai.db      → runtime/bonsai.db.precv5-<timestamp>  (if present)
```

**Important**: the cloud archive may contain labeled injection data not present on the laptop.
Review `runtime/archive.precv5-*/` for Parquet files before deleting anything.

### Step 6 — Verification (always runs)
Same checks as the laptop cleanup.

## Done Criteria

- [ ] `No bonsai/chaos_runner processes`
- [ ] `docker ps` shows no bonsai/clab containers
- [ ] `containerlab inspect` reports no active labs
- [ ] `systemctl is-active bonsai` returns inactive/not-found
- [ ] `crontab -l` shows no bonsai entries
- [ ] `runtime/` contains only `.precv5-*` backup dirs

## Post-Cleanup: Review Cloud Archive

Before the next CV5 deploy cycle, SSH into the VM and check:

```bash
# How much chaos archive data is on the cloud?
du -sh /opt/bonsai/runtime/archive.precv5-*/
find /opt/bonsai/runtime/archive.precv5-* -name "*.parquet" | wc -l

# If there's meaningful data, download it to the laptop before continuing:
rsync -avz ubuntu@<cloud-vm-ip>:/opt/bonsai/runtime/archive.precv5-*/ \
    ~/Desktop/bonsai/runtime/cloud_archive_precv5/
```

## Known Gotchas (learned 2026-05-12)

**SSH user is `opc`, not `ubuntu`** — the cloud VM uses Oracle Linux, not Ubuntu.
Always SSH as `opc@<ip>`. The `BONSAI_SSH_USER` variable in `scripts/cloud/instance.env`
is now set correctly.

**bonsai.service has `Restart=on-failure`** — killing the process with `kill -9` looks
like a crash and systemd restarts it after 10 seconds. The correct sequence is:
```bash
sudo systemctl stop bonsai    # clean SIGTERM → no restart triggered
sudo systemctl disable bonsai # prevent start on reboot
# kill chaos_runner by PID (not pkill -f which can hit SSH session processes)
PIDS=$(ps aux | grep -E "chaos_runner.sh|chaos_runner.py" | grep -v grep | awk '{print $2}')
kill -9 $PIDS
```
If `systemctl stop` doesn't hold (service in mid-restart cycle), wait 15 seconds and retry
— the `RestartSec=10s` delay means a second `stop` always wins.

**`pkill -f bonsai` kills the OCI PCP monitoring daemon** — the `pmie` process has
`bonsai-cloud-spike` in its log path argument, matching the pattern. Use
`pkill -f "target/release/bonsai"` or kill by PID.

## Notes

- The cloud VM typically runs at OCI Always Free tier: 4 vCPU, 24 GB RAM, 200 GB disk.
- If `containerlab` is not installed on the VM, skip step 3 (the script tolerates the absence).
- `docker compose down -v` removes named volumes. If Splunk/Elastic data needs to be preserved,
  run `scripts/backup_volumes.sh` before cleanup.
- After cleanup, pull the latest repo and re-run `scripts/cloud/deploy.sh` for CV5 bring-up.
