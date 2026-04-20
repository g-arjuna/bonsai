# BONSAI — Consolidated Backlog v1.0

> Single source of truth for outstanding work. Supersedes the planning in `PHASE5_REVIEW_AND_DESIGN.md` and its addendum where they diverge. Those documents remain as historical design records; this one is the living priority list.
>
> Authored after a full review of the v3 repo (Phase 5.0/5.1/5.2 complete, Phase 6.0 UI complete, Phase 6.1 pending, Codex harvest round 1 complete with 9 playbooks).

---

## How This Backlog Is Structured

Five tiers, in strict priority order. Don't jump tiers unless the prior tier is complete or a blocking dependency.

- **Tier 0 — Bleeders**: things that are currently broken or silently wrong. Fix first; they undermine everything above them.
- **Tier 1 — Foundational architectural seams**: the three big architectural asks (distributed collection, dynamic onboarding, TSDB/event bus). Must land before the system grows further.
- **Tier 2 — ML correctness and catalogue integration**: making what's already built actually work end-to-end.
- **Tier 3 — Lab and fault-injection for training data**: the forcing function that produces signal for the ML models to learn anything useful.
- **Tier 4 — Planned extensions and polish**: NL query layer, Phase 6.1 onboarding UI, observability upgrades, temporal queries.

Each item: **what, why, where, done-when**. No prose paragraphs. No "this would be nice" entries.

---

# TIER 0 — Bleeders

These are quietly wrong today. Fix before building anything on top.

## T0-1 — Playbook library directory split

**What**: Codex produced 9 playbook YAML files at `/playbooks/library/`. The `PlaybookCatalog` loader in `python/bonsai_sdk/playbooks/catalog.py` points at `python/bonsai_sdk/playbooks/library/`. At runtime, none of the 9 Codex playbooks are visible to the executor.

**Why it matters**: the remediation path silently falls back to "no playbook selected" for every detection. `RemediationExecutor` writes `status=skipped` with reason "no playbook for rule=... vendor=..." — exactly the failure mode that's hardest to notice because it looks like intentional conservatism.

**Where**:
- `python/bonsai_sdk/playbooks/catalog.py:9` — `LIBRARY_DIR` constant
- `/playbooks/library/` — the 9 real playbooks
- `/python/bonsai_sdk/playbooks/library/` — only the migration entry

**Done when**:
- Single canonical library location (recommended: `/playbooks/library/` at repo root — matches `SOURCE_STRATEGY.md` and the harvest output)
- `PlaybookCatalog.__init__` resolves the canonical path via `Path(__file__).parents[N] / "playbooks" / "library"` or via an explicit config key
- The duplicate under `python/bonsai_sdk/playbooks/library/` is deleted
- `RemediationExecutor` logs the catalog path and playbook count at startup so this can never silently drift again
- Integration test asserts all 9 YAMLs load

## T0-2 — Verification field schema mismatch

**What**: Codex YAMLs use `verification.expected_graph_state` as the Cypher key. `PlaybookExecutor.verify()` at `python/bonsai_sdk/playbooks/executor.py:69` reads `vfy["cypher"]`. Result: `vfy.get("cypher")` returns None, the check short-circuits to `return True`, and every auto-remediation reports success without ever running the verification query.

**Why it matters**: success/failure signal for Model C training is corrupted. Every `Remediation.status` in the graph reads `success` regardless of whether the device recovered. Training the remediation classifier on this data teaches it that every action always works.

**Where**: `python/bonsai_sdk/playbooks/executor.py:68-91`

**Done when**:
- Decide on the canonical field name — I recommend `expected_graph_state` (matches the harvest prompt and every Codex YAML)
- Update `executor.py` to read that field
- Migrate the single SDK-internal playbook (`python/bonsai_sdk/playbooks/library/bgp_session_down.yaml`) to use the canonical name before deleting the duplicate per T0-1
- Update `CODEX_HARVEST_RESUME_PROMPT.md` to lock the field name explicitly so future harvest rounds can't drift
- A unit test exercises verify() with a mocked `client.query` returning a truthy result and asserts the playbook is marked verified; a second test uses a missing/empty response and asserts verified=False

