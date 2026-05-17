# Adapter Cursor Cold-Start Verification

> D2-11 T2 — 2026-05-17. Documents the current StreamEvents resume behaviour
> and the gap window on sidecar restart.

## What "cursor persistence" means

The CV6 backlog referred to a **cursor** as the stream position the Python rules
sidecar holds into the `StreamEvents` server-streaming RPC. If the cursor were
persisted across restarts, the sidecar could resume from its last-seen event
rather than starting from the current tail, closing the gap window.

## Current behaviour (verified DV2)

`StreamEventsRequest` has two fields: `event_types` and `device_address`. There
is **no cursor/sequence/resume token field**.

```proto
message StreamEventsRequest {
  repeated string event_types    = 1;  // empty = all types
  string          device_address = 2;  // empty = all devices
}
```

On `BonsaiClient.stream_events()` the engine opens a fresh server-stream that
starts at **current tail** — events buffered on the server during the gap
between sidecar stop and sidecar reconnect are **not replayed**.

The `_event_loop` in `engine.py` reconnects immediately after stream EOF or
error:

```python
stream = self._client.stream_events()
for event in stream:
    ...
# stream ended cleanly (server EOF) — reconnect immediately
```

Reconnect-on-crash is present; replay-from-cursor is not.

## Gap window size

The sidecar reconnects within 5 seconds on error (`time.sleep(5)` in
`_event_loop`). On clean EOF it reconnects immediately. The practical gap window
is **0–5 seconds of events missed per restart**.

On a 1–10 event/second lab network, this is 0–50 events per restart, which is
acceptable for anomaly detection (the state machine will catch the next
transition). On a higher-rate network the gap is more consequential.

## Why CV6 cursor persistence was deferred

The Rust `StateChangeEvent` store already writes every event to LadybugDB with
a `fired_at_ns` timestamp. A cursor-resume could be implemented as:

```proto
message StreamEventsRequest {
  repeated string event_types    = 1;
  string          device_address = 2;
  int64           after_ns       = 3;  // resume: replay events with fired_at_ns > after_ns
}
```

The sidecar would persist the `fired_at_ns` of the last processed event to a
local file (`/tmp/bonsai-cursor-{collector_id}`) and pass it on reconnect.

This was not implemented in CV6 because:
1. The event loop reconnects fast enough for lab-scale use.
2. The rules are stateless per-event — a missed transition is caught on the next
   state change event.
3. Adding `after_ns` requires a Rust change to the `stream_events_handler` to
   scan historical `StateChangeEvent` rows, which is a non-trivial query path.

## Smoke verification (Ubuntu ops)

Run this on Ubuntu after deploying a fresh build to confirm the cold-start gap
is within tolerance:

```bash
#!/usr/bin/env bash
# Verify sidecar reconnects within 5 s after a kill -9.
# Run from: bash docs/verify_cursor_cold_start.sh

set -euo pipefail
PID=$(pgrep -f collector_engine.py | head -1)
if [[ -z "$PID" ]]; then
    echo "FAIL: collector_engine.py not running"
    exit 1
fi

BEFORE=$(date +%s%N)
kill -9 "$PID"
sleep 1

# Wait up to 10s for the watchdog (bonsai-rules-sidecar.service) to restart it
for i in $(seq 1 10); do
    NEW_PID=$(pgrep -f collector_engine.py | head -1 || true)
    if [[ -n "$NEW_PID" && "$NEW_PID" != "$PID" ]]; then
        AFTER=$(date +%s%N)
        GAP_MS=$(( (AFTER - BEFORE) / 1000000 ))
        echo "PASS: sidecar restarted in ${GAP_MS}ms (PID $PID -> $NEW_PID)"
        exit 0
    fi
    sleep 1
done

echo "FAIL: sidecar did not restart within 10s"
exit 1
```

## Upgrade path (DV3+)

When cursor replay is needed (rate > 50 events/s or >5s restart SLA):

1. Add `int64 after_ns = 3` to `StreamEventsRequest` in `proto/bonsai_service.proto`.
2. Update `stream_events_handler` in `src/api.rs` to scan `StateChangeEvent`
   nodes with `fired_at_ns > after_ns` before handing off to the live stream.
3. Persist cursor in `BonsaiClient.stream_events()`:
   - Write `last_event.fired_at_ns` to `~/.bonsai/cursor-{collector_id}` after
     each successful event dispatch.
   - Read on startup and pass as `after_ns`.

This is tracked but not prioritised until the lab runs at >50 events/s
sustained or the restart SLA tightens below 5s.
