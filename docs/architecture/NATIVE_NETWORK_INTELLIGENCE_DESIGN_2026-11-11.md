# Native Network Intelligence: multivendor Rust normalization, configuration semantics, graph assurance and SONiC

**Design proposal / implementation contract — 11 November 2026. Not a shipped capability.**  
**Decision:** [ADR](ADR_NATIVE_NETWORK_INTELLIGENCE_2026-11-11.md) | **Execution:** [BN-024 in canonical backlog](../../BONSAI_BACKLOG.md) | **Source baseline:** Bonsai `main` `d5833fe82ac7a510599d6bc010d21b6ad6e9aef2`; pinned SuzieQ OSS `75198b5585ebef91f0659b6b1ec3c964cc2cd1ad`.  
**Review method:** static reading of selected Bonsai/SuzieQ sources and SONiC official design documents; **no Rust port, SuzieQ differential test, SONiC lab, runtime/LLM benchmark, UI or security test was run** in this documentation PR.

## 1. Product thesis and definition of success

Bonsai's existing strength is time-aware operational ingestion: gNMI, BMP/RIB history, event correlation, topology, a graph, optional inventory enrichment and a bounded LLM/operator loop. The missing next layer is an **executable semantic model of what each device is configured to do** and **why its observed state agrees or disagrees**, including before an incident and when a device is offline.

A successful real-world system:

- Starts with **zero external integrations** and accepts declared offline/unsupported assets; NetBox, Nautobot and CMDB are optional providers of *intended* facts.
- Uses **one product, one identity/authority model, one operator experience**, and in its standalone mode one Bonsai install. A Rust-native engine is the default; no SuzieQ deployment, database, polling service or credentials are required.
- Understands device-specific collection capability (not just vendor string), explicitly scoped CLI/NETCONF/gNMI/SNMP/controller/BMP data, quality and limits.
- Can answer “What is configured?”, “What is effectively configured?”, “What was observed?”, “Can this destination be reached in this VRF/as-of?”, “What changed?”, and “Which evidence is missing?” without inventing a successful path.
- Builds graph **from semantically typed, scoped facts**, not unvalidated text extraction, auto-merged IPs, or a shortest physical-link traversal. Preserves evidence and counterevidence.
- Produces deterministic, permission-filtered assessments **before** sending anything to an LLM; an LLM narrates findings but never supplies an authoritative edge or config-policy result.
- Publishes a precise support level per NOS image/version/transport/feature. “Designed for SONiC” is not “validated on SONiC hardware.”

### Non-goals for the initial release

Full config-language coverage across all NOS; a general symbolic packet solver; mandatory SuzieQ; wholesale Rust translation of Python/Pandas; physical ASIC assurance from DB/CLI alone; production config writes; scanning unknown networks by default; cloning NetBox or a new second graph/RIB store; speculative vendor support. These require separate gates rather than silently broadening “supported”.

## 2. Existing code seams and unclosed preconditions

| Existing Bonsai source | What it offers now | Gap this design addresses |
|---|---|---|
| `src/config_store.rs` + `src/change_detection.rs` | Encrypted raw snapshot storage; hash/text-diff and history | No complete parsed/effective config semantics; must not treat line diffs as policy deltas |
| `src/enrichment/multi_source.rs` / `registry.rs` | `MultiSourceEnricher`, gNMI Get, CLI-first fallback | Feature/capability-specific capture planning, quality/coverage and safe scheduling |
| `src/enrichment/parser_chain_enricher.rs` / `src/parser_chain.rs` | Python CLI helper, parser chain, optional Python Genie/TextFSM | `suzieq_native` currently reaches line-splitting helper, not SuzieQ; primary default chain begins with weak native line parse; additionally **Rust posts `command_pattern` where the PyATS sidecar requires `command` and sends `pyats_genie` where the sidecar accepts `genie`/`auto` (or `textfsm`)**, while `ntc_templates` targets the generic line-splitter. Repair and test these contracts before trusting baseline results |
| `docker/sidecars/pyats/app.py` / `python/bootstrap_agent.py` | Existing Genie/NTC parsing and bootstrap | Preserve until per-feature native behavior is proved, then staged opt-out/rollback |
| `src/telemetry_identity.rs` / BN-021 | Trust-scoped exporter and canonical device binding | CLI/config observations need equivalent canonical binding; do not identify by hostname/IP alone |
| `src/graph/queries.rs` / `src/http_server/observability.rs` | Connectivity shortest-path and blast radius | Not VRF/route/policy/ECMP/FIB forwarding trace; expose graph-only mode honestly |
| `src/enrichment/netbox.rs` | Optional external inventory enrichment | Field ownership, authoritative external IDs and conflict handling; never overwrite site hierarchy from device text |
| `docs/architecture/config_state_lane.md` | Config-change event design | Distinguish existing implemented snapshots from historical pending design text; semantic change and operational causation require new proof |

