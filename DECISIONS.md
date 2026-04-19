# DECISIONS.md — Bonsai Architecture Decision Log

Append-only. Never edit or delete past entries. Add new entries at the bottom.
Format: `## YYYY-MM-DD — <title>`

---

## 2026-04-17 — Core language: Rust

**Decision**: Rust (stable, edition 2024) for the ingestion and graph engine core.

**Alternatives considered**: Go, Python.

**Rationale**:
- Streaming telemetry ingestion at scale benefits from zero-cost abstractions and
  predictable latency — no GC pauses on the hot path.
- Parsing untrusted protobuf from network devices benefits from memory safety guarantees.
- tokio async runtime is mature and well-suited to the per-device subscriber pool pattern.
- Holo (MIT-licensed Rust routing suite with native gNMI) serves as both a reference
  implementation and a ContainerLab peer — reduces the "Rust in network automation is
  uncharted" risk.
- Go was the serious alternative; rejected because this is a learning project and
  the Rust upskill is part of the explicit goal.
- Python rejected for the core due to GIL and async limitations at streaming scale;
  Python is appropriate for the rules engine and ML pipeline (Phase 4/5).

---

## 2026-04-17 — Async runtime: tokio only

**Decision**: tokio is the one and only async runtime. No async-std, no smol.

**Rationale**: tonic (gRPC) requires tokio. Mixing runtimes causes hard-to-debug
panics. Constraint makes dependency choices simpler for the lifetime of the project.

---

## 2026-04-17 — gRPC/protobuf: tonic + prost

**Decision**: tonic for the gRPC transport layer, prost for protobuf code generation
from the official openconfig/gnmi .proto files.

**Rationale**: tonic is the de-facto standard tokio-native gRPC crate. prost integrates
cleanly with tonic's build pipeline. The Holo project uses the same stack — provides
a reference to cross-check against.

---

## 2026-04-17 — Graph database: DEFERRED — research task for Phase 1

**Decision**: Not yet decided. Three candidates:

1. **Kuzu v0.11.3** — MIT, embedded, Rust bindings exist. Archived (Apple acquisition
   Oct 2025) so no future development, but stable and known-working.
2. **Ladybug** — community fork of Kuzu by Arun Sharma. Intends to be a 1:1 Kuzu
   replacement. Early stage; stability unknown.
3. **ArcadeDB** — Apache 2.0, multi-model, OpenCypher support (97.8% TCK pass).
   More mature but runs as a server process, not embedded.

**Action**: Spend 2–3 days in Phase 1 benchmarking with a small synthetic graph
workload (node upserts, edge traversals, temporal queries). Document outcome here.

**Leaning**: Start with Kuzu v0.11.3 for stability. Plan migration path to Ladybug
if it stabilizes by end of Phase 2.

---

## 2026-04-17 — Telemetry ingestion: gNMI Subscribe only (no polling, no SNMP, no NETCONF)

**Decision**: gNMI Subscribe (STREAM mode, ON_CHANGE + SAMPLE) is the only ingestion
path. No SNMP, no NETCONF, no REST scraping, no Telegraf intermediate hop.

**Rationale**: The project's core thesis is streaming-first. Polling defeats the
purpose. SNMP is legacy. NETCONF is request/response. Taking gNMI directly keeps
the latency path short and the architecture honest.

**Constraint**: This is non-negotiable for the lifetime of the project.

---

## 2026-04-17 — Python integration: REST API first, PyO3 later

**Decision**: Python layer communicates with the Rust core via a REST API. PyO3
FFI binding is a future option once the API is stable.

**Rationale**: REST is simpler to iterate on during early phases. PyO3 offers lower
latency and tighter integration but adds build complexity before the API contract
is known. Revisit at end of Phase 3.

---

## 2026-04-17 — Project name: bonsai

**Decision**: The project is named **bonsai**.

**Rationale**: Deliberate cultivation of something precise and living. Mirrors the
core discipline of this project — prune scope ruthlessly, shape carefully, let it
grow only where it should. The folder name was already bonsai; the metaphor fits.

---

## 2026-04-17 — SR Linux gNMI path normalization: use native paths at ingestion, normalize in pipeline

**Decision**: Subscribe using SR Linux native path `interface[name=*]/statistics`
(singular, no `srl_nokia-interfaces:` prefix). Normalization to OpenConfig canonical
paths (`/interfaces/interface[name=*]/state/counters`) is deferred to a Phase 2
normalization layer.

