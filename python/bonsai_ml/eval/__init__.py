"""Evaluation harnesses for Bonsai detector baselines."""

from .rule_baseline import (
    DetectionEvent,
    FaultInjection,
    RuleEvaluation,
    RuleEvaluationReport,
    evaluate_rule_baseline,
)
from .tabular_ml import (
    TabularFeatureWindow,
    TabularMLEvaluationReport,
    TabularScoredWindow,
    evaluate_tabular_ml_detector,
    score_tabular_model,
)

__all__ = [
    "DetectionEvent",
    "FaultInjection",
    "RuleEvaluation",
    "RuleEvaluationReport",
    "TabularFeatureWindow",
    "TabularMLEvaluationReport",
    "TabularScoredWindow",
    "evaluate_rule_baseline",
    "evaluate_tabular_ml_detector",
    "score_tabular_model",
]
