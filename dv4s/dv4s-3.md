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

