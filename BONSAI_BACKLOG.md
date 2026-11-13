# Bonsai backlog

Updated: 11 November 2026. **This is the only active product backlog.**
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

| BN-023 | P1 Intake — existing Kubernetes chart is not a proven integrated deployment | **Validate and correct the existing** deploy/helm/bonsai chart, not a second chart. Static-review gap: values-fleet.yaml declares collectorReplicas=3 but no collector workload is rendered; the chart does not package the separate bonsai-routing service/ingress, and the multi-replica HA StatefulSet has independent graph/archive PVCs without a demonstrated single writer/replication/fencing contract. Deliver staged single-core + routing + two-collector deployment first; enforce BMP authenticated source integrity across CNI/NAT/LB, scoped collector mTLS/queues, API and UDP ingress, non-root and network isolation, secrets/rotation, probes, backup/restore and upgrades. Only promote HA/fleet claims after killed-pod/failover + replay/graph parity/zero silent loss and split-brain rejection on a reproducible kind/k3s lab. Keep the safe chart mode single-writer until authority is proven. | BN-005/007/008/009/015/019/021/022; [11 Nov code audit](docs/architecture/NETWORK_AGNOSTIC_READINESS_REVIEW_2026-11-11.md), deploy/helm/bonsai/{values-fleet.yaml,values-ha.yaml,templates/} |
| BN-024 | **P1 Intake — new native-network-intelligence capability, no implementation claimed** | Rust-native multi-NOS state normalization (selective SuzieQ OSS reference/port), lossless configuration parsing, effective policy semantics, typed temporal graph reconciliation, honest topology/expected/observed path proof, SONiC extension and deterministic LLM evidence. No SuzieQ service required at runtime. **Do not** replace PyATS/TextFSM or project into authoritative graph until source/license/parity/trust gates pass; per-feature rollout only. [ADR](docs/architecture/ADR_NATIVE_NETWORK_INTELLIGENCE_2026-11-11.md), [design](docs/architecture/NATIVE_NETWORK_INTELLIGENCE_DESIGN_2026-11-11.md), layered BN-024-00…25 below. | Novel semantic-engine epic; builds on existing BN-003/006/007/008/013/015/016/019/021/022/023 rather than duplicating their completed work |

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

**BN-021d — NetFlow v9/IPFIX identity and template lifecycle** — IMPLEMENTED + LAB VERIFIED (11 November).
- Done: collision-resistant provisional source key
  `flowsrc/{collector}/{proto}/{ip}:{port}/{sid|odid}{id}` carried by
  `TelemetryUpdate.target`, `TelemetryEvent::NetflowRecord.exporter_id`,
  AppFlow node id and `BonsaiEvent.device_address`; `exporter_address`
  kept as transport evidence only. Strict IPFIX template parser
  (per-specifier bounds, enterprise PEN preserved, varlen →
  non-decodable, message-length/padding validation). Bounded
  `FlowTemplateCache`: (collector, SocketAddr, version) → domain →
  template with caps on every dimension, TTL expiry, idle-context
  eviction, global count/byte budgets, v9 uptime + IPFIX export-time
  restart clearing, port-change isolation, RFC 7011 §8.4 UDP withdrawal
  ignore. Packet handling is transactional — all set boundaries are
  validated before any cache mutation, so rejected packets cannot alter
  template/restart state. Independent-review hardening (11 Nov):
  NetFlow graph projection fails closed for every flow update — no
  `DeviceAddress` refresh, `CARRIES_FLOW`, or `SRC_HOST`/`DST_HOST`
  edges — because no authenticated flow ingress exists yet and even a
  serialized `identity_resolution` is self-asserted msgpack evidence.
  Flow-derived utilization/DDoS device alerts are likewise disabled
  until verified ingress exists. AppFlow id is the full 5-tuple
  including `src_port`. Plain UDP
  stays `provisional` with `device_id = null`; `resolve_flow_identity`
  is the verified-ingress extension point.
- Verified: `tests/bn021d_flow_exporter_isolation.rs` 15/15 + 3 unit
  tests (incl. canonical-collision, forged-claim, transactional
  malformed-packet and src_port AppFlow regressions); live docker NAT
  lab (3 exporters → one SNAT source) — 7
  distinct provisional AppFlow identities, zero canonical Device
  attribution, template-id/protocol collisions decoded independently,
  malformed flood rejected, collector restart safe.
- Pending: authenticated flow transport (TLS relay / TCP-SCTP IPFIX)
  to populate `device_id`; TCP/SCTP withdrawal semantics.
- Evidence: `docs/merger/evidence/2026-11-11-bn021d-flow-exporter-isolation/`;
  lab `lab/acceptance/bn021d-flow-nat/`.
- Acceptance: NAT collision, protocol/template-ID reuse, source-port
  changes and restart cannot cross-contaminate decoding or attribution
  — VERIFIED for observational separation; canonical attribution
  remains gated on verified ingress by design.

**BN-021e — Remaining protocol adapters** — OPEN (sFlow + syslog + SNMP-trap + OTLP slices IMPLEMENTED 11–12 November).
- sFlow: DONE for the isolation slice — provisional `sflowsrc/{collector}/{transport}/agent{addr}/sub{id}` keys separate NAT-shared agents, agent claims, sub-agents and collector scopes; `agent_address`/`sub_agent_id`/sequence/uptime preserved as self-reported evidence; graph fails closed (AppFlow + source-scoped events only — no `DeviceAddress` refresh, `CARRIES_FLOW`, `SRC_HOST`/`DST_HOST`, `Interface` counter mutation, or flow-derived device alerts); AppFlow id is the full 5-tuple. Verified: `tests/bn021e_sflow_exporter_isolation.rs` 7/7 + 2 unit tests. Canonical sFlow attribution still awaits a transport-attested ingress path (BN-021g).
- Syslog: DONE for the isolation slice — provisional `syslogsrc/{collector}/{transport}/{peer}[/conn{id}]` keys separate transport senders (incl. relays and NAT endpoints) and TCP sessions; claimed hostname is demoted to an explicit candidate hypothesis (`unique`/`ambiguous`/`unresolved` — duplicate hostnames across tenants never pick arbitrarily); `SyslogConfig.collector_id` added; graph fails closed (provisional `StateChangeEvent` scoped to the source key + read-only candidate probes — no `Device` upsert, `REPORTED_BY`, canonical state mutation, or `IsisAdjacency`/service/QoS writes from claims; forged serialized identity fields ignored). Fact events renamed `syslog_fact_candidate`/`syslog_fact_unresolved` — old `joined`/`orphan` python rules no longer fire (intentional). Verified: `tests/bn021e_syslog_source_identity.rs` 10/10 + 13 lib tests incl. 44-fixture corpus. Canonical syslog attribution still awaits a transport-attested ingress path (BN-021g). Residual: device-scoped `SignalPolicy` filters key on the source key now (role/site scopes still apply).
- SNMP: DONE for the trap-receiver isolation slice (11 November) — provisional `snmpsrc/{collector}/udp/{peer_ip}:{peer_port}` keys separate the observed UDP sender, collector namespace (`SnmpConfig.collector_id`), configured polling target (demoted to an explicit `unique`/`ambiguous`/`unresolved` candidate hypothesis — never the authenticated sender), v1 `agent-addr`, v3 engine IDs and USM security name (all preserved as self-reported, unverified claims). Dedup moved after community/USM filtering and is keyed by `(trust domain, source key, trap OID)`; empty/missing/disallowed communities now fail closed under a configured allowlist. Graph fails closed: provisional `StateChangeEvent`s scoped to the source key + read-only candidate probes — no `Device` upsert, `DeviceAddress` refresh, `REPORTED_BY`, canonical Interface/BGP/BFD/ISIS/OSPF/service mutation, or detection/remediation under trap-claimed identity; facts emit `snmp_fact_candidate`/`snmp_fact_unresolved`. SNMPv3 auth digest is NOT verified — `usm_auth_status: "unverified"` everywhere; encrypted scoped PDUs rejected explicitly; full USM auth deferred to BN-021g. Verified: `tests/bn021e_snmp_source_identity.rs` 11/11 + lib parser/target tests. Canonical SNMP attribution still awaits a transport-attested ingress path (BN-021g). Polling/target management untouched.
- OTLP: DONE for the isolation slice (12 November) — provisional `otlpsrc/{collector}/http/{ip}:{port}` keys derive from the observed TCP peer only (`ConnectInfo`; `X-Forwarded-For`/headers/payload fields never consulted); `service.name`/`host.name`/`peer.address`/metric values preserved as self-reported evidence with a consistent source-scoped `otlpapp/{source_key}/{service}` provisional id (no canonical Application nodes, no global service-name identity). Graph fails closed: provisional `StateChangeEvent`s (`otlp_span_event`/`otlp_metric_event`) + read-only exact-match candidate probes — the `address STARTS WITH` joins, `RUNS_SERVICE`/`HOST_RUNS_SERVICE`/`APP_IMPACTED_BY_NETWORK` edges and Application health-column writes from untrusted claims are removed. Ingestion budgets bound body (axum 2 MiB), resource groups, spans, data points, attributes, string lengths, timestamps; non-finite metrics dropped; drops metric'd via `bonsai_otlp_items_dropped_total`. Evidence durable via parquet archiver + graph events. Verified: `tests/bn021e_otlp_source_identity.rs` 8/8 + BN-021/022 regression batch. Canonical OTLP attribution still awaits authenticated ingress (BN-021g). HTTP/JSON only — no OTLP/gRPC claimed. Residual: Application health columns no longer update from OTLP (documented coverage change); per-connection source keys are session-scoped, not stable device identity.

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

