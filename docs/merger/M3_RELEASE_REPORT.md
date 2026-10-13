# Milestone M3 Prototype Report (`B16`–`B20`) & Reconciled Definition of Done

**Date**: 12 October 2026 (Asia/Kolkata)  
**Repository**: `bonsai` (`main`)  
**Imported RustyBMP Baseline**: `/home/arjuna/rustybmp-b01` (`f330febcca2f2870d4dd29526057385abc4f308a`, derived from `e2a1189392db520b25a376795e1bfae72eca5b55`)  
**Current Standing**: Integrated Development Prototype — see [CURRENT_STATE_REVIEW_2026-10-12.md](CURRENT_STATE_REVIEW_2026-10-12.md) and [STATUS.md](STATUS.md).

---

## 1. Release Definition of Done Checklist (Handoff §11 — Reconciled)

- [x] **One Bonsai UI/login/inventory/configuration authority; no publicly exposed duplicate routing control plane**:
  - Public UI and API traffic enters through Bonsai (`/api/routing/v1/*`, `/routing` in `ui/src/App.svelte`).
  - `bonsai-routing` (`src/bin/routing_service.rs`) binds to `127.0.0.1:8081` (or internal Compose network `bonsai-internal`) and checks `x-bonsai-routing-token` when configured (mandatory default enforcement tracked under `R06`).
- [x] **One tested BMP decoder and explicit family/view support matrix**:
  - Canonical BMP/BGP wire decoder is `crates/rbmp-core` (`parse_bmp_message_with_context`, `validate_bmp_header`, `read_bmp_frame`).
  - Legacy `BmpConfig::should_spawn_legacy_receiver()` defaults to `false` (`authority = "routing_service"`).
  - Explicit `FamilySupportStatus` (`Supported`, `ExperimentalParseOnly`, `Unsupported`) is exposed via `/api/routing/v1/coverage` and enforced in `/api/routing/v1/routes`.
- [x] **No cross-exporter/VRF/path collisions; monitoring loss distinct from withdrawal**:
  - RIB state is keyed by `ObservationSource` (`collector_id`, `exporter_id`, `exporter_addr`, `exporter_instance`, `session_epoch`), `PeerKey` (`speaker_addr`, `peer_type`, `peer_distinguisher`, `peer_address`), `ViewKey` (`peer`, `afi_safi`, `rib_type`, `direction`, `is_post_policy`, `is_filtered`), and `RouteKey` (`view`, `nlri`, `path_id`).
  - TCP EOF without a BGP withdrawal records a durable, epoch-fenced `TransportGapRecord` (`completeness_state = "gap"`) while preserving last-known routes in the RIB (`R04`).
- [x] **Durable replay and idempotency proven in-process; history and current state agree after restart (`R01`–`R04` repaired)**:
  - `RibManager::process_for_epoch` returns `Vec<RibEvent>` directly (`R01`), eliminating broadcast channel lag from the history-authority path.
  - `BmpJournal` and `RibEventJournal` (`crates/rbmp-store/src/journal.rs`) truncate torn crash tails on disk via `set_len(valid_len)` + `sync_all()` (`R03`), persist `session_epoch`, raw wire hex, and `JournalTransportGap` records (`R02`, `R04`), and reconcile missing `RibEventJournal` records idempotently across multiple restarts (`tests/r01_r04_routing_history_recovery.rs`).
  - `project_routing_events_to_graph` (`src/routing_service/mod.rs`) persists `RoutingProjectionCheckpoint` (`routing-projection-checkpoint-v1`) in `GraphStore` (background runtime caller tracked under `R05`).
- [ ] **Authentication/scope/secret handling enforced by default across all deployment paths (`R06` pending)**:
  - Explicit RBAC and API-key `routing:read` scope checks (`401`/`403`/`200`), recursive `redact_secrets_in_json`, and strict `RejectPolicy` default for SSH host keys are verified in `tests/b11_b12_b13_routing_workflow.rs` and `tests/test_b15_ssh_host_key_policy.py`.
  - Default-enabled auth, required internal token in Compose, and `PrefixHistoryQuery` VRF/exporter filtering are tracked under `R06`.
