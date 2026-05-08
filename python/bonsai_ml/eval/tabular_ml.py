"""Tabular ML detector evaluation for Bonsai chaos archives.

The evaluator is intentionally model-interface driven rather than tied to a
specific sklearn class.  It accepts synthetic feature windows today and real
archive-derived windows later, converts model scores into DetectionEvent rows,
then reuses the rule-baseline evaluator so rules, tabular ML, and GNN reports
share one metric contract.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, Iterable, Mapping, Sequence

from .rule_baseline import (
    DEFAULT_GRACE_NS,
    DetectionEvent,
    FaultInjection,
    RuleEvaluationReport,
    evaluate_rule_baseline,
)


DEFAULT_DETECTOR_ID = "ml_anomaly_v1"


@dataclass(frozen=True, slots=True)
class TabularFeatureWindow:
    """One timestamped tabular feature vector to score."""

    sample_id: str
    target: str
    observed_at_ns: int
    features: tuple[float, ...]
    metadata: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_mapping(cls, row: Mapping[str, Any]) -> "TabularFeatureWindow":
        feature_values = row.get("features")
        if feature_values is None:
            feature_values = [
                value
                for key, value in row.items()
                if key not in _RESERVED_WINDOW_KEYS and _is_number(value)
            ]
        if isinstance(feature_values, Mapping):
            ordered_values = [feature_values[key] for key in sorted(feature_values)]
        else:
            ordered_values = list(feature_values)

        observed_at_ns = _parse_ns(
            row.get("observed_at_ns")
            or row.get("timestamp_ns")
            or row.get("occurred_at_ns")
            or row.get("timestamp")
        )
        target = str(row.get("target") or row.get("hostname") or row.get("device") or "")
        sample_id = str(row.get("sample_id") or row.get("id") or f"{target}:{observed_at_ns}")
        return cls(
            sample_id=sample_id,
            target=target,
            observed_at_ns=observed_at_ns,
            features=tuple(float(value) for value in ordered_values),
            metadata=dict(row),
        )


@dataclass(frozen=True, slots=True)
class TabularScoredWindow:
    """A feature window plus the model's anomaly score."""

    window: TabularFeatureWindow
    score: float
    is_detection: bool

    def to_detection(self, detector_id: str) -> DetectionEvent | None:
        if not self.is_detection:
            return None
        return DetectionEvent(
            detection_id=f"{detector_id}:{self.window.sample_id}",
            rule_id=detector_id,
            target=self.window.target,
            detected_at_ns=self.window.observed_at_ns,
            metadata={**self.window.metadata, "ml_score": self.score},
        )


@dataclass(slots=True)
class TabularMLEvaluationReport:
    """Structured ML evaluation wrapper with shared rule metrics."""

    detector_id: str
    threshold: float
    scored_windows: list[TabularScoredWindow]
    baseline: RuleEvaluationReport

    def as_dict(self) -> dict[str, Any]:
        return {
            "detector_id": self.detector_id,
            "threshold": self.threshold,
            "windows_scored": len(self.scored_windows),
            "detections": sum(1 for row in self.scored_windows if row.is_detection),
            "baseline": self.baseline.as_dict(),
        }

    def to_markdown(self) -> str:
        lines = [
            "# Tabular ML Detector Evaluation",
            "",
            f"- Detector: {self.detector_id}",
            f"- Threshold: {self.threshold:.3f}",
            f"- Windows scored: {len(self.scored_windows)}",
            f"- Detections: {sum(1 for row in self.scored_windows if row.is_detection)}",
            "",
            self.baseline.to_markdown().rstrip(),
        ]
        return "\n".join(lines) + "\n"


