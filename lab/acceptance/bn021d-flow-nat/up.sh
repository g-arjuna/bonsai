#!/usr/bin/env bash
# BN-021d real-UDP NAT acceptance — bring-up.
#
# Topology (single docker bridge net, dedicated bn021d-* names only):
#
#   exp-a (10.241.0.31) --+
#   exp-b (10.241.0.32) --+-- bn021d-nat (10.241.0.10, iptables)
#   exp-c (10.241.0.33) --+        |  DNAT udp/2055 -> 10.241.0.20
#                          SNAT -> 10.241.0.200 (single translated src)
#                                 +--> bn021d-bonsai (10.241.0.20)
#                                       UDP :2055  HTTP :8080
#
# ALL exporters' UDP/2055 traffic is SNAT'd to the single translated
# source 10.241.0.200. The collector therefore observes one NAT address
# with per-flow distinct source ports — exactly the ambiguity BN-021d
# addresses. Exporters deliberately reuse the same NetFlow source-id /
# IPFIX ODID and template-id numbers with DIFFERENT template definitions.
#
# Prerequisites:
#   - docker with bridge networking
#   - cargo build --bin bonsai   (debug binary copied into image/)
#   - python3 (only for verify.sh on the host)
#
# Safety: refuses to run if bn021d-* resources or runtime evidence
# already exist unless BN021D_RESET=1 (which archives runtime/ first).
set -euo pipefail

LAB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$LAB_DIR/../../.." && pwd)"
RT="$ROOT/docs/merger/evidence/2026-11-11-bn021d-flow-exporter-isolation/runtime"
FRR=quay.io/frrouting/frr:10.6.1   # has python3; already used by bn021c lab
NATIMG=frrouting/frr:v8.4.1
PFX=bn021d
RESET="${BN021D_RESET:-0}"
HTTP_PORT=18200
NET=$PFX-lan

existing_resources() {
  {
    docker ps -a --format '{{.Names}}' | grep "^${PFX}-" || true
    docker network ls --format '{{.Name}}' | grep "^${PFX}-" || true
  } | sort -u
}

if [ ! -f "$LAB_DIR/image/bonsai" ]; then
  echo "bn021d-flow-nat: missing image/bonsai — run 'cargo build --bin bonsai' then 'cp target/debug/bonsai lab/acceptance/bn021d-flow-nat/image/bonsai'" >&2
  exit 1
fi

RESOURCES="$(existing_resources || true)"
if [ "$RESET" != "1" ] && [ -n "$RESOURCES" ]; then
  echo "bn021d-flow-nat: refusing to run — resources exist: $RESOURCES" >&2
  echo "Re-run with BN021D_RESET=1 to archive runtime evidence and rebuild." >&2
  exit 1
fi

if [ "$RESET" = "1" ]; then
  mkdir -p "$RT/_archive"
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  for c in nat bonsai exp-a exp-b exp-c; do docker rm -f "$PFX-$c" >/dev/null 2>&1 || true; done
  docker network rm "$NET" >/dev/null 2>&1 || true
  for n in pub sitea siteb; do docker network rm "$PFX-$n" >/dev/null 2>&1 || true; done
  for f in "$RT"/*.json "$RT"/*.log "$RT"/nat-rules.txt "$RT"/bonsai.db "$RT"/bonsai.db.wal "$RT"/bonsai.db.shm "$RT"/bonsai_config.db* "$RT"/bonsai-registry.json; do
    [ -e "$f" ] && mv "$f" "$RT/_archive/$(basename "$f").$stamp" || true
  done
  rm -rf "$RT/credentials"
fi

mkdir -p "$RT" "$RT/credentials"

docker build -q -t bn021d-bonsai:local "$LAB_DIR/image" >/dev/null

docker network create --subnet 10.241.0.0/24 $NET >/dev/null

# ── NAT node (router-on-a-stick on the shared bridge) ───────────────────
docker run -d --name $PFX-nat --cap-add NET_ADMIN --cap-add NET_RAW \
  --sysctl net.ipv4.ip_forward=1 \
  --network $NET --ip 10.241.0.10 \
  --entrypoint sleep "$NATIMG" infinity >/dev/null
NATIF=$(docker exec $PFX-nat sh -c "ip -o -4 addr show | awk '/10\.241\.0\.10/ {print \$2}'" | cut -d@ -f1)
# Translated source address exporters appear as.
docker exec $PFX-nat ip addr add 10.241.0.200/32 dev "$NATIF"

NATSETUP=$(cat <<EOF
set -e
NATIF=$NATIF
iptables -P FORWARD ACCEPT
iptables -t nat -F POSTROUTING
iptables -t nat -F PREROUTING
# DNAT: exporters dial the NAT address; forward udp/2055 to bonsai.
iptables -t nat -A PREROUTING -i \$NATIF -p udp --dport 2055 -j DNAT --to-destination 10.241.0.20
# SNAT: every exporter collapses to the single translated source.
iptables -t nat -A POSTROUTING -o \$NATIF -s 10.241.0.0/24 -p udp --dport 2055 -j SNAT --to-source 10.241.0.200
iptables -t nat -L POSTROUTING -n -v --line-numbers
iptables -t nat -L PREROUTING -n -v --line-numbers
EOF
)
docker exec $PFX-nat sh -c "$NATSETUP" | tee "$RT/nat-rules.txt"

# ── Bonsai (mode "all": core GraphStore consumes NetflowRecord) ─────────
docker run -d --name $PFX-bonsai --network $NET --ip 10.241.0.20 \
  -v "$LAB_DIR:/app/config:ro" \
  -v "$RT:/app/runtime" \
  -e BONSAI_VAULT_PASSPHRASE=bn021d-lab-pass \
  -e BONSAI_ALLOW_INSECURE_NO_AUTH=1 \
  -e RUST_LOG=info,bonsai=debug \
  -p 127.0.0.1:${HTTP_PORT}:8080 \
  bn021d-bonsai:local >/dev/null

# ── Exporters (FRR image has python3) ───────────────────────────────────
for spec in "exp-a 10.241.0.31" "exp-b 10.241.0.32" "exp-c 10.241.0.33"; do
  set -- $spec
  docker run -d --name $PFX-$1 --network $NET --ip $2 \
    -v "$LAB_DIR/flow_exporter.py:/flow_exporter.py:ro" \
    --entrypoint sleep "$FRR" infinity >/dev/null
done

echo "bn021d-flow-nat: lab up. bonsai http on 127.0.0.1:${HTTP_PORT}"
echo "run scenarios:  bash $LAB_DIR/run_scenarios.sh"
echo "verify:         bash $LAB_DIR/verify.sh"
echo "teardown:       BN021D_RESET=1 bash $LAB_DIR/up.sh   (archives runtime/)"
