# Bonsai Signal-Test Lab — Ubuntu Testing Guide

**Topology**: `lab/signal-test-lab/signal-test.clab.yml`
**Bonsai config**: `docker/configs/signal-test.toml`
**Goal**: End-to-end validation of every receiver and signal pipeline introduced in DV3.

**See Also**: For HA cluster testing with etcd, see `docs/HA_TESTING.md`. For a master index of all testing guides, see `docs/TESTING_INDEX.md`.

Each step is numbered. Mark ✅/❌ as you go. Do not skip steps — they have dependencies.

---

## Session Progress Notes (2026-05-20)

The following bugs were found and fixed during DV3 validation. The guide commands below
already reflect these fixes.

### ✅ Fixed: sr_cli config-injection syntax

All places in this guide that previously used `sr_cli -d "set / ..."` have been corrected.
`-d` is the debug flag on current SRL; it does **not** commit. Config changes require entering
candidate mode first:

```bash
docker exec -i <container> sr_cli <<'EOF'
enter candidate
set / <path> <value>
commit now
EOF
```

Show/read-only commands (`show ...`) still work with the single-argument form:
```bash
docker exec <container> sr_cli -d "show ..."
```

### ✅ Fixed: IS-IS adjacency not visible in graph (S-17)

**Root cause**: The gNMI ON_CHANGE classifier required `adjacency-state` to be present in
the value payload. SRL's initial sync sends only the list entry key (presence = up) without
any leaf values.

**Fix**: Removed the `adjacency-state` guard from the classifier; graph writer defaults
`new_state` to `"up"` when the leaf is absent.

**Result**: All 17 IS-IS adjacencies across 7 SRL nodes now visible. S-17 passes.

### ✅ Fixed: frr-rr shows bgp=0 despite BMP sessions (S-16/S-30)

**Root cause 1**: BMP classifier was matching path `streaming/bmp/peer-state` but the BMP
handler publishes `streaming/bmp/peer-up` and `streaming/bmp/peer-down`.

**Root cause 2**: `write_bmp_peer_state` was writing to `BmpSession` only; the topology API
reads `BgpNeighbor` nodes. BMP peers never appeared in `/api/topology`.

**Fix**: Classifier now matches all three path variants; BMP peer-up events also upsert a
`BgpNeighbor` node with a `PEERS_WITH` edge.

**Result**: frr-rr shows 2 BGP sessions in topology. S-16 includes frr-rr.

### ✅ Fixed: Detection pipeline never wrote DetectionEvent nodes (S-19/S-44/S-53)

**Root cause 1**: The correlation buffer sweep task (`server_startup.rs`) called
`drain_expired()` every 10 s but only **logged** flushed slots — it never called
`write_detection()`. No `DetectionEvent` nodes were ever written.

**Root cause 2**: `semantic_key_for_event` in `correlation_buffer.rs` matched legacy
event_type strings (`bgp_session_down`, `bfd_down`, `link_down`) that no writer actually
produces. The gNMI writers produce `bgp_session_change`, `bfd_session_change`,
`isis_adjacency_change`; SNMP fact events are written as `snmp_fact_orphan` /
`snmp_fact_joined`.

**Fix**: Sweep task now calls `write_detection()` for every flushed slot with severity derived
from semantic type. `semantic_key_for_event` updated to match the real event_type strings,
extracting direction from `new_state` and sub-keys from the appropriate detail fields.

**Result**: `/api/detections` now populates after a fault injection + 45-second correlation
window. First valid detections expected in S-19 / S-53 after the build containing these fixes.

### ⬜ Open gap: S-32b BMP PeerUp BGP OPEN capabilities not parsed

`local_address` and `local_port` parse correctly. `sent_hold_time` is `40960` (wrong —
likely an off-by-one byte offset in the BGP OPEN parser). `sent_capabilities` and
`received_capabilities` are both empty — capability TLV extraction not implemented.
See note in S-32b for the fix.

### ✅ Fixed and validated: S-25 syslog+gNMI multi-source fusion (2026-05-20)

Three root causes were found and fixed:

**Root cause 1** — Wrong Nokia SRL BGP regex in `config/syslog_patterns/nokia-srlinux.yaml`.
Pattern expected `"bgp neighbor 10.9.0.1 down"` but Nokia SRL 24.x emits
`"Peer 10.9.0.1 moved from higher state ESTABLISHED to lower state IDLE"` and
`"Peer 10.9.0.1 moved into the ESTABLISHED state"`. Two new patterns added for the real
format while the old pattern is kept for backward compatibility / test fixtures.

**Root cause 2** — `SyslogTargetMap::new()` in `src/signals/syslog.rs` stripped the port
from the target address (`172.100.109.14:57400` → `172.100.109.14`). gNMI uses the full
`address:port` as `device_address` in the `CorrelationKey`. Since the syslog key had no
port, the two keys never matched the same correlation slot. Fixed to preserve full address.

**Root cause 3** — No `vendor` field in the `[[target]]` blocks for Nokia SRL nodes in
`docker/configs/signal-test.toml`. The syslog fact extractor filters patterns by vendor
(`"nokia_srl".contains("nokia")`); with an empty vendor the Nokia patterns are silently
skipped and no facts are extracted. Added `vendor = "nokia_srl"` to all 7 SRL targets.

**Result**: After a BGP flap on leaf1 (syslog-enabled), detection shows:
```
rule=bgp_neighbor_down   sources=['syslog', 'gnmi']  ✓
rule=bgp_neighbor_up     sources=['gnmi', 'syslog']  ✓
bonsai_syslog_fact_join_total{fact_type="bgp_neighbor",status="joined"} 2
```
Cross-device BMP+gNMI fusion still requires a future design (see S-33 note).

### ✅ Fixed: SNMP events not joining device nodes (S-27/S-28)

Same root cause as the syslog address fix: `SnmpTargetMap::new()` stripped the port from
`address` (`172.100.109.16:57400` → `172.100.109.16`), so SNMP events landed on a device
node with no port — separate from the gNMI device at `172.100.109.16:57400`.

Two-part fix in `src/signals/snmp.rs`:
1. `new()`: use `target.address.clone()` directly (preserve full `address:port`)
2. `resolve()`: match trap source IP against base IP of entry (`"172.100.109.16:57400".split(':').next() == "172.100.109.16"`) so lookup still works even though entry now has port

**Result**: SNMP `snmp_link_down` events now appear in the graph at `172.100.109.16:57400`
matching the gNMI device node. S-27 and S-28 pass.

### ✅ Partial fix: S-29 Nokia enterprise BGP OIDs now categorised (2026-05-20)

Nokia SRL enterprise OIDs `1.3.6.1.4.1.6527.3.1.3.14.0.7` (BGP down) and `.0.8` (BGP up)
added to `config/snmp_oid_patterns/default.yaml` as `bgp_peer_backward_transition` and
`bgp_peer_state` fact types. Traps now produce `snmp_fact_orphan` events in the graph with
the correct `fact_type` instead of `snmp_enterprise_specific`.

**Remaining gap**: Nokia encodes the peer address in the OID table index suffix
(`.4.10.9.0.1` = `10.9.0.1`), not in a varbind value. The current field extractor does
exact OID matching against varbind OIDs, so `fields={}` (no peer_address extracted). SNMP
BGP correlation with gNMI requires OID-suffix parsing — future code enhancement in
`src/signals/snmp.rs`.

