"""Shared readiness thresholds and validation helpers for ML training."""
from __future__ import annotations

from collections import Counter
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import TYPE_CHECKING

import numpy as np
import pandas as pd

TRAINING_HYGIENE_CUTOFF_ISO = "2026-04-20T09:32:50+00:00"
TRAINING_HYGIENE_CUTOFF_NS = int(
    datetime.fromisoformat(TRAINING_HYGIENE_CUTOFF_ISO).timestamp() * 1e9
)

MODEL_A_MIN_ANOMALY_ROWS = 50
MODEL_A_MIN_NORMAL_ROWS = 200
MODEL_C_MIN_SUCCESS_ROWS = 50
MODEL_C_MIN_ACTIONS = 2
MODEL_C_MIN_STATUS_CLASSES = 2
MODEL_C_MIN_ROWS_PER_ACTION = 2
MODEL_C_MIN_ROWS_PER_STATUS = 2
VALID_REMEDIATION_STATUSES = {"success", "failed", "skipped"}

_ANOMALY_REQUIRED_COLUMNS = {
    "label",
    "event_type",
    "oper_status",
    "occurred_at_ns",
    "peer_count_total",
    "peer_count_established",
    "recent_flap_count",
}
_REMEDIATION_REQUIRED_COLUMNS = {
    "action",
    "status",
    "attempted_at_ns",
    "event_type",
    "oper_status",
    "occurred_at_ns",
    "peer_count_total",
    "peer_count_established",
    "recent_flap_count",
}
_NUMERIC_COLUMNS = [
    "peer_count_total",
    "peer_count_established",
    "recent_flap_count",
    "occurred_at_ns",
]


if TYPE_CHECKING:
    from .client import BonsaiClient


@dataclass
class ReadinessCheck:
    name: str
    ready: bool
    stats: dict[str, object]
    problems: list[str]


def filter_post_cutoff_remediations(
    df: pd.DataFrame, cutoff_ns: int = TRAINING_HYGIENE_CUTOFF_NS
) -> pd.DataFrame:
    if "attempted_at_ns" not in df.columns:
        return df.iloc[0:0].copy()
    attempted = pd.to_numeric(df["attempted_at_ns"], errors="coerce").fillna(0).astype("int64")
    return df.loc[attempted > cutoff_ns].copy()


def validate_anomaly_dataframe(df: pd.DataFrame) -> ReadinessCheck:
    problems = _missing_columns(df, _ANOMALY_REQUIRED_COLUMNS)
    stats = {
        "rows_total": int(len(df)),
        "rows_anomaly": 0,
        "rows_normal": 0,
        "null_cells": 0,
        "null_rate_required": 0.0,
        "rule_ids": 0,
        "label_distribution": {},
        "event_type_distribution": {},
        "numeric_ranges": {},
    }

    if not problems:
        labels = pd.to_numeric(df["label"], errors="coerce")
        label_counts = Counter(labels.dropna().astype(int).astype(str))
        stats["rows_anomaly"] = int((labels == 1).sum())
        stats["rows_normal"] = int((labels == 0).sum())
        null_cells, null_rate = _required_null_stats(df, _ANOMALY_REQUIRED_COLUMNS)
        stats["null_cells"] = null_cells
        stats["null_rate_required"] = null_rate
        stats["rule_ids"] = int(df.loc[labels == 1, "rule_id"].fillna("").astype(str).replace("", pd.NA).dropna().nunique()) if "rule_id" in df.columns else 0
        stats["label_distribution"] = dict(label_counts)
        stats["event_type_distribution"] = _value_counts(df["event_type"])
        stats["numeric_ranges"] = _numeric_ranges(df, _NUMERIC_COLUMNS)

        if stats["rows_anomaly"] < MODEL_A_MIN_ANOMALY_ROWS:
            problems.append(
                f"need at least {MODEL_A_MIN_ANOMALY_ROWS} anomaly rows, found {stats['rows_anomaly']}"
            )
        if stats["rows_normal"] < MODEL_A_MIN_NORMAL_ROWS:
            problems.append(
                f"need at least {MODEL_A_MIN_NORMAL_ROWS} normal rows, found {stats['rows_normal']}"
            )
        if labels.isna().any():
            problems.append("label column contains non-numeric values")
        if set(labels.dropna().astype(int).unique()) - {0, 1}:
            problems.append("label column must contain only 0/1 values")
        if stats["null_cells"] > 0:
            problems.append(
                f"required columns contain {stats['null_cells']} null cells "
                f"({stats['null_rate_required']:.2%} null rate)"
            )

        _validate_numeric_columns(df, _NUMERIC_COLUMNS, problems)
        _validate_feature_ranges(df, problems)

    return ReadinessCheck("model_a", not problems, stats, problems)


