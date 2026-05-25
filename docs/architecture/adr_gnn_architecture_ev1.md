# ADR: GNN Architecture for EV1 — STGNN with Attention Explainability

**Status**: Accepted  
**Date**: 2026-05-25  
**Deciders**: Arjuna Ganesan  
**Context**: Bonsai EV1 sprint — upgrading the GNN anomaly detection layer from static HeteroGAT to a production-grade Spatio-Temporal GNN with operator-explainable attention.

---

## Problem Statement

The existing GNN (`python/bonsai_ml/gnn/model.py`) is a 2-layer `HeteroGATConv` spatial-only model. It processes one snapshot at a time with four node types (`device`, `interface`, `bgp_neighbor`, `bfd_session`) and a 23-dimensional device feature vector.

Three critical gaps block production value:

1. **No temporal dimension** — Network faults are inherently temporal. A device that has been stable for 30 days and suddenly spikes is categorically different from one with chronic instability. The current model treats every snapshot as independent. It cannot distinguish "first fault in 6 months" from "3rd fault this week".

2. **No attention explainability** — GAT computes attention weights but they are discarded. The investigation runtime cannot tell the operator *why* the GNN flagged a device. A black-box anomaly score has low NOC trust. "Device X is anomalous because neighbours Y and Z both have degraded BGP" is operationally actionable; a bare score is not.

3. **Label sparsity** — After 6 months of operation, fault snapshots typically represent <5% of total. Standard supervised training with cross-entropy loss on this imbalance produces models that default to predicting "normal" and achieve high accuracy by ignoring all faults. Self-supervised pre-training (NCT) addresses this before supervised fine-tuning begins.

---

## Decision: STGNN (GRU-over-GATv2) + NCT Pre-training

### Architecture

```
Input: T=8 consecutive graph snapshots (HeteroData objects)
  │
  ▼
[Spatial Layer per snapshot]
  HeteroGATv2Conv (2 layers, 8 heads, hidden_dim=64)
  → Per-node embedding h_i^t ∈ R^64 for each snapshot t
  → Attention weights α_ij^t retained per edge (for explainability)
  │
  ▼
[Temporal Layer per node]
  GRU(input=64, hidden=64, num_layers=1, batch_first=True)
  Applied independently per node type over sequence [h_i^1, h_i^2, ..., h_i^T]
  → Final hidden state z_i ∈ R^64 (encodes "how has this node changed over T snapshots")
  │
  ▼
[Output Head]
  Dropout(0.1) → Linear(64 → 2)
  → Per-node anomaly logit [normal, anomalous]
```

### Why GATv2 over GAT v1

GAT v1 (Veličković et al. 2018) computes attention as:
```
α_ij = softmax(LeakyReLU(a^T · [W·h_i ∥ W·h_j]))
```
This is a **static** attention mechanism — the attention weight depends only on the transformed features, and the same linear `a^T` is applied to all pairs. Brody et al. (2021) show this causes **rank collapse**: in practice, all nodes in a layer attend with nearly equal weights because the single-vector `a` cannot distinguish different query-key relationships. GATv2 fixes this with:
```
α_ij = softmax(a^T · LeakyReLU(W · [h_i ∥ h_j]))
```
The concatenation happens *before* the linear projection, making the attention **dynamic** — different source-destination pairs compute meaningfully different weights. At Bonsai's scale (N<500 nodes), GATv2 adds negligible compute overhead and materially improves gradient signal during training.

### Why GRU over Transformer for temporal encoding

| Criterion | GRU | Transformer |
|-----------|-----|-------------|
| Training stability at N<500 nodes | Excellent | Requires large data |
| Memory per node | O(T · hidden) | O(T² · heads) |
| Cold-start handling (< T snapshots) | Padding + masking, stable | Degrades without positional encoding |
| Parameter count | ~50K for this config | ~200K+ |
| T>16 performance | Plateau | Outperforms GRU |

**Decision**: GRU is adopted for EV1. Revisit Transformer temporal encoder at EV2 if GRU temporal gain plateaus and archive depth exceeds 90 days (sufficient training data for T>16).

### Training phases

**Phase 1 — NCT Self-supervised Pre-training**

Noise-Contrastive Training (NCT) pre-trains the GATv2 spatial layers on the topology structure itself, without requiring fault labels. The intuition: topologically adjacent devices (spine-leaf pairs, BGP peers) should have similar embeddings; randomly sampled non-adjacent pairs should not.

Loss:
```
L_NCT = -log(exp(sim(z_i, z_j+) / τ) / Σ_k exp(sim(z_i, z_k-) / τ))
```
where sim = cosine similarity, τ=0.07 (temperature), j+ = topological neighbour, k- = random non-neighbour.

Gate: NCT pre-training runs only when `snapshot_count >= 30` (configurable). Below this, supervised fine-tuning starts from random init with focal loss to handle imbalance (see EV1-8).

Pre-trained weights: `models/nct_pretrain.pt`

**Phase 2 — Supervised Fine-tuning**

Loads NCT pre-trained weights, adds temporal GRU layers (random init), trains on fault labels from chaos archive. Uses focal loss with γ=2 to further down-weight easy negatives. Saves `models/stgnn_v1.pt`.

---

## Alternatives Evaluated

### Static HeteroGAT (current baseline)
- **Pros**: Simple, low memory, already implemented.
- **Cons**: No temporal dimension, GATv1 rank collapse, feature dim stale (23 dims, missing cpu/memory/uptime). Retained as baseline for ablation comparison.
- **Decision**: Replaced in production inference. Kept in codebase as `--model-type hetero_gat` flag for ablation.

