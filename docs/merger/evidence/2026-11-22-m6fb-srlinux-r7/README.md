# M6F-B — SR Linux production Core/collector acceptance (run r7)

Complete production-path run against a real Nokia SR Linux device,
executed twice with identical results (r6 = same stages, rev 4;
r7 = this directory, rev 5, with container logs captured).

## Path proven

Real SR Linux → authenticated Rust collector → Core-issued YANG
discovery job → live gNMI Capabilities + YANG Library Get → Core-side
applicability resolution → persisted device evidence → schema-bound
plan preview → explicit Admin activation → exact authorized
SubscribeRequest → real telemetry received → subscription lifecycle +
coverage evidence → revocation and verified withdrawal.

## Key identifiers

- Tested tree: `feat/m6fb-gnmi-interop` @ `ce9b2cb` + working-tree
  production fixes (gnmi_path module-prefix tolerance, Nokia YANG
  parser dialect + block-skip fix, assignment-override collector
  resync). Runtime binary sha256 prefix `a2765e3aac3f9221`
  (`/tmp/bonsai-m6fb-e2e/bonsai`, ci profile build lane).
- NOS: Nokia SR Linux `26.3.1-410`, image `ghcr.io/nokia/srlinux`
  (clab `nokia_srlinux` kind), gNMI `172.100.102.11:57400`.
- Core/collector image: `bn021d-bonsai:local`
  (`sha256:7e1c27f9ceb1e…`), collector id `m6f-col-1`.
- Discovery job `fdc72ed7-22fc-4cb1-916b-21adb9c30643` → completed,
  applicability snapshot 8 (`overall_status=applicable`), schema
  `ysc-d7254b6e840b0f0041c4b2458bb509a1`, content_id
  `1lg3lmex1lg3ln8{d1`, datastore `ietf-datastores:operational`.
- Plan `53a64d7c6aebc133`, approval revision 5, binding fingerprint in
  `api/21-plan-activate.json`. Selected path:
  `/interface[name=mgmt0]/statistics` SAMPLE 10 s — see `plan.json`.
- Coverage: `api/31-telemetry-observed.json` shows
  `interface[name=mgmt0]/statistics` `observed` with
  `first_observed_at_ns`/`last_observed_at_ns` advancing (~9 s apart).
- Revocation: `api/40-plan-revoke.json` state `revoked`;
  `api/42-post-revoke-assignment.json` + `verify_withdrawal` stage —
  observed timestamps stop advancing after revoke.
- No gNMI Set: the collector code path issues only Capabilities, Get
  and Subscribe RPCs; `collector.log`/`core.log` contain no Set.

## Topology / isolation

- `m6f-bonsai-e2e` (internal): core only.
- `m6f-srlinux-mgmt`: device + collector.
- Core→device gNMI: CLOSED (proven, `isolation-proof.json` in r6 run /
  earlier evidence); collector→device and collector→core mTLS: OPEN.

## Stage results (`api/00-summary.json`)

All stages PASS: admin_login, identity_device, collector_binding
(idempotent), device_credential, device_register, device_assignment,
collector_registered, catalogue_compile (11 modules / 419 paths),
discovery_job_queued, discovery_job_completed, applicability_snapshot,
plan_preview, plan_activate, telemetry_observed, plan_revoke,
withdrawal_verified. Overall: PASS.

## Known limitations (honest scope)

- Single vendor (SR Linux), single path, SAMPLE mode only.
- ON_CHANGE, delete semantics, other list keys/interfaces: NOT_EXECUTED.
- SR Linux reports statistics at container granularity with
  module-prefixed element names; leaf-level plan paths are not covered
  by container-granular notifications (see MATRIX.md notes).
- Post-revoke status rows remain `observed` with frozen timestamps
  until the freshness/stale sweep — withdrawal is proven by cessation
  of advancing observations, not row deletion.
- Stale-generation lifecycle resend denials appear in `core.log`
  (`assignment authority not current`) — expected fencing of old
  assignment generations, not a failure of the current session.
