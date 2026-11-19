# Telemetry persistence, historical analytics and storage lifecycle — 2026-11-19

Status: architecture review and implementation intake; not a runtime qualification.
Baseline: main `1313388a7c3f87f67d96395d69569b0e671a866b`.
Extends [resource/durability review](RESOURCE_DURABILITY_ARCHIVE_SIZING_REVIEW_2026-11-16.md)
and [global transport review (PR #34)](GLOBAL_TELEMETRY_TRANSPORT_REVIEW_2026-11-19.md).
One canonical backlog, no new epic, dependency or implementation in this PR.

## Decision and product boundary

Bonsai owns canonical network intelligence and the evidence interface, not
necessarily every historical byte. Keep Ladybug for current/temporal semantic
state, dedicated BMP/RIB journals for routing recovery, a bounded local
receipt/spool for transport, typed numeric history in an approved TSDB where
required, Parquet for economical historical evidence (not merely ML), and
approved external log/flow/trace stores for bulk data. No mandatory TSDB,
object store, Kafka, ClickHouse or InfluxDB; standalone remains viable with
explicitly bounded local storage and published history limitations. Do not
build a general TSDB, SIEM, data lake or replicated broker inside Bonsai.

## Source-verified baseline

| Surface | Current implementation and gap |
|---|---|
| Core graph | src/graph/mod.rs:6668,6936 interface summary/raw writes MERGE and replace latest values. SensorReading similarly upserts; this is not per-sample history. AppFlow full tuple churn can grow cardinality. Preserve structural topology and evidence provenance; don't put every metric or flow in permanent graph nodes. |
| Collector graph | src/collector/graph.rs reduced local Device/Interface/BGP/BFD/LLDP/event state, not a full Core replica. src/config.rs:1762–1776 default graph buffer pools min(2 GiB,25% RAM) Core and min(256 MiB,10% RAM) collector; these are memory settings, not disk caps. |
| Streaming Parquet | src/archive.rs:112–165,516–638, src/config.rs:1552–1589: opt-in by default; 10s/1000-row flush, ZSTD level 12, dictionary encoding, per-target/hour writers, idle close 2h, forced rotation 1h. Value is JSON text; no complete typed historical query, compaction or durable external publication contract. Broadcast lag drops rows. Startup prunes stale zero-byte files, not age/size retention of valid archives. |
| Archive placement | docs/archive_format.md says collector-local; src/server_startup.rs:907–936 permits both Core and collector if enabled. docker/configs/core.toml enables runtime/archive; collector-1.toml has no archive enable and forwards 60s summaries. Both enabled can duplicate forwarded observations (not necessarily identical resolution). Reconcile docs and effective ownership; never assume global raw coverage from Core archives. |
| Storage guard | src/config.rs:1593–1625,1778–1836: nominal 10 GiB archive, 5 GiB graph, 80% warning, 300s checks. src/disk_guard.rs:74–137 warns/emits health/metrics, does not enforce deletion or admission despite config comment. Background guard Core-only (startup:953–967); does not budget all volumes or collector surfaces. |
| Graph retention | src/retention.rs and startup:2135–2165 prune StateChangeEvent by age (24h) and count (10k), hourly. Count is periodic, not a hard admission bound. Does not imply AppFlow, DetectionEvent, ConfigSnapshot, embeddings or all node/edge retention. Verify incident refs before expiry and logical deletion vs physical page reuse/reclamation. |
| Forwarder | src/ingest.rs queue defaults 1 GiB/24h, cap enforced during drain not append; durability and efficiency gaps already owned by BN-008/009/021g/023 and PR #34. Queue is outage staging, not history. |
| Routing | src/routing_service/mod.rs:659–721 loads/replays BmpJournal and RibEventJournal, reconciles event sequences and projects checkpointed state; src/bin/routing_service.rs leases journal pair. rbmp-store DuckDB code exists but active routing service relies on append-only JSONL journals; no complete bounded snapshot/segment retention established there. Never truncate live recovery history just to meet quota. |
| Syslog | src/signals/syslog.rs:727–778,1008–1043 default runtime/signals/syslog.jsonl appended/flushed before shun and graph shedding; no built-in size/age rotation in writer. Graph filtering does not bound raw disk. Flush != durable fsync; archive error must not imply full coverage. |
| ML exports | python/bonsai_ml/{export_job,parquet_store}.py separate runtime/parquet dataset catalogue/quality checks, latest symlink and keep-last-N cleanup helper (invocation/whole-lifecycle enforcement not established). Copies and superseded datasets may accumulate; separate from raw archive. |
| Embeddings/audit/other | EventEmbedding/template registry lifecycle under BN-011/WP6.5A; src/audit.rs daily JSONL append invokes 30-day cleanup (not hard quota or guaranteed scheduled cleanup if idle). Include SQLite/WAL, config captures, model artifacts, logs, cursors, backup/temp/compaction and incident evidence pins in budget inventory. |

## Output adapter maturity (static, not fresh live qualification)

`src/output/traits.rs` registry/config/credential alias, batching, audit and
run state provide a useful extension seam. Do not equate adapter existence,
connectivity test, or a successful HTTP response with durable destination
storage. Existing smoke/E2E scripts are not evidence of current fleet-scale
replay, interoperability or complete coverage.

- Prometheus Remote Write (`src/output/prometheus.rs:120–190,275–369`):
  numeric scalar/flat numeric or boolean object fields mapped to path-derived
  metric names/labels, millisecond timestamps, Protobuf/Snappy HTTP. A 1024
  entry DropNewest subscriber and in-memory batch (taken before send) mean
  failed sends/restart can lose samples; no WAL/retry contract. Verify
  per-series ordering/duplicate timestamps, schema/label collisions, precision,
  counters/resets, units, histograms, summary/nested values, explicit
  missingness and cardinality; do not infer full source coverage. Separate
  Bonsai self-monitoring scrape from network-device history.
- **Placement gap:** `src/output/mod.rs` describes collector-side metrics;
  `src/server_startup.rs:1566,1960–1996` starts enabled generic adapters in
  Core-only branch. No equivalent collector-only generic launch path established
  in audit. A Core receiving summarized counters cannot reconstruct discarded
  high-resolution samples. Choose export stage before loss of required
  resolution, after authenticated scoped identity and normalization; if local
  collector export chosen, explicitly authorize destination, scope/labels and
  assignment epochs; no alternate graph identity authority or duplicate series
  writer. Backfill/reassignment and revoked source tests mandatory.
- Splunk HEC / Elastic (`src/output/{splunk_hec,elastic}.rs`): graph-poll
  detections and linked remediation information, not raw logs/flows or a
  general historical ingestion path. Persisted cursor, pre-send dedup and
  timestamp-only pagination; Elastic HTTP success misses per-item failures,
  HEC HTTP success not indexing ack. Existing outbound contract work owns
  retries, composite cursor, dedup after ack and sink-specific acknowledgments.
- ServiceNow EM: operational notifications/events, not a bulk persistence
  target. Preserve scoped credentials and distinct notification guarantees.
- Syslog/SNMP modules exist but generic build factory only constructs
  Prometheus/HEC/Elastic/ServiceNow; syslog accepted type without builder.
  Trace actual wiring/support separately, do not claim full adapter parity.
- No dedicated native InfluxDB or ClickHouse adapter or complete streaming
  object-storage offload established. First qualify existing Remote Write with
  an approved compatible endpoint (Prometheus receiver, Mimir, VictoriaMetrics
  as applicable) and reliable event exporters. Backend-specific connectors
  only for demonstrated schema, query, operational or customer need.
  Optional S3-compatible finalized archive publication has strategic value
  after local lifecycle/manifest proof; no required cloud service.

## Target lifecycle policy and placement

Use a versioned effective per-scope/class policy, not independent uncoordinated
TTL toggles. Every class declares owner, stage, local/destination bytes and age,
resolution, raw vs derived, event/receive time semantics, priority, admission
and overload behavior, durability/ack/checkpoint, retention vs incident pins,
deletion/reclamation and coverage. Enforce hard append/admission limits and
filesystem reserves on both roles; separate journal/graph and bulk scratch
budgets or volumes. Observe actual free bytes/inodes, physical used bytes,
growth rate/time-to-full, oldest age, writer/FD counts, lag, expired vs dropped
vs deliberately filtered and committed records. Local quota must include
staging/compaction/restore/backup overhead. A full external sink cannot justify
unbounded local buffering; critical capacity reserved, explicit gap if exhausted,
no false freshness. Never evict authoritative graph identity to save bulk space.

Illustrative policy only; exact durations are deployment requirements, not
Bonsai defaults or committed SLA:

| Class | Operational state | Historical destination and example retention | Collector responsibility |
|---|---|---|---|
| Interface/device/optical/power/QoS/RDMA numeric metrics | Latest and incident extrema in graph | Approved TSDB, raw 14–30d; 5m/hour rollups 1–2y, typed Parquet where appropriate | Scoped normalized pre-summary export or bounded spool; not permanent history |
| BGP/BFD/IGP/interface transitions and gNMI deletes/sync | Authoritative graph with provenance/coverage | Durable event log/archive; explicit event time and tombstones, incident pins | Preserve critical transitions, replay/gaps |
| BMP/RIB | Canonical routing state and graph summary | Ordered journal + recovery snapshots; detailed history 7–30d only after proven safe checkpoint/restore, significant events longer | No competing routing authority |
| NetFlow/IPFIX/sFlow | Bounded conversation/service summaries; TTL ephemeral AppFlow | Columnar flow store or typed Parquet; raw 3–14d, aggregates 90–365d | Decode with exporter/template affinity, bounded sampling/spool |
| Syslog/security events | Selected state facts and incidents | Existing SIEM/log archive; ordinary 7–30d, selected longer per policy; audit separate | Bounded raw rotation, preserve critical evidence and distinct shun/export policy |
| OTLP metrics / spans | Relevant service context, not all raw traces | Metrics TSDB / approved trace backend, separate class TTL | Bounded admission and scope |
| Wireless client/session and ephemeral endpoints | Current attachment and bounded session facts | Short raw 1–7d, session/site aggregates 30–180d; privacy-sensitive | Cardinality and per-site limits |
| Config versions / topology / inventory | Versioned canonical state, no generic age deletion | Secure version archive, approved baselines, policy-defined retention | Authorized acquisition only |
| Incidents/audit | Current investigation and reference integrity | Pin required evidence, secure external archive per policy | Bounded staging, not a second audit authority |
| ML datasets/vectors/templates | Model-compatible references | Versioned dataset store, model-revision/orphan GC and pins | No indefinite copies or training scratch |
| Recovery spools | Progress and gaps | Delete after verified durable handoff, bounded outage window | Hard local quota + reserve; not long-term store |

Downsample by typed semantic descriptor: units, counter width/reset/wrap and
delta/rate, gauge time-weighted statistics, min/max/peak, histogram merge,
sample count/window coverage, event-time/late data policy and source identity.
No fabricated zero or mean-only concealment of microbursts; critical transitions
are never time-rounded into a numeric average. External retention is a declared
contract (verify/query when supported), not silently controlled by Bonsai.

## Historical evidence access

Remote Write is a write API, not historical query. Extend BN-024-20/013/008
existing authorized evidence ABI with one bounded historical metrics pilot
(Prometheus-compatible instant/range queries) and existing Parquet query path
where available; reuse SIEM inbound-query design rather than new parallel
adapter registry. Scope time, source, tenant and metric/series allowlist;
query byte/row/time/work/concurrency budgets and cancellation; return typed
units, window/resolution, coverage, gaps, freshness, source checkpoint and
original evidence handle. No arbitrary query/credential or cross-tenant
bypass; unavailable/expired/partial backend is unknown, never zero or absence.
Historical graph join must use as-of topology and identity, not today's
attachment as a historical fact. Test path/metric time alignment, reset,
backend outage, retention expiry, raw vs summarized series, overlap/replay,
scope denial and slow query versus ingestion. A TSDB and Parquet query engine
are replaceable backends, not new sources of canonical graph identity.

## Implementation order and acceptance (existing BN owners)

1. BN-008/009/019/023: implement per-class byte quotas/reserves, durable
   spool/receipt, graph/transient TTL, syslog rotation and archive ownership;
   protect routing/config/audit writes under simultaneous bulk exhaustion.
   Retention must be crash-safe and replay-aware. Test separate Core/collector
   ENOSPC, inode exhaustion, archive disabled/external-only, destination down
   longer than configured horizon, startup recovery, and physical reclamation.
2. BN-007/008/019: routing snapshot/segment lifecycle with independently
   verified live/replay RIB and graph checkpoint equivalence; interrupted
   compaction/restore, epoch/gap and history coverage. No deletion of sole
   recovery copy and no arbitrary journal TTL. Coordinate with existing
   canonical routing service.
3. BN-015/016/019/008/021/023: qualify Remote Write placement and metric ABI;
   bounded retry/outbox with explicit best-effort option, stable IDs/series,
   per-sink ack/lag, auth and tenant isolation; actual receiver query confirms
   correct points/resolution/scope, outage/replay and no duplicate authority.
   Harden existing alert adapters as already specified. No new backend by default.
4. BN-008/009/011/019/023: finalized archive manifest/offload and verified
   restore; only retire sole copy after committed coverage. Test partial
   segment/upload, checksum/catalog mismatch, concurrent retention/pins, file
   count and small-file compaction debt; reconcile ML export cleanup and audit.
5. BN-024-20/013/008/016/019: authorized bounded historical metrics/archive
   evidence pilot and correlation with routing/state/logs. Measure query
   contention, actual data coverage and agent abstention on missing history.
6. BN-019/009/023/024-23: deployment capacity model and mixed-source sustained,
   burst, churn, outage and catch-up soak. Record per-class encoded/physical
   bytes, compression, write amplification, queue and retained age, read
   latency, graph growth, RPO/RTO and operating costs; publish measured limits.

Sizing example only: 5,000 forwarded records/s x 300 B = 129.6 GB/day;
12h outage = 64.8 GB before overhead, 30d at assumed 4:1 compression =
972 GB before indexing, replication and headroom. One collector's spool and
historical archive are separate allocations; fleet sums and concurrent
compaction/restore must be modeled. No benchmark implied.

References: [Prometheus storage](https://prometheus.io/docs/prometheus/latest/storage/),
[Mimir retention](https://grafana.com/docs/mimir/latest/configure/configure-metrics-storage-retention/),
[InfluxDB 3 durability](https://docs.influxdata.com/influxdb3/core/reference/internals/durability/),
[VictoriaMetrics stream aggregation](https://docs.victoriametrics.com/victoriametrics/stream-aggregation/),
[ClickHouse TTL](https://clickhouse.com/docs/guides/developer/ttl).
Existing research references, not a new vendor qualification. Implementation
requires targeted runtime evidence before bounded-storage, reliable external
history or enterprise-scale promotion. Secure Core ordering unchanged.
