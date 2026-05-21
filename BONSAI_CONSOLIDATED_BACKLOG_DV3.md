# BONSAI Consolidated Backlog — DV3
**Created**: 2026-05-18  
**Updated decisions**: 2026-05-20 (session 15 — signal-test-lab validation + code audit)  
**Baseline**: DV2 sprint complete, v0.2.0 shipped, 30-day data collection running on cloud VM (ubuntu@<ip>)  
**Laptop**: Full bare-metal wipe — simulates a new user install from scratch. Repo gone, Docker volumes gone, binary gone.  
**Scope of DV3**: Productionisation, clean install story, structured onboarding, enrichment testing, remediation maturity, AI investigations, and a long-overdue README rewrite.

### Decisions Closed in Sessions 13–14

| # | Decision | Outcome |
|---|---|---|
| S13-1 | Detection/Remediation priority write channel | `WriteCoordinator` split into normal + priority `mpsc` channels; `biased select!` ensures detection/remediation is never queued behind telemetry batches. (D25) |
| S13-2 | SubscriptionStatus batched independently | `sub_status_pending` buffer (128 cap) in coordinator; flushes on timer tick or full telemetry batch — not on every subscription renewal. (D26) |
| S13-3 | CorrelationBuffer: multi-source deduplication | `src/correlation_buffer.rs` — 45s window keyed by `(device, semantic_type, sub_key)`. `record()` returns `Absorbed` for duplicates. Sweep task + Prometheus counter. (D27) |
| S13-4 | Change-detection subscriber 8× capacity + DropOldest | `MpscSubscriber::new("change-detection", 2048, OverflowPolicy::DropOldest)`. (D28) |
| S13-5 | Python SDK bounded WindowRegistry | `max_entries=4096`, FIFO eviction, `evict_stale()`. (D29) |
| S13-6 | Python SDK `_last_fired` TTL eviction | `_evict_last_fired(now)` at start of `evaluate_graph()`. (D29) |
| S13-7 | Python multi-source event IDs in SDK + proto | `Features.source_event_ids`, `Detection.effective_source_event_ids`, `create_detection(source_event_ids=...)`, `repeated string source_event_ids = 9` in proto. (D27) |
| S14-1 | write_detection + write_remediation transactional | Both functions wrap Cypher ops in `BEGIN TRANSACTION / COMMIT`. (D25 follow-on) |
| S14-2 | Live UI 3-panel environment-agnostic refactor | `SiteRail.svelte` + `LiveStatusBar.svelte` (new); `Live.svelte` 3-column grid; `Topology.svelte` 30-role alias map + degree-percentile auto-tier + conditional BGP column; `Events.svelte` flex-fill + SSE reconnect + severity coloring + collapsible detail. (D30) |

### Decisions Closed in Session 10

| # | Decision | Outcome |
|---|---|---|
| S10-1 | Event feed filtering | `source_type` tag on `BonsaiEvent` + `StateChangeEvent`. New `/api/events/history` endpoint. Filter bar in `Events.svelte`. (D18) |
| S10-2 | Detection provenance | `DetectionEvent` gains `source_types`, `correlation_latency_ms`. `TRIGGERED_BY` becomes multi-edge. Incident card shows correlation chain + blast radius inline. (D19) |
| S10-3 | Topology completeness | `topology_handler` includes `HostEndpoint` nodes + `recent_event_count` per Device. `Topology.svelte` renders host nodes + event heatmap rings. (D20) |
| S10-4 | SNMP OID-to-graph correlation | `SnmpFactExtractor` with YAML OID patterns. `SnmpFact` published on bus. `join_snmp_fact()` joins to Interface/BgpNeighbor graph nodes. (D21) |
| S10-5 | Receiver supervisor + hot-reload | `ReceiverSupervisor` with `AbortHandle` per receiver. Port/enable changes apply immediately via restart, no process restart needed. Port conflict detection returns 409. (D22) |
| S10-6 | HTTP port + all listen addrs configurable | `http_addr` key added to `bonsai.toml`. HTTP UI port no longer hardcoded. All ports documented in `bonsai.toml.example`. BMP default changed to 10179. (D23) |
| S10-7 | Collector health propagation | Enriched heartbeat proto (queue depth, uptime, receiver statuses, resource metrics). DiagnosticState wired correctly. Core Collectors UI shows true per-collector state. (D24) |

### Decisions Closed in Session 9

| # | Decision | Outcome |
|---|---|---|
| S9-1 | NetFlow exporter identity | `TelemetryUpdate.target` = exporter IP (the switch/AP), not the flow src IP. `NetflowRecord` gains `exporter_address` field. `CARRIES_FLOW` edge from Device to AppFlow. (D13) |
| S9-2 | HostEndpoint node semantics | Always optional, arch-agnostic. SP deployments = zero HostEndpoints. kind field drives display only, not logic. (D14) |
| S9-3 | Streaming config ownership | Core owns streaming config. Collectors poll `GET /api/settings/streaming` from Core at startup and every 60s. (D15) |
| S9-4 | Streaming signals GUI | New `/settings` route in Svelte SPA. One card per receiver protocol. PATCH writes delta to bonsai.toml. (D16) |
| S9-5 | OTLP span → graph | `write_otlp_span` upserts Application node + RUNS_SERVICE edge when peer_address matches a Device or HostEndpoint. (D17) |

### Decisions Closed in Session 8

| # | Decision | Outcome |
|---|---|---|
| S8-1 | AI provider implementation order | **Gemini first, then Moonshot** — both use free-tier/low-cost APIs. Anthropic/OpenAI deferred. |
| S8-2 | Onboarding UI architecture | **Retire `Setup.svelte` entirely.** `Onboarding.svelte` becomes the single entry-point with a `first_run` mode for environment/site/cred setup. |
| S8-3 | Laptop wipe scope | **Full bare-metal wipe** — no repo, no Docker, no bonsai binary. Fresh install flow is the test. |
| S8-4 | NetBox version support | **Both 3.x and 4.x.** Version detected at runtime from `GET /api/` meta endpoint. Configurable as `netbox_version = "auto"` in bonsai.toml. |
| S8-5 | event_detection.rs status | **Already retired (believed done).** Confirm with grep — if the file still exists as dead code, remove it. D3-9 T1 becomes a verification task, not a build task. |

---

## Context & Decisions Carried Forward

| Decision | Status |
|---|---|
| Two labs = redundant chaos signal for GNN; cloud DC is the single collection source | CLOSED — laptop wiped (bare) |
| Rust binary + Python sidecar = current distribution; no native installer exists yet | GAP → D3-1 |
| Onboarding: Setup.svelte retired; Onboarding.svelte is sole entry-point with first_run mode | CLOSED S8-2 → D3-2 |
| Discovery/gNMI readiness endpoints exist; no end-to-end lab-backed test harness | GAP → D3-3 |
| Enrichment enrichers: NetBox + ServiceNow both coded; no structured test suite | GAP → D3-4 |
| Remediation approvals UI built; no proactive detection→proposal auto-trigger flow | GAP → D3-5 |
| AI: Gemini first, Moonshot second (free tier). Anthropic/OpenAI deferred. | CLOSED S8-1 → D3-6 |
| README.md is empty | CRITICAL → D3-7 |
| MCP server is read-only; no write-back from AI proposals | GAP → D3-6 |
| Graph enrichment from CLI scraping coded in enricher; rack/PDU/optical nodes still open from DV2 | CARRY → D3-8 |
| GNN: 30d data accumulation in progress; event_detection.rs believed retired — verify | CARRY → D3-9 |
| NetBox: dual 3.x/4.x support, version auto-detected from API meta, configurable | CLOSED S8-4 → D3-4 |

---

## Epic Map

```
D3-1  Clean Install & Distribution                   [⚠️ T1/T4 DONE Session 19 (install.sh + Makefile); T2 DONE Session 17; T3 pre-existing; T5 manual validation pending]
D3-2  Structured Onboarding (UI-first, NetBox import, cred association) [⚠️ T1/T2/T3/T5/T7 pre-existing; T4/T6 DONE Session 19 (bulk import + multi-cred apply)]
D3-3  gNMI Onboarding Flow — Lab-Backed End-to-End Test [⏳ NOT STARTED]
D3-4  Graph Enrichment — CLI Scraping + NetBox + Rack Validation [⏳ NOT STARTED]
D3-5  Remediation Maturity — Auto-proposal + HITL Flow [✅ ALL DONE Session 17]
D3-6  AI Investigations — Provider Integration + Key Management [✅ T1-T8 ALL DONE Session 17]
D3-7  README & Documentation Refresh                 [⚠️ T2 DONE Session 17 (toml.example + README AI env var); T1/T3/T4/T5 remain]
D3-8  Graph Node Completeness (DV2 carry-overs)      [✅ ALL DONE Session 17]
D3-9  GNN Production Readiness                       [⚠️ T1/T3/T4 DONE Session 18; T2/T5 data-dependent]
D3-10 Developer Experience & CI Health               [⚠️ T1/T2/T3/T4 DONE Session 18; T5 (Playwright) remains]
D3-11 Streaming & Endpoint Graph Completeness        [✅ ALL TASKS DONE — Session 9-10]
D3-12 Signal Observability, Detection Provenance & Topology Completeness
      [⚠️ PARTIAL: H2/J DONE in code; G1 source_type missing; G2/G3 filter UI pending]
D3-13 Receiver Supervisor, Hot-Reload & Full Port Configurability
      [⚠️ PARTIAL: T1/T2 ReceiverSupervisor+migration DONE; T6 http_addr DONE;
       K3 PATCH→restart NOT wired; K4 syslog/snmp settings absent; K5 live status pending]
D3-14 Collector Health & Telemetry Propagation to Core
      [⚠️ PARTIAL: T1-T3/T5-T7/T9-T11 DONE; T4/T8 receiver_statuses proto pending]
D3-15 Scalability & Multi-Source Correlation         [✅ ALL TASKS DONE — Session 13-14]
D3-16 Live UI: Environment-Agnostic 3-Panel Refactor [✅ ALL TASKS DONE — Session 14]
```

