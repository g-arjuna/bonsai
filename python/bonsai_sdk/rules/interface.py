"""Interface anomaly detection rules."""
from __future__ import annotations

import json
import time
from typing import TYPE_CHECKING, Optional

from ..detection import Detector, Features

if TYPE_CHECKING:
    from ..client import BonsaiClient

# Thresholds
ERROR_RATE_THRESHOLD = 100   # errors/s
UTIL_THRESHOLD_PCT   = 80    # octets utilisation %

# Track previous counter snapshot for rate calculation: key → (timestamp_ns, errors)
_prev_errors: dict[str, tuple[int, int]] = {}
# Track previous octets snapshot: key → (timestamp_ns, in_octets, out_octets)
_prev_octets: dict[str, tuple[int, int, int]] = {}


class InterfaceDown(Detector):
    """Interface oper-status transitions to down."""
    rule_id = "interface_down"
    severity = "critical"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "interface_oper_status_change":
            return None
        detail = json.loads(event.detail_json or "{}")
        status = detail.get("oper_status", "").lower()
        if status not in ("down", "lower-layer-down"):
            return None
        f = Features.from_event(event, detail)
        f.if_name    = detail.get("if_name", "")
        f.oper_status = status
        return f

    def detect(self, features: Features) -> Optional[str]:
        return (
            f"Interface {features.if_name} on {features.device_address} "
            f"is operationally {features.oper_status}"
        )


class InterfaceErrorSpike(Detector):
    """Error counter rate exceeds threshold."""
    rule_id = "interface_error_spike"
    severity = "warn"

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "bgp_session_change":
            return None   # fired from counter polling, not events — see engine poll loop
        return None

    def detect(self, features: Features) -> Optional[str]:
        return None  # evaluated by the poll-based branch, not here

    @staticmethod
    def evaluate_counters(device_address: str, if_name: str, in_errors: int, out_errors: int, ts_ns: int) -> Optional[str]:
        """Called by the engine poll loop; returns a reason string or None."""
        key = f"{device_address}:{if_name}"
        total = in_errors + out_errors
        if key in _prev_errors:
            prev_ts, prev_total = _prev_errors[key]
            elapsed_s = (ts_ns - prev_ts) / 1e9
            if elapsed_s > 0:
                rate = (total - prev_total) / elapsed_s
                if rate > ERROR_RATE_THRESHOLD:
                    _prev_errors[key] = (ts_ns, total)
                    return (
                        f"Interface {if_name} on {device_address}: "
                        f"error rate {rate:.0f}/s exceeds threshold {ERROR_RATE_THRESHOLD}/s"
                    )
        _prev_errors[key] = (ts_ns, total)
        return None


class InterfaceHighUtilization(Detector):
    """Octets rate exceeds 80% of known link capacity — placeholder threshold check."""
    rule_id = "interface_high_utilization"
    severity = "warn"
    # Phase 4 uses a fixed 1 Gbps assumption for lab links.
    LINK_CAPACITY_BPS = 1_000_000_000

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        return None  # poll-based, not event-driven

    def detect(self, features: Features) -> Optional[str]:
        return None

    @staticmethod
    def evaluate_counters(device_address: str, if_name: str, in_octets: int, out_octets: int, ts_ns: int) -> Optional[str]:
        key = f"{device_address}:{if_name}"
        if key in _prev_octets:
            prev_ts, prev_in, prev_out = _prev_octets[key]
            elapsed_s = (ts_ns - prev_ts) / 1e9
            if elapsed_s > 0:
                in_bps  = (in_octets  - prev_in)  * 8 / elapsed_s
                out_bps = (out_octets - prev_out) * 8 / elapsed_s
                max_bps = max(in_bps, out_bps)
                pct     = max_bps / InterfaceHighUtilization.LINK_CAPACITY_BPS * 100
                if pct > UTIL_THRESHOLD_PCT:
                    _prev_octets[key] = (ts_ns, in_octets, out_octets)
                    return (
                        f"Interface {if_name} on {device_address}: "
                        f"utilisation {pct:.0f}% exceeds threshold {UTIL_THRESHOLD_PCT}%"
                    )
        _prev_octets[key] = (ts_ns, in_octets, out_octets)
        return None


INTERFACE_RULES: list[Detector] = [
    InterfaceDown(),
    InterfaceErrorSpike(),
    InterfaceHighUtilization(),
]
