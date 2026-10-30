# Bonsai — DV4 Supplement Backlog: Network-Wide DDoS Awareness

> **Sprint**: DV4-Supplement
> **Analysis basis**: Full review of DV4 codebase state (streaming sources: gNMI, syslog, SNMP, NetFlow, sFlow, OTLP, BMP, BGP-LS), existing graph schema, output adapters (Splunk, Elastic, SNOW EM, Prometheus), ML sidecar wiring, and investigation runtime.
> **Principle**: Every item is grounded in actual code state — not documentation assumptions. Architecture is additive: builds on top of existing DV4 infrastructure without breaking existing flows.

---

## Motivation

Bonsai already ingests rich multi-source telemetry from network devices. The goal of this supplement is to channel that data into a **network-wide DDoS situational awareness layer**. The key design principles are:

1. **Not perimeter-centric** — DDoS signals are gathered from all device layers (core, distribution, access, edge, backbone) simultaneously. No assumption that DDoS is only a perimeter/edge problem.
2. **Pattern-aware, not traffic-volume-only** — Detect 1990s-style floods AND 2026-style low-and-slow, amplification, reflection, and protocol-abuse attacks. Differentiate bulk legitimate traffic (a user downloading GBs) from attack signatures.
3. **Graph-first enrichment** — All signals feed the graph with DDoS-specific node types, edges, and temporal relationships. The ML sidecar (already integrated) consumes this graph as its feature store.
4. **Time-to-react first** — The moment an anomaly pattern is confirmed, the response chain is triggered: DDoS Cloud Sink API → BGP prefix advertisement change → BMP post-incident assurance.
5. **BMP as assurance layer** — BMP is used post-detection to verify that BGP route changes (blackhole/community signalling to cloud DDoS scrubbing) landed correctly and that prefix restoration worked.
6. **False positive discipline** — All DDoS detections require multi-source corroboration or ML confidence threshold before escalating. Single-source volume spikes are flagged for triage, not auto-remediated.

---

## Epic Overview

| Epic | Title | Priority |
|------|-------|----------|
| DS-1 | DDoS Signal Extraction: Multi-Source Telemetry Enrichment | P0 |
| DS-2 | DDoS Graph Schema: Nodes, Edges, Attack Fingerprints | P0 |
| DS-3 | DDoS Detection Rules: Pattern Library + Synthesizer Integration | P0 |
| DS-4 | DDoS Response: Cloud Sink Integration + BGP Signalling | P0 |
| DS-5 | BMP Post-Incident Assurance: Prefix Convergence Verification | P1 |
| DS-6 | DDoS ML Feature Pipeline: Graph-to-Feature Export for Sidecar | P1 |
| DS-7 | DDoS Incident UI: Timeline, Attack Map, Mitigation Tracker | P1 |
| DS-8 | DDoS Simulation + Testing Harness | P2 |

---

---

## DS-1 — DDoS Signal Extraction: Multi-Source Telemetry Enrichment

### Analysis

Bonsai already receives telemetry from gNMI, syslog, SNMP traps, NetFlow/IPFIX, sFlow, OTLP, BMP, and BGP-LS. However, none of these sources currently extract the specific data fields that are most diagnostic for DDoS: packet-rate per protocol, SYN:ACK ratios, ICMP flood counters, TCP state exhaustion metrics, DNS query amplification ratios, fragment counters, and per-prefix source diversity. The following per-source gaps are identified:

**gNMI (`src/streaming/mod.rs`, `src/telemetry.rs`):**
- Interface in/out counters and error rates are already written to `Interface` nodes.
- Missing: per-protocol packet counters (ucast/mcast/bcast split), QoS queue drop rates, ACL hit counters, CPU/memory utilisation under load, LPTS (Local Packet Transport Services) drop counters (Cisco XR — critical for control-plane DDoS visibility), CoPP (Control Plane Policing) violation counters.
- Path profiles (`config/path_profiles/`) do not include any DDoS-specific gNMI paths. No vendor provides these in the default profiles today.
- Missing: gNMI subscription paths for Nokia SRL `/platform/linecard[*]/forwarding-complex[*]/resource-utilization`, Cisco XR `/oc-pf:forwarding-info/oc-pf:policy-forwarding/oc-pf:interface[*]/oc-pf:state`, Arista EOS `Interfaces/Interface/State/Counters/InUcastPkts`.

**Syslog (`src/signals/syslog.rs`, `config/syslog_patterns/`):**
- No DDoS-specific syslog patterns in any vendor file. ACL deny storms, CoPP threshold violations, uRPF drop messages, RTBH route install confirmations, and BGP community-triggered RTBH events are not captured.
- Cisco XR: `%SEC_LOGIN-1-QUIET_MODE_ON`, `%COPP-6-POLICY_VIOLATION`, `%IOSXR-3-COPP_DROP`, uRPF `%IPFIB-6-URPF_DROPS`.
- Arista EOS: `ACLMGR: ACL [name] on [intf]: [n] hit(s)`, `ROUTING: BGP route [prefix] added with community`.
- Nokia SRL: `interface-blackhole`, `route-blackhole`, `ip-filter-entry-statistics`.
- Juniper JunOS: `RT_FLOW_SESSION_DENY`, `COROUTED_POLICER_RATE_EXCEEDED`, `RPD_RT_ADD_BLACKHOLE`.
- FRR: no DDoS-relevant syslog patterns currently.

**SNMP (`src/signals/snmp.rs`):**
- Interface counters via `IF-MIB` are already partially parsed (via OID patterns). Missing: `ifInDiscards`, `ifInErrors`, `ifOutDiscards`, `ifOutErrors` broken out per-interface vs just aggregate.
- No OID patterns for: `ipIfStatsInDiscards`, `ipIfStatsHCInOctets` per-protocol (CISCO-IP-STAT-MIB), Cisco `ciscoEnvMonFanStatusChangeNotif` (irrelevant) vs `cBgpPeerFsmStateChange`, Juniper `jnxPfeFEDrops` (Packet Forwarding Engine drops — DDoS key signal).
- No SNMP polling of ACL hit counters via SNMP MIB tables (CISCO-IP-ACL-MIB, etc.).

**NetFlow/IPFIX (`src/streaming/netflow.rs`):**
- `AppFlow` node captures: src/dst address, bytes_per_sec, protocol. Missing fields critical for DDoS: `src_port`, `dst_port`, `tcp_flags` (SYN flood detection), `flow_packets` (packet count), `input_snmp` (ingress interface index → SNMP IfIndex → Interface node link), `bgp_next_hop`, `src_as`, `dst_as` (ASN-level attribution), `flow_direction` (ingress vs egress).
- No per-flow TCP flag analysis. SYN floods (all SYN, no ACK), SYN-ACK amplification, RST floods are invisible.
- NetFlow v9/IPFIX template field IDs for these exist but are not mapped in `parse_netflow_data_record()`.

**sFlow (`src/streaming/sflow.rs`):**
- Already parses raw packet headers (IP/TCP/UDP). Missing: extraction of TCP flags byte, ICMP type/code, UDP destination port for amplification vectors (DNS=53, NTP=123, SSDP=1900, memcached=11211).
- Counter samples already written to Interface. Missing: `sflow_sample_rate` as a normalisation factor (sFlow is sampled 1:N — all rates must be multiplied by sampling rate to get real traffic).
- No per-protocol packet rate derived from sFlow samples.

**BMP (`src/streaming/bmp.rs`):**
- Route monitoring and STATS_REPORT already implemented (D4-11). Missing DDoS-specific use: detecting sudden new BGP prefixes from unexpected ASNs (prefix hijack indicator), detecting RTBH community receipt on upstream sessions (upstream is scrubbing), detecting withdrawal of prefixes attacked (victim withdrawing to null-route).
- `STATS_REPORT` counters: `rejected_prefixes` (type 0), `duplicate_prefix_withdrawal` (type 4), `invalidated_cluster_list_loop` (type 5) — hijack indicators.

**OTLP (`src/streaming/otlp.rs`):**
- Span/metrics receiver already exists. Missing: no correlation between application-layer error rate spikes and concurrent network DDoS detections. This cross-layer correlation is important — if a UDP flood hits the uplink but application HTTP requests spike in timeouts simultaneously, that is a strong corroboration signal.

### Tasks

**T1 — gNMI: DDoS path profile for all supported vendors**
- Create `config/path_profiles/ddos_detection.yaml` covering:
  - **Nokia SRL**: `/interface[*]/statistics` (all counters), `/platform/linecard[*]/forwarding-complex[*]/resource-utilization` (TCAM, forwarding resource pressure), `/network-instance[*]/ip-forwarding/statistics` (forward/drop per protocol).
  - **Cisco IOS-XR**: `/oc-if:interfaces/oc-if:interface[*]/oc-if:state/oc-if:counters`, `/lpts/pifib/hardware/stats` (LPTS drops — key for CPU DDoS), `/copp/policy-map[*]/class-default/statistics` (CoPP violation rates).
  - **Arista EOS**: `/interfaces/interface[*]/state/counters` (full), `/qos/interfaces/interface[*]/output/queues/queue[*]/state` (queue drops indicate saturation).
  - **Juniper JunOS**: `/interfaces/interface[*]/state/counters`, `/pfe-statistics/traffic/traffic-class-information/traffic-class[*]/traffic-class-bytes`.
  - **FRR (Linux)**: `/interfaces/interface[*]/state/counters` via OpenConfig (FRR gNMI supports this).
  - **OpenConfig universal**: `openconfig-interfaces:interfaces`, `openconfig-platform:components` (line card resources).
- Subscribe at 10s sample interval (faster than default 30s for anomaly detection responsiveness).
- Tag new paths with `ddos: true` metadata in YAML for selective subscription filtering.

**T2 — gNMI: Telemetry writer for DDoS-specific counters**
- Extend `TelemetryEvent` variants in `src/telemetry.rs`:
  - `CoppViolation { device_address, class_name, dropped_packets, rate_pps }` — from CoPP path.
  - `LptsDropStats { device_address, flow_type, accepted_pps, dropped_pps }` — from XR LPTS.
  - `ForwardingResourcePressure { device_address, linecard_id, tcam_utilization_pct, nexthop_utilization_pct }` — from SRL/XR.
  - `ProtocolPacketRate { device_address, if_name, protocol, in_pps, out_pps, in_drop_pps }` — per-protocol rate.
- Write to graph via new graph writers (see DS-2).
- Emit `BonsaiEvent` with `source_type="gnmi_ddos"` for downstream detection pipeline.

**T3 — Syslog: DDoS-specific pattern files per vendor**
- Add `ddos_patterns` section to each vendor syslog YAML (or new `ddos_<vendor>.yaml` files):
  - **Cisco IOS-XE/XR**: `%COPP-6-POLICY_VIOLATION`, `%SEC-6-IPACCESSLOGP` (ACL permit/deny per proto/port), `%IPFIB-6-URPF_DROPS`, `%BGP-5-RT_ADD_BLACKHOLE` (RTBH confirmation), `%BGP-6-COMM_MATCH` (community-based route policy triggers).
  - **Arista EOS**: `ACLMGR: ACL [name] matched [n] packets`, `ROUTING: Null route installed for [prefix]`, `BGP-4-RTBH_ADDED`.
  - **Nokia SRL**: `interface blackhole route installed`, `ip-filter statistics threshold exceeded`, `copp-policy violat`.
  - **Juniper JunOS**: `RT_FLOW_SESSION_DENY [src] [dst] proto [n]`, `BGP_BLACKHOLE_ROUTE_INSTALLED`, `COROUTED_POLICER_RATE_EXCEEDED`.
  - **FRR**: `route-map match community [rtbh-community]`, `zebra: Installing RTBH route`.
