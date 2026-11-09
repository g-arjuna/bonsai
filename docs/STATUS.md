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
| BN-003 Partial | Fresh 12-browser-journey run passes; submitted/fresh capture has zero exact fixture matches; native cookie and token-revocation checks exercised. 8 Nov: acceptance gate rewired to the genuine VRF stream (no `build_rd_bmp_frames` injection) — 12/12 Playwright journeys pass on real FRR 10.6.1 dual-session BMP (global + VRF BLUE, RD 65000:100, 10.99.1.0/24); `run_bmp_listener` epoch-sharing fix verified live (`complete_for_observed_view` + journal restart recovery) | Live-router VRF browser acceptance now evidenced; remaining gates are operational (BN-014/019 soak/pilot) |
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

**Next action:** BN-019 operational/release proof is effectively complete —
packaged build/boot, volume backup→restore, install/upgrade, `bonsai-migrate`
drill, packaged-deployment browser acceptance (12/12 journeys on genuine
FRR BMP), bounded packaged soak, and handoff reconciliation all evidenced.
Only the full-length soak window remains open (shared with BN-009).
UI warning and link/provenance debt are cleared. BN-009/BN-013 intake triage remains doable
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

BN-009 live heartbeat (8 November): `collector_engine.py` registered with
the packaged deployment over gRPC — `/api/sidecars` shows healthy with
advancing `status_json` (uptime/cpu/rss/queue/rules_loaded=35); SIGTERM →
registry transitions stale (45s) → lost (120s) matching
`sidecar_registry.rs` thresholds. Gap found: sidecar ML/export jobs hit
unauthenticated 401s on the JWT API (no credential path); heartbeat
timestamp fields render null. Evidence:
[live heartbeat](merger/evidence/2026-11-08-bn009-live-heartbeat/README.md).

BN-009 sidecar→API auth (9 November): `BONSAI_API_TOKEN` (`bsk_` API key)
support added via `bonsai_ml/api_auth.py` and injected into all sidecar
HTTP call sites (job engine aiohttp, requests workers, urllib probes,
`BonsaiClient` env fallback, snapshot/inference clients). Verified live
against the packaged deployment: minted `bn009-sidecar` key (operator
scope), restarted `collector_engine` — zero 401s (was 21), ML jobs
succeed via scheduler with real `MlJobRun` records, schedules/exports
writes land. Also fixed `export_job` `quality_json` type bug (PATCH 422 →
records stuck `running`). `last_heartbeat_at` null was a field-name
expectation, not an API defect (`*_ns` fields populated). New findings:
`/api/graph/snapshot` 500s on missing DuckDB `TIMESTAMP_NS` (pre-existing,
affects JWT too); pandas absent in venv → empty parquet exports.
Evidence: [api-auth](merger/evidence/2026-11-09-bn009-api-auth/README.md).

BN-009 restart/overload slice (9 November): sidecar SIGKILL → registry
stale (~61 s) → lost (~125 s) → restart re-registered healthy with zero
401s; `docker restart` of the app container exercised the
`reregister_required` heartbeat path (new sidecar_id, job/export/schedule
state durable); routing container restart replayed the journal to
identical RIB state (`durable_sequence=297`, `peers_up=6`,
`transport_gaps=1` unchanged — no synthesized withdrawals). Forward-queue
overload exercised same day via an env-gated synthetic detection injector
(`BONSAI_TEST_DETECTION_COUNT`, off by default): with core unreachable the
queue reached 946/1000, `queue_drops_today=549`, `priority_only_mode=true`,
and graceful shutdown reported `drops=549` — no silent loss, drops are
counted and surfaced. Full-length soak window STARTED 9 Nov 05:48Z —
detached 60 s sampler writing
`docs/merger/evidence/2026-11-09-bn009-soak-full/soak-metrics.csv`
(routing health, sidecar health, registry, container memory); finalize by
reviewing the CSV for gaps/drops/memory growth.
Evidence:
[soak](merger/evidence/2026-11-09-bn009-soak/README.md),
[overload](merger/evidence/2026-11-09-bn009-forward-queue-overload/README.md).

