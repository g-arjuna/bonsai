# BONSAI — Backlog Delta Series, v1 (DV1.0)

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_CV7.md`. Authored 2026-05-15 after the chunked code review (`CHUNKED_CODE_REVIEW_2026-05-15.md`) confirmed three critical findings the previous reviews missed.
>
> **Why D-series, not CV8**: the operator asked for a fresh designation to mark a different working rhythm. The C-series was operational consolidation; the D-series interleaves operational fixes with feature development as the operator explicitly requested ("we should have interspread tasks completing operational tasks and also feature development… we should not be lagging in feature development"). The naming makes the rhythm shift visible.
>
> **What DV1 is**: five tiers, each with a finite task list and a done-when criterion. Two of the tiers (D-1 critical fixes, D-2 breadth-of-testing) clear operational debt that prior sprints repeatedly failed to close. One tier (D-3) does the structural cleanup that prevents future AI code reviews from missing critical bugs. One tier (D-4) lands two backlog features (K8s Helm chart, eBPF spike) so feature development is no longer indefinitely deferred. The fifth tier (D-5) does GNN pre-work that does not need archive depth yet — it runs in parallel with the operational chaos cycle continuing in the background.
>
> **What DV1 is not**: an architectural rewrite. The architectural decisions made in CV7 (sidecar registration protocol, bonpy UI separation, event_detection.rs retirement gate) are sound. DV1 finishes their execution rather than re-litigating them.
>
> **The promise DV1 keeps**: by end of sprint, `bash scripts/ops/rebuild_and_validate.sh` returns PASS=14 FAIL=0 (currently PASS=6 FAIL=3 WARN=3). The Python rules sidecar is running and registered. `event_detection.rs` is deleted. The CLI parser chain has tests. There is one end-to-end gNMI path-find → detection trace test. `main.rs` and `http_server.rs` are sub-1000 lines each. A K8s Helm chart is committed. A 1-week eBPF spike has produced a scoping document and a 100-line proof. GNN pre-work scaffolding is in the codebase ready for when archive depth meets the trigger.

---

## Table of Contents

1. [The Findings This Backlog Addresses](#findings)
2. [What Has Changed Mentally Since CV7](#mental-shift)
3. [TIER D-1 — Critical Fixes](#tier-d1) ⚡ START HERE ⚡
4. [TIER D-2 — Operational Coverage](#tier-d2)
5. [TIER D-3 — Structural Cleanup](#tier-d3)
6. [TIER D-4 — Interleaved Feature Development](#tier-d4)
7. [TIER D-5 — GNN Pre-Work](#tier-d5)
8. [Tracked Future Threads](#tracked)
9. [Where We Are. Where We Intend to Be.](#motivation)
10. [Execution Order](#execution-order)
11. [Guardrails — Updated](#guardrails)

---

## <a id="findings"></a>The Findings This Backlog Addresses

From the chunked review document. Severity inherited verbatim.

| Finding | Severity | Addressed by |
|---|---|---|
| **F-1**: HTTP bind panic in spawned task silently kills HTTP server while main process continues | **CRITICAL** | D-1 T1 |
| **F-2**: Rules sidecar not running in validation runs — but root cause is F-1 (wrapper waits for /health, never reaches sidecar start) | **CRITICAL** | D-1 T2 (verification) |
| **F-3**: `event_detection.rs` retirement gate not yet met | HIGH | D-2 T1 |
| **F-4**: `main.rs` (2,515 lines) and `http_server.rs` (7,779 lines) are oversized | HIGH | D-3 T1 + T2 |
| **F-5**: CLI parser chain has unknown test coverage; syslog has 44 fixtures, CLI may have zero | HIGH | D-2 T2 |
| **F-6**: gNMI path-find → subscription → detection has no single end-to-end trace test | HIGH | D-2 T3 |
| **F-7**: Per-rule_id firing matrix does not exist for the 18 catalogued rules | MEDIUM | D-2 T4 |
| **F-8**: Documentation sprawl (2.1 MB; two parallel "memory" channels; playbooks not indexed) | MEDIUM | D-3 T3 |
| **F-9**: `pre_cv2_freeze_*/` is 53 MB in repo; no longer needed | LOW | D-3 T4 |
| **F-10**: Substantial deferred feature debt across CV1-CV6 (~15 items tracked but not built) | MEDIUM (strategic) | D-4 (selective) |

---

## <a id="mental-shift"></a>What Has Changed Mentally Since CV7

CV7 framed itself as "no new features, stabilization only." That was the right disposition for the moment, but the operator's mood signal during this transition ("we should not be lagging in feature development") pushes back against indefinite feature freeze. The D-series picks up the interleaved rhythm:

- **Each sprint has at least one feature item.** Not as a token gesture — as a real piece of work that lands a backlog item from F-10.
- **Operational fixes are sized small, not large.** D-1 is two days. D-2 is one week. D-3 is one week. Mechanical scope.
- **Reviewer discipline is encoded in the process.** The chunked review pattern that found F-1 is documented in `docs/review_discipline.md` (a new artefact from D-3 T5) so future AI sessions don't repeat the same surface-level review failure.
- **GNN pre-work runs in parallel.** Code-side preparations for the training sprint don't need archive depth. They land throughout DV1 and are ready when the trigger fires (probably DV2 or DV3).

---

## <a id="tier-d1"></a>TIER D-1 — Critical Fixes ⚡ START HERE ⚡

**Total estimate: 1-2 days.** Unblocks the entire validation cascade. Without D-1, no other D-tier work can verify against reality.

### D1-T1 (DV1) — HTTP bind error propagation

**The bug**: `src/main.rs:1005-1025` spawns the HTTP listener inside `tokio::spawn(async move { let listener = ... .expect("..."); ... });`. The `.expect()` panics the spawned task. The panic does **not** propagate to the main task because the `JoinHandle` is discarded. Bonsai's main process continues running — subscribers, governors, archive — while the HTTP server is dead. From the outside the process looks healthy (PID alive, gNMI flowing, logs writing) but `/health`, `/api/sidecars`, `/api/topology` all return 404 because port 3000 is owned by some other process (typically a previous orphaned bonsai or a docker container that didn't get torn down).

**Fix shape (Variant C from the chunked review)**:

```rust
// in main(), BEFORE spawning the long-lived axum task:
let listener = tokio::net::TcpListener::bind(http_addr)
    .await
    .with_context(|| format!("failed to bind HTTP port at {http_addr}"))?;
