#!/usr/bin/env bash
# build_lane_gc_selftest.sh — failure-injection coverage for
# scripts/ci/build-lane-gc.sh (BN-019 artifact retention).
#
# Exercises the REAL script against tiny synthetic lane dirs — no Rust is
# compiled, no real lane is touched. Covers: dry-run vs --apply, lane lock
# contention, unsafe-path refusal, ci-profile protection, stale-unit
# probe semantics, protected-file preservation, failure handling and
# disk-recovery reporting.
#
# Usage: scripts/ci/build_lane_gc_selftest.sh
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GC="$REPO_ROOT/scripts/ci/build-lane-gc.sh"
WORK="$(mktemp -d /tmp/build-lane-gc-selftest.XXXXXX)"
PASS=0; FAIL=0
trap 'rm -rf "$WORK"' EXIT

ok()  { PASS=$((PASS+1)); echo "PASS  $1"; }
bad() { FAIL=$((FAIL+1)); echo "FAIL  $1"; }

# ── fixture: synthetic lane ────────────────────────────────────────────
# $1 = lane dir. Creates .lane.lock/.lane-compat.json plus debug/ and ci/
# profile dirs with deps artifacts, fingerprint units, incremental cache,
# dwp files and protected non-artifact files.
make_lane() {
  local L="$1"
  mkdir -p "$L/debug/deps" "$L/debug/.fingerprint" "$L/debug/incremental/unit1" \
           "$L/ci/deps" "$L/ci/.fingerprint" "$L/ci/incremental/unit1"
  : > "$L/.lane.lock"; echo '{}' > "$L/.lane-compat.json"
  # debug profile: two generations of one test target + dep artifacts
  echo x > "$L/debug/deps/yang_catalogue-aaaaaaaabbbbbbbb"
  echo x > "$L/debug/deps/yang_catalogue-aaaaaaaabbbbbbbb.d"
  echo x > "$L/debug/deps/yang_catalogue-ccccccccdddddddd"
  echo x > "$L/debug/deps/yang_catalogue-ccccccccdddddddd.dwp"
  echo obj > "$L/debug/deps/yang_catalogue-ccccccccdddddddd.0.rcgu.o"
  echo obj > "$L/debug/deps/yang_catalogue-ccccccccdddddddd.0.rcgu.dwo"
  echo x > "$L/debug/deps/libserde-eeeeeeeeeeeeeeee.rlib"
  echo x > "$L/debug/deps/libserde-eeeeeeeeeeeeeeee.rmeta"
  echo x > "$L/debug/deps/README-not-an-artifact"
  mkdir -p "$L/debug/.fingerprint/bonsai-aaaaaaaabbbbbbbb" \
           "$L/debug/.fingerprint/bonsai-ccccccccdddddddd" \
           "$L/debug/.fingerprint/bonsai-ffffffffffffffff"
  echo x > "$L/debug/.fingerprint/bonsai-aaaaaaaabbbbbbbb/lib-bonsai"
  echo x > "$L/debug/.fingerprint/bonsai-ccccccccdddddddd/test-yang_catalogue"
  echo x > "$L/debug/.fingerprint/bonsai-ffffffffffffffff/lib-bonsai"
  echo x > "$L/debug/incremental/unit1/o.o"
  echo x > "$L/debug/bonsai.dwp"
  # ci profile: one small artifact set (protected unless --include-ci)
  echo x > "$L/ci/deps/yang_catalogue-1111111122222222"
  echo x > "$L/ci/deps/yang_catalogue-1111111122222222.dwp"
  mkdir -p "$L/ci/.fingerprint/bonsai-1111111122222222"
  echo x > "$L/ci/.fingerprint/bonsai-1111111122222222/test-yang_catalogue"
  echo x > "$L/ci/incremental/unit1/o.o"
}

