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

---

## 2026-04-20 — Event bus seam: `InProcessBus` over tokio broadcast

**Decision**: Inter-component event fan-out uses a small `EventBus` trait with an
`InProcessBus` implementation backed by `tokio::sync::broadcast`. Subscribers publish
`TelemetryUpdate` events to the bus; downstream consumers subscribe independently.

**Alternatives considered**: Keep the original single `mpsc` channel from subscriber
to graph writer, introduce NATS/Kafka immediately, or use a trait-less direct
`broadcast::Sender` everywhere.

**Rationale**:
- The original `mpsc` shape hard-wired one producer to one consumer. Adding archive,
  metrics, or external sinks would have required touching the subscriber path each time.
- `broadcast` provides the needed one-to-many fan-out today with minimal operational
  overhead and no extra infrastructure, which matches the single-host Phase 2 scope.
- The trait wrapper preserves a migration seam for a future distributed bus without
  leaking transport details throughout the codebase.
- Going straight to NATS/Kafka would add operational breadth before the local graph
  and archive pipeline are stable, which violates the project’s scope discipline.

---

## 2026-04-20 — Counter write debounce: 10 seconds per `(device, interface)`

**Decision**: `InterfaceStats` writes to the graph are debounced to at most once every
10 seconds for each `(device, interface)` key. State-transition events (BGP, BFD, LLDP,
interface oper-status) bypass the debounce and write immediately.

**Alternatives considered**: No debounce, a global debounce across all counters,
shorter intervals such as 1–5 seconds, or longer intervals such as 30–60 seconds.

**Rationale**:
- Counter telemetry is the highest-volume stream and was the main source of graph
  write pressure; debouncing reduces lock contention without sacrificing topology or
  state-transition fidelity.
- The per-`(device, interface)` scope keeps hot interfaces from suppressing updates on
  unrelated links. A global debounce would create cross-interface coupling and hide data.
- Ten seconds is long enough to collapse flood-style counter churn but short enough to
  preserve useful utilisation and error-rate trends at lab scale.
- State transitions represent control-plane truth changes, not rolling counters, so they
  must stay immediate even under load.

---

## 2026-04-20 — Retention count-cap semantics: exact oldest-event deletion

**Decision**: Count-based retention deletes the exact oldest `StateChangeEvent` IDs
needed to return under `max_state_change_events`. Ties on `occurred_at` are resolved by
selecting the oldest limited ID set first and deleting only those rows.

**Alternatives considered**: Age-based retention only, deleting all rows at or before
the timestamp of the excess-th oldest event, or disabling count caps until Parquet
archive exists.

**Rationale**:
- A hard count cap is still useful before the Parquet archive lands because it bounds
  graph growth independently of wall-clock age and protects long-lived lab runs.
- Timestamp-based cutoff deletion is simpler but can over-delete when multiple events
  share the same timestamp, which distorts training history unevenly.
- Exact ID deletion is deterministic and preserves the intended retention budget even
  during bursty event storms.
- Detection and remediation history remain outside this cap; only `StateChangeEvent`
  hot history is pruned.

---

## 2026-04-20 — Playbook verification field: `expected_graph_state` is canonical

**Decision**: Playbook verification queries use `expected_graph_state` as the canonical
field name. `cypher` remains supported as a legacy alias for backward compatibility with
older or hand-written playbooks.

**Alternatives considered**: Keep `cypher` as the canonical field, support only one name
and break older playbooks, or defer verification entirely and accept unverified
remediation outcomes.

**Rationale**:
- `expected_graph_state` is semantically clearer: it names the purpose of the query
  rather than the query language alone.
- Keeping the legacy alias avoids needless breakage while the playbook catalog is still
  young and likely to have hand-authored variants.
- Verification cannot be optional in practice because remediation success labels feed
  Model C training. Silent “success” without a graph check corrupts the dataset.
- This choice lets the catalog converge on one documented shape without forcing an
  immediate cleanup of every historical YAML.

---

## 2026-04-20 — Shared feature extraction split: unconditional ML, gated rules

**Decision**: `extract_features_for_event()` in `ml_detector.py` is the canonical event
feature extractor. `MLDetector` calls it unconditionally for every event; rule detectors
perform fast event/rule gating and then call the shared extractor before applying any
rule-specific thresholds or windows.

**Alternatives considered**: Separate feature extraction paths for ML and rules, keep
all rule-specific inline extraction logic, or build a per-event shared cache inside the
engine before the rule/ML split.

**Rationale**:
- Training/inference skew is a real failure mode; one canonical extractor keeps field
  population logic in one place and makes regressions testable.
- ML needs a dense, always-on feature path because the model decides whether an event is
  anomalous; skipping extraction based on rule semantics would bias the model input.
- Rules still benefit from pre-extraction gating so they can cheaply ignore unrelated
  events and preserve their explicit semantics.
- A richer shared cache may be worthwhile later, but the current split solves the drift
  problem without introducing a heavier engine redesign.

---

## 2026-04-20 — Typed defaults for training rows via `typing.get_type_hints()`

**Decision**: Empty/default feature rows in the training pipeline are generated using
`typing.get_type_hints(Features)` so each field gets a type-correct default (`0`, `0.0`,
`""`, `{}`) even under `from __future__ import annotations`.

**Alternatives considered**: Maintain a handwritten default map, use dataclass field
metadata without resolving postponed annotations, or tolerate mixed dtypes and clean
them later in pandas/pyarrow.

**Rationale**:
- Postponed annotations turn field types into strings at runtime; naive inspection can
  default numeric columns to `""`, which poisons Parquet schema inference.
- Resolving type hints from the source dataclass keeps the defaulting logic coupled to
  the actual `Features` contract instead of a second manually synchronized map.
- Type-correct defaults preserve stable Arrow/Parquet schemas and make downstream ML
  exports predictable.
- Cleaning bad dtypes later is the wrong layer; the exporter should emit structurally
  correct rows at the source.

