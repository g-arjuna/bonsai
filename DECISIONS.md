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
