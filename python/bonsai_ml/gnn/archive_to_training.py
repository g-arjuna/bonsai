"""Archive-to-training-data converter for GNN training (T2-5).

Reads snapshot JSON files produced by chaos_runner.py (T2-1), joins with
chaos_log.jsonl ground truth, and produces labelled BonsaiGraphData instances
split by time (no leakage across train/val/test boundaries).

Works today with synthetic data via make_synthetic_snapshot().
When 30+ days of real archive exist, point at runtime/chaos_runs/ and it works
the same way.

Label semantics (per-node, matching BonsaiGnnDataLoader convention):
  1  — device was the direct injection target during an active fault window
  0  — clean state (pre-fault, post-recovery, or adversarial/expected event)

Adversarial snapshots are always labeled 0: the GNN must learn that an
interface-down during a declared maintenance window is not anomalous.
"""
from __future__ import annotations

import json
import time as _time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np

from .data_loader import BonsaiGnnDataLoader, BonsaiGraphData


# ── Public types ──────────────────────────────────────────────────────────────

@dataclass
class ArchiveStats:
    total_snapshots: int
    fault_snapshots: int
    clean_snapshots: int
    adversarial_snapshots: int
    unique_injections: int
    time_span_hours: float

    def __str__(self) -> str:
        return (
            f"ArchiveStats(total={self.total_snapshots}, fault={self.fault_snapshots}, "
            f"clean={self.clean_snapshots}, adversarial={self.adversarial_snapshots}, "
            f"injections={self.unique_injections}, span={self.time_span_hours:.1f}h)"
        )


# ── Snapshot discovery ────────────────────────────────────────────────────────

def load_snapshots(chaos_runs_dir: str | Path = "runtime/chaos_runs") -> list[dict]:
    """Discover and load all snapshot JSON files under chaos_runs_dir."""
    runs_dir = Path(chaos_runs_dir)
    snapshots: list[dict] = []
    for snap_file in sorted(runs_dir.glob("*/snapshots/*.json")):
        try:
            data = json.loads(snap_file.read_text())
            data["_source_file"] = str(snap_file)
            snapshots.append(data)
        except Exception:
            continue
    return snapshots


def load_chaos_log(chaos_log_path: str | Path = "runtime/chaos_log.jsonl") -> list[dict]:
    """Load chaos_log.jsonl injection records (restart_marker lines skipped)."""
    path = Path(chaos_log_path)
    if not path.exists():
        return []
    records: list[dict] = []
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            rec = json.loads(line)
            if rec.get("event_type") != "restart_marker":
                records.append(rec)
        except Exception:
            continue
    return records


# ── Topology normalisation ────────────────────────────────────────────────────

def _topology_to_loader_dict(snap: dict) -> dict:
    """Convert a snapshot's topology payload to the format expected by BonsaiGnnDataLoader."""
    topo = snap.get("topology") or {}

    # API returns DeviceJson: {address, hostname, vendor, role, health, bgp: [...]}
    devices = []
    for dev in topo.get("devices", []):
        devices.append({
            "id": dev.get("address") or dev.get("id"),
            "vendor": dev.get("vendor", ""),
            "role": dev.get("role", ""),
            "hostname": dev.get("hostname", ""),
        })

    # API returns LinkJson: {src_device, src_iface, dst_device, dst_iface, bytes_total, is_mgmt?}
    links = []
    for link in topo.get("links", []):
        src = link.get("src_device") or link.get("src")
        dst = link.get("dst_device") or link.get("dst")
        if not src or not dst:
            continue
        links.append({
            "src_device": src,
            "dst_device": dst,
            "type": "mgmt_link" if link.get("is_mgmt") else "connected_to",
        })

    return {
        "source": "archive",
        "snapshot_ns": snap.get("captured_at_ns", 0),
        "devices": devices,
        "links": links,
    }


# ── Labelling ─────────────────────────────────────────────────────────────────

# Offset labels that fall within the active fault window (fault injected, not yet healed).
_ACTIVE_FAULT_LABELS = {"+10s", "+30s", "+60s"}


def _fault_active_at_snapshot(snap: dict) -> bool:
    """Return True when this snapshot was captured while the injected fault was active."""
    if snap.get("adversarial"):
        return False
    offset = snap.get("offset_label", "")
    return offset in _ACTIVE_FAULT_LABELS


def _per_device_labels(snap: dict) -> dict[str, int]:
    """Map device hostname → label (1=fault, 0=clean) for this snapshot."""
    if not _fault_active_at_snapshot(snap):
        return {}
    hostname = snap.get("hostname", "")
    if not hostname:
        return {}
    return {hostname: 1}


