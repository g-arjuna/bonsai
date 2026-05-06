#!/usr/bin/env bash
# scripts/sprint1_preflight.sh — Pre-flight checks for Bv2-mod Sprint 1 stack bring-up.
#
# Verifies that every pre-condition is satisfied before you attempt to start
# ContainerLab, docker compose, or the bonsai stack. Run this first; fix what
# it flags before proceeding.
#
# Usage:
#   scripts/sprint1_preflight.sh           # check everything
#   scripts/sprint1_preflight.sh --lab dc  # only DC lab checks
#   scripts/sprint1_preflight.sh --lab sp  # only SP lab checks

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LAB_FILTER="${2:-all}"   # dc | sp | all

PASS=0
FAIL=0
WARN=0

ok()   { echo "  ✓ $*";  ((PASS++)) || true; }
fail() { echo "  ✗ $*" >&2; ((FAIL++)) || true; }
warn() { echo "  ⚠ $*";  ((WARN++)) || true; }
section() { echo ""; echo "── $* ──────────────────────────────────────"; }

# ── Parse args ────────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --lab) LAB_FILTER="${2:-all}"; shift 2 ;;
        *) shift ;;
    esac
done

echo "Bonsai Sprint 1 pre-flight check"
echo "Repo root: $REPO_ROOT"
echo "Lab filter: $LAB_FILTER"

# ── 1. Tools ──────────────────────────────────────────────────────────────────
section "Required tools"

command -v docker    &>/dev/null && ok "docker found" || fail "docker not found"
command -v clab      &>/dev/null && ok "containerlab (clab) found" || warn "clab not found — run lab deploy from WSL"

# docker compose v2
if docker compose version &>/dev/null 2>&1; then
    ok "docker compose v2 found"
else
    fail "docker compose v2 not found (need 'docker compose', not 'docker-compose')"
fi

command -v python3 &>/dev/null && ok "python3 found" || fail "python3 not found"
command -v curl    &>/dev/null && ok "curl found"    || fail "curl not found"

# ── 2. .env ───────────────────────────────────────────────────────────────────
section ".env file"

ENV_FILE="${REPO_ROOT}/.env"
if [[ ! -f "$ENV_FILE" ]]; then
    fail ".env not found — copy .env.example and fill in BONSAI_VAULT_PASSPHRASE + SPLUNK_* vars"
else
    ok ".env exists"
    # Check required vars are non-empty
    source "$ENV_FILE" 2>/dev/null || true
    [[ -n "${BONSAI_VAULT_PASSPHRASE:-}" ]] && ok "BONSAI_VAULT_PASSPHRASE set" \
        || fail "BONSAI_VAULT_PASSPHRASE is empty — bonsai compose will refuse to start"
    [[ -n "${SPLUNK_PASSWORD:-}" ]]         && ok "SPLUNK_PASSWORD set" \
        || warn "SPLUNK_PASSWORD empty — Splunk profile will fail"
    [[ -n "${SPLUNK_HEC_TOKEN:-}" ]]        && ok "SPLUNK_HEC_TOKEN set" \
        || warn "SPLUNK_HEC_TOKEN empty — Splunk HEC won't work"
fi

# ── 3. TLS certs for compose distributed mode ─────────────────────────────────
section "Compose TLS (two-collector mode)"

