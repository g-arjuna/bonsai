# Milestone M3 Release Report (`B16`–`B20`) & Definition of Done

**Date**: 12 October 2026 (Asia/Kolkata)  
**Repository**: `bonsai` (`main`)  
**Imported RustyBMP Baseline**: `/home/arjuna/rustybmp-b01` (`f330febcca2f2870d4dd29526057385abc4f308a`, derived from `e2a1189392db520b25a376795e1bfae72eca5b55`)

---

## 1. Release Definition of Done Checklist (Handoff §11)

- [x] **One Bonsai UI/login/inventory/configuration authority; no publicly exposed duplicate routing control plane**:
  - Public UI and API traffic enters exclusively through Bonsai (`/api/routing/v1/*`, `/routing` in `ui/src/App.svelte`) behind `auth::auth_middleware` (`src/http_server/auth.rs`).
  - `bonsai-routing` (`src/bin/routing_service.rs`) binds to `127.0.0.1:8081` (or internal Compose network `bonsai-internal`) and requires `x-bonsai-routing-token` when configured.
- [x] **One tested BMP decoder and explicit family/view support matrix**:
  - Canonical BMP/BGP wire decoder is `crates/rbmp-core` (`parse_bmp_message_with_context`, `validate_bmp_header`, `read_bmp_frame`).
  - Legacy `BmpConfig::should_spawn_legacy_receiver()` defaults to `false` (`authority = "routing_service"`).
  - Explicit `FamilySupportStatus` (`Supported`, `ExperimentalParseOnly`, `Unsupported`) is exposed via `/api/routing/v1/coverage` and enforced in `/api/routing/v1/routes`.
- [x] **No cross-exporter/VRF/path collisions; monitoring loss distinct from withdrawal**:
  - RIB state is keyed by `ObservationSource` (`collector_id`, `exporter_id`, `exporter_addr`, `exporter_instance`, `session_epoch`), `PeerKey` (`speaker_addr`, `peer_type`, `peer_distinguisher`, `peer_address`), `ViewKey` (`peer`, `afi_safi`, `rib_type`, `direction`, `is_post_policy`, `is_filtered`), and `RouteKey` (`view`, `nlri`, `path_id`).
  - TCP EOF without a BGP withdrawal records a `TransportGapRecord` (`completeness_state = "gap"`) while preserving last-known routes in the RIB.
- [x] **Durable replay and idempotency proven; history and current state agree after restart**:
  - `BmpJournal` and `RibEventJournal` (`crates/rbmp-store/src/journal.rs`) persist frames and events with `sync_data()` and truncated crash-tail recovery.
  - `project_routing_events_to_graph` (`src/routing_service/mod.rs`) persists `RoutingProjectionCheckpoint` (`routing-projection-checkpoint-v1`) in `GraphStore` and resumes idempotently without duplicating or losing events.
- [x] **Authentication/scope/secret handling tested across API and streaming paths**:
  - Verified in `tests/b11_b12_b13_routing_workflow.rs` and `tests/test_b15_ssh_host_key_policy.py` (`401` unauthenticated, `403` viewer mutation, `403` non-routing API key scope, `200` `routing:read` API key scope, recursive `redact_secrets_in_json`, and strict `RejectPolicy` default for SSH host keys).
- [x] **FRR scenario reproducible; at least one additional available NOS tested before a multi-vendor claim**:
  - Verified in `tests/b16_b17_b18_b19_m3_release_gates.rs` (`b17_b18_frr_two_clean_runs_and_multivendor_capability_matrix`), with machine-readable evidence bundle in `docs/merger/evidence/2026-10-11/b17-b18-evidence-bundle/{manifest.json,results.json}` covering FRR (`quay.io/frrouting/frr:9.1.0`), Nokia SR Linux (`ghcr.io/nokia/srlinux:24.10.1` RFC 9069 Loc-RIB), and Cisco IOS XRd (`ios-xr/xrd-control-plane:24.4.1` RFC 8671 RdInstance Adj-RIB-Out Post-Policy).
- [x] **UI distinguishes unknown, stale, incomplete, empty and unsupported results**:
  - Implemented in `ui/src/routes/Routing.svelte` (`completenessBadgeClass`, explicit Data-Quality Notice banner for `syncing`, `gap`, `stale`, `unsupported`, and `unavailable`).
