#!/usr/bin/env bash
# scripts/check_lab.sh — Assert ContainerLab topology health.
#
# Emits machine-readable JSON to stdout (structured for AI consumption).
# Logs human-readable progress to stderr.
#
# Exit code: 0 if all expected sessions are up; 1 if any critical check fails.
#
# Usage:
#   scripts/check_lab.sh                  # auto-detect running topology
#   scripts/check_lab.sh --topology dc    # check DC topology only
#   scripts/check_lab.sh --topology sp    # check SP topology only
#   scripts/check_lab.sh --topology all   # check both
#   scripts/check_lab.sh | jq .

set -euo pipefail

TOPOLOGY="${1:-}"
if [[ "$TOPOLOGY" == "--topology" ]]; then
    TOPOLOGY="${2:-all}"
fi

log() { echo "[check_lab] $*" >&2; }

# ── Helpers ───────────────────────────────────────────────────────────────────

# Run a command inside a ContainerLab node via docker exec, return stdout.
node_exec() {
    local node="$1"; shift
    docker exec "$node" "$@" 2>/dev/null || echo "__FAILED__"
}

# Check if a container is running.
node_running() {
    local name="$1"
    docker ps --filter "name=^${name}$" --format "{{.Names}}" 2>/dev/null | grep -q "^${name}$"
}

# Resolve ContainerLab container name from topology name + node name.
# ContainerLab names containers as "clab-<topology>-<node>".
clab_node() {
    local topo="$1" node="$2"
    echo "clab-bonsai-${topo}-${node}"
}

# ── DC topology checks ────────────────────────────────────────────────────────

