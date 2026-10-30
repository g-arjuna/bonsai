# BN-005 implementation and acceptance closeout

15 October 2026, Asia/Kolkata. Implementation is in the working tree based on
`e4d8c44`; it has not been committed or pushed. The source hashes, commands,
exit codes and retained intermediate failures in
[evidence/results.json](evidence/2026-10-15-bn005-implementation/results.json)
identify the reviewed state. This report supersedes the outstanding work in the
[six-step closure plan](BN005_CLOSEOUT_PLAN_2026-10-15.md). Earlier reviews remain
historical evidence.

**Acceptance decision: BN-005 Verified for the supported contract below.** All
six closure subactions pass. This is a development acceptance gate, not a
production-readiness declaration for Bonsai or the entire merger.

## Analysis and implementation

The submitted strict parser, route-family allowlist and native read-only
transactions repaired the previously identified bypasses. Inspection found that
the two advertised remaining actions were insufficient to close the complete
plan. Investigator prefetch/model calls still bypassed the MCP gate; device tools
accepted additional ownership dimensions their stores could not establish;
summary builders ignored injected filters; incident results included neighbouring
devices; output truncation counted characters rather than encoded bytes. The
implementation addresses those boundaries as well as deployment and late history.

| Closure subaction | Resulting implementation and acceptance |
|---|---|
| 1. Principal and endpoint capabilities | Strict typed grants retained. Restricted identities use explicit endpoint policies. Legacy webhook/remediation grants stay in their route families. Speaker-only read keys may read their own investigations; global ApiReadonly retains its stricter role boundary. |
| 2. Scope support/default denial | An explicit supported subset is enforced, described below. Tenant labels cannot authorize reads. Unsupported combinations, unscoped summaries and restricted exporter ingestion fail closed. Encoded query keys/values are decoded before comparison and canonical encoding. Neighbour data is projected out of scoped incident/blast-radius results. |
| 3. Shared dispatcher | MCP and both investigator prefetch/model execution require ToolExecutionContext. HTTP investigations inherit authenticated role/scope. Automatic/default investigations use device-only read access. Raw dispatch/query helpers are private. Existing trusted library tests now supply explicit global identities; their original result assertions remain. |
| 4. Read-only/bounds/cancellation | Native read-only transactions and lexical transaction/multiple-statement rejection retained. MCP and explorer use a shared async runner with engine deadline and a cancellation watchdog that interrupts until work finishes. Worker cleanup is observed before a subsequent write. Query input is capped at 16 KiB; RANGE accepts literal bounds generating at most 10,000 values. Both the serialized explorer result and tool payload, including UTF-8/JSON escaping and metadata, fit 65,536 bytes. JSON-RPC/provider transport framing is outside the tool-payload cap. |
| 5. Actual deployment/restart | Actual routing executable rejects invalid token/path/address configurations before serving, and holds exclusive journal leases so a second process cannot share the files. Explicit loopback development mode and authenticated non-loopback startup are exercised. A hard process kill/restart restores route identity/history and accepts a newer epoch/attribute update. A separate Docker gate removes the first container and restores from the same named volume in a replacement container. |
| 6. Bounded late history | Sequence lookup uses binary search over ordered events. Event requests scan at most 10,000 records and return advancing scan cursors even for empty sparse pages. Diagnostics inspect a bounded recent window and expose historical coverage. Ingestion publication is serialized so a cursor cannot pass an allocated but unpersisted event. Tests cover late scoped records beyond 12,500, paging/reopen and concurrent writers. |

## Supported authorization contract

“Unsupported” means explicit denial, not an empty successful result. This is a
narrower, defensible contract than interpreting unsupported restrictions as
labels on global reads. Persisted tenant ownership is not implemented by this
change. No production tenant-isolation claim is made for the current data model.

