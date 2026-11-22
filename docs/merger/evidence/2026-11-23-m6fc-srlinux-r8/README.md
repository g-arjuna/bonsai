# M6F-C — SR Linux production-path regression + telemetry granularity (run r8)

Regression run of the **merged M6F-C production code** (PR #55 schema-aware
coverage + authoritative withdrawal, PR #57 applicability negatives) against
the real Nokia SR Linux lab device — required because the new coverage code
changed how module-prefixed observations (`srl_nokia-interfaces:interface`)
establish coverage.

## Result: ALL STAGES PASS (`api/00-summary.json` overall=PASS)

- Tested tree: `feat/m6fc-l4-provenance-wiring` @ `9c76320`
  (= merged `main` `993d4f8` + lab-harness-only commits; `src/` identical
  to merged main). `00-provenance.json`: `tree_dirty=false`,
  `final_sha_acceptance_eligible=true`.
- Binary sha256 `080e9000239a…` (ci profile, shared build lane),
  runtime image `bn021d-bonsai:local` for both core and collector.
- NOS: Nokia SR Linux `26.3.1-410`, gNMI `172.100.102.11:57400` (clab CA TLS).
- Catalogue: **11-module `srl_nokia-*` closure, 419 paths, status=complete**,
  schema `ysc-4f20e39183beea7edbb16f2d943fa8bf`. Reproduced r7's scoped
  catalogue by `yang import` of the transitive `srl_nokia-interfaces`
  import closure (11 files, zero external imports) into a fresh library —
  the full 3-vendor sync (840 modules / 25 945 paths) exceeds
  `MAX_PATH_RESULTS` and is correctly ineligible (`partial` +
  `paths_truncated`); scoped catalogue selection is the intended operator
  flow. Note: schema_id differs from r7's `ysc-d7254b…` because module
  provenance fields (import vs sync source_ref) feed schema identity.
- Applicability snapshot 4: `overall_status=applicable`,
  datastore `ietf-datastores:operational`, content_id `1lg3lmex1lg3ln8{d1`.
- Plan `53a64d7c6aebc133` rev 3 activated; telemetry observed with
  advancing `last_observed_at_ns`; revocation verified through ALL
  authoritative checks: binding_revoked, acquisition_disabled,
  withdrawal_issued, collector_ack, subscription_terminated,
  no_continued_collection, old_session_rejected.

## Stage E — telemetry granularity findings

1. SR Linux emits the container-level JSON_IETF blob at
   `/interface[name=mgmt0]/statistics` (path elem
   `srl_nokia-interfaces:interface[name=mgmt0]`). Coverage is
   **container-granular by design**: `observation_covers` requires the
   observation to equal-or-descend from the expectation, so a container
   blob never fabricates coverage of unrequested child leaves, and the
   plan subscribes at the same container granularity.
2. Counter projection verified end-to-end (`api/32-device-counter-get.json`
   is a direct device Get captured after the run):
   - device `in-octets` = `1512523`, `out-octets` = `111058882`
   - API projection (`00-provenance.json metric_projection`) =
     `in_octets 1512029`, `out_octets 111057051` — a slightly older
     SAMPLE, consistent with counters advancing between samples.
   No production normalization deficiency demonstrated.
3. `collector_binding` returns http=400 on repeat drives (idempotent
   re-bind of an already-bound collector) — cosmetic, stage still PASS.

## Known limitations

- Regression scope: single path (`/interface[name=mgmt0]/statistics`
  SAMPLE 10 s). ON_CHANGE, deletes, other vendors unchanged.
- Lab-only mTLS; no gNMI Set anywhere in the path.
