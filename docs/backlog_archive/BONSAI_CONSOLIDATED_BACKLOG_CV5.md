# BONSAI — Backlog Charlie Series, v5 (CV5.0)

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_CV4.md`. Authored 2026-05-12 after comprehensive code review of CV4 landings and discussion of cleanup, lab placement, SP strategy, and GNN training philosophy.
>
> **What this is**: a backlog that treats CV4 as having substantially landed (resource governor, MCP server, adapter cursor fix, parquet rotation, install_cron scripts, 7-day handoff doc) but never operationally proven. CV5 starts with a clean-slate cleanup — complete teardown of laptop and cloud bonsai instances — and rebuilds with the discipline established in CV1-CV4 plus six new things:
>
> 1. **Complete laptop + cloud cleanup**. Both environments are in unknown state. Stale containers, half-applied configs, possibly leaking processes. CV5 Sprint 1 is teardown-to-zero before any new work.
> 2. **Single management network as standard**. We previously paid for lab-architecture coupling because mgmt-net changed per topology. This is now an invariant.
> 3. **Central feature testing index**. A single canonical document at `docs/testing/FEATURE_INDEX.md` consolidates what each bonsai feature is, how it's tested, what the artefact location is, and current pass/fail/skip status. Replaces the scattered random tests pattern.
> 4. **Lab placement decision**. Given resource constraints, pick exactly one architecture per environment. DC on laptop, SP on cloud (when SP lab works). Possibly second cloud arriving — note where it would go.
> 5. **SP lab strategy with vendor research**. Previous Nokia SR Linux SR attempts hit dead-ends. Do the research before more cycles burn. Decide between SR Linux (revisit specific failures), Cisco XRd (more features, harder operationally), or hybrid. Specify the lab with config files before bring-up.
> 6. **GNN training philosophy clarified**. This deserves its own section because the operator's question is the right one. Chaos-from-small-labs does narrow training-feature-distribution but does NOT necessarily narrow structural-pattern learning. The training philosophy is: lab provides labeled supervised data; production provides unlabeled calibration data; the deployed model adapts per-environment through onboarding calibration, not through retraining.
>
> Plus six findings from the CV4 code review (C4-N1 through C4-N6) that need addressing in the new sprints.

---

## Table of Contents

1. [Where We Are, Honestly](#where-we-are)
2. [CV4 Code Review — Sprint-by-Sprint](#audit)
3. [Findings C4-N1 through C4-N6](#findings)
4. [GNN Training Philosophy (deserves its own section)](#gnn-philosophy)
5. [TIER 1 — Complete Cleanup of Laptop and Cloud](#tier-1) ⚡ START HERE ⚡
6. [TIER 2 — Single Management Network as Invariant](#tier-2)
7. [TIER 3 — Central Feature Testing Index](#tier-3)
8. [TIER 4 — Lab Placement Decision](#tier-4)
9. [TIER 5 — SP Lab Strategy](#tier-5)
10. [TIER 6 — Fix CV4 Code Review Findings](#tier-6)
11. [TIER 7 — Hands-Off 7-Day Proof (re-attempt with clean state)](#tier-7)
12. [TIER 8 — GNN Path Forward](#tier-8)
13. [Tracked Future Threads](#tracked)
14. [Code Quality, Motivation, Where We Are](#motivation)
15. [Execution Order](#execution-order)
16. [Guardrails](#guardrails)

---

## <a id="where-we-are"></a>Where We Are, Honestly

The codebase has matured substantially. ~75K Rust lines, ~16K Python, Svelte UI. Discovery-driven layered ingestion, multi-source streaming, synthesizer, change detection, ServiceNow AIOps, adaptive resource governance, MCP server, archive-to-training converter. The architectural foundation is in better shape than the operational story.

**What's true today**:
- Bonsai compiles cleanly. The pre-CV2 freeze is intact as a known-good restoration point.
- CV4 code landed: resource governor (387 lines), MCP server (728 lines), Splunk/Elastic cursor fix, parquet age-based rotation, install_cron scripts (laptop and cloud), 7-day handoff doc.
- Smoke + e2e testing framework exists and has been validated by Gemini at least once.
- The chaos cycle has accumulated some data (159 labeled injections logged on 5-11), but archive parquet writers were not closing, so external visibility was zero.

**What's not true today** despite the framing of "CV4 substantially landed":
- **No new daily reports since 2026-05-11.** The cron was supposedly installed in CV4 Sprint 3; it has not produced output.
- **No new e2e tests after 2026-05-12.** The adapter cursor fix shipped but has not been validated against a fresh Splunk/Elastic.
- **Archive directory empty.** Either chaos has not been running, or rotation hasn't been validated against a live cycle.
- **Cloud daily-sync** still has not produced `sync/cloud-spike/*` branches on origin.
- **Resource governor counts but does not act** (Finding C4-N2 below). Currently a passive monitor.

**The friction pattern**: CV2 → CV3 → CV4 each landed substantial code. Each was supposed to be followed by operational proof. Each ran into a different obstacle (mid-process restarts, script smell, governor without action plumbing). Each next sprint added more code on top of un-validated previous code.

**The break in CV5**: complete cleanup. Reset both environments to known-zero state. Rebuild with the existing code but apply the new operational discipline (single mgmt net, feature index, lab placement decision, governor active-control fix). Then run the 7-day hands-off proof. Only after that, proceed to GNN training.

---

## <a id="audit"></a>CV4 Code Review — Sprint-by-Sprint

### Sprint 1 — Test Framework Stabilization ✅ LANDED

| Item | Status | Evidence |
|---|---|---|
| T1-1 e2e SKIP semantics | ✅ Done | `scripts/e2e_output_adapters_test.sh:52-57,350,371` — SKIP is now a real status with its own markdown template |
| T1-2 daily check aggregation | ✅ Done (partial) | `scripts/bv5_daily_check.sh` updated to distinguish prereq_missing — verify it produces "pass_with_caveats" not "fail" when cloud_sync hasn't run |
| T1-3 parquet rotation interval | ✅ Done | `src/archive.rs:71-95,163` — `max_file_age_secs` config + age timer; `bonsai.toml.example:803` config field |
| T1-4 driver result UI surface | ⚠️ Verify | Operations.svelte modified — needs visual check that operations workspace shows driver result breakdown |

### Sprint 2 — Adapter Push Pipeline ✅ LANDED

| Item | Status | Evidence |
|---|---|---|
| T2-1 diagnose | ✅ Done | Root cause was unbounded `HashSet<String>` of pushed IDs; replaced with monotonic `cursor_ns` |
| T2-2 fix | ✅ Done | `src/output/splunk_hec.rs` and `elastic.rs` both use `WHERE e.fired_at > $since_ns`; cursor advances per-batch |
| T2-3 smoke regression test | ✅ Done | `scripts/smoke/smoke_output_adapters.sh:47-66` injects synthetic detection and verifies `last_push_at_ns` advances |

**Note**: cursor initializes at `now_ns() - 120_000_000_000`. See Finding C4-N1.

### Sprint 3 — Hands-Off Operational Proof ⚠️ CODE LANDED, NOT EXECUTED

| Item | Status | Evidence |
|---|---|---|
| T3-1 cron installation | ✅ Code | `scripts/install_cron.sh` (65 lines), `scripts/cloud/install_cron.sh` (86 lines) — idempotent, both with --list/--remove |
| T3-2 cloud daily-sync repair | ⚠️ Probably code-only | `scripts/cloud/daily_sync.sh` and `daily_sync_check.sh` modified; **no `sync/cloud-spike/*` branches on origin** still |
| T3-3 7-day operation test | ❌ Not started | Doc exists at `docs/operations/7day_handoff.md` (107 lines); execution has not started — no daily reports after 5-11 |
| T3-4 7-day dashboard | ⚠️ Partial | Operations.svelte modified; visual confirmation pending |

### Sprint 4 — Adaptive Resource Governance ⚠️ OBSERVABILITY LANDED, ACTION NOT WIRED

| Item | Status | Evidence |
|---|---|---|
| T4-1 environment probe | ✅ Done | `src/resource_profile.rs` (237 lines) — Tiny/Small/Medium/Large/XLarge derived from RAM; defaults table |
| T4-2 inbound rate governance | ⚠️ Observability only | Governor counts events, emits metrics, sets flags. **No ingest path consults `rate_shedding_active`.** See C4-N2. |
| T4-3 memory pressure governance | ⚠️ Observability only | Same pattern — counts, emits metrics, sets flags. **No code path actually shrinks LRU caches or triggers flush.** See C4-N2. |
| T4-4 write pressure governance | ⚠️ Observability only | Same pattern. No batch-size expansion happens. |
| T4-5 observability | ✅ Done | `/api/governance/state` endpoint, `bonsai_governance_action_total` counter |
| T4-6 documentation | ✅ Done | `docs/resource_profiles.md` |

**The honest read**: the governor is a passive monitor. It correctly identifies pressure but cannot do anything about it. Closing this gap is in CV5 Tier 6.

### Sprint 5 — Agent-Friendly Interface ✅ LANDED WITH NOTES

| Item | Status | Evidence |
|---|---|---|
| T5-1 MCP server | ✅ Done | `src/mcp_server.rs` (728 lines) — JSON-RPC 2.0, initialize/tools/list/tools/call, 5 tools |
| T5-2 grounded response composition | ⚠️ Verify | Recurrence indicators in MCP_server `RULE_CATALOGUE`; need to confirm `/api/incidents/{id}/grounded` endpoint exists |
| T5-3 self-describing schema | ⚠️ Partial | OpenAPI partial — every endpoint declared in `http_server.rs` schema block — full OpenAPI 3 generator not visible |
| T5-4 recurrence indicators | ✅ Done | RULE_CATALOGUE has substantive recurrence_indicators per rule with concrete Cypher queries and correlation hints |
| T5-5 natural-language reference resolution | ⚠️ Verify | Need to check for `/api/resolve` endpoint |

**Note on T5-1**: Cypher read-only enforcement is substring matching. See Finding C4-N3.

---

## <a id="findings"></a>Findings C4-N1 through C4-N6

### C4-N1 — Adapter cursor 120s startup window

**Location**: `src/output/splunk_hec.rs:108` and equivalent in `elastic.rs`

**Severity**: LOW

After a bonsai restart, the cursor initializes 120 seconds back. If an adapter has been offline for 24h (cold restart, deploy, OOM-kill recovery), the first cycle only sees the last 120s of detections. Older queued detections are silently lost.

**Why it matters**: when bonsai is restarted during an incident — exactly when output adapters matter most — recent detections may have already aged past the cursor window. The operator's downstream SIEM (Splunk, Elastic) loses those events.

**Fix shape**: persist `cursor_ns` to disk on graceful shutdown; restore on startup. Cold-restart (no persisted cursor) keeps the 120s default. Reconnect path looks at last_push_at from `/api/adapters/{name}/state` to decide replay window.

**Effort**: 2-3 hours.

### C4-N2 — Resource governor observes pressure but cannot act ⚡ HIGH

**Location**: `src/resource_governor.rs`

**Severity**: HIGH — this is the difference between "monitoring" and "governance"

The governor's three loops correctly detect memory pressure, write pressure, and rate excess. They emit metrics, increment counters, set `*_active` flags. **They cannot do anything about the pressure** because no other module reads the flags. Concretely:

- `memory_pressure_active = true` → comment in `govern_memory_hard` says "we don't have a direct flush RPC today, so we emit the metric and let the governor flag drive ingest shedding as relief." But ingest doesn't read the flag.
- `write_pressure_active = true` → write coordinator does not consult it; batch size stays static.
- `rate_shedding_active = true` → ingest receivers (syslog, SNMP, BMP, gNMI) do not consult it; events are not shed.

The end state is that under real pressure, the governor logs warnings and the kill-switch (Bv4 memory budget assertions) eventually triggers a hard stop. No graduated degradation actually happens.

**Why it matters**: this is the foundational scale-readiness feature. Production deployments running at memory limits will still hit the kill-switch with no warning landing. The metrics show pressure but the system cannot relieve it.

**Fix shape**: add governor-aware paths in three places:
1. **Ingest**: each receiver checks `rate_shedding_active` and drops low-priority updates (BMP Stats messages, debounced counters, ON_CHANGE re-confirmations) when true.
2. **Write coordinator**: `flush_telemetry_batch` reads governor batch-size override when `write_pressure_active` is true; expands batch size up to 2× profile default.
3. **Ingest debounce caches**: a watcher task observes `memory_shrink_count`; when it increments, shrinks the 16-shard LRU caches by `target * 0.75`.

**Effort**: ~1 week. This is the substantive CV5 work in Tier 6.

### C4-N3 — MCP Cypher read-only enforcement via substring matching

**Location**: `src/mcp_server.rs:473` `is_readonly_cypher`

**Severity**: MEDIUM (no external exposure yet, but hardening required before MCP is reachable from outside localhost)

`is_readonly_cypher` rejects `cypher` strings containing `"CREATE "`, `"SET "`, `"DELETE "`, `"MERGE "`, `"REMOVE "`, `"DETACH "` (case-folded). This is bypassable by:

- Comments containing mutation keywords being ignored by substring match while a real mutation hides elsewhere
- Multi-statement queries (if Kuzu supports them)
- Cypher string literals containing the keyword as data: `MATCH (n {label: 'SET '}) RETURN n` is incorrectly rejected (false positive)
- Function or variable names containing keywords

**Why it matters**: the MCP server will eventually be reachable from outside localhost (a Claude Code or Gemini session, a ServiceNow AIOps integration). String-matching is brittle.

**Fix shape**: open the LadybugDB connection in read-only transaction mode for MCP queries. Kuzu/lbug supports read transactions; using one is more robust than parsing Cypher. The substring matcher stays as a defense-in-depth first gate, but the real protection is the read-only transaction.

**Effort**: 2-3 hours plus testing against a few attack vectors.

### C4-N4 — Resource profile discretization at GB boundaries

**Location**: `src/resource_profile.rs:38-47`

**Severity**: LOW (operational surprise, not bug)

`from_ram` floors at GB boundaries: 0-1 GB = Tiny, 2-5 GB = Small, etc. A 1.9 GB VM gets Tiny (256 MB budget); a 2.0 GB VM gets Small (512 MB budget). Operationally surprising at the boundary.

**Fix shape**: document the discretization explicitly in `docs/resource_profiles.md` (which already exists — add a "Boundary Behavior" section). Optionally introduce hysteresis if a VM is right at the boundary, but documentation is sufficient.

**Effort**: 1 hour documentation.

### C4-N5 — Operational discipline drift: no daily reports since 5-11

**Location**: `docs/test_results/daily_runs/` contains 5-08 and 5-11 only

**Severity**: HIGH (operational, not code)

The 7-day handoff doc is well-written. The cron installer script is well-written. Neither has been executed against a real bonsai cycle. The operational discipline that CV4 was meant to establish is still hypothetical.

**Fix shape**: this is CV5 Tier 7. After cleanup (Tier 1) and lab placement (Tier 4), run the 7-day proof and accept its result whatever it is.

**Effort**: 7 days wall clock + ~30 min/day check.

### C4-N6 — Archive empty despite chaos running

**Location**: `runtime/archive/` empty in the current zip

**Severity**: MEDIUM (could be: chaos hasn't run recently, or rotation didn't catch, or zip didn't capture archive dir)

The 5-11 daily showed `archive_bytes:0` because parquet writers stayed open. T1-3 fixed the rotation to 60 min default. If chaos has been running with the rotated config, the archive should now have closed files. The zip showing empty archive suggests **chaos hasn't run since the rotation fix shipped**.

**Fix shape**: Tier 1 cleanup confirms whether archive is real-empty or capture-empty. Tier 7 hands-off proof generates real archive growth.

**Effort**: addressed operationally by Tier 1 + Tier 7.

---

## <a id="gnn-philosophy"></a>GNN Training Philosophy

This is the operator's most important question. **Will training on small-lab chaos narrow bonsai's scope?** The honest answer requires distinguishing what the GNN learns from what it overfits to.

### The honest concern

Bonsai's chaos archive comes from:
- 8-node DC lab on laptop (SR Linux, EVPN-VxLAN)
- 6-node DC lab on cloud (same)
- Eventually SP lab (vendor TBD per Tier 5)

Training data has 1 vendor (Nokia SR Linux), 1 architecture (EVPN-VxLAN CLOS), ~10 device count, synthetic chaos (netem, interface-shut, BGP reset, BFD timeout, route flap, adversarial cases for "looks-like-fault-but-isn't"). Real production networks have:
- Multiple vendors (Cisco IOS-XR, Junos, Arista, FRR, Nokia)
- Multiple architectures (DC, SP, campus, hybrid, cloud)
- Hundreds to thousands of devices
- Real noise: maintenance windows, intentional churn, asymmetric routing, traffic bursts
- Signals bonsai doesn't ingest yet: wireless, optical, hardware FRU, environmental

**The core question is whether a GNN trained on the small lab learns transferable patterns, or learns lab-specific shortcuts.**

### What the GNN actually learns

Two distinct things at different levels of abstraction:

**Level 1 — Vendor/feature-specific patterns**: how SR-Linux's `/network-instance/.../bgp/neighbor/state` field transitions during a session reset; how SR Linux encodes oper-status. A model trained only on SR Linux develops embeddings tuned to SR Linux quirks. **This does narrow scope** — deployed against Cisco IOS-XR with different state encodings, the model's confidence will be miscalibrated.

**Level 2 — Graph-structural propagation patterns**: when a device loses an uplink, blast-radius neighbors show reachability degradation within 30s; when BFD goes down, BGP follows within ~hold-time; when a route flap originates upstream, its propagation signature looks like a wave across multiple devices. **These are vendor-independent.** A GNN learning structural patterns at this level generalizes across vendor implementations because the underlying protocols behave consistently.

**The training philosophy follows from this distinction**: design the GNN's node features to be vendor-neutral wherever possible. Vendor identity is one of many features, not the dominant one. Then the model learns structural patterns and uses vendor identity as a calibration signal, not a primary predictor.

### Concrete training philosophy (CV5 commitment)

1. **Vendor-neutral node features**. Node embeddings include: degree, role-quartile (from topology layout), observed-protocol-set (cardinality of {BGP, IS-IS, BFD, OSPF} active), recent-event-rate (windowed count of state transitions), time-since-last-event. Vendor identity is included as a one-hot vector — but it should never dominate the embedding norm. Empirically validate this with feature ablation.

2. **Lab provides supervised data; production provides distribution calibration**. The lab-trained model is a **base model**. Deployment includes a "calibration phase" where the model observes 7 days of local non-fault traffic to learn the deployment's noise floor and recalibrate decision thresholds. This is exactly how SuzieQ's anomaly detection works in practice — labelled lab patterns + per-deployment threshold tuning.

3. **Train on structural diversity, not data quantity alone**. 500 labeled injections across a single 8-node lab gives less diversity than 200 labeled injections across DC + SP + 3 fault families. CV5 Tier 5's SP lab is therefore strategically important for the GNN, not just for feature coverage. Cross-topology training produces more transferable embeddings.

4. **Adversarial cases are non-negotiable**. CV3 introduced `chaos_plans/adversarial_cases.yaml` for "looks like fault but isn't" scenarios. These are critical for keeping false-positive rate low in production. A production-deployable model must train with adversarial examples weighted at least 15% of total examples.

5. **Online learning is the long-term answer to generalization**. The base model deploys; operator feedback labels false positives and false negatives; the model updates incrementally. This requires infrastructure we don't have yet (a labeled-event feedback loop in the UI), but it's the path to truly general models. Track as future, not for CV5.

### What this means operationally

**Do not delay the GNN sprint to wait for more diverse training data.** The 30-day archive trigger from Bv5 still holds. When triggered:

- Train the GNN on whatever chaos archive exists (DC + SP if Tier 5 SP lab is running by then)
- Evaluate on held-out chaos archive AND on a slice of *unlabeled* production-like traffic if available (operator's own home lab? a friend's controlled environment?) to check that base-model anomaly scores have reasonable distribution under non-fault conditions
- Ship with a documented "calibrate against your deployment for 7 days before relying on absolute scores; relative ordering is more reliable than absolute thresholds"
- The model card explicitly states: "trained on labeled chaos from DC EVPN-VxLAN (SR Linux) + SP MPLS-SRTE (vendor); generalization to other vendor implementations is not validated; calibration recommended"

### The honest deployment story

When someone deploys bonsai against their network for the first time:
1. Run for 7 days in observe-only mode (no detections firing to AIOps)
2. GNN scores accumulate; operator reviews top-N high-score events as a sanity check
3. Calibration phase ends; operator either accepts the calibrated thresholds or tunes them
4. Detections start firing to AIOps

This is more honest than "deploy and detect immediately" and aligns with how anomaly detection products are deployed in practice.

### Where this lives in the backlog

- **CV5 Tier 8**: GNN training run with vendor-neutral features, multi-topology training data (DC + SP), adversarial-weighted, with calibration phase support.
- **Post-CV5**: online learning infrastructure (labeled feedback loop).
- **Forever**: the model card is the contract with deployers about what's been validated and what hasn't.

---

## <a id="tier-1"></a>TIER 1 — Complete Cleanup of Laptop and Cloud ⚡ START HERE ⚡

The operator's first ask. Both environments are in unknown state. CV3-CV4 produced significant code with partial operational validation; neither environment is trusted. Tier 1 zeros them out before any new work.

### T1-1 (CV5) — Laptop cleanup runbook

**What**: a documented procedure (script + checklist) that takes the laptop from "current possibly-messy state" to "ready to start fresh." Specifically:

```bash
# Stop everything bonsai-related
bash scripts/install_cron.sh --remove        # remove cron
sudo systemctl stop bonsai 2>/dev/null       # stop service if installed
pkill -9 -f bonsai 2>/dev/null               # kill any leftover processes
pkill -9 -f chaos_runner 2>/dev/null         # kill chaos daemons

# Tear down containerlab labs
sudo containerlab destroy -t lab/dc/dc-evpn-srv6.clab.yml 2>/dev/null
sudo containerlab destroy -t lab/sp/sp-mpls-srte.clab.yml 2>/dev/null
sudo containerlab destroy -t lab/fast-iteration/multivendor.clab.yml 2>/dev/null

# Stop and remove all external infrastructure containers
docker compose -f docker/compose-external.yml --profile all down -v
docker compose -f docker-compose.yml down -v

# Clean up known runtime state (DESTRUCTIVE — back up first if needed)
mv runtime/archive runtime/archive.precv5-$(date +%s) 2>/dev/null
mv runtime/logs runtime/logs.precv5-$(date +%s) 2>/dev/null
mv runtime/driver_results runtime/driver_results.precv5-$(date +%s) 2>/dev/null
rm -rf runtime/bonsai.db.local runtime/bonsai.db.wal.local 2>/dev/null

# Verify clean state
docker ps -a --filter "name=clab\|bonsai\|netbox\|splunk\|elastic"  # should be empty
sudo containerlab inspect                                            # should report no labs
ps aux | grep -E "bonsai|chaos_runner" | grep -v grep               # should be empty
ls runtime/archive runtime/logs 2>/dev/null                          # should not exist
```

**Where**: new script `scripts/cleanup_laptop.sh` plus checklist in `docs/operations/laptop_cleanup.md`.

**Done when**: laptop reports zero bonsai processes, zero clab containers, zero docker-compose stacks, archive/logs/runtime backed up to dated dirs. Operator confirms via the verification commands.

### T1-2 (CV5) — Cloud cleanup runbook

**What**: equivalent for the OCI cloud VM. Steps:

```bash
# SSH to cloud VM, then:
bash scripts/cloud/install_cron.sh --remove
sudo systemctl stop bonsai
pkill -9 -f bonsai

sudo containerlab destroy -t lab/cloud-dc-6node.yml

# Cloud may have used host docker for collectors — tear down
docker compose -f docker/compose-external.yml --profile all down -v

# Move runtime aside; do NOT delete (will be reviewed for any unique archive data)
mv runtime/archive runtime/archive.precv5-$(date +%s)
mv runtime/logs runtime/logs.precv5-$(date +%s)
```

**Where**: new script `scripts/cloud/cleanup.sh` plus checklist in `docs/operations/cloud_cleanup.md`.

**Done when**: cloud VM verified clean. SSH+verification steps documented.

### T1-3 (CV5) — Pre-CV5 freeze (replaces pre-CV2 freeze as restoration point)

**What**: before starting any rebuild, snapshot the cleaned-up state as a known-good baseline. Mirrors what pre-CV2 freeze did. Tarball the runtime directory, copy LadybugDB, copy any meaningful archive deltas (the `*.precv5-*` backups).

Pre-CV5 freeze becomes the rollback point for CV5 work. The pre-CV2 freeze is preserved (it represents a meaningful earlier state).

**Where**: `pre_cv5_freeze_<timestamp>/` directory at repo root.

**Done when**: freeze tarball created and verified; restore procedure documented.

---

## <a id="tier-2"></a>TIER 2 — Single Management Network as Invariant

The operator named this directly: changing mgmt-network per topology caused churn. Fix as an architectural invariant going forward.

### T2-1 (CV5) — Audit current lab YAMLs for mgmt-network consistency

**What**: check all `lab/**/*.clab.yml` for `mgmt:` blocks. Catalog what's there currently:

```bash
grep -l "mgmt:" lab/**/*.yml
grep -A 3 "mgmt:" lab/**/*.yml | grep -E "network|ipv4-subnet|bridge"
```

Confirm whether each lab uses the same `network: bonsai-mgmt` or has divergent definitions.

**Where**: audit produces `docs/operations/mgmt_net_audit.md`.

**Done when**: list of all labs with their current mgmt-network state.

### T2-2 (CV5) — Standardize on single mgmt network: `bonsai-mgmt`

**What**: every lab YAML uses identical mgmt-network configuration. Proposed standard:

```yaml
mgmt:
  network: bonsai-mgmt
  ipv4-subnet: 172.100.100.0/24
  ipv6-subnet: 2001:db8:1::/64
