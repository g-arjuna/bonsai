# Ubuntu DV2 Backlog — Execution Instructions

> Generated 2026-05-17 after Mac-side DV2 sessions 1–5.
> All tasks below require `cargo build` and run on the **Ubuntu ops machine** only.
> Mac-side work is complete. Do `git pull origin main` first on Ubuntu before starting.

---

## Prerequisites

```bash
git pull origin main
bash scripts/ops/rebuild_and_validate.sh   # confirm PASS=16 before touching anything
```

---

## D2-1 T1 — Delete `event_detection.rs` (retire the old detection engine)

**Effort**: 1.5h smoke + 30min deletion + 30min verify
**Why**: `event_detection.rs` (191 lines) is a dead parallel detection path. All detections now flow through the Python sidecar. The file is wired but produces no operational detections. This is the final DV1 carryover.

### Steps

1. Run the full gate smoke (1 hour):
   ```bash
   bash scripts/smoke/smoke_gate_full.sh --duration 3600
   ```
   Expected: all three rule_ids (`bgp_session_down`, `bfd_session_down`, `interface_down`) fire across ≥15 of 20 cycles.

2. Verify `detections_out_total` counter in sidecar heartbeat:
   ```bash
   curl -s http://localhost:3000/api/sidecars | jq '.[].heartbeat.detections_out_total'
   ```
   Expected: non-zero integer.

3. Delete the file and its wiring:
   ```bash
   rm src/event_detection.rs
   ```
   Then in `src/lib.rs` remove:
   ```rust
   pub mod event_detection;
   ```
   Then in `src/server_startup.rs` find and remove:
   ```rust
   bonsai::event_detection::start(...)
   ```

4. Build and validate:
   ```bash
   cargo build --release 2>&1 | tail -20
   bash scripts/ops/rebuild_and_validate.sh
   ```
   Expected: PASS=16 (same as before).

5. Write gate report:
   ```bash
   echo "event_detection.rs retired $(date -u +%Y-%m-%dT%H:%MZ). PASS=16." \
     > docs/test_results/event_detection_retired_$(date +%Y-%m-%d).md
   ```

---

## D2-4 T2 — Emit `config_change_event` from `event_bus.rs`

**Effort**: 2 days
**Why**: `config.py` rules (`ConfigChanged`, `ConfigCausedFault`) are complete Python stubs waiting for this event type. The path profiles already have `config_paths` sections. This wires the Rust side.

### Context
- Path profiles with `config_paths` already exist in `config/path_profiles/` (dc_evpn_leaf, dc_spine_standard, sp_p_core, sp_pe_full).
- The Python rules in `python/bonsai_sdk/rules/config.py` consume `config_change_event` with fields: `device_address`, `yang_path`, `new_value`, `previous_value`, `occurred_at_ns`.
- Architecture spec: `docs/architecture/config_state_lane.md`.

### Steps

1. In `src/event_bus.rs` (or wherever telemetry updates are dispatched), add handling for gNMI notifications on paths matching the `config_paths` section of the device's active path profile.

2. When a gNMI update arrives on a `config_paths` entry:
   - Read the old value from the graph (or last-known cache).
   - Emit a `BonsaiEvent` with:
     ```rust
     BonsaiEvent {
         device_address: update.target.clone(),
         event_type: "config_change_event".to_string(),
         detail_json: serde_json::json!({
             "yang_path": path,
             "new_value": new_val,
             "previous_value": prev_val,
         }).to_string(),
         occurred_at_ns: update.timestamp_ns,
         state_change_event_id: "".to_string(),
     }
     ```

3. Smoke test:
   ```bash
   # On SR Linux lab device, change a BGP neighbor admin-state:
   # ssh admin@srl-leaf1 "configure; set /network-instance default protocols bgp neighbor 10.0.0.1 admin-state disable; commit"
   # Then verify in bonsai Live feed: config_change_event appears with yang_path and new_value.
   curl -s http://localhost:3000/api/events | grep config_change_event | head -3
   ```

4. Verify detection fires:
   ```bash
   curl -s http://localhost:3000/api/detections | jq '.[] | select(.rule_id == "config_changed")'
   ```

---

## D2-4 T4 — `ConfigSnapshot` graph node

**Effort**: 1 day (after D2-4 T2)
**Why**: Stores every config change as a graph node for timeline queries and the MCP Explorer.

### Steps

