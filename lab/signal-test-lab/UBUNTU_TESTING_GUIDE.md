# Bonsai Signal-Test Lab — Ubuntu Testing Guide

**Topology**: `lab/signal-test-lab/signal-test.clab.yml`
**Bonsai config**: `docker/configs/signal-test.toml`
**Goal**: End-to-end validation of every receiver and signal pipeline introduced in DV3.

Each step is numbered. Mark ✅/❌ as you go. Do not skip steps — they have dependencies.

---

## Quick Reference

| Receiver | Protocol | Port | Source nodes |
|---|---|---|---|
| gNMI | gRPC/TLS | 57400 (on nodes) | all 7 SRL nodes |
| Syslog UDP | UDP | 5514 | srl-leaf1, srl-leaf2 |
| SNMP traps | UDP | 9162 | srl-leaf3, srl-leaf4 |
| BMP | TCP | 5000 | srl-spine1, srl-spine2 |
| NetFlow v5 | UDP | 2055 | linux-host1 (softflowd) |
| OTLP HTTP | HTTP | 4318 | linux-host1 (otelcol-contrib) |

| Container naming convention | `clab-bonsai-signal-test-<node>` |
|---|---|
| ContainerLab CA cert | `lab/signal-test-lab/clab-bonsai-signal-test/.tls/ca/ca.pem` |

---

## Phase 0 — Ubuntu Clean Slate

### S-00: Verify environment and clean up previous state

```bash
# Confirm on Ubuntu (not Mac)
uname -a                            # must show Linux
whoami                              # must NOT be root for most steps

# Wipe any previous bonsai signal-test state
rm -rf runtime/bonsai-signal-test.db  runtime/archive  runtime/signals  runtime/streaming
mkdir -p runtime/signals runtime/streaming logs

# Stop any running bonsai processes
pkill -f 'bonsai' || true
pkill -f 'collector_engine' || true
sleep 2

# Check ports are free
ss -tlnp | grep -E ':3000|:5000|:5514|:9162|:4318|:2055' \
  && echo "WARNING: ports in use" || echo "OK: all receiver ports free"
```

**Expected**: All ports free. If any are bound, identify and stop the owner.

---

## Phase 1 — Build

### S-01: Pull latest code

```bash
cd /opt/bonsai
git pull origin main
git log --oneline -3
```

**Expected**: Shows the `dv3(s13-14)` commit at HEAD.

---

### S-02: Build bonsai binary

```bash
cargo build --release 2>&1 | tail -20
echo "Exit: $?"
```

**Expected**: `Compiling bonsai` ... `Finished release`. Exit 0.
**If FAIL**: Record the error in `docs/test_results/`. Common issues:
- Missing `libssl-dev`: `sudo apt install libssl-dev pkg-config`
- Missing `protobuf-compiler`: `sudo apt install protobuf-compiler`
- Missing `clang`: `sudo apt install clang`

---

### S-03: Regenerate Python gRPC stubs

```bash
if [[ -x .venv/bin/python ]]; then PY=.venv/bin/python; else PY=python3; fi
$PY python/gen_protos.py
echo "Exit: $?"
```

**Expected**: Exit 0. Stubs regenerated in `python/generated/`.

---

### S-04: Run unit tests

```bash
cargo test --release 2>&1 | tail -30
echo "Exit: $?"
```

**Expected**: All tests pass. No `FAILED`. Exit 0.

---

## Phase 2 — ContainerLab Deployment

### S-05: Deploy the signal-test-lab topology

```bash
cd /opt/bonsai
sudo containerlab deploy \
  --topo lab/signal-test-lab/signal-test.clab.yml \
  --reconfigure 2>&1 | tail -30
```

**Expected**: All 8 nodes created (7 SRL + 1 linux). Output ends with `INFO ... finished provisioning`.

**Note**: `--reconfigure` forces fresh config even if containers already exist.

---

### S-06: Verify all containers are running

```bash
sudo containerlab inspect --topo lab/signal-test-lab/signal-test.clab.yml
```

**Expected**: 8 rows, all state `running`.

```bash
# Verify management IPs are reachable
for ip in 172.100.109.11 172.100.109.12 172.100.109.13 \
          172.100.109.14 172.100.109.15 172.100.109.16 \
          172.100.109.17 172.100.109.20; do
  ping -c1 -W2 $ip &>/dev/null && echo "OK  $ip" || echo "FAIL $ip"
done
```

**Expected**: All 8 show `OK`.

---

### S-07: Verify ContainerLab TLS CA cert exists

```bash
CA_CERT="lab/signal-test-lab/clab-bonsai-signal-test/.tls/ca/ca.pem"
ls -lh "$CA_CERT" && openssl x509 -in "$CA_CERT" -noout -subject
```

**Expected**: File exists. Subject shows the clab CA.

---

### S-08: Wait for IS-IS convergence

```bash
# Give IS-IS ~30s to converge on all nodes
sleep 30

# Verify IS-IS adjacency on spine1 (should show 3 adjacencies: super1, leaf1-3)
docker exec clab-bonsai-signal-test-srl-spine1 \
  sr_cli -d "show network-instance default protocols isis adjacency"
```

**Expected**: At least 4 adjacencies in `UP` state: super1 + leaf1 + leaf2 + leaf3.

---

### S-09: Verify BGP sessions established

```bash
# super1 should show 6 RR clients
docker exec clab-bonsai-signal-test-srl-super1 \
  sr_cli -d "show network-instance default protocols bgp neighbor"
```

**Expected**: 6 BGP sessions (spine1, spine2, leaf1-4), all `established`.

---

## Phase 3 — Bonsai Startup

### S-10: Initialise credential vault

```bash
export BONSAI_VAULT_PASSPHRASE="bonsai-signal-test-pass"
# Init vault (skip if already exists)
./target/release/bonsai --config docker/configs/signal-test.toml --init-vault || true
```

---

### S-11: Start bonsai with signal-test config

```bash
export BONSAI_VAULT_PASSPHRASE="bonsai-signal-test-pass"

nohup ./target/release/bonsai \
  --config docker/configs/signal-test.toml \
  > logs/bonsai-signal-test.log 2>&1 &

echo "bonsai PID: $!"
echo $! > runtime/bonsai-signal-test.pid
```

---

### S-12: Wait for bonsai to be healthy

```bash
echo "Waiting for bonsai /health..."
for i in $(seq 1 30); do
  CODE=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:3000/health 2>/dev/null || echo 000)
  echo "  [$i] HTTP $CODE"
  [[ "$CODE" == "200" || "$CODE" == "503" ]] && echo "UP" && break
  sleep 3
done
curl -s http://127.0.0.1:3000/health | python3 -m json.tool
```

**Expected**: `{"status": "ok", ...}` within 90 seconds.

---

### S-13: Verify all receiver ports are listening

```bash
for port in 3000 50051 5514 9162 5000 4318 2055; do
  ss -tlnp 2>/dev/null | grep -q ":$port\b" \
    && echo "LISTEN :$port" \
    || echo "MISSING :$port  ← check bonsai log"
done
```

**Expected**: All 7 ports showing `LISTEN`. Port 2055 uses UDP — check with:

```bash
ss -ulnp | grep ':2055'
```

---

### S-14: Confirm gNMI subscriptions connecting

```bash
sleep 20   # give subscriptions time to establish

curl -s http://127.0.0.1:3000/api/topology | python3 -c "
import json, sys
d = json.load(sys.stdin)
devices = d.get('devices', [])
print(f'Devices in graph: {len(devices)}')
for dev in devices:
    print(f\"  {dev['hostname']:20s}  BGP sessions: {len(dev.get('bgp',[]))}  interfaces: {len(dev.get('interfaces',[]))}\")
"
```

**Expected**: 7 devices. Each showing interface and BGP data. May take up to 90s for all to appear.

---

## Phase 4 — gNMI Receiver Tests

### S-15: gNMI T1 — Interface counter ingest

```bash
curl -s http://127.0.0.1:3000/api/topology | python3 -c "
import json, sys
d = json.load(sys.stdin)
for dev in d.get('devices', []):
    ifaces = dev.get('interfaces', [])
    if ifaces:
        print(f\"{dev['hostname']}: {len(ifaces)} interfaces, first: {ifaces[0]['name']} oper={ifaces[0].get('oper_state','?')}\")
"
```

