# BONSAI — Consolidated Backlog v12.0

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_V11.md`. Produced 2026-05-03 after focused, chunk-based code review of post-v11 main with explicit instruction to perform independent analysis rather than restating concerns as bullets.
>
> **What v12 is**: a sharply-focused round of validation and stabilisation. v11 landed substantial infrastructure (DC + SP labs, external compose, four feedback-driver dirs, three new CI workflows, disk_guard, memory_profile, archive compression hardening). v12 walks through the residual problems the operator observed (9 GB memory, slow startup, empty Incidents tab, "disconnected" Collectors, environment churn) and treats each as a concrete code-grounded bug rather than a theme.
>
> **The smoking gun for the 9 GB memory bug is identified in this review** (Tier 1, F-1 below): `Database::new(path, SystemConfig::default())` in `src/graph/mod.rs:149`. Kuzu/lbug's default `SystemConfig` allocates a buffer pool sized to ~80% of system RAM. On a 16 GB laptop this is ~12 GB; the observed 9 GB is consistent. Cap it explicitly and the symptom evaporates.
>
> **Document discipline**: prior backlogs (v2-v11) remain in the repo. Strategic positioning, audience framing, gNMI-only hot path, controller-less primary target, enrichment-as-differentiator, AIOps-feeder framing, HIL graduated remediation, OutputAdapter architecture all remain unchanged and are referenced rather than restated. v12 spends real estate on the new findings.

---

## Table of Contents

1. [Audience and Positioning](#positioning) — see v7
2. [Progress Since v11](#progress)
3. [Independent Code Findings (F-1 through F-12)](#findings) — read this first
4. [TIER 0 — Bugfixes from Findings](#tier-0)
5. [TIER 1 — Memory Architecture (the 9 GB bug)](#tier-1)
6. [TIER 2 — Binary Self-Containment](#tier-2)
7. [TIER 3 — Always-On Lab and External Infrastructure](#tier-3)
8. [TIER 4 — UI/API Liveness and Ground-Truth Verification](#tier-4)
9. [TIER 5 — Startup Time Investigation](#tier-5)
10. [TIER 6 — Carryover from v11](#tier-6)
11. [Execution Order](#execution-order)
12. [Guardrails](#guardrails)

---

## <a id="positioning"></a>Audience and Positioning

**Unchanged from v7-v11.** Primary: controller-less environments (DC, campus wired/wireless, SP backbones). Secondary: multi-controller correlation. AIOps integration as feeder, not replacement. See `BONSAI_CONSOLIDATED_BACKLOG_V7.md` for full rationale.

---

## <a id="progress"></a>Progress Since v11 — Verified Against Main

v11 landed substantial work. Verified by code review.

| v11 item | Status | Evidence |
|---|---|---|
| T1-1 v11 compose-external.yml umbrella | ✅ Done | `docker/compose-external.yml` with `netbox`, `splunk`, `elastic`, `prometheus`, `all` profiles |
| T1-2 v11 seed_external.sh | ✅ Done | `scripts/seed_external.sh`, `scripts/seed_splunk.py`, `scripts/seed_elastic.py` |
| T1-3 v11 configure_external.sh | ✅ Done | `scripts/configure_external.sh` |
| T1-4 v11 check_external.sh JSON output | ✅ Done | `scripts/check_external.sh` |
| T1-5 v11 documentation | ✅ Done | `docs/external_infra.md` |
| T2-1 v11 DC EVPN-SRv6 topology | ✅ Done | `lab/dc/dc-evpn-srv6.clab.yml` + `lab/dc/configs/`, `lab/dc/README.md` |
| T2-2 v11 SP MPLS-SRTE topology | ✅ Done | `lab/sp/sp-mpls-srte.clab.yml` + `lab/sp/configs/`, `lab/sp/README.md` |
| T2-5 v11 lab readiness probe | ✅ Done | `scripts/check_lab.sh` |
| T2-6 v11 fault catalogue | ✅ Done | `lab/fault_catalog.yaml` |
| T3-1 v11 Playwright UI driver | ✅ Done | `tests/ui_driver/` |
| T3-2 v11 API contract driver | ✅ Done | `tests/api_driver/` |
| T3-3 v11 event-stream driver | ✅ Done | `tests/event_driver/` |
| T3-4 v11 chaos harness | ✅ Done | `tests/chaos_harness/` |
| T3-5 v11 unified status emitter | ✅ Done (assumed; needs verify in `src/http_server.rs`) |
| T3-6 v11 ai_feedback_protocol.md | ✅ Done | `docs/ai_feedback_protocol.md` |
| T3-7 v11 CI integration | ✅ Done | `.github/workflows/feedback-loop.yml` |
| T3-8 v11 AI consumption examples | ✅ Done | `docs/ai_feedback_examples.md` |
| T4-1 v11 memory profiling | ✅ Done | `src/memory_profile.rs` + `--memory-profile` flag |
| T4-2 v11 identify 9 GB culprit | ⚠️ Partial | `docs/test_results/memory_investigation/` exists; whether the LadybugDB buffer pool root-cause was identified is to be verified. **F-1 below states the cause directly.** |
| T4-3 v11 fix culprit(s) | ❌ Not yet done — F-1, F-2, F-3 below |
| T4-4 v11 DB compression audit | ✅ Done | `src/archive.rs` `writer_properties` allows configurable compression level |
| T4-5 v11 disk-aware sizing | ✅ Done | `src/disk_guard.rs` |
| T4-6 v11 memory/disk panels | ✅ Done | `ui/src/routes/Operations.svelte` extended |
| T4-7 v11 CI memory budget | ✅ Done | `.github/workflows/memory-budget.yml` |
| T4-8 v11 resource contract doc | ✅ Done | `docs/resource_contract.md` |
| T2-3 v11 campus topology | Deferred — operator chose not to scope |
| T2-4 v11 lab-aware compose profiles | ✅ Done | `docker-compose.yml` updated; `docker/configs/lab-dc.toml`, `lab-sp.toml` |

v10 carryover (PDI live tests, HIL e2e) still pending operator inputs.

**Two findings from this review on v11 work**:
- The memory-budget CI workflow was created but **likely fails** today because F-1 below has not been fixed; the budget is exceeded by the LadybugDB default buffer pool alone.
- The chaos harness exists but **fault catalogue → harness wiring needs end-to-end run** to confirm matrix output is real.

---

## <a id="findings"></a>Independent Code Findings — F-1 through F-12

These are the concrete code-grounded findings from this review session. **Each maps to a Tier 0 fix or a Tier 1-5 task.**

### F-1 — LadybugDB buffer pool defaults to ~80% of system RAM (the 9 GB bug)

**Location**: `src/graph/mod.rs:149`

```rust
let db = Database::new(path, SystemConfig::default()).context("failed to open LadybugDB")?;
```

**Issue**: `SystemConfig::default()` in lbug 0.15.3 (Kuzu fork) sets `buffer_pool_size = 80% of detected system memory`. On a 16 GB laptop this allocates ~12 GB, growing as the DB is exercised. **This is the dominant cause of the observed 9 GB consumption.** It is not a leak — it is a configured cache that bonsai never overrode.

**Why this matters for the audience**: bonsai targets resource-sparse systems. The "slim Rust core" pitch is undermined the moment LadybugDB grabs 12 GB. Operators on small VMs or edge devices cannot run bonsai at all without this fix.

**Fix**: explicit, conservative buffer pool sizing.
- Default: `min(2 GB, 25% of system RAM)` for core; `min(256 MB, 10% of system RAM)` for collector (collector graph is small by design)
- Configurable via `[graph.buffer_pool_bytes]` in bonsai.toml
- Logged at startup: "LadybugDB buffer pool: 2.0 GB (configured)" so the value is visible in support diagnostics

This is **the single highest-leverage fix in v12**. Estimated 1-day effort, 80% memory reduction.

### F-2 — Broadcast channel × subscriber count × per-message clone

**Location**: `src/event_bus.rs:43`, `src/main.rs:87`

```rust
pub fn new(capacity: usize) -> Arc<Self> {
    let (tx, _) = broadcast::channel(capacity);
    ...
}
```

Default capacity 2048. Subscriber inventory across the codebase:
1. `archive.rs:78` — archive writer
2. `ingest.rs:349` — collector forwarder
3. `main.rs:254` — graph writer
4. `subscription_status.rs:43` — subscription status tracker
5. `output/prometheus.rs:130` — Prometheus adapter
6. `output/traits.rs:321` × 3 — Splunk + Elastic + ServiceNow EM (when enabled)
7. SSE handlers in `http_server.rs` — one per active UI client

That's 6-10+ static subscribers, plus dynamic SSE clients.

**Issue**: `tokio::sync::broadcast` semantics — a message stays in the channel ring buffer until *every* subscriber has read it. One slow subscriber holds memory for all subscribers. With ~9 subscribers and 2048 capacity, worst-case is 18,432 cached `TelemetryUpdate` instances. Each is 6 `String` fields plus a `JsonValue` that for blob updates (BGP neighbor full state, network-instances) can reach 10-50 KB. Worst-case bound: ~1 GB just for the bus ring buffer.

This is **not a leak** — it's bounded by capacity — but it compounds with F-1 and F-3.

**Fix**:
- Reduce default capacity to 512
- Add per-subscriber lag tracking — log a warning + metric increment when a specific subscriber has been >50% behind for >10s
- Document the worst-case memory bound in `docs/resource_contract.md`

### F-3 — Graph writer's debounce HashMap has no eviction

**Location**: `src/main.rs:256`

```rust
let mut last_counter_write: HashMap<String, Instant> = HashMap::new();
```

**Issue**: keyed by `format!("{}:{}", target, if_name)`. Never evicted. Over a long-running deployment (especially with chaos / link-flap / interface-add scenarios), entries accumulate forever. This is a real memory leak, distinct from the counter summarizer (which v11 fixed with `max_entries: 1024`).

Per entry: ~50-byte key + 16-byte `Instant` = ~70 bytes. For 100,000 historical interfaces over a year that's 7 MB — negligible. **But it's the same anti-pattern as the counter summarizer, and the principle of bounded-by-config-not-runtime applies.**

**Fix**: same pattern as v11's counter summarizer fix. Cap at 1024 entries, evict by `last_update_ts`. Or replace with `lru::LruCache`.

### F-4 — Binary not self-contained: requires `LD_LIBRARY_PATH`

**Location**: `.cargo/config.toml:7`, `build.rs`, `docker/Dockerfile.bonsai:80`

The build sets `LBUG_SHARED=1` which produces `liblbug.so.0` as a shared object. The Dockerfile correctly bundles it into `/usr/local/lib/` and sets `LD_LIBRARY_PATH=/usr/local/lib`. **But there is no static-link path and no `RPATH` baked into the binary.** A user (or AI agent) who builds locally with `cargo build --release` and tries to run `./target/release/bonsai` gets:

```
error while loading shared libraries: liblbug.so.0: cannot open shared object file
```

This is exactly the friction the operator is reporting. AI agents that build and try to run the binary directly hit this every time.

**Fix options** (any of):
- (A) Add `[build] rustflags = ["-C", "link-arg=-Wl,-rpath,$ORIGIN"]` so the binary searches its own directory; bundle `liblbug.so.0` next to `bonsai`
- (B) Build a fully static Linux binary using `LBUG_STATIC=1` or by linking the `lbug` static lib (preferred — produces a single self-contained binary)
- (C) Switch to bundled `lbug` via `staticlib` Cargo feature if upstream provides one

**Recommended**: (B) static link by default for release builds; provide `LBUG_DYNAMIC=1` opt-out for development if the dynamic build is faster to iterate on. **Self-contained binary is non-negotiable for the resource-sparse-systems audience.**

### F-5 — Compose external services are not always-on

**Location**: `docker/compose-external.yml`

**Issue**: top-level external services (NetBox, Splunk, Elastic, Prometheus) lack `restart: unless-stopped`. Laptop reboot or accidental `docker compose down` brings them down. The operator then re-seeds, re-waits, re-burns AI cycles on environment churn — the exact failure mode v11 was designed to prevent.

**Fix**: every service in `compose-external.yml` gets `restart: unless-stopped`. ContainerLab device containers separately get `restart: unless-stopped` via the deploy script.

### F-6 — Test scripts must be able to reset external state via API

**Issue**: the v11 testing infrastructure assumes a fresh environment per test run. But the external services should stay up. Test scripts therefore need API-driven *data reset* hooks:
- NetBox: delete devices/sites/VLANs/prefixes via API; re-seed from `lab/seed/topology.yaml`
- Splunk: delete index contents (`POST /services/data/indexes/<name>/clean`)
- Elastic: delete index, recreate from template
- Prometheus: doesn't need reset (TSDB time-windows)
- ServiceNow PDI: delete bonsai-created CIs via filtered table query

**Fix**: each existing seed script gets a `--reset` flag that wipes bonsai-managed data before reseeding. Test scripts call `seed_<service>.py --reset` at the start of each scenario. The services themselves stay up.

### F-7 — UI workspaces are mostly fetch-once-on-mount

**Location**: `ui/src/routes/Incidents.svelte:12`, `ui/src/routes/Collectors.svelte:13`, and similar in Devices/Sites/Environments/Adapters/Approvals

**Issue**: only `lib/Events.svelte` and `lib/Onboarding.svelte` use SSE (`/api/events`). Every workspace route does `onMount(() => fetch(...))`. State that changes after page load (a collector reconnecting, a new detection firing, a remediation completing) is invisible until manual refresh.

**This is the structural cause of "Collectors shows disconnected" and "Incidents tab is empty" reports** — the UI showed a snapshot from when the page loaded, not current state.

**Fix**: extend the existing `/api/events` SSE channel to broadcast workspace-relevant events:
- `collector_status_change` — collector connect/disconnect/heartbeat-stale
- `detection_fired` — new detection event
- `incident_grouped` — newly-formed or updated incident
- `remediation_outcome` — playbook executed
- `enricher_run_completed` — enricher finished a pass
- `adapter_health_change` — adapter became healthy/unhealthy

Each route subscribes to relevant event types and patches its in-memory state. Polling fallback (60s interval) for browsers without SSE.

This is **not** a new feature — the SSE channel already exists; it just doesn't broadcast the events these workspaces need.

### F-8 — Empty Incidents tab is real-but-misdiagnosed

**Issue**: detection events come in via `DetectionEventIngest` RPC from the collector or via direct `write_detection`. **The Python rule engine on the collector must be running and producing detections, or the Incidents API returns an empty list.**

The user observed an empty Incidents tab. Causes are:
- Rule engine not running (configuration issue)
- Rules not matching (lab telemetry not exercising any rule's trigger)
- Detection events fired but write to graph failed (silent error)

**Fix**: the chaos harness from v11 should drive the fault catalogue and verify "fault X → detection Y appears in `/api/incidents`." If the matrix shows zero detections firing, that's the diagnosis.

This is a **testing-infrastructure issue**, not a code bug. Captured in T4-3 below.

### F-9 — Bonsai startup time

**Issue**: operator reports "quite some time" for startup. Looking at `main.rs`:

1. Crypto provider install (line 39) — instant
2. CLI subcommand parse (40-49) — instant
3. Tracing subscriber (51-56) — instant
4. Config load (64) — async file read, fast
5. Prometheus exporter install (75-84) — fast HTTP listener bind
6. Bus creation (87) — instant
7. **GraphStore::open** (line 219-232) — `spawn_blocking`, calls `Database::new` which:
   - Creates the Kuzu database files if they don't exist
   - Initialises the buffer pool (allocates the 12 GB from F-1!)
   - Loads existing graph state
8. **`init_schema`** (line 156) — runs DDL
9. **`backfill_remediation_trust_marks`** (line 157) — likely a one-time migration

The likely dominant time is steps 7-8. Allocation of multi-GB buffer pool is not free (page-in time). After F-1 fix this drops dramatically because allocation is small.

**Fix-as-side-effect-of-F-1**: smaller buffer pool means faster startup. After F-1, also instrument every startup phase with timing logs:

```
INFO bonsai: phase=config_load        elapsed_ms=12
INFO bonsai: phase=graph_open         elapsed_ms=480
INFO bonsai: phase=schema_init        elapsed_ms=85
INFO bonsai: phase=backfill           elapsed_ms=210
INFO bonsai: phase=collectors_connect elapsed_ms=1200
INFO bonsai: phase=ready              elapsed_ms=1987
```

This becomes the diagnostic for any future startup regression.

### F-10 — Compression level configurable but not yet documented as tunable

**Location**: `src/archive.rs::writer_properties`

V11 added configurable compression level. **But what level the operator should pick is not documented.** ZSTD level 3 (default) vs 9 vs 12 vs 22 has wildly different CPU/size tradeoffs.

**Fix**: `docs/archive_compression.md` with measured numbers from a representative workload. One paragraph on dictionary encoding. Not urgent — minor doc polish.

### F-11 — Test scripts run as one-shots; no programmatic teardown of bonsai

**Issue**: the v11 e2e scripts bring up bonsai via compose, run scenarios, but on failure leave bonsai in an indeterminate state. The next test run inherits the leftovers.

**Fix**: each e2e script ends with a guaranteed cleanup block (trap on EXIT) that:
- Tears down bonsai compose
- Runs `seed_<service>.py --reset` for each external service
- Leaves external services up (per F-5)

### F-12 — Sprint metrics in the UI use a one-shot fetch too

**Location**: `ui/src/routes/Operations.svelte`

Same pattern as F-7. The Operations workspace shows memory and disk metrics from a single `/api/_test/status` (or whatever the v11 unified status endpoint is named) call. **Memory growth over time is invisible from the UI** — the operator cannot watch RSS climb.

**Fix**: Operations workspace polls `/api/operations/status` every 5 seconds and renders a small live time-series of RSS, archive size, graph size. Lightweight, no external dashboard needed.

---

## <a id="tier-0"></a>TIER 0 — Bugfixes from Findings

### T0-1 (v12) — Cap LadybugDB buffer pool (F-1)

The single most important fix. ~30 lines.

```rust
// src/graph/mod.rs
let buffer_pool_bytes = cfg.graph.buffer_pool_bytes
    .unwrap_or_else(|| compute_default_buffer_pool());
