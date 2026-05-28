# GNN Architecture: EV1 — Spatio-Temporal Graph Neural Network

**Version**: EV1  
**Model identifier**: `stgnn_v1`  
**Status**: Production  
**Last updated**: 2026-05-25  

---

## Overview

EV1 upgrades Bonsai's anomaly detection layer from a static 2-layer `HeteroGATConv` model to a production-grade Spatio-Temporal GNN (STGNN). The core changes are:

- **Spatial layer**: `HeteroGATv2Conv` (Brody et al. 2021) replacing GATv1 — eliminates rank collapse via dynamic per-pair attention
- **Temporal layer**: per-node `GRU` over an 8-snapshot ring buffer — captures "first fault in 6 months" vs "3rd fault this week"
- **Explainability**: attention weights captured at inference, persisted to graph, surfaced in investigation context
- **Uncertainty**: conformal prediction (primary) + MC Dropout (cold-start fallback) — uncertainty-gated investigation triggering
- **Pre-training**: Noise-Contrastive Training (NCT) on topology structure before supervised fine-tuning

See `docs/architecture/adr_gnn_architecture_ev1.md` for the full architectural decision record and `docs/architecture/adr_gnn_uncertainty_ev1.md` for the uncertainty quantification strategy ADR.

---

## Architecture

### Data flow

```
/api/graph/snapshot (every 5 min)
    └─► SnapshotBuffer  [8-snapshot Arrow IPC ring buffer, disk-persistent]
            └─► STGNNModel.forward(snapshot_sequence)
                    │
                    ├─► [Spatial] HeteroGATv2Conv × 2 layers, 8 heads, hidden_dim=64
                    │       └─► per-node embedding h_i^t ∈ R^64, attention α_ij^t retained
                    │
                    └─► [Temporal] GRU(input=64, hidden=64) per node type, over T=8 steps
                                └─► final hidden state z_i ∈ R^64
                                        └─► Dropout(0.1) → Linear(64→2) → anomaly logit
                                                │
                                                ├─► POST /api/gnn/inference-results
                                                ├─► POST /api/gnn/attention  (AttentionSnapshot)
                                                └─► conformal/MC-Dropout uncertainty score
                                                        └─► investigation_trigger.rs
                                                            (auto-investigate if score > threshold
                                                             AND uncertainty < 0.3)
```

### Model equation

```
# Spatial pass (per snapshot t)
h_i^t = HeteroGATv2Conv(x_i, edge_index, edge_attr)   # ∈ R^64, with α_ij^t

# Temporal pass (per node i across T snapshots)
z_i = GRU([h_i^1, h_i^2, ..., h_i^T])[-1]            # final hidden state ∈ R^64

# Classification head
logit_i = Linear(Dropout(z_i))                         # [normal, anomalous] ∈ R^2
```

### Why GATv2 over GATv1

GATv1 attention: `α_ij = softmax(LeakyReLU(a^T · [W·h_i ∥ W·h_j]))`  
GATv2 attention: `α_ij = softmax(a^T · LeakyReLU(W · [h_i ∥ h_j]))`

The concatenation before the projection makes attention **dynamic** — different source-destination pairs compute meaningfully different weights. GATv1 suffers from rank collapse at Bonsai's scale (N<500 nodes) where a single `a^T` produces near-uniform weights. GATv2 adds negligible compute overhead.

### Why GRU over Transformer temporal encoder

| Criterion | GRU | Transformer |
|-----------|-----|-------------|
| Training stability at N<500 nodes | Excellent | Requires large data |
| Memory per node | O(T · hidden) | O(T² · heads) |
| Cold-start (< T snapshots) | Padding + masking, stable | Degrades without positional encoding |
| Parameter count | ~50K for this config | ~200K+ |

Revisit Transformer temporal encoder at EV2 if GRU temporal AUC gain plateaus and archive depth exceeds 90 days.

---

## Node Types and Feature Dimensions

