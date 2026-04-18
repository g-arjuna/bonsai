# Phase 4 — Rules-Based Detect and Heal

**Goal**: Deterministic anomaly detection and closed-loop auto-remediation.
A failure injected in ContainerLab is detected by a rule, a remediation fires, and
the graph shows recovery — all without manual intervention. Every detection and
remediation is written into the graph as labelled training data for Phase 5 ML.

---

## Progress Snapshot (Phases 1–3 Complete)

| Layer | Status | Notes |
|---|---|---|
| gNMI subscriber pool | ✅ | SRL + XRd + cEOS. ON_CHANGE + SAMPLE. Reconnects on drop. |
| Vendor detection | ✅ | Capabilities-driven, native-first, no static per-vendor branching. |
| Graph writer | ✅ | Device, Interface, BgpNeighbor, LldpNeighbor, StateChangeEvent, CONNECTED_TO. |
| BGP state transitions | ✅ | StateChangeEvent + BonsaiEvent broadcast on every session-state change. |
| Topology | ✅ | CONNECTED_TO edges from LLDP across all three vendors. |
| gRPC API | ✅ | Query, GetDevices, GetInterfaces, GetBgpNeighbors, GetTopology, StreamEvents. |
| Python SDK | ✅ | BonsaiClient with typed methods. End-to-end validated. |
| OSPF/IS-IS adjacency telemetry | ⏸ | Phase 4 work when OSPF lab is up. |
| Temporal queries | ⏸ | Schema designed, writes deferred. |
| gNMI Set / remediation | ⏸ | Phase 4 work. |

---

## Lab for Phase 4

**Use `lab/fast-iteration/bonsai-phase4.clab.yml`** (new, replaces multivendor.clab.yml for Phase 4).

### Topology

```
        [srl-spine1]  AS 65001   lo: 10.255.0.1
         /    |    \
        /     |     \
[srl-leaf1]   |  [srl-leaf2]
AS 65011      |   AS 65012
 lo:10.255.0.2 |   lo:10.255.0.3
        \     |     /
         \    |    /
          [xrd-pe1]  AS 65100   lo: 10.255.0.4
```

| Node | Role | RAM | BGP sessions |
|---|---|---|---|
| srl-spine1 | DC spine | ~1.5 GB | 3 (leaf1, leaf2, xrd-pe1) |
| srl-leaf1 | DC leaf | ~1.5 GB | 2 (spine1, xrd-pe1) |
| srl-leaf2 | DC leaf | ~1.5 GB | 2 (spine1, xrd-pe1) |
| xrd-pe1 | SP PE edge | ~2 GB | 3 (spine1, leaf1, leaf2) |
| **Total** | | **~6.5 GB** | 10 sessions |

**Protocols**:
- OSPF area 0 on all data-plane links (underlay) → future OSPF adjacency rules
- eBGP overlay (one session per link) → BGP rules
- SR-MPLS prefix-SIDs on SRL loopbacks (SID = 16000 + node index) → future SR rules
- LLDP on all nodes → topology edges

### Fault injection cheatsheet

```bash
# Add 100ms delay + 5% loss on spine→leaf1 link
clab tools netem set bonsai-p4 srl-spine1 e1-1 --delay 100ms --loss 5

# Clear impairment
clab tools netem set bonsai-p4 srl-spine1 e1-1 --delay 0ms

# Shut an interface (triggers interface-down rule + BGP session-down on that link)
ssh admin@172.100.102.11 'sr_cli "set / interface ethernet-1/1 admin-state disable"'

# Re-enable
ssh admin@172.100.102.11 'sr_cli "set / interface ethernet-1/1 admin-state enable"'

# Clear a BGP session manually (test the remediation action)
# XRd: ssh cisco@172.100.102.21 'clear bgp ipv4 unicast 10.0.14.0 soft'
```

---

## ML-Ready Detection Architecture

**Core principle**: rules and ML models are both *detectors*. The abstraction must
not change when we swap from rules to ML — only the `detect()` implementation changes.
Feature extraction is shared and writes to the graph from day one as training data.

```
gRPC StreamEvents
       │
       ▼
 FeatureExtractor          ← queries graph for context (recent history, peer count, etc.)
       │
       ▼ Features (typed dict, same shape for rules and ML)
       │
  ┌────┴────────────────────────┐
  │                             │
RuleDetector              MLDetector (Phase 5)
(threshold checks)        (model.predict(feature_vector))
  │                             │
  └────────────┬────────────────┘
               │ Optional[Detection]
               ▼
       DetectionWriter        ← writes DetectionEvent to graph
               │
               ▼
       RemediationExecutor    ← selects playbook, calls PushRemediation RPC
               │
               ▼
       RemediationWriter      ← writes Remediation node to graph
```

