#!/usr/bin/env bash
# CV7 T3-3 — Chaos stability smoke.
#
# Runs the chaos daemon for the configured WALL_SECS window. Pass = zero new
# restart_marker events in runtime/chaos_log.jsonl during the window.
#
# Default: 1 hour. Override with WALL_SECS env (e.g. WALL_SECS=300 for a quick
# regression).
#
# Laptop only (cloud has its own systemd-managed instance).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'

ENV_DETECTED="$(bash scripts/dev/whichenv.sh 2>/dev/null || echo unknown)"
if [[ "$ENV_DETECTED" == "mac-dev" ]]; then
  echo "${RED}Refused.${RESET} Laptop only." >&2
  exit 2
fi

WALL_SECS="${WALL_SECS:-3600}"
CHAOS_LOG="$REPO_ROOT/runtime/chaos_log.jsonl"

if [[ ! -f "$CHAOS_LOG" ]]; then
  # First run — touch it so the count-before-start is 0.
  mkdir -p "$REPO_ROOT/runtime"
  : > "$CHAOS_LOG"
fi

before=$(grep -c '"event_type":"restart_marker"' "$CHAOS_LOG" 2>/dev/null || echo 0)
echo "Pre-smoke restart_marker count: $before"

# Ensure the daemon is running; start it if not.
bash scripts/chaos_runner.sh --ensure-running || true
echo "Daemon status:"
bash scripts/chaos_runner.sh --status | head -5

echo
echo "${YELLOW}Running for ${WALL_SECS}s${RESET}…"
END_TS=$(( $(date +%s) + WALL_SECS ))
while (( $(date +%s) < END_TS )); do
  remaining=$(( END_TS - $(date +%s) ))
  printf '\r  %4ds remaining   ' "$remaining"
  sleep 10
done
echo
echo

after=$(grep -c '"event_type":"restart_marker"' "$CHAOS_LOG" 2>/dev/null || echo 0)
delta=$(( after - before ))

echo "Post-smoke restart_marker count: $after"
echo "Delta over window: $delta"

if (( delta == 0 )); then
  echo "${GREEN}PASS${RESET} — zero new restart_marker events in $WALL_SECS s."
  exit 0
else
  echo "${RED}FAIL${RESET} — $delta new restart_marker(s) in the window."
  echo "Recent markers:"
  grep '"event_type":"restart_marker"' "$CHAOS_LOG" | tail -10
  exit 1
fi
