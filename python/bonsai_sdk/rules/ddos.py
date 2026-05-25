"""
DS-3 T2/T3 — DDoS Detection Rules (data-plane focused)

Two rule classes:
  - DdosCorroborationRule: multi-source evidence accumulator that upgrades
    suspect → confirmed when corroboration_threshold sources agree within
    the corroboration window.
  - DdosVectorsRule: per-vector pattern matchers for SYN flood, DNS/NTP/SSDP
    amplification, ICMP flood, UDP fragment flood, and asymmetric flow duration.

Control-plane items (CoPP violations, LPTS exhaustion) are intentionally
excluded — the focus is on data-plane attack patterns per DS backlog scope.

These rules are off by default. They must be explicitly loaded by the collector
engine when ddos.enabled = true in bonsai.toml.
"""

from __future__ import annotations

import time
from collections import defaultdict
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Optional

from ..detection import Detector, Features

if TYPE_CHECKING:
    from ..client import BonsaiClient

# ── Evidence weight table ─────────────────────────────────────────────────────
# Weight of each source type for the confidence score (0.0–1.0 sum).
# Multiple flows from the same source type do NOT stack — only the presence of
# the source type counts towards corroboration.
_SOURCE_WEIGHTS: dict[str, float] = {
    "netflow":    0.30,
    "sflow":      0.30,
    "snmp":       0.20,
    "syslog":     0.15,
    "gnmi":       0.15,
    "bmp":        0.10,
    "otlp":       0.05,
}


@dataclass
class _EvidenceSlot:
    sources: set[str] = field(default_factory=set)
    first_seen: float = field(default_factory=time.time)
    last_seen: float = field(default_factory=time.time)
    detail: dict[str, Any] = field(default_factory=dict)
    max_pps: float = 0.0
    max_gbps: float = 0.0
    vectors: set[str] = field(default_factory=set)

    def confidence(self) -> float:
        score = sum(_SOURCE_WEIGHTS.get(s, 0.05) for s in self.sources)
        return min(1.0, score)


class DdosCorroborationRule(Detector):
    """
    DS-3 T2: Multi-source evidence corroboration — upgrades ddos_suspect to
    ddos_confirmed when confidence >= confidence_floor within the corroboration
    window.

    Input event_type consumed: ddos_interface_pps_spike (Rust-side, netflow/sflow).
    Additional corroboration arrives when the same device fires from snmp, syslog,
    or gnmi sources within the window.

    Output reason strings:
      - "ddos_suspect"    when single-source evidence arrives
      - "ddos_confirmed"  when corroboration_threshold sources agree
    """

    rule_id = "ddos_corroboration"
    severity = "high"
    scope = "local"

    CONFIDENCE_FLOOR: float = 0.50
    CORROBORATION_WINDOW: float = 60.0
    COOLDOWN: float = 300.0

    def __init__(self) -> None:
        self._evidence: dict[str, _EvidenceSlot] = {}
        self._last_confirmed: dict[str, float] = {}

    def extract_features(self, event: Any, client: Any) -> Optional[Features]:
        if event.event_type != "ddos_interface_pps_spike":
            return None
        import json as _json
        try:
            detail = _json.loads(event.detail_json) if isinstance(event.detail_json, str) else {}
        except Exception:
            detail = {}
        return Features.from_event(event, detail)

    def detect(self, features: Features) -> Optional[str]:
        now = time.time()
        source_type = features.detail.get("source_type", "netflow")
        pps = float(features.detail.get("observed_pps", 0.0))
        vector = features.detail.get("amplification_vector", "")
        key = f"{features.device_address}:{vector}"

        slot = self._evidence.get(key)
        if slot is None or (now - slot.first_seen > self.CORROBORATION_WINDOW):
            slot = _EvidenceSlot()
            self._evidence[key] = slot

        slot.sources.add(source_type)
        slot.last_seen = now
        slot.max_pps = max(slot.max_pps, pps)
        if vector:
            slot.vectors.add(vector)
        slot.detail.update(features.detail)

        confidence = slot.confidence()

        if confidence >= self.CONFIDENCE_FLOOR:
            last_conf = self._last_confirmed.get(key, 0.0)
            if now - last_conf < self.COOLDOWN:
                return None
            self._last_confirmed[key] = now
            return (
                f"ddos_confirmed: {len(slot.sources)} sources corroborate "
                f"pps_spike={pps:.0f} vector={vector or 'unknown'} "
                f"confidence={confidence:.2f}"
            )

        if len(slot.sources) == 1:
            return (
                f"ddos_suspect: single-source ({source_type}) pps_spike={pps:.0f} "
                f"vector={vector or 'unknown'} confidence={confidence:.2f}"
            )
        return None

    def evict_stale(self) -> None:
        now = time.time()
        stale = [k for k, v in self._evidence.items() if now - v.last_seen > self.CORROBORATION_WINDOW * 2]
        for k in stale:
            del self._evidence[k]