1. In `src/graph/mod.rs`, add to schema init:
   ```rust
   conn.query(
       "CREATE NODE TABLE IF NOT EXISTS ConfigSnapshot(\
           id             STRING,\
           device_address STRING,\
           yang_path      STRING,\
           new_value      STRING,\
           previous_value STRING,\
           occurred_at    TIMESTAMP,\
           PRIMARY KEY (id))"
   )?;
   ```

2. Add `upsert_config_snapshot(conn, id, device, yang_path, new_val, prev_val, ts)` helper following the `upsert_device` pattern.

3. Call it from the `config_change_event` handler (D2-4 T2).

4. Verify in Graph Explorer:
   ```cypher
   MATCH (c:ConfigSnapshot) RETURN c.device_address, c.yang_path, c.new_value ORDER BY c.occurred_at DESC LIMIT 10
   ```

---

## D2-5 T1 — Rack graph node + `rack_member` edges (NetBox enricher)

**Effort**: 2 days
**Why**: The `rack_isolated` rule (`python/bonsai_sdk/rules/rack.py`) calls `client.devices_in_rack()` which returns `[]` until `Rack` nodes exist. This materialises the substrate graph.

### Steps

1. In `src/graph/mod.rs` schema init, add:
   ```rust
   conn.query(
       "CREATE NODE TABLE IF NOT EXISTS Rack(\
           name     STRING,\
           site     STRING,\
           row_id   STRING,\
           metadata STRING,\
           PRIMARY KEY (name))"
   )?;
   conn.query(
       "CREATE REL TABLE IF NOT EXISTS rack_member(FROM Device TO Rack)"
   )?;
   ```

2. In the NetBox enricher (`src/enrichment/`), when processing a NetBox device record that has a `rack` field, call `upsert_rack` and create a `rack_member` edge.

3. Add `/api/devices?rack=<name>` query param support in `src/http_server/device.rs` (the `devices_in_rack()` Python client calls this endpoint).

4. Verify:
   ```bash
   curl -s "http://localhost:3000/api/devices?rack=rack-01" | jq '.devices[].address'
   ```
   And in graph:
   ```cypher
   MATCH (d:Device)-[:rack_member]->(r:Rack) RETURN d.address, r.name LIMIT 20
   ```

5. Run `rack_isolated` smoke: start simulator, cut subscriptions for ≥2 devices in same rack, confirm detection fires within 60s.

---

## D2-5 T3 — PDU SNMP polling

**Effort**: 1.5 days
**Why**: Power feed visibility for rack outage correlation. Without it, `rack_isolated` has no power-domain context.

### Steps

1. Add a PDU SNMP poller in `src/streaming/snmp.rs` (or new `src/streaming/pdu_snmp.rs`).
   - Poll `ENTITY-SENSOR-MIB` + `PDU-MIB` on configured PDU addresses (add `pdu_addresses` array to `bonsai.toml`).
   - For each outlet: emit a `pdu_outlet_state` event with `device_address` (PDU hostname), `outlet_id`, `state` (on/off/fault), `load_va`.

2. Add `PduOutlet` graph node:
   ```rust
   "CREATE NODE TABLE IF NOT EXISTS PduOutlet(\
       id            STRING,\
       pdu_address   STRING,\
       outlet_id     STRING,\
       state         STRING,\
       load_va       DOUBLE,\
       last_polled   TIMESTAMP,\
       PRIMARY KEY (id))"
   ```
   Add `POWERS` edge: `PduOutlet -> Rack`.

3. Detection rule: add `PduFeedLost` to `python/bonsai_sdk/rules/rack.py` — fires when a PDU outlet transitions to `fault` or `off` and a rack is mapped to that outlet.

---

## D2-6 T1–T4 — OTel ingestion + Host node type

**Effort**: 5 days total
**Why**: Brings host-state into the same graph as network state. Prerequisite for `service_path_degraded` (D2-8 T4).

### T1 — LLDP → Host node

Parse LLDP chassis-id / system-name from existing LLDP neighbor data (already in graph) and materialise `HostEndpoint` nodes:

```rust
"CREATE NODE TABLE IF NOT EXISTS HostEndpoint(\
    address   STRING,\
    hostname  STRING,\
    os        STRING,\
    rack      STRING,\
    PRIMARY KEY (address))"
"CREATE REL TABLE IF NOT EXISTS CONNECTED_TO(FROM HostEndpoint TO Device)"
```

