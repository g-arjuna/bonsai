/784


















i# BONSAI — Backlog Bravo Series, v5 (Bv5.0)

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_BV4.md`. Authored 2026-05-07 after end-to-end review of Bv4 sprint landings.
>
> **Bv4 was a clean-execution sprint.** All nine architectural concerns (C-1 through C-9) landed correctly. The ingestion architecture now scales to 200+ devices without the failure modes that crashed Bv2-mod. Cloud-spike infrastructure (Oracle Always Free provisioning, deployment, daily archive sync, kill criteria) is built but unused. Chaos plan and runner are built but no archive has accumulated.
>
> **Where bonsai stands today**: ~70% of the MVP stack is in place. The remaining 30% is **operational discipline** — running the chaos cycle, accumulating 30 days of archive, computing real-data baselines for detection rules. **The next gap is not engineering. It is data.**
>
> **What Bv5 is**:
> 1. The **data-gathering playbook** with two parallel tracks: laptop-based (always available) and cloud-based (timeboxed evaluation, kill if free-tier provisioning fails). For each: what quality of data, what chaos types, how injected, how monitored, how rotated, how to know when we have enough.
> 2. The **prioritised list of work that does NOT need to wait for data**. Things engineers can build while data accumulates.
> 3. **Honest readiness assessment** against the MVP definition and northstar from Bv2-mod.

---

## Table of Contents

1. [Audience and Positioning](#positioning) — see v7
2. [Bv4 Sprint Outcome — Verified Landing](#progress)
3. [MVP and Northstar Readiness Scorecard](#readiness)
4. [TIER 1 — Data Gathering Playbook (laptop + cloud paths)](#tier-1) ⚡ START NOW ⚡
5. [TIER 2 — Parallel Work That Doesn't Wait](#tier-2)
6. [TIER 3 — GNN Training (gates on data depth)](#tier-3)
7. [Carryover from Bv4](#carryover)
8. [Execution Order](#execution-order)
9. [Guardrails](#guardrails)

---

## <a id="positioning"></a>Audience and Positioning

**Unchanged from v7-Bv4.** Controller-less primary audience across DC, campus, SP. AIOps integration as feeder. Northstar: bonsai detects real faults in real labs against real telemetry, with graph-native impact analysis and Path A embeddings working — Path B GNN as the destination.

---

## <a id="progress"></a>Bv4 Sprint Outcome — Verified Landing

End-to-end code review confirms all Bv4 work landed cleanly. Removing completed items per operator instruction.

### All nine architectural concerns fixed

| Concern | Status | Evidence |
|---|---|---|
| C-1 batch all-or-nothing rollback | ✅ Fixed | `src/graph/mod.rs:1561-1583` — always COMMIT, individual errors logged + counted via `bonsai_graph_write_errors_total` |
| C-2 dual-bus pattern | ✅ Fixed | `src/event_bus.rs` — legacy broadcast removed, single router-only path |
| C-3 DropOldest unimplemented | ✅ Fixed | `src/event_bus.rs:131-178` — `BroadcastSubscriber` with true DropOldest; `MpscSubscriber::new` panics if asked for DropOldest |
| C-4 struct-clone per subscriber | ✅ Fixed | `src/event_bus.rs:36,219` — `Arc<TelemetryUpdate>` everywhere; pointer clones |
| C-5 RwLock per-update | ✅ Fixed | `src/event_bus.rs:185,244` — `ArcSwap` lock-free read path |
| C-6 three Mutex<LruCache> contention | ✅ Fixed | `src/ingest.rs:37-69` — 16-shard `ShardedLruCache` |
| C-7 LRU caps too small | ✅ Fixed | `src/ingest.rs:101-118`, `src/config.rs:434` — `[ingest.debounce_memory_bytes]` config; default 16 MiB; ~43K counter entries |
| C-8 no log rotation | ✅ Fixed | `src/main.rs:82-110` — `RollingFileAppender` with daily rotation, 7-day retention default, preflight disk check |
| C-9 counter-summary mode hidden | ✅ Fixed | `src/http_server.rs:1793` + `ui/src/routes/Operations.svelte:207-219` — counter mode visible in Operations workspace |

### Bv4 Tier 2-5 infrastructure delivered

| Item | Status | Evidence |
|---|---|---|
| File-rotated logging | ✅ Done | `[logging]` config section, per-module level overrides, log volume metrics |
| Always-on chaos plan (DC) | ✅ Built | `chaos_plans/always_on_dc.yaml` (132 lines, 18 fault entries: 6 netem_loss, 6 interface_shut, 6 bgp_session_down) |
| Always-on chaos plan (cloud-DC) | ✅ Built | `chaos_plans/always_on_cloud_dc.yaml` (84 lines, 10 cloud-safe fault entries: interface_shut + bgp_session_down; netem omitted because Oracle UEK lacks `sch_netem`) |
| Chaos runner daemon | ✅ Built | `scripts/chaos_runner.sh` (140 lines) + `scripts/chaos_runner.py` |
| Detection baseline computation | ✅ Built | `scripts/compute_detection_baselines.py` (384 lines) |
| Archive integrity verification | ✅ Built | `scripts/verify_archive.sh` (230 lines) |
| Cloud lab variant | ✅ Built | `lab/cloud-dc-6node.yml` + 6 SRL configs (sized for Oracle Always Free 24 GB ARM) |
| Oracle Always Free provisioning | ✅ Built | `scripts/cloud/oracle_setup.sh` (290 lines) |
| Cloud-init bootstrap | ✅ Built | `scripts/cloud/cloud_init.sh` (118 lines) |
| Cloud deploy script | ✅ Built | `scripts/cloud/deploy.sh` (314 lines) |
| Daily archive sync to GitHub | ✅ Built | `scripts/cloud/daily_sync.sh` (214 lines) |
| Cloud spike kill criteria | ✅ Documented | `docs/test_results/cloud_spike/KILL_CRITERIA.md` |
| Cloud spike report template | ✅ Documented | `docs/test_results/cloud_spike/REPORT_TEMPLATE.md` |

### Not yet started

- 🟡 Chaos archive accumulation — laptop quiet baseline started; cloud chaos started 2026-05-08 and is producing clean BGP/interface cycles
- 🟡 Cloud spike execution — Oracle Always Free VM provisioned and deployed; 5-day evaluation now running
- ❌ Detection baseline computation against real archive — script exists, no archive to compute against
- ❌ SP lab bring-up — deferred to after sufficient DC archive
- ❌ GNN training — gates on archive depth

### Execution update — 2026-05-08

- ✅ Laptop quiet-baseline track started: DC lab healthy enough for baseline, Bonsai running locally, Parquet archive enabled under `runtime/archive`, baseline snapshot captured.
- ✅ T2-1 GNN data loader skeleton landed with synthetic fixtures and unit tests.
- ✅ Cloud T1-B-1 provisioning completed: Oracle A1 Always Free VM is running at `4 OCPU / 24 GB RAM`, with `50 GB` boot + `150 GB` archive block volume (`200 GB` total).
- ✅ Cloud provisioning/deploy scripts hardened: `--dry-run` no longer launches instances, cloud-init firewalld calls are timeout-bounded, cloud deploy writes a cloud-specific `bonsai.toml`.
- ✅ T1-A-3 daily verification wrapper added: `scripts/bv5_daily_check.sh` writes `docs/test_results/daily_runs/<date>.md`; 2026-05-08 laptop snapshot captured.
- ✅ T2-2 synthetic rule-baseline harness added: `python/bonsai_ml/eval/rule_baseline.py` with precision/recall/F1, latency, clear-time, and Markdown reporting tests.
- ✅ T2-3 tabular ML detector harness added: `python/bonsai_ml/eval/tabular_ml.py` scores feature windows via model duck-typing and reuses the shared rule metric contract.
- ✅ Cloud T1-B-2 deployment completed: Docker/ContainerLab, 6-node SR Linux lab, Bonsai systemd service, NetBox, Prometheus, Grafana, archive verification, and chaos daemon are running on the VM.
- ✅ Cloud chaos adjusted for Oracle Always Free: netem removed from the cloud plan because `sch_netem` is absent on Oracle UEK; Nokia SR Linux BGP/interface faults use Docker transport so they do not depend on per-node management SSH.
- ⚠️ Cloud T1-B-3 sync is partially ready: daily cron is installed and archive verification passes, but `GITHUB_TOKEN` is missing from the VM environment, so GitHub push is not active yet.

---

## <a id="readiness"></a>MVP and Northstar Readiness Scorecard

Honest assessment against the Bv2-mod definitions.

### MVP definition (from Bv2-mod)

> Bonsai detects real faults in real labs using real telemetry, produces real detection events, the graph correctly captures the impact, an operator (or Claude Code session) can answer "what does this mean and what should I do about it" using the running system, and Path A graph embeddings compute against the populated real graph.

| Component | Status | Evidence |
|---|---|---|
| Lab operational | ✅ Done | DC EVPN-SRv6 lab runs cleanly (B1-B14 fixed in Sprint 1) |
| Real telemetry consumed | ✅ Done | gNMI subscriptions stream from 8 SR Linux nodes; ingestion architecture absorbs the load |
| Real detection events fire | ⚠️ Partial | Detection rules exist; triggered manually during Sprint 1; not yet validated systematically against chaos faults |
| Graph captures impact correctly | ✅ Done | Multi-hop queries land; blast radius works; mgmt-plane LLDP filtered out so topology is clean |
| Operator can answer "what does this mean" | ⚠️ Partial | Explorer + saved queries + topology UI work; investigation agent exists in code but **not used** (token budget); Claude Code as the diagnostic agent works as designed |
| Path A embeddings compute on real graph | ⚠️ Partial | Code exists; Path A model card landed; whether embeddings actually computed and persisted on the real graph not independently verified |
| **Sustained operation without crashing** | ✅ Done | Bv3 + Bv4 fixes mean memory stable, write contention solved, log rotation prevents disk fill |

**MVP readiness: ~85%.** Remaining 15% is data validation — we have the system, we need real-data evidence that detection works under fault, that embeddings cluster meaningfully, that Claude Code sessions productively diagnose regressions from `/api/_test/status`.

### Northstar definition (from v9 + Bv2-mod)

> Path B GNN catches at least one cascading-failure class that rules + tabular ML miss, with documented confusion matrix on a held-out chaos test set.

| Component | Status |
|---|---|
| Path A embeddings on real graph | Conditional on MVP completion |
| 30+ days of chaos archive | ❌ 0 days — has not started |
| Tabular ML baseline | ⚠️ Existing detector code — needs evaluation harness |
| Rule-based baseline | ⚠️ Existing detection rules — needs evaluation harness |
| GNN data loader | ❌ Not started (gates on archive existing) |
| GNN training | ❌ Not started (gates on data + loader) |
| Honest evaluation | ❌ Not started |
| Online inference | ❌ Not started |
| Model card | ❌ Not started |

**Northstar readiness: ~10% by code, but the time-to-northstar is dominated by data accumulation.** If chaos runner starts today and runs 30 days continuously, by Day 30 we have the data; engineering time for the loader + training + evaluation + model card is ~3-4 weeks parallel to the last 3 weeks of data accumulation. **Realistic time-to-northstar: 6-8 weeks if data accumulation starts now and engineering happens in parallel.**

---

## <a id="tier-1"></a>TIER 1 — Data Gathering Playbook ⚡ START NOW ⚡

This tier is the binding constraint. Two parallel paths so the operator is never blocked: laptop (always available) and cloud (timeboxed evaluation).

### What "good chaos data" actually means

The GNN's training quality and bonsai's detection rule quality both depend on the same property: **archive that contains diverse, faithfully-labelled, distributionally-representative fault scenarios with their telemetry traces and ground-truth labels.**

Concretely, a "good" chaos archive has:

1. **Coverage breadth** — every detection rule has been exercised at least 30 times under faults that should trigger it AND at least 30 times under faults that should not trigger it. Without both, false-positive vs true-positive rates can't be measured.
2. **Coverage depth per rule** — within each rule, faults span the parameter space (e.g. for `bgp_session_down`: brief sessions, sustained sessions, flapping sessions, multiple peers down, gradual onset, abrupt onset).
3. **Temporal diversity** — faults at different times of day, with different recovery delays, with overlapping faults vs isolated faults. The graph state when a fault hits matters; if every fault hits a clean graph the model learns trivially.
4. **Topological diversity** — faults on leaves vs spines vs super-spines. On uplinks vs cross-links. Single-device vs multi-device.
5. **Background traffic** — even if synthetic, non-fault traffic causes counter movement that the model needs to distinguish from fault signal.
6. **Honest ground truth** — each fault injection produces a labelled record: fault_id, target, parameters, inject_time, heal_time, expected_detection. This is the supervision signal.
7. **Archive integrity** — Parquet files readable, schema stable, compression ratios reasonable, no gaps.

### Quantity targets

For Path B GNN training the minimum useful archive size is approximately:
- **30 days of continuous operation** (provides enough non-fault baseline)
- **~500 fault injections** with diverse types (yields ~50 examples per detection rule)
- **~5-10 GB of compressed Parquet** (enough variety after deduplication)

These are floors, not ceilings. More data is better. The chaos plan should run continuously until the trigger condition is met, not for a fixed duration.

### How chaos gets introduced

The runner (`scripts/chaos_runner.sh` + `scripts/chaos_runner.py`) drives the plan (`chaos_plans/always_on_dc.yaml`). Each cycle:

1. **Picks a fault** from the catalogue weighted by `weight` field (current plan: 6 of each type, weights 3-5)
2. **Computes parameters** within ranges specified per fault type (e.g. `loss_percent: [2, 15]` picks a uniform random)
3. **Computes timing** — `injection_interval_seconds: [45, 120]` adds randomness between injections
4. **Injects** via `containerlab tools netem set` for impairments, `gnmic set` or container `docker exec` for protocol-level faults
5. **Waits the inject duration** (faults run for `healing_delay_seconds` — typically 30-90s)
6. **Heals** by removing impairment / restoring config
7. **Records** to `runtime/chaos_log.jsonl` with full provenance: fault_id, target, params, inject_ns, heal_ns, expected_detection_rule_id

Bonsai's archive captures the telemetry side; the chaos log captures the ground-truth side. Together they form the labelled training set.

### How it's monitored during the run

Three layers of monitoring, each at a different time scale:

**Real-time (seconds)**:
- `bonsai_event_bus_depth` gauge — should stay below 50%
- `bonsai_write_coordinator_queue_depth` gauge — should drain quickly
- `bonsai_graph_write_errors_total` counter — should not increment under normal conditions
- RSS via memory profile — should plateau within 15 min of start
- Disk free at log path — should stay above min_free_bytes

**Per-cycle (minutes)**:
- `runtime/driver_results/chaos.json` updated with per-fault outcome
- Each fault: did the expected detection fire within window? Latency? Resolved when fault healed?

**Daily (longer-term)**:
- `scripts/verify_archive.sh` runs nightly via cron
- Archive growth rate (target ~100-500 MB/day at normal counter rates)
- Detection rule baseline metrics computed by `scripts/compute_detection_baselines.py`
- Daily summary written to `docs/test_results/daily_runs/<date>.md`

### How to know we have enough

Trigger the GNN training phase when **all** of:
- Archive depth ≥ 30 calendar days (gives the model enough non-fault baseline)
- ≥ 500 chaos injections recorded
- ≥ 50 examples per active detection rule (covers parameter space)
- Detection rule baselines stable (p95 detection latency consistent for 7 consecutive days)
- No crashes or OOMs in last 14 days
- Archive integrity verifies cleanly for 14 consecutive nightly runs

If any of these are not met, accumulate more before training. Premature training on thin data produces overfit models that learn bring-up bugs, not generalisable patterns.

### Path A: Laptop-Based Data Gathering

**Always available. Start today. Continue regardless of cloud spike outcome.**

#### T1-A-1 (Bv5) — Operational baseline ahead of chaos start

Before starting the always-on cycle, establish the operational baseline:

1. **Cold-start checklist run** — `scripts/check_lab.sh dc` returns all-green; bonsai-core healthy; NetBox + ServiceNow PDI enrichers run successfully; Operations workspace shows green metrics.
2. **24-hour quiet run** — bonsai operating against the lab without injected chaos. Confirms:
   - RSS plateau (per Bv4 T1-5 budget assertions)
   - Log file growth rate matches expectation (~100 MB/day at info, ~500 MB/day at debug)
   - No silent enrichment failures
   - `/api/_test/status` returns green throughout
3. **Capture the baseline** — `runtime/baseline_status_<date>.json` snapshot for AI consumption later. This is the "no-fault" reference.

**Done when**: 24 hours of lab + bonsai running without injected faults; all health metrics stable.

#### T1-A-2 (Bv5) — Start the always-on chaos cycle

`bash scripts/chaos_runner.sh` (background daemon mode). Runs continuously. 30-minute cycles. ~3-5 fault injections per cycle (from current plan).

**Discipline during the run**:
- **Do not stop the daemon casually.** Each restart resets cycle counters and may produce gaps.
- **Do not change the chaos plan once started.** Plan changes mid-run produce inhomogeneous data.
- **If bonsai crashes, fix and restart immediately**, but flag the gap in `runtime/chaos_log.jsonl` with a `restart_marker` entry.
- **Daily check** of `bash scripts/chaos_runner.sh --status`. Reads recent log, confirms cycles completing.

**Operator effort during run**: ~5 minutes per day for status check, plus periodic disk-space monitoring.

**Done when**: chaos runner daemon up; first 100 cycles completed; archive growing.

#### T1-A-3 (Bv5) — Daily archive verification + baseline computation

A nightly cron entry:
```
0 3 * * * cd ~/bonsai && bash scripts/verify_archive.sh && python scripts/compute_detection_baselines.py >> docs/test_results/daily_runs/$(date +%Y-%m-%d).md
```

**Verifier confirms**:
- All Parquet files readable
- Schema stable
- Row counts monotonically increasing
- Compression ratio in expected range
- No corruption from process kills

**Baseline computation produces**:
- Per-rule firing rate (expected ≈ 1 per matching fault, 0 per non-matching)
- Per-rule p50/p95/p99 detection latency
- False positive count (rules that fired without matching fault in chaos log)
- Time-to-clear distribution

**Done when**: 14 consecutive nightly runs complete cleanly; baseline metrics stabilise.

#### T1-A-4 (Bv5) — Weekly review and curation

Once a week, the operator reviews:
- Detection rules that are firing too often → flag for tightening (Tier 2)
- Detection rules that never fire → flag for review (rule may be broken or fault may not exist in plan)
- Faults that don't produce expected detections → triage as bugs (Tier 2)
- Resource trends: RSS growth, disk growth, archive size

The review output is a short markdown weekly note in `docs/test_results/weekly_reviews/<date>.md`. Triggers targeted fixes (Tier 2 work) without stopping the chaos runner.

**Done when**: 4 weekly reviews completed (i.e. 4 weeks of chaos data); patterns visible in detection metrics.

#### T1-A-5 (Bv5) — Disk discipline on the laptop

The largest constraint on laptop-based gathering. Concrete numbers:
- 12-node lab at default rates: ~200 MB/day archive + ~500 MB/day logs = ~700 MB/day
- 30 days = ~21 GB
- Add 7-day log retention rotation: peak ~3.5 GB at any time

**Required actions before starting**:
- Confirm ≥ 30 GB free at archive path
- Confirm ≥ 5 GB free at log path (per `min_free_bytes` config)
- Set `[archive.retention_days]` to 60 (we want all data through GNN training)
- Set `[logging.retention_days]` to 7 (logs rotate; we don't need long log history)
- If laptop disk is constrained, mount external SSD or NAS at `~/bonsai/runtime/archive` and `~/bonsai/runtime/logs`

**Done when**: disk monitoring shows free space tracking expected consumption; no surprise disk-fill events.

### Path B: Cloud-Based Data Gathering (timeboxed)

**Optional. Run in parallel to Path A if free-tier provisioning succeeds. Kill if it fails.**

The cloud-spike infrastructure (Oracle Always Free, 6-node lab, daily GitHub sync, kill criteria) is built and ready. Per Bv4 plan: 5-day evaluation, then go/no-go decision.

#### T1-B-1 (Bv5) — Provision attempt (timeboxed at 1 day)

`bash scripts/cloud/oracle_setup.sh`

If provisioning fails (capacity unavailable, account verification issues, region restrictions), document the failure in `docs/test_results/cloud_spike/PROVISION_ATTEMPT_<date>.md` and **abandon Path B.** Continue with Path A only.

**Why timeboxed**: Oracle Always Free has unpredictable availability. Sometimes the ARM Always Free shape is "out of stock" for weeks. We do not engineer around this.

**Done when**: either VM is up with public IP and SSH access, OR the failure is documented and Path B is closed.

#### T1-B-2 (Bv5) — Cloud-init bootstrap

If T1-B-1 succeeds:

`bash scripts/cloud/deploy.sh` runs the full bring-up sequence on the fresh VM:
1. Install Docker + ContainerLab
2. Clone repo, build bonsai (or pull pre-built image)
3. Deploy 6-node cloud lab variant
4. Bring up bonsai-core + Prometheus + NetBox (no Splunk/Elastic to save resources)
5. Run `seed_external.sh` for NetBox seed
6. Start chaos runner with `chaos_plans/always_on_cloud_dc.yaml`

**Done when**: cloud chaos runner producing cycles; daily sync reaching GitHub.

**2026-05-08 status**: chaos runner is producing clean BGP/interface cycles after restart at `2026-05-08T15:36:57Z`; archive verification passes. Daily sync cron is installed, but GitHub push still needs a VM-side `GITHUB_TOKEN`.

#### T1-B-3 (Bv5) — Daily sync verification

The first 5 days are the spike's critical window. Each day, operator confirms:
- GitHub branch contains the previous day's archive snapshot
- Snapshot is readable and verifies cleanly
- VM stayed up (uptime > 24h since last incident)
- bonsai didn't crash (`bonsai_uptime_seconds` increasing)
- Chaos cycles completed at expected rate

**Done when**: 5 consecutive days of clean daily sync.

#### T1-B-4 (Bv5) — Day 5 go/no-go

Apply kill criteria from `docs/test_results/cloud_spike/KILL_CRITERIA.md`:
- Bonsai crashed > 1× per 24 hours → KILL
- Lab failed to stay up for 24h continuously → KILL
- Daily sync failed 2 consecutive days → KILL
- Free-tier resource limits hit → KILL
- After 5 days, archive doesn't differ meaningfully from laptop signal → KILL

If KILL: tear down VM (`oracle_setup.sh --destroy`); document findings; Path B closed.

If GO: extend to 30+ day continuous run; cloud archive becomes primary GNN training data.

**Done when**: 5-day spike report at `docs/test_results/cloud_spike/REPORT_<date>.md` captures decision + data.

#### T1-B-5 (Bv5) — Continuous operation (if GO)

Same shape as T1-A-3 / T1-A-4 but with daily GitHub sync instead of laptop-local archive verification. Operator effort: pull the daily snapshot, run baseline computation locally, weekly review against pulled data.

**Done when**: 30+ days of cloud archive synced to GitHub.

### Path resolution: which archive feeds GNN training?

If both paths succeed, **prefer cloud archive**. Reasons:
- Cloud has a clean, dedicated environment (no contention with operator daily computer use)
- Cloud chaos runner runs 24/7 reliably
- 6-node cloud lab has a simpler topology than 8-node laptop lab — cleaner ground truth

If only laptop succeeds, laptop archive feeds training. Smaller dataset but still sufficient at 30+ days.

If both fail (laptop disk fills, cloud spike killed), this is a project-level alarm. Stop Tier 2-3 work; investigate.

---

## <a id="tier-2"></a>TIER 2 — Parallel Work That Doesn't Wait

While archive accumulates (30+ days), engineering work proceeds on items that don't need real chaos data. **These are prioritised in execution order.**

### T2-1 (Bv5) — GNN data loader skeleton (can develop without data)

**What**: build the data loader against synthetic fixtures, not real archive. The loader's interface to the archive (Parquet reader, chaos log joiner, supervision label extractor) is defined and tested with mock data. When real archive arrives, the loader is ready.

**Why now**: zero dependency on chaos data; weeks of design work that can complete during accumulation.

**Where**: `python/bonsai_ml/gnn/data_loader.py` + `python/bonsai_ml/gnn/test_fixtures.py` (synthetic graph + chaos log).

**Done when**: loader produces valid PyTorch Geometric `Data` objects from synthetic input; unit tests pass; ready to drop in real archive once available.

### T2-2 (Bv5) — Detection rule baseline harness (no data needed for harness itself)

**What**: the harness for evaluating detection rules against archive. Reads chaos log + detection events, produces confusion matrix per rule. Today only `compute_detection_baselines.py` exists; flesh out into a proper evaluation harness with held-out splits, time-aware cross-validation, false-positive root-cause analysis.

**Why now**: critical for the comparison study (rule baseline vs tabular ML vs GNN). Takes time to build right; better to have it ready when archive matures.

**Where**: `python/bonsai_ml/eval/` directory (new).

**Done when**: harness runs end-to-end on synthetic chaos log + synthetic archive; produces structured evaluation report.

### T2-3 (Bv5) — Tabular ML detector evaluation harness ✅ Done

**What**: existing tabular ML detector code in collector_engine.py needs an evaluation harness like T2-2. We need apples-to-apples comparison across detector types.

**Why now**: same as T2-2 — comparison gates the GNN's "did it learn anything new" claim.

**Where**: `python/bonsai_ml/eval/tabular_ml.py`.

**Done when**: harness produces ML detector metrics in same format as rule baseline.

**2026-05-08 status**: done with synthetic fixtures and tests. The harness accepts timestamped feature windows, scores duck-typed sklearn-style models (`decision_function`, `predict_proba`, or `predict`), emits ML `DetectionEvent` rows, and reuses `RuleEvaluationReport` for TP/FP/FN/TN, precision/recall/F1, and latency metrics.

### T2-4 (Bv5) — Investigation agent productive use (when token budget arrives)

**What**: agent code exists but never runs against real investigations because of token-budget constraint. If/when budget appears, the agent becomes a Tier 2 deliverable. Token-spending happens against the running system; investigations land in `Investigations.svelte` UI; cost dashboard surfaces.

**Why now (or whenever)**: not gated by data; gated by operator allocating tokens. If allocated, immediate value.

**Done when**: 10 investigations completed against real chaos events; cost-per-investigation visible; agent reasoning trail readable in UI.

### T2-5 (Bv5) — Distributed mode against the lab

**What**: distributed mode (collector + core via gRPC mTLS) is coded but rarely operated. Spend 1 sprint running distributed-in-compose against the existing DC lab. Verify everything that works in monolithic also works distributed: enrichment, chaos detection, graph queries, embeddings.

**Why now**: validates the distributed code path before any K8s consideration. Low engineering risk; high confidence value.

**Where**: `docker/compose-distributed.yml` profile; runs alongside the chaos cycle.

**Done when**: 7 days of distributed-mode operation against chaos lab; metrics indistinguishable from monolithic.

**2026-05-09 status**: compose distributed validation has been realigned to the active `bonsai-dc` lab (`172.100.103.11-18`) and runs on alternate host ports (`3100`/`51051`) so it does not interrupt the monolithic laptop baseline on `3000`/`50051`. Two-collector smoke passed and the stack is running as Compose project `bonsai-distributed` for the validation window. During bring-up, distributed topology parity exposed a real bug: interface summaries from collectors created `Interface` nodes but did not run LLDP backfill, leaving `CONNECTED_TO` edges empty. Fixed in `src/graph/mod.rs`; post-fix distributed graph has 24 fabric edges.

### T2-6 (Bv5) — UI completion items (carryover from prior backlogs)

Bv1 Tier 6 items still pending. Build during data accumulation:
- **T6-1 operator path overrides UI workspace** (Bv1)
- **T6-2 subscription resolution audit** in DeviceDrawer (Bv1)
- **T2-3 mgmt-plane visibility toggle** (Bv3) — verify implementation if not already done

**Why now**: each is bounded scope, doesn't depend on chaos data.

### T2-7 (Bv5) — Documentation refresh (lowest priority)

`README.md` reflects Bv4 state, the cloud spike approach, the data-gathering operational discipline. CLAUDE.md / AGENTS.md updated for AI-agent consumption with current operational guidance.

**Why last**: docs over moving code produce incorrect docs. Land after Tier 1-2 stabilise.

### T2-8 (Bv5) — SP lab bring-up (after sufficient DC archive)

Once 30 days of DC archive accumulate (Tier 1 complete), bring up SP lab. **Do not start before DC archive completes.** Reason from Bv4: mixing DC and SP signal in early training data dilutes signal; better to validate generalisation later by training on DC, evaluating on SP.

---

## <a id="tier-3"></a>TIER 3 — GNN Training (gates on data depth)

Triggered when Tier 1 data conditions are met (≥30 days, ≥500 injections, ≥50 per rule, baselines stable, integrity clean).

### T3-1 (Bv5) — GNN training run

GraphSAGE or GAT, 2-3 layers, Device-node anomaly score. Train on 25 days, validate 5 days, test most recent day. Use Path A embeddings as initial node features.

**Where**: `python/bonsai_ml/gnn/train.py`. Builds on T2-1 data loader.

**Done when**: trained model exists; checkpoints + metadata saved.

### T3-2 (Bv5) — Comparison study: rules vs tabular ML vs GNN

Use harnesses from T2-2 + T2-3. Apply each to held-out test set. Confusion matrix:
- detected-by-GNN-only
- detected-by-rules-only
- detected-by-tabular-ML-only
- detected-by-multiple
- detected-by-none

**Done when**: comparison report at `docs/ml/gnn_evaluation_<date>.md` with honest numbers.

### T3-3 (Bv5) — Online inference path

If T3-2 shows GNN catches faults rules miss, deploy online. Graph snapshot every N seconds; GNN scores Devices; UI surfaces score; high-score detections become regular detections.

**Done when**: GNN scoring active in operations stack; UI shows scores alongside rule-based detections.

### T3-4 (Bv5) — Model card

Honest documentation. Algorithm, hyperparameters, data, evaluation, limitations explicit, recommended-use boundaries.

---

## <a id="carryover"></a>Carryover from Bv4

Items remaining valid; deferred behind Tier 1-3:

- **Investigation agent productive use** (post-MVP, pending token budget) — see T2-4
- **HIL graduated remediation** in production
- **Output adapter productive use** (Splunk/Elastic against real receivers)
- **Signals tier** (syslog/traps)
- **Controller adapter implementations** (demand-driven)
- **Catalogue plugin install command**
- **AIOps readiness checklist**
- **NL query, bulk CSV onboarding, scale architecture, S3 archive**
- **Campus topology**
- **Bitemporal schema, schema migration, Grafeo evaluation**

Plus the Bv2 hardcoding catalogue (H-1 through H-12) — most addressed by Bv3-Bv4 work; remainder opportunistic.

---

## <a id="execution-order"></a>Execution Order

The execution shape is unusual: Tier 1 is **wall-clock-time-dominated**, Tier 2 is **engineering-time-dominated**, and they run in parallel.

### Day 0-1 (immediate)
1. T1-A-1 operational baseline (24-hour quiet run starts)
2. (Parallel) T1-B-1 cloud provisioning attempt (timeboxed 1 day)

### Day 1-2
3. T1-A-2 start always-on chaos cycle (laptop)
4. (If T1-B-1 succeeded) T1-B-2 cloud-init bootstrap
5. (Parallel) T2-1 GNN data loader skeleton (engineering work begins)

### Day 2-7 (parallel tracks)
- **Operational track**: T1-A-3 daily verification, T1-B-3 cloud daily sync verification
- **Engineering track**: T2-1 + T2-2 + T2-3 evaluation harnesses

### Day 5
6. T1-B-4 cloud spike go/no-go decision

### Day 7-30 (chaos runs continuously, engineering proceeds)
- **Operational track**: T1-A-4 weekly reviews, T1-B-5 continuous cloud (if GO)
- **Engineering track**: T2-4 (if tokens), T2-5 distributed mode, T2-6 UI completion

### Day 30+
7. Trigger condition assessment for Tier 3
8. T3-1 GNN training (if ready)

### After GNN training
9. T3-2 comparison study
10. T3-3 online inference
11. T3-4 model card
12. T2-8 SP lab bring-up
13. T2-7 documentation refresh

### Estimated total
- 6-8 weeks to MVP-with-detection-validated (when baselines stabilise)
- 8-10 weeks to GNN trained and deployed (assuming chaos runs cleanly from Day 1)
- Cloud spike accelerates by ~2 weeks if it succeeds; laptop-only path still reaches the same destination

---

## <a id="guardrails"></a>Guardrails

### New in Bv5

- **Data accumulation does not pause for engineering.** Once chaos runner starts, it stays running until the archive depth trigger. Engineering work on Tier 2 must not require stopping the chaos cycle.
- **Cloud spike is timeboxed and kill-criteria-driven.** Do not pour effort into making provisioning work; if Oracle is out of capacity, document and move on.
- **Chaos plan is immutable mid-run.** Plan changes invalidate accumulated data. Plan iterations happen between major archive epochs (e.g. before a fresh 30-day run), not during.
- **Archive growth is tracked daily.** Disk-fill prevention is operational, not theoretical. If laptop disk free space drops below 10 GB, halt chaos immediately.
- **Engineering work does not gate on perfect data.** T2-1 / T2-2 / T2-3 use synthetic fixtures so they're ready when archive matures. Don't wait.

### Unchanged from v7-Bv4

All prior architectural invariants and discipline continue. Reference earlier backlogs.

### Anti-patterns to reject

- "Restart chaos runner because we changed a rule" — no, finish this archive epoch first
- "Cloud is preferred therefore wait until we have it" — no, laptop runs in parallel from Day 0
- "GNN can train on 7 days of data" — no, 30 days minimum (per Bv4 guardrail)
- "Skip the comparison study, the GNN is obviously better" — no, honest evaluation is required for the model card
- "Add new chaos types now that we see what's missing" — between epochs, not during

---

## What Bv5 Explicitly Excludes

- New functional features beyond chaos infrastructure + GNN training
- K8s deployment artefacts (defer until distributed mode validated in Bv5 T2-5)
- Investigation agent productive use without token budget
- Signals tier
- Controller adapters
- All Bv3 Tier 6 strategic carryover items

---

*Bv5.0 — authored 2026-05-07 after end-to-end review of post-Bv4 main. Confirms all 9 Bv4 architectural concerns landed cleanly (C-1 batch always-COMMIT, C-2 single-bus, C-3 BroadcastSubscriber for true DropOldest, C-4 Arc<TelemetryUpdate>, C-5 ArcSwap lock-free reads, C-6 16-shard ingest cache, C-7 RAM-budgeted caps, C-8 RollingFileAppender, C-9 counter mode in Operations UI). Establishes the data-gathering playbook with two parallel paths: laptop (always available, T1-A-1 through T1-A-5) and cloud (timeboxed evaluation, T1-B-1 through T1-B-5 with kill criteria). Specifies what "good chaos data" means, quantity targets (30 days / 500 injections / 50 per rule), monitoring at three time scales, trigger conditions for GNN training. Tier 2 lists prioritised parallel engineering work that doesn't wait for data: GNN data loader on synthetic fixtures, evaluation harnesses for rules + tabular ML + GNN, investigation agent (when tokens arrive), distributed mode validation, UI completion. Tier 3 GNN training gates on archive depth. MVP readiness: ~85%. Northstar readiness: ~10% by code, time-dominated by data accumulation. Estimated 6-8 weeks to MVP, 8-10 weeks to GNN deployed. References v7-Bv4 for unchanged context; Bv2-mod for MVP/northstar definitions.*
