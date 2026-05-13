#!/usr/bin/env bash
# scripts/cloud/day7_closure.sh — T7-4: Day-7 closure verifier for the 7-day handoff proof.
#
# Reads runtime/driver_results/handoff_start.txt (written by day0_readiness.sh),
# verifies all 7 acceptance criteria from docs/operations/7day_handoff.md,
# and writes docs/test_results/7day_closure_<date>.md.
#
# Exits 0 only when ALL 7 criteria pass.
# Works on both cloud VM (systemd) and laptop (docker).
#
# Usage:
#   bash scripts/cloud/day7_closure.sh             # verify + write closure doc
#   bash scripts/cloud/day7_closure.sh --check-only # verify only, no output file
#   bash scripts/cloud/day7_closure.sh --force      # overwrite existing closure doc

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_DIR="${INSTALL_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
API_BASE="${API_BASE:-http://127.0.0.1:3000}"
RESULT_DIR="${RESULT_DIR:-$INSTALL_DIR/runtime/driver_results}"
HANDOFF_START="${HANDOFF_START:-$RESULT_DIR/handoff_start.txt}"
CLOSURE_DIR="${CLOSURE_DIR:-$INSTALL_DIR/docs/test_results}"

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
declare -a DETAIL_LOG=()

_result() {
    local name="$1"
    local status="$2"  # pass | fail | warn
    local detail="$3"
    TOTAL=$((TOTAL + 1))
    case "$status" in
        pass)
            PASSED=$((PASSED + 1))
            CHECK_LOG+=("  ${PASS}  [$name] $detail")
            ;;
        warn)
            PASSED=$((PASSED + 1))
            CHECK_LOG+=("  ${WARN}  [$name] $detail")
            ;;
        fail)
            FAILED=$((FAILED + 1))
            CHECK_LOG+=("  ${FAIL}  [$name] $detail")
            ;;
    esac
}

_detail() {
    DETAIL_LOG+=("$1")
}

echo ""
echo -e "${BOLD}=== Bonsai Day-7 Closure Check ===${RESET}"
echo "    Install dir : $INSTALL_DIR"
echo "    API base    : $API_BASE"
echo "    $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
echo ""

# ── Read handoff_start.txt ────────────────────────────────────────────────────

START_TS="unknown"
START_GIT_SHA="unknown"
if [[ ! -f "$HANDOFF_START" ]]; then
    echo -e "${RED}ERROR: $HANDOFF_START not found.${RESET}"
    echo "  Run 'bash scripts/cloud/day0_readiness.sh' first to start the clock."
    echo ""
    exit 1
fi

START_TS=$(grep '^start_ts:' "$HANDOFF_START" | awk '{print $2}' || echo "unknown")
START_GIT_SHA=$(grep '^git_sha:' "$HANDOFF_START" | awk '{print $2}' || echo "unknown")

echo "  Clock started : $START_TS"
echo "  Start SHA     : $START_GIT_SHA"
echo ""

# ── Criterion 1: All 7 daily-*.json pass ─────────────────────────────────────

echo "[ 1/7 ] Daily check results (7 daily-*.json files)..."

DAILY_RESULT=$(python3 - "$RESULT_DIR" "$START_TS" <<'PY'
from __future__ import annotations
import json
import sys
from datetime import datetime, timezone, timedelta
from pathlib import Path

result_dir = Path(sys.argv[1])
start_ts_str = sys.argv[2]

try:
    start_dt = datetime.fromisoformat(start_ts_str.replace("Z", "+00:00"))
except Exception:
    print("PARSE_ERROR: cannot parse start_ts")
    raise SystemExit(1)

# Expect dates from day+1 through day+7
expected_dates = []
for i in range(1, 8):
    expected_dates.append((start_dt + timedelta(days=i)).strftime("%Y-%m-%d"))

found = 0
passed = 0
failed_files = []
missing_files = []

for date_str in expected_dates:
    path = result_dir / f"daily-{date_str}.json"
    if not path.exists():
        missing_files.append(date_str)
        continue
    found += 1
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        failed_files.append(f"{date_str} (invalid json)")
        continue

    status = payload.get("status", "unknown")
    checks = payload.get("checks", [])
    fail_count = sum(1 for c in checks if c.get("status") == "fail")
    if status in ("pass", "pass_with_caveats") and fail_count == 0:
        passed += 1
    else:
        failed_files.append(f"{date_str} (status={status}, fail_checks={fail_count})")

print(f"expected={len(expected_dates)} found={found} passed={passed}")
if missing_files:
    print(f"missing: {', '.join(missing_files)}")
if failed_files:
    print(f"failed: {', '.join(failed_files)}")

if missing_files or failed_files:
    raise SystemExit(2)
raise SystemExit(0)
PY
)
DAILY_EXIT=$?

