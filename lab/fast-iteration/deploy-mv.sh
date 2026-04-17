#!/usr/bin/env bash
# deploy-mv.sh — deploys the multi-vendor quad lab (SRL + XRd + cRPD + cEOS).
#
# Usage (from WSL):
#   bash /mnt/c/Users/arjun/Desktop/bonsai/lab/fast-iteration/deploy-mv.sh [deploy|destroy] [--reconfigure]

set -euo pipefail

ACTION=${1:-deploy}
RECONFIGURE=${2:-}
SRC="/mnt/c/Users/arjun/Desktop/bonsai/lab/fast-iteration"
LINUX_LAB="/home/${SUDO_USER:-$USER}/bonsai-labs/multivendor"

case "$ACTION" in
  deploy)
    echo "Syncing lab files to $LINUX_LAB ..."
    mkdir -p "$LINUX_LAB"
    rsync -a --exclude='clab-*' "$SRC/" "$LINUX_LAB/"
    cd "$LINUX_LAB"
    CLAB_ARGS=(deploy -t multivendor.clab.yml)
    if [ "$RECONFIGURE" = "--reconfigure" ]; then
      CLAB_ARGS=(deploy --reconfigure -t multivendor.clab.yml)
      echo "Containerlab deploy will re-apply startup configs."
    elif [ -n "$RECONFIGURE" ]; then
      echo "Usage: $0 [deploy|destroy] [--reconfigure]"
      exit 1
    fi
    sudo clab "${CLAB_ARGS[@]}"
    # Copy SRL CA cert for bonsai TLS (XRd and cRPD use no-TLS)
    cp "$LINUX_LAB/clab-bonsai-mv/.tls/ca/ca.pem" "$SRC/mv-ca.pem"
    echo "CA cert copied to $SRC/mv-ca.pem"

    # Add Windows route so bonsai.exe can reach the 172.100.101.x management network.
    WSL_IP=$(ip addr show eth0 2>/dev/null | awk '/inet / {split($2,a,"/"); print a[1]; exit}')
    echo "Adding Windows route 172.100.101.0/24 via WSL ($WSL_IP) ..."
    powershell.exe -Command "route add 172.100.101.0 mask 255.255.255.0 $WSL_IP" 2>/dev/null \
      && echo "Route added." \
      || echo "NOTE: run this in an admin PowerShell: route add 172.100.101.0 mask 255.255.255.0 $WSL_IP"

    echo ""
    echo "gNMI targets:"
    echo "  srl1  172.100.101.11:57400  (TLS,    nokia_srl)"
    echo "  xrd1  172.100.101.21:57400  (no-TLS, cisco_xrd)"
    echo "  crpd1 172.100.101.31:50051  (no-TLS, juniper_crpd)"
    echo "  ceos1 172.100.101.41:6030   (no-TLS, arista_ceos)"
    echo ""
    echo "SSH access (from WSL — XRd takes ~5 min to enable SSH after boot):"
    echo "  ssh admin@172.100.101.11    # SRL   pass: NokiaSrl1!"
    echo "  ssh cisco@172.100.101.21    # XRd   pass: cisco123!"
    echo "  ssh admin@172.100.101.31    # cRPD  pass: admin@123"
    echo "  ssh admin@172.100.101.41    # cEOS  pass: admin"
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
    echo "Usage: $0 [deploy|destroy] [--reconfigure]"
    exit 1
    ;;
esac
