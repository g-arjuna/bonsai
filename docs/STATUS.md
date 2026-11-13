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

BN-014c rehearsal (9 November): ran a controlled failure during an open
change window on the packaged deployment — webhook change
`manual:bn014c-rehearsal-001`, baseline capture, admin-down of frr2's
eBGP session to frr1 via vtysh, candidate capture, invariant
verification, restore, recovery re-capture (pass). Honest gap recorded:
the admin shutdown did not surface in the BMP-fed routing view within
~6 minutes (peer stayed `up`, no new `/events`) — peer-down/withdrawal
propagation needs investigation before claiming detection coverage for
this fault class. Evidence:
[bn014c rehearsal](merger/evidence/2026-11-09-bn014c-rehearsal/README.md).
BMP peer-down investigation same day: rbmp-rib PeerDown handling verified
correct in code; across admin shutdown AND hard `clear bgp` resets,
bonsai-routing logged zero peer events and peers stayed `up` while FRR's
`show bmp` MonSent counter incremented ~2/flap — strong indication FRR
10.6.2 does not emit BMP PeerUp/PeerDown on these transitions, but no
wire capture was possible (no CAP_NET_RAW, no FRR bmp debug cmd).
Documented next steps incl. privileged tcpdump and a hold-time/staleness
fallback. Evidence:
[bmp peerdown investigation](merger/evidence/2026-11-09-bmp-peerdown-investigation/README.md).

Frame-level tracing (10 November): `BONSAI_BMP_TRACE=1` added to
`run_bmp_listener` (commit f4f0363) logging every reconstructed BMP
frame's type/peer header. Three controlled flaps (admin shutdown + hard
clear, 45–60s windows) produced **zero BMP frames of any type** —
no PeerDown (type 2), no PeerUp (type 3), not even route-monitoring —
while the single verified BMP TCP session stayed ESTAB and the trace
itself proved live on reconnect. Peer identity confirmed as the real
type-0 neighbor (not a Loc-RIB virtual peer). Conclusion: the
notification disappears **upstream of Bonsai's decoder** — no frame
reaches the listener. Wire capture remains blocked (no CAP_NET_RAW); a
TCP-proxy or privileged capture container is the recommended next step,
plus FRR-side `debug bgp neighbor-events` logging. Consolidated record:
[bmp peerdown full investigation](merger/evidence/2026-11-10-bmp-peerdown-full-investigation/README.md).

**ROOT CAUSE (10 Nov ~04:05, FIXED ~04:10):** two `bonsai-routing`
instances — a stray `nohup ./target/debug/bonsai-routing` on the host
(PID 2633113, since 8 Nov) owned `172.100.207.1:11019` and received ALL
live BMP; the Docker container only had `127.0.0.1:11019` published,
unreachable from clab nodes, serving stale replayed state to the app.
Stray process stopped; container now publishes
`172.100.207.1:11019:11019`. **PeerDown verified end-to-end**: admin
shutdown → `mtype=2` frames → `peer down LocalSystemClosed2` → API shows
`10.100.1.0 down`/`10.100.1.1 down`; restore → PeerUp → `up`. FRR,
transport, decoder, manager, and API were never defective. New
secondary finding: docker-proxy masquerades clab BMP sources to
`172.20.0.1`, collapsing frr1+frr2 under one speaker identity —
follow-up: host-network mode or speaker disambiguation.

BN-014 completed on live data (10 November): after the deployment fix,
the full change-assurance cycle ran against real BMP state — webhook
`manual:bn014-live-001` → baseline (5 peers, 6 routes under speaker
`172.20.0.1`) → frr2 neighbor admin-down → candidate (4 routes) →
`verify-invariants` returned a **genuine `fail`**
(`no_new_peer_downs`: down peers 0→2) → restore → re-verify `pass`.
First nonzero-diff, failing-invariant result on live data; all four
BN-014 slices (capture, invariants, rehearsal, episode record) now
verified end-to-end. Evidence:
[bn014 live verification](merger/evidence/2026-11-10-bn014-live-verification/README.md).

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

BN-021 source-identity workstream opened (10 November): architecture
review extracted to backlog — canonical device/exporter/transport
identity model, per-protocol attribution rules, P0–P2 stages. P0
listener audit done ([audit](merger/evidence/2026-11-10-bn021-listener-audit/README.md)):
sFlow already uses payload agent_address (reference pattern); BMP
speaker still transport-IP keyed (Initiation sysName parsed but unused);
NetFlow template cache keyed by socket peer; syslog/SNMP use transport
peer_addr. Lab networking fixed via host-mode routing (speakers split
to real IPs). Also landed: change captures + invariant verdicts are now
append-only (`css-…-{phase}-{ts}`, `civ-…-{ts}`) — verified live, 3
verdict records preserved where 1 previously overwrote; pairing selects
newest per phase. Next BN-021 slices: (1) canonical exporter/device identity
contract + inventory bindings; (2) BMP connection-scoped exporter
resolution via Initiation; (3) NetFlow/IPFIX exporter provenance +
protocol-scoped template-cache hardening; (4) syslog/SNMP/OTLP
attribution + trust; (5) production acceptance, alias reconciliation,
monitoring-health semantics. NetFlow template caches already separate
by SocketAddr + source/domain ID — no NAT template collision
demonstrated; confirmed gap is downstream `exporter_address` using
bare transport IP.

