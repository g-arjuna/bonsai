"""Host-network correlation detection rules (D2-6 T4).

Rules:
  - HostNetworkFault: fires when a HostEndpoint loses OTLP connectivity
    (no otlp_span_event for >5 min) AND the device it connects to has an
    active interface_down detection.

Dependency:
  Requires D2-6 T1 (HostEndpoint materialization from LLDP) and D2-6 T2
  (OTLP receiver producing otlp_span_event StateChangeEvents). Structurally
  complete — activates automatically when both are live.
"""
from __future__ import annotations

import json
import time
from typing import TYPE_CHECKING, Optional

from ..detection import Detector, Features

if TYPE_CHECKING:
    from ..client import BonsaiClient

_OTLP_SPAN_EVENT = "otlp_span_event"
_IFACE_DOWN_EVENT = "interface_down_event"

# Five-minute silence threshold before declaring host connectivity lost.
_SILENCE_THRESHOLD_NS = 5 * 60 * 1_000_000_000

# host_address → last otlp_span_event timestamp_ns
_last_span_ns: dict[str, int] = {}


class HostNetworkFault(Detector):
    """HostEndpoint has gone OTLP-silent AND its connected device has an active
    interface_down detection.

    This correlates application-layer silence with a physical network fault,
    distinguishing a host-facing failure from a host application crash.
    """
    rule_id = "host_network_fault"
    severity = "warn"
    scope = "hybrid"
    recurrence_indicators = [
        "MATCH (h:HostEndpoint)-[:HOST_CONNECTS_TO]->(d:Device) WHERE h.address = $host RETURN d.address, d.hostname",
        "Check if LLDP neighbor entry for the host is still present on the connected device",
        "Verify OTLP collector is configured and running on the host",
        "Check interface state on the connected device port facing the host",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        occurred_at_ns = getattr(event, "occurred_at_ns", int(time.time() * 1e9))

        if event.event_type == _OTLP_SPAN_EVENT:
            # Keep last-seen table current so we can later check silence.
            try:
                detail = json.loads(getattr(event, "detail_json", None) or "{}")
            except (json.JSONDecodeError, TypeError):
                detail = {}
            host = detail.get("peer_address") or event.device_address
            if host:
                _last_span_ns[host] = occurred_at_ns
            return None

        if event.event_type != _IFACE_DOWN_EVENT:
            return None

        device_address = event.device_address

        # Query for HostEndpoints connected to this device.
        try:
            rows = client.query(
                "MATCH (h:HostEndpoint)-[:HOST_CONNECTS_TO]->(d:Device {address: $dev}) "
                "RETURN h.address, h.hostname",
                {"dev": device_address},
            )
        except Exception:
            return None

        if not rows:
            return None

        silent_hosts = []
        for row in rows:
            host_address = row[0] if isinstance(row, (list, tuple)) else row.get("h.address", "")
            host_hostname = row[1] if isinstance(row, (list, tuple)) else row.get("h.hostname", "")
            last_seen = _last_span_ns.get(host_address, 0)
            silence_ns = occurred_at_ns - last_seen
            if silence_ns >= _SILENCE_THRESHOLD_NS:
                silent_hosts.append({
                    "host_address": host_address,
                    "host_hostname": host_hostname,
                    "silence_minutes": round(silence_ns / 60_000_000_000, 1),
                    "last_seen_ns": last_seen,
                })

        if not silent_hosts:
            return None

        return Features(
            device_address=device_address,
            event_type=event.event_type,
            detail={
                "silent_hosts": silent_hosts,
                "host_count": len(silent_hosts),
                "trigger_event": _IFACE_DOWN_EVENT,
            },
            occurred_at_ns=occurred_at_ns,
            state_change_event_id=getattr(event, "state_change_event_id", ""),
        )

    def detect(self, features: Features) -> Optional[str]:
        count = features.detail.get("host_count", 0)
        hosts = features.detail.get("silent_hosts", [])
        if not hosts:
            return None
        worst = max(hosts, key=lambda h: h.get("silence_minutes", 0))
        suffix = f" (+{count - 1} more)" if count > 1 else ""
        return (
            f"{count} host(s) connected to {features.device_address} "
            f"lost OTLP connectivity — {worst['host_address']} "
            f"silent for {worst['silence_minutes']}min{suffix} "
            f"while interface_down is active on the upstream device"
        )


HOST_RULES: list[Detector] = [
    HostNetworkFault(),
]
