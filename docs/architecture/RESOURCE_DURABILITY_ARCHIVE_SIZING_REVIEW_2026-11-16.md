# Core/Collector Resource, Durability, Archive & Enterprise Sizing Review

Source-audited at `origin/main` 506595b (+ WP6.5A branch `fe4071e`
where noted), 2026-11-16. Static review + WP6.5A measured data where
available. Design/backlog only — no implementation authorized.

## F1. Flow cardinality and semantics (AppFlow)

Schema (`src/graph/mod.rs:1344`): `AppFlow{id, exporter_address,
src_address, dst_address, dst_port, src_port DEFAULT 0, protocol,
bytes_per_sec, packets_per_sec, updated_at}` — keyed by exporter +
five-tuple (id is derived per record).

Confirmed/likely findings:

- **New-flow cardinality:** every distinct five-tuple+exporter creates a
  node; no idle expiry/TTL eviction was found — nodes persist until
  retention. Session lifecycle (start/end/teardown) is not modeled;
  `updated_at` is a latest-upsert timestamp, so historical rates are
  lost (only latest `bytes_per_sec`/`packets_per_sec` survive).
- **Out-of-order updates:** last-writer-wins on the same key — an older
  flow update arriving late overwrites newer rates. No sequence/epoch
  guard observed.
- **Missing fields:** `src_port` defaults 0 (ICMP/unknown); protocol is
  a STRING — no numeric validation found in schema.
- **IPFIX absolute timestamps / rate fallback:** not verified in this
  pass — the AppFlow writer path (NetFlow/IPFIX decoder) needs a
  dedicated trace; flagged as unverified rather than assumed correct.
- **Ignored egress interface:** no `egress_interface`/`input/output
  interface` fields in schema — interface context is lost at the AppFlow
  boundary (topology correlation must go through Device/CARRIES_FLOW).
- **NAT/firewallEvent/vendor IEs:** not represented — no NAT
  pre/post tuples, no firewallEvent action IE, no vendor IE storage.
  These are exactly the fields the Connectivity Investigator (E1)
  needs — backlog item under BN-016/BN-024.
- **Unverified projection:** no flow→canonical-device/host/topology
  projection was found beyond `CARRIES_FLOW`/`SRC_HOST`/`DST_HOST`
  links created at ingest; HostEndpoint linkage is best-effort.

**Design direction (backlog):** bounded *operational communication
summaries* (per-device-pair, per-class aggregates with windowed
counters) stored separately from individual flow records — preserves
analytics value without unbounded AppFlow growth. Do not replicate
firewall traffic intelligence into both core and collector graphs
(see F2).

## F2. Core and collector graph

- Core graph (`GraphStore`, LadybugDB): full schema — devices, sites,
  interfaces, BGP/BFD, StateChangeEvent, EventEmbedding, AppFlow,
  Playbook, ParquetExport, etc. Single-writer via `write_lock`.
- Collector graph (`src/collector/graph.rs`, 638 lines): reduced local
  state for disconnected operation — provisional identities, local
  buffering, replay on reconnect. It is **not** a replica of core;
  canonical identity resolution stays core-side (consistent with
  BN-021e provisional-source-key design).
- Canonical identity: device/site/VRF authority lives in core;
  collector carries `ObservationSource`/provisional keys only.
- Stale topology: no TTL/lease mechanism found on topology nodes —
  staleness is expressed via session state/completeness flags rather
  than node expiry (deliberate: history must not silently vanish).
  Current-state vs history retention split is implicit, not schema-
  enforced — backlog item for explicit retention classes (F5).

## F3. Admission and governor

- Governor (`should_shed`, rate_shedding + memory_pressure) is
  **core-only at startup** — collector-side shedding not found.
- Profile defaults vs effective settings: governor config comes from
  `bonsai.toml`/runtime config; effective values are not echoed into
  metrics — operator cannot see *which* threshold fired without config
  access. Backlog: expose effective governor settings + per-class
  counters.
