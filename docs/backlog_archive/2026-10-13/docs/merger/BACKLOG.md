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

## Milestone M2 (B07, B09–B15) — One usable Bonsai workflow (completed; live UI/FRR lab R07 pending)

- **B07**: Canonical vendor/capability registry (`src/platform_registry.rs`, `python/bootstrap_agent.py`, `ui/src/lib/Onboarding.svelte`).
- **B09**: Bonsai peer-transition ordering (`src/graph/mod.rs`) and correlation window raw-evidence preservation (`src/correlation_buffer.rs`).
- **B10**: Workspace crate import (`crates/{rbmp-core,rbmp-rib,rbmp-store}`) with [B10_IMPORT_MANIFEST.md](B10_IMPORT_MANIFEST.md) and `RibEventJournal`.
- **B11–B13**: Reduced `bonsai-routing` service (`src/routing_service/mod.rs`), Bonsai RBAC & `RoutingAccessScope` proxy (`/api/routing/v1/*`), SSH `RejectPolicy` default, and checkpointed graph summary projector wired into `src/server_startup.rs` and `routing_v1_proxy_get_handler` ([b11-b12-b13-routing-workflow.log](evidence/2026-10-11/b11-b12-b13-routing-workflow.log), [r05-r06-verification.log](evidence/2026-10-12-remediation/r05-r06-verification.log)).
- **B14–B15**: Svelte 5 routing workspace (`ui/src/routes/Routing.svelte`) and unified Compose profile (`bonsai-standalone` + `bonsai-routing`).

## Milestone M3 (B16–B20) — Reproducible personal release (prototype scaffolding; live lab R07 pending)

1. **B16 — Versioned config/DB migration with backup and rollback (completed)**:
   - Implemented `create_backup`, `migrate_routing_jsonl_file`, `migrate_bonsai_toml_file`, and `rollback_from_backup` in `src/routing_service/migration.rs`.
   - Verified v1->v2 migration with `legacy_identity_incomplete = true` quality flags and byte-for-byte SHA-256 rollback in `tests/b16_b17_b18_b19_m3_release_gates.rs` ([b16-b19-m3-release-gates.log](evidence/2026-10-11/b16-b19-m3-release-gates.log)).
2. **B17 — FRR expected-state oracle and two-run reproducibility (synthetic PDU replay verified; live FRR lab pending under R07)**:
   - Verified two clean runs of in-process synthetic BMP frame replay against `ExpectedRouteOracle` ([manifest.json](evidence/2026-10-11/b17-b18-evidence-bundle/manifest.json), [results.json](evidence/2026-10-11/b17-b18-evidence-bundle/results.json)). Live Containerlab FRR execution with router CLI, packet capture, and UI verification is tracked under `R07`.
3. **B18 — Non-FRR NOS verification & capability matrix (synthetic PDU mutation only; live NOS pending under R07)**:
   - Verified byte-level BMP PDU decoding for RFC 9069 Loc-RIB (`peer_type=3`) and RFC 8671 `RdInstance` (`peer_type=1` `AdjRibOutPostPolicy`). Live container verification is tracked under `R07`.
4. **B19 — Fault, replay, and load verification (repaired by R01–R04)**:
   - Verified 1,000-route and 5,000-route deterministic bursts, withdrawals, malformed frames, on-disk torn crash-tail truncation (`BmpJournal` & `RibEventJournal`), crash-window event recovery, and durable epoch-fenced transport gaps ([b16-b19-m3-release-gates.log](evidence/2026-10-11/b16-b19-m3-release-gates.log), [r01-r04-reproductions-after.log](evidence/2026-10-12-remediation/r01-r04-reproductions-after.log)).
5. **B20 — Unified developer workflow, docs, and release checklist (reconciled under R00)**:
   - Created root [AGENTS.md](../../AGENTS.md), [justfile](../../justfile), workspace CI steps in `.github/workflows/ci.yml`, and reconciled [M3_RELEASE_REPORT.md](M3_RELEASE_REPORT.md).

## Strategy Slice P0 (A00–A03) — Truthful ML, evaluation & investigation runtime (completed)

1. **A00 — AI/ML & investigation runtime baseline reconciliation (completed)**:
   - Recorded exact line-level audit of `conformal.py`, `eval.py`, `stgnn.py`, `model.py`, and `src/investigation_runtime.rs` in [A00_AI_ML_BASELINE.md](A00_AI_ML_BASELINE.md).