**Context**: Nokia SR Linux 26.x deviates from OpenConfig canonical paths:
- Uses `interface` (singular) vs OpenConfig `interfaces` (plural)
- Responses may carry the `srl_nokia-interfaces:` model prefix on returned values
- ContainerLab gNMI server on port 57400; no separate `gnmi-server` config block
  needed in SR Linux 26.x (enabled automatically)

**Rationale**: Subscribing to native paths is required — SR Linux rejects the
OpenConfig canonical path. Normalizing at ingestion time (in the subscriber) would
couple device-specific quirks to the transport layer. A dedicated normalization
stage in the pipeline is cleaner and easier to test. Each NOS will need its own
normalization rules; centralizing them in one place is the right long-term shape.

**Impact on current code**: `interface_counters_path()` in `src/subscriber.rs`
uses `interface` (singular) with a wildcard key `name=*`. Path normalization is
out of scope until Phase 2.

---

## 2026-04-17 — gNMI authentication model: mTLS preferred, credentials-from-config as fallback

**Decision**: The correct authentication model for gNMI is mTLS (mutual TLS). Username/password
credentials are supported as a fallback for NOS implementations that require them, but
credentials must never appear in source code.

**Context and reasoning**:

gNMI has no authentication specification of its own — it rides gRPC over TLS. Most NOS
implementations (SR Linux, IOS-XR, Junos) bolt username/password onto gRPC metadata headers
because their gNMI server reuses the same AAA stack as SSH. This is a vendor implementation
detail, not a gNMI feature.

With mTLS, both sides present certificates. The device trusts certificates signed by a known
CA; bonsai presents a client certificate. No passwords are exchanged. This is the production-
correct model and what Google's production systems use. The "onboarding" step becomes: issue a
client cert to bonsai, add the CA to the device's trust store.

**Credential storage hierarchy** (most to least preferred):
1. No credentials (device configured with mTLS or `skip-authentication`)
2. Environment variables referenced by name in `bonsai.toml`
3. Inline plaintext in `bonsai.toml` — only for lab; `bonsai.toml` must not be committed
4. Hardcoded in source — **never acceptable**

**Impact on code**:
- `GnmiSubscriber` accepts `Option<String>` for username and password
- Interceptor only injects headers when credentials are `Some`
- No credentials in `main.rs`, `bonsai-mv.rs`, or any compiled binary
- mTLS client cert support added when needed (Phase 3 target)

**Constraint**: Credentials in source code are a hard block on any PR.

---

## 2026-04-17 — Device onboarding model: config-file-driven, Capabilities-based vendor detection

**Decision**: Targets are declared in an external `bonsai.toml` config file, not in source code.
Vendor family is auto-detected at runtime via the gNMI Capabilities RPC. An optional `vendor`
override in the config skips detection for known-problematic devices.

**Onboarding flow**:
1. Operator configures gNMI/gRPC on the device (port, TLS mode, credentials if required)
2. Operator appends a `[[target]]` block to `bonsai.toml` with address, TLS settings, credentials
3. Bonsai starts, reads `bonsai.toml`, connects to each target
4. Capabilities RPC returns `supported_models` — bonsai inspects model names to detect vendor:
   - `srl_nokia-*` prefix → `nokia_srl`
   - `Cisco-IOS-XR-*` prefix → `cisco_xrd`
   - `junos-*` prefix → `juniper_crpd`
   - `arista-*` / EOS models → `arista_ceos`
   - fallback → `openconfig` (uses OC canonical paths)
5. Bonsai selects subscription paths for the detected vendor and subscribes

**Dial-in vs dial-out**:
Bonsai uses dial-in (bonsai connects to device). Dial-out (device pushes to a collector endpoint)
is the correct model for large fleets but adds collector infrastructure complexity. Dial-out is
a Phase 3+ consideration. Dial-in is correct for single-host lab deployment (Phase 1–2).

**Config file security**:
- `bonsai.toml` is listed in `.gitignore` — actual credentials never committed
- `bonsai.toml.example` is committed — a redacted template with comments
- Production path: credentials come from environment variables referenced by name in the config

**Impact on code**:
- Single `bonsai` binary handles all topologies; `bonsai-mv.rs` is deleted
- `src/config.rs` owns the config model and TOML loading
- `GnmiSubscriber::new()` accepts `vendor_hint: Option<String>` — None triggers Capabilities detection

