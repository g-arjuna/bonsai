# Project Kickoff — Network State Engine (working title)

> This document is the context handoff. Paste it into the first message of any new Claude Code session. It captures the origin thesis, decisions made, scope constraints, and first actions.

---

## 1. Who I Am, What I'm Doing, Why

I am a network engineer with breadth across campus, data center, automation, and telemetry, but an honest sense of "mile wide, inch deep." My goal with this project is **not** to build a product, gather GitHub stars, or monetize. It is to:

1. Build a working prototype that implements the closed-loop autonomous network operations concept from Google's ANO framework paper, at lab scale, on ContainerLab
2. Upskill myself from a network engineer who knows *what* systems do into a systems person who understands *why* they work
3. Prove that an open source, LLM-agnostic stack can do what commercial platforms (Selector, Forward Networks, ServiceNow ITOM) do
4. Present findings — a blog, a NANOG/AutoCon talk, a working demo — regardless of whether the project sustains beyond that

Resources: Claude Pro subscription, personal time. No team, no budget, no deadlines from anyone but me.

**The project is MIT-licensed open source from day one, in a public GitHub repo.** A potential future integration with ServiceNow (my work context) comes *after* the open source core is working. Not before.

---

## 2. The Origin Thesis

Google's *Autonomous Network Operations framework for CSPs* (June 2025, Google Cloud blog) describes an AI-first architecture where:
- Continuous telemetry streams from network devices feed a live state model
- The state model is graph-native (Cloud Spanner Graph) with real-time relationships and historical state
- Graph Neural Networks and Gemini reason over the graph
- Closed-loop automation detects anomalies, predicts failures, and applies remediations
- Customers reporting results: Bell Canada (25% MTTR reduction), Deutsche Telekom (RAN Guardian multi-agent system)

The paper is describing **autonomous networking at Level 4/5** of the TM Forum autonomous networks scale. The same architecture at lab scale is buildable by one engineer with the right tools.

**My project replicates this architecture on ContainerLab as a proof of concept that the full loop — ingest → graph → detect → heal → verify — can be built entirely on open source primitives.**

---

## 3. Core Problem Statement (the gap I'm filling)

The open source ecosystem has strong individual components but no integrated **streaming-first, graph-native network state engine**. Specifically:

| Layer | Open Source Today | Status |
|---|---|---|
| Multi-vendor streaming telemetry ingestion | gNMI + OpenConfig + gNMIc / Telegraf | Mature |
| Time-series metrics storage | Prometheus / InfluxDB | Mature |
| Normalized tabular state with history | SuzieQ (Parquet) | Good |
| Intended state / source of truth | Nautobot / NetBox | Mature |
| **Live graph of operational state, streaming updates** | **Nothing complete** | **The gap** |
| **LLM-agnostic query layer over graph state** | **Nothing complete** | **The gap** |
| **Closed-loop predict/heal pipeline on OSS** | **Nothing complete** | **The gap** |

Forward Networks and Selector are commercial because they've built the graph + inference layer. That is the exact layer this project targets.

---

## 4. The Delta This Project Contributes

A single system that:
- Takes gNMI Subscribe streams directly (no Telegraf intermediate hop)
- Maintains a live topology + state graph that updates as telemetry arrives
- Preserves temporal queries ("what did the graph look like at time T")
- Exposes a Cypher query interface that any consumer (Python, LLM, Grafana, ServiceNow) can use
- Includes rules-based anomaly detection and healing as the initial loop
- Progresses to ML-based prediction and remediation as data accumulates

**Nothing in open source does this integrated thing today.** That is the contribution.

---

## 5. Architecture (high level)