### Session 15 Findings (2026-05-20) — signal-test-lab validation run

| Finding | Impact | Action |
|---------|--------|--------|
| `emit_oper_status_event` bypassed CorrelationBuffer — interface_down never fired | CRITICAL | **FIXED** — `write_state_change_event` with interface_down/up event types |
| Nokia SRL syslog "is now down" not matched by existing regex | HIGH | **FIXED** — new pattern in nokia-srlinux.yaml |
| SNMP orphan BGP trap dedup fails: trap encodes device's own connection IP not BGP peer | MEDIUM | KNOWN GAP — documented in guide S-44 |
| mode=all `run_collector_manager` never starts — 0 collectors register | HIGH | KNOWN GAP — in `server_startup.rs` if-block; needs fix in D3-13 |
| AppFlow/CARRIES_FLOW: SRL exports sFlow (not NetFlow/IPFIX); linux-host1 not a Device | LOW | KNOWN GAP — documented in guide S-38 |

---

## D3-1 — Clean Install & Distribution

### Problem
There is no single entry-point for a user who wants to install and run bonsai on a fresh machine. The current paths are:
- Build from source (`cargo build --release`) — requires Rust, clang, cmake, lbug.
- Docker Compose — works but requires Docker + ContainerLab knowledge + `.env` setup.
- No binary release, no Homebrew tap, no `install.sh`.

The result is that a real network engineer with Python knowledge but no Rust background cannot get started without pain.

### Goals
1. One-command install that produces a working bonsai on macOS (Apple Silicon) or Linux (x86-64/aarch64).
2. Clear decision matrix: Docker vs native binary vs dev mode — documented, not guessed.
3. `bonsai.toml.example` must be non-empty and have every field commented.
4. The install experience must be validated by actually wiping the laptop and going through it fresh — this IS the acceptance test.

### Tasks

| ID | Task | Priority | Notes |
|---|---|---|---|
| D3-1 T1 | **`install.sh` bootstrap script** | HIGH | Downloads pre-built binary from GitHub Releases OR builds from source if no binary available. Handles: Rust toolchain check, lbug .so placement, vault passphrase prompt, UI dist copy. Single command: `curl -sSf https://... \| bash` |
| D3-1 T2 | **`bonsai.toml.example` populated** | HIGH | Every config section with inline comments explaining each field. Currently empty (gitignored content not committed). |
| D3-1 T3 | **Docker quick-start path rationalised** | MEDIUM | `.env.example` → `.env` → `docker compose --profile dev up` should produce a fully running bonsai with the UI reachable. Currently requires manual `clab deploy` for the management network. Add a `docker compose --profile standalone` that skips `bonsai-mgmt` external network requirement for non-lab installs. |
| D3-1 T4 | **Makefile or `justfile` for common dev tasks** | LOW | `make dev`, `make test`, `make docker`, `make ui` |
| D3-1 T5 | **Fresh-install validation run** | HIGH | After wipe: follow `install.sh` → configure `bonsai.toml` → `docker compose --profile standalone up` → open UI → complete onboarding wizard. Document every friction point encountered. This drives D3-2 and D3-7. |

### Code Issues Identified
- `docker-compose.yml` line 235: `bonsai-mgmt` network is `external: true` with no fallback — this causes compose to refuse to start if ContainerLab has not pre-created the network. A `standalone` profile must use a non-external network.
- `Cargo.toml` is gitignored/empty in this view — if it is actually missing version bumps or dev-dep cleanup, that should be reviewed.
- `.env.example` instructions reference `scripts/generate_compose_tls.sh` before TLS is needed — confusing for first-run users who just want to see the UI.

---

## D3-2 — Structured Onboarding (UI-first + NetBox import + cred association)

### Problem
The current onboarding flow (`Setup.svelte` → `Onboarding.svelte`) works but:
1. It is a wizard-within-wizard design that is confusing in practice.
2. Manual device entry requires IP, vendor, role, site, credential — all filled in blind (no discovery before adding).
3. There is no NetBox import path in the UI (enrichment is post-onboarding only).
4. Credential association to a device is implicit via a text alias — there is no picker or validation.
5. No bulk import (CSV, JSON paste, or API-driven).
6. `Setup.svelte` skips straight to "Add my first device" without showing what a device needs before gNMI works.

### Goals
1. **Single onboarding entry-point**: `Setup.svelte` is retired. `Onboarding.svelte` gains a `first_run` prop — when true, it prepends Environment + Site + Credential steps before device import.
2. Wizard experience: Environment → Site(s) → Import devices (manual | NetBox | CSV) → Associate credentials → Discover gNMI → Confirm subscriptions.
3. NetBox import at onboarding time (not just enrichment). Supports NetBox 3.x and 4.x (version auto-detected).
4. Credential picker on device add — dropdown of existing aliases plus inline "add new" flow.
5. Post-add readiness badge per device: `gNMI OK` / `TLS issue` / `Credential missing`.

### Tasks

| ID | Task | Priority | Notes |
|---|---|---|---|
| D3-2 T1 | **Retire `Setup.svelte`; add `first_run` mode to `Onboarding.svelte`** | HIGH | Remove `Setup.svelte` and its route. Add `let { first_run = false } = $props()` to `Onboarding.svelte`. When `first_run = true` (detected via `is_first_run` from `/api/setup/status`), steps 1-3 are Environment / Site / Credentials before device import. `App.svelte` renders `<Onboarding first_run={true}>` on first run instead of `<Setup>`. |
| D3-2 T2 | **NetBox import at onboarding** | HIGH | New step: "Import from NetBox" — user pastes NetBox URL + API token. Backend calls NetBox `/api/dcim/devices/?status=active` (3.x) or `/api/dcim/devices/?status=active` (4.x — same path, different meta). Version auto-detected from `GET /api/` → `data.netbox-version`. Returns device list with IP, site, role pre-populated. User selects which ones to onboard. |
| D3-2 T3 | **Credential picker on device forms** | MEDIUM | `Credentials.svelte` and `Onboarding.svelte` device form: replace free-text alias with a `<select>` of existing vault aliases plus an inline "+ Add credential" modal. |
| D3-2 T4 | **Bulk import: CSV + JSON paste** | MEDIUM | New endpoint `POST /api/onboarding/bulk` accepts `[{address, hostname, vendor, role, site_id, credential_alias}]`. UI provides a CSV paste box and JSON textarea. |
| D3-2 T5 | **Post-onboard readiness badge** | HIGH | After device is added, trigger `/api/devices/{address}/gnmi-readiness` immediately and show `gNMI OK`, `TLS issue`, `Auth failed`, or `Unreachable` badge inline in the devices list. |
| D3-2 T6 | **Multi-device credential association** | MEDIUM | Allow one credential alias to be assigned to multiple devices in a single action. Needed for fleet onboarding (all devices share the same management credential). |
| D3-2 T7 | **Remove `Setup.svelte` dead file and route** | HIGH | Delete `ui/src/routes/Setup.svelte`. Remove its import from `App.svelte`. Remove `showSetup` / `setupChecked` state vars from `App.svelte`. Clean up `/api/setup/status` call — this endpoint can be repurposed to return `{is_first_run}` which `Onboarding` reads directly. |

### Code Issues Identified
- `Onboarding.svelte` has `STEPS = [{id:1, label:'Identity'}, ..., {id:4, label:'Confirm'}]` — identity step conflates adding a device and setting its credential as a single form, which fails when creds are added in a separate vault flow first. These should be decoupled.
- `Setup.svelte` `skipSetup()` creates a default Home Lab environment silently — no audit trail, no feedback to user. Should toast a confirmation.
- `managed_devices.rs` `add_managed_device_handler` does not check whether the credential alias exists in the vault before accepting the request — device gets added with a dangling alias. Should validate on creation.
- `Setup.svelte` integrations step only covers ServiceNow — NetBox is not offered during setup even though NetBox enricher is fully coded.

---

## D3-3 — gNMI Onboarding Flow: Lab-Backed End-to-End Test

### Problem
The gNMI readiness pipeline (`discovery.rs`, `device_gnmi_readiness_handler`, `device_streaming_readiness_handler`, `device_recommendations_handler`) is coded but has never been exercised end-to-end against a live ContainerLab topology in a controlled test.

Specifically:
- CLI scraping recommends gNMI paths based on config → path is shown in UI → user selects paths → subscription fires. This full chain has not been tested with a real lab device start-to-finish.
- The `device_reparse` endpoint exists but is not wired into any CI gate.
- Streaming readiness report (`DeviceStreamingReadinessResponse`) has `blockers` and `recommended_actions` fields but UI (`Onboarding.svelte`) only shows a text list — no actionable CTA per blocker.

### Goals
1. Spin up a ContainerLab (6-node Nokia SRL) locally.
2. Populate NetBox with device details.
3. Run the full manual onboarding flow: enter device → discover → review recommendations → select paths → confirm subscription active.
4. Run the NetBox import flow: import devices from NetBox → auto-associate existing credential → confirm gNMI.
5. Assert graph nodes created, LLDP links discovered, BGP sessions visible.

### Tasks

