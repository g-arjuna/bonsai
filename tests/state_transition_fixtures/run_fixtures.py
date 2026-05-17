#!/usr/bin/env python3
"""State transition fixture runner — D2-2 T3.

Validates that the state_mapping adapter produces the expected is_down/is_up
results for every fixture in tests/state_transition_fixtures/**/*.yaml.

Usage:
    python tests/state_transition_fixtures/run_fixtures.py

Exits 0 if all fixtures pass; 1 if any fail.
Mac-side safe: no bonsai process or lab required.
"""
from __future__ import annotations

import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(REPO_ROOT / "python"))

from bonsai_sdk.state_mapping import is_down, is_up  # noqa: E402

try:
    import yaml  # type: ignore
except ImportError:
    print("FAIL: PyYAML not available. pip install pyyaml", file=sys.stderr)
    sys.exit(1)

FIXTURES_DIR = Path(__file__).parent

RULE_LEAF_MAP = {
    "bfd_session_down":  "bfd_oper_state",
    "interface_down":    "interface_oper_status",
    "bgp_session_down":  "bgp_session_state",
    "bgp_session_flap":  "bgp_session_state",
}

pass_count = 0
fail_count = 0
skip_count = 0

for fixture_file in sorted(FIXTURES_DIR.rglob("*.yaml")):
    with fixture_file.open(encoding="utf-8") as fh:
        data = yaml.safe_load(fh)

    vendor = data.get("vendor", "")
    rule_id = data.get("rule_id", "")
    leaf = RULE_LEAF_MAP.get(rule_id)

    if not leaf:
        print(f"SKIP {fixture_file.relative_to(FIXTURES_DIR)}: no leaf mapping for rule_id={rule_id!r}")
        skip_count += 1
        continue

    for fx in data.get("fixtures", []):
        fx_id = fx.get("id", "?")
        desc = fx.get("description", "")
        expect_fires = fx.get("expect_fires", None)

        # Determine the state value to test depending on rule type
        if rule_id in ("bfd_session_down",):
            test_state = fx.get("new_state", "")
            old_state = fx.get("old_state", "")
            event_type = fx.get("event_type", "")
            # Wrong event type → never fires
            if event_type not in ("bfd_session_change",):
                actual_fires = False
            else:
                fires_new = is_down(vendor, leaf, test_state)
                fires_old = is_up(vendor, leaf, old_state) or old_state == "none"
                actual_fires = fires_new and fires_old

        elif rule_id == "interface_down":
            test_state = fx.get("oper_status", "")
            event_type = fx.get("event_type", "")
            if event_type != "interface_oper_status_change":
                actual_fires = False
            else:
                actual_fires = is_down(vendor, leaf, test_state)

        elif rule_id in ("bgp_session_down", "bgp_session_flap"):
            new_state = fx.get("new_state", "")
            old_state = fx.get("old_state", "")
            event_type = fx.get("event_type", "")
            if event_type != "bgp_session_change":
                actual_fires = False
            else:
                actual_fires = is_down(vendor, leaf, new_state) and is_up(vendor, leaf, old_state)

        else:
            actual_fires = False

        ok = (actual_fires == expect_fires)
        status = "PASS" if ok else "FAIL"
        if ok:
            pass_count += 1
        else:
            fail_count += 1

        marker = "✓" if ok else "✗"
        print(f"  {marker} {status}  {fixture_file.parent.name}/{fixture_file.stem}::{fx_id}")
        if not ok:
            print(f"       expected fires={expect_fires}  got fires={actual_fires}")
            print(f"       {desc}")

print()
print(f"Results: PASS={pass_count}  FAIL={fail_count}  SKIP={skip_count}")
sys.exit(0 if fail_count == 0 else 1)