## T0-3 — Temporal-by-design claim is still a lie

**What**: `CLAUDE.md:34` and `README.md` still say "DIY bitemporal (valid_from/valid_to on all nodes/edges)". The graph schema in `src/graph.rs` only has `updated_at`. Nothing carries `valid_from`/`valid_to`. "What did the graph look like 5 minutes ago" still cannot be answered.

**Why it matters**: this is the second review cycle this has been flagged. It's drifted twice. Now it matters for real because NL query (Phase 5.5 candidate) will start asking temporal questions the schema cannot answer.

**Where**: `CLAUDE.md:34`, `README.md` principles section

**Done when** (pick one):
- **Option A (cheap, honest)**: update both files to say "Append-only event log via StateChangeEvent; full bitemporal deferred to Phase 5.5"
- **Option B (do the work)**: add `valid_from`, `valid_to` to Device/Interface/BgpNeighbor/LldpNeighbor; on update, tombstone the old row (`valid_to = now()`) and insert a new row with `valid_from = now(), valid_to = NULL`; add a `GetGraphAsOf(timestamp)` RPC; index `valid_from` and `valid_to`
- My recommendation: **A now, B in Tier 4**. Temporal queries are not on any active feature; don't pay for them yet. But stop claiming you have them.

## T0-4 — `_empty_features` pollutes numeric fields with empty strings

**What**: `python/bonsai_sdk/training.py:139-150` builds a dict where categorical fields default to `""`. Specific overrides set the numeric fields to zero, but the fallback `{f.name: "" for f in fields(Features)}` runs first — any numeric field not in the override list gets `""`. Parquet writes mixed `object` dtype columns. `features_to_vector` then can't do `float(features.peer_count_total)` if the row came from the normal-window path with a miss.

**Why it matters**: silently trains on corrupt data. Anomaly scores become unreliable without any obvious error.

**Where**: `python/bonsai_sdk/training.py:139-150`

**Done when**:
- `_empty_features` explicitly types each field based on its dataclass type annotation, using `int` → 0, `str` → "", `dict` → "{}", etc.
- Add a unit test that calls `_empty_features` and asserts no numeric field is `""`
- Add a post-export validation step that reads the Parquet back and asserts column dtypes match expectations

## T0-5 — Vendor column in remediation training set is wrong

**What**: `python/bonsai_sdk/training.py:201` sets `"vendor": features.get("device_address", "")`. The vendor column is filled with IP addresses, not the actual vendor string. Model C cannot learn vendor-specific remediation preferences because the vendor feature has no signal.

**Why it matters**: Model C's whole purpose is picking the right playbook for the vendor. If it can't see the vendor, it's guessing.

**Where**: `python/bonsai_sdk/training.py:183-212`

**Done when**:
- Cypher query joins to Device: `MATCH (r:Remediation)-[:RESOLVES]->(e:DetectionEvent)<-[:TRIGGERED]-(d:Device)` and returns `d.vendor`
- `vendor` column is populated from the Device node, not from features
- Add an assertion in `train_remediation.py` that `vendor` values are one of the known vendor labels; fail loudly on anomalies
- A unit test exercises the export with a seeded graph and validates the vendor join

## T0-6 — MLDetector feature extraction diverges from RuleDetector

**What**: `python/bonsai_sdk/ml_detector.py:109-146` re-implements `extract_features` instead of reusing the rule detectors' extraction. Subtle differences: `state_change_event_id` is copied from the event, but rules don't do this; peer state lookup happens without the rule's `_HARD_DOWN_STATES` guard, so MLDetector extracts features even for transient BGP states that rules correctly ignore.

**Why it matters**: the whole point of the Detector ABC was shared feature code. If MLDetector extracts differently at training vs the rules extracted at detection time, training/inference skew returns through the back door.

**Where**: `python/bonsai_sdk/ml_detector.py:109-146`, `python/bonsai_sdk/rules/bgp.py::BgpSessionDown.extract_features`

**Done when**:
- `extract_features` in MLDetector is replaced with composition — a shared module-level `extract_features_for_event(event, client)` function that both RuleDetector subclasses and MLDetector call
- Rules subclass this and apply their own gating (event_type filter, state transition filter)
- MLDetector does not gate at all — it extracts features for every event and lets the model score
- The shared extractor is tested with representative events from all event types in the fixture suite

