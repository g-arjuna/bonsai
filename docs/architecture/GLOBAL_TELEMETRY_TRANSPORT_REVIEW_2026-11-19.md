# Global telemetry transport — architecture review (2026-11-19)

Status: proposed design / static source review, not a production qualification.
Baseline: main `ff97d5882f233d8ddb5ac226f243d20f83dda348`.
PR #32 was open at `c49cec7`; its subscription lifecycle work is not assumed merged.
This extends the [16 November resource/durability review](RESOURCE_DURABILITY_ARCHIVE_SIZING_REVIEW_2026-11-16.md), not a second backlog or a new transport implementation.

## Decision

Keep standalone and collector/Core gRPC deployment modes broker-free by default.
Keep the in-process bus for local dispatch, not as the durable acceptance boundary.
First fix the edge queue and durable Core acceptance / projection contract.
For independently scaled consumers, multi-AZ durable ingress, or measured fleet
requirements beyond the direct path, qualify an OPTIONAL regional replicated
log (Kafka or a compatible managed service). No new mandatory service, custom
distributed broker, or Kafka dependency in Secure Core. No multiple graph writers
until ownership/fencing and recovery are independently proved.

## Current source path and findings

- `src/ingest.rs:343–429, 612–675`: a local forwarding subscriber uses
  a 4096-record DropNewest channel BEFORE the disk append; queue open/write
  errors stop forwarding/writer, logged rather than proving recovery.
  Raw/debounce/summary modes are useful for traffic shaping, but counters
  and authoritative transitions have different semantics.
- `src/config.rs:1902–1925`: default queue 1 GiB, 24h age, 1000 records per
  replay batch. These are configuration limits, not demonstrated outage
  capacity. A zero configured limit is unlimited.
- `src/ingest.rs:831–1055`: synchronous per-record sync_data plus full
  pending scan for stats, limit enforcement only on drain, full pending
  materialization and remove(0) during eviction, rewrite on ack. Outage
  accumulation magnifies I/O, CPU and memory pressure. Crash after compacted
  file rename but before ack reset can leave an old byte offset in a new
  file generation (potential skipped retained records, not merely duplicate
  replay). Ack writes do not use durable atomic replace; test every
  boundary, malformed/truncated offsets and directory fsync failures. Also
  test append between empty batch check and Notify::notify_waiters wait,
  plus failed writer supervision. These are static risk findings, not
  reproduced crash outcomes.
- `src/api.rs:1989–2057`, `docs/collector_core_protocol.md`: authorized
  input is either suppressed by debounce (still increments accepted) or
  enqueued into volatile bus (accepted). Router overload returns retryable
  Unavailable, preserving the entire batch; this is newer than the old
  backlog's publish-returns-unit finding. A prior partial stream can be
  replayed. Accepted does NOT imply durable evidence or graph commit.
- `src/event_bus.rs:262–305`: PublishOutcome distinguishes router overload;
  broadcast remains best effort and is published even before router result.
  Per-subscriber queues are bounded, but join_all on each event lets a
  blocked graph subscriber delay subsequent routing to other consumers.
- `src/server_startup.rs:498–531`, `src/write_coordinator.rs:128–165,
  338–375`: graph writer blocks on coordinator capacity; coordinator
  defaults 4096 normal / 256 priority. Failed telemetry batches are logged,
  not durably retried. `src/graph/mod.rs` has process-local write_lock;
  this is not distributed writer fencing.
- `src/archive.rs:128–150`: Parquet consumer handles broadcast lag by
  counting dropped rows; archive is not a reliable journal or replay source
  for every accepted observation. Archive is opt-in by default.
- `src/telemetry_identity.rs:152–192` already supplies source identity
  evidence and scope/epoch; generic `TelemetryUpdate` and
  `TelemetryIngestUpdate` (proto/bonsai_service.proto:215–253) do not
  expose a universal immutable observation ID / per-source sequence.
  Extend the existing ABI, not a parallel identity system; do not assume
  general replay idempotency from routing or graph upsert behavior.
- `crates/rbmp-store/src/journal.rs`, `src/routing_service/mod.rs`:
  preserve existing canonical BMP/RIB journals, session fencing and
  graph projection checkpoint. Reuse their pattern, not a second
  authoritative routing decoder or journal.
- `src/ha_coordinator.rs`, `deploy/helm/bonsai`: etcd coordination and
  separate PVCs do not replicate graph/RIB state. BN-023 is already the
  owner of single-writer deployment, failover and split-brain evidence.

## Research and implications

