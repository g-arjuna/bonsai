#!/usr/bin/env bash
# probe-gnmi.sh — test gNMI capabilities and subscription paths for all
# multi-vendor lab devices, without rebuilding the Rust binary.
#
# Prerequisites (run from WSL):
#   bash -c "$(curl -sL https://get-gnmic.openconfig.net)"   # install gnmic
#   cp gnmi-creds.sh.example gnmi-creds.sh                   # fill credentials
#
# Usage:
#   bash probe-gnmi.sh                    # probe all devices
#   bash probe-gnmi.sh caps               # capabilities only
#   bash probe-gnmi.sh paths srl1         # path tests for one device
#   bash probe-gnmi.sh paths xrd1
#   bash probe-gnmi.sh paths crpd1
#   bash probe-gnmi.sh paths ceos1

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CREDS="$SCRIPT_DIR/gnmi-creds.sh"

# ── bootstrap ─────────────────────────────────────────────────────────────────

if ! command -v gnmic &>/dev/null; then
    echo "ERROR: gnmic not found. Install with:"
    echo "  bash -c \"\$(curl -sL https://get-gnmic.openconfig.net)\""
    exit 1
fi

if [[ ! -f "$CREDS" ]]; then
    echo "ERROR: $CREDS not found. Copy from gnmi-creds.sh.example and fill values."
    exit 1
fi

source "$CREDS"

MODE="${1:-all}"
TARGET="${2:-}"

# ── helpers ───────────────────────────────────────────────────────────────────

sep()   { echo; echo "══════════════════════════════════════════════════════"; echo "  $*"; echo "══════════════════════════════════════════════════════"; }
step()  { echo; echo "── $* ──────────────────────────────────────────────────"; }
ok()    { echo "  ✓ $*"; }
warn()  { echo "  ✗ $*"; }

# gnmic get — quick single-value fetch (uses Get RPC, simpler than Subscribe)
gnmi_get() {
    local addr="$1"; shift
    local path="$1"; shift
    local extra=("$@")
    gnmic --address "$addr" "${extra[@]}" get \
        --path "$path" \
        --encoding JSON_IETF \
        --format flat 2>&1 | head -40
}

# gnmic subscribe once — one streaming sample then exit
gnmi_sub_once() {
    local addr="$1"; shift
    local path="$1"; shift
    local extra=("$@")
    timeout 15 gnmic --address "$addr" "${extra[@]}" subscribe \
        --path "$path" \
        --mode once \
        --encoding JSON_IETF \
        --format flat 2>&1 | head -60 || true
}

# ── capabilities ─────────────────────────────────────────────────────────────

caps_srl() {
    sep "SRL1 — Nokia SR Linux ($SRL_ADDR)"
    step "Capabilities"
    gnmic --address "$SRL_ADDR" \
        --username "$SRL_USER" --password "$SRL_PASS" \
        --tls-ca "$SRL_CA" --tls-server-name "$SRL_TLS_SERVER" \
        capabilities --format flat 2>&1 \
        | grep -E "(supported-models|supported-encodings|gnmi-version|srl_nokia|openconfig)" \
        | head -30
}

caps_xrd() {
    sep "XRD1 — Cisco IOS-XRd ($XRD_ADDR)"
    step "Capabilities"
    gnmic --address "$XRD_ADDR" \
        --username "$XRD_USER" --password "$XRD_PASS" \
        --insecure \
        capabilities --format flat 2>&1 \
        | grep -E "(supported-models|supported-encodings|gnmi-version|Cisco-IOS-XR|openconfig)" \
        | head -30
}

caps_crpd() {
    sep "CRPD1 — Juniper cRPD ($CRPD_ADDR)"
    step "Capabilities (known to fail with internal auth error on 23.2R1.13)"
    gnmic --address "$CRPD_ADDR" \
        --insecure \
        capabilities --format flat 2>&1 \
        | grep -E "(supported-models|supported-encodings|gnmi-version|junos|openconfig)" \
        | head -30 || warn "Capabilities RPC failed — this is a known cRPD 23.2 bug"
}

caps_ceos() {
    sep "CEOS1 — Arista cEOS ($CEOS_ADDR)"
    step "Capabilities"
    gnmic --address "$CEOS_ADDR" \
        --username "$CEOS_USER" --password "$CEOS_PASS" \
        --insecure \
        capabilities --format flat 2>&1 \
        | grep -E "(supported-models|supported-encodings|gnmi-version|arista|EOS|openconfig)" \
        | head -30
}

# ── path tests ────────────────────────────────────────────────────────────────

paths_srl() {
    sep "SRL1 — path tests"
    local auth=(--username "$SRL_USER" --password "$SRL_PASS"
                --tls-ca "$SRL_CA" --tls-server-name "$SRL_TLS_SERVER")

    step "SRL native: interface statistics (expected: in-packets, out-packets, in-octets...)"
    gnmi_sub_once "$SRL_ADDR" \
        "interface[name=ethernet-1/1]/statistics" \
        "${auth[@]}" --encoding JSON_IETF

    step "SRL native: BGP neighbors (expected: session-state, peer-as...)"
    gnmi_sub_once "$SRL_ADDR" \
        "network-instance[name=default]/protocols/bgp/neighbor[peer-address=*]" \
        "${auth[@]}" --encoding JSON_IETF

    step "OC interfaces (for comparison — bonsai uses native for SRL)"
    gnmi_get "$SRL_ADDR" \
        "openconfig-interfaces:interfaces/interface[name=ethernet-1/1]/state/counters" \
        "${auth[@]}" --encoding JSON_IETF
}

