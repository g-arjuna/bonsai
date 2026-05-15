# Bonsai — Chunked Code Review and Findings

> Authored 2026-05-15. **This is a review document, not a backlog.** The operator asked for "a more detailed chunked review that understand the whole architecture and a detailed analysis before jumping on to backlog writing." This is that work, written so the operator can read it, agree or disagree with the findings, and then we shape the D-series together.
>
> The review is structured as I actually did it: walking the repo in chunks, recording observations at each one, surfacing concrete findings as they emerged. The order is preserved on purpose — earlier chunks frame later ones. If you skip to the findings list and skip the chunks, you'll get the right items but miss the path that led to them, which matters because three of the findings are not the ones I would have flagged at the start.

---

## What I read and in what order

| Chunk | Subject | Where it led |
|---|---|---|
| 1 | Top-level repo shape, sizes, surface | Found `main.rs` and `http_server.rs` are very large; bonpy is a separate UI; docs/test_results is substantial |
| 2 | Bonpy UI + sidecar registry — the integration the operator named | Found the CV7 ADR retiring `event_detection.rs`; found `python/collector_engine.py` is the rules sidecar |
| 3 | Latest CV7 validation report — what actually ran | Found three real FAILs and three WARNs; the validation infrastructure is honest, the code under test is not yet right |
| 4 | The bonsai HTTP server bind site in main.rs | Found the critical silent-panic finding — HTTP server panics inside a spawned task, main process keeps running |
| 5 | Detection paths in the current snapshot | Found event_detection.rs still wired despite ADR retiring it; ADR's gate ("after live verification") has not been met |
| 6 | Sidecar verification gap | Found bonsai-sidecar.log is empty — the rules sidecar never started in the validation runs |
| 7 | Documentation and process artefacts | Found docs are 2.1 MB; significant cognitive load risk; identified what to consolidate |
| 8 | What is NOT touched at all (gaps the prior reviews missed) | Identified four areas the prior reviews had no opinion on |

The full chunk-by-chunk record follows. After the chunks comes the consolidated findings list with severity, then the proposed shape of the D-series.

---

## Chunk 1 — Repository shape

The repo is 61 MB on disk (down from 114 MB previously — the operator has been curating). Major source surfaces:

- `src/main.rs` — **2,515 lines**. That is roughly 25× a typical Rust main.rs. Likely doing far more than process orchestration; mixing CLI subcommands, server startup, configuration, lifecycle.
- `src/http_server.rs` — **7,779 lines**. Single-file router for the entire HTTP API. Mixes routing, handlers, schema, examples, OpenAPI generation.
- `src/graph/` — 292 KB of source
- `src/enrichment/` — 124 KB
- `src/output/` — 112 KB
- `src/streaming/` — 64 KB
- `src/signals/` — 76 KB
- `src/synthesizer/` — 32 KB

Two UIs are present:
- `ui/` (620 KB) — the original bonsai UI, Svelte
- `ui-bonpy/` (100 KB) — a smaller, newer Svelte SPA called bonpy

Other surfaces:
- `python/` — 684 KB. Multiple entry points (`collector_engine.py`, `inject_fault.py`, `train_anomaly.py`, `train_remediation.py`, `soak_test.py`, `gen_protos.py`, `example.py`, `demo_phase4.py`, `demo_phase5.py`).
- `docs/` — 2.1 MB. **Larger than the entire source tree.**
- `docs/test_results/` — 1.1 MB of run artefacts. Worth distinguishing fresh from stale.
- `docs/backlog_archive/` — 388 KB. CV5's plan to archive old backlogs did land.
- `pre_cv2_freeze_*/` — 53 MB still in the repo. Was supposed to be a one-shot restoration point. Should be deleted now.
- `memory/` — 28 KB. A separate doc directory I had not seen before. Contains `MEMORY.md` and `project_sprint_progress.md`.
- `playbooks/` — 132 KB. A directory of operational playbooks (`DAY2_OPERATIONS_MATRIX.md`, `FUTURE_DETECTION_CANDIDATES.md`, library subdirectory). Also not previously surfaced in my reviews.

