# Bonsai current state

8 November repository review: BN-006 bounded live acceptance is complete;
the detailed snapshot below is the 30 October baseline. BN-006 code and fixtures
were committed in `0b89f00`. The EOS reconnect replay regression is fixed, including legacy
event and checkpoint recovery. Live EOS lifecycle, offline policy update, virtual FIB,
and SR Linux native collection evidence is in [BN-006 evidence](merger/evidence/2026-11-07-bn006-live/README.md).

Baseline snapshot: 30 October 2026 (Asia/Kolkata). Base Git commit: `e4d8c44`.
The BN-001–005 implementation is uncommitted working-tree code. Source hashes
and fresh results identify exactly what this review inspected.

**Standing: integrated development prototype.** Routing history/recovery is
substantially improved, dual live FRR streams and the browser workflow have
reproducible coverage. All three BN-001/002/003 items still have acceptance gaps.

Work from [BONSAI_BACKLOG.md](../BONSAI_BACKLOG.md). Latest review:
[30 October code/environment analysis and BN-006 native slice](merger/CURRENT_STATE_REVIEW_2026-10-30.md).
[Current evidence](merger/evidence/2026-10-30-bn006/results.json).
Earlier reviews and counts below are historical snapshots, not new live acceptance.

30 October revalidation: **46 focused Rust + 132 imported-crate tests** pass
(2 ignored tests excluded), plus **16 Python tests**. UI build and format/diff
checks pass; existing UI warnings remain. The new live slice is SR Linux CLI
capture only, not fresh FRR/browser or full second-NOS route acceptance.

| Area | Verified subcriteria | Remaining acceptance |
|---|---|---|
| BN-001 Partial | Durable scope/intent/completion records, direct failure/reopen tests, replay binding, open-time corruption/tail checks, cross-process scope ownership confirmed; runtime reload fails closed on corrupted/empty journal on disk (JournalPersistenceError); off-to-the-side atomic parsing under lock | Phase-specific process-kill and crash-phase recovery tests; actual device writes remain disabled |
| BN-002 Partial | Length-prefixed approval signing, mandatory enforced backing/identity/authority, report-fidelity binding, simulation-replay rejection; length-prefixed canonical report encoding eliminating identity delimiter collisions and binding limitations; enforced replay requires signing authority | Operational key lifecycle/migration policy and trusted live-lab assurance provenance (BN-014) |
| BN-003 Partial | Fresh 12-browser-journey run passes; submitted/fresh capture has zero exact fixture matches; native cookie and token-revocation checks exercised | Actual router VRF and deployed browser/graph restart assertions remain; BN-005 separately verifies actual process/container restart with new ingest |
| Routing history R01–R04 | 7 current regressions pass | Production scale/fault/soak remains release work |
| Wiring/scope R05–R06 / BN-004 | 2 wiring/scope regressions pass; provider clients have mock integration coverage | BN-004 real model comparison pending; mock scenario token/cost/conclusion is not model evidence; deployment/query scope under BN-005 |
| BN-005 Partial — temporary exception | Prior strict closure and current strict-mode regression assertions remain; default `audit_only` now relaxes tenant/unsupported-VRF denials at user request. Mappings persist; admin policy endpoint can restore strict denials | Tenant isolation is not enforced in compatibility mode. Authentication, roles, speaker and supported RD restrictions remain. See [policy contract](architecture/ownership_mapping.md) |
| BN-006 Complete (bounded live profile) | EOS 4.36.0F BMP lifecycle and offline startup-policy change across default and BLUE/RD 65001:100; cEOS software-FIB route/withdrawal probes; live SR Linux 26.3.1 collection through operator API and MCP; reconnect replay regression | No periodic scheduler, physical ASIC claim, IPv6, extra release/vendor coverage, or DuckDB EOR retirement parity. See [evidence and support limits](merger/evidence/2026-11-07-bn006-live/README.md) |
| ML/research | Metrics/alignment/prototype smoke evidence remains | Trained held-out models, actual forecasting/provider comparisons remain separate gates |
| Release hygiene | Format, strict selected-core Clippy and UI build pass; UI build warning-free with route-level code splitting and live-doc links verified (8 Nov, [UI](merger/evidence/2026-11-08-bn019-ui-warnings/README.md), [links](merger/evidence/2026-11-08-bn019-link-debt/README.md)) | Full-product release gates remain |