# ── 1. report is dry-run and parses ────────────────────────────────────
L1="$WORK/lane1"; make_lane "$L1"
R1="$(bash "$GC" report --dir "$L1" 2>/dev/null)"
echo "$R1" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["tool"]=="build-lane-gc.sh"; assert any(p["dir"]=="debug" for p in d["profiles"]); assert any(p["dir"]=="ci" for p in d["profiles"])' \
  && ok "report emits JSON inventory for all profiles" || bad "report emits JSON inventory"
[ -f "$L1/debug/deps/yang_catalogue-aaaaaaaabbbbbbbb" ] \
  && ok "report deletes nothing" || bad "report deletes nothing"

# ── 2. unsafe paths refused ────────────────────────────────────────────
for badpath in / "$HOME" "$REPO_ROOT" "$WORK/not-a-lane"; do
  if bash "$GC" report --dir "$badpath" >/dev/null 2>&1; then
    bad "report refuses unsafe path $badpath"
  else
    ok "report refuses unsafe path $(basename "$badpath")"
  fi
done

# ── 3. clean dry-run vs --apply (intermediates) ────────────────────────
L3="$WORK/lane3"; make_lane "$L3"
OUT3="$(bash "$GC" clean --dir "$L3" --intermediates 2>&1)"
echo "$OUT3" | grep -q "DRY-RUN" && ok "clean without --apply is dry-run" || bad "clean without --apply is dry-run"
[ -f "$L3/debug/deps/yang_catalogue-ccccccccdddddddd.0.rcgu.o" ] \
  && ok "dry-run deletes nothing" || bad "dry-run deletes nothing"
bash "$GC" clean --dir "$L3" --intermediates --apply >/dev/null 2>&1
[ ! -f "$L3/debug/deps/yang_catalogue-ccccccccdddddddd.0.rcgu.o" ] \
  && [ ! -f "$L3/debug/deps/yang_catalogue-ccccccccdddddddd.0.rcgu.dwo" ] \
  && ok "--apply removes intermediate objects" || bad "--apply removes intermediate objects"
[ -f "$L3/debug/deps/yang_catalogue-ccccccccdddddddd" ] \
  && ok "intermediates clean preserves final artifacts" || bad "intermediates clean preserves final artifacts"
[ -f "$L3/debug/deps/README-not-an-artifact" ] \
  && ok "non-artifact files preserved" || bad "non-artifact files preserved"
[ -f "$L3/.lane.lock" ] && [ -f "$L3/.lane-compat.json" ] \
  && ok "lane marker files preserved" || bad "lane marker files preserved"

# ── 4. ci profile protection ───────────────────────────────────────────
[ -f "$L3/ci/deps/yang_catalogue-1111111122222222" ] \
  && ok "ci profile untouched without --include-ci" || bad "ci profile untouched without --include-ci"
bash "$GC" clean --dir "$L3" --dwp --incremental --apply --include-ci >/dev/null 2>&1
[ ! -f "$L3/ci/deps/yang_catalogue-1111111122222222.dwp" ] \
  && [ ! -d "$L3/ci/incremental" ] \
  && ok "--include-ci permits ci cleanup" || bad "--include-ci permits ci cleanup"
[ -f "$L3/ci/deps/yang_catalogue-1111111122222222" ] \
  && ok "ci executables preserved (only dwp/incremental removed)" || bad "ci executables preserved"
[ ! -f "$L3/debug/bonsai.dwp" ] && [ ! -d "$L3/debug/incremental" ] \
  && ok "dwp+incremental removed on debug" || bad "dwp+incremental removed on debug"

# ── 5. manifest + disk recovery reporting ──────────────────────────────
MAN="$(ls -t "${XDG_CACHE_HOME:-$HOME/.cache}/bonsai-ci/gc-logs"/gc-*.jsonl 2>/dev/null | head -1)"
[ -n "$MAN" ] && grep -q '"disk_recovered_kib"' "$MAN" \
  && ok "manifest records disk recovery" || bad "manifest records disk recovery"
grep -q '"action": "removed"' "$MAN" && ok "manifest records removed paths" || bad "manifest records removed paths"

