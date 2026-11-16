#!/usr/bin/env bash
# Local containerized CI runner (BN-019-B6 / secure-core WP1).
#
# Executes the SAME canonical gate script as remote CI —
# `scripts/ci/gates.sh` — inside the development-only image defined by
# docker/ci/Dockerfile. No separate test commands: whatever gates.sh
# runs locally is what the GitHub workflow intends to run.
#
# Usage:
#   scripts/ci/run-local.sh [--build] [--trusted|--untrusted]
#                           [--shared-target|--shared-target-dir PATH]
#                           [--lbug-native PATH] [--net bridge|none]
#                           [l0 l1 l2 l3 ...]
#
# What it does:
#   1. Builds docker/ci/Dockerfile -> bonsai-ci:local (cached). The image
#      contains the pinned Rust toolchain + sccache; NO host toolchain,
#      cargo home, rustup home or developer config is mounted.
#   2. Creates a SELF-CONTAINED committed-source snapshot of HEAD via
#      `git clone` of the repository's common git dir + detached checkout
#      (exact tested SHA, full git metadata for L0 diff-base detection —
#      unlike `git worktree`, the .git directory travels inside the
#      mount). Uncommitted changes — including protected soak evidence —
#      are NOT carried into the container.
#   3. Phase A (provision, networked): idempotent dependency provisioning
#      into DEDICATED cache dirs — cargo registry, Python venv, npm,
#      Playwright browsers, UI build. Phase B (gates): `docker run
#      --network none` executes gates.sh against the warm caches. Test
#      execution is network-isolated; only provisioning touches the
#      network. (The UI build itself runs in phase A so that l1's
#      SKIP_UI_BUILD path verifies the already-built ui/dist.)
#
# Trust modes:
#   --trusted (default)  dedicated warm caches under
#                        ~/.cache/bonsai-ci (cargo registry, sccache,
#                        playwright, npm, venv). This is the local-dev
#                        lane. --shared-target additionally bind-mounts
#                        the invoking checkout's Cargo target/ — a
#                        writable shared target cache, permitted ONLY
#                        under this explicit trusted-local policy.
#   --untrusted          fresh per-run cache root (mktemp), cold sccache,
#                        ephemeral target inside the snapshot. For
#                        validating untrusted PR code: no executable
#                        cache or target state is shared with trusted
#                        runs. Cold provisioning is slower.
#
#   --shared-target-dir PATH  trusted-only managed build lane: PATH is
#                        bind-mounted as the container's Cargo target/
#                        for BOTH phases, so compatible trusted
#                        worktrees reuse warm dependency/native artifacts
#                        (incl. the lbug CMake build). A host-side flock
#                        on PATH/.lane.lock enforces one build owner.
#                        Host-side equivalent: scripts/ci/build-lane.sh.
#   --lbug-native PATH  trusted-only: PATH is a dir prepared by
#                        scripts/ci/lbug-native-prepare.sh, bind-mounted
#                        read-only at the identical path; LBUG_LIBRARY_DIR/
#                        LBUG_INCLUDE_DIR/CXXFLAGS switch lbug's build.rs
#                        to prebuilt-native mode so cargo check/build/test
#                        units compile only the small cxx bridge instead of
#                        re-running the CMake build. Never mix with
#                        bundled-mode runs on the same target dir.
#
# Never mounted: developer $HOME, ~/.cargo, ~/.rustup, docker socket,
# production databases, protected soak evidence. Container runs
# non-root with --memory/--cpus limits (defaults 24g/4, override via
# CI_MEMORY/CI_CPUS) and CARGO_BUILD_JOBS=2.
#
# Environment overrides:
#   CI_IMAGE        image tag                 (default bonsai-ci:local)
#   CI_MEMORY       docker --memory           (default 24g)
#   CI_CPUS         docker --cpus             (default 4)
#   CI_BUILD_JOBS   cargo jobs                (default 2)
#   CI_SNAPSHOT     keep snapshot dir         (default: removed on exit)
#   CI_CACHE_ROOT   trusted cache root        (default ~/.cache/bonsai-ci)
#   GATE_BASE       passed through to gates.sh (default origin/main)
set -euo pipefail
cd "$(dirname "$0")/../.."
ROOT="$(pwd)"