def evaluate_tabular_ml_detector(
    faults: Iterable[FaultInjection | Mapping[str, Any]],
    windows: Iterable[TabularFeatureWindow | Mapping[str, Any]],
    model: Any,
    *,
    detector_id: str = DEFAULT_DETECTOR_ID,
    threshold: float = 0.5,
    grace_ns: int = DEFAULT_GRACE_NS,
) -> TabularMLEvaluationReport:
    """Evaluate a tabular ML detector against chaos ground truth.

    Fault rows are normalised to ``detector_id`` so the one ML detector can be
    compared against all labelled anomaly windows.  Negative fault rows
    (``should_trigger=false``) still produce true-negative or false-positive
    counts through the shared evaluator.
    """
    fault_rows = [_coerce_ml_fault(row, detector_id) for row in faults]
    scored_windows = [
        _score_window(_coerce_window(window), model, threshold=threshold)
        for window in windows
    ]
    detections = [
        detection
        for detection in (row.to_detection(detector_id) for row in scored_windows)
        if detection is not None
    ]
    baseline = evaluate_rule_baseline(fault_rows, detections, grace_ns=grace_ns)
    return TabularMLEvaluationReport(
        detector_id=detector_id,
        threshold=threshold,
        scored_windows=scored_windows,
        baseline=baseline,
    )


def score_tabular_model(model: Any, features: Sequence[float]) -> float:
    """Return an anomaly score in [0, 1] for a duck-typed tabular model."""
    matrix = [list(float(value) for value in features)]
    if hasattr(model, "decision_function"):
        raw = _first_scalar(model.decision_function(matrix))
        return _clamp01(0.5 - raw)
    if hasattr(model, "predict_proba"):
        proba = model.predict_proba(matrix)
        row = proba[0]
        return _clamp01(float(row[1] if len(row) > 1 else row[0]))
    if hasattr(model, "predict"):
        prediction = _first_scalar(model.predict(matrix))
        if prediction == -1:
            return 1.0
        if prediction == 1:
            return 0.0
        return _clamp01(prediction)
    raise TypeError(f"Model {type(model)} has no recognised scoring interface")


def _score_window(
    window: TabularFeatureWindow,
    model: Any,
    *,
    threshold: float,
) -> TabularScoredWindow:
    score = score_tabular_model(model, window.features)
    return TabularScoredWindow(
        window=window,
        score=score,
        is_detection=score >= threshold,
    )


def _coerce_window(value: TabularFeatureWindow | Mapping[str, Any]) -> TabularFeatureWindow:
    return value if isinstance(value, TabularFeatureWindow) else TabularFeatureWindow.from_mapping(value)


def _coerce_ml_fault(value: FaultInjection | Mapping[str, Any], detector_id: str) -> FaultInjection:
    fault = value if isinstance(value, FaultInjection) else FaultInjection.from_mapping(dict(value))
    return FaultInjection(
        fault_id=fault.fault_id,
        rule_id=detector_id,
        target=fault.target,
        injected_at_ns=fault.injected_at_ns,
        healed_at_ns=fault.healed_at_ns,
        should_trigger=fault.should_trigger,
        metadata=fault.metadata,
    )


def _first_scalar(value: Any) -> float:
    current = value
    while isinstance(current, (list, tuple)):
        current = current[0]
    if hasattr(current, "tolist"):
        return _first_scalar(current.tolist())
    return float(current)


def _clamp01(value: float) -> float:
    return max(0.0, min(1.0, float(value)))


def _parse_ns(value: Any) -> int:
    if value in (None, ""):
        raise ValueError("timestamp is required")
    if isinstance(value, bool):
        raise ValueError("boolean is not a timestamp")
    if isinstance(value, (int, float)):
        return int(value * 1_000_000_000) if value < 1_000_000_000_000_000 else int(value)
    if isinstance(value, str):
        stripped = value.strip()
        try:
            numeric = float(stripped)
        except ValueError:
            dt = datetime.fromisoformat(stripped.replace("Z", "+00:00"))
            if dt.tzinfo is None:
                dt = dt.replace(tzinfo=timezone.utc)
            return int(dt.timestamp() * 1_000_000_000)
        return int(numeric * 1_000_000_000) if numeric < 1_000_000_000_000_000 else int(numeric)
    raise TypeError(f"unsupported timestamp type: {type(value)!r}")


def _is_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


_RESERVED_WINDOW_KEYS = {
    "sample_id",
    "id",
    "target",
    "hostname",
    "device",
    "observed_at_ns",
    "timestamp_ns",
    "occurred_at_ns",
    "timestamp",
    "features",
    "label",
}
