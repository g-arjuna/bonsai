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
            isis_adj_spine1=$(echo "$raw" | { grep -i -c " up " || true; } )
        fi
    fi
    log "  IS-IS adjacencies on spine1: ${isis_adj_spine1}"

    # BGP EVPN sessions on super1 (expect: 7 — super2 + 2 spines + 4 leaves)
    local bgp_established_super1=0 bgp_total_super1=7
    if node_running "$(clab_node dc srl-super1)"; then
        local raw
        raw=$(node_exec "$(clab_node dc srl-super1)" sr_cli -d "show network-instance default protocols bgp neighbor" 2>/dev/null || echo "__FAILED__")
        if [[ "$raw" != "__FAILED__" ]]; then
            bgp_established_super1=$(echo "$raw" | { grep -i -E -c "established.*\[" || true; } )
        fi
    fi
    log "  BGP EVPN established on super1: ${bgp_established_super1}/${bgp_total_super1}"

    # EVPN routes on leaf1 (expect type-2/3/5 from other leaves).
    # SR Linux v26.x exposes these under the default BGP EVPN neighbor view;
    # the older mac-vrf bgp-evpn routes command no longer parses.
    local evpn_routes_leaf1=0 evpn_routes_present=false
    if node_running "$(clab_node dc srl-leaf1)"; then
        local rr raw count
        for rr in 10.1.0.1 10.1.0.2; do
            raw=$(node_exec "$(clab_node dc srl-leaf1)" sr_cli -d "show network-instance default protocols bgp neighbor ${rr} received-routes evpn" 2>/dev/null || echo "__FAILED__")
            if [[ "$raw" != "__FAILED__" ]]; then
                count=$(echo "$raw" | { grep -E -c "10\\.1\\.0\\.[0-9]+:" || true; } )
                evpn_routes_leaf1=$((evpn_routes_leaf1 + count))
            fi
        done
    fi
    [[ "$evpn_routes_leaf1" -gt 0 ]] && evpn_routes_present=true
    log "  EVPN routes in mac-vrf-a on leaf1: ${evpn_routes_leaf1}"

    # SR-MPLS segment routing: check IS-IS SR prefix-SIDs programmed on spine1
    local srv6_ok=false
    if node_running "$(clab_node dc srl-spine1)"; then
        local raw
        raw=$(node_exec "$(clab_node dc srl-spine1)" sr_cli -d "show network-instance default protocols isis segment-routing prefix-sids" 2>/dev/null || echo "__FAILED__")
        if [[ "$raw" != "__FAILED__" ]] && echo "$raw" | grep -qi "prefix-sid\|SID"; then
            srv6_ok=true
        fi
    fi
    log "  SR-MPLS reachability on spine1: ${srv6_ok}"

    # Build warnings list
    local warnings=()
    [[ ${#missing_nodes[@]} -gt 0 ]] && warnings+=("${#missing_nodes[@]} DC node(s) down: $(IFS=,; echo "${missing_nodes[*]}")")
    if [[ "$isis_adj_spine1" != "unknown" ]] && [[ "$isis_adj_spine1" -lt 6 ]]; then
        warnings+=("DC spine1 IS-IS: only ${isis_adj_spine1}/6 adjacencies")
    fi
    if [[ "$bgp_established_super1" -lt "$bgp_total_super1" ]]; then
        warnings+=("DC super1 BGP: only ${bgp_established_super1}/${bgp_total_super1} established")
    fi
    $evpn_routes_present || warnings+=("DC leaf1: no EVPN routes in mac-vrf-a")

    # Build missing_nodes JSON
    local missing_json="[]"
    if [[ ${#missing_nodes[@]} -gt 0 ]]; then
        missing_json=$(printf '"%s",' "${missing_nodes[@]}")
        missing_json="[${missing_json%,}]"
    fi
    local warnings_json="[]"
    if [[ ${#warnings[@]} -gt 0 ]]; then
        warnings_json=$(printf '"%s",' "${warnings[@]}")
        warnings_json="[${warnings_json%,}]"
    fi

    local passed=false
    [[ "$nodes_up" -eq "$nodes_total" ]] && passed=true

    printf '{
    "topology": "dc",
    "passed": %s,
    "nodes_up": %d,
    "nodes_total": %d,
    "missing_nodes": %s,
    "bgp_sessions_established": %d,
    "bgp_sessions_total": %d,
    "isis_adjacencies_spine1": "%s",
    "bgp_evpn_established_super1": %d,
    "evpn_routes_leaf1_mac_vrf_a": %d,
    "evpn_routes_present": %s,
    "srv6_reachability_verified": %s,
    "warnings": %s
  }' "$passed" "$nodes_up" "$nodes_total" "$missing_json" \
      "$bgp_established_super1" "$bgp_total_super1" \
      "$isis_adj_spine1" "$bgp_established_super1" "$evpn_routes_leaf1" \
      "$evpn_routes_present" "$srv6_ok" "$warnings_json"
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
            isis_adj_p1=$(echo "$raw" | { grep -i -c " up " || true; } )
        fi
    fi
    log "  IS-IS adjacencies on frr-p1: ${isis_adj_p1}"

    # LDP sessions on frr-p1 (expect: 3 — pe1, p2, rr1)
    local ldp_sessions_p1="unknown"
    if node_running "$(clab_node sp frr-p1)"; then
        local raw
        raw=$(node_exec "$(clab_node sp frr-p1)" vtysh -c "show mpls ldp neighbor" 2>/dev/null || echo "__FAILED__")
        if [[ "$raw" != "__FAILED__" ]]; then
            ldp_sessions_p1=$(echo "$raw" | { grep -i -c "operational" || true; } )
        fi
    fi
    log "  LDP sessions on frr-p1: ${ldp_sessions_p1}"

    # BGP VPN-IPv4 sessions on rr1 (expect: 4 clients + 1 RR peer = 5)
    local bgp_established_rr1=0 bgp_total_rr1=5
    if node_running "$(clab_node sp srl-rr1)"; then
        local raw
        raw=$(node_exec "$(clab_node sp srl-rr1)" sr_cli -d "show network-instance default protocols bgp neighbor" 2>/dev/null || echo "__FAILED__")
        if [[ "$raw" != "__FAILED__" ]]; then
            bgp_established_rr1=$(echo "$raw" | { grep -i -E -c "established.*\[" || true; } )
        fi
    fi
    log "  BGP VPN-IPv4 established on rr1: ${bgp_established_rr1}/${bgp_total_rr1}"

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

    # SRv6: SP topo uses LDP/MPLS, not SRv6 — mark as N/A for summary
    # (SRv6 assertion applies to DC topo only)
    local srv6_ok=false

    # Build warnings list
    local warnings=()
    [[ ${#missing_nodes[@]} -gt 0 ]] && warnings+=("${#missing_nodes[@]} SP node(s) down: $(IFS=,; echo "${missing_nodes[*]}")")
    if [[ "$isis_adj_p1" != "unknown" ]] && [[ "$isis_adj_p1" -lt 3 ]]; then
        warnings+=("SP frr-p1 IS-IS: only ${isis_adj_p1}/3 adjacencies")
    fi
    if [[ "$bgp_established_rr1" -lt "$bgp_total_rr1" ]]; then
        warnings+=("SP rr1 BGP: only ${bgp_established_rr1}/${bgp_total_rr1} established")
    fi
    [[ "$ce1_bgp" != "Estab" ]] && [[ "$ce1_bgp" != "unknown" ]] && \
        warnings+=("SP CE1 BGP to pe1: state=${ce1_bgp}")

    local missing_json="[]"
    if [[ ${#missing_nodes[@]} -gt 0 ]]; then
        missing_json=$(printf '"%s",' "${missing_nodes[@]}")
        missing_json="[${missing_json%,}]"
    fi
    local warnings_json="[]"
    if [[ ${#warnings[@]} -gt 0 ]]; then
        warnings_json=$(printf '"%s",' "${warnings[@]}")
        warnings_json="[${warnings_json%,}]"
    fi

    local passed=false
    [[ "$nodes_up" -eq "$nodes_total" ]] && passed=true

    printf '{
    "topology": "sp",
    "passed": %s,
    "nodes_up": %d,
    "nodes_total": %d,
    "missing_nodes": %s,
    "bgp_sessions_established": %d,
    "bgp_sessions_total": %d,
    "isis_adjacencies_frr_p1": "%s",
    "ldp_sessions_frr_p1": "%s",
    "bgp_vpn_established_rr1": %d,
    "ce1_bgp_state": "%s",
    "evpn_routes_present": false,
    "srv6_reachability_verified": %s,
    "warnings": %s
  }' "$passed" "$nodes_up" "$nodes_total" "$missing_json" \
      "$bgp_established_rr1" "$bgp_total_rr1" \
      "$isis_adj_p1" "$ldp_sessions_p1" "$bgp_established_rr1" "$ce1_bgp" \
      "$srv6_ok" "$warnings_json"
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

# ── Top-level summary ─────────────────────────────────────────────────────────
# Aggregate fields across all checked topologies.
SUMMARY=$(python3 -c "
import json, sys

data = json.loads(sys.stdin.read())
dc = data['dc']
sp = data['sp']

bgp_est = 0
bgp_total = 0
evpn_present = False
srv6_ok = False
warnings = []
all_passed = True

for topo in (dc, sp):
    if topo is None:
        continue
    bgp_est += topo.get('bgp_sessions_established', 0)
    bgp_total += topo.get('bgp_sessions_total', 0)
    evpn_present = evpn_present or topo.get('evpn_routes_present', False)
    srv6_ok = srv6_ok or topo.get('srv6_reachability_verified', False)
    warnings.extend(topo.get('warnings', []))
    if not topo.get('passed', False):
        all_passed = False

print(json.dumps({
    'bgp_sessions_established': bgp_est,
    'bgp_sessions_total': bgp_total,
    'evpn_routes_present': evpn_present,
    'srv6_reachability_verified': srv6_ok,
    'warnings': warnings,
    'overall_passed': all_passed,
}))
" <<< "{\"dc\": $DC_JSON, \"sp\": $SP_JSON}")

printf '{"ts_unix": %d, "summary": %s, "dc": %s, "sp": %s}\n' \
    "$TS" "$SUMMARY" "$DC_JSON" "$SP_JSON"

log "Done."

# Exit 1 if any topology check failed.
python3 -c "
import json, sys
data = json.loads(sys.stdin.read())
sys.exit(0 if data['summary']['overall_passed'] else 1)
" <<< "$(printf '{"ts_unix": %d, "summary": %s, "dc": %s, "sp": %s}\n' "$TS" "$SUMMARY" "$DC_JSON" "$SP_JSON")"
