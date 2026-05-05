"""Tests for bonsai_ml.embeddings and bonsai_ml.feature_schema (T5-3)."""
import json
import tempfile
from pathlib import Path
from unittest.mock import MagicMock, patch

import numpy as np
import pytest

from bonsai_ml.embeddings import (
    compute_spectral_embedding,
    fetch_adjacency,
    push_embeddings,
    run_embedding_pipeline,
)
from bonsai_ml.feature_schema import SPECTRAL_V1_SCHEMA, FeatureSchema


# ── helpers ───────────────────────────────────────────────────────────────────

def _mock_topology(devices: list[str], links: list[tuple[str, str]]) -> dict:
    return {
        "devices": [{"address": d, "hostname": d, "vendor": "test"} for d in devices],
        "links": [
            {"src_device": s, "src_iface": "eth0", "dst_device": d, "dst_iface": "eth0"}
            for s, d in links
        ],
    }


def _mock_client(topology: dict) -> MagicMock:
    client = MagicMock()
    client._http_json.side_effect = lambda method, path, payload=None: (
        topology if path == "/api/topology" else {}
    )
    return client


# ── fetch_adjacency ───────────────────────────────────────────────────────────

def test_fetch_adjacency_simple_chain():
    topo = _mock_topology(["A", "B", "C"], [("A", "B"), ("B", "C")])
    client = _mock_client(topo)
    devices, adj = fetch_adjacency(client)
    assert sorted(devices) == ["A", "B", "C"]
    assert "B" in adj["A"]
    assert "A" in adj["B"]
    assert "C" in adj["B"]


def test_fetch_adjacency_is_undirected():
    topo = _mock_topology(["X", "Y"], [("X", "Y")])
    client = _mock_client(topo)
    _, adj = fetch_adjacency(client)
    assert "Y" in adj["X"]
    assert "X" in adj["Y"]


def test_fetch_adjacency_no_self_loops():
    topo = _mock_topology(["A"], [("A", "A")])
    client = _mock_client(topo)
    _, adj = fetch_adjacency(client)
    assert "A" not in adj.get("A", [])


def test_fetch_adjacency_empty_topology():
    topo = _mock_topology([], [])
    client = _mock_client(topo)
    devices, adj = fetch_adjacency(client)
    assert devices == []
    assert adj == {}


def test_fetch_adjacency_no_duplicate_neighbors():
    # Two links between same pair should only result in one entry each side
    topo = {
        "devices": [{"address": "A"}, {"address": "B"}],
        "links": [
            {"src_device": "A", "dst_device": "B"},
            {"src_device": "A", "dst_device": "B"},
        ],
    }
    client = _mock_client(topo)
    _, adj = fetch_adjacency(client)
    assert adj["A"].count("B") == 1
    assert adj["B"].count("A") == 1


# ── compute_spectral_embedding ────────────────────────────────────────────────

def test_compute_spectral_embedding_output_shape():
    devices = ["A", "B", "C", "D"]
    adj = {"A": ["B"], "B": ["A", "C"], "C": ["B", "D"], "D": ["C"]}
    result = compute_spectral_embedding(devices, adj, n_components=4)
    assert set(result.keys()) == set(devices)
    for vec in result.values():
        assert len(vec) == 4


def test_compute_spectral_embedding_pads_small_graph():
    # 3-node graph, request 16 dims → should pad with zeros not crash
    devices = ["A", "B", "C"]
    adj = {"A": ["B"], "B": ["A", "C"], "C": ["B"]}
    result = compute_spectral_embedding(devices, adj, n_components=16)
    assert all(len(v) == 16 for v in result.values())


def test_compute_spectral_embedding_empty_graph():
    result = compute_spectral_embedding([], {}, n_components=8)
    assert result == {}


def test_compute_spectral_embedding_isolated_device():
    devices = ["connected", "isolated"]
    adj = {"connected": [], "isolated": []}
    result = compute_spectral_embedding(devices, adj, n_components=2)
    assert set(result.keys()) == set(devices)
    assert len(result["isolated"]) == 2


def test_compute_spectral_embedding_star_topology():
    hub = "H"
    leaves = ["L1", "L2", "L3", "L4"]
    devices = [hub] + leaves
    adj = {hub: leaves[:], **{lf: [hub] for lf in leaves}}
    result = compute_spectral_embedding(devices, adj, n_components=4)
    # Hub and leaves should have different embeddings
    hub_vec = np.array(result[hub])
    leaf_vecs = [np.array(result[lf]) for lf in leaves]
    leaf_centroid = np.mean(leaf_vecs, axis=0)
    # Hub embedding differs from the leaf centroid
    assert np.linalg.norm(hub_vec - leaf_centroid) > 1e-6, (
        "hub should be distinguishable from leaves in embedding space"
    )


def test_compute_spectral_embedding_reproducible():
    devices = ["A", "B", "C", "D"]
    adj = {"A": ["B"], "B": ["A", "C"], "C": ["B", "D"], "D": ["C"]}
    r1 = compute_spectral_embedding(devices, adj, n_components=4, random_state=0)
    r2 = compute_spectral_embedding(devices, adj, n_components=4, random_state=0)
    for addr in devices:
        diff = sum(abs(a - b) for a, b in zip(r1[addr], r2[addr]))
        assert diff < 1e-9, "same random_state should produce identical output"


# ── push_embeddings ───────────────────────────────────────────────────────────

