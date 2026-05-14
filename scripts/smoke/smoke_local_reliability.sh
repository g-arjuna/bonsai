#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${1:-http://127.0.0.1:3100}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PYTHON_BIN="$REPO_ROOT/.venv/bin/python3"
INJECTOR="$REPO_ROOT/python/inject_fault.py"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

require() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

require curl
require docker
[[ -x "$PYTHON_BIN" ]] || fail "missing venv python at $PYTHON_BIN"
[[ -f "$INJECTOR" ]] || fail "missing injector at $INJECTOR"

ops_json="$(curl -sf "$BASE_URL/api/operations")" || fail "operations API unreachable at $BASE_URL"
latest_before="$(curl -sf "$BASE_URL/api/detections?limit=1" | python3 -c 'import json,sys; data=json.load(sys.stdin); items=data.get("detections", []); print(items[0]["fired_at_ns"] if items else 0)')" || latest_before=0

read -r observed silent collectors detection_before <<<"$(
  python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("observed_subscriptions",0), d.get("silent_subscriptions",0), d.get("collectors_connected",0), d.get("detection_events",0))' <<<"$ops_json"
)"

[[ "$observed" -gt 0 ]] || fail "observed_subscriptions must be > 0"
[[ "$silent" -eq 0 ]] || fail "silent_subscriptions must be 0"
[[ "$collectors" -ge 1 ]] || fail "collectors_connected must be >= 1"

echo "operations healthy: observed=$observed silent=$silent collectors=$collectors detections=$detection_before latest=$latest_before"

export BONSAI_FAULT_TRANSPORT=docker
"$PYTHON_BIN" "$INJECTOR" --topology bonsai-dc iface-down srl-leaf2 ethernet-1/1 >/dev/null
sleep 8
"$PYTHON_BIN" "$INJECTOR" --topology bonsai-dc iface-up srl-leaf2 ethernet-1/1 >/dev/null

for _ in $(seq 1 18); do
  current="$(curl -sf "$BASE_URL/api/operations" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("detection_events",0))')" || current="$detection_before"
  latest_now="$(curl -sf "$BASE_URL/api/detections?limit=1" | python3 -c 'import json,sys; data=json.load(sys.stdin); items=data.get("detections", []); print(items[0]["fired_at_ns"] if items else 0)')" || latest_now="$latest_before"
  if [[ "$current" -gt "$detection_before" || "$latest_now" -gt "$latest_before" ]]; then
    echo "fresh detection observed: detections $detection_before -> $current, latest $latest_before -> $latest_now"
    exit 0
  fi
  sleep 5
done

fail "no fresh detection appeared after interface flap"
