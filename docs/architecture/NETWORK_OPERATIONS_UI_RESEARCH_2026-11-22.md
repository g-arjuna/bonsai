# Evidence-first network operations UI: SkyCMD inspiration and cross-domain design research

**Research date:** 22 November 2026. **Status:** design/implementation intake only; not a shipped feature, live browser qualification, production KPI, or claim of access to SkyCMD's application. **Canonical delivery:** BN-030 in BONSAI_BACKLOG.md. Complements BN-013/015/019/020/024/025/026/027/028/029 and the 19 November UI-first configuration review. Never create a second network-state authority, duplicate topology engine, or new mandatory time-series database.

## 1. What can actually be verified: source and media ledger

The user referenced a SkyCMD LinkedIn/YouTube demonstration with Upscale personnel Varun Vij and Ravi Vaidyanathan discussing PFC/pause frames. We searched company announcements, indexed LinkedIn posts, YouTube and Tech Field Day descriptions (22 Nov 2026). **The exact publicly playable SkyCMD demonstration and its individual GUI frames could not be retrieved or authenticated in this research environment. NO video frames were captured, sampled, or watched through a GUI.** Do not present screenshots of Upscale's **SkyD** design tool as SkyCMD operations-console evidence. This distinction is a release gate for future UI-competitive claims.

| Material | Source and verifiable substance | Confidence and limitation |
|---|---|---|
| Upscale SkyCMD | Upscale Token Fabric announcement (8 Oct 2026), https://upscale.com/ and archived full release https://ebs.publicnow.com/view/B0EA858119B0917F031EF7B4D68C39A195D60C95 . SkyCMD is described as an agent-native **orchestration and observability** plane across scale-up and scale-out elements; public language is a product claim, not a validated screenshot or measured capability. | High confidence in vendor's *positioning*; no verified GUI/control-specific detail. |
| Upscale SkyD | https://upscale.com/uvd and Tech Field Day 5 Nov 2026 https://techfieldday.com/video/purpose-built-for-ai-scale-with-upscale/ . This is a **fabric architecture/design** workflow (layouts, validated network plans, cabling/BOM), **not** SkyCMD incident response. | High confidence in separation; useful inspiration for rack/row/pod navigation and planned-vs-observed topology only. |
| AI network theory / pause framing | Networking Field Day 40, 9 Apr 2026 https://techfieldday.com/video/upscale-ais-point-of-view-with-aravind-srikumar/ and https://techfieldday.com/video/upscale-ai-networking-what-has-changed-with-ai/ . PFC/LLR and tightly synchronized AI workloads appear in talk descriptions, not proof of SkyCMD displaying a particular diagnostic pane. | High confidence in public talk descriptions, not an observed product feature. |
| Person attribution | Public indexed Upscale-associated LinkedIn reference includes Varun Vij and Ravi Vaidyanathan: https://www.linkedin.com/in/akshara-kante-bb46b0359 . | Supports association, not attribution of any particular screen or demo. |
| Kentik AI Advisor / network AI | https://www.kentik.com/blog/introducing-kentik-ai-advisor/ and https://www.kentik.com/blog/ai-diagnostics-in-kentik-nms/ . Contextual questions; network/problem-scoped tool investigation; configuration vs observed differences and verifiable read-only explanation. | Public product descriptions and images; validate any implementation details in live product before claiming parity. |
| ThousandEyes Path Visualization | https://docs.thousandeyes.com/product-documentation/internet-and-wan-monitoring/viewing-data/network/path-visualization and https://www.thousandeyes.com/product/internet-and-wan-monitoring/path-visualization . Time-scoped path, loss/latency, hop drilldown, multiple perspectives. | Official documentation; measured path is not proof of arbitrary data-plane forwarding or loss at every intermediate ICMP hop. |
| ThousandEyes traffic/event context | https://docs.thousandeyes.com/product-documentation/internet-and-wan-monitoring/traffic-insights and https://www.thousandeyes.com/product/assurance/ai . Cross-filtering around event and vantage context. | Official documentation; user-facing specific controls subject to product revision. |
| Cisco congestion precedent | Public Nexus Dashboard congestion panel image showing trend and PFC/ECN/queue affordances: https://www.cisco.com/ . Referenced during visual search; this document does not redistribute or imply endorsement. | Illustrative screenshot from a separate vendor; not SkyCMD. |
| Selector/Aviz supplemental | https://www.selector.ai/ and Tech Field Day Aviz demos https://techfieldday.com/ . Relevant ideas: incident-focused evidence gathering, prioritization, device/queue drilldown, and assistant interaction. | Selective public material; no verified feature-equivalence comparison. |

