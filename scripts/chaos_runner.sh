#!/usr/bin/env bash
# scripts/chaos_runner.sh — Laptop chaos daemon. CV7 T2-3 simplified.
#
# Cloud uses systemd directly: `systemctl start bonsai-chaos.service`. This
# script is for the Ubuntu laptop ONLY. The dual-mode systemd-scope detection
# from CV6 is gone — one tool per environment per the CV7 guardrails.
#
# CV7 T3-2 hardening:
#   • flock-based mutual exclusion (prevents the cron-race "stale pid file"
#     restart loop observed on 2026-05-13)
#   • main loop wraps the Python invocation so a non-zero exit doesn't kill
#     the daemon — it logs, waits, retries
#   • restart markers still emitted to runtime/chaos_log.jsonl for triage
#
# Usage:
#   bash scripts/chaos_runner.sh                # background daemon
#   bash scripts/chaos_runner.sh --fg           # foreground (Ctrl-C clean exit)
#   bash scripts/chaos_runner.sh --stop         # stop the running daemon
#   bash scripts/chaos_runner.sh --status       # status + last 20 log lines
#   bash scripts/chaos_runner.sh --ensure-running   # idempotent (cron-safe)
#   bash scripts/chaos_runner.sh --dry-run      # one cycle, no real injection
#
# Requires: .venv at repo root, clab on PATH, chaos plan YAML.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNTIME_DIR="$REPO_ROOT/runtime"
LOG_FILE="$RUNTIME_DIR/chaos_runner.log"
PID_FILE="$RUNTIME_DIR/chaos_runner.pid"
LOCK_FILE="$RUNTIME_DIR/chaos_runner.lock"
CHAOS_LOG_JSONL="$RUNTIME_DIR/chaos_log.jsonl"
PLAN="${PLAN:-$REPO_ROOT/chaos_plans/always_on_dc.yaml}"
PYTHON="$REPO_ROOT/.venv/bin/python3"
RUNNER="$REPO_ROOT/scripts/chaos_runner.py"
CYCLE_PAUSE_SECS=30

mkdir -p "$RUNTIME_DIR"

# Default to ContainerLab docker transport on laptop unless operator pins SSH.
if [[ -z "${BONSAI_FAULT_TRANSPORT:-}" ]] && command -v docker &>/dev/null; then
  export BONSAI_FAULT_TRANSPORT="docker"
fi

# ── Helpers ───────────────────────────────────────────────────────────────────

_log() { echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] $*" | tee -a "$LOG_FILE"; }
_die() { _log "ERROR: $*"; exit 1; }

_json_escape() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }

_write_restart_marker() {
  local reason="$1" old_pid="${2:-}" ts
  ts="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  printf '{"event_type":"restart_marker","ts":"%s","reason":"%s","old_pid":"%s","plan":"%s"}\n' \
    "$ts" "$(_json_escape "$reason")" "$(_json_escape "$old_pid")" "$(_json_escape "$PLAN")" \
    >> "$CHAOS_LOG_JSONL"
}

_preflight() {
  [[ -f "$PYTHON" ]] || _die ".venv not found at $REPO_ROOT/.venv — activate or create it first"
  [[ -f "$PLAN" ]]   || _die "Chaos plan not found: $PLAN"
  [[ -f "$RUNNER" ]] || _die "chaos_runner.py not found: $RUNNER"
  if ! command -v clab &>/dev/null; then
    _log "WARNING: clab not on PATH — netem faults will be skipped"
  fi
}

# Refuse on Mac per dev/ops boundary.
ENV_DETECTED="$(bash "$REPO_ROOT/scripts/dev/whichenv.sh" 2>/dev/null || echo unknown)"
if [[ "$ENV_DETECTED" == "mac-dev" ]]; then
  echo "Refused: chaos_runner.sh is laptop-only. See docs/operations/dev_vs_ops_boundary.md" >&2
  exit 2
fi

# ── --stop ────────────────────────────────────────────────────────────────────
if [[ "${1:-}" == "--stop" ]]; then
  if [[ -f "$PID_FILE" ]]; then
    PID="$(<"$PID_FILE")"
    if kill -0 "$PID" 2>/dev/null; then
      _log "Sending SIGTERM to chaos_runner daemon (PID $PID)"
      kill "$PID"
      pkill -P "$PID" 2>/dev/null || true
    else
      _log "PID $PID not running — cleaning stale pid file"
    fi
    rm -f "$PID_FILE"
  else
    echo "No pid file found at $PID_FILE — daemon may not be running"
  fi
  exit 0
fi

