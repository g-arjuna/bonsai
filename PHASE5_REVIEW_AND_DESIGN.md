# PHASE5_REVIEW_AND_DESIGN.md

> Review of Phases 1–4 of bonsai against its founding philosophy, plus the architectural baseline for Phase 5 ML work and a principled approach to scale-readiness without premature optimisation.

---

## Part 0 — Why This Document Exists

The original thesis was not to build something famous. It was:

1. Replicate Google's ANO framework architecture at lab scale using open source primitives
2. Fill a real gap — a streaming-first, graph-native network state engine with closed-loop detect/heal that the OSS ecosystem does not have
3. Upskill from mile-wide-inch-deep into someone who understands systems at depth
4. Be vendor-neutral, LLM-agnostic, and disciplined about scope

After four completed phases it is the right moment to stop and audit. This document answers three questions:

- Did we stay true to the philosophy?
- What's architecturally solid and what's a future pain point?
- What does Phase 5 need to look like so the ML work actually happens instead of dying in refactor?

The review is deliberately honest. Where things drifted, we say so. Where we nailed it, we say so. This is a self-respect exercise, not a validation exercise.

---

## Part 1 — Philosophy Fidelity Check

Marking each founding principle: ✅ held, ⚠️ drifted but recoverable, ❌ violated.

### ✅ Streaming-first ingestion

`src/subscriber.rs` is genuinely streaming. Every device gets its own tokio task running a long-lived gNMI Subscribe. Path selection uses `Mode::Stream`, with `ON_CHANGE` for state transitions and `SAMPLE` for counters. There is no polling loop anywhere in the ingestion path. When connectivity drops, exponential backoff reconnects. This is exactly what we said we'd build.

One nuance: the Python rule engine does have a poll loop (`engine.py::_poll_loop`) for counter deltas and topology diffs. This is correct — counter rates need two samples, so polling the graph at a cadence is architecturally appropriate. It's polling the graph, not the devices. The streaming-first principle is about ingestion, and it holds.

### ✅ Graph-native state

`src/graph.rs` models the network as a property graph: Device, Interface, BgpNeighbor, LldpNeighbor as nodes; HAS_INTERFACE, PEERS_WITH, HAS_LLDP_NEIGHBOR, CONNECTED_TO as edges. CONNECTED_TO edges are derived from LLDP data with proper backfill logic for the race condition where LLDP arrives before the interface node exists. StateChangeEvent, DetectionEvent, and Remediation are nodes linked via REPORTED_BY, TRIGGERED, RESOLVES — the full detection-and-healing story lives in the graph.

This is unambiguously graph-native. No flat table dumps, no parallel key-value stores for state. Good.

### ✅ Vendor-neutrality via Capabilities-driven path selection

`ModelCapabilities::from_response` in `subscriber.rs` decides which subscription paths to use based on the Capabilities RPC response from the device itself. SRL native paths get used because SRL advertises `srl_nokia-*` models; OpenConfig paths are the fallback. There is no per-vendor `if vendor == "cisco" { ... } else if vendor == "juniper" { ... }` branching.

The `vendor_label` field is explicitly documented as *for logging and Device node tagging only, never for path routing decisions*. This is the right discipline and it shows up in the code the way it should.

Normalisation via `json_i64_multi` with candidate key names (`in-packets` SRL, `packets-received` XR native, `input-packets` Junos, `in-pkts` OC) is the right pattern. A fifth vendor adds key aliases, not code branches.

### ✅ LLM-agnostic query layer

The gRPC API (`proto/bonsai_service.proto`) exposes raw Cypher via `Query()` plus typed helpers (`GetDevices`, `GetInterfaces`, `GetBgpNeighbors`, `GetTopology`, `StreamEvents`). This is a proper neutral surface. An LLM can call these as tools via MCP; Grafana can call them via a REST proxy; a Python script uses them directly. No LLM-specific coupling anywhere.

### ✅ Credentials never leak

`DECISIONS.md` captures the constraint and `remediations.py` comment ("credentials never leave the Rust process") enforces it. `PushRemediation` takes target address + YANG path + JSON value, nothing more. The Rust `gnmi_set.rs` is the only place that handles credentials. Python doesn't see them. This is architecturally clean and aligns with the mTLS-preferred decision.

### ⚠️ Temporal by design — *designed but not implemented*

