# Phase 4 — Rules-Based Detect and Heal

**Goal**: Deterministic anomaly detection and closed-loop auto-remediation.
A failure injected in ContainerLab is detected by a rule, a remediation fires, and
the graph shows recovery — all without manual intervention. Every detection and
remediation is written into the graph as training data for Phase 5 ML.

---

## Progress Snapshot (Phases 1–3 Complete)

### What exists today

| Layer | Status | Notes |
|---|---|---|
| gNMI subscriber pool | ✅ Done | SRL + XRd + cEOS. ON_CHANGE + SAMPLE per path. Reconnects on drop. |
| Vendor detection | ✅ Done | Capabilities-driven, native-first path selection, no static per-vendor branching. |
| Graph writer | ✅ Done | Device, Interface, BgpNeighbor, LldpNeighbor, StateChangeEvent, CONNECTED_TO edges. |
| BGP state transitions | ✅ Done | StateChangeEvent written on every session-state change; `BonsaiEvent` broadcast. |
| Topology | ✅ Done | 7/8 CONNECTED_TO edges from LLDP (SRL↔cEOS↔XRd bidirectional). |
| gRPC API | ✅ Done | `BonsaiGraph` service: Query, GetDevices, GetInterfaces, GetBgpNeighbors, GetTopology, StreamEvents. |
| Python SDK | ✅ Done | `BonsaiClient`, typed wrappers for all RPCs, `stream_events()` iterator. End-to-end validated. |
| cRPD telemetry | ⏸ Deferred | BGP flap diagnosed (capability mismatch); `hold-time 90` + `family inet unicast` config added. Return in Phase 4 when cRPD rules are needed. |
| Temporal queries | ⏸ Deferred | Schema designed (valid_from/valid_to), writes not yet implemented. |
| gNMI Set / remediation | ⏸ Not started | Phase 4 work. |

---

## Phase 4 Task List

### 1 — Graph schema: DetectionEvent + Remediation nodes

Add two new node types and their edges to `src/graph.rs`:

- `DetectionEvent`: `id`, `device_address`, `rule_id`, `severity` (info/warn/critical),
  `detail_json`, `fired_at TIMESTAMP_NS`
- `Remediation`: `id`, `detection_id`, `action`, `status` (pending/success/failed/skipped),
  `detail_json`, `attempted_at`, `completed_at`
- Edge `TRIGGERED`: Device → DetectionEvent
- Edge `RESOLVES`: Remediation → DetectionEvent

Add `CreateDetection` and `CreateRemediation` RPCs (or fold into `Query` write — decide, see §Decisions).

### 2 — Rust: gNMI Set RPC

Add `PushRemediation` to `proto/bonsai_service.proto`:

```proto
message RemediationRequest {
  string target_address = 1;   // e.g. "172.100.101.11:57400"
  string yang_path      = 2;
  string json_value     = 3;   // RFC 7951 JSON
}
message RemediationResponse {
  bool   success = 1;
  string error   = 2;
}
rpc PushRemediation(RemediationRequest) returns (RemediationResponse);
```

Implement in `src/api.rs`: look up the target's subscriber connection (or open a
short-lived gNMI Set connection), execute `gnmi.Set`, return result.

### 3 — Python: rule engine runner

`python/bonsai_sdk/rules.py`:
- Base class `Rule` with `evaluate(event: StateEvent) -> Optional[Detection]`
- `RuleEngine`: holds a list of `Rule` instances, subscribes to `StreamEvents`,
  dispatches each event to all rules, collects `Detection` objects
- Writes detections back to the graph via `BonsaiClient.create_detection()`

```python
class Rule:
    rule_id: str
    severity: str

    def evaluate(self, event: StateEvent, client: BonsaiClient) -> Optional[Detection]:
        ...
```

### 4 — Python: state window for multi-event rules

Rules like "BGP flapped 3 times in 5 minutes" need a sliding window of past events.
Implement a simple `EventWindow` — a `deque` capped by time — passed into `evaluate()`.
This is in-process state only; Phase 5 can replace it with graph queries over the
`StateChangeEvent` history.

### 5 — Python: initial rule set (target: 8–10 rules for demo)

**BGP rules** (highest value, easiest to trigger in lab):
- `BgpSessionDown` — session_state transitions to non-established → CRITICAL
- `BgpSessionFlap` — session flapped ≥3 times in 5 minutes → CRITICAL
- `BgpAllPeersDown` — all peers on a device go down simultaneously → CRITICAL (likely upstream)
- `BgpSessionNeverEstablished` — new peer seen but never reaches established in 60s → WARN

