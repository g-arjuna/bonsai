#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/smoke/common.sh
source "${SCRIPT_DIR}/common.sh" "${1:-http://127.0.0.1:3000}"

FIXTURE_DIR="${REPO_ROOT}/tests/syslog_fixtures"

if [[ ! -d "${FIXTURE_DIR}" ]]; then
  finish_skip "syslog_fixtures" "fixture directory missing: ${FIXTURE_DIR}"
fi

DETAILS="$(python3 - <<'PY' "${BASE_URL}" "${FIXTURE_DIR}"
import json
import sys
import urllib.error
import urllib.request
from pathlib import Path

try:
    import yaml
except ModuleNotFoundError as exc:
    raise SystemExit(f"PyYAML is required for syslog fixture smoke: {exc}")

base_url = sys.argv[1].rstrip("/")
fixture_dir = Path(sys.argv[2])

details = []
failures = []

for fixture_path in sorted(fixture_dir.glob("*.yaml")):
    fixture = yaml.safe_load(fixture_path.read_text(encoding="utf-8"))
    fixture_id = fixture["fixture_id"]
    payload = {
        "raw": fixture["raw"],
        "vendor": fixture["vendor"],
        "transport": fixture.get("transport", "udp"),
        "peer_addr": fixture.get("peer_addr", "127.0.0.1:5514"),
    }
    req = urllib.request.Request(
        f"{base_url}/api/_test/syslog/parse",
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            result = json.loads(resp.read().decode("utf-8"))
    except urllib.error.URLError as exc:
        raise SystemExit(f"fixture endpoint unavailable for {fixture_id}: {exc}")

    check = {
        "name": fixture_id,
        "status": "pass",
        "vendor": fixture["vendor"],
        "adversarial": bool(fixture.get("adversarial", False)),
        "category": fixture.get("category", ""),
    }

    if result["event"]["category"] != fixture["expected_category"]:
        check["status"] = "fail"
        check["error"] = (
            f"expected category {fixture['expected_category']}, "
            f"got {result['event']['category']}"
        )
        failures.append(fixture_id)
        details.append(check)
        continue

    expected_trigger = bool(fixture.get("expected_signal_trigger", False))
    if bool(result["config_change_trigger"]) != expected_trigger:
        check["status"] = "fail"
        check["error"] = (
            f"expected config_change_trigger={expected_trigger}, "
            f"got {result['config_change_trigger']}"
        )
        failures.append(fixture_id)
        details.append(check)
        continue

    expected_fact_type = fixture.get("expected_fact_type", "") or ""
    if expected_fact_type:
        fact = next((item for item in result["facts"] if item["fact_type"] == expected_fact_type), None)
        if fact is None:
            check["status"] = "fail"
            check["error"] = f"expected fact_type {expected_fact_type}, got {[item['fact_type'] for item in result['facts']]}"
            failures.append(fixture_id)
            details.append(check)
            continue
        expected_fields = fixture.get("expected_fields", {}) or {}
        mismatches = []
        for key, value in expected_fields.items():
            actual = fact["fields"].get(key)
            if actual != value:
                mismatches.append(f"{key}={actual!r} expected {value!r}")
        if mismatches:
            check["status"] = "fail"
            check["error"] = "; ".join(mismatches)
            failures.append(fixture_id)
            details.append(check)
            continue
        check["fact_type"] = expected_fact_type
        check["fact_fields"] = fact["fields"]
    else:
        if result["facts"]:
            check["note"] = f"no fact assertion; parser returned {len(result['facts'])} fact(s)"

    details.append(check)

vendor_rollups = {}
category_rollups = {}

for item in details:
    vendor_key = item["vendor"]
    vendor_rollups.setdefault(vendor_key, {"pass": 0, "fail": 0, "adversarial": 0})
    vendor_rollups[vendor_key][item["status"]] += 1
    vendor_rollups[vendor_key]["adversarial"] += int(bool(item.get("adversarial")))

    category_key = item.get("category", "unknown") or "unknown"
    category_rollups.setdefault(category_key, {"pass": 0, "fail": 0})
    category_rollups[category_key][item["status"]] += 1

for vendor, counts in sorted(vendor_rollups.items()):
    details.append(
        {
            "name": f"vendor:{vendor}",
            "status": "fail" if counts["fail"] else "pass",
            "vendor": vendor,
            "kind": "vendor_summary",
            "fixture_count": counts["pass"] + counts["fail"],
            "adversarial_count": counts["adversarial"],
        }
    )

for category, counts in sorted(category_rollups.items()):
    details.append(
        {
            "name": f"category:{category}",
            "status": "fail" if counts["fail"] else "pass",
            "category": category,
            "kind": "category_summary",
            "fixture_count": counts["pass"] + counts["fail"],
        }
    )

if failures:
    print(json.dumps({"details": details, "failures": failures}))
    raise SystemExit(1)

print(json.dumps({"details": details, "failures": []}))
PY
)" || {
  FAILURE_DETAILS="$(python3 - <<'PY' "${DETAILS:-{}}"
import json, sys
raw = sys.argv[1]
try:
    parsed = json.loads(raw)
    print(json.dumps(parsed.get("details", [])))
except Exception:
    print("[]")
PY
)"
  finish_fail "syslog_fixtures" "fixture-driven syslog validation failed" "${FAILURE_DETAILS}"
}

DETAILS_JSON="$(python3 - <<'PY' "${DETAILS}"
import json, sys
parsed = json.loads(sys.argv[1])
print(json.dumps(parsed["details"]))
PY
)"

SUMMARY="$(python3 - <<'PY' "${DETAILS}"
import json, sys
parsed = json.loads(sys.argv[1])
details = parsed["details"]
fixtures = [item for item in details if item.get("kind") not in {"vendor_summary", "category_summary"}]
vendors = sorted({item["vendor"] for item in fixtures})
adversarial = sum(1 for item in fixtures if item.get("adversarial"))
categories = sorted({item.get("category", "") for item in fixtures if item.get("category")})
print(
    f"validated {len(fixtures)} syslog fixtures across {len(vendors)} vendors "
    f"and {len(categories)} categories; adversarial fixtures={adversarial}"
)
PY
)"

finish_pass "syslog_fixtures" "${SUMMARY}" "${DETAILS_JSON}"
