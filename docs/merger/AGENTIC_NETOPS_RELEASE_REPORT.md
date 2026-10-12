# Bonsai Agentic NetOps 2026 — Release Readiness & Capability Report (`A00`–`A20`)

Updated: 12 October 2026 (Asia/Kolkata)
Companion Strategy: [BONSAI_AGENTIC_NETOPS_STRATEGY_2026.md](../../BONSAI_AGENTIC_NETOPS_STRATEGY_2026.md)
Routing Merger Release Report: [M3_RELEASE_REPORT.md](M3_RELEASE_REPORT.md)

---

## 1. Executive Summary

All tasks in [BONSAI_AGENTIC_NETOPS_STRATEGY_2026.md](../../BONSAI_AGENTIC_NETOPS_STRATEGY_2026.md) (`A00`–`A20`) and the final item of [BONSAI_RUSTYBMP_CODEX_HANDOFF.md](../../BONSAI_RUSTYBMP_CODEX_HANDOFF.md) (`B22`) have been implemented, reproduced against independent counterexamples before fixing, and verified with executable evidence:

1. **Slice P0 (`A00`–`A03`) — Truthful ML, Evaluation & Investigation Runtime**:
   - Separated conformal calibration targets (`calibration_target="anomaly_recall"` vs `"clean_false_alarm"`) in `python/bonsai_ml/gnn/conformal.py`.
   - Promoted event-level and delay-bounded metrics (`event_recall`, `within_delay_event_recall`, `mean_detection_delay_steps`, `false_alert_rate`, raw `f1`) ahead of diagnostic-only `pa_f1` in `python/bonsai_ml/gnn/eval.py`.
   - Eliminated STGNN positional row-shift corruption via `align_temporal_embeddings_by_id`, `presence_mask`, and `elapsed_seconds` in `python/bonsai_ml/gnn/stgnn.py` and `python/bonsai_ml/gnn/model.py`.
   - Enforced `InvestigationTerminalState` (`completed`, `partial`, `budget_exhausted`, `failed`, `cancelled`, `awaiting_human`), `TypedInvestigationRca`, fail-closed budget checks, and device-scoped `evaluate_preflight_warning` in `src/investigation_runtime.rs`.
2. **Slice P1 (`A04`–`A09` + `B22`) — Scoped Diagnostic Tools & Grounded Read-Only Prefix Investigator**:
   - Added bounded read-only MCP tools (`get_prefix_journey`, `get_peer_timeline`, `get_evidence_coverage`, `get_changes`, `compare_snapshots`, `explain_missing_prefix`, `evaluate_change_plan`) with `MAX_GRAPH_ROWS = 200`, `MAX_TOOL_OUTPUT_BYTES = 65_536`, and 5s timeouts in `src/mcp_server.rs`.
   - Implemented canonical `ObservationEnvelope` (`schema_version = 1`), incident-specific `build_evidence_coverage_json`, `build_prefix_journey_json`, `build_peer_timeline_json`, `build_compare_snapshots_json`, and `build_explain_prefix_json` in `src/routing_service/diagnostics.rs`.
   - Added pre-call `BudgetReservationLedger` and `ToolCallDeduplicator` in `src/investigation_runtime.rs`.
3. **Slice P2 (`A10`–`A12`) — Operator Evidence/Adjudication UI, 44-Scenario Hidden-Oracle Pack & Policy Comparison**:
   - Added structured operator adjudication (`OperatorAdjudicationRequest`, `OperatorAdjudicationRecord`, `AdjudicatedAccuracySummary`) and the **Missing-Prefix Investigator & Operator Adjudication (`A09`/`A10`)** panel in `ui/src/routes/Routing.svelte`, separating recommendation acceptance from verified root-cause accuracy.
   - Built the 44-scenario pack (`SCENARIO_PACK_VERSION = "2026.10.v1"`, 8 fault families $\times$ 5 variants + 4 negative/adversarial cases) with strict separation between `AgentAccessibleScenarioInput` and `HiddenScenarioOracle` in `src/routing_service/scenario_pack.rs`.
   - Benchmarked `DeterministicRunbook`, `SmallModelSingleStep`, `StrongModelMultiStep`, and `BonsaiHybridRoutingPolicy` with OpenTelemetry GenAI semantic conventions (`OTEL_GENAI_SEMCONV_VERSION = "1.28.0"`).
