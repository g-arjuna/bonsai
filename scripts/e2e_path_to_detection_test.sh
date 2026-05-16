#!/usr/bin/env bash
# e2e_path_to_detection_test.sh — D2-T3 (DV1)
#
# End-to-end trace: gNMI path-find → subscription → state-change-event →
# Python sidecar detection.
#
# Follows ONE event through five subsystems and timestamps every stage.
# Passes only if ALL stages occur within 30 s of fault injection.
#
# Prerequisites (run on Ubuntu ops laptop with live DC lab):
#   - bonsai running with rules sidecar registered:
#       bash scripts/ops/start_bonsai_with_sidecar.sh
#   - Lab devices reachable (check: bash scripts/check_lab.sh)
#   - BONSAI_LOCAL_ADDR set or defaults to localhost:50051
#   - python3 with pyyaml, grpcio, protobuf (pip install -r python/requirements.txt)
#   - inject_fault.py accessible at python/inject_fault.py
#
# Usage:
#   bash scripts/e2e_path_to_detection_test.sh [options]
#
# Options:
#   --device   ADDR   device address to target (default: first SRL leaf from bonsai.toml)
#   --peer     IP     BGP peer to disable (default: auto-detect first eBGP peer)
#   --timeout  SECS   max seconds to wait for detection (default: 30)
#   --bonsai   URL    bonsai HTTP base URL (default: http://localhost:3000)
#   --heal            re-enable the peer after the test (default: true)
#   --no-heal         skip re-enabling (leaves fault injected)
#   --trace-out FILE  write trace artefact to FILE (default: /tmp/e2e_trace_<ts>.yaml)
#
# Exit codes:
#   0  all five stages passed within timeout
#   1  one or more stages failed or timed out

set -euo pipefail

# ── defaults ──────────────────────────────────────────────────────────────────
BONSAI_HTTP="${BONSAI_HTTP:-http://localhost:3000}"
BONSAI_LOCAL_ADDR="${BONSAI_LOCAL_ADDR:-localhost:50051}"
TARGET_DEVICE=""
TARGET_PEER=""
TIMEOUT_SECS=30
HEAL=true
TS=$(date +%Y%m%d_%H%M%S)
TRACE_OUT="/tmp/e2e_trace_${TS}.yaml"
PY=$(command -v python3 || command -v python)

# ── arg parse ─────────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --device)    TARGET_DEVICE="$2"; shift ;;
        --peer)      TARGET_PEER="$2";   shift ;;
        --timeout)   TIMEOUT_SECS="$2";  shift ;;
        --bonsai)    BONSAI_HTTP="$2";   shift ;;
        --heal)      HEAL=true  ;;
        --no-heal)   HEAL=false ;;
        --trace-out) TRACE_OUT="$2"; shift ;;
        *) echo "Unknown arg: $1"; exit 1 ;;
    esac
    shift
done

# ── colour helpers ─────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BOLD='\033[1m'; RESET='\033[0m'
pass()  { echo -e "  ${GREEN}PASS${RESET} $*"; }
fail()  { echo -e "  ${RED}FAIL${RESET} $*"; }
info()  { echo -e "  ${YELLOW}....${RESET} $*"; }
stage() { echo -e "\n${BOLD}[$1]${RESET} $2"; }

# ── stage timing recorder ─────────────────────────────────────────────────────
declare -A STAGE_TS
record() { STAGE_TS["$1"]=$(date +%s%3N); }   # milliseconds

# ── helper: GET bonsai API ─────────────────────────────────────────────────────
bonsai_get() {
    curl -sf --max-time 5 "${BONSAI_HTTP}${1}" 2>/dev/null || true
}

# ── helper: wait for JSON array to contain rule_id ────────────────────────────
wait_for_detection() {
    local rule_id="$1"
    local device="$2"
    local deadline=$(( $(date +%s) + TIMEOUT_SECS ))
    while [[ $(date +%s) -lt $deadline ]]; do
        local result
        result=$(bonsai_get "/api/detections?device_address=${device}&limit=50")
        if echo "$result" | "$PY" -c "
import sys, json
data = json.load(sys.stdin)
rows = data if isinstance(data, list) else data.get('items', data.get('detections', []))
found = any(r.get('rule_id') == '${rule_id}' for r in rows)
sys.exit(0 if found else 1)
" 2>/dev/null; then
            return 0
        fi
        sleep 2
    done
    return 1
}