---

## 2026-04-20 — Cold archive format: collector-local parquet batches off the event bus

**Decision**: Raw `TelemetryUpdate` events are archived from the event bus into Parquet
files on local disk. The archive is collector-local in the future distributed design;
today it is core-local only because the current deployment is single-process and
single-host. Default flush cadence is 10 seconds with an early flush at 1000 rows.

**Alternatives considered**: Keep raw events only in the graph, archive directly from
the graph after retention pruning, or introduce a remote object store / TSDB before a
local Parquet layer exists.

**Rationale**:
- The graph is the hot state store, not the long-range raw-history substrate. Training
  needs more history than the bounded `StateChangeEvent` graph should retain.
- Archiving off the event bus keeps the cold path additive: the graph writer and archive
  subscriber both observe the same decoded `TelemetryUpdate` stream without coupling the
  graph schema to archival needs.
- Local Parquet is the simplest useful format: columnar, compressible, readable from
  Python/pandas, and operationally aligned with the single-host phase.
- Ten seconds is short enough to keep data fresh for inspection while avoiding a file
  per event burst. The row-count flush cap prevents large in-memory buffers during
  sustained telemetry floods.
- In a future distributed topology, each collector should write its own local archive
  shard with the same schema and partitioning. No central archive coordinator is needed
  for v1.

---

## 2026-04-20 — Python tooling and live lab operations are WSL-first with a repo-local `.venv`

**Decision**: Python tooling for Bonsai runs from a project-local `.venv/` created
inside WSL. Dependency declarations stay in `python/pyproject.toml`. Chaos tooling,
fault injection, and any `clab`-dependent workflows are executed from WSL because the
live ContainerLab lab and `clab` binary live there on this machine.

**Alternatives considered**: Install Python packages into machine-global Windows
interpreters, rely on the Codex bundled runtime for project dependencies, or keep a
Windows-hosted venv and shell out across the Windows/WSL boundary for `clab`.

**Rationale**:
- The live lab is hosted in WSL, so Windows Python cannot be the trusted execution
  environment for `clab tools netem` and related lab-side actions.
- A repo-local `.venv/` makes Python dependencies reproducible, isolated, and visible
  to every contributor without coupling the project to one developer's global package
  state.
- `python/pyproject.toml` already expresses the dependency contract; the venv is the
  installation target, not a second dependency specification.
- Keeping Rust on the existing Windows `--release` path avoids churn in the validated
  build workflow while still moving the lab-facing Python path to the environment where
  the lab actually runs.

---

## 2026-04-20 — Model C remediation training excludes pre-T0-2 outcomes

**Decision**: Model C training data uses a hard remediation cutoff at
`2026-04-20T09:32:50Z`, the timestamp of commit `4a5cd707b7e59aa77d3f08a0bffb7a0c3ec72189`
(`T0-1 + T0-2: fix playbook catalog path and verification field name`). Remediation
rows with `attempted_at <= cutoff` are considered untrustworthy for training because
they were recorded before `verify()` produced real success/failure labels.

**Alternatives considered**: Keep all historical Remediation rows, manually scrub old
rows from the graph, or add a new `trustworthy` property to the schema and migrate all
historical data.

**Rationale**:
- The verification fix changed the semantic meaning of `Remediation.status`; old rows
  can overstate success and would poison Model C if left unfiltered.
- A dated cutoff is the smallest honest move: it is explicit, auditable, and works with
  the current schema without a graph migration.
- Filtering in both readiness checks and the training path prevents stale data from
  silently inflating counts or leaking into the classifier.
- A future schema-level `trustworthy` flag may still be worthwhile, but the cutoff gets
  the project back to truthful training data immediately.

---

## 2026-04-20 â€” Remediation trust is explicit in the graph via sidecar marks

**Decision**: trust for remediation outcomes is represented in-graph using a
`RemediationTrustMark` node linked to each `Remediation` by `TRUST_MARKS`. Startup
backfills marks for legacy rows using the existing `2026-04-20T09:32:50Z` cutoff, and
new remediations are marked at write time.

**Alternatives considered**: continue filtering only in Python training code, delete all
pre-cutoff remediation rows, or alter the `Remediation` node schema in place to add a
new property.

**Rationale**:
- The previous cutoff-only approach was honest for training, but the graph itself still
  looked authoritative even though historical rows were semantically tainted.
- A sidecar mark makes trust explicit without risking an in-place mutation of the
  existing `Remediation` table on LadybugDB.
- Backfilling on startup keeps legacy databases truthful without a one-off migration
  command.
- Training readiness and Model C export can now query trusted remediations directly from
  graph state instead of carrying the trust boundary only as an application convention.

---

## 2026-04-20 - Dynamic device registry uses a local JSON backing file in v1

**Decision**: the runtime `ApiRegistry` persists managed devices in a local
`bonsai-registry.json` file, seeded from `bonsai.toml` on startup and then mutated
through Bonsai gRPC CRUD calls. The registry file is gitignored and treated as local
runtime state rather than source-controlled configuration.

**Alternatives considered**: add a dedicated SQLite registry database immediately,
keep managed-device state only in memory, or continue using `bonsai.toml` as the only
source of truth and require a restart for every device change.

**Rationale**:
- JSON is the lightest durable store that closes T1-3a without introducing a second
  embedded database beside LadybugDB.
- Seeding from `bonsai.toml` preserves today's static-target workflow while allowing
  API-added devices to survive a process restart.
- Keeping the registry local and gitignored avoids leaking lab-specific env-var names
  or CA file paths into committed project configuration.
- If onboarding later needs richer queries or relationships, the JSON-backed seam can
  be replaced with SQLite without changing the gRPC surface or subscriber manager
  contract.

---

## 2026-04-21 - Device discovery is a probe-only gNMI Capabilities RPC