- Each pattern extracts: `affected_prefix`, `protocol`, `drop_count`, `interface_name`, `acl_name` as structured fact fields.
- New `fact_type` values: `copp_violation`, `urpf_drop_flood`, `acl_deny_flood`, `rtbh_route_installed`, `rtbh_route_removed`.

**T4 — SNMP: DDoS-specific OID patterns**
- Extend `config/snmp_oid_patterns/default.yaml` with:
  - IF-MIB: `ifInDiscards` (1.3.6.1.2.1.2.2.1.13), `ifInErrors` (1.3.6.1.2.1.2.2.1.14), `ifOutDiscards` (1.3.6.1.2.1.2.2.1.19), `ifOutErrors` (1.3.6.1.2.1.2.2.1.20) — all per interface index.
  - IP-MIB: `ipIfStatsInDiscards` (1.3.6.1.2.1.4.31.3.1.9), `ipIfStatsHCInOctets` (1.3.6.1.2.1.4.31.3.1.6).
  - Cisco CISCO-IP-STAT-MIB traps: `cippIfHighInputQueueDrops`, `cippIfCBQoSViolation`.
  - Juniper JUNIPER-MIB: `jnxPfeFEDrops` (PFE forwarding engine drops — critical), `jnxRouteRejectTable` notification.
  - Nokia TIMETRA: `tmnxBfdSessionStateChange` (already exists), add `tmnxSvcTlsPortStatusChange` for service-level drop events.
- Add vendor-specific MIB bundles for download: Cisco, Juniper, Nokia DDoS-relevant MIBs (via MIB upload pipeline already built in D4-1 T4).

**T5 — NetFlow/IPFIX: DDoS field extraction**
- Extend `AppFlow` node (and NetFlow parser in `src/streaming/netflow.rs`) to capture:
  - `src_port: u16`, `dst_port: u16` — required for port-based attack vectors (DNS, NTP, SSDP, memcached amplification).
  - `tcp_flags: u8` — bitfield (SYN=0x02, ACK=0x10, RST=0x04, FIN=0x01, URG=0x20). Critical for SYN flood and RST flood patterns.
  - `flow_packets: u64` — packet count for the flow. pps = flow_packets / duration.
  - `src_as: u32`, `dst_as: u32` — BGP ASN attribution. IPFIX field IDs: 16 (bgpSourceAsNumber), 17 (bgpDestinationAsNumber).
  - `input_snmp: u32`, `output_snmp: u32` — interface SNMP ifIndex. Maps to `Interface` node via `if_index` field (already added in D4-5 T3 migration). Enables per-interface flow volume aggregation.
  - `flow_direction: u8` — 0=ingress, 1=egress. Separates inbound attack from outbound C2.
- NetFlow v9 template field IDs for all above: standard IANA-assigned (IN_PKTS=2, TCP_FLAGS=6, L4_SRC_PORT=7, L4_DST_PORT=11, INPUT_SNMP=10, OUTPUT_SNMP=14, BGP_AS=16/17, DIRECTION=61).
- IPFIX equivalents: same IDs per RFC 5102.

**T6 — sFlow: DDoS field normalisation + TCP/UDP extraction**
- Extend `src/streaming/sflow.rs` flow sample parsing:
  - Extract `tcp_flags` from raw packet bytes at correct TCP header offset (after IP header length from IHL field).
  - Extract `icmp_type` and `icmp_code` from ICMP header (IP protocol 1).
  - Extract `udp_dst_port` for UDP traffic — map to known amplification port list (53/DNS, 123/NTP, 1900/SSDP, 11211/memcached, 389/LDAP, 5353/mDNS, 19/chargen, 17/qotd).
  - Propagate `sampling_rate` from sFlow sample header through to `AppFlow` node as `sflow_sample_rate: u32`.
  - Derive `estimated_pps = (flow_packets * sampling_rate) / duration_secs` and store on `AppFlow`.
- Write amplification vector tag to `AppFlow.amplification_vector: Option<String>` based on `udp_dst_port` match.

**T7 — OTLP: Cross-layer DDoS impact correlation**
- When `write_otlp_metrics()` receives a metrics batch and detects `error_rate > threshold` or `latency_p99 > threshold` on an `Application` node:
  - Query for active DDoS `DetectionEvent` nodes on devices linked via `RUNS_SERVICE` or `COMPUTE_CONNECTED_TO` within ±60s window.
  - If match found, create `APP_DEGRADED_BY_DDOS(Application→DetectionEvent)` edge.
  - This edge confirms application impact, raises DDoS detection severity from `high` to `critical` automatically.
- Conversely: if a DDoS `DetectionEvent` fires, query associated `Application` nodes and flag them with `under_ddos_impact: bool` property.

**T8 — BMP: Attack-relevant route event extraction**
- Extend `src/streaming/bmp.rs` ROUTE_MONITORING handler to classify:
  - **RTBH community receipt**: If UPDATE contains community `65535:666` (RFC 7999 BLACKHOLE) or configured RTBH communities → write `DdosRouteEvent(type=rtbh_community_received)`.
  - **Prefix hijack signal**: If UPDATE advertises a more-specific prefix for a protected prefix from an unexpected ASN (not in `allowed_origin_asns` config) → write `DdosRouteEvent(type=unexpected_origin_as)`.
  - **Victim withdraw**: If prefix in `protected_prefixes` list is withdrawn → write `DdosRouteEvent(type=victim_prefix_withdrawn)`.
  - **Scrubbing community**: If configurable scrubbing communities (e.g., `64512:9999`) appear → write `DdosRouteEvent(type=scrubbing_community_signalled)`.
- `DdosRouteEvent` is a new node type (see DS-2). Link to `BgpSession` via `HAS_DDOS_ROUTE_EVENT`.
- These events feed the BMP assurance layer (DS-5).

---

## DS-2 — DDoS Graph Schema: Nodes, Edges, Attack Fingerprints

### Analysis

The existing graph schema (`src/graph/mod.rs`) has rich node/rel types for network topology, detection events, BGP sessions, and remediation. It does not have any DDoS-specific concepts: there is no model for attack fingerprints, no representation of traffic baselines vs anomalies, no per-prefix attack record, no scrubbing session tracking, and no way to record the multi-dimensional nature of an attack (vector, volume, scope, affected prefixes, involved devices). The DDoS schema supplement must be additive — it layers new node/rel types on top of the existing graph rather than replacing anything.

**Key design decisions:**
- `DdosEvent` is the top-level aggregate node (one per detected attack campaign). It links to multiple `DetectionEvent` nodes (multi-source corroboration), multiple `AttackVector` nodes (one per protocol/port attack dimension), and multiple `AffectedPrefix` nodes.
- `AttackVector` is a typed measurement node that captures the quantitative signature of one attack dimension: SYN flood from ASN X at Y pps targeting Z prefix.
- `AffectedPrefix` is distinct from a BGP `Prefix` node — it records the attack impact on a specific prefix/subnet, including scrubbing action taken.
- `DdosRouteEvent` records BGP-level events triggered by or in response to a DDoS (RTBH community, unexpected origin AS, scrubbing community signalling).
- `TrafficBaseline` is a rolling statistical node per (device, interface, protocol) that records p50/p95/p99 of pps and bytes/sec over configurable windows. The ML sidecar uses this as its normalisation baseline.
- `MitigationAction` tracks what external DDoS cloud sink was called, what was requested (null-route, scrub, rate-limit), and whether it was confirmed.

### Tasks

**T1 — Core DDoS node tables**

Add the following node tables to `src/graph/mod.rs` schema + KuzuDB migrations:

```
DdosEvent {
    id: STRING PRIMARY KEY,
    campaign_id: STRING,           -- links events across a multi-wave attack
    state: STRING,                 -- detecting / confirmed / mitigating / mitigated / post_incident
    confidence: FLOAT,             -- 0.0-1.0, multi-source corroboration score
    attack_start_ns: INT64,
    attack_end_ns: INT64,          -- nullable until resolved
    primary_vector: STRING,        -- udp_flood / syn_flood / icmp_flood / dns_amp / ntp_amp / http_flood / protocol_anomaly
    secondary_vectors: STRING,     -- JSON array of additional vectors
    target_summary: STRING,        -- human-readable: "prefix 198.51.100.0/24 on leaf4/core2/pe1"
    total_devices_involved: INT64,
    max_observed_pps: FLOAT,
    max_observed_gbps: FLOAT,
    source_diversity_score: FLOAT, -- 0.0=single source, 1.0=maximally distributed (botnet)
    created_at_ns: INT64,
    updated_at_ns: INT64
}

AttackVector {
    id: STRING PRIMARY KEY,
    ddos_event_id: STRING,
    vector_type: STRING,           -- syn_flood / udp_flood / icmp_flood / dns_amplification / ntp_amplification / ssdp_amplification / http_flood / slowloris / bgp_hijack
    protocol: STRING,              -- tcp / udp / icmp / other
    dst_port: INT64,               -- nullable for non-port vectors
    observed_pps: FLOAT,
    observed_bps: FLOAT,
    sampling_rate_normalised: BOOLEAN,  -- true if sFlow sampling rate applied
    top_src_prefixes: STRING,      -- JSON: [{"prefix": "x.x.x.x/y", "pct": 0.15}]
    top_src_asns: STRING,          -- JSON: [{"asn": 12345, "pct": 0.3}]
    tcp_flags_pattern: STRING,     -- "SYN_ONLY" / "SYN_ACK" / "RST_FLOOD" / "ACK_FLOOD" / "FIN_FLOOD"
    amplification_factor: FLOAT,   -- request_bytes / response_bytes ratio (for amp vectors)
    first_seen_ns: INT64,
    last_seen_ns: INT64
}

AffectedPrefix {
    id: STRING PRIMARY KEY,
    prefix: STRING,                -- CIDR notation
    is_protected: BOOLEAN,         -- in configured protected_prefixes list
    origin_asn: INT64,
    attack_traffic_gbps: FLOAT,
    legitimate_traffic_pct: FLOAT, -- estimated % of total that is legit (ML-derived)
    rtbh_applied: BOOLEAN,
    scrubbing_applied: BOOLEAN,
    scrubbing_provider: STRING,    -- e.g. "cloudflare" / "akamai" / "lumen" / "custom"
    mitigation_start_ns: INT64,
    mitigation_end_ns: INT64,
    prefix_restored: BOOLEAN,
    created_at_ns: INT64
}

DdosRouteEvent {
    id: STRING PRIMARY KEY,
    event_type: STRING,            -- rtbh_community_received / unexpected_origin_as / victim_prefix_withdrawn / scrubbing_community_signalled / prefix_restored / blackhole_withdrawn
    prefix: STRING,
    bgp_community: STRING,         -- community value that triggered event
    observed_asn: INT64,           -- ASN that advertised (for hijack detection)
    expected_asn: INT64,           -- configured expected origin (nullable)
    bgp_session_id: STRING,        -- FK to BgpSession
    occurred_at_ns: INT64
}

TrafficBaseline {
    id: STRING PRIMARY KEY,        -- device_address + ":" + if_name + ":" + protocol
    device_address: STRING,
    if_name: STRING,
    protocol: STRING,              -- "total" / "tcp" / "udp" / "icmp" / "tcp_syn" / "dns"
    window_minutes: INT64,         -- baseline window (default 60)
    p50_pps: FLOAT,
    p95_pps: FLOAT,
    p99_pps: FLOAT,
    p50_bps: FLOAT,
    p95_bps: FLOAT,
    p99_bps: FLOAT,
    sample_count: INT64,
    last_value_pps: FLOAT,
    last_value_bps: FLOAT,
    deviation_score: FLOAT,        -- current_value / p95 (>1 = anomalous)
    last_updated_ns: INT64
}

MitigationAction {
    id: STRING PRIMARY KEY,
    ddos_event_id: STRING,
    action_type: STRING,           -- rtbh_announce / scrubbing_divert / flowspec_inject / rate_limit / acl_push / cloud_sink_api / prefix_restore
    provider: STRING,              -- "cloudflare" / "akamai" / "lumen" / "arbor" / "local"
    target_prefix: STRING,
    api_request_json: STRING,      -- request sent to cloud sink (sanitised)
    api_response_json: STRING,     -- response received
    api_http_status: INT64,
    requested_at_ns: INT64,
    confirmed_at_ns: INT64,        -- nullable until confirmed
    confirmation_source: STRING,   -- "bmp_assurance" / "api_poll" / "manual"
    reverted_at_ns: INT64,         -- nullable until reverted
    revert_confirmed_at_ns: INT64  -- nullable until revert confirmed
}
```

