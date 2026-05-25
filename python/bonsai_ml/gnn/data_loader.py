"""Synthetic-first GNN data loader for Bonsai graph snapshots.

The real chaos archive takes weeks to accumulate.  This module establishes the
loader contract now so training code can be developed against deterministic
fixtures, then pointed at Parquet + chaos-log inputs later.
"""
from __future__ import annotations

import csv
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import numpy as np


DEFAULT_FEATURE_NAMES = [
    # dim 0: topology
    "degree",
    # dims 1-6: vendor OHE
    "vendor_nokia",
    "vendor_cisco",
    "vendor_juniper",
    "vendor_arista",
    "vendor_frr",
    "vendor_other",
    # dims 7-18: role OHE
    "role_super_spine",
    "role_spine",
    "role_leaf",
    "role_pe",
    "role_p",
    "role_rr",
    "role_ce",
    "role_access",
    "role_distribution",
    "role_core",
    "role_edge",
    "role_other",
    # dims 19-22: spectral embedding (topology position)
    "embedding_0",
    "embedding_1",
    "embedding_2",
    "embedding_3",
    # dims 23-35: EV1-1 T6 expanded operational features
    "cpu_util_pct",
    "memory_used_pct",
    "uptime_log_secs",
    "has_thermal_warning",
    "bgp_session_count",
    "ospf_neighbor_count",
    "interface_count",
    "bmp_session_count",
    "gnn_quality_score",
    "model_enterprise",
    "model_cloud",
    "model_sp",
    "is_in_redundancy_group",
]

VENDOR_FEATURES = {
    "nokia": "vendor_nokia",
    "srlinux": "vendor_nokia",
    "srl": "vendor_nokia",
    "cisco": "vendor_cisco",
    "ios-xrd": "vendor_cisco",
    "iosxrd": "vendor_cisco",
    "juniper": "vendor_juniper",
    "crpd": "vendor_juniper",
    "vjunos": "vendor_juniper",
    "vjunos-evolved": "vendor_juniper",
    "arista": "vendor_arista",
    "ceos": "vendor_arista",
    "frr": "vendor_frr",
    "holo": "vendor_frr",
}

ROLE_FEATURES = {
    "super": "role_super_spine",
    "super-spine": "role_super_spine",
    "superspine": "role_super_spine",
    "spine": "role_spine",
    "leaf": "role_leaf",
    "pe": "role_pe",
    "provider-edge": "role_pe",
    "provider_edge": "role_pe",
    "p": "role_p",
    "provider": "role_p",
    "rr": "role_rr",
    "route-reflector": "role_rr",
    "route_reflector": "role_rr",
    "ce": "role_ce",
    "customer-edge": "role_ce",
    "customer_edge": "role_ce",
    "access": "role_access",
    "distribution": "role_distribution",
    "dist": "role_distribution",
    "core": "role_core",
    "edge": "role_edge",
    "campus-edge": "role_edge",
    "internet-edge": "role_edge",
}


@dataclass(slots=True)
class BonsaiGraphData:
    """Small, dependency-light equivalent of a PyTorch Geometric graph batch."""

    x: np.ndarray
    edge_index: np.ndarray
    y: np.ndarray
    node_ids: list[str]
    feature_names: list[str]
    edge_types: list[str]
    snapshot_ns: int
    metadata: dict[str, Any] = field(default_factory=dict)

    def validate(self) -> None:
        if self.x.ndim != 2:
            raise ValueError("x must be a 2D node-feature matrix")
        if self.edge_index.shape[0] != 2:
            raise ValueError("edge_index must have shape [2, num_edges]")
        if self.y.shape[0] != self.x.shape[0]:
            raise ValueError("y must contain one label per node")
        if len(self.node_ids) != self.x.shape[0]:
            raise ValueError("node_ids length must match node count")
        if len(self.feature_names) != self.x.shape[1]:
            raise ValueError("feature_names length must match feature dimension")

    def to_pyg(self) -> Any:
        """Return a torch_geometric.data.Data object when optional deps exist."""
        try:
            import torch
            from torch_geometric.data import Data
        except ImportError as exc:  # pragma: no cover - exercised by caller env
            raise RuntimeError(
                "PyTorch Geometric conversion requires optional dependencies: "
                "torch and torch-geometric"
            ) from exc

        return Data(
            x=torch.tensor(self.x, dtype=torch.float32),
            edge_index=torch.tensor(self.edge_index, dtype=torch.long),
            y=torch.tensor(self.y, dtype=torch.long),
            node_ids=self.node_ids,
            feature_names=self.feature_names,
            snapshot_ns=self.snapshot_ns,
            metadata=self.metadata,
        )


