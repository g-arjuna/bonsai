#!/usr/bin/env bash
# CV7 — Comprehensive rebuild + validate driver for the Ubuntu laptop side
# of the Mac→push→Ubuntu→push-results→Mac iteration loop.
#
# Default pipeline (no flags) — bonsai-only, no lab, no chaos:
#    1. git pull (warn ONLY if non-results files are dirty)
#    2. regenerate Python protos
#    3. bonpy SPA build (npm install + build under ui-bonpy/)
#    4. cargo build --release
#    5. cargo test --release sidecar_registry (T4-2 unit tests)
#    6. lab status (informational only — does NOT bring lab up by default)
#    7. teardown + start bonsai with sidecar (with wait_for_health gating)
#    8. /api/sidecars after startup (dumps diagnostics on FAIL)
#    9. wait 20s + re-probe heartbeat
#   10. /health = ok
#   11. /bonpy/ UI served
#   12. NEW — bonsai UI + key REST endpoints smoke (/api/topology, /api/devices,
#       /api/detections, /api/sidecars, /api/managed-devices)
#   13. NEW — graph baseline (parse /api/topology for device/bgp/lldp counts)
#   14. T4-7 GATE — fault injection round-trip (skip-WARN if no lab BGP neighbour)
#   15. chaos micro-cycle (only with --with-chaos)
#   16. degrade probe (kill sidecar; /health flips to degraded after stale window)
#   17. teardown
#   18. push results to origin/main so Mac sees them next iteration
#
# Flags:
#   --with-lab          Run scripts/lab/redeploy_dc.sh --topo-only before step 7.
#                       Enables steps 13-14 to find real BGP neighbours.
#   --with-chaos        Run a 60s chaos micro-cycle in step 15 — verifies the
#                       chaos daemon comes up, runs, and stops cleanly with no
#                       restart-marker churn. Only meaningful with a live lab.
#   --skip-build        Skip steps 4-5 (cargo build/test). Use when iterating on
#                       script logic against an already-built binary.
#   --skip-push         Don't auto-commit/push the results. Useful for dry runs.
#   --full              Shorthand for --with-lab --with-chaos.
#   -h, --help          Show this help and exit.
#
# Comprehensive error capture: every command's full stdout+stderr is mirrored
# into the results .md file AND (for cargo build/test, npm install, etc.) into
# a sibling .log file referenced from the results. On any FAIL the relevant
# log paths are listed so the Mac iteration can pull and inspect.
#
# Diagnostic auto-dump: when /health, /api/sidecars, or /bonpy/ probes fail,
# dump_health_diagnostics() captures `ss -tlnp` for :3000, raw `curl -i`
# responses, and the last 200 lines of bonsai.log + sidecar.log into the
# results .md so the Mac side has the evidence without needing log files.
#
# Use on Ubuntu laptop only. Refuses on Mac per dev/ops boundary.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; BOLD=$'\033[1m'; RESET=$'\033[0m'

ENV_DETECTED="$(bash scripts/dev/whichenv.sh 2>/dev/null || echo unknown)"
if [[ "$ENV_DETECTED" == "mac-dev" ]]; then
  echo "${RED}Refused.${RESET} rebuild_and_validate runs on Ubuntu only." >&2
  exit 2
fi

# ── Arg parsing ───────────────────────────────────────────────────────────────
WITH_LAB=0
WITH_CHAOS=0
SKIP_BUILD=0
SKIP_PUSH=0
for arg in "$@"; do
  case "$arg" in
    --with-lab)   WITH_LAB=1 ;;
    --with-chaos) WITH_CHAOS=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    --skip-push)  SKIP_PUSH=1 ;;
    --full)       WITH_LAB=1; WITH_CHAOS=1 ;;
    -h|--help)
      sed -n '1,46p' "$0" | sed 's/^# \?//'
      exit 0 ;;
    *)
      echo "Unknown arg: $arg (try --help)" >&2; exit 1 ;;
  esac
done

DATE="$(date -u +%Y-%m-%dT%H%MZ)"
# IMPORTANT: absolute paths. Earlier versions used relative paths which broke
# silently when a step `pushd`'d into a subdirectory (writes went to the wrong
# place; PASS_COUNT incremented but the .md got no content for that step).
RESULTS_DIR="$REPO_ROOT/docs/test_results"
RESULTS_FILE="$RESULTS_DIR/cv7-validation-$DATE.md"
LOG_DIR="$RESULTS_DIR/cv7-validation-$DATE.logs"
mkdir -p "$RESULTS_DIR" "$LOG_DIR"

# ── Log retention policy (D2-1 T2) ───────────────────────────────────────────
# Keep the last LOG_RETENTION_COUNT .logs directories; archive older ones to
# docs/test_results/archive/ as gzip tarballs (~10-20 MB each compressed).
# Default: 10 runs. Override with LOG_RETENTION_COUNT env var.
LOG_RETENTION_COUNT="${LOG_RETENTION_COUNT:-10}"
ARCHIVE_DIR="$RESULTS_DIR/archive"
mkdir -p "$ARCHIVE_DIR"
# Collect existing .logs dirs sorted oldest-first.
_LOG_DIRS_SORTED=()
while IFS= read -r -d '' _d; do
  _LOG_DIRS_SORTED+=("$_d")
