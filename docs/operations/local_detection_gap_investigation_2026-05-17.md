# Local Detection Gap Investigation - 2026-05-17

## Summary

This investigation explains why local chaos injection was active, telemetry was
flowing, archive files were being written, but local detections were mostly
missing during 30-day run bring-up on the Ubuntu laptop.

The root cause was a broken effective runtime implementation of
`BonsaiClient.device_vendor()` in `python/bonsai_sdk/client.py`. Vendor-aware
rules were resolving `vendor=""`, which suppressed detections such as
`interface_down`, `bfd_session_down`, and `bgp_session_down`.

## Symptoms

- `bash scripts/ops/start_30day_run.sh --status` showed archive activity but no
  useful local detections.
- `bash scripts/chaos_runner.sh` was actively injecting real faults.
- `/api/sidecars` showed `events_in_total` increasing.
- `/api/detections` either stayed empty or only showed unrelated detections like
  `bgp_never_established`.

## Proof chaos and telemetry were healthy

The issue was not "chaos is broken" and not "telemetry is missing".

Observed during triage:

- live chaos injected faults like:
  - `interface_shut`
  - `bfd_session_down`
  - `bgp_session_down`
- Bonsai archive continued writing parquet row groups under `runtime/archive`
- gRPC `StreamEvents()` showed real state events, including interface down/up
  transitions from the lab devices

That narrowed the gap to the sidecar rules path rather than the lab, archive,
or subscriber path.

## Root cause

`python/bonsai_sdk/client.py` contained two `device_vendor()` implementations.

The later definition was taking precedence at runtime and attempted:

- `GET /api/devices/{address}`

That path returned an empty vendor for device addresses like
`172.100.103.13:57400`.

By contrast, the graph query path returned the correct vendor:

- `nokia_srl`

Vendor-aware rules depend on correct state mapping, so the empty vendor caused
rules like:

- `interface_down`
- `bfd_session_down`
- `bgp_session_down`

to be suppressed even though the triggering events were real.

## Fix

Permanent fix:

- remove the duplicate `device_vendor()` override
- resolve vendor via a graph query instead of the broken HTTP path

The same investigation showed `device_rack()` should also use the graph query
helper for address-based device lookups.

## Operational workaround used during investigation

Before the permanent repo fix was committed, a temporary runtime-only patched
sidecar wrapper was used from `/tmp/patched_sidecar.py` to monkeypatch
`BonsaiClient.device_vendor()`.

That workaround should not be relied on long-term. It exists only to explain
how detections were restored before the source fix landed.

## Validation after fix

After the corrected vendor lookup path was used, `/api/detections` again showed
vendor-sensitive detections with `vendor="nokia_srl"`, including:

- `interface_down`
- `bfd_session_down`
- `bgp_session_down`

## Remaining note

During triage, `/api/sidecars` sometimes showed `detections_out_total=0` even
while `/api/detections` contained new detections. That appears to be a separate
counter/reporting issue and not the root cause of the local no-detection gap.
