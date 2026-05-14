.

.# BONSAI — Backlog Bravo Series, v3 (Bv3.0)

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_BV2_MOD.md`. Authored 2026-05-06 after careful review of post-Sprint-1 main, the operator's two-day operation experience documented in `docs/bus_memory_investigation_2026-05-06.md`, and the explicit request to engineer (not patch) the ingestion architecture before continuing on the MVP path.
>
> **What changed since Bv2-mod**: Sprint 1 of operate-first ran. The lab came up. NetBox came up. ServiceNow PDI came up. **Bonsai itself proved unable to sustain telemetry from the 8-node DC EVPN lab without crashing or growing memory unboundedly.** That's not a bug to patch — it's an architectural shape that was never tested against real-volume telemetry and is now visible.
>
> The operator captured five distinct failure modes in `bus_memory_investigation_2026-05-06.md`. The thread tying them together is structural: bonsai's hot-path write architecture treats each `TelemetryUpdate` as a single-row write through a single-writer database with no batching, while the broadcast bus has six potential subscribers any one of which can stall the entire pipeline. The mitigations applied (60s debounce, 2 GiB buffer pool, hierarchical topology layout) addressed symptoms; the root cause is unsolved.
>
> **Bv3 priorities, in order**:
>
> 1. **Engineer the ingestion+write path** so it absorbs real-volume telemetry from a 12-node lab without crashing on a 16 GB laptop. This is what blocks MVP.
> 2. **Engineer the topology rendering** so it works without prior knowledge of the topology shape — DC, SP, campus, mixed, unknown — and without false mgmt-plane links.
> 3. **Remove the ServiceNow mock entirely**. NetBox is local; ServiceNow is the operator's PDI. The mock is a confusion source.
> 4. **Continue MVP path** — enrichment productive, Path A on real graph, then Path B GNN as the north star, **only after** items 1-3 land.

---

## Table of Contents

1. [Audience and Positioning](#positioning) — see v7
2. [Bv2-mod Sprint 1 Outcome](#sprint1)
3. [Architectural Findings From Sprint 1](#findings)
4. [TIER 1 — Ingestion + Write Architecture](#tier-1) ⚡ MVP-BLOCKING ⚡
5. [TIER 2 — Topology-Agnostic Visualisation](#tier-2)
6. [TIER 3 — ServiceNow Cleanup](#tier-3)
7. [TIER 4 — Resume MVP Path (Bv2-mod Sprint 3 onward)](#tier-4)
8. [TIER 5 — Path A on real graph + Path B GNN (north star)](#tier-5)
9. [TIER 6 — Carryover from Bv2/Bv2-mod](#tier-6)
10. [Execution Order](#execution-order)
11. [Guardrails](#guardrails)

---

## <a id="positioning"></a>Audience and Positioning

**Unchanged from v7-Bv2-mod.** Controller-less primary audience across DC, campus, SP. AIOps integration as feeder, not replacement. The northstar from Bv2-mod stands: bonsai detects real faults in real labs against real telemetry, with graph-native impact analysis and Path A embeddings working.

---

## <a id="sprint1"></a>Bv2-mod Sprint 1 Outcome — What Happened

Sprint 1 was operate-first: bring up the world, capture what's broken. The captured deliverable is `docs/test_results/sprint1_operation/state-of-system-2026-05-05.md` (290 lines) plus `docs/bus_memory_investigation_2026-05-06.md` (255 lines).

**What worked**:
- 14 distinct lab/configs/seed bugs (B1-B14) found and fixed in the SR Linux startup configs, NetBox container, ServiceNow seed script, and topology seed YAML.
- DC EVPN-SRv6 lab fully operational with 8 nodes after B1-B12 fixes.
- NetBox seeded with the 8-node topology (B4 + B5 fixes).
- ServiceNow PDI seeded successfully (B14 fixes for `cmdb_ci_service` table and `sys_id` deserialization).
- Hierarchical D3 topology layout for DC Clos fabric.
- Collectors-page false-alarm fix in monolithic mode.

**What broke (and the architectural lessons)**:
- **bonsai-core OOMs after ~5 hours** of sustained operation against the 8-node lab with 320 interfaces. Buffer pool fills with dirty pages faster than eviction can free them.
- **Event bus saturates within ~1 minute** of fresh start. RSS grows from 156 MB to 5.2+ GB in 30 minutes; bus_depth pegged at 4096/4096.
- **NetBox enricher write contention** — every enrichment write fails with "Cannot start a new write transaction in the system" because the graph writer holds the write lock continuously.
- **D3 topology rendering** clusters all nodes into an indecipherable jumble in any non-DC topology because the tier vocabulary is hardcoded to spine/super-spine/leaf.
- **Mgmt-plane LLDP** (every node sees every other node via mgmt0 since they share a docker bridge) generates N×(N-1)/2 false topology links in addition to the real fabric edges.

The mitigations applied (debounce 10s→60s, buffer pool 2 GiB, capacity 4096, hierarchical layout for DC) are honest engineering responses to symptoms. They do not fix the underlying shape problem.

---

## <a id="findings"></a>Architectural Findings From Sprint 1

Five concrete findings drive Tiers 1-3. Each is named so it can be referenced from PRs and other docs.

### A-1 — Ingestion is a single-row write loop with no batching

**Where**: `src/main.rs:273-319` (graph writer task) calls `store_writer.write(update).await` once per `TelemetryUpdate`. `write()` at `src/graph/mod.rs:1521-1538` does `spawn_blocking` + `write_lock.lock()` + `write_blocking()` per update.

**Why this matters**:
- 8 devices × ~40 interfaces × initial state-snapshot burst ≈ 320 individual write transactions in the first minute alone
- Each transaction allocates dirty pages in the LadybugDB buffer pool; eviction happens between transactions, not within one
- Single-writer semantics serialise all writes; no concurrency gain available from threading
- The graph-writer task is the only consumer that actually drains the bus on a per-update basis; other subscribers (archive, output adapters, subscription_status) batch internally

**The right shape**: the graph writer should batch updates into transaction-sized chunks. A 1-second time window OR 256-update size threshold (whichever first) captures hundreds of updates into one transaction. lbug serialises one big transaction faster than 256 small ones.

### A-2 — Six potential subscribers on one broadcast channel

**Where**: `src/event_bus.rs::InProcessBus` is a `tokio::sync::broadcast` channel. Subscribers found in code:
1. `src/main.rs:273` — graph writer
2. `src/archive.rs:78` — Parquet archiver
3. `src/subscription_status.rs:43` — last-update tracker
4. `src/output/prometheus.rs:130` — Prometheus adapter
5. `src/output/traits.rs:321` — base trait used by Splunk + Elastic + ServiceNow EM (3 more receivers when enabled)
6. `src/ingest.rs:349` — collector forwarder (in collector mode)

**Why this matters**: tokio broadcast frees a slot only when **every** receiver has consumed the message. The slowest subscriber sets the pace for the whole channel. If the graph writer is blocked on a slow lbug write, the entire bus stalls and the producer side accumulates back-pressure (since `publish` calls `try_send`, this means messages are *dropped* into the void, not queued).

**The right shape**:
- Subscribers that don't need every update (counters, debounced) should be **debounce-aware** at subscribe time, not at receive time, so they don't sit on slots they won't read
- Subscribers that do need every update (archive, output adapters) should pull from a **lag-tolerant queue** instead of a fixed-capacity broadcast
- Two-tier architecture: a fast "router" channel and per-subscriber bounded queues with explicit overflow policy

### A-3 — Write contention between graph writer and enrichers

**Where**: `src/enrichment/netbox.rs` calls `with_write_retry()` (newly added in Sprint 1) to handle the "Only one write transaction in the system" error. This is treating the symptom, not the cause.

**Why this matters**: enrichment is intentional, scheduled, low-frequency. Telemetry writes are continuous, high-frequency. They should never compete for the write lock. The current architecture has them race; the loser retries with exponential backoff, and during the startup burst the loser exhausts retries.

**The right shape**: a **single write coordinator** task that owns all write-transaction issuance. Telemetry writes and enrichment writes both submit to the coordinator's queue. The coordinator batches telemetry into chunks, runs enrichment between batches at a configured cadence, and never deadlocks itself. This is a textbook actor pattern.

### A-4 — Topology rendering hardcoded to DC Clos vocabulary

**Where**: `ui/src/lib/Topology.svelte:180-186` `nodeTier()` returns 0 for `role === 'spine' && hostname.includes('super')`, 1 for `role === 'spine'`, 2 otherwise. Tier labels at line 211: `['Super-Spines', 'Spines', 'Leaves']`.

**Why this matters**:
- An SP topology (PE/P/RR/CE) collapses to tier 2 for every node — entire topology in one row
- A campus topology (access/distribution/core) similarly collapses
- An unknown-role topology (operator hasn't filled in roles yet) collapses
- The labels actively lie: "Leaves" appears when there are no leaves, just unknown-role nodes

**The right shape**: rendering algorithm derives layout from **graph structure** (degree distribution, betweenness, hierarchy depth from a chosen root) rather than role string vocabulary. When operator-supplied role hints exist, they refine the layout; when absent, the layout still works.

### A-5 — Mgmt-plane LLDP creates N×(N-1) false topology links

**Where**: bonsai ingests every LLDP neighbor regardless of source interface. ContainerLab places all device containers on a shared `clab_mgmt` bridge. Every node sees every other node via mgmt0 LLDP. The graph stores all of these; the topology API returns them; the UI renders them.

**Why this matters**: visual rendering is overwhelmed by false fully-meshed links. Even after A-4 (smarter layout), the graph contains noise that confuses graph algorithms — `device_centrality` reports every node as maximally connected, `site_dependency_depth` cross-site reachability is meaningless, blast radius queries explode.

**The right shape**: distinguish **fabric LLDP** from **mgmt-plane LLDP** at ingest time. Two signals:
1. The `local_interface` is a known mgmt interface (mgmt0, eth0, fxp0, etc., per vendor)
2. The link is "fully meshed" (every node connects to every other node) — high probability mgmt-plane

Fabric LLDP becomes `CONNECTED_TO`; mgmt LLDP becomes `MGMT_LINK` (separate edge type) and is excluded from topology rendering by default. Operators can opt in to seeing mgmt links via a UI toggle.

---

## <a id="tier-1"></a>TIER 1 — Ingestion + Write Architecture ⚡ MVP-BLOCKING ⚡

The operator was unequivocal: "this is not a trial-and-error problem, it's an engineering problem." Tier 1 designs and implements the engineered ingestion architecture. **Until this lands, no further MVP work proceeds** because every test against the running lab will be poisoned by the same memory and crash issues.

### T1-1 (Bv3) — Single-writer coordinator with batched transactions

**What**: a dedicated `write_coordinator` task that owns the lbug write-transaction issuance. All graph-write callers (telemetry from the bus, enrichment runs, subscription status, registry mutations, embedding upserts) submit `WriteRequest` enums to its mpsc queue.

**Architecture**:

```rust
enum WriteRequest {
    Telemetry(TelemetryUpdate),
    Enrichment { source: String, properties: Vec<(NodeRef, String, Value)> },
    SubscriptionStatus(SubscriptionStatusWrite),
    Detection(DetectionEventWrite),
    Embedding { address: String, version: String, vector: Vec<f32> },
    // ... other write types
}

