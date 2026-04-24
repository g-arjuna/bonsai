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
| `error` | `string` | Error message if processing failed. |
| `protocol_version`| `uint32` | Version of the protocol used by the core. |

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
