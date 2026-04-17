#!/usr/bin/env bash
# deploy-mv.sh — deploys the multi-vendor triangle lab (SRL + XRd + cRPD).
#
# Usage (from WSL):
#   bash /mnt/c/Users/arjun/Desktop/bonsai/lab/fast-iteration/deploy-mv.sh [deploy|destroy]

set -euo pipefail

ACTION=${1:-deploy}
SRC="/mnt/c/Users/arjun/Desktop/bonsai/lab/fast-iteration"
LINUX_LAB="/home/${SUDO_USER:-$USER}/bonsai-labs/multivendor"

case "$ACTION" in
  deploy)
    echo "Syncing lab files to $LINUX_LAB ..."
    mkdir -p "$LINUX_LAB"
    rsync -a --exclude='clab-*' "$SRC/" "$LINUX_LAB/"
    cd "$LINUX_LAB"
    sudo clab deploy -t multivendor.clab.yml
    # Copy SRL CA cert for bonsai TLS (XRd and cRPD use no-TLS)
    cp "$LINUX_LAB/clab-bonsai-mv/.tls/ca/ca.pem" "$SRC/mv-ca.pem"
    echo "CA cert copied to $SRC/mv-ca.pem"
    echo ""
    echo "Targets:"
    echo "  srl1  172.100.101.11:57400  (TLS, vendor: nokia_srl)"
    echo "  xrd1  172.100.101.21:57400  (no-TLS, vendor: cisco_xrd)"
    echo "  crpd1 172.100.101.31:50051  (no-TLS, vendor: juniper_crpd)"
    ;;
  destroy)
    if [ -d "$LINUX_LAB" ]; then
      cd "$LINUX_LAB"
      sudo clab destroy -t multivendor.clab.yml
    else
      echo "Lab dir $LINUX_LAB not found — nothing to destroy."
    fi
    ;;
  *)
    echo "Usage: $0 [deploy|destroy]"
    exit 1
    ;;
esac
