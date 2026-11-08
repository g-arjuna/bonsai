# Bonsai backlog

Updated: 8 November 2026. **This is the only active product backlog.**
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
| BN-009 | P1 Partial | Sidecar/BonPy operational lifecycle: one event/DB contract, supervision/watchdog, health and backlog visibility, export catalog/schedules/quality, tracked jobs/retry/cancel, observable resource limits and one Bonsai UI entry. Reconcile existing functionality before building. Acceptance is a restart/overload soak with no silent data loss. 8 Nov intake reconciliation: registry/heartbeat/stale-lost, health gate, `/api/ml/{exports,schedules,jobs}` + cancel, job_engine memory shedding, Sidecars UI and rules-sidecar systemd unit all exist — build extends, not recreates ([intake](docs/merger/evidence/2026-11-08-bn009-intake/README.md)). Same day: `status_json` now carries cpu/rss/queue/uptime metrics end-to-end and `/api/sidecars` registry renders in the Sidecars UI ([evidence](docs/merger/evidence/2026-11-08-bn009-lifecycle/README.md)). Remaining: restart/overload soak acceptance with no silent data loss; live heartbeat exercised 8 Nov against packaged deployment — register/heartbeat/`status_json` metrics healthy, SIGTERM → stale (45s) → lost (120s) per registry thresholds; new gap found: sidecar ML/export jobs get unauthenticated 401s against the JWT-protected API (no credential path), and `last_heartbeat_at` renders null ([evidence](docs/merger/evidence/2026-11-08-bn009-live-heartbeat/README.md)). 9 Nov: `BONSAI_API_TOKEN` (`bsk_` key) auth wired through `bonsai_ml/api_auth.py` into all sidecar HTTP call sites + `BonsaiClient` env fallback; verified live on packaged deployment — 0 × 401 (was 21), scheduler jobs succeed with real `MlJobRun` records, schedules/exports writes land; `export_job` `quality_json` 422 bug fixed; `last_heartbeat_at` null confirmed a field-name expectation, not an API defect; new findings: `/api/graph/snapshot` 500 (missing DuckDB `TIMESTAMP_NS`, pre-existing) and pandas absent → empty parquet exports ([evidence](docs/merger/evidence/2026-11-09-bn009-api-auth/README.md)). Remaining: restart/overload soak acceptance with no silent data loss (shared design with BN-019). | EV1-2/4/5/6/9, D3-13/14, D4-9/13/21 |
| BN-010 | P2 Partial | Train and evaluate STGNN on versioned temporal graph datasets with chronological episode-level train/validation/test separation, clean-validation calibration, independent held-out faults, no-topology/no-temporal/cold-start ablations, event-level delay/false-alert metrics and model/checkpoint provenance. Integrate live graph telemetry and bounded continuous retraining only after offline acceptance; keep statistical fallback. | A00–A02/A15, R08, EV1-1/8, D3-9; BN-008/009 |
| BN-011 | P2 Intake | Integrate semantic/spectral embeddings and PCA display with live graph snapshots. Version feature schema/model, preserve stable entity IDs, fit transforms on training data, bound caches/jobs, expose stale/missing vectors. Validate text/syslog/config/CLI and topology embeddings independently; PCA is visualization, not evidence of anomaly quality. | EV1-3, D3-9, BN-008–010 |
| BN-012 | P2 Partial | Run a real licensed forecasting challenger against statistical baselines on held-out chronology with clean validation thresholds, measured inference/resource cost and fault/maintenance covariates. Record exact model/weights/license and promotion criteria; do not treat the current mean-shift surrogate as Chronos inference. | A14, R08; BN-009 |
| BN-013 | P1 Intake | Unified incident/evidence/operator workflow: grounded RCA, timelines, recommendation versus independently adjudicated correctness, feedback provenance, DB-controlled rules/playbooks and regression scenarios. Confirm current UI/rule support before replacing it. 8 Nov intake triage: Incidents/Investigations/Audit UI, investigation_runtime/trigger, playbook.rs, playbooks/library YAML rules, prefix-investigator diagnostics + adjudicate endpoint inventoried; gaps are grounded-RCA proof, adjudication loop, feedback provenance, DB rule control, incident→scenario regression gate ([intake](docs/merger/evidence/2026-11-08-bn013-intake/README.md)). First slice: trace one live incident journey end-to-end and record the break points. | A09–A11, D3-5/6/16, D4-4/8/15, EV1-7; BN-003/004/008 |
| BN-014 | P1 Partial | Real lab change assurance and read-only shadow pilot: collect baseline/candidate state from dedicated NOS instances, independent invariants and provenance, failures/recovery rehearsal and durable operator audit; report utility and failure taxonomy over real episodes. No autonomous live remediation acceptance until durable/signature gates and explicit authorization. | A16–A20, R09, D4-16; BN-001/002/006/013 |
| BN-015 | P2 Partial | Onboarding/inventory/security/config lifecycle: structured onboarding and actual gNMI E2E, credentials/SSH/vault/rotation, RBAC/LDAP, persisted device/exporter+RD ownership mappings and admin audit API now implemented without new access denials ([rollout contract](docs/architecture/ownership_mapping.md)); temporary compatibility access now enabled by default at user request; isolated query filtering/enforcement, trusted import provenance and assignment history remain, NetBox/rack enrichment, bounded read-only CLI adapters, config/UI authority and database backup/restore. Reconcile implemented legacy tasks through one operator journey. | D3-2/3/4/6, D4-3/7/13/14/17/18; BN-005/007 |
| BN-016 | P2 Intake | Complete multi-source telemetry and graph quality: vendor syslog/SNMP/MIB, suppression, flows/hosts/OTLP/TSDB, redundancy, optics/power/temperature, endpoint/service topology. Validate actual captures, normalization, sample-rate units, graph completeness and resource pressure per selected source. | D3-8/11/12/15, D4-1/2/5/6/10/12/20, Ubuntu DV2 carryovers; BN-008 |
| BN-017 | P3 Deferred | DDoS observation-only slice: corroborated signal/graph/rule features, incident timeline, reproducible benign/fault scenarios and false-positive evaluation. Cloud sink/BGP/RTBH response is separately deferred behind BN-001/002/014 and authorized execution scope. | DS-1–DS-8, especially DS-4; BN-008/013/016 |
| BN-018 | P3 Deferred | Routing security extensions: live RPKI ROA validation/cache lifecycle (fresh/stale/unavailable, RTR failure/reconnect), independent origin-validation cases and cache provenance. Keep RPKI origin validation distinct from BGPsec path-signature verification; BGPsec stays explicitly unverified until genuine wire/signature/trust-chain support and independent tests. | RustyBMP security roadmap; BN-003/007 |
| BN-019 | P1 Partial | Operational/release proof: install and upgrade, reproducible packaged build, dedicated lab browser acceptance, backups/recovery, collector limits/observability and sidecar soak. Reconcile legacy clean-slate and seven-day handoff criteria to the read-only prototype boundary. UI accessibility/bundle warnings resolved 8 Nov (zero-warning build, route-level code splitting; [evidence](docs/merger/evidence/2026-11-08-bn019-ui-warnings/README.md)); documentation link/provenance debt reviewed same day — 4 dead live-doc links fixed, 0 remaining ([evidence](docs/merger/evidence/2026-11-08-bn019-link-debt/README.md)). Remaining: the install/lab/soak gates. 8 Nov intake triage: Dockerfile/compose stack, backup/restore scripts, `bonsai-migrate`, `just verify-release` inventoried — gaps are executed install/upgrade, image build+boot, restore drill, soak (shared with BN-009); first slice proposed: packaged build + boot gate ([intake](docs/merger/evidence/2026-11-08-bn019-intake/README.md)). 8 Nov: packaged build + boot gate EXECUTED — image builds (Dockerfile fixed: protoc/mold/sccache, `CARGO_PROFILE_RELEASE_*` env so cook deps are reused: cook 12 s + bins 9 m vs prior 15–47 min double compiles), standalone profile boots both containers healthy, `/health` ok + all `/api/routing/v1/*` proxy 200 via JWT; container HEALTHCHECK fixed (`/readyz`, token probe for bonsai-routing). Backup→restore volume drill passed (project-prefix resolution fixed in both scripts). All planned BN-019 gates now have executed evidence; only the full-length soak window remains open (shared with BN-009). ([evidence](docs/merger/evidence/2026-11-08-bn019-packaged-build/README.md)). Same day: install-from-package + upgrade-in-place drill PASS — `scripts/install.sh` run end-to-end in an isolated Compose project (fresh clone @ `ec50410`, remapped ports, generated `.env`): image build → both containers healthy → JWT + all routing proxy endpoints 200; idempotent re-run is a no-op; forced-image upgrade recreates containers with graph-volume sentinel, admin login and routing API verified post-upgrade ([evidence](docs/merger/evidence/2026-11-08-bn019-install/README.md)). Same day: `bonsai-migrate` live drill PASS host-side — backup/migrate-jsonl/migrate-toml/rollback SHA-verified on a real lab FRR journal snapshot (2,890 v2 records, semantically-identical rewrite noted), synthetic v1 fixture (2 migrations, 2 `legacy_identity_incomplete` flags, 1 corrupt line skipped), legacy TOML vendor aliases, and corrupted-backup rollback fails closed; binary stays out of the image, quiesced-copy procedure documented ([evidence](docs/merger/evidence/2026-11-08-bn019-migrate/README.md)). Same day: packaged-deployment browser acceptance PASS — all 12 BN-003 Playwright journeys against `bonsai:latest` compose deployment with genuine FRR BMP (host-network routing for real exporter IPs; two defects handled: bridge SNAT speaker misattribution documented, first-run onboarding gate cleared via admin API) ([evidence](docs/merger/evidence/2026-11-08-bn019-packaged-browser/README.md)). Bounded 12-min packaged soak PASS (24/24 samples complete, 0 gaps/restarts, stable memory) + shared BN-009/BN-019 soak design; full-length soak still open ([evidence](docs/merger/evidence/2026-11-08-bn019-soak/README.md)). Legacy clean-slate/7-day handoff criteria reconciled to read-only prototype boundary — install/upgrade/backup/browser/migrate gates done; unattended-proof/chaos/governor criteria explicitly deferred to BN-009/014/016 ([evidence](docs/merger/evidence/2026-11-08-bn019-handoff/README.md)). | B16–B20, A20, D3-1/7/10, D4-19/21/22/23; BN-003/005–009 |

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