---

## 2026-04-17 — Graph database: LadybugDB (lbug crate) with Grafeo as fallback

**Decision**: Use **LadybugDB** (`lbug` crate on crates.io) as the embedded graph
database for Phase 2. **Grafeo** is the named fallback if Ladybug stalls.

**Candidates evaluated**:

| Candidate | Status | License | Rust embed | Cypher | Temporal | Verdict |
|---|---|---|---|---|---|---|
| Kuzu v0.11.3 | Archived (Apple, Oct 2025) | Formerly MIT | FFI (frozen) | Yes | DIY | Avoid |
| **LadybugDB v0.15.3** | Active, ~2-week cadence | MIT | FFI (`lbug`) | Yes | DIY | **Chosen** |
| ArcadeDB v26.3.2 | Active | Apache 2.0 | JVM-only | Yes (97.8% TCK) | Time-series only | Wrong fit |
| SurrealDB | Active | BSL 1.1 | Native Rust | No (SurrealQL) | Native, excellent | BSL + no Cypher |
| Cozo | Abandoned (Dec 2023) | MPL 2.0 | Native Rust | No (Datalog) | Native, excellent | Abandoned |
| Grafeo v0.5.x | New (Mar 2026) | Apache 2.0 | Native Rust | Yes | DIY | Too new — watch |
| FalkorDB | Active | SSPLv1 | Redis module | Yes | DIY | SSPLv1 + Redis dep |

**Rationale for LadybugDB**:
- Only option satisfying all hard constraints at once: embedded in-process, MIT,
  Rust bindings, Cypher/OpenCypher, active development
- Direct code fork of Kuzu — same columnar storage, vectorized execution, MVCC
  transactions, and Cypher implementation. Kuzu benchmarks apply as baseline.
- v0.15.3 as of April 2026, consistent release cadence since November 2025
- Arun Sharma (founder) has prior distributed graph systems experience (Facebook
  Dragon, Google)

**Known gap — temporal queries**: LadybugDB has no native time-travel or point-in-time
snapshot feature. "What did the graph look like 5 minutes ago" requires DIY
bitemporal modeling:
- Every node and edge carries `valid_from TIMESTAMP` and `valid_to TIMESTAMP`
- On update: set `valid_to = now()` on the existing record, insert new record with
  `valid_from = now()` and `valid_to = NULL`
- Historical queries: `WHERE valid_from <= $t AND (valid_to IS NULL OR valid_to > $t)`
- This is standard practice, adds one extra write per update, and works cleanly
  at our scale (hundreds of upserts/minute for Phase 1–2)

**Risks and mitigations**:
- Risk: Ladybug is a 6-month-old fork with a single primary maintainer — bus factor
- Mitigation: Grafeo (Apache 2.0, pure Rust, embedded, Cypher) is the named fallback.
  It appeared March 2026 and has code quality concerns (AI-generated at scale), but
  if Ladybug stalls, Grafeo should be re-evaluated. Set a 6-month review checkpoint.
- Risk: FFI bindings over C++ core (same as Kuzu) — not pure Rust
- Mitigation: acceptable for Phase 2; Grafeo would resolve this if it matures

**Fallback trigger**: if LadybugDB has no release activity for 60+ days, evaluate
Grafeo for replacement before writing more graph code.

---

## 2026-04-18 — Capabilities-driven native-first subscription strategy

**Decision**: All path selection is driven exclusively by the device's Capabilities
response. Native vendor models are preferred when advertised; OpenConfig is the
fallback. No per-vendor static branching in path selection logic.

**Context**: Phase 2 multi-vendor lab (SRL, XRd, cEOS). Prior approach had static
per-vendor path tables that required code changes when adding a new vendor or
when a device firmware changed supported models.

**Rules implemented**:
- `has_srl_native` (srl_nokia in model names) → SRL native paths for all concerns
- `has_xr_native` (Cisco-IOS-XR-infra-statsd-oper model present) → XR native stats
- `has_oc_interfaces/bgp/lldp` → OC paths as fallback
- Vendor label is derived from Capabilities for logging and Device node tagging only,
  never for path routing decisions
- Same rule applies to every vendor: native-first, OC second, no duplicates

