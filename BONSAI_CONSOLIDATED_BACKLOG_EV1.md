# Bonsai — EV1 Backlog: ML Intelligence, GNN Architecture & BonPy–Bonsai Unification

> **Sprint**: EV1 (Embedded Vision 1)
> **Analysis basis**: Full audit of `python/bonsai_ml/`, `python/bonsai_sdk/`, `python/collector_engine.py`, `ui-bonpy/`, `src/graph/mod.rs`, `src/investigation_runtime.rs`, all rules in `python/bonsai_sdk/rules/`, `python/bonsai_sdk/training.py`, `python/bonsai_sdk/training_readiness.py`, `python/bonsai_ml/gnn/model.py`, `python/bonsai_ml/embeddings.py`, `python/bonsai_ml/feature_schema.py`, and the complete DV4 + DV4S backlog state.
> **Principle**: Every gap identified is grounded in actual code state. Architecture is additive — builds on DV4 infrastructure. No existing signal flows are broken. All ML items are off-by-default with explicit gates.

---

## Context: What Already Exists

Before listing gaps, it is critical to understand exactly what is already in place so we do not re-specify already-built work.

### Python ML layer (already shipped)
- `python/bonsai_ml/gnn/model.py` — `HeteroGNN` with `GATConv` (2-layer, 4-head), node types: `device/interface/bgp_neighbor/bfd_session`. Feature dim: device=23, interface=8, bgp_neighbor=6, bfd_session=4.
- `python/bonsai_ml/gnn/data_loader.py` — `BonsaiGnnDataLoader` with 23 device features (degree, vendor OHE ×6, role OHE ×12, embedding ×4). Loads from `runtime/chaos_runs/` snapshots + `chaos_log.jsonl`.
- `python/bonsai_ml/gnn/archive_to_training.py` — Snapshot discovery, chaos log join, label assignment (fault=1, clean=0), train/val/test split by time.
- `python/bonsai_ml/gnn/calibration.py` — `CalibrationStore`, 7-day calibration phase before production inference.
- `python/bonsai_ml/gnn/eval.py` — Evaluation metrics.
- `python/bonsai_ml/embeddings.py` — Spectral embedding (Laplacian eigenmaps via sklearn). Pushes to `/api/graph/embeddings/upsert`. Only topology-based (physical links).
- `python/bonsai_ml/feature_schema.py` — `FeatureSchema` with SHA-256 hash for drift detection. `SPECTRAL_V1_SCHEMA` defined.
- `python/bonsai_sdk/training.py` — Parquet export of `DetectionEvent` + normal windows + `Remediation` nodes. Two modes: anomaly / remediation.
- `python/bonsai_sdk/training_readiness.py` — `ReadinessCheck` thresholds (Model A: 50 anomaly rows + 200 normal; Model C: 50 success rows, 2 actions, 2 status classes).
- `python/bonsai_sdk/ml_detector.py` — `MLDetector` backed by joblib sklearn models (IsolationForest). 6-feature vector. Drops into `RuleEngine` seamlessly.
- `python/bonsai_sdk/engine.py` — `RuleEngine`: event loop + 30s poll loop. 14 rule modules. Loads ML models from `models/` at startup. Change-context annotation. Two background threads.
- `python/collector_engine.py` — Full sidecar: health HTTP on :9200, RegisterSidecar gRPC, 15s heartbeat, local graph write + core forwarding queue.

### BonPy UI (already shipped — rudimentary)
- `ui-bonpy/` — Svelte 5 app. Shows: StatusBanner, SidecarCard list (polls `/api/sidecars` every 5s), RuleFiringTable (polls `/api/detections`), MlModelPanel (static). No routing. No job control. No Parquet visibility. Footer literally says "editor / retraining / GNN console coming in CV8+".

### Rules (already shipped)
- 14 rule modules: `bgp.py`, `bfd.py`, `config.py`, `interface.py`, `optical.py`, `rack.py`, `syslog.py`, `snmp.py`, `streaming.py`, `topology.py`, `app.py`, `host.py`, `ddos.py`. All Python class-based detectors.
- Config-driven: rules live in Python source files. No DB-backed rule toggling (only `sidecar_rule_toggle` ConfigItem exists in Rust side, not wired to per-rule Python enable/disable).

### Integration gaps (root cause of all EV1 work)
1. **No production continuous run model** — sidecar is started manually via CLI. No systemd/supervisor wiring, no health watchdog with auto-restart.
2. **No Parquet observability** — training data exported via CLI (`python export_training.py`). No UI, no scheduling, no file catalog, no size/quality monitoring.
3. **GNN is static HeteroGAT (spatial only)** — no temporal dimension, no structural uncertainty, no attention explainability, no NCT (Noise-Contrastive Training) or control-weighted loss.
4. **Embeddings are spectral only** — CLI-run, topology-only. No text embeddings for syslog messages, device configs, or CLI outputs. These are extremely high-value signals that go completely unembedded.
5. **BonPy ↔ Bonsai integration is HTTP polling** — rules sidecar polls REST and calls gRPC. No shared event bus subscription, no SSE push, no DB-level integration.
6. **ML job lifecycle is invisible** — no job tracking, no schedule, no outcome capture, no retry on failure.
7. **Rules and playbooks are config files** — editing requires SSH + file edit + restart. No UI management, no per-rule DB enable/disable, no A/B testing between rule variants.

---

## Epic Overview

| Epic | Title | Priority |
|------|-------|----------|
| EV1-1 | GNN Architecture Upgrade: STGNN + Attention Explainability | P0 |
| EV1-2 | Parquet Pipeline Control: Catalog, Schedule, Quality Monitor | P0 |
| EV1-3 | Semantic Embeddings: Syslog, Config, CLI Text Vectorisation | P0 |
| EV1-4 | BonPy–Bonsai Deep Integration: Event Bus + DB Unification | P0 |
| EV1-5 | ML Job Engine: Scheduler, Lifecycle, Background Tasks | P1 |
| EV1-6 | BonPy UI Rewrite: Full MLOps Console (2026 Stack) | P1 |
| EV1-7 | Rule + Playbook Management: DB-Backed, UI-Controlled | P1 |
| EV1-8 | Structural Uncertainty: NCT, Control-Weighted GNN, Conformal | P2 |
| EV1-9 | Continuous Production Run: Sidecar Hardening + Watchdog | P1 |

---

---

## EV1-1 — GNN Architecture Upgrade: STGNN + Attention Explainability

### Analysis

The current GNN in `python/bonsai_ml/gnn/model.py` is a **2-layer heterogeneous GAT (HeteroGAT)** operating purely spatially — it processes one snapshot at a time, with no concept of how a device's embedding has changed over the last N snapshots. Node types are `device`, `interface`, `bgp_neighbor`, `bfd_session`. Feature dim is `device=23` (degree + vendor OHE + role OHE + 4 spectral embedding dims).

**Critical architectural gaps:**

1. **No temporal dimension** — Network anomalies are fundamentally temporal. A BGP session that has been stable for 30 days and then flaps is categorically different from one that has flapped 3 times this week. The current model processes each snapshot in isolation and cannot distinguish these. STGNN (Spatio-Temporal GNN) adds a temporal axis by stacking snapshots into a sequence and applying a temporal encoder (GRU, LSTM, or Transformer) on top of the spatial GNN output for each node.

2. **No attention explainability** — GAT computes attention weights between nodes but they are not exposed, persisted, or surfaced to the operator. The investigation runtime (`src/investigation_runtime.rs`) cannot explain *which neighbour devices contributed to a GNN anomaly score*. This is critical for NOC trust: "GNN says device X is anomalous because devices Y and Z, which are directly connected, both have degraded BGP sessions" is operationally useful. A black-box score is not.

3. **Node types too narrow** — Current node types exclude: `OspfNeighbor`, `BfdSession` (exists but 4-dim = useless), `SensorReading` (thermal), `AppFlow` (traffic), `StpInstance`, `Vrf`, `RedundancyGroup`, `ArpEntry`. These were all added to the graph schema in DV4 batches 10-23 but are completely absent from the GNN feature space.

4. **Feature dim is stale** — Device feature dim = 23 (degree + 6 vendor + 12 role + 4 embedding). In practice, after batch 23, a device node now has `model`, `serial_number`, `cpu_util_pct`, `memory_used_mb`, `memory_total_mb`, `uptime_seconds` — all highly anomaly-relevant but not in the feature vector.

5. **Edge types are structural only** — Current edges: `has_interface`, `has_bgp_neighbor`, `has_bfd_session`, `connected_to`. Missing: `HAS_OSPF_NEIGHBOR`, `HAS_BMP_SESSION`, `HAS_STP_INSTANCE`, `CARRIES_FLOW`, `MEMBER_OF` (RedundancyGroup), `APP_IMPACTED_BY_NETWORK`. These encode operational state that the GNN cannot see.

6. **No STGNN vs pure-attention decision documentation** — The choice between STGNN (sequential GNN over time windows), full attention-based (Transformer on graph patches), NCT (Noise-Contrastive Training for self-supervised pre-training), and Control-Weighted GNN (down-weight nodes in maintenance windows) is undocumented. Each has different trade-offs for a network-scale graph with O(100) nodes and high class imbalance (normal >> fault).

**Recommendation — STGNN with attention explainability:**
For Bonsai's scale (50–500 devices, 5–50 snapshots/day, high class imbalance), the optimal architecture is:
- **Spatial layer**: `HeteroGATv2Conv` (GAT v2, fixes rank collapse in GAT v1 — Brody et al. 2021) over the full heterogeneous graph.
- **Temporal layer**: Per-node `GRU` over the last `T=8` snapshots (30-minute windows = 4 hours of context). GRU is preferred over Transformer at this scale: fewer parameters, stable gradients, handles variable-length history via masking.
- **Attention capture**: At inference, store per-edge attention weights from `GATv2Conv` as a `GnnAttentionSnapshot` node in the graph, linked to the `Investigation` that triggered inference. The Rust investigation runtime queries these weights and injects them into the investigation prompt as "top contributing neighbours".
- **Self-supervised pre-training** (NCT): Before supervised fine-tuning on fault labels, pre-train on node-pair contrastive loss using the topology structure. This addresses the label sparsity problem — even after 6 months of operation, fault examples may be <5% of snapshots.

### Tasks

**T1 — Architecture decision record: STGNN vs alternatives** ✅ batch2
- Create `docs/architecture/adr_gnn_architecture_ev1.md` documenting:
  - **STGNN (GRU-over-GAT)**: Chosen baseline. Why: matches temporal anomaly nature, GRU stable at N<500 nodes, can be pre-trained self-supervised. Cons: requires snapshot buffer (8 snapshots × graph size ~= 4MB RAM).
  - **Pure attention (Graph Transformer)**: Evaluated. Rejected at this scale — O(N²) attention is wasteful for sparse topology graphs, requires large data volume for stable training. Revisit at EV3 when archive exceeds 180 days.
  - **STGNN with Transformer temporal encoder**: Revisit at EV2 if GRU temporal gain plateaus. Transformer temporal encoder needs T>16 to outperform GRU.
  - **NCT pre-training**: Adopted as Phase 1 (self-supervised). Supervised fine-tuning as Phase 2. Rationale: solves label sparsity.
  - **Control-Weighted GNN**: Adopted as training loss modifier. See EV1-8.
  - **Static HeteroGAT (current)**: Documented as baseline for ablation comparison.