# ── helper: check state_change_events written ─────────────────────────────────
wait_for_state_change() {
    local device="$1"
    local event_type="$2"
    local deadline=$(( $(date +%s) + TIMEOUT_SECS ))
    while [[ $(date +%s) -lt $deadline ]]; do
        local result
        result=$(bonsai_get "/api/state_change_events?device_address=${device}&event_type=${event_type}&limit=10")
        if echo "$result" | "$PY" -c "
import sys, json
data = json.load(sys.stdin)
rows = data if isinstance(data, list) else data.get('items', data.get('events', []))
sys.exit(0 if rows else 1)
" 2>/dev/null; then
            return 0
        fi
        sleep 2
    done
    return 1
}

# ── helper: get sidecar detections_out_total ──────────────────────────────────
sidecar_detections_count() {
    bonsai_get "/api/sidecars" | "$PY" -c "
import sys, json
data = json.load(sys.stdin)
sidecars = data if isinstance(data, list) else data.get('sidecars', [])
for s in sidecars:
    if s.get('kind') == 'rules':
        print(s.get('detections_out_total', s.get('metrics', {}).get('detections_out_total', 0)))
        sys.exit(0)
print(0)
" 2>/dev/null || echo "0"
}

# ── STEP 0: resolve device + peer ─────────────────────────────────────────────
echo -e "\n${BOLD}=== E2E Path-to-Detection Trace Test ===${RESET}"
echo "  bonsai HTTP : $BONSAI_HTTP"
echo "  timeout     : ${TIMEOUT_SECS}s"
echo "  trace out   : $TRACE_OUT"

stage "0" "Resolve target device and BGP peer"

