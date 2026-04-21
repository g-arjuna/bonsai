"""Tests for T0-4 (_empty_features types), T0-5 (vendor join), T0-6 (shared extractor)."""
from __future__ import annotations

from unittest.mock import MagicMock, patch


# ── T0-4: _empty_features numeric field types ─────────────────────────────────

def test_empty_features_no_string_numerics():
    """Every int/float field in _empty_features must default to 0, never ''."""
    from bonsai_sdk.training import _empty_features
    import typing
    from bonsai_sdk.detection import Features

    result = _empty_features("10.0.0.1:57400", "bgp_session_change", {}, 0)
    hints = typing.get_type_hints(Features)

    for name, hint in hints.items():
        if hint in (int, float):
            assert result[name] != "", (
                f"Numeric field '{name}' defaulted to '' in _empty_features — "
                f"will corrupt Parquet dtype"
            )
            assert isinstance(result[name], (int, float)), (
                f"Numeric field '{name}' should be int/float, got {type(result[name])}"
            )


def test_empty_features_str_fields_are_strings():
    """str fields must default to '', not 0."""
    from bonsai_sdk.training import _empty_features
    import typing
    from bonsai_sdk.detection import Features

    result = _empty_features("addr", "etype", {}, 0)
    hints = typing.get_type_hints(Features)
    for name, hint in hints.items():
        if hint is str and name not in ("device_address", "event_type"):
            assert isinstance(result[name], str), (
                f"String field '{name}' is not str: got {type(result[name])}"
            )


# ── T0-5: vendor column uses Device join ──────────────────────────────────────

def test_remediation_export_vendor_join():
    """export_remediation_training_set must query trusted remediations and d.vendor."""
    from bonsai_sdk.training import export_remediation_training_set

    client = MagicMock()
    # Return one row with vendor = "nokia_srl" (8th column)
    client.query.return_value = [[
        "rem-1", "det-1", "bgp_session_down", "bgp_session_bounce",
        "success",
        1000000,
        '{"device_address":"10.0.0.1","event_type":"bgp_session_change","detail":"{}","peer_address":"10.0.0.2","old_state":"established","new_state":"idle","peer_count_total":2,"peer_count_established":1,"recent_flap_count":0,"if_name":"","oper_status":"","occurred_at_ns":0,"state_change_event_id":""}',
        1000000,
        "nokia_srl",   # d.vendor — this is the fix
    ]]

    with patch("bonsai_sdk.training.pq.write_table"), \
         patch("bonsai_sdk.training.pa") as mock_pa:
        mock_pa.Table.from_pandas.return_value = MagicMock()
        export_remediation_training_set(client, "/tmp/test.parquet")

    cypher_called = client.query.call_args[0][0]
    assert "RemediationTrustMark" in cypher_called, (
        "Cypher query must read graph trust marks so Model C excludes untrusted rows"
    )
    assert "m.trustworthy = 1" in cypher_called, (
        "Cypher query must filter to trusted remediations"
    )
    assert "d.vendor" in cypher_called, (
        "Cypher query must join to Device and return d.vendor (T0-5 fix)"
    )
    assert "device_address" not in cypher_called.split("RETURN")[1], (
        "vendor column must come from d.vendor join, not device_address"
    )


# ── T0-6: shared feature extractor ───────────────────────────────────────────

def _make_bgp_event(device="10.0.0.1", etype="bgp_session_change",
                    detail_json='{"peer":"10.0.0.2","old_state":"established","new_state":"idle"}'):
    ev = MagicMock()
    ev.device_address = device
    ev.event_type = etype
    ev.detail_json = detail_json
    ev.occurred_at_ns = 1000000000
    ev.state_change_event_id = "ev-123"
    return ev


def test_extract_features_for_event_bgp():
    """Shared extractor populates BGP peer fields correctly."""
    from bonsai_sdk.ml_detector import extract_features_for_event
    from bonsai_sdk.detection import Features

    client = MagicMock()
    neighbor = MagicMock()
    neighbor.session_state = "established"
    client.get_bgp_neighbors.return_value = [neighbor, MagicMock(session_state="idle")]

    f = extract_features_for_event(_make_bgp_event(), client)
    assert isinstance(f, Features)
    assert f.peer_address == "10.0.0.2"
    assert f.old_state == "established"
    assert f.new_state == "idle"
    assert f.peer_count_total == 2
    assert f.peer_count_established == 1


def test_ml_detector_uses_shared_extractor():
    """MLDetector.extract_features must delegate to extract_features_for_event."""
    from bonsai_sdk.ml_detector import MLDetector, extract_features_for_event

    client = MagicMock()
    client.get_bgp_neighbors.return_value = []

    model_mock = MagicMock()
    with patch("bonsai_sdk.ml_detector.load_model", return_value=model_mock), \
         patch("bonsai_sdk.ml_detector.extract_features_for_event") as mock_extract:
        mock_extract.return_value = MagicMock()
        detector = MLDetector("ml_test", "fake_path.joblib", threshold=0.5)
        ev = _make_bgp_event()
        detector.extract_features(ev, client)
        mock_extract.assert_called_once_with(ev, client)


def test_rule_detector_matches_ml_detector_features_for_bgp_down():
    """Rule and ML detectors should share the same canonical BGP feature extraction."""
    from bonsai_sdk.ml_detector import MLDetector
    from bonsai_sdk.rules.bgp import BgpSessionDown

    client = MagicMock()
    client.get_bgp_neighbors.return_value = [
        MagicMock(session_state="established"),
        MagicMock(session_state="idle"),
    ]

    with patch("bonsai_sdk.ml_detector.load_model", return_value=MagicMock()):
        ml_detector = MLDetector("ml_test", "fake_path.joblib", threshold=0.5)

    rule_features = BgpSessionDown().extract_features(_make_bgp_event(), client)
    ml_features = ml_detector.extract_features(_make_bgp_event(), client)

    assert rule_features == ml_features


def test_shared_extractor_interface_event():
    """Shared extractor populates if_name for interface events."""
    from bonsai_sdk.ml_detector import extract_features_for_event

    client = MagicMock()
    ev = _make_bgp_event(
        etype="interface_oper_status_change",
        detail_json='{"if_name":"ethernet-1/1","oper_status":"down"}'
    )
    f = extract_features_for_event(ev, client)
    assert f.if_name == "ethernet-1/1"
    assert f.oper_status == "down"
    client.get_bgp_neighbors.assert_not_called()
