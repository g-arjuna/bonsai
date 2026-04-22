# Archive Format

Bonsai's Parquet archive is an operator-facing copy of the telemetry stream.
It is meant for offline inspection, training-data export, and later graph
embedding/GNN work. In distributed mode the archive is collector-local: each
collector writes the updates it receives on its own event bus.

## Layout

Archive files are partitioned by UTC day and target. Each collector keeps one
open Parquet writer per `(target, hour)` and appends flushes as row groups:

```text
archive/
  2026/
    04/
      22/
        target=172_100_102_12_57400__hour=09.parquet
        target=172_100_102_12_57400__hour=09__part-01.parquet
```

The `__part-NN` suffix appears when Bonsai restarts during the same hour. It
prevents overwriting a closed Parquet file, because Parquet cannot be safely
appended after its footer is written.

## Schema

| Column | Type | Meaning |
| --- | --- | --- |
| `timestamp_ns` | `int64` | Telemetry timestamp in Unix nanoseconds |
| `target` | `string` | gNMI target address, usually `host:port` |
| `vendor` | `string` | Vendor label detected from Capabilities or configured hint |
| `hostname` | `string` | Operator-provided hostname, when known |
| `path` | `string` | Full gNMI path for the update |
| `value` | `string` | JSON-serialized telemetry value |
| `event_type` | `string` | Bonsai classifier output, such as `interface_stats` |

The ingest wire format uses MessagePack for `TelemetryIngestUpdate.value_msgpack`
to reduce collector-to-core payload size. The archive intentionally stores
`value` as JSON text instead. That choice makes Parquet files easy to inspect
with pandas, DuckDB, and command-line tools without requiring Bonsai's protobuf
or MessagePack code.

## Reading With Pandas

```python
import json
import pandas as pd

df = pd.read_parquet("archive")
df["value_obj"] = df["value"].map(json.loads)

interfaces = df[df["event_type"] == "interface_stats"]
print(interfaces[["timestamp_ns", "target", "path", "value_obj"]].head())
```

## Reading With DuckDB

```sql
SELECT
  timestamp_ns,
  target,
  path,
  json_extract(value, '$.in-octets') AS in_octets
FROM read_parquet('archive/**/*.parquet')
WHERE event_type = 'interface_stats';
```

Column names inside `value` follow the vendor/gNMI payload shape Bonsai
received. Consumers should tolerate missing keys and vendor-specific spelling.
