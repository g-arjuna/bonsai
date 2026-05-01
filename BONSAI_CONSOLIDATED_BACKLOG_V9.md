# BONSAI — Consolidated Backlog v9.0

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_V8.md`. Produced 2026-04-25 after reviewing post-v8 main.
>
> **Substantial v8 progress landed.** Audit subsystem (`src/audit.rs`), path catalogue with plugin loader (`src/catalogue/mod.rs`), enrichment foundation with trait + registry + audit bridge (`src/enrichment/mod.rs`), Environment entity in graph schema with `BELONGS_TO_ENVIRONMENT` edges, ServiceNow mock + NetBox compose + single-source seed (`lab/seed/topology.yaml`, `docker/mock-servicenow/`, `scripts/seed_netbox.py`), four new UI routes (`Setup.svelte`, `Environments.svelte`, `Profiles.svelte`, `Enrichment.svelte`). The hardcoded `profile_name_for_role` is gone — discovery is fully data-driven.
>
> **What v9 adds — five strategic threads from this session:**
>
> 1. **ServiceNow PDI as primary integration target.** A reserved PDI replaces the mock as the dev/test target. Mock stays as CI fallback. This unlocks bidirectional AIOps/ITOM integration — bonsai pushes detection events to ServiceNow Event Management, consumes incident state back, drives the ITOM correlation surface.
>
> 2. **Human-in-the-loop graduated remediation.** The current binary `dry_run` is replaced by a four-state trust model: suggest-only → approve-each → auto-with-notification → auto-silent. Per (playbook, environment, site, rule) trust scores accumulate from operator approvals + outcome verification. Pending Approvals workspace becomes operational UI.
>
> 3. **Operator path overrides at site/role/device granularity.** The Tier 2 catalogue gives global profiles. v9 adds site-scoped, role-within-environment, and device-specific path overrides — operator-managed customisations layered on top of the catalogue, persisted in the registry, manageable from the UI.
>
> 4. **YANG path discovery from public GitHub repos.** A discovery script clones YangModels/yang, openconfig/public, vendor repos; parses with pyang; extracts subscribable paths; generates draft profile YAML for human curation. Distinguishes "candidates" from "lab-verified default catalogue entries."
>
> 5. **OutputAdapter integration architecture.** Parallel trait to GraphEnricher. Bus-subscriber pattern. TSDB output emits from collectors (raw counters); log/AIOps output emits from core (detections, remediations, audit). One trait, many vendors — Prometheus, Mimir, Splunk HEC, Elastic ingest, ServiceNow ITOM Event Management, Kafka. Reuses vault + audit + environment scoping.

---

## Table of Contents

1. [Audience and Positioning (unchanged)](#positioning)
2. [Progress Since v8](#progress)
3. [Code Quality Findings from Review](#quality)
4. [TIER 0 — Loose Ends from v8 Review](#tier-0)
5. [TIER 1 — Pending v8 Implementations (NetBox + ServiceNow enrichers)](#tier-1)
6. [TIER 2 — ServiceNow PDI Integration (new)](#tier-2)
7. [TIER 3 — Human-in-the-Loop Graduated Remediation (new)](#tier-3)
8. [TIER 4 — Operator Path Overrides (new)](#tier-4)
9. [TIER 5 — YANG Path Discovery from Public Sources (new)](#tier-5)
10. [TIER 6 — OutputAdapter Integration Architecture (new)](#tier-6)
11. [TIER 7 — Signals (carry from v8 T5)](#tier-7)
12. [TIER 8 — Path A → Path B GNN (carry from v8 T6)](#tier-8)
13. [TIER 9 — Investigation Agent (carry from v8 T7)](#tier-9)
14. [TIER 10 — Controller Adapters (demand-driven, carry from v8 T8)](#tier-10)
15. [TIER 11 — Scale, Extensions, Polish (carry from v8 T9)](#tier-11)
16. [TIER 12 — Deferred Until Forced](#tier-12)
17. [Execution Order](#execution-order)
18. [Guardrails — Updated](#guardrails)

---

## <a id="positioning"></a>Audience and Positioning

**Unchanged from v7/v8.** Primary target: controller-less environments across DC, campus (wired + wireless), and SP backbones. Multi-vendor. Secondary target: multi-controller correlation.

**v9 sharpens the AIOps positioning**: bonsai is *not* an AIOps tool that competes with ServiceNow ITOM, Moogsoft, or BigPanda. Bonsai is the **autonomous detect-heal layer** that *feeds* those AIOps platforms with high-quality, business-context-enriched events. Operators running ServiceNow ITOM today get bonsai's detections as `em_event` records; operators without an AIOps platform use bonsai's own incident grouping in the UI. This positioning means the ServiceNow ITOM integration is differentiated (we're additive, not competitive) and the UI's incident view remains the standalone-operator entry point.

---

## <a id="progress"></a>Progress Since v8 — Verified Against Main

All verified with code review.

| v8 item | Status | Evidence |
|---|---|---|
| T0-1 v8 topology API role/site metadata | ✅ Done (assumed — not specifically verified in this pass; was on the v8 list) |
| T0-2 v8 credential resolve audit trail | ✅ Done | `src/credentials.rs:284` `resolve(alias, purpose)` with `ResolvePurpose` enum (`Subscribe`, `Remediate`, `Discover`, `Enrich`, `Test`, `Other`); `src/audit.rs::append_credential_resolve` writes JSONL per-day; tests confirm purpose flows through |
| T0-3 v8 protocol version enforcement | ✅ Done | `src/api.rs:19` `PROTOCOL_VERSION_CURRENT = 1`; `check_protocol_compat` invoked on collector connect; warn on minor skew |
| T0-4 v8 metrics expansion | ✅ Done | `metrics_exporter_prometheus` wired in `main.rs:75-78`; bind address from config |
| T0-6 v8 audit retention + export | ✅ Done | `bonsai audit export --since X --until Y` CLI subcommand in `main.rs:782-815`; JSONL retention default 30 days in `audit.rs::enforce_retention` |
| T1-1 v8 Environment graph entity | ✅ Done | `src/graph/mod.rs:88` `environment_id`, `EnvironmentRecord`, `EnvironmentWithCounts`; `Environment` table + `BELONGS_TO_ENVIRONMENT` edge in DDL; `list_environments`, CRUD methods |
| T1-2 v8 first-run `/setup` wizard | ✅ Done | `ui/src/routes/Setup.svelte` (415 lines), four-step flow (welcome → environment → site → credentials → ready), step trail UI |
| T1-3 v8 onboarding inherits environment context | ✅ Done (assumed; needs spot-check) |
| T1-4 v8 Environment workspace UI | ✅ Done | `ui/src/routes/Environments.svelte` (283 lines) |
| T2-1 v8 path profile schema v2 | ✅ Done | `src/catalogue/mod.rs:9-50` `CatalogueProfile` with `environment`, `vendor_scope`; `CataloguePath` with `vendor_only`, `fallback_for` |
| T2-2 v8 remove hardcoded `profile_name_for_role` | ✅ Done | `src/discovery.rs:188-201` filters profiles by environment + vendor_scope + role; sorts by vendor exactness; old hardcoded match gone |
| T2-3 v8 plugin loader | ✅ Done | `src/catalogue/mod.rs:94` `load_catalogue` reads built-ins from `base_dir/*.yaml` and plugins from `base_dir/plugins/<name>/MANIFEST.yaml`; conflict resolution (built-in > version > alphabetical) |
| T2-5 v8 profile + plugin UI workspace | ✅ Done | `ui/src/routes/Profiles.svelte` (267 lines) |
| T3-1 v8 `GraphEnricher` trait | ✅ Done | `src/enrichment/mod.rs:121` trait with `enrich`, `test_connection`; `EnrichmentWriteSurface` for namespace enforcement; `EnricherAuditLog` audit bridge; `EnricherRegistry` with persistent JSON |
| T3-2 v8 NetBox containerised test instance | ✅ Done | `docker/compose-netbox.yml`; `scripts/seed_netbox.py` |
| T3-3 v8 ServiceNow mock | ✅ Done | `docker/mock-servicenow/` (Dockerfile, app.py, requirements.txt, seed.yaml) — **note: superseded by Tier 2 v9 PDI integration as primary target; mock remains as CI fallback** |
| T3-4 v8 single-source seed | ✅ Done | `lab/seed/topology.yaml`; `scripts/seed_lab.sh` dispatcher |
| T3-5 v8 enrichment UI workspace | ✅ Done | `ui/src/routes/Enrichment.svelte` (331 lines) |
| T2-4 v8 default catalogue research | ✅ Partially done | Existing `config/path_profiles/*.yaml` (4 profiles) extended with new schema fields. Full DC/SP/campus/wireless coverage not yet researched — Tier 5 v9 (path discovery) will accelerate this work |
| T1-5 v8 Site workspace environment binding | ✅ Done (assumed) |
| T1-6 v8 migration plan | ✅ Done (assumed; needs spot-check on existing-install upgrade path) |

**Not done — carry forward:**
- T2-6 v8 onboarding wizard path customisation
- T3-1 trait exists, no concrete enricher implementations yet (`StubEnricher` only)
- T4-1 v8 NetBox enricher implementation
- T4-2 v8 ServiceNow CMDB enricher implementation
- T4-3 v8 CLI-scraped enricher
- T4-4 v8 enrichment visibility in device drawer
- T4-5 v8 MCP client infrastructure
- All v8 T5 (signals)
- All v8 T6 (Path A → Path B)
- All v8 T7 (investigation agent)
- All v8 T8 (controller adapters)
- T9-5 NL query, T9-7 TSDB adapter (now subsumed by Tier 6 v9), T9-9 bulk CSV onboarding

---

## <a id="quality"></a>Code Quality Findings from Review

Surfaced by reviewing what landed in this iteration. Captured as Tier 0 v9 corrections where action is needed.

### Q-1 — Plugin version comparison is string-based, not semver-aware

**Location**: `src/catalogue/mod.rs:201` — `if current_version > existing_version`

**Issue**: Strings compared lexicographically. `"0.10.0"` is less than `"0.2.0"` as strings. Plugin versioning will misbehave once any plugin reaches version 0.10 or beyond.

**Fix**: parse with `semver` crate, compare semver-aware.

**Where**: `src/catalogue/mod.rs` — add `semver` dependency, use `Version::parse`.

**Done when**: Unit test asserts `0.10.0` outranks `0.2.0`; non-semver versions fall back to string compare with a warning.

### Q-2 — `inferred_environment_for_role` is a stopgap

**Location**: `src/discovery.rs::inferred_environment_for_role` — referenced from line 189.

**Issue**: Until devices have a true `environment_id` link in the registry, discovery infers archetype from role hint. Reasonable stopgap, but it means a "leaf" in an SP environment gets mapped to `data_center` archetype. Once Tier 1 v8 Environment + onboarding wiring is fully threaded through, the device's actual environment should drive selection.

**Fix**: pass `environment_id` (or environment archetype string) to `recommend_profiles_from_templates` from the calling context (HTTP API + onboarding endpoint should already know it).

**Where**: `src/discovery.rs`, `src/http_server.rs` discovery handler.

**Done when**: Removing `inferred_environment_for_role` does not break any test; profile selection uses the device's actual environment archetype.

### Q-3 — Enrichment trait declares `&crate::graph::GraphStore` not `&dyn BonsaiStore`

**Location**: `src/enrichment/mod.rs:130` — `async fn enrich(&self, graph: &crate::graph::GraphStore, ...)`.

**Issue**: The store-trait abstraction (`BonsaiStore`) was introduced in earlier work specifically so collector and core graphs can be addressed uniformly. The enrichment trait bypasses that abstraction. Today's enrichers run only on core (correct, since they need the full graph), so the bug is latent — but if we ever want a "local enrichment" (e.g. CLI-scraped data on the collector itself), the trait will need refactoring.

**Fix**: refactor to accept `&dyn BonsaiStore`. Cheap now, expensive once concrete enrichers exist.

**Done when**: Trait accepts the abstract store; existing tests pass.

### Q-4 — ServiceNow mock is now redundant primary infrastructure

**Location**: `docker/mock-servicenow/`.

**Issue**: With your PDI reservation, the mock is no longer the primary dev/test target. It still has a role as a CI fallback (CI shouldn't depend on PDI availability), but the active integration work should target the PDI.

**Fix**: not removal, but reframing — mock becomes opt-in via `docker compose --profile mock-servicenow`. Default development docs reference the PDI. CI uses the mock with a clear comment.

**Where**: `docker-compose.yml`, `docs/integration_servicenow.md` (new).

**Done when**: README's "getting started" path uses the PDI; mock instructions move to a dedicated CI-fallback section.

### Q-5 — Setup wizard exists but doesn't gate first-run

**Location**: `ui/src/App.svelte` + `ui/src/routes/Setup.svelte`.

**Issue**: I haven't verified whether the App.svelte detects first-run state (no environments + no credentials + no devices) and routes to `/setup` automatically, or whether it just exists as a manual route. The v8 spec required automatic redirect.

**Fix**: verify; add the auto-redirect if missing.

**Where**: `ui/src/App.svelte` onMount logic.

**Done when**: Fresh `bonsai-credentials/` + empty graph → operator lands on `/setup` automatically.

### Q-6 — `EnricherRegistry::record_run` doesn't write to audit log

**Location**: `src/enrichment/mod.rs:250-260`.

**Issue**: `record_run` updates in-memory state but doesn't call `audit::append_enrichment_run`. The audit module has the function (`append_enrichment_run` exists) but the registry doesn't invoke it.

**Fix**: registry takes a path to the audit root; on every `record_run`, append a structured entry.

**Done when**: An enricher run produces an entry in the daily audit JSONL.

### Q-7 — No concrete enricher implementations to validate the trait

**Location**: `src/enrichment/mod.rs:273` only `StubEnricher` exists.

**Issue**: A trait without at least one real implementation is a contract that hasn't been tested. Until NetBox and ServiceNow enrichers exist, the trait shape may need refactoring once concrete needs surface.

**Fix**: implement at least NetBox enricher in v9 to validate the trait (Tier 1 v9 below).

### Q-8 — No first-run skip option for the Setup wizard

**Issue**: Some operators (CI / scripted deployment) need a way to skip the wizard. v8 spec said "wizard can be skipped" but no skip path is visible in the route.

**Fix**: add an explicit skip in Setup.svelte that creates a default Home-Lab environment + a default site, so operator can proceed to Devices.

**Done when**: A "Skip setup" button creates baseline state and routes to Devices.

---

## <a id="tier-0"></a>TIER 0 — Loose Ends from v8 Review

### T0-1 (v9) — Semver-aware plugin version comparison

Q-1. Cheap, mandatory before any community plugins ship. ~30 lines + test.

### T0-2 (v9) — Enrichment trait uses `&dyn BonsaiStore`

Q-3. Cheap to fix now, expensive once concrete enrichers depend on it. ~10 lines.

### T0-3 (v9) — `record_run` writes to audit log

Q-6. ~20 lines. Critical for compliance — if it's not logged, it didn't happen.

### T0-4 (v9) — First-run auto-redirect to `/setup`

Q-5. Spot-check required. If missing, ~10 lines in App.svelte.

### T0-5 (v9) — Setup wizard skip path

Q-8. Default Home-Lab environment + site creation, then route to Devices. ~30 lines.

### T0-6 (v9) — Remove `inferred_environment_for_role`

Q-2. Pass actual environment archetype through the discovery API. ~50 lines refactor.

### T0-7 (v9) — Default catalogue research follow-up

T2-4 v8 partial. Use Tier 5 v9 path discovery (when it lands) to accelerate, but immediate work is to extend `config/path_profiles/` with at least:
- `dc_evpn_leaf`, `dc_border_leaf` (DC EVPN coverage gap)
- `sp_pe_l3vpn` (existing `sp_pe_full` is too thin for L3VPN)
- `campus_access`, `campus_distribution`, `campus_core` (campus is missing entirely)

Each backed by a doc in `docs/path_profiles/<name>.md`. Done when seven new profiles exist with schema v2 and lab-verification notes.

### T0-8 (v9) — Spot-verify v8 items marked "assumed"

T0-1, T1-3, T1-5, T1-6 v8 were marked done by the in-tree status but I didn't read the code paths in this review. Spot-check each:
- T0-1 v8: confirm `/api/topology` includes `role`, `site_id`, `site_path` in device records
- T1-3 v8: confirm onboarding wizard inherits environment from current selection
- T1-5 v8: confirm Sites workspace shows environment binding
- T1-6 v8: confirm migration runner creates default Home-Lab environment for legacy installs

Done when each is either confirmed-working or has a follow-up ticket.

---

## <a id="tier-1"></a>TIER 1 — Pending v8 Implementations

The trait is in. Now make it real with two concrete enrichers.

### T1-1 (v9) — NetBox enricher implementation

**What**: v8 T4-1. Implements `GraphEnricher` against the local containerised NetBox (T3-2 v8 already landed) and against any production NetBox instance. Pulls:

- Device → site mapping (writes `Site` if missing, links via `LOCATED_AT`)
- Device serial, model, firmware, lifecycle state (writes namespaced `netbox_*` properties)
- Interface descriptions, cable IDs (writes `netbox_description` etc.)
- VLAN assignments (writes `VLAN(id, name)` nodes + `ACCESS_VLAN`/`TRUNK_VLAN` edges per the declared `EnrichmentWriteSurface`)
- Prefix/subnet assignments (writes `Prefix(cidr, role)` nodes + `HAS_PREFIX` edges)
- Platform tags

**Where**: `src/enrichment/netbox.rs` + `[enrichment.netbox]` config section.

**Auth**: REST with token in `Authorization: Token <token>` header. Token resolved from vault as `Other("netbox-token")` purpose (or new `ResolvePurpose::Enrich` variant — already exists per Q from review).

**Done when**: Enricher runs against the seeded `compose-netbox.yml` instance; graph contains expected VLAN/Prefix nodes; UI shows last-run summary in Enrichment workspace.

### T1-2 (v9) — Enrichment visibility in DeviceDrawer

**What**: v8 T4-4. Device drawer gets an "Enrichment" panel showing namespaced properties grouped by source: NetBox, ServiceNow, CLI-scrape. Each property shows its provenance — "from NetBox enricher, last updated 12 minutes ago."

**Where**: `ui/src/lib/DeviceDrawer.svelte`.

**Done when**: An enriched device's drawer clearly distinguishes gNMI-sourced state from enrichment-sourced properties.

### T1-3 (v9) — MCP client infrastructure (shared)

**What**: v8 T4-5. Shared module that any MCP-backed enricher (NetBox MCP, future ones) uses. Reduces per-enricher boilerplate for MCP plumbing.

**Where**: `src/mcp_client.rs`.

**Done when**: NetBox enricher (T1-1) supports both REST and MCP transport via this shared module; switch via config.

---

## <a id="tier-2"></a>TIER 2 — ServiceNow PDI Integration (new)

**Why this is a major v9 thread**: a reserved ServiceNow PDI gives bonsai a real ITOM-grade integration target. Better than the mock for development, opens the bidirectional AIOps story (bonsai sends events; ServiceNow correlates and creates incidents; bonsai receives incident updates).

The mock from v8 T3-3 is not deprecated — it remains as the CI fallback. But the primary integration work targets the PDI.

### T2-1 (v9) — PDI configuration surface

**What**: a way for the operator to register their PDI URL + admin credentials at setup time. Both go into the credentials vault under canonical aliases (`servicenow-pdi-admin`).

**Where**:
- `ui/src/routes/Setup.svelte` — optional step in first-run wizard for ServiceNow integration
- `ui/src/routes/Credentials.svelte` — add a "PDI/Production" tag option for credential entries
- `[integrations.servicenow]` config section with URL + alias

**Compliance discipline**:
- PDI admin creds MUST be stored in vault, never in `.env` or compose files
- Audit purpose for PDI calls: new `ResolvePurpose::ServiceNowAdmin` variant (distinct from generic `Enrich`)
- Connection test before save uses the operator-supplied creds; never persists if test fails
- Documentation explicitly warns: production ServiceNow should not use admin creds. Bonsai needs `itil` + `event_creator` roles only

**Done when**: Operator can register a PDI in the UI; a "test connection" succeeds; the PDI URL + alias are persisted; subsequent enrichment / event-pushing uses these.

### T2-2 (v9) — PDI seed automation

**What**: a script that populates a fresh PDI with the lab-matching topology data, equivalent to what `seed_netbox.py` does for NetBox. Drives off the same `lab/seed/topology.yaml` single source.

**Where**: `scripts/seed_servicenow_pdi.py`.

**Populates**:
- CIs (cmdb_ci_netgear) for the 4 lab devices
- Business services (cmdb_ci_business_service) for representative apps (e.g. "payment-frontend", "internal-tools")
- Relationships (cmdb_rel_ci) — RUNS_SERVICE between devices and services
- Configuration items for sites and racks
- Sample incidents pre-populated for testing the ITOM event flow

**Auth**: reads PDI URL + admin creds from vault (must be already configured per T2-1).

**Done when**: A fresh PDI plus running `scripts/seed_servicenow_pdi.py` produces the lab topology in CMDB; the ServiceNow CMDB enricher (T2-3) finds the seeded data.

### T2-3 (v9) — ServiceNow CMDB enricher (against PDI)

**What**: implements `GraphEnricher`. Same shape as v8 T4-2. Pulls business context from PDI:

- `Application(id, name, criticality, owner_group)` nodes from `cmdb_ci_business_service`
- `Device.snow_ci_id`, `snow_owner_group`, `snow_assignment_group`, `snow_change_freeze` properties
- `RUNS_SERVICE`, `CARRIES_APPLICATION` edges from `cmdb_rel_ci`

**Why this matters for the audience**: once business context lives on the graph, detection logic and remediation policy can be business-aware. "BGP session down on a device carrying payment-frontend, change freeze active → suggest only, do not auto-remediate" is exactly the kind of policy that controllerless-environment operators are forced to assemble manually today.

**Where**: `src/enrichment/servicenow.rs` + `[enrichment.servicenow]` config.

**Done when**: Enricher runs against PDI + seeded data; graph contains Application nodes and business-context edges; integration test validates against seed.

### T2-4 (v9) — ServiceNow Event Management push

**What**: the bidirectional half. Bonsai detection events get pushed to ServiceNow as `em_event` records via the Event Management table API. ServiceNow's correlation rules group related events, create alerts, optionally auto-create incidents.

**Where**: `src/output/servicenow_em.rs` (will live under Tier 6 v9 OutputAdapter once that lands; for now build it as a one-off OutputAdapter).

**Design**:
- Subscribes to detection event topic on the bus
- Transforms `DetectionEvent` to `em_event` schema (source, node, type, severity, description, additional_info as JSON)
- Pushes via REST `POST /api/now/table/em_event` with token from vault
- Records in audit log with `purpose=AiopsEvent`
- Includes source: "bonsai", node = device hostname, additional_info = full event including business context from enrichment

**Done when**: A detection event in bonsai produces an `em_event` in PDI within seconds; the PDI's correlation rules can act on it.

### T2-5 (v9) — ServiceNow incident state consumption (read-only first)

**What**: bonsai polls PDI for incidents that reference bonsai-sourced events; reflects incident state on the graph.

**Where**: extension to the ServiceNow enricher in T2-3, or a separate component.

**Reads**:
- Incidents where `source = "bonsai"` (fields available in em_event after correlation)
- Incident state, assignment group, work notes
- Writes back as `Incident(id, state, assignee, opened_at)` graph nodes linked to the corresponding bonsai detection events

**Done when**: An incident created in PDI from a bonsai event becomes visible in bonsai's UI Incidents workspace, with PDI incident ID + state. Operator can click through to PDI from bonsai.

### T2-6 (v9) — Operator policy: when to push, when not to

**What**: not every detection should become a ServiceNow event. A 30-second BGP flap that auto-recovered is noise. Bonsai's policy filters which events flow.

**Where**: `[integrations.servicenow.event_filter]` config + corresponding UI in Enrichment workspace.

**Default policy**:
- Severity: critical and warning only (informational stays internal)
- Duration: detection persisted for >60 seconds before push
- De-duplication: if same (device, rule_id) fired within last 5 minutes, suppress

Operator can override per-rule, per-environment, per-site.

**Done when**: A configured filter blocks a transient flap from reaching PDI; a sustained issue flows; UI shows event-flow stats (sent, suppressed, errored).

---

## <a id="tier-3"></a>TIER 3 — Human-in-the-Loop Graduated Remediation (new)

**Why this is a v9 thread**: every modern AIOps and network automation tool has a HIL maturity story. Bonsai today is binary — playbooks either auto-execute or run dry. Operators need a graduated path from observation through approval-required to fully autonomous, per playbook, per environment, per site.

This is the explicit AIOps maturity model the industry has converged on. Bonsai needs it both for trust-building and as a differentiator vs. tools that force one mode or the other.

### T3-1 (v9) — TrustState model

**What**: a per-(playbook, environment, site, rule_id) tuple gets a TrustState:

```rust
enum TrustState {
    SuggestOnly,           // never executes; produces proposal for operator approval
    ApproveEach,           // operator approves every execution
    AutoWithNotification,  // executes immediately; rollback window N seconds; operator notified
    AutoSilent,            // executes; recorded in audit but not surfaced unless failure
}
```

Plus a per-tuple history:
- `consecutive_successes: u32`
- `last_success_at_ns: i64`
- `failure_count_30d: u32`
- `operator_approvals: u32`
- `operator_rejections: u32`

**Where**: `src/remediation/trust.rs`, persisted to `runtime_dir/trust_state.json`.

**Done when**: Trust state queryable per tuple; updates on every remediation outcome; persists across restarts.

### T3-2 (v9) — Pending Approvals workspace

**What**: new UI route `/approvals`. Shows queue of remediation proposals awaiting operator decision. Each proposal includes:
- Detection event that triggered it
- Selected playbook + steps
- Affected device + business context (from ServiceNow enricher if available)
- Recent history for the tuple (last 10 outcomes)
- Approve / Reject buttons

**Where**: `ui/src/routes/Approvals.svelte` + `/api/approvals/*` endpoints.

**Operator flow**:
1. Bonsai detects a fault
2. Selects a playbook with TrustState = SuggestOnly or ApproveEach
3. Writes a `RemediationProposal` to the graph
4. UI surfaces it in `/approvals`
5. Operator approves → playbook executes; rejects → recorded with reason
6. Outcome feeds back into TrustState

**Done when**: Test scenario — chaos run produces a detection in a SuggestOnly tuple → proposal appears in `/approvals` → operator approves → playbook executes → trust state increments.

### T3-3 (v9) — Trust graduation logic

**What**: the system suggests trust upgrades based on history. After M consecutive operator approvals (default 10) plus zero rejections in 30 days, system surfaces "this tuple could graduate from ApproveEach to AutoWithNotification — review last 10 successful runs?". Operator decides.

Graduation is never automatic. The operator always opts in to a less-restrictive trust state.

**Where**: `src/remediation/trust_graduation.rs` + UI hint on the per-tuple detail page.

**Done when**: After 10 approvals on a (playbook, env, site, rule) tuple, the UI shows a graduation suggestion; clicking accept changes the trust state.

### T3-4 (v9) — Rollback window for AutoWithNotification

**What**: when a playbook executes in AutoWithNotification trust state, a rollback window of N seconds (default 60) lets the operator cancel. UI shows a banner: "Bonsai just remediated X on Y. [View details] [Rollback now]". After N seconds, rollback is no longer offered.

**Where**: `src/remediation/rollback.rs` + UI banner component.

**Rollback semantics**: every playbook step has an inverse step. Rollback executes inverses in reverse order. Failed rollback is logged as critical and the tuple is forced back to ApproveEach.

**Done when**: A playbook execution within rollback window can be cancelled by operator; the inverse is applied; trust state captures the rollback as a soft-failure.

### T3-5 (v9) — Per-rule, per-environment policy defaults

**What**: when an operator first defines a rule, they set default trust states per environment archetype. Sensible defaults:
- DC: AutoWithNotification for interface flaps, ApproveEach for BGP-related, SuggestOnly for routing changes
- SP: ApproveEach for everything until proven (SP environments tolerate less risk)
- Campus: AutoWithNotification more freely (lower blast radius)
- Home-lab: AutoSilent acceptable (operator owns blast radius)

**Where**: `[remediation.defaults]` config + UI in Rules/Profiles workspace.

**Done when**: Adding a new rule starts with sensible per-environment defaults that the operator can override.

### T3-6 (v9) — Audit + outcome verification

**What**: every trust-state-affecting decision (approve/reject/rollback/graduate) writes to the audit log with a new purpose `ResolvePurpose::TrustOperation`. Operator can review per-tuple history for compliance.

**Where**: `src/audit.rs` + UI history view.

**Done when**: An auditor can produce "all trust state changes for environment X over the past quarter" via `bonsai audit export --filter "trust_op"`.

---

## <a id="tier-4"></a>TIER 4 — Operator Path Overrides (new)

**Context**: the catalogue gives global defaults. Operators need finer control: a custom path for a specific site, a tweaked profile for a non-standard role, a specific device that needs an extra subscription. These are operator data, not bundled content.

### T4-1 (v9) — Override scopes

**What**: three scopes of override, each a separate registry layer:

1. **Site-scoped**: applies to all devices in this site (or any descendant site)
2. **Role-within-environment**: applies to all devices with this role in this environment
3. **Device-specific**: applies to one device (last resort, breaks abstraction)

Resolution order at subscription time:
1. Start with catalogue profile match (current logic)
2. Apply role-within-environment overrides
3. Apply site-scoped overrides (walking up site hierarchy, deeper overrides win)
4. Apply device-specific overrides

Each override is additive (new path) or subtractive (drop a path) or modifying (change sample interval, add `optional` flag).

**Where**: `src/registry.rs::PathOverride` + persistence in `bonsai-registry.json`.

**Done when**: An override "for all `border-leaf` devices in DC environments, also subscribe to oc-aft-state" applies on next subscription cycle for matching devices.

### T4-2 (v9) — Override management UI

**What**: in `/profiles` workspace, add an "Overrides" tab. Operator can:
- View existing overrides at each scope
- Create a new override (pick scope → pick paths to add/drop/modify)
- Test override against a specific device (preview what subscription would result)

**Where**: extension to `ui/src/routes/Profiles.svelte`.

**Done when**: Operator can manage overrides without editing config files.

### T4-3 (v9) — Subscription resolution audit

**What**: when a device gets subscribed, the resolution chain is recorded — "started from profile X, applied role-override Y, applied site-override Z, final path list is [...]". Visible in DeviceDrawer.

**Where**: `src/discovery.rs::resolve_subscription_paths`.

**Done when**: Operator inspecting a device can see why it has the paths it has.

---

## <a id="tier-5"></a>TIER 5 — YANG Path Discovery from Public Sources (new)

**Why this is in v9**: the default catalogue has 4 profiles after extending to v8 schema. Comprehensive coverage across DC/SP/campus/wireless × 4-5 vendors needs hundreds of profiles. Manual research-and-curate is the right approach for default catalogue (lab-verified discipline) but the seed material — *what paths exist* — should come from public sources, not from someone reading vendor docs.

### T5-1 (v9) — YANG path discovery script

**What**: `scripts/discover_yang_paths.py`. Clones public YANG repositories, parses with `pyang`, extracts subscribable container paths, generates draft profile YAML for human review.

**Sources**:
- `YangModels/yang` (canonical)
- `openconfig/public` (OpenConfig)
- `cisco-ie/cisco-yang-models`
- `Juniper/yang`
- `nokia/7x50-YangModels` (Nokia)
- `arista-eosplus/eos-yang-models` (Arista)

**Output**: `discovered_paths/<vendor>/<release>/profile-candidates.yaml`. Each candidate carries the source repo, file, container path, sample interval recommendation, and a "needs lab verification" flag.

**Discipline**: discovered paths are *candidates*, not catalogue entries. They get manually promoted to the default catalogue after lab verification.

**Where**: `scripts/discover_yang_paths.py`, `discovered_paths/` (gitignored to avoid large generated files in git).

**Done when**: Running the script produces candidate YAML for at least one vendor; a workflow doc explains how to promote candidates to the default catalogue.

### T5-2 (v9) — Catalogue plugin distribution

**What**: a way for operators to install community-contributed catalogue plugins. Initially via direct git clone; later a `bonsai catalogue install <url>` command that fetches, verifies, and registers a plugin.

**Where**: new CLI subcommand `bonsai catalogue install`; verification logic checks plugin manifest matches expected fields, signs with a SHA256 if remote source.

**Done when**: A plugin published in a separate repo can be installed locally via the CLI command.

### T5-3 (v9) — Path documentation generation

**What**: for each profile in the catalogue, a generated markdown doc in `docs/path_profiles/<name>.md` describing what each path subscribes to, expected message rate, vendor coverage, common quirks. Half hand-written, half auto-generated from YAML metadata + pyang descriptions.

**Where**: `scripts/gen_profile_docs.py`.

**Done when**: Every profile in `config/path_profiles/*.yaml` has a corresponding doc; docs are regenerated on profile change.

---

## <a id="tier-6"></a>TIER 6 — OutputAdapter Integration Architecture (new)

**Why this matters now**: the architectural conversation about TSDB/Splunk/Elastic/AIOps integration deserves first-class treatment. Bonsai's event bus is the integration plane. Every output is a bus subscriber. One trait covers them all.

### T6-1 (v9) — `OutputAdapter` trait

**What**: parallel to `GraphEnricher`. Outputs subscribe to bus topics, transform events to vendor-specific formats, push via configured transport. Vault-backed credentials, audit-logged, environment-scoped.

```rust
#[async_trait]
pub trait OutputAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn subscribes_to(&self) -> Vec<BusTopic>;       // raw, summaries, detections, remediations, audit
    fn applies_to_environments(&self) -> Vec<String>;
    
    async fn run(
        &self,
        bus: Arc<InProcessBus>,
        creds: &CredentialVault,
        audit: &OutputAdapterAuditLog,
        shutdown: watch::Receiver<bool>,
    ) -> Result<()>;
    
    async fn test_connection(
        &self,
        creds: &CredentialVault,
    ) -> Result<()>;
}
```

**Where**: `src/output/mod.rs` + `src/output/traits.rs`.

**Design principles**:
- **Outputs run on the right side**: TSDB outputs run on collectors (raw counters); detection/event outputs run on core (aggregated context)
- **Outputs never modify the bus** — read-only subscribers
- **One adapter per (output system, configuration)** — multiple Splunk endpoints means multiple adapter instances
- **Failure is isolated** — one adapter failing does not affect others or the bus

**Done when**: Trait compiles; one stub adapter passes integration tests.

### T6-2 (v9) — Prometheus remote-write adapter (collector-side)

**What**: first concrete adapter. Subscribes to counter summary topic on collector. Transforms summaries to Prometheus remote-write format. Pushes to operator-configured endpoint.

**Why first**: most operators in the controller-less audience already run Prometheus or compatible (Mimir, Thanos, Cortex). This is the immediate win.

**Where**: `src/output/prometheus.rs`.

**Labels**: enriched at adapter time. `device`, `interface`, `vendor`, `role`, `site`, `environment`. Application context available only on core, so this adapter runs collector-side without business context. (An optional second pass on core can add business-aware metrics for a smaller set of golden signals.)

**Done when**: Distributed compose with a Prometheus container shows bonsai metrics with all labels populated; Grafana dashboard JSON in `docs/grafana/`.

### T6-3 (v9) — Splunk HEC adapter (core-side)

**What**: second concrete adapter. Subscribes to detection events + remediation outcomes on core. Pushes to Splunk HTTP Event Collector. JSON payload includes full event + business context from enrichment.

**Where**: `src/output/splunk_hec.rs`.

**Why core-side**: detection events are aggregated at core; pushing from collectors would fragment the stream and lose cross-collector correlation context.

**Done when**: A detection event lands in Splunk within seconds; payload validates against Splunk schema; UI shows adapter health.

### T6-4 (v9) — Elastic ingest adapter (core-side)

**What**: third concrete. Same pattern as Splunk but for Elastic.

**Where**: `src/output/elastic.rs`.

**Done when**: Detection events appear in Elastic; ECS-compliant field mapping; index lifecycle policy documented.

### T6-5 (v9) — ServiceNow ITOM Event Management adapter (core-side)

**What**: This is what T2-4 v9 implements — `em_event` push. As OutputAdapter rather than a one-off, it inherits all the discipline (audit, environment scope, vault, test connection).

**Where**: `src/output/servicenow_em.rs`. T2-4 v9 work refactored to fit the trait once T6-1 lands.

### T6-6 (v9) — Adapter management UI

**What**: extension to `/operations` or new `/integrations` route. Shows all configured adapters, their health, throughput, last-run/last-error.

**Where**: UI work.

**Done when**: Operator can configure, test, enable/disable adapters from the UI; sees throughput per adapter; gets alerted on adapter failure.

### T6-7 (v9) — AIOps readiness checklist

**What**: a documented gate that says "bonsai is ready to feed AIOps platforms when X, Y, Z." Helps operators decide when to point ServiceNow ITOM (or Moogsoft, BigPanda) at bonsai.

**Criteria**:
- Detection events stable (low false-positive rate per (env, rule) — measurable from history)
- Trust model populated for the environment (Tier 3 v9)
- Enrichment producing business-context labels (Tier 1/2 v9)
- Audit log retention satisfies operator's compliance requirement
- Output adapter health green for last 7 days

**Where**: `docs/aiops_readiness.md` + a dashboard panel in `/operations`.

**Done when**: A self-check produces a green/amber/red status; operator knows whether bonsai is ready to be a real AIOps source.

---

## <a id="tier-7"></a>TIER 7 — Signals (carry from v8 T5)

(Unchanged — syslog + SNMP traps as signal collectors, signal-aware detectors, signal-triggered investigations.)

Sequenced after enrichment because business-context graph + trust model makes signal-driven detection meaningfully better.

---

## <a id="tier-8"></a>TIER 8 — Path A → Path B GNN (carry from v8 T6)

(Unchanged. Path A graph embeddings as stepping stone; Path B PyTorch Geometric GNN as destination. By the time Path B starts, the graph carries gNMI state + NetBox structure + ServiceNow business context + environment archetype + path profile metadata + trust outcomes. Trust-history graph features become useful GNN inputs.)

---

## <a id="tier-9"></a>TIER 9 — Investigation Agent (carry from v8 T7)

(Unchanged. LangGraph scaffolding. Tool surface gets richer with v9: agent can query trust state for proposals, agent can suggest playbook trust upgrades after observing successful runs. Mandatory human approval still binding.)

---

## <a id="tier-10"></a>TIER 10 — Controller Adapters (demand-driven, carry from v8 T8)

(Unchanged. Trait design only as a low-cost artifact. Implementations demand-driven.)

---

## <a id="tier-11"></a>TIER 11 — Scale, Extensions, Polish

### T11-1 — Scale architecture doc
### T11-2 — Core bottleneck profiling (post-Tier 6 — output adapters change the load shape)
### T11-3 — S3-compatible archive backend
### T11-4 — Disconnected-ops capability flag
### T11-5 — NL query layer
### T11-6 — ML feature schema versioning
### T11-7 — Multi-arch build verification
### T11-8 — Map visualisation
### T11-9 — Bulk onboarding CSV
### T11-10 — Core-to-collector request-ID tracing
### T11-11 — Documentation website with environment-archetype tutorials

---

## <a id="tier-12"></a>TIER 12 — Deferred Until Forced

- Bitemporal schema
- Schema migration tooling
- Grafeo migration evaluation
- Workspace split (current incremental build is fine)
- Kubernetes deployment manifests
- Auth/RBAC
- Multi-tenancy in the graph
- Production HA for the core

---

## <a id="execution-order"></a>Recommended Execution Order

### Sprint 1 — Tier 0 close-outs + path catalogue research (1-2 weeks)
1. T0-1 v9 semver-aware plugin compare
2. T0-2 v9 BonsaiStore in enrichment trait
3. T0-3 v9 record_run audit logging
4. T0-4 v9 first-run auto-redirect
5. T0-5 v9 Setup wizard skip path
6. T0-6 v9 remove `inferred_environment_for_role`
7. T0-7 v9 catalogue gap-fill (7 new profiles + docs)
8. T0-8 v9 spot-verify v8 "assumed" items

### Sprint 2 — NetBox enricher implementation (2 weeks)
9. T1-1 v9 NetBox enricher
10. T1-2 v9 enrichment visibility in DeviceDrawer
11. T1-3 v9 MCP client infrastructure

### Sprint 3 — ServiceNow PDI integration (2-3 weeks) ⚡
12. T2-1 v9 PDI configuration surface
13. T2-2 v9 PDI seed automation
14. T2-3 v9 ServiceNow CMDB enricher
15. T2-4 v9 ServiceNow Event Management push (one-off, refactored later)
16. T2-5 v9 incident state consumption
17. T2-6 v9 event filter policy

### Sprint 4 — Human-in-the-loop graduated remediation (2-3 weeks)
18. T3-1 v9 TrustState model
19. T3-5 v9 per-rule per-environment defaults
20. T3-2 v9 Pending Approvals workspace
21. T3-3 v9 graduation logic
22. T3-4 v9 rollback window
23. T3-6 v9 trust audit

### Sprint 5 — Operator path overrides (1-2 weeks)
24. T4-1 v9 override scopes (site / role-env / device)
25. T4-2 v9 override management UI
26. T4-3 v9 subscription resolution audit

### Sprint 6 — YANG path discovery (1-2 weeks)
27. T5-1 v9 path discovery script
28. T5-3 v9 doc generation
29. T5-2 v9 plugin install command

### Sprint 7 — OutputAdapter foundation + Prometheus (2 weeks)
30. T6-1 v9 OutputAdapter trait
31. T6-2 v9 Prometheus remote-write adapter
32. T6-6 v9 adapter management UI

### Sprint 8 — Splunk + Elastic adapters (1-2 weeks)
33. T6-3 v9 Splunk HEC
34. T6-4 v9 Elastic ingest

### Sprint 9 — ServiceNow EM as proper OutputAdapter + AIOps readiness (1 week)
35. T6-5 v9 refactor T2-4 to OutputAdapter
36. T6-7 v9 AIOps readiness checklist

### Sprint 10 — Signals (2 weeks)
37. v8 T5-1 through T5-5 (syslog + traps)

### Sprint 11 — Path A embeddings + ML schema versioning (1-2 weeks)
38. v8 T6-1 + T11-6 v9

### Sprint 12 — NL query (1 week)
39. T11-5 v9 NL query

### Sprint 13 — Investigation agent (2-3 weeks)
40. v8 T7-1 through T7-4 (with trust-state-aware tools)

### Sprint 14 — Path B GNN (3-4 weeks)
41. v8 T6-2 + T6-3 (with trust-history features)

### Longer horizon
- Controller adapters (demand-driven)
- Other v8 T6/T7/T8 carryovers
- T11 polish

### Deferred until forced
Tier 12 items.

---

## <a id="guardrails"></a>Guardrails — Updated for v9

### New in v9

- **HIL is a graduated path, not a binary switch.** Every remediation flows through TrustState. Operators graduate trust deliberately, never automatically. No tuple ever skips ApproveEach without operator opt-in.
- **Production credentials never co-exist with dev creds.** PDI admin creds for development must never be used in production deployments. Production ServiceNow integration uses scoped roles (itil + event_creator), not admin.
- **Output adapters are read-only on the bus.** Subscribers, never modifiers. An adapter that needs to influence the graph or the event bus is the wrong abstraction.
- **AIOps readiness is gated.** Bonsai is not "always ready" to feed AIOps platforms — it's ready when detection quality, trust state, and enrichment maturity meet the documented criteria.
- **Path discovery candidates are not catalogue entries.** Discovered paths from public YANG repos are *candidates*. Lab verification is required before promotion to the default catalogue.

### Unchanged from v8 (still binding)

- Audience: controller-less primary, multi-controller correlation secondary
- gNMI only for hot-path telemetry state
- Syslog and traps as signals, never state
- tokio only for async Rust
- Credentials via vault only; resolve carries purpose; audit logged
- No Kubernetes in v0.x
- Every non-trivial decision gets an ADR at commit time
- Detect-heal loop does not call an LLM or any enrichment source synchronously
- All operator-facing functionality lives on core
- Enrichers never call LLMs on device configuration
- Collectors scale horizontally; core scales vertically in v1
- Build time is a first-class metric
- Code landing ≠ work complete
- Distributed mode must run distributed (mTLS, no plaintext)
- Environment awareness is first-class
- Path catalogue is data, not code
- Integration mocks come before integration implementations (mocks remain as CI fallback even when PDI is primary)

### Anti-patterns to reject

- "Skip TrustState; just have an auto/manual flag" — no, the graduated path is the architecture
- "Push everything to AIOps; let them filter" — no, bonsai filters before push (T2-6)
- "Bonsai becomes a CMDB" — no, ServiceNow is the CMDB; bonsai enriches its graph from CMDB
- "Auto-import every YANG path" — no, candidates require lab verification
- "OutputAdapter can write back to the graph" — no, read-only subscribers
- "Production deployments use the PDI for testing" — no, separate environments
- All prior v6/v7/v8 anti-patterns remain in force

---

## What v9 Explicitly Excludes

For scope discipline, do not start:
- Trust state changes that bypass operator approval
- Output adapters that write to the bus
- Auto-import of unverified YANG paths into the default catalogue
- AIOps positioning as competitive with ServiceNow ITOM (we're additive)
- Individual controller adapters (still demand-driven from v7)
- LLM-based parsing of device configuration anywhere outside the investigation agent
- Auth/RBAC, multi-tenancy, production HA, Kubernetes
- Workspace split, bitemporal schema, schema migration, Grafeo eval

---

*Version 9.0 — authored 2026-04-25 after reviewing post-v8 main. Verifies substantial v8 progress (audit, catalogue with plugin loader, enrichment foundation with trait + registry + audit bridge, Environment graph entity, Setup wizard, four new UI routes, mock + seed infrastructure, hardcoded role lookup gone). Adds five strategic threads: ServiceNow PDI as primary integration target with bidirectional ITOM Event Management flow; HIL graduated remediation with TrustState model and Pending Approvals workspace; operator path overrides at site/role/device granularity; YANG path discovery from public GitHub repos with curation discipline; OutputAdapter integration architecture for TSDB/Splunk/Elastic/AIOps with collector-side metrics and core-side events. Surfaces 8 code quality findings (Q-1 through Q-8) for Tier 0 cleanup. Sequences 14 sprints to keep operator value delivered every iteration through the strategic ML/agent endpoints.*
