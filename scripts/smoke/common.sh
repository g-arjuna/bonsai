#!/usr/bin/env bash
set -euo pipefail

SMOKE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SMOKE_DIR}/../.." && pwd)"
BASE_URL="${BASE_URL:-${1:-http://127.0.0.1:3000}}"
RESULT_DIR="${REPO_ROOT}/runtime/driver_results"
mkdir -p "${RESULT_DIR}"

api_call() {
  local method="$1"
  local path="$2"
  local body="${3:-}"

  if [[ -n "${body}" ]]; then
    curl -sf -X "${method}" \
      -H "Content-Type: application/json" \
      -d "${body}" \
      "${BASE_URL}${path}"
  else
    curl -sf -X "${method}" "${BASE_URL}${path}"
  fi
}

write_result() {
  local name="$1"
  local status="$2"
  local summary="$3"
  local details_json="${4:-[]}"
  local result_file="${RESULT_DIR}/smoke_${name}.json"

  python3 - <<'PY' "${result_file}" "${name}" "${status}" "${summary}" "${BASE_URL}" "${details_json}"
import json
import sys
import time
from pathlib import Path

result_file, name, status, summary, base_url, details_json = sys.argv[1:]
details = json.loads(details_json)
payload = {
    "driver": f"smoke_{name}",
    "ts_unix": int(time.time()),
    "base_url": base_url,
    "status": status,
    "ok": status == "pass",
    "summary": summary,
    "checks": details,
}
Path(result_file).write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
print(json.dumps(payload, indent=2))
PY
}

finish_pass() {
  write_result "$1" "pass" "$2" "${3:-[]}"
}

finish_fail() {
  write_result "$1" "fail" "$2" "${3:-[]}"
  exit 1
}

finish_skip() {
  write_result "$1" "skip" "$2" "${3:-[]}"
}

read_config_value() {
  local dotted_key="$1"
  local config_path="${BONSAI_CONFIG:-${REPO_ROOT}/bonsai.toml}"
  python3 - <<'PY' "${config_path}" "${dotted_key}"
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib  # type: ignore

config_path = Path(sys.argv[1])
dotted_key = sys.argv[2]
if not config_path.exists():
    print("")
    raise SystemExit(0)

data = tomllib.loads(config_path.read_text(encoding="utf-8"))
node = data
for part in dotted_key.split("."):
    if not isinstance(node, dict) or part not in node:
        print("")
        raise SystemExit(0)
    node = node[part]

if isinstance(node, bool):
    print("true" if node else "false")
elif node is None:
    print("")
else:
    print(node)
PY
}