| Node type | EV1 dims | Key features |
|-----------|----------|--------------|
| `device` | 36 | cpu_util_pct, memory_used_pct, uptime_log_secs, has_thermal_warning, bgp/ospf session counts, gnn_quality_score, model OHE×3, is_in_redundancy_group |
| `interface` | 14 | in/out error rates, in/out utilization_pct, is_in_lag, optical_rx_dbm_normalised |
| `bgp_neighbor` | 12 | adj_rib_in_routes, loc_rib_routes, prefixes_rejected, hold_time, is_external, session_uptime_log_secs |
| `bfd_session` | 10 | detect_multiplier, interval_ms, registered_protocols_count, is_up, is_in_redundancy_path, source OHE |
| `ospf_neighbor` | 8 | state, area, metric, dr_bdr_flag, uptime_log_secs, dead_interval, priority, retransmit_count |
| `redundancy_group` | 6 | type OHE×4 (lag/vrrp/hsrp/ecmp), member_count, all_members_up_flag |
| `sensor_reading` | 4 | temp_normalised, type OHE×3 (temp/fan/power), is_above_warning, is_above_critical |

The device feature vector is augmented with 4 PCA-compressed config embedding dimensions (PCA 384→4), giving an effective 40-dim device vector at inference.

**Edge types** (EV1 additions): `has_ospf_neighbor`, `member_of` (Device/Interface → RedundancyGroup), `carries_flow` (Device → AppFlow), `has_sensor` (Device → SensorReading).

Feature schema constant: `python/bonsai_ml/feature_schema.py` → `DEVICE_V2_SCHEMA`. A SHA-256 hash of this schema is checked before each inference run; a mismatch is a hard failure.

---

## Training Pipeline

### Phase 1 — NCT Self-supervised Pre-training

Noise-Contrastive Training pre-trains the GATv2 spatial layers on topology structure alone, without fault labels. Topologically adjacent nodes (spine-leaf pairs, BGP peers) should have similar embeddings; randomly sampled non-adjacent pairs should not.

```
L_NCT = -log( exp(sim(z_i, z_j+) / τ) / Σ_k exp(sim(z_i, z_k-) / τ) )
```

- `sim` = cosine similarity
- `τ = 0.07` (temperature)
- `j+` = topological neighbour
- `k-` = random non-neighbour (noise curriculum)

**Noise curriculum**: light (5% edge removal) → medium (15% + feature perturbation) → heavy (30% + spurious edges added).

Gate: NCT runs only when `snapshot_count >= 30`. Below this threshold, supervised fine-tuning starts from random init with focal loss. Output: `models/nct_pretrain.pt`.

### Phase 2 — Supervised Fine-tuning

Loads NCT pre-trained weights, attaches the GRU temporal layer (random init), trains on fault labels from the chaos archive.

- **Loss**: `FocalControlWeightedLoss` — focal loss (γ=2.0) for class imbalance, with change-window weighting: per-node loss weight multiplied by `(1 - 0.9 × in_change_window)` to suppress false positives during planned maintenance
- **Optimizer**: Adam with `CosineAnnealingLR`
- **Regularisation**: gradient clip `max_norm=1.0`, Dropout(0.1) on output head
- **Quality gate**: AUC ≥ 0.65 AND F1 ≥ 0.40 on held-out validation set. Failed gate → dead-letter, no auto-activation
- Output: `models/stgnn_v1.pt`

### Phase 3 — Conformal Calibration

Runs automatically after training on a held-out calibration set.

```python
# Non-conformity score: 1 - softmax probability for the true class
nc_scores = 1 - model.predict_proba(X_cal)[y_cal]
q_hat = np.quantile(nc_scores, 1 - alpha)   # alpha = 0.10 → 90% coverage
```

Output: `models/conformal_qhat_alpha0.1.json`. This threshold `q_hat` is used at inference to determine the prediction set for each node.

### Full training command