2. **A01 — Conformal calibration & TSAD evaluation metrics (completed)**:
   - Reproduced both Strategy §3 counterexamples ([a01-reproductions-before.log](evidence/2026-10-11/a01-reproductions-before.log)) and fixed `python/bonsai_ml/gnn/conformal.py` + `python/bonsai_ml/gnn/eval.py` ([a01-reproductions-after.log](evidence/2026-10-11/a01-reproductions-after.log)).
3. **A02 — STGNN temporal entity alignment by stable ID (completed)**:
   - Reproduced entity row-shift corruption ([a02-reproductions-before.log](evidence/2026-10-11/a02-reproductions-before.log)) and implemented `align_temporal_embeddings_by_id` with `presence_mask` and `elapsed_seconds` in `python/bonsai_ml/gnn/stgnn.py` and `node_ids` for all 8 node types in `python/bonsai_ml/gnn/model.py` ([a02-reproductions-after.log](evidence/2026-10-11/a02-reproductions-after.log)).
4. **A03 — Typed investigation results & honest terminal states (completed)**:
   - Reproduced malformed RCA JSON acceptance and global-quality masking of device-specific gaps ([a03-reproductions-before.log](evidence/2026-10-11/a03-reproductions-before.log)) and implemented `InvestigationTerminalState`, `TypedInvestigationRca`, `resolve_investigation_outcome`, fail-closed budget check, and device-scoped `evaluate_preflight_warning` in `src/investigation_runtime.rs` ([a03-reproductions-after.log](evidence/2026-10-11/a03-reproductions-after.log)).

## Strategy Slice P1 (A04–A09 + B22) — Scoped routing diagnostic tools & grounded prefix investigator (completed; repaired by R05/R06)

1. **A04 — Bounded scoped diagnostic tools (completed; repaired by R05/R06)**:
   - Registered `get_prefix_journey`, `get_peer_timeline`, `get_evidence_coverage`, `get_changes`, `compare_snapshots`, and `explain_missing_prefix` in `src/mcp_server.rs`, enforcing `.take(MAX_GRAPH_ROWS + 1)` (`MAX_GRAPH_ROWS = 200`), `MAX_TOOL_OUTPUT_BYTES = 65536`, 5s query timeouts, and remote HTTP fallback to `bonsai-routing` ([r05-r06-verification.log](evidence/2026-10-12-remediation/r05-r06-verification.log)).
2. **A05–A07 — Versioned observation envelope, incident-specific coverage & prefix journey (completed for BMP)**:
   - Implemented `ObservationEnvelope` (`schema_version = 1`), `build_evidence_coverage_json`, `build_prefix_journey_json`, `build_peer_timeline_json`, and `build_compare_snapshots_json` in `src/routing_service/diagnostics.rs` and wired `/internal/routing/v1/*` + `/api/routing/v1/*` routes ([a04-a09-reproductions-after.log](evidence/2026-10-11/a04-a09-reproductions-after.log)).
3. **A08–A09 & B22 — Pre-call budget reservation & grounded read-only prefix investigator (completed; repaired by R05)**:
   - Implemented `BudgetReservationLedger` and `ToolCallDeduplicator` in `src/investigation_runtime.rs`, wired both into `run_investigation_with_provider`, and implemented `build_explain_prefix_json` in `src/routing_service/diagnostics.rs` ([a04-a09-reproductions-after.log](evidence/2026-10-11/a04-a09-reproductions-after.log), [r05-r06-verification.log](evidence/2026-10-12-remediation/r05-r06-verification.log)).

## Strategy Slice P2 (A10–A12) — Operator evidence/feedback UI, scenario runner with hidden oracles & model/prompt trace comparison (simulated policies; R08 live models pending)

1. **A10 — Operator evidence & structured adjudication UI/API (completed)**:
   - Added `OperatorAdjudicationRequest`, `OperatorAdjudicationRecord`, `AdjudicatedAccuracySummary`, `/internal/routing/v1/adjudicate-prefix`, `/internal/routing/v1/adjudications`, and `/api/routing/v1/*` proxy routes, plus the "Missing-Prefix Investigator & Operator Adjudication (A09 / A10)" view in `ui/src/routes/Routing.svelte` ([a10-a12-reproductions-after.log](evidence/2026-10-11/a10-a12-reproductions-after.log)).
