# BONSAI — Backlog Bravo Series, v2-modified (Bv2-mod)

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_BV2.md`. Authored 2026-05-04 as a pivot, not a redraft, after operator clarification on three structural facts:
>
> 1. **MVP, not a polished v1, is the next milestone.** The current Bv2 sequenced Sprints 1-2 as preparation (hardcoding cleanup, feedback infrastructure) before any operating began. This pivot flips that: operate first, fix what real operation surfaces.
> 2. **Token budget for the investigation agent is not currently available.** The agent code in `python/bonsai_agent/` requires Anthropic API tokens to function. Until budget is allocated, every agent-related task is post-MVP.
> 3. **The "AI feedback loop" is Claude Code / Codex (subscription-billed), not Claude API (token-billed).** The diagnostic intelligence is the dev's coding agent observing the running application, not a bonsai-side LLM consumer. This is dramatically cheaper and arguably more effective. The application emits structured signal; the agent (separate process, separate billing) consumes it.
>
> **What this changes in priorities**: Sprints reorder around what's actually gating MVP rather than what's preparatory. The GNN remains the north star but its sprint is sized realistically — it needs 30+ days of real chaos-run archive, which only accumulates if we engage the lab continuously starting now.
>
> **What stays unchanged**: every prior backlog (v2-v12, Bv1, Bv2 itself) remains in repo. Strategic positioning, audience framing, gNMI-only hot path, controller-less primary target, enrichment philosophy, AIOps-feeder framing all unchanged. Bv2-mod replaces Bv2's execution sequence; it does not relitigate architecture.

---

## Northstar

> **Bonsai detects real faults in real labs using real telemetry, produces real detection events, the graph correctly captures the impact, an operator (or Claude Code session) can answer "what does this mean and what should I do about it" using the running system, and Path A graph embeddings compute against the populated real graph.**

That is MVP. Notably **not** part of MVP:
- The investigation agent running productively (deferred until token budget exists)
- Path B GNN (the destination after enough archive accumulates; not the immediate gate)
- Output adapters wired to real receivers beyond Prometheus (Splunk/Elastic/ServiceNow EM are post-MVP)
- HIL graduated remediation in production use
- Signals tier (syslog/traps)
- Controller adapter implementations

The north star — graph-native ML capability — is reachable from MVP in roughly 2 additional sprints once the archive accumulates. Total estimate from today: **5-6 sprints to MVP, 7-8 to north star.**

---

## Honest Accounting — What Has Been Operated vs Merely Coded

This is the framing the rest of Bv2-mod is built on. Reading code that compiles and tests that pass is not the same as "the system works."

### Coded but never operated (or barely operated)

- **NetBox enricher**: ~712 lines in `src/enrichment/netbox.rs`, 8 tests. The companion NetBox container has not been brought up; no real device data has flowed through.
- **ServiceNow CMDB enricher + EM push adapter**: ~899 lines combined in `src/enrichment/servicenow.rs` and `src/output/servicenow_em.rs`, 14 tests. The mock has not been engaged in operation; PDI credentials are not yet provided.
- **Splunk + Elastic output adapters**: ~968 lines combined, 15 tests. The companion containers have not been brought up; no real events have flowed.
- **DC + SP labs (`lab/dc/dc-evpn-srv6.clab.yml`, `lab/sp/sp-mpls-srte.clab.yml`)**: deployed once during sprint review, possibly. Not running continuously. ContainerLab device containers are heavyweight; sustained operation has not been tested.
- **Path A spectral embeddings**: 185 lines, 8 tests against synthetic mocks. Has never been computed against a real bonsai graph.
- **Investigation agent**: 522 lines, 12 tests. Has never investigated a real fault. Cannot run without Anthropic API token budget.
- **Chaos harness firing the fault catalogue**: scripted; not yet running on a continuous cycle producing real matrix output.
- **Always-on feedback runner**: not yet built; conceptual in Bv2.
- **Claude Code / Codex consumption of `/api/_test/status`**: documented in `docs/ai_feedback_protocol.md`, but the workflow has not been exercised by a real Claude Code session against a real running bonsai.

### Operated and verified

- **Core + collector distributed mode** with mTLS — Sprint 4 testing results captured this.
- **Path catalogue plugin loader, Environment graph entity, first-run wizard** — exercised during prior sprint reviews.
- **Memory architecture post-v12 fixes** (F-1 buffer pool cap, F-3 LRU eviction) — empirical measurements documented.
- **Audit subsystem** with credential resolve audit trail — exercised in unit tests + observed during operations.
- **Graph schema (41 tables) creation** — DDL runs cleanly; verified in operations.
- **Multi-hop graph queries on the synthetic test fixtures** — 38 tests pass.

### The gap

**Six months of enrichment + adapter code lives in main without ever having been run end-to-end.** This is the gap between "coded" and "MVP-ready." Closing it is what Sprint 1 of Bv2-mod is about.

---

## Sprint Plan — Operate-First Sequence

### Sprint 1 — Bring up the world; capture what's actually broken (1-2 weeks)

**Goal**: get the running system to a state where the next sprint's work has real ground truth to push against. This is *not* a sprint of new feature code; it is a sprint of operating the existing code and fixing only what real operation reveals as broken.

**Concrete tasks**:

1. **Stand up the always-on stack**:
   - `lab/dc/Makefile` up — DC EVPN-SRv6 lab running, all 8 NOSes booted, all configs loaded, all BGP/IS-IS/EVPN sessions established
   - `lab/sp/Makefile` up — SP MPLS-SRTE lab running, all 9 NOSes booted, all sessions established
   - `docker compose -f docker/compose-external.yml --profile all up -d` — NetBox + Splunk + Elastic + Prometheus all up with `restart: unless-stopped`
   - `docker compose --profile two-collector up -d` — bonsai-core + 2 collectors against the labs
   - Run `scripts/seed_external.sh` — NetBox populated with topology matching the labs

2. **Seed and verify enrichment**:
   - Configure NetBox enricher in Enrichment workspace
   - Click "Test connection" → verify success
   - Click "Run now" → verify enrichment populates the graph
   - **Capture every failure that surfaces in `docs/test_results/sprint1_operation/`**

3. **Verify detection path**:
   - Inject one fault from `lab/fault_catalog.yaml` (e.g. `bgp-session-down-leaf1-spine1`)
   - Watch SSE on `/api/events` for detection event
   - Watch `/api/incidents` for incident grouping
   - Verify UI Incidents tab populates (no longer empty)
   - **Capture every failure in `docs/test_results/sprint1_operation/`**

4. **Engage Claude Code / Codex as the diagnostic agent**:
   - Start a Claude Code session in the repo
   - Point it at `/api/_test/status` and `runtime/driver_results/`
   - For each captured failure, the session reads the structured signal, identifies the failing component, proposes a targeted fix
   - Fixes land as small targeted PRs; each PR's PR-comment captures what the feedback loop surfaced

5. **Run the four drivers manually** (continuous-running infrastructure is Sprint 2):
   - `tests/api_driver/run.py` — capture which endpoints return empty when populated, which return errors
   - `tests/event_driver/run.py` — capture which events fire, which don't
   - `tests/ui_driver/` Playwright suite — capture which UI workspaces show stale data, which screenshots regress
   - `tests/chaos_harness/run.py` — capture which faults produce the expected detection within the expected window

6. **Document the operational reality** — `docs/test_results/sprint1_operation/state-of-the-system-<date>.md` is the deliverable. It captures: what works, what's broken, what failure mode each broken component produces, what's been fixed in this sprint, what carries forward.

**Sprint 1 success criterion**: at the end of the sprint, an operator (or Claude Code session) can answer the question "is bonsai working in our lab right now?" with evidence rather than speculation.

**Why this is Sprint 1**: every other sprint depends on this. Hardcoding cleanup is meaningless if the code path with the hardcoding isn't being exercised. Always-on feedback infrastructure is meaningless without a baseline of what "working" looks like. GNN training requires archive accumulation that only starts when the lab runs continuously.

### Sprint 2 — Targeted fixes from Sprint 1 plus continuous-running infrastructure (1-2 weeks)

**Goal**: convert Sprint 1's discoveries into targeted fixes; engage continuous running so the archive accumulates and Sprint 3+ has stable ground truth.

**Concrete tasks**:

1. **Land the targeted fixes** that Sprint 1 surfaced — likely subset of Bv2's H-1 through H-12 list, but **only the ones that real operation flagged as broken**. Specifically expected to surface:
   - H-1 (DC-centric tier vocabulary) — surfaces the moment SP topology runs through `subscription_health_by_tier`
   - H-9 (sanitiser false positives) — surfaces when a real Cypher query against real data trips the substring matcher
   - H-7 (site_dependency hop limit) — surfaces if any DC-DC cross-site query returns truncated results

   **Hardcoding fixes for code paths that Sprint 1 didn't exercise are deferred.** Specifically: H-2, H-3, H-4 (agent pricing/model/budget) defer because the agent isn't being run; H-12 (Decimal cost) defers for the same reason; H-5 (HTTPS gate in embeddings CLI) defers if Sprint 1 didn't run embeddings.

2. **Always-on feedback runner** (Bv2 T1-1):
   - `scripts/feedback_runner.sh` long-lived process
   - Drivers on a 5-minute cycle (api/event/ui), 15-minute cycle (chaos)
   - Output continuously to `runtime/driver_results/`

3. **Lab persistence verification**:
   - `restart: unless-stopped` on every external service confirmed
   - Lab Makefiles produce idempotent up/down/reset
   - One reboot of the laptop recovers cleanly without manual intervention
   - Archive directory survives reboots

4. **Baseline rotation** (Bv2 T1-5):
   - When all drivers green for 24 hours, rotate `runtime/baseline_status.json`
   - Old baselines kept 30 days for Claude Code regression analysis

5. **Document the Claude Code / Codex consumption pattern** (revising Bv2 T1-7):
   - The pattern is *not* an API-consuming agent. It's a documentation surface that explains, to a Claude Code or Codex session running on the dev's machine, which files to read first when invoked.
   - Concrete: `CLAUDE.md` gains a section "Diagnosing what's broken" pointing at `/api/_test/status`, `runtime/driver_results/`, the baseline diff procedure
   - Three worked examples in `docs/ai_feedback_examples.md` showing Claude Code / Codex sessions diagnosing real regressions surfaced in Sprint 1

**Sprint 2 success criterion**: the always-on stack runs unattended for 72 hours; archive accumulates; drivers run continuously; Claude Code / Codex sessions can read structured signal and propose targeted fixes without burning Anthropic API tokens on the application side.

### Sprint 3 — Enrichment running on the real graph + Path A against real data (2 weeks)

**Goal**: real enrichment data flows on the graph; Path A embeddings compute against the populated real graph; the graph-native value extraction tier earns its keep.

**Concrete tasks**:

1. **NetBox enricher productive**:
   - Scheduled runs via the enrichment registry (not manual)
   - Idempotency verified across multiple runs
   - VLAN, Prefix, Application, namespaced `netbox_*` properties land on real Device nodes
   - DeviceDrawer shows enrichment-sourced properties distinguishably
   - Enrichment workspace shows last-run summary with non-zero `nodes_touched` and `edges_created`

2. **ServiceNow enricher against the mock** (not PDI; PDI is post-MVP unless credentials arrive):
   - Mock seeded with topology matching the lab
   - Enricher runs; Application + RUNS_SERVICE + CARRIES_APPLICATION edges land
   - Verified via Cypher query in the Explorer

3. **Path A spectral embeddings against the real graph**:
   - `python -m bonsai_ml.embeddings --base-url http://localhost:3000 --dim 16` against the populated real graph
   - Embeddings posted to `/api/graph/embeddings/upsert`
   - Device nodes carry `embedding` properties retrievable via Explorer
   - **Critical milestone**: this is the first time bonsai's ML layer has touched a real graph

