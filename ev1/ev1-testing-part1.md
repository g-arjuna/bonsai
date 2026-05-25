# EV1 Ubuntu Testing Guide — Part 1: Infrastructure, Onboarding & Core Signals

> **Sprint**: EV1 (Embedded Vision 1)
> **Ubuntu ops box prerequisites**: Rust 1.95+, cmake, protoc, Docker 24+, ContainerLab, Python 3.12+
> **Build prerequisite**: `git pull && cargo build --release && cd ui && npm ci && npm run build && cd ../ui-bonpy && npm ci && npm run build`

---

## Phase 0 — Pre-Flight Checklist

Before any EV1 testing, complete this checklist. Every item must pass.

```bash
# 1. Pull latest code
cd ~/bonsai
git pull origin main

# 2. Full Rust build
cargo build --release
# Expected: compiles successfully. etcd-client and lbug warnings are OK. Zero errors.

# 3. UI build
cd ui && npm ci && npm run build
cd ../ui-bonpy && npm ci && npm run build
cd ..

# 4. Python ML deps
cd python
pip install -e ".[dev]"
pip install sentence-transformers torch torch-geometric apscheduler pyarrow scikit-learn hdbscan
cd ..

# 5. ContainerLab available
sudo clab version   # expect: >= 0.54

# 6. Docker running
docker ps           # expect: daemon running

# 7. Disk space
df -h               # expect: >= 20 GB free on runtime partition

# 8. Wipe any previous runtime
rm -rf runtime/
```

**EV1-specific Python ML dep check:**
```bash
python3 -c "import torch; import torch_geometric; import sentence_transformers; import apscheduler; import pyarrow; print('ALL OK')"
# Expected: ALL OK
```

**ServiceNow PDI check** (only if testing SNOW integration):
```
- Open https://developer.servicenow.com
- Wake PDI if hibernated (takes ~5 min)
- Note instance URL and credentials
```

---

## Phase 1 — Clean-Slate Bonsai Core Startup

```bash
# Create minimal config
cp bonsai.toml.example bonsai.toml

# Set vault passphrase
export BONSAI_VAULT_PASSPHRASE="test-passphrase-ev1-2026"

# Start in mode=all (core + collector + local sidecar in one process)
./target/release/bonsai --config bonsai.toml &
BONSAI_PID=$!

# Wait for health
sleep 5
curl -s http://localhost:3000/health | python3 -m json.tool
```

**Expected health response fields:**
```json
{
  "status": "ok",
  "version": "...",
  "mode": "all",
  "graph_node_count": 0,
  "sidecar_registered": false
}
```

**UI check:**
```
open http://localhost:3000
```
- [ ] Onboarding wizard appears on first boot
- [ ] No JS console errors
- [ ] BonPy console accessible at `http://localhost:3000/bonpy/`
- [ ] BonPy nav shows: Dashboard, Jobs, Models, Exports, GNN, Embeddings, Rules, Detections

---

## Phase 2 — Device Onboarding (Two Methods)

### Method A — PyATS Bootstrap (Automated)

Requires ContainerLab SRL lab running. Start it:

```bash
# Start 3-node SRL fast-iteration lab
cd lab/fast-iteration
sudo clab deploy -t 3node-srl.clab.yml
cd ../..

# Verify lab nodes are reachable
ping -c1 172.20.20.2   # srl1
ping -c1 172.20.20.3   # srl2
ping -c1 172.20.20.4   # srl3
```

Add device credential first:
```bash
curl -s -X POST http://localhost:3000/api/credentials \
  -H 'Content-Type: application/json' \
  -d '{"alias":"srl-lab","username":"admin","password":"NokiaSrl1!","type":"gnmi"}' | python3 -m json.tool
```

Bootstrap a device:
```bash
curl -s -X POST http://localhost:3000/api/devices/bootstrap \
  -H 'Content-Type: application/json' \
  -d '{
    "address": "172.20.20.2",
    "credential_alias": "srl-lab",
    "vendor": "nokia_srlinux",
    "profile": "dc_leaf"
  }' | python3 -m json.tool
```

**Expected response fields:**
```json
{
  "status": "seeded",
  "device_address": "172.20.20.2",
  "interfaces_seeded": 8,
  "bgp_neighbors_seeded": 2,
  "lldp_neighbors_seeded": 2,
  "ospf_neighbors_seeded": 0,
  "bfd_sessions_seeded": 0,
  "platform_detail": {"model": "7220 IXR-D2", ...}
}
```