```bash
# NCT pre-training (≥30 snapshots required)
python -m bonsai_ml.gnn.nct

# Supervised fine-tune + conformal calibration + register
python python/train_stgnn.py --model-type stgnn --register

# Activate model version
curl -X POST http://localhost:3000/api/ml/models/{id}/activate
```

---

## Uncertainty Quantification

Two methods are available; the system selects automatically based on calibration set availability.

### Conformal Prediction (primary)

Provides a **distribution-free 90% coverage guarantee**: on any new graph snapshot, the true class is in the predicted set at least 90% of the time, regardless of the input distribution.

At inference:
1. Compute non-conformity score `nc = 1 - softmax(anomaly_logit)`
2. If `nc > q_hat` → prediction set includes "anomalous"
3. Uncertainty score = `|q_hat - nc|` (distance from decision boundary)

### MC Dropout (cold-start fallback)

Used when no calibration set exists (fewer than `mc_dropout_min_samples` fault examples). Runs `mc_dropout_samples=20` forward passes with Dropout active, reports variance as uncertainty.

```python
model.train()   # Dropout active
scores = [model(x) for _ in range(20)]
uncertainty = np.var(scores, axis=0)
```

### Uncertainty-gated investigation triggering

`src/investigation_trigger.rs` auto-triggers an investigation only when:
- Anomaly score > `investigation_score_threshold` (default: 0.7)
- Uncertainty score < `BONSAI_GNN_UNCERTAINTY_GATE` (default: 0.3)

High-uncertainty predictions are logged but not escalated. This prevents spurious investigations from cold-start or topology-change periods.

See `docs/architecture/adr_gnn_uncertainty_ev1.md` for full rationale and the coverage target analysis.

---

## Attention Explainability

GATv2 attention weights are captured at inference and written back to the graph, bridging the "black-box score" trust gap with the NOC.

### Pipeline

1. `GATv2Conv.forward(return_attention_weights=True)` returns `(output, (edge_index, alpha))`
2. `AttentionSnapshot` dataclass captures, per anomalous node, the top-5 contributing neighbours with edge type and weight
3. Python posts to `POST /api/gnn/attention` → stored as `GnnAttentionSnapshot` node in KuzuDB
4. `src/investigation_runtime.rs` queries these weights when building investigation context

### Example investigation context injection

```
GNN attention (inference 2026-05-25T14:32:11Z):
  device/spine-2  contributed α=0.42  (edge: bgp_peer)
  device/pe-1     contributed α=0.31  (edge: bgp_peer)
  device/leaf-3   contributed α=0.18  (edge: lldp_neighbor)
  → Both spine-2 and pe-1 have degraded BGP sessions
```

This context is appended to the investigation prompt without any changes to the core investigation pipeline.

---

## Semantic Embeddings

Three embedding pipelines augment the GNN and investigation context.

### Syslog message embeddings

- **Model**: `all-MiniLM-L6-v2` (local, CPU-optimised) / `nomic-embed-text` (Ollama) / `text-embedding-3-small` (OpenAI)
- **Pipeline**: events tagged `needs_embedding=true` in graph → `syslog_embedding_worker.py` batch-processes every 60 s → `EventEmbedding` nodes stored
- **Uses**: cosine similarity search for investigation context ("similar past events"), weekly `SyslogClusterer` (MiniBatchKMeans, 20 clusters, HDBSCAN for outlier detection)

### Device config embeddings

- **Pipeline**: `config_embedding_worker.py` runs every 6 h → fetches unembedded configs from `/api/devices/unembedded-config` → embeds full config text → `DeviceConfigEmbedding` node
- **Compression**: PCA 384→4 dims (fits within GNN device feature budget), injected into `device` feature vector at inference

### Detection reason embeddings

- **Pipeline**: `detection_clustering.py` — HDBSCAN cluster detection reasons weekly
- **Uses**: surfaces "3 similar BGP-related detections in the last 48 hours" as investigation context

---

## Sidecar Production Operation

The ML pipeline runs inside `collector_engine.py` (the Python sidecar), supervised by systemd.