BN-021 slice 1 verified live (10 November): peers API now returns
`speaker_sys_name`/`speaker_sys_descr` from BMP Initiation — confirmed
`frr1`/`frr2` per speaker on the packaged deployment. Slice 2
(NetFlow/IPFIX) analyzed: template cache is already collision-safe
(SocketAddr key incl. port); residual gap is downstream
`exporter_address` identity collapse — deferred to the canonical
exporter-identity layer rather than a format-breaking patch.

BN-021b identity-resolver slice (10 November): the canonical identity
contract now has a trusted-binding resolver and durable storage.
`telemetry_identity.rs` gained protocol/scope/collector/session-epoch
fields on the evidence envelope, `TrustedBinding` + `CanonicalDevice`
types, and a stateless `IdentityResolver` (resolved / provisional /
ambiguous / unknown / conflicting; only Resolved may drive
authoritative state). SQLite schema v3 adds `canonical_devices`
(opaque `dev-<uuid>` id stable across address changes) and
`exporter_bindings` (scope+protocol+exporter → device, optional
evidence pins, provenance, revoke-only invalidation) — additive, the
address-keyed `devices` table is untouched. Bindings require an
existing canonical device and reject overlapping claims at
registration; admin-only `/api/identity/*` endpoints provision them
(telemetry cannot mint bindings). `identity_from_bmp_observation`
adapts `ObservationSource`+sysName into the envelope — NOT yet wired
into ingestion; PeerKey/RIB/journal remain transport-IP keyed (that is
BN-021c). Verified: `cargo check --lib` clean; 13/13 tests in
`tests/bn021b_trusted_identity.rs` pass incl. v2→v3 migration on a
populated DB; BN-022 read-boundary suite re-run as regression.

BN-021c exporter-isolation slice (10 November): BMP ingestion, session,
RIB, journal, replay and projection state are now keyed by logical
exporter identity, not transport IP. `SpeakerKey{speaker_addr,
speaker_instance}` partitions `RibManager.speakers`; `PeerKey` gained a
`speaker_instance` discriminator (FNV-1a of the exporter scope, `Copy`
preserved); `ObservationSource`/`BmpMessage` carry
`exporter_instance`/`exporter_id`/`device_id` (serde-defaulted, legacy
records map to instance 0). The listener now requires a BMP Initiation
before creating state, resolves sysName evidence against the trusted
binding store (`bonsai_config.db` beside the journals or
`BONSAI_ROUTING_IDENTITY_DB`), and assigns namespace-encoded `exp:*`
scopes (binding scope+protocol+canonical id — identical canonical
exporter ids in different sites cannot collide) to resolved exporters
(shared across connections = FRR global+VRF association preserved) or
isolated `conn:{id}` scopes otherwise — unresolved/ambiguous/
conflicting evidence fails closed and is never promoted to canonical
device state. Disconnects, transport gaps, EOR clearing, decode
contexts and `conn:*` retirement are all fenced per `SpeakerKey`.
Journals/replay carry the scope end-to-end; `JournalTransportGap`/
`PrefixHistoryItem` gained `exporter_instance`; projection/checkpoint/
graph ids use `ip|instance`.

BN-021c hardening pass (10 November): audit found and fixed five
defects — (1) replay recorded `conn:*` disconnect gaps but never
retired the speakers, diverging from live state (now retires on the
disconnect record and prunes gaps/decode contexts); (2)
`process_for_epoch` fenced only stale Termination/EOR, letting stale
PeerUp/PeerDown/RouteMonitoring mutate newer sessions (now all
payloads fenced; stale EOR still reserves its event sequence for
deterministic replay); (3) `exp:{canonical}` dropped the
(scope, protocol) namespace — latent cross-site collision (now
namespace-encoded); (4) routes/prefix-history/events APIs lacked
exporter-instance selectors (added); (5) `conn:*` retirement was not
gated on the disconnect actually being accepted (now gated).
`tests/bn021c_exporter_isolation.rs` grew to 7 tests: cross-scope
duplicate canonical ids, trusted bindings behind one NAT IP, stale
epoch fencing, journal replay equivalence, and a real-TCP loopback
listener test with two simultaneous exporters. Verified:
`cargo check -p bonsai --all-targets` clean; bn021c 7/7, bn021b 13/13,
bn005_closeout 11/11, rbmp-rib 42/42, rbmp-store 3/3,
bn006_reconnect_projection, r01, r05, ownership and read-boundary
suites pass. Known limitation: `conn:*` speakers with no disconnect
record (SIGKILL) replay as live last-known state — the journal cannot
distinguish them from live connections; deferred to BN-021f alias
reconciliation.

BN-021c review-findings pass + NAT acceptance (10–11 November): the
published-commit review findings were reproduced and dispositioned.
P0-A: `identity_from_bmp_observation` resolved only the default scope
and `exp:{canonical}` dropped the (scope, protocol) namespace — fixed
via `RoutingServiceConfig.identity_scope` and namespace-encoded
`exp:{scope}:{canonical}` instances (default scope keeps legacy
`exp:{canonical}` for journal continuity). P0-B: an invalid BMP frame
broke the read loop without a disconnect reason — fixed; rejections
record distinct `bmp_frame_rejected:*`/transport-error reasons and
still run conn-scope retirement. P0-C: BMP Termination removed the
whole `SpeakerKey` session, so one FRR connection's Termination could
kill sibling state — fixed by deferring termination per sending
connection. P0-D: identical self-reported sysName claims coalesce —
confirmed as a trust assumption (lab-verified: an impostor claiming
`frr-a` merged into `exp:exp-frr-a`); ambiguous/conflicting claims
fail closed into `conn:*` isolation; authenticated ingress identity
remains future work. P1: the routing service now opens the identity
DB read-only (`open_file_readonly`) — no schema/WAL mutation. P2:
FNV speaker-instance hashing documented as a bounded collision risk.
`tests/bn021c_exporter_isolation.rs` is now 12 tests including real-TCP
loopback coverage of invalid-frame lifecycle and Termination sibling
isolation. Live containerlab NAT acceptance is DONE — three FRR
exporters (AS 65001/2/3) behind one iptables SNAT source
(10.240.0.200 → bonsai 10.240.0.20:11019) with a BGP route-server;
scenarios A–G (isolation, withdrawal, PeerDown, disconnect/reconnect,
unknown exporter, conflicting claims, SIGKILL replay) all verified
against the deployed HTTP API — evidence in
`docs/merger/evidence/2026-11-10-bn021c-nat-acceptance/`. BN-005
kill/restart last-known-state semantics preserved: recorded
disconnects replay as gaps, `conn:*` without a disconnect replays as
live last-known (documented limitation).

