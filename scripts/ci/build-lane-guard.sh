#!/usr/bin/env bash
# Shared build-lane / native-bundle compatibility guard (BN-019-B2).
# Single validation path used by BOTH build-lane.sh (host cargo) and
# run-local.sh (container gates) so the two entry points cannot drift.
#
# Subcommands:
#   check  --dir LANE --mode bundled|prebuilt [--native-id ID]
#          [--native-path PATH] [--reset]
#          Validates the lane compat marker (.lane-compat.json):
#            - lane identity + marker_version
#            - lbug_mode matches the requested mode
#            - prebuilt runs record the exact native bundle_id
#              (content fingerprint) — prebuilt A vs B is a mismatch
#            - toolchain drift is diagnostic-only (cargo fingerprints
#              already key on rustc version)
#          Exit 0 = proceed; 1 = hard fail; 3 = mode/fingerprint
#          mismatch that MAY be remediated by explicit artifact reset
#          (--reset-lbug-artifacts) or a different lane.
#          Fail-closed: malformed marker, missing marker on a POPULATED
#          lane, or unknown marker_version are hard failures.
#   record --dir LANE --mode M [--native-id ID] [--native-path P]
#          [--label L]
#          Atomically writes/updates .lane-compat.json for the lane.
#   env    --dir BUNDLE
#          Prints `export` lines for LBUG_LIBRARY_DIR/LBUG_INCLUDE_DIR/
#          CXXFLAGS derived from the bundle's manifest.json (validated
#          structured metadata — env.sh is never sourced by consumers).
#   sanity
#          Fails if the caller's environment already carries lbug-native
#          settings (LBUG_LIBRARY_DIR/LBUG_INCLUDE_DIR or stale
#          lbug-native -I flags in CXXFLAGS) that would silently leak
#          into a bundled build or cross-contaminate bundles.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PREPARE="$SCRIPT_DIR/lbug-native-prepare.sh"

cmd="${1:-}"; shift || true
case "$cmd" in check|record|env|sanity) ;; *) echo "usage: $0 check|record|env|sanity ..." >&2; exit 2 ;; esac

# ── sanity: inherited lbug-native env must not leak across modes ──────
if [ "$cmd" = "sanity" ]; then
  bad=0
  for v in LBUG_LIBRARY_DIR LBUG_INCLUDE_DIR; do
    if [ -n "${!v:-}" ]; then
      echo "build-lane-guard: GATE FAIL — $v is already set in the environment." >&2
      echo "  Bundled builds must not inherit prebuilt settings; prebuilt" >&2
      echo "  settings must come only from --lbug-native. Unset it or use" >&2
      echo "  the matching mode explicitly." >&2
      bad=1
    fi
  done
  # Stale generated include roots inside CXXFLAGS (e.g. a previously
  # sourced env.sh) would contaminate whichever mode runs next.
  if printf '%s' "${CXXFLAGS:-}" | grep -Eq 'include/(build-src|lbug-src|fastpfor|alp)([ /]|$)'; then
    echo "build-lane-guard: GATE FAIL — CXXFLAGS contains lbug-native include" >&2
    echo "  roots (likely a sourced env.sh). Unset CXXFLAGS or remove the" >&2
    echo "  stale -I flags before invoking the lane." >&2
    bad=1
  fi
  [ "$bad" = 0 ]
  exit $?
fi

# ── env: emit exports from validated manifest metadata ────────────────
if [ "$cmd" = "env" ]; then
  DIR=""
  while [ "$#" -gt 0 ]; do case "$1" in --dir) DIR="$2"; shift 2 ;; *) echo "usage: $0 env --dir BUNDLE" >&2; exit 2 ;; esac; done
  [ -n "$DIR" ] || { echo "env: --dir required" >&2; exit 2; }
  python3 - "$DIR/manifest.json" "$DIR" <<'PY'
import json, sys, os
mf = json.load(open(sys.argv[1]))
dest = os.path.realpath(sys.argv[2])
lib = os.path.join(dest, "lib")
inc = os.path.join(dest, "include", "lbug-src")
cxx = mf.get("cxxflags") or ""
# cxxflags were recorded with the publish-time dest path; if the bundle
# is consumed at a different realpath the recorded -I roots would be
# wrong — fail closed rather than emit stale flags.
if dest not in cxx:
    print(f"build-lane-guard: GATE FAIL — manifest cxxflags do not reference {dest}", file=sys.stderr)
    sys.exit(1)
print(f'export LBUG_LIBRARY_DIR="{lib}"')
print(f'export LBUG_INCLUDE_DIR="{inc}"')
print(f'export CXXFLAGS="{cxx} ${{CXXFLAGS:-}}"')
PY
  exit $?
fi

# ── check / record ────────────────────────────────────────────────────
DIR=""; MODE=""; NATIVE_ID=""; NATIVE_PATH=""; LABEL=""; RESET=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --dir) DIR="$2"; shift 2 ;;
    --mode) MODE="$2"; shift 2 ;;
    --native-id) NATIVE_ID="$2"; shift 2 ;;
    --native-path) NATIVE_PATH="$2"; shift 2 ;;
    --label) LABEL="$2"; shift 2 ;;
    --reset) RESET=1; shift ;;
    *) echo "build-lane-guard: unknown arg $1" >&2; exit 2 ;;
  esac