- **Syslog shedding suppresses message AND extracted facts** —
  `handle_frame` returns before `bus.publish` for both the event and
  per-fact publishes when `should_shed()` (`src/signals/syslog.rs:592`).
  Archive retains raw JSONL, so evidence is not lost, but graph
  enrichment has a gap. Gap is metric-visible
  (`bonsai_syslog_shed_total`) but not recorded as an explicit graph
  `completeness_state` gap — **recommended fix (backlog): emit a
  bounded `ingest_gap` marker event so downstream consumers see the
  hole rather than infer it.**
- Backpressure oper-status debounce can suppress real transitions —
  debounce window may hide short flaps; verify debounce vs transition
  semantics during BN-009 soak analysis.
- Global 10 s rate window vs burst protection: a single global window
  cannot distinguish sustained load from microbursts — per-source
  budgets needed.
- Critical topology evidence reservation: no reserved lane found —
  BMP/routing evidence competes with syslog/noise in the same governor.
  **Design (backlog):** per-source/per-class byte+rate+cardinality
  budgets, a reserved critical-evidence lane (routing/session state),
  hysteresis on shed enter/exit, and operator-visible accounting
  (accepted/shed/queued bytes per class). Not implemented this session.
- Write coordinator failure/retry: `write_lock` poisoning propagates
  as error; coordinator retry semantics not fully traced — flag for
  BN-009 verification.

## F4. Collector disk queue — durability findings

Inspection targets (all **confirmed by code reading**, severity per
item):

- Forwarding subscriber `DropNewest` before disk append — newest
  telemetry dropped under pressure before it ever reaches disk.
- "Core accepted" = event-bus enqueue, **not** durable commit — the
  durability boundary is the collector's own disk queue, not the bus.
- Size limit enforced on **drain**, not append — queue can exceed its
  cap between checks.
- Append stats scans all unacknowledged records — O(queue) per stat.
- Queue-limit enforcement materializes all records — memory spike at
  the enforcement point.
- Ack compaction rewrites remaining records — write amplification on
  every ack batch.
- Per-record `sync_data` — fsync per record (durable but slow; correct
  durability choice, costly throughput).
- Ack file/crash consistency: ack state and data file are not updated
  atomically — a crash between data-append and ack-commit can replay
  (acceptable: replay is idempotent upstream) or, worse, lose the ack
  for already-consumed records.

**Durability guarantee (honest):** the collector disk queue provides
bounded local durability *up to its own ack boundary* — it does **not**
by itself prove end-to-end at-least-once delivery to core. Three
distinct loss windows must be reported separately:

- **Loss before durable acknowledgement** — `DropNewest` before
  append, send failures while the queue is empty/draining, and
  overflow beyond the configured cap all lose records that were
  never durably accepted.
- **Replay after crash** — data/ack non-atomicity can replay
  already-consumed records; replay is idempotent upstream but is not
  free (duplicate projection work, cursor ambiguity).
- **Overflow loss** — the cap is enforced on drain, not append, so a
  disconnected core can leave the queue growing past its configured
  limit until the next drain check; records accepted-then-evicted
  are loss without an explicit per-record gap marker.

Even with a healthy queue, "core accepted" remains an event-bus
enqueue, not a durable commit — downstream write failures
(`write_coordinator.rs` logs without a durable retry contract) are a
separate unaccounted-loss window. No exactly-once anywhere.

**Design direction (backlog, smallest suitable evolution):** segmented
durable queue/journal — append-only segment files with a compact
index, size cap enforced at append, ack = segment-level commit marker
(batch, not per-record fsync), stats from the index not a scan,
compaction by segment deletion not rewrite. Critical-evidence ack
must have a defined durable boundary (segment fsync before ack).
Kafka only for deployments already requiring distributed durable
transport — not adopted here.

## F5. Archive & retention — storage surfaces inventory

