#!/usr/bin/env bash
# scripts/extract_lab_ca.sh — Copy ContainerLab-generated CA cert to the path
# expected by docker/configs/lab-dc.toml and lab-sp.toml.
#
# ContainerLab writes each topology's CA cert to:
#   <topo-dir>/clab-<name>/.tls/ca/ca.pem
#
# lab-dc.toml and lab-sp.toml expect:
#   lab/dc/ca.pem
#   lab/sp/ca.pem
#
# Run this once after each `clab deploy`:
#   scripts/extract_lab_ca.sh dc    # after clab deploy for DC lab
#   scripts/extract_lab_ca.sh sp    # after clab deploy for SP lab
#   scripts/extract_lab_ca.sh all   # both
#
# Safe to re-run — overwrites with latest cert.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
    echo "Usage: $0 dc | sp | all" >&2
    exit 1
}

extract_dc() {
    local src="${REPO_ROOT}/lab/dc/clab-bonsai-dc/.tls/ca/ca.pem"
    local dst="${REPO_ROOT}/lab/dc/ca.pem"
    if [[ ! -f "$src" ]]; then
        echo "ERROR: DC CA cert not found at $src" >&2
        echo "       Run: sudo clab deploy -t lab/dc/dc-evpn-srv6.clab.yml (from lab/dc/)" >&2
        return 1
    fi
    cp "$src" "$dst"
    echo "DC CA cert → lab/dc/ca.pem  ($(wc -c < "$dst") bytes)"
}

extract_sp() {
    local src="${REPO_ROOT}/lab/sp/clab-bonsai-sp/.tls/ca/ca.pem"
    local dst="${REPO_ROOT}/lab/sp/ca.pem"
    if [[ ! -f "$src" ]]; then
        echo "ERROR: SP CA cert not found at $src" >&2
        echo "       Run: sudo clab deploy -t lab/sp/sp-mpls-srte.clab.yml (from lab/sp/)" >&2
        return 1
    fi
    cp "$src" "$dst"
    echo "SP CA cert → lab/sp/ca.pem  ($(wc -c < "$dst") bytes)"
}

case "${1:-}" in
    dc)  extract_dc ;;
    sp)  extract_sp ;;
    all) extract_dc; extract_sp ;;
    *)   usage ;;
esac