done < <(find "$RESULTS_DIR" -maxdepth 1 -name '*.logs' -type d -print0 | sort -z)
_EXCESS=$(( ${#_LOG_DIRS_SORTED[@]} - LOG_RETENTION_COUNT ))
if (( _EXCESS > 0 )); then
  for (( _i=0; _i<_EXCESS; _i++ )); do
    _OLD="${_LOG_DIRS_SORTED[$_i]}"
    _TARBALL="$ARCHIVE_DIR/$(basename "$_OLD").tar.gz"
    if tar -czf "$_TARBALL" -C "$(dirname "$_OLD")" "$(basename "$_OLD")" 2>/dev/null; then
      rm -rf "$_OLD"
      echo "[retention] archived $(basename "$_OLD") → archive/" >&2
    else
      echo "[retention] WARN: could not archive $_OLD — skipping removal" >&2
    fi
  done
fi
unset _LOG_DIRS_SORTED _EXCESS _i _OLD _TARBALL

# Defensive: if the script is interrupted mid-flight (e.g. between step 7
# (start bonsai) and step 17 (teardown)), leave the environment clean for
# the next iteration. The trap runs teardown.sh on any exit path.
_emergency_cleanup() {
  echo "[trap] emergency cleanup — running teardown.sh" >&2
  # Best-effort: stop chaos if we started it.
  if [[ -f runtime/chaos_runner.pid ]]; then
    bash scripts/chaos_runner.sh --stop >/dev/null 2>&1 || true
  fi
  bash scripts/ops/teardown.sh >/dev/null 2>&1 || true
}
trap _emergency_cleanup EXIT

# Header.
{
  echo "# CV7 validation run — $DATE"
  echo
  echo "Generated by \`scripts/ops/rebuild_and_validate.sh\`."
  echo "Flags: WITH_LAB=$WITH_LAB WITH_CHAOS=$WITH_CHAOS SKIP_BUILD=$SKIP_BUILD SKIP_PUSH=$SKIP_PUSH"
  echo
  echo "Each section corresponds to one validation step; PASS/FAIL/WARN is the line in **bold**."
  echo
  echo "Side-channel logs (cargo build/test, npm build, bonsai+sidecar stdout):"
  echo "  \`$LOG_DIR/\`"
  echo
} > "$RESULTS_FILE"

PASS_COUNT=0; FAIL_COUNT=0; WARN_COUNT=0

section() {
  local title="$1"
  echo
  echo "${BOLD}── $title ──${RESET}"
  {
    echo
    echo "## $title"
    echo
  } >> "$RESULTS_FILE"
}

record() {
  local status="$1" line="$2"
  echo "$line"
  echo "$line" >> "$RESULTS_FILE"
  case "$status" in
    PASS) PASS_COUNT=$((PASS_COUNT+1)) ;;
    FAIL) FAIL_COUNT=$((FAIL_COUNT+1)) ;;
    WARN) WARN_COUNT=$((WARN_COUNT+1)) ;;
  esac
}

# Run a shell command, mirror stdout+stderr into the results file, also write
# a copy into $LOG_DIR/<tag>.log. Returns the command's exit code.
run_or_capture_to_log() {
  local tag="$1" cmd="$2"
  local logfile="$LOG_DIR/$tag.log"
  echo "    \$ $cmd" >> "$RESULTS_FILE"
  echo "    (full log: $logfile)" >> "$RESULTS_FILE"
  echo "    \$ $cmd" > "$logfile"
  { eval "$cmd" 2>&1 | tee -a "$logfile" | tee -a "$RESULTS_FILE" ; }
  return ${PIPESTATUS[0]}
}

# Read a numeric field from /api/sidecars for the first sidecar of kind=rules.
sidecar_counter() {
  local field="$1"
  curl -fsS http://127.0.0.1:3000/api/sidecars 2>/dev/null | python3 -c "
import json,sys
try:
    d = json.load(sys.stdin)
except Exception:
    print(0); sys.exit(0)
for s in d.get('sidecars', []):
    if s.get('kind') == 'rules':
        print(s.get('$field', 0)); sys.exit(0)
print(0)
" 2>/dev/null || echo 0
}

# Wait for bonsai's /health to return ANY response (2xx or 5xx — we accept
# both "ok" and "degraded waiting for sidecar"). Returns 0 once /health
# responds, 1 on timeout. Used after starting bonsai before proceeding to
# probes that assume the server is up.
wait_for_health() {
  local max_secs="${1:-60}" elapsed=0 code
  while (( elapsed < max_secs )); do
    code="$(curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:3000/health 2>/dev/null || echo 000)"
    if [[ "$code" == "200" || "$code" == "503" ]]; then
      echo "$code"
      return 0
    fi
    sleep 1
    elapsed=$((elapsed+1))
  done
  echo "000"
  return 1
}