**Expected**: All 7 devices have interfaces. Most `oper_state=up`.

---

### S-16: gNMI T2 — BGP state visible

```bash
curl -s http://127.0.0.1:3000/api/topology | python3 -c "
import json, sys
d = json.load(sys.stdin)
total_bgp = 0
for dev in d.get('devices', []):
    for n in dev.get('bgp', []):
        state = n.get('session_state', '?')
        print(f\"  {dev['hostname']:20s} → {n['peer']:16s}  {state}\")
        total_bgp += 1
print(f'Total BGP sessions visible: {total_bgp}')
"
```

**Expected**: ~12+ BGP sessions total (each leaf→super1, each spine→super1, etc). All `established`.

---

### S-17: gNMI T3 — IS-IS adjacency visible in graph

```bash
curl -s http://127.0.0.1:3000/api/topology | python3 -c "
import json, sys
d = json.load(sys.stdin)
for dev in d.get('devices', []):
    isis = dev.get('isis_adjacencies', [])
    if isis:
        print(f\"{dev['hostname']}: {len(isis)} IS-IS adjacencies\")
"
```

**Expected**: spine1 and spine2 each show adjacencies to super1 + leaves.

---

### S-18: gNMI T4 — Interface admin-down triggers StateChangeEvent

```bash
# Inject: shut leaf4 uplink
docker exec clab-bonsai-signal-test-srl-leaf4 \
  sr_cli -d "set / interface ethernet-1/1 admin-state disable"

sleep 15

# Check for StateChangeEvent in events stream
curl -s "http://127.0.0.1:3000/api/events/history?device=srl-leaf4&limit=10" \
  | python3 -c "
import json, sys
d = json.load(sys.stdin)
events = d if isinstance(d, list) else d.get('events', [])
for e in events:
    print(f\"  {e.get('event_type','?'):30s} {e.get('device_address','?')}\")
"

# Heal
docker exec clab-bonsai-signal-test-srl-leaf4 \
  sr_cli -d "set / interface ethernet-1/1 admin-state enable"
```

**Expected**: `interface_admin_down` or `interface_oper_down` event for srl-leaf4.

---

### S-19: gNMI T5 — BGP session down → detection fired

```bash
# Inject: disable BGP on leaf4
docker exec clab-bonsai-signal-test-srl-leaf4 \
  sr_cli -d "set / network-instance default protocols bgp admin-state disable"

sleep 30

# Check detections
curl -s http://127.0.0.1:3000/api/detections | python3 -c "
import json, sys
d = json.load(sys.stdin)
items = d if isinstance(d, list) else d.get('detections', [])
for it in list(items)[-10:]:
    print(f\"  {it.get('rule_id','?'):30s}  {it.get('device_address','?')}\")
"

# Heal
docker exec clab-bonsai-signal-test-srl-leaf4 \
  sr_cli -d "set / network-instance default protocols bgp admin-state enable"
```

**Expected**: `bgp_session_down` or `bgp_all_peers_down` detection for srl-leaf4.

---

### S-20: gNMI T6 — BFD session down (via 100% loss)

```bash
# Inject: 100% loss on leaf3 e1-1 (uplink to spine1)
sudo containerlab tools netem set bonsai-signal-test \
  srl-leaf3 e1-1 --loss 100

sleep 20

curl -s "http://127.0.0.1:3000/api/events/history?device=srl-leaf3&limit=15" \
  | python3 -c "
import json, sys
d = json.load(sys.stdin)
events = d if isinstance(d, list) else d.get('events', [])
for e in events:
    print(f\"  {e.get('event_type','?'):35s} {e.get('source_type','?')}\")
"

# Heal
sudo containerlab tools netem reset bonsai-signal-test srl-leaf3 e1-1
```

**Expected**: `bfd_session_down` event visible. source_type=`gnmi`.

---

## Phase 5 — Syslog Receiver Tests

### S-21: Syslog T1 — UDP syslog arriving at bonsai

```bash
# Verify syslog archive is being written
ls -lh runtime/signals/syslog.jsonl 2>/dev/null || echo "File not yet created"

# Wait up to 60s for first syslog messages
for i in $(seq 1 12); do
  [[ -s runtime/signals/syslog.jsonl ]] && echo "Syslog data arriving" && break
  echo "  [$i] waiting..."
  sleep 5
done

tail -5 runtime/signals/syslog.jsonl 2>/dev/null | python3 -m json.tool 2>/dev/null | head -40
```

**Expected**: JSONL lines with `source_ip`, `raw_message`, `parsed` fields.
**If empty**: Check `ss -ulnp | grep :5514`. Verify leaf1 syslog config is applied:
```bash
docker exec clab-bonsai-signal-test-srl-leaf1 \
  sr_cli -d "show system logging remote-server"
```

---

### S-22: Syslog T2 — Force syslog event by config commit on leaf1

```bash
# A config commit generates a syslog "candidate configuration committed" message
docker exec clab-bonsai-signal-test-srl-leaf1 \
  sr_cli -d "set / system information description 'syslog-test-trigger'; commit stay"

sleep 10

# Check syslog archive for the commit event
grep -i "commit\|config" runtime/signals/syslog.jsonl 2>/dev/null | tail -3 \
  | python3 -c "
import sys, json
for line in sys.stdin:
    try:
        d = json.loads(line)
        print(f\"  src={d.get('source_ip','?'):15s}  msg={d.get('raw_message','?')[:80]}\")
    except: print(line[:100])
"
```

**Expected**: Line with `source_ip=172.100.109.14` (leaf1 mgmt IP) and config-commit message.

---

### S-23: Syslog T3 — Syslog fact extracted and written to graph

```bash
# Trigger a BGP state change on leaf1 — should produce syslog fact bgp_neighbor
docker exec clab-bonsai-signal-test-srl-leaf1 \
  sr_cli -d "set / network-instance default protocols bgp neighbor 10.9.0.1 admin-state disable"
sleep 5
docker exec clab-bonsai-signal-test-srl-leaf1 \
  sr_cli -d "set / network-instance default protocols bgp neighbor 10.9.0.1 admin-state enable"

sleep 15

curl -s "http://127.0.0.1:3000/api/events/history?device=srl-leaf1&limit=20" \
  | python3 -c "
import json, sys
d = json.load(sys.stdin)
events = d if isinstance(d, list) else d.get('events', [])
syslog_events = [e for e in events if e.get('source_type') == 'syslog']
print(f'Syslog-sourced events: {len(syslog_events)}')
for e in syslog_events:
    print(f\"  {e.get('event_type','?'):35s}  detail={str(e.get('detail',''))[:60]}\")
"
```

**Expected**: At least one `bgp_neighbor_down` or `bgp_neighbor` event with `source_type=syslog`.

---

### S-24: (Removed — TCP syslog not applicable)

Nokia SRL does not support two remote-server entries to the same IP with different
transports. Syslog is validated via UDP only (leaf1 + leaf2 → bonsai:5514).

---

### S-25: Syslog T5 — Multi-source correlation check (gNMI + syslog same event)

```bash
# Watch the correlation counter after triggering a BGP event on leaf2 (syslog-enabled)
# Prometheus metric: bonsai_correlation_multi_source_total
curl -s http://127.0.0.1:9100/metrics 2>/dev/null \
  | grep "bonsai_correlation_multi_source_total" \
  || echo "Bonsai metrics not on :9100 — check metrics_addr in config"

# Alternative: check bonsai log for "multi-source fusion"
grep -i "multi.source\|fusion\|absorbed" logs/bonsai-signal-test.log 2>/dev/null | tail -10
```

**Expected**: Counter > 0 after triggering BGP events on syslog-enabled nodes.

---

## Phase 6 — SNMP Trap Receiver Tests

### S-26: SNMP T1 — Verify SNMP trap receiver is accepting traps

