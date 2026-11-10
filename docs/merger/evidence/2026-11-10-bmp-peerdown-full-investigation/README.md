# BMP PeerDown propagation — full investigation record (10 Nov 2026)

Consolidated record of the BN-014c follow-up investigation into why an
FRR BGP neighbor shutdown produced no observable peer-down event in
bonsai-routing. Combines the 9 Nov rehearsal finding, the upstream FRR
10.6.2 source analysis, and the 10 Nov frame-level tracing work.

## 1. Symptom

During `2026-11-09-bn014c-rehearsal`, admin shutdown and hard reset of
frr2's eBGP neighbor `10.100.1.0` (AS 65001↔65002) produced:

- peer `state` stayed `up` in `/api/routing/v1/peers?speaker=172.100.207.12`
- no new entries in `/api/routing/v1/events`
- no `BGP peer down` / `peer up` / `speaker` lines in
  `docker logs bonsai-bonsai-routing-1`
- route `10.10.1.0/24` correctly retained (it is **locally originated**
  by frr2 — `nexthop 0.0.0.0(frr2)` — so retention is expected, not a bug)

## 2. Code changes made for this investigation

| Commit | Change | File |
|---|---|---|
| `f4f0363` | `BONSAI_BMP_TRACE=1` frame-level tracing: every fully reconstructed BMP frame logs `speaker_ip`, BMP version, message length, message type, and first 48 header bytes as hex, inside `run_bmp_listener`'s frame loop (after `read_bmp_frame`, before `ingest_bmp_bytes`). Disabled by default; no behavior change when unset. | `src/routing_service/mod.rs` (~line 553) |

Prior BN-014 work this investigation builds on (already committed):

- `bba5df9` — lbug `$desc` reserved-word fix + `load_device_ci_map`
  schema fix + `x-bonsai-routing-token` auth in change_capture
- `753fbbb` — BN-014b `verify-invariants` endpoints + persisted verdicts
- `b3da565` — BN-014c rehearsal evidence
- `1730af9` — first investigation writeup

No other source changes were needed: `crates/rbmp-rib/src/manager.rs`
PeerDown handling (`peer.on_down`, RIB clear/LLGR-stale, `PeerDown`
RibEvent emit) was verified correct by inspection and unit coverage.

## 3. Upstream FRR 10.6.2 findings (from supplied research)

- FRR 10.6.2 **does** implement BMP PeerDown (`bgpd/bgp_bmp.c`:
  `bmp_peer_backward` → `bmp_send_all_bgp(peer, true)` →
  `bmp_peerstate` → `BMP_TYPE_PEER_DOWN_NOTIFICATION`) and PeerUp, via
  `peer_backward_transition` / `peer_status_changed` hooks.
- Upstream regression test `tests/topotests/bgp_bmp/test_bgp_bmp_1.py`
  `test_peer_down()` uses `clear bgp *` and asserts the BMP server
  receives peer-down — so emission is expected behavior, not a missing
  feature.
- `show bmp`'s `MonSent` counts **Route Monitoring messages only** —
  it cannot confirm or deny PeerUp/PeerDown traffic.
- RFC 9069 Loc-RIB peer (type 3) caveat noted; verified below that the
  observed peer is the real neighbor, not a virtual Loc-RIB peer.

## 4. Tests executed (all live, containerlab `bonsai-bn003-r07vrf`)

| # | Test | Result |
|---|---|---|
| 1 | 9 Nov: admin `neighbor 10.100.1.0 shutdown`, ~6 min observation | peer stayed `up`, no events, no logs |
| 2 | 9 Nov: `clear bgp 10.100.1.0` hard reset | same — nothing observed |
| 3 | 10 Nov: traced flap #1 (admin down 45s / up 30s) | **0 bmp_frame lines** in window |
| 4 | 10 Nov: traced flap #2 (`docker logs -f`, down 60s / up 45s) | **0 lines total** — no frames of any type |
| 5 | 10 Nov: traced flap #3 (down 50s / up 40s, `--since` window) | **0 bmp_frame**, mtype histogram empty |
| 6 | BMP session enumeration on frr2 | **exactly one** TCP session `172.100.207.12→172.100.207.1:11019`; `show bmp` reports monitoring for `BGP VRF default` only |
| 7 | Peer identity check | `/peers` shows `10.100.1.0` peer_as 65001 — the real type-0 neighbor, not a Loc-RIB virtual peer |
| 8 | Trace liveness proof | on service reconnect, `bmp_frame` lines emitted (22 observed) — instrumentation confirmed working |