BN-009 graph snapshot slice (9 November): `/api/graph/snapshot` and the DB
purge path were fixed by binding timestamps as Lbug `TimestampNs` parameters
instead of the unsupported DuckDB `TIMESTAMP_NS(...)` call. Verified on a
rebuilt packaged deployment (`bonsai:latest`, image build 485.50 s): snapshot
endpoint 500→200 with real topology (3 devices, 11 BGP sessions), and a
disposable `GnnScore` purge deleted exactly 1 row while real counts stayed
unchanged. Follow-up same day: two defects found during scheduled-trigger
verification were fixed — `_register_schedule` passed
`conflict_policy="replace"` as a string while APScheduler 4.0.0a6 compares
against the `ConflictPolicy` enum (schedule updates silently dropped), and
`SnapshotStore._write_arrow` wrote heterogeneous record batches into a
single-schema Arrow IPC file (corrupt files, `store.health()` also called a
nonexistent method). After fixes, scheduled `graph_snapshot`/`gnn_snapshot`
fired via APScheduler test crons and succeeded with real `MlJobRun` records;
schedules restored to `0 */4 * * *`. Evidence:
[graph snapshot](merger/evidence/2026-11-09-bn009-graph-snapshot/README.md),
[scheduled jobs](merger/evidence/2026-11-09-bn009-scheduled-snapshot-jobs/README.md).

BN-009 lifecycle slice (8 November): `collector_engine.py` heartbeat
`status_json` now reports uptime/cpu/rss/queue-depth/drops/rules-loaded
(stdlib /proc, null-safe); Sidecars UI gained a gRPC-registry panel rendering
`/api/sidecars` with parsed metrics — the registry was previously not
surfaced in the UI. Verified: py_compile, /proc parsing, clean UI build.
Supervision/resource-limit gaps closed by inspection (single combined
sidecar unit + `BONSAI_SIDECAR_MAX_MEM_MB` enforcement). Live heartbeat
exercise done same day (see above); remaining: restart/overload soak
acceptance.
Evidence: [BN-009 lifecycle](merger/evidence/2026-11-08-bn009-lifecycle/README.md).

BN-020 first slice (9 November): UI login page added — `Login.svelte` +
auth gate in `App.svelte` probing `/api/setup/status` (401 → login screen,
HttpOnly `bonsai_token` JWT cookie, no JS token storage). `admin` DB user
created with policy-compliant lab password (literal admin/admin rejected
by password policy). Verified live on rebuilt image: unauth 401 → login
success (role=admin) → authed 200; wrong password rejected; brute-force
Gemini provider fixed same day: Interactions API implemented as primary
(`POST /v1beta/interactions`, store=false, typed function steps) with
generateContent preserved as user-selectable legacy (`api_style` per
provider in vault + Settings UI); provider timeout raised to 120s for
thinking models. Verified live: interactions ok 13.8s, legacy ok 39.5s
on gemini-3.8-flash. BN-014 intake (9 November): change-management machinery inventoried —
change_management.rs (ServiceNow sync, webhook ingest, event/incident
linking), /api/changes + context endpoints, governance.rs,
scenario_pack.rs, approval.rs. Gaps: baseline/candidate capture,
independent invariants, failure rehearsal, episode utility/failure
taxonomy. Proposed slices BN-014a–d; first slice is read-only
baseline/candidate capture on containerlab NOS instances. Evidence:
[intake](merger/evidence/2026-11-09-bn014-intake/README.md).

