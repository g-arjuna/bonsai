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
    """Output schema for a GNN evaluation run — feeds the model card.

    Primary metrics are raw point-wise ``f1``, ``event_recall``,
    ``within_delay_event_recall``, ``mean_detection_delay_steps``, and
    ``false_alert_rate``. Point-adjusted F1 (``pa_f1``) is retained only as a
    diagnostic field because a single late detection in a long anomaly window
    inflates ``pa_f1`` to 1.0 (see BONSAI_AGENTIC_NETOPS_STRATEGY_2026.md §3).
    """

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
    event_recall: float = 0.0
    within_delay_event_recall: float = 0.0
    mean_detection_delay_steps: float = float("nan")
    false_alert_count: int = 0
    false_alert_rate: float = 0.0
    pa_f1_is_diagnostic_only: bool = True

    def as_dict(self) -> dict:
        d = asdict(self)
        d["confusion"] = asdict(self.confusion)
        return d

    def summary_line(self) -> str:
        delay_str = (
            f"{self.mean_detection_delay_steps:.1f}"
            if not math.isnan(self.mean_detection_delay_steps)
            else "n/a"
        )
        return (
            f"{self.model_tag}: F1={self.f1:.3f} "
            f"EventRecall={self.event_recall:.3f} "
            f"EarlyEventRecall={self.within_delay_event_recall:.3f} "
            f"Delay={delay_str} "
            f"FAR={self.false_alert_rate:.4f} "
            f"PA-F1={self.pa_f1:.3f}(diag) "
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


def extract_anomaly_segments(y_true: list[int]) -> list[tuple[int, int]]:
    """Return contiguous (start_idx, end_idx) inclusive segments where y_true == 1."""
    segments: list[tuple[int, int]] = []
    in_segment = False
    start = 0
    for i, label in enumerate(y_true):
        if label == 1 and not in_segment:
            in_segment = True
            start = i
        elif label == 0 and in_segment:
            segments.append((start, i - 1))
            in_segment = False
    if in_segment:
        segments.append((start, len(y_true) - 1))
    return segments


def compute_event_metrics(
    y_true: list[int],
    y_pred: list[int],
    max_delay_steps: int = 5,
) -> tuple[float, float, float]:
    """Compute (event_recall, within_delay_event_recall, mean_detection_delay_steps).

    Args:
        y_true: Ground-truth binary labels (1=anomalous).
        y_pred: Predicted binary labels (1=anomalous).
        max_delay_steps: Maximum allowed steps after segment start for an event
            detection to count toward ``within_delay_event_recall``.

    Returns:
        Tuple of ``(event_recall, within_delay_event_recall, mean_detection_delay_steps)``.
    """
    segments = extract_anomaly_segments(y_true)
    if not segments:
        return 0.0, 0.0, float("nan")

    detected = 0
    detected_within_delay = 0
    delays: list[int] = []

    for seg_start, seg_end in segments:
        first_hit = None
        for j in range(seg_start, seg_end + 1):
            if y_pred[j] == 1:
                first_hit = j
                break
        if first_hit is not None:
            detected += 1
            delay = first_hit - seg_start
            delays.append(delay)
            if delay <= max_delay_steps:
                detected_within_delay += 1

    n_seg = len(segments)
    event_recall = detected / n_seg
    within_delay_recall = detected_within_delay / n_seg
    mean_delay = sum(delays) / len(delays) if delays else float("nan")
    return event_recall, within_delay_recall, mean_delay


def point_adjusted_f1(
    y_true: list[int],
    y_score: list[float],
    threshold: float = 0.5,
) -> float:
    """Compute point-adjustment F1 (PA-F1) — DIAGNOSTIC ONLY.

    WARNING: If any single point in a contiguous anomaly segment is detected
    (even the very last point of a 100-step window), PA-F1 marks the entire
    segment as detected. Do NOT use PA-F1 as the primary ranking metric; use
    raw ``f1``, ``within_delay_event_recall``, ``mean_detection_delay_steps``,
    and ``false_alert_rate`` from :func:`evaluate_gnn`.
    """
    if len(y_true) != len(y_score):
        raise ValueError("y_true and y_score must have equal length")
    if not y_true:
        return 0.0

    y_pred = [1 if s >= threshold else 0 for s in y_score]
    anomaly_segments = extract_anomaly_segments(y_true)

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
    max_delay_steps: int = 5,
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
        max_delay_steps: Maximum allowed detection delay in steps for
            ``within_delay_event_recall`` (default 5).

    Returns:
        :class:`GnnEvalReport` with all metrics populated.
    """
    if len(y_true) != len(y_score):
        raise ValueError("y_true and y_score must have equal length")

    y_pred = [1 if s >= threshold else 0 for s in y_score]
    cm = compute_confusion(y_true, y_pred)
    f1 = cm.f1
    pa_f1 = point_adjusted_f1(y_true, y_score, threshold)
    auc = compute_auc_roc(y_true, y_score)
    event_recall, within_delay_recall, mean_delay = compute_event_metrics(
        y_true, y_pred, max_delay_steps=max_delay_steps
    )
    total_normal = cm.tn + cm.fp
    false_alert_rate = (cm.fp / total_normal) if total_normal > 0 else 0.0

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
        event_recall=event_recall,
        within_delay_event_recall=within_delay_recall,
        mean_detection_delay_steps=mean_delay,
        false_alert_count=cm.fp,
        false_alert_rate=false_alert_rate,
        pa_f1_is_diagnostic_only=True,
    )


@dataclass
class ComparisonStudyRow:
    detector: str
    f1: float
    pa_f1: float
    auc_roc: float
    notes: str = ""
    event_recall: float = 0.0
    within_delay_event_recall: float = 0.0
    mean_detection_delay_steps: float = float("nan")
    false_alert_rate: float = 0.0


def run_comparison_study(
    y_true: list[int],
    detectors: list[tuple[str, list[float], str]],
    threshold: float = 0.5,
    max_delay_steps: int = 5,
) -> list[ComparisonStudyRow]:
    """Compare multiple detectors on the same ground-truth labels.

    Args:
        y_true: Shared ground-truth binary labels.
        detectors: List of ``(name, y_scores, notes)`` tuples.
        threshold: Shared classification threshold for all detectors.
        max_delay_steps: Max allowed delay for early event detection credit.

    Returns:
        List of :class:`ComparisonStudyRow`, sorted by raw ``f1`` and
        ``within_delay_event_recall`` descending (never by inflated ``pa_f1``).
    """
    rows = []
    for name, y_score, notes in detectors:
        report = evaluate_gnn(
            name,
            y_true,
            y_score,
            threshold=threshold,
            notes=notes,
            max_delay_steps=max_delay_steps,
        )
        rows.append(
            ComparisonStudyRow(
                detector=name,
                f1=report.f1,
                pa_f1=report.pa_f1,
                auc_roc=report.auc_roc,
                notes=notes,
                event_recall=report.event_recall,
                within_delay_event_recall=report.within_delay_event_recall,
                mean_detection_delay_steps=report.mean_detection_delay_steps,
                false_alert_rate=report.false_alert_rate,
            )
        )
    return sorted(
        rows,
        key=lambda r: (r.f1, r.within_delay_event_recall, -r.false_alert_rate),
        reverse=True,
    )
