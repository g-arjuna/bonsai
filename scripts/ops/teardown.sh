#!/usr/bin/env bash
# CV7 — Clean teardown for the laptop run.
#
# Stops the rules sidecar, then bonsai, removes PID files. By default does NOT
# touch containerlab or external infra containers — pass --full to also tear
# those down.
#
# Use on Ubuntu laptop only.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'

FULL=0
case "${1:-}" in
  --full)  FULL=1 ;;
  -h|--help)
    cat <<EOF
teardown.sh — stop bonsai + sidecar (CV7 T2-1 / validation framework)

  (no args)    Stop bonsai + sidecar only. Leaves containerlab + external infra running.
  --full       Additionally: containerlab destroy --cleanup --graceful + docker compose down.
EOF
    exit 0
    ;;
esac

ENV_DETECTED="$(bash scripts/dev/whichenv.sh 2>/dev/null || echo unknown)"
if [[ "$ENV_DETECTED" == "mac-dev" ]]; then
  echo "${RED}Refused.${RESET} This script is laptop-only; do NOT run from Mac." >&2
  exit 2
fi

stop_pidfile() {
  local label="$1" pidfile="$2"
  if [[ -f "$pidfile" ]]; then
    local pid; pid="$(cat "$pidfile" 2>/dev/null || true)"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      echo "${YELLOW}stopping $label${RESET} (pid $pid)"
      kill "$pid" 2>/dev/null || true
      # Wait up to 5s for clean exit.
      for _ in $(seq 1 5); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 1
      done
      if kill -0 "$pid" 2>/dev/null; then
        echo "${RED}$label didn't exit, sending SIGKILL${RESET}"
        kill -9 "$pid" 2>/dev/null || true
      fi
    else
      echo "  $label pidfile present but stale (pid $pid)"
    fi
    rm -f "$pidfile"
  else
    echo "  $label not running (no pidfile)"
  fi
}

# Stop sidecar first so it doesn't issue heartbeats during bonsai shutdown.
stop_pidfile "rules sidecar" "runtime/bonsai-sidecar.pid"
stop_pidfile "bonsai"        "runtime/bonsai.pid"

# Also catch orphaned processes (e.g. from a prior killed wrapper).
pkill -f 'python.*collector_engine.py' 2>/dev/null || true
pkill -f 'target/release/bonsai|/usr/local/bin/bonsai' 2>/dev/null || true

if (( FULL == 1 )); then
  echo
  echo "${YELLOW}--full mode${RESET}"
  if command -v containerlab >/dev/null 2>&1; then
    if [[ -f lab/dc/bonsai.clab.yml ]]; then
      echo "containerlab destroy --cleanup --graceful (dc)"
      containerlab destroy --cleanup --graceful -t lab/dc/bonsai.clab.yml 2>&1 | tail -5 || true
    fi
    if [[ -f lab/sp/bonsai.clab.yml ]]; then
      echo "containerlab destroy --cleanup --graceful (sp)"
      containerlab destroy --cleanup --graceful -t lab/sp/bonsai.clab.yml 2>&1 | tail -5 || true
    fi
  fi
  if command -v docker >/dev/null 2>&1 && [[ -f docker/compose.yml ]]; then
    echo "docker compose down (external infra)"
    docker compose -f docker/compose.yml down 2>&1 | tail -5 || true
  fi
fi

echo
echo "${GREEN}teardown complete${RESET}"