```bash
# Send a manual test trap to bonsai using snmptrap tool on Ubuntu
# Install if not present: sudo apt install snmp
snmptrap -v 2c -c bonsai-test 127.0.0.1:9162 '' \
  1.3.6.1.6.3.1.1.5.3 \
  1.3.6.1.2.1.2.2.1.1.1 i 1 \
  1.3.6.1.2.1.2.2.1.2.1 s "ethernet-1/1" \
  1.3.6.1.2.1.2.2.1.7.1 i 2 \
  1.3.6.1.2.1.2.2.1.8.1 i 2 2>/dev/null

sleep 3

ls -lh runtime/signals/snmp.jsonl 2>/dev/null && \
  tail -2 runtime/signals/snmp.jsonl | python3 -m json.tool 2>/dev/null | head -20
```

**Expected**: `snmp.jsonl` receives the trap with `trap_oid=1.3.6.1.6.3.1.1.5.3` (linkDown).

---

### S-27: SNMP T2 — SNMP trap from leaf3 (injected via interface shutdown)

```bash
# Shut leaf3 e1-2 (uplink to spine2) — should emit linkDown trap
docker exec clab-bonsai-signal-test-srl-leaf3 \
  sr_cli -d "set / interface ethernet-1/2 admin-state disable"

sleep 10

# Check SNMP archive for trap from leaf3
grep "172.100.109.16" runtime/signals/snmp.jsonl 2>/dev/null | tail -3 \
  | python3 -c "
import sys, json
for line in sys.stdin:
    try:
        d = json.loads(line)
        print(f\"  src={d.get('source_ip','?'):16s}  oid={d.get('trap_oid','?')}  fact={d.get('fact_type','raw')}\")
    except: print(line[:120])
"

# Heal
docker exec clab-bonsai-signal-test-srl-leaf3 \
  sr_cli -d "set / interface ethernet-1/2 admin-state enable"
```

**Expected**: Record with `source_ip=172.100.109.16`, `trap_oid=1.3.6.1.6.3.1.1.5.3` (linkDown).
`fact_type=link_down` confirms OID pattern extraction worked.

---

### S-28: SNMP T3 — SNMP-sourced StateChangeEvent in graph

```bash
curl -s "http://127.0.0.1:3000/api/events/history?device=srl-leaf3&limit=20" \
  | python3 -c "
import json, sys
d = json.load(sys.stdin)
events = d if isinstance(d, list) else d.get('events', [])
snmp_events = [e for e in events if e.get('source_type') == 'snmp']
print(f'SNMP-sourced events for srl-leaf3: {len(snmp_events)}')
for e in snmp_events:
    print(f\"  {e.get('event_type','?'):35s}  detail={str(e.get('detail',''))[:60]}\")
"
```

**Expected**: At least one `link_down` or `interface_oper_down` event with `source_type=snmp`.

---

### S-29: SNMP T4 — BGP trap from leaf4

```bash
# Shut BGP on leaf4 to generate bgpBackwardTransition trap
docker exec clab-bonsai-signal-test-srl-leaf4 \
  sr_cli -d "set / network-instance default protocols bgp neighbor 10.9.0.1 admin-state disable"

sleep 10

grep "172.100.109.17" runtime/signals/snmp.jsonl 2>/dev/null | tail -3 \
  | python3 -c "
import sys, json
for line in sys.stdin:
    try:
        d = json.loads(line)
        print(f\"  fact_type={d.get('fact_type','?'):25s}  oid={d.get('trap_oid','?')}\")
    except: print(line[:100])
"

# Heal
docker exec clab-bonsai-signal-test-srl-leaf4 \
  sr_cli -d "set / network-instance default protocols bgp neighbor 10.9.0.1 admin-state enable"
```

**Expected**: `fact_type=bgp_peer_backward_transition` trap from leaf4 mgmt IP.

---

## Phase 7 — BMP Receiver Tests

### S-30: BMP T1 — BMP sessions established from spine1 and spine2

```bash
# Check bonsai BMP archive for BMP initiation messages
ls -lh runtime/streaming/bmp.jsonl 2>/dev/null || echo "BMP archive not yet created"

sleep 30   # BMP sessions need time to establish after BGP convergence

tail -5 runtime/streaming/bmp.jsonl 2>/dev/null | python3 -c "
import sys, json
for line in sys.stdin:
    try:
        d = json.loads(line)
        print(f\"  type={d.get('msg_type','?'):20s}  peer={d.get('peer_address','?')}\")
    except: print(line[:120])
" 2>/dev/null
```

**Expected**: BMP messages from spine1 and spine2 (`source_ip=172.100.109.12` and `.13`).

---

### S-31: BMP T2 — Verify BMP session shows in bonsai log

```bash
grep -i "bmp\|bgp.monitoring" logs/bonsai-signal-test.log 2>/dev/null | tail -10
```

**Expected**: Log lines showing BMP session accepted for spine1 and spine2.

---

### S-32: BMP T3 — BMP route advertisement visible (ROUTE_MONITORING)

```bash
# After BGP converges, BMP should send ROUTE_MONITORING for each RIB entry
grep "route_monitoring\|ROUTE_MONITORING\|peer_up" runtime/streaming/bmp.jsonl 2>/dev/null \
  | wc -l
```

**Expected**: Count > 0. Each established BGP session generates at minimum a PEER_UP + ROUTE_MONITORING message.

---

### S-33: BMP T4 — Multi-source fusion: BMP + gNMI same BGP event

```bash
# Cause BGP flap on spine1 → should be seen by BOTH gNMI and BMP paths
docker exec clab-bonsai-signal-test-srl-spine1 \
  sr_cli -d "set / network-instance default protocols bgp neighbor 10.9.0.1 admin-state disable"
sleep 5
docker exec clab-bonsai-signal-test-srl-spine1 \
  sr_cli -d "set / network-instance default protocols bgp neighbor 10.9.0.1 admin-state enable"

sleep 20

# Check CorrelationBuffer fusion counter
curl -s http://127.0.0.1:9100/metrics 2>/dev/null \
  | grep "bonsai_correlation_multi_source_total"

grep -i "Absorbed\|multi.source" logs/bonsai-signal-test.log 2>/dev/null | tail -5
```

**Expected**: `bonsai_correlation_multi_source_total` counter incremented OR log lines showing `Absorbed` events.

---

## Phase 8 — NetFlow Receiver Tests

### S-34: NetFlow T1 — Install softflowd on linux-host1

```bash
# Install softflowd inside the linux-host1 container
docker exec clab-bonsai-signal-test-linux-host1 \
  bash -c "apt-get update -qq && apt-get install -y -qq softflowd iproute2 iputils-ping"
```

**Expected**: Exit 0. softflowd installed.

---

### S-35: NetFlow T2 — Configure host1 interfaces and routing

```bash
# Configure eth1 (leaf1-facing) and eth2 (leaf2-facing) on host1
docker exec clab-bonsai-signal-test-linux-host1 bash -c "
  ip addr add 10.9.20.1/31 dev eth1 2>/dev/null || true
  ip addr add 10.9.20.3/31 dev eth2 2>/dev/null || true
  ip link set eth1 up
  ip link set eth2 up
  ip route add default via 10.9.20.0 dev eth1 2>/dev/null || true
  ip addr show eth1
  ip addr show eth2
"
```

**Expected**: eth1 shows `10.9.20.1/31`, eth2 shows `10.9.20.3/31`.

---

### S-36: NetFlow T3 — Generate traffic and start softflowd

```bash
# Generate traffic between host1 and leaf1 loopback (via eth1)
docker exec -d clab-bonsai-signal-test-linux-host1 \
  bash -c "for i in \$(seq 1 60); do ping -c1 10.9.20.0 &>/dev/null; sleep 1; done"

# Start softflowd: capture on eth1, export NetFlow v5 to bonsai host at port 2055
# bonsai is reachable from host1 via mgmt network 172.100.109.0/24
# The Docker host gateway on bonsai-mgmt is 172.100.109.1
docker exec -d clab-bonsai-signal-test-linux-host1 \
  softflowd -i eth1 -n 172.100.109.1:2055 -v 5 -t maxlife=30

# Also export on eth2 (v9 for variety)
docker exec -d clab-bonsai-signal-test-linux-host1 \
  softflowd -i eth2 -n 172.100.109.1:2055 -v 9 -t maxlife=30

echo "softflowd started"
sleep 40   # wait for first flow export (softflowd exports after flow expiry)
```

