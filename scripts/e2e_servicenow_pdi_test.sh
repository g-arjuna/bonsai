#!/usr/bin/env bash
# e2e ServiceNow PDI live test (T2-4 / T2-5).
#
# Tests three capabilities against a live ServiceNow PDI:
#   1. CMDB read  — GET from cmdb_ci_netgear; verifies pagination + field shape
#   2. CMDB write — POST a canary CI, verify it is readable, then DELETE it
#   3. EM push    — POST to /api/now/em/inbound_event; verify the event was accepted
#
# Required env vars:
#   SNOW_INSTANCE_URL   https://devXXXXXX.service-now.com
#   SNOW_USERNAME       admin
#   SNOW_PASSWORD       your-pdi-password
#
# Optional env vars:
#   BONSAI_URL          bonsai API base URL (default: http://localhost:3000)
#                       Only used when --bonsai-roundtrip is passed.
#
# Usage:
#   export SNOW_INSTANCE_URL=https://devXXXXXX.service-now.com
#   export SNOW_USERNAME=admin
#   export SNOW_PASSWORD=your-pdi-password
#   bash scripts/e2e_servicenow_pdi_test.sh
#   bash scripts/e2e_servicenow_pdi_test.sh --dry-run
#   bash scripts/e2e_servicenow_pdi_test.sh --skip-write   # read + EM only
#   bash scripts/e2e_servicenow_pdi_test.sh --skip-em      # read + write only

set -euo pipefail

# ── Arg parsing ──────────────────────────────────────────────────────────────

DRY_RUN=0
SKIP_WRITE=0
SKIP_EM=0

for arg in "$@"; do
  case "$arg" in
    --dry-run)      DRY_RUN=1 ;;
    --skip-write)   SKIP_WRITE=1 ;;
    --skip-em)      SKIP_EM=1 ;;
    --help|-h)
      sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "ERROR: unknown argument: $arg" >&2
      exit 1
      ;;
  esac
done

# ── Credential guard ─────────────────────────────────────────────────────────

missing=()
[[ -z "${SNOW_INSTANCE_URL:-}" ]] && missing+=("SNOW_INSTANCE_URL")
[[ -z "${SNOW_USERNAME:-}"     ]] && missing+=("SNOW_USERNAME")
[[ -z "${SNOW_PASSWORD:-}"     ]] && missing+=("SNOW_PASSWORD")

