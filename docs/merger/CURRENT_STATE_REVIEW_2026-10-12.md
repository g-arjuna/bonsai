# Current-state review — 12 October 2026

Reviewed Bonsai `f42dcfa` on `main` and RustyBMP integration checkout `f330feb` on `integration/b01-baseline`. Bonsai matches its cached `origin/main` reference; this review did not fetch remote refs or inspect cloud CI. Both inspected worktrees were clean before review artifacts were added.

**Assessment: substantial implementation and regression coverage are present, but the blanket “M0–M3 and A00–A20 complete” status is not supported by the acceptance evidence. The repository is an integrated development prototype with unresolved routing-history failures, production wiring gaps, and simulated evaluation/assurance layers.**

This review reads the original routing handoff, the companion NetOps strategy, current status/backlog/release reports, the nine intervening Bonsai commits, and targeted implementation/test paths. It does not certify every line or every workflow. Application code, existing evidence, labs and device configuration were not changed. Temporary review tests were removed from `tests/` after execution; their source and results are preserved under [review evidence](evidence/2026-10-12-review/results.json).

## What changed

The diff from audited Bonsai `2090396` contains nine commits, 212 files, 56,580 insertions and 724 deletions, including imported code, lockfiles and evidence logs. File/line counts do not establish readiness.

| Commit | Delivered implementation |
|---|---|
| `f7bf149` | M0 protocol fixtures, parser/framing repairs and CI baseline; earlier ML/UI edits incorporated |
| `16fff23` | Typed routing identities, decode context, journals and feature gating |
| `85de666` | Platform registry, peer-transition ordering and correlation fixes |
| `4f2ba31` | Imported routing workspace, reduced service, proxy/RBAC functions, graph-projector helper and Routing UI |
| `ae4780b` | Migration/rollback helpers, synthetic replay/load/oracle tests, release documentation and justfile |
| `e25364f` | Conformal/evaluation corrections, STGNN entity alignment and investigation terminal-state/schema changes |
| `e3e7c3d` | Diagnostic/evidence helpers, prefix investigator and budget/dedup helper types |
| `1d9f9ee` | Operator adjudication UI, synthetic scenario pack and simulated policy comparison |
| `f42dcfa` | NOS parser, toy forecasting/ablation experiments, plan/assurance/executor simulation and pilot-summary helpers |

Retain the useful work: the canonical crates are imported with provenance, the wire regressions are now repaired, composite peer identities improve exporter/VRF isolation, and the main UI has routing views. The actual STGNN forward path uses ID alignment. The old conformal/point-adjustment counterexamples are regression cases. Strict RCA/terminal-state handling and purpose-built evidence tools provide a workable foundation.

The earlier `rustybmp:b01` Docker build finished successfully; its current local image ID matches the recorded `fce0778a7034...`. The subsequent B01 Docker/API/save-load log exists. That historical RustyBMP image is not a verification of the present Bonsai release or the imported routing runtime.

## Executed checks at this revision

| Check | Result |
|---|---|
| Imported routing crates: `cargo test -p rbmp-core -p rbmp-rib -p rbmp-store --locked --offline` | **132 passed, 0 failed, 2 ignored** |
| Existing B11/B12/B13, A04/A09 and A13/A20 integration targets | **12 passed, 0 failed** |
| Selected A01/A02/A14/A15 Python regressions | **10 passed, 0 failed** |
| Four new recovery/history review probes | **0 passed, 4 failed**, Cargo exit 101 |
| `cargo fmt --all -- --check` | Exit 1; 32,674 output lines |
| `cargo clippy -p rbmp-core --locked --offline -- -D warnings` | Exit 101; 24 diagnostics in `rbmp-core` |

Exact commands, logs and probe source: [evidence directory](evidence/2026-10-12-review/results.json). Passing checks are selected regression suites, not a full workspace/CI pass. No real router scenario, cloud CI, UI E2E, actual model benchmark, training run or operational shadow pilot was executed. The M3 test that writes committed evidence was inspected rather than rerun, preserving its existing artifacts.

Three existing STATUS evidence links have missing targets (`b07-python-vendor-registry.log`, `b07-rust-vendor-registry.log`, `b09-peer-and-correlation.log`). See [missing-link inventory](evidence/2026-10-12-review/missing-document-links.json). This is document drift, not proof those checks never ran; repair the references during reconciliation.

## Findings that block acceptance

**R01 — P0: accepted route updates can silently lose all history events.**

[`ingest_bmp_message`](../../src/routing_service/mod.rs) subscribes to a 4,096-capacity broadcast stream, processes the entire message, then drains it with `while let Ok(ev) = rx.try_recv()` (lines 202–214). A lag error ends that loop without reporting loss. The new probe submits a parsed batch of 5,000 distinct announcements through the accepted-message entry point: the RIB contains 5,000 routes, ingestion returns success, and the event journal gains **zero** route events. This is a service/history reproduction, not a live wire interoperability test. B06/B19 acceptance must reopen. Disposable broadcast remains on the only emitted-event path to history, contrary to the handoff.