**Media / screenshot completion record (22 Nov):** SkyCMD exact Varun/Ravi demo: **NOT FOUND / NOT CAPTURED**; SkyD public presentation: **DISCOVERED / DESCRIBED / NOT CAPTURED FRAME-BY-FRAME**; Kentik/ThousandEyes/Selector: public product visuals and docs inspected as **separate illustrative references**. Do not turn this ledger into invented “screens observed at 01:20” annotations.

### When the exact SkyCMD media becomes accessible: reproducible frame audit

Capture **8–12 frames at meaningful interaction boundaries**, not random intervals: (1) landing/navigation and hierarchy; (2) selection of site/pod/fabric; (3) global time/tenant/scope state; (4) highlighted congested leaf/link; (5) queue/PFC counters with priority and TX/RX direction; (6) chart timeline and brush; (7) drilldown to a precise port/queue; (8) correlated incident/evidence pane; (9) diagnostic question/answer; (10) action/approval gate; (11) expanded/collapsed panel; (12) errors/missing observations. Record source URL, presenter, publication date, timecode, full-size frame width/height, visible control labels, clicks/transitions, hover contents, semantic colors, actual vs inferred evidence, and whether the element is SkyCMD vs SkyD. Use permission-appropriate internally stored captures and metadata; avoid committing copyrighted vendor frames or relying on marketing mockups as runtime proof. Before writing a “borrowed from video” claim, require two verified adjacent frames or transcript/context showing interaction and behavior.

## 2. Competitive interaction findings: what to adopt, not clone

| Pattern / source | Operator value | Bonsai interpretation |
|---|---|---|
| SkyCMD stated agent-native unified operations | A network-wide problem viewed as one investigation rather than device-by-device CLI. | One durable investigation context: scope + as-of time + affected service/flow + deterministic graph + evidence claims + safe next check. Claim is an **inspiration**, not a screen observation. |
| SkyD rack/row/pod design hierarchy | Large AI topology becomes navigable through physical organization and planned relationships. | Optional AI fabric rack/pod/rail views alongside observed L2/L3; physical plan is intended evidence, not real port connectivity or forwarding proof. |
| Kentik contextual AI Advisor | Ask a question with investigation scope and see tool/evidence steps and differences between configured and observed behavior. | Evidence-first copilot **adjacent** to a selected incident/link/path with explicit observations, competing explanations, negative evidence, unavailable checks and read-only tool receipts. |
| ThousandEyes time-synchronized path and vantage | Selecting an event time immediately changes path, loss/latency, and nodes being compared. | Global time anchor and shared selection across topology, charts, flows, route state, device drawer and incident timeline; label probe viewpoint and observed vs predicted hops. |
| Cisco / Aviz queue-focused congestion | PFC/ECN/queue data presented together so a queue problem is visible as a transport story, not isolated counters. | Typed congestion mini-workbench by interface, priority, direction, clock, queue and source; trend and cause-vs-symptom split, clickable evidence to packet/port. |
| Selector investigative heatmap | Prioritize hotspots without inspecting every device. | Metric-specific matrix/heatmap by pod/rail/interface/priority or site/AP/SSID, gated by actual metric coverage. Never paint missing as zero. |

