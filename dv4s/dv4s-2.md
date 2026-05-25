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