let mut sysconfig = SystemConfig::default();
sysconfig.buffer_pool_size = buffer_pool_bytes;
let db = Database::new(path, sysconfig).context("failed to open LadybugDB")?;
info!(buffer_pool_mb = buffer_pool_bytes / 1024 / 1024, "LadybugDB opened");
```

`compute_default_buffer_pool()`: `min(2 GB, 25% of detected RAM)` for core; `min(256 MB, 10%)` for collector.

Same fix in `src/collector/graph.rs::CollectorGraphStore::open`.

**Done when**: a fresh bonsai run shows RSS plateau at <2.5 GB instead of 9 GB; CI memory-budget workflow goes green.

### T0-2 (v12) — Bound graph writer debounce HashMap (F-3)

Replace `HashMap` with `lru::LruCache`, capacity 1024. ~10 lines.

### T0-3 (v12) — Reduce default bus capacity, add slow-subscriber lag metric (F-2)

- `default_bus_capacity()` → 512 (was 2048)
- Add per-receiver lag tracking by subscribing to a metrics channel
- Document worst-case bound in `docs/resource_contract.md`

### T0-4 (v12) — Static-link lbug for self-contained binary (F-4)

The big architectural win. Two commits:
- (a) Provide `LBUG_STATIC=1` build path that links `lbug` as a static library; verify `ldd target/release/bonsai` shows no `liblbug.so.0` dependency
- (b) Update CI to produce static-link release artefacts; update Dockerfile to use static build (drops the `find liblbug.so.0` step)

**Done when**: `cargo build --release` produces a binary that runs standalone with no `LD_LIBRARY_PATH` and no shared-lib dependencies on lbug.

### T0-5 (v12) — Always-on compose policy (F-5)

Add `restart: unless-stopped` to every service in `compose-external.yml` and `docker-compose.yml`.

### T0-6 (v12) — Add `--reset` flag to seed scripts (F-6)

Each of `seed_netbox.py`, `seed_servicenow_pdi.py`, `seed_splunk.py`, `seed_elastic.py` gains a `--reset` flag that deletes bonsai-managed data before re-seeding. Idempotent. Used by test scripts.

### T0-7 (v12) — SSE event broadcasting for workspace updates (F-7)

Extend the existing `/api/events` SSE channel to broadcast workspace-relevant events. Each UI workspace subscribes to relevant event types. Polling fallback at 60s.

### T0-8 (v12) — Startup phase timing logs (F-9)

Wrap each startup phase in `main.rs` with `let phase_start = Instant::now(); ... info!(phase=..., elapsed_ms=phase_start.elapsed().as_millis() as u64)`. Enables future regression detection.

### T0-9 (v12) — Test script cleanup-on-exit (F-11)

Each `scripts/e2e_*.sh` gets `trap cleanup EXIT` invariant.

### T0-10 (v12) — Live polling for Operations workspace (F-12)

Operations route polls `/api/operations/status` every 5s; small client-side ring buffer renders RSS / archive size / graph size as a sparkline.

---

## <a id="tier-1"></a>TIER 1 — Memory Architecture (the 9 GB bug)

### T1-1 (v12) — Validate F-1 fix end-to-end

**What**: with T0-1 in place, run a representative workload (lab DC topology, full subscription, 30 minutes) with `--memory-profile`. Confirm:
- RSS plateaus under 2.5 GB
- LadybugDB allocation as logged at startup matches the configured cap
- No GB-scale growth over the 30-minute window
- Counter summarizer entries stable around N (interface count)
- Bus depth stays low

Capture in `docs/test_results/memory_investigation/post_t0_1_validation.md`.

### T1-2 (v12) — Memory budget assertions in CI

**What**: the v11 `memory-budget.yml` workflow likely fails today because of F-1. After T0-1 fix, set the budget to 1.5 GB peak RSS for the 10-minute synthetic-load run. Failure means a regression.

### T1-3 (v12) — Document the resource contract numbers

**What**: `docs/resource_contract.md` exists from v11 — update with measured numbers from T1-1. Specifically:
- Peak RSS for 4-device lab: target <500 MB
- Peak RSS for 8-device DC + 9-device SP lab: target <1.5 GB
- Per-device memory cost: linear, ~50 MB per active subscription
- Bus ring buffer worst-case bound: capacity × max_message_size × subscriber_count

### T1-4 (v12) — Add memory health check to `/api/operations/status`

**What**: status endpoint returns:

```json
{
  "memory": {
    "rss_bytes": 480000000,
    "rss_pct_of_budget": 32.0,
    "ladybug_buffer_pool_bytes": 2147483648,
    "bus_capacity": 512,
    "bus_depth": 12,
    "bus_subscribers": 9,
    "counter_summarizer_entries": 48,
    "graph_writer_debounce_entries": 48
  }
}
```

UI polls this at 5s (T0-10).

---

## <a id="tier-2"></a>TIER 2 — Binary Self-Containment

### T2-1 (v12) — Static lbug build path (F-4)

Covered by T0-4. Specific implementation tasks:
1. Investigate whether lbug 0.15.3 supports `staticlib` build
2. If yes: add Cargo feature `static-lbug` and use it in release profile
3. If no: bundle `liblbug.so.0` next to the binary with `RPATH=$ORIGIN` as a fallback
4. Update Dockerfile to drop the `find liblbug.so.0` step
5. Add a CI assertion: `ldd target/release/bonsai | grep -c liblbug` returns 0

### T2-2 (v12) — Release artefact pipeline

**What**: GitHub Actions workflow that produces signed binary artefacts on release tags:
- `bonsai-linux-amd64` — static-linked, runs standalone
- `bonsai-linux-arm64` — same
- `bonsai-darwin-amd64` and `bonsai-darwin-arm64` — for Mac developers
- Each binary uploaded as a release asset

**Done when**: a tagged release produces downloadable binaries that `chmod +x && ./bonsai --version` works on the target OS.

### T2-3 (v12) — `bonsai healthcheck-build` self-test

**What**: a one-shot `bonsai self-test` subcommand that exercises the binary's runtime dependencies and reports:

```
$ bonsai self-test
[✓] LadybugDB linkage:       static (no shared libs)
[✓] crypto provider:         rustls-aws-lc-rs
[✓] tokio runtime:           OK
[✓] gRPC client:             OK
[✓] config parser:           OK
Binary ready.
```

Useful for AI agents to verify before further automation. ~50 lines.

---

## <a id="tier-3"></a>TIER 3 — Always-On Lab and External Infrastructure

### T3-1 (v12) — `restart: unless-stopped` everywhere (F-5)

Covered by T0-5.

### T3-2 (v12) — Lab compose with persistence (F-5)

ContainerLab is currently brought up via `containerlab deploy -t lab/dc/...clab.yml` which is one-shot. **The DC and SP labs are not part of compose-external.yml.** Wrap them:

- `lab/dc/Makefile` with `up`, `down`, `status`, `reset` targets
- `containerlab deploy --reconfigure` for idempotent re-deploy
- Document the once-per-laptop bring-up pattern: `make -C lab/dc up && make -C lab/sp up && docker compose -f docker/compose-external.yml --profile all up -d`

After this single command sequence, **everything stays up across reboots** unless explicitly torn down.

### T3-3 (v12) — Test data reset, not infrastructure tear-down (F-6)

Covered by T0-6 (seed --reset flags). Plus:

- A wrapper script `scripts/reset_for_test.sh` that calls every `seed_*.py --reset` plus restarts bonsai-core (not the external services)
- Documented as the canonical test-prep step

### T3-4 (v12) — Health probes for always-on services

**What**: `scripts/check_external.sh` (already exists from v11) gets a `--watch` mode that polls every 30s and emits status to a known location. UI's Operations workspace reads from this for the external-services panel.

### T3-5 (v12) — Lab health probe extension

**What**: `scripts/check_lab.sh` (already exists from v11) confirms not just that ContainerLab containers are up, but that each device's startup config has actually loaded (BGP sessions established, IS-IS adjacencies up, EVPN routes propagated). Returns JSON for AI consumption.

```json
{
  "topology": "lab/dc/dc-evpn-srv6.clab.yml",
  "devices_up": 8,
  "devices_total": 8,
  "bgp_sessions_established": 14,
  "bgp_sessions_total": 14,
  "evpn_routes_present": true,
  "srv6_reachability_verified": true,
  "warnings": []
}
```

---

## <a id="tier-4"></a>TIER 4 — UI/API Liveness and Ground-Truth Verification

### T4-1 (v12) — SSE event broadcasting (F-7)

Covered by T0-7. Specific event types to add:

| Event | Producer | Consumer routes |
|---|---|---|
| `collector_status_change` | core API on heartbeat-stale or reconnect | Collectors, Live |
| `detection_fired` | core on `write_detection` | Incidents, Live |
| `incident_grouped` | core after grouping pass | Incidents |
| `remediation_outcome` | core after playbook execution | Approvals, Incidents |
| `enricher_run_completed` | enrichment registry | Enrichment |
| `adapter_health_change` | adapter trait wrapper | Adapters, Operations |
| `setup_state_changed` | core on first credentials/site/env created | App.svelte (refresh setup status) |

### T4-2 (v12) — UI driver verifies what user sees, not what API returns (F-7)

**What**: the v11 `tests/ui_driver/` Playwright suite gets extended with **screen-level assertions** for each workspace:

For the Collectors workspace test:
- Bring up bonsai with two collectors
- Open `/collectors`
- Assert: at least one row shows `connected: true` badge in green (DOM check, not API check)
- Stop one collector container
- Wait 30 seconds
- Assert: that row shows `offline: true` badge in red **without page refresh**
- Restart the collector
- Wait 30 seconds
- Assert: row returns to green **without page refresh**

For Incidents:
- Inject a fault from `lab/fault_catalog.yaml`
- Wait for detection event (subscribe to `/api/events` in the test)
- Assert: at least one incident card visible on `/incidents` with the expected severity badge
- Heal the fault
- Assert: incident card eventually disappears or shows resolved state

This **catches F-7-class issues** — the UI displays what it last fetched, not current state — automatically.

### T4-3 (v12) — Detection-firing chaos matrix (F-8)

**What**: extend the v11 chaos harness to validate the empty-Incidents-tab diagnosis. For every fault in `lab/fault_catalog.yaml`:

```yaml
faults:
  - id: bgp-session-down-leaf1-spine1
    expected_detection: bgp_neighbor_down
    expected_window_seconds: 30
