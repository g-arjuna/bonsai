"""Tests for the tabular ML detector evaluation harness."""
from __future__ import annotations

from bonsai_ml.eval import (
    TabularFeatureWindow,
    evaluate_tabular_ml_detector,
    score_tabular_model,
)
from bonsai_ml.eval.test_fixtures import (
    BASE_NS,
    SECOND,
    synthetic_tabular_ml_records,
)


class SumScoreModel:
    def predict(self, matrix):
        return [matrix[0][0]]


class ProbabilityModel:
    def predict_proba(self, matrix):
        score = min(1.0, matrix[0][0])
        return [[1.0 - score, score]]


class DecisionFunctionModel:
    def decision_function(self, matrix):
        return [matrix[0][0]]


class IsolationStyleModel:
    def predict(self, matrix):
        return [-1 if matrix[0][0] > 0.5 else 1]


def test_tabular_ml_eval_uses_rule_metric_contract():
    faults, windows = synthetic_tabular_ml_records()
    report = evaluate_tabular_ml_detector(faults, windows, SumScoreModel(), threshold=0.75)

    result = report.baseline.rules["ml_anomaly_v1"]
    assert result.true_positive == 1
    assert result.false_positive == 1
    assert result.false_negative == 1
    assert result.true_negative == 1
    assert result.precision == 0.5
    assert result.recall == 0.5
    assert result.latency_ns == [8 * SECOND]

    data = report.as_dict()
    assert data["detector_id"] == "ml_anomaly_v1"
    assert data["windows_scored"] == 3
    assert data["detections"] == 2


def test_tabular_ml_markdown_embeds_shared_baseline_table():
    faults, windows = synthetic_tabular_ml_records()
    markdown = evaluate_tabular_ml_detector(
        faults,
        windows,
        SumScoreModel(),
        threshold=0.75,
    ).to_markdown()

    assert "# Tabular ML Detector Evaluation" in markdown
    assert "| ml_anomaly_v1 | 1 | 1 | 1 | 1 | 0.500 | 0.500 | 0.500 | 8000.000 |" in markdown


def test_tabular_feature_window_accepts_mapping_features_and_iso_timestamp():
    window = TabularFeatureWindow.from_mapping(
        {
            "id": "iso-window",
            "hostname": "srl-leaf1",
            "timestamp": "2026-05-08T12:00:00Z",
            "features": {"b": 2, "a": 1},
        }
    )

    assert window.sample_id == "iso-window"
    assert window.target == "srl-leaf1"
    assert window.features == (1.0, 2.0)
    assert window.observed_at_ns == 1_778_241_600_000_000_000


def test_tabular_model_scoring_supports_common_model_interfaces():
    assert score_tabular_model(ProbabilityModel(), [0.8]) == 0.8
    assert score_tabular_model(DecisionFunctionModel(), [-0.2]) == 0.7
    assert score_tabular_model(IsolationStyleModel(), [0.8]) == 1.0
    assert score_tabular_model(IsolationStyleModel(), [0.1]) == 0.0


def test_detector_id_can_be_overridden_for_comparison_runs():
    fault = {
        "fault_id": "custom",
        "fault_type": "bgp_session_down",
        "hostname": "srl-leaf1",
        "injected_at_ns": BASE_NS,
        "healed_at_ns": BASE_NS + 30 * SECOND,
    }
    window = {
        "sample_id": "custom-window",
        "target": "srl-leaf1",
        "observed_at_ns": BASE_NS + SECOND,
        "features": [0.9],
    }

    report = evaluate_tabular_ml_detector(
        [fault],
        [window],
        SumScoreModel(),
        detector_id="tabular_iforest_v1",
        threshold=0.5,
    )

    assert set(report.baseline.rules) == {"tabular_iforest_v1"}
    assert report.baseline.rules["tabular_iforest_v1"].true_positive == 1
