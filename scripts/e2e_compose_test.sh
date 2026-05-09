#!/usr/bin/env bash
# T2-1 (v10) — Docker compose end-to-end test
#
# Prerequisites:
#   - Docker and docker compose v2 installed
#   - scripts/generate_compose_tls.sh available
#   - scripts/seed_lab_creds.sh available
#   - BONSAI_VAULT_PASSPHRASE set in environment (or use --passphrase)
#
# Usage:
#   ./scripts/e2e_compose_test.sh [--dry-run] [--keep-running] [--passphrase <phrase>]
#
# Produces:
#   docs/test_results/e2e_compose/$(date +%Y%m%d)-<result>.md

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESULT_DIR="${REPO_ROOT}/docs/test_results/e2e_compose"
LOG_FILE="/tmp/bonsai-e2e-compose-$(date +%Y%m%d-%H%M%S).log"
DRY_RUN=false
KEEP_RUNNING=false
PASSPHRASE="${BONSAI_VAULT_PASSPHRASE:-}"
HTTP_PORT="${BONSAI_DISTRIBUTED_HTTP_PORT:-3100}"
GRPC_PORT="${BONSAI_DISTRIBUTED_GRPC_PORT:-51051}"
CLAB_NETWORK="${CLAB_NETWORK:-bonsai-dc-mgmt}"
COMPOSE_PROJECT_NAME="${COMPOSE_PROJECT_NAME:-bonsai-distributed}"
COMPOSE_PROFILE="${BONSAI_DISTRIBUTED_PROFILE:-two-collector}"
BONSAI_HTTP="http://localhost:${HTTP_PORT}"

# ── argument parsing ─────────────────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) DRY_RUN=true ;;
        --keep-running) KEEP_RUNNING=true ;;
        --passphrase) PASSPHRASE="$2"; shift ;;
        --http-port) HTTP_PORT="$2"; BONSAI_HTTP="http://localhost:${HTTP_PORT}"; shift ;;
        --grpc-port) GRPC_PORT="$2"; shift ;;
        --clab-network) CLAB_NETWORK="$2"; shift ;;
        --profile) COMPOSE_PROFILE="$2"; shift ;;
        *) echo "Unknown argument: $1"; exit 1 ;;
    esac
    shift
done

# ── helpers ──────────────────────────────────────────────────────────────────

log() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG_FILE"; }
fail() { log "FAIL: $*"; RESULT="FAIL"; }
pass() { log "PASS: $*"; }

env_value_present() {
    local name="$1"
    if [[ -n "${!name:-}" ]]; then
        return 0
    fi
    [[ -f "${REPO_ROOT}/.env" ]] && grep -Eq "^${name}=.+" "${REPO_ROOT}/.env"
}

env_file_value() {
    local name="$1"
    [[ -f "${REPO_ROOT}/.env" ]] || return 0
    awk -F= -v name="$name" '$1 == name { sub(/^[^=]*=/, ""); print; exit }' "${REPO_ROOT}/.env"
}

port_open() {
    local port="$1"
    timeout 1 bash -c ":</dev/tcp/127.0.0.1/${port}" >/dev/null 2>&1
}

wait_for_http() {
    local url="$1" max_secs="$2" interval=2
    local elapsed=0
    while ! curl -sf "$url" >/dev/null 2>&1; do
        sleep "$interval"
        elapsed=$((elapsed + interval))
        if [[ $elapsed -ge $max_secs ]]; then
            return 1
        fi
    done
    return 0
}

RESULT="PASS"

# ── cleanup (runs on EXIT regardless of success/failure) ──────────────────────

cleanup() {
    cd "${REPO_ROOT}"
    if [[ "$KEEP_RUNNING" != "true" ]]; then
        COMPOSE_PROJECT_NAME="$COMPOSE_PROJECT_NAME" \
        docker compose --profile "$COMPOSE_PROFILE" down --remove-orphans --volumes 2>>"$LOG_FILE" || true
    fi
}
trap cleanup EXIT

# ── preflight ─────────────────────────────────────────────────────────────────

log "=== Bonsai Docker Compose E2E Test ==="
log "Repo: ${REPO_ROOT}"
log "Log: ${LOG_FILE}"
log "Compose project: ${COMPOSE_PROJECT_NAME}"
log "Compose profile: ${COMPOSE_PROFILE}"
log "Keep running: ${KEEP_RUNNING}"
log "HTTP: ${BONSAI_HTTP}"
log "gRPC host port: ${GRPC_PORT}"
log "ContainerLab network: ${CLAB_NETWORK}"

if ! command -v docker &>/dev/null; then
    echo "error: docker not found on PATH" >&2; exit 2
fi
if ! docker compose version &>/dev/null; then
    echo "error: docker compose v2 not found" >&2; exit 2
fi
if [[ -z "$PASSPHRASE" ]]; then
    PASSPHRASE="$(env_file_value BONSAI_VAULT_PASSPHRASE)"
fi
if [[ -z "$PASSPHRASE" ]]; then
    echo "error: BONSAI_VAULT_PASSPHRASE not set (use --passphrase or export the env var)" >&2; exit 2
fi
if ! env_value_present BONSAI_SRL_USERNAME || ! env_value_present BONSAI_SRL_PASSWORD; then
    echo "error: BONSAI_SRL_USERNAME/BONSAI_SRL_PASSWORD must be set in .env or the environment for distributed DC validation" >&2
    exit 2
fi
if ! docker network inspect "$CLAB_NETWORK" >/dev/null 2>&1; then
    echo "error: Docker network '$CLAB_NETWORK' not found; deploy the DC lab first or pass --clab-network" >&2
    exit 2