```
┌─────────────────────────────────────────────────────────────┐
│  ContainerLab topologies — DC spine-leaf AND SP edge/core   │
│  Real-vendor primary targets:                               │
│  - Nokia SR Linux (container, free)                         │
│  - Cisco IOS-XRd (container, free for lab — CCO account)    │
│  - Juniper cRPD / vJunosEvolved (container, free w/ account)│
│  - Arista cEOS (container, free w/ registration)            │
│  Open-source reference implementations:                     │
│  - Holo (Rust) and FRR — for rapid iteration and fallback   │
│  gNMI enabled on every node, OpenConfig YANG paths          │
└─────────────────┬───────────────────────────────────────────┘
                  │ gRPC/gNMI Subscribe streams
                  ▼
┌─────────────────────────────────────────────────────────────┐
│  RUST CORE — the engine                                     │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ gNMI subscriber pool (tokio async, per-device)      │   │
│  │   → parses protobuf, normalizes OpenConfig paths    │   │
│  ├─────────────────────────────────────────────────────┤   │
│  │ Graph writer (batched, debounced)                   │   │
│  │   → writes to embedded graph DB                     │   │
│  │   → maintains temporal version chain                │   │
│  ├─────────────────────────────────────────────────────┤   │
│  │ Query API (Cypher over REST or gRPC)                │   │
│  └─────────────────────────────────────────────────────┘   │
└─────────────────┬───────────────────────────────────────────┘
                  │ REST / gRPC
                  ▼
┌─────────────────────────────────────────────────────────────┐
│  PYTHON SDK & APPLICATION LAYER                             │
│  - Query helpers, anomaly rules (Phase 4)                   │
│  - ML pipeline: autoencoder, LSTM, remediation model (P5)   │
│  - Remediation push (gNMI Set back to devices)              │
└─────────────────────────────────────────────────────────────┘
```

Key architectural principles:
- **Streaming-first, not polling.** No timers, no scheduled scrapes. Everything flows as telemetry arrives.
- **Graph-native, not relational.** Relationships are first-class. Topology traversal is the primary query pattern.
- **Temporal by design.** Every state change is versioned so we can reconstruct graph state at any past time.
- **LLM-agnostic.** The query API is Cypher. Any LLM can call it via MCP or function-calling. No coupling to a specific model.

---

## 6. Technology Stack — With Rationale

### Rust core
- **Language**: Rust (stable, edition 2024)
- **Async runtime**: tokio
- **gRPC**: tonic
- **gNMI protobuf**: generate from the official `openconfig/gnmi` .proto files via prost
- **Reference implementation for gNMI client**: study the gNMI client code inside the **Holo project** (MIT-licensed Rust routing protocol suite, actively developed, has native gNMI support). Holo is also useful as a *peer* in ContainerLab — it runs as a containerized routing daemon, so I can test against a pure-Rust stack.

Rationale for Rust over Go or Python: performance matters for streaming ingestion at scale, memory safety matters when parsing untrusted protobuf, and the async story is mature. Main downside — smaller community in network automation space — is acceptable because this is a learning project and the Rust core doesn't need mass contributors.

### Graph database — CRITICAL UPDATE FROM EARLIER DISCUSSION
My prior recommendation was **Kuzu**. As of October 2025, Kuzu was acquired by Apple and the repo is archived. This does not block the project but it changes the options:

1. **Kuzu v0.11.3 (archived)** — still works, MIT license, Rust bindings exist. Usable but no future development.
2. **Ladybug** — community fork of Kuzu by Arun Sharma (ex-Facebook, ex-Google), intends to be "DuckDB for graphs" and a 1:1 Kuzu replacement. Early stage, community-driven.
3. **Bighorn** — Kineviz's fork of Kuzu, focused on their GraphXR use case.
4. **ArcadeDB** — Apache 2.0, multi-model, runs as server (not embedded), Cypher support via OpenCypher (97.8% TCK pass). More mature but different deployment model.
5. **Memgraph** — BSL 1.1 (not strictly open source by OSI definition), in-memory, fast, real-time focused.

**Research decision needed in Phase 1**: benchmark Kuzu v0.11.3 (frozen but mature) vs Ladybug (active, may be unstable) vs ArcadeDB (mature, server not embedded). My default leaning: start with Kuzu v0.11.3 because it's stable and the code works. Plan migration path to Ladybug if it stabilizes. Revisit at end of Phase 2.

### Python layer
- **FFI**: PyO3 for tight binding to Rust core, OR a REST API if PyO3 proves too complex early. Default: REST API first (simpler), PyO3 later (once API stable).
- **ML**: PyTorch for autoencoder/LSTM, scikit-learn for IsolationForest (simpler baseline).
- **Rules engine**: plain Python initially, optionally explore Drools-like DSL later.

