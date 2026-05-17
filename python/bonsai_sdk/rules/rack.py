"""Rack-level substrate detection rules (D2-5 T2).

Rules:
  - RackIsolated: fires when ≥50% of devices in a rack lose subscription within
    a 60-second window. Indicates a power or fabric failure affecting the whole
    rack — the "power went out in rack 5" detection.

Dependency on D2-5 T1 (Ubuntu):
  These rules fire on `subscription_lost` events. Until the `Rack` graph node and
  `rack_member` edges are materialised by the NetBox enricher (D2-5 T1), the
  `devices_in_rack()` client call returns [] and these rules produce no detections.
  The rules are structurally complete and activate automatically when T1 lands.
"""
from __future__ import annotations

import time
from collections import defaultdict
from typing import TYPE_CHECKING, Optional

from ..detection import Detector, Features

if TYPE_CHECKING:
    from ..client import BonsaiClient

_SUBSCRIPTION_LOST_EVENT = "subscription_lost"

# Window within which multiple subscription_lost events on the same rack are
# considered a correlated outage.
_RACK_WINDOW_NS = 60_000_000_000  # 60 seconds

# Threshold: fraction of rack devices that must lose subscription to fire.
_RACK_ISOLATION_THRESHOLD = 0.50  # 50%

# Minimum rack population to avoid false positives on single-device racks.
_MIN_RACK_POPULATION = 2

# Per-rack sliding window: rack -> list of (device_address, occurred_at_ns)
_rack_loss_window: dict[str, list[tuple[str, int]]] = defaultdict(list)


def _record_loss(rack: str, device_address: str, occurred_at_ns: int) -> None:
    cutoff = occurred_at_ns - _RACK_WINDOW_NS
    entries = _rack_loss_window[rack]
    entries = [(d, t) for d, t in entries if t >= cutoff]
    if not any(d == device_address for d, _ in entries):
        entries.append((device_address, occurred_at_ns))
    _rack_loss_window[rack] = entries


def _lost_in_window(rack: str, occurred_at_ns: int) -> list[str]:
    cutoff = occurred_at_ns - _RACK_WINDOW_NS
    return [d for d, t in _rack_loss_window.get(rack, []) if t >= cutoff]


class RackIsolated(Detector):
    """≥50% of devices in a rack lose gNMI subscription within 60 seconds.

    Severity 'critical' — a whole-rack outage is typically a power or ToR
    switch failure and affects all hosts in the rack.

    Requires D2-5 T1 (NetBox rack_member edges) for `devices_in_rack()` to
    return non-empty results.
    """
    rule_id = "rack_isolated"
    severity = "critical"
    scope = "hybrid"
    recurrence_indicators = [
        "MATCH (r:Rack {name: $rack})<-[:rack_member]-(d:Device) RETURN d.address, d.oper_status",
        "Check PDU power feed state for the rack (D2-5 T3 — SNMP PDU polling)",
        "Count subscription_lost events for rack devices in last 5m vs total rack population",
        "Check ToR switch for interface-down detections coinciding with the outage window",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != _SUBSCRIPTION_LOST_EVENT:
            return None

        occurred_at_ns = getattr(event, "occurred_at_ns", int(time.time() * 1e9))

        rack = client.device_rack(event.device_address)
        if not rack:
            return None

        _record_loss(rack, event.device_address, occurred_at_ns)

        all_rack_devices = client.devices_in_rack(rack)
        rack_population = len(all_rack_devices)
        if rack_population < _MIN_RACK_POPULATION:
            return None

        lost_devices = _lost_in_window(rack, occurred_at_ns)
        lost_fraction = len(lost_devices) / rack_population

        if lost_fraction < _RACK_ISOLATION_THRESHOLD:
            return None

        f = Features(
            device_address=event.device_address,
            event_type=event.event_type,
            detail={
                "rack": rack,
                "lost_devices": lost_devices,
                "lost_count": len(lost_devices),
                "rack_population": rack_population,
                "lost_fraction_pct": round(lost_fraction * 100, 1),
            },
            occurred_at_ns=occurred_at_ns,
            state_change_event_id=getattr(event, "state_change_event_id", ""),
        )
        return f

    def detect(self, features: Features) -> Optional[str]:
        rack = features.detail.get("rack", "unknown")
        lost = features.detail.get("lost_count", 0)
        total = features.detail.get("rack_population", 0)
        pct = features.detail.get("lost_fraction_pct", 0)
        lost_list = ", ".join(features.detail.get("lost_devices", [])[:5])
        suffix = "..." if features.detail.get("lost_count", 0) > 5 else ""
        return (
            f"Rack {rack}: {lost}/{total} devices ({pct}%) lost subscription in 60s "
            f"— possible rack isolation. Affected: {lost_list}{suffix}"
        )


RACK_RULES: list[Detector] = [
    RackIsolated(),
]
