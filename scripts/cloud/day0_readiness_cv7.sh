#!/usr/bin/env bash
# CV7 T7-1 — Day-0 readiness for the CV7-revised 7-day handoff proof.
#
# Wraps scripts/cloud/day0_readiness.sh (checks 1-7) and adds the four CV7
# gates (checks 8-11):
#   8.  Mac is clean (only meaningful if run pre-push from Mac — laptop/cloud
#       always passes this trivially; we still log it for the record)
#   9.  Chaos stability: zero restart markers in 1 hour
#   10. Latest CI binary installed (/usr/local/lib/bonsai/current matches HEAD)
#   11. Rules sidecar bound + healthy (/api/sidecars; /health == ok)
#
# Pass = all 11 checks PASS. Only then is the clock-start file written.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

GREEN=$'\033[0;32m'; RED=$'\033[0;31m'; YELLOW=$'\033[0;33m'; BOLD=$'\033[1m'; RESET=$'\033[0m'

ENV_DETECTED="$(bash scripts/dev/whichenv.sh 2>/dev/null || echo unknown)"
if [[ "$ENV_DETECTED" == "mac-dev" ]]; then
  echo "${RED}Refused.${RESET} Run on ubuntu-ops or cloud-ops." >&2
  exit 2
fi

CHECK_ONLY=false
FORCE=false
SKIP_CHAOS_SMOKE=false
for arg in "$@"; do
  case "$arg" in
    --check-only)        CHECK_ONLY=true ;;
    --force)             FORCE=true ;;
    --skip-chaos-smoke)  SKIP_CHAOS_SMOKE=true ;;
    --help|-h)
      cat <<EOF
Usage: $0 [--check-only] [--force] [--skip-chaos-smoke]

  --check-only          Run all checks but do not write the clock-start file.
  --force               Overwrite an existing clock-start file.
  --skip-chaos-smoke    Skip the 1-hour chaos stability check (CV7 check #9).
                        Use only when an equivalent smoke has already been
                        captured into docs/test_results/.
EOF
      exit 0
      ;;
    *) echo "Unknown arg: $arg" >&2; exit 1 ;;
  esac
done

FAILS=0
pass() { printf "  ${GREEN}✓${RESET} %s\n" "$1"; }
warn() { printf "  ${YELLOW}⚠${RESET} %s\n" "$1"; }
fail() { printf "  ${RED}✗${RESET} %s\n" "$1"; FAILS=$((FAILS+1)); }

echo "${BOLD}── CV7 Day-0 readiness ────────────────────────────────${RESET}"
echo

# ── Checks 1-7 via the existing script ────────────────────────────────────────
echo "${BOLD}[1-7] Base Day-0 checks (legacy)${RESET}"
if bash scripts/cloud/day0_readiness.sh --check-only; then
  pass "Base Day-0 checks 1-7 passed"
else
  fail "Base Day-0 checks 1-7 FAILED — re-run scripts/cloud/day0_readiness.sh --check-only for details"
fi
echo

# ── Check 8: Mac cleanliness recorded ─────────────────────────────────────────
echo "${BOLD}[8] Mac cleanliness (informational on ops env)${RESET}"
warn "Skipped: this check belongs to the Mac pre-push step; passes trivially on ops env."
echo

# ── Check 9: Chaos stability smoke ────────────────────────────────────────────
echo "${BOLD}[9] Chaos stability (1h smoke)${RESET}"
if [[ "$SKIP_CHAOS_SMOKE" == "true" ]]; then
  warn "Skipped (--skip-chaos-smoke). Ensure equivalent smoke is captured in docs/test_results/."
else
  if WALL_SECS="${CV7_SMOKE_SECS:-3600}" bash scripts/smoke/smoke_chaos_stability.sh; then
    pass "Chaos stability: zero restart markers in window"
  else
    fail "Chaos stability FAILED — investigate runtime/chaos_log.jsonl"
  fi
fi
echo

# ── Check 10: Latest CI binary installed ──────────────────────────────────────
echo "${BOLD}[10] Latest CI binary installed${RESET}"
HEAD_SHA="$(git rev-parse HEAD)"
INSTALLED_SHA="$(cat /usr/local/lib/bonsai/current 2>/dev/null || echo '')"
if [[ -z "$INSTALLED_SHA" ]]; then
  fail "No installed binary recorded at /usr/local/lib/bonsai/current. Run scripts/ops/pull_and_install.sh (laptop) or scripts/cloud/pull_and_install.sh (cloud)."
elif [[ "$INSTALLED_SHA" != "$HEAD_SHA" ]]; then
  fail "Installed binary SHA ($INSTALLED_SHA) does not match repo HEAD ($HEAD_SHA). Pull-and-install needed."
else
  pass "Installed binary matches HEAD ($HEAD_SHA)"
fi
echo

# ── Check 11: Sidecar bound + /health == ok ──────────────────────────────────
echo "${BOLD}[11] Rules sidecar bound and /health == ok${RESET}"
SIDECARS_JSON="$(curl -fsS http://127.0.0.1:3000/api/sidecars 2>/dev/null || echo '')"
if [[ -z "$SIDECARS_JSON" ]]; then
  fail "/api/sidecars not reachable. Start bonsai first."
else
  HAVE_RULES=$(echo "$SIDECARS_JSON" | jq -r '[.sidecars[] | select(.kind=="rules" and .status=="healthy")] | length' 2>/dev/null || echo 0)
  if [[ "$HAVE_RULES" -ge 1 ]]; then
    pass "≥1 healthy rules sidecar registered"
  else
    fail "No healthy rules sidecar registered. Start scripts/ops/start_bonsai_with_sidecar.sh (laptop) or systemctl status bonsai-rules-sidecar (cloud)."
  fi
fi
HEALTH_JSON="$(curl -fsS http://127.0.0.1:3000/health 2>/dev/null || echo '{}')"
HEALTH_STATUS="$(echo "$HEALTH_JSON" | jq -r '.status // "unknown"' 2>/dev/null || echo unknown)"
if [[ "$HEALTH_STATUS" == "ok" ]]; then
  pass "/health == ok"
else
  fail "/health == $HEALTH_STATUS (expected ok). Body: $HEALTH_JSON"
fi
echo

# ── Verdict ───────────────────────────────────────────────────────────────────
if (( FAILS > 0 )); then
  echo "${RED}${BOLD}FAIL${RESET} — $FAILS CV7 readiness gate(s) did not pass."
  echo "Do NOT start the 7-day clock until all gates pass."
  exit 1
fi

echo "${GREEN}${BOLD}PASS${RESET} — all 11 CV7 readiness gates are green."

if [[ "$CHECK_ONLY" == "true" ]]; then
  echo "(--check-only: not writing clock-start file)"
  exit 0
fi

RESULT_DIR="$REPO_ROOT/runtime/driver_results"
START_FILE="$RESULT_DIR/handoff_start_cv7.txt"
mkdir -p "$RESULT_DIR"

if [[ -f "$START_FILE" && "$FORCE" != "true" ]]; then
  echo "${YELLOW}Existing clock-start file at $START_FILE${RESET}"
  echo "Pass --force to overwrite."
  exit 0
fi

cat > "$START_FILE" <<EOF
CV7 7-day handoff clock started.
Started at: $(date -u +%Y-%m-%dT%H:%M:%SZ)
SHA at start: $(git rev-parse HEAD)
Environment: $ENV_DETECTED
Closure due: $(date -u -d '+7 days' +%Y-%m-%d 2>/dev/null || date -u +%Y-%m-%d)
EOF
echo
cat "$START_FILE"
