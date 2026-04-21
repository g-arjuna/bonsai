"""MLDetector — drop-in replacement for RuleDetector backed by a trained sklearn model.

Usage:
    from bonsai_sdk.ml_detector import MLDetector

    detectors = [
        ...existing RuleDetector instances...,
        MLDetector(
            rule_id="ml_anomaly_v1",
            model_path="models/anomaly_v1.joblib",
            threshold=0.6,
            severity="warn",
        ),
    ]

The rule engine treats MLDetector identically to RuleDetector: same ABC, same
extract_features / detect contract. Only detect() changes — it calls model.predict()
instead of applying thresholds.

features_to_vector() is a module-level function shared by training and inference.
Both paths call the same code, preventing training/inference skew.
"""
from __future__ import annotations

import json
from typing import TYPE_CHECKING, Optional

import joblib
import numpy as np

from .detection import Detector, Features

if TYPE_CHECKING:
    from .client import BonsaiClient


# ── Feature vector contract ────────────────────────────────────────────────────

# Ordered list of numeric feature names extracted from a Features object.
# Categorical fields (device_address, event_type, etc.) are excluded or encoded.
# This list is the ML contract — do not reorder; append only.
NUMERIC_FEATURES = [
    "peer_count_total",
    "peer_count_established",
    "recent_flap_count",
    "occurred_at_ns",           # raw; models treat as a relative time signal
]

# Categorical fields encoded as integers.
OPER_STATUS_ENCODING = {"up": 1, "down": 0, "": -1}
EVENT_TYPE_ENCODING  = {
    "bgp_neighbor_state_change":   1,
    "interface_oper_status_change": 2,
    "lldp_neighbor_change":        3,
    "bfd_session_state_change":    4,
}


def features_to_vector(features: Features) -> np.ndarray:
    """Convert a Features dataclass to a fixed-length float32 numpy array.

    The vector layout is:
      [peer_count_total, peer_count_established, recent_flap_count,
       occurred_at_ns_scaled, oper_status_encoded, event_type_encoded]

    occurred_at_ns is scaled to seconds-since-epoch to keep values in a
    reasonable range alongside counts.
    """
    vec = [
        float(features.peer_count_total),
        float(features.peer_count_established),
        float(features.recent_flap_count),
        features.occurred_at_ns / 1e9,          # ns → seconds
        float(OPER_STATUS_ENCODING.get(features.oper_status.lower(), -1)),
        float(EVENT_TYPE_ENCODING.get(features.event_type, 0)),
    ]
    return np.array(vec, dtype=np.float32)


def load_model(model_path: str):
    """Load a joblib-serialised sklearn model (IsolationForest, etc.)."""
    return joblib.load(model_path)


# ── Shared feature extraction ─────────────────────────────────────────────────

def extract_features_for_event(event, client: "BonsaiClient") -> Features:
    """Canonical feature extractor shared by rule detectors and MLDetector.

    Rule detectors apply their own gating (event_type filter, state transition
    filter) BEFORE calling this. MLDetector calls this unconditionally and lets
    the model score every event — the model decides what's anomalous.

    Keeping extraction in one place prevents training/inference skew (T0-6).
    """
    detail: dict = {}
    try:
        detail = json.loads(event.detail_json or "{}")
    except (json.JSONDecodeError, AttributeError):
        pass

    f = Features(
        device_address=event.device_address,
        event_type=event.event_type,
        detail=detail,
        occurred_at_ns=event.occurred_at_ns,
        state_change_event_id=getattr(event, "state_change_event_id", ""),
    )

    f.oper_status = detail.get("oper_status", detail.get("new_state", ""))

    if event.event_type in ("bgp_session_change", "bfd_session_change"):
        f.peer_address = detail.get("peer", "")
        f.old_state    = detail.get("old_state", "")
        f.new_state    = detail.get("new_state", "")
        if event.event_type == "bfd_session_change":
            f.if_name = detail.get("if_name", "")

    if event.event_type == "bgp_session_change":
        try:
            neighbors = client.get_bgp_neighbors(event.device_address)
            f.peer_count_total       = len(neighbors)
            f.peer_count_established = sum(
                1 for n in neighbors if n.session_state == "established"
            )
        except Exception:
            pass

    if event.event_type in ("interface_oper_status_change", "interface_stats"):
        f.if_name     = detail.get("if_name", "")
        f.oper_status = detail.get("oper_status", "")

    return f


# ── MLDetector ────────────────────────────────────────────────────────────────

class MLDetector(Detector):
    """Anomaly detector backed by a trained sklearn or PyTorch model.

    Compatible with any model that exposes .predict() returning a scalar in [0, 1],
    or a scikit-learn model where decision_function() < 0 means anomalous.
    """

    def __init__(
        self,
        rule_id: str,
        model_path: str,
        threshold: float = 0.5,
        severity: str = "warn",
        auto_remediate: bool = False,
    ) -> None:
        self.rule_id        = rule_id
        self.severity       = severity
        self.auto_remediate = auto_remediate
        self._model         = load_model(model_path)
        self._threshold     = threshold

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        # MLDetector scores every event — no gating here. Gating is the rule's job.
        return extract_features_for_event(event, client)

    def detect(self, features: Features) -> Optional[str]:
        vec   = features_to_vector(features).reshape(1, -1)
        score = self._anomaly_score(vec)
        if score >= self._threshold:
            return (
                f"ML({self.rule_id}): anomaly score {score:.3f} "
                f"(threshold {self._threshold})"
            )
        return None

    def _anomaly_score(self, vec: np.ndarray) -> float:
        """Return a score in [0, 1] where higher = more anomalous.

        IsolationForest: decision_function returns negative values for anomalies.
        We flip and clip to [0, 1].
        """
        model = self._model
        if hasattr(model, "decision_function"):
            raw = float(model.decision_function(vec)[0])
            # decision_function range is roughly [-0.5, 0.5]; map to [0, 1].
            return float(np.clip(0.5 - raw, 0.0, 1.0))
        if hasattr(model, "predict_proba"):
            return float(model.predict_proba(vec)[0][1])
        if hasattr(model, "predict"):
            return float(model.predict(vec)[0])
        raise TypeError(f"Model {type(model)} has no recognised predict interface")
