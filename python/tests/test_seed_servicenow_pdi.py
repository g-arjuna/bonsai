"""Tests for scripts/seed_servicenow_pdi.py (T0-2 / Q-6, Q-7, Q-8 fixes)."""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from unittest.mock import MagicMock, patch, call

import pytest

# Load the script as a module without executing main()
_SCRIPT = Path(__file__).resolve().parent.parent.parent / "scripts" / "seed_servicenow_pdi.py"
spec = importlib.util.spec_from_file_location("seed_pdi", _SCRIPT)
_mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(_mod)
SnowClient = _mod.SnowClient


# ── Q-8: lookup uses limit=1 ─────────────────────────────────────────────────

def test_lookup_one_uses_limit_1():
    """_lookup_one must pass limit=1 so a large PDI can't cause missed pages."""
    calls = []

    def mock_get(table, query="", fields="", limit=500):
        calls.append(limit)
        return []

    client = SnowClient("https://dev.example.com", "admin", "pass")
    client.get = mock_get
    result = client._lookup_one("cmdb_ci_netgear", "name", "router-1")
    assert result is None
    assert calls == [1], f"expected limit=1, got {calls}"


def test_lookup_one_returns_first_match():
    def mock_get(table, query="", fields="", limit=500):
        return [{"sys_id": "abc", "name": "router-1"}]

    client = SnowClient("https://dev.example.com", "admin", "pass")
    client.get = mock_get
    result = client._lookup_one("cmdb_ci_netgear", "name", "router-1")
    assert result == {"sys_id": "abc", "name": "router-1"}


# ── Q-7: verify after upsert ─────────────────────────────────────────────────

def test_upsert_verifies_after_create(capsys):
    """After a POST, upsert must do a lookup GET and warn if record not found."""
    session_mock = MagicMock()
    session_mock.post.return_value.raise_for_status = MagicMock()
    session_mock.post.return_value.json.return_value = {"result": {"sys_id": "new123"}}

    client = SnowClient("https://dev.example.com", "admin", "pass")
    client.session = session_mock

    lookup_calls = []

    def mock_lookup_one(table, field, value):
        lookup_calls.append(value)
        # First call (existence check) returns None (new record)
        # Second call (verification) also returns None → triggers warning
        return None

    client._lookup_one = mock_lookup_one
    client.upsert("cmdb_ci_netgear", "name", "router-1", {"name": "router-1"})

    captured = capsys.readouterr()
    assert "WARNING" in captured.out, "warning expected when post-write lookup fails"
    assert len(lookup_calls) == 2, "lookup must be called twice (exist-check + verify)"


def test_upsert_verifies_after_patch_and_finds_record(capsys):
    """After a PATCH, upsert verifies and does NOT warn when record is found."""
    session_mock = MagicMock()
    session_mock.patch.return_value.raise_for_status = MagicMock()
    session_mock.patch.return_value.json.return_value = {"result": {"sys_id": "ex1"}}

    client = SnowClient("https://dev.example.com", "admin", "pass")
    client.session = session_mock

    lookup_calls = []

    def mock_lookup_one(table, field, value):
        lookup_calls.append(value)
        return {"sys_id": "ex1", "name": value}

    client._lookup_one = mock_lookup_one
    client.upsert("cmdb_ci_netgear", "name", "router-1", {"name": "router-1"})

    captured = capsys.readouterr()
    assert "WARNING" not in captured.out, "no warning expected when record found after write"


# ── Q-6: --use-vault exits with code 2 ───────────────────────────────────────

def test_use_vault_flag_exits_2():
    """--use-vault must exit with code 2 (not implemented)."""
    with pytest.raises(SystemExit) as exc:
        with patch("sys.argv", ["seed_pdi", "--use-vault"]):
            _mod.main()
    assert exc.value.code == 2


# ── idempotency: running twice should not warn about duplicates ───────────────

def test_upsert_dry_run_does_not_call_api():
    """In dry-run mode, no POST/PATCH calls should be made."""
    session_mock = MagicMock()
    client = SnowClient("https://dev.example.com", "admin", "pass", dry_run=True)
    client.session = session_mock
    client._lookup_one = lambda *_: None
    client.upsert("cmdb_ci_netgear", "name", "r1", {"name": "r1"})
    session_mock.post.assert_not_called()
    session_mock.patch.assert_not_called()