**Interface rules** (need oper-status telemetry — see Decision D4):
- `InterfaceDown` — link goes operationally down → CRITICAL
- `InterfaceErrorSpike` — in_errors or out_errors rate > threshold/s → WARN
- `InterfaceHighUtilization` — in_octets or out_octets rate > 80% of capacity → WARN (needs capacity config)

**Topology rules**:
- `ConnectedToEdgeLost` — a CONNECTED_TO edge existed last cycle but is absent → WARN
  (catches physical disconnect without an interface-down event)

### 6 — Python: remediation playbooks

For each rule with a safe automated action:

| Detection | Action | gNMI Set path | Safe to automate? |
|---|---|---|---|
| `BgpSessionDown` (stuck in active/idle) | Clear BGP session | vendor-specific reset path | Cautiously yes — clear only, not config change |
| `BgpSessionFlap` | Log + alert only | — | No — too risky without root cause |
| `InterfaceDown` | Log + alert only | — | No — may be intentional |
| `InterfaceErrorSpike` | Log + alert only | — | No — needs diagnosis first |
| `BgpAllPeersDown` | Log + alert only | — | No — upstream issue, local action won't help |

Phase 4 realistic auto-heal: BGP session clear only. Everything else fires a
`Remediation` node with `status=skipped` and a human-readable reason.

### 7 — Python: remediation executor

`python/bonsai_sdk/remediations.py`:
- `Executor`: takes a `Detection`, selects a playbook, calls `BonsaiClient.push_remediation()`,
  writes a `Remediation` node with outcome
- Safety guard: if >N remediations fired in last T minutes for the same device, skip
  and log a circuit-breaker event

### 8 — Demo script

`python/demo_phase4.py`:
1. Start `RuleEngine` + `Executor` in background threads
2. Print live detections and remediations as they arrive
3. Instructions: manually shut a BGP session in ContainerLab
4. Watch: detection fires → BGP-clear remediation executes → graph shows recovery

### 9 — Record all events in graph for Phase 5 training data

Ensure every `DetectionEvent` and `Remediation` node is written with full timestamps
and detail JSON. Phase 5 ML will query these as labeled training examples
(detection = anomaly label, remediation outcome = ground truth for classifier).

---

## Open Decisions

These must be resolved before or during Phase 4 implementation. Flag in DECISIONS.md
when each is settled.

### D1 — Rule trigger model: event-driven vs poll-based

**Option A — Event-driven** (consume `StreamEvents`, evaluate on each event):
- Pro: sub-second detection latency; rules see every transition
- Con: multi-event rules (flap counting) need in-process state; if the rule engine
  restarts, window state is lost

**Option B — Poll-based** (query graph on a timer, e.g. every 10s):
- Pro: stateless rules, trivially restartable; can express any graph pattern as Cypher
- Con: 10s latency on detection; misses transient events that resolve before next poll

**Leaning**: Hybrid — event-driven for immediate single-event rules (session down),
poll-based (30s) for pattern rules (flap rate, topology diff). Implement event-driven
first, add poll loop for pattern rules when needed.

---

### D2 — gNMI Set transport: Rust proxy vs Python direct

**Option A — Python calls new `PushRemediation` gRPC on the Rust core**:
- Pro: Rust manages all device connections (one connection pool); Python never touches
  devices directly; credentials stay in Rust/bonsai.toml
- Con: adds a new RPC and Rust code; Rust must store per-target connection handles

**Option B — Python connects to devices directly via `pygnmi` or `scrapli-gnmi`**:
- Pro: Python is fully self-contained for remediation; no new Rust code
- Con: Python needs credentials (re-reads bonsai.toml or gets them separately);
  connection lifecycle is duplicated; harder to test

**Leaning**: Option A (Rust proxy). Credentials and connection lifecycle belong
in one place. The new RPC is ~50 lines of Rust. This is also the right shape for
Phase 5 where the ML model pushes remediations.

---

### D3 — DetectionEvent schema: extend StateChangeEvent vs separate node type

**Option A — Add `rule_id` and `severity` to `StateChangeEvent`**:
- Pro: one table, simpler schema, existing RPCs cover it
- Con: mixes raw telemetry transitions (phase 2) with rule-derived detections (phase 4);
  confuses ML training data separation

**Option B — Separate `DetectionEvent` node**:
- Pro: clean separation; Phase 5 can query detections independently; graph traversal
  `(d:Device)-[:TRIGGERED]->(de:DetectionEvent)-[:RESOLVES]-(r:Remediation)` is
  natural and readable
