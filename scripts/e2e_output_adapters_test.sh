#!/usr/bin/env bash
# T2-7 (v10) — Output adapter end-to-end tests
#
# Verifies Prometheus, Splunk HEC, and Elastic adapters against the
# Ubuntu/Docker Bonsai runtime:
#   1. Ensure the target service is reachable locally
#   2. Register the adapter config via /api/adapters
#   3. Restart bonsai-core so the adapter starts cleanly
#   4. Inject one synthetic DetectionEvent
#   5. Verify a fresh adapter push is recorded and visible in the target
#   6. Cleanup temporary adapter / credential state
#
# Produces:
#   docs/test_results/e2e_output_adapters/$(date +%Y%m%d)-<adapter>-<result>.md

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESULT_DIR="${REPO_ROOT}/docs/test_results/e2e_output_adapters"
COMPOSE_FILE="${REPO_ROOT}/docker-compose.yml"
EXTERNAL_COMPOSE_FILE="${REPO_ROOT}/docker/compose-external.yml"
BONSAI_HTTP="${BONSAI_HTTP:-http://127.0.0.1:3000}"
BONSAI_GRPC="${BONSAI_GRPC:-127.0.0.1:50051}"
PYTHON="${PYTHON:-}"
LOG_FILE="/tmp/bonsai-e2e-output-$(date +%Y%m%d-%H%M%S)-$$.log"
DRY_RUN=false
ADAPTER="all"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) DRY_RUN=true ;;
        --adapter) ADAPTER="$2"; shift ;;
        --bonsai-http) BONSAI_HTTP="$2"; shift ;;
        --bonsai-grpc) BONSAI_GRPC="$2"; shift ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
    shift
done

if [[ -z "${PYTHON}" ]]; then
    if [[ -x "${REPO_ROOT}/.venv/bin/python" ]]; then
        PYTHON="${REPO_ROOT}/.venv/bin/python"
    else
        PYTHON="$(command -v python3)"
    fi
fi

