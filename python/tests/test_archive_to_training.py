"""Tests for bonsai_ml.gnn.archive_to_training (T2-5).

All tests use synthetic snapshots — no real chaos archive needed.
"""
import time

import numpy as np
import pytest

from bonsai_ml.gnn.archive_to_training import (
    ArchiveStats,
    graphs_from_synthetic,
    make_synthetic_snapshot,
    time_split,
    _fault_active_at_snapshot,
    _per_device_labels,
)


DEVICES = ["srl-leaf1", "srl-spine1", "srl-leaf2"]


# ── make_synthetic_snapshot ───────────────────────────────────────────────────

def test_synthetic_pre_snapshot_no_hostname():
    snap = make_synthetic_snapshot(DEVICES, offset_label="pre")
    assert snap["hostname"] == ""
    assert snap["offset_label"] == "pre"
    assert snap["inject_ns"] == 0
    assert not snap["adversarial"]
    assert len(snap["topology"]["devices"]) == 3
    assert len(snap["topology"]["links"]) == 2


def test_synthetic_active_fault_has_inject_ns():
    snap = make_synthetic_snapshot(DEVICES, fault_hostname="srl-leaf1", offset_label="+30s")
    assert snap["hostname"] == "srl-leaf1"
    assert snap["inject_ns"] > 0
    assert snap["inject_ns"] < snap["captured_at_ns"]


def test_synthetic_adversarial_flag():
    snap = make_synthetic_snapshot(DEVICES, fault_hostname="srl-leaf1", offset_label="+30s",
                                   adversarial=True)
    assert snap["adversarial"] is True


# ── _fault_active_at_snapshot / _per_device_labels ───────────────────────────

def test_pre_snapshot_not_active():
    snap = make_synthetic_snapshot(DEVICES, fault_hostname="srl-leaf1", offset_label="pre")
    assert not _fault_active_at_snapshot(snap)
    assert _per_device_labels(snap) == {}


def test_plus10s_snapshot_is_active():
    snap = make_synthetic_snapshot(DEVICES, fault_hostname="srl-leaf1", offset_label="+10s")
    assert _fault_active_at_snapshot(snap)
    assert _per_device_labels(snap) == {"srl-leaf1": 1}


def test_plus300s_snapshot_not_active():
    snap = make_synthetic_snapshot(DEVICES, fault_hostname="srl-leaf1", offset_label="+300s")
    assert not _fault_active_at_snapshot(snap)
    assert _per_device_labels(snap) == {}


def test_adversarial_plus30s_not_active():
    snap = make_synthetic_snapshot(DEVICES, fault_hostname="srl-leaf1", offset_label="+30s",
                                   adversarial=True)
    assert not _fault_active_at_snapshot(snap)
    assert _per_device_labels(snap) == {}


# ── graphs_from_synthetic ─────────────────────────────────────────────────────

def _make_run(base_ts_ns: int, step_ns: int = 5_000_000_000) -> list[dict]:
    """Build one complete fault cycle: pre + 3 active + 2 recovery snapshots."""
    labels = ["pre", "+10s", "+30s", "+60s", "+300s", "+1800s"]
    return [
        make_synthetic_snapshot(
            DEVICES,
            fault_hostname="srl-leaf1",
            offset_label=label,
            ts_ns=base_ts_ns + i * step_ns,
        )
        for i, label in enumerate(labels)
    ]


def test_graphs_from_synthetic_basic():
    snapshots = _make_run(time.time_ns())
    graphs, stats = graphs_from_synthetic(snapshots)

    assert stats.total_snapshots == 6
    assert stats.fault_snapshots == 3     # +10s, +30s, +60s
    assert stats.clean_snapshots == 3     # pre, +300s, +1800s
    assert stats.adversarial_snapshots == 0
    assert stats.unique_injections == 1
    assert len(graphs) == 6


def test_graphs_sorted_by_time():
    snapshots = _make_run(time.time_ns())
    graphs, _ = graphs_from_synthetic(snapshots)
    ns_seq = [g.snapshot_ns for g in graphs]
    assert ns_seq == sorted(ns_seq)


