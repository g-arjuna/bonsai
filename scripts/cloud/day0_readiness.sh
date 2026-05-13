#!/usr/bin/env bash
# scripts/cloud/day0_readiness.sh — T7-1: Day-0 pre-flight for the 7-day handoff proof.
#
# Runs all Day-0 checks from docs/operations/7day_handoff.md.
# Exits 0 only if ALL required checks pass.
# On success, writes runtime/driver_results/handoff_start.txt to start the clock.
#
# Works on both cloud VM (systemd + /mnt/bonsai-archive) and laptop (docker + runtime/).
#
# Usage:
#   bash scripts/cloud/day0_readiness.sh              # full check + start clock if ready
#   bash scripts/cloud/day0_readiness.sh --check-only # check only, do not write start file
#   bash scripts/cloud/day0_readiness.sh --force      # overwrite handoff_start.txt if exists

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_DIR="${INSTALL_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
ARCHIVE_MOUNT="${ARCHIVE_MOUNT:-/mnt/bonsai-archive}"
API_BASE="${API_BASE:-http://127.0.0.1:3000}"
RESULT_DIR="${RESULT_DIR:-$INSTALL_DIR/runtime/driver_results}"
HANDOFF_START="${HANDOFF_START:-$RESULT_DIR/handoff_start.txt}"
FEATURE_INDEX="$INSTALL_DIR/docs/testing/FEATURE_INDEX.md"

CHECK_ONLY=false
FORCE=false
for arg in "$@"; do
    case "$arg" in
        --check-only) CHECK_ONLY=true ;;
        --force)      FORCE=true ;;
        --help|-h)
            echo "Usage: $0 [--check-only] [--force]"
            exit 0
            ;;
        *) echo "Unknown argument: $arg" >&2; exit 1 ;;
    esac
done

# ── Colour helpers ────────────────────────────────────────────────────────────

if [[ -t 1 ]]; then
    GREEN='\033[0;32m'
    RED='\033[0;31m'
    YELLOW='\033[0;33m'
    BOLD='\033[1m'
    RESET='\033[0m'
else
    GREEN='' RED='' YELLOW='' BOLD='' RESET=''
fi

PASS="${GREEN}PASS${RESET}"
FAIL="${RED}FAIL${RESET}"
WARN="${YELLOW}WARN${RESET}"

# ── Check state ───────────────────────────────────────────────────────────────

TOTAL=0
PASSED=0
FAILED=0
declare -a CHECK_LOG=()

_result() {
    local name="$1"
    local status="$2"  # pass | fail | warn
    local detail="$3"
    TOTAL=$((TOTAL + 1))
    case "$status" in
        pass)
            PASSED=$((PASSED + 1))
            CHECK_LOG+=("  ${PASS}  $name — $detail")
            ;;
        warn)
            # Warnings count as pass (non-blocking) but are surfaced
            PASSED=$((PASSED + 1))
            CHECK_LOG+=("  ${WARN}  $name — $detail")
            ;;
        fail)
            FAILED=$((FAILED + 1))
            CHECK_LOG+=("  ${FAIL}  $name — $detail")
            ;;
    esac
}

echo ""
echo -e "${BOLD}=== Bonsai Day-0 Readiness Check ===${RESET}"
echo "    Install dir : $INSTALL_DIR"
echo "    API base    : $API_BASE"
echo "    $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
echo ""

# ── Check 1: bonsai-mgmt Docker network present ───────────────────────────────

echo "[ 1/7 ] Docker network bonsai-mgmt..."
if ! command -v docker &>/dev/null; then
    _result "docker_network" "fail" "docker not found on PATH"
else
    # Match bonsai-mgmt or bonsai-*-mgmt (topology-prefixed variants like bonsai-cloud-dc-mgmt)
    NET_NAMES=$(docker network ls --format '{{.Name}}' 2>/dev/null | grep -E '^bonsai(-[^/]+)?-mgmt$' || true)
    NET_COUNT=$(echo "$NET_NAMES" | grep -c . || true)
    if [[ "$NET_COUNT" -ge 1 ]]; then
        NET_NAME=$(echo "$NET_NAMES" | head -1)
        NET_SUBNET=$(docker network inspect "$NET_NAME" --format '{{range .IPAM.Config}}{{.Subnet}}{{end}}' 2>/dev/null || echo "unknown")
        _result "docker_network" "pass" "$NET_NAME present (subnet: $NET_SUBNET)"
    else
        _result "docker_network" "fail" "no bonsai mgmt network found — run 'sudo containerlab deploy' first"
    fi
fi

# ── Check 2: ContainerLab lab nodes running ────────────────────────────────────