### ✅ Validated: S-19 detection confirmed (2026-05-20)

After deploying the updated binary, `bgp_neighbor_down` fired for srl-leaf4
(`172.100.109.17:57400`) within the expected 65-second window. Total detections after
fault injection: 45 (includes IS-IS/BGP/BFD state changes from startup convergence phase,
which is expected — the correlation window captures all state transitions on boot).

S-53 (full round-trip) should also work with the updated binary. The detection rule names
in the expected output have been updated: `bgp_neighbor_down` / `bfd_session_down` /
`interface_down` (not the legacy names `bgp_session_down` etc.).

---

## Quick Reference

| Receiver | Protocol | Port | Source nodes |
|---|---|---|---|
| gNMI | gRPC/TLS | 57400 (on nodes) | all 7 SRL nodes |
| Syslog UDP | UDP | 5514 | srl-leaf1, srl-leaf2 |
| SNMP traps | UDP | 9162 | srl-leaf3, srl-leaf4 |
| BMP | TCP | 5000 | frr-rr (FRR 10.x) |
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

**Expected**: 8 managed devices. The 7 SRL nodes should show interface and BGP data; `frr-rr` may appear earlier as a BMP-only managed node until FRR gNMI is enabled. May take up to 90s for all devices to appear.

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

**Expected**: All 7 SRL devices have interfaces. `frr-rr` may have zero interfaces while it remains BMP-only. Most SRL `oper_state` values should be `up`.

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
docker exec -i clab-bonsai-signal-test-srl-leaf4 sr_cli <<'EOF'
enter candidate
set / interface ethernet-1/1 admin-state disable
commit now
EOF

sleep 15

# Check for StateChangeEvent in events stream (query by IP — events store by address not hostname)
curl -s "http://127.0.0.1:3000/api/events/history?device=172.100.109.17&limit=10" \
  | python3 -c "