**T2 — Extend node types and feature dimensions** ✅ batch6
- Update `python/bonsai_ml/gnn/model.py`:
  - Add node types: `ospf_neighbor`, `bfd_session` (expand dim 4→10), `redundancy_group`, `app_flow`, `sensor_reading`.
  - Updated `node_feature_dims`:
    - `device`: 23 → 36 (add: cpu_util_pct, memory_used_pct, uptime_log_secs, has_thermal_warning, bgp_session_count, ospf_neighbor_count, interface_count, bmp_session_count, gnn_quality_score, model_ohe×3, is_in_redundancy_group).
    - `interface`: 8 → 14 (add: in_error_rate, out_error_rate, in_utilization_pct, out_utilization_pct, is_in_lag, optical_rx_dbm_normalised).
    - `bgp_neighbor`: 6 → 12 (add: adj_rib_in_routes, loc_rib_routes, prefixes_rejected, hold_time, is_external, session_uptime_log_secs).
    - `bfd_session`: 4 → 10 (add: detect_multiplier, interval_ms, registered_protocols_count, is_up, is_in_redundancy_path, source_ohe).
    - `ospf_neighbor`: NEW, dim=8 (state, area, metric, dr_bdr_flag, uptime_log_secs, dead_interval, priority, retransmit_count).
    - `redundancy_group`: NEW, dim=6 (type_ohe×4: lag/vrrp/hsrp/ecmp, member_count, all_members_up_flag).
    - `sensor_reading`: NEW, dim=4 (temp_normalised, type_ohe×3: temp/fan/power, is_above_warning, is_above_critical).
  - Add edge types: `has_ospf_neighbor`, `has_bfd_session` (was stub), `member_of` (Device/Interface→RedundancyGroup), `carries_flow` (Device→AppFlow), `has_sensor` (Device→SensorReading).
  - Update `build_hetero_data()` to populate all new node/edge types from graph snapshot API.

**T3 — Implement STGNN temporal layer** ✅ batch2
- Create `python/bonsai_ml/gnn/stgnn.py`:
  - `SnapshotBuffer`: Ring buffer of the last `T=8` `HeteroData` objects per graph. Thread-safe. Serialisable to disk (`runtime/gnn_snapshot_buffer.pkl`) for restart recovery.
  - `TemporalGNNLayer`: Wraps `HeteroGNN` spatial encoder. For each node type, applies a per-type `nn.GRU(input_size=hidden_channels, hidden_size=hidden_channels, num_layers=1, batch_first=True)` over the T-snapshot sequence.
  - `STGNNModel`: Full model = `HeteroGNN` (T spatial encodings) → `TemporalGNNLayer` → `OutputHead`.
  - `forward(snapshot_sequence: list[HeteroData]) -> dict[str, Tensor]`: Processes sequence, returns per-node anomaly logits for the latest snapshot.
  - Fallback: If buffer has < T snapshots (cold start), pad with zero vectors. Model trained with masking to handle this gracefully.
  - `BonsaiGnnConfig` updated: add `temporal_window: int = 8`, `gru_num_layers: int = 1`.

**T4 — GATv2 upgrade (fix rank collapse)** ✅ batch2
- Replace `GATConv` → `GATv2Conv` from `torch_geometric.nn` in `model.py`.
- `GATv2Conv` uses dynamic attention (Brody et al. 2021): computes attention as `LeakyReLU(W·[h_i || h_j])` instead of `LeakyReLU(a·[Wh_i || Wh_j])`. This eliminates rank collapse where all nodes attend equally.
- Migration is a 1-line change per edge type. No change to model output shape.
- Update `BonsaiGnnConfig.num_heads` default: 4 → 8 (GATv2 more efficient per head at same depth).

**T5 — Attention weight capture and graph persistence** ✅ batch2
- In `stgnn.py`, add `return_attention_weights=True` to `GATv2Conv` forward calls.
- Create `AttentionSnapshot` dataclass: `{snapshot_ns, node_id, node_type, top_k_neighbours: list[{neighbour_id, neighbour_type, edge_type, weight}]}`.
- After inference, POST top-5 attention weights per anomalous node to new Rust endpoint `POST /api/gnn/attention` (see EV1-4 T4).
- These are stored as `GnnAttentionSnapshot` nodes in the graph (see EV1-4 T3).
- `investigation_runtime.rs` queries attention weights for the device under investigation and injects as context: "GNN attention: device X contributed 0.42, device Y contributed 0.31 — both have degraded BGP."

**T6 — Update data_loader for new feature dimensions** ✅ batch2
- `data_loader.py`: Update `DEFAULT_FEATURE_NAMES` list (23 → 36 dims for device).
- Update `build_hetero_data()` and `BonsaiGnnDataLoader.from_snapshot()` to populate new columns.
- Add `from_api_snapshot(client)` method that calls `/api/graph/snapshot` (new endpoint in EV1-4) to get a live snapshot dict for inference.
- Update `SPECTRAL_V1_SCHEMA` → `DEVICE_V2_SCHEMA` in `feature_schema.py` with new feature names and hash. Version bump prevents accidental cross-version inference.

**T7 — NCT pre-training scaffold** ✅ batch2
- Create `python/bonsai_ml/gnn/nct.py`:
  - `NodePairSampler`: Samples positive pairs (topologically adjacent nodes) and negative pairs (randomly sampled non-adjacent nodes) from `HeteroData`.
  - `NCTLoss`: Noise-contrastive loss = `-log(exp(sim(z_i, z_j+)) / sum(exp(sim(z_i, z_k-))))` where sim = cosine similarity on node embeddings.
  - `pretrain_nct(model, snapshots, epochs=50, lr=1e-3)`: Pre-trains the spatial GNN layers using NCT, freezes temporal layers.
  - Gate: pre-training only runs when `training_readiness.snapshot_count >= 30` (configurable). Below this threshold, model uses random init + supervised fine-tuning only.
  - Pre-trained weights saved to `models/nct_pretrain.pt`. Supervised fine-tuning loads and continues from these weights.

**T8 — Training pipeline integration** ✅ batch4
- Update `python/train_anomaly.py`:
  - Accept `--model-type` flag: `hetero_gat` (legacy), `stgnn` (new default).
  - Load `SnapshotBuffer` from archive, build temporal sequences.
  - Phase 1: NCT pre-training (if `pretrain.pt` not already current).
  - Phase 2: Supervised fine-tuning with fault labels from chaos log.
  - Save `models/stgnn_v1.pt` + updated `FeatureSchema`.
  - Emit `MlJobEvent` (new type, see EV1-5) on completion with metrics: val_auc, val_f1, num_training_snapshots.

**T9 — Model card update** ✅ batch4
- Update `python/bonsai_ml/model_cards/` with new card for `stgnn_v1`:
  - Architecture: STGNN (GATv2-GRU), 8-snapshot window, T nodes.
  - Training data: chaos archive snapshots + operator-labelled fault events.
  - Feature schema hash (from `FeatureSchema`).
  - Validation metrics: AUC, F1, precision, recall, FPR at threshold.
  - NCT pre-training summary: epochs, final NCT loss.
  - Known limitations: cold-start (< 8 snapshots), class imbalance (addressed by focal loss, see EV1-8 T3).
  - Threshold guidance: 0.7 for production (low FPR), 0.5 for investigation trigger.

---

---

## EV1-2 — Parquet Pipeline Control: Catalog, Schedule, Quality Monitor

### Analysis

Training data export today is entirely manual: `python export_training.py --output data/training.parquet`. This is a CLI script. There is no:
- **Parquet file catalog** — files are written to ad-hoc paths, no metadata tracking (when exported, how many rows, which time window, which model version they correspond to).
- **Scheduled export** — no cron or scheduler. An operator must remember to run export before training.
- **Quality gate on export** — `training_readiness.py` has thresholds but they are only checked when the operator explicitly runs `scripts/check_training_readiness.py`. Nothing blocks a training run on bad data.
- **Parquet file health monitor** — no visibility into whether the `.parquet` file is valid, has correct column schema, has class balance, has label drift over time.
- **Export history** — no record of "this model was trained on this Parquet file exported at this timestamp covering data from T1 to T2".
- **Incremental export** — every export is full. For a long-running deployment with 6 months of archive, a full export runs the entire Cypher query each time. No incremental append strategy.
- **Multi-dataset management** — `training.parquet` and `remediation.parquet` exist as files but there is no versioning, no `latest` symlink, no `archive/` of previous exports.

In production, you cannot expect an operator to SSH in and run a Python script to export training data before every training run. The entire pipeline must be observable and automated:

```
Graph (KuzuDB) → Parquet Export Job → Parquet Catalog (DB) → Readiness Check → Training Job → Model Artifact → Model Registry → Inference Engine
```

Every step in this chain must be:
1. **Visible** — status, health, last run time, next scheduled run visible in UI.
2. **Controllable** — can be triggered, paused, cancelled from UI without SSH.
3. **Auditable** — every export and training run has a record with inputs, outputs, quality metrics.

### Tasks

**T1 — Parquet Catalog DB schema (Rust side)** ✅ batch2
- Add to `src/graph/mod.rs` — new node tables (always created, idempotent):
  - `ParquetExport(id, export_type, output_path, started_at_ns, completed_at_ns, row_count, anomaly_rows, normal_rows, since_ns, until_ns, schema_hash, status, error_message, model_version_trigger)` — one row per export run.
  - `MlJobRun(id, job_type, started_at_ns, completed_at_ns, status, trigger, input_parquet_id, output_model_path, val_auc, val_f1, error_message, config_json)` — one row per ML job run (training, evaluation, embedding compute, NCT pre-train).
  - `ModelArtifact(id, model_type, version, path, feature_schema_hash, trained_at_ns, val_auc, val_f1, val_precision, val_recall, threshold, is_active, retired_at_ns, model_card_path)` — model registry.
  - `TRAINED_ON(ModelArtifact → ParquetExport)`, `SUCCEEDED_BY(ModelArtifact → ModelArtifact)` rel tables.
- `GET /api/ml/exports` — list all `ParquetExport` records, ordered by `started_at_ns` desc.
- `GET /api/ml/exports/{id}` — single export detail.
- `POST /api/ml/exports` — create new export job record (initial status=`pending`).
- `PATCH /api/ml/exports/{id}` — update status/row_count on completion.
- `GET /api/ml/models` — list `ModelArtifact` records.
- `GET /api/ml/models/active` — return currently active model per type.
- `POST /api/ml/models/{id}/activate` — set `is_active=true` for this model, `false` for previous.

**T2 — Export job runner with catalog integration** ✅ batch2
- Create `python/bonsai_ml/export_job.py`:
  - `ParquetExportJob` class:
    - `run(client, export_type, since_ns, until_ns, output_dir)` → `ExportResult`.
    - Before export: POST to `/api/ml/exports` to create a catalog record (status=`running`).
    - Calls `export_training_set()` or `export_remediation_training_set()` from `bonsai_sdk/training.py`.
    - After export: validates Parquet schema (check all required columns present, dtypes correct).
    - Runs `ReadinessCheck` thresholds from `training_readiness.py`. Stores results in catalog record.
    - PATCH `/api/ml/exports/{id}` with final status, row counts, schema hash, quality results.
    - Returns `ExportResult(id, path, row_count, quality_passed, quality_report)`.
  - `IncrementalExportJob`: Variant that reads `last_export_until_ns` from most recent catalog record and exports only the delta since that point.
  - `ExportQualityReport`: class_balance_ratio, label_drift_score (KL divergence from previous export), missing_column_list, row_count, schema_version.
- CLI entrypoint: `python -m bonsai_ml.export_job --type anomaly --incremental` (replaces `export_training.py`).

**T3 — Parquet file validator** ✅ batch2
- Create `python/bonsai_ml/parquet_validator.py`:
  - `validate_parquet(path: str, schema_version: str) -> ValidationResult`.
  - Checks: file exists, readable by pyarrow, column set matches `FeatureSchema.feature_names`, numeric columns have correct dtype (float32/int64), no all-null columns, row count > 0, label balance (anomaly %: min 5%, max 50%).
  - `compute_label_drift(current_path, previous_path) -> float`: Jensen-Shannon divergence on label distribution. Alert threshold: JS > 0.3.
  - `compute_feature_drift(current_path, previous_path) -> dict[str, float]`: Per-column population stability index (PSI). Alert threshold: PSI > 0.2 for any feature.
  - Results stored in `ParquetExport` record as `quality_json`.

**T4 — Scheduled export via ML Job Engine** ✅ batch3
- See EV1-5 for the job scheduler. The export schedule is defined as a `MlJobSchedule` record:
  - Anomaly export: daily at 02:00 UTC, incremental.
  - Remediation export: weekly on Sunday 02:00 UTC, full.
  - GNN snapshot export: every 4 hours (feeds STGNN training buffer).