---

### S-37: NetFlow T4 — Verify NetFlow records arriving at bonsai

```bash
# Check if netflow data is in the graph
curl -s http://127.0.0.1:3000/api/topology | python3 -c "
import json, sys
d = json.load(sys.stdin)
# AppFlow nodes should be present if NetFlow is working
app_flows = d.get('app_flows', [])
print(f'AppFlow nodes in graph: {len(app_flows)}')
for f in app_flows[:5]:
    print(f\"  {f.get('src_address','?'):15s} → {f.get('dst_address','?'):15s}  bytes={f.get('bytes',0)}\")
"

# Check bonsai log for NetFlow ingest
grep -i "netflow\|flow\|AppFlow" logs/bonsai-signal-test.log 2>/dev/null | tail -10
```

**Expected**: AppFlow nodes visible in topology OR log shows NetFlow records being processed.

---

### S-38: NetFlow T5 — CARRIES_FLOW edge: Device → AppFlow

```bash
curl -s http://127.0.0.1:3000/api/topology | python3 -c "
import json, sys
d = json.load(sys.stdin)
for dev in d.get('devices', []):
    flows = dev.get('app_flows', dev.get('carries_flows', []))
    if flows:
        print(f\"{dev['hostname']}: CARRIES_FLOW to {len(flows)} AppFlow node(s)\")
"
```

**Expected**: srl-leaf1 or srl-leaf2 showing CARRIES_FLOW edges (since host1 traffic transits them).

---

## Phase 9 — OTLP Receiver Tests

### S-39: OTLP T1 — Install otelcol-contrib on linux-host1

```bash
# Download otelcol-contrib (OpenTelemetry Collector)
docker exec clab-bonsai-signal-test-linux-host1 bash -c "
  curl -sL https://github.com/open-telemetry/opentelemetry-collector-releases/releases/download/v0.96.0/otelcol-contrib_0.96.0_linux_amd64.tar.gz \
    -o /tmp/otelcol.tar.gz
  tar -xzf /tmp/otelcol.tar.gz -C /usr/local/bin/ otelcol-contrib
  otelcol-contrib --version
"
```

**Expected**: otelcol-contrib version printed. If download fails (no internet in container), use alternative in S-40b.

---

### S-40: OTLP T2 — Create otelcol config and start it

```bash
# Write otelcol config inside the container
docker exec clab-bonsai-signal-test-linux-host1 bash -c "
cat > /tmp/otelcol-config.yaml << 'EOF'
receivers:
  otlp:
    protocols:
      http:
        endpoint: 0.0.0.0:4317

exporters:
  otlphttp:
    endpoint: http://172.100.109.1:4318
    tls:
      insecure: true

service:
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [otlphttp]
EOF
echo 'Config written'
"

# Start otelcol in background
docker exec -d clab-bonsai-signal-test-linux-host1 \
  otelcol-contrib --config /tmp/otelcol-config.yaml

sleep 5
echo "otelcol-contrib started"
```

**NOTE — Alternative if otelcol download fails (S-40b)**:
Send a raw OTLP HTTP POST directly from the Ubuntu host to verify the receiver:

```bash
# S-40b: Direct curl OTLP trace (no otelcol needed)
curl -s -X POST http://127.0.0.1:4318/v1/traces \
  -H "Content-Type: application/json" \
  -d '{
    "resourceSpans": [{
      "resource": {
        "attributes": [{"key": "service.name", "value": {"stringValue": "bonsai-test-app"}}]
      },
      "scopeSpans": [{
        "spans": [{
          "traceId": "5b8efff798038103d269b6337e0a0000",
          "spanId": "eee19b7ec3c1b173",
          "name": "test-span",
          "startTimeUnixNano": "1708000000000000000",
          "endTimeUnixNano":   "1708000001000000000",
          "kind": 1,
          "attributes": [
            {"key": "peer.address", "value": {"stringValue": "172.100.109.14"}}
          ]
        }]
      }]
    }]
  }'
echo "OTLP HTTP response: $?"
```

---

### S-41: OTLP T3 — Verify Application node in graph

```bash
sleep 15

curl -s http://127.0.0.1:3000/api/topology | python3 -c "
import json, sys
d = json.load(sys.stdin)
apps = d.get('applications', [])
print(f'Application nodes in graph: {len(apps)}')
for app in apps:
    print(f\"  service={app.get('service_name','?'):25s}  peer={app.get('peer_address','?')}\")
"

grep -i "otlp\|Application\|RUNS_SERVICE" logs/bonsai-signal-test.log 2>/dev/null | tail -10
```

**Expected**: At least one Application node with `service_name=bonsai-test-app`.

---

### S-42: OTLP T4 — RUNS_SERVICE edge: Device → Application

```bash
curl -s http://127.0.0.1:3000/api/topology | python3 -c "
import json, sys
d = json.load(sys.stdin)
for dev in d.get('devices', []):
    apps = dev.get('applications', dev.get('runs_services', []))
    if apps:
        print(f\"{dev['hostname']}: RUNS_SERVICE → {[a.get('service_name','?') for a in apps]}\")
"
```

**Expected**: srl-leaf1 (IP 172.100.109.14 = peer.address in span) shows RUNS_SERVICE edge.

---

## Phase 10 — Multi-Source Correlation Tests

### S-43: Correlation T1 — Same fault seen by gNMI + syslog

Leaf2 is dual-homed AND has syslog enabled. Shutting its e1-1 uplink (to spine1) will:
- Generate gNMI `interface_oper_down` event
- Generate syslog `interface_state` fact (via nokia-srlinux.yaml pattern)
Both should land at `CorrelationBuffer` for the same `(srl-leaf2, interface_down, ethernet-1/1)` key.

```bash
# Record baseline counters
BEFORE=$(curl -s http://127.0.0.1:9100/metrics 2>/dev/null \
  | grep "bonsai_correlation_multi_source_total" | awk '{print $2}')
echo "Correlation counter before: ${BEFORE:-0}"

# Inject: disable e1-1 on leaf2
docker exec clab-bonsai-signal-test-srl-leaf2 \
  sr_cli -d "set / interface ethernet-1/1 admin-state disable"

sleep 30

# Check counters
AFTER=$(curl -s http://127.0.0.1:9100/metrics 2>/dev/null \
  | grep "bonsai_correlation_multi_source_total" | awk '{print $2}')
echo "Correlation counter after: ${AFTER:-0}"

# Check events — expect events from BOTH gnmi and syslog sources
curl -s "http://127.0.0.1:3000/api/events/history?device=srl-leaf2&limit=20" \
  | python3 -c "
import json, sys
d = json.load(sys.stdin)
events = d if isinstance(d, list) else d.get('events', [])
from collections import Counter
srcs = Counter(e.get('source_type','?') for e in events)
print('Event source breakdown for srl-leaf2:', dict(srcs))
"

# Heal
docker exec clab-bonsai-signal-test-srl-leaf2 \
  sr_cli -d "set / interface ethernet-1/1 admin-state enable"
```

**Expected**: Counter increased (multi-source fusion). Events include both `gnmi` and `syslog` source_types.

---

### S-44: Correlation T2 — Detection deduplication (same event, 3 sources)

Disable leaf3 BGP neighbor → generates: gNMI state-change + syslog BGP fact + SNMP bgpBackwardTransition trap.

```bash
docker exec clab-bonsai-signal-test-srl-leaf3 \
  sr_cli -d "set / network-instance default protocols bgp neighbor 10.9.0.1 admin-state disable"

sleep 30

# Count distinct detections for leaf3 — should be 1, not 3
curl -s http://127.0.0.1:3000/api/detections | python3 -c "
import json, sys
from collections import Counter
d = json.load(sys.stdin)
items = d if isinstance(d, list) else d.get('detections', [])
leaf3 = [it for it in items if 'leaf3' in str(it.get('device_address',''))]
print(f'Detections for srl-leaf3: {len(leaf3)}')
rules = Counter(it.get('rule_id') for it in leaf3)
print('Rules:', dict(rules))
"

# Heal
docker exec clab-bonsai-signal-test-srl-leaf3 \
  sr_cli -d "set / network-instance default protocols bgp neighbor 10.9.0.1 admin-state enable"
```

