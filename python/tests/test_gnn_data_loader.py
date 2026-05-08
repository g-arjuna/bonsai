"""Tests for the synthetic-first Bonsai GNN data loader."""
from __future__ import annotations

import csv

import numpy as np
import pytest

from bonsai_ml.gnn import BonsaiGnnDataLoader, BonsaiGraphData
from bonsai_ml.gnn.test_fixtures import synthetic_dc_snapshot


def test_loader_builds_graph_from_synthetic_snapshot():
    graph = BonsaiGnnDataLoader().from_snapshot(synthetic_dc_snapshot())
    assert isinstance(graph, BonsaiGraphData)
    assert graph.x.shape == (4, len(graph.feature_names))
    assert graph.edge_index.shape == (2, 8)
    assert graph.y.shape == (4,)
    assert graph.y.sum() == 1
    assert graph.node_ids[graph.y.argmax()] == "srl-leaf1"
    assert graph.metadata["source"] == "synthetic_dc_fixture"


def test_loader_features_include_degree_vendor_role_and_embeddings():
    graph = BonsaiGnnDataLoader().from_snapshot(synthetic_dc_snapshot())
    leaf_row = graph.node_ids.index("srl-leaf1")
    feature = {name: idx for idx, name in enumerate(graph.feature_names)}
    assert graph.x[leaf_row, feature["degree"]] == 2
    assert graph.x[leaf_row, feature["vendor_nokia"]] == 1
    assert graph.x[leaf_row, feature["role_leaf"]] == 1
    np.testing.assert_allclose(
        graph.x[leaf_row, [feature["embedding_0"], feature["embedding_1"]]],
        np.array([0.9, 0.1], dtype=np.float32),
    )


def test_loader_ignores_fault_outside_snapshot_window():
    snapshot = synthetic_dc_snapshot()
    snapshot["chaos_log"][0]["healed_at_ns"] = snapshot["snapshot_ns"] - 1
    graph = BonsaiGnnDataLoader().from_snapshot(snapshot)
    assert graph.y.sum() == 0


def test_loader_reads_chaos_csv_with_topology_payload(tmp_path):
    snapshot = synthetic_dc_snapshot()
    csv_path = tmp_path / "injections.csv"
    with open(csv_path, "w", newline="", encoding="utf-8") as fh:
        writer = csv.DictWriter(
            fh,
            fieldnames=["fault_type", "hostname", "param", "injected_at_ns", "healed_at_ns"],
        )
        writer.writeheader()
        writer.writerow(snapshot["chaos_log"][0])

    graph = BonsaiGnnDataLoader().from_files(
        {"devices": snapshot["devices"], "links": snapshot["links"]},
        csv_path,
        snapshot_ns=snapshot["snapshot_ns"],
    )
    assert graph.y.sum() == 1
    assert graph.metadata["source"] == "files"


def test_graph_validation_catches_bad_shapes():
    graph = BonsaiGnnDataLoader().from_snapshot(synthetic_dc_snapshot())
    graph.edge_index = np.zeros((3, 1), dtype=np.int64)
    with pytest.raises(ValueError, match="edge_index"):
        graph.validate()


def test_pyg_conversion_reports_missing_optional_dependencies(monkeypatch):
    graph = BonsaiGnnDataLoader().from_snapshot(synthetic_dc_snapshot())

    real_import = __import__

    def fake_import(name, *args, **kwargs):
        if name in {"torch", "torch_geometric.data"} or name.startswith("torch_geometric"):
            raise ImportError(name)
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr("builtins.__import__", fake_import)
    with pytest.raises(RuntimeError, match="PyTorch Geometric"):
        graph.to_pyg()
