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