### Lab environment
- **ContainerLab** for topology management (already familiar).
- **Primary vendor targets** (all freely accessible, account registration required for some):
  - **Nokia SR Linux** — containerized, excellent gNMI/OpenConfig support, relevant to both DC and SP
  - **Cisco IOS-XRd** — containerized IOS-XR, SP-grade, good gNMI support
  - **Juniper cRPD** and **vJunosEvolved** — containerized Junos for Juniper-shop credibility
  - **Arista cEOS** — mature gNMI, widely deployed in DC
- **Open-source reference implementations**: Holo (Rust, MIT) and FRR. Useful for fast iteration when heavyweight commercial containers are slow to boot, and as a truth-test that my code doesn't depend on vendor quirks.
- Goal: the same pipeline code runs against all of the above with only topology YAML changes. If a new vendor requires code changes, the normalization layer isn't doing its job.

### Observability for the project itself
- Use the same OpenTelemetry Rust SDK to instrument my own code so I can debug the ingestion pipeline in Grafana/Prometheus.

---

## 7. Scope — What's In, What's Out

### IN
- **Data center AND service provider topologies** (spine-leaf, SP edge/core, mixed)
- gNMI / OpenConfig only for ingestion
- **Four commercial vendor families as primary targets**: Nokia SR Linux, Cisco IOS-XRd, Juniper cRPD/vJunosEvolved, Arista cEOS
- Holo and FRR as open-source reference implementations
- YANG path focus: interfaces, BGP, OSPF, IS-IS, LLDP, platform, **plus SP-specific paths**: openconfig-mpls (LSPs, label-switched paths), openconfig-segment-routing (SR policies, SIDs), optionally openconfig-network-instance for VRF/L3VPN
- Closed-loop healing via gNMI Set back to devices
- Single-host deployment (no distributed ingestion for v1)

### OUT (explicit non-goals, say no to these)
- Campus and wireless environments (different telemetry characteristics, different vendor APIs)
- Optical transport layer (openconfig-terminal-device, transponders) — interesting but out of scope for v1
- SNMP ingestion (no legacy support)
- NETCONF (gNMI only, even though SP gear often still speaks NETCONF first)
- Kubernetes deployment, HA, clustering
- Multi-tenancy, RBAC, authentication beyond basic TLS
- Production-grade durability (WAL, replication)
- Admin/configuration UI (view-only UI in Phase 6, no writing via UI)
- Any vendor beyond the primary four for the first 6 months

Every feature request outside this list gets a polite "not in scope" and goes on a backlog.

---

## 8. Phased Roadmap — Practical Goals that Shift into the Complex Problem

Each phase is a self-contained milestone. If I stop after any phase, what I built still has value.

### Phase 1 — The Heartbeat (weeks 1–4)
**Goal**: A Rust binary that subscribes to gNMI streams from a ContainerLab topology (Holo + FRR nodes) and prints normalized, decoded updates to stdout.

**Success criteria**:
- `cargo run` subscribes to N devices in parallel
- Handles subscription lifecycle (connect, authenticate, subscribe paths, reconnect on drop)
- Prints interface counter updates, BGP state changes as human-readable JSON
- Runs for 24 hours continuously without leaking memory or dropping subscriptions

**Learning outcome**: async Rust, tonic/gRPC, protobuf, OpenConfig YANG paths, gNMI subscription modes (STREAM/POLL/ONCE, ON_CHANGE vs SAMPLE).

### Phase 2 — The Graph (weeks 5–12)
**Goal**: The Rust core writes the telemetry stream into an embedded graph DB. Live Cypher queries return current + historical state across DC and SP constructs.

**Success criteria**:
- Nodes: Device, Interface, BGP-Neighbor, OSPF-Adjacency, IS-IS-Adjacency, **MPLS-LSP**, **SR-Policy**, **Network-Instance** (VRF)
- Edges: HAS_INTERFACE, CONNECTED_TO (LLDP-derived), PEERS_WITH, ADJACENT_TO, **TRAVERSES** (LSP-over-links), **BINDS_TO** (SR-policy to SID list), **IN_VRF**
- Properties updated from telemetry with millisecond timestamps
- Cypher query "show me every BGP session whose state changed in the last 60 seconds" returns in <100ms
- Cypher query "show me every LSP that currently traverses interface X" returns correct paths
- Temporal query: "what was the state of the graph 5 minutes ago" works
- Same queries run correctly against topologies built from different vendor combinations (vendor-neutrality proof)

