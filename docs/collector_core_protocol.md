# Collector-Core Protocol Contract

This document defines the versioned gRPC contract between Bonsai Collectors and the Bonsai Core.

## Protocol Versioning

The protocol uses a single monotonic integer `protocol_version` field in key ingest messages.

- **Current Version**: 1
- **Compatibility Policy**:
  - **Minor Skew**: Core logs a warning if a collector's version differs but is within the same major version (currently all v1).
  - **Major Skew**: Core may reject connections if the version is incompatible.

## RPC Surface

### `TelemetryIngest(stream TelemetryIngestUpdate) -> TelemetryIngestResponse`

Used by collectors to push decoded telemetry events to the core.

#### `TelemetryIngestUpdate`
| Field | Type | Description |
|---|---|---|
| `collector_id` | `string` | Stable identity of the collector. |
| `target` | `string` | IP:Port of the network device. |
| `vendor` | `string` | Device vendor (e.g., `nokia_srl`, `cisco_xr`). |
| `hostname` | `string` | Hostname of the device. |
| `timestamp_ns` | `int64` | Time of observation at the collector. |
| `path` | `string` | Normalized gNMI path. |
| `value_msgpack` | `bytes` | MessagePack encoded telemetry value (used in `raw` and `debounced` modes). |
| `protocol_version`| `uint32` | Version of the protocol used by the collector. |
| `interface_summary`| `InterfaceSummary` | Aggregated counter stats (used in `summary` mode). |

#### `TelemetryIngestResponse`
| Field | Type | Description |
|---|---|---|
| `accepted` | `uint64` | Number of records successfully processed. |
| `rejected` | `uint64` | Number of records **permanently** rejected (authorization denial, malformed payload, protocol major skew). |
| `error` | `string` | Error message if processing failed. |
| `protocol_version`| `uint32` | Version of the protocol used by the core. |

#### Acknowledgement contract (BN-021g H2)

The core consumes the **entire** client stream before sending
`TelemetryIngestResponse`. Every submitted record ends in exactly one
of two outcomes:

- **accepted** — decoded, authorized and published to the event bus;
- **rejected** — permanently refused (per-record authorization denial,
  undecodable payload, protocol major skew). Rejected records are
  *retired* by the collector: they are never replayed.

Therefore `accepted + rejected == records submitted` and the collector
queue may acknowledge (retire) the whole batch once the response
arrives. A response is only sent after the full stream is consumed —
if the RPC errors instead (network failure, store outage, retryable
internal error), **no** acknowledgement is written and the entire
batch is retained for at-least-once replay.

Caveats:

- "Accepted" means published to the in-memory event bus, not a
  durably committed graph write — at-least-once delivery, never
  exactly-once.
- Rejection is per-record and interleaving-safe: a denied record never
  blocks later legitimate records in the same batch.
- Protocol-version skew is verified on **every** record, not just the
  first: `check_protocol_compat` classifies equal versions as
  `Compatible`, non-equal same-major versions as `MinorSkew` (logged,
  record processed) and difference ≥10 or version 0 as `MajorSkew`
  (record permanently rejected). A major-skew record arriving after
  compatible records in the same stream is still rejected.
- The collector validates the response before acknowledging: the
  response `protocol_version` must not be major-skewed, and
  `accepted + rejected` must exactly equal the submitted record count
  (checked arithmetic — impossible or inconsistent accounting is a
  protocol violation). On any violation the collector writes **no**
  acknowledgement and retains the whole batch for replay — loud
  failure, never silent record loss.
- Cores predating `rejected` (field absent → 0) still consumed the
  whole stream and had no permanent per-record rejections, so their
  `accepted` count equals the submitted count and the strict equality
  still holds — old cores remain compatible.

## Forwarding Modes

The collector can be configured in one of three forwarding modes via `[collector.filter.counter_forward_mode]`:

### 1. `raw`
Every decoded gNMI update is forwarded immediately to the core. No filtering is performed.

### 2. `debounced` (Default)
Counter updates (Interface statistics) are filtered at the collector side. If multiple updates for the same (device, interface) arrive within the `counter_debounce_secs` window, only the first one is forwarded; others are dropped.

