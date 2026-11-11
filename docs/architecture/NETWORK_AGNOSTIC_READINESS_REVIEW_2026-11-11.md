# Bonsai 2026 environment-agnostic readiness: source audit and bounded backlog decisions

**Review date:** 11 November 2026  
**Code baseline:** main at d9447fb14d9f23d82b244913da5a20c6efe91c8d  
**Scope:** repository source, current canonical backlog, selected existing lab profiles, Helm templates and UI/ML implementation. No new device, cloud, vendor, scale or live failover testing is claimed by this document. Concurrent NetFlow implementation was not changed.

## Executive finding

Bonsai is an unusually broad **integrated development prototype**, not yet a demonstrated vendor-, topology- and deployment-agnostic network digital twin or trusted autonomous operator. It has valuable actual components: gNMI discovery/profiles, SNMP/syslog/flow/OTLP receivers, multi-AFI/SAFI BMP parsing, a durable RIB/journal path, BGP-LS and EVPN decoders, LLDP/BGP/BFD/IS-IS graph data, stable trusted BMP exporter identity work, a graph-backed operator API, MCP/LLM investigation paths, rules/ML scaffolding, a usable multi-route SPA and a Helm chart. Bounded FRR/EOS/SR Linux evidence is real, but it is not evidence for arbitrary P/PE/MPLS, EVPN-VXLAN fabrics, SD-WAN overlays, Kubernetes HA or production ML.

**Make the product contract a trustworthy answer to “what is known, from which device/view/instance, at what time, with what contradictory or missing evidence?”** Protocol decoding is necessary but insufficient. Bonsai needs separately modeled physical/underlay, routing-control, VPN/overlay, installed-forwarding and application/service dependencies; reconciled identity and lifecycles; capability/coverage honesty; and deterministic, scoped evidence tools for every LLM.

The single active backlog remains BONSAI_BACKLOG.md. This review adds only the **new Kubernetes deployment acceptance lane BN-023**. Every other accepted finding is an **explicit bounded refinement** to existing BN-003/004/007/008/009/010/013/014/015/016/019/021/022, not a new competing roadmap. Historical backlog crosswalk and verified work remain intact.

## 1. What “agnostic” must mean

Agnostic does **not** mean claiming every protocol, vendor release or topology can be discovered without a compatible telemetry source. It means:

1. **Identity correctness across vendor, management IP reuse and transport NAT.** A raw IP, sysName, interface name, RD or observer is not a universal identity.
2. **Truthful capability negotiation.** Show per NOS/version/role/AFI-SAFI which facts are native, available through CLI/controller, unsupported, stale, conflicting or independently validated.
3. **Protocol-semantic preservation.** Treat BGP session, route/view, route target, LDP/SR/SRv6 label/SID, EVPN ESI/EVI/VNI, data-plane FIB and service intent as different kinds of evidence, not interchangeable links.
4. **Temporal graph semantics.** Represent upserts, withdrawals, deletes, EOR/synchronization, changes and unobserved periods without silently carrying stale topology forward as healthy.
5. **Useful bounded operational outcomes.** An unexplained route absence, unused link or QoS violation must be backed by measurements, explicit missing-observation categories and independent validation.

Use a five-level acceptance rubric per fact: **D** decoded, **I** ingested with provenance, **P** projected to typed graph links, **V** validated against a second independent view / real lab, **O** observed at production-shaped scale under faults. A YAML subscription profile or schema table counts at most as an intent to collect, not P/V/O. Keep a separate “unsupported / unavailable / not tested” column.

### Environment-by-environment decision table

