#!/usr/bin/env bash
# Trusted shared build-lane manager (BN-019 build acceleration).
#
# Runs a Cargo command against ONE managed, warm target directory so that
# compatible trusted worktrees reuse expensive dependency artifacts —
# above all the lbug/LadybugDB native CMake build (~35-77 min per cold
# build-script run on the dev host).
#
# Usage:
#   scripts/ci/build-lane.sh [--dir PATH] [--label NAME] -- <command...>
#
#   --dir PATH    managed target dir (default: $BONSAI_BUILD_LANE, else
#                 ~/.cache/bonsai-ci/build-lane). The directory is created
#                 if absent and is NEVER deleted by this script.
#   --label NAME  free-text lane label recorded in the compat marker.
#   --lbug-native PATH  source PATH/env.sh (from lbug-native-prepare.sh)
#                 before running: LBUG_LIBRARY_DIR/LBUG_INCLUDE_DIR/
#                 CXXFLAGS switch lbug's build.rs to prebuilt-native mode,
#                 skipping the CMake rebuild for every cargo unit kind.
#                 WARNING: never mix with bundled-mode runs on the same
#                 lane — the env vars are not cargo-fingerprinted.
#
# Scheduling contract:
#   A blocking flock(2) on <dir>/.lane.lock is held for the duration of
#   the command. Exactly one heavyweight Cargo build may own a lane at a
#   time; a second invocation waits instead of corrupting fingerprints.
#   Cargo's own target lock still applies inside the lane.
#
# Compatibility:
#   The lane records the toolchain identity (rustc/cargo/cmake/cxx
#   versions + rustc commit hash) in <dir>/.lane-compat.json on first
#   use and warns on later mismatches. Cargo fingerprints already key
#   on rustc version, profile, features and rustflags; this marker is
#   diagnostic, not a gate — do not bypass cargo fingerprints.
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

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dir)         DIR="$2"; shift 2 ;;
    --label)       LABEL="$2"; shift 2 ;;
    --lbug-native) LBUG_NATIVE="$2"; shift 2 ;;
    --)      shift; break ;;
    *)       echo "usage: $0 [--dir PATH] [--label NAME] [--lbug-native PATH] -- <command...>" >&2; exit 2 ;;
  esac
done
[ "$#" -gt 0 ] || { echo "build-lane: no command given" >&2; exit 2; }

if [ -n "$LBUG_NATIVE" ]; then
  [ -f "$LBUG_NATIVE/env.sh" ] || { echo "build-lane: $LBUG_NATIVE/env.sh missing — run lbug-native-prepare.sh first" >&2; exit 2; }
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

if [ ! -f "$COMPAT" ]; then
  cat > "$COMPAT" <<EOF
{
  "created_utc": "$NOW",
  "label": "$LABEL",
  "rustc": "$CUR_RUSTC",
  "rustc_commit": "$CUR_COMMIT",
  "cargo": "$CUR_CARGO",
  "cmake": "$CUR_CMAKE",
  "cxx": "$CUR_CXX",
  "lbug_mode": "$LBUG_MODE"
}
EOF
else
  python3 - "$COMPAT" "$CUR_RUSTC" "$CUR_COMMIT" "$CUR_CMAKE" "$CUR_CXX" "$LBUG_MODE" <<'PY' || true
import json, sys
path, rustc, commit, cmake, cxx, mode = sys.argv[1:7]
try:
    d = json.load(open(path))
except Exception:
    sys.exit(0)
mismatch = [k for k, cur in (("rustc", rustc), ("rustc_commit", commit),
                             ("cmake", cmake), ("cxx", cxx),
                             ("lbug_mode", mode))
            if d.get(k) not in (None, "", cur)]
if mismatch:
    print(f"build-lane: WARNING toolchain drift vs lane marker: {mismatch} "
          f"(lane={ {k: d.get(k) for k in mismatch} }, now="
          f"{{'rustc': '{rustc}', 'rustc_commit': '{commit}', "
          f"'cmake': '{cmake}', 'cxx': '{cxx}'}})", file=sys.stderr)
PY
fi

echo "build-lane: CARGO_TARGET_DIR=$DIR lbug=$LBUG_MODE cmd: $*" >&2
export CARGO_TARGET_DIR="$DIR"
exec "$@"