**Alternatives considered**: Static per-vendor tables (rejected — breaks at firmware
boundaries and requires code changes per device), always-OC (rejected — SRL does not
advertise OC model names; XR native stats are richer than OC counters).

---

## 2026-04-18 — Per-notification leaf grouping in subscriber

**Decision**: In `subscribe_telemetry`, scalar leaf updates within a single gNMI
notification are grouped by parent path before dispatch to the graph writer.

**Context**: cEOS (and some XRd paths) send individual scalar leaves rather than a
JSON blob at the container path. Without grouping, `interfaces/interface[name=X]/state/counters/in-pkts`
arrives as path=`…/in-pkts`, value=`12345` — no classifier can match it.
After grouping, the dispatcher sees path=`interfaces/interface[name=X]/state/counters`,
value=`{"in-pkts":12345,"out-pkts":...}` — same blob-at-container-path shape as SRL.

**Rule**: If a TypedValue is a scalar (Number/String/Bool), split on the last `/`,
accumulate leaves into a `HashMap<parent_path, JSON object>`, and emit one synthetic
TelemetryUpdate per parent path per notification. JSON object blobs and null values
are forwarded as-is. This transform is invisible to all downstream classifiers.

---

## 2026-04-18 — XRd BGP blob walker

**Decision**: XRd ON_CHANGE BGP sends partial `network-instances` JSON trees (one
blob per neighbor) rather than individual leaf paths. A dedicated `walk_xrd_bgp_blob`
function navigates `network-instance.protocols.protocol.bgp.neighbors.neighbor` to
extract `neighbor-address` and the `state` sub-object.

**Why a walker instead of path-based classification**: XRd does not send
`neighbors/neighbor[neighbor-address=X]/state` as the path; it always uses
`network-instances` as the top-level path with a partial JSON tree as the value.
The walker is the only viable approach without changing the subscription path.

**`BgpNeighborState` carries `state_value: Option<Value>`**: When the walker fires,
the pre-extracted `state` sub-object is passed along so `write_bgp_neighbor` reads
`session-state` and `peer-as` from the correct nesting level rather than the top-level
update value. `None` means callers use `u.value` directly (SRL native, OC paths).

---

## 2026-04-18 — XRd native LLDP: SAMPLE subscription + blob walker

**Decision**: Subscribe to XRd's native LLDP path
(`Cisco-IOS-XR-ethernet-lldp-oper:lldp/nodes/node/neighbors/details/detail`)
using SAMPLE mode (60 s interval), not ON_CHANGE. Walk the `lldp-neighbor[0]`
array to normalize into `{"chassis-id", "system-name", "port-id"}` before writing.

**Why SAMPLE instead of ON_CHANGE**: LLDP neighbors discovered before the subscription
starts will never trigger an ON_CHANGE event. SAMPLE guarantees the initial sync
includes all existing neighbors regardless of when they were discovered.

**Why native path**: The OC path (`openconfig/lldp`) root subscription returned no
data from XRd even though `openconfig-lldp` is advertised in Capabilities. The native
`ethernet-lldp-oper` path responds correctly.

**`LldpNeighbor` carries `state_value: Option<Value>`**: Same pattern as `BgpNeighborState`.
The walker produces a flat `{"chassis-id", "system-name", "port-id"}` object so
`write_lldp_neighbor` works identically for SRL native, OC (cEOS), and XRd native
without any vendor branching in graph.rs.

**XRd hostname**: Default IOS-XR hostname is "ios". Added `hostname xrd1` to xrd1.cfg
so LLDP broadcasts `system-name=xrd1`, allowing `try_connect_interfaces` to resolve
the Device node from SRL/cEOS LLDP data. Without this, edges from SRL/cEOS to XRd
are silently skipped (device lookup returns no rows).

---

## 2026-04-18 — Phase 3 Python SDK transport: gRPC

**Decision**: Expose the Bonsai graph to Python clients via a tonic gRPC server
(`BonsaiGraph` service in `proto/bonsai_service.proto`). Python uses `grpcio` stubs
generated by `grpcio-tools`.

**Rejected alternatives**:
- **PyO3 / native extension**: Near-zero latency but couples Python version to Rust
  build, prevents running the SDK against a remote instance, and requires a FFI
  boundary for every new call. Premature for a Phase 3 read path.
- **REST (axum/actix)**: Familiar but requires a separate HTTP server, JSON schema
  maintenance, and manual streaming support. gRPC gives streaming for free via
  `StreamEvents`.