- Con: one more schema migration

**Leaning**: Option B (separate node). The semantic distinction is real.
`StateChangeEvent` = raw telemetry transition. `DetectionEvent` = rule interpretation.

---

### D4 — Interface oper-status telemetry path

Interface down detection requires an oper-status subscription. We currently subscribe
to counters only. Need to add a path for `oper-status` / `admin-status` per vendor:

| Vendor | Native path | OC path |
|---|---|---|
| SRL | `interface[name=*]/oper-state` | N/A (rejected) |
| XRd | `Cisco-IOS-XR-pfi-im-cmd-oper:interfaces/interfaces/interface` | partial |
| cEOS | leaf updates via `openconfig-interfaces:interfaces/interface/state/oper-status` | ✅ |

Decision needed: subscribe to oper-status for all three vendors before Phase 4, or
write interface-down rules only for vendors where the path is confirmed working.

**Leaning**: Subscribe to SRL native + cEOS OC now; skip XRd oper-status until confirmed
(XRd BGP rules cover the primary demo). Document per-vendor status in telemetry.rs.

---

### D5 — Remediation safety model

Three options in increasing strictness:
1. **Dry-run flag** (`BONSAI_DRY_RUN=1`) — log what would be sent, no actual Set
2. **Per-rule whitelist** — only rules explicitly marked `auto_remediate=True` execute
3. **Circuit breaker** — auto-remediation halts if >N remediations fired in last T minutes for one device

All three are non-exclusive. Recommendation: implement all three. Default for Phase 4:
dry-run off, auto_remediate whitelist required, circuit breaker at 5 per device per 10 minutes.

---

### D6 — cRPD inclusion in Phase 4

cRPD was deferred in Phase 2 (BGP flap due to capability mismatch between cRPD 23.2
and SRL 26.x/XRd 24.x). `hold-time 90` and `family inet unicast` were added to crpd1.cfg.

Decision: test cRPD BGP stability before Phase 4 rules. If sessions stabilize, include
cRPD in the demo. If not, document cRPD as a known gap and demo with SRL+XRd+cEOS only.

**Leaning**: Test cRPD first. The Phase 4 demo is more compelling with all four vendors.
If cRPD still flaps, file it as a Junos capability negotiation issue and move on.

---

### D7 — Rule authoring format: Python classes vs YAML DSL

**Option A — Python classes** with a `Rule` base class:
- Pro: full Python expressiveness; easy to query graph, call external APIs;
  no parser to write or maintain
- Con: rules require Python knowledge; no hot-reload without restart

**Option B — YAML rule definitions** with a Python interpreter:
```yaml
- id: bgp_session_down
  match: event_type == "bgp_session_change" AND new_state != "established"
  severity: critical
  remediation: clear_bgp
```
- Pro: rules are readable by network engineers without Python; hot-reload possible
- Con: limited expressiveness; multi-step rules need escape hatches back to Python

**Leaning**: Python classes for Phase 4. The YAML DSL is the right end-state but
adds a parser and a DSL design problem before the first rule is even working.
YAML format can be added in Phase 5/6 once rule structure is proven.

---

## Phase 4 Success Criteria (from PROJECT_KICKOFF.md)

- [ ] Rule engine running, consuming `StreamEvents` from the gRPC server
- [ ] ≥8 rules implemented across BGP and interface categories
- [ ] Each fired rule writes a `DetectionEvent` node to the graph
- [ ] At least one end-to-end auto-remediation working: BGP session stuck → clear → recovery
- [ ] All remediations record outcome in a `Remediation` node
- [ ] Circuit breaker prevents runaway remediation
- [ ] Demo: break BGP in ContainerLab → detection → remediation → graph shows recovery
- [ ] Demo works with ≥2 vendor combinations (SRL+XRd minimum)
- [ ] All DetectionEvent + Remediation nodes timestamped and stored (Phase 5 training data)

---

## Dependencies and Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| gNMI Set path varies per vendor | High | Research SRL + XRd Set paths before coding. SRL has good docs. |
| cRPD BGP still flapping | Medium | Test first; demo without cRPD if needed |
| Phase 4 scope creep into ML | Medium | No ML in Phase 4. Classifier = rule. Detection = labeled event. Nothing more. |
| Auto-remediation causes worse state | Medium | Circuit breaker + whitelist + dry-run mode as defaults |
| LadybugDB new node types break existing queries | Low | Test schema migration on bonsai-mv.db before coding |