- Schedule is configurable via `POST /api/ml/schedules` (see EV1-5).
- Export job fires → creates catalog record → runs export → updates catalog record → triggers readiness check → if passes, enqueues training job.

**T5 — Parquet archive management** ✅ batch2
- `python/bonsai_ml/parquet_store.py`:
  - `ParquetStore(root_dir)`: Manages `root_dir/parquet/` directory.
  - Directory layout:
    ```
    runtime/parquet/
      anomaly/
        2026-05-25T02:00:00Z_v1_8542rows.parquet
        latest -> 2026-05-25T02:00:00Z_v1_8542rows.parquet
      remediation/
        2026-05-19T02:00:00Z_v1_1203rows.parquet
        latest -> ...
      gnn_snapshots/
        2026-05-25T06:00:00Z_T8_snapshots.pkl
        latest -> ...
    ```
  - `get_latest(export_type)` → path.
  - `cleanup_old(export_type, keep_last_n=10)`: Removes files older than the last N, preserves symlink.
  - `list_exports(export_type)` → list of `{path, rows, ts, size_bytes}`.
  - Integrates with catalog: every file written updates the catalog record with `output_path`.

**T6 — GNN snapshot buffer serialisation** ✅ batch3
- Create `python/bonsai_ml/gnn/snapshot_store.py`:
  - `SnapshotStore(root_dir)`: Writes/reads serialised `HeteroData` snapshot sequences.
  - Format: Apache Arrow IPC (`.arrow` files) — not pickle. Reason: pickle is version-sensitive, Arrow is schema-stable and readable without PyTorch.
  - `write_snapshot(snapshot: HeteroData, timestamp_ns: int)`: Appends to rolling 8-snapshot buffer. Evicts oldest when buffer full.
  - `load_buffer() -> list[HeteroData]`: Returns T most recent snapshots in chronological order.
  - `get_buffer_health() -> dict`: Returns `{buffer_size, oldest_ns, newest_ns, gap_seconds_max, is_stale}`. Stale = newest snapshot older than 1 hour.
- Called from STGNN inference loop (EV1-9 T3) and from training pipeline.

**T7 — Export quality dashboard data endpoint** ✅ batch3
- `GET /api/ml/exports/quality` — Returns quality summary across all recent exports:
  - `{export_type, last_export_at, row_count, class_balance_pct, label_drift_score, feature_drift_worst_column, feature_drift_worst_psi, quality_passed, model_trained_on_this}`.
- Used by BonPy UI (EV1-6) to show Parquet health card on the MLOps dashboard.
- Also feeds a Prometheus metric: `bonsai_ml_parquet_quality_passed{export_type}` (0/1 gauge).

**T8 — Cross-export lineage tracking** ✅ batch3
- `GET /api/ml/lineage/{model_id}` — Returns full lineage chain:
  - Which `ParquetExport` records this model was trained on.
  - What time window they cover.
  - What `MlJobRun` produced this model.
  - Whether the model is currently active.
  - What detections this model has fired (count, last fired at).
- This answers the operator question: "Was this detection generated by a model trained on stale data?"

---

---

## EV1-3 — Semantic Embeddings: Syslog, Config, CLI Text Vectorisation

### Analysis

**This is arguably the most impactful missing capability in the entire ML stack.**

Today's `python/bonsai_ml/embeddings.py` computes **spectral (Laplacian) embeddings** — purely topology-based. These encode where a device sits in the physical graph (spine, leaf, edge, etc.) but say nothing about what that device is *doing* or *saying*.

What goes completely unembedded today:
1. **Syslog messages** — Every syslog event written to the graph has a `message` field (raw text). These messages contain the most operationally rich signals: process names, error codes, interface descriptions, memory addresses, version strings, crash reasons. Zero of this is vectorised.
2. **Device configurations** — `bootstrap_agent.py` learns device config via PyATS/Genie. CLI config text (BGP neighbour configs, routing policy configs, ACL entries) is stored as JSON blobs but never embedded. Two devices with structurally identical configs but different BGP communities behave differently under certain faults.
3. **CLI operational state** — Parsed Genie output for OSPF, BGP, MPLS includes string fields (state descriptions, policy names, interface names with vendor-specific conventions). These are normalised to structured fields in the graph but their semantic richness is lost.
4. **Remediation action text** — Playbook YAML `action` strings are plain text. Whether a remediation action is semantically similar to a previously successful one could be used to predict success probability, but there is no embedding.
5. **Detection reason strings** — `DetectionEvent.reason` is a free-form string generated by rules. NLP similarity between detection reasons could cluster related incidents. Today they are treated as opaque strings.

**Why embeddings are critical for GNN:**
The current device feature vector includes `embedding_0..3` (4 dimensions from spectral embedding). The actual semantic content — what vendor software version, what config — contributes 0 dimensions. For fault detection, a device running a buggy software version that crashes on certain BGP UPDATE patterns should have its version string embedded. When the GNN sees multiple devices with similar version embeddings failing simultaneously, it can generalise.

**Why this cannot be on the hot path:**
Embedding computation using a sentence-transformer or LLM encoder is CPU/GPU intensive (50-500ms per text sample). The hot path (syslog write in `write_blocking()`) must complete in <5ms to avoid backpressure. Therefore:
- **Hot path**: Write raw text to graph as normal. Tag the node/event with `needs_embedding=true`.
- **Background job**: Embedding worker polls for `needs_embedding=true` records every 60s, batches them (batch_size=64), calls embedding model, writes vectors back.
- **Inference path**: At GNN inference time, look up pre-computed embeddings from `DeviceEmbedding` / `EventEmbedding` nodes. If missing, use zero vector (no blocking wait).

**Model selection:**
- **Sentence-transformers `all-MiniLM-L6-v2`** (384 dims, 22MB): Best cost/quality for network log text. Fast on CPU (200 samples/sec). No GPU required for Bonsai-scale data volumes. Open source, offline, no API key.
- **`nomic-embed-text` via Ollama** (768 dims): If Ollama sidecar is already running (D4-3 T5 wired this in), zero-cost upgrade path. Better quality at 2× dim cost.
- **OpenAI `text-embedding-3-small`** (1536 dims): Optional, API-key gated. Use only for investigation prompts where quality matters more than cost. Never for bulk syslog embedding (cost prohibitive).
- **For BonPy context**: Use sentence-transformers as default (offline, free, no API dependency). Configurable via `[ml.embedding]` section in bonsai.toml / DB.

### Tasks

**T1 — Syslog message embedding pipeline** ✅ batch3
- Create `python/bonsai_ml/text_embeddings.py`:
  - `TextEmbedder`: wraps `sentence_transformers.SentenceTransformer('all-MiniLM-L6-v2')` (or Ollama/OpenAI via config).
  - `embed_batch(texts: list[str]) -> np.ndarray`: Returns float32 array (N × dim).
  - `EmbeddingConfig`: model_name, dim, batch_size=64, max_text_length=256, ollama_url, openai_api_key_env.
  - `load_from_config(config: dict) -> TextEmbedder`.
- Create `python/bonsai_ml/syslog_embedding_worker.py`:
  - Polls `GET /api/events/unembedded?type=syslog&limit=200` (new Rust endpoint, see T5).
  - For each batch: extract `message` field, call `TextEmbedder.embed_batch()`.
  - POST to `POST /api/events/embeddings` with `{event_id, vector, model_name, computed_at_ns}`.
  - Sleep 60s between batches. Exponential back-off on error.
  - Metrics: `events_embedded_total`, `embedding_latency_ms_p50`, `embedding_batch_size`.
  - Emits `MlJobEvent(type=embedding_batch, count=N)` (see EV1-5).

**T2 — Device config/CLI text embedding** ✅ batch3
- Extend `bootstrap_agent.py` `_seed_device()`: after seeding, POST device config summary text to new endpoint `POST /api/devices/{address}/config-text` (raw text of key config sections: BGP config, ISIS config, interface config). Mark device as `needs_config_embedding=true`.
- `python/bonsai_ml/config_embedding_worker.py`:
  - Polls `GET /api/devices/unembedded-config?limit=50`.
  - For each device: fetch config text from `GET /api/devices/{address}/config-text`.
  - Embed with `TextEmbedder`.
  - POST to `POST /api/devices/{address}/config-embedding` with `{vector, model_name, computed_at_ns, schema_hash}`.
  - Stores as `DeviceConfigEmbedding` node (separate from topology `DeviceEmbedding`).

**T3 — Graph schema extensions for embeddings** ✅ batch7
- Add to `src/graph/mod.rs`:
  - `EventEmbedding(id, event_id, event_type, model_name, dim, vector_json, computed_at_ns, schema_hash)` — text embedding for syslog/state-change events.
  - `DeviceConfigEmbedding(id, device_address, model_name, dim, vector_json, computed_at_ns, schema_hash)` — device config embedding (separate from topology spectral embedding `DeviceEmbedding`).
  - `RemediationEmbedding(id, remediation_id, action_text, model_name, dim, vector_json, computed_at_ns)` — action text embedding.
  - `EMBEDDED_AS(StateChangeEvent → EventEmbedding)`, `CONFIG_EMBEDDED_AS(Device → DeviceConfigEmbedding)` rel tables.
  - `StateChangeEvent` migration: add `needs_embedding: bool` column (default true for new events).
  - `Device` migration: add `needs_config_embedding: bool` column.

**T4 — Rust endpoints for embedding lifecycle** ✅ batch7
- Add to `src/http_server/` (new file `src/http_server/ml_embeddings.rs`):
  - `GET /api/events/unembedded?type=syslog&limit=N` — returns StateChangeEvents where `needs_embedding=true`, ordered by `occurred_at` desc.
  - `POST /api/events/embeddings` — batch upsert `EventEmbedding` records. Marks source events `needs_embedding=false`.
  - `GET /api/devices/unembedded-config?limit=N` — returns Devices where `needs_config_embedding=true`.
  - `POST /api/devices/{address}/config-embedding` — upsert `DeviceConfigEmbedding`.
  - `GET /api/ml/embeddings/stats` — returns `{syslog_embedded, syslog_pending, config_embedded, config_pending, last_embed_at, model_name}`.
- Wired in `src/http_server/mod.rs`.

**T5 — Embedding-augmented GNN feature vector** ✅ batch6
- Update `data_loader.py` `build_hetero_data()`:
  - Device feature vector: replace 4 spectral dims with `spectral_dims_4 + config_embedding_compressed_8` (PCA-compressed 384-dim config embedding → 8 dims using pre-computed PCA from training data). Total device dims: 36 → 40.
  - Rationale: 8 dims for config semantics captures vendor-specific config clusters (Nokia EVPN config vs Cisco MPLS config vs FRR BGP-only config) that predict failure modes.
- `python/bonsai_ml/gnn/embedding_pca.py`:
  - `EmbeddingPCA`: fits PCA on all `DeviceConfigEmbedding` vectors, reduces 384 → 8 dims.
  - Saved as `models/config_embedding_pca.pkl`.
  - `transform(embedding_vector)` → 8-dim reduced vector.
  - Retrained automatically when config embedding count increases by >10% (triggers from EV1-5 scheduler).

**T6 — Syslog cluster analysis via embeddings** ✅ batch3
- Create `python/bonsai_ml/syslog_cluster.py`:
  - `SyslogClusterer`: fetches recent `EventEmbedding` vectors from graph API. Runs `sklearn.cluster.MiniBatchKMeans(n_clusters=20)` on the embedding matrix.
  - Assigns `syslog_cluster_id` (0-19) to each embedded event.
  - Writes cluster assignments back via `PATCH /api/events/cluster-labels` (batch).
  - Cluster centroids stored as `SyslogCluster(id, centroid_json, label, event_count, top_event_types)` nodes.
  - Surface in investigation: "This syslog message is in cluster 7 (BGP notification storms). 15 similar messages seen on 3 devices in last 24h."
  - Refreshed weekly by ML job scheduler.

