#!/usr/bin/env bash
# CV7 T6-2 — Laptop pull-and-install.
#
# Pulls latest main + downloads the bonsai-x86_64-linux artifact from the most
# recent successful build.yml workflow run, verifies the git SHA matches the
# artifact name, installs into /usr/local/lib/bonsai/versions/<sha>/, switches
# the /usr/local/bin/bonsai symlink, restarts via the laptop wrapper.
#
# Requires: `gh` CLI authenticated with read access to the repo.
# Laptop only.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'

ENV_DETECTED="$(bash scripts/dev/whichenv.sh 2>/dev/null || echo unknown)"
if [[ "$ENV_DETECTED" == "mac-dev" ]]; then
  echo "${RED}Refused.${RESET} Laptop only." >&2
  exit 2
fi

command -v gh >/dev/null 2>&1 || { echo "${RED}gh CLI missing${RESET}" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "${RED}curl missing${RESET}" >&2; exit 1; }

INSTALL_ROOT="${INSTALL_ROOT:-/usr/local/lib/bonsai}"
SYMLINK="${SYMLINK:-/usr/local/bin/bonsai}"
RUN_AS_ROOT="$(if [[ $(id -u) -eq 0 ]]; then echo ""; else echo "sudo"; fi)"

# ── 1. Sync repo ──────────────────────────────────────────────────────────────
echo "${YELLOW}git pull origin main${RESET}"
git pull origin main

EXPECTED_SHA="$(git rev-parse HEAD)"
EXPECTED_SHORT="$(git rev-parse --short HEAD)"
echo "Target SHA: $EXPECTED_SHA (${EXPECTED_SHORT})"

# ── 2. Find a matching artifact from the most recent successful build run ────
echo "${YELLOW}locating latest build.yml run that matches HEAD${RESET}"
RUN_ID="$(gh run list --workflow=build.yml --branch=main --status=success --json databaseId,headSha \
  | jq -r --arg sha "$EXPECTED_SHA" '[.[] | select(.headSha==$sha)] | .[0].databaseId // empty')"

if [[ -z "$RUN_ID" ]]; then
  echo "${RED}No successful build.yml run found for SHA $EXPECTED_SHA${RESET}" >&2
  echo "  Newer commits may not have built yet, or older builds may have rotated out." >&2
  echo "  Trigger one via: gh workflow run build.yml" >&2
  exit 1
fi
echo "  found run: $RUN_ID"

# ── 3. Download the binary artifact ──────────────────────────────────────────
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

ARTIFACT_NAME="bonsai-x86_64-linux-${EXPECTED_SHORT}"
echo "${YELLOW}downloading $ARTIFACT_NAME${RESET}"
gh run download "$RUN_ID" --name "$ARTIFACT_NAME" --dir "$WORK" \
  || { echo "${RED}gh run download failed${RESET}" >&2; exit 1; }

[[ -f "$WORK/bonsai" ]] || { echo "${RED}artifact missing bonsai binary${RESET}" >&2; exit 1; }
chmod +x "$WORK/bonsai"

# ── 4. Install + symlink ──────────────────────────────────────────────────────
DEST="$INSTALL_ROOT/versions/$EXPECTED_SHA"
echo "${YELLOW}installing to $DEST${RESET}"
$RUN_AS_ROOT mkdir -p "$DEST"
$RUN_AS_ROOT install -m 0755 "$WORK/bonsai" "$DEST/bonsai"

echo "${YELLOW}switching symlink $SYMLINK → $DEST/bonsai${RESET}"
$RUN_AS_ROOT ln -sfn "$DEST/bonsai" "$SYMLINK"

# Record the install in a current-marker file so rollback knows what to revert.
$RUN_AS_ROOT bash -c "echo '$EXPECTED_SHA' > '$INSTALL_ROOT/current'"
$RUN_AS_ROOT bash -c "date -u +%Y-%m-%dT%H:%M:%SZ > '$DEST/.installed_at'"

# Also download bonpy SPA dist (T4-5) if present in the same run.
BONPY_ARTIFACT="bonpy-dist-${EXPECTED_SHORT}"
echo "${YELLOW}downloading bonpy SPA $BONPY_ARTIFACT (optional)${RESET}"
gh run download "$RUN_ID" --name "$BONPY_ARTIFACT" --dir "$WORK/bonpy" 2>/dev/null \
  && {
    echo "  installing bonpy dist to $REPO_ROOT/ui-bonpy/dist/"
    mkdir -p "$REPO_ROOT/ui-bonpy/dist"
    cp -r "$WORK/bonpy/"* "$REPO_ROOT/ui-bonpy/dist/"
  } || echo "  (bonpy artifact not available; bonsai UI alone will work)"

# ── 5. Restart via wrapper ────────────────────────────────────────────────────
echo "${YELLOW}restarting bonsai + sidecar${RESET}"
bash scripts/ops/teardown.sh || true
bash scripts/ops/start_bonsai_with_sidecar.sh

echo
echo "${GREEN}installed bonsai $EXPECTED_SHA${RESET}"
echo "verify: curl -s http://localhost:3000/health | jq"
echo "rollback: bash scripts/ops/rollback.sh <prior-sha>"
