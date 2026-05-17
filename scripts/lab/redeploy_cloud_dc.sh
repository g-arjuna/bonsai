#!/usr/bin/env bash
# scripts/lab/redeploy_cloud_dc.sh — Full destroy+deploy of the cloud DC topology.
#
# IMPORTANT: This script manages ContainerLab topology ONLY.
# Bonsai is always run as a native process (never via docker compose).
# After this script: bash scripts/ops/start_30day_run.sh
#
# Run on the cloud VM (Ubuntu ARM OCI instance) directly, or via:
#   ssh ubuntu@<cloud-vm-ip> "cd /opt/bonsai && bash scripts/lab/redeploy_cloud_dc.sh"
#
# Usage:
#   bash scripts/lab/redeploy_cloud_dc.sh        # deploy topology + copy CA cert
#   bash scripts/lab/redeploy_cloud_dc.sh --check  # verify state without redeploying

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
TOPO_FILE="$REPO_ROOT/lab/cloud-dc-6node.yml"
CA_CERT="$REPO_ROOT/lab/clab-bonsai-cloud-dc/.tls/ca/ca.pem"
API_BASE="${API_BASE:-http://127.0.0.1:3000}"

CHECK_ONLY=false
for arg in "$@"; do
    case "$arg" in
        --check)  CHECK_ONLY=true ;;
        --help|-h)
            echo "Usage: $0 [--check]"
            exit 0
            ;;
        *) echo "Unknown argument: $arg" >&2; exit 1 ;;
    esac
done

if [[ -t 1 ]]; then
    BOLD='\033[1m'; GREEN='\033[0;32m'; RED='\033[0;31m'; YELLOW='\033[0;33m'; RESET='\033[0m'
else
    BOLD='' GREEN='' RED='' YELLOW='' RESET=''
fi

# ── --check ───────────────────────────────────────────────────────────────────

if $CHECK_ONLY; then
    echo -e "${BOLD}=== Cloud DC Lab State ===${RESET}"
    echo ""
    echo "ContainerLab nodes:"
    docker ps --filter "name=clab-bonsai-cloud-dc" --format "  {{.Names}}\t{{.Status}}" 2>/dev/null || echo "  (none)"
    echo ""
    echo "CA cert:"
    if [[ -f "$CA_CERT" ]]; then
        echo "  $CA_CERT"
        openssl x509 -in "$CA_CERT" -noout -fingerprint -dates 2>/dev/null | sed 's/^/  /'
    else
        echo "  NOT FOUND"
    fi
    echo ""
    OBS=$(curl -sf --max-time 3 "$API_BASE/api/operations" 2>/dev/null | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('observed_subscriptions',0))" 2>/dev/null || echo "?")
    echo "Subscriptions: observed=$OBS"
    exit 0
fi

# ── Preflight ─────────────────────────────────────────────────────────────────

echo ""
echo -e "${BOLD}=== Cloud DC Full Redeploy ===${RESET}"
echo "    Topology : $TOPO_FILE"
echo "    $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
echo ""

[[ -f "$TOPO_FILE" ]] || { echo -e "${RED}ERROR: topology file not found: $TOPO_FILE${RESET}" >&2; exit 1; }
command -v containerlab &>/dev/null || { echo -e "${RED}ERROR: containerlab not on PATH${RESET}" >&2; exit 1; }

# ── Step 1: destroy ───────────────────────────────────────────────────────────

echo -e "${BOLD}[1/4] Stopping bonsai and destroying cloud-dc topology...${RESET}"
cd "$REPO_ROOT"
docker compose --profile "$COMPOSE_PROFILE" stop 2>/dev/null || true
containerlab destroy -t "$TOPO_FILE" --cleanup --graceful 2>/dev/null || true
echo "      Done."
echo ""

# ── Step 2: deploy ────────────────────────────────────────────────────────────

echo -e "${BOLD}[2/4] Deploying cloud-dc topology (fresh CA + node certs)...${RESET}"
containerlab deploy -t "$TOPO_FILE"
echo ""

[[ -f "$CA_CERT" ]] || { echo -e "${RED}ERROR: CA cert missing after deploy${RESET}" >&2; exit 1; }
CA_FP=$(openssl x509 -in "$CA_CERT" -noout -fingerprint 2>/dev/null | cut -d= -f2)
echo "      CA cert: $CA_FP"
echo ""

# ── Step 3: verify TLS (non-fatal — nodes may still be booting) ───────────────

echo -e "${BOLD}[3/4] Checking node TLS (warnings only — SRL takes 60–90s)...${RESET}"
NODE_MAP=(
    "172.100.104.11:clab-bonsai-cloud-dc-srl-super1"
    "172.100.104.12:clab-bonsai-cloud-dc-srl-spine1"
    "172.100.104.13:clab-bonsai-cloud-dc-srl-leaf1"
    "172.100.104.14:clab-bonsai-cloud-dc-srl-leaf2"
    "172.100.104.15:clab-bonsai-cloud-dc-srl-leaf3"
    "172.100.104.16:clab-bonsai-cloud-dc-srl-leaf4"
)
TLS_OK=0; TLS_FAIL=0
for entry in "${NODE_MAP[@]}"; do
    ip="${entry%%:*}"; name="${entry##*:}"
    result=$(timeout 5 openssl s_client -connect "$ip:57400" -CAfile "$CA_CERT" \
        -servername "$name" </dev/null 2>&1 | grep "Verify return code" | head -1 || true)
    if echo "$result" | grep -q "0 (ok)"; then
        echo "  PASS  $name ($ip)"; TLS_OK=$((TLS_OK+1))
    else
        code=$(echo "$result" | grep -o '[0-9]* ([^)]*)' || echo "not ready")
        echo -e "  ${YELLOW}WAIT${RESET}  $name ($ip) — $code"; TLS_FAIL=$((TLS_FAIL+1))
    fi
done
[[ "$TLS_FAIL" -gt 0 ]] && echo -e "\n  ${YELLOW}$TLS_FAIL node(s) still booting — bonsai retries automatically.${RESET}"
echo ""

echo -e "${GREEN}=== Topology redeploy complete ===${RESET}"
echo "  CA: $CA_FP"
echo "  TLS OK: $TLS_OK/$((TLS_OK+TLS_FAIL)) nodes at deploy time"
echo ""
echo "  SRL nodes take 60-90s to fully boot."
echo "  Next step: bash scripts/ops/start_30day_run.sh"
echo ""