| Environment / operator question | Existing foundation | Missing proof / high-risk semantic gap | First useful acceptance |
|---|---|---|---|
| Enterprise campus L2/L3 | gNMI/LLDP, BGP/OSPF/IS-IS/BFD, interfaces, redundant-group and path UI | Incomplete delete/sync handling; IP-keyed graph aliases; directional LLDP reconciliation and partial discovery | Two-vendor adjacency mismatch and device-readdress test; missing links reported as unknown, not absent |
| SP P router, IS-IS/OSPF/LDP/SR | SP P path profiles, BGP-LS parser and JSON bridge, IGP signals | P routers need not participate in customer MP-BGP; BGP-LS view may be partial; operational LFIB/SID/label paths not proven | Live PE-P-P-PE trace with underlay IGP adjacency + correct label/SID/next-hop, incomplete hop explicitly flagged |
| SP PE, RR, MP-BGP VPNv4/v6 | Durable BMP RIB views; RD/VRF; rich multiprotocol parsers | RT import/export service membership, per-peer/view/AFI best vs installed route, MPLS label stacks and FIB parity are not graph-validated | One VPN prefix through PE-RR-PE with same-prefix overlapping VRFs, withdrawal, RT-policy change and CE-facing FIB corroboration |
| DC EVPN/VXLAN/EVPN-MPLS | EVPN NLRI types incl. RT1/2/3/4/5; DC lab and path profiles | ESI/EVI/MAC-VRF/IP-VRF/RT/VNI/VTEP associations and multihoming state not linked into a typed service graph; EVPN BGP reachability != VXLAN forwarding | Leaf-spine-RR fabric with two tenants, type-2/3/5 routes, VTEP/tunnel mapping, host moves, ESI failover and negative tests |
| SR-MPLS/SRv6/traffic engineering | BGP-LS/SR parsers, SrPolicy schema, SP profiles | Explicit path/label/SID list and installed forwarding fate not independently validated; resource/SRLG semantics incomplete | Live SR policy path-vs-FIB evidence and SRLG failure case, distinct intent vs operational views |
| SD-WAN, cloud-managed overlay | External enrichment/adapter framework, flows, OTLP/service model | No demonstrated controller-intent topology/overlay tunnel policy and path-health adapter; opaque vendor encryption hides device-local detail | Read-only normalized controller snapshot + edge flow/health, clearly mark controller assertion vs observed underlay |
| Mixed multi-site NAT/overlap | BN-021 verified BMP mTLS/identity work; graph ownership model | Main graph and collector write paths remain IP-primary-keyed; other protocols lack equivalent trusted binding; transport NAT remains threat | Same IP reused in independent site/VRF, two exporters behind NAT and cross-source resolution; no false merges |
| Cloud/Kubernetes hosted | Helm single/ha/fleet starter chart exists | Routing split, fleet pods, stateful authority and end-to-end identity/failover proof missing | Kind/k3s staged gates, single-writer baseline first; replicas only when data/authority is safe |

This is a **verification agenda**, not a claim those scenarios currently fail end to end. The narrow source findings below establish concrete risks; real-device acceptance must decide the actual support matrix.

## 2. Code review findings, source evidence and non-duplicate disposition

