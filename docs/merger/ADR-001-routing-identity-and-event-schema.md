# ADR-001: Typed Routing Identity and Versioned Event Schema (B04)

- **Status**: Accepted
- **Date**: 2026-10-11
- **Task**: `B04` (Milestone M1 — Trustworthy Routing Core)
- **Fixes**: `F03` (Route identity collapses independent observations)

## Context

In the baseline RustyBMP implementation (`e2a11893`):
1. `rbmp-rib/src/manager.rs` keyed `ribs` by `HashMap<IpAddr, RibTable>` under the assumption that a BGP peer IP is globally unique across all exporting routers and routing instances.
2. `rbmp-rib/src/session.rs` keyed `BmpSession::peers` by `HashMap<IpAddr, PeerSession>`, collapsing two VRFs (`PeerType::RdInstance` with distinct 8-byte `peer_distinguisher`s) or a Global instance and Local instance sharing the same peer address on the same exporting router.
3. `rbmp-rib/src/table.rs` keyed entries within a `RibType` by string concatenation (`format!("{}@{}", prefix, id)`), rather than a typed NLRI + optional Add-Path ID key.
4. `BmpSession` disconnects (`Termination`) were not fenced by connection/session epoch, allowing an older TCP connection closing late to wipe out state established by a newer connection from the same exporter.
5. `RibEvent` and `RouteChange` lacked explicit schema versioning, typed `ObservationSource` / `PeerKey` / `ViewKey` / `RouteKey` / `EventKey` descriptors, and dropped the RFC 7911 `path_id` from emitted `RouteChange` events.

Similarly, in Bonsai (`src/graph/mod.rs`), `upsert_bmp_session` keyed BMP sessions only by `device_address + peer_address`, and `write_bmp_route_monitoring` omitted `peer_distinguisher` and `path_id`.

## Decision

We establish five canonical, strongly typed, `Serialize`/`Deserialize`/`Eq`/`Hash`/`Ord` identity structures in `rbmp-core::bmp::types` (and re-exported by `rbmp-rib`):

1. **`ObservationSource`**:
   - `collector_id: String` — logical collector identifier (default `"default"`).
   - `exporter_id: String` — canonical device/exporter ID (or canonical speaker address string before inventory resolution).
   - `exporter_addr: IpAddr` — transport source IP attribute.
   - `exporter_instance: String` — routing engine / exporter instance (default `"default"`).
   - `session_epoch: u64` — monotonically increasing connection epoch per exporter, used to fence stale disconnect events.

2. **`PeerKey`**:
   - `speaker_addr: IpAddr` — exporting BMP router address.
   - `peer_type: PeerType` — RFC 7854 / RFC 9069 peer type (`GlobalInstance = 0`, `RdInstance = 1`, `LocalInstance = 2`, `LocRib = 3`).
   - `peer_distinguisher: [u8; 8]` — 8-byte distinguisher (VRF RD or Loc-RIB table distinguisher; `[0; 8]` for global).
   - `peer_address: IpAddr` — remote peer IP (or `0.0.0.0` / `::` for `PeerType::LocRib`).
   - *Peer ASN / BGP ID changes*: `peer_as` and `peer_bgp_id` are preserved on `PeerSession` and `PeerHeader`; if a peer re-establishes on the same `(speaker_addr, peer_type, peer_distinguisher, peer_address)` with an updated ASN or BGP ID, `PeerSession::on_up_with_header` updates the session attributes and increments `flap_count` without leaking orphaned `PeerKey` entries.

3. **`ViewKey`**:
   - `peer: PeerKey`
   - `afi_safi: AfiSafi`
   - `rib_type: RibType`
   - `direction: ViewDirection` (`Inbound`, `Outbound`, `Local`)
   - `is_post_policy: bool`
   - `is_filtered: bool` (RFC 9069 Loc-RIB `F` flag)

4. **`CanonicalNlri` & `RouteKey`**:
   - `CanonicalNlri`: tagged enum separating `IpUnicast(Prefix)`, `L3Vpn { rd: [u8; 8], prefix: Prefix }`, and `Evpn { rd: String, route_type: u8, key: String }` so BMP `peer_distinguisher` is never conflated with an NLRI Route Distinguisher.
   - `TableRouteKey`: `{ nlri: CanonicalNlri, path_id: Option<u32> }` used directly as the hash key inside `RibTable` (replacing string concatenation).
   - `RouteKey`: `{ view: ViewKey, nlri: CanonicalNlri, path_id: Option<u32> }`.

5. **`EventKey` & Versioned `RibEvent` (`EVENT_SCHEMA_VERSION = 2`)**:
   - `EventKey`: `{ schema_version: u16, source: ObservationSource, sequence: u64 }`.
   - `sequence` is a monotonically increasing per-source/manager sequence number (never a content hash, so legitimate repeated announcements remain distinct events).
   - `RouteChange` now carries `path_id: Option<u32>`, `peer_key: PeerKey`, `view_key: ViewKey`, and `route_key: RouteKey` alongside the existing fields.

## Migration & Legacy Data Policy

- **In-memory RIB (`rbmp-rib`)**: Keyed strictly by `PeerKey` (in `RibManager::ribs` and `BmpSession::peers`) and `TableRouteKey` (in `RibTable`). Legacy helper `RibManager::rib_for_peer(IpAddr)` is retained only as a single-peer convenience accessor while `rib_for_peer_key(&PeerKey)` and `ribs_for_speaker(IpAddr)` provide collision-free access.
- **Historical DuckDB / JSONL rows (`B06` / `B16`)**:
  - Existing rows written under schema v1 lack `peer_type`, `peer_distinguisher`, `path_id`, and `session_epoch`.
  - Per the handoff data-quality contract, migration **must never invent missing identity values**: migrated v1 rows default `peer_distinguisher` to `NULL` / legacy-unknown marker and carry `identity_quality = 'legacy_incomplete'` so UI and API consumers explicitly see when historical records predate full `PeerKey`/`RouteKey` capture.

