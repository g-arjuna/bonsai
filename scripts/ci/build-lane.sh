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
#   --lbug-native PATH  prepared dir from lbug-native-prepare.sh.
#                 The bundle is verified fail-closed (manifest v4:
#                 mandatory provenance fields, full archive+header content
#                 inventory, bundle_id, donor ABI provenance) and its
#                 content fingerprint is recorded in the lane marker —
#                 switching bundles requires --reset-lbug-artifacts or a
#                 new lane. LBUG_* env comes from manifest.json via
#                 build-lane-guard.sh, never from an unchecked env.sh.
#   --reset-lbug-artifacts
#                 required to switch a lane between bundled/prebuilt
#                 lbug modes OR between different prebuilt bundles:
#                 LBUG_* env vars and the native path are NOT
#                 Cargo-fingerprinted, so cargo cannot detect the change
#                 and would silently reuse stale build-script outputs.
#                 This flag runs `cargo clean -p lbug` against the lane
#                 first (which forces the lbug build script — and thus
#                 every dependent crate — to rebuild against the new
#                 mode/bundle).
#
# Scheduling contract:
#   A blocking flock(2) on <dir>/.lane.lock is held for the duration of
#   the command. Exactly one heavyweight Cargo build may own a lane at a
#   time; a second invocation waits instead of corrupting fingerprints.
#   Cargo's own target lock still applies inside the lane.
#
# Compatibility (shared with run-local.sh via build-lane-guard.sh):
#   The lane records lbug_mode + the exact native bundle_id fingerprint
#   in <dir>/.lane-compat.json. Mismatches FAIL CLOSED unless
#   --reset-lbug-artifacts is given; malformed/missing markers on
#   populated lanes fail closed. Inherited LBUG_* env or stale
#   lbug-native CXXFLAGS -I roots are rejected (build-lane-guard.sh
#   sanity) so a bundled build can never silently inherit prebuilt
#   settings.
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
GUARD="$SCRIPT_DIR/build-lane-guard.sh"

# Fail closed on inherited lbug-native settings leaking into this run.
bash "$GUARD" sanity

NATIVE_ID=""
if [ -n "$LBUG_NATIVE" ]; then
  # Canonicalize so <dir>.lock resolves to the same inode for every
  # consumer/publisher regardless of symlinked invocation paths.
  _native_in="$LBUG_NATIVE"
  LBUG_NATIVE="$(cd "$_native_in" 2>/dev/null && pwd -P || true)"
  [ -n "$LBUG_NATIVE" ] && [ -f "$LBUG_NATIVE/manifest.json" ] \
    || { echo "build-lane: $_native_in/manifest.json missing — run lbug-native-prepare.sh first" >&2; exit 2; }
  # Hold the bundle SHARED lock BEFORE verification and identity
  # extraction, through environment construction and actual consumption
  # (BUILD-07): prepare takes the exclusive side, so archives cannot be
  # replaced between verify and the build that trusts them.
  exec 7>"$LBUG_NATIVE.lock"
  flock -s 7
  # Fail-closed provenance/integrity check before trusting the dir.
  if ! NATIVE_VERDICT="$(bash "$SCRIPT_DIR/lbug-native-prepare.sh" --verify "$LBUG_NATIVE")"; then
    echo "build-lane: GATE FAIL — lbug-native dir failed verification:" >&2
    printf '%s\n' "$NATIVE_VERDICT" >&2
    exit 1
  fi
  NATIVE_ID="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["bundle_id"])' "$LBUG_NATIVE/manifest.json")"
  LBUG_MODE="prebuilt"
else
  LBUG_MODE="bundled"
fi

mkdir -p "$DIR"
exec 9>"$DIR/.lane.lock"
echo "build-lane: waiting for lock on $DIR" >&2
flock 9
echo "build-lane: lock acquired on $DIR" >&2

# Shared lane compatibility check (same guard run-local.sh uses).
RESET_ARG=""
[ "$RESET_LBUG" = "1" ] && RESET_ARG="--reset"
VERDICT="$(bash "$GUARD" check --dir "$DIR" --mode "$LBUG_MODE" \
    ${NATIVE_ID:+--native-id "$NATIVE_ID"} \
    ${LBUG_NATIVE:+--native-path "$LBUG_NATIVE"} $RESET_ARG || true)"
ACTION="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("action","fail"))' <<<"$VERDICT" 2>/dev/null || echo fail)"
RC=$?
case "$ACTION" in
  proceed) ;;
  reset)
    [ "$RESET_LBUG" = "1" ] || { echo "build-lane: internal guard error" >&2; exit 1; }
    echo "build-lane: $(python3 -c 'import json,sys; print(json.load(sys.stdin).get("reason",""))' <<<"$VERDICT")" >&2
    echo "build-lane: running 'cargo clean -p lbug' on lane $DIR" >&2
    CARGO_TARGET_DIR="$DIR" cargo clean -p lbug
    ;;
  *)
    echo "build-lane: GATE FAIL — lane compatibility check failed:" >&2
    printf '%s\n' "$VERDICT" >&2
    exit 1
    ;;
esac

# Record/refresh the marker for this run's mode + native fingerprint.
GUARD_RUSTC="$(rustc -vV 2>/dev/null | awk '/^release:/{print $2}')" \
GUARD_RUSTC_COMMIT="$(rustc -vV 2>/dev/null | awk '/^commit-hash:/{print $2}')" \
GUARD_CARGO="$(cargo --version 2>/dev/null | awk '{print $2}')" \
GUARD_CMAKE="$(cmake --version 2>/dev/null | awk 'NR==1{print $3}')" \
GUARD_CXX="$(c++ --version 2>/dev/null | awk 'NR==1{print $4}' | tr -d ')')" \
export GUARD_RUSTC GUARD_RUSTC_COMMIT GUARD_CARGO GUARD_CMAKE GUARD_CXX
bash "$GUARD" record --dir "$DIR" --mode "$LBUG_MODE" \
    ${NATIVE_ID:+--native-id "$NATIVE_ID"} \
    ${LBUG_NATIVE:+--native-path "$LBUG_NATIVE"} --label "$LABEL"

# Apply prebuilt env from validated manifest metadata. The bundle
# shared lock (fd 7) has been held since before verification and stays
# held through the child command so prepare cannot replace archives
# mid-build. Env arrives as TAB-separated KEY<TAB>VALUE and is applied
# via structured export — never eval (BUILD-12).
if [ -n "$LBUG_NATIVE" ]; then
  while IFS=$'\t' read -r _k _v; do
    [ -n "$_k" ] && export "$_k=$_v"
  done < <(bash "$GUARD" env --dir "$LBUG_NATIVE")
fi

echo "build-lane: CARGO_TARGET_DIR=$DIR lbug=$LBUG_MODE${NATIVE_ID:+ bundle=${NATIVE_ID:0:12}} cmd: $*" >&2
export CARGO_TARGET_DIR="$DIR"
exec "$@"