| ID | Task | Priority | Notes |
|---|---|---|---|
| D3-3 T1 | **NetBox seed script with lab topology data** | HIGH | Extend `scripts/seed_netbox.py` to populate all 6 SRL nodes with: correct mgmt IP, site, rack position, platform=nokia-srl, role. Currently the seed script exists but populates minimal data. |
| D3-3 T2 | **End-to-end onboarding test script** | HIGH | New `scripts/e2e_onboarding_test.sh` — spins lab, runs manual onboarding via API curl calls, checks graph for expected nodes/edges, checks subscription statuses, exits PASS/FAIL. |
| D3-3 T3 | **Blocker → CTA in UI** | MEDIUM | In `Onboarding.svelte` gNMI readiness step: for each `blocker` in the readiness report, show a specific action button (e.g. "Upload CA cert", "Check TLS domain"). |
| D3-3 T4 | **Verify path selection flow** | MEDIUM | After profile is selected and paths saved, verify that `SubscriptionStatus` entries appear for the device within 30s. Test script checks `/api/devices/{address}` for `subscription_statuses` non-empty. |
| D3-3 T5 | **Credential pre-test in discovery flow** | MEDIUM | `discover_handler` in `managed_devices.rs` should test the credential (gNMI Capabilities call) before returning recommendations. If auth fails, `DiscoveryReport.warnings` should include "Authentication failed — check credential alias". |

### Code Issues Identified
- `discovery.rs` `discover_device()` does not verify TLS if `ca_cert_path` is None but device uses TLS — it falls through to a misleading "vendor_detected: unknown" rather than a clear TLS error in `warnings`.
- `Onboarding.svelte` step 2 (Discovery) fires the discovery API on button click but does not debounce — rapid clicks can queue multiple in-flight discovery requests for the same device.
- `device_gnmi_readiness_handler` in `device.rs` re-runs a full gNMI Capabilities check every call with no caching — expensive for the UI polling pattern.

---

## D3-4 — Graph Enrichment: CLI Scraping + NetBox + Rack Validation

### Problem
Enrichment works in isolation (enricher registry, NetBox REST client, ServiceNow CMDB sync are all coded). However:
1. CLI scraping enrichment (`parser_chain_enricher.rs`) has never been tested against a live device config in a lab context.
2. NetBox enricher populates `netbox_*` properties but rack-level graph nodes (DV2 D2-5) are still pending.
3. There is no test that verifies the graph shows NetBox-sourced site → rack → device hierarchy.
4. The enrichment UI (`Enrichment.svelte`) shows enricher status but no per-device enrichment property inspector linked to the graph.

### Tasks

| ID | Task | Priority | Notes |
|---|---|---|---|
| D3-4 T0 | **NetBox 3.x / 4.x dual support** | HIGH | CLOSED S8-4. In `netbox.rs`: on enricher init, call `GET {base_url}/api/` → parse `data["netbox-version"]`. If semver ≥ 4.0, use 4.x API paths (no changes yet known but pagination shape differs). Store detected version in `NetboxEnricher` struct. Log at init. Config key: `extra.netbox_version = "auto"` (auto-detect, default) or `"3"` / `"4"` (pin). |
| D3-4 T1 | **Rack graph node + rack_member edges** | HIGH | DV2 carry-over D2-5 T1. Add `Rack` node type to `graph/mod.rs`. NetBox enricher writes `RACK_MEMBER` edge from Device to Rack. `SiteRecord` already has no rack field — Rack is a peer node, not a sub-field of Site. |
| D3-4 T2 | **NetBox enricher: onboarding import endpoint** | HIGH | Allow NetBox enricher to be invoked at onboarding time. New API endpoint `POST /api/enrichment/netbox/import` that takes `{url, token, site_slug, netbox_version}` and returns a list of importable devices. Used by D3-2 T2. |
| D3-4 T3 | **CLI config scraping test against live lab** | HIGH | `scripts/e2e_netbox_enricher_test.sh` already exists — extend it to also run parser_chain_enricher against the lab DC and assert that `config_*` properties are written to Device nodes. |
| D3-4 T4 | **Device enrichment inspector in UI** | MEDIUM | `DeviceDrawer.svelte` already has an enrichment section — ensure it calls `/api/devices/{address}/enrichment` and renders grouped properties by source (netbox, config_scrape, servicenow). Currently the endpoint exists but drawer rendering is minimal. |
| D3-4 T5 | **HostEndpoint from LLDP** | MEDIUM | DV2 carry-over D2-6 T1. `HostEndpoint` node representing a non-managed server/host discovered via LLDP. Write from `ingest.rs` when LLDP neighbor has no matching Device node in the graph. |
| D3-4 T6 | **PDU SNMP receiver** | LOW | DV2 carry-over D2-5 T3. SNMP trap receiver for PDU power events. Adds `PowerUnit` node + `POWERED_BY` edges. Low priority until 30d collection is complete. |

### Code Issues Identified
- `netbox.rs` queries `/api/dcim/devices/?limit=200` with a hard-coded limit — if the NetBox instance has >200 devices it silently truncates. Must paginate using `?offset=` until `next` is null.
- `parser_chain_enricher.rs` calls the pyats sidecar synchronously inside an async task without a timeout guard. If the sidecar is unreachable, the enricher task hangs indefinitely.
- `enrichment/registry.rs` stores enricher configs as `Vec<EnricherConfig>` but has no dedup check — if the same enricher name is configured twice in `bonsai.toml`, both run, doubling write side-effects.
- `Enrichment.svelte` "Run now" button calls `POST /api/enrichment/{name}/run` but has no timeout feedback — if the enricher is slow, the UI shows a spinner indefinitely with no abort option.

---

## D3-5 — Remediation Maturity: Auto-Proposal + Human-in-the-Loop Flow

### Problem
Remediation is architecturally complete (trust model, approval flow, rollback, graduation all coded). However:
1. There is no automatic detection → proposal creation path. Currently proposals are only created by explicit `POST /api/approvals` calls (from test scripts or manual API calls). The graph detects a BGP down event but nothing creates a remediation proposal for it.
2. The Approvals UI shows proposals but the operator has no context beyond raw JSON steps — no plain-English explanation of what the playbook will do.
3. Rollback is wired but the rollback window (time-gated) is not clearly shown in the UI.
4. There is no audit trail of "who approved what and when" visible in the UI.

### Goals
1. When a detection fires (DetectionEvent node written), the system automatically creates a pending `RemediationProposal` if a matching playbook exists.
2. Approvals page shows human-readable playbook description alongside raw steps.
3. Operator sees a rollback countdown timer on recently-approved proposals.
4. Audit log is queryable from the UI.

### Tasks

| ID | Task | Priority | Notes |
|---|---|---|---|
| D3-5 T1 | **Auto-proposal trigger from detection events** | HIGH | In `change_detection.rs` (or event_bus subscriber), after a DetectionEvent is written, look up matching playbook(s) for `rule_id`. If found, call `write_remediation_proposal()` automatically. New config key `[remediation] auto_propose = true` guards the feature (default false initially). |
| D3-5 T2 | **Playbook catalogue with human-readable descriptions** | HIGH | Each playbook TOML/YAML gets a `description` and `risk_level` field. `Approvals.svelte` renders `p.description` as a prominent card subtitle so operators understand what they're approving without reading raw JSON steps. |
| D3-5 T3 | **Rollback countdown in Approvals UI** | MEDIUM | For proposals in `approved` state with an active rollback window, show a live countdown (e.g. "Rollback window: 4m 32s remaining"). `active_rollbacks` is already returned by the API. |
| D3-5 T4 | **Audit trail workspace** | MEDIUM | New `Audit` route in UI (linked from sidebar). Calls `/api/audit/export?since=&until=` (endpoint exists in `audit.rs`). Renders a time-sorted table of operator actions: credential resolves, approvals, rejections, graduations. |
| D3-5 T5 | **Proposal context enrichment** | MEDIUM | Before showing a proposal in Approvals, fetch the original DetectionEvent and show its `features_json` in a readable summary: "BGP peer 10.0.0.1 went down on leaf-01 at 14:32:01" rather than raw keys. |
| D3-5 T6 | **Validate playbook definitions** | LOW | Extend `scripts/validate_playbooks.py` to check that all `gnmi_set` steps have valid path syntax and that all referenced credential aliases exist in the vault. Run as part of `rebuild_and_validate.sh`. |

### Code Issues Identified
- `approvals_approve_handler` in `remediation.rs` executes proposal steps synchronously in the HTTP handler (line 273: `execute_proposal_steps(...).await`). For multi-step playbooks this can block the HTTP worker for seconds. Should be moved to a background task with SSE status updates.
- `write_remediation_proposal()` in `graph/mod.rs` does not check for duplicate proposals (same detection_id + playbook_id). If called twice (e.g. retry), it creates duplicate rows. Add an `ON MATCH DO NOTHING` or unique constraint check.
- The trust graduation logic in `check_graduation()` uses `consecutive_approvals_required` from config but this field is not present in `docker/configs/all.toml` example — runtime default is fine but operators don't know to set it. Add it to `bonsai.toml.example`.
- `Approvals.svelte` calls `window.prompt()` for operator notes — a native browser dialog that cannot be styled or pre-populated. Replace with an inline modal.

---

## D3-6 — AI Investigations: Provider Integration + Key Management

### Problem
The `Investigations` page exists and the `InvestigationRecord` / `ToolCallRecord` data model is in the graph. The MCP server (`mcp_server.rs`) exposes read-only tools. However:
1. There is no AI provider integration — bonsai cannot actually call an LLM. The investigation flow creates a record but cannot run any reasoning.
2. There is no API key management in the UI. Operators cannot configure Anthropic / OpenAI / Gemini / Moonshot keys from the UI.
3. The `create_investigation_handler` creates a record but immediately sets status to `"failed"` because no AI backend is wired.
4. The MCP server tools are excellent primitives (`get_incident`, `query_devices`, `blast_radius`, `query_graph`) — an AI using these over a agentic loop would produce high-quality network diagnostics. The wiring just does not exist.

