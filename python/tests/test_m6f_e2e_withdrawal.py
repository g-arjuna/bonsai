"""Withdrawal-verification tests for lab/acceptance/m6f/tools/m6f_e2e.py.

M6F-C: the post-revoke check must require AUTHORITATIVE evidence —
binding revocation, acquisition disable, issued withdrawal, collector
acknowledgement, subscription termination and a stability window with
no continued collection. A frozen timestamp across a short poll
interval is explicitly NOT sufficient.

The noncompliant-collector case is the regression that matters: a
collector that keeps streaming (subscription_count > 0, advancing
observation timestamps) must make verification FAIL — the old check
passed precisely in this scenario.
"""
from __future__ import annotations

import importlib.util
import itertools
import json
import time
import types
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
TOOL = REPO / "lab/acceptance/m6f/tools/m6f_e2e.py"

spec = importlib.util.spec_from_file_location("m6f_e2e", TOOL)
m6f_e2e = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m6f_e2e)

ADDR = "172.16.0.10:57400"
COLLECTOR = "m6f-col-1"
PATH = "/interface[name=mgmt0]/statistics"


def _status(status, obs_ns=1_000, upd_ns=2_000, sample_ns=10_000_000_000):
    return {
        "path": PATH,
        "status": status,
        "last_observed_at_ns": obs_ns,
        "updated_at_ns": upd_ns,
        "sample_interval_ns": sample_ns,
    }


def _device(statuses, binding=None, acquisition="disabled"):
    return {
        "address": ADDR,
        "schema_binding": binding,
        "acquisition": acquisition,
        "subscription_statuses": statuses,
    }


def _assignment(sub_count, heartbeat_ns=None):
    return {
        "collectors": [{
            "id": COLLECTOR,
            "connected": True,
            "last_heartbeat_ns": heartbeat_ns or time.time_ns(),
            "subscription_count": sub_count,
            "pending_withdrawal_devices": [],
        }],
        "unassigned_count": 0,
        "unassigned_devices": [],
    }


class FakeApi:
    """Minimal Api stand-in: scripted device/assignment read-backs and
    a canned SSE event list. No network, no real collector."""

    def __init__(self, device_fn, assignment_fn, events=()):
        self.device_fn = device_fn
        self.assignment_fn = assignment_fn
        self.events = list(events)
        self.token = "test-token"

    def get(self, path):
        if path == f"/api/devices/{ADDR}":
            return 200, self.device_fn()
        if path == "/api/assignment/status":
            return 200, self.assignment_fn()
        return 404, {}

    def stream_events(self, duration):
        for ev in self.events:
            yield ev
        # return immediately — the pump thread simply ends


def _args(tmp_path, timeout=2, window=0.3, poll=0.05):
    return types.SimpleNamespace(
        device_addr=ADDR,
        collector_id=COLLECTOR,
        evidence_dir=str(tmp_path),
        timeout=timeout,
        poll_secs=poll,
        withdraw_window=window,
        revoke_reason="test",
    )


def _run(api, tmp_path):
    results = []
    ok = m6f_e2e.cmd_verify_withdrawal(api, _args(tmp_path), results)
    by_stage = {r["stage"]: r for r in results}
    return ok, by_stage


def _plan_state_event():
    return {
        "device_address": ADDR,
        "event_type": "subscription_plan_state",
        "detail_json": json.dumps({"plan_id": "p1", "state": "disabled"}),
        "occurred_at_ns": time.time_ns(),
    }


def test_compliant_withdrawal_verifies(tmp_path):
    """All authoritative evidence present → PASS."""
    api = FakeApi(
        lambda: _device([_status("superseded")]),
        lambda: _assignment(0),
        events=[_plan_state_event()],
    )
    ok, by_stage = _run(api, tmp_path)
    assert ok is True
    assert by_stage["withdrawal_verified"]["ok"] is True
    for sub in ("binding_revoked", "acquisition_disabled", "issued",
                "collector_ack", "subscription_terminated",
                "no_continued_collection", "old_session_rejected"):
        assert by_stage[f"withdrawal_{sub}"]["ok"] is True, sub


def test_noncompliant_collector_fails(tmp_path):
    """Collector keeps streaming after revoke: subscription_count
    stays >0 AND observed timestamps keep advancing. The old
    frozen-timestamp check passed here; the authoritative check MUST
    fail."""
    counter = itertools.count(1_000)

    def device_fn():
        # 'observed' forever, last_observed advancing every read —
        # exactly what continued unauthorized collection looks like.
        n = next(counter) * 1_000_000_000
        return _device([_status("observed", obs_ns=n, upd_ns=n)])

    api = FakeApi(
        device_fn,
        lambda: _assignment(1),  # still one live gNMI subscription
        events=[_plan_state_event()],
    )
    ok, by_stage = _run(api, tmp_path)
    assert ok is False
    assert by_stage["withdrawal_verified"]["ok"] is False
    assert by_stage["withdrawal_collector_ack"]["ok"] is False
    # quiesce never happens: continued collection is never disproven
    assert by_stage["withdrawal_no_continued_collection"]["ok"] is False


def test_frozen_timestamps_but_live_subscription_fails(tmp_path):
    """Timestamps frozen (the old check's only signal) but the
    collector still reports a live subscription — noncompliance is
    caught by collector evidence, not timestamp silence."""
    api = FakeApi(
        lambda: _device([_status("superseded")]),
        lambda: _assignment(1),
        events=[_plan_state_event()],
    )
    ok, by_stage = _run(api, tmp_path)
    assert ok is False
    assert by_stage["withdrawal_collector_ack"]["ok"] is False


def test_status_writes_advancing_post_quiesce_fails(tmp_path):
    """Durable statuses sit in terminal states but keep being
    rewritten (updated_at_ns advances) — evidence of continued
    processing after withdrawal. Must fail the stability window."""
    counter = itertools.count(2_000)

    def device_fn():
        return _device([_status("superseded",
                                upd_ns=next(counter) * 1_000_000)])

    api = FakeApi(
        device_fn,
        lambda: _assignment(0),
        events=[_plan_state_event()],
    )
    ok, by_stage = _run(api, tmp_path)
    assert ok is False
    assert by_stage["withdrawal_no_continued_collection"]["ok"] is False


def test_missing_plan_state_and_nonterminal_statuses_fail(tmp_path):
    """No SSE plan-state event AND statuses never reached terminal
    state → withdrawal_issued cannot be proven → FAIL."""
    api = FakeApi(
        lambda: _device([_status("pending")]),
        lambda: _assignment(0),
        events=[],  # SSE delivers nothing
    )
    ok, by_stage = _run(api, tmp_path)
    assert ok is False
    assert by_stage["withdrawal_issued"]["ok"] is False
    assert by_stage["withdrawal_subscription_terminated"]["ok"] is False


def test_binding_still_present_fails(tmp_path):
    """Revocation never landed: schema_binding still populated →
    binding_revoked fails even if everything else looks clean."""
    api = FakeApi(
        lambda: _device([_status("superseded")],
                        binding={"mode": "schema_bound",
                                 "schema_id": "ysc-x"}),
        lambda: _assignment(0),
        events=[_plan_state_event()],
    )
    ok, by_stage = _run(api, tmp_path)
    assert ok is False
    assert by_stage["withdrawal_binding_revoked"]["ok"] is False