| Surface | Path | Bounds observed |
|---|---|---|
| Syslog JSONL | `runtime/signals/syslog.jsonl` | per-record flush, **no rotation/size cap** |
| General Parquet | `ParquetExport` pipeline | `DropOldest` subscriber policy; per-export row bounds via query materialization caps |
| Collector queue | collector disk queue | size cap on drain only (F4) |
| Core graph | LadybugDB `graph.db` | buffer-pool bound; node/rel growth unbounded per type |
| Collector graph | reduced local | smaller, replay-scoped |
| Routing journals | `BmpJournal`, `RibEventJournal`, `RoutingProjectionCheckpoint` | durable by design (AGENTS contract) |
| ML registry/vectors | `runtime/message_templates.db` + `EventEmbedding` nodes | **unbounded growth** — known WP6.5A open item (retention/shared-vector decision pending C5) |
| Operational/audit | `audit.rs` JSONL, adapter cursors | append-only |

Other confirmed items:

- Parquet `DropOldest` subscriber drops oldest pending exports under
  pressure — silent evidence loss at the export layer (metric only).
- Open partition writer cardinality: partition count is config-driven;
  no per-partition byte/FD bound verified — flag for verification.
- Archive write failure + restart: writers reopen append-mode; partial
  last line possible after crash — readers must tolerate torn tail
  (consistent with R01–R04 torn-tail handling elsewhere).
- Syslog per-record flush (no fsync) — see SYS-09.
- Duplicate core/collector archives: not found — single archive path.
- Disk guard (`disk_guard.rs`) **warns but does not enforce** — no
  hard stop on full disk; combined with unbounded JSONL/graph growth
  this is the top operational risk.
- Retention today is effectively `StateChangeEvent`-centric; AppFlow,
  embeddings, registry, orphan HostEndpoints grow without a matrix.
- Ladybug physical space reuse requires compaction checkpoints —
  `checkpoint_threshold` is set (2 MiB) but long-run space reclamation
  unverified. **A Ladybug checkpoint is not proof of physical disk
  reclamation** — checkpoints bound WAL/recovery cost; actual file
  shrink/reuse must be measured (file size before/after, page
  reuse), not inferred from checkpoint cadence.

**Retention matrix proposal (backlog, per evidence class):**

| Class | Hot | Warm | Cold | Notes |
|---|---|---|---|---|
| Routing/session state | graph current | journals → Parquet | archive | durable by contract |
| Syslog raw | JSONL (rotated) | Parquet | — | rotation + fsync policy (SYS-E) |
| AppFlow records | graph (TTL!) | aggregates | — | add idle TTL + summaries (F1) |
| Embeddings/registry | graph+SQLite | — | purge by model rev | WP6.5A C5 decision |
| Shun/audit logs | JSONL rotated | — | — | compliance window |
| Collector queue | disk segments | — | drop-oldest w/ gap marker | F4 evolution |

Explicit vocabulary required downstream: `raw_not_collected` /
`filtered` / `sampled` / `aggregated` / `expired` / `lost` /
`durably_retained` — never conflate "not collected" with "lost".
Raw security logs stay in the existing SIEM by default (E2).

## F6. Sizing worksheet (workload-based, measured vs illustrative)

Measured anchors (WP6.5A evidence + runs.csv):

- lbug CMake cold build 35–77 min; prebuilt-native mode ~1 min check.
- WP6.5A 10k-message dedup benchmark: bge-small CPU baseline 106.3 s
  vs dedup 54.7 s wall; **57.8% provider-text reduction**.
- Lifecycle test binary: ~4.1–4.7 GB peak RSS, ~1 GB dev binary.
- `liblbug.a` ≈ 2.36 GB per bundled build.

Illustrative worksheet (NOT measured — projections only):

| Input | Drives | Watch |
|---|---|---|
| Devices/interfaces | graph nodes, target maps | per-device fixed cost |
| Routes/path cardinality | RIB journals, path queries | write amplification |
| Unique flows/window | AppFlow nodes — **largest unbounded term** | needs TTL+aggregates |
| EPS / burst bytes | syslog archive, bus, governor | shed accounting |
| Archive compression | Parquet ratio ~5–15× JSONL (est.) | measure per class |
| Outage spool | collector queue × outage hours × EPS | F4 segment sizing |
| Graph write amp | events→nodes+rels+indexes | measure per event type |
| Query concurrency | Ladybug single-writer + read pool | p95 under load |
| ML memory | embedder + registry LRU + vectors | per-model footprint |
| Disk headroom | sum of all surfaces × retention | disk_guard is warn-only |