### Goals
1. Operator configures an AI provider (Anthropic / OpenAI / Gemini / Moonshot) + API key from the UI settings page.
2. When an investigation is created (manual or auto), bonsai spawns an async agent that uses the MCP tools in a loop until it reaches a conclusion.
3. Each tool call is persisted as a `ToolCallRecord` so the reasoning trail is visible in the UI.
4. The agent produces a `summary` and optionally a `proposal_json` (remediation playbook proposal).
5. Gemini 2.5 Pro / Claude 4 / GPT-4o should all work via a provider-abstracted client.

### Tasks

| ID | Task | Priority | Notes |
|---|---|---|---|
| D3-6 T1 | **AI provider config in bonsai.toml** | HIGH | New `[ai]` section: `provider = "gemini"` (default), `model = "gemini-2.5-pro"`, `api_key_env = "BONSAI_AI_API_KEY"`, `per_investigation_budget_usd = 0.10`, `daily_budget_usd = 1.00`. Supported providers (in order): `gemini`, `moonshot`, `anthropic`, `openai`. Free-tier-first. |
| D3-6 T2 | **AI provider abstraction trait (Rust)** | HIGH | New `src/ai_provider.rs` — `AiProvider` trait with `complete(messages, tools) -> Result<AiResponse>`. **Implement first**: `GeminiProvider` (calls `generativelanguage.googleapis.com/v1beta/models/{model}:generateContent`) with function calling support. **Second**: `MoonshotProvider` (calls `api.moonshot.cn/v1/chat/completions` — OpenAI-compatible schema). `AnthropicProvider` and `OpenAiProvider` as stubs, enabled later. |
| D3-6 T3 | **Agent loop in investigation runtime** | HIGH | New `src/investigation_runtime.rs` — async task spawned by `create_investigation_handler`. Loop: call LLM with current context + MCP tool definitions → parse tool calls → execute via existing MCP tool handlers → append results → repeat until `stop_reason = end_turn` or budget exceeded. Write `ToolCallRecord` after each tool call. |
| D3-6 T4 | **API key management in UI** | HIGH | Since `Setup.svelte` is retired, add an "AI" step to the `Onboarding.svelte` first_run flow (step 5, after Credentials). Also reachable at any time from a settings panel (gear icon in sidebar). Fields: provider selector (`gemini` / `moonshot` / `anthropic` / `openai`), model name, API key input (stored in vault under alias `bonsai-ai-key`, never returned to UI), per-investigation and daily budget caps. Test-connection button calls new `POST /api/ai/test` endpoint. |
| D3-6 T5 | **Grounded incident handler wired to AI** | MEDIUM | `grounded_incident_handler` in `remediation.rs` currently exists — wire it to the new AI provider so it can be called from the MCP `get_incident` tool to produce a grounded narrative. |
| D3-6 T6 | **Auto-trigger investigation on unmatched detections** | MEDIUM | When a DetectionEvent fires for a `rule_id` that has no matching playbook (i.e. no auto-proposal), automatically trigger an investigation with `trigger = "auto"`. Config: `[ai] auto_investigate_unmatched = true`. |
| D3-6 T7 | **Investigation context builder** | MEDIUM | Before the first LLM call, build a structured context prompt: device summary, recent state changes, blast radius, LLDP topology snippet, current BGP state. Reduces LLM hallucination and tool call count. |
| D3-6 T8 | **Cost tracking + daily budget gate** | MEDIUM | Track `tokens_used` and `cost_usd` per investigation (already in `InvestigationRecord`). Accumulate daily across all investigations. Gate: if daily budget exceeded, new investigations queue but do not fire until next UTC day. |

### Code Issues Identified
- `Investigations.svelte` `triggerForm` requires a `detection_id` to be typed manually — operators don't know the UUID of the detection. The trigger form should show a searchable dropdown of recent detections.
- `create_investigation_handler` body struct `CreateInvestigationBody` has a `trigger` field with `default_trigger = "auto"` but the UI hardcodes `trigger: 'operator'` — inconsistency. The UI default should match.
- `InvestigationRecord.proposal_json` stores a raw JSON string — if the AI produces an invalid JSON proposal the UI `JSON.parse()` call on line 188 of `Investigations.svelte` will throw and the entire detail panel breaks. Wrap in a try/catch with a fallback raw text view.
- The MCP tools `query_graph` passes raw Cypher from the LLM directly to the graph DB — this is a read-only passthrough but there is no sanitisation or query complexity limit. A runaway Cypher query could spike CPU. Add a `LIMIT` injection and query timeout.

---

## D3-7 — README & Documentation Refresh

### Problem
`README.md` is empty. `DECISIONS.md` is empty. `bonsai.toml.example` is empty. For anyone not in the original development thread, the project is a black box.

### Goals
1. `README.md` is the primary landing page: what bonsai is, who it is for, how to get started in 5 minutes, architecture diagram (ASCII or Mermaid), link map to other docs.
2. `bonsai.toml.example` has every section with inline comments.
3. `DECISIONS.md` captures the key architectural decisions (graph DB choice, Rust+Python split, MCP server, trust model, etc.).
4. All API endpoints documented (OpenAPI already generated at `/openapi.json` via `mcp_routes.rs` — README should point to it).

### Tasks

| ID | Task | Priority | Notes |
|---|---|---|---|
| D3-7 T1 | **README.md — full rewrite** | CRITICAL | Sections: Overview, Architecture, Quick Start (Docker), Quick Start (Native), Configuration, Workspaces (UI guide), API, Python SDK, GNN, Contributing. |
| D3-7 T2 | **bonsai.toml.example** | HIGH | Every field in `config.rs` documented with: default value, valid values, when to change it. Covers runtime, collector, graph, retention, enrichment, remediation, ai, gnn, signals. |
| D3-7 T3 | **DECISIONS.md** | MEDIUM | Capture: lbug graph DB rationale, Rust-first with Python ML sidecar, two-binary (bonsai + healthcheck), MCP server for AI tool use, trust model for autonomous remediation, environment/archetype system. |
| D3-7 T4 | **Architecture ASCII diagram** | MEDIUM | In README: shows data flow from gNMI telemetry → collector → graph → change detection → remediation/investigation. |
| D3-7 T5 | **Workspace user guide** | LOW | `docs/ui/` folder with one Markdown per UI workspace: Live, Incidents, Devices, Onboarding, Enrichment, Approvals, Investigations, Explorer. |

### Code Issues Identified
- `src/http_server/mcp_routes.rs` generates an OpenAPI schema at `/openapi.json` but the schema is built from hand-coded structs in `schema.rs` (77 KB). If a new endpoint is added without updating `schema.rs`, it silently disappears from the docs. Consider adding a CI check that diffs `/openapi.json` against a golden file.
- `src/mcp_server.rs` `RULE_CATALOGUE` is a static array of rules — but the Python-side `bonsai_sdk/rules/` directory is the authoritative source of rule IDs. These can drift. Add a test that cross-checks the Rust catalogue against the Python rule classes.

---

## D3-8 — Graph Node Completeness (DV2 Carry-overs)

These are items that were deferred in DV2 and are needed for a complete network graph representation.

| ID | Task | Priority | Source |
|---|---|---|---|
| D3-8 T1 | **config_change_event emission** | HIGH | DV2 D2-4 T2. `event_bus.rs` should emit a `config_change` BonsaiEvent when `ConfigChangeEvent` nodes are written. Needed for SSE stream completeness. |
| D3-8 T2 | **EntityIdentity node + upsert routing** | HIGH | DV2 D2-9 T1/T3. A normalised identity node that links a device's hostname, chassis ID, and management IP. Prevents duplicate Device nodes when the same device is seen via gNMI (by IP) and LLDP (by chassis ID). |
| D3-8 T3 | **Rack graph node + rack_member edges** | HIGH | DV2 D2-5 T1. `Rack` node. `(:Device)-[:RACK_MEMBER]->(:Rack)-[:IN_SITE]->(:Site)`. Populated by NetBox enricher from `rack.name`. |
| D3-8 T4 | **HostEndpoint from LLDP** | MEDIUM | DV2 D2-6 T1. Non-managed hosts discovered via LLDP. `HostEndpoint` node with `chassis_id`, `mgmt_ip`, `system_name`. Written from `ingest.rs` LLDP path when no Device node matches. |
| D3-8 T5 | **OpticalChannel graph node** | LOW | DV2 D2-7 T1/T2. `OpticalChannel` node for optical transport monitoring. gNMI/SNMP receiver for DWDM parameters (power, frequency, SNR). |
| D3-8 T6 | **PDU SNMP receiver** | LOW | DV2 D2-5 T3. `PowerUnit` node + `POWERED_BY` edges from PDU SNMP traps. |

### Code Issues Identified
- `graph/mod.rs` device upsert logic: `upsert_device()` in `common.rs` uses `MERGE (d:Device {address: $addr})` — if the same physical device has two management IPs (e.g. IPv4 and IPv6), two separate Device nodes are created with no link. D3-8 T2 (EntityIdentity) directly resolves this.
- `event_bus.rs` `emit()` broadcasts to in-process receivers only — there is no persistence of events that fired while no receiver was subscribed. On server restart, the first 30s of events are silently dropped. Consider a small ring buffer (last 1000 events) queryable via `/api/events/recent`.
- `ingest.rs` LLDP ingest creates `Interface` nodes for both local and remote ports but does not create a `HostEndpoint` for the remote side when it is not a managed device. This is the root gap for D3-8 T4.

---

## D3-9 — GNN Production Readiness

### Problem
The GNN pipeline (Python `bonsai_ml/gnn/`) is architecturally complete with model, calibration, eval, and training scripts. The 30-day data collection on the cloud VM is running to accumulate training data. But:
1. The GNN trigger in the Rust binary is gated by `event_detection_retirement_gate.sh` — not yet passed on Ubuntu (DV2 D2-1 T1 is still open).
2. `archive_to_training.py` converts archive data to training samples — but the conversion pipeline has never been run on real (non-synthetic) data.
3. `calibration.py` mode produces `gnn_calibration_scores` in the graph but the Operations UI has no calibration score viewer.
4. GNN inference produces anomaly scores, but these are not linked to investigations — when GNN fires an anomaly, it should optionally trigger an AI investigation (D3-6 T6).

