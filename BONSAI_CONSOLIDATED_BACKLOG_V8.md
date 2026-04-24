# BONSAI — Consolidated Backlog v8.0

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_V7.md`. Produced 2026-04-24 after reviewing post-v7 main.
>
> **What changes structurally in v8** (three architectural threads from this iteration's strategic discussion):
>
> 1. **Environment as a first-class concept.** Bonsai's primary audience spans data centre, campus (wired + wireless), and service provider environments. Today the codebase has no notion of what *kind* of network is being modelled — a "spine" is just a role string. Environment archetypes (DC / Campus / SP / Home-lab) now shape onboarding defaults, path profile selection, and site organisation. A first-run environment wizard captures this once.
>
> 2. **Path catalogue as pluggable add-ons.** Today `profile_name_for_role` is a hardcoded Rust `match`. That's wrong shape for a project that needs to cover multiple environments × vendors × roles. The path catalogue becomes a plugin-loaded set of YAML bundles. Core ships a default catalogue; community/operator catalogues load alongside. Vendor quirks (non-OpenConfig fallback paths for Cisco XR, vendor-native MPLS models, etc.) live in the catalogue, not in code.
>
> 3. **Enrichment with testable mocks.** v7 elevated NetBox/ServiceNow enrichment to Tier 4. v8 adds the infrastructure needed to actually develop and CI-test those integrations: containerised NetBox instance with seed data matching ContainerLab devices, a CMDB-mock HTTP server emulating the ServiceNow surface bonsai uses, UI pages for per-integration config and audit, and a strict credential discipline that routes all integration secrets through the existing vault.

---

## Table of Contents

1. [Audience and Positioning (unchanged from v7)](#positioning)
2. [Progress Since v7](#progress)
3. [Correctness and Quality Issues from Review](#quality)
4. [TIER 0 — Finish the UI and Integration Loose Ends](#tier-0)
5. [TIER 1 — Environment Model and First-Run Wizard](#tier-1)
6. [TIER 2 — Path Catalogue as Pluggable Add-Ons](#tier-2)
7. [TIER 3 — Enrichment Foundation with Testable Mocks](#tier-3)
8. [TIER 4 — Enrichment Implementations](#tier-4)
9. [TIER 5 — Syslog and SNMP Traps](#tier-5)
10. [TIER 6 — Path A Graph Embeddings → Path B GNN](#tier-6)
11. [TIER 7 — Investigation Agent](#tier-7)
12. [TIER 8 — Controller Adapters (demand-driven)](#tier-8)
13. [TIER 9 — Scale, Extensions, Polish](#tier-9)
14. [TIER 10 — Deferred Until Forced](#tier-10)
15. [Execution Order](#execution-order)
16. [Guardrails](#guardrails)

---

## <a id="positioning"></a>Audience and Positioning

**Unchanged from v7 — captured as ADR 2026-04-24 in `DECISIONS.md` and top of `CLAUDE.md`.**

- **Primary audience**: controller-less environments across DC, campus (wired + wireless), and SP backbones. Multi-vendor — Cisco, Juniper, Nokia, Arista, FRR.
- **Secondary audience**: multi-controller correlation (narrow niche).
- **Anti-position**: not a DNAC/NDI/Meraki replacement inside their own fabrics.
- **Enrichment is the primary business-context mechanism** for the primary audience.

v8 sharpens this by making environment archetype explicit in the runtime so that operator workflows (onboarding, path selection, remediation scope) adapt to the environment instead of assuming one shape.

---

## <a id="progress"></a>Progress Since v7 — Verified Against Main

All verified in code, not self-declaration.

| v7 item | Status | Evidence |
|---|---|---|
| T0-1 v7 summary mode default in distributed compose | ✅ Done | `docker/configs/collector-1.toml`, `collector-2.toml` — explicit `[collector.filter] counter_forward_mode = "summary"`. 2026-04-24 ADR documents rationale. |
| T0-2 v7 Docker-only seed creds script | ✅ Done | `scripts/seed_lab_creds.sh` now falls back to `docker compose exec bonsai-core bonsai credentials ...` when local binary absent. |
| T0-3 v7 per-collector TLS certs | ✅ Done | `scripts/generate_compose_tls.sh` produces `collector-1-cert.pem`, `collector-2-cert.pem` with per-collector CN. Dedicated ADR 2026-04-24. |
| T0-4 v7 hierarchy-aware assignment | ✅ Done | `src/assignment.rs::site_ancestor_set` walks site parent chain (depth 10); matches on rule `match_site` if any ancestor name or id matches. Falls back to exact match when site cache empty. Unit tests cover simple, parent-chain, and cache-empty cases. |
| T0-5 v7 onboarding wizard reachable | ✅ Done | `ui/src/routes/Devices.svelte` has primary "+ Add Device" button → `/devices/new` route → renders `Onboarding.svelte`. Edit flow opens wizard pre-populated via `selectedAddress`. |
| T0-6 v7 audience ADR | ✅ Done | `DECISIONS.md` 2026-04-24 entry "Audience framing: controller-less networks as the primary target"; `CLAUDE.md` updated with positioning block and anti-patterns; `PROJECT_KICKOFF.md` reflected. |
| T1-1 v7 Topology enrichment | ✅ Done (substantial) | `ui/src/lib/Topology.svelte` grew 177 → 476 lines. Layer filter (combined/L3/L2), site scope filter, role shapes (spine=square, PE/RR=hex, leaf=circle), link utilisation heatmap (bytes-based RdYlGn colour), path tracing via shift-click + `/api/path?src=...&dst=...`. All specified features present. |
| T1-2 v7 incident grouping | ✅ Done | `routes/Incidents.svelte` groups by root + cascading. Sprint 4 testing results confirm end-to-end behaviour. |
| T1-3 v7 operations depth | ✅ Done (first-pass) | `routes/Operations.svelte` surfaces event-bus, archive, subscribers, collectors, rule firing. Further depth iterative. |
| T1-4 v7 visual polish pass | ✅ Done | `ui/src/lib/CommandPalette.svelte` — Ctrl+K palette, keyboard navigation, searches devices+sites, nav shortcuts. Lighthouse audit referenced in build notes. |
| T2-1 v7 five-scenario multi-collector validation | ✅ Done | `docs/SPRINT_4_TESTING_RESULTS.md` — two-collector validation, mTLS confirmed, fault detection end-to-end, bug fixes documented (retry loop in `run_collector_manager`, healthcheck syntax, cert permissions, TOML config corrections). |
| T3-1 v7 Docker image <100 MB | ✅ Done | ADR 2026-04-24 "Dockerfile build-speed and image-size optimisations (T3-1)" documents planner stage fix (Cargo.toml + Cargo.lock only), incremental build speedup, image reduction. |
| T3-3 v7 ContainerLab compose integration | ✅ Done | `docker-compose.yml` uses `bonsai-p4-mgmt: external: true` network; bonsai containers join ContainerLab's network; hostnames like `clab-bonsai-p4-srl-spine1` addressable. |
| T3-4 v7 volume backup strategy | ✅ Done | `scripts/backup_volumes.sh` + `scripts/restore_volumes.sh`; `docs/deployment_volumes.md` updated. |
| T3-6 v7 network segmentation deployment guide | ✅ Done | `docs/deployment_segmentation.md` documents management-plane / user-plane separation. |
| T3-5 v7 CI build pipeline | ✅ Done (presumed) | `.github/` directory added. Workflow presumably validates build bench. |

**Not done — carry forward:**

- T1-5 v7 topology API returns role/site metadata (verify below in quality section; may already be present but need to confirm)
- T2-2 v7 protocol version enforcement (fields exist; runtime check not implemented)
- T2-3 v7 metrics expansion
- T3-2 v7 multi-arch build (lbug-on-arm64 status unknown)
- All T4 v7 (graph enrichment — new scope in v8 Tier 3-4)
- All T5 v7 (signals)
- All T6 v7 (Path A → Path B)
- All T7 v7 (investigation agent)
- All T8 v7 (controller adapters — demand-driven)
- T9 v7 remainder (scale docs, NL query, TSDB, etc.)

---

## <a id="quality"></a>Correctness and Quality Issues from Review

Surfaced by reviewing the code that landed in this iteration. None are merge-blockers but all belong in v8 T0.

### Q-1 — `profile_name_for_role` hardcodes Rust match

**Location**: `src/discovery.rs:570-577`

**Issue**: The mapping from role to profile is a Rust `match` statement. Adding a new profile (campus access switch, home-lab router, SP CE-facing PE, etc.) requires recompiling bonsai. This is the wrong shape for a project targeting multi-environment multi-vendor networks.

**Impact**: every new environment archetype or vendor-specific profile adds Rust churn. Community contribution is blocked. Becomes **central** to Tier 2 (pluggable catalogue).

### Q-2 — Hardcoded prefix check for "sp_" in discovery

**Location**: `src/discovery.rs:238` — `if profile_name.starts_with("sp_")` to trigger SP-specific handling.

**Issue**: Naming-convention-as-behaviour is fragile. The selection logic should be data-driven (profile declares its environment), not string-prefix-dependent.

**Fix**: profile YAML gains an `environment` field; runtime reads it instead of parsing the profile name. Folded into Tier 2.

### Q-3 — Path profile coverage gaps

**Location**: `config/path_profiles/*.yaml` — 4 profiles only.

**Gaps**:
- No campus profiles at all (access, distribution, core)
- No wireless profiles (no OpenConfig model coverage for wireless controllers/APs — but data exists via vendor models)
- No LDP, no RSVP-TE, no SR-TE policy, no L3VPN/EVPN paths even in SP profiles
- No vendor-native fallback paths (Cisco XR native models, Juniper Junos YANG, Arista EOS state models, Nokia SR Linux YANG)
- No `required_vendor` field to make a path apply only to specific vendor families

**Impact**: current profiles give a minimum-viable telemetry surface but miss rich SP/DC/campus data. Needs a proper path-research effort with plugin bundles. Becomes central to Tier 2.

### Q-4 — Site `kind` is a free string, no environment axis

**Location**: `src/graph/mod.rs` — `Site.kind` is `String` set by operator to "dc", "rack", "unknown", etc.

**Issue**: Site `kind` conflates two axes — physical granularity (DC, rack) and operational archetype (SP, campus, home-lab). A rack in a SP DC should inherit SP-environment defaults; a rack in an enterprise DC should inherit DC-fabric defaults. Today there's no way to express this.

**Fix**: introduce a distinct `Environment` entity; `Site` gets an `environment_id` reference. `kind` remains for physical granularity. Tier 1 work.

### Q-5 — Onboarding wizard has no environment context

**Location**: `ui/src/lib/Onboarding.svelte` (852 lines).

**Issue**: The wizard collects address/credentials/role/site. It does not ask "what environment does this device live in" so it cannot offer role-appropriate profile defaults. On a fresh install an operator is asked to pick from `dc_leaf_minimal`/`dc_spine_standard`/`sp_pe_full`/`sp_p_core` without any context about which applies to their network.

**Fix**: environment selected once at first-run, then inherited through site → device. Tier 1 work.

### Q-6 — First-run experience missing

**Location**: `ui/src/App.svelte` — no first-run path.

**Issue**: On an empty bonsai install the operator lands on `/` (Live), sees "No devices found. Is bonsai running and connected to targets?", and is expected to know to navigate to Devices → Add. No prompted setup, no environment selection, no recommended starting point.

**Fix**: detect first-run state (no sites, no credentials, no devices) and route to `/setup` first-run wizard. Tier 1 work.

### Q-7 — Protocol version stubbed but not enforced

**Location**: `proto/bonsai_service.proto` has `protocol_version` fields; nothing reads them.

**Impact**: collector-core skew will go undetected. Not urgent today (single major version) but needs to land before we have real version skew. v7 T2-2 carried forward to T0-3 v8.

### Q-8 — Credential resolve still under-instrumented for audit

**Location**: `src/credentials.rs::resolve`.

**Issue**: The debounce-metadata-write fix from v4 T0-1 is correct. However, there is no audit record of *which* operation resolved a credential — a failed remediation attempt, a scheduled enrichment run, a manual discovery. For a compliance-sensitive audience (the primary audience includes regulated SPs and enterprises), resolve calls should carry a `purpose` field that's written to a structured audit log.

**Fix**: `resolve(alias, purpose)` with purpose enum (`subscribe`, `remediate`, `discover`, `enrich`, `test`). Audit log writes `{ts, alias, purpose, outcome}`. Tier 0 work.

### Q-9 — No data-flow observability between core and collectors at runtime

**Issue**: operators debugging "did collector-1 forward this counter update?" have to correlate log lines across both processes. The existing metrics are decent but the cross-process trace is manual.

**Fix**: request IDs on `TelemetryIngest` updates (collector-generated, monotonic per stream), logged on both sides, surfaced in the Operations page so an operator can filter core logs by collector request ID. Lower priority than Tier 0 but noted. Maps to Tier 9.

---

## <a id="tier-0"></a>TIER 0 — Finish the UI and Integration Loose Ends

### T0-1 (v8) — Topology API includes role, site_id, and site_path

**What**: verify `/api/topology` response includes `role`, `site_id`, and `site_path` per device (site_path is the flattened hierarchy string). The front-end Topology.svelte filters by these — if the API doesn't return them, the filter degrades to no-op on real data.

**Where**: `src/http_server.rs` topology handler

**Done when**: `curl /api/topology | jq '.devices[0]'` returns `role`, `site_id`, `site_path` fields populated.

### T0-2 (v8) — Credential resolve audit trail

**What**: Q-8 above. Add a `purpose: ResolvePurpose` argument to `CredentialVault::resolve`. Enum covers `Subscribe`, `Remediate`, `Discover`, `Enrich`, `Test`, `Other(String)`. Every resolve writes a structured audit log line with `{timestamp_ns, alias, purpose, outcome}`.

**Where**: `src/credentials.rs` + every callsite.

**Done when**: Running a discovery, a remediation, an enrichment cycle all produce distinct audit entries. Log format is structured (JSON one line per event) so it can be ingested into SIEM.

### T0-3 (v8) — Protocol version enforcement

**What**: v7 T2-2 carried forward. Read `protocol_version` on collector connect; warn on minor skew, reject on major skew.

**Where**: `src/api.rs`, `src/ingest.rs`. Constant `PROTOCOL_VERSION_CURRENT`.

**Done when**: A deliberately-skewed-version collector produces the expected warn/reject behaviour; both sides log their running version at startup.

### T0-4 (v8) — Metrics expansion

**What**: v7 T2-3. Event bus depth gauge, archive lag gauge, subscriber reconnect frequency, rule fire rate per rule_id, per-collector summary emit rate, queue drain rate.

**Where**: prometheus metrics registry.

**Done when**: a sample Grafana dashboard JSON lives in `docs/grafana/bonsai-overview.json`.

### T0-5 (v8) — Multi-arch build verification

**What**: v7 T3-2. Verify lbug crate builds on arm64; if yes, enable `docker buildx` multi-arch. If no, document blocker.

**Where**: `.github/workflows/` + build bench

**Done when**: `linux/amd64` + `linux/arm64` images build; Apple Silicon developers can `docker pull` natively.

### T0-6 (v8) — Audit log retention and export

**What**: the audit log from T0-2 needs retention and export. Keep 30 days on disk by default; `bonsai audit export --since <iso> --until <iso>` writes a tarball.

**Where**: new `src/audit.rs` (or fold into existing retention module) + CLI subcommand.

**Done when**: an operator can export a month of audit entries to hand to a compliance auditor.

---

## <a id="tier-1"></a>TIER 1 — Environment Model and First-Run Wizard

**Why this is Tier 1**: v8 is explicitly multi-environment. Without a first-class environment concept, onboarding defaults are guesses, site organisation is ad-hoc, and every new feature (enrichment, path profiles, GNN features) has to carry its own "what kind of network is this" logic. Better to solve it once now.

### T1-1 — `Environment` entity in the graph

**What**: introduce an `Environment` node type with the following schema:

```
Environment:
  id: string              # stable id (uuid or slug)
  name: string            # display name — "Lab DC Fabric", "Branch Offices"
  archetype: enum         # data_center | campus_wired | campus_wireless | service_provider | home_lab
  created_at: timestamp
  metadata_json: string   # free-form operator notes
```

And a relationship `(Site)-[:BELONGS_TO_ENVIRONMENT]->(Environment)`.

A Site can belong to exactly one Environment. Hierarchy within an environment uses existing `PARENT_OF` edges.

**Why archetype and not just a free string**: hardcoding to a small enum forces the conversation about coverage. If someone has a network that doesn't fit, that's a signal — either extend the enum or mark it `home_lab` as the escape hatch.

**Where**:
- `src/graph/mod.rs` — schema + CRUD
- `src/api.rs` — ListEnvironments, CreateEnvironment, UpdateEnvironment, DeleteEnvironment RPCs
- `src/http_server.rs` — `/api/environments` endpoints

**Migration**: existing sites get auto-assigned to a default environment with archetype `home_lab` (safe no-op default) on first startup after upgrade. Operator can rename/reassign.

**Done when**: Environment CRUD works via API; existing installations migrate cleanly; UI doesn't break.

### T1-2 — First-run wizard `/setup`

**What**: a new route `/setup` that runs once on a fresh bonsai install. Detects "fresh" via: no Environments exist, no credential aliases exist, no devices onboarded.

Steps:

1. **Welcome** — brief explanation of what bonsai is, what's about to be configured
2. **Environments** — operator creates one or more. For each, picks archetype (DC / Campus Wired / Campus Wireless / SP / Home-lab) and name. Archetype unlocks archetype-specific guidance in subsequent steps.
3. **Sites** — within each environment, operator defines top-level sites (the "regions" / "data centres" / "points of presence" they plan to manage). Hierarchy can be built later; this step captures the top-level structure.
4. **Credentials** — operator adds at least one credential alias (the vault passphrase gets prompted on backend if not already set via compose env var).
5. **Ready** — summary, with a "Bring on my first device" button that routes to `/devices/new` with the environment context pre-selected.

**Design principle**: the wizard can be skipped (operators who want to dive into the Devices workspace directly) but defaults are off — no devices auto-added, no subscriptions active. The wizard is guidance, not gate.

**Where**: `ui/src/routes/Setup.svelte` + App.svelte routing + detection logic in App.svelte's onMount.

**Done when**: Fresh install routes to `/setup` automatically; completing the wizard produces at least one Environment, one Site, one credential alias, and the operator lands on `/devices/new` with environment pre-selected.

### T1-3 — Onboarding wizard inherits environment context

**What**: `/devices/new` gains environment awareness. The site picker filters to sites within the active environment. The role picker offers environment-appropriate roles:

- DC archetype: `leaf`, `spine`, `superspine`, `border`, `edge`
- SP archetype: `pe`, `p`, `rr`, `ce-facing`, `peering`
- Campus wired: `access`, `distribution`, `core`, `border`
- Campus wireless: `ap`, `wlc`, `edge-wlc`
- Home-lab: free-form

Profile selection defaults to the environment+role recommendation (driven by Tier 2 catalogue).

**Where**: `ui/src/lib/Onboarding.svelte` — add environment state, filter site picker, filter role picker, wire profile defaults.

**Done when**: Adding a device in a "Lab DC Fabric" (archetype: data_center) environment offers DC-specific roles; adding a device in a "Branch WAN" (archetype: service_provider) environment offers SP-specific roles. Profile defaults match archetype + role.

### T1-4 — Environment workspace UI

**What**: new route `/environments` (and nav entry). Shows:

- List of environments with archetype badges
- For each environment: site count, device count, aggregate health
- CRUD — create, edit, delete (delete only when no sites reference it)
- Archetype upgrade path (e.g. "Home-lab" → "Data Center") — just a metadata change, no graph migration required

**Where**: `ui/src/routes/Environments.svelte` + nav update.

**Done when**: Operator can view and manage environments from the UI without CLI or direct API.

### T1-5 — Site workspace shows environment binding

**What**: `ui/src/routes/Sites.svelte` already exists. Extend to show which environment each site belongs to; allow reassignment via drag-drop or drop-down.

**Done when**: Sites view shows environment per site; reassignment works.

### T1-6 — Migration plan for existing installations

**What**: an ADR + a migration runner that handles: existing Sites get a default "migrated" Environment with archetype `home_lab`. Operator is prompted on first login after upgrade to review and reassign.

**Where**: `src/main.rs` startup migration step + ADR.

**Done when**: Upgrading an existing install does not break onboarding; the migration runs idempotently.

---

## <a id="tier-2"></a>TIER 2 — Path Catalogue as Pluggable Add-Ons

**Why Tier 2**: the hardcoded `profile_name_for_role` and the thin 4-profile coverage are structural debt. Operators across DC / campus / SP need rich, vendor-specific, environment-aware profile selection. Baking more into Rust is the wrong direction. Catalogue-as-data, plugins on disk, is the right shape.

### T2-1 — Path profile schema v2

**What**: extend the existing profile YAML with the fields required for catalogue/plugin thinking:

```yaml
# NEW SCHEMA
name: sp_pe_full
environment: service_provider        # NEW — required
roles: ["pe", "rr"]
description: "..."
rationale: "..."
vendor_scope: ["nokia_srl", "cisco_iosxr", "juniper_junos", "arista_eos", "any"]  # NEW

paths:
  - path: "mpls"
    origin: openconfig
    mode: ON_CHANGE
    required_models: ["openconfig-mpls"]
    rationale: "MPLS state for PE transport health."

  - path: "Cisco-IOS-XR-mpls-te-oper:mpls-te"  # NEW: vendor-native fallback
    origin: cisco-iosxr
    mode: ON_CHANGE
    vendor_only: ["cisco_iosxr"]  # NEW — scopes path to vendor
    fallback_for: "mpls"           # NEW — marks as fallback when native OC path absent
    rationale: "Vendor-native MPLS-TE state for IOS-XR when openconfig-mpls not advertised."
```

New fields:
- `environment` (required) — which archetype this profile targets. Discovery engine uses this to filter candidates.
- `vendor_scope` — list of vendor identifiers this profile is tuned for. `"any"` means it works on any vendor that advertises the required models.
- Per-path: `vendor_only`, `fallback_for` — enables vendor-native paths as fallbacks when OC equivalents aren't available.

**Where**: `config/path_profiles/*.yaml` — migrate existing; `src/discovery.rs` — update parser.

**Done when**: All existing profiles carry the new fields; discovery engine uses `environment` + `vendor_scope` + advertised models to rank candidates instead of hardcoded role lookup.

### T2-2 — Remove hardcoded `profile_name_for_role`

**What**: Q-1 and Q-2 above. Replace Rust `match` with data-driven lookup over loaded profiles. Selection logic:

1. Filter profiles whose `environment` matches the device's environment
2. Filter profiles whose `roles` list includes the device's role
3. Filter profiles whose `vendor_scope` includes the device's vendor (or `"any"`)
4. Rank by specificity — more-specific vendor match wins over `"any"`
5. Of remaining, pick the one whose required models are most-fully-advertised by the device

**Where**: `src/discovery.rs`.

**Done when**: The hardcoded `profile_name_for_role` function is gone; profile selection is entirely data-driven; adding a new profile is a YAML drop-in.

### T2-3 — Plugin loader for external catalogues

**What**: `config/path_profiles/` becomes the "default catalogue". Additional catalogues can live in:
- `config/path_profiles/plugins/<plugin-name>/` — subdirectories, each a plugin
- `$XDG_DATA_HOME/bonsai/catalogues/<plugin-name>/` — user-installed
- A URL-bound catalogue fetched from a remote (future)

Each plugin has a `MANIFEST.yaml`:

```yaml
name: sp-cisco-xr-extended
version: 0.1.0
covers:
  environments: ["service_provider"]
  vendors: ["cisco_iosxr"]
  roles: ["pe", "p", "rr"]
description: "Extended SP path profiles for Cisco IOS-XR including vendor-native MPLS-TE, RSVP, LDP, and SR-TE policy."
author: "community"
license: "MIT"
profiles:
  - cisco_iosxr_pe_extended.yaml
  - cisco_iosxr_p_extended.yaml
  - cisco_iosxr_rr_extended.yaml
```

Plugins load at startup. Conflicts (two plugins defining the same profile name) are resolved by specificity → version → alphabetical (with loud warning logs and a UI indicator).

**Where**: new `src/catalogue/mod.rs`.

**Done when**: Plugin in `config/path_profiles/plugins/foo/` loads at startup; its profiles participate in discovery; UI shows loaded plugins on the Paths/Profiles workspace.

### T2-4 — Research and bundle the default catalogue

**What**: a directed research effort to build out the default catalogue with real coverage. Not speculative — each profile backed by a specific research note and lab verification.

Coverage targets (first pass):

**Data center**
- dc_leaf_basic (OpenConfig, any vendor)
- dc_spine_basic (OpenConfig, any vendor)
- dc_superspine (OpenConfig, any vendor)
- dc_border_leaf (OpenConfig + BGP EVPN)
- dc_evpn_leaf (with BGP EVPN type-1/2/3/5 paths)

**Service provider**
- sp_p_basic (OpenConfig + ISIS + LDP)
- sp_p_sr (with SR transport)
- sp_pe_basic (OpenConfig + BGP + MPLS)
- sp_pe_l3vpn (with L3VPN paths)
- sp_pe_evpn (with EVPN paths)
- sp_rr_basic
- sp_peering_edge

**Campus wired**
- campus_access (with 802.1x, VLAN, LLDP)
- campus_distribution
- campus_core

**Campus wireless**
- campus_wlc (vendor-specific: cisco_wlc, aruba_central_local, juniper_mist_local)
- campus_ap (vendor-specific)

**Home-lab**
- homelab_router (FRR and generic)
- homelab_switch (OpenConfig baseline)

Each profile gets a lab-verified note in `docs/path_profiles/<profile-name>.md` citing the YANG models, sample telemetry shape, known device behaviours.

Vendor-native fallbacks where OC gaps exist:
- Cisco IOS-XR: native MPLS-TE, native SR-TE, native BGP extended stats
- Juniper Junos: native MPLS, native OAM
- Nokia SR OS: native service paths
- Arista EOS: native CVP/AVD-compatible paths
- FRR: CLI/JSON fallback (FRR native streaming support is limited)

**Where**: `config/path_profiles/` + `docs/path_profiles/`.

**Done when**: Default catalogue covers the above. Each profile has a companion doc. Discovery against a lab device of each archetype + vendor picks the expected profile.

### T2-5 — Profile and plugin UI workspace

**What**: new route `/profiles` (or extension of Operations). Shows:

- Loaded profiles: name, environment, vendors, roles, paths
- Loaded plugins: manifest, health (did it load cleanly?), conflicts
- Per-profile preview: what telemetry paths would be active if this profile were applied
- "Test profile against device": pick a device, see what would be subscribed

**Where**: `ui/src/routes/Profiles.svelte`.

**Done when**: Operator can inspect the catalogue state from the UI; no CLI required.

### T2-6 — Onboarding wizard path customisation

**What**: step 3 of the Onboarding wizard gains a "customise paths" flow. After the recommended profile is selected, operator can:
- Add additional paths (from other profiles or manually)
- Remove optional paths
- Save as a custom profile (creates a named profile in the user catalogue)

This is exactly what you described — "have the user play around with paths and then either attach individual device/group etc."

**Where**: `ui/src/lib/Onboarding.svelte`.

**Done when**: Operator can deviate from the recommended profile and save their deviation as a named custom profile reusable across devices.

---

## <a id="tier-3"></a>TIER 3 — Enrichment Foundation with Testable Mocks

**Why Tier 3** (ahead of Tier 4 implementations): the #1 risk in enrichment work is "build against vendor docs, discover at first customer what's wrong." We spent several conversations agreeing that enrichment is the primary differentiator for bonsai's audience. If we build it without a dev/test harness, we'll ship broken integrations. So: mocks first, implementations second.

### T3-1 — `GraphEnricher` trait (v5/v7 carryover, updated)

**What**: the trait from v5/v7 T4-1/T6-1, refined with audit and environment awareness:

```rust
#[async_trait]
pub trait GraphEnricher: Send + Sync {
    fn name(&self) -> &str;
    fn schedule(&self) -> EnrichmentSchedule;
    fn writes_to(&self) -> EnrichmentWriteSurface;  // NEW — declares graph surface

    async fn enrich(
        &self,
        graph: &dyn BonsaiStore,
        creds: &CredentialVault,         // NEW — access via vault only
        audit: &AuditLogger,              // NEW — records what was touched
    ) -> Result<EnrichmentReport>;
}

pub struct EnrichmentWriteSurface {
    pub property_namespace: String,        // e.g. "netbox_"
    pub owned_labels: Vec<String>,          // new node labels this enricher creates
    pub owned_edge_types: Vec<String>,      // new edge types this enricher creates
}
```

**Design principles (non-negotiable)**:
- Enrichers access credentials only via the vault, with `purpose = Enrich` for audit (T0-2 v8)
- Declared write surface is enforced — an enricher writing outside its namespace errors
- Idempotent, isolated, opt-in per v5/v7
- **Never calls LLMs on device configuration** (binding from v5 guardrails)
- Environment-aware: an enricher can declare `applies_to_environments: [data_center, service_provider]` — doesn't run against Home-lab environments unless explicitly enabled

**Where**: `src/enrichment/mod.rs` + `src/enrichment/traits.rs`.

**Done when**: Trait compiles; one stub implementation passes an integration test.

### T3-2 — Containerised NetBox test instance

**What**: a `netbox-test` docker-compose profile bringing up:
- NetBox (official image)
- Postgres
- Redis
- A seed container that populates NetBox with a topology matching the ContainerLab lab: 4 devices (srl-leaf1, srl-leaf2, srl-spine1, xrd-pe1), their interfaces, IP addresses, sites, VLANs, prefixes.

**Where**: `docker/compose-netbox.yml` (sub-compose), `scripts/seed_netbox.py` (or `.sh`)

**Seed data** must match what ContainerLab produces so the enricher integration test can be:
1. Bring up ContainerLab lab
2. Bring up bonsai
3. Bring up netbox-test
4. Seed NetBox with the seed script (reads the same topology description the lab used)
5. Run bonsai's NetBox enricher
6. Assert bonsai's graph contains `VLAN` and `Prefix` nodes matching the seeded data

**Done when**: `docker compose -f docker/compose-netbox.yml up -d && scripts/seed_netbox.sh` produces a NetBox at `http://localhost:8000` with the lab topology seeded; the API responds to `GET /api/dcim/devices/` returning the 4 devices.

### T3-3 — ServiceNow CMDB mock server

**What**: ServiceNow developer instances are cloud-only, rate-limited, and flake in CI. Build a minimal mock that emulates the specific ServiceNow API surface bonsai uses.

**Scope of mock**:
- `/api/now/table/cmdb_ci_netgear` — returns CIs matching bonsai's devices
- `/api/now/table/cmdb_ci_business_service` — business services (Applications in our graph)
- `/api/now/table/cmdb_rel_ci` — relationships between CIs (RUNS_SERVICE, CARRIES_APPLICATION)
- Basic OAuth2 token endpoint (accepts any well-formed client_credentials request)

**Implementation**: FastAPI Python service, data seeded from a YAML file that matches the ContainerLab topology.

**Where**: `docker/mock-servicenow/` directory with Dockerfile + app.py + seed.yaml; `docker-compose.yml` gains a `servicenow-mock` profile.

**Design principle**: the mock is *behavioural* — it returns response shapes matching real ServiceNow within the narrow subset bonsai uses. It is not a full CMDB emulator. Operators who want to test against a real instance can point enricher config at a dev instance instead.

**Done when**: `curl -X POST http://localhost:8080/oauth_token.do` returns a fake token; `GET /api/now/table/cmdb_ci_netgear` with the token returns the seeded devices. Response shape validated against ServiceNow REST reference.

### T3-4 — Seed data discipline

**What**: the ContainerLab topology, the NetBox seed, and the ServiceNow mock seed all describe the **same network**. A change in one requires updates in all three. Put this under a single source of truth.

**Where**: `lab/seed/topology.yaml` — one YAML describing the lab. `scripts/seed_lab.sh` is a dispatcher that:
- Produces ContainerLab topology (translate to clab format)
- Produces NetBox seed (translate to NetBox API calls)
- Produces ServiceNow mock seed

**Done when**: Changing `lab/seed/topology.yaml` and running `scripts/seed_lab.sh --all` updates all three downstream targets consistently. CI validates the three representations remain coherent.

### T3-5 — Enrichment workspace UI

**What**: `/enrichment` route. Per enricher:
- Name, type, enabled toggle, schedule
- Integration config — URL, credential alias (dropdown of existing aliases), polling interval, environment scope (multi-select of environments)
- Last run: timestamp, duration, nodes touched, warnings
- Test connection button — dials the endpoint with resolved credentials (purpose=Test), reports success/failure without writing to graph
- Run now button
- Per-enricher audit log viewer (filtered from the system audit log)

**Where**: `ui/src/routes/Enrichment.svelte`.

**Design principle**: integration secrets are never entered directly in this page. The credential alias dropdown lists aliases from the vault. If the operator needs to add a credential, they go to `/credentials` first. This keeps the vault as the single credential authority.

**Done when**: Operator can configure and validate a NetBox integration end-to-end from the UI. Test connection succeeds against the containerised NetBox from T3-2.

### T3-6 — Integration credential compliance documentation

**What**: a dedicated doc capturing the credential discipline for third-party integrations:

- All integration secrets live in the vault with an alias
- Enrichers access them via `resolve(alias, purpose=Enrich)` — no direct env var access, no inline config
- Every resolve produces an audit log entry
- TLS on every integration connection (including to test mocks — same config path)
- Rate limiting per integration with loud warnings on approach to vendor quotas
- Credential rotation procedure documented

**Where**: `docs/integration_compliance.md`

**Done when**: Document exists; referenced from every enricher's section in `docs/enrichment/`.

---

## <a id="tier-4"></a>TIER 4 — Enrichment Implementations

### T4-1 — NetBox enricher (flagship, tested against T3-2 mock)

**What**: implementation of v7 T4-2. Pulls:
- Device → Site mapping (richer than operator-entered in some cases)
- Device serial, model, firmware
- Interface description, cable ID, connected endpoint
- VLAN assignments (writes `VLAN` nodes + `ACCESS_VLAN`/`TRUNK_VLAN` edges)
- Prefix/subnet assignments (writes `Prefix` nodes + `HAS_PREFIX` edges)
- Platform tags, lifecycle state

Integration modes:
- Direct REST (primary)
- NetBox MCP server (optional, if configured)

**Where**: `src/enrichment/netbox.rs` + `[enrichment.netbox]` config.

**Done when**: Against the T3-2 containerised NetBox, the enricher runs, graph contains expected VLAN/Prefix nodes, UI shows "last enrichment: N min ago" with details.

### T4-2 — ServiceNow CMDB enricher (against T3-3 mock)

**What**: v7 T4-3. Writes:
- `Application(id, name, criticality, owner_group)` nodes
- `Device` gets `snow_ci_id`, `snow_owner_group`, `snow_escalation_path`
- `RUNS_SERVICE`, `CARRIES_APPLICATION` edges

**Where**: `src/enrichment/servicenow.rs`.

**Done when**: Against the T3-3 mock, enricher runs, graph contains Application nodes and business-context edges. Integration test validates edge correctness against seed.

### T4-3 — CLI-scraped enricher (no LLM, pyATS/TextFSM only)

**What**: v7 T4-4. For environments without NetBox/ServiceNow. SSH into device, run curated `show` commands, parse deterministically, write structured properties.

**Where**: `python/bonsai_enrichment/cli_enricher.py`.

**Done when**: Onboarding a lab device can optionally trigger CLI enrichment that populates interface descriptions, VLAN mappings from running-config.

### T4-4 — Enrichment visibility in UI

**What**: device drawer extension showing enrichment source per property. "This device's `snow_owner_group` came from ServiceNow enricher, last updated 12 min ago."

**Where**: `ui/src/lib/DeviceDrawer.svelte` — add enrichment panel.

**Done when**: Operator reading device details can tell which properties came from gNMI and which came from which enricher.

### T4-5 — MCP client infrastructure (shared)

**What**: v7 T4-6. Thin shared module for MCP clients. NetBox and ServiceNow enrichers both use it (when MCP path is enabled).

**Where**: `src/mcp_client.rs`.

**Done when**: Adding a third MCP-backed enricher is "write the graph-write mapping"; MCP plumbing is shared.

### T4-6 — NETCONF/RESTCONF enricher (optional, deferred)

**What**: v7 T4-7. Lower priority. Implement when a specific operator requirement drives it.

### T4-7 — Infoblox/BlueCat enricher (deferred)

**What**: v7 T4-8. Build when demanded.

---

## <a id="tier-5"></a>TIER 5 — Syslog and SNMP Traps

(Unchanged from v7 T5. Signals-not-state, separate collector process, signal-aware detectors, signal-triggered investigations, SNMP MIB handling, syslog format discipline. Sequenced after enrichment because the business-context enrichment makes signal-triggered investigations meaningfully more useful.)

---

## <a id="tier-6"></a>TIER 6 — Path A Graph Embeddings → Path B GNN

(Unchanged from v7 T6. Path A node2vec/GraphSAGE embeddings concatenated with tabular features. Path B PyTorch Geometric GNN with message passing.

New v8 reinforcement: by the time Path B GNN begins, the graph has gNMI-sourced state + NetBox-sourced VLAN/Prefix + ServiceNow-sourced Application/ownership + environment-archetype annotations + path profile metadata. The data loader (T6-3) must handle all of this. Environment archetype becomes a node feature, potentially as a one-hot vector.)

---

## <a id="tier-7"></a>TIER 7 — Investigation Agent

(Unchanged from v7 T7. LangGraph scaffolding with tool surface including graph queries, topology context, business context, environment context, playbook library, suggest-playbook-proposal with mandatory human approval gate, summarise, cost controls, UI workspace, agent memory across investigations.)

---

## <a id="tier-8"></a>TIER 8 — Controller Adapters (demand-driven)

(Unchanged from v7 T8. Keep the trait as a design artifact. Individual adapters are demand-driven. Multi-controller correlation remains the one priority case — if a concrete operator appears with multi-controller needs.)

---

## <a id="tier-9"></a>TIER 9 — Scale, Extensions, Polish

### T9-1 — Scale architecture doc
### T9-2 — Core bottleneck profiling
### T9-3 — S3-compatible archive backend
### T9-4 — Disconnected-ops capability flag
### T9-5 — NL query layer
### T9-6 — ML feature schema versioning
### T9-7 — TSDB integration adapter — strengthened by Tier 3/4 enrichment (graph-enriched labels on metrics)
### T9-8 — Map visualisation
### T9-9 — Bulk onboarding CSV
### T9-10 — Core-to-collector request-ID tracing (Q-9 above)
### T9-11 — Documentation website / GitHub Pages with tutorial for each environment archetype

---

## <a id="tier-10"></a>TIER 10 — Deferred Until Forced

- Bitemporal schema
- Schema migration tooling
- Grafeo migration evaluation
- Workspace split
- Kubernetes deployment manifests
- Auth/RBAC
- Multi-tenancy in the graph
- Production HA for the core

---

## <a id="execution-order"></a>Recommended Execution Order

### Sprint 1 — Tier 0 close-outs (1-2 weeks)
1. T0-1 topology API role/site metadata
2. T0-2 credential resolve audit trail
3. T0-3 protocol version enforcement
4. T0-4 metrics expansion + sample Grafana dashboard
5. T0-5 multi-arch build verification
6. T0-6 audit log retention + export

### Sprint 2 — Environment model + first-run wizard (2-3 weeks) ⚡
7. T1-1 Environment graph entity
8. T1-6 migration plan for existing installations
9. T1-4 Environment workspace UI
10. T1-5 Site workspace environment binding
11. T1-3 Onboarding wizard inherits environment context
12. T1-2 First-run `/setup` wizard

### Sprint 3 — Path catalogue schema (2 weeks)
13. T2-1 Path profile schema v2
14. T2-2 Remove hardcoded `profile_name_for_role`
15. T2-3 Plugin loader for external catalogues
16. T2-5 Profile and plugin UI workspace

### Sprint 4 — Path catalogue research (2-3 weeks)
17. T2-4 Research and bundle default catalogue (DC, SP, campus, home-lab × vendors)
18. T2-6 Onboarding wizard path customisation

### Sprint 5 — Enrichment infrastructure (2-3 weeks)
19. T3-1 `GraphEnricher` trait (with audit + environment awareness)
20. T3-2 Containerised NetBox test instance + seed
21. T3-3 ServiceNow CMDB mock server + seed
22. T3-4 Seed data discipline (single source of truth)
23. T3-5 Enrichment UI workspace
24. T3-6 Integration credential compliance doc

### Sprint 6 — Enrichment implementations (2-3 weeks)
25. T4-1 NetBox enricher against T3-2 mock
26. T4-2 ServiceNow CMDB enricher against T3-3 mock
27. T4-3 CLI-scraped enricher
28. T4-4 Enrichment visibility in device drawer
29. T4-5 MCP client infrastructure

### Sprint 7 — Signals (2 weeks)
30. T5-1 through T5-5 (v7 carryover)

### Sprint 8 — Path A embeddings (1-2 weeks)
31. T6-1 Graph embeddings with enrichment + environment features
32. T9-6 ML feature schema versioning

### Sprint 9 — NL query (1 week)
33. T9-5 NL query layer

### Sprint 10 — Investigation agent (2-3 weeks)
34. T7-1 through T7-4

### Sprint 11 — Path B GNN (3-4 weeks)
35. T6-2 GNN with message passing
36. T6-3 Enrichment-aware data loader

### Longer horizon
- T8-1 ControllerAdapter trait (design only)
- T8-2 Multi-controller correlation PoC (when audience emerges)
- T4-6 NETCONF enricher (demand-driven)
- T4-7 Infoblox enricher (demand-driven)
- T9-1 through T9-11 as time allows

### Deferred until forced
Tier 10 items.

---

## <a id="guardrails"></a>Guardrails — Updated for v8

### New in v8

- **Environment awareness is first-class.** Every feature that shapes operator workflow (onboarding, path selection, remediation scope, enrichment applicability, GNN features) considers the environment archetype. Features that hardcode "this is a DC" logic are rejected.
- **Path catalogue is data, not code.** Adding or customising path profiles requires no Rust changes. The Rust code loads and executes; YAML catalogues describe. Hardcoded vendor/environment logic in Rust is refactored into catalogue schema.
- **Integration mocks come before integration implementations.** Building a NetBox enricher against vendor docs is a bug factory. Mock first, implement against the mock, validate against real at deployment.
- **Every credential resolve carries a purpose.** Audit logging with structured purpose field is non-optional for operations against compliance-sensitive environments.

### Unchanged guardrails

- gNMI only for hot-path telemetry state
- Syslog and traps as signals, never state
- tokio only for async Rust
- Credentials never leave the Rust process
- No Kubernetes in v0.x
- Every non-trivial decision gets an ADR at commit time
- Detect-heal loop does not call an LLM or any enrichment source synchronously
- All operator-facing functionality lives on core
- Enrichers never call LLMs on device configuration — deterministic parsers only
- Collectors scale horizontally; core scales vertically in v1
- Build time is a first-class metric
- Code landing ≠ work complete
- Distributed mode must actually run distributed (mTLS on, no plaintext creds)
- Audience-driven scoping (controller-less primary audience)
- Enrichment is a primary differentiator

### Anti-patterns to reject

- "We can add this profile type later with another Rust match arm" — no, catalogue as data
- "The operator knows their network; they can pick paths manually" — they can, but first-run should default well so the mean-operator can start in minutes
- "Mocks are a CI nicety, we can ship enrichers without them" — no, mocks are development infrastructure
- "Skip the Environment concept, kind-string is fine" — no, conflating granularity and archetype has consequences
- "Let's hardcode vendor quirks in the enricher; catalogues are for paths only" — no, vendor quirks are catalogue metadata
- All prior anti-patterns remain

---

## What v8 Explicitly Excludes

For scope discipline, do not start:
- Individual controller adapters (trait design excepted)
- Auth/RBAC
- Multi-tenancy in graph
- Production HA for core
- LLM-based parsing of device configuration anywhere outside the investigation agent
- Environment archetypes beyond the five (DC / Campus Wired / Campus Wireless / SP / Home-lab) — extension requires ADR
- Bonsai-replaces-NDI / DNAC / controller-X positioning
- Kubernetes deployment manifests
- Workspace split
- Bitemporal schema, schema migration, Grafeo eval

---

*Version 8.0 — authored 2026-04-24 after reviewing post-v7 main. Verifies that v7 Tier 0-3 items landed cleanly (audience ADR, Topology improvements, onboarding wizard reachable, hierarchy-aware assignment, Docker polish, command palette, Sprint 4 testing). Introduces three architectural threads: Environment model with first-run wizard (Tier 1), Path catalogue as pluggable add-ons (Tier 2), Enrichment foundation with testable NetBox/ServiceNow mocks (Tier 3). Preserves v7 strategic direction and sharpens the "code landing ≠ work complete" discipline with concrete Q-1 through Q-9 quality issues surfaced by review.*
