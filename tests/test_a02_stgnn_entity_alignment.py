"""A02 regression tests: STGNN temporal entity alignment by stable node IDs.

Reproduces Strategy §3 finding: STGNNModel.forward() previously aligned temporal
embeddings by tensor row position (`padded[:n, offset + i, :] = emb[:n]`),
mixing different devices' histories when nodes were added, removed, or reordered
between snapshots.
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
PYTHON_DIR = ROOT / "python"
if str(PYTHON_DIR) not in sys.path:
    sys.path.insert(0, str(PYTHON_DIR))


def test_stgnn_forward_invariance_under_historical_node_insertion_and_permutation() -> None:
    """When a historical snapshot has a subset of nodes or a different node order,
    each device in the latest snapshot must receive its own historical embedding
    (and zeros + mask=0 when absent), never a neighbour's shifted row.
    """
    torch = pytest.importorskip("torch")
    pytest.importorskip("torch_geometric")

    from bonsai_ml.gnn.model import BonsaiGnnConfig, build_hetero_data
    from bonsai_ml.gnn.stgnn import build_stgnn

    torch.manual_seed(42)
    cfg = BonsaiGnnConfig(hidden_channels=16, num_heads=4, num_layers=1, dropout=0.0, temporal_window=3)
    model = build_stgnn(cfg)
    model.eval()

    # Snapshot t=0 has only rtr-b and rtr-c connected to each other (rtr-a does not exist yet).
    snap0_without_a = build_hetero_data(
        {
            "snapshot_ns": 1_000_000_000,
            "devices": [
                {"id": "rtr-b", "vendor": "frr", "role": "spine", "cpu_util_pct": 85.0},
                {"id": "rtr-c", "vendor": "nokia", "role": "leaf", "cpu_util_pct": 12.0},
            ],
            "links": [
                {"src": "rtr-b", "dst": "rtr-c"},
                {"src": "rtr-c", "dst": "rtr-b"},
            ],
        }
    )

    # Snapshot t=0 with rtr-a present (isolated) vs absent: for rtr-b and rtr-c,
    # their spatial embeddings at t=0 are identical whether rtr-a is absent or isolated,
    # so their final GRU logits at t=1 must match!
    snap0_with_isolated_a = build_hetero_data(
        {
            "snapshot_ns": 1_000_000_000,
            "devices": [
                {"id": "rtr-a", "vendor": "cisco", "role": "border", "cpu_util_pct": 50.0},
                {"id": "rtr-b", "vendor": "frr", "role": "spine", "cpu_util_pct": 85.0},
                {"id": "rtr-c", "vendor": "nokia", "role": "leaf", "cpu_util_pct": 12.0},
            ],
            "links": [
                {"src": "rtr-b", "dst": "rtr-c"},
                {"src": "rtr-c", "dst": "rtr-b"},
            ],
        }
    )

    # Snapshot t=1 has rtr-a, rtr-b, rtr-c.
    snap1 = build_hetero_data(
        {
            "snapshot_ns": 2_000_000_000,
            "devices": [
                {"id": "rtr-a", "vendor": "cisco", "role": "border", "cpu_util_pct": 55.0},
                {"id": "rtr-b", "vendor": "frr", "role": "spine", "cpu_util_pct": 90.0},
                {"id": "rtr-c", "vendor": "nokia", "role": "leaf", "cpu_util_pct": 15.0},
            ],
            "links": [
                {"src": "rtr-b", "dst": "rtr-c"},
                {"src": "rtr-c", "dst": "rtr-b"},
            ],
        }
    )

    with torch.no_grad():
        logits_when_a_added, _ = model([snap0_without_a, snap1])
        logits_when_a_present, _ = model([snap0_with_isolated_a, snap1])

    # Because rtr-a has no edges to rtr-b or rtr-c, rtr-b (index 1 in snap1) and
    # rtr-c (index 2 in snap1) have the exact same spatial embeddings at t=0 and t=1
    # in both sequences. Under ID-aligned temporal fusion, their logits at t=1
    # MUST be identical (whereas row-position alignment shifted rtr-b's t=0
    # embedding into rtr-a and rtr-c's t=0 embedding into rtr-b!).
    assert torch.allclose(
        logits_when_a_added["device"][1],
        logits_when_a_present["device"][1],
        atol=1e-6,
    ), (
        f"rtr-b logits corrupted when rtr-a was inserted at t=1: "
        f"{logits_when_a_added['device'][1]} vs {logits_when_a_present['device'][1]}"
    )
    assert torch.allclose(
        logits_when_a_added["device"][2],
        logits_when_a_present["device"][2],
        atol=1e-6,
    ), (
        f"rtr-c logits corrupted when rtr-a was inserted at t=1: "
        f"{logits_when_a_added['device'][2]} vs {logits_when_a_present['device'][2]}"
    )

    # Also verify presence mask and elapsed seconds when running [snap0_without_a, snap1]
    with torch.no_grad():
        model([snap0_without_a, snap1])
    mask = model.last_presence_masks["device"]
    # T=3, 2 snapshots -> slot 0 is padded (0), slot 1 is t=0, slot 2 is t=1.
    # Row 0 (rtr-a) was absent at t=0 (slot 1 -> 0.0) and present at t=1 (slot 2 -> 1.0).
    assert mask.shape == (3, 3)
    assert mask[0].tolist() == [0.0, 0.0, 1.0]
    assert mask[1].tolist() == [0.0, 1.0, 1.0]
    assert mask[2].tolist() == [0.0, 1.0, 1.0]
    assert model.last_elapsed_seconds.tolist() == pytest.approx([0.0, 1.0, 0.0])


def test_stgnn_forward_invariance_under_reordered_historical_node_ids() -> None:
    """Reordering rows and node_ids in an earlier snapshot must not change latest logits."""
    torch = pytest.importorskip("torch")
    pytest.importorskip("torch_geometric")

    from bonsai_ml.gnn.model import BonsaiGnnConfig, build_hetero_data
    from bonsai_ml.gnn.stgnn import build_stgnn

    torch.manual_seed(7)
    cfg = BonsaiGnnConfig(hidden_channels=16, num_heads=4, num_layers=1, dropout=0.0, temporal_window=3)
    model = build_stgnn(cfg)
    model.eval()

    snap0 = build_hetero_data(
        {
            "snapshot_ns": 1_000_000_000,
            "devices": [
                {"id": "rtr-1", "vendor": "frr", "role": "spine", "cpu_util_pct": 10.0},
                {"id": "rtr-2", "vendor": "nokia", "role": "leaf", "cpu_util_pct": 90.0},
            ],
            "links": [
                {"src": "rtr-1", "dst": "rtr-2"},
                {"src": "rtr-2", "dst": "rtr-1"},
            ],
        }
    )
    snap0_reordered = build_hetero_data(
        {
            "snapshot_ns": 1_000_000_000,
            "devices": [
                {"id": "rtr-1", "vendor": "frr", "role": "spine", "cpu_util_pct": 10.0},
                {"id": "rtr-2", "vendor": "nokia", "role": "leaf", "cpu_util_pct": 90.0},
            ],
            "links": [
                {"src": "rtr-1", "dst": "rtr-2"},
                {"src": "rtr-2", "dst": "rtr-1"},
            ],
        }
    )
    # Reverse row order in snap0_reordered (and swap edge indices 0 <-> 1)
    snap0_reordered["device"].x = snap0["device"].x.flip(0)
    snap0_reordered["device"].node_ids = ["rtr-2", "rtr-1"]
    snap0_reordered["device", "connected_to", "device"].edge_index = (
        1 - snap0["device", "connected_to", "device"].edge_index
    )

    snap1 = build_hetero_data(
        {
            "snapshot_ns": 2_000_000_000,
            "devices": [
                {"id": "rtr-1", "vendor": "frr", "role": "spine", "cpu_util_pct": 20.0},
                {"id": "rtr-2", "vendor": "nokia", "role": "leaf", "cpu_util_pct": 80.0},
            ],
            "links": [
                {"src": "rtr-1", "dst": "rtr-2"},
                {"src": "rtr-2", "dst": "rtr-1"},
            ],
        }
    )

    with torch.no_grad():
        logits_orig, _ = model([snap0, snap1])
        logits_reordered, _ = model([snap0_reordered, snap1])

    assert torch.allclose(logits_orig["device"], logits_reordered["device"], atol=1e-6)


def test_build_hetero_data_populates_all_node_ids_and_timestamps() -> None:
    """build_hetero_data must populate stable node_ids for all 8 node types and snapshot_ns."""
    pytest.importorskip("torch")
    pytest.importorskip("torch_geometric")
    from bonsai_ml.gnn.model import build_hetero_data

    hd = build_hetero_data(
        {
            "snapshot_ns": 5_000_000_000,
            "devices": [{"id": "rtr-1", "vendor": "frr"}],
            "interfaces": [{"device_address": "rtr-1", "name": "eth0"}],
            "bgp_sessions": [{"device_address": "rtr-1", "peer_address": "10.0.0.2"}],
            "bfd_sessions": [{"device_address": "rtr-1", "peer_address": "10.0.0.2"}],
            "ospf_neighbors": [{"device_address": "rtr-1", "neighbor_id": "2.2.2.2"}],
            "redundancy_groups": [{"id": "rg-lag-1", "member_addresses": ["rtr-1"]}],
            "sensors": [{"device_address": "rtr-1", "sensor_name": "cpu-temp"}],
            "app_flows": [{"device_address": "rtr-1", "flow_id": "flow-42"}],
            "links": [],
        }
    )
    assert hd.snapshot_ns == 5_000_000_000
    assert hd["device"].node_ids == ["rtr-1"]
    assert hd["interface"].node_ids == ["rtr-1:eth0"]
    assert hd["bgp_neighbor"].node_ids == ["rtr-1:10.0.0.2"]
    assert hd["bfd_session"].node_ids == ["rtr-1:10.0.0.2"]
    assert hd["ospf_neighbor"].node_ids == ["rtr-1:2.2.2.2"]
    assert hd["redundancy_group"].node_ids == ["rg-lag-1"]
    assert hd["sensor_reading"].node_ids == ["rtr-1:cpu-temp"]
    assert hd["app_flow"].node_ids == ["rtr-1:flow-42"]

