# GNN Feature Engineering Audit

> D5-T1 (DV1) — 2026-05-15. Audits `python/bonsai_ml/gnn/data_loader.py`
> against the CV5 GNN philosophy commitment: structural features dominate,
> vendor identity is a small one-hot tail.

---

## Feature inventory (`DEFAULT_FEATURE_NAMES`, 23 dimensions)

| Index | Feature name | Category | Vendor-dependent? | Notes |
|---|---|---|---|---|
| 0 | `degree` | Structural | No | Graph degree — topology-derived, fully vendor-neutral |
| 1 | `vendor_nokia` | Vendor one-hot | Yes | SR Linux |
| 2 | `vendor_cisco` | Vendor one-hot | Yes | IOS-XRd |
| 3 | `vendor_juniper` | Vendor one-hot | Yes | cRPD / vJunosEvolved |
| 4 | `vendor_arista` | Vendor one-hot | Yes | cEOS |
| 5 | `vendor_frr` | Vendor one-hot | Yes | FRR / Holo |
| 6 | `vendor_other` | Vendor one-hot | Yes | Catch-all |
| 7 | `role_super_spine` | Role one-hot | No | Role is operator-defined, not vendor-dependent |
| 8 | `role_spine` | Role one-hot | No | |
| 9 | `role_leaf` | Role one-hot | No | |
| 10 | `role_pe` | Role one-hot | No | Provider edge |
| 11 | `role_p` | Role one-hot | No | Provider core |
| 12 | `role_rr` | Role one-hot | No | Route reflector |
| 13 | `role_ce` | Role one-hot | No | Customer edge |
| 14 | `role_access` | Role one-hot | No | |
| 15 | `role_distribution` | Role one-hot | No | |
| 16 | `role_core` | Role one-hot | No | |
| 17 | `role_edge` | Role one-hot | No | |
| 18 | `role_other` | Role one-hot | No | Catch-all |
| 19 | `embedding_0` | Spectral embedding | No | Laplacian eigenmap dim 0 (computed by `bonsai_ml.embeddings`) |
| 20 | `embedding_1` | Spectral embedding | No | |
| 21 | `embedding_2` | Spectral embedding | No | |
| 22 | `embedding_3` | Spectral embedding | No | |

**Summary**: 1 structural + 6 vendor one-hot + 12 role one-hot + 4 spectral = 23 total.

---

## CV5 philosophy compliance check

| Requirement | Status | Evidence |
|---|---|---|
| Structural features dominate | **PASS** | 17/23 features (74%) are vendor-independent (degree + role + embedding). Vendor is 6/23 (26%). |
| Vendor identity is a small one-hot tail | **PASS** | 6 vendor dimensions, all binary. Maximum vendor contribution to embedding norm: 1 (single 1.0 in a one-hot). |
| Empirically validatable via feature ablation | **READY** | `eval.py` `GnnEvalReport.feature_ablation` dict + `run_comparison_study()` support ablation runs when training data exists. |
| Role is operator-defined, not vendor-inferred | **PASS** | `_role_feature()` maps role strings directly; vendor key has no bearing on role assignment. |
| Spectral embeddings are included | **PASS** | 4 Laplacian eigenmap dimensions from `bonsai_ml.embeddings.compute_spectral_embedding()`. |

---

## Gaps identified

| Gap | Severity | Proposed resolution |
|---|---|---|
| No `recent_event_rate` feature | Medium | Add as structural feature: events per node per hour from the StateChangeEvent log. Needs archive depth — defer to DV2. |
| No `time_since_last_event` feature | Medium | Add as structural feature: seconds since last StateChangeEvent for this node. Defer to DV2. |
| No `observed_protocol_set` one-hot | Low | BGP/BFD/IS-IS/OSPF flags per node. Can be derived from gNMI subscription paths currently active. Defer to DV2. |
| Vendor tail is 6 dims but only 4 supported vendors + FRR + other | Acceptable | Dimension count matches supported vendor families. No action needed. |
| `role_other` catch-all fires for unknown hostnames | Acceptable | LP-derived hostname inference (super/spine/leaf) reduces misses. Document as known behaviour. |

---

## Recommended additions for DV2 (when archive depth ≥30 days)

Extend `DEFAULT_FEATURE_NAMES` with:

```python
"recent_event_rate",       # events per node per hour (rolling 1-hour window)
"time_since_last_event",   # seconds since last StateChangeEvent
"observed_bgp",            # 1.0 if device has active BGP subscription
"observed_bfd",            # 1.0 if device has active BFD subscription
"observed_isis",           # 1.0 if device has active IS-IS subscription
```

This brings the total to 28 dimensions: 6/28 vendor (21%) and 22/28 structural (79%). Vendor tail
fraction decreases further as structural features increase — exactly the CV5 direction.

---

## Feature ablation plan (DV2, post-training)

When the first model trains, run the following ablations via `run_comparison_study()`:

1. **Ablate vendor one-hot** (set vendor dims to 0.0): verify F1 drops by <5% — confirms structural features dominate.
2. **Ablate role one-hot**: verify F1 drops — confirms role carries signal.
3. **Ablate spectral embedding**: verify F1 drops — confirms topology signal from embeddings.
4. **Ablate degree only**: low expected impact on its own.

Results go into `GnnEvalReport.feature_ablation` and into the model card.

---

## File reference

- Feature definitions: `python/bonsai_ml/gnn/data_loader.py:17-41` (`DEFAULT_FEATURE_NAMES`)
- Vendor mapping: `python/bonsai_ml/gnn/data_loader.py:43-58` (`VENDOR_FEATURES`)
- Role mapping: `python/bonsai_ml/gnn/data_loader.py:60-84` (`ROLE_FEATURES`)
- Spectral embeddings: `python/bonsai_ml/embeddings.py`
- Model scaffold: `python/bonsai_ml/gnn/model.py`
- Eval harness: `python/bonsai_ml/gnn/eval.py`