| ID / severity | Current source observation | Why it matters | Owning backlog |
|---|---|---|---|
| F01 P0/P1 identity | src/graph/common.rs:51 merges Device by address; :160 merges DeviceAddress by IP, :186 maps address to device. src/collector/graph.rs:374,592 builds interface and LLDP IDs from bare device address. | Trusted exporter identity in src/telemetry_identity.rs does not by itself make all graph writers canonical; overlapping site addresses / NAT may merge distinct entities or split one device after renumbering. | BN-008/015/021; extend trusted identity through graph projection, not a second resolver |
| F02 P1 state lifecycle | src/subscriber.rs:274-332 processes notification updates, ignores delete and only logs sync response; typed_value_to_json :1017-1032 defaults unsupported forms to null. | A removed BGP neighbor/link, replaced subscription or stale value may persist as a “real” graph object. | **Already explicitly noted** in BN-003/008 source-audited refinements; no duplicate item |
| F03 P1 BGP-LS authority | crates/rbmp-core/src/bgp/bgpls.rs decodes wire structures; src/streaming/bgp_ls.rs:16-55 implements a different narrowed JSON Node/Link/SrPolicy event, :57-149 accepts TCP JSON, warns on archive failure but still publishes; graph/mod.rs:994-1030 has BgpLsNode/Link and HAS_BGPLS_NODE/HAS_BGPLS_LINK, not necessarily domain-canonical node-to-link semantics. | Multiple IGP instances, producer views, missing withdrawals and ingest recovery can yield a plausible but wrong path. RFC 9552 supersedes 7752 and requires BGP-LS Instance-ID / domain disambiguation. | BN-007/008/016 |
| F04 P1 EVPN / VPN projection | crates/rbmp-core/src/bgp/evpn.rs decodes multiple EVPN route types; graph/mod.rs:1680 Vrf includes RD and RT JSON, while topology_handler in src/http_server/observability.rs:39 onward primarily assembles Interface CONNECTED_TO + BGP/BFD/IS-IS links. | Route parsing and VRF storage are not equivalent to a queryable ESI/EVI/RT/VNI/VTEP/overlay service path. | BN-003/008/016 |
| F05 P1 topology read | src/http_server/observability.rs:79-117 derives the main physical-link list from CONNECTED_TO and MGMT_LINK. ui/src/lib/Topology.svelte:138-160 fetches topology and incidents, :212+ draws nodes by device address. | “All links” is easily mistaken for all inferred paths. A service trace must separately surface source view, coverage, missing hop and layer. | BN-008/013/016 |
| F06 P1 stale/conflicting claims | src/reconciler.rs contains host/entity matching heuristics; source-aware canonical identity is being developed separately. | A single hint (IP, hostname, remote interface alias) must not silently become authoritative multi-source equivalence. | BN-008/015/021 |
| F07 P1 ML label/identity | python/bonsai_ml/gnn/data_loader.py:338-350 falls back through id/address/hostname for node keys; :364-379 attaches chaos labels by hostname, while synthetic/live feature layouts differ. | Label and feature alignment can silently fail when identity is canonicalized or address changes; model performance is not trustworthy until checked. | BN-010 and BN-008 |
| F08 P1 ML output safety | python/bonsai_ml/inference_loop.py:240 uses one fixed score threshold; :260-278 opens POST responses but does not check status, then returns an inference result even after submission failures. | A successful local forward pass is not a durable, visible inference. Scores lack explicit calibration/feature coverage and failed HTTP must be observable/retried or marked failed. | BN-009/010 |
| F09 P1 scale/read path | src/graph/mod.rs is a large multifunction store; several writers use eager MERGE and graph-derived API queries. | Incremental projection, idempotent change replay, tenant scoping, indexes and bounded traversals must be proven with realistic cardinality before multi-site claims. | BN-007/008/016/019 |
| F10 P0 Kubernetes authority | deploy/helm/bonsai/values-fleet.yaml declares collectorReplicas: 3; templates/deployment.yaml only renders one core deployment and optional in-pod sidecar, with no collector deployment template. templates/service.yaml exposes http/grpc/metrics, no BMP listener or flow UDP service. | “fleet” values do not deploy a collector fleet or canonical bonsai-routing ingress. Chart cannot be called integrated system deployment. | **New BN-023** |
| F11 P0 Kubernetes HA | values-ha.yaml sets two StatefulSet replicas; templates/statefulset.yaml gives per-pod graph/archive volumeClaimTemplates and a shared multi-replica service. No chart-level proof of graph replication, singular projection authority or shared routing journal/failover. | An N-replica StatefulSet is not automatically a consistent HA core. Accept only a single-writer baseline until split-brain fencing/replication are demonstrated; don't expose N writers behind one service. | **New BN-023**, dependencies BN-007/008/021 |
| F12 P1 UI evidence semantics | ui/src/lib/Topology.svelte:130-135 scales link heat color using cumulative bytes_total rather than rate. Routing.svelte already supports speaker/peer/view/VRF/AFI filtering; many routes already exist. | Heat colors can encode equipment lifetime, not current utilization. Expand existing controls rather than replacing the UI; add as-of and confidence layers. | BN-013/016 |

Not raised as new work: the backlog already owns GNMI deletes, BMP journal fail-closed behavior, dropped queue reporting, provider cost/quality comparison, STGNN train/holdout/ablation, security storage extraction, NetFlow trusted identity, incident adjudication, playbook approval and release soak. Preserve their original criteria and tests.

## 3. Protocol-to-graph correctness contract

### Model planes separately

Use typed identities and edges, plus observations that support them:

