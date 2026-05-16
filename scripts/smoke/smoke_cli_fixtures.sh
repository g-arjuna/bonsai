#!/usr/bin/env bash
# smoke_cli_fixtures.sh — D2-T2 (DV1)
#
# Validates all 40 CLI parser chain fixtures in tests/cli_fixtures/.
# For each fixture, sends a ParseRequest to the bonsai_native sidecar
# (or a mock) and asserts the expected_parsed keys are present in the response.
#
# Usage:
#   bash scripts/smoke/smoke_cli_fixtures.sh [--mock] [--sidecar-url URL]
#
# Options:
#   --mock          Skip live HTTP calls; pass if expected_parsed non-empty (fixture
#                   schema validation only). Safe to run on Mac without sidecars.
#   --sidecar-url   Override default bonsai_native URL (default: http://127.0.0.1:9102)
#
# Exit codes:
#   0  all fixtures passed
#   1  one or more fixtures failed

set -euo pipefail

FIXTURES_DIR="$(cd "$(dirname "$0")/../../tests/cli_fixtures" && pwd)"
SIDECAR_URL="http://127.0.0.1:9102"
MOCK=false
PASS=0
FAIL=0
SKIP=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --mock)         MOCK=true ;;
        --sidecar-url)  SIDECAR_URL="$2"; shift ;;
        *) echo "Unknown arg: $1"; exit 1 ;;
    esac
    shift
done

PY=$(command -v python3 || command -v python)
if [[ -z "$PY" ]]; then
    echo "ERROR: python3 not found on PATH" >&2
    exit 1
fi

# Python helper: validate one fixture
#   $1 = fixture YAML path
#   $2 = sidecar URL
#   $3 = mock (true/false)
run_fixture() {
    local fixture_path="$1"
    local url="$2"
    local mock="$3"

    "$PY" - "$fixture_path" "$url" "$mock" <<'PYEOF'
import sys, json, yaml, urllib.request, urllib.error

fixture_path, url, mock = sys.argv[1], sys.argv[2], sys.argv[3] == "true"

with open(fixture_path) as f:
    fixture = yaml.safe_load(f)

fid     = fixture.get("fixture_id", fixture_path)
vendor  = fixture.get("vendor", "")
command = fixture.get("command", "")
raw     = fixture.get("raw", "")
expected = fixture.get("expected_parsed", {})

if not expected:
    print(f"  SKIP {fid}: no expected_parsed defined")
    sys.exit(2)

if mock:
    print(f"  PASS {fid}: [mock] fixture schema valid (vendor={vendor}, command={command})")
    sys.exit(0)

payload = json.dumps({"vendor": vendor, "command_pattern": command, "raw_output": raw}).encode()
req = urllib.request.Request(
    url + "/parse",
    data=payload,
    headers={"Content-Type": "application/json"},
    method="POST",
)
try:
    with urllib.request.urlopen(req, timeout=10) as resp:
        result = json.loads(resp.read())
except urllib.error.URLError as e:
    print(f"  FAIL {fid}: sidecar unreachable: {e}")
    sys.exit(1)

parsed = result.get("parsed_json", {})
missing = []
for key in expected:
    if key not in parsed:
        missing.append(key)

if missing:
    print(f"  FAIL {fid}: missing keys in parsed response: {missing}")
    sys.exit(1)
else:
    print(f"  PASS {fid}")
    sys.exit(0)
PYEOF
}

echo "=== CLI Parser Fixture Smoke ==="
echo "    fixtures : $FIXTURES_DIR"
echo "    sidecar  : $SIDECAR_URL"
echo "    mock     : $MOCK"
echo ""

for fixture in "$FIXTURES_DIR"/*.yaml; do
    set +e
    run_fixture "$fixture" "$SIDECAR_URL" "$MOCK"
    rc=$?
    set -e
    case $rc in
        0) ((PASS++)) ;;
        2) ((SKIP++)) ;;
        *) ((FAIL++)) ;;
    esac
done

echo ""
echo "=== Results: PASS=$PASS  FAIL=$FAIL  SKIP=$SKIP ==="
if [[ $FAIL -gt 0 ]]; then
    exit 1
fi
exit 0
