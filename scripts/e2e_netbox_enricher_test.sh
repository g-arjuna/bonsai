#!/usr/bin/env bash
# T2-3 (v10) — NetBox enricher live integration test
#
# Verifies the NetBox enricher against the current HTTP API surface:
#   1. Seed NetBox with the standard lab topology
#   2. Create a temporary credential alias in the Bonsai vault
#   3. Register a temporary NetBox enricher config
#   4. Test connectivity
#   5. Trigger a manual enrichment run
#   6. Verify run state reports touched nodes
#
# Produces:
#   docs/test_results/e2e_netbox/$(date +%Y%m%d)-<pass|fail>.md

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESULT_DIR="${REPO_ROOT}/docs/test_results/e2e_netbox"
COMPOSE_FILE="${REPO_ROOT}/docker-compose.yml"
BONSAI_HTTP="${BONSAI_HTTP:-http://localhost:3000}"
NETBOX_URL="${NETBOX_URL:-http://localhost:8000}"
NETBOX_URL_BONSAI="${NETBOX_URL_BONSAI:-}"
NETBOX_TOKEN="${NETBOX_TOKEN:-${NETBOX_API_TOKEN:-bonsai-dev-token}}"
LOG_FILE="/tmp/bonsai-e2e-netbox-$(date +%Y%m%d-%H%M%S)-$$.log"
DRY_RUN=false
RESULT="PASS"
SUMMARY_DETAIL="not_run"
RUN_TS="$(date +%s)"
ENRICHER_NAME="netbox-lab-test-${RUN_TS}"
CREDENTIAL_ALIAS="netbox-e2e-${RUN_TS}"
NODES_TOUCHED=0
WARNINGS_COUNT=0
RESULT_WRITTEN=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) DRY_RUN=true ;;
        --bonsai-http) BONSAI_HTTP="$2"; shift ;;
        --netbox-url) NETBOX_URL="$2"; shift ;;
        --netbox-token) NETBOX_TOKEN="$2"; shift ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
    shift
done

