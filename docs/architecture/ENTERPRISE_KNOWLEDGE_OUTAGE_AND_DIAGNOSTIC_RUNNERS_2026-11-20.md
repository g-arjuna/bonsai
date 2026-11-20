# Enterprise knowledge, dark-site assurance and diagnostic-runner integration

**Research/intake only — 20 November 2026.** Repo baseline: main at bd7f1c776cdef441f12e5b1e3bb9b358bbc89001. No runtime changes, new authority, schema migration, live device commands, external service calls or performance acceptance in this review. Product work is tracked only in [BN-028](../../BONSAI_BACKLOG.md#bn-028--enterprise-knowledge-and-dark-site-assurance-20-november-2026); preserve BN-021/022/024 and operational no-device-write policy.

## 1. Decision: record what an enterprise knows without pretending it was observed

Bonsai should offer a **native knowledge and incident workbench** with **optional, independently deployable ingestion/diagnostic workers**. The differentiator is a deterministic, temporal, citeable account of intended, documented, observed and inferred network relationships, **including when the network is not currently observable**. It is not a replacement for NetBox, Nautobot, Infrahub, ITSM, an enterprise document platform, a network automation engine or an LLM platform.

Keep four truth lanes separate:
1. **Declared/approved intent:** design record, approved circuit, service mapping, DR path, site OOB plan and runbook; owned by the originating system/team.
2. **Observed evidence:** gNMI/config/LLDP/BMP/RIB/syslog/flow, verified collectors and timestamped device/API checks.
3. **Inferred or extracted candidate:** OCR/LLM/diagram association, discovered alias, possible dependency or hypothesis; not authoritative.
4. **Incident reasoning:** deterministic checks plus ranked hypotheses/counterevidence, with unknown/partial/stale as first-class results.

Documented LLDP links can preserve a useful *last known* network map during a site outage, but are NOT proof of present adjacency. A circuit record is a planned dependency, not proof a carrier circuit is currently up. A silent collector is not proof the site is down. Evidence validity and time matter as much as entity identity.

## 2. Bonsai implementation seams and source-audited gaps

Static inspection of current main (no runtime or lab gate in this review):

| Existing seam | What it offers; gap to close |
|---|---|
| src/enrichment/netbox.rs | Read-only NetBox devices/interfaces/sites/VLANs/prefixes and graph enrichment; no circuit or power-feed retrieval in this adapter. Expand mapping through existing enrichment authority; do not create another independent Device owner. |
| src/graph/common.rs and src/graph/mod.rs | Device/Site/address/topology graph; no Circuit graph class in the inspected graph source. Extend BN-024 typed fact and graph projection with explicit site/provider/OOB/power relationships. |
| src/assignment.rs | Collector assignments and authority epochs; assignment site/role is not a tested fault-domain/reachability model. Site location, collector execution placement, network path, backhaul and OOB independence must be declared and observed separately. |
| src/collector/graph.rs | Collector intentionally does not keep the full Site hierarchy or subscription status in its reduced graph. Do not assume offline collectors have all the knowledge required for independent diagnosis. |
| src/collector/diagnostic_server.rs | Diagnostic service is disabled by default; when enabled the inspected listener binds all interfaces on plaintext HTTP with optional header password. Do NOT use this endpoint as a general remote command/OOB channel. Loopback/mTLS gateway and strict boundary required before enterprise promotion. |
| src/ingest.rs; docs/collector_core_protocol.md; docs/architecture/GLOBAL_TELEMETRY_TRANSPORT_REVIEW_2026-11-19.md | Forwarding queue and at-least-once retry exist, but core accepted means in-memory bus publication, NOT durable graph commit. Source-reviewed queue durability/ack/compaction risks remain; no claim that a site's complete outage history will synchronize without loss. |
| src/investigation_runtime.rs; src/mcp_server.rs | Scoped read-only investigation tools, missing-prefix explanations, bounded results; not yet document-citation/knowledge reconciliation or an OOB/site-blindness investigator. Do not add unrestricted shell/SSH tool dispatch here. |
| src/integrations/servicenow_aiops.rs | Incident synchronization exists; carrier-ticket creation, ISP contract/API rights, dedup and policy approval are **not** established by it. |
| docs/architecture/BROWNFIELD_ENTERPRISE_COEXISTENCE_2026-11-19.md; NATIVE_NETWORK_INTELLIGENCE_DESIGN_2026-11-11.md | Already define brownfield federation, existing BN-024 FactEnvelope, source authority and temporal reconciliation. Reuse rather than replace. |

These are **static findings and omissions**, not verified exploits, runtime reliability results or proof that no neighboring module covers part of a feature. Reconcile at implementation start.

## 3. HLD: add an optional knowledge plane, not a second telemetry engine

    Enterprise sources:
      Git/Markdown, wiki/SharePoint/Confluence, PDF/DOCX, CSV/XLSX,
      diagrams, CMDB/IPAM/NetBox/Nautobot/Infrahub, ITSM, circuit inventory,
      facility/UPS/PDU, SD-WAN controller and operator-authored runbooks
                      |
       versioned source connectors + bounded extraction workers
                      |
       immutable source revision / digest / ACL / provenance ledger
                      |
       deterministic parser + candidate extraction + typed validation
                      |
         human approval or explicitly trusted field mapping
                      |
       canonical FactEnvelope + reconciliation + single graph writer
                      |
          existing Ladybug semantic graph (bounded references)
            /                   |                   \
    authorized source       full-text/optional       existing observations
    pointers and blobs      embedding side index     + routing journals
            \                   |                   /
           one scoped evidence API / MCP / incident workbench
                                   |
                optional approved diagnostic integration runner
                                   |
             remote sites / OOB / provider portal / ITSM systems

Source documents and bulk extracted text should stay in the source repository or a bounded, access-controlled content store; Ladybug holds **semantic nodes/edges, citation pointers and current/temporal assertions**, not PDFs, chunks, credentials, every log line, or a vector per observation. A text index (keyword/BM25 first; vector/embeddings optional) is a separate, rebuildable derived index: never promote embedding similarity to a graph fact. Use existing write coordinator, tenant/source-scoped reconciliation, identity authority and shared UI; optional worker failures must not stall telemetry ingestion.

This maps to the [NAF Framework](https://reference.networkautomation.forum/Framework/Framework/): existing Bonsai collection + observability, federated intent, separate orchestration/execution and presentation. The [IETF NMOP knowledge-graph construction draft](https://idomingu.github.io/draft-marcas-nmop-kg-construct/draft-marcas-nmop-kg-construct.html) motivates explicit ingestion, semantic mapping, materialization-versus-virtualization and provenance; it is an **Internet-Draft, not an RFC**.

## 4. LLD: identity, statement provenance and competing sources

Minimum proposed types (versioned serde schema before implementation):

| Contract | Required semantics |
|---|---|
| KnowledgeSource | immutable source ID, type/URI, tenant/site scope, connector principal, approved owner, classification, per-field authority, refresh mode/TTL, disabled/revoked state, retention and deletion policy. |
| SourceRevision | revision/ETag/commit + SHA-256 digest, fetched/modified time, ACL version, redaction policy, parser/mapping revision, error/partial result and source locator; encrypt original sensitive content or reference approved upstream location. |
| DocumentAnchor | page/section/table/sheet/cell or diagram element, content span/hash and stable revision pointer; no citation without retrievable authorized evidence. |
| KnowledgeAssertion | stable scoped subject/predicate/object, assertion kind (declared/observed/extracted), valid-from/to, recorded-at, source revision+anchor, mapper version, reviewer/approval, confidence **only for extraction**, freshness and conflict/retirement status. |
| SiteReachability | site, plane (data/mgmt/OOB/collector-control), vantage point/fault domain, path/dependency IDs, protocol/check result, observed-at, receipt-at, source epoch, coverage and reason for unknown. |
| DiagnosticReceipt | proposal ID, principal, target, approved template/version/parameters, authorization scope, runner placement+attestation, command/transport policy, start/end, redacted output digest/pointer, explicit timeout/denied/partial outcome. |

Canonical entities: Tenant/Environment, Site/Facility, Device/Interface, Prefix/VRF, Service/Application, NetworkSegment, Circuit/Provider/Contract, VPN/Tunnel (IPsec/DMVPN/GRE/MPLS/SD-WAN; *configured* vs *observed*), Collector/ObservationPath, OOB Endpoint/Console/ManagementPath, PowerFeed/PDU/UPS/FacilityDependency, Owner/Runbook/Change/Ticket/Document. Prefer scoped ID derived from tenant + authoritative source key, **not just hostname, IP, circuit number or site name**. Use time-bounded same-as candidates and approved resolution; overlapping RFC1918 addresses and multiple carriers with the same circuit ID must never collapse.

Examples:
- Document says branch A has primary MPLS circuit C1, backup internet C2 and LTE OOB. Record three distinct paths, intended roles, provider contacts and their dependencies. Do not infer live routing/active backup.
- Legacy HLD says tunnel DMVPN HUB1-SPOKE1. Store the assertion and citation; only config/runtime observations can establish effective tunnel/crypto/overlay status.
- LLDP last observed at T0 is preserved with historical validity. At T1 when telemetry stops, label it last-known/stale, not a currently verified link.
- Facility document maps router PSU→rack PDU→UPS→utility supply. Absence of PDU data cannot establish a utility-power cause.

**Reconciliation:** use precedence **per attribute and scope**, not a global source trust score. For example, IPAM owns intended prefixes, contract system owns provider circuit ID, current device observation owns operational interface state; document prose proposes but does not silently override. Keep both conflicting assertions, cite each, offer explicit review. Every extractor must support incrementally **updated, deleted, superseded and revoked** sources, with tombstones and rebuild/replay. Snapshot partial failure must not wipe existing facts or mark missing children deleted.

**Ingestion order:** (1) curated CSV/JSON schema and manually approved runbooks; (2) bounded NetBox circuit/provider/console/power inventory plus existing devices; (3) Git-backed Markdown and wiki; (4) PDF/DOCX/XLSX with table anchors; (5) diagram extraction as suggestions only; (6) incremental external SoTs and enterprise document connectors. Source selection is opt-in and read-only. No untrusted macros, embedded scripts, SSRF, unsafe ZIP expansion, unlimited OCR or automated external-provider transmission. Credential/account info, shared secrets, private keys, device passwords, SNMP communities, recovery codes and sensitive client data must be stripped or denied before index/graph/model usage. ACL checks at ingest, search, anchor resolution, graph joins, export and deletion; cache/embedding/backup deletion semantics included.

## 5. Dark-site investigation: model *observability* and dependencies

A site status needs **independent axes**, each with explicit freshness and vantage point:

| Axis | Example observations | Invalid conclusion |
|---|---|---|
| Enterprise data path | WAN SLA/route, BGP/BFD, tunnel/IPsec, provider alarm, remote synthetic reachability | No route seen does not prove utility power loss. |
| Management/OOB path | Dedicated LTE/console, mgmt VRF, remote PDU API, cloud device controller, out-of-band probe | Mgmt accessible does not mean customer traffic works; its independence must be verified. |
| Collector path | Collector agent heartbeat at Core, target sampling health, assignment authority, site/region placement, outbound transport and queue watermark | No received data does not mean device is dead; collector itself may be partitioned. |
| Facility and independent provider | UPS/PDU sensor status, site power events, ISP API/ticket, secondary carrier peer signal | No event is not proof of healthy power/provider. |

Placement is a **site relationship**, not an implicit property of a Collector ID: in-site versus regional/cloud, power and network fate-sharing, which management route each device uses, inbound reachability, queue durability/remaining disk and autonomous independence. A collector physically at a dark site may have perfectly buffered metrics but be unreachable by Core; until replay its latest observations are **unknown to Core**. Conversely, a regional collector can lose device access while retaining a healthy Core heartbeat.

Proposed deterministic incident flow:
1. Record exact **last positive observation** and individual source watermarks/epochs; open a SiteEvidenceGap episode once source-specific expected windows expire (do not mark all devices down).
2. Fan out bounded read-only checks from **independent vantage points**: last known upstream/PE/routing event; backup carrier, OOB mgmt, collector control channel, carrier alarm and remote power where preauthorized.
3. Classify evidence into **data-plane impaired; management-only impaired; collector partitioned; facility suspect; provider suspect; mixed; indeterminate**. Never equate two shared-fate checks with two independent witnesses.
4. Retrieve scoped as-of topology, design, circuit, ISP contract, dependency chain, runbooks, service/customer impact and last-known LLDP/config/BMP without inventing current state.
5. Choose the next discriminating test by information gain and policy/cost: check regional PE/BFD, OOB console, UPS event or carrier portal. Return evidence IDs, missing checks, access availability and alternate hypotheses.
6. On reconnection, ingest stamped source-time records with assigned collector epoch, gaps/coverage, late and duplicate reconciliation. Update episode causality only with corroborating evidence; keep the original incident decision and corrections for audit.
7. Draft a carrier ticket **only when policy allows**: provider/circuit/site/customer scope, alarm and time window, test results, contract/SLA, previous duplicate; no claim ISP-fault solely because BGP session dropped. Offer human approval and upstream ITSM incident linkage.

Sample reasoning: 09:02 primary WAN BFD goes down at remote PE; Core cannot poll site; OOB LTE console responds at 09:04 and UPS reads normal; site collector heartbeat may be missing. Outcome: **likely primary carrier/WAN isolation; power failure not supported; collector replay pending**. If LTE and UPS are also unreachable over the same backhaul, independence is unproven; outcome remains **indeterminate**, not automatic power-failure diagnosis.

## 6. Diagnostic runner vs full network automation

**Default: Bonsai-native typed diagnostic API, not arbitrary SSH from Core.** Read-only diagnostics are sometimes necessary for stale/no telemetry. Reuse BN-024-04 capture, BN-022 authority, the existing credential vault and investigation/audit runtime. Add an optional site/regional **DiagnosticRunner** adapter (Rust first when performant/safe; interoperable with existing AWX/Ansible/Nornir/pyATS/NAPALM/Scrapli workflows). The runner must have pinned SSH host keys or mTLS/controller validation, narrowly scoped approved commands (exact templates + validated structured parameters, not prefix allowlist or shell), no escalation/config terminal, hard deadline/output/concurrency/site budget, explicit operator or preapproved *read-only* incident policy, no stored output secrets, audit-before-call and credential separation. Treat the device output as hostile/untrusted text including prompt injection.

Separate stages: **proposal → policy/approval → dispatched worker → bound result/timeout → redaction → typed assertion → review**. Never let an agent use retrieved runbook text to request unauthorized commands. Unknown vendor and malformed command → deny. Use explicit provenance on all fallback CLI parsing; no silently successful empty table. Reject diagnostic work when trust and auth are unavailable. No live writes, reboot, interface shutdown, config changes, provider API mutations or automated ticket filing under this intake.

Comparison:
- [Transit Community Edition](https://github.com/transit-community/transit): useful example of session/serial/packet-capture tools, on-device read-only policy checks, per-command human approval and local credential isolation. Desktop operator tool; **not** a reason to embed an interactive SSH terminal into Core or reuse code without license/security review.
- [GlobalNOC troubleshooting talk](https://www.youtube.com/watch?v=VoMPsTpvnvU): describes Nagios→ServiceNow→AWX workflow selection and bundled read-only evidence; borrow the **incident-to-evidence procedure**, not a mandatory AWX stack.
- [Nornir](https://nornir.readthedocs.io/en/latest/), [NAPALM](https://napalm.readthedocs.io/en/stable/), [Scrapli](https://scrapli.github.io/scrapli_community/user_guide/basic_usage/), pyATS/Genie: useful driver/getter/parser reference or external executors; native Rust only after measured use case, parity/semantic tests and license review.
- [Batfish](https://batfish.readthedocs.io/en/stable/notebooks/differentialQuestions.html): optional external offline config reachability/diff check; always label **modeled** vs live observed, and record input snapshot. [Temporal](https://docs.temporal.io/durable-execution): reference for durable workflow state, not mandatory runtime dependency.
- NetBox/Nautobot/Infrahub remain source-of-truth peers. Prefer their own circuit/site/power models and ownership to designing a universal new CMDB inside Bonsai.

## 7. Native product interfaces and operational safeguards

One **Site Outage** workbench under existing Investigations: incident timeline and freshness by source, alternate-vantage grid, collector placement/fate sharing and pending replay, **last-known topology** (time slider), power/ISP/OOB dependency layer, documents/runbook citation drawer, next safe diagnostic, ticket draft with approval and service-impact estimate. Do not fabricate availability by using green/grey telemetry gaps; use unknown, stale, complete, partial and conflicting badges.

Candidate read API and MCP tools, all derived from one typed read service with principal-filtered discovery:
- get_site_context, get_site_dependencies, get_site_observability, get_last_known_topology, get_knowledge_citations, compare_documented_vs_observed, explain_site_darkness, propose_diagnostic_check, get_diagnostic_result and draft_provider_ticket.
- Versioned source/admin APIs for import preview, mapping, owner/authority approvals, sync health, conflicts and removal. Every request bounded by principal/tenant/site/time and schema; signed/opaque authorized cursors, query budgets and no raw Cypher/LLM-issued SSH. A citation anchor must be reauthorized at dereference, including after source ACL changes.

Security gates: no auto-upload of enterprise documents to an LLM or embedding SaaS; embeddings opt-in per data class and approved local model/region. Prompt injection and adversarial HLDs/diagrams must never change ACL, graph authority, diagnostic policies or tickets. Avoid searching/indexing secrets. Ensure facility contacts, circuit account details and OOB endpoints have stricter entitlements than generic topology. Source disabled/revoked, tenant offboarding and legal deletion must propagate to materialized assertions, fulltext/vector caches and exports.

Performance gates: async source workers with bounded parse/file/table/diagram budgets, deduplicated digest/revision, batch writer and incremental diff; no large documents through telemetry channels, no graph node per text chunk or time sample, no heavy PDF/vector inference on ingest Tokio workers. Measure parse bytes/sec, enrichment lag, queue age, index size and graph read p95 **while** telemetry writes run. Optional external search index is rebuildable. Measure site-outage data-loss/RPO and evidence lag with controlled disconnections instead of claiming offline durability from a configured spool size.

## 8. Acceptance scenario pack (first implementation order)

| Scenario and independent witnesses | Required verdict / negative case |
|---|---|
| Primary carrier outage; remote PE sees BFD down, independent LTE OOB/UPS up | Data plane impaired, power hypothesis unsupported; no automatic ISP blame/ticket without carrier evidence and approval. |
| Utility power loss; independent remote UPS/PDU alarm, site OOB absent, two carrier paths lose devices | Facility suspect with evidence; shared-feed OOB absence not independent confirmation. |
| Data forwarding down but mgmt VRF still reachable; device config/optics available | Surface path-specific divergence, do not report site entirely unreachable. |
| In-site collector stores locally but loses Core; regional vantage remains alive | Collector partitioned + withheld live state; replay dedup, lateness/coverage and outage RPO bounded/measured. |
| Regional collector alive, device/management inaccessible; backup carrier and remote ISP probes present | Separate collector health from remote site health and source-specific blindness. |
| Both diverse ISP circuits lose shared last mile, correlated PDU independent alarm absent | Correlated shared fate rather than two independent carrier confirmations. |
| Stale HLD claims DMVPN/backup; config shows removal, LLDP as-of old | Conflicting/retired intent, no stale path treated live. |
| Two overlapping 10.1.1.1 sites; duplicate provider circuit ID; renamed site | No identity merge, no cross-tenant evidence leakage, explicit ambiguous mapping. |
| Untrusted PDF says to run configure terminal or exfiltrate credentials | Never promoted into diagnostics or tool policy; deny with auditable reason. |
| ITSM duplicate incident/provider ticket, restored device with delayed metrics | Stable dedup key, no duplicate external mutation, preserve correction chronology and outcome. |
| Parser/ACL/embedding failure and Core restart during projection | Previous approved evidence remains bounded/stale; no partial delete, replay idempotent, fail-closed on revoked sources. |

Stage G0: offline source fixtures, schema/identity/security/temporal ABI, no live graph authority. G1: one approved NetBox circuit+OOB mapping and Git runbook, UI citation, no LLM. G2: controlled branch outage with two independent vantage points, collector disconnection/replay and source/observability gaps. G3: allowlisted diagnostic integration, operator-approved carrier-ticket draft and browser/MCP adversarial journeys. Existing L0–L4 gates and real device/source evidence remain separate; mock-only success cannot promote a production claim.

## 9. Network Automation Forum material and research method

The requested [Network Automation Forum channel](https://www.youtube.com/@NetworkAutomationForum/videos) contains a large evolving library; channel video metadata and descriptions, the published AutoCon 0–6 programmes, NAF's [reference framework](https://reference.networkautomation.forum/Framework/Framework/) and its [docs-as-code article](https://networkautomation.forum/blog/network-automation-documentation-as-code) were accessible. **The full video library and full spoken transcripts were not accessible for end-to-end viewing. The following is a selected, relevant talk/programme synthesis, not a claim to have watched every video or verified every on-stage implementation.** Talk titles without independently accessible video description are used only as indicators of a research direction.

| Recorded talk or programme | Signal for Bonsai / interpretation limit |
|---|---|
| [Claudia de Luna — Start at the Beginning: Automating Network Design (AutoCon 3)](https://www.youtube.com/watch?v=PoQUA3dgmkQ) | Prefer structured, programmatically consumable design data over static HLD diagrams as a primary long-term model. Source: published talk description. |
| [Munachimso Nwaiwu — CLI to Model: Foundational SoT for Brownfield Networks](https://www.youtube.com/watch?v=5iIx-PYqbBI) | Seed typed context from existing network state when HLD is incomplete; keep observed vs intended provenance, not a blanket claim that reverse-engineered state is approved intent. Source: description. |
| [Joshua McNamara — Network Troubleshooting Automation](https://www.youtube.com/watch?v=VoMPsTpvnvU) | Alert→ServiceNow→AWX/Ansible diagnostic orchestration is a practical integration pattern. Source: published description. |
| [Dinesh Dutt — The NAF Network Automation Framework](https://www.youtube.com/watch?v=i1eve1uB-Rs) | Function-level modularity supports Bonsai as evidence/observability + optional federation, not a mandatory all-in-one orchestration vendor. Source: NAF published specification and session link. |
| [Jose Miguel Izquierdo — Docs as Code (AutoCon 2)](https://www.youtube.com/watch?v=80lWOkyRKf0) | Version documentation/diagrams with source data and CI; diagrams are derived representations, not authoritative merely by looking plausible. Source: NAF published article. |
| [AutoCon 4 programme](https://networkautomation.forum/autocon4): modular Ansible, brownfield source of truth, ESnet discovery, MCP abstraction, AI and troubleshooting | Prioritize source ownership, independent discovery and defined contracts over one generic executor. Programme titles/workshop descriptions, not viewed session transcripts. |
| [AutoCon 5 programme](https://networkautomation.forum/autocon5): Infrahub, NetBox closed loop, PyTest/mock failure, AI/RAG/MCP workshops | Reuse source-of-truth and validation tooling, exercise partial failures and negative cases; programme only. |
| [AutoCon 6 programme](https://networkautomation.forum/autocon6), 18–20 Nov 2026: federated Nautobot, intended config, mock devices, digital twins, AI evaluation, resumable workflows, Internet2 multi-agent troubleshooting | Relevant upcoming/just-delivered session themes, **not assumed published recordings** at this cutoff. Avoid unsupported claims of adoption or efficacy. |

Other primary references: [NetBox circuits](https://netboxlabs.com/docs/netbox/models/circuits/circuittermination/), [NetBox power feed](https://netboxlabs.com/docs/netbox/models/dcim/powerfeed/), [Infrahub schemas and provenance](https://docs.infrahub.app/topics/schema/), [OTel collector resilience](https://opentelemetry.io/docs/collector/resiliency/), [IETF RFC 9232 telemetry](https://www.rfc-editor.org/rfc/rfc9232) and [IETF NMOP KG construction draft](https://idomingu.github.io/draft-marcas-nmop-kg-construct/draft-marcas-nmop-kg-construct.html). Vendor capability and event title are **not** Bonsai implementation proof.

**Implementation ownership:** BN-028 adds the missing cross-domain domain model and scenario gates. Reuse BN-015 U1/U2/U6/U7 (NetBox/site), BN-008/016 (topology and time), BN-021 (trusted ingress), BN-022/AG (scope and disclosure), BN-024-02/04/14/15/20/21/22 (facts/capture/reconciliation/tools/UI/security), BN-009/019/023 (durability/performance/HA), BN-025/027 (environment/power), and existing ServiceNow integration. Sequence after Secure Core blockers, not as a competing migration.
