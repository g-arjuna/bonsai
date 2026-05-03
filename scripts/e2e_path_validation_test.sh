#!/usr/bin/env bash
# T2-8 (v10) — Path profile validation against live device capabilities
#
# Tests that for each monitored device:
#   1. bonsai has active subscriptions (paths are registered)
#   2. Interface counter telemetry is flowing (interfaces path delivers data)
#   3. BGP neighbor state is populated (bgp path delivers data)
#   4. LLDP neighbor data is visible (lldp path delivers data)
#   5. No device is in a degraded/error health state
#
# This validates the full path profile stack end-to-end:
# gNMI subscribe → decode → graph write → API query
#
# For new-device onboarding path discovery (pre-add), use
# the interactive wizard at /api/onboarding/discover (POST).
#
# Prerequisites:
#   - ContainerLab lab running (bonsai-phase4 topology)
#   - bonsai binary built: cargo build --release
#   - bonsai running and reachable with devices connected
#
# Usage:
#   ./scripts/e2e_path_validation_test.sh [--dry-run] [--bonsai-http URL] [--min-devices N]
#
# Produces:
#   docs/test_results/e2e_path_validation/$(date +%Y%m%d)-<result>.md

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESULT_DIR="${REPO_ROOT}/docs/test_results/e2e_path_validation"
BONSAI_HTTP="${BONSAI_HTTP:-http://localhost:3000}"
LOG_FILE="/tmp/bonsai-e2e-pathval-$(date +%Y%m%d-%H%M%S).log"
DRY_RUN=false
RESULT="PASS"
MIN_DEVICES=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)       DRY_RUN=true ;;
        --bonsai-http)   BONSAI_HTTP="$2"; shift ;;
        --min-devices)   MIN_DEVICES="$2"; shift ;;
        *) echo "Unknown argument: $1"; exit 1 ;;
    esac
    shift
done

