#!/usr/bin/env bash
# Trusted shared build-lane manager (BN-019 build acceleration).
#
# Runs a Cargo command against ONE managed, warm target directory so that
# compatible trusted worktrees reuse expensive dependency artifacts —
# above all the lbug/LadybugDB native CMake build (~35-77 min per cold
# build-script run on the dev host).
#
# Usage:
#   scripts/ci/build-lane.sh [--dir PATH] [--label NAME]
#                            [--lbug-native PATH] [--reset-lbug-artifacts]
#                            -- <command...>
#
#   --dir PATH    managed target dir (default: $BONSAI_BUILD_LANE, else
#                 ~/.cache/bonsai-ci/build-lane). The directory is created
#                 if absent and is NEVER deleted by this script.
#   --label NAME  free-text lane label recorded in the compat marker.
#   --lbug-native PATH  source PATH/env.sh (from lbug-native-prepare.sh)
#                 before running: LBUG_LIBRARY_DIR/LBUG_INCLUDE_DIR/
#                 CXXFLAGS switch lbug's build.rs to prebuilt-native mode,
#                 skipping the CMake rebuild for every cargo unit kind.
#                 The dir is verified fail-closed first (manifest
#                 provenance, content inventory, toolchain/ABI match).
#   --reset-lbug-artifacts
#                 required to switch a lane between bundled and
#                 prebuilt lbug modes: LBUG_* env vars are NOT
#                 Cargo-fingerprinted, so cargo cannot detect the mode
#                 change and would silently reuse stale build-script
#                 outputs. This flag runs `cargo clean -p lbug` against
#                 the lane first, forcing a fresh lbug build script.
#
# Scheduling contract:
#   A blocking flock(2) on <dir>/.lane.lock is held for the duration of
#   the command. Exactly one heavyweight Cargo build may own a lane at a
#   time; a second invocation waits instead of corrupting fingerprints.
#   Cargo's own target lock still applies inside the lane.
#
# Compatibility:
#   The lane records the toolchain identity (rustc/cargo/cmake/cxx
#   versions + rustc commit hash) and the lbug mode in
#   <dir>/.lane-compat.json on first use. lbug_mode mismatches FAIL
#   CLOSED (cargo cannot fingerprint LBUG_* env); toolchain drift is
#   still diagnostic-only because Cargo fingerprints already key on
#   rustc version, profile, features and rustflags — do not bypass
#   cargo fingerprints.
#
# Trust boundary:
#   ONLY use this for trusted, committed-source builds. Never point it
#   at untrusted checkouts expecting isolation: the lane is a shared
#   writable target, equivalent to sharing one checkout's target/.
#   It contains no secrets, but artifacts are not an isolation boundary.
#
# Environment:
#   CARGO_TARGET_DIR is set to the lane dir for the child command.
#   All other cargo config (.cargo/config.toml rustflags, sccache
#   rustc-wrapper, mold linker) resolves from the invoking checkout and
#   MUST stay identical across lane users — mixed rustflags/toolchains
#   silently fork fingerprints and defeat reuse.
set -euo pipefail

DIR="${BONSAI_BUILD_LANE:-$HOME/.cache/bonsai-ci/build-lane}"
LABEL=""
LBUG_NATIVE=""
RESET_LBUG=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dir)         DIR="$2"; shift 2 ;;
    --label)       LABEL="$2"; shift 2 ;;
    --lbug-native) LBUG_NATIVE="$2"; shift 2 ;;
    --reset-lbug-artifacts) RESET_LBUG=1; shift ;;
    --)      shift; break ;;
    *)       echo "usage: $0 [--dir PATH] [--label NAME] [--lbug-native PATH] [--reset-lbug-artifacts] -- <command...>" >&2; exit 2 ;;
  esac
done
[ "$#" -gt 0 ] || { echo "build-lane: no command given" >&2; exit 2; }

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

LBUG_NATIVE_VERDICT=""
if [ -n "$LBUG_NATIVE" ]; then
  [ -f "$LBUG_NATIVE/env.sh" ] || { echo "build-lane: $LBUG_NATIVE/env.sh missing — run lbug-native-prepare.sh first" >&2; exit 2; }
  # Fail-closed provenance/integrity check before trusting the dir.
  if ! LBUG_NATIVE_VERDICT="$(bash "$SCRIPT_DIR/lbug-native-prepare.sh" --verify "$LBUG_NATIVE")"; then
    echo "build-lane: GATE FAIL — lbug-native dir failed verification:" >&2
    printf '%s\n' "$LBUG_NATIVE_VERDICT" >&2
    exit 1
  fi
  # shellcheck disable=SC1090
  . "$LBUG_NATIVE/env.sh"
  LBUG_MODE="prebuilt"
