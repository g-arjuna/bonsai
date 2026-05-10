# BONSAI — Backlog Charlie Series, v1 (CV1.0)

> **A series reset, not a continuation.** Authored 2026-05-09 after Bv6 execution paused mid-flight to consolidate two design conversations into a coherent forward plan.
>
> **Why "Charlie"** — the Bravo series shipped the core architecture (graph queries, write coordinator, signals tier groundwork). What the design conversations produced is a **pivot in how bonsai thinks about ingestion**. That pivot is structural enough to deserve a fresh series rather than another point release on Bravo.
>
> **The pivot, stated cleanly**: bonsai is no longer "gNMI-first hot path with polling fallback." It is **a streaming-where-possible engine with discovery-driven layered ingestion that determines per-device what's possible and surfaces gaps to operators**. Three layers — streaming (gNMI Subscribe), pull-on-demand (gNMI Get and CLI), and out-of-band (controllers, IPAM/CMDB, future SuzieQ-pattern poller). Each layer's data carries provenance. The path catalogue stops being a config-time decision and becomes a runtime-informed subscription strategy fed by enrichment.
>
> **Why this matters in real-world terms**: enterprise deployments have devices in mixed states of gNMI readiness. Some devices have working gNMI, some have certs missing, some have firewalls blocking, some have software versions with known gNMI bugs, some are vendors that don't speak gNMI at all. Today bonsai gives up on devices where gNMI doesn't work. Post-pivot, bonsai onboards every device via CLI first, captures what data is achievable from each layer, and tells operators what blockers stand between them and full streaming coverage. **That's the difference between a research artifact and a deployable tool.**
>
> **What carries forward unchanged**: the streaming-first hot path remains the goal where achievable. The graph-native engine, write coordinator, signals tier (syslog + SNMP), AIOps integration plans, GNN training trigger condition all remain as designed. The pivot adds a layer; it doesn't replace one.
>
> **What's explicitly absorbed from Bv6** (which landed substantially before halt): syslog daemon (579 lines), SNMP daemon (886 lines), syslog rules (5), SNMP rules (4), BFD chaos coverage, operational health thresholds doc, chaos auto-restart. These items are complete and continue without modification.

---

## Table of Contents