### `Features` object

```python
@dataclass
class Features:
    # From the triggering event
    device_address: str
    event_type: str
    detail: dict

    # From graph context (queried at detection time)
    peer_count_total: int        # total BGP sessions on device
    peer_count_established: int  # currently established
    recent_flap_count: int       # state changes for this peer in last 5 min
    uptime_seconds: int          # time since session last established

    # Raw timestamp (for training data labelling)
    occurred_at_ns: int
```

This dict is written to `DetectionEvent.detail_json` verbatim. Phase 5 reads it
back and builds a feature matrix with no re-extraction — the training data is
already there in the graph.

### Base classes

```python
class Detector(ABC):
    rule_id: str
    severity: str   # "info" | "warn" | "critical"

    @abstractmethod
    def extract_features(self, event: StateEvent, client: BonsaiClient) -> Optional[Features]:
        """Return None to skip this event entirely (fast path)."""

    @abstractmethod
    def detect(self, features: Features) -> Optional[str]:
        """Return a human-readable reason string if the rule fires, else None."""
```

A `RuleDetector` implements both methods with threshold logic.
A future `MLDetector` keeps the same `extract_features()` and overrides `detect()`
with `model.predict(features.to_vector()) > threshold`.

**No refactor needed when adding ML** — the `RuleEngine` loop doesn't change,
the graph schema doesn't change, only the `detect()` body is swapped.

---

## Phase 4 Task List

### 1 — Graph schema: DetectionEvent + Remediation

New node types in `src/graph.rs`:

```
DetectionEvent(id, device_address, rule_id, severity, features_json, fired_at)
Remediation(id, detection_id, action, status, detail_json, attempted_at, completed_at)
```

New edges: `TRIGGERED` (Device → DetectionEvent), `RESOLVES` (Remediation → DetectionEvent).

New API RPCs in `proto/bonsai_service.proto`:
```proto
rpc CreateDetection(CreateDetectionRequest) returns (CreateDetectionResponse);
rpc CreateRemediation(CreateRemediationRequest) returns (CreateRemediationResponse);
rpc PushRemediation(PushRemediationRequest) returns (PushRemediationResponse);
```

### 2 — Rust: PushRemediation gNMI Set

```proto
message PushRemediationRequest {
  string target_address = 1;
  string yang_path      = 2;
  string json_value     = 3;  // RFC 7951 JSON
}
message PushRemediationResponse {
  bool   success = 1;
  string error   = 2;
}
```

Credentials never leave the Rust process — Python only passes the target address
and YANG path, not credentials. The Rust handler looks up the target's existing
connection (or opens a short-lived one) and executes `gnmi.Set`.

### 3 — Python: Detector base class + FeatureExtractor

`python/bonsai_sdk/detection.py`:
- `Features` dataclass (as above)
- `Detection` dataclass: `rule_id`, `severity`, `features`, `reason`
- `Detector` ABC: `extract_features()` + `detect()`

### 4 — Python: EventWindow

`python/bonsai_sdk/window.py`:
- `EventWindow(device_address, peer_address, window_seconds=300)`
- Thread-safe `deque` of `(timestamp_ns, event_type)` entries, pruned on access
- `count(event_type)` → int (used for flap counting)

### 5 — Python: RuleEngine runner

`python/bonsai_sdk/engine.py`:
- Subscribes to `StreamEvents` from the gRPC API
- Dispatches each event to all registered `Detector` instances
- Calls `detect()` on features where `extract_features()` returns non-None
- Passes positive detections to `RemediationExecutor`

### 6 — Python: Rule implementations (target: 8 for demo)

**BGP rules** (all trigger from `bgp_session_change` events):

| Rule ID | Condition | Severity | Auto-heal? |
|---|---|---|---|
| `bgp_session_down` | new_state ∉ {established, active} | critical | Yes — BGP soft-clear |
| `bgp_session_flap` | ≥3 flaps in 5 min for same peer | critical | No — log only |
| `bgp_all_peers_down` | established_count == 0 on device | critical | No — upstream fault |
| `bgp_never_established` | peer seen >90s, never established | warn | No — config issue |

**Interface rules** (trigger from interface telemetry — requires oper-status sub, see Task 7):

| Rule ID | Condition | Severity | Auto-heal? |
|---|---|---|---|
| `interface_down` | oper-status → down | critical | No |
| `interface_error_spike` | error rate > 100/s | warn | No |
| `interface_high_utilization` | octets rate > 80% of known capacity | warn | No |

**Topology rule**:

| Rule ID | Condition | Severity | Auto-heal? |
|---|---|---|---|
| `topology_edge_lost` | CONNECTED_TO edge absent after it existed | warn | No |

