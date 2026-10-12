# First routing integration actions

Canonical current state: [STATUS.md](STATUS.md). Full backlog B00–B22 and architecture contracts: [source handoff, sections 6–8](../../BONSAI_RUSTYBMP_CODEX_HANDOFF.md).

## B00 — establish the baseline (completed)

Initial inventory is recorded in [../baseline.md](../baseline.md). Both HEADs match the audit; both worktrees contain existing local work. Preserve that work. Record the revision of any future isolated checkout separately from the source checkout. Existing NOS tags/digests and stopped lab are evidence of availability only.

## B01 — credible test and CI gates (completed)

Implementation and local evidence are recorded in [B01_REPORT.md](B01_REPORT.md) and [b01-docker-verification.log](evidence/2026-10-11/b01-docker-verification.log). Changes are reviewable in `/home/arjuna/rustybmp-b01` and the [portable patch](evidence/2026-10-11/rustybmp-b01-b02.patch).

1. Isolated RustyBMP checkout in `/home/arjuna/rustybmp-b01` (`integration/b01-baseline`). `/home/arjuna/rustybmp` and its modified documentation/untracked scenario remain intact.
2. Locked dependency fetch and Rust 1.95.0 toolchain alignment verified across both repos.
3. Fresh test enumeration and execution recorded across `rbmp-core`, `rbmp-rib`, and workspace (`264` tests passing after M0+M1 additions).
4. RustyBMP CI failure masking (`|| echo`, `continue-on-error`) removed from required checks; required Python test runner fails on assertions, skips, collection errors, and zero tests.
5. Docker image (`rustybmp:b01` / `rustybmp:ci`, 134.86 MB) built, started with `/health` check, passed 24/24 API tests (`0 skipped`), and verified across `docker save`/`docker load`.
6. Bonsai CI aligned with Rust 1.95.0, native build prerequisites, and `sccache`/`mold`.
7. Pre-existing formatting/Clippy drift recorded separately without masking.

## B02 — independent wire expectations and RFC parser repairs (completed)

Verified via `python3 docs/merger/evidence/2026-10-11/inspect_fixtures.py /home/arjuna/rustybmp-b01` ([rustybmp-fixture-inspection-after.json](evidence/2026-10-11/rustybmp-fixture-inspection-after.json)) and `bmp_rfc_contract` in both repositories ([b02-rustybmp-reproductions-after.log](evidence/2026-10-11/b02-rustybmp-reproductions-after.log), [b02-bonsai-reproductions-after.log](evidence/2026-10-11/b02-bonsai-reproductions-after.log)).

- Archived original ten malformed 7-byte fixtures in `tests/fixtures/bmp/negative_legacy_7byte/` with `manifest.json` SHA-256 provenance; generated 10 valid 6-byte fixtures in `tests/fixtures/bmp/*.bin`.
- Repaired Initiation TLVs (`0=String, 1=sysDescr, 2=sysName` + unknown TLV preservation via `parse_initiation_tlvs`), Termination TLVs (`0=String, 1=Reason(u16)`), Local Instance (`peer_type=2`) `O`-flag handling, Loc-RIB contextual `F` flag (`0x80` without IPv6 `0x08` collision), and 6-byte BMP / 19-byte BGP length validation in both RustyBMP and Bonsai (`src/streaming/bmp.rs`).
- Repaired inline `ROUTE_MONITOR_PDU` withdrawal fixture (`75/27` bytes) while retaining `76/28` as a negative test.

## B03 — framing bounds, timeouts, collector & TLS hardening (completed)

- Enforced pre-allocation BMP frame bounds (`6..=max_frame_bytes`) and separate header/body read timeouts via `read_bmp_frame` in both RustyBMP (`rbmp-core`, `rbmp-server`) and Bonsai (`src/streaming/bmp.rs`).
- Capped `StatsReport` pre-allocation capacity and hardened public BGP parsers (`attributes.rs`, `capabilities.rs`, `nlri.rs`) against truncated inputs.
- Hardened `collector_protocol.rs`, `proxy.rs`, and `bin/collector.rs` with bounded frame reads and timeouts.
- Fixed `tls.rs` to register CA certificates into `RootCertStore`, build `WebPkiClientVerifier` when `client_auth_required = true`, and pass `cfg.bmp.max_connections` instead of hardcoded `256`.

## Milestone M1 (B04 -> B05 -> B06 -> B08) — Trustworthy routing core (completed)

Portable patch: [rustybmp-m1-b04-b08.patch](evidence/2026-10-11/rustybmp-m1-b04-b08.patch).

1. **B04 — Typed route, peer, view and event identities (completed)**:
   - Added `ObservationSource`, `PeerKey`, `ViewKey`, `RouteKey`, and `EventKey` in `rbmp-core::bmp::types` ([ADR-001](ADR-001-routing-identity-and-event-schema.md)).
   - Replaced `HashMap<IpAddr, RibTable>` and `HashMap<IpAddr, PeerSession>` in `rbmp-rib` with `HashMap<ViewKey, RibTable>` and `HashMap<PeerKey, PeerSession>`.
   - Upgraded `RibEvent` to `schema_version = 2` with `session_epoch`, `route_distinguisher`, `peer_bgp_id`, `path_id`, and `event_key()`.
2. **B05 — Session-aware BGP decoding and End-of-RIB (completed)**:
   - Added `BgpDecodeContext` and `FamilySupportStatus` (`Supported`, `ExperimentalParseOnly`, `Unsupported`) in `rbmp-core`.
   - Fixed 2-byte vs 4-byte `AS_PATH` decoding, `AS_TRANS` (`23456`) + `AS4_PATH` reconstruction (RFC 6793 §4.2.3), `AS4_AGGREGATOR` merge (RFC 6793 §4.2.2), `MP_REACH_NLRI` `next_hop` propagation, per-AFI/SAFI Add-Path `path_id` decoding, and all-family `EndOfRib` detection (`BgpUpdate::eor_afi_safi()`).
