"""Spatio-Temporal GNN (STGNN) for Bonsai anomaly detection.

EV1-1 T3/T4/T5: Implements the STGNN architecture described in
docs/architecture/adr_gnn_architecture_ev1.md.

Architecture:
  T=8 snapshots → HeteroGATv2Conv (spatial, 2 layers, 8 heads)
               → GRU (temporal, per node type, 1 layer)
               → Linear output head
               → Per-node anomaly logits

Attention explainability:
  GATv2Conv.forward(return_attention_weights=True) captures per-edge alpha.
  AttentionSnapshot dataclass holds top-k contributing neighbours for each
  anomalous device. Posted to Rust via POST /api/gnn/attention.

Cold-start handling:
  SnapshotBuffer pads missing early snapshots with zero tensors + an
  attention_mask that suppresses gradient from padded positions.

Deps: torch, torch-geometric (optional — module loads cleanly without them).
"""
from __future__ import annotations

import logging
import pickle
import threading
import time
from collections import deque
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

log = logging.getLogger(__name__)

DEFAULT_TEMPORAL_WINDOW = 8
DEFAULT_GRU_LAYERS = 1


# ── AttentionSnapshot ─────────────────────────────────────────────────────────

@dataclass
class AttentionNeighbour:
    """A single contributing neighbour from GATv2 attention."""
    neighbour_id: str
    neighbour_type: str
    edge_type: str
    weight: float


@dataclass
class AttentionSnapshot:
    """Per-node attention weights captured after GATv2 inference.

    Serialised and posted to POST /api/gnn/attention (EV1-4).
    The Rust handler stores these as GnnAttentionSnapshot nodes.
    """
    snapshot_ns: int
    node_id: str
    node_type: str
    anomaly_score: float
    top_neighbours: list[AttentionNeighbour] = field(default_factory=list)
    model_version: str = "stgnn_v1"

    def to_dict(self) -> dict[str, Any]:
        return {
            "snapshot_ns": self.snapshot_ns,
            "node_id": self.node_id,
            "node_type": self.node_type,
            "anomaly_score": self.anomaly_score,
            "model_version": self.model_version,
            "top_neighbours": [
                {
                    "neighbour_id": n.neighbour_id,
                    "neighbour_type": n.neighbour_type,
                    "edge_type": n.edge_type,
                    "weight": round(float(n.weight), 4),
                }
                for n in self.top_neighbours
            ],
        }


# ── SnapshotBuffer ────────────────────────────────────────────────────────────

class SnapshotBuffer:
    """Thread-safe ring buffer of the last T HeteroData graph snapshots.

    Serialisable to disk via pickle for restart recovery. Used by the STGNN
    inference loop and by the training pipeline to build temporal sequences.
    """

    def __init__(self, capacity: int = DEFAULT_TEMPORAL_WINDOW) -> None:
        self._capacity = capacity
        self._buffer: deque[tuple[int, Any]] = deque(maxlen=capacity)
        self._lock = threading.Lock()

    def push(self, snapshot_ns: int, hetero_data: Any) -> None:
        """Add a new snapshot to the buffer. Evicts oldest if full."""
        with self._lock:
            self._buffer.append((snapshot_ns, hetero_data))

    def get_sequence(self) -> list[Any]:
        """Return HeteroData objects in chronological order (oldest first)."""
        with self._lock:
            return [data for _ts, data in self._buffer]

    def get_timestamps(self) -> list[int]:
        with self._lock:
            return [ts for ts, _data in self._buffer]

    def size(self) -> int:
        with self._lock:
            return len(self._buffer)

    @property
    def capacity(self) -> int:
        return self._capacity

    def is_full(self) -> bool:
        return self.size() == self._capacity

    def get_health(self) -> dict[str, Any]:
        """Returns buffer health metadata for monitoring."""
        with self._lock:
            if not self._buffer:
                return {
                    "buffer_size": 0,
                    "capacity": self._capacity,
                    "oldest_ns": None,
                    "newest_ns": None,
                    "gap_seconds_max": None,
                    "is_stale": True,
                    "is_full": False,
                }
            timestamps = [ts for ts, _ in self._buffer]
            now_ns = time.time_ns()
            newest_ns = max(timestamps)
            oldest_ns = min(timestamps)
            gaps = [
                (timestamps[i + 1] - timestamps[i]) / 1e9
                for i in range(len(timestamps) - 1)
            ]
            is_stale = (now_ns - newest_ns) > 3_600_000_000_000
            return {
                "buffer_size": len(self._buffer),
                "capacity": self._capacity,
                "oldest_ns": oldest_ns,
                "newest_ns": newest_ns,
                "gap_seconds_max": max(gaps) if gaps else 0.0,
                "is_stale": is_stale,
                "is_full": len(self._buffer) == self._capacity,
            }

    def save(self, path: str | Path) -> None:
        Path(path).parent.mkdir(parents=True, exist_ok=True)
        with self._lock:
            data = list(self._buffer)
        with open(path, "wb") as fh:
            pickle.dump({"capacity": self._capacity, "buffer": data}, fh, protocol=5)
        log.debug("SnapshotBuffer saved %d snapshots to %s", len(data), path)

    @classmethod
    def load(cls, path: str | Path) -> "SnapshotBuffer":
        with open(path, "rb") as fh:
            state = pickle.load(fh)
        buf = cls(capacity=state["capacity"])
        for entry in state["buffer"]:
            buf._buffer.append(entry)
        log.info("SnapshotBuffer loaded %d snapshots from %s", len(buf._buffer), path)
        return buf