### 7 — Rust: oper-status telemetry subscription

Add oper-status subscription paths for the Phase 4 lab (SRL + XRd):

| Vendor | Path |
|---|---|
| SRL | `interface[name=*]/oper-state` (native, ON_CHANGE) |
| XRd | `Cisco-IOS-XR-pfi-im-cmd-oper:interfaces/interfaces/interface` (SAMPLE 30s) |

Add `InterfaceOperStatus` event variant in `src/telemetry.rs`.
Add `write_interface_oper_status()` in `src/graph.rs` (updates Interface node, emits BonsaiEvent).

### 8 — Python: RemediationExecutor + circuit breaker

`python/bonsai_sdk/remediations.py`:
- `Executor.run(detection)` → selects playbook from a whitelist dict
- Calls `client.push_remediation()` for whitelisted rules, skips others
- Circuit breaker: skip auto-heal if ≥5 remediations fired for same device in 10 min
- Always writes a `Remediation` node (status=success/failed/skipped)

### 9 — Demo script

`python/demo_phase4.py`:
1. Start `RuleEngine` + `Executor` in background threads
2. Print live detection + remediation events
3. Inject a BGP session failure (shut interface via ContainerLab)
4. Watch: detection → BGP clear → graph shows recovery

---

## Resolved Decisions

### D2 ✅ — gNMI Set transport: Rust proxy

Credentials stay in one place (Rust, bonsai.toml). Python passes target address + YANG path
only. `PushRemediation` RPC in `proto/bonsai_service.proto`. No credentials in Python.

### D3 ✅ — Separate DetectionEvent node

`StateChangeEvent` = raw telemetry transition. `DetectionEvent` = rule interpretation.
Separate nodes keep Phase 5 ML training data unambiguous. `features_json` column stores
the full feature vector so training requires no re-extraction.

### D5 ✅ — Remediation safety model

All three layers enforced:
1. **Dry-run flag**: `BONSAI_DRY_RUN=1` — logs the action, no Set sent
2. **Whitelist**: only rules with `auto_remediate=True` in the rule definition execute
3. **Circuit breaker**: ≥5 remediations for same device in 10 min → halt + log

### D6 ✅ — cRPD removed

cRPD removed from all topologies. BGP capability negotiation between cRPD 23.2 and
SRL 26.x/XRd 24.x causes persistent session flapping. Removing an unstable node is
the right call — Phase 4 needs reliable baseline behaviour to distinguish injected faults
from pre-existing flaps. cRPD can be revisited when a stable version is available.

---

## Open Decisions

### D1 — Rule trigger model (hybrid approach pending validation)

**Leaning**: event-driven (StreamEvents) for single-event rules (session down),
poll-based query (30s) for pattern rules (flap rate, topology diff).
Confirm this works before implementing the poll loop.

### D4 — Interface oper-status telemetry

SRL: `interface[name=*]/oper-state` native ON_CHANGE — confirmed working in Phase 2.
XRd: `Cisco-IOS-XR-pfi-im-cmd-oper:interfaces` SAMPLE — needs validation against xrd-pe1.
Decision: subscribe to both in Phase 4, document which XRd path works.

### D7 — Rule authoring: Python classes for Phase 4, YAML DSL later

Python classes now (faster to ship, full expressiveness).
YAML DSL is the right end-state for network engineer authoring — design after
Phase 4 rules are proven to have a stable shape.

---

## Phase 4 Success Criteria

- [ ] Rule engine running, consuming StreamEvents from gRPC server
- [ ] ≥8 rules across BGP + interface categories
- [ ] DetectionEvent written to graph for every fired rule
- [ ] BGP session clear: end-to-end auto-heal working (shut interface → detect → clear → recover)
- [ ] Circuit breaker prevents runaway remediation
- [ ] All Remediation nodes timestamped with outcome (training data ready)
- [ ] Demo works with srl-spine1 + srl-leaf1 + xrd-pe1 as minimum vendor mix
- [ ] features_json on DetectionEvent is the complete Phase 5 feature vector (no re-extraction)

---

## Risks

| Risk | Mitigation |
|---|---|
| SRL SR-MPLS config syntax wrong for this SRL version | Test with just OSPF first; add SR after OSPF adjacencies are confirmed in graph |
| XRd oper-status path returns wrong structure | SAMPLE 30s + log raw updates before writing classifier |
| gNMI Set BGP clear path differs per vendor | Research SRL + XRd Set paths before coding; start with SRL only if needed |
| Phase 4 scope creep into ML | No model inference in Phase 4. Feature extraction + labelling only. |
| LadybugDB schema migration breaks existing queries | Test on a copy of bonsai.db before adding new node types |