The bitemporal pattern (valid_from/valid_to on every node) is documented in DECISIONS.md as the agreed approach, but the schema shows `updated_at` only. There is no valid_from/valid_to column, no tombstone writes on state change, no "reconstruct graph state at time T" query.

The README says "temporal by design — every state change versioned, reconstruct graph state at any past time." The current implementation does not do this. What it does have is an append-only StateChangeEvent table that captures transitions, which gives you *event history* but not *graph state at time T*.

This is a drift, not a violation. It's fine to defer — but it needs to be called out explicitly as a known gap, not left as an implied-done item in the README. **Action: update README to say "Temporal: StateChangeEvent append-only log (current) → full bitemporal in Phase 5.5 or when ML needs it."**

Phase 5 will actually need temporal queries for training data ("what did the graph look like 15 minutes before this failure"). So this gap becomes real work soon.

### ⚠️ Scope discipline — *mostly held, one quiet expansion*

Held:
- No SNMP, no NETCONF — confirmed
- No Kubernetes, no HA, no multi-tenancy — confirmed
- cRPD was deferred cleanly, not absorbed — confirmed
- No config-writing UI — confirmed

Quiet expansion:
- The demo feature expansion into *three* trigger modes (event-driven, poll-based counters, poll-based topology) was not in PROJECT_KICKOFF.md. It's defensible and correct — but it happened without an ADR capturing the trade-off. 
- Juniper Junos field-name handling shows up in `telemetry.rs` even though cRPD was deferred. Dead code in an otherwise tight codebase.

**Action: write an ADR for the hybrid trigger model retroactively (DECISIONS.md already has 2026-04-18 "Rule trigger model: hybrid event + poll" — good, this was captured). Remove the Junos classifier paths until cRPD is re-enabled, or leave a TODO comment marking them explicitly as speculative. Prefer removal — you can always add them back when the vendor is back in scope.**

### ❌ Nothing outright violated

No principle was outright violated. The one place to be vigilant: the rule engine's poll loop is correct, but it's a foothold where future "just poll the device directly" pressure could creep in. Watch for this in Phase 5.

---

## Part 2 — Architectural Analysis

### What's genuinely solid

**The subscriber / telemetry / graph separation is the right shape.** You have three clean layers:

1. `subscriber.rs` — the wire. Speaks gNMI, decodes protobuf, emits `TelemetryUpdate` structs.
2. `telemetry.rs` — the classifier. Pattern-matches the path+value into `TelemetryEvent` enums.
3. `graph.rs` — the writer. Consumes events, issues Cypher upserts, emits `BonsaiEvent` broadcasts.

These layers are decoupled via an `mpsc::channel<TelemetryUpdate>` (telemetry channel) and a `broadcast::channel<BonsaiEvent>` (event bus). The telemetry channel is bounded at 1024 — ingestion back-pressure propagates naturally. The event broadcast is 1024 deep with lagged-receiver-dropped semantics — correct for streaming to consumers that might be slow.

This three-layer split with two channels is genuinely good. It's what lets you bolt on anomaly detection without touching the ingestion code, and it's what will let you bolt on ML inference the same way.

**The leaf-grouping transform in `subscriber.rs`** (consolidating scalar leaves from cEOS and XRd back into container-level JSON blobs) is a thoughtful fix. The alternative — classifiers that handle both granularities — would have multiplied the `telemetry.rs` surface. You did the normalisation at the transport edge where it belongs.

**The `Detector` ABC with shared `extract_features`** is exactly the right abstraction for Phase 5. When you move from rules to ML, `extract_features` stays, `detect()` swaps. `features_json` is stored on every DetectionEvent in the graph from day one — so you already have labelled training data being generated by Phase 4. This is one of the best architectural decisions in the project.