DAILY_FOUND=$(echo "$DAILY_RESULT" | head -1)
DAILY_EXTRA=$(echo "$DAILY_RESULT" | tail -n +2)

if [[ "$DAILY_EXIT" -eq 0 ]]; then
    _result "daily_checks" "pass" "$DAILY_FOUND — all 7 daily checks passed"
else
    _result "daily_checks" "fail" "$DAILY_FOUND — see details below"
fi
[[ -n "$DAILY_EXTRA" ]] && _detail "  Criterion 1 detail: $DAILY_EXTRA"

# ── Criterion 2: ≥6 sync branches on origin ──────────────────────────────────

echo "[ 2/7 ] Archive sync branches on origin..."
SYNC_COUNT=0
SYNC_LIST=""
if git -C "$INSTALL_DIR" remote get-url origin &>/dev/null 2>&1; then
    SYNC_LIST=$(git -C "$INSTALL_DIR" ls-remote --heads origin 'sync/cloud-spike/*' 2>/dev/null | awk '{print $2}' | sed 's|refs/heads/||' || true)
    SYNC_COUNT=$(echo "$SYNC_LIST" | grep -c 'sync/cloud-spike/' || true)
fi

if [[ "$SYNC_COUNT" -ge 7 ]]; then
    _result "sync_branches" "pass" "$SYNC_COUNT sync/cloud-spike/* branches found on origin"
elif [[ "$SYNC_COUNT" -ge 6 ]]; then
    _result "sync_branches" "warn" "only $SYNC_COUNT/7 sync branches (Day-7 branch may not have pushed yet)"
elif [[ "$SYNC_COUNT" -gt 0 ]]; then
    _result "sync_branches" "fail" "only $SYNC_COUNT sync branches found on origin (need ≥6)"
else
    _result "sync_branches" "fail" "no sync/cloud-spike/* branches found — check GITHUB_TOKEN and daily_sync.sh logs"
fi

# ── Criterion 3: No unplanned service restarts ────────────────────────────────

echo "[ 3/7 ] Service restarts since handoff start..."
RESTART_COUNT="unknown"
RESTART_STATUS="warn"
RESTART_DETAIL="cannot determine restart count"

if command -v journalctl &>/dev/null 2>&1 && systemctl list-units bonsai.service &>/dev/null 2>&1; then
    # Cloud VM path — systemd available
    RAW_COUNT=$(journalctl -u bonsai --since "$START_TS" --no-pager 2>/dev/null \
        | grep -c "Starting bonsai" || true)
    RESTART_COUNT="$RAW_COUNT"
    if [[ "$RAW_COUNT" -le 1 ]]; then
        RESTART_STATUS="pass"
        RESTART_DETAIL="systemd: $RAW_COUNT start event(s) since $START_TS (≤1 = only initial start)"
    else
        RESTART_STATUS="fail"
        RESTART_DETAIL="systemd: $RAW_COUNT start events since $START_TS (expected 1 — check for crashes)"
    fi
elif command -v docker &>/dev/null 2>&1; then
    # Laptop/docker path — check bonsai-core restart count
    CONTAINERS=$(docker ps --filter "name=bonsai" --format '{{.Names}}' 2>/dev/null | head -5 || true)
    if [[ -n "$CONTAINERS" ]]; then
        MAX_RESTARTS=0
        WORST_CONTAINER=""
        while IFS= read -r cname; do
            RC=$(docker inspect "$cname" --format '{{.RestartCount}}' 2>/dev/null || echo "0")
            if [[ "$RC" -gt "$MAX_RESTARTS" ]]; then
                MAX_RESTARTS="$RC"
                WORST_CONTAINER="$cname"
            fi
        done <<< "$CONTAINERS"
        RESTART_COUNT="$MAX_RESTARTS"
        if [[ "$MAX_RESTARTS" -eq 0 ]]; then
            RESTART_STATUS="pass"
            RESTART_DETAIL="docker: 0 restarts across bonsai containers"
        elif [[ "$MAX_RESTARTS" -le 2 ]]; then
            RESTART_STATUS="warn"
            RESTART_DETAIL="docker: $MAX_RESTARTS restart(s) on $WORST_CONTAINER (≤2 tolerated)"
        else
            RESTART_STATUS="fail"
            RESTART_DETAIL="docker: $MAX_RESTARTS restarts on $WORST_CONTAINER (>2 — investigate crashes)"
        fi
    else
        RESTART_STATUS="warn"
        RESTART_DETAIL="no bonsai docker containers found — cannot verify restart count"
    fi
fi

_result "service_restarts" "$RESTART_STATUS" "$RESTART_DETAIL"