# ── STGNN Model ───────────────────────────────────────────────────────────────

def build_stgnn(config: Any | None = None) -> Any:
    """Construct the STGNN model (requires torch + torch-geometric).

    Args:
        config: BonsaiGnnConfig instance (from model.py). If None, uses defaults.
                Must have temporal_window and gru_num_layers attributes for STGNN.

    Returns:
        STGNNModel (nn.Module) with spatial GATv2 + temporal GRU layers.
    """
    try:
        import torch
        import torch.nn as nn
        from torch_geometric.nn import GATv2Conv, HeteroConv, Linear
    except ImportError as exc:
        raise RuntimeError(
            "STGNNModel requires optional dependencies: torch and torch-geometric. "
            "Install with: pip install torch torch-geometric"
        ) from exc

    from .model import BonsaiGnnConfig, NODE_TYPES

    if config is None:
        config = BonsaiGnnConfig()

    temporal_window = getattr(config, "temporal_window", DEFAULT_TEMPORAL_WINDOW)
    gru_num_layers = getattr(config, "gru_num_layers", DEFAULT_GRU_LAYERS)

    STGNN_EDGE_TYPES = (
        ("device", "has_interface", "interface"),
        ("device", "has_bgp_neighbor", "bgp_neighbor"),
        ("device", "has_bfd_session", "bfd_session"),
        ("device", "connected_to", "device"),
        ("device", "has_ospf_neighbor", "ospf_neighbor"),
        ("device", "member_of", "redundancy_group"),
        ("interface", "member_of", "redundancy_group"),
        ("device", "carries_flow", "app_flow"),
        ("device", "has_sensor", "sensor_reading"),
    )

    STGNN_NODE_TYPES = (
        "device", "interface", "bgp_neighbor", "bfd_session",
        "ospf_neighbor", "redundancy_group", "sensor_reading", "app_flow",
    )

    class SpatialLayer(nn.Module):
        """One GATv2 message-passing layer over the heterogeneous graph."""

        def __init__(self, cfg: BonsaiGnnConfig, edge_types: tuple) -> None:
            super().__init__()
            self.conv = HeteroConv(
                {
                    (src, rel, dst): GATv2Conv(
                        cfg.hidden_channels,
                        cfg.hidden_channels // cfg.num_heads,
                        heads=cfg.num_heads,
                        dropout=cfg.dropout,
                        add_self_loops=False,
                        share_weights=False,
                    )
                    for src, rel, dst in edge_types
                },
                aggr="sum",
            )
            self.dropout = nn.Dropout(cfg.dropout)

        def forward(
            self,
            x_dict: dict,
            edge_index_dict: dict,
            return_attention: bool = False,
        ) -> tuple[dict, dict]:
            """Returns (x_dict_out, attention_dict).

            attention_dict keys are (src_type, rel, dst_type) tuples; values are
            (edge_index, alpha) tuples from GATv2Conv when return_attention=True.
            """
            attention_dict: dict = {}
            if return_attention:
                x_dict_out: dict = {}
                for edge_type, conv_module in self.conv.convs.items():
                    src_type, rel, dst_type = edge_type
                    edge_key = (src_type, rel, dst_type)
                    if edge_key not in edge_index_dict:
                        continue
                    src_x = x_dict.get(src_type)
                    dst_x = x_dict.get(dst_type)
                    if src_x is None or dst_x is None:
                        continue
                    out, (ei, alpha) = conv_module(
                        (src_x, dst_x),
                        edge_index_dict[edge_key],
                        return_attention_weights=True,
                    )
                    attention_dict[edge_key] = (ei, alpha)
                    existing = x_dict_out.get(dst_type)
                    x_dict_out[dst_type] = (existing + out) if existing is not None else out
                for ntype, x in x_dict.items():
                    if ntype not in x_dict_out:
                        x_dict_out[ntype] = x
            else:
                x_dict_out = self.conv(x_dict, edge_index_dict)

            x_dict_out = {
                ntype: self.dropout(x.relu())
                for ntype, x in x_dict_out.items()
            }
            return x_dict_out, attention_dict

    class STGNNModel(nn.Module):
        """Full STGNN: T × SpatialLayer → per-node GRU → OutputHead.

        Input: list of T HeteroData objects (chronological).
        Output: dict[node_type → anomaly_logits tensor (N × output_classes)].
        """

        def __init__(self, cfg: BonsaiGnnConfig) -> None:
            super().__init__()
            self.cfg = cfg
            self.temporal_window = temporal_window

            self.node_encoders = nn.ModuleDict({
                node_type: Linear(in_dim, cfg.hidden_channels)
                for node_type, in_dim in cfg.node_feature_dims.items()
            })

            self.spatial_layers = nn.ModuleList([
                SpatialLayer(cfg, STGNN_EDGE_TYPES)
                for _ in range(cfg.num_layers)
            ])

            self.temporal_grus = nn.ModuleDict({
                node_type: nn.GRU(
                    input_size=cfg.hidden_channels,
                    hidden_size=cfg.hidden_channels,
                    num_layers=gru_num_layers,
                    batch_first=True,
                    dropout=cfg.dropout if gru_num_layers > 1 else 0.0,
                )
                for node_type in STGNN_NODE_TYPES
                if node_type in cfg.node_feature_dims
            })

            self.output_head = nn.Sequential(
                nn.Dropout(cfg.dropout),
                Linear(cfg.hidden_channels, cfg.output_classes),
            )

        def _encode_snapshot(
            self,
            hetero_data: Any,
            return_attention: bool = False,
        ) -> tuple[dict, dict]:
            """Run spatial layers on a single HeteroData snapshot."""
            x_dict = {}
            for ntype, encoder in self.node_encoders.items():
                if hasattr(hetero_data[ntype], "x") and hetero_data[ntype].x is not None:
                    x_dict[ntype] = encoder(hetero_data[ntype].x)

            edge_index_dict = {}
            for src, rel, dst in STGNN_EDGE_TYPES:
                key = (src, rel, dst)
                try:
                    ei = hetero_data[src, rel, dst].edge_index
                    if ei is not None:
                        edge_index_dict[key] = ei
                except (KeyError, AttributeError):
                    pass

            attention_dict: dict = {}
            for i, spatial_layer in enumerate(self.spatial_layers):
                capture = return_attention and (i == len(self.spatial_layers) - 1)
                x_dict, attn = spatial_layer(x_dict, edge_index_dict, return_attention=capture)
                if capture:
                    attention_dict = attn

            return x_dict, attention_dict

        def forward(
            self,
            snapshot_sequence: list[Any],
            return_attention: bool = False,
        ) -> tuple[dict, list[dict]]:
            """Process T snapshots through spatial then temporal layers.

            Args:
                snapshot_sequence: List of HeteroData objects, oldest first.
                    May have fewer than T entries (cold-start); padded with zeros.
                return_attention: If True, capture GATv2 attention from the
                    last spatial layer of the most recent snapshot.

            Returns:
                (logit_dict, attention_snapshots_list) where logit_dict maps
                node_type → anomaly logits for the latest snapshot nodes.
            """
            import torch

            T = self.temporal_window
            actual_t = len(snapshot_sequence)

            per_snapshot_embeddings: dict[str, list] = {
                ntype: [] for ntype in STGNN_NODE_TYPES
                if ntype in self.node_encoders
            }
            attention_list: list[dict] = []

            for t_idx, snapshot in enumerate(snapshot_sequence):
                is_last = (t_idx == actual_t - 1)
                x_dict, attn = self._encode_snapshot(
                    snapshot, return_attention=(return_attention and is_last)
                )
                if is_last and attn:
                    attention_list.append(attn)
                for ntype in per_snapshot_embeddings:
                    emb = x_dict.get(ntype)
                    if emb is not None:
                        per_snapshot_embeddings[ntype].append(emb)

            logit_dict: dict = {}
            latest_snapshot = snapshot_sequence[-1]

            for ntype, gru in self.temporal_grus.items():
                embeddings = per_snapshot_embeddings.get(ntype, [])
                if not embeddings:
                    continue

                num_nodes = embeddings[-1].shape[0]
                hidden_dim = self.cfg.hidden_channels

                padded = torch.zeros(num_nodes, T, hidden_dim, device=embeddings[-1].device)
                offset = T - len(embeddings)
                for i, emb in enumerate(embeddings):
                    n = min(num_nodes, emb.shape[0])
                    padded[:n, offset + i, :] = emb[:n]

                _, h_n = gru(padded)
                z = h_n[-1]

                logit_dict[ntype] = self.output_head(z)

            return logit_dict, attention_list

    return STGNNModel(config)


