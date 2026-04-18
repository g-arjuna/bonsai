"""BGP anomaly detection rules."""
from __future__ import annotations

import json
import time
from typing import TYPE_CHECKING, Optional

from ..detection import Detector, Detection, Features
from ..window import WindowRegistry

if TYPE_CHECKING:
    from ..client import BonsaiClient

_FLAP_REGISTRY = WindowRegistry(window_seconds=300)
_FLAP_THRESHOLD = 3  # flaps in 5 min before firing BgpSessionFlap


class BgpSessionDown(Detector):
    """Session transitions to a non-established, non-active state — peer is down."""
    rule_id = "bgp_session_down"
    severity = "critical"
    auto_remediate = True
    remediation_action = "bgp_session_bounce"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "bgp_session_change":
            return None
        detail = json.loads(event.detail_json or "{}")
        new_state = detail.get("new_state", "")
        if new_state in ("established", "active", ""):
            return None   # not a hard-down state
        f = Features.from_event(event, detail)
        f.peer_address = detail.get("peer", "")
        f.old_state    = detail.get("old_state", "")
        f.new_state    = new_state
        # How many peers are still up on this device?
        try:
            neighbors = client.get_bgp_neighbors(event.device_address)
            f.peer_count_total       = len(neighbors)
            f.peer_count_established = sum(1 for n in neighbors if n.session_state == "established")
        except Exception:
            pass
        return f

    def detect(self, features: Features) -> Optional[str]:
        if features.new_state not in ("established", "active", ""):
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
        detail = json.loads(event.detail_json or "{}")
        peer = detail.get("peer", "")
        key  = f"{event.device_address}:{peer}"
        win  = _FLAP_REGISTRY.get(key)
        win.record(event.occurred_at_ns, "bgp_session_change")
        flap_count = win.count()
        if flap_count < _FLAP_THRESHOLD:
            return None
        f = Features.from_event(event, detail)
        f.peer_address      = peer
        f.old_state         = detail.get("old_state", "")
        f.new_state         = detail.get("new_state", "")
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
        detail = json.loads(event.detail_json or "{}")
        try:
            neighbors = client.get_bgp_neighbors(event.device_address)
        except Exception:
            return None
        total       = len(neighbors)
        established = sum(1 for n in neighbors if n.session_state == "established")
        if total == 0 or established > 0:
            return None
        f = Features.from_event(event, detail)
        f.peer_address           = detail.get("peer", "")
        f.peer_count_total       = total
        f.peer_count_established = 0
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
        detail = json.loads(event.detail_json or "{}")
        peer   = detail.get("peer", "")
        new_state = detail.get("new_state", "")
        key    = f"{event.device_address}:{peer}"

        if new_state == "established":
            self._first_seen.pop(key, None)
            return None

        if key not in self._first_seen:
            self._first_seen[key] = event.occurred_at_ns
            return None

        age_ns = event.occurred_at_ns - self._first_seen[key]
        if age_ns < self._TIMEOUT_NS:
            return None

        f = Features.from_event(event, detail)
        f.peer_address = peer
        f.new_state    = new_state
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
