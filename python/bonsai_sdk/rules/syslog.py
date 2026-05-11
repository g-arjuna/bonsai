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


def _fact_type(features: Features) -> str:
    return str(features.detail.get("fact_type", "")).lower()


def _fact_fields(features: Features) -> dict:
    fields = features.detail.get("fields", {})
    return fields if isinstance(fields, dict) else {}


def _join_payload(features: Features) -> dict:
    join = features.detail.get("join", {})
    return join if isinstance(join, dict) else {}


def _join_graph_state(features: Features) -> dict:
    graph_state = _join_payload(features).get("graph_state", {})
    return graph_state if isinstance(graph_state, dict) else {}


def _normalize_state(value: str) -> str:
    normalized = str(value).strip().lower()
    if normalized in {"established", "up"}:
        return normalized
    if normalized in {"down", "idle", "active", "connect"}:
        return normalized
    return normalized


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


class SyslogGnmiDisagreement(Detector):
    rule_id = "syslog_gnmi_disagreement"
    severity = "warn"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "syslog_fact_joined":
            return None
        features = extract_features_for_event(event, client)
        if _fact_type(features) != "bgp_neighbor":
            return None
        fields = _fact_fields(features)
        graph_state = _join_graph_state(features)
        observed = _normalize_state(fields.get("new_state", ""))
        current = _normalize_state(graph_state.get("session_state", ""))
        if not observed or not current or observed == current:
            return None
        return features

    def detect(self, features: Features) -> Optional[str]:
        fields = _fact_fields(features)
        graph_state = _join_graph_state(features)
        peer = fields.get("peer_address", fields.get("peer", "unknown-peer"))
        return (
            f"Syslog and graph disagree on BGP state for {peer} on {features.device_address}: "
            f"syslog={fields.get('new_state', 'unknown')} graph={graph_state.get('session_state', 'unknown')}"
        )


class OrphanInterfaceMention(Detector):
    rule_id = "orphan_interface_mention"
    severity = "warn"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "syslog_fact_orphan":
            return None
        features = extract_features_for_event(event, client)
        if _fact_type(features) != "interface_state":
            return None
        fields = _fact_fields(features)
        if not fields.get("if_name", fields.get("interface", fields.get("interface_name", ""))):
            return None
        return features

    def detect(self, features: Features) -> Optional[str]:
        fields = _fact_fields(features)
        if_name = fields.get("if_name", fields.get("interface", fields.get("interface_name", "unknown-interface")))
        return (
            f"Syslog referenced unmanaged or unresolved interface {if_name} "
            f"on {features.device_address}: {features.detail.get('message', '')}"
        )


class MultiSourceCorrelation(Detector):
    rule_id = "multi_source_correlation"
    severity = "info"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "syslog_fact_joined":
            return None
        features = extract_features_for_event(event, client)
        if _fact_type(features) not in {"bgp_neighbor", "interface_state"}:
            return None
        join = _join_payload(features)
        if join.get("status") != "joined":
            return None
        return features

    def detect(self, features: Features) -> Optional[str]:
        join = _join_payload(features)
        fields = _fact_fields(features)
        fact_type = _fact_type(features)
        if fact_type == "bgp_neighbor":
            peer = fields.get("peer_address", fields.get("peer", "unknown-peer"))
            return (
                f"Multi-source correlation confirmed BGP peer {peer} on {features.device_address} "
                f"via syslog plus graph state"
            )
        if_name = fields.get("if_name", fields.get("interface", fields.get("interface_name", "unknown-interface")))
        return (
            f"Multi-source correlation resolved syslog interface mention {if_name} "
            f"to managed graph state on {features.device_address}"
        )


SYSLOG_RULES: list[Detector] = [
    SyslogAuthFailureCluster(),
    SyslogHardwareError(),
    SyslogSoftwareCrash(),
    SyslogLicenseExpiry(),
    SyslogProtocolError(),
    SyslogBpduGuardActivation(),
    SyslogStpTopologyChange(),
    SyslogGnmiDisagreement(),
    OrphanInterfaceMention(),
    MultiSourceCorrelation(),
]
