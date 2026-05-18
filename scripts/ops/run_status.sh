#!/usr/bin/env bash
# scripts/ops/run_status.sh — Visual 30-day run health dashboard.
#
# Run at any time to see EXACTLY where you are.
# Each check prints PASS / WARN / FAIL with colour.
# Exit code = number of FAILs (0 = all green).
#
# Usage:
#   bash scripts/ops/run_status.sh           # full dashboard
#   bash scripts/ops/run_status.sh --once    # run once and exit (default)
#   bash scripts/ops/run_status.sh --watch   # refresh every 10s (Ctrl-C to exit)

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

WATCH=0
[[ "${1:-}" == "--watch" ]] && WATCH=1

GRN=$'\033[32m'; YLW=$'\033[33m'; RED=$'\033[31m'; CYN=$'\033[36m'
BOLD=$'\033[1m'; DIM=$'\033[2m'; RST=$'\033[0m'

PASS=0; WARN=0; FAIL=0

_pass() { PASS=$((PASS+1)); echo "  ${GRN}✓ PASS${RST}  $*"; }
_warn() { WARN=$((WARN+1)); echo "  ${YLW}⚠ WARN${RST}  $*"; }
_fail() { FAIL=$((FAIL+1)); echo "  ${RED}✗ FAIL${RST}  $*"; }
_info() { echo "  ${DIM}     ${RST}  $*"; }
_head() { echo ""; echo "${BOLD}${CYN}── $* ──────────────────────────────────────────${RST}"; }

_run_once() {
  PASS=0; WARN=0; FAIL=0

  echo ""
  echo "${BOLD}╔══════════════════════════════════════════════════╗${RST}"
  echo "${BOLD}║     bonsai 30-day run status  $(date -u '+%Y-%m-%dT%H:%M:%SZ')  ║${RST}"
  echo "${BOLD}╚══════════════════════════════════════════════════╝${RST}"

  # ── 1. Binary version ────────────────────────────────────────────────────────
  _head "1. Binary"
  if [[ -x "$REPO_ROOT/target/release/bonsai" ]]; then
    BIN_TS="$(stat -c '%Y' "$REPO_ROOT/target/release/bonsai" 2>/dev/null || stat -f '%m' "$REPO_ROOT/target/release/bonsai" 2>/dev/null || echo 0)"
    BIN_AGE_H=$(( ( $(date +%s) - BIN_TS ) / 3600 ))
    _pass "target/release/bonsai exists (built ${BIN_AGE_H}h ago)"
  else
    _fail "target/release/bonsai NOT FOUND — run: cargo build --release"
  fi

  GIT_SHA="$(git rev-parse --short=8 HEAD 2>/dev/null || echo unknown)"
  GIT_BRANCH="$(git branch --show-current 2>/dev/null || echo unknown)"
  _info "repo: $GIT_SHA on $GIT_BRANCH"

  # ── 2. bonsai process ────────────────────────────────────────────────────────
  _head "2. bonsai process"
  if [[ -f runtime/bonsai.pid ]]; then
    PID="$(cat runtime/bonsai.pid 2>/dev/null)"
    if [[ -n "$PID" ]] && kill -0 "$PID" 2>/dev/null; then
      _pass "running (pid $PID)"
    else
      _fail "pidfile present but process DEAD (stale pid $PID)"
    fi
  else
    _fail "NOT running (no pidfile)"
  fi

  # ── 3. /health endpoint ──────────────────────────────────────────────────────
  _head "3. /health"
  HEALTH="$(curl -sf --max-time 3 "http://127.0.0.1:3000/health" 2>/dev/null || echo '')"
  if [[ -z "$HEALTH" ]]; then
    _fail "no response from http://127.0.0.1:3000/health"
  else
    STATUS="$(echo "$HEALTH" | python3 -c "import json,sys; print(json.load(sys.stdin).get('status','?'))" 2>/dev/null || echo '?')"
    VERSION="$(echo "$HEALTH" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('version','?')+' '+d.get('git_sha','?'))" 2>/dev/null || echo '?')"
    if [[ "$STATUS" == "ok" ]]; then
      _pass "status=ok  version=$VERSION"
    elif [[ "$STATUS" == "degraded" ]]; then
      MISSING="$(echo "$HEALTH" | python3 -c "import json,sys; print(json.load(sys.stdin).get('missing_required_sidecars','?'))" 2>/dev/null)"
      _warn "status=degraded  version=$VERSION  missing=$MISSING"
    else
      _fail "unexpected status='$STATUS'"
    fi
  fi

  # ── 4. Rules sidecar ─────────────────────────────────────────────────────────
  _head "4. Rules sidecar"
  if [[ -f runtime/bonsai-sidecar.pid ]]; then
    SC_PID="$(cat runtime/bonsai-sidecar.pid 2>/dev/null)"
    if [[ -n "$SC_PID" ]] && kill -0 "$SC_PID" 2>/dev/null; then
      _pass "running (pid $SC_PID)"
    else
      _fail "pidfile present but process DEAD (pid $SC_PID) — check logs/bonsai-sidecar.log"
    fi
  else
    _fail "NOT running (no pidfile) — run: start_30day_run.sh"
  fi

  SIDECARS="$(curl -sf --max-time 3 "http://127.0.0.1:3000/api/sidecars" 2>/dev/null || echo '')"
  if [[ -n "$SIDECARS" ]]; then
    python3 - <<PYEOF 2>/dev/null || true
