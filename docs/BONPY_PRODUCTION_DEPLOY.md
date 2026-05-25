# BonPy — Production Deployment Guide

**EV1-9 T8**

## Overview

The Bonsai sidecar (`collector_engine.py`) is a single Python process that runs continuously alongside Bonsai core. It manages:

- **RuleEngine** — event loop evaluating detection rules
- **BonsaiJobEngine** — APScheduler background thread (ML job scheduling)
- **StgnnInferenceLoop** — GNN inference every 5 min (registered with job engine)
- **HealthHTTP** — `GET :9200/health` (always reachable)
- **PrometheusMetrics** — `GET :9201/metrics`

---

## Prerequisites

| Requirement | Version |
|---|---|
| Python | 3.12+ |
| pip | 23+ |
| Bonsai core | running, accessible at gRPC addr |
| Available RAM | ≥ 2 GB (MemoryMax enforced by systemd) |

**Required Python packages** (install into venv):
```
torch>=2.3
torch-geometric>=2.5
pyarrow>=16
sentence-transformers>=3.0
apscheduler>=4.0
aiohttp>=3.9
prometheus-client>=0.20
scikit-learn>=1.5
pandas>=2.2
pyarrow>=16
requests>=2.32
```

---

## Installation

```bash
# 1. Create venv (as bonsai user)
python3.12 -m venv /opt/bonsai/.venv
source /opt/bonsai/.venv/bin/activate

# 2. Install dependencies
pip install torch torchvision --index-url https://download.pytorch.org/whl/cpu
pip install torch-geometric pyarrow sentence-transformers apscheduler aiohttp \
            prometheus-client scikit-learn pandas requests

# 3. Install Bonsai Python SDK
pip install -e /opt/bonsai/python/

# 4. Install systemd service
sudo cp /opt/bonsai/deploy/systemd/bonsai-rules-sidecar.service \
        /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable bonsai-rules-sidecar.service
```

---

## Configuration

Override defaults via `/etc/bonsai/sidecar.env`:

```ini
BONSAI_LOCAL_ADDR=localhost:50051
BONSAI_CORE_ADDR=core-host:50051
BONSAI_COLLECTOR_ID=rules-prod-01
BONSAI_API_URL=http://localhost:3000
BONSAI_SIDECAR_HEALTH_PORT=9200
BONSAI_SIDECAR_MAX_MEM_MB=1800
BONSAI_GNN_INTERVAL_SECS=300
BONSAI_GNN_THRESHOLD=0.5
```

Permissions: `chmod 640 /etc/bonsai/sidecar.env && chown root:bonsai /etc/bonsai/sidecar.env`

---

## Startup Sequence

1. `systemctl start bonsai.service` — Bonsai core must be up first
2. `systemctl start bonsai-rules-sidecar.service` — sidecar starts
3. Sidecar startup order (non-blocking):
   - Health HTTP on `:9200` starts immediately
   - Core forwarder thread starts (reconnects to gRPC core)
   - ML job engine starts (loads schedules from `/api/ml/schedules`)
   - STGNN inference loop registered with job engine
   - Local collector connector thread starts (reconnects with backoff)
4. On first connect to local collector: registers sidecar at `/api/sidecars`

The sidecar is **fully functional** (health check, job engine, metrics) even before the local collector connection is established. This means `systemctl start` returns immediately.

---

## First Run: Cold-Start Behaviour

| State | Behaviour |
|---|---|
| No trained model | Rules-only mode. Job engine schedules export + clustering. Inference loop skips each cycle with `no model` warning. |
| < 8 snapshots | GNN inference uses reduced temporal context (≥2 snaps). First full window after ~40 min. |
| First anomaly export | Triggered by job engine at `cron(hour=2)`. After export, if `quality_passed=True`, triggers STGNN training automatically. |
| Model trained | Model registered via API. Activate from BonPy UI (`/bonpy/models`). Inference begins on next cycle. |

---

## Monitoring

### Health endpoint
```bash
curl http://localhost:9200/health | jq
```
Key fields: `connected_to_core`, `connected_to_local`, `job_engine_running`, `snapshot_buffer_size`, `snapshot_buffer_stale`, `queue_depth`, `queue_drops_today`.

### Prometheus metrics
```bash
curl http://localhost:9201/metrics
```
Metrics: `bonsai_ml_job_runs_total`, `bonsai_ml_job_duration_seconds`, `bonsai_ml_job_last_success_timestamp`, `bonsai_ml_pending_embeddings`, `bonsai_sidecar_memory_mb`.

Add to `docker/prometheus/prometheus.yml`:
```yaml
scrape_configs:
  - job_name: bonsai_sidecar
    static_configs:
      - targets: ['localhost:9201']
```

### BonPy UI
Access at `http://<bonsai-host>:<port>/bonpy/`. Shows live dashboard, job status, model registry, detection stream.

### Logs
```bash
journalctl -u bonsai-rules-sidecar -f
```

---

## Upgrade Procedure

```bash
# 1. Deploy new code
sudo systemctl stop bonsai-rules-sidecar

# 2. sidecar receives SIGTERM → drains queue (up to 10s) → flushes snapshot buffer → stops job engine → exits
# Timeout: 30s (TimeoutStopSec in service file)

# 3. Pull + install
cd /opt/bonsai
git pull origin main
source .venv/bin/activate
pip install -e python/

# 4. Restart
sudo systemctl start bonsai-rules-sidecar
```

---

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `no active STGNN model found` in logs | No trained model or wrong `MODELS_DIR` | Run `python train_stgnn.py`, then activate from `/bonpy/models` |
| `queue >95% full — dropping warning` | Core forwarder disconnected, detections backing up | Check `connected_to_core` in health. Verify gRPC addr + network. |
| Health returns `queue_drops_today > 0` | Forward queue overflow (only critical/high pass in priority mode) | Check core connectivity. Increase `MemoryMax` if RSS is capped. |
| `snapshot_buffer_stale: true` | Inference loop failing silently or no model | Check logs for `InferenceLoop:` errors. Verify model file exists. |
| Sidecar OOM-killed, restarts every 10s | RSS exceeds 2GB MemoryMax | Check `bonsai_sidecar_memory_mb` metric. Reduce `BONSAI_SIDECAR_MAX_MEM_MB` or unload models. |
| `RegisterSidecar failed` | Bonsai core not yet accepting gRPC on startup | Non-fatal — heartbeat loop retries. Wait for `connected_to_core: true`. |
| `APScheduler unavailable` | Missing apscheduler package | `pip install apscheduler>=4.0` in venv |
| Job engine not starting | Import error in `job_engine.py` | Check logs for `WARNING: ML job engine unavailable`. Missing torch/apscheduler? |

---

## Memory Budget (typical, 100-device network)

| Component | Estimated RSS |
|---|---|
| Python base + SDK | ~80 MB |
| STGNN model (420K params) | ~50 MB |
| IsolationForest (legacy) | ~20 MB |
| Snapshot buffer (8 × 100 devices × 40 dims) | ~1.5 MB |
| Embedding cache (10K entries × 1536 dims) | ~60 MB |
| Sentence-transformers model | ~440 MB |
| Working memory (torch, pandas, etc.) | ~150 MB |
| **Total** | **~850 MB** |

`MemoryMax=2G` provides 2.4× headroom for spikes during training jobs.
