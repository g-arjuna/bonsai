#!/usr/bin/env bash
# Sprint 5 preflight: output adapter stack + ServiceNow PDI readiness.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${REPO_ROOT}/.env"
COMPOSE_FILE="${REPO_ROOT}/docker/compose-external.yml"
START_STACK=false
RUN_CHECKS=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --start) START_STACK=true ;;
        --check) RUN_CHECKS=true ;;
        *) echo "Unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

PASS=0
FAIL=0
WARN=0

ok()   { echo "  + $*"; ((PASS++)) || true; }
fail() { echo "  - $*" >&2; ((FAIL++)) || true; }
warn() { echo "  ! $*"; ((WARN++)) || true; }
section() { echo ""; echo "-- $* --"; }

echo "Bonsai Sprint 5 preflight"
echo "Repo root: $REPO_ROOT"

section "Required tools"
command -v docker >/dev/null 2>&1 && ok "docker found" || fail "docker not found"
docker compose version >/dev/null 2>&1 && ok "docker compose found" || fail "docker compose not found"
command -v curl >/dev/null 2>&1 && ok "curl found" || fail "curl not found"
command -v jq >/dev/null 2>&1 && ok "jq found" || warn "jq not found"
command -v python3 >/dev/null 2>&1 && ok "python3 found" || fail "python3 not found"

section ".env"
if [[ -f "$ENV_FILE" ]]; then
    ok ".env exists"
    # shellcheck disable=SC1090
    set -a
    source "$ENV_FILE" 2>/dev/null || true
    set +a
else
    fail ".env missing; copy .env.example first"
fi

[[ -n "${BONSAI_VAULT_PASSPHRASE:-}" ]] && ok "BONSAI_VAULT_PASSPHRASE set" || fail "BONSAI_VAULT_PASSPHRASE missing"
[[ -n "${SPLUNK_PASSWORD:-}" ]] && ok "SPLUNK_PASSWORD set" || fail "SPLUNK_PASSWORD missing"
[[ -n "${SPLUNK_HEC_TOKEN:-}" ]] && ok "SPLUNK_HEC_TOKEN set" || fail "SPLUNK_HEC_TOKEN missing"
[[ -n "${SNOW_INSTANCE_URL:-}" ]] && ok "SNOW_INSTANCE_URL set" || warn "SNOW_INSTANCE_URL missing"
[[ -n "${SNOW_USERNAME:-}" ]] && ok "SNOW_USERNAME set" || warn "SNOW_USERNAME missing"
[[ -n "${SNOW_PASSWORD:-}" ]] && ok "SNOW_PASSWORD set" || warn "SNOW_PASSWORD missing"

section "Compose assets"
[[ -f "$COMPOSE_FILE" ]] && ok "docker/compose-external.yml present" || fail "compose-external.yml missing"
[[ -f "${REPO_ROOT}/scripts/check_external.sh" ]] && ok "check_external.sh present" || fail "check_external.sh missing"
[[ -f "${REPO_ROOT}/scripts/e2e_output_adapters_test.sh" ]] && ok "output adapter e2e script present" || fail "e2e_output_adapters_test.sh missing"
[[ -f "${REPO_ROOT}/scripts/e2e_servicenow_pdi_test.sh" ]] && ok "ServiceNow PDI e2e script present" || fail "e2e_servicenow_pdi_test.sh missing"

section "Live services"
if curl -skf "https://localhost:8088/services/collector/health" >/dev/null 2>&1; then
    ok "Splunk HEC reachable"
else
    warn "Splunk HEC not reachable"
fi
if curl -sf "http://localhost:8000/api/" -H "Authorization: Token ${NETBOX_API_TOKEN:-bonsai-dev-token}" >/dev/null 2>&1; then
    ok "NetBox reachable"
else
    warn "NetBox not reachable"
fi
if curl -sf "http://localhost:9200/_cluster/health" >/dev/null 2>&1; then
    ok "Elasticsearch reachable"
else
    warn "Elasticsearch not reachable"
fi
if curl -sf "http://localhost:9093/-/ready" >/dev/null 2>&1; then
    ok "Prometheus reachable"
else
    warn "Prometheus not reachable"
fi
if [[ -n "${SNOW_INSTANCE_URL:-}" ]] && curl -sf "${SNOW_INSTANCE_URL%/}/api/now/table/sys_user?sysparm_limit=1" \
    -u "${SNOW_USERNAME:-}:${SNOW_PASSWORD:-}" >/dev/null 2>&1; then
    ok "ServiceNow PDI reachable"
elif [[ -n "${SNOW_INSTANCE_URL:-}" ]]; then
    warn "ServiceNow PDI configured but not reachable"
fi

if $START_STACK; then
    section "Starting Sprint 5 local stack"
    docker compose -f "$COMPOSE_FILE" --profile netbox --profile splunk --profile elastic --profile prometheus up -d
fi

if $RUN_CHECKS; then
    section "External check"
    (
        cd "$REPO_ROOT"
        ./scripts/check_external.sh
    )
fi

echo ""
echo "Summary: ${PASS} pass  ${WARN} warn  ${FAIL} fail"
echo ""
echo "Recommended Sprint 5 flow:"
echo "  1. scripts/sprint5_preflight.sh --start --check"
echo "  2. cargo build --release"
echo "  3. source .env && scripts/e2e_servicenow_pdi_test.sh"
echo "  4. scripts/e2e_output_adapters_test.sh --adapter all"
echo "  5. curl http://localhost:3000/api/adapters | jq ."

exit $([[ $FAIL -gt 0 ]] && echo 1 || echo 0)