else
  LBUG_MODE="bundled"
fi

mkdir -p "$DIR"
exec 9>"$DIR/.lane.lock"
echo "build-lane: waiting for lock on $DIR" >&2
flock 9
echo "build-lane: lock acquired on $DIR" >&2

COMPAT="$DIR/.lane-compat.json"
CUR_RUSTC="$(rustc -vV 2>/dev/null | awk '/^release:/{print $2}')"
CUR_COMMIT="$(rustc -vV 2>/dev/null | awk '/^commit-hash:/{print $2}')"
CUR_CARGO="$(cargo --version 2>/dev/null | awk '{print $2}')"
CUR_CMAKE="$(cmake --version 2>/dev/null | awk 'NR==1{print $3}')"
CUR_CXX="$(c++ --version 2>/dev/null | awk 'NR==1{print $4}' | tr -d ')')"
NOW="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

MODE_RESET_DONE=0
if [ -f "$COMPAT" ]; then
  # lbug_mode is NOT cargo-fingerprinted: a bundled/prebuilt switch on the
  # same lane silently reuses stale lbug build-script outputs. Fail closed
  # unless the operator explicitly resets the lbug artifacts first.
  LANE_MODE="$(python3 - "$COMPAT" <<'PY' 2>/dev/null || true
import json, sys
try:
    print(json.load(open(sys.argv[1])).get("lbug_mode") or "")
except Exception:
    print("")
PY
)"
  if [ -n "$LANE_MODE" ] && [ "$LANE_MODE" != "$LBUG_MODE" ]; then
    if [ "$RESET_LBUG" != "1" ]; then
      echo "build-lane: GATE FAIL — lane $DIR was last used in lbug mode" >&2
      echo "  '$LANE_MODE' but this run requests '$LBUG_MODE'. LBUG_* env" >&2
      echo "  vars are not Cargo-fingerprinted, so cargo would reuse stale" >&2
      echo "  lbug build-script outputs. Either use a different --dir, or" >&2
      echo "  re-run with --reset-lbug-artifacts (runs 'cargo clean -p lbug'" >&2
      echo "  on the lane first)." >&2
      exit 1
    fi
    echo "build-lane: lbug mode switch $LANE_MODE -> $LBUG_MODE; cleaning lbug artifacts in lane" >&2
    CARGO_TARGET_DIR="$DIR" cargo clean -p lbug
    MODE_RESET_DONE=1
  fi
fi

if [ ! -f "$COMPAT" ] || [ "$MODE_RESET_DONE" = "1" ]; then
  cat > "$COMPAT" <<EOF
{
  "created_utc": "$NOW",
  "label": "$LABEL",
  "rustc": "$CUR_RUSTC",
  "rustc_commit": "$CUR_COMMIT",
  "cargo": "$CUR_CARGO",
  "cmake": "$CUR_CMAKE",
  "cxx": "$CUR_CXX",
  "lbug_mode": "$LBUG_MODE",
  "lbug_native": $(python3 -c 'import json,sys; print(json.dumps(sys.argv[1] or None))' "${LBUG_NATIVE:-}")
}
EOF
else
  python3 - "$COMPAT" "$CUR_RUSTC" "$CUR_COMMIT" "$CUR_CMAKE" "$CUR_CXX" <<'PY' || true
import json, sys
path, rustc, commit, cmake, cxx = sys.argv[1:6]
try:
    d = json.load(open(path))
except Exception:
    sys.exit(0)
mismatch = [k for k, cur in (("rustc", rustc), ("rustc_commit", commit),
                             ("cmake", cmake), ("cxx", cxx))
            if d.get(k) not in (None, "", cur)]
if mismatch:
    print(f"build-lane: WARNING toolchain drift vs lane marker: {mismatch} "
          f"(lane={ {k: d.get(k) for k in mismatch} })", file=sys.stderr)
PY
fi

echo "build-lane: CARGO_TARGET_DIR=$DIR lbug=$LBUG_MODE cmd: $*" >&2
export CARGO_TARGET_DIR="$DIR"
exec "$@"
