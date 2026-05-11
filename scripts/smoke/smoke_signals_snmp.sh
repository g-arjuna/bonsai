#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/smoke/common.sh
source "${SCRIPT_DIR}/common.sh" "${1:-http://127.0.0.1:3000}"

ENABLED="$(read_config_value "signals.snmp.enabled")"
ARCHIVE_PATH="$(read_config_value "signals.snmp.archive_path")"
[[ -n "${ARCHIVE_PATH}" ]] || ARCHIVE_PATH="runtime/signals/snmp.jsonl"

if [[ "${ENABLED}" != "true" ]]; then
  finish_skip "signals_snmp" "snmp receiver disabled in ${BONSAI_CONFIG:-bonsai.toml}"
fi

STATUS="$(api_call GET "/api/_test/status")" || finish_fail "signals_snmp" "status endpoint unavailable"
FULL_ARCHIVE_PATH="${REPO_ROOT}/${ARCHIVE_PATH}"

DETAILS="$(python3 - <<'PY' "${STATUS}" "${FULL_ARCHIVE_PATH}"
import json
import os
import sys

status_raw, archive_path = sys.argv[1:]
status = json.loads(status_raw)

assert "memory" in status and "disk" in status

details = [
    {"check": "status_endpoint", "status": "pass"},
    {
        "check": "archive_path",
        "status": "pass" if os.path.exists(archive_path) else "warn",
        "path": archive_path,
    },
]
print(json.dumps(details))
PY
)" || finish_fail "signals_snmp" "snmp smoke validation failed"

finish_pass "signals_snmp" "validated snmp signal status path and archive path visibility" "${DETAILS}"
