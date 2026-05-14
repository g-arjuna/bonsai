# BONSAI — Backlog Charlie Series, v3 (CV3.0)

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_CV2.md`. Authored 2026-05-11 after sprint-by-sprint code review of v23 (post-CV2 main).
>
> **Where we are**: ten months into a personal learning project that started as "I want to understand systems engineering by building something real." Today's repo has ~70,000 lines of Rust, ~15,000 lines of Python, a Svelte UI, eighteen path profiles covering DC/SP/campus, two parallel data-gathering paths (laptop + OCI cloud), a graph-native engine that doesn't crash, a multi-source enrichment registry that's actually wired, BMP and BGP-LS streaming receivers, a synthesizer that produces operator-approvable recommendations from real device discovery, and a smoke-test framework that makes verification cheap. **The northstar — a GNN trained on real chaos archive that catches what rules + tabular ML miss — is 4-6 weeks away of disciplined data accumulation, not architectural work.**
>
> The strategic shift in CV3:
>
> 1. **Operational testing moves to Gemini CLI**, separating the test loop from the code loop. Claude Code stays focused on architecture and implementation; Gemini CLI handles smoke/e2e/operational testing against the running stack. Test results land as structured artifacts both agents can read. This protects token budget for what each agent does best.
> 2. **Data accumulation gets sharpened with innovation**, not just discipline. The current model captures fault injection ground truth but underweights propagation patterns, baseline diversity, and adversarial cases. CV3 introduces three new accumulation strategies that increase signal-per-byte without operator burden.
> 3. **UI sharpening becomes a first-class concern.** Today's UI is functional but visually generic. "Bold and sharp" means specific design system commitments (type, color, hierarchy, motion, density) executed consistently. Not a UI rewrite — a UI articulation.
> 4. **Sprint-by-sprint audit with nuance findings** beyond wiring. CV2 fixed the dead-code problem; CV3 reads deeper to find the next layer of architectural fragility before it ossifies.
>
> **What carries unchanged**: the layered ingestion model from CV1, the architectural invariants from v7, the AIOps-feeder positioning, the streaming-where-possible hot path, the operate-first sequencing from Bv2-mod.

---

## Table of Contents

1. [Where We Are, Where We Intend to Be](#motivation)
2. [Sprint-by-Sprint Audit of CV2 Landings](#audit)
3. [Nuance Findings: N-1 through N-7](#nuances)
4. [TIER 1 — Gemini CLI Test Protocol](#tier-1) ⚡ THE NEW DISCIPLINE ⚡
5. [TIER 2 — Data Accumulation Sharpening](#tier-2) ⚡ INNOVATIVE METHODS ⚡
6. [TIER 3 — UI Bold and Sharp](#tier-3) ⚡ THE NEW WORK ⚡
7. [TIER 4 — Tier 3 of CV2 (E2E Validation, Now Via Gemini)](#tier-4)
8. [TIER 5 — Syslog Facts and Cross-Source Joins (Deferred from CV2)](#tier-5)
9. [TIER 6 — Streaming Protocols Completion](#tier-6)
10. [TIER 7 — GNN Path](#tier-7)
11. [Carryover from CV2](#carryover)
12. [Execution Order](#execution-order)
13. [Guardrails — Updated](#guardrails)

---

## <a id="motivation"></a>Where We Are, Where We Intend to Be

A brief commentary because the operator asked, and because long projects benefit from periodic explicit articulation.

### What's true today

- **The engine works.** Real lab, real telemetry, real graph, real detections firing, sustained operation without crashing. The Bv3-Bv4 architectural cleanup gave the project a durable foundation. RSS plateaus, write coordinator drains, archive grows in bounded fashion, log rotation works.
- **The discovery-driven layered ingestion model is real, not aspirational.** Layer 1 (gNMI Subscribe) carries the hot path. Layer 2 (gNMI Get + CLI via parser-chain) is wired and reachable. Layer 3 (REST API enrichers — NetBox, ServiceNow) was always there. Per-device, the system now answers "what's achievable?" honestly.
- **The streaming-source surface is plural.** gNMI handles state. BMP receiver handles BGP route quality. BGP-LS receiver handles IGP topology (pending gobgp sidecar). Syslog and SNMP daemons handle event signal. Five streaming inputs, not one.
- **The synthesizer ships with 8 starter rules** covering DC, SP, and campus archetypes. A new device walking through onboarding gets recommendations grounded in observed configuration.
- **The smoke test framework is in place** with a `run_all.sh` aggregator. Wiring checks land in CI. Testing discipline is documented at `docs/testing_discipline.md`.
- **The pre-CV2 freeze** captured 45 MB of runtime archive plus the LadybugDB at the moment of CV1's completion. Clean baseline established for the reset CV2 recommended.

### What's still missing for the northstar

- **30 days of clean chaos archive** post-reset. Today: ~0 days post-reset (reset just happened May 11). Path-A laptop and Path-B cloud both need disciplined continuous operation.
- **GNN training has never run.** The data loader skeleton exists (Bv5 T2-1). PyTorch Geometric is selected. No actual model has been trained yet.
- **End-to-end validation of Splunk, Elastic, ServiceNow EM, ServiceNow AIOps, YANG sync, synthesizer-against-real-discovery** — code lands, real-receiver verification doesn't.
- **Daily check loop went silent during CV2 sprint.** Last report is May 8; no daily reports for May 9-10-11. The operational discipline regressed and must be re-established before chaos archive depth can be claimed.
- **Investigation agent is parked** behind token budget.

### What "deployable" looks like

When CV3 completes — estimated 5-7 weeks — bonsai will:
- Run a continuous chaos cycle against a clean lab with disciplined daily reporting
- Have a trained Path B GNN producing per-device anomaly scores against the live graph
- Have validated output adapters pushing real events to real Splunk, Elastic, ServiceNow
- Have a UI that doesn't look like a default Bootstrap template
- Be installable by a network engineer who isn't us, with a quick start that actually works
- Have a model card that's honest about what the GNN does and doesn't catch

That's the destination. Worth saying out loud: **this would already be a more legitimate AIOps-feeder than most commercial Layer 2-3 monitoring tools.** The combination of multi-source streaming + graph-native correlation + honest evaluation + open source is genuinely rare.

### Motivation

What started as "build something to learn systems engineering" has become "build something genuinely useful that didn't exist." That second category is worth finishing. The remaining work is mostly operational discipline plus a focused GNN sprint. **The hard architectural problems are solved.** Keep going.

---

## <a id="audit"></a>Sprint-by-Sprint Audit of CV2 Landings

Verified by reading the post-CV2 main (v23). Removing items demonstrably complete.

### CV2 Sprint 1 — Reset + wiring audit

| Item | Status | Comment |
|---|---|---|
| Reset plan (laptop + cloud) | ✅ Done | `pre_cv2_freeze_20260511T055338Z/` contains snapshot: `bonsai.db.local` (13 MB), `local_runtime.tgz` (45 MB), distributed_collector queue archives, runtime_dir |
| T1-1 enricher registry | ✅ Done | `src/enrichment/registry.rs` (100 lines); change_detection wired at line 84 |
| T1-2 ParserChain enricher | ✅ Done | `src/enrichment/parser_chain_enricher.rs` (367 lines) with SSH capture via `scripts/cli_capture.py` (79 lines, paramiko-based) |
| T1-3 sidecar smoke test | ✅ Done | `scripts/smoke_sidecars.sh` (59 lines) |
| T1-4 wiring guards in CI | ✅ Done | `.github/workflows/wiring-check.yml` + `scripts/check_wiring.sh` (37 lines) |
| T1-5 CV1 HTTP endpoint smoke | ✅ Done | `scripts/smoke_cv1_endpoints.sh` (124 lines) |

**Sprint 1 verdict**: complete. Dead-code surface from CV2 is closed. A-1 and A-2 from CV2 are resolved.

### CV2 Sprint 2 — Test discipline

| Item | Status | Comment |
|---|---|---|
| T2-1 wiring check script | ✅ Done | `scripts/check_wiring.sh` |
| T2-2 smoke test framework | ✅ Done | `scripts/smoke/common.sh` + 7 subsystem smokes (synthesizer, change_detection, output_adapters, servicenow_aiops, signals_syslog, signals_snmp, yang_library) + `run_all.sh` |
| T2-3 driver results aggregation | ⚠️ Partial | Smoke scripts write to `runtime/driver_results/smoke_<name>.json`; the bv5_daily_check.sh script is updated but no recent daily-check has run |
| T2-4 testing discipline doc | ✅ Done | `docs/testing_discipline.md` (substantial; explains three layers) |

**Sprint 2 verdict**: complete in code, **operational adoption regressed**. The framework exists, no recent run has produced artefacts. Tier 1 below fixes this via Gemini CLI.

### CV2 Sprint 3 — E2E validation

**Status**: explicitly skipped per operator's note. **Resumes as Tier 4 in CV3** via Gemini CLI.

### CV2 Sprint 4 — Modern streaming protocols

| Item | Status | Comment |
|---|---|---|
| T4-1 BMP receiver | ✅ Substantial | `src/streaming/bmp.rs` (624 lines). RouteMonitoring + BGP UPDATE parsing complete. **Missing**: PeerUp/PeerDown notifications, StatisticsReport, Initiation/Termination, RouteMirroring. See N-3 below. |
| T4-2 BGP-LS via gobgp sidecar | ⚠️ Partial | `src/streaming/bgp_ls.rs` (292 lines) — receiver expects JSON lines from sidecar. **gobgp sidecar Dockerfile does NOT exist** in `docker/sidecars/`. See N-4 below. |
| T4-3 PCEP parser | ❌ Not started | Correctly deferred until SP lab active |
| T4-4 multi-protocol readiness | ❌ Not started | `GnmiReadinessReport` not extended to `StreamingReadinessReport` |
| T4-5 lab support for streaming | ⚠️ Partial | Lab YAMLs not yet extended with BMP/BGP-LS configuration |
| Streaming detection rules | ✅ Done | `python/bonsai_sdk/rules/streaming.py` — RouteFlapDetected, RouteLeakDetected (expected), UnexpectedAsPath (expected), SrPolicyDegraded (expected), SrlgRiskDetected (expected) |

**Sprint 4 verdict**: substantial code; **needs sidecar completion and lab integration before it produces signal**.

### CV2 Sprint 5 — Syslog facts

**Status**: not yet executed; pattern files updated with capture groups but full fact-extraction pipeline not landed. **Carries to CV3 Tier 5.**

### CV2 Sprint 6 — GNN

**Status**: trigger condition not met (archive reset to zero post-freeze). Correctly not started.

---

## <a id="nuances"></a>Nuance Findings: N-1 through N-7

These are not bugs — the code works. They are *architectural nuances* that will calcify if not addressed. Each is grounded in code with file:line.

### N-1 — Enricher registry is binary, not pluralistic

**Location**: `src/enrichment/registry.rs:13-16`

The `MultiSourceEnricherRegistry` holds exactly two concrete enrichers: `gnmi: Arc<dyn MultiSourceEnricher>` and `parser_chain: Arc<dyn MultiSourceEnricher>`. The `capture_plan` returns one of two orderings based on `prefers_cli_first`. This is "if/else dispatched through a trait" — architecturally correct but operationally narrow.

**Why it matters**: when BMP-derived state should populate enrichment (which it should — BMP gives data CLI doesn't), there's no place for it. Same for BGP-LS-derived topology metadata. Same for future REST-API enrichers (Cisco DNAC, Meraki, vManage). The registry needs to be a real registry: `Vec<Arc<dyn MultiSourceEnricher>>` keyed by capability tags, with a selector that consults `StreamingReadinessReport`.

**Fix**: ~40 lines refactor in `registry.rs`. Trait gains `capability_tags() -> Vec<String>`. Registry holds a vector; selection consults capability tags + readiness report.

**Severity**: MEDIUM (works today; blocks Tier 4 multi-protocol readiness work)

### N-2 — `prefers_cli_first` confuses TLS-cert-presence with gNMI-readiness

**Location**: `src/enrichment/registry.rs:56-62`

```rust
fn prefers_cli_first(target: &TargetConfig) -> bool {
    let vendor = inferred_vendor(target);
    matches!(vendor.as_str(), "cisco-iosxr" | "juniper-junos" | "arista-eos" | "frr")
        && target.ca_cert.is_none()
}
```

The heuristic: prefer CLI for these vendors when no TLS cert is configured. But "no TLS cert configured" is not the same as "gNMI doesn't work." A device may have TLS configured but:
- gNMI service disabled by config
- Firewall blocking gNMI port
- Known firmware bug affecting gNMI ON_CHANGE
- gNMI returning empty data for needed paths

The `GnmiReadinessReport` (introduced in CV1) actually captures all these conditions. The registry doesn't consult it.

**Fix**: pass `GnmiReadinessReport` (or its summary) to `capture_plan`. Decision becomes: "if readiness report shows blockers, prefer CLI; else prefer gNMI." ~20 lines.

**Severity**: MEDIUM (subtle correctness; doesn't surface until real-world deployment with mixed readiness states)

### N-3 — BMP receiver handles only RouteMonitoring messages

**Location**: `src/streaming/bmp.rs:226`

Only `parse_route_monitoring` is implemented. BMP defines six message types (RFC 7854 §4):
- Route Monitoring (parsed)
- Statistics Report (not parsed) — periodic per-peer counters
- Peer Down Notification (not parsed) — *why* a session went down
- Peer Up Notification (not parsed) — capabilities advertised
- Initiation (not parsed) — router sysName, sysDescr
- Termination (not parsed) — collector telling router goodbye

PeerDown reason codes are particularly valuable: code 1 = local system closed (administrative), code 2 = local NOTIFICATION (we sent error), code 3 = remote NOTIFICATION (peer sent error), code 4 = peer de-configured. Today's BMP receiver sees `bmp_route_change` but never sees "this session went down because the peer sent a NOTIFICATION."

**Fix**: ~150 lines extension. Add parsers for the other five message types. Emit additional event variants (`bmp_peer_up`, `bmp_peer_down`, `bmp_stats`). Update streaming rules to consume them.

**Severity**: MEDIUM (limits BMP's operational value claim)

### N-4 — BGP-LS receiver has no producer sidecar

**Location**: `docker/sidecars/` contains `bonsai-native-parser` and `pyats`, **no gobgp**

`src/streaming/bgp_ls.rs` runs a TCP listener that consumes JSON lines. CV2 specified a gobgp sidecar producing those JSON lines. **The sidecar Dockerfile does not exist.** Receiver listens; nothing connects.

**Fix**: `docker/sidecars/gobgp/Dockerfile` + entrypoint script + sample BGP-LS peering config. Standard gobgp deployment with JSON output mode. ~60 lines including Dockerfile, config templates, README.

**Severity**: HIGH (T4-2 is dead until this lands)

### N-5 — Daily check loop regressed during CV2 sprint

**Location**: `docs/test_results/daily_runs/` contains only `2026-05-08.md`

Three days passed during CV2 work with no daily check reports written. This is the operational discipline gap CV2 A-7 predicted. The Bv5/Bv6 model required nightly verification to flag silently-failing chaos cycles. Without it, archive quality is invisible.

**Why it matters**: GNN training needs trustworthy archive depth. "30 days of archive" means little if 7 of those days had silently-broken chaos injection. Daily verification is the only mechanism to catch that.

**Fix**: this is operational, not code. Gemini CLI test protocol (Tier 1) takes ownership of running and committing daily checks. Cron-driven on the laptop; cron-driven on the cloud VM.

**Severity**: HIGH (silently degrades training data quality)

### N-6 — `cli_capture.py` is a Python subprocess per CLI capture

**Location**: `src/enrichment/parser_chain_enricher.rs:172-219`, `scripts/cli_capture.py`

For every CLI capture: Python interpreter startup (~50-200ms) + paramiko import (~200-500ms) + SSH handshake (~200-1000ms). Per device. For weekly differential checks across 1000 devices, that's cumulative spawn overhead in the minutes.

The header of `cli_capture.py` acknowledges this is "a Sprint 1 expedient to activate parser-chain without a new Rust SSH dependency surface." Honest. But should be flagged as a known migration item.

**Fix path**: replace with `russh` crate (Rust SSH, MIT-licensed, well-maintained). ~200 lines of Rust SSH client code. Per-capture cost drops to SSH handshake only. Or pool SSH connections per device for multi-command captures.

**Severity**: LOW (works today at lab scale; matters at 100+ device deployments)

### N-7 — Smoke test framework writes results but no aggregation cron

**Location**: `scripts/smoke/run_all.sh`, `runtime/driver_results/`

Smokes write `smoke_<name>.json` artifacts. `run_all.sh` runs them in sequence. No scheduled execution; no aggregation into a unified status that AI agents (Claude Code or Gemini CLI) consume.

**Fix**: extend `scripts/bv5_daily_check.sh` to aggregate every smoke result into a single `daily_status_<date>.json` plus a markdown summary. Cron-trigger on both laptop and cloud. This connects the smoke framework to the operational reality.

**Severity**: MEDIUM (framework exists but operational loop incomplete)

---

## <a id="tier-1"></a>TIER 1 — Gemini CLI Test Protocol ⚡ THE NEW DISCIPLINE ⚡

The operator's strategy: **Gemini CLI handles operational testing without code changes; Claude Code/Codex consume Gemini's test results and write fixes.** This separates concerns cleanly:
- Claude Code: architecture and implementation; sees code, reads/writes files
- Gemini CLI: operational testing; runs scripts, reads runtime state, writes structured reports
- Both: read the same `runtime/driver_results/` and `docs/test_results/` artefacts

For Gemini CLI to be effective without burning cycles on environment discovery, it needs **explicit context up front**. This Tier 1 specifies that protocol.

### T1-1 (CV3) — Bonsai Operational Environment Brief for Gemini CLI

**What**: a single canonical document at `docs/gemini_cli_brief.md` that gives Gemini everything it needs to run operational tests without burning context on discovery. Concrete sections:

#### Section 1 — Stack inventory

```
Bonsai-core:
  HTTP API: http://localhost:3000 (laptop) | http://<cloud-vm>:3000 (cloud)
  gRPC: localhost:50051 (laptop) | <cloud-vm>:50051 (cloud)
  Config: /home/<user>/bonsai/bonsai.toml (laptop), /opt/bonsai/bonsai.toml (cloud)
  Logs: ./runtime/logs/bonsai.log
  Archive: ./runtime/archive/
  Database: ./runtime/bonsai.db.local