3. **B06 — Durable journal, DuckDB commit checking & crash recovery (completed)**:
   - Created `BmpJournal` (`crates/rbmp-store/src/journal.rs`) with `sync_data()` durability and truncated crash-tail recovery (`replay_into`).
   - Updated `rbmp-store::writer::flush_batch` to check `COMMIT` errors, roll back failed transactions, only increment `rib_events_written_total` after durable commit, and persist `EndOfRib` markers to `peer_events`.
4. **B08 — Truthful feature gating (completed)**:
   - Updated `BgpsecValidator` (`crates/rbmp-enrichment/src/bgpsec.rs`) to return `BgpsecVerdict::Unverified` when RFC 8205 signature input is absent and require real ECDSA P-256 verification when signature input is provided.
   - Removed split-brain HA leader fallbacks in RustyBMP (`crates/rbmp-server/src/ha.rs`) and Bonsai (`src/ha_coordinator.rs`) when Redis/etcd is unreachable or unconfigured, and added file-backed persistence to Bonsai's `RollbackRegistry` (`src/remediation/rollback.rs`).

## Milestone M2 (B07, B09–B15) — One usable Bonsai workflow (completed)

1. **B07 — Canonical vendor/capability registry (completed)**:
   - Unified canonical vendor IDs (`nokia_srl`, `cisco_xrd`, `cisco_iosxe`, `cisco_nxos`, `arista_eos`, `juniper_junos`, `frrouting`) across `src/platform_registry.rs`, `python/bootstrap_agent.py`, and `ui/src/lib/Onboarding.svelte`, removing silent Cisco fallback on unknown vendor strings ([b07-python-vendor-registry.log](evidence/2026-10-11/b07-python-vendor-registry.log), [b07-rust-vendor-registry.log](evidence/2026-10-11/b07-rust-vendor-registry.log)).
2. **B09 — Bonsai peer-transition ordering and correlation fixes (completed)**:
   - Fixed `src/graph/mod.rs::write_bmp_peer_state` to read prior state before `upsert_bmp_session`, composite session key (`router_address|peer_type|rd|peer_address`), and `routing_session_epoch` provenance; preserved raw `StateChangeEvent` evidence on correlation window expiry in `src/correlation_buffer.rs` ([b09-peer-and-correlation.log](evidence/2026-10-11/b09-peer-and-correlation.log)).
3. **B10 — Routing crate import into Bonsai workspace (completed)**:
   - Imported `rbmp-core`, `rbmp-rib`, and `rbmp-store` from `/home/arjuna/rustybmp-b01` (`f330feb`) into `/home/arjuna/Desktop/bonsai/crates/` with [B10_IMPORT_MANIFEST.md](B10_IMPORT_MANIFEST.md) and `RibEventJournal` (`132 passed; 0 failed; 2 ignored` in [b10-workspace-import.log](evidence/2026-10-11/b10-workspace-import.log)).
4. **B11 — Reduced routing service and versioned internal/public API (completed)**:
   - Added `bonsai-routing` (`src/routing_service/mod.rs`, `src/bin/routing_service.rs`) serving `/internal/routing/v1/{health,speakers,peers,routes,prefix-history,coverage,events,ingest-bmp}` and Bonsai proxy `/api/routing/v1/*` with graceful `503` degradation and explicit transport-gap vs BGP-withdrawal distinction ([b11-b12-b13-routing-workflow.log](evidence/2026-10-11/b11-b12-b13-routing-workflow.log)).
5. **B12 — Shared authentication boundary, RBAC & SSH host-key hardening (completed)**:
   - Enforced Bonsai JWT/API-key RBAC (`routing:read` scope, viewer/operator roles) on `/api/routing/v1/*`, internal service token on `/internal/routing/v1/*`, recursive secret redaction (`redact_secrets_in_json`), and `RejectPolicy` default SSH host-key verification in `python/bootstrap_agent.py` and `python/inject_fault.py` ([b11-b12-b13-routing-workflow.log](evidence/2026-10-11/b11-b12-b13-routing-workflow.log), [b15-ssh-host-key-policy.log](evidence/2026-10-11/b15-ssh-host-key-policy.log)).
6. **B13 — Checkpointed graph summary projection (completed)**:
   - Implemented idempotent `project_routing_events_to_graph` with durable `RoutingProjectionCheckpoint` in `GraphStore` (`ConfigItem` `routing-projection-checkpoint-v1`), projecting speaker/peer session summaries and per-peer route counts without duplicating raw route churn into `BgpRoute` ([b11-b12-b13-routing-workflow.log](evidence/2026-10-11/b11-b12-b13-routing-workflow.log)).
7. **B14 — Svelte 5 routing views in Bonsai `ui/` (completed)**:
   - Created `ui/src/routes/Routing.svelte` and wired `/routing` in `ui/src/App.svelte` with speakers, peers, RIB explorer, prefix timeline (before/after attributes), and completeness/freshness/gap/unsupported badges ([b14-ui-build.log](evidence/2026-10-11/b14-ui-build.log)).
8. **B15 — Unified Compose profile and single BMP listener authority (completed)**:
   - Added `BmpConfig::authority = "routing_service"` (`should_spawn_legacy_receiver() == false`), built `bonsai-routing` in `docker/Dockerfile.bonsai`, and added `bonsai-routing` to `docker-compose.yml` under `standalone`/`routing` profiles ([b15-ssh-host-key-policy.log](evidence/2026-10-11/b15-ssh-host-key-policy.log)).
