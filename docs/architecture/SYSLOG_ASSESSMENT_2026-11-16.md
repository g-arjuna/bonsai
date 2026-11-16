# Syslog Receiver Assessment (SYS-01…SYS-09)

Source-audited at `feat/wp65a-template-dedup` merged with `origin/main`
(candidate SHA `fe4071e` + merge `bfea68b`; receiver code unchanged by
WP6.5A). Scope: `src/signals/syslog.rs`, `src/shun.rs`,
`src/http_server/shun.rs`, `src/config.rs`, `src/collector/`.
Static source review only — no live receiver run.

Bonsai is a network context/evidence engine, not a SIEM or a general
syslog collector. Findings are classified: **defect** (correctness/
durability bug), **limitation** (deliberate or accepted scope boundary),
**gap** (missing capability worth a backlog item).

## SYS-01 Bounded TCP framing — PASS with notes

- RFC 6587 octet-counting is implemented (`run_tcp_octet_counting`,
  `src/signals/syslog.rs:436`): `<len> <MSG>` with the length prefix
  capped at 7 digits and `msg_len <= max_frame_bytes`
  (`syslog.max_frame_bytes`, default 8192, `src/config.rs:537`).
  Oversized or malformed frames close the connection — fail-closed, no
  unbounded allocation (`msg_buf` is allocated only after the bound
  check).
- Dual-mode framing: connections are probed for octet-counting vs
  newline framing (`src/signals/syslog.rs:354-378`).
- **Limitation:** on any framing error the whole TCP connection is
  dropped rather than resynchronized — acceptable for evidence
  collection (sender retransmits/next connection), but worth noting for
  chatty senders.

## SYS-02 Idle/shutdown handling — PARTIAL

- EOF and read errors terminate the per-connection task cleanly
  (`return` on empty `fill_buf`, warn on error); connection counting
  (`conn_count`) is decremented on exit.
- **Gap:** no idle/keepalive timeout on established TCP connections was
  found — a silently dead peer can hold a task indefinitely. Low risk
  (tasks are cheap) but unbounded in count. Recommend a backlog item
  for an idle timeout + `receiver_supervisor` visibility.
- Shutdown: receiver tasks are spawned under the supervisor; no
  graceful drain of in-flight frames was observed — bounded by frame
  size, acceptable.

## SYS-03 RFC 5424 structured data — PARTIAL

- `parse_rfc5424` (`src/signals/syslog.rs:1161`) handles VERSION=1,
  splits HEADER fields, normalizes NILVALUE `-` via `normalise_nil`,
  and falls back to `parse_legacy` on malformed input — no panic path.
- **Limitation:** STRUCTURED-DATA is *not parsed or retained*. When the
  SD element starts with `[` or is `-`, the parser skips it and keeps
  only the MSG part (`src/signals/syslog.rs:1174-1184`). SD-ID params
  (e.g. `exampleSDID@32473 iut="3"`) are evidence that is currently
  discarded — relevant for firewall/vendor telemetry that carries
  structured context. Backlog item: bounded SD capture into the event
  record (cap elements/params, preserve raw anyway since `raw` is
  archived verbatim).
- **Note:** BOM (`\ufeff`) handling per RFC 5424 §6.1 is not stripped —
  a BOM would land inside MSG. Minor.

## SYS-04 Timestamp provenance — LIMITATION (explicit)

- `SyslogEvent.timestamp_ns` is always the *receiver* clock
  (`now_ns()` at ingest, `src/signals/syslog.rs:1122` signature takes
  `timestamp_ns` from caller). The RFC 5424 TIMESTAMP header field is
  read but discarded (`let _timestamp = parts.next()?`), and RFC 3164
  has no timestamp at all.
- Consequence: clock skew between device and collector is invisible;
  backlogged/batched forwarding loses original event time. The raw
  message is archived, so sender timestamps remain recoverable
  forensically but are not queryable.
- **Recommendation (backlog):** parse RFC 5424 TIMESTAMP into an
  optional `sender_timestamp_ns` field with explicit provenance
  (`timestamp_provenance: receiver|sender|none`) — consistent with the
  project's "never fabricate" rule; do not silently substitute.

## SYS-05 PRI validation — MINOR DEFECT

- `parse_priority` (`src/signals/syslog.rs:1150`) accepts any `u8`
  inside `<...>` — values 192–255 are impossible per RFC 5424
  (facility 0–23 × 8 + severity 0–7 ⇒ PRI ≤ 191). PRI 200+ is stored
  verbatim and `facility = pri/8`, `severity = pri%8` produce
  out-of-range derived values.
- **Defect (minor):** no PRI range check; malformed `<pri>` without a
  closing `>` is treated as "no priority" rather than rejected — both
  are fail-safe (no panic) but the invalid-PRI case should be counted
  via a metric for visibility.

## SYS-06 UDP truncation — GAP (unverified bound)

