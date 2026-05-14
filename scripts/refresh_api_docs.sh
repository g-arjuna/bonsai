#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${1:-http://localhost:3000}"
OUT_DIR="${2:-docs/openapi/examples/live}"

echo "refreshing live OpenAPI examples from ${BASE_URL}"
bash "$(dirname "$0")/harvest_api_examples.sh" "$BASE_URL" "$OUT_DIR"

cat <<EOF
live examples refreshed under ${OUT_DIR}
Swagger UI will prefer these files at runtime when present.
Canonical fallback examples remain under docs/openapi/examples/.
EOF