**BN-021g — Production trust foundation** — SLICE 1 IMPLEMENTED
(12 November): authenticated collector identity + scoped ingest
authorization on the existing collector→core gRPC channel.

- **BN-021g-1 collector trust foundation (this slice, hardened after
  independent review):** transport principal extracted from tonic
  `TlsConnectInfo` peer certificates (never payload/metadata);
  `"sha256:<hex>"` leaf fingerprint must match
  `exporter_bindings.match_ingress_credential` on an ACTIVE
  credential-pinned binding — certificate/collector mismatch, unbound
  certs, revoked bindings, ambiguous bindings, unattested principals
  and unauthorized scopes are rejected per message. Capability gates:
  `collector` (ingest plane per claimed `collector_id`, heartbeat,
  register stream), `detection-ingest` (separate authority + target/
  source-event provenance verification), `core-admin` (device/site/
  credential mutation, query, remediation writes), `sidecar`,
  `operator-read` (global reads incl. topology/sites/event stream;
  device-scoped reads limited to assigned devices), any-pinned-binding
  (`get_leader`). **Classified observation boundary (H1):** each record
  is decoded, classified (`TelemetryEvent::authority`) and only then
  authorized — canonical classes (YANG paths AND canonical writers on
  `streaming/` paths: BMP, BGP-LS, interface summaries, config-state)
  require an enabled registry device assigned to the collector;
  provisional classes require a `*src/` source key re-namespaced to
  the authenticated collector; unclassified paths are ignored. Path
  prefixes never establish graph authority. **Queue ack contract
  (H2):** the core consumes the whole ingest stream before responding;
  `accepted`+`rejected`==submitted, rejected records are permanently
  retired, RPC errors retain the batch — acks describe records
  actually processed. **Per-message capability revalidation (H3):**
  `detection-ingest` is re-checked on every message of an open stream;
  authz decisions are epoch-stamped so binding mutations invalidate
  the cache immediately. **Detection provenance (H4):** remote
  detections require non-empty, existing, same-device,
  server-provenanced non-provisional source events; malformed payloads
  rejected; `remote_detection_ingest` provenance marker stored;
  storage failures are retryable stream errors. **Read ambiguity
  (H5):** scoped reads use the same trusted-binding ambiguity rules as
  ingest (no `DISTINCT` collapse); one credential cannot be provisioned
  for two collector identities. **Session lifecycle (H6):** assignment
  cleanup is owned by a stream drop guard (disconnect/cancellation
  safe); generation check + cleanup run under one lock. **Auth
  policy:** `runtime.auth_mode` = `required` (default, fail
  closed) / `development-compat` (explicit dev opt-in). Authz decisions
  cached ≤5s (bounded 4096-entry cache, epoch-invalidated);
  durable provenance via `audit_log`. Verified:
  `tests/bn021g_collector_trust_foundation.rs`
  25/25 real loopback mTLS + real GraphStore wiring — classified-
  authority canonical-writer gating, provisional namespace
  re-homing, per-message detection revocation mid-stream, evidence
  provenance denial, ambiguous-binding read denial, disconnect
  cleanup — plus real-server queue drain tests (interleaved
  rejection, all-reject, retryable failure). Evidence:
  `docs/merger/evidence/2026-11-12-bn021g-collector-trust-foundation/`.
- **Residual-findings round (R1–R4, 13 November, same branch):** a
  third review confirmed four residual defects, all closed:
  **R1** provisional normalization now unconditionally pins effective
  `source_key`/`exporter_id`/`identity_status` server-side (missing,
  null, numeric or malformed producer claims cannot reach
  `classify()`'s untrusted `exporter_address` fallback; originals
  preserved under `claimed_*` evidence; `otlpapp/` ids re-derived
  under the authenticated namespace; payload prefix must match the
  classified protocol). **R2** `verify_detection_scope` is a positive
  allowlist of server-established `source_type` provenance with
  structured detail parsing — the previously missing `sce-no-prov`
  submission is now tested; malformed details permanent, store
  failures retryable. **R3** the collector validates ingest responses
  before acking (response protocol version not major-skewed;
  `accepted + rejected` == submitted via checked arithmetic —
  violations retain the batch); protocol version is verified per
  record; `InProcessBus::publish` returns `PublishOutcome` so router
  saturation is a retryable failure, not silent loss. **R4** active
  `protocol="collector"` identities are globally unique (schema v5
  partial unique indexes + provisioning checks + authorization
  fail-closed on ambiguous rows; rotation keeps one `binding_id`).
  Cache epoch is captured before the store read and bumped before
  audit persistence; indexed credential lookup verified via
  `EXPLAIN QUERY PLAN`. Verified:
  `tests/bn021g_collector_trust_foundation.rs` **31/31** + 16/16
  ingest drain tests (scripted-core contract: consistent batch,
  accounting mismatch, impossible counts, version skew, bus
  saturation, replay) + lib 299 pass/5 documented BN-019-B6 baseline
  failures + 110/110 BN-021b–f/BN-022/syslog/snmp/otlp/sflow
  regressions.
- **BN-021g-2 first verified original-source projection:** narrow OTLP
  case via direct authenticated producer identity or explicitly
  authorized attestation; no arbitrary remote-peer ownership inference.
- **BN-021g-3 trusted syslog acquisition.**
- **BN-021g-4 NetFlow/IPFIX + sFlow acquisition.**
- **BN-021g-5 SNMPv3 USM cryptographic verification (supported libs).**
- **BN-021g-6 production acceptance:** NAT, overlapping private
  addresses, replay, rotation overlap, failover, revocation fan-out,
  migration, coverage, evidence retention, explicit trust states in
  API/UI.

Remaining BN-021g-1 limitations: assignment authorizes the collector's
acquisition process for a configured device — it is NOT cryptographic
proof of original device identity, so a compromised collector can
still forge observations for its assigned devices (BN-021g-2+ owns
verified original-source projection); revocation cannot recall device
credentials already delivered over an assignment stream (rotate them
operationally); the 5s authz cache is the documented maximum
revocation window; capability bindings (`collector`,
`detection-ingest`, `core-admin`, `sidecar`, `operator-read`) must be
provisioned via the existing identity admin API before mTLS clients
can use those RPCs; `runtime.auth_mode=development-compat` must never
be used outside isolated loopback development.

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


### 11 November 2026 topology-agnostic and trusted-operator review: delta only

**Source review:** [2026 network-agnostic readiness audit](docs/architecture/NETWORK_AGNOSTIC_READINESS_REVIEW_2026-11-11.md), based on main @ d9447fb14d9f23d82b244913da5a20c6efe91c8d. It distinguishes **decoded (D), ingested (I), graph-projected (P), independently validated (V), operationally proven (O)**. No lab test, model benchmark, external-agent test or production HA acceptance was executed in this documentation slice. These are additions to **existing BN owners** (not a second queue); BN-023 is the sole new top-level ID because the previously implemented Helm chart lacks a current production-shaped release contract.

