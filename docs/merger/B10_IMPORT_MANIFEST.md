# B10 Routing Crate Import Manifest

## 1. Provenance and Source Revisions

- **Audited upstream RustyBMP SHA**: `e2a1189392db520b25a376795e1bfae72eca5b55`
- **Hardened integration source SHA (`/home/arjuna/rustybmp-b01`, branch `integration/b01-baseline`)**: `f330febcca2f2870d4dd29526057385abc4f308a`
  - Includes Milestone M0 (`B01`–`B03`), Milestone M1 (`B04`–`B06`, `B08`), and `B07` protocol/identity/durability/capability fixes.
- **Target Bonsai pre-import SHA**: `85de666` (`main`)
- **Upstream license**: `Apache-2.0` (`authors = ["Arjuna Ganesan"]`)
- **Preserved pre-merge lockfiles**:
  - `docs/merger/rustybmp-Cargo.lock.orig` (exact `Cargo.lock` from `/home/arjuna/rustybmp-b01` at `f330febcca2f2870d4dd29526057385abc4f308a`)
  - `docs/merger/bonsai-Cargo.lock.orig` (exact `Cargo.lock` from `/home/arjuna/Desktop/bonsai` at `85de666`)

## 2. File and Crate Mapping

| RustyBMP source path (`f330febcca2f2870d4dd29526057385abc4f308a`) | Bonsai target path | Notes |
|---|---|---|
| `crates/rbmp-core/**` | `crates/rbmp-core/**` | RFC 7854 / 8671 / 9069 BMP & BGP protocol parser, typed identities (`ObservationSource`, `PeerKey`, `ViewKey`, `RouteKey`, `EventKey`), session-aware `BgpDecodeContext`, `FamilySupportStatus`, gated `PathMarkingProfile`. |
| `benches/bmp_parse.rs` | `crates/rbmp-core/benches/bmp_parse.rs` | Moved inside `crates/rbmp-core/` so the crate is self-contained inside the Bonsai workspace. |
| `tests/integration/**` | `crates/rbmp-core/tests/integration/**` | `bmp_pdu`, `frr_bmp`, and `mcp_tools` integration test modules. |
| `tests/regressions/bmp_rfc_contract.rs` | `crates/rbmp-core/tests/regressions/bmp_rfc_contract.rs` | B02/B03/B05 independent RFC wire contract regression suite. |
| `tests/fixtures/bmp/**` | `crates/rbmp-core/tests/fixtures/bmp/**` | 10 valid 6-byte BMP fixtures + `negative_legacy_7byte/` malformed corpus and `manifest.json`. |
| `crates/rbmp-rib/**` | `crates/rbmp-rib/**` | Composite-keyed in-memory RIB engine (`RibManager`, `RibTable`, `BmpSession`, `PeerSession`, `RibEvent` v2, YAML/Roto filter chain). |
| `crates/rbmp-store/**` | `crates/rbmp-store/**` | Durable `BmpJournal` + `RibEventJournal` (`PrefixHistoryItem` before/after reconstruction, sequence cursor replay); DuckDB modules (`duck`, `schema`, `writer`, `query`) gated under optional `duckdb-store` feature. |

## 3. Explicitly Excluded Components (B10 Scope Boundary)

Per Section 6 of `BONSAI_RUSTYBMP_CODEX_HANDOFF.md`, the following RustyBMP components are **not** imported into the default Bonsai runtime:
- `crates/rbmp-kafka` (`rdkafka` / CMake native build)
- `crates/rbmp-nats` (`async-nats`)
- `crates/rbmp-enrichment` (external RPKI/BGPsec/Whois/GeoIP enrichment daemons)
- `crates/rbmp-mrt` (offline MRT dump importer)
- `bmppy/` Python ML anomaly detector sidecar
- `crates/rbmp-server` standalone UI, JWT login, and duplicate device inventory (replaced by the reduced Bonsai routing service `bonsai-routing` and Bonsai's `/api/routing/v1/*` proxy surface in B11–B15).