**Decision**: `DiscoverDevice` probes a candidate device with gNMI Capabilities and
returns a structured report without mutating the runtime registry. The RPC accepts
credential environment variable names only, never plaintext credential values. Discovery
now reads file-backed path profiles when available and retains the original built-in
recommendation logic as a fallback for missing or malformed local profile files.

**Alternatives considered**: make discovery automatically add devices to the registry,
allow plaintext credentials in the request for convenience, or block `T1-3c` until the
path-profile YAML templates exist.

**Rationale**:
- Probe-only behavior keeps operator intent explicit: discovery answers "what is this
  device and what should we subscribe to?" while `AddDevice` remains the lifecycle
  mutation.
- Env-var-only credentials keep the API aligned with the project rule that credentials
  must not appear in source or committed files.
- File-backed recommendations keep the Capabilities/reporting seam operator-visible
  while the built-in fallback keeps local discovery usable if profiles are temporarily
  broken during development.
- Reusing model-family rules avoids vendor branching by config string and keeps
  OpenConfig/native path choice tied to what the device actually advertises.

---

## 2026-04-21 - Local runtime ownership is Windows core, WSL lab

**Decision**: On this workstation, Bonsai's Rust core/API/UI process runs from Windows
PowerShell, while the live ContainerLab lab and lab-affecting Python tools run from WSL.
Repo helper scripts are the canonical interface for repeated local tasks:
`start_bonsai_windows.ps1`, `stop_bonsai_windows.ps1`, `check_dev_env.ps1`,
`search_repo.ps1`, and `regenerate_python_stubs.ps1`.

**Alternatives considered**: run Bonsai itself from WSL, rely on ambiguous PATH commands
such as bare `python`, `python3`, and `rg`, or keep using one-off Start-Process commands.

**Rationale**:
- Bonsai has already been validated on the Windows Rust `--release` path, and Windows is
  where the UI/API process is expected to listen for local operator access.
- ContainerLab, `clab`, and `netem` live in WSL, so lab mutation must stay there.
- The local PATH contains unreliable entries: the Chocolatey `rg.exe` shim can fail with
  access denied, and sandboxed shells may not execute user Python without explicit
  permission even though the interpreter exists.
- Durable scripts make tool resolution explicit and keep future sessions from
  rediscovering the same environment boundary.

---

## 2026-04-21 - Discovery path recommendations are YAML-backed templates

**Decision**: `DiscoverDevice` path recommendations are driven by YAML profile files
under `config/path_profiles/`. Each path declares its gNMI path, origin, subscription
mode, sample interval, rationale, and required model gates. Discovery loads the profiles,
selects by role, filters paths against advertised Capabilities models, and reports
warnings for unsupported paths that were dropped.

**Alternatives considered**: keep path recommendations hardcoded in Rust, wait until a
future UI wizard exists before adding templates, or store profiles in TOML/JSON to avoid
a YAML parser.

**Rationale**:
- Templates make onboarding behavior inspectable and editable without changing Rust code.
- Model gates preserve the project rule that path selection follows what the device
  advertises, not a vendor string alone.
- YAML is the backlog-requested operator-facing format and is easier to read for path
  catalog work than JSON.
- The previous Rust built-in recommendations remain as a fallback so malformed or
  missing template files do not make discovery unusable during local development.

---

## 2026-04-21 - Subscription path health is explicit graph state

**Decision**: Bonsai records subscription path health in graph `SubscriptionStatus`
nodes linked from `Device` by `HAS_SUBSCRIPTION_STATUS`. Subscribers publish their
actual path plan after a successful Subscribe RPC. A verifier task watches the telemetry
bus for 30 seconds and marks each path as `pending`, `observed`, or
`subscribed_but_silent`.

**Alternatives considered**: infer subscription health only from logs, write status only
inside the subscriber task, or treat missing telemetry as a subscriber connection error
instead of path-level graph state.

**Rationale**:
- The operator needs to know which subscribed paths are truly producing data, not just
  whether the gNMI stream is connected.
- Keeping status in graph state makes the honesty layer queryable by API, UI, and later
  CLI commands without scraping logs.
- The verifier observes the same event bus as the graph/archive consumers, so it stays
  additive and does not sit in the hot subscriber write path.
- Event-family matching handles vendor response shape differences while still preserving
  path-level accountability.

---

## 2026-04-21 - Distributed collector lands as runtime modes plus ingest seam

**Decision**: T1-2 starts with one Bonsai binary and three runtime modes: `all`,
`core`, and `collector`. `all` preserves today's single-process behavior. `core`
runs the graph/API/UI side and exposes a client-streaming `TelemetryIngest` RPC.
`collector` runs local gNMI subscribers and forwards decoded telemetry updates to
the configured core endpoint. The core republishes ingested updates onto its local
event bus so graph, rule, and status consumers do not need a second write path.

**Alternatives considered**: split the repository into separate binaries, introduce
NATS/Kafka before there is a remote collector, or delay all T1-2 work until mTLS,
zstd compression, and disk-backed queues could land together.

**Rationale**:
- A mode switch keeps deployment simple while making the collector/core boundary
  explicit and testable.
- Reusing the event bus on both sides preserves the additive subscriber architecture:
  local gNMI and remote collector ingest feed the same downstream consumers.
- `all` mode keeps the Windows lab workflow stable while `core` and `collector`
  allow distributed validation to start incrementally.
- gRPC zstd compression is deferred because enabling tonic's zstd feature on this
  Windows/MSVC build links `zstd-sys` alongside LadybugDB's bundled `zstd.lib`,
  causing duplicate symbol failures. The seam is intentionally left ready for a
  follow-up compression slice once the link strategy is chosen.

---

## 2026-04-21 - Managed device addresses are validated before persistence

**Decision**: The runtime registry accepts only explicit `host:port` device
addresses. Hosts may be DNS-style hostnames, IPv4 addresses, or bracketed IPv6
addresses; ports must parse as `1..=65535`. Invalid input fails before writing
`bonsai-registry.json` with the operator-facing message `device address must be
host:port`.