### Pure Graph Transformer (full self-attention over graph patches)
- **Pros**: Handles long-range dependencies, strong at large scale.
- **Cons**: O(N²) attention — wasteful for sparse topology graphs where average degree ~3-4. Requires >500 training snapshots for stable gradient. At Bonsai's EV1 scale (30-90 snapshots), it overfits severely.
- **Decision**: Rejected for EV1. Revisit at EV3 when archive exceeds 180 days.

### STGNN with Transformer temporal encoder
- **Pros**: Better than GRU at T>16.
- **Cons**: Needs T>16 to beat GRU; requires careful positional encoding for irregular snapshot timestamps; more parameters.
- **Decision**: Deferred to EV2. Trigger: GRU temporal AUC gain < 0.02 over static spatial model at 90-day archive depth.

### Control-Weighted GNN (down-weight nodes in maintenance windows)
- **Pros**: Reduces false positives during planned changes.
- **Cons**: Requires change window integration (ServiceNow change_request, DV4 change management).
- **Decision**: Adopted as training loss modifier in EV1-8 (not this batch). Implementation: multiply per-node loss weight by `(1 - 0.9 * in_change_window)` during training.

---

## Node Types and Feature Dimensions (EV1 target)

| Node type | v1 dim | v2 dim | New features added |
|-----------|--------|--------|--------------------|
| `device` | 23 | 36 | cpu_util_pct, memory_used_pct, uptime_log_secs, has_thermal_warning, bgp_session_count, ospf_neighbor_count, interface_count, bmp_session_count, gnn_quality_score, model_ohe×3, is_in_redundancy_group |
| `interface` | 8 | 14 | in_error_rate, out_error_rate, in_utilization_pct, out_utilization_pct, is_in_lag, optical_rx_dbm_normalised |
| `bgp_neighbor` | 6 | 12 | adj_rib_in_routes, loc_rib_routes, prefixes_rejected, hold_time, is_external, session_uptime_log_secs |
| `bfd_session` | 4 | 10 | detect_multiplier, interval_ms, registered_protocols_count, is_up, is_in_redundancy_path, source_ohe |
| `ospf_neighbor` | NEW | 8 | state, area, metric, dr_bdr_flag, uptime_log_secs, dead_interval, priority, retransmit_count |
| `redundancy_group` | NEW | 6 | type_ohe×4 (lag/vrrp/hsrp/ecmp), member_count, all_members_up_flag |
| `sensor_reading` | NEW | 4 | temp_normalised, type_ohe×3 (temp/fan/power), is_above_warning, is_above_critical |

New edge types: `has_ospf_neighbor`, `member_of` (Device/Interface→RedundancyGroup), `carries_flow` (Device→AppFlow), `has_sensor` (Device→SensorReading).

---

## Attention Explainability Pipeline

1. At inference, `GATv2Conv.forward(return_attention_weights=True)` returns `(output, (edge_index, alpha))`.
2. `AttentionSnapshot` dataclass captures per-anomalous-node top-5 contributing neighbours with edge type and weight.
3. Python posts `AttentionSnapshot` to `POST /api/gnn/attention` (Rust endpoint, EV1 Batch 1 `ml_jobs.rs`).
4. Stored as `GnnAttentionSnapshot` node in KuzuDB.
5. `src/investigation_runtime.rs` queries attention weights when generating investigation context: "GNN attention: spine-2 contributed α=0.42, pe-1 contributed α=0.31 — both have degraded BGP."

This closes the black-box trust gap without requiring any changes to the core investigation pipeline.

---

## Performance Targets

| Metric | Static GATv1 baseline | STGNN target |
|--------|----------------------|--------------|
| Val AUC (synthetic chaos) | 0.71 | ≥0.85 |
| Val F1 (threshold=0.7) | 0.42 | ≥0.65 |
| False positive rate | ~18% | ≤8% |
| Inference latency (T=8, N=100) | <50ms | <200ms |
| Cold-start (< T snapshots) | Degraded | Graceful (zero-padding + masking) |

---

## Implementation Files

| File | Status | Description |
|------|--------|-------------|
| `python/bonsai_ml/gnn/model.py` | Modified | GATv2 upgrade, new node types, expanded dims |
| `python/bonsai_ml/gnn/stgnn.py` | NEW | SnapshotBuffer, TemporalGNNLayer, STGNNModel, AttentionSnapshot |
| `python/bonsai_ml/gnn/nct.py` | NEW | NodePairSampler, NCTLoss, pretrain_nct() |
| `python/bonsai_ml/gnn/data_loader.py` | Modified | New feature dims, from_api_snapshot() |
| `python/bonsai_ml/feature_schema.py` | Modified | DEVICE_V2_SCHEMA constant |
| `src/http_server/ml_jobs.rs` | EV1 Batch 1 | POST /api/gnn/attention, GET /api/gnn/results |

---

## Risks and Mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Insufficient training data for GRU (< T=8 snapshots) | High at EV1 | Zero-padding with attention mask; model trained on variable-length sequences |
| GATv2 memory spike on dense subgraphs | Low | Sparse edge implementation; max edges cap in data_loader |
| NCT pre-training divergence | Medium | Learning rate warmup (100 steps), gradient clipping (max_norm=1.0), early stopping on NCT loss plateau |
| Feature schema drift between training and inference | High if not gated | DEVICE_V2_SCHEMA hash check before inference; hard fail if mismatch |