def validate_remediation_dataframe(
    df: pd.DataFrame, cutoff_ns: int = TRAINING_HYGIENE_CUTOFF_NS
) -> ReadinessCheck:
    problems = _missing_columns(df, _REMEDIATION_REQUIRED_COLUMNS)
    filtered = filter_post_cutoff_remediations(df, cutoff_ns=cutoff_ns)
    stats = {
        "rows_total": int(len(df)),
        "rows_post_cutoff": int(len(filtered)),
        "rows_success": 0,
        "action_classes": 0,
        "status_classes": 0,
        "null_cells": 0,
        "null_rate_required": 0.0,
        "cutoff_iso": TRAINING_HYGIENE_CUTOFF_ISO,
        "action_distribution": {},
        "status_distribution": {},
        "numeric_ranges": {},
    }

    if not problems:
        statuses = filtered["status"].fillna("").astype(str)
        actions = filtered["action"].fillna("").astype(str)
        action_distribution = _value_counts(actions)
        status_distribution = _value_counts(statuses)
        stats["rows_success"] = int((statuses == "success").sum())
        stats["action_classes"] = int(actions.replace("", pd.NA).dropna().nunique())
        stats["status_classes"] = int(statuses.replace("", pd.NA).dropna().nunique())
        null_cells, null_rate = _required_null_stats(filtered, _REMEDIATION_REQUIRED_COLUMNS)
        stats["null_cells"] = null_cells
        stats["null_rate_required"] = null_rate
        stats["action_distribution"] = action_distribution
        stats["status_distribution"] = status_distribution
        stats["numeric_ranges"] = _numeric_ranges(filtered, _NUMERIC_COLUMNS + ["attempted_at_ns"])

        if stats["rows_post_cutoff"] == 0:
            problems.append(
                f"no remediation rows found after cutoff {TRAINING_HYGIENE_CUTOFF_ISO}"
            )
        if stats["rows_success"] < MODEL_C_MIN_SUCCESS_ROWS:
            problems.append(
                f"need at least {MODEL_C_MIN_SUCCESS_ROWS} successful remediations after cutoff, found {stats['rows_success']}"
            )
        if stats["action_classes"] < MODEL_C_MIN_ACTIONS:
            problems.append(
                f"need at least {MODEL_C_MIN_ACTIONS} action types after cutoff, found {stats['action_classes']}"
            )
        if stats["status_classes"] < MODEL_C_MIN_STATUS_CLASSES:
            problems.append(
                f"need at least {MODEL_C_MIN_STATUS_CLASSES} remediation status classes after cutoff, found {stats['status_classes']}"
            )
        if stats["null_cells"] > 0:
            problems.append(
                f"required columns contain {stats['null_cells']} null cells after cutoff "
                f"({stats['null_rate_required']:.2%} null rate)"
            )

        invalid_statuses = sorted(set(statuses.unique()) - VALID_REMEDIATION_STATUSES - {""})
        if invalid_statuses:
            problems.append(f"unexpected remediation statuses: {', '.join(invalid_statuses)}")
        if (actions == "").any():
            problems.append("action column contains empty values after cutoff")

        for action, count in action_distribution.items():
            if count < MODEL_C_MIN_ROWS_PER_ACTION:
                problems.append(
                    f"action class '{action}' has {count} row(s) after cutoff; "
                    f"need at least {MODEL_C_MIN_ROWS_PER_ACTION}"
                )
        for status, count in status_distribution.items():
            if status and count < MODEL_C_MIN_ROWS_PER_STATUS:
                problems.append(
                    f"status class '{status}' has {count} row(s) after cutoff; "
                    f"need at least {MODEL_C_MIN_ROWS_PER_STATUS}"
                )

        _validate_numeric_columns(
            filtered, _NUMERIC_COLUMNS + ["attempted_at_ns"], problems, " after cutoff"
        )
        _validate_feature_ranges(filtered, problems, " after cutoff")
        _validate_remediation_time_order(filtered, problems)

    return ReadinessCheck("model_c", not problems, stats, problems)


