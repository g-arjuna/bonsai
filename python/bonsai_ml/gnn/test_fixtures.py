"""Synthetic graph fixtures for GNN loader tests and early training work."""
from __future__ import annotations


def synthetic_dc_snapshot(snapshot_ns: int = 1_700_000_060_000_000_000) -> dict:
    """Return a labelled 4-node DC fabric snapshot with one active fault."""
    return {
        "source": "synthetic_dc_fixture",
        "snapshot_ns": snapshot_ns,
        "devices": [
            {
                "id": "srl-spine1",
                "hostname": "srl-spine1",
                "vendor": "nokia",
                "role": "spine",
                "embedding": [0.2, 0.8, 0.1, 0.0],
            },
            {
                "id": "srl-spine2",
                "hostname": "srl-spine2",
                "vendor": "nokia",
                "role": "spine",
                "embedding": [0.3, 0.7, 0.1, 0.0],
            },
            {
                "id": "srl-leaf1",
                "hostname": "srl-leaf1",
                "vendor": "nokia",
                "role": "leaf",
                "embedding": [0.9, 0.1, 0.2, 0.0],
            },
            {
                "id": "srl-leaf2",
                "hostname": "srl-leaf2",
                "vendor": "nokia",
                "role": "leaf",
                "embedding": [0.8, 0.2, 0.3, 0.0],
            },
        ],
        "links": [
            {"src_device": "srl-spine1", "dst_device": "srl-leaf1", "type": "connected_to"},
            {"src_device": "srl-spine1", "dst_device": "srl-leaf2", "type": "connected_to"},
            {"src_device": "srl-spine2", "dst_device": "srl-leaf1", "type": "connected_to"},
            {"src_device": "srl-spine2", "dst_device": "srl-leaf2", "type": "connected_to"},
        ],
        "chaos_log": [
            {
                "fault_type": "interface_shut",
                "hostname": "srl-leaf1",
                "param": "ethernet-1/1",
                "injected_at_ns": snapshot_ns - 30_000_000_000,
                "healed_at_ns": snapshot_ns + 30_000_000_000,
            }
        ],
    }