class BonsaiGnnDataLoader:
    """Build node-classification graph tensors from Bonsai-like snapshots."""

    def __init__(self, feature_names: list[str] | None = None) -> None:
        self.feature_names = feature_names or DEFAULT_FEATURE_NAMES[:]
        self._feature_index = {name: i for i, name in enumerate(self.feature_names)}

    @classmethod
    def from_api_snapshot(cls, snapshot_response: dict[str, Any], *, as_pyg: bool = False) -> Any:
        """Build graph tensors from a live /api/graph/snapshot API response.

        The snapshot endpoint returns a dict with keys: devices, links,
        snapshot_ns, and optionally interfaces, bgp_neighbors, bfd_sessions.
        This method normalises the response for use in STGNN inference.

        Args:
            snapshot_response: JSON dict from GET /api/graph/snapshot.
            as_pyg: If True, return a PyG HeteroData object instead of
                BonsaiGraphData (requires torch + torch-geometric).

        Returns:
            BonsaiGraphData or PyG HeteroData (same schema as from_snapshot).
        """
        loader = cls()
        devices_raw = snapshot_response.get("devices", [])
        links_raw = snapshot_response.get("links", [])
        snapshot_ns = int(snapshot_response.get("snapshot_ns", 0))

        for dev in devices_raw:
            if "cpu_util_pct" not in dev:
                dev["cpu_util_pct"] = None
            if "memory_used_pct" not in dev:
                mu = dev.get("memory_used_mb")
                mt = dev.get("memory_total_mb")
                dev["memory_used_pct"] = (
                    (float(mu) / float(mt) * 100.0)
                    if mu and mt and float(mt) > 0
                    else None
                )
            if "uptime_seconds" not in dev:
                dev["uptime_seconds"] = None

        snapshot = {
            "source": "api",
            "snapshot_ns": snapshot_ns,
            "devices": devices_raw,
            "links": links_raw,
            "chaos_log": [],
        }
        return loader.from_snapshot(snapshot, as_pyg=as_pyg)

    def from_snapshot(self, snapshot: dict[str, Any], *, as_pyg: bool = False) -> Any:
        """Convert a synthetic Bonsai snapshot into graph tensors.

        Expected snapshot keys:
          - snapshot_ns: timestamp for label extraction
          - devices: [{id/address/hostname, vendor, role, embedding?}, ...]
          - links: [{src_device, dst_device, type?}, ...]
          - chaos_log: optional labelled fault records
        """
        devices = _normalise_devices(snapshot.get("devices", []))
        links = snapshot.get("links", [])
        snapshot_ns = int(snapshot.get("snapshot_ns", 0))
        chaos_log = snapshot.get("chaos_log", [])

        node_ids = sorted(devices)
        node_index = {node_id: idx for idx, node_id in enumerate(node_ids)}
        degree = _degree_by_node(node_ids, links)
        labels = _labels_by_node(node_ids, chaos_log, snapshot_ns)

        x = np.zeros((len(node_ids), len(self.feature_names)), dtype=np.float32)
        y = np.zeros((len(node_ids),), dtype=np.int64)

        for node_id in node_ids:
            row = node_index[node_id]
            device = devices[node_id]
            self._set_feature(x, row, "degree", float(degree.get(node_id, 0)))
            self._set_one_hot(x, row, _vendor_feature(device.get("vendor")))
            self._set_one_hot(x, row, _role_feature(device.get("role") or node_id))

            for emb_idx, value in enumerate(device.get("embedding", [])[:4]):
                self._set_feature(x, row, f"embedding_{emb_idx}", float(value))

            cpu = device.get("cpu_util_pct")
            if cpu is not None:
                self._set_feature(x, row, "cpu_util_pct", float(cpu) / 100.0)

            mem_pct = device.get("memory_used_pct")
            if mem_pct is not None:
                self._set_feature(x, row, "memory_used_pct", float(mem_pct) / 100.0)
            else:
                mu = device.get("memory_used_mb")
                mt = device.get("memory_total_mb")
                if mu and mt and float(mt) > 0:
                    self._set_feature(x, row, "memory_used_pct", float(mu) / float(mt))

            uptime = device.get("uptime_seconds")
            if uptime and float(uptime) > 0:
                import math
                self._set_feature(x, row, "uptime_log_secs", math.log1p(float(uptime)))

            thermal = device.get("has_thermal_warning", False)
            self._set_feature(x, row, "has_thermal_warning", 1.0 if thermal else 0.0)

            for count_feat in (
                "bgp_session_count", "ospf_neighbor_count",
                "interface_count", "bmp_session_count",
            ):
                val = device.get(count_feat)
                if val is not None:
                    import math
                    self._set_feature(x, row, count_feat, math.log1p(float(val)))

            quality = device.get("gnn_quality_score")
            if quality is not None:
                self._set_feature(x, row, "gnn_quality_score", float(quality))

            hw_model = str(device.get("model") or "").lower()
            if any(t in hw_model for t in ("xe", "xr", "mx", "ex", "qfx", "catalyst")):
                self._set_feature(x, row, "model_enterprise", 1.0)
            elif any(t in hw_model for t in ("srl", "ceos", "cloud")):
                self._set_feature(x, row, "model_cloud", 1.0)
            elif any(t in hw_model for t in ("asr", "ncs", "mx", "ptx", "7750")):
                self._set_feature(x, row, "model_sp", 1.0)

            rg = device.get("is_in_redundancy_group", False)
            self._set_feature(x, row, "is_in_redundancy_group", 1.0 if rg else 0.0)

            y[row] = labels.get(node_id, 0)

        edge_index, edge_types = _edge_index(node_index, links)
        graph = BonsaiGraphData(
            x=x,
            edge_index=edge_index,
            y=y,
            node_ids=node_ids,
            feature_names=self.feature_names[:],
            edge_types=edge_types,
            snapshot_ns=snapshot_ns,
            metadata={
                "source": snapshot.get("source", "synthetic"),
                "num_faults": len(chaos_log),
            },
        )
        graph.validate()
        return graph.to_pyg() if as_pyg else graph

    def from_files(
        self,
        topology_json: dict[str, Any],
        chaos_csv: str | Path,
        *,
        snapshot_ns: int,
        as_pyg: bool = False,
    ) -> Any:
        """Build a graph from a topology payload and chaos-run CSV."""
        snapshot = {
            "source": "files",
            "snapshot_ns": snapshot_ns,
            "devices": topology_json.get("devices", []),
            "links": topology_json.get("links", []),
            "chaos_log": load_chaos_csv(chaos_csv),
        }
        return self.from_snapshot(snapshot, as_pyg=as_pyg)

    def _set_feature(self, x: np.ndarray, row: int, name: str, value: float) -> None:
        col = self._feature_index.get(name)
        if col is not None:
            x[row, col] = value

    def _set_one_hot(self, x: np.ndarray, row: int, name: str) -> None:
        self._set_feature(x, row, name, 1.0)