**Observation Q-1**: `main.rs` is 2,515 lines and `http_server.rs` is 7,779 lines. These are not normal sizes. Large files are correlated with three problems we keep hitting: AI agents miss critical bugs, refactors are scary, and operational state gets entangled. The Rust convention here is small main, push logic into modules. This is fixable mechanically without changing behaviour.

**Observation Q-2**: there is real documentation sprawl — 2.1 MB of docs, larger than the source tree. CV7 Tier 5 tried to address this with `docs/CANONICAL.md`. The CANONICAL doc exists (16 KB) but I have no signal yet whether it has actually displaced reading other files for new AI sessions. Worth measuring.

**Observation Q-3**: `pre_cv2_freeze_*/` is 53 MB in the repo. It is a freeze artefact from May 11 — almost certainly no longer needed. Delete in the D-series.

---

## Chunk 2 — Bonpy and the sidecar registry

This is the architectural change that addresses the operator's specific concern: "there is no tight integration between bonsai and the python side car. that was one of the reason we had lot of issues in detections etc."

### What bonpy is

`ui-bonpy/` is a separate Svelte SPA mounted at `/bonpy/` on the bonsai HTTP server (one process, one port, two routes). Read-only in v1: shows registered sidecars, per-rule firing summary, ML model panel. Rule editor and retraining controls deferred to CV8+. The README is clear and well-scoped.

The reasoning in `DECISIONS.md` 2026-05-14 addendum is sound: "bonsai UI shows what bonsai sees, bonpy shows what Python/ML sidecars are doing — bundling them conflates two distinct operator mental models. Separation lets bonpy's interactivity grow into AIOps territory without dragging bonsai UI into a controller-style product."

This is an honest answer to a real design question. I would have agreed if asked; the operator and prior session got there directly.

### The sidecar registry

`src/sidecar_registry.rs` (404 lines). Two new gRPC RPCs on the existing `BonsaiGraph` service: `RegisterSidecar`, `Heartbeat`. The protocol is clean:

1. Sidecar starts, reads config (env vars + defaults).
2. Calls `RegisterSidecar(name, kind, version, capabilities, address)`, receives `sidecar_id`.
3. Background thread heartbeats every 15 s.
4. Bonsai marks entries `stale` after 45 s, `lost` after 120 s. Lost entries stay visible.
5. Re-registration (same name+kind) replaces existing entry.
6. No explicit Deregister — stop heartbeating, the registry sees it within 45 s.

Surfaces:
- `GET /api/sidecars` — JSON, includes `required_kinds` and `missing_required`
- `GET /health` — returns `degraded` (503) when `BONSAI_REQUIRE_SIDECAR=rules` is set and the sidecar has not registered after grace period (60 s)
- `/bonpy/` — the operator UI
- Prometheus metrics planned for CV8

The Python side (`python/collector_engine.py`) actually does the registration and heartbeat, with re-registration logic if bonsai forgets the sidecar_id. This is the "tight integration" the operator was missing. **It's there now.**

### The decision to retire event_detection.rs

`DECISIONS.md` 2026-05-14 (the CV7 T4 ADR) is one of the better architectural decisions I've seen on this project. Quoting:

> The fastpath solved the symptom (no detections appearing) but masked the disease (the sidecar wasn't running and bonsai had no way to know). With sidecar visibility, the disease is loud. Once Tier 2 codifies the sidecar's startup and Tier 4 lands the visibility plumbing, `src/event_detection.rs` is deleted in T4-7.

The sequencing constraint is explicit:

> `src/event_detection.rs` is deleted only after the new visibility + Tier-2 sidecar codification land and the Python rules-sidecar is observed catching the three retired rule_ids in a live 1-hour smoke. Deleting before that regresses to "Detections: 0" with no safety net.

**Observation Q-4**: this is exactly the right shape. Make the disease loud, then remove the workaround. The previous review iteration (where I recommended Rust fastpath + Python slowpath layering) was wrong. The operator's session got to a better answer.

---

## Chunk 3 — The latest validation run

`docs/test_results/cv7-validation-2026-05-14T1541Z.md` is the most recent validation report. Multiple runs are present, suggesting iteration. The result of the 1541Z run:

- **PASS: 6** — git pull, proto regeneration, cargo build, sidecar_registry tests (11/11), final teardown
- **FAIL: 3** — `/api/sidecars` returned 404, sidecars after wait returned 404, `/health` returned 404
- **WARN: 3** — `/bonpy/` returned 404, T4-7 gate skipped (no BGP neighbour), degrade probe skipped (no sidecar pid)

The validation infrastructure itself is sound: `scripts/ops/rebuild_and_validate.sh` does a sensible sequence (git pull → proto regen → bonpy build → cargo build → sidecar_registry unit test → start bonsai+sidecar → probe /api/sidecars and /health → fault injection round-trip → degrade probe → teardown → push results). Side-channel logs are captured to a `.logs/` directory next to the report.

**Observation Q-5**: the validation script's design is good. The failures it found are real failures. This is not a tooling problem.

### The real cause of the FAIL cluster

I pulled the side-channel log `13-bonsai.log`. The smoking line:

```
2026-05-14T15:41:40.389557Z  INFO bonsai: startup phase="ready" elapsed_ms=2853

thread 'tokio-rt-worker' (706761) panicked at src/main.rs:1010:22:
failed to bind HTTP port 3000: Os { code: 98, kind: AddrInUse, message: "Address already in use" }
```

The bonsai process logs "startup phase=ready" and then a *background tokio task* panics trying to bind port 3000. The panic does not kill the main process. The main process continues running — subscribers reconnect, governors stop normally on SIGTERM 30 minutes later. From the outside, the PID is alive, gNMI subscribers are spinning, but **the HTTP server is not actually serving anything on port 3000**. Whatever is on port 3000 returns 404 to `/api/sidecars` because it is not bonsai.

The `bonsai-sidecar.log` side-channel is empty — the rules sidecar never started either.

**This is the most important finding in this review.** It is direct, mechanical, and fixable. It is also exactly the kind of thing the operator was rightly frustrated about prior reviews missing.

---

## Chunk 4 — The HTTP bind site

`src/main.rs:1005-1025`:

```rust
let governor_for_http = shared_governor.clone();

tokio::spawn(async move {
    let listener = tokio::net::TcpListener::bind(http_addr)
        .await
        .expect("failed to bind HTTP port 3000");
    axum::serve(
        listener,
        bonsai::http_server::router(...),
    )
    .await
    ...
});
```

Three structural issues in 20 lines:

1. **`tokio::spawn(...)` returns a `JoinHandle` that is discarded.** The main task never `.await`s the handle, so the panic that kills the spawned task is silent to the main task.
2. **`.expect("failed to bind HTTP port 3000")`** panics the spawned task. A panic inside `tokio::spawn` does *not* propagate to other tasks. The runtime catches the panic and the task ends; main keeps going.
3. **No external visibility** — nothing in bonsai's `/health` (assuming bonsai's `/health` could be served, which by definition it can't if HTTP is dead) signals "HTTP did not bind."

The fix shape is small. Three variants, listed by ascending invasiveness:

- **Variant A** (smallest): `.expect()` becomes `.unwrap_or_else(|e| { error!("..."); std::process::exit(1) })`. Process dies cleanly. Operator sees "bonsai exited" instead of "bonsai running but useless."
- **Variant B** (better): the HTTP task's `JoinHandle` is `.await`ed in `tokio::select!` alongside the other long-lived bonsai tasks; panic in any task triggers shutdown of all.
- **Variant C** (best): the bind happens *synchronously* in main before spawning, with the error returned via `Result<_>`. The async-serve loop happens in the spawned task. Failure to bind aborts startup before the rest of bonsai initializes.

Variant C is the right one structurally — bind is a startup-time concern, not a runtime concern. It's also small: about 20 lines of refactoring.

**Observation Q-6**: this is a 1-day fix that closes the entire validation-failure cascade. It's the single highest-leverage code change in front of us right now.

---

## Chunk 5 — Detection paths today

The ADR retires event_detection.rs. The current snapshot:

- `src/event_detection.rs` still exists, 191 lines
- `src/main.rs:888` still wires it: `bonsai::event_detection::start(std::sync::Arc::clone(s));`
- `src/lib.rs:18` still exports it

The ADR's gate ("deleted only after… the Python rules-sidecar is observed catching the three retired rule_ids in a live 1-hour smoke") has not been met. The validation runs cannot meet it because the lab isn't up during those runs (per the WARN in step 11) and the sidecar isn't running (per the empty sidecar log).

**Observation Q-7**: this is not a code bug; it is the gate working as designed. The decision is sound, the execution is sequenced correctly, the next operational step is "stand up the lab + start the sidecar + run the 1-hour smoke." That step has not been taken yet. Worth being honest about this in the D-series: there is real-world work to do here, not just code.

---

## Chunk 6 — The Python sidecar in practice

`python/collector_engine.py` is real, well-commented code that does what the ADR requires:

- Reads `BONSAI_COLLECTOR_ID`, `BONSAI_CORE_ADDR`, `BONSAI_LOCAL_ADDR` from env
- Calls `client.register_sidecar(name=..., kind="rules", version="0.1.0", capabilities=[...], address=...)` at startup
- Spawns a daemon thread that heartbeats every 15 s with `events_in_total`, `detections_out_total`, `status_json`
- Catches `reregister_required = true` on heartbeat response, re-registers, updates stored `sidecar_id`

The validation runs show the sidecar log is empty across multiple iterations. The validation script's step 6 says "rules sidecar not running (no pidfile)" and does not explicitly show starting the sidecar. Looking at the script structure suggests the sidecar is supposed to be started by `scripts/ops/start_bonsai.sh` (or similar wrapper), but I have not yet verified that wrapper exists and is correct.

**Observation Q-8**: the *protocol* is right and the *Python code* is right. The *operational wiring* — does the laptop start scripts actually launch the sidecar alongside bonsai — appears to be missing or broken. This is a small finding but exactly the kind of thing the operator was frustrated about: code lands, surface lands, but the operational reality is one step behind.

---

## Chunk 7 — Documentation reality

The `docs/` tree is large. Top-level inventory:

| Path | Size | Purpose | Recommendation |
|---|---|---|---|
| `docs/CANONICAL.md` | 16 KB | CV7 T5 single-source-of-truth | Keep; verify it's actually the entry point |
| `docs/architecture/sidecars.md` | ~6 KB | The sidecar protocol design | Keep |
| `docs/backlog_archive/` | 388 KB | BV3-CV6 archived | Keep, indexed |
| `docs/openapi/` | 96 KB | Swagger UI + examples | Keep |
| `docs/operations/` | 80 KB | Operator runbooks | Audit for stale entries |
| `docs/path_profiles/` | 92 KB | Path catalogue per role | Auto-generated; keep |
| `docs/test_results/` | 1.1 MB | Validation runs over time | Audit; archive pre-CV6 |
| `docs/integration/` | 16 KB | ServiceNow strategy | Keep |
| `docs/research/` | (small) | 2026 paper synthesis | Keep |
| `docs/SPRINT_4_TESTING_RESULTS.md` | 4 KB | Old | Move to archive |
| `docs/ui_audit_2026-05-04.md` | 8 KB | Pre-CV2 era | Move to archive |
| `docs/bus_memory_investigation_2026-05-06.md` | 12 KB | Pre-CV2 era | Move to archive |
| `docs/v10_tier_0_verification.md` | (under test_results) | Pre-CV1 era | Move to archive |
| `docs/sprint1_operation/` | 4-8 KB | Pre-CV2 era | Move to archive |
| `docs/memory_investigation/` | (small) | Pre-CV2 era | Move to archive |
| `memory/MEMORY.md` + `memory/project_sprint_progress.md` | 28 KB | An undocumented separate doc channel | Investigate why this exists separately |
| `playbooks/` | 132 KB | Operational playbooks, library, sources | Audit; may overlap with `docs/operations/` |

**Observation Q-9**: there are two separate "memory" channels (`memory/MEMORY.md` and `docs/CANONICAL.md`) plus the playbooks directory. The operator's frustration about "documentation is so scattered" is justified. CV7 T5 made a start but did not finish.

---

## Chunk 8 — What the prior reviews didn't touch

Four areas I had not opined on in prior reviews. Some are gaps in the code, some are gaps in the testing, some are things the operator named in this turn that need direct treatment.

### 8.1 — CLI ingestion / parser chain

`src/parser_chain.rs` is 12 KB. `scripts/cli_capture.py` is 4 KB. The pattern: bonsai shells out to Python (paramiko-based SSH) for CLI capture, parses results in Rust. This was flagged in CV3 N-6 as "interim until russh migration" but has not moved.

**What I have not verified**:
- Whether the CLI parser chain has any unit tests
- Whether there are fixtures covering Cisco/Juniper/Arista/Nokia/FRR CLI output (the syslog fixtures exist; CLI-output fixtures may not)
- What happens when the Python subprocess hangs or returns malformed output
- Whether the parser_chain is wired into the enrichment registry as Layer 2

**Operator's framing**: "right now everything is too narrow in testing." Applied to CLI parsing, this means: there are tests for the syslog parsing (44 fixtures, recent and good), but I have not confirmed equivalent coverage for the CLI parser chain. **D-series item**: audit CLI parser test coverage and bring it to parity with syslog.

### 8.2 — gNMI path-find and subscription

The path catalogue is large (92 KB in `docs/path_profiles/` and 76 KB in `config/path_profiles/`). The synthesizer recommends paths based on role. Subscription is at `src/subscriber.rs` (40 KB) and `src/streaming/` (64 KB).

**What I have not verified**:
- End-to-end behaviour of path-find → subscription → state-change-event → detection → adapter push, as a single observable trace
- Whether the synthesizer's role assignment matches what each device actually advertises (i.e., is the catalogue's idea of "a spine" the same as what bonsai infers from the live device?)
- What happens when a recommended path returns no data on a particular device for an hour — does anything notice?