### 3. `summary`
Counter updates are aggregated into 60-second UTC-aligned windows. A single `InterfaceSummary` message is sent per interface per minute, containing:
- `min`, `max`, `mean` for each counter.
- `delta` (difference between last and first value in the window).

Non-counter events (e.g., BGP state changes, interface oper-status) are always forwarded immediately regardless of the mode.

## M6E — Authenticated remote YANG discovery

Two additive RPCs on the collector→Core channel carry bounded,
Core-issued YANG discovery jobs. They reuse the same mTLS/token
authenticated session as registration, heartbeat and telemetry; there
is no inbound collector management server.

### Capability advertisement

`CollectorIdentity.features` (repeated string) advertises explicit
capabilities. Remote discovery requires BOTH
`protocol_version >= 6` (`YANG_DISCOVERY_CONTRACT_VERSION`) AND the
`yang_discovery_v1` feature flag on the collector's **active** session.
A session lacking either receives `discovery_supported=false` and zero
executable jobs — a pre-contract collector can never be handed work it
cannot understand.

### `PollYangDiscoveryJobs(PollYangDiscoveryJobsRequest) -> PollYangDiscoveryJobsResponse`

Collector-initiated bounded poll (the collector drives; Core never
pushes jobs into the assignment stream, so subscription lifecycle is
untouched). Requires a transport-attested, credential-bound collector
principal — a shared PSK alone never authorizes credential-bearing
remote discovery.

Request: `collector_id`, `protocol_version`.
Response: `jobs` (bounded batch, stamped with the current
`collector_session_generation`, `authority_epoch`,
`assignment_generation`, device identity, datastore, deadline),
`cancelled_job_ids`, `discovery_supported`.

Core-side poll semantics: lazily expires deadline-passed jobs,
cancels jobs whose device binding is gone (`device_removed`) or whose
authority was superseded (`device_binding_changed` →
`stale_authority`), and re-delivers `dispatched` jobs under a NEW
session generation after reconnect. Duplicate delivery is expected
and safe.

### `SubmitYangDiscoveryResult(YangDiscoveryResult) -> YangDiscoveryResultAck`

Submits bounded discovery evidence (or an explicit job-level failure
outcome). Core validates, in order: authenticated principal ==
claimed `collector_id` == job's assigned collector; job exists,
pending and within deadline; session generation, authority epoch and
assignment generation still current; canonical device identity and
current assignment; protocol compatibility; payload size and JSON
structural validity; observation-time bounds; out-of-order fencing
(older jobs never supersede newer accepted evidence).

Only then does Core deserialize the evidence, load the verified active
catalogue, resolve effective applicability with the M6C-1 resolver and
persist a normal snapshot — identical to the Core-direct path.

Ack: `accepted`, `reason` (machine-readable rejection/failure
classification), `snapshot_id`, `duplicate` (idempotent replay of an
identical completed result).

### Evidence contents and limits

`capabilities_json` / `yang_library_json` carry the serialized
`CapabilityEvidence` / `YangLibraryOutcome` produced by the same
acquisition primitives as Core-direct discovery. Bounds:
`YANG_DISCOVERY_MAX_EVIDENCE_BYTES` (4 MiB per payload),
`YANG_DISCOVERY_MAX_DIAGNOSTICS` (64), module/module-set/recursion
bounds inside the shared parser, per-collector (8), per-device (2) and
global (256) active-job caps, 8-job poll batch, 90 s collector-side
execution budget inside the durable job deadline (default 120 s,
max 600 s). Oversized payloads are rejected outright — never truncated
into apparently complete evidence.

### What the collector must never do

No gNMI Set, no Subscribe changes, no subscription restarts, no
approval of collection plans, no authoritative path decisions, no
arbitrary network targets, no credentials in job payloads, evidence,
snapshots, logs or durable job rows. A hung device is isolated in a
bounded worker (max 2 concurrent per collector) and cannot block
heartbeats, assignment updates, withdrawal or shutdown.