def format_check(check: ReadinessCheck) -> str:
    lines = [f"{check.name}: {'READY' if check.ready else 'NOT READY'}"]
    for key, value in check.stats.items():
        lines.append(f"  {key}: {value}")
    if check.problems:
        lines.append("  problems:")
        for problem in check.problems:
            lines.append(f"    - {problem}")
    return "\n".join(lines)


def build_graph_readiness_from_summary(
    summary: dict[str, object],
) -> tuple[ReadinessCheck, ReadinessCheck]:
    detection_events = int(summary.get("detection_events", 0) or 0)
    state_change_events = int(summary.get("state_change_events", 0) or 0)
    rule_counts = Counter(_as_count_mapping(summary.get("rule_distribution", {})))
    action_counts = Counter(
        _as_count_mapping(summary.get("action_distribution_post_cutoff", {}))
    )
    status_counts = Counter(
        _as_count_mapping(summary.get("status_distribution_post_cutoff", {}))
    )
    remediation_rows_post_cutoff = int(
        summary.get("remediation_rows_post_cutoff", 0) or 0
    )

    model_a_problems: list[str] = []
    if detection_events < MODEL_A_MIN_ANOMALY_ROWS:
        model_a_problems.append(
            f"need at least {MODEL_A_MIN_ANOMALY_ROWS} DetectionEvents, found {detection_events}"
        )
    if state_change_events < MODEL_A_MIN_NORMAL_ROWS:
        model_a_problems.append(
            f"need at least {MODEL_A_MIN_NORMAL_ROWS} StateChangeEvents for normal sampling, found {state_change_events}"
        )

    model_a = ReadinessCheck(
        name="model_a",
        ready=not model_a_problems,
        stats={
            "detection_events": detection_events,
            "state_change_events": state_change_events,
            "distinct_rules": len(rule_counts),
            "top_rules": dict(rule_counts.most_common(5)),
        },
        problems=model_a_problems,
    )

    success_count = status_counts.get("success", 0)
    model_c_problems: list[str] = []
    if success_count < MODEL_C_MIN_SUCCESS_ROWS:
        model_c_problems.append(
            f"need at least {MODEL_C_MIN_SUCCESS_ROWS} successful remediations after cutoff, found {success_count}"
        )
    if len(action_counts) < MODEL_C_MIN_ACTIONS:
        model_c_problems.append(
            f"need at least {MODEL_C_MIN_ACTIONS} remediation actions after cutoff, found {len(action_counts)}"
        )
    if len(status_counts) < MODEL_C_MIN_STATUS_CLASSES:
        model_c_problems.append(
            f"need at least {MODEL_C_MIN_STATUS_CLASSES} remediation status classes after cutoff, found {len(status_counts)}"
        )

    model_c = ReadinessCheck(
        name="model_c",
        ready=not model_c_problems,
        stats={
            "cutoff_iso": str(summary.get("cutoff_iso", TRAINING_HYGIENE_CUTOFF_ISO)),
            "remediation_rows_post_cutoff": remediation_rows_post_cutoff,
            "successful_remediations_post_cutoff": success_count,
            "action_classes_post_cutoff": len(action_counts),
            "status_classes_post_cutoff": len(status_counts),
            "status_distribution_post_cutoff": dict(status_counts),
            "top_actions_post_cutoff": dict(action_counts.most_common(5)),
        },
        problems=model_c_problems,
    )

    return model_a, model_c