**Alternatives considered**: continue accepting arbitrary strings and let the
subscriber surface connection errors later, require IP literals only, or resolve
hostnames during validation.

**Rationale**:
- Onboarding should catch malformed input at Save time, not after the subscriber
  loop starts and emits a transport error.
- Hostnames remain valid because lab and production inventories often use DNS
  names rather than management IPs.
- Validation does not perform DNS resolution, keeping registry writes fast,
  deterministic, and usable before the lab or network is reachable.

---

## 2026-04-21 - Collector ingest values use MessagePack bytes

**Decision**: `TelemetryIngestUpdate` now carries telemetry values as
`bytes value_msgpack = 7`. The bytes are MessagePack-encoded
`serde_json::Value` payloads. The Rust collector/core conversion layer owns
the encode/decode boundary, and generated Python gRPC stubs are regenerated
from the same proto so external consumers see the binary field name and type.

**Alternatives considered**: keep the JSON string field until distributed
hardening, use `google.protobuf.Any`, add a custom value `oneof`, or add
compression before changing the value encoding.

**Rationale**:
- MessagePack preserves Bonsai's current schemaless telemetry value model
  without requiring a custom proto value taxonomy before the normalized graph
  model is fully stable.
- Binary encoding removes JSON text overhead for numeric counter values while
  keeping the distributed ingest seam simple and testable.
- This is intentionally a protocol break while collector/core callsites are
  still few; changing it after disk queues and compression would create a more
  expensive migration.
- The value-field change reduces encoded value bytes and total protobuf bytes,
  but repeated per-update metadata such as `target` and `path` still dominate
  scalar counter messages. Larger stream-level reductions belong in the later
  T2 compression/queue/batching work.

---

## 2026-04-21 - Local credential vault stores aliases, not secrets, in APIs

**Decision**: Bonsai stores reusable device credentials in a local
passphrase-encrypted `age` vault under `bonsai-credentials/vault.age`, with
plaintext `metadata.json` containing only aliases and timestamps. Devices may
reference `credential_alias`; subscriber startup, discovery, and remediation
resolve credentials in-process using this order: vault alias, environment
variables, then inline lab-only config. gRPC, HTTP, UI, and Python APIs can
list/add/remove aliases, but list responses never include usernames or
passwords. The current unlock mechanism is the environment variable
`BONSAI_VAULT_PASSPHRASE`.

**Alternatives considered**: keep env-var-only credentials, store credentials
inline in `bonsai-registry.json`, use a remote secret manager, implement a
lower-level AES-GCM vault directly, or delay vault integration until the full
onboarding wizard exists.

**Rationale**:
- Alias-based credentials make onboarding usable for multiple devices without
  restarting Bonsai or creating per-device process environment variables.
- The threat model is local disk snooping: encrypted `vault.age` protects
  secrets at rest, while `metadata.json` is intentionally non-secret. This does
  not protect against a compromised Bonsai process or host memory inspection.
- Secrets stay in Rust process memory and flow only to gNMI client calls; HTTP,
  gRPC, Python, and UI list operations return alias metadata only.
- Env vars and inline lab config remain valid fallbacks for headless and lab
  workflows, preserving existing deployments while making the safer path
  available.
- Using `age` avoids inventing cryptography and keeps the vault format simple
  enough for a v1 single-host Bonsai deployment. Remote stores and key rotation
  remain out of scope until real operator demand appears.

---

## 2026-04-21 - Site is first-class graph state

**Decision**: Bonsai represents operator sites as `Site` nodes with stable IDs,
display names, parent IDs, kind labels, optional coordinates, metadata JSON, and
`PARENT_OF` hierarchy edges. Managed devices remain configured with a
human-facing `TargetConfig.site` string for now; startup and registry add/update
paths migrate that string into a `Site` node with `kind = "unknown"` when needed
and link the device with `Device-[:LOCATED_AT]->Site`. Site management is exposed
through gRPC, HTTP, Python, and a minimal onboarding picker.

**Alternatives considered**: keep `site` as an opaque device attribute until the
wizard rewrite, require operators to pre-create sites before adding devices, or
store site hierarchy only in the registry JSON.

**Rationale**:
- Putting sites in the graph makes locality queryable by the same Cypher
  traversal model as devices, interfaces, BGP neighbors, detections, and
  remediations.
- Keeping `TargetConfig.site` as a string alias avoids a registry migration and
  lets existing `bonsai-registry.json`/`bonsai.toml` entries self-heal into graph
  sites on startup.
- Existing string sites get `kind = "unknown"` so no operator data is lost while
  the later onboarding wizard adds richer site creation and editing affordances.
- `LOCATED_AT` is rewired on each registry sync so moving a device between sites
  does not leave multiple active location edges.
- Site ACLs, map visualization, and automatic site inference remain out of
  scope for v1; sites are operational graph metadata, not a security boundary.

---

## 2026-04-21 - Onboarding wizard persists operator-selected subscription paths

**Decision**: Discovery recommendations now carry per-path `optional` metadata,
and managed devices may persist `selected_paths` in the runtime registry. The
subscriber still performs Capabilities detection for encoding and vendor labels,
but when a non-empty selected path plan exists it builds the gNMI Subscribe
request from that operator-approved plan instead of deriving paths solely from
Capabilities. The HTTP onboarding facade exposes this through
`POST /api/onboarding/devices/with_paths`; the Svelte onboarding workspace is a
four-step wizard with a separate managed-device list.

**Alternatives considered**: keep path selection UI-only until a later runtime
refactor, store only the profile name and recompute paths on every restart, or
replace Capabilities-derived fallback paths entirely.

**Rationale**:
- A wizard path checklist is only useful if the runtime honors the selected
  paths. Persisting the concrete path list makes the saved operator intent
  visible and restart-safe.
- Required/optional metadata belongs in discovery output because the YAML
  profile already owns that knowledge; the UI should not infer it from path
  names.