echo "[ 2/7 ] ContainerLab nodes..."
if ! command -v docker &>/dev/null; then
    _result "lab_nodes" "fail" "docker not found"
else
    # Count containers with clab- prefix (works for any topology)
    CLAB_RUNNING=$(docker ps --filter "name=clab-bonsai" --format '{{.Names}}' 2>/dev/null | wc -l)
    if [[ "$CLAB_RUNNING" -ge 4 ]]; then
        _result "lab_nodes" "pass" "$CLAB_RUNNING ContainerLab nodes running"
    elif [[ "$CLAB_RUNNING" -gt 0 ]]; then
        _result "lab_nodes" "warn" "only $CLAB_RUNNING ContainerLab nodes running (expected ≥4)"
    else
        _result "lab_nodes" "fail" "no ContainerLab nodes running — deploy a topology first"
    fi
fi

# ── Check 3: bonsai API responding with active subscriptions ──────────────────

echo "[ 3/7 ] Bonsai API + gNMI subscriptions..."
API_OUT=""
if API_OUT=$(curl -sf --max-time 5 "$API_BASE/api/operations" 2>/dev/null); then
    OBS_SUBS=$(echo "$API_OUT" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('observed_subscriptions',0))" 2>/dev/null || echo "0")
    if [[ "$OBS_SUBS" -gt 0 ]]; then
        _result "gnmi_subscriptions" "pass" "bonsai API up; observed_subscriptions=$OBS_SUBS"
    else
        _result "gnmi_subscriptions" "warn" "bonsai API up but observed_subscriptions=0 (lab nodes may still be converging)"
    fi
else
    _result "gnmi_subscriptions" "fail" "bonsai API not responding at $API_BASE (is bonsai running?)"
fi

# ── Check 4: Cron jobs installed ─────────────────────────────────────────────

echo "[ 4/7 ] Cron entries..."
CRONTAB_OUT="$(crontab -l 2>/dev/null || true)"
HAS_SYNC=false
HAS_CHECK=false
echo "$CRONTAB_OUT" | grep -q "bonsai-cloud-sync"  && HAS_SYNC=true
echo "$CRONTAB_OUT" | grep -q "bonsai-cloud-check" && HAS_CHECK=true

if $HAS_SYNC && $HAS_CHECK; then
    _result "cron_installed" "pass" "bonsai-cloud-sync and bonsai-cloud-check both present"
elif ! $HAS_SYNC && ! $HAS_CHECK; then
    _result "cron_installed" "fail" "neither bonsai-cloud-sync nor bonsai-cloud-check installed — run scripts/cloud/install_cron.sh"
elif ! $HAS_SYNC; then
    _result "cron_installed" "fail" "bonsai-cloud-sync missing — run scripts/cloud/install_cron.sh"
else
    _result "cron_installed" "fail" "bonsai-cloud-check missing — run scripts/cloud/install_cron.sh"
fi

# ── Check 5: GITHUB_TOKEN set and usable ─────────────────────────────────────

echo "[ 5/7 ] GITHUB_TOKEN and daily_sync --dry-run..."
# Source env file from known paths if token not already in environment.
# Cron subprocesses don't inherit the interactive shell's exports.
if [[ -z "${GITHUB_TOKEN:-}" ]]; then
    for _env_file in "$HOME/.bonsai.env" "/opt/bonsai/instance.env"; do
        if [[ -f "$_env_file" ]]; then
            # shellcheck source=/dev/null
            . "$_env_file"
            break
        fi
    done
fi
if [[ -z "${GITHUB_TOKEN:-}" ]]; then
    _result "github_token" "fail" "GITHUB_TOKEN not set and not found in ~/.bonsai.env or /opt/bonsai/instance.env"