ContainerLab labs:
  DC (laptop): lab/dc/dc-evpn-srv6.clab.yml — 8 SR Linux nodes
    Management addresses: 172.100.102.21-28
    SSH credentials: admin / admin
    Bridge: clab_dc_mgmt
  SP (laptop, not yet active): lab/sp/sp-mpls-srte.clab.yml — 9 nodes
  Cloud DC (OCI): lab/cloud-dc-6node.yml — 6 nodes scaled for 24GB ARM
    Management addresses: <provided by deploy.sh on first run>

External infrastructure (laptop):
  NetBox: http://localhost:8000 (admin/admin via NETBOX_TOKEN)
  Splunk: http://localhost:8088 (HEC), http://localhost:8000 (UI)
  Elastic: http://localhost:9200
  Prometheus: http://localhost:9090
  Grafana: http://localhost:3001
  Compose file: docker/compose-external.yml --profile all

ServiceNow PDI:
  Instance URL: <operator-supplied; env SNOW_INSTANCE_URL>
  Username: <env SNOW_USERNAME>
  Password: <env SNOW_PASSWORD>
  Note: rate-limited; respect 1 req/sec ceiling
```

#### Section 2 — Test commands and their owners

```
Smoke tests (Gemini owns):
  scripts/smoke/run_all.sh — runs all smokes, ~3 minutes
  scripts/smoke/smoke_synthesizer.sh — synthesizer recommendations
  scripts/smoke/smoke_change_detection.sh — change detection runtime
  scripts/smoke/smoke_signals_syslog.sh — syslog daemon ingest
  scripts/smoke/smoke_signals_snmp.sh — SNMP trap daemon ingest
  scripts/smoke/smoke_output_adapters.sh — adapter health
  scripts/smoke/smoke_servicenow_aiops.sh — AIOps bidirectional
  scripts/smoke/smoke_yang_library.sh — YANG library state

