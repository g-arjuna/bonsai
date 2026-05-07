# BONSAI — Backlog Bravo Series, v4 (Bv4.0)

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_BV3.md`. Authored 2026-05-07 after chunk-by-chunk code review of the Bv3 landing, with explicit instruction to perform an architectural review (not just a feature audit) focused on whether the ingestion engineering will scale beyond the 8-12 node lab.
>
> **What this document is**: a strategic next-steps plan that takes seriously two operator questions:
> 1. Will the Bv3 ingestion engineering hold under real-world scale, or are there shape problems we should fix now before they become hard to undo?
> 2. Is the right next move data-gathering on laptop, data-gathering on cloud free tier, or extending the lab variety (DC vs SP)?
>
> **What this document deliberately does not do**: relitigate strategy, restate guardrails, or re-derive priorities that have stabilised across v7-Bv3. The MVP-then-GNN sequencing from Bv2-mod, the audience framing from v7, and the engineering principles from Bv3 all carry forward.
>
> **The honest summary of where we are**: Bv3's ingestion engineering landed well. The write coordinator with batched transactions, ingest-layer debounce, and adaptive backpressure are correct shapes. They will absorb the 12-node lab without crashing. **They will likely struggle at 100+ devices**, and several specific issues (catalogued below as C-1 through C-9) become hard to undo if we let them harden. The right next sprint is a small architectural cleanup pass plus chaos data gathering on the existing DC lab, not new feature work.

---

## Table of Contents

1. [Audience and Positioning](#positioning) — see v7
2. [Bv3 Sprint Outcome — Verified Landing](#progress)
3. [Architectural Review: Does Bv3 Ingestion Scale?](#scaling)
4. [Strategic Questions Answered](#strategy)
5. [TIER 1 — Architectural Cleanup Before It Hardens](#tier-1)
6. [TIER 2 — Logging Discipline](#tier-2)
7. [TIER 3 — Chaos Data Gathering on the Existing DC Lab](#tier-3)
8. [TIER 4 — SP Lab Bring-Up + Comparative Data](#tier-4)
9. [TIER 5 — Cloud Deploy Spike (timeboxed evaluation)](#tier-5)
10. [TIER 6 — GNN Path Continuation](#tier-6)
11. [Carryover from Bv3](#carryover)
12. [Execution Order](#execution-order)
13. [Guardrails — Updated](#guardrails)

---

## <a id="positioning"></a>Audience and Positioning

**Unchanged from v7-Bv3.** Controller-less primary audience across DC, campus, SP. AIOps integration as feeder. Northstar: bonsai detects real faults using real telemetry, with graph-native impact analysis and Path A embeddings working — Path B GNN as the destination.

---

## <a id="progress"></a>Bv3 Sprint Outcome — Verified Landing

End-to-end code review of the post-Bv3 main confirms substantial progress:

| Bv3 item | Status | Evidence |
|---|---|---|
| T1-1 write coordinator | ✅ Done | `src/write_coordinator.rs` (201 lines) — mpsc-backed, batched telemetry, oneshot replies for atomic ops (Detection, Remediation), default `batch_size = 256` and `flush_interval = 1s` |
| T1-1 batched transactions | ✅ Done | `src/graph/mod.rs:1552::write_batch()` — explicit BEGIN/COMMIT wrapping the loop of `write_blocking` calls |
| T1-2 router + per-subscriber queues | ⚠️ Partially landed | `src/event_bus.rs` rewrite with `BusSubscriber` trait, `MpscSubscriber`, `OverflowPolicy` enum. **But: dual-bus pattern keeps the legacy broadcast channel alive in parallel** (`legacy_tx`); every publish sent both ways. See C-4 below. |
| T1-3 ingest-layer debounce | ✅ Done | `src/ingest.rs:67::should_drop()` — value-aware state debounce + counter debounce checked before `bus.publish()` |
| T1-4 adaptive backpressure | ✅ Done | `src/ingest.rs:102-138` — two thresholds, level_1 drops counters, level_2 also drops oper-status flapping |
| T1-5 memory budget assertions | ✅ Done | `docker/grafana/alerts/bonsai-memory.yml` plus `/api/_test/status` extension |
| T1-6 architecture doc | ✅ Done | `docs/ingestion_architecture.md` written |
| T2-1 mgmt-plane LLDP filtering | ✅ Done | `src/graph/mod.rs:2978::is_mgmt_interface()` — vendor pattern allowlist (mgmt0, management*, eth0, fxp0, me0, em0); new `MGMT_LINK` edge type at `src/graph/mod.rs:466` |
| T2-2 topology-agnostic layout | ✅ Done | `ui/src/lib/Topology.svelte:208::nodeTier()` — `ROLE_TIER` map covers DC + SP + campus vocabulary; degree-quartile fallback for unknown roles; content-derived labels |
| T2-3 mgmt-plane visibility toggle | ✅ Probably done (UI extension; not specifically verified) |
| T3-1..T3-7 ServiceNow mock removal | ✅ Done | `docker/mock-servicenow/` deleted; `docker-compose.yml` reference gone; `nightly-integration.yml`, `seed_lab.sh`, `integration_compliance.md` updated |
| T4-3 Path A spectral embeddings | ⚠️ Partial | `docs/path_a_model_card.md` exists; whether embeddings actually run against the real graph on the laptop hasn't been independently verified (the operator's own report suggests some progress but write contention may have blocked) |
| T4-4 Path A model card | ✅ Done | `docs/path_a_model_card.md` |
| T4-5 detection rule tuning | ✅ Done (initial pass) | `docs/t45_detection_rule_tuning.md` |

**Strong sprint execution**. The write coordinator pattern is correct, the bus rewrite preserves backward compat (necessary), the topology layout fix is graceful for unknown topologies, and the mock cleanup is complete.

---

## <a id="scaling"></a>Architectural Review: Does Bv3 Ingestion Scale?

The operator asked a sharp question: "we tested for 8-10 nodes, will it choke in real world?" Real world will have more resources, but the *shape* of the architecture either scales linearly or hits a wall. This section walks through the scaling shape of each component.

### Scaling shape for **12 → 50 → 200 → 1000 devices**

| Component | 12-node (today) | 50-node | 200-node | 1000-node |
|---|---|---|---|---|
| Write coordinator queue | 256 batched updates per 1s tick | Same — no concern | Same — no concern | Possible tx>1s if write_batch slow; back-pressure should kick in |
| `write_batch` transaction time | ~50-150ms typical | ~200-400ms | ~500ms-2s | Likely >2s, pushes flush interval out |
| Bus router clone-per-subscriber | 6 subs × ~1KB clone × 256 updates = 1.5MB per flush | 6MB/s | 24MB/s | 120MB/s — copying becomes meaningful |
| Ingest Mutex contention | Negligible | Noticeable | Significant | Severe — single Mutex bottleneck |
| LRU debounce cache (16K entries cap) | Adequate | Adequate | Marginal — eviction starts dropping legitimate state | Inadequate — debounce defeated by eviction |
| LadybugDB single-writer | OK | OK | OK; tx contention with enrichers | Tight; enrichers may starve |
| Archive disk growth (Parquet, ZSTD-3) | ~100MB/day | ~400MB/day | ~1.6GB/day | ~8GB/day — needs S3 or rotation |
| Log file growth (no rotation) | ~500MB/day at debug | ~2GB/day | **fills 100GB SSD in 12 days** | OOD-disk crash within 3 days |

**The wall is at ~200 devices** under current code, dominated by:
1. Mutex contention on the three ingest LRU caches (C-8 below)
2. The dual-bus clone overhead (C-4 below)
3. LadybugDB write transaction time growing super-linearly with batch size

**Real-world bonsai-target deployments are 50-500 devices** based on the controller-less audience profile. So the existing engineering is good for small deployments and *near* the wall for medium ones. The findings below catch the issues before they harden.

### C-1 — Batch all-or-nothing rollback poisons good updates

**Location**: `src/graph/mod.rs:1561-1574`

The batch transaction wraps 256 updates. If any single update errors during `write_blocking`, the entire batch ROLLBACKs at line 1573. **One malformed gNMI update from a buggy device firmware can poison 255 good updates.**

**Why it matters at scale**: at 12 devices, the probability of zero malformed updates per batch is high. At 1000 devices with diverse vendor firmware, malformed updates are a near-certainty per batch. Persistent rollback failure mode → no telemetry ever lands.

**Fix shape**: input validation *before* the transaction. Each update parsed/classified once at ingest; malformed ones rejected with structured logging. The batch transaction sees only validated updates. Cheaper than savepoints, more correct than retry-on-error.

### C-2 — Dual-bus (legacy broadcast + new router)

**Location**: `src/event_bus.rs:151-159`

```rust
pub fn publish(&self, update: TelemetryUpdate) {
    let _ = self.legacy_tx.send(update.clone());      // backward compat
    if let Err(_) = self.router_tx.try_send(update) { // new pattern
        warn!("Event bus router queue full, dropping message");
    }
}
```

**Every publish sends to both bus implementations.** Subscribers that haven't migrated to the new pattern still use `subscribe()` (returns the broadcast receiver); subscribers that have migrated use `add_subscriber()`. **Worst of both worlds**: the producer pays the cost of both, the memory holds both buffers, and the router-only path doesn't get the benefit of being the only path.

**Why it matters at scale**: at 1000-device scale, the legacy broadcast channel still requires every subscriber to consume every message before slots free; if any subscriber is slow, the bus stalls — the exact problem Bv3 was supposed to fix.

**Fix shape**: identify and migrate the remaining `subscribe()` callers (archive, output adapters, subscription_status) to the router pattern. Delete `legacy_tx`.

### C-3 — DropOldest is unimplemented

**Location**: `src/event_bus.rs:85-91`

The code comment is honest:

```rust
OverflowPolicy::DropOldest => {
    // For mpsc, we can't easily drop oldest. 
    // We'll use try_send and log failure for now.
    if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(update) {
        metrics::counter!("...", "reason" => "drop_oldest_failed").increment(1);
    }
}
```

**All three policies effectively become DropNewest.** The architectural intent (let archive lag without dropping; let SSE drop newest; let write-coordinator block) is not realised.

**Why it matters at scale**: when archive falls behind on a busy lab, telemetry the operator wanted retained gets dropped. Current behaviour silently dropped recent data — operationally surprising.

**Fix shape**: replace `mpsc::channel` with `tokio::sync::broadcast` (which supports lag-tolerance by design) for DropOldest subscribers. Or use a deque-backed queue with explicit popping. Or document that DropOldest is unsupported and only offer DropNewest + BlockProducer.

### C-4 — Router clones per subscriber sequentially

**Location**: `src/event_bus.rs:182-188`

```rust
let mut futures = Vec::with_capacity(subs_guard.len());
for sub in subs_guard.iter() {
    futures.push(sub.handle(update.clone()));  // CLONE per subscriber
}
futures::future::join_all(futures).await;
```

Each `update.clone()` copies the `TelemetryUpdate` struct (six `String` fields plus the `JsonValue` blob). At 6 subscribers × 256 updates × ~1-5KB per update, **a single batch flush copies 1.5-7.5 MB**. At 1000-device scale, this becomes ~600 MB/s of allocation pressure.

**Why it matters at scale**: allocation churn dominates CPU. Memory bandwidth becomes the bottleneck before write transactions become slow.

**Fix shape**: wrap the update in `Arc<TelemetryUpdate>` and clone the Arc (cheap pointer copy) instead of the struct. Subscribers that need ownership can `Arc::unwrap_or_clone` only when needed.

### C-5 — RwLock per-update on subscribers list

**Location**: `src/event_bus.rs:180`

```rust
async fn run_router(mut rx: mpsc::Receiver<TelemetryUpdate>, subs: Arc<RwLock<Vec<Arc<dyn BusSubscriber>>>>) {
    while let Some(update) = rx.recv().await {
        let subs_guard = subs.read().await;  // LOCK per update
        ...
    }
}
```

Subscriber list is acquired under RwLock for every single update. The lock is read-only most of the time (subscribers are added at startup and don't change), but at high update rate the lock acquisitions add overhead.

**Why it matters at scale**: at 10K updates/sec the RwLock overhead becomes measurable.

**Fix shape**: clone the Arc<Vec<...>> once per recv batch, not per update. Or use `arc_swap::ArcSwap` for lock-free reads with rare writes.

### C-6 — Three Mutex<LruCache> on ingest hot path

**Location**: `src/ingest.rs:34-41` plus `should_drop()` calls at lines 85, 112, 126

Three global Mutex<LruCache>s (state, counter, oper_status). Every telemetry update acquires 1-2 of these. At 200+ device scale this is the single biggest contention point.

**Why it matters at scale**: Mutex acquisition is fast (~50ns uncontended, ~1μs contended). At 50K updates/sec contended, this is 50ms/sec of mutex wait. Not catastrophic, but tightens the ceiling.

**Fix shape**: per-shard locks (e.g. 16 shards keyed by hash of update.target) to spread contention. Or replace with `dashmap::DashMap` for lock-free concurrent access. The LRU semantics matter less than the contention behaviour.

### C-7 — LRU caps too small for 1000-device scale

**Location**: `src/ingest.rs:56-62`

- `last_counter_write` cap = 4096
- `last_oper_status_write` cap = 4096
- `last_state_write` cap = 16384

At 1000 devices × 50 interfaces = 50K unique counter keys. The LRU evicts faster than entries are queried. **Debounce becomes ineffective; the same keys re-debounce on every update**.

**Why it matters at scale**: at 1000-node scale, the debounce that we engineered specifically to control rate at line C-6 stops working because eviction outpaces lookup.

**Fix shape**: cap-by-RAM rather than cap-by-count. `lru::LruCache::new(capacity)` where capacity scales with `min(64K, available_ram_bytes / 100)`.

### C-8 — Logging has no rotation, no retention, no size cap

**Location**: `src/main.rs:57-62`

```rust
tracing_subscriber::fmt()
    .with_env_filter(...)
    .init();