**The credentials-stay-in-Rust-via-PushRemediation model** is production-correct. It solves a real problem (don't leak creds to Python) with a clean mechanism (proxy RPC). This generalises: any future component that needs to touch devices goes through this RPC and inherits the credential discipline.

**The LadybugDB choice**, given Kuzu's acquisition, is the right defensive pick. Active fork, MIT, Cypher, Rust bindings, columnar OLAP strengths. Grafeo is noted as a fallback. The 60-day-no-release review trigger is exactly the right form of contingency thinking.

### What's a future pain point

**1. Single writer, single process, single DB file.** `write_lock: Arc<Mutex<()>>` serialises all graph writes through a global in-process mutex because LadybugDB only permits one concurrent write transaction. For the lab this is fine — write rate is tens per second, mutex contention is negligible. At scale this is a wall. Every additional subscriber contends for the same lock. Throughput is capped by single-threaded write performance.

The *seam* for fixing this later exists (`GraphStore::write` is the single entry point), but the current code does not batch writes or decouple subscribers from the writer. At 10,000 updates/second this becomes a bottleneck quickly.

**2. The telemetry channel is a shared mpsc.** Every subscriber shares one `tokio::sync::mpsc::Sender<TelemetryUpdate>` (channel capacity 1024). If one subscriber floods, everyone is affected. Better shape is per-subscriber channels or a lock-free MPMC like `flume` with fair scheduling. Not urgent, but worth noting.

**3. No event durability.** Events are in-memory broadcasts. If the API is restarted, consumers miss what happened during the downtime. For lab this is fine. For any production use you'd want the event bus to be replayable (Kafka, NATS JetStream, or even just an append-only file with an offset).

**4. `backfill_connected_to` is a scan-on-write.** Every interface write runs two LLDP lookups. For a 3-node lab this is cheap. For 500 devices × 100 interfaces each, this is 50,000 scans per full telemetry cycle. Indexed or not, it adds up. Make a mental note: this is the kind of thing that is fine until it isn't.

**5. `get_bgp_state` reads before every BGP write** to detect transitions. Same pattern as (4) — a read before every write. At lab scale invisible; at fleet scale this doubles write I/O.

**6. Two-step BGP session-bounce via sleep(1).** `remediations.py::_bgp_session_bounce` does `disable → sleep(1) → enable`. This is fine for the demo. For a real system, this should watch the graph for `session_state == "idle"` confirmation before re-enabling. Otherwise on a slow-converging session, the enable fires before disable has taken effect.

**7. `bonsai.db` is named fixed but has no migration story.** If you add a column to `Interface`, existing databases break. A schema version and migration path is deferred; that's fine; but capture it in DECISIONS.md as a known deferral.

None of these are urgent. All are seams that need to exist for scale. Several of them are design-by-leaving-the-right-entry-point-obvious (the single `write_lock`, the single channel) — which is actually correct for this phase. The discipline is to write code that will be rearranged at scale, not code that is already scaled.

### What looks over-engineered for a lab

Surprisingly little. The code is right-sized for where it is. If anything, I'd flag:

- The `StateChangeEvent` table duplicates information that is also in the event broadcast. For the lab you could get away with broadcast-only. But the table is the persistence story and it's cheap to keep — don't remove it.
- The full TLS handling in `subscriber.rs::connect` and `gnmi_set.rs::open_channel` is thorough. For a lab with plaintext gRPC to SRL, this is overkill. But security shortcuts become habits; the thoroughness is correct.

### What looks under-engineered

- **No metrics on bonsai itself.** There is no `/metrics` endpoint showing telemetry ingestion rate, graph write latency, broadcast lag, subscriber reconnect count. Phase 5 ML will need to observe these. Adding a Prometheus `/metrics` endpoint exposing bonsai's internal counters is low effort and pays off immediately.
- **No integration test for the full loop.** Unit tests exist but the "inject a BGP flap, verify it shows up in the graph" flow is manual. This is where the lab doubles as a test harness — a `pytest` that spins up ContainerLab, runs bonsai, injects a flap, asserts graph state.
- **DetectionEvent has no link to the StateChangeEvent that triggered it.** You can correlate by timestamp + device, but there should be a `TRIGGERED_BY` edge from DetectionEvent to StateChangeEvent. This is important for Phase 5 — when ML says "this was wrong", you need to know which input triggered it.

---

## Part 3 — Scale Concerns, Addressed Concretely

The user asked: *do we need to prune events, split the writer, make bonsai itself distributable, onboard devices dynamically?* The right answer is not "yes, do all of this now" and not "no, don't worry about it." It is: **draw the seams cleanly today so each of these is additive later, not a rewrite.**

### Event pruning and retention

**The problem**: StateChangeEvent and DetectionEvent grow forever. At one event per second per device, a 100-device fleet produces ~8.6M events/day. LadybugDB will handle this for a while, then get slow.

**The seam that needs to exist now**: a `retention` module with a single entry point, `prune_events(older_than: Duration)`. It does nothing useful yet except:

```rust
pub async fn prune_events(store: Arc<GraphStore>, cutoff: OffsetDateTime) -> Result<PruneStats>
```

Starts as a Cypher `MATCH (e:StateChangeEvent) WHERE e.occurred_at < $cutoff DELETE e`. Runs on a tokio interval. That's phase-5-ready.

**The actual plan** (for Phase 5.5):

1. StateChangeEvent: keep 72 hours hot in the graph. Older events get exported to Parquet files (one per day, device-partitioned) and deleted from the graph.
2. DetectionEvent + Remediation: keep forever in the graph — these are small, high-value, and Phase 5 training data.
3. Interface counter state (`Interface` node properties): always current value only. Historical counter trajectories live in Parquet exports, not the graph.

This gives you a "hot graph" (fast queries, recent state) and a "cold event log" (for ML training, forensics). The split is a natural scale boundary.

**Why Parquet not another time-series DB**: no new runtime dependency. PyArrow reads Parquet from Python directly into pandas. Perfect for ML. And Parquet files on disk are an archive format with zero operational cost.

### Splitting the writer

**The problem today**: single Rust binary, single writer, single graph file. Vertical only.

**The seam that makes horizontal possible**: the subscriber → writer hand-off is already an `mpsc::Sender<TelemetryUpdate>`. If that channel becomes a network queue (NATS subject, Kafka topic, or just a gRPC stream), subscribers can run as separate processes feeding one or more writers.

**The architectural move** (when you actually need it):

```
  [Subscriber 1]─┐
  [Subscriber 2]─┼──►[NATS subject: telemetry.raw]──►[Writer 1]──►[graph shard A]
  [Subscriber 3]─┘                                  [Writer 2]──►[graph shard B]
```

Each subscriber claims a subset of devices via consistent hashing on `target_address`. Writers shard by the same hash so the same device always lands in the same graph shard. Cross-shard queries use a federation layer (or the API aggregates).

**What to do now**: nothing except *keep the `TelemetryUpdate` struct serde-serializable* (which it already is via serde_json). The day you want to cross a process boundary, it's `to_json` and `from_json` on both sides.

**What the existing code needs to NOT do**: never let the subscriber read from the graph. Right now the subscriber doesn't — it only writes to the channel. Keep this pure. A subscriber that reads the graph to decide what to do next is a subscriber that can't be horizontally scaled.

Audit check: `subscriber.rs` has no reference to `GraphStore` — only the channel. ✅ Clean.

### Dynamic device onboarding

**The problem**: `bonsai.toml` lists every target. Adding a device requires a config edit and restart. Doesn't scale beyond a lab.

**The right shape**: introduce a `DeviceRegistry` trait with a single concrete implementation today (file-backed) and a clear migration path to a runtime API.

```rust
#[async_trait]
pub trait DeviceRegistry: Send + Sync {
    async fn list_active(&self) -> Result<Vec<TargetConfig>>;
    async fn subscribe_changes(&self) -> Receiver<RegistryChange>;
}

pub enum RegistryChange {
    Added(TargetConfig),
    Removed(String),        // address
    Updated(TargetConfig),
}
```

Implementations:
- `FileRegistry`: wraps `bonsai.toml`, watches the file for changes (notify crate), emits `RegistryChange` events. Phase 4.5 work — small.
- `ApiRegistry` (Phase 6 / when needed): backed by an internal DB, exposed via gRPC `AddDevice` / `RemoveDevice` / `ListDevices` RPCs. Registry changes broadcast to the main loop.
- `NautobotRegistry` / `NetBoxRegistry` (eventual): sync from external source of truth.

The main loop consumes `RegistryChange` events and spawns / cancels subscriber tasks accordingly. Adding a device is: `AddDevice` RPC → ApiRegistry emits `Added` → main loop spawns subscriber → subscriber runs Capabilities → subscribes → telemetry flows.

**This is the thing that lets a UI in Phase 6 actually do something real.** The UI is a thin gRPC client that calls `AddDevice`.

### Discovery: turning onboarding into "here's an IP, figure it out"

Once `DeviceRegistry` exists, the natural next layer is `Discovery`. Input: an IP address and credentials. Output: vendor, hostname, gNMI capability report, recommended subscription paths, LLDP-discovered neighbors.

This is a gRPC RPC: `DiscoverDevice(address, credentials) → DiscoveryReport`. Under the hood:

1. Connect to gNMI on the address
2. Run Capabilities RPC
3. Return a report: vendor detected, models supported, paths recommended, encoding
4. Optionally: run a short subscription, identify LLDP neighbors, suggest *those* as next devices to onboard

This makes bonsai self-extending. Add one device, discover its neighbors, invite the operator to add them too. This is how commercial tools do it. It's not hard — the pieces are all in the codebase already (`detect_capabilities`, `GnmiSubscriber`). It's a composition.

**Do this in Phase 5.5 alongside the UI.** Not now.

### Things genuinely not worth doing yet

- Distributed graph (ArcadeDB cluster, sharded LadybugDB). Don't. Until you've felt the pain of single-node, the distributed version is premature.
- Kafka/Pulsar for telemetry bus. Don't. The in-process mpsc is fine. NATS is the first step up if needed.
- Multi-tenancy / RBAC. Don't. Scope is still "one operator, one fleet."
- A configuration UI that writes configs to devices. Don't. That was explicitly out of scope in the original project kickoff. Remember: Phase 6 UI is *view-only*.

---

## Part 4 — Phase 5 Architectural Baseline

This is the section that matters most. Phase 5 is where the project either lands the original thesis or drifts into a science project. The baseline below is designed to *prevent the common failure mode* of ML-on-streaming-data: building models that can't be deployed because the feature pipeline was a Jupyter notebook.

### Principle: training and inference share the same feature code

The biggest mistake in applied ML in production systems is having two feature pipelines — one in pandas for training, one in Python dicts for inference. They drift. Models that worked in the notebook fail in production because the features are subtly different.

**The bonsai approach**: `Detector.extract_features()` is already the single feature code path. It ran in Phase 4 producing labelled data in the graph. It will run in Phase 5 producing inference-time features. The `Features` dataclass is the contract. The same Python code produces training rows and inference vectors.

This is why the Phase 4 abstraction was worth the effort. Now we cash it in.

### Phase 5 component architecture

```
┌─────────────────────────────────────────────────────────────────┐
│  TRAINING (offline, batch)                                      │
│                                                                 │
│  [Graph] ──Cypher query──► [Event+Features export (Parquet)]   │
│                                   │                             │
│                                   ▼                             │
│                            [Training notebook / script]         │
│                                   │                             │
│                        ┌──────────┼──────────┐                  │
│                        ▼          ▼          ▼                  │
│                  [Autoencoder]  [LSTM]  [Remediation classifier]│
│                        │          │          │                  │
│                        ▼          ▼          ▼                  │
│                          models/*.pt (artefacts)                │
└─────────────────────────────────────────────────────────────────┘
                                   │
                                   ▼
┌─────────────────────────────────────────────────────────────────┐
│  INFERENCE (online, streaming)                                  │
│                                                                 │
│  StreamEvents ──► FeatureExtractor ──► [MLDetector]             │
│                      (same as P4)           │                   │
│                                              ▼                  │
│                                    [DetectionWriter]            │
│                                              │                  │
│                                              ▼                  │
│                                [RemediationSelector (ML)]       │
│                                              │                  │
│                                              ▼                  │
│                                 [PushRemediation RPC]           │
└─────────────────────────────────────────────────────────────────┘
```

### Layer 1: Training data export

**Module**: `python/bonsai_sdk/training.py`

**Function**: `export_training_set(output_path, since_ns, until_ns)` 

Queries the graph:

```cypher
MATCH (e:DetectionEvent)
WHERE e.fired_at >= $since AND e.fired_at < $until
OPTIONAL MATCH (r:Remediation)-[:RESOLVES]->(e)
RETURN e.rule_id, e.severity, e.features_json, e.fired_at,
       r.action, r.status, r.completed_at
```

Writes Parquet with one row per DetectionEvent:
- **Input features**: everything in `features_json` parsed into columns
- **Anomaly label**: 1 (fired) — the unlabelled "normal" class comes from a separate query
- **Remediation outcome** (if present): action + status as labels for the remediation classifier

For the "normal" class (required for autoencoder/classifier training), query the graph at random non-detection timestamps and extract features for all devices. These are your negative examples.

**Critical property**: the features match exactly what inference will see because they came from the same `extract_features()` code in Phase 4.

### Layer 2: Three models, three distinct jobs

Do not conflate these. Each is small, each is trained separately, each has a distinct role.

**Model A — Per-entity anomaly detector (autoencoder or IsolationForest)**

- **Input**: a feature vector for one entity (device+interface, or device+BGP-peer) at one point in time
- **Output**: anomaly score (0–1)
- **Training data**: all Features from normal periods, one vector per entity per sample
- **Starting point**: `sklearn.ensemble.IsolationForest` — no PyTorch, no GPU, works on a laptop, ships in 20 lines of code. It works. Upgrade to a small PyTorch autoencoder only if IF isn't good enough.
- **Why this matters**: rules fire on thresholds. This fires on *unusualness relative to this entity's history*. An interface that normally has 0 errors suddenly having 10/s is anomalous even though 10/s is below the rule threshold of 100/s.

**Model B — Sequence prediction (LSTM)**

- **Input**: last N time-sliced graph state snapshots for an entity (e.g. last 20 minutes in 60-second bins)
- **Output**: probability of failure in next M minutes
- **Training data**: walk back from each real failure event (DetectionEvent with severity=critical) and collect the preceding N bins; pair with random non-failure windows
- **Starting point**: small LSTM, 1–2 layers, 32 hidden units. PyTorch. Still fits comfortably on a laptop.
- **Why this matters**: this is the "predict before it breaks" capability. Rules can't see the precursor pattern. Sequence models can.

**Model C — Remediation selector (classifier)**

- **Input**: current features + set of candidate remediation actions
- **Output**: which action has historically worked best for this feature pattern
- **Training data**: Remediation nodes in the graph, labelled by their `status` field (success / failed / skipped)
- **Starting point**: gradient-boosted tree (XGBoost or LightGBM). Interpretable via feature importance. Small. Fast to train.
- **Why this matters**: eventually you have multiple remediation options for the same detection. This picks one based on what has worked before.

**Sequencing**: A first, then C, then B. A is the easiest to evaluate (you know your lab's normal behaviour). C needs enough Remediation events to have labels — that accrues naturally as Phase 4 runs. B needs enough *failure* events — inject faults deliberately for weeks to accumulate data.

### Layer 3: Inference integration — the MLDetector

**Module**: `python/bonsai_sdk/ml_detector.py`

```python
class MLDetector(Detector):
    """Drop-in replacement for a RuleDetector using a trained model."""
    
    def __init__(self, rule_id: str, model_path: str, threshold: float,
                 severity: str = "warn", auto_remediate: bool = False):
        self.rule_id = rule_id
        self.severity = severity
        self.auto_remediate = auto_remediate
        self._model = load_model(model_path)   # IsolationForest, LSTM, whatever
        self._threshold = threshold
    
    def extract_features(self, event, client):
        # Reuse the existing rule's extract_features — OR a shared one.
        # This is the whole point of the ABC: feature code is shared across rules and models.
        return ...  # same code path as Phase 4
    
    def detect(self, features):
        vector = features_to_vector(features)
        score = self._model.predict(vector)
        if score > self._threshold:
            return f"ML({self.rule_id}): anomaly score {score:.2f} (threshold {self._threshold})"
        return None
```

Registered in the same rule list as `RuleDetector` instances. The `RuleEngine` doesn't know the difference. This is the payoff for the Phase 4 abstraction.

### Layer 4: Remediation selection with ML

**Module**: `python/bonsai_sdk/ml_remediation.py`

When multiple playbooks are available for a detection, route through the Model C classifier:

```python
class MLRemediationSelector:
    def select(self, detection, candidates: list[str]) -> str | None:
        features = detection.features
        vector  = features_to_vector(features)
        probs   = self._classifier.predict_proba(vector, candidates)
        best    = max(probs, key=probs.get)
        return best if probs[best] > CONFIDENCE_FLOOR else None
```

Integrates into `RemediationExecutor` as the selection step before `_execute`. Fallback to the existing whitelist-based selection when confidence is low. Don't remove the existing path — fall back to it.

### Layer 5: The training loop for the demo

The *Phase 5 demo* needs to show ML catching something rules missed. The scripted flow:

1. Run the rules-based phase 4 system for ~1 week, injecting faults continuously (ContainerLab netem, interface bounces). Accumulate DetectionEvents and Remediations.
2. Export training data (`export_training_set`). Should have hundreds of labelled failures and thousands of normal-period samples.
3. Train Model A locally. Evaluate: can it detect a slow-degrading link before any rule threshold is crossed?
4. Deploy as an `MLDetector` next to the rules. Run both in parallel for a week. Log when each fires.
5. **Demo moment**: start a gradual degradation (netem increasing loss 0.1% / minute). Rules stay silent until loss hits the threshold. ML fires 8 minutes earlier. Show both in the live event stream.

### What Phase 5 does NOT do

- **No reinforcement learning.** You don't have the data volume or the safe exploration environment. RL for remediation selection is a research project; we're doing applied ML.
- **No LLM in the loop for detection.** LLMs in the detection path introduce non-determinism, token costs, and latency. The rule engine and ML detectors are the right layer. LLM integration belongs in a *query layer* (ask bonsai questions in natural language) — that's a separate idea.
- **No retraining pipeline.** One-shot training is fine for the demo. Automated retraining, drift detection, model registry — those are production problems, not Phase 5 problems.
- **No online learning.** Models are trained offline, loaded at inference time, replaced by restart.

### Phase 5 success criteria

- [ ] `training.py` exports Parquet from the graph reliably
- [ ] `export_training_set` produces both positive (fired detections) and negative (sampled normal periods) examples
- [ ] IsolationForest-based `MLDetector` runs in the `RuleEngine` alongside rules, shares the same `extract_features`
- [ ] Demo shows the ML detector firing on a slow degradation before any rule does
- [ ] Remediation classifier picks different actions for different feature patterns, and outcomes are logged
- [ ] One blog post written about the Phase 4→5 transition and what changed

---

## Part 5 — Immediate Actions (before starting Phase 5 coding)

In order, each small enough to complete in a focused session:

1. **Update README.md** — current status says Phase 2; it's Phase 4 complete. Fix the "Temporal by design" claim to match reality.
2. **Write DECISIONS.md entries** for: (a) the Junos classifier dead code (decision: keep with TODO or remove), (b) the retention/pruning deferral, (c) the schema migration deferral.
3. **Add `TRIGGERED_BY` edge from DetectionEvent to StateChangeEvent** — small graph.rs change, saves you Phase 5 pain when tracing back from ML detection to input.
4. **Add a `/metrics` endpoint** (Prometheus format) exposing: telemetry updates/sec per device, graph write latency p50/p99, event broadcast lag, subscriber reconnect count. Ten lines of `metrics` crate. This is your observability of bonsai itself.
5. **Write one integration test** that deploys the Phase 4 topology, waits for Capabilities, injects a BGP flap, and asserts a DetectionEvent exists in the graph within N seconds. Use pytest + subprocess + clab. One test is enough — it's a smoke test.
6. **Scaffold the `retention` module** (empty `prune_events` function wired to a tokio interval, no-op in config). Seam exists, no behaviour change yet.
7. **Scaffold the `DeviceRegistry` trait** with `FileRegistry` wrapping current config. Main loop consumes `RegistryChange` events even though today the only change is startup. Seam exists.

These seven actions take the codebase from "Phase 4 done" to "Phase 5-ready" without shipping any Phase 5 feature. They also tighten things that were already slightly loose.

---

## Part 6 — Reminders to Keep in the Room

When Phase 5 work is happening and the ML starts looking exciting, these are the things that kill the project if you forget them. Paste them into session start prompts if needed.

**The goal was never a product.** It was systems understanding and a working proof. Don't optimise for anything other than "I built this, I understand how every layer works, I can show it working, I learned what the paper was pointing at."

**No scope expansion without an ADR.** If something new is being added, it gets an entry in DECISIONS.md before the code is written. The discipline is what keeps bonsai bonsai and not a 50-module sprawl.

**The UI, when it happens, is view-only.** Not a config tool. Not a dashboard product. A way to *see* what the system is doing. If someone (including you) starts asking for admin features, refuse.

**The ML models are not the point.** The point is the full loop working. A mediocre IsolationForest that you trained on your own graph data, that fires in the same pipeline as rules, using the same features, is worth more than a state-of-the-art model that lives in a notebook.

**Vendor-neutrality is already paying off.** Don't compromise it in Phase 5. Feature extraction must be vendor-agnostic. Models train on normalised features. The `Features` dataclass is the contract.

**Scale seams, not scale implementations.** Every pattern in Part 3 is a *seam* that makes future scaling possible without rewrites. Do not implement the scaling itself. Leave the entry points obvious.

---

*Version 1.0 — written at Phase 4 complete, before Phase 5 coding begins. Update in place as the Phase 5 plan evolves.*