**Expected**: 1-2 detections for srl-leaf3 (not 3). CorrelationBuffer absorbed the duplicates.

---

## Phase 11 — HostEndpoint Tests

### S-45: HostEndpoint T1 — LLDP-discovered host endpoint

SRL nodes broadcast LLDP. linux-host1 (as a Linux container) should appear as an LLDP neighbor on leaf1 and leaf2. When `chassis_id` doesn't match any Device, `write_lldp_neighbor` should upsert a `HostEndpoint`.

```bash
# Verify LLDP on leaf1 (should see linux-host1 on e1-2)
docker exec clab-bonsai-signal-test-srl-leaf1 \
  sr_cli -d "show system lldp neighbor"

# Check topology for HostEndpoint nodes
curl -s http://127.0.0.1:3000/api/topology | python3 -c "
import json, sys
d = json.load(sys.stdin)
hosts = d.get('host_endpoints', [])
print(f'HostEndpoint nodes: {len(hosts)}')
for h in hosts:
    print(f\"  ip={h.get('ip','?'):15s}  kind={h.get('kind','?'):10s}  hostname={h.get('hostname','?')}\")
"
```

**Expected**: At least one HostEndpoint with `kind=unknown` representing linux-host1.

---

### S-46: HostEndpoint T2 — CONNECTED_TO edge visible

```bash
curl -s http://127.0.0.1:3000/api/topology | python3 -c "
import json, sys
d = json.load(sys.stdin)
for dev in d.get('devices', []):
    hosts = dev.get('connected_hosts', dev.get('host_endpoints', []))
    if hosts:
        print(f\"{dev['hostname']}: CONNECTED_TO {len(hosts)} HostEndpoint(s)\")
"
```

**Expected**: srl-leaf1 and/or srl-leaf2 show CONNECTED_TO edge to the host.

---

## Phase 12 — Settings API Tests

### S-47: Settings T1 — GET /api/settings/streaming

```bash
curl -s http://127.0.0.1:3000/api/settings/streaming | python3 -m json.tool | head -40
```

**Expected**: JSON showing all 7 receiver configs with their current enabled/addr state.

---

### S-48: Settings T2 — PATCH: disable and re-enable syslog receiver

```bash
# Disable syslog
curl -s -X PATCH http://127.0.0.1:3000/api/settings/streaming \
  -H "Content-Type: application/json" \
  -d '{"syslog": {"enabled": false}}' | python3 -m json.tool

sleep 5

# Verify port is no longer listening
ss -ulnp | grep ':5514' && echo "STILL LISTENING" || echo "OK: port released"

# Re-enable
curl -s -X PATCH http://127.0.0.1:3000/api/settings/streaming \
  -H "Content-Type: application/json" \
  -d '{"syslog": {"enabled": true, "udp_addr": "0.0.0.0:5514"}}' | python3 -m json.tool

sleep 5

ss -ulnp | grep ':5514' && echo "OK: syslog receiver back" || echo "FAIL: port still released"
```

**Expected**: Port released on disable. Port re-bound on enable. `requires_restart: false` in response.

---

## Phase 13 — Collector Health Tests

### S-49: Collector T1 — Verify heartbeat fields populated

```bash
curl -s http://127.0.0.1:3000/api/collectors | python3 -c "
import json, sys
d = json.load(sys.stdin)
collectors = d if isinstance(d, list) else d.get('collectors', [])
for c in collectors:
    print(f\"  id={c.get('collector_id','?'):20s}\")
    print(f\"    uptime_secs:       {c.get('uptime_secs',0)}\")
    print(f\"    queue_depth:       {c.get('queue_depth_updates',0)}\")
    print(f\"    memory_used_mb:    {c.get('memory_used_mb',0):.1f}\")
    print(f\"    active_subs:       {c.get('active_subscribers',0)}\")
"
```

**Expected**: `uptime_secs > 0`, `queue_depth_updates ≥ 0`, `memory_used_mb > 0`.

---

### S-50: Collector T2 — Streaming receiver badges on collector card

```bash
curl -s http://127.0.0.1:3000/api/collectors | python3 -c "
import json, sys
d = json.load(sys.stdin)
collectors = d if isinstance(d, list) else d.get('collectors', [])
for c in collectors:
    badges = c.get('streaming_status', {})
    print(f\"  Collector '{c.get('collector_id','?')}' streaming badges:\")
    for name, status in badges.items():
        print(f\"    {name:12s}: state={status.get('state','?')} packets={status.get('packet_count',0)}\")
"
```

**Expected**: `syslog`, `snmp`, `bmp`, `netflow`, `otlp` receivers all showing state=`listening`.

---

## Phase 14 — Live UI Smoke Test

### S-51: Open bonsai UI and verify 3-panel layout

On any machine that can reach the Ubuntu IP, open:
```
http://<ubuntu-ip>:3000/
```

Navigate to **Live** in the sidebar.

**Manual checks**:
- [ ] SiteRail (left panel) shows `signal-test-lab` site with device count
- [ ] Topology canvas (centre) shows all 7 nodes with tier layout (super1 top, spines middle, leaves bottom)
- [ ] BGP table visible (shows established sessions)
- [ ] Events feed (right panel) shows live events from the lab
- [ ] LiveStatusBar (top) shows correct device count, SSE dot is green/pulsing
- [ ] Clicking a site in SiteRail isolates the topology canvas to that site

---

### S-52: Verify SSE event stream

```bash
# Check SSE endpoint is streaming events
timeout 15 curl -s -N http://127.0.0.1:3000/api/events/stream 2>&1 | head -20
```

**Expected**: `data: {...}` JSON lines arriving. `event: message` headers.

---

## Phase 15 — Fault Injection Round-Trip

### S-53: Full round-trip — leaf4 total isolation → detections → incidents

This is the end-to-end gate test. leaf4 is single-homed to spine2.

```bash
# Record baseline
DET_BEFORE=$(curl -s http://127.0.0.1:3000/api/detections \
  | python3 -c "import json,sys; d=json.load(sys.stdin); items=d if isinstance(d,list) else d.get('detections',[]); print(len(items))")
echo "Detections before: $DET_BEFORE"

# Inject: disable leaf4 uplink (total isolation — single-homed)
docker exec clab-bonsai-signal-test-srl-leaf4 \
  sr_cli -d "set / interface ethernet-1/1 admin-state disable"

echo "Fault injected. Waiting 60s for detection pipeline..."
sleep 60

# Count new detections
DET_AFTER=$(curl -s http://127.0.0.1:3000/api/detections \
  | python3 -c "import json,sys; d=json.load(sys.stdin); items=d if isinstance(d,list) else d.get('detections',[]); print(len(items))")
echo "Detections after: $DET_AFTER (delta=$((DET_AFTER - DET_BEFORE)))"

# Show detection details
curl -s http://127.0.0.1:3000/api/detections | python3 -c "
import json, sys
d = json.load(sys.stdin)
items = d if isinstance(d, list) else d.get('detections', [])
leaf4_dets = [it for it in items if 'leaf4' in str(it.get('device_address',''))]
print(f'Detections for srl-leaf4: {len(leaf4_dets)}')
for it in leaf4_dets:
    src_types = it.get('source_types', '?')
    lat_ms = it.get('correlation_latency_ms', '?')
    print(f\"  rule={it.get('rule_id','?'):30s}  sources={src_types}  latency={lat_ms}ms\")
"

# Show incidents
curl -s http://127.0.0.1:3000/api/incidents | python3 -c "
import json, sys
d = json.load(sys.stdin)
incidents = d if isinstance(d, list) else d.get('incidents', [])
leaf4_inc = [it for it in incidents if 'leaf4' in str(it)]
print(f'Incidents involving srl-leaf4: {len(leaf4_inc)}')
"

# Heal
docker exec clab-bonsai-signal-test-srl-leaf4 \
  sr_cli -d "set / interface ethernet-1/1 admin-state enable"
```