- **Asset / physical:** CanonicalDevice; Interface (ifIndex plus reboot/rename scope); Port/LAG/member; LLDP-verified or single-sided tentative adjacency; site, rack and transport alias. An interface can exist without a proven physical peer.
- **Underlay:** IGP Instance (protocol, AS/area/level/MT-ID, BGP-LS Instance-ID); IGP Node and Link descriptors; reachability, TE metric/SRLG and observed SRGB/SID state. A BGP-LS node is not automatically equivalent to a discovered Device.
- **Routing control:** BGP Session and Peer view, AFI/SAFI, route, advertised/accepted/best/withdrawn, source speaker, session epoch, Add-Path and route-reflector policy context. Monitor known views, do not infer an absent view is an absent route.
- **Overlay / tenant service:** VRF and MAC-VRF; VPN Route Distinguisher vs Route Target import/export policy; EVI, ESI, Ethernet Tag/VLAN, VNI, VTEP/next-hop and encapsulation; EVPN MAC/IP/prefix mobility and multihoming. Distinguish RT (membership policy) from RD (route uniqueness).
- **Forwarding / fate:** Device-local RIB versus FIB/LFIB/label stack or tunnel programmed state, interface queue, flow sampling and forwarding tests. Path inference outputs hypothesis + completeness, not "packet proven" by BGP updates.
- **Service / owner:** Service, endpoint, application, intent, source-of-truth owner and operational change; keep asserted intent and observed state as separate edges.

A minimal observation record should include tenant/site/network-instance scope; trusted canonical identity **or explicitly unresolved** status; source/collector principal; source path/protocol; device/collector/ingest timestamps and clock uncertainty; session epoch/sequence/cursor/view; parser + schema version; upsert/delete/sync/gap event; proof origin; quality, freshness/TTL and retention; typed object key. A graph edge carries evidence IDs and life interval [valid_from, valid_to), not merely a last-updated timestamp. Inferences are distinct edges with validation class and contradictory evidence.

### Cross-view link algorithm

1. Parse and preserve original peer/AFI/SAFI/route-view and per-producer identifiers. Validate identities before mutating authoritative nodes.
2. Project **source-specific observed objects** and their tombstones into replayable materialized state. Store independent views rather than taking latest writer wins.
3. Resolve candidate equivalences by explicit canonical bindings (strong), normalized multi-attribute keys (conditional), or weak heuristics (tentative). Refuse ambiguous many-to-one merges.
4. Reconcile physical L2, underlay, L3 control, VPN overlay and FIB into separate typed links; reflect contradictory sources and missing coverage.
5. Query a bounded temporal snapshot with a defined as-of watermark; return supporting evidence and missing observations together.
6. Verify with controlled failures: interface removal, BGP-LS withdrawal, peer restart/EOR, EVPN mass withdrawal/MAC mobility, conflicting RD/RT/VNI, route-reflector-only visibility, wrong clock, collector failover and device renumbering.

No autonomous operation, LLM-generated remediation or AI readiness score should use an inferred path without scope, coverage and source lineage.

## 4. Kubernetes architecture, not merely a Helm deployment

The source already has deploy/helm/bonsai chart, single/ha/fleet values, PVCs, probes, secret and optional sidecar. **Do not create a second chart.** Its modes are scaffolds until the following gates pass.

**Recommended staged shape:**

- **K0 functional single writer:** separate bonsai core/application and bonsai-routing into documented Pods/services with isolated persistent data and compatible API token/auth, ingress ports for BMP and bounded flow UDP. Pin images/digests, constrain requests/limits, readiness, liveness and termination behavior. Run read-only collection first.
- **K1 collector-fleet:** collectors as separate site/zone-scoped deployments with durable queues, assignments and mTLS identities, managed egress to devices and core. Disambiguate source across NAT/load balancers. Do not use “one DaemonSet per node” without an explicit need; choose deployment versus DaemonSet based on device reachability/placement.
- **K2 available, not falsely HA:** fenced leader / single writable graph and RIB journal, separate stateless web/read layer as appropriate, storage/backup consistency and explicit RTO/RPO. Multi-replica ingestion must not fork the durable speaker epoch or graph truth. Only advertise HA after failover and replay parity.
- **K3 security and upgrades:** NetworkPolicies for management/ingest/operator planes; least-privileged ServiceAccounts, non-root/rootless containers, secrets/cert rotation, topology-scoped service discovery, disruption budgets, rolling upgrade rehearsal, loss/latency/backpressure SLO and PVC recovery.