In `src/graph/mod.rs` `write_lldp_neighbor`: if `system_capabilities` does not include "bridge"/"router", create a `HostEndpoint` node and `CONNECTED_TO` edge.

### T2 — OTLP receiver

Add `src/streaming/otlp.rs` — HTTP/2 OTLP receiver on port 4318. Accept `ResourceSpans`. Parse `peer.address`, `db.name`, `http.url` span attributes. Emit `otlp_span` events to the event bus.

```bash
# bonsai.toml addition:
[otlp]
enabled = true
port = 4318
```

### T3 — Host reconciliation

In `src/enrichment/`, add a reconciliation pass that matches `HostEndpoint.address` against `TelemetryUpdate.target` and LLDP neighbor entries to deduplicate hosts seen via multiple sources.

### T4 — Host-network correlation rule

Add `python/bonsai_sdk/rules/host.py`:
- `HostNetworkFault`: fires when a `HostEndpoint` loses connectivity (no OTLP spans for >5 min AND the connected Device has an active `interface_down` detection).
- Severity `warn`.

---

## D2-7 T1–T2 — `OpticalChannel` graph node + gNMI/SNMP receiver

**Effort**: 3 days
**Why**: The `optical_rx_degrading` rule (`python/bonsai_sdk/rules/optical.py`) is complete. This wires the data source.

### T1 — OpticalChannel graph node

```rust
"CREATE NODE TABLE IF NOT EXISTS OpticalChannel(\
    id               STRING,\
    device_address   STRING,\
    name             STRING,\
    rx_power_dbm     DOUBLE,\
    tx_power_dbm     DOUBLE,\
    osnr_db          DOUBLE,\
    pre_fec_ber      DOUBLE,\
    laser_bias_ma    DOUBLE,\
    temperature_c    DOUBLE,\
    last_sampled     TIMESTAMP,\
    PRIMARY KEY (id))"
```

Add `upsert_optical_channel(conn, device, name, rx, tx, osnr, ber, bias, temp, ts)`.

### T2 — gNMI subscriber for optical paths

Add `openconfig-platform-optical-channel/state/input-power/instant` and related paths to a new `optical_dwdm.yaml` path profile (or extend existing profiles via `path_profiles/`).

When a gNMI update arrives on an optical path:
- Call `upsert_optical_channel`.
- Emit `optical_channel_state` event with `channels` array in `detail_json`.

### Simulator validation (before hardware)

```bash
# On Ubuntu, inject simulator events:
python3 experiments/optical_simulator/simulate.py \
  --scenario degrade \
  --bonsai-url http://localhost:3000 \
  --ticks 25

# Verify OpticalChannel nodes:
curl -s http://localhost:3000/api/explorer \
  -d '{"cypher":"MATCH (oc:OpticalChannel) RETURN oc.name, oc.rx_power_dbm ORDER BY oc.rx_power_dbm"}' | jq

# Verify detection fires after 20 ticks:
curl -s http://localhost:3000/api/detections | jq '.[] | select(.rule_id == "optical_rx_degrading")'
```

---

## D2-8 T2–T4 — Netflow receiver, AppFlow graph, `service_path_degraded`

**Effort**: 5 days
**Why**: App dependency matrix — "which services are affected by this network fault?"
Decision: Angle A (netflow) first. Full scoping in `docs/research/app_dependency_matrix_2026-05-17.md`.

### T2 — Netflow v9/v10 receiver

Add `src/streaming/netflow.rs`:
- UDP listener on port 2055.
- Parse Netflow v9 and IPFIX (v10) templates + data records.
- Emit `app_flow` events with `src_address`, `dst_address`, `dst_port`, `protocol`, `bytes`, `packets`, `flow_start_ns`, `flow_end_ns`.

```bash
# bonsai.toml addition:
[netflow]
enabled = true
port = 2055
```

Configure router to export:
```
# IOS-XR:
flow exporter BONSAI destination <ubuntu-ip> transport udp 2055 source Loopback0
flow monitor BONSAI record netflow ipv4 original-input exporter BONSAI
interface GigabitEthernet0/0/0/0 flow monitor BONSAI ingress

# Nokia SR Linux:
set /network-instance default ip-flow-export collector-address <ubuntu-ip> collector-port 2055
```

### T3 — AppFlow graph