BN-021c independent PR review + hardening (11 November): a second
review pass over PR #2 (`d19394c`) confirmed and fixed three defect
classes. P0-1: concurrent same-claim connections could merge into one
`exp:*` speaker — the listener now admits at most ONE live connection
per resolved scope; additional simultaneous same-claim connections are
`conn:*`-isolated with attribution preserved. Sequential same-claim
rejoins and claim-matched `device_id` attribution remain spoofable —
P0 production attribution gate stays OPEN pending authenticated
ingress (BN-021f). P0-2: `record_transport_disconnect_for_key`
ignored journal append failures and still retired `conn:*` state —
now returns `TransportDisconnectOutcome` (Stale/NoActiveSession/
Recorded/PersistenceFailed), appends durably BEFORE mutating memory,
and retirement is gated on durable success; failed persistence is
logged and leaves live state identical to replay. P0-3: a connection
that sends BMP Termination no longer accepts further frames; a clean
Termination + EOF no longer leaves a meaningless transport gap
(`NoActiveSession`). P1-2: `exp:{scope}:{canonical}` components are
percent-encoded (separator injection impossible); FNV-64 speaker
partitioning documented honestly as a hash, not the scope string.
`tests/bn021c_exporter_isolation.rs` is now 17 tests. NAT Scenario F
reclassified FAIL/OPEN in the acceptance README; `runtime/` journal
artifacts confirmed local-only (gitignored), `up.sh` now derives paths
and refuses silent lab destruction without `BN021C_RESET=1`. Review
evidence: `docs/merger/evidence/2026-11-11-bn021c-pr2-review/`.
Merge recommendation: Outcome B — mergeable as restricted partial
capability with explicit acceptance of the same-claim limitation.

BN-021f authenticated BMP ingress (11 November): the BN-021c P0
identity-attribution gate is now closed for the default deployment
mode. `bonsai-routing` can terminate mutual TLS on the BMP listener
(`bmp_mtls`; env `BONSAI_ROUTING_BMP_TLS_{CERT,KEY,CLIENT_CA}`); the
verified principal is the SHA-256 of the client-certificate DER,
produced only by the TLS layer — never from BMP payload fields.
Trusted bindings gained `match_ingress_credential` (schema v4,
additive migration); `IdentityResolver::resolve_verified_ingress`
authorizes only credential-pinned bindings, with the existing
evidence/contradiction rules. Verified resolutions get the
provenance-fenced `vexp:*` scope namespace — legacy claim-based `exp:*`
journal records stay readable but can never be joined by new
connections, and replay never upgrades unverified provenance.
`BmpIdentityMode::Verified` is the default; plaintext/unauthenticated
or unbound-credential connections always isolate into `conn:*` with
`device_id = None` (also fixing the BN-021c defect where an isolated
same-claim sibling retained `device_id`). `ClaimCompatible` remains an
explicit, unsafe opt-in. Live NAT lab (`lab/acceptance/bn021c-nat`)
now runs FRR → per-exporter mTLS shim → shared SNAT source → Bonsai:
sequential same-claim impersonation (the exact BN-021c P0 repro) is
rejected, legitimate reconnects keep canonical identity, SIGKILL
replay preserves verified provenance. Verified: bn021f suite 9/9,
bn021c 17/17, bn021b 13/13, bn005_closeout 11/11, rbmp-rib/rbmp-store,
bn006, r01_r04, r05_r06, bn022 — 0 failures; clippy warnings only
plus one pre-existing `never_loop` error in untouched
`src/http_server/device.rs` (reproduced on baseline). Remaining:
production exporter mTLS story; simultaneous old+new credential
authorization during rotation (sequential re-pin documented).
Evidence:
`docs/merger/evidence/2026-11-11-bn021f-authenticated-bmp/`.

BN-021f hardening pass (11 November): the four remaining security/
lifecycle findings from the PR #2 review are resolved. TLS
handshakes no longer serialize the BMP accept loop — they run in
per-connection tasks bounded by a semaphore (default 64 concurrent,
10 s timeout, excess dropped, shutdown-aware). An active-session
registry plus periodic batched binding revalidation
(`BONSAI_ROUTING_BMP_RECHECK_INTERVAL_MS`, default 5000) revokes live
sessions when a binding/credential/device mapping changes: ingestion
stops, the transport closes through the durable disconnect path with
a `bmp_authorization_revoked` gap, last-known routes are preserved,
and unauthorized reconnects isolate to `conn:*`. Multiple sessions
sharing one verified principal are all invalidated by one revocation.
Credential rotation is a documented sequential re-pin that preserves
canonical exporter/device identity and `vexp:*` scope; simultaneous
overlap authorization is deferred (schema v5 candidate). Production
deployment policy now refuses non-loopback plaintext BMP and
claim-compatible mode unless explicit dev overrides are set, and
fully validates mTLS material at startup; compose exposes the TLS env
contract. `VerifiedIngressPrincipal` can no longer be minted outside
the transport layer — private fields, `pub(crate)` attestation
constructor, and deserialization always yields an unattested value
that fails closed. Live NAT R1–R5 verified revocation, multi-session
revocation, rotation, TLS saturation/recovery, and SIGKILL
restart/replay. Verified: bn021f_hardening 9/9,
bn021f_authenticated_ingress 9/9, all prior regression suites 0
failures. Evidence:
`docs/merger/evidence/2026-11-11-bn021f-revocation-hardening/`.

