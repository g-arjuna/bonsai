"""Synthetic fixtures for detector-evaluation harness tests."""
from __future__ import annotations


BASE_NS = 1_778_240_000_000_000_000
SECOND = 1_000_000_000


def synthetic_rule_eval_records() -> tuple[list[dict], list[dict]]:
    """Return fault and detection records with TP, FP, FN, and TN coverage."""
    faults = [
        {
            "fault_id": "f-bgp-tp",
            "fault_type": "bgp_session_down",
            "hostname": "srl-leaf1",
            "injected_at_ns": BASE_NS,
            "healed_at_ns": BASE_NS + 60 * SECOND,
            "should_trigger": True,
        },
        {
            "fault_id": "f-if-fn",
            "fault_type": "interface_down",
            "hostname": "srl-leaf2",
            "injected_at_ns": BASE_NS + 120 * SECOND,
            "healed_at_ns": BASE_NS + 180 * SECOND,
            "should_trigger": True,
        },
        {
            "fault_id": "f-bgp-tn",
            "fault_type": "bgp_session_down",
            "hostname": "srl-leaf3",
            "injected_at_ns": BASE_NS + 240 * SECOND,
            "healed_at_ns": BASE_NS + 300 * SECOND,
            "should_trigger": False,
        },
    ]
    detections = [
        {
            "detection_id": "d-bgp-tp",
            "rule_id": "bgp_session_down",
            "target": "srl-leaf1",
            "detected_at_ns": BASE_NS + 7 * SECOND,
            "cleared_at_ns": BASE_NS + 74 * SECOND,
        },
        {
            "detection_id": "d-bgp-fp",
            "rule_id": "bgp_session_down",
            "target": "srl-leaf4",
            "detected_at_ns": BASE_NS + 400 * SECOND,
        },
    ]
    return faults, detections