```rust
"CREATE NODE TABLE IF NOT EXISTS AppFlow(\
    id             STRING,\
    src_address    STRING,\
    dst_address    STRING,\
    dst_port       INT64,\
    protocol       STRING,\
    bytes_per_sec  DOUBLE,\
    packets_per_sec DOUBLE,\
    last_seen      TIMESTAMP,\
    PRIMARY KEY (id))"
```

Upsert on each netflow record (aggregate per 60s).

### T4 — `service_path_degraded` detection rule

Add `python/bonsai_sdk/rules/app.py`:

```python
class ServicePathDegraded(Detector):
    rule_id = "service_path_degraded"
    severity = "warn"
    # Fires when AppFlow bytes_per_sec drops >80% AND the path between
    # src/dst HostEndpoint nodes passes through a device with an active
    # interface_down or bgp_session_down detection.
```

---

## D2-9 T1–T3 — Entity reconciliation

**Effort**: 5 days
**Why**: Multiple ingestion sources (gNMI, LLDP, NetBox, netflow, OTLP) create duplicate or mismatched graph nodes for the same physical entity. Reconciliation is the glue.

### T1 — Entity identity table

Add `EntityIdentity` node — canonical record binding multiple source IDs to one entity:

```rust
"CREATE NODE TABLE IF NOT EXISTS EntityIdentity(\
    canonical_id   STRING,\
    entity_type    STRING,\
    addresses      STRING,\  -- JSON array of known addresses/IDs
    source_ids     STRING,\  -- JSON dict: {source: id}
    PRIMARY KEY (canonical_id))"
```

### T2 — Reconciler service

Add `src/reconciler.rs`:
- On startup, scans `Device`, `HostEndpoint`, `OpticalChannel`, `Rack` for ambiguous IDs.
- Merges duplicate nodes that share ≥2 identifiers (MAC, FQDN, loopback IP).
- Runs on a 60s polling loop.

### T3 — Node refactoring

Update existing `upsert_*` functions to route through the reconciler identity table before inserting, so new nodes are matched against existing canonicals rather than creating duplicates.

---

## D2-12 — GNN training trigger check

**Effort**: 15 min check, then 1 week training (if triggered)
**Trigger conditions** (all must be true):
- Archive depth ≥ 30 calendar days
- ≥ 500 chaos injections total
- ≥ 50 detection examples per active rule_id (bgp, bfd, interface all need this)

**Check**:
```bash
# Archive depth:
curl -s http://localhost:3000/api/operations | jq '.archive_lag_seconds, .archive_rows_buffered'

# Per-rule detection counts:
curl -s http://localhost:3000/api/detections | jq 'group_by(.rule_id) | map({rule: .[0].rule_id, count: length})'
```

**If triggered**: open D2-13 — run first GNN training cycle using `python/bonsai_ml/gnn/` scaffolding from DV1. See `python/bonsai_ml/gnn/archive_to_training.py` for the data loading path. Output: model card at `python/bonsai_ml/model_cards/gnn_v1.md`.

---

## Validation checklist after all Ubuntu tasks

```bash
bash scripts/ops/rebuild_and_validate.sh   # must still be PASS=16+
# Additional checks:
curl -s http://localhost:3000/api/detections | jq 'map(.rule_id) | unique | sort'
# Expected to include: bgp_session_down, bfd_session_down, interface_down,
#   config_changed, config_caused_fault (after chaos + config change),
#   rack_isolated (after rack node population),
#   optical_rx_degrading (after simulator or real hardware)
```

---

## Dependency order

```
D2-1 T1  (independent — run first, clears debt)
D2-4 T2  → D2-4 T4  (config_change_event must exist before ConfigSnapshot node)
D2-5 T1  → D2-5 T3  (Rack nodes before PDU→Rack edge)
D2-6 T1  → D2-6 T2  → D2-6 T3  → D2-6 T4  (LLDP→Host before OTel, reconcile after both)
D2-7 T1  → D2-7 T2  (OpticalChannel node before gNMI subscriber writes to it)
D2-8 T2  → D2-8 T3  → D2-8 T4  (receiver before graph before rule)
D2-9     (run after D2-5/D2-6/D2-7/D2-8 — reconciles all new node types)
D2-12    (check trigger at any time; training starts when triggered)
```

Parallel-safe pairs (no dependency between them):
- D2-1 T1 ∥ D2-4 T2
- D2-5 T1 ∥ D2-6 T1
- D2-7 T1 ∥ D2-8 T2