// The coordinator:
async fn write_coordinator(
    mut rx: mpsc::Receiver<WriteRequest>,
    db: Arc<Database>,
    cfg: WriteCoordinatorConfig,
) {
    let mut telemetry_batch: Vec<TelemetryUpdate> = Vec::with_capacity(cfg.batch_size);
    let mut flush_timer = tokio::time::interval(cfg.flush_interval);
    
    loop {
        tokio::select! {
            req = rx.recv() => match req {
                Some(WriteRequest::Telemetry(u)) => {
                    telemetry_batch.push(u);
                    if telemetry_batch.len() >= cfg.batch_size {
                        flush_telemetry_batch(&db, &mut telemetry_batch).await;
                    }
                }
                Some(WriteRequest::Enrichment { source, properties }) => {
                    // Flush any pending telemetry first; enrichment runs cleanly between batches
                    if !telemetry_batch.is_empty() {
                        flush_telemetry_batch(&db, &mut telemetry_batch).await;
                    }
                    apply_enrichment(&db, source, properties).await;
                }
                // ... other write types
                None => break,
            },
            _ = flush_timer.tick() => {
                if !telemetry_batch.is_empty() {
                    flush_telemetry_batch(&db, &mut telemetry_batch).await;
                }
            }
        }
    }
}
```

**Default config**:
- `batch_size = 256` updates per transaction
- `flush_interval = 1 second`
- Backpressure: queue is bounded at 4096; on full, oldest telemetry updates are dropped first (telemetry is regenerable; other writes are not)

**Where**:
- New `src/write_coordinator.rs` (~300 lines)
- `src/main.rs` graph writer task replaced with a dispatcher that pushes `WriteRequest::Telemetry` to the coordinator
- `src/enrichment/mod.rs` enricher trait modified to push `WriteRequest::Enrichment` instead of opening its own connection
- `src/graph/mod.rs::write()` becomes deprecated; callers move to the coordinator

**Done when**:
- Memory profile shows RSS plateau within 15 minutes of fresh start, not unbounded growth
- 8-node DC lab sustains 4-hour run with RSS stable below 1.5 GB
- Enrichment runs succeed during startup burst with zero "write transaction in the system" errors
- `bonsai_graph_write_latency_seconds` p99 below 100ms during normal operation

### T1-2 (Bv3) — Replace broadcast bus with router + per-subscriber bounded queues

**What**: the broadcast channel pattern — where one slow subscriber stalls the whole channel — is replaced with a router task and per-subscriber tokio mpsc queues. Each subscriber has its own bounded queue; on overflow, each subscriber declares its own policy (drop oldest, drop newest, log warning).

**Architecture**:

```rust
// Producer publishes once.
bus.publish(update);

