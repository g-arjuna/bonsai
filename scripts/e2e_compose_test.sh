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
#   ./scripts/e2e_compose_test.sh [--dry-run] [--passphrase <phrase>]
#
# Produces:
#   docs/test_results/e2e_compose/$(date +%Y%m%d)-<result>.md

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESULT_DIR="${REPO_ROOT}/docs/test_results/e2e_compose"
BONSAI_HTTP="http://localhost:3000"
LOG_FILE="/tmp/bonsai-e2e-compose-$(date +%Y%m%d-%H%M%S).log"
DRY_RUN=false
PASSPHRASE="${BONSAI_VAULT_PASSPHRASE:-}"

# ── argument parsing ─────────────────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) DRY_RUN=true ;;
        --passphrase) PASSPHRASE="$2"; shift ;;
        *) echo "Unknown argument: $1"; exit 1 ;;
    esac
    shift
done

# ── helpers ──────────────────────────────────────────────────────────────────

log() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG_FILE"; }
fail() { log "FAIL: $*"; RESULT="FAIL"; }
pass() { log "PASS: $*"; }

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
    docker compose --profile distributed down --remove-orphans --volumes 2>>"$LOG_FILE" || true
}
trap cleanup EXIT

# ── preflight ─────────────────────────────────────────────────────────────────

log "=== Bonsai Docker Compose E2E Test ==="
log "Repo: ${REPO_ROOT}"
log "Log: ${LOG_FILE}"

if ! command -v docker &>/dev/null; then
    echo "error: docker not found on PATH" >&2; exit 2
fi
if ! docker compose version &>/dev/null; then
    echo "error: docker compose v2 not found" >&2; exit 2
fi
if [[ -z "$PASSPHRASE" ]]; then
    echo "error: BONSAI_VAULT_PASSPHRASE not set (use --passphrase or export the env var)" >&2; exit 2
fi

log "Preflight checks passed"

if [[ "$DRY_RUN" == "true" ]]; then
    log "Dry-run mode — stopping after preflight"
    exit 0
fi

# ── teardown any previous run ────────────────────────────────────────────────

log "Tearing down any previous compose stack..."
cd "${REPO_ROOT}"
docker compose --profile distributed down --remove-orphans --volumes 2>>"$LOG_FILE" || true

# ── generate TLS certificates ────────────────────────────────────────────────

log "Generating compose TLS certs..."
bash scripts/generate_compose_tls.sh >>"$LOG_FILE" 2>&1

# ── start compose stack ──────────────────────────────────────────────────────

log "Starting distributed compose stack..."
export BONSAI_VAULT_PASSPHRASE="$PASSPHRASE"
docker compose --profile distributed up -d 2>>"$LOG_FILE"

# ── wait for bonsai-core healthcheck ─────────────────────────────────────────

log "Waiting for bonsai-core to become healthy (up to 60s)..."
if ! wait_for_http "${BONSAI_HTTP}/api/topology" 60; then
    fail "bonsai-core health endpoint did not respond within 60s"
    docker compose logs --tail=50 >>"$LOG_FILE" 2>&1
    docker compose down --remove-orphans --volumes 2>>"$LOG_FILE" || true
    exit 1
fi
pass "bonsai-core health OK"

# ── seed lab credentials ─────────────────────────────────────────────────────

log "Seeding lab credentials..."
bash scripts/seed_lab_creds.sh --non-interactive >>"$LOG_FILE" 2>&1
pass "Lab credentials seeded"

# ── assertions ───────────────────────────────────────────────────────────────

log "Asserting /api/setup/status shows is_first_run: false..."
SETUP_STATUS=$(curl -sf "${BONSAI_HTTP}/api/setup/status")
if echo "$SETUP_STATUS" | grep -q '"is_first_run":false'; then
    pass "/api/setup/status is_first_run=false"
else
    fail "/api/setup/status: unexpected response: $SETUP_STATUS"
fi

log "Asserting /api/credentials lists seeded aliases..."
CREDS=$(curl -sf "${BONSAI_HTTP}/api/credentials")
if echo "$CREDS" | grep -q '"alias"'; then
    pass "/api/credentials lists aliases"
else
    fail "/api/credentials returned no aliases: $CREDS"
fi

log "Asserting /api/onboarding/devices is empty before device add..."
DEVICES=$(curl -sf "${BONSAI_HTTP}/api/onboarding/devices")
if echo "$DEVICES" | grep -q '^\[\]$\|"devices":\[\]'; then
    pass "/api/onboarding/devices empty"
fi

log "Asserting /api/collectors lists collectors with connected: true..."
COLLECTORS=$(curl -sf "${BONSAI_HTTP}/api/collectors")
if echo "$COLLECTORS" | grep -q '"connected":true'; then
    pass "/api/collectors shows connected collectors"
else
    fail "/api/collectors shows no connected collectors: $COLLECTORS"
fi

# ── teardown ─────────────────────────────────────────────────────────────────

log "Tearing down compose stack..."
docker compose --profile distributed down --remove-orphans --volumes 2>>"$LOG_FILE" || true

# ── write test result ─────────────────────────────────────────────────────────

mkdir -p "$RESULT_DIR"
RESULT_FILE="${RESULT_DIR}/$(date +%Y%m%d)-${RESULT,,}.md"
BONSAI_SHA=$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo "unknown")

cat > "$RESULT_FILE" <<EOF
# Docker Compose E2E integration test

**Date**: $(date +%Y-%m-%d)
**Operator**: $(git config user.name 2>/dev/null || echo "unknown")
**Bonsai version**: ${BONSAI_SHA}
**Lab topology**: docker compose --profile distributed
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
