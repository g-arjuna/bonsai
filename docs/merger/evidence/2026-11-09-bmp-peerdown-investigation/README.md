# BMP peer-down propagation investigation (9 Nov 2026)

Follow-up to `2026-11-09-bn014c-rehearsal`: the admin-down injection never
surfaced in the BMP-fed routing view. This note records what was checked
and what remains unproven.

## Verified facts

- `crates/rbmp-rib/src/manager.rs` handles `BmpPayload::PeerDown`
  correctly: marks the `PeerSession` down, clears (or LLGR-stale-marks)
  the peer's RIB, syncs route counts, emits a `PeerDown` RibEvent. If a
  PeerDown BMP message arrived, the peer would show `down` and the log
  would contain `BGP peer down`.
- `docker logs bonsai-bonsai-routing-1` during BOTH an admin shutdown
  (`neighbor 10.100.1.0 shutdown`) and a hard reset (`clear bgp
  10.100.1.0`) showed **zero** peer up/down/disconnect log lines; the
  `/api/routing/v1/peers` view kept `state: up` throughout, and
  `/api/routing/v1/events` showed no new events.
- FRR-side `show bmp` on frr2: `MonSent` (route-monitoring counter)
  incremented by ~2 per flap (11 → 13 → 15 across three flaps), i.e. FRR
  *is* emitting BMP route-monitoring traffic around the transitions —
  but no PeerUp/PeerDown effect was observed downstream.
- `10.10.1.0/24` is locally originated by frr2 (`nexthop 0.0.0.0(frr2)`),
  so route retention during the outage is correct behavior, not a bug.
- Session restored and re-Established after each test; lab left healthy.

## Not proven (blocked by environment)

- Whether FRR actually puts PeerDown/PeerUp BMP messages on the wire.
  `debug bgp bmp` is not a valid vtysh command on FRR 10.6.2, bgpd logs
  do not appear in the container's stdout, and `tcpdump` on the host
  fails with `Operation not permitted` (no CAP_NET_RAW). No packet
  capture was possible in this session.

## Most likely explanation

FRR's BMP implementation appears to emit route-monitoring updates but
not Peer Up/Down notifications for these transitions (admin shutdown and
hard clear alike). If confirmed on the wire, this is an FRR-side
limitation: peer liveness in bonsai-routing would then rely on
SpeakerDown/TCP EOF or hold-time expiry rather than explicit PeerDown —
a real observability gap for peer-down detection.

## Next steps to close the question

1. Wire capture during a flap: run tcpdump as root on the host or in a
   privileged container attached to the clab management network, filter
   `tcp port 11019`, decode BMP message types (PeerDown=2, PeerUp=3).
2. Check FRR 10.6.2 BMP docs/source for PeerDown emission conditions
   (some versions only send PeerUp/Down for neighbors in monitored
   VRFs/address-families at session setup, or gate PeerDown on
   non-admin resets).
3. If FRR never sends PeerDown, consider a defensive fallback in
   bonsai-routing: mark peers stale after hold-time expiry or on
   Adj-Rib-In silence, so peer-down is never silently "up" forever.
