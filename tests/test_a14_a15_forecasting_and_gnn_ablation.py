"""Tests for Slice P3 (A14–A15): Forecasting challenger evaluation & STGNN ablation."""

import numpy as np
import pytest

from bonsai_ml.gnn.ablation import (
    chronological_train_val_test_split,
    get_foundation_model_license_ledger,
    run_forecasting_challenger_experiment,
    run_pytorch_stgnn_held_out_experiment,
    run_stgnn_ablation_experiment,
)


def test_a14_chronological_split_rejects_non_monotonic_shuffle_and_audits_licenses():
    # Non-monotonic (shuffled) timestamps must be rejected so adjacent incident windows
    # are never leaked across train/test splits.
    with pytest.raises(ValueError, match="monotonically non-decreasing"):
        chronological_train_val_test_split([10, 20, 30, 15, 40, 50, 60, 70, 80, 90])

    train_idx, val_idx, test_idx = chronological_train_val_test_split(
        list(range(100, 1100, 10))
    )
    assert train_idx[-1] < val_idx[0]
    assert val_idx[-1] < test_idx[0]

    ledger = get_foundation_model_license_ledger()
    assert ledger["chronos_2_small"].unrestricted_self_hosted_production_drop_in is True
    assert ledger["chronos_2_small"].weights_license == "Apache-2.0"
    # Strategy §7 [S12]: TimesFM 3.0 downloaded weights are NOT an unrestricted self-hosted drop-in
    assert ledger["timesfm_3_0"].unrestricted_self_hosted_production_drop_in is False


def test_a14_forecasting_challenger_beats_univariate_ewma_under_planned_maintenance_covariate():
    n = 120
    timestamps = [1700000000 + i * 60 for i in range(n)]
    cov_maintenance = np.zeros(n, dtype=int)
    labels = np.zeros(n, dtype=int)
    series = np.full(n, 100.0, dtype=float)

    # Planned maintenance windows in train, val, and test (shift +40.0, NOT a fault)
    cov_maintenance[20:26] = 1
    series[20:26] += 40.0
    cov_maintenance[70:74] = 1
    series[70:74] += 40.0
    cov_maintenance[90:95] = 1
    series[90:95] += 40.0

    # True unexpected routing anomaly in test window (t=105..109) without maintenance
    labels[105:110] = 1
    series[105:110] += 55.0

    report = run_forecasting_challenger_experiment(
        timestamps_sec=timestamps,
        series=series,
        covariate_maintenance_active=cov_maintenance,
        labels=labels,
        max_false_alert_rate=0.05,
    )

    assert report["split_policy"] == "strict_chronological_train_val_test"
    assert report["challenger_beats_baseline_at_equal_budget"] is True
    assert (
        report["challenger_covariate_test_metrics"]["f1"]
        > report["baseline_ewma_test_metrics"]["f1"]
    )


def test_a15_stgnn_ablation_compares_full_model_against_no_topology_and_no_temporal():
    # 6 devices: devices 0 and 1 experience a correlated link/BGP cascade between t-1 and t
    x_prev = np.array(
        [
            [0.10, 0.10],
            [0.10, 0.12],
            [0.11, 0.09],
            [0.10, 0.10],
            [0.09, 0.11],
            [0.00, 0.00],  # node 5 absent at t-1 (cold start at t)
        ]
    )
    x_curr = np.array(
        [
            [0.92, 0.88],  # faulted
            [0.85, 0.90],  # faulted neighbor
            [0.12, 0.10],
            [0.10, 0.11],
            [0.11, 0.10],
            [0.12, 0.11],  # cold-start healthy node joining at t
        ]
    )
    adj = np.array(
        [
            [1, 1, 0, 0, 0, 0],
            [1, 1, 0, 0, 0, 0],
            [0, 0, 1, 1, 0, 0],
            [0, 0, 1, 1, 0, 0],
            [0, 0, 0, 0, 1, 1],
            [0, 0, 0, 0, 1, 1],
        ],
        dtype=float,
    )
    mask_prev = np.array([1, 1, 1, 1, 1, 0], dtype=float)
    mask_curr = np.array([1, 1, 1, 1, 1, 1], dtype=float)
    labels = np.array([1, 1, 0, 0, 0, 0], dtype=int)

    res = run_stgnn_ablation_experiment(
        node_features_t_minus_1=x_prev,
        node_features_t=x_curr,
        adjacency=adj,
        presence_mask_t_minus_1=mask_prev,
        presence_mask_t=mask_curr,
        true_fault_labels=labels,
    )

    assert res["entity_alignment_verified"] is True
    assert res["cold_start_entity_count"] == 1
    assert res["improves_over_simpler_ablations"] is True
    assert res["full_stgnn_metrics"]["event_recall"] == 1.0
    assert res["promotion_decision"] == "promote_to_shadow_ranking_aid_only"
    assert res["execution_mode"] == "heuristic_feature_proxy"
    assert "measured_latency_ms" in res


def test_a15_pytorch_stgnn_held_out_experiment():
    res = run_pytorch_stgnn_held_out_experiment(
        num_devices=8,
        temporal_steps=4,
        fault_device_idx=1,
    )
    assert res["experiment_id"] == "a15-pytorch-stgnn-held-out-v1"
    assert res["execution_mode"] == "measured_pytorch_model"
    assert res["entity_alignment_verified"] is True
    assert res["cold_start_entity_handled"] is True
    assert res["inference_latency_ms"] > 0.0
    assert res["total_experiment_time_ms"] > 0.0
    assert 0.0 <= res["validation_calibrated_threshold"] <= 1.0
    assert res["held_out_test_metrics"]["precision"] >= 0.0
    assert res["promotion_decision"] == "verified_pytorch_stgnn_prototype"