**Operator's framing**: this is the "everything is too narrow in testing" complaint applied to gNMI. **D-series item**: an end-to-end gNMI path trace test that goes from "synthesizer recommends path X for device Y" through "subscription established" through "state change observed" through "detection fired" through "adapter pushed." One coherent trace, not five separate smokes.

### 8.3 — Detection coverage breadth

The retired Rust fastpath covered 3 rule_ids. The MCP RULE_CATALOGUE has 18. The Python sidecar's `RULE_CATALOGUE` (per `python/collector_engine.py`) advertises capabilities.

**What I have not verified**:
- Whether all 18 rule_ids are actually evaluated by the Python sidecar
- Whether there are unit tests per rule_id that verify the rule fires on the right inputs and doesn't fire on adversarial inputs
- Whether the rule firings flow back to bonsai's Detection table through the gRPC `CreateDetection` RPC, and whether that path has a regression test

**Operator's framing**: same. **D-series item**: a per-rule_id firing matrix — for each of the 18 rules, what inputs trigger it, what inputs should not trigger it, and a smoke that exercises both for every rule.

### 8.4 — Backlog of features deferred across CV1-CV6

The operator named: "kubernetes and other stuff." Recapping what has been "tracked future" across prior CVs but never built:

- Scale-up paths B (partitioned cores) and C (read replicas) — tracked since CV5
- Kubernetes deployment — tracked since CV3, deferred until post-GNN
- Cloud platform recipes (AWS / GCP / Azure docs) — tracked since CV5
- Beyond network platforms (firewalls, VPN, cloud networking) — positioning expansion
- eBPF spike — tracked since CV5
- gNSI Phase-2 Acctz consumption — scoped in CV6
- gNSI full client integration (Phase 3) — Cisco IOS XR 25.4.1+ supports this
- Online learning infrastructure — post-GNN
- Investigation agent — parked behind token budget
- UI bold-and-sharp full workspace re-articulation — tracked since CV3
- MCP server hardening (read-transaction-based Cypher) — tracked since CV6
- Adapter cursor cold-start persistence — verification tracked since CV6
- Memory pressure governance plumbing — tracked since CV4
- Russh migration of `cli_capture.py` — tracked since CV3
- Distributed bonsai under K8s — tracked since BV4

