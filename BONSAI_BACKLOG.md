# Bonsai backlog

Updated: 10 November 2026. **This is the only active product backlog.**
Current state: [docs/STATUS.md](docs/STATUS.md). Latest review: [BN-006 live acceptance](docs/merger/evidence/2026-11-07-bn006-live/README.md). Earlier claim validation:
[BN-005 implementation closeout](docs/merger/BN005_IMPLEMENTATION_CLOSEOUT_2026-10-15.md).

Bonsai owns UI, inventory, configuration and authentication. RustyBMP contributes
protocol, RIB and history libraries, not a second product/control plane. The
current result is an integrated development prototype with a live FRR backend
gate; live FRR browser journeys pass, while actual router VRF browser
acceptance, trained model evaluation and an operational shadow pilot remain.

## Working rules

- Work one bounded item at a time; record its source revision (commit, or base commit plus working-tree hashes), executed command and evidence before marking it verified.
- `Ready` means actionable, `Partial` means code exists but acceptance remains, `Intake` means historical requirements need current-code triage, and `Deferred` means intentionally outside the immediate sequence. No historical checkbox automatically becomes verified work.
- Evidence levels are distinct: unit, in-process integration, live lab, browser end-to-end, and operational pilot. Model forward passes and simulated traces do not prove trained-model performance or live provider results.
- Record completion here and update the state snapshot; do not create another sprint backlog. Historical IDs remain searchable in the crosswalk below.
- Operational device writes remain disabled by the development contract. Any later execution feature needs a separate authorized implementation scope and all execution gates below.

## First actions and release blockers

| ID | Priority / state | Work and acceptance | Dependencies / origin |
|---|---|---|---|
| BN-001 | P0 Complete (bounded executor scope) | Phase-specific process-kill recovery verified at all five durable journal phases (lock acquired, intent, completed, aborted, lock released) via real child-process abort; interrupted intents reconcile through operator-driven `abort_interrupted_execution` with durable abort + scoped lock release; torn-tail/corrupt/vanished journal fail-closed behavior retained. Limits: abort() simulates kill after sync, not physical power loss; reconciliation is manual. Actual device writes stay disabled. | R09, A18; [evidence](docs/merger/evidence/2026-11-08-bn001-crash-recovery/README.md) |
| BN-002 | P1 Partial | Length-prefixed approval signing and shared replay/fresh authorization verified; simulation replay rejected in enforced mode. Added durable signing-key lifecycle: versioned key store beside the journal (atomic write, 0600) with register/rotate-overlap/revoke surviving restart and visible across executor instances; corrupt/vanished store fails closed. Assurance reports now carry optional HMAC signatures verified in enforced mode (`MissingAssuranceSignature`). Remaining: vault/HSM custody, per-role key separation, operational-boundary transport, asymmetric non-repudiation (deferred); no V2 migration needed — canonical encodings unchanged. | R09, A16–A18; [evidence](docs/merger/evidence/2026-11-08-bn002-key-lifecycle/README.md) |
| BN-003 | P1 Partial | Native cookie login and revoked-token replay are exercised. 12 browser journeys pass cleanly; reconnect completeness is synchronized and verified. The fresh live capture has zero exact synthetic-fixture frame matches (capture-provenance.json). BN-005 independently verifies actual routing-process restart and named-volume container replacement. 8 Nov: dedicated `bonsai-bn003-vrf` lab deployed (FRR 10.6.2, kernel-VRF BLUE provisioned pre-start via bootstrap.sh, `rd vpn export 65000:100`, real eBGP route 10.99.1.0/24) — **wire proof PASSED**: genuine BMP Peer Up + Route Monitoring with peer type=1 RD-instance distinguisher 65000:100, captured in `genuine-bmp-stream.bin` ([evidence](docs/merger/evidence/2026-11-08-bn003-genuine-vrf-bmp/README.md)). Same day, phase 2: `bonsai-routing` ingested the genuine stream — RdInstance peer 65000:100 up, `10.99.1.0/24` populated in LocRib + AdjRibIn pre/post-policy views with VRF key and exporter identity preserved; restart gap recorded honestly as `completeness_state: gap` ([evidence](docs/merger/evidence/2026-11-08-bn003-genuine-vrf-bmp/README.md)). Third phase same day: r07-addressing variant lab `bonsai-bn003-r07vrf` deployed — VRF BLUE active, eBGP Established, and a second `bonsai-routing` instance on `172.100.207.1:11019` confirmed genuine `RdInstance 65000:100` peers + `10.99.1.0/24` in all views. The acceptance test's lab topology can now replace `build_rd_bmp_frames` fixture injection. Fourth phase same day: acceptance test rewired to genuine stream — `frr-bmp-r07-vrf.clab.yml` replaces fixture injection, unified multi-session accept manager, both frr1 BMP targets cycled on disconnect/reconnect; **gate PASSED**: 12/12 Playwright journeys + wire proof (14× RD 65000:100 in live capture, zero synthetic frames) ([evidence](docs/merger/evidence/2026-11-08-bn003-acceptance-genuine/README.md)). Genuine multi-instance BMP exposed and fixed a real defect: `run_bmp_listener` opened a new speaker epoch per TCP connection, fencing sibling sessions' EORs as stale (VRF peers stuck `syncing`); concurrent connections now share the speaker epoch — deployed proof `complete_for_observed_view` + journal restart recovery on the live lab. Remaining: BN-003 live-router-VRF browser acceptance is now evidenced; soak/operational pilot stays under BN-014/019. | B08–B14, B17–B19, R05–R07; verified in tests/bn003_live_routing_browser_acceptance.rs |
| BN-004 | P1 Partial | Production provider clients and investigation runtime have mock HTTP/trait integration coverage. Held-out policy benchmark uses ScenarioMockProvider with hardcoded conclusion/token/cost values. Real hosted/local model execution and actual cost/quality comparison remain pending; retain existing wiring tests. | A03–A12, R05–R08; six mock wiring tests pass in the BN-005 closeout; real model evidence still pending |
| BN-005 | P0 Partial (temporary exception) | Strict-mode closure remains regression-tested. User-requested default `audit_only` temporarily relaxes tenant and unsupported-VRF denials while retaining authentication, roles, speaker restrictions and supported RD filters. Admin policy API restores `strict`; no automatic expiry. Ownership mappings persist but do not enforce isolation. Default-mode tenant isolation is deliberately not certified. | R06, B11–B13, D4-3; [policy contract](docs/architecture/ownership_mapping.md); prior strict closeout is historical evidence |
| BN-006 | P1 Complete (bounded live profile) | Verified EOS 4.36.0F BMP/RIB lifecycle and offline startup-policy change across default and BLUE/RD 65001:100 IPv4 views; actual cEOS software-FIB forwarding/withdrawal probes; live SR Linux 26.3.1 native collection through operator API and MCP. Scope and limits are in the [support/evidence report](docs/merger/evidence/2026-11-07-bn006-live/README.md). | A13, B07, B17–B18, R07; live evidence under `docs/merger/evidence/2026-11-07-bn006-live/` |
| BN-007 | P1 Partial | Intake triage done: canonical path is `routing_service`+rbmp crates only; legacy `src/streaming/bmp.rs`+`BgpRibEntry` graph writers identified as duplicate-authority candidates (retire blocked on projection parity). Fixed: settings-API authority bypass, dead `[bmp]` config block, missing package license + CDLA allow-list (`cargo deny` passes), `bonsai-migrate` operator CLI (backup/migrate/rollback SHA-verified), `authority` typo warning, LICENSE/NOTICE attribution. Journal backups confirmed covered via `bonsai_graph` volumes. Imported-crate gate: 135 tests pass. Leftovers disposed: rbmp crates versioned to 0.2.0 in lockstep, `duckdb-store` kept optional-disabled pending EOR parity (documented), legacy receiver retirement still blocked on projection parity, CI/install gates deferred to BN-019. | B00–B20, R10, D3-1/10, D4-11/19/22/23; [audit](docs/merger/evidence/2026-11-08-bn007-audit/README.md) |