**T2 — DDoS relationship tables**

Add the following rel tables to `src/graph/mod.rs`:

```
-- Top-level linkage
DDOS_INVOLVES_DEVICE(DdosEvent → Device)
    role: STRING           -- "targeted" / "amplifier" / "reflector" / "transit"
    traffic_fraction: FLOAT

DDOS_HAS_VECTOR(DdosEvent → AttackVector)
    detected_at_ns: INT64

DDOS_TARGETS_PREFIX(DdosEvent → AffectedPrefix)
    role: STRING           -- "primary" / "collateral"

DDOS_CORROBORATED_BY(DdosEvent → DetectionEvent)
    source_type: STRING    -- which signal corroborated this

DDOS_HAS_MITIGATION(DdosEvent → MitigationAction)
    sequence_order: INT64

-- Route events
HAS_DDOS_ROUTE_EVENT(BgpSession → DdosRouteEvent)
    updated_at_ns: INT64

DDOS_ROUTE_EVENT_ON(DdosRouteEvent → AffectedPrefix)
    updated_at_ns: INT64

-- Baseline tracking
BASELINE_FOR(TrafficBaseline → Interface)
    protocol: STRING

-- Post-incident assurance
MITIGATION_VERIFIED_BY(MitigationAction → DdosRouteEvent)
    verified_at_ns: INT64
```

**T3 — TrafficBaseline rolling update**
- Implement `update_traffic_baseline()` in `src/graph/mod.rs`.
- Called every 60s from a background task (similar to `archive.rs` retention task).
- Queries last N minutes of `Interface` counter history and computes p50/p95/p99 using incremental Welford algorithm.
- Stores result in `TrafficBaseline` nodes.
- `deviation_score = current_pps / p95_pps` — written to `TrafficBaseline.deviation_score` and also propagated to the linked `Interface` node as `ddos_deviation_score` for quick topology canvas overlay.
- Separate baselines per protocol where data is available (gNMI protocol-level counters from DS-1 T1/T2 required first).
- Background task: `spawn_baseline_task()` in `src/server_startup.rs`, interval configurable via `[ddos.baseline_window_minutes]` in config.

**T4 — gNMI CoPP/LPTS graph writers**
- Add graph write functions in `src/graph/mod.rs`:
  - `write_copp_violation()`: upserts a short-lived `CourtesyPolicer` stat onto the `Device` node properties (`copp_drop_pps_last`, `copp_violated_at_ns`, `copp_class_name`). Emits `BonsaiEvent(source_type="gnmi_ddos")`.
  - `write_lpts_stats()`: writes `lpts_drop_pps_last`, `lpts_accepted_pps_last`, `lpts_flow_type` onto `Device`.
  - `write_forwarding_pressure()`: writes `tcam_utilization_pct`, `nexthop_utilization_pct` onto `Device`.
- These properties feed the DDoS detection rules (DS-3) and are surfaced in the UI (DS-7).

**T5 — NL query schema extension**
- Extend `GRAPH_SCHEMA` constant in `src/http_server/nl_query.rs` with all DDoS node types, rel types, and semantic notes.
- Add 6 DDoS-specific few-shot query examples:
  1. "Show me all active DDoS events with mitigation status"
  2. "Which prefixes are under attack right now?"
  3. "What is the attack vector breakdown for DdosEvent X?"
  4. "Show traffic baseline deviation for all interfaces on core1"
  5. "Which devices are involved as amplifiers in recent events?"
  6. "Show me the mitigation timeline for the last attack"
- Investigation agent (DS-6) will also use these schema hints.

---

## DS-3 — DDoS Detection Rules: Pattern Library + Synthesizer Integration

### Analysis

Bonsai's detection pipeline today operates via the Python sidecar (`python/collector_engine.py`, `python/bonsai_sdk/rules/`) and via Rust-side synthesizer rules in `src/graph/mod.rs` for graph-write-time detections. Neither has any DDoS-aware rules. The existing rules fire on BGP state changes, interface up/down, BFD session changes — all topology-event-oriented. DDoS detection requires a fundamentally different rule type: **rate-based, multi-dimensional, and time-series-aware** rather than state-change-based.

Key detection philosophy:
- **Single-source volume spike** (e.g., one interface at 95% utilisation): emit `ddos_suspect` with `low` confidence. Do NOT auto-remediate. Queue for multi-source corroboration.
- **Multi-source corroboration** (e.g., gNMI counter spike + syslog CoPP violation + NetFlow SYN-only pattern on same device within 30s): upgrade to `ddos_confirmed` with `high` confidence. Trigger response chain.
- **Network-wide pattern** (e.g., >3 devices simultaneously showing deviation_score >5 on same destination prefix): upgrade to `ddos_campaign` severity `critical`. Trigger campaign-level response.
- **Protocol-specific patterns** have distinct rules — SYN flood looks different from DNS amplification, which looks different from a GBs-of-legitimate-traffic burst.

### Tasks

**T1 — Rust-side graph-write detection rules (low-latency path)**

Add new synthesizer detection fact_types in `src/graph/mod.rs` (called from telemetry writers, not the Python sidecar):

- `ddos_interface_pps_spike`: fired from `write_copp_violation()` and `write_protocol_packet_rate()` when `deviation_score > 10.0` (configurable). Severity: `medium`. Sub-key: `device_address:if_name`.
- `ddos_copp_violation`: fired from `write_copp_violation()` when `copp_drop_pps > configured_threshold`. Severity: `high`. This is a control-plane DDoS indicator.
- `ddos_lpts_exhaustion`: fired from `write_lpts_stats()` when LPTS drops >0 for >30s continuously on same flow type. Severity: `high`. XR-specific.
- `ddos_tcam_pressure`: fired from `write_forwarding_pressure()` when `tcam_utilization_pct > 90`. Severity: `medium`. Indicates forwarding table exhaustion from attack traffic or misconfigured ACLs.
- `ddos_syslog_acl_flood`: fired from syslog `fact_type=acl_deny_flood` when ACL deny count rate exceeds threshold. Severity: `medium`.
- `ddos_rtbh_installed`: fired from syslog `fact_type=rtbh_route_installed`. Severity: `info` (informational — RTBH was already applied upstream or locally, records it in the DDoS timeline).

All new fact_types added to the severity map in `src/server_startup.rs`.

**T2 — Python sidecar: DDoS multi-source corroboration rules**

New rule file `python/bonsai_sdk/rules/ddos_correlation.py`:

```python
class DdosCorroborationRule(StreamingRule):
    """
    Upgrades ddos_suspect detections to ddos_confirmed when multi-source evidence
    accumulates within a 60s window for the same device/prefix scope.
    """
    CORROBORATION_WINDOW_S = 60
    MIN_SOURCES_FOR_CONFIRMED = 2  # configurable
    SUSPECT_FACT_TYPES = {
        "ddos_interface_pps_spike",
        "ddos_syslog_acl_flood",
        "flow_interface_utilization_high",   # existing D4-10 rule
    }
    STRONG_INDICATORS = {
        "ddos_copp_violation",
        "ddos_lpts_exhaustion",
        "ddos_tcam_pressure",
    }
```

- Maintains a `SuspectWindow` dict keyed by `device_address` (or `affected_prefix` if available): tracks source types seen within the window.
- When `len(unique_source_types) >= MIN_SOURCES_FOR_CONFIRMED` and at least one `STRONG_INDICATOR` OR three `SUSPECT_FACT_TYPES` from different source_types: fires `ddos_confirmed` detection with `confidence` field set.
- `ddos_confirmed` detection includes `features.attack_vectors` (aggregated from all corroborating detections) and `features.affected_prefix`.

**T3 — Python sidecar: Protocol-specific attack vector rules**

New rule file `python/bonsai_sdk/rules/ddos_vectors.py`:

- **SYN flood rule** (`ddos_syn_flood`): `AppFlow.tcp_flags == SYN_ONLY (0x02)` AND `flow.dst_port IN [80, 443, 8080, 22, 25, 53]` AND `pps > baseline.p95_pps * 5`. Severity: `high`. Includes `features.tcp_flags_pattern="SYN_ONLY"` and estimated pps.
- **UDP amplification rule** (`ddos_amplification`): `AppFlow.amplification_vector IS NOT NULL` (set by sFlow parser DS-1 T6) AND `pps > baseline.p95_pps * 3`. Severity: `high`. Includes `features.amplification_vector` (dns/ntp/ssdp/memcached) and `features.amplification_factor`.
- **ICMP flood rule** (`ddos_icmp_flood`): ICMP protocol flow AND `pps > baseline.p95_pps * 10`. Severity: `medium`.
- **DNS query flood rule** (`ddos_dns_flood`): UDP dst_port=53 AND query rate spike > p99*3 (from SNMP/gNMI DNS server stats or flow data). Severity: `high`.
- **Slowloris / HTTP low-and-slow rule** (`ddos_http_slow`): TCP dst_port=80/443 AND new connection rate high but bytes/connection very low AND connection state table growing (from gNMI platform resource stats). Severity: `medium`.
- **BGP prefix hijack signal** (`ddos_bgp_hijack_suspect`): `DdosRouteEvent.event_type=unexpected_origin_as` AND prefix in `protected_prefixes`. Severity: `high`. Special: bypass multi-source requirement — single BMP signal is sufficient.
- **Volumetric campaign rule** (`ddos_campaign`): >3 unique `device_address` values each showing `ddos_confirmed` within 120s window AND same `affected_prefix` OR same destination ASN. Severity: `critical`. Creates a `DdosEvent(state=confirmed)` node aggregating all constituent detections.