BN-013b (9 November): adjudication + feedback + real-provider
investigation verified live — test-openapi (gpt-6-luna) ran the full
agentic loop (8 tool calls, 61k tokens, terminal `budget_exhausted` at
$0.10 cap); feedback persists FeedbackRecord, accuracy endpoint
reflects it, adjudicate-prefix persists adjudication + accuracy.
Provider fixes: Gemini Interactions API primary + legacy fallback +
per-provider api_style, 300s timeout, function_result name fix, OpenAI
reasoning_effort=none for tool turns. Gaps: no `complete` status yet
(Gemini 503s + budget cap); Interactions tool-calling needs schema
work. Evidence: [bn013 adjudication](merger/evidence/2026-11-09-bn013-adjudication-feedback/README.md).

GitHub Actions workflows removed — account
billing-locked, every job failed in ~2s and emailed on each push
(evidence: [gemini interactions](merger/evidence/2026-11-09-bn020-gemini-interactions/README.md)). rate-limit lockout observed live (3 attempts/5min → 30min). Same day:
logout added (sidebar Sign out → `POST /api/auth/logout` → 401 after);
post-lockout re-verified — admin login success, cookie-session provider
save `has_api_key:true`, logout→401. Evidence:
[ui login](merger/evidence/2026-11-09-bn020-ui-login/README.md).

BN-013 first slice (9 November): traced one live incident journey on the
packaged deployment. `/api/incidents` returns 2 grouped incidents with
populated correlation chain/blast radius; `POST /api/investigations`
creates a record (HTTP 200) but the AI investigation remains
`status=running` with `tokens_used=0`/`tool_calls=[]` after ~55 s — likely
no AI provider configured or a stalled spawn, and there is no
failure/timeout transition. `completed_at_ns` equals `started_at_ns` while
running; no incident→investigation automation exists. Break points
recorded; next: adjudication/feedback endpoints and investigation
lifecycle states. Evidence:
[incident trace](merger/evidence/2026-11-09-bn013-incident-trace/README.md).
BN-013a (9 November): fixed — investigations created with no AI provider
configured now reach terminal `failed` with
`STOP_REASON=no_ai_provider` and a real `completed_at_ns` instead of
sticking `running` forever; verified live on rebuilt `bonsai:latest`.
This deployment has no AI provider configured, so grounded-RCA quality
remains BN-004-blocked. Evidence:
[investigation lifecycle](merger/evidence/2026-11-09-bn013-investigation-lifecycle/README.md).

BN-014a baseline/candidate capture (9 November): the change webhook and
capture path had three latent defects, all fixed and verified live on the
packaged deployment. lbug's Cypher lexer reserves `DESC`, so the
`$desc` query parameter failed to parse (`expected rule oC_SingleQuery`)
— renamed to `$description` in the ChangeRequest upsert plus the
identically broken SavedQuery create and both Playbook writes.
`load_device_ci_map` selected nonexistent `Device.snow_sys_id`; CI
sys_ids actually live on `EnrichmentProperty` (`key='snow_ci_id'`) and
the map now resolves hostname→address from those. `change_capture`'s
routing-service client used `Authorization: Bearer` while the service
requires the `x-bonsai-routing-token` header (same as the
`/api/routing/v1/*` proxy). Regression test
`change_request_upsert_creates_then_matches` covers upsert create+match
and full `ingest_webhook_change`. Live: webhook ingest 200 (twice —
ON CREATE and ON MATCH), baseline+candidate captures returned real
counts (route `10.10.1.0/24`, 2 peers up on 172.100.207.12), and
`/api/changes/{id}/snapshots` reports `status=diffed` with honest zero
deltas (no routing change was made between phases). Evidence:
[bn014a lbug upsert](merger/evidence/2026-11-09-bn014a-lbug-upsert/README.md).

BN-014b invariant checker (9 November): `POST
/api/changes/{id}/verify-invariants` evaluates four independent
invariants — `capture_complete`, `no_new_peer_downs`, `prefix_retention`,
`expected_prefixes_present` (operator-supplied, `skipped` when absent) —
purely from the persisted baseline/candidate snapshots, persisting
per-device verdicts as `change_invariant_verdict` ConfigItems readable
via `GET /api/changes/{id}/invariants`. Verified live: real snapshot
pair → overall `pass`; bogus expected prefix → overall `fail` with the
missing prefix named; 3 unit tests pass. Evidence:
[bn014b invariants](merger/evidence/2026-11-09-bn014b-invariants/README.md).

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