IMAGE="${CI_IMAGE:-bonsai-ci:local}"
MEMORY="${CI_MEMORY:-24g}"
CPUS="${CI_CPUS:-4}"
JOBS="${CI_BUILD_JOBS:-2}"
KEEP_SNAPSHOT="${CI_SNAPSHOT:-0}"
MODE="trusted"
SHARED_TARGET=0
SHARED_TARGET_DIR=""
LBUG_NATIVE_DIR=""
GATE_NET="none"

TIERS=()
BUILD_IMAGE=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --build)         BUILD_IMAGE=1 ;;
    --trusted)       MODE="trusted" ;;
    --untrusted)     MODE="untrusted" ;;
    --shared-target) SHARED_TARGET=1 ;;
    --shared-target-dir)
      [ "$#" -ge 2 ] || { echo "--shared-target-dir requires PATH" >&2; exit 2; }
      SHARED_TARGET_DIR="$2"; shift ;;
    --lbug-native)
      [ "$#" -ge 2 ] || { echo "--lbug-native requires PATH" >&2; exit 2; }
      LBUG_NATIVE_DIR="$2"; shift ;;
    --net)
      [ "$#" -ge 2 ] || { echo "--net requires bridge|none" >&2; exit 2; }
      GATE_NET="$2"; shift ;;
    l0|l1|l2|l3|l4|manifest) TIERS+=("$1") ;;
    *) echo "usage: $0 [--build] [--trusted|--untrusted] [--shared-target|--shared-target-dir PATH] [--lbug-native PATH] [--net bridge|none] [l0 l1 l2 l3 ...]" >&2; exit 2 ;;
  esac
  shift
done
case "$GATE_NET" in bridge|none) ;; *) echo "--net must be bridge|none" >&2; exit 2 ;; esac
# Fail fast on prohibited trust-mode combinations before doing any work.
if [ "$MODE" = "untrusted" ]; then
  [ -z "$LBUG_NATIVE_DIR" ] || { echo "GATE FAIL: --lbug-native is only permitted in --trusted mode" >&2; exit 2; }
  [ -z "$SHARED_TARGET_DIR" ] || { echo "GATE FAIL: --shared-target-dir is only permitted in --trusted mode" >&2; exit 2; }
  [ "$SHARED_TARGET" = "0" ] || { echo "GATE FAIL: --shared-target is only permitted in --trusted mode" >&2; exit 2; }
fi
[ "${#TIERS[@]}" -gt 0 ] || TIERS=(l0 l1 l2 l3)

GUARD="$ROOT/scripts/ci/build-lane-guard.sh"
PREPARE="$ROOT/scripts/ci/lbug-native-prepare.sh"

# Fail closed on inherited lbug-native settings leaking into this run.
bash "$GUARD" sanity

# Early fail-closed verification of the native bundle (host side, strict
# toolchain) so lane/env problems surface before any docker work. The
# bundle is ALSO re-verified inside the gate container (phase A,
# --gate-env) — the actual execution environment.
NATIVE_ID=""
if [ -n "$LBUG_NATIVE_DIR" ]; then
  [ -f "$LBUG_NATIVE_DIR/env.sh" ] || { echo "GATE FAIL: $LBUG_NATIVE_DIR/env.sh missing — run scripts/ci/lbug-native-prepare.sh" >&2; exit 2; }
  # Canonicalize (physical path) so <dir>.lock is the same inode for
  # every consumer/publisher regardless of symlinked invocation paths.
  LBUG_NATIVE_DIR="$(cd "$LBUG_NATIVE_DIR" && pwd -P)"
  # Hold the bundle SHARED lock BEFORE verification and identity
  # extraction, through the whole container run (BUILD-07): prepare
  # takes the exclusive side, so archives cannot be replaced between
  # verify and consumption.
  exec 7>"$LBUG_NATIVE_DIR.lock"
  flock -s 7
  if ! NATIVE_VERDICT="$(bash "$PREPARE" --verify "$LBUG_NATIVE_DIR")"; then
    echo "GATE FAIL: --lbug-native dir failed verification:" >&2
    printf '%s\n' "$NATIVE_VERDICT" >&2
    exit 2
  fi
  NATIVE_ID="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["bundle_id"])' "$LBUG_NATIVE_DIR/manifest.json")"