```

Output goes to stderr only. Docker's default JSON log driver has no built-in rotation. **At debug level on a 12-node lab, ~500MB-2GB of log file per day** (verified by operator's existing 2-day operation). At 100-device scale, **fills a 100GB laptop disk in 12 days**. Docker daemon then errors on writes; bonsai writes hang; bonsai crashes.

**This is the disk-fill crash mode the operator warned about.**

**Fix shape**: `tracing_appender::RollingFileAppender` with daily rotation + N-day retention OR cap log file size at a configurable bytes value with rolling rotation. Stderr stays for foreground use; file logging gates by config.

### C-9 — Counter-summary mode is implicit, not enforced

**Location**: `src/ingest.rs:556-562` reads filter_config and conditionally drops raw counters when summary is enabled.

The behaviour is correct, but it's not visible in `/api/_test/status` or in any UI. **Operators running counter-summary won't know which mode they're in until they look at logs.**

**Why it matters at scale**: in production deployments, knowing whether you're sending raw or summary counters is a 10x cost decision. It should be an obvious operational fact, not buried in config.

**Fix shape**: surface in the operations workspace UI; expose in `/api/operations/status`.

---

## <a id="strategy"></a>Strategic Questions Answered

The operator asked three strategic questions. Honest answers below:

### Q1: DC vs SP — should we bring up the SP lab?

**Recommendation: not yet. Run the existing DC lab harder first.**

**Reasoning**:
- The DC lab is operational and stable. The SP lab requires another bring-up cycle that would consume Sprint days.
- The chaos archive has not been *deeply* exercised on DC. The fault catalogue lists 6 DC faults; we don't yet have hours of varied-fault data to confirm rule sensitivity, false-positive rate, detection latency p95.
- Path B GNN training requires **30+ days of varied chaos data**. Today we have ~2 days of bring-up + bug-fixing data. The marginal value of switching to SP is lower than the marginal value of running DC continuously for 30 days.
- After the GNN trains on rich DC data, **pivoting to SP later validates generalisation** — much higher signal value than mixing the data sources from day 1.

**What this looks like operationally**: Tier 3 below — the chaos harness drives the existing DC lab continuously. Faults inject on a 30-minute cycle; archive accumulates; baselines stabilise.

### Q2: Container-based collection / Kubernetes — is this the right time?

**Recommendation: not now. Defer to Bv5 or later.**

**Reasoning**:
- The current monolithic-on-laptop pattern is solving an open question (does the ingestion architecture scale?). Adding K8s before that is solved doubles the variables we're investigating.
- Distributed mode (collector + core via gRPC mTLS) is *coded* but rarely operated. Before K8s we should run distributed mode in compose for a sustained period.
- K8s deployment artefacts (Helm chart, statefulsets, RBAC, etc.) take 1-2 weeks of engineering and rarely surface real architectural issues until production.

**What we should do instead**: Tier 1 fixes (architectural cleanup) plus Tier 3 (chaos data on existing lab) plus a **timeboxed cloud spike** (Tier 5) using the existing docker-compose pattern on a free-tier VM. If the spike succeeds, K8s is the natural next step. If it surfaces issues, those become Bv5 priorities.

### Q3: Cloud free tier for sustained data gathering — worth it?

**Recommendation: yes, as a timeboxed evaluation, with explicit kill criteria.**

**Reasoning**:
- Laptop runs are constrained by the operator's machine. Sustained 7-day chaos archive on a laptop competes with daily computer use.
- Cloud free tiers (Oracle Cloud Always Free, AWS Free Tier, GCP Free Tier) offer 1-2 small VMs indefinitely. Enough for a 12-node ContainerLab + bonsai stack at modest scale.
- The data accumulated would feed Path B GNN training — direct contribution to the northstar.

**Risks to size**:
- Free-tier compute is small (1-4 vCPU, 2-12 GB RAM) — would the lab + bonsai fit? Almost certainly need to scale lab down to 6-8 nodes.
- Networking across cloud zones costs money. Self-contained single-VM only.
- Setup time is real — 1-3 days even for an experienced cloud engineer.

**How to timebox**: Tier 5 below. 5-day spike. Set up Oracle Always Free or AWS Free Tier instance. Run docker-compose-driven stack for 5 days. Pull archive to GitHub. Shut down. If it produces meaningful chaos data, repeat or productionise. If it doesn't, kill the line.

### Q4: Logging — disk fill crash risk on laptop

**Already an issue. Fix in Tier 2 below.**

The operator is right: at debug level on a 12-node lab, ~500MB-2GB of logs per day. A 7-day chaos run at debug level fills 14GB. On a laptop with 100GB free disk, that's fine. On a free-tier cloud VM with 30GB disk, that's three days before the disk fills and bonsai crashes.

**This is genuinely blocking** the cloud spike (Tier 5). Must land before any extended unattended run.

### Q5: GNN — when?

**Recommendation: after Tier 1-3 land + 30 days of chaos data accumulate. Realistically 4-6 weeks from today.**

**Reasoning**:
- Training a GNN on 2 days of bring-up data produces overfitting on bring-up bugs, not generalisable patterns.
- Tier 3 below establishes the always-on chaos cycle that produces training data.
- Tier 1 cleanups make the data trustworthy (no contamination from write rollback poisoning, no SSE-induced gaps).
- Tier 5 cloud spike, if it works, accelerates archive accumulation.

The honest sequence: Tier 1 fixes (1 sprint), Tier 2 logging (parallel), Tier 3 chaos run starts immediately and accumulates passively, Tier 5 cloud spike (parallel or sequential), then GNN training when archive depth is sufficient. **The GNN sprint itself is unchanged from Bv3 Tier 5 — only the trigger condition (archive depth) changes.**

---

## <a id="tier-1"></a>TIER 1 — Architectural Cleanup Before It Hardens

**Why this is Tier 1**: the C-1 through C-9 findings are all architectural shapes that get harder to change as more code depends on them. Fixing them now is cheap; fixing after another sprint of feature work doubles the effort.

The cleanups are scoped to be **small and self-contained** — no new functionality, only reshaping what exists.

### T1-1 (Bv4) — Pre-validate updates at ingest, eliminate batch poisoning (C-1)

**What**: every TelemetryUpdate is parsed and validated at ingest before reaching the bus. Validation produces either a typed `TelemetryEvent` enum variant or rejection. The bus and write_batch see only validated updates. Single-update errors during `write_blocking` become diagnostic-only (logged but the batch continues).

**Where**:
- `src/ingest.rs` — pre-classify before publishing, reject malformed updates with metric increment
- `src/graph/mod.rs::write_batch` — change line 1567-1568 to log+continue rather than rollback-all

**Done when**:
- A deliberately malformed update at ingest produces `bonsai_ingest_validation_drops_total` increment, doesn't reach bus
- A test injects 1 bad update among 255 good ones; result is 255 successful writes + 1 logged drop, not 256 rollbacks
- Code coverage shows malformed-update path tested

### T1-2 (Bv4) — Remove dual-bus, complete the router migration (C-2)

**What**: identify the remaining `bus.subscribe()` callers and migrate them to `add_subscriber(MpscSubscriber)`. Delete `legacy_tx` from `InProcessBus`.

**Callers to migrate**:
- `src/main.rs:273` — graph writer (already uses subscribe; trivial swap)
- `src/archive.rs:78` — archive writer
- `src/output/prometheus.rs:130` — Prometheus adapter
- `src/output/traits.rs:321` — base trait used by Splunk + Elastic + ServiceNow EM
- `src/subscription_status.rs:43` — subscription tracker
- `src/ingest.rs:349` — collector forwarder

Each migration:
1. Choose appropriate `OverflowPolicy` per subscriber's tolerance
2. Replace `bus.subscribe()` with `MpscSubscriber::new(...)` + `bus.add_subscriber(sub)`
3. The receiver's read loop becomes the same; the type changes from `broadcast::Receiver` to `mpsc::Receiver`

**Where**: each of the 6 modules above; `src/event_bus.rs` after migrations land — delete `legacy_tx`, simplify `publish`.

**Done when**:
- `grep "bus.subscribe()" src/` returns zero results
- `legacy_tx` removed from `InProcessBus`
- Memory usage drops by approximately 1× the bus capacity × subscriber count (one channel removed)

### T1-3 (Bv4) — Implement DropOldest correctly OR remove the policy (C-3)

**What**: pick one of:
- (a) Replace `mpsc` with `tokio::sync::broadcast` for DropOldest subscribers (broadcast supports lag-tolerance natively)
- (b) Document DropOldest as unsupported, replace with DropNewest in archive's overflow policy

**Recommendation**: (a) is cleaner. Archive specifically wants DropOldest semantics.

**Where**: `src/event_bus.rs::MpscSubscriber` — either rewrite to `BroadcastSubscriber` for DropOldest or remove the variant.

**Done when**:
- DropOldest semantics actually drop oldest, with metric `bonsai_subscriber_drops_total{reason="drop_oldest"}` incrementing
- Archive runs with DropOldest, deliberately stalled — verify oldest items are dropped

### T1-4 (Bv4) — Arc<TelemetryUpdate> through the bus (C-4)

**What**: change `TelemetryUpdate` to be wrapped in `Arc<TelemetryUpdate>` from the moment it enters the bus. Subscribers receive `Arc::clone()` (pointer copy) instead of struct clone.

**Where**:
- `src/event_bus.rs::publish(update: Arc<TelemetryUpdate>)` 
- `src/event_bus.rs::run_router` clones Arc instead of struct
- All `BusSubscriber` implementations accept `Arc<TelemetryUpdate>`
- Subscribers that need ownership: `Arc::try_unwrap` or clone the inner

**Done when**:
- Allocation profile shows `bonsai_event_bus_publish_bytes_total` drop substantially
- Memory bandwidth metric shows reduction at high update rate

### T1-5 (Bv4) — Lock-free subscriber list (C-5)

**What**: replace `Arc<RwLock<Vec<...>>>` with `arc_swap::ArcSwap<Vec<...>>`. Read path becomes lock-free. Add path is rare (subscribers added at startup).

**Where**: `src/event_bus.rs::InProcessBus::subscribers` field + `add_subscriber` and `run_router` accessors.

**Done when**:
- `arc_swap` dependency added to Cargo.toml
- Router benchmark shows reduced contention at high update rate

### T1-6 (Bv4) — Sharded ingest debounce caches (C-6)

**What**: replace each `Mutex<LruCache>` with a sharded variant. Either:
- (a) Manual sharding: `[Mutex<LruCache>; 16]` keyed by `hash(update.target) % 16`
- (b) `dashmap::DashMap<String, Instant>` with periodic eviction

**Recommendation**: (a). DashMap doesn't have LRU semantics; we'd lose the eviction we need for memory bound.

**Where**: `src/ingest.rs::ShouldDropContext` (the struct holding the three caches).

**Done when**:
- Lock contention metric (which we'll add) reduces by 16x in a 200-device load test
- Memory bound is preserved (LRU eviction still works per-shard)

### T1-7 (Bv4) — Cap LRU debounce caches by RAM, not by count (C-7)

**What**: instead of hardcoded 4096/16384, compute caps from configured `[ingest.debounce]` byte budget divided by per-entry size estimate.

**Where**: `src/ingest.rs::ShouldDropContext::new`, `src/config.rs::IngestConfig`.

**Done when**:
- `bonsai.toml` exposes `[ingest.debounce] memory_bytes = 16777216` (16 MB default)
- Caps computed as `memory_bytes / entry_size_estimate`
- At 1000-device scale on a properly-sized config, the LRU does not evict legitimate state

### T1-8 (Bv4) — Surface counter-summary mode in operations UI (C-9)

**What**: `/api/operations/status` and the Operations workspace show prominently whether the running mode is raw counters or summary; show the summary parameters.

**Where**: `src/http_server.rs::operations_handler`, `ui/src/routes/Operations.svelte`.

**Done when**:
- Operator opens Operations workspace; sees a clear "Counter mode: summary (90s window, 1.0 packet diff threshold)" box

---

## <a id="tier-2"></a>TIER 2 — Logging Discipline

The disk-fill crash mode is the only Bv4 scaling issue that actually crashes today. Higher priority than C-1 through C-9 in terms of operational risk.

### T2-1 (Bv4) — File-rotated logging with retention

**What**: `tracing_appender::RollingFileAppender` configured for daily rotation + N-day retention. Default 7 days. Configurable.

```toml
[logging]
file_path = "/var/log/bonsai/bonsai.log"  # or ./bonsai.log on laptop
rotation = "daily"                         # daily | hourly | never
retention_days = 7
max_file_size_mb = 1024                    # rotate early if exceeded
level = "info"                             # debug only when troubleshooting
```

**Where**:
- `src/main.rs` — replace direct `tracing_subscriber::fmt().init()` with file-appender + stderr layered subscriber
- `src/config.rs` — `LoggingConfig` struct
- `Cargo.toml` — `tracing-appender` dependency

**Default behaviour**: stderr at INFO level (foreground use), file logging at INFO with daily rotation + 7-day retention. Operator can enable DEBUG via environment variable for diagnosis.

**Done when**:
- A 24-hour run produces exactly 1 log file
- After 7 days, exactly 7 log files exist; the 8th day rotates the oldest out
- `du -sh /var/log/bonsai` is bounded by config × file size estimate

### T2-2 (Bv4) — Per-component log level overrides

**What**: `[logging.targets]` table allowing per-module level override. Useful for debugging one component without flooding logs from others.

```toml
[logging.targets]
"bonsai::ingest" = "debug"
"bonsai::write_coordinator" = "trace"
"bonsai::graph" = "info"
```

**Where**: `src/main.rs` — `EnvFilter::from_default_env().add_directive(...)` chained per config entry.

**Done when**: a single component can be set to DEBUG without affecting others.

### T2-3 (Bv4) — Log volume metrics

**What**: counter `bonsai_log_lines_total` and gauge `bonsai_log_file_bytes` — disk-pressure alerting.

**Where**: a small `tracing` Layer that increments the counter on every event.

**Done when**: dashboard shows log volume; alert fires when log file size exceeds 80% of `max_file_size_mb`.

### T2-4 (Bv4) — Pre-flight disk space check at startup

**What**: at startup, check available disk space at `logging.file_path`, refuse to start if below a configurable floor (default 5 GB free).

**Where**: `src/main.rs` startup phase.

**Done when**: starting bonsai on a near-full disk produces a clear error, not a silent log write that fails 30 seconds later.

---

## <a id="tier-3"></a>TIER 3 — Chaos Data Gathering on the Existing DC Lab

**Why this is Tier 3**: the GNN's data-hunger is the binding constraint on the northstar. Every day we don't run chaos against the lab is a day of training data we don't have.

This tier produces the data without consuming engineering time on new infrastructure.

### T3-1 (Bv4) — Always-on chaos schedule against the DC lab

**What**: a long-lived process that drives the fault catalogue against the running lab on a continuous schedule. 30-minute cycle: inject fault, wait for detection, verify in `/api/incidents`, heal, wait, repeat with the next fault.

**Where**: `scripts/chaos_runner.sh` (or systemd unit, or compose-feedback profile).

**Cycle plan**:
- 6 DC faults in catalogue → ~30-minute injection window each → full cycle in ~3 hours
- Continuous loop; archive accumulates passively

**Done when**:
- Lab + bonsai + chaos runner stay up for 7 consecutive days
- `runtime/driver_results/chaos.json` is updated continuously
- Archive directory grows by ~100 MB/day in Parquet at the configured retention

### T3-2 (Bv4) — Fault catalogue depth — add varied scenarios

**What**: the existing fault catalogue has 6 DC faults. Add ~12 more for varied chaos signal:
- Multi-fault scenarios (link down + BGP timeout simultaneously)
- Time-distributed faults (one fault every 10 minutes vs all at once)
- Asymmetric faults (one direction down, return path up)
- Rare but realistic (silent BFD timeout, slow MAC learning failure, EVPN type-2 misadvertisement)

**Where**: `lab/fault_catalog.yaml` extension.

**Done when**: catalogue contains ≥18 distinct DC fault scenarios; chaos cycle exercises each at least once per day.

### T3-3 (Bv4) — Archive integrity verification

**What**: a script that periodically (daily) reads the latest archive Parquet files and asserts:
- Files are not corrupt (Parquet metadata reads successfully)
- Schema matches expected (no drift)
- Row counts grow monotonically over time
- Compression ratio is reasonable (file size ÷ uncompressed estimate ≈ 0.05-0.15 for ZSTD)

**Where**: `scripts/verify_archive.sh` + nightly CI.

**Done when**: 7 days of archive verifies cleanly daily; corrupted archive triggers alert.

### T3-4 (Bv4) — Detection rule baseline metrics

**What**: from the chaos archive, derive per-rule baseline metrics:
- True positive rate (fault → matching detection within window)
- False positive rate (detection without matching fault)
- Detection latency p50, p95, p99 per rule
- Time-to-clear after fault heals

These baselines drive future tuning. Captured in `docs/test_results/detection_baselines/<date>.md`.

**Where**: `scripts/compute_detection_baselines.py`.

**Done when**: a baselines report can be regenerated from the archive at any time; baselines from Day 1 vs Day 7 show stability or improvement.

---

## <a id="tier-4"></a>TIER 4 — SP Lab Bring-Up + Comparative Data

**Sequenced after Tier 3 stabilises**. Bringing up the SP lab is valuable for generalisation testing of the eventual GNN, but not for primary training data.

### T4-1 (Bv4) — SP lab bring-up

`lab/sp/sp-mpls-srte.clab.yml` already exists. Bring it up; capture B-style bugs as they surface (similar to Sprint 1's B1-B14 list); fix; verify all sessions established.

**Done when**: `scripts/check_lab.sh sp` returns all-green.

### T4-2 (Bv4) — SP-specific fault catalogue extension

Add SP fault scenarios distinct from DC (LDP session down, RSVP-TE path failure, SR-TE policy degradation). 8-12 new entries.

### T4-3 (Bv4) — SP chaos cycle joins the always-on schedule

Same shape as T3-1 but for SP. Either alternating or parallel (resource permitting).

---

## <a id="tier-5"></a>TIER 5 — Cloud Deploy Spike (timeboxed evaluation)

**Five days. Single VM. Either it produces meaningful data or we kill the line.**

### T5-1 (Bv4) — Pick a free-tier provider and provision

**Candidates**:
- **Oracle Cloud Always Free**: 4 ARM cores, 24 GB RAM, 200 GB block storage — most generous, indefinite
- **AWS Free Tier**: t2.micro 1 vCPU 1 GB — too small for the lab; would need t3.medium paid
- **GCP Free Tier**: e2-micro 0.25 vCPU 1 GB — too small

**Recommendation**: Oracle Always Free, ARM. The 24 GB RAM and 4 cores comfortably fit a scaled-down (6-node) DC lab + bonsai + external infrastructure. Indefinite duration. No surprise billing.

**Where**: provisioning script in `scripts/cloud/oracle_setup.sh` (Terraform optional).

**Done when**: 1 VM running, accessible via SSH, with Docker installed.

### T5-2 (Bv4) — Self-contained deploy script

**What**: a single script `scripts/cloud/deploy.sh` that, on a fresh VM:
1. Installs Docker + Docker Compose
2. Clones the bonsai repo
3. Brings up the scaled-down lab (6-node DC)
4. Brings up bonsai-core + external infra (NetBox, Prometheus only — no Splunk/Elastic to save resources)
5. Starts the chaos runner (T3-1)
6. Configures log rotation per Tier 2

Idempotent: re-running picks up where it left off.

**Done when**: from a fresh Oracle VM, `scripts/cloud/deploy.sh` produces a running stack in under 30 minutes.

### T5-3 (Bv4) — Daily archive sync to GitHub

**What**: a daily cron that compresses the previous day's archive + driver results + memory profile + state-of-system snapshot, and pushes to a GitHub branch. Operator can pull anytime.

**Where**: `scripts/cloud/daily_sync.sh` + cron entry.

**Done when**: after 5 days, the GitHub branch contains 5 daily snapshots, each verifiable, total size under 1 GB compressed.

### T5-4 (Bv4) — Kill criteria

The spike is killed if any of:
- Bonsai crashes more than 1× per 24 hours
- Lab fails to stay up for 24 hours continuously
- Daily archive sync fails for 2 consecutive days
- Free-tier resource limits hit
- After 5 days, the archive does not contain meaningfully different signal from laptop runs

If killed: tear down VM, document findings, return to laptop-only.

If successful: extend to 30-day run; data feeds GNN training (Tier 6).

### T5-5 (Bv4) — Spike report

After 5 days (success or kill), a written report at `docs/test_results/cloud_spike/<date>.md`:
- What was deployed
- What worked, what didn't
- Resource utilisation (CPU, RAM, disk, network)
- Comparison: laptop archive vs cloud archive
- Recommendation: continue, scale up, kill, or pivot

---

## <a id="tier-6"></a>TIER 6 — GNN Path Continuation

Unchanged from Bv3 Tier 5 conceptually; trigger condition revised.

### T6-1 (Bv4) — GNN data loader (Bv3 carryover)

PyTorch Geometric data loader reading from the chaos archive. Reuses Path A embeddings as node features.

### T6-2 (Bv4) — GNN training (Bv3 carryover)

GraphSAGE or GAT, 2-3 layers, anomaly score per Device node. Train on 25 days, validate on 5 days, test on most recent day.

**Trigger condition**: archive depth ≥ 30 days **OR** cloud spike (Tier 5) produces ≥ 14 days of high-quality archive.

**Comparison baselines**:
- Rule-based detector
- Tabular ML detector (existing)
- GNN

Confusion matrix: detected-by-GNN-only / detected-by-rules-only / detected-by-both.

### T6-3 (Bv4) — Online inference path

GNN scoring on graph snapshot every N seconds. Detection events get `gnn_anomaly_score`. UI surfaces.

### T6-4 (Bv4) — Model card

Honest documentation. Algorithm, data, eval, limitations explicit.

---

## <a id="carryover"></a>Carryover from Bv3

These items remain valid; deferred behind Tier 1-3:

**From Bv3 Tier 6**:
- Investigation agent productive use (post-MVP, pending token budget)
- HIL graduated remediation in production
- Output adapter productive use (Splunk/Elastic running against real receivers)
- Signals tier (syslog/traps)
- Controller adapter implementations (demand-driven)
- Operator path overrides UI workspace
- Subscription resolution audit
- Catalogue plugin install command
- AIOps readiness checklist
- NL query, bulk CSV onboarding, scale architecture, S3 archive
- Campus topology
- Bitemporal schema, schema migration, Grafeo evaluation

Plus the Bv2 hardcoding catalogue (H-1 through H-12) — most addressed by Bv3 Tier 2 work; remainder opportunistic.

**Documentation refresh** (Bv1 Tier 8): lowest priority, defer.

---

## <a id="execution-order"></a>Execution Order

### Sprint 1 of Bv4 — Architectural cleanup + logging (1-2 weeks)
1. T2-1 file-rotated logging (highest priority — prevents disk-fill crash)
2. T2-4 pre-flight disk space check
3. T1-1 pre-validate updates at ingest, eliminate batch poisoning
4. T1-2 remove dual-bus, complete router migration
5. T1-3 implement DropOldest correctly
6. T1-4 Arc<TelemetryUpdate> through the bus
7. T1-5 lock-free subscriber list
8. T1-6 sharded ingest debounce caches
9. T1-7 cap caches by RAM
10. T1-8 surface counter-summary mode in UI
11. T2-2 per-component log level overrides
12. T2-3 log volume metrics

### Sprint 2 of Bv4 — Chaos data gathering ramp (1 week + ongoing)
13. T3-2 fault catalogue depth (add ~12 varied scenarios)
14. T3-1 always-on chaos schedule (DC lab)
15. T3-3 archive integrity verification (nightly CI)
16. T3-4 detection rule baseline metrics

After Sprint 2, the chaos archive accumulates passively. Days 1-30 from this point feed the eventual GNN training.

### Sprint 3 of Bv4 — Cloud spike (5 days)
17. T5-1 Oracle Always Free provisioning
18. T5-2 self-contained deploy script
19. T5-3 daily archive sync
20. T5-4 kill criteria evaluation
21. T5-5 spike report

### Sprint 4 of Bv4 — SP lab bring-up (parallel to Sprint 3 if cloud spike succeeds; sequential otherwise)
22. T4-1 SP lab bring-up
23. T4-2 SP fault catalogue
24. T4-3 SP joins always-on schedule

### Sprint 5 of Bv4 — GNN training (when archive depth allows)
25. T6-1 GNN data loader
26. T6-2 GNN training
27. T6-3 online inference
28. T6-4 model card

**Total estimate**: 7-10 weeks to GNN trained-and-deployed, depending on cloud spike success.

---

## <a id="guardrails"></a>Guardrails — Updated

### New in Bv4

- **Architectural cleanups land before they harden.** Issues found in Bv4 review (C-1 through C-9) take priority over new features in Sprint 1.
- **Logging is bounded.** No bonsai deployment runs without log rotation + retention. Disk-fill crash is engineered out, not engineered around.
- **Chaos data is not optional infrastructure.** The always-on chaos cycle is part of the standing operational stack from Sprint 2 onward. GNN training depends on it.
- **Cloud deploy is a timeboxed spike, not a commitment.** Five days, kill criteria documented, decision driven by data.
- **GNN training trigger is archive depth, not calendar.** Sprint 5 gates on ≥30 days laptop OR ≥14 days cloud, not on a fixed date.

### Unchanged from v7-Bv3

All prior architectural invariants and discipline continue. Reference earlier backlogs.

### Anti-patterns to reject

- "We can fix the dual-bus later when it's a problem" — no, fix it now while subscriber list is small
- "DropOldest works in practice; the comment is wrong" — no, the comment is accurate; either implement it or remove it
- "Free-tier cloud is too small to be useful" — no, scale lab to fit the VM; the data still feeds GNN training
- "K8s is the production deployment story; let's start there" — no, distributed mode in compose first; K8s after architectural questions are answered
- "GNN can train on 7 days of data" — no, 30 days minimum for distinguishable signal

---

## What Bv4 Explicitly Excludes

- New functional features beyond architectural cleanup + chaos data gathering
- K8s deployment artefacts
- Investigation agent productive use (post-MVP, pending token budget)
- Signals tier
- Controller adapters
- All Bv3 Tier 6 strategic carryover items

---

*Bv4.0 — authored 2026-05-07 after chunk-by-chunk architectural review of post-Bv3 main. Confirms that Bv3 ingestion engineering landed substantively (write coordinator, batched transactions, ingest-layer debounce, adaptive backpressure, mgmt-plane LLDP filtering, topology-agnostic layout, ServiceNow mock removal). Surfaces 9 architectural concerns (C-1 through C-9) that scale poorly beyond ~200 devices: batch all-or-nothing rollback, dual-bus pattern, broken DropOldest, struct-clone on the bus, RwLock per-update, three Mutex<LruCache>s, undersized LRU caps, no log rotation, hidden counter-summary mode. Tier 1 cleans these before they harden. Tier 2 fixes the disk-fill crash mode operator warned about. Tier 3 starts always-on chaos data gathering against the existing DC lab — passive accumulation toward GNN training. Tier 4 brings up SP lab in parallel. Tier 5 timeboxed cloud spike (Oracle Always Free) for 5-day evaluation. Tier 6 GNN training when archive depth allows. Strategic recommendation: do NOT bring up SP yet, do NOT pursue K8s yet, DO timebox a cloud spike, DO accumulate 30 days of DC chaos data first. Estimated 7-10 weeks to GNN trained and deployed. References v2-Bv3 for all unchanged context.*
