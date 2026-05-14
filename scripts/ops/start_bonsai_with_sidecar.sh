#!/usr/bin/env bash
# CV7 T2-1 — Laptop startup wrapper.
#
# Starts bonsai + the rules sidecar as two foreground/background processes
# with linked lifecycle. Use on the Ubuntu laptop ONLY (per dev/ops boundary).
# Cloud uses systemd units in deploy/systemd/.
#
# Usage:
#   bash scripts/ops/start_bonsai_with_sidecar.sh [--foreground|-f]
#
# Default: starts both in the background, prints PIDs, returns immediately.
# --foreground keeps the script alive; Ctrl-C cleanly tears down both.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; BOLD=$'\033[1m'; RESET=$'\033[0m'

# ── Environment guard ─────────────────────────────────────────────────────────
ENV_DETECTED="$(bash scripts/dev/whichenv.sh 2>/dev/null || echo unknown)"
if [[ "$ENV_DETECTED" == "mac-dev" ]]; then
  echo "${RED}Refused.${RESET} This script is laptop-only (ubuntu-ops); do NOT run from Mac." >&2
  echo "See: docs/operations/dev_vs_ops_boundary.md" >&2
  exit 2
fi

# ── Args ──────────────────────────────────────────────────────────────────────
FOREGROUND=0
case "${1:-}" in
  -f|--foreground) FOREGROUND=1 ;;
  ""|--background) FOREGROUND=0 ;;
  -h|--help)
    cat <<EOF
start_bonsai_with_sidecar.sh — laptop startup wrapper (CV7 T2-1)

  --foreground, -f     Keep wrapper alive; Ctrl-C tears down both processes.
  --background         (default) Start both in background, return immediately.
EOF
    exit 0
    ;;
  *)
    echo "Unknown arg: $1" >&2
    exit 1
    ;;
esac

# ── Sanity ────────────────────────────────────────────────────────────────────
mkdir -p runtime logs
BONSAI_LOG="logs/bonsai.log"
SIDECAR_LOG="logs/bonsai-sidecar.log"
BONSAI_PID_FILE="runtime/bonsai.pid"
SIDECAR_PID_FILE="runtime/bonsai-sidecar.pid"

# Refuse to start if already running.
if [[ -f "$BONSAI_PID_FILE" ]] && kill -0 "$(cat "$BONSAI_PID_FILE" 2>/dev/null)" 2>/dev/null; then
  echo "${RED}bonsai already running${RESET} (pid $(cat "$BONSAI_PID_FILE")). Run scripts/ops/teardown.sh first." >&2
  exit 1
fi

# Decide how to invoke bonsai. Prefer the installed binary; fall back to cargo
# run for the interim phase (pre Tier 6 CI/CD).
if [[ -x /usr/local/bin/bonsai ]]; then
  BONSAI_CMD=(/usr/local/bin/bonsai)
  echo "${GREEN}using installed bonsai binary${RESET}"
elif [[ -x "$REPO_ROOT/target/release/bonsai" ]]; then
  BONSAI_CMD=("$REPO_ROOT/target/release/bonsai")
  echo "${GREEN}using release-built target/release/bonsai${RESET}"
elif command -v cargo >/dev/null 2>&1; then
  BONSAI_CMD=(cargo run --release --quiet)
  echo "${YELLOW}cargo run --release fallback (slow first start)${RESET}"
else
  echo "${RED}no bonsai binary and no cargo on PATH${RESET}" >&2
  exit 1
fi

# CV7 T4-6: require the rules sidecar — /health will report degraded until it registers.
export BONSAI_REQUIRE_SIDECAR="${BONSAI_REQUIRE_SIDECAR:-rules}"

echo "${BOLD}Starting bonsai${RESET}"
echo "  log:     $BONSAI_LOG"
echo "  pidfile: $BONSAI_PID_FILE"
echo "  require: $BONSAI_REQUIRE_SIDECAR"

"${BONSAI_CMD[@]}" >> "$BONSAI_LOG" 2>&1 &
BONSAI_PID=$!
echo "$BONSAI_PID" > "$BONSAI_PID_FILE"
echo "  pid:     $BONSAI_PID"