**Counterpatterns to reject:** (i) a giant permanently moving force-directed graph; (ii) every device or telemetry sample as a graph node; (iii) flat 27-item admin sidebar as the primary incident journey; (iv) generic “health 99%” absent denominators; (v) inference presented as observed packets; (vi) LLM free-text as an unverified root cause; (vii) silent stale data after reconnect; (viii) auto-generating device commands or an action button implying device writes are enabled.

## 3. Existing Bonsai UI: source-level review (main, 22 Nov 2026)

Reviewed actual tracked source, rather than historical sprint checkboxes:

| Source | Verified UI behavior / gap |
|---|---|
| ui/package.json | Svelte 5, D3, Vite, lucide-svelte: keep these and avoid new framework/rewrite. |
| ui/src/App.svelte | Approx. **27 flat navigation destinations**; route-level lazy imports exist. Split primary diagnostic workspaces from administration without removing power-user destinations. |
| ui/src/routes/Live.svelte | Three-column **140 px site rail / flexible D3 topology / 320 px event feed**, top status bar, a device drawer. A useful nucleus; fixed sizing, no shared incident time/flow context or task-driven pane. |
| ui/src/lib/Topology.svelte | Role-tier categorization across DC, SP, WAN, campus/wireless; L2/BGP/BFD/ISIS layers and trace highlights, D3 simulation. Important existing invariant: BGP peer endpoints are backend-resolved, not inferred by browser. Force layout and all-device rendering require bounded, grouped multiscale behavior. |
| ui/src/lib/DeviceDrawer.svelte | Fourteen tabs: interfaces, peers, paths, schema, recommendations, events, detections, readiness, config, enrichment, CMDB, telemetry, flows, audit. Preserve advanced material, but show task-specific cards first and make provenance/time visible. |
| ui/src/routes/Investigations.svelte | Investigation list, manual trigger via detection/device, tool-call history, feedback, accuracy endpoint; no coordinated topology/time selection in reviewed source. |
| ui/src/routes/Routing.svelte | Routing health/speakers/peers/routes/coverage, filterable RIB view, prefix-history and missing-prefix investigation; 15-second polling. Rich independent workspace but requires correlated navigation. |
| ui/src/lib/CommandPalette.svelte vs ui/src/App.svelte | **Specific real mismatch:** keyboard shortcuts 6–9 map to Enrichment/Adapters/Approvals/Explorer in palette; sidebar shows Integrations/Approvals/Explorer/Investigations. Fix via one canonical shared workspace map and browser test before expanding navigation. |
| ui/src/lib/design/colors.js | Existing dark tokens: #09090c background, #13151a surface, #1c1f27 elevated, teal #5eead4, green #34d399, amber #fbbf24, red #f87171, blue #60a5fa, gray #9ca3af. Rationalize semantic roles instead of replacing for aesthetic novelty. |
| docs/ui_audit_2026-05-04.md | **Historical** warnings on fetch-once registry mutations and SSE; re-test current Topology/Devices/Approvals and source reconnect before treating prior findings as active defects. |
| docs/architecture/UI_FIRST_CONFIGURATION_FLEET_REVIEW_2026-11-19.md | Strong existing desired/applied/effective config and restart truth contract; BN-030 must re-use it, not create a second settings backend. |
| docs/STATUS.md | M6D/E/F real Nokia YANG/gNMI lifecycle, observed vs approved telemetry, no authorized operational device writes. UX must preserve these truth states and avoid implying support beyond tested NOS/OS versions. |

**Conclusion:** no visual rewrite now. Develop a **shared investigation context and evidence composition layer** and incrementally make current Live, Routing, DeviceDrawer, Investigations and existing metric/TSDB read adapter behave like one product. Fix specific existing interaction debt early.

## 4. Proposed Bonsai interaction model

**Operator objective:** in one minute answer **what is impacted, where, when, by which observation, with what confidence, and what safe check next**.

### Shell and layout

