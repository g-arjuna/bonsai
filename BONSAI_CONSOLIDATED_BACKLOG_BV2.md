# BONSAI — Backlog Bravo Series, v2 (Bv2.0)

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_BV1.md`. Produced 2026-05-04 after chunk-by-chunk review of the Bv1 first-sprint code land.
>
> **Bv1 progress is substantial.** The graph-native value extraction tier (the entire reason for the Bravo series reset) has landed in code. Multi-hop queries, blast radius, graph algorithms, the explorer UI with sanitiser, saved queries, test fixtures, spectral embeddings, the investigation agent with cost controls and tool surface, and the Investigations + Explorer UI workspaces are all in main. F-4 binary self-containment also substantially landed — `LBUG_SHARED=1` is commented out in `.cargo/config.toml`; static build is now the default. **This is the most consequential single landing the project has had.**
>
> **Two things to do now**:
>
> 1. **Hunt hardcoding that would break in real deployments.** The user explicitly asked. Twelve specific items found in chunked review (H-1 through H-12 below). Several are real correctness issues — the DC-centric tier vocabulary in `algorithms.rs::subscription_health_by_tier` mislabels SP and campus topologies; the agent's pricing constants are already stale relative to the model it uses.
>
> 2. **Engage the iterative feedback loop continuously.** Infrastructure is mature: four drivers writing structured JSON to `runtime/driver_results/`, unified `/api/_test/status` endpoint, protocol doc at 207 lines, CI workflow on PR + push. **The conditions for continuous running are met.** Bv2 documents the operational plan to engage it as the primary feedback signal for ongoing work.
>
> **Document discipline**: prior backlogs (v2-v12, Bv1) remain in repo. Strategic positioning, audience framing, controller-less primary target, gNMI-only hot path, enrichment philosophy, AIOps-feeder framing, HIL graduated remediation, OutputAdapter architecture all unchanged and referenced rather than restated. Bv2 spends real estate on the two threads above plus carry-forward of remaining Bv1 items that did not land in Sprint 1.

---

## Table of Contents

1. [Audience and Positioning](#positioning) — see v7
2. [Bv1 Sprint Progress — Verified](#progress)
3. [Hardcoding Findings (H-1 through H-12)](#hardcoding) — read this first
4. [TIER 0 — Hardcoding Cleanup](#tier-0)
5. [TIER 1 — Engage the Iterative Feedback Loop Continuously](#tier-1) ⚡ START NOW ⚡
6. [TIER 2 — Carryover from Bv1 Tier 1 (graph foundation completion)](#tier-2)
7. [TIER 3 — Carryover from Bv1 Tier 2 (Path A polish, Path B GNN)](#tier-3)
8. [TIER 4 — Carryover from Bv1 Tier 3 (Investigation Agent maturity)](#tier-4)
9. [TIER 5 — Test Coverage and Operational Hardening](#tier-5)
10. [TIER 6 — Carryover from Bv1 Tier 4-7 (binary, UI, strategic)](#tier-6)
11. [TIER 7 — Documentation Refresh (lowest priority, unchanged)](#tier-7)
12. [Execution Order](#execution-order)
13. [Guardrails](#guardrails)

---

## <a id="positioning"></a>Audience and Positioning

**Unchanged from v7-Bv1.** See `BONSAI_CONSOLIDATED_BACKLOG_V7.md` for the full rationale. Controller-less primary audience across DC, campus, SP. AIOps integration as feeder, not replacement.

---

## <a id="progress"></a>Bv1 Sprint Progress — Verified Against Main

End-to-end code review confirms Sprints 1+2+3+4 of Bv1 substantially landed. ~6,700 new Rust lines + ~900 new UI lines + ~700 new Python lines + 38 tests in new graph modules.

| Bv1 item | Status | Evidence |
|---|---|---|
| T1-1 multi-hop pattern queries | ✅ Done | `src/graph/queries.rs` (858 lines) with 13 production queries + 17 tests: `neighbors_of_device`, `shortest_topology_path`, `blast_radius`, `devices_in_environment`, `detections_in_environment`, `applications_on_site`, `devices_missing_enrichment`, `orphan_devices`, `detections_without_remediation`, `subscription_health_for_device`, `co_firing_detections`, `device_enrichment_context`, `topology_edges` |
| T1-2 in-DB shortest path | ⚠️ Partial | `shortest_topology_path` exists but does Rust-side BFS calling `neighbors_of_device` per frontier rather than a single in-DB Cypher shortestPath call. **lbug 0.15.3 caps variable-length traversal upper bound at 30 hops** (documented in code at queries.rs:282), which justifies the multi-call approach. Improvement vs. v12 (was loading all edges into Vec); not the spec target. Carry into Bv2 as accepted compromise OR T2-1 below if revisiting. |
| T1-3 blast radius traversal endpoint | ✅ Done | `queries.rs::blast_radius` + `/api/blast-radius/:device_address` route |
| T1-4 graph algorithms | ✅ Done | `src/graph/algorithms.rs` (365 lines) with 5 algorithms + 8 tests: `device_centrality`, `site_dependency_depth`, `detection_correlation`, `subscription_health_by_tier`, `graph_insights` |
| T1-5 graph explorer UI workspace | ✅ Done | `src/graph/explorer.rs` (263 lines, sanitiser + 13 tests) + `ui/src/routes/Explorer.svelte` (630 lines) + `/api/explorer/query` endpoint |
| T1-6 saved queries | ✅ Done | `/api/explorer/saved-queries` GET/POST, `/api/explorer/saved-queries/:id/delete` |
| T1-7 graph query test framework | ✅ Done | `src/graph/test_fixtures.rs` (506 lines) — 2-spine/4-leaf DC + SP pair + isolated, applications, detections, enrichment, subscription status |
| T2-1 graph embeddings | ✅ Done | `python/bonsai_ml/embeddings.py` (185 lines) — sklearn SpectralEmbedding, posts to `/api/graph/embeddings/upsert` |
| T3-1 agent scaffolding with graph-aware tools | ✅ Done | `python/bonsai_agent/agent.py` (166 lines) — Anthropic ReAct loop, tool surface in `tools.py` (240 lines): `get_blast_radius`, `query_graph`, `get_recent_detections`, `get_remediation_history`, `summarise`, `propose_playbook` |
| T3-2 agent UI workspace | ✅ Done | `ui/src/routes/Investigations.svelte` (289 lines) + 4 endpoints under `/api/investigations/` |
| T3-3 agent cost controls | ✅ Done | `python/bonsai_agent/budget.py` (116 lines) — fail-closed per-investigation + daily limits |
| F-4 v12 carryover (binary self-containment) | ✅ Substantially done | `.cargo/config.toml` line 8: `LBUG_SHARED = "1"` is commented out. Static build is the default. Verify with `ldd target/release/bonsai` post-build to confirm zero shared lib deps. |

**Not yet done (carry forward to Bv2)**:
- T1-2 single-Cypher shortestPath (blocked by lbug 0.15.3 cap; documented as accepted compromise)
- T2-2 Path B GNN (Sprint 5 in Bv1 plan)
- T3-2 v9 agent UI workspace polish — basic UI landed; test feedback loop will surface gaps
- T3-3 cost dashboard surfacing in UI
- T3-4 agent memory across investigations
- T4-1 static-link lbug verification step (CI assertion)
- All Bv1 Tier 5 (test coverage gaps) — partial; PDI live tests still pending operator inputs
- All Bv1 Tier 6 (UI completion — operator path overrides workspace, subscription resolution audit)
- All Bv1 Tier 7 (strategic carryover — signals, controller adapters, NL query, etc.)
- Bv1 Tier 8 (documentation refresh)

---

## <a id="hardcoding"></a>Hardcoding Findings — H-1 through H-12

The user explicitly asked to be the judge of hardcoding that would break in real deployments. Each finding has location, evidence, blast-radius assessment, and concrete fix.

### H-1 — DC-centric tier vocabulary in `subscription_health_by_tier`

**Severity: HIGH — real correctness issue**

**Location**: `src/graph/algorithms.rs:162-238`

**Evidence**:
```rust
/// Subscription health grouped by topology tier.
/// Tier is derived from undirected degree: spine (≥4), aggregation (2–3), leaf (1), isolated (0).
pub fn subscription_health_by_tier(conn: &Connection<'_>) -> Result<Vec<TierHealthRow>> {
```

**The problem**: tier vocabulary is hardcoded for DC fabrics. In an SP backbone, a P router has degree 4-8 but is not a "spine"; a CE-facing PE has degree 1-2 but is not a "leaf"; an RR has degree 0 (logical-only) but is not "isolated." In a campus topology, a distribution switch has degree 2-3 but is not "aggregation" in the DC sense. The function will return misleading labels in any non-DC environment.

This is **the only real correctness issue** in the hardcoding inventory; the others are config-not-baked-in concerns.

**Fix**: tier labels parameterised by environment archetype. The function signature gains an `archetype: &str` argument; tier-vocabulary maps live in config keyed by archetype:

```toml
[graph.tier_vocabulary.data_center]
spine = { min_degree = 4 }
aggregation = { min_degree = 2, max_degree = 3 }
leaf = { degree = 1 }
isolated = { degree = 0 }

[graph.tier_vocabulary.service_provider]
core = { min_degree = 6 }
distribution = { min_degree = 3, max_degree = 5 }
edge = { min_degree = 1, max_degree = 2 }
isolated = { degree = 0 }

[graph.tier_vocabulary.campus_wired]
core = { min_degree = 4 }
distribution = { min_degree = 2, max_degree = 3 }
access = { degree = 1 }
isolated = { degree = 0 }
```

When called without archetype, function returns a generic degree-band label (low/medium/high/isolated) not a fabricated DC term.

**Where**: `src/graph/algorithms.rs`, `src/config.rs`, plus UI surface in Operations workspace to show archetype-correct labels.

### H-2 — Pricing constants in `bonsai_agent/budget.py` already stale

**Severity: MEDIUM — wrong cost numbers in operator-visible reports**

**Location**: `python/bonsai_agent/budget.py:23-26`

**Evidence**:
```python
# Approximate Anthropic pricing (claude-3-5-haiku) — for cost estimation only.
# Update if model or pricing changes.
_INPUT_COST_PER_M  = 0.80   # USD per 1M input tokens
_OUTPUT_COST_PER_M = 4.00   # USD per 1M output tokens
```

vs. `python/bonsai_agent/agent.py:25`:
```python
_MODEL = "claude-haiku-4-5-20251001"   # cost-efficient for tool-use loops
```

**The problem**: pricing is hardcoded for `claude-3-5-haiku` while the agent uses `claude-haiku-4-5`. Cost estimation is wrong from day one. Operators reading the cost dashboard will trust a wrong number.

**Fix**: pricing as config keyed by model identifier. Agent.py reads pricing from `[agent.pricing.<model>]` table in bonsai.toml; budget.py looks up cost using the model the agent is configured with. Default config ships with current Anthropic public prices; operator can override for Bedrock/Vertex/contracted-rate deployments.

```toml
[agent]
model = "claude-haiku-4-5-20251001"

[agent.pricing.claude-haiku-4-5-20251001]
input_per_m_usd = 1.00
output_per_m_usd = 5.00
```

**Where**: `python/bonsai_agent/agent.py`, `budget.py`, `src/config.rs` (if read on the Rust side too), `bonsai.toml.example`.

### H-3 — Agent model and max-turns hardcoded

**Severity: MEDIUM — blocks compliance and per-environment customisation**

**Location**: `python/bonsai_agent/agent.py:25-26`

**Evidence**:
```python
_MODEL = "claude-haiku-4-5-20251001"   # cost-efficient for tool-use loops
_MAX_TURNS = 12                         # hard cap on tool-call rounds
```

**The problem**: operators with stricter compliance may need to:
- Pin a specific older model version for reproducibility
- Use Anthropic via Bedrock or Vertex (different model strings)
- Cap turns lower for cost reasons or higher for complex investigations

None of this is currently config.

**Fix**: `[agent]` section in bonsai.toml exposes `model`, `max_turns`, `system_prompt_override`. Agent reads at startup. Bedrock/Vertex deployments choose at config time.

**Where**: `python/bonsai_agent/agent.py`, `bonsai.toml.example`, `python/bonsai_sdk/client.py` if config is fetched from core.

### H-4 — Agent token budget defaults not exposed as config

**Severity: LOW — defaults are fine; absence of config blocks operators from changing them**

**Location**: `python/bonsai_agent/budget.py:20-21`

**Evidence**:
```python
DEFAULT_PER_INVESTIGATION = 50_000
DEFAULT_DAILY = 500_000
```

**The problem**: 50K per investigation and 500K daily are reasonable starting points but operators with high incident volume or low budget tolerance need to tune them. No `[agent.budget]` config section exists.

**Fix**: `[agent.budget]` config keys `per_investigation_tokens` and `daily_tokens`. Budget reads at init. Defaults remain the current values.

**Where**: `python/bonsai_agent/budget.py`, `bonsai.toml.example`.

### H-5 — Embeddings CLI defaults to plain HTTP without HTTPS warning

**Severity: MEDIUM — silent insecure operation in non-localhost deployments**

**Location**: `python/bonsai_ml/embeddings.py:166`

**Evidence**:
```python
parser.add_argument("--base-url", default="http://127.0.0.1:3000")
```

**The problem**: localhost default is fine, but the CLI accepts any URL including non-localhost plain HTTP. In any real deployment the bonsai HTTP API is over TLS. The CLI silently sends embeddings (which include node identities and graph structure) over the wire unencrypted with no warning.

**Fix**: emit a loud warning when `--base-url` is non-localhost AND scheme is `http://`. In strict mode (env var `BONSAI_REQUIRE_HTTPS=1`) refuse to connect over plain HTTP except to localhost.

**Where**: `python/bonsai_ml/embeddings.py`, `python/bonsai_sdk/client.py` (apply globally — same pattern likely needed in agent's HTTP calls to the bonsai core).

### H-6 — BFS depth hardcoded to 30 in `shortest_topology_path`

**Severity: LOW — driven by lbug 0.15.3 limitation; documented**

**Location**: `src/graph/queries.rs:208`

**Evidence**:
```rust
'bfs: for _ in 0..30 {
```

**The problem**: BFS depth for path computation is hardcoded to 30 hops. lbug 0.15.3 caps variable-length traversal upper bound at 30 (the comment at line 282 documents this), so even if we wanted a deeper search the DB couldn't do it in one query. **Fine for DC and most SP topologies**; could be insufficient for very deep hierarchies (multi-region SP backbones with transit, hyperscale fabrics with super-pods).

**Fix**: extract to `[graph.path.max_bfs_depth]` config, default 30. Document the lbug 0.15.3 cap in the config comment so operators know when to revisit. When lbug supports deeper traversal, lift the default.

**Where**: `src/graph/queries.rs`, `src/config.rs`.

### H-7 — `site_dependency_depth` hop limit too low for 3-tier DC

**Severity: MEDIUM — mislabels deep hierarchies**

**Location**: `src/graph/algorithms.rs:110`

**Evidence**:
```cypher
MATCH (d)-[:HAS_INTERFACE|CONNECTED_TO*1..6]-(n:Device)
```

**The problem**: 6 graph edges = 2 physical hops (each physical hop is `HAS_INTERFACE` + `CONNECTED_TO` + `HAS_INTERFACE` = 3 graph edges). For a 3-tier DC fabric (leaf → spine → super-spine = 3 physical hops = 9 graph edges) the cross-site reachability count is silently truncated.

**Fix**: parameterise via `[graph.algorithms.site_dependency_max_physical_hops]` config (default 4 = 12 graph edges, sufficient for 4-tier hierarchies). Consider exposing per-archetype defaults.

**Where**: `src/graph/algorithms.rs`, `src/config.rs`.

### H-8 — Detection correlation `LIMIT 50` hardcoded

**Severity: LOW — caps result set, may surface late as operators scale**

**Location**: `src/graph/algorithms.rs:146`

**Evidence**:
```cypher
ORDER BY co_count DESC \
LIMIT 50
```

**The problem**: 50 is a reasonable default for surfacing top correlations in the UI. For an operator with 200+ active rules, the top 50 may miss real correlations buried in the long tail.

**Fix**: parameterise via `[graph.algorithms.correlation_limit]` config or allow caller to pass a limit. Default 50 stays.

**Where**: `src/graph/algorithms.rs`.

### H-9 — Explorer sanitiser produces false positives on string literals

**Severity: MEDIUM — operator-confusing false positives in the headline UI feature**

**Location**: `src/graph/explorer.rs:15-32`

**Evidence**:
```rust
const BANNED_KEYWORDS: &[&str] = &[
    "CREATE", "DELETE", "DROP", "MERGE", "REMOVE", "DETACH", "CALL", "SET",
];

pub fn validate_query(cypher: &str) -> Result<(), String> {
    let upper = cypher.to_uppercase();
    let upper_bytes = upper.as_bytes();
    for &kw in BANNED_KEYWORDS {
        if keyword_present(upper_bytes, kw.as_bytes()) {
            return Err(format!(...));
        }
    }
    Ok(())
}
```

**The problem 1**: substring word-boundary matching is fragile. Queries like `MATCH (n) WHERE n.name = "DELETE the past" RETURN n` are blocked even though `DELETE` is inside a string literal. Operators investigating an incident named "DELETE-test-fix" cannot query it.

**The problem 2**: `CALL` is banned. This blocks legitimate read-only Cypher introspection like `CALL show_tables()` (lbug/Kuzu introspection). Operators and AI agents can't introspect schema through the explorer.

**Fix**: two-stage approach.
- Stage 1 (cheap): strip string literals (`"..."`, `'...'`) and Cypher comments (`//...`, `/*...*/`) before scanning for banned keywords.
- Stage 2 (better): allow-list `CALL` for known read-only procedures (`show_tables`, `show_attached_databases`); reject `CALL` for unknown procedures.
- Long-term (Bv3+): replace substring scan with a real Cypher parser. lbug may have one; if not, an open-source Cypher AST library exists.

**Where**: `src/graph/explorer.rs`.

### H-10 — `Budget.check` called after `charge` increments — minor

**Severity: VERY LOW — fail-closed semantics work correctly; just flag**

**Location**: `python/bonsai_agent/agent.py` (the loop) + `budget.py:73`

**The problem**: order is `charge() → check()`. If a single tool call pushes total over the limit, the tokens are already spent before `check` raises. In practice, tool calls are small and the over-spend is bounded; the fail-closed semantics still prevent further spend. Just worth a code comment so future maintainers understand the order.

**Fix**: add a comment in `Budget.charge` documenting the order-of-operations and that a single tool call may push slightly over the limit before check raises.

**Where**: `python/bonsai_agent/budget.py`.

### H-11 — `_DEFAULT_GRAPH_BUFFER_POOL` derived from RAM still — verify cap floors

**Severity: LOW — verify rather than fix**

**Location**: `src/graph/mod.rs` (the v12 F-1 fix area)

The v12 fix capped LadybugDB buffer pool at `min(2 GB, 25% RAM)`. **Verify**:
- On a 4 GB VM (which is realistic for resource-sparse deployments), 25% = 1 GB; the cap is 1 GB, not 2 GB. Fine.
- On a 256 MB CI runner (which is unusual but possible), 25% = 64 MB; that's tight. There should be a floor (e.g. 128 MB minimum) so the DB has enough working memory.

**Fix**: add a documented floor: `max(min(2 GB, 25% RAM), 128 MB)`. Log the chosen value and the inputs (RAM detected, formula applied, floor activated yes/no).

**Where**: `src/graph/mod.rs::compute_default_buffer_pool` or wherever the formula lives.

### H-12 — `_INPUT_COST_PER_M = 0.80` per-million dollar precision is float

**Severity: VERY LOW — accumulated rounding in cost reports**

**Location**: `python/bonsai_agent/budget.py:25-26`

**The problem**: pricing as `float` accumulates rounding error. Over 1M token transactions the error is sub-cent; over 100M it's measurable. For a long-lived deployment that does enough investigations to spend $500/year, the report could be ±$5 wrong.

**Fix**: use `decimal.Decimal` for cost accumulation; coerce to float only at JSON serialization. Lower priority than H-1 through H-9.

**Where**: `python/bonsai_agent/budget.py`.

---

## <a id="tier-0"></a>TIER 0 — Hardcoding Cleanup

All H-1 through H-12 above. Sequenced by severity:

### T0-1 (Bv2) — Tier vocabulary parameterised by archetype (H-1)

**Highest priority hardcoding fix**. Real correctness issue. ~80 lines (config schema + algorithm refactor + tests). Verify with the Bv1 test fixture (which has both DC and SP devices) that SP devices get correct labels.

### T0-2 (Bv2) — Agent model + pricing as config (H-2 + H-3)

Pair the two together. `[agent]` section adds `model`, `max_turns`. `[agent.pricing.<model>]` table holds cost per million tokens. Agent reads model + pricing at startup. Default config ships current Haiku-4.5 values. ~40 lines.

### T0-3 (Bv2) — Agent budget config (H-4)

`[agent.budget]` exposes `per_investigation_tokens` + `daily_tokens`. Defaults stay 50K / 500K. ~10 lines.

### T0-4 (Bv2) — HTTPS gate in bonsai SDK (H-5)

Apply globally in `python/bonsai_sdk/client.py` so both the embeddings CLI and the agent inherit. Warn-or-refuse depending on env var. ~20 lines.

### T0-5 (Bv2) — Graph algorithm + query parameterisation (H-6 + H-7 + H-8)

Three small config exposures: `[graph.path.max_bfs_depth]`, `[graph.algorithms.site_dependency_max_physical_hops]`, `[graph.algorithms.correlation_limit]`. Functions read at call time. ~30 lines total.

### T0-6 (Bv2) — Explorer sanitiser improvements (H-9)

Two-stage: strip string literals + comments; allow-list known read-only `CALL` procedures. Test cases:
- `MATCH (n) WHERE n.name = "DELETE me" RETURN n` → allowed
- `CALL show_tables()` → allowed
- `CALL db.somethingDangerous()` → rejected
- `MATCH (n) /* DELETE */ RETURN n` → allowed

~50 lines + 6-8 new tests.

### T0-7 (Bv2) — Buffer pool floor (H-11)

Verify and add floor. ~5 lines + log line.

### T0-8 (Bv2) — Budget order-of-operations comment (H-10)

Documentation only. ~3 lines.

### T0-9 (Bv2) — Decimal cost accumulation (H-12)

Lowest priority. Defer if Sprint 1 is at capacity.

---

## <a id="tier-1"></a>TIER 1 — Engage the Iterative Feedback Loop Continuously

**Why this is Tier 1**: the user explicitly asked me to be the judge of when to start. **My judgement is: start now.**

The infrastructure has all five required ingredients:

1. **Drivers writing structured JSON** — `tests/api_driver/`, `tests/event_driver/`, `tests/ui_driver/`, `tests/chaos_harness/` all writing to `runtime/driver_results/<driver>.json`. Verified in code: each driver's `run.py` declares `--output runtime/driver_results/<driver>.json` as default; `playwright.config.js` writes `runtime/driver_results/ui.json`.

2. **Unified status endpoint** — `/api/_test/status` (`src/http_server.rs:1743`) reads driver results plus memory profile plus disk guard plus `runtime/external_status.json`. Single curl returns complete project health.

3. **Protocol documentation** — `docs/ai_feedback_protocol.md` (207 lines) and `docs/ai_feedback_examples.md` (195 lines). Format documented; consumption examples worked.

4. **CI workflow** — `.github/workflows/feedback-loop.yml` runs on PR + push. Already exercised on every change.

5. **Memory and binary stability** — v12 fixes (F-1 buffer pool cap, F-3 LRU eviction, F-9 startup logging) verified in place. F-4 binary self-containment substantially landed.

The remaining work is **operational**: make the feedback loop run continuously *outside* of PR gates so it produces a real signal stream.

### T1-1 (Bv2) — Always-on feedback runner

**What**: a long-lived process that runs the four drivers on a schedule against a continuously-running bonsai+lab+external-infra stack. NOT just on PRs.

**Operational shape**:
- Drivers run on a 5-minute cycle (api/event/ui) and 15-minute cycle (chaos)
- Output flows to `runtime/driver_results/` continuously
- Stack stays up indefinitely (lab, NetBox, Splunk, Elastic, Prometheus, bonsai-core, two collectors)
- `/api/_test/status` always returns fresh data
- Failures surface as Prometheus alerts plus structured Slack/email summaries (operator's choice)

**Why this matters operationally**: today the feedback loop runs only on PR, so AI sessions diagnosing live issues have no current signal. With always-on, an AI session asks "is bonsai working right now?" and gets a definitive answer.

**Where**: new `scripts/feedback_runner.sh` plus `docker/compose-feedback.yml` profile or a systemd unit pattern.

**Done when**: a single command brings up the always-on stack; the AI feedback protocol doc updated to reference the always-on endpoint as authoritative.

### T1-2 (Bv2) — Targeted code corrections from feedback signal

**What**: a documented operational pattern where AI sessions consume `/api/_test/status` deltas to identify regressions and propose targeted fixes.

**Workflow**:
1. AI session reads `/api/_test/status` at start of work
2. Compares to last-known-good (from `runtime/baseline_status.json`)
3. Notes which drivers regressed (api/event/ui/chaos)
4. Reads driver-specific result files for failure details
5. Proposes targeted fix scoped to the regression
6. After fix, re-runs drivers; updates baseline if green

**Where**: extension to `docs/ai_feedback_examples.md`. Three new worked examples:
- AI diagnoses "the api driver suddenly fails on /api/explorer/query after schema migration"
- AI diagnoses "the chaos matrix shows bgp_session_down detection latency increased from 12s to 45s"
- AI diagnoses "the ui driver screenshot diff shows the Operations workspace lost the memory sparkline"

**Done when**: doc shows three end-to-end examples of AI-driven targeted code corrections from feedback signal.

### T1-3 (Bv2) — Prometheus + Grafana feedback dashboard

**What**: a Grafana dashboard backed by Prometheus showing live feedback-loop signal:
- Driver pass rates over time (api/event/ui/chaos)
- Memory + disk trending
- Detection-firing latency by rule_id
- External infra health
- Investigation cost per day

**Where**: `docs/grafana/bonsai-feedback-loop.json`.

**Done when**: dashboard imports cleanly; operator sees regressions visually within 15 minutes of occurrence.

### T1-4 (Bv2) — Chaos harness on the always-on lab

**What**: chaos harness drives the full `lab/fault_catalog.yaml` against the always-on lab on a 4-hour cycle. Produces matrix of fault → detection → latency captured to `docs/test_results/chaos_matrix/<date>.md`.

**Where**: extends `tests/chaos_harness/run.py` with cycle scheduling.

**Done when**: every fault in the catalogue is exercised at least 6 times per day; matrix shows both pass rate and detection latency distribution.

### T1-5 (Bv2) — Automatic baseline rotation

**What**: when all drivers go green for 24 hours, automatically rotate `runtime/baseline_status.json`. Old baselines kept for 30 days for diff/regression analysis.

**Where**: `scripts/rotate_baseline.sh` cron job.

**Done when**: baseline auto-rotates; AI sessions see fresh baselines without manual intervention.

### T1-6 (Bv2) — Feedback-loop alerting

**What**: structured alerts when:
- Any driver red for >30 minutes
- Memory growth exceeds 50% RSS in any 1-hour window
- Detection latency p95 doubles vs. baseline
- External infra health degrades
- Investigation budget exceeded for the day

**Where**: Prometheus alert rules + alertmanager config in `docker/compose-feedback.yml`.

**Done when**: a deliberately-broken change produces a clear alert within 30 minutes of the next driver cycle.

### T1-7 (Bv2) — AI session prompt template for feedback-driven work

**What**: a documented prompt template AI sessions use when invoked for "fix what's broken" work:

```
Read /api/_test/status. Compare to last baseline. List regressions
in priority order (chaos > api > event > ui). For top regression,
read the corresponding driver result file from runtime/driver_results/.
Propose a single targeted fix. Do not propose feature work.
```

**Where**: `docs/ai_feedback_protocol.md` extension.

**Done when**: template is concrete enough that a fresh AI session, given only that prompt and access to the codebase, produces a useful targeted-fix PR.

---

## <a id="tier-2"></a>TIER 2 — Carryover from Bv1 Tier 1 (Graph Foundation Completion)

Most Bv1 Tier 1 landed. What remains:

### T2-1 (Bv2) — Single-Cypher shortest path (Bv1 T1-2 carryover)

**What**: when lbug 0.15.3 limits are revisited (or lbug version bumped to one supporting deeper traversal), revisit `shortest_topology_path` to use `MATCH p = shortestPath(...)` in a single in-DB call.

**Pre-requisite**: lbug supports deeper variable-length traversal OR the single-Cypher version performs comparably to the current Rust BFS at the typical depths we use (≤6 physical hops).

**Done when**: benchmark shows the single-call version is at least as fast as the Rust BFS for paths up to 10 hops; new implementation has a unit test in `queries.rs::tests`.

### T2-2 (Bv2) — Graph algorithm tests against archetype-specific fixtures (Bv1 T5-7 carryover)

Tier 0 T0-1 above adds archetype-aware tier vocabulary. The test fixtures need extension: today the fixture has an SP pair (pe1, pe2) but the SP topology isn't deep enough to exercise the SP tier vocabulary. Add a 4-device SP topology (P1-P2-PE1-PE2) to `test_fixtures.rs` so SP-archetype algorithms can be tested with realistic structure.

**Where**: `src/graph/test_fixtures.rs`, `src/graph/algorithms.rs::tests`.

### T2-3 (Bv2) — Schema-introspection allow-list in explorer (covered by T0-6)

Same as T0-6 above. Mention here for traceability.

---

## <a id="tier-3"></a>TIER 3 — Carryover from Bv1 Tier 2 (Path A polish, Path B GNN)

### T3-1 (Bv2) — Path A model card

**What**: Bv1 T2-1 spectral embeddings landed; the model card promised in the spec hasn't been written. Document algorithm, dimensions, hyperparameters, dataset, evaluation methodology, known limitations.

**Where**: `docs/ml/path_a_model_card.md`.

**Done when**: a model card exists at the same level of rigour as published ML papers (algorithm, data, eval, limitations).

### T3-2 (Bv2) — Path B GNN (Bv1 T2-2 carryover)

PyTorch Geometric GNN. Pre-requisite: months of archived telemetry from chaos runs against DC + SP labs (now feasible with always-on lab from Tier 1).

**Done when**: GNN trained on 30+ days of chaos archive shows performance ≥ rule-based baseline on a held-out chaos test set; model card with confusion matrix vs rules + tabular ML; deployed as third detector with online inference path.

### T3-3 (Bv2) — Enrichment-aware data loader (Bv1 T2-3 carryover)

GNN data loader handles all enrichment property types via schema registry. Build during T3-2.

### T3-4 (Bv2) — Online inference path (Bv1 T2-4 carryover)

GNN inference on graph snapshot every N seconds; detection events get `gnn_anomaly_score` field; UI surfaces it.

---

## <a id="tier-4"></a>TIER 4 — Carryover from Bv1 Tier 3 (Investigation Agent Maturity)

### T4-1 (Bv2) — Cost dashboard surfacing in UI (Bv1 T3-3 polish)

`Investigations.svelte` shows per-investigation cost; needs an aggregate dashboard showing daily spend, cost-per-investigation distribution, top-cost investigations, budget headroom.

**Where**: extension to `Investigations.svelte` or new tab in Operations workspace.

### T4-2 (Bv2) — Agent memory across investigations (Bv1 T3-4 carryover)

PastInvestigation graph nodes; agent retrieves prior similar investigations as context. Reduces token usage on recurring patterns.

**Pre-requisite**: enough completed investigations (~20+) to provide retrieval value.

### T4-3 (Bv2) — Agent test against the always-on chaos harness

**What**: the agent's investigation quality evaluated against known-correct answers from the chaos harness. For each cataloged fault, the harness records what the correct investigation conclusion is; agent runs against the same fault; compare conclusions.

**Where**: `tests/agent_eval/` driver.

**Done when**: agent quality has a measurable score; regressions in agent reasoning surface in the feedback loop.

---

## <a id="tier-5"></a>TIER 5 — Test Coverage and Operational Hardening

### T5-1 (Bv2) — ServiceNow PDI live tests (when operator provides credentials)

Bv1 T5-1 carryover. Trigger when `SNOW_INSTANCE_URL/USERNAME/PASSWORD` provided.

### T5-2 (Bv2) — HIL e2e test (Bv1 T5-2 carryover)

Now subsumed by Tier 1 chaos-on-always-on-lab — verify that fault → detection → proposal → approve → execute → outcome → trust update flow runs through the chaos harness on every cycle.

### T5-3 (Bv2) — Mutation testing on critical modules (Bv1 T5-4 carryover)

`cargo-mutants` weekly job on `credentials.rs`, `audit.rs`, `remediation/trust.rs`, `assignment.rs`, plus `graph/queries.rs` and `graph/algorithms.rs`. Mutation score ≥80%.

### T5-4 (Bv2) — Verify nightly integration CI runs (Bv1 T5-5 carryover)

Workflow exists; verify it actually runs and produces artefacts. Cheap operational check.

### T5-5 (Bv2) — Detection-firing chaos matrix output validation (Bv1 T5-6 carryover)

Subsumed by Tier 1 T1-4.

---

## <a id="tier-6"></a>TIER 6 — Carryover from Bv1 Tier 4-7

Lower-priority items from Bv1 that didn't land in Sprint 1.

### T6-1 — F-4 binary self-containment CI assertion (Bv1 T4-1 completion)

`LBUG_SHARED=1` is now commented out by default; static build is the default. **Add a CI assertion**: `ldd target/release/bonsai | grep -c liblbug` returns 0. Catches regressions if anyone re-enables the shared build accidentally.

**Where**: `.github/workflows/release.yml` step.

### T6-2 — Release artefact validation (Bv1 T4-2)

Verify `release.yml` produces self-contained artefacts on tag.

### T6-3 — `bonsai self-test` subcommand (Bv1 T4-3)

Verify implementation. If missing, build it. AI agents call this before automation.

### T6-4 — Operator path overrides UI (Bv1 T6-1)

Data model exists in `src/registry.rs::PathOverride`; UI workspace not yet built.

### T6-5 — Subscription resolution audit (Bv1 T6-2)

Device drawer "Effective subscription" panel showing resolution chain.

### T6-6 — Strategic carryover (Bv1 Tier 7)

Catalogue plugin install command, AIOps readiness checklist, signals (syslog + traps), controller adapters demand-driven, NL query, bulk CSV, scale architecture, S3 archive, campus topology, ML feature schema versioning, bitemporal schema, schema migration, Grafeo eval. All defer.

---

## <a id="tier-7"></a>TIER 7 — Documentation Refresh (lowest priority, unchanged)

Bv1 Tier 8 items (README, CLAUDE.md, DECISIONS.md, sprint progress, path profile docs, output adapter docs, UI component docs). All defer until substantive code work completes per the operator's standing instruction.

---

## <a id="execution-order"></a>Execution Order

### Sprint 1 — Hardcoding cleanup (1-2 weeks) ⚡
1. T0-1 Tier vocabulary parameterised by archetype (H-1) — highest priority correctness fix
2. T0-2 Agent model + pricing as config (H-2 + H-3)
3. T0-3 Agent budget config (H-4)
4. T0-4 HTTPS gate in bonsai SDK (H-5)
5. T0-5 Graph algorithm + query parameterisation (H-6 + H-7 + H-8)
6. T0-6 Explorer sanitiser improvements (H-9)
7. T0-7 Buffer pool floor (H-11)
8. T0-8 Budget order-of-operations comment (H-10)
9. T2-2 SP topology in test fixtures (Sprint 1 dependency for T0-1 testing)

### Sprint 2 — Engage feedback loop continuously (1-2 weeks) ⚡
10. T1-1 Always-on feedback runner
11. T1-3 Prometheus + Grafana feedback dashboard
12. T1-4 Chaos harness on always-on lab
13. T1-5 Automatic baseline rotation
14. T1-6 Feedback-loop alerting
15. T1-7 AI session prompt template
16. T1-2 Targeted code corrections worked examples
17. T6-1 F-4 binary self-containment CI assertion (paired since release.yml is touched)

### Sprint 3 — Investigation agent maturity (2 weeks)
18. T4-1 Cost dashboard surfacing
19. T4-3 Agent test against chaos harness
20. T3-1 Path A model card
21. T0-9 Decimal cost accumulation (H-12) — if capacity

### Sprint 4 — Path B GNN (3-4 weeks)
22. T3-2 GNN with message passing
23. T3-3 Enrichment-aware data loader
24. T3-4 Online inference path
25. T4-2 Agent memory (parallel; needs investigation history accumulation)

### Sprint 5 — Test coverage (1-2 weeks)
26. T5-1 ServiceNow PDI live tests (when credentials available)
27. T5-3 Mutation testing
28. T5-4 Nightly CI verification
29. T2-1 Single-Cypher shortest path (when lbug supports deeper traversal)

### Sprint 6 — UI completion (1-2 weeks)
30. T6-4 Operator path overrides UI
31. T6-5 Subscription resolution audit

### After Bv2 — strategic carryover and documentation
- T6-6 strategic carryover items (signals, controller adapters, NL query, etc.)
- Tier 7 documentation refresh

### Continuously running throughout Sprints 2-6
- T1 feedback loop (always-on)
- AI sessions consume feedback signal for targeted corrections per T1-2

---

## <a id="guardrails"></a>Guardrails

### New in Bv2

- **Path discovery candidates require lab verification.** Every new Cypher query, algorithm, or feature surface gets exercised against the test fixtures (DC + SP) before merge. The chaos harness exercises against the lab on every always-on cycle. Code that passes unit tests but fails the chaos matrix is rejected.
- **Hardcoded thresholds get config keys.** Operator-tunable values (BFS depth, hop limits, result limits, tier vocabulary) live in `bonsai.toml`, not Rust code. New defaults require a config schema entry.
- **Production credentials never co-exist with dev creds.** The HTTPS gate (T0-4) makes plain-HTTP a development-only mode that fails closed in production.
- **Memory bounded by configuration, not by detected RAM.** v12 invariant continues. Buffer pool floor (H-11) ensures sub-512MB systems still get a working DB.
- **The feedback loop is the primary signal source for targeted fixes.** AI sessions consume `/api/_test/status` first; design or feature work requires explicit operator approval (the loop is for fixing, not extending).

### Unchanged from v7-Bv1

All prior architectural invariants and discipline continue:
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
- UI shows current state via SSE, not last-fetched
- Graph queries must use the graph database (no Rust-Vec-and-loop for new code)
- Explorer is read-only and sanitised
- GNN training uses real chaos-run archive

### Anti-patterns to reject

- "Hardcoding is fine; we'll fix it later" — no, the H-1 tier vocabulary is already producing wrong labels; this is exactly "later" and we're fixing it
- "Feedback loop only on PR is enough" — no, always-on or AI sessions diagnosing live issues have stale signal
- "Skip the chaos harness this sprint" — no, the matrix is the primary regression detector once always-on is engaged
- "Documentation refresh first" — no, code work always before docs (operator standing instruction)
- "Add a feature instead of fixing the regression" — no, T1-7 prompt template binds AI sessions to fix-not-feature
- All prior anti-patterns remain in force

---

## What Bv2 Explicitly Excludes

- New functional features (the feature surface from Bv1 is rich enough)
- Auth/RBAC, multi-tenancy, production HA, Kubernetes
- Workspace split (current build is fine)
- Bitemporal schema, schema migration, Grafeo evaluation (Tier 6 strategic carryover)
- Controller adapter implementations (demand-driven)
- Real-time streaming GNN (offline batch is the v0 path)
- Auto-graduation of trust state
- Output adapters that write back to the bus
- Auto-import of unverified YANG paths into the default catalogue
- Bonsai-replaces-NDI/DNAC/Meraki marketing positioning

---

*Bv2.0 — authored 2026-05-04 after chunk-by-chunk review of Bv1 first-sprint code land. Verifies substantial Bv1 progress: graph-native value extraction tier landed (multi-hop queries, blast radius, algorithms, explorer with sanitiser, saved queries, test fixtures), Path A spectral embeddings, investigation agent with tools/budget/UI. Surfaces 12 hardcoding findings (H-1 through H-12) — H-1 is a real correctness issue (DC-centric tier vocabulary), H-2 is a stale pricing constants mismatch, H-9 is a sanitiser false-positive issue. Engages the iterative feedback loop continuously as Tier 1 — infrastructure (drivers, status endpoint, protocol doc, CI workflow, memory + binary stability) is mature; remaining work is operational always-on plus baseline rotation plus targeted-fix worked examples. Sprints 1-2 are the highest-leverage 2-4 weeks: hardcoding cleanup gets correctness right; always-on feedback loop becomes the primary signal for ongoing work. After Sprint 2, Sprints 3-6 mature the agent, build the GNN, fill test gaps, complete the UI. References v2-v12 + Bv1 for all unchanged context.*