### Job schedule (default)

| Job | Schedule | Description |
|-----|----------|-------------|
| `gnn_inference` | `interval(5 min)` | STGNN forward pass → write-back to graph |
| `syslog_embedding` | `interval(60 s)` | Batch embed pending syslog events |
| `graph_snapshot` | `interval(4 h)` | Capture snapshot for STGNN buffer |
| `anomaly_export_daily` | `cron(hour=2)` | Incremental Parquet export, quality gated |
| `remediation_export_weekly` | `cron(day=0, hour=2)` | Full remediation training export |
| `detection_clustering` | `cron(day=0, hour=3)` | HDBSCAN cluster detection reasons |
| `config_embedding` | `interval(6 h)` | Embed device config text |

Schedules are managed via `POST /api/ml/schedules` or the BonPy UI at `/bonpy/jobs`.

### Startup and shutdown

- **Non-blocking startup**: sidecar starts immediately, registers with Bonsai core, then enters reconnect loop. Bonsai core starts whether or not the sidecar is available.
- **Graceful shutdown**: `SIGTERM` drains the forward queue (up to 10 s), cancels in-flight jobs, saves the `SnapshotBuffer` to disk.
- **Memory bounds**: `MlMemoryManager` enforces RSS limits — LRU model cache eviction before Bonsai core's resource governor kicks in. Configured via `BONSAI_ML_RSS_LIMIT_MB`.
- **Queue backpressure**: forward queue has a capacity cap; overflow is dropped-oldest with a log warning and `bonsai_ml_queue_overflow_total` counter increment.

### Health and metrics

```bash
# Sidecar health (includes model loaded, inference times, queue depth, memory)
curl http://localhost:9200/health | python3 -m json.tool

# Prometheus metrics (job runs, parquet rows, AUC, pending embeddings, memory)
curl http://localhost:9201/metrics

# Bonsai proxy (enriched with registry data)
curl http://localhost:3000/api/sidecar/status
```

---

## Performance Targets

| Metric | Static GATv1 baseline | STGNN (EV1 target) |
|--------|----------------------|--------------------|
| Val AUC (synthetic chaos) | 0.71 | ≥ 0.85 |
| Val F1 (threshold = 0.7) | 0.42 | ≥ 0.65 |
| False positive rate | ~18% | ≤ 8% |
| Inference latency (T=8, N=100) | < 50 ms | < 200 ms |
| Cold-start (< T snapshots) | Degraded | Graceful (zero-padding + masking) |
| Conformal coverage | — | ≥ 90% |
| MC Dropout uncertainty (cold-start) | — | < 0.3 on stable topology |

---

## Cold-Start Behaviour

When the `SnapshotBuffer` has fewer than T=8 snapshots:

- Available snapshots are left-aligned; missing positions are zero-padded
- An attention mask zeros out contributions from padded timesteps in the GRU
- Conformal calibration requires a minimum of `mc_dropout_min_samples` fault examples — until met, MC Dropout is used
- Investigation triggering is suppressed until the buffer reaches ≥ 4 snapshots (configurable via `BONSAI_GNN_MIN_SNAPSHOTS_BEFORE_TRIGGER`)

---

## Key Configuration

| Variable / setting | Default | Description |
|--------------------|---------|-------------|
| `BONSAI_GNN_INTERVAL_SECS` | 300 | Inference job interval |
| `BONSAI_GNN_UNCERTAINTY_GATE` | 0.3 | Max uncertainty to auto-trigger investigation |
| `BONSAI_ML_RSS_LIMIT_MB` | 2048 | RSS hard limit before model cache eviction |
| `nct_min_snapshots` | 30 | Minimum snapshots before NCT pre-training runs |
| `mc_dropout_samples` | 20 | Forward passes for MC Dropout uncertainty |
| `conformal_alpha` | 0.10 | Target miscoverage rate (→ 90% coverage) |
| `snapshot_buffer_size` | 8 | Number of snapshots in temporal window (T) |

