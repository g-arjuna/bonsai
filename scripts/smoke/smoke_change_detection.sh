#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/smoke/common.sh
source "${SCRIPT_DIR}/common.sh" "${1:-http://127.0.0.1:3000}"

TOPOLOGY="$(api_call GET "/api/topology")" || finish_fail "change_detection" "topology endpoint unavailable"

ADDRESS="$(python3 - <<'PY' "${TOPOLOGY}"
import json
import sys

payload = json.loads(sys.argv[1])
devices = payload.get("devices", [])
print(devices[0]["address"] if devices else "")
PY
)"

if [[ -z "${ADDRESS}" ]]; then
  finish_skip "change_detection" "no managed devices available for change-detection smoke"
fi

REPARSE="$(api_call POST "/api/devices/${ADDRESS}/reparse" '{"reason":"sprint2 smoke change-detection"}')" \
  || finish_fail "change_detection" "manual reparse request failed for ${ADDRESS}"
sleep 2
CONFIG_HISTORY="$(api_call GET "/api/devices/${ADDRESS}/config-history")" \
  || finish_fail "change_detection" "config-history endpoint failed for ${ADDRESS}"

DETAILS="$(python3 - <<'PY' "${ADDRESS}" "${REPARSE}" "${CONFIG_HISTORY}"
import json
import sys

address, reparse_raw, history_raw = sys.argv[1:]
reparse = json.loads(reparse_raw)
history = json.loads(history_raw)

assert reparse["success"] is True
assert history["address"] == address
assert isinstance(history.get("snapshots", []), list)
assert isinstance(history.get("changes", []), list)

details = [
    {"check": "manual_reparse", "status": "pass", "message": reparse["message"]},
    {
        "check": "config_history",
        "status": "pass",
        "snapshots": len(history.get("snapshots", [])),
        "changes": len(history.get("changes", [])),
    },
]
print(json.dumps(details))
PY
)" || finish_fail "change_detection" "change-detection response shape invalid for ${ADDRESS}"

finish_pass "change_detection" "validated manual reparse and config-history flow for ${ADDRESS}" "${DETAILS}"