def load_chaos_csv(path: str | Path) -> list[dict[str, Any]]:
    """Load chaos_runner.py CSV records into typed dictionaries."""
    records: list[dict[str, Any]] = []
    with open(path, newline="", encoding="utf-8") as fh:
        for row in csv.DictReader(fh):
            for key in ("injected_at_ns", "healed_at_ns"):
                row[key] = int(row[key]) if row.get(key) else None
            records.append(dict(row))
    return records


def _normalise_devices(raw_devices: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    devices: dict[str, dict[str, Any]] = {}
    for device in raw_devices:
        node_id = (
            device.get("id")
            or device.get("address")
            or device.get("hostname")
            or device.get("name")
        )
        if not node_id:
            continue
        devices[str(node_id)] = dict(device)
    return devices


def _degree_by_node(node_ids: list[str], links: list[dict[str, Any]]) -> dict[str, int]:
    degree = {node_id: 0 for node_id in node_ids}
    for link in links:
        src = _link_endpoint(link, "src")
        dst = _link_endpoint(link, "dst")
        if src in degree and dst in degree and src != dst:
            degree[src] += 1
            degree[dst] += 1
    return degree


def _labels_by_node(
    node_ids: list[str],
    chaos_log: list[dict[str, Any]],
    snapshot_ns: int,
) -> dict[str, int]:
    labels = {node_id: 0 for node_id in node_ids}
    for fault in chaos_log:
        hostname = fault.get("hostname") or fault.get("target")
        injected = fault.get("injected_at_ns")
        healed = fault.get("healed_at_ns")
        if hostname not in labels or injected is None:
            continue
        end_ns = healed if healed is not None else snapshot_ns
        if int(injected) <= snapshot_ns <= int(end_ns):
            labels[str(hostname)] = 1
    return labels


def _edge_index(
    node_index: dict[str, int],
    links: list[dict[str, Any]],
) -> tuple[np.ndarray, list[str]]:
    edges: list[tuple[int, int]] = []
    edge_types: list[str] = []
    for link in links:
        src = _link_endpoint(link, "src")
        dst = _link_endpoint(link, "dst")
        if src not in node_index or dst not in node_index or src == dst:
            continue
        link_type = str(link.get("type") or link.get("edge_type") or "connected_to")
        edges.append((node_index[src], node_index[dst]))
        edges.append((node_index[dst], node_index[src]))
        edge_types.extend([link_type, link_type])

    if not edges:
        return np.zeros((2, 0), dtype=np.int64), []
    return np.array(edges, dtype=np.int64).T, edge_types


def _link_endpoint(link: dict[str, Any], side: str) -> str:
    return str(
        link.get(f"{side}_device")
        or link.get(f"{side}_id")
        or link.get(side)
        or ""
    )


def _vendor_feature(vendor: Any) -> str:
    key = str(vendor or "").lower()
    return VENDOR_FEATURES.get(key, "vendor_other")


def _role_feature(role: Any) -> str:
    text = str(role or "").lower().replace("_", "-").replace(" ", "-")
    tokens = [part for part in text.replace("/", "-").split("-") if part]
    token_set = set(tokens)
    token_roots = {token.rstrip("0123456789") for token in tokens}

    for key, feature in ROLE_FEATURES.items():
        normalised_key = key.replace("_", "-")
        if normalised_key == text or normalised_key in token_set or normalised_key in token_roots:
            return feature

    # Preserve the legacy hostname-derived inference for common DC names like
    # srl-leaf1 and srl-spine2 without letting one-letter SP roles overmatch.
    for key in ("super", "superspine", "spine", "leaf"):
        if key in text:
            return ROLE_FEATURES[key]

    return "role_other"