**T7 — Semantic similarity search for investigations** ✅ batch7
- `GET /api/ml/similar-events?event_id={id}&limit=10` — for a given event, returns the 10 most semantically similar events by cosine similarity on `EventEmbedding` vectors.
- Implementation: load all `EventEmbedding` vectors for the same `event_type`, compute cosine similarity, return top-k.
- Used by investigation runtime: when an investigation is triggered, inject "5 most similar historical events" with their resolution status. "Similar to: BGP notification storm on spine-2 on 2026-03-12 — resolved by soft-clear."

**T8 — Detection reason clustering** ✅ batch6
- `python/bonsai_ml/detection_clustering.py`:
  - Embeds `DetectionEvent.reason` strings using `TextEmbedder`.
  - Clusters using HDBSCAN (variable number of clusters, handles noise well — better than KMeans for sparse detection streams).
  - Assigns `incident_cluster_id` to each detection. Detections in the same cluster are likely the same root cause.
  - Feeds into BonPy investigation aggregation: "7 detections in the last hour all belong to cluster 3 (ISIS adjacency instability)."
  - New Rust `DetectionEvent` migration: add `incident_cluster_id: Option<String>` column.

---

---

## EV1-4 — BonPy–Bonsai Deep Integration: Event Bus + DB Unification

### Analysis

The current integration between Bonsai (Rust core) and BonPy (Python sidecar) is:
1. **Python polls REST** — `RuleEngine._event_loop()` calls `client.stream_events()` which is a gRPC streaming call. This works but is one-directional and polling-based on the Python side.
2. **Python writes via gRPC** — Detections forwarded to core via `DetectionEventIngest` gRPC stream. This is robust.
3. **Python queries REST** — For graph queries (BGP neighbour counts, topology, etc.), Python calls HTTP REST endpoints.
4. **No shared job state** — ML jobs running in Python have no visibility in the Bonsai DB. A training job that runs for 20 minutes writes its result to a local file. Bonsai knows nothing about it.
5. **No GNN inference → Bonsai feedback loop** — GNN anomaly scores are not written back to the graph. If the GNN scores a device at 0.87 (highly anomalous), Bonsai's investigation trigger does not see this. The investigation_trigger.rs in `src/investigation_trigger.rs` only fires on `detection_fired` events from rules, not GNN.
6. **No attention weights in graph** — GATv2 attention weights (which neighbours contributed) are computed but discarded.
7. **No model registry in graph** — `ModelArtifact` records (see EV1-2 T1) don't exist yet.
8. **BonPy lives in a different port** — `ui-bonpy` is a separate Vite app on a different port. There is no single-pane-of-glass. An operator has to switch browser tabs to check ML status while watching the network topology.

**The integration vision for EV1:**
BonPy and Bonsai must share a **single graph database** for all ML state. The Python ML layer writes to Bonsai's KuzuDB via API, not to a separate store. This means:
- GNN inference results → Bonsai graph → Bonsai investigation trigger can see GNN anomaly scores.
- ML job runs → Bonsai graph → Bonsai UI shows ML job status in its "Database" section.
- Attention weights → Bonsai graph → Bonsai investigation runtime injects them into AI prompts.
- The BonPy UI (EV1-6) is rebuilt as a **section within** or **deeply linked from** the main Bonsai UI, not a separate island.

**SSE push vs polling:**
Today's BonPy UI polls `/api/sidecars` every 5 seconds. For a production MLOps console, this is too slow for job progress feedback and too fast for idle state (wasted requests). The 2026 answer is **Server-Sent Events (SSE)** — already implemented in Bonsai for governance events (`D4-21 T2`). Extending SSE to ML job events enables real-time job progress bars, live training loss curves, live GNN inference alerts, all from a persistent HTTP connection without polling overhead.

### Tasks

**T1 — ML event SSE channel** ✅ batch3
- Extend `src/resource_governor.rs` → NEW `src/ml_event_bus.rs`:
  - `MlEventBus`: `tokio::sync::broadcast::channel(capacity=2048)`.
  - `MlEvent` enum variants:
    - `JobStarted{job_id, job_type, triggered_by}`
    - `JobProgress{job_id, step, total_steps, metric_name, metric_value}`
    - `JobCompleted{job_id, job_type, outcome, val_auc, val_f1, model_path}`
    - `JobFailed{job_id, job_type, error}`
    - `ExportStarted{export_id, export_type, estimated_rows}`
    - `ExportCompleted{export_id, row_count, quality_passed}`
    - `GnnInferenceCompleted{snapshot_ns, anomalous_devices: list, top_score: f64}`
    - `EmbeddingBatchCompleted{events_embedded, model_name}`
    - `ModelActivated{model_id, model_type, val_auc}`
    - `TrainingReadinessChanged{export_type, was_ready, is_ready}`
  - `GET /api/ml/events/stream` — SSE endpoint, subscribes to `MlEventBus`.
  - Python `MlEventEmitter` class (in `python/bonsai_ml/event_emitter.py`): HTTP POST to new `POST /api/ml/events/publish` which fanouts to SSE subscribers.
  - `src/lib.rs`: `pub mod ml_event_bus`, registered in `server_startup.rs`.

**T2 — GNN inference result write-back to graph** ✅ batch3
- Add to `src/graph/mod.rs`:
  - `GnnInferenceResult(id, snapshot_ns, model_id, device_address, anomaly_score, threshold, is_anomalous, top_contributing_device_1, top_contributing_device_2, attention_weight_1, attention_weight_2, inferred_at_ns)` — node table.
  - `GNN_SCORED(Device → GnnInferenceResult)` rel table.
- `POST /api/gnn/inference-results` — batch upsert inference results from Python. Called after every STGNN inference pass.
- `GET /api/gnn/inference-results?device_address={addr}&since_ns={ns}` — query inference history for a device.
- `src/investigation_trigger.rs` extended: in addition to watching `detection_fired` events, also watch `GnnInferenceResult` writes where `is_anomalous=true` AND `anomaly_score > gnn_trigger_threshold` (configurable, default 0.75). Auto-trigger investigation for high-scoring devices not already under investigation.

**T3 — Attention weight persistence** ✅ batch3
- Add to `src/graph/mod.rs`:
  - `GnnAttentionSnapshot(id, inference_result_id, source_device_address, neighbour_device_address, edge_type, attention_weight, snapshot_ns)` — top-k attention weights per device.
  - `HAS_ATTENTION(GnnInferenceResult → GnnAttentionSnapshot)` rel.
- `POST /api/gnn/attention` — batch upsert attention snapshots from Python.
- In `src/investigation_runtime.rs`: before building the investigation prompt, query:
  ```cypher
  MATCH (r:GnnInferenceResult {device_address: $addr})-[:HAS_ATTENTION]->(a:GnnAttentionSnapshot)
  WHERE a.snapshot_ns > $since_ns
  RETURN a.neighbour_device_address, a.edge_type, a.attention_weight
  ORDER BY a.attention_weight DESC LIMIT 5
  ```
  Inject as: "GNN attention context (most influential neighbours): device X (bgp_neighbor, weight=0.42), device Y (connected_to, weight=0.31), ..."

**T4 — Python GNN inference client** ✅ batch3
- Create `python/bonsai_ml/gnn/inference_client.py`:
  - `GnnInferenceClient(bonsai_client)`:
    - `run_inference(snapshot_buffer: list[HeteroData]) -> InferenceResult`.
    - Loads active `STGNNModel` from `ModelArtifact` (fetched via `/api/ml/models/active?type=stgnn`).
    - Runs `model.forward(snapshot_sequence)`.
    - Extracts per-device anomaly scores + attention weights.
    - POSTs results to `/api/gnn/inference-results` and `/api/gnn/attention`.
    - Emits `GnnInferenceCompleted` ML event via `MlEventEmitter`.
    - Returns `InferenceResult(anomalous_devices, scores, attention_by_device)`.
  - `run_inference_loop(client, interval_secs=300)`: Runs inference every 5 minutes in a background thread. Feeds `SnapshotBuffer` from live graph snapshots (see T6).

**T5 — Live graph snapshot API** ✅ batch3
- Add to `src/http_server/observability.rs`:
  - `GET /api/graph/snapshot` — Returns current graph state as a JSON dict suitable for `build_hetero_data()`:
    ```json
    {
      "snapshot_ns": 1748190000000000000,
      "devices": [...],
      "links": [...],
      "bgp_neighbors": [...],
      "bfd_sessions": [...],
      "ospf_neighbors": [...],
      "redundancy_groups": [...],
      "sensor_readings": [...],
      "interfaces": [...]
    }
    ```
  - Each device object includes all fields needed for the 40-dim feature vector (degree, vendor, role, cpu_util_pct, memory_used_pct, uptime_seconds, bgp_session_count, ospf_neighbor_count, etc.).
  - Cached for 30s (served from in-memory snapshot, refreshed on a background timer).
  - Python STGNN inference loop calls this endpoint every `inference_interval` seconds to get fresh snapshots for the buffer.

**T6 — Shared topology snapshot for STGNN buffer** ✅ batch3
- `python/bonsai_ml/gnn/snapshot_client.py`:
  - `GraphSnapshotClient(bonsai_http_url)`:
    - `fetch_snapshot() -> dict`: GET `/api/graph/snapshot`, returns raw dict.
    - `to_hetero_data(snapshot_dict) -> HeteroData`: Calls `build_hetero_data()`.
    - `run_snapshot_loop(buffer: SnapshotBuffer, interval_secs: int)`: Background thread fetching snapshots, appending to `SnapshotBuffer`, saving to `SnapshotStore`.

**T7 — BonPy UI port unification** ✅ batch6
- The `ui-bonpy/` Vite app is built as a **standalone SPA** accessible at `/bonpy/` on the main Bonsai HTTP server (via static file serving in `src/http_server/mod.rs`).
- Remove the separate `ui-bonpy` port. Instead:
  - `BONSAI_HTTP_ADDR` serves both `/` (main UI) and `/bonpy/` (BonPy MLOps console).
  - Add to `src/http_server/mod.rs`: static file route for `/bonpy/` pointing to built `ui-bonpy/dist/`.
  - The main Bonsai UI `App.svelte` adds a "ML Console" nav entry linking to `/bonpy/`.
  - BonPy UI adds "← Network View" link back to the main UI.
  - Shared Bonsai API base URL — BonPy UI makes all API calls to the same origin, no CORS issues.
- `ui-bonpy/vite.config.js`: Update `base: '/bonpy/'`, add `build.outDir: '../dist-bonpy'` (output into `dist-bonpy/` at repo root, Rust serves from there).

**T8 — Unified authentication** ✅ batch3
- BonPy UI reuses Bonsai session tokens (from `D4-3 T2`). When served from the same origin, the session cookie (`bst_*`) applies to all `/api/` calls including ML endpoints.
- Python sidecar authenticates to Bonsai API with a scoped API key (`bsk_*`) with role `ApiReadonly` for query endpoints and a separate key with role `Operator` for write endpoints (inference results, training job records).
- Keys provisioned via `POST /api/auth/apikeys` with `scope=ml_sidecar`.

---

---

## EV1-5 — ML Job Engine: Scheduler, Lifecycle, Background Tasks

### Analysis

The current ML "scheduler" is: the operator runs a script. There is no scheduler. There is no job queue. There is no retry. There is no dependency chain ("only train if export succeeded"). This is not production-grade.

In 2026, the accepted pattern for Python background jobs that need:
- UI visibility
- Scheduling (cron-like)
- Retry on failure
- Progress streaming
- Dependency ordering
- Persistence across restarts

is **Celery + Redis** (mature, 12+ years, massive ecosystem) OR **Celery + RabbitMQ** OR, for a simpler embedded solution, **APScheduler + SQLAlchemy job store**. For Bonsai's scale (O(10) jobs/day, not O(10,000)), the right choice is:

**APScheduler 4.x** (pure Python, no broker dependency) with job state persisted to a SQLite job store at `runtime/ml_jobs.db`. APScheduler 4.x supports async schedulers, per-job result storage, and event callbacks. Combined with the `MlEventBus` (EV1-4 T1) for real-time progress streaming, this gives full MLOps visibility without introducing Redis/RabbitMQ operational overhead.

**Why not Celery?** Celery requires a separate broker (Redis or RabbitMQ), a separate result backend, and a separate worker process. For Bonsai's embedded deployment model (single-box common, no external services), this is too heavy. If Bonsai grows to multi-node distributed training, Celery can be introduced at EV3.