fi

# Unified shared-target guard (BUILD-09): BOTH the managed
# --shared-target-dir lane AND the legacy --shared-target path get the
# same flock + compatibility-marker protection, so no shared target
# entry point can silently mix bundled/prebuilt lbug modes or native
# bundles. Runs BEFORE any image/snapshot/docker work so incompatibility
# fails fast. --shared-target is retained only as a deprecated alias for
# the checkout's own target/ (bundled fallback preserved); new work
# should use --shared-target-dir.
if [ "$SHARED_TARGET" = "1" ] && [ -n "$SHARED_TARGET_DIR" ]; then
  echo "GATE FAIL: --shared-target and --shared-target-dir are mutually exclusive" >&2; exit 2
fi
LANE_DIR=""; LANE_LABEL=""
if [ -n "$SHARED_TARGET_DIR" ]; then
  if [ "$MODE" != "trusted" ]; then
    echo "GATE FAIL: --shared-target-dir is only permitted in --trusted mode" >&2; exit 2
  fi
  if [ -d "$SHARED_TARGET_DIR" ]; then
    SHARED_TARGET_DIR="$(cd "$SHARED_TARGET_DIR" && pwd -P)"
  else
    mkdir -p "$SHARED_TARGET_DIR"
    SHARED_TARGET_DIR="$(cd "$SHARED_TARGET_DIR" && pwd -P)"
  fi
  LANE_DIR="$SHARED_TARGET_DIR"; LANE_LABEL="run-local"
fi
if [ "$SHARED_TARGET" = "1" ]; then
  if [ "$MODE" != "trusted" ]; then
    echo "GATE FAIL: --shared-target is only permitted in --trusted mode" >&2; exit 2
  fi
  echo "=== NOTE: --shared-target is deprecated; prefer --shared-target-dir PATH ===" >&2
  LANE_DIR="$ROOT/target"; LANE_LABEL="run-local --shared-target"
fi
if [ -n "$LANE_DIR" ]; then
  mkdir -p "$LANE_DIR"
  # Explicit single-owner scheduling: one heavyweight Cargo build owns the
  # shared target for the whole container run (both phases). The lock is a
  # host-side flock on the lane dir; it is held until this script exits.
  exec 8>"$LANE_DIR/.lane.lock"
  echo "=== TRUSTED-LOCAL policy: shared build lane $LANE_DIR (flock held for run) ==="
  flock 8
  # Shared lane compatibility check — the SAME guard build-lane.sh uses:
  # lane identity, bundled/prebuilt mode, exact native bundle_id
  # fingerprint, effective-CXXFLAGS fingerprint, fail-closed on
  # malformed/missing markers. There is no in-container remediation
  # path: on mismatch run
  #   scripts/ci/build-lane.sh --dir "$LANE_DIR" \
  #     [--lbug-native DIR] --reset-lbug-artifacts -- true
  # on the host, or choose a different lane dir.
  LANE_MODE="bundled"; [ -n "$LBUG_NATIVE_DIR" ] && LANE_MODE="prebuilt"
  if ! LANE_VERDICT="$(bash "$GUARD" check --dir "$LANE_DIR" \
        --mode "$LANE_MODE" ${NATIVE_ID:+--native-id "$NATIVE_ID"} \
        ${LBUG_NATIVE_DIR:+--native-path "$LBUG_NATIVE_DIR"})"; then
    echo "GATE FAIL: shared build lane $LANE_DIR failed compatibility check:" >&2
    printf '%s\n' "$LANE_VERDICT" >&2
    echo "Remediate on the host: scripts/ci/build-lane.sh --dir '$LANE_DIR' --reset-lbug-artifacts -- true" >&2
    exit 2
  fi
  GUARD_RUSTC="$(rustc -vV 2>/dev/null | awk '/^release:/{print $2}')" \
  GUARD_RUSTC_COMMIT="$(rustc -vV 2>/dev/null | awk '/^commit-hash:/{print $2}')" \
  GUARD_CARGO="$(cargo --version 2>/dev/null | awk '{print $2}')" \
  GUARD_CMAKE="$(cmake --version 2>/dev/null | awk 'NR==1{print $3}')" \
  GUARD_CXX="$(c++ --version 2>/dev/null | awk 'NR==1{print $4}' | tr -d ')')" \
  export GUARD_RUSTC GUARD_RUSTC_COMMIT GUARD_CARGO GUARD_CMAKE GUARD_CXX
  bash "$GUARD" record --dir "$LANE_DIR" --mode "$LANE_MODE" \
      ${NATIVE_ID:+--native-id "$NATIVE_ID"} \
      ${LBUG_NATIVE_DIR:+--native-path "$LBUG_NATIVE_DIR"} --label "$LANE_LABEL"