**Expected**:
- `delta ≥ 1` detections
- Rules fired: `interface_admin_down`, `bgp_session_down`, `bfd_session_down`
- `source_types` should include 1-3 sources (gNMI + SNMP + syslog if all active on leaf4)
- At least one incident created

---

## Phase 16 — Teardown and Results

### S-54: Stop bonsai cleanly

```bash
kill $(cat runtime/bonsai-signal-test.pid 2>/dev/null) 2>/dev/null || pkill -f 'bonsai' || true
sleep 3
echo "bonsai stopped"
```

---

### S-55: Stop linux-host1 traffic generators

```bash
docker exec clab-bonsai-signal-test-linux-host1 \
  bash -c "pkill softflowd; pkill otelcol-contrib; pkill ping" 2>/dev/null || true
```

---

### S-56: Optionally destroy the lab

```bash
# Preserve lab for additional manual testing:
# sudo containerlab inspect --topo lab/signal-test-lab/signal-test.clab.yml

# Full teardown when done:
sudo containerlab destroy \
  --topo lab/signal-test-lab/signal-test.clab.yml \
  --cleanup
```

---

## Summary Checklist

Copy this to your results `.md` after each run:

| Step | Test | Status |
|------|------|--------|
| S-00 | Clean slate + port check | ⬜ |
| S-01 | git pull at correct SHA | ⬜ |
| S-02 | cargo build --release | ⬜ |
| S-03 | Python proto regen | ⬜ |
| S-04 | cargo test | ⬜ |
| S-05 | ContainerLab deploy | ⬜ |
| S-06 | All 8 nodes running | ⬜ |
| S-07 | CA cert exists | ⬜ |
| S-08 | IS-IS converged | ⬜ |
| S-09 | BGP sessions established | ⬜ |
| S-10 | Vault init | ⬜ |
| S-11 | Bonsai started | ⬜ |
| S-12 | /health = ok | ⬜ |
| S-13 | All 8 ports listening | ⬜ |
| S-14 | 7 devices in graph | ⬜ |
| S-15 | gNMI: interface counters | ⬜ |
| S-16 | gNMI: BGP state | ⬜ |
| S-17 | gNMI: IS-IS adjacency | ⬜ |
| S-18 | gNMI: interface admin-down event | ⬜ |
| S-19 | gNMI: BGP down → detection | ⬜ |
| S-20 | gNMI: BFD session down | ⬜ |
| S-21 | Syslog: UDP archive receiving | ⬜ |
| S-22 | Syslog: commit message captured | ⬜ |
| S-23 | Syslog: fact extracted → graph | ⬜ |
| S-24 | (Removed — TCP syslog N/A) | — |
| S-25 | Syslog: multi-source fusion counter | ⬜ |
| S-26 | SNMP: manual trap test | ⬜ |
| S-27 | SNMP: linkDown trap from leaf3 | ⬜ |
| S-28 | SNMP: SNMP-sourced event in graph | ⬜ |
| S-29 | SNMP: BGP backward-transition trap | ⬜ |
| S-30 | BMP: archive receiving | ⬜ |
| S-31 | BMP: session log confirmed | ⬜ |
| S-32 | BMP: ROUTE_MONITORING messages | ⬜ |
| S-33 | BMP: multi-source fusion BMP+gNMI | ⬜ |
| S-34 | NetFlow: softflowd installed | ⬜ |
| S-35 | NetFlow: host1 interfaces configured | ⬜ |
| S-36 | NetFlow: traffic generated | ⬜ |
| S-37 | NetFlow: AppFlow nodes in graph | ⬜ |
| S-38 | NetFlow: CARRIES_FLOW edge | ⬜ |
| S-39 | OTLP: otelcol installed (or curl test) | ⬜ |
| S-40 | OTLP: collector config + start | ⬜ |
| S-41 | OTLP: Application node in graph | ⬜ |
| S-42 | OTLP: RUNS_SERVICE edge | ⬜ |
| S-43 | Correlation: gNMI+syslog same event | ⬜ |
| S-44 | Correlation: detection dedup (1 not 3) | ⬜ |
| S-45 | HostEndpoint: LLDP inference | ⬜ |
| S-46 | HostEndpoint: CONNECTED_TO edge | ⬜ |
| S-47 | Settings: GET /api/settings/streaming | ⬜ |
| S-48 | Settings: PATCH disable/enable syslog | ⬜ |
| S-49 | Collector health: uptime + queue | ⬜ |
| S-50 | Collector health: receiver badges | ⬜ |
| S-51 | Live UI: 3-panel layout manual check | ⬜ |
| S-52 | SSE: event stream flowing | ⬜ |
| S-53 | Round-trip: leaf4 isolation → incident | ⬜ |
| S-54 | Teardown: bonsai stopped | ⬜ |
| S-55 | Teardown: host1 processes stopped | ⬜ |
| S-56 | Teardown: lab destroyed (optional) | ⬜ |

---

## Common Failure Patterns and Fixes

### Build fails on Ubuntu — missing dependencies

```bash
sudo apt update && sudo apt install -y \
  build-essential pkg-config libssl-dev clang cmake protobuf-compiler \
  git curl wget jq python3 python3-pip python3-venv nodejs npm snmp
```

### ContainerLab deploy fails — Docker or ContainerLab not installed

```bash
# Install ContainerLab
bash -c "$(curl -sL https://get.containerlab.dev)"
# Verify
sudo containerlab version
```

### gNMI subscriptions not connecting — CA cert missing

```bash
ls -la lab/signal-test-lab/clab-bonsai-signal-test/.tls/ca/ca.pem
# If missing, the deploy didn't finish — re-run S-05 with --reconfigure
```

### Syslog port 5514 — packets arriving but not in archive

SRL syslog uses the management interface (172.100.109.x). The Docker host must have a route to this subnet:
```bash
ip route show | grep '172.100.109'
# If missing:
# ContainerLab creates the bonsai-mgmt bridge automatically — check:
ip link show bonsai-mgmt
```

### SNMP traps not arriving — community string mismatch

SRL uses `bonsai-test` as community. Bonsai SNMP receiver accepts all communities by default.
If traps are not in the archive, test the raw UDP path:
```bash
tcpdump -i any -n port 9162 &
sleep 5
# trigger trap from leaf3
docker exec clab-bonsai-signal-test-srl-leaf3 \
  sr_cli -d "set / interface ethernet-1/2 admin-state disable"
sleep 3
kill %1
```

### BMP session not establishing

SRL BMP requires BGP to be established first. Wait for S-09 to confirm BGP before checking BMP.
If BMP still doesn't establish after 60s:
```bash
docker exec clab-bonsai-signal-test-srl-spine1 \
  sr_cli -d "show network-instance default protocols bgp-monitoring"
```
Look for `state: session-established`. If state is `connecting`, the IP/port may be wrong — verify bonsai is listening on `:5000` and the container can reach `172.100.109.1`.

### NetFlow records not appearing in graph

Check the exporter identity fix (D3-15 T1): `TelemetryUpdate.target` must be the exporter IP, not flow src.
```bash
grep -i "netflow\|exporter" logs/bonsai-signal-test.log | tail -20
```

### OTLP receiver 4318 — no application nodes

Check that `peer.address` attribute in the span matches a Device address in the graph. The RUNS_SERVICE edge is only written when peer IP matches a Device or HostEndpoint:
```bash
curl -s http://127.0.0.1:3000/api/topology | python3 -c "
import json,sys
d=json.load(sys.stdin)
addrs = {dev['address'] for dev in d.get('devices',[])}
print('Device addresses in graph:', addrs)
"
# The OTLP span peer.address must match one of these
```

---

## Phase 17 — External Integrations Testing

**Prerequisite**: Phases 0–3 complete (bonsai running, lab deployed).
**Reference**: `docker/compose-signal-test.yml`, `docker/configs/signal-test.toml` (integration comments).

---

### S-57: Start external integration containers

```bash
cd /opt/bonsai
docker compose -f docker/compose-signal-test.yml up -d

# Wait for all services to become healthy (~2–3 min)
docker compose -f docker/compose-signal-test.yml ps
```