info!(addr = %http_addr, "HTTP listener bound");

// Then spawn axum::serve(listener, router).await in the background.
// The bind happened on the main task; a bind failure aborts startup.
```

Three guarantees this gives us:
1. Bind failure aborts bonsai startup with a clear error, not a silent panic.
2. The HTTP listener exists by the time main proceeds to start subscribers — no race between subscribers connecting and HTTP being reachable.
3. The error is propagated through `Result<()>`, which means the systemd unit + the laptop wrapper script both see a non-zero exit and don't silently start the sidecar against a half-dead bonsai.

**Additional hardening**: the spawned axum task's `JoinHandle` should be tracked. If `axum::serve(...).await` returns (which it does when the listener closes), main should treat that as a shutdown signal and exit. Use `tokio::select!` over the HTTP task handle alongside the existing shutdown signals.

**Where**: `src/main.rs:1005-1025` and surrounding spawn block. About 30 lines of refactor.

**Effort**: 0.5 day code + 0.5 day testing (verify validation script step 7 passes and the orphan-port-3000 case fails cleanly).

**Done when**:
- `cargo build --release` succeeds (no semantic change to behaviour).
- Running bonsai with port 3000 already bound exits non-zero with the error message printed.
- Running `bash scripts/ops/rebuild_and_validate.sh --skip-build` produces PASS at step 7 (bonsai HTTP listener up).

### D1-T2 (DV1) — Verify sidecar startup after F-1 fix

The chunked review initially framed F-2 as "rules sidecar not wired into the laptop start script." On closer inspection (`scripts/ops/start_bonsai_with_sidecar.sh:154` does start the sidecar via `python python/collector_engine.py >> $SIDECAR_LOG 2>&1 &`), the root cause is downstream of F-1: the wrapper waits 60 s for bonsai's `/health` to respond before starting the sidecar. When HTTP panics silently, `/health` never responds, the wrapper exits with error, the sidecar is never launched, and `bonsai-sidecar.log` stays empty.

**What this task does**: after D1-T1 lands, re-run the validation script and verify:
1. `/health` returns 503 (degraded — sidecar not yet registered) within 10s of bonsai start.
2. Wrapper proceeds to start the sidecar.
3. Sidecar registers within 15s.
4. `/health` flips to 200 (ok) after registration.
5. `/api/sidecars` returns the registered sidecar with kind=`rules`.

**If any of those fails**: the failure mode is now operationally loud, not silent. The validation report tells the operator exactly which step broke and the side-channel logs have the evidence.

**Where**: no code change in DV1. Verification only.

**Effort**: 30 minutes assuming D1-T1 is correct.

**Done when**: a validation run produces PASS at steps 7, 8, 9 (HTTP up, sidecars registered, sidecars healthy).

---

## <a id="tier-d2"></a>TIER D-2 — Operational Coverage

**Total estimate: 1 week.** This is the "everything is too narrow in testing" treatment from the operator's framing. Four discrete tasks, each scoped to a specific narrowness in current test coverage.

### D2-T1 (DV1) — Close the event_detection.rs retirement gate

The CV7 T4 ADR specifies the gate explicitly: "deleted only after… the Python rules-sidecar is observed catching the three retired rule_ids in a live 1-hour smoke."

**What this requires**:
1. Lab is up (`bash scripts/lab/redeploy_dc.sh --topo-only` — 8 SR Linux nodes, BGP EVPN converged).
2. Bonsai is running with `BONSAI_REQUIRE_SIDECAR=rules`.
3. Rules sidecar is registered and heartbeating.
4. Fault injection runs for 1 hour against the lab, injecting BGP session down, BFD session down, interface oper-status changes.
5. `/api/detections` shows detection rows with `rule_id` in {`bgp_session_down`, `bfd_session_down`, `interface_down`}.
6. The detections originated from the Python sidecar (visible via the sidecar's heartbeat `detections_out_total` counter incrementing).

**Once gate closes**: delete `src/event_detection.rs` (191 lines), remove `pub mod event_detection` from `src/lib.rs:18`, remove `event_detection::start(...)` call from `src/main.rs:888`. Unit tests for event_detection move to archive or delete.

**Where**: operational test + code deletion.

**Effort**: 1.5 hours for the smoke (1 hour smoke + setup/teardown + verification) + 30 minutes for the deletion.

**Done when**:
- Smoke report at `docs/test_results/event_detection_retirement_gate_<date>.md` documents the 1-hour run and the rule_id firings observed.
- `src/event_detection.rs` does not exist.
- `cargo build --release` succeeds.
- Re-run of validation script still PASSes the detection-related steps.

### D2-T2 (DV1) — CLI parser chain test coverage

**The gap**: `src/parser_chain.rs` (12 KB) drives CLI capture against Cisco/Juniper/Arista/Nokia/FRR via `scripts/cli_capture.py` (Python paramiko shell-out). The syslog parser has 44 fixtures (`tests/syslog_fixtures/`). The CLI parser may have zero. The chunked review did not confirm this; D2-T2 first audits, then closes the gap.

**Two steps**:

**Step 2a — Audit (0.5 day)**: enumerate every `parser_chain.rs` parser function. For each, find any existing test (search `tests/`, `src/parser_chain.rs#tests`, `tests/parser_chain*`). Produce `docs/testing/cli_parser_audit.md` with one row per parser: name, vendor, what it parses (e.g., `show ip bgp summary`), whether unit tests exist, whether integration fixtures exist.