Kubernetes StatefulSet gives persistent identities/volumes, **not** application-level data replication. Source-IP fidelity through LoadBalancer/NodePort/CNI/NAT is deployment-dependent and must be tested with authenticated exporter identities. No generic HTTP Ingress for raw BMP TCP or flow UDP is assumed. These gates require live kind/k3s (or equivalent) rollout plus kill/restart/rebalance, no duplicate authoritative telemetry, no unaccounted queue drops and usable debug/rollback instructions.

## 5. Trusted operator and LLMOps — internal or external agents

Google Cloud's Autonomous Network Operations whitepaper is a useful **reference architecture**, emphasizing observability, topology, analytics/digital twins and governed actions; it is not Bonsai's current acceptance evidence. A generic external agent such as NetClaw is an **untrusted caller** unless authentication, scope, data retention and policy are verified. Reuse the current MCP, API, provider, investigation and execution gates; do not build a second agent brain or expose raw graph stores.

**Northbound deterministic evidence tools to extend existing MCP/REST:** scoped get_device/get_vrf/get_service/get_routing_view; get_path with explicit physical/underlay/overlay/FIB layers; get_observation_coverage and last-success/gap; compare_snapshots(as_of_a, as_of_b); explain_route(prefix, AFI/SAFI, VRF, view, peer, time); get_change_assurance(invariant verdicts and independently captured evidence); propose_read_only_checks; retrieve_adjudication. Each response returns schema version, authorized scope, timestamp, identity status, provenance/evidence references, counterevidence, missing/unsupported sources, limit/truncation and no-claim state. Enforce authorization before tools, not after the LLM composes a prompt.

**Trust levels:**

- **T0 Discovery:** read-only, cite evidence and explicit uncertainty; tool limits and prompt-injection-resistant handling of telemetry/config strings.
- **T1 Investigation:** hypothesis/alternative/contradiction scoring, replayable trace and operator adjudication; independently measured utility, false positives and costs (BN-004/013).
- **T2 Change assurance:** deterministic plan-specific invariants, signed scope/approval, candidate diff, maintenance windows, dry-run and rollback rehearsal (BN-001/002/014).
- **T3 Autonomous writes:** **out of current authorized scope**. No provider or agent receives device-write authority until explicit future authorization, safety/approval, multi-party execution controls and live fail-closed proof.

Benchmark misleading but plausible situations: no BMP coverage but “prefix missing”; old route after restart; overlapping tenant IPs; conflicting BGP-LS producers; an EVPN control plane with a broken VTEP; a model claiming causality from attention weights; adversarial log strings requesting a secret. Correct answer can be “insufficient evidence”. Record unanswered rate, operator time saved, false diagnosis, false confidence and denied writes, not just token cost.

## 6. ML/analytics: practical scenarios before a new model family

Prefer deterministic topology/counter/intent rules as the **challenger every model must beat**. Existing STGNN, PCA/embeddings, clustering, forecasting, rules and job engine form a base; they are not demonstrated field-ready ML.