**Verify in graph (Explorer → Cypher):**
```cypher
MATCH (d:Device {address: "172.20.20.2"})
RETURN d.address, d.hostname, d.vendor, d.model, d.cpu_util_pct, d.memory_used_mb
```
- [ ] Device node exists with model/CPU/memory populated
- [ ] `needs_config_embedding = true` on device node

```cypher
MATCH (d:Device {address: "172.20.20.2"})-[:HAS_INTERFACE]->(i:Interface)
RETURN i.name, i.if_index, i.is_in_lag
```
- [ ] Interfaces seeded (≥4 expected for SRL dc_leaf)

**Bulk bootstrap** (all 3 lab nodes):
```bash
curl -s -X POST http://localhost:3000/api/devices/bootstrap/bulk \
  -H 'Content-Type: application/json' \
  -d '{
    "devices": [
      {"address":"172.20.20.2","credential_alias":"srl-lab","vendor":"nokia_srlinux","profile":"dc_leaf"},
      {"address":"172.20.20.3","credential_alias":"srl-lab","vendor":"nokia_srlinux","profile":"dc_leaf"},
      {"address":"172.20.20.4","credential_alias":"srl-lab","vendor":"nokia_srlinux","profile":"dc_spine"}
    ],
    "parallel": 3
  }' | python3 -m json.tool
```
- [ ] All 3 devices seeded
- [ ] `seeded_count: 3` in response

### Method B — Manual Onboarding via UI

For environments without PyATS access:

1. Open `http://localhost:3000`
2. Navigate to **Devices** → **Add Device**
3. Fill form:
   - Address: `192.168.1.1`
   - Vendor: `Nokia SR Linux`
   - Credential: select `srl-lab` from vault
   - Profile: `dc_leaf`
4. Click **Add Device**

**Verify:**
```bash
curl -s http://localhost:3000/api/onboarding/devices | python3 -m json.tool
```
- [ ] Device appears in list with `status: "managed"`

**Manual seed via API** (if PyATS unavailable, seed known topology):
```bash
curl -s -X POST http://localhost:3000/api/devices/seed \
  -H 'Content-Type: application/json' \
  -d '{
    "address": "192.168.1.1",
    "hostname": "spine-1",
    "vendor": "nokia_srlinux",
    "interfaces": [
      {"name": "ethernet-1/1", "ip": "10.0.0.1", "is_up": true},
      {"name": "ethernet-1/2", "ip": "10.0.0.3", "is_up": true}
    ],
    "bgp_neighbors": [
      {"peer_address": "10.0.0.2", "local_as": 65001, "remote_as": 65002, "state": "established"}
    ],
    "lldp_neighbors": [
      {"local_interface": "ethernet-1/1", "neighbor_chassis_id": "aa:bb:cc:dd:ee:ff", "neighbor_hostname": "leaf-1"}
    ]
  }' | python3 -m json.tool
```
- [ ] `status: "seeded"` in response
- [ ] Device queryable in Explorer

---

## Phase 3 — gNMI Telemetry Flow

Start gNMI subscription to the lab device:
```bash
# Verify gNMI readiness
curl -s "http://localhost:3000/api/devices/172.20.20.2/gnmi-readiness" | python3 -m json.tool
# Expected: {"reachable": true, "gnmi_available": true}
```

In **Settings → Streaming**, confirm gNMI subscription is active. Then inject some traffic on the SRL nodes to generate interface counter updates.

**Verify interface counters flowing:**
```cypher
MATCH (d:Device {address:"172.20.20.2"})-[:HAS_INTERFACE]->(i:Interface)
WHERE i.in_octets IS NOT NULL
RETURN i.name, i.in_octets, i.out_octets, i.in_errors, i.out_errors
```
- [ ] At least one interface has non-null `in_octets`
- [ ] Counters updating every 30s (default SAMPLE interval)

**Verify BGP state flowing:**
```cypher
MATCH (d:Device {address:"172.20.20.2"})-[:HAS_BGP_NEIGHBOR]->(b:BgpNeighbor)
RETURN b.peer_address, b.state, b.adj_rib_in_routes, b.hold_time
```
- [ ] BGP neighbors exist with state populated

---

## Phase 4 — Syslog Reception

Configure SRL to send syslog to bonsai (port 5514 UDP):
```bash
# On SRL node via SSH
ssh admin@172.20.20.2
# In SRL CLI:
# /system logging remote 172.20.20.1 port 5514 facility local7
```

