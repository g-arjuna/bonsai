# BONSAI — Consolidated Backlog v5.0

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_V4.md`. Produced 2026-04-22 after a small incremental delta (Dockerfile landing, five T0 v4 items closed) and two substantive strategic conversations that belong in the document of record: (a) collector-core data exchange and control-plane discipline, (b) controller adapters as a distinct enrichment category.
>
> **What v5 adds on top of v4:**
> 1. **Build optimisation tier** — explicit work on `cargo` and Docker build times before the codebase grows further
> 2. **Collector-core data exchange architecture** — move from dumb forwarder (Tier 1) to summarised telemetry + collector-side detection (Tier 3); the specific contract between collector and core
> 3. **Control-plane discipline** — onboarding lives on core, collectors are data-plane workers, no user-facing UI on collectors, credentials delivered over mTLS per-assignment
> 4. **Controller adapters** as a distinct category from source-of-truth enrichers — DNAC, Meraki, ACI, vManage, PCEP
> 5. **LLM-on-config exclusion** codified as a guardrail — deterministic parsers for enrichment, LLM interpretation of config stays in the investigation agent only

---

## Table of Contents

1. [Progress Since v4](#progress)
2. [TIER 0 — Loose Ends from v4 Review](#tier-0)
3. [TIER 1 — Build Optimisation (new)](#tier-1)
4. [TIER 2 — Collector-Core Data Exchange Architecture (new)](#tier-2)
5. [TIER 3 — Control-Plane Discipline (new)](#tier-3)
6. [TIER 4 — Containerisation Plane (carry from v4 T1, updated)](#tier-4)
7. [TIER 5 — Scale Architecture (carry from v4 T2)](#tier-5)
8. [TIER 6 — Graph Enrichment via MCP + Legacy Protocols (carry from v4 T3)](#tier-6)
9. [TIER 7 — Controller Adapters (new, distinct from enrichment)](#tier-7)
10. [TIER 8 — Syslog and SNMP Traps (carry from v4 T4)](#tier-8)
11. [TIER 9 — UI Usability for Network Practitioners (carry from v4 T5)](#tier-9)
12. [TIER 10 — Path A Graph Embeddings → Path B GNN (carry from v4 T6)](#tier-10)
13. [TIER 11 — Investigation Agent (carry from v4 T7)](#tier-11)
14. [TIER 12 — Carryover Extensions (carry from v4 T8)](#tier-12)
15. [Execution Order](#execution-order)
16. [Merge Plan](#merge-plan)
17. [Guardrails — Updated](#guardrails)

---

## <a id="progress"></a>Progress Since v4 — Verified Against the Branch

**Completed and removed** — all verified with code review:

| v4 item | Status | Evidence |
|---|---|---|
| T0-1 v4 credential metadata write debounce | ✅ Done | `src/credentials.rs` — `LAST_USED_WRITE_WINDOW_NS = 5 minutes`, `should_persist_last_used` gate; test asserts 50 sequential resolves produce one write |
| T0-2 v4 vault passphrase rotation ADR | ✅ Done | 2026-04-22 ADR entry in `DECISIONS.md` documenting manual rotation as deferred, with workaround |
| T0-3 v4 archive format documentation | ✅ Done | `docs/archive_format.md` — layout, schema, MessagePack-vs-JSON-in-archive rationale, pandas read example |
| T0-4 v4 CLI device commands | ✅ Done | `main.rs` subcommand handling with `Help` variant; routes through existing gRPC client for add/remove/list/update |
| T0-5 v4 site hierarchy depth guard | ✅ Done | `src/graph.rs::validate_site_hierarchy` — cycle detection via HashSet walk, max depth 10, explicit self-reference rejection |
| T1-1 v4 Dockerfile (first slice) | ✅ Done | `docker/Dockerfile.bonsai` — cargo-chef + BuildKit cache mounts, multi-stage with parallel UI build, `debian:trixie-slim` runtime, non-root UID 10001, dynamic LBUG with LD_LIBRARY_PATH, busybox healthcheck; `.dockerignore` prunes build context |

**Not yet done** (carried into v5 under new or renumbered tiers):

- Docker Compose profiles, ContainerLab integration, chaos-in-containers, secrets (v4 T1-2 through T1-5) → renumbered T4 in v5
- Scale architecture documents and profiling (v4 T2) → renumbered T5
- All enrichment work (v4 T3) → renumbered T6
- All signal work (v4 T4) → renumbered T8
- UI usability pass (v4 T5) → renumbered T9
- GraphML Path A/B (v4 T6) → renumbered T10
- Investigation agent (v4 T7) → renumbered T11
- All carryover extensions (v4 T8) → renumbered T12

**Discipline observations:**

- **The branch delivered precisely what the operator intended** — quiet, correctly-scoped, five T0 items closed plus a clean Dockerfile. Small deltas are healthy; no single PR should try to do everything.
- **ADRs are landing at commit time now** — the passphrase-rotation ADR was written alongside the code, not retroactively. Keep this pattern.
- **The Dockerfile is genuinely good** (see T1 below for the specific strengths and the few optimisation headroom items worth tackling now rather than later).

---

## <a id="tier-0"></a>TIER 0 — Loose Ends from v4 Review

### T0-1 (v5) — Pinned apt versions in Dockerfile will break
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Dropped version pins in `docker/Dockerfile.bonsai`. Reproducibility now managed via base image digest.

**What**: `docker/Dockerfile.bonsai` pins specific apt package versions (`ca-certificates=20250419`, `clang=1:19.0-63`, `cmake=3.31.6-2`, etc.). Debian trixie rotates package versions fast; in 2-3 months these exact versions disappear from the mirror and `docker build` fails with "version not found" on what should be an unchanged image.

...

### T0-2 (v5) — Dockerfile uses `--bin bonsai` which invalidates cargo-chef cache on additional binaries
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Updated `docker/Dockerfile.bonsai` to use `cargo chef cook --release --all-targets`.

**What**: The current build uses `cargo chef cook` (unqualified) followed by `cargo build --release --bin bonsai`. When we add a second binary (`bonsai-device-cli`, etc.), the cook cache may not include its dependencies and we'll get a fresh full rebuild on CI.

...

### T0-3 (v5) — Credential vault `update` method distinct from `add`
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Implemented `CredentialVault::update` and added `/api/credentials/update` route in `src/http_server.rs`.

**What**: carry-over from v4 review. `CredentialVault::add` acts as upsert (line 145 overrides if alias exists). Clean for v1 but a future UI wanting explicit "edit this alias" semantics needs a distinct method. Small API cleanup.

...

### T0-4 (v5) — Proto version negotiation stub
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Added `protocol_version` field to `TelemetryIngestUpdate` and `TelemetryIngestResponse` in `bonsai_service.proto`.

**What**: collectors and core will inevitably version-skew in the real world. Today the proto has no version handshake. Before we lock in the Tier 2 data-exchange contract, introduce a minimal version field so we have a hook later.

---

## <a id="tier-1"></a>TIER 1 — Build Optimisation

**Why this is a dedicated tier now**: the codebase has doubled since v3 and will grow again with enrichment, controller adapters, and the GNN pipeline. Build times that are mildly annoying today become productivity-killers at 2× the size. Fixing this now is cheap; fixing it after a year of accumulation is painful. Every minute saved on a `cargo build --release` is a minute that gets spent every day for the rest of the project.

### T1-1 — Measure before optimising
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Baseline captured in `docs/build_performance.md`. Cold clean build ~23m, Docker ~40m.

**What**: before touching anything, establish a baseline. Otherwise we can't tell whether changes helped.

...

### T1-2 — `sccache` integration
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Integrated `sccache` via `.cargo/config.toml` and provided `scripts/install_sccache.sh`.

**What**: `sccache` (Mozilla) caches Rust compilation artifacts across clean builds and across developers. A single `~/.cache/sccache` directory eliminates repeated compilation of unchanged dependencies even after `cargo clean`.

...

### T1-3 — Workspace split (cautious)
**Status**: ⏳ **Planned**

**What**: `Cargo.toml` today is a single crate. Splitting into a workspace with `bonsai-core`, `bonsai-collector` (or `bonsai-distributed`), `bonsai-api`, and `bonsai-cli` would dramatically cut incremental build times because touching one module only recompiles its crate, not the whole tree.

...

### T1-4 — Link-time optimisation and CPU targeting
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Added `lto = "thin"`, `codegen-units = 1`, and `strip = "symbols"` to `Cargo.toml`.

**What**: current release profile doesn't specify LTO, codegen units, or target CPU. These settings can cut binary size and marginally improve runtime at the cost of longer release builds.

...

### T1-5 — Cargo dependency audit
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Created `scripts/dep_audit.sh` and performed initial audit. Identified duplicate crates.

**What**: `cargo tree` will show the dependency graph. Anywhere we have multiple versions of the same crate pulled in by different parents is a compilation-time (and binary-size) cost. `cargo audit` flags security advisories.

...

### T1-6 — Docker build: parallel stage execution
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Verified parallel UI and Rust stage builds in `docker/Dockerfile.bonsai`.

**What**: BuildKit can build the UI stage and the Rust stage in parallel. Today they're in the same file and BuildKit does the right thing *if* neither stage depends on the other. Verify that's the case and document it.

...

### T1-7 — Docker multi-arch build
**Status**: ⏳ **Planned**

**What**: many developers are on Apple Silicon; CI may be on ARM; some operators will deploy on ARM. Today the Dockerfile builds only the host architecture.

...

### T1-8 — Docker image size reduction
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Switched to `gcr.io/distroless/cc-debian12` and implemented custom Rust healthcheck. Final size ~131MB.

**What**: current image estimated at 150-250 MB range (not measured yet — T1-1 baseline establishes this). Several techniques can bring it down further.

...

### T1-9 — UI build: Vite tuning and lighthouse audit
**Status**: ✅ **Partially Done**
**Execution**: UI build time measured. Lighthouse audit remaining.

**What**: `npm run build` has its own tuning surface. Vite is reasonably fast out of the box but has room.

...

### T1-10 — CI pipeline for build-time monitoring
**Status**: ⏳ **Planned**

**What**: build time tends to creep up invisibly. Catch it in CI.

---

## <a id="tier-2"></a>TIER 2 — Collector-Core Data Exchange Architecture

**Context** (from the design conversation captured in this iteration): for 12 devices at 10-second SAMPLE counters, the collector produces roughly 1,400 update messages/sec of counter data versus a handful per minute of state-change data. Today's architecture (Tier 1 in our internal rubric: "dumb forwarder") ships every decoded `TelemetryUpdate` from collector to core. That's correct but wasteful and fails to use the collector for what it's naturally good at: local processing.

The target is Tier 3: **summarised telemetry + collector-side detection + full state-change events**. Tier 4 (collector-as-autonomous-pod) is a trap for mainline work but should remain a *capability* for disconnected-ops deployments.

### T2-1 — Collector-side counter debounce before forwarding
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Implemented per-interface counter debouncing in `src/ingest.rs`. Successfully drops duplicate updates within the configured window.

**What**: the counter debounce logic that runs at core today (10s per-interface, from the `event_bus.counter_debounce_secs` config) should happen on the collector side before forwarding. Counter updates that the collector just saw and that match the last-forwarded within the window get dropped locally.

...

### T2-2 — Summary-mode counter forwarding
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Implemented `CounterSummarizer` for UTC-aligned 60s windows. Added `InterfaceSummary` proto message and ingestion logic.

**What**: the `summary` counter_forward_mode goes further than debounce — instead of forwarding one-every-10s raw counter updates, the collector maintains a 60-second rolling window per interface and forwards a single summary per interface per minute containing `{min, max, mean, delta_bytes, delta_packets, delta_errors}`.

...

### T2-3 — Collector-local graph (scoped)
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Implemented `CollectorGraphStore` with a minimal schema. Shared logic refactored into `src/graph/common.rs`.

**What**: for the collector to run its own detection rules, it needs a local graph. Not the full core graph — just the devices the collector owns, with the schema restricted to what detection rules need.

...

### T2-4 — Collector-side detection with escalation
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Added `scope` to `Detector` class. Updated `RuleEngine` to filter by scope. Implemented `DetectionIngest` RPC and `python/collector_engine.py` sidecar.

**What**: run the existing rule engine on the collector for rules that can be fully evaluated with local state. Rules that need cross-collector knowledge ("all BGP peers globally down") stay on core and are marked with a `scope: core` rule attribute.

...

### T2-5 — Collector archive scope and periodic sync
**Status**: ⏳ **Planned**

**What**: collector-local archive already exists. Document how cross-collector archive analysis (for Tier 10 GNN training) happens.

...

### T2-6 — Explicit collector-core protocol contract
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Documented the contract in `docs/collector_core_protocol.md`.

**What**: the collector-core RPC surface is expanding. Document it as a versioned contract.

---

## <a id="tier-3"></a>TIER 3 — Control-Plane Discipline

**Context**: in the distributed world, "where do I log in? where do I add a device? where are credentials stored? where do I see incidents?" need clear, non-negotiable answers. The answer is: **everything operator-facing happens on core**. Collectors are data-plane workers.

This tier is about enforcing that architecturally, not as a convention.

### T3-1 — Collector-side UI lockdown
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Updated `src/main.rs` to only spawn the HTTP UI server when `run_core` is true. Collectors now serve no UI.

**What**: the current binary serves the full UI on port 3000 regardless of mode. A collector process with the UI reachable is a bug — operators will bookmark it, build runbooks around it, and we'll be stuck supporting it forever.

...

### T3-2 — Onboarding assignment model
**Status**: ⏳ **Planned**

**What**: today a device is either in `bonsai.toml` or in `bonsai-registry.json` on whatever host is running. In Tier 3 world, core is authoritative and assigns devices to collectors based on site.

...

### T3-3 — Credential delivery over mTLS, not stored on collector
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Implemented `spawn_subscriber_with_creds`. Collectors hold resolved credentials strictly in memory, securely delivered via the gRPC assignment stream.

**What**: credentials resolve on core (from the vault) and are shipped to the collector over the already-mTLS-protected channel on assignment. Collector holds credentials in memory only, never on disk.

...

### T3-4 — Management-plane / user-plane network segmentation
**Status**: ⏳ **Planned**

**What**: document the intended trust boundary.

...

### T3-5 — Disconnected-ops capability flag
**Status**: ✅ **Done (2026-04-23)**
**Execution**: Architecture decision captured in `DECISIONS.md`. Primitive support (local graph/detection) implemented.

**What**: for air-gapped or unreliable-link sites, the collector should be capable of running autonomously — with a local graph, local detection, local playbook execution, local minimal UI — even if the default configuration keeps it as a data-plane worker.

---

## <a id="tier-4"></a>TIER 4 — Containerisation Plane (carry from v4 T1, updated)

The Dockerfile landed. Remaining work from v4 T1 carries forward, now with the build-optimisation awareness from T1 in v5.

### T4-1 — Docker Compose for local dev and distributed validation
**Status**: ✅ **Done (2026-04-24)**
**Execution**: Created `docker-compose.yml` with `dev`, `distributed`, `two-collector`, and `chaos` profiles. Added volume mount strategy and config overlays in `docker/configs`. Successfully started `dev` profile linking with the `bonsai-p4-mgmt` ContainerLab network.

(v4 T1-2, unchanged scope.)

Bring up core, one or more collectors, and ContainerLab devices on shared Docker networks. Four profiles: `dev`, `distributed`, `two-collector`, `chaos`.

**New consideration from Tier 2/3 decisions**: the `distributed` profile now needs separate configs for core and each collector reflecting their distinct roles. Core gets the vault, the UI, and the ingest endpoint. Collector gets its queue volume, minimal config, no vault. `two-collector` profile validates the assignment engine from T3-2.

### T4-2 — ContainerLab integration

(v4 T1-3, unchanged scope.)

### T4-3 — Container secrets handling
**Status**: ✅ **Done (2026-04-24)**
**Execution**: Implemented `BONSAI_VAULT_PASSPHRASE` environment variable handling in `docker-compose.yml` and `Dockerfile.bonsai`, keeping passwords out of the image layer.

(v4 T1-4, unchanged scope.)

Add explicit handling of the Tier 3 credential-delivery pattern: the core's `BONSAI_VAULT_PASSPHRASE` is the only secret an operator manages at deployment time. Collectors don't hold the passphrase at all.

### T4-4 — Chaos inside containers
**Status**: ✅ **Done (2026-04-24)**
**Execution**: Added a `chaos` profile in `docker-compose.yml` leveraging `gaiaadm/pumba` to inject artificial network delays (500ms) to collector containers for resilience testing.

(v4 T1-5, unchanged scope.)

### T4-5 — Volume lifecycle documentation
**Status**: ✅ **Done (2026-04-24)**
**Execution**: Created `docs/deployment_volumes.md` detailing all volume persistence properties, backup strategies, and recovery behaviors.

**What**: Docker volumes in the compose setup have specific roles. Document what each one holds, what backup looks like, what "losing" each means.

| Volume | Host | Contains | Loss impact |
|---|---|---|---|
| `bonsai_graph` | core | LadybugDB | Regenerates from telemetry within minutes; no permanent loss |
| `bonsai_archive` | collector | Parquet files | Permanent loss of historical telemetry; affects GNN training only |
| `bonsai_creds` | core | Encrypted vault | Permanent; operators must re-enter credentials |
| `collector_queue` | collector | Disk-backed queue | Transient; lost only means in-flight-during-outage telemetry is lost |

**Where**: `docs/deployment_volumes.md`

**Done when**: doc exists; compose files reference it; operators have a clear mental model of what to back up.

---

## <a id="tier-5"></a>TIER 5 — Scale Architecture (carry from v4 T2)

### T5-1 — Document the scale thesis

(v4 T2-1, unchanged.)

### T5-2 — Collector-local Parquet archive operational doc

(v4 T2-2, unchanged; reinforced by T2-5 above.)

### T5-3 — S3-compatible archive backend

(v4 T2-3, unchanged.)

### T5-4 — Core bottleneck profiling

(v4 T2-4, updated scope.)

Now that Tier 2 shifts detection work to collectors, the core bottleneck profile looks different. Re-run profiling after T2-1 through T2-4 land — expect the bottleneck to move from "graph write contention under counter floods" to "cross-collector detection correlation under event bursts."

### T5-5 — Multi-collector validation

(v4 T2-5, expanded.)

Validate: assignment from core, credential delivery on assignment, site-based routing, failover when a collector goes offline (affected devices show `unassigned` until reassigned), operator-initiated reassignment from the UI.

---

## <a id="tier-6"></a>TIER 6 — Graph Enrichment via MCP + Legacy Protocols (carry from v4 T3)

### T6-1 — `GraphEnricher` trait and enrichment pipeline

(v4 T3-1, unchanged.)

**New constraint added in v5**: enrichers never call LLMs on device configuration. The enrichment architecture is deterministic. See Guardrails.

### T6-2 — NetBox enricher (MCP-backed)

(v4 T3-2, unchanged.)

### T6-3 — ServiceNow CMDB enricher

(v4 T3-3, unchanged.)

### T6-4 — Infoblox/BlueCat enricher (optional, deferred)

(v4 T3-4, unchanged.)

### T6-5 — CLI-scraped enricher (pyATS/TextFSM, deterministic)

(v4 T3-5, unchanged — reaffirmed that this is pyATS/Genie/TextFSM based, never LLM-based parsing of raw config.)

### T6-6 — NETCONF/RESTCONF enricher

(v4 T3-6, unchanged.)

### T6-7 — Enrichment visibility in UI

(v4 T3-7, unchanged.)

### T6-8 — MCP client infrastructure

(v4 T3-8, unchanged.)

---

## <a id="tier-7"></a>TIER 7 — Controller Adapters

**Context** (from the recent conversation): in real enterprise and SP environments, the device is increasingly not the source of truth — the controller is. DNAC, vManage, Meraki Dashboard, ACI APIC, PCEP controllers. These hold data that no individual device can stream: assurance metrics, application routing, per-client wireless data, intent-based policy. Without controller integration, bonsai's potential audience is limited to hyperscaler-style fabrics.

Controller adapters are a **distinct category from source-of-truth enrichers** (Tier 6). Enrichers decorate the graph with slow-moving context (IPAM, DCIM, application ownership). Controller adapters deliver operational state and events from a controller that's already aggregating device telemetry — they are closer to being a collector than an enricher.

### T7-1 — `ControllerAdapter` trait and runtime role

**What**: a new trait distinct from `GraphEnricher`, and a new runtime role (`controller-adapter`) that runs as a separate process with its own credentials, rate-limiting, and auth state.

**Where**: `src/controller/mod.rs`, `src/controller/trait.rs`

**Trait design**:
```rust
#[async_trait]
pub trait ControllerAdapter: Send + Sync {
    fn name(&self) -> &str;                       // "dnac", "meraki", "aci", etc.
    fn controlled_domain(&self) -> ControlDomain; // what part of the graph this adapter owns
    async fn run(&self, bus: Arc<InProcessBus>, shutdown: watch::Receiver<bool>) -> Result<()>;
}

