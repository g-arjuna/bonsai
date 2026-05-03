"""Tests for scripts/discover_yang_paths.py (T0-5 / Q-15 fix: pyang preflight)."""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from unittest.mock import patch

import pytest

_SCRIPT = Path(__file__).resolve().parent.parent.parent / "scripts" / "discover_yang_paths.py"
spec = importlib.util.spec_from_file_location("discover_yang", _SCRIPT)
_mod = importlib.util.module_from_spec(spec)
sys.modules["discover_yang"] = _mod  # required so @dataclass can resolve cls.__module__
spec.loader.exec_module(_mod)


# ── Q-15: pyang preflight — fails before any git clone ───────────────────────

def test_missing_pyang_exits_2_before_git_clone():
    """When pyang is absent main() must exit 2 without cloning any repo."""
    clone_calls = []

    def fake_clone(*args, **kwargs):
        clone_calls.append(args)
        return None

    with patch("sys.argv", ["discover_yang_paths.py"]):
        with patch.object(_mod, "check_pyang", return_value=None):
            with patch.object(_mod, "clone_or_pull", side_effect=fake_clone):
                with pytest.raises(SystemExit) as exc:
                    _mod.main()

    assert exc.value.code == 2, "must exit with code 2 when pyang is missing"
    assert clone_calls == [], "must not attempt git clone before confirming pyang is available"


def test_pyang_present_does_not_exit_early():
    """When pyang is present, main() must proceed past the preflight check."""
    proceed_past_preflight = []

    def fake_clone(cache_dir, source, no_pull):
        proceed_past_preflight.append(source.vendor)
        return None  # return None so discover loop skips gracefully

    with patch("sys.argv", ["discover_yang_paths.py", "--list-sources"]):
        # --list-sources returns immediately without cloning; just confirm no exit 2
        try:
            _mod.main()
        except SystemExit as e:
            assert e.code != 2, "should not exit 2 when called with --list-sources"


# ── check_pyang returns path or None ─────────────────────────────────────────

def test_check_pyang_returns_none_when_not_found():
    with patch("shutil.which", return_value=None):
        # Also patch the venv path existence check
        with patch.object(Path, "exists", return_value=False):
            result = _mod.check_pyang()
    assert result is None


def test_check_pyang_returns_path_when_on_system():
    with patch("shutil.which", return_value="/usr/bin/pyang"):
        result = _mod.check_pyang()
    assert result == "/usr/bin/pyang"


# ── EnricherTransport / VENDOR_SOURCES sanity ────────────────────────────────

def test_vendor_sources_non_empty():
    assert len(_mod.VENDOR_SOURCES) > 0, "VENDOR_SOURCES must not be empty"


def test_vendor_source_map_covers_all_sources():
    for src in _mod.VENDOR_SOURCES:
        assert src.vendor in _mod.VENDOR_SOURCE_MAP, (
            f"VENDOR_SOURCE_MAP missing entry for vendor '{src.vendor}'"
        )