| Surface | Supported identity/resource scope |
|---|---|
| MCP query_graph | Explicit global Viewer/Operator/Admin. All resource-scoped identities denied; ApiReadonly denied. |
| MCP device inventory, changes, detections, incident, blast radius | Global identities or **speaker-only** restrictions; at least ApiReadonly. VRF/tenant restrictions denied because these records have no corresponding ownership fields. Scoped neighbourhood results omit foreign devices and unowned neighbour applications/detections. |
| MCP prefix journey, peer timeline, evidence coverage, explain missing prefix | Speaker constraint, optionally RD; at least ApiReadonly. VRF-only transport evidence denied because transport gaps belong to exporters. Tenant restrictions denied. |
| MCP compare_snapshots | Speaker and/or RD, no tenant restriction; at least ApiReadonly. |
| MCP evaluate_change_plan | Operator/Admin; declared speaker/RD/tenant fields must match the submitted typed plan. This evaluates caller-supplied modeled data; it does not establish ownership of stored telemetry or authorize execution. |
| Public routing resource GETs | Speaker and/or RD for peers/routes/history/events/snapshot comparison. Transport-evidence diagnostics additionally require speaker. Tenant-restricted reads denied. |
| Public routing health/speakers/coverage/adjudication list | Explicit global access. Restricted callers denied because these builders do not implement complete resource predicates. |
| Public investigation APIs | Speaker-only restrictions. Own reads succeed; foreign resources and unsupported VRF/tenant combinations deny. Creation/completion still require Operator. |
| Public BMP ingestion | Explicit global routing write authority; resource-restricted keys cannot reset exporter-wide state. Internal collector ingestion retains its private service-token boundary. |
| Legacy webhook/remediation grants | Their explicit API route families; no global MCP, routing or detection access through role promotion. |

Client `_scoped_*` fields are stripped before trusted context is installed.
Denial responses for incident/investigation identifiers do not disclose the
foreign resource's owner. SQL predicates for device/detection reads precede result
limits. Global durable sequence cursors are pagination metadata, not per-tenant
sequence namespaces. Scoped simple routing responses return unknown aggregate
completeness rather than another speaker's global aggregate; individual route
observations retain their own completeness.

This policy has intentional compatibility effects: previously accepted composite
keys cannot read device/investigation records, tenant-restricted telemetry reads
are rejected, scoped summary/dashboard calls can return 403, and public exporter
injection needs explicit global write authority. Existing positive tests use
supported narrower grants, while added matrix assertions retain explicit denial
coverage for the formerly accepted unsupported combinations. No security assertion
was changed into a successful empty response.

## History contract and limits

- `/events`: maximum 10,000 inspected records per request, result limit 1–10,000.
  Follow `next_after_seq` until `truncated` is false. An empty page can still be
  partial; it advances through unrelated speakers/RDs without claiming absence.
  `scanned_events`, `scan_limit`, `scanned_to_seq` and `history_coverage` expose work.
  Complete means the requested interval after the supplied cursor is exhausted,
  not that a caller starting mid-journal received all earlier history.
- Prefix history, journey, peer timeline and evidence coverage: at most the latest
  10,000 records. `history_window` reports the full journal count, scanned sequence
  range/count and truncation; `history_coverage` describes historical coverage,
  independently of current RIB synchronization. Window counters are not lifetime
  totals. The first before-attribute baseline may predate a partial window.
- Snapshot comparison refuses to reconstruct a baseline from an incomplete tail:
  it returns partial coverage and no invented diff. Use paginated events for a
  complete reconstruction when history exceeds the budget. Diff result truncation
  is also explicit.
- Explain-prefix returns insufficient historical evidence when its window is
  partial; it cannot conclude that a route was never advertised from a bounded
  tail. Full-journal loading/recovery at startup remains unchanged. This gate
  establishes bounded request work, not production storage/retention/soak capacity.

## Reproductions and verification

Before correction, two new acceptance assertions failed: identical journal paths
were accepted, and Unicode/escaping exceeded the output byte cap. Both failure
logs are retained. Intermediate cancellation probes also failed: a one-shot
interrupt could be reset during statement setup, and very large RANGE constants
spent time in planning before native execution cancellation. The corrected runner
reissues interruption, and oversized/dynamic generators fail validation. The
accepted cancellation workload executes a large graph aggregation, verifies
worker completion and proves a subsequent write succeeds. The original row-count
sentinel regression was repaired without changing its expected value.

| Final command/gate | Result |
|---|---|
| `cargo test --locked --test bn005_closeout --test bn005_deployment_and_scope_hardening --test bn004_production_investigator_wiring --test r05_r06_runtime_wiring_and_scope --test a13_a20_change_assurance_and_executor --test r01_r04_routing_history_recovery --test b11_b12_b13_routing_workflow` | 62 passed: 11 closeout, 13 hardening, 6 mock investigator, 2 wiring/scope, 20 executor, 7 history, 3 workflow |
| `cargo test --locked --lib graph::explorer::tests` | 19 passed; other 249 library tests filtered, not claimed as run |
| `cargo test --locked -p rbmp-core -p rbmp-rib -p rbmp-store` | 132 passed; 2 old FRR 9.1 tests ignored, not counted as passes |
| `cargo test --locked --test bn003_live_routing_browser_acceptance -- --nocapture` with dedicated Docker host/evidence directory | 2 Rust tests and all 12 Chromium journeys passed against dedicated FRR 10.6.1 |
| `python3 tests/bn005_container_mount_acceptance.py <evidence-directory>` with Docker host | Actual named-volume container replacement passed |
| `PYTHONPATH=. .venv/bin/pytest tests/test_*.py` | 16 passed |
| `cargo fmt --check`, `git diff --check` | Passed |
| `cargo clippy -p rbmp-core --locked --offline -- -D warnings` | Passed; selected core, not whole-product lint certification |
| `npm --prefix ui run build` | Passed; existing accessibility/bundle warnings retained |