```

The harness:
1. Asserts pre-fault: no detections matching `expected_detection` for the affected device
2. Injects the fault
3. Waits up to `expected_window_seconds`
4. Asserts: `/api/incidents` returns an incident whose root rule_id matches `expected_detection`
5. Heals the fault
6. Asserts: incident shows resolved or window-closed state

Output: a matrix of `{fault_id: (expected, observed, latency_ms, passed)}` written to `docs/test_results/chaos_matrix/<date>.md`.

**This makes the iterative AI feedback loop concrete.** An AI session asks "which detections work?" — answer is one matrix file.

### T4-4 (v12) — Per-route screenshot diff in CI

**What**: Playwright captures a screenshot of each route after the workspace has loaded and SSE has connected. CI compares against a baseline; visual regressions flag.

**Where**: `tests/ui_driver/` extension.

**Done when**: a UI change that breaks the rendering of any workspace fails CI on screenshot diff.

### T4-5 (v12) — UI accessibility-and-correctness audit

**What**: a manual one-time review of every workspace asking:
- Are all visible numbers/labels correct against API ground truth?
- Are there UI elements that show stale data after navigation?
- Are there unhandled error states (loading-forever, blank-on-error)?
- Are interactions (button click, form submit) wired to backend handlers that exist?

Output: `docs/ui_audit_2026-05-03.md` — a checklist of issues by route. Becomes Tier 4 v13 if substantial. Effort: ~half day.

---

## <a id="tier-5"></a>TIER 5 — Startup Time Investigation

### T5-1 (v12) — Phase timing logs (F-9)

Covered by T0-8.

### T5-2 (v12) — Startup time budget in CI

**What**: the build-baseline workflow gets a complementary `startup-time.yml` that:
- Builds bonsai
- Times `./bonsai --once-and-exit` (a new flag that boots, reaches ready, and exits)
- Compares against a baseline; flags >25% regressions

**Done when**: PRs that slow down startup get a CI warning.

### T5-3 (v12) — Schema migration / backfill optimisation

**What**: `backfill_remediation_trust_marks` in `src/graph/mod.rs:157` runs on every startup. If it's a one-shot migration, it should set a marker in the DB and skip subsequent runs.

**Where**: `src/graph/mod.rs`.

**Done when**: second startup is significantly faster than first because backfill is skipped.

---

## <a id="tier-6"></a>TIER 6 — Carryover from v11

These remain valid; deprioritised behind v12 fixes.

- **From v11**: T0-1 verify v10 Tier 0 fixes line-by-line (low priority)
- **From v10**: T2-4 PDI live test, T2-5 PDI EM push live (operator-supplied PDI required)
- **From v10**: T3-3 a11y audit (subsumed by T4-5 v12)
- **From v9 strategic**: path overrides, AIOps readiness checklist, signals, GNN, investigation agent, controller adapters — all defer until after v12 polish

---

## <a id="execution-order"></a>Execution Order

**Sprint 1 v12 — Memory + binary fixes (1-2 weeks)** ⚡
1. T0-1 cap LadybugDB buffer pool — single most important fix
2. T0-2 graph writer HashMap eviction
3. T0-3 reduce bus capacity + slow-subscriber metric
4. T0-4 static-link lbug
5. T0-8 startup phase timing logs
6. T1-1 validate the fix with profile capture
7. T1-2 update memory-budget CI assertion
8. T1-3 update resource_contract.md numbers

**Sprint 2 v12 — Always-on infrastructure (1 week)**
9. T0-5 restart: unless-stopped policies
10. T0-6 seed script --reset flags
11. T3-2 lab Makefiles
12. T3-3 reset_for_test.sh wrapper
13. T0-9 e2e script trap cleanup
14. T2-2 release artefact pipeline
15. T2-3 bonsai self-test subcommand

**Sprint 3 v12 — UI liveness (2 weeks)**
16. T0-7 SSE event broadcasting
17. T0-10 Operations live polling sparklines
18. T4-1 SSE event types per workspace
19. T4-2 UI driver screen-level assertions
20. T4-3 chaos matrix wiring (close the empty-Incidents diagnosis)
21. T4-4 screenshot diff in CI
22. T4-5 UI audit doc

**Sprint 4 v12 — Startup polish (1 week)**
23. T5-2 startup time CI budget
24. T5-3 backfill skip on subsequent runs
25. T1-4 memory health in /api/operations/status
26. T3-4 always-on health watch
27. T3-5 lab health probe with feature-level assertions

**After v12 — return to v11/v10 carryover and v9 strategic threads**
- Path A → Path B GNN sequencing
- Investigation agent
- Signals tier
- Controller adapters demand-driven

---

## <a id="guardrails"></a>Guardrails

### New in v12

- **Memory bounded by configuration, not detected RAM.** Every cache-or-buffer-allocator-by-default-sized-to-RAM is explicitly capped in bonsai.toml. Any new dependency that does the same is wrapped at our boundary.
- **Binary is self-contained.** Release builds produce a single executable runnable on the target OS without `LD_LIBRARY_PATH`, sidecar `.so` files, or operator setup. Dynamic-link development builds are fine; release artefacts are static.
- **Infrastructure stays up across reboots.** External services and labs use `restart: unless-stopped`. Tests reset *data*, not *services*.
- **UI shows current state, not last-fetched state.** Every workspace either subscribes to relevant SSE events or polls at a documented interval. Fetch-once-on-mount alone is rejected for stateful views.
- **Test scripts verify what the user sees.** Screen-level assertions (DOM presence, badge colours, list lengths) complement API-level assertions. The chaos harness produces a matrix of fault → detection → UI-render that is the iterative feedback loop's primary artefact.
- **Startup time is measured, budgeted, and logged.** Every phase emits a timed log line; CI catches regressions.

### Unchanged from v7-v11

All prior architectural invariants and discipline continue. References v7 § Audience and Positioning, v9 § Guardrails, v11 § Guardrails for the full list.

### Anti-patterns to reject

- "Default RAM-proportional cache is fine; the OS will manage" — no, cap explicitly
- "AI agents can set `LD_LIBRARY_PATH` themselves" — no, self-contained binary
- "Operators can re-bring up NetBox after a reboot" — no, always-on
- "The UI works after a refresh" — no, SSE-driven liveness
- "Tests pass when the API returns the right thing" — no, screen-level verification too
- "Empty Incidents tab means no detections fired" — true, but the chaos matrix tells you that *programmatically*

---

## What v12 Explicitly Excludes

- New functional features
- Path A/B GNN, investigation agent, signals, controller adapters
- Any expansion of audience scope beyond controller-less + multi-controller correlation
- Bitemporal schema, schema migration, Grafeo eval
- Auth/RBAC, multi-tenancy, production HA, Kubernetes
- Workspace split

---

*Version 12.0 — authored 2026-05-03 after chunk-based code review of post-v11 main. Identifies the 9 GB memory bug as `Database::new(path, SystemConfig::default())` in src/graph/mod.rs:149 — Kuzu/lbug default buffer pool sized to ~80% of system RAM (F-1). Documents 11 other concrete code findings (F-2 through F-12) with location, evidence, and fix. Adds Tier 0 quick-fixes, Tier 1 memory architecture validation, Tier 2 binary self-containment via static lbug link, Tier 3 always-on infrastructure with API-driven test reset (not service teardown), Tier 4 UI liveness via SSE event broadcasting and screen-level test verification, Tier 5 startup time investigation. Carry-forward of v9 strategic threads (controller adapters, GNN, agent, signals) deferred until v12 polish completes. References v2-v11 for unchanged audience, architecture, and guardrails.*
