#!/bin/sh
# D4-23 T1: linux-host1 startup script — starts lldpd so S-45/S-46 pass.
# nicolaka/netshoot already ships lldpd; we just need to start it.
# ContainerLab bind-mounts this as /startup.sh and the exec-cmd runs it.

set -e

# Install lldpd if not present (netshoot has it; fallback for other images)
if ! command -v lldpd >/dev/null 2>&1; then
    apt-get update -qq && apt-get install -y -q lldpd 2>/dev/null \
        || apk add --no-cache lldpd 2>/dev/null \
        || true
fi

# Start lldpd in background (if binary present)
if command -v lldpd >/dev/null 2>&1; then
    # -d: run as daemon, -L: enable LLDP-MED network endpoint, -x: enable CDP (for broader compat)
    lldpd -d -L || true
    echo "[host1-startup] lldpd started"
else
    echo "[host1-startup] WARNING: lldpd not found — S-45/S-46 will not pass"
fi

# Start softflowd for NetFlow v9 export to bonsai on leaf1 (eth1 toward leaf1)
if command -v softflowd >/dev/null 2>&1; then
    softflowd -i eth1 -n 172.100.109.1:2055 -v 9 -t maxlife=60 || true
    softflowd -i eth2 -n 172.100.109.1:2055 -v 9 -t maxlife=60 || true
    echo "[host1-startup] softflowd started on eth1+eth2"
fi

echo "[host1-startup] linux-host1 ready"