- Keeping Capabilities detection in the subscriber preserves encoding selection
  and safe fallback behavior for legacy registry entries that do not yet have
  `selected_paths`.
- Storing concrete paths instead of just profile names avoids surprises if YAML
  templates change after a device has already been onboarded.

---

## 2026-04-21 - Device stop/start is registry state, not deletion

**Decision**: Managed devices now carry an `enabled` flag in the runtime
registry. Disabled devices remain visible and editable, but the subscriber
manager skips them at startup and stops any running subscriber when an update
sets `enabled = false`. Bulk Stop/Start/Restart in the UI and
`bonsai device stop|start|restart` in the CLI update this same flag instead of
removing registry entries.

**Alternatives considered**: implement stop as device removal, keep stop/start
as UI-only buttons with no persisted state, or add a separate in-memory
subscriber control plane that would be lost on restart.

**Rationale**:
- Operators need a maintenance state that survives a Bonsai restart; deleting a
  device loses onboarding metadata and selected paths.
- Reusing registry update events keeps lifecycle control in the same path as
  device edits, avoiding a second subscriber-control mechanism.
- Restart is represented as an update with `enabled = true`, which intentionally
  reuses the existing stop-then-spawn behavior for updated targets.
- Removal confirmation reports subscription and remediation-trust impact, but
  it does not physically delete graph history. Bonsai's graph remains
  operational history; the registry only controls active management.

---

## 2026-04-22 - Tonic zstd uses shared Ladybug on Windows

**Decision**: Collector-to-core `TelemetryIngest` uses tonic's native zstd
compression. On Windows/MSVC, Bonsai builds LadybugDB as `lbug_shared.dll` by
setting `LBUG_SHARED=1` in `.cargo/config.toml`. The root build script copies
that DLL into `target/release` so standalone release binaries can run without
manual PATH changes.

**Alternatives considered**: use gzip, implement a Bonsai-specific compressed
protobuf envelope, pin older zstd crate versions, or build Ladybug against a
system zstd with `BUNDLE_ZSTD=OFF`.

**Rationale**:
- Gzip was rejected because the ingest stream is hot-path telemetry; zstd gives
  better compression/CPU tradeoffs for the long-term collector/core seam.
- Tonic zstd is preferable to a custom envelope because it keeps compression at
  the transport layer and avoids a protocol compatibility fork before the disk
  queue and batching work.
- The static Ladybug build bundles `zstd.lib`; enabling tonic zstd also pulls
  `zstd-sys`, producing duplicate zstd symbols in the Windows executable link.
  Shared Ladybug keeps those Ladybug-bundled native symbols outside Bonsai's
  executable link unit.
- `BUNDLE_ZSTD=OFF`/system zstd is not the current path because this machine
  does not have a stable pkg-config/vcpkg zstd setup, and adding one would make
  local onboarding more fragile than the shared-Ladybug switch.
- Copying `lbug_shared.dll` during the Bonsai build preserves the normal
  `target/release/bonsai.exe` workflow after `cargo build --release`.

---

## 2026-04-22 - Collector ingest uses an append-only disk queue

**Decision**: Collector mode persists decoded telemetry to an append-only local
queue before forwarding it to the core. The queue stores records in
`queue.dat` as little-endian `u32 payload_len`, little-endian `i64
enqueued_unix_ns`, then a prost-encoded `TelemetryIngestUpdate`; `queue.ack`
stores the byte offset of the last core-accepted record. Reconnect replay sends
FIFO batches through tonic zstd and advances `queue.ack` only after the core
returns an accepted ingest response.

**Alternatives considered**: keep the in-memory mpsc stream and accept loss
during outages, use sled/RocksDB, or wait for a broader batching protocol.

**Rationale**:
- The collector must keep subscribing while the core is unavailable; placing
  the bus-to-disk writer outside the gRPC connection loop prevents outage-time
  telemetry loss.
- A simple append-only file is easier to inspect and recover than an embedded KV
  database, and it is enough for the single-host/lab-scale v1 constraint.
- Acking only after the core response favors at-least-once delivery over silent
  loss. If a stream fails before response, records remain queued and may replay.
- Retention is explicit and local: `[collector.queue]` controls path,
  `max_bytes`, `max_age_hours`, `drain_batch_size`, and
  `log_interval_seconds`. Expired or over-budget records are dropped only during
  queue compaction and are logged loudly.
- T2-4 remains responsible for the long live two-process outage run; this slice
  provides the durable mechanism and focused restart/retention tests.

---

## 2026-04-22 - Distributed ingest mTLS is optional but strict when enabled

**Decision**: The collector-to-core `TelemetryIngest` channel supports optional
mutual TLS through `[runtime.tls]`. Core mode uses `cert`/`key` as the server
identity and `ca_cert` as the required client trust root. Collector mode uses
`ca_cert` to verify the core and presents `cert`/`key` as its client identity;
`server_name` overrides endpoint-host verification when the lab connects by IP.

**Alternatives considered**: leave distributed ingest unauthenticated until a
later production hardening pass, use server-only TLS, or add token-based
collector authentication.

**Rationale**:
- The distributed seam accepts graph-writing telemetry; unauthenticated ingest
  is too easy to spoof even in a lab.
- mTLS matches the network-control-plane shape better than bearer tokens:
  collectors prove identity during handshake, before any telemetry stream is
  accepted.
- TLS remains optional so single-process `mode = "all"` and local development do
  not need certificates.
- One lab CA is enough for v1. It signs the core server certificate and all
  collector client certificates; richer per-site CA hierarchy is intentionally
  deferred.
- Live valid-cert and no-cert handshake proof is grouped with T2-4 so the final
  two-process validation exercises mTLS, zstd compression, and queue replay
  together against the lab.

---

## 2026-04-22 - Distributed transport validation is a separate milestone from healing/archive validation

