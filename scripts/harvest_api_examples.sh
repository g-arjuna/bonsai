#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${1:-http://localhost:3000}"
OUT_DIR="${2:-docs/openapi/examples/live}"

mkdir -p "$OUT_DIR"

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required" >&2
  exit 1
fi

have_jq=0
if command -v jq >/dev/null 2>&1; then
  have_jq=1
fi

write_get_example() {
  local path="$1"
  local outfile="$2"
  local url="${BASE_URL}${path}"

  echo "harvesting ${url} -> ${outfile}"
  if [[ "$have_jq" -eq 1 ]]; then
    curl --fail --silent --show-error "$url" | jq '.' > "${OUT_DIR}/${outfile}"
  else
    curl --fail --silent --show-error "$url" > "${OUT_DIR}/${outfile}"
  fi
}

write_post_example() {
  local path="$1"
  local outfile="$2"
  local body="$3"
  local url="${BASE_URL}${path}"

  echo "harvesting ${url} -> ${outfile}"
  if [[ "$have_jq" -eq 1 ]]; then
    curl --fail --silent --show-error \
      -H "content-type: application/json" \
      -d "$body" \
      "$url" | jq '.' > "${OUT_DIR}/${outfile}"
  else
    curl --fail --silent --show-error \
      -H "content-type: application/json" \
      -d "$body" \
      "$url" > "${OUT_DIR}/${outfile}"
  fi
}

write_get_example "/api/topology" "topology.json"
write_get_example "/api/detections?limit=5" "detections.json"
write_get_example "/api/incidents?window_secs=30&limit=20" "incidents.json"
write_get_example "/api/readiness" "readiness.json"
write_get_example "/api/operations" "operations.json"
write_get_example "/api/onboarding/devices" "managed_devices.json"
write_get_example "/api/setup/status" "setup_status.json"
write_get_example "/api/yang/modules" "yang_modules.json"
write_get_example "/api/yang/search?q=bgp%20neighbor%20state" "yang_search.json"
write_get_example "/api/profiles" "profiles.json"

if [[ "$have_jq" -eq 1 ]]; then
  first_address="$(jq -r '.devices[0].address // empty' "${OUT_DIR}/managed_devices.json")"
  first_alias="$(jq -r '.devices[0].credential_alias // empty' "${OUT_DIR}/managed_devices.json")"
  first_role="$(jq -r '.devices[0].role // empty' "${OUT_DIR}/managed_devices.json")"
  first_tls_domain="$(jq -r '.devices[0].tls_domain // empty' "${OUT_DIR}/managed_devices.json")"

  if [[ -n "$first_address" ]]; then
    write_get_example "/api/devices/${first_address}" "device_detail.json"
    write_get_example "/api/devices/${first_address}/gnmi-readiness" "device_gnmi_readiness.json"
    write_get_example "/api/devices/${first_address}/streaming-readiness" "device_streaming_readiness.json"
    write_get_example "/api/devices/${first_address}/recommendations" "device_recommendations.json"
  else
    echo "skipping device-specific harvest: no managed device found"
  fi

  if [[ -n "$first_address" && -n "$first_alias" ]]; then
    discovery_body="$(jq -n \
      --arg address "$first_address" \
      --arg credential_alias "$first_alias" \
      --arg tls_domain "$first_tls_domain" \
      --arg role_hint "$first_role" \
      '{address:$address, credential_alias:$credential_alias, tls_domain:$tls_domain, role_hint:$role_hint, environment_archetype:"data_center"}')"
    write_post_example "/api/onboarding/discover" "onboarding_discover.json" "$discovery_body"
  else
    echo "skipping onboarding discovery harvest: need managed device address + credential alias"
  fi
else
  echo "jq not installed; skipping device-specific and discovery example harvest"
fi

echo "examples written to ${OUT_DIR}"
