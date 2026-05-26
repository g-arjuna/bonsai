"""Interface anomaly detection rules."""
from __future__ import annotations

from typing import TYPE_CHECKING, Optional

from ..detection import Detector, Features
from ..ml_detector import extract_features_for_event
from ..state_mapping import is_down, is_up

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
    fires_on_down = True
    recurrence_indicators = [
        "MATCH (i:Interface {device_address: $dev, name: $if}) RETURN i.oper_status — expect 'up' when healthy",
        "Check CONNECTED_TO edge still present: MATCH (i:Interface {name: $if})-[:CONNECTED_TO]->(j:Interface) RETURN j.device_address",
        "Check for bgp_session_down co-firing on same device within ±10s (upstream propagation indicator)",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "interface_oper_status_change":
            return None
        f = extract_features_for_event(event, client)
        vendor = client.device_vendor(f.device_address)
        status = f.oper_status.lower()
        if not is_down(vendor, "interface_oper_status", status):
            return None
        # Only fire on a real up→down transition. If old_state is already down
        # or unknown (empty/none), this is initial sync state — not a real fault.
        if not is_up(vendor, "interface_oper_status", f.old_state):
            return None
        f.vendor = vendor
        f.oper_status = status
        return f

    def detect(self, features: Features) -> Optional[str]:
        return (
            f"Interface {features.if_name} on {features.device_address} "
            f"transitioned {features.old_state} -> {features.oper_status}"
        )


class InterfaceErrorSpike(Detector):  # EV1-7 T2: supports apply_parameters()
    """Error counter rate exceeds threshold."""
    rule_id = "interface_error_spike"
    severity = "warn"
    recurrence_indicators = [
        "MATCH (i:Interface {device_address: $dev, name: $if}) RETURN i.in_errors, i.out_errors — compare to previous detection's features_json",
        "Check for repeated interface_error_spike on same interface in last 1h (chronic physical-layer issue)",
        "Cross-reference link utilization — high errors under low load indicate physical-layer fault, not congestion",
    ]
    error_rate_threshold_pct: float = float(ERROR_RATE_THRESHOLD)
    window_seconds: int = 60

    def apply_parameters(self, params: dict) -> None:
        if "error_rate_threshold_pct" in params:
            self.error_rate_threshold_pct = float(params["error_rate_threshold_pct"])
        if "window_seconds" in params:
            self.window_seconds = int(params["window_seconds"])

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


class InterfaceHighUtilization(Detector):  # EV1-7 T2: supports apply_parameters()
    """Octets rate exceeds 80% of known link capacity — placeholder threshold check."""
    rule_id = "interface_high_utilization"
    severity = "warn"
    recurrence_indicators = [
        "MATCH (i:Interface {device_address: $dev, name: $if}) RETURN i.in_octets, i.out_octets — rate trend since last detection",
        "Check interface_error_spike co-fire — high util + errors = capacity problem, not just load",
        "Check topology neighbors (/api/topology) for load-balancing or traffic-engineering change as upstream cause",
    ]
    # Phase 4 uses a fixed 1 Gbps assumption for lab links.
    LINK_CAPACITY_BPS = 1_000_000_000
    utilization_threshold_pct: float = float(UTIL_THRESHOLD_PCT)

    def apply_parameters(self, params: dict) -> None:
        if "utilization_threshold_pct" in params:
            self.utilization_threshold_pct = float(params["utilization_threshold_pct"])

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