**Expected**: All services show `healthy` or `running`.
Services: `bonsai-netbox`, `bonsai-netbox-db`, `bonsai-netbox-redis`,
`bonsai-signal-prom`, `bonsai-signal-grafana`, `bonsai-signal-elastic`,
`bonsai-signal-kibana`, `bonsai-signal-splunk`.

```bash
# Quick reachability check for each service
curl -sf http://localhost:8080/api/ -H "Authorization: Token bonsai-signal-test-token-0000000001" \
  | python3 -m json.tool | grep -i '"status"'     # NetBox
curl -sf http://localhost:9090/-/healthy           # Prometheus
curl -sf http://localhost:9200/_cluster/health     # Elasticsearch
```

**Expected**: NetBox returns `"status": "ok"`, Prometheus returns `Prometheus is Healthy.`,
Elasticsearch returns `"status":"green"` or `yellow`.

---

### S-58: Verify ServiceNow PDI connectivity

```bash
export SNOW_PDI_URL="https://<your-pdi>.service-now.com"
export SNOW_PDI_USER="<pdi-user>"
export SNOW_PDI_PASSWORD="<pdi-password>"

curl -sf -u "${SNOW_PDI_USER}:${SNOW_PDI_PASSWORD}" \
  "${SNOW_PDI_URL}/api/now/table/sys_user?sysparm_limit=1" \
  | python3 -m json.tool | grep '"result"'
```

**Expected**: ServiceNow PDI returns a JSON `result` array. If this fails, verify the PDI is awake and the user has table API access.

---

### S-59: Provision vault credentials for integrations

```bash
export BONSAI_VAULT_PASSPHRASE=bonsai-signal-test-pass

# NetBox API token
./target/release/bonsai credential add \
  --alias netbox-lab \
  --username admin \
  --password bonsai-signal-test-token-0000000001

# Splunk HEC token
./target/release/bonsai credential add \
  --alias splunk-hec-lab \
  --username splunk \
  --password bonsai-signal-hec-token-00000001

# ServiceNow PDI
./target/release/bonsai credential add \
  --alias snow-pdi \
  --username "${SNOW_PDI_USER}" \
  --password "${SNOW_PDI_PASSWORD}"

# Verify vault entries
./target/release/bonsai credential list
```

**Expected**: Lists `netbox-lab`, `splunk-hec-lab`, `snow-pdi` with `purpose: enrich/aiops_event`.

---

### S-60: Register NetBox enricher via UI

1. Open `http://localhost:3000/integrations` in a browser.
2. Click **Enrichment Sources** tab → **+ Add enricher**.
3. Fill in:
   - Name: `netbox-lab`
   - Type: `NetBox (IPAM/DCIM)`
   - Base URL: `http://localhost:8080`
   - Credential alias: `netbox-lab`
   - Poll interval: `300`
   - **NetBox advanced options** → Endpoint roles: `server,ap,phone,cpe,printer,workstation`
4. Click **Test connection** → expect `✓ Connected`.
5. Click **Save enricher**.
6. Click **Run now** on the `netbox-lab` card.

**Expected**: Card shows "Last run: *timestamp* · Xs · N nodes touched". No errors.

Verify via API:
```bash
curl -s http://localhost:3000/api/enrichment | python3 -m json.tool | grep -E '"name"|"last_run"'
```

---

### S-61: Verify NetBox enrichment wrote graph nodes

```bash
# Check for VLAN, Prefix, Rack or HostEndpoint nodes added by NetBox enricher
curl -s 'http://localhost:3000/api/explorer/query' \
  -H 'Content-Type: application/json' \
  -d '{"cypher": "MATCH (n) WHERE n.netbox_site IS NOT NULL RETURN n.hostname, n.netbox_site, n.netbox_rack LIMIT 10"}' \
  | python3 -m json.tool
```

**Expected**: Returns Device nodes with `netbox_site` and `netbox_rack` properties populated from NetBox.

If NetBox has no devices yet (fresh install), seed it first:
```bash
# Seed NetBox with lab devices via API
for ip in 172.100.109.11 172.100.109.12 172.100.109.13 172.100.109.14 172.100.109.15 172.100.109.16 172.100.109.17; do
  name=$(grep -B2 "address.*${ip}" docker/configs/signal-test.toml | grep hostname | awk -F'"' '{print $2}')
  [ -z "$name" ] && continue
  curl -sf -X POST http://localhost:8080/api/dcim/devices/ \
    -H "Authorization: Token bonsai-signal-test-token-0000000001" \
    -H "Content-Type: application/json" \
    -d "{\"name\":\"${name}\",\"device_type\":1,\"role\":1,\"site\":1,\"status\":\"active\",
         \"primary_ip4\":{\"address\":\"${ip}/24\"}}" | python3 -m json.tool | grep '"id"'
done
```

Then re-run the enricher and re-verify.

---

### S-62: Register Prometheus Remote Write adapter via UI

1. Click **Output Adapters** tab → **+ Add adapter**.
2. Fill in:
   - Name: `prom-lab`
   - Type: `Prometheus Remote Write`
   - Endpoint URL: `http://localhost:9090/api/v1/write`
   - Flush interval: `15`
   - **Prometheus options** → Job label: `bonsai-signal-test`
3. Click **Test connection** → expect `✓ Connected`.
4. Click **Save adapter**.

**Expected**: Card appears with type badge **Prometheus Remote Write** in orange. Status shows "No push recorded yet — adapter starts on next server boot."

Restart bonsai to activate the adapter:
```bash
pkill -f bonsai || true; sleep 2
BONSAI_VAULT_PASSPHRASE=bonsai-signal-test-pass \
  ./target/release/bonsai --config docker/configs/signal-test.toml &
sleep 10
```

After 15 seconds (one flush interval), refresh the Integrations page.

**Expected**: Card shows "Last push: *timestamp* · N events · X KB".

Verify in Prometheus:
```bash
curl -sg 'http://localhost:9090/api/v1/query?query=bonsai_interface_statistics_in_octets' \
  | python3 -m json.tool | grep '"resultType"'
```

**Expected**: `"resultType": "vector"` with one or more results.

---

### S-63: Register Elasticsearch adapter via UI

1. Click **+ Add adapter**.
2. Fill in:
   - Name: `elastic-lab`
   - Type: `Elasticsearch Bulk API`
   - Endpoint URL: `http://localhost:9200`
   - Flush interval: `30`
   - **Elasticsearch options** → Index: `bonsai-detections`, Auth type: `Basic auth`, Dedup window: `60`
3. Click **Test connection** → expect `✓ Connected`.
4. Click **Save adapter**.

To generate a detection (needed for push verification):
```bash
# Inject a test detection
curl -s -X POST http://localhost:3000/api/_test/inject_detection \
  -H "Content-Type: application/json" \
  -d '{"device_address":"172.100.109.14","rule_id":"test_bgp_down","severity":"warning"}'
```

Wait 30 seconds for flush, then check Elasticsearch:
```bash
curl -s 'http://localhost:9200/bonsai-detections/_search?size=5' \
  | python3 -m json.tool | grep -E '"rule_id"|"severity"|"@timestamp"'
```

**Expected**: Returns hits with `rule.id = "test_bgp_down"`, `event.kind = "alert"`, `event.module = "bonsai"`.

---

### S-64: Register Splunk HEC adapter via UI

1. Click **+ Add adapter**.
2. Fill in:
   - Name: `splunk-lab`
   - Type: `Splunk HEC`
   - Endpoint URL: `http://localhost:8088`
   - Credential alias: `splunk-hec-lab`
   - Flush interval: `30`
   - **Splunk HEC options** → Sourcetype: `bonsai:detection`, Index: `main`, Skip TLS: ✓
3. Click **Test connection** → expect `✓ Connected`.
4. Click **Save adapter**.

Wait 30 seconds after injecting a detection (see S-63), then verify in Splunk:
```bash
# Query Splunk via REST API
curl -sk -u admin:bonsai-splunk-admin \
  'https://localhost:8089/services/search/jobs/export?search=search+index%3Dmain+sourcetype%3D%22bonsai%3Adetection%22&output_mode=json&count=5' \
  | python3 -c "import json,sys; [print(json.dumps(json.loads(l))) for l in sys.stdin if l.strip()]" 2>/dev/null \
  | grep -i 'rule_id\|severity' | head -10
```