fi
if port_open "$HTTP_PORT"; then
    echo "error: localhost:${HTTP_PORT} is already reachable; choose --http-port or stop the conflicting process" >&2
    exit 2
fi
if port_open "$GRPC_PORT"; then
    echo "error: localhost:${GRPC_PORT} is already reachable; choose --grpc-port or stop the conflicting process" >&2
    exit 2
fi

log "Preflight checks passed"

if [[ "$DRY_RUN" == "true" ]]; then
    log "Dry-run mode — stopping after preflight"
    exit 0
fi

# ── teardown any previous run ────────────────────────────────────────────────

log "Tearing down any previous compose stack..."
cd "${REPO_ROOT}"
COMPOSE_PROJECT_NAME="$COMPOSE_PROJECT_NAME" \
docker compose --profile "$COMPOSE_PROFILE" down --remove-orphans --volumes 2>>"$LOG_FILE" || true

# ── generate TLS certificates ────────────────────────────────────────────────

log "Generating compose TLS certs..."
bash scripts/generate_compose_tls.sh >>"$LOG_FILE" 2>&1

# ── start compose stack ──────────────────────────────────────────────────────

log "Starting distributed compose stack..."
export BONSAI_VAULT_PASSPHRASE="$PASSPHRASE"
export BONSAI_DISTRIBUTED_HTTP_PORT="$HTTP_PORT"
export BONSAI_DISTRIBUTED_GRPC_PORT="$GRPC_PORT"
export CLAB_NETWORK
export COMPOSE_PROJECT_NAME
docker compose --profile "$COMPOSE_PROFILE" up -d 2>>"$LOG_FILE"

# ── wait for bonsai-core healthcheck ─────────────────────────────────────────

log "Waiting for bonsai-core to become healthy (up to 60s)..."
if ! wait_for_http "${BONSAI_HTTP}/api/topology" 60; then
    fail "bonsai-core health endpoint did not respond within 60s"
    docker compose --profile "$COMPOSE_PROFILE" logs --tail=50 >>"$LOG_FILE" 2>&1
    docker compose --profile "$COMPOSE_PROFILE" down --remove-orphans --volumes 2>>"$LOG_FILE" || true
    exit 1
fi
pass "bonsai-core health OK"

# ── assertions ───────────────────────────────────────────────────────────────

log "Asserting /api/setup/status shows configured targets..."
SETUP_STATUS=$(curl -sf "${BONSAI_HTTP}/api/setup/status")
if echo "$SETUP_STATUS" | grep -q '"has_devices":true'; then
    pass "/api/setup/status has_devices=true"
else
    fail "/api/setup/status: unexpected response: $SETUP_STATUS"
fi

log "Asserting /api/credentials is reachable..."
CREDS=$(curl -sf "${BONSAI_HTTP}/api/credentials")
if echo "$CREDS" | grep -q '"unlocked"'; then
    pass "/api/credentials reachable"
else
    fail "/api/credentials unexpected response: $CREDS"
fi

log "Asserting /api/onboarding/devices lists DC lab targets..."
DEVICES=$(curl -sf "${BONSAI_HTTP}/api/onboarding/devices")
if echo "$DEVICES" | grep -q '172\.100\.103\.11:57400'; then
    pass "/api/onboarding/devices includes DC lab targets"
else
    fail "/api/onboarding/devices missing DC lab targets: $DEVICES"
fi

log "Asserting /api/collectors lists collectors with connected: true..."
COLLECTORS=$(curl -sf "${BONSAI_HTTP}/api/collectors")
if printf '%s' "$COLLECTORS" | python3 -c '
import json, sys
payload = json.load(sys.stdin)
collectors = {c.get("id"): c for c in payload.get("collectors", [])}
required = ("collector-1", "collector-2")
ok = all(collectors.get(cid, {}).get("connected") is True for cid in required)
ok = ok and all(collectors.get(cid, {}).get("assigned_device_count", 0) > 0 for cid in required)
sys.exit(0 if ok else 1)
'; then
    pass "/api/collectors shows collector-1 and collector-2 connected"
else
    fail "/api/collectors missing connected collector-1/collector-2: $COLLECTORS"
fi

# ── teardown / keep running ──────────────────────────────────────────────────

if [[ "$KEEP_RUNNING" == "true" ]]; then
    log "Leaving compose stack running for distributed validation window"
else
    log "Tearing down compose stack..."
    docker compose --profile "$COMPOSE_PROFILE" down --remove-orphans --volumes 2>>"$LOG_FILE" || true
fi

# ── write test result ─────────────────────────────────────────────────────────

mkdir -p "$RESULT_DIR"
RESULT_FILE="${RESULT_DIR}/$(date +%Y%m%d)-${RESULT,,}.md"
BONSAI_SHA=$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo "unknown")

cat > "$RESULT_FILE" <<EOF
# Docker Compose E2E integration test

**Date**: $(date +%Y-%m-%d)
**Operator**: $(git config user.name 2>/dev/null || echo "unknown")
**Bonsai version**: ${BONSAI_SHA}
**Lab topology**: docker compose --profile ${COMPOSE_PROFILE}
**HTTP URL**: ${BONSAI_HTTP}
**Host gRPC port**: ${GRPC_PORT}
**ContainerLab network**: ${CLAB_NETWORK}
**Kept running**: ${KEEP_RUNNING}
**External versions**: Docker $(docker --version | cut -d' ' -f3 | tr -d ',')

## Result

**${RESULT}**

## Log

\`${LOG_FILE}\`
EOF

log "Result written to: $RESULT_FILE"
log "=== ${RESULT} ==="

if [[ "$RESULT" == "FAIL" ]]; then
    exit 1
fi
