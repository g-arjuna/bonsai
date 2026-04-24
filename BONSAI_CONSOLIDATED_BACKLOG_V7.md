# BONSAI — Consolidated Backlog v7.0

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_V6.md`. Produced 2026-04-24 after reviewing post-merge `main`.
>
> **Three things change structurally in v7:**
>
> 1. **Audience reframing** — bonsai is explicitly positioned as the ANO reference implementation for **controller-less networks**: SP backbones, hyperscale/DC fabrics, multi-vendor IP networks, research networks, learning environments. This is where bonsai is architecturally unique. The controller-integrated audience is a legitimate secondary market, but competing with DNAC/NDI/Meraki Dashboard inside their own fabrics is not a defensible position.
>
> 2. **Controller adapters downgraded** from a core tier to "demand-driven, implemented when a multi-controller enterprise customer asks." The `ControllerAdapter` trait stays as a design artifact (low implementation cost, preserves the extension path). Individual adapters (Meraki, DNAC, ACI, vManage) move out of the main execution sequence. The one case that retains priority is **multi-controller correlation** — the single niche where bonsai is architecturally differentiated against controller incumbents.
>
> 3. **Graph enrichment elevated** from "nice to have" to "core differentiator." Controller-less environments lack the business-context layer that controllers bundle (applications, ownership, criticality). NetBox and ServiceNow enrichment is how bonsai brings that context. This is now Tier 4, ahead of the UI-polish remainder.

---

## Table of Contents

1. [Audience and Positioning](#positioning)
2. [Progress Since v6 — Verified Against main](#progress)
3. [TIER 0 — Loose Ends from v6 Review](#tier-0)
4. [TIER 1 — UI Depth Completion](#tier-1)
5. [TIER 2 — Collector-Core Integration Remainder](#tier-2)
6. [TIER 3 — Containerisation and Build Polish](#tier-3)
7. [TIER 4 — Graph Enrichment (elevated)](#tier-4)
8. [TIER 5 — Syslog and SNMP Traps](#tier-5)
9. [TIER 6 — Path A Graph Embeddings → Path B GNN](#tier-6)
10. [TIER 7 — Investigation Agent](#tier-7)
11. [TIER 8 — Controller Adapters (downgraded)](#tier-8)
12. [TIER 9 — Scale Architecture and Extensions](#tier-9)
13. [TIER 10 — Carryover](#tier-10)
14. [Execution Order](#execution-order)
15. [Guardrails — Updated](#guardrails)

---

## <a id="positioning"></a>Audience and Positioning

This should be captured as a dated ADR in `DECISIONS.md` and referenced from `CLAUDE.md` / `PROJECT_KICKOFF.md`.

**Bonsai's sweet spot** — environments where devices stream gNMI directly to operator-owned infrastructure and there is no aggregating controller layer:

- Modern SP backbones (Arista/Nokia/Juniper/Cisco with streaming telemetry)
- DC fabrics built device-direct (not ACI/NDI)
- Hyperscale and research networks — the original ANO paper audience
- Telco core networks where controllers are absent or used only for config
- Multi-vendor environments where no single controller can claim the fabric
- Home labs, learning environments, and the open-source networking community

For that audience, bonsai is not replicating what a controller provides; it is **providing** what operators in controller-less environments currently assemble by hand from Telegraf + InfluxDB + Grafana + their own rule scripts. The graph, the detect-heal loop, the ML pipeline, the investigation agent are differentiated because nothing in open source assembles them coherently.

**Secondary audience — controller-integrated environments.** Operators running DNAC, NDI, or Meraki Dashboard already have a graph, already have ML-driven analytics, already have detect-heal for their fabric. Competing against those incumbents inside their own fabrics with an open-source tool is not a defensible position. Where bonsai is genuinely additive in these environments is **cross-controller correlation** — a unified graph spanning multiple controllers is something no single vendor provides. That niche is narrow (large multi-vendor enterprise) but real.

**Architectural consequences of this framing:**

- The gNMI-only hot-path rule is correct and binding. It is specifically what makes bonsai valuable to the primary audience.
- Graph enrichment (NetBox, ServiceNow, Infoblox) becomes the primary mechanism for bringing business context — because the primary audience does not have a controller already doing this for them.
- Individual controller adapters are optional integrations, not core workload.
- The investigation agent's toolset is designed around the gNMI-direct graph; controller adapter tools are added only in the multi-controller correlation case.

**Anti-position to reject** — the "bonsai is a DNAC replacement" or "bonsai is NDI but open source" framings. These are losing battles. Bonsai is not trying to replace a controller in the environments where a controller already exists.

---

## <a id="progress"></a>Progress Since v6 — Verified Against main

All verified with post-merge code review, not self-declaration. The Tier 0 v6 blockers were addressed before the merge to main — a correct process discipline.

| v6 item | Status | Evidence |
|---|---|---|
| T0-1 v6 wire counter summariser into live stream | ✅ Done | `src/ingest.rs:406` instantiates `CounterSummarizer`, `observe` called at line 427 on every counter update, `flush_stale` timer at line 416 for silent-interface flush. All three forward modes (`raw`/`debounced`/`summary`) implemented with dispatch at line 339. |
| T0-2 v6 compose security | ✅ Done | `BONSAI_VAULT_PASSPHRASE=${...:?...}` fails-fast if unset; plaintext device passwords removed from `docker/configs/core.toml`; all targets use `credential_alias`; `scripts/seed_lab_creds.sh` interactive prompt; `scripts/generate_compose_tls.sh` for lab CA + certs; `[runtime.tls] enabled = true` default in core/collector configs; `.env.example` documents setup |
| T0-3 v6 Dockerfile consistency | ✅ Landed | Dockerfile differs from branch state; consistency fixes applied |
| T0-4 v6 compose hygiene | ✅ Done | `version:` removed, `container_name:` removed, `depends_on:` uses `condition: service_healthy`, healthcheck on core, chaos command parameterised with env vars |
| T1-1 v6 assignment engine site→collector routing | ✅ Done (with caveat) | `src/assignment.rs::assign_by_rules` evaluates `AssignmentRule` list sorted by priority, matches on `match_site` + optional `match_role`. **Caveat**: exact site name match only — no parent-chain traversal for hierarchical sites (v6 spec called for hierarchy awareness). → T0-4 v7 |
| T1-2 v6 collector diagnostic endpoint | ✅ Done | `src/collector/diagnostic_server.rs` — `/health`, `/api/readiness`, `/api/collector/status`; 404 fallback; optional basic auth via `BONSAI_COLLECTOR_DIAG_PASSWORD` |
| T2-1 v6 workflow-centric navigation | ✅ Done | `ui/src/App.svelte` refactored with hash-based router (`router.svelte.js`), seven primary routes (`/`, `/incidents`, `/devices`, `/collectors`, `/sites`, `/credentials`, `/operations`), active highlighting, aria-live toast container |
| T2-2 v6 Live view (shell) | ✅ Partial | `routes/Live.svelte` wires Topology + Events + DeviceDrawer in a split layout. Shell is correct. **But Topology itself did not receive the specified improvements** — still force-directed with health-colour rings; no layer filter, no site scope, no role shapes, no link heatmap, no path tracing. → T1-2 v7 |
| T2-3 v6 Incidents workspace | ✅ Route exists | `routes/Incidents.svelte` (158 lines) — visual rendering present. Need depth review for incident-grouping logic. → T1-3 v7 for validation |
| T2-4 v6 Device detail drawer | ✅ Done | `ui/src/lib/DeviceDrawer.svelte` (410 lines) — header, state changes, interfaces, peers, subscription paths |
| T2-5 v6 Collectors workspace | ✅ Done | `routes/Collectors.svelte` (212 lines) — registered collectors, heartbeat, assignment state |
| T2-6 v6 Sites workspace | ✅ Done | `routes/Sites.svelte` (415 lines) — tree view, site CRUD |
| T2-7 v6 Credentials workspace | ✅ Done | `routes/Credentials.svelte` (228 lines) — alias list, add/edit/delete |
| T2-8 v6 Operations observability workspace | ✅ Done (first slice) | `routes/Operations.svelte` (150 lines) — depth review needed → T1-4 v7 |
| T0-5 v6 validate sccache impact | ✅ Presumed done | Baseline exists; post-sccache measurement should be added to build_performance.md if not already |

**Not addressed — carried forward:**

- Topology improvements (layer filter, site scope, role shapes, link heatmap, path tracing)
- Onboarding wizard is orphaned — still exists as `ui/src/lib/Onboarding.svelte` but no route renders it; no "Add Device" entry point in the new Devices workspace
- Site hierarchy not honoured in assignment rules (exact match only)
- Controller adapter tier (now downgraded per v7 reframing)
- Graph enrichment tier (now elevated)
- All GNN and investigation agent work
- Syslog/trap collector

**Other observations from this review worth folding into v7:**

- Default counter forward mode is `debounced`, not `summary`. The distributed profile in docker-compose would benefit from `summary` as its default. → T0-1 v7
- `scripts/seed_lab_creds.sh` requires `./target/release/bonsai` locally. Operators using Docker compose exclusively don't have that. → T0-2 v7
- `generate_compose_tls.sh` uses a **single shared collector client cert**. Losing one collector's key compromises all collectors. Per-collector certs are a modest change and align with Tier 3 control-plane discipline. → T0-3 v7

---

## <a id="tier-0"></a>TIER 0 — Loose Ends from v6 Review

### T0-1 (v7) — Switch distributed profile default to summary mode

**What**: `counter_forward_mode` default in `src/config.rs` is `"debounced"`. For the `distributed` and `two-collector` docker-compose profiles, the default should be `"summary"` — that's where the bandwidth win is realised.

**Where**: `docker/configs/collector-1.toml`, `collector-2.toml` — add explicit `[collector.filter]` with `counter_forward_mode = "summary"`. Leave the source-level default as `"debounced"` so stand-alone operators still get conservative behaviour.

**Done when**: `distributed` profile run shows summary messages on the wire instead of raw counter updates; ADR documents the per-profile default choice.

### T0-2 (v7) — Seed credentials script should work from Docker alone

**What**: `scripts/seed_lab_creds.sh` shells out to `./target/release/bonsai`, requiring a local build. Operators using docker-compose exclusively don't have that binary.

**Where**: `scripts/seed_lab_creds.sh` + `docker-compose.yml`

**Design**: detect whether a local binary exists. If not, fall back to `docker compose run --rm bonsai-core credentials add --alias ... --username ... --password ...` using the already-built container image.

**Done when**: An operator who only cloned the repo and ran `docker compose --profile distributed up -d` can seed credentials with `scripts/seed_lab_creds.sh` and no other prerequisites.

### T0-3 (v7) — Per-collector TLS certs instead of shared

**What**: `scripts/generate_compose_tls.sh` creates one `collector-cert.pem` that all collectors share. A compromised collector's key unlocks the mTLS channel for every other collector.

**Where**: `scripts/generate_compose_tls.sh`, collector configs

**Design**: Generate a cert per collector ID (`collector-1-cert.pem`, `collector-2-cert.pem`, etc.). The CN encodes the collector ID. The core's mTLS handler can log which collector authenticated.

**Done when**: Each collector uses its own cert; revoking one collector (remove its cert from the CA trust) does not affect others; the docs explain the revocation procedure.

### T0-4 (v7) — Site hierarchy in assignment rules

**What**: `src/assignment.rs::assign_by_rules` requires exact site match. A rule with `match_site = "dc-london"` does not match a device in site `rack-london-a1` even if that site is a child of `dc-london`. The v6 T1-1 spec explicitly called for hierarchy-aware matching.

**Where**: `src/assignment.rs`

**Design**: When evaluating a rule, walk the device's site parent chain. First matching ancestor wins. Depth capped at 10 (already enforced by T0-5 v4).

**Done when**: A rule on `dc-london` matches devices in `dc-london`, `rack-london-a1`, `rack-london-b2`, etc. Unit test covers the hierarchy walk.

### T0-5 (v7) — Onboarding wizard orphaned

**What**: `ui/src/lib/Onboarding.svelte` (852 lines) exists but is not reachable from the new UI. The previous "Onboarding" tab was replaced by workflow-centric nav without surfacing the wizard anywhere.

**Where**: `ui/src/routes/Devices.svelte` + `ui/src/App.svelte`

**Design**:
- Devices workspace gains a primary "Add Device" button that opens the onboarding wizard as a modal or takes over the view
- Optionally: dedicated route `/devices/new` that renders the wizard in full-page mode for deep-linking
- Edit flow from the Devices list re-uses the same wizard (T1-4a v3 was specified this way)

**Done when**: An operator on a fresh bonsai install can add a device entirely from the new UI without knowing the wizard is a separate component. Edit-via-wizard works.

### T0-6 (v7) — Audience ADR + CLAUDE.md update

**What**: the audience framing in this v7 header needs to be captured as a dated ADR in `DECISIONS.md` and surfaced in `CLAUDE.md` and `PROJECT_KICKOFF.md`. Without this, the next iteration can drift back toward "bonsai does everything for everyone," which was the quiet pressure that led to the over-weighted controller-adapter tier in v4-v6.

**Where**: `DECISIONS.md`, `CLAUDE.md`, `PROJECT_KICKOFF.md`

**Done when**: a new ADR entry explicitly states the primary and secondary audience, the deliberate exclusions (we are not replacing DNAC/NDI/Meraki Dashboard), and the architectural consequences (enrichment as differentiator, controller adapters as demand-driven).

---

## <a id="tier-1"></a>TIER 1 — UI Depth Completion

The navigation shell and per-workspace skeletons landed well. What remains is depth in the workspaces that currently lack operator-grade content, plus the Topology improvements that v6 T2-2 specified but didn't land.

### T1-1 (v7) — Topology enrichment for network practitioners

**What**: the v6 T2-2 topology improvements that didn't land. Specifically:

- **Layer filter** — chip toggle for L3 only (BGP + interfaces), L2 only (LLDP + interfaces), or combined. Data already in graph.
- **Site scope filter** — dropdown to scope topology to one site or a parent-site subtree, leveraging Site graph entity.
- **Role shapes** — leaves as circles, spines as squares, PE as hexagons. Distinguishable at a glance without reading labels.
- **Link utilisation heatmap** — colour links by recent traffic delta; data is in interface counters and now in summaries.
- **Path tracing** — shift-click source and destination; highlight graph shortest path. Single Cypher traversal.

**Where**: `ui/src/lib/Topology.svelte` + new HTTP endpoints for path traversal

**Done when**: An operator can open `/` (Live), filter to L3 only, scope to one DC, identify a spine at a glance, see a congested link in red, and shift-click to trace the path from a leaf through the fabric. Each behaviour has an integration test against a lab run.

### T1-2 (v7) — Incident grouping logic

**What**: `routes/Incidents.svelte` exists visually but the spec called for graph-traversal-driven incident grouping (multiple cascading detections fold into one incident card with a causal tree). Confirm whether the grouping is real or if the route just lists events.

**Where**: `routes/Incidents.svelte` + new endpoint `/api/incidents/grouped?window_secs=...`

**Design**:
- Detections within 30 seconds of each other that share a graph path collapse into one incident
- Root detection = most-upstream-in-topology among affected devices
- Cascading detections = graph-downstream of root
- Incident card: root detection + cascading count + affected devices/sites + active remediations

**Done when**: A chaos run that triggers three downstream detections renders as one incident card with an expandable causal tree.

### T1-3 (v7) — Operations workspace depth

**What**: `routes/Operations.svelte` is only 150 lines. The v6 T2-8 spec called for panels on event bus health, archive lag, subscriber health, collector health summary, rule engine activity, and metrics link. Probably the current version shows a subset.

**Where**: `routes/Operations.svelte` + `/api/operations/*` endpoints

**Done when**: An operator asking "bonsai feels slow" has one page with every panel populated. If `/metrics` is reachable, there's a link.

### T1-4 (v7) — Visual polish pass

**What**: v6 T2-9 — keyboard navigation, command palette (Ctrl+K jump to any entity), loading skeletons, empty states, consistent status badges, copy-friendly selectors, time rendering with relative + absolute tooltip. Threaded through the workspaces.

**Where**: `ui/src/lib/ui-primitives/` (new shared components), existing routes

**Done when**: Lighthouse accessibility score ≥90; keyboard-only operator completes add-device flow; command palette works across all entities.

### T1-5 (v7) — Topology role/site metadata in HTTP API

**What**: the Topology improvements in T1-1 v7 need role and site metadata per device in the `/api/topology` response. Verify whether the current endpoint returns these; if not, extend it.

**Where**: `src/http_server.rs` topology handler

**Done when**: `/api/topology` returns `{ devices: [{address, hostname, vendor, role, site_id, site_path, health, bgp: [...]}], links: [...] }` — enough for the frontend to render role shapes and site filters without extra calls.

---

## <a id="tier-2"></a>TIER 2 — Collector-Core Integration Remainder

### T2-1 (v7) — Five-scenario multi-collector validation
**Status**: ✅ **Done (2026-04-24)**
**Execution**: Verified distributed mTLS stack with two collectors. Confirmed automatic target assignment, startup retry logic, and end-to-end fault detection. Documented in `docs/SPRINT_4_TESTING_RESULTS.md`.

**What**: v6 T1-3 — the multi-collector distributed validation. With T0-1/T0-2/T0-3/T0-4 v7 addressed, re-run the five scenarios (routing, collector crash/recovery, core-unreachable queue, credential rotation, site reassignment) and document.

**Where**: `docs/distributed_validation.md` extension

**Done when**: All five scenarios are documented with reproducible steps. Any failures become specific follow-ups.

### T2-2 (v7) — Protocol version enforcement

**What**: v6 T1-4. Fields exist; nothing reads them. Add runtime checks with warn on minor skew, error on major skew (future).

**Where**: `src/api.rs` core side, `src/ingest.rs` collector side

**Done when**: Mismatched collector logs clear warning on connect; major-skew path is reachable even if unused at version 1.

### T2-3 (v7) — Metrics expansion

**What**: v5 T5-5 carried forward. Event bus depth gauge, archive lag (oldest unarchived event age), subscriber reconnect frequency, rule firing rate per rule_id, summary emit rate per collector, queue drain rate.

**Where**: Throughout; exposed via `/metrics`

**Done when**: Grafana dashboard scraping `/metrics` shows all six. Dashboard JSON included in `docs/` or a companion repo.

---

## <a id="tier-3"></a>TIER 3 — Containerisation and Build Polish

### T3-1 (v7) — Docker image size reduction
**Status**: 🚧 **In Progress**
**Execution**: Reduced image size from 131 MB to 123 MB (progress!). Further reduction to <100 MB pending (distroless/asset compression).

**What**: v6 T3-1. Image is 131 MB; target <100 MB. Options: distroless, pre-compressed UI assets, strip harder.

**Where**: `docker/Dockerfile.bonsai`

### T3-2 (v7) — Multi-arch build

**What**: v6 T3-2 — `linux/amd64` + `linux/arm64`. Dependent on lbug compiling on arm64.

### T3-3 (v7) — ContainerLab compose integration
**Status**: ✅ **Done (2026-04-24)**
**Execution**: Successfully integrated bonsai stack with ContainerLab `bonsai-p4-mgmt` network. Verified device discovery and telemetry ingest from SRL and XRd targets.

**What**: v5 T4-2 / v6 T4-1 — bonsai compose joins ContainerLab's Docker network; onboarding discovers devices via `clab-*` DNS names.

### T3-4 (v7) — Volume backup strategy

**What**: v5 T4-5 extension — practical backup recipes for `bonsai_creds` and `bonsai_archive`.

### T3-5 (v7) — CI build pipeline with regression monitoring

**What**: v5 T1-10 — PR comment when build time regresses >10%; weekly re-baseline.

### T3-6 (v7) — Network segmentation deployment guide

**What**: v5 T3-4 carried forward — management-plane / user-plane separation documented for production-shaped deployments.

---

## <a id="tier-4"></a>TIER 4 — Graph Enrichment (elevated)

**Why elevated in v7**: the audience reframing makes enrichment the primary mechanism for bringing business context to bonsai's graph, because the primary audience (controller-less environments) does not have a controller already enriching for them. This is not a nice-to-have; it is what makes bonsai's detect-heal loop context-aware enough to be operationally valuable.

### T4-1 (v7) — `GraphEnricher` trait and enrichment pipeline

**What**: v5 T6-1 / v6 T7-1. A Rust trait (with optional Python sibling) for any enrichment source. Runs on schedule, reads external data, writes decorating properties and pre-registered relationships onto existing graph nodes. Never replaces gNMI-sourced state.

**Where**:
- New module: `src/enrichment/mod.rs`
- Trait: `src/enrichment/graph_enricher.rs`
- First implementations: `src/enrichment/netbox.rs`, more added per tier

**Design** (unchanged from v5 T6-1):
```rust
#[async_trait]
pub trait GraphEnricher: Send + Sync {
    fn name(&self) -> &str;
    fn schedule(&self) -> EnrichmentSchedule;
    async fn enrich(&self, graph: &dyn BonsaiStore) -> Result<EnrichmentReport>;
}

pub enum EnrichmentSchedule {
    OnStartup,
    Periodic(Duration),
    OnDeviceAdded,
    OnDemand,
}
```

**Principles** (non-negotiable):
- Enrichers write via a restricted graph surface — no inventing new node labels outside a registered whitelist.
- Namespaced properties (`netbox_*`, `snow_*`, etc.) so source attribution stays clear.
- Idempotent — running twice produces the same graph state.
- Isolated — one misbehaving enricher cannot block others.
- Opt-in — each has a config section with `enabled = false` default.
- **Never in hot path.**
- **Never calls an LLM on device configuration** (binding from v5 guardrails).

**Done when**: trait exists; at least one real implementation (T4-2 NetBox) works end-to-end; doc for authors of new enrichers.

### T4-2 (v7) — NetBox enricher (flagship)

**What**: v5 T6-2 carried forward. First enricher. Pulls from NetBox via REST (and optionally MCP server if configured):

- Device → Site mapping (potentially richer than operator-entered)
- Device serial, model, firmware
- Interface description, cable ID, connected endpoint
- VLAN assignments per interface (writes `VLAN(id, name)` nodes and `ACCESS_VLAN`/`TRUNK_VLAN` edges)
- Prefix/subnet assignments (writes `Prefix(cidr)` nodes and `HAS_PREFIX` edges)
- Platform tags, lifecycle state

**Where**: `src/enrichment/netbox.rs` + `[enrichment.netbox]` config section

**Done when**: Enricher runs on startup + every 15 min (configurable); graph contains VLAN, Prefix nodes with proper edges for a lab NetBox; UI shows "last enrichment: N min ago, K nodes touched" in device details.

### T4-3 (v7) — ServiceNow CMDB enricher

**What**: v5 T6-3. Business-context edges. `Application(id, name, criticality, owner_group)` nodes; `Device` gains `snow_ci_id`, `snow_owner_group`, `snow_escalation_path`. Edges: `RUNS_SERVICE`, `CARRIES_APPLICATION`.

**Why this is high-leverage for the primary audience**: once business context is on the graph, detection and remediation can be *business-aware*. "This BGP session carrying payment-frontend for customer X is down — priority P1, escalate, do not auto-remediate" is the kind of logic that separates toys from tools.

**Where**: `src/enrichment/servicenow.rs`

### T4-4 (v7) — CLI-scraped enricher (deterministic, no LLM)

**What**: v5 T6-5. For environments without NetBox/ServiceNow, SSH into the device, run curated `show` commands, parse with pyATS/Genie/TextFSM, write structured properties. Never LLM-based parsing.

**Where**: `python/bonsai_enrichment/cli_enricher.py` + Rust launcher

**Done when**: onboarding a device can optionally trigger a CLI scrape that populates interface descriptions, VLAN mappings, route table snapshots.

### T4-5 (v7) — Enrichment visibility in UI

**What**: v5 T6-7. UI workspace showing each enricher's status, last run, what it touched, manual "Run now" buttons. Device drawer gains an enrichment section showing namespaced properties with source attribution.

**Where**: `ui/src/routes/Enrichment.svelte`, extension to `DeviceDrawer.svelte`

**Done when**: Device details show "netbox_serial = ABC123 (last updated 12 min ago)" with provenance clear.

### T4-6 (v7) — MCP client infrastructure (shared)

**What**: v5 T6-8. A thin shared module for talking to any MCP server. NetBox (if MCP path enabled) and future MCP-backed integrations use it.

**Where**: `src/mcp_client.rs`

**Done when**: Adding a new MCP-backed enricher is mostly "write the graph-write mapping" — MCP plumbing is shared.

### T4-7 (v7) — NETCONF/RESTCONF enricher (optional)

**What**: v5 T6-6. For vendors where NETCONF exposes richer structured context than gNMI. Same `GraphEnricher` trait, deterministic (YANG-aware) parsing.

**Priority**: below NetBox and ServiceNow. Implement when a specific operator requirement drives it.

### T4-8 (v7) — Infoblox/BlueCat enricher (deferred)

**What**: v5 T6-4. For environments where IPAM lives outside NetBox.

**Priority**: build when demanded.

---

## <a id="tier-5"></a>TIER 5 — Syslog and SNMP Traps

(v5 T8 / v6 T9 unchanged — signals, not state; separate collector process; signal-aware detectors; signal-triggered investigations. Sequenced after enrichment because orphan-signal routing to the investigation agent is only useful once the agent has the business-context graph from Tier 4.)

### T5-1 through T5-5

Unchanged — signal collector (syslog UDP 514 / TCP 6514, SNMP traps UDP 162), signal-aware detectors, signal-triggered investigation routing, SNMP MIB handling, syslog format discipline.

---

## <a id="tier-6"></a>TIER 6 — Path A Graph Embeddings → Path B GNN

(v5 T10 / v6 T10 unchanged.)

**Sequencing reinforced by v7 audience reframing**: by the time Path B GNN starts, the graph has gNMI-sourced state + NetBox-sourced structure (VLANs, Prefixes) + ServiceNow-sourced business context (Applications, ownership) + optional CLI-scraped richness. That is genuinely richer than gNMI-only and gives the GNN meaningful training signal.

### T6-1 — Path A: node2vec/GraphSAGE embeddings

**What**: graph embeddings as additional features on existing tabular ML. Few sessions; topology-awareness without writing a GNN.

### T6-2 — Path B: Proper GNN with message passing

**What**: PyTorch Geometric. Node-level task. Trained on months of archived data. Coexists with rules and MLDetector as a third detector.

### T6-3 — Enrichment-aware GNN data loader

**What**: the loader must handle numeric, categorical, text, and timestamp enrichment properties. Small schema registry per property type.

---

## <a id="tier-7"></a>TIER 7 — Investigation Agent

(v5 T11 / v6 T11 unchanged.)

**New v7 consideration**: with enrichment elevated, the agent's toolset includes `get_business_context(device_address)` from day one — not as a future upgrade. The agent's value is dramatically higher once ServiceNow CMDB context is in the graph, because investigations can reason about application impact, not just device state.

### T7-1 through T7-4

Unchanged — LangGraph scaffolding with tool surface including graph queries, topology context, business context, playbook library, suggest-playbook-proposal (with mandatory human approval gate), summarise. Cost controls. UI workspace. Agent memory across investigations.

---

## <a id="tier-8"></a>TIER 8 — Controller Adapters (downgraded)

**v7 position**: these are optional integrations, implemented on demand when a specific multi-controller customer drives the requirement. They do not belong in the main execution sequence. The `ControllerAdapter` trait itself is low-cost to design and keeps the extension path open.

### T8-1 (v7) — `ControllerAdapter` trait (keep)

**What**: the trait definition remains useful. Schema-ownership model, distinct runtime role, collector-substitute capability for devices managed exclusively by a controller. Small amount of code.

**Where**: `src/controller/mod.rs`, `src/controller/trait.rs`

**Priority**: low, but implement when any controller adapter work starts so the pattern is established correctly.

### T8-2 (v7) — Multi-controller correlation proof-of-concept

**What**: the one case where bonsai is architecturally unique against controller incumbents. A lab topology with one controller-managed segment (e.g. simulated ACI fabric using containerlab APIC stub) and one gNMI-direct segment. Bonsai pulls state from both into one graph; an incident spanning both segments is observable in bonsai when neither controller alone can see it.

**Where**: design exercise + reference implementation

**Priority**: when a concrete multi-controller audience emerges. Design and document the value prop; don't build speculatively.

### T8-3 through T8-7 — individual adapters (demand-driven)

Meraki, DNAC/Catalyst Center, ACI APIC, vManage, Arista CloudVision, PCEP. All deferred until a specific operator requirement drives implementation. Keep the list in the backlog for continuity but do not schedule.

**Explicitly not targeted for implementation in v7 scope.**

---

## <a id="tier-9"></a>TIER 9 — Scale Architecture and Extensions

### T9-1 — Scale architecture doc

(v5 T5-1 / v6 T6 carried forward.)

### T9-2 — Core bottleneck profiling

(v5 T5-4.)

### T9-3 — S3-compatible archive backend

(v5 T5-3.)

### T9-4 — Disconnected-ops capability flag

(v5 T3-5 / v6 T5-2.)

### T9-5 — NL query layer

(v5 T12-1.)

### T9-6 — ML feature schema versioning

(v5 T12-2.)

### T9-7 — TSDB integration adapter

(v5 T12-8.) Positioning strengthened by v7 enrichment: graph-enriched labels (role, site, application, criticality) on TSDB metrics make bonsai a genuine Telegraf-plus for the primary audience.

### T9-8 — Map visualisation (optional)

(v5 T12-10.)

### T9-9 — Bulk onboarding CSV

(v5 T12-9.)

---

## <a id="tier-10"></a>TIER 10 — Deferred Until Forced

- Bitemporal schema (forced by NL query about history)
- Schema migration tooling (forced by a breaking schema change)
- Grafeo migration evaluation (forced by LadybugDB 60-day quiet)
- Workspace split (forced by incremental build time >60s — currently 20s)

---

## <a id="execution-order"></a>Recommended Execution Order

### Sprint 1 — Tier 0 close-outs and audience doc (1 week)
1. T0-6 audience ADR and CLAUDE.md/PROJECT_KICKOFF update
2. T0-5 onboarding wizard reachable from Devices workspace
3. T0-4 site hierarchy in assignment rules
4. T0-1 summary mode as default in distributed compose profile
5. T0-2 Docker-only seed creds script path
6. T0-3 per-collector TLS certs

### Sprint 2 — UI depth (2 weeks)
7. T1-5 topology HTTP API returns role/site metadata
8. T1-1 Topology: layer filter, site scope, role shapes, link heatmap, path tracing
9. T1-2 Incident grouping logic end-to-end
10. T1-3 Operations workspace depth
11. T1-4 Visual polish pass (keyboard, command palette, skeletons)

### Sprint 3 — Collector-core integration completion (1-2 weeks)
12. T2-1 five-scenario multi-collector validation
13. T2-2 protocol version enforcement
14. T2-3 metrics expansion

### Sprint 4 — Containerisation polish (1-2 weeks)
15. T3-1 image size <100 MB
16. T3-3 ContainerLab integration
17. T3-4 volume backup strategy
18. T3-6 network segmentation deployment guide

### Sprint 5 — Enrichment foundation (2-3 weeks) ⚡
19. T4-1 `GraphEnricher` trait + pipeline
20. T4-6 MCP client infrastructure
21. T4-2 NetBox enricher (flagship)
22. T4-5 enrichment visibility in UI

### Sprint 6 — Business-context enrichment (2 weeks)
23. T4-3 ServiceNow CMDB enricher
24. T4-4 CLI-scraped enricher

### Sprint 7 — Signals (2 weeks)
25. T5-1 through T5-5 — signal collector, detectors, format discipline, trap handling

### Sprint 8 — Path A embeddings (1-2 weeks)
26. T6-1 graph embeddings with enrichment features
27. T9-6 ML feature schema versioning (required before embeddings ship)

### Sprint 9 — NL query (1 week)
28. T9-5 NL query layer

### Sprint 10 — Investigation agent (2-3 weeks)
29. T7-1 scaffolding with business-context tool
30. T7-4 cost controls
31. T7-2 UI workspace

### Sprint 11 — Path B GNN (3-4 weeks)
32. T6-2 GNN with message passing
33. T6-3 enrichment-aware data loader
34. T7-3 agent memory

### Longer horizon (demand-driven)
- T8-1 `ControllerAdapter` trait design
- T8-2 multi-controller correlation proof-of-concept (when audience emerges)
- T4-7 NETCONF enricher (when demanded)
- T4-8 Infoblox enricher (when demanded)
- T9-3 S3 archive backend
- T9-7 TSDB adapter
- T9-2 core profiling
- T3-2 multi-arch build
- T9-8 map UI
- T9-9 bulk onboarding CSV
- T3-5 CI regression monitoring
- T9-4 disconnected-ops flag

### Deferred until forced
Tier 10 items — bitemporal schema, schema migration, Grafeo, workspace split.

---

## <a id="guardrails"></a>Guardrails — Updated for v7

### New in v7

- **Audience-driven scoping.** bonsai's primary target is controller-less environments. Feature work that only serves the controller-integrated secondary audience is deprioritised unless it addresses the specific multi-controller correlation niche. If a feature proposal starts with "this would help customers running DNAC/NDI/Meraki," check whether the controller already provides it; if yes, reject.
- **Enrichment is a primary differentiator, not an afterthought.** The primary audience does not have a controller bringing business context. Bonsai brings it via NetBox and ServiceNow enrichment. This tier outranks controller adapter work.

### Unchanged architectural invariants

- gNMI only for hot-path telemetry state from devices
- Syslog and traps as signals, never as state sources
- tokio only for async Rust
- Credentials never leave the Rust process
- No Kubernetes in v0.x
- No fifth vendor before four are vendor-neutral
- Every non-trivial decision gets an ADR at commit time
- Detect-heal loop does not call an LLM or any enrichment source synchronously
- All operator-facing functionality lives on core
- Enrichers never call LLMs on device configuration — deterministic parsers only (pyATS/TextFSM/YANG)
- Collectors scale horizontally; core scales vertically in v1
- Tabular ML remains production path until GNN has honest validation
- Build time is a first-class metric
- Code landing ≠ work complete: no callsite = not mergeable
- Distributed mode must actually run distributed (mTLS on, no plaintext creds)

### Anti-patterns to reject (expanded)

- "We should build a DNAC replacement" — no, wrong audience, losing position
- "Bonsai should work for every network everywhere" — no, focus matters
- "Let's add a controller adapter speculatively" — no, demand-driven only
- "Controller integration is the primary enrichment story" — no, NetBox/ServiceNow for the primary audience
- "Let's skip the enrichment tier and jump to GNN" — no, GNN without enriched graph is just what v5 critics said about tabular ML
- All prior anti-patterns remain in force

---

## What v7 Explicitly Excludes

For scope discipline, do not start:
- Individual controller adapters beyond the trait design (demand-driven only)
- Auth/RBAC of any kind
- Multi-tenancy in the graph
- Production HA for the core
- A fifth vendor before four are vendor-neutral
- LLM-based parsing of device configuration anywhere outside the investigation agent
- A bonsai-replaces-NDI marketing pitch
- Kubernetes deployment manifests
- Workspace split (defer — 20s incremental is fine)
- Bitemporal schema, schema migration, Grafeo eval

---

*Version 7.0 — authored 2026-04-24 after reviewing post-merge main. Reframes audience explicitly (controller-less is the sweet spot). Downgrades controller adapters to demand-driven. Elevates graph enrichment as the primary business-context mechanism for the primary audience. Incorporates verified v6 progress (counter summariser wired, compose security fixed, UI nav shell built, collector diagnostic endpoint, assignment routing rules). Calls out specific UI gaps (Topology improvements missed, Onboarding wizard orphaned) and small rough edges (per-collector TLS, Docker-only seed script, hierarchy-aware assignment).*