**Expected**: Returns JSON lines containing `rule_id` and `severity` fields from the injected detection.

---

### S-65: Register ServiceNow EM adapter via UI

1. Click **+ Add adapter**.
2. Fill in:
   - Name: `snow-em-lab`
   - Type: `ServiceNow Event Mgmt`
   - Endpoint URL: your `${SNOW_PDI_URL}` value, for example `https://dev123456.service-now.com`
   - Credential alias: `snow-pdi`
   - Flush interval: `60`
   - **ServiceNow EM options** → Min severity: `Warning and above`, Min age: `30`, Dedup window: `120`
3. Click **Test connection** → expect `✓ Connected`.
4. Click **Save adapter**.

Wait 60 seconds after injecting a detection with severity ≥ warning. Verify the PDI received the event:
```bash
curl -sf -u "${SNOW_PDI_USER}:${SNOW_PDI_PASSWORD}" \
  "${SNOW_PDI_URL}/api/now/table/em_event?sysparm_limit=5&sysparm_query=source=bonsai^ORDERBYDESCsys_created_on" \
  | python3 -m json.tool | grep -E '"node"|"severity"|"source"'
```

**Expected**: Shows at least 1 `em_event` with `source = "bonsai"`, the device address in `node`, and expected severity.

---

### S-66: Verify push audit log in UI

1. On the **Integrations** page → **Output Adapters** tab, scroll to **Push audit log**.
2. Verify rows appear for `prom-lab`, `elastic-lab`, `splunk-lab`, `snow-em-lab`.
3. Check for `success` outcome badges. No `error` rows.

Via API:
```bash
curl -s http://localhost:3000/api/adapters/audit | python3 -m json.tool | grep -E '"adapter"|"outcome"|"events_pushed"'
```

**Expected**: Each adapter appears with `outcome: "success"` and `events_pushed > 0`.

---

### S-67: Grafana dashboard smoke test

1. Open `http://localhost:3001` (Grafana).
2. Login: `admin` / `bonsai-grafana`.
3. Add Prometheus datasource: `http://prometheus:9090`.
4. Create a simple graph: metric `bonsai_interface_statistics_in_octets`.

**Expected**: Time-series graph shows data points from the last 5 minutes.

---

### S-68: Full integration round-trip — fault → detect → push to all sinks

```bash
# 1. Inject leaf4 BGP fault (shut down uplink to spine2)
docker exec clab-bonsai-signal-test-srl-leaf4 \
  sr_cli -d "enter candidate; /interface ethernet-1/1 admin-state disable; commit stay"

sleep 90   # allow detection to fire + min_age filters to pass

# 2. Inject a detection directly (if rules engine hasn't fired yet)
curl -s -X POST http://localhost:3000/api/_test/inject_detection \
  -H "Content-Type: application/json" \
  -d '{"device_address":"172.100.109.17","rule_id":"bgp_session_down","severity":"critical"}'

sleep 65   # wait for all adapter flush cycles

# 3. Verify all sinks received the event
echo "=== Elasticsearch ==="
curl -s 'http://localhost:9200/bonsai-detections/_search?size=1&sort=@timestamp:desc' \
  | python3 -m json.tool | grep -E '"rule.id"|"host.ip"' || \
curl -s 'http://localhost:9200/bonsai-detections/_search?size=1' \
  | python3 -m json.tool | grep -E '"rule_id"|"device_address"'

echo "=== ServiceNow EM (PDI) ==="
curl -sf -u "${SNOW_PDI_USER}:${SNOW_PDI_PASSWORD}" \
  "${SNOW_PDI_URL}/api/now/table/em_event?sysparm_limit=5&sysparm_query=source=bonsai^ORDERBYDESCsys_created_on" \
  | python3 -m json.tool | grep -E '"node"|"severity"|"source"'

echo "=== Adapter push audit ==="
curl -s http://localhost:3000/api/adapters/audit \
  | python3 -c "
import json,sys
entries=json.load(sys.stdin).get('entries',[])
for e in entries[:8]:
    print(f'  {e[\"adapter\"]:20s}  {e[\"outcome\"]:10s}  {e.get(\"events_pushed\",0)} events')"

# 4. Heal the fault
docker exec clab-bonsai-signal-test-srl-leaf4 \
  sr_cli -d "enter candidate; /interface ethernet-1/1 admin-state enable; commit stay"
```

**Expected**:
- Elasticsearch: `rule_id = "bgp_session_down"` in `bonsai-detections`.
- ServiceNow PDI: ≥1 `em_event` row with `source = "bonsai"` and `severity = 1` (critical).
- Adapter audit: `elastic-lab`, `splunk-lab`, `snow-em-lab` all show `success` with `events_pushed ≥ 1`.

---

### S-69: Integrations UI — edit, disable, re-enable

1. On the Integrations page, click **Edit** on `elastic-lab`.
2. Change Dedup window to `30`.
3. Click **Test connection** from inside the form → expect `✓ Connected`.
4. Click **Save adapter**.
5. Toggle `enabled: false` → save → verify card shows greyed-out with `disabled` badge.
6. Re-enable → save → verify normal state restored.

---

### Phase 17 Summary Checklist

| Step | Test | ✅/❌ |
|------|------|------|
| S-57 | All 8 integration containers healthy | |
| S-58 | ServiceNow PDI table API reachable | |
| S-59 | Vault credentials: netbox-lab, splunk-hec-lab, snow-pdi | |
| S-60 | NetBox enricher registered + Test connection OK | |
| S-61 | NetBox enrichment wrote graph nodes (netbox_site property) | |
| S-62 | Prometheus adapter registered + push confirmed in Prometheus | |
| S-63 | Elasticsearch adapter registered + detection document in index | |
| S-64 | Splunk HEC adapter registered + event in Splunk main index | |
| S-65 | ServiceNow EM adapter registered + PDI received em_event | |
| S-66 | Push audit log visible in UI for all 4 adapters | |
| S-67 | Grafana shows bonsai telemetry metrics | |
| S-68 | Full round-trip: fault → detection → all 4 sinks pushed | |
| S-69 | UI edit/disable/re-enable cycle works correctly | |

---

### Phase 17 Troubleshooting

**NetBox 404 on enrichment run**
Verify NetBox is fully started: `docker logs bonsai-netbox --tail 30`. The `SKIP_SUPERUSER=false`
token `bonsai-signal-test-token-0000000001` is created on first boot. If it wasn't created:
```bash
docker exec -it bonsai-netbox python3 /opt/netbox/netbox/manage.py \
  create_token --user admin --token bonsai-signal-test-token-0000000001
```

**Elasticsearch `index_not_found_exception`**
The index is auto-created on first bulk push. If it doesn't appear, check Elasticsearch logs:
```bash
docker logs bonsai-signal-elastic --tail 20
```
Confirm security is disabled: `curl -sf http://localhost:9200 | grep cluster_name`.

**Splunk HEC returns 403**
The HEC token must match exactly. Verify via Splunk management API:
```bash
curl -sk -u admin:bonsai-splunk-admin \
  'https://localhost:8089/services/data/inputs/http?output_mode=json' \
  | python3 -m json.tool | grep '"token"'
```
If wrong, delete and recreate: Settings → Data Inputs → HTTP Event Collector.

**ServiceNow EM adapter: "credential resolve failed"**
The vault must be unlocked with `BONSAI_VAULT_PASSPHRASE=bonsai-signal-test-pass`. Re-export and restart bonsai.

**Prometheus remote-write: no metrics after restart**
Verify the adapter is running by checking the adapter list:
```bash
curl -s http://localhost:3000/api/adapters | python3 -m json.tool | grep '"is_running"'
```
If `false` for `prom-lab`, the adapter config may have been lost. Re-register via the UI.
Adapters load their configs from `runtime/adapter_configs.json` at startup.

**UI Integrations page: form not saving extra fields**
The `extra` object is passed as-is to the backend. If type-specific extra fields appear blank after reload, verify the `GET /api/adapters` response includes the `extra` object. Check with:
```bash
curl -s http://localhost:3000/api/adapters | python3 -m json.tool | grep -A5 '"extra"'
```
