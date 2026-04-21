"""Lazy package exports so utility modules can load without optional gRPC deps."""
from __future__ import annotations

from importlib import import_module

__all__ = [
    "BonsaiClient",
    "Detector",
    "Detection",
    "Features",
    "MLDetector",
    "MLRemediationSelector",
    "RemediationExecutor",
    "RuleEngine",
    "features_to_vector",
]

_EXPORTS = {
    "BonsaiClient": ("bonsai_sdk.client", "BonsaiClient"),
    "Detector": ("bonsai_sdk.detection", "Detector"),
    "Detection": ("bonsai_sdk.detection", "Detection"),
    "Features": ("bonsai_sdk.detection", "Features"),
    "MLDetector": ("bonsai_sdk.ml_detector", "MLDetector"),
    "MLRemediationSelector": ("bonsai_sdk.ml_remediation", "MLRemediationSelector"),
    "RemediationExecutor": ("bonsai_sdk.remediations", "RemediationExecutor"),
    "RuleEngine": ("bonsai_sdk.engine", "RuleEngine"),
    "features_to_vector": ("bonsai_sdk.ml_detector", "features_to_vector"),
}


def __getattr__(name: str):
    if name not in _EXPORTS:
        raise AttributeError(f"module 'bonsai_sdk' has no attribute {name!r}")
    module_name, attr_name = _EXPORTS[name]
    module = import_module(module_name)
    value = getattr(module, attr_name)
    globals()[name] = value
    return value