Rule: no enterprise throughput claims from resource-profile constants
alone — every number above marked measured or illustrative; the 10k/
100k WP6.5A baseline (C5) supplies the first real ingestion/storage
curve. Distinguish measured vs projected in all future sizing docs.

## G. Outbound & inbound integrations audit

Current adapters (`src/output/`): `splunk_hec.rs`, `elastic.rs`,
`prometheus.rs`, `servicenow_em.rs`, `syslog_adapter.rs`,
`change_management.rs`, `servicenow_aiops.rs`.

Source-reviewed concerns — **confirmed defects (bounded fixes
proposed, not implemented)**:

1. **Splunk/Elastic dedup state updated before successful delivery** —
   `splunk_hec.rs:266` inserts into `dedup` inside the per-record loop;
   the insert is not gated on the HEC POST succeeding for that record.
   A failed request followed by cursor/dedup advancement can suppress
   the retry → silent detection loss. **Fix:** commit dedup only after
   per-record 2xx (or HEC `ack` when implemented).
2. **Timestamp-only cursor pagination** — `cursor_ns` advances to max
   `fired_at_ns` (`splunk_hec.rs:233`); records sharing the same
   timestamp at the boundary can be skipped or duplicated. **Fix:**
   cursor = (timestamp, event_id) composite.
3. **Elastic bulk per-item errors not checked** — bulk API returns 200
   with per-item `error` entries; adapter treats HTTP 200 as success.
   **Fix:** parse items array, count/retry failures.
4. **HEC indexing acknowledgement not implemented** — Splunk HEC
   `indexer_ack` option unused; fire-and-forget. **Fix:** optional
   `indexer_ack_enabled` with bounded wait.
5. **Prometheus Remote Write failed-batch durability** — no WAL/spool
   observed for failed batches; retry policy unverified. **Fix:**
   bounded outbox or explicit drop metric.

Guarantee classification (precise, per adapter — none offer durable
delivery today):

- **Splunk HEC** — fire-and-forget HTTP POST; indexing is not
  acknowledged (`indexer_ack` unused). Combined with the
  dedup-before-ack defect below, effective guarantee is
  *at-most-once under failure* for affected records, best-effort
  otherwise.
- **Elastic** — bulk HTTP 200 treated as success while per-item
  `error` entries are unchecked; same at-most-once-under-failure
  exposure for rejected items.
- **Prometheus remote-write** — failed batches have no observed
  durable outbox/retry; classification is best-effort with possible
  silent loss.
- **ServiceNow EM / syslog adapter** — outbound notification paths;
  delivery semantics unverified per-item (no ack contract found);
  treat as best-effort until individually verified.

Proposed corrective tasks are small and independent (per-adapter,
per-guarantee) — schedule under BN-008/BN-019, not a rewrite.
Preserved requirements for the fixes: Splunk/Elastic **per-item
acknowledgement**, a **composite (timestamp, event_id) cursor**
(not timestamp-only), and **dedup committed only after ack**;
Prometheus remote-write **durability decision** (bounded outbox vs
explicit drop metric) recorded per deployment; bounded
segmented-journal evolution retained **without** a Kafka default;
topology-safe admission control with a **protected critical-evidence
lane** retained per F3.

**Inbound evidence-query adapter contract (design, begins with SIEM
search):** see E2 — `bonsai-evidence-reader` role, structured
predicates only, tenant/index/time/result limits, evidence references
(sourcype/index/event.id/query_sid), vault-isolated credentials,
operator authorization, audit logging, no agent-generated SPL/CLI,
no raw logs to hosted LLMs.
