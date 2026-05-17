"""BGP anomaly detection rules."""
from __future__ import annotations

from typing import TYPE_CHECKING, Optional

from ..detection import Detector, Features
from ..ml_detector import extract_features_for_event
from ..state_mapping import is_down, is_up
from ..window import WindowRegistry

if TYPE_CHECKING:
    from ..client import BonsaiClient

_FLAP_REGISTRY = WindowRegistry(window_seconds=300)
_FLAP_THRESHOLD = 3  # flaps in 5 min before firing BgpSessionFlap


# BgpSessionDown fires only on established->down transitions.
# active/idle cycling during reconnection is normal; only the loss of an
# established session is a true fault. The state_mapping adapter translates
# vendor strings to semantic DOWN/ESTABLISHED via the YAML registry.


class BgpSessionDown(Detector):
    """Session transitions to idle — peer was reset or administratively disabled."""
    rule_id = "bgp_session_down"
    severity = "critical"
    auto_remediate = True
    remediation_action = "bgp_session_bounce"
    recurrence_indicators = [
        "MATCH (n:BgpNeighbor {device_address: $dev, peer_address: $peer}) RETURN n.session_state — expect 'established' when healthy",
        "Count bgp_session_down DetectionEvents for this device/peer pair in last 24h",
        "Check gNMI subscription status for openconfig-bgp on this device (/api/devices/{address})",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "bgp_session_change":
            return None
        f = extract_features_for_event(event, client)
        vendor = client.device_vendor(f.device_address)
        if not is_down(vendor, "bgp_session_state", f.new_state):
            return None
        if not is_up(vendor, "bgp_session_state", f.old_state):
            return None
        f.vendor = vendor
        return f

    def detect(self, features: Features) -> Optional[str]:
        if is_down(features.vendor, "bgp_session_state", features.new_state):
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
    recurrence_indicators = [
        "Count bgp_session_flap DetectionEvents for this device/peer pair in last 1h — ≥2 incidents indicates chronic instability",
        "Check for bfd_session_down co-firing within ±5s (indicates routing-layer cause, not BGP policy)",
        "Compare peer_count_established across last 3 bgp_session_flap detections for this device",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "bgp_session_change":
            return None
        f = extract_features_for_event(event, client)
        vendor = client.device_vendor(f.device_address)
        # Only count established->down transitions as flaps; retry cycles don't count.
        if not is_down(vendor, "bgp_session_state", f.new_state) or not is_up(vendor, "bgp_session_state", f.old_state):
            return None
        f.vendor = vendor
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
    recurrence_indicators = [
        "MATCH (n:BgpNeighbor {device_address: $dev}) RETURN n.peer_address, n.session_state — all should be 'established' when healthy",
        "Check for interface_down DetectionEvents on same device within ±30s (hardware-fault co-indicator)",
        "Check blast radius (/api/blast-radius/{address}) — bgp_all_peers_down typically has wide downstream impact",
    ]

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
    recurrence_indicators = [
        "Verify path between device and peer exists: GET /api/path?src={device}&dst={peer}",
        "Check BFD session state for this peer — bfd_session_down co-fire means underlay reachability issue",
        "Check DetectionEvent history: if bgp_never_established fires repeatedly, peer config is likely misconfigured",
    ]

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