### Tasks

| ID | Task | Priority | Notes |
|---|---|---|---|
| D3-9 T1 | **Verify event_detection.rs retirement** | HIGH | S8-5: believed already retired. Action: `grep -r 'event_detection' src/` — if the file exists as dead code or is still referenced, remove it and update `lib.rs`. If already gone, mark this task DONE and remove D2-1 T1 references from scripts. Gate: `event_detection_retirement_gate.sh` should already pass. |
| D3-9 T2 | **Archive → training pipeline smoke test** | HIGH | After 7 days of cloud collection, run `python archive_to_training.py` against the real archive and verify: samples are generated, feature schema matches `feature_schema.py`, no NaN values. |
| D3-9 T3 | **Calibration score viewer in Operations UI** | MEDIUM | `Operations.svelte` has daily check and weekly trend. Add a "GNN Calibration" panel showing: score distribution histogram (last 24h), P95 score, threshold recommendation. Data from `GET /api/operations/gnn-calibration` (new endpoint). |
| D3-9 T4 | **GNN anomaly → investigation auto-trigger** | MEDIUM | When GNN inference produces an anomaly score above threshold in production mode, emit a DetectionEvent with `rule_id = "gnn_anomaly"`. D3-6 T6 then auto-triggers an investigation. |
| D3-9 T5 | **Model card update** | LOW | `bonsai_ml/model_cards/` — update after first real training run with: dataset size, split, AUC-ROC, calibration curve, false positive rate at P95 threshold. |

### Code Issues Identified
- `archive_to_training.py` reads the archive with a hard-coded `window_seconds = 300` for feature aggregation — this should be configurable, especially for the first training run where the optimal window is unknown.
- `calibration.py` `CalibrationStore.append()` writes scores to a flat pickle file — not appropriate for the graph store model used elsewhere. Should write to the `gnn_calibration_scores` lbug table (already defined in `graph/mod.rs` schema).
- `model.py` `BonsaiGNN` uses fixed hidden dimensions (64, 32) with no hyperparameter config. For real data with different graph sizes, these may be inadequate. Add a `ModelConfig` dataclass.
- The GNN training script `train_anomaly.py` does not set a random seed — training results are not reproducible. Add `torch.manual_seed()` and `numpy.random.seed()`.

---

## D3-10 — Developer Experience & CI Health

### Problem
The CI and developer workflow have several friction points that accumulate over a multi-month project.

### Tasks

| ID | Task | Priority | Notes |
|---|---|---|---|
| D3-10 T1 | **GitHub Actions CI pipeline** | HIGH | `.github/` has workflow files — verify: `cargo test --workspace` runs on PR, UI `npm run build` runs, Python `pytest` runs. Add a job that validates `bonsai.toml.example` parses correctly against `config.rs`. |
| D3-10 T2 | **Rust test coverage gate** | MEDIUM | Add `cargo llvm-cov` step to CI. Target: >60% coverage on `src/graph/`, `src/enrichment/`, `src/remediation/`. |
| D3-10 T3 | **`scripts/cleanup_laptop.sh` wipe procedure** | HIGH | The existing script exists but needs a DV3-specific pass: stop all Docker services, remove bonsai volumes, remove ContainerLab topologies, remove bonsai binary, clear vault. Document the exact wipe sequence for a clean slate. |
| D3-10 T4 | **Dependency audit automation** | LOW | `deny.toml` exists — add `cargo deny check` to CI. Run `pip-audit` on `python/` in CI. |
| D3-10 T5 | **UI test harness** | LOW | Add Playwright smoke tests for: setup wizard completion, device add flow, approvals list rendering. Currently zero UI tests. |

### Code Issues Identified
- `.github/` has 10 items but their content is hidden (gitignored in this view) — verify that workflows are actually wired to run on push/PR and not just on manual dispatch.
- `scripts/cleanup_laptop.sh` does `docker volume rm` with a list of volume names — this list is not kept in sync with the `volumes:` section of `docker-compose.yml`. When new volumes are added to compose, they are not cleaned by the script.
- `pyproject.toml` has `bonsai-sdk` as the wheel package but `hatch.build.targets.wheel.packages = ["bonsai_sdk"]` — the `bonsai_ml` and `bonsai_agent` packages are NOT included in the wheel. If a user installs `bonsai-sdk` from PyPI they would not get ML or agent code. Needs a separate `bonsai-ml` package or a `[project.optional-dependencies]` group that pulls `bonsai_ml` in.
- `collector_engine.py` in `python/` is a standalone script with no test coverage and no integration into `pyproject.toml` packages. It should either be packaged or documented as a script.

---

## D3-11 — Streaming & Endpoint Graph Completeness (Session 9)

### Problem
Six structural gaps identified in the streaming and endpoint graph layer:
1. NetFlow exporter IP is lost — `target` is set to flow src IP, not the exporting router.
2. `AppFlow` nodes have zero edges — they are disconnected from the Device topology.
3. No `HostEndpoint` node type — servers, APs, phones, CPE have nowhere to live in the graph.
4. OTLP spans are received but never written to the graph — `Application` node is never populated.
5. No GUI to configure or troubleshoot any streaming receiver (NetFlow, OTLP, Syslog, SNMP, BMP, BGP-LS).
6. Collector streaming config requires SSH + bonsai.toml edit — not manageable from Core GUI.

### Tasks

| ID | Task | Priority | Status |
|---|---|---|---|
| D3-11 T1 | **Track A1: Fix NetFlow exporter identity** — pass `peer.ip()` as `TelemetryUpdate.target`; add `exporter_address` to `NetflowRecord` TelemetryEvent; update `publish_flow` in `netflow.rs` | HIGH | ✅ |
| D3-11 T2 | **Track A2: CARRIES_FLOW edge** — after `upsert_app_flow` in `write_netflow_record`, MERGE `(d:Device)-[:CARRIES_FLOW]->(f:AppFlow)` using exporter address | HIGH | ✅ |
| D3-11 T3 | **Track B1: HostEndpoint schema** — add `HostEndpoint` node table + `CONNECTED_TO(HostEndpoint→Interface)`, `SRC_HOST(AppFlow→HostEndpoint)`, `DST_HOST(AppFlow→HostEndpoint)` rel tables to `graph/mod.rs` | HIGH | ✅ |
| D3-11 T4 | **Track B2: `upsert_host_endpoint` helper** — new function in `common.rs`; idempotent MERGE on `id=ip`; ON MATCH updates kind/hostname/vendor only if non-empty | HIGH | ✅ |
| D3-11 T5 | **Track B3: NetBox enricher HostEndpoint pass** — second `dcim/devices/` fetch filtered by configurable `endpoint_roles` list (default: server, ap, phone, cpe, printer); upsert HostEndpoint + CONNECTED_TO via NetBox `connected_endpoints` | HIGH | ✅ |
| D3-11 T6 | **Track B4: LLDP → HostEndpoint inference** — in `write_lldp_neighbor`, if LLDP peer chassis-id does not match any `Device.address`, upsert `HostEndpoint {kind: "unknown"}` | MEDIUM | ✅ |
| D3-11 T7 | **Track C1: AppFlow ↔ HostEndpoint edges** — in `write_netflow_record`, after CARRIES_FLOW, do IP-lookup MERGE for `SRC_HOST` and `DST_HOST`; silent no-op if no HostEndpoint exists | MEDIUM | ✅ |
| D3-11 T8 | **Track D1: Settings backend** — new `src/http_server/settings.rs`; `GET /api/settings/streaming` + `PATCH /api/settings/streaming`; surgical TOML rewrite of `[streaming.*]` sections | HIGH | ✅ |
| D3-11 T9 | **Track D2: Settings UI** — new `ui/src/routes/Settings.svelte` with streaming receiver cards (enabled toggle, port, protocol badge, discard/save); wired into App.svelte nav | HIGH | ✅ |
| D3-11 T10 | **Track E1: Collector streaming status** — extend `CollectorStatusJson` with `streaming_status` map; show per-collector receiver badges in `Collectors.svelte` | MEDIUM | ✅ |
| D3-11 T11 | **Track F1: OTLP span → graph** — `write_otlp_span` upserts `Application` node; MERGE `RUNS_SERVICE` edge when `peer_address` matches Device or HostEndpoint | MEDIUM | ✅ |

---

## D3-14 — Collector Health & Telemetry Propagation to Core

**Decision**: D24 (collector health/telemetry propagation: full-fidelity enriched heartbeat)

**Context (Session 10 code audit — 8 critical bugs found)**:

**Bug 1 (CRITICAL)** — `@/Users/arjuna.ganesan/bonsai/src/ingest.rs:1513-1518`: `CollectorStats.queue_depth_updates` and `uptime_secs` are hardcoded `0`. Core Collectors page always shows queue=0, uptime=0.

**Bug 2 (CRITICAL)** — `@/Users/arjuna.ganesan/bonsai/src/server_startup.rs:893-902`: `DiagnosticState` is created but `mark_registered()` and `update_stats()` are never called. The local `/api/collector/status` endpoint always returns `registered_with_core: false`, `queue_depth: 0`, `assigned_devices: []` — stale forever.

**Bug 3 (WRONG DATA)** — `@/Users/arjuna.ganesan/bonsai/src/http_server/governance.rs:165`: `streaming_status` on every collector card is built from the **Core's own** `StreamingConfig`, not the remote collector's config. Wrong ports, wrong enabled flags in distributed deployments.

**Bug 4** — `CollectorStats` proto (4 fields only) carries no receiver health, no resource metrics, no error counts. There is no way for Core to know if collector's syslog/SNMP receivers are running.