**Learning outcome**: graph data modeling for both DC and SP constructs, Cypher, graph DB internals, temporal versioning patterns, debouncing high-frequency updates, OpenConfig path normalization across vendors.

*Note: Phase 2 is longer than originally planned (12 weeks instead of 10) because the graph model now covers SP constructs and four real-vendor normalizations.*

### Phase 3 — The Python Layer (weeks 13–18)
**Goal**: A Python SDK that queries the graph, computes derived metrics, and can push config changes back via gNMI Set.

**Success criteria**:
- `pip install` the SDK from local path
- Python helper functions for common queries (show device health, show BGP peer status, show interface utilization trends, show LSP state, show SR policies)
- Script that detects a link flap in the graph and opens a (fake for now) ServiceNow-style ticket locally
- Remediation: Python can push a gNMI Set through the Rust core back to a device and observe the graph state recover

**Learning outcome**: PyO3 or REST API design, SDK ergonomics, Python-side state management, round-tripping back into the network.

### Phase 4 — Rules-Based Detect and Heal (weeks 19–26)
**Goal**: Deterministic anomaly detection and closed-loop healing.

**Success criteria**:
- Rule engine in Python with at least 10–15 rules covering both DC and SP patterns (BGP session flap, interface error spike, MTU mismatch, adjacency loss, CPU high, link asymmetry, LSP re-optimization events, SR-policy bindings failing, etc.)
- Each rule fires a Detection Event into the graph
- Pre-defined remediation playbooks respond to specific events
- End-to-end demo: I break something in ContainerLab → system detects → heals → graph shows recovery — all without manual intervention
- Demo works with at least two different vendor combinations to prove normalization
- All events, detections, and remediations are recorded in the graph for later ML training

**Learning outcome**: rule engine design, event-driven architecture, safe automated config changes, building ground-truth datasets.

### Phase 5 — ML Prediction and Remediation Selection (months 7–10)
**Goal**: The part the paper is really about.

**Success criteria**:
- Autoencoder (or IsolationForest) for per-entity anomaly detection on graph metric time-series
- LSTM trained on sequences of graph state to predict link failures 5–15 minutes before they happen
- Classifier that given current graph state picks the best remediation from a set
- Demo: a link gradually degrading (no threshold crossed) triggers the ML prediction alert and the system preemptively shifts traffic before actual failure

**Learning outcome**: applied ML on graph time-series data, feature engineering from graph state, model deployment and inference inside a streaming system.

### Phase 6 — The MVP Feel (weeks 44–52)
**Goal**: A minimal web UI that makes the system demonstrable and that conveys the *feel* of the project without becoming a product.

**Strictly scope-constrained UI** — view-only, no admin, no configuration, no user management. Three views only:
1. **Live topology graph** — nodes and edges rendered with real-time state (colors reflect health, thickness reflects utilization). Cytoscape.js or D3.js.
2. **Event stream** — scrolling feed of detections, ML predictions, remediations as they happen. Each event clickable to show the graph-state-at-time.
3. **Closed-loop trace** — when a detection → remediation → recovery cycle completes, show a visual timeline of what the system did, with the graph state at each step.

**Technology default**: Svelte + D3.js for minimum build-system complexity. Alternative: React + Cytoscape.js if Svelte learning curve is unwelcome. Back end is already the Cypher query API built in Phase 3 — UI is a pure consumer.

**Success criteria**:
- Can run locally, single binary or single `docker compose up`
- Renders a ContainerLab topology in real time
- During a rehearsed failure-recovery demo, every step is visible in the UI without me touching the CLI
- Screenshots and a 2-minute video are ready for Phase 7

**Explicit non-goals for the UI**: device config editing, user authentication, dashboards with gauges, any form of settings page. If a feature doesn't directly contribute to watching the closed loop work, it doesn't belong in the UI.

