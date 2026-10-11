# Routing integration status

Updated: 11 October 2026 (Asia/Kolkata).

Product decision: retain Bonsai as UI, inventory, configuration and authentication authority; extract RustyBMP routing components only after protocol and recovery gates. The source handoff is [BONSAI_RUSTYBMP_CODEX_HANDOFF.md](../../BONSAI_RUSTYBMP_CODEX_HANDOFF.md).

Initial analysis recorded the environment baseline. Subsequent sessions completed **Milestone M0 (B00–B03)**: CI/toolchain/Docker gates (B01), RFC 7854/8671/9069/4271/4760 wire/TLV/view parser and fixture repairs in both RustyBMP and Bonsai (B02), and TCP framing bounds, timeouts, collector framing, TLS client-auth enforcement and public BGP parser hardening (B03). RustyBMP changes live in the isolated `/home/arjuna/rustybmp-b01` worktree on `integration/b01-baseline`; original local edits in both repositories are preserved. Neither repository was committed or pushed. See [B01_REPORT.md](B01_REPORT.md) and [machine-readable results](evidence/2026-10-11/b01-results.json).

| Task | State | Evidence / remaining work |
|---|---|---|
| **B00** (M0) | Completed | [baseline.md](../baseline.md): exact source SHAs (`bonsai` `20903963`, `rustybmp` `e2a11893`), dirty trees, tools, resources, Docker endpoints, images, labs, ports and checks. |
| **B01** (M0) | Completed (local) | Rust baseline + Docker release build (`rustybmp:b01` `sha256:fce0778a7034...`, 134.86 MB) verified; container `/health` + 24/24 API tests (`0 skipped`); `docker save`/`docker load` `rustybmp:ci` verified ([b01-docker-verification.log](evidence/2026-10-11/b01-docker-verification.log)). Pre-existing formatting/Clippy drift remains visible for a dedicated cleanup slice. |
| **B02** (M0) | Completed | Independent RFC expectations (`bmp_rfc_contract`) reproduced 8/11 Rust failures in RustyBMP and 5/6 in Bonsai before fixes ([b02-bonsai-reproductions-before.log](evidence/2026-10-11/b02-bonsai-reproductions-before.log)), then repaired Initiation/Termination TLVs, Local Instance (`peer_type=2`) `O` flag, Loc-RIB `F` flag (`0x80` vs IPv6), and 6-byte BMP / 19-byte BGP length validation in both repos. Original 10 malformed 7-byte fixtures archived under `tests/fixtures/bmp/negative_legacy_7byte/` with `manifest.json` SHA-256 provenance; regenerated 10 valid 6-byte fixtures verified by `inspect_fixtures.py` ([rustybmp-fixture-inspection-after.json](evidence/2026-10-11/rustybmp-fixture-inspection-after.json)). Python protocol/ML: **102 passed**; RustyBMP Rust workspace: **241 passed, 0 failed, 2 ignored** ([b02-rustybmp-reproductions-after.log](evidence/2026-10-11/b02-rustybmp-reproductions-after.log)). |
| **B03** (M0) | Completed | Added `validate_bmp_header` and bounded `read_bmp_frame` (header + body read timeouts, pre-allocation frame cap) in both RustyBMP (`rbmp-core`, `rbmp-server`) and Bonsai (`src/streaming/bmp.rs`). Hardened `StatsReport` pre-allocation cap, public BGP parsers (`attributes.rs`, `capabilities.rs`, `nlri.rs`), collector framing (`collector_protocol.rs`, `proxy.rs`, `bin/collector.rs`), and `tls.rs` (CA store registration, `WebPkiClientVerifier`, configurable `max_connections`). Verified by `bmp_rfc_contract` (16/16 in RustyBMP, 7/7 in Bonsai) and `rbmp-server` `receiver::tests` (4/4). |
| **B04–B06, B08** (M1) | Not started | **M1 — Trustworthy routing core**: B04 typed `ObservationSource`/`PeerKey`/`ViewKey`/`RouteKey`/`EventKey` identities (fixing `HashMap<IpAddr, RibTable>` collision F03); B05 session-aware ASN width/Add-Path/EOR; B06 durable journal & DuckDB transaction commit error handling (fixing F05); B08 truthful feature gating (BGPsec/HA/remediation). |
| **B07, B09–B15** (M2) | Not started | **M2 — One usable Bonsai workflow**: B07 vendor registry; B09 Bonsai peer-transition & correlation fixes; **B10 crate import (`rbmp-core`, `rbmp-rib`, `rbmp-store`) into Bonsai workspace**; B11 internal routing API; B12 shared auth/RBAC; B13 graph projection; B14 Svelte 5 UI port into Bonsai `ui/`; B15 unified Compose profile. |
| **B16–B22** (M3 / Later) | Not started | **M3 — Reproducible release**: B16–B20 vendor lab, throughput/crash benchmarks, Playwright E2E, docs, CI matrix; B21–B22 deferred scale-out & cryptography. |

## Corrections to the handoff

- F13's Rust test discovery assertion is incorrect at the pinned RustyBMP SHA: `crates/rbmp-core/Cargo.toml` has `[[test]] name = "integration", path = "../../tests/integration/mod.rs"`. Cargo metadata discovers it. That module includes `bmp_pdu`, `frr_bmp` and `mcp_tools`; it does not include `api_smoke` or `retention_sweep`. Verify coverage; do not add duplicate target wiring.
- The upstream audit could not execute tests. This host has Rust/build tools, both project virtual environments and a working system Docker daemon. System Python lacks pytest, but the project virtual environments have it.
- RustyBMP's modified local testing documents report earlier FRR/XRd and UI runs. Preserve that work; treat those as historical reports until rerun, and do not use them to dismiss fixture/identity/durability defects.
- Docker's default `rancher-desktop` context is unavailable. Containerlab uses the working system socket. Select the endpoint explicitly for future commands without changing the user's global context.

## Next bounded task

Start **Milestone M1 (B04 -> B05 -> B06 -> B08)** in `/home/arjuna/rustybmp-b01` before importing crates into Bonsai (B10):
1. **B04**: Replace IP-only `HashMap<IpAddr, RibTable>` in `rbmp-rib` with composite `ObservationSource`, `PeerKey`, `ViewKey`, `RouteKey`, and `EventKey` keys so multi-router, multi-VRF, pre/post-policy, Loc-RIB, and Add-Path routes never collide.
2. **B05**: Thread session negotiation state (`PeerUp` 2-byte vs 4-byte ASN and Add-Path per AFI/SAFI) and explicit End-of-RIB (`EOR`) markers through `rbmp-core` and `rbmp-rib`.
3. **B06**: Check DuckDB `COMMIT` errors (`let _ = conn.execute_batch("COMMIT");`), align `BMP_SCHEMA_DDL` with `001_bmp_tables.sql`, and implement bounded replay/recovery before crate import into Bonsai.
4. Decide with the user whether to commit/branch the completed M0 baseline (`B00–B03`) in Bonsai and `/home/arjuna/rustybmp-b01`.

