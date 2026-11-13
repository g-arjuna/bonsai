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

## Phase B closure (P0 follow-up, commits 2e50f1f + ad76c15)

### B1 — Transaction ownership (root cause found and fixed)

`reconcile_bgp_session_edge` opened its own `BEGIN TRANSACTION` while
`GraphStore::write_batch` already held one — lbug rejects nested write
transactions, so **every** BGP/BMP write inside a production batch failed
silently at the reconcile step. Additionally, lbug rolls back the whole
active transaction on *any* statement error, after which later
statements run in autocommit.

Fix: one transaction owner per write operation.

- `write()`/`write_batch()` wrap `write_blocking` and own the single
  transaction; reconcile helpers never open nested transactions.
- `write_batch` tracks transaction death: if the batch transaction is
  rolled back (lbug error or Rust error), only the failed updates are
  retried individually through `retry_update_individually`, each on its
  own connection/transaction. Batching stays enabled; failures are
  counted and surfaced, never swallowed.
- `write()` tolerates COMMIT on an already-dead transaction (lbug
  semantics) instead of reporting a spurious second error.

Regression evidence (`graph::tests::bgp_*`, all via `GraphStore::write_batch`):

- `bgp_batch_write_produces_session_edges_and_state` — multiple BGP
  observations in one batch produce session edges + node state.
- `bgp_batch_failure_does_not_poison_other_updates` — a failing update
  inside a batch does not lose sibling updates; fallback retry lands
  them; error counter/fallback path exercised.
- `bgp_batch_ambiguous_peer_produces_no_edge` — ambiguous resolution
  inside a batch yields no `BGP_SESSION_WITH` edge.
- Mixed BMP/gNMI batches covered by
  `bgp_session_gnmi_and_bmp_keep_separate_provenance` and peer-state
  route-monitoring tests.
- Replay/restart idempotence:
  `bgp_session_replay_is_idempotent_across_restart`.

Batch latency: the happy path is unchanged (single transaction, same
statement count minus the removed nested BEGIN/COMMIT pair). The
fallback path only engages on transaction death and re-runs affected
updates individually — measured indirectly via the focused suite
(18 tests, ~14s incl. process startup); no per-batch overhead was added
to the success path.

### B2 — Complete session identity on relationships

`BGP_SESSION_WITH` relationships are now keyed by `session_id` (the
`BgpNeighbor` node id) and carry `peer_type`, `route_distinguisher`,
`network_instance`, `source` (gnmi|bmp) and `peer_address`. gNMI paths
containing `network-instance[name=X]` join the session identity for
non-default instances (`{device}:{ni}:{peer}`); the conventional
default/absent instance keeps the legacy `{device}:{peer}` node id for
compatibility. BMP session ids already encoded peer type + RD.
Distinct sessions sharing a peer address no longer collapse;
reconciliation of one session cannot delete another
(`bgp_same_peer_distinct_{peer_types,route_distinguishers}`,
`bgp_gnmi_same_peer_two_network_instances_keep_separate_sessions`).

### B3 — Fail-closed resolver

`resolve_peer_typed` now distinguishes: successful empty lookup,
unique consistent owner, ambiguity/conflict, and **failed/incomplete
evidence lookup** → new `PeerResolution::Unavailable`. Any tier
prepare/query failure yields `Unavailable` — never `Resolved` on
partial evidence. `resolve_peer_to_device` maps non-authoritative
outcomes to `None`. Regression:
`bgp_resolution_unavailable_fails_closed`,
`bgp_session_{ambiguous_alias,conflicting_tiers}_produces_no_edge`.

### B4 — Graph/API consistency + migration

- Ownership mutations (`KNOWN_ADDRESS_OF` alias links, `EntityIdentity`
  mgmt_ip claims, device address writes) now re-resolve affected BGP
  session edges immediately inside the same write — stale edges are
  invalidated when ownership becomes ambiguous/conflicting without
  waiting for the next BGP observation
  (`bgp_session_stale_edge_reconciled_on_ownership_loss`).
- Startup migration `migrate_legacy_bgp_session_edges`: unkeyed legacy
  `BGP_SESSION_WITH` edges (empty `session_id`) are re-keyed when the
  session identity is uniquely recoverable from stored `BgpNeighbor`
  evidence, otherwise explicitly invalidated (edge removed, historical
  `BgpNeighbor` observations retained). Ambiguous legacy edges are
  never guessed. Schema ALTERs now distinguish already-present columns
  from real migration failures.
- Topology API resolves each session live through the same
  `resolve_peer_typed` contract with a per-peer resolution cache within
  the request snapshot; `peer_device` is populated only for `Resolved`.

### B5 — Smaller fixes

- `write_bgp_neighbor` projects the **effective persisted peer-as**
  (stored value preserved on ON_CHANGE omissions) onto the relationship
  (`bgp_peer_as_preserved_when_update_omits_it`).
- `write_bmp_peer_state` no longer reconciles the session edge twice —
  `upsert_bmp_session` is the single reconcile owner.
- Resolution cost: topology handler caches `resolve_peer_typed` results
  per peer address within one request; no long-lived stale cache.

### Verified (Phase B)

- `cargo check -p bonsai --lib` — clean.
- `cargo test -p bonsai --lib graph::tests::bgp_session` — **18/18**.
- `cargo test -p bonsai --lib graph::` — **98 pass / 1 fail** =
  documented BN-019-B6 baseline
  (`subscription_status_write_preserves_device_metadata`, unchanged).
- `cargo test --test bn016_u1_topology_browser` — **1/1**: real
  headless Chromium against the canonical router; asserts 4 sessions on
  the router (2 same-peer distinct network-instances, 1 down, 1
  unresolved with no `peer_device`), BGP legend rendered, zero page
  errors. Spec: `tests/ui_driver/topology_bgp.spec.js`.
- `npm run build` (ui) — clean, 6.51s.

### Remaining limitations (Phase B)

- lbug rolls back the entire active transaction on any statement
  error; the batch fallback therefore re-executes failed updates
  individually rather than partially committing a batch. StateChange
  events broadcast during a rolled-back batch can be re-published by
  the fallback retry (at-least-once notification semantics; graph state
  itself is consistent).
- `MONITORS_BGP_ROUTER` and collector-local `BgpNeighbor` writes do not
  produce `BGP_SESSION_WITH` edges (unchanged scope).
