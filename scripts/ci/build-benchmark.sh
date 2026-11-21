#!/usr/bin/env bash
# build-benchmark.sh — safe, non-destructive build performance measurement
# for the Bonsai Rust workspace (BN-019 build optimization).
#
# Replaces scripts/build_bench.sh, which ran `cargo clean`, touched tracked
# source files and `docker build --no-cache` on every invocation — unsuitable
# for routine development on a shared, disk-constrained host.
#
# DEFAULT MODE IS NON-DESTRUCTIVE:
#   * Measures WARM-build activity on the trusted shared build lane
#     (scripts/ci/build-lane.sh) — no `cargo clean`, no source modification,
#     no Docker builds, no pruning of any kind.
#   * The only state touched is Cargo's own incremental/fingerprint cache,
#     exactly as a normal `cargo test` would touch it.
#
# What it measures (per phase, wall-clock seconds):
#   env       toolchain/profile/lbug/sccache/disk snapshot
#   check     cargo check -p bonsai --lib           (typecheck)
#   libbuild  cargo test -p bonsai --lib --no-run   (libtest compile+link)
#   libtest   cargo test -p bonsai --lib            (test execution, warm)
#   itbuild   cargo test -p bonsai --test T --no-run (integration compile+link)
#   itest     cargo test -p bonsai --test T         (test execution, warm)
#   itedit    touch tests/T.rs + rebuild --no-run   (test-file-only edit loop;
#             mtime-only touch, file CONTENT is never modified)
#
# Output:
#   * JSON detail -> docs/build-times/benchmarks/<utc>-<label>.json
#   * One CSV row per phase appended to docs/build-times/runs.csv
#     (existing schema: date,command,scope,wall_seconds,result,notes)
#
# COLD MODE (explicit opt-in only):
#   --cold --isolated-target DIR [--lbug-native DIR]
#   Builds the test matrix from scratch into a NEW, quota-bounded target
#   directory. Never touches the shared lane or any existing target dir.
#   Check `df -h` first — a cold bonsai test build needs tens of GB.
#   If the shared lane uses a prebuilt lbug bundle, --lbug-native is required
#   (otherwise the cold build would attempt a 35-77 min bundled CMake build);
#   pass --bundled-ok to explicitly accept a bundled cold build.
#
# Usage:
#   scripts/ci/build-benchmark.sh [--profile ci|dev] [--label NAME]
#       [--test-target NAME] [--skip-tests]
#       [--lbug-native DIR] [--bundled-ok]
#       [--cold --isolated-target DIR]
#       [--out-dir DIR] [--lane DIR]
#
# Environment:
#   BONSAI_BUILD_LANE  shared lane dir (default ~/.cache/bonsai-ci/build-lane)
set -euo pipefail
cd "$(dirname "$0")/../.."
ROOT="$(pwd)"

PROFILE="ci"
LABEL="manual"
TEST_TARGET="bmp_rfc_contract"
SKIP_TESTS=0
LBUG_NATIVE=""
BUNDLED_OK=0
COLD=0
ISOLATED_TARGET=""
OUT_DIR="docs/build-times/benchmarks"
LANE="${BONSAI_BUILD_LANE:-$HOME/.cache/bonsai-ci/build-lane}"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --profile)          PROFILE="$2"; shift 2 ;;
    --label)            LABEL="$2"; shift 2 ;;
    --test-target)      TEST_TARGET="$2"; shift 2 ;;
    --skip-tests)       SKIP_TESTS=1; shift ;;
    --lbug-native)      LBUG_NATIVE="$2"; shift 2 ;;
    --bundled-ok)       BUNDLED_OK=1; shift ;;
    --cold)             COLD=1; shift ;;
    --isolated-target)  ISOLATED_TARGET="$2"; shift 2 ;;
    --out-dir)          OUT_DIR="$2"; shift 2 ;;
    --lane)             LANE="$2"; shift 2 ;;
    -h|--help)          sed -n '2,62p' "$0"; exit 0 ;;
    *) echo "build-benchmark: unknown arg $1" >&2; exit 2 ;;
  esac