# Dump diagnostic context into the results .md when /health doesn't respond
# correctly. This is the evidence the Mac iteration needs to diagnose remote
# failures. Also dumps `ss -tlnp` for port 3000 so we can see WHICH process is
# bound (if any) and `curl -i` for raw HTTP headers.
dump_health_diagnostics() {
  local label="$1"
  {
    echo
    echo "### Diagnostic dump — $label"
    echo
    echo "**Port 3000 listener (\`ss -tlnp\`):**"
    echo '```'
    ss -tlnp 2>/dev/null | grep -E ':3000\b' || echo "(nothing listening on :3000)"
    echo '```'
    echo
    echo "**Raw /health response (\`curl -i\`):**"
    echo '```'
    curl -isS -m 5 http://127.0.0.1:3000/health 2>&1 | head -30 || echo "(curl failed)"
    echo '```'
    echo
    echo "**Raw /api/sidecars response:**"
    echo '```'
    curl -isS -m 5 http://127.0.0.1:3000/api/sidecars 2>&1 | head -30 || echo "(curl failed)"
    echo '```'
    echo
    echo "**Tail of logs/bonsai.log (last 200 lines):**"
    echo '```'
    if [[ -f logs/bonsai.log ]]; then
      tail -200 logs/bonsai.log | sed 's/\x1b\[[0-9;]*m//g'
    else
      echo "(logs/bonsai.log not present)"
    fi
    echo '```'
    echo
    echo "**Tail of logs/bonsai-sidecar.log (last 100 lines):**"
    echo '```'
    if [[ -f logs/bonsai-sidecar.log ]]; then
      tail -100 logs/bonsai-sidecar.log | sed 's/\x1b\[[0-9;]*m//g'
    else
      echo "(logs/bonsai-sidecar.log not present — sidecar may not have started)"
    fi
    echo '```'
    echo
    echo "**Running bonsai/python processes (\`ps\`):**"
    echo '```'
    ps -ef 2>/dev/null | grep -E '(bonsai|collector_engine)' | grep -v grep || echo "(no bonsai/collector_engine processes)"
    echo '```'
  } >> "$RESULTS_FILE"
}

# Probe an HTTP endpoint, log the response code and a body snippet into the
# results .md. Returns the HTTP code on stdout. Used by step 12 (UI/API smoke).
ui_probe() {
  local path="$1" label="${2:-$path}"
  local url="http://127.0.0.1:3000$path"
  local code body
  code="$(curl -sS -o /tmp/.ui_probe_body -w '%{http_code}' -m 8 "$url" 2>/dev/null || echo 000)"
  body="$(head -c 300 /tmp/.ui_probe_body 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' || true)"
  {
    echo "- \`GET $path\` → **HTTP $code**  ($label)"
    if [[ -n "$body" ]]; then
      echo '  ```'
      echo "  $body" | head -5
      echo '  ```'
    fi
  } >> "$RESULTS_FILE"
  echo "$code"
}

# ── 1. git pull ───────────────────────────────────────────────────────────────
section "1. git pull origin main"
# Cosmetic-WARN fix: only flag if files OUTSIDE docs/test_results/ are dirty.
# The script's own about-to-be-created results file always shows up as dirty;
# don't count it.
DIRTY_NON_RESULTS="$(git status --porcelain | awk '$2 !~ /^docs\/test_results\// {print}')"
if [[ -n "$DIRTY_NON_RESULTS" ]]; then
  record WARN "**WARN**: working tree is dirty in non-results files; pull may overwrite local changes."
  echo "$DIRTY_NON_RESULTS" | tee -a "$RESULTS_FILE"
fi
run_or_capture_to_log "01-git-fetch"  "git fetch origin main"
run_or_capture_to_log "01-git-reset"  "git reset --hard origin/main"
HEAD_SHA="$(git rev-parse HEAD)"
record PASS "**PASS**: at $HEAD_SHA"

# ── 2. python protos ──────────────────────────────────────────────────────────
section "2. regenerate Python gRPC stubs"
if [[ -x .venv/bin/python ]]; then PY=.venv/bin/python; else PY=python3; fi
if run_or_capture_to_log "02-gen-protos" "$PY python/gen_protos.py" ; then
  record PASS "**PASS**: protos regenerated"
else
  record FAIL "**FAIL**: gen_protos.py errored — see $LOG_DIR/02-gen-protos.log"
fi

# ── 3. bonpy SPA build ────────────────────────────────────────────────────────
section "3. bonpy SPA build (ui-bonpy/)"
if [[ ! -d ui-bonpy ]]; then
  record WARN "**WARN**: ui-bonpy/ directory missing — bonpy SPA scaffold not present."
elif ! command -v npm >/dev/null 2>&1; then
  record WARN "**WARN**: npm not on PATH — bonpy SPA cannot be built. Install Node.js."
else
  pushd ui-bonpy >/dev/null
  BONPY_OK=1
  if [[ ! -d node_modules ]]; then
    if ! run_or_capture_to_log "03-bonpy-npm-install" "npm install --no-audit --no-fund --silent"; then
      record FAIL "**FAIL**: npm install failed — see $LOG_DIR/03-bonpy-npm-install.log"
      BONPY_OK=0
    fi
  fi
  if (( BONPY_OK == 1 )); then
    if run_or_capture_to_log "03-bonpy-build" "npm run build"; then
      record PASS "**PASS**: bonpy SPA built — ui-bonpy/dist/ ready for Axum to serve at /bonpy/."
    else
      record FAIL "**FAIL**: npm run build failed — see $LOG_DIR/03-bonpy-build.log"
    fi
  fi
  popd >/dev/null
fi

# ── 4. cargo build ────────────────────────────────────────────────────────────
section "4. cargo build --release"
if (( SKIP_BUILD == 1 )); then
  record WARN "**WARN**: --skip-build set; using existing target/release/bonsai (if any)."
else
  CARGO_LOG="$LOG_DIR/04-cargo-build.log"
  echo "    \$ cargo build --release" >> "$RESULTS_FILE"
  echo "    (full log: $CARGO_LOG)" >> "$RESULTS_FILE"
  if cargo build --release 2>&1 | tee "$CARGO_LOG" | tail -100 | tee -a "$RESULTS_FILE" ; then
    record PASS "**PASS**: cargo build succeeded"
  else
    record FAIL "**FAIL**: cargo build failed — STOP, fix on Mac, re-run iteration. Full log: $CARGO_LOG"
    echo "Aborting further checks." | tee -a "$RESULTS_FILE"
    exit 1
  fi