All runtime tunables are DB-backed. Update via `PATCH /api/settings/gnn` or the BonPy UI.

---

## Implementation Files

| File | Description |
|------|-------------|
| `python/bonsai_ml/gnn/model.py` | GATv2 spatial layer, new node types, expanded feature dims |
| `python/bonsai_ml/gnn/stgnn.py` | `SnapshotBuffer`, `TemporalGNNLayer`, `STGNNModel`, `AttentionSnapshot` |
| `python/bonsai_ml/gnn/nct.py` | `NodePairSampler`, `NCTLoss`, `pretrain_nct()`, noise curriculum |
| `python/bonsai_ml/gnn/loss.py` | `FocalControlWeightedLoss` |
| `python/bonsai_ml/gnn/conformal.py` | `ConformalCalibrator`, `ConformalPredictor`, uncertainty-gated prediction |
| `python/bonsai_ml/gnn/snapshot_store.py` | Arrow IPC snapshot buffer (disk-persistent) |
| `python/bonsai_ml/feature_schema.py` | `DEVICE_V2_SCHEMA` constant + schema hash |
| `python/bonsai_ml/inference_client.py` | STGNN inference + write-back to Bonsai graph |
| `python/bonsai_ml/inference_loop.py` | `StgnnInferenceLoop` — 5-min APScheduler job |
| `python/bonsai_ml/text_embeddings.py` | `TextEmbedder` wrapping sentence-transformers / Ollama / OpenAI |
| `python/bonsai_ml/syslog_embedding_worker.py` | 60 s batch embedder for syslog events |
| `python/bonsai_ml/config_embedding_worker.py` | 6 h batch embedder for device configs |
| `python/bonsai_ml/detection_clustering.py` | HDBSCAN cluster detection reasons |
| `python/bonsai_ml/memory_manager.py` | `MlMemoryManager` with LRU model cache + RSS bounds |
| `python/bonsai_ml/snapshot_client.py` | `GraphSnapshotClient` (fetch + convert `/api/graph/snapshot`) |
| `python/bonsai_ml/export_job.py` | `ParquetExportJob` with catalog integration + quality gate |
| `python/bonsai_ml/parquet_validator.py` | Schema validation, class balance, PSI drift detection |
| `python/bonsai_ml/model_cards/stgnn_v1.md` | Model card (training data, metrics, limitations, versioning) |
| `python/train_stgnn.py` | Standalone training script: NCT pretrain → fine-tune → gate → register |
| `src/http_server/ml_jobs.rs` | REST endpoints: exports, models, jobs, schedules, GNN results, attention, embeddings |
| `src/ml_event_bus.rs` | SSE ML event channel (`/api/ml/events/stream`) |
| `src/investigation_trigger.rs` | Uncertainty-gated auto-investigation from GNN scores |
| `src/http_server/observability.rs` | `/api/graph/snapshot` live snapshot endpoint |
| `deploy/systemd/bonsai-rules-sidecar.service` | Systemd unit with hardening (ProtectSystem, PrivateTmp, MemoryMax) |

---

## Related Documents

- `docs/architecture/adr_gnn_architecture_ev1.md` — Architecture Decision Record: STGNN vs alternatives, GATv2 vs GATv1, GRU vs Transformer, NCT rationale
- `docs/architecture/adr_gnn_uncertainty_ev1.md` — ADR: Conformal Prediction vs MC Dropout vs Deep Ensembles, uncertainty gate design
- `python/bonsai_ml/model_cards/stgnn_v1.md` — Model card: training data, feature schema, validation metrics, known limitations
- `ev1/ev1-1.md` — EV1 sprint spec: GNN architecture tasks
- `ev1/ev1-8.md` — EV1 sprint spec: structural uncertainty tasks
- `ev1/ev1-9.md` — EV1 sprint spec: sidecar hardening tasks
- `docs/EV1_UBUNTU_TESTING_GUIDE.md` — End-to-end EV1 testing procedures