class DdosVectorsRule(Detector):
    """
    DS-3 T3: Data-plane attack vector classifier.

    Fires on app_flow_event events and classifies the vector (syn_flood,
    dns_amplification, icmp_flood, udp_fragment_flood, asymmetric_low_and_slow).
    All patterns are data-plane only — no control-plane (CoPP/LPTS) involvement.
    """

    rule_id = "ddos_vectors"
    severity = "high"
    scope = "local"

    SYN_FLOOD_PPS_THRESHOLD = 50_000
    AMPLIFICATION_BPP_THRESHOLD = 512
    ICMP_FLOOD_PPS_THRESHOLD = 10_000
    UDP_FRAG_PPS_THRESHOLD = 20_000
    UDP_FRAG_MAX_BYTES_PER_PKT = 128
    ASYMMETRIC_DURATION_SECS = 300
    ASYMMETRIC_MAX_PPS = 10.0

    def extract_features(self, event: Any, client: Any) -> Optional[Features]:
        if event.event_type != "app_flow_event":
            return None
        import json as _json
        try:
            detail = _json.loads(event.detail_json) if isinstance(event.detail_json, str) else {}
        except Exception:
            detail = {}
        return Features.from_event(event, detail)

    def detect(self, features: Features) -> Optional[str]:
        d = features.detail
        proto = d.get("protocol", "").lower()
        pps = float(d.get("packets_per_sec", 0.0))
        bps = float(d.get("bytes_per_sec", 0.0))
        tcp_flags_pattern = d.get("tcp_flags_pattern", "")
        amplification_vector = d.get("amplification_vector", "")
        icmp_type = int(d.get("icmp_type", 0))
        flow_start_ns = int(d.get("flow_start_ns", 0))
        flow_end_ns = int(d.get("flow_end_ns", 0))
        duration_secs = max(0.0, (flow_end_ns - flow_start_ns) / 1e9) if flow_end_ns > flow_start_ns else 0.0
        bytes_per_pkt = (bps / pps) if pps > 0 else 0.0

        if proto == "tcp" and tcp_flags_pattern == "SYN_ONLY" and pps >= self.SYN_FLOOD_PPS_THRESHOLD:
            return f"syn_flood: pps={pps:.0f}"

        if amplification_vector and bytes_per_pkt >= self.AMPLIFICATION_BPP_THRESHOLD:
            return f"{amplification_vector}_amplification: bpp={bytes_per_pkt:.0f} pps={pps:.0f}"

        if proto == "icmp" and icmp_type in (8, 0) and pps >= self.ICMP_FLOOD_PPS_THRESHOLD:
            return f"icmp_flood: pps={pps:.0f} icmp_type={icmp_type}"

        if proto == "udp" and pps >= self.UDP_FRAG_PPS_THRESHOLD and 0 < bytes_per_pkt <= self.UDP_FRAG_MAX_BYTES_PER_PKT:
            return f"udp_fragment_flood: pps={pps:.0f} bpp={bytes_per_pkt:.0f}"

        if duration_secs >= self.ASYMMETRIC_DURATION_SECS and 0 < pps <= self.ASYMMETRIC_MAX_PPS and bps > 0:
            return f"asymmetric_low_and_slow: duration={duration_secs:.0f}s pps={pps:.2f}"

        return None