---

# TIER 1 — Foundational Architectural Seams

The three big asks from the latest design conversation. Must land as scaffolds now even if full implementations come later. Order matters: TSDB/event-bus first because it affects the shape of everything else.

## T1-1 — Telemetry event bus with retention layer (your concern (c))

**Summary**: today, gNMI updates flow `subscriber → mpsc::channel → graph::write()` directly. Every update incurs a LadybugDB write under the single `write_lock`. This is correct for a 3-node lab at <100 updates/second. At 1,000 updates/sec × 50 devices, it becomes the system's bottleneck and the graph becomes a hot-state-plus-history monster.

**The architectural move**: separate three concerns that are currently one.

1. **Wire layer** (unchanged): gNMI Subscribe, protobuf decode, classification → `TelemetryUpdate` struct
2. **NEW: Event bus layer**: an append-only log of every `TelemetryUpdate`. Local tokio `broadcast::channel` for fan-out, plus a segmented file-backed log for durability. The graph writer and the TSDB archiver both consume from this bus.
3. **Hot graph layer** (existing): writes only *state-changing* updates (new values, transitions). Counter samples are written debounced (max once per 10s per interface) because the counter value isn't meaningful per-sample — rates are.
4. **NEW: Cold archive layer**: Parquet files segmented by hour, partitioned by device. The bus archiver drains to Parquet at a 10-second cadence. Compression happens for free via Parquet's columnar encoding.

**Why this is right**:
- The graph becomes a *current state + short history* store (bounded size)
- The cold archive is the training-data source, the forensics store, and the long-tail analytics substrate — all in one file format
- Consumers (future ML jobs, NL queries, UI analytics) read Parquet, never the hot graph
- Pruning the graph gets aggressive; pruning the archive stays conservative

**Sub-tasks in order**:

**T1-1a — Introduce `EventBus` trait**. In `src/event_bus.rs`: a trait with `publish(TelemetryUpdate)`, `subscribe() -> Receiver<TelemetryUpdate>`. First implementation is `InProcessBus` wrapping a `tokio::sync::broadcast::Sender` with a configurable capacity. The subscriber no longer writes directly to the graph channel — it publishes to the bus.

**T1-1b — Move graph writer to bus consumer pattern**. The current `graph_writer` tokio task in `main.rs:52-60` becomes a bus subscriber. Pull, classify, write. Same behaviour, different source.

**T1-1c — Add the cold archive consumer**. `src/archive.rs`: a tokio task that subscribes to the bus, buffers `TelemetryUpdate`s into Arrow record batches, flushes to Parquet every 10 seconds to `archive/YYYY/MM/DD/HH/<device>.parquet`. Dependencies: `arrow`, `parquet` Rust crates.

**T1-1d — Debounce counter writes**. In the graph consumer, classify updates that carry *counter* values separately from *state transition* values. Counter writes are rate-limited to once per 10 seconds per `(device, interface)` pair via an in-memory last-written timestamp. State transitions always write. This reduces graph writes by an order of magnitude without losing signal.

**T1-1e — Retention actually prunes Interface history in graph**. Today `retention.rs:18` only deletes `StateChangeEvent` older than cutoff. Extend it to truncate Interface rows older than the retention window if bitemporal fields are added later (paired with T0-3 Option B), or to simply bound the StateChangeEvent table to the last 24 hours as a stronger default.

**T1-1f — Config surface**. `[event_bus]` section in `bonsai.toml`: `capacity`, `archive_enabled`, `archive_path`, `archive_flush_interval_seconds`. `[retention]` extended with `max_state_change_events` as an alternative to time-based cutoff.

**Done when**: a 2-hour run with the Phase 4 topology produces a populated archive directory with readable Parquet files; the graph's `StateChangeEvent` count stays bounded; graph-write latency drops measurably under a `soak_test.py` equivalent that spams interface counter updates.

**Explicit anti-goals**: **no Kafka, no Redpanda, no NATS yet**. The in-process bus is the right size for single-host. The trait lets you swap later. Do not introduce a network-backed broker until you have felt single-host pain.