def test_push_embeddings_calls_api():
    client = MagicMock()
    client._http_json.return_value = {}
    embeddings = {"10.0.0.1": [0.1, 0.2], "10.0.0.2": [0.3, 0.4]}
    count = push_embeddings(client, embeddings, version="spectral_v1")
    assert count == 2
    client._http_json.assert_called_once()
    call_args = client._http_json.call_args
    assert call_args[0][0] == "POST"
    assert "/api/graph/embeddings/upsert" in call_args[0][1]
    payload = call_args[0][2]
    assert len(payload["records"]) == 2


def test_push_embeddings_skips_empty_vectors():
    client = MagicMock()
    client._http_json.return_value = {}
    embeddings = {"A": [0.1, 0.2], "B": []}
    count = push_embeddings(client, embeddings, version="v1")
    assert count == 1
    payload = client._http_json.call_args[0][2]
    addresses = [r["device_address"] for r in payload["records"]]
    assert "A" in addresses
    assert "B" not in addresses


def test_push_embeddings_no_api_call_when_all_empty():
    client = MagicMock()
    push_embeddings(client, {"A": []}, version="v1")
    client._http_json.assert_not_called()


# ── run_embedding_pipeline ────────────────────────────────────────────────────

def test_run_embedding_pipeline_end_to_end():
    topo = _mock_topology(
        ["s1", "s2", "l1", "l2"],
        [("s1", "l1"), ("s1", "l2"), ("s2", "l1"), ("s2", "l2")],
    )
    client = MagicMock()
    client._http_json.side_effect = lambda method, path, payload=None: (
        topo if path == "/api/topology" else {}
    )
    result = run_embedding_pipeline(client)
    assert result["devices"] == 4
    assert result["pushed"] == 4
    assert "schema_hash" in result


def test_run_embedding_pipeline_empty_topology():
    client = _mock_client(_mock_topology([], []))
    result = run_embedding_pipeline(client)
    assert result["devices"] == 0
    assert result["pushed"] == 0


# ── FeatureSchema (T7-11) ─────────────────────────────────────────────────────

def test_feature_schema_hash_is_deterministic():
    s1 = FeatureSchema(
        version="v1", algorithm="spectral", dimension=8,
        hyperparams={"a": 1}, feature_names=["f0", "f1"],
    )
    s2 = FeatureSchema(
        version="v1", algorithm="spectral", dimension=8,
        hyperparams={"a": 1}, feature_names=["f0", "f1"],
    )
    assert s1.schema_hash == s2.schema_hash


def test_feature_schema_hash_changes_on_version():
    s1 = FeatureSchema("v1", "spectral", 8, {}, ["f0"])
    s2 = FeatureSchema("v2", "spectral", 8, {}, ["f0"])
    assert s1.schema_hash != s2.schema_hash


def test_feature_schema_hash_changes_on_dimension():
    s1 = FeatureSchema("v1", "spectral", 8, {}, ["f0"])
    s2 = FeatureSchema("v1", "spectral", 16, {}, ["f0"])
    assert s1.schema_hash != s2.schema_hash


def test_feature_schema_hash_changes_on_feature_names():
    s1 = FeatureSchema("v1", "spectral", 4, {}, ["f0", "f1"])
    s2 = FeatureSchema("v1", "spectral", 4, {}, ["f0", "DIFFERENT"])
    assert s1.schema_hash != s2.schema_hash


def test_feature_schema_hash_stable_across_created_at():
    s1 = FeatureSchema("v1", "a", 4, {}, ["f"], created_at_iso="2026-01-01T00:00:00Z")
    s2 = FeatureSchema("v1", "a", 4, {}, ["f"], created_at_iso="2099-12-31T23:59:59Z")
    # created_at excluded from hash — hash must be identical
    s1.schema_hash = s1._compute_hash()
    s2.schema_hash = s2._compute_hash()
    assert s1.schema_hash == s2.schema_hash


def test_feature_schema_save_load_roundtrip():
    schema = FeatureSchema(
        version="test_v1", algorithm="spectral", dimension=4,
        hyperparams={"n_neighbors": 5}, feature_names=["g0", "g1", "g2", "g3"],
    )
    with tempfile.TemporaryDirectory() as tmpdir:
        path = Path(tmpdir) / "schema.json"
        schema.save(path)
        loaded = FeatureSchema.load(path)
    assert loaded.version == schema.version
    assert loaded.algorithm == schema.algorithm
    assert loaded.dimension == schema.dimension
    assert loaded.hyperparams == schema.hyperparams
    assert loaded.feature_names == schema.feature_names
    assert loaded.schema_hash == schema.schema_hash


def test_feature_schema_matches():
    s1 = FeatureSchema("v1", "spectral", 8, {"k": 3}, ["a", "b"])
    s2 = FeatureSchema("v1", "spectral", 8, {"k": 3}, ["a", "b"])
    s3 = FeatureSchema("v2", "spectral", 8, {"k": 3}, ["a", "b"])
    assert s1.matches(s2)
    assert not s1.matches(s3)


def test_spectral_v1_schema_hash_is_known():
    # Freeze the canonical schema hash so a hyperparameter change is caught.
    expected = "16930b3e902e7600028ec87215ba6b5d7949899cc9bb61e4576a85e3d2995f75"
    assert SPECTRAL_V1_SCHEMA.schema_hash == expected, (
        f"spectral_v1 schema hash changed — bump version to spectral_v2 "
        f"and update model card. Got: {SPECTRAL_V1_SCHEMA.schema_hash}"
    )