**Step 2b — Fixture authoring (3 days)**: where coverage is missing, author CLI-output fixtures following the same pattern as `tests/syslog_fixtures/`:

```yaml
# tests/cli_fixtures/cisco-iosxr-show-bgp-summary.yaml
fixture_id: cisco-iosxr-show-bgp-summary-converged
vendor: cisco-iosxr
command: "show bgp summary"
raw: |
  BGP router identifier 10.0.0.1, local AS number 65001
  Neighbor        Spk    AS MsgRcvd MsgSent   TblVer  InQ OutQ Up/Down  St/PfxRcd
  10.0.0.2          0  65002    1234    1230      450    0    0 00:23:45        42
expected_parsed:
  router_id: "10.0.0.1"
  local_as: 65001
  neighbors:
    - peer: "10.0.0.2"
      remote_as: 65002
      msg_rcvd: 1234
      uptime: "00:23:45"
      prefixes_received: 42
```

**Initial coverage target** (8 fixtures per vendor × 5 vendors = 40 fixtures):
- BGP summary (converged + transient)
- Interface status
- IS-IS adjacencies
- OSPF neighbors
- BFD sessions
- Route table snippet
- LDP neighbors (where supported)
- LLDP neighbors

**Step 2c — Smoke (0.5 day)**: `scripts/smoke/smoke_cli_fixtures.sh` runs each fixture through the parser, validates the extracted fields match expected_parsed. Pattern mirrors `smoke_syslog_fixtures.sh`.

**Where**: `tests/cli_fixtures/`, `docs/testing/cli_parser_audit.md`, `scripts/smoke/smoke_cli_fixtures.sh`.

**Effort**: 4 days total (0.5 audit + 3 fixtures + 0.5 smoke).

**Done when**:
- CLI parser audit doc exists with one row per parser.
- 40 CLI fixtures exist.
- `smoke_cli_fixtures.sh` runs and reports per-fixture pass/fail.
- Feature index has a new row "CLI parsing coverage" with per-vendor fixture counts.

### D2-T3 (DV1) — End-to-end gNMI path-find → detection trace test

**The gap**: bonsai has individual smokes for path discovery, subscription, archive, detection, output. There is no single test that follows one event from path-find through subscription through state-change through detection through adapter push, as one coherent trace.

