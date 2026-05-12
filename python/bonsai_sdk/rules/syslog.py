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

_CONFIG_CHANGE_WINDOW = WindowRegistry(window_seconds=600)
_CONFIG_CHANGE_THRESHOLD = 3

# Tracks hardware_error events per device: maps device_address -> list[timestamp_ns]
_HARDWARE_ERROR_WINDOW = WindowRegistry(window_seconds=60)


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


class SyslogBfdDisagreement(Detector):
    """Fires when a syslog bfd_session fact and graph BFD state disagree."""

    rule_id = "syslog_bfd_disagreement"
    severity = "warn"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "syslog_fact_joined":
            return None
        features = extract_features_for_event(event, client)
        if _fact_type(features) != "bfd_session":
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
        key = (
            fields.get("remote_address")
            or fields.get("if_name")
            or "unknown"
        )
        return (
            f"Syslog and graph disagree on BFD state for {key} on "
            f"{features.device_address}: syslog={fields.get('new_state', 'unknown')} "
            f"graph={graph_state.get('session_state', 'unknown')}"
        )


class SyslogConfigChangeCluster(Detector):
    """Fires when ≥3 config_change_detail facts arrive from the same device within 10 minutes."""

    rule_id = "syslog_config_change_cluster"
    severity = "warn"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type not in {"syslog_fact_joined", "syslog_fact_orphan"}:
            return None
        features = extract_features_for_event(event, client)
        if _fact_type(features) != "config_change_detail":
            return None
        win = _CONFIG_CHANGE_WINDOW.get(f"cfg:{event.device_address}")
        win.record(event.occurred_at_ns, "config_change")
        count = win.count("config_change")
        if count < _CONFIG_CHANGE_THRESHOLD:
            return None
        features.recent_flap_count = count
        return features

    def detect(self, features: Features) -> Optional[str]:
        fields = _fact_fields(features)
        username = fields.get("username", "unknown")
        return (
            f"Config change cluster on {features.device_address}: "
            f"{features.recent_flap_count} commits in 10 minutes "
            f"(last committer: {username})"
        )


class SyslogHardwareInterfaceCorrelation(Detector):
    """Fires when an interface_state down fact follows a hardware_error on the same device within 60s."""

    rule_id = "syslog_hardware_interface_correlation"
    severity = "critical"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        features = extract_features_for_event(event, client)
        fact_t = _fact_type(features)

        # Record hardware errors in the window for this device
        if event.event_type == "syslog_hardware":
            win = _HARDWARE_ERROR_WINDOW.get(f"hw:{event.device_address}")
            win.record(event.occurred_at_ns, "hardware_error")
            return None

        # On interface down facts, check if a hardware error preceded this
        if event.event_type not in {"syslog_fact_joined", "syslog_fact_orphan"}:
            return None
        if fact_t != "interface_state":
            return None
        fields = _fact_fields(features)
        if _normalize_state(fields.get("new_state", "")) not in {"down", "administratively down"}:
            return None
        win = _HARDWARE_ERROR_WINDOW.get(f"hw:{event.device_address}")
        hw_count = win.count("hardware_error")
        if hw_count == 0:
            return None
        features.detail["hardware_error_count_in_window"] = hw_count
        return features

    def detect(self, features: Features) -> Optional[str]:
        fields = _fact_fields(features)
        if_name = fields.get("if_name", "unknown-interface")
        hw_count = features.detail.get("hardware_error_count_in_window", 1)
        return (
            f"Interface {if_name} on {features.device_address} went down after "
            f"{hw_count} hardware error(s) in the last 60s — possible PSU/fan fault"
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
    SyslogBfdDisagreement(),
    SyslogConfigChangeCluster(),
    SyslogHardwareInterfaceCorrelation(),
    OrphanInterfaceMention(),
    MultiSourceCorrelation(),
]