## T1-2 — Distributed collector architecture (your concern (a))

**Summary**: today, bonsai is a single binary that connects to every device. Two problems: (1) the bonsai-to-device gNMI path can fail and updates are lost for the outage window; (2) in production, telemetry should be collected close to the source for compression and resilience.

**The architectural move**: one binary, two modes selected by config — `collector` (edge, co-located with devices) and `core` (receives collector feeds, runs graph/rules/ML).

**Design**:

```
┌──────────────────────────┐   ┌──────────────────────────┐
│ Collector Node A         │   │ Collector Node B         │
│ ├─ gNMI subscriptions    │   │ ├─ gNMI subscriptions    │
│ ├─ Local buffer (disk)   │   │ ├─ Local buffer (disk)   │
│ │  during core outages   │   │ │  during core outages   │
│ └─ gRPC stream → core    │   │ └─ gRPC stream → core    │
└──────────┬───────────────┘   └──────────┬───────────────┘
           │                              │
           │  zstd-compressed             │
           │  TelemetryUpdate stream      │
           │                              │
           └──────────┬───────────────────┘
                      │
                      ▼
           ┌──────────────────────────┐
           │ Core Node                │
           │ ├─ EventBus              │
           │ ├─ Graph writer          │
           │ ├─ Rules / ML            │
           │ ├─ gRPC API              │
           │ └─ UI                    │
           └──────────────────────────┘
```

**Key properties**:

- **Same binary, different mode**: `bonsai serve --mode=collector` vs `bonsai serve --mode=core` vs `bonsai serve --mode=all` (default; everything in one process, the current behaviour).
- **Protocol between collector and core**: extend `proto/bonsai_service.proto` with a new `TelemetryIngest` service that streams `TelemetryUpdate` protobufs. Collector client, core server.
- **Compression**: gRPC-level gzip or zstd on the `TelemetryIngest` stream. Native tonic support via `accept_compressed` / `send_compressed`.
- **Disconnection resilience**: collector buffers unsent updates to a local disk queue (simple segment files, not a DB) when the core is unreachable. Drains on reconnect.
- **Authentication**: mTLS between collector and core. The core maintains a list of authorised collector certificates. Credentials for gNMI devices live on the collector side (same discipline as today).
- **Core is stateless about collectors** in v1: a core accepts streams from N collectors. No consensus, no collector-to-collector coordination. Each device is owned by exactly one collector (static config).

**Sub-tasks in order**:

**T1-2a — Introduce mode selection**. Add `--mode` CLI flag, `[mode]` config block. Default `all`. Wire up a `RuntimeMode` enum that gates which subsystems start.

**T1-2b — Add `TelemetryIngest` gRPC service**. Core-side: accepts a client-streaming RPC, publishes each received `TelemetryUpdate` to the local `EventBus`. Collector-side: a new `UpstreamPublisher` that consumes from the local `EventBus` (!) and forwards to core — meaning on the collector, the subscriber publishes to local bus, the publisher consumes from local bus and forwards upstream. This symmetric-bus model is what makes the three modes work cleanly.

**T1-2c — Disk-backed local queue for collector**. `sled` or a simple segment-file format. When the upstream connection fails, the publisher writes to disk and the queue is drained when reconnection succeeds. Cap the queue size; on overflow, drop oldest (document this in DECISIONS.md).

**T1-2d — gRPC compression**. Enable zstd in tonic client/server config for the `TelemetryIngest` service.

**T1-2e — mTLS between collector and core**. Reuse the TLS plumbing already present in `gnmi_set.rs` and `subscriber.rs`. The certificate verification is straightforward; the certificate issuance workflow is a manual lab process (openssl + a simple CA), documented in `docs/distributed_setup.md`.

**T1-2f — Topology for multi-collector lab**. Add `lab/distributed/` with a ContainerLab topology that includes two collector containers and one core container, with SRL devices distributed across them. Proves the path end-to-end.

**Done when**: a two-collector, one-core setup ingests telemetry; killing the core for 60 seconds and bringing it back shows all buffered updates replayed and the graph fully consistent with what would have been ingested in single-node mode.