| Scenario | Feature/ground truth design | Deterministic first pass | ML value if independently better | Mandatory false-positive controls |
|---|---|---|---|---|
| **Unused / stranded links** | LLDP neighbor, role, admin/oper, interface speed, counter deltas over 7/30/90 days, reset/wrap flags, LAG/ECMP utilization, flows, change tickets/intent | Classify never-used vs idle-by-design vs standby/protection vs telemetry-unavailable vs suspect-underutilized, with time window and confidence | Cluster stable peer traffic shapes, rank avoidable CAPEX or asymmetric capacity | Protect HA spare paths, interconnects, backup schedules, bursty/seasonal links, maintenance and incomplete device coverage |
| **QoS policy/experience violation** | Class-map/policy attachment, ingress/egress direction, DSCP/classification/remarking, per-class offered/forwarded/dropped bytes/packets, occupancy, ECN/WRED, bursts and service latency; known counter widths | Detect configured-vs-applied mismatch, drop/latency threshold exceedance and priority inversion with class-specific rates | Rank unusual queues, forecast congestion and correlate observed app degradation | Do not infer queue-level facts from aggregate port counters/NetFlow alone; distinguish congestion from policing and missing counters |
| **Previously undetected path drift / blackhole** | MP-BGP route change + route view, next-hop recursion, RT membership, underlay/BGP-LS changes, FIB/LFIB probe when available | Cross-view contradiction rules: advertised but not imported, best but not installed, reachable PE but missing label/tunnel | Rank recurring cross-domain patterns or blast radius, never assert physical path from RIB alone | Separate missing source from no path; use paired ground-truth probes and counterevidence |
| **Weak redundancy / single point of failure** | LLDP/IGP path, SRLG, BFD, ESI multihoming, BGP add-path/ECMP, policy, business service owners | Graph cut/SRLG diversity with coverage and expected protection level | Prioritize change impact and outlier resilience behavior | Intentional single-homing, same failure domain, incomplete remote topology |
| **Microburst / QoS forecasting** | High-resolution device queue drops/occupancy, buffer and latency with sampling metadata, maintenance history | Rolling quantiles, change-point and seasonal baselines | Forecast only if held-out precision, lead time and resource cost beat baseline | 5-minute snapshots cannot prove sub-second microbursts; telemetry collection capability is a prerequisite |

**Required repair/evidence:** (i) train/test split by incident episode, time, topology and NOS; no leakage from chaos injection metadata or train-fitted encoders; (ii) strict canonical entity IDs, masks for missing types and temporal windows, and invariant label matching; (iii) compare with rule/statistical/tabular baselines and no-graph/no-time ablations; (iv) calibrate thresholds by incident cost and alert volume, with explicit calibration drift; (v) **check HTTP status, idempotency and persistence of published model results** in the inference loop, report stale model/score and missing features; (vi) link prediction to operator feedback and ground truth, not automatic remediation. Measure episodes/week, false alerts per device-day, time-to-detect, precision/recall, missed high-impact incidents, explanation correctness and actual avoided troubleshooting effort. No unsupported “production AUC” claim from synthetic chaos data.

## 7. UI and operator workflow audit

Existing strengths: Live three-panel layout, role-aware login, Sites/Devices, Routing & RIB views, filters, graph Explorer, Incidents/Investigations, Approvals/Audit, Collectors/Sidecars, Integrations with test/audit endpoints, and initial accessibility/Playwright gates. **Iterate, do not rewrite.**

Prioritize one **investigate a service or prefix** journey:
1. Search by site, tenant/VRF, endpoint, service/prefix, canonical device or alias. Indicate which criteria were resolved and whether the principal has sufficient scope.
2. Choose physical / underlay / BGP-control / EVPN-VPN / FIB / service overlay layers, with clear legend. Show unresolved/disputed nodes and a numerical coverage denominator per selected layer.
3. Show an **as-of** timestamp and timeline diff with all route-view and collector-gap context; support compare before/after maintenance and export a bounded evidence bundle.
4. On a path or node, show the exact observations, original source/vendor, collection lag, parser and link-confidence reason; a “why is this link missing?” diagnostic differentiates unsupported source, lack of subscription, partial peer view, stale sync and genuine absence.
5. Tie incident → investigation → evidence → human adjudication → controlled next read-only check → assurance/invariant result. Add operator feedback that records actual correctness and missing signal, not just thumbs up.
6. Health colors and “utilization” require deltas over an explicit rate window, capacity, sample coverage and counter-reset handling. Cumulative octets are not current utilization.
7. Extend existing Playwright/a11y tests for large topology with >1k visible entities / lazy query and pagination, keyboard/table alternatives for SVG, narrow viewports, role-specific 403/unknown states, missing/stale data, errors, safe exports and accessible alerts.

Do not surface an AI-generated “complete topology” badge unless the per-plane coverage proof is available. Prefer progress by verified scenarios to attractive but misleading dashboards.

## 8. Sequencing and gates