TLS_DIR="${REPO_ROOT}/docker/tls"
if [[ -d "$TLS_DIR" ]] && ls "$TLS_DIR"/*.pem &>/dev/null 2>&1; then
    ok "docker/tls/ has certs ($(ls "$TLS_DIR"/*.pem 2>/dev/null | wc -l) PEM files)"
else
    warn "docker/tls/ missing or empty — run: scripts/generate_compose_tls.sh (needed for two-collector mode)"
fi

# ── 4. DC lab pre-conditions ──────────────────────────────────────────────────
if [[ "$LAB_FILTER" == "dc" || "$LAB_FILTER" == "all" ]]; then
    section "DC lab (lab/dc/)"

    TOPO="${REPO_ROOT}/lab/dc/dc-evpn-srv6.clab.yml"
    [[ -f "$TOPO" ]] && ok "dc-evpn-srv6.clab.yml present" || fail "topology file missing: $TOPO"

    # Check all startup configs referenced in the topology are present
    for cfg in srl-super1 srl-super2 srl-spine1 srl-spine2 srl-leaf1 srl-leaf2 srl-leaf3 srl-leaf4; do
        [[ -f "${REPO_ROOT}/lab/dc/configs/${cfg}.cfg" ]] && ok "config: $cfg.cfg" \
            || fail "missing startup config: lab/dc/configs/${cfg}.cfg"
    done

    # CA cert — needed before bonsai-lab-dc can connect to gNMI
    CA_DC="${REPO_ROOT}/lab/dc/ca.pem"
    if [[ -f "$CA_DC" ]]; then
        ok "lab/dc/ca.pem present ($(wc -c < "$CA_DC") bytes)"
    else
        warn "lab/dc/ca.pem missing — deploy DC lab first, then run: scripts/extract_lab_ca.sh dc"
    fi

    # Check if DC clab state dir exists (indicates lab was deployed at some point)
    CLAB_DC="${REPO_ROOT}/lab/dc/clab-bonsai-dc"
    if [[ -d "$CLAB_DC" ]]; then
        ok "DC clab state dir exists (lab has been deployed before)"
    else
        warn "DC clab state dir not found — lab not yet deployed"
    fi
fi

# ── 5. SP lab pre-conditions ──────────────────────────────────────────────────
if [[ "$LAB_FILTER" == "sp" || "$LAB_FILTER" == "all" ]]; then
    section "SP lab (lab/sp/)"

    TOPO="${REPO_ROOT}/lab/sp/sp-mpls-srte.clab.yml"
    [[ -f "$TOPO" ]] && ok "sp-mpls-srte.clab.yml present" || fail "topology file missing: $TOPO"

    # FRR config dirs
    for node in frr-ce1 frr-ce2 frr-p1 frr-p2; do
        [[ -d "${REPO_ROOT}/lab/sp/configs/${node}" ]] && ok "config dir: $node" \
            || fail "missing config dir: lab/sp/configs/${node}"
    done
    for cfg in srl-pe1 srl-pe2 srl-pe3 srl-rr1 srl-rr2; do
        [[ -f "${REPO_ROOT}/lab/sp/configs/${cfg}.cfg" ]] && ok "config: $cfg.cfg" \
            || fail "missing startup config: lab/sp/configs/${cfg}.cfg"
    done

    CA_SP="${REPO_ROOT}/lab/sp/ca.pem"
    if [[ -f "$CA_SP" ]]; then
        ok "lab/sp/ca.pem present ($(wc -c < "$CA_SP") bytes)"
    else
        warn "lab/sp/ca.pem missing — deploy SP lab first, then run: scripts/extract_lab_ca.sh sp"
    fi

    CLAB_SP="${REPO_ROOT}/lab/sp/clab-bonsai-sp"
    if [[ -d "$CLAB_SP" ]]; then
        ok "SP clab state dir exists (lab has been deployed before)"
    else
        warn "SP clab state dir not found — lab not yet deployed"
    fi
fi

# ── 6. Python dependencies ────────────────────────────────────────────────────
section "Python dependencies"

VENV="${REPO_ROOT}/.venv"
if [[ -d "$VENV" ]]; then
    ok ".venv exists"
    PYTHON="${VENV}/bin/python3"
    "$PYTHON" -c "import requests" 2>/dev/null && ok "requests importable"  || fail "requests not installed in .venv — run: pip install -e python/[dev]"
    "$PYTHON" -c "import yaml"     2>/dev/null && ok "pyyaml importable"    || fail "pyyaml not installed"
else
    warn ".venv not found — create with: python3 -m venv .venv && .venv/bin/pip install -e python/[dev]"
    PYTHON="python3"
fi

# ── 7. Runtime dirs ───────────────────────────────────────────────────────────
section "Runtime directories"

mkdir -p "${REPO_ROOT}/runtime/driver_results" && ok "runtime/driver_results/ ready"
mkdir -p "${REPO_ROOT}/docs/test_results/chaos_matrix" && ok "docs/test_results/chaos_matrix/ ready"
mkdir -p "${REPO_ROOT}/docs/test_results/sprint1_operation" && ok "docs/test_results/sprint1_operation/ ready"

# ── 8. Bonsai image ───────────────────────────────────────────────────────────
section "Bonsai Docker image"

if docker image inspect bonsai:latest &>/dev/null 2>&1; then
    BUILD_DATE=$(docker inspect --format '{{.Created}}' bonsai:latest 2>/dev/null | cut -c1-10)
    ok "bonsai:latest image present (built $BUILD_DATE)"
else
    warn "bonsai:latest image not found — build with: docker compose build"
fi

# ── 9. External services (if already running) ─────────────────────────────────
section "External services (live check — skip if not yet started)"

check_url() {
    local name="$1" url="$2"
    if curl -sf --max-time 3 "$url" -o /dev/null 2>/dev/null; then
        ok "$name reachable ($url)"
    else
        warn "$name not reachable — start with: docker compose -f docker/compose-external.yml --profile all up -d"
    fi
}

check_url "NetBox"         "http://localhost:8000/api/"
check_url "Elasticsearch"  "http://localhost:9200/_cluster/health"
check_url "Prometheus"     "http://localhost:9093/-/ready"
check_url "Grafana"        "http://localhost:3001/api/health"

# ── 10. Bonsai API (if already running) ───────────────────────────────────────
section "Bonsai API (live check — skip if not yet started)"

if curl -sf --max-time 3 "http://localhost:3000/api/topology" -o /dev/null 2>/dev/null; then
    ok "bonsai API reachable at localhost:3000"
    # Quick sanity: topology populated?
    TOPO_RESP=$(curl -sf --max-time 5 "http://localhost:3000/api/topology" 2>/dev/null || echo "{}")
    DEVICE_COUNT=$(echo "$TOPO_RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); print(len(d.get('devices',d if isinstance(d,list) else [])))" 2>/dev/null || echo "?")
    if [[ "$DEVICE_COUNT" != "0" && "$DEVICE_COUNT" != "?" ]]; then
        ok "  topology populated ($DEVICE_COUNT devices)"
    else
        warn "  topology empty — gNMI collection not yet started or no devices connected"
    fi
else
    warn "bonsai API not reachable — start with: docker compose --profile lab-dc up -d"
fi

# ── Summary ───────────────────────────────────────────────────────────────────
echo ""
echo "════════════════════════════════════════"
echo "Pre-flight summary: ${PASS} pass  ${WARN} warn  ${FAIL} fail"
echo ""

if [[ $FAIL -gt 0 ]]; then
    echo "BLOCKED: fix the ${FAIL} failure(s) above before proceeding."
    echo ""
    echo "Next steps after fixing failures:"
fi

echo "Sprint 1 bring-up sequence (from WSL for clab, host for docker):"
echo ""
echo "  # 1. Build bonsai image (host)"
echo "  docker compose build"
echo ""
echo "  # 2. Start external services (host)"
echo "  docker compose -f docker/compose-external.yml --profile all up -d"
echo "  scripts/seed_external.sh"
echo "  scripts/check_external.sh"
echo ""
echo "  # 3. Deploy DC lab (WSL)"
echo "  cd lab/dc && sudo clab deploy -t dc-evpn-srv6.clab.yml --reconfigure"
echo ""
echo "  # 4. Extract DC CA cert (host, after step 3)"
echo "  scripts/extract_lab_ca.sh dc"
echo ""
echo "  # 5. Start bonsai against DC lab (host)"
echo "  docker compose --profile lab-dc up -d"
echo ""
echo "  # 6. Verify collection started (host)"
echo "  curl http://localhost:3000/api/topology | python3 -m json.tool"
echo "  curl http://localhost:3000/api/_test/status | python3 -m json.tool"
echo ""
echo "  # 7. Configure enrichment (host — after step 2 + 5)"
echo "  scripts/configure_external.sh"
echo "  # Merge generated sections from docker/configs/core.toml.generated into lab-dc.toml"
echo "  # Restart bonsai-lab-dc to pick up enrichment config"
echo ""
echo "  # 8. Run drivers (host)"
echo "  python tests/api_driver/run.py"
echo "  python tests/event_driver/run.py"
echo "  python tests/chaos_harness/run.py --topology dc --write-matrix"
echo "  cd tests/ui_driver && npx playwright test"
echo ""
echo "  # 9. Inject first fault (WSL)"
echo "  python tests/chaos_harness/run.py --fault dc-link-down-leaf2-spine1"
echo ""
echo "  # 10. Repeat steps 3-9 for SP lab"
echo "  cd lab/sp && sudo clab deploy -t sp-mpls-srte.clab.yml --reconfigure"
echo "  scripts/extract_lab_ca.sh sp"
echo "  docker compose --profile lab-sp up -d  # port 3001"
echo ""
echo "Document all failures in:"
echo "  docs/test_results/sprint1_operation/state-of-system-2026-05-05.md"
echo ""
[[ $FAIL -eq 0 ]] && exit 0 || exit 1