4. **Path A model card** (Bv1 T3-1 carryover):
   - Algorithm, hyperparameters, dataset (the populated real graph from Sprint 2-3 operation)
   - At least one evaluation: do the embeddings cluster meaningfully? Do leaf devices cluster separately from spines? Do PEs separate from P routers?
   - Limitations explicit

5. **Graph algorithm validation against the real topology**:
   - `device_centrality` on the real DC + SP labs — do spines and core PEs surface as high-centrality?
   - `site_dependency_depth` — do sites with dense cross-connection surface correctly?
   - `subscription_health_by_tier` post-H-1-fix — do SP devices get correct labels?
   - `co_firing_detections` — what actually correlates over the 2 weeks of operation?

**Sprint 3 success criterion**: a Cypher query in the Explorer answers a real operational question using real enriched data; embeddings exist on real Device nodes; one new graph algorithm test against real-graph fixtures (not synthetic).

### Sprint 4 — UI completion + chaos archive deepens (1-2 weeks)

**Goal**: fix the UI gaps real operation flagged; let the chaos archive deepen ahead of GNN training.

**Concrete tasks**:

1. **UI completion items the feedback loop flagged**:
   - Likely candidates: operator path overrides workspace (Bv1 T6-1), subscription resolution audit (Bv1 T6-2), Investigations workspace polish, cost dashboard surfacing (when agent eventually runs)
   - **Specific items chosen by what Sprint 1-3 operation flagged as confusing or stale**

