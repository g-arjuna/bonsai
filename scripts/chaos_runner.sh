#!/usr/bin/env bash
# scripts/chaos_runner.sh — Always-on DC chaos harness.
#
# Runs chaos_runner.py in 30-minute cycles indefinitely, accumulating GNN
# training data. Restarts automatically on crash. Writes to runtime/.
#
# Usage:
#   bash scripts/chaos_runner.sh              # background daemon (detaches)
#   bash scripts/chaos_runner.sh --fg         # foreground; Ctrl-C stops cleanly
#   bash scripts/chaos_runner.sh --stop       # kill the running daemon
#   bash scripts/chaos_runner.sh --status     # print daemon status + recent log
#   bash scripts/chaos_runner.sh --dry-run    # one dry-run cycle, then exit
#
# Requires: WSL with clab on PATH, .venv at repo root.
# Chaos plan: chaos_plans/always_on_dc.yaml

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNTIME_DIR="$REPO_ROOT/runtime"
LOG_FILE="$RUNTIME_DIR/chaos_runner.log"
PID_FILE="$RUNTIME_DIR/chaos_runner.pid"
PLAN="$REPO_ROOT/chaos_plans/always_on_dc.yaml"
PYTHON="$REPO_ROOT/.venv/bin/python3"
RUNNER="$REPO_ROOT/scripts/chaos_runner.py"
CYCLE_PAUSE_SECS=30   # gap between consecutive 30-min cycles

mkdir -p "$RUNTIME_DIR"

_log() { echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] $*" | tee -a "$LOG_FILE"; }
_die() { _log "ERROR: $*"; exit 1; }

# ── --stop ────────────────────────────────────────────────────────────────────
if [[ "${1:-}" == "--stop" ]]; then
    if [[ -f "$PID_FILE" ]]; then
        PID=$(<"$PID_FILE")
        if kill -0 "$PID" 2>/dev/null; then
            _log "Sending SIGTERM to chaos_runner daemon (PID $PID)"
            kill "$PID"
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
        PID=$(<"$PID_FILE")
        if kill -0 "$PID" 2>/dev/null; then
            echo "chaos_runner daemon is RUNNING (PID $PID)"
        else
            echo "chaos_runner daemon is STOPPED (stale PID $PID)"
        fi
    else
        echo "chaos_runner daemon is NOT RUNNING (no pid file)"
    fi
    echo ""
    echo "=== Last 20 log lines ==="
    [[ -f "$LOG_FILE" ]] && tail -20 "$LOG_FILE" || echo "(no log yet)"
    exit 0
fi

# ── Preflight checks ──────────────────────────────────────────────────────────
[[ -f "$PYTHON" ]] || _die ".venv not found at $REPO_ROOT/.venv — activate or create it first"
[[ -f "$PLAN" ]]   || _die "Chaos plan not found: $PLAN"
[[ -f "$RUNNER" ]] || _die "chaos_runner.py not found: $RUNNER"

if ! command -v clab &>/dev/null; then
    _log "WARNING: clab not on PATH — netem faults will be skipped"
fi

DRY_RUN_FLAG=""
if [[ "${1:-}" == "--dry-run" ]]; then
    DRY_RUN_FLAG="--dry-run"
    _log "Dry-run mode — one cycle, no actual fault injection"
fi

# ── Main loop (foreground worker) ─────────────────────────────────────────────
_run_loop() {
    _log "=== Chaos runner started (PID $$) ==="
    _log "Plan: $PLAN"
    _log "Python: $PYTHON"
    _log "Log: $LOG_FILE"

    CYCLE=0
    while true; do
        CYCLE=$((CYCLE + 1))
        _log "--- Cycle $CYCLE start ---"

        # Run one 30-min plan cycle; never let a crash kill the daemon
        if "$PYTHON" "$RUNNER" "$PLAN" $DRY_RUN_FLAG 2>&1 | tee -a "$LOG_FILE"; then
            _log "--- Cycle $CYCLE finished cleanly ---"
        else
            EXIT_CODE=$?
            _log "--- Cycle $CYCLE exited with code $EXIT_CODE — restarting after ${CYCLE_PAUSE_SECS}s ---"
            sleep "$CYCLE_PAUSE_SECS"
        fi

        # Exit after one cycle in dry-run mode
        [[ -n "$DRY_RUN_FLAG" ]] && { _log "Dry-run complete. Exiting."; exit 0; }

        _log "Pausing ${CYCLE_PAUSE_SECS}s before next cycle..."
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
    EXISTING_PID=$(<"$PID_FILE")
    if kill -0 "$EXISTING_PID" 2>/dev/null; then
        echo "chaos_runner daemon is already running (PID $EXISTING_PID)"
        echo "Use --stop to stop it, or --status to check."
        exit 1
    else
        _log "Stale pid file found ($EXISTING_PID) — removing"
        rm -f "$PID_FILE"
    fi
fi

# Fork into background, keeping log appended
nohup bash "$0" --fg >>"$LOG_FILE" 2>&1 &
DAEMON_PID=$!
echo "$DAEMON_PID" > "$PID_FILE"
_log "Daemon started in background (PID $DAEMON_PID)"
echo "chaos_runner daemon started (PID $DAEMON_PID)"
echo "Log: $LOG_FILE"
echo "Stop with: bash scripts/chaos_runner.sh --stop"
