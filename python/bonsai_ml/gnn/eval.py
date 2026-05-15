"""Standardised evaluation harness for Bonsai anomaly detectors.

D5-T5 (DV1): Implements the TSAD evaluation framework (arxiv 2603.09675)
adopted in CV6 T4. Provides:
  - Confusion matrix, precision, recall, F1, AUC-ROC
  - Point-adjustment-aware F1 (PA-F1) for time-series anomaly detection
  - Comparison study runner: rules vs tabular ML vs GNN
  - Output schema for the model card

This is scaffolding — no training data yet. The harness is ready for the first
archive-depth-triggered evaluation run (expected DV2 or DV3).

Numpy is required. Scikit-learn is optional (needed for AUC-ROC).
"""
from __future__ import annotations

import math
from dataclasses import dataclass, asdict, field
from typing import Any


@dataclass
class ConfusionMatrix:
    tp: int = 0
    tn: int = 0
    fp: int = 0
    fn: int = 0

    @property
    def precision(self) -> float:
        denom = self.tp + self.fp
        return self.tp / denom if denom else 0.0

    @property
    def recall(self) -> float:
        denom = self.tp + self.fn
        return self.tp / denom if denom else 0.0

    @property
    def f1(self) -> float:
        p, r = self.precision, self.recall
        return 2 * p * r / (p + r) if (p + r) else 0.0

    @property
    def accuracy(self) -> float:
        total = self.tp + self.tn + self.fp + self.fn
        return (self.tp + self.tn) / total if total else 0.0


@dataclass
class GnnEvalReport:
    """Output schema for a GNN evaluation run — feeds the model card."""

    model_tag: str
    num_samples: int
    num_anomalies: int
    confusion: ConfusionMatrix
    f1: float
    pa_f1: float
    auc_roc: float
    threshold: float
    notes: str = ""
    feature_ablation: dict[str, float] = field(default_factory=dict)

    def as_dict(self) -> dict:
        d = asdict(self)
        d["confusion"] = asdict(self.confusion)
        return d

    def summary_line(self) -> str:
        return (
            f"{self.model_tag}: F1={self.f1:.3f} PA-F1={self.pa_f1:.3f} "
            f"AUC-ROC={self.auc_roc:.3f} "
            f"(n={self.num_samples}, anomalies={self.num_anomalies})"
        )


def compute_confusion(
    y_true: list[int],
    y_pred: list[int],
) -> ConfusionMatrix:
    """Compute a binary confusion matrix from integer label lists."""
    cm = ConfusionMatrix()
    for true, pred in zip(y_true, y_pred):
        if true == 1 and pred == 1:
            cm.tp += 1
        elif true == 0 and pred == 0:
            cm.tn += 1
        elif true == 0 and pred == 1:
            cm.fp += 1
        else:
            cm.fn += 1
    return cm


def point_adjusted_f1(
    y_true: list[int],
    y_score: list[float],
    threshold: float = 0.5,
) -> float:
    """Compute point-adjustment F1 (PA-F1) per arxiv 2603.09675.

    Point adjustment: if any point in a contiguous anomaly segment is detected,
    the entire segment is considered detected. This is the standard TSAD metric
    that accounts for detection latency within a fault window.

    Args:
        y_true: Ground-truth binary labels (1=anomalous).
        y_score: Model anomaly scores in [0, 1].
        threshold: Score threshold for classifying as anomalous.

    Returns:
        PA-F1 score.
    """
    if len(y_true) != len(y_score):
        raise ValueError("y_true and y_score must have equal length")
    if not y_true:
        return 0.0

    y_pred = [1 if s >= threshold else 0 for s in y_score]

    anomaly_segments: list[tuple[int, int]] = []
    in_segment = False
    start = 0
    for i, label in enumerate(y_true):
        if label == 1 and not in_segment:
            in_segment = True
            start = i
        elif label == 0 and in_segment:
            anomaly_segments.append((start, i - 1))
            in_segment = False
    if in_segment:
        anomaly_segments.append((start, len(y_true) - 1))

    y_pred_pa = list(y_pred)
    for seg_start, seg_end in anomaly_segments:
        if any(y_pred[j] == 1 for j in range(seg_start, seg_end + 1)):
            for j in range(seg_start, seg_end + 1):
                y_pred_pa[j] = 1

    cm = compute_confusion(y_true, y_pred_pa)
    return cm.f1


