#!/bin/sh
# Runs as container root before watchfrr, using iproute2 from the stock image.
# This is intentionally NOT an FRR daemon capability workaround.
set -eu

node="${1:-}"
case "$node" in frr1|frr2) ;; *) echo "Unexpected node: $node" >&2; exit 2 ;; esac

# Containerlab creates the container and attaches the data links afterwards.
# Do not start FRR until BOTH expected links are present.
tries=0
until [ -e /sys/class/net/eth1 ] && [ -e /sys/class/net/eth2 ]; do
  tries=$((tries + 1))
  if [ "$tries" -ge 150 ]; then
    echo "ERROR: timed out waiting for containerlab eth1 and eth2" >&2
    exit 1
  fi
  sleep 0.2
done

echo "[$node] creating Linux VRF BLUE (table 1001) before FRR startup"
ip link add BLUE type vrf table 1001
ip link set dev BLUE up
ip route add table 1001 unreachable default metric 4278198272
ip link set dev eth2 master BLUE
ip link set dev eth2 up

# An actual connected route exists for the frr2 'network' statement.
# frr1 receives 10.99.1.0/24 via eBGP inside BLUE; it does not originate it.
if [ "$node" = "frr2" ]; then
  ip link add bn003pfx type dummy
  ip link set dev bn003pfx master BLUE
  ip addr add 10.99.1.1/24 dev bn003pfx
  ip link set dev bn003pfx up
fi

ip -d link show BLUE
ip -d link show eth2
ip route show table 1001

echo "[$node] kernel VRF ready; starting unmodified FRR 10.6.2 entry command"
exec /usr/lib/frr/docker-start
