# BONSAI — Consolidated Backlog v2.0

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG.md` (v1.0). Produced after verifying the v4 repository state against v1's action items.
>
> **What changed from v1:** all Tier 0 bleeders fixed with regression tests; T1-1 event bus landed with counter debounce and retention; T3-2 chaos runner exists (minus a plan file). TSDB integration added as T4-8 per the architectural conversation on additive bus subscribers.

---

## Progress Since v1 — Verified Against Code

**Completed and removed from this backlog:**

| v1 item | Status | Evidence |
|---|---|---|
| T0-1 catalog path split | ✅ Done | `catalog.py:14` resolves `Path(__file__).parents[3] / "playbooks" / "library"`; SDK-internal library cleaned; load-count printed at startup; 4 regression tests in `test_catalog_and_executor.py` |
| T0-2 verification field mismatch | ✅ Done | `executor.py:74` reads `expected_graph_state` with `cypher` as legacy alias; `$if_name` substitution added; 4 regression tests |
| T0-3 temporal claim lie | ✅ Done | `CLAUDE.md:34` honestly states "append-only StateChangeEvent log (current); full bitemporal deferred to T4-3" |
| T0-4 empty_features typing | ✅ Done | `training.py:139-157` uses `typing.get_type_hints()` for typed defaults; test asserts no numeric field ever holds `""` |
| T0-5 vendor join | ✅ Done | `training.py:192-197` `OPTIONAL MATCH (d:Device)` returning `d.vendor`; test asserts Cypher contains the join |
| T1-1a EventBus trait | ✅ Done | `src/event_bus.rs` — trait + `InProcessBus`, swap-ready for NATS/Kafka |
| T1-1b graph writer as consumer | ✅ Done | `main.rs:69-114` subscribes from bus; old mpsc removed |
| T1-1d counter debounce | ✅ Done | `main.rs:74-98` per-(device,interface) last-write tracker, state transitions bypass, lagged-reader warning |
| T1-1e retention with count cap | ✅ Done | `retention.rs` both `prune_events` (age) and `prune_events_by_count` exist |
| T1-1f config surface | ✅ Done | `[event_bus]` and extended `[retention]` sections in `config.rs` |
| T3-2 chaos runner | ✅ Done (mostly) | `scripts/chaos_runner.py` — well-structured, SIGINT-safe, CSV-flush per cycle |

**Partially done — carried forward as -cont items:**

- **T0-6-cont**: shared `extract_features_for_event` exists and MLDetector delegates, but the rule detectors in `rules/bgp.py`, `rules/interface.py`, `rules/topology.py`, `rules/bfd.py` still have their own inline extraction — four separate implementations the shared path was meant to replace.
- **T3-2-cont**: chaos runner works but `chaos_plans/baseline_mix.yaml` does not exist. Nothing to run until someone writes at least one plan.

**Not yet started — remain in backlog:**

- T1-1c Parquet archive consumer (the only Tier-1 hole)
- T1-2 Distributed collector architecture
- T1-3 Dynamic device onboarding
- T2-1 (renumbered T2-4) Playbook validation script — `$if_name` substitution landed but catalog-wide `catalog.validate()` did not
- T2-2 ML feature schema versioning
- T2-3 Training data validity check
- T2-4 (original) BFD schema extension — superseded; BFD playbook now coexists without schema changes
- T3-1 Lab expansion
- T3-3 Training readiness check
- T3-4 Gradual degradation scenarios
- All T4 extensions

**Discipline observations worth capturing before moving on:**

1. **ADR debt**. DECISIONS.md has no entries newer than 2026-04-19 despite at least eight architectural decisions landing since. The event bus, the counter debounce interval, the retention count-cap behaviour, the canonical vs. legacy verification field, the shared extractor split, the `typing.get_type_hints`-based default scheme — all are decisions that should have ADR rows. This is now a concrete debt, not a soft observation. See T0-7.
2. **Test quality is good.** The tests for T0 fixes are behavioural, not cosmetic. They'd catch regressions. This is a cultural win worth noticing.
3. **Retention count-cap edge case.** `retention.rs:80` uses `occurred_at <= $cutoff` which could over-delete when multiple events share the exact same nanosecond timestamp. Low-impact but noted.

---

# The v2 Backlog — Ordered Priority

Five tiers, same structure as v1. Items retain their original codes where they carry forward so history is traceable.

---

# TIER 0 — Loose Ends and Discipline Debt

These are small, cheap, and directly undermine what's already built if left unaddressed.

## T0-6-cont — Migrate rule detectors to the shared extractor

**What**: `extract_features_for_event` in `ml_detector.py` is the canonical feature extractor. MLDetector delegates to it. The four rule files (`rules/bgp.py`, `rules/interface.py`, `rules/topology.py`, `rules/bfd.py`) still have their own inline implementations.

**Why**: the shared extractor's value comes from being *actually shared*. Today it's "shared between MLDetector and itself," which is not shared. Any future change to feature extraction has to be mirrored across five places or drift will return.

**Where**: `python/bonsai_sdk/rules/*.py` (four files)

**Done when**:
- Each rule detector's `extract_features` method calls `extract_features_for_event` as its first step
- Rule-specific gating (e.g., `BgpSessionDown._HARD_DOWN_STATES`) is applied *after* the shared call, returning `None` to skip
- An integration test instantiates one rule detector and MLDetector with the same event, asserts the returned Features dataclasses are equal (modulo the optional fields MLDetector populates unconditionally)

## T0-7 — Close the DECISIONS.md debt

**What**: write backdated ADR entries for the decisions that landed without one. At minimum:
- Event bus as `broadcast::Sender` with `InProcessBus` trait wrapper
- Counter debounce default of 10 seconds and the per-(device, interface) scope
- Retention count-cap semantics (`<=` cutoff, tie-breaking behaviour)
- Canonical verification field name, legacy alias kept for backward compat
- Shared feature extraction split between MLDetector (unconditional) and rules (gated)
- Typed-default scheme via `typing.get_type_hints()` for `_empty_features`

**Why**: the DECISIONS log is the only record of *why* something was built a specific way. Six weeks from now someone — including you — will ask "why is the default debounce 10s and not 5 or 30?" and there needs to be an answer.

**Where**: `DECISIONS.md`

**Done when**: six new dated ADR entries exist, each with "Decision / Alternatives considered / Rationale" structure matching existing entries.

## T0-8 — Retention count-cap tie-breaking

**What**: `retention.rs::prune_events_by_count` uses `occurred_at <=` which could over-delete when multiple events share the same timestamp.

**Why**: at lab scale with sub-millisecond telemetry, timestamp collisions are rare but real, especially on event bursts. Over-deletion corrupts training data unevenly.

**Where**: `src/retention.rs:78-83`

**Done when** (pick one):
- (Recommended) Switch to a two-stage delete: identify the exact IDs to delete using a `LIMIT $excess` query, then `DELETE WHERE id IN $ids`. Deterministic, no over-deletion.
- Alternative: use `<` instead of `<=` and delete exactly `excess` rows regardless of ties. Simpler but may under-delete.
- Add a unit test that seeds 10 events at the same timestamp and asserts count-cap behaviour matches the documented semantics.

---

# TIER 1 — The Remaining Architectural Seams

Three big items left. Order matters: archive first (unlocks training at scale), onboarding second (unlocks lab variety without restart pain), distributed collection third.

## T1-1c — Parquet archive consumer

**What**: a new `src/archive.rs` module. A tokio task that subscribes to the event bus, buffers incoming `TelemetryUpdate`s into Arrow record batches, flushes to Parquet on a configurable cadence.

**Why this is now the highest-priority Tier-1 item**: the event bus exists; the graph writer and retention cap it correctly; but the *cold storage* half of the hot/cold split is missing. Without Parquet archive, long-range ML training has no substrate. You'd be training Model A on the 50,000 StateChangeEvents that fit in the graph, which is not enough variety for anomaly detection on multi-week patterns.

**Where**:
- New file: `src/archive.rs`
- Modification: `main.rs` spawns the archive consumer alongside the graph writer
- Modification: `Cargo.toml` adds `arrow` and `parquet` crates
- Modification: `config.rs` extends `[event_bus]` or adds `[archive]` section

**Design**:

```rust
// Inside a new archive.rs
pub async fn run_archiver(
    bus: Arc<InProcessBus>,
    archive_path: PathBuf,
    flush_interval: Duration,
    max_batch_rows: usize,
) -> Result<()> {
    let mut rx = bus.subscribe();
    let mut buffer: Vec<TelemetryUpdate> = Vec::with_capacity(max_batch_rows);
    let mut flush_timer = tokio::time::interval(flush_interval);
    flush_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            recv = rx.recv() => match recv {
                Ok(update) => {
                    buffer.push(update);
                    if buffer.len() >= max_batch_rows {
                        flush(&mut buffer, &archive_path).await?;
                    }
                }
                Err(Lagged(n)) => warn!(dropped = n, "archiver lagged"),
                Err(Closed) => break,
            },
            _ = flush_timer.tick() => {
                if !buffer.is_empty() {
                    flush(&mut buffer, &archive_path).await?;
                }
            }
        }
    }
    Ok(())
}
```

**File layout**: one Parquet file per hour per device.
```
archive/
  2026/04/20/
    10.1.1.1_57400__hour-14.parquet
    10.1.1.2_57400__hour-14.parquet
    10.1.1.1_57400__hour-15.parquet
```

**Schema** (Arrow columns, aligning with `TelemetryUpdate`):
- `timestamp_ns: int64` (nanoseconds since epoch)
- `target: string`
- `vendor: string`
- `hostname: string`
- `path: string`
- `value: string` (JSON-serialised)
- `event_type: string` (classification output — InterfaceStats / BgpNeighborState / LldpNeighbor / InterfaceOperStatus / Ignored)

Columnar compression is zstd by default — Parquet handles it transparently, no config needed.

**Distributed-aware positioning**: per our architectural conversation, the archive is *collector-local* in the future distributed topology. Today it's core-local because collector == core. The file layout is identical in both modes. When T1-2 lands, each collector writes its own archive directory; no refactor needed. Document this in an ADR so the intent is preserved.

**Config**:
```toml
[archive]
enabled = true
path = "archive"
flush_interval_seconds = 10
max_batch_rows = 1000
```

**Done when**:
- A 30-minute run with the Phase 4 topology produces a populated `archive/YYYY/MM/DD/` tree
- `pd.read_parquet("archive/")` (with recursive discovery) in Python returns a DataFrame with the expected columns
- File sizes and compression ratios are logged at flush time so drift can be observed over time
- ADR entry explains collector-local positioning and the flush-interval default
- A `scripts/archive_stats.py` script reports total rows, bytes on disk, compression ratio, oldest/newest timestamp per device

## T1-3 — Dynamic device onboarding

**What**: replace `FileRegistry` (which returns a never-yielding channel) with a real `ApiRegistry` that mutates at runtime via gRPC RPCs, plus a `DiscoverDevice` RPC that connects to a candidate device and reports capabilities and recommended paths.

**Why before T1-2**: every time the lab needs a new device or a different vendor mix, the current workflow is "edit bonsai.toml, restart bonsai, lose the in-memory state." Chaos runs currently require fixed topologies. Training data variety suffers because adding a fifth device is friction. Onboarding is the unblock.

**Where**:
- New file: `src/discovery.rs` — the `DiscoverDevice` RPC handler
- Modification: `src/registry.rs` — `ApiRegistry` implementation
- Modification: `proto/bonsai_service.proto` — new RPCs
- Modification: `src/api.rs` — wire up the new RPCs
- Modification: `src/main.rs` — subscribe to registry change events and spawn/cancel subscribers accordingly
- Modification: `src/config.rs` — add `role` and `site` to `TargetConfig`
- New directory: `config/path_profiles/` — YAML templates per (vendor, role) combination

**Sub-tasks in order**:

**T1-3a — `ApiRegistry` with persistent state**. Backing store: a small SQLite database at `bonsai-registry.db` (or a JSON file if SQLite feels too heavy for v1). Survives restart. RPCs: `AddDevice`, `RemoveDevice`, `UpdateDevice`, `ListDevices`. Change events published to the existing `subscribe_changes` channel so `main.rs`'s consumer at lines 215-223 starts doing real work.

**T1-3b — Subscriber lifecycle from registry events**. Today `main.rs:119-142` spawns a subscriber per target at startup and that's it. Refactor: subscribers are spawned in response to `RegistryChange::Added` events; cancelled on `RegistryChange::Removed`. `FileRegistry` emits `Added` for each file-config target at startup, preserving today's behaviour. Use a `JoinSet<()>` keyed by device address so cancellation is trivial.

**T1-3c — `DiscoverDevice` RPC**. Takes address, credentials (env var names only, never plaintext), TLS settings. Steps: connect → Capabilities RPC → vendor detection → match against path profiles → return a structured report.

```proto
rpc DiscoverDevice(DiscoverRequest) returns (DiscoveryReport);

message DiscoverRequest {
  string address = 1;
  string username_env = 2;
  string password_env = 3;
  string ca_cert_path = 4;
  string tls_domain = 5;
  string role_hint = 6;  // "leaf" | "spine" | "pe" | "p" | "rr" | ""
}

message DiscoveryReport {
  string vendor_detected = 1;
  repeated string models_advertised = 2;
  string gnmi_encoding = 3;
  repeated PathProfileMatch recommended_profiles = 4;
  repeated string warnings = 5;
}

message PathProfileMatch {
  string profile_name = 1;
  repeated SubscriptionPath paths = 2;
  string rationale = 3;
  float confidence = 4;  // 0.0-1.0, based on how many expected models are advertised
}
```

**T1-3d — Path profile templates**. Start minimal — four profiles is enough:

```
config/path_profiles/
  dc_leaf_minimal.yaml    # interfaces (stats + oper-state), BGP, LLDP
  dc_spine_standard.yaml  # same as leaf
  sp_pe_full.yaml         # adds MPLS, ISIS, segment-routing
  sp_p_core.yaml          # ISIS, LDP, segment-routing (no BGP by default)
```

Each profile lists path specs that get filtered by the device's actual capability advertisement. Example: `sp_pe_full` lists `openconfig-mpls` — if the device doesn't advertise it, that path is dropped from the recommendation and a warning is emitted in the `DiscoveryReport`.

**T1-3e — Runtime path verification feedback loop**. After a device is added and subscribed, track which of the recommended paths produce updates within 30 seconds. Paths that return no data get flagged `subscribed_but_silent` in the graph — as a property on the Device node or a separate `SubscriptionStatus` node. This is the honesty layer: the system tells the operator what's actually working.

**Done when**:
- `bonsai device add <addr> --env-user X --env-pass Y --role leaf` (new CLI command against the gRPC API) discovers, selects a profile, subscribes, and produces telemetry — no restart
- `bonsai device remove <addr>` cancels the subscriber task and tombstones the Device node (does not hard-delete — preserves history)
- `bonsai device list` shows each device with its active subscription paths and `subscribed_but_silent` flags
- Integration test that exercises the full add → discover → subscribe → verify cycle against a ContainerLab SRL

## T1-2 — Distributed collector architecture

**Same scope and sub-tasks as v1 T1-2.** No changes — this item was not touched in v4. Deferred behind T1-1c and T1-3 because:

1. The event bus is the boundary the collector/core split lands on; that's now in place.
2. Dynamic onboarding works for local-mode first; once it works locally, extending it to remote-collector is mostly protobuf plumbing, not new architecture.
3. Parquet archive needs to be collector-local in the distributed future (per our conversation), so T1-1c should bake that in from the start and T1-2 can assume it.

Scope reminder:
- One binary, three modes (`collector`, `core`, `all`)
- `TelemetryIngest` gRPC service streaming `TelemetryUpdate` protobufs
- zstd compression on that stream
- Disk-backed local queue on collector for core-outage resilience
- mTLS between collector and core

---

# TIER 2 — ML Correctness and Catalog Integration

## T2-4 — Catalog-wide validation script

**What**: a `scripts/validate_playbooks.py` that loads every YAML in `playbooks/library/`, resolves preconditions against an empty `Features` object (checks for KeyError), resolves placeholders against `Features` fields (checks for unknown tokens), and parses Cypher verification queries for references to node labels that don't exist in the schema.

**Why**: the T0-2 fix accepted whatever was in the YAML. T0-6 test catches `$if_name` substitution specifically. But nothing today catches a playbook that references `features.neighbor_ip` (wrong field name) or `MATCH (n:OspfNeighbor)` (node label doesn't exist in the schema). That means a silently-broken playbook can sit in the library until the first time it's actually invoked.

**Where**: new file `scripts/validate_playbooks.py`

**Done when**:
- The script loads all 9 playbooks without errors
- Each playbook passes: precondition eval against empty Features returns bool (not KeyError); every `{placeholder}` token in steps and verification queries matches a Features field; every node label in verification Cypher is one of `Device|Interface|BgpNeighbor|LldpNeighbor|StateChangeEvent|DetectionEvent|Remediation`
- The BFD playbook (`bfd_session_down.yaml`) is either kept with `requires_schema_extension: BfdSession` and flagged, or its verification query rewritten to reference only existing node labels
- CI would run this — even if CI doesn't exist yet, the script exits non-zero on any failure so it can be wired in later

## T2-2 — ML feature schema versioning

**Same as v1.** Not touched.

Status check: today `features_to_vector()` is hardcoded to produce 6 floats in a fixed order. Training data stored today and a model loaded tomorrow after a schema change will silently disagree. The fix remains: bundle models as `{"model": ..., "feature_schema_version": N, "feature_names": [...]}`; assert match at load.

## T2-3 — Training data validity check

**Same as v1.** Not touched.

Status check: `train_anomaly.py` and `train_remediation.py` exist but have no pre-training validation. Row count, class balance, null rates, value ranges — none are checked. This blocks Model A from being trustworthy even after chaos runs accumulate data.

## T2-5 — NEW — Model C training blocked until verify() produces real labels

**What**: before the T0-2 fix, every `Remediation.status` in the graph read `success` because `verify()` short-circuited. Any training data accumulated before v4 is tainted. Model C cannot be trained on it.

**Why**: honesty. The whole point of the T0-2 fix was to make Remediation outcomes trustworthy. That means starting from clean data. The fix doesn't retroactively correct rows written before it.

**Where**: data hygiene action, not a code change

**Done when**:
- Any pre-v4 Remediation rows in the current `bonsai.db` are either deleted or marked `trustworthy=false` (add a column) so training scripts can filter them out
- A dated entry in DECISIONS.md captures the data-hygiene cutoff
- `train_remediation.py` is updated to filter by `attempted_at > <cutoff>`
- Accumulating fresh, post-fix Remediation data is explicitly part of the T3-3 readiness check

---

# TIER 3 — Lab and Fault Injection

## T3-2-cont — Author the chaos plan files

**What**: the runner exists. The plans do not.

**Where**: new directory `chaos_plans/`

**Done when**:
- `chaos_plans/baseline_mix.yaml` exists — roughly the example in v1 T3-2 (bgp down, interface shut, netem loss with weighted random selection)
- `chaos_plans/bgp_heavy.yaml` — weighted toward BGP events to accumulate Model C training data for BGP playbooks faster
- `chaos_plans/gradual_only.yaml` — exclusively `gradual_degradation` events (tied to T3-4)
- A short `chaos_plans/README.md` explaining which plan serves which training data goal
- The chaos runner has been executed end-to-end at least once against `baseline_mix.yaml` with `--duration-hours 1` and the resulting CSV verified

## T3-1 — Lab expansion

**Same as v1.** Not touched. Still needed for variety. Priority: after T1-3 (dynamic onboarding) lands so new topologies can be swapped in without fighting the config file.

## T3-3 — Training readiness check

**Same as v1.** Not touched. Script `scripts/check_training_readiness.py` that queries the graph and reports counts against the minimum bars for each model. Training scripts default to fail below the bar with a `--force` override.

**Additional requirement tied to T2-5**: the readiness check must filter Remediation rows by the data-hygiene cutoff so stale pre-fix data doesn't inflate the count.

## T3-4 — Gradual degradation scenarios

**Same as v1.** Not touched. New fault type in the chaos runner: `gradual_degradation` that ramps netem loss/delay over a window. Flagship demo: MLDetector fires 5 minutes before rules, on a slowly-degrading link.

---

# TIER 4 — Extensions and Polish

## T4-1 — Phase 6.1 onboarding UI

**Same as v1.** Builds on T1-3; when the gRPC onboarding API exists, the UI is a wizard over it. Three-step flow: address+creds → discovery report → path selection → confirm.

## T4-2 — Natural-language query layer

**Same as v1.** 200-line Python module using the existing `Query()` gRPC RPC. Two LLM calls per question (plan + render), one safe-mode guard to reject DELETE/DROP/MERGE in generated Cypher. Low code volume, high demo value.

## T4-3 — Full bitemporal schema

**Same as v1.** Defer until NL query in T4-2 starts asking questions today's schema can't answer ("what was the BGP state at 10am"). That's the forcing function.

## T4-4 — Metrics expansion

**Same as v1.** Histogram for graph-write latency (verify it's populated), event-bus depth gauge, archive lag gauge (oldest unarchived event age — this becomes valid once T1-1c lands), subscriber reconnect frequency per device, rule-firing rate per rule_id.

## T4-5 — Schema migration path

**Same as v1.** Defer until the pain is real.

## T4-6 — LLM-assisted playbook suggestion (Layer 3)

**Same as v1.** After the catalog has 15+ hand-written and lab-verified entries. Mandatory human approval gate. `SuggestRemediation` gRPC RPC.

## T4-7 — Grafeo migration readiness

**Same as v1.** Monitor LadybugDB release cadence. 60-day-no-release trigger for 3-day evaluation spike.

## T4-8 — NEW — TSDB integration adapter

**What**: per our architectural conversation, a new bus subscriber that translates `TelemetryUpdate` into Prometheus remote-write format (or InfluxLine) and pushes to an operator-configured TSDB endpoint. Telegraf replacement.

**Why it's additive, not required**: bonsai functions completely without it. The TSDB is for operators who already run Grafana dashboards and want to feed them from bonsai's already-open gNMI subscription instead of running a redundant Telegraf pipeline.

**Why it matters strategically**: this is what positions bonsai as a *Telegraf replacement plus*. Same subscription, same counter data, plus graph-derived label enrichment (role, site, upstream device, peer count) that Telegraf fundamentally cannot produce because Telegraf doesn't know the topology.

**Where**:
- New file: `src/tsdb_adapter.rs` — bus subscriber
- Modification: `config.rs` — `[tsdb]` section (default disabled)
- New dependency: `prometheus` or a remote-write HTTP client

**Design**:

```rust
// src/tsdb_adapter.rs
pub async fn run_tsdb_adapter(
    bus: Arc<InProcessBus>,
    graph: Arc<GraphStore>,
    endpoint: String,
    flush_interval: Duration,
) -> Result<()> {
    let mut rx = bus.subscribe();
    let mut buffer: Vec<TimeSeries> = Vec::new();
    let mut flush_timer = tokio::time::interval(flush_interval);

    loop {
        tokio::select! {
            recv = rx.recv() => match recv {
                Ok(update) => {
                    // Only interface stats get translated to metrics;
                    // state transitions stay in the graph.
                    if let TelemetryEvent::InterfaceStats { if_name } = update.classify() {
                        let labels = enrich_labels(&graph, &update).await;
                        buffer.extend(build_timeseries(&update, &if_name, labels));
                    }
                }
                Err(Lagged(n)) => warn!(dropped = n, "tsdb adapter lagged"),
                Err(Closed) => break,
            },
            _ = flush_timer.tick() => {
                if !buffer.is_empty() {
                    push_remote_write(&endpoint, &buffer).await?;
                    buffer.clear();
                }
            }
        }
    }
    Ok(())
}

async fn enrich_labels(graph: &GraphStore, update: &TelemetryUpdate) -> Labels {
    // Query the graph for Device.role, Device.site, and upstream connections.
    // This is the differentiator — Telegraf cannot do this.
    // Returns Prometheus-format labels: {device, role, site, upstream}
}
```

**Config**:
```toml
[tsdb]
enabled = false                           # default off
endpoint = "http://prometheus:9090/api/v1/write"
flush_interval_seconds = 15
enrichment_enabled = true                 # attach graph-derived labels
```

**Done when**:
- Bonsai can run with `tsdb.enabled = true` against a local Prometheus with remote-write enabled
- Grafana connected to that Prometheus shows interface counter time series
- Metrics carry graph-enriched labels (`role="leaf"`, `site="dc-lon"`) — verify by filtering in Grafana
- A short operator guide (`docs/tsdb_integration.md`) explains the config and the operational tradeoff (TSDB is optional, graph is the source of truth)
- ADR entry explaining why this is additive, not core

---

# Recommended Execution Order

Rebuilt for v2 state. Each line is one focused session or two; dependencies are honoured.

**Next sprint (unblock ML training):**
1. T0-6-cont — migrate rule detectors to shared extractor (1 session)
2. T0-7 — close ADR debt (1 session, cheap but overdue)
3. T2-4 — catalog validation script (1 session)
4. T1-1c — Parquet archive consumer (2 sessions)
5. T3-2-cont — author chaos_plans/baseline_mix.yaml and run for 2h (1 session, overlaps with above)

**Sprint after (start producing training data):**
6. T3-3 — training readiness check (1 session)
7. T2-5 — data hygiene cutoff for stale Remediation rows (part of same session)
8. T2-3 — training script data validity checks (1 session)
9. Run chaos continuously in the background for 2–3 weeks while the next items happen

**Sprint after that (widen the lab):**
10. T1-3 — dynamic onboarding via ApiRegistry + DiscoverDevice (3 sessions)
11. T3-1 — expanded lab topologies (2 sessions, builds on T1-3)
12. T3-4 — gradual degradation scenarios (1 session)

**Then the reward work:**
13. T4-2 — natural-language query layer (1 session — unblocks a flagship demo)
14. T2-2 — ML feature schema versioning (1 session)

**Longer horizon (in any order):**
15. T4-1 — onboarding UI
16. T4-8 — TSDB adapter
17. T1-2 — distributed collector

**Defer until forced by pain:**
- T4-3 bitemporal schema (forced by T4-2 NL queries about the past)
- T4-5 schema migration (forced by a breaking schema change)
- T4-6 LLM playbook suggestion (forced by catalog hitting 15+ entries)
- T4-7 Grafeo evaluation (forced by LadybugDB 60-day quiet period)

---

# Guardrails — Still Binding

Unchanged from v1. These are non-negotiable and should travel with every session:

- gNMI only. No SNMP, no NETCONF.
- tokio only. No async-std, no smol.
- Every non-trivial decision → ADR in DECISIONS.md *at commit time*, not later.
- No Kubernetes in v0.x. Collectors on separate hosts is fine; orchestration is not.
- No fifth vendor until the four vendor families work vendor-neutrally across the full remediation stack.
- Phase 6 UI: view-plus-onboarding, never arbitrary-config-to-devices.
- Credentials never leave Rust. Python holds env var *names*, not values.

---

# What This Backlog Continues to Exclude

Unchanged from v1. For scope discipline:

- No auth/RBAC work; the gRPC API is still trusting.
- No multi-tenant graph.
- No production-grade HA.
- No universal vendor library — Codex catalog grows organically.
- No replacement for Nautobot/NetBox.
- No streaming ML inference at horizontal scale.

---

*Version 2.0 — authored after reviewing v4 of the repository. All Tier 0 v1 items verified fixed; T1 event bus confirmed landed; T1-1c, T1-2, T1-3 outstanding; TSDB adapter added per the April 2026 architectural conversation on additive bus subscribers.*
