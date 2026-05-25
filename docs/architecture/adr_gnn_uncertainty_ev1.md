# ADR: GNN Uncertainty Quantification Strategy — EV1

**Status:** Accepted  
**Date:** 2026-05-25  
**Author:** Bonsai ML Team  
**Epic:** EV1-8 — Structural Uncertainty: NCT, Control-Weighted GNN, Conformal Prediction

---

## Context

The Bonsai STGNN outputs a scalar anomaly score per device per snapshot. This is a point estimate. A score of 0.72 could mean:

- "This device is clearly anomalous" (model confident, score trustworthy).
- "The model has never seen this topology before; it is interpolating" (model uncertain, score unreliable).

For a NOC auto-triggering investigations on GNN alerts, this distinction is critical. Triggering on high-uncertainty scores wastes analyst time. Not triggering on high-confidence scores creates missed incidents.

Bonsai faces two compounding challenges:

1. **Label sparsity**: <5% of snapshots are faults in production. Most fault labels come from synthetic chaos injection, not real incidents.
2. **Distribution shift**: The graph topology changes as devices are added/removed. The model must handle unseen subgraph patterns gracefully.

Three independent approaches were evaluated and their selection rationale is documented here.

---

## Decision 1: NCT (Noise-Contrastive Training) Pre-Training

**Chosen.**

### Rationale

NCT addresses label sparsity by pre-training the spatial GATv2 layers using graph topology as self-supervision — no fault labels required. The model learns "what does a real network topology look like?" before supervised fine-tuning begins.

This is effective because Bonsai topologies are highly structured (spine-leaf, ring, BGP full-mesh). The model can learn strong priors about valid vs. corrupted topology from unlabelled snapshots alone.

### Implementation

- `python/bonsai_ml/gnn/nct.py`: `pretrain_nct()`, `NodePairSampler`, `NCTLoss`.
- `NoiseSchedule`: three-phase noise curriculum.
  - Epoch 1–10 (light): 5% edge drop.
  - Epoch 11–30 (medium): 15% edge drop + 10% feature perturbation.
  - Epoch 31+ (heavy): 30% edge drop + 20% feature perturbation + 5% spurious edges.
- `NodeFeatureInvariance`: vendor OHE (indices 1–6) and role OHE (indices 7–18) are protected from perturbation. Only operational features (cpu_util, error rates, uptime) are perturbed.
- Gate: NCT runs only when ≥30 snapshots are available; below this, supervised training starts from random init.

### Trade-offs

- Adds ~50 epochs of pre-training time (~minutes on CPU, seconds on GPU).
- Requires PyTorch Geometric. Falls back gracefully to supervised-only if unavailable.
- Starting noise curriculum from light→heavy is critical: starting heavy causes the model to ignore topology entirely.

---

## Decision 2: CW-GNN (Control-Weighted GNN Loss)

**Chosen.**

### Rationale

During operator-declared maintenance windows (change requests in the graph via `ChangeRequest` nodes), network events are expected and not anomalous. Without CW-GNN, the model penalises itself for "missing" these events during training, degrading generalisation.

CW-GNN modifies the training loss per-sample:

- `change_weight = 0.0`: fault on a device with active change request → zero gradient contribution.
- `change_weight = 0.5`: fault on a different device in the same snapshot as a change window.
- `change_weight = 1.0`: no active change request → full gradient.

### Implementation

- `python/bonsai_ml/gnn/loss.py`: `ControlWeightedLoss`, `FocalControlWeightedLoss`.
- `python/bonsai_ml/gnn/archive_to_training.py`: `compute_sample_weights()`, `compute_control_mask()`.

### Trade-offs

- Requires `ChangeRequest` nodes populated in the graph (from change management integration). Without them, all weights default to 1.0 (equivalent to standard cross-entropy).
- `FocalControlWeightedLoss` combines focal loss (γ=2.0) with control weighting for double protection against easy-negative dominance in imbalanced datasets.

