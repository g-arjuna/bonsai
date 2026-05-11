#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/smoke/common.sh
source "${SCRIPT_DIR}/common.sh" "${1:-http://127.0.0.1:3000}"

TOPOLOGY="$(api_call GET "/api/topology")" || finish_fail "synthesizer" "topology endpoint unavailable"

ADDRESS="$(python3 - <<'PY' "${TOPOLOGY}"
import json
import sys

payload = json.loads(sys.argv[1])
devices = payload.get("devices", [])
print(devices[0]["address"] if devices else "")
PY
)"

if [[ -z "${ADDRESS}" ]]; then
  finish_skip "synthesizer" "no managed devices available for synthesizer smoke"
fi

READINESS="$(api_call GET "/api/devices/${ADDRESS}/gnmi-readiness")" \
  || finish_fail "synthesizer" "gNMI readiness endpoint failed for ${ADDRESS}"
RECOMMENDATIONS="$(api_call GET "/api/devices/${ADDRESS}/recommendations")" \
  || finish_fail "synthesizer" "recommendations endpoint failed for ${ADDRESS}"

DETAILS="$(python3 - <<'PY' "${ADDRESS}" "${READINESS}" "${RECOMMENDATIONS}"
import json
import sys

address, readiness_raw, recommendations_raw = sys.argv[1:]
readiness = json.loads(readiness_raw)
recommendations = json.loads(recommendations_raw)
report = recommendations.get("report", {})

assert readiness["address"] == address
assert "report" in readiness
assert isinstance(report.get("warnings", []), list)

details = [
    {"check": "gnmi_readiness", "status": "pass", "address": address},
    {
        "check": "recommendations",
        "status": "pass",
        "matched_profiles": len(report.get("recommended_profiles", [])),
        "warnings": len(report.get("warnings", [])),
        "blockers": len(report.get("blockers", [])),
    },
]
print(json.dumps(details))
PY
)" || finish_fail "synthesizer" "synthesizer response shape invalid for ${ADDRESS}"

finish_pass "synthesizer" "validated readiness and synthesizer recommendation endpoints for ${ADDRESS}" "${DETAILS}"
