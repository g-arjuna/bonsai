"""Heterogeneous GNN model scaffold for Bonsai anomaly detection.

D5-T2 (DV1): Implements the heterogeneous GNN with GAT attention architecture
adopted in CV6 T4 (Xi et al. 2026). This is pre-work scaffolding — no training
runs yet. The model is ready for the first archive-depth-triggered training run
(expected DV2 or DV3, gate: ≥30 days archive, ≥500 injections, ≥50 examples
per active rule).

Node types: Device, Interface, BgpNeighbor, BfdSession
Edge types: has_interface, has_bgp_neighbor, has_bfd_session, connected_to

Dependency note: torch and torch-geometric are OPTIONAL. All imports are
deferred to method bodies so the module loads cleanly in environments where
only numpy is available (e.g. the laptop's dev environment).
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


NODE_TYPES = ("device", "interface", "bgp_neighbor", "bfd_session")
EDGE_TYPES = (
    ("device", "has_interface", "interface"),
    ("device", "has_bgp_neighbor", "bgp_neighbor"),
    ("device", "has_bfd_session", "bfd_session"),
    ("device", "connected_to", "device"),
)

DEFAULT_HIDDEN_CHANNELS = 64
DEFAULT_NUM_HEADS = 4
DEFAULT_NUM_LAYERS = 2
DEFAULT_DROPOUT = 0.1


@dataclass
class BonsaiGnnConfig:
    """Hyper-parameters for the heterogeneous GAT model."""

    hidden_channels: int = DEFAULT_HIDDEN_CHANNELS
    num_heads: int = DEFAULT_NUM_HEADS
    num_layers: int = DEFAULT_NUM_LAYERS
    dropout: float = DEFAULT_DROPOUT
    node_feature_dims: dict[str, int] = field(default_factory=lambda: {
        "device": 23,
        "interface": 8,
        "bgp_neighbor": 6,
        "bfd_session": 4,
    })
    output_classes: int = 2


def build_model(config: BonsaiGnnConfig | None = None) -> Any:
    """Construct the heterogeneous GAT model.

    Returns a ``torch.nn.Module`` (specifically a ``HeteroGNN`` instance) when
    PyTorch Geometric is available. Raises ``RuntimeError`` with a clear
    message when optional deps are missing.

    Args:
        config: Model hyper-parameters. Defaults to ``BonsaiGnnConfig()``.

    Returns:
        An untrained ``HeteroGNN`` model ready for ``.train()`` / ``.eval()``.
    """
    if config is None:
        config = BonsaiGnnConfig()
    try:
        import torch
        import torch.nn as nn
        from torch_geometric.nn import GATConv, HeteroConv, Linear
    except ImportError as exc:
        raise RuntimeError(
            "BonsaiGNN requires optional dependencies: torch and torch-geometric. "
            "Install with: pip install torch torch-geometric"
        ) from exc

    class HeteroGNN(nn.Module):
        def __init__(self, cfg: BonsaiGnnConfig) -> None:
            super().__init__()
            self.cfg = cfg

            self.node_encoders = nn.ModuleDict({
                node_type: Linear(in_dim, cfg.hidden_channels)
                for node_type, in_dim in cfg.node_feature_dims.items()
            })

            self.convs = nn.ModuleList()
            for _ in range(cfg.num_layers):
                conv = HeteroConv(
                    {
                        (src, rel, dst): GATConv(
                            cfg.hidden_channels,
                            cfg.hidden_channels // cfg.num_heads,
                            heads=cfg.num_heads,
                            dropout=cfg.dropout,
                            add_self_loops=False,
                        )
                        for src, rel, dst in EDGE_TYPES
                    },
                    aggr="sum",
                )
                self.convs.append(conv)

            self.output_head = nn.Sequential(
                nn.Dropout(cfg.dropout),
                Linear(cfg.hidden_channels, cfg.output_classes),
            )

            self.dropout = nn.Dropout(cfg.dropout)

        def forward(self, x_dict: dict, edge_index_dict: dict) -> dict:
            x_dict = {
                node_type: self.node_encoders[node_type](x)
                for node_type, x in x_dict.items()
            }

            for conv in self.convs:
                x_dict = conv(x_dict, edge_index_dict)
                x_dict = {
                    node_type: self.dropout(x.relu())
                    for node_type, x in x_dict.items()
                }

            return {
                node_type: self.output_head(x)
                for node_type, x in x_dict.items()
            }

    return HeteroGNN(config)


def build_hetero_data(snapshot: dict[str, Any]) -> Any:
    """Convert a Bonsai graph snapshot into a PyG HeteroData object.

    This is the bridge between ``BonsaiGnnDataLoader.from_snapshot()`` (which
    produces numpy arrays) and the heterogeneous model which needs typed node
    and edge tensors.

    Args:
        snapshot: A dict with keys ``devices``, ``links``, ``chaos_log``,
                  ``snapshot_ns`` — same schema as ``BonsaiGnnDataLoader``.

    Returns:
        A ``torch_geometric.data.HeteroData`` instance, or raises
        ``RuntimeError`` if torch-geometric is not installed.
    """
    try:
        import torch
        from torch_geometric.data import HeteroData
    except ImportError as exc:
        raise RuntimeError(
            "build_hetero_data requires torch and torch-geometric"
        ) from exc

    from .data_loader import (
        _degree_by_node,
        _labels_by_node,
        _normalise_devices,
        _vendor_feature,
        _role_feature,
        VENDOR_FEATURES,
        ROLE_FEATURES,
    )

    devices = _normalise_devices(snapshot.get("devices", []))
    links = snapshot.get("links", [])
    snapshot_ns = int(snapshot.get("snapshot_ns", 0))
    chaos_log = snapshot.get("chaos_log", [])

    node_ids = sorted(devices)
    node_index = {nid: idx for idx, nid in enumerate(node_ids)}
    degree = _degree_by_node(node_ids, links)
    labels = _labels_by_node(node_ids, chaos_log, snapshot_ns)

    import numpy as np

    num_nodes = len(node_ids)
    device_feature_dim = 23

    x_device = np.zeros((num_nodes, device_feature_dim), dtype=np.float32)
    y_device = np.zeros((num_nodes,), dtype=np.int64)

    for nid in node_ids:
        row = node_index[nid]
        d = devices[nid]
        x_device[row, 0] = float(degree.get(nid, 0))
        vendor_feat = _vendor_feature(d.get("vendor"))
        vendor_names = list(VENDOR_FEATURES.values())
        vendor_names_unique = list(dict.fromkeys(vendor_names))
        if vendor_feat in vendor_names_unique:
            x_device[row, 1 + vendor_names_unique.index(vendor_feat)] = 1.0
        role_feat = _role_feature(d.get("role") or nid)
        role_names = list(ROLE_FEATURES.values())
        role_names_unique = list(dict.fromkeys(role_names))
        if role_feat in role_names_unique:
            x_device[row, 7 + role_names_unique.index(role_feat)] = 1.0
        for emb_idx, val in enumerate(d.get("embedding", [])[:4]):
            x_device[row, 19 + emb_idx] = float(val)
        y_device[row] = labels.get(nid, 0)

    data = HeteroData()
    data["device"].x = torch.tensor(x_device, dtype=torch.float32)
    data["device"].y = torch.tensor(y_device, dtype=torch.long)
    data["device"].node_ids = node_ids

    for node_type, feat_dim in [
        ("interface", 8),
        ("bgp_neighbor", 6),
        ("bfd_session", 4),
    ]:
        data[node_type].x = torch.zeros((0, feat_dim), dtype=torch.float32)

    for src_type, rel, dst_type in EDGE_TYPES:
        if src_type == "device" and dst_type == "device":
            edges = []
            for link in links:
                src_key = str(
                    link.get("src_device") or link.get("src_id") or link.get("src") or ""
                )
                dst_key = str(
                    link.get("dst_device") or link.get("dst_id") or link.get("dst") or ""
                )
                if src_key in node_index and dst_key in node_index:
                    edges.append((node_index[src_key], node_index[dst_key]))
            if edges:
                t = torch.tensor(edges, dtype=torch.long).T
            else:
                t = torch.zeros((2, 0), dtype=torch.long)
            data[src_type, rel, dst_type].edge_index = t
        else:
            data[src_type, rel, dst_type].edge_index = torch.zeros((2, 0), dtype=torch.long)

    return data
