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

# ── Inject + push-verify ──────────────────────────────────────────────────────
# Only run when at least one adapter is configured; skip otherwise.
ADAPTER_COUNT="$(python3 -c "import json,sys; print(len(json.loads(sys.argv[1]).get('adapters',[])))" "${ADAPTERS}")"

if [[ "${ADAPTER_COUNT}" -eq 0 ]]; then
    DETAILS="$(python3 - <<'PY' "${DETAILS}"
import json, sys
d = json.loads(sys.argv[1])
d.append({"check": "inject_push_verify", "status": "skip", "reason": "no adapters configured"})
print(json.dumps(d))
PY
)"
    finish_pass "output_adapters" "validated output adapter list and audit surfaces (no adapters to inject-test)" "${DETAILS}"
    exit 0
fi

# Inject a synthetic detection via the test endpoint
INJECT_RESP="$(api_call POST "/api/_test/inject_detection" \
    '{"device_address":"10.0.0.1","rule_id":"smoke_inject_test","severity":"info"}')" \
    || finish_fail "output_adapters" "inject_detection endpoint unavailable"

FIRED_AT_NS="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get('fired_at_ns',0))" "${INJECT_RESP}")"

# Poll /api/adapters for up to 30s waiting for any adapter to record a push
# after the injection timestamp.
PUSH_SEEN=false
for _i in $(seq 1 6); do
    sleep 5
    ADAPTERS_NOW="$(api_call GET "/api/adapters" 2>/dev/null)" || continue
    PUSH_SEEN="$(python3 - <<'PY' "${ADAPTERS_NOW}" "${FIRED_AT_NS}"
import json, sys
adapters_raw, fired_ns_str = sys.argv[1:]
adapters = json.loads(adapters_raw).get("adapters", [])
fired_ns = int(fired_ns_str)
for a in adapters:
    st = a.get("state") or {}
    last = st.get("last_push_at_ns") or 0
    if isinstance(last, int) and last >= fired_ns:
        print("true")
        raise SystemExit(0)
print("false")
PY
)"
    if [[ "${PUSH_SEEN}" == "true" ]]; then
        break
    fi
done

INJECT_STATUS="pass"
INJECT_REASON="push observed within 30s of injection"
if [[ "${PUSH_SEEN}" != "true" ]]; then
    INJECT_STATUS="fail"
    INJECT_REASON="no adapter recorded a push within 30s of injection"
fi

DETAILS="$(python3 - <<'PY' "${DETAILS}" "${INJECT_STATUS}" "${INJECT_REASON}"
import json, sys
d = json.loads(sys.argv[1])
status, reason = sys.argv[2], sys.argv[3]
d.append({"check": "inject_push_verify", "status": status, "reason": reason})
print(json.dumps(d))
PY
)"

if [[ "${INJECT_STATUS}" == "fail" ]]; then
    finish_fail "output_adapters" "adapter push not observed within 30s" "${DETAILS}"
fi

finish_pass "output_adapters" "validated output adapter list, audit, and push pipeline" "${DETAILS}"
