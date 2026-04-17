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
