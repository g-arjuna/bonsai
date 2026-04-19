"""Training data export — queries the graph and writes Parquet for ML training.

Two exports:
  1. Labelled anomalies: one row per DetectionEvent with features + remediation outcome.
  2. Normal windows: random 60-second snapshots with no concurrent DetectionEvent (negative class).

Both exports share the same column schema so they can be concatenated into a single
training dataset. label=1 for anomalies, label=0 for normal windows.
"""
from __future__ import annotations

import json
import time
from dataclasses import fields
from typing import TYPE_CHECKING

import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq

from .detection import Features

if TYPE_CHECKING:
    from .client import BonsaiClient


# Columns that come from the graph query (not from features_json).
_GRAPH_COLS = ["detection_id", "rule_id", "severity", "fired_at_ns",
               "remediation_action", "remediation_status", "label"]

# All feature field names in declaration order.
_FEATURE_COLS = [f.name for f in fields(Features)]


def export_training_set(
    client: "BonsaiClient",
    output_path: str,
    since_ns: int = 0,
    until_ns: int | None = None,
) -> int:
    """Export DetectionEvents + features to a Parquet file.

    Returns the number of rows written (anomaly + normal combined).
    Requires pyarrow and pandas.
    """
    if until_ns is None:
        until_ns = int(time.time() * 1e9)

    anomaly_rows = _export_anomalies(client, since_ns, until_ns)
    normal_rows  = _export_normal_windows(client, since_ns, until_ns)

    all_rows = anomaly_rows + normal_rows
    if not all_rows:
        return 0

    df = pd.DataFrame(all_rows, columns=_GRAPH_COLS + _FEATURE_COLS)
    pq.write_table(pa.Table.from_pandas(df), output_path)
    return len(all_rows)


def _export_anomalies(client: "BonsaiClient", since_ns: int, until_ns: int) -> list[dict]:
    cypher = """
        MATCH (e:DetectionEvent)
        WHERE e.fired_at >= $since AND e.fired_at < $until
        OPTIONAL MATCH (r:Remediation)-[:RESOLVES]->(e)
        RETURN e.id, e.rule_id, e.severity, e.fired_at, e.features_json,
               r.action, r.status
    """
    # BonsaiClient.query doesn't support params yet — inline scalars.
    cypher = cypher.replace("$since", str(since_ns)).replace("$until", str(until_ns))
    rows = client.query(cypher)

    result = []
    for row in rows:
        det_id, rule_id, severity, fired_at, features_json, action, status = (
            row + [None] * (7 - len(row))
        )
        features = _parse_features(features_json)
        record = {
            "detection_id":         det_id or "",
            "rule_id":              rule_id or "",
            "severity":             severity or "",
            "fired_at_ns":          fired_at or 0,
            "remediation_action":   action or "",
            "remediation_status":   status or "",
            "label":                1,
            **features,
        }
        result.append(record)
    return result


def _export_normal_windows(
    client: "BonsaiClient", since_ns: int, until_ns: int, max_samples: int = 500
) -> list[dict]:
    """Sample StateChangeEvents that have NO concurrent DetectionEvent within ±30s."""
    cypher = """
        MATCH (e:StateChangeEvent)
        WHERE e.occurred_at >= $since AND e.occurred_at < $until
        AND NOT EXISTS {
            MATCH (d:DetectionEvent)
            WHERE abs(d.fired_at - e.occurred_at) < 30000000000
        }
        RETURN e.device_address, e.event_type, e.detail, e.occurred_at
        LIMIT $limit
    """
    cypher = (cypher
              .replace("$since", str(since_ns))
              .replace("$until", str(until_ns))
              .replace("$limit", str(max_samples)))
    rows = client.query(cypher)

    result = []
    for row in rows:
        addr, etype, detail_str, occurred_at = (row + [None] * (4 - len(row)))
        detail = json.loads(detail_str or "{}")
        features = _empty_features(addr or "", etype or "", detail, occurred_at or 0)
        record = {
            "detection_id":         "",
            "rule_id":              "",
            "severity":             "",
            "fired_at_ns":          occurred_at or 0,
            "remediation_action":   "",
            "remediation_status":   "",
            "label":                0,
            **features,
        }
        result.append(record)
    return result


def _parse_features(features_json: str | None) -> dict:
    if not features_json:
        return _empty_features("", "", {}, 0)
    try:
        d = json.loads(features_json)
        # Ensure all expected feature columns are present with defaults.
        defaults = {f.name: f.default for f in fields(Features)
                    if f.default is not f.default.__class__}  # skip non-scalar defaults
        return {col: d.get(col, defaults.get(col, "")) for col in _FEATURE_COLS}
    except (json.JSONDecodeError, TypeError):
        return _empty_features("", "", {}, 0)


def _empty_features(device_address: str, event_type: str, detail: dict, occurred_at: int) -> dict:
    base = {f.name: "" for f in fields(Features)}
    base.update({
        "device_address": device_address,
        "event_type":     event_type,
        "detail":         json.dumps(detail),
        "occurred_at_ns": occurred_at,
        "peer_count_total":       0,
        "peer_count_established": 0,
        "recent_flap_count":      0,
    })
    return base


# ── Remediation training export ────────────────────────────────────────────────

# Columns for the remediation training set.
_REM_COLS = [
    "remediation_id", "detection_id", "rule_id", "action",
    "status",          # "success" | "failed" | "skipped" — the label
    "vendor",
    "fired_at_ns",
]


def export_remediation_training_set(
    client: "BonsaiClient",
    output_path: str,
    since_ns: int = 0,
    until_ns: int | None = None,
) -> int:
    """Export Remediation nodes joined to DetectionEvent features for Model C training.

    Each row is one attempted remediation with:
      - Feature columns from the triggering DetectionEvent
      - action: what was attempted
      - status: success / failed / skipped (the multi-class label)
      - vendor: from features_json device_address (best-effort lookup)

    Returns the number of rows written.
    """
    if until_ns is None:
        until_ns = int(time.time() * 1e9)

    cypher = """
        MATCH (r:Remediation)-[:RESOLVES]->(e:DetectionEvent)
        WHERE e.fired_at >= $since AND e.fired_at < $until
        RETURN r.id, r.detection_id, e.rule_id, r.action, r.status, e.features_json, e.fired_at
    """
    cypher = cypher.replace("$since", str(since_ns)).replace("$until", str(until_ns))
    rows = client.query(cypher)

    result = []
    for row in rows:
        rem_id, det_id, rule_id, action, status, features_json, fired_at = (
            row + [None] * (7 - len(row))
        )
        features = _parse_features(features_json)
        record = {
            "remediation_id": rem_id or "",
            "detection_id":   det_id or "",
            "rule_id":        rule_id or "",
            "action":         action or "",
            "status":         status or "",
            "vendor":         features.get("device_address", ""),
            "fired_at_ns":    fired_at or 0,
            **features,
        }
        result.append(record)

    if not result:
        return 0

    df = pd.DataFrame(result)
    pq.write_table(pa.Table.from_pandas(df), output_path)
    return len(result)