BN-019 packaged build + boot gate (8 November): `docker/Dockerfile.bonsai`
now builds a working `bonsai:latest` image (added `protobuf-compiler`,
`mold`, `sccache`; `CARGO_PROFILE_RELEASE_*` env pins so cargo-chef deps
are reused — cook 12 s, bin step 9 m vs prior 15–47 min double compiles).
Fixed container HEALTHCHECK (`/readyz`, was auth-gated `/api/readiness`)
and gave `bonsai-routing` a token-authenticated probe. Standalone compose
profile boots healthy; `/health` ok, UI served, all `/api/routing/v1/*`
proxy endpoints 200 via JWT. Volume backup→restore drill passed after
fixing project-prefix resolution in `backup_volumes.sh`/
`restore_volumes.sh`. Install/upgrade, lab browser acceptance, soak and
handoff reconciliation remain. Evidence:
[packaged build](merger/evidence/2026-11-08-bn019-packaged-build/README.md).

BN-019 install + upgrade drill (8 November): `scripts/install.sh` executed
end-to-end in an isolated Compose project (pristine clone @ `ec50410`,
project `bn019inst`, ports remapped to 13000/25051/127.0.0.1:11020 via
`COMPOSE_FILE`/`COMPOSE_PROJECT_NAME` — running deployment and lab
`bonsai-routing` untouched). Install-from-package PASS: build → both
containers healthy → `/health` ok, JWT login, all `/api/routing/v1/*`
200 (`prefix-journey` requires `?prefix=`). Idempotent re-run is a no-op
(byte-identical image, no container recreation). Upgrade-in-place PASS:
forced new image (`sha256:e5c32496c77f`) recreated both containers; graph
volume sentinel, admin login and routing proxy verified post-upgrade.
Not exercised: interactive passphrase prompt, `--source`/`--uninstall`/
`--verify`. Evidence:
[install drill](merger/evidence/2026-11-08-bn019-install/README.md).

BN-019 bonsai-migrate drill (8 November): host-side run of the BN-007 CLI
against real data — 2,890-record genuine FRR lab journal snapshot (all
schema v2; migrate rewrites bytes but preserves semantics — rollback
restores exact SHA), synthetic v1 fixture (2 migrations flagged
`legacy_identity_incomplete`, 1 corrupt line skipped), legacy TOML vendor
alias migration, and corrupted-backup rollback fails closed. Binary is
not shipped in the image; quiesced-copy operator procedure documented.
Evidence: [migrate drill](merger/evidence/2026-11-08-bn019-migrate/README.md).

BN-019 packaged browser acceptance + soak + handoff (8 November): all 12
BN-003 Playwright journeys passed against the packaged `bonsai:latest`
compose deployment fed by genuine FRR BMP — required host-network
`bonsai-routing` (bridge SNAT collapses exporter IPs to the docker
gateway; documented as a packaging finding) and clearing the first-run
onboarding gate via admin API. A control shim drove vtysh +
`docker restart` for the journey hooks. 12-minute packaged soak: 24/24
samples `complete_for_observed_view`, 0 gaps/restarts, stable memory;
shared BN-009/BN-009 soak design recorded, full-length window still open.
Legacy CV5 clean-slate/7-day criteria reconciled to the read-only
prototype boundary (unattended-proof/chaos/governor deferred to
BN-009/014/016). Evidence:
[browser](merger/evidence/2026-11-08-bn019-packaged-browser/README.md),
[soak](merger/evidence/2026-11-08-bn019-soak/README.md),
[handoff](merger/evidence/2026-11-08-bn019-handoff/README.md).

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