- **File export (JSON/SQLite)**: Polling-only, no streaming, brittle on Windows file
  locking. Unacceptable for a real-time rules engine in Phase 4.

**Why gRPC**:
1. Single source of truth: the `.proto` file is the contract for both sides.
2. `StreamEvents` server-streaming is the Phase 4 rules-engine entry point — getting
   it right now avoids a breaking API change later.
3. `grpcio-tools` generates typed Python stubs; the SDK layer is a thin wrapper.
4. The gRPC server runs inside the existing tokio runtime as a spawned task — no
   second process or port conflicts beyond the single `api_addr` config key.

**Event broadcast**: `tokio::sync::broadcast::channel(1024)` in `GraphStore`.
`write_state_change_event` publishes a `BonsaiEvent` after every DB insert.
`StreamEvents` subscribes via `BroadcastStream`; lagged receivers are silently
dropped (broadcast semantics: no back-pressure on the writer).

---

## 2026-04-18 — Fixed graph database name: bonsai.db

**Decision**: The graph database file is always named `bonsai.db`, regardless of
topology or lab context. `graph_path` defaults to `bonsai.db` in `src/main.rs`;
`bonsai.toml` sets it explicitly to `bonsai.db`.

**Why**: Topology-specific names (e.g. `bonsai-mv.db`) are an operational anti-pattern.
In production you don't rename your database when you add a device. The graph accumulates
state across topology changes — a name change would lose history and confuse automation
that references a known path.

---

## 2026-04-18 — cRPD removed from all topologies

**Decision**: Juniper cRPD is removed from `multivendor.clab.yml` and will not appear
in the Phase 4 lab topology.

**Why**: cRPD 23.2R1.13 had persistent BGP session flapping against SRL 26.x and
XRd 24.x (diagnosed as capability negotiation failure during OPEN exchange). Phase 4
requires a stable baseline to distinguish injected faults from pre-existing flaps.
An unstable node undermines that. cRPD can be re-evaluated when a stable image
compatible with current SRL/XRd firmware is available.

**Not a permanent decision**: cRPD remains in scope per PROJECT_KICKOFF.md as one of
the four target vendor families. This is a deferral, not a removal from the roadmap.

---

## 2026-04-18 — ML-ready detection abstraction: Detector base class

**Decision**: The Phase 4 rule engine uses a `Detector` ABC with two methods:
`extract_features()` and `detect()`. Rules and future ML models both implement this
interface. Feature extraction is shared; only `detect()` changes when moving to ML.

**Why**: Separating feature extraction from decision logic means:
1. Training data is stored in the graph from day one (features_json on DetectionEvent)
   with no re-extraction needed in Phase 5.
2. Swapping a rule for an ML model is a one-method change, not a pipeline refactor.
3. Rules can be validated by inspecting the feature dict, not just the firing behaviour.

**Feature storage**: Every `DetectionEvent` carries `features_json` — the complete
feature vector that triggered it. Phase 5 reads these as labelled training examples
(`DetectionEvent` = anomaly label, `Remediation.status` = outcome label for the
remediation classifier).

---

## 2026-04-18 — gNMI Set transport: Rust proxy via PushRemediation RPC

**Decision**: Python never connects to devices directly for remediation. All gNMI Set
calls go through a new `PushRemediation` RPC on the Rust gRPC server. Python sends:
target address + YANG path + JSON value. Rust executes the Set using the existing
connection (or opens a short-lived one), returns success/error.

**Why**: Credentials (usernames, passwords, TLS certs) are owned by the Rust process
via `bonsai.toml`. Passing them to Python would duplicate credential storage and risk
them appearing in Python logs or stack traces. The Rust proxy is also the correct shape
for Phase 5 where the ML remediation selector pushes actions — one integration point,
one credential store, one connection pool.

**Constraint**: Python never receives or stores credentials. This is non-negotiable.

---

## 2026-04-18 — Phase 4 lab topology: DC spine-leaf + SP PE (bonsai-phase4.clab.yml)

**Decision**: The Phase 4 test lab is `lab/fast-iteration/bonsai-phase4.clab.yml`:
3× Nokia SR Linux (spine + 2 leaves) + 1× Cisco IOS-XRd (SP PE edge). Total ~6.5 GB RAM.

