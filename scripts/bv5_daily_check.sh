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
RESULT_DIR="${RESULT_DIR:-$REPO_ROOT/runtime/driver_results}"
DAILY_JSON="${DAILY_JSON:-$RESULT_DIR/daily.json}"
ENSURE_CHAOS="${ENSURE_CHAOS:-true}"
PYTHON="${PYTHON:-$REPO_ROOT/.venv/bin/python3}"
[[ -x "$PYTHON" ]] || PYTHON="python3"

mkdir -p "$OUT_DIR"
mkdir -p "$RESULT_DIR"

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

_capture_cmd() {
    local outfile="$1"
    shift
    "$@" >"$outfile" 2>&1 || true
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

_driver_results_summary() {
    "$PYTHON" - "$REPO_ROOT" <<'PY'
from __future__ import annotations

import json
import sys
from pathlib import Path

driver_dir = Path(sys.argv[1]) / "runtime" / "driver_results"
if not driver_dir.exists():
    print("status: WARN - runtime/driver_results does not exist")
    raise SystemExit(0)

files = sorted(driver_dir.glob("*.json"))
if not files:
    print("status: WARN - no driver result files present")
    raise SystemExit(0)

counts = {"pass": 0, "fail": 0, "skip": 0, "prereq_missing": 0, "unknown": 0}
for path in files:
    # Exclude daily.json — it is a derived meta-file that would cause self-referential aggregation
    if path.name == "daily.json":
        continue
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except Exception as exc:
        print(f"{path.name}: invalid json - {exc}")
        counts["fail"] += 1
        continue

    status = payload.get("status")
    if status is None:
        if payload.get("failed", 0) == 0:
            status = "pass"
        else:
            status = "fail"
    if status not in counts:
        status = "unknown"
    counts[status] += 1
    summary = payload.get("summary", "")
    print(f"{path.name}: {status} {summary}".rstrip())

total = sum(counts.values())
print(
    f"totals: {total} files, pass={counts['pass']}, fail={counts['fail']}, "
    f"skip={counts['skip']}, prereq_missing={counts['prereq_missing']}, unknown={counts['unknown']}"
)
if counts["fail"] > 0:
    print("status: FAIL - at least one driver result reported failure")
elif counts["prereq_missing"] > 0 and counts["fail"] == 0:
    print("status: PASS_WITH_CAVEATS - some prerequisites not yet met; no real failures")
elif counts["pass"] == 0:
    print("status: WARN - no passing driver results recorded")
else:
    print("status: PASS - driver results aggregated cleanly")
PY
}

{
    printf '# BV5 Daily Check — %s\n\n' "$DATE_UTC"
    printf 'Generated: %s\n\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

    STATUS_TMP="$(mktemp)"
    DRIVER_TMP="$(mktemp)"
    ARCHIVE_TMP="$(mktemp)"
    CHAOS_STATUS_TMP="$(mktemp)"
    CHAOS_ENSURE_TMP="$(mktemp)"
    CHAOS_SUMMARY_TMP="$(mktemp)"
    LAB_TMP="$(mktemp)"
    trap 'rm -f "$STATUS_TMP" "$DRIVER_TMP" "$ARCHIVE_TMP" "$CHAOS_STATUS_TMP" "$CHAOS_ENSURE_TMP" "$CHAOS_SUMMARY_TMP" "$LAB_TMP"' EXIT

    _capture_cmd "$STATUS_TMP" curl -fsS "$API_BASE/api/_test/status"
    _capture_cmd "$DRIVER_TMP" _driver_results_summary
    _capture_cmd "$ARCHIVE_TMP" bash "$REPO_ROOT/scripts/verify_archive.sh" "$ARCHIVE_DIR" --json
    if [[ "${ENSURE_CHAOS}" == "true" ]]; then
        _capture_cmd "$CHAOS_ENSURE_TMP" bash "$REPO_ROOT/scripts/chaos_runner.sh" --ensure-running
        printf '\nChaos ensure step output captured before status check.\n' >>"$CHAOS_ENSURE_TMP"
    fi
    _capture_cmd "$CHAOS_STATUS_TMP" bash "$REPO_ROOT/scripts/chaos_runner.sh" --status
    _capture_cmd "$CHAOS_SUMMARY_TMP" _chaos_summary
    _capture_cmd "$LAB_TMP" bash "$REPO_ROOT/scripts/check_lab.sh" "$LAB_SCOPE"

    _section "Bonsai Status"
    printf '```text\n'
    cat "$STATUS_TMP"
    printf '\n```\n'

    _section "Driver Results Summary"
    printf '```text\n'
    cat "$DRIVER_TMP"
    printf '\n```\n'

    _section "Archive Verification"
    printf '```text\n'
    cat "$ARCHIVE_TMP"
    printf '\n```\n'

    _section "Chaos Runner Status"
    printf '```text\n'
    cat "$CHAOS_STATUS_TMP"
    if [[ -s "$CHAOS_ENSURE_TMP" ]]; then
        printf '\n'
        cat "$CHAOS_ENSURE_TMP"
    fi
    printf '\n```\n'

    _section "Chaos Cycle Summary"
    printf '```text\n'
    cat "$CHAOS_SUMMARY_TMP"
    printf '\n```\n'

    _section "Lab Health"
    printf '```text\n'
    cat "$LAB_TMP"
    printf '\n```\n'

    _section "Operator Notes"
    printf -- '- Fill in any incidents, restarts, or known maintenance windows here.\n'
} > "$OUT_FILE"

"$PYTHON" - <<'PY' "$REPO_ROOT" "$API_BASE" "$DATE_UTC" "$OUT_FILE" "$STATUS_TMP" "$DRIVER_TMP" "$ARCHIVE_TMP" "$CHAOS_STATUS_TMP" "$CHAOS_ENSURE_TMP" "$CHAOS_SUMMARY_TMP" "$LAB_TMP" "$DAILY_JSON" "$LAB_SCOPE"
from __future__ import annotations

import json
import sys
import time
from pathlib import Path

(
    repo_root,
    base_url,
    date_utc,
    out_file,
    status_file,
    driver_file,
    archive_file,
    chaos_status_file,
    chaos_ensure_file,
    chaos_summary_file,
    lab_file,
    daily_json,
    lab_scope,
) = sys.argv[1:]


def read_text(path: str) -> str:
    return Path(path).read_text(encoding="utf-8").strip()


def classify(text: str, pass_markers: list[str], warn_markers: list[str] | None = None) -> tuple[str, bool]:
    lowered = text.lower()
    warn_markers = warn_markers or []
    if any(marker in lowered for marker in ("status: fail", '"status":"fail"', '"status": "fail"', "error:", "curl: ", "not running")):
        return "fail", False
    if any(marker in lowered for marker in warn_markers):
        return "skip", False
    if any(marker in lowered for marker in pass_markers):
        return "pass", True
    return "skip", False


status_text = read_text(status_file)
driver_text = read_text(driver_file)
archive_text = read_text(archive_file)
chaos_status_text = read_text(chaos_status_file)
chaos_ensure_text = read_text(chaos_ensure_file)
chaos_summary_text = read_text(chaos_summary_file)
lab_text = read_text(lab_file)

checks = []

bonsai_status, bonsai_ok = classify(status_text, ['"ts_unix"', '"driver_results"'])
checks.append({"name": "bonsai_status", "check": "bonsai_status", "status": bonsai_status, "ok": bonsai_ok})

driver_status, driver_ok = classify(
    driver_text,
    ["status: pass - driver results aggregated cleanly"],
    ["status: warn", "status: pass_with_caveats"],
)
checks.append({"name": "driver_results", "check": "driver_results", "status": driver_status, "ok": driver_ok})

archive_status, archive_ok = classify(archive_text, ['"status":"pass"', '"status": "pass"'], ['"status":"warn"', '"status": "warn"'])
checks.append({"name": "archive_verification", "check": "archive_verification", "status": archive_status, "ok": archive_ok})

chaos_combined = "\n".join([chaos_status_text, chaos_ensure_text, chaos_summary_text]).lower()
if "status: pass - recent chaos injections present" in chaos_combined or "daemon is running" in chaos_combined:
    chaos_status, chaos_ok = "pass", True
elif "status: warn" in chaos_combined:
    chaos_status, chaos_ok = "skip", False
elif "status: fail" in chaos_combined or "daemon is not running" in chaos_combined:
    chaos_status, chaos_ok = "fail", False
else:
    chaos_status, chaos_ok = "skip", False
checks.append({"name": "chaos_runner_status", "check": "chaos_runner_status", "status": chaos_status, "ok": chaos_ok})

if '"overall_passed": false' in lab_text.lower() or '"passed": false' in lab_text.lower():
    lab_status, lab_ok = "fail", False
else:
    lab_status, lab_ok = classify(lab_text, ['"overall_passed": true', '"passed": true'])
checks.append({"name": "lab_health", "check": "lab_health", "status": lab_status, "ok": lab_ok})

top_status = "pass"
if any(check["status"] == "fail" for check in checks):
    top_status = "fail"
elif any(check["status"] in ("skip", "pass_with_caveats") for check in checks):
    top_status = "pass_with_caveats"

summary_bits = [
    f"bonsai={bonsai_status}",
    f"archive={archive_status}",
    f"driver_results={driver_status}",
    f"chaos={chaos_status}",
    f"lab={lab_status}",
]

payload = {
    "driver": "daily_check",
    "ts_unix": int(time.time()),
    "base_url": base_url,
    "status": top_status,
    "ok": top_status == "pass",
    "summary": f"daily check complete; {' '.join(summary_bits)}",
    "checks": checks,
    "artifacts": {
        "markdown_report": out_file,
    },
    "environment": {
        "git_sha": "unknown",
        "lab_scope": "unknown",
        "date_utc": date_utc,
    },
}

git_head = Path(repo_root, ".git", "HEAD")
if git_head.exists():
    try:
        import subprocess

        payload["environment"]["git_sha"] = subprocess.check_output(
            ["git", "-C", repo_root, "rev-parse", "--short", "HEAD"],
            text=True,
        ).strip()
    except Exception:
        pass

payload["environment"]["lab_scope"] = lab_scope
try:
    payload["artifacts"]["markdown_report"] = str(Path(out_file).resolve().relative_to(Path(repo_root).resolve()))
except Exception:
    payload["artifacts"]["markdown_report"] = out_file

Path(daily_json).write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
print(json.dumps(payload, indent=2))
PY

echo "Wrote $OUT_FILE"
echo "Wrote $DAILY_JSON"

# Archive a dated copy so /api/operations/weekly-trend can read the last 7 days.
DATED_JSON="${RESULT_DIR}/daily-${DATE_UTC}.json"
cp "$DAILY_JSON" "$DATED_JSON"
echo "Wrote $DATED_JSON"
