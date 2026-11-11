#!/usr/bin/env bash
# BN-021d scenario driver — runs the exporter scripts inside the lab
# containers in the order that exercises NAT identity isolation,
# template-id collision, protocol collision, port change, v9 restart,
# unbound claims, malformed input, and collector restart.
set -euo pipefail

LAB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RT="$(cd "$LAB_DIR/../../.." && pwd)/docs/merger/evidence/2026-11-11-bn021d-flow-exporter-isolation/runtime"
# Exporters dial the NAT pub address; up.sh installs a DNAT rule that
# forwards udp/2055 to the bonsai container while SNAT collapses every
# exporter to the single translated source 10.241.0.200.
TARGET=10.241.0.10:2055

log() { echo "[$(date -u +%H:%M:%S)] $*" | tee -a "$RT/scenarios.log"; }

mkdir -p "$RT"
: > "$RT/scenarios.log"

run() { # container scenario [extra args...]
  local c="$1" sc="$2"; shift 2
  log "exec $c scenario=$sc $*"
  docker exec "$c" python3 /flow_exporter.py --target "$TARGET" \
    --scenario "$sc" "$@" 2>&1 | tee -a "$RT/scenarios.log"
}

log "=== phase 1: concurrent exporters behind shared NAT ==="
run bn021d-exp-a nat-a --sport 49152 --src-ip 192.0.2.1 &
PA=$!
run bn021d-exp-b collide --sport 49152 --src-ip 203.0.113.1 &
PB=$!
run bn021d-exp-c unbound --sport 49152 --src-ip 198.51.100.9 &
PC=$!
wait $PA $PB $PC

log "=== phase 2: settle, then malformed flood from exp-b ==="
docker exec -i bn021d-exp-b python3 - <<'PY' 2>&1 | tee -a "$RT/scenarios.log"
import socket, time
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(("0.0.0.0", 49153))
t = ("10.241.0.10", 2055)
# truncated header, bad version, insane lengths, random bytes
for i, pkt in enumerate([
    b"\x00\x0a",                          # 2 bytes
    b"\x00\x0b\x00\x01" + b"\x00" * 40,   # version 11
    b"\x00\x09" + b"\xff" * 60,           # v9 garbage
    b"\x00\x0a\xff\xff" + b"\x00" * 100,  # ipfix len=65535
    bytes(range(256)),
]):
    s.sendto(pkt, t)
    time.sleep(0.05)
print("malformed packets sent")
PY

log "=== phase 3: collector restart — stale templates must not decode ==="
docker restart bn021d-bonsai >/dev/null
# Wait for the HTTP API (graph store open) before re-arming templates.
for i in $(seq 1 60); do
  curl -sf http://127.0.0.1:18200/readyz >/dev/null 2>&1 && break
  sleep 1
done
log "bonsai readiness: $(curl -s http://127.0.0.1:18200/readyz || echo unreachable)"
# data-only on the post-restart collector: template state was in-memory,
# so this MUST be dropped (unknown template).
run bn021d-exp-a data-only --sport 49152
# then re-arm template + send again via 'template' then rely on nat-a
# already having sent data; send a fresh template+data by re-running nat-a
# would duplicate ports — instead send template then a data-only is not
# possible (data-only uses tid 400 which template re-arms).
run bn021d-exp-a template --sport 49152
run bn021d-exp-a data-only --sport 49152

log "=== done ==="