**Next planned item: BN-019 operational/release proof (install/upgrade, packaged build, lab browser acceptance, backups/recovery, soak) — UI warning and link/provenance debt now cleared.** BN-009/BN-013 intake triage also remains doable without external deps. BN-003/004 need live lab or real provider credentials; BN-005 awaits an explicit strict-mode decision.

BN-005 has an explicit temporary ownership-access exception. Its strict profile was verified in the [closeout report](docs/merger/BN005_IMPLEMENTATION_CLOSEOUT_2026-10-15.md).
Its [evidence manifest](docs/merger/evidence/2026-10-15-bn005-implementation/results.json)
records the base commit and exact working-tree hashes; implementation remains uncommitted.
BN-001–004 retain their separate acceptance gaps. Operational device writes stay disabled.

BN-006 closeout (7 November): in addition to the reconnect replay fix documented
in the [investigation](docs/merger/BN006_RECONNECT_INVESTIGATION_2026-11-07.md), the bounded live profile completed lifecycle, route-policy,
VRF/RD isolation, software-FIB, and SR Linux native-collection checks. It does not claim
physical ASIC forwarding, scheduled polling, broader platform/release support, or optional
DuckDB retirement parity. See [evidence and limits](docs/merger/evidence/2026-11-07-bn006-live/README.md).
BN-007 intake: the optional legacy DuckDB writer stores EOR events but does not
materialize their retired route identities; reconcile its schema and lifecycle
before claiming equivalent history or enabling it as an authority.

## Unified product queue

