#!/usr/bin/env bash
# scripts/cloud/daily_check_push.sh — Run daily check and push the report to GitHub.
#
# Wraps bv5_daily_check.sh: runs the health check, then commits and pushes the
# markdown report to main so every day's result is visible on GitHub.
#
# Called by cron at 02:30 UTC (bonsai-cloud-check tag).
# Works on both laptop (LAB_SCOPE=dc) and cloud (LAB_SCOPE=cloud-dc).
#
# Usage:
#   bash scripts/cloud/daily_check_push.sh [--no-push]
#   LAB_SCOPE=cloud-dc bash scripts/cloud/daily_check_push.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_DIR="${INSTALL_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

NO_PUSH=false
for arg in "$@"; do
    case "$arg" in
        --no-push) NO_PUSH=true ;;
        --help|-h)
            echo "Usage: $0 [--no-push]"
            exit 0
            ;;
    esac
done

# Source env for GITHUB_TOKEN — cron subprocesses don't inherit interactive exports
for _env_file in "$HOME/.bonsai.env" "/opt/bonsai/instance.env"; do
    if [[ -f "$_env_file" ]]; then
        # shellcheck source=/dev/null
        . "$_env_file"
        break
    fi
done

_log() { echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] $*"; }

_log "=== daily_check_push start (LAB_SCOPE=${LAB_SCOPE:-dc}) ==="

# ── Run daily check ───────────────────────────────────────────────────────────

bash "$INSTALL_DIR/scripts/bv5_daily_check.sh" || true
_log "bv5_daily_check.sh complete"

# ── Git commit + push ─────────────────────────────────────────────────────────

if $NO_PUSH; then
    _log "--no-push: skipping git commit+push"
    exit 0
fi

if [[ -z "${GITHUB_TOKEN:-}" ]]; then
    _log "WARN: GITHUB_TOKEN not set — report saved locally only"
    exit 0
fi

TODAY=$(date -u '+%Y-%m-%d')
cd "$INSTALL_DIR"

# Check if there's anything new in daily_runs/
HAS_NEW=false
if git ls-files --others --exclude-standard -- docs/test_results/daily_runs/ 2>/dev/null | grep -q .; then
    HAS_NEW=true
elif ! git diff --quiet HEAD -- docs/test_results/daily_runs/ 2>/dev/null; then
    HAS_NEW=true
fi

if ! $HAS_NEW; then
    _log "No new daily run report — nothing to commit"
    exit 0
fi

git add docs/test_results/daily_runs/

REMOTE_URL=$(git remote get-url origin 2>/dev/null || echo "")
if [[ -z "$REMOTE_URL" ]]; then
    _log "WARN: no git remote 'origin' — cannot push"
    exit 0
fi

AUTHED_URL="https://x-token:${GITHUB_TOKEN}@${REMOTE_URL#https://}"

git -c user.name="bonsai-daily-check" \
    -c user.email="noreply@bonsai" \
    commit -m "chore: daily check $TODAY" 2>/dev/null || \
    _log "Nothing new to commit"

if git push "$AUTHED_URL" HEAD:main 2>/dev/null; then
    _log "Pushed daily report to main (docs/test_results/daily_runs/$TODAY.md)"
else
    _log "WARN: git push failed — report committed locally but not pushed to GitHub"
fi