**Hard preconditions from the existing backlog:** BN-015-U1 (inventory-only NetBox import), BN-015-U2 (site metadata integrity) and BN-016-U1 (false BGP graph edges) must be fixed/tested before new normalized facts are allowed into authoritative topology. BN-021g/BN-008 device/source trust and BN-022 scoped reads remain gates. Do not reopen or undo verified BMP/RIB/BN-014 work.

## 3. Runtime architecture and boundaries

~~~text
read-only acquisition (inventory-bound, capability-gated)
  gNMI | NETCONF | SSH command allowlist | SNMP | SONiC gNMI | controller API
  existing BMP/RIB/BGP-LS/NetFlow/syslog streams --------------------------+
            |                                                            |
            v                                                            |
authenticated capture envelope --> encrypted raw/evidence store            |
            |                                                            |
            v                                                            |
Rust extraction + vendor normalizers (SuzieQ-referenced, versioned)        |
            |                                                            |
            +--> typed observed facts -----------------------------------+
            |
            +--> lossless config syntax tree -> reference/default/semantic resolver
                                                 |
                                      typed configured/effective facts
                                                 |
optional intended inventory/CMDB ---------------+
                                                 |
                                                 v
                          identity + scope + time + coverage reconciliation
                              |                  |                  |
                          graph projection  indexed RIB/FIB   typed assessments
                              |                  |                  |
                              +-------- path/policy proof ----------+
                                                 |
                               versioned curated API/MCP tools
                                                 |
                      one Bonsai UI and grounded operator/LLM workflow
~~~

**Proposed modules, not assertions of present files:** `src/network_intelligence/{types,capabilities,capture,extract,normalizers,config_ast,config_semantics,reconcile,path,assurance,api}.rs`, or an equivalent internal crate boundary after design review. Reuse existing `CredentialVault`, source-identity resolver, `ConfigStore`, graph write coordinator, event/journal lifecycle, API auth/scope filters and collector status machinery.

One standalone binary can host trusted Rust extractors. A hostile/unbounded parser, large external dependency or low-trust community plugin may require a **restricted subprocess** with bounded IPC. This still ships as one Bonsai product. The isolation/availability/security contract outranks a simplistic single-process goal. For fleet/HA use existing BN-023 design; do not force all routing or polling into one failure domain.

### 3.1 Source-of-truth and authority ledger

Every asserted fact is an **observation with authority constraints**, not a globally upsertable row:

| Plane | Examples | What it cannot prove |
|---|---|---|
| `intended` | NetBox device/site/cable/VRF/tenant, ServiceNow owner/CI, manual topology | That a port or BGP neighbor actually exists/up |
| `configured` | Running config AST statement, SONiC CONFIG_DB record | That inheritance was resolved, config applied, or traffic forwards |
| `effective` | Resolved BGP peer-group policy, VRF import RT, access policy (within tested dialect) | That runtime installed the intended state |
| `observed` | BMP Adj-RIB views, gNMI state, CLI RIB, SONiC APPL_DB/STATE_DB, LLDP | That all sources/AFIs were observed or ASIC forwarding is correct |
| `validated` | Independent FIB/LFIB/probe/controlled lab assertion | That a different time/VRF/packet class is valid |

The observed plane subdivides **control, operational and forwarding** facets with exact protocol, peer/view and producer scope. Model authority on each *property*, not a winner-take-all device blob. Allow multiple incompatible assertions, maintain supersession/tombstones, and issue evidence-based conflicts instead of last-write-wins.

### 3.2 Minimal fact/evidence contract (conceptual; schema to be versioned by BN-024-02)

~~~text
FactEnvelope {
  contract_version, fact_id, entity_key, facet, truth_plane,
  environment_id, tenant_id?, device_id?, network_instance_id?, asic_namespace?,
  source_id, collector_id?, transport_principal?, identity_status,
  source_kind, raw_evidence_id, input_digest, parser_id, parser_version, rule_id?,
  source_time?, captured_at, received_at, valid_from, valid_to?,
  collection_epoch?, cursor?, last_success_at?, coverage,
  semantic_state, value, interpretation, contradictory_fact_ids[],
  uncertainty_reasons[], completeness_state, authorization_scope
}
~~~

