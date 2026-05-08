"""Rule-baseline evaluation for Bonsai chaos archives.

This module is deliberately data-source agnostic: callers can feed it synthetic
fixtures now and real chaos-log / DetectionEvent records once the archive
matures.  The evaluator matches detections to labelled fault windows and emits
per-rule confusion metrics suitable for the later rules-vs-ML-vs-GNN study.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
from statistics import mean
from typing import Any, Iterable


DEFAULT_GRACE_NS = 30 * 1_000_000_000


@dataclass(frozen=True, slots=True)
class FaultInjection:
    """Ground-truth chaos injection window."""

    fault_id: str
    rule_id: str
    target: str
    injected_at_ns: int
    healed_at_ns: int | None = None
    should_trigger: bool = True
    metadata: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_mapping(cls, row: dict[str, Any]) -> "FaultInjection":
        rule_id = str(
            row.get("expected_detection_rule_id")
            or row.get("rule_id")
            or row.get("fault_type")
            or "unknown"
        )
        target = str(row.get("target") or row.get("hostname") or row.get("device") or "")
        injected_at_ns = _parse_ns(row.get("injected_at_ns") or row.get("inject_ns"))
        healed_at_ns = _parse_optional_ns(row.get("healed_at_ns") or row.get("heal_ns"))
        fault_id = str(row.get("fault_id") or f"{rule_id}:{target}:{injected_at_ns}")
        should_trigger = _parse_bool(row.get("should_trigger"), default=True)
        return cls(
            fault_id=fault_id,
            rule_id=rule_id,
            target=target,
            injected_at_ns=injected_at_ns,
            healed_at_ns=healed_at_ns,
            should_trigger=should_trigger,
            metadata=dict(row),
        )

    def window_end_ns(self, grace_ns: int) -> int:
        if self.healed_at_ns is not None:
            return self.healed_at_ns + grace_ns
        return self.injected_at_ns + grace_ns


@dataclass(frozen=True, slots=True)
class DetectionEvent:
    """Detected event produced by Bonsai or a synthetic fixture."""

    detection_id: str
    rule_id: str
    target: str
    detected_at_ns: int
    cleared_at_ns: int | None = None
    metadata: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_mapping(cls, row: dict[str, Any]) -> "DetectionEvent":
        rule_id = str(row.get("rule_id") or row.get("fault_type") or "unknown")
        target = str(row.get("target") or row.get("hostname") or row.get("device") or "")
        detected_at_ns = _parse_ns(
            row.get("detected_at_ns") or row.get("timestamp_ns") or row.get("timestamp")
        )
        cleared_at_ns = _parse_optional_ns(
            row.get("cleared_at_ns") or row.get("resolved_at_ns") or row.get("cleared_at")
        )
        detection_id = str(row.get("detection_id") or row.get("id") or f"{rule_id}:{target}:{detected_at_ns}")
        return cls(
            detection_id=detection_id,
            rule_id=rule_id,
            target=target,
            detected_at_ns=detected_at_ns,
            cleared_at_ns=cleared_at_ns,
            metadata=dict(row),
        )


@dataclass(slots=True)
class RuleEvaluation:
    """Per-rule confusion metrics and timing distributions."""

    rule_id: str
    true_positive: int = 0
    false_positive: int = 0
    false_negative: int = 0
    true_negative: int = 0
    latency_ns: list[int] = field(default_factory=list)
    clear_time_ns: list[int] = field(default_factory=list)
    matched_fault_ids: list[str] = field(default_factory=list)
    false_positive_detection_ids: list[str] = field(default_factory=list)
    false_negative_fault_ids: list[str] = field(default_factory=list)

    @property
    def precision(self) -> float | None:
        denom = self.true_positive + self.false_positive
        return self.true_positive / denom if denom else None

    @property
    def recall(self) -> float | None:
        denom = self.true_positive + self.false_negative
        return self.true_positive / denom if denom else None

    @property
    def f1(self) -> float | None:
        precision = self.precision
        recall = self.recall
        if precision is None or recall is None or precision + recall == 0:
            return None
        return 2 * precision * recall / (precision + recall)

    def as_dict(self) -> dict[str, Any]:
        latencies_ms = [ns / 1_000_000 for ns in self.latency_ns]
        clear_ms = [ns / 1_000_000 for ns in self.clear_time_ns]
        return {
            "rule_id": self.rule_id,
            "tp": self.true_positive,
            "fp": self.false_positive,
            "fn": self.false_negative,
            "tn": self.true_negative,
            "precision": self.precision,
            "recall": self.recall,
            "f1": self.f1,
            "latency_ms": _distribution(latencies_ms),
            "clear_time_ms": _distribution(clear_ms),
            "matched_fault_ids": self.matched_fault_ids,
            "false_positive_detection_ids": self.false_positive_detection_ids,
            "false_negative_fault_ids": self.false_negative_fault_ids,
        }


@dataclass(slots=True)
class RuleEvaluationReport:
    """Structured report for all evaluated rules."""

    rules: dict[str, RuleEvaluation]
    grace_ns: int
    total_faults: int
    total_detections: int

    def as_dict(self) -> dict[str, Any]:
        return {
            "grace_ns": self.grace_ns,
            "total_faults": self.total_faults,
            "total_detections": self.total_detections,
            "rules": {rule_id: rule.as_dict() for rule_id, rule in sorted(self.rules.items())},
        }

    def to_markdown(self) -> str:
        lines = [
            "# Detection Rule Baseline Evaluation",
            "",
            f"- Faults: {self.total_faults}",
            f"- Detections: {self.total_detections}",
            f"- Grace window: {self.grace_ns / 1_000_000_000:.0f}s",
            "",
            "| Rule | TP | FP | FN | TN | Precision | Recall | F1 | p95 latency ms |",
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
        for rule_id, evaluation in sorted(self.rules.items()):
            data = evaluation.as_dict()
            latency = data["latency_ms"]
            lines.append(
                "| {rule} | {tp} | {fp} | {fn} | {tn} | {precision} | {recall} | {f1} | {p95} |".format(
                    rule=rule_id,
                    tp=data["tp"],
                    fp=data["fp"],
                    fn=data["fn"],
                    tn=data["tn"],
                    precision=_fmt_float(data["precision"]),
                    recall=_fmt_float(data["recall"]),
                    f1=_fmt_float(data["f1"]),
                    p95=_fmt_float(latency.get("p95")),
                )
            )
        return "\n".join(lines) + "\n"


def evaluate_rule_baseline(
    faults: Iterable[FaultInjection | dict[str, Any]],
    detections: Iterable[DetectionEvent | dict[str, Any]],
    *,
    grace_ns: int = DEFAULT_GRACE_NS,
) -> RuleEvaluationReport:
    """Evaluate rule detections against chaos ground truth.

    Matching requires the same rule id, same target, and detection timestamp
    inside ``[injected_at_ns, healed_at_ns + grace_ns]``. One detection can match
    at most one fault. Negative fault rows with ``should_trigger=false`` count
    as true negatives unless a matching detection fires in their window.
    """
    fault_rows = [_coerce_fault(fault) for fault in faults]
    detection_rows = sorted(
        (_coerce_detection(detection) for detection in detections),
        key=lambda detection: detection.detected_at_ns,
    )
    rules = _initial_rules(fault_rows, detection_rows)
    matched_detection_ids: set[str] = set()

    for fault in sorted(fault_rows, key=lambda row: row.injected_at_ns):
        evaluation = rules.setdefault(fault.rule_id, RuleEvaluation(fault.rule_id))
        match = _first_matching_detection(
            fault,
            detection_rows,
            matched_detection_ids,
            grace_ns=grace_ns,
        )

        if fault.should_trigger:
            if match is None:
                evaluation.false_negative += 1
                evaluation.false_negative_fault_ids.append(fault.fault_id)
            else:
                evaluation.true_positive += 1
                evaluation.matched_fault_ids.append(fault.fault_id)
                evaluation.latency_ns.append(match.detected_at_ns - fault.injected_at_ns)
                matched_detection_ids.add(match.detection_id)
                if fault.healed_at_ns is not None and match.cleared_at_ns is not None:
                    evaluation.clear_time_ns.append(max(0, match.cleared_at_ns - fault.healed_at_ns))
        else:
            if match is None:
                evaluation.true_negative += 1
            else:
                evaluation.false_positive += 1
                evaluation.false_positive_detection_ids.append(match.detection_id)
                matched_detection_ids.add(match.detection_id)

    for detection in detection_rows:
        if detection.detection_id in matched_detection_ids:
            continue
        if _inside_any_fault_window(detection, fault_rows, grace_ns=grace_ns):
            continue
        evaluation = rules.setdefault(detection.rule_id, RuleEvaluation(detection.rule_id))
        evaluation.false_positive += 1
        evaluation.false_positive_detection_ids.append(detection.detection_id)

    return RuleEvaluationReport(
        rules=rules,
        grace_ns=grace_ns,
        total_faults=len(fault_rows),
        total_detections=len(detection_rows),
    )


def _initial_rules(
    faults: list[FaultInjection],
    detections: list[DetectionEvent],
) -> dict[str, RuleEvaluation]:
    rule_ids = {fault.rule_id for fault in faults} | {detection.rule_id for detection in detections}
    return {rule_id: RuleEvaluation(rule_id) for rule_id in sorted(rule_ids)}


def _first_matching_detection(
    fault: FaultInjection,
    detections: list[DetectionEvent],
    matched_detection_ids: set[str],
    *,
    grace_ns: int,
) -> DetectionEvent | None:
    window_end_ns = fault.window_end_ns(grace_ns)
    for detection in detections:
        if detection.detection_id in matched_detection_ids:
            continue
        if detection.rule_id != fault.rule_id or detection.target != fault.target:
            continue
        if fault.injected_at_ns <= detection.detected_at_ns <= window_end_ns:
            return detection
    return None


def _inside_any_fault_window(
    detection: DetectionEvent,
    faults: list[FaultInjection],
    *,
    grace_ns: int,
) -> bool:
    return any(
        fault.rule_id == detection.rule_id
        and fault.target == detection.target
        and fault.injected_at_ns <= detection.detected_at_ns <= fault.window_end_ns(grace_ns)
        for fault in faults
    )


def _coerce_fault(value: FaultInjection | dict[str, Any]) -> FaultInjection:
    return value if isinstance(value, FaultInjection) else FaultInjection.from_mapping(value)


def _coerce_detection(value: DetectionEvent | dict[str, Any]) -> DetectionEvent:
    return value if isinstance(value, DetectionEvent) else DetectionEvent.from_mapping(value)


def _parse_ns(value: Any) -> int:
    parsed = _parse_optional_ns(value)
    if parsed is None:
        raise ValueError("timestamp is required")
    return parsed


def _parse_optional_ns(value: Any) -> int | None:
    if value in (None, ""):
        return None
    if isinstance(value, bool):
        raise ValueError("boolean is not a timestamp")
    if isinstance(value, (int, float)):
        return int(value * 1_000_000_000) if value < 1_000_000_000_000_000 else int(value)
    if isinstance(value, str):
        stripped = value.strip()
        if not stripped:
            return None
        try:
            numeric = float(stripped)
        except ValueError:
            dt = datetime.fromisoformat(stripped.replace("Z", "+00:00"))
            if dt.tzinfo is None:
                dt = dt.replace(tzinfo=timezone.utc)
            return int(dt.timestamp() * 1_000_000_000)
        return int(numeric * 1_000_000_000) if numeric < 1_000_000_000_000_000 else int(numeric)
    raise TypeError(f"unsupported timestamp type: {type(value)!r}")


def _parse_bool(value: Any, *, default: bool) -> bool:
    if value in (None, ""):
        return default
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        return value.strip().lower() not in {"0", "false", "no", "n"}
    return bool(value)


def _distribution(values: list[float]) -> dict[str, float | None]:
    if not values:
        return {"count": 0, "mean": None, "p50": None, "p95": None, "p99": None}
    return {
        "count": len(values),
        "mean": mean(values),
        "p50": _percentile(values, 50),
        "p95": _percentile(values, 95),
        "p99": _percentile(values, 99),
    }


def _percentile(values: list[float], p: float) -> float:
    sorted_values = sorted(values)
    idx = p / 100 * (len(sorted_values) - 1)
    lo = int(idx)
    hi = min(lo + 1, len(sorted_values) - 1)
    frac = idx - lo
    return sorted_values[lo] * (1 - frac) + sorted_values[hi] * frac


def _fmt_float(value: float | None) -> str:
    return "n/a" if value is None else f"{value:.3f}"
