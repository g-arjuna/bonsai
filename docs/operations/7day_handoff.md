# Bonsai 7-Day Hands-Off Operational Proof

## Purpose

Validates that bonsai runs autonomously for 7 days on the cloud VM with no manual
intervention: gNMI ingestion stays live, chaos faults are detected and remediated,
archive snapshots sync to GitHub nightly, and the daily check stays green.

---

## Day 0 — Start Criteria (must all be true before the clock starts)

| Check | How to verify |
|-------|--------------|
| Bonsai service running | `systemctl status bonsai` → active (running) |
| ContainerLab topology up | `sudo containerlab inspect` → all nodes `running` |
| gNMI subscriptions active | `/api/operations` → `observed_subscriptions > 0` |
| Crons installed | `crontab -l` shows `bonsai-cloud-sync` and `bonsai-cloud-check` |
| GITHUB_TOKEN set | `bash scripts/cloud/daily_sync.sh --dry-run` exits 0 |
| Archive mount writable | `df -h /mnt/bonsai-archive` shows headroom |
| Day-0 daily check passes | `bash scripts/bv5_daily_check.sh` exits 0, status `pass` |

Start the clock only when all six checks pass. Record the start timestamp in
`runtime/driver_results/handoff_start.txt`.

---

## Daily Validation (Days 1–6)

Each morning, pull the sync branch and run the daily check remotely:

```bash
# On laptop
git fetch origin
git branch -r | grep sync/cloud-spike   # confirm last night's sync landed

# Check the daily report
cat docs/test_results/daily_runs/$(date -u '+%Y-%m-%d').md
```

The UI Operations page → "7-Day Trend" panel shows pass/fail/skip counts per day.
Open `/api/operations/weekly-trend` for the raw JSON.

### Escalation thresholds

| Condition | Action |
|-----------|--------|
| `fail > 0` in daily check | SSH in and run `bash scripts/bv5_daily_check.sh` manually, check logs |
| Sync branch not updated overnight | Verify `GITHUB_TOKEN` not expired, check `logs/daily_sync.log` |
| `observed_subscriptions` drops to 0 | Restart bonsai: `systemctl restart bonsai`; check ContainerLab |
| RSS > 1.5 GiB | Check `/api/_test/status` for budget breach details |

---

## Day 7 — Acceptance Criteria

All of the following must be true to declare the 7-day run a success:

| # | Criterion | Pass condition |
|---|-----------|---------------|
| 1 | Daily check green | All 7 `daily-*.json` files in `runtime/driver_results/` have `status: pass` or `pass_with_caveats` with `fail == 0` |
| 2 | Archive syncs complete | 7 snapshot tarballs present in the `sync/cloud-spike/*` branches |
| 3 | No service restarts | `journalctl -u bonsai --since "7 days ago" \| grep -c "Starting bonsai"` equals 1 (the initial start) |
| 4 | gNMI subscriptions stable | `/api/operations` shows `silent_subscriptions == 0` at Day 7 check |
| 5 | Detections present | `/api/detections` returns at least 1 event (confirms loop fired during chaos) |
| 6 | Memory within budget | `/api/_test/status` shows `memory_rss_pct_of_budget < 80%` |
| 7 | Archive disk within budget | `/api/_test/status` shows `archive_disk_pct < 80%` |

---

## Collecting the Final Report

```bash
# Pull last 7 daily snapshots
git fetch origin
for d in $(seq 0 6); do
    branch="sync/cloud-spike/$(date -u -d "$d days ago" '+%Y-%m-%d' 2>/dev/null \
        || date -u -v-${d}d '+%Y-%m-%d')"
    git show "origin/$branch:README.md" 2>/dev/null || true
done

# Weekly trend JSON
curl -s http://localhost:3000/api/operations/weekly-trend | python3 -m json.tool
```

Commit the final daily report and close the sprint:

```bash
git add docs/test_results/daily_runs/
git commit -m "ops: 7-day handoff complete — $(date -u '+%Y-%m-%d')"
```

---

## Rollback / Emergency Recovery

```bash
# Hard-restart the full stack
systemctl stop bonsai
sudo containerlab destroy -t /opt/bonsai/lab/cloud-dc-6node.yml
sudo containerlab deploy -t /opt/bonsai/lab/cloud-dc-6node.yml
systemctl start bonsai
sleep 10
bash scripts/bv5_daily_check.sh
```

Archive data is never lost — Parquet files on the archive mount survive restarts.