| ID | Priority / state | Work and acceptance | Dependencies / origin |
|---|---|---|---|
| BN-008 | P1 Partial | Unify observation provenance across BMP, gNMI, syslog, flows, CLI and config changes: source/entity/tenant, event versus observation time, cursor/epoch, quality/staleness and coverage. Prove restart-safe sidecar/event delivery and graph projection; distinguish absent evidence from zero/healthy. | A04–A08, D3-8/11/12/14/15, D4-5/6/10/12; BN-003/005 |
| BN-009 | P1 Partial | Sidecar/BonPy operational lifecycle: one event/DB contract, supervision/watchdog, health and backlog visibility, export catalog/schedules/quality, tracked jobs/retry/cancel, observable resource limits and one Bonsai UI entry. Reconcile existing functionality before building. Acceptance is a restart/overload soak with no silent data loss. 8 Nov intake reconciliation: registry/heartbeat/stale-lost, health gate, `/api/ml/{exports,schedules,jobs}` + cancel, job_engine memory shedding, Sidecars UI and rules-sidecar systemd unit all exist — build extends, not recreates ([intake](docs/merger/evidence/2026-11-08-bn009-intake/README.md)). Same day: `status_json` now carries cpu/rss/queue/uptime metrics end-to-end and `/api/sidecars` registry renders in the Sidecars UI ([evidence](docs/merger/evidence/2026-11-08-bn009-lifecycle/README.md)). Remaining: restart/overload soak acceptance with no silent data loss; live heartbeat exercised 8 Nov against packaged deployment — register/heartbeat/`status_json` metrics healthy, SIGTERM → stale (45s) → lost (120s) per registry thresholds; new gap found: sidecar ML/export jobs get unauthenticated 401s against the JWT-protected API (no credential path), and `last_heartbeat_at` renders null ([evidence](docs/merger/evidence/2026-11-08-bn009-live-heartbeat/README.md)). 9 Nov: `BONSAI_API_TOKEN` (`bsk_` key) auth wired through `bonsai_ml/api_auth.py` into all sidecar HTTP call sites + `BonsaiClient` env fallback; verified live on packaged deployment — 0 × 401 (was 21), scheduler jobs succeed with real `MlJobRun` records, schedules/exports writes land; `export_job` `quality_json` 422 bug fixed; `last_heartbeat_at` null confirmed a field-name expectation, not an API defect; new findings: `/api/graph/snapshot` 500 (missing DuckDB `TIMESTAMP_NS`, pre-existing) and pandas absent → empty parquet exports ([evidence](docs/merger/evidence/2026-11-09-bn009-api-auth/README.md)). Same day: restart/overload slice PASS — sidecar SIGKILL → stale (~61s) → lost (~125s) → re-register healthy with 0 × 401; app container restart → `reregister_required` re-registration; routing container restart → journal replay identical RIB (`durable_sequence=297`, peers_up=6, gaps unchanged); forward-queue overload exercised same day via env-gated synthetic detection injector (no device writes): queue_depth 946/1000, 549 drops, priority_only_mode on, graceful shutdown flushed pending — see [overload evidence](docs/merger/evidence/2026-11-09-bn009-forward-queue-overload/README.md); full-length soak window STARTED 9 Nov 05:48Z — detached 60 s sampler writing `docs/merger/evidence/2026-11-09-bn009-soak-full/soak-metrics.csv` (routing health, sidecar health, registry, container memory); finalize by reviewing CSV for gaps/drops/memory growth ([setup](docs/merger/evidence/2026-11-09-bn009-soak-full/README.md)). Same day: graph snapshot/purge `TIMESTAMP_NS` fix verified on packaged rebuild — `/api/graph/snapshot` 500→200 with real topology, disposable `GnnScore` purge returned `deleted_count=1` with real rows untouched, and `graph_snapshot`/`gnn_snapshot` job functions succeeded via direct `_run_job` with real `MlJobRun` records ([evidence](docs/merger/evidence/2026-11-09-bn009-graph-snapshot/README.md)). Same day follow-up: both blockers fixed — `_register_schedule` now passes `ConflictPolicy.replace` (enum, not string) so API schedule updates replace persisted APScheduler triggers, `_take_graph_snapshot` uses `get_buffer_health()`, and `SnapshotStore._write_arrow` writes a single-schema JSON payload (`bonsai_hetero_snapshot_v2`) instead of invalid multi-schema batches; scheduled `graph_snapshot`/`gnn_snapshot` then fired via APScheduler `* * * * *` test crons and succeeded with real `MlJobRun` records, schedules restored to `0 */4 * * *` ([evidence](docs/merger/evidence/2026-11-09-bn009-scheduled-snapshot-jobs/README.md)). | EV1-2/4/5/6/9, D3-13/14, D4-9/13/21 |
| BN-010 | P2 Partial | Train and evaluate STGNN on versioned temporal graph datasets with chronological episode-level train/validation/test separation, clean-validation calibration, independent held-out faults, no-topology/no-temporal/cold-start ablations, event-level delay/false-alert metrics and model/checkpoint provenance. Integrate live graph telemetry and bounded continuous retraining only after offline acceptance; keep statistical fallback. | A00–A02/A15, R08, EV1-1/8, D3-9; BN-008/009 |
| BN-011 | P2 Intake | Integrate semantic/spectral embeddings and PCA display with live graph snapshots. Version feature schema/model, preserve stable entity IDs, fit transforms on training data, bound caches/jobs, expose stale/missing vectors. Validate text/syslog/config/CLI and topology embeddings independently; PCA is visualization, not evidence of anomaly quality. | EV1-3, D3-9, BN-008–010 |
| BN-012 | P2 Partial | Run a real licensed forecasting challenger against statistical baselines on held-out chronology with clean validation thresholds, measured inference/resource cost and fault/maintenance covariates. Record exact model/weights/license and promotion criteria; do not treat the current mean-shift surrogate as Chronos inference. | A14, R08; BN-009 |
| BN-013 | P1 Partial | Unified incident/evidence/operator workflow: grounded RCA, timelines, recommendation versus independently adjudicated correctness, feedback provenance, DB-controlled rules/playbooks and regression scenarios. Confirm current UI/rule support before replacing it. 8 Nov intake triage: Incidents/Investigations/Audit UI, investigation_runtime/trigger, playbook.rs, playbooks/library YAML rules, prefix-investigator diagnostics + adjudicate endpoint inventoried; gaps are grounded-RCA proof, adjudication loop, feedback provenance, DB rule control, incident→scenario regression gate ([intake](docs/merger/evidence/2026-11-08-bn013-intake/README.md)). First slice DONE 9 Nov: live trace — `/api/incidents` returns 2 grouped incidents with correlation chain/blast radius populated; `POST /api/investigations` creates a record (200) but the AI investigation stays `status=running` with `tokens_used=0`/`tool_calls=[]` after ~55 s (no provider configured or stalled spawn — break point), `completed_at_ns==started_at_ns` while running, and there is no incident→investigation automation (operator must POST manually) ([evidence](docs/merger/evidence/2026-11-09-bn013-incident-trace/README.md)). BN-013a DONE 9 Nov: no-provider investigations now reach terminal `failed` with `STOP_REASON=no_ai_provider` + real `completed_at_ns` (was stuck `running` forever); verified live on rebuilt image — this deployment has no AI provider configured, so grounded-RCA quality stays BN-004-blocked ([evidence](docs/merger/evidence/2026-11-09-bn013-investigation-lifecycle/README.md)). BN-013b DONE 9 Nov: real-provider investigation ran the full agentic loop on test-openapi (8 tool calls, 61k tokens, terminal `budget_exhausted` at $0.10 cap — cost enforcement verified); feedback endpoint persists FeedbackRecord, `/api/investigations/accuracy` reflects it, `/api/routing/v1/adjudicate-prefix` persists adjudication + accuracy summary. Provider fixes landed: Gemini Interactions API primary + legacy fallback + per-provider api_style, 300s timeout, function_result name fix, OpenAI reasoning_effort=none for tool turns. Honest gaps: no `complete` status yet (Gemini 503s + budget cap), Interactions tool-calling needs schema work ([evidence](docs/merger/evidence/2026-11-09-bn013-adjudication-feedback/README.md)). Next: BN-013c playbook authority reconciliation. | A09–A11, D3-5/6/16, D4-4/8/15, EV1-7; BN-003/004/008 |
| BN-014 | P1 Partial | Real lab change assurance and read-only shadow pilot: collect baseline/candidate state from dedicated NOS instances, independent invariants and provenance, failures/recovery rehearsal and durable operator audit; report utility and failure taxonomy over real episodes. No autonomous live remediation acceptance until durable/signature gates and explicit authorization. | 9 Nov intake triage: change_management.rs (ServiceNow sync, webhook ingest, event/incident linking), /api/changes + context endpoints, governance.rs, scenario_pack.rs, approval.rs inventoried; gaps are baseline/candidate capture, independent invariants, rehearsal, episode report ([intake](docs/merger/evidence/2026-11-09-bn014-intake/README.md)). Proposed slices: BN-014a baseline/candidate capture → BN-014b invariants → BN-014c rehearsal → BN-014d episode report. A16–A20, R09, D4-16; BN-001/002/006/013. 9 Nov: BN-014a DONE live — fixed three latent defects on the webhook/capture path (lbug reserved-word param `$desc`→`$description` in ChangeRequest upsert + SavedQuery/Playbook writes; `load_device_ci_map` queried nonexistent `Device.snow_sys_id`, now reads `EnrichmentProperty snow_ci_id`; capture used Bearer auth, routing service requires `x-bonsai-routing-token` header). Verified on packaged deployment: webhook ingest 200 (create+match), baseline+candidate captures returned real per-device route/peer counts (1 route `10.10.1.0/24`, 2 peers up on 172.100.207.12), snapshots diff `status=diffed` with honest zero deltas. Regression test `change_request_upsert_creates_then_matches` added. ([evidence](docs/merger/evidence/2026-11-09-bn014a-lbug-upsert/README.md)). 9 Nov: BN-014b DONE — `POST /api/changes/{id}/verify-invariants` + `GET /api/changes/{id}/invariants` evaluate four independent invariants (capture_complete, no_new_peer_downs, prefix_retention, expected_prefixes_present) purely from persisted snapshots; verdicts persisted as `change_invariant_verdict` ConfigItems. Verified live both directions (pass on real pair, fail on bogus expected prefix) + 3 unit tests. ([evidence](docs/merger/evidence/2026-11-09-bn014b-invariants/README.md)). 9 Nov: BN-014c rehearsal executed — admin-down injected on clab frr2 (172.100.207.12) during open change window; capture+invariant+recovery loop verified live, BUT honest gap found: admin neighbor shutdown did not surface in the BMP-fed view within ~6 min (peer stayed `up`, no new events) — peer-down propagation needs investigation before claiming detection coverage. ([evidence](docs/merger/evidence/2026-11-09-bn014c-rehearsal/README.md)). 10 Nov: gap root-caused and BN-014 verified on LIVE data — stray host `./target/debug/bonsai-routing` (PID 2633113) owned `172.100.207.1:11019` receiving all BMP while the container served stale replay; killed it, published container BMP on the gateway IP. PeerDown proven end-to-end (`mtype=2` → `peer down LocalSystemClosed2` → API `down`). Full BN-014 cycle re-run live (`manual:bn014-live-001`, speaker 172.20.0.1): baseline 5 peers/6 routes → frr2 neighbor shutdown → candidate 4 routes + **genuine invariant FAIL** (`no_new_peer_downs`: 0→2 down peers) → restore → re-verify **pass**. Secondary finding: docker-proxy collapses clab speakers to `172.20.0.1` (follow-up: host-net or speaker disambiguation). ([evidence](docs/merger/evidence/2026-11-10-bn014-live-verification/README.md), [root cause](docs/merger/evidence/2026-11-10-bmp-peerdown-full-investigation/README.md)) |
| BN-015 | P2 Partial | Onboarding/inventory/security/config lifecycle: structured onboarding and actual gNMI E2E, credentials/SSH/vault/rotation, RBAC/LDAP, persisted device/exporter+RD ownership mappings and admin audit API now implemented without new access denials ([rollout contract](docs/architecture/ownership_mapping.md)); temporary compatibility access now enabled by default at user request; isolated query filtering/enforcement, trusted import provenance and assignment history remain, NetBox/rack enrichment, bounded read-only CLI adapters, config/UI authority and database backup/restore. Reconcile implemented legacy tasks through one operator journey. | D3-2/3/4/6, D4-3/7/13/14/17/18; BN-005/007 |
| BN-016 | P2 Intake | Complete multi-source telemetry and graph quality: vendor syslog/SNMP/MIB, suppression, flows/hosts/OTLP/TSDB, redundancy, optics/power/temperature, endpoint/service topology. Validate actual captures, normalization, sample-rate units, graph completeness and resource pressure per selected source. | D3-8/11/12/15, D4-1/2/5/6/10/12/20, Ubuntu DV2 carryovers; BN-008 |
| BN-017 | P3 Deferred | DDoS observation-only slice: corroborated signal/graph/rule features, incident timeline, reproducible benign/fault scenarios and false-positive evaluation. Cloud sink/BGP/RTBH response is separately deferred behind BN-001/002/014 and authorized execution scope. | DS-1–DS-8, especially DS-4; BN-008/013/016 |
| BN-018 | P3 Deferred | Routing security extensions: live RPKI ROA validation/cache lifecycle (fresh/stale/unavailable, RTR failure/reconnect), independent origin-validation cases and cache provenance. Keep RPKI origin validation distinct from BGPsec path-signature verification; BGPsec stays explicitly unverified until genuine wire/signature/trust-chain support and independent tests. | RustyBMP security roadmap; BN-003/007 |
| BN-020 | P1 Partial | UI authentication: the SPA has no login page and no token handling — every `/api/*` call from the browser 401s under `require_auth` (observed live 9 Nov: Settings → LLM provider save failed with no way to authenticate). Add a username/password login page that POSTs `/api/auth/login` (JWT cookie `bonsai_token`), gate the app shell on an auth probe, surface role/401 states honestly, and cover logout. 9 Nov FIRST SLICE DONE: `Login.svelte` + auth gate in `App.svelte` (probe `/api/setup/status`, 401→login, HttpOnly `bonsai_token` cookie, no JS token storage); `admin` DB user created (policy-compliant lab password — literal admin/admin rejected by 16-char policy); verified live on rebuilt image — unauth 401, login success role=admin, authed 200, wrong-pass rejected, rate-limit lockout observed live; logout UI added same day (sidebar Sign out → `POST /api/auth/logout` → 401 after); post-lockout re-verified — admin login success, cookie-session provider save `has_api_key:true`, logout→401 ([evidence](docs/merger/evidence/2026-11-09-bn020-ui-login/README.md)). | D4-7 auth/RBAC; BN-003 browser acceptance, BN-019 packaged browser |
| BN-021 | P0 Partial — production attribution gate | Telemetry source identity/ingress correctness: transport IP is evidence, not authoritative device identity. 10 Nov: listener audit and host-network BMP source-IP preservation LAB VERIFIED (temporary Compose override; packaged production configuration OPEN). BMP Initiation sysName/sysDescr surfaced in speakers/peers APIs (live verified), provisional `exporter_id` from untrusted sysName and `session_epoch` exposed, peers `?exporter_id=` filter added; inventory-backed device binding and RIB/PeerKey isolation from translated source IP OPEN. NetFlow/IPFIX template caches now separated by (SocketAddr, protocol version, source/domain ID, template ID) with 4096 outer-context cap; flow payload includes a provisional IP-derived `exporter_id`. Downstream `exporter_address` and `TelemetryUpdate.target` remain bare `peer.ip()`, so NAT identity collision is unresolved; cap is not template expiry/restart handling. P0 acceptance: two simultaneous exporters behind one NAT address cannot merge authoritative RIB or graph/flow identity; unknown/conflicting sources are isolated. Remaining detailed slices BN-021a–f below. | BN-014/015/016/019; [listener audit](docs/merger/evidence/2026-11-10-bn021-listener-audit/README.md), [host-network lab proof](docs/merger/evidence/2026-11-10-bn021-source-identity/README.md); code `src/routing_service/mod.rs` and `src/streaming/netflow.rs` |
| BN-022 | P0 Partial — read boundary contained; storage migration OPEN | Security finding confirmed by code inspection: auth signing secret, password verifiers and hashed API-key records are persisted in graph `ConfigItem` records; existing read-only Cypher validated mutation safety but did not restrict access to credential rows. 10 Nov CONTAINMENT SLICE DONE: centralized `GraphReadAccess` policy in `src/graph/explorer.rs` — positive node-label allowlist enforced before execution for every non-admin principal (Viewer/Operator/ApiReadonly/scoped keys/unauthenticated); `ConfigItem`, `ConfigSnapshot`, `ConfigChange` denied; unlabeled node patterns rejected unless bound-var references; UNION scope resets. Applied at MCP `query_graph` (role-threaded), HTTP Explorer (role extension, missing role ⇒ Standard), NL `run_cypher` (always Standard — LLM output is untrusted), and `verify_graph_state` proposal queries. `/api/config-items` GET filters and POST rejects sensitive classes (`user`, `session`, `api_key`, `jwt_secret`, `jwt_revoked`, `cert_config`) below Admin; `/api/db/backup` now Admin-only (full-DB snapshot contains credentials). Regression: `tests/bn022_graph_read_boundary.rs` (5 tests, synthetic markers only) + explorer/nl_query unit tests — all pass; bn005 suites 24/24 pass. OPEN: protected-storage migration (JWT secret out of graph, verifiers to restricted storage, race-safe init, migration/rollback, rotation/revocation, restart behavior), process-wide AI test `set_var`, secure cookie flags for trusted TLS, Swagger self-hosting, CSPRNG JWT key; property-level allowlist and ConfigChange export review remain residual risks. Code finding verified, exploit NOT exercised against real credentials. | BN-005/015/020; `src/graph/explorer.rs`, `src/mcp_server.rs`, `src/http_server/{auth,observability,nl_query,remediation}.rs`, `tests/bn022_graph_read_boundary.rs` |
| BN-019 | P1 Partial | Operational/release proof: install and upgrade, reproducible packaged build, dedicated lab browser acceptance, backups/recovery, collector limits/observability and sidecar soak. Reconcile legacy clean-slate and seven-day handoff criteria to the read-only prototype boundary. UI accessibility/bundle warnings resolved 8 Nov (zero-warning build, route-level code splitting; [evidence](docs/merger/evidence/2026-11-08-bn019-ui-warnings/README.md)); documentation link/provenance debt reviewed same day — 4 dead live-doc links fixed, 0 remaining ([evidence](docs/merger/evidence/2026-11-08-bn019-link-debt/README.md)). Remaining: the install/lab/soak gates. 8 Nov intake triage: Dockerfile/compose stack, backup/restore scripts, `bonsai-migrate`, `just verify-release` inventoried — gaps are executed install/upgrade, image build+boot, restore drill, soak (shared with BN-009); first slice proposed: packaged build + boot gate ([intake](docs/merger/evidence/2026-11-08-bn019-intake/README.md)). 8 Nov: packaged build + boot gate EXECUTED — image builds (Dockerfile fixed: protoc/mold/sccache, `CARGO_PROFILE_RELEASE_*` env so cook deps are reused: cook 12 s + bins 9 m vs prior 15–47 min double compiles), standalone profile boots both containers healthy, `/health` ok + all `/api/routing/v1/*` proxy 200 via JWT; container HEALTHCHECK fixed (`/readyz`, token probe for bonsai-routing). Backup→restore volume drill passed (project-prefix resolution fixed in both scripts). All planned BN-019 gates now have executed evidence; only the full-length soak window remains open (shared with BN-009). ([evidence](docs/merger/evidence/2026-11-08-bn019-packaged-build/README.md)). Same day: install-from-package + upgrade-in-place drill PASS — `scripts/install.sh` run end-to-end in an isolated Compose project (fresh clone @ `ec50410`, remapped ports, generated `.env`): image build → both containers healthy → JWT + all routing proxy endpoints 200; idempotent re-run is a no-op; forced-image upgrade recreates containers with graph-volume sentinel, admin login and routing API verified post-upgrade ([evidence](docs/merger/evidence/2026-11-08-bn019-install/README.md)). Same day: `bonsai-migrate` live drill PASS host-side — backup/migrate-jsonl/migrate-toml/rollback SHA-verified on a real lab FRR journal snapshot (2,890 v2 records, semantically-identical rewrite noted), synthetic v1 fixture (2 migrations, 2 `legacy_identity_incomplete` flags, 1 corrupt line skipped), legacy TOML vendor aliases, and corrupted-backup rollback fails closed; binary stays out of the image, quiesced-copy procedure documented ([evidence](docs/merger/evidence/2026-11-08-bn019-migrate/README.md)). Same day: packaged-deployment browser acceptance PASS — all 12 BN-003 Playwright journeys against `bonsai:latest` compose deployment with genuine FRR BMP (host-network routing for real exporter IPs; two defects handled: bridge SNAT speaker misattribution documented, first-run onboarding gate cleared via admin API) ([evidence](docs/merger/evidence/2026-11-08-bn019-packaged-browser/README.md)). Bounded 12-min packaged soak PASS (24/24 samples complete, 0 gaps/restarts, stable memory) + shared BN-009/BN-019 soak design; full-length soak still open ([evidence](docs/merger/evidence/2026-11-08-bn019-soak/README.md)). Legacy clean-slate/7-day handoff criteria reconciled to read-only prototype boundary — install/upgrade/backup/browser/migrate gates done; unattended-proof/chaos/governor criteria explicitly deferred to BN-009/014/016 ([evidence](docs/merger/evidence/2026-11-08-bn019-handoff/README.md)). | B16–B20, A20, D3-1/7/10, D4-19/21/22/23; BN-003/005–009 |