def summarize_graph_readiness(client: "BonsaiClient") -> dict[str, object]:
    detection_rows = client.query("MATCH (e:DetectionEvent) RETURN e.rule_id")
    state_rows = client.query("MATCH (e:StateChangeEvent) RETURN count(e)")
    remediation_rows = client.query(
        "MATCH (m:RemediationTrustMark)-[:TRUST_MARKS]->(r:Remediation) "
        "WHERE m.trustworthy = 1 "
        "RETURN r.action, r.status"
    )

    rule_counts = Counter(str((row + [""])[0] or "") for row in detection_rows)
    rule_counts.pop("", None)
    state_change_events = 0
    if state_rows and state_rows[0]:
        state_change_events = int(state_rows[0][0] or 0)

    remediation_rows_post_cutoff = 0
    action_counts: Counter[str] = Counter()
    status_counts: Counter[str] = Counter()
    for row in remediation_rows:
        action, status = row + [None] * (2 - len(row))
        remediation_rows_post_cutoff += 1
        action_s = str(action or "")
        status_s = str(status or "")
        if action_s:
            action_counts[action_s] += 1
        if status_s:
            status_counts[status_s] += 1

    return {
        "detection_events": len(detection_rows),
        "state_change_events": state_change_events,
        "rule_distribution": dict(rule_counts),
        "cutoff_iso": TRAINING_HYGIENE_CUTOFF_ISO,
        "remediation_rows_post_cutoff": remediation_rows_post_cutoff,
        "action_distribution_post_cutoff": dict(action_counts),
        "status_distribution_post_cutoff": dict(status_counts),
    }


def query_graph_readiness(client: "BonsaiClient") -> tuple[ReadinessCheck, ReadinessCheck]:
    return build_graph_readiness_from_summary(summarize_graph_readiness(client))


def _missing_columns(df: pd.DataFrame, required: set[str]) -> list[str]:
    missing = sorted(required - set(df.columns))
    if not missing:
        return []
    return [f"missing required columns: {', '.join(missing)}"]


def _required_null_stats(df: pd.DataFrame, columns: set[str]) -> tuple[int, float]:
    if len(df) == 0 or not columns:
        return 0, 0.0
    null_cells = int(df[list(columns)].isna().sum().sum())
    total_cells = len(df) * len(columns)
    return null_cells, null_cells / total_cells if total_cells else 0.0


def _value_counts(values: pd.Series) -> dict[str, int]:
    return {
        str(key): int(value)
        for key, value in values.fillna("").astype(str).value_counts().sort_index().items()
        if str(key)
    }


def _numeric_ranges(df: pd.DataFrame, columns: list[str]) -> dict[str, dict[str, float]]:
    ranges: dict[str, dict[str, float]] = {}
    for col in columns:
        if col not in df.columns or len(df) == 0:
            continue
        values = pd.to_numeric(df[col], errors="coerce")
        finite = values[np.isfinite(values.to_numpy(dtype=np.float64, na_value=np.nan))]
        if finite.empty:
            continue
        ranges[col] = {"min": float(finite.min()), "max": float(finite.max())}
    return ranges


def _validate_numeric_columns(
    df: pd.DataFrame,
    columns: list[str],
    problems: list[str],
    suffix: str = "",
) -> None:
    for col in columns:
        values = pd.to_numeric(df[col], errors="coerce")
        if values.isna().any():
            problems.append(f"{col} contains non-numeric values{suffix}")
            continue
        if not np.isfinite(values.to_numpy(dtype=np.float64)).all():
            problems.append(f"{col} contains non-finite numeric values{suffix}")
        if (values < 0).any():
            problems.append(f"{col} contains negative values{suffix}")


def _validate_feature_ranges(
    df: pd.DataFrame, problems: list[str], suffix: str = ""
) -> None:
    total = pd.to_numeric(df["peer_count_total"], errors="coerce")
    established = pd.to_numeric(df["peer_count_established"], errors="coerce")
    if not total.isna().any() and not established.isna().any():
        if (established > total).any():
            problems.append(f"peer_count_established exceeds peer_count_total{suffix}")

    occurred = pd.to_numeric(df["occurred_at_ns"], errors="coerce")
    if not occurred.isna().any() and (occurred <= 0).any():
        problems.append(f"occurred_at_ns must be positive{suffix}")


def _validate_remediation_time_order(
    df: pd.DataFrame, problems: list[str]
) -> None:
    occurred = pd.to_numeric(df["occurred_at_ns"], errors="coerce")
    attempted = pd.to_numeric(df["attempted_at_ns"], errors="coerce")
    if occurred.isna().any() or attempted.isna().any():
        return
    if (attempted < occurred).any():
        problems.append("attempted_at_ns is earlier than occurred_at_ns after cutoff")


def _as_count_mapping(raw: object) -> dict[str, int]:
    if not isinstance(raw, dict):
        return {}
    counts: dict[str, int] = {}
    for key, value in raw.items():
        key_s = str(key or "")
        if not key_s:
            continue
        counts[key_s] = int(value or 0)
    return counts