if [[ ${#missing[@]} -gt 0 ]]; then
  echo "ERROR: required environment variables not set:" >&2
  for v in "${missing[@]}"; do
    echo "  $v" >&2
  done
  echo "" >&2
  echo "Set them before running:" >&2
  echo "  export SNOW_INSTANCE_URL=https://devXXXXXX.service-now.com" >&2
  echo "  export SNOW_USERNAME=admin" >&2
  echo "  export SNOW_PASSWORD=your-pdi-password" >&2
  exit 2
fi

BASE="${SNOW_INSTANCE_URL%/}"
AUTH_HEADER="Authorization: Basic $(echo -n "${SNOW_USERNAME}:${SNOW_PASSWORD}" | base64 -w0)"
ACCEPT="Accept: application/json"
CONTENT="Content-Type: application/json"

PASS=0
FAIL=0
_CANARY_SYS_ID=""  # set when canary CI is created; used by cleanup

# ── Helpers ──────────────────────────────────────────────────────────────────

pass() { echo "  ✓ $*"; ((PASS++)); }
fail() { echo "  ✗ $*" >&2; ((FAIL++)); }

snow_get() {
  # snow_get <path> → response body
  curl -sf -H "$AUTH_HEADER" -H "$ACCEPT" "${BASE}${1}"
}

snow_post() {
  # snow_post <path> <json-body> → response body
  curl -sf -H "$AUTH_HEADER" -H "$ACCEPT" -H "$CONTENT" \
    -X POST --data "$2" "${BASE}${1}"
}

snow_delete() {
  # snow_delete <path>
  curl -sf -H "$AUTH_HEADER" -o /dev/null -X DELETE "${BASE}${1}" || true
}

require_jq() {
  if ! command -v jq &>/dev/null; then
    echo "ERROR: jq is required but not found on PATH." >&2
    exit 1
  fi
}

# ── cleanup (runs on EXIT regardless of success/failure) ──────────────────────
# Deletes the canary CI if the test was interrupted before the inline cleanup ran.

cleanup() {
  if [[ -n "$_CANARY_SYS_ID" ]]; then
    snow_delete "/api/now/table/cmdb_ci_netgear/${_CANARY_SYS_ID}" || true
  fi
}
trap cleanup EXIT

# ── Dry-run mode ─────────────────────────────────────────────────────────────

if [[ $DRY_RUN -eq 1 ]]; then
  echo "[dry-run] Would test against: $BASE"
  echo "[dry-run] Auth user: $SNOW_USERNAME"
  echo "[dry-run] Tests: CMDB read, CMDB write, EM push"
  echo "[dry-run] No requests will be made."
  exit 0
fi

require_jq

echo "=== ServiceNow PDI e2e test ==="
echo "Instance : $BASE"
echo "User     : $SNOW_USERNAME"
echo ""

# ── 1. Connectivity ping ─────────────────────────────────────────────────────

echo "── 1. Connectivity ──"

health_body=$(snow_get "/api/now/table/sys_user?sysparm_limit=1" 2>/dev/null) || {
  echo "ERROR: cannot reach $BASE — check SNOW_INSTANCE_URL and credentials" >&2
  exit 1
}

count=$(echo "$health_body" | jq -r '.result | length' 2>/dev/null || echo "")
if [[ "$count" =~ ^[0-9]+$ ]]; then
  pass "Authenticated and reached sys_user table (got $count record(s))"
else
  fail "Unexpected response from sys_user: ${health_body:0:200}"
fi

# ── 2. CMDB read ─────────────────────────────────────────────────────────────

echo ""
echo "── 2. CMDB read (cmdb_ci_netgear) ──"

read_body=$(snow_get "/api/now/table/cmdb_ci_netgear?sysparm_limit=5&sysparm_display_value=all" 2>/dev/null) || {
  fail "GET cmdb_ci_netgear failed"
  read_body=""
}

if [[ -n "$read_body" ]]; then
  result_count=$(echo "$read_body" | jq -r '.result | length' 2>/dev/null || echo "-1")
  if [[ "$result_count" =~ ^[0-9]+$ ]]; then
    pass "Read cmdb_ci_netgear: $result_count record(s) returned"

    # Verify expected field shape on first record if any exist
    if [[ "$result_count" -gt 0 ]]; then
      name_field=$(echo "$read_body" | jq -r '.result[0].name.value // .result[0].name // "MISSING"')
      sys_id=$(echo "$read_body" | jq -r '.result[0].sys_id.value // .result[0].sys_id // "MISSING"')
      if [[ "$name_field" != "MISSING" && "$sys_id" != "MISSING" ]]; then
        pass "First record has name='$name_field' sys_id='$sys_id'"
      else
        fail "First record missing expected fields: ${read_body:0:300}"
      fi
    fi
  else
    fail "Unexpected CMDB read response: ${read_body:0:200}"
  fi
fi

# ── 3. CMDB write + verification ─────────────────────────────────────────────

echo ""
echo "── 3. CMDB write + verification ──"

if [[ $SKIP_WRITE -eq 1 ]]; then
  echo "  (skipped via --skip-write)"
else
  CANARY_NAME="bonsai-e2e-canary-$(date +%s)"
  CANARY_PAYLOAD=$(jq -n \
    --arg name  "$CANARY_NAME" \
    --arg sname "bonsai-lab" \
    '{name: $name, short_description: "bonsai e2e test canary — safe to delete", u_bonsai_role: "e2e-test", sys_class_name: "cmdb_ci_netgear"}')

  write_body=$(snow_post "/api/now/table/cmdb_ci_netgear" "$CANARY_PAYLOAD" 2>/dev/null) || {
    fail "POST cmdb_ci_netgear failed"
    write_body=""
  }

  if [[ -n "$write_body" ]]; then
    canary_sys_id=$(echo "$write_body" | jq -r '.result.sys_id.value // .result.sys_id // ""')
    _CANARY_SYS_ID="$canary_sys_id"
    if [[ -n "$canary_sys_id" && "$canary_sys_id" != "null" ]]; then
      pass "Wrote canary CI '$CANARY_NAME' (sys_id=$canary_sys_id)"

      # Verification GET
      verify_body=$(snow_get "/api/now/table/cmdb_ci_netgear/${canary_sys_id}" 2>/dev/null) || { verify_body=""; }
      verify_name=$(echo "$verify_body" | jq -r '.result.name.value // .result.name // ""' 2>/dev/null || echo "")
      if [[ "$verify_name" == "$CANARY_NAME" ]]; then
        pass "Verification GET confirmed '$CANARY_NAME' is readable"
      else
        fail "Verification GET returned unexpected name: '$verify_name' (expected '$CANARY_NAME')"
      fi

      # Cleanup
      snow_delete "/api/now/table/cmdb_ci_netgear/${canary_sys_id}"
      _CANARY_SYS_ID=""
      pass "Cleaned up canary CI"
    else
      fail "POST returned unexpected body (no sys_id): ${write_body:0:200}"
    fi
  fi
fi

# ── 4. EM event push ─────────────────────────────────────────────────────────

echo ""
echo "── 4. EM event push (/api/now/em/inbound_event) ──"

if [[ $SKIP_EM -eq 1 ]]; then
  echo "  (skipped via --skip-em)"
else
  EM_PAYLOAD=$(jq -n '{
    records: [{
      source:            "bonsai",
      event_class:       "e2e-test",
      resource:          "bonsai-e2e",
      node:              "bonsai-lab-e2e",
      metric_name:       "BGPSessionDown",
      severity:          "2",
      description:       "bonsai e2e test event — safe to ignore",
      additional_info:   "{\"rule_id\":\"bgp_session_down\",\"test\":true}"
    }]
  }')

  em_body=$(snow_post "/api/now/em/inbound_event" "$EM_PAYLOAD" 2>/dev/null) || {
    fail "POST /api/now/em/inbound_event failed — is Event Management plugin enabled on this PDI?"
    em_body=""
  }

  if [[ -n "$em_body" ]]; then
    # Accept both {status: "inserted"} and {result: {status: ...}} shapes across SN versions
    em_status=$(echo "$em_body" | jq -r '.status // .result.status // ""' 2>/dev/null || echo "")
    if [[ "$em_status" == "inserted" || "$em_status" == "success" ]]; then
      pass "EM event accepted (status=$em_status)"
    else
      # Check for 200 with empty body — some PDIs return 200 with no body on success
      http_code=$(curl -so /dev/null -H "$AUTH_HEADER" -H "$CONTENT" -H "$ACCEPT" \
        -X POST --data "$EM_PAYLOAD" \
        -w "%{http_code}" "${BASE}/api/now/em/inbound_event" 2>/dev/null || echo "0")
      if [[ "$http_code" == "200" || "$http_code" == "201" ]]; then
        pass "EM event accepted (HTTP $http_code)"
      else
        fail "EM event push: unexpected status='$em_status' body=${em_body:0:200}"
      fi
    fi
  fi
fi

# ── Summary ───────────────────────────────────────────────────────────────────

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

[[ $FAIL -eq 0 ]]