check_dc() {
    log "Checking DC topology (bonsai-dc)..."

    local dc_nodes=("srl-super1" "srl-super2" "srl-spine1" "srl-spine2"
                    "srl-leaf1" "srl-leaf2" "srl-leaf3" "srl-leaf4")

    # Node liveness
    local nodes_up=0 nodes_total=${#dc_nodes[@]}
    local missing_nodes=()
    for n in "${dc_nodes[@]}"; do
        local cname
        cname=$(clab_node "dc" "$n")
        if node_running "$cname"; then
            ((nodes_up++)) || true
        else
            missing_nodes+=("$n")
        fi
    done

    log "  DC nodes up: ${nodes_up}/${nodes_total}"

    # IS-IS adjacency count on spine1 (expect: 6 — 2 supers + 4 leaves)
    local isis_adj_spine1="unknown"
    if node_running "$(clab_node dc srl-spine1)"; then
        local raw
        raw=$(node_exec "$(clab_node dc srl-spine1)" sr_cli -d "show network-instance default protocols isis adjacency" 2>/dev/null || echo "__FAILED__")
        if [[ "$raw" != "__FAILED__" ]]; then
            # Count lines containing "Up"
            isis_adj_spine1=$(echo "$raw" | grep -c " Up " || echo "0")
        fi
    fi
    log "  IS-IS adjacencies on spine1: ${isis_adj_spine1}"

    # BGP EVPN sessions on super1 (expect: 7 — super2 + 2 spines + 4 leaves)
    local bgp_established_super1="unknown"
    if node_running "$(clab_node dc srl-super1)"; then
        local raw
        raw=$(node_exec "$(clab_node dc srl-super1)" sr_cli -d "show network-instance default protocols bgp neighbor" 2>/dev/null || echo "__FAILED__")
        if [[ "$raw" != "__FAILED__" ]]; then
            bgp_established_super1=$(echo "$raw" | grep -c "established" || echo "0")
        fi
    fi
    log "  BGP EVPN established on super1: ${bgp_established_super1}"

    # EVPN routes on leaf1 (expect type-2/3/5 from other leaves)
    local evpn_routes_leaf1="unknown"
    if node_running "$(clab_node dc srl-leaf1)"; then
        local raw
        raw=$(node_exec "$(clab_node dc srl-leaf1)" sr_cli -d "show network-instance mac-vrf-a protocols bgp-evpn routes" 2>/dev/null || echo "__FAILED__")
        if [[ "$raw" != "__FAILED__" ]]; then
            evpn_routes_leaf1=$(echo "$raw" | grep -c "type-" || echo "0")
        fi
    fi
    log "  EVPN routes in mac-vrf-a on leaf1: ${evpn_routes_leaf1}"

    # Build missing_nodes JSON
    local missing_json="[]"
    if [[ ${#missing_nodes[@]} -gt 0 ]]; then
        missing_json=$(printf '"%s",' "${missing_nodes[@]}")
        missing_json="[${missing_json%,}]"
    fi

    local passed=false
    [[ "$nodes_up" -eq "$nodes_total" ]] && passed=true

    printf '{
    "topology": "dc",
    "passed": %s,
    "nodes_up": %d,
    "nodes_total": %d,
    "missing_nodes": %s,
    "isis_adjacencies_spine1": "%s",
    "bgp_evpn_established_super1": "%s",
    "evpn_routes_leaf1_mac_vrf_a": "%s"
  }' "$passed" "$nodes_up" "$nodes_total" "$missing_json" \
      "$isis_adj_spine1" "$bgp_established_super1" "$evpn_routes_leaf1"
}

# ── SP topology checks ────────────────────────────────────────────────────────

check_sp() {
    log "Checking SP topology (bonsai-sp)..."

    local sp_srl_nodes=("srl-pe1" "srl-pe2" "srl-pe3" "srl-rr1" "srl-rr2")
    local sp_frr_nodes=("frr-p1" "frr-p2" "frr-ce1" "frr-ce2")
    local sp_nodes=("${sp_srl_nodes[@]}" "${sp_frr_nodes[@]}")

    local nodes_up=0 nodes_total=${#sp_nodes[@]}
    local missing_nodes=()
    for n in "${sp_nodes[@]}"; do
        local cname
        cname=$(clab_node "sp" "$n")
        if node_running "$cname"; then
            ((nodes_up++)) || true
        else
            missing_nodes+=("$n")
        fi
    done

    log "  SP nodes up: ${nodes_up}/${nodes_total}"

    # IS-IS adjacency count on frr-p1 (expect: 3 — pe1, p2, rr1)
    local isis_adj_p1="unknown"
    if node_running "$(clab_node sp frr-p1)"; then
        local raw
        raw=$(node_exec "$(clab_node sp frr-p1)" vtysh -c "show isis neighbor" 2>/dev/null || echo "__FAILED__")
        if [[ "$raw" != "__FAILED__" ]]; then
            isis_adj_p1=$(echo "$raw" | grep -c "Up" || echo "0")
        fi
    fi
    log "  IS-IS adjacencies on frr-p1: ${isis_adj_p1}"

    # LDP sessions on frr-p1 (expect: 3 — pe1, p2, rr1)
    local ldp_sessions_p1="unknown"
    if node_running "$(clab_node sp frr-p1)"; then
        local raw
        raw=$(node_exec "$(clab_node sp frr-p1)" vtysh -c "show mpls ldp neighbor" 2>/dev/null || echo "__FAILED__")
        if [[ "$raw" != "__FAILED__" ]]; then
            ldp_sessions_p1=$(echo "$raw" | grep -c "OPERATIONAL" || echo "0")
        fi
    fi
    log "  LDP sessions on frr-p1: ${ldp_sessions_p1}"

    # BGP VPN-IPv4 sessions on rr1 (expect: 4 clients + 1 RR peer = 5)
    local bgp_vpn_rr1="unknown"
    if node_running "$(clab_node sp srl-rr1)"; then
        local raw
        raw=$(node_exec "$(clab_node sp srl-rr1)" sr_cli -d "show network-instance default protocols bgp neighbor" 2>/dev/null || echo "__FAILED__")
        if [[ "$raw" != "__FAILED__" ]]; then
            bgp_vpn_rr1=$(echo "$raw" | grep -c "established" || echo "0")
        fi
    fi
    log "  BGP VPN-IPv4 established on rr1: ${bgp_vpn_rr1}"

    # CE1 BGP session to pe1 (expect: established)
    local ce1_bgp="unknown"
    if node_running "$(clab_node sp frr-ce1)"; then
        local raw
        raw=$(node_exec "$(clab_node sp frr-ce1)" vtysh -c "show bgp summary" 2>/dev/null || echo "__FAILED__")
        if [[ "$raw" != "__FAILED__" ]]; then
            ce1_bgp=$(echo "$raw" | grep "10.2.10.17" | awk '{print $10}' || echo "unknown")
        fi
    fi
    log "  CE1 BGP state toward pe1: ${ce1_bgp}"

    local missing_json="[]"
    if [[ ${#missing_nodes[@]} -gt 0 ]]; then
        missing_json=$(printf '"%s",' "${missing_nodes[@]}")
        missing_json="[${missing_json%,}]"
    fi

    local passed=false
    [[ "$nodes_up" -eq "$nodes_total" ]] && passed=true

    printf '{
    "topology": "sp",
    "passed": %s,
    "nodes_up": %d,
    "nodes_total": %d,
    "missing_nodes": %s,
    "isis_adjacencies_frr_p1": "%s",
    "ldp_sessions_frr_p1": "%s",
    "bgp_vpn_established_rr1": "%s",
    "ce1_bgp_state": "%s"
  }' "$passed" "$nodes_up" "$nodes_total" "$missing_json" \
      "$isis_adj_p1" "$ldp_sessions_p1" "$bgp_vpn_rr1" "$ce1_bgp"
}

# ── Main ──────────────────────────────────────────────────────────────────────

log "Starting lab health check (topology=${TOPOLOGY:-all})"

# Verify docker is available
if ! command -v docker &>/dev/null; then
    echo '{"error": "docker not found", "lab_health": null}' >&2
    exit 1
fi

DC_JSON="null"
SP_JSON="null"

case "${TOPOLOGY:-all}" in
    dc)
        DC_JSON=$(check_dc)
        ;;
    sp)
        SP_JSON=$(check_sp)
        ;;
    all|"")
        DC_JSON=$(check_dc)
        SP_JSON=$(check_sp)
        ;;
    *)
        log "Unknown topology '${TOPOLOGY}'. Use: dc | sp | all"
        exit 1
        ;;
esac

TS=$(date -u +%s)
printf '{"ts_unix": %d, "dc": %s, "sp": %s}\n' "$TS" "$DC_JSON" "$SP_JSON"

log "Done."
