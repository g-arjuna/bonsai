"""Tests for DV1 D5 GNN pre-work scaffolding.

Covers:
  - D5-T1: Feature audit constants (vendor-neutral dominance)
  - D5-T3: FocalLoss / focal_loss (numpy path where torch absent)
  - D5-T4: CalibrationStore read/write/summary
  - D5-T5: evaluate_gnn, point_adjusted_f1, run_comparison_study, compute_confusion
"""
from __future__ import annotations

import math
import tempfile
from pathlib import Path

import pytest


# ── D5-T1: Feature engineering audit ─────────────────────────────────────────

def test_default_feature_names_count():
    from bonsai_ml.gnn.data_loader import DEFAULT_FEATURE_NAMES
    assert len(DEFAULT_FEATURE_NAMES) == 23


def test_vendor_features_are_minority():
    from bonsai_ml.gnn.data_loader import DEFAULT_FEATURE_NAMES
    vendor_features = [f for f in DEFAULT_FEATURE_NAMES if f.startswith("vendor_")]
    structural_and_role = [f for f in DEFAULT_FEATURE_NAMES if not f.startswith("vendor_")]
    assert len(vendor_features) < len(structural_and_role), (
        "Vendor features should be a minority of total features (CV5 philosophy)"
    )


def test_vendor_features_exactly_six():
    from bonsai_ml.gnn.data_loader import DEFAULT_FEATURE_NAMES
    vendor_features = [f for f in DEFAULT_FEATURE_NAMES if f.startswith("vendor_")]
    assert len(vendor_features) == 6


def test_spectral_embeddings_present():
    from bonsai_ml.gnn.data_loader import DEFAULT_FEATURE_NAMES
    embedding_dims = [f for f in DEFAULT_FEATURE_NAMES if f.startswith("embedding_")]
    assert len(embedding_dims) == 4


def test_degree_feature_present():
    from bonsai_ml.gnn.data_loader import DEFAULT_FEATURE_NAMES
    assert "degree" in DEFAULT_FEATURE_NAMES


# ── D5-T3: FocalLoss (numpy-only path) ───────────────────────────────────────

def test_focal_loss_import_no_torch():
    """FocalLoss class should be importable even without torch."""
    from bonsai_ml.gnn.loss import FocalLoss
    fl = FocalLoss(gamma=2.0, alpha=0.25)
    assert fl.gamma == 2.0
    assert fl.alpha == 0.25


def test_focal_loss_repr():
    from bonsai_ml.gnn.loss import FocalLoss
    fl = FocalLoss(gamma=2.0, alpha=0.25)
    r = repr(fl)
    assert "gamma=2.0" in r
    assert "alpha=0.25" in r


def test_focal_loss_with_torch():
    pytest.importorskip("torch")
    import torch
    from bonsai_ml.gnn.loss import focal_loss, FocalLoss

    torch.manual_seed(42)
    logits = torch.tensor([[2.0, 0.5], [0.1, 3.0], [1.0, 1.0]], dtype=torch.float32)
    targets = torch.tensor([0, 1, 0], dtype=torch.long)

    loss = focal_loss(logits, targets, gamma=2.0, alpha=0.25)
    assert loss.item() > 0.0
    assert not math.isnan(loss.item())

    fl = FocalLoss(gamma=2.0, alpha=0.25)
    loss2 = fl(logits, targets)
    assert abs(loss.item() - loss2.item()) < 1e-6


def test_focal_loss_gamma_zero_equals_cross_entropy():
    pytest.importorskip("torch")
    import torch
    import torch.nn.functional as F
    from bonsai_ml.gnn.loss import focal_loss

    torch.manual_seed(0)
    logits = torch.randn(8, 3)
    targets = torch.randint(0, 3, (8,))

    fl = focal_loss(logits, targets, gamma=0.0, alpha=None)
    ce = F.cross_entropy(logits, targets)
    assert abs(fl.item() - ce.item()) < 1e-5, (
        "focal_loss with gamma=0 and no alpha should equal cross-entropy"
    )


# ── D5-T4: CalibrationStore ───────────────────────────────────────────────────