fi

SHA="$(git rev-parse HEAD)"
GIT_COMMON="$(git rev-parse --path-format=absolute --git-common-dir)"
SNAP="$(mktemp -d /tmp/bonsai-ci-src.XXXXXX)"
CACHE_ROOT=""
cleanup() {
  if [ "$KEEP_SNAPSHOT" != "1" ]; then
    rm -rf "$SNAP"
  else
    echo "snapshot kept at $SNAP"
  fi
  if [ "$MODE" = "untrusted" ] && [ -n "$CACHE_ROOT" ]; then
    rm -rf "$CACHE_ROOT"
  fi
}
trap cleanup EXIT

# ── 1. image ────────────────────────────────────────────────────────────
if [ "$BUILD_IMAGE" = "1" ] || ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "=== building CI image $IMAGE ==="
  docker build -f docker/ci/Dockerfile -t "$IMAGE" docker/ci
fi

# ── 2. self-contained committed-source snapshot at exact SHA ───────────
echo "=== snapshotting $SHA into $SNAP (clone of $GIT_COMMON) ==="
git clone --quiet "$GIT_COMMON" "$SNAP"
git -C "$SNAP" checkout --quiet --detach "$SHA"
# Never carry protected soak evidence into the container.
rm -rf "$SNAP/docs/merger/evidence/2026-11-09-bn009-soak-full" 2>/dev/null || true

# ── 3. dedicated cache root per trust mode ─────────────────────────────
if [ "$MODE" = "trusted" ]; then
  CACHE_ROOT="${CI_CACHE_ROOT:-$HOME/.cache/bonsai-ci}"
else
  CACHE_ROOT="$(mktemp -d /tmp/bonsai-ci-cache.XXXXXX)"
fi
mkdir -p "$CACHE_ROOT"/{cargo,sccache,ms-playwright,npm,venv,home}

LOG_DIR="$ROOT/target/ci/local-run-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$LOG_DIR" 2>/dev/null \
  || { LOG_DIR="$SNAP/target/ci-local-$(date +%Y%m%d-%H%M%S)"; mkdir -p "$LOG_DIR"; }

# Shared mounts: snapshot at the invoking checkout's path (so repo-
# relative paths inside gates.sh behave identically), dedicated caches
# under /ci-cache, ephemeral HOME at /ci-cache/home. NO host $HOME/.cargo,
# $HOME/.rustup, docker socket or other developer state.
MOUNTS=(
  -v "$SNAP:$ROOT"
  -v "$CACHE_ROOT/cargo:/ci-cache/cargo"
  -v "$CACHE_ROOT/sccache:/ci-cache/sccache"
  -v "$CACHE_ROOT/ms-playwright:/ci-cache/ms-playwright"
  -v "$CACHE_ROOT/npm:/ci-cache/npm"
  -v "$CACHE_ROOT/venv:/ci-cache/venv"
  -v "$CACHE_ROOT/home:/ci-cache/home"
)
if [ "$SHARED_TARGET" = "1" ] && [ -n "$SHARED_TARGET_DIR" ]; then
  echo "GATE FAIL: --shared-target and --shared-target-dir are mutually exclusive" >&2; exit 2
fi
if [ "$SHARED_TARGET" = "1" ]; then
  mkdir -p "$ROOT/target"
  MOUNTS+=(-v "$ROOT/target:$ROOT/target")
  echo "=== TRUSTED-LOCAL policy: sharing Cargo target $ROOT/target ==="
fi
if [ -n "$SHARED_TARGET_DIR" ]; then
  mkdir -p "$SHARED_TARGET_DIR"
  MOUNTS+=(-v "$SHARED_TARGET_DIR:$ROOT/target")
