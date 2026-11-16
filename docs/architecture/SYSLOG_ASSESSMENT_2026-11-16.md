# Syslog Receiver Assessment (SYS-01…SYS-09)

Source-audited at `feat/wp65a-template-dedup` merged with `origin/main`
(candidate SHA `fe4071e` + merge `bfea68b`; receiver code unchanged by
WP6.5A). Scope: `src/signals/syslog.rs`, `src/shun.rs`,
`src/http_server/shun.rs`, `src/config.rs`, `src/collector/`.
Static source review only — no live receiver run.

Corrected 2026-11-16 (closeout review): SYS-01 reclassified — the
newline-framing path is a confirmed P0 availability defect, not a
bounded PASS. SYS-02, SYS-03, SYS-04, SYS-06, SYS-08 and SYS-09
updated against source (`run_tcp_newline`, `run_tcp`,
`run_udp`, `parse_rfc5424`, `parse_legacy`, `SyslogArchive::append`).

Bonsai is a network context/evidence engine, not a SIEM or a general
syslog collector. Findings are classified: **defect** (correctness/
durability bug), **limitation** (deliberate or accepted scope boundary),
**gap** (missing capability worth a backlog item).

## SYS-01 Bounded TCP framing — SPLIT VERDICT

- **Octet-counting (RFC 6587) — PASS.** `run_tcp_octet_counting`
  (`src/signals/syslog.rs:440`) caps the length prefix at 7 digits,
  enforces `msg_len <= max_frame_bytes` (`syslog.max_frame_bytes`,
  default 8192, `src/config.rs:537`) *before* allocating `msg_buf`,
  and closes the connection on malformed/oversized frames —
  fail-closed, no unbounded allocation.
- **Newline framing — confirmed P0 availability defect.**
  `run_tcp_newline` (`src/signals/syslog.rs:395-434`) calls
  `reader.read_line(&mut line)` which appends to `line` until a `\n`
  or EOF with **no bound during accumulation**; the
  `n > max_frame_bytes` check runs only *after* the entire
  unterminated line has been read into memory. A sender that never
  emits a delimiter (or emits one after gigabytes) causes unbounded
  memory growth per connection — and with `MAX_TCP_CONNECTIONS`
  (SYS-02) such senders can also exhaust the connection pool.
  **Required fix (next small runtime slice, post-WP6.5A):** bounded
  incremental framing — accumulate with an early-abort cap at
  `max_frame_bytes` (e.g. `take`/`read_until` on a bounded buffer or
  chunked `fill_buf` scanning), closing the connection on overflow
  rather than completing the read. Regression tests: oversized
  frames, fragmented frames, valid frames following invalid input
  where the framing contract permits resync, and both framing modes.
- Dual-mode framing: connections are probed for octet-counting vs
  newline framing (`src/signals/syslog.rs:363-366`) by peeking the
  first byte — a digit selects octet-counting.
- **Limitation:** on any framing error the whole TCP connection is
  dropped rather than resynchronized — acceptable for evidence
  collection (sender retransmits/next connection), but worth noting
  for chatty senders.

## SYS-02 Connection admission and idle handling — DEFECT (availability)

- There is a **finite** limit: `MAX_TCP_CONNECTIONS = 128`
  (`src/signals/syslog.rs:305`); excess accepts are rejected with
  `bonsai_syslog_tcp_rejected_total` and `conn_count` is decremented
  on task exit. The defect is not unbounded task count — it is that
  **idle or slow connections can consume the entire 128-slot pool**
  and starve legitimate senders.
- **Gap (confirmed):** no initial-read deadline (a connection that
  sends nothing holds a slot forever), no established-frame/idle
  deadline (a stalled mid-frame sender holds a slot indefinitely),
  and no cancellation of in-flight connection tasks on shutdown —
  the accept loop stops but spawned readers persist until EOF/error.
- **Required fix (same slice as SYS-01):** bounded initial-read and
  per-frame/idle deadlines, `tokio::select!` cancellation on the
  shutdown signal inside connection tasks, and orderly drain.
  Regression tests: connection-slot recovery after idle/slow-sender
  eviction, slow-sender behavior under both framing modes.

## SYS-03 RFC 5424 structured data — DEFECT (MSG-boundary corruption)

