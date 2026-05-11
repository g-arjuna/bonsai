#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if ! command -v docker >/dev/null 2>&1; then
  echo "docker is required for sidecar smoke tests" >&2
  exit 1
fi

PYATS_IMAGE="bonsai-pyats-sidecar:smoke"
NATIVE_IMAGE="bonsai-native-parser-sidecar:smoke"
PYATS_CONTAINER="bonsai-pyats-sidecar-smoke"
NATIVE_CONTAINER="bonsai-native-parser-smoke"
PYATS_PORT="${BONSAI_PYATS_SMOKE_PORT:-19101}"
NATIVE_PORT="${BONSAI_NATIVE_SMOKE_PORT:-19102}"

cleanup() {
  docker rm -f "$PYATS_CONTAINER" "$NATIVE_CONTAINER" >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker build -t "$PYATS_IMAGE" -f docker/sidecars/pyats/Dockerfile . >/dev/null
docker build -t "$NATIVE_IMAGE" -f docker/sidecars/bonsai-native-parser/Dockerfile . >/dev/null

docker run -d --rm --name "$PYATS_CONTAINER" -p "${PYATS_PORT}:9101" \
  "$PYATS_IMAGE" uvicorn app:app --host 0.0.0.0 --port 9101 >/dev/null
docker run -d --rm --name "$NATIVE_CONTAINER" -p "${NATIVE_PORT}:9102" \
  "$NATIVE_IMAGE" uvicorn app:app --host 0.0.0.0 --port 9102 >/dev/null

for url in "http://127.0.0.1:${PYATS_PORT}/healthz" "http://127.0.0.1:${NATIVE_PORT}/healthz"; do
  for _ in $(seq 1 20); do
    if curl -sf "$url" >/dev/null; then
      break
    fi
    sleep 1
  done
  curl -sf "$url" >/dev/null
done

REQUEST='{"parser":"pyats_genie","vendor":"cisco-iosxr","command_pattern":"show bgp summary","raw_output":"Neighbor 10.0.0.1 Established"}'
PYATS_RESPONSE="$(curl -sf -X POST "http://127.0.0.1:${PYATS_PORT}/parse" -H "Content-Type: application/json" -d "$REQUEST")"
NATIVE_RESPONSE="$(curl -sf -X POST "http://127.0.0.1:${NATIVE_PORT}/parse" -H "Content-Type: application/json" -d "$REQUEST")"

python3 - <<'PY' "$PYATS_RESPONSE" "$NATIVE_RESPONSE"
import json
import sys

labels = ["pyats", "native"]
for label, raw in zip(labels, sys.argv[1:]):
    payload = json.loads(raw)
    if "parser" not in payload or "parsed_json" not in payload:
        raise SystemExit(f"{label} response missing expected keys")
    parsed = payload["parsed_json"]
    if parsed.get("command_pattern") != "show bgp summary":
        raise SystemExit(f"{label} sidecar returned wrong command pattern")
print("PASS: sidecar smoke responses validated")
PY