2. **A11 — Executable 44-scenario pack with hidden oracles across all 8 fault families (completed)**:
   - Implemented `build_starter_scenario_pack(seed)` (`SCENARIO_PACK_VERSION = "2026.10.v1"`, 44 scenarios) with strict separation between `AgentAccessibleScenarioInput` and `HiddenScenarioOracle` in `src/routing_service/scenario_pack.rs`.
3. **A12 — Model / prompt / policy trace comparison (completed; repaired by R08)**:
   - Implemented `compare_investigator_policies_on_pack` (`OTEL_GENAI_SEMCONV_VERSION = "1.28.0"`) with explicit `PolicyExecutionMode` (`MeasuredInProcessRunbook`, `MeasuredHybridRoutingService`, `SimulatedModelTraceScaffold`), measured wall-clock latencies, and scope notes in `src/routing_service/scenario_pack.rs` ([a11-a12-scenario-benchmark-report.json](evidence/2026-10-11/a11-a12-scenario-benchmark-report.json)).
 
## Strategy Slice P3 (A13–A20) — Second-NOS adapter pilot, forecasting/STGNN ablations, approval-bound change assurance & release readiness (completed; repaired by R08/R09)
 
1. **A13 — Second-NOS adapter workshop pilot (fixture parser verified)**:
   - Implemented deterministic Nokia SR Linux BGP neighbor parser (`parse_nokia_srl_bgp_neighbor_json`) with `ParseQualityState` (`Complete`, `PartialUnknownFields`, `SchemaMismatch`) in `src/remediation/approval.rs`.
2. **A14–A15 — Forecasting challenger & STGNN ablation benchmarks (completed; repaired by R08)**:
   - Implemented `chronological_train_val_test_split`, measured covariate residual challenger vs univariate EWMA with validation-clean threshold calibration and recorded latency, foundation-model weight-license ledger, and `run_stgnn_ablation_experiment` + `run_pytorch_stgnn_held_out_experiment` (`STGNNModel` forward pass, attention extraction, and held-out test evaluation) in `python/bonsai_ml/gnn/ablation.py`.
3. **A16–A20 — Typed `RemediationPlan`, digital-twin verification gate, approval-bound executor, shadow pilot & release report (completed; repaired by R09)**:
   - Implemented `RemediationPlan` (`compute_plan_hash`), `verify_plan_in_digital_twin` + MCP `evaluate_change_plan` with typed `AssuranceFidelity` (`ModeledSnapshotSimulation` vs `LiveContainerlabVerified`), `ApprovalBoundExecutor` + `SignedPlanApproval` with HMAC-SHA256 constant-time verification, restart persistence, and `ShadowPilotAuditJournal` in `src/remediation/approval.rs` and `src/mcp_server.rs`.
 
## Post-Review Remediation Queue (`R00`–`R10`)
 
See [CURRENT_STATE_REVIEW_2026-10-12.md](CURRENT_STATE_REVIEW_2026-10-12.md) and [STATUS.md](STATUS.md):
- **R00–R10 (All Completed & Verified)**:
  - Reconciled status/release documentation, repaired missing B07/B09 log links (`R00`).
  - Removed broadcast lag from `ingest_bmp_message` (`R01`).
  - Added deterministic `RibEvent` UUIDs and missing-event recovery on restart (`R02`).
  - Truncated torn crash tails on disk in `BmpJournal` and `RibEventJournal` (`R03`).
  - Persisted epoch-fenced transport disconnect gaps (`R04`).
  - Wired `project_remote_routing_events_to_graph`, remote MCP routing HTTP fallback, and `BudgetReservationLedger` + `ToolCallDeduplicator` into `run_investigation_with_provider` (`R05`).
  - Enforced `speaker`/`vrf_rd` scoping, `RoutingAccessScope` token checks, bounded Cypher row materialization, and required `BONSAI_ROUTING_SERVICE_TOKEN` (`R06`).
  - Verified live Containerlab FRR two-run workflow with eBGP peering, BMP Initiation/PeerUp, 3-view route announcement, attribute change, withdrawal, TCP EOF gap preservation, epoch 2 reconnect, and reproducible digests (`R07`).
  - Reconciled model comparison with explicit execution modes, measured wall-clock latencies, validation-clean threshold calibration, and PyTorch STGNN held-out evaluation (`R08`).
  - Added typed digital-twin fidelity, HMAC-SHA256 constant-time approval signature verification, persistent file-backed executor journaling with crash restart recovery, and durable shadow pilot audit logs (`R09`).
  - Resolved all Clippy diagnostics in `rbmp-core` and workspace formatting (`R10`).





