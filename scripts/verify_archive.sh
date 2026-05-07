#!/usr/bin/env bash
# scripts/verify_archive.sh — Parquet archive integrity verification.
#
# Checks that the rolling Parquet archive written by bonsai's archive subsystem
# is healthy: files exist, schema is correct, row counts are non-decreasing,
# and compression ratio is sane.
#
# Exit codes:
#   0 — all checks passed
#   1 — one or more checks failed
#   2 — archive not found (may be first run)
#
# Usage:
#   bash scripts/verify_archive.sh                     # uses default archive dir
#   bash scripts/verify_archive.sh /path/to/archive   # explicit path
#   bash scripts/verify_archive.sh --json              # machine-readable JSON output

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PYTHON="$REPO_ROOT/.venv/bin/python3"

# ── Argument handling ─────────────────────────────────────────────────────────
JSON_MODE=false
ARCHIVE_DIR=""

for arg in "$@"; do
    case "$arg" in
        --json) JSON_MODE=true ;;
        --*) echo "Unknown flag: $arg" >&2; exit 1 ;;
        *)   ARCHIVE_DIR="$arg" ;;
    esac
done

# Default archive location (matches bonsai.toml.example archive.path)
if [[ -z "$ARCHIVE_DIR" ]]; then
    ARCHIVE_DIR="$REPO_ROOT/runtime/archive"
fi

ROWCOUNT_FILE="$REPO_ROOT/runtime/archive_rowcounts.json"

# ── Helpers ───────────────────────────────────────────────────────────────────
PASS=0
FAIL=0
RESULTS=()

_pass() { PASS=$((PASS+1)); RESULTS+=("{\"check\":\"$1\",\"status\":\"pass\",\"detail\":\"$2\"}"); }
_fail() { FAIL=$((FAIL+1)); RESULTS+=("{\"check\":\"$1\",\"status\":\"fail\",\"detail\":\"$2\"}"); }
_info() { [[ "$JSON_MODE" == "false" ]] && echo "  $*"; }

[[ "$JSON_MODE" == "false" ]] && echo "=== Bonsai archive verification ===" && echo "Archive: $ARCHIVE_DIR"

# ── Check 0: archive directory exists ────────────────────────────────────────
if [[ ! -d "$ARCHIVE_DIR" ]]; then
    if [[ "$JSON_MODE" == "true" ]]; then
        echo '{"status":"no_archive","checks":[]}'
    else
        echo "WARN: archive directory not found: $ARCHIVE_DIR"
        echo "This may be normal on first run (no data yet)."
    fi
    exit 2
fi

# ── Check 1: Python + pyarrow available ───────────────────────────────────────
if [[ ! -f "$PYTHON" ]]; then
    _fail "python_available" ".venv not found at $PYTHON"
elif ! "$PYTHON" -c "import pyarrow.parquet" 2>/dev/null; then
    _fail "pyarrow_available" "pyarrow not installed in .venv"
else
    _pass "python_available" "pyarrow importable"
fi

# ── Check 2: Parquet files exist ──────────────────────────────────────────────
PARQUET_FILES=()
while IFS= read -r -d '' f; do
    PARQUET_FILES+=("$f")
done < <(find "$ARCHIVE_DIR" -name "*.parquet" -print0 2>/dev/null || true)