~~~text
┌ Bonsai · [Environment ▼] [Site / Pod / Service ▾] [As-of LIVE ▾] [Time range ▾] [Search ⌘K] [Coverage ◉] ┐
│ Primary: Overview | Investigate | Explore topology | History | Compare  ·  Admin [+]                    │
├ Scope rail ─────────┬ Linked workbench: map / path / heatmap / rack / client journey ──────┬ Evidence rail ──┤
│ Global / region     │   * highlighted affected nodes/edges, grouped at scale              │ Observed        │
│ Site / fabric / pod │   * selected interface/queue or service path                         │ Hypotheses      │
│ Tenant / VRF / SSID │   * safe zoom, pin, layer, legend and “unknown” rendering             │ Counterevidence │
│ Impact rank         │   * compare T-1 / T0 / now, selected vantage                        │ Gaps / freshness│
├─────────────────────┴──────────────────────────────────────────────────────────┴────────────────┤
│ Correlated timeline: rates | errors | RTT | drops | queue | pause | incident | config | BGP | sampling gaps │
├ Selected entity: identity + scope + last observed + source + version + tabs for detailed config/telemetry ┤
│ [Open underlying evidence] [Compare paths] [Explain] [Export sanitized case] [Request safe read-only check] │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
~~~

This is a **design proposal**, not a claim that SkyCMD uses these buttons. It deliberately preserves Svelte routes, existing topology/device drawer, one database authority, graph facts, and API security boundaries.

- **Global immutable investigation context (URL/deep-link + state):** tenant and authorized scope; incident ID; selected entity IDs; domain (AI fabric/SP/campus); time anchor, comparison window, source watermarks; metric selection; layer/vantage; source/model version; query/permission budgets. Shareable case must serialize bounded handles, not secret-bearing raw query or sensitive context.
- **Three primary motions:** (1) triage **impact overview**, (2) focus **topology and time**, (3) validate **evidence and next check**. Expert tools available from context cards and command palette, not forced through 14-tabs manually.
- **Topology choice by question:** fabric pod/rail/leaf/spine/rack; SP service/prefix/PE/VRF/LSP/SR-policy/IGP adjacency; campus site/building/WLC/AP/client experience/PoE access; cross-domain service/flow overlay. Physical vs logical vs discovered vs intended are distinct edge types. No “current forwarding” unless supported by real forwarding or active-probe evidence.
- **Device detail redesign:** sticky identity (hostname, role, site, tenant, management reachability, collector/vantage, platform, supported metric families), compact active incident and impact cards; contextual interface/queue/flow/route evidence at the top; advanced schema, config, history, events, audit in grouped expandable details. Retain all 14 functional tabs until fully migrated and covered by browser tests.
- **Search and buttons:** nav ≤5 top-level operational categories, separate admin drawer/section, keyboard command palette generated from same route registry; click and keyboard focus equivalent; chart brushing updates path, selected port and evidence rail; back/forward restores context; safe manual refresh indicates exact observation time, timeout and gap.
- **Colors and motion:** dark baseline retained; red only confirmed impact/failure with verifiable scope, amber degraded, blue informational, teal selected, gray unavailable/unknown, striped/dotted missing data, distinct outlined predicted/declared values; never only color-coded. Animate only changes/events with timestamps, reduced-motion preference, no pulsing whole topology or heavy particle links.
- **Responsiveness:** resizable keyboard-accessible panes, 1440+ desktop 3-panel, 1024 tablet 2-pane, narrow display stacked view with explicit active scope and evidence. Charts and map stable under real-time updates and pausing. Hover is never sole means of seeing important data.

## 5. Typed diagnostics: three different domain stories, one visual grammar

### 5.1 AI fabric / RoCE congestion: “PFC storm on leaf A port Ethernet1/12”

**Facets:** site/pod, rail, logical fabric, sender/receiver, NIC, leaf, fabric link, exact ingress/egress direction, priority (IEEE 802.1p), queue, ECN profile, affected workload/job if *authorized* attribution exists.