else
    TOKEN_LEN=${#GITHUB_TOKEN}
    # Validate token against GitHub API — works on both laptop and cloud without
    # cloud-specific paths (daily_sync.sh tries /mnt/bonsai-archive on laptop).
    HTTP_STATUS=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 \
        -H "Authorization: token $GITHUB_TOKEN" \
        https://api.github.com/user 2>/dev/null || echo "000")
    if [[ "$HTTP_STATUS" == "200" ]]; then
        _result "github_token" "pass" "GITHUB_TOKEN valid (len=$TOKEN_LEN; GitHub API 200)"
    else
        _result "github_token" "fail" "GITHUB_TOKEN set but GitHub API returned HTTP $HTTP_STATUS (invalid or expired?)"
    fi
fi

# ── Check 6: Archive path writable ────────────────────────────────────────────

echo "[ 6/7 ] Archive path..."
# Prefer cloud archive mount; fall back to repo runtime dir
if [[ -d "$ARCHIVE_MOUNT/archive" ]]; then
    ARCHIVE_PATH="$ARCHIVE_MOUNT/archive"
    DISK_INFO=$(df -h "$ARCHIVE_MOUNT" 2>/dev/null | tail -1 | awk '{print "used=" $3 " avail=" $4 " use%=" $5}' || echo "unknown")
    if touch "$ARCHIVE_PATH/.readiness_probe" 2>/dev/null; then
        rm -f "$ARCHIVE_PATH/.readiness_probe"
        _result "archive_writable" "pass" "cloud archive mount writable ($DISK_INFO)"
    else
        _result "archive_writable" "fail" "cloud archive mount not writable: $ARCHIVE_PATH"
    fi
elif [[ -d "$INSTALL_DIR/runtime/archive" ]] || mkdir -p "$INSTALL_DIR/runtime/archive" 2>/dev/null; then
    ARCHIVE_PATH="$INSTALL_DIR/runtime/archive"
    if touch "$ARCHIVE_PATH/.readiness_probe" 2>/dev/null; then
        rm -f "$ARCHIVE_PATH/.readiness_probe"
        DISK_INFO=$(df -h "$ARCHIVE_PATH" 2>/dev/null | tail -1 | awk '{print "avail=" $4 " use%=" $5}' || echo "unknown")
        _result "archive_writable" "pass" "local runtime archive writable ($DISK_INFO)"
    else
        _result "archive_writable" "fail" "runtime/archive not writable"
    fi
else
    _result "archive_writable" "fail" "neither $ARCHIVE_MOUNT/archive nor runtime/archive accessible"
fi

# ── Check 7: Feature index exists and is recent ───────────────────────────────

echo "[ 7/7 ] Feature index..."
if [[ ! -f "$FEATURE_INDEX" ]]; then
    _result "feature_index" "fail" "docs/testing/FEATURE_INDEX.md not found — run T3-1"
else
    # Warn if older than 7 days (stale)
    INDEX_AGE_DAYS=$(python3 -c "
import os, time
mtime = os.path.getmtime('$FEATURE_INDEX')
age_days = (time.time() - mtime) / 86400
print(f'{age_days:.1f}')
" 2>/dev/null || echo "0")
    if python3 -c "exit(0 if float('$INDEX_AGE_DAYS') <= 7 else 1)" 2>/dev/null; then
        _result "feature_index" "pass" "FEATURE_INDEX.md present (age: ${INDEX_AGE_DAYS} days)"
    else
        _result "feature_index" "warn" "FEATURE_INDEX.md is ${INDEX_AGE_DAYS} days old — consider updating before start"
    fi
fi

# ── Summary ───────────────────────────────────────────────────────────────────

echo ""
echo -e "${BOLD}Results:${RESET}"
for line in "${CHECK_LOG[@]}"; do
    echo -e "$line"
done

echo ""
echo -e "${BOLD}Score: $PASSED/$TOTAL checks passed | $FAILED failed${RESET}"

if [[ "$FAILED" -gt 0 ]]; then
    echo ""
    echo -e "${RED}NOT READY — fix the $FAILED failing check(s) above before starting the 7-day clock.${RESET}"
    echo ""
    exit 1
fi

echo ""
echo -e "${GREEN}READY — all $TOTAL checks passed.${RESET}"

# ── Start the clock ───────────────────────────────────────────────────────────

if $CHECK_ONLY; then
    echo "(--check-only: not writing handoff_start.txt)"
    echo ""
    exit 0
fi

if [[ -f "$HANDOFF_START" ]] && ! $FORCE; then
    EXISTING_START=$(cat "$HANDOFF_START" | head -1 || echo "unknown")
    echo ""
    echo -e "${YELLOW}WARN: $HANDOFF_START already exists (started: $EXISTING_START).${RESET}"
    echo "  Use --force to overwrite and reset the 7-day window."
    echo ""
    exit 0
fi

mkdir -p "$RESULT_DIR"

START_TS=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
GIT_SHA=$(git -C "$INSTALL_DIR" rev-parse --short HEAD 2>/dev/null || echo "unknown")

cat > "$HANDOFF_START" <<EOF
start_ts: $START_TS
git_sha: $GIT_SHA
api_base: $API_BASE
install_dir: $INSTALL_DIR
checks_passed: $PASSED
checks_total: $TOTAL
EOF

echo "Clock started: $HANDOFF_START"
echo "  start_ts : $START_TS"
echo "  git_sha  : $GIT_SHA"
echo ""
echo "Day-7 closure: run 'bash scripts/cloud/day7_closure.sh' after 7 days."
echo ""
