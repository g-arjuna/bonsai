#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASE_URL="${1:-http://127.0.0.1:3000}"

scripts=(
  "smoke_synthesizer.sh"
  "smoke_change_detection.sh"
  "smoke_yang_library.sh"
  "smoke_output_adapters.sh"
  "smoke_servicenow_aiops.sh"
  "smoke_signals_syslog.sh"
  "smoke_syslog_fixtures.sh"
  "smoke_signals_snmp.sh"
)

failed=0
for script in "${scripts[@]}"; do
  echo "==> ${script}"
  if ! "${SCRIPT_DIR}/${script}" "${BASE_URL}"; then
    failed=1
  fi
done

exit "${failed}"