1. [Audience and Positioning — Sharpened](#positioning)
2. [Bv6 Sprint Outcome — What Landed Before Halt](#progress)
3. [The Pivot, In Detail](#pivot)
4. [Lab and Cloud Run — Refactoring Assessment](#labcloud)
5. [TIER 1 — Engineering Hygiene Carried From Bv6](#tier-1)
6. [TIER 2 — Multi-Source Enrichment + Layered Ingestion](#tier-2) ⚡ THE NEW WORK ⚡
7. [TIER 3 — Path Relevance Synthesizer](#tier-3) ⚡ THE NEW WORK ⚡
8. [TIER 4 — YANG Library Lifecycle (online + offline + restricted)](#tier-4)
9. [TIER 5 — Output Adapter Validation (carried from Bv6 Tier 3)](#tier-5)
10. [TIER 6 — ServiceNow AIOps Integration (carried from Bv6 Tier 4)](#tier-6)
11. [TIER 7 — GNN Training (parallel)](#tier-7)
12. [TIER 8 — Documentation Updates](#tier-8)
13. [Carryover from Bv6](#carryover)
14. [Execution Order](#execution-order)
15. [Guardrails](#guardrails)

---

## <a id="positioning"></a>Audience and Positioning — Sharpened

**Updated to reflect the pivot**:

> **An open-source graph-native AIOps feeder for controller-less network environments. Bonsai onboards multi-vendor devices via CLI-first discovery, identifies achievable data sources per device (streaming gNMI, pull-on-demand gNMI Get, CLI parsing, out-of-band APIs), recommends relevant subscriptions based on observed configuration, and surfaces blockers between current state and full streaming coverage. It correlates multi-layer signal (gNMI state + syslog + SNMP traps + config drift + topology) into impact-aware incidents, ranks them by graph-derived blast radius, and pushes them to AIOps platforms. A GNN trained on accumulated chaos archive provides anomaly scores that catch what rule-based detectors miss.**

The pivot adds two phrases: **"CLI-first discovery"** and **"surfaces blockers between current state and full streaming coverage."** These are not marketing additions; they describe a real shift in how bonsai approaches devices.

**Anti-positioning unchanged**: not a replacement for ServiceNow ITOM, Splunk ITSI, Datadog NPM, or Cisco Catalyst Center. Bonsai feeds these. Bonsai is also not a config management system; it detects drift, it does not enforce.

---

## <a id="progress"></a>Bv6 Sprint Outcome — What Landed Before Halt

End-to-end code review of v21 confirms substantial Bv6 work landed before the halt. Carry-forward items below.

| Bv6 item | Status | Evidence |
|---|---|---|
| T1-1 chaos runner auto-start + auto-restart | ✅ Done | `scripts/chaos_runner.sh:92-114` `--ensure-running` flag added; daemon restarts on stale PID |
| T1-2 BFD chaos coverage | ✅ Done | `chaos_plans/always_on_dc.yaml` now has 6 BFD faults + 6 each of bgp/interface/netem (24 total, was 18) |
| T1-3 leaf1 EVPN routes fix | ⚠️ Status unclear | Need lab health check to confirm |
| T1-4 operational thresholds doc | ✅ Done | `docs/operational_health_thresholds.md` with structured threshold matrices |
| T1-5 GNN data loader feature-space generalisation | ⚠️ Partial | `python/bonsai_ml/gnn/data_loader.py` modified but role expansion (PE/P/RR/CE) not visible — verify |
| T2-1 syslog ingestion daemon | ✅ Done | `src/signals/syslog.rs` (579 lines): UDP/TCP listeners, structured `SyslogEvent`, classification into Auth/Hardware/Software/Protocol/License/Custom, archive integration |
| T2-1 syslog rules | ✅ Done | `python/bonsai_sdk/rules/syslog.py` (154 lines): 5 rules with tests |
| T2-2 SNMP trap ingestion daemon | ✅ Done | `src/signals/snmp.rs` (886 lines): UDP listener, OID-aware parsing, well-known OIDs (cold/warm start, link up/down, auth failure) |
| T2-2 SNMP rules | ✅ Done | `python/bonsai_sdk/rules/snmp.py` (97 lines): 4 rules with tests |
| T2-1 + T2-2 daemons wired into main | ✅ Done | `src/main.rs:385-415` starts daemons when `cfg.signals.{syslog,snmp}.enabled` and collector role |

### Bv6 work not yet started (carries to CV1 as remaining tiers)

- T2-3 configuration drift detection
- T2-4 layer 2-3 gNMI rule expansion (LLDP mismatch, IS-IS flap, LACP, VRRP, LDP, RSVP-TE)
- T2-5 auto-correlation across signals
- T3-1..T3-4 output adapter e2e validation (Splunk, Elastic, ServiceNow EM)
- T4-1..T4-5 ServiceNow AIOps bidirectional integration
- T5-x GNN training (gates on archive depth)
- T6-x real-hardware-only schemas (power, optical, wireless, hardware FRU)

These carry forward unchanged in CV1 as Tiers 5-7.

---

## <a id="pivot"></a>The Pivot, In Detail

The conversation between previous backlog drafts surfaced a structural insight that doesn't fit the Bravo plan. Stated in three layers:

### Layer 1 — Streaming (gNMI Subscribe), unchanged

The hot path remains gNMI Subscribe with ON_CHANGE + SAMPLE modes. Sub-second latency. This is where detection rules, write coordinator, and graph state live. **No change to the streaming engine itself.**

What changes: streaming is no longer assumed-available. Bonsai now distinguishes "this device is streaming-ready" from "this device has streaming blockers" from "this device cannot stream."

### Layer 2 — Pull-on-demand (gNMI Get and CLI), new architectural surface

Triggered by:
- **Real-time change signals**: syslog patterns matching `Configured by user X`, `commit complete`, `running-config saved`; gNMI ON_CHANGE on config-version paths; SNMP `ciscoConfigManMIB` notifications. When any fires, queue a re-parse for that device within minutes.
- **Scheduled differential check**: weekly (operator-configurable interval) `gnmi_get` of full config or CLI `show running-config`. Compare to local guarded store. If different, parse and update; if identical, just refresh "last verified unchanged."
- **Operator-triggered**: explicit re-parse button in DeviceDrawer; post-remediation verification; investigation forced-refresh.

This layer captures what gNMI Subscribe doesn't: configuration text, MAC tables, ARP entries, vendor-specific operational state, environmental snapshots that change rarely.

**Multi-source parser pattern**: when CLI parsing is needed, run a priority chain. Default ordering by `(vendor, command_pattern)`:

```toml
[enrichment.parsers."cisco-iosxr"."show bgp neighbors"]
priority = ["pyats_genie", "ntc_templates", "bonsai_native"]

[enrichment.parsers."arista-eos"."show ip bgp summary"]
priority = ["suzieq_native", "pyats_genie", "ntc_templates"]

[enrichment.parsers."srlinux"."info from running interface"]
priority = ["bonsai_native"]
```

First parser to succeed wins. In dev mode, additional parsers run as second opinions for consensus checking. Production runs single-parser per command. The graph stores `ParsedCommandResult` with provenance — primary parser, agreement state with secondary parsers, captured timestamp. Disagreement between parsers is itself signal.

**Parser-as-sidecar architecture**: pyATS Genie has heavy dependencies (~200 MB Python tree, Cisco-specific lifecycle). Running it embedded in bonsai-core surprises operators. The pattern is sidecar microservice — each parser ecosystem runs as a separate container, called by bonsai over a small HTTP API. Operators choose which sidecars to deploy. The default deployment ships only `bonsai_native` (regex-based, no external dependencies); operators add `pyats-sidecar`, `suzieq-sidecar`, `ntc-templates-sidecar` per their environment.

### Layer 3 — Out-of-band (REST APIs from controllers, IPAM/CMDB, future SuzieQ-pattern), unchanged

NetBox enricher, ServiceNow CMDB enricher continue as today. Future additions live here: Cisco DNAC reader, Meraki API reader, vManage, Mist. The `Enricher` trait stays generic; new enrichers slot in without architectural change.

A SuzieQ-pattern enricher is a Layer 3 future addition: hourly poll of multi-vendor devices for non-streaming state, normalize to bonsai schema, emit graph properties. Treated as one of many Layer 3 sources, not a primary data path.

### What this enables — operator onboarding flow

**Pre-pivot**: operator adds device with credentials → bonsai connects via gNMI → if gNMI works, capabilities discovery → path catalogue picks profile → subscriptions start. If gNMI doesn't work, onboarding fails or limps.

**Post-pivot**: operator adds device with credentials → bonsai opens SSH session (universal management plane, almost always works) → runs structured discovery sequence (`show version`, `show running-config`, `show ip interface brief`, vendor-equivalent) → multi-source parser extracts hardware model, software version, configured features, certificate state, gNMI configuration state, firewall rules → produces TWO outputs:

1. **gNMI Readiness Report**: gNMI service status, TLS cert validity, required encoding support, known firmware bugs that affect gNMI. Per-device, surfaced in DeviceDrawer.

2. **Path Relevance Synthesis**: based on observed configuration plus capabilities (when available), recommend specific gNMI paths that matter for *this* device in *this* environment. Operator approves or rejects.

If gNMI is ready: subscriptions start with the synthesized path set.
If gNMI is not ready: bonsai reports specific blockers and continues with CLI-derived enrichment until gNMI prerequisites are sorted.

### Provenance throughout

Every graph property carries provenance: source (gnmi_subscribe / gnmi_get / cli / netbox / servicenow / synthesized), captured_at_ns, parser (when applicable), confidence (high for streaming, medium for periodic poll, lower for synthesized). Detection rules and the GNN can filter on confidence — high-confidence streaming data drives detection; lower-confidence data informs context.

### What is NOT changing

- The streaming-first hot path. Where gNMI is achievable, it's still primary.
- The graph schema. Properties carry provenance as new fields; no schema rewrite.
- The write coordinator and bus architecture. New data sources publish to the same bus.
- The detection rule engine. New rules can use new signal sources; existing rules unchanged.
- The path catalogue plugin loader. Profiles still ship; the synthesizer recommends them.

---

## <a id="labcloud"></a>Lab and Cloud Run — Refactoring Assessment

You asked for a comment on whether the lab runs on laptop and cloud need refactoring. Honest assessment:

### Current state

**Laptop lab** (`lab/dc/dc-evpn-srv6.clab.yml`): 8-node EVPN-SRv6 fabric. Stable. Bv5 daily check from 2026-05-08 confirmed `8/8 nodes up, 7/7 BGP sessions established, IS-IS adjacencies present`. One outstanding warning: `DC leaf1: no EVPN routes in mac-vrf-a` — Bv6 T1-3 was supposed to fix this; status uncertain.

**Cloud lab** (`lab/cloud-dc-6node.yml`): 6-node DC fabric scaled for Oracle Always Free 24 GB ARM. Per operator confirmation, OCI cloud spike running and accumulating archive.

**Chaos plan**: 24 fault entries (6 each of bgp_session_down, bfd_session_down, interface_shut, netem_loss). Solid breadth for the gNMI rule set.

### Refactoring needed — yes, three specific items

**1. Lab metadata for the synthesizer**: today the lab YAMLs declare nodes but not the role/environment metadata that the synthesizer will need. Need to add per-node `role: spine|leaf|super-spine|pe|p|rr|ce|access|distribution|core` and per-topology `environment: data_center|service_provider|campus_wired`. This is a 30-minute change but affects how the synthesizer learns paths from lab observations.

**2. Lab variant for SP coverage**: `lab/sp/sp-mpls-srte.clab.yml` exists but has not been brought up. Bv5 deferred this until DC archive matures (still correct sequencing). When SP comes up, the multi-source enrichment work in Tier 2 needs SP-specific test fixtures so the synthesizer can be validated against PE/P/RR roles.

**3. Cloud lab: archive-only, no GNN training**: the cloud OCI ARM has 24 GB RAM and 4 OCPU. Sufficient for lab + bonsai-core + chaos + archive. **Insufficient for GNN training** (which we'd want on a GPU box anyway). Document this clearly: cloud accumulates archive; archive syncs to GitHub; GNN training happens elsewhere (operator's workstation, on-prem GPU, or a one-off rented GPU instance for the training run).

### What does NOT need refactoring

- Lab topology itself: 8-node DC is fine. No need to grow it just yet.
- ContainerLab definition pattern: solid, multi-vendor support is real.
- Chaos runner architecture: Bv6 T1-1 fixed the auto-restart issue; running cleanly is now operational discipline.
- Cloud spike infrastructure: per operator confirmation, working as designed.

### Recommendation

Land the three small refactors in CV1 Sprint 1 alongside the engineering hygiene from Bv6 T1-3 and T1-5. They unblock the synthesizer work in Tier 3.

---

## <a id="tier-1"></a>TIER 1 — Engineering Hygiene Carried From Bv6

These items were Bv6 Tier 1 and didn't all land. Carry into CV1 as Sprint 1.

### T1-1 (CV1) — Verify Bv6 T1-3 leaf1 EVPN routes resolved

Run `scripts/check_lab.sh dc` and confirm clean output. If still warning, investigate leaf1 mac-vrf-a configuration.

**Done when**: lab health check returns zero warnings.

### T1-2 (CV1) — Complete Bv6 T1-5 GNN data loader feature-space generalisation

Verify `python/bonsai_ml/gnn/data_loader.py` actually has the SP role expansion (PE/P/RR/CE) and campus expansion (access/distribution/core/edge). If only DC roles, complete the feature-space.

**Done when**: feature space supports DC + SP + campus archetypes; SP-specific test fixture exercises PE/P roles.

### T1-3 (CV1) — Lab metadata enrichment for synthesizer training

Add per-node `role` and per-topology `environment` to:
- `lab/dc/dc-evpn-srv6.clab.yml`
- `lab/sp/sp-mpls-srte.clab.yml`
- `lab/cloud-dc-6node.yml`
- `lab/fast-iteration/*.yml` (verify exists)

Document the convention in `docs/lab_metadata.md` so future lab authors follow it.

**Done when**: every lab YAML declares role and environment metadata; synthesizer test fixtures consume this metadata.

### T1-4 (CV1) — Cloud GNN training boundary documented

`docs/cloud_lab.md` (new) documents what cloud-spike does and doesn't support: archive accumulation yes; GNN training no. Includes the "where to train" guidance — operator workstation with GPU, on-prem CUDA box, or rented GPU instance.

**Done when**: doc exists; cloud spike report template references it.

---

## <a id="tier-2"></a>TIER 2 — Multi-Source Enrichment + Layered Ingestion ⚡ THE NEW WORK ⚡

This tier implements Layer 2 (pull-on-demand) and the multi-source parser pattern. Substantial. Sequenced as a single coherent sprint because the components are tightly coupled.

### T2-1 (CV1) — `MultiSourceEnricher` trait + scaffolding

**What**: a new enricher type alongside `NetBoxEnricher` and `ServiceNowEnricher`. Subclasses run different commands or queries and emit normalized graph properties. Common to all subclasses:

- Triggered by change-detection pipeline (real-time signal, scheduled diff, operator-triggered)
- Reads credentials from vault with `purpose=MultiSourceEnrichment` audit tag
- Writes to graph via `WriteRequest::Enrichment` through write coordinator
- Emits provenance fields with every property

**Where**: `src/enrichment/multi_source.rs` plus `src/enrichment/mod.rs` extension.

**Done when**: trait surface defined; baseline subclass that calls `gnmi_get` for a known-good set of paths runs in tests.

### T2-2 (CV1) — Local guarded config store

**What**: encrypted-at-rest store for last-known config snapshots and diff history. Reuses credentials vault encryption pattern.

Schema:
- Per-device snapshot (encrypted blob)
- Hash of snapshot for fast equality check
- Timestamp of last verified state
- Provenance (which trigger captured this)
- Diff history retention (30 days default, configurable)

Diff representation: full snapshot at known-good baseline, structured diffs against baseline, periodic re-baselining (operator-triggered or after N diffs).

**Where**: `src/config_store.rs`, schema in `src/graph/mod.rs`, new graph node `ConfigSnapshot` and `ConfigChange`.

**Done when**: config store reads/writes encrypted; diff between snapshots produces structured `ConfigChange` graph nodes; operator UI shows diff history per device.

### T2-3 (CV1) — Change detection pipeline (three triggers)

**What**: when any of these fires for a device, queue a re-parse:

**Real-time signals**:
- Syslog patterns: vendor-specific config-changed patterns. Pattern library lives in `config/syslog_patterns/<vendor>.yaml` (one file per vendor, since the patterns are vendor-specific knowledge that mirrors path profiles)
- gNMI ON_CHANGE on config-version paths (e.g., `/system/configuration/state/last-changed`)
- SNMP `ciscoConfigManMIB`, Junos `configuration-database-change` traps

**Scheduled differential**:
- Weekly default (operator-configurable per device or globally)
- `gnmi_get` of full config OR `cli show running-config`
- Compare hash to local store; if different, full parse + update store
- Either way, refresh "last verified" timestamp

**Operator-triggered**:
- "Re-parse this device now" button in DeviceDrawer
- Post-remediation verification (automatic after playbook execution)
- Investigation forced-refresh

**Where**:
- `src/enrichment/change_detection.rs` (new)
- `config/syslog_patterns/` (new directory; vendor-specific patterns)
- DeviceDrawer button in UI

**Done when**: a manual config change on a lab device produces a re-parse within 2 minutes (via syslog pattern); weekly scheduled check runs; operator button triggers immediate re-parse.

### T2-4 (CV1) — Multi-source parser with priority chain

**What**: when CLI parsing is needed, consult priority chain per `(vendor, command_pattern)`. First success wins; consensus mode (dev only) runs all and compares.

**Default priority library** (ships with bonsai):
- Cisco IOS-XR: pyats_genie → ntc_templates → bonsai_native
- Cisco IOS classic: pyats_genie → ntc_templates → bonsai_native
- Cisco NX-OS: pyats_genie → suzieq_native → ntc_templates
- Arista EOS: suzieq_native → pyats_genie → ntc_templates
- Juniper Junos: pyats_genie → ntc_templates → bonsai_native (Junos has structured XML, less parsing needed)
- Nokia SR Linux: bonsai_native (SR Linux speaks gNMI well; CLI rarely needed)
- Cumulus / SONiC: suzieq_native → ntc_templates → bonsai_native
- FRR: bonsai_native (vtysh output is structured-ish)
- Palo Alto: bonsai_native (uses XML API)
- F5: bonsai_native (iControl REST)

**Where**:
- `src/enrichment/parser_chain.rs` (new)
- Parser HTTP client trait for sidecar communication
- `bonsai.toml.example` adds `[enrichment.parsers]` section with defaults

**Done when**: a parsed BGP-summary command from a Cisco IOS-XR device returns identical data through pyATS chain and through native fallback; consensus mode flags disagreements.

### T2-5 (CV1) — Parser sidecar reference implementations

**What**: ship two sidecar containers as reference, with operators free to add more:

- `pyats-sidecar`: Python container with pyATS+Genie, exposes `/parse` HTTP endpoint, takes (vendor, command, raw_output) and returns parsed JSON
- `bonsai-native-parser`: Rust binary with regex+textfsm parsers for the long tail; ships in default deployment

Sidecars are optional `docker compose --profile parsers up -d`. Without them, parser chain falls back to bonsai-native.

**Where**:
- `docker/sidecars/pyats/` (new)
- `docker/sidecars/bonsai-native-parser/` (or built into core, depending on size)
- Compose profile `parsers`

**Done when**: pyATS sidecar parses Cisco IOS-XR output via HTTP call from bonsai-core; without sidecar, fallback path works; CI exercises both modes.

### T2-6 (CV1) — Provenance fields throughout the graph

**What**: every graph property gets `source`, `captured_at_ns`, `confidence` fields. Schema migration:

- `Device`, `Interface`, `BgpNeighbor`, etc. nodes — properties tagged with provenance
- New `PropertyProvenance` graph node referenced by property updates (alternative to inline fields if schema migration is too disruptive)
- Write helpers carry provenance from source to graph

**Where**: schema migration in `src/graph/mod.rs`; write helpers updated; Explorer UI surfaces provenance on hover.

**Done when**: a Cypher query can filter by source (`MATCH (d:Device)-[:HAS_INTERFACE]->(i:Interface) WHERE i.admin_status_source = 'gnmi_subscribe' RETURN i`); UI shows provenance badge per property.

### T2-7 (CV1) — gNMI Readiness Report

**What**: per-device structured report capturing:

- gNMI service status (running/stopped/not-configured)
- TLS certificate validity (valid/expired/self-signed/missing/expires_at)
- Required encoding support (proto/json/json_ietf availability)
- Known firmware bugs affecting gNMI on this software version (lookup against shipped bug database)
- Specific blockers ("port 57400 unreachable from collector", "cert CN mismatch", "OpenConfig YANG version <X> on device, ≥Y required for path Z")
- Recommended actions ("upgrade to firmware Y.Z.W", "regenerate certificate with SAN matching IP", "open firewall rule for port 57400 from <collector_ip>")

**Where**:
- `src/discovery.rs` extended to compute readiness during onboarding
- Graph node `GnmiReadiness` per Device
- DeviceDrawer surfaces the report
- `/api/devices/<address>/gnmi-readiness` endpoint

**Done when**: onboarding a device with broken cert produces a clear blocker report; DeviceDrawer shows the report; operator can fix the blocker and re-trigger discovery.

---

## <a id="tier-3"></a>TIER 3 — Path Relevance Synthesizer ⚡ THE NEW WORK ⚡

This tier turns the enrichment from Tier 2 into actionable subscription recommendations. Per your request, ship a starter library covering DC + SP + campus archetypes.

### T3-1 (CV1) — Synthesizer engine

**What**: takes enrichment + capabilities + role + environment as input, produces recommended subscription set with rationale.

**Inputs**:
- gNMI Capabilities (paths the device offers)
- gNMI Readiness (which paths actually work; bug database lookups)
- CLI-derived feature inventory (configured protocols, configured features)
- Hardware model and software version
- Operator role hint (if provided)
- Environment archetype

**Logic**: rule library maps `(feature, environment, role) → [recommended_paths]`. Rules consult the path catalogue to find concrete paths matching abstract recommendations.

**Output**:
- Recommended subscription list with confidence per path
- Rationale per path ("subscribed bgp-rib because BGP configured AND path advertised AND no firmware bug")
- Blockers ("would subscribe ldp-sessions but firmware 7.5.1 has known bug — recommend 7.5.2")
- Gaps ("BFD state can only be obtained via CLI for this device — falling back to 5-minute poll")

**Where**:
- `src/synthesizer/mod.rs` (new module)
- Rule library at `config/synthesizer_rules/` (separate from path profiles — synthesizer rules are higher-level than path profiles)

**Done when**: a synthesized recommendation for a known DC EVPN leaf produces 12-15 paths with rationale; operator can approve or override.

### T3-2 (CV1) — Starter rule library

**What**: ship rules covering:

- **DC**: spine, super-spine, leaf, border (EVPN-VXLAN, IP underlay, fabric VLANs)
- **SP**: PE (L3VPN, EVPN, L2VPN, BGP-LU), P (LDP, RSVP-TE, SR-MPLS), RR (BGP route-reflection), CE (peer to PE), peering edge
- **Campus**: core, distribution, access, WLC, edge
- **Vendor variants**: per major vendor where rules differ (e.g., Cisco IOS-XR L3VPN paths differ from Junos L3VPN paths)

Each rule is small and named:
```yaml
# config/synthesizer_rules/dc_evpn_leaf.yaml
name: dc_evpn_leaf
description: EVPN-VXLAN leaf in DC fabric
preconditions:
  role_hint: leaf
  environment: data_center
  features: [bgp, evpn, vxlan]  # at least one must be configured
recommended_paths:
  - id: bgp_neighbor_state
    catalogue_ref: dc_evpn_leaf.yaml#bgp_neighbor_state
    confidence: high
    rationale: BGP-EVPN required for fabric forwarding
  - id: evpn_route_state
    catalogue_ref: dc_evpn_leaf.yaml#evpn_route_state
    confidence: high
    rationale: EVPN routes carry MAC-IP bindings for fabric
  # ... etc
```

**Where**: `config/synthesizer_rules/` (new directory) — shipped with bonsai; operators can override.

**Done when**: starter library covers DC + SP + campus archetypes for the major vendor families; rules unit-tested against synthetic device profiles.

### T3-3 (CV1) — Synthesizer recommendations UI

**What**: when a device is onboarded or re-discovered, the recommendations appear in a "Recommended subscriptions" panel:

- Recommended path list with rationale per path
- Operator can approve all, approve some, override (add/remove specific paths)
- Approved set becomes the device's subscription
- Future re-discovery flags drift from operator-approved set ("new feature configured since last review — additional path recommended?")

**Where**: extension to DeviceDrawer in UI, new `/api/devices/<address>/recommendations` endpoint.

**Done when**: operator can review, approve, override recommendations from UI; approved subscriptions persist in graph.

### T3-4 (CV1) — Operator override library

**What**: operators inevitably curate per-environment overrides. The override library is `config/synthesizer_overrides/<environment_name>.yaml` — operator-authored, gitignored from defaults but documented.

Override examples:
- "All devices in environment 'prod-dc' subscribe to interface-stats path even if synthesizer says no"
- "All Cisco IOS-XR devices skip evpn-route-state because it's expensive on our hardware"
- "Site 'london' devices add custom-vendor-mib path"

**Where**: `config/synthesizer_overrides/` (operator-managed); merge logic in `src/synthesizer/mod.rs`.

**Done when**: override library documented; merge precedence is `defaults < environment override < site override < device override`.

---

## <a id="tier-4"></a>TIER 4 — YANG Library Lifecycle (online + offline + restricted)

You raised this as deserving thought. Operators will deploy bonsai in three connectivity contexts. Each needs a clean YANG path library workflow.

### T4-1 (CV1) — Online sync from public YANG repos

**What**: a `bonsai yang-sync` CLI command that:

1. Fetches latest YANG modules from canonical sources:
   - `github.com/openconfig/public` (OpenConfig)
   - `github.com/YangModels/yang` (vendor + IETF tree)
   - Vendor-specific: `github.com/CiscoDevNet/yang`, `github.com/Juniper/yang`, `github.com/aristanetworks/yang`, `github.com/nokia/srlinux-yang-models`

2. Validates downloaded modules (pyang or libyang)
3. Indexes paths into a local catalogue at `runtime/yang_catalogue/`
4. Surfaces new paths discovered since last sync

**Where**: new CLI subcommand `bonsai yang-sync`; sync logic in `src/yang_sync.rs` or Python helper.

**Done when**: from a fresh bonsai install with internet, `bonsai yang-sync` populates the local catalogue with current OpenConfig + major vendor modules.

### T4-2 (CV1) — Manual upload of YANG modules

**What**: for environments where bonsai has internet but the operator wants curated YANG modules (specific vendor branches, internal extensions, vendor-NDA-only modules), provide a manual import path:

- `bonsai yang-import <directory>` validates and imports a directory of `.yang` files
- UI workspace `/yang-library` shows imported modules, source provenance, last-updated timestamp
- Operators can mark modules as "trusted" or "experimental"

**Where**: CLI subcommand; UI route `Yang.svelte`.

**Done when**: operator can import a vendor-supplied YANG bundle from a USB drive; modules visible in UI; paths usable in synthesizer.

### T4-3 (CV1) — Offline / restricted environment workflow

**What**: many enterprise deployments forbid outbound internet from production. Provide a two-machine workflow:

1. Operator's workstation (with internet): runs `bonsai yang-bundle <vendor> <version>` to produce a signed YANG bundle (tarball + manifest + checksum + signature).

2. Air-gapped bonsai installation: `bonsai yang-install <bundle>` validates signature, validates modules, imports.

Bundle includes provenance: source repo, git commit, fetch date, signing key.

**Where**: extend `yang-sync` and `yang-import` with bundle subcommands.

**Done when**: a YANG bundle produced on workstation can be transferred via removable media to an air-gapped bonsai instance; instance imports cleanly with cryptographic verification.

### T4-4 (CV1) — Synthesizer YANG awareness

**What**: synthesizer rules can reference YANG modules by name and version. When a device's gNMI Capabilities advertises module versions, the synthesizer knows which paths are available.

**Where**: synthesizer rule schema extended; `bonsai yang-search <feature>` CLI helper for operators discovering paths.

**Done when**: synthesizer correctly recommends paths only when device's advertised modules support them.

---

## <a id="tier-5"></a>TIER 5 — Output Adapter Validation (Bv6 Tier 3 carryover)

Unchanged from Bv6 Tier 3:

### T5-1 (CV1) — Splunk HEC adapter e2e test
### T5-2 (CV1) — Elastic adapter e2e test
### T5-3 (CV1) — ServiceNow EM adapter e2e test against PDI
### T5-4 (CV1) — Output adapter health monitoring

---

## <a id="tier-6"></a>TIER 6 — ServiceNow AIOps Integration (Bv6 Tier 4 carryover)

Unchanged from Bv6 Tier 4:

### T6-1 (CV1) — Bidirectional incident sync
### T6-2 (CV1) — Auto-correlation feeds ServiceNow
### T6-3 (CV1) — Auto-clearing of correlated tickets
### T6-4 (CV1) — Root-cause hint via graph blast radius
### T6-5 (CV1) — ServiceNow ITSM playbook bridge

---

## <a id="tier-7"></a>TIER 7 — GNN Training (parallel)

Unchanged from Bv6 Tier 5 / Bv5 Tier 3. Trigger condition unchanged. Now benefits from richer multi-signal data (syslog + SNMP + config drift + multi-source enrichment in addition to gNMI).

### T7-1 (CV1) — GNN training when archive depth allows
### T7-2 (CV1) — Comparison study: rules vs tabular ML vs GNN
### T7-3 (CV1) — Online inference path
### T7-4 (CV1) — Model card with multi-signal coverage documentation

---

## <a id="tier-8"></a>TIER 8 — Documentation Updates

The pivot needs the documentation surface refreshed. **Per operator's standing instruction**, this is lowest priority — but it does need to land before next public communication or release.

### T8-1 (CV1) — DECISIONS.md update

Add structured ADR entries:
- "Discovery-driven layered ingestion" — three layers, provenance, when each layer is appropriate
- "Multi-source parser priority chain" — sidecar pattern, default priority library
- "gNMI Readiness Report" — what it captures, why it's first-class
- "Path Relevance Synthesizer" — replaces static catalogue selection
- "YANG library lifecycle" — online/manual/offline-restricted workflows
- "Lab metadata convention" — role + environment in every lab YAML

### T8-2 (CV1) — README.md update

The README's onboarding story needs the new flow:

> Bonsai onboards via SSH (universal management plane), discovers what's possible, recommends streaming subscriptions where supported, surfaces blockers where not. CLI-derived enrichment runs continuously alongside streaming where streaming is available.

Quick start updated to mention `bonsai yang-sync` as a step. Architecture diagram updated to show three ingestion layers explicitly.

### T8-3 (CV1) — CLAUDE.md / AGENTS.md update

AI-agent consumption guidance updated for the new layered ingestion. New endpoints (`/api/devices/<address>/gnmi-readiness`, `/api/devices/<address>/recommendations`) documented for AI session use.

### T8-4 (CV1) — Architecture note for the pivot

`docs/architecture_layered_ingestion.md` (new) — single page explaining the three layers, when each is used, provenance fields, examples per common deployment scenario (greenfield gNMI-ready DC; mixed brownfield with classic IOS; air-gapped restricted environment).

### T8-5 (CV1) — Operator deployment guide update

`docs/deployment_guide.md` updated to include the new connectivity-context decision tree:
- Has internet, devices speak gNMI well → Layer 1 primary, minimal Layer 2/3
- Has internet, mixed device fleet → all three layers, parser sidecars deployed
- Air-gapped restricted → Layer 2/3 primary (CLI + scheduled poll), YANG bundles imported manually, Layer 1 where possible

---

## <a id="carryover"></a>Carryover from Bv6

Items remaining valid; deferred behind Tier 1-7:

- **Bv6 T2-3 configuration drift detection** → subsumed by CV1 Tier 2 (same work, integrated with multi-source enricher)
- **Bv6 T2-4 layer 2-3 gNMI rule expansion** → subsumed by CV1 Tier 3 (synthesizer recommends new rules where features exist)
- **Bv6 T2-5 auto-correlation across signals** → still needed; defer to Bv7 or post-MVP
- **Bv6 Tier 6 real-hardware-only schemas** (power, optical, wireless, FRU) → defer to post-northstar
- **Investigation agent productive use** (post-MVP, pending token budget)
- **HIL graduated remediation** in production
- **Operator path overrides UI workspace, subscription resolution audit** → subsumed/related to CV1 Tier 3 synthesizer UI
- **Catalogue plugin install command, AIOps readiness checklist, NL query, bulk CSV onboarding, scale architecture, S3 archive backend, campus topology lab, bitemporal schema, schema migration, Grafeo evaluation** → strategic carryover

Plus the Bv2 hardcoding catalogue (H-1 through H-12) — most addressed; remainder opportunistic.

---

## <a id="execution-order"></a>Execution Order

CV1 is meaningfully larger than Bv6 in scope. Sequencing is critical.

### Sprint 1 (1-2 weeks) — Hygiene + lab metadata
1. T1-1 verify leaf1 EVPN routes
2. T1-2 GNN data loader feature-space generalisation
3. T1-3 lab metadata for synthesizer
4. T1-4 cloud GNN boundary documented

### Sprint 2 (4-5 weeks) — Multi-source enrichment + layered ingestion ⚡
5. T2-1 MultiSourceEnricher trait
6. T2-2 local guarded config store
7. T2-3 change detection pipeline
8. T2-4 multi-source parser with priority chain
9. T2-5 parser sidecars
10. T2-6 provenance fields
11. T2-7 gNMI Readiness Report

### Sprint 3 (3-4 weeks) — Path Relevance Synthesizer ⚡
12. T3-1 synthesizer engine
13. T3-2 starter rule library (DC + SP + campus)
14. T3-3 synthesizer recommendations UI
15. T3-4 operator override library

### Sprint 4 (2 weeks) — YANG library lifecycle
16. T4-1 online sync
17. T4-2 manual upload
18. T4-3 offline / restricted workflow
19. T4-4 synthesizer YANG awareness

### Sprint 5 (1-2 weeks) — Output adapter validation
20. T5-1 Splunk e2e
21. T5-2 Elastic e2e
22. T5-3 ServiceNow EM e2e
23. T5-4 adapter health

### Sprint 6 (2-3 weeks) — ServiceNow AIOps integration
24. T6-1 through T6-5

### Sprint 7 (3-4 weeks, parallel to 4-6) — GNN training when archive ready
25. T7-1 through T7-4

### Sprint 8 (1-2 weeks) — Documentation updates
26. T8-1 through T8-5

### Continuously running through all sprints
- Chaos cycle on DC lab (laptop) — passive archive accumulation
- Cloud chaos cycle (OCI Always Free) — passive archive accumulation
- Daily verification cron

### Estimated total
**16-22 weeks** to a state where bonsai has:
- Layered ingestion proven across the three connectivity contexts (internet, hybrid, air-gapped)
- Multi-source enrichment with priority chain across major vendors
- Synthesizer-driven path recommendations with operator override
- YANG library lifecycle for all three deployment modes
- Validated output adapters (Prometheus + Splunk + Elastic + ServiceNow EM)
- Real bidirectional ServiceNow AIOps integration
- Trained Path B GNN on multi-signal archive
- Documentation refreshed to reflect the pivot

This is genuinely a deployable open-source GNN-driven AIOps feeder. **Different timeline than Bv6 (which was 12-16 weeks) because the pivot adds material new work**: multi-source enrichment + synthesizer + YANG lifecycle. Each is deferrable in principle but each is what makes the difference between research-quality and deployment-quality.

---

## <a id="guardrails"></a>Guardrails

### New in CV1

- **Discovery is layered, not single-protocol.** No new code path assumes gNMI-only. Every onboarding code path considers the layered ingestion model.
- **Provenance is non-negotiable.** Every graph property carries source + confidence. Detection rules and the GNN can filter on these.
- **Synthesizer recommends; operator approves.** Never auto-subscribe based on synthesizer alone. Operator approval gate stays.
- **YANG library has three deployment modes.** Online, manual, air-gapped. None is privileged; all three are first-class.
- **Parser sidecars are optional, not required.** Default deployment (without sidecars) must work; sidecars enhance.
- **Multi-source parser priority is configuration, not code.** Operators tune per-environment without recompiling bonsai.
- **gNMI Readiness Report is first-class operational output.** Surface in DeviceDrawer; not buried in logs.

### Unchanged from v7-Bv6

All prior architectural invariants. Reference earlier backlogs.

### Anti-patterns to reject

- "Trust the synthesizer's recommendations and auto-subscribe" — no, operator approval is the gate
- "Skip CLI-first discovery for gNMI-ready devices" — no, run discovery anyway; the data is useful for change detection and config drift
- "Embed pyATS in bonsai-core" — no, sidecar; keeps deployment options open
- "Ship without provenance fields and add later" — no, schema is hard to retrofit
- "YANG sync is online-only because that's simpler" — no, air-gapped enterprises are real users
- "Synthesizer rules in code" — no, YAML configuration

---

## What CV1 Explicitly Excludes

- New marketing positioning beyond "graph-native AIOps feeder"
- Auth/RBAC, multi-tenancy, production HA, K8s
- Workspace split
- Auto-graduation of trust state
- Replacement positioning vs ServiceNow ITOM, etc.
- Wireless / hardware-FRU / optical chaos simulation
- Auto-execution of synthesizer recommendations
- Bidirectional integration with non-ServiceNow AIOps platforms (build pattern reusable but only ServiceNow lands in CV1)

---

*CV1.0 — authored 2026-05-09 after pause of Bv6 mid-execution. Captures the structural pivot from "gNMI-first hot path with polling fallback" to "discovery-driven layered ingestion with streaming-where-possible." Three ingestion layers (streaming, pull-on-demand, out-of-band) each carrying provenance. Multi-source parser priority chain with sidecar pattern. Path Relevance Synthesizer producing operator-approvable recommendations from enrichment + capabilities. YANG library lifecycle for online + manual + air-gapped deployment modes. Lab metadata standardisation. Documentation surface refreshed to reflect the pivot. Carries forward Bv6 substantial work: signals tier (syslog + SNMP daemons + rules + chaos coverage), operational thresholds. Carries forward unstarted Bv6 work: output adapter validation, ServiceNow AIOps integration, GNN training. Estimated 16-22 weeks to a deployable open-source graph-native AIOps feeder. References v2-Bv6 for unchanged architectural decisions; positions CV1 as a series reset rather than point release because the ingestion shift is structural.*
