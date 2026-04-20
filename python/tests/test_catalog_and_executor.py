"""Tests for T0-1 (catalog path) and T0-2 (verification field name)."""
from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest

from bonsai_sdk.playbooks.catalog import PlaybookCatalog, LIBRARY_DIR
from bonsai_sdk.playbooks.executor import PlaybookExecutor


# ── T0-1: catalog path and YAML loading ──────────────────────────────────────

def test_library_dir_points_to_repo_root():
    """LIBRARY_DIR must resolve to <repo_root>/playbooks/library/, not the SDK-internal one."""
    assert LIBRARY_DIR.exists(), f"Library dir not found: {LIBRARY_DIR}"
    # Must NOT be inside the python/ subtree
    assert "python" not in LIBRARY_DIR.parts, (
        f"LIBRARY_DIR is still pointing inside python/: {LIBRARY_DIR}"
    )


def test_all_nine_playbooks_load():
    """All 9 YAML files in playbooks/library/ must load without error."""
    catalog = PlaybookCatalog()
    rule_ids = catalog.all_rule_ids()
    expected = {
        "bgp_session_down",
        "bgp_session_flap",
        "bgp_all_peers_down",
        "bgp_never_established",
        "interface_down",
        "interface_error_spike",
        "interface_high_utilization",
        "topology_edge_lost",
        "bfd_session_down",
    }
    missing = expected - set(rule_ids)
    assert not missing, f"Playbooks missing from catalog: {missing}"


def test_no_duplicate_in_sdk_library():
    """The SDK-internal library/ should not contain any YAML playbooks."""
    sdk_lib = Path(__file__).parents[1] / "bonsai_sdk" / "playbooks" / "library"
    yamls = list(sdk_lib.glob("*.yaml"))
    assert yamls == [], (
        f"Stale YAML files found in SDK-internal library (should be empty): "
        f"{[f.name for f in yamls]}"
    )


def test_for_detection_returns_playbooks_for_srl():
    catalog = PlaybookCatalog()
    playbooks = catalog.for_detection("bgp_session_down", "nokia_srl")
    assert len(playbooks) >= 1
    assert all(p.get("vendor") in ("nokia_srl", "*") for p in playbooks)


# ── T0-2: verification field name ─────────────────────────────────────────────

def _make_detection(peer="10.0.0.1", device="172.0.0.1:57400",
                    old_state="established"):
    from bonsai_sdk.detection import Detection, Features
    features = Features(
        device_address=device,
        event_type="bgp_session_change",
        detail={},
        peer_address=peer,
        old_state=old_state,
        new_state="idle",
        peer_count_total=1,
        peer_count_established=0,
        recent_flap_count=1,
        oper_status="up",
        occurred_at_ns=0,
    )
    return Detection(rule_id="bgp_session_down", severity="critical",
                     features=features, reason="test")


def test_verify_reads_expected_graph_state_field():
    """verify() must use `expected_graph_state`, not `cypher`."""
    client = MagicMock()
    client.query.return_value = [[True]]  # graph says session is established

    catalog = MagicMock()
    executor = PlaybookExecutor(catalog=catalog, client=client)

    playbook = {
        "verification": {
            "wait_seconds": 1,
            "expected_graph_state": (
                'MATCH (n:BgpNeighbor {peer_address: $peer_address}) '
                'WHERE n.session_state = "established" RETURN count(n) > 0'
            ),
        }
    }
    result = executor.verify(playbook, _make_detection())
    assert result is True
    assert client.query.called


def test_verify_returns_false_when_graph_empty():
    """verify() must return False when the recovery query returns no rows."""
    client = MagicMock()
    client.query.return_value = []

    catalog = MagicMock()
    executor = PlaybookExecutor(catalog=catalog, client=client)

    playbook = {
        "verification": {
            "wait_seconds": 1,
            "expected_graph_state": (
                'MATCH (n:BgpNeighbor {peer_address: $peer_address}) '
                'WHERE n.session_state = "established" RETURN count(n) > 0'
            ),
        }
    }
    result = executor.verify(playbook, _make_detection())
    assert result is False


def test_verify_legacy_cypher_field_still_works():
    """Legacy `cypher` key is accepted as a fallback so nothing breaks mid-migration."""
    client = MagicMock()
    client.query.return_value = [[True]]

    catalog = MagicMock()
    executor = PlaybookExecutor(catalog=catalog, client=client)

    playbook = {
        "verification": {
            "wait_seconds": 1,
            "cypher": 'MATCH (n:BgpNeighbor) RETURN count(n) > 0',
        }
    }
    result = executor.verify(playbook, _make_detection())
    assert result is True


def test_verify_no_verification_block_returns_true():
    """Playbooks without a verification block pass through as successful."""
    client = MagicMock()
    catalog = MagicMock()
    executor = PlaybookExecutor(catalog=catalog, client=client)

    result = executor.verify({}, _make_detection())
    assert result is True
    assert not client.query.called
