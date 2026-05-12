# BONSAI — Backlog Charlie Series, v4 (CV4.0)

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_CV3.md`. Authored 2026-05-12 after end-to-end review of the post-CV3 codebase including Gemini's daily artefacts and sprint closure document.
>
> **The honest re-read**: CV3 landed more than the operator's framing suggested. Verified by diff against the post-CV2 baseline (v23), CV3 delivered substantial work across every tier — Gemini protocol artefacts (brief, task template, closure doc), e2e validation runs against Splunk/Elastic/NetBox/path-validation, chaos plan extension to 25 fault entries plus protected baselines plus adversarial cases, fault propagation snapshots wired into the chaos runner, archive-to-training converter with tests, BMP receiver completion (PeerDown/PeerUp/Stats/Init/Term all parsed), gobgp sidecar with Dockerfile + BGP-LS bridge, UI design tokens (typography, colors, density toggle), and 24 hours of continuous chaos operation producing 159 labeled fault injections.
>
> **What CV3 did not deliver cleanly** are five specific defects that Gemini's testing exposed:
> 1. A script-level smell in `e2e_output_adapters_test.sh` where unrun tests report as PASS with summary `not_run` (masks real coverage gaps)
> 2. A real bonsai application defect: synthetic detection injection succeeds but adapters don't record fresh pushes within the 120s window
> 3. Cloud daily-sync has never produced a branch on origin (operational completion, not code defect)
> 4. Daily-check aggregation conflates "prerequisite missing" with "test failure," producing misleading overall FAIL verdicts when 8 of 10 driver results pass
> 5. Parquet archive writers stay open for the full chaos cycle, so `archive_bytes=0` at any given check even after hundreds of injections — verification scripts and operators see "no data" when data is accumulating in open files
>
> **What CV4 is**: a stabilization backlog focused on those five defects, hands-off operational proof, then two new architectural threads carried from the prior conversation — adaptive resource governance and agent-friendly interface. **Smaller surface than CV3, tighter scope, ends with a clean baseline ready for the GNN training sprint.**
>
> **The motivational reframe for the operator**: the foundation is in much better shape than the friction made it feel. Chaos cycle is running, lab is healthy, smokes all pass, BMP is complete, gobgp is shipped, UI design system has started, and 159 labeled fault injections are already in the chaos archive. CV4 is the work that converts that foundation into something the operator can trust to run for 30 days without daily intervention.

---

## Table of Contents

1. [Where We Are — Honest Account](#where-we-are)
2. [Sprint-by-Sprint CV3 Audit](#audit)
3. [Five Defects from Gemini's Testing](#defects)
4. [TIER 1 — Test Framework Stabilization](#tier-1) ⚡ START HERE ⚡
5. [TIER 2 — Adapter Push Pipeline Fix](#tier-2)
6. [TIER 3 — Hands-Off Operational Proof](#tier-3)
7. [TIER 4 — Adaptive Resource Governance](#tier-4) ⚡ THE NEW WORK ⚡
8. [TIER 5 — Agent-Friendly Interface](#tier-5) ⚡ THE NEW WORK ⚡
9. [TIER 6 — Syslog Facts (Carryover from CV3 Tier 5)](#tier-6)
10. [TIER 7 — GNN Training](#tier-7)
11. [Tracked Future Threads](#tracked)
12. [Execution Order](#execution-order)
13. [Guardrails — Updated](#guardrails)

---

## <a id="where-we-are"></a>Where We Are — Honest Account

The post-CV3 codebase is in materially better shape than the back-and-forth suggested. Concrete evidence from the daily run report and Gemini closure:

- **Lab healthy**: 8/8 DC nodes up, 7/7 BGP EVPN established, 6 IS-IS adjacencies on spine1, EVPN routes present in mac-vrf-a
- **Bonsai healthy**: RSS 295 MB stable, event bus depth 0, write coordinator queue 0%, no budget breaches
- **Chaos cycle running**: 159 injections logged in last 24h across 12 cycles, ground truth CSV at `chaos_runs/20260511T191211Z/injections.csv`, restart marker present (graceful recovery worked)
- **All 7 smoke tests pass**: synthesizer, change_detection, yang_library, output_adapters list/audit, servicenow_aiops read-only, signals_syslog, signals_snmp
- **CV1 endpoints smoke passes**: gNMI readiness, recommendations, config-history, yang/modules, yang/search all reachable
- **Two known issues**: cloud_sync never ran (no branches on origin); adapters not pushing fresh detections

The "FAIL" verdicts in the closure doc are real but narrow. The cloud sync failure is "this hasn't happened yet" rather than "this is broken." The adapter push failure is a real defect with a specific symptom. The daily check FAIL is an aggregation bug — 8 of 10 pass.

**What this means for the operator's mental model**: trust the foundation, fix the small specific things, prove the system runs without you for a week, then start the GNN sprint. Nothing in CV4 requires architectural rework. Everything in CV4 is incremental.

---

## <a id="audit"></a>Sprint-by-Sprint CV3 Audit

Verified against `git`-equivalent diff of v23 (post-CV2) vs the current codebase plus Gemini's artefacts.

### Sprint 1 — Gemini CLI test protocol ✅ LANDED

| Item | Evidence |
|---|---|
| T1-1 Gemini brief | `docs/gemini_cli_brief.md` exists |
| T1-2 git branch policy | Closure doc demonstrates Gemini wrote to dedicated artefact paths |
| T1-3 result format spec | All driver_results write structured JSON; smoke + e2e + daily all conform |
| T1-4 pre-task brief template | `docs/gemini_task_template.md` exists |
| Daily-check resurrection | `docs/test_results/daily_runs/2026-05-11.md` exists; cron-driven format |
| Cloud sync verification | `scripts/cloud/daily_sync_check.sh` exists but reports no branches yet |

### Sprint 2 — E2E validation via Gemini ✅ MOSTLY LANDED, 2 DEFECTS EXPOSED

| Item | Result |
|---|---|
| T4-1 Splunk e2e | Pass (May 11, marked "not_run") then fail (May 12, real symptom) — see Defect 1+2 |
| T4-2 Elastic e2e | Same pattern as Splunk |
| T4-3 ServiceNow EM | Not yet attempted by Gemini |
| T4-4 ServiceNow AIOps bidirectional | Not yet attempted by Gemini |
| T4-5 YANG sync against real repos | Not yet attempted by Gemini |
| T4-6 Synthesizer against real lab | Smoke validated; full e2e against lab not yet attempted |
| New: e2e_netbox | Pass (May 11) — `docs/test_results/e2e_netbox/20260511-pass.md` |
| New: e2e_path_validation | Pass (May 11) |

### Sprint 3 — Data accumulation sharpening ✅ LANDED

| Item | Evidence |
|---|---|
| T2-1 fault propagation snapshots | `scripts/chaos_runner.py` lines 311-512: `_take_snapshot`, `_heal_with_snapshots`, `_schedule_async_snapshots` |
| T2-2 protected baseline windows | `chaos_plans/always_on_dc.yaml` declares `protected_baselines` block |
| T2-3 adversarial cases | `chaos_plans/adversarial_cases.yaml` exists with `injection_frequency_divisor` |
| T2-4 cloud-sync verification | Script exists, has not produced output |
| T2-5 archive-to-training converter | `python/bonsai_ml/gnn/archive_to_training.py` (366 lines) + tests (227 lines) |

### Sprint 4 — Streaming protocol completion ✅ LANDED

| Item | Evidence |
|---|---|
| T6-1 BMP message types completion | `src/streaming/bmp.rs` parses PeerDown/PeerUp/Stats/Init/Term — N-3 from CV3 closed |
| T6-2 gobgp sidecar | `docker/sidecars/gobgp/{Dockerfile,entrypoint.sh,gobgp.toml,bgp_ls_bridge.py}` — N-4 from CV3 closed |
| T6-3 enricher registry pluralisation | Not yet — `registry.rs` still binary; defer |
| T6-4 multi-protocol readiness | Not yet — defer |
| T6-5 lab streaming config | Not yet — defer |
| T6-6 russh migration | Correctly deferred |

### Sprints 5-6 — UI bold and sharp ⚠️ PARTIAL

| Item | Evidence |
|---|---|
| T3-1 type system tokens | `ui/src/lib/design/typography.js` exists |
| T3-2 color system tokens | `ui/src/lib/design/colors.js` + `tokens.css` |
| T3-3 hierarchy applied | Operations.svelte modified, Incidents.svelte modified — needs visual review |
| T3-4 motion tokens | Not yet visible in design directory; defer |
| T3-5 workspace re-articulations | Partial: Operations + Incidents touched; Topology modified |
| T3-6 density toggle | `ui/src/lib/components/DensityToggle.svelte` exists |
| T3-7 keyboard navigation | CommandPalette.svelte modified |

### Sprint 7 — Syslog facts ❌ NOT STARTED

Syslog pattern files updated for capture groups (config/syslog_patterns/*.yaml all differ), and `python/bonsai_sdk/rules/syslog.py` modified. The full `SyslogFact` event type + cross-source join engine + cross-source detection rules — these did not land. Carries forward to CV4 Tier 6.

### Sprint 9 — GNN training ❌ NOT STARTED (correctly — trigger condition not met)

Archive depth target: 30 days. Today: ~1 day of chaos accumulation (159 injections recorded but parquet files still open). Correctly deferred.

---

## <a id="defects"></a>Five Defects from Gemini's Testing

Each grounded in artefact evidence. These drive CV4 Tier 1-3.

### Defect 1 — `e2e_output_adapters_test.sh` reports PASS for unrun tests

**Location**: `scripts/e2e_output_adapters_test.sh:52-57`

```bash
RESULT_PROMETHEUS="SKIP"
RESULT_SPLUNK="SKIP"
RESULT_ELASTIC="SKIP"
SUMMARY_PROMETHEUS="not_run"
SUMMARY_SPLUNK="not_run"
SUMMARY_ELASTIC="not_run"
```

When the script's adapter loop bails before reaching `pass()` or `fail()`, the final write-result block treats absence-of-fail as pass and writes "PASS / Summary: not_run". Evidence: `docs/test_results/e2e_output_adapters/20260511-splunk-pass.md` has `Summary: not_run` — a non-result reported as success.

**Severity**: HIGH. Masks real coverage gaps. Operators reading test artefacts see "passed" when the test never executed against the adapter.

**Fix**: when `RESULT_*` stays at `SKIP` through the run, write a SKIP result file, not a PASS. Treat skip as a distinct status; never collapse to pass.

### Defect 2 — Adapter push pipeline doesn't dispatch fresh synthetic detections

**Location**: `src/output/{splunk_hec.rs, elastic.rs}` and the dispatch path from detection events through output adapters

**Symptom from Gemini closure**: "A fresh detection was successfully injected, but the adapter state endpoint did not record any pushes occurring after the injection time within the 120s polling window."

**Likely causes** (need code investigation):
- Output adapter has a staleness window that filters out "very recent" detections (anti-flapping logic that over-fires)
- Adapter health-check or readiness gate is blocking dispatch
- Adapter's queue isn't drained when detection arrives — only on scheduled flush
- Subscription wiring uses `subscribe()` legacy path that's gone (event_bus is router-only post-Bv4)

**Severity**: HIGH. This is the primary user-facing function of the output adapters — push real detections to external systems. If fresh detections aren't pushed, every output adapter integration is broken.

**Fix**: investigation work. CV4 Tier 2.

### Defect 3 — Cloud daily-sync has never produced a sync branch

**Location**: `scripts/cloud/daily_sync.sh` + cloud VM cron + GITHUB_TOKEN env

**Symptom from Gemini closure**: `FAIL: no sync/cloud-spike branches found on origin`

**Likely causes**: cron not installed on cloud VM, GITHUB_TOKEN not set, git push failing silently, or daily_sync.sh has a real bug. Cannot diagnose from artefacts alone — requires cloud VM access.

**Severity**: MEDIUM. Cloud is supposed to be the secondary archive accumulation path. Until the sync runs, we can't trust the cloud spike as a GNN data source.

**Fix**: operational. CV4 Tier 3.

### Defect 4 — Daily-check aggregation marks overall FAIL when prerequisites are missing

**Location**: `scripts/bv5_daily_check.sh` driver-results aggregation

**Symptom**: 10 driver results checked, 8 pass, 2 fail. The 2 failures are `cloud_sync_check` (no branches yet — prerequisite missing) and `daily.json` (self-reference — circular: "daily.json fails because driver_results=fail; driver_results=fail because daily.json fails"). Status: "FAIL — at least one driver result reported failure."

This logic does not distinguish:
- Test ran and failed (real signal)
- Prerequisite missing (operational not-yet-done)
- Aggregation includes self-reference (logic bug)

**Severity**: MEDIUM. Misleading. Operators see "FAIL" daily reports when the system is actually healthy.

**Fix**: aggregation logic. CV4 Tier 1.

### Defect 5 — Parquet archive writers stay open; archive looks empty even when data accumulates

**Location**: archive rotation policy

**Symptom from daily report**: `archive_bytes:0` even after 159 chaos injections logged and 24h of operation. Verification script correctly notes "no closed parquet files found yet; active writers are still open."

**Why it matters**: operators looking at the daily report see "no archive" and assume nothing is being recorded. Cloud sync can't push files that haven't been closed. Archive verification can't validate schema of open files.

**Fix**: rotate parquet writers on a time interval (e.g., every 60 min) so closed files become visible without waiting for size threshold. CV4 Tier 1.

---

## <a id="tier-1"></a>TIER 1 — Test Framework Stabilization ⚡ START HERE ⚡

Smallest tier. Fixes Defects 1, 4, 5. About 1 week.

### T1-1 (CV4) — `e2e_output_adapters_test.sh` distinguishes pass/skip/fail correctly

**What**: rewrite the result-write block so unrun tests produce SKIP artefacts, not PASS. Introduce `RESULT_*=SKIP` as a terminal state with its own markdown template. Aggregation scripts treat SKIP as "test infrastructure not ready" — distinct from PASS and FAIL.

**Where**: `scripts/e2e_output_adapters_test.sh` final-write block; apply same pattern to `scripts/e2e_netbox_enricher_test.sh`, `scripts/e2e_servicenow_pdi_test.sh` for consistency.

**Done when**: rerun e2e_output_adapters with Splunk's compose profile stopped → produces a `20260512-splunk-skip.md` artefact, not a misleading pass.

### T1-2 (CV4) — Daily-check aggregation distinguishes prerequisite from failure

**What**: `bv5_daily_check.sh` aggregation reads each driver_result's `status` field and bins into `pass / fail / skip / prereq_missing`. The overall verdict logic becomes:
- All pass → overall pass
- Any real fail → overall fail
- No fails, but skips or prereq_missing → overall pass_with_caveats (yellow status, not red)

Specifically: cloud_sync_check should report `prereq_missing` when there are no branches yet (it has nothing to verify), not `fail`. The daily.json file should not self-reference in aggregation (introduce a primary/derived split).

**Where**: `scripts/bv5_daily_check.sh` + the smoke common `write_result` helper.

**Done when**: a daily check run with chaos healthy + cloud_sync prereq-missing produces "PASS_WITH_CAVEATS" verdict, not "FAIL", and the caveat is named explicitly.

### T1-3 (CV4) — Parquet rotation interval

**What**: parquet writers in the archive subsystem flush + close on a configurable interval (default 60 min). Active writers can stay open up to that interval, but a closed file appears at least once per hour. This makes archive verification meaningful, makes cloud sync possible, and gives operators visible feedback that data is accumulating.

**Where**: `src/archive.rs` write loop. Add `[archive.max_file_age_seconds]` config option (default 3600).

**Done when**: a 90-minute chaos cycle produces ≥1 closed parquet file visible to `scripts/verify_archive.sh`.

### T1-4 (CV4) — Driver result categorisation surfaces in UI

**What**: the Operations workspace surfaces driver_result status with proper categories. Today the daily report is a markdown file. Surface a parallel JSON summary at `/api/operations/daily-check` consumed by the UI. Operators see "8 pass / 1 prereq_missing / 1 self-reference" not just "FAIL".

**Where**: `src/http_server.rs` adds an endpoint; UI Operations workspace renders the summary.

**Done when**: opening Operations workspace shows the same information that's in the daily markdown, formatted clearly with semantic colors.

---

## <a id="tier-2"></a>TIER 2 — Adapter Push Pipeline Fix

Defect 2 specifically. About 1 week of investigation + fix.

### T2-1 (CV4) — Diagnose the adapter push gap

**What**: Gemini's failure says "adapter never recorded a fresh push for the synthetic detection." Diagnose where in the pipeline the gap is:

1. Verify the bus subscription path: is the output adapter still subscribed via the legacy `subscribe()` API that was removed in Bv4? (CV2 supposedly migrated all adapters to MpscSubscriber, but maybe one was missed.)
2. Verify the adapter dispatch loop receives the detection event from the bus
3. Verify the adapter doesn't have an anti-flap window that filters very-recent detections
4. Verify the adapter's HTTP push code actually fires when buffer is non-empty
5. Verify `/api/adapters/{name}/state` updates `last_push_at` correctly

**Where**: instrument the adapter dispatch path with structured tracing; run e2e and grep for the trace events.

**Done when**: root cause identified and documented.

### T2-2 (CV4) — Fix the adapter push gap

Depends on T2-1 outcome. Most likely fix is one of:
- Re-subscribe through MpscSubscriber if legacy subscribe was used
- Remove or shrink anti-flap window
- Add immediate-flush path for detection events (separate from sample-flush)
- Fix `last_push_at` update logic

**Done when**: rerun of e2e_output_adapters with all three adapters produces PASS artefacts with non-zero push counts. Smoke test extended to detect this regression in future.

### T2-3 (CV4) — Add adapter push smoke test

**What**: extend `smoke_output_adapters.sh` to inject a synthetic detection via `/api/_test/inject_detection`, then verify within 30 seconds that `/api/adapters/{name}/state` shows a fresh `last_push_at`. This converts Defect 2 into a smoke regression: if it ever happens again, the smoke catches it next daily check.

**Where**: extend `scripts/smoke/smoke_output_adapters.sh`.

**Done when**: smoke runs in <30s, validates fresh-push path end-to-end.

---

## <a id="tier-3"></a>TIER 3 — Hands-Off Operational Proof

The operator's specific concern: "i am still not confident of the cron jobs and the data collection." Address head-on. About 1 week.

### T3-1 (CV4) — Cron installation script + verification

**What**: `scripts/install_cron.sh` (idempotent) that:
- Installs the daily check cron on the laptop (3am UTC)
- Installs the ensure-chaos-running cron (every 30 min)
- Installs the parquet rotation cron if not handled in-process (T1-3 should make this unnecessary)
- Verifies via `crontab -l` post-install
- Writes a structured report of what was installed

For the cloud VM, the equivalent: `scripts/cloud/install_cron.sh` invoked from `deploy.sh`.

**Where**: new scripts; documented in operations guide.

**Done when**: running the install script produces a verifiable cron set + a structured report. Operator never edits crontab by hand again.

### T3-2 (CV4) — Cloud daily-sync repair

**What**: investigate why no `sync/cloud-spike/*` branches exist. Likely causes:
- GITHUB_TOKEN not set on cloud VM
- git remote not configured for HTTPS auth
- daily_sync.sh fails silently and exits 0
- cron entry doesn't actually exist on cloud VM

Diagnose, fix, run once manually to seed first branch, verify cron invocation works.

**Where**: `scripts/cloud/daily_sync.sh` + cloud VM cron + secrets management.

**Done when**: at least 1 `sync/cloud-spike/YYYYMMDD` branch exists on origin, contains expected artefacts, and is reproducible.

### T3-3 (CV4) — 7-day hands-off operation test

**What**: declare the operational baseline. Day 0: start chaos runner, install crons, verify smokes pass, verify daily check runs. Day 7: review the seven daily-check reports without any intervening operator action. Acceptance:
- 7 consecutive daily check reports written
- Each one shows pass-with-caveats or full pass
- Chaos cycle uninterrupted (≤2 restart markers in chaos log)
- Archive grows visibly (closed parquet files appear)
- Cloud sync produces 6 branches (Day 0 sync produces one for Day -1, but Day 0 itself syncs on Day 1)

**Where**: operational. Document the start/end criteria. Operator's only action between Day 0 and Day 7 is checking that the daily reports appear.

**Done when**: 7 days of clean reports. If anything breaks during the 7 days, fix it and restart the window. **This is the gate for trusting hands-off operation toward the GNN trigger.**

### T3-4 (CV4) — Operational health dashboard

**What**: the Operations workspace already shows real-time state. Add a "Last 7 days" panel that summarizes the daily reports. Aggregates: daily check pass rate, chaos injection count per day, archive bytes accumulated per day, output adapter push counts, smoke pass rate.

**Where**: extend `ui/src/routes/Operations.svelte`.

**Done when**: opening Operations shows a 7-day trend the operator can scan in 10 seconds.

---

## <a id="tier-4"></a>TIER 4 — Adaptive Resource Governance ⚡ THE NEW WORK ⚡

Carried from our prior conversation. About 2 weeks. Land before syslog facts or production-scale streaming.

The core insight: bonsai today has *static* resource configuration. Memory bound is set in config. Caches sized in config. Batch sizes set in config. **Bonsai doesn't probe its environment or self-throttle when pressure rises.** Three feedback loops solve this.

### T4-1 (CV4) — Environment probe at startup

**What**: at startup, probe and classify the runtime environment:
- Available RAM (`sysinfo` crate)
- Available CPU cores (`num_cpus`)
- Available disk space at archive + log paths
- Whether running in cgroup-constrained environment (read `/sys/fs/cgroup/memory.max` on Linux)
- Whether running in container vs host (presence of `/.dockerenv`)

Derive a `ResourceProfile` enum: `Tiny / Small / Medium / Large / XLarge`. Load profile-specific defaults for:
- Memory budget (override `min(2GB, 25% RAM)` rule)
- LRU cache sizes
- Write coordinator batch size
- Event bus channel capacity
- Archive flush interval

Operator config still takes precedence; the probe sets sensible defaults for the un-configured case.

**Where**: new `src/resource_profile.rs`; called from `main.rs` startup.

**Done when**: starting bonsai on a 4 GB VM picks `Small` profile with appropriate defaults; starting on a 32 GB workstation picks `Large`. Profile logged at INFO. Documented in `docs/resource_profiles.md`.

### T4-2 (CV4) — Inbound rate governance

**What**: per source (gNMI subscribe, BMP, BGP-LS, syslog UDP, SNMP trap), measure events-per-second. When any source exceeds the profile's budget for that source, the governor responds.

Per-source policies:
- **gNMI Subscribe**: renegotiate sample intervals upward (request 30s instead of 10s) on next reconnect. Streaming subscriptions on ON_CHANGE are not throttled.
- **BMP**: drop low-priority message types (StatisticsReport droppable, RouteMonitoring not).
- **Syslog UDP**: per-source-IP quotas; shed overflow with metric increment.
- **SNMP Traps**: rate limit per source with bucket.

**Where**: extend `src/ingest.rs` and the per-source receivers. New `src/resource_governor.rs`.

**Done when**: synthetic load test (replay syslog at 10K/s when budget is 1K/s) shows the governor sheds the excess, metric `bonsai_rate_shed_total{source=...}` increments, bonsai memory stays bounded.

### T4-3 (CV4) — Memory pressure governance

**What**: a background task observes RSS every 5s. When RSS approaches budget:
- Shrink LRU debounce caches by 25%
- Increase debounce intervals by 50%
- Trigger early archive flush (close current parquet, open new one)
- Ask write coordinator to flush more aggressively

When RSS retreats from budget, gradually relax.

**Where**: `src/resource_governor.rs`. Reuses Bv4 memory budget assertions as kill-switch; governance is the graceful-degradation curve before kill-switch.

**Done when**: synthetic memory-pressure test (force-grow event bus depth) shows the governor responds with degradation actions visible in logs and metrics, RSS retreats, system stays operational.

### T4-4 (CV4) — Write pressure governance

**What**: observe `write_coordinator_queue_pct`. When > 50% sustained for 60s:
- Increase batch size (fewer transactions, more updates per transaction)
- Selectively skip low-value writes (counter samples within debounce noise)
- Optionally route to in-memory-only state with deferred persistence

When queue retreats below 30%, relax.

**Where**: `src/resource_governor.rs` + write_coordinator integration.

**Done when**: synthetic write-spike (replay archived parquet at 10× speed) shows queue grows but doesn't OOM; governor responds; queue eventually drains.

### T4-5 (CV4) — Governance observability

**What**: every governance action emits a structured event. `bonsai_governance_action_total{action="shrink_lru",reason="memory_pressure",profile="small"}` counter. Operations workspace shows governance state visibly.

**Where**: metrics + UI extension.

**Done when**: 1 hour of operation under synthetic pressure produces a readable governance trace.

### T4-6 (CV4) — Documentation

**What**: `docs/resource_profiles.md` describes each profile, its defaults, and when to choose each. `docs/operational_health_thresholds.md` updated to reference governance actions.

---

## <a id="tier-5"></a>TIER 5 — Agent-Friendly Interface ⚡ THE NEW WORK ⚡

Carried from our prior conversation. About 1-2 weeks. Read-side composition over existing data; no LLM in hot path.

### T5-1 (CV4) — MCP server exposing bonsai's core read APIs

**What**: thin shim wrapping the existing REST endpoints as MCP tool definitions. Read-only. Stateless.

Tools exposed:
- `get_incident` (by id) — returns full incident with correlated detections, blast radius, member rules
- `query_devices` (with filters) — returns matching devices with current state
- `get_device_blast_radius` (by address) — returns affected services + downstream impact
- `list_active_detections` (with time window + severity filter)
- `query_graph` (Cypher passthrough) — read-only graph queries

**Where**: new crate or module `src/mcp_server.rs`. Use the `rmcp` Rust crate. Runs as separate process or feature-gated binary so it can be deployed independently.

**Done when**: Claude Desktop / Claude Code / Gemini CLI can connect to bonsai's MCP server, list tools, and call them against the running stack.

### T5-2 (CV4) — Grounded response composition

**What**: new endpoint `/api/incidents/{id}/grounded` returns:
- Detection events with timestamps
- Topological context (blast radius, affected services, upstream causes)
- Procedural references (links to detection-rule documentation)
- Recurrence indicators (from rule docs)

Pure read-side composition over existing data. No new storage. Implements the "three sources of grounding" pattern: topology + procedure + live state in one response.

**Where**: extend `src/http_server.rs` and `src/graph/queries.rs`.

**Done when**: GETting `/api/incidents/{id}/grounded` returns a structured payload with all three grounding sources for a real chaos incident.

### T5-3 (CV4) — Self-describing schema endpoint

**What**: `/api/schema` returns OpenAPI 3 spec with rich field descriptions for every endpoint. Lets agents introspect bonsai without prior knowledge.

**Where**: `src/http_server.rs`.

**Done when**: an agent given only the schema URL can construct valid requests to bonsai's endpoints.

### T5-4 (CV4) — Recurrence indicators on detection rules

**What**: detection rule documentation gains a `recurrence_indicators` field — observable patterns that signal "this is happening again." Pattern from the CNS document. Per-rule docs include "what to check when this fires."

**Where**: rule documentation files (`python/bonsai_sdk/rules/*.py` docstrings + auto-extraction); surfaced in grounded responses (T5-2).

**Done when**: every detection rule has at least one recurrence indicator documented.

### T5-5 (CV4) — Natural-language reference resolution

**What**: `/api/resolve?q=<text>` takes a fuzzy reference ("the BGP issue from earlier", "spine1's incidents today") and returns candidate stable IDs with confidence scores. Doesn't need to be smart — string match against recent incidents + device hostnames + rule IDs.

**Where**: new endpoint in `src/http_server.rs`.

**Done when**: common queries from a Claude/Gemini session resolve to stable bonsai IDs.

---

## <a id="tier-6"></a>TIER 6 — Syslog Facts (Carryover from CV3 Tier 5)

Unchanged from CV3 Tier 5. About 2-3 weeks. Land after Tier 4 governance because syslog volume scales hard.

### T6-1 (CV4) — Syslog pattern files extended with capture groups
### T6-2 (CV4) — SyslogFact event type + extraction pipeline
### T6-3 (CV4) — Cross-source join engine
### T6-4 (CV4) — Cross-source detection rules

---

## <a id="tier-7"></a>TIER 7 — GNN Training

Gates on Tier 3 hands-off operation proof + archive depth. Trigger condition unchanged from Bv5:
- Archive depth ≥ 30 calendar days post-reset
- ≥ 500 chaos injections (currently 159, on pace for 30 days)
- ≥ 50 examples per active detection rule
- Baselines stable for 7 consecutive days
- No crashes for 14 days
- Integrity verifies for 14 nights

### T7-1 (CV4) — Archive-to-training converter validated against real archive

`archive_to_training.py` exists with tests against synthetic. Validate against real 30-day archive when available.

### T7-2 (CV4) — GraphSAGE/GAT training

When trigger met.

### T7-3 (CV4) — Comparison study (rules vs tabular ML vs GNN)

Use harnesses from Bv5.

### T7-4 (CV4) — Online inference path

Graph snapshot every N seconds; GNN scores Devices; UI surfaces.

### T7-5 (CV4) — Model card

Honest documentation. Multi-signal coverage. Limitations explicit.

---

## <a id="tracked"></a>Tracked Future Threads

From our prior conversation. **Not work now**; documented so we don't accidentally close the door.

### Scale-up architecture paths (Path A / B / C)

Single-writer LadybugDB binds at ~1000 devices. Three plausible paths beyond:
- **Path A**: vertical scale-up of single-writer (cheap, ~1 sprint, 2-5x headroom)
- **Path B**: partitioned cores (~2 months, changes shape)
- **Path C**: read replicas + write leader (helps read-heavy load)

Today's architecture is closest to Path A. Don't preclude B or C. Specifically: when schema changes land, consider partition-key fields. When bus changes land, consider routing-by-partition.

### Kubernetes deployment

Defer until after GNN northstar lands. Helm chart pattern: three shapes (single-node, HA-core StatefulSet, collector-fleet Deployment). Stateful workloads use PersistentVolumes. Configuration via ConfigMaps. Secrets via Kubernetes Secrets or cloud-platform-native stores.

### Cloud platform deployability

Mostly documentation work, not code. Per-platform recipes for AWS/GCP/Azure/OCI when we deploy there. Today's code is already cloud-portable in shape.

### Beyond network platforms (firewalls, VPN, cloud networking)

Positioning expansion. Most fits the existing layered ingestion model. Cloud networking (AWS VPC, GCP VPC, Azure VNet) is a new Layer 3 enricher per cloud. Worth folding into positioning revision after GNN lands.

### eBPF spike

Genuinely interesting for Linux-host-level network telemetry where gNMI doesn't reach. Timeboxed exploration, 1 week, single Linux host, simple program. Output is either "yes, fits, here's a tier" or "interesting but not yet." Defer until after Tier 4 governance and Tier 5 agent interface land.

### nftables

Operations tool. Not architecturally interesting. Tabled.

---

## <a id="execution-order"></a>Execution Order

### Sprint 1 (1 week) — Test framework stabilization
1. T1-1 e2e script SKIP semantics
2. T1-2 daily check aggregation
3. T1-3 parquet rotation interval
4. T1-4 driver result UI surface

### Sprint 2 (1 week) — Adapter push pipeline
5. T2-1 diagnose
6. T2-2 fix
7. T2-3 smoke regression test

### Sprint 3 (1 week) — Hands-off operational proof
8. T3-1 cron installation
9. T3-2 cloud daily-sync repair
10. T3-3 7-day operation test starts
11. T3-4 7-day dashboard

### Sprint 4 (2 weeks) — Adaptive resource governance
12. T4-1 environment probe
13. T4-2 inbound rate governance
14. T4-3 memory pressure governance
15. T4-4 write pressure governance
16. T4-5 observability
17. T4-6 documentation

### Sprint 5 (1-2 weeks) — Agent-friendly interface
18. T5-1 MCP server
19. T5-2 grounded response endpoint
20. T5-3 schema endpoint
21. T5-4 recurrence indicators
22. T5-5 natural-language resolution

### Sprint 6 (2-3 weeks) — Syslog facts (CV3 Tier 5 carryover)
23. T6-1 through T6-4

### Continuously through all sprints
- Chaos cycle runs uninterrupted on laptop
- Cloud chaos cycle runs uninterrupted on OCI
- Daily check reports accumulate
- Archive accumulates toward 30-day GNN trigger

### Sprint 7 (3-4 weeks) — GNN training when trigger met
24. T7-1 through T7-5

### Estimated total
**6-9 weeks** to a state where bonsai has:
- All Gemini-exposed defects fixed
- Hands-off operation proven over 7 days
- Resource governance for production scale
- Agent-friendly interface enabling consumption from Claude Code / Gemini / ServiceNow / others
- Syslog evolved into structured facts with cross-source joins
- Path B GNN trained on real chaos archive with snapshots + adversarial cases + protected baselines

Most of the architectural work is done. CV4 is the consolidation.

---

## <a id="guardrails"></a>Guardrails — Updated

### New in CV4

- **SKIP is a first-class status, distinct from PASS and FAIL.** Test infrastructure not ready is not the same as test passing.
- **Daily check aggregation distinguishes prereq_missing from failure.** Missing prerequisites are operational state, not test signal.
- **Parquet writers rotate on time, not just size.** Visible archive growth matters as much as efficient compression.
- **Hands-off operation is the gate.** Seven consecutive clean daily reports without operator intervention is the trust threshold.
- **Resource governance is non-optional at production scale.** Static config is not sufficient.
- **Bonsai is a real MCP server.** Agent consumption is a first-class audience.
- **Recurrence indicators on every detection rule.** Pattern adopted from the CNS document.

### Unchanged from v7-CV3

All prior architectural invariants. Reference earlier backlogs.

### Anti-patterns to reject

- "Pass when not run" — never. SKIP is the right status.
- "FAIL aggregation when prereqs missing" — distinguish operational state from test signal.
- "Add new features before hands-off proof" — don't. Sprint 3 gates the rest.
- "Skip the agent interface until later" — no. The compounding value of agent consumption justifies landing it now alongside governance.
- "Polish UI before stabilizing tests" — no. Test stability is foundational.

---

## What CV4 Explicitly Excludes

- Path B/C scale-up architecture (tracked, not built)
- K8s deployment (post-GNN)
- eBPF exploration (after Tier 4/5)
- Beyond-network positioning expansion (post-GNN)
- Multi-agent orchestration patterns (not our architecture)
- New ingestion layers beyond what's already specified

---

*CV4.0 — authored 2026-05-12 after end-to-end review of post-CV3 codebase including Gemini's daily artefacts and sprint closure document. CV3 actually delivered substantially: Gemini protocol works, e2e harness ran, chaos accumulation has begun (159 injections in 24h with snapshots, protected baselines, adversarial cases), BMP receiver complete, gobgp sidecar shipped, UI design tokens started, archive-to-training converter implemented with tests. Five specific defects exposed by Gemini drive CV4 Tier 1-3: e2e script pass-by-not-run smell, adapter push pipeline gap, cloud sync never run, daily check aggregation conflates prereq with failure, parquet writers stay open hiding archive growth. Tier 4 introduces adaptive resource governance (environment probe + three feedback loops + observability) — foundational for production scale. Tier 5 introduces agent-friendly interface (MCP server + grounded responses + schema + recurrence indicators + reference resolution) — the bridge to AIOps adoption. Tier 6 carries syslog facts forward. Tier 7 GNN training gates on archive depth. Tracked future threads (scale-up paths, K8s, cloud platforms, beyond-network positioning, eBPF) preserved without being built. Estimated 6-9 weeks to deployable destination. References v7-CV3 for unchanged context.*
