"""Tests for bonsai_agent tools and budget (T3-1 / T3-3)."""
import json
from unittest.mock import MagicMock, patch

import pytest

from bonsai_agent.budget import Budget, BudgetExceeded
from bonsai_agent.tools import TOOL_SCHEMAS, call_tool


# ── tool tests ────────────────────────────────────────────────────────────────

def _client(response: dict) -> MagicMock:
    c = MagicMock()
    c._http_json.return_value = response
    return c


def test_get_blast_radius_calls_correct_endpoint():
    client = _client({"devices": ["10.0.0.1"]})
    result = call_tool(client, "get_blast_radius", {"device_address": "10.0.0.1:57400", "max_hops": 2})
    client._http_json.assert_called_once()
    args = client._http_json.call_args[0]
    assert "/api/blast-radius/10.0.0.1:57400" in args[1]
    assert "max_hops=2" in args[1]
    assert result == {"devices": ["10.0.0.1"]}


def test_get_blast_radius_returns_error_on_exception():
    client = MagicMock()
    client._http_json.side_effect = RuntimeError("HTTP 500")
    result = call_tool(client, "get_blast_radius", {"device_address": "x"})
    assert "error" in result


def test_get_application_impact_posts_cypher():
    client = _client({"rows": []})
    call_tool(client, "get_application_impact", {"device_address": "10.0.0.2:57400"})
    args = client._http_json.call_args[0]
    assert args[0] == "POST"
    assert "/api/explorer/query" in args[1]
    payload = args[2]
    assert "RUNS_SERVICE" in payload["cypher"]


def test_query_graph_posts_cypher():
    client = _client({"rows": [["10.0.0.1"]]})
    result = call_tool(client, "query_graph", {"cypher": "MATCH (d:Device) RETURN d.address"})
    args = client._http_json.call_args[0]
    assert "MATCH" in args[2]["cypher"]


def test_get_recent_detections_passes_window():
    client = _client({"rows": []})
    call_tool(client, "get_recent_detections", {"device_address": "10.0.0.1", "window_secs": 600})
    payload = client._http_json.call_args[0][2]
    assert payload["params"]["window_ns"] == 600 * 1_000_000_000


def test_get_remediation_history_calls_explorer():
    client = _client({"rows": []})
    call_tool(client, "get_remediation_history", {"device_address": "10.0.0.1"})
    args = client._http_json.call_args[0]
    assert "TRIGGERED_BY" in args[2]["cypher"]


def test_summarise_is_client_free():
    client = MagicMock()
    result = call_tool(client, "summarise", {"text": "BGP session down on spine1."})
    client._http_json.assert_not_called()
    assert result["summary"] == "BGP session down on spine1."


def test_propose_playbook_posts_to_approvals():
    client = _client({"id": "proposal-123"})
    result = call_tool(client, "propose_playbook", {
        "detection_id": "det-1",
        "playbook_id": "bgp_restart",
        "rationale": "BGP session down, restart may help",
    })
    args = client._http_json.call_args[0]
    assert args[0] == "POST"
    assert "/api/approvals" in args[1]
    payload = args[2]
    assert payload["detection_id"] == "det-1"
    assert payload["playbook_id"] == "bgp_restart"


def test_unknown_tool_returns_error():
    client = MagicMock()
    result = call_tool(client, "does_not_exist", {})
    assert "error" in result
    assert "unknown tool" in result["error"]


def test_tool_schemas_have_required_names():
    names = {s["name"] for s in TOOL_SCHEMAS}
    for expected in [
        "get_blast_radius", "get_application_impact", "query_graph",
        "get_recent_detections", "get_remediation_history", "summarise", "propose_playbook",
    ]:
        assert expected in names, f"tool schema missing: {expected}"


def test_tool_schemas_all_have_input_schema():
    for schema in TOOL_SCHEMAS:
        assert "input_schema" in schema
        assert schema["input_schema"]["type"] == "object"


# ── budget tests (T3-3) ───────────────────────────────────────────────────────

def test_budget_charge_and_check_within_limit():
    b = Budget(per_investigation=1000, daily=10000)
    b.charge("inv-1", input_tokens=100, output_tokens=50)
    b.check("inv-1")  # should not raise


def test_budget_raises_on_per_investigation_breach():
    b = Budget(per_investigation=100, daily=10000)
    b.charge("inv-1", input_tokens=80, output_tokens=30)
    with pytest.raises(BudgetExceeded, match="inv-1"):
        b.check("inv-1")


def test_budget_raises_on_daily_breach():
    b = Budget(per_investigation=100000, daily=200)
    b.charge("inv-1", input_tokens=100, output_tokens=50)
    b.charge("inv-2", input_tokens=100, output_tokens=50)
    with pytest.raises(BudgetExceeded, match="daily"):
        b.check("inv-2")


def test_budget_get_usage_tracks_tokens():
    b = Budget()
    b.charge("inv-x", input_tokens=500, output_tokens=200)
    u = b.get_usage("inv-x")
    assert u is not None
    assert u.total_tokens == 700
    assert u.input_tokens == 500
    assert u.output_tokens == 200


def test_budget_cost_usd_nonzero():
    b = Budget()
    b.charge("inv-c", input_tokens=1_000_000, output_tokens=0)
    u = b.get_usage("inv-c")
    assert u.cost_usd > 0


def test_budget_daily_summary_structure():
    b = Budget(daily=5000)
    b.charge("inv-a", input_tokens=200, output_tokens=100)
    s = b.daily_summary()
    assert s["total_tokens"] == 300
    assert s["daily_limit"] == 5000
    assert "cost_usd" in s
    assert s["investigations"] == 1


def test_budget_unknown_investigation_does_not_raise():
    b = Budget(per_investigation=100)
    b.check("nonexistent")  # no usage recorded → should not raise


def test_budget_accumulates_across_charges():
    b = Budget(per_investigation=1000)
    b.charge("inv-1", input_tokens=300, output_tokens=100)
    b.charge("inv-1", input_tokens=200, output_tokens=100)
    u = b.get_usage("inv-1")
    assert u.total_tokens == 700


def test_budget_different_investigations_tracked_independently():
    b = Budget(per_investigation=100, daily=10000)
    b.charge("inv-a", input_tokens=90, output_tokens=20)  # 110 total → breaches limit of 100
    b.charge("inv-b", input_tokens=10, output_tokens=5)
    with pytest.raises(BudgetExceeded):
        b.check("inv-a")
    b.check("inv-b")  # still within limit