def test_calibration_store_write_and_load():
    from bonsai_ml.gnn.calibration import CalibrationStore, make_calibration_record

    with tempfile.TemporaryDirectory() as tmpdir:
        store = CalibrationStore(runtime_dir=tmpdir, threshold=0.5, min_samples=3)
        assert store.record_count() == 0

        r1 = make_calibration_record("srl-leaf1", 0.9, "hetero_gat_v1")
        r2 = make_calibration_record("srl-leaf2", 0.3, "hetero_gat_v1")
        store.write(r1)
        store.write(r2)

        assert store.record_count() == 2
        records = store.load()
        assert len(records) == 2
        scores = {r.node_id: r.anomaly_score for r in records}
        assert scores["srl-leaf1"] == pytest.approx(0.9)
        assert scores["srl-leaf2"] == pytest.approx(0.3)


def test_calibration_store_batch_write():
    from bonsai_ml.gnn.calibration import CalibrationStore, make_calibration_record

    with tempfile.TemporaryDirectory() as tmpdir:
        store = CalibrationStore(runtime_dir=tmpdir)
        records = [
            make_calibration_record(f"node-{i}", i * 0.1, "test_model")
            for i in range(10)
        ]
        store.write_batch(records)
        assert store.record_count() == 10


def test_calibration_summary_not_ready_below_min_samples():
    from bonsai_ml.gnn.calibration import CalibrationStore, make_calibration_record

    with tempfile.TemporaryDirectory() as tmpdir:
        store = CalibrationStore(runtime_dir=tmpdir, threshold=0.5, min_samples=100)
        for i in range(10):
            store.write(make_calibration_record(f"node-{i}", 0.6, "v1"))

        summary = store.summary()
        assert summary.num_records == 10
        assert not summary.ready_for_production
        assert summary.min_samples_required == 100


def test_calibration_summary_ready_above_min_samples():
    from bonsai_ml.gnn.calibration import CalibrationStore, make_calibration_record

    with tempfile.TemporaryDirectory() as tmpdir:
        store = CalibrationStore(runtime_dir=tmpdir, threshold=0.5, min_samples=5)
        for i in range(6):
            store.write(make_calibration_record(f"node-{i}", 0.4 + i * 0.1, "v1"))

        summary = store.summary()
        assert summary.ready_for_production
        assert summary.num_records == 6


def test_calibration_summary_empty_store():
    from bonsai_ml.gnn.calibration import CalibrationStore

    with tempfile.TemporaryDirectory() as tmpdir:
        store = CalibrationStore(runtime_dir=tmpdir)
        summary = store.summary()
        assert summary.num_records == 0
        assert not summary.ready_for_production


def test_calibration_summary_fraction_above_threshold():
    from bonsai_ml.gnn.calibration import CalibrationStore, make_calibration_record

    with tempfile.TemporaryDirectory() as tmpdir:
        store = CalibrationStore(runtime_dir=tmpdir, threshold=0.5, min_samples=1)
        store.write_batch([
            make_calibration_record("a", 0.8, "v1"),
            make_calibration_record("b", 0.2, "v1"),
            make_calibration_record("c", 0.6, "v1"),
            make_calibration_record("d", 0.1, "v1"),
        ])
        summary = store.summary()
        assert summary.fraction_above_threshold == pytest.approx(0.5)


# ── D5-T5: Evaluation harness ─────────────────────────────────────────────────

def test_compute_confusion_perfect():
    from bonsai_ml.gnn.eval import compute_confusion
    cm = compute_confusion([1, 0, 1, 0], [1, 0, 1, 0])
    assert cm.tp == 2
    assert cm.tn == 2
    assert cm.fp == 0
    assert cm.fn == 0
    assert cm.precision == 1.0
    assert cm.recall == 1.0
    assert cm.f1 == 1.0


def test_compute_confusion_all_wrong():
    from bonsai_ml.gnn.eval import compute_confusion
    cm = compute_confusion([1, 1, 0, 0], [0, 0, 1, 1])
    assert cm.tp == 0
    assert cm.fn == 2
    assert cm.fp == 2
    assert cm.f1 == 0.0


