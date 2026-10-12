"""A01 regression tests: Conformal calibration claims and TSAD evaluation metrics.

Reproduces the two counterexamples from BONSAI_AGENTIC_NETOPS_STRATEGY_2026.md §3
with independent mathematical expectations before and after fixing conformal.py
and eval.py.
"""
from __future__ import annotations

import math
import sys
from pathlib import Path

import numpy as np
import pytest

ROOT = Path(__file__).resolve().parents[1]
PYTHON_DIR = ROOT / "python"
if str(PYTHON_DIR) not in sys.path:
    sys.path.insert(0, str(PYTHON_DIR))

from bonsai_ml.gnn.conformal import ConformalCalibrator
from bonsai_ml.gnn.eval import evaluate_gnn, point_adjusted_f1, run_comparison_study


def test_counterexample_1_anomaly_recall_calibration_covers_anomalies() -> None:
    """Counterexample 1 (Strategy §3):
    With calibration scores [0.5]*20 + [0.1]*5 and labels [0]*20 + [1]*5,
    a calibrator claiming (1 - alpha) = 0.90 anomaly coverage must calibrate
    nonconformity on positive (anomalous) samples so at least 90% of anomalies
    are captured (recall >= 0.90), rather than setting threshold=0.5 from clean
    samples and achieving 0% anomaly recall.
    """
    scores = [0.5] * 20 + [0.1] * 5
    labels = [0] * 20 + [1] * 5

    cal = ConformalCalibrator(alpha=0.1)
    res = cal.calibrate(scores, labels)

    # Threshold must be <= 0.1 so that the anomalous samples (score=0.1) are covered
    assert res.threshold <= 0.1 + 1e-9, (
        f"Expected anomaly-recall threshold <= 0.1, got {res.threshold}"
    )
    assert res.empirical_coverage >= 0.90, (
        f"Expected empirical anomaly recall >= 0.90, got {res.empirical_coverage}"
    )
    preds = cal.predict(scores)
    anomaly_preds = preds[np.array(labels) == 1]
    assert float(anomaly_preds.mean()) >= 0.90


def test_clean_false_alarm_calibration_does_not_claim_anomaly_recall_guarantee() -> None:
    """When calibrating only on clean (label=0) scores for false-alarm control,
    coverage_interval() must NOT claim an anomaly recall coverage_guarantee.
    """
    scores = [0.5] * 20 + [0.1] * 5
    labels = [0] * 20 + [1] * 5

    cal = ConformalCalibrator(alpha=0.1, calibration_target="clean_false_alarm")
    res = cal.calibrate(scores, labels)
    assert math.isclose(res.threshold, 0.5, rel_tol=1e-6)

    ci = cal.coverage_interval(scores)
    assert ci["calibration_target"] == "clean_false_alarm"
    assert ci["anomaly_coverage_guarantee"] is None
    assert math.isclose(ci["clean_specificity_bound"], 0.9, rel_tol=1e-6)


def test_anomaly_recall_calibration_rejects_missing_positive_samples() -> None:
    """Calibrating for anomaly_recall without any label=1 examples must raise ValueError."""
    cal = ConformalCalibrator(alpha=0.1)
    with pytest.raises(ValueError, match="anomalous"):
        cal.calibrate([0.1, 0.2, 0.3], [0, 0, 0])


def test_counterexample_2_late_single_point_detection_is_not_inflated_in_primary_metrics() -> None:
    """Counterexample 2 (Strategy §3):
    For 100 normal points, 100 anomalous points, then 100 normal points,
    flagging only the last anomalous point (index 199) gives raw F1 = 2/101 (~0.019802),
    detection delay = 99 steps, and within-delay event recall (max_delay_steps=5) = 0.0.
    """
    y_true = [0] * 100 + [1] * 100 + [0] * 100
    y_score_late = [0.0] * 300
    y_score_late[199] = 0.9  # Only the 100th point of the anomaly window is flagged

    expected_raw_f1 = 2.0 / 101.0  # TP=1, FP=0, FN=99 -> 2*1 / (2*1 + 0 + 99) = 2/101
    report = evaluate_gnn("late_detector", y_true, y_score_late, threshold=0.5)

    assert math.isclose(report.f1, expected_raw_f1, rel_tol=1e-6)
    assert report.event_recall == 1.0
    assert report.mean_detection_delay_steps == 99.0
    assert report.within_delay_event_recall == 0.0
    assert report.false_alert_count == 0
    assert math.isclose(report.false_alert_rate, 0.0, abs_tol=1e-9)

    # Compare an early detector (flags first 10 points of the anomaly window, delay=0)
    # against the late 1-point detector (delay=99).
    y_score_early = [0.0] * 300
    for idx in range(100, 110):
        y_score_early[idx] = 0.9

    early_report = evaluate_gnn("early_detector", y_true, y_score_early, threshold=0.5, max_delay_steps=5)
    assert early_report.mean_detection_delay_steps == 0.0
    assert early_report.within_delay_event_recall == 1.0
    assert early_report.f1 > report.f1

    comparison = run_comparison_study(
        y_true,
        [
            ("late_1pt", y_score_late, "late single point"),
            ("early_10pt", y_score_early, "early 10 points"),
        ],
        threshold=0.5,
    )
    assert comparison[0].detector == "early_10pt"