All rules extend `StreamingRule` from `python/bonsai_sdk/rules/streaming.py`. Each rule uses `Features` dataclass with new DDoS-specific fields registered in `python/bonsai_sdk/detection.py`.

**T4 — Detection feature schema extension**

Extend `Features` dataclass in `python/bonsai_sdk/detection.py` with DDoS fields:

```python
@dataclass
class Features:
    # ... existing fields ...
    # DDoS-specific
    attack_vector: str = ""            # syn_flood / udp_flood / dns_amp / etc.
    affected_prefix: str = ""          # CIDR of attacked prefix
    observed_pps: float = 0.0
    observed_gbps: float = 0.0
    baseline_p95_pps: float = 0.0
    deviation_score: float = 0.0       # observed / baseline_p95
    tcp_flags_pattern: str = ""
    amplification_vector: str = ""     # dns / ntp / ssdp / memcached
    amplification_factor: float = 0.0
    source_diversity: float = 0.0      # 0=single source, 1=fully distributed
    top_src_asns: list = field(default_factory=list)
    top_src_prefixes: list = field(default_factory=list)
    ddos_event_id: str = ""            # populated when campaign-level event created
    corroborating_sources: list = field(default_factory=list)  # ["gnmi", "syslog", "netflow"]
    confidence: float = 0.0
```

**T5 — Confidence scoring model**

Implement `compute_ddos_confidence()` in `python/bonsai_sdk/rules/ddos_correlation.py`:

```
base_score = 0.0
+ 0.20 if gNMI counter evidence (deviation_score > 5)
+ 0.25 if NetFlow/sFlow vector evidence (tcp_flags or amplification)
+ 0.20 if syslog ACL/CoPP evidence
+ 0.20 if SNMP trap evidence (interface errors, PFE drops)
+ 0.15 if BMP route event evidence (RTBH community or hijack)
+ 0.10 bonus if all 5 sources corroborate (bonus for completeness)
- 0.20 if only single device (could be local misconfiguration)
+ 0.10 if multiple devices affected (confirms network scope)
+ 0.15 if affected prefix is in protected_prefixes list
confidence = clamp(base_score, 0.0, 1.0)
```

Threshold to fire `ddos_confirmed`: `confidence >= 0.50`.
Threshold to fire `ddos_campaign` (critical): `confidence >= 0.75` AND `devices_involved >= 3`.

**T6 — Syslog shun integration for DDoS noise**

During confirmed DDoS events, certain syslog message categories can flood at thousands/sec (ACL deny logs, CoPP violation logs). Integrate with the Shun engine (`src/shun.rs`):

- When `ddos_confirmed` fires and `observed_pps > ddos_shun_pps_threshold` (configurable, default 1000 pps for log volume): auto-create a `ShunRule` with:
  - `scope_type = "device"`, `scope_value = device_address`
  - `match_type = "fact_type"`, `match_value = "acl_deny_flood"` (suppress the repetitive fact from re-evaluation — the detection already fired)
  - `action = "rate_limit"`, `rate_limit_per_min = 10` (allow 10 per min for audit trail)
  - `expires_at_ns = confirmed_at_ns + 3600_000_000_000` (auto-expire after 1 hour)
- Auto-shun is gated on `ddos.auto_shun_on_confirmed: bool` config (default false — requires explicit opt-in).
- Audit-logged as `auto_shun_created_by_ddos_event`.

**T7 — Playbook: DDoS response initiation**

Create `playbooks/library/ddos_confirmed.yaml`:

```yaml
id: ddos_confirmed
description: "Confirmed DDoS attack — initiate multi-layer mitigation"
trigger_rule_ids:
  - ddos_confirmed
  - ddos_campaign
min_confidence: 0.5
severity_floor: high

steps:
  - action: query_graph
    description: "Identify affected prefixes and devices"
    cypher: |
      MATCH (d:DdosEvent {id: $event_id})-[:DDOS_TARGETS_PREFIX]->(p:AffectedPrefix)
      RETURN p.prefix, p.is_protected, p.origin_asn

  - action: call_api
    description: "Notify DDoS cloud sink (see DS-4)"
    endpoint: "POST /api/ddos/mitigation/request"
    payload_template: "{ddos_event_id, affected_prefix, attack_vector, observed_gbps}"

  - action: verify_graph
    description: "Verify MitigationAction node created"
    expected_graph_state: |
      MATCH (m:MitigationAction {ddos_event_id: $event_id})
      WHERE m.api_http_status >= 200 AND m.api_http_status < 300
      RETURN count(m) > 0

  - action: human_in_the_loop
    description: "Operator approval required before BGP RTBH announcement"
    severity_override: critical
    timeout_seconds: 300

  - action: gnmi_set
    description: "Apply RTBH community to attacked prefix (if HITL approved)"
    device: "$affected_device"
    path: "/routing-policy/defined-sets/bgp-defined-sets/community-sets/community-set[name=RTBH]/config/community-member"
    value: "$rtbh_community"

  - action: verify_graph
    description: "Verify BMP confirms RTBH route installed within 120s"
    expected_graph_state: |
      MATCH (r:DdosRouteEvent {event_type: 'rtbh_community_received', prefix: $affected_prefix})
      WHERE r.occurred_at_ns > $action_requested_at_ns
      RETURN count(r) > 0
    timeout_seconds: 120
```

---

## DS-4 — DDoS Response: Cloud Sink Integration + BGP Signalling

### Analysis

Bonsai already has output adapters (`src/output/`) for Splunk, Elastic, Prometheus, ServiceNow EM, and SNMP. None are DDoS-cloud-sink-aware. DDoS mitigation platforms have their own APIs (Cloudflare Magic Transit, Akamai Prolexic, Lumen DDoS Mitigation, Radware DefensePro, Arbor Sightline) which receive "signal" API calls to activate scrubbing or blackholing. In parallel, BGP-based mitigations (RTBH via RFC 7999, FlowSpec via RFC 5575) can be signalled directly to upstream routers or to cloud providers as BGP communities. Bonsai needs to:

1. **Detect fast** → already built in DS-3.
2. **Signal cloud sink via API** → a new output adapter `src/output/ddos_cloud_sink.rs`.
3. **Signal via BGP** → a new `bgp_mitigation_signal.rs` that uses the existing `gnmi_set.rs` infrastructure to push RTBH community or FlowSpec rules to configured devices.
4. **Track the mitigation lifecycle** via `MitigationAction` nodes (DS-2 T1).
5. **Confirm via BMP** that the routes changed as expected (DS-5).

**Existing infrastructure to leverage:**
- `src/output/traits.rs` — `OutputAdapter` trait. New cloud sink adapters will implement this.
- `src/credentials.rs` + `CredentialVault` — All API keys for cloud sinks stored in vault by alias.
- `src/gnmi_set.rs` — Already supports gNMI SET operations. Route-policy community injection can be done via gNMI SET.
- `src/http_server/remediation.rs` — Human-in-the-loop (HITL) approval path already exists. BGP mitigation with HITL = safest path.
- `src/integrations/` — Pattern for new integration modules.

**Supported cloud sink integration models:**
- **Direct API**: Cloud provider REST/JSON API receives prefix + action (scrub/blackhole/rate-limit). Provider re-routes traffic to scrubbing centre.
- **BGP RTBH**: Bonsai announces prefix with `65535:666` (RFC 7999) community to upstream via policy. Upstream drops traffic at peering point. Fast but brutal — all traffic to prefix dropped.
- **BGP FlowSpec (RFC 5575)**: Bonsai injects FlowSpec rule (match src/dst/proto/port, action drop/redirect) into routing table. Fine-grained — can drop only SYN packets to port 443 from ASN 12345 while allowing others.
- **BGP Community Signalling to scrubber**: Announce prefix with provider-specific community (e.g., `64512:9999` for Cloudflare) to divert traffic through scrubbing without full blackhole.

### Tasks

**T1 — DDoS Cloud Sink output adapter**

New file `src/output/ddos_cloud_sink.rs`:

```rust
pub struct DdosCloudSinkAdapter {
    config: DdosCloudSinkConfig,   // from [ddos.cloud_sinks[]]
    vault: Arc<CredentialVault>,
    http: reqwest::Client,
    audit: OutputAdapterAuditLog,
}
```

- Implements `OutputAdapter` trait with `OutputTopic::DetectionEvents`.
- Filters bus for `source_type="detection"` events with `rule_id IN ["ddos_confirmed", "ddos_campaign"]`.
- On receive: calls provider-specific API to signal mitigation start.
- Supported provider adapters (pluggable via `provider_type` config field):
  - `CloudflareProvider`: `POST https://api.cloudflare.com/client/v4/accounts/{account_id}/magic-transit/routes` with prefix + blackhole/advertise action.
  - `AkamaiProvider`: Akamai Prolexic API `POST /session-provisioning/v1/sessions` with SID, prefix, action.
  - `LumenProvider`: Lumen/CenturyLink DDoS API `POST /v1/mitigations` with prefix, type=blackhole/divert.
  - `ArborProvider`: Arbor Sightline REST API `POST /api/sp/mitigations/` with TMS mitigation JSON.
  - `GenericRestProvider`: configurable REST endpoint, method, auth (Bearer/Basic/API-key-header), payload template (Handlebars-style `{{prefix}}` substitution). For any provider not natively supported.
- On API call: writes `MitigationAction` node to graph with full request/response (sanitised — API keys redacted).
- API auth: `credential_alias` in config → vault resolve.
- Config structure in `src/config.rs`:

```toml
[[ddos.cloud_sinks]]
name = "cloudflare-mt"
provider_type = "cloudflare"          # cloudflare / akamai / lumen / arbor / generic_rest
enabled = false                       # off by default — explicit enablement required
credential_alias = "cloudflare_api_token"
account_id = "xxx"                    # provider-specific
api_base_url = "https://api.cloudflare.com/client/v4"
min_confidence_to_trigger = 0.7       # confidence threshold from DS-3 T5
require_hitl = true                   # human approval before API call
hitl_timeout_seconds = 300
protected_prefixes = ["198.51.100.0/24", "203.0.113.0/24"]
auto_restore_after_minutes = 60       # if 0, no auto-restore
```

**T2 — BGP RTBH signalling via gNMI SET**

New file `src/integrations/bgp_mitigation.rs`:

