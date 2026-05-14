#!/usr/bin/env bash
# CV7 T1-3 — Environment detector.
#
# Prints one of: mac-dev | ubuntu-ops | cloud-ops | unknown
# Exits 0 always (callers branch on stdout).
#
# Detection heuristics:
# - mac-dev   : uname -s == Darwin
# - cloud-ops : Linux + aarch64 + Oracle/OCI markers OR cloud-init metadata
# - ubuntu-ops: Linux + (x86_64 or arm) + containerlab installed or labs present
# - unknown   : anything else — caller should stop and ask the user
#
# See docs/operations/dev_vs_ops_boundary.md for what each environment may do.

set -u

kernel="$(uname -s 2>/dev/null || echo unknown)"
arch="$(uname -m 2>/dev/null || echo unknown)"

if [[ "$kernel" == "Darwin" ]]; then
  echo "mac-dev"
  exit 0
fi

if [[ "$kernel" == "Linux" ]]; then
  is_cloud=0
  # OCI / cloud markers — non-fatal if missing.
  if [[ -f /sys/class/dmi/id/sys_vendor ]] && grep -qi 'oracle' /sys/class/dmi/id/sys_vendor 2>/dev/null; then
    is_cloud=1
  fi
  if [[ -d /var/lib/cloud/instance ]] || [[ -f /etc/oci-instance ]]; then
    is_cloud=1
  fi
  if [[ "$arch" == "aarch64" || "$arch" == "arm64" ]] && (( is_cloud == 1 )); then
    echo "cloud-ops"
    exit 0
  fi

  # ubuntu-ops markers: containerlab installed OR ~/labs/ dir OR running clab networks.
  is_ops=0
  command -v containerlab >/dev/null 2>&1 && is_ops=1
  command -v clab          >/dev/null 2>&1 && is_ops=1
  [[ -d "$HOME/clab"       ]] && is_ops=1
  [[ -d "$HOME/labs"       ]] && is_ops=1
  [[ -d "$HOME/bonsai/lab" ]] && is_ops=1
  if command -v docker >/dev/null 2>&1 && docker network ls 2>/dev/null | grep -q '^clab-' ; then
    is_ops=1
  fi

  if (( is_ops == 1 )); then
    echo "ubuntu-ops"
    exit 0
  fi

  # Generic Linux with no ops markers — likely a fresh laptop or CI runner.
  echo "unknown"
  exit 0
fi

echo "unknown"
exit 0
