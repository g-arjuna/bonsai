"""MLRemediationSelector — picks the best remediation action using Model C.

Model C is a multi-class classifier trained on historical Remediation nodes.
Input: feature vector from Detection + one-hot encoded candidate actions.
Output: the candidate action with highest predicted success probability.

Falls back to the existing playbook-catalog selection when:
  - Model is not loaded
  - Confidence is below CONFIDENCE_FLOOR
  - Only one candidate is available

Usage in RemediationExecutor:
    selector = MLRemediationSelector.load("models/remediation_v1.joblib")
    best = selector.select(detection, ["srl_bgp_admin_state_bounce", "no_action_alert_only"])
    if best:
        playbook = catalog.get(best)
"""
from __future__ import annotations

from typing import Optional

import joblib
import numpy as np

from .detection import Detection
from .ml_detector import features_to_vector

CONFIDENCE_FLOOR = 0.55   # minimum predicted success probability to act


class MLRemediationSelector:
    """Select the remediation action most likely to succeed for a given detection."""

    def __init__(self, model, action_classes: list[str]) -> None:
        self._model         = model
        self._action_classes = action_classes   # ordered list of class labels from training

    @classmethod
    def load(cls, model_path: str) -> "MLRemediationSelector":
        bundle = joblib.load(model_path)
        return cls(bundle["model"], bundle["action_classes"])

    def select(self, detection: Detection, candidates: list[str]) -> Optional[str]:
        """Return the candidate action with the highest predicted success probability.

        Returns None if confidence is too low or no candidates provided.
        """
        if not candidates:
            return None
        if len(candidates) == 1:
            return candidates[0]

        best_action = None
        best_prob   = 0.0

        for action in candidates:
            prob = self._success_probability(detection, action)
            if prob > best_prob:
                best_prob   = prob
                best_action = action

        if best_prob < CONFIDENCE_FLOOR:
            return None
        return best_action

    def _success_probability(self, detection: Detection, action: str) -> float:
        """Predict P(status == 'success') for a (features, action) pair."""
        vec = self._encode(detection, action)
        model = self._model

        if hasattr(model, "predict_proba"):
            # Binary or multi-class; look for the "success" class index.
            proba = model.predict_proba(vec.reshape(1, -1))[0]
            classes = list(model.classes_)
            if "success" in classes:
                return float(proba[classes.index("success")])
            return float(proba[-1])   # last class as fallback

        if hasattr(model, "decision_function"):
            raw = float(model.decision_function(vec.reshape(1, -1))[0])
            return float(np.clip(0.5 + raw, 0.0, 1.0))

        raise TypeError(f"Model {type(model)} has no recognised predict interface")

    def _encode(self, detection: Detection, action: str) -> np.ndarray:
        """Concatenate the base feature vector with a one-hot action encoding."""
        base = features_to_vector(detection.features)

        # One-hot encode the candidate action against all known action classes.
        action_vec = np.zeros(len(self._action_classes), dtype=np.float32)
        if action in self._action_classes:
            action_vec[self._action_classes.index(action)] = 1.0

        return np.concatenate([base, action_vec])