---

## Decision 3: Uncertainty Quantification Method

### Option A: Conformal Prediction (Split Conformal)

**Chosen as primary.**

**Rationale:** Distribution-free, post-hoc, no retraining required. Provides a mathematically rigorous coverage guarantee: "With probability 1−α, the true label is in the prediction set." Works with any trained model.

**Implementation:**
- `python/bonsai_ml/gnn/conformal.py`: `ConformalCalibrator`, `ConformalCalibrator.calibrate()`.
- Nonconformity score: `s_i = 1 − softmax(logit_fault)[i]` for positive (fault) calibration samples.
- Threshold: `q_hat = (ceil((n+1)(1−α)) / n)`-quantile of `{s_i}` (finite-sample correction).
- Coverage targets: α=0.1 (90%) in production; α=0.05 (95%) for high-stakes auto-remediation paths.
- Requires ≥30 fault examples in the calibration set. Triggered automatically after every training run.

### Option B: MC Dropout

**Chosen as cold-start fallback.**

**Rationale:** When a held-out calibration set is unavailable (<100 fault examples total, common in lab or early-production deployments), MC Dropout provides uncertainty estimates with no calibration set requirement.

**Implementation:**
- `python/bonsai_ml/gnn/conformal.py`: `MCDropoutEstimator`.
- N=20 stochastic forward passes with `model.train()` active (dropout on).
- `mean_score = mean(scores)`, `uncertainty = std(scores)`.
- Gate: `mc_dropout_samples=0` disables this; defaults to zero uncertainty (single-pass).
- `MCDropoutEstimator.from_conformal_or_fallback()`: loads conformal calibrator if available, otherwise prepares MC Dropout.

**Trade-off:** N× inference cost. Not suitable for real-time inference paths at scale. Acceptable for Bonsai's O(100) device topology at O(5min) inference cadence.

### Option C: Bayesian GNN (Deep Ensembles)

**Rejected.**

Requires 5× independent training runs with different random seeds. Prohibitive on single-box deployment (each run takes hours on CPU). The uncertainty quality improvement over MC Dropout does not justify the operational overhead at Bonsai's current scale.

### Option D: Laplace Approximation

**Rejected.**

Requires computing the Hessian of the loss with respect to all model parameters. Not compatible with heterogeneous GNN architectures (the approximation quality degrades with message-passing non-linearities). The `laplace-torch` library does not support PyG HeteroData natively.

---

## Uncertainty Gating in Investigation Trigger

The `investigation_trigger.rs` uncertainty gate (`BONSAI_GNN_UNCERTAINTY_GATE`, default 0.0 = disabled):

```
if anomaly_score > threshold AND uncertainty_margin < uncertainty_gate:
    → auto-trigger investigation
elif anomaly_score > threshold AND uncertainty_margin >= uncertainty_gate:
    → emit GnnUncertainHighAlert (BonPy UI display only)
    → do NOT auto-trigger
```

The `uncertainty_margin` field is written to `GnnInferenceResult` nodes by the Python inference pipeline and read by the Rust trigger.

---

## Coverage Targets Summary

| Use case | α | Coverage guarantee | Notes |
|---|---|---|---|
| Standard NOC alerting | 0.10 | ≥90% | Default production setting |
| Auto-remediation paths | 0.05 | ≥95% | High-stakes: BGP clear, interface shutdown |
| Lab / cold-start | N/A | N/A (MC Dropout) | No coverage guarantee; use with caution |

---

## References

- Angelopoulos & Bates (2021). "A Gentle Introduction to Conformal Prediction and Distribution-Free Uncertainty Quantification." arXiv:2107.07511.
- Gal & Ghahramani (2016). "Dropout as a Bayesian Approximation." ICML 2016.
- You et al. (2020). "Graph Contrastive Learning with Augmentations." NeurIPS 2020.