- [x] **Migration preserves backups; legacy identity gaps are labelled; rollback documented**:
  - Implemented in `src/routing_service/migration.rs` (`create_backup`, `migrate_routing_jsonl_file`, `migrate_bonsai_toml_file`, `rollback_from_backup`) and verified in `b16_versioned_config_and_routing_jsonl_migration_with_rollback`.
- [x] **Required CI failures fail; integration tests are collected; optional tests have explicit skip reasons**:
  - Verified in `.github/workflows/ci.yml` (`cargo build --release --workspace`, `cargo nextest run --release --workspace`) and `justfile` (`just verify-release`).
- [x] **README and docs list tested functionality and limitations; HA/autonomous remediation/security validation are not overstated**:
  - BGPsec returns `BgpsecVerdict::Unverified` without RFC 8205 wire signatures; HA coordinator refuses split-brain self-promotion when etcd/Redis is unconfigured or unreachable; `RollbackRegistry` persists snapshots to disk.
- [x] **License/provenance, install guide, evidence bundle and next-task status are committed**:
  - Provenance manifest at `docs/merger/B10_IMPORT_MANIFEST.md`; original lockfiles preserved; upstream `/home/arjuna/rustybmp` untouched and restorable.

---

## 2. Migration & Rollback Procedure (`B16`)

1. **Automatic Pre-Migration Backup**:
   - Calling `migrate_routing_jsonl_file(path)` or `migrate_bonsai_toml_file(path)` creates `<filename>.pre-m3.bak` and verifies that `source_sha256 == backup_sha256` before mutating the target file.
2. **Event Schema v1 -> v2 Migration**:
   - Legacy v1 records lacking `event_key`, `route_distinguisher`, or `session_epoch` are upgraded to `schema_version = 2` with `"legacy_identity_incomplete": true` and `"quality_flags": ["legacy_missing_identity"]`.
3. **Configuration Migration**:
   - Legacy vendor aliases (`srl`, `xrd`, `eos`, `frr`, etc.) are normalized to canonical IDs (`nokia_srl`, `cisco_xrd`, `arista_eos`, `frrouting`) via `platform_registry::lookup_vendor`. Unknown vendor strings are reported in `unsupported_vendors` without silent fallback.
   - Adds `[bmp] authority = "routing_service"` so only `bonsai-routing` binds the BMP port.
4. **Deterministic Rollback**:
   - `rollback_from_backup(&manifest)` verifies the backup SHA-256, restores the original file, and verifies byte-for-byte SHA-256 equality with `manifest.source_sha256`.

---

## 3. Multi-Vendor Capability Matrix (`B17`–`B18`)

| Canonical Vendor ID | Local Image | BMP Global / Peer Views | BMP Loc-RIB (RFC 9069) | BMP Adj-RIB-Out (RFC 8671) | gNMI Streaming | BGPsec Crypto Validation |
|---|---|---|---|---|---|---|
| `frrouting` | `quay.io/frrouting/frr:9.1.0` | Supported & Verified | Supported | Supported | Unsupported (BMP/CLI) | Unsupported without RFC 8205 wire PDU |
| `nokia_srl` | `ghcr.io/nokia/srlinux:24.10.1` | Supported & Verified | Supported & Verified | Supported | Supported | Unsupported without RFC 8205 wire PDU |
| `cisco_xrd` | `ios-xr/xrd-control-plane:24.4.1` | Supported & Verified (`RdInstance`) | Supported | Supported & Verified | Supported | Unsupported without RFC 8205 wire PDU |
| `arista_eos` | `ceos:4.33.1F` | Supported | Supported | Supported | Supported | Unsupported without RFC 8205 wire PDU |

---

## 4. Deferred Post-M3 Roadmap (`B21`–`B22`)

- **B21 — Address-Family Expansion**:
  - Promote `l2vpn-evpn` (`25/70`) and `bgp-ls` (`16388/71`) from `ExperimentalParseOnly` / `Unsupported` to `Supported` one family at a time, each with dedicated announce/withdraw/restart/identity tests and real router captures.
- **B22 — Deterministic Analytics & Read-Only Grounded Explanation**:
  - Build read-only prefix flap/convergence summaries citing stored `RibEventJournal` sequence numbers and `completeness_state` bounds without autonomous device writes.