**Why this topology**:
- Each device has 2–3 BGP sessions → rules like "one peer down" vs "all peers down" are testable
- OSPF area 0 underlay → future OSPF adjacency rules without topology changes
- SR-MPLS prefix-SIDs on SRL → SP path telemetry in Phase 4/5 without adding nodes
- 3× SRL minimises RAM (SRL ≈ 1.5 GB vs cEOS ≈ 2 GB, XRd ≈ 2 GB)
- All links are point-to-point `/31`s → clean LLDP CONNECTED_TO edges, no broadcast domains

**Fault injection**: ContainerLab `tools netem` for link impairment; `sr_cli` or
bonsai `PushRemediation` for interface/BGP-level faults. No external tool required.

---

## 2026-04-18 — Rule trigger model: hybrid event + poll (D1 resolved)

**Decision**: Event-driven (StreamEvents gRPC) for single-event rules; poll-based
(30 s interval) for pattern rules requiring history or counter deltas.

**Why**:
- BGP state transitions (`bgp_session_change`) arrive as ON_CHANGE gNMI updates;
  routing them directly through StreamEvents gives sub-second detection latency.
- Interface counter deltas (error rate, utilisation) require two samples — polling
  the graph every 30 s is simpler and correct; the SAMPLE interval is 10 s so
  at least two data points are always available.
- Topology diff (`topology_edge_lost`) requires comparing the current LLDP edge set
  against the previous snapshot — inherently poll-based.
- The `RuleEngine` implements both loops in parallel background threads. Validated
  in Phase 4: event loop and poll loop coexist without interference.

---

## 2026-04-18 — Interface oper-status telemetry: SRL ON_CHANGE confirmed (D4 resolved)

**Decision**: SRL native oper-state path (`interface[name=*]/oper-state`) with
ON_CHANGE subscription is the primary interface oper-status source. XRd deferred.

**What was found**:
- SRL sends oper-state as a scalar leaf. The subscriber's leaf-grouping logic
  consolidates it under the parent container path
  (`srl_nokia-interfaces:interface[name=X]` with value `{"oper-state":"up/down"}`).
  The original classifier checked `path.ends_with("/oper-state")` which never matched
  the grouped form. Fixed by checking `json_find(value, "oper-state").is_some()`.
- XRd: the `oc_interfaces` OC subscription is accepted by XRd but oper-status events
  were not seen in Phase 4 testing. Deferred — XRd interface rules are not blocked
  on this since the PE node is not a target for interface-level auto-remediation.

**Phase 5 note**: When XRd oper-status is needed, subscribe to
`Cisco-IOS-XR-pfi-im-cmd-oper:interfaces` (SAMPLE 30 s) and extend the OC
oper-status classifier to normalise the XR native field names.

---

## 2026-04-18 — BgpSessionDown guard: established->idle only

**Decision**: `BgpSessionDown` and `BgpSessionFlap` only fire when
`old_state == "established"` transitions to `new_state == "idle"`.

**Why**: The BGP FSM has many transient states (active, opensent, openconfirm,
connect) that cycle during normal reconnection. Firing on `active->idle` (the
exponential backoff retry) caused a remediation feedback loop: each `bgp_session_bounce`
resets the session to idle, which triggers another detection, saturating the circuit
breaker within seconds. Only `established->idle` means a working session was lost
and warrants remediation. All other non-established transitions are normal FSM cycling.

---

## 2026-04-18 — Junos native interface classifier paths removed from telemetry.rs

**Decision**: The Junos-specific interface stats classifier block in `telemetry.rs`
(lines matching `input-bytes` / `output-bytes` / `input-packets` field names from
`junos-state-interfaces`) is removed. cRPD is not in scope for Phase 4/5 and these
paths were never validated against a live device.

**Rationale**: Dead code rots. An untested vendor path gives false confidence and
creates maintenance surface with no benefit until cRPD is re-enabled. The block
should be re-added when cRPD is back in scope, informed by actual Capabilities and
live validation, not copied from a prior speculative attempt.

**Re-enable condition**: When cRPD is added back to the Phase N lab topology, add
an ADR capturing the validated Junos gNMI subscription path and field names before
adding any code. Do not restore the old block without validation.

---

## 2026-04-18 — StateChangeEvent pruning deferred to Phase 5.5