The advertised-tool matrix checks 12 tools × 4 roles × 8 global/restriction
combinations (384 cases in one test); actual HTTP matrix tests exercise supported
and unsupported resource dimensions and encoded aliases. Subprocess tests also
prove a concurrent second owner is rejected. A new explorer probe reproduced
oversized Unicode results before correction; the shared runner now applies its
byte budget to rows and column metadata, and attempts rollback through a drop
guard on every exit path.

The additional library check initially could not compile: 13 legacy test
initializers lacked newer struct fields, and two BMP OPEN fixture ASN literals
exceeded u16. Test-only neutral defaults and ASN 65000 (matching the encoded
capability) repair these fixtures without changing assertions or production
parsing. The failed/cancelled intermediate checks are retained separately from
passing final evidence. No failed or skipped run is counted as acceptance.

The Docker gate mounts the reviewed host binary into the recorded existing Bonsai
runtime image. It verifies real mount persistence/container replacement, not a
freshly built distributable image. Compose requires an `.env` file absent in this
checkout: ordinary rendering fails closed; rendering with `--no-env-resolution`
validates the declared token and mount configuration. Packaged-image installation
and upgrades remain BN-019. Container identities, image/binary hashes, before/
after history and logs are retained. Only uniquely named test containers/volume
are removed.

The live browser runner accepts BONSAI_EVIDENCE_DIR so the new captures/screenshots
are retained separately. Submitted October 13 evidence is preserved. Live FRR
traffic and synthetic VRF fixtures remain separately identified. No paid provider
calls, operational device configuration changes, commits or pushes are made by
this task; browser mutations are limited to its disposable dedicated FRR lab.

## Files changed in this closeout

- Routing startup/history: `src/bin/routing_service.rs`, `src/routing_service/mod.rs`, `src/routing_service/diagnostics.rs`, `crates/rbmp-store/src/journal.rs`.
- Authorization/dispatch: `src/http_server/auth.rs`, `src/http_server/mod.rs`, `src/http_server/remediation.rs`, `src/investigation_runtime.rs`, `src/mcp_server.rs`.
- Graph execution/repository predicates: `src/graph/explorer.rs`, `src/graph/mod.rs`, `src/http_server/observability.rs`.
- Acceptance/regression tests: `tests/bn005_closeout.rs`, `tests/bn005_container_mount_acceptance.py`, `tests/bn005_deployment_and_scope_hardening.rs`, `tests/bn003_live_routing_browser_acceptance.rs`, `tests/r05_r06_runtime_wiring_and_scope.rs`, `tests/a13_a20_change_assurance_and_executor.rs`.
- Additional test-only fixture repairs: `src/enrichment/registry.rs`, `src/registry.rs`, `src/signals/snmp.rs`, `src/signals/syslog.rs`, `src/streaming/mod.rs`, `src/streaming/bmp.rs`, `src/synthesizer/mod.rs`, `src/output/elastic.rs`, `src/output/splunk_hec.rs`, `src/output/traits.rs`, `src/integrations/servicenow_aiops.rs`, plus the fixture in `src/graph/mod.rs`.
- Current planning/evidence: `BONSAI_BACKLOG.md`, `docs/STATUS.md`, this report and `docs/merger/evidence/2026-10-15-bn005-implementation/`. Earlier user edits and historical evidence are preserved.

## Product work after this gate

BN-005 does not complete the other backlog items. BN-001 retains executor
crash-phase reconciliation, BN-002 operational keys/live-assurance provenance,
BN-003 real router VRF and deployed browser/graph assertions, and BN-004 real
provider quality/cost evaluation. The new synthetic routing-process and Docker
restart gates strengthen backend persistence evidence; they do not become a live
VRF browser journey or a real model benchmark. Persisted tenant ownership belongs
to the existing inventory/security work in BN-015; its absence is protected by
explicit denial in this gate. BN-006's second-NOS live gate is the next bounded
implementation item after verification. Operational device writes remain disabled.