**What this test does**:
1. Start fresh bonsai + sidecar against the lab.
2. Synthesizer recommends paths for `srl-leaf1` (a known device with a known role).
3. Apply the recommended paths.
4. Inject `bgp neighbor disable` on `srl-leaf1` for one peer.
5. Verify within 30s: (a) gNMI subscription observed the state transition; (b) graph store wrote a state-change-event row; (c) Python sidecar wrote a detection row with `rule_id=bgp_session_down`; (d) output adapter (if configured) pushed the detection.

The trace is one timestamp series across five subsystems. The test passes if and only if every stage occurred and the timing is plausible (sub-30-second end-to-end).

**Why this is the right test**: it's the only test that catches integration regressions between *the components the operator named as a list* (path-find, subscription, detection, output). Per-subsystem smokes can each pass while the integration fails — that's exactly what produced the "Detections: 0" symptom in CV6.

**Where**: `tests/e2e/path_to_detection_trace.rs` (Rust integration test) or `scripts/e2e_path_to_detection_test.sh` (shell-based, easier to debug).

**Effort**: 2 days.

**Done when**:
- The test is runnable against a live lab + bonsai + sidecar.
- A single pass produces a trace artefact listing the timestamps at each stage.
- Adding to feature index as "E2E path-to-detection trace" with current pass/fail status.

### D2-T4 (DV1) — Per-rule_id firing matrix

**The gap**: the MCP `RULE_CATALOGUE` has 18 rule_ids. The Python sidecar's `RULE_CATALOGUE` advertises capabilities. No structured "for each rule_id, what inputs trigger it / what inputs should not trigger it" exists.

**What this produces**: `tests/rule_firing_matrix.yaml`:

```yaml
- rule_id: bgp_session_down
  fires_on:
    - input: "BgpNeighbor state transitions Established → Active"
      fixture: bgp_session_estab_to_active.yaml
    - input: "BgpNeighbor state transitions Established → Idle (admin reset)"
      fixture: bgp_session_admin_reset.yaml
  does_not_fire_on:
    - input: "BgpNeighbor first observed as Idle (never was Established)"
      fixture: bgp_initial_idle.yaml
    - input: "BgpNeighbor flapped within 5s — debounced"
      fixture: bgp_flap_debounce.yaml
- rule_id: bfd_session_down
  fires_on: [...]
  does_not_fire_on: [...]
# 16 more rules
```

The matrix is the spec. `scripts/smoke/smoke_rule_firing_matrix.sh` reads the matrix, injects each fixture's input, verifies the rule fires (or doesn't) accordingly.

**Effort**: 3 days. Matrix authoring is the bulk; the smoke script is mechanical.

**Done when**:
- Matrix YAML has all 18 rule_ids.
- Each rule has at least 1 `fires_on` and 1 `does_not_fire_on` entry.
- The smoke runs and reports per-rule pass/fail.

---

## <a id="tier-d3"></a>TIER D-3 — Structural Cleanup