done
[ -n "$DIR" ] && [ -n "$MODE" ] || { echo "check/record: --dir and --mode required" >&2; exit 2; }
case "$MODE" in bundled|prebuilt) ;; *) echo "bad --mode $MODE" >&2; exit 2 ;; esac

MARKER="$DIR/.lane-compat.json"
LOCKFILE="$DIR/.lane.lock"

lane_populated() {
  [ -d "$DIR" ] || return 1
  # populated = anything other than the lock/marker/guard temp files
  find "$DIR" -mindepth 1 -maxdepth 1 \
    ! -name '.lane.lock' ! -name '.lane-compat.json' ! -name '*.tmp' \
    -print -quit 2>/dev/null | grep -q .
}

if [ "$cmd" = "check" ]; then
  python3 - "$DIR" "$MODE" "$NATIVE_ID" "$RESET" "$MARKER" <<'PY'
import json, os, sys

d, mode, native_id, reset, marker = sys.argv[1:6]
reset = reset == "1"
out = {"ok": False, "action": "fail", "reason": "", "warnings": []}

def finish(action, reason="", warnings=None):
    out.update(ok=(action == "proceed"), action=action, reason=reason,
               warnings=warnings or [])
    print(json.dumps(out))
    sys.exit(0 if action == "proceed" else (3 if action == "reset" else 1))

populated = os.path.isdir(d) and any(
    e not in (".lane.lock", ".lane-compat.json") and not e.endswith(".tmp")
    for e in os.listdir(d))

if not os.path.isfile(marker):
    if populated:
        finish("fail", f"lane {d} is populated but has no .lane-compat.json marker — cannot prove lbug mode/fingerprint; use a new lane dir or remove the lane contents deliberately")
    finish("proceed", "fresh lane")

try:
    raw = open(marker).read()
    m = json.loads(raw)
except Exception as e:
    finish("fail", f"lane marker {marker} is malformed: {e}")
if not isinstance(m, dict):
    finish("fail", "lane marker is not a JSON object")

mv = m.get("marker_version")
if mv is None:
    # legacy v0 markers (pre-guard) lack marker_version; accept only if
    # they carry the fields we can still verify, else fail closed.
    if "lbug_mode" not in m:
        finish("fail", "lane marker has no marker_version and no lbug_mode — unrecognized format")
    mv = 0
elif mv != 1:
    finish("fail", f"unsupported marker_version {mv}")

lane_mode = m.get("lbug_mode")
if lane_mode not in ("bundled", "prebuilt"):
    finish("fail", f"lane marker lbug_mode {lane_mode!r} unrecognized")

lane_native = m.get("lbug_native_id") or ""
warnings = []

if lane_mode != mode:
    if reset:
        finish("reset", f"lbug mode switch {lane_mode} -> {mode}")
    finish("fail", f"lane {d} last used in lbug mode '{lane_mode}', requested '{mode}'; LBUG_* env is not cargo-fingerprinted — use --reset-lbug-artifacts or a different lane")

if mode == "prebuilt":
    if not native_id:
        finish("fail", "prebuilt run requires --native-id (bundle content fingerprint)")
    if lane_native != native_id:
        if reset:
            finish("reset", f"native bundle changed ({(lane_native or 'none')[:12]} -> {native_id[:12]})")
        finish("fail", f"lane {d} was built against native bundle {(lane_native or 'unrecorded')[:12]}…, requested {native_id[:12]}…; prebuilt bundles are not interchangeable — use --reset-lbug-artifacts or a different lane")
else:
    if lane_native:
        finish("fail", "lane marker records a prebuilt native id but bundled mode requested — marker inconsistent")

# toolchain drift is diagnostic-only: cargo fingerprints already key on
# rustc version/profile/features/rustflags, and `record` rewrites the
# marker each run so drift is naturally refreshed.

finish("proceed", warnings=warnings)
PY
  exit $?
fi

# ── record: atomic marker write ───────────────────────────────────────
NOW="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
mkdir -p "$DIR"
TMP="$MARKER.tmp.$$"
python3 - "$TMP" "$DIR" "$MODE" "$NATIVE_ID" "$NATIVE_PATH" "$LABEL" "$NOW" <<'PY'
import json, sys, os, hashlib
path, d, mode, nid, npath, label, now = sys.argv[1:8]
try:
    lane_id = hashlib.sha256(os.path.realpath(d).encode()).hexdigest()[:16]
except Exception:
    lane_id = ""
m = {
    "marker_version": 1,
    "lane_id": lane_id,
    "created_utc": now,
    "updated_utc": now,
    "label": label or "",
    "lbug_mode": mode,
    "lbug_native_id": nid or "",
    "lbug_native_path": npath or "",
    "rustc": os.environ.get("GUARD_RUSTC", ""),
    "rustc_commit": os.environ.get("GUARD_RUSTC_COMMIT", ""),
    "cargo": os.environ.get("GUARD_CARGO", ""),
    "cmake": os.environ.get("GUARD_CMAKE", ""),
    "cxx": os.environ.get("GUARD_CXX", ""),
}
json.dump(m, open(path, "w"), indent=2)
PY
mv "$TMP" "$MARKER"
exit 0
