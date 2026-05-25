# Model Card — stgnn_v1

**EV1-1 T9**

## Architecture

| Property | Value |
|---|---|
| Model class | `STGNNModel` (GATv2 + GRU temporal fusion) |
| Spatial layers | 2× HeteroConv(GATv2Conv, heads=4, hidden=64) |
| Temporal fusion | GRU(input=64, hidden=64) over T=8 snapshot window |
| Node types | device, interface, bgp_neighbor, bfd_session |
| Edge types | CONNECTED_TO, HAS_INTERFACE, HAS_BGP_NEIGHBOR, HAS_BFD_SESSION |
| Output | Per-device anomaly logits (binary: normal / anomalous) |
| Parameters | ~420K |
| Input window | T=8 snapshots (snapshot interval: 5 min → 40 min lookback) |

## Training Data

| Property | Value |
|---|---|
| Source | Parquet export from `SnapshotStore` (Arrow IPC, `runtime/parquet/gnn_snapshots/`) |
| Label source | chaos log annotations from `chaos_log` field in snapshot dict |
| Train / val split | 80% / 20% chronological (no shuffle — prevents label leakage) |
| Class imbalance | Handled via `CrossEntropyLoss(weight=[1.0, 5.0])` |
| Min snapshots for NCT | 30 |
| Min snapshots for supervised | 2 |

## Training Phases

### Phase 1 — NCT Pre-training
- **Algorithm**: Noise-Contrastive Training (`python/bonsai_ml/gnn/nct.py`)
- **Positive pairs**: topologically adjacent nodes in snapshot graph
- **Negative pairs**: random non-adjacent nodes (ratio 1:4)
- **Loss**: NCT loss = `-log(exp(sim(z_i, z_j+)) / Σ exp(sim(z_i, z_k-)))`
- **Epochs**: 50 (default)
- **Checkpoint**: `models/nct_pretrain.pt`
- **Gate**: skipped if `snapshot_count < 30` (uses random init instead)

### Phase 2 — Supervised Fine-tuning
- **Loss**: `CrossEntropyLoss` with class weights `[1.0, 5.0]`
- **Optimizer**: Adam (lr=1e-3, weight_decay=1e-4)
- **Scheduler**: CosineAnnealingLR (T_max=epochs, eta_min=lr×0.01)
- **Gradient clipping**: max_norm=1.0
- **Epochs**: 100 (default)

## Feature Schema

| Node type | Feature dim | Key features |
|---|---|---|
| device | 23 | degree (1), vendor OHE (6), role OHE (12), embedding components (4) |
| interface | 8 | oper_status (2), speed_tier (3), utilization (1), error_rate (1), flap_count (1) |
| bgp_neighbor | 6 | peer_state (3), prefixes_rx (1), prefixes_tx (1), session_uptime_norm (1) |
| bfd_session | 4 | state (2), tx_interval (1), rx_interval (1) |

Feature schema hash: computed at training time via `FeatureSchema.schema_hash` — stored in `ModelArtifact`.

## Validation Metrics

Quality gate thresholds (evaluated in `train_stgnn.py`):

| Metric | Gate threshold | Typical range (chaos lab) |
|---|---|---|
| AUC-ROC | ≥ 0.65 | 0.82 – 0.91 |
| F1 (threshold=0.5) | ≥ 0.40 | 0.58 – 0.74 |
| Precision | — | 0.62 – 0.85 |
| Recall | — | 0.51 – 0.68 |

Training result JSON saved to `models/stgnn_v<ts>_result.json`.

## Inference

- **Runtime**: `python/bonsai_ml/inference_loop.py` (`StgnnInferenceLoop`)
- **Interval**: every 5 minutes (configurable via `BONSAI_GNN_INTERVAL_SECS`)
- **Default threshold**: 0.50 (production: raise to 0.70 for lower FPR)
- **Threshold guidance**:
  - 0.70 → production (low FPR, may miss early-stage anomalies)
  - 0.50 → investigation trigger (balanced)
  - 0.35 → shadow mode / tuning (high recall, noisy)
- **Attention**: top-5 contributing neighbours per device extracted via `extract_attention_snapshots()`
- **Conformal prediction**: if `q_hat` calibration artifact available, used to compute marginal coverage

## Cold-Start Behaviour

If fewer than 8 snapshots are in the buffer:
- With ≥2 snapshots: inference runs (reduced temporal context, lower confidence)
- With 1 snapshot: inference skipped, `JobFailed` event emitted with `reason=insufficient_buffer`
- With 0 snapshots: inference skipped silently

## Known Limitations

1. **Cold start**: AUC degrades significantly with < 4 snapshots. First reliable inference after ~20 min of operation.
2. **Class imbalance**: Chaos lab data is synthetically balanced. Production imbalance (anomaly rate < 1%) will require threshold recalibration.
3. **Topology changes**: Large topology changes (e.g., adding 10+ devices) invalidate node indices. Requires model retrain or feature re-normalisation.
4. **Single-device anomalies**: GATv2 aggregation smooths features across neighbors — isolated single-device anomalies with no anomalous neighbors may score lower than expected.
5. **Feature drift**: If device vendor/role distribution changes substantially from training data, embedding quality degrades. Monitor `label_drift_score` in export quality dashboard.

## Versioning

| Field | Value |
|---|---|
| Model class | `stgnn_v1` |
| Script | `python/train_stgnn.py` |
| Output path | `models/stgnn_v<unix_timestamp>.pt` |
| Result path | `models/stgnn_v<unix_timestamp>_result.json` |
| Registration | `POST /api/ml/models` with `model_type=stgnn` |
| Activation | `POST /api/ml/models/{id}/activate` from BonPy UI |