**Learning outcome**: front-end architecture for real-time graph visualization, WebSocket or SSE event streaming, API-to-UI contract design.

### Phase 7 — The Presentation (months 12+)
**Goal**: Consolidate into blog posts, a conference talk (NANOG or AutoCon), and a demo video. This is the career-facing artifact. Deliberately planned, not an afterthought.

Outputs:
- Written blog series (3–5 posts) — the journey from paper to working system, architectural decisions, things that broke, what ML added over rules
- Conference submission — NANOG and/or AutoCon (EU/US)
- Demo video — 2 minutes polished, 15 minutes detailed walkthrough
- Public repo with README, quickstart, and sample ContainerLab topologies

*Note: overall timeline extended by ~10 weeks vs v1 of this document because scope expanded to include SP, four real vendors, and a minimal UI. That's the honest trade-off and it's the right one.*

---

## 9. Research Items — Do These First or Early

Items I genuinely don't know the answer to. Investigate in Phase 1, document decisions in a `DECISIONS.md` log.

1. **Vendor container image access** — register for Nokia (SR Linux is freely pullable from GHCR), Cisco CCO (for XRd), Juniper (for cRPD/vJunosEvolved), and Arista (for cEOS). Do this in the very first week so no phase is ever blocked waiting on account approvals.
2. **Graph DB choice** — Kuzu v0.11.3 (archived but stable) vs Ladybug fork (active, early) vs ArcadeDB (mature, server-mode). Spend 2–3 days benchmarking with a small synthetic graph workload before committing.
3. **Vendor gNMI path quirks** — each vendor implements OpenConfig YANG slightly differently. Build a small test harness in Phase 1 that subscribes to the same path across all four and documents where normalization is needed. This is the foundation for vendor-neutrality claims.
4. **OpenConfig SP models** — survey the maturity of openconfig-mpls, openconfig-segment-routing, openconfig-network-instance across the four target vendors. Some may be partial. Pick the subset that's well-supported across all four for Phase 2.
5. **Holo / FRR as fast-iteration targets** — confirm both support the gNMI paths we care about well enough to be a viable fast-iteration alternative when the commercial containers are slow to boot.
6. **Temporal graph pattern** — there are multiple approaches: (a) append-only with time-indexed edges, (b) snapshots at intervals, (c) bi-temporal modeling with valid/transaction time. Survey Neo4j temporal patterns, Memgraph time-travel, academic graph-DB-time literature before deciding.
7. **gNMI Subscribe backpressure** — what happens when ingest outpaces graph write? Research existing backpressure patterns (e.g., how gnmi-gateway from Netflix handles this).
8. **PyO3 vs REST** — build a minimal hello-world both ways, decide based on ergonomics and latency.
9. **ML data pipeline** — how do I export graph time-series snapshots as training data? Parquet dumps on a schedule? Kafka? Direct graph queries into pandas?
10. **UI framework choice** (Phase 6 research, not blocking) — Svelte + D3 vs React + Cytoscape.js. Decide based on a quick spike closer to the time.

---

## 10. Key References — Libraries, Projects, Papers to Have at Hand

**The origin thesis**
- Google ANO framework blog post (June 2025) — the architectural vision
- Bell Canada + Google Cloud announcement — proof point for 25% MTTR reduction
- Deutsche Telekom RAN Guardian — proof point for multi-agent autonomous operations

**Protocols and standards**
- OpenConfig gNMI spec (openconfig/gnmi) — the protocol definition
- OpenConfig YANG models (openconfig/public) — vendor-neutral data models, including openconfig-mpls and openconfig-segment-routing for SP

**Commercial vendor container images (primary targets)**
- Nokia SR Linux — ghcr.io/nokia/srlinux, free
- Cisco IOS-XRd — available via Cisco CCO account, free for lab
- Juniper cRPD and vJunosEvolved — available via Juniper account
- Arista cEOS — available via arista.com with free registration

**Reference implementations and collectors**
- Holo project (holo-routing/holo) — Rust routing protocols with gNMI, MIT licensed, actively developed
- gnmi-gateway (openconfig/gnmi-gateway, Netflix) — reference architecture for distributed gNMI collection
- gNMIc (Nokia-maintained, github.com/openconfig/gnmic) — mature Go gNMI client for reference