- `entity_key` is **scoped**: never bare IP, reported hostname or interface label as global authority; include network instance, VRF/table, AFI/SAFI and SONiC ASIC namespace when applicable.
- `valid_from/to` is when a value is believed valid, not just stored; `captured_at` is snapshot time, `received_at` ingestion time. Version parser/normalizer, mapping, source event and graph projection so replay can rebuild a known version.
- `coverage` identifies exactly which device/service/VRF/AFI/paths were attempted, returned complete/partial/failed/unsupported, last successful poll, pagination and rate limits. A successful empty query is distinguishable from no poll and a stale successful poll.
- `semantic_state` distinguishes `known / unsupported / not-collected / unknown / stale / conflicting / validated`. For false certainty control, evidence can be `provisional` or `ambiguous`; only trusted canonical binding permits authoritative writes.
- `interpretation` carries a rule type and bounded confidence/limitations, **not LLM-generated truth**. LLM text is commentary attached separately.

Keep raw encrypted configs out of the graph. Graph stores indexed entities and relations plus evidence references/digests; RIB/FIB, counter series and large snapshots remain in their appropriate stores. Provide **as-of snapshots and reproducible revision manifests**, not time-filtered graph edges joined to unconstrained latest RIB.

### 3.3 Pluggable NOS/capability architecture

Do **not** implement `if vendor == X` across every subsystem. A vendor extension is a **declarative profile plus scoped Rust feature modules**, selected by observed NOS family, OS version/build, platform SKU, role, image, transport and source capabilities.

Conceptual interfaces:

~~~rust
trait NosProfile {
    fn identity(&self) -> NosFingerprint;
    fn declared_capabilities(&self) -> Vec<CapabilityClaim>;
    fn safe_capture_plan(&self, observed: &DiscoveredCapabilities)
        -> Vec<ReadOnlyCapturePlan>;
    fn normalizers(&self) -> Vec<NormalizerDescriptor>;
    fn config_syntax(&self) -> Option<ConfigDialect>;
    fn semantic_features(&self) -> Vec<SemanticFeatureDescriptor>;
}
trait FeatureNormalizer {
    fn normalize(&self, raw: &CapturedEvidence, ctx: &ScopeContext)
        -> Result<NormalizedBatch, NormalizeError>;
}
trait ConfigResolver {
    fn resolve(&self, ast: &LosslessConfigAst, ctx: &ResolutionContext)
        -> ResolutionResult; // typed facts + explicit holes/unsupported constructs
}
~~~

These are **illustrative**, not code to paste verbatim. Capability status must distinguish `declared`, `advertised`, `probe_verified`, `parsed`, `semantically_resolved`, `projected` and `independently_validated`, with transport and evidence. No profile override across a different environment without explicit selection. Unknown versions are **untested** (never silently “all versions”).

Require an extension manifest with: family/subfamily, regex or exact supported versions, capability table, safe read-only capture recipes, command-output formats, schema/normalization versions, config dialect + references, explicit unsupported features, deterministic tests, credential/transport requirements, CPU/memory budget and maintenance owner. Versioned feature flags allow per-vendor/per-feature rollback.

## 4. Reuse SuzieQ OSS deliberately; do not preserve accidental constraints

**Pinned reference:** `netenglabs/suzieq@75198b5585ebef91f0659b6b1ec3c964cc2cd1ad`, Apache-2.0 repository license. Examine actual individual assets for third-party provenance and include correct copyright notices before copying. Do not ingest Enterprise/proprietary material. Require an SBOM, file-level decisions and third-party code review (BN-024-00).

| SuzieQ OSS reference | Real role | Native Bonsai plan | Acceptance / traps |
|---|---|---|---|
| `suzieq/config/{interfaces,bgp,routes,lldp,arpnd,macs,evpnVni,mlag,...}.yml` | NOS command/REST mapping and normalization expressions | Create a **restricted compiled extraction DSL** and versioned source catalog; preserve vendor semantics and expose unsupported fields | Differential raw input → typed output with negative fixtures; never execute arbitrary expression/template code |
| `config/schema/*.avsc` | Schema types/default/key fields | Adapt into Bonsai `FactEnvelope` + feature schemas with field provenance, no global hostname keys | Schema evolution, typed null/unknown, no fictitious zero values |
| `poller/worker/services/svcparser.py` | Hierarchical JSON-to-record extraction | Rust path traversal/extraction compiler; retain per-record parent keys (VRF, prefix, AFI/SAFI), nested nexthop/oif relationships | Test empty hierarchy, 1-to-many arrays, VRF overlap, platform-shaped JSON and resource limits |
| `poller/worker/services/{interfaces,bgp,routes,...}.py` | NOS-specific data cleanup, unit and state normalization | Selective Rust `FeatureNormalizer` per NOS/version; keep adapters small | Speed units, case, AFI/SAFI, VRF defaults, interface names, VTEP, ECMP adjacency, suppression of false defaults |
| `poller/worker/services/service.py` | Processing, merging, default/type coercion, poller lifecycle | Port **normalization behaviors** only; Bonsai owns scheduler, trust, write/checkpoint and store | Do not reproduce silent coercion/“default” records that invent facts; no new polling database |
| `engines/pandas/path.py` | L2/L3/VRF recursive hop discovery using tables | Build typed path resolver against Bonsai graph and routing stores | Per-hop as-of coverage, loop/ECMP/recursive limits, MPLS/overlay distinction, no physical BFS masquerading as forwarding |
| `engines/pandas/devconfig.py` | Raw config retrieval and section extraction (CiscoConfParse) | Reference for collection only; **new lossless AST + semantic resolver required** | Section extraction is **not** effective routing-policy interpretation |
| `tests/unit` and `tests/integration` | Published parser and network-state fixtures | Permitted, attributed golden corpus in fixture manifest; compare outputs with pinned Python in offline harness | Document coverage, fixture ownership/license, optional Python **test** runner only; production never requires SuzieQ |