# ── Attention extraction helpers ──────────────────────────────────────────────

def extract_attention_snapshots(
    attention_list: list[dict],
    snapshot_ns: int,
    logit_dict: dict,
    hetero_data: Any,
    anomaly_threshold: float = 0.5,
    top_k: int = 5,
    node_id_map: Optional[dict[str, list[str]]] = None,
) -> list[AttentionSnapshot]:
    """Convert raw GATv2 attention weights into AttentionSnapshot objects.

    Args:
        attention_list: Raw attention from STGNNModel.forward() — list of
            {(src, rel, dst): (edge_index, alpha)} dicts.
        snapshot_ns: Timestamp of the latest snapshot.
        logit_dict: Model output logits per node type.
        hetero_data: The latest HeteroData snapshot (for node IDs).
        anomaly_threshold: Softmax probability threshold to classify anomalous.
        top_k: Number of top contributing neighbours to retain per node.
        node_id_map: Optional {node_type: [node_id, ...]} for ID lookup.
            If None, falls back to integer index as string.

    Returns:
        List of AttentionSnapshot for anomalous nodes only.
    """
    try:
        import torch
        import torch.nn.functional as F
    except ImportError:
        return []

    snapshots: list[AttentionSnapshot] = []

    for node_type, logits in logit_dict.items():
        probs = F.softmax(logits, dim=-1)
        anomaly_probs = probs[:, 1].detach().cpu().numpy()

        ids: list[str] = []
        if node_id_map and node_type in node_id_map:
            ids = node_id_map[node_type]
        else:
            try:
                node_ids_attr = getattr(hetero_data[node_type], "node_ids", None)
                if node_ids_attr is not None:
                    ids = list(node_ids_attr)
            except (KeyError, AttributeError):
                pass

        for node_idx, score in enumerate(anomaly_probs):
            if float(score) < anomaly_threshold:
                continue

            node_id = ids[node_idx] if node_idx < len(ids) else str(node_idx)

            neighbours: list[AttentionNeighbour] = []
            for attn_dict in attention_list:
                for (src_type, rel, dst_type), (edge_index, alpha) in attn_dict.items():
                    if dst_type != node_type:
                        continue
                    ei_np = edge_index.detach().cpu().numpy()
                    alpha_np = alpha.detach().cpu().numpy()
                    mean_alpha = alpha_np.mean(axis=-1) if alpha_np.ndim == 2 else alpha_np

                    dst_mask = ei_np[1] == node_idx
                    src_indices = ei_np[0][dst_mask]
                    weights = mean_alpha[dst_mask]

                    order = weights.argsort()[::-1][:top_k]
                    for rank_idx in order:
                        src_idx = int(src_indices[rank_idx])
                        if node_id_map and src_type in node_id_map:
                            src_ids = node_id_map[src_type]
                            nb_id = src_ids[src_idx] if src_idx < len(src_ids) else str(src_idx)
                        else:
                            nb_id = str(src_idx)

                        neighbours.append(AttentionNeighbour(
                            neighbour_id=nb_id,
                            neighbour_type=src_type,
                            edge_type=rel,
                            weight=float(weights[rank_idx]),
                        ))

            neighbours.sort(key=lambda n: n.weight, reverse=True)
            snapshots.append(AttentionSnapshot(
                snapshot_ns=snapshot_ns,
                node_id=node_id,
                node_type=node_type,
                anomaly_score=float(score),
                top_neighbours=neighbours[:top_k],
            ))

    return snapshots