**Decision**: No event retention / pruning logic will run in Phases 4 or 5.
A `retention` module seam (`src/retention.rs`, `prune_events` function) will be
scaffolded in Phase 5.0 hygiene work so the entry point exists, but it will be
a no-op and disabled by default in `bonsai.toml`.

**Rationale**: The lab topology generates tens of events per minute — volume is
irrelevant today. Phase 5 ML training needs the full DetectionEvent + Remediation
history intact. Deleting events before Phase 5 training data is exported would
destroy training labels. Pruning of raw StateChangeEvents (keeping 72h hot,
exporting to Parquet) is the right long-term model but is Phase 5.5 work.

**Phase 5.5 plan** (capture here so it is not forgotten):
- StateChangeEvent: keep 72h in graph, export older records to Parquet (one file
  per day, device-partitioned), delete from graph after export.
- DetectionEvent + Remediation: keep forever in graph — small, high-value training data.
- The `prune_events(store, cutoff)` function scaffolded now runs on a tokio interval;
  enabling it requires setting `[retention] enabled = true` in `bonsai.toml`.

---

## 2026-04-18 — Schema migration story deferred; LadybugDB has no ALTER TABLE

**Decision**: Adding columns to existing LadybugDB node tables (e.g., adding
`firmware_version` to Device, or `triggered_by_id` to DetectionEvent) requires
dropping and recreating the table. There is no `ALTER TABLE ADD COLUMN` in the
current LadybugDB/Kuzu Cypher dialect. This is a known gap; no migration
infrastructure is built for Phase 5.0.

**Rationale**: The schema is stable at Phase 4. Phase 5.0 adds one new edge type
(`TRIGGERED_BY` from DetectionEvent to StateChangeEvent) — this is an `CREATE REL TABLE`
not an `ALTER`, so it is safe. No existing node table columns change. The migration
problem only becomes real when a node property must be added to a table with existing
data.

**Mitigation until a migration story exists**:
1. New properties are added via new edge types where possible (avoids ALTER).
2. Node table schema changes require a `bonsai.db` rebuild (acceptable for a lab
   deployment — stop bonsai, delete the DB, restart to rebuild from live telemetry).
3. Schema version is tracked as a property on a singleton `SchemaVersion` node
   (to be added). If the running code expects a higher version than the DB contains,
   bonsai logs a warning and exits cleanly rather than writing corrupt state.

**Long-term**: evaluate LadybugDB's migration support as it matures; if it gains
`ALTER TABLE ADD COLUMN`, remove this constraint.


---

## 2026-04-19 — ML feature contract: `features_to_vector()` as shared training/inference path

**Decision**: A single module-level function `features_to_vector(Features) -> np.ndarray` in
`python/bonsai_sdk/ml_detector.py` is the exclusive encoding path for both training data
export and live inference.

**Rationale**: The most common failure mode in applied ML on streaming systems is maintaining
two feature pipelines — one in pandas for training, one in Python dicts for inference — that
drift apart. Models that worked in the notebook fail silently in production. By enforcing a
single function as the contract, training rows and inference vectors are always identical.
The `Features` dataclass is the API boundary; `features_to_vector()` is the serialiser.

**Consequences**: Any new feature added to `Features` must also be added to
`features_to_vector()` and models must be retrained. Feature vector layout is append-only —
prepending or reordering breaks existing models without a version bump.

---

## 2026-04-19 — Model sequencing: A before C before B

**Decision**: Phase 5 trains three models in order: A (IsolationForest anomaly detector),
C (GBT remediation classifier), B (LSTM sequence predictor).

**Rationale**:
- Model A requires only normal + anomaly windows, accumulates from day one of Phase 4.
  IsolationForest needs no GPU, trains in seconds, interpretable. Start here.
- Model C requires labelled Remediation nodes (action + success/failed status). These
  accumulate naturally as Phase 4 auto-remediation runs. Train once ~50+ remediations exist.
- Model B requires enough *failure precursor sequences* (N samples before each critical event).
  This needs deliberate fault injection over weeks. Train last.
Training B before A wastes effort on data that doesn't exist yet.

---

## 2026-04-19 — MLDetector integrated as a standard Detector in RuleEngine

**Decision**: `MLDetector` implements the same `Detector` ABC as rule-based detectors.
`RuleEngine` scans `model_dir` at startup and appends loaded `MLDetector` instances to
`self._rules`. If no model files are found the engine starts in rules-only mode without error.