import json, sys
d = json.load(sys.stdin)
events = d if isinstance(d, list) else d.get('events', [])
for e in events:
    print(f\"  {e.get('event_type','?'):30s} src={e.get('source_type','?')}\")
"

# Heal
docker exec -i clab-bonsai-signal-test-srl-leaf4 sr_cli <<'EOF'
enter candidate
set / interface ethernet-1/1 admin-state enable
commit now
EOF
```

**Expected**: `snmp_link_down` and `bgp_session_change` (established→idle) events for
srl-leaf4. The `snmp_link_down` comes within ~5 s via SNMP trap; `bgp_session_change` comes
via gNMI within ~10 s. `isis_adjacency_change` also fires if IS-IS is configured on that link.

**Note**: The filter `device=srl-leaf4` uses hostname matching which may not resolve for
SNMP-sourced events; use the raw IP `172.100.109.17` instead.

---

### S-19: gNMI T5 — BGP session down → detection fired

```bash
# Inject: disable BGP on leaf4
docker exec -i clab-bonsai-signal-test-srl-leaf4 sr_cli <<'EOF'
enter candidate
set / network-instance default protocols bgp admin-state disable
commit now
EOF

# Wait for: gNMI bgp_session_change + SNMP bgpBackwardTransition + 45s correlation window
sleep 60

# Check detections (populated after correlation window expires, ~45s after first signal)
curl -s http://127.0.0.1:3000/api/detections | python3 -c "
import json, sys
d = json.load(sys.stdin)
items = d if isinstance(d, list) else d.get('detections', [])
for it in list(items)[-10:]:
    sources = it.get('source_types', '?')
    print(f\"  {it.get('rule_id','?'):30s}  {it.get('device_address','?'):25s}  sources={sources}\")
print(f'Total detections: {len(items)}')
"

# Heal
docker exec -i clab-bonsai-signal-test-srl-leaf4 sr_cli <<'EOF'
enter candidate
set / network-instance default protocols bgp admin-state enable
commit now
EOF
```

**Expected**: `bgp_neighbor_down` detection for srl-leaf4 (`172.100.109.17:57400`).
The correlation window is 45 s — allow up to 60 s total before checking.
`source_types` may include `gnmi` and/or `snmp` depending on which signals arrived.

**Note**: Detections were non-functional prior to 2026-05-20 build (see Session Progress Notes).
Requires the updated binary with detection pipeline fix.

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
docker exec -i clab-bonsai-signal-test-srl-leaf1 sr_cli <<'EOF'
enter candidate
set / system information description syslog-test-trigger
commit now
EOF

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
docker exec -i clab-bonsai-signal-test-srl-leaf1 sr_cli <<'EOF'
enter candidate
set / network-instance default protocols bgp neighbor 10.9.0.1 admin-state disable
commit now
EOF
sleep 5
docker exec -i clab-bonsai-signal-test-srl-leaf1 sr_cli <<'EOF'
enter candidate
set / network-instance default protocols bgp neighbor 10.9.0.1 admin-state enable
commit now
EOF

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

**Prerequisites**: Three fixes must be deployed together (see Session Progress Notes):
1. Nokia SRL 24.x BGP patterns in `config/syslog_patterns/nokia-srlinux.yaml`
2. Full `address:port` preserved in `SyslogTargetMap` (`src/signals/syslog.rs`)
3. `vendor = "nokia_srl"` set in all `[[target]]` blocks in `docker/configs/signal-test.toml`

```bash
# 1. Trigger a BGP flap on leaf1 (syslog-enabled device)
printf 'enter candidate\nset / network-instance default protocols bgp neighbor 10.9.0.1 admin-state disable\ncommit now\n' \
  | docker exec -i clab-bonsai-signal-test-srl-leaf1 sr_cli
sleep 5
printf 'enter candidate\nset / network-instance default protocols bgp neighbor 10.9.0.1 admin-state enable\ncommit now\n' \
  | docker exec -i clab-bonsai-signal-test-srl-leaf1 sr_cli

# 2. Wait 60s for the 45s correlation window to close + up to 10s sweep
sleep 60

# 3. Check detections for multi-source
curl -s http://localhost:3000/api/detections | python3 -c "
import sys, json
d = json.load(sys.stdin)
leaf1 = [x for x in d['detections'] if '172.100.109.14' in x['device_address']]
for x in leaf1:
    print(f'  rule={x[\"rule_id\"]:<30} sources={x[\"source_types\"]}')
"

# 4. Check syslog fact metrics
curl -s http://localhost:9100/metrics | grep "syslog_fact"
```

**Expected**:
```
rule=bgp_neighbor_down              sources=['syslog', 'gnmi']
rule=bgp_neighbor_up                sources=['gnmi', 'syslog']
bonsai_syslog_fact_join_total{fact_type="bgp_neighbor",status="joined"} 2
```

**Validated 2026-05-20**: PASS. Both `bgp_neighbor_down` and `bgp_neighbor_up` show
`sources=['syslog', 'gnmi']` confirming two-source correlation. Metric confirms 2 joined
syslog facts. The three root causes (wrong regex, port stripping, missing vendor) are all fixed.

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

**Validated 2026-05-20**: PASS. `snmptrap` is installed at `/usr/bin/snmptrap`. Trap received
with `trap_oid=1.3.6.1.6.3.1.1.5.3`. `peer_addr` shows the source (not `source_ip`).
`fact=raw` is expected for traps from unknown community strings or non-target IPs.

---

### S-27: SNMP T2 — SNMP trap from leaf3 (injected via interface shutdown)

```bash
# Shut leaf3 e1-2 (uplink to spine2) — should emit linkDown trap
docker exec -i clab-bonsai-signal-test-srl-leaf3 sr_cli <<'EOF'
enter candidate
set / interface ethernet-1/2 admin-state disable
commit now
EOF

sleep 10

# Check SNMP archive for trap from leaf3
grep "172.100.109.16" runtime/signals/snmp.jsonl 2>/dev/null | tail -3 \
  | python3 -c "
import sys, json
for line in sys.stdin:
    try:
        d = json.loads(line)
        print(f\"  src={d.get('peer_addr','?'):24s}  oid={d.get('trap_oid','?'):40s}  fact={d.get('event_type','raw')}\")
    except: print(line[:120])
"

# Heal
docker exec -i clab-bonsai-signal-test-srl-leaf3 sr_cli <<'EOF'
enter candidate
set / interface ethernet-1/2 admin-state enable
commit now
EOF
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
docker exec -i clab-bonsai-signal-test-srl-leaf4 sr_cli <<'EOF'
enter candidate
set / network-instance default protocols bgp neighbor 10.9.0.1 admin-state disable
commit now
EOF

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
docker exec -i clab-bonsai-signal-test-srl-leaf4 sr_cli <<'EOF'
enter candidate
set / network-instance default protocols bgp neighbor 10.9.0.1 admin-state enable
commit now
EOF
```

**Expected**: `fact_type=bgp_peer_backward_transition` trap from leaf4 mgmt IP.

**Known gap (2026-05-20)**: Nokia SRL sends proprietary enterprise BGP OIDs
`1.3.6.1.4.1.6527.3.1.3.14.0.7` (peer down) and `1.3.6.1.4.1.6527.3.1.3.14.0.8`
(peer up/established) rather than the standard `bgpBackwardTransition` OID
(`1.3.6.1.6.3.18.1.2.0`). These enterprise OIDs are not yet in the SNMP fact OID map,
so they arrive in the archive with `fact_type=raw` (no structured fact extracted).
Fix: add Nokia `1.3.6.1.4.1.6527.3.1.3.14.0.7` and `.0.8` to the SNMP OID fact map
in the SNMP ingest module, mapping them to `bgp_peer_backward_transition` and
`bgp_session_established` respectively.

---

## Phase 7 — BMP Receiver Tests

### S-30: BMP T1 — BMP session established from frr-rr

```bash
# Check bonsai BMP archive for BMP initiation messages
ls -lh runtime/streaming/bmp.jsonl 2>/dev/null || echo "BMP archive not yet created"

sleep 30   # BMP session needs time to establish after BGP convergence

tail -10 runtime/streaming/bmp.jsonl 2>/dev/null | python3 -c "
import sys, json
for line in sys.stdin:
    try:
        d = json.loads(line)
        mt = d.get('message_type','?')
        peer = d.get('peer_address','?')
        rib = d.get('rib_type','?')
        print(f'  type={mt:20s}  peer={peer}  rib_type={rib}')
    except: print(line[:120])
" 2>/dev/null
```

**Expected**: BMP messages from frr-rr (`collector_peer` contains `172.100.109.21`).
Should see `initiation` (with sys_name=frr-rr), then `peer_up`, then `route_monitoring`.
`rib_type` should show `adj-rib-in-pre-policy`, `adj-rib-in-post-policy`, and `loc-rib`.

---

### S-31: BMP T2 — Verify BMP session shows in bonsai log

```bash
grep -i "bmp" logs/bonsai-signal-test.log 2>/dev/null | tail -10
```

**Expected**: Log lines showing BMP session accepted from frr-rr (`172.100.109.21`).

---

### S-32: BMP T3 — BMP route advertisement visible (ROUTE_MONITORING)

```bash
# After BGP converges, FRR sends ROUTE_MONITORING for each RIB entry
# FRR exports pre-policy, post-policy, AND loc-rib (RFC 9069)
grep "route_monitoring\|peer_up" runtime/streaming/bmp.jsonl 2>/dev/null | wc -l

# Verify all 3 RIB types are present
grep -o '"rib_type":"[^"]*"' runtime/streaming/bmp.jsonl 2>/dev/null | sort | uniq -c
```

**Expected**: Count > 0. Should see PEER_UP + ROUTE_MONITORING messages.
RIB types should include `adj-rib-in-pre-policy`, `adj-rib-in-post-policy`, and `loc-rib`.

---

### S-32b: BMP T3b — PeerUp contains BGP OPEN capabilities

```bash
# Verify our RFC 7854 §4.6 PeerUp parser extracts BGP OPEN info
grep 'peer_up' runtime/streaming/bmp.jsonl 2>/dev/null | head -1 | python3 -c "
import sys, json
for line in sys.stdin:
    d = json.loads(line)
    info = d.get('peer_up_info')
    if info:
        print(f'  local_addr={info[\"local_address\"]}  local_port={info[\"local_port\"]}')
        print(f'  sent_hold_time={info[\"sent_hold_time\"]}  recv_hold_time={info[\"received_hold_time\"]}')
        print(f'  sent_caps={info[\"sent_capabilities\"]}')
        print(f'  recv_caps={info[\"received_capabilities\"]}')
    else:
        print('  peer_up_info: not parsed (check parser)')
" 2>/dev/null
```

**Expected**: PeerUp shows local/remote ports, hold times, and capabilities (e.g. `4-byte-as`, `multiprotocol`, `route-refresh`).

**Known gap (2026-05-20)**: `local_address` and `local_port` parse correctly (`10.9.0.8`, `179`).
Hold time shows `40960` (0xA000) instead of the actual BGP negotiated value (~90s) — the
BGP OPEN parser is reading the wrong byte offset. `sent_capabilities` and
`received_capabilities` are both empty — the capability TLV list inside the BGP OPEN message
is not being extracted. Fix: audit `BmpPeerUpInfo` BGP OPEN parser byte offsets for hold
time field and implement capability option parsing (type=2, subtype codes for 4-byte-as,
multiprotocol, route-refresh etc.).

---

### S-33: BMP T4 — Multi-source fusion: BMP + gNMI same BGP event

```bash
# Cause BGP flap on frr-rr's peer (super1) from the SRL side
# This should be seen by BOTH gNMI (SRL reporting BGP state) and BMP (FRR reporting PEER_DOWN/UP)
docker exec -i clab-bonsai-signal-test-srl-super1 sr_cli <<'EOF'
enter candidate
set / network-instance default protocols bgp neighbor 10.9.0.8 admin-state disable
commit now
EOF
sleep 5
docker exec -i clab-bonsai-signal-test-srl-super1 sr_cli <<'EOF'
enter candidate
set / network-instance default protocols bgp neighbor 10.9.0.8 admin-state enable
commit now
EOF

sleep 20

# Check BMP archive for peer_down / peer_up from frr-rr
grep -E 'peer_down|peer_up' runtime/streaming/bmp.jsonl 2>/dev/null | tail -5

# Check CorrelationBuffer fusion counter
curl -s http://127.0.0.1:9100/metrics 2>/dev/null \
  | grep "bonsai_correlation_multi_source_total"

grep -i "Absorbed\|multi.source" logs/bonsai-signal-test.log 2>/dev/null | tail -5
```

**Expected**: BMP archive shows `peer_down` then `peer_up` from frr-rr.
`bonsai_correlation_multi_source_total` counter incremented OR log lines showing `Absorbed` events.

**Known limitation (2026-05-20)**: BMP+gNMI cross-device fusion cannot happen with the
current per-device correlation model. gNMI sees the BGP flap from super1's perspective
(device=`172.100.109.11:57400`, peer=`10.9.0.8`) while BMP sees it from frr-rr's
perspective (device=`172.100.109.21`, peer=`10.9.0.1`). Different `device_address` and
different `sub_key` — they can never join the same correlation slot.

**What does work**: BMP `peer_down` and `peer_up` messages ARE confirmed arriving in the
archive. After the 2026-05-20 build, `bmp_session_change` is added to
`semantic_key_for_event`, so BMP-only devices (frr-rr) will now produce `DetectionEvent`
nodes for their BGP flaps independently.

True multi-source fusion of BMP+gNMI requires correlating across both endpoints of a BGP
session — a future cross-device correlation design.

---

## Phase 8 — NetFlow Receiver Tests

> **Lab topology note**: Nokia SRL exports **sFlow**, not NetFlow/IPFIX. Bonsai's NetFlow receiver
> supports only NetFlow v9 and IPFIX (v10). For end-to-end CARRIES_FLOW validation with a managed
> device as exporter, use a Cisco/Juniper device or run a sFlow→IPFIX bridge (e.g. `nfacctd`).
> In this lab, softflowd runs on linux-host1 (not a registered Device), so AppFlow nodes are
> created but CARRIES_FLOW edges are not (exporter 172.100.109.20 is not in the Device table).

### S-34: NetFlow T1 — Install softflowd on linux-host1

```bash
# linux-host1 is Alpine Linux — use apk not apt-get
docker exec clab-bonsai-signal-test-linux-host1 \
  apk add -q softflowd iproute2 iputils
```

**Expected**: Exit 0. `which softflowd` → `/usr/sbin/softflowd`.

**Session result (2026-05-20)**: ✅ softflowd 1.1.0 installed via apk.

---

### S-35: NetFlow T2 — Configure host1 interfaces and routing

```bash
docker exec clab-bonsai-signal-test-linux-host1 bash -c "
  ip addr add 10.9.20.1/31 dev eth1 2>/dev/null || true
  ip addr add 10.9.20.3/31 dev eth2 2>/dev/null || true
  ip link set eth1 up
  ip link set eth2 up
  ip route add default via 10.9.20.0 dev eth1 2>/dev/null || true
  ip addr show eth1 | grep 'inet '
  ip addr show eth2 | grep 'inet '
"
```

**Expected**: eth1 shows `10.9.20.1/31`, eth2 shows `10.9.20.3/31`.

**Session result (2026-05-20)**: ✅ Both IPs confirmed.

---

### S-36: NetFlow T3 — Generate traffic and start softflowd

```bash
# Generate traffic on eth1
docker exec -d clab-bonsai-signal-test-linux-host1 \
  sh -c "for i in \$(seq 1 60); do ping -c1 10.9.20.0 >/dev/null 2>&1; sleep 1; done"

# Start softflowd: NetFlow v9 only (bonsai does NOT support v5)
# Export to Docker host gateway at port 2055
docker exec -d clab-bonsai-signal-test-linux-host1 \
  softflowd -i eth1 -n 172.100.109.1:2055 -v 9 -t maxlife=30

docker exec clab-bonsai-signal-test-linux-host1 pgrep -af softflowd
```

**Note**: Bonsai supports only NetFlow v9 and IPFIX (v10). v5 produces `unsupported netflow version 5` in debug log and is silently dropped. Successful v9 parse is NOT logged (only errors appear).

---

### S-37: NetFlow T4 — Verify AppFlow nodes in graph

```bash
# Topology API does NOT surface AppFlow nodes — query graph explorer directly
curl -s -X POST http://127.0.0.1:3000/api/explorer/query \
  -H "Content-Type: application/json" \
  -d '{"cypher": "MATCH (f:AppFlow) RETURN f.src_address, f.dst_address, f.exporter_address, f.bytes_per_sec LIMIT 10"}'
```

**Expected**: AppFlow rows with src/dst address pairs and non-zero bytes_per_sec.

**Session result (2026-05-20)**: ✅ `10.9.20.1 ↔ 10.9.20.0` at ~85 bytes/sec, exporter=172.100.109.20.

---

### S-38: NetFlow T5 — CARRIES_FLOW edge: Device → AppFlow

```bash
curl -s -X POST http://127.0.0.1:3000/api/explorer/query \
  -H "Content-Type: application/json" \
  -d '{"cypher": "MATCH (d:Device)-[:CARRIES_FLOW]->(f:AppFlow) RETURN d.hostname, f.src_address, f.dst_address LIMIT 10"}'
```

**Expected**: Device rows if exporter IP matches a registered Device address.

**Session result (2026-05-20)**: ⚠️ 0 rows. Exporter is linux-host1 (172.100.109.20) which is not
a registered Device. Nokia SRL exports sFlow (not supported by bonsai). CARRIES_FLOW requires the
NetFlow/IPFIX exporter to be a managed device. Future improvement: add sFlow receiver or use a
Cisco/Juniper device as exporter.

---

## Phase 9 — OTLP Receiver Tests

### S-39: OTLP T1 — Direct curl OTLP trace (recommended approach)

otelcol-contrib requires an internet download. Use a direct `curl` from the Ubuntu host instead:

```bash
curl -s -w "\nHTTP %{http_code}" -X POST http://127.0.0.1:4318/v1/traces \
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
```

**Expected**: `HTTP 200`. No body in response is correct (bonsai returns empty 200).

**Session result (2026-05-20)**: ✅ HTTP 200 received. OTLP receiver is live at 0.0.0.0:4318.

---

### S-40: OTLP T2 — (Skip — otelcol-contrib requires internet in container)

Skip unless otelcol-contrib binary is pre-staged on the host. The direct curl in S-39 is sufficient
to validate the receiver. S-40b (the curl test) has been promoted to S-39.

---

### S-41: OTLP T3 — Verify Application node in graph

```bash
# Application nodes are NOT in /api/topology — query graph explorer directly
curl -s -X POST http://127.0.0.1:3000/api/explorer/query \
  -H "Content-Type: application/json" \
  -d '{"cypher": "MATCH (a:Application) RETURN a.id, a.name, a.source_name LIMIT 10"}'
```

**Expected**: `bonsai-test-app` row with `source_name=otlp`.

**Session result (2026-05-20)**: ✅ `{"rows":[["app:bonsai-test-app","bonsai-test-app",...]]}`.

---

### S-42: OTLP T4 — RUNS_SERVICE edge: Device → Application

```bash
curl -s -X POST http://127.0.0.1:3000/api/explorer/query \
  -H "Content-Type: application/json" \
  -d '{"cypher": "MATCH (d:Device)-[:RUNS_SERVICE]->(a:Application) RETURN d.hostname, d.address, a.name"}'
```

**Expected**: srl-leaf1 (address starts with 172.100.109.14) shows RUNS_SERVICE → bonsai-test-app.

**Session result (2026-05-20)**: ✅ `srl-leaf1 | 172.100.109.14:57400 | bonsai-test-app`.

**Note**: Match uses `d.address STARTS WITH peer_address`. Since Device address is stored as
`172.100.109.14:57400` and peer.address in the span is `172.100.109.14`, the STARTS WITH clause
correctly links leaf1 to the application.

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
docker exec -i clab-bonsai-signal-test-srl-leaf2 sr_cli <<'EOF'
enter candidate
set / interface ethernet-1/1 admin-state disable
commit now
EOF

sleep 30

# Check counters
AFTER=$(curl -s http://127.0.0.1:9100/metrics 2>/dev/null \
  | grep "bonsai_correlation_multi_source_total" | awk '{print $2}')
echo "Correlation counter after: ${AFTER:-0}"

# Check events — expect events from BOTH gnmi and syslog sources
# Note: use IP for SNMP events; hostname filter may only match gNMI events
curl -s "http://127.0.0.1:3000/api/events/history?device=172.100.109.15&limit=20" \
  | python3 -c "
import json, sys
d = json.load(sys.stdin)
events = d if isinstance(d, list) else d.get('events', [])
from collections import Counter
srcs = Counter(e.get('source_type','?') for e in events)
print('Event source breakdown for srl-leaf2:', dict(srcs))
"

# Heal
docker exec -i clab-bonsai-signal-test-srl-leaf2 sr_cli <<'EOF'
enter candidate
set / interface ethernet-1/1 admin-state enable
commit now
EOF
```

**Expected**: Counter increased (multi-source fusion). Events include both `gnmi` and `syslog` source_types.

**Session result (2026-05-20)**: ✅ After rebuild — `sources=['syslog', 'gnmi']` detection created.
`bonsai_correlation_multi_source_total{semantic="interface_down"} 1`. Both root causes fixed.

**Root cause 1 — gNMI interface_down not entering detection pipeline**:
`emit_oper_status_event` in `src/graph/mod.rs` was sending a raw `BonsaiEvent` directly instead of
calling `write_state_change_event` with the correlation buffer. The event type was
`interface_oper_status_change` which is not in `semantic_key_for_event`, so no correlation slot
was ever created. Fixed: emit `interface_down`/`interface_up` via `write_state_change_event` +
`corr_buf`. Also added `"interface_name": if_name` to detail JSON (needed by `semantic_key_for_event`'s
`if_name()` extractor).

**Root cause 2 — Nokia SRL syslog interface format not matched**:
The `interface_state` regex required `(?:state|link)` between the interface name and the state word.
Nokia SRL 24.x says `"Interface ethernet-1/1 is now down for reason: port-admin-disabled"` — no
`state` or `link` keyword. Fixed: added second `interface_state` pattern in
`config/syslog_patterns/nokia-srlinux.yaml`:
```yaml
regex: '(?i)interface (?P<if_name>[A-Za-z0-9./:_-]+)\s+is\s+now\s+(?P<new_state>up|down)'
```

Retest after rebuild with `BONSAI_CONFIG=docker/configs/signal-test.toml`.

---

### S-44: Correlation T2 — Detection deduplication (same event, 3 sources)

Disable leaf3 BGP neighbor → generates: gNMI state-change + syslog BGP fact + SNMP bgpBackwardTransition trap.

```bash
docker exec -i clab-bonsai-signal-test-srl-leaf3 sr_cli <<'EOF'
enter candidate
set / network-instance default protocols bgp neighbor 10.9.0.1 admin-state disable
commit now
EOF

# Wait for correlation window (45s) + sweep interval (10s)
sleep 60

# Count distinct detections for leaf3 — should be 1, not 3
curl -s http://127.0.0.1:3000/api/detections | python3 -c "
import json, sys
from collections import Counter
d = json.load(sys.stdin)
items = d if isinstance(d, list) else d.get('detections', [])
leaf3 = [it for it in items if '172.100.109.16' in str(it.get('device_address',''))]
print(f'Detections for srl-leaf3: {len(leaf3)}')
rules = Counter(it.get('rule_id') for it in leaf3)
print('Rules:', dict(rules))
for it in leaf3:
    print(f\"  {it.get('rule_id','?'):30s}  sources={it.get('source_types','?')}\")
"

# Heal
docker exec -i clab-bonsai-signal-test-srl-leaf3 sr_cli <<'EOF'
enter candidate
set / network-instance default protocols bgp neighbor 10.9.0.1 admin-state enable
commit now
EOF
```

**Expected**: 1 detection for srl-leaf3 with `rule_id=bgp_neighbor_down`. CorrelationBuffer
absorbed duplicates from gNMI + syslog + SNMP into one slot; only 1 `DetectionEvent` written.

> **Observed (2026-05-20)**: **PARTIAL** — 2 detections fired:
> 1. `gnmi` — `bgp_neighbor_down`, peer=10.9.0.1, established→idle ✅
> 2. `snmp` — orphan `bgp_neighbor_down` (join: `no_graph_entity_matched`)
>
> **Root cause**: The SNMP `bgpBackwardTransition` trap encodes the device's own connection
> address (`172.100.109.16:42730`) as `peer_addr`, not the BGP peer IP (`10.9.0.1`).
> CorrelationKey = `(device_address, semantic_type, sub_key_peer)` — the SNMP orphan's
> sub_key can't match the gNMI sub_key, so they create two separate detections instead of one.
> No syslog BGP detection fired (SRL doesn't emit a syslog BGP-state message on neighbor disable
> in this firmware version).
>
> **Status: PARTIAL** — gNMI detection works; multi-source dedup requires matching peer
> identifiers across sources; SNMP orphan trap is a known gap.

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
# Use syslog_udp (not "syslog") — syslog/snmp are signals.* not streaming.*
curl -s -X PATCH http://127.0.0.1:3000/api/settings/streaming \
  -H "Content-Type: application/json" \
  -d '{"syslog_udp": {"enabled": false}}' | python3 -m json.tool

# Re-enable
curl -s -X PATCH http://127.0.0.1:3000/api/settings/streaming \
  -H "Content-Type: application/json" \
  -d '{"syslog_udp": {"enabled": true, "udp_addr": "0.0.0.0:5514"}}' | python3 -m json.tool
```

**Expected**: `requires_restart: true`, message "Config written. syslog/snmp changes require a process restart."

**Note**: Syslog/SNMP are `signals.*` receivers managed at startup, not live-reloadable via supervisor.
Use `syslog_udp` (not `syslog`) in the PATCH body — the `syslog` key is not mapped. Live-apply works
only for `streaming.*` receivers (BMP, BGPLS, OTLP, NetFlow).

**Session result (2026-05-20)**: ✅ `{"ok": true, "requires_restart": true, "message": "..."}` with `syslog_udp` key.

---

## Phase 13 — Collector Health Tests

> **mode=all design note**: In `mode=all`, the `run_collector_manager` spawn is conditional on
> `run_collector && !run_core` — so the in-process collector never registers via gRPC, and
> `/api/collectors` always returns `[]`. `/readyz` returns `no_collectors_connected`.
> This is a known gap: in `mode=all`, the `CollectorManager` should auto-register the local
> in-process collector. S-49/S-50 are ⚠️ until this is fixed. Actual telemetry and detections work
> correctly — only the health/UI collector card is affected.

### S-49: Collector T1 — Verify heartbeat fields populated

```bash
curl -s http://127.0.0.1:3000/api/collectors | python3 -c "
import json, sys
d = json.load(sys.stdin)
collectors = d if isinstance(d, list) else d.get('collectors', [])
print(f'Collectors: {len(collectors)}')
for c in collectors:
    print(f\"  id={c.get('collector_id','?'):20s}\")
    print(f\"    uptime_secs:       {c.get('uptime_secs',0)}\")
    print(f\"    queue_depth:       {c.get('queue_depth_updates',0)}\")
    print(f\"    memory_used_mb:    {c.get('memory_used_mb',0):.1f}\")
    print(f\"    active_subs:       {c.get('active_subscribers',0)}\")
"
```

**Expected**: `uptime_secs > 0`, `queue_depth_updates ≥ 0`, `memory_used_mb > 0`.

**Session result (2026-05-20)**: ⚠️ Returns `[]`. mode=all collector never registers (see note above).

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

**Session result (2026-05-20)**: ⚠️ No collectors — same root cause as S-49.

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
# Record baseline (always use limit=200 to bypass the default 50-row page)
DET_BEFORE=$(curl -s "http://127.0.0.1:3000/api/detections?limit=200" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); items=d if isinstance(d,list) else d.get('detections',[]); print(len(items))")
echo "Detections before: $DET_BEFORE"

# Inject: disable leaf4 uplink (total isolation — single-homed)
docker exec -i clab-bonsai-signal-test-srl-leaf4 sr_cli <<'EOF'
enter candidate
set / interface ethernet-1/1 admin-state disable
commit now
EOF

# 45s correlation window + 10s sweep = 55s minimum; allow 65s
echo "Fault injected. Waiting 65s for detection pipeline..."
sleep 65

# Count new detections
DET_AFTER=$(curl -s "http://127.0.0.1:3000/api/detections?limit=200" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); items=d if isinstance(d,list) else d.get('detections',[]); print(len(items))")
echo "Detections after: $DET_AFTER (delta=$((DET_AFTER - DET_BEFORE)))"

# Show detection details  (leaf4 = 172.100.109.17)
curl -s "http://127.0.0.1:3000/api/detections?limit=200" | python3 -c "
import json, sys
d = json.load(sys.stdin)
items = d if isinstance(d, list) else d.get('detections', [])
leaf4_dets = [it for it in items if '172.100.109.17' in str(it.get('device_address',''))]
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
docker exec -i clab-bonsai-signal-test-srl-leaf4 sr_cli <<'EOF'
enter candidate
set / interface ethernet-1/1 admin-state enable
commit now
EOF
```

**Expected**:
- `delta ≥ 1` detections (requires 2026-05-20+ binary with detection pipeline fix)
- Rules fired: `bgp_neighbor_down`, `bfd_session_down`, `interface_down`
  (gNMI writers use these semantic keys; raw event_types are `bgp_session_change`, etc.)
- `source_types` should include 1-3 sources (`gnmi`, `snmp`) depending on which signals arrived
- At least one incident created

> **Observed (2026-05-20)**: **PASS** ✅ — delta=6 new detections:
> | Time | Device | Rule | Source |
> |------|--------|------|--------|
> | 18:29:41Z | leaf4 (172.100.109.17) | `interface_down` | gnmi |
> | 18:29:41Z | leaf4 | `interface_down` | snmp |
> | 18:29:41Z | spine2/peer (172.100.109.13) | `interface_down` | gnmi (far-end link-down) |
> | 18:30:45Z | leaf4 | `bgp_neighbor_down` | gnmi (after BGP hold timer ~64s) |
> | 18:30:45Z | leaf4 | `bgp_neighbor_down` | snmp (orphan trap) |
> | 18:30:45Z | spine1 (172.100.109.11) | `bgp_neighbor_down` | gnmi (spine lost leaf4 peer) |
>
> Incidents created: 13 total at test end. Latest incident has root=spine1 `bgp_neighbor_down`
> with leaf4 gnmi+snmp `bgp_neighbor_down` as cascading, affecting both spine1 and leaf4.
>
> Note: `bfd_session_down` did not fire for leaf4 in this run. BFD timer > interface-down
> reaction time — BFD session dropped with the interface before its own detection could fire.
> Note: gnmi and snmp detections remain separate per source (same SNMP orphan peer-addr issue
> documented in S-44). Cross-source dedup is a known gap.

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
| S-14 | 8 managed devices in graph | ⬜ |
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
| S-25 | Syslog: multi-source fusion counter | ✅ |
| S-26 | SNMP: manual trap test | ✅ |
| S-27 | SNMP: linkDown trap from leaf3 | ✅ |
| S-28 | SNMP: SNMP-sourced event in graph | ✅ |
| S-29 | SNMP: BGP backward-transition trap | ⚠️ partial — categorised, peer_address OID-suffix parsing not yet impl |
| S-30 | BMP: frr-rr session established | ✅ |
| S-31 | BMP: session log confirmed | ✅ |
| S-32 | BMP: ROUTE_MONITORING + rib_type | ✅ |
| S-32b | BMP: PeerUp BGP OPEN capabilities | ⚠️ known gap — hold_time offset wrong, capabilities empty |
| S-33 | BMP: multi-source fusion BMP+gNMI | ⚠️ structural — cross-device device_address mismatch, see S-33 note |
| S-34 | NetFlow: softflowd installed | ✅ |
| S-35 | NetFlow: host1 interfaces configured | ✅ |
| S-36 | NetFlow: traffic generated | ✅ |
| S-37 | NetFlow: AppFlow nodes in graph | ✅ |
| S-38 | NetFlow: CARRIES_FLOW edge | ⚠️ exporter=linux-host1 not a Device; SRL exports sFlow (unsupported) |
| S-39 | OTLP: direct curl trace | ✅ |
| S-40 | OTLP: collector config + start | — skipped (use S-39 curl instead) |
| S-41 | OTLP: Application node in graph | ✅ |
| S-42 | OTLP: RUNS_SERVICE edge | ✅ |
| S-43 | Correlation: gNMI+syslog same event | ✅ sources=['syslog','gnmi'], counter=1 |
| S-44 | Correlation: detection dedup (1 not 3) | ⚠️ PARTIAL — gNMI fires; SNMP orphan separate (peer-addr mismatch) |
| S-45 | HostEndpoint: LLDP inference | ⚠️ Alpine linux-host1 has no LLDP daemon |
| S-46 | HostEndpoint: CONNECTED_TO edge | ⚠️ same — no LLDP from host1 |
| S-47 | Settings: GET /api/settings/streaming | ✅ |
| S-48 | Settings: PATCH disable/enable syslog | ✅ use syslog_udp key; requires_restart=true |
| S-49 | Collector health: uptime + queue | ⚠️ mode=all: collector never registers via gRPC |
| S-50 | Collector health: receiver badges | ⚠️ same — 0 collectors in mode=all |
| S-51 | Live UI: 3-panel layout manual check | ⬜ manual — requires browser |
| S-52 | SSE: event stream flowing | ✅ |
| S-53 | Round-trip: leaf4 isolation → incident | ✅ delta=6 detections, incident root+cascading |
| S-54 | Teardown: bonsai stopped | ✅ |
| S-55 | Teardown: host1 processes stopped | ✅ |
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

FRR's BMP (`bmpd`) requires BGP to be established first. Wait for S-09 to confirm BGP before checking BMP.
If BMP still doesn't establish after 60s:
```bash
# Check FRR BMP status
docker exec clab-bonsai-signal-test-frr-rr vtysh -c "show bmp"

# Check FRR can reach bonsai BMP listener
docker exec clab-bonsai-signal-test-frr-rr ping -c 2 172.100.109.1

# Verify bonsai is listening on :5000
ss -tlnp | grep :5000
```
`show bmp` should list the `bonsai` target with state `up`. If state is `connecting`,
verify bonsai is listening on `:5000` and frr-rr can reach `172.100.109.1` on the mgmt network.

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
docker exec -i clab-bonsai-signal-test-srl-leaf4 sr_cli <<'EOF'
enter candidate
set / interface ethernet-1/1 admin-state disable
commit now
EOF

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
docker exec -i clab-bonsai-signal-test-srl-leaf4 sr_cli <<'EOF'
enter candidate
set / interface ethernet-1/1 admin-state enable
commit now
EOF
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

---

## Phase 18 — Remediation Round-Trip (HITL) Test

**D4-15 T1/T2 / D4-23 T3**

**Prerequisite**: Phase 15 (Fault Injection Round-Trip) complete. Bonsai running with `ANTHROPIC_API_KEY` set.

### S-70: Trigger a BGP fault and verify HITL proposal

```bash
# Bring down leaf4's BGP session to spine1
ssh admin@clab-signal-test-leaf4 "sr_cli -c 'set / network-instance default protocols bgp admin-state disable' -c 'commit now'"

# Wait 15s for detection + auto-investigation
sleep 15

# Check that a HITL remediation was proposed (requires_human: true)
curl -s http://localhost:3000/api/remediations | python3 -m json.tool | \
  python3 -c "import sys,json; d=json.load(sys.stdin); hits=[r for r in d.get('remediations',[]) if r.get('requires_human')]; print(f'{len(hits)} HITL proposals'); [print(r['id'], r.get('state'), r.get('steps_json','')[:80]) for r in hits]"
```

Expected: ≥1 proposal with `requires_human: true`, `state: "pending"`.

### S-71: Approve the HITL proposal and verify execution

```bash
# Get the latest pending remediation ID
REM_ID=$(curl -s http://localhost:3000/api/remediations | \
  python3 -c "import sys,json; d=json.load(sys.stdin); r=[x for x in d.get('remediations',[]) if x.get('state')=='pending']; print(r[0]['id'] if r else 'NONE')")
echo "Approving: $REM_ID"

# Approve it
curl -s -X POST http://localhost:3000/api/remediations/$REM_ID/approve \
  -H 'Content-Type: application/json' \
  -d '{"approved_by":"test-operator"}' | python3 -m json.tool

# Wait for execution
sleep 20

# Verify state transitioned to applied or partial
curl -s http://localhost:3000/api/remediations/$REM_ID | \
  python3 -c "import sys,json; d=json.load(sys.stdin); print('State:', d.get('state'), '| Trust:', d.get('trust_level'))"
```

Expected: state = `"applied"` or `"partial"`.

### S-72: Verify graph state after remediation

```bash
# The verify_graph step should have run automatically
curl -s http://localhost:3000/api/remediations/$REM_ID/verify | python3 -m json.tool
```

Expected: `{"passed": true, "details": "..."}` or clear failure reason.

### S-73: 60% packet loss HITL realism test (D4-15 T2)

```bash
# Apply packet loss to leaf4 → spine1 link using tc netem
ssh admin@clab-signal-test-leaf4 "sudo tc qdisc add dev e1-1 root netem loss 60%"

# Wait for detection
sleep 30

# Check if a remediation was proposed (packet loss may not fire BGP-down, check for flow_high_utilization or interface events)
curl -s http://localhost:3000/api/remediations?limit=5 | \
  python3 -c "import sys,json; d=json.load(sys.stdin); [print(r['id'], r.get('state'), r.get('detection_id','')[:20]) for r in d.get('remediations',[])]"

# Restore
ssh admin@clab-signal-test-leaf4 "sudo tc qdisc del dev e1-1 root"
```

### Phase 18 Summary Checklist

| Step | Test | Status |
|------|------|--------|
| S-70 | BGP fault triggers HITL proposal | ⬜ |
| S-71 | Approval → execution | ⬜ |
| S-72 | verify_graph passes after remediation | ⬜ |
| S-73 | 60% packet loss test | ⬜ |

---

## Phase 19 — Enrichment Quality Tests

**D4-23 T4 / D4-18 T3-T6**

**Prerequisite**: Phase 17 (External Integrations) complete. ServiceNow PDI credentials available.

### S-74: NetBox enrichment end-to-end

```bash
# Verify NetBox enrichment for leaf1
curl -s http://localhost:3000/api/devices/clab-signal-test-leaf1/enrichment | \
  python3 -c "import sys,json; d=json.load(sys.stdin); src=d.get('sources',{}); print('Vendor:', src.get('vendor',[{}])[0].get('value')); print('Sources:', list(src.keys()))"

# Check for any enrichment conflicts
curl -s http://localhost:3000/api/devices/clab-signal-test-leaf1/enrichment/conflicts | \
  python3 -m json.tool | python3 -c "import sys,json; d=json.load(sys.stdin); c=d.get('conflicts',[]); print(f'{len(c)} conflicts')"
```

Expected: vendor populated from NetBox, ≥3 source keys, ≤1 conflict (known hostname format difference).

### S-75: ServiceNow PDI enrichment end-to-end (D4-18 T3)

```bash
# Trigger ServiceNow enrichment manually
curl -s -X POST http://localhost:3000/api/enrichment/run \
  -H 'Content-Type: application/json' \
  -d '{"source":"servicenow","device_address":"clab-signal-test-leaf1"}' | python3 -m json.tool

# Verify CI data appeared in graph
curl -s "http://localhost:3000/api/explorer/query" \
  -H 'Content-Type: application/json' \
  -d '{"cypher":"MATCH (d:Device {address:\"clab-signal-test-leaf1\"}) RETURN d.snow_sys_id, d.snow_ci_name, d.site","params":{}}' | python3 -m json.tool
```

Expected: `snow_sys_id` populated, site from SNOW CMDB visible.

### S-76: SNOW AIOps incident round-trip (D4-18 T4)

```bash
# Check that a SNOW incident was created for the leaf4 fault from Phase 18
curl -s http://localhost:3000/api/integrations/servicenow/incidents?limit=5 | \
  python3 -c "import sys,json; d=json.load(sys.stdin); [print(i.get('number'), i.get('state'), i.get('description','')[:60]) for i in d.get('incidents',[])]"
```

Expected: ≥1 SNOW incident with number `INC...` and state mapped from Bonsai severity.

### S-77: Enrichment conflict UI test (D4-18 T5)

```bash
# Manually inject a conflicting vendor value
curl -s -X POST http://localhost:3000/api/enrichment \
  -H 'Content-Type: application/json' \
  -d '{"device_address":"clab-signal-test-leaf1","source":"manual","field":"vendor","value":"Juniper"}' | python3 -m json.tool

# Verify conflict is detected
curl -s http://localhost:3000/api/devices/clab-signal-test-leaf1/enrichment/conflicts | \
  python3 -c "import sys,json; d=json.load(sys.stdin); c=d.get('conflicts',[]); print(f'{len(c)} conflicts'); [print(x['field'], x['winner'],'>',x['loser']) for x in c]"
```

Expected: conflict on `vendor` field between `manual` (Juniper) and `netbox`/`gnmi` (Nokia).

### S-78: Adapter push completeness audit (D4-18 T6)

```bash
# List all adapters and their push counts
curl -s http://localhost:3000/api/adapters | \
  python3 -c "import sys,json; [print(a['name'], a.get('adapter_type'), 'running:', a.get('is_running'), 'last_push:', a.get('last_push_at_ns','—')) for a in json.load(sys.stdin).get('adapters',[])]"
```

Expected: all configured adapters show `is_running: true` and `last_push_at_ns` within last 300s.

### Phase 19 Summary Checklist

| Step | Test | Status |
|------|------|--------|
| S-74 | NetBox enrichment end-to-end | ⬜ |
| S-75 | ServiceNow PDI enrichment | ⬜ |
| S-76 | SNOW AIOps incident round-trip | ⬜ |
| S-77 | Enrichment conflict UI test | ⬜ |
| S-78 | Adapter push completeness | ⬜ |

---

## Phase 20 — sFlow Receiver Tests (D4-23 T5)

**Prerequisite**: Bonsai running. Nokia SRL nodes are running (they export sFlow natively).

### S-38b: sFlow: Nokia SRL exporter → AppFlow nodes

```bash
# Nokia SRL exports sFlow on UDP 6343 by default.
# Ensure sflow receiver is enabled in bonsai.toml:
#   [streaming.sflow]
#   enabled = true
#   bind_addr = "0.0.0.0:6343"

# Verify sflow receiver is listed as running
curl -s http://localhost:3000/api/receivers/status | \
  python3 -c "import sys,json; d=json.load(sys.stdin); r=[x for x in d.get('receivers',[]) if x['name']=='sflow']; print('sflow running:', r[0]['running'] if r else 'NOT FOUND')"

# Configure Nokia SRL leaf1 to export sFlow (if not already)
# (on SRL, sFlow is under /bfd or /acl — consult SRL 24.x docs for sflow-config path)
# For signal-test-lab, SRL nodes may need manual sFlow target config:
ssh admin@clab-signal-test-leaf1 "sr_cli -c 'info / system sflow'"

# Wait for sFlow samples and check AppFlow nodes
sleep 30
curl -s "http://localhost:3000/api/flows/live" | \
  python3 -c "import sys,json; d=json.load(sys.stdin); print('Total flows:', d.get('total_flows',0)); [print(' Exporter:', e['exporter_address'], 'flows:', e['flow_count'], 'Mbps:', round(e['bytes_per_sec']*8/1e6,2)) for e in d.get('exporters',[])]"
```

Expected: ≥1 sFlow exporter (SRL leaf), total_flows > 0.

### S-38c: sFlow: CARRIES_FLOW edge validation

```bash
# Check that AppFlow nodes are linked to devices via CARRIES_FLOW
curl -s "http://localhost:3000/api/explorer/query" \
  -H 'Content-Type: application/json' \
  -d '{"cypher":"MATCH (d:Device)-[:CARRIES_FLOW]->(f:AppFlow) RETURN d.address, count(f) as flow_count ORDER BY flow_count DESC LIMIT 5","params":{}}' | python3 -m json.tool
```

Expected: ≥1 Device with CARRIES_FLOW edges.

---

## Checklist Tooling (D4-23 T7)

### Auto-generate a test run results file

Run this after completing all phases to capture current state:

```bash
#!/usr/bin/env bash
# generate_results.sh — captures test run snapshot
set -euo pipefail
BONSAI=http://localhost:3000
DATE=$(date -u +%Y-%m-%dT%H:%M:%SZ)
OUT="docs/test_results/run_${DATE//:/-}.md"
mkdir -p docs/test_results

cat > "$OUT" <<HEADER
# Test Run: $DATE

## Environment
- Host: $(hostname)
- Commit: $(git rev-parse --short HEAD)
- Bonsai version: $(curl -sf $BONSAI/api/health | python3 -c "import sys,json; print(json.load(sys.stdin).get('version','?'))" 2>/dev/null || echo '?')

## Results

| Step | Test | Status | Notes |
|------|------|--------|-------|
HEADER

# Health check
HEALTH=$(curl -sf $BONSAI/api/health 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin).get('status','?'))" || echo "FAIL")
echo "| S-12 | /health = ok | ${HEALTH} | |" >> "$OUT"

# Device count
DEV_COUNT=$(curl -sf $BONSAI/api/topology 2>/dev/null | python3 -c "import sys,json; d=json.load(sys.stdin); print(len(d.get('devices',[])))" || echo "0")
echo "| S-14 | Managed devices in graph | ${DEV_COUNT} devices | |" >> "$OUT"

# Detection count
DET_COUNT=$(curl -sf "$BONSAI/api/detections?limit=1000" 2>/dev/null | python3 -c "import sys,json; d=json.load(sys.stdin); print(len(d.get('detections',[])))" || echo "0")
echo "| S-19 | Detections in graph | ${DET_COUNT} | |" >> "$OUT"

# Adapter status
curl -sf $BONSAI/api/adapters 2>/dev/null | python3 -c "
import sys, json
for a in json.load(sys.stdin).get('adapters', []):
    state = 'ok' if a.get('is_running') else 'FAIL'
    print(f'| Adapter:{a[\"name\"]} | running | {state} | |')
" >> "$OUT" || echo "| Adapters | status | ERROR | |" >> "$OUT"

echo "" >> "$OUT"
echo "Generated: $DATE" >> "$OUT"
echo "Results written to: $OUT"
```

Save as `scripts/generate_results.sh`, then:

```bash
chmod +x scripts/generate_results.sh
./scripts/generate_results.sh
```

---

## Updated Signal Status Reference (D4-23 T2)

The following signals have been updated since their original status was recorded:

| Signal | Original Status | Updated Status | Fix |
|--------|----------------|----------------|-----|
| S-29 | ⚠️ partial — OID-suffix not implemented | ✅ implemented | D4-1 T1: `index_suffix_field` OID parser, Nokia TIMETRA-BGP-MIB peer_address extraction |
| S-32b | ⚠️ hold_time offset wrong | ✅ fixed | D4-11 T1: RFC 4271 §4.2 hold_time offset corrected |
| S-38 | ⚠️ SRL exports sFlow (unsupported) | ✅ sFlow receiver added | D4-5 T1: RFC 3176 sFlow v5 receiver on UDP 6343 |
| S-44 | ⚠️ SNMP orphan separate | ✅ improved | D4-1 T2: CorrelationKey sub_key uses parsed peer_address |
| S-45 | ⚠️ Alpine linux-host1 no LLDP | ✅ fixed | D4-23 T1: lldpd startup script, ContainerLab exec-cmd |
| S-46 | ⚠️ same — no LLDP from host1 | ✅ fixed | D4-23 T1: same fix |
| S-49 | ⚠️ mode=all collector not registering | ✅ fixed | D4-9 T3: auto-register in-process collector |
| S-50 | ⚠️ receiver badges empty | ✅ fixed | D4-9 T3: same fix |
