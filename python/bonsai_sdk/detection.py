"""Detection abstraction — shared base for rule-based and ML detectors.

Both phases use the same interface:
  Phase 4: RuleDetector.detect() applies threshold logic to Features
  Phase 5: MLDetector.detect() calls model.predict(features.to_vector())

Feature extraction is shared; features_json is stored on every DetectionEvent
so Phase 5 training requires no re-extraction from the graph.
"""
from __future__ import annotations

import json
import time
from abc import ABC, abstractmethod
from dataclasses import asdict, dataclass, field
from typing import TYPE_CHECKING, Optional

if TYPE_CHECKING:
    from .client import BonsaiClient


@dataclass
class Features:
    """Normalized feature vector extracted from an event + graph context."""
    # From the triggering event
    device_address: str
    event_type: str
    detail: dict

    # Graph context (populated by extract_features)
    peer_address: str = ""
    old_state: str = ""
    new_state: str = ""
    peer_count_total: int = 0
    peer_count_established: int = 0
    recent_flap_count: int = 0   # state changes for this peer in last 5 min
    if_name: str = ""
    oper_status: str = ""

    # Raw timestamp
    occurred_at_ns: int = 0
    # UUID of the StateChangeEvent that triggered this detection; empty for poll-based rules
    state_change_event_id: str = ""

    def to_json(self) -> str:
        return json.dumps(asdict(self))

    @classmethod
    def from_event(cls, event, detail: dict) -> "Features":
        return cls(
            device_address=event.device_address,
            event_type=event.event_type,
            detail=detail,
            occurred_at_ns=event.occurred_at_ns,
            state_change_event_id=getattr(event, "state_change_event_id", ""),
        )


@dataclass
class Detection:
    rule_id: str
    severity: str          # "info" | "warn" | "critical"
    features: Features
    reason: str            # human-readable explanation
    auto_remediate: bool = False
    remediation_action: str = ""   # e.g. "bgp_soft_clear"


class Detector(ABC):
    """Base class for rule-based and ML anomaly detectors.

    Subclasses implement extract_features() to gather context from the graph,
    and detect() to decide whether to fire. Only detect() changes when moving
    from rules to ML — everything else stays the same.
    """
    rule_id: str
    severity: str
    auto_remediate: bool = False
    remediation_action: str = ""
    # scope: 'local' (eval on collector), 'core' (eval on core), 'hybrid' (both)
    scope: str = "local"

    @abstractmethod
    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        """Return None to skip this event (fast path before graph queries)."""

    @abstractmethod
    def detect(self, features: Features) -> Optional[str]:
        """Return a reason string if the rule fires, else None. No side effects."""
