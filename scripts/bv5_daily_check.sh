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
PYTHON="${PYTHON:-$REPO_ROOT/.venv/bin/python3}"
[[ -x "$PYTHON" ]] || PYTHON="python3"

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

_chaos_summary() {
    "$PYTHON" - "$REPO_ROOT" <<'PY'
from __future__ import annotations

import csv
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

repo = Path(sys.argv[1])
now_ns = int(datetime.now(timezone.utc).timestamp() * 1_000_000_000)
two_hours_ns = 2 * 60 * 60 * 1_000_000_000
day_ns = 24 * 60 * 60 * 1_000_000_000

cycles_24h = 0
injections_24h = 0
last_injection_ns: int | None = None
last_injection_desc = ""

for csv_path in sorted((repo / "chaos_runs").glob("*/injections.csv")):
    cycle_rows = 0
    cycle_recent = False
    try:
        with csv_path.open(newline="", encoding="utf-8") as fh:
            for row in csv.DictReader(fh):
                raw_ns = row.get("injected_at_ns")
                if not raw_ns:
                    continue
                injected_ns = int(raw_ns)
                cycle_rows += 1
                if last_injection_ns is None or injected_ns > last_injection_ns:
                    last_injection_ns = injected_ns
                    last_injection_desc = (
                        f"{row.get('fault_type', 'unknown')} on "
                        f"{row.get('hostname', 'unknown')} ({csv_path.parent.name})"
                    )
                if now_ns - injected_ns <= day_ns:
                    injections_24h += 1
                    cycle_recent = True
    except Exception as exc:
        print(f"WARN: unable to read {csv_path}: {exc}")
        continue
    if cycle_rows and cycle_recent:
        cycles_24h += 1

restart_markers_24h = 0
jsonl_path = repo / "runtime" / "chaos_log.jsonl"
if jsonl_path.exists():
    with jsonl_path.open(encoding="utf-8") as fh:
        for line in fh:
            try:
                record = json.loads(line)
                ts = datetime.fromisoformat(record["ts"].replace("Z", "+00:00"))
            except Exception:
                continue
            if record.get("event_type") == "restart_marker" and now_ns - int(ts.timestamp() * 1_000_000_000) <= day_ns:
                restart_markers_24h += 1

if last_injection_ns is None:
    print("chaos cycles in last 24h: 0")
    print("chaos injections in last 24h: 0")
    print("last chaos injection: none found")
    print("status: FAIL - no chaos injection records found")
else:
    age_seconds = (now_ns - last_injection_ns) / 1_000_000_000
    age_minutes = age_seconds / 60
    print(f"chaos cycles in last 24h: {cycles_24h}")
    print(f"chaos injections in last 24h: {injections_24h}")
    print(f"restart markers in last 24h: {restart_markers_24h}")
    print(f"last chaos injection: {age_minutes:.1f} minutes ago - {last_injection_desc}")
    if now_ns - last_injection_ns > two_hours_ns:
        print("status: FAIL - chaos has not run for more than 2 hours")
    elif injections_24h == 0:
        print("status: WARN - no injections completed in the last 24 hours")
    else:
        print("status: PASS - recent chaos injections present")
PY
}

{
    printf '# BV5 Daily Check — %s\n\n' "$DATE_UTC"
    printf 'Generated: %s\n\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

    _run_block "Bonsai Status" curl -fsS "$API_BASE/api/_test/status"
    _run_block "Archive Verification" bash "$REPO_ROOT/scripts/verify_archive.sh" "$ARCHIVE_DIR" --json
    _run_block "Chaos Runner Status" bash "$REPO_ROOT/scripts/chaos_runner.sh" --status
    _run_block "Chaos Cycle Summary" _chaos_summary
    _run_block "Lab Health" bash "$REPO_ROOT/scripts/check_lab.sh" "$LAB_SCOPE"

    _section "Operator Notes"
    printf -- '- Fill in any incidents, restarts, or known maintenance windows here.\n'
} > "$OUT_FILE"

echo "Wrote $OUT_FILE"