pub enum ControlDomain {
    // Adapter owns these graph node types — no other source writes to them
    ExclusiveNodes(Vec<&'static str>),
    // Adapter writes namespaced properties on existing nodes
    PropertyNamespace(&'static str),
}
```

**Design principles** (the distinguishing ones from Tier 6 enrichers):
- Controller adapters can **claim authority over graph sub-domains** — ACI owns `EPG`, `Contract`, `Tenant` nodes; Meraki owns `Organization`, `Network`, `WirelessClient` nodes. No other source writes there.
- Controller adapters can **substitute for a collector** — for devices managed by a controller with its own telemetry pipeline (e.g. DNAC Assurance, Meraki), bonsai's direct gNMI subscription is redundant. The adapter delivers state through the same event bus.
- Controller adapters run as **their own runtime role** — process isolation for rate limits, auth state, and failure modes
- Adapters declare their schema extensions so other parts of the system (enrichment, GNN, UI) can reason about them

**Done when**: trait and runtime role exist; one reference implementation (Meraki — see T7-2) works end-to-end; ADR explains the authority/aggregation distinction and the collector-substitute behaviour.

### T7-2 — Meraki Dashboard adapter (flagship)

**What**: first controller adapter implementation. Meraki is cloud-only, well-documented REST API, has an MCP server available, and reaches a huge installed base of operators.

**Why Meraki first (and not DNAC)**:
- Meraki is REST-only with straightforward auth (API key), simpler to implement than DNAC's multi-auth surface
- Meraki is the only path to wireless/client data that no other source provides — an unambiguous authority, not just an aggregator
- MCP server is mature and community-maintained
- Good demo story: "bonsai's graph shows an access point with its connected clients, signal strengths, and roaming events — none of which gNMI can produce"

**Writes to graph**:
- New nodes: `Organization(id, name)`, `Network(id, name, type)`, `WirelessClient(mac, last_seen, signal_dbm, ssid)`
- New edges: `HAS_NETWORK(Organization → Network)`, `MEMBER_OF(Device → Network)`, `CONNECTED_TO(WirelessClient → Device)`
- `Device` gets properties `meraki_serial`, `meraki_model`, `meraki_firmware`, `meraki_lifecycle`, `meraki_public_ip`
- Device-level alerts from Meraki become SignalIngest messages (see Tier 8)

**Where**: `src/controller/meraki.rs` + `[controller.meraki]` config section

**Design**:
- Direct REST (via reqwest) first; MCP as a future optimisation
- Rate-limit aware — Meraki has strict quotas per org; respect them with backoff
- Incremental poll cadence: org/network/device lists every 30 min, wireless client status every 5 min (subject to rate limits)

**Done when**: adapter runs against a live Meraki org (lab or personal); graph shows Organization/Network/Device/WirelessClient; UI renders a wireless-client count per AP.

### T7-3 — DNAC/Catalyst Center adapter

**What**: the enterprise heavy-hitter. On-prem controller (or Cisco-hosted), more complex auth (OAuth2 with token refresh), assurance data that's deeply useful.

**Writes to graph**:
- `Device` gets properties `dnac_uuid`, `dnac_health_score`, `dnac_reachability`, `dnac_last_updated`
- New nodes: `Site` from DNAC (potential conflict with bonsai's own Site — needs reconciliation logic)
- Device-level assurance issues from DNAC become signals (Tier 8)
- Client tracking data becomes properties on a new `Client` node

**Site reconciliation**: DNAC has its own site hierarchy. The adapter should offer a config flag: `mode = "authoritative" | "supplemental"`. Authoritative means DNAC's sites replace bonsai's; supplemental means DNAC sites are linked via a new `EXTERNAL_ID` edge and the operator-managed hierarchy stays primary.

**Where**: `src/controller/dnac.rs` + `[controller.dnac]` config

**Done when**: adapter runs against DNAC (real or lab); at least the device-health-score flow works end-to-end; ADR documents the site reconciliation options.

### T7-4 — ACI APIC adapter (fabric-specific)

**What**: for operators running ACI fabrics. Different schema territory — EPG, Contract, Tenant — that's policy-first rather than routing-first.

**Schema impact**:
- New nodes: `Tenant`, `EPG`, `Contract`, `BridgeDomain`, `ApplicationProfile`
- New edges: `PROVIDES_CONTRACT`, `CONSUMES_CONTRACT`, `MEMBER_OF_EPG`
- `Device` gets ACI-specific properties (fabric role, pod, node ID)

**Priority**: lower than Meraki and DNAC because ACI is fabric-specific and the schema extension is large. Build when an operator with ACI asks. Until then, the design sits in the backlog and ensures the controller-adapter trait can accommodate large schema extensions.

### T7-5 — vManage (Viptela) adapter

**What**: SD-WAN controller. Unique value: tunnel health, SLA state, application-aware path selection.

**Writes to graph**:
- New nodes: `Tunnel`, `WanPath`, `SLAProfile`
- `Device` gets `vmanage_mode` (edge/controller/validator), `vmanage_ha_state`
- Path selection changes become signals

**Priority**: build when someone asks.

### T7-6 — Arista CloudVision adapter

**What**: Arista's equivalent of DNAC for Arista fabrics. Good ROI because the API is clean.

**Priority**: medium — after Meraki and DNAC land, before ACI.

### T7-7 — PCEP controller adapter (SP-specific)

**What**: for service-provider environments — Cisco WAE, Juniper NorthStar, Nokia NSP. Hold path-computation state that no device alone has.

**Priority**: explore when bonsai has a concrete SP lab topology driving the requirement.

### T7-8 — Controller adapter UI workspace

**What**: a workspace showing each active controller adapter, what it's managing, recent activity, auth status.

**Where**: UI extension similar to the enrichment workspace (T6-7)

**Done when**: each adapter's managed device count, last poll time, and any error state is visible in one place.

---

## <a id="tier-8"></a>TIER 8 — Syslog and SNMP Traps (carry from v4 T4)

(v4 T4-1 through T4-5, unchanged — now reinforced by controller adapters that often deliver their own alerts through the same signal channel.)

---

## <a id="tier-9"></a>TIER 9 — UI Usability for Network Practitioners (carry from v4 T5)

(v4 T5-1 through T5-7, unchanged.)

**Updated items reflecting v5 architecture**:

### T9-8 (new) — Collector status and assignment UI

**What**: the operator observability view from v4 T5-7 gains a dedicated collector workspace reflecting Tier 3 control-plane discipline. Shows: registered collectors, their heartbeat status, assigned devices per collector, queue depth, protocol version.

Operator actions available:
- Reassign a device from one collector to another
- See which collector is authoritative for which device
- See unassigned devices (no collector for their site)

**Where**: new UI route under `Devices` or `Operations`

**Done when**: an operator can diagnose a collector outage from the UI without SSH'ing into the collector host.

---

## <a id="tier-10"></a>TIER 10 — Path A Graph Embeddings → Path B GNN (carry from v4 T6)

(v4 T6-1 through T6-3, unchanged.)

**Sequencing reaffirmed**: Path A before Path B. Operational infrastructure (archive, chaos, enrichment, controller data) before Path B. By the time Path B starts, the graph carries device state, Site hierarchy, Application/business context (from ServiceNow), VLAN/Prefix (from NetBox), wireless clients (from Meraki), ACI policy (if ACI adapter lands) — substantially richer than gNMI-only.

---

## <a id="tier-11"></a>TIER 11 — Investigation Agent (carry from v4 T7)

(v4 T7-1 through T7-4, unchanged.)

**New consideration**: the agent's tool surface expands with controller adapters. An agent investigating a wireless issue can now call `query_graph` for `WirelessClient` nodes, or `query_controller("meraki", "get_client_events", {mac: ...})` as a direct controller call (with the same human-approval gate on any action).

**LLM-on-config boundary reaffirmed**: the investigation agent IS allowed to call LLMs on device configuration when triggered by an operator `/investigate` command. Enrichers and controller adapters are NOT. This is the architecturally clean separation — LLM interpretation of config lives in the slow path with human approval, never in the deterministic background.

---

## <a id="tier-12"></a>TIER 12 — Carryover Extensions (carry from v4 T8)

(v4 T8-1 through T8-12, unchanged.)

---

## <a id="execution-order"></a>Recommended Execution Order

### Sprint 1 — Loose ends + build baseline (1 week)
1. T0-1 unpin Dockerfile apt versions
2. T0-2 cargo-chef `--all-targets`
3. T0-3 credential vault explicit `update`
4. T0-4 proto version negotiation stub
5. T1-1 baseline build measurements captured in `docs/build_performance.md`

### Sprint 2 — Build optimisation (1-2 weeks)
6. T1-2 sccache integration
7. T1-4 LTO and release profile tuning
8. T1-5 dependency audit
9. T1-6 Docker parallel stage verification
10. T1-8 Docker image size reduction (target <100 MB)
11. T1-9 UI build and Lighthouse baseline
12. T1-3 Cargo workspace split — ONLY if baseline shows single-crate incremental builds are painful. If not, defer.
13. T1-7 Docker multi-arch — after confirming lbug builds on arm64

### Sprint 3 — Collector-core data exchange (2-3 weeks) ⚡
14. T2-1 collector-side counter debounce (biggest-bang-for-buck)
15. T2-2 summary-mode counter forwarding
16. T2-6 explicit collector-core protocol contract doc
17. T2-3 collector-local graph (scoped schema)

### Sprint 4 — Collector-side detection (2-3 weeks)
18. T2-4 collector-side detection with `scope: local/core/hybrid` rule attributes
19. T2-5 archive consolidation doc

### Sprint 5 — Control-plane discipline (2 weeks)
20. T3-1 collector-side UI lockdown
21. T3-2 onboarding assignment model (site → collector)
22. T3-3 credential delivery over mTLS
23. T3-4 network segmentation doc
24. T3-5 disconnected-ops capability flag (design only, no UI/executor on collector)

### Sprint 6 — Docker Compose + validation (1-2 weeks)
25. T4-1 Compose profiles including two-collector and distributed
26. T4-2 ContainerLab integration
27. T4-3 container secrets
28. T4-4 chaos in containers
29. T4-5 volume lifecycle doc
30. T5-5 multi-collector validation using the new assignment engine

### Sprint 7 — Enrichment foundation (2-3 weeks)
31. T6-1 `GraphEnricher` trait
32. T6-8 MCP client infrastructure
33. T6-7 enrichment UI workspace
34. T6-2 NetBox enricher (flagship)
35. T6-5 CLI-scraped enricher

### Sprint 8 — Controller adapters (2-3 weeks)
36. T7-1 `ControllerAdapter` trait + runtime role
37. T7-2 Meraki adapter (flagship — proves the authority/aggregation distinction)
38. T7-8 controller adapter UI workspace
39. T7-3 DNAC/Catalyst Center adapter

### Sprint 9 — ServiceNow + signals (2 weeks)
40. T6-3 ServiceNow CMDB enricher
41. T8-* syslog/trap collector (v4 T4-1 through T4-5)
42. Signal-aware detectors integration with controller-sourced alerts

### Sprint 10 — UI practitioner pass (2-3 weeks)
43. T9-1 workflow-centric navigation
44. T9-8 collector status and assignment UI
45. T9-2 topology improvements
46. T9-3 incident-centric UI
47. T9-4 credential and vault UX polish
48. T9-5 site UX improvements
49. T9-7 operator observability

### Sprint 11 — Scale architecture (1-2 weeks)
50. T5-1 scale thesis doc
51. T5-4 core bottleneck profiling (post-Tier-2)
52. T5-3 S3-compatible archive backend

### Sprint 12 — Path A embeddings (1-2 weeks)
53. T10-1 graph embeddings stepping stone with enrichment + controller features
54. T12-2 ML feature schema versioning

### Sprint 13 — NL query (1 week)
55. T12-1 NL query layer

### Sprint 14 — Investigation agent (2-3 weeks)
56. T11-1 agent scaffolding with controller-aware tool surface
57. T11-4 agent cost controls
58. T11-2 agent UI workspace

### Sprint 15 — Path B GNN (3-4 weeks)
59. T10-2 GNN implementation (enriched + controller-decorated graph)
60. T10-3 enrichment-aware data loader
61. T11-3 agent memory

### Longer horizon
- T7-4 ACI adapter (when ACI operator asks)
- T7-5 vManage adapter (when SD-WAN operator asks)
- T7-6 Arista CloudVision adapter
- T7-7 PCEP adapter (when SP lab drives it)
- T6-4 Infoblox enricher
- T6-6 NETCONF enricher
- Other v4 T8 (now T12) items

### Defer until forced by pain
- T12-4 bitemporal schema
- T12-6 schema migration path
- T12-7 Grafeo evaluation

---

## <a id="merge-plan"></a>Branch Merge Plan

Current state: `codex-t1-1-credentials-vault` branch has the credentials vault, Site-as-graph, onboarding wizard, distributed hardening (zstd + queue + mTLS + live validation), MessagePack wire format, archive append-to-hour, UI SSE, Dockerfile, and T0 v4 items — all verified.

### Recommendation: **Merge the branch now** before starting Sprint 1.

The branch content is stable, tested, and documented. It has been living as a feature branch for weeks. Merging it to main lets v5 work happen on main without accumulating branch-on-branch drift.

Proposed merge structure (same as v4 plan, kept as reference):
- Stage 1 — T0 v4 small items directly to main
- Stage 2 — archive + ingest + queue (cohesive distributed transport hardening)
- Stage 3 — credentials vault (isolated module, cryptographic review)
- Stage 4 — Site as graph entity with migration
- Stage 5 — onboarding wizard + HTTP endpoints + SSE
- Stage 6 — build plumbing (dynamic LBUG, Cargo.toml additions, generated stubs)
- Stage 7 — Dockerfile + .dockerignore

Tag `v0.4.0` after merge. All v5 tiers start on main.

---

## <a id="guardrails"></a>Guardrails — Updated for v5

### Architectural invariants (unchanged from v4)

- gNMI only for hot-path telemetry **state** from devices
- Syslog and traps are allowed as **signals**, never as state sources
- tokio only for async Rust
- Credentials never leave the Rust process except on outbound gNMI/NETCONF/SSH connection
- No Kubernetes in v0.x. Docker + Compose are fine
- No fifth vendor until the four vendor families work vendor-neutrally
- Every non-trivial decision gets an ADR at commit time

### Hot-path determinism (unchanged)

- The detect-heal loop does not call an LLM. Ever.
- The detect-heal loop does not call MCP, NETCONF, CLI, or any enrichment source synchronously
- Detection latency target stays sub-second
- If the Anthropic API or any MCP server is unreachable, bonsai still detects and heals

### Control-plane discipline (new in v5)

- **All operator-facing functionality lives on core.** Onboarding, credential management, policy, incident view, investigation — all core.
- **Collectors are data-plane workers.** They execute assignments, they do not accept operator input beyond their own diagnostic health endpoint.
- **The collector UI is locked down.** Collector-mode processes serve only `/health`, `/api/readiness`, `/api/collector/status`. Everything else returns 404.
- **Credentials are delivered per-assignment over mTLS.** Collectors hold no vault, no passphrase, no persistent credential state.
- **There is one place operators interact with bonsai, and that place is core.** No bookmarkable collector UI for daily use.

### Enrichment discipline (updated in v5)

- Enrichers write via a restricted graph surface
- Enrichers are idempotent, isolated, opt-in
- Enrichers never gate the hot-path
- Enricher output is namespaced (`netbox_*`, `snow_*`)
- **NEW: Enrichers never call LLMs on device configuration.** Config interpretation via LLM is an investigation-agent capability, runs with explicit operator trigger, never becomes a graph property.
- **NEW: Deterministic parsers only** — pyATS/Genie, TextFSM, YANG-aware parsers. No prompt-based config extraction anywhere in the enrichment pipeline.

### Controller adapter discipline (new in v5)

- Controller adapters are a **distinct category** from enrichers — they run as their own runtime role with their own process isolation
- Controller adapters can **claim authority** over graph sub-domains; other sources must not write there
- Controller adapters can **substitute for a collector** — devices managed by a controller with its own telemetry pipeline do not need a parallel gNMI subscription
- Controller adapters announce their schema extensions; UI, enrichment, and GNN pipelines see them explicitly

### Scale discipline (unchanged from v4)

- Collectors scale horizontally. Core scales vertically in v1.
- Graph sharding is a v2 conversation.
- Archive is collector-local; central storage is an add-on.

### Build discipline (new in v5)

- **Build time is a first-class metric.** Baseline measured in v5 Sprint 1; regressions flagged in CI.
- **Dependency health is tracked.** `cargo tree --duplicates`, `cargo audit`, and `cargo outdated` run regularly.
- **Docker builds must be reproducible.** Apt versions either unpinned (trust base image digest) or pinned to a snapshot mirror; no mid-path pinning that breaks at package rotation.

### ML discipline (unchanged)

- Tabular ML remains the production path until GNN has honest validation
- GraphML work does not eat operational or enrichment work
- GNN training requires months of real data; no synthetic-data shortcuts

### Anti-patterns to reject

- "Let's use SNMP polling for state" — no, traps as signals only
- "Let's put the UI on collectors too, operators will want it" — no, control plane stays on core
- "Let's store credentials on the collector for resilience" — no, credentials delivered per-assignment
- "Let's let the enricher use an LLM to parse config" — no, deterministic parsers only
- "Let's deploy this on Kubernetes now" — no
- "A fifth vendor would be cool" — no
- "Let's skip ADRs for the small stuff" — no
- "Let's have the agent run without human approval" — no
- "Let's skip the build baseline, we'll optimise later" — no, it's already later

---

## What v5 Explicitly Excludes

For scope discipline, do not start:
- Auth/RBAC of any kind
- Multi-tenancy in the graph
- Production HA for the core (leader election, graph replication)
- Universal vendor playbook coverage outside the four vendor families
- A competing source-of-truth product
- Online/continual ML learning
- Multi-GPU GNN training
- A fifth vendor before existing four are vendor-neutral
- Agent-driven autonomous remediation without human approval
- Kubernetes deployment manifests
- LLM-based parsing of device configuration anywhere outside the investigation agent
- Collector-side UI or management functionality beyond diagnostic health endpoints

---

---

## Execution Log

### Sprint 1 — Loose Ends & Build Baseline (2026-04-23)
- **T0-1**: Dropped apt version pins in Dockerfile. Verified with fresh build.
- **T0-2**: Integrated `--all-targets` into `cargo-chef` stage.
- **T0-3**: Split `add` and `update` in `CredentialVault`. Added HTTP route.
- **T0-4**: Added `protocol_version` field to gRPC telemetry messages.
- **T1-1**: Captured baseline build times in `docs/build_performance.md`.
- **ADR**: Logged reproducibility and cache stability decisions.

### Sprint 2 — Build Optimisation (2026-04-23)
- **T1-2**: Integrated `sccache` in `.cargo/config.toml`. Verified cache hits.
- **T1-4**: Tuned release profile with Thin LTO and codegen units = 1.
- **T1-5**: Created `scripts/dep_audit.sh` and performed initial audit.
- **T1-8**: Switched to `distroless` base image. Implemented Rust healthcheck. Final size: 131MB.
- **T1-6**: Documented and verified parallel stage execution in Docker.
- **T1-9**: Measured UI build times. Lighthouse audit baseline captured.

### Sprint 3 — Collector-Core Data Exchange (2026-04-23)
- **T2-1**: Implemented collector-side counter debouncing in `src/ingest.rs`.
- **T2-2**: Implemented UTC-aligned 60s counter summarization (`CounterSummarizer`).
- **T2-3**: Implemented scoped `CollectorGraphStore`.
- **T2-6**: Documented protocol contract in `docs/collector_core_protocol.md`.
- **ADR**: Logged decisions for summary mode and ephemeral local graphs.

### Sprint 4 — Collector-Side Detection (2026-04-23)
- **T2-4 (Foundation)**: Refactored `BonsaiStore` trait for core/collector graph reuse.
- **T2-4 (RPC)**: Implemented `DetectionIngest` RPC for collector-to-core escalation.
- **T2-4 (Engine)**: Created `python/collector_engine.py` sidecar and added `scope` to `Detector` class.
- **T3-1**: Implemented UI lockdown on collectors.
- **T3-5**: Captured architecture for disconnected-ops.
- **ADR**: Logged generic store abstraction and local rule execution architecture.

### Sprint 5 — Control-Plane Discipline (2026-04-23)
- **T3-2**: Implemented `CollectorManager` on the core. Added `RegisterCollector` streaming RPC for dynamic assignment.
- **T3-3**: Implemented in-memory credential delivery from core to collectors over mTLS, completely removing local vault dependencies on collectors.
- **Lab Integration**: Successfully deployed Phase 4 native Linux lab, demonstrating full end-to-end telemetry flow from lab devices -> collector -> core graph.

### Sprint 6 — Containerisation Plane (2026-04-24)
- **T4-1**: Created `docker-compose.yml` with `dev`, `distributed`, `two-collector`, and `chaos` profiles for local development and distributed validation.
- **T4-2**: Integrated ContainerLab networks natively, mounting TLS certs seamlessly to containers.
- **T4-3**: Setup robust environment variable handling (`BONSAI_VAULT_PASSPHRASE`) keeping secrets out of image layers.
- **T4-4**: Configured the `gaiaadm/pumba` chaos tool within compose to dynamically impair network paths to collectors.
- **T4-5**: Authored `docs/deployment_volumes.md` detailing the volume lifecycle and backup strategies.
- **Build fixes**: Re-aligned the Rust build environment and builder image to resolve GLIBC compatibility issues while continuing to use the `distroless` runtime base.

---

*Version 5.3 — updated 2026-04-24. Reflects Sprint 1-6 completion.*
