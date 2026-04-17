#!/usr/bin/env bash
# deploy.sh — copies lab files to Linux-native filesystem before deploying.
# Needed because ContainerLab bind-mounts into SR Linux containers, and
# chmod inside those mounts fails on the Windows 9P (NTFS) filesystem.
#
# Usage (from WSL):
#   bash /mnt/c/Users/arjun/Desktop/bonsai/lab/fast-iteration/deploy.sh [deploy|destroy]

set -euo pipefail

ACTION=${1:-deploy}
SRC="/mnt/c/Users/arjun/Desktop/bonsai/lab/fast-iteration"
LINUX_LAB="/home/${SUDO_USER:-$USER}/bonsai-labs/fast-iteration"

case "$ACTION" in
  deploy)
    echo "Syncing lab files to $LINUX_LAB ..."
    mkdir -p "$LINUX_LAB"
    rsync -a --exclude='clab-*' "$SRC/" "$LINUX_LAB/"
    cd "$LINUX_LAB"
    sudo clab deploy -t 3node-srl.clab.yml
    # Copy clab CA cert to Windows project dir so the Rust binary can use it for TLS
    cp "$LINUX_LAB/clab-bonsai-srl/.tls/ca/ca.pem" "$SRC/ca.pem"
    echo "CA cert copied to $SRC/ca.pem"
    ;;
  destroy)
    if [ -d "$LINUX_LAB" ]; then
      cd "$LINUX_LAB"
      sudo clab destroy -t 3node-srl.clab.yml
    else
      echo "Lab dir $LINUX_LAB not found — nothing to destroy."
    fi
    ;;
  *)
    echo "Usage: $0 [deploy|destroy]"
    exit 1
    ;;
esac
