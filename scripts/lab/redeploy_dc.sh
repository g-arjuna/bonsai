#!/usr/bin/env bash
# scripts/lab/redeploy_dc.sh — Full destroy+deploy of the DC lab topology.
#
# WHY full destroy instead of rolling update:
#   ContainerLab generates a fresh CA keypair on each deploy. A partial/rolling
#   update reconfigures existing nodes but does NOT regenerate their TLS certs.
#   This causes cert split-brain: some nodes present certs from the old CA, some
#   from the new CA, and bonsai can only trust one CA at a time. Symptoms: only
#   a subset of nodes get active gNMI subscriptions; topology appears disconnected.
#
#   Always destroy first → fresh CA → all nodes get matching certs → bonsai
#   force-recreated so it reads the new CA cert via bind mount.
#
# Usage:
#   bash scripts/lab/redeploy_dc.sh              # full redeploy + restart bonsai
#   bash scripts/lab/redeploy_dc.sh --topo-only  # topology only, skip bonsai restart
#   bash scripts/lab/redeploy_dc.sh --check      # verify state without redeploying

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
TOPO_FILE="$REPO_ROOT/lab/dc/dc-evpn-srv6.clab.yml"
CA_CERT="$REPO_ROOT/lab/dc/clab-bonsai-dc/.tls/ca/ca.pem"
COMPOSE_PROFILE="lab-dc"
BONSAI_CONTAINER="bonsai-bonsai-lab-dc-1"
API_BASE="${API_BASE:-http://127.0.0.1:3000}"

TOPO_ONLY=false
CHECK_ONLY=false
for arg in "$@"; do
    case "$arg" in
        --topo-only) TOPO_ONLY=true ;;
        --check)     CHECK_ONLY=true ;;
        --help|-h)
            echo "Usage: $0 [--topo-only] [--check]"
            echo "  --topo-only  Deploy topology only; do not restart bonsai"
            echo "  --check      Show current state without redeploying"
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

# ── --check: show current state ───────────────────────────────────────────────

if $CHECK_ONLY; then
    echo -e "${BOLD}=== DC Lab State ===${RESET}"
    echo ""
    echo "ContainerLab nodes:"
    docker ps --filter "name=clab-bonsai-dc" --format "  {{.Names}}\t{{.Status}}" 2>/dev/null || echo "  (docker unavailable)"
    echo ""
    echo "Bonsai container:"
    docker ps --filter "name=$BONSAI_CONTAINER" --format "  {{.Names}}\t{{.Status}}" 2>/dev/null || echo "  not running"
    echo ""
    echo "CA cert:"
    if [[ -f "$CA_CERT" ]]; then
        echo "  $CA_CERT"
        openssl x509 -in "$CA_CERT" -noout -fingerprint -dates 2>/dev/null | sed 's/^/  /'
    else
        echo "  NOT FOUND (topology not deployed)"
    fi
    echo ""
    echo "Subscriptions:"
    OBS=$(curl -sf --max-time 3 "$API_BASE/api/operations" 2>/dev/null | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('observed_subscriptions',0))" 2>/dev/null || echo "?")
    echo "  observed_subscriptions=$OBS"
    exit 0
fi

# ── Preflight ─────────────────────────────────────────────────────────────────

echo ""
echo -e "${BOLD}=== DC Lab Full Redeploy ===${RESET}"
echo "    Topology : $TOPO_FILE"
echo "    $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
echo ""

if [[ ! -f "$TOPO_FILE" ]]; then
    echo -e "${RED}ERROR: topology file not found: $TOPO_FILE${RESET}" >&2
    exit 1
fi

if ! command -v containerlab &>/dev/null; then
    echo -e "${RED}ERROR: containerlab not found on PATH${RESET}" >&2
    exit 1
fi

if ! command -v docker &>/dev/null; then
    echo -e "${RED}ERROR: docker not found on PATH${RESET}" >&2
    exit 1
fi

# ── Step 1: destroy existing topology ─────────────────────────────────────────

echo -e "${BOLD}[1/4] Destroying existing DC topology...${RESET}"
# --cleanup removes the clab-bonsai-dc/ directory including .tls/ so the next
# deploy generates a FRESH CA keypair. Without --cleanup, clab reuses the old
# CA cert on disk, which does NOT fix cert split-brain for nodes that have
# certs from an even older CA.
containerlab destroy -t "$TOPO_FILE" --cleanup --graceful 2>/dev/null || true
echo "      Done."
echo ""

# ── Step 2: deploy fresh ──────────────────────────────────────────────────────