**Phase A: correctness (P0/P1):** complete BN-021 and BN-022 security/identity boundaries without disrupting NetFlow work; BN-008 strict provenance / delete/sync / graph reconciliation; BN-007 BGP-LS projection authority and replay; BN-023 Kubernetes single-writer packaging blocker. A no-data or uncertain answer must remain safe.

**Phase B: environment slices (P1):** one PE-P-PE L3VPN service trace; one EVPN/VXLAN leaf/RR fabric; one SD-WAN controller adapter. Each gets a capture provenance manifest, AFI/SAFI support matrix, topology and normalized graph snapshots, controlled faults and independent oracle. Add support only for verified NOS/version combinations.

**Phase C: operator usefulness (P1/P2):** graph as-of/diff and coverage APIs, grounded MCP/REST tool schemas, UI service-path workflow, unused-link and class-aware QoS deterministic rules, ML offline reproducible benchmarks and read-only shadow pilot. Promote models only if they beat baselines with measured operator impact.

**Phase D: availability/scale:** staged K8s collector fleet, durable queue overload/replay, leader fencing and recovery, measured scale, security/upgrade evidence. Do not promote “HA”, “vendor agnostic”, “autonomous” or “QoS/microburst detected” labels without their respective acceptance.

### Decision gates and owners

- **Gate 1** capability table: facts D/I/P/V/O by NOS/version, physical role, routing instance and plane, signed off in BN-016/019 support matrix.
- **Gate 2** identity and lifecycle: two overlapped tenant IPs, NAT exporters, alias rotation, duplicate BGP-LS producer, withdraw+replay, zero spurious merges, no silent authoritative writes.
- **Gate 3** two service traces: PE-P-PE and EVPN-VXLAN, route+graph+installed forwarding evidence with explicit unsupported facets.
- **Gate 4** K8s: deployed core + routing + two collectors, proper source identity, pod kill/failover and storage recovery, then only optionally HA.
- **Gate 5** trusted operations: an external agent using only curated read tools obtains the same validated, scope-limited answer as the UI; failure injections produce “unknown” and no unauthorized write.
- **Gate 6** ML utility: unused link and QoS cases have adjudicated ground truth and beat deterministic alternatives on chronological / topology / NOS holdouts.

## 9. References and verification posture

- Google Cloud, *Autonomous Network Operations* (whitepaper): https://services.google.com/fh/files/misc/autonomous_network_operations.pdf — architectural inspiration, not an operational certification.
- IETF RFC 9552, *Distribution of Link-State and Traffic Engineering Information Using BGP*: https://www.rfc-editor.org/rfc/rfc9552.html — BGP-LS instance/descriptor and partial-view semantics; supersedes RFC 7752.
- IETF RFC 7432, *BGP MPLS-Based Ethernet VPN*: https://www.rfc-editor.org/rfc/rfc7432.html — EVPN route/multihoming semantics.
- IETF RFC 8365, *EVPN Network Virtualization Overlay*: https://www.rfc-editor.org/rfc/rfc8365.html — VXLAN/VNI/RT overlay relationships.
- IETF RFC 4364, *BGP/MPLS IP VPNs*: https://www.rfc-editor.org/rfc/rfc4364.html — RD/RT service separation.
- Kubernetes workload/stateful semantics: https://kubernetes.io/docs/concepts/workloads/ and https://kubernetes.io/docs/concepts/workloads/controllers/statefulset/ — persistent Pod identity != data replication.
- NetClaw (candidate generic external agent): https://netclaw.dev/ — verify supported transports, security, model and MCP integration in an actual read-only adapter spike; no claims of built-in Bonsai compatibility.
- Bonsai existing research and benchmark taxonomy: BONSAI_AGENTIC_NETOPS_STRATEGY_2026.md, docs/architecture/GNN_ARCHITECTURE_EV1.md, docs/merger/AGENTIC_NETOPS_RELEASE_REPORT.md.

**Review type:** code/static-contract analysis plus public standards; not a deployment/packet capture/security exploit, code test, QoS benchmark or model quality evaluation. Source line numbers refer to the 11 November main revision above. Do not mark any new acceptance complete from this document.