# ── Wait for bonsai HTTP to be reachable ──────────────────────────────────────
# IMPORTANT: with BONSAI_REQUIRE_SIDECAR=rules, bonsai's /health intentionally
# returns 503 "degraded" until the sidecar registers. We must NOT treat 503 as
# failure here — the wrapper is responsible for starting the sidecar next, and
# the sidecar's registration is what flips /health to 200. Earlier versions
# used `curl -fsS` which rejected 503 → wrapper exited without ever starting
# the sidecar (classic deadlock; root-caused 2026-05-14T1541Z).
#
# We accept any response code from /health (i.e. bonsai's HTTP server bound
# the port). We just need to know "the server is listening". Anything from
# 200/503 to 404 means the listener is up.
wait_for_http() {
  local url="$1" max_secs=60 elapsed=0 code
  while (( elapsed < max_secs )); do
    code="$(curl -sS -o /dev/null -w '%{http_code}' "$url" 2>/dev/null || echo 000)"
    case "$code" in
      200|2[0-9][0-9]|5[0-9][0-9])
        echo "$code"
        return 0
        ;;
    esac
    if ! kill -0 "$BONSAI_PID" 2>/dev/null; then
      echo "${RED}bonsai died during startup${RESET} (see $BONSAI_LOG)" >&2
      return 1
    fi
    sleep 1
    elapsed=$((elapsed+1))
  done
  return 1
}

echo "Waiting for bonsai /health to respond on :3000…"
if HEALTH_CODE="$(wait_for_http "http://127.0.0.1:3000/health")"; then
  echo "${GREEN}bonsai HTTP up${RESET} (/health → $HEALTH_CODE; 503 is expected before sidecar registers)"
else
  echo "${RED}bonsai did not bind :3000 within 60s${RESET}" >&2
  echo "tail of log:" >&2
  tail -50 "$BONSAI_LOG" >&2 || true
  exit 1
fi

# ── Start the rules sidecar ───────────────────────────────────────────────────
echo "${BOLD}Starting rules sidecar${RESET}"
echo "  log:     $SIDECAR_LOG"
echo "  pidfile: $SIDECAR_PID_FILE"

# The Python sidecar talks to bonsai's local gRPC port (default 50051 — mode=all)
# and registers itself via RegisterSidecar (T4-3).
export BONSAI_LOCAL_ADDR="${BONSAI_LOCAL_ADDR:-localhost:50051}"
export BONSAI_CORE_ADDR="${BONSAI_CORE_ADDR:-$BONSAI_LOCAL_ADDR}"
export BONSAI_COLLECTOR_ID="${BONSAI_COLLECTOR_ID:-rules-local}"

# Pick python from the repo venv if present, else system python.
if [[ -x "$REPO_ROOT/.venv/bin/python" ]]; then
  PY="$REPO_ROOT/.venv/bin/python"
elif command -v python3 >/dev/null 2>&1; then
  PY=python3
else
  echo "${RED}no python found${RESET}" >&2
  kill "$BONSAI_PID" 2>/dev/null || true
  exit 1
fi

"$PY" python/collector_engine.py >> "$SIDECAR_LOG" 2>&1 &
SIDECAR_PID=$!
echo "$SIDECAR_PID" > "$SIDECAR_PID_FILE"
echo "  pid:     $SIDECAR_PID"

# ── Termination handler (only matters if --foreground) ────────────────────────
cleanup() {
  echo
  echo "${YELLOW}tearing down…${RESET}"
  [[ -n "${SIDECAR_PID:-}" ]] && kill "$SIDECAR_PID" 2>/dev/null || true
  [[ -n "${BONSAI_PID:-}"  ]] && kill "$BONSAI_PID"  2>/dev/null || true
  rm -f "$SIDECAR_PID_FILE" "$BONSAI_PID_FILE"
}

if (( FOREGROUND == 1 )); then
  trap cleanup INT TERM EXIT
  echo
  echo "${GREEN}both running. Ctrl-C to stop.${RESET}"
  echo "  bonsai  → http://localhost:3000/        (bonsai UI)"
  echo "  bonpy   → http://localhost:3000/bonpy/  (sidecar status)"
  echo "  sidecar → /api/sidecars"
  echo "  health  → /health"
  wait
else
  echo
  echo "${GREEN}both started in background.${RESET}"
  echo "  tail -f $BONSAI_LOG"
  echo "  tail -f $SIDECAR_LOG"
  echo "  scripts/ops/teardown.sh   # to stop"
fi