echo -e "${BOLD}[2/4] Deploying DC topology (fresh CA + node certs)...${RESET}"
containerlab deploy -t "$TOPO_FILE"
echo ""

# Verify CA cert was generated
if [[ ! -f "$CA_CERT" ]]; then
    echo -e "${RED}ERROR: CA cert not found after deploy: $CA_CERT${RESET}" >&2
    exit 1
fi
CA_FP=$(openssl x509 -in "$CA_CERT" -noout -fingerprint 2>/dev/null | cut -d= -f2)
echo "      CA cert generated: $CA_FP"
echo ""

# ── Step 3: verify all node certs match the new CA ────────────────────────────

echo -e "${BOLD}[3/4] Verifying node TLS certs against new CA...${RESET}"
NODE_MAP=(
    "172.100.103.11:clab-bonsai-dc-srl-super1"
    "172.100.103.12:clab-bonsai-dc-srl-super2"
    "172.100.103.13:clab-bonsai-dc-srl-spine1"
    "172.100.103.14:clab-bonsai-dc-srl-spine2"
    "172.100.103.15:clab-bonsai-dc-srl-leaf1"
    "172.100.103.16:clab-bonsai-dc-srl-leaf2"
    "172.100.103.17:clab-bonsai-dc-srl-leaf3"
    "172.100.103.18:clab-bonsai-dc-srl-leaf4"
)
TLS_OK=0
TLS_FAIL=0
for entry in "${NODE_MAP[@]}"; do
    ip="${entry%%:*}"
    name="${entry##*:}"
    # SRL takes 60–90s to fully boot; use a short connect timeout and treat
    # failures as warnings only — bonsai retries subscriptions automatically.
    result=$(timeout 5 openssl s_client -connect "$ip:57400" -CAfile "$CA_CERT" \
        -servername "$name" </dev/null 2>&1 | grep "Verify return code" | head -1 || true)
    if echo "$result" | grep -q "0 (ok)"; then
        echo "  PASS  $name ($ip)"
        TLS_OK=$((TLS_OK + 1))
    else
        code=$(echo "$result" | grep -o '[0-9]* ([^)]*)' || echo "not ready")
        echo -e "  ${YELLOW}WAIT${RESET}  $name ($ip) — ${code} (node may still be booting)"
        TLS_FAIL=$((TLS_FAIL + 1))
    fi
done

if [[ "$TLS_FAIL" -gt 0 ]]; then
    echo ""
    echo -e "  ${YELLOW}$TLS_FAIL node(s) not yet TLS-ready.${RESET}"
    echo "  SRL takes 60–90s to fully boot. Bonsai retries automatically."
    echo "  Recheck: bash scripts/lab/redeploy_dc.sh --check"
fi
echo ""

if $TOPO_ONLY; then
    echo -e "${YELLOW}--topo-only: skipping bonsai restart.${RESET}"
    echo "Run: docker compose --profile $COMPOSE_PROFILE up -d --force-recreate"
    exit 0
fi

# ── Step 4: force-recreate bonsai so it reads new CA from bind mount ──────────

echo -e "${BOLD}[4/4] Force-recreating bonsai container (flush cached TLS config)...${RESET}"
cd "$REPO_ROOT"

# Stop first so subscriptions drain cleanly
docker compose --profile "$COMPOSE_PROFILE" stop 2>/dev/null || true

# Force-recreate picks up the new CA cert via bind mount
docker compose --profile "$COMPOSE_PROFILE" up -d --force-recreate

echo ""

# ── Wait for subscriptions ────────────────────────────────────────────────────

echo "Waiting for bonsai API to come up (up to 60s)..."
for i in $(seq 1 12); do
    if curl -sf --max-time 3 "$API_BASE/api/operations" &>/dev/null; then
        break
    fi
    sleep 5
done

OBS=$(curl -sf --max-time 5 "$API_BASE/api/operations" 2>/dev/null | \
    python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('observed_subscriptions',0))" 2>/dev/null || echo "?")
echo ""
echo -e "${GREEN}=== Redeploy complete ===${RESET}"
echo "  CA fingerprint      : $CA_FP"
echo "  TLS verified        : $TLS_OK/$((TLS_OK+TLS_FAIL)) nodes"
echo "  observed_subscriptions (current): $OBS"
echo ""
echo "  Note: SRL nodes take 60–90s to fully converge. Re-check subscriptions"
echo "  in 2 minutes: curl -s http://127.0.0.1:3000/api/operations | python3 -m json.tool"
echo ""