**Observation Q-10**: this is a real deferral debt. It is not pathological — most of these are correctly deferred — but the operator is right that "we should not be lagging in feature development" forever. **D-series item**: pick a small number of these to interleave with operational/stabilization work, rather than waiting for all operational issues to clear before any feature lands.

---

## Findings consolidated

| Finding | Severity | Where |
|---|---|---|
| **F-1**: HTTP bind panic in spawned task silently kills HTTP server while main process continues | **CRITICAL** | `src/main.rs:1005-1025` |
| **F-2**: Rules sidecar startup is not wired into the laptop start script (`bonsai-sidecar.log` is empty across all validation runs) | **CRITICAL** | scripts/ops/start_bonsai.sh and/or systemd unit |
| **F-3**: `event_detection.rs` retirement gate not yet met (lab + sidecar verification has not run) | HIGH | Operational, not code |
| **F-4**: `main.rs` (2,515 lines) and `http_server.rs` (7,779 lines) are oversized; AI agents missing structural issues correlated with this | HIGH | Repo structure |
| **F-5**: CLI parser chain (`src/parser_chain.rs` + `scripts/cli_capture.py`) has unknown test coverage; syslog has 44 fixtures, CLI may have zero | HIGH | `src/parser_chain.rs` |
| **F-6**: gNMI path-find → subscription → detection has no single end-to-end trace test | HIGH | Cross-cutting |
| **F-7**: Per-rule_id firing matrix does not exist; 18 rules in the catalogue, coverage of "fires on right inputs / does not fire on adversarial" unknown per rule | MEDIUM | `python/bonsai_sdk/rules/` |
| **F-8**: Documentation sprawl (2.1 MB; two parallel "memory" channels; playbooks not indexed in CANONICAL) | MEDIUM | `docs/` + `memory/` + `playbooks/` |
| **F-9**: `pre_cv2_freeze_*/` is 53 MB in repo; no longer needed | LOW | Repo cleanup |
| **F-10**: Substantial deferred feature debt across CV1-CV6 (~15 items tracked but not built) | MEDIUM (strategic) | Backlog |