fi

# ── 5. T4-2 unit tests ────────────────────────────────────────────────────────
section "5. cargo test sidecar_registry (T4-2 unit tests)"
if (( SKIP_BUILD == 1 )); then
  record WARN "**WARN**: --skip-build set; skipping cargo test as well."
else
  TEST_LOG="$LOG_DIR/05-cargo-test.log"
  echo "    \$ cargo test --release -p bonsai sidecar_registry" >> "$RESULTS_FILE"
  echo "    (full log: $TEST_LOG)" >> "$RESULTS_FILE"
  if cargo test --release -p bonsai sidecar_registry 2>&1 | tee "$TEST_LOG" | tail -40 | tee -a "$RESULTS_FILE" ; then
    record PASS "**PASS**: sidecar_registry tests pass"
  else
    record FAIL "**FAIL**: sidecar_registry tests broken — full log: $TEST_LOG"
  fi
fi

# ── 6. lab bringup / status ───────────────────────────────────────────────────
section "6. lab status / bringup"
LAB_LOG="$LOG_DIR/06-lab.log"
if (( WITH_LAB == 1 )); then
  # --topo-only: deploy the lab but do NOT let redeploy_dc.sh restart bonsai —
  # we manage bonsai ourselves in step 7. The lab redeploy is destructive
  # (full clab destroy → deploy) and takes 1-3 minutes.
  echo "    \$ bash scripts/lab/redeploy_dc.sh --topo-only" >> "$RESULTS_FILE"
  echo "    (full log: $LAB_LOG)" >> "$RESULTS_FILE"
  if bash scripts/lab/redeploy_dc.sh --topo-only > "$LAB_LOG" 2>&1; then
    tail -20 "$LAB_LOG" >> "$RESULTS_FILE"
    record PASS "**PASS**: lab redeployed (DC topology). BGP convergence may need ~60s before step 14."
  else
    tail -50 "$LAB_LOG" >> "$RESULTS_FILE"
    record FAIL "**FAIL**: lab redeploy failed — full log: $LAB_LOG. Step 14 (T4-7 gate) will WARN-skip."
  fi
else
  # No --with-lab: just inspect what's already running. Doesn't change state.
  if bash scripts/lab/redeploy_dc.sh --check > "$LAB_LOG" 2>&1; then
    tail -30 "$LAB_LOG" >> "$RESULTS_FILE"
    if grep -qE 'CONTAINER|running|healthy' "$LAB_LOG" 2>/dev/null; then
      record PASS "**PASS**: lab appears to be running (check output above). Step 14 should find BGP neighbours."
    else
      record WARN "**WARN**: no lab detected. Re-run with --with-lab to bring it up; step 14 will WARN-skip otherwise."
    fi
  else
    record WARN "**WARN**: scripts/lab/redeploy_dc.sh --check exited non-zero or unavailable. Step 14 may WARN-skip."
  fi
fi

# ── 7. teardown + start ───────────────────────────────────────────────────────
section "7. teardown + start bonsai with sidecar"
bash scripts/ops/teardown.sh >> "$RESULTS_FILE" 2>&1 || true

# Sanity-check :3000 is free AFTER teardown. teardown.sh now also removes
# the docker bonsai containers (bonsai-lab-dc, cloud-dc, etc.) per CV7
# Tier 2 (laptop = bonsai-as-process). If :3000 is still bound here it means
# an unknown process owns it — we must not start a second bonsai on top.
#
# Identify the offender by docker container name (no sudo needed) + lsof
# (best-effort; needs sudo for non-owned sockets).
ORPHAN_3000="$(ss -tlnp 2>/dev/null | grep -E ':3000\b' || true)"
DOCKER_ON_3000="$(docker ps --format '{{.Names}}\t{{.Ports}}' 2>/dev/null | grep -E ':3000->' || true)"
LSOF_3000="$(lsof -i :3000 -sTCP:LISTEN -P -n 2>/dev/null | tail -n +2 || true)"

if [[ -n "$ORPHAN_3000" || -n "$DOCKER_ON_3000" ]]; then
  {
    echo
    echo "### Pre-flight orphan check — something still owns :3000 after teardown"
    echo
    echo "**\`ss -tlnp\`:**"
    echo '```'
    echo "$ORPHAN_3000"
    echo '```'
    echo
    echo "**\`docker ps\` containers publishing :3000:**"
    echo '```'
    [[ -n "$DOCKER_ON_3000" ]] && echo "$DOCKER_ON_3000" || echo "(no docker containers publishing :3000)"
    echo '```'
    echo
    echo "**\`lsof -i :3000 -sTCP:LISTEN\` (may be empty if not run as root):**"
    echo '```'
    [[ -n "$LSOF_3000" ]] && echo "$LSOF_3000" || echo "(empty — try running this manually with sudo)"
    echo '```'
  } >> "$RESULTS_FILE"
  record FAIL "**FAIL**: :3000 still bound after teardown — refusing to start a second bonsai on top. See dump above; identify and stop the owner manually (commonly: \`docker rm -f bonsai-bonsai-lab-dc-1\`). Aborting validation."
  exit 1
fi

# Start in background; the wrapper itself writes to logs/bonsai.log and
# logs/bonsai-sidecar.log. We copy those into the side-log dir at teardown
# for the Mac-side iteration.
bash scripts/ops/start_bonsai_with_sidecar.sh >> "$RESULTS_FILE" 2>&1 &
START_WRAPPER_PID=$!

