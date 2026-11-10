#!/usr/bin/env bash
# BN-021c real-TCP NAT acceptance — bring-up.
#
# Topology (all docker bridge nets, dedicated bn021c-* names only):
#
#   frr-a (10.240.1.11, AS65001, sitea) --+
#   frr-b (10.240.2.12, AS65002, siteb) --+--> bn021c-nat (iptables SNAT)
#   frr-c (10.240.3.13, AS65003, sitec) --+        |
#                                           +--> bn021c-bonsai (10.240.0.20)
#   peer-rs (10.240.0.50, AS65000, pub) <--+
#
# NAT translations (on bn021c-nat, pub iface):
#   tcp dport 11019 (BMP)  -> 10.240.0.200  (ALL exporters share this source)
#   tcp dport 179  (BGP)   -> 10.240.0.20X  (per-site, so peer sees distinct neighbors)
set -euo pipefail
cd "$(dirname "$0")"
LAB=/home/arjuna/Desktop/bonsai/lab/acceptance/bn021c-nat
RT=/home/arjuna/Desktop/bonsai/docs/merger/evidence/2026-11-10-bn021c-nat-acceptance/runtime
FRR=quay.io/frrouting/frr:10.6.1
NATIMG=frrouting/frr:v8.4.1
PFX=bn021c

cleanup() {
  for n in nat bonsai peer frr-a frr-b frr-c; do docker rm -f "$PFX-$n" >/dev/null 2>&1 || true; done
  for n in pub sitea siteb sitec; do docker network rm "$PFX-$n" >/dev/null 2>&1 || true; done
}
cleanup

docker network create --subnet 10.240.0.0/24 $PFX-pub
docker network create --subnet 10.240.1.0/24 $PFX-sitea
docker network create --subnet 10.240.2.0/24 $PFX-siteb
docker network create --subnet 10.240.3.0/24 $PFX-sitec

# ── NAT node ──────────────────────────────────────────────────────────────
docker run -d --name $PFX-nat --cap-add NET_ADMIN --cap-add NET_RAW \
  --sysctl net.ipv4.ip_forward=1 \
  --network $PFX-pub --ip 10.240.0.10 \
  --entrypoint sleep "$NATIMG" infinity
for spec in "sitea 10.240.1.2" "siteb 10.240.2.2" "sitec 10.240.3.2"; do
  set -- $spec
  docker network connect --ip "$2" $PFX-$1 $PFX-nat
done

PUBIF=$(docker exec $PFX-nat sh -c "ip -o -4 addr show | awk '/10\.240\.0\.10/ {print \$2}'" | cut -d@ -f1)
echo "nat pub iface: $PUBIF"
# SNAT addresses must exist on the pub iface so return traffic ARPs here
# and conntrack can un-translate it back to the private exporters.
docker exec $PFX-nat ip addr add 10.240.0.200/32 dev "$PUBIF"
docker exec $PFX-nat ip addr add 10.240.0.201/32 dev "$PUBIF"
docker exec $PFX-nat ip addr add 10.240.0.202/32 dev "$PUBIF"
docker exec $PFX-nat ip addr add 10.240.0.203/32 dev "$PUBIF"

NATSETUP=$(cat <<EOF
set -e
PUBIF=$PUBIF
iptables -P FORWARD ACCEPT
iptables -t nat -F POSTROUTING
# BMP (dport 11019): every exporter shares ONE translated source.
for s in 10.240.1.0/24 10.240.2.0/24 10.240.3.0/24; do
  iptables -t nat -A POSTROUTING -s \$s -o \$PUBIF -p tcp --dport 11019 -j SNAT --to-source 10.240.0.200
done
# BGP (dport 179): per-site translated source so the route-server sees
# three distinct directly-connected eBGP neighbors.
iptables -t nat -A POSTROUTING -s 10.240.1.0/24 -o \$PUBIF -p tcp --dport 179 -j SNAT --to-source 10.240.0.201
iptables -t nat -A POSTROUTING -s 10.240.2.0/24 -o \$PUBIF -p tcp --dport 179 -j SNAT --to-source 10.240.0.202
iptables -t nat -A POSTROUTING -s 10.240.3.0/24 -o \$PUBIF -p tcp --dport 179 -j SNAT --to-source 10.240.0.203
echo '--- nat POSTROUTING ---'
iptables -t nat -L POSTROUTING -n -v --line-numbers
EOF
)
docker exec $PFX-nat sh -c "$NATSETUP" | tee "$RT/nat-rules.txt"

# ── Bonsai routing service ────────────────────────────────────────────────
python3 "$LAB/make_identity_db.py" "$RT/bonsai_config.db"
docker run -d --name $PFX-bonsai --network $PFX-pub --ip 10.240.0.20 \
  -v "$RT:/app/runtime" \
  -e BONSAI_ROUTING_BMP_ADDR=0.0.0.0:11019 \
  -e BONSAI_ROUTING_HTTP_ADDR=0.0.0.0:8081 \
  -e BONSAI_ROUTING_BMP_JOURNAL=/app/runtime/bmp.journal \
  -e BONSAI_ROUTING_EVENT_JOURNAL=/app/runtime/events.jsonl \
  -e BONSAI_ROUTING_IDENTITY_DB=/app/runtime/bonsai_config.db \
  -e BONSAI_ROUTING_SERVICE_TOKEN=bn021c-lab-token \
  -e RUST_LOG=info,bonsai=debug \
  -p 127.0.0.1:18099:8081 \
  --entrypoint /usr/local/bin/bonsai-routing bn021c-routing:local

# ── BGP route-server peer ─────────────────────────────────────────────────
docker run -d --name $PFX-peer --network $PFX-pub --ip 10.240.0.50 \
  --cap-add NET_ADMIN --cap-add SYS_ADMIN \
  -v "$LAB/configs/peer:/etc/frr" \
  "$FRR"

# ── Exporters ─────────────────────────────────────────────────────────────
docker run -d --name $PFX-frr-a --hostname frr-a --network $PFX-sitea --ip 10.240.1.11 \
  --cap-add NET_ADMIN --cap-add SYS_ADMIN \
  -v "$LAB/configs/frr-a:/etc/frr" "$FRR"
docker run -d --name $PFX-frr-b --hostname frr-b --network $PFX-siteb --ip 10.240.2.12 \
  --cap-add NET_ADMIN --cap-add SYS_ADMIN \
  -v "$LAB/configs/frr-b:/etc/frr" "$FRR"
docker run -d --name $PFX-frr-c --hostname frr-c --network $PFX-sitec --ip 10.240.3.13 \
  --cap-add NET_ADMIN --cap-add SYS_ADMIN \
  -v "$LAB/configs/frr-c:/etc/frr" "$FRR"

# Default routes for exporters (via their NAT gateway).
docker exec $PFX-frr-a ip route replace default via 10.240.1.2
docker exec $PFX-frr-b ip route replace default via 10.240.2.2
docker exec $PFX-frr-c ip route replace default via 10.240.3.2

echo "bring-up complete; waiting for BGP/BMP convergence..."
sleep 20
for c in peer frr-a frr-b frr-c; do
  echo "=== $c ==="
  docker exec $PFX-$c vtysh -c 'show bgp ipv4 unicast summary' 2>/dev/null | tail -8 || true
done
