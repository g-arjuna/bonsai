#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/smoke/common.sh
source "${SCRIPT_DIR}/common.sh" "${1:-http://127.0.0.1:3000}"

INCIDENTS="$(api_call GET "/api/incidents")" || finish_fail "servicenow_aiops" "incidents endpoint unavailable"
APPROVALS="$(api_call GET "/api/approvals")" || finish_fail "servicenow_aiops" "approvals endpoint unavailable"
TRUST="$(api_call GET "/api/trust")" || finish_fail "servicenow_aiops" "trust endpoint unavailable"

DETAILS="$(python3 - <<'PY' "${INCIDENTS}" "${APPROVALS}" "${TRUST}"
import json
import sys

incidents_raw, approvals_raw, trust_raw = sys.argv[1:]
incidents = json.loads(incidents_raw)
approvals = json.loads(approvals_raw)
trust = json.loads(trust_raw)

assert isinstance(incidents.get("incidents", []), list)
assert isinstance(approvals.get("proposals", []), list)
assert isinstance(trust.get("trust", []), list)

details = [
    {"check": "incidents", "status": "pass", "incident_count": len(incidents.get("incidents", []))},
    {"check": "approvals", "status": "pass", "proposal_count": len(approvals.get("proposals", []))},
    {"check": "trust", "status": "pass", "trust_entries": len(trust.get("trust", []))},
]
print(json.dumps(details))
PY
)" || finish_fail "servicenow_aiops" "incident/trust response shape invalid"

finish_pass "servicenow_aiops" "validated read-only incident, approvals, and trust surfaces" "${DETAILS}"
