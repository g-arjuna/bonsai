"""Train STGNN — Spatio-Temporal GNN for Bonsai anomaly detection.

EV1-1 T8: Training pipeline for STGNNModel (GATv2-GRU, T=8 snapshots).

Usage:
    # From SnapshotStore (recommended):
    python train_stgnn.py --snapshot-dir runtime/parquet/gnn_snapshots

    # From Parquet export (legacy path):
    python train_stgnn.py --parquet data/anomaly_export.parquet

    # Phase 1 only (NCT pre-training):
    python train_stgnn.py --snapshot-dir runtime/parquet/gnn_snapshots --phase nct

    # Phase 2 only (supervised, skip pretrain):
    python train_stgnn.py --snapshot-dir runtime/parquet/gnn_snapshots --phase supervised

    # Full pipeline with API registration:
    python train_stgnn.py --snapshot-dir runtime/parquet/gnn_snapshots \\
        --api-url http://localhost:3000 --register

Requires: torch, torch-geometric, pyarrow, scikit-learn
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import time
from dataclasses import dataclass, asdict
from pathlib import Path
from typing import Optional

sys.path.insert(0, str(Path(__file__).parent))

import logging
log = logging.getLogger(__name__)
logging.basicConfig(level=logging.INFO, format="%(asctime)s [train_stgnn] %(message)s")

MIN_SNAPSHOTS_FOR_NCT = 30
MIN_SNAPSHOTS_FOR_SUPERVISED = 2
DEFAULT_NCT_EPOCHS = 50
DEFAULT_TRAIN_EPOCHS = 100
DEFAULT_LR = 1e-3
DEFAULT_THRESHOLD = 0.5
MODELS_DIR = "models"


@dataclass
class TrainingResult:
    model_path: str
    val_auc: float
    val_f1: float
    val_precision: float
    val_recall: float
    threshold: float
    num_training_snapshots: int
    nct_pretrained: bool
    nct_final_loss: float
    schema_hash: str
    quality_passed: bool
    duration_s: float


def _load_snapshots_from_store(snapshot_dir: str) -> list:
    """Load HeteroData snapshots from SnapshotStore."""
    from bonsai_ml.gnn.snapshot_store import SnapshotStore
    store = SnapshotStore(store_dir=snapshot_dir, max_buffer_size=512)
    snapshots = store.load_buffer()
    log.info("Loaded %d snapshots from %s", len(snapshots), snapshot_dir)
    return snapshots


def _load_snapshots_from_parquet(parquet_path: str) -> list:
    """Convert Parquet export rows to HeteroData snapshots (legacy path)."""
    try:
        import pyarrow.parquet as pq
        from bonsai_ml.gnn.data_loader import BonsaiGnnDataLoader
    except ImportError as exc:
        log.error("Missing dependency: %s", exc)
        sys.exit(1)

    table = pq.read_table(parquet_path).to_pandas()
    log.info("Loaded %d rows from %s", len(table), parquet_path)

    try:
        from bonsai_ml.gnn.model import build_hetero_data
        snapshot_groups = table.groupby("snapshot_ns")
        snapshots = []
        for ts_ns, group in snapshot_groups:
            snap_dict = {
                "snapshot_ns": ts_ns,
                "devices": group.to_dict("records"),
                "links": [],
                "chaos_log": [],
            }
            try:
                hetero = build_hetero_data(snap_dict)
                snapshots.append(hetero)
            except Exception as exc:
                log.debug("Skipping snapshot at %s: %s", ts_ns, exc)
        log.info("Converted %d snapshots from Parquet", len(snapshots))
        return snapshots
    except ImportError as exc:
        log.error("Could not build HeteroData: %s", exc)
        sys.exit(1)


def run_nct_pretrain(model, snapshots: list, epochs: int, lr: float) -> float:
    """Phase 1: NCT pre-training. Returns final NCT loss."""
    if len(snapshots) < MIN_SNAPSHOTS_FOR_NCT:
        log.warning(
            "Only %d snapshots (need %d for NCT) — skipping NCT, using random init",
            len(snapshots), MIN_SNAPSHOTS_FOR_NCT,
        )
        return 0.0

    from bonsai_ml.gnn.nct import pretrain_nct

    pretrain_dir = Path(MODELS_DIR)
    pretrain_dir.mkdir(parents=True, exist_ok=True)
    checkpoint_path = str(pretrain_dir / "nct_pretrain.pt")

    log.info("Phase 1: NCT pre-training (%d epochs, %d snapshots)", epochs, len(snapshots))
    final_loss = pretrain_nct(
        model=model,
        snapshots=snapshots,
        epochs=epochs,
        lr=lr,
        checkpoint_path=checkpoint_path,
    )
    log.info("NCT pre-training complete. Final loss: %.4f", final_loss)
    log.info("Checkpoint saved to %s", checkpoint_path)
    return final_loss


def run_supervised(model, snapshots: list, epochs: int, lr: float, threshold: float) -> dict:
    """Phase 2: supervised fine-tuning. Returns val metrics."""
    try:
        import torch
        import torch.nn as nn
        from torch.optim import Adam
        from torch.optim.lr_scheduler import CosineAnnealingLR
        from sklearn.metrics import roc_auc_score, f1_score, precision_score, recall_score
        from sklearn.model_selection import train_test_split
    except ImportError as exc:
        log.error("Missing torch/sklearn dependency: %s", exc)
        sys.exit(1)

    labeled_snapshots = [s for s in snapshots if hasattr(s["device"], "y") and s["device"].y.sum() >= 0]
    labeled_snapshots = snapshots

    if len(labeled_snapshots) < MIN_SNAPSHOTS_FOR_SUPERVISED:
        log.error("Not enough snapshots for supervised training")
        sys.exit(1)

    n_val = max(1, int(len(labeled_snapshots) * 0.2))
    train_snaps = labeled_snapshots[:-n_val]
    val_snaps = labeled_snapshots[-n_val:]

    opt = Adam(model.parameters(), lr=lr, weight_decay=1e-4)
    scheduler = CosineAnnealingLR(opt, T_max=epochs, eta_min=lr * 0.01)
    loss_fn = nn.CrossEntropyLoss(weight=torch.tensor([1.0, 5.0]))

    log.info("Phase 2: supervised fine-tuning (%d epochs, %d train snaps, %d val snaps)",
             epochs, len(train_snaps), len(val_snaps))

    model.train()
    for epoch in range(epochs):
        epoch_loss = 0.0
        for snap in train_snaps:
            try:
                x_dict = {k: snap[k].x for k in snap.node_types if snap[k].x is not None}
                edge_dict = {e: snap[e].edge_index for e in snap.edge_types}
                out = model(x_dict, edge_dict)
                if "device" not in out or not hasattr(snap["device"], "y"):
                    continue
                logits = out["device"]
                labels = snap["device"].y
                if labels.shape[0] != logits.shape[0]:
                    continue
                loss = loss_fn(logits, labels)
                opt.zero_grad()
                loss.backward()
                torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0)
                opt.step()
                epoch_loss += loss.item()
            except Exception as exc:
                log.debug("Skipping snapshot in train: %s", exc)
        scheduler.step()
        if (epoch + 1) % 10 == 0:
            log.info("  epoch %d/%d loss=%.4f", epoch + 1, epochs, epoch_loss)

    model.eval()
    all_probs, all_labels = [], []
    with torch.no_grad():
        for snap in val_snaps:
            try:
                x_dict = {k: snap[k].x for k in snap.node_types if snap[k].x is not None}
                edge_dict = {e: snap[e].edge_index for e in snap.edge_types}
                out = model(x_dict, edge_dict)
                if "device" not in out or not hasattr(snap["device"], "y"):
                    continue
                probs = torch.softmax(out["device"], dim=-1)[:, 1].cpu().numpy()
                labels = snap["device"].y.cpu().numpy()
                all_probs.extend(probs.tolist())
                all_labels.extend(labels.tolist())
            except Exception as exc:
                log.debug("Skipping snapshot in val: %s", exc)

    if not all_labels or sum(all_labels) == 0:
        log.warning("No anomaly labels in validation set — returning dummy metrics")
        return {"val_auc": 0.5, "val_f1": 0.0, "val_precision": 0.0, "val_recall": 0.0}

    import numpy as np
    probs_arr = np.array(all_probs)
    labels_arr = np.array(all_labels)
    preds = (probs_arr >= threshold).astype(int)

    try:
        auc = roc_auc_score(labels_arr, probs_arr)
    except Exception:
        auc = 0.5
    f1   = f1_score(labels_arr, preds, zero_division=0)
    prec = precision_score(labels_arr, preds, zero_division=0)
    rec  = recall_score(labels_arr, preds, zero_division=0)

    log.info("Validation: AUC=%.3f F1=%.3f Precision=%.3f Recall=%.3f", auc, f1, prec, rec)
    return {"val_auc": auc, "val_f1": f1, "val_precision": prec, "val_recall": rec}


def save_model(model, output_path: str) -> None:
    import torch
    Path(output_path).parent.mkdir(parents=True, exist_ok=True)
    torch.save(model.state_dict(), output_path)
    log.info("Model saved to %s", output_path)


def register_with_api(api_url: str, result: TrainingResult, model_type: str = "stgnn") -> str:
    """POST the trained model to /api/ml/models. Returns model ID."""
    try:
        import requests
        payload = {
            "model_type": model_type,
            "version": f"stgnn_v{int(time.time())}",
            "val_auc": result.val_auc,
            "val_f1": result.val_f1,
            "val_precision": result.val_precision,
            "val_recall": result.val_recall,
            "threshold": result.threshold,
            "trained_at_ns": time.time_ns(),
            "num_training_snapshots": result.num_training_snapshots,
            "nct_pretrained": result.nct_pretrained,
            "schema_hash": result.schema_hash,
            "model_path": result.model_path,
        }
        resp = requests.post(f"{api_url}/api/ml/models", json=payload, timeout=10)
        if resp.ok:
            model_id = resp.json().get("id", "unknown")
            log.info("Registered model as id=%s", model_id)
            return model_id
    except Exception as exc:
        log.warning("Could not register model with API: %s", exc)
    return ""


def emit_job_event(api_url: str, result: TrainingResult, model_id: str) -> None:
    """Emit MlJobEvent on training completion."""
    try:
        import requests
        requests.post(
            f"{api_url}/api/ml/events/publish",
            json={
                "event_type": "stgnn_training_complete",
                "payload": {
                    "model_id": model_id,
                    "val_auc": result.val_auc,
                    "val_f1": result.val_f1,
                    "num_training_snapshots": result.num_training_snapshots,
                    "quality_passed": result.quality_passed,
                    "duration_s": result.duration_s,
                },
            },
            timeout=5,
        )
    except Exception as exc:
        log.debug("Could not emit job event: %s", exc)


def main() -> None:
    ap = argparse.ArgumentParser(description="Train STGNN anomaly detector")
    ap.add_argument("--snapshot-dir", default=None, help="Path to SnapshotStore directory")
    ap.add_argument("--parquet", default=None, help="Path to Parquet export (legacy)")
    ap.add_argument("--output", default=None, help="Output model path (default: models/stgnn_v<ts>.pt)")
    ap.add_argument("--phase", choices=["nct", "supervised", "both"], default="both")
    ap.add_argument("--nct-epochs", type=int, default=DEFAULT_NCT_EPOCHS)
    ap.add_argument("--train-epochs", type=int, default=DEFAULT_TRAIN_EPOCHS)
    ap.add_argument("--lr", type=float, default=DEFAULT_LR)
    ap.add_argument("--threshold", type=float, default=DEFAULT_THRESHOLD)
    ap.add_argument("--api-url", default=None, help="Bonsai API URL for model registration")
    ap.add_argument("--register", action="store_true", help="Register model via API after training")
    args = ap.parse_args()

    if not args.snapshot_dir and not args.parquet:
        ap.error("Provide --snapshot-dir or --parquet")

    try:
        import torch
        from bonsai_ml.gnn.stgnn import build_stgnn
        from bonsai_ml.feature_schema import DEVICE_V2_SCHEMA
    except ImportError as exc:
        log.error("Missing dependency: %s — install torch + torch-geometric", exc)
        sys.exit(1)

    t_start = time.time()

    if args.snapshot_dir:
        snapshots = _load_snapshots_from_store(args.snapshot_dir)
    else:
        snapshots = _load_snapshots_from_parquet(args.parquet)

    if len(snapshots) < MIN_SNAPSHOTS_FOR_SUPERVISED:
        log.error("Need at least %d snapshots, got %d", MIN_SNAPSHOTS_FOR_SUPERVISED, len(snapshots))
        sys.exit(1)

    model = build_stgnn()
    schema_hash = DEVICE_V2_SCHEMA.schema_hash

    nct_loss = 0.0
    nct_pretrained = False

    if args.phase in ("nct", "both"):
        nct_loss = run_nct_pretrain(model, snapshots, args.nct_epochs, args.lr)
        nct_pretrained = nct_loss > 0.0

    val_metrics = {"val_auc": 0.5, "val_f1": 0.0, "val_precision": 0.0, "val_recall": 0.0}
    if args.phase in ("supervised", "both"):
        val_metrics = run_supervised(model, snapshots, args.train_epochs, args.lr, args.threshold)

    ts = int(time.time())
    output_path = args.output or f"{MODELS_DIR}/stgnn_v{ts}.pt"
    save_model(model, output_path)

    duration_s = time.time() - t_start
    quality_passed = val_metrics["val_auc"] >= 0.65 and val_metrics["val_f1"] >= 0.4

    result = TrainingResult(
        model_path=output_path,
        val_auc=val_metrics["val_auc"],
        val_f1=val_metrics["val_f1"],
        val_precision=val_metrics["val_precision"],
        val_recall=val_metrics["val_recall"],
        threshold=args.threshold,
        num_training_snapshots=len(snapshots),
        nct_pretrained=nct_pretrained,
        nct_final_loss=nct_loss,
        schema_hash=schema_hash,
        quality_passed=quality_passed,
        duration_s=duration_s,
    )

    log.info("Training complete in %.1fs. Quality: %s", duration_s, "PASS" if quality_passed else "FAIL")
    log.info("Metrics: %s", json.dumps(val_metrics, indent=2))

    model_id = ""
    if (args.register or args.api_url) and args.api_url:
        model_id = register_with_api(args.api_url, result, "stgnn")
        emit_job_event(args.api_url, result, model_id)

    result_path = output_path.replace(".pt", "_result.json")
    with open(result_path, "w") as f:
        json.dump(asdict(result), f, indent=2)
    log.info("Training result saved to %s", result_path)

    if not quality_passed:
        log.warning("Quality gate FAILED (AUC=%.3f < 0.65 or F1=%.3f < 0.40)",
                    result.val_auc, result.val_f1)
        sys.exit(2)


if __name__ == "__main__":
    main()
