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


def synthetic_tabular_ml_records() -> tuple[list[dict], list[dict]]:
    """Return labelled faults plus feature windows for tabular ML evaluation."""
    faults = [
        {
            "fault_id": "ml-bgp-tp",
            "fault_type": "bgp_session_down",
            "hostname": "srl-leaf1",
            "injected_at_ns": BASE_NS,
            "healed_at_ns": BASE_NS + 60 * SECOND,
            "should_trigger": True,
        },
        {
            "fault_id": "ml-if-fn",
            "fault_type": "interface_shut",
            "hostname": "srl-leaf2",
            "injected_at_ns": BASE_NS + 120 * SECOND,
            "healed_at_ns": BASE_NS + 180 * SECOND,
            "should_trigger": True,
        },
        {
            "fault_id": "ml-bgp-tn",
            "fault_type": "bgp_session_down",
            "hostname": "srl-leaf3",
            "injected_at_ns": BASE_NS + 240 * SECOND,
            "healed_at_ns": BASE_NS + 300 * SECOND,
            "should_trigger": False,
        },
    ]
    windows = [
        {
            "sample_id": "w-tp",
            "target": "srl-leaf1",
            "observed_at_ns": BASE_NS + 8 * SECOND,
            "features": [0.9, 0.2, 3.0],
        },
        {
            "sample_id": "w-normal",
            "target": "srl-leaf3",
            "observed_at_ns": BASE_NS + 260 * SECOND,
            "features": [0.1, 0.0, 0.0],
        },
        {
            "sample_id": "w-fp",
            "target": "srl-leaf4",
            "observed_at_ns": BASE_NS + 400 * SECOND,
            "features": [0.8, 0.7, 5.0],
        },
    ]
    return faults, windows