- **BN-003 / BN-006 / BN-007 / BN-016 — 2026 environment capability matrix and semantic witnesses (P1).** Inventory capabilities per NOS **and version**, node role (P/PE/RR/leaf/spine/SD-WAN edge), instance, AFI/SAFI and available source (BMP, gNMI, CLI, BGP-LS, controller, FIB). Make the UI/support matrix show D/I/P/V/O separately; a profile, decoded NLRI or lab topology file is not deployed proof. First bounded lab journeys: (1) MP-BGP VPNv4/v6 PE-P-PE with RD-vs-RT import/export, per-view RIB, labeled/SR forwarding, and control-plane versus FIB distinction (do not require a P router to carry customer BGP); (2) BGP EVPN/VXLAN leaf-RR fabric with EVI/ESI/MAC-VRF/IP-VRF/VTEP/VNI, RT1/2/3/4/5, multihoming, host mobility, withdrawal and physical-underlay versus overlay paths. Add negative tests for overlapping VRFs/tenants, route reflector incomplete views and unsupported features. Publish oracle NOS captures, time-scoped graph projection, parity checks, known limitations and independent forwarding tests before vendor-agnostic claims.
- **BN-007 / BN-008 / BN-016 — BGP-LS source and domain authority (P1).** Reconcile rich rbmp-core wire decoding with src/streaming/bgp_ls.rs's separate JSON-line bridge and graph BgpLsNode/BgpLsLink writer; preserve RFC 9552 BGP-LS Instance-ID, protocol/IGP domain/AS/area/MT-ID and original descriptor keys, producer identity, path/view and withdrawals. Bridge archives that fail while published updates continue must be explicitly gap-marked/replayed or fail closed, not silently become durable evidence. Test two IGP domains using identical router IDs, disagreeing redundant producers, link withdraw/re-add, replay, disconnected bridge and truncated frames. Prevent inferred BGP-LS topology from being passed off as installed forwarding.
- **BN-008 / BN-015 / BN-016 / BN-021 — one canonical, temporal and layered graph (P1).** Extend trusted exporter resolution through all authoritative graph write paths, not just BMP PeerKey: src/graph/common.rs currently keys Device/DeviceAddress by address/IP and src/collector/graph.rs keys Interface/LldpNeighbor by bare address. Provide migration + backward-compatible selectors from address aliases to scoped canonical device_id/interface/VRF identity; quarantine unresolved/conflicting sources. Model physical LLDP (including one-sided), underlay IGP/SR, BGP control adjacency, VPN/EVPN service membership and VTEP/tunnel, installed FIB/LFIB, and application/service dependency as **different edge types**, each with source evidence, confidence, freshness, first/last seen, delete/tombstone, sync/epoch, gap and as-of semantics. Define reconciler conflict rules, never equate BGP reachability with forwarding. Tests: overlapping private IP at separate sites, NAT exporters, device renumber/ifIndex reset, duplicate BGP-LS producer, missed gNMI delete, BMP/EOR restart, EVPN mobility and RT policy change; graph diff must reflect the actual source view without cross-tenant merge or phantom healthy link. GNMI delete/sync and graph projection checkpoint defects are **already** tracked above: implement them in the same slice, do not reopen as new work.
- **BN-015 / BN-016 — read-only SD-WAN and cloud overlays (P2).** Use the existing integration/adapter framework for one actual controller API (fabric inventory, VPN/segment, tunnels, color/transport, policy intent, SLA-loss/latency/jitter and edge health), mapping asserted controller intent separately from observed edge flow/underlay. No proprietary protocol guessing from encrypted packets. Gate on authentic controller responses, stale/offline state, overlapping tenant route domains, topology reconciliation and independent edge health; do not claim general SD-WAN support from a schema alone.
- **BN-004 / BN-013 / BN-014 / BN-022 — deterministic internal and external LLM operator boundary (P1).** Reuse shipped MCP, graph/read boundary, AI provider and investigation/approval systems. Design versioned least-privilege **curated** tools for scoped as-of route/peer/VRF/service/path/coverage, before-after diff and independently checked change invariants; include evidence IDs, counterevidence, source view, timestamp/lag, unsupported facts, truncation and exact authorization on every tool response. Treat NetClaw or any external LLM as an untrusted scoped client, never direct graph-authority/secret or device write access. Replay test missing telemetry masquerading as absence, cross-tenant source collision, malicious prompt-like syslog/config payload, provider outage, stale evidence, model false certainty and unauthorized write attempts. Compare model answers with deterministic validators/adjudicated operator outcomes and budgets; T0 read-only → T1 adjudicated investigation → T2 signed human-approved assurance; autonomous operational writes **remain disallowed**.
- **BN-009 / BN-010 / BN-011 / BN-012 — ML delivery correctness before promotion (P1/P2).** Existing STGNN training/ablation/held-out gates remain; additionally enforce graph snapshot model schema/identity and label lineage. python/bonsai_ml/gnn/data_loader.py currently chooses id/address/hostname fallback and chaos labels by hostname; require explicit canonical ID mapping and missing-feature masks with adversarial reorder/rename/overlap tests and no unlabelled positives/negatives. python/bonsai_ml/inference_loop.py currently discards POST HTTP result status, may report a successful run on failed persistence and applies a fixed threshold: record model+feature+calibration+as-of versions, check 2xx, idempotently retry/mark failure, expose stale/missing scores, use workload-calibrated thresholds and rollback to statistical baselines. Do not claim synthetic-chaos AUC or attention weights as field performance or causal RCA.
- **BN-013 / BN-016 — useful deterministic use cases preceding ML novelty (P1/P2).** Add **unused/underused link triage** with admin/oper status, LLDP/underlay, LAG/ECMP and redundancy intent, rate-normalized counter deltas (wrap/reset), long-lived seasonality, flow corroboration, active/standby distinction, telemetry coverage, maintenance windows and an adjudicated “safe spare vs stranded capacity” result; no recommendation to shut a link on traffic alone. Add **class-aware QoS verification** by comparing intended/attached policy, DSCP classification/remarking, offered/forwarded/drop, queue depth/ECN/WRED and measured service impact; no queue or microburst inference from only aggregate interface/NetFlow samples. Correlate blackhole/path drift and SRLG/diversity gaps through independent RIB/underlay/FIB assertions. Require baselines vs ML holdouts, device-days false alerts, per-incident latency and operator utility; missing sources ⇒ unknown, not zero utilization or compliance.
- **BN-013 / BN-015 / BN-016 / BN-019 — existing UI workflow, not a rewrite (P2).** Preserve Live's three panels, Routing AFI/VRF/view filters, Explorer, Sidecars, Integrations and Incident/Investigation routes. Add one traceable **service/prefix to evidence** journey: physical/IGP/control/EVPN/FIB layer switches, tenant/network-instance scope, coverage denominator and source trust badge, unresolved/conflict states, time window/as-of and diff, explicit “why not discovered?” breakdown, source-to-edge lineage, permission-aware read-only next check, evidence export/adjudication. src/http_server/observability.rs's main link listing is largely LLDP-centric, and ui/src/lib/Topology.svelte heat color uses cumulative bytes, not bytes/sec; correct semantic labels and normalized windows before showing utilization. Extend current Playwright/accessibility coverage with 1k+ node virtualization/pagination, keyboard-friendly graph alternatives, narrow viewports, 401/403, incomplete data and recovery state.
- **BN-023 / BN-019 — Kubernetes acceptance ordering (P1).** Start with a single graph/RIB writer and separate bonsai-routing and collector workloads; verify scoped identity across LoadBalancer/NAT, TCP BMP versus UDP flow exposure, mTLS, durable collector queues, release upgrade/restore, per-site placement, NetworkPolicies, restrictive ServiceAccounts and probes. Do not call the existing chart “collector fleet” or “HA” merely because values-fleet.yaml declares replicas or the HA StatefulSet creates separate PVCs: no shared state consistency or leader fencing is thereby established. Run kind/k3s live no-duplication, source-rebinding, restart/pod-disruption/rollout and split-brain negatives before increasing replicas or release claims.

**Priority decisions:** P0 preserve BN-021 ingress and BN-022 credential boundaries; P1 next focus BN-008/016 scoped temporal topology and BN-023 safe deployment; then PE-P-PE and EVPN/VXLAN support witnesses; then curated LLM tools and operator journeys; then SD-WAN controller pilot and promoted ML. Do not impede the in-progress BN-021d NetFlow slice. All new work remains Intake/Partial until a named test plus lab/capture provenance establishes acceptance.


### 11 November 2026 — environment onboarding, UI semantics and competitor-reuse acceptance

