#!/usr/bin/env bash
# Local containerized CI runner (BN-019-B6 / secure-core WP1).
#
# Executes the SAME canonical gate script as remote CI —
# `scripts/ci/gates.sh` — inside the development-only image defined by
# docker/ci/Dockerfile. No separate test commands: whatever gates.sh
# runs locally is what the GitHub workflow intends to run.
#
# Usage:
#   scripts/ci/run-local.sh [l0 l1 l2 l3 ...] [--build]
#   scripts/ci/run-local.sh l0 l1 l2 l3
#
# What it does:
#   1. Builds docker/ci/Dockerfile -> bonsai-ci:local (cached).
#   2. Creates a CLEAN committed-source snapshot of HEAD via
#      `git worktree` (exact tested SHA, full git metadata for L0 diff
#      base detection). Uncommitted changes — including protected soak
#      evidence — are NOT carried into the container.
#   3. Runs the container with resource limits (24 GB / ~4 CPUs),
#      CARGO_BUILD_JOBS=2, non-root, no docker socket, no host home
#      mounts beyond the explicit cache/target bind mounts below.
#
# Cache strategy (disk-safe): the existing ~141 GB Cargo target, the
# rustup toolchain, cargo registry, sccache cache and Playwright browser
# cache are bind-mounted at their IDENTICAL host paths inside the
# container. Same rustc 1.95.0, same absolute paths, same profile =>
# fingerprints and sccache entries remain valid; no second target tree
# is created. If a mount is absent the container still works — it just
# compiles cold (needs ~140 GB free; check `df -h` first).
#
# Environment overrides:
#   CI_IMAGE        image tag            (default bonsai-ci:local)
#   CI_MEMORY       docker --memory      (default 24g)
#   CI_CPUS         docker --cpus        (default 4)
#   CI_BUILD_JOBS   cargo jobs           (default 2)
#   CI_SNAPSHOT     keep snapshot dir    (default: removed on exit)
#   GATE_BASE       passed through to gates.sh
set -euo pipefail
cd "$(dirname "$0")/../.."
ROOT="$(pwd)"

IMAGE="${CI_IMAGE:-bonsai-ci:local}"
MEMORY="${CI_MEMORY:-24g}"
CPUS="${CI_CPUS:-4}"
JOBS="${CI_BUILD_JOBS:-2}"
KEEP_SNAPSHOT="${CI_SNAPSHOT:-0}"

TIERS=()
BUILD_IMAGE=0
for arg in "$@"; do
  case "$arg" in
    --build) BUILD_IMAGE=1 ;;
    l0|l1|l2|l3|l4|manifest) TIERS+=("$arg") ;;
    *) echo "usage: $0 [--build] [l0 l1 l2 l3 ...]" >&2; exit 2 ;;
  esac
done
[ "${#TIERS[@]}" -gt 0 ] || TIERS=(l0 l1 l2 l3)

SHA="$(git rev-parse HEAD)"
SNAP="$(mktemp -d /tmp/bonsai-ci-src.XXXXXX)"
cleanup() {
  if [ "$KEEP_SNAPSHOT" != "1" ]; then
    git worktree remove --force "$SNAP" 2>/dev/null || rm -rf "$SNAP"
  else
    echo "snapshot kept at $SNAP"
  fi
}
trap cleanup EXIT

# ── 1. image ────────────────────────────────────────────────────────────
if [ "$BUILD_IMAGE" = "1" ] || ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "=== building CI image $IMAGE ==="
  docker build -f docker/ci/Dockerfile -t "$IMAGE" docker/ci
fi

# ── 2. clean committed-source snapshot at exact SHA ─────────────────────
echo "=== snapshotting $SHA into $SNAP ==="
git worktree add --detach "$SNAP" "$SHA" >/dev/null
# Never carry protected soak evidence into the container.
rm -rf "$SNAP/docs/merger/evidence/2026-11-09-bn009-soak-full" 2>/dev/null || true

# ── 3. run gates in the container ───────────────────────────────────────
# Host paths mounted at IDENTICAL container paths so Cargo fingerprints,
# sccache keys and dep-info absolute paths stay valid.
HOST_HOME="$(eval echo ~$(whoami))"
mkdir -p "$ROOT/target" "$HOME/.cache" 2>/dev/null || true

LOG_DIR="$ROOT/target/ci/local-run-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$LOG_DIR"

set +e
docker run --rm \
  --name "bonsai-ci-$$" \
  --memory="$MEMORY" --memory-swap="$MEMORY" --cpus="$CPUS" \
  --user "$(id -u):$(id -g)" \
  --network bridge \
  -e CARGO_BUILD_JOBS="$JOBS" \
  -e CARGO_PROFILE=ci \
  -e GATE_BASE="${GATE_BASE:-origin/main}" \
  -e HOME="$HOST_HOME" \
  -e CARGO_HOME="$HOST_HOME/.cargo" \
  -e RUSTUP_HOME="$HOST_HOME/.rustup" \
  -e SCCACHE_DIR="$HOST_HOME/.cache/sccache" \
  -e PLAYWRIGHT_BROWSERS_PATH="$HOST_HOME/.cache/ms-playwright" \
  -v "$SNAP:$ROOT" \
  -v "$ROOT/target:$ROOT/target" \
  -v "$HOST_HOME/.cargo:$HOST_HOME/.cargo" \
  -v "$HOST_HOME/.rustup:$HOST_HOME/.rustup" \
  -v "$HOST_HOME/.cache/sccache:$HOST_HOME/.cache/sccache" \
  -v "$HOST_HOME/.cache/ms-playwright:$HOST_HOME/.cache/ms-playwright" \
  "$IMAGE" \
  bash -lc "
    set -o pipefail
    cd '$ROOT'
    rustc -Vv | head -2
    cargo --version; sccache --version 2>/dev/null || true
    mkdir -p target/ci
    scripts/ci/gates.sh ${TIERS[*]} 2>&1 | tee 'target/ci/local-gates-$(date +%s).log'
    exit \${PIPESTATUS[0]}
  "
RC=$?
set -e

echo "=== local CI finished: sha=$SHA image=$IMAGE tiers=${TIERS[*]} exit=$RC ==="
echo "logs: $LOG_DIR / target/ci/local-gates-*.log"
exit "$RC"
