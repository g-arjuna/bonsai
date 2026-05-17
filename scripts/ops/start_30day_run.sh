#!/usr/bin/env bash
# scripts/ops/start_30day_run.sh — Single unified startup for the 30-day GNN data run.
#
# USE THIS SCRIPT on BOTH the Ubuntu laptop and the cloud VM.
# Bonsai runs as a NATIVE PROCESS only. Docker / docker compose are NEVER used
# for bonsai itself (only ContainerLab uses docker for the SRL nodes).
#
# What this script does:
#   1. Kills any running bonsai / sidecar / chaos processes
#   2. Removes stale docker bonsai containers (prevents port 3000 conflicts)
#   3. Verifies the bonsai binary exists (target/release/bonsai)
#   4. Verifies bonsai.toml exists with required archive settings
#   5. Starts bonsai (native binary, background)
#   6. Waits for /health to respond
#   7. Starts the rules sidecar (python/collector_engine.py, background)
#   8. Waits for sidecar to register
#   9. Starts the chaos daemon (background, 30-day duration)
#  10. Prints a status summary
#
# Prerequisites:
#   - cargo build --release is current (run rebuild_and_validate.sh first)
#   - ContainerLab topology is up (bash scripts/lab/redeploy_dc.sh or redeploy_cloud_dc.sh)
#   - bonsai.toml exists with [archive] and [retention] configured
#   - Python venv is set up (.venv/bin/python exists)
#
# Usage:
#   bash scripts/ops/start_30day_run.sh [--chaos-plan <plan.yaml>] [--no-chaos]
#
#   --chaos-plan <file>   Chaos plan to use (default: chaos_plans/always_on_dc.yaml for
#                         Ubuntu laptop; chaos_plans/always_on_cloud_dc.yaml for cloud VM)
#   --no-chaos            Start bonsai + sidecar only; skip the chaos daemon
#   --restart-chaos       Kill and restart the chaos daemon only (bonsai + sidecar untouched)
#   --status              Show current process status and exit

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; BOLD=$'\033[1m'; RESET=$'\033[0m'

# ── Constants ─────────────────────────────────────────────────────────────────

BONSAI_LOG="logs/bonsai.log"
SIDECAR_LOG="logs/bonsai-sidecar.log"
CHAOS_LOG="logs/chaos-30day.log"
BONSAI_PID_FILE="runtime/bonsai.pid"
SIDECAR_PID_FILE="runtime/bonsai-sidecar.pid"
CHAOS_PID_FILE="runtime/chaos-30day.pid"
THIRTY_DAYS_SECS=$((30 * 24 * 3600))

# ── Arg parsing ───────────────────────────────────────────────────────────────

NO_CHAOS=0
RESTART_CHAOS_ONLY=0
STATUS_ONLY=0
CHAOS_PLAN=""

for arg in "$@"; do
  case "$arg" in
    --no-chaos)           NO_CHAOS=1 ;;
    --restart-chaos)      RESTART_CHAOS_ONLY=1 ;;
    --status)             STATUS_ONLY=1 ;;
    --chaos-plan)         : ;;  # handled below as pair
    *)
      if [[ "${PREV_ARG:-}" == "--chaos-plan" ]]; then
        CHAOS_PLAN="$arg"
      fi
      ;;
  esac
  PREV_ARG="$arg"
done

# ── Status-only mode ──────────────────────────────────────────────────────────

if (( STATUS_ONLY == 1 )); then
  echo "${BOLD}=== Bonsai 30-day run status ===${RESET}"
  echo ""

  check_pid() {
    local label="$1" pf="$2"
    if [[ -f "$pf" ]]; then
      local pid; pid="$(cat "$pf" 2>/dev/null || true)"
      if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        echo "  ${GREEN}RUNNING${RESET}  $label  (pid $pid)"
      else
        echo "  ${RED}DEAD${RESET}     $label  (stale pidfile: $pf)"
      fi
    else
      echo "  ${YELLOW}STOPPED${RESET}  $label  (no pidfile)"
    fi
  }

  check_pid "bonsai" "$BONSAI_PID_FILE"
  check_pid "sidecar" "$SIDECAR_PID_FILE"
  check_pid "chaos  " "$CHAOS_PID_FILE"

  echo ""
  echo "  /health:"
  curl -sf --max-time 3 "http://127.0.0.1:3000/health" 2>/dev/null || echo "  (no response)"
  echo ""
  echo "  archive files:"
  find runtime/archive -name "*.parquet" 2>/dev/null | wc -l | xargs -I{} echo "    {} parquet files"
  echo "  archive size: $(du -sh runtime/archive 2>/dev/null | cut -f1 || echo '0')"
  echo ""
  echo "  sidecar stats:"
  curl -sf --max-time 3 "http://127.0.0.1:3000/api/sidecars" 2>/dev/null | \
    python3 -c "
import json,sys
sc=json.load(sys.stdin).get('sidecars',[])
for s in sc:
  print(f'    {s[\"name\"]}: events_in={s[\"events_in_total\"]} dets_out={s[\"detections_out_total\"]} status={s[\"status\"]}')
