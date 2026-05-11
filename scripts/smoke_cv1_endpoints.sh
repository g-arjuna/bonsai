#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${1:-http://127.0.0.1:3000}"
CORE_CONTAINER="${BONSAI_CORE_CONTAINER:-}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESULT_DIR="${REPO_ROOT}/runtime/driver_results"
RESULT_FILE="${RESULT_DIR}/cv1_endpoints.json"
SMOKE_RESULT_FILE="${RESULT_DIR}/smoke_cv1_endpoints.json"

mkdir -p "$RESULT_DIR"

api_call() {
  local method="$1"
  local path="$2"
  local body="${3:-}"

  if [[ -n "$CORE_CONTAINER" ]]; then
    if [[ -n "$body" ]]; then
      docker run --rm --network "container:${CORE_CONTAINER}" \
        curlimages/curl:8.8.0 -sS -X "$method" \
        -H "Content-Type: application/json" \
        -d "$body" \
        "http://127.0.0.1:3000${path}"
    else
      docker run --rm --network "container:${CORE_CONTAINER}" \
        curlimages/curl:8.8.0 -sS -X "$method" \
        "http://127.0.0.1:3000${path}"
    fi
  else
    if [[ -n "$body" ]]; then
      curl -sf -X "$method" \
        -H "Content-Type: application/json" \
        -d "$body" \
        "${BASE_URL}${path}"
    else
      curl -sf -X "$method" "${BASE_URL}${path}"
    fi
  fi
}

TOPOLOGY="$(api_call GET "/api/topology")"
ADDRESS="$(python3 - <<'PY' "$TOPOLOGY"
import json
import sys

payload = json.loads(sys.argv[1])
devices = payload.get("devices", [])
if not devices:
    raise SystemExit("no devices returned by topology API")
address = devices[0].get("address")
if not address:
    raise SystemExit("first topology device has no address")
print(address)
PY
)"

REPARSE="$(api_call POST "/api/devices/${ADDRESS}/reparse" '{"reason":"cv2 sprint1 endpoint smoke"}')"
GNMI_READINESS="$(api_call GET "/api/devices/${ADDRESS}/gnmi-readiness")"
RECOMMENDATIONS="$(api_call GET "/api/devices/${ADDRESS}/recommendations")"
YANG_MODULES="$(api_call GET "/api/yang/modules")"
YANG_SEARCH="$(api_call GET "/api/yang/search?q=interface")"
sleep 2
CONFIG_HISTORY="$(api_call GET "/api/devices/${ADDRESS}/config-history")"

python3 - <<'PY' \
  "$BASE_URL" \
  "$ADDRESS" \
  "$REPARSE" \
  "$CONFIG_HISTORY" \
  "$GNMI_READINESS" \
  "$RECOMMENDATIONS" \
  "$YANG_MODULES" \
  "$YANG_SEARCH" \
  "$RESULT_FILE" \
  "$SMOKE_RESULT_FILE"
import json
import sys
from pathlib import Path

base_url, address, reparse_raw, config_history_raw, readiness_raw, recommendations_raw, modules_raw, search_raw, result_path, smoke_result_path = sys.argv[1:]
reparse = json.loads(reparse_raw)
config_history = json.loads(config_history_raw)
readiness = json.loads(readiness_raw)
recommendations = json.loads(recommendations_raw)
modules = json.loads(modules_raw)
search = json.loads(search_raw)

assert reparse["success"] is True
assert config_history["address"] == address
assert "snapshots" in config_history and "changes" in config_history
assert config_history["snapshots"], "config history returned no snapshots"
assert readiness["address"] == address and "report" in readiness
assert "report" in recommendations
assert "warnings" in recommendations["report"]
assert "modules" in modules
assert "result" in search

summary = {
    "status": "pass",
    "base_url": base_url,
    "device_address": address,
    "checks": [
        {"endpoint": f"/api/devices/{address}/reparse", "status": "pass"},
        {"endpoint": f"/api/devices/{address}/config-history", "status": "pass"},
        {"endpoint": f"/api/devices/{address}/gnmi-readiness", "status": "pass"},
        {"endpoint": f"/api/devices/{address}/recommendations", "status": "pass"},
        {"endpoint": "/api/yang/modules", "status": "pass"},
        {"endpoint": "/api/yang/search?q=interface", "status": "pass"},
    ],
}
Path(result_path).write_text(json.dumps(summary, indent=2) + "\n")
smoke_summary = {
    "driver": "smoke_cv1_endpoints",
    "ts_unix": __import__("time").time_ns() // 1_000_000_000,
    "base_url": base_url,
    "status": "pass",
    "ok": True,
    "summary": f"validated CV1 HTTP endpoints for {address}",
    "checks": summary["checks"],
}
Path(smoke_result_path).write_text(json.dumps(smoke_summary, indent=2) + "\n")
print(json.dumps(summary, indent=2))
PY