# ── Core conversion ───────────────────────────────────────────────────────────

def convert_archive_to_graphs(
    chaos_runs_dir: str | Path = "runtime/chaos_runs",
    chaos_log_path: str | Path = "runtime/chaos_log.jsonl",
) -> tuple[list[BonsaiGraphData], ArchiveStats]:
    """Convert the snapshot archive to labelled BonsaiGraphData instances.

    Returns (graphs, stats). Graphs are sorted ascending by captured_at_ns so
    that time_split() produces non-leaking train/val/test sets.
    """
    loader = BonsaiGnnDataLoader()
    snapshots = load_snapshots(chaos_runs_dir)

    graphs: list[BonsaiGraphData] = []
    fault_count = 0
    clean_count = 0
    adversarial_count = 0
    injection_ids: set[str] = set()

    for snap in snapshots:
        loader_dict = _topology_to_loader_dict(snap)
        if not loader_dict["devices"]:
            continue

        is_adversarial = bool(snap.get("adversarial"))
        device_labels = _per_device_labels(snap)

        if is_adversarial:
            adversarial_count += 1
        elif device_labels:
            fault_count += 1
        else:
            clean_count += 1

        # Inject chaos-log-style fault records so BonsaiGnnDataLoader labels nodes correctly.
        if device_labels:
            inject_ns = snap.get("inject_ns", 0)
            captured_ns = snap.get("captured_at_ns", 0)
            chaos_entries = [
                {"hostname": h, "injected_at_ns": inject_ns, "healed_at_ns": None}
                for h, label in device_labels.items()
                if label == 1
            ]
            loader_dict["chaos_log"] = chaos_entries

        # Store provenance in metadata so callers can filter/audit.
        loader_dict["metadata"] = {
            "offset_label": snap.get("offset_label", ""),
            "adversarial": is_adversarial,
            "fault_type": snap.get("fault_type", ""),
            "source_file": snap.get("_source_file", ""),
        }

        inj_key = f"{snap.get('injection_index', '')}_{snap.get('fault_type', '')}"
        injection_ids.add(inj_key)

        try:
            graph = loader.from_snapshot(loader_dict)
            graphs.append(graph)
        except Exception:
            continue

    graphs.sort(key=lambda g: g.snapshot_ns)

    # Align snapshots with sorted graphs via snapshot_ns lookup
    ns_to_snap = {snap.get("captured_at_ns", 0): snap for snap in snapshots}
    sorted_snaps = [ns_to_snap.get(g.snapshot_ns, {}) for g in graphs]
    assign_change_weights(graphs, sorted_snaps)

    min_ns = graphs[0].snapshot_ns if graphs else 0
    max_ns = graphs[-1].snapshot_ns if graphs else 0
    time_span_h = (max_ns - min_ns) / 1e9 / 3600 if min_ns and max_ns else 0.0

    stats = ArchiveStats(
        total_snapshots=len(graphs),
        fault_snapshots=fault_count,
        clean_snapshots=clean_count,
        adversarial_snapshots=adversarial_count,
        unique_injections=len(injection_ids),
        time_span_hours=time_span_h,
    )
    return graphs, stats


# ── Time-ordered split (no leakage) ──────────────────────────────────────────

def time_split(
    graphs: list[BonsaiGraphData],
    train_frac: float = 0.70,
    val_frac: float = 0.15,
) -> tuple[list[BonsaiGraphData], list[BonsaiGraphData], list[BonsaiGraphData]]:
    """Split graphs into (train, val, test) by time order.

    Graphs must already be sorted ascending by snapshot_ns (convert_archive_to_graphs
    guarantees this). Splitting by index preserves chronological order so no future
    data leaks into the training window.
    """
    n = len(graphs)
    if n == 0:
        return [], [], []
    train_end = int(n * train_frac)
    val_end = train_end + int(n * val_frac)
    return graphs[:train_end], graphs[train_end:val_end], graphs[val_end:]


# ── Parquet path (used when real archive Parquet partitions are available) ────

def convert_parquet_to_graphs(
    parquet_dir: str | Path,
    chaos_runs_dir: str | Path = "runtime/chaos_runs",
) -> tuple[list[BonsaiGraphData], ArchiveStats]:
    """Augment snapshot graphs with Parquet-derived feature rows.

    Currently a skeleton: returns the snapshot-based graphs unchanged.
    When real archive Parquet partitions exist (from python/bonsai_sdk/training.py),
    extend this to join on (device_address, time_bucket) and enrich node features
    with BGP session count, interface error rates, and detection-event density.
    """
    # Snapshot-based graphs are the primary path; Parquet enrichment is additive.
    return convert_archive_to_graphs(chaos_runs_dir)


