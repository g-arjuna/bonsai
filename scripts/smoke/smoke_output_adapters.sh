#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/smoke/common.sh
source "${SCRIPT_DIR}/common.sh" "${1:-http://127.0.0.1:3000}"

ADAPTERS="$(api_call GET "/api/adapters")" || finish_fail "output_adapters" "adapter list endpoint unavailable"
AUDIT="$(api_call GET "/api/adapters/audit")" || finish_fail "output_adapters" "adapter audit endpoint unavailable"

DETAILS="$(python3 - <<'PY' "${ADAPTERS}" "${AUDIT}"
import json
import sys

adapters_raw, audit_raw = sys.argv[1:]
adapters = json.loads(adapters_raw)
audit = json.loads(audit_raw)

assert isinstance(adapters.get("adapters", []), list)
assert isinstance(audit.get("entries", []), list)

details = [
    {"check": "adapter_list", "status": "pass", "adapter_count": len(adapters.get("adapters", []))},
    {"check": "adapter_audit", "status": "pass", "audit_entries": len(audit.get("entries", []))},
]
print(json.dumps(details))
PY
)" || finish_fail "output_adapters" "output adapter response shape invalid"

finish_pass "output_adapters" "validated output adapter list and audit surfaces" "${DETAILS}"