**Rationale**: Keeping ML and rules under the same dispatch loop means: (1) the engine
doesn't know or care whether a detector is rules-based or ML, (2) ML detections follow the
exact same path to `DetectionEvent` write and `RemediationExecutor` as rule detections,
(3) model upgrades are a file drop — replace `models/anomaly_v1.joblib`, restart engine.

**Consequences**: `MLDetector.extract_features()` must do graph queries itself (no feature
sharing with co-firing rule detectors). Acceptable at lab scale; at fleet scale, a shared
feature cache per event would avoid duplicate graph reads.

---

## 2026-04-19 — Phase 6 HTTP server: Axum in-process over FastAPI

**Decision**: Phase 6 HTTP/SSE server is implemented in Rust (Axum 0.8) running
in-process alongside the Tonic gRPC server, not as a separate FastAPI process.

**Alternatives considered**: FastAPI (Python), separate Go HTTP proxy.

**Rationale**:
- Axum shares the same `Arc<GraphStore>` and `broadcast::Sender<BonsaiEvent>` as
  Tonic — SSE subscribers receive events with zero extra serialization (no gRPC hop).
- FastAPI would require 3 serialization round-trips per SSE event: BonsaiEvent →
  protobuf → gRPC → Python → JSON → HTTP. At fleet scale this is significant overhead.
- Both Axum and Tonic run on the same tokio runtime — no cross-runtime contention.
- Long-term scalability: Axum handles tens of thousands of concurrent SSE connections
  without a GIL. FastAPI is capped by Python threading for CPU-bound work.
- Single binary, single process, single port pair (50051 gRPC + 3000 HTTP). No
  inter-process coordination, no credential duplication.

**Endpoints**:
- `GET /api/topology` — devices, LLDP links, BGP sessions, computed health
- `GET /api/detections?limit=N` — recent DetectionEvents + Remediations
- `GET /api/trace/:id` — closed-loop trace steps for one DetectionEvent
- `GET /api/events` — SSE stream of live BonsaiEvents (BroadcastStream)
- `/*` fallback — Svelte SPA static files from `ui/dist/`

**Constraint**: Phase 6 UI is view-only. No config writing to devices, no admin
features, no authentication beyond TLS. Onboarding (Phase 6.1) means "add a device
to bonsai's monitoring scope via AddDevice RPC" — not writing config to the device.

---

## 2026-04-19 — BGP peer_as write-through bug: ON_CHANGE clobber fix

**Decision**: `write_bgp_neighbor` only updates `peer_as` on MERGE ON MATCH when the
incoming value is non-zero. Previously, ON_CHANGE notifications for session-state
transitions (e.g. idle→established after remediation) omitted `peer-as`, causing
`json_i64(val, "peer-as")` to return 0 and clobber the stored value.

**Fix**: Construct the MERGE cypher dynamically — include `n.peer_as = $peer_as` in
the ON MATCH SET clause only when `peer_as != 0`. ON CREATE always sets it (correct
for initial population). The pattern generalises: any field that arrives only on
initial advertisement should use conditional ON MATCH writes.

**Why this matters**: The same class of bug affects any field that gNMI devices send
once at session start but not on subsequent ON_CHANGE updates. Review other MERGE
statements if similar staleness bugs are observed on other fields.

---

## 2026-04-19 — Phase 6 UI: Svelte + Vite + D3

**Decision**: Phase 6 SPA uses Svelte 5 + Vite + D3-force for the topology graph.

**Rationale**: Minimum build-system complexity. Svelte compiles to vanilla JS with no
runtime framework overhead. D3-force gives a physics-based graph layout with zoom/pan
in ~100 lines. The built output in `ui/dist/` is served as static files by Axum's
ServeDir — no separate web server needed.

**Three views**:
1. Topology — D3-force graph (zoom/pan/drag), health-colored nodes (green/yellow/red),
   LLDP link hover shows interface names, BGP peer table below. Auto-refreshes 15s.
2. Events — SSE consumer on `/api/events`, live reverse-chronological feed,
   pause/clear, "View trace →" link on each event with a state_change_event_id.
3. Trace — Fetches `/api/trace/:id`, shows trigger → detection → remediation timeline.

**Build**: `cd ui && npm run build` → `ui/dist/`. `ui/dist/` and `ui/node_modules/`
are gitignored (reproducible from source). Committed: all src/ files, package.json,
vite.config.js, svelte.config.js.
