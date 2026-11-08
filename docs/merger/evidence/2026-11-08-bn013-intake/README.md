# BN-013 intake triage — 8 November 2026

Base commit `26dbd39`. Inventory of existing incident/evidence/operator
workflow surfaces before building. Acceptance (grounded RCA, adjudicated
correctness, feedback provenance, DB-controlled rules/playbooks, regression
scenarios) remains open.

## What already exists

- **UI**: `ui/src/routes/Incidents.svelte` (716 lines), `Investigations.svelte`
  (380), `Operations.svelte`, `Audit.svelte`, `Governance.svelte`,
  `Approvals.svelte`, `SynthesizerRules.svelte`.
- **Runtime**: `src/investigation_runtime.rs` (1375), `src/investigation_trigger.rs`
  (256), `src/playbook.rs` (84), `src/ai_provider.rs` (provider clients —
  mock-covered under BN-004).
- **Playbooks/rules**: `playbooks/library/*.yaml` (~10+ rule packs: bgp
  session down/flap/never-established, interface down/error spike, ddos,
  bfd/isis/ospf adjacency), `playbooks/sources/`, gap analysis + day-2 ops
  matrix docs.
- **Prefix investigator**: `src/routing_service/diagnostics.rs` +
  `tests/a04_a09_b22_prefix_investigator.rs` (evidence coverage, prefix
  journey, peer timeline, adjudicate-prefix endpoint exists in
  `routing_v1_routes`).
- **Audit**: `src/audit.rs`, approval flow (`Approvals.svelte`,
  `handle_routing_restart`-style control endpoints in tests).

## Gaps vs BN-013 acceptance

1. Grounded RCA quality unproven — provider calls are mock-covered only
   (BN-004 dependency for real evaluation).
2. Recommendation-vs-adjudicated-correctness loop not evidenced end-to-end.
3. Feedback provenance (operator corrections feeding rules) not traced.
4. DB-controlled rules/playbooks: library is file-based YAML; DB control
   surface needs reconciliation.
5. Regression-scenario harness exists (`scenario_pack.rs`) but no linked
   incident→scenario regression gate.

## Suggested first bounded slice

Trace one real incident journey end-to-end on the live lab stack
(detection → incident → investigation → recommendation → operator
disposition) and record where the chain breaks; that gap list becomes the
build queue. Depends on BN-004 only for model-quality claims, not for the
workflow trace.
