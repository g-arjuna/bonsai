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
#
# Prerequisites:
#   - docker with bridge networking
#   - python3 (identity DB generation only; no third-party modules)
#   - images: quay.io/frrouting/frr:10.6.1, frrouting/frr:v8.4.1,
#     and local image bn021c-routing:local built from this repo
#     (see image/Dockerfile; rebuild after code changes)
#
# Safety: this script NEVER silently destroys existing lab state.
#   - If any bn021c-* container/network or runtime journal/evidence
#     files already exist, it refuses to run unless BN021C_RESET=1.
#   - With BN021C_RESET=1, existing runtime artifacts (journals, event
#     log, identity DB, nat-rules) are first archived under
#     runtime/_archive/<name>.<utc-timestamp> so prior evidence is not
#     lost, then the lab is rebuilt from scratch (old journals are
#     never reused).
#
# Generated artifacts (all under the gitignored runtime/ dir):
#   runtime/bmp.journal, runtime/events.jsonl, runtime/bonsai_config.db,
#   runtime/nat-rules.txt, runtime/_archive/*
set -euo pipefail

LAB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$LAB_DIR/../../.." && pwd)"
RT="$ROOT/docs/merger/evidence/2026-11-10-bn021c-nat-acceptance/runtime"
FRR=quay.io/frrouting/frr:10.6.1
NATIMG=frrouting/frr:v8.4.1
PFX=bn021c
RESET="${BN021C_RESET:-0}"

existing_resources() {
  {
    docker ps -a --format '{{.Names}}' | grep "^${PFX}-" || true
    docker network ls --format '{{.Name}}' | grep "^${PFX}-" || true
  } | sort -u
}

existing_runtime_files() {
  local f
  for f in bmp.journal events.jsonl bonsai_config.db nat-rules.txt; do
    [ -e "$RT/$f" ] && echo "$f"
  done
}

cleanup() {
  for n in nat bonsai peer frr-a frr-b frr-c; do docker rm -f "$PFX-$n" >/dev/null 2>&1 || true; done
  for n in pub sitea siteb sitec; do docker network rm "$PFX-$n" >/dev/null 2>&1 || true; done
}

RESOURCES="$(existing_resources || true)"
RUNTIME_FILES="$(existing_runtime_files || true)"
if [ "$RESET" != "1" ] && { [ -n "$RESOURCES" ] || [ -n "$RUNTIME_FILES" ]; }; then
  {
    echo "bn021c-nat: refusing to run — lab resources or runtime evidence already exist."
    [ -n "$RESOURCES" ] && echo "  docker resources: $(echo "$RESOURCES" | tr '\n' ' ')"
    [ -n "$RUNTIME_FILES" ] && echo "  runtime files:    $(echo "$RUNTIME_FILES" | tr '\n' ' ')"
    echo "Re-run with BN021C_RESET=1 to archive runtime artifacts and rebuild the lab."
  } >&2
  exit 1
fi

if [ "$RESET" = "1" ]; then
  # Archive prior runtime evidence before teardown so journals and the
  # identity DB from earlier runs are never silently destroyed.
  if [ -d "$RT" ]; then
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    for f in "$RT"/*; do
      [ -f "$f" ] || continue
      mkdir -p "$RT/_archive"
      mv "$f" "$RT/_archive/$(basename "$f").$stamp"
      echo "archived $f -> runtime/_archive/$(basename "$f").$stamp"
    done
  fi
  cleanup
fi

mkdir -p "$RT"

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
python3 "$LAB_DIR/make_identity_db.py" "$RT/bonsai_config.db"
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
  -v "$LAB_DIR/configs/peer:/etc/frr" \
  "$FRR"

# ── Exporters ─────────────────────────────────────────────────────────────
docker run -d --name $PFX-frr-a --hostname frr-a --network $PFX-sitea --ip 10.240.1.11 \
  --cap-add NET_ADMIN --cap-add SYS_ADMIN \
  -v "$LAB_DIR/configs/frr-a:/etc/frr" "$FRR"
docker run -d --name $PFX-frr-b --hostname frr-b --network $PFX-siteb --ip 10.240.2.12 \
  --cap-add NET_ADMIN --cap-add SYS_ADMIN \
  -v "$LAB_DIR/configs/frr-b:/etc/frr" "$FRR"
docker run -d --name $PFX-frr-c --hostname frr-c --network $PFX-sitec --ip 10.240.3.13 \
  --cap-add NET_ADMIN --cap-add SYS_ADMIN \
  -v "$LAB_DIR/configs/frr-c:/etc/frr" "$FRR"

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