**Explicit anti-goals**: no HA for core. No collector clustering. No distributed graph. Single-host core with N collectors is the target — that's already a meaningful architectural upgrade.

## T1-3 — Dynamic device onboarding (your concern (b))

**Summary**: today, `bonsai.toml` lists every target. To add a device you edit the file and restart. There is no way to suggest subscription paths based on device type, and no way to discover capabilities from the UI.

**The architectural move**: three capabilities, each adding value independently.

1. **`DeviceRegistry` that actually mutates at runtime** (replaces today's FileRegistry-that-never-emits)
2. **`DiscoverDevice` RPC** that connects, runs Capabilities, and returns what paths make sense
3. **Path profile templates** that suggest subscription paths based on vendor + device role (leaf / spine / PE / route-reflector)

**Sub-tasks in order**:

**T1-3a — `ApiRegistry` implementation**. New struct implementing `DeviceRegistry`. Backed by an internal SQLite (or even just a JSON file) that survives restarts. gRPC RPCs: `AddDevice`, `RemoveDevice`, `UpdateDevice`, `ListDevices`. When a device is added, emits `RegistryChange::Added` on the `subscribe_changes` channel. The main loop (today in `main.rs:155`) responds by spawning a subscriber task. When removed, the task is cancelled.

**T1-3b — Subscriber lifecycle management**. Refactor `main.rs` so subscribers are created from `RegistryChange` events, not from a static loop at startup. `FileRegistry` emits `Added` events for each target at startup, preserving today's behaviour for free.

**T1-3c — `DiscoverDevice` RPC**. Takes `address`, credentials (env var names, never plaintext). Connects, runs gNMI Capabilities, returns a `DiscoveryReport`:
```
DiscoveryReport {
  vendor_detected: string
  models_advertised: repeated string
  gnmi_encoding: string
  recommended_paths: repeated PathProfile
  warnings: repeated string
}
PathProfile {
  profile_name: string   // "dc_leaf_minimal", "sp_pe_full", etc.
  paths: repeated SubscriptionPath
  rationale: string
}
```

**T1-3d — Path profile templates**. YAML in `config/path_profiles/`:
- `dc_leaf_minimal.yaml` — interfaces, BGP, LLDP
- `dc_spine_standard.yaml` — same as leaf plus ECMP stats
- `sp_pe_full.yaml` — interfaces, BGP, ISIS, LDP, MPLS, segment routing
- `sp_p_core.yaml` — interfaces, ISIS, LDP, segment routing

Each profile lists path specs that get filtered by the device's actual capabilities. Example: `sp_pe_full` asks for `openconfig-mpls` — if the device doesn't advertise it, that path is dropped from the recommendation and a warning is emitted.

**T1-3e — Device role and geography metadata**. Add `role` (leaf / spine / pe / p / rr) and `site` (free-form string) to `TargetConfig`. These drive profile selection during onboarding: if role=leaf, default profile is `dc_leaf_minimal`.

**T1-3f — UI onboarding wizard** (deferred to T4-1 but designed here). Three steps: (1) address + credentials-env-var-name, (2) discover result displayed with path checkboxes, (3) confirm → registry mutation.

**T1-3g — Runtime path verification**. After adding a device, bonsai subscribes and within 30 seconds reports back which of the recommended paths produced actual updates. Paths that return no data in that window get flagged in the UI as `subscribed_but_silent`. This is the feedback loop that makes the onboarding honest — the UI tells the operator what's actually working.

**Done when**:
- `bonsai device add 10.1.1.1 --env-user SRL_USER --env-pass SRL_PASS --role leaf` (CLI command against the gRPC API) discovers and subscribes without restart
- The same operation via the UI wizard produces the same result
- A device removed via `bonsai device remove 10.1.1.1` stops its subscriber and its Device node is tombstoned (not deleted) in the graph

**Explicit anti-goals**: no multi-tenancy on the registry, no RBAC. Anyone with access to the gRPC API can add/remove devices. Matches the single-operator scope.

---

# TIER 2 — ML Correctness and Catalogue Integration

Now that Tier 0 bleeders and Tier 1 seams are in, make the existing ML pipeline work end-to-end against a real catalogue.

## T2-1 — Catalogue integration review

**What**: audit every Codex-produced playbook YAML for:
- preconditions that reference `Features` fields that don't exist (e.g., `features.if_name` exists only for interface events, not BGP)
- path placeholders that don't match `Features` fields (`{peer_address}` is fine, `{neighbor_ip}` is not)
- verification queries that reference node labels not in the current schema (`:BfdSession` doesn't exist today)

**Why**: Codex did the prose right but the engine can't execute what it can't resolve. Each YAML needs a 5-minute review against the actual code.

**Where**: `/playbooks/library/*.yaml` (9 files)

**Done when**:
- Every YAML passes a `catalog.validate()` method that checks: rule_id exists in rules code; every precondition eval'd against an empty Features object returns a bool (not KeyError); every path placeholder matches a Features field; verification Cypher references nodes/edges that exist in the schema
- A CI-style script (`scripts/validate_playbooks.py`) runs this and returns non-zero on any failure
- The BFD playbook YAML is either kept and flagged `requires_schema_extension: BfdSession` (with a follow-up backlog item T2-4), or removed until the schema catches up

## T2-2 — ML feature vector drift protection

**What**: today, `features_to_vector()` in `ml_detector.py:60` hardcodes the field order. If someone adds a field to `Features`, training data written yesterday and inference today go out of sync silently. Same issue as T0-4 but one level up.

**Why**: this is the fundamental ML-in-production failure mode that the Detector ABC was supposed to prevent. Right now the ABC is only half the protection.

**Done when**:
- Every trained model artifact is serialised as a dict: `{"model": ..., "feature_schema_version": N, "feature_names": [...], "label_encoder": ...}`
- `load_model()` returns a bundle; `MLDetector` asserts `feature_schema_version` matches its compiled-in version
- When the version mismatches, MLDetector logs a warning and refuses to score — falls back to the downstream rules detector
- `features_to_vector` takes the expected feature_names list as an argument and produces the vector in that order, failing loudly if any expected name is missing

## T2-3 — Training data validity check

**What**: `train_anomaly.py` and `train_remediation.py` currently load Parquet and train. No validation of data shape, class balance, or value ranges before training. Easy to train a useless model on 3 rows of all-zeros.

**Done when**:
- Both training scripts print a data summary before training: row count, class balance, feature value ranges, null counts per column
- Each script fails loudly if: row count < 100; any class has < 10 examples; any numeric feature column is more than 50% null; any categorical column has only one unique value
- The summary is also written as a JSON file next to the model artifact for provenance

## T2-4 — BFD schema extension (optional, conditional on T2-1 outcome)

**What**: Codex produced a BFD playbook YAML. The schema doesn't model BFD sessions. Either extend the schema or remove the YAML.

**Done when** (if extending): `BfdSession` node with `device_address, local_address, remote_address, session_state, detection_multiplier, updated_at`; `HAS_BFD_SESSION` edge from Device; classification in `telemetry.rs` for BFD paths; equivalent write function in `graph.rs`.

**My take**: do this only if the immediate next round of Codex work targets BFD detection rules. Otherwise delete the YAML.

---

# TIER 3 — Lab and Fault Injection for Training Data

The ML models are trained today on a handful of events. None of them will be useful until the lab generates weeks of varied events. This is the forcing function.

## T3-1 — Lab expansion for variety

**What**: today's lab is `lab/fast-iteration/bonsai-phase4.clab.yml` with 3 SRL + 1 XRd. That's enough for one failure pattern at a time. For training, need more variety without paying for more RAM.

**Done when**:
- A `lab/training/` subdirectory with multiple topologies that bonsai can switch between: `dc_leaf_spine_4.clab.yml` (1 spine, 3 leaf), `sp_mini.clab.yml` (3 SRL + 2 XRd with MPLS/SR), and a `mixed.clab.yml` with all four vendors once cRPD returns
- Each topology has a matching startup script that deploys it and configures known-good BGP + IGP + MPLS state
- A `lab/training/README.md` documents when to use which topology

## T3-2 — Automated fault injection harness

**What**: `inject_fault.py` exists but is a one-shot tool. For accumulating training data, need sustained, varied, reproducible fault generation over days.

**The design**: a `scripts/chaos_runner.py` that:
- Reads a YAML fault plan (`chaos_plans/*.yaml`)
- Runs for a configured duration (default: 24 hours)
- Every N minutes, picks a fault type and target, injects it, waits, heals it
- Logs a ground-truth CSV of every fault injected with timestamps (bonsai's detections can be compared against this ground truth to compute true-positive / false-negative rates)

**Example plan** (`chaos_plans/baseline_mix.yaml`):
```yaml
duration_hours: 24
injection_interval_seconds: [60, 300]   # random within range
faults:
  - type: interface_shut
    targets: [srl-leaf1, srl-leaf2]
    interfaces: [e1-1, e1-2]
    weight: 3
    healing_delay_seconds: [20, 60]
  - type: netem_loss
    targets: [srl-spine1]
    interfaces: [e1-1]
    loss_percent: [1, 5, 15]
    weight: 2
    healing_delay_seconds: [120, 300]
  - type: bgp_session_down
    targets: [srl-leaf1]
    peer_addresses: [10.0.0.1]
    weight: 1
    healing_delay_seconds: [30, 90]
```

**Done when**:
- `scripts/chaos_runner.py` runs a 24-hour plan end-to-end
- Ground-truth CSV written to `chaos_runs/<timestamp>/injections.csv`
- A companion `scripts/evaluate_detections.py` joins the ground truth against `DetectionEvent` rows and reports true-positive rate, false-negative rate, detection latency per fault type
- The current `inject_fault.py` is refactored to be the single-shot entry point the chaos runner calls per injection

## T3-3 — Training data accumulation discipline

**What**: define the data volume target before ML training is meaningful.

**Minimum bars**:
- **Model A (anomaly)**: at least 1000 normal samples and 100 real DetectionEvents across at least 5 distinct fault types
- **Model C (remediation)**: at least 50 Remediation events per (rule_id, vendor) pair with actual outcome labels (success/failed). This is the demanding one — today you have one remediable rule+vendor (SRL BGP), so you need 50 SRL BGP bounces with varied outcomes
- **Model B (LSTM failure prediction)**: deferred — needs weeks, not days, of varied failure patterns

**Done when**: a `scripts/check_training_readiness.py` queries the graph and reports the current count against each bar; ML training scripts (T2-3) refuse to run below the bar by default, with a `--force` flag to override for experimentation.

## T3-4 — Synthetic degradation scenarios

**What**: chaos injection is binary (fault on/off). Real-world failures are usually gradual degradation. Autoencoder-style anomaly detection is most valuable on gradual degradation because rules are blind to sub-threshold changes.

**Done when**:
- A new fault type `gradual_degradation` in the chaos runner: incrementing netem loss/delay over a configurable window (e.g., 0% → 15% loss over 20 minutes)
- Demoable scenario: gradual_degradation on a BGP session's underlying link, MLDetector fires 5 minutes before the session actually goes down, rules stay silent until the session collapses
- This scenario becomes the flagship demo for Phase 5

---

# TIER 4 — Planned Extensions and Polish

Everything else. Do not touch until Tier 0–3 are truly done.

## T4-1 — Phase 6.1 onboarding UI

Planned per the CLAUDE.md roadmap. Builds on T1-3 (dynamic onboarding is already a gRPC API by the time the UI is added). Svelte component + wizard form. 

## T4-2 — Natural-language query layer

Per the PHASE5_ADDENDUM document. Unlocks NL questions against the graph. Blocked on T0-3 Option B if the questions include "as of time T"; otherwise unblocked. Low code volume (~200 lines Python), high demo value.

## T4-3 — Full bitemporal schema

Per T0-3 Option B. Defer until a concrete query pattern actually needs it. NL queries that involve past state are a plausible trigger.

## T4-4 — Metrics expansion

Today: Prometheus exporter at `[::1]:9090`, basic counters. Add:
- Histogram of graph-write latency (already scaffolded but verify it's populated)
- Event-bus depth gauge
- Archive lag gauge (oldest unarchived event age)
- Subscriber reconnect frequency per device
- Rule-firing rate per `rule_id`

## T4-5 — Schema migration path

Today: adding a column to an existing node table breaks deployment on existing databases. Eventually needed: a `schema_version` table in the graph, numbered migration scripts, a migrator that runs on startup.

**My take**: don't do this until you hit the pain yourself. Premature.

## T4-6 — LLM-assisted playbook suggestion (Layer 3)

Per the PHASE5_ADDENDUM three-layer remediation model. After the catalogue has 15+ hand-written/validated entries, the `SuggestRemediation` gRPC RPC is worth building. Human approval gate is mandatory. 

## T4-7 — Grafeo migration readiness

Per DECISIONS.md, LadybugDB has a 60-day-no-release trigger for evaluation of Grafeo. Check release cadence monthly. If LadybugDB goes quiet for a full 60 days, allocate a 3-day evaluation spike for Grafeo before writing more graph code.

---

# Recommended Execution Order

Not the same as the tier order — this is the tactical sequence that respects dependencies and reasoning about risk:

1. **T0-1, T0-2 together** (1 session): fix the catalog path and verification field name. Without this, all Phase 5 remediation is broken.
2. **T0-3 Option A, T0-4, T0-5, T0-6** (1–2 sessions): drift and correctness fixes. Cheap, unblocking.
3. **T2-1** (1 session): audit the 9 Codex playbooks against the fixed catalogue. Outcome is a known-good baseline.
4. **T1-1 sub-tasks a, b, d, e** (2–3 sessions): EventBus + debouncing + retention that actually prunes. Graph becomes bounded.
5. **T3-2, T3-3** (1–2 sessions): automated chaos runner and training readiness check. Start running 24h loops — you'll be accumulating data while doing everything else.
6. **T1-1 sub-tasks c, f** (1 session): archive to Parquet. Now you have cold storage for training data that doesn't grow the graph.
7. **T2-2, T2-3** (1 session): ML pipeline robustness. Version-locked feature contract, data validity checks.
8. **T1-3** (2–3 sessions): dynamic onboarding. Now the lab can grow without restarts.
9. **T3-1, T3-4** (2 sessions): expanded lab, gradual degradation scenarios.
10. **T1-2** (3–4 sessions): distributed collector architecture. Build once Tier 0–2 is solid.
11. **T4-2** (1 session): NL query. The reward for doing the boring work first.
12. **T4-1** (2–3 sessions): onboarding UI.
13. Everything else in T4 as needed.

---

# Guardrails — Don't Break These

Carried forward from the original kickoff and still binding:

- **No SNMP, no NETCONF. Ever.** gNMI only.
- **No tokio alternative. Ever.**
- **No Kubernetes in v0.x.** Single-host core, collectors as separate binaries on separate hosts is fine, but no orchestration.
- **Every non-trivial decision gets an ADR in DECISIONS.md.** If Tier 1 work produces >10 decisions, that's normal and correct.
- **No fifth vendor until the four current targets work vendor-neutrally across the whole remediation stack.** cRPD still deferred.
- **Phase 6 UI is view-plus-onboarding, never config-writing-to-devices.** Config goes through playbooks executed via `PushRemediation`. No UI field that sends arbitrary gNMI Set to arbitrary paths.
- **Credentials never leave Rust.** No Python process ever holds a password. Env var names only.

---

# What This Backlog Deliberately Does Not Include

To stay honest about scope:

- No auth/RBAC work. The gRPC API is still trusting.
- No multi-tenant graph. One bonsai instance, one network under management.
- No production-grade HA. Core restarts lose nothing because of T1-1e archiving, but there's no failover.
- No attempt at a universal vendor library. The Codex catalogue is the catalogue; it grows organically.
- No attempt to replace Nautobot/NetBox. The `DeviceRegistry` trait has a stub for `NautobotRegistry` in the design but it's nowhere in the backlog — that's a Phase 7+ consideration if it ever happens.
- No streaming ML inference at scale. The rule engine is one process; MLDetector is in-process. Horizontal scaling of inference is explicitly deferred.

---

*Version 1.0 — produced after reviewing v3 of the repository (Phase 5.0/5.1/5.2 + Phase 6.0 complete, 9 Codex playbooks harvested, architectural expansion pending). This document is intended to be paste-able at the top of a Claude Code session as the single source of priority truth.*