FRR `show bmp` `MonSent` went 11→13→15 across earlier flaps (~2 route-
monitoring msgs per flap: updates + EOR) — consistent with BMP route
monitoring flowing while peer notifications do not.

## 5. Conclusion — where the notification disappears

**Upstream of Bonsai's decoder: no BMP frame reaches the TCP listener
during the transition.** Rules out, in order:

- ~~Bonsai BMP parser/decoder~~ — zero frames arrive to decode
- ~~peer identity / peer-type mismatch~~ — nothing to misidentify; also
  verified the displayed peer is the genuine type-0 neighbor
- ~~manager PeerDown handling~~ — code verified correct; never invoked
- ~~event publication / REST API~~ — no RibEvent is ever produced
- ~~multiple BMP sessions / wrong monitored instance~~ — single session,
  `BGP VRF default` monitored, neighbor is in that instance

Remaining candidates, unranked:

1. **FRR 10.6.2 does not emit PeerUp/PeerDown for this configuration**
   despite implementing the code path — e.g. gating on monitored
   address-family/VRF policy flags, or a defect specific to admin
   shutdown vs the upstream test's `clear bgp *`.
2. **FRR emits but on a path not reaching this listener** — contradicted
   by the single `ss`-verified session to `172.100.207.1:11019`.
3. **Bonsai's `read_bmp_frame` silently dropping frames** — cannot be
   fully excluded without wire capture; the trace only logs *complete*
   frames, so a malformed/truncated frame could theoretically be dropped
   earlier. However `read_bmp_frame` errors would surface as
   `bmp_transport_error` disconnects, none of which occurred (the BMP
   TCP session stayed `ESTAB` throughout — verified via `ss` on frr2).

## 6. What was NOT achieved

- **No wire-level packet capture.** `tcpdump` on the host fails
  (`Operation not permitted`, no CAP_NET_RAW); the FRR container lacks
  tcpdump; `debug bgp bmp` is not a valid FRR 10.6.2 vtysh command
  (`debug bgp neighbor-events` exists but bgpd logs don't reach container
  stdout). The frame trace is the strongest available evidence short of
  raw bytes.
- **PeerDown not demonstrated end-to-end.** The success criterion —
  admin shutdown producing an observable PeerDown transition through the
  API — could not be met because no frame arrives. This is reported as a
  blocking boundary, not claimed fixed.