paths_xrd() {
    sep "XRD1 — path tests"
    local auth=(--username "$XRD_USER" --password "$XRD_PASS" --insecure)

    step "XR NATIVE: infra-statsd-oper generic-counters (target for bonsai)"
    echo "  Path: Cisco-IOS-XR-infra-statsd-oper:infra-statistics/interfaces/interface[interface-name=*]/latest/generic-counters"
    gnmi_sub_once "$XRD_ADDR" \
        "Cisco-IOS-XR-infra-statsd-oper:infra-statistics/interfaces/interface[interface-name=*]/latest/generic-counters" \
        "${auth[@]}"

    step "OC interfaces (fallback — bonsai uses if XR native not detected)"
    echo "  Path: openconfig-interfaces:interfaces/interface[name=*]/state/counters"
    gnmi_get "$XRD_ADDR" \
        "openconfig-interfaces:interfaces/interface[name=*]/state/counters" \
        "${auth[@]}" --encoding JSON_IETF

    step "OC BGP network-instances (used by bonsai)"
    echo "  Path: openconfig-network-instance:network-instances/network-instance[name=default]/protocols/protocol/bgp/neighbors"
    gnmi_get "$XRD_ADDR" \
        "openconfig-network-instance:network-instances" \
        "${auth[@]}" --encoding JSON_IETF
}

paths_crpd() {
    sep "CRPD1 — path tests"
    local auth=(--insecure)

    step "Junos NATIVE interfaces (no origin — routes into junos-state tree)"
    echo "  Path: interfaces/interface[name=*]"
    gnmi_sub_once "$CRPD_ADDR" \
        "interfaces/interface[name=*]" \
        "${auth[@]}" --encoding JSON

    step "Junos NATIVE interfaces — specific interface to avoid wildcard rejection"
    echo "  Path: interfaces/interface[name=eth1]"
    gnmi_get "$CRPD_ADDR" \
        "interfaces/interface[name=eth1]" \
        "${auth[@]}" --encoding JSON

    step "OC interfaces with origin (known to fail on cRPD — for confirmation)"
    echo "  Path: openconfig-interfaces:interfaces  (expect: error)"
    gnmi_get "$CRPD_ADDR" \
        "openconfig-interfaces:interfaces" \
        "${auth[@]}" --encoding JSON 2>&1 || warn "OC interfaces rejected (expected on cRPD)"

    step "OC BGP via network-instances (used by bonsai — should work)"
    echo "  Path: openconfig-network-instance:network-instances"
    gnmi_sub_once "$CRPD_ADDR" \
        "openconfig-network-instance:network-instances" \
        "${auth[@]}" --encoding JSON
}

paths_ceos() {
    sep "CEOS1 — path tests"
    local auth=(--username "$CEOS_USER" --password "$CEOS_PASS" --insecure)

    step "OC interfaces (primary — cEOS has full OC support)"
    echo "  Path: openconfig-interfaces:interfaces/interface[name=*]/state/counters"
    gnmi_get "$CEOS_ADDR" \
        "openconfig-interfaces:interfaces/interface[name=*]/state/counters" \
        "${auth[@]}" --encoding JSON_IETF

    step "EOS NATIVE (eos_native provider — richer but complex Sysdb paths)"
    echo "  Path: eos_native:/Sysdb/interface/counter/eth/phy/slice"
    gnmi_get "$CEOS_ADDR" \
        "eos_native:/Sysdb/interface/counter/eth/phy/slice" \
        "${auth[@]}" --encoding JSON 2>&1 || warn "EOS native path not available or wrong path"

    step "OC BGP network-instances"
    echo "  Path: openconfig-network-instance:network-instances"
    gnmi_get "$CEOS_ADDR" \
        "openconfig-network-instance:network-instances" \
        "${auth[@]}" --encoding JSON_IETF

    step "OC LLDP (checking if cEOS supports it)"
    gnmi_get "$CEOS_ADDR" \
        "openconfig-lldp:lldp" \
        "${auth[@]}" --encoding JSON_IETF
}

# ── dispatch ─────────────────────────────────────────────────────────────────

run_caps() {
    case "${1:-all}" in
        srl1)  caps_srl  ;;
        xrd1)  caps_xrd  ;;
        crpd1) caps_crpd ;;
        ceos1) caps_ceos ;;
        *)     caps_srl; caps_xrd; caps_crpd; caps_ceos ;;
    esac
}

run_paths() {
    case "${1:-all}" in
        srl1)  paths_srl  ;;
        xrd1)  paths_xrd  ;;
        crpd1) paths_crpd ;;
        ceos1) paths_ceos ;;
        *)     paths_srl; paths_xrd; paths_crpd; paths_ceos ;;
    esac
}

case "$MODE" in
    caps)  run_caps  "$TARGET" ;;
    paths) run_paths "$TARGET" ;;
    all)
        run_caps
        echo
        echo "════════════════════════════════════════════════════════"
        echo "  PATH TESTS"
        echo "════════════════════════════════════════════════════════"
        run_paths
        ;;
    *)
        echo "Usage: $0 [all|caps|paths] [srl1|xrd1|crpd1|ceos1]"
        exit 1
        ;;
esac

echo
echo "Done. Compare field names above against src/graph.rs write_interface()."