def compute_auc_roc(y_true: list[int], y_score: list[float]) -> float:
    """Compute AUC-ROC. Uses scikit-learn when available, falls back to a
    pure-python trapezoidal approximation otherwise."""
    try:
        from sklearn.metrics import roc_auc_score
        return float(roc_auc_score(y_true, y_score))
    except ImportError:
        pass
    except Exception:
        return float("nan")

    pairs = sorted(zip(y_score, y_true), key=lambda x: -x[0])
    tp = fp = 0
    total_pos = sum(y_true)
    total_neg = len(y_true) - total_pos
    if total_pos == 0 or total_neg == 0:
        return float("nan")

    auc = 0.0
    prev_fp = 0
    prev_tp = 0
    for _, label in pairs:
        if label == 1:
            tp += 1
        else:
            fp += 1
            auc += (tp + prev_tp) / 2.0
            prev_fp = fp
            prev_tp = tp
    return auc / (total_pos * total_neg)


def evaluate_gnn(
    model_tag: str,
    y_true: list[int],
    y_score: list[float],
    threshold: float = 0.5,
    notes: str = "",
    feature_ablation: dict[str, float] | None = None,
) -> GnnEvalReport:
    """Run the full TSAD evaluation suite for a GNN model.

    Args:
        model_tag: Short identifier for the model version (e.g. ``"hetero_gat_v1"``).
        y_true: Ground-truth binary labels (1=anomalous).
        y_score: Model anomaly scores in [0, 1].
        threshold: Classification threshold.
        notes: Free-form notes for the model card.
        feature_ablation: Optional dict mapping feature name → F1-delta when
            that feature is ablated. Populated by the feature ablation runner.

    Returns:
        :class:`GnnEvalReport` with all metrics populated.
    """
    y_pred = [1 if s >= threshold else 0 for s in y_score]
    cm = compute_confusion(y_true, y_pred)
    f1 = cm.f1
    pa_f1 = point_adjusted_f1(y_true, y_score, threshold)
    auc = compute_auc_roc(y_true, y_score)

    return GnnEvalReport(
        model_tag=model_tag,
        num_samples=len(y_true),
        num_anomalies=sum(y_true),
        confusion=cm,
        f1=f1,
        pa_f1=pa_f1,
        auc_roc=auc,
        threshold=threshold,
        notes=notes,
        feature_ablation=feature_ablation or {},
    )


@dataclass
class ComparisonStudyRow:
    detector: str
    f1: float
    pa_f1: float
    auc_roc: float
    notes: str = ""


def run_comparison_study(
    y_true: list[int],
    detectors: list[tuple[str, list[float], str]],
    threshold: float = 0.5,
) -> list[ComparisonStudyRow]:
    """Compare multiple detectors on the same ground-truth labels.

    Args:
        y_true: Shared ground-truth binary labels.
        detectors: List of ``(name, y_scores, notes)`` tuples.
        threshold: Shared classification threshold for all detectors.

    Returns:
        List of :class:`ComparisonStudyRow`, one per detector, sorted by F1 descending.
    """
    rows = []
    for name, y_score, notes in detectors:
        report = evaluate_gnn(name, y_true, y_score, threshold=threshold, notes=notes)
        rows.append(ComparisonStudyRow(
            detector=name,
            f1=report.f1,
            pa_f1=report.pa_f1,
            auc_roc=report.auc_roc,
            notes=notes,
        ))
    return sorted(rows, key=lambda r: r.f1, reverse=True)