Or send test syslog directly:
```bash
echo '<14>May 25 10:00:00 172.20.20.2 bgpd: %BGP-5-ADJCHANGE: neighbor 10.0.0.2 Up' | \
  nc -u -w1 localhost 5514
```

**Verify syslog received:**
```bash
curl -s "http://localhost:3000/api/events/history?type=syslog&limit=5" | python3 -m json.tool
```
- [ ] Syslog events appear with `fact_type` populated
- [ ] Device address mapped correctly
- [ ] Events with `needs_embedding = true` field

```cypher
MATCH (e:StateChangeEvent {source: "syslog"})
WHERE e.needs_embedding = true
RETURN e.device_address, e.message, e.occurred_at
LIMIT 5
```
- [ ] Events tagged `needs_embedding = true` appear — these feed the syslog embedding worker

---

## Phase 5 — SNMP Trap Reception

Send test SNMP v2c trap:
```bash
# Send a BGP peer-down trap (OID 1.3.6.1.2.1.15.7)
snmptrap -v 2c -c public localhost:9162 "" 1.3.6.1.2.1.15.7

# Or via Python
python3 -c "
from pysnmp.hlapi import *
g = sendNotification(
    SnmpEngine(),
    CommunityData('public'),
    UdpTransportTarget(('localhost', 9162)),
    ContextData(),
    'trap',
    NotificationType(ObjectIdentity('1.3.6.1.2.1.15.7'))
)
for errorIndication, errorStatus, errorIndex, varBinds in g:
    print('Sent')
"
```

**Verify:**
```bash
curl -s "http://localhost:3000/api/events/history?type=snmp&limit=5" | python3 -m json.tool
```
- [ ] SNMP trap event present with `fact_type`

---

## Phase 6 — Multi-Source Correlation

Inject a BGP session down event via both syslog and SNMP within the 45-second correlation window:

```bash
# Syslog: BGP down
echo '<11>May 25 10:05:00 172.20.20.2 bgpd: %BGP-3-NOTIFICATION: neighbor 10.0.0.2 Down' | \
  nc -u -w1 localhost 5514

sleep 5

# Trigger a gNMI state change by admin-disabling interface on SRL
# (or send via SNMP trap)
```

**Verify single correlated detection:**
```bash
curl -s "http://localhost:3000/api/detections?limit=10" | python3 -m json.tool
```
- [ ] Detection appears with `source_event_ids` containing IDs from multiple sources
- [ ] `correlation_fused: true` on the detection (or single detection from multiple sources, not duplicates)

**Verify correlation buffer metric:**
```bash
curl -s http://localhost:3000/health | python3 -m json.tool | grep correlation
```
- [ ] `bonsai_correlation_multi_source_total` counter incrementing

---

## Phase 7 — Detection Firing Baseline

Inject a BGP all-peers-down scenario by flapping the BGP neighbor:

```bash
# Simulate BGP all-peers-down detection via syslog storm
for i in 1 2 3; do
  echo "<11>May 25 10:10:0${i} 172.20.20.2 bgpd: %BGP-3-NOTIFICATION: neighbor 10.0.0.2 Down" | \
    nc -u -w1 localhost 5514
  sleep 1
done
```

**Verify detection in UI and API:**
```bash
curl -s "http://localhost:3000/api/detections?limit=5" | python3 -m json.tool
```
- [ ] `bgp_session_down` or similar rule_id
- [ ] Severity: `high` or `critical`
- [ ] `reason` string is human-readable

**Verify blast radius:**
```bash
DEVICE="172.20.20.2"
curl -s "http://localhost:3000/api/blast-radius/${DEVICE}" | python3 -m json.tool
```
- [ ] Response includes `affected_interfaces`, `bgp_sessions`, `bfd_sessions`

---

## Phase 8 — Remediation Proposal Flow

```bash
# Check if a remediation proposal was auto-created
curl -s "http://localhost:3000/api/approvals?limit=5" | python3 -m json.tool
```
- [ ] Proposal appears with playbook steps
- [ ] Trust state = `suggest_only` initially

**Approve a proposal:**
```bash
PROPOSAL_ID="<id-from-above>"
curl -s -X POST "http://localhost:3000/api/approvals/${PROPOSAL_ID}/approve" | python3 -m json.tool
```
- [ ] Status changes to `approved` → `executing` → `executed`

**Verify via graph:**
```cypher
MATCH (r:RemediationProposal {id: "<proposal-id>"})
RETURN r.status, r.outcome, r.executed_at_ns
```
- [ ] `outcome: "success"` after execution

---

*Part 1 complete. Continue with Part 2 for ML pipeline testing.*