import json, sys
sc = json.loads('''$SIDECARS''').get('sidecars', [])
for s in sc:
    ev = s.get('events_in_total', 0)
    det = s.get('detections_out_total', 0)
    name = s.get('name', '?')
    status = s.get('status', '?')
    print(f"         {name}: events_in={ev}  dets_out={det}  status={status}")
PYEOF
  fi

  # ── 5. ContainerLab topology ─────────────────────────────────────────────────
  _head "5. ContainerLab topology"
  if command -v docker >/dev/null 2>&1; then
    CLAB_NODES="$(docker ps --filter "name=clab-bonsai" --format "{{.Names}}:{{.Status}}" 2>/dev/null)"
    NODE_COUNT="$(echo "$CLAB_NODES" | grep -c "Up" 2>/dev/null || echo 0)"
    if [[ "$NODE_COUNT" -ge 6 ]]; then
      _pass "$NODE_COUNT clab nodes running"
    elif [[ "$NODE_COUNT" -gt 0 ]]; then
      _warn "only $NODE_COUNT clab nodes running (expected ≥6)"
      echo "$CLAB_NODES" | sed 's/^/         /'
    else
      _fail "no clab nodes running — run: bash scripts/lab/redeploy_dc.sh"
    fi
  else
    _warn "docker not available — cannot check clab topology"
  fi

  # ── 6. Device subscriptions ──────────────────────────────────────────────────
  _head "6. gNMI subscriptions"
  TOPO="$(curl -sf --max-time 5 "http://127.0.0.1:3000/api/topology" 2>/dev/null || echo '')"
  if [[ -n "$TOPO" ]]; then
    python3 - <<PYEOF 2>/dev/null || true
import json, sys
d = json.loads('''$TOPO''')
devs = d.get('devices', [])
total = len(devs)
healthy = sum(1 for x in devs if x.get('health') == 'healthy')
warn_c = sum(1 for x in devs if x.get('health') == 'warn')
crit = sum(1 for x in devs if x.get('health') == 'critical')
col = '\033[32m' if healthy == total else '\033[33m' if healthy > 0 else '\033[31m'
rst = '\033[0m'
print(f"  {col}{'✓ PASS' if healthy==total else ('⚠ WARN' if healthy>0 else '✗ FAIL')}{rst}  devices={total}  healthy={healthy}  warn={warn_c}  critical={crit}")
PYEOF
  else
    _warn "topology API not reachable"
  fi

  # ── 7. Detections flowing ────────────────────────────────────────────────────
  _head "7. Detections"
  DETS="$(curl -sf --max-time 5 "http://127.0.0.1:3000/api/detections" 2>/dev/null || echo '')"
  if [[ -n "$DETS" ]]; then
    echo "$DETS" | python3 -c "