fi
# Container env: ephemeral HOME, dedicated CARGO_HOME, cache dirs.
# PATH is set inside the run command (toolchain dir resolved there so the
# Dockerfile stays the single version pin).
BASE_ENV=(
  -e CARGO_BUILD_JOBS="$JOBS"
  -e CARGO_PROFILE=ci
  -e HOME=/ci-cache/home
  -e CARGO_HOME=/ci-cache/cargo
  -e SCCACHE_DIR=/ci-cache/sccache
  -e PLAYWRIGHT_BROWSERS_PATH=/ci-cache/ms-playwright
  -e npm_config_cache=/ci-cache/npm
  -e GATE_BASE="${GATE_BASE:-origin/main}"
  # ui/dist is produced by phase A provisioning; l1 verifies it exists.
  -e SKIP_UI_BUILD=1
)

if [ -n "$LBUG_NATIVE_DIR" ]; then
  # Bundle already verified + bundle_id captured, and the shared lock
  # (fd 7) has been held since BEFORE verification through this whole
  # run so prepare cannot swap archives mid-gate. Env comes from
  # validated manifest.json via the shared guard as TAB-separated
  # KEY<TAB>VALUE applied by structured export — never eval (BUILD-12).
  while IFS=$'\t' read -r _k _v; do
    [ -n "$_k" ] && export "$_k=$_v"
  done < <(bash "$GUARD" env --dir "$LBUG_NATIVE_DIR")
  # env paths are absolute under LBUG_NATIVE_DIR; bind-mount the dir at
  # the identical container path (read-only) so they resolve verbatim.
  MOUNTS+=(-v "$LBUG_NATIVE_DIR:$LBUG_NATIVE_DIR:ro")
  # If lib/ entries are symlinks into a donor target tree, the identical
  # donor paths must exist inside the container or the archives dangle.
  # Mount each unique resolved target's donor root read-only at the same
  # absolute path. hardlink/copy dirs need no extra mounts.
  DONOR_MOUNTS="$(python3 - "$LBUG_NATIVE_DIR" <<'PY'
import json, os, sys
dest = sys.argv[1]
mf = json.load(open(os.path.join(dest, "manifest.json")))
donor = mf.get("donor_out") or ""
roots = set()
for n in sorted(os.listdir(os.path.join(dest, "lib"))):
    p = os.path.join(dest, "lib", n)
    if os.path.islink(p):
        t = os.path.realpath(p)
        # mount the deepest existing ancestor that contains the target
        d = os.path.dirname(t)
        while d and not os.path.isdir(d):
            d = os.path.dirname(d)
        if d:
            roots.add(d)
# prefer the manifest donor_out when it covers the targets
covered = donor and all(r == donor or r.startswith(donor + os.sep) or donor.startswith(r + os.sep) for r in roots)
out = []
if covered and os.path.isdir(donor):
    out.append(donor)
else:
    out = sorted(roots)
print("\n".join(out))
PY
)"
  if [ -n "$DONOR_MOUNTS" ]; then
    while IFS= read -r d; do
      [ -n "$d" ] || continue
      [ -d "$d" ] || { echo "GATE FAIL: lbug-native symlink donor $d missing on host" >&2; exit 2; }
      MOUNTS+=(-v "$d:$d:ro")
      echo "=== TRUSTED-LOCAL policy: lbug-native donor mount $d (ro) ==="
    done <<< "$DONOR_MOUNTS"
  fi
  BASE_ENV+=(
    -e "LBUG_LIBRARY_DIR=$LBUG_LIBRARY_DIR"
    -e "LBUG_INCLUDE_DIR=$LBUG_INCLUDE_DIR"
    -e "CXXFLAGS=${CXXFLAGS:-}"
  )
  echo "=== TRUSTED-LOCAL policy: prebuilt lbug native dir $LBUG_NATIVE_DIR ==="
fi

# Does the requested tier set need UI/python provisioning?
NEED_UI=0; NEED_PY=0; NEED_BROWSER=0
for t in "${TIERS[@]}"; do
  case "$t" in
    l1) NEED_UI=1; NEED_PY=1 ;;
    l3) NEED_UI=1; NEED_PY=1; NEED_BROWSER=1 ;;
  esac
done