### BN-021 — Telemetry source identity and ingress correctness

**BN-021a — Ingress transport correctness** — LAB VERIFIED; production packaging OPEN.
- Inventory listeners, bind addresses, Docker network paths and NAT behavior.
- Preserve original source addresses where technically possible.
- Validate the host-network or directly routed ingress deployment.
- Restrict management APIs independently from telemetry listeners.
- Acceptance: two BMP exporters remain distinct across restart/redeploy.

**BN-021b — Canonical identity contract** — PARTIAL (resolver + persistence done; ingestion not wired).
- Done (10 Nov): `src/telemetry_identity.rs` extended — envelope now carries
  protocol, scope, collector_id and session_epoch alongside transport
  endpoint, provisional exporter claim, reported identity and observation
  domain. New `TrustedBinding` (scope+protocol+canonical exporter_id →
  canonical device_id, optional exact-match evidence pins, provenance,
  active/revoked), `CanonicalDevice` (opaque `dev-<uuid>` id surviving
  address changes), and stateless `IdentityResolver` returning
  resolved/provisional/ambiguous/unknown/conflicting with raw evidence
  preserved. SQLite schema v3 adds `canonical_devices` +
  `exporter_bindings` (additive; `devices` table and address-keyed
  consumers untouched); bindings require an existing canonical device,
  reject overlapping/conflicting claims at registration, are idempotent
  per binding_id, and revoke explicitly. Admin-only HTTP API
  `/api/identity/{devices,bindings,bindings/{id}/revoke,resolve}` writes
  only `operator_provisioned` provenance — telemetry cannot mint bindings.
  Narrow BMP adapter `identity_from_bmp_observation` maps
  `ObservationSource`+Initiation sysName into the envelope (not wired
  into ingestion). 13 integration tests cover NAT exporters, overlapping
  site addresses, shared sysName, missing/conflicting bindings,
  registration rejection, reopen persistence, ODID reuse, provenance and
  the v2→v3 migration (`tests/bn021b_trusted_identity.rs`).