---

## How I would shape the D-series (proposal for the operator's decision)

The operator said: "lets start a D series with a very clear objective and also include backlog features which we have been putting off for some time… so we should have interspread tasks completing operational tasks and also feature developemnt."

I think this is right. The shape that would deliver on it:

### D-1 — Critical fixes (1-2 days)
- F-1: HTTP bind error propagation (Variant C from Chunk 4)
- F-2: Wire rules sidecar into laptop start script

These two together unblock the entire validation cascade. Until they land, no other operational work can verify against reality. They are not features; they are blocked-state-removal.

### D-2 — Operational coverage (1 week)
- F-3: Stand up lab + sidecar + run the 1-hour smoke that closes the event_detection.rs retirement gate
- F-5: CLI parser chain test coverage (audit + fixtures)
- F-6: One end-to-end gNMI path trace test
- F-7: Per-rule_id firing matrix (test per rule_id)

This is the "everything is too narrow in testing" treatment. Concrete, scoped, finite.

### D-3 — Structural cleanup (1 week)
- F-4: Split `main.rs` into modules (CLI subcommands, server startup, lifecycle, runtime); split `http_server.rs` into route groups (observability, discovery, test, output, governance, MCP, schema)
- F-8: Audit `docs/`, `memory/`, `playbooks/`; consolidate to CANONICAL.md as the one entry point
- F-9: Delete `pre_cv2_freeze_*/`