2. **Chaos archive deepens passively** (no action required other than keep the always-on stack running)

3. **Detection rule tuning** based on what fired in Sprint 1-3:
   - Rules that fired excessively → tightened
   - Rules that didn't fire when they should → loosened or fixed
   - Rules whose outcomes match `docs/test_results/chaos_matrix/` patterns → graduate from `SuggestOnly` to `ApproveEach` trust state

4. **Mutation testing on critical modules** (Bv1 T5-3 carryover):
   - cargo-mutants on `credentials.rs`, `audit.rs`, `remediation/trust.rs`, `assignment.rs`, `graph/queries.rs`, `graph/algorithms.rs`
   - Mutation score ≥80%

5. **Path profile validation against the real labs**:
   - Each path profile in the catalogue exercised against a matching device in the lab
   - `subscribed_but_silent` resolved; subscriptions either receive data or are correctly flagged

**Sprint 4 success criterion**: UI shows correct ground truth in every workspace exercised; chaos archive has 14+ days of accumulated data; detection rules have been tuned at least once based on real firing patterns.

### MVP gate (between Sprint 4 and Sprint 5)

By the end of Sprint 4, MVP definition is met:
- Real lab; real telemetry; real detection events; correct graph state; populated UI; embeddings on real nodes; Claude Code / Codex sessions productive against the system.