**R02 — P0: crash recovery does not repair history from accepted observations.**

[`RoutingServiceState::new`](../../src/routing_service/mod.rs) replays the BMP journal into a new `RibManager`, discards its emitted events, then opens the separate event journal (lines 115–123). A simulated crash window after committed raw append but before event projection restores current state while leaving history empty. The new probe demonstrates this. Stable source/event identities, persisted projection checkpoints, and idempotent regeneration of missing history are still required. The “raw” journal stores decoded `BmpMessage` objects, not original wire bytes, and omits the caller's connection epoch.

**R03 — P0: a recovered torn tail corrupts the next append.**

[`RibEventJournal::open`](../../crates/rbmp-store/src/journal.rs) skips incomplete final JSON but reopens the unchanged file in append mode (lines 187–206). The new probe appends a torn tail, successfully reopens, appends another observation, then fails the following restart: the incomplete bytes were joined to the next JSON record. Reading past a crash tail is not a complete recovery procedure. BMP journal replay also leaves its skipped tail on disk. Repair/truncate/quarantine the tail before accepting further records, with explicit failure handling.

**R04 — P1: monitoring gaps disappear after restart.**

`record_transport_disconnect` updates only the in-memory map (lines 222–228), and startup constructs an empty map. The new restart probe loses the previously recorded gap. Any valid message also clears the speaker gap before a new synchronization is established. Preserve gap/session lifecycle events durably and use epoch-aware reconnect/completeness rules. Old-connection EOF currently has no epoch argument, so it can mark a newer session as gapped.

**R05 — P1: important runtime helpers are not connected to the deployed workflow.**

- `project_routing_events_to_graph` has no production caller; its callers are tests. The proxy reads/report checkpoints and lag, but does not run a projector. B13 is a tested helper, not an operating projection pipeline.
- MCP routing tools resolve an explicitly supplied or globally registered in-process routing state. `register_default_routing_service` has no production caller; deployed routing is a separate process. The real investigator's `call_tool` path supplies `None`, so the new tools return “routing service state is not registered in-process” rather than querying that process.
- `BudgetReservationLedger` is used by the scenario simulation/tests, and `ToolCallDeduplicator` by tests; neither is wired into `run_investigation`. That real loop still calls `provider.complete` after a spent-so-far check. Durable reservations, concurrency/restart/cancellation handling and real duplicate-call suppression remain open under A08.

Source: [`mcp_server.rs`](../../src/mcp_server.rs) lines 954–963/1124; [`investigation_runtime.rs`](../../src/investigation_runtime.rs) lines 252–275/759–875; [`routing_service/mod.rs`](../../src/routing_service/mod.rs) line 1069; production call-site searches in this review.

**R06 — P1: advertised scope/security boundaries remain incomplete.**

Auth still defaults to opt-in in `auth_middleware` (lines 898–912); permissive CORS remains. Compose defaults the internal token to empty, and `check_internal_token` accepts requests when no token is configured. RBAC tests demonstrate explicitly enabled auth, not secure deployment defaults. Model graph queries use the normal connection and the existing keyword denylist, materialize all result rows before the 200-row presentation cap, and a timeout does not cancel the blocking query. `tenant_id` is caller-supplied envelope metadata rather than an enforced ownership partition. Prefix-history's query accepts only prefix/limit and its response omits the peer distinguisher/VRF identity, so the same prefix across exporters/instances is not selectable there. This does not meet the end-to-end scoped-evidence contract.

Source: [`auth.rs`](../../src/http_server/auth.rs), [`mcp_server.rs`](../../src/mcp_server.rs) lines 855–888, [`diagnostics.rs`](../../src/routing_service/diagnostics.rs), [`PrefixHistoryQuery`](../../src/routing_service/mod.rs) line 340, and `docker-compose.yml` lines 38/72. No exploit or production exposure was tested.

**R07 — release evidence: B17/B18 are synthetic replay tests, not verified NOS runs.**

[`b17_b18_frr_two_clean_runs_and_multivendor_capability_matrix`](../../tests/b16_b17_b18_b19_m3_release_gates.rs) calls generated-frame helpers directly. “SR Linux” and “XRd” inputs are mutations of those generated frames (lines 325–345). The test hardcodes image strings into `host_images_verified` without Docker inspection, deployment, router CLI output or captures (lines 419 onwards). The recorded FRR 9.1.0, SR Linux 24.10.1, XRd 24.4.1 and cEOS 4.33.1F tags are absent from this host's current image listing. Useful byte-level protocol tests cannot establish version-specific NOS support. The M3 capability matrix and checked release gate overstate the evidence. `just lab-test` currently runs that synthetic test and does not deploy a lab.