- [ ] **Live Containerlab FRR two-run reproducibility and non-FRR NOS lab verification (`R07` pending)**:
  - `tests/b16_b17_b18_b19_m3_release_gates.rs` (`b17_b18_frr_two_clean_runs_and_multivendor_capability_matrix`) verifies deterministic **in-process synthetic BMP frame replay** against `ExpectedRouteOracle` ([manifest.json](evidence/2026-10-11/b17-b18-evidence-bundle/manifest.json), [results.json](evidence/2026-10-11/b17-b18-evidence-bundle/results.json)).
  - Live Containerlab FRR deployment with router CLI output, packet/BMP capture, and browser UI verification is pending under `R07`.
- [x] **UI distinguishes unknown, stale, incomplete, empty and unsupported results**:
  - Implemented in `ui/src/routes/Routing.svelte` (`completenessBadgeClass`, explicit Data-Quality Notice banner for `syncing`, `gap`, `stale`, `unsupported`, and `unavailable`).
- [x] **Migration preserves backups; legacy identity gaps are labelled; rollback documented**:
  - Implemented in `src/routing_service/migration.rs` (`create_backup`, `migrate_routing_jsonl_file`, `migrate_bonsai_toml_file`, `rollback_from_backup`) and verified in `b16_versioned_config_and_routing_jsonl_migration_with_rollback`.
- [ ] **Full workspace formatting and Clippy cleanliness (`B01` cleanup pending)**:
  - Selected routing crate tests (`132 passed`), `R01`–`R04` recovery probes (`7 passed`), workflow tests, and Python regressions pass; workspace `cargo fmt --all -- --check` and `rbmp-core` Clippy diagnostics require cleanup.
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

## 3. Protocol vs Live NOS Verification Matrix (`B17`–`B18`, Reconciled)

| Canonical Vendor ID | Target NOS Image | Byte-Level BMP PDU Decode (`rbmp-core` / `rbmp-rib`) | Live Containerlab / Router CLI Verification (`R07`) | gNMI Streaming | BGPsec Crypto Validation |
|---|---|---|---|---|---|
| `frrouting` | FRR (pinned host image) | Verified (Global / Peer Views, 4-byte ASN, Add-Path, EoR) | **Pending (`R07`)** — current `B17` test uses synthetic frames | Unsupported (BMP/CLI) | Unsupported without RFC 8205 wire PDU |
| `nokia_srl` | Nokia SR Linux | Verified (Synthetic RFC 9069 Loc-RIB `peer_type=3` PDU + `A13` JSON fixture parser) | **Pending (`R07`)** — no live container run executed | Adapter implemented | Unsupported without RFC 8205 wire PDU |
| `cisco_xrd` | Cisco IOS XRd | Verified (Synthetic RFC 8671 `RdInstance` `peer_type=1` `AdjRibOutPostPolicy` PDU) | **Pending (`R07`)** — no live container run executed | Adapter implemented | Unsupported without RFC 8205 wire PDU |
| `arista_eos` | Arista cEOS | Registry & parser support only | **Not executed** | Adapter implemented | Unsupported without RFC 8205 wire PDU |

---

## 4. Deferred Post-M3 Roadmap (`B21`–`B22`)

- **B21 — Address-Family Expansion**:
  - Promote `l2vpn-evpn` (`25/70`) and `bgp-ls` (`16388/71`) from `ExperimentalParseOnly` / `Unsupported` to `Supported` one family at a time, each with dedicated announce/withdraw/restart/identity tests and real router captures.
- **B22 — Deterministic Analytics & Read-Only Grounded Explanation**:
  - Build read-only prefix flap/convergence summaries citing stored `RibEventJournal` sequence numbers and `completeness_state` bounds without autonomous device writes.

