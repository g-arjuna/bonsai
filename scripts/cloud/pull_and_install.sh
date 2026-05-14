#!/usr/bin/env bash
# CV7 T6-3 — Cloud variant of pull_and_install. Same shape as the laptop
# script, but downloads the aarch64 artefact and restarts the systemd
# services after install.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'

ENV_DETECTED="$(bash scripts/dev/whichenv.sh 2>/dev/null || echo unknown)"
if [[ "$ENV_DETECTED" == "mac-dev" ]]; then
  echo "${RED}Refused.${RESET} Cloud script — not for Mac." >&2
  exit 2
fi

INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
INSTALL_NAME="bonsai"
VERSIONS_DIR="${VERSIONS_DIR:-/usr/local/lib/bonsai/versions}"
ARTEFACT_NAME="bonsai-aarch64-linux"

echo "${YELLOW}step 1${RESET}: git pull origin main"
git fetch origin main
git reset --hard origin/main
HEAD_SHA="$(git rev-parse HEAD)"
echo "  now at $HEAD_SHA"

echo "${YELLOW}step 2${RESET}: locate latest build run for this SHA"
RUN_ID="$(gh run list --workflow=build.yml --branch=main --status=success --limit=20 --json databaseId,headSha \
  | python3 -c '
import json, sys
runs = json.load(sys.stdin)
sha = sys.argv[1]
for r in runs:
    if r["headSha"].startswith(sha[:12]):
        print(r["databaseId"]); sys.exit(0)
print("")
' "$HEAD_SHA")"

if [[ -z "$RUN_ID" ]]; then
  echo "${RED}No CI artefact found for $HEAD_SHA${RESET}" >&2
  exit 1
fi
echo "  run id $RUN_ID"

echo "${YELLOW}step 3${RESET}: download artefact $ARTEFACT_NAME"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT
gh run download "$RUN_ID" --name "$ARTEFACT_NAME" --dir "$WORK_DIR"
[[ -f "$WORK_DIR/bonsai" ]] || { echo "${RED}artefact missing bonsai binary${RESET}" >&2; exit 1; }

if [[ -f "$WORK_DIR/GIT_SHA" ]] && [[ "$(cat "$WORK_DIR/GIT_SHA")" != "$HEAD_SHA" ]]; then
  echo "${RED}SHA mismatch${RESET}" >&2; exit 1
fi
if [[ -f "$WORK_DIR/bonsai.sha256" ]]; then
  (cd "$WORK_DIR" && shasum -a 256 -c bonsai.sha256) || exit 1
fi

chmod +x "$WORK_DIR/bonsai"

echo "${YELLOW}step 4${RESET}: install to $VERSIONS_DIR/$HEAD_SHA"
sudo mkdir -p "$VERSIONS_DIR/$HEAD_SHA"
sudo install -m 0755 "$WORK_DIR/bonsai" "$VERSIONS_DIR/$HEAD_SHA/bonsai"
sudo ln -sfn "$VERSIONS_DIR/$HEAD_SHA/bonsai" "$INSTALL_DIR/$INSTALL_NAME"

echo "${YELLOW}step 5${RESET}: restart systemd units (bonsai → triggers BindsTo sidecar)"
sudo systemctl daemon-reload
sudo systemctl restart bonsai.service
sleep 2
sudo systemctl status bonsai.service --no-pager 2>&1 | head -8
echo
sudo systemctl status bonsai-rules-sidecar.service --no-pager 2>&1 | head -6

echo
echo "${GREEN}cloud install complete${RESET}"
echo "Health endpoint:"
curl -fsS http://localhost:3000/health || echo "  (not yet reachable)"