log()  { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG_FILE"; }
fail() { log "FAIL: $*"; RESULT="FAIL"; }
pass() { log "PASS: $*"; }
set_summary() { SUMMARY_DETAIL="$*"; }

resolve_bonsai_endpoint_url() {
    local url="$1"
    python3 - <<'PY' "$url"
from urllib.parse import urlparse, urlunparse
import sys

raw = sys.argv[1]
parsed = urlparse(raw)
host = parsed.hostname
if host in {"localhost", "127.0.0.1"}:
    netloc = parsed.netloc.replace(host, "host.docker.internal", 1)
    parsed = parsed._replace(netloc=netloc)
print(urlunparse(parsed))
PY
}

bonsai_runs_in_container() {
    command -v docker >/dev/null 2>&1 || return 1
    docker compose -f "${COMPOSE_FILE}" ps -q bonsai-core bonsai-all bonsai-lab-dc bonsai-lab-sp 2>/dev/null | grep -q .
}

json_success() {
    python3 - <<'PY' "$1"
import json
import sys

payload = json.loads(sys.argv[1])
raise SystemExit(0 if payload.get("success") else 1)
PY
}

write_result() {
    [[ "${RESULT_WRITTEN}" == "true" ]] && return 0
    mkdir -p "${RESULT_DIR}"
    RESULT_FILE="${RESULT_DIR}/$(date +%Y%m%d)-${RESULT,,}.md"
    BONSAI_SHA="$(git -C "${REPO_ROOT}" rev-parse --short HEAD 2>/dev/null || echo "unknown")"
    NETBOX_VER="$(curl -sf "${NETBOX_URL}/api/" -H "Authorization: Token ${NETBOX_TOKEN}" | jq -r '.["netbox-version"] // "unknown"' 2>/dev/null || echo "unknown")"

    cat > "${RESULT_FILE}" <<EOF
# NetBox Enricher E2E integration test

**Date**: $(date +%Y-%m-%d)
**Operator**: $(git config user.name 2>/dev/null || echo "unknown")
**Bonsai version**: ${BONSAI_SHA}
**NetBox URL**: ${NETBOX_URL}
**NetBox version**: ${NETBOX_VER}
**Topology source**: lab/seed/topology.yaml

## Result

**${RESULT}**

## Summary

${SUMMARY_DETAIL}

| Check | Value |
|-------|-------|
| Credential alias created | ${CREDENTIAL_ALIAS} |
| Enricher name | ${ENRICHER_NAME} |
| Nodes touched | ${NODES_TOUCHED} |
| Warning count | ${WARNINGS_COUNT} |

## Log

\`${LOG_FILE}\`
EOF

    RESULT_WRITTEN=true
    log "Result written to: ${RESULT_FILE}"
}

cleanup() {
    curl -sf -X POST "${BONSAI_HTTP}/api/enrichment/remove" \
        -H "Content-Type: application/json" \
        -d "{\"name\":\"${ENRICHER_NAME}\"}" >>"$LOG_FILE" 2>&1 || true
    curl -sf -X POST "${BONSAI_HTTP}/api/credentials/remove" \
        -H "Content-Type: application/json" \
        -d "{\"alias\":\"${CREDENTIAL_ALIAS}\"}" >>"$LOG_FILE" 2>&1 || true
    write_result || true
}
trap cleanup EXIT

wait_for_http() {
    local url="$1" max="$2" elapsed=0
    while ! curl -sf "$url" >/dev/null 2>&1; do
        sleep 3
        elapsed=$((elapsed + 3))
        [[ $elapsed -ge $max ]] && return 1
    done
}

wait_for_netbox_api() {
    local max="$1" elapsed=0
    while ! curl -sf "${NETBOX_URL}/api/" -H "Authorization: Token ${NETBOX_TOKEN}" >/dev/null 2>&1; do
        sleep 3
        elapsed=$((elapsed + 3))
        [[ $elapsed -ge $max ]] && return 1
    done
}

ensure_netbox() {
    if wait_for_netbox_api 5; then
        return 0
    fi
    if command -v docker >/dev/null 2>&1; then
        log "NetBox not reachable at ${NETBOX_URL}; attempting docker compose start..."
        docker compose -f "${REPO_ROOT}/docker/compose-external.yml" --profile netbox up -d >>"$LOG_FILE" 2>&1 || true
        wait_for_netbox_api 120
        return $?
    fi
    return 1
}

enricher_entry_json() {
    python3 - <<'PY' "$1" "$2"
import json
import sys

payload = json.loads(sys.argv[1])
name = sys.argv[2]

for entry in payload.get("enrichers", []):
    config = entry.get("config", {})
    if config.get("name") == name:
        print(json.dumps(entry))
        raise SystemExit(0)

print("{}")
PY
}

log "=== Bonsai NetBox Enricher E2E Test ==="

if [[ -z "${NETBOX_URL_BONSAI}" ]]; then
    if bonsai_runs_in_container; then
        NETBOX_URL_BONSAI="$(resolve_bonsai_endpoint_url "${NETBOX_URL}")"
    else
        NETBOX_URL_BONSAI="${NETBOX_URL}"
    fi
fi
log "Using local NetBox URL: ${NETBOX_URL}"
log "Using Bonsai-facing NetBox URL: ${NETBOX_URL_BONSAI}"

if ! curl -sf "${BONSAI_HTTP}/api/topology" >/dev/null 2>&1; then
    RESULT="FAIL"
    set_summary "bonsai_unreachable at ${BONSAI_HTTP}"
    echo "error: bonsai not reachable at ${BONSAI_HTTP}" >&2
    exit 1
fi

log "Waiting for NetBox to be ready at ${NETBOX_URL} (up to 90s)..."
if ! ensure_netbox; then
    RESULT="FAIL"
    set_summary "netbox_unreachable at ${NETBOX_URL} after compose start attempt"
    echo "error: NetBox not reachable at ${NETBOX_URL}" >&2
    exit 1
fi

log "Preflight checks passed"
[[ "${DRY_RUN}" == "true" ]] && { log "Dry-run mode — exiting"; exit 0; }

log "Step 1: Seeding NetBox with standard bonsai topology..."
cd "${REPO_ROOT}"
python3 scripts/seed_netbox.py --url "${NETBOX_URL}" --token "${NETBOX_TOKEN}" >>"$LOG_FILE" 2>&1
pass "NetBox seeded from lab/seed/topology.yaml"

log "Step 2: Adding temporary NetBox credential alias..."
CRED_RESULT="$(curl -sf -X POST "${BONSAI_HTTP}/api/credentials" \
    -H "Content-Type: application/json" \
    -d "{\"alias\":\"${CREDENTIAL_ALIAS}\",\"username\":\"token\",\"password\":\"${NETBOX_TOKEN}\"}" \
    2>>"$LOG_FILE" || echo '{"success":false,"error":"credential request failed"}')"
if json_success "${CRED_RESULT}"; then
    pass "Credential alias ${CREDENTIAL_ALIAS} stored"
else
    fail "Credential alias creation failed: ${CRED_RESULT}"
    set_summary "credential_alias_creation_failed"
fi

log "Step 3: Registering NetBox enricher config..."
ENRICH_RESULT="$(curl -sf -X POST "${BONSAI_HTTP}/api/enrichment" \
    -H "Content-Type: application/json" \
    -d "$(python3 - <<'PY' "${ENRICHER_NAME}" "${NETBOX_URL_BONSAI}" "${CREDENTIAL_ALIAS}"
import json, sys
name, base_url, credential_alias = sys.argv[1:]
print(json.dumps({
    "config": {
        "name": name,
        "enricher_type": "netbox",
        "enabled": True,
        "base_url": base_url,
        "credential_alias": credential_alias,
        "poll_interval_secs": 0,
        "environment_scope": [],
        "extra": {
            "transport": "rest",
            "max_concurrent_requests": 2,
        },
    }
}))
PY
)" 2>>"$LOG_FILE" || echo '{"success":false,"error":"enrichment upsert failed"}')"
if json_success "${ENRICH_RESULT}"; then
    pass "Enricher config ${ENRICHER_NAME} added"
else
    fail "Enricher config add failed: ${ENRICH_RESULT}"
    set_summary "enricher_config_add_failed"
fi

log "Step 4: Testing enricher connection..."
TEST_RESULT="$(curl -sf -X POST "${BONSAI_HTTP}/api/enrichment/test" \
    -H "Content-Type: application/json" \
    -d "{\"name\":\"${ENRICHER_NAME}\"}" 2>>"$LOG_FILE" || echo '{"success":false,"message":"test request failed"}')"
if json_success "${TEST_RESULT}"; then
    pass "Enricher connection test passed"
else
    fail "Enricher connection test failed: ${TEST_RESULT}"
    set_summary "enricher_connection_test_failed"
fi

log "Step 5: Triggering manual enrichment run..."
RUN_RESULT="$(curl -sf -X POST "${BONSAI_HTTP}/api/enrichment/run" \
    -H "Content-Type: application/json" \
    -d "{\"name\":\"${ENRICHER_NAME}\"}" 2>>"$LOG_FILE" || echo '{"success":false,"message":"run request failed"}')"
if json_success "${RUN_RESULT}"; then
    pass "Manual enrichment run started"
else
    fail "Enrichment run request failed: ${RUN_RESULT}"
    set_summary "enrichment_run_request_failed"
fi

log "Step 6: Waiting for completion (up to 60s)..."
MAX_WAIT=60
ELAPSED=0
while true; do
    LIST_JSON="$(curl -sf "${BONSAI_HTTP}/api/enrichment" 2>>"$LOG_FILE" || echo '{"enrichers":[]}')"
    ENTRY_JSON="$(enricher_entry_json "${LIST_JSON}" "${ENRICHER_NAME}")"
    IS_RUNNING="$(python3 - <<'PY' "${ENTRY_JSON}"
import json, sys
entry = json.loads(sys.argv[1])
print("true" if entry.get("state", {}).get("is_running", False) else "false")
PY
)"
    if [[ "${IS_RUNNING}" == "false" && "${ELAPSED}" -gt 0 ]]; then
        NODES_TOUCHED="$(python3 - <<'PY' "${ENTRY_JSON}"
import json, sys
entry = json.loads(sys.argv[1])
print(entry.get("state", {}).get("last_run_nodes_touched") or 0)
PY
)"
        WARNINGS_COUNT="$(python3 - <<'PY' "${ENTRY_JSON}"
import json, sys
entry = json.loads(sys.argv[1])
print(len(entry.get("state", {}).get("last_run_warnings") or []))
PY
)"
        LAST_ERROR="$(python3 - <<'PY' "${ENTRY_JSON}"
import json, sys
entry = json.loads(sys.argv[1])
print(entry.get("state", {}).get("last_run_error") or "")
PY
)"
        if [[ -n "${LAST_ERROR}" ]]; then
            fail "Enrichment completed with error: ${LAST_ERROR}"
            set_summary "last_run_error=${LAST_ERROR}"
        elif [[ "${NODES_TOUCHED}" -gt 0 ]]; then
            pass "Enrichment complete: nodes_touched=${NODES_TOUCHED} warnings=${WARNINGS_COUNT}"
            set_summary "nodes_touched=${NODES_TOUCHED} warnings=${WARNINGS_COUNT}"
        else
            fail "Enrichment completed but nodes_touched=${NODES_TOUCHED}"
            set_summary "nodes_touched=${NODES_TOUCHED}"
        fi
        break
    fi
    sleep 5
    ELAPSED=$((ELAPSED + 5))
    [[ ${ELAPSED} -ge ${MAX_WAIT} ]] && { fail "Enrichment did not complete within ${MAX_WAIT}s"; set_summary "enrichment_timeout=${MAX_WAIT}s"; break; }
done
write_result
log "=== ${RESULT} ==="
[[ "${RESULT}" == "FAIL" ]] && exit 1 || exit 0