**Bug 5** — Collector memory/CPU/disk utilisation never propagated to Core. `memory_profile` and `resource_governor` are local-only.

**Bug 6** — Collector-side error/warn events (queue high-water, subscriber drop, port conflict) invisible at Core.

### Track M — Fix Heartbeat Payload (D24)

| Task | Description | Priority | Status |
|---|---|---|---|
| D3-14 T1 | **M1: Wire uptime_secs in heartbeat** — record `startup_instant: Instant` at the start of `run_collector_manager`. Each heartbeat tick computes `uptime_secs = startup_instant.elapsed().as_secs() as i64`. One-line fix. | HIGH | ✅ |
| D3-14 T2 | **M2: Wire queue_depth_updates in heartbeat** — shared `Arc<AtomicU64>` counter; `log_queue_status` stores `pending_records` into it; `run_collector_manager` heartbeat tick reads it. | HIGH | ✅ |
| D3-14 T3 | **M3: Extend `CollectorStats` proto** — added fields 5-11: `queue_bytes`, `queue_utilization_pct`, `active_subscribers`, `failed_subscribers`, `memory_used_bytes`, `recent_warn_count`, `recent_error_count`. Additive — old collectors send zero defaults. | HIGH | ✅ |
| D3-14 T4 ✅ | **M4: Add `repeated ReceiverStatus` to `CollectorStats` proto** — `CollectorReceiverStatus` proto message added; `receiver_statuses = 12` on `CollectorStats`; `api.rs` heartbeat handler maps to `ReceiverStatusRecord`. | — new proto message `CollectorReceiverStatus { name, state, addr, packet_count, error_count, last_packet_at_ns }`. Populated from `ReceiverSupervisor.status_snapshot()` (D3-13 T1 prerequisite). If D3-13 not yet landed, send empty repeated field. | MEDIUM | ⏳ |
| D3-14 T5 | **M5: `CollectorRuntimeState` stores enriched fields** — `CollectorRuntimeState`, `CollectorStatus`, `record_heartbeat()`, `collector_status_summary()` all extended. `api.rs` `heartbeat` handler passes new fields through. `ingest.rs` populates `queue_bytes`/`queue_utilization_pct` from shared AtomicU64 counters, `memory_used_bytes` from `read_process_memory_bytes()` (Linux `/proc/self/status`). | HIGH | ✅ |

### Track N — Fix DiagnosticState Wiring (D24)

| Task | Description | Priority | Status |
|---|---|---|---|
| D3-14 T6 | **N1: Pass `DiagnosticState` into `run_collector_manager`** — `Option<DiagnosticState>` threaded in; `mark_registered()` called after successful `register_collector` RPC. | HIGH | ✅ |
| D3-14 T7 | **N2: Wire `update_stats()` from `run_core_forwarder`** — `Option<DiagnosticState>` threaded into `run_core_forwarder` → `log_queue_status`; calls `ds.update_queue_depth()` on every log interval. `update_stats()` called from manager heartbeat tick. Added `update_queue_depth()` method to `DiagnosticState`. | HIGH | ✅ |
| D3-14 T8 ✅ | **N3: Add receiver_statuses to DiagnosticState** — `DiagnosticState` has `receiver_statuses: Vec<ReceiverStatusEntry>`, `update_receiver_statuses()`, called from `ingest.rs` heartbeat tick. ✅ | — `DiagnosticState` gains `receiver_statuses: Vec<ReceiverStatusSnapshot>`. New method `update_receiver_statuses(Vec<ReceiverStatusSnapshot>)`. Called by `ReceiverSupervisor` after each restart or on a 30s poll tick. `/api/collector/status` response includes `receiver_statuses`. | MEDIUM | ⏳ |

### Track O — Fix Core UI per-collector data (D24)

| Task | Description | Priority | Status |
|---|---|---|---|
| D3-14 T9 | **O1: Remove `streaming_status_from_config()` from collector cards** — removed the erroneous `streaming_badges.clone()` assignment from `collectors_handler`. `streaming_status` now stays as empty `HashMap::new()` for remote collectors until heartbeat-reported data lands in T10. | HIGH | ✅ |
| D3-14 T10 | **O2: `CollectorStatusJson` gains health fields** — `queue_bytes`, `queue_utilization_pct`, `active_subscribers`, `failed_subscribers`, `memory_used_mb` (bytes÷1MiB), `recent_warn_count`, `recent_error_count` added to struct and populated by `collector_status_json()`. | HIGH | ✅ |
| D3-14 T11 | **O3: Collectors.svelte health panel** — (a) queue utilisation bar (green<50%/yellow<80%/red≥80%, tooltip shows % + bytes), (b) subscriber row (active chip + failed chip if >0), (c) memory chip + warn/error chips. All panels are conditional on data presence — no visual noise on fresh installs. `fmtBytes()` and `queueColor()` helpers added. | HIGH | ✅ |

### Execution order
T1 and T2 are independent one-liners — start there. T3→T5 as a batch (proto change + handler). T6→T7 as a batch (DiagnosticState wiring). T9 is a bugfix that can land before T4/T8. T8 depends on D3-13. T10→T11 at the end.

---

## D3-13 — Receiver Supervisor, Hot-Reload & Full Port Configurability

**Decisions**: D22 (receiver supervisor + hot-reload), D23 (HTTP port + all listen addrs configurable)

**Context (Session 10 findings)**:
- All 7 receivers (syslog, snmp, bmp, bgp_ls, otlp, netflow, pcep) are fire-and-forget `tokio::spawn` tasks — no stored handle, no restart, no live status. Port conflicts cause silent death.
- HTTP UI port is **hardcoded** to `0.0.0.0:3000` in `server_startup.rs` — absent from `bonsai.toml.example`, cannot be overridden without editing source.
- `PATCH /api/settings/streaming` currently always returns `requires_restart: true` — forces full process restart even for simple enable/disable toggles.
- Syslog and SNMP are entirely absent from `StreamingSettingsResponse` — Settings UI cannot control them.
- Distributed deployments (collector mode) need per-collector port customisation independently of the core, for environments where port assignments differ per PoP.

### Track K — ReceiverSupervisor (D22)

| Task | Description | Priority | Status |
|---|---|---|---|
| D3-13 T1 | **K1: `src/receiver_supervisor.rs`** — `ReceiverSupervisor` struct with `HashMap<&'static str, ReceiverEntry>`. Each entry holds: `abort_handle: Option<AbortHandle>`, `status: ReceiverStatus` (state enum: `listening\|stopped\|error\|port_conflict`), `addr: String`, `packet_count: u64`, `error_count: u64`, `last_packet_at_ns: Option<i64>`. Methods: `spawn(name, config, factory_fn)`, `restart(name, new_config)`, `status_snapshot() -> Vec<ReceiverStatusSnapshot>`. Shared in `AppState` as `Arc<RwLock<ReceiverSupervisor>>`. | HIGH | ✅ Confirmed in code |
| D3-13 T2 | **K2: Migrate all receiver spawns to supervisor** — replace each bare `tokio::spawn` block in `server_startup.rs` (lines 425-560) with `supervisor.spawn(name, cfg, factory)`. Each receiver factory does a pre-bind check: if `bind()` fails with `AddrInUse`, sets `port_conflict` status, does NOT panic/warn-and-die silently. Covered: `syslog_udp`, `syslog_tcp`, `snmp_udp`, `bmp_tcp`, `bgp_ls_tcp`, `otlp_http`, `netflow_udp`. | HIGH | ✅ Confirmed in code |
| D3-13 T3 | **K3: `PATCH /api/settings/streaming` calls supervisor.restart()** — after writing TOML delta, call `supervisor.restart(name, new_config)` for each changed receiver. Return `{ ok: true, requires_restart: false, receiver_statuses: {...} }`. If bind fails (port conflict), return HTTP 409 with `{ ok: false, error: "Port 5514 is already in use", receiver: "syslog_udp" }`. Remove blanket `requires_restart: true`. | HIGH | ✅ Done session 15 — syslog/snmp now use supervisor.spawn(); requires_restart always false |
| D3-13 T4 | **K4: Add syslog + snmp to StreamingSettingsResponse** — `[signals.syslog]` and `[signals.snmp]` are currently absent from the Settings API response and the Settings UI. Add `syslog` and `snmp` fields to `StreamingSettingsResponse`. Add corresponding `ReceiverPatch` fields to `StreamingSettingsPatch`. Update `settings.rs` GET and PATCH handlers. Update TOML write to also rewrite `[signals.*]` sections. | HIGH | ✅ Already present in settings.rs |
| D3-13 T5 | **K5: `GET /api/settings/streaming` returns live supervisor status** — extend response with `receiver_statuses: HashMap<String, ReceiverStatusSnapshot>` (state, addr, packet_count, error_count, last_packet_at_ns). Polled by the Settings UI every 5s to show live status without page reload. | MEDIUM | ✅ Added in session 15; Settings.svelte already polled /api/receivers/status |

### Track L — Port Configurability (D23)

| Task | Description | Priority | Status |
|---|---|---|---|
| D3-13 T6 | **L1: `http_addr` config key** — add `http_addr: String` field to the root `BonsaiConfig` struct in `config.rs`, default `"0.0.0.0:3000"`. Remove hardcoded `"0.0.0.0:3000"` from `server_startup.rs:995`. Read from `cfg.http_addr`. Changing `http_addr` requires process restart — communicated in UI and docs. | HIGH | ✅ Confirmed in config.rs |
| D3-13 T7 | **L2: `bonsai.toml.example` — consolidate all listen addresses** — add a new `# ── Listen Addresses ──` section near the top of `bonsai.toml.example` that documents ALL configurable addresses: `http_addr`, `api_addr`, `metrics_addr`, and all `[signals.*]` + `[streaming.*]` addresses. Add comments explaining privilege requirements (ports < 1024 need root or `cap_net_bind_service`). Change BMP default to `"0.0.0.0:10179"` in config.rs and example. | MEDIUM | ⏳ |
| D3-13 T8 | **L3: Settings UI — live receiver status + port conflict UX** — update `Settings.svelte` receiver cards to: (a) poll `GET /api/settings/streaming` every 5s for live status, (b) show status badge (`listening ●` green / `stopped ○` grey / `error ▲` amber / `port conflict ✕` red), (c) on save, if API returns 409 show inline error "Port 5514 in use — try a different port", (d) add syslog and snmp cards (currently absent). Add HTTP UI port field to a new "Core Listeners" card that shows the current `http_addr` and `api_addr` with a restart-required warning on edit. | HIGH | ⏳ Depends on T3/T4 |