This is the moment to assess: **is the system genuinely usable**? If yes, proceed to GNN sprint. If no, Sprint 5 is another stabilisation pass.

### Sprint 5 — Path B GNN training (3-4 weeks)

**Goal**: train and deploy the Path B GNN against the accumulated chaos archive.

**Pre-requisites** (verified before sprint starts):
- 30+ days of accumulated chaos archive in the always-on lab
- Path A embeddings stable on Device nodes
- Detection rules tuned with known true-positive / false-positive baselines
- Graph queries exercise real data without surprises

**Concrete tasks**:

1. **Build the data loader** (Bv1 T2-3):
   - `python/bonsai_ml/gnn/data_loader.py`
   - Reads from the chaos archive
   - Handles all enrichment property types via schema registry
   - Produces PyTorch Geometric `Data` objects

2. **Train the GNN** (Bv1 T2-2):
   - `python/bonsai_ml/gnn/train.py`
   - Architecture: GraphSAGE or GAT with 2-3 layers
   - Task: node-level anomaly score for Device nodes
   - Train on 25 days; validate on 5 days; test on the most recent day's chaos faults
   - Compare against rule-based baseline + tabular ML baseline
   - Produce confusion matrix: detected-by-GNN-only / detected-by-rules-only / detected-by-both

3. **Online inference path** (Bv1 T2-4):
   - GNN scoring on graph snapshot every N seconds
   - Detection events get `gnn_anomaly_score` field
   - UI surfaces it where relevant