BN-005 closeout snapshot verification: **62 focused Rust tests**, **19 explorer unit tests**, **132 imported-crate tests**
(**2 older FRR tests remain ignored and are not counted as passes**), and **12 live
FRR Chromium journeys** (plus both Rust browser orchestrator tests) pass. The real
Docker named-volume/container replacement gate and **16 Python tests** pass.
Formatting, strict selected-core Clippy and UI build pass; existing UI warnings remain.
The library test target compiles after repairing stale test fixtures; the other
249 library tests and a full workspace release/soak run are not claimed as executed.

The strict BN-005 profile remains tested. The current default has a user-requested
temporary exception: tenant and unsupported-VRF ownership restrictions no longer
block access. Speaker and supported routing RD filters remain. This default is
not a tenant isolation guarantee. Use the admin policy API to restore `strict`.
The original closeout manifest identifies its earlier tested source snapshot.

**Next action:** BN-019 operational/release proof (install/upgrade, packaged
build, lab browser acceptance, backups/recovery, soak); UI warning and
link/provenance debt are cleared. BN-009/BN-013 intake triage remains doable
without external deps.
BN-003 genuine router VRF/deployed browser-graph parity, and BN-004 real
provider evaluation remain open. Operational device writes remain disabled.

BN-007 intake (8 November): legacy/duplicate-authority inventory completed;
fixed a settings-API path that could spawn a second BMP authority, a dead
`[bmp]` config block, and license-gate failures (`cargo deny` passes;
135 imported-crate tests pass). Added `bonsai-migrate` operator CLI
(backup/migrate/rollback, SHA-verified end-to-end), `authority` typo warning,
and LICENSE/NOTICE RustyBMP attribution. Journal backups confirmed covered
via `bonsai_graph` volumes. Leftovers disposed: rbmp crates versioned to
0.2.0, `duckdb-store` kept optional-disabled pending EOR parity, legacy
receiver retirement still blocked on projection parity, CI/install gates
deferred to BN-019. Evidence: [BN-007 audit](merger/evidence/2026-11-08-bn007-audit/README.md).

BN-009 lifecycle slice (8 November): `collector_engine.py` heartbeat
`status_json` now reports uptime/cpu/rss/queue-depth/drops/rules-loaded
(stdlib /proc, null-safe); Sidecars UI gained a gRPC-registry panel rendering
`/api/sidecars` with parsed metrics — the registry was previously not
surfaced in the UI. Verified: py_compile, /proc parsing, clean UI build.
Supervision/resource-limit gaps closed by inspection (single combined
sidecar unit + `BONSAI_SIDECAR_MAX_MEM_MB` enforcement). Remaining: live
heartbeat exercise and the restart/overload soak acceptance.
Evidence: [BN-009 lifecycle](merger/evidence/2026-11-08-bn009-lifecycle/README.md).

BN-009 intake reconciliation (8 November): existing sidecar lifecycle
surfaces inventoried — registry/heartbeat/stale-lost, `BONSAI_REQUIRE_SIDECAR`
health gate, `/api/ml/{exports,schedules,jobs}` incl. cancel, job_engine
memory-pressure shedding, `/sidecars` UI route, rules-sidecar systemd unit.
Build should extend these; gaps are unified event contract, watchdog
coverage, backlog visibility, per-sidecar resource metrics, soak acceptance.
Evidence: [BN-009 intake](merger/evidence/2026-11-08-bn009-intake/README.md).

