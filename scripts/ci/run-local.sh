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
#                           [--shared-target] [--net bridge|none]
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
GATE_NET="none"

TIERS=()
BUILD_IMAGE=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --build)         BUILD_IMAGE=1 ;;
    --trusted)       MODE="trusted" ;;
    --untrusted)     MODE="untrusted" ;;
    --shared-target) SHARED_TARGET=1 ;;
    --net)
      [ "$#" -ge 2 ] || { echo "--net requires bridge|none" >&2; exit 2; }
      GATE_NET="$2"; shift ;;
    l0|l1|l2|l3|l4|manifest) TIERS+=("$1") ;;
    *) echo "usage: $0 [--build] [--trusted|--untrusted] [--shared-target] [--net bridge|none] [l0 l1 l2 l3 ...]" >&2; exit 2 ;;
  esac
  shift
done
case "$GATE_NET" in bridge|none) ;; *) echo "--net must be bridge|none" >&2; exit 2 ;; esac
[ "${#TIERS[@]}" -gt 0 ] || TIERS=(l0 l1 l2 l3)

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
if [ "$SHARED_TARGET" = "1" ]; then
  if [ "$MODE" != "trusted" ]; then
    echo "GATE FAIL: --shared-target is only permitted in --trusted mode" >&2; exit 2
  fi
  mkdir -p "$ROOT/target"
  MOUNTS+=(-v "$ROOT/target:$ROOT/target")
  echo "=== TRUSTED-LOCAL policy: sharing Cargo target $ROOT/target ==="
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
    # Locked dependency provisioning into the dedicated CARGO_HOME.
    cargo fetch --locked
    # Python venv (persistent via /ci-cache/venv; symlinked into snapshot
    # because gates.sh prefers \$ROOT/.venv/bin/python).
    if [ ! -x /ci-cache/venv/bin/python ]; then
      python3.12 -m venv /ci-cache/venv
      /ci-cache/venv/bin/pip install --upgrade pip
    fi
    # Editable SDK install is path-dependent ($ROOT differs across
    # checkouts) and the ml extra is required by SDK test modules —
    # verify both and reinstall if either went stale.
    if ! /ci-cache/venv/bin/python -c 'import bonsai_sdk, joblib' >/dev/null 2>&1; then
      /ci-cache/venv/bin/pip install -e 'python[dev,ml]'
    fi
    ln -sfn /ci-cache/venv .venv
    if [ '$NEED_UI' = '1' ]; then
      (cd ui && npm ci --no-audit --no-fund && npm run build)
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