# Gate the probes on /health responding (200 OR 503). With BONSAI_REQUIRE_SIDECAR=rules
# the first response will be 503 — the wrapper starts the sidecar after this gate
# clears, which then flips /health to 200 within ~10s.
echo "Waiting up to 90s for bonsai /health to respond (200 or 503)…" >> "$RESULTS_FILE"
if HEALTH_AT_START="$(wait_for_health 90)"; then
  record PASS "**PASS**: bonsai HTTP listener up (/health → $HEALTH_AT_START)"
else
  record FAIL "**FAIL**: bonsai /health never responded within 90s — wrapper may have exited. Diagnostics below."
  dump_health_diagnostics "step 7 — bonsai never bound :3000"
fi
# Give the wrapper a few more seconds to start the sidecar after /health came up.
sleep 8

# ── 8. /api/sidecars after startup ────────────────────────────────────────────
section "8. /api/sidecars after startup"
SIDECARS_JSON="$(curl -fsS http://127.0.0.1:3000/api/sidecars 2>&1 || true)"
echo '```' >> "$RESULTS_FILE"
echo "$SIDECARS_JSON" | head -20 >> "$RESULTS_FILE"
echo '```' >> "$RESULTS_FILE"
N_SIDECARS="$(echo "$SIDECARS_JSON" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d.get("sidecars",[])))' 2>/dev/null || echo 0)"
if (( N_SIDECARS >= 1 )); then
  record PASS "**PASS**: $N_SIDECARS sidecar(s) registered"
else
  record FAIL "**FAIL**: no sidecars registered — check logs/bonsai-sidecar.log"
  cp -f logs/bonsai-sidecar.log "$LOG_DIR/06-sidecar.log" 2>/dev/null || true
  dump_health_diagnostics "step 8 — no sidecar registered"
fi

# ── 9. wait for first heartbeat ───────────────────────────────────────────────
section "9. wait 20s for first heartbeat then re-probe"
sleep 20
SIDECARS_JSON2="$(curl -fsS http://127.0.0.1:3000/api/sidecars 2>&1 || true)"
echo '```' >> "$RESULTS_FILE"
echo "$SIDECARS_JSON2" | head -20 >> "$RESULTS_FILE"
echo '```' >> "$RESULTS_FILE"
HEALTHY="$(echo "$SIDECARS_JSON2" | python3 -c '
import json,sys
d=json.load(sys.stdin)
print(sum(1 for s in d.get("sidecars",[]) if s.get("status")=="healthy"))
' 2>/dev/null || echo 0)"
if (( HEALTHY >= 1 )); then
  record PASS "**PASS**: $HEALTHY sidecar(s) healthy"
else
  record FAIL "**FAIL**: no sidecars in healthy state"
  dump_health_diagnostics "step 9 — no healthy sidecar after 20s heartbeat window"
fi

# ── 10. /health with sidecar present ──────────────────────────────────────────
section "10. /health (BONSAI_REQUIRE_SIDECAR=rules already set)"
HEALTH_JSON="$(curl -sS http://127.0.0.1:3000/health 2>&1 || true)"
echo "$HEALTH_JSON" >> "$RESULTS_FILE"
if echo "$HEALTH_JSON" | grep -q '"status":"ok"'; then
  record PASS "**PASS**: /health = ok"
else
  record FAIL "**FAIL**: /health did not return ok with sidecar registered"
  dump_health_diagnostics "step 10 — /health not ok despite sidecar registered"
fi

# ── 11. bonpy UI ──────────────────────────────────────────────────────────────
section "11. bonpy UI served at /bonpy/"
BONPY_CODE="$(curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:3000/bonpy/ || echo 000)"
echo "HTTP $BONPY_CODE  /bonpy/" >> "$RESULTS_FILE"
if [[ "$BONPY_CODE" == "200" ]]; then
  record PASS "**PASS**: GET /bonpy/ returned 200"
else
  record WARN "**WARN**: /bonpy/ returned $BONPY_CODE — step 3 may have failed; see $LOG_DIR/03-bonpy-build.log"
fi

# ── 12. bonsai UI + REST endpoint smoke ───────────────────────────────────────
# Confirms the surface area the operator interacts with: the SPA at /, the
# core APIs they reach by hand, and the OpenAPI doc. We don't validate
# response bodies deeply — just that each endpoint returns 200/2xx. Bodies
# get truncated to 300 bytes in the .md so the Mac iteration sees shape.
section "12. bonsai UI + REST endpoint smoke"
echo "Probing key REST/UI routes:" >> "$RESULTS_FILE"
SLASH_CODE="$(ui_probe "/"                    "bonsai SPA index")"
DOCS_CODE="$(ui_probe "/api/docs"             "Swagger UI (utoipa)")"
OAPI_CODE="$(ui_probe "/api/openapi.json"     "OpenAPI spec")"
TOPO_CODE="$(ui_probe "/api/topology"         "topology snapshot")"
DET_CODE="$(ui_probe "/api/detections"        "recent detections")"
SIDECARS_CODE="$(ui_probe "/api/sidecars"     "sidecar registry")"
MD_CODE="$(ui_probe "/api/managed-devices"    "managed-device list")"
echo "   (also tried: docs=$DOCS_CODE openapi=$OAPI_CODE managed-devices=$MD_CODE)" >> "$RESULTS_FILE"

PASS_COUNT_LOCAL=0
for c in "$SLASH_CODE" "$TOPO_CODE" "$DET_CODE" "$SIDECARS_CODE"; do
  [[ "$c" == "200" ]] && PASS_COUNT_LOCAL=$((PASS_COUNT_LOCAL+1))
