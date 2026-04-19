"""Topology anomaly detection rules."""
from __future__ import annotations

import time
from typing import TYPE_CHECKING, Optional

from ..detection import Detector, Features

if TYPE_CHECKING:
    from ..client import BonsaiClient


class TopologyEdgeLost(Detector):
    """A CONNECTED_TO edge that existed in the previous poll cycle is now absent."""
    rule_id = "topology_edge_lost"
    severity = "warn"

    _prev_edges: set[tuple[str, str, str, str]] = set()

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        return None  # poll-based — evaluated by engine.poll_topology()

    def detect(self, features: Features) -> Optional[str]:
        return None

    @classmethod
    def evaluate_topology(cls, current_edges: list, client: "BonsaiClient") -> list[tuple[str, str, str]]:
        """Returns list of (device_address, if_name, reason) for lost edges. Updates internal state."""
        current_set = {
            (e.src_device, e.src_interface, e.dst_device, e.dst_interface)
            for e in current_edges
        }
        lost   = cls._prev_edges - current_set if cls._prev_edges else set()
        cls._prev_edges = current_set
        results = []
        for src_dev, src_if, dst_dev, dst_if in lost:
            reason = (
                f"CONNECTED_TO edge lost: {src_dev}:{src_if} -> {dst_dev}:{dst_if} "
                f"(was present, now absent from LLDP)"
            )
            results.append((src_dev, src_if, reason))
        return results


TOPOLOGY_RULES = TopologyEdgeLost()