**Parity methodology:** for each candidate field choose a baseline fixture, expected output and quality classification (same/corrected/intentional deviation/unsupported); pin upstream commit and command/capture version; compare repeated runs to ensure determinism and replay idempotency. Intentional deviations (e.g., fixing an inherited ambiguous default) need a written justification and an independent device oracle. A successful parse is *not* success when output is a line array or contains an invented default.

Avoid dynamic Python `eval` and arbitrary shell templates. Parse a finite safe expression grammar (field paths, keys, optional/default, simple typed operators) with compilation-time errors, execution budgets, untrusted text size limits and fuzz tests. Escapes, Unicode, pathological nested JSON and expression explosions must be covered. If a SuzieQ service definition requires unsupported dynamic logic, reimplement the exact validated behavior as explicit Rust code rather than implicitly evaluating code.

**Port candidates priority:** interface admin/oper + addresses → LLDP/LAG/VLAN/VRF → BGP peers (per AFI/SAFI) + IP routes → ARP/ND/MAC → EVPN VNI → OSPF/IS-IS/BFD assertions and config families. These are **candidates**, not coverage claims. SONiC uses native DB semantics in addition to SuzieQ's SONiC-normalized operational outputs.

## 5. Configuration interpretation: syntax is not semantics

### 5.1 Collection and secure storage

Choose only authorized, read-only capture per device capability: model-driven `Get`/NETCONF `get-config`/REST/approved SSH command. Preserve running vs startup/candidate provenance, software build and snapshot source. On SONiC distinguish live CONFIG_DB from saved `config_db.json`; it is not safe to treat these as identical. Snapshot with encryption, content hash, ingestion provenance and rotation/deletion/backup discipline. Sanitize secrets **before** graph/LLM/logging; raw encrypted storage remains privileged. The present `ConfigStore` is a starting point, not an implementation of all encryption/access/key lifecycle goals.

### 5.2 Lossless parsing and reference resolution

1. **Lossless concrete syntax representation:** preserve original bytes/normalized newlines, comments, source spans, ordered clauses, hierarchy, quoting, unknown statements and parse errors; allow redaction on read, not silent data loss at ingestion.
2. **Dialect plugins:** IOS/XE/XR/EOS-style hierarchy, Junos set/hierarchical syntax including groups/apply-groups/exclusions and inheritance, SR Linux/YANG model-driven config, SONiC JSON/DB plus FRR control config; narrow platform/version compatibility and explicit unsupported syntax. Do **not** assume one Cisco-like grammar handles all.
3. **Reference-resolution phase:** prefix/community/as-path sets, policy/route-map clauses, peer groups, templates, interface inheritance, VRFs, address families, RDs/RTs/VNI and next-hop policy refs. Preserve an explanation graph from effective fact back to source statement spans.
4. **Effective semantic IR:** only for verified feature subsets; evaluate ordered match/action policy and defaults with deterministic semantics. Model permitted/denied/unresolved, non-applicable and vendor-specific behavior. Do not convert unrecognized terms to permissive defaults.
5. **Typed, time-scoped semantic delta:** compare old and new AST/effective facts and project changed dependencies; bounded incremental invalidation and rollback. A 1-line RT or prefix-set change can affect many sessions/routes, even with no neighbor config diff.

Explicit stages of completeness: `parsed syntax` ≠ `references resolved` ≠ `effective semantics proven` ≠ `configured dependencies projected` ≠ `operationally observed`. Export each stage and negative reasons in the UI/API.

### 5.3 Example semantic relationship

~~~text
PE1 --HAS_VRF--> customer-blue
customer-blue --IMPORTS_RT--> 65000:101  [configured, config snapshot A]
PE2 --EXPORTS_RT--> 65000:100             [configured, config snapshot B]
PE2 --ADVERTISES_ROUTE--> VPNv4 NLRI      [observed, BMP Adj-RIB-Out view]
PE1 --RECEIVED_ROUTE--> VPNv4 NLRI        [observed, BMP Adj-RIB-In view]
PE1 --HAS_INSTALLED_ROUTE--> ?            [unknown: no authenticated FIB view]
policy_check --DISAGREES_WITH--> RT pair  [effective, only if applicable policy solved]
~~~

