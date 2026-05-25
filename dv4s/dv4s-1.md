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