**R08 — research evidence: A12/A14/A15 are simulations/toy experiments.**

- A12 runs the same deterministic prefix investigator for all four policy names, deliberately worsens one “small model” case, and uses fixed cost/latency constants. No small/strong-model API calls or measured traces exist in this comparison. The 44-case pack is useful scaffolding; its published comparison is not model-selection evidence.
- A14's challenger is a training-split mean plus a maintenance-covariate shift. It does not load or run Chronos. Resource/latency ledger values are hardcoded, not measurements from this experiment.
- A15 combines NumPy feature averages, temporal differences and adjacency multiplication. It does not run/train the implemented STGNN; `_eval_scores` selects its threshold using the same evaluation labels. It cannot establish held-out STGNN improvement or justify promotion.

Source: [`scenario_pack.rs`](../../src/routing_service/scenario_pack.rs) lines 130–151/876–896/1025 onwards; [`ablation.py`](../../python/bonsai_ml/gnn/ablation.py) lines 152–165/228–246. This review does not re-audit model-license claims.

**R09 — A17/A18/A19 are data-model simulations, not lab/operational execution.**

`verify_plan_in_digital_twin` checks caller-supplied booleans/counts, without invoking a twin, applying the diff or gathering independent pre/postchecks. `ApprovalBoundExecutor` keeps its locks, idempotency records and journal in memory; “apply” and “rollback” produce strings rather than device actions. `SignedPlanApproval` has no signature-verification path. Its hash/expiry/schema tests are useful, but do not establish authenticated, restart-safe execution. The shadow pilot is a summary over supplied records rather than operational pilot evidence. These must stay explicitly prototype-only; this review performed no device writes.

Source: [`approval.rs`](../../src/remediation/approval.rs) lines 325–415/423–475/521–631.

Additional bounded follow-ups: negotiate Add-Path direction using both OPENs instead of enabling it for any positive capability mode (compare `BgpDecodeContext::from_capabilities` and the service's capability union with [RFC 7911 section 5](https://www.rfc-editor.org/rfc/rfc7911.html#section-5)); restore connection/admission limits in the reduced listener; make diagnostic queries expose continuation/truncation beyond their first 5,000/10,000 events; distinguish heuristic `is_best` from router-reported selection; extend the envelope beyond BMP. The [adapter cursor document](../architecture/adapter_cursor_cold_start.md) remains accurate that `StreamEventsRequest` has no resume cursor: routing journals have not fixed sidecar replay globally.

## Reconciled standing and next actions

| Area | Defensible current standing |
|---|---|
| M0 / protocol foundation | Substantial implementation; imported regression checks pass. Workspace formatting and focused `rbmp-core` Clippy checks fail; full CI was not run. |
| M1 / identity and durability | Composite identities and journal primitives implemented; replay/loss/gap acceptance reopened by R01–R04. |
| M2 / native workflow | UI/API/functions implemented; graph/MCP runtime wiring and scope/defaults incomplete. UI build evidence is not UI E2E. |
| M3 / release and multivendor | Migration/replay scaffolding exists; real two-run FRR/NOS, full fault/load/migration and release acceptance pending. |
| NetOps P0 | Useful calibration/evaluation/entity-alignment/terminal-state corrections; selected regressions pass. |
| NetOps P1/P2 | Deterministic diagnostic and UI/scenario scaffolding exists; runtime integration, durable budgets and actual model comparison incomplete. |
| NetOps P3 | Parser/schema/toy-ML/assurance helpers exist; real adapter captures, trained-model ablations, twin execution and shadow pilot pending. |

Proceed with verification and repair before adding features:

1. Reconcile STATUS/release matrices and generated evidence labels with the distinctions above. Retain original artifacts as historical evidence; stop treating hardcoded NOS tags or simulated policy scores as measured results.
2. First code task: adopt the 5,000-announcement failure as a regression and remove broadcast lag from the history-authority path. Do not merely swallow lag errors or increase capacity.
3. Next code task: implement authoritative replay/checkpoints with durable lifecycle/gap records, safe torn-tail recovery and stable identities. Test both crash windows and a second restart after recovery.
4. Wire the projector and actual investigator to the private routing API; enforce scoped access and durable budget reservations in that real path.
5. Prove one real FRR announce/change/withdraw/BMP-only-disconnect/restart workflow, twice, using the actually available pinned image, CLI/capture evidence and real UI. Add one non-FRR NOS only after that gate.
6. Run actual held-out model/forecast/STGNN comparisons before promotion. Keep executor/twin/shadow helpers labelled as simulations until independent lab/pilot and recovery evidence exists.

No implementation fixes, status-row rewrites, commits or pushes were made in this review. STATUS gains a prominent link to this evidence-backed assessment while preserving the other tool's existing completion entries for reconciliation.