# ── 6. lock contention: clean waits for the lane lock ──────────────────
L6="$WORK/lane6"; make_lane "$L6"
( exec 8>"$L6/.lane.lock"; flock -x 8; sleep 3 ) &
HOLDER=$!
sleep 0.5
START=$(date +%s)
bash "$GC" clean --dir "$L6" --intermediates --apply >/dev/null 2>&1
ELAPSED=$(( $(date +%s) - START ))
wait "$HOLDER" 2>/dev/null || true
[ "$ELAPSED" -ge 2 ] && ok "clean blocks on held lane lock (${ELAPSED}s)" \
  || bad "clean blocks on held lane lock (${ELAPSED}s)"

# ── 7. stale-units: probe-defined live set ─────────────────────────────
L7="$WORK/lane7"; make_lane "$L7"
# Probe reports ONLY the cccccccc generation as current (plus libserde).
# Emit realpath'd filenames so the live-set match works.
PROBE="python3 -c 'import os,json,sys
for p in sys.argv[1:]:
    print(json.dumps({\"reason\":\"compiler-artifact\",\"filenames\":[os.path.realpath(p)]}))' \
$L7/debug/deps/yang_catalogue-ccccccccdddddddd \
$L7/debug/deps/yang_catalogue-ccccccccdddddddd.dwp \
$L7/debug/deps/libserde-eeeeeeeeeeeeeeee.rlib \
$L7/debug/deps/libserde-eeeeeeeeeeeeeeee.rmeta"
bash "$GC" clean --dir "$L7" --profile dev --stale-units --apply \
     --probe-cmd "$PROBE" >/dev/null 2>&1
[ ! -f "$L7/debug/deps/yang_catalogue-aaaaaaaabbbbbbbb" ] \
  && [ ! -d "$L7/debug/.fingerprint/bonsai-aaaaaaaabbbbbbbb" ] \
  && ok "stale-units removes old generation + fingerprint" || bad "stale-units removes old generation + fingerprint"
[ -f "$L7/debug/deps/yang_catalogue-ccccccccdddddddd" ] \
  && [ -d "$L7/debug/.fingerprint/bonsai-ccccccccdddddddd" ] \
  && ok "stale-units preserves probe-live generation" || bad "stale-units preserves probe-live generation"
# fingerprint unit with NO deps artifact and not in live set is preserved
# (only units owning stale deps artifacts are removed — conservative).
[ -d "$L7/debug/.fingerprint/bonsai-ffffffffffffffff" ] \
  && ok "orphan fingerprint without stale deps artifact preserved" || bad "orphan fingerprint without stale deps artifact preserved"
# ci untouched (no --include-ci, profile=dev anyway)
[ -f "$L7/ci/deps/yang_catalogue-1111111122222222" ] \
  && ok "stale-units scoped to selected profile" || bad "stale-units scoped to selected profile"

# ── 8. interrupted/re-run idempotence ──────────────────────────────────
OUT8="$(bash "$GC" clean --dir "$L7" --profile dev --stale-units --apply \
     --probe-cmd "$PROBE" 2>&1)"
echo "$OUT8" | grep -qi "removed=0" \
  && ok "re-run after completed clean is idempotent" || bad "re-run after completed clean is idempotent"

# ── 9. failure handling: undeletable file does not abort the run ───────
L9="$WORK/lane9"; make_lane "$L9"
chmod 555 "$L9/debug/deps"
bash "$GC" clean --dir "$L9" --intermediates --apply >/dev/null 2>&1
chmod 755 "$L9/debug/deps"
MAN9="$(ls -t "${XDG_CACHE_HOME:-$HOME/.cache}/bonsai-ci/gc-logs"/gc-*.jsonl | head -1)"
grep -q '"action": "error:' "$MAN9" \
  && ok "undeletable file recorded as error, run continues" || bad "undeletable file recorded as error"

echo
echo "build_lane_gc_selftest: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