**Why not Prefect / Airflow?** Same reason — heavyweight, require their own servers, not embeddable.

**APScheduler 4.x + SQLite job store** gives:
- Cron triggers, interval triggers, one-off triggers.
- Persistent job store: scheduler restart restores all scheduled jobs from SQLite.
- Per-job execution records (last_run, next_run, last_outcome).
- Python-native: lives in `python/bonsai_ml/job_engine.py`, no new infrastructure.

**Job dependency chain:**
```
[Cron trigger: daily 02:00]
    → IncrementalExportJob (anomaly)
        → on_success: check TrainingReadiness
            → if ready: TrainingJob (stgnn)
                → on_success: EmbeddingPCARefreshJob
                    → on_success: ModelActivateJob (if val_auc > threshold)
[Cron trigger: every 5min]
    → GnnInferenceJob (uses active model + snapshot buffer)
[Cron trigger: every 60s]
    → SyslogEmbeddingWorkerJob (batch embed pending events)
[Cron trigger: every 4h]
    → GraphSnapshotCaptureJob (feed STGNN buffer)
[Cron trigger: weekly Sunday]
    → RemediationExportJob
    → DetectionClusteringJob
```

### Tasks

**T1 — ML Job Engine core** ✅ batch3
- Create `python/bonsai_ml/job_engine.py`:
  - `BonsaiJobEngine`:
    - Uses `apscheduler.AsyncScheduler` with `SQLiteJobStore(path="runtime/ml_jobs.db")`.
    - `register_job(job_id, fn, trigger, ...)`: registers a job. Idempotent — if job already exists with same trigger, skip.
    - `trigger_job(job_id)`: one-off trigger (for UI-initiated runs).
    - `cancel_job(job_id)`: cancel a running or scheduled job.
    - `get_job_status(job_id) -> JobStatus`.
    - `list_jobs() -> list[JobStatus]`.
  - `JobStatus(job_id, state, last_run_at, next_run_at, last_outcome, error_message, run_count)`.
  - `JobState` enum: `idle`, `running`, `succeeded`, `failed`, `cancelled`.
  - On job start: POST `MlJobRun` record to Bonsai via `/api/ml/jobs` (see T2). Emit `JobStarted` ML event.
  - On job complete: PATCH `MlJobRun` record. Emit `JobCompleted` or `JobFailed` ML event.
  - Engine runs in a dedicated asyncio event loop in a background thread, started from `collector_engine.py` main.

**T2 — Rust ML job run API** ✅ batch3
- Add to `src/http_server/` (new `src/http_server/ml_jobs.rs`):
  - `MlJobRunRecord` struct (mirrors `MlJobRun` graph node from EV1-2 T1).
  - `POST /api/ml/jobs` — create new job run record.
  - `PATCH /api/ml/jobs/{id}` — update status, metrics.
  - `GET /api/ml/jobs` — list all job runs, paginated.
  - `GET /api/ml/jobs/{id}` — single job run detail.
  - `GET /api/ml/jobs/schedules` — list registered job schedules (polled from Python via `/api/ml/schedules` endpoint, see T3).
  - `POST /api/ml/jobs/{id}/cancel` — cancel request (sets a `cancel_requested` flag; Python engine polls this).
- Wired in `src/http_server/mod.rs`.

**T3 — Schedule management API** ✅ batch3
- Add `MlJobSchedule(id, job_id, cron_expr, enabled, last_modified_by, last_modified_at)` to graph schema.
- `GET /api/ml/schedules` — list all schedules.
- `POST /api/ml/schedules` — create or update schedule `{job_id, cron_expr, enabled}`.
- `DELETE /api/ml/schedules/{id}` — remove schedule.
- Python engine on startup: reads all schedules from `/api/ml/schedules`, registers them with APScheduler. On schedule PATCH: Python engine polls for changes every 60s (or receives SSE notification).
- Default schedules created at first startup (if not already in DB):
  ```
  anomaly_export_daily:    cron(hour=2, minute=0)
  remediation_export_weekly: cron(day_of_week=0, hour=2, minute=0)
  gnn_inference:           interval(minutes=5)
  syslog_embedding:        interval(seconds=60)
  graph_snapshot:          interval(hours=4)
  detection_clustering:    cron(day_of_week=0, hour=3, minute=0)
  config_embedding:        interval(hours=6)
  ```

**T4 — Job progress streaming** ✅ batch3
- `python/bonsai_ml/job_progress.py`:
  - `JobProgressReporter(job_id, emitter: MlEventEmitter)`:
    - `report(step, total_steps, metric_name=None, metric_value=None)`: Emits `JobProgress` ML event.
    - `set_total(n)`: Set total steps after start (for jobs where total is unknown until started).
  - Used by all job functions as a dependency-injected progress reporter.
  - Training job reports: step=epoch, total_steps=max_epochs, metric_name="loss", metric_value=loss.
  - Export job reports: step=rows_written, total_steps=estimated_rows.
  - Embedding job reports: step=events_embedded, total_steps=pending_events.

**T5 — Job dependency chain wiring** ✅ batch3
- In `job_engine.py`, implement `on_job_success_trigger(parent_job_id, child_job_id, condition_fn)`:
  - After `parent_job_id` completes with state=`succeeded`, evaluate `condition_fn(job_result)`.
  - If True: trigger `child_job_id` immediately (one-off).
  - Example: after `anomaly_export_daily` succeeds, check `result.quality_passed == True`, if so trigger `stgnn_training`.
- Implement the full dependency chain documented in the Analysis section.
- Dependency chain is stored as `MlJobDependency(parent_job_id, child_job_id, condition_json)` nodes in Bonsai graph.

**T6 — Job retry and dead-letter handling** ✅ batch3
- APScheduler 4.x supports `max_instances` and `misfire_grace_time`. Extend with:
  - `max_retries=3` per job. On failure: exponential back-off (5min, 15min, 45min).
  - On `max_retries` exhausted: job state = `failed`, ML event emitted, BonPy UI shows alert.
  - `DeadLetterJob` queue: list of jobs that exhausted retries, with error details. Viewable in BonPy UI.
  - `POST /api/ml/jobs/{id}/retry` — operator can manually retry a dead-letter job.

**T7 — Resource governor integration** ✅ batch6
- Extend `src/resource_governor.rs` (already exists from D4-21):
  - New pressure source: `MlJobPressure`. When `should_shed()` is true (memory pressure), the ML job engine pauses non-critical jobs (training, clustering). Only inference and embedding workers continue (they are smaller).
  - Python job engine polls `GET /api/governance/pressure` every 30s. Pauses heavy jobs when `write_pressure=true`.
  - Resumes when pressure clears. In-progress jobs are not killed, only new job starts are blocked.

**T8 — Unified observability: Prometheus metrics** ✅ batch3
- Python `job_engine.py` exposes Prometheus metrics on `:9201/metrics`:
  - `bonsai_ml_job_runs_total{job_id, outcome}` — counter.
  - `bonsai_ml_job_duration_seconds{job_id}` — histogram.
  - `bonsai_ml_job_last_success_timestamp{job_id}` — gauge (Unix epoch of last success).
  - `bonsai_ml_parquet_rows_exported{export_type}` — gauge (latest export row count).
  - `bonsai_ml_model_val_auc{model_type}` — gauge (active model AUC).
  - `bonsai_ml_gnn_anomalous_devices_total` — counter (cumulative GNN anomaly detections).
  - `bonsai_ml_pending_embeddings{embedding_type}` — gauge (unembedded events/devices).
- Scrape target added to `docker/prometheus/prometheus.yml`.

---

---

## EV1-6 — BonPy UI Rewrite: Full MLOps Console (2026 Stack)

### Analysis

The current `ui-bonpy` is a single `App.svelte` file with 4 components: `StatusBanner`, `SidecarCard`, `RuleFiringTable`, `MlModelPanel`. The footer literally says "editor / retraining / GNN console coming in CV8+". It is a placeholder.

**What a 2026 MLOps console must show for this domain:**

1. **Sidecar Health** (already exists, good) — keep, improve.
2. **Live detection stream** (partial) — needs severity filtering, device filter, rule filter, SSE push instead of polling.
3. **Parquet export status** — completely missing. Show: last export timestamp, row count, quality gate result, class balance, next scheduled export, manual trigger button.
4. **Training job history** — completely missing. Show: job list with start/end time, outcome, val_AUC/F1, training duration, model produced.
5. **Active models** — incomplete `MlModelPanel`. Need: model type, version, feature schema hash, val metrics, threshold, when activated, detections fired, model card link.
6. **GNN inference live feed** — completely missing. Show: last inference result (which devices were scored anomalous, scores, top contributing neighbours), inference interval, model used.
7. **Embedding health** — completely missing. Show: pending embeddings count per type, last batch time, embedding model name, batch latency.
8. **Job scheduler** — completely missing. Show: cron schedule for each job, enabled/disabled toggle, last run, next run, manual trigger button.
9. **Rule/playbook management** — see EV1-7 (separate epic, surfaced in BonPy UI too).
10. **Training data explorer** — basic Parquet quality visualisation: class distribution bar chart, feature distribution for top-10 features, drift vs previous export.

**Technology choices for 2026:**
- **SvelteKit** (replaces plain Svelte 5 SPA): file-based routing, server-side rendering option, built-in page transitions. The current `ui-bonpy` is a single-page app with no routing — SvelteKit solves this cleanly without React overhead.
- **SSE (EventSource API)**: Already used in main Bonsai UI for governance events. BonPy UI subscribes to `GET /api/ml/events/stream` for real-time job progress, GNN alerts, embedding completion.
- **TanStack Query (svelte-query)**: Replaces manual `setInterval` polling. Smart cache invalidation, background refetch, loading/error states. Works with SvelteKit.
- **Chart.js via `svelte-chartjs`**: Lightweight charts for training loss curves, class balance bars, drift sparklines. No D3 needed (D3 is already used in main UI for topology canvas).
- **Shared design tokens**: Reuse `ui/src/app.css` tokens (`--accent-primary`, `--bg-base`, `--border`, etc.) in BonPy UI. Same look-and-feel, same font.

**Architecture:**
```
/bonpy/                    → Dashboard (sidecar health, summary cards)
/bonpy/jobs                → Job scheduler + history
/bonpy/models              → Model registry + activation
/bonpy/exports             → Parquet catalog + quality
/bonpy/gnn                 → GNN inference live feed + attention viz
/bonpy/embeddings          → Embedding health + cluster explorer
/bonpy/rules               → Rule management (see EV1-7)
/bonpy/detections          → Detection stream with ML annotations
```

### Tasks

**T1 — Migrate to SvelteKit** ✅ batch3
- Convert `ui-bonpy/` from plain Svelte SPA → SvelteKit project:
  - `svelte.config.js`: adapter-static (for serving from Rust static file server).
  - `src/routes/` directory structure: `+layout.svelte`, `+page.svelte` per route.
  - Shared layout `+layout.svelte`: nav sidebar (same style as main UI), SSE connection manager.
  - Base path: `base: '/bonpy'` in `svelte.config.js`.
  - Build output: `dist-bonpy/` (Rust serves from here at `/bonpy/`).
  - Port forward: `vite.config.js` proxy `/api/` → `http://localhost:3000/api/` in dev.
  - Dependencies: `@sveltejs/kit`, `@sveltejs/adapter-static`, `@tanstack/svelte-query`, `svelte-chartjs`, `chart.js`. Pin versions in `package.json`.

**T2 — Dashboard page (/bonpy/)** ✅ batch3
- `src/routes/+page.svelte` — Summary dashboard:
  - **System health strip**: 5 status indicators (sidecars, active model, last GNN inference, last export, job engine). Each: green/amber/red + last-seen timestamp.
  - **Active detections card**: count by severity (critical/high/warn/info) with sparkline (last 24h).
  - **GNN status card**: active model version, last inference time, number of anomalous devices in last inference, top anomalous device + score.
  - **Parquet freshness card**: last anomaly export age (hours), row count, quality badge (PASS/FAIL/STALE).
  - **Next scheduled jobs**: list of next 5 jobs to run with countdown timer.
  - All cards use SSE for live update (no polling on dashboard).