- `BgpMitigationSignal` struct: `prefix`, `action` (rtbh/flowspec/community_signal/restore), `target_devices` (list of device addresses to apply on), `rtbh_community`, `flowspec_rule_json`.
- `announce_rtbh()`: uses `gnmi_set.rs` to push route-policy community tag to device.
  - Nokia SRL: gNMI SET on `/routing-policy/community-set[name=RTBH-BLACKHOLE]/member` + static route to Null0 with community.
  - Cisco IOS-XR: gNMI SET on `/routing-policy/route-policies/route-policy[name=RTBH_POLICY]/statements`.
  - Arista EOS: gNMI SET or SSH fallback (EOS gNMI SET has limited policy write support).
  - FRR: SSH/CLI via PyATS sidecar (FRR gNMI SET not fully available for BGP policy). Use `vtysh -c "route-map RTBH permit 10 ; match community RTBH_COMM"`.
  - **Safety**: `announce_rtbh()` ALWAYS requires HITL approval (`hitl_timeout_seconds` from config) before executing. The HITL check uses the existing remediation HITL mechanism (`src/remediation/`).
- `restore_prefix()`: reverse of `announce_rtbh()` — removes community from prefix, withdraws null route.
- `inject_flowspec_rule()`: injects RFC 5575 FlowSpec NLRI via BGP. This is a more advanced capability — requires BGP daemon support (FRR supports FlowSpec, Cisco XR supports it). Deferred to T5.
- `announce_scrubbing_community()`: applies provider-specific divert community instead of full blackhole. Less disruptive than RTBH.

**T3 — Mitigation lifecycle state machine**

Implement `DdosMitigationFsm` in `src/integrations/bgp_mitigation.rs`:

```
States: Idle → Detected → AwaitingHitl → Signalling → Active → Verifying → Mitigated → Restoring → Restored

Transitions:
  Idle         → Detected    : ddos_confirmed detection fires
  Detected     → AwaitingHitl: cloud sink or BGP RTBH enabled + require_hitl=true
  AwaitingHitl → Signalling  : operator approves (HITL)
  AwaitingHitl → Idle        : operator rejects (HITL) OR timeout
  Detected     → Signalling  : require_hitl=false (auto-trigger)
  Signalling   → Active      : cloud sink API returns 2xx OR gNMI SET succeeds
  Signalling   → Detected    : cloud sink API returns error (retry with backoff)
  Active       → Verifying   : BMP assurance check triggered (DS-5)
  Verifying    → Mitigated   : BMP confirms RTBH/scrubbing route received
  Verifying    → Active      : BMP check inconclusive (retry after 30s)
  Mitigated    → Restoring   : auto_restore_after_minutes elapsed OR operator restore command
  Restoring    → Restored    : cloud sink API restore 2xx + BMP confirms prefix restored
```

- Each transition emits a `BonsaiEvent` with `source_type="ddos_fsm"` for UI SSE streaming.
- `DdosEvent.state` field (DS-2 T1) is updated on every transition.
- SSE endpoint: `/api/ddos/events/stream` — clients subscribe for real-time state transitions.

**T4 — Mitigation API endpoints**

New handler file `src/http_server/ddos.rs`:

- `POST /api/ddos/mitigation/request` — trigger mitigation for a prefix/event (validates confidence, checks HITL if required, creates `MitigationAction` node, calls cloud sink adapter).
- `POST /api/ddos/mitigation/{id}/approve` — HITL approval for pending mitigation.
- `POST /api/ddos/mitigation/{id}/reject` — HITL rejection.
- `POST /api/ddos/mitigation/{id}/restore` — manual restore of a mitigated prefix.
- `GET /api/ddos/events` — list all `DdosEvent` nodes with state, confidence, affected prefixes, mitigation actions.
- `GET /api/ddos/events/{id}` — full detail of one event including attack vectors, corroborating detections, mitigation timeline.
- `GET /api/ddos/events/stream` — SSE stream for real-time DDoS event state changes.
- `GET /api/ddos/config` — current DDoS config (cloud sinks, protected prefixes, thresholds).
- `PATCH /api/ddos/config` — update DDoS config at runtime (persisted to ConfigItem DB).
- `GET /api/ddos/baselines` — current traffic baselines per device/interface/protocol.
- `GET /api/ddos/mitigations/active` — all currently active mitigations (state = Active or Mitigated).
- Role requirements: `GET` endpoints → Viewer+. Mitigation trigger/approve/restore → Operator+. Config PATCH → Admin.

**T5 — FlowSpec injection (advanced, deferred)**

- Implement RFC 5575 FlowSpec NLRI construction for FRR BGP daemon.
- FRR `vtysh` command generation: `bgp flowspec-mode {device_id}`, `flowspec src-prefix {prefix} proto tcp dst-port 80 action drop`.
- Validate via BMP: after FlowSpec inject, monitor BMP ROUTE_MONITORING for FlowSpec NLRI receipt by downstream BGP peers.
- This is a P2 task — requires lab validation with FRR FlowSpec capability before implementation.

**T6 — Protected prefix management API + UI**

- `GET /api/ddos/protected-prefixes` — list of prefixes in the protection scope.
- `POST /api/ddos/protected-prefixes` — add a prefix with metadata (owner, ASN, description, cloud_sink_bindings).
- `DELETE /api/ddos/protected-prefixes/{prefix}` — remove from protection scope.
- Stored as `ConfigItem` records with `config_class="ddos_protected_prefix"`.
- UI: `DdosConfig.svelte` sub-page within the DDoS dashboard (DS-7) for prefix management.
- **Auto-discovery**: when `AffectedPrefix` nodes are created by detection (DS-2 T1), prompt operator "Would you like to add 198.51.100.0/24 to the protected prefix list?"

**T7 — Rate limiting and safety guards**

Critical safety mechanisms to prevent accidental network disruption:
- **Max concurrent mitigations**: configurable `ddos.max_concurrent_mitigations` (default 3). If exceeded: queue new mitigations for HITL even if `require_hitl=false`.
- **Cool-down period**: `ddos.mitigation_cooldown_seconds` (default 300) — cannot re-trigger mitigation for same prefix within cool-down after restore.
- **Confidence floor**: `ddos.min_confidence_for_auto_trigger` (default 0.75) — below this, ALWAYS require HITL regardless of config.
- **Protected devices**: `ddos.never_apply_rtbh_on` — list of device addresses where RTBH gNMI SET is never permitted (e.g., core routers where mis-config could cause outage).
- **Dry-run mode**: `ddos.dry_run = true` — all API calls and gNMI SETs are simulated (logged but not executed). Default `true` on first install.
- All safety guard violations are audit-logged as `ddos_safety_guard_triggered`.

---

## DS-5 — BMP Post-Incident Assurance: Prefix Convergence Verification

### Analysis

BMP (BGP Monitoring Protocol, RFC 7854) gives Bonsai a passive, real-time view of what BGP routes every monitored router has in its Adj-RIB-In (routes received from peers) and Loc-RIB (routes selected for the routing table). This makes BMP the ideal **assurance layer** for DDoS mitigation confirmation:

1. **Mitigation confirmation**: When Bonsai issues an RTBH community or calls a cloud sink API, the expected outcome is that the attacked prefix gets routed to a null route or a scrubbing centre. BMP should show the prefix's next-hop change (to 0.0.0.0/Null, or to a scrubbing centre address) on relevant BGP sessions within a convergence window (typically 10-60 seconds).

2. **Scrubbing community confirmation**: If a provider-specific divert community was announced, BMP on the upstream peering session should show the prefix being re-advertised WITH the community back towards the network, confirming the provider received and actioned it.

3. **Restoration verification**: When mitigation is restored (RTBH withdrawn, scrubbing community removed), BMP should confirm the prefix is re-announced to legitimate next-hops within the convergence window.

4. **Failure escalation**: If BMP does not confirm within the convergence window, emit `ddos_mitigation_unconfirmed` detection and alert the operator. This prevents silent failures where the API call succeeded but BGP didn't propagate.

5. **Post-incident prefix audit**: After restoration, BMP data allows auditing: were there any unexpected origin ASN changes? Were any more-specific prefixes injected during the attack? Did any peer de-prefer the prefix during the incident? This is the "post-incident analysis" dimension.

**Existing infrastructure:**
- `src/streaming/bmp.rs`: Full BMP parser, ROUTE_MONITORING, STATS_REPORT, PEER_UP/DOWN.
- `src/graph/mod.rs`: `BgpSession`, `DdosRouteEvent` (added DS-2 T1), `HAS_DDOS_ROUTE_EVENT` rel.
- `src/streaming/bmp.rs`: `write_bmp_route_monitoring()` already writes prefix path attributes.
- D4-11 (completed): BMP STATS_REPORT parsing writes adj_rib_in route counts.

### Tasks

**T1 — BMP route event classifier for DDoS patterns**

Extend `write_bmp_route_monitoring()` in `src/streaming/bmp.rs`:

For each ROUTE_MONITORING UPDATE, classify the event against active `DdosEvent` nodes:
- **RTBH confirmation check**: If prefix matches an `AffectedPrefix` with `rtbh_applied=true` AND next-hop changes to `0.0.0.0` or `192.0.2.1` (RFC 5737 documentation range used as null) OR AS_PATH contains configured RTBH peer ASN → write `DdosRouteEvent(type=rtbh_community_received)` and call `confirm_mitigation_action()`.
- **Scrubbing community check**: If prefix matches `AffectedPrefix.prefix` AND UPDATE communities contain configured scrubbing community value → write `DdosRouteEvent(type=scrubbing_community_signalled)`.
- **Unexpected origin detection**: If prefix is in `protected_prefixes` AND UPDATE origin ASN NOT in `allowed_origin_asns` config AND this is a new announcement (not withdrawal) → write `DdosRouteEvent(type=unexpected_origin_as)`. This is the hijack early-warning.
- **Prefix restoration check**: If prefix was previously `rtbh_applied=true` AND UPDATE is a new announcement with a non-null legitimate next-hop AND community list does NOT contain RTBH community → write `DdosRouteEvent(type=prefix_restored)` and call `confirm_restoration()`.
- **Victim withdraw**: If prefix in `protected_prefixes` AND UPDATE is a `WITHDRAWN_ROUTES` entry (not just implicit withdraw) → write `DdosRouteEvent(type=victim_prefix_withdrawn)`.

New helper: `lookup_active_ddos_event_for_prefix(prefix: &str) -> Option<String>` — Cypher query against `AffectedPrefix` nodes with active `DdosEvent` links.

**T2 — Assurance timer + confirmation engine**

New module `src/integrations/bmp_assurance.rs`:

```rust
pub struct BmpAssuranceEngine {
    graph: Arc<GraphStore>,
    pending_confirmations: Arc<Mutex<HashMap<String, PendingConfirmation>>>,
    // keyed by MitigationAction.id
}

struct PendingConfirmation {
    mitigation_action_id: String,
    ddos_event_id: String,
    expected_event_type: String,    // "rtbh_community_received" / "prefix_restored"
    prefix: String,
    deadline_ns: i64,
    check_interval_secs: u64,
}
```

- `register_pending(mitigation_action_id, ddos_event_id, prefix, expected_type, convergence_window_secs)` — adds to pending map.
- Background sweep every 5s: for each `PendingConfirmation`, query graph for `DdosRouteEvent` matching expected_type + prefix + occurred_at_ns > mitigation_requested_at_ns.
- If found: call `confirm_mitigation_action(mitigation_action_id, confirmed_at_ns, source="bmp_assurance")`. Updates `MitigationAction.confirmed_at_ns` and `confirmation_source`. Transitions `DdosEvent.state` to `Mitigated` or `Restored` via FSM.
- If not found before deadline: emit `ddos_mitigation_unconfirmed` detection (severity=`critical`) with context. Also emits `BonsaiEvent(source_type="ddos_assurance", event_type="mitigation_unconfirmed")`.
- Wired into `server_startup.rs` alongside the BMP receiver.