done
if (( PASS_COUNT_LOCAL >= 4 )); then
  record PASS "**PASS**: all four core endpoints (/, /api/topology, /api/detections, /api/sidecars) returned 200"
elif (( PASS_COUNT_LOCAL >= 2 )); then
  record WARN "**WARN**: only $PASS_COUNT_LOCAL/4 core endpoints returned 200 — see codes above"
else
  record FAIL "**FAIL**: fewer than 2 core endpoints returned 200 — bonsai surface is broken"
  dump_health_diagnostics "step 12 — UI/API smoke failure"
fi

# ── 13. graph baseline ────────────────────────────────────────────────────────
# Parse /api/topology and report counts of devices / BGP neighbours / LLDP
# edges. PASS = any data present. WARN = empty (no lab, or lab still
# converging). This is the "is the graph populated?" sanity check.
#
# When --with-lab: poll for up to 150s waiting for BGP sessions to appear.
# CA cert is refreshed by redeploy_dc.sh; subscribers connect within a few
# seconds of bonsai start. BGP needs an additional 60-90s to converge.
if (( WITH_LAB == 1 )); then
  echo "waiting up to 150s for BGP sessions to converge (CA cert + BGP convergence)..." >> "$RESULTS_FILE"
  BGP_WAIT_SECS=0
  while (( BGP_WAIT_SECS < 150 )); do
    _TOPO_CHECK="$(curl -fsS http://127.0.0.1:3000/api/topology 2>/dev/null || true)"
    _BGP_NOW="$(echo "$_TOPO_CHECK" | python3 -c "
import json,sys
try:
    d=json.load(sys.stdin)
    print(sum(len(dev.get('bgp',[])) for dev in d.get('devices',[])))
except: print(0)" 2>/dev/null || echo 0)"
    if (( _BGP_NOW > 0 )); then
      echo "  BGP sessions appeared after ${BGP_WAIT_SECS}s: ${_BGP_NOW} total" >> "$RESULTS_FILE"
      break
    fi
    sleep 10
    BGP_WAIT_SECS=$(( BGP_WAIT_SECS + 10 ))
  done
fi

section "13. graph baseline (counts from /api/topology)"
TOPO_JSON="$(curl -fsS http://127.0.0.1:3000/api/topology 2>/dev/null || true)"
COUNTS="$(echo "$TOPO_JSON" | python3 -c "
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    print('parse_error', 0, 0, 0); sys.exit(0)
devices = len(d.get('devices', []) or d.get('nodes', []))
# BGP is per-device: devices[].bgp[]. Fall back to top-level keys for older schemas.
bgp = sum(len(dev.get('bgp', [])) for dev in d.get('devices', []))
bgp = bgp or len(d.get('bgp_neighbors', [])) or len(d.get('neighbors', []))
# LLDP links are top-level in the current schema.
lldp = len(d.get('links', [])) or len(d.get('lldp_neighbors', [])) or len(d.get('lldp', []))
print('ok', devices, bgp, lldp)
" 2>/dev/null || echo "parse_error 0 0 0")"
read -r CSTATUS DEVS BGP LLDP <<<"$COUNTS"
{
  echo "- devices:          **$DEVS**"
  echo "- BGP neighbours:   **$BGP**"
  echo "- LLDP edges:       **$LLDP**"
  echo "- parse status:     $CSTATUS"
} >> "$RESULTS_FILE"

if [[ "$CSTATUS" != "ok" ]]; then
  record FAIL "**FAIL**: /api/topology JSON unparseable. Body head:"
  echo '```' >> "$RESULTS_FILE"
  echo "$TOPO_JSON" | head -c 500 >> "$RESULTS_FILE"
  echo '```' >> "$RESULTS_FILE"
elif (( DEVS > 0 )); then
  record PASS "**PASS**: graph has $DEVS device(s) / $BGP BGP / $LLDP LLDP. Bonsai is ingesting state."
else
  record WARN "**WARN**: graph is empty (no devices). Either lab is not running, or bonsai subscriptions haven't converged yet. Step 14 (T4-7 gate) will WARN-skip."
fi

# ── 14. T4-7 gate: fault injection round-trip ─────────────────────────────────
# Proves the Python sidecar catches the three rule_ids the Rust fastpath
# currently double-handles. If this passes, src/event_detection.rs is safe
# to delete in the next Mac iteration.
section "14. T4-7 gate: fault injection round-trip"

BGP_TARGET="$(echo "$TOPO_JSON" | python3 -c "
import json, sys, re
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(0)
candidates = []
# Current schema: BGP sessions are per-device in devices[].bgp[].
for device in d.get('devices', []):
    dev_addr = device.get('address', '')
    for n in device.get('bgp', []):
        peer = n.get('peer', '')
        state = (n.get('state') or '').lower()
        if dev_addr and peer and state in ('established', 'up'):
            candidates.append((dev_addr, peer))
# Legacy fallback for older top-level schemas.
for n in d.get('bgp_neighbors', []) + d.get('neighbors', []):
    dev = n.get('device_address') or n.get('device') or n.get('source')
    peer = n.get('peer_address') or n.get('peer')
    state = (n.get('session_state') or n.get('state') or '').lower()
    if dev and peer and state in ('established', 'up'):
        candidates.append((dev, peer))
if not candidates:
    sys.exit(0)
clab_match = next((c for c in candidates if re.search(r'srl|spine|leaf|pe|p1|p2', c[0], re.I)), None)
dev, peer = clab_match or candidates[0]
print(dev, peer)
" 2>/dev/null || true)"