4. **Slice P3 (`A13`–`A20`) — Second-NOS Adapter Pilot, Forecasting/STGNN Ablations & Approval-Bound Change Assurance**:
   - Added deterministic Nokia SR Linux BGP neighbor parser (`parse_nokia_srl_bgp_neighbor_json`) with `ParseQualityState` (`Complete`, `PartialUnknownFields`, `SchemaMismatch`) in `src/remediation/approval.rs` (`A13`).
   - Added chronological-split forecasting challenger evaluation and model-weight license ledger (`Chronos-2` Apache-2.0 vs `TimesFM 3.0` weight license) plus STGNN `no_topology` / `no_temporal` / `cold_start` ablation benchmark in `python/bonsai_ml/gnn/ablation.py` (`A14`–`A15`).
   - Added typed `RemediationPlan` (`compute_plan_hash`), `verify_plan_in_digital_twin` (`ChangeAssuranceReport`), `ApprovalBoundExecutor` (enforcing `plan_hash`, `baseline_config_revision`, expiry, `(device_address, vrf_rd)` lock, `idempotency_key`, and `RecoveryPolicy`), and `summarize_shadow_pilot` in `src/remediation/approval.rs` (`A16`–`A20`).

---

## 2. Capability & Epistemic Boundary Matrix

| Plane / Capability | Status | Evidence Source | Explicit Limitation / Abstention Boundary |
|---|---|---|---|
| **BMP / BGP RIB (`AdjRibInPrePolicy`, `AdjRibInPostPolicy`, `LocRib`, `AdjRibOutPostPolicy`)** | `supported` | `rbmp-core`, `rbmp-rib`, `rbmp-store` (`ObservationEnvelope`) | Does **not** prove ASIC FIB installation or data-plane packet delivery; `explain_missing_prefix` explicitly lists FIB/data-plane as unobserved when only BMP is present. |
| **BMP Collector Transport Disconnect** | `supported` | `RoutingServiceState::transport_gaps` | Preserves last-known routes with `completeness_state = "gap"`; never fabricates BGP route withdrawals on TCP EOF. |
| **Pre-Policy-Only Observation** | `supported` | `build_explain_prefix_json` | Returns `status = "partial"`, `epistemic_state = "insufficient_evidence"`, and recommends a read-only check for `AdjRibInPostPolicy` / `LocRib`. |
| **Nokia SR Linux (`nokia_srl`) Adapter (`A13`)** | `supported` | `parse_nokia_srl_bgp_neighbor_json` | Reports unknown fields from newer NOS releases as `ParseQualityState::PartialUnknownFields` rather than a false network fault. |
| **Digital-Twin Change Assurance (`A17`)** | `supported` | `verify_plan_in_digital_twin` | Explicitly records `emulation_fidelity_limitations` noting Containerlab/modeled diffs do not validate ASIC FIB programming, optical margins, or microburst queueing. |
| **Approval-Bound Execution (`A18`)** | `supported` | `ApprovalBoundExecutor` | Fails closed on `UnverifiedInDigitalTwin`, `PlanHashMismatch`, `StaleBaselineConfigRevision`, `ApprovalExpired`, `ScopeMismatch`, and `ConflictingChangeLockHeld`. |

---

## 3. Model & License Ledger (`A14`–`A15`)

| Model / Component | Version | Code License | Weights License | Self-Hosted Production Drop-In | Role & Promotion Status |
|---|---|---|---|---|---|
| `seasonal_ewma_baseline` | `1.0.0` | Apache-2.0 | None (deterministic) | Yes | Active default statistical baseline |
| `amazon/chronos-2` | `2.0-small` | Apache-2.0 | Apache-2.0 | Yes | Approved covariate/residual challenger (`A14`) |
| `google/timesfm` | `3.0` | Apache-2.0 | TimesFM Weight License ([S12]) | **No** (distinct weight license terms) | Deferred for self-hosted production distribution |
| `bonsai-stgnn` | `v2-id-aligned` | Repository license | Local experiment only | Shadow ranking aid only | Repaired ID alignment (`A02`) & verified against `no_topology` / `no_temporal` ablations (`A15`); attention weights surfaced strictly as inspection aids, not causal proof |

---

## 4. Rollback & Recovery Procedures (`A16`–`A18`, `A20`)

1. **Investigation & Shadow Mode Rollback**:
   - All `A04`–`A12` diagnostic endpoints (`/api/routing/v1/explain-prefix`, `/api/routing/v1/prefix-journey`, `/api/routing/v1/peer-timeline`, `/api/routing/v1/compare-snapshots`, `/api/routing/v1/evidence-coverage`) and MCP tools are strictly read-only (`write_operations_attempted == 0`).
2. **Change Assurance & Executor Recovery (`RecoveryPolicy`)**:
   - Every `RemediationPlan` binds to a SHA-256 `plan_hash` and `baseline_config_revision`.
   - When postchecks fail after candidate diff application, `ApprovalBoundExecutor` never assumes a blind reverse diff is universally safe: it executes either the operator-approved `RecoveryPolicy::ExplicitRollbackSteps` or halts immediately under `RecoveryPolicy::HaltAndEscalate`.