- Open: wire resolution into live ingestion (BMP/NetFlow/syslog adapters),
  PeerKey/RIB/journal re-keying (BN-021c), inventory import path,
  device_id selector in routing APIs, alias reconciliation.

**BN-021c — BMP exporter identity** — PARTIAL (core isolation done; live-containerlab + alias reconciliation open).
- Done: Initiation sysName/sysDescr exposed in speakers/peers APIs;
  provisional `exporter_id` and `session_epoch` surfaced; `?exporter_id=`
  peer selector implemented. These are reported/context identities, not trusted
  inventory device bindings.
- Done (10 Nov): trusted resolver wired into the BMP listener — every
  connection must open with Initiation, then resolves against
  `bonsai_config.db` bindings (`BONSAI_ROUTING_IDENTITY_DB` override).
  Resolved exporters share a stable namespace-encoded `exp:*` speaker
  scope (binding scope+protocol+canonical id, so identical canonical
  exporter ids in different sites cannot collide; multi-instance
  association preserved); everything else gets an isolated `conn:{id}`
  scope. `SpeakerKey`/`PeerKey.speaker_instance` partition speakers,
  peers, RIBs, decode contexts, transport gaps, journals, replay and
  projection/graph ids; `conn:*` state retires on disconnect while
  `exp:*` keeps last-known routes under the gap.