| Reference | Pattern | Bonsai implication |
|---|---|---|
| [OpenTelemetry resilience](https://opentelemetry.io/docs/collector/resiliency/) and [scaling](https://opentelemetry.io/docs/collector/scaling/) | Agent/gateway, bounded sending queue, persistent WAL, optional broker; stateful stages require affinity | Preserve local buffering even with broker; scale stateless ingress, partition stateful acquisition |
| [Grafana Mimir ingest storage](https://grafana.com/docs/mimir/latest/get-started/about-grafana-mimir-architecture/about-ingest-storage-architecture/) | Kafka durability before client ack, independent read projection and offset-aware strong reads | Durable receipt separate from query visibility; expose watermarks and bounded strong-read mode |
| [VictoriaMetrics topologies](https://docs.victoriametrics.com/guides/vm-architectures/) and [vmagent](https://docs.victoriametrics.com/vmagent/) | Local compressed disk spool, replicated/sharded storage, optional Kafka | Broker-free path can scale; bound disk usage and catch-up |
| [Cloudflare GoFlow](https://github.com/cloudflare/goflow) | Decode flow protocols, protobuf, optional Kafka, enrich then analytical storage | High-volume flow lane and independent analytics, not per-flow graph expansion |
| [Cloudflare global pipeline (2026)](https://blog.cloudflare.com/rewinding-the-exponential/) | Edge senders, central receivers, Buftee buffering, consumers, compression, decentralized sampling and aggregation | Broker is one part of design; reduce volume near source and isolate consumers |
| [Uber M3](https://www.uber.com/in/en/blog/m3/) and [ingestion optimization](https://www.uber.com/us/en/blog/optimizing-m3/) | Shard-aware aggregation, regional storage and federated queries | Global view without replicating all raw data globally |
| [pmacct FAQ](https://github.com/pmacct/pmacct/blob/master/FAQS) | Data reduction and distributing exporter inputs among collectors | Protocol-specific affinity and aggregation for network flows |
| [Kafka multi-datacenter guidance](https://kafka.apache.org/39/operations/datacenters/) and [processing semantics](https://kafka.apache.org/35/streams/core-concepts/) | Local clusters and asynchronous mirroring; transactional guarantees bounded by participating stores | No WAN-stretched cluster; external graph projection requires idempotence/checkpoints |

These are design comparisons, not Bonsai benchmarks or guarantees that any
vendor's published rate transfers to Bonsai.

## Target contract and topology

1. Edge collector: bounded per-source/class admission; stable source-scoped
   event IDs and session epoch/sequence assigned before retry; segmented WAL
   with checksums, generation-aware durable commit/ack, batched fsync,
   incremental accounting and hard append-time quotas. Reserve capacity for
   critical state; account for upstream UDP loss/unknown coverage.
2. Regional ingress: existing authenticated gRPC boundary validates assignment,
   scope, schema, provenance and resource budgets. Only acknowledge critical
   evidence after durable append (local journal in direct mode, replicated
   log in enterprise mode). Separate durable accepted, intentional
   suppression, permanent reject and retryable failure in a negotiated ABI.
   Do not silently upgrade legacy accepted to durable accepted. Reauthorize
   queued evidence and preserve acquisition authority/epoch; no stale replay
   restoring revoked live authority.
3. Independent durable consumers: graph projector, archive/TSDB, correlation/
   rules, embeddings/ML and external exports with per-consumer offsets,
   idempotence, explicit dead-letter/quarantine and bounded retry. Graph
   checkpoint advances after committed effect; crash between commit and
   offset advance must replay safely. Do not put raw RIBs, flows or samples
   into graph by default. Best-effort UI fanout may remain volatile.
4. Partition by verified scope + stable source/stream identity, not source IP,
   hostname, arrival time or collector name alone. BMP speaker/session,
   gNMI target/assignment epoch and IPFIX exporter/domain/template context
   need affinity. Ordering is per logical stream; use event time, sequence,
   epoch, watermarks and explicit gaps for cross-stream reconciliation.
   Preserve repeated legitimate occurrences, not content-hash dedup alone.
5. Global federation (future BN-023/024/013 design): region-local ingestion,
   storage and ownership; global read/query layer for selected evidence and
   summaries, per-region/source checkpoints and explicit missing regions.
   Cross-region replication is async with declared RPO; failover requires
   fenced ownership, restore and parity. Do not promote regional graph
   sharding or active-active claims from broker partitions alone.

Critical transitions, tombstones, subscription coverage, audit and routing
events: no intentional silent loss; reserve capacity, fail visibly or mark
a durable scoped gap and reconcile. Numeric metrics: explicit aggregation,
reset/wrap, units and coverage. High-rate flow/wireless/client/syslog streams:
bounded cardinality, sampling/filtering by policy, raw evidence/archive where
required, avoid indiscriminate high-rate graph writes. Security-relevant logs
and extracted state facts need separate retention/priority. No broker topic
may bypass trusted ingress or become an alternate canonical identity store.

## Capacity and promotion criteria

Use actual encoded bytes/s and samples/s per source/class plus sustained and
burst sink throughput. Outage spool = rate × bytes × outage seconds + measured
overhead/headroom. Illustrative only: 5,000 forwarded records/s × 300 B =
1.5 MB/s, 129.6 GB/day and 5.4 GB/hour; a 1 GiB default holds ~12 minutes
before overhead, not 24 hours. A four-hour backlog at that rate is 72M records;
to drain it in two hours while live traffic continues requires 15k records/s
sustained. Measure write amplification, oldest age and memory as well as EPS.

Extend existing BN-008/009/019/021/023 acceptance with:
- independent IDs/counts at receive, append, ack, commit and projection;
  every admitted critical record present or an explicit bounded gap,
  no duplicate effects, no false state resurrection;
- kill/restart before/after fsync, ack, rename, graph commit/checkpoint;
  disk full/corrupt/permission failure, WAN outage beyond retention,
  disconnected producer, slow/failed graph/archive/ML consumer, replay
  concurrent with live load, source handover/revocation, split brain;
- measured steady/burst mix (gNMI, BMP, flow, syslog, wireless/AI metrics),
  queue bytes/age, critical lane p99, projection lag, RPO/RTO, catch-up,
  graph query latency under ingest, RSS/IOPS, per-source fairness and
  source-specific gap accounting. Publish limits, not arbitrary device counts.

Broker PoC only after direct-path correctness and measured need: e.g. host
loss must not lose acknowledged data; independent replay of multiple
consumers; required recovery window cannot be met by local spool; or
customer already operates a supported broker. Use regional replicated
topics, durable quorum acknowledgement, scoped service ACLs, retention
and partition sizing, consumer lag metrics, and fault tests for broker
unavailability/rebalance/retention expiry, schema skew and cross-region
replication lag. Compare direct vs broker on the same workloads and failure
matrix. No automatic production promotion or external dependency until
benefit, operator burden and rollback are reviewed.

No runtime/load tests or implementation changes in this documentation review.

## Second-pass sustainability review (19 November)

Primary upstream references independently rechecked; these are incremental
acceptance refinements, not new product epics or dependencies.

- **Telegraf** ([pipeline](https://docs.influxdata.com/telegraf/v1/concepts/data-pipeline/),
  [tracking](https://docs.influxdata.com/telegraf/v1/concepts/metrics/),
  [configuration](https://docs.influxdata.com/telegraf/v1/configuration/)):
  input/processor/aggregator/output stages with per-output buffers and
  explicit filters, tracking for supported queue inputs, collection and
  flush jitter. Memory buffers lose oldest data on overflow; disk buffers
  are not disk-space capped. Reuse BN-024-03/25's extension seam and existing
  outputs/sidecars rather than a new plugin framework or mandatory Telegraf.
  Explicit delivery, trust, resource and version contracts per adapter;
  no generic metric adapter can establish canonical routing authority.
  Compare supported gNMI/SNMP outputs as differential references, not
  protocol-completeness or durability oracles.
- **InfluxDB 3 Core** ([durability](https://docs.influxdata.com/influxdb3/core/reference/internals/durability/),
  [object storage](https://docs.influxdata.com/influxdb3/core/object-storage/),
  [last-value cache](https://docs.influxdata.com/influxdb3/core/admin/last-value-cache/)):
  default acknowledgement follows WAL persistence; recent data is queryable
  before conversion to Parquet. Last-value cache is cardinality-sensitive
  and volatile on restart; object catalog requires conditional creates and
  strong read/list visibility. Core is OSS; Enterprise adds HA/read
  replicas and historical-query capabilities, not assumed present in Core.
  Bonsai already archives Arrow/Parquet and uses Ladybug for current
  semantic state. In src/archive.rs:516–588, files are opened under final
  names and closed by ArrowWriter; no durable manifest publication shown
  there. Refine BN-008/009/019/023: immutable finalized segments with
  checksummed manifest, source/scope, time bounds, schema revision, counts
  and source checkpoint; publish only after verified file/object durability.
  Acknowledge archive progress after manifest commit, never delete the
  sole replay copy before verified recovery coverage. Test partial upload,
  interrupted close, manifest/segment mismatch, orphan cleanup, concurrent
  retention and restore. Bound small-file/FD/compaction debt, cache and
  catalog memory, and measure historical query cost. Object storage is
  conditional, not a required service; never assume a shared bucket
  provides transactional graph or writer ownership.
- **Mimir** ([architecture](https://grafana.com/docs/mimir/latest/get-started/about-grafana-mimir-architecture/),
  [query scheduler](https://grafana.com/docs/mimir/latest/references/architecture/components/query-scheduler/),
  [shuffle sharding](https://grafana.com/docs/mimir/latest/configure/configure-shuffle-sharding/)):
  one binary can deploy multiple roles; separately scaled query scheduling
  and tenant fault isolation protect shared resources. Keep Bonsai's
  unified build; split roles only when measured. src/graph/explorer.rs:
  734–788 already has active diagnostics and engine interruption on
  deadline/cancellation. That does not establish fair scheduling or
  ingestion protection across API/MCP/UI/ML. Extend BN-024-20/013/009:
  bounded per-principal/scope concurrency and work budgets, cancellation
  until worker actually exits, reserved ingestion resources, separate
  interactive vs batch workload quotas. Test expensive concurrent
  investigations/export against live routing/gNMI and measure committed
  critical p99/lag, query p99, fairness, RSS/IO and recovery. No new
  scheduler service or distributed graph required.
- **Akvorado** ([design](https://github.com/akvorado/akvorado/blob/main/console/data/docs/80-design.md),
  [overview](https://akvorado.net/docs/intro)): raw flow UDP receive is
  separated from Kafka-backed parsing and SNMP/gNMI/BMP enrichment, then
  ClickHouse. UDP receive cannot wait for CPU-heavy parsing; broker cannot
  restore packets lost before it. Bonsai src/streaming/netflow.rs:799–850
  receives and decodes in one loop with local template state. For BN-016/
  009/021, benchmark kernel/socket loss separately from application drops,
  parse cost and downstream lag under burst. Only if bottleneck observed,
  introduce bounded receive/worker separation with stable exporter/domain
  affinity (templates/data ordering and restart epochs), original capture
  time and provenance. No blind load balancing or duplicate route authority.
  Evaluate packet batching/socket tuning before adding services; no claim of
  lossless UDP.
- **gNMIc** ([collector](https://gnmic.openconfig.net/user_guide/collector/collector_intro/),
  [clustering](https://gnmic.openconfig.net/user_guide/HA/)): dynamic config,
  distributed locks and target assignment, collect once/fan out. Reuse
  Bonsai assignment generations and exact subscription lifecycle.
  src/subscriber.rs:112–141 uses deterministic exponential reconnect sleep;
  apply bounded jitter, fleet-wide concurrency admission and staged
  initial sync/replay across sites under BN-003/008/009/023. Do not use
  a broker as acquisition ownership or indiscriminately duplicate sessions.
- **VictoriaMetrics** ([vmagent](https://docs.victoriametrics.com/victoriametrics/vmagent/),
  [stream aggregation](https://docs.victoriametrics.com/victoriametrics/stream-aggregation/))
  and **Prometheus** ([remote write](https://prometheus.io/docs/practices/remote_write/)):
  active series and new-series churn are separate costs; event-time windows
  require late-data policy, and increasing sender shards increases memory
  and can overload the receiver. Existing BN-025-02 reset/time semantics
  and BN-009 cardinality budgets cover this; measure active vs new keys per
  hour/day, and test churn, late histogram buckets, aggregation policy
  revision, sample coverage and adaptive catch-up within bounded bytes/
  concurrency. No timestamp rounding of distinct critical events.
- **Cloudflare** ([2026 pipeline evolution](https://blog.cloudflare.com/rewinding-the-exponential/)):
  schema decomposition, compression, decentralized sampling and stream
  aggregation reduce infrastructure growth; evaluate cost per usable
  observation rather than copying its event-rate benchmark. Existing
  sizing/retention and telemetry lanes already cover the main direction.
  Record bytes at acquisition/wire/archive, CPU per decoded record,
  graph amplification, usable retained coverage and compression ratio;
  compare fixed representative workloads across releases. Sampling must
  preserve probability/coverage and not remove critical transitions.

### Closeout

Refine existing backlog rows for archive manifest/restore, fair read-side
resource isolation, UDP pre-decode loss, and synchronized fleet recovery.
Other findings reinforce existing BN-024 extension, admission, storage,
metric and sizing work: no new backlog epic, no required InfluxDB,
Telegraf, ClickHouse or Kafka runtime. No runtime qualification or
benchmark results in this documentation-only pass.
