# BN-007 merger/release audit — intake & first fixes, 8 November 2026

Base commit `0b89f00`; changes are uncommitted working-tree code.

## Legacy / duplicate-authority inventory (intake complete)

**Canonical routing path (kept):** `src/routing_service/` + `bonsai-routing`
binary use only `rbmp-core`/`rbmp-rib`/`rbmp-store` (`BmpJournal`,
`RibEventJournal`, `RoutingProjectionCheckpoint`). No `RibManager`/`BmpJournal`
duplicates exist under `src/`; no MRT code present (`rbmp-mrt` excluded at
import per `docs/merger/B10_IMPORT_MANIFEST.md`).

**Legacy candidates identified:**

| Path | Status | Disposition |
|---|---|---|
| `src/streaming/bmp.rs` legacy receiver + `src/graph/mod.rs` `BgpRibEntry` writers + `src/archive.rs` BMP arms | Conditionally live; weak graph identity (no PeerKey/ViewKey/path_id/epoch) | **Retire only after** routing-service→graph projection covers `BgpRibEntry` consumers or explicit deprecation — NOT retired in this pass |
| `src/routing_service/migration.rs` (B16 backup/migrate/rollback) | Library only; no CLI/HTTP/startup entry point | Wire an operator command or document manual use before claiming M3 rollback done |
| `crates/rbmp-store` `duckdb-store` feature | Compiled out everywhere; EOR retirement parity open (BN-006 note) | Decide keep-optional vs wire vs remove |
| `scripts/backup_volumes.sh`/`restore_volumes.sh` | Cover `bonsai_creds`/`bonsai_graph` volumes only | Do not cover `runtime/` routing journals — gap for BN-019 |

## Defects found and fixed this pass

1. **Duplicate-authority bypass (fixed):** `PATCH /api/settings/streaming`
   respawned the legacy BMP receiver whenever `patch.bmp` was present,
   ignoring `BmpConfig.authority`. Now gated by
   `should_spawn_legacy_receiver()` (`src/http_server/settings.rs`), matching
   startup behavior. Residual: no HTTP-level regression test exists; the
   config contract is covered by `b11_b12_b13` tests.
2. **Dead config block (fixed):** `docker/configs/standalone.toml` used a
   top-level `[bmp]` section that `Config` never reads (silently ignored,
   wrong key `listen_addr`). Corrected to `[streaming.bmp]` with `tcp_addr`.
3. **License/provenance (fixed):** root `bonsai` package had no `license`
   field (`cargo deny` failure); added `Apache-2.0`. `webpki-roots` uses
   `CDLA-Permissive-2.0` — added to `deny.toml` allow list.
   `cargo deny check licenses sources` now passes.

## Gates executed

- `cargo test -p rbmp-core -p rbmp-rib -p rbmp-store`: **135 passed, 0 failed**
  (2 docker-dependent FRR tests ignored, not counted).
- `cargo test --test b11_b12_b13_routing_workflow`: 3 passed.
- `cargo deny check licenses sources`: licenses ok, sources ok.
- `cargo fmt --check`, `git diff --check`: clean.

## Second pass (same day): remaining bounded items

- **Migration operator entry point (done):** new `bonsai-migrate` binary
  (`src/bin/bonsai_migrate.rs`) exposes `backup`, `migrate-jsonl`,
  `migrate-toml`, `rollback <manifest.json>`. Verified end-to-end on a real
  file: v1→v2 migration reported `legacy_identity_incomplete` and rollback
  restored byte-identical content (SHA-256 match).
- **Journal backup coverage (verified, no change needed):** routing journals
  live under `/app/runtime`, which is the `bonsai_graph`/`bonsai_graph_dev`
  named volume already covered by `scripts/backup_volumes.sh`. Earlier intake
  note corrected.
- **`authority` validation (done):** unrecognized `[streaming.bmp] authority`
  values now log a startup warning instead of silently selecting
  routing-service mode (`src/server_startup.rs`).
- **License/provenance (done):** added root `LICENSE` (Apache-2.0) and
  `NOTICE` attributing the RustyBMP import with manifest SHAs.

## Third pass (same day): leftover dispositions

- **Version skew (resolved):** `workspace.package.version` bumped `0.1.0` →
  `0.2.0` so vendored rbmp crates version in lockstep with bonsai.
  `cargo metadata --no-deps` validates manifests.
- **DuckDB `duckdb-store` (disposition recorded):** kept optional and disabled;
  enabling requires EOR retirement parity first — documented in
  `crates/rbmp-store/Cargo.toml`. Not wired as a durable-history authority.
- **Legacy receiver retirement:** still correctly blocked — canonical
  `RoutingProjectionCheckpoint` projection does not yet cover `BgpRibEntry`
  consumers; retirement deferred until parity is demonstrated.
- **CI/install/upgrade gates:** deferred to BN-019 scope.

## Remaining BN-007 work (not done — requires dedicated passes)
- Run declared CI/release gates and install/upgrade acceptance (BN-019 overlap).