- Done (10 Nov, hardening pass): stale-epoch fencing now covers ALL
  payload types (PeerUp/PeerDown/RouteMonitoring, not just
  Termination/EOR) so a stale connection cannot mutate a newer
  session; replay retires `conn:*` speakers on their disconnect
  records and prunes their gaps/decode contexts so live and replay
  agree; disconnect retirement is gated on the gap record actually
  being accepted; routes/prefix-history/events APIs accept
  `exporter_instance` selectors so NAT-shared exporters stay
  individually queryable. Known limitation: `conn:*` speakers with no
  disconnect record (SIGKILL) replay as live last-known state — the
  journal cannot distinguish them from live connections; documented,
  deferred to alias reconciliation.
  `tests/bn021c_exporter_isolation.rs` proves NAT'd exporters never
  merge (7/7) plus binding resolution, stale-epoch fencing, live TCP
  NAT isolation and replay/live equivalence.
- Done (11 Nov, review-findings pass + NAT acceptance): P0-A scope
  namespace encoded via configurable `identity_scope`
  (`exp:{scope}:{canonical}`, default scope keeps legacy
  `exp:{canonical}`); P0-B invalid BMP frames record distinct
  `bmp_frame_rejected:*` disconnects and still retire conn scopes;
  P0-C BMP Termination is per-connection deferred so siblings are
  unaffected; P0-D ambiguous claims fail closed into `conn:*`
  (sysName coalescing documented as a trust assumption, lab-verified);
  P1 identity DB opened read-only by the routing service; P2 FNV
  partitioning risk documented. Live containerlab NAT acceptance
  EXECUTED: three FRR exporters behind one SNAT source
  (10.240.0.200), scenarios A–G pass — evidence in
  `docs/merger/evidence/2026-11-10-bn021c-nat-acceptance/`;
  `tests/bn021c_exporter_isolation.rs` is 12/12 including real-TCP
  loopback lifecycle tests.
- Done (11 Nov, independent PR review + hardening): concurrent
  same-claim connections can no longer merge — one live connection per
  `exp:*` scope, extras `conn:*`-isolated; disconnect journal failures
  return `TransportDisconnectOutcome` and block retirement instead of
  diverging live/replay; post-Termination frames rejected; clean
  Termination leaves no transport gap; `exp:*` scope components
  percent-encoded. Scenario F reclassified FAIL/OPEN — sequential
  same-claim impersonation and claim-based device attribution still
  require authenticated ingress. Evidence:
  `docs/merger/evidence/2026-11-11-bn021c-pr2-review/`;
  `tests/bn021c_exporter_isolation.rs` is 17/17.
- Pending: historical alias reconciliation, authenticated ingress
  exporter identity (sysName claims are self-reported), NetFlow/syslog
  adapter wiring (BN-021d+).
- Acceptance: independent BMP sessions sharing a NAT address never merge
  route/peer state — PARTIAL: verified for distinct claims and
  concurrent identical claims (post-hardening); sequential identical
  claims still coalesce — RESOLVED by BN-021f authenticated ingress
  (sequential same-claim now fails closed into `conn:*`; verified
  live in the mTLS NAT lab).

**BN-021d — NetFlow v9/IPFIX identity and template lifecycle** — PARTIAL.
- Done: template cache keyed by UDP SocketAddr + protocol version +
  Source ID/ODID + template ID, with 4096 outer-context guard; flow payload
  now includes provisional `exporter_id` alongside IP-valued
  `exporter_address`.
- Pending: inventory-backed exporter/device binding; distinct transport
  provenance, downstream `TelemetryUpdate.target` and graph association;
  template expiry, withdrawals, exporter restart/port changes and
  per-context size limits. Source ID/ODID are NOT globally unique, and an
  IP-derived provisional exporter ID still collides under shared NAT.
- Acceptance: NAT collision, protocol/template-ID reuse, source-port
  changes and restart cannot cross-contaminate decoding or attribution.

**BN-021e — Remaining protocol adapters** — OPEN.
- Syslog: relay transport identity vs claimed originator (trust levels).
- SNMP: configured polling target vs trap sender vs engine identity.
- OTLP: trusted resource/service identity, not collector IP.
- sFlow: keep payload agent address; validate trust/binding.

**BN-021f — Authenticated BMP ingress** — IMPLEMENTED + LAB VERIFIED (11 November).
- Done: mutual-TLS BMP ingress terminated in `bonsai-routing`
  (`bmp_mtls` config; `BONSAI_ROUTING_BMP_TLS_{CERT,KEY,CLIENT_CA}`);
  verified principal = SHA-256 of the client-cert DER, produced only
  by the TLS layer; `match_ingress_credential` binding pin (schema v4,
  additive/idempotent migration); `vexp:*` verified scope namespace
  fencing legacy claim-based `exp:*` journals; `BmpIdentityMode`
  Verified (default) vs explicit unsafe ClaimCompatible; conn:* scopes
  never carry `device_id`; live NAT lab with per-exporter client certs
  behind one translated source — sequential same-claim impersonation
  rejected, legitimate reconnect/multi-connection/replay verified.
- Pending: production exporter mTLS support or equivalent authenticated
  sidecar (lab uses `bmp_tls_proxy.py`); cross-protocol identity
  regression for NetFlow/IPFIX (BN-021d gate). Simultaneous
  old+new credential authorization during rotation (credential-list
  schema) is a deferred enhancement — sequential re-pin procedure
  documented.
- Evidence: `docs/merger/evidence/2026-11-11-bn021f-authenticated-bmp/`;
  tests `tests/bn021f_authenticated_ingress.rs` 9/9.

**BN-021f hardening — revocation & TLS availability** — IMPLEMENTED +
LAB VERIFIED (11 November).
- Done: TLS handshakes moved off the accept loop into bounded
  per-connection tasks (semaphore `MAX_HANDSHAKES`=64, 10 s timeout,
  shutdown-aware); active-session registry + periodic batched
  binding revalidation (`RECHECK_INTERVAL_MS`=5000) revokes live
  sessions on binding/credential/device change with durable
  `bmp_authorization_revoked` gap and last-known preservation;
  credential rotation = binding re-pin preserving canonical identity;
  production deployment policy requires mTLS off-loopback, Verified
  mode, and explicit dev overrides for plaintext/claim_compat;
  `VerifiedIngressPrincipal` fields private + transport-only
  constructor + fail-closed deserialization.
- Verified: `tests/bn021f_hardening.rs` 9/9; live NAT R1–R5 (active
  revocation, multi-session revocation, rotation, 64-permit TLS
  saturation + recovery, SIGKILL restart/replay).
- Evidence: `docs/merger/evidence/2026-11-11-bn021f-revocation-hardening/`.

**BN-021g — Production acceptance** — OPEN (renamed from BN-021f slot;
see above for the implemented slice).
- Cross-protocol identity regression tests; multi-site overlapping
  addresses; unknown/conflicting handling; collector restart/failover;
  source-alias migration + journal replay; identity metrics/diagnostics;
  simultaneous credential-authorization rotation (schema v5 candidate).

### Source-audited correctness refinements (existing BN ownership)

These are code-grounded acceptance additions, **not** a replacement
roadmap. Work proceeds in bounded slices; completed or lab-verified features
are not reclassified as absent.