// The router clones to each subscriber's queue.
// Each subscriber's queue is a tokio::sync::mpsc::channel(capacity).
// On full, the router applies the subscriber's overflow policy.

trait BusSubscriber: Send + Sync {
    fn name(&self) -> &str;
    fn capacity(&self) -> usize;
    fn overflow_policy(&self) -> OverflowPolicy;
    fn submit(&self, update: TelemetryUpdate);
}

enum OverflowPolicy {
    DropOldest,    // for archive — losing one update is fine
    DropNewest,    // for output adapters — once-and-done is fine
    BlockProducer, // for write coordinator — must not lose
}
```

**Why this matters vs the existing broadcast**:
- A slow archive does not stall a fast graph writer
- An SSE subscriber with no clients connected does not retain messages forever (DropOldest)
- The producer does not bear the cost of slow subscribers
- Per-subscriber metrics (queue depth, overflow count) make slowness diagnosable

**Where**:
- `src/event_bus.rs` substantially rewritten
- All six subscribers ported to the new trait
- Existing `EventBus` trait kept as a deprecation shim; backends that implement `subscribe()` get auto-wrapped with the new pattern

**Done when**:
- A deliberately-slow subscriber (sleep 5s per update) does not affect other subscribers' throughput
- `bonsai_subscriber_queue_depth{subscriber="archive"}` and similar are first-class metrics
- The "100% full" warning is per-subscriber and identifies the laggard by name

### T1-3 (Bv3) — Counter sample debounce moves to ingest layer

**What**: today the graph writer at `src/main.rs:286-304` checks `LruCache<String, Instant>` to skip counter writes within the debounce window. **This is the wrong layer**. The bus has already accepted the message; the bus capacity has been consumed; the message has been broadcast to all five other subscribers; only then does the graph writer decide to skip.

The right place is **at ingest time** (`src/ingest.rs::process_subscribe_response` or the gNMI ingestion equivalent). If a counter update arrives within the debounce window, drop it before publishing. The bus stays empty; other subscribers don't see it; downstream cost is zero.

**Where**:
- `src/ingest.rs` — pre-publish debounce check
- `src/main.rs` — remove the writer-side debounce
- `src/config.rs` — `[ingest.counter_debounce_secs]` (move from `[event_bus]`)

**Done when**:
- bus_depth during startup burst is observably lower than current
- Archive partition counts decrease (fewer counter updates archived)
- The graph writer task is simpler (no per-update keying logic)

### T1-4 (Bv3) — Adaptive backpressure for telemetry surge

**What**: when the write coordinator queue is >75% full, telemetry ingest applies adaptive backpressure to gNMI subscriptions. Two graduated responses:

1. **75-90% full**: drop INTERFACE_STATS counter updates at ingest (keep state changes, BGP, IS-IS, BFD)
2. **90-100% full**: also drop INTERFACE_OPER state if changing rapidly (>10 transitions/min)

Detection events, BGP/IS-IS/BFD adjacency changes, and explicitly-flagged "critical" paths are **never** dropped. They have priority queues feeding the coordinator.

**Where**:
- `src/write_coordinator.rs` exposes queue depth via `Arc<AtomicU64>`
- `src/ingest.rs` consults the depth before publishing each update
- `src/config.rs` — `[ingest.backpressure]` thresholds and exemption list

**Done when**:
- A burst of 1000 counter updates triggers the backpressure response visibly in metrics
- BGP detection events still fire during the backpressure window
- `bonsai_ingest_backpressure_drops_total{path}` increments correctly

### T1-5 (Bv3) — Memory budget assertions in the always-on lab

**What**: a Prometheus alert rule plus `/api/_test/status` field that asserts:
- RSS < 1.5 GB at all times in lab-dc profile
- buffer_pool_pct < 80% at all times
- write_coordinator_queue_depth < 75% sustained for >5 min

Alert fires within 1 minute of breach; status endpoint reflects breach for AI consumption.

**Where**:
- `docker/grafana/alerts/bonsai-memory.yml`
- `src/http_server.rs::test_status_handler` extension

**Done when**:
- A deliberate config regression (e.g. removing the batch_size cap) triggers the alert
- `/api/_test/status` shows the breach as a structured field

### T1-6 (Bv3) — Engineering doc for the new architecture

**What**: a single-page architecture doc at `docs/ingestion_architecture.md` explaining: the write coordinator, the per-subscriber queue pattern, ingest-layer debounce, backpressure thresholds. References A-1 through A-3 above.

**Where**: `docs/ingestion_architecture.md`

**Done when**: the doc is concrete enough that a Claude Code session diagnosing a future ingestion regression can read it and understand the design intent without re-deriving from code.

---

## <a id="tier-2"></a>TIER 2 — Topology-Agnostic Visualisation

### T2-1 (Bv3) — Mgmt-plane LLDP filtering at ingest

**What**: A-5. At LLDP ingest, classify each neighbor as fabric or mgmt. Two signals combined:

1. **Per-vendor mgmt-interface allowlist**: each `Capabilities` profile declares its mgmt interfaces (`mgmt0` for SR Linux, `Mgmt0/RP0/CPU0/0` for Cisco IOS-XR, `fxp0` for Junos, `Management1` for Arista, `eth0` for FRR). LLDP neighbors received from these interfaces become `MGMT_LINK` edges instead of `CONNECTED_TO`.

2. **Density heuristic**: at graph-write time, if N×(N-1)/2 LLDP edges exist among N devices and they all originate from interface(s) matching pattern `mgmt|management|fxp|console`, the entire set is reclassified as `MGMT_LINK`.

**Where**:
- `config/path_profiles/*.yaml` — declare mgmt interface patterns per vendor
- `src/graph/mod.rs::write_lldp_neighbor` — apply the classification
- New edge type `MGMT_LINK` in graph schema

**Done when**:
- The 8-node DC lab shows 14 fabric `CONNECTED_TO` edges (expected for the topology) instead of 14 + 28 = 42 (fabric + mgmt-mesh)
- An operator query `MATCH (a:Device)-[:CONNECTED_TO]-(b:Device) RETURN a, b` returns only the fabric topology

### T2-2 (Bv3) — Topology-agnostic layout algorithm

**What**: A-4. Replace `nodeTier()` hardcoded to DC vocabulary with a layout algorithm that derives hierarchy from graph structure.

**Algorithm** (heuristic, runs client-side):
1. Identify a root using degree centrality + betweenness — the highest-betweenness node, or operator-supplied
2. Compute BFS depth from root for every node
3. Each BFS depth becomes a tier (Y rail)
4. Within a tier, X position is decided by the simulation balanced against connectivity to adjacent tiers
5. If operator role hints exist (spine, leaf, pe, p, rr, ce, access, distribution, core, super-spine), they refine which tier the node lands in; absent hints fall back to BFS-depth-only

**For an unknown-role topology**: the layout still works because BFS depth from the betweenness-central node always exists.

**For a flat topology** (ring, mesh): all nodes at one depth; layout becomes circular.

**Tier labels**: derived from the most common role at each depth (e.g. "spine layer" when 60% of tier-1 nodes have role=spine; "depth 1" when no role majority). Never lie about the layer's contents.

**Where**:
- `ui/src/lib/Topology.svelte` rewrite of layout function
- Backend `/api/topology` extension to return suggested-root + role-distribution stats so the client doesn't recompute

**Done when**:
- The 8-node DC lab renders identically to today (3 tiers: super-spines, spines, leaves)
- A 9-node SP lab (when running) renders with correct tier separation (PE/P/RR or by depth from a chosen anchor)
- A 4-node lab with no role hints renders without jumble; nodes don't drift off-canvas
- Operator can override the suggested root via UI dropdown

### T2-3 (Bv3) — Mgmt-plane visibility toggle

**What**: a UI toggle in Topology workspace: "Show mgmt-plane links". Default off. When on, `MGMT_LINK` edges render as dashed grey lines. When off (default), only fabric edges render. Useful for diagnosing actual mgmt connectivity issues.

**Where**: `ui/src/lib/Topology.svelte` extension.

**Done when**: toggle works; default state is off; mgmt-plane state visible when toggled on.

---

## <a id="tier-3"></a>TIER 3 — ServiceNow Cleanup

The user's instruction: "remove all references to servicenow mock. we are only doing pdi. it should confuse anyone. netbox is local servicenow is pdi."

### T3-1 (Bv3) — Delete `docker/mock-servicenow/`

Files to remove:
- `docker/mock-servicenow/Dockerfile`
- `docker/mock-servicenow/app.py`
- `docker/mock-servicenow/requirements.txt`
- `docker/mock-servicenow/seed.yaml`
- The `docker/mock-servicenow/` directory itself

### T3-2 (Bv3) — Remove mock references from compose

`docker-compose.yml` — delete the entire `servicenow-mock:` service block plus the `servicenow-mock` and `enrichment-test` profile mentions.

### T3-3 (Bv3) — Remove mock references from scripts

- `scripts/seed_lab.sh` — strip the mock-servicenow handling
- `scripts/e2e_servicenow_pdi_test.sh` — remove any mock fallback paths
- `python/tests/test_seed_servicenow_pdi.py` — remove mock-targeting tests

### T3-4 (Bv3) — Remove mock references from CI

`.github/workflows/nightly-integration.yml` — delete any `servicenow-mock` step.

### T3-5 (Bv3) — Remove mock references from docs

- `docs/integration_compliance.md` — replace mock references with PDI-only guidance
- `memory/project_sprint_progress.md` — update references
- The old backlogs (v8, v9) are historical and stay; new backlog content (Bv3 itself, ai_feedback_protocol.md, README) refers only to PDI

### T3-6 (Bv3) — README update

The README's "Quick start" section gains a clear single source of truth:

> **Enrichment integrations**:
> - **NetBox** is local — bring up via `docker compose -f docker/compose-external.yml --profile netbox up -d`. Pre-seeded by `scripts/seed_external.sh`.
> - **ServiceNow** is your own ServiceNow PDI (Personal Developer Instance). Get one from [developer.servicenow.com](https://developer.servicenow.com), set `SNOW_INSTANCE_URL`, `SNOW_USERNAME`, `SNOW_PASSWORD` in `.env`, and `scripts/seed_external.sh` will populate it.

No mock referenced anywhere.

### T3-7 (Bv3) — Verify the cleanup

A grep -r over the whole repo for "mock-servicenow", "servicenow-mock", "servicenow_mock", "mock.servicenow" returns zero hits in non-historical files (old backlogs are excluded; they are archive).

---

## <a id="tier-4"></a>TIER 4 — Resume MVP Path (Bv2-mod Sprint 3 onward)

After Tier 1-3 land, MVP work resumes from where Bv2-mod Sprint 3 was planned.

### T4-1 — NetBox enricher productive against the real graph

Now possible because T1-1 eliminates write contention. Schedule enricher to run hourly; verify VLAN/Prefix/Application nodes land on the graph; verify enrichment workspace shows last-run summary with `nodes_touched > 0`.

### T4-2 — ServiceNow PDI enricher productive

Same pattern as T4-1; runs against the operator's PDI.

### T4-3 — Path A spectral embeddings against the real graph

`python -m bonsai_ml.embeddings --base-url http://localhost:3000 --dim 16` against the populated, enriched real graph. Embeddings post to `/api/graph/embeddings/upsert`. Device nodes carry `embedding` properties. Verify in Explorer.

### T4-4 — Path A model card

Honest documentation of algorithm, hyperparameters, dataset, evaluation. Reference v9 T8 + Bv2-mod Sprint 3.

### T4-5 — Detection rule tuning from real lab data

Rules that fire excessively → tighten. Rules that don't fire when expected → fix. Detection latency p95 baselines from chaos harness.

---

## <a id="tier-5"></a>TIER 5 — Path A on real graph + Path B GNN (north star)

Reachable after Tier 4 and after enough chaos archive accumulates. References v9 T8, Bv1 T2, Bv2 Tier 3, Bv2-mod Sprint 5. Unchanged.

### T5-1 — GNN data loader against the populated chaos archive
### T5-2 — GraphSAGE / GAT training; honest evaluation vs rules + tabular ML
### T5-3 — Online inference path; UI surfacing of GNN scores
### T5-4 — Model card

---

## <a id="tier-6"></a>TIER 6 — Carryover from Bv2/Bv2-mod

All unchanged from Bv2-mod Tier 6:
- Investigation agent in production (post-MVP, pending token budget)
- HIL graduated remediation in production use
- Output adapter productive use beyond Prometheus
- Signals tier (syslog/traps)
- Controller adapter implementations
- Operator path overrides UI workspace
- Subscription resolution audit
- Catalogue plugin install command
- AIOps readiness checklist
- NL query, bulk CSV onboarding, scale architecture, S3 archive
- Campus topology
- Bitemporal schema, schema migration, Grafeo evaluation

Plus the Bv2 hardcoding catalogue (H-1 through H-12) — mostly fixed by Tier 1-2 of Bv3 since they removed the code paths the hardcoding lived on. Specifically:
- H-1 (DC-centric tier vocabulary in `subscription_health_by_tier`) — addressed by T2-2 generalising to topology-agnostic layout; the algorithm itself still has the hardcoding and gets fixed in T6 carryover
- H-2 through H-4 (agent config) — defer with Tier 6 (agent itself is post-MVP)
- H-9 (sanitiser false positives) — Tier 6 carryover
- Others — opportunistic

---

## <a id="execution-order"></a>Execution Order

### Sprint 1 of Bv3 — Ingestion architecture (3-4 weeks) ⚡ MVP-BLOCKING ⚡

Bigger sprint than usual. The architecture changes are coupled and must land together.

1. T1-3 ingest-layer debounce (smallest, lands first as warmup)
2. T1-1 write coordinator (the centerpiece)
3. T1-2 router + per-subscriber queues (paired with T1-1 — both touch the bus)
4. T1-4 adaptive backpressure
5. T1-5 memory budget assertions
6. T1-6 architecture doc

**Sprint 1 success criterion**: 12-node lab sustains 8-hour run with RSS stable below 1.5 GB and zero crashes.

### Sprint 2 of Bv3 — Topology rendering + ServiceNow cleanup (1-2 weeks)

7. T3-1 through T3-7 ServiceNow mock removal (small; can be done in parallel with rendering work)
8. T2-1 mgmt-plane LLDP filtering (graph-write change)
9. T2-2 topology-agnostic layout algorithm (UI change)
10. T2-3 mgmt-plane visibility toggle (UI extension)

**Sprint 2 success criterion**: DC lab + SP lab both render correctly without prior knowledge of the topology shape; mgmt-plane links not in default view; mock references gone repo-wide.

### Sprint 3 of Bv3 — Resume MVP path (2 weeks)

Equivalent to Bv2-mod Sprint 3.

11. T4-1 NetBox enricher productive
12. T4-2 ServiceNow PDI enricher productive
13. T4-3 Path A embeddings against real graph
14. T4-4 Path A model card
15. T4-5 Detection rule tuning

**Sprint 3 success criterion**: a Cypher query in the Explorer answers a real operational question using real enriched data; embeddings exist on real Device nodes; detection rules tuned with documented baselines.

### MVP gate (between Sprint 3 and Sprint 4)

By the end of Sprint 3, MVP definition is met under the more demanding criterion that includes "system runs sustained without crashing." Operator assesses readiness for GNN sprint.

### Sprint 4 of Bv3 — Path B GNN (3-4 weeks)

16. T5-1 GNN data loader
17. T5-2 training + honest evaluation
18. T5-3 online inference path
19. T5-4 model card

**Sprint 4 success criterion**: GNN trained on 30+ days of real chaos archive shows performance ≥ rule-based baseline on held-out test set; deployed; UI surfaces scores.

### After Bv3 — strategic carryover

Tier 6 items (investigation agent in production, HIL, signals, etc.) are post-MVP and defer to subsequent backlogs.

---

## Estimated Timeline

| Milestone | Sprints | Cumulative weeks |
|---|---|---|
| Bv3 Sprint 1: ingestion architecture | 3-4 | 3-4 |
| Bv3 Sprint 2: topology + cleanup | 1-2 | 4-6 |
| Bv3 Sprint 3: MVP resume | 2 | 6-8 |
| **MVP gate** | | **6-8 weeks** |
| Bv3 Sprint 4: GNN | 3-4 | 9-12 |
| **North-star milestone** | | **9-12 weeks** |

Bv3 adds ~2-4 weeks to Bv2-mod's timeline because Tier 1 is genuine architectural work, not a quick fix. The trade-off is that everything after Tier 1 runs on a stable foundation; without Tier 1, every subsequent test is poisoned.

---

## <a id="guardrails"></a>Guardrails

### New in Bv3

- **Single write coordinator owns lbug write transactions.** No component may open a write transaction directly except via `WriteRequest` submission to the coordinator. New PRs adding `Connection::new(&db).execute(...)` for writes are rejected.
- **Telemetry writes batch.** Default batch size 256, default flush interval 1 second. Per-update writes to lbug are rejected on review.
- **Bus subscribers declare overflow policy.** Every subscriber explicitly chooses DropOldest, DropNewest, or BlockProducer. Implicit "wait forever" semantics are gone.
- **Counter debounce is at ingest, not at the writer.** Once a TelemetryUpdate is on the bus, it's getting written. If we don't want it written, we don't put it on the bus.
- **Topology rendering does not assume DC vocabulary.** Layout algorithms derive structure from the graph, not from role-string heuristics.
- **Mgmt-plane LLDP is a separate edge type from fabric LLDP.** Default views show fabric only. Operators opt in to mgmt visibility.
- **ServiceNow is PDI-only.** Mock is removed; references in current docs and scripts use the PDI exclusively.

### Unchanged from v7-Bv2-mod

All prior architectural invariants and discipline continue. Reference earlier backlogs.

### Anti-patterns to reject

- "We can patch the buffer pool size to make it work" — no, the buffer pool is not the issue, it's the write rate
- "Just increase the bus capacity to 16384" — no, bigger capacity hides slow subscribers, doesn't solve them
- "Enrichment retry-on-conflict is good enough" — no, the conflict shouldn't exist
- "DC vocabulary is fine; SP can come later" — no, the topology algorithm must work for unknown shapes today
- "The mock is still useful for CI" — no, PDI testing is the discipline; mock confuses

---

## What Bv3 Explicitly Excludes

- New functional features beyond ingestion + rendering + cleanup
- Investigation agent productive use (post-MVP, pending token budget)
- Signals tier
- Controller adapters
- All Tier 6 strategic carryover items

---

*Bv3.0 — authored 2026-05-06. Engineers the ingestion + write path so bonsai sustains real-volume telemetry from a 12-node lab without the OOM/crash/contention failure modes documented in `docs/bus_memory_investigation_2026-05-06.md`. Engineers topology rendering to work without prior knowledge of topology shape. Removes the ServiceNow mock entirely per operator direction. Carries Bv2-mod's MVP-then-GNN sequencing forward unchanged. Sprint 1 is the centerpiece (ingestion architecture); Sprint 2 cleans up rendering + mock; Sprint 3 resumes the MVP path; Sprint 4 trains the GNN. Estimated 6-8 weeks to MVP, 9-12 weeks to north star. References v2-Bv2-mod for unchanged context. Audience framing, gNMI-only hot path, controller-less primary target, AIOps-feeder positioning, and all prior architectural decisions remain unchanged.*