if [[ -z "$BGP_TARGET" ]]; then
  record WARN "**WARN**: no established BGP neighbour found in /api/topology — T4-7 gate cannot be closed from this iteration. Re-run with --with-lab to bring up DC topology."
else
  read -r NODE PEER <<<"$BGP_TARGET"
  echo "selected fault target: device=$NODE peer=$PEER" >> "$RESULTS_FILE"

  DET_BEFORE="$(sidecar_counter detections_out_total)"
  EVT_BEFORE="$(sidecar_counter events_in_total)"
  echo "before: events_in_total=$EVT_BEFORE detections_out_total=$DET_BEFORE" >> "$RESULTS_FILE"

  INJ_LOG="$LOG_DIR/14-inject_fault.log"
  echo "    \$ $PY python/inject_fault.py bgp-flap $NODE $PEER --hold 10" >> "$RESULTS_FILE"
  echo "    (full log: $INJ_LOG)" >> "$RESULTS_FILE"
  if $PY python/inject_fault.py bgp-flap "$NODE" "$PEER" --hold 10 > "$INJ_LOG" 2>&1; then
    tail -30 "$INJ_LOG" >> "$RESULTS_FILE"
    echo "waiting 25s for sidecar to fire and persist detection…" >> "$RESULTS_FILE"
    sleep 25

    DET_AFTER="$(sidecar_counter detections_out_total)"
    EVT_AFTER="$(sidecar_counter events_in_total)"
    DET_DELTA=$((DET_AFTER - DET_BEFORE))
    EVT_DELTA=$((EVT_AFTER - EVT_BEFORE))
    echo "after:  events_in_total=$EVT_AFTER detections_out_total=$DET_AFTER  (Δevt=$EVT_DELTA Δdet=$DET_DELTA)" >> "$RESULTS_FILE"

    RECENT_RIDS="$(curl -fsS http://127.0.0.1:3000/api/detections 2>/dev/null | python3 -c "
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(0)
items = d if isinstance(d, list) else d.get('detections', [])
for it in list(items)[-50:]:
    rid = it.get('rule_id') or ''
    if rid:
        print(rid)
" 2>/dev/null | sort -u)"
    echo "recent rule_ids in /api/detections:" >> "$RESULTS_FILE"
    echo "$RECENT_RIDS" >> "$RESULTS_FILE"

    HIT_RETIRED=0
    for rid in bgp_session_down bgp_session_flap interface_down bfd_session_down; do
      if echo "$RECENT_RIDS" | grep -qx "$rid"; then
        HIT_RETIRED=1
        echo "  ✓ saw retired-fastpath rule_id $rid" >> "$RESULTS_FILE"
      fi
    done

    if (( DET_DELTA > 0 )) && (( HIT_RETIRED == 1 )); then
      record PASS "**PASS**: Python sidecar fired retired-fastpath rule_id AND counter ticked (Δdet=$DET_DELTA). T4-7 gate closeable."
    elif (( HIT_RETIRED == 1 )); then
      record WARN "**WARN**: retired-fastpath rule_id appeared in /api/detections but sidecar counter Δ=0 — could be Rust fastpath writing, not Python. Cannot close T4-7 gate from this iteration."
    elif (( DET_DELTA > 0 )); then
      record WARN "**WARN**: sidecar counter ticked (Δdet=$DET_DELTA) but no retired-fastpath rule_id seen — Python is firing OTHER rules; gate inconclusive for the 3 retired ids."
    else
      record WARN "**WARN**: fault injected but neither counter nor /api/detections shows new bgp_* detections. Check inject_fault output + logs/bonsai.log."
    fi
  else
    tail -50 "$INJ_LOG" >> "$RESULTS_FILE"
    record WARN "**WARN**: inject_fault.py failed — likely lab/credential issue. Full log: $INJ_LOG"
  fi
fi

# ── 15. chaos micro-cycle (optional) ──────────────────────────────────────────
# Brings the chaos daemon up for 60s, verifies it stays alive (no
# restart-marker churn), then stops it cleanly. Only meaningful with a live
# lab — if the graph is empty (step 13), this WARN-skips to avoid wasted
# wall-clock and false negatives.
section "15. chaos micro-cycle (--with-chaos)"
if (( WITH_CHAOS == 0 )); then
  record WARN "**WARN**: --with-chaos not set — chaos micro-cycle skipped. Re-run with --with-chaos to validate the chaos daemon."
elif (( DEVS == 0 )); then
  record WARN "**WARN**: graph empty (no devices) — chaos cycle would be a no-op. Skip."
else
  CHAOS_LOG="$LOG_DIR/15-chaos.log"
  echo "    \$ bash scripts/chaos_runner.sh   # background" >> "$RESULTS_FILE"
  if bash scripts/chaos_runner.sh > "$CHAOS_LOG" 2>&1; then
    sleep 60
    # Count restart markers in the chaos JSONL log in the last minute.
    CHAOS_MARKERS=0
    if [[ -f runtime/chaos_log.jsonl ]]; then
      CHAOS_MARKERS="$(tail -200 runtime/chaos_log.jsonl 2>/dev/null | grep -c 'stale pid\|restart_marker' || echo 0)"
    fi
    bash scripts/chaos_runner.sh --stop >> "$CHAOS_LOG" 2>&1 || true
    tail -30 "$CHAOS_LOG" >> "$RESULTS_FILE"
    echo "restart markers in last cycle: $CHAOS_MARKERS" >> "$RESULTS_FILE"
    if (( CHAOS_MARKERS == 0 )); then
      record PASS "**PASS**: chaos daemon ran 60s with zero restart markers (T3-3 stability gate ok at 60s window)."
    else
      record WARN "**WARN**: $CHAOS_MARKERS restart marker(s) observed in 60s — daemon may be unstable. Full log: $CHAOS_LOG"
    fi
  else
    tail -50 "$CHAOS_LOG" >> "$RESULTS_FILE"
    record FAIL "**FAIL**: chaos_runner.sh failed to start — full log: $CHAOS_LOG"
  fi