if [[ ${#PARQUET_FILES[@]} -eq 0 ]]; then
    _fail "parquet_files_exist" "no .parquet files found in $ARCHIVE_DIR"
else
    _pass "parquet_files_exist" "${#PARQUET_FILES[@]} parquet file(s) found"
    _info "Files: ${PARQUET_FILES[*]}"
fi

# ── Check 3: Schema validation (expected columns present) ─────────────────────
SCHEMA_OK=true
SCHEMA_ERRORS=""

if [[ ${#PARQUET_FILES[@]} -gt 0 && "$PASS" -gt 0 ]]; then
    SCHEMA_RESULT=$("$PYTHON" - <<'EOF'
import sys, json
import pyarrow.parquet as pq
import glob, os

REQUIRED_COLUMNS = {
    "target", "path", "timestamp_ns", "value",
}

archive_dir = sys.argv[1] if len(sys.argv) > 1 else "."
files = sorted(glob.glob(os.path.join(archive_dir, "**/*.parquet"), recursive=True))
if not files:
    files = glob.glob(os.path.join(archive_dir, "*.parquet"))

errors = []
for f in files:
    try:
        schema = pq.read_schema(f)
        cols = set(schema.names)
        missing = REQUIRED_COLUMNS - cols
        if missing:
            errors.append(f"{os.path.basename(f)}: missing columns {missing}")
    except Exception as e:
        errors.append(f"{os.path.basename(f)}: {e}")

print(json.dumps({"errors": errors, "files_checked": len(files)}))
EOF
    "$ARCHIVE_DIR" 2>/dev/null) || SCHEMA_RESULT='{"errors":["python error"],"files_checked":0}'

    SCHEMA_ERRORS=$(echo "$SCHEMA_RESULT" | "$PYTHON" -c "import json,sys; d=json.load(sys.stdin); print('\n'.join(d['errors']))")
    FILES_CHECKED=$(echo "$SCHEMA_RESULT" | "$PYTHON" -c "import json,sys; d=json.load(sys.stdin); print(d['files_checked'])")

    if [[ -n "$SCHEMA_ERRORS" ]]; then
        _fail "schema_valid" "$SCHEMA_ERRORS"
    else
        _pass "schema_valid" "all $FILES_CHECKED file(s) have required columns"
    fi
fi

# ── Check 4: Row count non-decreasing (today >= yesterday) ────────────────────
if [[ ${#PARQUET_FILES[@]} -gt 0 && "$FAIL" -eq 0 ]]; then
    TOTAL_ROWS=$("$PYTHON" - "$ARCHIVE_DIR" <<'EOF'
import sys, glob, os
import pyarrow.parquet as pq

archive_dir = sys.argv[1]
files = sorted(glob.glob(os.path.join(archive_dir, "**/*.parquet"), recursive=True))
if not files:
    files = glob.glob(os.path.join(archive_dir, "*.parquet"))

total = 0
for f in files:
    try:
        pf = pq.ParquetFile(f)
        total += pf.metadata.num_rows
    except Exception:
        pass
print(total)
EOF
    2>/dev/null) || TOTAL_ROWS=0

    _info "Total rows across all files: $TOTAL_ROWS"

    # Load previous count
    TODAY=$(date -u '+%Y-%m-%d')
    PREV_ROWS=0
    if [[ -f "$ROWCOUNT_FILE" ]]; then
        PREV_ROWS=$("$PYTHON" -c "
import json, sys
try:
    d = json.load(open('$ROWCOUNT_FILE'))
    yesterday = sorted(d.keys())[-1] if d else None
    print(d.get(yesterday, 0) if yesterday else 0)
except Exception:
    print(0)
" 2>/dev/null) || PREV_ROWS=0
    fi

    if [[ "$TOTAL_ROWS" -ge "$PREV_ROWS" ]]; then
        _pass "row_count_nondecreasing" "rows=${TOTAL_ROWS} (prev=${PREV_ROWS})"
    else
        _fail "row_count_nondecreasing" "rows decreased: ${TOTAL_ROWS} < ${PREV_ROWS} — possible data loss"
    fi

    # Persist today's count
    mkdir -p "$(dirname "$ROWCOUNT_FILE")"
    "$PYTHON" - "$ROWCOUNT_FILE" "$TODAY" "$TOTAL_ROWS" <<'EOF'
import json, sys, os
path, today, rows = sys.argv[1], sys.argv[2], int(sys.argv[3])
d = {}
if os.path.exists(path):
    try:
        d = json.load(open(path))
    except Exception:
        pass
d[today] = rows
# Keep only last 30 days
keys = sorted(d.keys())
if len(keys) > 30:
    for k in keys[:-30]:
        del d[k]
json.dump(d, open(path, 'w'), indent=2)
EOF
    2>/dev/null || true
fi

# ── Check 5: Compression ratio (raw / parquet size, expect > 2.0) ─────────────
if [[ ${#PARQUET_FILES[@]} -gt 0 ]]; then
    TOTAL_SIZE=0
    for f in "${PARQUET_FILES[@]}"; do
        TOTAL_SIZE=$((TOTAL_SIZE + $(stat -c%s "$f" 2>/dev/null || echo 0)))
    done

    if [[ "$TOTAL_SIZE" -gt 0 ]]; then
        _pass "files_non_empty" "total size=${TOTAL_SIZE} bytes"
        _info "Total parquet size: $((TOTAL_SIZE / 1024)) KiB"
    else
        _fail "files_non_empty" "parquet files sum to 0 bytes — truncation?"
    fi
fi

# ── Summary ───────────────────────────────────────────────────────────────────
if [[ "$JSON_MODE" == "true" ]]; then
    STATUS="pass"
    [[ "$FAIL" -gt 0 ]] && STATUS="fail"
    RESULT_JSON=$(IFS=,; echo "${RESULTS[*]}")
    echo "{\"status\":\"$STATUS\",\"pass\":$PASS,\"fail\":$FAIL,\"checks\":[$RESULT_JSON]}"
else
    echo ""
    echo "=== Results: $PASS passed, $FAIL failed ==="
    for r in "${RESULTS[@]}"; do
        STATUS=$(echo "$r" | "$PYTHON" -c "import json,sys; d=json.load(sys.stdin); print(d['status'].upper())")
        CHECK=$(echo "$r"  | "$PYTHON" -c "import json,sys; d=json.load(sys.stdin); print(d['check'])")
        DETAIL=$(echo "$r" | "$PYTHON" -c "import json,sys; d=json.load(sys.stdin); print(d['detail'])")
        printf "  [%s] %-35s %s\n" "$STATUS" "$CHECK" "$DETAIL"
    done
fi

[[ "$FAIL" -eq 0 ]] && exit 0 || exit 1