import json, sys, collections
d = json.load(sys.stdin)
dets = d.get('detections', [])
counts = collections.Counter(x.get('rule_id','?') for x in dets)
total = len(dets)
col = '\033[32m' if total > 0 else '\033[33m'
rst = '\033[0m'
status = '\\u2713 PASS' if total > 0 else '\\u26a0 WARN'
print(f'  {col}{status}{rst}  total={total}  rules={dict(counts)}')
" 2>/dev/null || true
  else
    _warn "detections API not reachable"
  fi

  # ── 8. Archive ───────────────────────────────────────────────────────────────
  _head "8. Archive"
  ARCHIVE_DIR="runtime/archive"
  if [[ -d "$ARCHIVE_DIR" ]]; then
    PARQUET_COUNT="$(find "$ARCHIVE_DIR" -name "*.parquet" 2>/dev/null | wc -l | tr -d ' ')"
    ARCHIVE_SIZE="$(du -sh "$ARCHIVE_DIR" 2>/dev/null | cut -f1 || echo '?')"
    NEWEST_AGE_MINS=999
    NEWEST="$(find "$ARCHIVE_DIR" -name "*.parquet" -printf '%T@\t%p\n' 2>/dev/null | sort -rn | head -1 | cut -f2)"
    if [[ -n "$NEWEST" ]]; then
      NEWEST_TS="$(stat -c '%Y' "$NEWEST" 2>/dev/null || stat -f '%m' "$NEWEST" 2>/dev/null || echo 0)"
      NEWEST_AGE_MINS=$(( ( $(date +%s) - NEWEST_TS ) / 60 ))
    fi
    if [[ "$PARQUET_COUNT" -gt 0 && "$NEWEST_AGE_MINS" -lt 65 ]]; then
      _pass "$PARQUET_COUNT parquet files  size=$ARCHIVE_SIZE  newest=${NEWEST_AGE_MINS}m ago"
    elif [[ "$PARQUET_COUNT" -gt 0 ]]; then
      _warn "$PARQUET_COUNT files  size=$ARCHIVE_SIZE  newest=${NEWEST_AGE_MINS}m ago (stale — expected <65m)"
    else
      _fail "no parquet files in $ARCHIVE_DIR"
    fi

    # Archive depth (days)
    OLDEST_TS="$(find "$ARCHIVE_DIR" -name "*.parquet" -printf "%T@\n" 2>/dev/null | sort -n | head -1 || echo '')"
    if [[ -n "$OLDEST_TS" ]]; then
      DEPTH_DAYS="$(python3 -c "import time; print(round((time.time() - $OLDEST_TS) / 86400, 1))" 2>/dev/null || echo '?')"
      if python3 -c "exit(0 if $OLDEST_TS > 0 and ($(date +%s) - $OLDEST_TS)/86400 >= 1 else 1)" 2>/dev/null; then
        _info "archive depth: ${DEPTH_DAYS} days"
      else
        _info "archive depth: <1 day (run just started)"
      fi
    fi
  else
    _fail "runtime/archive does not exist — bonsai has not written any archive data yet"
  fi

  # ── 9. Chaos daemon ──────────────────────────────────────────────────────────
  _head "9. Chaos daemon"
  CHAOS_PID_FILE="runtime/chaos_runner.pid"
  if [[ -f "$CHAOS_PID_FILE" ]]; then
    C_PID="$(cat "$CHAOS_PID_FILE" 2>/dev/null)"
    if [[ -n "$C_PID" ]] && kill -0 "$C_PID" 2>/dev/null; then
      _pass "running (pid $C_PID)"
      # Last injection log line
      if [[ -f runtime/chaos_log.jsonl ]]; then
        LAST_EVENT="$(tail -1 runtime/chaos_log.jsonl 2>/dev/null | python3 -c "import json,sys; e=json.load(sys.stdin); print(e.get('event_type','?')+' '+e.get('ts','?'))" 2>/dev/null || echo '?')"
        _info "last event: $LAST_EVENT"
      fi
      if [[ -f runtime/chaos_runner.log ]]; then
        INJECT_COUNT="$(grep -c '"inject"' runtime/chaos_log.jsonl 2>/dev/null || grep -c 'inject' runtime/chaos_runner.log 2>/dev/null || echo '?')"
        _info "total injections: $INJECT_COUNT"
      fi
    else
      _fail "pidfile present but chaos daemon DEAD — run: bash scripts/chaos_runner.sh"
    fi
  else
    _warn "chaos daemon NOT running — run: bash scripts/chaos_runner.sh"
  fi

  # ── 10. bonsai.toml archive settings ─────────────────────────────────────────
  _head "10. bonsai.toml (archive settings)"
  if [[ -f bonsai.toml ]]; then
    ARCHIVE_ENABLED="$(grep -E '^\s*enabled\s*=' bonsai.toml | head -1 | grep -c 'true' || echo 0)"
    MAX_AGE="$(grep -E '^\s*max_age_hours\s*=' bonsai.toml | head -1 | grep -oE '[0-9]+' | head -1 || echo '?')"
    GNN_MODE="$(grep -E 'inference_mode' bonsai.toml | head -1 | grep -oE '"[^"]+"' | tr -d '"' || echo '?')"
    if [[ "$ARCHIVE_ENABLED" -ge 1 ]]; then
      _pass "archive enabled  max_age_hours=$MAX_AGE  gnn.inference_mode=$GNN_MODE"
    else
      _fail "[archive] enabled=true NOT set in bonsai.toml"
    fi
  else
    _fail "bonsai.toml not found"
  fi

  # ── 11. Port conflicts ───────────────────────────────────────────────────────
  _head "11. Port conflicts"
  if command -v ss >/dev/null 2>&1; then
    PORT3000="$(ss -tlnp 2>/dev/null | grep ':3000' || true)"
  else
    PORT3000="$(lsof -iTCP:3000 -sTCP:LISTEN 2>/dev/null || true)"
  fi
  if echo "$PORT3000" | grep -q 'docker\|containerd'; then
    _fail "port 3000 held by a docker container — remove with: docker rm -f <container>"
    echo "$PORT3000" | sed 's/^/         /'
  elif [[ -n "$PORT3000" ]]; then
    _pass "port 3000 in use by non-docker process (expected: bonsai)"
  else
    if [[ -f runtime/bonsai.pid ]] && kill -0 "$(cat runtime/bonsai.pid 2>/dev/null)" 2>/dev/null; then
      _warn "port 3000 not listed but bonsai pid alive — may be starting up"
    else
      _info "port 3000 free (bonsai not running)"
    fi
  fi

  # ── Summary ──────────────────────────────────────────────────────────────────
  echo ""
  echo "${BOLD}──────────────────────────────────────────────────────${RST}"
  P_COL="${GRN}"; W_COL="${YLW}"; F_COL="${RED}"
  [[ "$FAIL" -gt 0 ]] && SUMMARY_COL="$RED" || { [[ "$WARN" -gt 0 ]] && SUMMARY_COL="$YLW" || SUMMARY_COL="$GRN"; }
  echo "${SUMMARY_COL}${BOLD}  PASS=$PASS  WARN=$WARN  FAIL=$FAIL${RST}"
  if [[ "$FAIL" -gt 0 ]]; then
    echo "${RED}  Action required — fix FAIL items before the archive run is valid.${RST}"
  elif [[ "$WARN" -gt 0 ]]; then
    echo "${YLW}  Run is active but has warnings — investigate before relying on archive.${RST}"
  else
    echo "${GRN}  All checks green — 30-day run is healthy.${RST}"
  fi
  echo ""
}

if (( WATCH == 1 )); then
  while true; do
    clear
    _run_once
    echo "${DIM}  refreshing in 10s… (Ctrl-C to exit)${RST}"
    sleep 10
  done
else
  _run_once
  exit "$FAIL"
fi
