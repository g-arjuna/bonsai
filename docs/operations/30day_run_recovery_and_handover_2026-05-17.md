# 30-Day Run Recovery And Handover - 2026-05-17

## Scope

This note captures the operational findings from the 30-day run cleanup and
redeploy work on:

- local Ubuntu laptop
- Oracle cloud instance

It is intended to prevent repeat investigation during future cleanup/redeploy
cycles.

## Local final standing

Local runtime was stabilized successfully.

Healthy state observed:

- `/health` returned `{"status":"ok"}`
- topology showed `devices=8 healthy=8 links=82`
- archive writes resumed under `runtime/archive`
- chaos was actively injecting faults
- vendor-sensitive detections returned after the client fix

## Local startup findings

### `start_30day_run.sh` chaos branch was broken

The script attempted to launch:

- `tests/chaos_harness/run.py --plan ... --duration ...`

That command failed because the harness did not support those arguments.

The supported laptop chaos path is:

- `scripts/chaos_runner.sh`

The startup script was updated to use that supported path and to stop any
existing `chaos_runner.sh` daemon during restart/cleanup flows.

### Persistent local services that worked

During recovery, the following user services proved to be a stable way to keep
the laptop runtime anchored:

- `bonsai30-local-core.service`
- `bonsai30-local-sidecar.service`
- `bonsai30-local-chaos.service`

Operational status commands:

```bash
systemctl --user status bonsai30-local-core bonsai30-local-sidecar bonsai30-local-chaos
curl http://127.0.0.1:3000/health
curl http://127.0.0.1:3000/api/sidecars
curl http://127.0.0.1:3000/api/detections
```

Operational stop command:

```bash
systemctl --user stop bonsai30-local-chaos bonsai30-local-sidecar bonsai30-local-core
```

## Local code handoff items

These are the changes that were required to stop rediscovering the same issues:

1. Fix `python/bonsai_sdk/client.py`
   - remove the duplicate `device_vendor()` definition
   - use graph query lookup for device vendor
   - use the same graph query helper for `device_rack()`

2. Fix `scripts/ops/start_30day_run.sh`
   - stop invoking the unsupported chaos harness arguments
   - start the supported chaos daemon via `scripts/chaos_runner.sh`

3. Fix `src/streaming/mod.rs` test initializer
   - add missing `netflow` and `otlp` fields to the `StreamingConfig` test
     initializer

## Cloud findings

Cloud was intentionally left clean and stopped because the operator did not
want an older Bonsai binary running there.

Observed blockers on the then-current cloud image:

- OS/image was Oracle Linux 8.10
- toolchain was GCC 8.5
- rebuilding Bonsai from source failed in `lbug` because `arm_sve.h` was
  missing

That meant:

- current source could not be rebuilt cleanly there
- only an older prebuilt Bonsai binary was runnable
- that older binary must not be used for the 30-day run

## Recommended zero-cost OCI rebuild path

To stay within Oracle Always Free and avoid hidden cost:

1. terminate the current compute instance when no longer needed
2. delete unattached boot volumes or block volumes only
3. delete any unused custom images or snapshots tied to the old instance
4. create a single Ubuntu ARM Always Free instance
5. avoid multiple instances, extra block volumes, and stray reserved IP usage

## Validation artifact from this cycle

Generated validation note already present:

- `docs/test_results/cv7-validation-2026-05-17T1100Z.md`

Important result from that run:

- `cargo build --release` passed
- `cargo test --release -p bonsai sidecar_registry` failed before the
  `StreamingConfig` test initializer fix

## Recommended next operator flow

For the next clean local 30-day bring-up:

1. clean runtime and lab state
2. redeploy the DC lab
3. start Bonsai with `bash scripts/ops/start_30day_run.sh`
4. verify `/health`, `/api/sidecars`, `/api/detections`, and archive growth
5. only attempt cloud after confirming the cloud VM is a clean Ubuntu
   Always Free instance that can build current source