The analysis may say “import RT mismatch is consistent with the missing route,” **not** “definitively caused the failure” unless the required policy/FIB evidence excludes other causes. RD makes VPN routes unique; **RT controls import/export membership**. Prefix NLRI, AFI/SAFI, VRF/table and observation view must be explicit. A P router carrying only underlay routes must not be treated as a missing MP-BGP failure.

### 5.4 Incremental semantic graph

Use explicit node/edge families: `ConfigSnapshot`, `ConfigStatement` (only selected non-secret fact with encrypted offset), `ConfigPolicy`, `PrefixSet`, `CommunitySet`, `BgpNeighborConfig`, `AddressFamily`, `RoutingInstance`, `RouteTarget`, `RouteDistinguisher`, `Vni/Evi`, `InterfaceConfig`, `NextHopDependency`, `FactEvidence` and `Assessment`, plus links to **existing canonical entities**, not new competing Device identities. Final graph schema must be reviewed for LadybugDB compatibility, indexes, migration/upsert rules, bounded cardinality, data access and freshness/retirement semantics.

Never emit a physical `CONNECTED_TO` solely because a config has a BGP neighbor. Never use an external site string to upsert or modify an existing authoritative site; enforce BN-015-U2/BN-016-U1. Use an idempotent, source-scoped projection transaction with replay-safe checkpoints and tombstones. If projection fails, source facts remain durable and write checkpoint does **not** advance. An unresolved device receives a quarantined evidence node, not an authoritative config link.

## 6. SONiC: design-for-extension exemplar, not just another parser

**SONiC is not simply “Linux + FRR” or a Cisco-like running configuration.** Official SONiC documentation describes the Redis-backed `CONFIG_DB` (configuration), `APPL_DB` (application state), `STATE_DB` (state/dependencies), `ASIC_DB` (ASIC programming representation) and `COUNTERS_DB`; the management surface may expose SONiC-specific/native gNMI and YANG/OpenConfig. Capabilities differ by distro/image/platform. Multi-ASIC devices can have a host/global namespace and multiple NPU namespaces with **different DB instances**, interfaces and control components.

### 6.1 Proposed SONiC adapter contract

| Axis | Concrete requirement |
|---|---|
| Identification | Detect image/version, build, HWSKU/ASIC count, host namespace and per-NPU namespace; mark unknown if probe denied |
| Safe acquisition | Prefer authenticated gNMI Get/Subscribe of supported models/DB data; read-only CLI/NETCONF/API fallback when *verified* for that image; never assume direct Redis exposed remotely |
| Configuration | Normalize live CONFIG_DB tables (PORT/PORTCHANNEL/VLAN/INTERFACE/VRF/ACL/DEVICE_METADATA etc.) with schema-specific key separation; separately model FRR/BGP config where required |
| State | APPL_DB route/next-hop and STATE_DB dependency/health observations labeled as intermediate state; COUNTERS_DB rate/unit normalization; optional ASIC_DB with strong qualifier |
| Identity | Key interface and route records by (canonical device, ASIC namespace, network instance/VRF/table, interface/prefix), preserve Linux/FRR internal names as aliases |
| Relation | Port ↔ LAG ↔ VLAN ↔ VRF ↔ next hop ↔ configured/observed route; model per-NPU internal links separately from external cables |
| Resilience | Disconnected namespace, down DB service, missing telemetry path, partial DB sample, stale poll and cross-namespace duplicate interface name must not merge unrelated state |
| Security | No global privileged Redis or device write; read-only accounts, TLS/host-key validation, scoped credentials and sensitive tables excluded from model output |
| Acceptance | Genuine SONiC single-ASIC and multi-ASIC fixtures, one live SONiC lab (hardware/ASIC claims withheld unless independently proven), restart/replay, negative alias and stale-state tests |

Examples: a `PORTCHANNEL_MEMBER` config relationship is *configured membership*, while its operational LAG state comes from independent capture. A route programmed through APPL_DB is not guaranteed to have reached a real ASIC. A packet trace requiring physical dataplane proof must show `not validated` unless verified by a suitable independent observation.

The same extension model should handle **Cumulus/SONiC Linux+FRR**, **Junos**, **IOS-XR/XE**, **EOS**, **Nokia SR Linux/SR OS**, and future vendor families without core `if/else` branches. A module author supplies fixtures and support claims for each capability, not just a vendor name. Community extensions run in restricted trust modes until reviewed and verified.

## 7. Path and policy intelligence: three honest path modes