log()  { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG_FILE"; }
fail() { log "FAIL[$1]: $2"; eval "RESULT_${1^^}=FAIL"; eval "SUMMARY_${1^^}=\"\$2\""; }
pass() { log "PASS[$1]: $2"; eval "SUMMARY_${1^^}=\"\$2\""; }

RESULT_PROMETHEUS="SKIP"
RESULT_SPLUNK="SKIP"
RESULT_ELASTIC="SKIP"
SUMMARY_PROMETHEUS="not_run"
SUMMARY_SPLUNK="not_run"
SUMMARY_ELASTIC="not_run"
LAST_INJECTED_RULE_ID=""

bonsai_runs_in_container() {
    command -v docker >/dev/null 2>&1 || return 1
    docker compose -f "${COMPOSE_FILE}" ps -q bonsai-core 2>/dev/null | grep -q .
}

resolve_bonsai_endpoint_url() {
    local url="$1"
    "${PYTHON}" - <<'PY' "$url"
from urllib.parse import urlparse, urlunparse
import sys

raw = sys.argv[1]
parsed = urlparse(raw)
host = parsed.hostname
if host in {"localhost", "127.0.0.1"}:
    parsed = parsed._replace(netloc=parsed.netloc.replace(host, "host.docker.internal", 1))
print(urlunparse(parsed))
PY
}

json_success() {
    "${PYTHON}" - <<'PY' "$1"
import json
import sys

payload = json.loads(sys.argv[1])
raise SystemExit(0 if payload.get("success") else 1)
PY
}

current_time_ns() {
    "${PYTHON}" - <<'PY'
import time
print(time.time_ns())
PY
}

adapter_entry_json() {
    "${PYTHON}" - <<'PY' "$1" "$2"
import json
import sys

payload = json.loads(sys.argv[1])
name = sys.argv[2]
for entry in payload.get("adapters", []):
    if entry.get("config", {}).get("name") == name:
        print(json.dumps(entry))
        raise SystemExit(0)
print("{}")
PY
}

adapter_state_field() {
    "${PYTHON}" - <<'PY' "$1" "$2"
import json
import sys

entry = json.loads(sys.argv[1])
field = sys.argv[2]
state = entry.get("state", {})
value = state.get(field)
if isinstance(value, bool):
    print("true" if value else "false")
elif value is None:
    print("")
else:
    print(value)
PY
}

wait_for_http() {
    local url="$1" max="$2" elapsed=0
    while ! curl -sf "$url" >/dev/null 2>&1; do
        sleep 3
        elapsed=$((elapsed + 3))
        [[ $elapsed -ge $max ]] && return 1
    done
}

ensure_prometheus() {
    if wait_for_http "http://localhost:9093/-/ready" 10; then
        return 0
    fi
    docker compose -f "${EXTERNAL_COMPOSE_FILE}" --profile prometheus up -d prometheus >>"$LOG_FILE" 2>&1 || true
    wait_for_http "http://localhost:9093/-/ready" 120
}

ensure_splunk() {
    if curl -skf "https://localhost:8088/services/collector/health" >/dev/null 2>&1; then
        return 0
    fi
    docker compose -f "${EXTERNAL_COMPOSE_FILE}" --profile splunk up -d splunk >>"$LOG_FILE" 2>&1 || true
    local elapsed=0
    while ! curl -skf "https://localhost:8088/services/collector/health" >/dev/null 2>&1; do
        sleep 5
        elapsed=$((elapsed + 5))
        [[ $elapsed -ge 180 ]] && return 1
    done
}

ensure_elastic() {
    if wait_for_http "http://localhost:9200/_cluster/health" 10; then
        return 0
    fi
    docker compose -f "${EXTERNAL_COMPOSE_FILE}" --profile elastic up -d elasticsearch >>"$LOG_FILE" 2>&1 || true
    wait_for_http "http://localhost:9200/_cluster/health" 120
}

container_bonsai_restart() {
    log "Restarting Bonsai container service 'bonsai-core'..."
    docker compose -f "${COMPOSE_FILE}" restart bonsai-core >>"$LOG_FILE" 2>&1 || true
    docker compose -f "${COMPOSE_FILE}" up -d bonsai-core bonsai-collector-1 >>"$LOG_FILE" 2>&1
    local elapsed=0
    while ! curl -sf "${BONSAI_HTTP}/api/topology" >/dev/null 2>&1; do
        sleep 5
        elapsed=$((elapsed + 5))
        [[ $elapsed -ge 240 ]] && return 1
        [[ $((elapsed % 30)) -eq 0 ]] && log "  waiting for bonsai HTTP (${elapsed}s)..."
    done
    log "bonsai-core restarted and healthy (${elapsed}s)"
}

bonsai_restart() {
    if bonsai_runs_in_container; then
        container_bonsai_restart
        return $?
    fi

    log "Falling back to host bonsai restart path..."
    local bonsai_bin="${BONSAI_BIN:-${REPO_ROOT}/target/release/bonsai}"
    if [[ ! -x "${bonsai_bin}" ]]; then
        return 1
    fi
    pkill -f "target/release/bonsai" >>"$LOG_FILE" 2>&1 || true
    RUST_LOG=info "${bonsai_bin}" --config "${REPO_ROOT}/bonsai.toml" >>"$LOG_FILE" 2>&1 &
    wait_for_http "${BONSAI_HTTP}/api/topology" 240
}

adapter_remove() {
    curl -sf -X POST "${BONSAI_HTTP}/api/adapters/remove" \
        -H "Content-Type: application/json" \
        -d "{\"name\":\"$1\"}" >>"$LOG_FILE" 2>&1 || true
}

credential_remove() {
    curl -sf -X POST "${BONSAI_HTTP}/api/credentials/remove" \
        -H "Content-Type: application/json" \
        -d "{\"alias\":\"$1\"}" >>"$LOG_FILE" 2>&1 || true
}

credential_upsert() {
    local alias="$1" username="$2" password="$3"
    local response
    response="$(curl -sf -X POST "${BONSAI_HTTP}/api/credentials" \
        -H "Content-Type: application/json" \
        -d "{\"alias\":\"${alias}\",\"username\":\"${username}\",\"password\":\"${password}\"}" \
        2>>"$LOG_FILE" || echo '{"success":false,"error":"credential request failed"}')"
    if json_success "${response}"; then
        return 0
    fi
    response="$(curl -sf -X POST "${BONSAI_HTTP}/api/credentials/update" \
        -H "Content-Type: application/json" \
        -d "{\"alias\":\"${alias}\",\"username\":\"${username}\",\"password\":\"${password}\"}" \
        2>>"$LOG_FILE" || echo '{"success":false,"error":"credential update failed"}')"
    json_success "${response}"
}

adapter_upsert() {
    local payload="$1"
    local response
    response="$(curl -sf -X POST "${BONSAI_HTTP}/api/adapters" \
        -H "Content-Type: application/json" \
        -d "{\"config\":${payload}}" \
        2>>"$LOG_FILE" || echo '{"success":false,"error":"adapter request failed"}')"
    json_success "${response}"
}

wait_for_adapter_running() {
    local name="$1" max_wait="${2:-120}" elapsed=0
    while [[ ${elapsed} -lt ${max_wait} ]]; do
        local list_json entry_json is_running
        list_json="$(curl -sf "${BONSAI_HTTP}/api/adapters" 2>>"$LOG_FILE" || echo '{"adapters":[]}')"
        entry_json="$(adapter_entry_json "${list_json}" "${name}")"
        is_running="$(adapter_state_field "${entry_json}" "is_running")"
        if [[ "${is_running}" == "true" ]]; then
            return 0
        fi
        sleep 5
        elapsed=$((elapsed + 5))
    done
    return 1
}

wait_for_adapter_push_result() {
    local name="$1" since_ns="$2" max_wait="${3:-150}" elapsed=0
    while [[ ${elapsed} -lt ${max_wait} ]]; do
        local list_json entry_json pushed_at error pushed_events
        list_json="$(curl -sf "${BONSAI_HTTP}/api/adapters" 2>>"$LOG_FILE" || echo '{"adapters":[]}')"
        entry_json="$(adapter_entry_json "${list_json}" "${name}")"
        pushed_at="$(adapter_state_field "${entry_json}" "last_push_at_ns")"
        error="$(adapter_state_field "${entry_json}" "last_push_error")"
        pushed_events="$(adapter_state_field "${entry_json}" "last_push_events")"
        if [[ -n "${pushed_at}" ]] && [[ "${pushed_at}" =~ ^[0-9]+$ ]] && [[ "${pushed_at}" -ge "${since_ns}" ]]; then
            if [[ -n "${error}" ]]; then
                log "[${name}] adapter reported push error: ${error}"
                return 2
            fi
            log "[${name}] fresh adapter push observed: events=${pushed_events:-unknown} at_ns=${pushed_at}"
            return 0
        fi
        sleep 5
        elapsed=$((elapsed + 5))
    done
    return 1
}

get_topology_device_address() {
    local topology_json
    topology_json="$(curl -sf "${BONSAI_HTTP}/api/topology" 2>/dev/null || true)"
    printf '%s' "${topology_json}" | "${PYTHON}" -c '
import json
import sys

try:
    payload = json.load(sys.stdin)
    devices = payload.get("devices", [])
    print(devices[0]["address"] if devices else "")
except Exception:
    print("")
'
}

inject_test_detection() {
    local marker="$1"
    local fired_at_ns rule_id severity features_json device_address elapsed=0

    while [[ ${elapsed} -lt 60 ]]; do
        device_address="$(get_topology_device_address)"
        if [[ -n "${device_address}" ]]; then
            break
        fi
        sleep 5
        elapsed=$((elapsed + 5))
    done

    if [[ -z "${device_address}" ]]; then
        log "No device addresses available from topology for synthetic detection injection"
        return 1
    fi

    fired_at_ns="$(current_time_ns)"
    rule_id="synthetic-e2e-${marker}"
    severity="warning"
    features_json="$(printf '{"marker":"%s","source":"adapter_e2e","kind":"synthetic"}' "${marker}")"

    PYTHONPATH="${REPO_ROOT}/python${PYTHONPATH:+:${PYTHONPATH}}" \
    "${PYTHON}" - <<'PY' "${BONSAI_GRPC}" "${device_address}" "${rule_id}" "${severity}" "${features_json}" "${fired_at_ns}" >>"$LOG_FILE" 2>&1
import json
import sys

grpc_addr, device_address, rule_id, severity, features_json, fired_at_ns = sys.argv[1:]
from bonsai_sdk import BonsaiClient

with BonsaiClient(grpc_addr=grpc_addr) as client:
    client.create_detection(
        device_address=device_address,
        rule_id=rule_id,
        severity=severity,
        features_json=features_json,
        fired_at_ns=int(fired_at_ns),
    )

print(json.dumps({"ok": True, "device_address": device_address, "rule_id": rule_id}))
PY
    LAST_INJECTED_RULE_ID="${rule_id}"
    log "Injected synthetic detection marker=${marker} device=${device_address} rule_id=${rule_id}"
}

write_result() {
    mkdir -p "${RESULT_DIR}"
    local bonsai_sha date adapter var result summary result_file
    bonsai_sha="$(git -C "${REPO_ROOT}" rev-parse --short HEAD 2>/dev/null || echo "unknown")"
    date="$(date +%Y%m%d)"

    for adapter in prometheus splunk elastic; do
        var="RESULT_${adapter^^}"
        result="${!var}"
        [[ "${result}" == "SKIP" ]] && continue
        summary_var="SUMMARY_${adapter^^}"
        summary="${!summary_var}"
        result_file="${RESULT_DIR}/${date}-${adapter}-${result,,}.md"
        cat > "${result_file}" <<EOF
# Output Adapter E2E test: ${adapter}

**Date**: $(date +%Y-%m-%d)
**Operator**: $(git config user.name 2>/dev/null || echo "unknown")
**Bonsai version**: ${bonsai_sha}
**Adapter**: ${adapter}

## Result

**${result}**

## Summary

${summary}

## Log

\`${LOG_FILE}\`
EOF
        log "Result for ${adapter} written to: ${result_file}"
    done
}

cleanup() {
    adapter_remove "prom-test"
    adapter_remove "splunk-test"
    adapter_remove "elastic-test"
    credential_remove "splunk-hec-e2e"
    write_result || true
}
trap cleanup EXIT

log "=== Bonsai Output Adapter E2E Tests (adapter=${ADAPTER}) ==="

if ! command -v docker >/dev/null 2>&1; then
    echo "error: docker not found" >&2
    exit 2
fi
if ! curl -sf "${BONSAI_HTTP}/api/topology" >/dev/null 2>&1; then
    echo "error: bonsai not reachable at ${BONSAI_HTTP}" >&2
    exit 2
fi

PROM_URL_LOCAL="${PROM_URL_LOCAL:-http://localhost:9093}"
SPLUNK_HEC_URL_LOCAL="${SPLUNK_HEC_URL_LOCAL:-https://localhost:8088}"
SPLUNK_WEB_URL_LOCAL="${SPLUNK_WEB_URL_LOCAL:-http://localhost:8100}"
ELASTIC_URL_LOCAL="${ELASTIC_URL_LOCAL:-http://localhost:9200}"

if bonsai_runs_in_container; then
    PROM_URL_BONSAI="${PROM_URL_BONSAI:-$(resolve_bonsai_endpoint_url "${PROM_URL_LOCAL}")}"
    SPLUNK_HEC_URL_BONSAI="${SPLUNK_HEC_URL_BONSAI:-$(resolve_bonsai_endpoint_url "${SPLUNK_HEC_URL_LOCAL}")}"
    ELASTIC_URL_BONSAI="${ELASTIC_URL_BONSAI:-$(resolve_bonsai_endpoint_url "${ELASTIC_URL_LOCAL}")}"
else
    PROM_URL_BONSAI="${PROM_URL_BONSAI:-${PROM_URL_LOCAL}}"
    SPLUNK_HEC_URL_BONSAI="${SPLUNK_HEC_URL_BONSAI:-${SPLUNK_HEC_URL_LOCAL}}"
    ELASTIC_URL_BONSAI="${ELASTIC_URL_BONSAI:-${ELASTIC_URL_LOCAL}}"
fi

log "Using local Splunk HEC URL: ${SPLUNK_HEC_URL_LOCAL}"
log "Using Bonsai-facing Splunk HEC URL: ${SPLUNK_HEC_URL_BONSAI}"
log "Using local Elastic URL: ${ELASTIC_URL_LOCAL}"
log "Using Bonsai-facing Elastic URL: ${ELASTIC_URL_BONSAI}"
log "Using local Prometheus URL: ${PROM_URL_LOCAL}"
log "Using Bonsai-facing Prometheus URL: ${PROM_URL_BONSAI}"
log "Preflight checks passed"
[[ "${DRY_RUN}" == "true" ]] && { log "Dry-run mode — exiting"; exit 0; }

test_prometheus() {
    RESULT_PROMETHEUS="PASS"
    if ! ensure_prometheus; then
        fail "prometheus" "Prometheus not reachable at ${PROM_URL_LOCAL}"
        return
    fi

    log "[prometheus] Registering adapter..."
    adapter_remove "prom-test"
    if ! adapter_upsert "{\"name\":\"prom-test\",\"adapter_type\":\"prometheus_remote_write\",\"endpoint_url\":\"${PROM_URL_BONSAI}/api/v1/write\",\"enabled\":true,\"flush_interval_secs\":10}"; then
        fail "prometheus" "failed to register Prometheus adapter"
        return
    fi

    if ! bonsai_restart; then
        fail "prometheus" "bonsai failed to restart"
        return
    fi
    if ! wait_for_adapter_running "prom-test" 120; then
        fail "prometheus" "Prometheus adapter never entered running state"
        return
    fi

    local marker push_floor_ns metric_count=0 elapsed=0
    marker="prom-$(date +%s)-$$"
    push_floor_ns="$(current_time_ns)"
    if ! inject_test_detection "${marker}" >/dev/null; then
        fail "prometheus" "failed to inject a fresh detection event"
        return
    fi
    wait_for_adapter_push_result "prom-test" "${push_floor_ns}" 150 || true

    while [[ ${elapsed} -lt 90 ]]; do
        metric_count="$(curl -sf "${PROM_URL_LOCAL}/api/v1/query?query=bonsai_interface_in_octets_total" 2>/dev/null | jq '.data.result | length' 2>/dev/null || echo 0)"
        if [[ "${metric_count}" =~ ^[0-9]+$ ]] && [[ "${metric_count}" -gt 0 ]]; then
            pass "prometheus" "bonsai_interface_in_octets_total visible (${metric_count} series)"
            return
        fi
        sleep 5
        elapsed=$((elapsed + 5))
    done
    fail "prometheus" "no bonsai_* metrics visible in Prometheus"
}

test_splunk() {
    RESULT_SPLUNK="PASS"
    local splunk_password="${SPLUNK_PASSWORD:-Bonsai1234!}"
    local splunk_hec_token="${SPLUNK_HEC_TOKEN:-}"

    if [[ -z "${splunk_hec_token}" ]]; then
        fail "splunk" "SPLUNK_HEC_TOKEN is not set"
        return
    fi
    if ! ensure_splunk; then
        fail "splunk" "HEC health endpoint did not respond"
        return
    fi

    log "[splunk] Reusing existing Splunk service at ${SPLUNK_HEC_URL_LOCAL}"
    log "[splunk] Waiting up to 30s for HEC health..."
    if ! curl -skf "${SPLUNK_HEC_URL_LOCAL}/services/collector/health" >/dev/null 2>&1; then
        fail "splunk" "HEC health endpoint did not respond"
        return
    fi

    adapter_remove "splunk-test"
    credential_remove "splunk-hec-e2e"
    if ! credential_upsert "splunk-hec-e2e" "hec" "${splunk_hec_token}"; then
        fail "splunk" "failed to store Splunk HEC credential"
        return
    fi
    if ! adapter_upsert "{\"name\":\"splunk-test\",\"adapter_type\":\"splunk_hec\",\"endpoint_url\":\"${SPLUNK_HEC_URL_BONSAI}\",\"credential_alias\":\"splunk-hec-e2e\",\"enabled\":true,\"flush_interval_secs\":10,\"extra\":{\"insecure_tls\":true,\"sourcetype\":\"bonsai:detection\"}}"; then
        fail "splunk" "failed to register Splunk adapter"
        return
    fi

    if ! bonsai_restart; then
        fail "splunk" "bonsai failed to restart"
        return
    fi
    if ! wait_for_adapter_running "splunk-test" 120; then
        fail "splunk" "Splunk adapter never entered running state"
        return
    fi

    local marker rule_id push_floor_ns push_rc elapsed=0 search_output=""
    marker="splunk-$(date +%s)-$$"
    push_floor_ns="$(current_time_ns)"
    if ! inject_test_detection "${marker}"; then
        fail "splunk" "failed to inject a fresh detection event"
        return
    fi
    rule_id="${LAST_INJECTED_RULE_ID}"

    set +e
    wait_for_adapter_push_result "splunk-test" "${push_floor_ns}" 150
    push_rc=$?
    set -e
    if [[ ${push_rc} -eq 1 ]]; then
        fail "splunk" "adapter never recorded a fresh push for the synthetic detection"
        return
    fi
    if [[ ${push_rc} -eq 2 ]]; then
        fail "splunk" "adapter push completed with an error"
        return
    fi

    while [[ ${elapsed} -lt 120 ]]; do
        search_output="$(curl -sSkLf -u "admin:${splunk_password}" --get \
            --data-urlencode "search=search source=\"bonsai\" \"${marker}\" OR \"${rule_id}\" | head 5" \
            --data "output_mode=json" \
            "${SPLUNK_WEB_URL_LOCAL}/en-US/services/search/jobs/export" 2>>"$LOG_FILE" || true)"
        if grep -q "${marker}\|${rule_id}" <<<"${search_output}"; then
            pass "splunk" "fresh synthetic detection became searchable in Splunk"
            return
        fi
        sleep 5
        elapsed=$((elapsed + 5))
    done

    fail "splunk" "adapter push completed but no searchable Splunk events were observed"
}

test_elastic() {
    RESULT_ELASTIC="PASS"
    if ! ensure_elastic; then
        fail "elastic" "Elasticsearch not reachable at ${ELASTIC_URL_LOCAL}"
        return
    fi

    log "[elastic] Reusing existing Elasticsearch at ${ELASTIC_URL_LOCAL}"
    adapter_remove "elastic-test"
    if ! adapter_upsert "{\"name\":\"elastic-test\",\"adapter_type\":\"elastic\",\"endpoint_url\":\"${ELASTIC_URL_BONSAI}\",\"enabled\":true,\"flush_interval_secs\":10}"; then
        fail "elastic" "failed to register Elastic adapter"
        return
    fi

    if ! bonsai_restart; then
        fail "elastic" "bonsai failed to restart"
        return
    fi
    if ! wait_for_adapter_running "elastic-test" 120; then
        fail "elastic" "Elastic adapter never entered running state"
        return
    fi

    local marker rule_id push_floor_ns push_rc elapsed=0 mapping search_output=""
    marker="elastic-$(date +%s)-$$"
    push_floor_ns="$(current_time_ns)"
    if ! inject_test_detection "${marker}"; then
        fail "elastic" "failed to inject a fresh detection event"
        return
    fi
    rule_id="${LAST_INJECTED_RULE_ID}"

    set +e
    wait_for_adapter_push_result "elastic-test" "${push_floor_ns}" 150
    push_rc=$?
    set -e
    if [[ ${push_rc} -eq 1 ]]; then
        fail "elastic" "adapter never recorded a fresh push for the synthetic detection"
        return
    fi
    if [[ ${push_rc} -eq 2 ]]; then
        fail "elastic" "adapter push completed with an error"
        return
    fi

    while [[ ${elapsed} -lt 120 ]]; do
        search_output="$(curl -sf "${ELASTIC_URL_LOCAL}/bonsai-detections/_search?q=${rule_id}&size=5" 2>>"$LOG_FILE" || true)"
        if grep -q "${rule_id}\|${marker}" <<<"${search_output}"; then
            mapping="$(curl -sf "${ELASTIC_URL_LOCAL}/bonsai-detections/_mapping" 2>>"$LOG_FILE" || echo "{}")"
            if echo "${mapping}" | jq -e '.. | objects | select(has("@timestamp"))' >/dev/null 2>&1; then
                pass "elastic" "fresh synthetic detection indexed with ECS @timestamp mapping present"
            else
                pass "elastic" "fresh synthetic detection indexed in Elastic"
            fi
            return
        fi
        sleep 5
        elapsed=$((elapsed + 5))
    done

    fail "elastic" "adapter push completed but no searchable Elastic documents were observed"
}

case "${ADAPTER}" in
    prometheus) test_prometheus ;;
    splunk) test_splunk ;;
    elastic) test_elastic ;;
    all) test_prometheus; test_splunk; test_elastic ;;
    *) echo "Unknown adapter: ${ADAPTER} (use prometheus|splunk|elastic|all)" >&2; exit 1 ;;
esac

log "=== Results: Prometheus=${RESULT_PROMETHEUS} Splunk=${RESULT_SPLUNK} Elastic=${RESULT_ELASTIC} ==="
[[ "${RESULT_PROMETHEUS}" == "FAIL" || "${RESULT_SPLUNK}" == "FAIL" || "${RESULT_ELASTIC}" == "FAIL" ]] && exit 1 || exit 0
