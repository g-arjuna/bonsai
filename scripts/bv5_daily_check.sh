#!/usr/bin/env bash
# scripts/bv5_daily_check.sh — BV5 daily laptop/cloud archive health note.
#
# Intended for the Tier 1 daily verification loop. It records the current Bonsai
# health endpoint, archive verifier output, chaos-runner status, and optional lab
# check into docs/test_results/daily_runs/<date>.md.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

DATE_UTC="$(date -u '+%Y-%m-%d')"
OUT_DIR="${OUT_DIR:-$REPO_ROOT/docs/test_results/daily_runs}"
OUT_FILE="${OUT_FILE:-$OUT_DIR/$DATE_UTC.md}"
API_BASE="${API_BASE:-http://127.0.0.1:3000}"
ARCHIVE_DIR="${ARCHIVE_DIR:-$REPO_ROOT/runtime/archive}"
LAB_SCOPE="${LAB_SCOPE:-dc}"

mkdir -p "$OUT_DIR"

_section() {
    printf '\n## %s\n\n' "$1"
}

_run_block() {
    local title="$1"
    shift
    _section "$title"
    printf '```text\n'
    "$@" 2>&1 || true
    printf '\n```\n'
}

{
    printf '# BV5 Daily Check — %s\n\n' "$DATE_UTC"
    printf 'Generated: %s\n\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

    _run_block "Bonsai Status" curl -fsS "$API_BASE/api/_test/status"
    _run_block "Archive Verification" bash "$REPO_ROOT/scripts/verify_archive.sh" "$ARCHIVE_DIR" --json
    _run_block "Chaos Runner Status" bash "$REPO_ROOT/scripts/chaos_runner.sh" --status
    _run_block "Lab Health" bash "$REPO_ROOT/scripts/check_lab.sh" "$LAB_SCOPE"

    _section "Operator Notes"
    printf -- '- Fill in any incidents, restarts, or known maintenance windows here.\n'
} > "$OUT_FILE"

echo "Wrote $OUT_FILE"
