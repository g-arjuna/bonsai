# Config-State Lane Architecture

> D2-4 — 2026-05-17. Describes the design for ingesting configuration changes
> as first-class signals, storing them as graph nodes, and correlating them with
> operational faults.

## Why this matters

bonsai today is blind to **why** a fault happened. An operator admin-disables a
BGP session and the system fires `bgp_session_down` — but it doesn't know that
the operator caused it. Distinguishing operator-caused faults from network-caused
faults requires treating config changes as signals on the same plane as
operational telemetry.

The Config-State Lane closes that gap. When complete:
- Every config change on a managed device produces a `config_changed` detection.
- If an operational fault follows within 60 seconds on the same device, a
  `config_caused_fault` detection fires and references the config change.
- The incident view shows "config change occurred 23s before this incident on
  `spine-01`" as a correlation hint.

## Design

```
gNMI ON_CHANGE (config paths)
         │
         ▼
 subscriber.rs — config subscription set
   (separate from state_paths subscription)
         │
         ▼
 event_bus: config_change_event
   { device_address, yang_path, new_value, previous_value, occurred_at_ns }
         │
   ┌─────┴──────────────────────────┐
   ▼                                ▼
graph write:                  rules sidecar:
ConfigSnapshot node           ConfigChanged (info)
linked to Device              ConfigCausedFault (high)
```

## Components

### 1. Path profiles — `config_paths` section (DONE: D2-4 T1)

Each path profile YAML now has an optional `config_paths:` section alongside
`paths:`. The subscriber reads this list and opens a **separate gNMI
subscription** for each entry, all in `ON_CHANGE` mode.

Profiles with `config_paths`:
- `sp_pe_full.yaml` — BGP neighbor admin-state, interface admin-state, routing-policy, AS-number
- `sp_p_core.yaml` — interface admin-state, IS-IS instance admin-state
- `dc_spine_standard.yaml` — interface and BGP neighbor admin-state
- `dc_evpn_leaf.yaml` — interface, BGP neighbor, VxLAN VNI

### 2. Event type — `config_change_event` (PENDING: D2-4 T2, Ubuntu)

**New event type** emitted by `src/event_bus.rs` when a config-state path
produces an ON_CHANGE update. Fields:

```rust
pub struct ConfigChangeEvent {
    pub device_address: String,
    pub yang_path: String,
    pub new_value: serde_json::Value,
    pub previous_value: Option<serde_json::Value>,
    pub occurred_at_ns: u64,
}
```

The `previous_value` field requires the subscriber to retain the last-seen value
per (device, path) key. On first subscription the value is recorded but no event
is emitted (cold-start baseline).

**Where**: `src/event_bus.rs` (new variant), `src/subscriber.rs` (emit on
config path update).

### 3. Graph node — `ConfigSnapshot` (PENDING: D2-4 T4, Ubuntu)

Every `config_change_event` creates a `ConfigSnapshot` node in LadybugDB:

```cypher
CREATE (cs:ConfigSnapshot {
  id: $uuid,
  device_address: $device_address,
  yang_path: $yang_path,
  new_value: $new_value,
  previous_value: $previous_value,
  occurred_at_ns: $occurred_at_ns
})
CREATE (cs)-[:CONFIG_CHANGE_ON]->(d:Device {address: $device_address})
```

Queryable from the MCP Explorer: `MATCH (cs:ConfigSnapshot {device_address:
'spine-01.dc1'}) RETURN cs.yang_path, cs.new_value ORDER BY cs.occurred_at_ns
DESC LIMIT 20`.

**Where**: `src/graph/config_snapshot.rs` (new), called from the
`config_change_event` handler in `write_coordinator.rs`.

### 4. Detection rules — `config.py` (DONE: D2-4 T3)

`python/bonsai_sdk/rules/config.py` contains two rules:

**`ConfigChanged`** (severity: `info`)
- Fires on every `config_change_event`
- Creates an audit trail detection for every config change
- Records the event in an in-process window for `ConfigCausedFault` correlation
- Enabled as soon as T2 (event type) lands

**`ConfigCausedFault`** (severity: `high`)
- Fires on operational events (`bgp_session_change`, `interface_oper_status_change`, etc.)
- Checks `_recent_config_changes` for the same device within the prior 60 seconds
- If a config change is found, fires with a reference to the config yang_path and
  the lag in milliseconds
- Enabled as soon as T2 lands (no additional Rust work required)

### 5. UI surfacing (PENDING: D2-4 T5)

- Live event feed: `config_changed` detections appear in the Events SSE stream
  with a distinct colour (blue/teal for "operator action").
- Incidents view: when a `config_caused_fault` detection groups into an incident,
  show "⚠ Config change on spine-01 preceded this by 23ms" in the incident header.
- Explorer: pre-built query "Recent config changes on this device" available from
  the device detail panel.

## Activation sequence

1. **D2-4 T1** (Mac, DONE): `config_paths` sections in path profiles.
2. **D2-4 T2** (Ubuntu): `config_change_event` variant in `event_bus.rs`;
   `src/subscriber.rs` reads `config_paths` and opens a separate subscription;
   emits event on ON_CHANGE. Estimated 2 days.
3. **D2-4 T3** (Mac, DONE): `config.py` rules — activated automatically when
   the event type lands.
4. **D2-4 T4** (Ubuntu): `ConfigSnapshot` graph node + `config_snapshot.rs`.
   Estimated 2 days.
5. **D2-4 T5** (Mac, post-T4): Svelte UI changes.

## Known limitations

- `previous_value` requires in-process state in the subscriber. On restart, the
  first config update will have `previous_value: null`. This is acceptable —
  the change is still recorded; only the diff is missing.
- The 60-second correlation window is hard-coded in `config.py`. If a config
  change and fault are separated by more than 60s (e.g., a delayed effect), the
  correlation won't fire. The window is tunable via a future `correlation_window_secs`
  field in the path profile.
- Config-path subscriptions are optional. If a vendor doesn't support the
  configured path (advertised model set doesn't include it and `optional: true`),
  the subscription is silently skipped. The `config_paths` entries use the same
  model gating as `paths`.

## Testing (Ubuntu)

```bash
# 1. admin-disable a BGP session on a lab device
# 2. Observe config_change_event in the event stream (/api/events SSE)
# 3. Observe config_changed detection at /api/detections
# 4. Observe bgp_session_down detection within 60s
# 5. Observe config_caused_fault detection referencing the config change
# 6. Confirm ConfigSnapshot node in graph: run MCP query
#    MATCH (cs:ConfigSnapshot) RETURN cs.device_address, cs.yang_path, cs.occurred_at_ns
```