**T3 — BMP STATS_REPORT DDoS correlation**

Extend `write_bmp_statistics_report()` in `src/streaming/bmp.rs`:

During an active `DdosEvent` (state = Active or Mitigating):
- Monitor `rejected_prefixes` (STATS type 0): sudden drop in rejected prefixes for a previously attacked session may indicate filtering is working.
- Monitor `adj_rib_in` count: rapid growth in Adj-RIB-In can indicate prefix injection/hijack attack.
- Monitor `duplicate_prefix_withdrawal` (STATS type 4): high rate → possible BGP instability caused by attack.
- Write counter history to `BgpSession.adj_rib_in_history_json` (last 10 readings as JSON array with timestamps) for trend analysis.
- Fire `ddos_bgp_table_instability` (severity=`high`) if adj_rib_in changes >10% within one STATS_REPORT cycle AND active DdosEvent exists.

**T4 — Post-incident BGP prefix audit**

New API endpoint `GET /api/ddos/events/{id}/bgp-audit`:

After a DdosEvent transitions to `Restored` state, generate a BGP audit report:
- **Origin ASN timeline**: Were there any unexpected origin ASN advertisements for affected prefixes during the incident window?
- **More-specific prefix analysis**: Were any /25, /26, /27 more-specifics of the attacked prefix announced by unknown ASNs? (MOAS — Multiple Origin AS, hijack signal).
- **Convergence time**: Measured delta between `MitigationAction.requested_at_ns` and `MitigationAction.confirmed_at_ns` per action.
- **Session stability**: Were any BMP sessions lost during the incident (could indicate router CPU DDoS impacting BMP)?
- **Peer de-preference events**: Did any BGP peer withdraw sessions during the incident?

Data sources: `BgpSession`, `DdosRouteEvent`, `MitigationAction`, `BmpSession` nodes.
Returns JSON report suitable for incident post-mortem documentation.

**T5 — Protected prefix allowed-origin config**

Extend `src/config.rs` with `DdosProtectedPrefix`:

```toml
[[ddos.protected_prefixes]]
prefix = "198.51.100.0/24"
description = "Customer A primary block"
origin_asn = 65001
allowed_origin_asns = [65001, 65002]   # secondary ASNs allowed
rtbh_community = "65535:666"
scrubbing_community = "64512:9999"
cloud_sink_binding = "cloudflare-mt"    # which cloud sink to notify
auto_rtbh_on_confirmed = false          # require HITL before RTBH
```

- `scripts/import_protected_prefixes.py` — bulk import from CSV or from NetBox IP prefix table (NetBox enricher already has subnet nodes).
- `GET /api/ddos/protected-prefixes` uses both config-file entries and `ConfigItem` DB entries (config_class=ddos_protected_prefix).

**T6 — BMP session loss as DDoS indicator**

When BMP sessions (`BmpSession` nodes) are disconnected during a period with active `DetectionEvent` nodes on the same device, correlate:
- If `peer_down_reason = "remote_system_closed"` (BMP PEER_DOWN reason 4) AND `DetectionEvent` with `rule_id` matching interface/CPU stress pattern exists within ±60s → create `DDOS_CORROBORATED_BY(DdosEvent→DetectionEvent)` link.
- BMP session loss during attack is a secondary indicator — router CPU pressure from attack traffic caused BMP session to drop.
- Fire `ddos_bmp_session_lost_under_attack` (severity=`medium`) with context.

**T7 — Multi-session convergence tracking**

For large networks with many BGP speakers, RTBH propagation should be confirmed on ALL sessions, not just one:

- When `announce_rtbh()` fires, identify all BMP-monitored sessions where the affected prefix is in Adj-RIB-In.
- Register one `PendingConfirmation` per session.
- Track `confirmed_session_count` vs `expected_session_count` on `MitigationAction`.
- If `confirmed_session_count / expected_session_count < 0.8` after convergence window → `ddos_mitigation_partial_convergence` detection (severity=`high`) — some routers didn't receive the RTBH.
- Full confirmation only when all expected sessions confirm.

---

## DS-6 — DDoS ML Feature Pipeline: Graph-to-Feature Export for Sidecar

### Analysis

The ML sidecar (`python/bonsai_ml/`, `python/bonsai_sdk/`) already has a GNN pipeline (`bonsai_ml/gnn/`) and feature schema (`bonsai_ml/feature_schema.py`). The embeddings module (`bonsai_ml/embeddings.py`) exists but has no DDoS-specific feature extraction. The sidecar can already call the Bonsai gRPC API and graph query endpoints. What is missing is:

1. A **DDoS feature vector** definition: what graph properties to extract, how to aggregate them into features, and at what temporal granularity.
2. A **baseline comparison** feature: current_value / baseline_p95 ratio for each metric (the `deviation_score` already computed by `TrafficBaseline` nodes in DS-2 T3).
3. A **multi-device temporal feature matrix**: for the campaign detection model, features are not per-device but per (time_window, affected_prefix) aggregated across all devices.
4. A **ground-truth labelling mechanism**: for supervised learning, the sidecar needs to know which `DdosEvent` nodes were confirmed true positives and which were false positives (from operator feedback — DS-7 UI).
5. A **continuous feature export** pipeline: features must be continuously computed and available, not computed on-demand, so the ML model can score in near-real-time.

**This epic is the bridge between the graph-enrichment work (DS-1→DS-5) and eventual ML training/inference. It does NOT include model training itself — that is out of scope for this supplement.**

### Tasks

**T1 — DDoS feature schema definition**

New file `python/bonsai_ml/ddos_feature_schema.py`:

```python
DDOS_FEATURE_SCHEMA = {
    # Per-interface, per-protocol, 60s window
    "interface_pps_ratio": "current_pps / baseline_p95_pps",         # deviation ratio
    "interface_bps_ratio": "current_bps / baseline_p95_bps",
    "tcp_syn_ratio": "syn_packets / total_tcp_packets",               # 1.0 = pure SYN flood
    "udp_amplification_ratio": "amplification_vector_pps / total_pps",
    "icmp_ratio": "icmp_pps / total_pps",
    "new_source_ip_entropy": "unique_src_ips in window / expected_unique_src_ips",
    # Per-device
    "copp_drop_rate": "copp_drop_pps (normalised to device class)",
    "lpts_drop_rate": "lpts_drop_pps (XR-specific, 0 for others)",
    "tcam_pressure": "tcam_utilization_pct / 100",
    "cpu_ratio": "cpu_util_pct / baseline_cpu_p95",
    "acl_deny_rate": "acl_deny_pps / baseline_acl_deny_p95",
    # BGP/BMP features
    "prefix_stability": "1 - (prefix_withdrawals / adj_rib_in_total)",
    "unexpected_asn_score": "1 if unexpected_origin_as event else 0",
    "rtbh_active": "1 if AffectedPrefix.rtbh_applied else 0",
    # Multi-source corroboration score
    "corroboration_source_count": "count(distinct source_types in window)",
    "corroboration_strength": "sum(source weights) per DS-3 T5 confidence model",
    # Temporal features
    "attack_duration_seconds": "now - DdosEvent.attack_start_ns",
    "ramp_rate": "pps_delta / time_delta (attack speed)",
    "burst_pattern": "std_dev(pps_in_window) / mean(pps_in_window)",
}
```

Features are computed at 3 temporal granularities:
- **10s window**: for real-time detection (low-latency).
- **60s window**: for pattern classification (most useful for ML).
- **300s window**: for campaign-level trending (ramp-rate, burst pattern).

**T2 — Feature extraction pipeline**

New file `python/bonsai_ml/ddos_features.py`:

```python
class DdosFeatureExtractor:
    """
    Queries the Bonsai graph API to extract DDoS feature vectors.
    Called by the sidecar on a configurable interval (default 10s).
    """
    
    def extract_device_features(self, device_address: str, window_s: int = 60) -> DdosDeviceFeature:
        """Extract per-device feature vector from graph."""
        # Queries: TrafficBaseline, Interface counters, CoPP stats, LPTS stats
        # Returns: DdosDeviceFeature dataclass
    
    def extract_prefix_features(self, prefix: str, window_s: int = 60) -> DdosPrefixFeature:
        """Extract per-prefix feature vector: aggregates all devices seeing traffic to prefix."""
        # Queries: AppFlow nodes filtered by dst_prefix, AffectedPrefix, DdosRouteEvent
    
    def extract_campaign_feature_matrix(self, window_s: int = 300) -> CampaignFeatureMatrix:
        """
        Extract multi-device feature matrix for campaign detection.
        Returns: (n_devices, n_features) numpy array + device_address index
        """
        # Used by GNN model: each device is a node, features are edge attributes
    
    def compute_source_diversity(self, flows: List[AppFlow]) -> float:
        """Shannon entropy of source IP /24 prefixes → 0=single source, 1=full diversity."""
```

- Uses `GET /api/explorer/query` (NL query endpoint) with pre-built Cypher queries for each feature type.
- Falls back to direct graph queries via `bonsai_sdk.client.py` if NL query latency is too high.
- `DdosDeviceFeature` and `DdosPrefixFeature` are dataclasses registered in `ddos_feature_schema.py`.

**T3 — Continuous feature export**

New sidecar task in `python/collector_engine.py` or new `python/ddos_feature_daemon.py`:

- Runs as a background thread in the sidecar process.
- Every 10s: calls `extract_device_features()` for all tracked devices.
- Writes feature vectors to:
  - In-memory `FeatureCache` (ring buffer, last 30 readings per device) for immediate rule evaluation.
  - Graph: upserts `DdosFeatureSnapshot` property set onto each `Device` node (latest feature vector as JSON blob) for NL query access.
  - Optional TSDB export: if TSDB configured (`[integrations.tsdb]`), push feature time-series for historical trend analysis and model training data collection.
- `GET /api/ddos/features/{device_address}` — returns current feature vector JSON for a device.
- `GET /api/ddos/features/matrix` — returns current campaign feature matrix as JSON.

**T4 — Ground-truth labelling for supervised training**

Extend `DdosEvent` node with labelling fields:
- `operator_verdict: String` — `"true_positive"` / `"false_positive"` / `"indeterminate"`.
- `verdict_note: String` — free-text operator annotation.
- `verdict_at_ns: Int64`.
- `verdict_by: String` — operator ID from auth system.

New API: `POST /api/ddos/events/{id}/verdict` — sets operator verdict. Role: Operator+.

In `python/bonsai_ml/ddos_features.py`, `export_labelled_dataset()`:
- Queries all `DdosEvent` nodes with `operator_verdict IS NOT NULL`.
- For each event: retrieves the feature snapshots from the event window (from graph or TSDB).
- Exports as JSON Lines dataset: `{features: {...}, label: "true_positive", event_id: "..."}`.
- `GET /api/ddos/training-export` — triggers export, returns download link.
- Dataset format is compatible with standard ML training frameworks (scikit-learn, PyTorch, TensorFlow).

