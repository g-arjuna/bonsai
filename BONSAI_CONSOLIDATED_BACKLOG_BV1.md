# BONSAI — Backlog Bravo Series, v1 (Bv1.0)

> **A fresh, revised consolidation.** Supersedes v2-v12. Produced 2026-05-04 after end-to-end review of the post-v12 codebase.
>
> **What "Bravo" means**: the alpha series (v2-v12) traced a long arc from credentials vault through controller-less framing, environment archetypes, path catalogue plugin loader, enrichment infrastructure, OutputAdapter framework, HIL graduated remediation, the v10 testing tier, the v11 external-infra-and-lab work, and the v12 memory + binary fixes. **Most of that infrastructure has landed and works.** The Bravo series resets the focus around the one thing repeatedly deferred: **graph-native value extraction**.
>
> **Top priority is graph development.** Not GNN — that's the destination, not the next step. The intermediate work that's been postponed is:
> - Real graph queries instead of relational `MATCH (x) RETURN x.field` patterns
> - In-database traversal (shortest path, blast radius, dependency walk) instead of Rust-side Vec-and-loop
> - A graph explorer UI for operators and AI agents to ask topology questions interactively
> - Graph algorithms (centrality, clustering) that surface which devices matter
> - A test framework that exercises graph queries, not just CRUD on schema tables
>
> This unblocks Path A embeddings (which need the graph as a real graph) and Path B GNN (which needs graph-shaped training data).
>
> **All open items from prior backlogs are absorbed**. Every backlog v2-v12 was reviewed; pending items are either folded into a Bv1 tier or explicitly carried forward in Tier 7. Nothing is dropped silently.
>
> **README and other docs at lowest priority** (Tier 8). Important but not blocking; cleaned up after the substantive work.

---

## Table of Contents

