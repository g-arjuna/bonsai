"""Syslog-derived anomaly detection rules."""
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


class SyslogAuthFailureCluster(Detector):
    rule_id = "syslog_auth_failure_cluster"
    severity = "warn"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "syslog_auth":
            return None
        features = extract_features_for_event(event, client)
        win = _AUTH_WINDOW.get(f"auth:{event.device_address}")
        win.record(event.occurred_at_ns, event.event_type)
        count = win.count(event.event_type)
        if count < _AUTH_THRESHOLD:
            return None
        features.recent_flap_count = count
        return features

    def detect(self, features: Features) -> Optional[str]:
        return (
            f"Syslog auth failures reached {features.recent_flap_count} events in 5 minutes "
            f"on {features.device_address}"
        )


class SyslogHardwareError(Detector):
    rule_id = "syslog_hardware_error"
    severity = "critical"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "syslog_hardware":
            return None
        features = extract_features_for_event(event, client)
        text = _message_text(features)
        if not any(token in text for token in ("fail", "error", "alarm", "critical", "down")):
            return None
        return features

    def detect(self, features: Features) -> Optional[str]:
        return f"Hardware syslog on {features.device_address}: {features.detail.get('message', '')}"


class SyslogSoftwareCrash(Detector):
    rule_id = "syslog_software_crash"
    severity = "critical"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "syslog_software":
            return None
        features = extract_features_for_event(event, client)
        text = _message_text(features)
        if not any(token in text for token in ("crash", "panic", "core", "restart", "restarted")):
            return None
        return features

    def detect(self, features: Features) -> Optional[str]:
        return f"Software crash syslog on {features.device_address}: {features.detail.get('message', '')}"


class SyslogLicenseExpiry(Detector):
    rule_id = "syslog_license_expiry"
    severity = "warn"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "syslog_license":
            return None
        features = extract_features_for_event(event, client)
        text = _message_text(features)
        if not any(token in text for token in ("expire", "expired", "expiry", "license")):
            return None
        return features

    def detect(self, features: Features) -> Optional[str]:
        return f"License warning on {features.device_address}: {features.detail.get('message', '')}"


class SyslogProtocolError(Detector):
    rule_id = "syslog_protocol_error"
    severity = "warn"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "syslog_protocol":
            return None
        features = extract_features_for_event(event, client)
        text = _message_text(features)
        if not any(token in text for token in ("down", "flap", "lost", "reset", "mismatch")):
            return None
        return features

    def detect(self, features: Features) -> Optional[str]:
        return f"Protocol syslog on {features.device_address}: {features.detail.get('message', '')}"


class SyslogBpduGuardActivation(Detector):
    rule_id = "syslog_bpduguard_activation"
    severity = "critical"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "syslog_protocol":
            return None
        features = extract_features_for_event(event, client)
        text = _message_text(features)
        if "bpduguard" not in text and "bpdu guard" not in text:
            return None
        return features

    def detect(self, features: Features) -> Optional[str]:
        return f"BPDUGuard syslog on {features.device_address}: {features.detail.get('message', '')}"


class SyslogStpTopologyChange(Detector):
    rule_id = "syslog_stp_topology_change"
    severity = "warn"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "syslog_protocol":
            return None
        features = extract_features_for_event(event, client)
        text = _message_text(features)
        if "topology change" not in text and "stp topology" not in text:
            return None
        return features

    def detect(self, features: Features) -> Optional[str]:
        return f"Spanning-tree topology change on {features.device_address}: {features.detail.get('message', '')}"


SYSLOG_RULES: list[Detector] = [
    SyslogAuthFailureCluster(),
    SyslogHardwareError(),
    SyslogSoftwareCrash(),
    SyslogLicenseExpiry(),
    SyslogProtocolError(),
    SyslogBpduGuardActivation(),
    SyslogStpTopologyChange(),
]
