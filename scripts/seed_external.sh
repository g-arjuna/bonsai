#!/usr/bin/env bash
# scripts/seed_external.sh — Seed all external bonsai services from lab/seed/topology.yaml.
#
# Runs in order: NetBox → Splunk → Elasticsearch. ServiceNow PDI is operator-triggered
# separately (requires SNOW_* env vars for a personal developer instance).
#
# Prerequisites:
#   - External services running: docker compose -f docker/compose-external.yml --profile all up -d
#   - .env sourced or exported (SPLUNK_PASSWORD, SPLUNK_HEC_TOKEN)
#   - Python venv active or dependencies installed: requests, pyyaml
#
# Usage:
#   source .env && scripts/seed_external.sh [--skip-netbox] [--skip-splunk] [--skip-elastic]

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT_DIR="${REPO_ROOT}/scripts"
LOG_FILE="/tmp/bonsai-seed-external-$(date +%Y%m%d-%H%M%S).log"

SKIP_NETBOX=false
SKIP_SPLUNK=false
SKIP_ELASTIC=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-netbox)  SKIP_NETBOX=true ;;
        --skip-splunk)  SKIP_SPLUNK=true ;;
        --skip-elastic) SKIP_ELASTIC=true ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
    shift
done

log() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG_FILE"; }
ok()  { log "  [OK] $*"; }
err() { log "  [FAIL] $*" >&2; }

log "=== Bonsai external service seed ==="
log "Log: $LOG_FILE"

PYTHON="${REPO_ROOT}/.venv/bin/python"
[[ -x "$PYTHON" ]] || PYTHON="python3"

RESULTS=()

# ── NetBox ────────────────────────────────────────────────────────────────────

if [[ "$SKIP_NETBOX" == "false" ]]; then
    log "--- NetBox ---"
    NETBOX_URL="${NETBOX_URL:-http://localhost:8000}"
    NETBOX_TOKEN="${NETBOX_API_TOKEN:-bonsai-dev-token}"

    if ! curl -sf "${NETBOX_URL}/api/" -H "Authorization: Token ${NETBOX_TOKEN}" >/dev/null 2>&1; then
        err "NetBox not reachable at ${NETBOX_URL} — is the container running?"
        RESULTS+=("netbox: SKIP (not reachable)")
    else
        if "$PYTHON" "${SCRIPT_DIR}/seed_netbox.py" \
                --url "$NETBOX_URL" \
                --token "$NETBOX_TOKEN" \
                2>&1 | tee -a "$LOG_FILE"; then
            ok "NetBox seeded"
            RESULTS+=("netbox: OK")
        else
            err "NetBox seed failed"
            RESULTS+=("netbox: FAIL")
        fi
    fi
else
    RESULTS+=("netbox: SKIP (--skip-netbox)")
fi

# ── Splunk ────────────────────────────────────────────────────────────────────

if [[ "$SKIP_SPLUNK" == "false" ]]; then
    log "--- Splunk ---"

    if [[ -z "${SPLUNK_PASSWORD:-}" ]]; then
        err "SPLUNK_PASSWORD not set — source .env or export it"
        RESULTS+=("splunk: SKIP (SPLUNK_PASSWORD not set)")
    elif [[ -z "${SPLUNK_HEC_TOKEN:-}" ]]; then
        err "SPLUNK_HEC_TOKEN not set — source .env or export it"
        RESULTS+=("splunk: SKIP (SPLUNK_HEC_TOKEN not set)")
    elif ! curl -sf "http://localhost:8088/services/collector/health" >/dev/null 2>&1; then
        err "Splunk HEC not reachable at localhost:8088 — is the container running?"
        RESULTS+=("splunk: SKIP (not reachable)")
    else
        if "$PYTHON" "${SCRIPT_DIR}/seed_splunk.py" \
                --url "http://localhost:8100" \
                --hec-url "http://localhost:8088" \
                --password "$SPLUNK_PASSWORD" \
                --hec-token "$SPLUNK_HEC_TOKEN" \
                2>&1 | tee -a "$LOG_FILE"; then
            ok "Splunk seeded"
            RESULTS+=("splunk: OK")
        else
            err "Splunk seed failed"
            RESULTS+=("splunk: FAIL")
        fi
    fi
else
    RESULTS+=("splunk: SKIP (--skip-splunk)")
fi

# ── Elasticsearch ─────────────────────────────────────────────────────────────

if [[ "$SKIP_ELASTIC" == "false" ]]; then
    log "--- Elasticsearch ---"

    if ! curl -sf "http://localhost:9200/_cluster/health" >/dev/null 2>&1; then
        err "Elasticsearch not reachable at localhost:9200 — is the container running?"
        RESULTS+=("elastic: SKIP (not reachable)")
    else
        if "$PYTHON" "${SCRIPT_DIR}/seed_elastic.py" \
                --url "http://localhost:9200" \
                2>&1 | tee -a "$LOG_FILE"; then
            ok "Elasticsearch seeded"
            RESULTS+=("elastic: OK")
        else
            err "Elasticsearch seed failed"
            RESULTS+=("elastic: FAIL")
        fi
    fi
else
    RESULTS+=("elastic: SKIP (--skip-elastic)")
fi

# ── ServiceNow PDI (operator-triggered) ───────────────────────────────────────

if [[ -n "${SNOW_INSTANCE_URL:-}" && -n "${SNOW_USERNAME:-}" && -n "${SNOW_PASSWORD:-}" ]]; then
    log "--- ServiceNow PDI ---"
    if "$PYTHON" "${SCRIPT_DIR}/seed_servicenow_pdi.py" 2>&1 | tee -a "$LOG_FILE"; then
        ok "ServiceNow PDI seeded"
        RESULTS+=("servicenow: OK")
    else
        err "ServiceNow PDI seed failed"
        RESULTS+=("servicenow: FAIL")
    fi
else
    RESULTS+=("servicenow: SKIP (SNOW_* env vars not set — run manually when PDI is available)")
fi

# ── Summary ───────────────────────────────────────────────────────────────────

log ""
log "=== Seed summary ==="
for r in "${RESULTS[@]}"; do
    log "  $r"
done

FAILED=$(printf '%s\n' "${RESULTS[@]}" | grep -c ': FAIL' || true)
if [[ "$FAILED" -gt 0 ]]; then
    log "FAIL: $FAILED service(s) failed to seed"
    exit 1
fi
log "All seeds complete — log: $LOG_FILE"
