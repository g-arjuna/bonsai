"""Heterogeneous GNN model scaffold for Bonsai anomaly detection.

EV1-1 T4: Upgraded from GATConv (GAT v1) to GATv2Conv (Brody et al. 2021) to
eliminate rank collapse. Node types expanded from 4 to 8. Feature dimensions
updated per ADR adr_gnn_architecture_ev1.md. Added temporal_window and
gru_num_layers to BonsaiGnnConfig for STGNN (stgnn.py).

Node types: device, interface, bgp_neighbor, bfd_session,
            ospf_neighbor, redundancy_group, sensor_reading, app_flow
Edge types: has_interface, has_bgp_neighbor, has_bfd_session, connected_to,
            has_ospf_neighbor, member_of, carries_flow, has_sensor

Dependency note: torch and torch-geometric are OPTIONAL. All imports are
deferred to method bodies so the module loads cleanly in environments where
only numpy is available (e.g. the laptop's dev environment).
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


NODE_TYPES = (
    "device", "interface", "bgp_neighbor", "bfd_session",
    "ospf_neighbor", "redundancy_group", "sensor_reading", "app_flow",
)
EDGE_TYPES = (
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

DEFAULT_HIDDEN_CHANNELS = 64
DEFAULT_NUM_HEADS = 8
DEFAULT_NUM_LAYERS = 2
DEFAULT_DROPOUT = 0.1
DEFAULT_TEMPORAL_WINDOW = 8
DEFAULT_GRU_NUM_LAYERS = 1


@dataclass
class BonsaiGnnConfig:
    """Hyper-parameters for the heterogeneous GATv2 / STGNN model.

    node_feature_dims reflects EV1-1 T6 expanded feature set.
    temporal_window and gru_num_layers are used by STGNNModel in stgnn.py.
    """

    hidden_channels: int = DEFAULT_HIDDEN_CHANNELS
    num_heads: int = DEFAULT_NUM_HEADS
    num_layers: int = DEFAULT_NUM_LAYERS
    dropout: float = DEFAULT_DROPOUT
    temporal_window: int = DEFAULT_TEMPORAL_WINDOW
    gru_num_layers: int = DEFAULT_GRU_NUM_LAYERS
    node_feature_dims: dict[str, int] = field(default_factory=lambda: {
        "device": 36,
        "interface": 14,
        "bgp_neighbor": 12,
        "bfd_session": 10,
        "ospf_neighbor": 8,
        "redundancy_group": 6,
        "sensor_reading": 4,
        "app_flow": 6,
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
        from torch_geometric.nn import GATv2Conv, HeteroConv, Linear
    except ImportError as exc:
        raise RuntimeError(
            "BonsaiGNN requires optional dependencies: torch and torch-geometric. "
            "Install with: pip install torch torch-geometric"
        ) from exc

    class HeteroGNN(nn.Module):
        """Static spatial-only GATv2 model (baseline / ablation).

        For the full STGNN (temporal GRU over T snapshots), see stgnn.py.
        Uses GATv2Conv (Brody et al. 2021) to fix rank collapse vs GATv1.
        """

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
                        (src, rel, dst): GATv2Conv(
                            cfg.hidden_channels,
                            cfg.hidden_channels // cfg.num_heads,
                            heads=cfg.num_heads,
                            dropout=cfg.dropout,
                            add_self_loops=False,
                            share_weights=False,
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

    import math
    import numpy as np

    devices = _normalise_devices(snapshot.get("devices", []))
    links = snapshot.get("links", [])
    snapshot_ns = int(snapshot.get("snapshot_ns", 0))
    chaos_log = snapshot.get("chaos_log", [])

    # ── Device nodes (dim=36, EV1-1 T2) ──────────────────────────────────────
    node_ids = sorted(devices)
    node_index = {nid: idx for idx, nid in enumerate(node_ids)}
    degree = _degree_by_node(node_ids, links)
    labels = _labels_by_node(node_ids, chaos_log, snapshot_ns)

    num_nodes = len(node_ids)
    DEVICE_DIM = 36
    x_device = np.zeros((num_nodes, DEVICE_DIM), dtype=np.float32)
    y_device = np.zeros((num_nodes,), dtype=np.int64)

    vendor_names_unique = list(dict.fromkeys(VENDOR_FEATURES.values()))
    role_names_unique = list(dict.fromkeys(ROLE_FEATURES.values()))

    for nid in node_ids:
        row = node_index[nid]
        d = devices[nid]
        # dim 0: degree
        x_device[row, 0] = float(degree.get(nid, 0))
        # dims 1-6: vendor OHE
        vendor_feat = _vendor_feature(d.get("vendor"))
        if vendor_feat in vendor_names_unique:
            x_device[row, 1 + vendor_names_unique.index(vendor_feat)] = 1.0
        # dims 7-18: role OHE
        role_feat = _role_feature(d.get("role") or nid)
        if role_feat in role_names_unique:
            x_device[row, 7 + role_names_unique.index(role_feat)] = 1.0
        # dims 19-22: spectral embedding
        for emb_idx, val in enumerate(d.get("embedding", [])[:4]):
            x_device[row, 19 + emb_idx] = float(val)
        # dims 23-35: operational features
        cpu = d.get("cpu_util_pct")
        if cpu is not None:
            x_device[row, 23] = float(cpu) / 100.0
        mem_pct = d.get("memory_used_pct")
        if mem_pct is None:
            mu, mt = d.get("memory_used_mb"), d.get("memory_total_mb")
            if mu and mt and float(mt) > 0:
                mem_pct = float(mu) / float(mt) * 100.0
        if mem_pct is not None:
            x_device[row, 24] = float(mem_pct) / 100.0
        uptime = d.get("uptime_seconds")
        if uptime and float(uptime) > 0:
            x_device[row, 25] = math.log1p(float(uptime))
        x_device[row, 26] = 1.0 if d.get("has_thermal_warning") else 0.0
        for off, key in enumerate(("bgp_session_count", "ospf_neighbor_count",
                                   "interface_count", "bmp_session_count")):
            v = d.get(key)
            if v is not None:
                x_device[row, 27 + off] = math.log1p(float(v))
        q = d.get("gnn_quality_score")
        if q is not None:
            x_device[row, 31] = float(q)
        hw = str(d.get("model") or "").lower()
        if any(t in hw for t in ("xe", "xr", "mx", "ex", "qfx", "catalyst")):
            x_device[row, 32] = 1.0
        elif any(t in hw for t in ("srl", "ceos", "cloud")):
            x_device[row, 33] = 1.0
        elif any(t in hw for t in ("asr", "ncs", "ptx", "7750")):
            x_device[row, 34] = 1.0
        x_device[row, 35] = 1.0 if d.get("is_in_redundancy_group") else 0.0
        y_device[row] = labels.get(nid, 0)

    # ── Interface nodes (dim=14, EV1-1 T2) ───────────────────────────────────
    interfaces = snapshot.get("interfaces", [])
    IFACE_DIM = 14
    x_iface = np.zeros((len(interfaces), IFACE_DIM), dtype=np.float32)
    iface_to_device: list[tuple[int, int]] = []
    for k, iface in enumerate(interfaces):
        dev_addr = str(iface.get("device_address") or iface.get("device") or "")
        if dev_addr in node_index:
            iface_to_device.append((node_index[dev_addr], k))
        x_iface[k, 0] = float(iface.get("in_error_rate") or 0.0)
        x_iface[k, 1] = float(iface.get("out_error_rate") or 0.0)
        in_util = iface.get("in_utilization_pct") or iface.get("in_utilization")
        x_iface[k, 2] = float(in_util) / 100.0 if in_util is not None else 0.0
        out_util = iface.get("out_utilization_pct") or iface.get("out_utilization")
        x_iface[k, 3] = float(out_util) / 100.0 if out_util is not None else 0.0
        x_iface[k, 4] = 1.0 if iface.get("is_in_lag") else 0.0
        rx_dbm = iface.get("optical_rx_dbm")
        if rx_dbm is not None:
            x_iface[k, 5] = (float(rx_dbm) + 30.0) / 30.0
        speed = iface.get("speed_mbps") or iface.get("speed")
        if speed is not None:
            x_iface[k, 6] = math.log1p(float(speed))
        x_iface[k, 7] = 1.0 if str(iface.get("oper_state") or "").lower() == "up" else 0.0
        x_iface[k, 8] = 1.0 if iface.get("is_mgmt") else 0.0

    # ── BGP neighbor nodes (dim=12, EV1-1 T2) ────────────────────────────────
    bgp_sessions = snapshot.get("bgp_sessions", [])
    BGP_DIM = 12
    x_bgp = np.zeros((len(bgp_sessions), BGP_DIM), dtype=np.float32)
    bgp_to_device: list[tuple[int, int]] = []
    for k, sess in enumerate(bgp_sessions):
        dev_addr = str(sess.get("device_address") or sess.get("local_address") or "")
        if dev_addr in node_index:
            bgp_to_device.append((node_index[dev_addr], k))
        x_bgp[k, 0] = 1.0 if str(sess.get("session_state") or "").lower() == "established" else 0.0
        x_bgp[k, 1] = float(sess.get("received_prefix_count") or 0.0)
        x_bgp[k, 2] = float(sess.get("sent_prefix_count") or 0.0)
        x_bgp[k, 3] = math.log1p(float(sess.get("adj_rib_in_routes") or 0.0))
        x_bgp[k, 4] = math.log1p(float(sess.get("loc_rib_routes") or 0.0))
        x_bgp[k, 5] = math.log1p(float(sess.get("prefixes_rejected") or 0.0))
        hold = sess.get("hold_time") or sess.get("hold_timer")
        x_bgp[k, 6] = float(hold) if hold is not None else 90.0
        x_bgp[k, 7] = 1.0 if str(sess.get("peer_type") or "").lower() == "external" else 0.0
        uptime = sess.get("session_uptime_seconds") or sess.get("uptime_seconds")
        if uptime is not None:
            x_bgp[k, 8] = math.log1p(float(uptime))

    # ── BFD session nodes (dim=10, EV1-1 T2) ─────────────────────────────────
    bfd_sessions = snapshot.get("bfd_sessions", [])
    BFD_DIM = 10
    x_bfd = np.zeros((len(bfd_sessions), BFD_DIM), dtype=np.float32)
    bfd_to_device: list[tuple[int, int]] = []
    for k, sess in enumerate(bfd_sessions):
        dev_addr = str(sess.get("device_address") or "")
        if dev_addr in node_index:
            bfd_to_device.append((node_index[dev_addr], k))
        x_bfd[k, 0] = 1.0 if str(sess.get("state") or "").lower() == "up" else 0.0
        x_bfd[k, 1] = float(sess.get("detect_multiplier") or 3.0)
        interval = sess.get("interval_ms") or sess.get("min_rx_ms") or 300
        x_bfd[k, 2] = float(interval) / 1000.0
        x_bfd[k, 3] = math.log1p(float(sess.get("registered_protocols_count") or 1.0))
        x_bfd[k, 4] = 1.0 if sess.get("is_in_redundancy_path") else 0.0
        proto = str(sess.get("source_protocol") or "").lower()
        x_bfd[k, 5] = 1.0 if proto == "bgp" else 0.0
        x_bfd[k, 6] = 1.0 if proto == "ospf" else 0.0
        x_bfd[k, 7] = 1.0 if proto == "isis" else 0.0

    # ── OSPF neighbor nodes (dim=8, EV1-1 T2, NEW) ───────────────────────────
    ospf_neighbors = snapshot.get("ospf_neighbors", [])
    OSPF_DIM = 8
    x_ospf = np.zeros((len(ospf_neighbors), OSPF_DIM), dtype=np.float32)
    ospf_to_device: list[tuple[int, int]] = []
    for k, nbr in enumerate(ospf_neighbors):
        dev_addr = str(nbr.get("device_address") or "")
        if dev_addr in node_index:
            ospf_to_device.append((node_index[dev_addr], k))
        state = str(nbr.get("state") or "").lower()
        x_ospf[k, 0] = 1.0 if state == "full" else (0.5 if state in ("2way", "exstart", "exchange", "loading") else 0.0)
        x_ospf[k, 1] = float(nbr.get("metric") or 0.0) / 1000.0
        x_ospf[k, 2] = 1.0 if nbr.get("is_dr") else 0.0
        x_ospf[k, 3] = 1.0 if nbr.get("is_bdr") else 0.0
        uptime = nbr.get("uptime_seconds")
        if uptime:
            x_ospf[k, 4] = math.log1p(float(uptime))
        x_ospf[k, 5] = float(nbr.get("dead_interval") or 40.0) / 40.0
        x_ospf[k, 6] = float(nbr.get("priority") or 1.0) / 255.0
        x_ospf[k, 7] = math.log1p(float(nbr.get("retransmit_count") or 0.0))

    # ── RedundancyGroup nodes (dim=6, EV1-1 T2, NEW) ─────────────────────────
    redundancy_groups = snapshot.get("redundancy_groups", [])
    RG_DIM = 6
    x_rg = np.zeros((len(redundancy_groups), RG_DIM), dtype=np.float32)
    rg_device_edges: list[tuple[int, int]] = []
    for k, rg in enumerate(redundancy_groups):
        rg_type = str(rg.get("group_type") or "").lower()
        x_rg[k, 0] = 1.0 if rg_type == "lag" else 0.0
        x_rg[k, 1] = 1.0 if rg_type == "vrrp" else 0.0
        x_rg[k, 2] = 1.0 if rg_type == "hsrp" else 0.0
        x_rg[k, 3] = 1.0 if rg_type == "ecmp" else 0.0
        x_rg[k, 4] = math.log1p(float(rg.get("member_count") or 0.0))
        x_rg[k, 5] = 1.0 if rg.get("all_members_up") else 0.0
        for dev_addr in rg.get("member_addresses", []):
            if str(dev_addr) in node_index:
                rg_device_edges.append((node_index[str(dev_addr)], k))

    # ── SensorReading nodes (dim=4, EV1-1 T2, NEW) ───────────────────────────
    sensors = snapshot.get("sensors", [])
    SENSOR_DIM = 4
    x_sensor = np.zeros((len(sensors), SENSOR_DIM), dtype=np.float32)
    sensor_to_device: list[tuple[int, int]] = []
    for k, s in enumerate(sensors):
        dev_addr = str(s.get("device_address") or "")
        if dev_addr in node_index:
            sensor_to_device.append((node_index[dev_addr], k))
        temp = s.get("temperature_celsius")
        if temp is not None:
            x_sensor[k, 0] = (float(temp) - 25.0) / 75.0
        stype = str(s.get("sensor_type") or "").lower()
        x_sensor[k, 1] = 1.0 if stype == "temperature" else 0.0
        x_sensor[k, 2] = 1.0 if s.get("above_warning") else 0.0
        x_sensor[k, 3] = 1.0 if s.get("above_critical") else 0.0

    # ── AppFlow nodes (dim=6, EV1-1 T2) ──────────────────────────────────────
    app_flows = snapshot.get("app_flows", [])
    AF_DIM = 6
    x_af = np.zeros((len(app_flows), AF_DIM), dtype=np.float32)
    af_to_device: list[tuple[int, int]] = []
    for k, af in enumerate(app_flows):
        dev_addr = str(af.get("device_address") or "")
        if dev_addr in node_index:
            af_to_device.append((node_index[dev_addr], k))
        x_af[k, 0] = math.log1p(float(af.get("bytes_per_sec") or 0.0))
        x_af[k, 1] = math.log1p(float(af.get("packets_per_sec") or 0.0))
        x_af[k, 2] = float(af.get("flow_count") or 0.0)
        proto = str(af.get("protocol") or "").lower()
        x_af[k, 3] = 1.0 if proto == "tcp" else 0.0
        x_af[k, 4] = 1.0 if proto == "udp" else 0.0

    # ── Assemble HeteroData ───────────────────────────────────────────────────
    data = HeteroData()
    data["device"].x = torch.tensor(x_device, dtype=torch.float32)
    data["device"].y = torch.tensor(y_device, dtype=torch.long)
    data["device"].node_ids = node_ids

    data["interface"].x = torch.tensor(x_iface, dtype=torch.float32)
    data["bgp_neighbor"].x = torch.tensor(x_bgp, dtype=torch.float32)
    data["bfd_session"].x = torch.tensor(x_bfd, dtype=torch.float32)
    data["ospf_neighbor"].x = torch.tensor(x_ospf, dtype=torch.float32)
    data["redundancy_group"].x = torch.tensor(x_rg, dtype=torch.float32)
    data["sensor_reading"].x = torch.tensor(x_sensor, dtype=torch.float32)
    data["app_flow"].x = torch.tensor(x_af, dtype=torch.float32)

    def _make_edges(pairs: list[tuple[int, int]]) -> "torch.Tensor":
        if not pairs:
            return torch.zeros((2, 0), dtype=torch.long)
        return torch.tensor(pairs, dtype=torch.long).T

    # Device-device physical links
    dev_dev_edges = []
    for link in links:
        src_key = str(link.get("src_device") or link.get("src_id") or link.get("src") or "")
        dst_key = str(link.get("dst_device") or link.get("dst_id") or link.get("dst") or "")
        if src_key in node_index and dst_key in node_index:
            dev_dev_edges.append((node_index[src_key], node_index[dst_key]))

    data["device", "connected_to", "device"].edge_index = _make_edges(dev_dev_edges)
    data["device", "has_interface", "interface"].edge_index = _make_edges(iface_to_device)
    data["device", "has_bgp_neighbor", "bgp_neighbor"].edge_index = _make_edges(bgp_to_device)
    data["device", "has_bfd_session", "bfd_session"].edge_index = _make_edges(bfd_to_device)
    data["device", "has_ospf_neighbor", "ospf_neighbor"].edge_index = _make_edges(ospf_to_device)
    data["device", "member_of", "redundancy_group"].edge_index = _make_edges(rg_device_edges)
    data["device", "has_sensor", "sensor_reading"].edge_index = _make_edges(sensor_to_device)
    data["device", "carries_flow", "app_flow"].edge_index = _make_edges(af_to_device)
    data["interface", "member_of", "redundancy_group"].edge_index = torch.zeros((2, 0), dtype=torch.long)

    return data