- **BN-003 / BN-008 — gNMI state and provenance:** `src/subscriber.rs`
  currently ignores `Notification.delete`, logs `SyncResponse` without
  tracking per-device/path synchronization, and maps unsupported
  TypedValue variants to null. Require delete/tombstone semantics,
  reconnect sync/absence reconciliation, per-path coverage/freshness,
  source and collector timestamps, subscriber ID/sequence, complete
  typed-value fixture coverage, and vendor-backed BGP peer/LLDP link
  deletion tests. Validate duplicate subscriptions, network-instance
  scoping, jittered backoff and concurrency limits before expanding
  platform coverage.

- **BN-003 / BN-007 — BMP fail-closed transport and journal:** In
  `src/routing_service/mod.rs`, an `ingest_bmp_bytes` error currently
  breaks without marking a transport gap, and a transport-gap journal
  append error is ignored. In `crates/rbmp-store/src/journal.rs`,
  `RibEventJournal::open` reads an existing journal with
  `unwrap_or_default()`; I/O failure must never be treated as empty
  history. Test malformed frames, unreadable/corrupt journal, failed gap
  persistence, half-open/idle connections, connection limits, speaker
  allowlist and epoch-fenced reconnect. Transport failure means
  monitoring unavailable, not an inferred BGP PeerDown.

- **BN-008 / BN-009 — delivery correctness:** `InProcessBus::publish`
  returns `()` and drops when its router queue fills, while a broadcast
  send result is ignored. Define durable acceptance vs best-effort
  delivery, explicit queue/lag loss metrics, backpressure/spill behavior,
  commit-before-event and retry/idempotency semantics. Provide fault
  tests with no *unaccounted* loss; a lossless guarantee is not yet proved.

- **BN-007 / BN-013 / BN-014 — projection and agent truthfulness:**
  retire legacy BMP graph writers only after routing projection parity;
  do not advance projection checkpoint on failed writes. Define core
  entity schema/freshness and graph-derived as-of/diff APIs only after
  source provenance and projection correctness. Restrict external
  ad-hoc graph queries until BN-022 closes; favor curated, scoped tools.
  Preserve completed BN-014 live change-assurance evidence rather than
  reopening it as never implemented.

- **BN-019 — reproducible verification and repository hygiene:**
  `justfile` has `check-fast`, `test-replay`, `verify-release` but
  no unified `just ci` gate. Add fmt, scoped clippy, fast checks,
  replay/security regression and UI build on a working runner; capture
  revision and green CI before merges. Record a clean-tree prototype
  tag only after a reviewed release gate. Audit root artifacts and
  referenced evidence before archiving/removing large files; preserve
  reproducibility. Optional Cargo feature gating needs measured
  compile-time/binary-size effects and dependency review first.

- **BN-015 — control-plane storage design:** explicitly document
  inventory/config/auth/application state ownership across graph,
  restricted SQLite and vault, including backup/restore and migrations.
  Record the supported tenant/deployment scope in DECISIONS.md; do not
  silently relax an existing auth/ownership policy or assert a new
  product scope merely from a proposed plan.

## Verified foundation, not a new queue

- Imported routing crates and Bonsai routing service, public proxy/UI, scope-aware diagnostics, migration helpers and graph projector exist.
- R01–R04 repair direct durable event delivery, replay reconciliation, torn-tail truncation and durable epoch-fenced transport gaps; focused regression results are in the current review.
- R05–R06 add runtime wiring, mocked-provider integration, query row materialization bounds and scoped API-key checks. The strict deployment/security baseline is verified under BN-005; its current default ownership-access exception is tracked there. Live browser/model boundaries remain BN-003/004.
- R07 adds genuine two-run FRR router/BMP backend verification. Native second-NOS CLI capture now exists; BMP/RIB lifecycle and remaining browser gates stay BN-003/006.
- R08 adds honest execution-mode tags, elapsed measurements, clean-validation thresholds and a real PyTorch forward-pass smoke test. Actual trained/held-out models and provider comparisons remain BN-004/010/012.
- R09 and the new BN-001/002 work add signing, durable intent/lock records, completion-before-memory updates and shadow audit JSONL. Phase-specific crash reconciliation and authenticated operational assurance provenance remain open; BN-014 still needs a live twin/pilot.
- R10 addresses workspace formatting and selected core Clippy; it does not establish whole-product readiness.

## Historical source crosswalk

The exact pre-consolidation files and SHA-256 manifest are in
[the 13 October archive](docs/backlog_archive/2026-10-13/README.md).
This crosswalk preserves scope, not historical completion claims. Each intake
item must compare the archived acceptance criteria with current code and tests,
then add only concrete remaining work to its existing BN item. Do not reopen
already verified tasks or execute old destructive instructions verbatim.