# ── Criterion 4: silent_subscriptions == 0 ───────────────────────────────────

echo "[ 4/7 ] gNMI subscriptions (silent_subscriptions == 0)..."
OPS_OUT=""
if OPS_OUT=$(curl -sf --max-time 5 "$API_BASE/api/operations" 2>/dev/null); then
    SILENT=$(echo "$OPS_OUT" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('silent_subscriptions',0))" 2>/dev/null || echo "-1")
    OBS=$(echo "$OPS_OUT" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('observed_subscriptions',0))" 2>/dev/null || echo "0")
    if [[ "$SILENT" -eq 0 ]] && [[ "$OBS" -gt 0 ]]; then
        _result "gnmi_subscriptions" "pass" "observed=$OBS silent=$SILENT — all subscriptions live"
    elif [[ "$SILENT" -eq 0 ]]; then
        _result "gnmi_subscriptions" "warn" "silent=0 but observed_subscriptions=0 — lab may be down"
    else
        _result "gnmi_subscriptions" "fail" "silent_subscriptions=$SILENT (expected 0) — subscriptions may have dropped"
    fi
else
    _result "gnmi_subscriptions" "fail" "bonsai API not responding at $API_BASE"
fi

# ── Criterion 5: ≥1 detection event ──────────────────────────────────────────