**Decision**: T2-4 closes when the distributed collector/core transport is
proven against the live lab: Windows collector, Windows core, WSL-hosted lab
targets, disk-backed outage queue, replay, zstd compression, mTLS, and graph
ingest. Remediation/healing-loop validation and archive parity remain separate
backlog validations because they exercise different subsystems.

**Alternatives considered**: block T2-4 until archive parity and the full
detect-heal loop are exercised in the same run, or mark only unit tests as
sufficient for the distributed seam.

**Rationale**:
- The distributed seam has its own failure modes: Windows-to-WSL reachability,
  core outage buffering, reconnect replay, TLS identity, compression, and graph
  ingestion. These are now proven together with real lab telemetry.
- Combining transport, archive parity, and closed-loop healing in one gate would
  make failures ambiguous and slow the backlog. Keeping transport as its own
  milestone gives us a clean regression point.
- The 2026-04-22 run reached all four lab gNMI targets, queued 1,474 records
  while the core was offline, replayed 3,314 records through zstd+mTLS, and
  produced graph writes for SR Linux and IOS-XRd.
- The wrong-CA collector smoke forced a real ingest RPC and delivered zero
  records, with zero core accept events for the bad collector identity, so mTLS
  protects the graph-writing ingest stream.

---

## 2026-04-22 - Archive writes one Parquet file per target per hour

**Decision**: The telemetry archive uses an append-to-current-hour layout. Each
collector process keeps one open Parquet `ArrowWriter` per `(target, hour)`
partition and appends each flush as another row group. Writers close when the
stream advances into a later hour, when the event bus closes, or when Bonsai
receives its graceful shutdown signal.

**Alternatives considered**: keep one Parquet file per flush and add a later
compaction job, or close/reopen the same hourly path on every flush.

**Rationale**:
- Keeping open hourly writers fixes the small-file explosion at the source:
  five flushes across four targets in the same hour now produce four files, not
  twenty.
- A compaction job would be simpler to bolt on but doubles I/O and leaves the
  archive inefficient until compaction catches up.
- Parquet files cannot be safely appended after their footer is closed. On
  process restart within the same hour, Bonsai creates a `__part-NN` file
  instead of overwriting or corrupting the existing closed file.
- Active-hour files become fully readable when closed at hour rollover or
  graceful shutdown. An unclean process kill can still leave the current open
  file without a footer; that is acceptable for this lab-scale archive and is
  consistent with the project's explicit no-production-WAL scope for v1.
- Close logs report final file size, total raw bytes, rows, and compression
  ratio so archive efficiency is visible without adding another metrics slice.

---

## 2026-04-22 - Credential vault passphrase rotation is manual in v1

**Decision**: Bonsai's local credential vault does not support in-place
passphrase rotation in v1. Rotating `BONSAI_VAULT_PASSPHRASE` requires the
operator to unlock with the old passphrase, re-add or export/re-import the
credential aliases under a vault opened with the new passphrase, and restart
Bonsai with the new environment.

**Alternatives considered**: add a `RotateCredentialVaultPassphrase` RPC now,
write a one-off migration command, or defer rotation until a broader secret
management abstraction exists.

**Rationale**:
- The vault is a local lab-scale store protecting secrets at rest, not a remote
  enterprise KMS. In-place rotation is useful, but it is not required for the
  current onboarding and collector validation milestones.
- Rotation touches the whole encrypted payload and needs careful operator UX so
  a failed rotation does not strand credentials or write secrets to logs.
- The manual workaround is acceptable for v0.x: start Bonsai with the old
  passphrase, add aliases into a fresh vault directory using the new passphrase,
  update `credentials.path` if needed, and restart.
- Deferring a rotation RPC keeps the current API smaller and leaves room for a
  future `CredentialStore` abstraction that can also cover remote secret stores.

---

## 2026-04-22 - Bonsai uses one container image for all runtime roles

**Decision**: Bonsai's container packaging starts with a single multi-stage
image built by `docker/Dockerfile.bonsai`. The image contains the release Rust
binary and built Svelte UI assets; runtime role remains a configuration choice
through `runtime.mode = "all" | "core" | "collector"` rather than a separate
image per role.

**Alternatives considered**: build separate `bonsai-core` and
`bonsai-collector` images, copy host-built binaries into a runtime image, or
defer containerization until Compose is designed.

**Rationale**:
- One image keeps core/collector version skew impossible during local
  distributed validation. Operators deploy the same artifact with different
  config and volume mounts.
- Multi-stage builds preserve reproducibility: Rust and Node toolchains stay in
  builder stages, while the runtime image is Debian slim plus `curl` for the
  healthcheck.
- The image runs as UID/GID 10001 and writes only to mounted Bonsai state
  directories under `/var/lib/bonsai`.
- The healthcheck targets the core/all HTTP readiness endpoint. Compose can
  override or disable it for collector-only roles, which do not serve the UI.
- Docker is a v0.x deployment plane for Bonsai; Kubernetes manifests remain
  explicitly out of scope.

---

## 2026-04-22 - Container runtime uses trixie and ships LadybugDB's shared library

**Decision**: The Bonsai Docker image builds on the Rust 1.91 Debian trixie
cargo-chef image and runs on Debian trixie slim. The runtime image explicitly
copies `liblbug.so.0` alongside the stripped Bonsai binary and sets
`LD_LIBRARY_PATH=/usr/local/lib`. The healthcheck uses BusyBox `wget` instead
of `curl`.

**Alternatives considered**: stay on bookworm, statically link LadybugDB inside
the container, install `curl` for the healthcheck, or disable the image
healthcheck until Compose exists.

**Rationale**:
- LadybugDB's Linux C++ build includes `<format>`, which requires a newer
  libstdc++ than Debian bookworm's default toolchain provides. Trixie gives the
  container a toolchain that matches the current dependency graph.
- The repo-local `LBUG_SHARED=1` avoids the zstd symbol conflict but means the
  runtime layer must carry `liblbug.so.0`; copying it from the builder makes the
  container self-contained.
