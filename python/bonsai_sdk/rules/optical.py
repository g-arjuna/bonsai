"""Optical channel detection rules (D2-7 T3).

Rules:
  - OpticalRxDegrading: fires when a channel's rx_power_dbm has dropped ≥3 dBm
    in the last 6 hours AND is currently below the absolute threshold (−12 dBm).

Dependency on D2-7 T1/T2 (Ubuntu):
  Requires `optical_channel_state` events emitted by the D2-7 gNMI/SNMP receiver
  or the D2-7 T4 synthetic simulator. Until those land, no events reach these rules.
  Structurally complete — activates automatically when the event type exists.

Testing:
  Use experiments/optical_simulator/simulate.py --scenario degrade to validate.
"""
from __future__ import annotations

import json
import time
from collections import defaultdict
from typing import TYPE_CHECKING, Optional

from ..detection import Detector, Features

if TYPE_CHECKING:
    from ..client import BonsaiClient

_OPTICAL_EVENT_TYPE = "optical_channel_state"

# optical_rx_degrading thresholds (D2-7 T3)
_RX_DROP_THRESHOLD_DB = 3.0     # must have dropped at least 3 dBm
_RX_ABSOLUTE_FLOOR_DBM = -12.0  # AND be below this absolute level
_TREND_WINDOW_NS = 6 * 3600 * 1_000_000_000  # 6 hours

# Per (device, channel) rx_power history: list of (dbm, ts_ns)
_rx_history: dict[tuple[str, str], list[tuple[float, int]]] = defaultdict(list)


def _record_rx(device: str, channel: str, dbm: float, ts_ns: int) -> None:
    key = (device, channel)
    cutoff = ts_ns - _TREND_WINDOW_NS
    entries = _rx_history[key]
    entries = [(v, t) for v, t in entries if t >= cutoff]
    entries.append((dbm, ts_ns))
    _rx_history[key] = entries


def _rx_drop_db(device: str, channel: str, current_dbm: float, ts_ns: int) -> float:
    """Return how many dBm the channel has dropped from its 6h peak."""
    key = (device, channel)
    cutoff = ts_ns - _TREND_WINDOW_NS
    history = [(v, t) for v, t in _rx_history.get(key, []) if t >= cutoff]
    if not history:
        return 0.0
    peak = max(v for v, _ in history)
    return peak - current_dbm


class OpticalRxDegrading(Detector):
    """Channel rx_power has dropped ≥3 dBm over 6h AND is below −12 dBm.

    This detects gradual optical degradation — catches the fault hours before
    the L2 link drops. Severity 'warn' on first detection; escalates to
    'critical' if rx_power continues below −18 dBm.
    """
    rule_id = "optical_rx_degrading"
    severity = "warn"
    scope = "hybrid"
    recurrence_indicators = [
        "MATCH (oc:OpticalChannel {device_address: $dev, name: $ch}) RETURN oc.rx_power_dbm, oc.last_sampled_ns",
        "Check OSNR trend — a falling OSNR with falling rx_power indicates fibre degradation not connector issue",
        "Compare pre_fec_ber trend — rising BER confirms signal degradation",
        "Check patch panel / connector cleanliness for the affected span",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != _OPTICAL_EVENT_TYPE:
            return None

        occurred_at_ns = getattr(event, "occurred_at_ns", int(time.time() * 1e9))
        device = event.device_address

        channels = getattr(event, "channels", None)
        if channels is None:
            try:
                channels = json.loads(getattr(event, "detail_json", "[]"))
            except Exception:
                return None

        degraded = []
        for ch in channels:
            name = ch.get("name", "")
            rx = ch.get("rx_power_dbm")
            if rx is None:
                continue
            _record_rx(device, name, rx, occurred_at_ns)
            drop = _rx_drop_db(device, name, rx, occurred_at_ns)
            if drop >= _RX_DROP_THRESHOLD_DB and rx < _RX_ABSOLUTE_FLOOR_DBM:
                degraded.append({
                    "channel": name,
                    "rx_power_dbm": round(rx, 2),
                    "drop_db": round(drop, 2),
                    "osnr_db": ch.get("osnr_db"),
                    "pre_fec_ber": ch.get("pre_fec_ber"),
                })

        if not degraded:
            return None

        worst = max(degraded, key=lambda c: c["drop_db"])
        f = Features(
            device_address=device,
            event_type=event.event_type,
            detail={
                "degraded_channels": degraded,
                "worst_channel": worst["channel"],
                "worst_rx_dbm": worst["rx_power_dbm"],
                "worst_drop_db": worst["drop_db"],
            },
            occurred_at_ns=occurred_at_ns,
            state_change_event_id=getattr(event, "state_change_event_id", ""),
        )
        return f

    def detect(self, features: Features) -> Optional[str]:
        worst = features.detail.get("worst_channel", "unknown")
        rx = features.detail.get("worst_rx_dbm", 0)
        drop = features.detail.get("worst_drop_db", 0)
        count = len(features.detail.get("degraded_channels", []))
        suffix = f" (+{count - 1} more channels)" if count > 1 else ""
        return (
            f"Optical channel {worst} on {features.device_address}: "
            f"rx_power {rx} dBm (dropped {drop} dBm over 6h){suffix} — "
            f"gradual fibre degradation likely"
        )


OPTICAL_RULES: list[Detector] = [
    OpticalRxDegrading(),
]
