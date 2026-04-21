"""BFD anomaly detection rules."""
from __future__ import annotations

from typing import TYPE_CHECKING, Optional

from ..detection import Detector, Features
from ..ml_detector import extract_features_for_event

if TYPE_CHECKING:
    from ..client import BonsaiClient


_DOWN_STATES = {"down"}
_UP_STATES = {"up"}


class BfdSessionDown(Detector):
    """Session transitions from up to down."""
    rule_id = "bfd_session_down"
    severity = "critical"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "bfd_session_change":
            return None
        f = extract_features_for_event(event, client)
        new_state = f.new_state.lower()
        old_state = f.old_state.lower()
        if new_state not in _DOWN_STATES or old_state not in _UP_STATES:
            return None
        f.old_state = old_state
        f.new_state = new_state
        return f

    def detect(self, features: Features) -> Optional[str]:
        if features.new_state in _DOWN_STATES:
            peer = f" peer {features.peer_address}" if features.peer_address else ""
            iface = f" on {features.if_name}" if features.if_name else ""
            return (
                f"BFD{peer}{iface} on {features.device_address} "
                f"transitioned {features.old_state} -> {features.new_state}"
            )
        return None


BFD_RULES: list[Detector] = [
    BfdSessionDown(),
]