`/api/path` currently exposes a graph connectivity route, not a forwarding trace. Introduce a separate **typed path query** with `mode = topology | expected | observed | compare`; do not silently change the old endpoint. Inputs: canonical source or source attachment, destination IP/MAC/prefix, tenant/VRF, AFI/SAFI, packet-class selectors (when supported), as-of time/view, max hops/branches/depth, trust policy, timeouts. Every hop includes route/neighbor/interface references, evidence, coverage and reason codes.

1. **Topology path:** physical/logical adjacency from LLDP/cables, not proof of forwarding. A BGP session or CMDB path is separately typed evidence.
2. **Expected path:** compile applicable configuration/intent into a limited forwarding hypothesis, using route-policy/VRF resolution, longest-prefix match, interface/ARP/ND/recursive next hop, ECMP alternatives and encapsulation semantics **only for certified features**.
3. **Observed path:** walk current/captured RIB/FIB/LFIB/neighbor/overlay state, with source/view and as-of evidence. **BGP Loc-RIB or APPL_DB is not automatically hardware FIB**; distinguish software installed, device-reported, and probe-validated.
4. **Compare/assure:** show disagreements and unknown segments with tests, not a single green line. Support “config says route eligible; BMP shows received; FIB unavailable” as a valid partial result.

### 7.1 Algorithm contracts

- LPM selects within the correct VRF/table/AFI; next-hop resolution is recursively bounded, memoized per snapshot and loop/cycle-safe. Record each decision and the matching route or policy.
- ECMP and policy-based routing can return **multiple** bounded branches. Record excluded branches, pagination/truncation and unknown load balance; do not infer that all paths carry a specific flow.
- L2 forwarding requires MAC/ARP/ND/VLAN/VNI context at an observation time, not only LLDP; handle asymmetry, MLAG, moves, split horizon, overlay VTEP recursion.
- MPLS/SR/VPN/EVPN are **separate feature gates**: label stack, transport/SR policy, RT import/export, overlay encapsulation, remote VTEP and EVPN route type each need live semantic witnesses and source freshness.
- TTL/hop, VRF import mismatch, missing neighbor, device outside scope, identity conflict, stale source, collection gap, unsupported feature, data-plane unavailable and path-loop use distinct terminal reasons.
- Bound worst-case hop count, ECMP fan-out, memory/time and graph query cost; fail with `partial/truncated` and cursor, never `no path exists` on an incomplete observation set.

### 7.2 Independent validation

Build lab oracles: FRR VPNv4/v6 PE-P-PE with real RD/RT import/export, EOS/FRR FIB checks, SR Linux config, EVPN/VXLAN leaf-RR and SONiC single-/multi-ASIC when available. Capture genuine config, state, BMP and forwarding/probe evidence at controlled times. Compare native assessments with independent device command output and optionally **offline** Batfish/SuzieQ path results where supported; do not require those tools in deployed Bonsai. Test negative evidence (missing RT, route withdrawal, stale state, duplicate IP, BGP-LS duplicate area/ID, EVPN MAC move and unsupported ASIC) before a validated badge.

## 8. Evidence-first LLM and operator experience

**LLMs must never be the parser/normalizer/authoritative graph writer.** The agent receives a small bounded `InvestigationEvidenceBundle` prepared by deterministic code: question/scope, fact IDs, query version, authorized config excerpts, as-of, intended/config/effective/observed/validated state, source provenance, counterevidence, unsupported feature reasons, collection completeness, path branching/truncation, tested invariants and audit ID.

Curated, typed, scope-enforced operations (illustrative):
- `get_device_configuration_facts(device_id, feature, as_of)`
- `explain_effective_policy(device_id, vrf, neighbor, prefix, as_of)`
- `compare_network_state(entity, baseline, candidate, planes)`
- `trace_network_path(source, destination, vrf, as_of, mode, packet_class?)`
- `explain_topology_conflicts(scope, layer, as_of)`
- `get_evidence(evidence_id)` (redacted, authorized)
- `assess_change_impact(change_id, scope)` (read-only, hypotheses and checks)

Each response is role/tenant/VRF/device-scope checked **after** reference resolution and includes exact limits. Default `audit_only` ownership compatibility in BN-005 is **not a sufficient multi-tenant proof**; do not ship cross-tenant analysis until strict scope gates are exercised. Prompt-injection text in device config, LLDP descriptions, syslog and CMDB must be treated as **untrusted data**, not instructions. No raw credential, secret vault, arbitrary Cypher/SSH or unauthorized config download in a tool result. User-approved operational writes remain outside this project and behind BN-001/002/014.

### Unified UI, not a rewrite

Extend existing onboarding/Devices/Topology/Routing/Investigations surfaces:

- **Onboarding:** declare first, collect second; per-device/NOS capability grid (`configured/observed/unsupported/untested`), safe capture-plan preview, credentials and last successful collection; no forced gNMI/NetBox.
- **Device inspector:** “Configuration / Effective / Operational / Validation” tabs with parser quality, timestamp, diff and redacted source anchor; no fake healthy badge from `enabled`.
- **Topology:** physically connected, BGP/IGP control, VPN/EVPN service, configuration dependency and observed forwarding **separate selectable layers**; filter by scope, site subtree, VRF and as-of. Inventory-only and conflict states remain visible.
- **Path explorer:** three modes + compare; branch alternatives, per-hop source and gaps, exportable bounded evidence report; “No supported path can be proved” ≠ “no network path exists.”
- **Investigation:** suggested questions with deterministic evidence citations, confidence/unknown reasons, independent verifier results, LLM narration and counterfactuals. Show “provider unavailable” without losing useful deterministic explanation.
- **Administration:** signed/vendor profile versions, safe activation/rollback, poll budget, parser/semantic coverage and plugin audit; operator can disable one module without disconnecting all devices.

Keyboard/table alternatives, rendering at 1k devices/5k links, stale-refresh behavior, scoped export and offline/permission UX must reuse BN-019-U1 gates. P0 UI/site/false-edge defects take precedence.

## 9. Real-world safety, scale, migration and operability

- **Security by construction:** allowlisted read-only command templates, no arbitrary end-user shell; host-key pinning, TLS validation, credential vault scope, per-device/RBAC/tenant/VRF checks, output redaction, file/regex/JSON limits, parser fuzzing, bounded subprocess for untrusted extensions, no dynamic parser expressions. Audit capture and evidence reads. A single runtime is not permission to run unsafe third-party parsing in-process.
- **Capture budget and resilience:** user-controlled schedule per feature; per-site/vendor/device concurrency caps, rate/CPU/SSH load budgets, jitter, cancellation, retry/backoff; avoid polling full RIB repeatedly if BMP/gNMI already supplies coverage. A CLI/NETCONF/gNMI failure remains a source-specific error, not “device down.”
- **Graph and history:** semantic snapshots can be large; store bulk routes in indexed routing store, not one vertex per repeated route read. Deterministic projection IDs, backfill/replay versioning, double-write shadow mode, field-level authority, tombstones, bounded migration and rollback. Do not alter existing graph authority or stored snapshot plaintext format without a migration/recovery drill.
- **Deployment:** standalone feature-off-by-default → shadow → explicit per-device opt-in → staged default, with per-feature rollback and a measurable resource envelope. HA/fleet/disconnected collector and backup/restore tests follow BN-023/019; no undocumented all-in-one deployment claim.
- **Data lifecycle:** declared retention, authorized deletion and reprocessing, encryption key rotation, schema upgrades and immutable raw-evidence hash. Provenance/consent and tenant-scope migration are especially important for third-party/license-bound data.
- **Vendor contract:** versioned support matrix with D decoded, I ingested, N normalized, S semantically resolved, P projected, V independently verified, O operationally proven; exact version/role/AFI/feature/transport. No “supports SONiC” umbrella row without a tested feature and image.

## 10. Evidence-first verification program

| Scenario | Given / stimulus | Expected result |
|---|---|---|
| A: no integration/no gNMI | Empty Bonsai, one CLI-only P router, one offline PE | Inventory-only presence, optional read-only collection; no fabricated protocol/path state |
| B: source collision | Same hostname/10.0.0.1 in two sites/tenants/SONiC namespaces; NAT BMP exporters | No cross-scope merge or wrong graph edge; quarantine unresolved facts |
| C: parser equivalence | Pinned SuzieQ EOS/FRR/Junos/IOS-XR samples with nested list, blank VRF and ambiguous fields | Rust normalized facts match approved goldens or explicitly documented correction; no line-array “success” |
| D: config inheritance | IOS-XR/EOS peer group, Junos apply-groups, conflicting inherited policy | Effective policy has source spans, correct precedence and typed holes when unimplemented |
| E: VPN route-target mismatch | PE1 export RT differs from PE2 import RT, BMP route received, FIB unavailable | Cautious policy mismatch assessment; no fabricated forwarding failure or success |
| F: route withdrawal / stale snapshot | BMP withdraw at T+3, CLI snapshot success at T, collector restart gap | As-of view distinguishes source observation times; uncertainty, no stale “route present” override |
| G: path candidates | ECMP, recursive next hop, P-router underlay-only, loop, unresolved ARP, unsupported ACL | Bounded branches and termination reasons, never simple BFS claimed as forwarding |
| H: EVPN VXLAN | Leaf/RR, VNI/RT and MAC move; lost LLDP/one-sided endpoint | Separate overlay vs physical links and mobility validity; independent negative check |
| I: SONiC single ASIC | Live CONFIG_DB vs saved file differ; APPL_DB route without FIB proof | Correct live-vs-startup plane and “hardware unknown” status |
| J: SONiC multi ASIC | Host + two NPU namespaces duplicate interface/VRF-like keys, missing one DB | No collision, per-namespace completeness, no invented physical cable |
| K: adversarial inputs | Malformed/very large output, template expression abuse, secret-bearing config, prompt-like interface description | Bounded failure, no secret in logs/graph/LLM, no arbitrary code or shell |
| L: upgrade/rollback | Replay old facts and partial graph write; disable native parser mid-rollout | Deterministic idempotency, projection checkpoint only after commit, fallback retains existing data and functionality |
| M: usability | Operator finds missing VPN route; no AI provider and Viewer role | UI shows deterministic evidence, partial path/unknown, redacted scoped source, terminal provider state and no forbidden controls |
| N: performance | 1k devices, high-cardinality routes, 5k links, concurrent snapshots, slow SONiC API | Measure ingestion rate, memory, DB cardinality, p95 path latency, impact on BMP queue; budgets set before promotion |