# ── 4. phase A: provision dependencies (networked) ─────────────────────
echo "=== phase A: provision (network bridge, cache root: $CACHE_ROOT) ==="
set +e
docker run --rm \
  --name "bonsai-ci-provision-$$" \
  --memory="$MEMORY" --memory-swap="$MEMORY" --cpus="$CPUS" \
  --user "$(id -u):$(id -g)" \
  --network bridge \
  "${BASE_ENV[@]}" \
  "${MOUNTS[@]}" \
  "$IMAGE" \
  bash -lc "
    set -euo pipefail
    export PATH=\"/opt/rustup/toolchains/\$(ls /opt/rustup/toolchains | head -1)/bin:/usr/local/bin:\$PATH\"
    cd '$ROOT'
    echo \"rustc: \$(rustc --version)\"; echo \"cargo: \$(cargo --version)\"; echo \"sccache: \$(sccache --version)\"
    rustc -Vv
    # Re-verify the native bundle INSIDE the actual gate environment
    # (--gate-env: integrity/identity/ABI provenance enforced; the
    # preparing host's rustc/cmake/cxx versions need not equal the
    # container toolchain — the cxx bridge is compiled fresh here).
    if [ -n '${LBUG_NATIVE_DIR:-}' ]; then
      bash scripts/ci/lbug-native-prepare.sh --verify --gate-env '$LBUG_NATIVE_DIR'
    fi
    # Locked dependency provisioning into the dedicated CARGO_HOME.
    cargo fetch --locked
    # Python venv (persistent via /ci-cache/venv; symlinked into snapshot
    # because gates.sh prefers \$ROOT/.venv/bin/python).
    if [ ! -x /ci-cache/venv/bin/python ]; then
      python3.12 -m venv /ci-cache/venv
      /ci-cache/venv/bin/pip install --upgrade pip
    fi
    # Editable SDK install is path-dependent ($ROOT differs across
    # checkouts) and the ml extra is required by SDK test modules.
    # Reinstall unconditionally: an import check cannot prove the installed
    # environment still matches this checkout's pyproject specification.
    /ci-cache/venv/bin/pip install -e 'python[dev,ml]'
    ln -sfn /ci-cache/venv .venv
    if [ '$NEED_UI' = '1' ]; then
      (cd ui && npm ci --no-audit --no-fund && npm run build)
      # tests/ui_driver/package-lock.json is committed, so npm ci runs
      # unconditionally and fails closed on any lockfile/package.json drift
      # instead of silently resolving a different dependency tree.
      (cd tests/ui_driver && npm ci --no-audit --no-fund)
    fi
    if [ '$NEED_BROWSER' = '1' ]; then
      (cd tests/ui_driver && npx playwright install chromium)
    fi
    echo 'provision OK'
  "
PRC=$?
set -e
if [ "$PRC" != "0" ]; then
  echo "=== local CI finished: sha=$SHA image=$IMAGE mode=$MODE tiers=${TIERS[*]} exit=$PRC (provision failed) ==="
  exit "$PRC"
fi

# ── 5. phase B: gates (network-isolated by default) ────────────────────
echo "=== phase B: gates.sh ${TIERS[*]} (network: $GATE_NET) ==="
set +e
docker run --rm \
  --name "bonsai-ci-gates-$$" \
  --memory="$MEMORY" --memory-swap="$MEMORY" --cpus="$CPUS" \
  --user "$(id -u):$(id -g)" \
  --network "$GATE_NET" \
  "${BASE_ENV[@]}" \
  "${MOUNTS[@]}" \
  "$IMAGE" \
  bash -lc "
    set -o pipefail
    export PATH=\"/opt/rustup/toolchains/\$(ls /opt/rustup/toolchains | head -1)/bin:/usr/local/bin:\$PATH\"
    cd '$ROOT'
    mkdir -p target/ci
    scripts/ci/gates.sh ${TIERS[*]} 2>&1 | tee 'target/ci/local-gates-$(date +%s).log'
    exit \${PIPESTATUS[0]}
  "
RC=$?
set -e

echo "=== local CI finished: sha=$SHA image=$IMAGE mode=$MODE tiers=${TIERS[*]} exit=$RC ==="
echo "logs: $LOG_DIR ; gate log inside snapshot target/ci/ (kept only with CI_SNAPSHOT=1)"
exit "$RC"