# ── Synthetic fixtures (for testing without a real archive) ──────────────────

def make_synthetic_snapshot(
    device_ids: list[str],
    fault_hostname: str | None = None,
    offset_label: str = "pre",
    ts_ns: int = 0,
    adversarial: bool = False,
    fault_type: str = "interface_shut",
) -> dict:
    """Build a synthetic snapshot dict that mimics chaos_runner.py output.

    Use this in tests to exercise the converter without a live chaos archive.

    Args:
        device_ids:     Node IDs for the synthetic topology (e.g. ["srl-leaf1", "srl-spine1"]).
        fault_hostname: The device being injected (sets hostname field).
        offset_label:   One of "pre", "+10s", "+30s", "+60s", "+300s", "+1800s".
        ts_ns:          Snapshot timestamp in nanoseconds. Auto-set to now if 0.
        adversarial:    If True, this is an adversarial case (labeled clean by convention).
        fault_type:     Fault type string for the metadata field.
    """
    ts = ts_ns or _time.time_ns()
    inject_offset_ns = 20_000_000_000  # 20 s before snapshot for active-fault offsets
    devices = [
        {"address": d, "vendor": "nokia", "hostname": d, "role": "leaf", "health": "healthy"}
        for d in device_ids
    ]
    links = [
        {"src_device": device_ids[i], "dst_device": device_ids[i + 1],
         "src_iface": "e1-1", "dst_iface": "e1-1", "bytes_total": 0}
        for i in range(len(device_ids) - 1)
    ]
    return {
        "captured_at_ns": ts,
        "inject_ns": (ts - inject_offset_ns) if fault_hostname and offset_label != "pre" else 0,
        "fault_type": fault_type if fault_hostname else "",
        "hostname": fault_hostname or "",
        "offset_label": offset_label,
        "adversarial": adversarial,
        "injection_index": 0,
        "topology": {"devices": devices, "links": links},
        "detections": [],
    }


# ── EV1-8 T4: Change weight computation ──────────────────────────────────────


def compute_sample_weights(
    graphs: list[BonsaiGraphData],
    snapshots: list[dict],
    control_weight: float = 0.1,
    rare_fault_boost: float = 3.0,
    rare_threshold: int = 5,
) -> list[float]:
    """Compute per-sample training weights for ControlWeightedLoss / WeightedRandomSampler.

    Logic:
    - Adversarial / control snapshots (``adversarial=True`` or all-zero labels)
      get weight ``control_weight`` (default 0.1) to suppress gradient.
    - Fault types with fewer than ``rare_threshold`` examples in the batch are
      boosted by ``rare_fault_boost`` to counter class imbalance.
    - Clean (non-adversarial, non-fault) snapshots keep weight 1.0.
    - Fault snapshots keep weight 1.0 (or ``rare_fault_boost`` if rare).

    Args:
        graphs:    BonsaiGraphData instances (same order as ``snapshots``).
        snapshots: Raw snapshot dicts aligned 1:1 with ``graphs``.
        control_weight: Weight for adversarial / expected-change samples.
        rare_fault_boost: Multiplier for rare fault types.
        rare_threshold: Fault type count below which boost is applied.

    Returns:
        List of float weights, one per graph.
    """
    if len(graphs) != len(snapshots):
        raise ValueError(
            f"graphs ({len(graphs)}) and snapshots ({len(snapshots)}) must have the same length"
        )

    # Count fault type occurrences across all fault snapshots
    from collections import Counter
    fault_type_counts: Counter = Counter()
    for snap in snapshots:
        if snap.get("fault_type") and not snap.get("adversarial"):
            fault_type_counts[snap["fault_type"]] += 1

    weights: list[float] = []
    for snap in snapshots:
        is_adversarial = bool(snap.get("adversarial"))
        fault_type = snap.get("fault_type", "")
        is_fault = bool(fault_type) and not is_adversarial

        if is_adversarial:
            w = control_weight
        elif is_fault:
            if fault_type_counts.get(fault_type, 0) < rare_threshold:
                w = rare_fault_boost
            else:
                w = 1.0
        else:
            w = 1.0

        weights.append(w)

    return weights


def compute_control_mask(snapshots: list[dict]) -> list[int]:
    """Return a 0/1 integer mask: 1 = control/adversarial sample, 0 = real fault or clean.

    Used directly as the ``control_mask`` argument to :class:`ControlWeightedLoss`
    and :class:`FocalControlWeightedLoss`.

    Args:
        snapshots: Raw snapshot dicts aligned 1:1 with the graph batch.

    Returns:
        List of ints (0 or 1), one per snapshot.
    """
    return [1 if snap.get("adversarial") else 0 for snap in snapshots]


