"""SNMP-trap-derived anomaly detection rules."""
from __future__ import annotations

from typing import TYPE_CHECKING, Optional

from ..detection import Detector, Features
from ..ml_detector import extract_features_for_event
from ..window import WindowRegistry

if TYPE_CHECKING:
    from ..client import BonsaiClient

_AUTH_WINDOW = WindowRegistry(window_seconds=300)
_AUTH_THRESHOLD = 3


def _message_text(features: Features) -> str:
    return str(features.detail.get("message", "")).lower()


class SnmpColdWarmStart(Detector):
    rule_id = "snmp_cold_warm_start"
    severity = "warn"
    recurrence_indicators = [
        "Check StateChangeEvent count for this device in last 5 min — rapid restarts indicate instability",
        "Verify gNMI subscription reconnected after restart: GET /api/devices/{address} subscription_statuses",
        "Check YANG capabilities repopulated after restart: GET /api/yang/modules?device={address}",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type not in {"snmp_cold_start", "snmp_warm_start"}:
            return None
        return extract_features_for_event(event, client)

    def detect(self, features: Features) -> Optional[str]:
        kind = features.event_type.removeprefix("snmp_").replace("_", " ")
        return f"SNMP {kind} trap on {features.device_address}"


class SnmpAuthFailureBurst(Detector):
    rule_id = "snmp_auth_failure_burst"
    severity = "critical"
    recurrence_indicators = [
        "Count snmp_auth_failure_burst DetectionEvents per source IP in last 24h — cross-device pattern indicates scanning",
        "Check if same source IP appears in auth-failure events across multiple devices (lateral movement indicator)",
        "Review SNMPv3 credential rotation schedule — burst after credential change indicates stale client config",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "snmp_auth_failure":
            return None
        features = extract_features_for_event(event, client)
        win = _AUTH_WINDOW.get(f"snmp-auth:{event.device_address}")
        win.record(event.occurred_at_ns, event.event_type)
        count = win.count(event.event_type)
        if count < _AUTH_THRESHOLD:
            return None
        features.recent_flap_count = count
        return features

    def detect(self, features: Features) -> Optional[str]:
        return (
            f"SNMP authentication failures reached {features.recent_flap_count} traps in 5 minutes "
            f"on {features.device_address}"
        )


class SnmpEnvironmentalThresholdBreach(Detector):
    rule_id = "snmp_environmental_threshold_breach"
    severity = "critical"
    recurrence_indicators = [
        "Check platform health via gNMI openconfig-platform path on this device",
        "Check for interface_down or bgp_session_down co-firing within ±5min of this detection (thermal impact cascade)",
        "Count snmp_environmental_threshold_breach events for this device in last 24h — repeated events indicate cooling failure",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "snmp_environmental":
            return None
        features = extract_features_for_event(event, client)
        text = _message_text(features)
        if not any(token in text for token in ("psu", "power", "temperature", "thermal", "fan", "voltage")):
            return None
        return features

    def detect(self, features: Features) -> Optional[str]:
        return f"Environmental SNMP trap on {features.device_address}: {features.detail.get('message', '')}"


class SnmpFruFailure(Detector):
    rule_id = "snmp_fru_failure"
    severity = "critical"
    recurrence_indicators = [
        "MATCH (c:Component {device_address: $dev}) RETURN c.name, c.state — check platform component inventory via gNMI",
        "Check for interface_down on interfaces served by the failed FRU within ±30s",
        "Check bgp_session_down on sessions using adjacencies on the affected linecard",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "snmp_fru_failure":
            return None
        features = extract_features_for_event(event, client)
        text = _message_text(features)
        if not any(token in text for token in ("fru", "linecard", "line card", "module", "fabric", "chassis")):
            return None
        return features

    def detect(self, features: Features) -> Optional[str]:
        return f"FRU SNMP trap on {features.device_address}: {features.detail.get('message', '')}"


SNMP_RULES: list[Detector] = [
    SnmpColdWarmStart(),
    SnmpAuthFailureBurst(),
    SnmpEnvironmentalThresholdBreach(),
    SnmpFruFailure(),
]
