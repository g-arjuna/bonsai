# M6F-C Stage G — SR Linux operational resilience (run g1)

Bounded failure-injection run against the **live M6F production chain**
(Core `m6f-bonsai-core` + collector `m6f-bonsai-collector` + Nokia SR
Linux `clab-m6f-srlinux-srlinux` 26.3.1-410, gNMI
`172.100.102.11:57400` tls_verify) using the same activated plan as the
r8 regression: plan `53a64d7c6aebc133`, path
`/interface[name=mgmt0]/statistics`, SAMPLE 10 s, `acquisition=explicit`
with schema-bound plan authority.

All times UTC 2026-11-22 (directory named by local date).

## Scenario results

| Scenario | Action | Result | Evidence |
|---|---|---|---|
| Baseline | plan active, telemetry `observed`, timestamps advancing | PASS | `api/31-telemetry-observed.json`, `api/00-summary.json` |
| Collector restart | `docker restart m6f-bonsai-collector` @10:50:05 | PASS — collector reconnected to core (mTLS), assignment sync gen 13, subscriber restarted and re-subscribed the SAME plan_id; new session (`first_observed_at_ns` reset), `observed` with advancing timestamps within ~5 s | `api/60-pre-collector-restart.json`, `api/61-collector-restart-watch.jsonl`, `collector-resilience.log` |
| Device disconnect | `docker stop clab-m6f-srlinux-srlinux` @10:51:34 | PASS — subscription status truthfully degraded to `stale` ~27 s after stop with frozen `last_observed_at_ns`; no fabricated state | `api/62-pre-device-stop.json`, `api/63-device-down-watch.jsonl` |
| Device reconnect | `docker start` @10:53:22 | PASS — idle-stream watchdog forced reconnect @10:53:26 (idle_secs=120), one bounded retry failed while the device booted (10:53:38, backoff 5 s→10 s), connected 10:53:48, re-subscribed same plan_id; fresh session observed from 10:53:50, timestamps advancing ~10 s cadence | `api/64-device-recovery-watch.jsonl`, `collector-resilience.log` |
| Core restart | `docker restart m6f-bonsai-core` @10:57:45 | PASS — durable registry/vault survived; core up in ~8 s; collector reconnected and re-established assignment; new session (`first_observed_at_ns` 1795345078705044837), `observed` + advancing | `api/65-pre-core-restart.json`, `api/66-post-core-restart-t20.json`, `api/67-post-core-restart-watch.jsonl`, `core-restart.log` |
| Revocation after recovery | `POST /api/devices/{addr}/schema-binding/revoke` | PASS — `state=revoked`; ALL seven authoritative withdrawal checks: binding_revoked, acquisition_disabled, withdrawal_issued (SSE `subscription_plan_state` disabled event), collector_ack (connected, subscription_count=0, fresh heartbeat), subscription_terminated (no observed/pending), no_continued_collection (45 s window > 2×10 s sample interval, frozen timestamps), old_session_rejected | `api/40-plan-revoke.json`, `api/41/42-*-post-revoke-*.json`, `api/43-withdrawal-evidence.json` |

## Honest observations / limitations

- **Device-stop detection latency:** `docker stop` produced no immediate
  gRPC stream error in collector logs; the subscriber detected the dead
  stream via the 120 s SAMPLE idle watchdog (`forcing reconnect
  idle_secs=120`). Coverage still degraded truthfully to `stale` via
  timestamp age, and recovery was bounded — but stream-error detection
  on abrupt container stop is slower than TCP-level teardown would
  suggest. Not a correctness defect; recorded for the backlog.
- **Collector shutdown log noise:** during the collector restart the
  subscriber logged `lifecycle channel saturated/closed — shutdown
  terminal transition dropped`. The replacement session registered
  cleanly on reconnect so coverage stayed correct, but a dropped
  terminal lifecycle event on shutdown is worth a follow-up look.
- **Not executed (bounded scope):** assignment withdrawal/reassignment
  mid-stream, catalogue change during an active subscription, expired
  evidence aging, and revocation *during* the reconnect window. The
  revoke exercised here ran on a healthy recovered chain.
- Provenance: `l4_provenance` stage PASS —
  `sha=4eb35f08e4a0 dirty=False`, image `bn021d-bonsai:local` for both
  core and collector, NOS `26.3.1-410`, schema
  `ysc-4f20e39183beea7edbb16f2d943fa8bf`.