def test_fault_node_labeled_1():
    snap = make_synthetic_snapshot(DEVICES, fault_hostname="srl-leaf1", offset_label="+30s",
                                   ts_ns=time.time_ns())
    graphs, _ = graphs_from_synthetic([snap])
    assert len(graphs) == 1
    g = graphs[0]
    leaf1_idx = g.node_ids.index("srl-leaf1")
    assert g.y[leaf1_idx] == 1

    # Other nodes must be 0
    other_idxs = [i for i, n in enumerate(g.node_ids) if n != "srl-leaf1"]
    assert all(g.y[i] == 0 for i in other_idxs)


def test_clean_nodes_all_zero():
    snap = make_synthetic_snapshot(DEVICES, offset_label="pre", ts_ns=time.time_ns())
    graphs, _ = graphs_from_synthetic([snap])
    assert len(graphs) == 1
    assert np.all(graphs[0].y == 0)


def test_adversarial_labeled_clean():
    snap = make_synthetic_snapshot(DEVICES, fault_hostname="srl-leaf1", offset_label="+30s",
                                   adversarial=True, ts_ns=time.time_ns())
    graphs, stats = graphs_from_synthetic([snap])
    assert stats.adversarial_snapshots == 1
    assert stats.fault_snapshots == 0
    assert np.all(graphs[0].y == 0)


def test_graph_validates():
    snap = make_synthetic_snapshot(DEVICES, fault_hostname="srl-leaf1", offset_label="+60s",
                                   ts_ns=time.time_ns())
    graphs, _ = graphs_from_synthetic([snap])
    graphs[0].validate()  # must not raise


# ── time_split ────────────────────────────────────────────────────────────────

def test_time_split_proportions():
    n = 100
    base = time.time_ns()
    snapshots = [
        make_synthetic_snapshot(DEVICES, ts_ns=base + i * 1_000_000_000)
        for i in range(n)
    ]
    graphs, _ = graphs_from_synthetic(snapshots)
    train, val, test = time_split(graphs, train_frac=0.70, val_frac=0.15)

    assert len(train) == 70
    assert len(val) == 15
    assert len(test) == 15
    assert len(train) + len(val) + len(test) == n


def test_time_split_no_overlap():
    n = 60
    base = time.time_ns()
    snapshots = [
        make_synthetic_snapshot(DEVICES, ts_ns=base + i * 1_000_000_000)
        for i in range(n)
    ]
    graphs, _ = graphs_from_synthetic(snapshots)
    train, val, test = time_split(graphs)

    train_ts = {g.snapshot_ns for g in train}
    val_ts = {g.snapshot_ns for g in val}
    test_ts = {g.snapshot_ns for g in test}

    assert train_ts.isdisjoint(val_ts)
    assert train_ts.isdisjoint(test_ts)
    assert val_ts.isdisjoint(test_ts)


def test_time_split_chronological_ordering():
    n = 30
    base = time.time_ns()
    snapshots = [
        make_synthetic_snapshot(DEVICES, ts_ns=base + i * 1_000_000_000)
        for i in range(n)
    ]
    graphs, _ = graphs_from_synthetic(snapshots)
    train, val, test = time_split(graphs)

    # All train timestamps must precede all val, and all val before all test.
    assert max(g.snapshot_ns for g in train) < min(g.snapshot_ns for g in val)
    assert max(g.snapshot_ns for g in val) < min(g.snapshot_ns for g in test)


def test_time_split_empty():
    train, val, test = time_split([])
    assert train == val == test == []


# ── ArchiveStats ──────────────────────────────────────────────────────────────

def test_archive_stats_str():
    stats = ArchiveStats(10, 3, 6, 1, 2, 4.5)
    s = str(stats)
    assert "total=10" in s
    assert "span=4.5h" in s


def test_multi_injection_unique_count():
    base = time.time_ns()
    snaps = []
    for inj in range(3):
        for i, label in enumerate(["pre", "+10s", "+30s"]):
            s = make_synthetic_snapshot(
                DEVICES, fault_hostname="srl-leaf1",
                offset_label=label, ts_ns=base + (inj * 10 + i) * 1_000_000_000,
            )
            s["injection_index"] = inj
            snaps.append(s)

    _, stats = graphs_from_synthetic(snaps)
    assert stats.unique_injections == 3
    assert stats.total_snapshots == 9