**Source-audited UI/product review:** [onboarding, site/DC topology and competitor audit](docs/architecture/UI_ONBOARDING_TOPOLOGY_PRODUCT_REVIEW_2026-11-11.md), based on main @ `50ba09bd352d7471ff996a95b67f6532bbeca371` (after merge of PR #3). Reviewed App navigation, all current routes and shared components, onboarding/discovery/registry APIs, graph/site/topology projection, selected browser tests and the existing SuzieQ ADR. **These are taskable sub-slices of existing BN owners, not a second active backlog or a UI rewrite.** Static review only; no browser/device/NetBox/ServiceNow/benchmark results were produced. Protect the current BN-021d NetFlow implementation and earlier verified acceptance.

**Explicit UX invariant across all sub-slices:** An environment/site/asset can exist with zero supported collection protocols and without NetBox/CMDB. Every displayed device/link/path/fact has identity, intended-vs-observed status, source authority, time, scope, coverage and an honest `unsupported / unknown / unobserved / stale / conflicted / validated` state. Registered equipment is not silently absent from topology merely because it has not produced a graph observation. Conversely, inventory/CMDB intent alone may never become a verified adjacency. Do not claim end-to-end forwarding from BGP or LLDP alone.

| Sub-slice / priority | Source-grounded problem and exact work | Acceptance gate (red test, then fix; check deployed behavior separately) | Dependencies / owner |
|---|---|---|---|
| **BN-015-U1 / P0 — NetBox selected import contract** | `ui/src/lib/Onboarding.svelte:421-449` posts `selected_paths: []` to `/api/onboarding/devices/with_paths`; `src/http_server/managed_devices.rs:245-255` refuses zero paths. Separate inventory registration from subscription activation; import preview must not require gNMI. Preserve external immutable object IDs/role/site/tenant/status and user-selected credential policy; no arbitrary `credentials[0]` assignment or `leaf` default for unknown role. | Browser mocked paginated NetBox → preview → selected import produces explicit persistent inventory-only assets, per-row success/fail, duplicate/name/IP conflicts, a later opt-in collection plan, safe retries and rollback preview. Import after zero-path is not labelled “streaming”; check errors/rejected rows. | BN-015/008/021/022 |
| **BN-015-U2 / P0 — site corruption on device save** | `save_managed_device` calls `sync_sites_from_targets` (`managed_devices.rs:320-325`), which uses a name-derived ID to upsert a blank root Site (`graph/mod.rs:2654-2673`), while `upsert_site_record:9226-9237` overwrites parent/kind/coords/metadata on match. Stop untrusted device `site` strings from mutating existing Site records; stable site association must be an explicit ID with conflict/reconciliation policy. **FIXED (13 November):** `sync_sites_from_targets` no longer upserts Site records at all — `ensure_target_site` resolves the free-text claim by name-derived id, then UNIQUE exact-name match (explicit-id/imported sites), else creates the minimal record create-only (NetBox-backfill pattern: `ON MATCH` touches nothing); ambiguous display names yield no link and no duplicate. Device saves now only write the `LOCATED_AT` association; site admin `upsert_site`/`validate_site_hierarchy` unchanged. Verified: 6 new `graph::tests::site_sync_*` regressions (hierarchy preservation, slug-id clobber, move-only-association, ambiguity, idempotence+restart, imported-record preservation) + corrected stale `site_sync_creates_site_and_located_at_edge` baseline failure (canonical port-stripped `Device.address`); before-fix reproduction: 5/6 new tests FAIL on pre-fix sync (name-derived duplicates `tower-a`/`imported-dc`/`dc-1` + mis-links). Residual: name-derived device.site claims remain ambiguous for duplicate display names (explicit-id resolution added; canonical site_id migration stays BN-015-U3). | Persist DC→pod→rack with custom kind, parent, coordinates, environment and metadata; add/update device in pod and assert **all** properties and hierarchy unchanged, including after restart/import/replay. No silent site creation or root reparenting. Preserve existing `validate_site_hierarchy` cycle protection. | BN-015/008; P0 UX/data-integrity gate |
| **BN-016-U1 / P0 — BGP graph false-edge protection** | `observability.rs:253-266` resolves a BGP peer to the first BGP_SESSION_WITH destination where destination is any onboarded device. Replace `allowed_devices.contains(dst)` alternative with exact trusted peer/session/VRF/collector identity and explicit unresolved state; expose unknown peer as stub, not a fabricated link. | At least two neighbors from same source (R→A, R→B), one with overlapping/private IP, one unknown/withdrawn, one multihop/RR: verify each drawn link belongs only to its session and no arbitrary path becomes reachable. Cross-check graph/query vs API vs browser. | BN-008/016/021 |
| **BN-015-U3 / P1 — canonical geography and name migration** | Site IDs created via `site_id_from_name`; `site_summary_handler` maps subtree **names** to devices (`managed_devices.rs:400-419`); `/api/topology` reduces sites to `HashMap<name,id>` (`observability.rs:219-234`). Device/site selection uses free-text names even though the backend supports hierarchy IDs. Migrate to stable site_id, preserve path aliases, support duplicate names in different parents, safe rename/move/re-parent and NetBox/CMDB ID mapping. | Two regions each contain `DC-1/Rack-1`, and both have device address 10.0.0.1 in separate scopes: site tree, child counts, device membership, map and search never leak or collide. Orphan/missing sites explicitly presented. Client prevents picking descendants as parent before backend rejects cycles. | BN-015/008/021, after U2 |
| **BN-015-U4 / P1 — protocol-optional device onboarding** | `src/discovery.rs:86-129` probes only gNMI Capabilities; `Onboarding.svelte:1054-1071` requires it and at least one gNMI path to advance. Support manual/CSV/JSON/NetBox/Nautobot/CMDB inventory-first entry and optional per-protocol activation (gNMI, SSH/CLI, SNMP poll/trap, BMP exporter, syslog, flow, controller). No blind network scanning by default, no credential required merely to declare an asset. | Clean install with no SoT/CMDB: manually declare SNMP-only access switch, CLI-only P router, passive BMP RR, offline imported PE; each remains searchable and site-associated, with `inventory only / unsupported / not probed` badges, not rejected or “healthy”. Attempting unsupported gNMI must not block activating another supported method. | BN-015/016/009/021 |
| **BN-015-U5 / P1 — resolved capability and profile accuracy** | `discovery.rs:795-809` RoleEnv override currently checks role but ignores environment; onboarding discovery request does not pass selected environment archetype even though `DiscoveryInput` supports it. Distinguish advertised models, successfully subscribed paths, CLI/SNMP coverage, parsed facts and graph-projected facts; validate vendor/role/environment/AFI/SAFI predicates before enabling a profile, with preview and rollback. | Two same-role devices in DC vs SP/campus receive **different expected** path plans; unsupported path renders unsupported, missing telemetry renders unobserved, expired subscription renders stale; no accidental override cross-scope. Verify after restart with actual status evidence and negative paths. | BN-003/008/015/016 |
| **BN-015-U6 / P1 — optional intended-state and CMDB reconciliation** | NetBox/ServiceNow enrichers and UI integration tabs exist, but no unified conflict-first onboarding contract. Add a per-field authority matrix (Bonsai manual, NetBox/Nautobot intended DCIM/IPAM, ServiceNow CI/service ownership, device observed), connector identity + last sync, import/export preview, immutable external keys, conflict queue, partial failures, limits and audit; preserve no-integration mode. Don't write to external SoT by default. | No connector → functional; NetBox-only → planned assets/sites, not assumed operational; CMDB-only → owner/CI edges without invented cables; both disagree on site/role/hostname → conflict shown and resolved without last-writer-wins, no authority spill into tenant/VRF. Cross-region duplicate IP and missing primary IP covered. | BN-015/008/016/022 |
| **BN-016-U2 / P1 — inventory-aware geographic graph** | `observability.rs:289-313` only returns observed Device graph rows filtered to registry; `SiteRail.svelte` derives sites only from returned devices; `Topology.svelte:59-71` filters exact `Device.site` strings rather than subtree IDs. Introduce scoped union of declared-only + observed assets and explicit unresolved/quarantined sources; site catalogue (including empty sites), region/DC/pod/rack grouping, breadcrumbs/subtree scope and off-site link stubs; stable pin/layout. | Empty site and offline declared asset visible as such; selecting DC includes child pod/rack and optionally inter-site stubs; no cross-site false edges or “site missing” from no telemetry; agent/API and UI agree for same site_id/tenant scope and as-of time. | BN-015-U2/U3, BN-008/016 |
| **BN-016-U3 / P1 — layered link truth, as-of, conflicts** | Live topology is built largely from LLDP CONNECTED_TO and resolved BGP/BFD/IS-IS pairs; it offers All/Fabric/Routing layers, no universal source-evidence and as-of contract. Separate inventory/intent, physical, IGP/SR underlay, BGP control, VPN/EVPN overlay, FIB/forwarding, service and impact. Each edge carries first/last observation, provenance IDs, completeness, source-specific state, direction, contradicting evidence and life interval; conditional “no path” wording by coverage. | One-sided LLDP, multiple BGP peers, unsupported P-router MP-BGP, EVPN route with broken VTEP, EOR/reconnect gap and collector loss: UI never renders a verified end-to-end path without forwarding evidence. Click an edge → original source/view/timestamp/cursor/identity or explicit unavailable. | BN-003/007/008/016, BN-021; prior 2026 topology audit |
| **BN-016-U4 / P2 — map rate/performance and hierarchy UX** | `Topology.svelte:130-135` uses lifetime byte totals to paint link heat, not utilization; D3 simulation redraws on reactive updates, and rendering is client-side. Add bounded API snapshots/pagination, hierarchical clusters and progressive layout, normalized counter deltas with speed/interval/wrap/reset, unknown-rate labels; persistent user layout, zoom/fit/search, legend and keyboard-readable table. | 1k+ devices/5k+ links/100 sites stress fixture; interaction/CPU budgets measured, selection/focus and labels remain stable on 15s refresh; zero sample or counter reset cannot be shown as zero utilization; cross-boundary links remain discoverable. | BN-016/019 |
| **BN-020-U1 / P1 — first-run/auth state truth** | `Onboarding.svelte:208-225` skip writes default environment/site without checking HTTP results and always completes; `governance.rs:561-590` detects first run via records; `App.svelte:109-139` treats setup-probe network failure as authenticated. Add explicit resumable setup choice (manual/SoT/read-only), persisted progress and rollback; distinct 401/403/offline/first-run/partially-configured states, role-aware disabled actions and audit. | Simulate offline API, auth expiry and failure mid-wizard: no made-up site, false login, repeated wizard, secret leak or completion banner; refresh resumes correctly. Viewer sees inventory-only but not forbidden writes. | BN-015/020/022 |
| **BN-013-U1 / P1 — one evidence-first operator journey** | Current `Live`, `DeviceDrawer`, `Routing`, `Explorer`, `Incidents`, `Investigations` and `Approvals` are individually useful but don't share a verified network entity and time context. Introduce shared scope/as-of/evidence/deep-link contracts: env→site→device/VRF→prefix/path→incident→investigation→independent assurance; no LLM-only inferred relationships. | Reproduce a missing VPN prefix and EVPN MAC move with a mixed-quality source: same identity/view/time and evidence IDs across pages, contradictory facts visible, missing protocol yields unknown, no provider yields safe terminal error, action approval shows plan hash/scope and explicit dev-write-disabled state. | BN-004/008/013/014/015/016 |
| **BN-015-U7 / P2 — avoid reinventing SuzieQ/NetBox/Batfish** | Existing `docs/architecture/adr_suzieq_vs_pyats.md` already evaluates SuzieQ and defers it as a primary poller; alternative tooling provides mature normalized tables and modeled verification. Timebox an **optional** SuzieQ read-only snapshot adapter + NetBox/Nautobot SoT/CMDB importer benchmark + Batfish offline change assurance oracle; no embedding/duplicate polling engine or independent credentials/config database without an ADR. Evaluate commercial IP Fabric/Forward/NetBrain API bridges only when licensed and present. | Two real NOS/scenarios compare source coverage, normalized tables, historical validity, path/intent differences, false-link rate, freshness, operator time and memory/cost. Document no-go if integration adds more authority or operational debt than value; reuse existing PyATS/TextFSM where appropriate. No “better than” marketing without results. | BN-006/007/015/016/019; competitor audit in [review](docs/architecture/UI_ONBOARDING_TOPOLOGY_PRODUCT_REVIEW_2026-11-11.md) |
| **BN-019-U1 / P1 — route-by-route UI/a11y contract** | `App.svelte` currently routes `/integrations` while `tests/ui_driver/topology.spec.js` and `a11y.spec.js` reference legacy `/topology`, `/enrichment` and `/adapters`; separate component files may be unreachable. Generic page-load checks don't validate semantics. Audit each live component, remove/redirect obsolete routes only after parity, unify loading/empty/permission/offline UX, and update `ui/README.md` Vite-starter content. | Authenticated Playwright journeys for no external SoT, real/mock NetBox and CMDB, role-scoped operations, missing protocols, site subtree, failure/retry, false edge, stale time and 1k-node map; axe + keyboard/table alternatives, mobile view, router deep links and zero regressions for proven BN-003/019 flows. Record precise build/test artifacts; no unrun check marked passed. | BN-013/015/016/019/020/022/023 |

**Operator-facing priority:** fix BN-015-U1/U2 and BN-016-U1 before expanding the topology UI; then BN-015-U3/U4/U5 and BN-016-U2 for safe inventory and coverage; then BN-016-U3 and BN-013-U1 for evidence-driven visual operations; only then optional product adapters and advanced layout. Prior BN-015 onboarding and BN-016 graph tasks were *not* deleted or reclassified as absent. Include these slices inside those items' existing definition of done.

**UI route/component inventory:** the [review](docs/architecture/UI_ONBOARDING_TOPOLOGY_PRODUCT_REVIEW_2026-11-11.md) lists every current surface (`App`, first-run, onboarding, environments, sites/site rail, Devices/DeviceDrawer, Profiles, Credentials, Collectors, Routing, Live/Topology, Events/Trace, Incidents, Investigations, Explorer/DB, Integrations plus legacy Adapters/Enrichment, Approvals/Audit, HA/Governance, Sidecar/Operations, Security/Settings/Certs, Syslog/SNMP, StreamControl, Endpoints, Config Library, Users, CommandPalette, Toast/density) with per-surface follow-up. Existing working controls remain authoritative until independently tested; no wholesale UI rewrite or promise of full vendor support.

### BN-024 — Native network intelligence: staged Rust-port and network-understanding program

**New, distinct epic; priority P1 Intake (11 November 2026).** Source/design: [ADR](docs/architecture/ADR_NATIVE_NETWORK_INTELLIGENCE_2026-11-11.md) and [implementation contract](docs/architecture/NATIVE_NETWORK_INTELLIGENCE_DESIGN_2026-11-11.md), audited against Bonsai `main` `d5833fe82ac7a510599d6bc010d21b6ad6e9aef2` and SuzieQ OSS `75198b5585ebef91f0659b6b1ec3c964cc2cd1ad`. **None of the BN-024 items below is implemented, tested or completed by this documentation change.** Each is a bounded future PR requiring code-level re-triage against then-current `main`. New BN-024 explicitly owns only **native normalization, configuration semantics, semantic reconciliation, path reasoning and their experience**. Existing owners continue to own source identity/provenance (BN-008/021), onboarding/Sot (BN-015), base topology (BN-016), operator/LLM (BN-013), security (BN-005/022), release (BN-019/023). Do not duplicate or reclassify verified work.

**Strategic constraints:** one installable Bonsai product; no required SuzieQ, NetBox or CMDB; Rust-native feature modules where safe; Python/Genie/TextFSM retained until proven parity; no device writes; vendor extensions gated by explicit image/version/transport/feature tests. A single product may use isolated workers for unsafe parsing and fleet routing. Five separately sourced truth planes: intended, configured, effective, observed and validated. “Not observed” is not “down,” “successfully parsed” is not “semantically understood,” and BGP/LLDP/LPM without FIB evidence is not ASIC forwarding proof. Support matrix per NOS/version/feature uses **D** decoded, **I** ingested, **N** normalized, **S** effective semantics, **P** graph projected, **V** independently validated and **O** operationally proven. Every slice records exact revision, fixtures, source rights, executed tests/measurements, failures and rollout constraints.

#### Layer 0 — rights, quality baseline, contracts (G0; no graph-writer enablement)

| Slice / status | Scope and implementation boundary | Acceptance (include negative/red tests) | Dependency / rollout gate |
|---|---|---|---|
| **BN-024-00 / P0 Ready — OSS provenance and port license manifest** | Pin SuzieQ source SHA, upstream module/file versions and license/NOTICE chain; inventory `suzieq/config/*.yml`, `config/schema/*.avsc`, `poller/worker/services/{service,svcparser,interfaces,bgp,routes,...}.py`, `engines/pandas/path.py` and fixtures. Identify separately licensed TextFSM/templates and other third-party assets. Publish reusable-vs-reference-only decisions, Apache-2.0 attributions, modified-file notices, SBOM and legal escalation for unclear sources; never copy Enterprise code. | CI scans attribution/SBOM/third-party allowlist and rejects unreviewed fixtures; exact pinned source and hash reproducible; one deliberately missing-notice fixture must fail. | BN-007 licensing; **no copied implementation before this**. |
| **BN-024-01 / P0 Ready — legacy parser contract repair, baseline corpus and differential harness** | **Confirmed source mismatch:** `src/parser_chain.rs` posts `command_pattern` to PyATS while `docker/sidecars/pyats/app.py::ParseRequest` requires `command`; the Rust `pyats_genie` backend label is not one of the Python `genie/auto/textfsm` selectors, and `ntc_templates` is dispatched to the generic native line parser. Default `bonsai_native` is a successful-looking line split. Add red request-schema + dispatch tests, repair routing with explicit backend response and fail closed for unstructured fallback (no “parity” based on fake success). Reproduce pinned SuzieQ OSS service outputs offline; build device NOS-version/capture/service fixture manifest from upstream and Bonsai genuine FRR/EOS/SR Linux evidence. Establish independent oracle command outputs and field-level quality classifications; label coverage gaps for XR/Junos/SONiC rather than borrowing a vendor claim. No SuzieQ runtime dependency in production. | Deterministic raw → expected schema fixture tests include null/empty VRF, nested ECMP, bad units, duplicate names/IP, malformed JSON, no data, stale capture, conflicting fields; compare Rust vs pinned Python with exact intended deviations reviewed. Baseline resource/time and device capture load measured. | BN-006/015-U7; benchmark only; Python allowed in **test harness**. |
| **BN-024-02 / P0 Ready — canonical typed fact, time and evidence ABI** | Define versioned serde/JSON+schema contract for `FactEnvelope`: canonical device/tenant/env/network-instance/VRF/AFI/ASIC scope; plane/facet; source identity/epoch/cursor, time fields, collection coverage, trust, validity, evidence digest, parser and mapping revisions, semantic/unknown status. Bind existing `telemetry_identity.rs`, graph IDs and routing history; restrict bulk config/RIB storage. | Property-based tests across overlapping IP, exporter NAT, renumber/rename, two source instances, replay/restart, old-schema migration, missing poll/zero result and clock skew; no ambiguous fact mutates authoritative device or site; as-of never mixes latest data silently. | BN-008/021g/015-U2/U3/016-U1/022; **graph writes disabled until these pass**. |
| **BN-024-03 / P0 Ready — extension ABI, feature manifest and architecture seam** | Design `NosProfile`/`FeatureNormalizer`/`ConfigResolver` contracts + registry, semantic schema evolution, packaging/runtime boundary, safe profile precedence (NOS family+exact version/build+role+feature+transport+ASIC namespace), feature flags and per-plugin budgets. Audit current `MultiSourceEnricher`, `ParserChain`, `ConfigStore`, writer and auth paths to avoid second source of authority. | A “dummy future NOS” module and a SONiC metadata fixture register without changing core collectors/graph code; wrong image/version/capability cannot select profile; missing module reports untested/unsupported; hot-disable/rollback and schema compatibility proven in unit tests. | BN-009/015-U4/U5/019/023; ADR design review before implementation. |

**G0 blocking correctness:** Existing BN-015-U1 (NetBox zero-path import), BN-015-U2 (site hierarchy overwrite) and BN-016-U1 (arbitrary BGP link resolution) are **mandatory fixes before enabling authoritative projections**, not new BN-024 copies. BN-021 identity trust and BN-022 scoped reads are mandatory for untrusted config/CLI facts. Do not hold the entire offline differential harness hostage to those fixes.

#### Layer 1 — Rust operational normalization and SONiC adapters (G1)

| Slice / status | Scope and implementation boundary | Acceptance (include negative/red tests) | Dependency / rollout gate |
|---|---|---|---|
| **BN-024-04 / P1 Intake — safe inventory-bound read-only capture** | Integrate a bounded capture plan into existing enrichment/credentials/collector lifecycle: explicit allowlisted SSH commands, gNMI `Get`/Subscribe, later NETCONF/controller where independently supported. Record running vs startup/config and poll success/partial/failure separately; no new credential database, no scan, no unrestricted commands. Rate/site/device budgets, jitter/cancel/backoff and source-specific completeness. | CLI-only router and offline asset retain inventory; unsupported gNMI doesn't block CLI; invalid SSH host key/TLS, over-budget job, auth failure, timeout, device restart and collector loss produce audited typed gaps, not healthy/down or invented empty tables. | BN-015-U4/U5, BN-009, BN-021/022; read-only explicitly. |
| **BN-024-05 / P1 Intake — restricted Rust extraction DSL** | Selectively port behavior of SuzieQ `svcparser.py` and applicable YAML selectors into a compiled, non-Turing-complete path/field DSL; preserve VRF/nested list parent context and typed defaults. Unsupported expression → error or explicit Rust normalizer, never eval/shell execution or an auto-success line array. | Golden parity for vendor JSON, TextFSM-style input if licensed, 1-to-many next hops, empty hierarchy, null/default and unit conversions; fuzz nested/hostile input, regex/JSON complexity, malformed expression and memory/CPU limits. Test that arbitrary expression injection cannot execute. | BN-024-00/01/02/03; parallel parser only. |
| **BN-024-06 / P1 Intake — interface/L2 operational facts** | Port audited SuzieQ cleaners per exact NOS/service for interface state/address/speed, LLDP, VLAN, LAG/MLAG, VRF and neighbor tables; separate observed link from configured neighbor/CMDB cable. Refuse fake numeric zero defaults and preserve interface/ASIC aliases. | Independently reviewed EOS/FRR/SR Linux + available Junos/XR fixtures: normalized precision/recall, admin-vs-oper distinction, speed unit/wrap, LAG member, one-sided LLDP, overlap and interface rename; compare all corresponding old parser outputs; error/unsupported fixtures. | BN-024-05, BN-008/016-U3; no authoritative projection yet. |
| **BN-024-07 / P1 Intake — routing/control/overlay normalizers** | Port scoped BGP peers by VRF/AFI/SAFI, RIB routes/nexthops/ECMP, ARP/ND/MAC, OSPF/IS-IS/BFD where captured and EVPN VNI/status, reusing existing Bonsai BMP/RIB as **separate** observation authority. Normalize RD/RT, VTEP, route preference only when correctly sourced; never create an installed FIB edge from BGP. | Multi-VRF duplicate prefix, RR and P-router no VPNv4, BGP withdraw/EOR gap, recursive next hops, EVPN MAC move, vendor route state/AFI and missing AFI fixtures; exact view/timestamp, no false graph edge. Differential SuzieQ/Genie and independent device capture matrix recorded. | BN-024-02/05/06, BN-003/006/007/008. |
| **BN-024-08 / P1 Intake — SONiC single-/multi-ASIC read-only profile** | Implement SONiC as a **versioned extension**, not generic FRR: detect image/build/HWSKU, host+NPU namespace; ingest authorized CONFIG_DB, APPL_DB, STATE_DB, counters and supported native gNMI/YANG paths, preserve saved config vs live differences and internal per-ASIC links. No remote privileged Redis assumption or ASIC forwarding claim. | Genuine SONiC version-tagged fixtures for port/LAG/VLAN/VRF/route, live-vs-startup mismatch and missing namespace; two NPU namespaces with duplicate interface keys never collapse; APPL_DB route does **not** get `validated FIB`; unsupported image/path returns untested. Live gates separated in BN-024-24. | BN-024-02/03/04/05; SONiC official DB/multi-namespace docs; BN-021/022. |
| **BN-024-09 / P1 Intake — shadow parity, feature rollout and fallback** | Run new Rust normalizers **alongside** existing Genie/TextFSM/gNMI/CLI pipeline on same captured evidence, compare typed outputs, record corrected vs regressed behavior and collect resource impact. Never let duplicate writers compete; default feature OFF until promoted and reversibly ON per (NOS, version, feature). | Real multi-device captures; no authority/graph delta in shadow, reproducible false-positive/false-negative report and thresholds ratified before rollout; restart/slow parser/cancel/rollback restores old path, no unexplained dropped observations; no Python removal before equivalent deployed acceptance. | BN-024-01/06/07, BN-009/019; **G1**. |

#### Layer 2 — secure configuration and effective semantics (G2)

| Slice / status | Scope and implementation boundary | Acceptance (include negative/red tests) | Dependency / rollout gate |
|---|---|---|---|
| **BN-024-10 / P1 Intake — config-source capture/retention contract** | Extend existing `ConfigStore` and change-detection capture for running/startup/candidate, source/transport, immutable hashes, authorized decrypt, redaction metadata, key rotation/restore and capture coverage. Prefer model-driven read path, pinned SSH host key and read-only command allowlist; do not dump secrets into graph/logs/LLM. | Device with masked and unmasked secret strings, denied key, stale saved vs running config, failed/partial transfer, change-event coalescing; snapshot is encrypted, replayable and scoped, diff doesn't erase negative observation; backup/restore and scope tests. | BN-015 vault/config authority, BN-022; port not evidence until device validation. |
| **BN-024-11 / P1 Intake — lossless per-dialect config AST** | Build span-aware ordered syntax parsers for small verified EOS/IOS/XR-style and Junos hierarchical/set subsets; SONiC typed JSON/DB and SR Linux/YANG profiles as independent dialects. Preserve comments, tokens, unknown syntax, nested scope and parse failures, no monolithic Cisco grammar or silent drop. | Round-trip/line-span provenance; two NOS implementations with true captures, malformed input fuzz, duplicate clauses, special quoting, Junos groups and vendor version deltas. Unrecognized construct produces explicit parse hole and cannot turn into a permissive policy. | BN-024-01/03/10; no effective semantics yet. |
| **BN-024-12 / P1 Intake — effective configuration IR and resolver** | Define typed representation for VRFs, interfaces, neighbor/peer-group AFIs, prefix/community/as-path sets, ordered policies, references/defaults/inheritance. Resolve precedence using tested NOS feature semantics and source spans; cycles/unknown operators remain unresolved. Avoid confusing config lexical presence with runtime activation. | Per-vendor golden policy cases: multi-level inheritance, shadow/override, unknown command, unresolved ref, cyclic peer/template, deletion and default; deterministic verdict `allow/deny/unknown/not-applicable` with citation and no default allow. | BN-024-02/11; platform/feature-specific, documented gaps. |
| **BN-024-13 / P1 Intake — BGP, RD/RT, VRF, route-policy semantic slice** | Implement bounded initial feature family: VPNv4/v6/IPv4 AFs, neighbor export/import policy, route maps, route targets, prefix-list influence and next-hop config references. Model RD as uniqueness identity and RT as import membership; preserve peer/view and scope. | Genuine PE-P-PE route-target mismatch with observed BMP reception and missing VRF FIB; matching RT with policy deny; RR/P underlay-only; policy clauses inherited through peer group; negative ambiguous policies. Report hypothesis vs validated cause, no route installation inference. | BN-024-12 and BN-003/006/008; **first useful network semantics milestone**. |
| **BN-024-14 / P1 Intake — graph projection, migrations and indexes** | Project versioned, non-secret semantic nodes/edges into **existing** graph via canonical IDs, evidence links and valid intervals; no new Device/IP authority and no bulk RIB/config duplication. Migration, backfill, idempotent replay, explicit delete/tombstone and writer checkpoint after committed writes. Label configured/effective/observed/validated distinctly. | Site hierarchy save, false BGP edge and same-IP cross-tenant regressions from BN-015-U2/BN-016-U1; hash-stable replay, writer crash before/after commit, rollback, reparse vN+1, stale source withdrawal; graph counts bounded and no phantom healthy/path link. | BN-024-02/10/12/13; **blocked on BN-015-U2, BN-016-U1, BN-008, BN-021/022**. |
| **BN-024-15 / P1 Intake — temporal reconciliation and conflict explanations** | Build deterministic field-level authority/identity/coverage reconciler for intended/configured/effective/control/forwarding evidence; as-of joins with snapshots/cursors, source success metadata, gaps, conflicts, corrections and no-result semantics; no last-writer-wins. | NetBox says one rack, observed/CMDB another; same hostname/IP different scope; stale CLI routes vs newer BMP withdrawal; missing gNMI delete and partial EOR; false “route absent”, contradiction preserved, no unauthorized device merge or silent data loss. | BN-024-02/14, BN-008/015-U6/021; **G2**. |

#### Layer 3 — path, impact and independent assurance (G3)

| Slice / status | Scope and implementation boundary | Acceptance (include negative/red tests) | Dependency / rollout gate |
|---|---|---|---|
| **BN-024-16 / P1 Intake — explicit path-mode API and scope** | Separate graph-connectivity `topology` from configuration `expected`, device-state `observed` and `compare` modes; typed input source/dest/VRF/AFI/time/packet-class, per-hop evidence, completeness, reason codes, limits/cursors, auth. Existing `/api/path` retains backward-compatible documented graph-only meaning. | Graph connectivity without route, observed route without FIB, unreachable attachment, out-of-scope tenant and unknown destination: no mode returns “verified forwarding”; response schema/migration and timeout tests. | BN-024-02/14/15, BN-013/022. |
| **BN-024-17 / P1 Intake — Rust route-aware hop resolver** | LPM by VRF/table/AFI, typed recursive next-hop and ARP/ND adjacency lookup, bounded ECMP fanout, visited-set/loop termination, source/destination attachment, as-of RIB/FIB distinction and policy uncertainty; no Pandas data-frame port. Cache only by complete snapshot/version/scope. | 2+ recursive route levels, ECMP, interface-down, loop, blank ARP, route withdrawal, asymmetric path and large route table; compare with real FIB/probes and SuzieQ's path case where applicable; no unsupported/hardware path silently proven, resource budgets measured. | BN-024-07/13/15/16; **G3 v1** after independently verified cases. |
| **BN-024-18 / P2 Deferred until G3 v1 — EVPN/VXLAN/MPLS/SR path extensions** | Extend path state machine for VTEP underlay recursion, EVPN EVI/VNI/MAC+IP and RT membership, MPLS label/SR policy/LFIB/transport, multi-homing and VXLAN tunnel. Separate BGP-LS learned topology, BGP control state, software FIB and physical probes. | Genuine leaf-RR EVPN RT2/5, mobility/withdrawal, dual VTEPs, PE-P-PE VPN labels and SR variation; configured-vs-observed path disagreements, bounded branches and explicitly unsupported hardware; no generic feature claim without platform witness. | BN-024-17, BN-006/008/016, capability matrix; do not block useful v1. |
| **BN-024-19 / P1 Intake — semantic config diff and change impact** | Replace line-change-only conclusions with reference-aware semantic deltas: changed prefix/community/route policy/RT/neighbor/interface → affected sessions/VRFs/prefixes/services; preserve existing BN-014 baseline/candidate invariants and distinguish possible effect from observed fault. | Change one RT/peer-group policy referenced by many routes, no-change text churn, config change with no fault, fault without config change and missing observational coverage; exact dependent list/truncation, independent device proof, no fabricated causality. | BN-024-12/13/15, BN-014; no operational writes. |

#### Layer 4 — LLM, operator UX, hardening and production acceptance (G4)

| Slice / status | Scope and implementation boundary | Acceptance (include negative/red tests) | Dependency / rollout gate |
|---|---|---|---|
| **BN-024-20 / P1 Intake — deterministic evidence tools for LLM/MCP** | Add curated `get_config_facts`, `explain_effective_policy`, `compare_network_state`, `trace_path`, `assess_change_impact` APIs with typed evidence IDs, counterevidence, as-of/version, coverage, bounded redacted excerpts and auth scope. LLM provides narrative only; never parser/edge authority or arbitrary CLI/Cypher. | Adversarial config and LLDP prompt injection, fake source claim, untrusted CMDB, cross-tenant/VRF request, truncated query, no-provider run and hallucinated path: tools deny/leak nothing and return independent deterministic partial result; clinician-like answer quality not inferred from model fluency. | BN-024-02/13/15/17/19, BN-004/013/022; no reads beyond current auth policy. |
| **BN-024-21 / P1 Intake — usable single-product operator journey** | Evolve **existing** Onboarding/Devices/Config History/Topology/Routing/Investigations UI with capability and parser-state badges, redacted config anchor/effective explanation, configured vs observed graph layers, time-aware 3-mode path explorer, conflict and unknown diagnostics. One product without SuzieQ or CMDB; no wholesale redesign. | Real browser journey: standalone CLI-only device → config snapshot → BGP/VRF policy → missing VPN prefix → path compare → independently cited explanation; stale/unsupported/no NetBox/no LLM/Viewer/expired auth, accessibility keyboard/table, 1k devices/5k links, no fabricated healthy/path. | BN-015-U1/U2/U4/U5/U6, BN-016-U1/U2/U3, BN-019-U1, BN-024-13/16/20. |
| **BN-024-22 / P0 promotion gate — security/plugin/resource containment** | Threat-model untrusted parsers/templates/SSH/SONiC DB payloads and config secrets; command allowlists, host-key and TLS trust, credentials, per-source/tenant authorization, safe memory/CPU limits and optional sandboxed worker; protect graph/LLM from prompt injection and model output confusion. Feature-specific rollback and audit. | Fuzz Rust DSL/AST/path, malicious YAML/expression, oversized JSON/regex, secret-bearing config, revoked principal mid-capture, unauthorized export, NPU-scoped alias and queue saturation. No write surface, auth bypass or unaccounted source-loss; fail closed with metrics. | BN-005/007/009/021/022, BN-024-03/04/05/10/20; **mandatory for any production promotion**. |
| **BN-024-23 / P1 Intake — migration, performance, HA and release proof** | Run shadow→canary→per-feature default only after calibrated thresholds. Measure normalized-field errors, false graph edges/path proofs, device CPU/poll impact, RSS/CPU/disk/queue, p95 path, 1k/5k UI, snapshot retention, upgrade/restore and HA/replay. Keep Python fallback until same hardware/feature live validation and reversible rollback, no duplicate collection/write authority. | Clean install/offline, disable/enable native plugin mid-incident, SIGKILL/replay, journal/graph checkpoint, schema vN rollback, upgrade on real data, slow SONiC namespace, resource saturation, no stealth loss. Publish *support matrix D/I/N/S/P/V/O* per exact NOS version and pilot feedback; unverified = not available. | BN-019/023/009, BN-024-09/14/17/20/21/22; **G4**. |
| **BN-024-24 / P1 Intake — SONiC live feature-specific acceptance** | Extend BN-024-08 with an actual supported SONiC single-ASIC and, separately, multi-ASIC deployment when a reproducible image/hardware is available. Test native gNMI or read-only API, config live/saved divergence, namespaces, BGP/FRR integration, LAG/VLAN/VRF, route/neighbor state, restart and credentials. | Captured version/build/HWSKU/namespace/source and repeatable CLI/API evidence; APPL_DB installed ≠ ASIC probe; at least one deliberately missing NPU namespace and stale CONFIG_DB state; per-feature D/I/N/S/P/V/O label, explicitly **untested** until hardware or matching lab exists. | BN-024-08/11/14/17/22/23; do not claim SONiC production from fixtures. |
| **BN-024-25 / P2 Deferred — extensible NOS ecosystem and conformance kit** | Publish versioned NOS manifest/driver/normalization/config-dialect conformance fixtures, plugin vetting, semantic schema migrations, contribution rules, CI compatibility matrix and per-NOS support owner. Candidates: additional Junos variants, IOS-XE/XR, EOS, SR Linux/SR OS, Cumulus, SONiC derivatives, controller-driven devices. Prefer model-driven APIs and vendor fixtures; no core switch proliferation. | Independent contributor adds a new **test-only** NOS profile without editing shared graph, credentials or API core; unsupported versions remain untested; invalid license/secret/escape/namespace failures block promotion; release/rollback across feature versions proven. | BN-024-00/03/09/22/23, BN-019/023; after two proven NOS slices. |

**Operator-facing priority and critical path:** (1) source/legal/fixture baseline BN-024-00/01, existing P0 site/NetBox/BGP-edge defects, (2) fact contract/plug-in design BN-024-02/03, (3) safe capture + native extraction BN-024-04…09 on a **single verified NOS × feature** (not a giant translation), (4) secure config AST/effective BGP/VRF semantics BN-024-10…13, (5) trusted graph/reconcile BN-024-14/15, (6) narrow path/certified assessment BN-024-16/17/19, (7) LLM/UI/release gates BN-024-20…24; advanced EVPN/MPLS and broad NOS SDK BN-024-18/25 remain separately gated. All items **Intake/Ready/Deferred**, never “Done,” until versioned evidence is produced.

**Existing BN-015-U7 disposition:** its SuzieQ/Genie/NetBox/Batfish **comparative benchmark** remains valid as a baseline (BN-024-01/09), but an optional SuzieQ read API integration is **no longer the preferred product architecture**. This supersession is in the [ADR](docs/architecture/ADR_NATIVE_NETWORK_INTELLIGENCE_2026-11-11.md), not an instruction to remove PyATS today. Reuse existing separate BN-015/U/016/U backlog gates and BN-014 assurance; do not double-count their acceptance under this epic.


### BN-019 build throughput, CI correctness and agent workflow — 11 November 2026

**P1/P0 targeted acceptance additions to existing BN-019, not a competing epic.** Design/agent policy: [11 November Build Acceleration Playbook](docs/BUILD_ACCELERATION_PLAYBOOK_2026-11-11.md). Static source audit of `AGENTS.md`, `docs/DEVELOPMENT.md`, `.cargo/config.toml`, `Cargo.toml`, `justfile`, `Makefile`, `docs/build-times/runs.csv` and checked-in CI paths on main `f4243cd15da570fade44c69d67db18aefee151cd`; compared with Rust Cargo/sccache/nextest upstream docs. **No benchmark, code fix, or performance gain is claimed in this documentation PR.** The observed problem is *compile/codegen/link and expensive duplicate targets*: reported 9–15 minute first integration target, ~55 minute new worktree check, ~6–9 minute release Docker rebuilds, and ENOSPC; warm `bonsai --lib` checks can be seconds. Avoid using dissimilar reported timings as an aggregate benchmark.

| Slice / priority / state | Bounded change (one reviewed PR where feasible) | Measurable acceptance, including safeguards | Owner / dependency |
|---|---|---|---|
| **BN-019-B1 / P1 Ready — profiling and build-critical-path baseline** | Instrument **specific** commands: `cargo check -p bonsai --lib --timings`, `cargo test -p bonsai --test <known_target> --no-run --timings`, `cargo build --bin bonsai-routing --timings` and a targeted rbmp crate; capture worktree/target-dir, sccache hits/misses, toolchain, CPU/RAM/disk, features/profile, mold, job count, peak RSS and actual test execution separately. Keep `runs.csv` format, dated extended evidence. Identify dependency critical path (lbug/CMake, codegen/link, proc macros, build scripts). | Repeat like-for-like host/SHA/profile and controlled warmed and **isolated** cold cases; three warm samples each for priority commands; record p50/p95 **only if sample sizes support them** (otherwise individual timings), plus top slow units and before/after target sizes. No destructive `cargo clean` on shared workspace; no invented percentage improvement. Deliver prioritized root-cause report and proposed numeric target budgets grounded in measurements. | BN-019/007/009; precedes global Cargo tuning |
| **BN-019-B2 / P1 Ready — worktree/build-owner and disk safety** | Establish one primary warmed `target` and one heavy Cargo build owner/queue; document compatible `CARGO_TARGET_DIR` reuse versus lock contention and per-worktree isolation; disk preflight and quota/retention, cache ownership, recovery from ENOSPC; optional tested cross-worktree sccache path normalization; no unbounded parallel compiler workers. | Reproduce two concurrent agent requests without duplicate 600-dependency cold builds, lost work, target lock storms, CPU/IO starvation or disk exhaustion. Preserve protected BN-009 soak CSV, other agents' worktrees/targets, running labs and cache. Prove a safe shared-target scenario and one isolated-target scenario with time/RSS/disk metrics; never delete artifacts without active owner approval. | BN-009/019/023; operational policy in AGENTS.md |
| **BN-019-B3 / P1 Ready — trustworthy progressive test selection** | Map source components/paths → minimum typecheck, unit/contract, targeted integration/replay/security, UI/Browser and actual lab acceptance. Encode a documented L0–L4 ladder in agent docs and optionally fast `just` recipes; deliberately batch related `--test` targets in one invocation. Prefer owner-crate tests rather than creating another heavyweight integration executable, but never move tests to weaker authority. | Exemplars for rbmp parser-only, BN-021 identity/BMP, graph/onboarding, UI-only and BN-024 future parser. A changed shared identity/graph/schema/source expands to conservative mandatory regressions; filtered execution is not mislabeled as avoided compilation. Every PR records exact test outcomes and every unrun gate remains open. | BN-007/013/019/021/024; preserve acceptance coverage |
| **BN-019-B4 / P1 Intake — measured Cargo dev/profile and sccache experiment** | Compare current `[profile.dev] opt-level=1, incremental=true, debug=1` and `[profile.dev.package."*"] opt-level=3, codegen-units=256` against isolated alternative dependency optimization (e.g. opt0/opt1 with targeted opt2/3 carve-outs), tuned codegen/debug, and sccache/incremental combinations. Profile `LBUG_SHARED=1` in safe lab only with loader/ABI validation; measure CPU/memory-limited `cargo -j N` vs default. **No blind global profile changes**. | Benchmark under same toolchain/commit/host, isolate target dirs and memory budget, measure cold+warm compile/link/test runtime, correctness/resource trade-offs and disk; preserve normal sccache wrapper and mold, no accidental flag churn. Accept config change only with demonstrated net improvement and equivalent real-routing/graph behavior; revert otherwise. Document cache invalidation for rollout. | After B1 and B2, BN-007/019 |
| **BN-019-B5 / P1 Intake — isolate high-churn Rust algorithms from main Bonsai link** | Profile whether moving pure vendor normalization/config-policy/path algorithms into **small internal Rust crates** with minimal deps avoids `bonsai` + lbug/parquet/axum test links; leverage existing `rbmp-*` pattern. No new graph, identity authority or duplicate types; create an interface/fixture design before changing crate topology. | One representative BN-024 parser/routing-policy edit: crate-only build+golden/fuzz tests execute without compiling/linking heavyweight Bonsai test binary; integration boundary still validated by a full-product test; measured improvement and stable schema/replay; no circular dep, API regressions or extra artifact pressure. | BN-024-00…03/05/11, BN-007/019; benefits need proof |
| **BN-019-B6 / P0 Ready — fail-closed CI gate and compile-once test execution** | No `.github/workflows/` currently checked in; audit `justfile`/`Makefile`: `make test-all` and `make test-integration` can mask actual failure via `|| true`. Introduce a **minimal protected/check-required** CI workflow: fast static/format/wiring and applicable Python/UI checks, Rust small-crate tests, build heavyweight binaries once, then mandatory scoped integration/security/replay suites; optionally use pinned `cargo-nextest` archive+partition when demonstrated. Include failure-path CI tests; don't automatically release Docker on docs-only changes. | Intentional failing Rust/Python/UI/test target yields red status; missing test binary is not green; disallow no-check merges for code, deterministic Cargo.lock/toolchain/profile, no secrets in cache; verify branch/PR SHA and exact run links. Compare CI wall/cost against old manual scope, use cached compile artifacts correctly, no silently skipped required tests. Gate release/lab promotion separately. | BN-007/019/022/023; restore reliable merge signals before optimizing away gates |
| **BN-019-B7 / P2 Intake — reuse release images and expensive integration environments** | Analyze repeated `docker compose build bonsai-standalone bonsai-routing` (typically ~6–9 minutes in recent log) and pinned build context dependency invalidation. Scope Docker/OCI rebuild to changed binaries and package from an already verified SHA where safe; reuse cached dependencies and keep artifact digests/SBOM, no stale image used as new code proof. Avoid full containerlab teardown/recreation when a bounded updated binary suffices. | Compare one source-only, one Cargo.lock and one routing-only change with clean/provenance-checked image digest, fixed identity of the executed binary, lab correctness, build minutes and cache consumption. Changed code must be deployed, no “passed” from prior image; Docker cache cleanup cannot destroy other agents' evidence. | BN-007/019/023; resource and lab ownership |
| **BN-019-B8 / P1 Intake — measurable sustained throughput and adoption** | Roll out AGENTS/dev/docs fixes, tracked developer/Devin build lane policy and dashboards/weekly evidence. Measure developer feedback p50 on equivalent commands, compile/link vs test split, sccache effectiveness, disk/ENOSPC incidents, cancelled duplicate compiles, PR lead time and full required-gate completion. Consider remote sccache/distributed compilation **only** with an authenticated/sandboxed, economically justified design; no uncontrolled public build workers. | A sequence of at least 5 real bounded PRs with reproducible timings and no missing required tests, no shared-target delete or soak corruption, zero false-green CI; numeric performance/cost target set from B1 and tracked after changes. Revert/regressions documented. Do not claim a blanket x-times acceleration without controlled proof. | BN-009/019/023/024; after B1…B7 |

**Devin execution priority:** B1 baseline and B6 fail-closed status **first**, B2 worktree/disk ownership at the same time, B3 cheap test ladder immediately in practice; then B4 controlled profile experiment, B5 selective pure-crate split with BN-024, B7 Docker/lab reuse, B8 sustained proof. Keep **BN-021d/021e/021g** protocol/security tasks and existing P0 graph/onboarding fixes running through properly scoped, passing tests—performance work must not silently replace product acceptance.

**Existing evidence/source limitations:** `docs/build_performance.md`'s early 2026 cold/warm figures and `runs.csv`'s varied November measurements are historical and have mixed profiles/hosts/commands; `AGENTS.md` already requires sccache and focused builds while older `docs/DEVELOPMENT.md` contradicted it. These documentation corrections are guidance only. Rust stable [timings](https://doc.rust-lang.org/cargo/reference/timings.html), [profiles](https://doc.rust-lang.org/cargo/reference/profiles.html), [cache](https://doc.rust-lang.org/cargo/reference/build-cache.html), [test selection](https://doc.rust-lang.org/cargo/commands/cargo-test.html), [sccache](https://github.com/mozilla/sccache/blob/main/docs/Rust.md) and [nextest archives](https://nexte.st/docs/ci-features/archiving/) underpin the proposed experiments.


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
| Kubernetes chart and distributed ingress | BN-023: review existing single/ha/fleet Helm implementation; no multi-replica/collector support inferred from chart values alone |
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