### Distributed deployment note

In collector mode (`runtime.mode = "collector"`), each collector has its own `bonsai.toml` with its own `[signals.*]` and `[streaming.*]` ports. The Core's `GET /api/settings/streaming` returns Core-owned defaults, but the Settings UI on the **collector's diagnostic port** (when `collector.diagnostic_port` is set) should show and control that collector's own ports. This is the per-PoP customisation path — no central override of collector ports (that would require the Core knowing each collector's environment). D3-13 T3/T5 must work against the locally running process regardless of mode.

---

## D3-12 — Signal Observability, Detection Provenance & Topology Completeness

**Decisions**: D18 (event feed filtering), D19 (detection provenance), D20 (topology completeness), D21 (SNMP correlation)

**Context (Session 10 code review findings)**:
- Event feed is unfiltered — 200-event firehose with no device/site/source filter. Unusable in multi-device deployments.
- Detection provenance is invisible — incidents show rule_id only; no source attribution, no correlation timing, no blast radius inline, no grouping rationale.
- Topology omits HostEndpoint nodes entirely (D3-11 added schema but topology_handler was never updated).
- SNMP traps are first-tier only — raw event stored, OID varbinds never parsed into typed joins (Interface, BgpNeighbor). Parity gap with syslog fact pipeline.

### Track G — Event Feed Filtering (D18)

| Task | Description | Priority | Status |
|---|---|---|---|
| D3-12 T1 | **G1: `source_type` on BonsaiEvent + StateChangeEvent** — add `source_type: String` field to `BonsaiEvent` struct and `StateChangeEvent` graph node. Populate in every `write_state_change_event` callsite: `gnmi` (interface/BGP/BFD/LLDP/config), `syslog`, `snmp`, `netflow`, `otlp`, `detection`, `registry`. Add `source_type` to `SsePayload`. | HIGH | ⏳ NOT done — field absent from BonsaiEvent |
| D3-12 T2 | **G2: `/api/events/history` endpoint** — `GET /api/events/history?source=&device=&site=&limit=100`. Queries `StateChangeEvent` from graph DB with optional WHERE filters. Returns last N events matching criteria. Complements live SSE with a queryable history. | HIGH | ⚠️ Route exists in mod.rs; handler impl TBD |
| D3-12 T3 | **G3: Event feed filter bar UI** — `Events.svelte` gains: source group chips (ALL / gNMI / Syslog / SNMP / NetFlow / OTLP / Detection), device address text filter (client-side), site selector dropdown, severity filter. Uses history endpoint for initial load; SSE continues for live appending (filtered client-side). | HIGH | ⏳ Depends on T1 |

### Track H — Detection Provenance (D19)

| Task | Description | Priority | Status |
|---|---|---|---|
| D3-12 T4 | **H1: Multi-source TRIGGERED_BY edges** — change `WriteRequest::Detection` and `write_detection()` to accept `source_event_ids: Vec<String>` (replacing single `state_change_event_id`). Write one `TRIGGERED_BY` edge per source event ID. Existing callers pass their single ID as a `vec![id]` — no breaking change to rule firing logic. | HIGH | ⏳ |
| D3-12 T5 | **H2: `source_types` + `correlation_latency_ms` on DetectionEvent** — `DetectionEvent` node gains two new properties. `source_types` = comma-separated distinct source_type values from the contributing StateChangeEvents. `correlation_latency_ms` = `fired_at_ns - min(occurred_at_ns of source events)` in milliseconds. `DetectionRow` and `DetectionsResponse` gain these fields. | HIGH | ✅ Confirmed in graph/mod.rs — source_types Vec<String> on DetectionRow |
| D3-12 T6 | **H3: `correlation_chain` + `blast_radius_summary` on IncidentJson** — `incidents_handler` fetches `TRIGGERED_BY` source events for the root detection (1 extra graph query). `IncidentJson` gains `correlation_chain: Vec<CorrelationStep>` (ordered signal→detection with source_type + event_type + timestamp) and `blast_radius_summary: Option<BlastRadiusSummary>` (device count + app count from blast_radius query on root device). | HIGH | ⏳ |
| D3-12 T7 | **H4: Incident card provenance panel UI** — expand `Incidents.svelte` incident card: (a) source attribution badge row (gNMI/Syslog/SNMP/NetFlow chips — colored by protocol), (b) correlation latency chip ("correlated in 340ms"), (c) blast radius summary inline ("3 devices · 2 apps in blast radius"), (d) grouping rationale line ("grouped: 2 detections on 2 devices within 12s window"). | HIGH | ⏳ |

### Track I — Topology Completeness (D20)

| Task | Description | Priority | Status |
|---|---|---|---|
| D3-12 T8 | **I1: HostEndpoint nodes in topology_handler** — add graph query `MATCH (h:HostEndpoint)-[:CONNECTED_TO]->(i:Interface) RETURN h.ip, h.kind, h.hostname, h.vendor, i.device_address, i.name`. Add `TopologyResponse.host_endpoints: Vec<HostEndpointJson>`. Add `DeviceJson.recent_event_count: usize` (count of StateChangeEvents in last 1h per device). | MEDIUM | ⏳ |
| D3-12 T9 | **I2: Topology.svelte HostEndpoint rendering** — render HostEndpoint nodes as small diamonds (◆) positioned near their attached Device node. Color by `kind`: server=teal, ap=blue, phone=purple, unknown=grey. Event heatmap ring around Device node: grey(0), yellow(1-5), orange(6-20), red(21+). Toggle to show/hide host endpoints. | MEDIUM | ⏳ |

### Track J — SNMP OID-to-Graph Correlation (D21)

| Task | Description | Priority | Status |
|---|---|---|---|
| D3-12 T10 | **J1: `config/snmp_oid_patterns/default.yaml`** — YAML file mapping OID prefixes to `fact_type` + `field_extraction` rules. Covers: `linkDown`→`link_down` (extract `if_name` from varbind ifDescr/ifAlias), `linkUp`→`link_up`, `bgpBackwardTransition`→`bgp_neighbor_down` (extract `peer_address` from bgpPeerRemoteAddr varbind). Include Cisco enterprise OID prefixes for interface traps. | MEDIUM | ✅ Confirmed in config/snmp_oid_patterns/default.yaml |
| D3-12 T11 | **J2: `SnmpFactExtractor` + `SnmpFact` struct** — parallel to `SyslogFactExtractor`. Loads YAML patterns from `config/snmp_oid_patterns/`. `extract(&SnmpTrapEvent)` returns `Vec<SnmpFact>` where `SnmpFact` has `fact_type, fields: BTreeMap<String,String>`. `run_snmp_receiver` calls extractor and publishes additional `TelemetryUpdate` at `signals/snmp_fact/{fact_type}`. | MEDIUM | ✅ Confirmed in src/signals/snmp.rs |
| D3-12 T12 | **J3: `TelemetryEvent::SnmpFact` + `join_snmp_fact()`** — new variant `SnmpFact{fact_type}` in `telemetry.rs`. `write_snmp_fact_event()` in `graph/mod.rs` mirrors `write_syslog_fact_event()`. `join_snmp_fact()` uses same join logic: if `if_name` field → lookup Interface state; if `peer_address` field → lookup BgpNeighbor state. Stores join context in `detail_json` of the StateChangeEvent. | MEDIUM | ⏳ |

---

## D3-15 — Scalability & Multi-Source Correlation (Sessions 13–14)

**Decisions**: D25 (priority write channel), D26 (SubscriptionStatus batching), D27 (CorrelationBuffer), D28 (change-detection capacity), D29 (Python SDK bounds)

### Problem
Eight scalability/correctness issues identified under load:
- Detection/remediation latency spikes under high telemetry ingest (queued behind batch)
- SubscriptionStatus renewals flush telemetry pipeline ~100×/min at scale
- Multi-source signal duplication: same fault fires detection 3× (gNMI + syslog + BMP)
- Change-detection subscriber queue fills under burst load; drops detection signals
- Python `WindowRegistry` unbounded memory growth in long-running sessions (30d+)
- Python `_last_fired` dict accumulates stale entries indefinitely
- Multi-source event IDs not propagated to `CreateDetectionRequest` proto
- `write_detection` / `write_remediation` non-transactional Cypher ops risk partial writes

### Tasks