- BuildKit cache mounts keep the expensive native C++/Rust build practical
  without committing the target directory into image layers.
- BusyBox keeps the readiness healthcheck available while bringing the Docker
  image below the 200 MB target in Docker's normal image listing.

---

## 2026-04-23 — Unpinned apt versions in Dockerfile for maintenance

**Decision**: Drop specific version pins for `apt` packages in `docker/Dockerfile.bonsai`.

**Rationale**:
- Debian trixie (testing) rotates package versions frequently. Pinned versions (e.g., `cmake=3.31.6-2`) often disappear from mirrors within months, causing builds to fail on unchanged source code.
- Reproducibility is better managed by pinning the base image digest rather than individual system packages.
- Maintenance overhead of updating pins every few months outweighs the marginal reproducibility benefit in a development-heavy phase.

**Done when**: `docker build` succeeds without version-not-found errors; base image digest fixes the repository state in effect.

---

## 2026-04-23 — cargo-chef --all-targets for cache stability

**Decision**: Use `cargo chef cook --all-targets` in the Docker build pipeline.

**Rationale**:
- The current build only warms the cache for the main `bonsai` binary. Adding additional binaries (e.g., `bonsai-device-cli`) would invalidate the cache and force a full rebuild.
- `--all-targets` ensures that the dependency cache includes all binaries, tests, and examples defined in `Cargo.toml`.
- This keeps the dev-loop and CI builds fast (under 30s for no-source-change rebuilds) even as the workspace grows.

---

## 2026-04-23 — Protocol version negotiation stub

**Decision**: Introduce a `protocol_version: uint32` field in the collector-core gRPC messages (`TelemetryIngestUpdate` and `TelemetryIngestResponse`).

**Rationale**:
- Collectors and core will inevitably version-skew in production.
- A version stub allows the core to detect and log warnings (or reject) incompatible connections early, before processing malformed telemetry.
- Starting with version 1 now provides the necessary hook for Tier 2/3 data-exchange contracts without a breaking change later.

**Versioning Policy**: Semantic versioning on protocol. Major bumps indicate incompatibility; minor bumps indicate backward-compatible additions.

---

## 2026-04-23 — Generic Graph Store Abstraction (BonsaiStore trait)

**Decision**: Implement a `BonsaiStore` trait to unify `GraphStore` (core) and `CollectorGraphStore` (collector). The trait is `#[tonic::async_trait]` compatible and used by the gRPC `BonsaiService` and background tasks (subscription verifier, site sync).

**Rationale**:
- Eliminates code duplication between core and collector graph handlers.
- Allows the same gRPC service implementation to run in both modes, enabling collectors to expose a local query/mutation API.
- Enables background tasks like the subscription verifier to operate seamlessly regardless of whether they are running on a core or a collector.
- Uses `tonic::async_trait` to handle the `dyn BonsaiStore` compatibility requirements for shared tasks.

---

## 2026-04-23 — Collector-Side Local Rule Execution Architecture

**Decision**: Run the rule engine as a standalone Python sidecar (`python/collector_engine.py`) alongside the Rust collector. The sidecar connects to the local collector's gRPC API to stream events and query the local graph.

**Rationale**:
- Moves detection logic closer to the data source, reducing core load.
- Preserves the Python-based rule ecosystem while leveraging Rust for high-performance telemetry ingestion.
- Enables "disconnected-ops" where detection continues even if the core is unreachable.
- Collector-local graph contains only the nodes/edges needed for detection (Device, Interface, BGP, etc.).

---

## 2026-04-24 — Per-collector mTLS certificates instead of shared collector cert

**Decision**: `scripts/generate_compose_tls.sh` now generates one client cert per collector ID (`collector-1-cert.pem`, `collector-2-cert.pem`, etc.) with CN=`bonsai-<collector-id>`. Each collector config references its own cert/key pair. The previous shared `collector-cert.pem` is no longer generated.

**Rationale**: A single shared collector cert means losing one collector's private key compromises the mTLS channel for all collectors. With per-collector certs, revoking a compromised collector is a matter of removing its cert from the CA trust bundle on the core; other collectors continue operating unaffected. The CN encodes the collector ID, so the core can log which collector authenticated on each connection. The cost is minimal: one extra `openssl req` + `x509` invocation per collector at setup time.

**Adding new collectors**: Add the collector ID to `COLLECTOR_IDS` in `generate_compose_tls.sh` and re-run with `--force`, or generate the cert manually. The collector's config must reference its own `<id>-cert.pem` / `<id>-key.pem`.

**Done when**: Each collector in `docker/configs/` references its own cert; the script documents the revocation procedure in its output.

---

## 2026-04-24 — Counter forward mode: summary as default in distributed compose profiles

**Decision**: The source-level default for `counter_forward_mode` remains `"debounced"` (in `src/config.rs`). The distributed and two-collector compose profiles (`docker/configs/collector-1.toml`, `docker/configs/collector-2.toml`) explicitly set `counter_forward_mode = "summary"` under `[collector.filter]`.

**Rationale**:
- `"debounced"` is the right conservative default for stand-alone operators: it forwards individual counter updates after a quiet period, giving full per-update fidelity without flooding the core.
- In distributed compose profiles, the bandwidth win from `"summary"` matters: collectors aggregate delta counters over a 60-second window and forward a single summary message instead of every raw update. This reduces the collector-to-core gRPC ingest volume significantly for high-rate counter paths.
- Keeping the source-level default as `"debounced"` means new operator deployments get conservative behavior automatically; the explicit per-profile override makes the distributed profile's intent self-documenting.

**Done**: `collector-1.toml` and `collector-2.toml` already carry `counter_forward_mode = "summary"` and `counter_window_secs = 60`.

---

## 2026-04-24 — Audience framing: controller-less networks as the primary target

**Decision**: Bonsai's primary target audience is controller-less network environments. Controller-integrated environments are a secondary audience with a narrower, specific integration story.

