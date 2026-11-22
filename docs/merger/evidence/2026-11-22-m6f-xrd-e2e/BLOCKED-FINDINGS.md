# M6F-C XRd 24.4.2 — production-path acceptance: BLOCKED (truthful)

Date: 2026-11-22/23 (UTC timestamps in `api/*.json`)
Branch: `fix/m6fc-coverage` @ `c8943f3` (+ lab-script parameterization, uncommitted)
NOS: Cisco XRd 24.4.2 LNT, image `ios-xr/xrd-control-plane:24.4.2`
(`sha256:b3bd8e8ea4892d4dd9b0a32c223990965f6f874b6b7ebf732b27dd5ab3ca487f`)
gNMI: `172.100.103.21:9339`, plaintext (lab endpoint), creds `clab` (containerlab default)

## Result summary

| Layer | Result |
|---|---|
| Containerlab boot (inotify fix verified) | PASS — up >4h, IOS XR 24.4.2 banner |
| gNMI probe: Capabilities | PASS (`capabilities.json`, `probe-result.json` in `../2026-11-22-m6f-xrd/`) |
| gNMI probe: RFC 8525 YANG Library | PASS (`yang-library-ietf.json`) |
| gNMI probe: legacy YANG Library | PASS (`yang-library-legacy.json`) |
| gNMI probe: subscribe-once counters | PASS (`subscribe-once.json`) |
| Production path: admin→device→collector→discovery→applicability | PASS through snapshot 3 |
| Production path: schema-bound plan preview | **FAIL — correctly refused activation** |

## Why plan preview refused (precise reasons)

`api/20-plan-preview.json` → `activation_permitted=false`,
`reasons=[snapshot_not_fresh, freshness:EvidenceTruncated,
overall_status:conflicted, path[0]:unknown]`.
Applicability snapshot 3 (`api/12-applicability.json`):
`overall_status=conflicted`, datastore resolved to
`ietf-datastores:running`.

Three independent, genuine vendor/library mismatches — none are Bonsai
defects and none were worked around:

1. **No `operational` datastore declared.** XRd's RFC 8525 YANG Library
   (`yang-library-ietf.json`) declares only
   `ietf-datastores:candiate` (sic — Cisco spelling) and
   `ietf-datastores:running`, both bound to schema `All`. A plan
   requesting `ietf-datastores:operational` resolves to
   `datastore_schema_missing` → whole effective set blocked. Re-running
   discovery with `M6F_DATASTORE=ietf-datastores:running` resolved the
   datastore (snapshot 3) — kept as the honest XRd datastore identity.

2. **Module revision mismatch (OpenConfig).** XRd 24.4.2's module-set
   `All` reports e.g. `openconfig-interfaces` rev `2022-10-25`,
   `openconfig-types` `2019-04-16`, `openconfig-yang-types` `2021-07-14`.
   The pinned Bonsai catalogue compiles 2026-era revisions
   (`openconfig-interfaces` `2026-10-05`). Every openconfig module
   resolves `module_revision_unresolved` → `unknown`. Matching would
   require a revision-pinned OpenConfig source closure for XRd 24.4.2 —
   not present in `config/yang_sources.toml` today.

3. **Organization conflict (IETF modules).** XRd capabilities advertise
   `ietf-interfaces`/`ietf-yang-types` with organization
   `IETF NETMOD (NETCONF Data Modeling Language) Working Group`; the
   compiled YANG modules declare
   `IETF NETMOD (Network Modeling) Working Group`. Free-text org
   mismatch → `module_organization_conflict` → `conflicted` (tested,
   deliberate fail-closed policy in
   `src/yang_schema/applicability.rs` `evaluate_module`; covered by
   `tests/yang_device_applicability.rs` T3). Even a revision-matched
   catalogue cannot clear this while the policy stands — the org string
   is descriptive metadata, not YANG module identity
   (namespace+name+revision), so this is flagged as a candidate
   policy-review item, not silently relaxed.

`freshness:EvidenceTruncated` / `snapshot_not_fresh` are downstream
consequences of `overall_status=conflicted` (freshness treats
conflicted/unknown snapshots as ineligible evidence).

## What was NOT done

- No schema-authority weakening, no partial-catalogue activation, no
  capabilities-only bypass, no fabricated applicability.
- No gNMI Set or live device writes.
- The 8-module scoped catalogue used here
  (`ysc-ba690e83a7084df6fa0070b3a5567304`, complete, 165 paths) is an
  operator-scoped lab catalogue (openconfig-interfaces closure), not a
  claim of full XRd schema coverage.

## Path to unblock (for backlog)

- Pin an OpenConfig (and IETF) source closure matching XRd 24.4.2's
  reported revisions, or extend the resolver to select per-device
  revision candidates without weakening identity checks.
- Review whether capabilities-vs-catalogue `organization` free-text
  mismatch should remain `Conflicted` (current tested policy) or become
  advisory — decision belongs to schema-authority owners.
- XRd declares no operational datastore; confirm which datastore gNMI
  subscriptions actually bind to before treating `running` as the
  telemetry datastore identity.