| Source | Destination / disposition |
|---|---|
| B00–B03 | Verified baseline/parser foundation; continuing release gates BN-007/019 |
| B04–B06 | RIB/history foundation; current recovery verification in review; scale/semantics audit BN-007 (including directional AddPath negotiation and source-aware collector limits) |
| B07 | Vendor metadata exists; bounded live second-NOS acceptance is recorded under BN-006 |
| B08–B10 | Proxy/UI/correlation foundation; end-to-end acceptance BN-003/008 |
| B11–B13 | Auth/redaction/scoping foundation; deployment/security acceptance BN-005/015 |
| B14–B15 | Investigation/SSH helper foundation; operational acceptance BN-004/013/015 |
| B16–B20 | Migration/synthetic release helpers exist; live/release/legacy retirement BN-003/006/007/019 |
| B21–B22 | Prefix intelligence/grounded investigator foundation; BN-004/008/013 |
| A00–A02 | Metrics/alignment foundation; trained-model acceptance BN-010 |
| A03 | Runtime schema/budget foundation; operational loop BN-004/005 |
| A04–A08 | Evidence/diagnostic helpers; multi-source acceptance BN-005/008 |
| A09–A12 | Investigator/adjudication/scenario scaffolds; real provider/browser evidence BN-003/004/013 |
| A13 | Native SR Linux capture and bounded live route acceptance are recorded under BN-006; broader release/vendor coverage remains out of scope |
| A14–A15 | Forecasting surrogate/PyTorch smoke evidence; full model experiments BN-010/012 |
| A16–A20 | Plan/signature/audit helpers; execution/assurance/pilot BN-001/002/014/019 |
| R00 | Documentation reconciliation now superseded by this backlog and current review |
| R01–R04 | Focused recovery fixes verified; retain regression gates |
| R05–R06 | Wiring/scope tests verified; deployment/operational gates BN-003–005 |
| R07 | Live FRR backend gate; browser/second NOS BN-003/006 |
| R08 | Evidence labeling/calibration partially closes review; models/providers BN-004/010/012 |
| R09 | Signature/persistence primitives partially close review; safety/pilot BN-001/002/014 |
| R10 | Formatting/selected core lint; broader release gates BN-007/019 |
| DV1/DV2 root files | Empty at consolidation; no tasks inferred |
| Ubuntu DV2 task instructions | Historical intake BN-015/016/019; archive exact task text |
| RustyBMP handoff and Agentic NetOps strategy | Reference designs; executable work is this queue |
| ServiceNow AIOps strategy / playbooks source strategy | Design references for BN-013/015; no separate active queue |
| Seven-day operational handoff | Historical procedure; current acceptance reconciled in BN-019 |
| D3-1: Clean Install & Distribution | BN-007, BN-019; existing code requires intake verification |
| D3-2: Structured Onboarding (UI-first + NetBox import + cred association) | BN-015; existing code requires intake verification |
| D3-3: gNMI Onboarding Flow: Lab-Backed End-to-End Test | BN-015; existing code requires intake verification |
| D3-4: Graph Enrichment: CLI Scraping + NetBox + Rack Validation | BN-015, BN-016; existing code requires intake verification |
| D3-5: Remediation Maturity: Auto-Proposal + Human-in-the-Loop Flow | BN-013, BN-014; existing code requires intake verification |
| D3-6: AI Investigations: Provider Integration + Key Management | BN-004, BN-015; existing code requires intake verification |
| D3-7: README & Documentation Refresh | BN-019; existing code requires intake verification |
| D3-8: Graph Node Completeness (DV2 Carry-overs) | BN-008, BN-016; existing code requires intake verification |
| D3-9: GNN Production Readiness | BN-010, BN-011; existing code requires intake verification |
| D3-10: Developer Experience & CI Health | BN-007, BN-019; existing code requires intake verification |
| D3-11: Streaming & Endpoint Graph Completeness (Session 9) | BN-008, BN-016; existing code requires intake verification |
| D3-14: Collector Health & Telemetry Propagation to Core | BN-008, BN-009; existing code requires intake verification |
| D3-13: Receiver Supervisor, Hot-Reload & Full Port Configurability | BN-009; existing code requires intake verification |
| D3-12: Signal Observability, Detection Provenance & Topology Completeness | BN-008, BN-016; existing code requires intake verification |
| D3-15: Scalability & Multi-Source Correlation (Sessions 13–14) | BN-008, BN-016; existing code requires intake verification |
| D3-16: Live UI: Environment-Agnostic 3-Panel Refactor (Session 14) | BN-003, BN-013; existing code requires intake verification |
| D4-1: SNMP + Syslog: Vendor Architecture + MIB Management | BN-016; existing code requires intake verification |
| D4-2: Syslog Shunning / Suppression Engine | BN-016; existing code requires intake verification |
| D4-3: App Security: RBAC, LDAP, Secrets + LLM Key Management | BN-005, BN-015; existing code requires intake verification |
| D4-4: Incidents UI Revamp | BN-013; existing code requires intake verification |
| D4-5: Flow Support: sFlow + Server/VM Nodes + TSDB Integration | BN-008, BN-016; existing code requires intake verification |
| D4-6: Graph Quality Metrics + LLM Readiness Score | BN-008, BN-016; existing code requires intake verification |
| D4-7: Config Consolidation: TOML/YAML Sprawl to DB/UI | BN-015; existing code requires intake verification |
| D4-8: LLM Feedback Loop + RCA Supervised Testing | BN-004, BN-013; existing code requires intake verification |
| D4-9: Sidecar ML Python: Tight Integration + Health UI | BN-009; existing code requires intake verification |
| D4-10: NetFlow/OTLP Multi-Source Correlation: Deep Analysis | BN-008, BN-016; existing code requires intake verification |
| D4-11: BMP RFC 7854 Completeness | BN-007; existing code requires intake verification |
| D4-12: Redundancy Use Case: Graph-baked Detection | BN-008, BN-016; existing code requires intake verification |
| D4-13: UI-based DB Management | BN-009, BN-015; existing code requires intake verification |
| D4-14: Vault Hardening + Init Caveats | BN-015; existing code requires intake verification |
| D4-15: HITL + Remediation Testing | BN-013, BN-014; existing code requires intake verification |
| D4-16: BGP Config Change: FRR + BMP + Graph + Remediation | BN-003, BN-014; existing code requires intake verification |
| D4-17: Revised Device Onboarding: PyATS-first Bootstrap | BN-015; existing code requires intake verification |
| D4-18: NetBox + ServiceNow Enrichment + AIOps Quality Testing | BN-015; existing code requires intake verification |
| D4-19: End-to-End Clean-Slate Testing | BN-007, BN-019; existing code requires intake verification |
| D4-20: Environment Data: Power, Temperature, Optics, Sensors | BN-016; existing code requires intake verification |
| D4-21: Resource Governor UI Page | BN-009, BN-019; existing code requires intake verification |
| D4-22: install.sh + Makefile + CI Pipeline Hardening | BN-007, BN-019; existing code requires intake verification |
| D4-23: Ubuntu Testing Guide: Remaining Items + Phase 17 Completion | BN-019; existing code requires intake verification |
| DS-1: DDoS Signal Extraction: Multi-Source Telemetry Enrichment | BN-017; existing code requires intake verification |
| DS-2: DDoS Graph Schema: Nodes, Edges, Attack Fingerprints | BN-017; existing code requires intake verification |
| DS-3: DDoS Detection Rules: Pattern Library + Synthesizer Integration | BN-017; existing code requires intake verification |
| DS-4: DDoS Response: Cloud Sink Integration + BGP Signalling | BN-017, BN-014; existing code requires intake verification |
| DS-5: BMP Post-Incident Assurance: Prefix Convergence Verification | BN-003, BN-017; existing code requires intake verification |
| DS-6: DDoS ML Feature Pipeline: Graph-to-Feature Export for Sidecar | BN-010, BN-017; existing code requires intake verification |
| DS-7: DDoS Incident UI: Timeline, Attack Map, Mitigation Tracker | BN-013, BN-017; existing code requires intake verification |
| DS-8: DDoS Simulation + Testing Harness | BN-017; existing code requires intake verification |
| EV1-1: GNN Architecture Upgrade: STGNN + Attention Explainability | BN-010; existing code requires intake verification |
| EV1-2: Parquet Pipeline Control: Catalog, Schedule, Quality Monitor | BN-009; existing code requires intake verification |
| EV1-3: Semantic Embeddings: Syslog, Config, CLI Text Vectorisation | BN-011; existing code requires intake verification |
| EV1-4: BonPy–Bonsai Deep Integration: Event Bus + DB Unification | BN-008, BN-009; existing code requires intake verification |
| EV1-5: ML Job Engine: Scheduler, Lifecycle, Background Tasks | BN-009; existing code requires intake verification |
| EV1-6: BonPy UI Rewrite: Full MLOps Console (2026 Stack) | BN-009; existing code requires intake verification |
| EV1-7: Rule + Playbook Management: DB-Backed, UI-Controlled | BN-013; existing code requires intake verification |
| EV1-8: Structural Uncertainty: NCT, Control-Weighted GNN, Conformal Prediction | BN-010; existing code requires intake verification |
| EV1-9: Continuous Production Run: Sidecar Hardening + Watchdog | BN-009, BN-019; existing code requires intake verification |

## Definition of done

A BN item closes only when its named acceptance is demonstrated at the required
evidence level, dependencies are satisfied, failure paths are checked, and the
support matrix is updated. Passing test counts, enum labels, UI builds and
historical “completed” tables alone do not close a live or operational gate.