" 2>/dev/null || echo "    (api not reachable)"
  echo ""
  exit 0
fi

# ── Helpers ───────────────────────────────────────────────────────────────────

log()  { echo "${GREEN}[start_30day]${RESET} $*"; }
warn() { echo "${YELLOW}[start_30day] WARN:${RESET} $*"; }
die()  { echo "${RED}[start_30day] ERROR:${RESET} $*" >&2; exit 1; }

mkdir -p runtime logs

stop_pid() {
  local label="$1" pf="$2"
  if [[ -f "$pf" ]]; then
    local pid; pid="$(cat "$pf" 2>/dev/null || true)"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      log "stopping $label (pid $pid)"
      kill "$pid" 2>/dev/null || true
      for _ in $(seq 1 8); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
      kill -9 "$pid" 2>/dev/null || true
    fi
    rm -f "$pf"
  fi
}

# ── Restart-chaos-only mode ───────────────────────────────────────────────────

if (( RESTART_CHAOS_ONLY == 1 )); then
  stop_pid "chaos" "$CHAOS_PID_FILE"
  pkill -f 'chaos_harness/run.py' 2>/dev/null || true
  bash scripts/chaos_runner.sh --stop >/dev/null 2>&1 || true
  # fall through to chaos startup below
fi

# ── Kill everything first (unless restart-chaos-only) ────────────────────────

if (( RESTART_CHAOS_ONLY == 0 )); then
  log "stopping any existing bonsai / sidecar / chaos processes..."
  stop_pid "sidecar" "$SIDECAR_PID_FILE"
  stop_pid "bonsai"  "$BONSAI_PID_FILE"
  stop_pid "chaos"   "$CHAOS_PID_FILE"

  # Belt-and-suspenders: kill any orphaned processes by name
  pkill -f 'python.*collector_engine.py' 2>/dev/null || true
  pkill -f 'target/release/bonsai'       2>/dev/null || true
  pkill -f 'chaos_harness/run.py'        2>/dev/null || true
  bash scripts/chaos_runner.sh --stop >/dev/null 2>&1 || true

  # ── Remove stale docker bonsai containers (frees port 3000) ─────────────────
  # These containers are left over from old docker compose workflows.
  # They bind port 3000 and block native bonsai from starting.
  if command -v docker >/dev/null 2>&1; then
    for c in bonsai-bonsai-lab-dc-1 bonsai-bonsai-cloud-dc-1 bonsai-bonsai-lab-sp-1 bonsai-bonsai-dev-1; do
      if docker ps -a --format '{{.Names}}' 2>/dev/null | grep -qx "$c"; then
        warn "removing docker bonsai container $c (would conflict on port 3000)"
        docker rm -f "$c" >/dev/null 2>&1 || true
      fi
    done
  fi

  # Give the OS a moment to release the port.
  sleep 1

  # ── Pre-flight checks ────────────────────────────────────────────────────────

  if [[ ! -x "$REPO_ROOT/target/release/bonsai" ]]; then
    die "target/release/bonsai not found. Run: cargo build --release"
  fi

  if [[ ! -f "$REPO_ROOT/bonsai.toml" ]]; then
    die "bonsai.toml not found. Copy bonsai.toml.example and configure [archive] and [retention]."
  fi

  # Warn if archive is not enabled in bonsai.toml
  if ! grep -q '^enabled\s*=\s*true' "$REPO_ROOT/bonsai.toml" 2>/dev/null; then
    warn "bonsai.toml may not have [archive] enabled=true — check before a 30-day run"
  fi

  # Pick python
  if [[ -x "$REPO_ROOT/.venv/bin/python" ]]; then
    PY="$REPO_ROOT/.venv/bin/python"
  elif command -v python3 >/dev/null 2>&1; then
    PY=python3
  else
    die "no python found — run: python3 -m venv .venv && .venv/bin/pip install -e python/"
  fi

  # ── Start bonsai ─────────────────────────────────────────────────────────────

  log "starting bonsai (native binary)..."
  export BONSAI_REQUIRE_SIDECAR="${BONSAI_REQUIRE_SIDECAR:-rules}"

  "$REPO_ROOT/target/release/bonsai" >> "$BONSAI_LOG" 2>&1 &
  BONSAI_PID=$!
  echo "$BONSAI_PID" > "$BONSAI_PID_FILE"
  log "bonsai pid=$BONSAI_PID  log=$BONSAI_LOG"

  # Wait for HTTP listener (accept any 2xx/5xx — 503 is normal before sidecar registers)
  log "waiting for bonsai HTTP on :3000 (up to 90s)..."
  for i in $(seq 1 90); do
    code="$(curl -sS -o /dev/null -w '%{http_code}' "http://127.0.0.1:3000/health" 2>/dev/null || echo 000)"
    case "$code" in
      2??|5??) log "bonsai HTTP up (code=$code)"; break ;;
    esac
    if ! kill -0 "$BONSAI_PID" 2>/dev/null; then
      die "bonsai died during startup — see $BONSAI_LOG"
    fi
    sleep 1
  done
  # Final check
  code="$(curl -sS -o /dev/null -w '%{http_code}' "http://127.0.0.1:3000/health" 2>/dev/null || echo 000)"
  case "$code" in
    2??|5??) : ;;
    *) die "bonsai did not respond on :3000 within 90s — see $BONSAI_LOG" ;;
  esac

  # ── Start sidecar ────────────────────────────────────────────────────────────

  log "starting rules sidecar..."
  export BONSAI_LOCAL_ADDR="${BONSAI_LOCAL_ADDR:-localhost:50051}"
  export BONSAI_CORE_ADDR="${BONSAI_CORE_ADDR:-$BONSAI_LOCAL_ADDR}"
  export BONSAI_COLLECTOR_ID="${BONSAI_COLLECTOR_ID:-rules-local}"

  PYTHONUNBUFFERED=1 "$PY" python/collector_engine.py >> "$SIDECAR_LOG" 2>&1 &
  SIDECAR_PID=$!
  echo "$SIDECAR_PID" > "$SIDECAR_PID_FILE"
  log "sidecar pid=$SIDECAR_PID  log=$SIDECAR_LOG"

  # Wait for sidecar to register (up to 30s)
  log "waiting for sidecar to register (up to 30s)..."
  SIDECAR_REGISTERED=0
  for i in $(seq 1 30); do
    count="$(curl -sf --max-time 3 "http://127.0.0.1:3000/api/sidecars" 2>/dev/null | \
      python3 -c "import json,sys; print(len(json.load(sys.stdin).get('sidecars',[])))" 2>/dev/null || echo 0)"
    if [[ "$count" -ge 1 ]]; then
      SIDECAR_REGISTERED=1; break
    fi
    sleep 1
  done

  if (( SIDECAR_REGISTERED == 1 )); then
    log "/health = $(curl -sf --max-time 3 "http://127.0.0.1:3000/health" 2>/dev/null || echo '?')"
  else
    warn "sidecar did not register within 30s — check $SIDECAR_LOG"
  fi
