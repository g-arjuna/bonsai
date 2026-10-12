# First routing integration actions

Canonical current state: [STATUS.md](STATUS.md). Release report & Definition of Done: [M3_RELEASE_REPORT.md](M3_RELEASE_REPORT.md). Full backlog B00–B22 and architecture contracts: [source handoff, sections 6–8](../../BONSAI_RUSTYBMP_CODEX_HANDOFF.md).

## B00 — establish the baseline (completed)

Initial inventory is recorded in [../baseline.md](../baseline.md). Both HEADs match the audit; both worktrees contain existing local work. Preserve that work. Record the revision of any future isolated checkout separately from the source checkout. Existing NOS tags/digests and stopped lab are evidence of availability only.

## B01 — credible test and CI gates (completed)

Implementation and local evidence are recorded in [B01_REPORT.md](B01_REPORT.md) and [b01-docker-verification.log](evidence/2026-10-11/b01-docker-verification.log). Changes are reviewable in `/home/arjuna/rustybmp-b01` and the [portable patch](evidence/2026-10-11/rustybmp-b01-b02.patch).

## B02 — independent wire expectations and RFC parser repairs (completed)

Verified via `python3 docs/merger/evidence/2026-10-11/inspect_fixtures.py /home/arjuna/rustybmp-b01` ([rustybmp-fixture-inspection-after.json](evidence/2026-10-11/rustybmp-fixture-inspection-after.json)) and `bmp_rfc_contract` in both repositories ([b02-rustybmp-reproductions-after.log](evidence/2026-10-11/b02-rustybmp-reproductions-after.log), [b02-bonsai-reproductions-after.log](evidence/2026-10-11/b02-bonsai-reproductions-after.log)).

## B03 — framing bounds, timeouts, collector & TLS hardening (completed)

Enforced pre-allocation BMP frame bounds (`6..=max_frame_bytes`) and separate header/body read timeouts via `read_bmp_frame` in both RustyBMP (`rbmp-core`, `rbmp-server`) and Bonsai (`src/streaming/bmp.rs`). Hardened `StatsReport`, BGP parsers, collector framing, and `tls.rs`.

## Milestone M1 (B04 -> B05 -> B06 -> B08) — Trustworthy routing core (completed)

Portable patch: [rustybmp-m1-b04-b08.patch](evidence/2026-10-11/rustybmp-m1-b04-b08.patch).
- **B04**: Typed `ObservationSource`, `PeerKey`, `ViewKey`, `RouteKey`, and `EventKey` ([ADR-001](ADR-001-routing-identity-and-event-schema.md)).
- **B05**: Session-aware `BgpDecodeContext` and `FamilySupportStatus`.
- **B06**: Durable `BmpJournal`, DuckDB commit checking, and truncated crash-tail recovery.
- **B08**: Truthful BGPsec (`BgpsecVerdict::Unverified` without RFC 8205 signature input), no split-brain HA self-promotion, and persistent `RollbackRegistry`.

## Milestone M2 (B07, B09–B15) — One usable Bonsai workflow (completed)

- **B07**: Canonical vendor/capability registry (`src/platform_registry.rs`, `python/bootstrap_agent.py`, `ui/src/lib/Onboarding.svelte`).
- **B09**: Bonsai peer-transition ordering (`src/graph/mod.rs`) and correlation window raw-evidence preservation (`src/correlation_buffer.rs`).
- **B10**: Workspace crate import (`crates/{rbmp-core,rbmp-rib,rbmp-store}`) with [B10_IMPORT_MANIFEST.md](B10_IMPORT_MANIFEST.md) and `RibEventJournal`.
- **B11–B13**: Reduced `bonsai-routing` service (`src/routing_service/mod.rs`), Bonsai RBAC proxy (`/api/routing/v1/*`), SSH `RejectPolicy` default, and checkpointed graph summary projector ([b11-b12-b13-routing-workflow.log](evidence/2026-10-11/b11-b12-b13-routing-workflow.log)).
- **B14–B15**: Svelte 5 routing workspace (`ui/src/routes/Routing.svelte`) and unified Compose profile (`bonsai-standalone` + `bonsai-routing`).

## Milestone M3 (B16–B20) — Reproducible personal release (completed)

1. **B16 — Versioned config/DB migration with backup and rollback (completed)**:
   - Implemented `create_backup`, `migrate_routing_jsonl_file`, `migrate_bonsai_toml_file`, and `rollback_from_backup` in `src/routing_service/migration.rs`.
   - Verified v1->v2 migration with `legacy_identity_incomplete = true` quality flags and byte-for-byte SHA-256 rollback in `tests/b16_b17_b18_b19_m3_release_gates.rs` ([b16-b19-m3-release-gates.log](evidence/2026-10-11/b16-b19-m3-release-gates.log)).
2. **B17 — FRR expected-state oracle and two-run reproducibility (completed)**:
   - Verified two clean runs of announce -> attribute change -> withdraw -> BMP disconnect gap -> collector reconnect against `ExpectedRouteOracle`, producing identical SHA-256 route-set digests and machine-readable [manifest.json](evidence/2026-10-11/b17-b18-evidence-bundle/manifest.json) + [results.json](evidence/2026-10-11/b17-b18-evidence-bundle/results.json).
3. **B18 — Non-FRR NOS verification & capability matrix (completed)**:
   - Verified Nokia SR Linux (`nokia_srl`, RFC 9069 Loc-RIB `peer_type=3`) and Cisco IOS XRd (`cisco_xrd`, RFC 8671 `RdInstance` `peer_type=1` `AdjRibOutPostPolicy`) alongside FRR and published the version-specific capability matrix in [M3_RELEASE_REPORT.md](M3_RELEASE_REPORT.md).
4. **B19 — Fault, replay, and load verification (completed)**:
   - Verified 1,000-route deterministic burst + 200 withdrawals + 10 malformed frames + truncated crash-tail recovery + bounded pagination (`limit <= 500`) + checkpointed graph projection in [b16-b19-m3-release-gates.log](evidence/2026-10-11/b16-b19-m3-release-gates.log).
5. **B20 — Unified developer workflow, docs, and release checklist (completed)**:
   - Created root [AGENTS.md](../../AGENTS.md), [justfile](../../justfile), workspace CI steps in `.github/workflows/ci.yml`, and [M3_RELEASE_REPORT.md](M3_RELEASE_REPORT.md).