BN-021d NetFlow/IPFIX identity isolation (11 November): the flow
receiver now separates observed transport evidence from provisional
exporter identity and verified ingress. `TelemetryUpdate.target`, the
bus event and the AppFlow node id all carry a collision-resistant
`flowsrc/{collector}/{proto}/{ip}:{port}/{sid|odid}{id}` key, so
NAT-shared exporters no longer collapse downstream; `exporter_address`
remains as bare-IP evidence. The IPFIX template parser is strict
(per-specifier bounds, enterprise PEN preserved, varlen fields marked
non-decodable, RFC 7011 message-length/padding validation) and the
template cache is bounded on every dimension (contexts, domains,
templates, fields, total count, bytes) with TTL expiry, idle eviction,
v9/IPFIX restart clearing and source-port-change isolation; UDP template
withdrawals are ignored per RFC 7011 §8.4. Packet handling is
transactional: every flowset/set boundary is validated before any cache
mutation, so a rejected packet cannot alter template or restart state.
Independent-review hardening: NetFlow graph projection fails closed
for every flow update — no `DeviceAddress` freshness updates, no
`CARRIES_FLOW`, and no `SRC_HOST`/`DST_HOST` endpoint edges — because
no authenticated flow ingress exists yet and even a serialized
`identity_resolution` is self-asserted msgpack evidence, not proof of
a transport-attested resolver; flow-derived utilization/DDoS device
alerts are disabled for the same reason. The AppFlow id is the full
5-tuple including `src_port`. Plain UDP flows stay `provisional` with
`device_id = null` — a matching operator binding or transport address
can never confer canonical device identity. `resolve_flow_identity` is
the extension point for a future authenticated flow ingress. Verified: bn021d suite
15/15 + 3 unit tests, all BN-005/BN-021b/c/f and routing regressions
0 failures, and a
live docker NAT lab (3 exporters SNAT'd to one source) producing 7
distinct provisional AppFlow identities with zero Device attribution
and clean malformed-packet rejection. Evidence:
`docs/merger/evidence/2026-11-11-bn021d-flow-exporter-isolation/`;
lab `lab/acceptance/bn021d-flow-nat/`.

BN-021e sFlow identity isolation (11 November, first adapter slice):
the sFlow receiver no longer treats the self-reported `agent_address`
as exporter identity. Every datagram now yields a provisional
`sflowsrc/{collector}/{transport}/{agent}/{sub}` source key covering
the observed UDP endpoint (IP + source port), collector namespace,
agent claim and sub-agent id; `agent_address`, `agent_sub_id`,
`sflow_sequence` and `sflow_uptime` are preserved as self-reported
evidence and `exporter_address` is the observed transport IP.
`sFlowConfig.collector_id` added (default `default`). The graph writer
fails closed exactly like NetFlow: AppFlow (full 5-tuple id) +
source-scoped `app_flow_event`/`sflow_counters_event` only — no
`DeviceAddress` refresh, `CARRIES_FLOW`, `SRC_HOST`/`DST_HOST`,
canonical `Interface` counter mutation, or flow-derived device alerts
(the now-unused utilization/DDoS emitters were removed). Plain UDP
sFlow stays `provisional` with `device_id = null`; canonical
attribution awaits a transport-attested ingress path (BN-021g).
Verified: `tests/bn021e_sflow_exporter_isolation.rs` 7/7 + 2 unit
tests. Syslog/SNMP/OTLP adapters remain open under BN-021e.

BN-022 containment slice (10 November): ad-hoc graph reads are now
denied-by-default for non-admin principals. `src/graph/explorer.rs`
gained `GraphReadAccess` + `validate_query_for_access`: a positive
node-label allowlist (all schema tables except `ConfigItem`,
`ConfigSnapshot`, `ConfigChange`) enforced before execution; unlabeled
node patterns rejected unless they are bound-variable references;
UNION resets binding scope. Wired into MCP `query_graph` (role-aware),
HTTP Explorer (role extension; missing role ⇒ Standard), NL
`run_cypher` (always Standard — model output is untrusted), and
`verify_graph_state` proposal queries. `/api/config-items` GET filters
and POST rejects credential classes (`user`, `session`, `api_key`,
`jwt_secret`, `jwt_revoked`, `cert_config`) below Admin;
`/api/db/backup` is now Admin-only. Verified: 5 new integration tests
(`tests/bn022_graph_read_boundary.rs`, synthetic markers only), 8 new
explorer/nl_query unit tests, bn005 suites 24/24 — all pass. Storage
migration (JWT secret/verifiers out of the graph) remains open.

BN-021e syslog identity isolation (11 November, second adapter slice):
the syslog receiver no longer treats a message's claimed hostname as
device identity. Every frame now yields a provisional
`syslogsrc/{collector}/{transport}/{peer}[/conn{id}]` source key —
UDP sender endpoint, TCP connection peer plus per-connection session
scope, and collector namespace (`SyslogConfig.collector_id`, default
`default`). The hostname→target match is demoted to an explicit
candidate hypothesis: exactly one configured hostname match →
`unique_hostname_match`; duplicates across sites/tenants →
`ambiguous_hostname_match` with all candidates preserved (no arbitrary
selection, and ambiguous claims no longer steer vendor pattern
selection); none → `unresolved`. Relay vs originator stays separated:
the source key identifies the sender while `reported_hostname`,
`peer_addr`, `transport` and raw bytes remain evidence. The graph
projection fails closed for every syslog update: provisional
`StateChangeEvent`s scoped to the source key (no `REPORTED_BY`, no
`Device` upsert) plus read-only candidate probes against
BgpNeighbor/Interface/BfdSession/OspfNeighbor/IsisAdjacency state.
Fact events are now `syslog_fact_candidate`/`syslog_fact_unresolved`
(the mutating joiners were removed); forged serialized
`identity_resolution`/`device_id`/`verified_ingress` fields cannot
authorize anything. Category events keep `syslog_{category}` names
scoped to the source key. Verified:
`tests/bn021e_syslog_source_identity.rs` 10/10 + 13 lib tests incl.
the 44-fixture corpus. SNMP/OTLP adapters remain open under BN-021e;
canonical attribution awaits BN-021g verified ingress.

BN-021e SNMP trap identity isolation (11 November, third adapter
slice): the SNMP trap receiver no longer treats the transport source
IP as device identity. Every accepted trap now yields a provisional
`snmpsrc/{collector}/udp/{peer_ip}:{peer_port}` source key covering
the observed UDP endpoint and collector namespace
(`SnmpConfig.collector_id`, default `default`). The configured polling
target match is demoted to an explicit candidate hypothesis:
`unique_target_match` / `ambiguous_target_match` (duplicate IPs across
tenants — no arbitrary selection, no vendor steering) /
`unresolved`. All self-reported claims stay separate evidence:
v1 `agent-addr` → `reported_agent_address`, v3 USM engine/context
engine IDs → `reported_engine_id`, `usm_security_name` with
`usm_auth_status: "unverified"`, community presence only
(`community_present`; the string itself and `raw_hex` are redacted
from published values and retained solely in the restricted forensic
archive, which now flushes before publish like the syslog fix).
SNMPv3 message authentication is NOT verified — a recognized security
name is not authentication; encrypted scoped PDUs are rejected with an
explicit parse error. Community/USM filtering now runs BEFORE dedup
(re-keyed to `(trust domain, source key, trap OID)`, bounded), so a
rejected packet cannot suppress a later accepted observation; a
configured allowlist rejects empty/missing/malformed communities.
The graph projection fails closed: provisional `StateChangeEvent`s
scoped to the source key plus read-only candidate probes
(Interface/BgpNeighbor/OspfNeighbor/IsisAdjacency) — no `Device`
upsert, `DeviceAddress` refresh, `REPORTED_BY`, or canonical
Interface/BGP/BFD/ISIS/OSPF/service mutation from trap claims, and no
detection/remediation under trap-claimed identity. Fact events are
now `snmp_fact_candidate`/`snmp_fact_unresolved` (the mutating
`join_snmp_fact` was removed); `correlation_buffer` accepts the new
type names so SNMP facts keep contributing correlation keys. Forged
serialized `identity_resolution`/`device_id`/`verified_ingress`
fields cannot authorize anything. Verified:
`tests/bn021e_snmp_source_identity.rs` 11/11 + lib SNMP parser/target
tests + BN-021/022 regression batch. OTLP remains the open BN-021e
adapter; canonical SNMP attribution awaits BN-021g verified ingress.

BN-021e OTLP source identity isolation (12 November, fourth adapter
slice — completes the BN-021e unauthenticated-adapter set): the OTLP
HTTP/JSON receiver no longer treats `service.name`, `host.name` or
`peer.address` as identity. Every observation is keyed to a
provisional `otlpsrc/{collector}/http/{peer_ip}:{peer_port}` source
identity derived from the observed TCP peer (`ConnectInfo`) —
`X-Forwarded-For`, forwarding headers and payload fields are never
consulted for identity. Reported resource/span attributes are
preserved as evidence; a consistent source-scoped
`otlpapp/{source_key}/{service}` provisional id replaces the old
inconsistent `app:{service}` / `{peer}:{service}` schemes, and no
canonical Application node is created per unverified service name.
The graph projection fails closed: provisional
`StateChangeEvent`s (`otlp_span_event`/`otlp_metric_event`) scoped to
the source key plus read-only exact-match candidate probes against
Device/HostEndpoint — the `address STARTS WITH` prefix joins,
`RUNS_SERVICE`/`HOST_RUNS_SERVICE`/`APP_IMPACTED_BY_NETWORK` edges
and Application health-column writes (cpu_pct, memory_mb,
req_per_sec, error_rate, metric_json) from untrusted claims are
removed. Ingestion budgets bound body size (axum default 2 MiB),
resource groups (32), scopes (32), items per request (4096) and per
scope (1024), attributes scanned (256), string fields (512 chars)
and timestamps (non-positive/>+24h fall back to receive time);
non-finite metric values are dropped, never coerced; drops are
counted via `bonsai_otlp_items_dropped_total`. Evidence is durable
through the parquet archiver and graph `StateChangeEvent`s. OTLP
canonical Application metrics therefore no longer update from
unverified producers — a deliberate coverage change pending
BN-021g verified ingress; HTTP/JSON only (no gRPC claimed).
Verified: `tests/bn021e_otlp_source_identity.rs` 8/8 + BN-021/022
regression batch. All BN-021e unauthenticated adapters (NetFlow/
IPFIX, sFlow, syslog, SNMP, OTLP) are now isolated; BN-021g
production acceptance is next.

PR #11 review resolution + merge (12 November): independent review of
the OTLP slice confirmed and fixed four findings before merge.
B1 — strict numeric parsing: absent/malformed/non-finite
`asDouble`/`asInt`, missing histogram `count` and unsupported value
shapes are rejected and counted (`partialSuccess.rejectedDataPoints`),
never fabricated into `0.0`; a genuine reported `0` stays valid, and
histograms preserve cumulative count/sum/min/max/bucket evidence
without synthesizing mean/rate/rps. B2 — honest HTTP acceptance:
structurally malformed requests get 400, partial acceptance reports
exact per-item `rejectedSpans`/`rejectedDataPoints`, legitimately
empty requests stay 200; drop accounting is per item, not per
truncation event. B3 — `probe_otlp_candidate` dropped `LIMIT 1`:
bounded multi-match collection reports `unresolved`/`ambiguous`/
`candidate` explicitly, always `authority: "unverified"`,
`scope: "unscoped"`. B4 — verified provisional `StateChangeEvent`s
are durable (Kuzu) and bounded by request budgets plus existing
age/count retention; no new storage. Verified: OTLP suite 14/14
(incl. new `candidate_probe_reports_ambiguity`), BN-021/022
regression batch 109/109, `cargo test --lib` 290 pass / 5 fail —
the same 5 pre-existing baseline failures (archive overflow,
site_sync, subscription_status, registry, service_discovery),
tracked under BN-019-B6. Merged as `9b80cd4`.

BN-021g-1 collector trust foundation (12 November): the collector→core
gRPC channel now authenticates and authorizes the FORWARDING
collector without granting it authority over downstream observations.
New `src/collector_auth.rs`: the peer principal is extracted from
tonic `TlsConnectInfo` peer certificates (populated only after the
TLS handshake verified the client cert against the configured client
CA — payload and metadata are never consulted) and hashed as
`sha256:<hex>` of the DER leaf, the same convention as BN-021f.
Authorization reuses `exporter_bindings`: an ACTIVE binding with
`protocol="collector"`, `exporter_id` = claimed `collector_id` and
`match_ingress_credential` = the connection fingerprint must exist —
a valid certificate cannot claim another collector's id, an unbound
CA-issued cert gains nothing, and revocation/rotation take effect
within a bounded 5s decision-cache window (4096-entry cap). Every
relevant RPC is gated by capability: ingest plane per claimed
`collector_id` (telemetry/detection streams, register, heartbeat),
`core-admin` for device/site/credential mutation, query and
remediation writes, `sidecar` for sidecar registration, and
any-active-pinned-binding for the read plane; `get_leader` stays
reachable for provisioned collectors. Durable provenance is recorded
once per (credential, collector) per stream in `audit_log`.
Plaintext deployments remain the explicit legacy dev mode. The
authorized collector's payload claims remain provisional evidence —
this slice changes nothing downstream (BN-021g-2+ owns verified
original-source projection). `server_startup` now passes the shared
registry SQLite store into the gRPC service (previously `None`, which
would have left authz without bindings). Verified: real loopback mTLS
acceptance `tests/bn021g_collector_trust_foundation.rs` 6/6 —
authorized ingest/heartbeat/register/get_leader, collector-id
impersonation, unbound certificate, forged serialized principal,
active revocation, credential rotation and replayed-record denial.
Evidence: `docs/merger/evidence/2026-11-12-bn021g-collector-trust-foundation/`.

BN-021g-1 review hardening (12 November, same branch): independent
review found the first slice authorized the CONNECTION but not the
OBSERVATION — an authenticated collector could submit canonical-path
telemetry for any target, forge inventory metadata, submit arbitrary
DetectionEvents, read global topology, and keep revoked assignment
streams alive. All findings G1–G7 are now closed:

- **G1 observation boundary** — `telemetry_ingest` authorizes each
  update: provisional paths (`streaming/`, `signals/`) stay
  provisional; canonical paths require the target to be an enabled
  registry device assigned to the authenticated collector, and
  vendor/hostname/role/site are rewritten from authoritative inventory.
  Relabeling a flow/OTLP payload onto a YANG path cannot bypass the
  assigned-device check.
- **G2 detection authority** — `DetectionIngest` requires the separate
  `detection-ingest` capability plus verification that the target is
  assigned and every `source_event_ids` reference exists, belongs to
  the same canonical device, and is not provisional evidence. Remote
  `auto_remediate`/`remediation_action` fields are ignored.
- **G3 RPC matrix** — explicit capabilities: `collector` (ingest,
  heartbeat, register stream), `detection-ingest`, `core-admin`
  (mutations, `query`, remediation writes), `sidecar`, `operator-read`
  (global reads incl. topology/sites/event stream); device-scoped
  reads return only assigned devices; `get_leader` needs any pinned
  binding. Exporter-protocol bindings grant no core RPC access.
- **G4 stream fencing** — assignment and event streams revalidate
  authorization per delivery and periodically while idle (revoked
  credentials stop receiving device usernames/passwords within ~5s);
  collector sessions are generation-fenced so a stale stream cannot
  unregister or displace a reconnect.
- **G5 auth policy** — `runtime.auth_mode` = `required` (default;
  missing/unattested principal fails closed) or `development-compat`
  (explicit opt-in, loud startup warning); unknown values fail config
  parsing.
- **G6 grant hardening** — every authorization path requires a
  transport-attested principal; ambiguous bindings fail closed.
- **G7 provenance** — registration audit records the real certificate
  fingerprint; audit-write failures are logged and metric'd rather
  than claimed durable; cache stays bounded (4096, 5s TTL) with
  indexed lookups per miss.

Trust contract documented: assignment authorizes the collector's
acquisition process for the configured device — it is NOT cryptographic
proof of original device identity (BN-021g-2+). Revocation cannot
recall credentials already delivered over a stream; rotate device
credentials when revoking a collector. Verified:
`tests/bn021g_collector_trust_foundation.rs` 19/19 (real mTLS + real
GraphStore graph-writer wiring — cross-collector/unknown-target/
forged-metadata denial, provisional-flow isolation, detection
provenance denial, capability matrix, active-stream revocation,
generation fencing, auth-mode enforcement). `cargo test --lib`
290 pass / 5 fail = unchanged BN-019-B6 baseline. Evidence:
`docs/merger/evidence/2026-11-12-bn021g-collector-trust-foundation/`.

BN-021g-1 final hardening (12 November, same branch): a second
independent review confirmed six residual defects (H1–H6), all now
closed on the same architecture:

- **H1 classified authority** — the `streaming/`/`signals/` path-prefix
  bypass is gone. Every record is decoded, classified
  (`TelemetryEvent::authority`: CanonicalDevice / Provisional /
  Ignored) and authorized by class: canonical writers reachable through
  `streaming/` paths (BMP peer-state/route-monitoring/initiation/
  statistics, BGP-LS node/link/policy, interface summaries,
  config-state leaves) now require the same enabled+assigned-device
  check as YANG paths; provisional classes require a `*src/` source
  key whose collector namespace is rewritten to the authenticated
  collector id (cross-collector evidence-key forgery impossible);
  unclassified paths are ignored. Serialized BMP/BGP-LS payloads via
  generic ingest cannot acquire original-receiver authority.
- **H2 queue acks** — `TelemetryIngestResponse.rejected` added; the
  core consumes the whole stream before responding so
  accepted+rejected==submitted and the collector retires the whole
  batch. Permanent rejections (authz denial, malformed payload,
  protocol major skew) never replay; RPC errors retain everything.
  Verified with the real `drain_queue_to_core` + real core server:
  interleaved rejection, fully-rejected batch, retryable core outage.
- **H3 per-message capability** — `detection-ingest` is revalidated on
  every message of an open stream; authz decisions are stamped with a
  process-local epoch bumped on every binding upsert/revoke, so
  revocation is visible on the next check (no TTL wait) — verified by
  revoking only the detection binding mid-stream.
- **H4 detection provenance** — remote detections now require
  non-empty source-event references that exist, belong to the
  authorized device, and carry server-owned non-provisional
  `source_type`; malformed JSON permanently rejected; stored
  `source_types` marked `remote_detection_ingest`; graph-write
  failures abort as retryable errors.
- **H5 read ambiguity** — `collector_ids_for_credential` no longer
  collapses with `DISTINCT`; scoped reads apply the same ambiguity
  rules as ingest; the store rejects a second collector identity per
  credential.
- **H6 session lifecycle** — assignment-stream cleanup is owned by a
  drop guard (fires on disconnect/cancellation, even before first
  poll); generation check + all cleanup run under one
  `active_collectors` lock so stale cleanup cannot touch a newer
  session. Worst-case idle-stream revocation bound ≈5s poll interval.

Verified: `tests/bn021g_collector_trust_foundation.rs` **25/25** +
3 real-server queue-drain unit tests + focused lib modules 15/15.
Evidence: `docs/merger/evidence/2026-11-12-bn021g-collector-trust-foundation/`.

BN-021g-1 residual round (13 November, same branch): a third
independent review confirmed four residual defects (R1–R4), all now
closed:

- **R1 provisional normalization** — `authorize_provisional_source`
  unconditionally pins effective `source_key`/`exporter_id` to the
  authenticated collector namespace and classified protocol prefix;
  `identity_status`/`authority`/canonical fields are forced
  provisional/unverified regardless of producer claims; missing, null,
  numeric or malformed identity fields can no longer trigger
  `classify()`'s untrusted `exporter_address` fallback; `otlpapp/`
  provisional ids are re-derived under the normalized key; every
  original producer claim is preserved under `claimed_*` evidence
  keys.
- **R2 detection provenance** — `verify_detection_scope` is now a
  positive allowlist of server-established `source_type` values with
  structured detail parsing (missing/null/empty/unknown provenance
  and malformed details permanently rejected; store failures
  retryable). Correction: the earlier
  `sce-no-prov` test never actually submitted a referencing
  detection — it does now, along with unknown/provisional/missing/
  cross-device/malformed/db-failure cases.
- **R3 queue ack contract** — `drain_queue_to_core` refuses to ack
  inconsistent or impossible `accepted + rejected` accounting
  (checked arithmetic) and major-skewed response protocol versions;
  violations retain the batch for replay. Core verifies
  `protocol_version` on every record — major skew is a permanent
  per-record rejection even mid-stream. `InProcessBus::publish`
  returns `PublishOutcome`; router-queue saturation surfaces as a
  retryable drain failure instead of a silent drop.
- **R4 global collector uniqueness** — schema v5 partial unique
  indexes make active `protocol="collector"` `exporter_id` and
  `match_ingress_credential` globally unique; provisioning rejects
  cross-scope/cross-credential conflicts (concurrent provisioning
  serialized, one winner); authorization fails closed on
  pre-existing ambiguous rows for every claimant; rotation keeps a
  single `binding_id`. Authz decisions are epoch-stamped with the
  epoch captured before the store read and the epoch bumps before
  audit persistence, so a pre-revocation lookup cannot cache a stale
  grant and an audit failure cannot strand one. Indexed credential
  lookup proven via `EXPLAIN QUERY PLAN`.

Verified: `tests/bn021g_collector_trust_foundation.rs` **31/31** +
16/16 ingest drain tests + lib 299 pass / 5 documented BN-019-B6
baseline failures + 110/110 BN-021b–f / BN-022 / syslog / snmp /
otlp / sflow regressions. Revocation guarantee is process-local
(epoch) with cross-process changes bounded by the 5s TTL — recorded
under BN-021g-6 production acceptance. Evidence:
`docs/merger/evidence/2026-11-12-bn021g-collector-trust-foundation/`.

BN-015-U2 site-hierarchy preservation (13 November,
`fix/bn015-u2-preserve-site-hierarchy`): `save_managed_device` →
`sync_sites_from_targets` previously built a blank root `SiteRecord`
(name-derived id, empty parent, `kind="unknown"`, zeroed
coords/metadata) and ran it through `upsert_site_record`, whose
`ON MATCH SET` overwrote every field and deleted all `PARENT_OF`
edges — any device save flattened Region→DC→Building→Rack
hierarchies and could merge distinct same-named sites. Device saves
now resolve the free-text `site` claim via `ensure_target_site`:
name-derived id match, else UNIQUE exact-name match (explicit-id and
imported sites), else create-only insert (same shape as the NetBox
backfill — `ON MATCH` touches nothing). Ambiguous display names
produce no link and no duplicate; only the device's `LOCATED_AT`
association is written. Site admin `upsert_site` +
`validate_site_hierarchy` are unchanged and retain full authority.
Verified: 6 new `graph::tests::site_sync_*` regressions + the stale
`site_sync_creates_site_and_located_at_edge` baseline failure fixed
(queried port-full address instead of canonical `Device.address`);
lib 305 pass / 4 documented BN-019-B6 baseline failures (was 5).
Residual: duplicate display names across branches still cannot be
distinguished by a free-text device.site claim — canonical site_id
migration remains BN-015-U3. Evidence:
`docs/merger/evidence/2026-11-13-bn015-u2-site-hierarchy/`.

BN-016-U1 BGP false-edge protection (13 November,
`fix/bn016-u1-bgp-session-identity`): the topology API derived
`peer_device` by scanning every `BGP_SESSION_WITH` edge out of a
device and accepting any onboarded destination; the shared
`resolve_peer_to_device` returned the first `LIMIT 1` row across three
evidence tiers; edges were keyed only by `{source}` per device pair so
multiple sessions overwrote each other and stale resolutions stayed
authoritative forever; BMP's global-session id collided with the gNMI
`{device}:{peer}` `BgpNeighbor` id; and `Topology.svelte` re-resolved
peers client-side. Now `resolve_peer_typed` collects ALL evidence
(Device.address / DeviceAddress→KNOWN_ADDRESS_OF /
EntityIdentity.mgmt_ip) and returns `Resolved` only on a single
consistent claimant — `Unresolved`/`Ambiguous`/`Conflicted` are
explicit non-results. `BGP_SESSION_WITH` edges are keyed per session
`(source, peer_address)`, created only on unique resolution and
transactionally reconciled on every observation; `BgpNeighbor` carries
`source`, `resolved_device`, `resolution_status`, `resolution_reason`
with BMP sessions under `bmp:`-prefixed ids. API exposes
`peer_device`/`source`/`resolution`/`reason`; the UI draws only
backend-resolved peers and badges unresolved/ambiguous/conflict.
Verified: 7 new `graph::tests::bgp_session_*` regressions + graph::
suite 87 pass / 1 documented BN-019-B6 baseline failure. Residual:
globally address-keyed schema fails closed on overlapping private
peer addresses (no cross-tenant isolation claim); gNMI path carries
no VRF discriminator. Evidence:
`docs/merger/evidence/2026-11-13-bn016-u1-bgp-session-identity/`.

BN-016-U1 Phase B P0 closure (13 November, merged PR #14 at
`25ce360e86b79a5d7d0478297bcd9a9e5a833a90`): the remaining correctness
defects are fixed. `reconcile_bgp_session_edge` opened a nested
transaction inside `write_batch`'s active transaction — lbug rejects
nested write transactions, so every BGP/BMP write inside production
batches failed; one transaction owner per write op now, with
dead-transaction tracking and per-update fallback retry (batching
stays enabled, failures surfaced). `BGP_SESSION_WITH` is keyed by
`session_id` plus `peer_type`/`route_distinguisher`/
`network_instance`/`source`; gNMI `network-instance[name=X]` joins
session identity for non-default instances. `resolve_peer_typed` fails
closed with an explicit `Unavailable` outcome when evidence tiers
cannot be examined. Ownership mutations re-resolve affected edges
immediately; a startup migration re-keys or invalidates legacy unkeyed
edges; schema ALTERs distinguish already-present columns from real
failures. Effective persisted peer-as is projected; duplicate BMP
reconcile removed; topology resolution cached per peer per request.
Verified: `graph::tests::bgp_session` 18/18, `graph::` 98 pass / 1
documented BN-019-B6 baseline failure, `bn016_u1_topology_browser`
1/1 (real Chromium, canonical router), ui build clean.