done

need() { command -v "$1" >/dev/null 2>&1 || { echo "build-benchmark: missing tool: $1" >&2; exit 2; }; }
need cargo; need rustc; need python3; need df; need sccache

if [ "$COLD" = "1" ]; then
  [ -n "$ISOLATED_TARGET" ] || { echo "build-benchmark: --cold requires --isolated-target DIR" >&2; exit 2; }
  case "$ISOLATED_TARGET" in
    "$LANE"|"$LANE"/*|"$ROOT"/target|"$ROOT"/target/*|"")
      echo "build-benchmark: GATE FAIL — isolated target must not overlap the shared lane or repo target" >&2; exit 1 ;;
  esac
  AVAIL_KB=$(df -k --output=avail "$(dirname "$ISOLATED_TARGET")" 2>/dev/null | tail -1 || df -k --output=avail / | tail -1)
  [ "${AVAIL_KB:-0}" -gt 40000000 ] || { echo "build-benchmark: GATE FAIL — <40GB free for cold build; refusing (disk floor)" >&2; exit 1; }
  TARGET_MODE="isolated"
else
  TARGET_MODE="lane"
fi

TS="$(date -u +%Y%m%dT%H%M%SZ)"
JSON_OUT="$OUT_DIR/${TS}-${LABEL//[^A-Za-z0-9_.-]/_}.json"
mkdir -p "$OUT_DIR"
RUNS_CSV="docs/build-times/runs.csv"
PHASES_FILE="$(mktemp)"
trap 'rm -f "$PHASES_FILE"' EXIT

log() { printf '[bench] %s\n' "$*" >&2; }

# ── peak RSS sampler: tracks the process tree rooted at $1 ─────────────
# Writes max tree RSS (KiB) to $2. Polls every 0.25s.
start_rss_sampler() {
  local pid="$1" out="$2"
  python3 - "$pid" "$out" <<'PY' &
import os, sys, time
root = int(sys.argv[1]); out = sys.argv[2]
page = os.sysconf('SC_PAGE_SIZE')
peak = 0
def tree_rss(pid):
    children = {}
    for p in os.listdir('/proc'):
        if not p.isdigit(): continue
        try:
            with open('/proc/%s/stat' % p, 'rb') as f:
                parts = f.read().rsplit(b')', 1)[1].split()
            children.setdefault(int(parts[1]), []).append(int(p))
        except Exception:
            pass
    total = 0; stack = [pid]; seen = set()
    while stack:
        cur = stack.pop()
        if cur in seen: continue
        seen.add(cur)
        try:
            with open('/proc/%s/stat' % cur, 'rb') as f:
                parts = f.read().rsplit(b')', 1)[1].split()
            total += int(parts[21]) * page // 1024
        except Exception:
            pass
        stack.extend(children.get(cur, ()))
    return total
while True:
    try:
        v = tree_rss(root)
        if v > peak: peak = v
    except Exception:
        pass
    try:
        os.kill(root, 0)
    except OSError:
        try:
            v = tree_rss(root)
            if v > peak: peak = v
        except Exception:
            pass
        break
    time.sleep(0.25)
open(out, 'w').write(str(peak))
PY
  echo $!
}

LAST_WALL=0; LAST_RSS=0; LAST_RC=0
# timed_run <command...> — wall + peak tree RSS of the command.
timed_run() {
  local t0 t1 rssf rss cmdpid sampler
  t0=$(date +%s.%N)
  rssf="$(mktemp)"
  "$@" &
  cmdpid=$!
  sampler=$(start_rss_sampler "$cmdpid" "$rssf")
  set +e; wait "$cmdpid"; LAST_RC=$?; set -e
  wait "$sampler" 2>/dev/null || true
  t1=$(date +%s.%N)
  rss=$(cat "$rssf" 2>/dev/null || echo 0); rm -f "$rssf"
  LAST_WALL=$(python3 -c "print(f'{$t1-$t0:.2f}')")
  LAST_RSS="${rss:-0}"
  log "phase: ${LAST_WALL}s wall, peak_tree_rss=${LAST_RSS}KiB rc=$LAST_RC :: $*"
}

csv_escape() { printf '%s' "$1" | tr ',' ';' | tr -d '"'; }
record_phase() {
  local phase="$1" scope="$2" result="$3" notes="$4"
  local now; now="$(date -u +%Y-%m-%d)"
  printf '%s,%s,%s,%s,%s,"%s"\n' \
    "$now" "$(csv_escape "build-benchmark $LABEL $phase")" \
    "$(csv_escape "$scope")" "$LAST_WALL" "$result" "$(csv_escape "$notes")" \
    >> "$RUNS_CSV"
  PHASE="$phase" SCOPE="$scope" RESULT="$result" NOTES="$notes" \
  WALL="$LAST_WALL" RSS="$LAST_RSS" python3 - "$PHASES_FILE" <<'PY'
import json, os, sys
rec = {"phase": os.environ["PHASE"], "scope": os.environ["SCOPE"],
       "wall_seconds": float(os.environ["WALL"]),
       "peak_tree_rss_kib": int(float(os.environ["RSS"] or 0)),
       "result": os.environ["RESULT"], "notes": os.environ["NOTES"]}
with open(sys.argv[1], "a") as f:
    f.write(json.dumps(rec) + "\n")
PY
}

sccache_stats() { sccache --show-stats 2>/dev/null | grep -E "Compile requests|Cache hits|Cache misses|Cache size" | tr '\n' '|' || true; }

# ── environment snapshot ────────────────────────────────────────────────
log "snapshot: profile=$PROFILE target_mode=$TARGET_MODE label=$LABEL"
RUSTC_VV="$(rustc -vV 2>/dev/null | tr '\n' ';')"
CARGO_V="$(cargo --version 2>/dev/null)"
HOST_UNAME="$(uname -srmo)"
NPROC="$(nproc)"
MEMINFO="$(free -m | awk '/^Mem:/{print $2"MB total "$7"MB avail"}')"
DF_BEFORE="$(df -h "$ROOT" | awk 'NR==2{print $4" avail on "$6}')"
SCCACHE_BEFORE="$(sccache_stats)"

# Lane/lbug identity (structured read, no eval)
LBUG_MODE="unknown"; LBUG_BUNDLE=""
if [ -f "$LANE/.lane-compat.json" ]; then
  LBUG_MODE="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("lbug_mode",""))' "$LANE/.lane-compat.json" 2>/dev/null || echo "")"
  LBUG_BUNDLE="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("lbug_native_id",""))' "$LANE/.lane-compat.json" 2>/dev/null || echo "")"
fi
if [ -n "$LBUG_NATIVE" ]; then
  LBUG_MODE="prebuilt"
  LBUG_BUNDLE="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]+"/manifest.json"))["bundle_id"])' "$LBUG_NATIVE" 2>/dev/null || echo "")"
fi

# Cold mode safety: a prebuilt lane means a bundled cold build would run the
# 35-77 min lbug CMake build — refuse unless explicitly acknowledged.
if [ "$TARGET_MODE" = "isolated" ] && [ "$LBUG_MODE" = "prebuilt" ] && [ -z "$LBUG_NATIVE" ] && [ "$BUNDLED_OK" != "1" ]; then
  echo "build-benchmark: GATE FAIL — shared lane uses prebuilt lbug; pass --lbug-native DIR or --bundled-ok" >&2
  exit 1
fi

# Isolated mode + prebuilt bundle: replicate build-lane.sh's verification and
# env derivation (same guard scripts — single validation path), and hold the
# bundle shared lock for the script's duration so archives cannot be replaced
# mid-build.
if [ -n "$LBUG_NATIVE" ] && [ "$TARGET_MODE" = "isolated" ]; then
  bash scripts/ci/build-lane-guard.sh sanity
  _native_real="$(cd "$LBUG_NATIVE" 2>/dev/null && pwd -P)" \
    || { echo "build-benchmark: bad --lbug-native $LBUG_NATIVE" >&2; exit 2; }
  [ -f "$_native_real/manifest.json" ] || { echo "build-benchmark: $_native_real/manifest.json missing" >&2; exit 2; }
  bash scripts/ci/lbug-native-prepare.sh --verify "$_native_real" >/dev/null \
    || { echo "build-benchmark: GATE FAIL — lbug-native dir failed verification" >&2; exit 1; }
  exec 7>"$_native_real.lock"; flock -s 7
  LBUG_NATIVE="$_native_real"
  while IFS=$'\t' read -r _k _v; do
    [ -n "$_k" ] && export "$_k=$_v"
  done < <(bash scripts/ci/build-lane-guard.sh env --dir "$_native_real")
fi

# ── build invocation wrapper ────────────────────────────────────────────
# lane mode: route through build-lane.sh (flock + compat guard + lbug env).
# isolated mode: plain cargo with CARGO_TARGET_DIR (lbug env NOT set here —
# callers needing prebuilt lbug in isolated mode must export LBUG_* themselves;
# build-lane-guard sanity rules still apply inside build-lane.sh only).
run_cargo() {
  if [ "$TARGET_MODE" = "lane" ]; then
    local args=(--dir "$LANE" --label "benchmark:$LABEL")
    [ -n "$LBUG_NATIVE" ] && args+=(--lbug-native "$LBUG_NATIVE")
    bash scripts/ci/build-lane.sh "${args[@]}" -- "$@"
  else
    CARGO_TARGET_DIR="$ISOLATED_TARGET" "$@"
  fi
}

# ── measurements ───────────────────────────────────────────────────────
BENCH_T0=$(date +%s)

timed_run true
record_phase env "workspace" "pass" "snapshot only; mode=$TARGET_MODE profile=$PROFILE"

timed_run run_cargo cargo check -p bonsai --lib --profile "$PROFILE" --offline
if [ "$LAST_RC" = 0 ]; then record_phase check "bonsai --lib ($PROFILE)" "pass" "typecheck"; else record_phase check "bonsai --lib ($PROFILE)" "fail rc=$LAST_RC" "typecheck"; fi

if [ "$SKIP_TESTS" = 0 ]; then
  timed_run run_cargo cargo test -p bonsai --lib --profile "$PROFILE" --no-run --offline --timings
  if [ "$LAST_RC" = 0 ]; then record_phase libbuild "bonsai --lib test ($PROFILE)" "pass" "compile+link"; else record_phase libbuild "bonsai --lib test ($PROFILE)" "fail rc=$LAST_RC" "compile+link"; fi

  timed_run run_cargo cargo test -p bonsai --lib --profile "$PROFILE" --offline
  if [ "$LAST_RC" = 0 ]; then record_phase libtest "bonsai --lib tests ($PROFILE)" "pass" "execution"; else record_phase libtest "bonsai --lib tests ($PROFILE)" "fail rc=$LAST_RC" "execution"; fi

  timed_run run_cargo cargo test -p bonsai --test "$TEST_TARGET" --profile "$PROFILE" --no-run --offline --timings
  if [ "$LAST_RC" = 0 ]; then record_phase itbuild "bonsai --test $TEST_TARGET ($PROFILE)" "pass" "compile+link"; else record_phase itbuild "bonsai --test $TEST_TARGET ($PROFILE)" "fail rc=$LAST_RC" "compile+link"; fi

  timed_run run_cargo cargo test -p bonsai --test "$TEST_TARGET" --profile "$PROFILE" --offline
  if [ "$LAST_RC" = 0 ]; then record_phase itest "bonsai --test $TEST_TARGET ($PROFILE)" "pass" "execution"; else record_phase itest "bonsai --test $TEST_TARGET ($PROFILE)" "fail rc=$LAST_RC" "execution"; fi

  # Test-file-only edit simulation: mtime touch only — file CONTENT is never
  # modified. Rebuilds just that integration test crate + its link, which is
  # the dominant cost of the daily test-edit loop.
  if [ -f "tests/$TEST_TARGET.rs" ]; then
    touch "tests/$TEST_TARGET.rs"
    timed_run run_cargo cargo test -p bonsai --test "$TEST_TARGET" --profile "$PROFILE" --no-run --offline --timings
    if [ "$LAST_RC" = 0 ]; then record_phase itedit "bonsai --test $TEST_TARGET edit-loop ($PROFILE)" "pass" "test-file-only recompile+link"; else record_phase itedit "bonsai --test $TEST_TARGET edit-loop ($PROFILE)" "fail rc=$LAST_RC" "test-file-only recompile+link"; fi
  fi
fi

BENCH_T1=$(date +%s)
DF_AFTER="$(df -h "$ROOT" | awk 'NR==2{print $4" avail on "$6}')"
SCCACHE_AFTER="$(sccache_stats)"

PROFILE_SUBDIR="$PROFILE"
[ "$PROFILE" = "dev" ] && PROFILE_SUBDIR="debug"
ART_DIR="$LANE/$PROFILE_SUBDIR/deps"
[ "$TARGET_MODE" = "isolated" ] && ART_DIR="$ISOLATED_TARGET/$PROFILE_SUBDIR/deps"
EXE_INFO="[]"
if [ -d "$ART_DIR" ]; then
  EXE_INFO=$(python3 - "$ART_DIR" <<'PY'
import os, sys, json
d = sys.argv[1]; out = []
for f in sorted(os.listdir(d)):
    p = os.path.join(d, f)
    if os.path.isfile(p) and os.access(p, os.X_OK) and '.' not in f:
        out.append({"name": f, "bytes": os.path.getsize(p)})
print(json.dumps(out))
PY
)
fi
LANE_SIZE=""
if [ "$TARGET_MODE" = "lane" ] && [ -d "$LANE" ]; then
  LANE_SIZE=$(du -sb "$LANE" 2>/dev/null | awk '{print $1}')
fi

PHASES_JSON=$(python3 - "$PHASES_FILE" <<'PY'
import json, sys
recs = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
print(json.dumps(recs))
PY
)

python3 - "$JSON_OUT" <<PY
import json
report = {
  "tool": "build-benchmark.sh", "label": "$LABEL", "utc": "$TS",
  "toolchain": {"rustc": "$RUSTC_VV", "cargo": "$CARGO_V",
                "host": "$HOST_UNAME", "cpus": $NPROC, "mem": "$MEMINFO"},
  "profile": "$PROFILE", "cargo_profile_subdir": "$PROFILE_SUBDIR",
  "target_mode": "$TARGET_MODE", "lane": "$LANE",
  "isolated_target": "$ISOLATED_TARGET" if "$TARGET_MODE" == "isolated" else "",
  "lbug": {"mode": "$LBUG_MODE", "bundle_id": "$LBUG_BUNDLE"},
  "disk": {"before": "$DF_BEFORE", "after": "$DF_AFTER"},
  "sccache": {"before": "$SCCACHE_BEFORE", "after": "$SCCACHE_AFTER"},
  "lane_size_bytes": int("$LANE_SIZE" or 0),
  "wall_total_seconds": int($BENCH_T1 - $BENCH_T0),
  "phases": json.loads("""$PHASES_JSON"""),
  "executables": json.loads("""$EXE_INFO"""),
}
json.dump(report, open("$JSON_OUT", "w"), indent=2)
print("wrote $JSON_OUT")
PY

log "done: $JSON_OUT ; appended rows to $RUNS_CSV"
