from __future__ import annotations

from unittest.mock import MagicMock, patch

import pandas as pd


def test_validate_anomaly_dataframe_requires_minimum_bars():
    from bonsai_sdk.training_readiness import validate_anomaly_dataframe

    rows = []
    for i in range(49):
        rows.append({
            "label": 1,
            "rule_id": "bgp_session_down",
            "event_type": "bgp_session_change",
            "oper_status": "",
            "occurred_at_ns": 1000 + i,
            "peer_count_total": 2,
            "peer_count_established": 1,
            "recent_flap_count": 0,
        })
    for i in range(200):
        rows.append({
            "label": 0,
            "rule_id": "",
            "event_type": "interface_stats",
            "oper_status": "up",
            "occurred_at_ns": 2000 + i,
            "peer_count_total": 0,
            "peer_count_established": 0,
            "recent_flap_count": 0,
        })

    check = validate_anomaly_dataframe(pd.DataFrame(rows))
    assert not check.ready
    assert any("50 anomaly rows" in problem for problem in check.problems)


def test_validate_remediation_dataframe_filters_by_cutoff():
    from bonsai_sdk.training_readiness import (
        TRAINING_HYGIENE_CUTOFF_NS,
        validate_remediation_dataframe,
    )

    rows = []
    for i in range(50):
        rows.append({
            "action": "bounce" if i % 2 == 0 else "alert_only",
            "status": "success" if i < 49 else "failed",
            "attempted_at_ns": TRAINING_HYGIENE_CUTOFF_NS + 1000 + i,
            "event_type": "bgp_session_change",
            "oper_status": "",
            "occurred_at_ns": TRAINING_HYGIENE_CUTOFF_NS + 1000 + i,
            "peer_count_total": 2,
            "peer_count_established": 1,
            "recent_flap_count": 0,
        })
    rows.append({
        "action": "bounce",
        "status": "success",
        "attempted_at_ns": TRAINING_HYGIENE_CUTOFF_NS - 1,
        "event_type": "bgp_session_change",
        "oper_status": "",
        "occurred_at_ns": TRAINING_HYGIENE_CUTOFF_NS - 1,
        "peer_count_total": 2,
        "peer_count_established": 1,
        "recent_flap_count": 0,
    })

    check = validate_remediation_dataframe(pd.DataFrame(rows))
    assert not check.ready
    assert check.stats["rows_post_cutoff"] == 50
    assert check.stats["rows_success"] == 49
    assert any("50 successful remediations" in problem for problem in check.problems)


def test_export_remediation_training_set_includes_attempted_at_and_filters_since():
    from bonsai_sdk.training import export_remediation_training_set

    client = MagicMock()
    client.query.return_value = [
        [
            "rem-old",
            "det-old",
            "bgp_session_down",
            "bounce",
            "success",
            100,
            '{"device_address":"10.0.0.1","event_type":"bgp_session_change","detail":"{}","peer_address":"10.0.0.2","old_state":"established","new_state":"idle","peer_count_total":2,"peer_count_established":1,"recent_flap_count":0,"if_name":"","oper_status":"","occurred_at_ns":100,"state_change_event_id":""}',
            100,
            "nokia_srl",
        ],
        [
            "rem-new",
            "det-new",
            "bgp_session_down",
            "bounce",
            "failed",
            200,
            '{"device_address":"10.0.0.1","event_type":"bgp_session_change","detail":"{}","peer_address":"10.0.0.2","old_state":"established","new_state":"idle","peer_count_total":2,"peer_count_established":1,"recent_flap_count":0,"if_name":"","oper_status":"","occurred_at_ns":200,"state_change_event_id":""}',
            200,
            "nokia_srl",
        ],
    ]

    captured = {}

    with patch("bonsai_sdk.training.pq.write_table"), patch("bonsai_sdk.training.pa") as mock_pa:
        def _capture(df):
            captured["df"] = df.copy()
            return MagicMock()

        mock_pa.Table.from_pandas.side_effect = _capture
        count = export_remediation_training_set(client, "/tmp/test.parquet", since_ns=150, until_ns=500)

    assert count == 1
    df = captured["df"]
    assert list(df["remediation_id"]) == ["rem-new"]
    assert int(df.iloc[0]["attempted_at_ns"]) == 200


def test_build_graph_readiness_from_summary_uses_shared_thresholds():
    from bonsai_sdk.training_readiness import build_graph_readiness_from_summary

    summary = {
        "detection_events": 55,
        "state_change_events": 240,
        "rule_distribution": {
            "bgp_session_down": 30,
            "interface_down": 25,
        },
        "cutoff_iso": "2026-04-20T09:32:50+00:00",
        "remediation_rows_post_cutoff": 80,
        "action_distribution_post_cutoff": {
            "bounce_bgp_neighbor": 60,
            "bounce_interface": 20,
        },
        "status_distribution_post_cutoff": {
            "success": 55,
            "failed": 15,
            "skipped": 10,
        },
    }

    model_a, model_c = build_graph_readiness_from_summary(summary)

    assert model_a.ready
    assert model_a.stats["distinct_rules"] == 2
    assert model_c.ready
    assert model_c.stats["successful_remediations_post_cutoff"] == 55


def test_summarize_graph_readiness_queries_trusted_remediations():
    from bonsai_sdk.training_readiness import summarize_graph_readiness

    client = MagicMock()
    client.query.side_effect = [
        [["bgp_session_down"], ["interface_down"]],
        [[250]],
        [["bounce_bgp_neighbor", "success"], ["log_only", "skipped"]],
    ]

    summary = summarize_graph_readiness(client)

    assert summary["detection_events"] == 2
    assert summary["state_change_events"] == 250
    assert summary["status_distribution_post_cutoff"] == {
        "success": 1,
        "skipped": 1,
    }
    trusted_query = client.query.call_args_list[2].args[0]
    assert "RemediationTrustMark" in trusted_query
    assert "m.trustworthy = 1" in trusted_query