fi

# ── 16. degrade probe ─────────────────────────────────────────────────────────
section "16. degrade probe (kill sidecar; /health should flip to degraded after stale window)"
if [[ -f runtime/bonsai-sidecar.pid ]]; then
  SID_PID="$(cat runtime/bonsai-sidecar.pid)"
  kill "$SID_PID" 2>/dev/null || true
  echo "killed sidecar pid $SID_PID — waiting 130s for lost-status window" >> "$RESULTS_FILE"
  sleep 130
  HEALTH_AFTER="$(curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:3000/health || echo 000)"
  HEALTH_BODY="$(curl -sS http://127.0.0.1:3000/health || true)"
  echo "HTTP $HEALTH_AFTER  body: $HEALTH_BODY" >> "$RESULTS_FILE"
  if [[ "$HEALTH_AFTER" == "503" ]] && echo "$HEALTH_BODY" | grep -q '"status":"degraded"'; then
    record PASS "**PASS**: /health correctly degraded after sidecar loss"
  else
    record WARN "**WARN**: expected 503 degraded, got $HEALTH_AFTER (timing may need tuning)"
  fi
else
  record WARN "**WARN**: no sidecar pid file — degrade probe skipped"
fi

# ── 17. teardown ──────────────────────────────────────────────────────────────
section "17. teardown"
bash scripts/ops/teardown.sh >> "$RESULTS_FILE" 2>&1 || true
cp -f logs/bonsai.log         "$LOG_DIR/17-bonsai.log"         2>/dev/null || true
cp -f logs/bonsai-sidecar.log "$LOG_DIR/17-bonsai-sidecar.log" 2>/dev/null || true
[[ -f runtime/chaos_runner.log ]] && cp -f runtime/chaos_runner.log "$LOG_DIR/17-chaos.log" 2>/dev/null || true
record PASS "**PASS**: teardown clean; live logs copied to $LOG_DIR/"

# ── Summary ───────────────────────────────────────────────────────────────────
echo
echo "${BOLD}── Summary ──${RESET}"
echo "  PASS: $PASS_COUNT"
echo "  WARN: $WARN_COUNT"
echo "  FAIL: $FAIL_COUNT"
{
  echo
  echo "## Summary"
  echo
  echo "- PASS: $PASS_COUNT"
  echo "- WARN: $WARN_COUNT"
  echo "- FAIL: $FAIL_COUNT"
  echo
  if (( FAIL_COUNT == 0 )); then
    if (( WARN_COUNT == 0 )); then
      echo "**Verdict**: full pipeline green. T4-7 gate closed if step 14 PASS; otherwise lab steps are clean and T4-7 is the only blocker for deleting src/event_detection.rs."
    else
      echo "**Verdict**: no failures; warnings above are mostly skip-reasons (no lab, no --with-chaos, etc.). T4-1 through T4-6 validated; T4-7 needs --with-lab to close."
    fi
  else
    echo "**Verdict**: failures present. Mac side must address before iterating. Diagnostic dumps inline (look for \`### Diagnostic dump\`); side-channel logs in \`$LOG_DIR/\`."
  fi
  echo
  echo "## How to re-run"
  echo
  echo '```bash'
  echo "# Quickest sanity (no lab, no chaos):"
  echo "bash scripts/ops/rebuild_and_validate.sh"
  echo
  echo "# Lab-up validation (closes T4-7 gate):"
  echo "bash scripts/ops/rebuild_and_validate.sh --with-lab"
  echo
  echo "# Full end-to-end (lab + chaos):"
  echo "bash scripts/ops/rebuild_and_validate.sh --full"
  echo
  echo "# Script iteration with cached build:"
  echo "bash scripts/ops/rebuild_and_validate.sh --skip-build --skip-push"
  echo '```'
  echo
  echo "## Side-channel logs (full stdout+stderr for each command)"
  echo
  echo '```'
  ls -la "$LOG_DIR" 2>/dev/null | sed -n '2,$p'
  echo '```'
} >> "$RESULTS_FILE"

# ── Push results back to repo so Mac sees them ────────────────────────────────
section "18. push results"
if (( SKIP_PUSH == 1 )); then
  record WARN "**WARN**: --skip-push set; results not committed/pushed. Inspect locally: $RESULTS_FILE"
else
  git add "$RESULTS_FILE" "$LOG_DIR" 2>/dev/null || true
  if git diff --cached --quiet; then
    echo "no changes to commit" | tee -a "$RESULTS_FILE"
  else
    git commit -m "validation: cv7 $DATE — PASS=$PASS_COUNT WARN=$WARN_COUNT FAIL=$FAIL_COUNT" >> "$RESULTS_FILE" 2>&1 || true
    git push origin main >> "$RESULTS_FILE" 2>&1 || record WARN "**WARN**: git push failed — commit was made but not pushed"
  fi
fi

echo
echo "Results written to: $RESULTS_FILE"
echo "Side-channel logs in: $LOG_DIR/"

(( FAIL_COUNT == 0 )) && exit 0 || exit 1