**Total estimate: 1 week.** Mechanical work. Low risk. High quality-of-life improvement for AI agents reading the codebase. Helps prevent future critical-bug misses (F-4 is the structural cause of the operator's frustration with prior reviews).

### D3-T1 (DV1) — Split `main.rs`

**Current state**: 2,515 lines. Mixes:
- The `main()` async function (lines 40-1218): CLI parsing, config loading, subsystem instantiation, server spawn, lifecycle
- TLS helpers (1219-1300)
- Device CLI subcommands (1604-1900)
- Audit CLI subcommands (1900-1940)
- Catalogue CLI subcommands (1940-2095)
- YANG CLI subcommands (2095-2240)
- CLI usage printers (1769-2278)
- Self-test runner (2278-end)

**Target structure**:
```
src/main.rs                  (≤ 300 lines: argument parsing, dispatch to subcommands or run_server)
src/bin/                     (if separate binaries make sense — keep current structure)
src/cli/                     (new module dir)
  mod.rs                     (CLI argument types)
  device.rs                  (run_device_cli + helpers)
  audit.rs                   (run_audit_cli)
  catalogue.rs               (run_catalogue_cli)
  yang.rs                    (run_yang_cli)
  self_test.rs               (run_self_test)
  usage.rs                   (all print_*_usage)
src/server_startup.rs        (new — the body of main()'s server-mode branch: bind, spawn, lifecycle, shutdown)
src/tls_helpers.rs           (TLS config builders)
```

**Effort**: 2 days. Compile-driven refactor — Rust's strictness makes this safe.

**Done when**:
- `src/main.rs` is ≤ 400 lines.
- `cargo build --release` produces a byte-identical binary (or near-identical; allow for symbol-table differences from module path changes).
- `cargo test --release` still passes.
- No public API change.

### D3-T2 (DV1) — Split `http_server.rs`

**Current state**: 7,779 lines. One file with the router, all handlers, schema strings, OpenAPI generation, example payload embedding, helper types.

**Target structure**:
```
src/http_server/
  mod.rs                     (≤ 400 lines: router(), handler imports, state types)
  observability.rs           (topology, devices, detections, incidents handlers)
  discovery.rs               (synthesizer, recommendations, onboarding, gnmi-readiness handlers)
  test_endpoints.rs          (/api/_test/* handlers — inject_detection, syslog/parse, etc.)
  outputs.rs                 (/api/adapters/* handlers)
  governance.rs              (/api/governance/state, /api/sidecars, /health)
  mcp_routes.rs              (/mcp, /api/docs, /api/openapi.json)
  config.rs                  (/api/config/*, /api/yang/* handlers)
  schema.rs                  (OpenAPI generation; the example-embedding boilerplate)
  swagger_ui.rs              (the static-file serving handler)
```

**Effort**: 3 days. Larger than D3-T1; more public functions to relocate.

**Done when**:
- No file in `src/http_server/` exceeds 1,500 lines.
- Router declaration in `mod.rs` is a single function ≤ 200 lines.
- `cargo build --release` succeeds.
- `cargo test --release` still passes.
- Swagger UI still works against the running bonsai.

### D3-T3 (DV1) — Documentation consolidation pass 2

**The gap**: CV7 T5 made `docs/CANONICAL.md` (16 KB). Two further sprawl sources remain:
- `memory/MEMORY.md` + `memory/project_sprint_progress.md` — separate doc channel not indexed in CANONICAL
- `playbooks/` directory (132 KB) — operational playbooks not indexed in CANONICAL

**What this task does**:
1. Audit `memory/` — is anything there not duplicated in CANONICAL? If duplicated, delete `memory/`. If unique, fold into CANONICAL.
2. Audit `playbooks/` — index every playbook in CANONICAL's "where to find things" table. If a playbook is stale, move to `docs/archive/`.
3. Audit `docs/test_results/` — anything older than 30 days that isn't a sprint closure should move to `docs/test_results/archive/`.

**Effort**: 1 day.

**Done when**:
- `memory/` is either deleted or its content is in CANONICAL.
- Every file in `playbooks/` has an entry in CANONICAL's table.
- `docs/test_results/` only contains current-sprint runs plus key historical closures.

### D3-T4 (DV1) — Delete pre_cv2_freeze

**The gap**: `pre_cv2_freeze_20260511T055338Z/` is 53 MB in the repo. It was a one-shot restoration point from CV2; no longer needed. Git history preserves the actual commit.

**Effort**: 5 minutes.

**Done when**: `pre_cv2_freeze_*/` is not in the repo. Git commit message documents that prior contents are at the relevant SHA.

### D3-T5 (DV1) — Author `docs/review_discipline.md`

**The gap**: prior code reviews missed F-1 because they scanned for surface deltas instead of walking integration boundaries. The operator's pushback on this turn is the right kind of pushback. Encode the discipline.

**What this doc captures**:
1. The chunked-review pattern: read in deliberate sections, name observations as they arise, don't jump to a backlog until findings are consolidated.
2. The "walk the data flow" prescription: for each major subsystem, follow a real event from ingestion through detection through output. Counting commits is not reviewing.
3. The "log tail beats summary" prescription: when investigating an operational failure, read side-channel logs before reading the summary verdict.
4. A short checklist for the AI agent reviewing future code drops: did you read main.rs's HTTP bind site? Did you read at least one validation report including the .logs/? Did you grep for `tokio::spawn` without await on JoinHandle?

**Effort**: 0.5 day.

**Where**: `docs/review_discipline.md`. Referenced from CANONICAL.

**Done when**: doc exists. The next AI session reading the repo for review starts at CANONICAL → review_discipline.md → then walks the data flow.

---

## <a id="tier-d4"></a>TIER D-4 — Interleaved Feature Development

**Total estimate: 1 week.** Two items from F-10 (the deferred feature debt). Picked because they address operator-named priorities (Kubernetes named directly) and they don't depend on D-1/D-2/D-3 completing first.

### D4-T1 (DV1) — Kubernetes Helm chart for bonsai

**Scope**: a single Helm chart that supports three deployment shapes:

1. **Single-node mode** — bonsai-core as a single pod, suitable for a small lab or proof-of-concept. One Deployment, one Service for HTTP+gRPC, PersistentVolume for archive + LadybugDB.

2. **HA-core mode** — bonsai-core as a StatefulSet with replication. Requires the existing distributed core/collector protocol. PersistentVolumeClaims per replica.

3. **Collector-fleet mode** — bonsai-core as a single Deployment, collectors as a separate Deployment scaled horizontally. Each collector runs the `collector_engine.py` sidecar.

**What goes in the chart**:
- `values.yaml` with mode selector (single | ha | fleet) and per-mode parameters
- Deployment / StatefulSet templates conditioned on mode
- Service templates for HTTP (3000), gRPC (50051)
- PersistentVolumeClaim templates with configurable storageClass
- ConfigMap templates for bonsai.toml
- Secret templates for credentials (Vault integration is out of scope; secrets are flat values for now)
- Sidecar pod-template for the rules sidecar in fleet mode

**What is explicitly out of scope**:
- Helm Chart Library publication (the chart lives in the repo, not in a public chart museum)
- Cluster-autoscaler integration
- Network policies (the operator should add them per their cluster's standards)
- Cert-manager integration (TLS config follows the existing static-cert pattern)

**Where**: `deploy/helm/bonsai/Chart.yaml` + `templates/` + `values.yaml`.

**Effort**: 3 days.

**Done when**:
- `helm lint deploy/helm/bonsai/` passes.
- `helm template deploy/helm/bonsai/ -f deploy/helm/bonsai/values-single.yaml` produces valid manifests.
- A README at `deploy/helm/bonsai/README.md` documents the three modes and what to set in values.yaml for each.
- The chart is referenced from CANONICAL.

### D4-T2 (DV1) — eBPF spike (timeboxed 1 week, scoping focus)

**Scope**: a focused investigation, not a feature commit. Produces a scoping document and a 100-line proof, not a permanent code surface.

**What the spike does**:

1. **Day 1 — research**: read recent eBPF-for-network-observability literature (Cilium, Pixie, libbpf, aya, the 2026 eBPF tooling landscape). Identify two or three pieces of telemetry that gNMI doesn't reach but eBPF does (interface RX/TX byte counters with sub-second granularity, TCP connection state transitions, kernel drop counters at the netdev layer).

2. **Day 2 — environment**: install `aya` toolchain on the laptop. Compile a hello-world eBPF program. Verify it loads on the laptop's kernel.

3. **Day 3-4 — proof**: a 100-line eBPF program that does one useful thing — recommend: a per-interface drop counter that emits to userspace via a perf buffer. Wire the userspace side into bonsai as a new collector kind (read-only, no integration into the bus yet).

4. **Day 5 — scoping document**: `docs/research/ebpf_scoping_<date>.md`:
   - What eBPF unlocks that gNMI doesn't
   - Resource footprint (kernel verifier complexity, memory, CPU)
   - Integration surface (would bonsai treat eBPF as another collector? another telemetry source? something else?)
   - Risks (kernel version drift, license — eBPF programs are typically GPL)
   - Recommendation: should bonsai adopt eBPF in DV2+? What scope?

**What is explicitly out of scope**:
- Production-quality eBPF programs
- Integration into the streaming hot path
- Cross-kernel-version testing

**Where**: `experiments/ebpf_spike_<date>/` (a sibling to `experiments/`, may need to create the directory). Document at `docs/research/ebpf_scoping_<date>.md`.

**Effort**: 1 week.

**Done when**:
- Scoping document exists.
- 100-line proof compiles and produces real per-interface drop counts on the laptop.
- A recommendation for DV2 disposition (adopt | defer | reject) is in the scoping doc with rationale.

---

## <a id="tier-d5"></a>TIER D-5 — GNN Pre-Work

**Total estimate: 2 weeks of code, gated by archive depth (not in DV1 critical path).** Code-side scaffolding that lands during DV1 but doesn't run training until archive depth meets the GNN trigger (≥ 30 calendar days post-reset, ≥ 500 chaos injections, ≥ 50 examples per active rule). The trigger is unlikely to fire during DV1 — these are preparations for DV2 or DV3.

### D5-T1 (DV1) — Vendor-neutral feature engineering audit

The CV5 GNN philosophy commits: structural features dominate, vendor identity is a small one-hot tail. Audit `python/bonsai_ml/gnn/data_loader.py` to verify:
- Node features are mostly vendor-independent (degree, role-quartile, observed-protocol-set, recent-event-rate, time-since-event)
- Vendor identity is included but does not dominate embedding norm
- Empirically validatable via feature ablation when training runs

**Effort**: 1 day audit + 1 day adjustments.

### D5-T2 (DV1) — Heterogeneous GNN with GAT attention scaffolding

The CV6 T4 research adoption commits to heterogeneous GNN with GAT attention layers (Xi et al. 2026). Scaffold the model definition in `python/bonsai_ml/gnn/model.py`:
- HeteroData typing for Device / Interface / BgpNeighbor / BfdSession node types
- GATConv layers for message passing
- Output head for per-node anomaly score

**Effort**: 2 days. No training runs yet.

### D5-T3 (DV1) — Focal loss in training pipeline

The TAGAE adoption (CV6 T4-2) commits to focal loss with γ=2. Implement in `python/bonsai_ml/gnn/loss.py`:
- FocalLoss(γ, α) per the TAGAE paper
- Replace cross-entropy in the training script when training runs

**Effort**: 0.5 day.

### D5-T4 (DV1) — Calibration phase support in inference path

The CV5 GNN philosophy commits: deployment includes a 7-day "calibration phase" where scores accumulate but no detections fire. Scaffold the toggle:
- `bonsai.toml` config `[gnn] inference_mode = "calibration" | "production"`
- Operations workspace UI gains a calibration-mode banner
- During calibration: GNN scores compute, persist to a `gnn_calibration_scores` table, do not flow to the Detection table
- Transition: operator reviews 7-day distribution, flips to production

**Effort**: 2 days.

### D5-T5 (DV1) — Standardized evaluation harness (arxiv 2603.09675)

The CV6 T4 research adoption commits to using the standardized TSAD evaluation framework. Scaffold the harness in `python/bonsai_ml/gnn/eval.py`:
- Confusion matrix, F1, AUC-ROC, point-adjustment-aware F1
- Comparison study runner (rules vs tabular ML vs GNN)
- Output schema for the model card

**Effort**: 2 days.

**Done when (all D-5 tasks)**:
- Scaffolding compiles and unit tests pass.
- No training runs yet.
- The training pipeline is ready for the first archive-depth-triggered run, expected DV2 or DV3.

---

## <a id="tracked"></a>Tracked Future Threads

Unchanged from CV7. None built in DV1 beyond D-4's two items.

- Scale-up architecture paths B (partitioned cores) and C (read replicas)
- Cloud platform recipes (AWS / GCP / Azure docs) — when deployed there
- Beyond network platforms (firewalls, VPN, cloud networking)
- gNSI Phase-2 Acctz consumption — scoping doc exists, build deferred
- gNSI full client integration (Phase 3)
- Online learning infrastructure
- Investigation agent (parked behind token budget)
- UI bold-and-sharp full workspace re-articulation
- MCP server hardening (read-transaction-based Cypher)
- Adapter cursor cold-start persistence verification
- Memory pressure governance plumbing (CV6 N-1)
- Russh migration of `cli_capture.py`

---

## <a id="motivation"></a>Where We Are. Where We Intend to Be.

The operator's mood signal from the prior turn was real: "i was not happy in the way code review here was undertaken." The cause is now diagnosed (F-4: oversized files + surface-level review pattern) and the fix is in D-3 T5 (review discipline doc). The fix is structural, not aspirational.

The findings F-1 and F-2 are the kind that should have been caught earlier. They weren't, because nobody read the side-channel logs. The validation infrastructure itself was honest — it caught the failures, it dumped the diagnostics, it pushed the results to git. The gap was on the review side, not the test side. That's a comforting realization: the operational discipline that the operator built into CV7's validation pipeline is working. The discipline that was missing was on the AI-side review of the artefacts. DV1 D-3 T5 closes that loop.

**Where we are**: bonsai compiles, has a working architecture, has a documented sidecar protocol, has a validation harness that finds real bugs. The failures it found are localized and fixable.

**Where we intend to be at end of DV1**: validation script returns PASS=14 FAIL=0. The Python rules sidecar registers and detects. event_detection.rs is deleted. CLI parser has tests. End-to-end path-to-detection trace exists. main.rs and http_server.rs are normal-sized. K8s Helm chart is committed. eBPF scoping document exists. GNN pre-work is ready.

**Where we intend to be by end of DV3 or DV4**: the GNN northstar is met. Archive depth ≥ 30 days. GNN trained on real chaos data. Model card published with structural-feature dominance documented. Two clouds running labs. Bonsai deployable to a fresh Kubernetes cluster from the Helm chart.

The remaining mile is finite. About 8-12 weeks of work depending on archive accumulation. Most of it is finishing decisions already made.

---

## <a id="execution-order"></a>Execution Order

Tighter sequencing than prior CVs. The critical path is D-1 → D-2 (T1 specifically) → everything else.

### Week 1
- Day 1 — D1-T1 (HTTP bind error propagation) — 0.5 day code + 0.5 day testing
- Day 2 — D1-T2 (sidecar verification) — 0.5 day; D2-T1 starts (event_detection retirement gate)
- Day 3 — D2-T1 completes (gate closes, event_detection.rs deleted); D2-T2 step 2a (CLI audit) starts
- Day 4-5 — D2-T2 step 2b (CLI fixtures)

### Week 2
- Day 6-7 — D2-T2 step 2c (CLI smoke) + D2-T3 (E2E path-to-detection trace)
- Day 8-10 — D2-T4 (per-rule_id firing matrix)

### Week 3
- Day 11-12 — D3-T1 (split main.rs)
- Day 13-15 — D3-T2 (split http_server.rs)

### Week 4
- Day 16 — D3-T3 (doc consolidation pass 2), D3-T4 (delete pre_cv2_freeze), D3-T5 (review discipline doc)
- Day 17-19 — D4-T1 (K8s Helm chart)
- Day 20 — D4-T1 done; D4-T2 starts (eBPF spike)

### Week 5
- Day 21-25 — D4-T2 continues (eBPF spike completes by Day 25)

### Parallel throughout (Weeks 1-5)
- D5-T1 through D5-T5 — GNN pre-work scaffolding, ~7 working days spread across the sprint, owned by whoever has spare cycles
- Chaos cycle continues accumulating archive

**Total wall clock**: 5 weeks. **Total active work**: ~4 weeks. Aligns with the operator's hint that next iteration may have another cloud — the K8s chart will be useful for that.

---

## <a id="guardrails"></a>Guardrails — Updated

### New in DV1

- **Side-channel logs are part of the review.** Future AI reviews must read at least one validation report's `.logs/` directory before submitting findings. Encoded in `docs/review_discipline.md`.
- **Spawned tokio tasks have their JoinHandles tracked.** No more silent panics. `tokio::spawn(...)` without storing the handle in a `tokio::select!` shutdown set is the new red flag.
- **Files over 2000 lines are smells.** When a new module grows past 1500 lines, it's a code review concern, not a quality concern.
- **Every CV/DV ends with at least one feature item landed.** No more indefinite feature freeze. D-4 T1 (K8s) is DV1's commitment to this.
- **GNN pre-work runs in parallel with everything else.** It doesn't gate on operational milestones; it gates on archive depth alone.

### Unchanged from CV7

All prior architectural invariants. Streaming-first hot path. Layered ingestion. Discovery-driven onboarding. Vault-only credentials. The bonsai-mgmt network invariant. Feature index canonical. Bonpy is the operator surface for sidecar state. event_detection.rs is retired (by D2-T1). Mac is dev-only, no toolchain. Ubuntu laptop is ops-only, runs cargo for interim builds.

### Anti-patterns to reject

- "Defer the critical fixes; do the easier work first" — no, D-1 unblocks everything else. Start there.
- "Skip the side-channel logs; the summary verdict is enough" — no, exactly the failure mode that produced this whole sprint.
- "Add another consolidation tier instead of features" — no, the operator's signal was the opposite. Interleave.
- "Polish the existing UI before deleting event_detection.rs" — no, sequencing matters.
- "Test breadth comes later; ship features first" — no, F-5 and F-6 specifically.

---

## What DV1 Explicitly Excludes

- New protocol receivers (PCEP, NetFlow, sFlow)
- Multi-tenant deployment
- Customer-facing landing page or branding
- Online learning infrastructure (post-GNN)
- gNSI full client work (Phase 3)
- Investigation agent without token budget
- Russh migration (still tracked)
- Beyond-network positioning expansion

---

*DV1.0 — authored 2026-05-15 after the chunked code review that found three critical issues (F-1 silent HTTP panic, F-2 sidecar startup dependency on F-1, F-3 event_detection retirement gate unmet) plus seven additional findings on testing breadth, structural cleanup, and deferred features. DV1 fixes F-1 in 0.5 day (Variant C bind-before-spawn pattern), verifies F-2 in 30 minutes after F-1, closes the F-3 retirement gate in 1.5 hours of live smoke, brings CLI parser coverage to syslog parity (40 fixtures), authors one end-to-end gNMI-to-detection trace test, builds a per-rule_id firing matrix (18 rules × fires-on + does-not-fire-on), splits main.rs (2515 → ≤400 lines) and http_server.rs (7779 → ≤1500/file), consolidates docs (deletes memory/ if redundant, indexes playbooks/), deletes pre_cv2_freeze, authors a review-discipline doc to prevent the next round of surface-level reviews, lands a Kubernetes Helm chart with three modes (single/HA/fleet), and timeboxes a 1-week eBPF scoping spike. GNN pre-work (vendor-neutral feature engineering audit, heterogeneous GNN with GAT attention scaffolding, focal loss, calibration phase support, standardized evaluation harness) runs in parallel and lands in code without training. Total wall clock: 5 weeks. Total active work: ~4 weeks. References CV7 for unchanged architectural context and CHUNKED_CODE_REVIEW_2026-05-15.md for the findings rationale.*