def test_evaluate_gnn_basic():
    from bonsai_ml.gnn.eval import evaluate_gnn
    y_true = [1, 0, 1, 0, 1, 0, 0, 1]
    y_score = [0.9, 0.1, 0.8, 0.2, 0.7, 0.3, 0.4, 0.6]
    report = evaluate_gnn("test_model", y_true, y_score, threshold=0.5)

    assert report.model_tag == "test_model"
    assert report.num_samples == 8
    assert report.num_anomalies == 4
    assert 0.0 <= report.f1 <= 1.0
    assert 0.0 <= report.pa_f1 <= 1.0
    assert 0.0 <= report.auc_roc <= 1.0 or math.isnan(report.auc_roc)


def test_evaluate_gnn_perfect_detector():
    from bonsai_ml.gnn.eval import evaluate_gnn
    y_true = [1, 0, 1, 0]
    y_score = [0.9, 0.1, 0.8, 0.2]
    report = evaluate_gnn("perfect", y_true, y_score, threshold=0.5)
    assert report.f1 == pytest.approx(1.0)


def test_point_adjusted_f1_full_segment_credit():
    """If any point in a fault segment is detected, the whole segment gets credit."""
    from bonsai_ml.gnn.eval import point_adjusted_f1
    y_true  = [0, 1, 1, 1, 0]
    y_score = [0.1, 0.1, 0.9, 0.1, 0.1]
    pa_f1 = point_adjusted_f1(y_true, y_score, threshold=0.5)
    assert pa_f1 == pytest.approx(1.0), (
        "Detecting one point in the fault window should credit the whole segment"
    )


def test_point_adjusted_f1_empty():
    from bonsai_ml.gnn.eval import point_adjusted_f1
    assert point_adjusted_f1([], [], threshold=0.5) == 0.0


def test_run_comparison_study_sorted_by_f1():
    from bonsai_ml.gnn.eval import run_comparison_study
    y_true = [1, 0, 1, 0, 1, 0]
    detectors = [
        ("weak",   [0.6, 0.5, 0.6, 0.5, 0.6, 0.5], "weak baseline"),
        ("strong", [0.9, 0.1, 0.9, 0.1, 0.9, 0.1], "strong detector"),
        ("random", [0.5, 0.5, 0.5, 0.5, 0.5, 0.5], "random"),
    ]
    rows = run_comparison_study(y_true, detectors)
    assert rows[0].detector == "strong"
    f1_values = [r.f1 for r in rows]
    assert f1_values == sorted(f1_values, reverse=True)


def test_gnn_eval_report_summary_line():
    from bonsai_ml.gnn.eval import evaluate_gnn
    y_true = [1, 0, 1, 0]
    y_score = [0.9, 0.1, 0.8, 0.2]
    report = evaluate_gnn("my_model", y_true, y_score)
    line = report.summary_line()
    assert "my_model" in line
    assert "F1=" in line
    assert "PA-F1=" in line


def test_gnn_eval_report_as_dict():
    from bonsai_ml.gnn.eval import evaluate_gnn
    y_true = [1, 0]
    y_score = [0.9, 0.1]
    report = evaluate_gnn("v1", y_true, y_score)
    d = report.as_dict()
    assert "confusion" in d
    assert "tp" in d["confusion"]
    assert "f1" in d


# ── D5-T2: model config (torch-free) ─────────────────────────────────────────

def test_bonsai_gnn_config_defaults():
    from bonsai_ml.gnn.model import BonsaiGnnConfig
    cfg = BonsaiGnnConfig()
    assert cfg.hidden_channels == 64
    assert cfg.num_heads == 4
    assert cfg.num_layers == 2
    assert cfg.output_classes == 2


def test_build_model_requires_torch():
    """build_model should raise RuntimeError with clear message when torch absent."""
    try:
        import torch  # noqa: F401
        pytest.skip("torch is present; skipping no-torch path")
    except ImportError:
        pass
    from bonsai_ml.gnn.model import build_model
    with pytest.raises(RuntimeError, match="torch"):
        build_model()


def test_build_model_with_torch():
    pytest.importorskip("torch")
    pytest.importorskip("torch_geometric")
    from bonsai_ml.gnn.model import build_model, BonsaiGnnConfig
    import torch

    cfg = BonsaiGnnConfig(hidden_channels=16, num_heads=2, num_layers=1)
    model = build_model(cfg)
    assert model is not None
    assert hasattr(model, "forward")
    params = sum(p.numel() for p in model.parameters())
    assert params > 0