- UDP path uses a single `recv_from` into a fixed buffer
  (`run_udp`, `src/signals/syslog.rs:239`). The buffer size was not
  confirmed in this pass; if it is smaller than `max_frame_bytes`,
  oversized datagrams are silently truncated by the kernel with no
  truncation flag check — a real evidence-loss path for burst senders.
- **Action:** verify buffer size ≥ max_frame_bytes and/or check the
  `MSG_TRUNC` equivalent; syslog has no truncation marker, so the only
  honest signal is a metric/counter. Backlog item.

## SYS-07 TLS — GAP (out of current scope)

- The syslog receiver binds plain TCP/UDP only; no TLS/rustls code
  exists in `src/signals/syslog.rs`. RFC 5425 (TLS syslog) is **not**
  implemented. TLS exists elsewhere (`RuntimeTlsConfig` for the
  collector→core channel, `HttpTlsConfig` for the API) but does not
  cover the syslog hop.
- **Classification:** deliberate scope boundary for the current
  release; enterprise deployments terminating TLS at a collector
  forward plain syslog internally. Backlog item if RFC 5425 becomes a
  requirement — prefer stunnel/envoy sidecar over in-tree TLS to keep
  the receiver small.

## SYS-08 Pattern hot reload — PASS

- `run_udp`/`run_tcp` watch a `watch` channel for pattern changes and
  log "hot-reloaded pattern extractor" (`src/signals/syslog.rs:272`).
  The fact extractor is rebuilt without restarting the receiver.
- Shun rules reload via `ShunEngine::reload` (`src/shun.rs:215`)
  after DB writes through `/api/shun/*` (`src/http_server/shun.rs`).

## SYS-09 Archive durability — PASS with caveats

- `SyslogArchive::append` (`src/signals/syslog.rs:833`) writes the
  JSONL line then awaits `file.flush()` — the comment correctly notes
  `write_all` only queues bytes; flush drains to the OS. Returned `Ok`
  means the line reached the file.
- **Caveat 1:** `flush()` is *not* `sync_all()`/fsync — a host crash
  can still lose the last lines in page cache. For a forensic archive
  this is a real but bounded durability gap; per-record fsync would
  be too costly at high EPS — recommend periodic fsync or group-commit
  as a backlog item, not a blocker.
- **Caveat 2:** no rotation/size cap on `runtime/signals/syslog.jsonl`
  — unbounded growth on long-running deployments. Backlog item tied to
  the retention matrix (see resource/durability review).
- **Caveat 3:** archive write failure is counted
  (`bonsai_syslog_archive_errors_total`) and logged but does NOT block
  bus publish — evidence ordering is preserved (archive attempted
  first) but a persistently failing archive silently degrades to
  bus-only delivery. Operator-visible via metric; acceptable, document.

## Suppression vs evidence deletion (explicit trace)

Order inside `handle_frame` (`src/signals/syslog.rs:521`):

1. `archive.append` — **always first**, before any suppression.
2. `SYSLOG_NOISE_APPS` (`sr_grpc_server`) → return. Archive retained.
3. `ShunEngine::evaluate` → `Dropped`/`RateLimited` → return.
   Archive retained; metrics `bonsai_syslog_shunned_total{action}`.
4. Governor `should_shed()` → return before `bus.publish`.
   Archive retained; metric `bonsai_syslog_shed_total`.

**Conclusion:** UI/API ignore-and-shun rules suppress *downstream*
processing (graph projection, fact extraction, enrichment) but never
delete archived raw evidence. This is the correct evidence-engine
posture: suppression ≠ deletion. The only true evidence-loss paths are
archive write failure (Caveat 3) and UDP truncation (SYS-06).

Shun rule flow: `POST /api/shun/rules` → DB persist →
`ShunEngine::reload` → runtime `evaluate(scope, message, fact_types)`
→ drop/rate_limit/pass. Scope is the unique candidate device address
or the provisional `syslogsrc/...` source key — never a canonical
identity (consistent with BN-021e source isolation).

## Standalone vs distributed collector

- The syslog receiver runs in the core `signals` path; the collector
  (`src/collector/graph.rs`, `diagnostic_server.rs`) maintains a
  *reduced local* graph — it is not a syslog replica. In distributed
  mode the same `handle_frame` path applies; the provisional source
  key (`syslogsrc/{collector}/{transport}/{peer}`) carries collector
  identity end-to-end, so collector-vs-core attribution survives.
- No separate collector-side syslog archive duplication was found —
  archive path is a single configured JSONL.

## Backlog items generated (fold into existing BN owners)

| ID | Item | Owner |
|---|---|---|
| SYS-A | RFC 5424 STRUCTURED-DATA bounded capture + sender TIMESTAMP provenance field | BN-016 |
| SYS-B | PRI range validation + invalid-PRI metric | BN-016 |
| SYS-C | UDP buffer/truncation verification + metric | BN-016 |
| SYS-D | TCP idle timeout for syslog connections | BN-009 |
| SYS-E | Archive rotation + periodic fsync policy | BN-008/BN-019 |
| SYS-F | RFC 5425 TLS termination decision (sidecar preferred) | BN-005 |