fi  # end of restart-chaos-only guard

# ── Start chaos daemon ────────────────────────────────────────────────────────

if (( NO_CHAOS == 0 )); then
  # Auto-detect chaos plan if not specified: laptop uses dc, cloud uses cloud_dc
  if [[ -z "$CHAOS_PLAN" ]]; then
    if [[ -f "$REPO_ROOT/chaos_plans/always_on_cloud_dc.yaml" ]] && \
       hostname | grep -qi "cloud\|oracle\|oci\|compute\|vm\|prod"; then
      CHAOS_PLAN="$REPO_ROOT/chaos_plans/always_on_cloud_dc.yaml"
    elif [[ -f "$REPO_ROOT/chaos_plans/always_on_dc.yaml" ]]; then
      CHAOS_PLAN="$REPO_ROOT/chaos_plans/always_on_dc.yaml"
    else
      warn "no chaos plan found — skipping chaos daemon"
      NO_CHAOS=1
    fi
  fi

  if (( NO_CHAOS == 0 )); then
    log "starting chaos daemon via scripts/chaos_runner.sh (plan=$(basename "$CHAOS_PLAN"))..."
    if PLAN="$CHAOS_PLAN" bash scripts/chaos_runner.sh >/dev/null 2>&1; then
      if [[ -f runtime/chaos_runner.pid ]]; then
        CHAOS_PID="$(cat runtime/chaos_runner.pid 2>/dev/null || true)"
        if [[ -n "$CHAOS_PID" ]]; then
          echo "$CHAOS_PID" > "$CHAOS_PID_FILE"
        fi
      fi
      log "chaos pid=${CHAOS_PID:-unknown}  log=$CHAOS_LOG"
    else
      warn "chaos_runner.sh failed to start — check runtime/chaos_runner.log"
    fi
  fi
fi

# ── Summary ───────────────────────────────────────────────────────────────────

echo ""
echo "${BOLD}=== 30-day run started ===${RESET}"
echo ""
[[ -f "$BONSAI_PID_FILE"  ]] && echo "  bonsai  : pid=$(cat $BONSAI_PID_FILE)   log=$BONSAI_LOG"
[[ -f "$SIDECAR_PID_FILE" ]] && echo "  sidecar : pid=$(cat $SIDECAR_PID_FILE)   log=$SIDECAR_LOG"
[[ -f "$CHAOS_PID_FILE"   ]] && echo "  chaos   : pid=$(cat $CHAOS_PID_FILE)   log=$CHAOS_LOG"
echo ""
echo "  UI   : http://localhost:3000/"
echo "  Check: bash scripts/ops/start_30day_run.sh --status"
echo "  Stop : bash scripts/ops/teardown.sh"
echo ""
echo "  IMPORTANT: Do NOT run docker compose for bonsai."
echo "  ContainerLab (clab-bonsai-dc-*) is the only thing using docker."
echo ""
