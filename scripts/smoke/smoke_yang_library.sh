#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/smoke/common.sh
source "${SCRIPT_DIR}/common.sh" "${1:-http://127.0.0.1:3000}"

MODULES="$(api_call GET "/api/yang/modules")" || finish_fail "yang_library" "yang modules endpoint unavailable"
SEARCH="$(api_call GET "/api/yang/search?q=interface")" || finish_fail "yang_library" "yang search endpoint unavailable"

DETAILS="$(python3 - <<'PY' "${MODULES}" "${SEARCH}"
import json
import sys

modules_raw, search_raw = sys.argv[1:]
modules = json.loads(modules_raw)
search = json.loads(search_raw)

assert isinstance(modules.get("modules", []), list)
assert "result" in search

details = [
    {"check": "modules", "status": "pass", "module_count": len(modules.get("modules", []))},
    {"check": "search", "status": "pass", "query": "interface"},
]
print(json.dumps(details))
PY
)" || finish_fail "yang_library" "yang library response shape invalid"

finish_pass "yang_library" "validated YANG catalogue list and search endpoints" "${DETAILS}"