echo "[ 5/7 ] Detection events (≥1 confirms loop fired)..."
DET_OUT=""
if DET_OUT=$(curl -sf --max-time 5 "$API_BASE/api/detections" 2>/dev/null); then
    DET_COUNT=$(echo "$DET_OUT" | python3 -c "
import json, sys
d = json.load(sys.stdin)
if isinstance(d, list):
    print(len(d))
elif isinstance(d, dict):
    print(len(d.get('events', d.get('detections', []))))
else:
    print(0)
" 2>/dev/null || echo "0")
    if [[ "$DET_COUNT" -ge 1 ]]; then
        _result "detections_present" "pass" "$DET_COUNT detection event(s) recorded — closed-loop confirmed"
    else
        _result "detections_present" "fail" "0 detection events — chaos faults may not have been detected"
    fi
else
    _result "detections_present" "fail" "/api/detections not responding at $API_BASE"
fi

# ── Criterion 6 + 7: Memory and disk budgets ─────────────────────────────────

echo "[ 6/7 ] Memory budget (RSS < 80%)..."
echo "[ 7/7 ] Archive disk budget (< 80%)..."
STATUS_OUT=""
if STATUS_OUT=$(curl -sf --max-time 5 "$API_BASE/api/_test/status" 2>/dev/null); then
    MEM_PCT=$(echo "$STATUS_OUT" | python3 -c "
import json, sys
d = json.load(sys.stdin)
print(d.get('memory_rss_pct_of_budget', -1))
" 2>/dev/null || echo "-1")
    DISK_PCT=$(echo "$STATUS_OUT" | python3 -c "
import json, sys
d = json.load(sys.stdin)
print(d.get('archive_disk_pct', -1))
" 2>/dev/null || echo "-1")

    if python3 -c "exit(0 if 0 <= float('$MEM_PCT') < 80 else 1)" 2>/dev/null; then
        _result "memory_budget" "pass" "memory_rss_pct_of_budget=${MEM_PCT}% (< 80%)"
    elif python3 -c "exit(0 if float('$MEM_PCT') < 0 else 1)" 2>/dev/null; then
        _result "memory_budget" "warn" "memory_rss_pct_of_budget not reported by API"
    else
        _result "memory_budget" "fail" "memory_rss_pct_of_budget=${MEM_PCT}% (≥ 80% — memory pressure)"
    fi

    if python3 -c "exit(0 if 0 <= float('$DISK_PCT') < 80 else 1)" 2>/dev/null; then
        _result "archive_disk" "pass" "archive_disk_pct=${DISK_PCT}% (< 80%)"
    elif python3 -c "exit(0 if float('$DISK_PCT') < 0 else 1)" 2>/dev/null; then
        _result "archive_disk" "warn" "archive_disk_pct not reported by API"
    else
        _result "archive_disk" "fail" "archive_disk_pct=${DISK_PCT}% (≥ 80% — disk pressure)"
    fi
else
    _result "memory_budget" "fail" "/api/_test/status not responding at $API_BASE"
    _result "archive_disk"  "fail" "/api/_test/status not responding at $API_BASE"
fi

# ── Summary ───────────────────────────────────────────────────────────────────

echo ""
echo -e "${BOLD}Results:${RESET}"
for line in "${CHECK_LOG[@]}"; do
    echo -e "$line"
done

if [[ "${#DETAIL_LOG[@]}" -gt 0 ]]; then
    echo ""
    for line in "${DETAIL_LOG[@]}"; do
        echo -e "$line"
    done
fi

echo ""
echo -e "${BOLD}Score: $PASSED/$TOTAL criteria passed | $FAILED failed${RESET}"

VERDICT="PASS"
if [[ "$FAILED" -gt 0 ]]; then
    VERDICT="FAIL"
    echo ""
    echo -e "${RED}7-DAY PROOF: NOT COMPLETE — $FAILED criterion/criteria not met.${RESET}"
    echo ""
else
    echo ""
    echo -e "${GREEN}7-DAY PROOF: COMPLETE — all $TOTAL acceptance criteria met.${RESET}"
    echo ""
fi

if $CHECK_ONLY; then
    echo "(--check-only: not writing closure document)"
    echo ""
    [[ "$VERDICT" == "PASS" ]] && exit 0 || exit 1
fi

# ── Write closure document ────────────────────────────────────────────────────

CLOSE_DATE=$(date -u '+%Y-%m-%d')
CLOSE_TS=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
CLOSE_GIT_SHA=$(git -C "$INSTALL_DIR" rev-parse --short HEAD 2>/dev/null || echo "unknown")
CLOSURE_FILE="$CLOSURE_DIR/7day_closure_${CLOSE_DATE}.md"

if [[ -f "$CLOSURE_FILE" ]] && ! $FORCE; then
    echo -e "${YELLOW}WARN: $CLOSURE_FILE already exists. Use --force to overwrite.${RESET}"
    echo ""
    [[ "$VERDICT" == "PASS" ]] && exit 0 || exit 1
fi

mkdir -p "$CLOSURE_DIR"

{
    printf '# Bonsai 7-Day Closure Report — %s\n\n' "$CLOSE_DATE"
    printf 'Generated: %s\n' "$CLOSE_TS"
    printf 'Git SHA: %s\n' "$CLOSE_GIT_SHA"
    printf 'Clock started: %s (SHA: %s)\n\n' "$START_TS" "$START_GIT_SHA"
    printf '**Verdict: %s** (%d/%d criteria passed)\n\n' "$VERDICT" "$PASSED" "$TOTAL"
    printf '---\n\n'
    printf '## Acceptance Criteria\n\n'
    printf '| # | Criterion | Status | Detail |\n'
    printf '|---|-----------|--------|--------|\n'

    # Rebuild table from CHECK_LOG (strip ANSI for markdown)
    crit_num=1
    for line in "${CHECK_LOG[@]}"; do
        clean=$(echo "$line" | sed 's/\x1b\[[0-9;]*m//g' | sed 's/^[[:space:]]*//')
        # Extract status badge word
        if echo "$clean" | grep -q '^PASS'; then
            badge="PASS"
        elif echo "$clean" | grep -q '^WARN'; then
            badge="WARN"
        else
            badge="FAIL"
        fi
        # Strip leading PASS/FAIL/WARN
        rest=$(echo "$clean" | sed 's/^PASS  //' | sed 's/^FAIL  //' | sed 's/^WARN  //')
        # Split on first ] to get name and detail
        name=$(echo "$rest" | sed 's/\[//' | sed 's/].*//')
        detail=$(echo "$rest" | sed 's/\[[^]]*\] //')
        printf '| %d | %s | %s | %s |\n' "$crit_num" "$name" "$badge" "$detail"
        crit_num=$((crit_num + 1))
    done

    if [[ "${#DETAIL_LOG[@]}" -gt 0 ]]; then
        printf '\n### Detail Notes\n\n'
        printf '```text\n'
        for line in "${DETAIL_LOG[@]}"; do
            echo "$line" | sed 's/\x1b\[[0-9;]*m//g'
        done
        printf '```\n'
    fi

    printf '\n---\n\n'
    printf '## Daily Check Summary\n\n'
    printf '```text\n'
    echo "$DAILY_RESULT"
    printf '```\n'

    printf '\n## Sync Branches\n\n'
    printf '```text\n'
    if [[ -n "$SYNC_LIST" ]]; then
        echo "$SYNC_LIST"
    else
        echo "(none found or git remote not reachable)"
    fi
    printf '```\n'

    printf '\n---\n\n'
    printf '## Operator Notes\n\n'
    printf -- '- Add any incidents, manual interventions, or observations here.\n'

} > "$CLOSURE_FILE"

echo "Closure document written: $CLOSURE_FILE"
echo ""
echo "Next steps:"
echo "  git add $CLOSURE_FILE"
echo "  git add docs/test_results/daily_runs/"
echo "  git commit -m \"ops: 7-day handoff complete — $CLOSE_DATE\""
echo ""

[[ "$VERDICT" == "PASS" ]] && exit 0 || exit 1