1. [State of the Code](#state)
2. [Pending Items From Prior Backlogs](#pending)
3. [TIER 1 — Graph-Native Value Extraction](#tier-1) ⚡ TOP PRIORITY ⚡
4. [TIER 2 — Path A → Path B GNN](#tier-2)
5. [TIER 3 — Investigation Agent](#tier-3)
6. [TIER 4 — Outstanding Memory and Binary Hygiene](#tier-4)
7. [TIER 5 — Test Coverage Gaps](#tier-5)
8. [TIER 6 — UI/UX Completion](#tier-6)
9. [TIER 7 — Strategic Carryover](#tier-7)
10. [TIER 8 — Documentation Refresh (lowest priority)](#tier-8)
11. [Execution Order](#execution-order)
12. [Guardrails](#guardrails)

---

## <a id="state"></a>State of the Code — Verified 2026-05-04

This section captures what's *actually working* right now, derived from code review rather than backlog declarations. Everything below is verified by reading the source.

### What works (production-shaped)

- **Distributed core/collector mode** with mTLS, disk-backed queue, summary-mode counter forwarding, credential delivery via assignment messages
- **Graph schema** (41 CREATE statements covering 18+ node types, 22+ relationship types) for Device, Site, Environment, Interface, BgpNeighbor, BfdSession, LldpNeighbor, StateChangeEvent, DetectionEvent, Remediation, RemediationTrustMark, RemediationProposal, SubscriptionStatus, EnrichmentProperty, VLAN, Prefix, Application, Incident, MigrationMarker
- **Path catalogue with plugin loader** — schema v2 with environment + vendor_scope + vendor_only + fallback_for; profile_name_for_role hardcode is gone
- **First-run setup wizard** (`/setup`) with environment + sites + credentials flow; auto-redirects on fresh install
- **NetBox enricher and ServiceNow CMDB enricher** — both implementations exist with tests; ServiceNow Event Management push (em_event) works as an OutputAdapter
- **Four output adapters with tests** — Prometheus remote-write, Splunk HEC, Elastic ingest, ServiceNow EM
- **HIL graduated remediation** — TrustState model, Pending Approvals workspace, graduation logic, rollback window, per-environment defaults, audit-logged
- **Audit subsystem** — `src/audit.rs`, JSONL daily files, retention, structured purpose-tagged credential resolves, audit export CLI
- **External infrastructure compose** — `docker/compose-external.yml` with NetBox + Splunk + Elastic + Prometheus + Grafana profiles
- **DC + SP ContainerLab topologies** — `lab/dc/dc-evpn-srv6.clab.yml` (8 NOSes, EVPN/SRv6/L3VPN), `lab/sp/sp-mpls-srte.clab.yml` (9 NOSes, MPLS/LDP/RSVP-TE/SR-MPLS/L3VPN); plus Makefiles for persistent up/down/reset
- **Lab fault catalogue** at `lab/fault_catalog.yaml` driving the chaos harness
- **Iterative AI feedback loop infrastructure** — `tests/api_driver/`, `tests/ui_driver/` (Playwright + a11y + screenshot diff + collectors + incidents specs), `tests/event_driver/`, `tests/chaos_harness/`; `/api/_test/status` unified status endpoint; `docs/ai_feedback_protocol.md`
- **Memory architecture fixes from v12** — LadybugDB buffer pool capped at `min(2 GB, 25% RAM)` for core (F-1), graph-writer LruCache eviction (F-3), bus capacity reduced to 512 with slow-subscriber metric (F-2), startup phase timing logs (F-9), Operations workspace live polling (F-12), seed scripts have `--reset` flags and `restart: unless-stopped` policies (F-5/F-6)
- **CI workflows** — ci.yml, build-baseline.yml, feedback-loop.yml, memory-budget.yml, nightly-integration.yml, release.yml, screenshot-diff.yml, startup-time.yml
- **Volume backup scripts** + deployment_segmentation doc + `bonsai self-test` (presumed; verify in T4-3)

### What's incomplete or open

- **Graph queries are mostly relational** — single-table MATCH, no multi-hop patterns, no shortestPath, no graph algorithms (Tier 1)
- **`path_handler` does BFS in Rust** instead of using the graph DB's traversal (Tier 1 T1-2)
- **No graph explorer UI** — operators cannot interactively query the graph (Tier 1 T1-5)
- **F-4 binary self-containment NOT done** — `LBUG_SHARED=1` still unconditional in `.cargo/config.toml`; binary still requires `liblbug.so.0` and `LD_LIBRARY_PATH` (Tier 4)
- **Only 5 unit tests in `src/graph/mod.rs`** for a 2882-line module (Tier 5)
- **No GNN, no embeddings, no investigation agent, no signals collector** — strategic items deferred across multiple backlogs (Tiers 2, 3, and 7)
- **Operator path overrides have data model only** — no UI workspace, no resolution audit, no documented examples (Tier 7)
- **PDI live tests** still pending operator-supplied credentials (Tier 7)
- **Schema migration tooling, bitemporal schema, NL query, controller adapters** — all deferred to Tier 7

---

## <a id="pending"></a>Pending Items From Prior Backlogs — Inventory

For full traceability. Every backlog v2-v12 was reviewed; this is what remains open. Mapped to the Bv1 tier each item now lives in.

| From | Item | Status | New home |
|---|---|---|---|
| v9 T4-1 | Operator path override scopes (data model exists; resolution + UI pending) | Partial | Tier 6 |
| v9 T4-2 | Override management UI | Open | Tier 6 |
| v9 T4-3 | Subscription resolution audit | Open | Tier 6 |
| v9 T5-2 | Catalogue plugin install command | Open | Tier 7 |
| v9 T6-7 | AIOps readiness checklist | Open | Tier 7 |
| v9 T7 | Signals (syslog + traps) | Open | Tier 7 |
| v9 T8 | Path A graph embeddings | Open | Tier 2 |
| v9 T8 | Path B GNN | Open | Tier 2 |
| v9 T9 | Investigation agent | Open | Tier 3 |
| v9 T10 | Controller adapters | Demand-driven | Tier 7 |
| v9 T11 | NL query, bulk CSV onboarding, scale architecture, S3 archive backend | Open | Tier 7 |
| v10 T2-4 | ServiceNow PDI live test | Awaiting operator PDI | Tier 5 |
| v10 T2-5 | ServiceNow EM push live | Awaiting operator PDI | Tier 5 |
| v10 T2-6 | HIL e2e test | Open | Tier 5 |
| v10 T4-3 | Nightly integration in CI | Workflow exists; verify it runs | Tier 5 |
| v10 T4-5 | cargo-mutants on critical modules | Open | Tier 5 |
| v11 T2-3 | Campus topology (DC + SP exist; campus deferred) | Open | Tier 7 |
| v12 T0-4 | Static-link lbug for self-contained binary (F-4) | NOT DONE | Tier 4 |
| v12 T2-2 | Release artefact pipeline | release.yml exists; verify it produces static binaries | Tier 4 |
| v12 T2-3 | `bonsai self-test` subcommand | Open or unverified | Tier 4 |
| v12 T4-3 | Detection-firing chaos matrix | Harness exists; matrix output in test_results/ to verify | Tier 5 |

---

## <a id="tier-1"></a>TIER 1 — Graph-Native Value Extraction ⚡ TOP PRIORITY ⚡

**Why this is Tier 1**: bonsai's pitch is "graph-native network state engine." The graph schema is rich (41 tables) but the *queries* are mostly single-table relational lookups. The most consequential code finding from this review:

> `src/http_server.rs::path_handler` loads all `(a)-[:CONNECTED_TO]->(b)` pairs into a `Vec<(String, String, String, String)>` and walks a BFS in Rust. **It does not use the graph database to compute the path.**

This is using LadybugDB (Kuzu) as a relational store. Bonsai gets none of the graph-native value the architecture promised. Until this changes, Path A embeddings have no real graph to embed; Path B GNN has no real graph topology to learn from; the investigation agent has no useful tool to ask "what's downstream of this fault."

The work below moves bonsai from "graph schema with relational access" to "graph-native query and traversal."

### T1-1 (Bv1) — Multi-hop pattern queries

**What**: replace single-table MATCH patterns with proper multi-hop queries throughout the read surface. Examples:

- "What devices are downstream of leaf1?" → `MATCH (leaf1:Device {address: $a})-[:HAS_INTERFACE]->(:Interface)-[:CONNECTED_TO]->(:Interface)<-[:HAS_INTERFACE]-(neighbor:Device) RETURN DISTINCT neighbor`
- "What applications run on devices with site=london?" → `MATCH (s:Site {name: 'london'})<-[:LOCATED_AT]-(d:Device)-[:RUNS_SERVICE]->(a:Application) RETURN a, count(d)`
- "What detection events fired on devices in environment X within window Y?" → `MATCH (env:Environment {id: $eid})<-[:BELONGS_TO_ENVIRONMENT]-(:Site)<-[:LOCATED_AT]-(d:Device)<-[:TRIGGERED]-(de:DetectionEvent) WHERE de.fired_at_ns > $since RETURN d, de`

**Where**: `src/graph/queries.rs` (new) — every query is a named function returning typed result rows. Existing inline queries in `src/graph/mod.rs` and `src/http_server.rs` migrate over time.

**Done when**: at least 12 production queries live in `queries.rs`; every UI workspace's API endpoints use them; the file is documented with one query per topic.

### T1-2 (Bv1) — Replace Rust-side BFS with graph-DB traversal

**What**: `path_handler` currently loads all CONNECTED_TO edges into Rust and walks BFS. Replace with the graph DB's variable-length path query:

```cypher
MATCH p = shortestPath(
  (src:Device {address: $src})-[:HAS_INTERFACE|CONNECTED_TO|HAS_INTERFACE*1..20]-(dst:Device {address: $dst})
)
RETURN nodes(p), relationships(p)
```

**Where**: `src/http_server.rs::path_handler` + new `src/graph/queries.rs::shortest_topology_path`.

**Caveat**: Kuzu/lbug 0.15.3 supports variable-length paths and shortest-path queries. Verify the exact syntax in lbug docs; some shortestPath syntax differs from Neo4j Cypher.

**Done when**: path_handler is one Cypher call; benchmark shows it scales to a 200-device topology faster than the current Rust BFS (which is O(E) load + O(V+E) walk every call).

### T1-3 (Bv1) — Blast radius traversal endpoint

**What**: a new endpoint `/api/blast-radius/:device_address?max_hops=3` that returns the set of devices, applications, services, and active detections reachable from the given device within N hops via dependency-relevant edges.

**Why this matters operationally**: when a fault fires on `leaf1`, the most useful view is "what depends on this — what services lose reachability, which applications are affected, which sites are impacted." This is graph-native by definition; it's the kind of query Cypher was designed for.

**Where**: `src/http_server.rs` + `src/graph/queries.rs::blast_radius`.

**Cypher shape**:
```cypher
MATCH (d:Device {address: $addr})
OPTIONAL MATCH (d)-[:RUNS_SERVICE|CARRIES_APPLICATION]->(a:Application)
OPTIONAL MATCH (d)-[:HAS_INTERFACE]->(:Interface)-[:CONNECTED_TO]->(:Interface)<-[:HAS_INTERFACE]-(neighbor:Device)
OPTIONAL MATCH (neighbor)-[:RUNS_SERVICE|CARRIES_APPLICATION]->(neighbor_app:Application)
OPTIONAL MATCH (d)-[:LOCATED_AT]->(:Site)-[:BELONGS_TO_ENVIRONMENT]->(env:Environment)
RETURN d, collect(DISTINCT a) AS direct_apps,
       collect(DISTINCT neighbor) AS neighbors,
       collect(DISTINCT neighbor_app) AS neighbor_apps,
       env
```

Multi-hop variant for `max_hops > 1` uses `[:HAS_INTERFACE|CONNECTED_TO*1..N]`.

**Done when**: the Incidents workspace shows a "blast radius" panel under each incident card listing affected devices/services/applications, derived from this endpoint.

### T1-4 (Bv1) — Graph algorithms surface

**What**: a small library of graph algorithms exposed through the API for operator and AI consumption.

Initial set (all backed by Cypher or by a one-time pre-computation refreshed every N minutes):

- **Device centrality** — degree centrality on the topology graph (`MATCH (d:Device)-[:HAS_INTERFACE]->(i:Interface)-[:CONNECTED_TO]-() RETURN d, count(*)`). Shows which devices carry the most peering — spines and core PEs surface naturally.
- **Site dependency depth** — for each site, count of devices in transitive blast-radius
- **Detection-correlation graph** — which detection rules co-fire on the same devices over a window? Useful for the investigation agent and for tuning.
- **Orphan devices** — devices with no LLDP neighbours (likely subscription issue or genuinely isolated)
- **Subscription health by topology depth** — silent subscriptions per topology level

**Where**: `src/graph/algorithms.rs`. Each algorithm is a function. UI exposes them in a new `/operations/graph-insights` panel.

**Done when**: each algorithm has a Cypher implementation, a unit test against the test graph, and a UI surface that renders results.

### T1-5 (Bv1) — Graph explorer UI workspace

**What**: a new route `/explorer` where an operator (or AI agent) can:

- Type a Cypher-like query (or pick from a query library) and see results as a node-link diagram or table
- Click a node and see all relationships in/out of it
- Pre-populated query library: "show me all devices in environment X", "show me detections in last hour", "show me devices missing enrichment", "show me incidents involving application Y"
- Query history persisted per session
- Read-only — no DELETE/UPDATE/CREATE permitted (sanitised at the API layer with a Cypher allow-list)

**Where**: `ui/src/routes/Explorer.svelte` + `src/http_server.rs::explorer_query_handler` + `src/graph/explorer.rs` (sanitiser).

**Why this matters**: it's the visible affordance that bonsai is graph-native. Today an operator looking at the UI sees nice workspaces but no way to ask their own questions. AI agents are in the same position. A query explorer with a curated library + free-form Cypher (sanitised) gives both.

**Done when**: operator can run a multi-hop pattern query in the explorer and see results; query library has 10+ curated examples; AI feedback protocol doc includes "use the explorer for ad-hoc topology questions."

### T1-6 (Bv1) — Saved queries and named insights

**What**: query results that are operationally interesting (e.g. "devices in environment X with active detections AND no recent enrichment AND > 50% interface utilisation") become saved named queries the operator can run on demand. Each saved query has metadata (created_at, last_run_at, last_result_count).

**Where**: new `SavedQuery` graph node + `src/http_server.rs` endpoints + UI surface in `/explorer`.

**Done when**: the saved-query roster appears as cards on the Operations workspace; clicking a card runs the query and shows results; failures (e.g. schema drift broke a query) surface clearly.

### T1-7 (Bv1) — Graph query test framework

**What**: dedicated test harness for graph queries. Today's `src/graph/mod.rs` has 5 tests for a 2882-line module; most cover CRUD, none cover multi-hop traversal.

The test harness:
- Builds a representative test graph (DC topology with 8 devices, 2 environments, 3 applications, 5 detections)
- Each query in `src/graph/queries.rs` has a test that asserts result structure + counts against the test graph
- Test graph fixtures live in `src/graph/test_fixtures.rs` and are reused by all query tests
- Snapshot testing (e.g. `insta` crate) for query result shapes — drift is loud

**Where**: `src/graph/queries.rs::tests`, `src/graph/test_fixtures.rs`.

**Done when**: every query in `queries.rs` has at least one unit test; snapshot tests exist for the canonical results.

---

## <a id="tier-2"></a>TIER 2 — Path A → Path B GNN

The graph foundation from Tier 1 unblocks this tier. Both deferred from v9 T8 and earlier.

### T2-1 (Bv1) — Path A: graph embeddings as features

**What**: node2vec or GraphSAGE embeddings over the bonsai graph, computed periodically, written back to the graph as `embedding` properties on Device nodes. Tabular ML detectors gain "graph-position embedding" as additional input features.

**Pre-requisites**: Tier 1 multi-hop queries; representative graph populated with NetBox + ServiceNow enrichment.

**Where**: new `python/bonsai_ml/embeddings.py`. Uses `pykeen` or `nodevectors` or `torch_geometric`. Reads the graph via the existing query API (NOT direct LadybugDB access — keeps the ML layer trait-clean). Writes embedding properties via a new `/api/graph/embeddings/upsert` endpoint.

**Done when**: a model card exists documenting the algorithm, hyperparameters, dataset (chaos-run + lab data), and at least one evaluation metric (intrinsic — node-classification accuracy on synthetic labels, or extrinsic — Model A baseline detector quality with vs without embedding features).

### T2-2 (Bv1) — Path B: GNN with message passing

**What**: PyTorch Geometric GNN trained against the enriched graph. Node-level classification task — score Device nodes for anomaly likelihood. Coexists with rule-based detectors and tabular ML; not a replacement.

**Pre-requisites**: Tier 1 + T2-1 + months of archived telemetry from chaos runs against the DC + SP labs.

**Where**: `python/bonsai_ml/gnn/` directory.

**Honest validation requirements** (from earlier guardrails):
- Train on archived chaos runs from at least 30 days
- Compare against rule-based baseline AND tabular MLDetector baseline
- Distinguish detected-by-GNN-only / detected-by-rules-only / detected-by-both
- Model card with limitations explicit

**Done when**: GNN catches at least one cascading-failure class that rules + MLDetector miss, with a documented confusion matrix on a held-out chaos test set; model lives in `python/bonsai_ml/gnn/checkpoints/` with versioned metadata.

### T2-3 (Bv1) — Enrichment-aware data loader

**What**: GNN data loader handles all enrichment-property types (numeric, categorical, text, timestamp). Small schema registry per property type so GNN training doesn't hand-code per-property feature extraction.

**Where**: `python/bonsai_ml/gnn/data_loader.py`.

**Done when**: adding a new enrichment property type (e.g. NetBox Cable IDs) doesn't require GNN code changes — the loader picks up the new property automatically via the schema registry.

### T2-4 (Bv1) — Online inference path

**What**: trained GNN runs as a third detector alongside rules + tabular ML. New detection events get an additional `gnn_anomaly_score` field. UI surfaces it where relevant.

**Where**: `python/collector_engine.py` extension. GNN inference is fast (< 100ms typical for the lab graph) but still off the hot path — runs on a snapshot of the graph every N seconds.

**Done when**: chaos run produces detections with a non-trivial spread of GNN anomaly scores; a high-score detection that rules missed is visible in the UI.

---

## <a id="tier-3"></a>TIER 3 — Investigation Agent

Deferred from v9 T9. Unblocked by Tier 1 graph queries and Tier 2 enrichment + GNN.

### T3-1 (Bv1) — Agent scaffolding with graph-aware tools

**What**: LangGraph-based investigation agent. Triggered by:
- Unmatched detection after 60s
- Operator-issued `/investigate <detection_id>`

Tools:
- `get_blast_radius(device_address, max_hops)` — Tier 1 T1-3
- `get_application_impact(device_address)` — derived from RUNS_SERVICE traversal
- `query_graph(saved_query_name OR cypher_pattern)` — Tier 1 T1-5/T1-6 (sanitised)
- `get_recent_detections(device_address, window_secs)`
- `get_remediation_history(device_address)`
- `summarise(text)` — final narrative
- `propose_playbook(detection_id, rationale)` — writes proposal, never executes

**Mandatory human approval gate** before any agent-proposed action executes. Audit-logged with `purpose=AgentInvestigation`.

**Where**: `python/bonsai_agent/`.

**Done when**: agent can investigate a chaos-injected fault, traverse blast radius, identify the affected application, propose a known playbook, and surface the proposal to the operator. End-to-end test in the chaos harness.

### T3-2 (Bv1) — Agent UI workspace

**What**: `/investigations` route. Lists pending investigations, completed history, the agent's reasoning trail per investigation (chain of tool calls and observations).

**Where**: `ui/src/routes/Investigations.svelte`.

**Done when**: operator can read an agent's reasoning trail and approve/reject proposals from one screen.

### T3-3 (Bv1) — Agent cost controls

**What**: per-investigation token budget (fail-closed if exceeded), daily token budget per operator, visible cost per investigation in the UI. Anthropic API token usage reported to Prometheus metrics.

**Where**: `python/bonsai_agent/budget.py`.

**Done when**: 10 investigations in a day produce a clear cost-per-investigation summary; budget breaches surface as visible UI warnings.

### T3-4 (Bv1) — Agent memory across investigations

**What**: PastInvestigation graph nodes — the agent retrieves prior similar investigations as context for new ones. Reduces token usage on recurring patterns.

**Where**: extends Tier 1 graph schema + `python/bonsai_agent/memory.py`.

**Done when**: a recurring fault that previously required full investigation is resolved more cheaply (fewer tool calls, fewer tokens) on the second occurrence.

---

## <a id="tier-4"></a>TIER 4 — Outstanding Memory and Binary Hygiene

The v12 work mostly landed. **One critical item is still open**: F-4 binary self-containment.

### T4-1 (Bv1) — Static-link lbug for self-contained binary (F-4 carryover)

**What**: today the binary requires `liblbug.so.0` and `LD_LIBRARY_PATH=/usr/local/lib`. AI agents that build locally and try to run the binary fail every time. The Dockerfile bundles the .so but local development and direct binary distribution are broken.

**Specific actions**:
1. Investigate whether lbug 0.15.3 supports a static-build feature flag; if yes, use it
2. If not: add a `LBUG_STATIC=1` build path that links the static lib
3. Drop `LBUG_SHARED=1` from `.cargo/config.toml` (or make it explicit opt-in for development with a comment)
4. CI assertion: `ldd target/release/bonsai | grep -c liblbug` returns 0 on Linux
5. Update Dockerfile to use the static build (drops the `find liblbug.so.0` step)
6. Verify `release.yml` produces self-contained artefacts

**Where**: `Cargo.toml`, `.cargo/config.toml`, `build.rs`, `docker/Dockerfile.bonsai`, `.github/workflows/release.yml`.

**Done when**: `cargo build --release && ldd target/release/bonsai` shows no lbug dependency; `./target/release/bonsai --version` runs without `LD_LIBRARY_PATH` set.

### T4-2 (Bv1) — Verify release artefact pipeline

**What**: `release.yml` exists. Validate that on a release tag it produces:
- `bonsai-linux-amd64` (static-linked, runs standalone)
- `bonsai-linux-arm64` (same)
- Optionally `bonsai-darwin-amd64` and `bonsai-darwin-arm64` for Mac developers

Each artefact uploaded as a release asset. Artefacts signed if signing infrastructure is available.

**Where**: `.github/workflows/release.yml`.

**Done when**: a tagged release produces downloadable binaries that `chmod +x && ./bonsai --version` works on the target OS.

### T4-3 (Bv1) — Verify `bonsai self-test` subcommand

**What**: spec'd in v12 T2-3. Verify implementation. If missing, build it. Subcommand exercises:

- LadybugDB linkage (static or dynamic)
- crypto provider availability (rustls)
- tokio runtime
- gRPC client
- config parser

Returns 0 on success, non-zero on any failure with diagnostic output. AI agents call this before further automation.

**Where**: `src/bin/` (probably new) + `src/main.rs` subcommand handler.

**Done when**: `bonsai self-test` returns a clear pass/fail summary in JSON; CI runs it.

---

## <a id="tier-5"></a>TIER 5 — Test Coverage Gaps

The v10 testing tier landed substantially, the v11 feedback loop landed substantially. What remains:

### T5-1 (Bv1) — ServiceNow PDI live tests (when operator provides PDI)

v10 T2-4 + T2-5 + the new `scripts/e2e_servicenow_pdi_test.sh` from v12. Activated when operator supplies `SNOW_INSTANCE_URL`, `SNOW_USERNAME`, `SNOW_PASSWORD`.

### T5-2 (Bv1) — HIL e2e test

v10 T2-6 carryover. Chaos harness drives a fault → detection → proposal → approve → execute → outcome flow end-to-end, with ten cycles to drive trust graduation.

### T5-3 (Bv1) — Graph query test framework

Tier 1 T1-7 — already listed above; mentioned here for cross-reference.

### T5-4 (Bv1) — Mutation testing on critical modules

v10 T4-5 carryover. `cargo-mutants` weekly job on `credentials.rs`, `audit.rs`, `remediation/trust.rs`, `assignment.rs`, plus the new `graph/queries.rs`. Mutation score ≥80% on each.

### T5-5 (Bv1) — Verify nightly integration CI runs

v10 T4-3 carryover. The workflow exists; verify it's actually scheduled and produces artefacts in `docs/test_results/` on each nightly run.

### T5-6 (Bv1) — Detection-firing chaos matrix output validation

v12 T4-3 carryover. The harness exists. Verify it produces a clear per-fault matrix in `docs/test_results/chaos_matrix/<date>.md` and that **every fault in `lab/fault_catalog.yaml` is exercised at least once on the nightly run**.

### T5-7 (Bv1) — Graph algorithm tests

For each algorithm in T1-4, a test against the test fixture graph that asserts expected output. Centrality on a known star topology produces the known centrality vector.

---

## <a id="tier-6"></a>TIER 6 — UI/UX Completion

### T6-1 (Bv1) — Operator path overrides UI workspace

v9 T4-2 carryover. Data model exists in `src/registry.rs::PathOverride`. Build the workspace where operators define site-scoped, role-within-environment, and device-specific overrides; preview which devices are affected before saving.

**Where**: extension to `ui/src/routes/Profiles.svelte` or new `Overrides.svelte`.

### T6-2 (Bv1) — Subscription resolution audit

v9 T4-3 carryover. Device drawer gains an "Effective subscription" panel showing the resolution chain: catalogue profile X → role-override Y → site-override Z → final path list. Operator can see *why* a device has the paths it has.

### T6-3 (Bv1) — Graph explorer UI

Tier 1 T1-5 — listed there.

### T6-4 (Bv1) — Investigations UI

Tier 3 T3-2 — listed there.

### T6-5 (Bv1) — Map visualisation (deferred to lower priority)

v9 T11 carryover. Sites with lat/lon render as markers on a map. Implement only if operator demand surfaces.

---

## <a id="tier-7"></a>TIER 7 — Strategic Carryover

Items genuinely valid but lower-priority than the Tier 1-3 graph + GNN + agent work.

### T7-1 — Catalogue plugin install command (v9 T5-2)

`bonsai catalogue install <url>` fetches, verifies, registers a community-contributed plugin.

### T7-2 — AIOps readiness checklist (v9 T6-7)

Self-check producing green/amber/red status: detection events stable, trust model populated, enrichment producing context labels, audit retention satisfies operator requirements, output adapter health green for last 7 days.

### T7-3 — Signals (syslog + traps) (v9 T7)

Separate collector process listening on UDP 514 / TCP 6514 / UDP 162. Signals as detection hints, never state. Signal-aware detectors and signal-triggered investigations (the latter ties to Tier 3 agent).

### T7-4 — Controller adapters (demand-driven, v9 T10)

Trait design only as a low-cost artefact. Implementations only when an operator specifically needs Meraki / DNAC / ACI / vManage integration. Multi-controller correlation remains the niche where bonsai is architecturally unique.

### T7-5 — NL query layer (v9 T11)

Natural-language interface that compiles operator questions to Cypher. Pre-requisites: rich Tier 1 query library. Sanitised through the same allow-list as the explorer.

### T7-6 — Bulk onboarding CSV (v9 T11)

CSV → onboarding API. Useful for operators with established inventories.

### T7-7 — Scale architecture documentation (v9 T11)

Operator-facing doc explaining how bonsai scales — collector horizontal, core vertical in v1, graph sharding deferred until forced.

### T7-8 — S3-compatible archive backend (v9 T11)

`S3Archive` impl behind the existing `LocalFileArchive` trait surface. Operators with > 10 collectors or constrained local disk benefit.

### T7-9 — Campus topology (v11 T2-3)

Most campus telemetry is a subset of DC EVPN; marginal value lower than further DC/SP work. Build when an operator with campus needs surfaces.

### T7-10 — TSDB integration adapter (existing OutputAdapter — Prometheus already covers most cases)

Legacy line item; mostly subsumed by the Prometheus output adapter that already exists.

### T7-11 — ML feature schema versioning (v9 T11)

Becomes relevant when Tier 2 GNN training operationalises. Add at that point.

### T7-12 — Bitemporal schema (deferred)

Forced only by a real NL-query-about-history requirement. Not urgent.

### T7-13 — Schema migration tooling (deferred)

Forced by a real breaking schema change. Not urgent.

### T7-14 — Grafeo migration evaluation (deferred)

Forced by LadybugDB going quiet for 60+ days. Not urgent.

---

## <a id="tier-8"></a>TIER 8 — Documentation Refresh (lowest priority)

Last in the queue, per operator instruction. Important but not blocking.

### T8-1 — README.md refresh

The current README is from a much earlier project state. Update for the post-v12 reality:

- One-paragraph identity (controller-less network state engine, gNMI hot-path, graph-native, AIOps feeder)
- Quick start in three commands (clone, set passphrase, compose up)
- Architecture diagram (core, collectors, lab, external infra)
- Link to `docs/ai_feedback_protocol.md`, `docs/external_infra.md`, `docs/resource_contract.md`
- Link to current backlog Bv1
- Build instructions including the static-link path from Tier 4 T4-1

### T8-2 — CLAUDE.md / AGENTS.md refresh

Update for AI-agent consumption — the audience framing, current state of the code, what tiers are active. Specifically tie to `docs/ai_feedback_protocol.md` and the unified status endpoint.

### T8-3 — DECISIONS.md consolidation

The decision log has accumulated across 12+ backlogs. A pass to verify ADRs are still accurate, mark deprecated decisions, and link related decisions together.

### T8-4 — Sprint result narratives

`memory/project_sprint_progress.md` exists. Refresh it with the post-v12 state and the new Bv1 sprint plan.

### T8-5 — Path profile docs validation

The 12 path profile docs in `docs/path_profiles/` were generated. Verify each is accurate against current vendor coverage and lab verification.

### T8-6 — Output adapter docs

Each adapter (Prometheus, Splunk, Elastic, ServiceNow EM) gets a one-page operator-facing doc with config example, expected metrics/events, troubleshooting tips.

### T8-7 — UI component doc

Lightweight documentation for each UI workspace — what data it shows, what API endpoints it consumes, what user actions it supports. Useful for AI agents understanding the UI surface.

---

## <a id="execution-order"></a>Execution Order

### Sprint 1 — Graph foundation (2-3 weeks) ⚡
1. T1-1 multi-hop pattern queries (≥12 production queries)
2. T1-2 replace Rust BFS with graph-DB shortest path
3. T1-7 graph query test framework
4. T1-3 blast radius traversal endpoint
5. Tier 4 T4-1 static-link lbug (parallel — different code path)

### Sprint 2 — Graph algorithms + explorer (2 weeks)
6. T1-4 graph algorithms (centrality, dependency depth, correlation, orphans, subscription health)
7. T1-5 Graph explorer UI workspace
8. T1-6 Saved queries

### Sprint 3 — Path A embeddings (2 weeks)
9. T2-1 graph embeddings (node2vec/GraphSAGE)
10. T5-3 graph query tests for the new embedding-driven queries
11. T7-11 ML feature schema versioning

### Sprint 4 — Investigation agent foundation (2-3 weeks)
12. T3-1 Agent scaffolding with graph-aware tools
13. T3-3 Agent cost controls
14. T3-2 Agent UI workspace

### Sprint 5 — Path B GNN (3-4 weeks)
15. T2-2 GNN with message passing
16. T2-3 Enrichment-aware data loader
17. T2-4 Online inference path
18. T3-4 Agent memory across investigations

### Sprint 6 — Test coverage and UI completion (1-2 weeks)
19. T5-2 HIL e2e test
20. T5-4 cargo-mutants on critical modules
21. T5-6 chaos matrix validation
22. T6-1 Operator path overrides UI
23. T6-2 Subscription resolution audit

### Sprint 7 — PDI live tests (when credentials available, 1 week)
24. T5-1 ServiceNow PDI live tests

### Sprint 8 — Strategic carryover items as time permits
25. T7-3 Signals
26. T7-2 AIOps readiness checklist
27. T7-1 Catalogue plugin install command

### Sprint 9 — Documentation refresh (lowest priority, 1 week)
28. T8-1 through T8-7

### Continuously
- T5-5 Verify nightly CI is producing artefacts (ongoing)
- T4-2 Release artefact pipeline (verify on each release)

---

## <a id="guardrails"></a>Guardrails

### New in Bv1

- **Graph queries must use the graph database.** Loading edges into a Rust Vec and walking BFS is rejected for new code; existing callers (path_handler) migrate by Sprint 1.
- **Every query in `src/graph/queries.rs` ships with a test.** No exceptions.
- **The graph explorer is read-only and sanitised.** Free-form Cypher is allowed only through an allow-list that rejects DELETE / UPDATE / CREATE / DROP statements.
- **GNN training uses the same archive that the chaos harness produces.** No synthetic data shortcuts; honest evaluation requires real chaos-run data.
- **Documentation refresh is the last priority, not the first.** Good code with stale docs is better than great docs over broken code. Tier 8 happens after Tiers 1-3.

### Unchanged from v7-v12

All prior architectural invariants and discipline continue. Reference earlier backlogs for the full list:
- gNMI-only hot path; syslog/traps as signals only
- tokio-only async Rust
- Vault-only credentials with purpose-tagged audit
- No Kubernetes in v0.x
- Every ADR at commit time
- No LLM in detect-heal hot path
- Enrichers no LLM on device configuration
- Collectors horizontal, core vertical
- Build time first-class metric
- Code landing ≠ work complete (no callsite = not mergeable)
- Distributed mode must run distributed (mTLS, no plaintext)
- Environment awareness first-class
- Path catalogue is data, not code
- HIL is graduated path, not binary
- OutputAdapter read-only on bus
- AIOps positioning as feeder, not replacement
- Memory bounded by configuration, not detected RAM
- UI shows current state via SSE, not last-fetched

### Anti-patterns to reject

- "Let's keep doing relational queries on the graph; they work" — no, this is the deferred work
- "Rust BFS is faster than the graph DB" — measure, then decide; for the lab scale today, in-DB shortest path is faster *and* simpler
- "Path A and Path B can run in parallel" — no, A first; B's data loader and feature schema build on A's embeddings infrastructure
- "Investigation agent can run without graph traversal tools" — no, the tools are how the agent earns its keep; without them it's a chatbot
- "We'll skip graph algorithm tests; the queries are 'obviously right'" — no, graph results are notoriously surprising and snapshot tests catch drift
- "Documentation can be done at the same time as the code" — no, docs land last; doing them in parallel produces incorrect docs because the code changes
- All prior anti-patterns remain in force

---

## What Bv1 Explicitly Excludes

- Auth/RBAC, multi-tenancy, production HA
- Kubernetes deployment manifests
- A fifth vendor before existing four are vendor-neutral
- LLM-based parsing of device configuration anywhere outside the investigation agent
- Workspace split (current build is fine)
- Bitemporal schema, schema migration, Grafeo evaluation (Tier 7 deferred)
- Controller adapter implementations (Tier 7, demand-driven)
- Real-time streaming GNN (offline batch is the v0 path)
- Auto-graduation of trust state
- Output adapters that write back to the bus
- Auto-import of unverified YANG paths into the default catalogue
- Bonsai-replaces-NDI/DNAC/Meraki marketing positioning

---

*Bv1.0 — authored 2026-05-04 after end-to-end review of post-v12 codebase plus full re-read of v2-v12 backlogs. Resets the sequence around graph-native value extraction (Tier 1) which has been deferred across multiple iterations. Path A → Path B GNN is Tier 2 (unblocked by Tier 1). Investigation agent is Tier 3 (unblocked by Tier 1 + Tier 2 enrichment). All open items from v2-v12 absorbed into Tiers 1-7 with full traceability. Documentation refresh is Tier 8 (lowest priority). Substantial v12 work verified landed (memory fixes F-1/F-2/F-3/F-9, SSE workspace updates F-7, Operations live polling F-12, restart policies F-5, seed --reset flags F-6, screenshot/startup-time/release CI workflows). One critical v12 item still open: F-4 binary self-containment via static lbug — moves to Tier 4 T4-1.*
