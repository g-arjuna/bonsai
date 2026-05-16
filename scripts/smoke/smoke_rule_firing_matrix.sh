#!/usr/bin/env bash
# smoke_rule_firing_matrix.sh — D2-T4 (DV1)
#
# Reads tests/rule_firing_matrix.yaml and verifies each rule fires (or not)
# for its specified inputs. Uses the Python rule classes directly — no gRPC,
# no live lab required.
#
# Usage:
#   bash scripts/smoke/smoke_rule_firing_matrix.sh [--rule RULE_ID]
#
# Options:
#   --rule RULE_ID   only run cases for the specified rule_id
#
# Exit codes:
#   0  all cases passed
#   1  one or more cases failed

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
MATRIX="$REPO_ROOT/tests/rule_firing_matrix.yaml"
FILTER_RULE=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --rule) FILTER_RULE="$2"; shift ;;
        *) echo "Unknown arg: $1"; exit 1 ;;
    esac
    shift
done

PY=$(command -v python3 || command -v python)
if [[ -z "$PY" ]]; then
    echo "ERROR: python3 not found" >&2; exit 1
fi

echo "=== Rule Firing Matrix Smoke ==="
echo "    matrix : $MATRIX"
[[ -n "$FILTER_RULE" ]] && echo "    filter : $FILTER_RULE"
echo ""

"$PY" - "$MATRIX" "$FILTER_RULE" "$REPO_ROOT" <<'PYEOF'
import sys
import json
import types
import yaml

matrix_path, filter_rule, repo_root = sys.argv[1], sys.argv[2], sys.argv[3]

sys.path.insert(0, repo_root + "/python")

# ── lazy imports (import only what we need) ───────────────────────────────────
from bonsai_sdk.rules.bfd import BFD_RULES
from bonsai_sdk.rules.bgp import BGP_RULES
from bonsai_sdk.rules.interface import INTERFACE_RULES
from bonsai_sdk.rules.snmp import SNMP_RULES
from bonsai_sdk.rules.syslog import SYSLOG_RULES
from bonsai_sdk.rules.streaming import STREAMING_RULES
from bonsai_sdk.rules.topology import TOPOLOGY_RULES

ALL_RULES = BFD_RULES + BGP_RULES + INTERFACE_RULES + SNMP_RULES + SYSLOG_RULES + STREAMING_RULES
RULE_MAP = {r.rule_id: r for r in ALL_RULES}

# TOPOLOGY_RULES is a class with class-level evaluate_topology (poll-only).
# It cannot be exercised via synthetic events; skip gracefully.
POLL_ONLY = {"topology_edge_lost", "srlg_risk_detected",
             "interface_error_spike", "interface_high_utilization"}


class FakeClient:
    def get_bgp_neighbors(self, _addr):
        return []


def make_event(rule_id, case):
    """Build a SimpleNamespace that looks like a pb.StateEvent to the rule."""
    detail = case.get("detail", {})
    e = types.SimpleNamespace()
    e.event_type       = case.get("event_type", "")
    e.device_address   = case.get("device_address", "10.0.0.1")
    e.detail_json      = json.dumps(detail)
    e.occurred_at_ns   = 0
    e.state_change_event_id = ""
    # Patch in detail fields as top-level attrs for rules that access them directly.
    for k, v in detail.items():
        if not hasattr(e, k):
            setattr(e, k, v)
    return e


_CLIENT = FakeClient()

RED   = "\033[0;31m"
GREEN = "\033[0;32m"
BOLD  = "\033[1m"
RESET = "\033[0m"

with open(matrix_path) as f:
    matrix = yaml.safe_load(f)

total_pass = total_fail = total_skip = 0

for entry in matrix:
    rule_id = entry["rule_id"]
    if filter_rule and rule_id != filter_rule:
        continue

    print(f"{BOLD}rule: {rule_id}{RESET}")

    if rule_id in POLL_ONLY:
        print(f"  SKIP (poll-only — exercised via poll loop, not synthetic events)\n")
        total_skip += 1
        continue

    rule = RULE_MAP.get(rule_id)
    if rule is None:
        print(f"  {RED}MISSING{RESET} — rule_id not found in loaded rules\n")
        total_fail += 1
        continue

    # ── fires_on ──────────────────────────────────────────────────────────────
    for case in entry.get("fires_on", []):
        label = case.get("label", "?")
        if case.get("skip_synthetic"):
            reason_txt = case.get("reason", "requires live graph or stateful registry")
            print(f"  SKIP fires_on  [{label}]: {reason_txt}")
            total_skip += 1
            continue
        ev = make_event(rule_id, case)
        try:
            features = rule.extract_features(ev, _CLIENT)
            if features is None:
                print(f"  {RED}FAIL{RESET} fires_on [{label}]: extract_features returned None")
                total_fail += 1
                continue
            reason = rule.detect(features)
            if reason:
                print(f"  {GREEN}PASS{RESET} fires_on  [{label}]")
                total_pass += 1
            else:
                print(f"  {RED}FAIL{RESET} fires_on [{label}]: detect() returned None")
                total_fail += 1
        except Exception as exc:
            print(f"  {RED}FAIL{RESET} fires_on [{label}]: exception: {exc}")
            total_fail += 1

    # ── does_not_fire_on ──────────────────────────────────────────────────────
    for case in entry.get("does_not_fire_on", []):
        label = case.get("label", "?")
        if case.get("skip_synthetic"):
            reason_txt = case.get("reason", "requires live graph or stateful registry")
            print(f"  SKIP no-fire   [{label}]: {reason_txt}")
            total_skip += 1
            continue
        ev = make_event(rule_id, case)
        try:
            features = rule.extract_features(ev, _CLIENT)
            if features is None:
                print(f"  {GREEN}PASS{RESET} no-fire  [{label}]")
                total_pass += 1
                continue
            reason = rule.detect(features)
            if reason is None:
                print(f"  {GREEN}PASS{RESET} no-fire  [{label}]")
                total_pass += 1
            else:
                print(f"  {RED}FAIL{RESET} no-fire [{label}]: rule fired unexpectedly: {reason!r}")
                total_fail += 1
        except Exception as exc:
            print(f"  {RED}FAIL{RESET} no-fire [{label}]: exception: {exc}")
            total_fail += 1

    print()

print(f"=== Results: PASS={total_pass}  FAIL={total_fail}  SKIP={total_skip} ===")
sys.exit(0 if total_fail == 0 else 1)
PYEOF
