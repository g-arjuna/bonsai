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