4. **Model card** documenting algorithm, hyperparameters, dataset, evaluation, limitations explicit

**Sprint 5 success criterion**: GNN catches at least one cascading-failure class that rules + tabular ML miss, with documented confusion matrix on held-out chaos test set; model deployed; UI shows GNN scores alongside rule-based detections.

### Sprint 6+ — Post-north-star

After Sprint 5, the project hits its stated north star. Subsequent work is genuine extension:

- **Investigation agent in production** — when token budget allocated; takes 1-2 sprints to mature against real investigations
- **PDI live tests** — when credentials provided
- **Output adapter productive use** — Splunk/Elastic/ServiceNow EM running against real receivers
- **Signals tier** (syslog + traps)
- **Controller adapters** (demand-driven)
- **Strategic carryover** items from Bv2 Tier 6

These are valuable but not part of the immediate MVP-to-north-star arc.

---

## What Bv2-mod Defers from Bv2

The hardcoding catalogue (H-1 through H-12) and the Tier 1 always-on infrastructure from Bv2 are *folded into* Bv2-mod's sprints rather than being the headline tiers themselves. Specifically:

- **H-1 (tier vocabulary)**: Sprint 2, expected to surface in Sprint 1 operation
- **H-2, H-3, H-4 (agent config)**: deferred to post-MVP since agent isn't run
- **H-5 (HTTPS gate)**: Sprint 2 if embeddings ran in Sprint 1; defer otherwise
- **H-6, H-7, H-8 (graph algorithm parameterisation)**: Sprint 2 for the ones Sprint 1 surfaced; defer the rest
- **H-9 (sanitiser false positives)**: Sprint 2, expected to surface in Sprint 1 explorer use
- **H-10, H-11, H-12 (low priority)**: opportunistic; not gating
- **Always-on feedback runner**: Sprint 2 (was Bv2 Tier 1)
- **Investigation agent maturity (Bv2 Tier 4)**: post-MVP
- **GNN (Bv2 Tier 3)**: Sprint 5 (was Bv2 Sprint 4)

Nothing is dropped. Items reorder around what's gating MVP.

---

## What Bv2-mod Carries Unchanged from Bv2

- All architectural invariants and guardrails (gNMI-only hot path, controller-less primary audience, vault-only credentials with purpose-tagged audit, OutputAdapter read-only on bus, HIL graduated, environment awareness first-class, path catalogue is data, no LLM in detect-heal, no LLM on device config)
- The Bv2 hardcoding inventory (H-1 through H-12) as the reference document for what to fix when the corresponding code path runs
- The Bv2 anti-patterns
- Documentation refresh as the lowest priority (Bv1 Tier 8, Bv2 Tier 7)

---

## Estimated Timeline

| Milestone | Sprints | Cumulative weeks |
|---|---|---|
| Sprint 1: operate-first | 1-2 | 1-2 |
| Sprint 2: targeted fixes + continuous | 1-2 | 2-4 |
| Sprint 3: enrichment + Path A | 2 | 4-6 |
| Sprint 4: UI + tuning | 1-2 | 5-8 |
| **MVP gate** | | **5-8 weeks** |
| Sprint 5: GNN | 3-4 | 8-12 |
| **North-star milestone** | | **8-12 weeks** |
| Post-north-star | ongoing | 12+ |