**T5 — GNN integration for DDoS campaign detection**

Extend `python/bonsai_ml/gnn/` pipeline for DDoS:

- `DdosGnnDataset`: constructs graph from live Bonsai data for GNN inference.
  - Nodes: Device + Interface + AppFlow nodes visible in current 60s window.
  - Node features: `DdosDeviceFeature` vector.
  - Edges: `CONNECTED_TO`, `CARRIES_FLOW` relationships with flow volume as edge weight.
- `DdosCampaignClassifier`: GNN model (architecture: GCN or GraphSAGE — 2 layers) that takes the above graph and outputs per-device anomaly score + global campaign probability.
- Inference pipeline:
  1. Feature extractor runs every 10s → builds `DdosGnnDataset`.
  2. GNN model forward pass → per-device scores.
  3. If global campaign probability > threshold → call `create_detection(rule_id="ddos_gnn_campaign")`.
  4. Detection includes `features.gnn_device_scores` (per-device anomaly contribution).
- **Training is out of scope for this supplement**. The model weight loading path (`load_checkpoint()`) is a stub that logs a warning if no checkpoint is found — inference is disabled until a trained checkpoint is provided.
- `GET /api/sidecar/ddos/model-status` — reports whether GNN model checkpoint is loaded and ready.

**T6 — Feature drift monitoring**

The baseline (`TrafficBaseline`) will drift over time (legitimate traffic growth, new applications). Feature drift causes false positives.

- `DdosBaselineDriftDetector` in `python/bonsai_ml/ddos_features.py`:
  - Compares current `TrafficBaseline.p95_pps` against rolling 7-day median of `p95_pps` snapshots.
  - If p95 has grown >50% vs 7-day median → emit `ddos_baseline_drift` detection (severity=`info`) to prompt operator to acknowledge the new baseline.
- Baseline snapshots stored as `ConfigItem` records (config_class=traffic_baseline_snapshot) with timestamps.
- `POST /api/ddos/baselines/{device_address}/acknowledge` — operator acknowledges new baseline, resets drift detector reference point.

---

## DS-7 — DDoS Incident UI: Timeline, Attack Map, Mitigation Tracker

### Analysis

The existing UI (`ui/src/routes/`) has purpose-built pages for Incidents, Topology, Explorer, Collectors, Governance, Settings, etc. None have DDoS-specific views. The DDoS UI requires a different information architecture than general incident management:

- **Time is the primary axis** — DDoS events evolve rapidly (seconds to minutes). A timeline view where detections, cloud sink calls, BGP changes, and confirmations are plotted chronologically is more valuable than a flat incident list.
- **Network scope needs spatial context** — an "attack map" showing which devices are involved, which prefixes are targeted, and where the attack traffic is flowing is essential for operator situational awareness.
- **Mitigation status must be glanceable** — operators under attack need to see at a glance: "is mitigation active? is it confirmed? is it working?"
- **Real-time updates are essential** — SSE streaming (already used in Governance.svelte) must drive live updates without polling.

The UI must not require a page refresh or manual query — it should auto-update via the `GET /api/ddos/events/stream` SSE endpoint defined in DS-4 T4.

### Tasks

**T1 — New `DdosDashboard.svelte` top-level page**

New file `ui/src/routes/DdosDashboard.svelte`:

**Layout (3 main areas):**

*Top strip — Live Status Bar*:
- Active attack count badge (green=0, amber=detecting, red=confirmed/campaign).
- Current mitigations count.
- Last updated timestamp (live from SSE).
- Global "DRY RUN MODE" warning badge if `ddos.dry_run=true` in config.
- Quick-action buttons: "View Active", "Configure", "Export Report".

*Left panel (1/3 width) — Active Events List*:
- One card per `DdosEvent` node with state ≠ `Idle` and ≠ `Restored`.
- Card shows: state pill (color-coded per FSM state), primary vector badge, affected prefix(es), confidence score ring (reuse Graph Health score ring component), device count, start time + elapsed.
- Click → loads event detail in right panel.
- "Past 24h" toggle shows resolved events.

*Right panel (2/3 width) — Event Detail + Timeline*:
- Header: DdosEvent state + confidence + affected prefixes.
- **Timeline tab** (default): vertical timeline of all events linked to this DdosEvent in chronological order:
  - `DetectionEvent` entries (colour: orange — detection signal)
  - `MitigationAction` entries (colour: blue — action taken)
  - `DdosRouteEvent` entries (colour: purple — BGP evidence)
  - State transitions (colour: grey — FSM state changes)
  - Each entry: source badge (gNMI/syslog/SNMP/NetFlow/sFlow/BMP), icon, timestamp, brief description.
- **Attack Vectors tab**: table of `AttackVector` nodes — vector_type, protocol, dst_port, observed_pps, observed_gbps, top_src_asns (chip list), tcp_flags_pattern.
- **Affected Prefixes tab**: table of `AffectedPrefix` nodes — prefix, origin_asn, rtbh_applied badge, scrubbing_applied badge, mitigation timing.
- **Mitigation tab**: ordered list of `MitigationAction` nodes — action_type, provider, status, confirmation badge (✓ BMP confirmed / ⏳ pending / ✗ unconfirmed), timing.
- **BGP Audit tab** (post-incident, visible when state=Restored): triggers `GET /api/ddos/events/{id}/bgp-audit`, renders convergence timeline, unexpected ASN events, more-specific prefix table.

**T2 — Attack map topology overlay**

Extend `ui/src/lib/Topology.svelte` (existing topology canvas) with DDoS overlay mode:

- New `ddosMode: boolean` prop. When true:
  - Devices involved in active `DdosEvent` nodes get a pulsing red halo overlay (CSS animation, `box-shadow: 0 0 12px var(--state-failed-border)`).
  - Role-specific overlays:
    - `targeted` devices: solid red halo.
    - `amplifier` devices: amber pulsing halo.
    - `transit` devices: grey overlay.
  - Active `AffectedPrefix` prefixes shown as floating label badges near the device with the most traffic exposure.
  - `CARRIES_FLOW` edges with high deviation_score shown as animated dashed lines (CSS keyframe animation on stroke-dashoffset).
  - Control plane stressed devices (`copp_drop_rate > threshold`): CPU icon overlay badge.
- DdosDashboard includes an embedded `<Topology ddosMode={true} />` panel (compact, no full-page navigation).
- Toggle button: "Topology View" / "Timeline View" for the right panel.

**T3 — Mitigation control panel**

Within the Event Detail right panel, **Mitigation tab** includes interactive controls:

- If `MitigationAction` is in `AwaitingHitl` state:
  - Large "APPROVE MITIGATION" button (green, prominent) + "REJECT" (grey).
  - Shows: action description, affected prefix, cloud sink name, dry-run indicator.
  - Confirmation modal: "You are about to instruct [Cloudflare Magic Transit] to divert traffic for [198.51.100.0/24]. This will affect [N] downstream devices. Confirm?"
  - On approve: `POST /api/ddos/mitigation/{id}/approve` + optimistic UI update.
- If state is `Mitigated`:
  - "RESTORE PREFIX" button (amber) — with same HITL modal pattern.
  - Shows: estimated traffic volume restored, cloud sink to notify.
- If state is `Active` and `auto_restore_after_minutes > 0`:
  - Countdown timer: "Auto-restore in [HH:MM:SS]". Operator can click "Restore Now" to skip.
- All control buttons are role-gated: Viewer sees them disabled with tooltip "Requires Operator role".

**T4 — DDoS configuration sub-page**

New sub-page accessible from DdosDashboard "Configure" button:

*Section 1 — Protected Prefixes*:
- Table of configured prefixes with columns: prefix, description, origin_asn, cloud_sink_binding, rtbh_community, auto_rtbh toggle.
- "Add Prefix" modal form.
- "Import from NetBox" button → calls `scripts/import_protected_prefixes.py` equivalent via API.

*Section 2 — Cloud Sinks*:
- Cards for each `[[ddos.cloud_sinks]]` entry showing: provider logo/name, enabled badge, last API call result, API response time.
- "Test Connection" button → `POST /api/ddos/cloud-sinks/{name}/test` (pings provider API with dry-run request).
- Edit form (inline) for min_confidence_to_trigger, require_hitl, auto_restore_after_minutes.

*Section 3 — Thresholds*:
- Sliders + number inputs for: `deviation_score_suspect_threshold` (default 5), `deviation_score_confirm_threshold` (default 10), `min_confidence_for_auto_trigger` (default 0.75), `corroboration_window_seconds` (default 60).
- "Dry Run Mode" toggle — prominently styled with amber warning when enabled.
- Save → `PATCH /api/ddos/config`.

*Section 4 — Baseline Review*:
- Table per device/interface with: current p50/p95/p99, deviation_score now, last_updated.
- Drift alerts shown inline (if `ddos_baseline_drift` detection exists for this device).
- "Acknowledge Drift" button per row.

**T5 — Real-time SSE integration**

Connect DdosDashboard to `GET /api/ddos/events/stream` SSE endpoint:

```javascript
// In DdosDashboard.svelte onMount:
const evtSource = new EventSource('/api/ddos/events/stream');
evtSource.onmessage = (e) => {
    const event = JSON.parse(e.data);
    // Dispatch to local store by event_type:
    // ddos_state_change → update event card state pill + timeline
    // ddos_mitigation_confirmed → update mitigation action confirmed badge
    // ddos_vector_update → update attack vectors tab
    // ddos_baseline_drift → show drift banner
    updateDdosStore(event);
};
```

- Svelte store `ddosStore.js`: `writeable` stores for `activeEvents`, `mitigationActions`, `routeEvents`.
- All UI components reactively derive from store — no polling loops.
- SSE reconnect logic: exponential backoff on connection loss (reuse pattern from Governance.svelte SSE implementation).

**T6 — Incident correlation: DDoS events in existing Incidents page**

Integrate DDoS events into the existing `Incidents.svelte` page:

- New event type filter: "DDoS" toggle alongside existing "Detection", "Remediation" filters.
- DDoS events rendered as a special card variant: attack badge with primary vector, affected prefix chips, confidence score, mitigation status pill.
- Click → opens DdosDashboard right-panel detail in a drawer overlay (reuse `DeviceDrawer.svelte` pattern).
- DDoS events included in ServiceNow incident creation (DS-4 cloud sink integration creates SNOW incidents with DDoS-specific fields: `ddos_event_id`, `affected_prefixes`, `mitigation_status`).

**T7 — Explorer pre-canned DDoS queries**

Extend `Explorer.svelte` pre-canned queries section with DDoS queries:

```
"Active DDoS events with mitigation status" →
MATCH (d:DdosEvent) WHERE d.state <> 'Idle' AND d.state <> 'Restored'
MATCH (d)-[:DDOS_HAS_MITIGATION]->(m:MitigationAction)
RETURN d.id, d.state, d.primary_vector, d.confidence, d.max_observed_gbps, m.action_type, m.confirmation_source

"Traffic baseline deviations > 5× across all devices" →
MATCH (b:TrafficBaseline) WHERE b.deviation_score > 5
MATCH (b)-[:BASELINE_FOR]->(i:Interface)
RETURN i.device_address, i.if_name, b.protocol, b.p95_pps, b.last_value_pps, b.deviation_score ORDER BY b.deviation_score DESC

"BGP RTBH events in last 24 hours" →
MATCH (r:DdosRouteEvent) WHERE r.event_type IN ['rtbh_community_received', 'prefix_restored']
AND r.occurred_at_ns > (timestamp() - 86400000000000)
RETURN r.prefix, r.event_type, r.bgp_community, r.occurred_at_ns ORDER BY r.occurred_at_ns DESC

"Devices acting as amplifiers in confirmed attacks" →
MATCH (d:DdosEvent)-[:DDOS_INVOLVES_DEVICE {role: 'amplifier'}]->(dev:Device)
RETURN dev.address, dev.hostname, count(d) as attack_count ORDER BY attack_count DESC

"Unconfirmed mitigations" →
MATCH (m:MitigationAction) WHERE m.confirmed_at_ns IS NULL
AND m.requested_at_ns < (timestamp() - 300000000000)
RETURN m.id, m.action_type, m.provider, m.target_prefix, m.api_http_status, m.requested_at_ns
```

**T8 — DDoS nav integration**

Wire `DdosDashboard` into `ui/src/App.svelte`:

- Add to NAV array under a new "Protect" section (after "Operate"):
  ```javascript
  { path: '/ddos', label: 'DDoS', icon: 'shield-alert', component: DdosDashboard }
  ```
- Nav group label: "Protect".
- Nav icon: `shield-alert` (Lucide icon, already used in icons.svg).
- Active attack badge on nav item: if `activeEvents.length > 0`, show red dot on nav icon.
- App.svelte `onMount`: poll `GET /api/ddos/events?state=active&limit=1` every 30s to update nav badge count (lightweight polling — SSE is per-page).

---

## DS-8 — DDoS Simulation + Testing Harness

### Analysis

End-to-end DDoS detection testing in a lab environment without actual attack traffic requires signal injection at the telemetry layer — not actual packet floods. The existing `tests/chaos_harness/` and `tests/event_driver/` frameworks provide a pattern for injecting signals via the gRPC API. However, DDoS testing requires injecting:

1. **High-rate metric anomalies** via synthetic gNMI telemetry (pps spike at 50× baseline).
2. **SYN-flood NetFlow records** with `tcp_flags=SYN_ONLY` and many source IPs.
3. **sFlow samples** with UDP dst_port=53 (DNS amplification simulation).
4. **syslog messages** matching CoPP violation and ACL deny flood patterns.
5. **SNMP traps** with ifInDiscards spike.
6. **BMP ROUTE_MONITORING** with RTBH community (simulating upstream response).
7. **Controlled multi-wave attacks**: single-vector → multi-vector → campaign → mitigation → restoration.

The testing harness also needs to validate the **time-to-detect** (TTD) and **time-to-mitigate** (TTM) metrics which are core to the DDoS supplement's value proposition.

### Tasks

**T1 — DDoS signal injection driver**

New file `tests/ddos_harness/inject.py`:

```python
class DdosSignalInjector:
    """
    Injects synthetic DDoS signals into Bonsai via its telemetry ingestion APIs.
    All injections are tagged with injection_id for traceability.
    """
    
    def inject_pps_spike(self, device_address, if_name, spike_multiplier=50, duration_s=60):
        """Inject gNMI-style interface PPS spike via Bonsai telemetry ingest."""
        # POST to /api/telemetry/inject (new endpoint, see T4)
    
    def inject_syn_flood_flows(self, target_prefix, src_count=1000, pps=50000):
        """Inject synthetic NetFlow records with SYN_ONLY flags from diverse sources."""
    
    def inject_dns_amplification_flows(self, target_prefix, amplification_factor=30):
        """Inject sFlow samples with udp_dst_port=53 at above-baseline rate."""
    
    def inject_copp_violation_syslog(self, device_address, class_name, drop_pps=5000):
        """Inject syslog message matching CoPP violation pattern."""
    
    def inject_rtbh_bmp_event(self, prefix, community="65535:666", from_session_id=None):
        """Inject BMP ROUTE_MONITORING UPDATE with RTBH community for prefix."""
    
    def inject_if_discard_snmp_trap(self, device_address, if_index, discard_count=100000):
        """Inject SNMP ifInDiscards trap."""
    
    def simulate_ddos_scenario(self, scenario: DdosScenario):
        """Run a full scenario: sequence of injections with timing."""
```

**T2 — DDoS test scenarios YAML**

New file `tests/ddos_harness/scenarios/`:

- `syn_flood_single_device.yaml`:
  ```yaml
  name: "SYN flood single device — expect ddos_confirmed"
  steps:
    - at_ms: 0
      action: inject_pps_spike
      device: "172.100.109.16"
      if_name: "ethernet-1/1"
      multiplier: 50
    - at_ms: 5000
      action: inject_syn_flood_flows
      target_prefix: "198.51.100.0/24"
      src_count: 500
      pps: 80000
    - at_ms: 8000
      action: inject_copp_violation_syslog
      device: "172.100.109.16"
      drop_pps: 3000
  expect:
    ddos_suspect_within_ms: 10000
    ddos_confirmed_within_ms: 15000
    ddos_confirmed_confidence_min: 0.6
  ```

- `dns_amplification_campaign.yaml` — multi-device DNS amplification from 3 reflectors.
- `ntp_amplification.yaml` — NTP monlist amplification pattern.
- `bgp_hijack_with_rtbh.yaml` — unexpected origin ASN → RTBH response → BMP confirmation.
- `multi_vector_campaign.yaml` — SYN flood + DNS amp + ICMP flood simultaneously from different source ASNs.
- `false_positive_bulk_download.yaml` — high PPS but legitimate: single source IP, TCP ACK-heavy traffic pattern, no SYN-only, no CoPP violations. **Must NOT fire ddos_confirmed.**

**T3 — DDoS harness runner**

New file `tests/ddos_harness/run.py`:

```python
def run_scenario(scenario_file: str, bonsai_url: str, dry_run: bool = False):
    """
    1. Load scenario YAML.
    2. Record baseline metrics.
    3. Execute injection steps with timing.
    4. Wait for expected detections.
    5. Measure TTD (time-to-detect) and TTM (time-to-mitigate if applicable).
    6. Write results to runtime/driver_results/ddos_{scenario_name}.json.
    """

def run_all_scenarios(bonsai_url: str):
    """Run all scenario files + generate summary report."""

def measure_ttd(expected_rule_id: str, injection_start_ns: int, timeout_s: int = 60) -> float:
    """Poll /api/detections until expected rule_id fires, return elapsed seconds."""
```

Outputs:
- Per-scenario JSON with: scenario_name, TTD_seconds, TTM_seconds, detections_fired (list), false_positive_count, confidence_scores.
- Summary markdown to `docs/DDOS_TEST_REPORT.md`.

**T4 — Telemetry injection API endpoint**

New endpoint `POST /api/telemetry/inject` in `src/http_server/observability.rs`:

- **Lab/test use only**: gated on `[lab] enabled = true` in config AND Operator+ role.
- Accepts JSON matching the existing `TelemetryUpdate` structure (already defined in `src/telemetry.rs`).
- Puts the synthetic update directly onto the internal event bus (bypassing the actual gNMI/syslog/NetFlow receivers).
- Full DDoS-relevant update types accepted: `InterfaceCounters`, `ProtocolPacketRate`, `CoppViolation`, `LptsDropStats`, `ForwardingResourcePressure`, `SflowRecord`, `NetflowRecord`, `SyslogFact`, `SnmpFact`.
- Security: injection endpoint is completely disabled in production mode (`[lab] enabled = false`).
- Injection events are tagged with `source_type="synthetic_injection"` and `injection_id` in graph writes for traceability.

**T5 — Time-to-react metrics**

Instrument the detection pipeline to record TTD/TTM:

New properties on `DdosEvent`:
- `injection_id: String` — links to the test injection that triggered this (nullable in production).
- `ttd_ms: Int64` — time from first telemetry anomaly to `ddos_suspect` detection fire.
- `ttc_ms: Int64` — time from first signal to `ddos_confirmed` fire (time-to-confirm).
- `ttm_ms: Int64` — time from `ddos_confirmed` to `MitigationAction.requested_at_ns` (time-to-mitigate).
- `ttv_ms: Int64` — time from mitigation request to BMP confirmation (time-to-verify).

API: `GET /api/ddos/metrics/timing` — aggregated TTD/TTC/TTM/TTV statistics (p50, p95, p99) across all completed DdosEvent nodes. Used for SLA reporting and benchmark tracking.

**T6 — False positive validation suite**

Critical anti-regression tests for false positive scenarios:

New file `tests/ddos_harness/scenarios/false_positive_suite.yaml` covering:

- `bulk_download.yaml`: Single source IP, large TCP flows, ACK-dominant, no CoPP violations → must NOT fire `ddos_confirmed`.
- `bgp_reconvergence_traffic.yaml`: Traffic spike during BGP reconvergence (high pps but short-lived, correlates with `bgp_neighbor_down` detection) → classify as `bgp_reconvergence` not `ddos_suspect`.
- `backup_window.yaml`: Scheduled nightly backup traffic (UDP dst_port=445/SMB or TCP dst_port=22/SSH, consistent source/dest, no port diversity) → must NOT fire.
- `videoconf_burst.yaml`: Sudden UDP video conferencing burst (SSRC-consistent RTP, dst_port=3478 STUN, single source ASN) → must NOT fire.
- `night_batch_job.yaml`: Periodic high-volume TCP connection from single server at 2am (matches baseline weekday pattern at different time window) — demonstrates time-of-day baseline awareness.

Each false positive test PASSES only if no `ddos_suspect` or higher fires within 120s of injection.

**T7 — Ubuntu testing guide integration**

Add DDoS testing phases to the Ubuntu testing guide (`docs/UBUNTU_TESTING_GUIDE.md`):

```
Phase 24: DDoS Signal Extraction Validation
  S-70: Verify TrafficBaseline nodes created for all interfaces
  S-71: Verify gNMI DDoS path profile subscribed on test device
  S-72: Verify syslog DDoS patterns match test messages

Phase 25: DDoS Detection Validation
  S-73: Run syn_flood_single_device scenario — verify ddos_suspect fires < 10s
  S-74: Run syn_flood_single_device — verify ddos_confirmed fires < 15s
  S-75: Run dns_amplification_campaign — verify ddos_campaign fires < 30s
  S-76: Run false_positive_bulk_download — verify NO ddos_confirmed fires

Phase 26: DDoS Response Validation (dry-run only in lab)
  S-77: Verify MitigationAction node created when ddos_confirmed fires
  S-78: Verify API call logged (dry-run) to configured cloud sink
  S-79: Inject rtbh_bmp_event — verify BMP assurance confirms within 30s
  S-80: Verify DdosEvent transitions to Mitigated state after confirmation

Phase 27: BMP Post-Incident Assurance
  S-81: Inject prefix_restored BMP event — verify DdosEvent transitions to Restored
  S-82: Verify BGP audit report generated with correct convergence time
  S-83: Verify MitigationAction.revert_confirmed_at_ns populated
```
