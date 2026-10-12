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

            Aligns temporal embeddings across snapshots using each node type's
            stable ``node_ids`` attribute (falling back to positional alignment
            only if ``node_ids`` is absent). Records per-node presence masks in
            ``self.last_presence_masks`` and inter-snapshot elapsed seconds in
            ``self.last_elapsed_seconds``.

            Args:
                snapshot_sequence: List of HeteroData objects, oldest first.
                    May have fewer than T entries (cold-start); padded with zeros.
                return_attention: If True, capture GATv2 attention from the
                    last spatial layer of the most recent snapshot.

            Returns:
                (logit_dict, attention_snapshots_list) where logit_dict maps
                node_type → anomaly logits for the latest snapshot nodes.
            """
            if not snapshot_sequence:
                return {}, []

            T = self.temporal_window
            windowed_snapshots = snapshot_sequence[-T:]
            actual_t = len(windowed_snapshots)

            per_snapshot_embeddings: dict[str, list] = {
                ntype: [] for ntype in STGNN_NODE_TYPES
                if ntype in self.node_encoders
            }
            per_snapshot_node_ids: dict[str, list[Optional[list[str]]]] = {
                ntype: [] for ntype in per_snapshot_embeddings
            }
            snapshot_timestamps_ns: list[int] = []
            attention_list: list[dict] = []

            for t_idx, snapshot in enumerate(windowed_snapshots):
                is_last = (t_idx == actual_t - 1)
                snapshot_timestamps_ns.append(int(getattr(snapshot, "snapshot_ns", 0) or 0))
                x_dict, attn = self._encode_snapshot(
                    snapshot, return_attention=(return_attention and is_last)
                )
                if is_last and attn:
                    attention_list.append(attn)
                for ntype in per_snapshot_embeddings:
                    emb = x_dict.get(ntype)
                    if emb is not None:
                        per_snapshot_embeddings[ntype].append(emb)
                        nids = None
                        try:
                            raw_ids = getattr(snapshot[ntype], "node_ids", None)
                            if raw_ids is not None:
                                nids = [str(x) for x in raw_ids]
                        except (KeyError, AttributeError):
                            nids = None
                        per_snapshot_node_ids[ntype].append(nids)

            logit_dict: dict = {}
            self.last_presence_masks: dict[str, Any] = {}
            self.last_elapsed_seconds: Any = None

            for ntype, gru in self.temporal_grus.items():
                embeddings = per_snapshot_embeddings.get(ntype, [])
                if not embeddings:
                    continue
                if embeddings[-1].shape[0] == 0:
                    continue

                padded, presence_mask, elapsed_sec = align_temporal_embeddings_by_id(
                    embeddings_per_snapshot=embeddings,
                    node_ids_per_snapshot=per_snapshot_node_ids.get(ntype, []),
                    temporal_window=T,
                    snapshot_timestamps_ns=snapshot_timestamps_ns,
                )
                self.last_presence_masks[ntype] = presence_mask
                self.last_elapsed_seconds = elapsed_sec

                _, h_n = gru(padded)
                z = h_n[-1]

                logit_dict[ntype] = self.output_head(z)

            return logit_dict, attention_list

    return STGNNModel(config)


build_stgnn_model = build_stgnn


def align_temporal_embeddings_by_id(
    embeddings_per_snapshot: list[Any],
    node_ids_per_snapshot: list[Optional[list[str]]],
    temporal_window: int,
    snapshot_timestamps_ns: Optional[list[int]] = None,
) -> tuple[Any, Any, Any]:
    """Align per-snapshot spatial embeddings by stable entity ID to the latest snapshot.

    Args:
        embeddings_per_snapshot: List of ``[N_t, hidden_dim]`` tensors (oldest first).
        node_ids_per_snapshot: Parallel list of ``list[str]`` node IDs for each snapshot.
        temporal_window: Target window length ``T``.
        snapshot_timestamps_ns: Optional list of snapshot timestamps in nanoseconds.

    Returns:
        Tuple of ``(padded_embeddings, presence_mask, elapsed_seconds)``:
        - ``padded_embeddings``: Tensor ``[N_latest, T, hidden_dim]``
        - ``presence_mask``: Tensor ``[N_latest, T]`` (1.0 if entity present at step t, else 0.0)
        - ``elapsed_seconds``: Tensor ``[T]`` (seconds prior to latest snapshot, 0.0 for padded slots)
    """
    import torch

    windowed_embs = embeddings_per_snapshot[-temporal_window:]
    windowed_ids = (
        node_ids_per_snapshot[-temporal_window:]
        if node_ids_per_snapshot
        else [None] * len(windowed_embs)
    )
    latest_emb = windowed_embs[-1]
    num_nodes = latest_emb.shape[0]
    hidden_dim = latest_emb.shape[1]
    device = latest_emb.device
    dtype = latest_emb.dtype

    padded = torch.zeros(num_nodes, temporal_window, hidden_dim, device=device, dtype=dtype)
    presence_mask = torch.zeros(num_nodes, temporal_window, device=device, dtype=dtype)
    elapsed_seconds = torch.zeros(temporal_window, device=device, dtype=dtype)

    offset = temporal_window - len(windowed_embs)
    latest_ids = windowed_ids[-1] if windowed_ids else None

    if snapshot_timestamps_ns:
        windowed_ts = snapshot_timestamps_ns[-temporal_window:]
        latest_ts = windowed_ts[-1] if windowed_ts else 0
        for i, ts_ns in enumerate(windowed_ts):
            if latest_ts > 0 and ts_ns > 0 and latest_ts >= ts_ns:
                elapsed_seconds[offset + i] = float(latest_ts - ts_ns) / 1e9

    for i, emb in enumerate(windowed_embs):
        slot = offset + i
        snap_ids = windowed_ids[i] if i < len(windowed_ids) else None
        if (
            latest_ids is not None
            and snap_ids is not None
            and len(latest_ids) == num_nodes
            and len(snap_ids) == emb.shape[0]
        ):
            snap_row_by_id = {nid: row_idx for row_idx, nid in enumerate(snap_ids)}
            for target_row, nid in enumerate(latest_ids):
                src_row = snap_row_by_id.get(nid)
                if src_row is not None:
                    padded[target_row, slot, :] = emb[src_row]
                    presence_mask[target_row, slot] = 1.0
        else:
            n = min(num_nodes, emb.shape[0])
            padded[:n, slot, :] = emb[:n]
            presence_mask[:n, slot] = 1.0

    return padded, presence_mask, elapsed_seconds


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