- `parse_rfc5424` (`src/signals/syslog.rs:1161`) uses
  `rest.splitn(8, ' ')`. STRUCTURED-DATA is not a single space-free
  token: SD elements may contain spaces inside quoted parameter
  values and multiple SD elements are space-separated. With
  `splitn(8)`, an SD element containing spaces is split across
  `parts[6]`/`parts[7]`; because `parts[6]` starts with `[`, the
  parser takes `parts[7]` — the *remainder of the SD element plus the
  real MSG* — as the message. **Result: MSG-boundary corruption, not
  merely discarded structured data** — SD parameter text (including
  the closing `]`) leaks into `event.message` and `event.raw`
  downstream consumers see a corrupted message.
- Additional gaps: multiple SD elements, escaped quotes (`\"`, `\]`,
  `\\` per RFC 5424 §6.3.3), and PARAM-NAME=VALUE validation are all
  unhandled; NILVALUE normalization applies only to HEADER fields.
- `parse_legacy` fallback on malformed 5424 input can further
  misattribute fields (hostname/message boundary guessed from token
  count).
- **Backlog item (unchanged owner BN-016):** real SD parsing —
  bounded element/param capture into the event record plus correct
  MSG extraction. `raw` is archived verbatim so evidence is
  recoverable, but parsed fields are currently unreliable for
  SD-bearing messages. BOM (`\ufeff`) per RFC 5424 §6.1 is also not
  stripped.

## SYS-04 Timestamp provenance — LIMITATION (explicit, refined)

- `SyslogEvent.timestamp_ns` is always the *receiver* clock
  (`now_ns()` at ingest). The RFC 5424 TIMESTAMP header field is read
  but discarded (`let _timestamp = parts.next()?`,
  `src/signals/syslog.rs:1167`).
- RFC 3164 timestamps (`Mon dd hh:mm:ss`) carry **no year and no
  timezone** — they cannot yield a trustworthy absolute sender time
  without assumptions (current-year guess, local-TZ guess). The
  correct posture is to record that sender time is *unavailable*
  (or only partially recoverable with explicit uncertainty), never
  to fabricate an absolute timestamp.
- Sender time, receive time and persistence time must remain
  separately identifiable fields with explicit provenance
  (`sender|receiver|none` + quality). Today only receiver time and
  the raw string exist; persistence (archive write) time is implicit
  in file order. Backlog item unchanged: optional
  `sender_timestamp_ns` + `timestamp_provenance` — consistent with
  the "never fabricate" rule.

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

## SYS-06 UDP truncation — CONFIRMED GAP (exact bound now verified)

- The UDP receive buffer is **exactly `max_frame_bytes`**
  (`vec![0_u8; max_frame_bytes.max(1)]`,
  `src/signals/syslog.rs:253`; default 8192). `recv_from` on a
  datagram larger than the buffer discards the excess and returns
  `n = buf.len()` — tokio does not surface a truncation flag, so an
  oversized datagram is **silently accepted as a complete frame** and
  parsed/archived as if whole. This is an evidence-integrity defect:
  truncated input is not distinguishable downstream.
- **Required (backlog, small):** explicit truncation detection and
  accounting — e.g. receive with `MSG_TRUNC` semantics (socket2 /
  raw fd) or treat `n == buf.len()` as suspect +
  `bonsai_syslog_udp_truncated_total` metric and an explicit
  `truncated` marker on the event rather than accepting the partial
  payload as complete evidence.

## SYS-07 TLS — GAP (out of current scope; attribution caveat added)

- The syslog receiver binds plain TCP/UDP only; no TLS/rustls code
  exists in `src/signals/syslog.rs`. RFC 5425 (TLS syslog) is **not**
  implemented. `RuntimeTlsConfig` covers the collector→core channel
  and `HttpTlsConfig` the API — **neither secures the
  device→collector syslog hop**; terminating TLS at a relay does not
  make the plaintext leg trustworthy.
- **Attribution caveat:** any TLS-terminating sidecar must preserve
  trustworthy exporter attribution — the relay's source IP and any
  self-claimed hostname are *not* canonical device identity (the
  existing provisional `syslogsrc/...` key and candidate-hostname
  model already encode this; a sidecar must not upgrade claimed
  identity to verified).
