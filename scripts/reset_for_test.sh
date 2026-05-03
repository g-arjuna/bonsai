#!/usr/bin/env bash
# reset_for_test.sh — canonical pre-test reset.
#
# Wipes all bonsai-managed data in external services (NetBox, Splunk,
# Elasticsearch, ServiceNow) and restarts the bonsai-core container so the
# next test run starts from a clean state.  External services stay up.
#
# Usage:
#   scripts/reset_for_test.sh [--profile <compose-profile>]
#
# Environment variables (all optional, fall back to dev defaults):
#   BONSAI_PROFILE    — compose profile to restart (default: dev)
#   NETBOX_URL        — default: http://localhost:8000
#   NETBOX_TOKEN      — default: bonsai-dev-token
#   SPLUNK_URL        — default: http://localhost:8100
#   SPLUNK_HEC_URL    — default: http://localhost:8088
#   SPLUNK_USERNAME   — default: admin
#   SPLUNK_PASSWORD   — required if resetting Splunk
#   SPLUNK_HEC_TOKEN  — required if resetting Splunk
#   ELASTIC_URL       — default: http://localhost:9200

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

BONSAI_PROFILE="${BONSAI_PROFILE:-dev}"
NETBOX_URL="${NETBOX_URL:-http://localhost:8000}"
NETBOX_TOKEN="${NETBOX_TOKEN:-bonsai-dev-token}"
SPLUNK_URL="${SPLUNK_URL:-http://localhost:8100}"
SPLUNK_HEC_URL="${SPLUNK_HEC_URL:-http://localhost:8088}"
SPLUNK_USERNAME="${SPLUNK_USERNAME:-admin}"
ELASTIC_URL="${ELASTIC_URL:-http://localhost:9200}"

cd "$REPO_ROOT"

echo "=== bonsai reset_for_test ==="

# ── NetBox ────────────────────────────────────────────────────────────────────
if curl -sf "${NETBOX_URL}/api/" >/dev/null 2>&1; then
    echo "[1/4] Resetting NetBox ..."
    python scripts/seed_netbox.py --reset --url "$NETBOX_URL" --token "$NETBOX_TOKEN"
else
    echo "[1/4] NetBox not reachable at ${NETBOX_URL} — skipping"
fi

# ── Elasticsearch ─────────────────────────────────────────────────────────────
if curl -sf "${ELASTIC_URL}/_cluster/health" >/dev/null 2>&1; then
    echo "[2/4] Resetting Elasticsearch ..."
    python scripts/seed_elastic.py --reset --url "$ELASTIC_URL"
else
    echo "[2/4] Elasticsearch not reachable at ${ELASTIC_URL} — skipping"
fi

# ── Splunk ────────────────────────────────────────────────────────────────────
if [[ -n "${SPLUNK_PASSWORD:-}" && -n "${SPLUNK_HEC_TOKEN:-}" ]]; then
    if curl -sf "${SPLUNK_URL}/services" -u "${SPLUNK_USERNAME}:${SPLUNK_PASSWORD}" \
            --insecure >/dev/null 2>&1; then
        echo "[3/4] Resetting Splunk ..."
        python scripts/seed_splunk.py --reset \
            --url "$SPLUNK_URL" \
            --hec-url "$SPLUNK_HEC_URL" \
            --username "$SPLUNK_USERNAME" \
            --password "$SPLUNK_PASSWORD" \
            --hec-token "$SPLUNK_HEC_TOKEN"
    else
        echo "[3/4] Splunk not reachable at ${SPLUNK_URL} — skipping"
    fi
else
    echo "[3/4] SPLUNK_PASSWORD / SPLUNK_HEC_TOKEN not set — skipping Splunk reset"
fi

# ── Bonsai restart ────────────────────────────────────────────────────────────
echo "[4/4] Restarting bonsai (profile: ${BONSAI_PROFILE}) ..."
if docker compose --profile "$BONSAI_PROFILE" ps --quiet 2>/dev/null | grep -q .; then
    docker compose --profile "$BONSAI_PROFILE" restart
    echo "  bonsai restarted"
else
    echo "  bonsai not running under profile '${BONSAI_PROFILE}' — skipping restart"
fi

echo "=== reset_for_test complete ==="
