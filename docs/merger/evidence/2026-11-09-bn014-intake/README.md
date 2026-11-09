# BN-014 intake triage — real lab change assurance / read-only shadow pilot (9 Nov 2026)

Acceptance: collect baseline/candidate state from dedicated NOS instances,
independent invariants + provenance, failures/recovery rehearsal, durable
operator audit; report utility + failure taxonomy over real episodes. No
autonomous live remediation until durable/signature gates + explicit
authorization. Deps: BN-001/002/006/013.

## Existing machinery (verified in code)

- `src/integrations/change_management.rs` (718 ln): `ChangeRequestRecord`,
  `ActiveChangeContext`, ServiceNow change sync (`run_change_sync`,
  `maybe_start`), `active_change_context`, `link_event_to_change`,
  `link_incident_to_change`, webhook ingest (`ingest_webhook_change`,
  `WebhookChangeEvent`).
- HTTP surface: `GET /api/changes`, `GET /api/changes/context/{device}`,
  `POST /api/webhooks/change-event`, `POST /api/integrations/servicenow/changes/sync`.
- `src/http_server/governance.rs` (1478 ln): governance/policy surface.
- `src/routing_service/scenario_pack.rs` (1667 ln): scenario harness —
  candidate for rehearsal/replay evidence.
- `src/remediation/approval.rs` (2147 ln): approval/signing gates (BN-002
  dependency — length-prefixed signing already landed).
- Investigation runtime already emits "change management context was not
  checked" coverage-gap warning — the join point exists but is unused.

## Gaps vs acceptance

1. **No baseline/candidate state collection** — nothing snapshots device
   config/RIB state before vs after a change window on dedicated NOS
   instances. Closest: journal/RIB history exists (BN-003/005) but is not
   framed as change baseline/candidate.
2. **No independent invariants** — no checker that asserts expected
   post-change properties (e.g. prefix still reachable, session stays
   up, RPKI/adj-rib sane) independent of the change system's own report.
3. **No failures/recovery rehearsal** — scenario_pack exists but no
   change-scoped rehearsal that injects failure and verifies detection +
   audit trail.
4. **Provenance/audit** — approval signing exists (BN-002); change↔
   incident/event linking exists; but no end-to-end episode record
   joining change → state diff → invariant verdict → operator audit.
5. **Utility/failure taxonomy report** — nothing aggregates episodes
   into the required report.

## Proposed slices (ordered)

1. **BN-014a — baseline/candidate capture**: on change-window open/close
   (webhook or manual), snapshot targeted device RIB/state via existing
   journal/routing APIs into a durable `ChangeEpisode` record.
2. **BN-014b — invariant checker**: small set of independent post-change
   assertions evaluated against captured state; verdicts persisted.
3. **BN-014c — rehearsal**: scenario_pack-driven failure injection during
   a lab change window; verify detection + audit.
4. **BN-014d — episode report**: aggregate episodes → utility + failure
   taxonomy doc/API.

First slice proposed: BN-014a baseline/candidate capture on the existing
containerlab NOS instances (no device writes — read-only state pulls).