**Related open source networking tools**
- SuzieQ (netenglabs/suzieq) — normalization patterns, multi-vendor state modeling
- Nautobot (nautobot/nautobot) — intended state model we'll eventually reconcile against
- NetClaw (automateyournetwork/netclaw) — MCP-pattern agentic networking reference

**Graph databases to evaluate**
- Kuzu (kuzudb/kuzu, archived) — v0.11.3 is the last usable release
- Ladybug — community fork of Kuzu to track
- ArcadeDB — backup graph DB choice, multi-model, Apache 2.0

**Lab environment**
- ContainerLab (srl-labs/containerlab) — topology management

---

## 11. Anti-Patterns — Things That Will Kill This Project

- Adding SNMP or NETCONF "because a user asked"
- Supporting campus / wireless before DC and SP both work
- Adding optical transport, encrypted overlays, or other layers before the core loop is solid
- **Letting the Phase 6 UI grow into a product** — it is a demo view, not a dashboard. No admin panels. No configuration pages. No user management. If a feature doesn't directly help visualize the closed loop, reject it.
- Trying to deploy to production Kubernetes before v0.1 works on my laptop
- Writing blog posts about the project before Phase 2 works
- Accepting contributions that expand scope rather than deepen existing scope
- Rewriting from Rust to Go because contributors complain
- Comparing myself to Forward Networks or Selector — they have teams, I'm one person learning
- Giving up at month 10 because it "isn't famous" — that was never the goal
- Chasing more vendors before the current four work vendor-neutrally — breadth before depth of normalization is a trap

---

## 12. First Actions (literal first session agenda)

In order, in Claude Code:

1. **Pick a project name.** Something short, memorable, doesn't clash with existing projects. Not `netgraph` (taken). Something like `netstate`, `topograph`, `livewire`, `kelvin` (lord kelvin / network temperature metaphor), or whatever resonates. Decide in session 1.
2. **Register for vendor container access** — do this on day one, before any code. Nokia (GHCR pull, immediate), Cisco CCO account, Juniper account, Arista account. Some take a day or two to approve. This is the unblock-the-future task.
3. **Create GitHub repo, MIT license, initial commit.** Empty but public.
4. **Write the README first.** Problem statement, architecture sketch, scope (DC + SP, four commercial vendors + open source references), roadmap with 7 phases, a "current status: nothing works yet" banner. This README is the north star.
5. **Create `DECISIONS.md`** — append-only log of technical decisions with date and rationale. First entry: "Chose Rust + embedded graph DB + streaming-first + real vendor targets, over Python or Go or polling model or open-source-only, because…"
6. **Set up initial ContainerLab topology** — start with Holo or FRR containers for immediate iteration while vendor accounts are being approved. 3-node topology, gNMI enabled, addresses + BGP configured. Commit topology YAML to repo under `lab/fast-iteration/`.
7. **Once vendor accounts are approved**, add topologies under `lab/real-vendors/` — at minimum a Nokia SR Linux 3-node lab and a Cisco XRd 3-node lab. Juniper and Arista can follow.
8. **Bootstrap Rust project** — `cargo new` with tonic + tokio + serde + prost. Get it to compile.
9. **Phase 1 first deliverable**: a binary that connects to one gNMI target (SR Linux is easiest to start with), runs a Subscribe for `/interfaces/interface/state/counters`, and prints updates. Just one device, one path. That's the hello world.

Everything after step 9 is forward motion.

---

## 13. How to Resume Context in Claude Code

Paste this entire document into the first message of any new Claude Code session. Add one line at the top: *"I am resuming work on the project described below. My current phase is X, last completed task Y, next task Z."*

Claude will have the full architectural context, the scope constraints, the anti-patterns, and the roadmap. The collaboration will pick up cleanly.

Update this document as decisions get made. It is a living spec, not a fixed charter.

---

*Document version: 1.1 — expanded to include Service Provider scope, four commercial vendor targets (Nokia, Cisco, Juniper, Arista) as primary, Holo/FRR as open-source references, and Phase 6 minimal web UI.*
*The project has no name yet. Name it in session 1 and rename this file.*
