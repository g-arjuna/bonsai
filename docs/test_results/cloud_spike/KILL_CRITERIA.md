# Cloud Spike — Kill Criteria

## Spike definition

A 5-day time-boxed evaluation of Oracle Always Free ARM (4 OCPU, 24 GB RAM)
as a continuous chaos data accumulation host for bonsai GNN training.

Provisioned: _<fill in when live>_
Kill-or-continue decision: _<provisioned date + 5 days>_

---

## Kill conditions (any one → kill)

| # | Condition | Measurement | Action if triggered |
|---|---|---|---|
| K-1 | Bonsai crashes >1× in any 24h window | `journalctl -u bonsai --since "24h ago" \| grep "panicked\|exited"` | Kill immediately |
| K-2 | ContainerLab topology down >4h continuously | `containerlab inspect` shows 0 running nodes | Kill if not recoverable in 1h |
| K-3 | Daily sync fails ≥2 consecutive days | `.synced-<date>` marker absent in `/mnt/bonsai-archive/snapshots/` | Kill after 2nd miss |
| K-4 | Free-tier resource ceiling hit | OCI console shows throttling or quota exceeded alert | Kill — cannot fix without billing |
| K-5 | Archive at day-5 contains <500 unique fault events | `python3 scripts/compute_detection_baselines.py --dry-run \| grep "Fault records"` | Kill — insufficient training signal |
| K-6 | Cloud archive signal indistinguishable from laptop | Manual review of detection_baselines.md: F1 variance <5% vs laptop baseline | Continue on laptop only |

---

## Continue conditions (all must hold)

- Zero K-1 through K-4 triggers across all 5 days
- ≥500 unique fault events accumulated
- Archive integrity check green every day (verify_archive.sh exit 0)
- bonsai p99 response latency <500ms on /api/topology (check system_snapshot.txt)
- At least one detection baseline report generated with >0 TP detections

**If continue**: extend to 30-day run. Day 30 archive feeds GNN training (T6-1).

---

## Evaluation checklist (day 5)

Run this on the VM to collect the decision inputs:

```bash
bash /opt/bonsai/scripts/cloud/daily_sync.sh --force
bash /opt/bonsai/scripts/verify_archive.sh /mnt/bonsai-archive/archive --json
python3 /opt/bonsai/scripts/compute_detection_baselines.py \
    --chaos-dir /opt/bonsai/chaos_runs \
    --archive-dir /mnt/bonsai-archive/archive \
    --dry-run
journalctl -u bonsai --since "5 days ago" | grep -c "panicked\|exited" || echo "0 crashes"
df -h /mnt/bonsai-archive
```

Fill in the spike report at `docs/test_results/cloud_spike/<date>.md` with findings.

---

## Teardown procedure (if killed)

```bash
# On VM: stop everything
sudo systemctl stop bonsai
bash /opt/bonsai/scripts/chaos_runner.sh --stop
sudo containerlab destroy --topo /opt/bonsai/lab/cloud-dc-6node.yml --cleanup || true
docker compose -f /opt/bonsai/compose-external.yml down

# On laptop: destroy OCI resources
bash scripts/cloud/oracle_setup.sh --destroy
```

Document findings in `docs/test_results/cloud_spike/<date>.md` before destroying.