**Observed evidence cards:** PFC TX/RX pause frames and pause duration per priority, PFC watchdog/storm indication, ingress/egress queue occupancy and watermark, buffer drops, ECN marks, CNP/receiver congestion feedback, output tail/WRED drops, utilization/microbursts, speed/optics/FEC, link and switch time consistency, collector rate/reset/cadence. Show different measurement sources and hardware/NOS support explicitly. PFC receive on A is not interchangeable with PFC transmit on B; frames vs paused time are different units. Pause can be symptom of downstream congestion, not proof of the original bottleneck. A port with PFC counters has not automatically demonstrated packet loss or application GPU stall.

**Visual story:** fabric rail/pod heatmap highlights which queue/priority on which leaf; selected edge exposes mirrored PFC TX/RX and queue history; global timeline aligns ECN/queue/PFC/throughput and workload degradation; evidence rail ranks “hot downstream queue”, “ECN control lag” and “link/optics issue” against contradictory normal evidence. Missing ECN/CNP sensor → “not measured”, never 0.

### 5.2 Service-provider/WAN/TCP: “retransmissions, window stalls and random drops”

**Facets:** service, subscriber/enterprise VRF, prefix, source/destination/workload, egress PE, interface/subinterface, tunnel/MPLS/SRv6/SR policy or service path, viewpoint (endpoint, host agent, packet capture, synthetic probe, flow telemetry, router), before/after routing change.

**Observed evidence:** per-observed-flow retransmission/duplicate ACK/SACK/DSACK/RACK if instrumented and lawful; RTT, RTT variance, reordering, observed receive-window advertisement and zero-window events, congestion window only if endpoint instrumentation exists, connection state/timeouts, PMTU/MSS, ECN, packets/bytes/rate, device queue drops, interface CRC/FEC/optics, policer offered/conform/exceed/drop (where exported), QoS shaping and capacity, BGP/IGP/LDP/RSVP/SR/LSP fault/change evidence. NetFlow/IPFIX *alone* does not directly expose TCP re-transmission cause or host cwnd/advertised rwnd unless extensions/packet-level/endpoint sources supply it. RTT from a probe is vantage-specific; mid-hop ICMP loss may reflect rate-limited TTL exceeded rather than transit forwarding drops.

**Visual story:** one path panel plus per-hop/segment lane, *distinct control-plane, predicted candidate path and verified probe path overlays*, time-brushed RTT/retrans/zero-window/policer drops. Show both app/receiver bottleneck and network causes, e.g. “receiver zero window despite low link drops” vs “microburst policing with normal receiver window”. Do not claim inferred MPLS data-plane route from a BGP RIB and LLDP link alone.

### 5.3 Campus/wireless: “a client has slow TCP and repeated onboarding”

**Facets:** site/building/floor (trusted layout only), AP/radio/band/channel/SSID/BSSID, client session via pseudonymous bounded key, switchport/PoE, VLAN, WLC path, RADIUS/DHCP/DNS upstream dependencies, WAN egress.

**Observed evidence:** RF retries vs **TCP** retransmissions distinguished, RSSI/SNR where available, MCS/NSS/airtime utilization/channel contention, association/auth/RADIUS/DHCP stage latencies and failures, roam and PMK/session changes, 802.1X/MAB policies, AP PoE allocated vs measured budget/mode degradation, switch interface errors/discards/queue, WLC/control tunnel reachability and client traffic vantage. No invented physical client location from RSSI; redact person/client identifiers by default.

**Visual story:** onboarding funnel with where failure occurs, AP/radio heatmap + floor-plan if georeferenced, client session timeline, selected AP uplink/wired path, RADIUS/DHCP evidence. Separate “Wi-Fi retry at air interface”, “AP uplink drop”, “WAN TCP retransmission” and “receiver window stall”; same surface grammar but different domain semantics.

### Evidence row contract (one for all domains)

