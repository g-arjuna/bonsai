"""BGP anomaly detection rules."""
from __future__ import annotations

from typing import TYPE_CHECKING, Optional

from ..detection import Detector, Features
from ..ml_detector import extract_features_for_event
from ..window import WindowRegistry

if TYPE_CHECKING:
    from ..client import BonsaiClient

_FLAP_REGISTRY = WindowRegistry(window_seconds=300)
_FLAP_THRESHOLD = 3  # flaps in 5 min before firing BgpSessionFlap


# Only fire when a session that WAS established drops to idle.
# active->idle is just the BGP retry timer cycling — normal reconnection behavior.
# opensent/openconfirm are establishment steps. Only established->idle is a true loss.
_HARD_DOWN_STATES = {"idle"}
_ESTABLISHED_FROM = {"established"}


class BgpSessionDown(Detector):
    """Session transitions to idle — peer was reset or administratively disabled."""
    rule_id = "bgp_session_down"
    severity = "critical"
    auto_remediate = True
    remediation_action = "bgp_session_bounce"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "bgp_session_change":
            return None
        f = extract_features_for_event(event, client)
        if f.new_state not in _HARD_DOWN_STATES or f.old_state not in _ESTABLISHED_FROM:
            return None
        return f

    def detect(self, features: Features) -> Optional[str]:
        if features.new_state in _HARD_DOWN_STATES:
            return (
                f"BGP peer {features.peer_address} on {features.device_address} "
                f"transitioned {features.old_state} -> {features.new_state} "
                f"({features.peer_count_established}/{features.peer_count_total} peers still up)"
            )
        return None


class BgpSessionFlap(Detector):
    """Session has flapped ≥3 times in 5 minutes — unstable neighbour."""
    rule_id = "bgp_session_flap"
    severity = "critical"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "bgp_session_change":
            return None
        f = extract_features_for_event(event, client)
        # Only count established->idle as a flap - retry cycles don't count.
        if f.new_state not in _HARD_DOWN_STATES or f.old_state not in _ESTABLISHED_FROM:
            return None
        key  = f"{event.device_address}:{f.peer_address}"
        win  = _FLAP_REGISTRY.get(key)
        win.record(event.occurred_at_ns, "bgp_session_change")
        flap_count = win.count()
        if flap_count < _FLAP_THRESHOLD:
            return None
        f.recent_flap_count = flap_count
        return f

    def detect(self, features: Features) -> Optional[str]:
        if features.recent_flap_count >= _FLAP_THRESHOLD:
            return (
                f"BGP peer {features.peer_address} on {features.device_address} "
                f"flapped {features.recent_flap_count} times in 5 minutes"
            )
        return None


class BgpAllPeersDown(Detector):
    """All BGP sessions on a device are gone simultaneously — likely upstream fault."""
    rule_id = "bgp_all_peers_down"
    severity = "critical"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "bgp_session_change":
            return None
        f = extract_features_for_event(event, client)
        if f.peer_count_total == 0 or f.peer_count_established > 0:
            return None
        return f

    def detect(self, features: Features) -> Optional[str]:
        if features.peer_count_total > 0 and features.peer_count_established == 0:
            return (
                f"All {features.peer_count_total} BGP sessions down on "
                f"{features.device_address} — possible upstream or hardware fault"
            )
        return None


class BgpNeverEstablished(Detector):
    """Peer has been seen for >90s without ever reaching established state."""
    rule_id = "bgp_never_established"
    severity = "warn"

    # Track when we first saw each peer
    _first_seen: dict[str, int] = {}
    _TIMEOUT_NS = 90 * 1_000_000_000

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "bgp_session_change":
            return None
        f = extract_features_for_event(event, client)
        key = f"{event.device_address}:{f.peer_address}"

        if f.new_state == "established":
            self._first_seen.pop(key, None)
            return None

        if key not in self._first_seen:
            self._first_seen[key] = event.occurred_at_ns
            return None

        age_ns = event.occurred_at_ns - self._first_seen[key]
        if age_ns < self._TIMEOUT_NS:
            return None

        return f

    def detect(self, features: Features) -> Optional[str]:
        return (
            f"BGP peer {features.peer_address} on {features.device_address} "
            f"has never reached established after 90s (currently {features.new_state})"
        )


BGP_RULES: list[Detector] = [
    BgpSessionDown(),
    BgpSessionFlap(),
    BgpAllPeersDown(),
    BgpNeverEstablished(),
]
