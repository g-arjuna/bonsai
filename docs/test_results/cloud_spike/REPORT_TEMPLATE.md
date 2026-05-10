# Cloud Spike Report — <YYYYMMDD>

> Copy this template to `<start-date>.md` and fill in during/after the spike.
> Delete all `<placeholder>` markers before committing.

---

## Summary

**Outcome**: [ ] Continue (extend to 30 days)  |  [ ] Kill (return to laptop-only)

**Spike dates**: _start_ → _end_  
**VM**: Oracle Always Free ARM — 4 OCPU, 24 GB RAM, 200 GB block storage  
**Region**: `<OCI_REGION>`

Boundary reminder: the cloud spike is for archive accumulation and operational
stability only. GNN training guidance lives in `docs/cloud_lab.md`.

---

## What was deployed

| Component | Version / Config |
|---|---|
| bonsai binary | `<git sha>` |
| ContainerLab topology | `lab/cloud-dc-6node.yml` |
| External infra | NetBox + Prometheus (Splunk/Elastic disabled) |
| Chaos plan | `chaos_plans/always_on_dc.yaml` |
| Archive storage | `/mnt/bonsai-archive` — <N> GB data volume |

---

## Stability

### Bonsai crashes

| Day | Crash count | Notes |
|---|---|---|
| Day 1 | | |
| Day 2 | | |
| Day 3 | | |
| Day 4 | | |
| Day 5 | | |

**Total crashes**: _N_  
**K-1 triggered?**: [ ] Yes  [ ] No

### ContainerLab uptime

| Day | Outages | Longest outage | Recovered? |
|---|---|---|---|
| Day 1 | | | |
| Day 2 | | | |
| Day 3 | | | |
| Day 4 | | | |
| Day 5 | | | |

**K-2 triggered?**: [ ] Yes  [ ] No

### Daily sync

| Day | Status | Snapshot size | Notes |
|---|---|---|---|
| Day 1 | | | |
| Day 2 | | | |
| Day 3 | | | |
| Day 4 | | | |
| Day 5 | | | |

**K-3 triggered?**: [ ] Yes  [ ] No

---

## Resource utilisation

_From `system_snapshot.txt` on day 5 (or most recent)._

| Metric | Day 1 | Day 3 | Day 5 |
|---|---|---|---|
| CPU avg % | | | |
| RAM used (GB) | | | |
| Disk used (GB / 200 GB) | | | |
| bonsai RSS (MB) | | | |
| clab node RAM total (GB) | | | |

**K-4 triggered?**: [ ] Yes  [ ] No

---

## Archive quality

### Volume

| Day | Parquet files | Total rows | Compressed size |
|---|---|---|---|
| Day 1 | | | |
| Day 2 | | | |
| Day 3 | | | |
| Day 4 | | | |
| Day 5 | | | |

**Total fault events**: _N_  
**K-5 triggered (<500)?**: [ ] Yes  [ ] No

### Detection baselines (day 5)

_Paste output of `compute_detection_baselines.py --dry-run` here._

```
<paste>
```

**K-6 triggered (F1 variance <5% vs laptop)?**: [ ] Yes  [ ] No

**Laptop F1 baseline** (from `docs/test_results/detection_baselines/latest.md`):

| Rule | Laptop F1 | Cloud F1 | Delta |
|---|---|---|---|
| | | | |

---

## What worked

- 
- 

## What didn't work

- 
- 

## Surprises

- 

---

## Recommendation

### If continuing:
- Extend chaos runner to 30 days
- Archive sync continues on cron
- No code changes required
- Trigger condition for GNN: day 30 archive passes `verify_archive.sh` with ≥5000 rows

### If killing:
- Document specific failure mode
- Tear down VM (`oracle_setup.sh --destroy`)
- Return to laptop-only chaos accumulation
- Revise kill criteria for next spike attempt

---

## Actions

| Action | Owner | By |
|---|---|---|
| | | |

---

_Report generated from `docs/test_results/cloud_spike/REPORT_TEMPLATE.md`_
