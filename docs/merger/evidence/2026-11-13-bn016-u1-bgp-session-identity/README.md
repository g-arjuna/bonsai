# BN-016-U1 — BGP false-edge protection (13 November 2026)

Branch: `fix/bn016-u1-bgp-session-identity` (from merged main `b7277ca`).

## Confirmed defects (before)

1. **`src/http_server/observability.rs` `topology_handler`** grouped BGP
   neighbors per device and derived `peer_device` by scanning *every*
   `BGP_SESSION_WITH` edge out of the source device:
   `find(|(src, dst, _)| src == &dev && (dst == &peer || allowed_devices.contains(dst)))`.
   Any onboarded destination connected to the source could be reported as
   the peer of *any* unresolved neighbor — the arbitrary-destination
   fallback named in the backlog.

2. **`src/graph/common.rs::resolve_peer_to_device`** ran three
   `LIMIT 1` lookups (Device.address → DeviceAddress→KNOWN_ADDRESS_OF →
   EntityIdentity.mgmt_ip) and returned the first row. Ambiguous alias
   ownership and contradictory tiers were invisible; row order decided.

3. **`BGP_SESSION_WITH` edges were keyed only by `{source}`** on a
   device pair (`MERGE (a)-[r:BGP_SESSION_WITH {source:'gnmi'}]->(b)`).
   Multiple sessions between the same pair shared one edge and
   overwrote `session_state`; a peer that later became unresolvable
   left the stale edge authoritative forever (no reconciliation).

4. **BMP `BgpNeighbor` id collision**: the BMP mirror used
   `bmp_session_id` = `{bare}:{peer}` for global sessions, identical to
   the gNMI id — a BMP report could overwrite a gNMI session node's
   state and lose provenance.

5. **`ui/src/lib/Topology.svelte`** independently re-resolved peers:
   `b.peer_device || (filteredAddresses.has(b.peer) ? b.peer : null)` —
   the browser could draw an edge the backend never established.

## Resolution contract (after)

`PeerResolution` (`src/graph/common.rs`) is the single deterministic
contract, evaluated by `resolve_peer_typed` against ALL evidence before
deciding:

| Tier | Evidence |
|---|---|
| 1 | exact `Device.address` match (primary, authoritative key) |
| 2 | `DeviceAddress.ip` → `KNOWN_ADDRESS_OF` (registered aliases: extra_ips, loopbacks, protocol-observed) |
| 3 | `EntityIdentity.mgmt_ip` → `HAS_IDENTITY` |

Outcomes: `Resolved(device)` only when every tier that has an opinion
names the SAME single device. `Unresolved` (no claims), `Ambiguous`
(multiple distinct claimants, no primary), `Conflicted` (primary owner
vs alias/identity claimant disagree — reported with both owners in
`resolution_reason`). No `LIMIT 1`, no prefix match, no provisional
flow/syslog/SNMP/OTLP claims.

`resolve_peer_to_device` is now a thin wrapper returning `Some` only
for `Resolved` — BFD/IS-IS/LLDP/BMP-monitor callers inherit the same
fail-closed behavior (no arbitrary first-row picks).

## Session vs aggregate-edge semantics

- `BgpNeighbor` is the **session-level evidence** node. gNMI sessions
  keep id `{device}:{peer}`; BMP sessions now get `bmp:{bmp_session_id}`
  so the two provenances can never collide. New columns record
  `source` (`gnmi`/`bmp`), `resolved_device`, `resolution_status`,
  `resolution_reason`. BMP `peer_type`/`router_distinguisher` remain in
  the `bmp:`-qualified session id and the authoritative `BmpSession`
  node.
- `BGP_SESSION_WITH` is an **aggregate** Device→Device edge keyed by
  `(src, dst, source, peer_address)` — one edge per observed session,
  carrying `session_state`/`peer_as`. It is derived ONLY from a unique
  `Resolved` outcome and is transactionally reconciled on every
  observation: unresolved/ambiguous/conflict deletes that session's
  edge so a stale resolution never presents itself as current.
- Legacy edges without `peer_address` (`''`/`NULL`) are treated as
  historical ambiguous relationships and removed the first time their
  session scope is re-observed; they rebuild through the same path.
  Schema upgrade: idempotent `ALTER TABLE … ADD` for
  `BgpNeighbor.{source,resolved_device,resolution_status,resolution_reason}`
  and `BGP_SESSION_WITH.peer_address`.

## API / UI

- `BgpJson` gains `source`, `resolution`
  (`resolved|unresolved|ambiguous|conflict`) and `reason`; `peer_device`
  is populated only for a unique resolution. Both `/api/topology` and
  the per-device detail endpoint resolve live through
  `resolve_peer_typed` — no D2D-edge scanning remains.
- `Topology.svelte` no longer falls back to raw peer-address matching;
  unresolved sessions render an explicit `unresolved`/`ambiguous`/
  `conflict` badge with the reason, and only `peer_device`-resolved
  sessions draw links. Down sessions keep their own edge state.

## Verified

- `cargo check -p bonsai --lib` — clean.
- `cargo test -p bonsai --lib graph::tests::bgp_session` — **7/7**:
  multi-neighbor isolation, unique alias, unknown peer, ambiguous alias,
  conflicting tiers, multi-session same pair, gNMI+BMP provenance,
  stale-edge reconciliation, replay/restart idempotence.
- `cargo test -p bonsai --lib graph::` — 87 pass / 1 fail =
  documented BN-019-B6 baseline
  (`subscription_status_write_preserves_device_metadata`).
- BMP peer-state / BN-021c exporter isolation / BN-021f authenticated
  ingress integration regressions — see PR body for executed results.

## Limitations (explicit)

- **H12 overlapping private addresses:** `DeviceAddress`/`Device` are
  globally address-keyed; two tenants claiming the same private peer IP
  resolve to `Ambiguous` — fail closed, no edge. Full
  overlapping-address tenant isolation remains future work (BN-008/024
  canonical scoped identity).
- **VRF/session context:** the gNMI BGP path does not carry a VRF
  discriminator into `BgpNeighbor`; BMP context lives in
  `bmp_session_id`/`BmpSession`. Missing context stays unknown rather
  than guessed.
- `MONITORS_BGP_ROUTER` (collector→router) uses the same strict
  resolver but has no keyed reconcile — noted for follow-up.
- `src/collector/graph.rs` (collector-local store) writes `BgpNeighbor`
  without D2D edges — unaffected, no false-edge surface.
- Browser Playwright topology acceptance not executed in this slice;
  UI change is presentation-only on top of the verified API contract.