log()  { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG_FILE"; }
fail() { log "FAIL: $*"; RESULT="FAIL"; }
pass() { log "PASS: $*"; }
warn() { log "WARN: $*"; }

# ── cleanup (runs on EXIT regardless of success/failure) ──────────────────────
# Read-only test — no state to tear down, but trap ensures LOG_FILE is flushed.

cleanup() {
    : # no teardown needed for read-only test
}
trap cleanup EXIT

# ── preflight ─────────────────────────────────────────────────────────────────

log "=== Bonsai Path Profile Validation E2E Test ==="
log "Bonsai HTTP: ${BONSAI_HTTP}"
log "Log: ${LOG_FILE}"

if ! curl -sf "${BONSAI_HTTP}/api/topology" >/dev/null 2>&1; then
    echo "error: bonsai not reachable at ${BONSAI_HTTP} — start bonsai first" >&2; exit 2
fi
if ! command -v jq &>/dev/null; then
    echo "error: jq not found on PATH" >&2; exit 2
fi

log "Preflight checks passed"
[[ "$DRY_RUN" == "true" ]] && { log "Dry-run mode — exiting"; exit 0; }

# ── Step 1: Enumerate devices ─────────────────────────────────────────────────

log "Step 1: Enumerating monitored devices from topology..."
TOPOLOGY=$(curl -sf "${BONSAI_HTTP}/api/topology" 2>/dev/null || echo '{"devices":[]}')
DEVICE_COUNT=$(echo "$TOPOLOGY" | jq '.devices | length' 2>/dev/null || echo 0)

if [[ "$DEVICE_COUNT" -lt "$MIN_DEVICES" ]]; then
    fail "Only $DEVICE_COUNT device(s) in topology, expected ≥${MIN_DEVICES}"
    mkdir -p "$RESULT_DIR"
    echo "**FAIL** — No devices found." > "${RESULT_DIR}/$(date +%Y%m%d)-fail.md"
    exit 1
fi
log "Found $DEVICE_COUNT device(s)"

DEVICES_WITH_COUNTERS=0
DEVICES_WITH_BGP=0
DEVICES_WITH_LLDP=0
DEVICES_WITH_SUBS=0
DEVICES_DEGRADED=0

# ── Step 2: Per-device telemetry validation ───────────────────────────────────

log "Step 2: Validating telemetry paths per device..."

while IFS= read -r addr; do
    [[ -z "$addr" ]] && continue
    log "  --- Device: $addr ---"

    DETAIL=$(curl -sf "${BONSAI_HTTP}/api/devices/${addr}" 2>/dev/null || echo '{}')

    # 2a. Subscription paths registered
    SUB_COUNT=$(echo "$DETAIL" | jq '.subscription_statuses | length' 2>/dev/null || echo 0)
    if [[ "$SUB_COUNT" -gt 0 ]]; then
        DEVICES_WITH_SUBS=$((DEVICES_WITH_SUBS + 1))
        log "  $addr: $SUB_COUNT subscription path(s) registered"
        echo "$DETAIL" | jq -r '.subscription_statuses[].path' 2>/dev/null | while read -r p; do
            log "    path: $p"
        done
    else
        warn "  $addr: no subscription paths — device may not have finished connecting"
    fi

    # 2b. Interface counters (interfaces path delivering data)
    IF_WITH_DATA=$(echo "$DETAIL" | jq '[.interfaces[]? | select(.in_octets > 0 or .out_octets > 0)] | length' 2>/dev/null || echo 0)
    if [[ "$IF_WITH_DATA" -gt 0 ]]; then
        pass "$addr: interface telemetry flowing ($IF_WITH_DATA interfaces with non-zero counters)"
        DEVICES_WITH_COUNTERS=$((DEVICES_WITH_COUNTERS + 1))
    else
        fail "$addr: no interface counter data received (interfaces path not delivering)"
    fi

    # 2c. BGP neighbor state (bgp path delivering data)
    BGP_COUNT=$(echo "$TOPOLOGY" | jq --arg a "$addr" '[.devices[] | select(.address == $a) | .bgp[]?] | length' 2>/dev/null || echo 0)
    if [[ "$BGP_COUNT" -gt 0 ]]; then
        ESTABLISHED=$(echo "$TOPOLOGY" | jq --arg a "$addr" \
            '[.devices[] | select(.address == $a) | .bgp[]? | select(.state == "established")] | length' 2>/dev/null || echo 0)
        pass "$addr: BGP telemetry flowing ($BGP_COUNT sessions, $ESTABLISHED established)"
        DEVICES_WITH_BGP=$((DEVICES_WITH_BGP + 1))
    else
        warn "$addr: no BGP sessions visible (normal if device has no BGP config)"
    fi

    # 2d. LLDP neighbors (lldp path delivering data)
    LLDP_COUNT=$(echo "$TOPOLOGY" | jq \
        '[.links[]? | select(.src_device == "'"$addr"'") | .src_iface] | length' 2>/dev/null || echo 0)
    if [[ "$LLDP_COUNT" -gt 0 ]]; then
        pass "$addr: LLDP telemetry flowing ($LLDP_COUNT links discovered)"
        DEVICES_WITH_LLDP=$((DEVICES_WITH_LLDP + 1))
    else
        warn "$addr: no LLDP links visible"
    fi

    # 2e. Health state
    HEALTH=$(echo "$TOPOLOGY" | jq -r --arg a "$addr" \
        '.devices[] | select(.address == $a) | .health' 2>/dev/null || echo "unknown")
    if [[ "$HEALTH" == "critical" ]]; then
        fail "$addr: health=critical"
        DEVICES_DEGRADED=$((DEVICES_DEGRADED + 1))
    else
        log "  $addr: health=$HEALTH"
    fi

done <<< "$(echo "$TOPOLOGY" | jq -r '.devices[].address')"

# ── Step 3: Summary ───────────────────────────────────────────────────────────

log "Step 3: Summary..."
log "  Total devices:                $DEVICE_COUNT"
log "  With subscriptions:           $DEVICES_WITH_SUBS / $DEVICE_COUNT"
log "  With interface counters:      $DEVICES_WITH_COUNTERS / $DEVICE_COUNT"
log "  With BGP telemetry:           $DEVICES_WITH_BGP / $DEVICE_COUNT"
log "  With LLDP telemetry:          $DEVICES_WITH_LLDP / $DEVICE_COUNT"
log "  Degraded (health=critical):   $DEVICES_DEGRADED"

if [[ "$DEVICES_WITH_COUNTERS" -eq 0 ]]; then
    fail "No devices delivered interface counter telemetry — check gNMI subscriptions"
fi
if [[ "$DEVICES_DEGRADED" -gt 0 ]]; then
    fail "$DEVICES_DEGRADED device(s) in critical health state"
fi

# ── write result ──────────────────────────────────────────────────────────────

mkdir -p "$RESULT_DIR"
RESULT_FILE="${RESULT_DIR}/$(date +%Y%m%d)-${RESULT,,}.md"
BONSAI_SHA=$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo "unknown")

cat > "$RESULT_FILE" <<EOF
# Path Profile Validation E2E test

**Date**: $(date +%Y-%m-%d)
**Operator**: $(git config user.name 2>/dev/null || echo "unknown")
**Bonsai version**: ${BONSAI_SHA}
**Lab topology**: lab/fast-iteration/bonsai-phase4

## Result

**${RESULT}**

## Summary

| Check | Count |
|-------|-------|
| Total devices | ${DEVICE_COUNT} |
| With subscription paths | ${DEVICES_WITH_SUBS} |
| With interface counter data | ${DEVICES_WITH_COUNTERS} |
| With BGP telemetry | ${DEVICES_WITH_BGP} |
| With LLDP telemetry | ${DEVICES_WITH_LLDP} |
| Critical health | ${DEVICES_DEGRADED} |

## Log

\`${LOG_FILE}\`
EOF

log "Result written to: $RESULT_FILE"
log "=== ${RESULT} ==="
[[ "$RESULT" == "FAIL" ]] && exit 1 || exit 0
