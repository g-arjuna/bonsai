"""BFD anomaly detection rules."""
from __future__ import annotations

from typing import TYPE_CHECKING, Optional

from ..detection import Detector, Features
from ..ml_detector import extract_features_for_event
from ..state_mapping import is_down, is_up

if TYPE_CHECKING:
    from ..client import BonsaiClient


_FIRE_FROM_NONE = "none"  # bootstrap sentinel: bonsai started while session was already down


class BfdSessionDown(Detector):
    """Session transitions from up to down."""
    rule_id = "bfd_session_down"
    severity = "critical"
    fires_on_down = True
    recurrence_indicators = [
        "MATCH (b:BfdSession {device_address: $dev}) RETURN b.peer_address, b.state — expect 'up' when healthy",
        "Check interface oper-status on the BFD-protected link — interface_down co-fire indicates physical cause",
        "Check bgp_session_down DetectionEvents within ±5s of this detection — BFD down typically precedes BGP down",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "bfd_session_change":
            return None
        f = extract_features_for_event(event, client)
        vendor = client.device_vendor(f.device_address)
        new_state = f.new_state.lower()
        old_state = f.old_state.lower()
        if not is_down(vendor, "bfd_oper_state", new_state):
            return None
        if not (is_up(vendor, "bfd_oper_state", old_state) or old_state == _FIRE_FROM_NONE):
            return None
        f.vendor = vendor
        f.old_state = old_state
        f.new_state = new_state
        return f

    def detect(self, features: Features) -> Optional[str]:
        if is_down(features.vendor, "bfd_oper_state", features.new_state):
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