- **Classification:** deliberate scope boundary for the current
  release; RFC 5425 remains a separately designed integration
  (SYS-F), not assumed covered by collector→core TLS.

## SYS-08 Pattern hot reload — PARTIAL (transport-inconsistent)

- **UDP:** `run_udp` selects on `pattern_rx` and hot-reloads the
  extractor without restart (`src/signals/syslog.rs:269-273`).
- **TCP:** `run_tcp` receives no `pattern_rx`; each accepted
  connection clones the extractor captured at listener start
  (`src/signals/syslog.rs:346`). Extractor updates never reach TCP
  connections — including long-lived ones — until process restart.
- **Defect (contract inconsistency):** identical operator action
  (update patterns) produces different behavior per transport. Fix:
  pass the watch channel into `run_tcp` and re-resolve per frame (or
  per connection) so both transports share one update contract.

## SYS-09 Archive durability — PASS with classified caveats

`handle_frame` order (`src/signals/syslog.rs:531-618`): archive
append is **always first**, before noise suppression, shun
evaluation and governor shedding. Explicit stage classification:

| Stage | Behavior | Evidence state |
|---|---|---|
| Archive attempted | `SyslogArchive::append` serializes the event and `write_all`s one JSONL line | attempted |
| OS-buffered write | `file.flush()` drains tokio's buffer to the OS — returned `Ok` means the line reached the file, **not** that it is durable | OS-buffered |
| Durable write | **No fsync/`sync_all`** — a host crash can lose the tail in page cache | not guaranteed |
| Downstream suppression | `SYSLOG_NOISE_APPS`, `ShunEngine` drop/rate-limit, governor `should_shed()` all return *after* archive | suppressed downstream only |
| Queue loss | shed path returns before `bus.publish`; metric `bonsai_syslog_shed_total` | gap is metric-visible, not a graph `completeness_state` marker |
| Truncation | UDP oversized datagrams accepted truncated (SYS-06); newline oversize drops the connection | silent today |
| Invalid decoding | `from_utf8_lossy` + `trim_end` — **the archived `raw` is NOT byte-exact**: invalid UTF-8 is replaced and trailing whitespace trimmed before archival | do not claim byte-exact archival |
| Restart/shutdown gaps | append-mode reopen can leave a torn last line after crash; in-flight TCP frames are not drained on shutdown | bounded, readers must tolerate torn tail |

- Archive write failure is counted
  (`bonsai_syslog_archive_errors_total`) and logged but does NOT block
  bus publish — a persistently failing archive silently degrades to
  bus-only delivery. Operator-visible via metric; acceptable,
  documented.
- **Do not assume TCP application-level retransmission** — framing
  errors drop the connection; resend behavior is sender-dependent.
- Caveats retained: no rotation/size cap on
  `runtime/signals/syslog.jsonl` (unbounded growth — backlog SYS-E);
  periodic fsync/group-commit recommended over per-record fsync.

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
delete archived raw evidence — with the SYS-09 caveats above (lossy
UTF-8/trim means "archived" ≠ byte-exact; flush ≠ fsync; UDP
truncation and archive failure are real loss paths). This is the
correct evidence-engine posture: suppression ≠ deletion.

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
| SYS-A | Bounded RFC 5424 STRUCTURED-DATA parsing (fix MSG-boundary corruption) + sender TIMESTAMP provenance field with RFC 3164 year/TZ uncertainty | BN-016 |
| SYS-B | PRI range validation + invalid-PRI metric | BN-016 |
| SYS-C | UDP truncation detection (`MSG_TRUNC`/suspect-length) + metric + explicit truncated marker | BN-016 |
| SYS-D | **P0:** bounded incremental newline framing + TCP initial-read/frame/idle deadlines + shutdown cancellation (with SYS-01) | BN-009/BN-016 |
| SYS-E | Archive rotation + periodic fsync policy + torn-tail tolerance documentation | BN-008/BN-019 |
| SYS-F | RFC 5425 TLS termination decision (sidecar preferred; preserve exporter attribution — relay IP/claimed hostname is not canonical identity) | BN-005 |
| SYS-G | TCP extractor hot-reload parity with UDP (shared update contract) | BN-016 |
| SYS-H | Shed/gap marker event so downstream sees archive-retained-but-unpublished holes as explicit `completeness_state` gaps | BN-008 |