- **Peer state correctness unproven.** Peers may remain `up`
  indefinitely absent PeerDown — a real observability gap. Per the
  research, hold-time/Adj-Rib-In-silence inference was explicitly
  rejected as invalid (KEEPALIVEs aren't visible in Route Monitoring);
  the correct fallback is representing BMP-speaker disconnect as
  "monitoring unavailable" rather than peer-down — a separate feature.
- **Learned-route withdrawal untested.** `10.10.1.0/24` is
  frr2-originated; a route genuinely learned from frr1 (e.g. a new
  `10.10.2.0/24` network statement on frr1) is needed for the Phase-5
  acceptance assertions — not yet added to the lab config.

## 7. Remaining unproven assumptions

- That FRR's BMP notification path is exercised at all in this lab —
  PeerUp messages *were* received historically (peer state `up`,
  `flap_count` increments on earlier reconnects), so PeerUp works at
  least on initial connect. Whether PeerUp re-fires on session
  re-establishment is untested (no frames observed on restore either).
- Whether the BMP Initiation/Termination or Statistics message types
  appear — never observed in traces (only PeerUp/RouteMonitoring seen on
  reconnect).
- FRR-side `debug bgp neighbor-events` output was not collected
  (logging not wired to a readable destination in the disposable lab).

## 8. ROOT CAUSE FOUND (10 Nov, ~04:05 UTC)

**Two `bonsai-routing` instances were running; we were watching the
wrong one.**

- `ss -tlnp` on the host showed TWO listeners on port 11019:
  - `127.0.0.1:11019` → docker-proxy → `bonsai-bonsai-routing-1`
    (PID 1998556, bridge net `bonsai_bonsai-internal`) — the instance we
    rebuilt, redeployed, and instrumented with `BONSAI_BMP_TRACE`.
  - `172.100.207.1:11019` → **PID 2633113 = `./target/debug/bonsai-routing`**,
    a `nohup`'d debug binary started 8 Nov 18:28 by an earlier session
    (journals `/tmp/bn003-journal-r07/`, HTTP `127.0.0.1:18082`,
    `BONSAI_ROUTING_BMP_ADDR=172.100.207.1:11019`).
- FRR's BMP target is `172.100.207.1:11019` → **all live BMP traffic has
  gone to the host debug process since 8 Nov**, not the container.
- The container's peer/route state is stale journal/checkpoint replay —
  which is why peers stayed `up` forever and zero frames arrived during
  flaps. The "22 bmp_frame lines on reconnect" were the only live frames
  the container ever saw.
- **Proof:** `/tmp/bn003-journal-r07/service.log` shows clean
  PeerDown/PeerUp pairs for every single flap:
  - `03:34:39 BGP peer down speaker=172.100.207.12 peer_addr=10.100.1.0
    reason_str=LocalSystemClosed2` → `03:35:48 peer up`
  - `03:40:03 peer down LocalSystemClosed2` → `03:40:54 peer up`
  - `04:02:56 peer down` → `04:03:34 peer up` (admin shutdown + restore)
  - Remote-side closes logged as `RemoteSystemClosed` with notification
    bytes on the frr1 speaker view.
- FRR 10.6.2 emits PeerDown correctly (`LocalSystemClosed2` = reason for
  admin shutdown); rbmp-rib handles it; journals persist it. **No defect
  in FRR, BMP transport, decoder, peer identity, manager, or API.**
- The Bonsai app (`bonsai-bonsai-standalone-1`) uses
  `BONSAI_ROUTING_SERVICE_URL=http://bonsai-routing:8081` → the
  **container**, i.e. the stale instance. BN-014a captures therefore read
  replayed state, not live BMP.

### Deployment fix required

1. Stop PID 2633113 (stray debug process; journals persist on disk).
2. Publish the container's BMP port on the clab gateway address:
   `172.100.207.1:11019:11019` (or `0.0.0.0`) in the compose override —
   the current `127.0.0.1:11019` mapping is unreachable from clab nodes.
3. Recreate `bonsai-routing`, wait for FRR BMP reconnect, re-run one
   flap to confirm PeerDown flows into the container and the app API.

### Lessons

- `docker logs` silence ≠ no traffic: verify *which process owns the
  listener* (`ss -tlnp` + `/proc/<pid>/cgroup` + `docker inspect`
  NetworkMode) before concluding a pipeline is broken.
- A host-network or stray dev process can silently win a port race
  against the compose deployment for weeks.

## 9. Recommended next actions

1. **Raw BMP byte capture without CAP_NET_RAW**: stand up a TCP proxy
   (e.g. socat or a small Rust forwarder) between frr2 and
   `172.100.207.1:11019`, or run a privileged `netshoot`/`tcpdump`
   container attached to the clab management network — decisive proof of
   whether FRR puts PeerDown bytes on the wire.
2. **FRR-side confirmation**: configure `log file ... debugging` +
   `debug bgp neighbor-events` in frr2's config, or check FRR 10.6.2
   `bgpd/bgp_bmp.c` gating conditions for `peer_backward_transition`
   (e.g. whether notifications require the peer to have been reported
   via PeerUp on *this* BMP session — session restarted 10 Nov, PeerUps
   from 8 Nov journal replay may not count).
3. **Lab config for Phase 5**: add `network 10.10.2.0/24` to frr1 so
   frr2 learns a genuinely remote prefix; re-run shutdown assertions on
   peer state + learned-route withdrawal separately.
4. **If FRR confirmed silent**: implement speaker-disconnect →
   `observability_state: monitoring_unavailable` semantics (per research
   §7) rather than peer-down inference.

## Evidence files

- `2026-11-09-bn014c-rehearsal/` — rehearsal snapshots, invariants,
  peer view, frr2 BGP summary
- `2026-11-09-bmp-peerdown-investigation/` — first-pass investigation
- This document — consolidated record
- Live trace: `BONSAI_BMP_TRACE=1` in `/tmp/bn019-compose-override.yml`
  (temporary; remove when investigation closes)
