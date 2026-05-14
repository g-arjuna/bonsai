#!/usr/bin/env bash
# CV7 T6-4 — Rollback: switch the bonsai symlink to a prior installed version.
#
# Usage:
#   bash scripts/ops/rollback.sh <sha-prefix>   # switch to a specific version
#   bash scripts/ops/rollback.sh --list          # show installed versions
#
# Refuses on Mac per dev/ops boundary.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'

ENV_DETECTED="$(bash scripts/dev/whichenv.sh 2>/dev/null || echo unknown)"
if [[ "$ENV_DETECTED" == "mac-dev" ]]; then
  echo "${RED}Refused.${RESET} rollback runs on Ubuntu/cloud only." >&2
  exit 2
fi

INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
INSTALL_NAME="bonsai"
VERSIONS_DIR="${VERSIONS_DIR:-/usr/local/lib/bonsai/versions}"

list_versions() {
  echo "${YELLOW}Installed versions in $VERSIONS_DIR:${RESET}"
  if [[ ! -d "$VERSIONS_DIR" ]]; then
    echo "  (none)"
    return
  fi
  local current=""
  if [[ -L "$INSTALL_DIR/$INSTALL_NAME" ]]; then
    current="$(readlink -f "$INSTALL_DIR/$INSTALL_NAME" 2>/dev/null || true)"
  fi
  for v in "$VERSIONS_DIR"/*/; do
    [[ -d "$v" ]] || continue
    local sha; sha="$(basename "$v")"
    local mark="  "
    [[ "$v$INSTALL_NAME" == "$current" || "$v"bonsai == "$current" ]] && mark="${GREEN}* ${RESET}"
    echo "  $mark$sha"
  done
  if [[ -n "$current" ]]; then
    echo
    echo "current symlink: $current"
  fi
}

if [[ "${1:-}" == "--list" || "${1:-}" == "-l" || -z "${1:-}" ]]; then
  list_versions
  exit 0
fi

TARGET_PREFIX="$1"

# Find a matching SHA directory.
MATCHES=()
for v in "$VERSIONS_DIR"/*/; do
  [[ -d "$v" ]] || continue
  sha="$(basename "$v")"
  if [[ "$sha" == "$TARGET_PREFIX"* ]]; then
    MATCHES+=("$sha")
  fi
done

if (( ${#MATCHES[@]} == 0 )); then
  echo "${RED}No installed version matches '$TARGET_PREFIX'${RESET}" >&2
  list_versions >&2
  exit 1
fi
if (( ${#MATCHES[@]} > 1 )); then
  echo "${RED}Ambiguous prefix '$TARGET_PREFIX' — matches:${RESET}" >&2
  for m in "${MATCHES[@]}"; do echo "  $m" >&2; done
  exit 1
fi

TARGET_SHA="${MATCHES[0]}"
TARGET_BIN="$VERSIONS_DIR/$TARGET_SHA/bonsai"

[[ -x "$TARGET_BIN" ]] || { echo "${RED}missing binary: $TARGET_BIN${RESET}" >&2; exit 1; }

echo "${YELLOW}rolling back to $TARGET_SHA${RESET}"
echo "  stopping current bonsai…"
bash scripts/ops/teardown.sh || true
echo "  switching symlink"
sudo ln -sfn "$TARGET_BIN" "$INSTALL_DIR/$INSTALL_NAME"
echo "  $INSTALL_DIR/$INSTALL_NAME -> $TARGET_BIN"

# If running under systemd on cloud, restart the service. On laptop, the
# operator restarts manually with the wrapper.
if systemctl is-enabled bonsai.service >/dev/null 2>&1; then
  echo "  systemd unit detected — restarting bonsai.service"
  sudo systemctl restart bonsai.service
fi

echo
echo "${GREEN}rolled back to $TARGET_SHA${RESET}"