BN-003 genuine VRF BMP — WIRE PROOF PASSED (8 November): rebuilt the lab
per FRR 10.6.2 research — kernel VRF BLUE provisioned as root before FRR
start (`bootstrap.sh` via containerlab `cmd:`), `rd vpn export 65000:100` in
the VRF BGP instance, real eBGP route `10.99.1.0/24` from frr2. FRR emitted
genuine BMP Peer Up + Route Monitoring with peer type=1 RD-instance
distinguisher 65000:100 — verified on the wire by `verify_bmp.py`, 5 frames
captured. Earlier failure root-caused: FRR does not create Linux VRFs and
never needed the capability workaround. Lab `bonsai-bn003-vrf` left running.
Remaining: deployed
browser/graph restart journey and acceptance-test fixture replacement.
Phase 2 same day: `bonsai-routing` ingested the genuine stream — RdInstance
peer 65000:100 up, `10.99.1.0/24` in LocRib + AdjRibIn pre/post-policy views
with VRF key/exporter identity preserved; restart gap honestly recorded.
Evidence:
[genuine VRF BMP](merger/evidence/2026-11-08-bn003-genuine-vrf-bmp/README.md).
Earlier blocked attempt:
[BN-003 VRF lab](merger/evidence/2026-11-08-bn003-vrf-lab/README.md). NetBox restarted (healthy, 8 seeded
devices); Splunk/Elastic/Kibana/Grafana/Prometheus confirmed up; stale clab
containers removed. Evidence:
[BN-003 VRF lab](merger/evidence/2026-11-08-bn003-vrf-lab/README.md).

BN-019 link/provenance debt (8 November): scripted check of 164 relative links
across 84 live docs found 4 dead links (absolute `file://`/machine paths, two
pre-consolidation backlog paths, and a never-committed CV7 sprint doc in
`sidecars.md`); all fixed, re-check clean. Archived backlogs excluded per their
freeze policy; `B10_IMPORT_MANIFEST.md`, `LICENSE`, `NOTICE` verified present.
Evidence: [link debt](merger/evidence/2026-11-08-bn019-link-debt/README.md).

BN-019 progress (8 November): UI accessibility/bundle warning debt cleared —
zero-warning `vite build`, route-level code splitting (entry 637 kB → 98 kB),
proper label/listbox/dialog semantics and programmatic palette focus. Browser
smoke over 15 hash routes passed on `vite preview`. Install/upgrade, lab
acceptance, backup/recovery and soak gates remain. Evidence:
[BN-019 UI warnings](merger/evidence/2026-11-08-bn019-ui-warnings/README.md).

BN-002 progress (8 November): signing keys and revocations are now durable in a
versioned key store beside the executor journal — register/rotate-overlap/revoke
survive restart and propagate across executor instances; corrupt or vanished
stores fail closed. Assurance reports carry optional HMAC signatures that
enforced mode now requires (`MissingAssuranceSignature`). 34 focused tests pass
(20 + 8 + 6). Remaining: vault custody, per-role key separation, boundary
transport, asymmetric crypto. Evidence:
[BN-002 key lifecycle](merger/evidence/2026-11-08-bn002-key-lifecycle/README.md).

BN-001 closeout (8 November): phase-specific process-kill recovery is verified
at all five durable executor journal phases using real child-process aborts, and
interrupted intents reconcile via the operator-driven
`abort_interrupted_execution` API (durable `ExecutionAborted` + scoped lock
release). 28 focused tests pass (20 existing + 8 new). Limits and evidence:
[BN-001 crash recovery](merger/evidence/2026-11-08-bn001-crash-recovery/README.md).

Ownership mapping follow-up (BN-015): explicit device and exporter/RD → tenant
bindings persist in the graph, with admin assignment/list/audit endpoints. Missing
and mismatched mappings add no access denials. Tenant/unsupported-VRF denials are
now temporarily relaxed; authentication, roles and supported speaker/RD controls remain.
This is an inventory/audit foundation, not completed tenant isolation. See the
[rollout contract](architecture/ownership_mapping.md).

Ownership follow-up verification: **3 ownership tests + 32 existing regressions**
pass; formatting and diff checks pass. The ownership suite includes JWT and
API-key administrator attribution. [Evidence](merger/evidence/2026-10-15-ownership-audit/results.json).
No operational owner assignments have been populated.

Temporary access-policy follow-up: **38 distinct Rust tests pass** (35 existing
mapping/regression tests plus 3 compatibility tests). Strict acceptance fixtures
retain all original assertions; separate tests verify the requested relaxation,
retained speaker/RD/role constraints and persisted admin restore switch.
[Current evidence](merger/evidence/2026-10-15-ownership-compatibility/results.json).


8 November BN-006 closeout: the live evidence validates only the bounded support
profile in the linked report. Native collection is operator-triggered; no scheduled
polling is present. The test used isolated task-owned cEOS/SR Linux nodes and made
no live configuration writes. BN-007 still owns the optional DuckDB EOR lifecycle gap.