if [[ -z "$TARGET_DEVICE" ]]; then
    TARGET_DEVICE=$(bonsai_get "/api/devices" | "$PY" -c "
import sys, json
data = json.load(sys.stdin)
devs = data if isinstance(data, list) else data.get('devices', [])
for d in devs:
    addr = d.get('address', '')
    vendor = d.get('vendor', '')
    hostname = d.get('hostname', '')
    if 'leaf' in hostname.lower() or 'leaf' in addr.lower():
        print(addr)
        sys.exit(0)
# fallback: first device
if devs:
    print(devs[0].get('address',''))
" 2>/dev/null || true)
fi

if [[ -z "$TARGET_DEVICE" ]]; then
    fail "Cannot resolve target device — pass --device or ensure bonsai has devices registered"
    exit 1
fi
info "target device: $TARGET_DEVICE"

if [[ -z "$TARGET_PEER" ]]; then
    TARGET_PEER=$(bonsai_get "/api/bgp_neighbors?device_address=${TARGET_DEVICE}&limit=10" | "$PY" -c "
import sys, json
data = json.load(sys.stdin)
neighbors = data if isinstance(data, list) else data.get('neighbors', data.get('items', []))
for n in neighbors:
    state = n.get('session_state', n.get('state', '')).lower()
    if state in ('established', 'up'):
        print(n.get('peer_address', n.get('neighbor_address', '')))
        sys.exit(0)
" 2>/dev/null || true)
fi

if [[ -z "$TARGET_PEER" ]]; then
    fail "Cannot resolve a BGP peer in Established state — check lab or pass --peer"
    exit 1
fi
info "target peer  : $TARGET_PEER"

PASS_COUNT=0
FAIL_COUNT=0

# ── STAGE 1: Verify gNMI subscription is active ───────────────────────────────
stage "1" "gNMI subscription active for $TARGET_DEVICE"
record "stage1_start"

SUB_ACTIVE=$(bonsai_get "/api/subscriptions?device_address=${TARGET_DEVICE}" | "$PY" -c "
import sys, json
data = json.load(sys.stdin)
subs = data if isinstance(data, list) else data.get('subscriptions', [])
bgp_subs = [s for s in subs if 'bgp' in str(s).lower() or 'session' in str(s).lower()]
print(len(subs))
" 2>/dev/null || echo "0")

record "stage1_end"
if [[ "$SUB_ACTIVE" -gt 0 ]]; then
    pass "gNMI subscriptions active: $SUB_ACTIVE paths"
    ((PASS_COUNT++))
else
    fail "No gNMI subscriptions found for $TARGET_DEVICE"
    ((FAIL_COUNT++))
fi

# ── STAGE 2: Snapshot pre-fault sidecar counter ───────────────────────────────
stage "2" "Pre-fault snapshot"
record "stage2_start"

PRE_DETECTIONS=$(sidecar_detections_count)
info "sidecar detections_out_total before fault: $PRE_DETECTIONS"
record "stage2_end"
((PASS_COUNT++))

# ── STAGE 3: Inject fault ─────────────────────────────────────────────────────
stage "3" "Inject BGP fault: disable peer $TARGET_PEER on $TARGET_DEVICE"
record "fault_inject"

INJECT_START=$(date +%s%3N)
info "running inject_fault.py srl_bgp_disable …"

# Derive device hostname from address (strip port)
DEVICE_HOST="${TARGET_DEVICE%%:*}"

set +e
"$PY" python/inject_fault.py srl_bgp_disable \
    --device "$DEVICE_HOST" \
    --peer "$TARGET_PEER" 2>&1 | sed 's/^/    /'
INJECT_RC=$?
set -e

record "fault_injected"
if [[ $INJECT_RC -eq 0 ]]; then
    pass "fault injected at $(date +%H:%M:%S) (took $(( $(date +%s%3N) - INJECT_START )) ms)"
    ((PASS_COUNT++))
else
    fail "inject_fault.py returned non-zero ($INJECT_RC); continuing to probe anyway"
    ((FAIL_COUNT++))
fi

# ── STAGE 4: State-change event written to graph ─────────────────────────────
stage "4" "Wait for state_change_event (bgp_session_change) in graph — timeout ${TIMEOUT_SECS}s"
record "stage4_start"

if wait_for_state_change "$TARGET_DEVICE" "bgp_session_change"; then
    record "stage4_done"
    LATENCY=$(( ${STAGE_TS["stage4_done"]} - ${STAGE_TS["fault_injected"]} ))
    pass "state_change_event observed in graph (+${LATENCY} ms after injection)"
    ((PASS_COUNT++))
else
    record "stage4_done"
    fail "state_change_event NOT seen within ${TIMEOUT_SECS}s"
    ((FAIL_COUNT++))
fi

# ── STAGE 5: Detection row written by Python sidecar ─────────────────────────
stage "5" "Wait for detection (bgp_session_down) from Python sidecar — timeout ${TIMEOUT_SECS}s"
record "stage5_start"

if wait_for_detection "bgp_session_down" "$TARGET_DEVICE"; then
    record "stage5_done"
    LATENCY=$(( ${STAGE_TS["stage5_done"]} - ${STAGE_TS["fault_injected"]} ))
    pass "bgp_session_down detection observed (+${LATENCY} ms after injection)"
    ((PASS_COUNT++))
else
    record "stage5_done"
    fail "bgp_session_down detection NOT seen within ${TIMEOUT_SECS}s"
    ((FAIL_COUNT++))
fi

POST_DETECTIONS=$(sidecar_detections_count)
info "sidecar detections_out_total after fault : $POST_DETECTIONS"

if [[ "$POST_DETECTIONS" -gt "$PRE_DETECTIONS" ]]; then
    pass "sidecar counter incremented ($PRE_DETECTIONS → $POST_DETECTIONS)"
    ((PASS_COUNT++))
else
    fail "sidecar counter did NOT increment ($PRE_DETECTIONS → $POST_DETECTIONS)"
    ((FAIL_COUNT++))
fi

# ── HEAL ──────────────────────────────────────────────────────────────────────
if $HEAL; then
    stage "heal" "Re-enable BGP peer $TARGET_PEER on $TARGET_DEVICE"
    set +e
    "$PY" python/inject_fault.py srl_bgp_enable \
        --device "$DEVICE_HOST" \
        --peer "$TARGET_PEER" 2>&1 | sed 's/^/    /'
    set -e
    info "peer re-enabled"
fi

# ── Write trace artefact ──────────────────────────────────────────────────────
cat > "$TRACE_OUT" <<YAML
trace_id: e2e_path_to_detection_${TS}
date: $(date -u +%Y-%m-%dT%H:%M:%SZ)
target_device: ${TARGET_DEVICE}
target_peer: ${TARGET_PEER}
result: $([ $FAIL_COUNT -eq 0 ] && echo PASS || echo FAIL)
pass: ${PASS_COUNT}
fail: ${FAIL_COUNT}
timing_ms:
  fault_inject_to_state_change: $(( ${STAGE_TS["stage4_done"]:-0} - ${STAGE_TS["fault_injected"]:-0} ))
  fault_inject_to_detection:   $(( ${STAGE_TS["stage5_done"]:-0} - ${STAGE_TS["fault_injected"]:-0} ))
  total_wall_ms:               $(( ${STAGE_TS["stage5_done"]:-0} - ${STAGE_TS["stage1_start"]:-0} ))
sidecar_counter_before: ${PRE_DETECTIONS}
sidecar_counter_after:  ${POST_DETECTIONS}
YAML

# ── Summary ───────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}=== E2E Trace Results ===${RESET}"
echo "  PASS : $PASS_COUNT"
echo "  FAIL : $FAIL_COUNT"
echo "  trace: $TRACE_OUT"

if [[ $FAIL_COUNT -gt 0 ]]; then
    echo -e "\n  ${RED}OVERALL: FAIL${RESET}"
    exit 1
fi
echo -e "\n  ${GREEN}OVERALL: PASS${RESET}"
exit 0