# ── --status ──────────────────────────────────────────────────────────────────
if [[ "${1:-}" == "--status" ]]; then
  if [[ -f "$PID_FILE" ]]; then
    PID="$(<"$PID_FILE")"
    if kill -0 "$PID" 2>/dev/null; then
      echo "chaos_runner daemon is RUNNING (PID $PID)"
    else
      echo "chaos_runner daemon is STOPPED (stale PID $PID)"
    fi
  else
    echo "chaos_runner daemon is NOT RUNNING (no pid file)"
  fi
  echo
  echo "=== Last 20 log lines ==="
  [[ -f "$LOG_FILE" ]] && tail -20 "$LOG_FILE" || echo "(no log yet)"
  exit 0
fi

# ── --ensure-running (cron-safe via flock) ────────────────────────────────────
if [[ "${1:-}" == "--ensure-running" ]]; then
  # flock prevents two cron invocations from both deciding to restart the daemon.
  exec 9>"$LOCK_FILE"
  if ! flock -n 9; then
    _log "ensure-running: another invocation holds the lock — exiting cleanly"
    exit 0
  fi
  if [[ -f "$PID_FILE" ]]; then
    EXISTING_PID="$(<"$PID_FILE")"
    if kill -0 "$EXISTING_PID" 2>/dev/null; then
      _log "ensure-running: daemon already running (PID $EXISTING_PID)"
      exit 0
    fi
    _log "ensure-running: stale pid file found ($EXISTING_PID) — restarting"
    _write_restart_marker "stale_pid" "$EXISTING_PID"
    rm -f "$PID_FILE"
  else
    _log "ensure-running: no pid file found — starting daemon"
    _write_restart_marker "missing_pid_file" ""
  fi
  _preflight
  nohup bash "$0" --fg >/dev/null 2>&1 &
  DAEMON_PID=$!
  echo "$DAEMON_PID" > "$PID_FILE"
  _log "ensure-running: daemon started in background (PID $DAEMON_PID)"
  exit 0
fi

# ── Main loop (foreground worker) ─────────────────────────────────────────────
_preflight

DRY_RUN_FLAG=""
if [[ "${1:-}" == "--dry-run" ]]; then
  DRY_RUN_FLAG="--dry-run"
  _log "Dry-run mode — one cycle, no actual fault injection"
fi

_run_loop() {
  cd "$REPO_ROOT"
  _log "=== Chaos runner started (PID $$) ==="
  _log "Plan: $PLAN  Python: $PYTHON  Transport: ${BONSAI_FAULT_TRANSPORT:-auto}"

  CYCLE=0
  while true; do
    CYCLE=$((CYCLE + 1))
    _log "--- Cycle $CYCLE start ---"

    # CV7 T3-2: NEVER let a Python exit kill the daemon. Capture exit code,
    # log it, write a restart marker if non-zero, sleep and retry.
    set +e
    "$PYTHON" "$RUNNER" "$PLAN" $DRY_RUN_FLAG 2>&1 | tee -a "$LOG_FILE"
    EXIT_CODE=${PIPESTATUS[0]}
    set -e

    if (( EXIT_CODE == 0 )); then
      _log "--- Cycle $CYCLE finished cleanly ---"
    else
      _log "--- Cycle $CYCLE exited with code $EXIT_CODE — recovering in ${CYCLE_PAUSE_SECS}s ---"
      _write_restart_marker "python_exit_${EXIT_CODE}" "$$"
    fi

    [[ -n "$DRY_RUN_FLAG" ]] && { _log "Dry-run complete. Exiting."; exit 0; }
    sleep "$CYCLE_PAUSE_SECS"
  done
}

# ── --fg: run in foreground ────────────────────────────────────────────────────
if [[ "${1:-}" == "--fg" || -n "$DRY_RUN_FLAG" ]]; then
  _run_loop
  exit 0
fi

# ── Background daemon mode (default) ─────────────────────────────────────────
if [[ -f "$PID_FILE" ]]; then
  EXISTING_PID="$(<"$PID_FILE")"
  if kill -0 "$EXISTING_PID" 2>/dev/null; then
    echo "chaos_runner daemon is already running (PID $EXISTING_PID)"
    echo "Use --stop to stop it, or --status to check."
    exit 1
  else
    _log "Stale pid file found ($EXISTING_PID) — removing"
    rm -f "$PID_FILE"
  fi
fi

nohup bash "$0" --fg >/dev/null 2>&1 &
DAEMON_PID=$!
echo "$DAEMON_PID" > "$PID_FILE"
_log "Daemon started in background (PID $DAEMON_PID)"
echo "chaos_runner daemon started (PID $DAEMON_PID)"
echo "Log: $LOG_FILE"
echo "Stop with: bash scripts/chaos_runner.sh --stop"
