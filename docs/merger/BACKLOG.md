# First routing integration actions

Canonical current state: [STATUS.md](STATUS.md). Full backlog B00–B22 and architecture contracts: [source handoff, sections 6–8](../../BONSAI_RUSTYBMP_CODEX_HANDOFF.md).

## B00 — establish the baseline

Initial inventory is recorded in [../baseline.md](../baseline.md). Both HEADs match the audit; both worktrees contain existing local work. Preserve that work. Record the revision of any future isolated checkout separately from the source checkout. Existing NOS tags/digests and stopped lab are evidence of availability only.

## B01 — credible test and CI gates (completed locally)

Implementation and local evidence are recorded in [B01_REPORT.md](B01_REPORT.md) and [b01-docker-verification.log](evidence/2026-10-11/b01-docker-verification.log). Changes are reviewable in `/home/arjuna/rustybmp-b01` and the [portable patch](evidence/2026-10-11/rustybmp-b01-b02.patch).

1. Isolated RustyBMP checkout in `/home/arjuna/rustybmp-b01` (`integration/b01-baseline`). `/home/arjuna/rustybmp` and its modified documentation/untracked scenario remain intact.
2. Locked dependency fetch and Rust 1.95.0 toolchain alignment verified across both repos.
3. Fresh test enumeration and execution recorded across `rbmp-core`, `rbmp-rib`, and workspace (`244` tests discovered after B02/B03 additions).
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

## Next slice: Milestone M1 (B04 -> B05 -> B06 -> B08) before Bonsai crate import (B10)

1. **B04 — Typed route, peer, view and event identities**: Replace `HashMap<IpAddr, RibTable>` in `rbmp-rib` with composite `ObservationSource`, `PeerKey`, `ViewKey`, `RouteKey`, and `EventKey` keys.
2. **B05 — Session-aware BGP decoding and End-of-RIB**: Thread `PeerUp` capabilities (2-byte vs 4-byte ASN, Add-Path per AFI/SAFI) and explicit `EOR` markers into `rbmp-core` and `rbmp-rib`.
3. **B06 — Durable journal, DuckDB commit checking & crash recovery**: Check `COMMIT` errors in `rbmp-store`, unify `BMP_SCHEMA_DDL` with `001_bmp_tables.sql`, and enforce bounded replay before **B10** imports `rbmp-core`, `rbmp-rib`, and `rbmp-store` into Bonsai.
4. **B08 — Truthful feature gating**: Gate placeholder BGPsec/HA/remediation paths behind read-only/unavailable status before exposing routing workflows in Bonsai (**B10–B15**).