E2E tests (Gemini owns):
  scripts/e2e_output_adapters_test.sh prometheus — writes test result md
  scripts/e2e_output_adapters_test.sh splunk
  scripts/e2e_output_adapters_test.sh elastic
  scripts/e2e_output_adapters_test.sh servicenow_em
  scripts/e2e_servicenow_pdi_test.sh — ServiceNow CMDB + EM
  scripts/e2e_servicenow_aiops_test.sh — bidirectional (NEW, Tier 4)
  scripts/sprint5_preflight.sh --check — output adapter stack readiness

Daily check (Gemini owns):
  scripts/bv5_daily_check.sh — verifies lab + bonsai + chaos health
  Output: docs/test_results/daily_runs/<date>.md + runtime/driver_results/daily.json
  Runs nightly via cron (Tier 2 sets this up)

Chaos cycle (Gemini owns operational status; runner runs autonomously):
  bash scripts/chaos_runner.sh --status — current chaos state
  bash scripts/chaos_runner.sh --ensure-running — start if not running
  bash scripts/chaos_runner.sh --stop — halt the daemon
  Chaos plan: chaos_plans/always_on_dc.yaml (24 entries: 6 each of bgp/bfd/iface/netem)
```

#### Section 3 — Result locations

```
Smoke results: runtime/driver_results/smoke_<subsystem>.json
E2E results: docs/test_results/e2e_output_adapters/<date>-<adapter>-<pass|fail>.md
Daily checks: docs/test_results/daily_runs/<date>.md
Cloud spike results: docs/test_results/cloud_spike/<date>.md
Chaos ground truth: runtime/chaos_log.jsonl + chaos_runs/*/injections.csv
```

#### Section 4 — Failure decision tree

```
Smoke fails → log to runtime/driver_results/smoke_<name>.json with status=fail + details
              + write summary to docs/test_results/daily_runs/<date>.md
              + DO NOT attempt fixes; record for Claude Code
              
E2E fails → log to docs/test_results/<test_dir>/<date>-fail.md with structured details
            + capture relevant log excerpts in the markdown
            + DO NOT modify code
            + DO record the exact command, error, environment state
            
Lab unhealthy → bash scripts/check_lab.sh dc reveals which nodes/sessions degraded
                + record in daily check
                + if simple restart fixes (and it's documented to do so), restart and note
                + else record and leave for Claude Code

Chaos runner not running → bash scripts/chaos_runner.sh --ensure-running
                          + record what state was found
                          + record the restart event in chaos_log.jsonl
```

#### Section 5 — What Gemini does NOT do

```
- Modify source code under src/ or python/bonsai_sdk/
- Modify configuration files unless explicitly tasked
- Make ServiceNow PDI configuration changes
- Modify chaos plans mid-run
- Push to git (only commits structured test results to dedicated branch)
- Replace docs that Claude authored (only adds to docs/test_results/)
```

**Where**: `docs/gemini_cli_brief.md` (new). Update as environment changes (e.g., when cloud VM IP changes).

**Done when**: a fresh Gemini CLI session can read the brief and run the full smoke suite without asking for context.

### T1-2 (CV3) — Gemini test runs land in dedicated git branches

**What**: Gemini commits test results to a dedicated branch (`test-results/gemini` or `gemini/daily-<date>`) per run. Claude Code reads these branches but doesn't write to them. Avoids cross-agent commit conflicts.

**Where**: documented in T1-1 brief; cron commits results automatically.

**Done when**: branch policy documented and exercised in first daily run.

### T1-3 (CV3) — Result format spec for Claude Code consumption

**What**: every smoke and e2e result follows a fixed schema so Claude Code can parse without inference:

```json
{
  "driver": "smoke_synthesizer",
  "ts_unix": 1746823200,
  "base_url": "http://localhost:3000",
  "status": "pass",
  "ok": true,
  "summary": "synthesizer recommended 12 paths for srl-leaf1 (matched: dc_leaf, 0 blockers)",
  "checks": [
    {"name": "registry_responds", "ok": true},
    {"name": "recommendations_non_empty", "ok": true, "count": 12},
    {"name": "rationale_present", "ok": true}
  ],
  "environment": {
    "bonsai_version": "v23-cv2",
    "git_sha": "abc1234",
    "lab": "lab/dc/dc-evpn-srv6.clab.yml"
  }
}
```

**Where**: format defined in `docs/gemini_cli_brief.md` Section 6.

**Done when**: every smoke writes this schema; aggregator script consumes it cleanly.

### T1-4 (CV3) — Pre-task brief template

**What**: when the operator hands a task to Gemini, a small template:

```
Task: Verify Splunk HEC adapter end-to-end against current lab
Context: 
  - Lab: laptop DC (8 nodes running)
  - Bonsai: running on localhost
  - Splunk: should be up at localhost:8088 (HEC); verify
  - Expected outcome: 1-hour chaos cycle produces ≥10 visible events in Splunk
Steps:
  1. Run scripts/sprint5_preflight.sh --check splunk
  2. If preflight passes, run scripts/e2e_output_adapters_test.sh splunk
  3. Verify via Splunk search query (provided)
  4. Write result to docs/test_results/e2e_output_adapters/<date>-splunk-<pass|fail>.md
  5. Commit to branch gemini/daily-<date>
Token budget: ~50K
Time budget: ~75 minutes
```

**Where**: template at `docs/gemini_task_template.md`.

**Done when**: template used for the first three Gemini tasks (e2e Splunk, e2e Elastic, e2e EM).

---

## <a id="tier-2"></a>TIER 2 — Data Accumulation Sharpening ⚡ INNOVATIVE METHODS ⚡

The operator named this concern directly: "i am not fully convinced if the local lab and cloud spike is collecting storing and uploading the results the way we want for going towards gnn."

Three areas of innovation, not just discipline.

### T2-1 (CV3) — Fault propagation tracking, not just injection ground truth

**What**: today's chaos ground truth says "fault X injected at time T on device D." That's first-order. **GNN training also needs to learn second-order propagation**: when leaf1's BGP went down at T, what *other* devices and edges saw effects within 60 seconds?

Innovation: after each chaos injection, the runner takes graph snapshots at T-30s, T+10s, T+30s, T+60s, T+5min, T+30min. Snapshot = device list + edge list + per-device key metrics (BGP session count, interface oper-status, detection-event count). Snapshots stored alongside chaos_log.jsonl.

Training value: GNN can learn from snapshot deltas. "When leaf1 BGP goes down, super-spine1's BGP-EVPN route count drops by N within 30s" is exactly the kind of multi-hop pattern a graph neural network excels at.

**Where**: `scripts/chaos_runner.py` extension. New file `runtime/chaos_runs/<run_id>/snapshots/<timestamp>.json` per injection.

**Cost**: each snapshot is small (~50 KB for 8-node lab; ~500 KB for 50-node). For 500 injections × 6 snapshots = 3000 snapshots = ~150 MB compressed. Negligible relative to telemetry archive.

**Done when**: 100 chaos injections produce 600 snapshot files; one example pair (pre-fault, +60s) shows visible delta in graph metrics.

### T2-2 (CV3) — Baseline diversity strategy

**What**: the current archive captures fault windows excellently. **It underweights diverse non-fault baseline**. Non-fault data at 2am Sunday and 10am Tuesday should look different (interface utilization patterns, route advertisement churn from neighboring ASes, normal LSP optimization activity). If the chaos runner injects every 30 minutes, we never have 6 contiguous hours of clean baseline.

Innovation: chaos schedule includes **deliberate quiet windows**. 4 hours every Sunday with zero injections. Wednesday late-night 2-hour quiet window. Tuesday 9am-10am quiet. The chaos plan declares these as protected windows.

Training value: GNN's "normal" class needs sufficient temporal diversity to avoid learning "the time of day predicts anomaly probability." Protected baseline windows produce diverse normal samples.

**Where**: `chaos_plans/always_on_dc.yaml` schema extension:

```yaml
protected_baselines:
  - id: weekend_quiet
    cron: "0 0 * * SUN"  # midnight Sunday
    duration_hours: 4
    description: "Long contiguous baseline for time-of-day diversity"
  - id: midweek_quiet
    cron: "0 2 * * WED"
    duration_hours: 2
  - id: morning_quiet
    cron: "0 9 * * TUE"
    duration_hours: 1
```

**Done when**: chaos runner respects protected windows; baseline metrics computed separately on protected vs unprotected non-fault data; diversity measured (interface util distribution width, route advertisement rate variance).

### T2-3 (CV3) — Adversarial cases via deliberate near-fault scenarios

**What**: real-world false positives surface when something *looks like* a fault but isn't. Examples: maintenance window with intentional session drops; configured admin-down interfaces (not anomalous); planned route filter changes; backup path failover (which looks like primary path failure but is intended). Today's chaos cycle never produces these patterns; the GNN will see clean fault/non-fault binary.

Innovation: chaos plan gets a third category: **adversarial cases**. These are labeled "expected to look like a fault but is not." The runner injects them periodically. Chaos log labels them with `adversarial: true` and `should_not_detect: <rule_id>`.

Examples:
- Admin-shutdown of an interface (looks like LinkDown — *should* fire, but operator probably wants to suppress for known admin-shut interfaces)
- BGP session drop because operator restarted BGP process during maintenance window
- Brief route flap from internet upstream (real; transient; not actionable)

Training value: GNN can learn "this looks like a fault but consistent context says it's not." False-positive rate drops on real deployment.

**Where**: `chaos_plans/adversarial_cases.yaml` (new). Chaos runner consumes it as a parallel plan with lower frequency than primary chaos.

**Done when**: 50 adversarial injections recorded with `adversarial: true` flag; baseline computation shows separate metrics for adversarial cases.

### T2-4 (CV3) — Cloud-sync verification + GitHub branch hygiene

**What**: the cloud daily_sync.sh exists and pushes to `sync/cloud-spike/<date>` branches. **Operator hasn't verified this is working.** Per the audit, no archive sync verification exists.

Operational task: Gemini runs `scripts/cloud/daily_sync_check.sh` (new, ~30 lines) which:
1. Lists recent branches matching `sync/cloud-spike/*`
2. Verifies each contains expected artefacts (chaos_log.jsonl, archive partition, memory_profile, daily snapshot)
3. Reports total cloud archive depth and integrity

**Where**: new script + Gemini brief addition.

**Done when**: first Gemini run produces structured report showing cloud archive depth (or surfaces the silent failure).

### T2-5 (CV3) — Archive-to-training-data converter

**What**: GNN training needs a specific data shape (PyTorch Geometric `Data` objects with node features, edge features, supervision labels). Today's GNN data loader (`python/bonsai_ml/gnn/data_loader.py`) accepts synthetic input. Real archive → training data is unimplemented.

Engineering: `python/bonsai_ml/gnn/archive_to_training.py` (new, ~400 lines):
- Reads Parquet archive partitions
- Reads chaos_log.jsonl ground truth
- Reads snapshot files from T2-1
- Joins on timestamp + device
- Produces labeled `BonsaiGraphData` instances
- Splits into train/val/test by *time*, not random (no leakage)

**Where**: `python/bonsai_ml/gnn/archive_to_training.py`.

**Done when**: converter produces a training set from current archive; shape verifiable; train/val/test split is time-ordered.

---

## <a id="tier-3"></a>TIER 3 — UI Bold and Sharp ⚡ THE NEW WORK ⚡

The operator named this specifically: "lets also look at ui sharpening. think deeply about the kind of platform, the ui ux requirement to make it look bold and sharp."

"Bold and sharp" is not technology; it's design commitments executed consistently. Today's UI is functional but visually generic (Bootstrap-feeling). What follows is specific.

### Design philosophy

Bonsai is **operator infrastructure for network engineers**. Not consumer software. The right reference points are: Linear (sharp typography, restrained color, motion as feedback), Tailscale admin (dense data presented clearly), Vercel dashboard (confident hierarchy, clear states). Wrong reference points: Datadog (busy, ad-revenue-driven density), Splunk (corporate, color-by-committee), generic Bootstrap admin templates.

### T3-1 (CV3) — Type system

**What**: replace whatever's currently being used with a deliberate type system.

```
Display:    Inter (variable font, 600-800 weight)
Body:       Inter (variable, 400-500 weight)  
Mono:       JetBrains Mono (for device names, addresses, technical values)

Sizes (rem):
  Display: 2.5 / 2.0 / 1.5
  Heading: 1.25 / 1.125
  Body: 1.0 / 0.875 (small)
  Mono: 0.875 / 0.8125 (small)

Line height: 1.15 for display, 1.5 for body
Tracking: -0.02em for display, normal for body, 0 for mono
```

Apply consistently across every workspace. Avoid mixing typefaces or using third families.

**Done when**: every text element across UI uses these tokens; design system file at `ui/src/lib/design/typography.ts` is the single source.

### T3-2 (CV3) — Color system

**What**: a restrained palette that signals state clearly.

```
Foundation:
  bg-base:         #0a0b0d (very dark blue-grey, not black)
  bg-surface:      #15171b
  bg-elevated:     #1d2026
  bg-glass:        rgba(255, 255, 255, 0.03)
  
Text:
  text-primary:    #e8eaed
  text-secondary:  #9aa0a6
  text-tertiary:   #5f6368
  text-on-accent:  #0a0b0d

State (used sparingly, always with semantic intent):
  state-healthy:   #34d399  (emerald)
  state-degraded:  #fbbf24  (amber)
  state-failed:    #f87171  (red)
  state-info:      #60a5fa  (blue)
  state-neutral:   #9ca3af  (gray)

Accent (single bonsai-brand accent, used sparingly):
  accent-primary:  #5eead4  (cyan-teal — distinctive but not branded-feeling)
  accent-muted:    #14b8a6
```

Two-mode design (dark default, light alternative; never both visible). Dark is the work mode; light is the share-screenshots mode.

**Done when**: design tokens file at `ui/src/lib/design/colors.ts`; every component uses tokens, never hex literals.

### T3-3 (CV3) — Hierarchy and density

**What**: confident hierarchy beats decorative borders and shadows. A workspace should have:
- One H1 (workspace name)
- 2-4 H2s (section dividers)
- Body content in clear vertical rhythm
- No more than 2 levels of visual elevation (surface → elevated)

Density rules:
- Tables: 32px row height, 12px column padding, no horizontal borders inside rows
- Cards: 16px padding, single 1px subtle border, no shadow except on hover
- Lists: 8px between items, no separators unless visually grouping

**Done when**: Operations and Incidents workspaces redesigned to these rules; visual audit passes.

### T3-4 (CV3) — Motion as feedback

**What**: motion exists to communicate state changes, not decorate.

```
Tokens:
  duration-instant:  100ms (state badge transitions)
  duration-fast:     200ms (modal open, drawer open)
  duration-medium:   400ms (workspace transitions)
  
Easing:
  out: cubic-bezier(0.2, 0, 0, 1)   (most enters)
  in:  cubic-bezier(0.8, 0, 0.6, 1) (most exits)
  inout: cubic-bezier(0.4, 0, 0.2, 1) (re-positioning)
```

No bouncy springs, no decorative loading spinners, no carousel transitions. Loading states are skeleton screens (3 grey blocks where content will appear), not spinners.

**Done when**: all transitions audited; no element uses default browser easing.

### T3-5 (CV3) — Key workspace re-articulations

Three workspaces get specific design love:

**Incidents** — the operator-facing surface that matters most.
- Card per incident with: severity stripe (left edge), title (root rule + affected device), one-line context, affected count, age, expand-on-click
- Inside expanded incident: blast radius mini-graph (3-tier topology), detection timeline, correlation log
- No icons-for-icons-sake; severity is a colored left stripe + sr-only-label

**Topology** — visual articulation of the graph.
- Background: subtle grid (0.02 opacity) to anchor coordinate system
- Nodes: circles with role-colored stroke (spine = brand-accent, leaf = neutral, super-spine = brand-accent saturated)
- Edges: thin lines, weight communicates BGP-LS-derived TE-metric; dashed lines for mgmt-plane
- Selection state: 2px stroke + outer glow + auto-zoom to fit selected + neighbors
- Hover: tooltip with FQDN + role + health summary, no other UI movement

**Operations** — the at-a-glance health dashboard.
- Six tiles in a 3×2 grid: write coordinator queue, event bus, archive lag, memory, disk, chaos cycle freshness
- Each tile has: large numeric value, secondary trend (sparkline), state color, click → detail
- One row below tiles: 24-hour timeline of detections fired (incident creation timeline)
- One row below that: recent activity (last 10 detection events, last 5 chaos injections)

**Done when**: these three workspaces look meaningfully different from before; design review against the philosophy references shows family resemblance.

### T3-6 (CV3) — Density toggles, not modes

**What**: instead of a separate "compact" mode, every list/table has a density toggle in the workspace header (Comfortable / Compact). Saves operator preference. Default to Comfortable for new sessions.

**Where**: `ui/src/lib/components/DensityToggle.svelte`.

### T3-7 (CV3) — Keyboard-first navigation

**What**: every workspace switch has a keyboard shortcut (Cmd-1 through Cmd-9 for the nine main routes). Search palette (Cmd-K) opens command palette with: navigate-to-device, navigate-to-incident, navigate-to-workspace, run-action.

**Where**: extend `ui/src/lib/CommandPalette.svelte`. Add keyboard hint overlay on first run.

**Done when**: every workspace reachable in 1 keypress + 1 search query.

---

## <a id="tier-4"></a>TIER 4 — E2E Validation (Tier 3 of CV2, Now Via Gemini)

The CV2 work that got deferred. Gemini owns execution; Claude Code consumes results.

### T4-1 (CV3) — Splunk HEC adapter e2e (Gemini)
### T4-2 (CV3) — Elastic adapter e2e (Gemini)
### T4-3 (CV3) — ServiceNow EM adapter e2e against PDI (Gemini)
### T4-4 (CV3) — ServiceNow AIOps bidirectional sync e2e (Gemini)
### T4-5 (CV3) — YANG sync against real OpenConfig repo (Gemini)
### T4-6 (CV3) — Synthesizer recommendations against real lab discovery (Gemini)

Each follows the T1-4 pre-task brief template. Each produces a structured artefact under `docs/test_results/<test_type>/<date>-<status>.md`. Claude Code reads these to triage fixes.

---

## <a id="tier-5"></a>TIER 5 — Syslog Facts and Cross-Source Joins (Deferred from CV2)

CV2 Sprint 5 deferred. Unchanged scope.

### T5-1 (CV3) — Syslog pattern files extended with capture groups
### T5-2 (CV3) — `SyslogFact` event type + extraction pipeline
### T5-3 (CV3) — Cross-source join engine
### T5-4 (CV3) — Cross-source detection rules

---

## <a id="tier-6"></a>TIER 6 — Streaming Protocols Completion

Three threads from CV2 Tier 4 partial work.

### T6-1 (CV3) — BMP receiver completion (N-3 fix)

Add PeerUp / PeerDown / StatisticsReport / Initiation / Termination message parsing. ~150 lines.

### T6-2 (CV3) — gobgp sidecar for BGP-LS (N-4 fix)

`docker/sidecars/gobgp/Dockerfile`, entrypoint, sample config. Compose profile `streaming`. ~60 lines.

### T6-3 (CV3) — Enricher registry pluralisation (N-1 fix)

Move from binary `(gnmi, parser_chain)` to `Vec<Arc<dyn MultiSourceEnricher>>` with capability-tag selection that consults readiness report (also N-2 fix).

### T6-4 (CV3) — Multi-protocol readiness report

Extend `GnmiReadinessReport` to `StreamingReadinessReport` covering gNMI, BMP, BGP-LS, syslog, SNMP per-device.

### T6-5 (CV3) — Lab configuration for BMP/BGP-LS

Lab YAMLs gain BMP-collector config on each SR Linux node. BGP-LS peering from one node to gobgp sidecar.

### T6-6 (CV3) — `cli_capture.py` → russh migration (N-6 fix, lower priority)

Defer until 100+ device deployments target. Document the migration plan; keep python helper functional.

---

## <a id="tier-7"></a>TIER 7 — GNN Path

Gates on Tier 2 archive depth + reset baseline.

### T7-1 (CV3) — Archive-to-training-data converter

Tier 2 T2-5 above.

### T7-2 (CV3) — GraphSAGE or GAT training

When archive depth + injections + per-rule examples + baseline stability conditions met (per Bv5 trigger criteria).

### T7-3 (CV3) — Comparison study (rules vs tabular ML vs GNN)

Apply each detector type to held-out test set. Confusion matrix. Categories: detected-by-GNN-only, detected-by-rules-only, detected-by-tabular-only, detected-by-multiple, detected-by-none.

### T7-4 (CV3) — Online inference path

Graph snapshot every N seconds; GNN scores Devices; UI surfaces.

### T7-5 (CV3) — Model card

Honest documentation. Algorithm, data, eval, limitations explicit.

---

## <a id="carryover"></a>Carryover from CV2

- CV2 daily check loop regression (N-5) — owned by Gemini in Tier 1
- Investigation agent (post-MVP, pending token budget)
- HIL graduated remediation in production
- Real-hardware-only schemas
- Bv2 hardcoding catalogue remainder

---

## <a id="execution-order"></a>Execution Order

### Sprint 1 (1 week) — Gemini CLI test protocol + daily-check resurrection
1. T1-1 Gemini CLI brief (Claude authors; cheap)
2. T1-2 git branch policy
3. T1-3 result format spec
4. T1-4 pre-task brief template
5. First operational handoff to Gemini: run all smokes against current laptop stack
6. T2-4 cloud sync verification (Gemini)
7. Resurrect daily check cron on laptop and cloud

### Sprint 2 (1-2 weeks) — E2E validation via Gemini
8. T4-1 through T4-6 (Gemini executes; Claude consumes results)
9. Triage fixes from results

### Sprint 3 (2 weeks) — Data accumulation sharpening
10. T2-1 fault propagation snapshots
11. T2-2 baseline diversity protected windows
12. T2-3 adversarial cases
13. T2-5 archive-to-training-data converter (skeleton; tests with synthetic)

### Sprint 4 (1-2 weeks) — Streaming protocols completion
14. T6-1 BMP message types
15. T6-2 gobgp sidecar
16. T6-3 enricher registry pluralisation
17. T6-4 multi-protocol readiness
18. T6-5 lab streaming config

### Sprint 5 (1 week) — UI sharpening kickoff
19. T3-1 type system tokens
20. T3-2 color system tokens
21. T3-3 hierarchy rules applied to Operations workspace as exemplar
22. T3-5 Incidents workspace re-articulation
23. T3-7 keyboard navigation

### Sprint 6 (1 week) — UI completion
24. T3-5 Topology re-articulation
25. T3-5 Operations final pass
26. T3-4 motion tokens applied
27. T3-6 density toggles

### Sprint 7 (1-2 weeks) — Syslog facts
28. T5-1 through T5-4

### Sprint 8 (3-4 weeks, parallel to Sprint 3-7) — Continuous data accumulation toward GNN trigger
29. Chaos cycle runs continuously on laptop and cloud
30. Daily check cron via Gemini produces structured reports
31. When archive depth + injection count + per-rule examples + baseline stability conditions all true: trigger GNN training

### Sprint 9 (3-4 weeks) — GNN training and deployment
32. T7-1 archive-to-training converter (real archive)
33. T7-2 GraphSAGE/GAT training
34. T7-3 comparison study
35. T7-4 online inference
36. T7-5 model card

### Estimated total
**8-12 weeks** to:
- Validated end-to-end output paths (Splunk, Elastic, ServiceNow, AIOps bidirectional)
- BMP + BGP-LS streaming producing operational signal
- UI that doesn't look like a default Bootstrap admin
- 30 days of clean post-reset chaos archive with propagation snapshots + adversarial cases + protected baselines
- Path B GNN trained, evaluated against rules + tabular ML, deployed to production with honest model card
- Fully exercised AIOps integration with ServiceNow

That's the deployable destination. **Most of the architectural work is done.** What remains is discipline, validation, and one focused training sprint.

---

## <a id="guardrails"></a>Guardrails — Updated

### New in CV3

- **Claude Code does not run operational tests.** Test execution is Gemini's domain. Claude reads results.
- **Gemini CLI does not modify source code.** Result writing is Gemini's domain. Code changes are Claude's.
- **Daily check is non-optional infrastructure.** A day without a daily check report is a day of degraded archive trust.
- **UI changes ship through the design system.** No component uses hex literals; all tokens.
- **Adversarial cases are part of training data.** GNN must see "looks like fault but isn't" patterns.
- **Protected baseline windows are sacrosanct.** No chaos injection during them, ever.
- **Fault propagation snapshots are mandatory.** Every chaos injection produces the snapshot sequence.

### Unchanged from v7-CV2

All prior architectural invariants. Reference earlier backlogs.

### Anti-patterns to reject

- "Claude can just run the test this once" — no, separation of concerns saves tokens
- "We'll do UI later when the engine is done" — no, UI sharpening lands now alongside data accumulation
- "Bold and sharp is subjective" — no, design tokens enforce it
- "30 days of any data is enough for GNN" — no, must include diverse baselines and adversarial cases
- "BMP route monitoring is enough" — no, peer notifications carry critical signal

---

## What CV3 Explicitly Excludes

- New protocol receivers beyond BMP/BGP-LS/PCEP
- K8s/RBAC/multi-tenancy
- Wireless / hardware-FRU / optical chaos simulation
- Auto-execution of synthesizer recommendations
- A UI rewrite beyond design system articulation
- Investigation agent without token budget

---

*CV3.0 — authored 2026-05-11 after sprint-by-sprint code review of post-CV2 main. CV2 substantially landed: enricher registry + parser-chain enricher wired, BMP receiver (RouteMonitoring only), BGP-LS receiver scaffolding (no sidecar yet), smoke test framework + wiring checks + CI workflow, testing discipline doc, pre-CV2 freeze applied as reset baseline. Seven nuance findings (N-1 binary registry, N-2 readiness-report bypass, N-3 BMP message-type gaps, N-4 missing gobgp sidecar, N-5 daily check regression, N-6 Python SSH subprocess cost, N-7 smoke aggregator missing). CV3 introduces Gemini CLI test protocol with explicit environment context to save tokens, data accumulation sharpening via fault propagation snapshots + protected baselines + adversarial cases, UI bold-and-sharp articulation with design system specification. Tier 4 picks up CV2's skipped Sprint 3 e2e validation now via Gemini. Carries forward syslog facts (Tier 5), streaming protocol completion (Tier 6), GNN path (Tier 7). Estimated 8-12 weeks to deployable destination with validated output adapters, BMP/BGP-LS operational, sharp UI, 30 days clean archive, trained Path B GNN. References v7-CV2 for unchanged context.*