Each visible evidence card includes: stable subject and identity scope; metric type/unit, direction/queue/priority/source; measured / declared / inferred / derived; observed and received times, window, sample rate and reset semantics; source/collector/vantage/software version; coverage/expected cadence/data gaps/staleness; confidence limited to method and source, not fabricated probability; scoped evidence handle, click-through, redaction and RBAC. Aggregation must specify denominator/window/weighting and never sum incompatible measurements or unrelated duplicate collectors. Correlation and causality are explicitly separate.

## 6. AI investigator: useful without hallucinated diagnoses or accidental execution

The assistant panel is an **evidence navigator**, not the sole truth authority. Proposed card order:

1. Operator question, exact tenant/scope/time/vantage, incident context.
2. **Observed facts** (linked evidence refs; timestamp; source; support status).
3. **Hypotheses** ranked and tagged *supported / contradicted / unresolved* by deterministic rule evidence.
4. **Counterevidence** and common confounders (clock skew, missing sensors, collector partition, route asymmetry, unrelated ICMP loss, client-side window backpressure).
5. **Unknowns and checks** (data unavailable vs denied vs not supported vs stale vs zero), and operator-safe read-only verification requests only through BN-028 policy/runner approvals.
6. Plain language narrative with explicit citations and “cannot determine” when evidence is inadequate.
7. Operator adjudication/rejected hypotheses, sanitized case export with watermark and replayable query/cursor versions.

The graph is an evidence index, not forwarding proof; the AI cannot bypass tenant/source field authorization, leak raw identities in model prompts, invent a route or activate gNMI/diagnostic actions by chat. Operational writes remain disabled as per current project contract.

## 7. Live data and performance: make the UI truthful rather than simply busy

- Query ownership: one Core authenticated typed API feeding UI and agent tooling, backed by bounded latest/summary history + optional approved TSDB adapter. **No browser-to-TSDB arbitrary PromQL, arbitrary SQL, raw graph, or untrusted external URL**. Reuse BN-024/026 metric history and existing BN-015 TSDB proxy; do not roll a parallel ingestion engine.
- Incremental updates: event cursor plus event-time window and monotonic scope generation; SSE/WS bounded replay with reconnect and gap detection; snapshot refresh only on invalidation/selection changes; client-side request cancellation, stale-response fencing, backoff. Never refresh entire network graph on each sample. Differentiate ingestion heartbeat, device clock and Core receive time.
- Query/UI budget contracts: paginate and virtualize list > visible items, cluster topology and drill by scope, progressive metric resolution/decimation preserving spikes and interval boundaries, bounded chart points, memory ceilings, accessibility tests; target **measured** 1k/10k visible inventory scenarios rather than falsely promising 100k device canvas.
- Event-time semantics: rates need counter reset/delta/rollover guards, missing intervals hatched, time skew and latency visualized, expired telemetry loses “LIVE” status, empty/unsupported/forbidden distinct.
- Browser perf acceptance goals (initial design targets, must benchmark and revise): p95 input-to-highlight <100 ms for local highlighted nodes; 60 fps while panning visible ≤500 entities on target laptop; p95 API first useful bounded result <1 s in lab; time-scrub to updated linked charts <500 ms with warm cache; reconnect gap visible <5 s after detection. Instrument rather than assuming a claim.
- Support OSS/offline/self-hosted first, optional TSDB connected by policy, zero mandatory browser SaaS/LLM/CDN. High-contrast, reduced motion, keyboard topology traversal, screen-reader summary for heatmaps and tool tips.

## 8. Implementation sequence and explicit acceptance scenarios

**P0 short slice (existing behavior):** canonical unified nav/shortcut registry; verify and fix Topology/Devices registry SSE refresh if stale on current source, disconnected/reconnect truth; smoke the Live 3-column pane and DeviceDrawer 14-tab click paths; identify 401/403/timeout/fallback states. Produce before/after Playwright and performance traces.

