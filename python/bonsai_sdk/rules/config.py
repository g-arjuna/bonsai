"""Config-state detection rules (D2-4 T3).

These rules fire on `config_change_event` events emitted by the Config-State
Lane (D2-4 T2, Rust). Until that event type is wired in server_startup.rs, these
rules receive no events and produce no detections — they are structurally complete
stubs ready to activate when T2 lands on Ubuntu.

Two rules:
  - ConfigChanged: fires on every config change; severity "info"; audit trail.
  - ConfigCausedFault: fires when a config change is followed by an operational
    fault on the same device within 60 seconds.
"""
from __future__ import annotations

import time
from typing import TYPE_CHECKING, Optional

from ..detection import Detector, Features
from ..window import WindowRegistry

if TYPE_CHECKING:
    from ..client import BonsaiClient

# EVENT_TYPE emitted by D2-4 T2 (src/event_bus.rs). Until that lands, no
# config_change_event will reach these rules.
_CONFIG_EVENT_TYPE = "config_change_event"

# Operational event types that can be caused by a config change.
_OPERATIONAL_FAULT_TYPES = frozenset([
    "bgp_session_change",
    "interface_oper_status_change",
    "bfd_session_change",
    "isis_adjacency_change",
    "ospf_adjacency_change",
])

# Window for config-caused-fault correlation: 60 seconds.
_CORRELATION_WINDOW_NS = 60_000_000_000

# Per-device registry of recent config changes: device_address -> list of
# (yang_path, occurred_at_ns). Entries older than the correlation window are
# pruned on each access.
_recent_config_changes: dict[str, list[tuple[str, int]]] = {}

_CONFIG_FLAP_REGISTRY = WindowRegistry(window_seconds=300)


def _record_config_change(device_address: str, yang_path: str, occurred_at_ns: int) -> None:
    """Store a config change for correlation lookups."""
    cutoff = occurred_at_ns - _CORRELATION_WINDOW_NS
    entries = _recent_config_changes.get(device_address, [])
    entries = [(p, t) for p, t in entries if t >= cutoff]
    entries.append((yang_path, occurred_at_ns))
    _recent_config_changes[device_address] = entries


def _find_recent_config_change(
    device_address: str, before_ns: int
) -> Optional[tuple[str, int]]:
    """Return the most recent config change within the correlation window, if any."""
    cutoff = before_ns - _CORRELATION_WINDOW_NS
    entries = _recent_config_changes.get(device_address, [])
    relevant = [(p, t) for p, t in entries if cutoff <= t <= before_ns]
    if not relevant:
        return None
    return max(relevant, key=lambda x: x[1])


class ConfigChanged(Detector):
    """Fires on every config_change_event.

    Severity 'info' — this is an audit trail rule. Every config change is a
    detection event so it appears in the timeline and can be correlated with
    subsequent operational faults by ConfigCausedFault.
    """
    rule_id = "config_changed"
    severity = "info"
    scope = "hybrid"
    recurrence_indicators = [
        "MATCH (n:ConfigSnapshot {device_address: $dev}) RETURN n.yang_path, n.new_value ORDER BY n.occurred_at_ns DESC LIMIT 10",
        "Check which operator session made the change (audit log at /api/investigations)",
        "Correlate with config_caused_fault detections within 60s on same device",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != _CONFIG_EVENT_TYPE:
            return None
        yang_path = getattr(event, "yang_path", "")
        new_value = getattr(event, "new_value", "")
        occurred_at_ns = getattr(event, "occurred_at_ns", int(time.time() * 1e9))
        _record_config_change(event.device_address, yang_path, occurred_at_ns)
        f = Features(
            device_address=event.device_address,
            event_type=event.event_type,
            detail={
                "yang_path": yang_path,
                "new_value": new_value,
                "previous_value": getattr(event, "previous_value", ""),
            },
            occurred_at_ns=occurred_at_ns,
            state_change_event_id=getattr(event, "state_change_event_id", ""),
        )
        return f

    def detect(self, features: Features) -> Optional[str]:
        yang_path = features.detail.get("yang_path", "unknown")
        new_value = features.detail.get("new_value", "")
        prev_value = features.detail.get("previous_value", "")
        if prev_value:
            return (
                f"Config change on {features.device_address}: "
                f"{yang_path} changed {prev_value!r} -> {new_value!r}"
            )
        return (
            f"Config change on {features.device_address}: "
            f"{yang_path} set to {new_value!r}"
        )


class ConfigCausedFault(Detector):
    """Fires when an operational fault occurs within 60s of a config change on the same device.

    Correlation logic:
    1. Receives an operational event (bgp_session_change, interface_oper_status_change, etc.)
    2. Checks _recent_config_changes for the same device within the last 60s.
    3. If a config change is found, fires with severity 'high' linking the two events.

    This answers the operator's question: "did a config change cause this fault?"
    """
    rule_id = "config_caused_fault"
    severity = "high"
    scope = "hybrid"
    recurrence_indicators = [
        "MATCH (cs:ConfigSnapshot {device_address: $dev}) WHERE cs.occurred_at_ns > $t - 60000000000 RETURN cs.yang_path, cs.new_value",
        "Open investigation to determine who made the config change and why",
        "Check if config_changed detection on same device precedes this fault by <60s",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type not in _OPERATIONAL_FAULT_TYPES:
            return None

        occurred_at_ns = getattr(event, "occurred_at_ns", int(time.time() * 1e9))
        recent = _find_recent_config_change(event.device_address, occurred_at_ns)
        if recent is None:
            return None

        config_path, config_ns = recent
        lag_ms = (occurred_at_ns - config_ns) // 1_000_000

        f = Features(
            device_address=event.device_address,
            event_type=event.event_type,
            detail={
                "fault_event_type": event.event_type,
                "config_yang_path": config_path,
                "config_lag_ms": lag_ms,
            },
            occurred_at_ns=occurred_at_ns,
            state_change_event_id=getattr(event, "state_change_event_id", ""),
        )
        return f

    def detect(self, features: Features) -> Optional[str]:
        fault_type = features.detail.get("fault_event_type", "operational fault")
        config_path = features.detail.get("config_yang_path", "unknown")
        lag_ms = features.detail.get("config_lag_ms", 0)
        return (
            f"Config change on {features.device_address} ({config_path}) "
            f"preceded {fault_type} by {lag_ms}ms — possible config-caused fault"
        )


CONFIG_RULES: list[Detector] = [
    ConfigChanged(),
    ConfigCausedFault(),
]
