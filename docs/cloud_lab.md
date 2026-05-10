# Cloud Lab Boundary

The OCI cloud spike is a long-running archive and chaos host, not a training
host.

## What the cloud lab is for

- Running `lab/cloud-dc-6node.yml` continuously
- Keeping chaos injection active for archive growth
- Capturing Parquet archive data and daily health snapshots
- Validating that Bonsai stays operational over multi-day runs

## What the cloud lab is not for

- GNN training
- GPU-backed experimentation
- Large offline feature engineering jobs
- Acting as the permanent home for model artifacts

The current Oracle Always Free shape gives us enough room for the lab, Bonsai,
and archive accumulation, but not enough headroom for realistic model-training
workloads. CV1 should treat that limit as intentional, not as an optimization
todo.

## Recommended training locations

- Operator workstation with a local GPU
- On-prem CUDA-capable server
- Short-lived rented GPU instance for scheduled training runs

## Operational rule

Use the cloud lab to accumulate data, then export or sync the archive to the
place where training happens. The archive location may stay cloud-resident, but
the training job should not run on the OCI Always Free VM.

## Related docs

- `docs/test_results/cloud_spike/REPORT_TEMPLATE.md`
- `docs/test_results/cloud_spike/KILL_CRITERIA.md`
- `docs/operational_health_thresholds.md`
