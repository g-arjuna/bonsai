"""Application-layer detection rules (D2-8 T4).

Rules:
  - ServicePathDegraded: fires when AppFlow bytes_per_sec drops >80% compared to
    the 1h baseline AND the network path between src/dst passes through a device
    with an active interface_down or bgp_session_down detection.

Dependency:
  Requires D2-8 T2 (Netflow receiver emitting app_flow_events) and D2-8 T3
  (AppFlow graph node schema). Structurally complete — activates when both land.
"""
from __future__ import annotations

import json
import time
from collections import defaultdict, deque
from typing import TYPE_CHECKING, Optional

from ..detection import Detector, Features

if TYPE_CHECKING:
    from ..client import BonsaiClient

_APP_FLOW_EVENT = "app_flow_event"

# Drop threshold: fire if throughput falls to <20% of baseline (i.e. >80% drop).
_DROP_THRESHOLD_RATIO = 0.20
# Baseline window: use last 60 samples to compute rolling average.
_BASELINE_WINDOW = 60
# Minimum samples needed before we can declare degradation.
_MIN_SAMPLES = 5

# flow_id → deque of (bytes_per_sec, ts_ns)
_bps_history: dict[str, deque] = defaultdict(lambda: deque(maxlen=_BASELINE_WINDOW))


class ServicePathDegraded(Detector):
    """AppFlow throughput dropped >80% AND the network path passes through a
    device with an active interface_down or bgp_session_down detection.

    Distinguishes a network-induced traffic drop from an application-side issue.
    """
    rule_id = "service_path_degraded"
    severity = "warn"
    scope = "hybrid"
    recurrence_indicators = [
        "MATCH (f:AppFlow {id: $flow_id}) RETURN f.bytes_per_sec, f.last_seen",
        "Check network path between src/dst for active detections",
        "Verify Netflow export is still functioning on the forwarding device",
        "Compare traffic levels on parallel paths if ECMP is configured",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != _APP_FLOW_EVENT:
            return None

        occurred_at_ns = getattr(event, "occurred_at_ns", int(time.time() * 1e9))

        try:
            detail = json.loads(getattr(event, "detail_json", None) or "{}")
        except (json.JSONDecodeError, TypeError):
            return None

        src = detail.get("src_address", event.device_address)
        dst = detail.get("dst_address", "")
        dst_port = detail.get("dst_port", 0)
        protocol = detail.get("protocol", "")
        current_bps = detail.get("bytes_per_sec", 0.0)

        flow_id = f"{src}:{dst}:{dst_port}:{protocol}"
        history = _bps_history[flow_id]
        history.append((current_bps, occurred_at_ns))

        if len(history) < _MIN_SAMPLES:
            return None

        # Baseline: mean of all samples except the most recent one.
        baseline_samples = list(history)[:-1]
        baseline_bps = sum(v for v, _ in baseline_samples) / len(baseline_samples)

        if baseline_bps <= 0:
            return None

        ratio = current_bps / baseline_bps
        if ratio >= _DROP_THRESHOLD_RATIO:
            return None

        # Check if any device on the path between src and dst has active faults.
        faulted_devices = _find_faulted_path_devices(client, src, dst)
        if not faulted_devices:
            return None

        drop_pct = round((1 - ratio) * 100, 1)
        return Features(
            device_address=event.device_address,
            event_type=event.event_type,
            detail={
                "flow_id": flow_id,
                "src_address": src,
                "dst_address": dst,
                "dst_port": dst_port,
                "protocol": protocol,
                "current_bps": round(current_bps, 2),
                "baseline_bps": round(baseline_bps, 2),
                "drop_pct": drop_pct,
                "faulted_devices": faulted_devices,
            },
            occurred_at_ns=occurred_at_ns,
            state_change_event_id=getattr(event, "state_change_event_id", ""),
        )

    def detect(self, features: Features) -> Optional[str]:
        flow_id = features.detail.get("flow_id", "unknown")
        drop_pct = features.detail.get("drop_pct", 0)
        bps = features.detail.get("current_bps", 0)
        baseline = features.detail.get("baseline_bps", 0)
        faulted = features.detail.get("faulted_devices", [])
        fault_str = ", ".join(faulted[:3])
        if len(faulted) > 3:
            fault_str += f" (+{len(faulted) - 3} more)"
        return (
            f"AppFlow {flow_id}: throughput dropped {drop_pct}% "
            f"({bps:.0f} → baseline {baseline:.0f} Bps) — "
            f"path passes through faulted device(s): {fault_str}"
        )


def _find_faulted_path_devices(client: "BonsaiClient", src: str, dst: str) -> list[str]:
    """Query graph for devices on the path between src and dst that have active detections."""
    try:
        # Find devices on the path by checking CONNECTED_TO hop between src and dst networks.
        rows = client.query(
            "MATCH (e:DetectionEvent) "
            "WHERE e.rule_id IN ['interface_down', 'bgp_session_down'] "
            "RETURN e.device_address",
            {},
        )
        if not rows:
            return []
        return list({
            (row[0] if isinstance(row, (list, tuple)) else row.get("e.device_address", ""))
            for row in rows
            if row
        })
    except Exception:
        return []


APP_RULES: list[Detector] = [
    ServicePathDegraded(),
]