The critical insight: **the chaos archive accumulates passively while sprints execute**, so by the time Sprint 5 starts, 30+ days of archive are available. The dependency is met by elapsed wall-clock time, not by additional sprint effort.

---

## Risks and Mitigations

**Risk 1**: Sprint 1 surfaces a bug so deep it consumes the whole sprint just stabilising the lab.
*Mitigation*: Sprint 1 is explicitly time-boxed at 2 weeks; if not done, Sprint 2 absorbs continuation. The principle "operate, don't build" stays constant.

**Risk 2**: NetBox enricher fails repeatedly against the real container in ways the mock didn't catch.
*Mitigation*: this is exactly what Sprint 1 is for. Failures are the deliverable. Claude Code session diagnoses; small targeted PRs fix; archive grows.

**Risk 3**: Lab containers consume too much laptop resource for sustained running.
*Mitigation*: Bv1 already sized lab containers conservatively (SR Linux, FRR, cRPD). If sustained running is infeasible, downsize to a smaller subset (e.g. DC topology only, 4 devices) and accept the smaller archive.

**Risk 4**: Token budget never appears for the investigation agent.
*Mitigation*: agent stays post-MVP. The MVP definition explicitly excludes it. North star is reachable without it.

**Risk 5**: After 30 days of operation, the chaos archive contains too few interesting events to train a GNN.
*Mitigation*: Sprint 4 includes detection rule tuning specifically to ensure the archive contains diverse signal. If the archive is genuinely thin, Sprint 5 starts with archive-augmentation (additional fault catalogue entries) before training.

---

## What Success Looks Like

By **Sprint 4 completion** (5-8 weeks from now):

A new Claude Code session walks into the repo, asks "is bonsai working in our lab?", reads `/api/_test/status`, reads the latest `docs/test_results/chaos_matrix/<date>.md`, and answers definitively. The session then asks "what's the impact of this latest detection on leaf1?", queries the graph via the Explorer endpoint, gets a blast-radius response with affected applications, and reports back with evidence.

By **Sprint 5 completion** (8-12 weeks):

The same session asks "did the GNN catch anything the rules missed in the last week?", queries `/api/detections`, filters by `gnn_anomaly_score > threshold AND no matching rule_id`, gets a list with concrete examples. The session can read the model card, understand the GNN's known limitations, and reason about whether the new detections are likely false positives or real catches.

That's the project's promised value, demonstrably running.

---

## Operator Action Items (Outside the Sprints)

Things that would accelerate the timeline if available:

1. **ServiceNow PDI credentials** — unlocks T5-1 PDI live tests. Currently parked behind operator availability.
2. **Anthropic API token budget for the investigation agent** — unlocks the agent in production. Currently parked behind allocation.
3. **A second machine running the always-on lab** — would let development continue on the laptop while archive accumulates undisturbed. Optional; not gating.

---

*Bv2-mod (modified) — authored 2026-05-04 as a structural pivot in execution sequence. Replaces Bv2's prep-first sprint plan with operate-first sequence: Sprint 1 brings up the world and captures real failure modes; Sprint 2 lands targeted fixes and engages continuous running; Sprint 3 makes enrichment and Path A productive on real data; Sprint 4 closes UI gaps and tunes detection; Sprint 5 trains the GNN against the accumulated archive. MVP at Sprint 4; north star (graph + GNN working) at Sprint 5. Investigation agent deferred to post-MVP pending token budget. Claude Code / Codex (subscription-billed) replaces API-consuming sessions as the diagnostic agent. Hardcoding fixes from Bv2 fold into sprints based on what real operation surfaces, not as a standalone tier. Strategic invariants from v7-Bv2 carry unchanged. Estimated 5-8 weeks to MVP, 8-12 weeks to north-star.*
