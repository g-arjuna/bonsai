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

## Strategy Slice P0 (A00–A03) — Truthful ML, evaluation & investigation runtime (completed)

1. **A00 — AI/ML & investigation runtime baseline reconciliation (completed)**:
   - Recorded exact line-level audit of `conformal.py`, `eval.py`, `stgnn.py`, `model.py`, and `src/investigation_runtime.rs` in [A00_AI_ML_BASELINE.md](A00_AI_ML_BASELINE.md).
2. **A01 — Conformal calibration & TSAD evaluation metrics (completed)**:
   - Reproduced both Strategy §3 counterexamples ([a01-reproductions-before.log](evidence/2026-10-11/a01-reproductions-before.log)) and fixed `python/bonsai_ml/gnn/conformal.py` + `python/bonsai_ml/gnn/eval.py` ([a01-reproductions-after.log](evidence/2026-10-11/a01-reproductions-after.log)).
3. **A02 — STGNN temporal entity alignment by stable ID (completed)**:
   - Reproduced entity row-shift corruption ([a02-reproductions-before.log](evidence/2026-10-11/a02-reproductions-before.log)) and implemented `align_temporal_embeddings_by_id` with `presence_mask` and `elapsed_seconds` in `python/bonsai_ml/gnn/stgnn.py` and `node_ids` for all 8 node types in `python/bonsai_ml/gnn/model.py` ([a02-reproductions-after.log](evidence/2026-10-11/a02-reproductions-after.log)).
4. **A03 — Typed investigation results & honest terminal states (completed)**:
   - Reproduced malformed RCA JSON acceptance and global-quality masking of device-specific gaps ([a03-reproductions-before.log](evidence/2026-10-11/a03-reproductions-before.log)) and implemented `InvestigationTerminalState`, `TypedInvestigationRca`, `resolve_investigation_outcome`, fail-closed budget check, and device-scoped `evaluate_preflight_warning` in `src/investigation_runtime.rs` ([a03-reproductions-after.log](evidence/2026-10-11/a03-reproductions-after.log)).

## Strategy Slice P1 (A04–A09 + B22) — Scoped routing diagnostic tools & grounded prefix investigator (completed)

1. **A04 — Bounded scoped diagnostic tools (completed)**:
   - Registered `get_prefix_journey`, `get_peer_timeline`, `get_evidence_coverage`, `get_changes`, `compare_snapshots`, and `explain_missing_prefix` in `src/mcp_server.rs`, enforcing `MAX_GRAPH_ROWS = 200`, `MAX_TOOL_OUTPUT_BYTES = 65536`, and 5s query timeouts.
2. **A05–A07 — Versioned observation envelope, incident-specific coverage & prefix journey (completed)**:
   - Implemented `ObservationEnvelope` (`schema_version = 1`), `build_evidence_coverage_json`, `build_prefix_journey_json`, `build_peer_timeline_json`, and `build_compare_snapshots_json` in `src/routing_service/diagnostics.rs` and wired `/internal/routing/v1/*` + `/api/routing/v1/*` routes.
   - Reproduced and fixed global coverage masking when a connected speaker has 0 peers ([a04-a09-reproductions-before.log](evidence/2026-10-11/a04-a09-reproductions-before.log)).
3. **A08–A09 & B22 — Pre-call budget reservation & grounded read-only prefix investigator (completed)**:
   - Implemented `BudgetReservationLedger` and `ToolCallDeduplicator` in `src/investigation_runtime.rs` and `build_explain_prefix_json` in `src/routing_service/diagnostics.rs`, verified across supported diagnosis (`import_policy_rejected_before_loc_rib`), contradicted hypothesis (`peer_session_down`), collector transport gap abstention, and pre-policy-only / FIB abstention in `tests/a04_a09_b22_prefix_investigator.rs` ([a04-a09-reproductions-after.log](evidence/2026-10-11/a04-a09-reproductions-after.log)).

## Strategy Slice P2 (A10–A12) — Operator evidence/feedback UI, scenario runner with hidden oracles & model/prompt trace comparison (completed)

1. **A10 — Operator evidence & structured adjudication UI/API (completed)**:
   - Reproduced legacy binary feedback counting recommendation acceptance (`"positive"`) with `"unknown"` root-cause diagnosis as 100% verified RCA accuracy ([a10-a12-reproductions-before.log](evidence/2026-10-11/a10-a12-reproductions-before.log)).
   - Added `OperatorAdjudicationRequest`, `OperatorAdjudicationRecord`, `AdjudicatedAccuracySummary`, `/internal/routing/v1/adjudicate-prefix`, `/internal/routing/v1/adjudications`, and `/api/routing/v1/*` proxy routes (`src/routing_service/diagnostics.rs`, `src/routing_service/mod.rs`, `src/http_server/mod.rs`), plus the "Missing-Prefix Investigator & Operator Adjudication (A09 / A10)" view in `ui/src/routes/Routing.svelte`.
2. **A11 — Executable 44-scenario pack with hidden oracles across all 8 fault families (completed)**:
   - Implemented `build_starter_scenario_pack(seed)` (`SCENARIO_PACK_VERSION = "2026.10.v1"`, 8 fault families $\times$ 5 variants + 4 negative/adversarial cases = 44 scenarios) with strict separation between `AgentAccessibleScenarioInput` and `HiddenScenarioOracle` in `src/routing_service/scenario_pack.rs`.
3. **A12 — Model / prompt / policy trace comparison with OpenTelemetry GenAI lineage (completed)**:
   - Implemented `compare_investigator_policies_on_pack` (`OTEL_GENAI_SEMCONV_VERSION = "1.28.0"`) comparing `DeterministicRunbook`, `SmallModelSingleStep`, `StrongModelMultiStep`, and `BonsaiHybridRoutingPolicy` across all 44 scenarios ([a11-a12-scenario-benchmark-report.json](evidence/2026-10-11/a11-a12-scenario-benchmark-report.json), [a10-a12-reproductions-after.log](evidence/2026-10-11/a10-a12-reproductions-after.log)).

## Next Slice P3 (A13–A14) — Approval-gated change planning & digital-twin verification gate (only after P0–P2 pass)



