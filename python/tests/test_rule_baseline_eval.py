"""Tests for the synthetic rule-baseline evaluation harness."""
from __future__ import annotations

from bonsai_ml.eval import DetectionEvent, FaultInjection, evaluate_rule_baseline
from bonsai_ml.eval.test_fixtures import BASE_NS, SECOND, synthetic_rule_eval_records


def test_rule_baseline_counts_tp_fp_fn_tn():
    faults, detections = synthetic_rule_eval_records()
    report = evaluate_rule_baseline(faults, detections)

    bgp = report.rules["bgp_session_down"]
    assert bgp.true_positive == 1
    assert bgp.false_positive == 1
    assert bgp.false_negative == 0
    assert bgp.true_negative == 1
    assert bgp.precision == 0.5
    assert bgp.recall == 1.0

    interface = report.rules["interface_down"]
    assert interface.true_positive == 0
    assert interface.false_negative == 1
    assert interface.recall == 0.0


def test_rule_baseline_records_latency_and_clear_time():
    faults, detections = synthetic_rule_eval_records()
    report = evaluate_rule_baseline(faults, detections)
    bgp = report.rules["bgp_session_down"]

    assert bgp.latency_ns == [7 * SECOND]
    assert bgp.clear_time_ns == [14 * SECOND]
    assert bgp.as_dict()["latency_ms"]["p95"] == 7000.0


def test_detection_after_grace_window_is_false_negative_and_false_positive():
    fault = FaultInjection(
        fault_id="f-late",
        rule_id="bgp_session_down",
        target="srl-leaf1",
        injected_at_ns=BASE_NS,
        healed_at_ns=BASE_NS + 10 * SECOND,
    )
    detection = DetectionEvent(
        detection_id="d-late",
        rule_id="bgp_session_down",
        target="srl-leaf1",
        detected_at_ns=BASE_NS + 50 * SECOND,
    )

    report = evaluate_rule_baseline([fault], [detection], grace_ns=5 * SECOND)
    result = report.rules["bgp_session_down"]
    assert result.false_negative == 1
    assert result.false_positive == 1


def test_mapping_coercion_accepts_iso_timestamps_and_expected_rule_id():
    fault = {
        "fault_id": "f-iso",
        "expected_detection_rule_id": "interface_down",
        "target": "srl-leaf1",
        "injected_at_ns": "2026-05-08T12:00:00Z",
        "healed_at_ns": "2026-05-08T12:01:00Z",
    }
    detection = {
        "id": "d-iso",
        "rule_id": "interface_down",
        "hostname": "srl-leaf1",
        "timestamp": "2026-05-08T12:00:05Z",
    }

    report = evaluate_rule_baseline([fault], [detection])
    result = report.rules["interface_down"]
    assert result.true_positive == 1
    assert result.latency_ns == [5 * SECOND]


def test_markdown_report_contains_rule_table():
    faults, detections = synthetic_rule_eval_records()
    markdown = evaluate_rule_baseline(faults, detections).to_markdown()

    assert "# Detection Rule Baseline Evaluation" in markdown
    assert "| bgp_session_down | 1 | 1 | 0 | 1 | 0.500 | 1.000 | 0.667 | 7000.000 |" in markdown