**Quality scorecard (measure, do not invent targets):** normalized-field precision/recall vs independently reviewed goldens, semantic policy correctness, unsupported-rate honesty, wrong-edge/tenant-merge count (zero accepted in security gates), as-of correctness, path false-proof rate, time to explain real incidents, collection device load, queue drop accounting, p95 query latency, disk/CPU/RSS, operator task success, LLM grounded answer accuracy against independent adjudication. Quantitative thresholds set with measured baseline and ratified before feature promotion.

### Release gates / dependency order

**G0 — trust/correctness:** fix BN-015-U1/U2, BN-016-U1 and required BN-008/021/022 boundaries; pin upstream, license and baselines. **G1 — native N normalization:** approved golden fixtures and safe read-only captures; ship one vendor/feature slice behind flags; fallback retained. **G2 — syntax/effective configuration:** two verified dialect/feature families with negative tests; graph projection as-of/idempotency proven. **G3 — forwarding/path:** compare expected vs observed and independent probes; unknown and conflict cases pass, no unconditional path guarantee. **G4 — operator and production:** UI/a11y/scoped evidence/LLM evaluation, resource and restart/upgrade/rollback labs, SONiC feature-specific live gate. No stage automatically authorizes higher-level claims.

A legitimate initial pilot can stop at G2 while still providing useful typed configuration and routing-policy facts. “Full network understanding” is an **expanding, versioned evidence contract**, not a binary feature flag.

## 11. Open design decisions to resolve before coding

- Canonical typed fact serialization (Rust serde + schema format), evidence retention and graph cardinality limits; bind to existing graph migrations and RIB/journal versions.
- Whether normalized capture lives in the current enrichers or a dedicated read-only work queue, without competing durable write authorities.
- Which safe extraction DSL subset covers most SuzieQ service definitions and which edge cases require hand-coded Rust logic.
- Exact first NOS/feature slice using available live fixtures (e.g., EOS/FRR interface/BGP/VRF), plus Junos/XR/SONiC test-image access/rights. No commitment to unavailable hardware or proprietary image.
- Configuration policy semantics depth: deterministic subset with unsupported fallback vs future independent symbolic solver, including vendor version drift.
- Data security classification and per-tenant secret redaction policy across raw snapshots, graph, UI and prompts.
- Which native modules can safely run in-process, which need restricted subprocess; source-integrity and third-party license review.

**No unresolved design decision is permission to invent a default.** BN-024-00 through BN-024-03 must choose these contracts and publish failing tests before major ports.

## References (primary source, not implied validation)

- SuzieQ pinned OSS: https://github.com/netenglabs/suzieq/tree/75198b5585ebef91f0659b6b1ec3c964cc2cd1ad — `suzieq/config/`, `suzieq/poller/worker/services/`, `suzieq/engines/pandas/path.py` and `devconfig.py`, `tests/integration`, LICENSE.
- Existing Bonsai [parsing ADR](adr_suzieq_vs_pyats.md), [source-of-truth/UI audit](UI_ONBOARDING_TOPOLOGY_PRODUCT_REVIEW_2026-11-11.md), [config-state lane](config_state_lane.md) and [network-agnostic review](NETWORK_AGNOSTIC_READINESS_REVIEW_2026-11-11.md).
- SONiC [telemetry/database layout](https://github.com/sonic-net/SONiC/blob/master/doc/system-telemetry/grpc_telemetry.md), [management framework](https://github.com/sonic-net/SONiC/blob/master/doc/mgmt/Management%20Framework.md), [multi-namespace DB](https://github.com/sonic-net/SONiC/blob/master/doc/database/multi_namespace_db_instances.md) and [multi-ASIC architecture](https://github.com/sonic-net/SONiC/blob/master/doc/multi_asic/SONiC_multi_asic_hld.md). These are design capabilities; verify actual image/version support before relying on an API.