**P1 vertical slice (focusing on a real incident):** shared URL/tenant/time selection and evidence card; one **AI-fabric PFC queue** journey wired from real typed payload to heatmap → queue + port trend → hypotheses and counterevidence; real port/priority/direction/gap proof with test source. Adopt one context-aware entity drawer and linked time scrub without rebuilding all routes. Then service-provider retransmission/window/policer and campus AP/RADIUS/PoE journeys with independent proof.

**P2 polish / advanced surface:** compare incident T-1/T0/now, saved sanitized operator playbook/case links, optional layout overlay/physical plan, controlled replay, per-role adaptive workspace and layout preferences.

Minimum independent acceptance dataset (synthetic tests prove UI mechanics only):
1. PFC TX storm on a congested egress priority with matching downstream queue rise; conflicting reverse-direction counters do **not** produce a false root cause.
2. ECN marks present, no observable CNP; render “not measured,” not zero or failed.
3. 10G measured offered load against 1G effective policer: compare declared/observed config and rate evidence; do not label every TCP retrans as policing.
4. Receiver zero-window/slow consumer while network interface drops remain low; rank receiver as candidate instead of blaming a WAN hop.
5. TCP retransmission spike with one-sided pcap and asymmetric path, no endpoint cwnd; show uncertainty and alternate checks.
6. Mid-hop ICMP TTL-exceeded drop with healthy destination probe; do not paint forwarding path red.
7. BGP/IGP change near RTT spike, but route only candidate path until independent forwarding/path proof.
8. Wi-Fi high MAC retries vs independently good WAN TCP; keep radio retry classification.
9. AP PoE insufficient, radio operating in limited mode, auth succeeds; show actual power vs class/advertised.
10. RADIUS accept but wrong VLAN/session enforcement; full onboarding stage chain and counterevidence.
11. One collector partitions from Core while devices still work; last-known graph and explicit “telemetry blind” mode.
12. Tenant-restricted/secret-bearing evidence and user after revocation: 403, no cached leakage, sanitized export.
13. Event burst, reconnect, late sample, cursor gap, old response after scope change: no stale success badge or redrawing full topology.
14. Large clustered topology + 20k client records/paged list: memory, responsiveness, search, screen-reader, keyboard checks.
15. Cross-domain site outage with unknown power vs WAN, OOB independently available: read-only next check, no invented power failure.

Browser E2E evidence must show API fixture + source identity plus viewport screenshots across 1440/1024/390 px and reduced-motion/high contrast. Real vendor/physical hardware data required for domain telemetry correctness and ROCe/wireless claims; lab replay alone is a contract test. Do not mark BN-030 shipped until backend ownership, coverage, UX accessibility, resource and auth security gates pass.

## 9. Decision and non-goals

**Decision:** build Bonsai's **Network Investigation Workbench** by composing extant Live, Routing, Investigations, device detail, site/collector/metric APIs behind one context/evidence protocol. Use SkyCMD's *claimed* agent-native unity as the philosophy; use publicly verifiable Kentik/ThousandEyes/queue-monitoring patterns for specific proven interactions; make BunSai multi-domain and evidence-driven. Avoid a second diagram engine, a 3D novelty-only rack view, unsupervised LLM-driven actions, copied branded UI, expensive per-packet graph storage, a mandatory cloud stack, or fake demonstration screenshots.

**Research follow-up:** capture a legitimate SkyCMD Varun/Ravi GUI walkthrough if a playable URL or accessible recording later becomes available; annotate exact timestamps, controls and behaviors to refine (not retroactively justify) the design. This is a separate media-evidence task, not a blocker on generic UX requirements.

**Linked work:** BONSAI_BACKLOG.md BN-030, BN-024 network semantics, BN-025 AI fabrics, BN-026 transport/service-provider, BN-027 campus, BN-028 enterprise knowledge, BN-029 persistence, docs/architecture/UI_FIRST_CONFIGURATION_FLEET_REVIEW_2026-11-19.md.