| ID | Task | Priority | Status |
|---|---|---|---|
| D3-15 T1 | **F2: Priority write channel** — `PriorityWriteRequest` enum; separate `mpsc` channel; `biased select!` in coordinator; `submit_priority()` method; all detection/remediation callers migrated | HIGH | ✅ |
| D3-15 T2 | **F3: SubscriptionStatus batching** — `sub_status_pending: Vec<SubscriptionStatusWrite>` (128 cap); `flush_sub_status_batch()` helper; flush on tick or full batch | HIGH | ✅ |
| D3-15 T3 | **F1: CorrelationBuffer** — new `src/correlation_buffer.rs`; `CorrelationKey{device, semantic_type, sub_key}`; 45s window; `record()` → `NewSlot\|Absorbed`; `drain_expired()` sweep task; `bonsai_correlation_multi_source_total` Prometheus counter; wired into `GraphStore` and all 6 correlatable write functions | HIGH | ✅ |
| D3-15 T4 | **F4: Transactional write_detection + write_remediation** — wrap all Cypher ops in `BEGIN TRANSACTION / COMMIT` | HIGH | ✅ |
| D3-15 T5 | **F8: Change-detection subscriber 8× capacity + DropOldest** | HIGH | ✅ |
| D3-15 T6 | **F5: WindowRegistry bounded** — `max_entries=4096`, FIFO eviction, `evict_stale()` | MEDIUM | ✅ |
| D3-15 T7 | **F6: `_last_fired` TTL eviction** — `_evict_last_fired(now)` at start of `evaluate_graph()` | MEDIUM | ✅ |
| D3-15 T8 | **F7: Python multi-source event IDs** — `Features.source_event_ids`, `Detection.effective_source_event_ids`, `create_detection(source_event_ids=...)`, `proto: repeated string source_event_ids = 9` | HIGH | ✅ |

### Key files
- `src/correlation_buffer.rs` — NEW
- `src/lib.rs` — `pub mod correlation_buffer`
- `src/graph/mod.rs` — CorrelationBuffer on GraphStore, 6 callsite updates, transactional writes
- `src/write_coordinator.rs` — PriorityWriteRequest, sub_status_pending, biased select
- `src/change_detection.rs` — subscriber capacity + DropOldest
- `src/server_startup.rs` — correlation sweep task
- `python/bonsai_sdk/window.py` — bounded WindowRegistry
- `python/bonsai_sdk/rules/streaming.py` — _evict_last_fired
- `python/bonsai_sdk/detection.py` — source_event_ids
- `python/bonsai_sdk/client.py` — source_event_ids param
- `python/collector_engine.py` — passes effective_source_event_ids
- `proto/bonsai_service.proto` — source_event_ids field 9

---

## D3-16 — Live UI: Environment-Agnostic 3-Panel Refactor (Session 14)

**Decision**: D30 (Live UI 3-panel environment-agnostic architecture)

### Problem
Live Status UI was DC-only, unscalable, and had no site context:
- Topology tiering: hardcoded `superspine/spine/leaf` + `hostname.includes('super')` heuristic
- BGP table always rendered (noisy on campus/IoT)
- Event feed: `max-height: 600px` fixed height, no SSE reconnect
- Site filter buried inside topology panel; no per-site health summary; no incident count

### Tasks

| ID | Task | Priority | Status |
|---|---|---|---|
| D3-16 T1 | **`SiteRail.svelte`** — new left-column site navigation; health dot, device count, incident badge per site; click-to-isolate topology to site | HIGH | ✅ |
| D3-16 T2 | **`LiveStatusBar.svelte`** — new 32px top bar; device count, health pills, incident count, SSE dot, last-updated age | MEDIUM | ✅ |
| D3-16 T3 | **`Live.svelte` 3-column layout** — `140px \| 1fr \| 320px` grid; single topo fetch drives SiteRail + StatusBar via `onTopoLoad` callback | HIGH | ✅ |
| D3-16 T4 | **`Topology.svelte` env-agnostic tier refactor** — `ROLE_TIER` alias map (30+ roles: DC/campus/SP/WAN/wireless/firewall/LB); degree-percentile auto-tier (first-class path); `activeSite` prop from parent; conditional BGP column (`hasBgpData`); flex-fill canvas; tier labels from actual node roles | HIGH | ✅ |
| D3-16 T5 | **`Events.svelte` scalable feed** — `flex:1` scroll list; SSE exponential-backoff reconnect (1s→30s); severity coloring from event type semantics; collapsible JSON detail (hover-reveal); `onSseChange` callback to parent | HIGH | ✅ |
| D3-16 T6 | **`colors.js` role coverage** — `roleStrokeColor()` covers all 30+ role aliases; hostname heuristic removed | MEDIUM | ✅ |

### Key files
- `ui/src/lib/SiteRail.svelte` — NEW
- `ui/src/lib/LiveStatusBar.svelte` — NEW
- `ui/src/routes/Live.svelte` — 3-panel orchestrator
- `ui/src/lib/Topology.svelte` — full refactor
- `ui/src/lib/Events.svelte` — scalable feed
- `ui/src/lib/design/colors.js` — expanded role colors

---

## Cross-Cutting Issues Found During Code Review

These issues cut across multiple modules and should be tracked as a group:

### Security
- `credentials.rs` vault passphrase is read from `BONSAI_VAULT_PASSPHRASE` env var. If bonsai is run as a systemd service with `Environment=` in the unit file, the passphrase is visible in `/proc/<pid>/environ` to root. Document the correct approach: use `EnvironmentFile=` pointing to a `600`-permission file.
- `managed_devices.rs` `test_credential_handler` returns `{success, message}` where `message` can include the raw gRPC error string — this may leak internal TLS certificate details to the browser. Strip the error to a safe summary.
- MCP `query_graph` tool passes raw Cypher to lbug. Even read-only, a deeply nested `MATCH` with no `LIMIT` can cause a full-graph scan. Inject `LIMIT 1000` and a 5s timeout.

### Performance
- `topology_handler` in `observability.rs` runs three separate blocking lbug queries on the thread pool (devices, links, BGP). These could be parallelised with `tokio::join!` on three `spawn_blocking` calls.
- `managed_devices_handler` calls `read_device_vendors()` (a full Device scan) on every request to back-fill vendor for devices without it. This should be a one-time migration or a cached lookup.
- `Onboarding.svelte` has a `scheduleDeviceRefresh()` with 250ms debounce polling — this fires on every `onMount` and page visibility change, causing unnecessary API traffic during active onboarding sessions.

### Logical/Design Issues
- `Setup.svelte` and `Onboarding.svelte` are two separate onboarding experiences that can both be reached. `Setup.svelte` is shown on `is_first_run`, `Onboarding.svelte` is the device wizard. There is a design confusion: a user who completes Setup then goes to "Add device" lands in Onboarding — but Onboarding also has environment/site/credential steps that duplicate Setup. The two should be merged (Setup becomes step 0 of Onboarding, or Onboarding replaces Setup entirely).
- The graph schema has `Device`, `Interface`, `BgpNeighbor`, `LldpNeighbor`, `Site`, `Environment` but no `Rack` — so the NetBox enricher writes `netbox_rack` as a string property on Device rather than a proper graph node. This is an anti-pattern: topology queries cannot traverse rack membership.
- `remediation/trust.rs` defines `TrustState` as `approve_each | auto_approve | blocked` but there is no UI to show the current trust state per device/rule/playbook tuple. The Approvals page shows the trust table but only for tuples that have existing proposals — new tuples are invisible until a proposal fires.
- `mcp_server.rs` `RULE_CATALOGUE` lists `bgp_session_down`, `bgp_session_flap`, `bgp_all_peers_down`, `bgp_never_established`, `interface_down`, `bfd_session_down` — but `change_detection.rs` and the Python detector rules may have additional rules not in this catalogue. Any rule not in `RULE_CATALOGUE` produces a degraded AI investigation (no `recurrence_indicators` for the LLM to use).

---

## Sprint Planning Suggestion for DV3

### Phase 1: Foundation (Weeks 1–2)
Focus on the clean install story and README so the project is shareable.
- D3-1 T1, T2, T3 (install + config example + Docker standalone)
- D3-7 T1, T2 (README + bonsai.toml.example)
- D3-10 T3 (laptop wipe procedure)

### Phase 2: Onboarding (Weeks 3–4)
New users must be able to get from zero to a monitored device in 15 minutes.
- D3-2 T1 → T5 (wizard redesign, NetBox import, credential picker, readiness badge)
- D3-3 T1, T2 (NetBox seed + e2e onboarding test)
- D3-4 T2 (NetBox enricher import endpoint)

### Phase 3: Enrichment + Graph (Weeks 5–6)
Validate the graph reflects reality.
- D3-4 T1, T3, T4 (Rack nodes, CLI scraping test, device inspector)
- D3-8 T1, T2, T3, T4 (config_change_event, EntityIdentity, Rack, HostEndpoint)
- D3-3 T3, T4, T5 (blocker CTA, path selection verify, credential pre-test)

### Phase 4: Remediation + AI (Weeks 7–9)
Close the detect → heal loop and add intelligence.
- D3-5 T1, T2, T3, T4 (auto-proposal, playbook desc, rollback countdown, audit trail)
- D3-6 T1, T2, T3, T4 (AI provider config, abstraction layer, agent loop, key mgmt UI)
- D3-6 T5, T6, T7, T8 (grounded handler, auto-trigger, context builder, budget gate)

### Phase 5: GNN + Polish (Week 10+)
Close the 30-day arc.
- D3-9 T1, T2, T3, T4 (event_detection retirement, archive pipeline test, calibration viewer, GNN→investigation)
- D3-10 T1, T2, T4 (CI pipeline, coverage, dep audit)
- D3-7 T3, T4, T5 (DECISIONS.md, architecture diagram, workspace guides)

---

## Decisions Closed (Session 8)

| Question | Answer |
|---|---|
| AI provider first | **Gemini** (free tier), then **Moonshot** (free tier). Anthropic / OpenAI as stubs only. |
| Onboarding merge | **`Setup.svelte` retired.** `Onboarding.svelte` is the sole entry-point with `first_run` prop. |
| Laptop wipe scope | **Full bare-metal wipe** — repo, Docker, binary, volumes all gone. Simulates new-user install. |
| NetBox version | **Auto-detect both 3.x and 4.x** from `/api/` meta. Configurable pin via `extra.netbox_version`. |
| event_detection.rs | **Believed already retired** — verify with grep, clean up dead references if found. |

---

*End of BONSAI_CONSOLIDATED_BACKLOG_DV3.md*
