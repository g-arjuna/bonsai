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

    # Vendor identifier (normalised; populated by extract_features via state_mapping)
    vendor: str = ""

    # Raw timestamp
    occurred_at_ns: int = 0
    # UUID of the primary StateChangeEvent that triggered this detection; empty for poll-based rules
    state_change_event_id: str = ""
    # All StateChangeEvent UUIDs that contributed (multi-source correlation).
    # Populated by the rule engine when a CorrelationBuffer slot is fused.
    # Falls back to [state_change_event_id] when only one source contributed.
    source_event_ids: list = field(default_factory=list)

    # Change Management context — populated when the detection fires during an
    # active change window (ServiceNow CHG, AAP job, manual maintenance).
    change_correlated: bool = False
    change_refs: list = field(default_factory=list)  # [{"id", "number", "source"}]

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
    # True when detection fired during an active change window.
    change_correlated: bool = False

    @property
    def effective_source_event_ids(self) -> list[str]:
        """Return all contributing StateChangeEvent IDs, or the single primary if no multi-source list."""
        ids = self.features.source_event_ids
        if ids:
            return ids
        if self.features.state_change_event_id:
            return [self.features.state_change_event_id]
        return []


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
    # Observable patterns that signal "this is happening again". Used by the
    # agent-friendly grounded response (T5-2) and MCP tool catalogue (T5-1).
    recurrence_indicators: list[str] = []

    @abstractmethod
    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        """Return None to skip this event (fast path before graph queries)."""

    @abstractmethod
    def detect(self, features: Features) -> Optional[str]:
        """Return a reason string if the rule fires, else None. No side effects."""
