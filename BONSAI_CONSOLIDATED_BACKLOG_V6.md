# BONSAI — Consolidated Backlog v6.0

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_V5.md`. Produced 2026-04-24 after reviewing the `feature-sprint-4-5-6-distributed-architecture` branch.
>
> **What this iteration delivers vs. what was planned:**
> - Build tuning tier landed (sccache, LTO, baselines captured)
> - Docker Compose skeleton with four profiles landed
> - Store trait abstraction (`BonsaiStore`) with Core + Collector implementations landed
> - Collector-local graph with restricted schema landed
> - Python collector engine with `scope: local | core | hybrid` rule attribution landed
> - Collector UI lockdown landed (stricter than specified — collector has *no* HTTP)
> - mTLS code fully wired (but not exercised by compose profiles)
> - Protocol versioning stubbed
> - Assignment engine landed partially (credential delivery + registry-driven dispatch, but no site→collector routing logic)
>
> **What remains unfinished and is carried forward:**
> - Counter summariser is **dead code** — module exists, tests pass internally, but zero callsites in src/. This is the single biggest Tier 2 gap.
> - Compose profiles don't enable mTLS; plaintext passwords in `docker/configs/core.toml`; `:-changeme` default passphrase is a security footgun.
> - Site→collector routing engine (T3-2 as specified) not built — `collector_id` is a manual per-target field.
> - Collector diagnostic endpoint (`/health`, `/api/collector/status`) not provided.
> - UI work not started; still the minimal v5 state.
> - All enrichment, controller-adapter, investigation-agent, and GNN work remains untouched.

---

## Table of Contents

1. [Progress Since v5 — Verified Against the Branch](#progress)
2. [TIER 0 — Critical Corrections (new category)](#tier-0)
3. [TIER 1 — Collector-Core Integration Gaps](#tier-1)
4. [TIER 2 — UI Usability (elevated from v5 T9)](#tier-2)
5. [TIER 3 — Build Optimisation Remainder](#tier-3)
6. [TIER 4 — Containerisation Hardening (carry from v5 T4)](#tier-4)
7. [TIER 5 — Control-Plane Completeness (carry from v5 T3)](#tier-5)
8. [TIER 6 — Scale Architecture (carry from v5 T5)](#tier-6)
9. [TIER 7 — Graph Enrichment via MCP + Legacy Protocols (carry from v5 T6)](#tier-7)
10. [TIER 8 — Controller Adapters (carry from v5 T7)](#tier-8)
11. [TIER 9 — Syslog and SNMP Traps (carry from v5 T8)](#tier-9)
12. [TIER 10 — Path A Graph Embeddings → Path B GNN (carry from v5 T10)](#tier-10)
13. [TIER 11 — Investigation Agent (carry from v5 T11)](#tier-11)
14. [TIER 12 — Carryover Extensions (carry from v5 T12)](#tier-12)
15. [Execution Order](#execution-order)
16. [Merge Plan](#merge-plan)
17. [Guardrails — Updated](#guardrails)

---

## <a id="progress"></a>Progress Since v5 — Verified Against the Branch

**Completed and removed** (verified with code review, not self-declaration):

| v5 item | Status | Evidence |
|---|---|---|
| T0-1 v5 unpin Dockerfile apt versions | ✅ Done | `docker/Dockerfile.bonsai` apt install has no version pins; comment explains reliance on base image digest |
| T0-2 v5 cargo-chef `--all-targets` | ✅ Done | Line 30 `cargo chef cook --release --all-targets --recipe-path recipe.json` |
| T0-4 v5 proto version negotiation stub | ✅ Done | `protocol_version: uint32` on `TelemetryIngestResponse`, `DetectionEventIngest`, and other RPCs; `collector_core_protocol.md` documents compat policy |
| T1-1 v5 baseline measurements | ✅ Done (partial) | `docs/build_performance.md` captures clean Rust (23 min), Docker clean (40 min), incremental, check, test, UI. **Caveat: baseline on personal Ubuntu, not CI; post-sccache numbers not measured** |
| T1-2 v5 sccache integration | ✅ Configured | `.cargo/config.toml` sets `rustc-wrapper = "sccache"`; `scripts/install_sccache.sh` provided. **Not validated with measurement.** |
| T1-4 v5 LTO and release profile tuning | ✅ Done | `Cargo.toml` release profile — `lto = "thin"`, `codegen-units = 1`, `strip = "symbols"`, `incremental = false` |
| T1-5 v5 dependency audit | ✅ Done | `scripts/dep_audit.sh`; 2026-04-23 run captured in `docs/build_performance.md` with full duplicate list |
| T1-6 v5 Docker parallel stage verification | ✅ Done | Documented in build_performance.md with "verified" flag |
| T2-3 v5 collector-local graph | ✅ Done | `src/collector/graph.rs` (435 lines), restricted schema (Device/Interface/BgpNeighbor/BfdSession/LldpNeighbor/StateChangeEvent/DetectionEvent/Remediation only — no Site, no VLAN, no Application); implements `BonsaiStore` trait |
| T2-4 v5 collector-side detection with scope attributes | ✅ Done | `python/collector_engine.py` + `engine.py` gates rules and ML detectors by `scope: local/core/hybrid`; `run_scope` config; `DetectionEventIngest` RPC carries detections from collector to core |
| T2-6 v5 collector-core protocol contract doc | ✅ Done | `docs/collector_core_protocol.md` with RPC surface, versioning, schema tables |
| T3-1 v5 collector UI lockdown | ✅ Done (stricter than specified) | HTTP UI server only binds in `run_core` blocks in `main.rs:548`. **Note: this means collectors have zero HTTP surface — see T1 v6 for the gap.** |
| T3-3 v5 credential delivery over mTLS | ✅ Code landed | `src/ingest.rs` has full `ClientTlsConfig` + `Identity` plumbing; `RuntimeTlsConfig` in config.rs. Credentials pass via `DeviceAssignment.username/password` delivered on assignment. **Caveat: compose profiles don't exercise this.** |
| T4-1 v5 Docker Compose profiles | ✅ Done (first pass) | `docker-compose.yml` with `dev`, `distributed`, `two-collector`, `chaos` profiles; `docker/configs/*.toml` per role. **Several issues — see T4 v6.** |
| T4-5 v5 volume lifecycle documentation | ✅ Done | `docs/deployment_volumes.md` |

**Partially done — material gaps**:

- **T2-1/T2-2 counter debounce + summariser is DEAD CODE.** `src/counter_summarizer.rs` is a correct 109-line implementation of the window-aligned, per-interface summariser. Protobuf messages (`InterfaceSummary`, `InterfaceCounterSummary`) and `summary_to_ingest_update` exist in `ingest.rs`. **Zero callsites anywhere in src/.** The collector still forwards every raw counter update. The primary Tier 2 bandwidth win is unrealised. → T0-1 v6.
- **T3-2 v5 assignment engine is partial.** `src/assignment.rs` has `CollectorManager` that forwards registry changes as `AssignmentUpdate` messages and resolves credentials from the vault. But `collector_id` on a target is manually set (operator edits config or calls API). The "site → collector" routing logic from v5 T3-2 is not built. → T1 v6.
- **T4-1 v5 compose profile mTLS.** mTLS is coded in Rust but the compose profiles use `http://bonsai-core:50051` (not https). No TLS certs for core↔collector (only for gNMI to lab devices). → T4 v6.
- **Security issues in the compose setup**:
  - `BONSAI_VAULT_PASSPHRASE=${BONSAI_VAULT_PASSPHRASE:-changeme}` default in compose — if operator doesn't set it, the vault is encrypted with `changeme`, same on every machine
  - `docker/configs/core.toml` contains plaintext `password = "NokiaSrl1!"` for all four lab devices — contradicts the credential-vault-only control-plane discipline
  - → T0-2 v6

**Other small code-review findings:**

- Dockerfile creates custom Rust healthcheck binary to "avoid busybox" but then installs `curl` in runtime; the healthcheck shells out to curl. Either commit to distroless (no curl) or accept busybox. Current state is muddled. → T0-3 v6.
- Dockerfile uses hardcoded UID 65532 with a distroless-style comment but runs on `debian:trixie-slim`. No user entry in `/etc/passwd`. → T0-3 v6.
- `docker-compose.yml` has obsolete `version: '3.8'` top-level field (deprecated by Compose v2). → T0-4 v6.
- `docker-compose.yml` sets `container_name:` rigidly, preventing the compose from running twice on one host. → T0-4 v6.
- `docker-compose.yml` `depends_on: [bonsai-core]` without `condition: service_healthy` — collectors start before core is listening, producing noisy retry logs. → T0-4 v6.
- Counter summariser's `InterfaceSummary` doesn't carry target/if_name fields — those would need to be added when summaries actually start flowing. → folded into T0-1 v6.
- Counter summariser uses lazy flush — a silent interface never emits its partial summary. Timer-driven flush is needed. → folded into T0-1 v6.
- `InterfaceCounterSummary::mean` is `f64` but `min/max` are `i64`. For counter deltas this is fine; for rate calculations the downstream consumer needs to know the window. → folded into T0-1 v6.

---

## <a id="tier-0"></a>TIER 0 — Critical Corrections

These are not optional. They fix integrity issues in what just landed.

### T0-1 (v6) — Wire the counter summariser into the live stream

**What**: `src/counter_summarizer.rs` is a complete implementation that never runs. Call `CounterSummarizer::observe` on every `TelemetryUpdate` in the collector forwarder path. When `observe` returns a `Some(InterfaceSummary)`, convert it via `summary_to_ingest_update` and send it to core instead of the raw updates. Drop raw counter updates that don't roll the window.

**Why this is critical**: T2-1/T2-2 was the whole point of the data-exchange redesign. Every raw counter update we ship from collector to core is wasted bandwidth and wasted core CPU on data the core doesn't need. Without this, distributed mode is just Tier 1 (dumb forwarder) with extra ceremony.

**Where**: `src/ingest.rs` collector forwarder path; probably a new field on `CollectorConfig` for window size

**Additional fixes in the same change**:
- Add `target: String` and `if_name: String` to the `InterfaceSummary` proto so summaries carry routing context
- Add a timer-driven flush — if no update arrives for an interface for `window_duration + 10s`, emit whatever's buffered
- Add a config flag `counter_forward_mode = "raw" | "debounced" | "summary"` matching v5's planned design; default to `"summary"` in distributed mode
- Unit test: stream 60 interface-counter updates across 2 interfaces, assert exactly 2 summaries emitted at minute boundary
- Integration test: one-site run with summary mode reduces collector→core bytes/minute by ≥80% vs raw mode

**Done when**: a two-minute distributed-mode run shows the collector emitting one summary per interface per minute on the wire, raw counter data visible only in the collector-local archive, core event bus receiving only state-change events + summaries, and measured bandwidth reduction documented.

### T0-2 (v6) — Security issues in compose setup

**What**: three concrete fixes to prevent the compose profiles from undermining the control-plane discipline from v5.

**Where**: `docker-compose.yml`, `docker/configs/core.toml`, `docker/configs/collector-*.toml`

**Fixes**:

1. **Remove the `:-changeme` passphrase default.** If `BONSAI_VAULT_PASSPHRASE` isn't set, the compose should fail with a clear error. Lead the operator to `docker/secrets/vault_passphrase.example` or an `.env.example` that documents the expectation. Never ship a shared default.

2. **Remove plaintext device passwords from `docker/configs/core.toml`.** Per v5 T3-1/T3-3, credentials live only in the vault. The compose startup script should:
   - Detect that no credentials exist in the vault
   - Prompt the operator once to enter lab credentials under an alias (e.g. `lab-admin`)
   - Re-populate `bonsai-registry.json` with `credential_alias: "lab-admin"` instead of inline credentials
   - Or: ship a `scripts/seed_lab_creds.sh` that adds aliases and populates the registry once

3. **Add mTLS to the distributed + two-collector profiles.** Ship a `scripts/generate_compose_tls.sh` that produces a CA, core cert, and collector certs into `docker/tls/`. Volume-mount into each container. Enable `runtime.tls.enabled = true` in core.toml and collector-*.toml. Flip `core_ingest_endpoint` from `http://` to `https://`.

**Done when**: Running the `distributed` profile with defaults fails loudly on missing `BONSAI_VAULT_PASSPHRASE`. No plaintext credentials anywhere in git. mTLS is the default transport, not an after-the-fact opt-in. Documentation captures the one-time cert-generation step in the quick-start guide.

### T0-3 (v6) — Dockerfile internal consistency

**What**: the Dockerfile has a few self-contradictions that add confusion without adding value.

**Where**: `docker/Dockerfile.bonsai`

**Fixes**:

1. **Pick one runtime posture and commit to it.** Options:
   - **A (recommended)**: Keep `debian:trixie-slim`. Create the `bonsai` user explicitly with `useradd`. Drop the "distroless 65532" comment. Keep curl installed for the healthcheck (remove the bespoke Rust healthcheck binary — unnecessary complexity).
   - **B**: Switch to `gcr.io/distroless/cc-debian12:nonroot`. Remove curl (distroless has no shell). Keep the bespoke Rust healthcheck binary. UID 65532 is correct in distroless.

2. **Document the `LBUG_SHARED=1` unconditional setting.** `.cargo/config.toml` currently sets it for all platforms. Windows developers without the shared build will get confusing errors. Either:
   - Guard it with a `[target.'cfg(not(windows))']` env section, OR
   - Add a comment explaining that Windows developers need extra setup (link to `build.rs:copy_lbug_shared_dll`)

3. **Remove the `RUSTC_WRAPPER=` lines inside Dockerfile** and rely on the default (no wrapper) inside the build image. Makes the absence of sccache inside containers explicit and documentable. Add a comment.

**Done when**: Dockerfile posture is consistent; a fresh reader does not hit "why is there curl if we removed busybox" confusion; `cargo build` on a clean Windows machine produces a clear error or succeeds, not a cryptic failure.

### T0-4 (v6) — Compose file hygiene

**What**: small fixes making the compose robust.

**Where**: `docker-compose.yml`

**Fixes**:

1. Remove `version: '3.8'` top-level line (obsolete, produces warning in Compose v2)
2. Remove `container_name:` from all services — let Docker auto-generate names. This also lets the compose run twice on one host (two lab environments).
3. Change `depends_on: [bonsai-core]` to the long-form with `condition: service_healthy` so collectors wait for core's healthcheck to pass before starting.
4. The `chaos` profile runs a hardcoded 500ms delay. Either parameterise via env vars or document this as a sample and suggest operators override via Compose file inheritance.

**Done when**: `docker compose up` produces no warnings; concurrent runs of two lab environments on one host work; collectors don't log connection-refused on initial startup.

### T0-5 (v6) — Validate sccache actually helps

**What**: sccache is configured but its impact isn't measured. The baseline in `build_performance.md` is pre-sccache. Run the baseline script again after sccache is primed (second run) and compare.

**Where**: `docs/build_performance.md` update + `scripts/build_bench.sh`

**Done when**: the document has a post-sccache measurement row showing the real improvement (or lack of it). If sccache's benefit is small on this codebase, the ADR explains why (likely because the builds are dominated by one big crate — the project itself — which sccache doesn't help).

### T0-6 (v6) — CI baseline instead of personal laptop baseline

**What**: the build_performance.md baseline was captured on the operator's Ubuntu laptop. CI build times are what matter operationally. Run the baseline on a standard CI runner (GitHub Actions `ubuntu-latest` at minimum).

**Where**: `.github/workflows/build_bench.yml` (new) + `docs/build_performance.md` update

**Done when**: build times in the doc are CI-sourced, runner-specified; a weekly CI job re-measures and catches regressions.

---

## <a id="tier-1"></a>TIER 1 — Collector-Core Integration Gaps

These are the items partially landed in the branch that need completion before distributed mode is operationally real.

### T1-1 (v6) — Assignment engine site→collector routing

**What**: today `TargetConfig.collector_id` is a free field the operator manually sets. v5 T3-2 specified an assignment engine that routes devices based on `(site, role) → collector` rules configured once. Build that.

**Where**:
- `src/assignment.rs` — extend `CollectorManager` with routing rules
- `config.rs` — new `[assignment]` section with routing rules
- `http_server.rs` — `/api/assignment/rules` endpoints

**Design**:
```toml
[[assignment.rules]]
match_site = "dc-london"       # site name or parent-site name (hierarchy aware)
match_role = "leaf"            # optional; omit to match any role
collector_id = "collector-1"
priority = 10                  # higher priority wins when multiple rules match

[[assignment.rules]]
match_site = "dc-paris"
collector_id = "collector-2"
priority = 10
```

When a device is added with no `collector_id`:
1. Assignment engine evaluates rules in priority order
2. First matching rule assigns the device
3. No rule matches → device stays unassigned, UI flags it clearly

**Additional requirements**:
- Removing a collector causes its devices to be re-evaluated against rules
- Changing a device's site re-evaluates its assignment
- Operator can override with explicit per-device assignment that takes precedence over rules

**Done when**: A two-collector test with three sites and routing rules produces correct automatic assignments when devices are added; reassignment on site change works; unassigned-because-no-rule devices render with a clear UI warning.

### T1-2 (v6) — Collector diagnostic endpoint

**What**: v5 T3-1 specified a minimal diagnostic HTTP endpoint on collectors (`/health`, `/api/readiness`, `/api/collector/status`) that operators can hit during troubleshooting without SSH. The branch took the stricter path (no HTTP at all on collectors). Add back a narrowly-scoped diagnostic endpoint.

**Where**: `src/collector/mod.rs` or new `src/collector/diagnostic_server.rs`

**Design**:
- Port configurable, disabled by default unless `collector.diagnostic_port` is set
- Three endpoints only: `/health` (liveness), `/api/readiness` (did we register with core successfully), `/api/collector/status` (queue depth, assigned devices, subscription states, last heartbeat, vault resolved-credentials count)
- Optional basic auth via `BONSAI_COLLECTOR_DIAG_PASSWORD`
- All other HTTP paths return 404 (not even redirects)

**Done when**: an operator debugging a collector that can't forward detections can hit `/api/collector/status`, see queue depth growing, see subscription states, and diagnose the issue without reading logs.

### T1-3 (v6) — Multi-collector validation end-to-end

**What**: once T0-1 (summariser), T0-2 (mTLS compose), T1-1 (routing), T1-2 (diagnostic) are done, run a clean validation pass against a two-collector topology.

**Where**: `docs/distributed_validation.md` extension

**Validation scenarios**:
1. Two collectors, three sites, route-driven assignment — correct devices land on correct collectors
2. Collector 1 crashes — its devices transition to `unassigned`; when it restarts, devices come back via re-registration
3. Core-unreachable test — collector queues detections and telemetry, both drain on reconnect
4. Credential rotation — operator changes alias on core, new credentials delivered on assignment update, collector switches without missing data
5. Site reassignment — operator moves device from site A to site B, collector-1 unsubscribes, collector-2 subscribes, data continuity verifiable

**Done when**: all five scenarios are documented with step-by-step reproduction; any that fail become specific follow-up items.

### T1-4 (v6) — Protocol version enforcement

**What**: the version field exists in proto; nothing reads it. Add compat checks.

**Where**: `src/api.rs` (core side — when collector connects), `src/ingest.rs` (collector side — when receiving responses)

**Design**:
- `PROTOCOL_VERSION_CURRENT: u32 = 1` constant in both
- Collector sends its version on first request (already does via `TelemetryIngestUpdate.protocol_version`)
- Core logs warning on mismatch; returns error on major-version skew (future)
- Both core and collector log their running version on startup

**Done when**: a collector and core with intentionally mismatched versions produce clear warning logs; major-skew path is reachable (even if there's only one major version today).

---

## <a id="tier-2"></a>TIER 2 — UI Usability (elevated priority)

**Context**: the UI has not been touched since the onboarding wizard landed. It remains a minimal demo UI. This is now the most operator-visible gap in the project. Network practitioners who would evaluate bonsai today would see a competent backend with a toy frontend and dismiss it.

This tier is elevated to Tier 2 (high priority, after Tier 0 corrections and Tier 1 completeness) because: (a) the backend is now capable enough that the UI is the bottleneck; (b) a real UI is the only way to do proper usability evaluation; (c) the gap is visible and embarrassing to show anyone.

### T2-1 (v6) — Workflow-centric navigation shell

**What**: replace the current navigation (`Topology / Onboarding / Events`) with one that matches network-practitioner workflows.

**Proposed routes**:
- `/` — **Live** (default landing): split view with topology left, live detection feed right. Operator's incident-time home.
- `/incidents` — incident-grouped view (not raw event list). An incident aggregates cascading detections from a root cause.
- `/devices` — managed devices (onboarding wizard + device list + device detail)
- `/sites` — site hierarchy workspace
- `/credentials` — vault UX (separate workspace, not buried in the wizard)
- `/collectors` — collector status (new) — shows registered collectors, queue depths, assignment state
- `/enrichment` — enricher status (for future Tier 7)
- `/investigations` — agent workspace (for future Tier 11)
- `/operations` — operator observability (event bus backpressure, archive lag, subscriber health, etc.)

**Where**: `ui/src/App.svelte` routing; new `ui/src/routes/` directory structure

**Design**:
- Use a proper client-side router (e.g. `svelte-navigator` or `@roxi/routify`) instead of the current manual tab switcher
- Active-section highlighting in the nav
- Deep-linking: every entity (device, incident, site) has a URL that can be bookmarked and shared
- Mobile-responsive nav that collapses to a hamburger on small screens (on-call engineers on phones)

**Done when**: each of the nine routes renders (even if initially empty placeholders for future tiers); `/devices/10.1.1.1` deep-links to the device detail; nav highlights the active route.

### T2-2 (v6) — Live view: topology + incident feed

**What**: the default landing page. Left: interactive topology. Right: live detection feed. This is where operators spend time during incidents.

**Where**: `ui/src/routes/Live.svelte`, reuses `Topology.svelte` as a component

**Design**:
- Topology pane:
  - Existing force-directed view as a starting point
  - **Health-colour nodes**: green (no recent detections), yellow (recent), red (active)
  - **Role shapes**: leaves circles, spines squares, PE/P hexagons. Distinguishable at a glance.
  - **Layer filter chip**: L3 only / L2 only / all. Graph data already has this information.
  - **Site filter**: scope to one site or one parent-site subtree
  - **Click node** → side drawer with device details, current state, recent detections
  - **Shift-click two nodes** → highlight graph-shortest path. Single Cypher traversal.
- Feed pane:
  - Live detection events, newest first
  - Severity colouring
  - Click detection → jumps to `/incidents/<id>` for full context
  - Filter chips: severity, rule type, site

**Done when**: operator opens bonsai during a test chaos run and can see the anomalous device glow red, click it for context, see the feed correlating events.

### T2-3 (v6) — Incident-centric grouping

**What**: the current `Events.svelte` shows raw detections linearly. Network practitioners think in **incidents** — a root cause and its cascading impacts. Group detections into incidents using graph traversal.

**Where**: `ui/src/routes/Incidents.svelte` + new gRPC endpoint `GroupedIncidents(window_secs)` on core

**Grouping rule (initial heuristic)**:
- Detections within 30 seconds of each other that share a path through the graph → one incident
- Root detection = the one most-upstream in the topology (lowest graph-incoming-edge count among affected devices)
- Cascading detections = downstream of the root

**Incident card displays**:
- Root detection (device, rule, time, severity)
- Cascading detection count ("+ 4 downstream detections")
- Affected devices list
- Affected sites list (leveraging Site as graph entity)
- Ongoing remediation status (pending / in-progress / succeeded / failed)
- Time-to-healed if closed
- Link to full causal tree (matches NetAI screenshot #10)

**Done when**: A BGP session flap producing 3 cascading detections renders as one incident card, not three separate rows; the expand-to-causal-tree view shows the topology-annotated propagation.

### T2-4 (v6) — Device detail drawer overhaul

**What**: device detail today is minimal. Operators want everything they'd normally `show interfaces`, `show bgp`, `show lldp`, `show logging` for — in one place, from the graph.

**Where**: `ui/src/components/DeviceDrawer.svelte`

**Sections**:
- Header: hostname, address, vendor, role, site, subscription status
- Recent state changes (last 30 min): what transitioned, when
- Interfaces: list with oper state, recent error counts, recent flap counts
- Peers (BGP/LLDP): list with state
- Subscription paths: what's subscribed, last telemetry received, any `silent` flags
- Enrichment (future): NetBox, ServiceNow, Meraki, DNAC data namespaced appropriately
- Recent detections on this device: last 5 with quick severity display
- Audit: who added this device, when, by what operator action

**Done when**: an operator troubleshooting a specific device has a single drawer that answers "what's going on with this box" without navigating elsewhere.

### T2-5 (v6) — Collector status workspace

**What**: the new `/collectors` route. Shows all registered collectors with their health, assignment state, queue depth, last heartbeat, version.

**Where**: `ui/src/routes/Collectors.svelte` + new `/api/collectors` endpoint

**Operator actions from this workspace**:
- See unassigned devices (devices with no collector due to no routing rule match)
- Manually reassign a device to a different collector (override rule)
- See per-collector subscription-observed vs subscribed-but-silent ratio — quick quality signal

**Done when**: a collector outage is visible from the UI within 2 minutes (heartbeat miss); an operator can reassign a device without CLI or config edits.

### T2-6 (v6) — Site hierarchy workspace

**What**: the `/sites` route. Currently operators can pick a site when onboarding but cannot visualise the hierarchy or manage sites properly.

**Where**: `ui/src/routes/Sites.svelte`

**Features**:
- Tree view of site hierarchy (expandable regions → DCs → racks)
- Site detail panel: devices in site, aggregate health, recent detections scoped to site, subscription summary
- Site CRUD: create/edit/delete, with parent picker and kind selector
- Drag-drop to reparent sites
- Optional lat/lon fields (lays groundwork for future map view)

**Done when**: operator can bring up bonsai, model their three-region five-DC corporate network as a site tree, and see each site's health independently.

### T2-7 (v6) — Credential vault workspace

**What**: extract credential management from the onboarding wizard into a dedicated workspace. Onboarding still has inline "add credential" but credentials are primarily managed from their own page.

**Where**: `ui/src/routes/Credentials.svelte`

**Features**:
- List of aliases (never the secret)
- Per-alias: created/updated timestamps, last-used timestamp, device count using this alias
- Add new credential (username + password + alias name)
- Edit credential (update password only — alias is immutable)
- Delete credential (blocked if any device references it; clear error)
- "Test credential" button — dials DiscoverDevice against an operator-specified test address to verify the credential works

**Done when**: credential lifecycle is manageable without touching the onboarding wizard; operators can rotate passwords with confidence.

### T2-8 (v6) — Operations observability workspace

**What**: the `/operations` route. "What is bonsai doing right now?"

**Where**: `ui/src/routes/Operations.svelte`

**Panels**:
- **Event bus health**: current throughput, lag per consumer, dropped events (broadcast overflow)
- **Archive health**: flush lag (how old is the oldest unarchived event), last flush duration, compression ratio
- **Subscriber health**: count of subscribers, reconnect frequency, per-device subscribed/observed/silent state counts
- **Collector health**: summary of `/collectors` with aggregate numbers
- **Rule engine**: detection rate per rule_id, any rules with zero fires in last hour (possibly dead)
- **Metrics link**: links to the `/metrics` Prometheus endpoint for deeper introspection

**Done when**: an operator asking "bonsai feels slow, what's happening?" has one page to check before escalating.

### T2-9 (v6) — Visual polish and accessibility

**What**: UI basics that every production tool has.

**Items**:
- **Keyboard navigation**: Tab through the wizard, Enter to advance, Esc to go back, Ctrl+K command palette (jump to any device/site/collector by name)
- **Dark mode default** (it already is), but respect `prefers-color-scheme`
- **Loading states**: skeletons instead of spinners for lists that have known-shape content
- **Error states**: every failed API call shows a dismissable toast with actionable message
- **Copy-friendly**: addresses, paths, and selectors have copy buttons
- **Empty states**: every list that could be empty has a helpful "no X yet, try Y" message
- **Time rendering**: relative ("2 min ago") with absolute tooltip on hover
- **Status badges**: consistent colour vocabulary across the app — green for healthy, yellow for degraded, red for failing, grey for unknown, blue for informational

**Where**: distributed across UI components + a `ui/src/lib/ui-primitives/` directory for shared components

**Done when**: Lighthouse accessibility score ≥90; a keyboard-only operator can complete the add-device workflow; no unnamed colour usage in production.

---

## <a id="tier-3"></a>TIER 3 — Build Optimisation Remainder

Most of the build work landed. These remain.

### T3-1 (v6) — Docker image size reduction

**What**: v5 T1-8. Current image is 131 MB (measured). Target <100 MB.

**Where**: `docker/Dockerfile.bonsai`

**Options (measure each)**:
- Switch to distroless (per T0-3 Option B)
- Pre-compress the UI static assets (gzip/brotli alongside the raw files)
- Strip additional symbols from the bonsai binary if not already

**Done when**: image <100 MB; build_performance.md updated.

### T3-2 (v6) — Docker multi-arch build

**What**: v5 T1-7. Today builds host arch only. Support `linux/arm64` for Apple Silicon.

**Where**: `docker/Dockerfile.bonsai` + CI

**Known risk**: LadybugDB's `lbug` crate needs to compile on arm64. If there's x86-specific assembly, this blocks multi-arch until upstream fixes.

**Done when**: `docker buildx build --platform linux/amd64,linux/arm64` succeeds; arm64 image works on Apple Silicon without emulation.

### T3-3 (v6) — UI build: Vite tuning and Lighthouse audit

**What**: v5 T1-9.

### T3-4 (v6) — CI pipeline for build-time monitoring

**What**: v5 T1-10.

### T3-5 (v6) — Workspace split (conditional)

**What**: v5 T1-3 was flagged as conditional on baseline showing pain. Baseline shows 23-minute clean build (meaningful) and 20-second incremental (acceptable). Incremental is fine — workspace split doesn't help the clean build. **Defer indefinitely unless incremental rises above 60 seconds.**

---

## <a id="tier-4"></a>TIER 4 — Containerisation Hardening (carry from v5 T4)

### T4-1 (v6) — ContainerLab integration

(v5 T4-2, unchanged.)

### T4-2 (v6) — Container secrets handling

(v5 T4-3, unchanged — but T0-2 v6 has already partly addressed this.)

### T4-3 (v6) — Chaos in containers

(v5 T4-4, unchanged — the current pumba-based chaos is a starting point; the bonsai chaos runner should also containerise.)

### T4-4 (v6) — Compose profile mTLS by default

Folded into T0-2 v6.

### T4-5 (v6) — Volume backup strategy

**What**: v5 T4-5 documented volume roles; now add practical backup patterns for the `bonsai_creds` and `bonsai_archive` volumes.

**Where**: `docs/deployment_volumes.md` extension

**Done when**: doc includes `docker run --rm -v bonsai_creds:/src -v $(pwd):/dst alpine tar czf /dst/creds-backup.tar.gz -C /src .` style recipes; restore procedure also documented.

---

## <a id="tier-5"></a>TIER 5 — Control-Plane Completeness (carry from v5 T3)

### T5-1 (v6) — Network segmentation doc

(v5 T3-4, unchanged — write the operator guide for management-plane / user-plane split.)

### T5-2 (v6) — Disconnected-ops capability flag design

(v5 T3-5, unchanged — design-only, no code yet.)

---

## <a id="tier-6"></a>TIER 6 — Scale Architecture (carry from v5 T5)

(v5 T5-1 through T5-5, unchanged.)

---

## <a id="tier-7"></a>TIER 7 — Graph Enrichment via MCP + Legacy Protocols (carry from v5 T6)

(v5 T6-1 through T6-8, unchanged.)

---

## <a id="tier-8"></a>TIER 8 — Controller Adapters (carry from v5 T7)

(v5 T7-1 through T7-8, unchanged. Priority order: Meraki first as the flagship, then DNAC, then demand-driven for ACI/vManage/Arista/PCEP.)

---

## <a id="tier-9"></a>TIER 9 — Syslog and SNMP Traps (carry from v5 T8)

(v5 T8, unchanged.)

---

## <a id="tier-10"></a>TIER 10 — Path A Graph Embeddings → Path B GNN (carry from v5 T10)

(v5 T10-1 through T10-3, unchanged. Still sequenced after enrichment and controller-adapter work so the graph is rich enough to justify GNN work.)

---

## <a id="tier-11"></a>TIER 11 — Investigation Agent (carry from v5 T11)

(v5 T11-1 through T11-4, unchanged.)

---

## <a id="tier-12"></a>TIER 12 — Carryover Extensions (carry from v5 T12)

(v5 T12-1 through T12-12, unchanged.)

---

## <a id="execution-order"></a>Recommended Execution Order

### Sprint 1 — Ship the Tier 0 corrections (1-2 weeks)
1. **T0-1 v6** — wire the counter summariser into the live stream. This is the single most important item in v6. Without it, distributed mode is just the old Tier 1 forwarder with extra cost.
2. T0-2 v6 — compose security fixes (no `:-changeme` default, no plaintext creds in configs, mTLS in distributed profiles by default)
3. T0-3 v6 — Dockerfile internal consistency
4. T0-4 v6 — compose hygiene (version, container_name, healthcheck ordering)
5. T0-5 v6 — validate sccache impact with measurement
6. T0-6 v6 — CI-sourced build baseline

### Sprint 2 — Collector-core integration gaps (1-2 weeks)
7. T1-1 v6 — assignment engine site→collector routing
8. T1-2 v6 — collector diagnostic endpoint
9. T1-4 v6 — protocol version enforcement (not just stubs)
10. T1-3 v6 — five-scenario multi-collector validation

### Sprint 3 — UI usability pass, phase 1 (2 weeks)
11. T2-1 v6 — workflow-centric navigation shell
12. T2-2 v6 — Live view (topology + incident feed)
13. T2-3 v6 — incident-centric grouping
14. T2-9 v6 — visual polish and accessibility (threaded through the UI work, not a separate sprint)

### Sprint 4 — UI usability pass, phase 2 (2 weeks)
15. T2-4 v6 — device detail drawer overhaul
16. T2-5 v6 — collector status workspace
17. T2-6 v6 — site hierarchy workspace
18. T2-7 v6 — credential vault workspace
19. T2-8 v6 — operations observability workspace

### Sprint 5 — Build remainder + containerisation hardening (1-2 weeks)
20. T3-1 v6 — Docker image <100 MB
21. T3-2 v6 — multi-arch build
22. T4-1 v6 — ContainerLab integration
23. T4-5 v6 — volume backup patterns
24. T5-1 v6 — network segmentation doc

### Sprint 6 — Enrichment foundation (2-3 weeks)
(v5 T6 — unchanged — `GraphEnricher` trait, MCP client infrastructure, NetBox enricher, CLI-scraped enricher, enrichment UI workspace)

### Sprint 7 — Controller adapters (2-3 weeks)
(v5 T7 — unchanged — `ControllerAdapter` trait, Meraki flagship, DNAC)

### Sprint 8 — ServiceNow + signals (2 weeks)
(v5 T6-3 + T8 — unchanged — ServiceNow CMDB + syslog/trap collector + signal-aware detectors)

### Sprint 9 — Path A embeddings (1-2 weeks)
(v5 T10-1 — unchanged.)

### Sprint 10 — NL query (1 week)
(v5 T12-1 — unchanged.)

### Sprint 11 — Investigation agent (2-3 weeks)
(v5 T11 — unchanged.)

### Sprint 12 — Path B GNN (3-4 weeks)
(v5 T10-2 — unchanged — the destination.)

### Longer horizon
(Unchanged from v5 — ACI/vManage/Arista/PCEP adapters, Infoblox enricher, NETCONF enricher, S3 archive, map UI, multi-layer topology UI, etc.)

### Defer until forced by pain
(Unchanged — bitemporal schema, schema migration, Grafeo eval, workspace split.)

---

## <a id="merge-plan"></a>Branch Merge Plan — `feature-sprint-4-5-6-distributed-architecture`

**The branch should NOT merge to main as-is.** The Tier 0 items above are blockers. Specifically:

- **Counter summariser dead code** is an integrity issue — code was written but not integrated. Merging sends the message "declared done" when it is not.
- **Compose security issues** (plaintext creds, `:-changeme` default) directly contradict the control-plane discipline we just set in v5.
- **Compose mTLS off** is a credibility issue for the distributed-mode validation claim.

**Recommended approach**: address Sprint 1 (Tier 0 corrections) as in-branch commits first, then merge. The Sprint 1 work is small (1-2 weeks at most); doing it in-branch keeps the merge coherent rather than shipping incomplete work and following up with patches.

Once Sprint 1 is done, stage the merge in sequence:

### Stage 1 — Small Tier 0 items directly to main
- T0-3, T0-4, T0-5, T0-6

### Stage 2 — Build tuning (1 PR)
- `.cargo/config.toml`
- Release profile changes in `Cargo.toml`
- `scripts/install_sccache.sh`, `build_bench.sh`, `dep_audit.sh`
- `docs/build_performance.md` with post-sccache and CI-sourced measurements

### Stage 3 — Store trait + collector graph (1 PR, careful review)
- `src/store.rs` (new trait)
- `src/collector/` module
- `src/graph/` directory refactor (if not already separate stage)
- Python collector engine (`python/collector_engine.py`, engine.py scope support)

### Stage 4 — Counter summariser (THIS stage must include integration, not just the module)
- `src/counter_summarizer.rs`
- Integration in `src/ingest.rs` forwarder path
- `CollectorFilterConfig` additions
- Proto additions for `InterfaceSummary` with target/if_name context

### Stage 5 — Assignment engine + protocol versioning
- `src/assignment.rs` with routing rules
- `[assignment.rules]` config surface
- Proto version enforcement in `api.rs` and `ingest.rs`
- `docs/collector_core_protocol.md`

### Stage 6 — Dockerfile and compose
- `docker/Dockerfile.bonsai` with T0-3 fixes
- `docker-compose.yml` with T0-4 hygiene + T0-2 security fixes
- `docker/configs/*.toml` without plaintext creds
- `scripts/generate_compose_tls.sh` for mTLS bootstrapping
- `docs/deployment_volumes.md`

### Stage 7 — mTLS wiring in ingest
- `src/ingest.rs` TLS path
- `src/config.rs` `RuntimeTlsConfig`
- `docs/distributed_tls.md` updated for compose usage

### After all stages
- Tag `v0.5.0`
- Release note summarising Tier 0 v5 items, T1-T3 v5 items, and the v6 corrections

---

## <a id="guardrails"></a>Guardrails — Updated for v6

### Added guardrail: "code landing" ≠ "work complete"

v6 surfaces a specific failure pattern worth naming: **the counter summariser was implemented with tests, documented in proto messages, had helper functions written — but had zero callsites.** This is a form of dead code that looks like progress but isn't. Add to the commit-discipline:

**A feature is not complete until it runs in the live path with a measurable effect.** Merging code that isn't called produces false signal. At PR-review time, for any new module, reviewer asks: "where is this called from?" No callsite = not mergeable.

### Added guardrail: "distributed mode must run distributed"

If compose profiles claim distributed architecture but run with `http://` (not mTLS) and plaintext credentials in config files, we're testing a cosplay of distributed mode, not distributed mode itself. **Validation scenarios (T1-3) and compose profiles must exercise the actual distributed discipline.**

### Unchanged guardrails from v5

- gNMI only for hot-path telemetry state; syslog/traps as signals
- tokio only for async Rust
- Credentials never leave the Rust process except on outbound gNMI connections; collectors hold credentials in memory only
- No Kubernetes in v0.x
- No fifth vendor before four work vendor-neutrally
- Every non-trivial decision gets an ADR at commit time
- Detect-heal loop does not call an LLM or any enrichment source synchronously
- All operator-facing functionality lives on core
- Enrichers never call LLMs on device configuration — deterministic parsers only (pyATS/TextFSM/YANG)
- Controller adapters are distinct from source-of-truth enrichers — separate runtime role
- Tabular ML remains the production path until GNN has honest validation
- Collectors scale horizontally; core scales vertically in v1
- Build time is a first-class metric

### Anti-patterns to reject

- "We shipped the module, wiring it up is a follow-up" — no. Code ships with callsites.
- "The default passphrase is fine for lab use" — no. A shared default is worse than a prompt that fails loudly.
- "We'll enable mTLS later, for now http:// is easier" — no. If the distributed architecture claim is real, mTLS is the default.
- "The UI is minimal but the backend is done, so we're good" — no. For a tool network practitioners will evaluate, the UI is the product.
- All prior v5 anti-patterns remain in force.

---

## What v6 Explicitly Excludes

For scope discipline, do not start:
- Controller adapters or enrichers until Tier 0 and Tier 1 are complete
- GNN work until enrichment is in place
- Kubernetes deployment manifests
- Auth/RBAC
- A fifth vendor before four are vendor-neutral
- LLM-based parsing of device configuration anywhere outside the investigation agent
- Workspace split or any other build optimisation beyond T3 items (keep v5's deferral)

---

*Version 6.0 — authored 2026-04-24 after reviewing the `feature-sprint-4-5-6-distributed-architecture` branch. Reflects substantial progress on store trait + collector graph + scope-attributed rules + build tuning + compose skeleton + protocol versioning. Surfaces critical gaps: counter summariser dead code, compose security issues, partial assignment engine. Elevates UI usability to Tier 2 priority. Adds two guardrails — "code landing ≠ work complete" and "distributed mode must run distributed" — drawn from patterns seen in this review.*