**Primary audience** — environments where devices stream gNMI directly to operator-owned infrastructure with no aggregating controller layer:
- Modern SP backbones (Arista/Nokia/Juniper/Cisco with streaming telemetry)
- DC fabrics built device-direct (not ACI/NDI)
- Hyperscale and research networks — the original ANO paper audience
- Telco core networks where controllers are absent or used only for config
- Multi-vendor environments where no single controller can claim the fabric
- Home labs, learning environments, and the open-source networking community

**Why**: For this audience, bonsai is not replicating what a controller provides — it is providing what operators currently assemble by hand from Telegraf + InfluxDB + Grafana + their own rule scripts. The graph, detect-heal loop, ML pipeline, and investigation agent are differentiated because nothing in open source assembles them coherently.

**Secondary audience** — controller-integrated environments. Operators running DNAC, NDI, or Meraki Dashboard already have a graph, already have ML-driven analytics, already have detect-heal for their fabric. Competing against those incumbents inside their own fabrics with an open-source tool is not a defensible position. The one niche where bonsai is genuinely additive is **cross-controller correlation** — a unified graph spanning multiple controllers is something no single vendor provides.

**Architectural consequences**:
- The gNMI-only hot-path rule is correct and binding. It is specifically what makes bonsai valuable to the primary audience.
- Graph enrichment (NetBox, ServiceNow) is the primary mechanism for bringing business context, because the primary audience does not have a controller already doing this.
- Individual controller adapters are optional integrations, not core workload. Implemented only when a specific multi-controller operator requirement drives them.
- The investigation agent's toolset is designed around the gNMI-direct graph; controller adapter tools are added only in the multi-controller correlation case.

**Anti-positions to reject**:
- "Bonsai is a DNAC replacement" — no, wrong audience, losing position.
- "Bonsai should work for every network everywhere" — no, focus matters.
- "Let's add a controller adapter speculatively" — no, demand-driven only.
- "Controller integration is the primary enrichment story" — no, NetBox/ServiceNow for the primary audience.

**Version note**: Captures the v7 backlog reframing. Supersedes any prior implicit framing that treated controller adapters as a core tier.

---

## 2026-04-23 — Detection Ingest RPC (Collector → Core)

**Decision**: Add a client-streaming `DetectionIngest` RPC to the core gRPC API. Collectors push locally-evaluated `DetectionEvent` records to the core for centralized monitoring and graph persistence.

**Rationale**:
- Provides a formal path for collectors to escalate anomalies to the core.
- Allows the core to maintain a global view of all detections across the fleet.
- Enables cross-site rule correlation on the core by treating incoming detections as triggers for global rules.
- `DetectionEvent` metadata includes features, reason, and severity for consistent UI rendering on the core.

---

## 2026-04-24 — Dockerfile build-speed and image-size optimisations (T3-1)

**Decision**: Three targeted changes to `docker/Dockerfile.bonsai`:

1. **Planner stage copies only Cargo manifests** (`Cargo.toml` + `Cargo.lock`). Previously `COPY . .` was used, causing the cargo-chef cook step to re-run whenever any file changed (Svelte sources, docs, proto files). Now only `Cargo.toml`/`Cargo.lock` changes bust the dependency cook cache.

2. **Compiled healthcheck binary replaces curl**. A `src/bin/healthcheck.rs` binary (stdlib only, 337 KB stripped) makes a raw HTTP/1.0 TCP probe to `/api/readiness`. `curl` (~4 MB + shared libs) is removed from the runtime image. This also fixes a latent bug where `docker-compose.yml` referenced `/usr/local/bin/healthcheck` but the image only contained curl.

3. **`liblbug.so.0` is stripped with `--strip-debug`**. The C++ shared library retains the symbol table needed for dynamic linking but drops debug symbols, reducing its size.

**Rationale**: The reported clean Docker build time was 40 minutes. The primary driver was the cargo-chef cook step re-running on every source change. With the manifest-only planner, incremental source-only builds skip the full dep compilation and land in the ~4s range (only the final `cargo build` step runs). Image size reduction (curl removal + library strip) is a secondary benefit contributing to the <100 MB target.


## 2026-04-24 — Sprint 2: Environment model as a first-class graph entity (T1-1, T1-6)

**Decision**: introduce `Environment` as a first-class node in the graph with an archetype enum (`data_center`, `campus_wired`, `campus_wireless`, `service_provider`, `home_lab`). Sites bind to exactly one Environment via a `BELONGS_TO_ENVIRONMENT` edge. The `Site.environment_id` field tracks the binding on the Rust struct for convenience. Onboarding, path-profile selection, enrichment applicability, and future GNN features all key off the archetype rather than free strings.

**Migration**: on first startup after upgrade, existing sites without an Environment binding are automatically assigned to a default environment (`id: "migrated-default"`, `archetype: home_lab`, `name: "Default (Migrated)"`) via `GraphStore::migrate_sites_to_default_environment()`. The migration is idempotent — subsequent startups are a no-op. Operators review and reassign via the `/environments` UI workspace.

**Why enum not free string**: forcing a small archetype enum surfaces coverage gaps explicitly. Any network archetype that doesn't fit the five is marked `home_lab` as an escape hatch and triggers a conversation about whether the enum should be extended (requiring a new ADR).

**Why five archetypes**: DC / campus-wired / campus-wireless / SP / home-lab covers the primary audience (controller-less DC fabrics, SP backbones, campus wired/wireless deployments, home labs). Per v8 backlog: extensions require an explicit ADR entry.

**API surface**: `GET /api/environments`, `POST /api/environments` (create), `POST /api/environments/update`, `POST /api/environments/remove`, `POST /api/environments/assign-site`. Setup detection: `GET /api/setup/status`.

**First-run detection**: `setup_status_handler` returns `is_first_run: true` when no non-default environments exist, no credential aliases are configured, and no devices are onboarded. The UI routes to `/setup` on this signal.