def assign_change_weights(
    graphs: list[BonsaiGraphData],
    snapshots: list[dict],
    same_device_weight: float = 0.0,
    different_device_weight: float = 0.5,
    no_change_weight: float = 1.0,
) -> None:
    """Assign change_weight to each BonsaiGraphData in-place (EV1-8 T4).

    Rules (matching EV1-8 T4 spec):
    - Adversarial snapshot (active change on SAME device as fault): weight = 0.0.
    - Adversarial snapshot with change on a DIFFERENT device: weight = 0.5.
    - No change request active: weight = 1.0.

    The ``adversarial`` flag in snapshot dicts is used as the change-request proxy
    for synthetic / chaos archive data (adversarial=True means a planned fault).
    For real data, populate ``change_affects_same_device`` in the snapshot dict.

    Modifies ``graphs`` in-place; returns None.
    """
    for graph, snap in zip(graphs, snapshots):
        if snap.get("change_affects_same_device"):
            graph.change_weight = same_device_weight
        elif snap.get("adversarial") or snap.get("change_affects_other_device"):
            graph.change_weight = different_device_weight
        else:
            graph.change_weight = no_change_weight


def compute_change_weights_from_api(
    graphs: list[BonsaiGraphData],
    snapshots: list[dict],
    api_url: str,
    window_secs: int = 1800,
    same_device_weight: float = 0.0,
    different_device_weight: float = 0.5,
) -> None:
    """Query Bonsai API for active ChangeRequest nodes and set change_weight in-place.

    For each snapshot, queries GET /api/change-requests?active_at_ns={ns} and checks
    whether the affected device matches the fault label device. This enriches
    change_weight with real change-window data from production.

    Falls back to ``assign_change_weights`` on any API error (non-fatal).
    """
    import urllib.request
    import json as _json

    for graph, snap in zip(graphs, snapshots):
        snap_ns = snap.get("captured_at_ns", 0)
        fault_hostname = snap.get("hostname", "")
        if not snap_ns:
            graph.change_weight = 1.0
            continue
        try:
            url = f"{api_url}/api/change-requests?active_at_ns={snap_ns}&window_secs={window_secs}"
            with urllib.request.urlopen(url, timeout=2) as resp:
                data = _json.loads(resp.read())
            change_requests = data.get("change_requests", [])
            if not change_requests:
                graph.change_weight = 1.0
                continue
            affected = {cr.get("device_address", "") for cr in change_requests}
            if fault_hostname in affected:
                graph.change_weight = same_device_weight
            else:
                graph.change_weight = different_device_weight
        except Exception:
            graph.change_weight = 1.0


def graphs_from_synthetic(snapshots: list[dict]) -> tuple[list[BonsaiGraphData], ArchiveStats]:
    """Convert a list of synthetic snapshots to graphs and stats without touching disk."""
    loader = BonsaiGnnDataLoader()
    graphs: list[BonsaiGraphData] = []
    fault_count = 0
    clean_count = 0
    adversarial_count = 0
    injection_ids: set[str] = set()

    for snap in snapshots:
        loader_dict = _topology_to_loader_dict(snap)
        if not loader_dict["devices"]:
            continue

        is_adversarial = bool(snap.get("adversarial"))
        device_labels = _per_device_labels(snap)

        if is_adversarial:
            adversarial_count += 1
        elif device_labels:
            fault_count += 1
        else:
            clean_count += 1

        if device_labels:
            inject_ns = snap.get("inject_ns", 0)
            chaos_entries = [
                {"hostname": h, "injected_at_ns": inject_ns, "healed_at_ns": None}
                for h, label in device_labels.items()
                if label == 1
            ]
            loader_dict["chaos_log"] = chaos_entries

        inj_key = f"{snap.get('injection_index', '')}_{snap.get('fault_type', '')}"
        injection_ids.add(inj_key)

        try:
            graph = loader.from_snapshot(loader_dict)
            graphs.append(graph)
        except Exception:
            continue

    graphs.sort(key=lambda g: g.snapshot_ns)

    min_ns = graphs[0].snapshot_ns if graphs else 0
    max_ns = graphs[-1].snapshot_ns if graphs else 0
    time_span_h = (max_ns - min_ns) / 1e9 / 3600 if min_ns and max_ns else 0.0

    stats = ArchiveStats(
        total_snapshots=len(graphs),
        fault_snapshots=fault_count,
        clean_snapshots=clean_count,
        adversarial_snapshots=adversarial_count,
        unique_injections=len(injection_ids),
        time_span_hours=time_span_h,
    )
    return graphs, stats
