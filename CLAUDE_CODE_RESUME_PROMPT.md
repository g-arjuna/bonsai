# Claude Code — Session Resume Prompt

> Paste this as the first message in a new Claude Code session. The prompt assumes the five Phase 5 documents are in the repo root.

---

I am resuming work on **bonsai**, a streaming-first graph-native network state engine. You will be my pair programmer for the Rust core and Python SDK work.

## Critical first step — read these in order

Before suggesting any code changes, read these five documents from the repo root:

1. `CLAUDE.md` — project identity, scope guardrails, non-negotiable rules
2. `DECISIONS.md` — the complete architectural decision log (append-only, never edit past entries)
3. `PHASE5_REVIEW_AND_DESIGN.md` — the Phase 1–4 audit and Phase 5 architectural baseline
4. `PHASE5_ADDENDUM_REMEDIATION_AND_NLQUERY.md` — three-layer remediation design, NL query plans
5. `PHASE5_PLAYBOOK_LIBRARY_BOOTSTRAP.md` — how the playbook catalog grows

You do not need to read `PHASE5_HARVEST_PROMPT.md` — that is being executed in parallel on a separate tool (Codex). The catalog YAML files produced by that workflow will land in `playbooks/library/` and we consume them from here.

After reading, acknowledge:
- The current phase (Phase 4 complete, moving into Phase 5.0 hygiene)
- The scope guardrails (no SNMP/NETCONF/Kubernetes/UI-for-config, gNMI-only, four vendor families)
- The fact that Phase 5.0 **precedes** Phase 5 ML coding and consists of hygiene + playbook catalog scaffolding

## Current state of the repo

Phases 1–4 are complete. The gNMI subscriber pool works, multi-vendor telemetry is writing into LadybugDB, the gRPC API exposes query and stream endpoints, the Python SDK runs a rule-based detection and remediation loop, and `DetectionEvent` / `Remediation` nodes accumulate in the graph as labelled Phase 5 training data.

The Phase 4 demo is working end-to-end on the `lab/fast-iteration/bonsai-phase4.clab.yml` topology (SRL spine-leaf plus XRd PE). cRPD is deferred. cEOS works but is not in the active Phase 4 topology.

## What we are doing in this session and the ones that follow

We are in a **pre-Phase-5 hygiene and catalog scaffolding phase** — call it Phase 5.0. No ML code yet. No NL query work yet. The goal is to leave the codebase clean and ready so that when Phase 5 ML work starts, there are no architectural surprises.

The agreed work items, in order:

1. **Update `README.md`** — current status wrongly says Phase 2 in progress; set it to "Phase 4 complete, Phase 5.0 in progress." Fix the "temporal by design" claim — we have `StateChangeEvent` append-only history but no bitemporal `valid_from` / `valid_to`. State this honestly.

2. **Write three ADRs in `DECISIONS.md`**:
   - Junos classifier paths in `telemetry.rs` — decide: remove, or leave with a `TODO(cRPD deferred)` comment. Prefer removal; cRPD is not in scope right now and dead code rots.
   - Event retention deferral — document that StateChangeEvent pruning is deferred to Phase 5.5, reference the scaffold we will create in item 6.
   - Schema migration deferral — document that adding columns to existing tables is not yet supported and will require a migration story in a future phase.

3. **Add a `TRIGGERED_BY` edge in `graph.rs`** from DetectionEvent to the StateChangeEvent that caused it. When a rule fires on a `bgp_session_change` event, the DetectionEvent should link to that specific StateChangeEvent so Phase 5 ML can trace inputs to detections without timestamp-guessing. Update the schema init, the Python SDK's `create_detection` call path, and the detection write path.

4. **Add a `/metrics` Prometheus endpoint** exposing:
   - telemetry updates per second per device (labelled)
   - graph write latency histogram (p50/p99)
   - broadcast event lag (subscriber drops counter)
   - subscriber reconnect counter (labelled by target)
   Use the `metrics` crate plus `metrics-exporter-prometheus`. Serve on a separate port (e.g., `[::1]:9090`) configurable via `bonsai.toml`. Keep it small — this is observability-of-bonsai, not product features.

5. **Write one integration test** (`tests/integration_bgp_flap.rs` or a Python pytest) that:
   - assumes ContainerLab is already running the Phase 4 topology
   - starts bonsai as a subprocess
   - waits for Capabilities and initial graph population
   - injects a BGP flap via SSH to an SRL node
   - asserts a `bgp_session_down` DetectionEvent exists in the graph within 30 seconds
   - cleans up
   This is a smoke test, not exhaustive — one test is enough for now. If Python is easier, use pytest with `subprocess`.

6. **Scaffold the `retention` module** (`src/retention.rs`) with:
   - A `prune_events(store, cutoff: OffsetDateTime) -> PruneStats` function that runs a Cypher delete on StateChangeEvents older than the cutoff
   - A tokio interval spawned from `main.rs` that calls it (disabled by default via config)
   - No behavioural change unless explicitly enabled
   The point is that the seam exists.

7. **Scaffold the `DeviceRegistry` trait** (`src/registry.rs`) with:
   - The trait as defined in `PHASE5_REVIEW_AND_DESIGN.md` (list_active, subscribe_changes)
   - A `FileRegistry` concrete impl that wraps today's `bonsai.toml` loader
   - `main.rs` loads the registry and consumes `RegistryChange` events in the main loop (today only `Added` events at startup, no dynamic changes)
   - No notify/file-watch yet. That's Phase 4.5.

8. **Scaffold `python/bonsai_sdk/playbooks/`** with:
   - `catalog.py` — loads YAML from `library/`, exposes `for_detection(rule_id, vendor)` returning matching playbook entries
   - `executor.py` — `PlaybookExecutor` that accepts a playbook + detection, walks the steps, calls `client.push_remediation` per gnmi_set step, runs the verification Cypher query
   - `library/__README__.md` — brief note pointing to the Codex harvest workflow
   - `library/bgp_session_down.yaml` — migrate the existing hardcoded SRL BGP admin-state bounce into the first YAML entry. This is the schema-shake-down entry; get it right, the rest follow the pattern.
   - Refactor `remediations.py` so `RemediationExecutor` delegates to `PlaybookExecutor` for playbook dispatch. Keep the circuit breaker and dry-run logic where they are.

## Working discipline for this session

- **One item at a time.** Complete each numbered task, confirm it compiles and tests pass, commit with a clear message, move to the next. Do not batch.
- **Every architectural decision gets an ADR entry** in `DECISIONS.md` with today's date. If you find yourself making a choice (e.g., which crate for metrics), capture the decision before writing the code.
- **Rust code must compile before ending any session.** Use `cargo build --release` on Windows (debug mode hits the MSVC 4GB static lib limit due to lbug). Clippy must pass with `-D warnings`.
- **Do not expand scope.** If you notice something worth doing that is not in the list above, add it to a `NEXT_UP.md` note and flag it, but do not start on it.
- **The `playbooks/library/` directory is shared with a parallel harvest workflow running in Codex.** Do not write playbook YAML yourself in this session except for the one migration entry (item 8). Harvest-produced entries will arrive via commits; your job is to make the executor correctly consume them when they do.

## What to do first

Read the five docs listed at the top. Then tell me:
1. Your one-sentence summary of what bonsai is and where we are
2. Which of the 8 tasks you propose to tackle first and why
3. Any open questions about scope, schema, or constraints before you touch code

Then we start on item 1 or your proposed first item.