**T3 — Jobs page (/bonpy/jobs)** ✅ batch3
- `src/routes/jobs/+page.svelte`:
  - **Schedule table**: job_id, cron_expr, enabled toggle, last_run, next_run, last_outcome badge, "Run Now" button.
  - **Job run history table**: job_id, started_at, duration, outcome (success/failed/cancelled), metrics (val_auc, row_count), expand for full details.
  - **Active job progress panel**: SSE-driven live progress bar for any running job. Shows current step/total, current metric (e.g., "epoch 12/50, loss=0.042"), elapsed time, cancel button.
  - **Dead letter queue**: failed jobs with retry button.
  - Schedule edit modal: cron expression editor with human-readable preview (e.g., "At 02:00 AM, daily").

**T4 — Models page (/bonpy/models)** ✅ batch3
- `src/routes/models/+page.svelte`:
  - **Model registry table**: model_type, version, val_AUC, val_F1, threshold, trained_at, is_active badge, "Activate" button (with confirmation modal), "View Card" button.
  - **Active model detail panel**: feature schema hash, training data lineage (which Parquet exports), training duration, calibration period (was there one?), detections fired since activation.
  - **Model comparison**: select 2 models → side-by-side val metrics table.
  - **Version history chart**: line chart of val_AUC over training runs (using Chart.js). Shows if model quality is improving over time.

**T5 — Exports page (/bonpy/exports)** ✅ batch3
- `src/routes/exports/+page.svelte`:
  - **Export catalog table**: export_type, started_at, row_count, anomaly%, normal%, quality badge, schema_hash, linked model (if this export was used for training).
  - **Quality detail modal**: per-export quality report — class balance bar, label drift from previous export (KL divergence), missing columns list, feature PSI table.
  - **Export schedule section**: shows next anomaly and remediation export times.
  - **Manual export trigger**: "Export Now" button for each export type. Shows progress via SSE.
  - **Parquet file browser**: list files in `runtime/parquet/`, show size, age, quick stats.

**T6 — GNN page (/bonpy/gnn)** ✅ batch3
- `src/routes/gnn/+page.svelte`:
  - **Inference timeline**: stacked bar chart (Chart.js) — per inference run: count of anomalous devices vs total. Last 24 runs.
  - **Latest inference results table**: device_address, anomaly_score (colour-coded), threshold, is_anomalous, top contributing neighbour + weight, inference time.
  - **Snapshot buffer health**: buffer_size/8 (progress bar), oldest_snapshot_age, newest_snapshot_age, is_stale warning.
  - **Attention graph mini-viz**: for selected anomalous device, show a small force-directed graph (using D3 or SVG) of the top-5 contributing neighbours with edge width proportional to attention weight.
  - **GNN anomaly → Investigation linkage**: if a GNN detection triggered an investigation, show "View Investigation" link.
  - **Inference settings**: model_id selector, threshold slider, inference interval setting, "Run Now" button.

**T7 — Embeddings page (/bonpy/embeddings)** ✅ batch3
- `src/routes/embeddings/+page.svelte`:
  - **Embedding health cards**: for each type (syslog, config, remediation):
    - Embedded count, pending count, last batch time, model name.
    - Throughput gauge (events/hour over last 24h).
  - **Syslog cluster explorer**: cluster list with cluster_id, event_count, top_event_types. Click cluster → see sample events.
  - **Config embedding space**: 2D UMAP projection of device config embeddings (pre-computed, not live). Hover to see device_address + vendor.
  - **Embedding drift monitor**: line chart of per-cluster size over time (cluster stability). If a cluster grows rapidly, it may indicate a new failure mode.

**T8 — Detection stream page (/bonpy/detections)** ✅ batch3
- `src/routes/detections/+page.svelte`:
  - SSE-driven live detection stream (subscribes to Bonsai SSE `/api/events/stream` filtered to `detection_fired`).
  - Per-detection row: device, rule_id, severity badge, reason (truncated), occurred_at, change-correlated flag, GNN score (if available for that device at that time), incident_cluster_id.
  - Filters: severity, device, rule_id, time range, ML-annotated only, change-correlated only.
  - Expand row: full features JSON, investigation link, remediation proposals.
  - "Bulk acknowledge" checkbox for NOC workflow.

**T9 — Shared SSE connection manager** ✅ batch3
- `src/lib/SseManager.js`:
  - Single `EventSource` connection per page to `/api/ml/events/stream`.
  - Dispatches events to Svelte stores keyed by `event_type`.
  - Auto-reconnects with exponential back-off (1s, 2s, 4s, max 30s).
  - Heartbeat detection: if no event for 60s, assume dead and reconnect.
  - Connection status exposed via `sse_connected` Svelte store — shown in header.

**T10 — Shared layout + nav** ✅ batch3
- `src/routes/+layout.svelte`:
  - Left sidebar nav: Dashboard, Jobs, Models, Exports, GNN, Embeddings, Rules, Detections.
  - Header: "bonpy MLOps" brand, connection status dot (green/amber/red for SSE + API health), "← Network View" link to main Bonsai UI at `/`.
  - Global error toast for failed API calls.
  - `SseManager` initialised here, shared across all routes.
  - Responsive: sidebar collapses to icons on narrow screens.

---

---

## EV1-7 — Rule + Playbook Management: DB-Backed, UI-Controlled

### Analysis

All 14 rule modules in `python/bonsai_sdk/rules/` are Python source files. Editing a rule means:
1. SSH into the server.
2. Edit the Python file.
3. Restart `collector_engine.py`.