```

Per-lab IP assignments stay unique (no collisions across labs that might co-exist), but the network name, subnet, and bridge stay constant. **Same name even when only one lab runs at a time.**

**Where**: edit every `lab/**/*.clab.yml`. Document the convention in `docs/lab_metadata.md`.

**Done when**: all labs use `bonsai-mgmt`; bringing up any lab uses the same docker network; operator can list `docker network ls | grep bonsai-mgmt` and see exactly one.

### T2-3 (CV5) — Bonsai config keys mgmt-network address ranges, not lab names

**What**: bonsai's collector configuration uses subnet-based device discovery, not lab-name-coupled. Wherever the code today says "if topology=dc, expect 172.100.103.0/24" that becomes "scan 172.100.100.0/24 for collectors." Reduces lab-architecture coupling.

**Where**: `src/config.rs`, `src/collector/mod.rs` — verify and tighten any place a topology name leaks into runtime behavior.

**Done when**: bonsai's runtime knows the mgmt subnet but does not know which topology is using it. Bringing up DC vs SP changes only the lab YAML, not bonsai config.

---

## <a id="tier-3"></a>TIER 3 — Central Feature Testing Index

Operator's third ask. Random scattered tests for individual features have produced ambiguity about "what is tested" vs "what just compiles." A single index resolves it.

### T3-1 (CV5) — Author `docs/testing/FEATURE_INDEX.md`

**What**: a single canonical document with one section per bonsai feature. Each section has:
- Feature name + brief description
- Where the feature is implemented (src path)
- How it's tested (unit / smoke / e2e / integration)
- Test artefact location (script + result file path)
- Current status (passing / failing / skip / not yet tested)
- Last tested date

**Initial inventory** (in author-order, ~30 features):

- gNMI Subscribe ingestion (streaming hot path)
- gNMI Get on-demand capture
- gNMI capabilities discovery
- CLI parser-chain enrichment (via SSH + paramiko)
- BMP receiver (RouteMonitoring + PeerUp/Down/Stats/Init/Term)
- BGP-LS receiver (via gobgp sidecar)
- syslog ingestion daemon
- SNMP trap ingestion daemon
- NetBox enricher (IPAM/DCIM)
- ServiceNow CMDB enricher
- ServiceNow EM output adapter
- ServiceNow AIOps bidirectional sync
- Splunk HEC output adapter
- Elastic output adapter
- Prometheus output adapter
- Graph write coordinator
- LadybugDB write_batch transactions
- Event bus (router + per-subscriber queues, ArcSwap)
- Ingest debounce (16-shard sharded LRU)
- Change detection runtime (3 triggers: syslog, scheduled, manual)
- Local guarded config store
- Path synthesizer (8 starter rules)
- YANG library lifecycle (sync/import/bundle)
- Operations workspace UI
- Incidents workspace UI (correlation, blast radius)
- Topology workspace UI (degree-quartile fallback)
- Resource governor (Tier 4 from CV4 — partial)
- MCP server (5 read tools)
- Investigation agent (parked behind tokens)
- Chaos runner with fault propagation snapshots
- Archive verifier
- Archive-to-training converter (synthetic + real archive)

For each: status row including last-validation-date and link to artefact.

**Where**: `docs/testing/FEATURE_INDEX.md`.

**Done when**: document exists, every feature has a row, current status is honest (most will be "passing-smoke, not-validated-e2e", which is fine — just write it down).

### T3-2 (CV5) — Wire feature index to the daily check

**What**: daily check produces a summary updating the index's "last tested" column. Doesn't need to be perfectly automated; even a daily appends-to-changelog pattern is enough.

**Where**: extend `scripts/bv5_daily_check.sh` to append a one-liner to `docs/testing/FEATURE_INDEX_HISTORY.md`.

**Done when**: 3 consecutive daily checks each add a history line.

### T3-3 (CV5) — Gemini brief references feature index

**What**: update `docs/gemini_cli_brief.md` to reference the feature index. When operator asks Gemini "what's the status of feature X?", Gemini reads the index first.

**Where**: edit `docs/gemini_cli_brief.md`.

---

## <a id="tier-4"></a>TIER 4 — Lab Placement Decision

Given resource constraints (laptop + OCI Always Free + possibly second cloud), commit to one architecture per environment.

### T4-1 (CV5) — Lab placement policy

**The decision**:
- **Laptop**: DC lab (`lab/dc/dc-evpn-srv6.clab.yml`, 8 SR-Linux nodes). Laptop has the most headroom; DC is operationally simpler; chaos cycle has been validated against it.
- **OCI cloud**: when SP lab is ready (Tier 5), SP runs on cloud. Until then, OCI runs the same DC lab (6-node variant `lab/cloud-dc-6node.yml`) to provide a second independent DC dataset.
- **Second cloud (if obtained)**: hyperscaler GKE/EKS/AKS Always Free? Probably not available. If obtained, runs the cloud-DC-6node lab to provide a third independent DC dataset, OR runs SP lab if Tier 5 hits a wall on OCI.

The rule: **never two architectures on the same VM**. Resource budget is too tight to multiplex.

**Where**: `docs/operations/lab_placement.md` — captures the decision plus rationale plus second-cloud guidance.

### T4-2 (CV5) — Resource budget per environment

For each environment, declare:
- RAM cap (laptop: depends; cloud OCI: 24 GB)
- Storage cap (laptop: 30 GB for runtime; cloud: 100 GB OCI default)
- Network cap (n/a for laptop; OCI: free tier egress limits)
- CPU cap (laptop: variable; OCI: 4 vCPU)

Map to resource profile from CV4 T4-1:
- Laptop typically Medium or Large
- OCI Small or Medium
- Second cloud (if any) Small

**Where**: `docs/operations/resource_budgets.md`.

---

## <a id="tier-5"></a>TIER 5 — SP Lab Strategy

The most strategically important tier. Previous Nokia SR Linux SR attempts failed. Decide before more cycles burn.

### T5-1 (CV5) — Research: SR-MPLS / SR-v6 / BGP-LU on SR Linux vs Cisco XRd

The operator asked me to web-search whether Nokia SR Linux SR is feasible. Let me build the research file now and address it directly in the backlog.

**What to investigate**:
- Is SR-MPLS support in containerized SR Linux mature in current releases (26.x)?
- What specific features failed: SR-MPLS data plane, BGP-LU, SR-v6 H.Encaps, RSVP-TE?
- Is the issue config syntax (operator didn't find right knobs) or feature absence (SR Linux container doesn't actually do it)?
- Cisco XRd: does it support SR-MPLS, SR-v6, BGP-LU, RSVP-TE in containerized form? Resource footprint?
- FRR: any SR support? Practical for PE/P emulation?
- Hybrid lab option: SR Linux for some roles, XRd for others?

**Where**: `docs/operations/sp_lab_research.md` — a research document, not just a backlog entry. Includes:
- Each candidate's feature matrix (SR-MPLS yes/no/partial, SR-v6 yes/no/partial, BGP-LU, RSVP-TE, IS-IS, MPLS LDP, BMP, BGP-LS)
- Each candidate's container resource footprint
- Each candidate's known issues from public sources
- Recommended choice with rationale

**Done when**: research doc exists with clear recommendation.

### T5-2 (CV5) — SP lab full specification (config files before bring-up)

**What**: based on T5-1 outcome, fully specify the SP lab including:
- Per-node configs (IS-IS, BGP, SR, MPLS)
- BMP collector configuration
- BGP-LS export configuration
- PCEP if applicable
- Expected adjacencies, LSPs, route counts at steady state

**Pre-specification before bring-up matters**: previous attempts burned cycles on hit-and-try. The lab is brought up only after the spec is reviewed and the expected steady state is documented.

**Where**: `lab/sp/sp-mpls-srte.clab.yml` updated + per-node config files in `lab/sp/configs/` + spec document at `docs/operations/sp_lab_spec.md`.

**Done when**: spec is complete; operator (or Gemini) confirms it before bring-up.

### T5-3 (CV5) — SP lab bring-up against spec

**What**: bring up the SP lab; verify each line item of the spec; document deviations.

**Where**: `docs/test_results/sp_lab_bringup_<date>.md`.

**Done when**: SP lab reports all-green against its spec. Move to operational mode (chaos runner targets SP topology in addition to DC).

### T5-4 (CV5) — SP chaos catalogue

**What**: SP-specific chaos catalogue covering: LDP session loss, RSVP-TE path failure, SR-policy degradation, BGP-LU label withdrawal, IS-IS adjacency timeout, P/PE link failure, RR session loss.

**Where**: `chaos_plans/always_on_sp.yaml`.

**Done when**: catalogue covers ≥6 fault types per SP role (P, PE, RR), can be run via existing chaos_runner.py.

---

## <a id="tier-6"></a>TIER 6 — Fix CV4 Code Review Findings

### T6-1 (CV5) — Resource governor active control plumbing (C4-N2)

**What**: wire governor flags through to actual control points. Three plumbings:

1. **Ingest** reads `rate_shedding_active`:
   - Syslog UDP receiver: drops events when `rate_shedding_active` AND inbound rate > profile budget × 0.9
   - SNMP trap receiver: same
   - BMP receiver: drops StatisticsReport messages (low-value) while keeping RouteMonitoring (high-value)
   - gNMI subscriber: does not drop (streaming bursts are usually meaningful); instead, observes counter `bonsai_gnmi_burst_events_total`

2. **Write coordinator** reads `write_pressure_active`:
   - Batch size expands to `profile_default * 2` when active
   - Flush interval reduces to `flush_interval_secs / 2` when active

3. **Ingest debounce caches** observe `memory_shrink_count`:
   - Per shard, cache size reduces by 25% on each increment
   - Debounce intervals increase by 50% on each increment
   - Recovery: when `memory_pressure_active` falls back to false for 60s, sizes reset

**Where**: `src/ingest.rs` (debounce watcher), `src/signals/syslog.rs`, `src/signals/snmp.rs`, `src/streaming/bmp.rs`, `src/write_coordinator.rs`.

**Done when**: synthetic test (replay archive at 10× speed) shows governor flags activate, control points respond visibly in metrics, RSS stays bounded.

### T6-2 (CV5) — Adapter cursor persistence (C4-N1)

**What**: persist `cursor_ns` to disk on graceful shutdown; restore on startup. Cold-start with no persisted cursor keeps 120s default.

**Where**: `src/output/{splunk_hec.rs, elastic.rs}` — write cursor to `runtime/adapter_state/{adapter_name}.cursor` on shutdown signal.

**Done when**: bonsai restart preserves cursor; reconnect picks up where left off.

### T6-3 (CV5) — MCP read-only Cypher hardening (C4-N3)

**What**: replace string-substring check with read-only transaction enforcement. Open LadybugDB connection with read-only mode for MCP queries. Keep substring matcher as defense-in-depth first gate, with comments explaining its limitations.

**Where**: `src/mcp_server.rs:489` `tool_query_graph`.

**Done when**: a deliberately-crafted DELETE query is rejected at the transaction level (not just the substring check), with a clear error message.

### T6-4 (CV5) — Resource profile boundary documentation (C4-N4)

**What**: add "Boundary Behavior" section to `docs/resource_profiles.md` describing GB floor discretization. Include explicit recommendation: when running on a boundary-VM (1.9GB, 5.9GB, etc.), operator should configure profile manually.

**Where**: edit `docs/resource_profiles.md`.

---

## <a id="tier-7"></a>TIER 7 — Hands-Off 7-Day Proof (re-attempt with clean state)

Now that Tier 1 cleaned everything and Tier 2-6 stabilized the foundation, the 7-day proof becomes meaningful.

### T7-1 (CV5) — Day-0 readiness checklist

The 7day_handoff.md doc already specifies Day-0 criteria. Run them. If any fails, fix before starting the clock.

Specifically validate:
- Single mgmt network: `docker network ls | grep bonsai-mgmt` returns exactly one
- Crons installed: `crontab -l` shows the expected entries
- GITHUB_TOKEN works: `bash scripts/cloud/daily_sync.sh --dry-run` exits 0
- Day-0 daily check: `bash scripts/bv5_daily_check.sh` exits 0 with status `pass`
- Feature index updated: T3-1 doc exists and recent

### T7-2 (CV5) — Start the clock

Record start timestamp in `runtime/driver_results/handoff_start.txt`. From this point, operator's only daily action is `git pull` + reading the daily report from the laptop side; checking the cloud sync branch each morning.

### T7-3 (CV5) — Daily validation (Days 1-6)

Per the existing handoff doc. Escalation thresholds apply. If anything breaks, fix and restart the 7-day window.

### T7-4 (CV5) — Day-7 closure

Run final daily check. Verify:
- 7 daily reports written
- 6 sync branches on origin (Day-1 sync is the first; Day-7 hasn't synced yet at closure time)
- ≥5 days of pass / pass_with_caveats verdicts (1 day of fail allowed if fixed)
- Archive bytes growing visibly
- Chaos cycle uninterrupted (≤2 restart markers)
- Resource governor activated at least once (validates T6-1) — synthetic load test on Day 4 is OK

**Done when**: Day-7 closure document declares success. Operator can trust hands-off operation.

---

## <a id="tier-8"></a>TIER 8 — GNN Path Forward

Triggered when Tier 7 succeeds AND archive depth ≥ 30 days post-cleanup AND chaos injection count ≥ 500 AND per-rule examples ≥ 50.

### T8-1 (CV5) — Vendor-neutral feature engineering

Per the GNN philosophy section. Audit `python/bonsai_ml/gnn/data_loader.py` feature vectors. Ensure structural features (degree, role-quartile, protocol-set, event-rate, time-since-event) are dominant. Vendor identity becomes a small one-hot tail, not a primary feature.

### T8-2 (CV5) — Multi-topology training data (when SP archive exists)

Train on DC + SP combined chaos archive. Held-out test on most-recent 5 days. Adversarial cases weighted ≥15%.

### T8-3 (CV5) — Comparison study (rules vs tabular ML vs GNN)

Use harnesses from Bv5. Honest confusion matrix per detector type.

### T8-4 (CV5) — Calibration phase support in inference path

Operations workspace gains a "calibration mode" toggle. When ON, GNN scores are computed and stored but no detections fire downstream. Toggle off after operator review of 7-day score distribution.

### T8-5 (CV5) — Model card with explicit generalization boundaries

The model card says: "trained on labeled chaos from DC (SR Linux) + SP (vendor TBD); structural-feature-dominated; generalization to other vendor implementations is not validated; calibration recommended before deployment-as-detector."

---

## <a id="tracked"></a>Tracked Future Threads

From prior conversations. Not built in CV5; preserved without architectural lock-in:

- **Scale-up architecture paths A/B/C**: Path A (vertical scale-up) is current. Path B (partitioned cores) and C (read replicas) require schema and bus changes we haven't precluded.
- **K8s deployment**: post-GNN. Helm chart with single-node, HA-core, collector-fleet shapes.
- **Cloud platform deployability docs**: per-platform recipes when we deploy there.
- **Beyond network platforms**: firewalls, VPN, cloud networking — positioning expansion post-GNN.
- **eBPF spike**: timeboxed exploration, single Linux host, ~1 week, after Tier 6-7 land.
- **Online learning infrastructure**: labeled feedback loop in UI; multi-deployment model evolution.
- **Investigation agent**: still parked behind token budget.

---

## <a id="motivation"></a>Code Quality, Motivation, Where We Are

### Code quality

**Overall: substantially mature.** ~75K Rust + ~16K Python is a real codebase. The architectural shape is stable across CV1-CV4 — discovery-driven layered ingestion is real, the bus + write_coordinator pattern is correct, the synthesizer is operator-approvable. New code lands in modules with consistent shape (struct + trait + tests). CV4's MCP server is a clean piece of work (728 lines including a substantive rule catalogue with concrete recurrence indicators).

**The recurring quality concern is operational integration, not code construction.** CV2 had dead code (registry+parser_chain unwired). CV3 had test framework smell (skip-as-pass). CV4 has governor-without-action-plumbing. The pattern: each sprint builds the right shape, but skips one or two wires that connect the shape to the running system. The wiring checks in CV2 caught the most blatant version; the more subtle versions (governor flags read by ingest) still slip through.

The fix is process, not architecture: every new module ships with at least one test that exercises its observable effect on the running system, not just its internal state. CV5 Tier 3 (feature index) institutionalizes this by tracking last-validation-date per feature.

### Motivation — where we are vs where we intend to be

**The honest framing**: bonsai has gone from "ambitious experiment" to "deployable foundation that hasn't been operationally proven." That's a meaningful place to be. Most personal-learning projects don't reach the foundation stage. From here, the work is integration discipline, not architectural exploration.

**The remaining mile**: hands-off 7-day proof, SP lab to broaden the chaos archive, GNN training with vendor-neutral features and calibration support, then deployable-as-tool. 8-14 weeks of work depending on SP lab bring-up cycle. Most of the engineering is done.

**The thing worth celebrating**: the GNN philosophy section above is genuinely thought-out and addresses a real concern. The MCP server is genuinely useful and positions bonsai as a credible AIOps feeder. The resource governor — even with the action-plumbing gap — is a substantively better-than-naive scale-readiness story. None of these are off-the-shelf patterns; they're products of real engineering thought applied to a specific problem.

**The thing to be honest about**: operational discipline is the hardest part of every infrastructure project. CV5 explicitly puts it before more code. That's the right move. The reset-and-rebuild pattern is psychologically heavy but technically correct.

---

## <a id="execution-order"></a>Execution Order

### Sprint 1 (1 week) — Cleanup
1. T1-1 laptop cleanup
2. T1-2 cloud cleanup
3. T1-3 pre-CV5 freeze
4. T2-1 mgmt-net audit

### Sprint 2 (1 week) — Standards
5. T2-2 single mgmt-network standardization
6. T2-3 config decoupling from lab name
7. T3-1 feature testing index
8. T3-2 daily-check integration
9. T3-3 Gemini brief update
10. T4-1 lab placement policy doc
11. T4-2 resource budget doc

### Sprint 3 (2 weeks) — SP research and spec
12. T5-1 SP lab vendor research
13. T5-2 SP lab full specification

### Sprint 4 (1 week) — Code review findings
14. T6-1 governor active control plumbing
15. T6-2 adapter cursor persistence
16. T6-3 MCP read-only hardening
17. T6-4 profile boundary docs

### Sprint 5 (1-2 weeks) — SP lab bring-up
18. T5-3 SP bring-up
19. T5-4 SP chaos catalogue

### Sprint 6 (7 days wall clock) — 7-day proof
20. T7-1 Day-0 readiness
21. T7-2 start the clock
22. T7-3 daily validation
23. T7-4 closure

### Sprint 7 (3-4 weeks, gated on Tier 7 + archive depth) — GNN
24. T8-1 vendor-neutral feature engineering
25. T8-2 multi-topology training
26. T8-3 comparison study
27. T8-4 calibration phase
28. T8-5 model card

**Estimated total**: 8-14 weeks. Heavily dependent on SP lab bring-up (Sprint 3+5) which could take 1 week or 3 weeks based on T5-1 outcome.

---

## <a id="guardrails"></a>Guardrails

### New in CV5

- **Single management network is invariant.** All labs use `bonsai-mgmt` network name; per-lab IPs unique but never collide; bringing up any lab uses the same docker network.
- **Lab placement is exclusive per environment.** Never two architectures on the same VM. Laptop = DC. Cloud = DC initially, SP when ready.
- **Pre-specification before bring-up.** Especially for SP lab: spec exists and is reviewed before any clab inspect runs.
- **Feature index is canonical.** Every bonsai feature has a row with current test status; daily check updates it.
- **Governor flags are read by control points, not just observed.** No more passive-monitor anti-pattern.
- **GNN training philosophy is vendor-neutral features + per-deployment calibration.** Lab is supervised data source; production is calibration data source.

### Unchanged from v7-CV4

All prior architectural invariants. Streaming-first hot path. Vault-only credentials. Layered ingestion. Discovery-driven onboarding. Agent-friendly read APIs.

### Anti-patterns to reject

- "Skip the cleanup, just rebuild on top of what's there" — no. CV5 starts with zero. Hidden state has caused enough churn.
- "Bring up SP lab and figure it out as we go" — no. Pre-spec.
- "Train GNN on whatever data exists, deploy as-is" — no. Vendor-neutral features + calibration are the deployment contract.
- "Governor observability is enough" — no. Action plumbing is required.
- "Mgmt network can vary per lab" — no, locked invariant.

---

## What CV5 Explicitly Excludes

- All tracked-future items (scale-up paths, K8s, beyond-network positioning, eBPF, online learning infrastructure)
- Multi-cloud K8s deployments
- Wireless / hardware-FRU / optical chaos
- Auto-execution of synthesizer recommendations
- LLM-mediated reasoning in the streaming hot path
- Investigation agent productive use without token budget

---

*CV5.0 — authored 2026-05-12 after comprehensive review of CV4 landings (resource governor, MCP server, adapter cursor fix, parquet rotation, install_cron, 7-day handoff doc). Identifies six findings (C4-N1 cursor startup window; C4-N2 governor observability-without-action — HIGH; C4-N3 MCP Cypher substring matching; C4-N4 profile boundary discretization; C4-N5 operational discipline drift since 5-11; C4-N6 archive empty despite chaos). Drives complete laptop+cloud cleanup before rebuild. Establishes single management network as invariant. Introduces central feature testing index replacing scattered tests. Commits to lab placement decision (DC laptop, SP cloud when ready). Specifies SP lab vendor research before bring-up. Closes the governor action-plumbing gap. Re-attempts hands-off 7-day proof. GNN training philosophy section addresses operator's question about scope: lab provides labeled supervised data, production provides calibration data, vendor-neutral structural features + per-deployment calibration is the deployment contract. Estimated 8-14 weeks. References v7-CV4 for unchanged context.*