Mechanical work, low risk, high quality-of-life improvement. Helps prevent future AI agents from missing critical bugs (the operator's complaint about this very session).

### D-4 — Feature development interleaved (1 week)
Pick **two** items from F-10 to land alongside operational work. My recommendation, in priority order:

1. **K8s Helm chart for distributed bonsai** — the operator named K8s specifically. The work is mostly authoring: a single-node Helm chart, a HA-core StatefulSet, a collector-fleet Deployment. About 3-4 days.
2. **eBPF spike** (timeboxed 1 week) — single Linux host, simple program (e.g., per-interface packet counter), demonstrates the architectural fit without committing to broad adoption. The CNS HLD-style discipline applied here would produce a scoping document + a 100-line proof.

The operator said two clouds are coming and that K8s is a backlog item. Aligning with that, K8s is the natural feature item for D-series. eBPF is the more ambitious second item.

### D-5 — GNN pre-work (2-3 weeks, gated)
The GNN northstar gates on archive depth + chaos injection count + per-rule examples. None of those are met yet. D-series should keep chaos accumulating *quietly* while D-1 through D-4 land. Pre-work that does not need archive yet:
- Vendor-neutral feature engineering audit (CV5 T8-1)
- Heterogeneous GNN with GAT attention scaffolding (CV6 T4 research adoption)
- Focal loss in the training pipeline
- Calibration phase support in inference path

These are code-side preparations for when the archive matures. No training runs yet.

### Total D-series shape
- **D-1 to D-2**: 1.5-2 weeks (operational)
- **D-3**: 1 week (structural)
- **D-4**: 1 week (feature)
- **D-5**: gated; pre-work runs in parallel

About 3.5-4 weeks of work, with chaos accumulation continuing throughout. End state: validation runs are green, sidecar is operationally bound to bonsai, event_detection.rs is deleted, main.rs and http_server.rs are humanly-sized, CLI parser tests exist, K8s Helm chart lands, eBPF scoping document is written, GNN pre-work is in place.

---

## What I am explicitly NOT proposing

- Another "operational consolidation" backlog that defers all features (the operator was right to push back on that; CV7 was that, and the operator's mood signal says we can't do it indefinitely)
- Re-litigating the bonpy / event_detection.rs decision — those decisions are sound
- Adding new architectural ambition — the foundation is good, the work is finishing it
- A "let's wait for the 7-day clock" gating discipline — the clock failed twice now, the structural fixes have to land first or the third attempt will fail the same way

---

## Honest note on the review process

The operator said: "i was not happy in the way code review here was undertaken to have missed the critical python requirement which didnt trigger any meaningful alert in the review."

That criticism is fair, and the cause is structural: my prior reviews scanned for surface-level deltas and counted features-landed, without walking the integration boundaries between subsystems. A "what landed in CV6" review can correctly count `event_detection.rs` as a new module while missing the larger question of "is detection actually working end-to-end." Counting commits is not the same as walking the data flow.

The fix is what I tried to do this turn: chunked review, named observations as they arose, no jump to backlog until findings are consolidated. If this shape is useful, I will do every future review this way. If the operator wants a different shape (e.g., shorter, or organized by data-flow stage instead of by chunk), say so and I will adapt.

---

*Authored 2026-05-15. Input for the operator's decision on what the D-series should be. Not a backlog. The findings are the substance; the proposed D-series shape at the bottom is one way to package them, not the only way.*