This is not acceptable in production. The NOC should be able to:
- Enable/disable individual rules without touching code.
- Adjust per-rule thresholds (e.g., change BGP flap count from 3 to 5 without code change).
- See what each rule fires on, its false-positive rate, when it last fired.
- A/B test a modified rule against the original (shadow mode: new rule fires but doesn't create detection).
- Create new syslog pattern rules entirely from UI without any Python.

**What already exists:**
- `D4-9 T4` (Batch 18): `GET /api/sidecar/rules` + `POST /api/sidecar/rules/{rule_id}/toggle` — sets `enabled` state via `ConfigItem` (config_class=sidecar_rule_toggle). The Python sidecar in `collector_engine.py` does not actually read these toggle states — the Rust side stores them but the Python `RuleEngine` loads all rules unconditionally from source. **This toggle mechanism is wired on the Rust side but not consumed by Python.** This is the root gap to fix.
- `D4-7 T4` (Batch 18): `SynthesizerRules.svelte` in main UI — shows ConfigItem-based rule/profile viewer. Edit JSON content, enable/disable. But this operates on Rust-side synthesizer rules (Cypher-based event synthesizers), not on Python rule detectors.
- `playbooks/library/` — 10 YAML playbook files + gap analysis. No UI management. No DB backing. Loading a playbook requires: edit YAML, reload (via config reload API).

**The gap**: Python `RuleEngine` needs to:
1. Consult DB at startup + periodically to check which rules are enabled/disabled.
2. Read per-rule parameter overrides from DB (e.g., flap threshold, cooldown window).
3. Support hot-reload of rule enable/disable without restart.
4. Support shadow-mode rules (fire but don't create detection, just log for FPR measurement).

**New syslog pattern rules from UI**: The `config/syslog_patterns/*.yaml` files are loaded into DB at boot (D4-7 T1). The `SynthesizerRules.svelte` page already edits these. What's missing is:
1. Creating a new syslog pattern rule entirely via UI (fill form → write to DB → Rust picks it up at next reload).
2. The Python sidecar having its own syslog pattern evaluator that reads from DB (rather than loading from YAML files).

**Playbook lifecycle**:
Playbooks should be first-class DB entities, not YAML files. The `YAML→DB migration` from D4-7 T2 already migrates syslog patterns and path profiles at boot. Playbooks need the same treatment. DB-backed playbooks means:
- Version-controlled via `updated_at_ns` + `updated_by`.
- Editable from UI without filesystem access.
- Per-playbook enabled/disabled flag.
- Execution count + success rate tracked automatically.

### Tasks

**T1 — Python RuleEngine DB-backed enable/disable** ✅ batch5
- Update `python/bonsai_sdk/engine.py` `RuleEngine.__init__()`:
  - After loading rules, call `_load_rule_overrides(client)`:
    - `GET /api/sidecar/rules` → returns list of `{rule_id, enabled, parameters_json}` from ConfigItem DB.
    - For each rule: if `enabled=false`, remove from `self._rules` list.
    - If `parameters_json` present, call `rule.apply_parameters(params_dict)` (see T2).
  - Start background thread `_rule_override_poller()`: every 60s, re-fetch overrides and apply diff:
    - New disabled rules: remove from `self._rules`.
    - Re-enabled rules: instantiate and add back to `self._rules`.
    - Updated parameters: call `rule.apply_parameters()` on affected rules.
    - No restart required. Thread-safe: use `threading.Lock` around `self._rules` list mutations.

**T2 — Per-rule parameter overrides** ✅ batch5
- Add `apply_parameters(params: dict) -> None` abstract method to `Detector` ABC in `detection.py`.
- Default implementation: no-op. Rules that expose tunable parameters override this.
- Implement in key rules:
  - `BGP_RULES[bgp_session_flap]`: `flap_count_threshold` (default 3), `flap_window_seconds` (default 300).
  - `INTERFACE_RULES[interface_error_spike]`: `error_rate_threshold_pct` (default 5.0), `window_seconds` (default 60).
  - `INTERFACE_RULES[interface_high_utilization]`: `utilization_threshold_pct` (default 80.0).
  - `STREAMING_RULES[srlg_risk_detected]`: `risk_threshold_percentage` (default 50).
  - `OPTICAL_RULES[optical_rx_power_low]`: `rx_power_dbm_threshold` (default -20.0).
- `GET /api/sidecar/rules/{rule_id}/parameters` — returns current parameters + defaults.
- `PATCH /api/sidecar/rules/{rule_id}/parameters` — update parameters in ConfigItem DB.

**T3 — Shadow mode for rules** ✅ batch5
- Add `shadow_mode: bool` field to `Detector` ABC. Default: False.
- Shadow mode behaviour: `detect()` is called normally, but if `shadow_mode=True`, the returned `reason` is NOT passed to `on_detection`. Instead, it is logged to a `ShadowDetection` list in memory.
- `GET /api/sidecar/rules/{rule_id}/shadow-firings?since=ns` — returns shadow firings for a rule in shadow mode. Used to measure FPR before promoting a rule to production.
- `POST /api/sidecar/rules/{rule_id}/shadow-mode` with `{enabled: bool}` — toggle shadow mode. Writes to ConfigItem DB. Picked up by rule override poller.
- BonPy UI rule detail page shows shadow firings as a separate stream with "Promote to production" button.

**T4 — Playbook DB migration and CRUD** ✅ batch5
- Extend Rust `src/graph/mod.rs`:
  - `Playbook(id, name, description, rule_ids_json, vendor, steps_json, verify_graph, enabled, version, created_at_ns, updated_at_ns, updated_by, execution_count, success_count)` node table.
  - Migrations for columns if table already exists.
- Boot-time migration (extend `migrate_yaml_config()` from D4-7):
  - Read all `playbooks/library/*.yaml` files.
  - Upsert as `Playbook` nodes (idempotent — don't overwrite if `updated_by != "boot_migration"`).
- HTTP endpoints (new `src/http_server/playbooks.rs`):
  - `GET /api/playbooks` — list all playbooks with execution stats.
  - `GET /api/playbooks/{id}` — full playbook detail.
  - `POST /api/playbooks` — create new playbook.
  - `PUT /api/playbooks/{id}` — update playbook (bumps version, sets updated_by).
  - `DELETE /api/playbooks/{id}` — soft delete (sets enabled=false).
  - `POST /api/playbooks/{id}/test` — dry-run test: given a device_address, check if playbook verify_graph passes currently.
- Wired in `src/http_server/mod.rs`.

**T5 — Playbook execution tracking** ✅ batch5
- When `src/http_server/remediation.rs` executes a playbook step (already wired from D4-3/D4-15):
  - After execution, PATCH `/api/playbooks/{id}` to increment `execution_count`.
  - On outcome=success: increment `success_count`.
  - Write `PlaybookExecution(id, playbook_id, detection_id, device_address, outcome, started_at_ns, completed_at_ns, operator_id, failure_step, failure_reason)` node.
  - `HAS_PLAYBOOK_EXECUTION(Playbook → PlaybookExecution)` rel.
- `GET /api/playbooks/{id}/executions` — list executions with outcomes.
- `GET /api/playbooks/stats` — aggregate: success_rate, avg_duration_ms, most_used, least_used.

**T6 — Syslog pattern rule creation from UI** ✅ batch5
- Create `POST /api/syslog-rules` — create a new syslog detection pattern entirely from UI:
  - Body: `{name, vendor, pattern_regex, fact_type, severity, description, enabled, example_message}`.
  - Writes to `ConfigItem(config_class="syslog_pattern", ...)` in DB.
  - Hot-loads into Rust syslog engine at next pattern refresh cycle (already exists via D4-7 `load_config_yaml_by_class()`).
- `PUT /api/syslog-rules/{id}` — edit pattern.
- `POST /api/syslog-rules/{id}/test` — test pattern against an example syslog message. Returns `{matched: bool, extracted_facts: {}}`.
- BonPy UI `src/routes/rules/+page.svelte`:
  - Rule list: Python rules + syslog pattern rules in unified view.
  - Python rules: enable/disable toggle, parameter sliders, shadow mode toggle, firing rate chart.
  - Syslog pattern rules: edit pattern, test modal with example message, vendor selector.
  - "New syslog rule" modal form.

**T7 — Rule firing analytics** ✅ batch5
- `RuleFiringStats(rule_id, window_hours, fire_count, false_positive_count, true_positive_count, shadow_fire_count, avg_confidence, last_fired_at_ns)` — computed from `DetectionEvent` + operator feedback records.
- `GET /api/sidecar/rules/analytics?window_hours=168` — returns stats for all rules over the given window.
- Used by BonPy UI rule list page to show firing rates, FPR, trend charts.
- Also feeds an alert: if any rule's `shadow_fire_count / window_hours > 10` (fires >10 times/hour in shadow mode but 0 in production), it flags the rule as potentially useful but disabled.

**T8 — Playbook management page (/bonpy/rules)** ✅ batch5
- `src/routes/rules/+page.svelte`:
  - Two tabs: "Detection Rules" and "Playbooks".
  - **Detection Rules tab**: unified list of Python rules + syslog pattern rules. Columns: rule_id, type (Python/syslog), enabled, shadow_mode, fire_count_24h, FPR_estimate. Actions: toggle enabled, toggle shadow, edit params (Python), edit pattern (syslog), view analytics.
  - **Playbooks tab**: playbook list. Columns: name, rule_ids, vendor, enabled, execution_count, success_rate. Actions: enable/disable, view executions, edit (opens YAML-like editor), test, clone (copy as new).
  - "New Syslog Rule" button → modal with pattern editor, live test against example message.
  - "New Playbook" button → structured form (name, rule_ids multi-select, vendor, steps YAML editor, verify_graph Cypher field).

---

---

## EV1-8 — Structural Uncertainty: NCT, Control-Weighted GNN, Conformal Prediction

### Analysis

This epic covers the theoretical depth of the GNN pipeline. Three distinct concepts are addressed:

---

**1. NCT (Noise-Contrastive Training / Noise-Contrastive Estimation)**

NCT is a self-supervised pre-training method where the model learns to distinguish real graph structure from corrupted (noise-injected) graph structure. In the context of Bonsai:
- **Positive samples**: actual graph snapshots from the live network.
- **Negative samples**: corrupted snapshots — randomly remove edges, add spurious edges, or permute node features.
- The model learns: "what does a real network topology look like?" before ever seeing a fault label.
- This is the most effective way to bootstrap a GNN with limited labelled fault examples (the label sparsity problem that Bonsai faces: <5% of snapshots are faults, many fault labels come from synthetic chaos injection, not real incidents).

NCT differs from simple contrastive learning: the noise distribution is parameterised so you can control what the model learns to be invariant to. For Bonsai: we want the model to be sensitive to topology changes (edge removal = link failure = anomaly) but invariant to vendor-specific node feature scaling (Nokia vs Cisco nodes have different raw feature value ranges).

Implementation: NCT pre-training added in EV1-1 T7 (`python/bonsai_ml/gnn/nct.py`). This epic deepens the NCT implementation with proper noise curriculum and edge-case handling.

---

**2. Control-Weighted GNN (CW-GNN)**

During operator-declared maintenance windows (change requests — already in graph via `ChangeRequest` nodes from the change management integration), network events are expected and not anomalous. The GNN should not flag a device as anomalous just because a BGP session was cleared during a planned maintenance.

CW-GNN addresses this by modifying the training loss: fault labels during active `ChangeRequest` windows are weighted down (toward 0 — "expected fault") rather than treated as hard positives. This prevents the model from penalising itself for "missing" faults that were operator-intended.

This is distinct from the `change_correlated` flag already in `detection.py` (which suppresses rule-based detections during change windows). CW-GNN applies the same logic at the **training signal** level for the graph neural network.

---

**3. Structural Uncertainty (Conformal Prediction)**

The current GNN outputs a single scalar anomaly score. This is a point estimate with no confidence interval. A score of 0.72 could mean:
- "This device is clearly anomalous" (low uncertainty, trustworthy score).
- "The model hasn't seen this topology configuration before, it's guessing" (high uncertainty, score should be treated with skepticism).

For a NOC, this distinction is critical. Auto-triggering an investigation on a 0.72-score device with high uncertainty wastes an analyst's time. Auto-triggering on 0.72 with low uncertainty is valuable.

**Conformal prediction** (Angelopoulos & Bates, 2021 — "Gentle Introduction to Conformal Prediction") provides distribution-free coverage guarantees: "With probability 1-α, the true label is in the prediction set." For anomaly detection, this means: "With 90% probability, this device's true anomaly state is within this range." This is valid without Gaussian assumptions and works with any trained model.

Implementation: post-hoc conformal calibration layer applied after the STGNN. No retraining needed. Requires a held-out calibration set (which Bonsai has via the chaos archive).

---

**Relationship between the three concepts:**
```
NCT pre-training  →  Better base representations  →  Lower calibration error for conformal
CW-GNN           →  Correct training signal        →  Lower variance on maintenance-window samples
Conformal         →  Uncertainty quantification     →  Trustworthy anomaly score ranges
```

All three are additive. NCT and CW-GNN improve model quality. Conformal prediction quantifies remaining uncertainty.

### Tasks

**T1 — NCT noise curriculum** ✅ batch2
- Extend `python/bonsai_ml/gnn/nct.py` (scaffolded in EV1-1 T7):
  - `NoiseSchedule`: defines how noise intensity increases over training epochs (warm-up curriculum).
  - Epoch 1-10: light noise (randomly remove 5% of edges).
  - Epoch 11-30: medium noise (remove 15% edges + perturb 10% of node features by ±0.2).
  - Epoch 31+: heavy noise (remove 30% edges + permute 20% node features + add 5% spurious edges).
  - Curriculum rationale: starting heavy makes the model resist structure entirely; curriculum forces it to first learn coarse structure, then fine-grained features.
  - `NodeFeatureInvariance`: a subset of features (vendor OHE, role OHE) should be invariant to perturbation — these are structural, not operational. Only operational features (cpu_util_pct, interface error rates) are perturbed.
  - Configurable: `nct_noise_levels` list in `BonsaiGnnConfig`.

**T2 — NCT edge-case: disconnected subgraphs** ✅ batch6
- Some Bonsai topologies may have isolated devices (device onboarded but no LLDP/BGP links yet). During NCT, these should not be used as positive pair samples (no positive pair can be constructed from an isolated node).
- `NodePairSampler` update: filter out isolated nodes (degree=0) from positive pair sampling. They can still be used as negative samples (anything is a valid negative for an isolated node).
- Isolated nodes during NCT: apply mean-field approximation — embed using only node features, no message passing.

**T3 — Control-Weighted GNN loss function** ✅ batch7
- Create `python/bonsai_ml/gnn/loss.py` (file already exists in `bonsai_ml/gnn/loss.py` — extend it):
  - Current: standard cross-entropy loss.
  - Add `ControlWeightedLoss(nn.Module)`:
    - Takes per-sample `change_weights: Tensor` (float in [0.0, 1.0]).
    - Weight = 0.0: this snapshot has an active change request → fault label weight = 0.0 (no gradient from this sample for the fault class).
    - Weight = 1.0: no change request active → normal gradient.
    - Weight = 0.1–0.5: partial credit (e.g., change window overlaps but fault is on an unrelated device).
    - `forward(logits, labels, change_weights) = CrossEntropy(logits, labels) * change_weights`.
  - `FocalControlWeightedLoss`: adds focal loss gamma parameter for class imbalance on top of control weighting. Focal loss down-weights easy negatives (correctly-predicted clean states), focusing gradient on hard positives (actual faults). `FL(pt) = -(1-pt)^gamma * log(pt)`. Recommended gamma=2.0 for Bonsai class imbalance.

**T4 — Change weight computation during training** ✅ batch7
- In `archive_to_training.py`, extend labelled dataset construction:
  - For each snapshot in the training set: query `ChangeRequest` nodes active during `snapshot_ns ± 30min` for any device in the snapshot.
  - If active change found on the SAME device as the fault label: assign `change_weight = 0.0`.
  - If active change found on a DIFFERENT device in the same snapshot: assign `change_weight = 0.5`.
  - No change request: `change_weight = 1.0`.
  - `BonsaiGraphData` dataclass: add `change_weight: float = 1.0` field.
  - `BonsaiGnnDataLoader.from_snapshot()` populates this from the chaos archive.

**T5 — Conformal prediction calibration layer** ✅ batch7
- Create `python/bonsai_ml/gnn/conformal.py`:
  - `ConformalCalibrator`:
    - `calibrate(model, calibration_loader, alpha=0.1)`: Runs model on calibration set, collects nonconformity scores `s_i = 1 - softmax(logit_fault)[i]` for all positive (fault) samples.
    - Sets threshold `q_hat = (1-alpha)-quantile of {s_i}` (standard conformal threshold).
    - Saves `q_hat` to `models/conformal_qhat_alpha0.1.json`.
  - `ConformalPredictor`:
    - `predict_set(logits: Tensor, q_hat: float) -> list[bool]`: Returns True (included in prediction set) if nonconformity score ≤ q_hat.
    - `predict_score_with_uncertainty(logits: Tensor, q_hat: float) -> tuple[float, float]`: Returns `(point_estimate, uncertainty_margin)`. `uncertainty_margin = |0.5 - softmax(logit_fault)|` — higher is less uncertain.
  - `ConformalMetrics`:
    - `coverage(prediction_sets, true_labels)` — fraction of true faults captured. Target: ≥ 1-alpha = 90%.
    - `efficiency(prediction_sets)` — fraction of samples with singleton prediction set (certain). Higher = better.
  - Calibration run triggered automatically after every training run (see EV1-5 T5).

**T6 — Uncertainty-gated investigation triggering** ✅ batch7
- Extend `src/investigation_trigger.rs`:
  - After GNN inference result write-back (EV1-4 T2), read `uncertainty_margin` from `GnnInferenceResult`.
  - Trigger investigation ONLY if: `anomaly_score > threshold AND uncertainty_margin < uncertainty_gate` (default `uncertainty_gate = 0.3`).
  - High-score + high-uncertainty: emit `GnnUncertainHighAlert` ML event (for BonPy UI display) but do NOT auto-trigger investigation.
  - High-score + low-uncertainty: auto-trigger investigation as normal.
  - Both values exposed as fields on `GnnInferenceResult` node.
  - Configurable: `gnn_uncertainty_gate` in `GnnConfig` (already exists in `src/config.rs`).

**T7 — MC Dropout uncertainty estimate (alternative to conformal)** ✅ batch7
- For deployments where a held-out calibration set is unavailable (small lab environments with <100 fault examples), implement **MC Dropout** uncertainty estimate as a fallback:
  - `MCDropoutEstimator`:
    - Run `model.forward(snapshot)` N=20 times with `model.train()` mode (dropout active).
    - Collect N anomaly scores per device.
    - `mean_score = mean(scores)`, `uncertainty = std(scores)`.
  - When conformal `q_hat` not available (file not found or calibration set too small), use MC Dropout.
  - MC Dropout is slower (N×inference time) but needs no calibration set.
  - Gate: `mc_dropout_samples = 0` disables this and falls back to single-pass inference.
  - Add `mc_dropout_samples: int = 0` to `BonsaiGnnConfig`.

**T8 — Architecture decision record** ✅ batch7
- Create `docs/architecture/adr_gnn_uncertainty_ev1.md`:
  - **NCT**: Chosen pre-training. Addresses label sparsity. Noise curriculum described.
  - **CW-GNN**: Chosen loss modifier. Addresses maintenance-window false positives.
  - **Conformal prediction**: Chosen uncertainty method. Distribution-free, post-hoc, no retraining. Requires calibration set (30+ fault examples).
  - **MC Dropout**: Fallback for cold-start. Higher inference cost. No calibration set needed.
  - **Bayesian GNN (Deep Ensembles)**: Evaluated. Rejected — requires 5× training runs, prohibitive on single-box deployment.
  - **Laplace approximation**: Evaluated. Rejected — requires Hessian approximation, not compatible with heterogeneous GNN.
  - **Coverage targets**: α=0.1 (90% coverage) in production. α=0.05 (95% coverage) for high-stakes auto-remediation paths.

---

---

## EV1-9 — Continuous Production Run: Sidecar Hardening + Watchdog

### Analysis

Today `collector_engine.py` is started manually:
```bash
python python/collector_engine.py
```

If it crashes, it stays down until someone notices and restarts it. There is no watchdog, no auto-restart, no crash dump, no memory limit enforcement, no graceful shutdown on SIGTERM. The systemd unit file `deploy/systemd/bonsai-rules-sidecar.service` exists but has no restart policy hardening.

**In production, the continuous run model must cover:**

1. **The ML job engine** (`BonsaiJobEngine` from EV1-5) must run continuously as a background process. It must survive Bonsai core restarts (it reconnects), Python exceptions (caught + logged), and machine reboots (systemd restart).

2. **The STGNN inference loop** must run every 5 minutes, fetching a live graph snapshot, running inference, writing results back. If it fails (model file missing, API timeout), it logs the error, emits a `JobFailed` ML event, and retries on the next cycle. It does not crash the parent process.

3. **The embedding workers** (syslog + config) must run on their schedules, handle API failures gracefully, and never block the rule evaluation event loop.

4. **Sidecar registration re-try loop**: The existing `_heartbeat_loop()` in `collector_engine.py` handles `reregister_required` from Bonsai. But if Bonsai is down at startup, the heartbeat thread never starts because the initial connection in `main()` blocks indefinitely. This is a deadlock. Need a non-blocking startup connection with retry.

5. **Memory management**: Python process grows over time. `WindowRegistry` in `window.py` has `max_entries=4096` (added in F5). But `EventEmbedding` cache in the embedding worker, STGNN snapshot buffer (8 snapshots × N devices × 40 dims), and ML model loaded in memory all need explicit bounds. Without bounds, the sidecar will be killed by the OS on constrained hardware.

6. **Graceful shutdown**: On SIGTERM, the sidecar should: stop accepting new events, flush the forward queue to core, save the STGNN snapshot buffer to disk, write a checkpoint for the job engine, then exit. Current `collector_engine.py` exits immediately on SIGTERM (no signal handler).

7. **Process supervision model**: The main process should be one Python process that runs:
   - `RuleEngine` (existing — event loop + poll loop threads)
   - `BonsaiJobEngine` (EV1-5 — APScheduler in background thread)
   - `SidecarHeartbeat` (existing)
   - `HealthHTTP` (existing on :9200)
   - `PrometheusMetrics` (EV1-5 T8 — on :9201)
   
   Not multiple separate processes. Simpler to supervise, simpler to share in-memory state (snapshot buffer, model, embedding cache).

### Tasks

**T1 — Non-blocking startup with reconnect loop** ✅ batch4
- Rewrite `collector_engine.py` `main()`:
  - Remove the `while True: try: with BonsaiClient(local_addr) as local_client:` blocking pattern.
  - Replace with: start all background threads immediately (health HTTP, core forwarder, heartbeat, job engine), then connect to local collector with retry in a dedicated thread.
  - Connection retry: `BonsaiConnector(local_addr, on_connected_callback)` class. Attempts connection every 5s with exponential back-off to max 60s. Calls `on_connected_callback(client)` when connected.
  - `on_connected_callback`: starts `RuleEngine`, registers sidecar, starts heartbeat.
  - On disconnection: gracefully stops `RuleEngine`, keeps all other threads running, begins reconnect cycle.
  - Result: health HTTP is always reachable even when disconnected from local collector. Job engine keeps running (scheduled jobs continue, just can't forward detections).

**T2 — Graceful shutdown handler** ✅ batch4
- Add `signal.signal(signal.SIGTERM, _handle_sigterm)` in `main()`:
  - `_handle_sigterm`:
    1. Log "received SIGTERM, shutting down gracefully".
    2. Set `_stop_event` (threading.Event) that all loops check.
    3. Wait max 10s for `forward_queue` to drain.
    4. Save STGNN snapshot buffer to disk (`SnapshotStore.flush()` — see EV1-2 T6).
    5. Save job engine checkpoint (`BonsaiJobEngine.checkpoint()`).
    6. Cancel all running APScheduler jobs.
    7. Log final metrics summary.
    8. `sys.exit(0)`.
  - Also handle `SIGINT` (Ctrl+C) the same way in dev.
  - Shutdown timeout: if any step takes >10s, force exit anyway (no hung shutdown).

**T3 — STGNN continuous inference loop** ✅ batch4
- Create `python/bonsai_ml/inference_loop.py`:
  - `StgnnInferenceLoop(engine: BonsaiJobEngine)`:
    - Registers `gnn_inference` job with interval trigger (default 5 min, configurable).
    - Job function `_run_inference()`:
      1. Load active `STGNNModel` from `ModelArtifact` (lazy-cached: reload only if model file mtime changes).
      2. Fetch latest graph snapshot via `GraphSnapshotClient.fetch_snapshot()`.
      3. Convert to `HeteroData` via `build_hetero_data()`.
      4. Append to `SnapshotBuffer`.
      5. If buffer has ≥2 snapshots: run `model.forward(buffer.get_sequence())`.
      6. Extract per-device scores + attention weights.
      7. Apply conformal prediction (if `q_hat` available).
      8. POST inference results to `/api/gnn/inference-results`.
      9. POST attention snapshots to `/api/gnn/attention`.
      10. Emit `GnnInferenceCompleted` ML event.
    - Error handling: any exception in steps 1-10 is caught, logged, `JobFailed` event emitted. Does not crash the process.
    - Model cold-start: if no active model found, logs warning and skips inference (no crash).
  - `start(job_engine: BonsaiJobEngine)`: registers the inference job.

**T4 — systemd service hardening** ✅ batch4
- Update `deploy/systemd/bonsai-rules-sidecar.service`:
  ```ini
  [Unit]
  Description=Bonsai Rules + ML Sidecar
  After=bonsai.service network.target
  PartOf=bonsai.service

  [Service]
  Type=simple
  User=bonsai
  WorkingDirectory=/opt/bonsai
  ExecStart=/opt/bonsai/venv/bin/python python/collector_engine.py
  Restart=on-failure
  RestartSec=10
  RestartBursts=5
  StartLimitInterval=120
  StartLimitBurst=5
  TimeoutStopSec=30
  KillMode=control-group
  KillSignal=SIGTERM
  MemoryMax=2G
  MemorySwapMax=0
  CPUQuota=150%
  Environment=BONSAI_CORE_ADDR=localhost:50051
  Environment=BONSAI_LOCAL_ADDR=localhost:50052
  StandardOutput=journal
  StandardError=journal
  SyslogIdentifier=bonsai-sidecar
  ```
- `PartOf=bonsai.service`: sidecar stops when Bonsai core stops (prevents orphan sidecar after core upgrade).
- `MemoryMax=2G`: prevents runaway growth from embedding cache or model load. OOM kills sidecar, systemd restarts it.
- `RestartBursts=5` + `StartLimitInterval=120`: allows 5 rapid restarts but backs off if crash loop detected.

**T5 — ML memory bounds** ✅ batch4
- `python/bonsai_ml/memory_manager.py`:
  - `MlMemoryManager`:
    - Tracks per-component memory usage estimates:
      - `ModelCache`: tracks loaded models. LRU eviction when total model memory > `max_model_memory_mb` (default 1024MB).
      - `EmbeddingCache`: in-memory cache of recently computed embeddings. LRU, max 10,000 entries.
      - `SnapshotBuffer`: bounded to T=8 snapshots. Memory estimate: N_devices × 40 dims × 4 bytes × 8 = ~50KB for 100 devices.
    - `get_memory_report() -> dict`: Returns per-component usage for health endpoint.
    - Periodic check (every 5 min): if total process RSS > `max_total_memory_mb`, evict caches.
  - Integration with health endpoint: `GET /health` response includes `memory_usage_mb` and `memory_components` dict.
  - Prometheus gauge: `bonsai_sidecar_memory_mb` — scraped by Prometheus (see EV1-5 T8).

**T6 — Forward queue backpressure + overflow handling** ✅ batch4
- Extend `collector_engine.py` forward queue handling:
  - Current: `forward_queue = queue.Queue(maxsize=1000)`. If full: silently drops detection with warning log.
  - Extend: when queue > 80% full, log warning + emit `MlEvent(type=queue_pressure)`.
  - When queue > 95% full: switch to priority-only mode — only `severity=critical` and `severity=high` detections are queued. `warn` and `info` are dropped with a counter.
  - Queue overflow counter: `forward_queue_drops_total` — Prometheus counter.
  - Queue depth gauge: `forward_queue_depth` — Prometheus gauge.
  - When core forwarder reconnects after disconnect: flush queue first, then emit a "catch-up batch" summary event.

**T7 — Sidecar health enrichment** ✅ batch4
- Extend `/health` response (currently in `_HealthHandler`):
  ```json
  {
    "status": "ok",
    "uptime_secs": 3600,
    "rules_loaded": 47,
    "rules_enabled": 42,
    "rules_shadow": 3,
    "last_detection_at_ns": 1748190000000000000,
    "detections_today": 12,
    "queue_depth": 3,
    "queue_drops_today": 0,
    "model_loaded": true,
    "model_id": "stgnn_v1_20260525",
    "last_inference_at_ns": 1748189700000000000,
    "snapshot_buffer_size": 7,
    "snapshot_buffer_stale": false,
    "embedding_pending_syslog": 142,
    "embedding_pending_config": 8,
    "job_engine_running": true,
    "next_job": {"id": "anomaly_export_daily", "in_seconds": 3247},
    "memory_usage_mb": 412,
    "connected_to_core": true,
    "connected_to_local": true
  }
  ```
- All fields consumed by BonPy UI dashboard page (EV1-6 T2).

**T8 — Deployment documentation** ✅ batch4
- Create `docs/BONPY_PRODUCTION_DEPLOY.md`:
  - Prerequisites: Python 3.12+, virtualenv, sentence-transformers, torch, torch-geometric.
  - Installation steps: venv setup, pip install, service install.
  - Configuration: environment variables, API key setup, embedding model selection.
  - Startup sequence: Bonsai core must be running before sidecar starts (enforced by `After=bonsai.service` in systemd).
  - First run: cold-start behaviour — no model (rules-only mode), snapshot buffer fills over time, first training job runs after export readiness.
  - Monitoring: health endpoint, Prometheus metrics, BonPy UI URL.
  - Troubleshooting: common errors (model file missing, API key invalid, memory OOM, queue backpressure).
  - Upgrade procedure: SIGTERM → wait for graceful shutdown → deploy new version → systemctl start.

---
