//! MCP (Model Context Protocol) server — T5-1 (CV4 Sprint 5).
//!
//! Exposes bonsai's core read APIs as MCP tools over a JSON-RPC 2.0 endpoint
//! at `POST /mcp`. Read-only. Stateless.
//!
//! ## Protocol
//! - `initialize`   → server capabilities + protocol version
//! - `tools/list`   → catalogue of all available tools
//! - `tools/call`   → invoke a tool by name with typed arguments
//!
//! ## Tools
//! - `get_incident`             — full incident by root detection ID
//! - `query_devices`            — devices matching hostname/address filter
//! - `get_device_blast_radius`  — reachable impact set for a device
//! - `list_active_detections`   — recent detection events with optional filters
//! - `query_graph`              — read-only Cypher passthrough
//!
//! ## Rule catalogue
//! Static map: rule_id → description + recurrence_indicators.
//! Used by T5-2 grounded responses and tool catalogue responses.

use axum::{Json, extract::State, http::StatusCode};
use lbug::{Connection, Value};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

use crate::graph::common::read_str;
use crate::http_server::AppState;

// ── Rule catalogue ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct RuleMeta {
    pub rule_id: &'static str,
    pub description: &'static str,
    pub severity: &'static str,
    pub recurrence_indicators: &'static [&'static str],
}

/// All known detection rules with their recurrence indicators.
/// Mirrors the Python Detector subclasses in bonsai_sdk/rules/*.
pub static RULE_CATALOGUE: &[RuleMeta] = &[
    RuleMeta {
        rule_id: "bgp_session_down",
        description: "BGP session transitions to idle — peer was reset or administratively disabled.",
        severity: "critical",
        recurrence_indicators: &[
            "MATCH (n:BgpNeighbor {device_address: $dev, peer_address: $peer}) RETURN n.session_state — expect 'established' when healthy",
            "Count bgp_session_down DetectionEvents for this device/peer pair in last 24h",
            "Check gNMI subscription status for openconfig-bgp on this device (/api/devices/{address})",
        ],
    },
    RuleMeta {
        rule_id: "bgp_session_flap",
        description: "BGP session has flapped ≥3 times in 5 minutes — unstable neighbour.",
        severity: "critical",
        recurrence_indicators: &[
            "Count bgp_session_flap DetectionEvents for this device/peer pair in last 1h — ≥2 incidents indicates chronic instability",
            "Check for bfd_session_down co-firing within ±5s (indicates routing-layer cause, not BGP policy)",
            "Compare peer_count_established across last 3 bgp_session_flap detections for this device",
        ],
    },
    RuleMeta {
        rule_id: "bgp_all_peers_down",
        description: "All BGP sessions on a device gone simultaneously — likely upstream fault.",
        severity: "critical",
        recurrence_indicators: &[
            "MATCH (n:BgpNeighbor {device_address: $dev}) RETURN n.peer_address, n.session_state — all should be 'established' when healthy",
            "Check for interface_down DetectionEvents on same device within ±30s (hardware-fault co-indicator)",
            "Check blast radius (/api/blast-radius/{address}) — bgp_all_peers_down typically has wide downstream impact",
        ],
    },
    RuleMeta {
        rule_id: "bgp_never_established",
        description: "Peer has been seen for >90s without ever reaching established state.",
        severity: "warn",
        recurrence_indicators: &[
            "Verify path between device and peer exists: GET /api/path?src={device}&dst={peer}",
            "Check BFD session state for this peer — bfd_session_down co-fire means underlay reachability issue",
            "Check DetectionEvent history: if bgp_never_established fires repeatedly, peer config is likely misconfigured",
        ],
    },
    RuleMeta {
        rule_id: "interface_down",
        description: "Interface oper-status transitions to down.",
        severity: "critical",
        recurrence_indicators: &[
            "MATCH (i:Interface {device_address: $dev, name: $if}) RETURN i.oper_status — expect 'up' when healthy",
            "Check CONNECTED_TO edge still present: MATCH (i:Interface {name: $if})-[:CONNECTED_TO]->(j:Interface) RETURN j.device_address",
            "Check for bgp_session_down co-firing on same device within ±10s (upstream propagation indicator)",
        ],
    },
    RuleMeta {
        rule_id: "interface_error_spike",
        description: "Interface error counter rate exceeds 100 errors/s threshold.",
        severity: "warn",
        recurrence_indicators: &[
            "MATCH (i:Interface {device_address: $dev, name: $if}) RETURN i.in_errors, i.out_errors — compare to previous detection's features_json",
            "Check for repeated interface_error_spike on same interface in last 1h (chronic physical-layer issue)",
            "Cross-reference link utilization — high errors under low load indicate physical-layer fault, not congestion",
        ],
    },
    RuleMeta {
        rule_id: "interface_high_utilization",
        description: "Interface octets rate exceeds 80% of known link capacity.",
        severity: "warn",
        recurrence_indicators: &[
            "MATCH (i:Interface {device_address: $dev, name: $if}) RETURN i.in_octets, i.out_octets — rate trend since last detection",
            "Check interface_error_spike co-fire — high util + errors = capacity problem, not just load",
            "Check topology neighbors (/api/topology) for load-balancing or traffic-engineering change as upstream cause",
        ],
    },
    RuleMeta {
        rule_id: "bfd_session_down",
        description: "BFD session transitions from up to down.",
        severity: "critical",
        recurrence_indicators: &[
            "MATCH (b:BfdSession {device_address: $dev}) RETURN b.peer_address, b.state — expect 'up' when healthy",
            "Check interface oper-status on the BFD-protected link — interface_down co-fire indicates physical cause",
            "Check bgp_session_down DetectionEvents within ±5s — BFD down typically precedes BGP down",
        ],
    },
    RuleMeta {
        rule_id: "topology_edge_lost",
        description: "A CONNECTED_TO LLDP edge present in the previous poll cycle is now absent.",
        severity: "warn",
        recurrence_indicators: &[
            "MATCH (a:Interface)-[:CONNECTED_TO]->(b:Interface) RETURN a.device_address, a.name, b.device_address, b.name — verify current LLDP topology",
            "Check interface_down DetectionEvents on both endpoint devices within ±60s",
            "Verify gNMI LLDP subscription active on both devices: GET /api/devices/{address}",
        ],
    },
    RuleMeta {
        rule_id: "route_flap_detected",
        description: "BMP route entry changed ≥3 times in 5 minutes — unstable prefix.",
        severity: "warn",
        recurrence_indicators: &[
            "Count route_flap_detected DetectionEvents for this device/peer/prefix in last 1h",
            "Check BgpNeighbor session state for the flapping peer: MATCH (n:BgpNeighbor {peer_address: $peer}) RETURN n.session_state",
            "Check for route_leak_detected co-firing on same device within same time window",
        ],
    },
    RuleMeta {
        rule_id: "unexpected_as_path",
        description: "BGP AS path contains repeated ASN — possible loop or misconfiguration.",
        severity: "warn",
        recurrence_indicators: &[
            "Compare AS path in features_json against historical BMP RouteMonitoring entries for same prefix",
            "Check bgp_session_down/flap co-fires for the peer announcing this route",
            "Review config-history for BGP policy changes: GET /api/devices/{address}/config-history",
        ],
    },
    RuleMeta {
        rule_id: "route_leak_detected",
        description: "Global prefix propagated with private ASN in path — likely route leak.",
        severity: "critical",
        recurrence_indicators: &[
            "Count route_leak_detected DetectionEvents for this device in last 24h — persistence indicates misconfigured BGP policy",
            "Check if same private ASN appears in other leaked routes on this device within this detection window",
            "Verify AS path against known legitimate paths via BGP-LS topology: MATCH (l:BgpLsLink {device_address: $dev}) RETURN l",
        ],
    },
    RuleMeta {
        rule_id: "sr_policy_degraded",
        description: "SR policy is no longer in an active/up state.",
        severity: "warn",
        recurrence_indicators: &[
            "MATCH (p:SrPolicy {device_address: $dev, name: $name}) RETURN p.status — expect active/up when healthy",
            "Check BgpLsLink state for links along this SR policy's candidate paths",
            "Check IS-IS/OSPF adjacency state for intermediate nodes via DetectionEvent history",
        ],
    },
    RuleMeta {
        rule_id: "srlg_risk_detected",
        description: "Multiple BGP-LS links share the same SRLG — single point of failure risk.",
        severity: "warn",
        recurrence_indicators: &[
            "MATCH (l:BgpLsLink) WHERE l.srlgs_json CONTAINS $srlg RETURN l.local_router_id, l.remote_router_id — enumerate all links sharing this SRLG",
            "Check for interface_down on any link in this SRLG within the last detection window",
            "Review topology for diversity: are there paths not sharing this SRLG? GET /api/path?src=...&dst=...",
        ],
    },
    RuleMeta {
        rule_id: "snmp_cold_warm_start",
        description: "Device sent SNMP cold/warm start trap — indicates restart.",
        severity: "warn",
        recurrence_indicators: &[
            "Check StateChangeEvent count for this device in last 5 min — rapid restarts indicate instability",
            "Verify gNMI subscription reconnected after restart: GET /api/devices/{address} subscription_statuses",
            "Check YANG capabilities repopulated after restart: GET /api/yang/modules?device={address}",
        ],
    },
    RuleMeta {
        rule_id: "snmp_auth_failure_burst",
        description: "SNMP authentication failures reached ≥3 traps in 5 minutes.",
        severity: "critical",
        recurrence_indicators: &[
            "Count snmp_auth_failure_burst DetectionEvents per source IP in last 24h — cross-device pattern indicates scanning",
            "Check if same source IP appears in auth-failure events across multiple devices (lateral movement indicator)",
            "Review SNMPv3 credential rotation schedule — burst after credential change indicates stale client config",
        ],
    },
    RuleMeta {
        rule_id: "snmp_environmental_threshold_breach",
        description: "SNMP environmental trap (PSU/temperature/fan/voltage) received.",
        severity: "critical",
        recurrence_indicators: &[
            "Check platform health via gNMI openconfig-platform path on this device",
            "Check for interface_down or bgp_session_down co-firing within ±5min of this detection (thermal impact cascade)",
            "Count snmp_environmental_threshold_breach events for this device in last 24h — repeated events indicate cooling failure",
        ],
    },
    RuleMeta {
        rule_id: "snmp_fru_failure",
        description: "SNMP FRU/linecard/module failure trap received.",
        severity: "critical",
        recurrence_indicators: &[
            "MATCH (c:Component {device_address: $dev}) RETURN c.name, c.state — check platform component inventory via gNMI",
            "Check for interface_down on interfaces served by the failed FRU within ±30s",
            "Check bgp_session_down on sessions using adjacencies on the affected linecard",
        ],
    },
    // ── CV7 follow-up: syslog + correlation rules (13 entries).
    // Reconciled with python/bonsai_sdk/rules/syslog.py so /api/sidecars
    // capabilities match the canonical catalogue. Surfaced in grounded
    // incident responses for rule_ids fired by the rules sidecar.
    RuleMeta {
        rule_id: "syslog_auth_failure_cluster",
        description: "≥3 syslog authentication-failure events from the same device in 5 minutes.",
        severity: "warn",
        recurrence_indicators: &[
            "Count syslog_auth StateChangeEvents for this device in last 24h — expect 0 when healthy",
            "MATCH (d:Device {address: $dev}) RETURN d.address — check device is still reachable",
            "Check /api/detections?device={address}&rule=syslog_auth_failure_cluster for prior detections in last 7 days",
        ],
    },
    RuleMeta {
        rule_id: "syslog_hardware_error",
        description: "Syslog hardware-category message contains failure/error/alarm/critical/down tokens.",
        severity: "critical",
        recurrence_indicators: &[
            "Count syslog_hardware StateChangeEvents for this device in last 24h — expect 0 when healthy",
            "MATCH (d:Device {address: $dev}) RETURN d.address, d.vendor — check device is still reachable",
            "Check /api/detections?device={address}&rule=syslog_hardware_error for prior hardware errors in last 7 days",
        ],
    },
    RuleMeta {
        rule_id: "syslog_software_crash",
        description: "Syslog software-category message contains crash/panic/core/restart tokens.",
        severity: "critical",
        recurrence_indicators: &[
            "Count syslog_software StateChangeEvents for this device in last 24h — pattern of crashes suggests systemic instability",
            "MATCH (d:Device {address: $dev}) RETURN d.address, d.vendor — check device uptime via gNMI /system/state/boot-time",
            "Check /api/detections?device={address}&rule=syslog_software_crash for prior crash events in last 30 days",
        ],
    },
    RuleMeta {
        rule_id: "syslog_license_expiry",
        description: "Syslog license-category message contains expire/expired/expiry/license tokens.",
        severity: "warn",
        recurrence_indicators: &[
            "Count syslog_license StateChangeEvents for this device in last 7 days — repeated warnings indicate expiry approaching",
            "MATCH (d:Device {address: $dev}) RETURN d.vendor — license format varies by vendor; check vendor portal",
            "Check /api/detections?device={address}&rule=syslog_license_expiry for first occurrence date to estimate urgency",
        ],
    },
    RuleMeta {
        rule_id: "syslog_protocol_error",
        description: "Syslog protocol-category message indicates down/flap/lost/reset/mismatch.",
        severity: "warn",
        recurrence_indicators: &[
            "Count syslog_protocol StateChangeEvents for this device in last 1h — flapping indicates instability",
            "MATCH (n:BgpNeighbor {device_address: $dev}) RETURN n.peer_address, n.session_state — correlate with BGP state",
            "Check /api/detections?device={address}&rule=syslog_protocol_error for recurrence frequency",
        ],
    },
    RuleMeta {
        rule_id: "syslog_bpduguard_activation",
        description: "BPDUGuard activated on an interface — port disabled by spanning-tree.",
        severity: "critical",
        recurrence_indicators: &[
            "Count syslog_protocol BPDUGuard events for this device in last 24h — repeated activations suggest upstream loop or misconfigured device",
            "MATCH (i:Interface {device_address: $dev}) RETURN i.name, i.in_pkts — identify the affected port",
            "Check /api/detections?device={address}&rule=syslog_bpduguard_activation for prior activations on same device",
        ],
    },
    RuleMeta {
        rule_id: "syslog_stp_topology_change",
        description: "Spanning-tree topology change reported in syslog.",
        severity: "warn",
        recurrence_indicators: &[
            "Count syslog_protocol STP topology-change events for this device in last 1h — multiple changes indicate instability",
            "MATCH (i:Interface {device_address: $dev}) RETURN i.name, i.in_pkts, i.out_pkts — check for interface flap correlation",
            "Check /api/detections?device={address}&rule=syslog_stp_topology_change for recurrence pattern",
        ],
    },
    RuleMeta {
        rule_id: "syslog_gnmi_disagreement",
        description: "Syslog BGP state disagrees with the gNMI-derived graph state for the same neighbour.",
        severity: "warn",
        recurrence_indicators: &[
            "MATCH (n:BgpNeighbor {device_address: $dev, peer_address: $peer}) RETURN n.session_state — expect 'established' when healthy",
            "Count syslog_fact_joined bgp_neighbor disagreement events for this peer in last 24h",
            "Check /api/detections?device={address}&rule=syslog_gnmi_disagreement for recurrence — persistent disagreement may indicate stale gNMI subscription",
        ],
    },
    RuleMeta {
        rule_id: "syslog_bfd_disagreement",
        description: "Syslog BFD state disagrees with the gNMI-derived graph state for the same session.",
        severity: "warn",
        recurrence_indicators: &[
            "MATCH (b:BfdSession {device_address: $dev}) RETURN b.remote_address, b.session_state, b.if_name — expect 'up' when healthy",
            "Count syslog_fact_joined bfd_session disagreement events for this device in last 24h",
            "Check /api/detections?device={address}&rule=syslog_bfd_disagreement — persistent disagreement may indicate gNMI path misconfiguration for BFD",
        ],
    },
    RuleMeta {
        rule_id: "syslog_config_change_cluster",
        description: "≥3 config_change_detail facts arrived from the same device within 10 minutes.",
        severity: "warn",
        recurrence_indicators: &[
            "Count syslog config_change_detail facts for this device in last 1h — cluster in short window signals automation gone wrong",
            "MATCH (d:Device {address: $dev}) RETURN d.address, d.vendor — check if device is under active maintenance window",
            "Check /api/detections?device={address}&rule=syslog_config_change_cluster for prior change clusters to identify automation frequency",
        ],
    },
    RuleMeta {
        rule_id: "syslog_hardware_interface_correlation",
        description: "Interface_state down fact follows a hardware_error on the same device within 60s.",
        severity: "critical",
        recurrence_indicators: &[
            "Count syslog_hardware StateChangeEvents for this device in last 1h — repeated hardware errors without interface recovery indicate physical fault",
            "MATCH (i:Interface {device_address: $dev}) RETURN i.name, i.in_errors, i.out_errors — check error counters on affected interfaces",
            "Check /api/detections?device={address}&rule=syslog_hardware_interface_correlation for prior correlations — PSU/fan faults recur until replaced",
        ],
    },
    RuleMeta {
        rule_id: "orphan_interface_mention",
        description: "Syslog references an interface name not present in the gNMI-managed graph state.",
        severity: "warn",
        recurrence_indicators: &[
            "MATCH (i:Interface {device_address: $dev}) RETURN i.name — verify syslog interface name matches gNMI subscription naming convention",
            "Count syslog_fact_orphan interface_state events for this device in last 24h — repeated orphans suggest monitoring gap",
            "Check /api/devices/{address} to confirm subscription paths cover the interface mentioned in syslog",
        ],
    },
    RuleMeta {
        rule_id: "multi_source_correlation",
        description: "Successful join of a syslog fact (bgp_neighbor or interface_state) with current graph state.",
        severity: "info",
        recurrence_indicators: &[
            "Count syslog_fact_joined events for this device in last 24h — rising join rate validates telemetry pipeline health",
            "MATCH (d:Device {address: $dev}) RETURN d.address, d.vendor — check device is active and subscriptions are running",
            "Check /api/detections?device={address}&rule=multi_source_correlation to verify cross-source coverage is stable",
        ],
    },
];

/// Lookup a rule by ID. O(n) over the catalogue (31 rules, negligible cost).
pub fn rule_meta(rule_id: &str) -> Option<&'static RuleMeta> {
    RULE_CATALOGUE.iter().find(|r| r.rule_id == rule_id)
}

// ── JSON-RPC 2.0 request / response types ────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct McpRpcRequest {
    #[serde(default)]
    pub jsonrpc: String,
    pub id: Option<JsonValue>,
    pub method: String,
    #[serde(default)]
    pub params: JsonValue,
}

#[derive(Debug, Serialize)]
pub struct McpRpcResponse {
    jsonrpc: &'static str,
    id: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<McpRpcError>,
}

#[derive(Debug, Serialize)]
pub struct McpRpcError {
    code: i32,
    message: String,
}

impl McpRpcResponse {
    fn ok(id: JsonValue, result: JsonValue) -> Self {
        McpRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }
    fn err(id: JsonValue, code: i32, message: impl Into<String>) -> Self {
        McpRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(McpRpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

fn mcp_content(value: JsonValue) -> JsonValue {
    json!({ "content": [{ "type": "text", "text": value.to_string() }] })
}

// ── Tool schema definitions ───────────────────────────────────────────────────

pub const MAX_GRAPH_ROWS: usize = 200;
pub const MAX_TOOL_OUTPUT_BYTES: usize = 65_536;
pub const TOOL_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub fn tool_schemas() -> JsonValue {
    json!({
        "tools": [
            {
                "name": "get_incident",
                "description": "Return a full incident by its root DetectionEvent ID, including cascading detections, affected devices, severity, and grounded context.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Root DetectionEvent ID (UUID)" }
                    },
                    "required": ["id"]
                }
            },
            {
                "name": "query_devices",
                "description": "Return devices matching an optional hostname or address filter. Returns address, hostname, vendor, health, and current BGP session states.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "filter": { "type": "string", "description": "Substring match against hostname or address. Omit to return all devices." }
                    }
                }
            },
            {
                "name": "get_device_blast_radius",
                "description": "Return all devices reachable from a given address within max_hops physical hops, plus applications and active detections in the reachable set.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "address": { "type": "string", "description": "Device IP address" },
                        "max_hops": { "type": "integer", "description": "Maximum physical hops (default 2, max 5)", "default": 2 }
                    },
                    "required": ["address"]
                }
            },
            {
                "name": "list_active_detections",
                "description": "Return recent DetectionEvents with optional filters. Returns detection ID, device, rule ID, severity, fired timestamp, and remediation status.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "description": "Maximum detections to return (default 50)", "default": 50 },
                        "severity": { "type": "string", "description": "Filter by severity: 'critical', 'warn', or 'info'. Omit for all." },
                        "rule_id": { "type": "string", "description": "Filter by rule ID. Omit for all rules." },
                        "device_address": { "type": "string", "description": "Filter by device address. Omit for all devices." }
                    }
                }
            },
            {
                "name": "query_graph",
                "description": "Execute a read-only Cypher query against the bonsai graph database (bounded to 200 rows and 64 KiB). Mutation keywords are rejected.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cypher": { "type": "string", "description": "Read-only Cypher query string" }
                    },
                    "required": ["cypher"]
                }
            },
            {
                "name": "get_prefix_journey",
                "description": "Return scoped route visibility, view divergence (AdjRibInPrePolicy, AdjRibInPostPolicy, LocRib, AdjRibOutPostPolicy), attribute transitions, and canonical observation envelopes for a prefix and optional VRF/speaker.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "prefix": { "type": "string", "description": "IPv4 or IPv6 CIDR prefix (e.g. '203.0.113.0/24')" },
                        "vrf_rd": { "type": "string", "description": "Optional Route Distinguisher / VRF scope (e.g. '0:0' or '65001:100')" },
                        "speaker": { "type": "string", "description": "Optional BMP speaker IP address" },
                        "view": { "type": "string", "description": "Optional RIB view filter" },
                        "limit": { "type": "integer", "description": "Maximum observations to return (default 50, max 200)", "default": 50 },
                        "tenant_id": { "type": "string", "description": "Optional tenant scope identifier" }
                    },
                    "required": ["prefix"]
                }
            },
            {
                "name": "get_peer_timeline",
                "description": "Return session epochs, BGP peer state transitions (PeerUp, PeerDown, EndOfRib), route-count summaries, and collector transport disconnect gaps.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "speaker": { "type": "string", "description": "Optional BMP speaker IP address" },
                        "peer": { "type": "string", "description": "Optional BGP peer IP address" },
                        "vrf_rd": { "type": "string", "description": "Optional VRF Route Distinguisher scope" },
                        "limit": { "type": "integer", "description": "Maximum transitions to return (default 100, max 500)", "default": 100 },
                        "tenant_id": { "type": "string", "description": "Optional tenant scope identifier" }
                    }
                }
            },
            {
                "name": "get_evidence_coverage",
                "description": "Return incident-scoped evidence completeness, observed vs missing BMP views, End-of-RIB status, and transport gaps for a specific speaker/VRF/prefix.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "speaker": { "type": "string", "description": "Target BMP speaker / router IP address" },
                        "vrf_rd": { "type": "string", "description": "Target VRF Route Distinguisher (e.g. '0:0' or '65001:100')" },
                        "prefix": { "type": "string", "description": "Target CIDR prefix" },
                        "tenant_id": { "type": "string", "description": "Optional tenant scope identifier" }
                    }
                }
            },
            {
                "name": "get_changes",
                "description": "Return recent configuration snapshots and change records scoped to a device address and bounded limit.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "device_address": { "type": "string", "description": "Target device IP address or hostname" },
                        "limit": { "type": "integer", "description": "Maximum change records to return (default 25, max 100)", "default": 25 }
                    }
                }
            },
            {
                "name": "compare_snapshots",
                "description": "Deterministically compare routing state between two durable RibEventJournal sequence numbers (before_seq and after_seq) with observation ID citations.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "before_seq": { "type": "integer", "description": "Earlier durable sequence number" },
                        "after_seq": { "type": "integer", "description": "Later durable sequence number" },
                        "speaker": { "type": "string", "description": "Optional BMP speaker IP filter" },
                        "vrf_rd": { "type": "string", "description": "Optional VRF Route Distinguisher filter" },
                        "prefix": { "type": "string", "description": "Optional CIDR prefix filter" },
                        "limit": { "type": "integer", "description": "Maximum diffs per category (default 100, max 500)", "default": 100 }
                    }
                }
            },
            {
                "name": "explain_missing_prefix",
                "description": "Run the grounded read-only 'Explain a Missing Prefix' investigator for a prefix and VRF/speaker, returning observed facts, ranked hypotheses, evidence citations, missing observations, and next discriminating check.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "prefix": { "type": "string", "description": "Target CIDR prefix (e.g. '203.0.113.0/24')" },
                        "vrf_rd": { "type": "string", "description": "Target VRF Route Distinguisher (default '0:0')" },
                        "speaker": { "type": "string", "description": "Target BMP speaker / router IP address" },
                        "question": { "type": "string", "description": "Optional operator question" },
                        "tenant_id": { "type": "string", "description": "Optional tenant scope identifier" }
                    },
                    "required": ["prefix"]
                }
            },
            {
                "name": "evaluate_change_plan",
                "description": "Evaluate a typed RemediationPlan against a DigitalTwinSnapshot before human approval, returning plan_hash, invariant results, unsupported constructs, and emulation fidelity limitations.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "plan": { "type": "object", "description": "Typed RemediationPlan JSON object" },
                        "digital_twin_snapshot": { "type": "object", "description": "DigitalTwinSnapshot JSON object" }
                    },
                    "required": ["plan", "digital_twin_snapshot"]
                }
            }
        ]
    })
}

// ── Tool implementations ──────────────────────────────────────────────────────

async fn tool_get_incident(
    store: &std::sync::Arc<crate::graph::GraphStore>,
    args: &JsonValue,
) -> Result<JsonValue, String> {
    let id = args["id"]
        .as_str()
        .ok_or("missing argument: id")?
        .to_string();

    let detections = store
        .read_detections_filtered(1, Some(id.clone()), None, None, None)
        .await
        .map_err(|e| e.to_string())?;

    let root = detections
        .into_iter()
        .find(|d| d.id == id)
        .ok_or_else(|| format!("DetectionEvent {id} not found"))?;

    if let Some(spk) = args.get("_scoped_speaker").and_then(|v| v.as_str()) {
        if root.device_address != spk {
            return Err(
                "routing_scope_violation: requested detection is outside authenticated scope"
                    .to_string(),
            );
        }
    }

    let mut blast = {
        let db = store.db();
        let addr = root.device_address.clone();
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            crate::graph::queries::blast_radius(&conn, &addr, 2).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())??
    };

    if args
        .get("_scoped_speaker")
        .and_then(|v| v.as_str())
        .is_some()
    {
        blast
            .reachable_devices
            .retain(|d| d.address == root.device_address);
        blast.neighbor_apps.clear();
        blast.active_detections.clear();
    }

    let rule = rule_meta(&root.rule_id);

    Ok(json!({
        "id": root.id,
        "device_address": root.device_address,
        "rule_id": root.rule_id,
        "severity": root.severity,
        "fired_at_ns": root.fired_at_ns,
        "remediation_status": root.remediation_status,
        "blast_radius": blast,
        "rule_description": rule.map(|r| r.description).unwrap_or(""),
        "recurrence_indicators": rule.map(|r| r.recurrence_indicators).unwrap_or(&[])
    }))
}

async fn tool_query_devices(
    store: &std::sync::Arc<crate::graph::GraphStore>,
    args: &JsonValue,
) -> Result<JsonValue, String> {
    let filter = args["filter"].as_str().unwrap_or("").to_lowercase();
    let scoped_speaker = args.get("_scoped_speaker").and_then(|v| v.as_str());

    let scope_address = scoped_speaker.map(String::from);
    let query_filter = filter.clone();
    let db = store.db();
    let raw: Vec<(String, String, String)> = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let mut stmt = conn.prepare(
            "MATCH (d:Device) WHERE ($scope = '' OR d.address = $scope) AND \
             ($filter = '' OR lower(d.address) CONTAINS $filter OR lower(d.hostname) CONTAINS $filter) \
             RETURN d.address, d.hostname, d.vendor LIMIT 501"
        ).map_err(|e| e.to_string())?;
        let rows: Vec<_> = conn.execute(&mut stmt, vec![
            ("scope", Value::String(scope_address.unwrap_or_default())),
            ("filter", Value::String(query_filter)),
        ]).map_err(|e| e.to_string())?
            .map(|row| (read_str(&row[0]), read_str(&row[1]), read_str(&row[2])))
            .collect();
        Ok::<_, String>(rows)
    })
    .await
    .map_err(|e| e.to_string())??;

    let truncated = raw.len() > 500;
    let devices: Vec<JsonValue> = raw
        .into_iter().take(500)
        .filter(|(addr, host, _)| {
            if let Some(spk) = scoped_speaker {
                if addr != spk {
                    return false;
                }
            }
            filter.is_empty()
                || addr.to_lowercase().contains(&filter)
                || host.to_lowercase().contains(&filter)
        })
        .map(|(address, hostname, vendor)| {
            json!({ "address": address, "hostname": hostname, "vendor": vendor })
        })
        .collect();

    let count = devices.len();
    Ok(json!({ "devices": devices, "count": count, "truncated": truncated, "row_cap": 500 }))
}

async fn tool_get_device_blast_radius(
    store: &std::sync::Arc<crate::graph::GraphStore>,
    args: &JsonValue,
) -> Result<JsonValue, String> {
    let address = args["address"]
        .as_str()
        .or_else(|| args["device_address"].as_str())
        .or_else(|| args["_scoped_speaker"].as_str())
        .ok_or("missing argument: address")?
        .to_string();
    let restricted = args
        .get("_scoped_speaker")
        .and_then(|v| v.as_str())
        .map(String::from);
    let max_hops = args["max_hops"].as_u64().unwrap_or(2).min(5) as usize;

    let db = store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::queries::blast_radius(&conn, &address, max_hops).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    let mut result = result;
    if let Some(speaker) = restricted {
        result.reachable_devices.retain(|d| d.address == speaker);
        // Neighbour applications/detections have no ownership fields in this result.
        result.neighbor_apps.clear();
        result.active_detections.clear();
    }
    serde_json::to_value(&result).map_err(|e| e.to_string())
}

async fn tool_list_active_detections(
    store: &std::sync::Arc<crate::graph::GraphStore>,
    args: &JsonValue,
) -> Result<JsonValue, String> {
    let limit = args["limit"].as_u64().unwrap_or(50).min(500) as u32;
    let severity_filter = args["severity"].as_str().unwrap_or("").to_string();
    let rule_filter = args["rule_id"].as_str().unwrap_or("").to_string();
    let device_filter = args["device_address"]
        .as_str()
        .or_else(|| args["address"].as_str())
        .or_else(|| args["_scoped_speaker"].as_str())
        .unwrap_or("")
        .to_string();

    let detections = store
        .read_detections_filtered(
            limit,
            None,
            Some(device_filter.clone()),
            Some(severity_filter.clone()),
            Some(rule_filter.clone()),
        )
        .await
        .map_err(|e| e.to_string())?;

    let filtered: Vec<JsonValue> = detections
        .into_iter()
        .filter(|d| {
            (severity_filter.is_empty() || d.severity == severity_filter)
                && (rule_filter.is_empty() || d.rule_id == rule_filter)
                && (device_filter.is_empty() || d.device_address == device_filter)
        })
        .map(|d| {
            json!({
                "id": d.id,
                "device_address": d.device_address,
                "rule_id": d.rule_id,
                "severity": d.severity,
                "fired_at_ns": d.fired_at_ns,
                "remediation_status": d.remediation_status,
                "remediation_action": d.remediation_action
            })
        })
        .collect();

    let count = filtered.len();
    Ok(json!({ "detections": filtered, "count": count }))
}

/// Returns `false` if `cypher` contains a mutation keyword that should never
/// appear in a read-only query.
///
/// **Hardening strategy (C4-N3 / T6-3)**:
/// 1. Strip single-line comments (`--` and `//` style) and block comments
///    (`/* */`) before scanning — otherwise a comment containing `SET` would
///    produce a false-positive rejection.
/// 2. Use word-boundary detection: keyword must be preceded by a non-word char
///    (or start-of-string) and followed by a non-alphanumeric/non-underscore
///    char (or end-of-string). This prevents `OFFSET` from matching `SET` and
///    prevents variable names containing keyword substrings from tripping the gate.
/// 3. Extended keyword list includes `DROP`, `LOAD`, and `CALL` (which can
///    invoke write-capable procedures in Kuzu).
///
/// Lexical defence in depth; execution also uses a native read-only transaction.
pub fn is_readonly_cypher(cypher: &str) -> bool {
    crate::graph::explorer::validate_query(cypher).is_ok()
}

/// Enforce a hard byte ceiling (`MAX_TOOL_OUTPUT_BYTES`) and attach `as_of` / `truncated` metadata (A04).
pub fn enforce_tool_output_bounds(mut payload: JsonValue) -> JsonValue {
    if let Some(obj) = payload.as_object_mut() {
        obj.entry("as_of".to_string())
            .or_insert_with(|| json!(chrono::Utc::now().to_rfc3339()));
        obj.entry("truncated".to_string())
            .or_insert_with(|| json!(false));
        obj.insert("byte_cap".to_string(), json!(MAX_TOOL_OUTPUT_BYTES));
    }
    let serialized = payload.to_string();
    if serialized.len() <= MAX_TOOL_OUTPUT_BYTES {
        return payload;
    }
    // Bound the serialized envelope, including UTF-8 and JSON escaping.
    let mut result = json!({
        "as_of": chrono::Utc::now().to_rfc3339(),
        "truncated": true,
        "byte_cap": MAX_TOOL_OUTPUT_BYTES,
        "original_bytes": serialized.len(),
        "truncated_preview": "",
    });
    let overhead = result.to_string().len();
    let mut remaining = MAX_TOOL_OUTPUT_BYTES.saturating_sub(overhead);
    let mut preview = String::new();
    for ch in serialized.chars() {
        let cost = match ch {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{0008}' | '\u{000c}' => 2,
            c if c <= '\u{001f}' => 6,
            c => c.len_utf8(),
        };
        if cost > remaining {
            break;
        }
        remaining -= cost;
        preview.push(ch);
    }
    result["truncated_preview"] = json!(preview);
    result
}

async fn tool_query_graph_with_store(
    store: &std::sync::Arc<crate::graph::GraphStore>,
    args: &JsonValue,
) -> Result<JsonValue, String> {
    let cypher = args["cypher"].as_str().ok_or("missing argument: cypher")?;

    if !is_readonly_cypher(cypher) {
        return Err("mutation keywords not permitted in read-only query_graph tool".to_string());
    }

    let result = crate::graph::explorer::execute_query_with_deadline(
        store.db(),
        cypher.to_string(),
        TOOL_QUERY_TIMEOUT,
    )
    .await?;
    let engine_truncated = result.truncated;
    let rows: Vec<Vec<String>> = result
        .rows
        .into_iter()
        .take(MAX_GRAPH_ROWS + 1)
        .map(|row| row.into_iter().map(|v| v.to_string()).collect())
        .collect();

    let total_rows = rows.len();
    let truncated = engine_truncated || total_rows > MAX_GRAPH_ROWS;
    let capped_rows: Vec<Vec<String>> = rows.into_iter().take(MAX_GRAPH_ROWS).collect();
    let row_count = capped_rows.len();

    Ok(enforce_tool_output_bounds(json!({
        "rows": capped_rows,
        "row_count": row_count,
        "total_rows": total_rows,
        "row_cap": MAX_GRAPH_ROWS,
        "truncated": truncated,
    })))
}

async fn tool_get_changes_with_store(
    store: &std::sync::Arc<crate::graph::GraphStore>,
    args: &JsonValue,
) -> Result<JsonValue, String> {
    let device_filter = args["device_address"]
        .as_str()
        .or_else(|| args["address"].as_str())
        .or_else(|| args["_scoped_speaker"].as_str())
        .map(|s| s.to_string());
    let limit = args["limit"].as_u64().unwrap_or(25).clamp(1, 100) as usize;
    let db = store.db();
    let dev_clone = device_filter.clone();

    let snapshots: Vec<JsonValue> = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let rows: Vec<JsonValue> = conn
            .query("MATCH (c:ConfigSnapshot) RETURN c.id, c.device_address, c.trigger, c.summary, c.captured_at")
            .map_err(|e| e.to_string())?
            .filter_map(|row| {
                let id = read_str(&row[0]);
                let dev = read_str(&row[1]);
                let trig = read_str(&row[2]);
                let sum = read_str(&row[3]);
                let cap = read_str(&row[4]);
                if let Some(ref f) = dev_clone {
                    if dev != *f {
                        return None;
                    }
                }
                Some(json!({
                    "snapshot_id": id,
                    "device_address": dev,
                    "trigger": trig,
                    "summary": sum,
                    "captured_at": cap,
                }))
            })
            .collect();
        Ok::<_, String>(rows)
    })
    .await
    .map_err(|e| e.to_string())??;

    let total = snapshots.len();
    let truncated = total > limit;
    let items: Vec<JsonValue> = snapshots.into_iter().take(limit).collect();

    Ok(enforce_tool_output_bounds(json!({
        "scope": { "device_address": device_filter },
        "changes": items,
        "count": items.len(),
        "total_matching": total,
        "truncated": truncated,
    })))
}

async fn resolve_in_process_routing_state(
    store: &crate::graph::GraphStore,
    explicit: Option<&std::sync::Arc<crate::routing_service::RoutingServiceState>>,
) -> Option<std::sync::Arc<crate::routing_service::RoutingServiceState>> {
    if let Some(st) = explicit {
        return Some(std::sync::Arc::clone(st));
    }
    let (_, _, has_explicit_db_url) =
        crate::routing_service::resolve_routing_service_endpoint_config(store).await;
    if has_explicit_db_url {
        return None;
    }
    crate::routing_service::default_routing_service()
}

async fn fetch_remote_routing_tool_json(
    store: &crate::graph::GraphStore,
    internal_subpath: &str,
    query_pairs: &[(&str, Option<String>)],
) -> Result<JsonValue, String> {
    let (base_url, token, _) =
        crate::routing_service::resolve_routing_service_endpoint_config(store).await;
    let url = format!("{base_url}/internal/routing/v1/{internal_subpath}");
    let client = reqwest::Client::builder()
        .timeout(TOOL_QUERY_TIMEOUT)
        .build()
        .map_err(|e| format!("failed to build routing HTTP client: {e}"))?;
    let params: Vec<(&str, String)> = query_pairs
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|val| (*k, val.clone())))
        .collect();
    let mut req = client.get(&url).query(&params);
    if let Some(t) = token {
        req = req.header("x-bonsai-routing-token", t);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("routing service HTTP request failed ({url}): {e}"))?;
    let status = resp.status();
    let mut body: JsonValue = resp
        .json()
        .await
        .map_err(|e| format!("invalid JSON from routing service ({url}): {e}"))?;
    crate::http_server::redact_secrets_in_json(&mut body);
    if !status.is_success() {
        let msg = body
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("remote routing tool error");
        return Err(format!("routing service HTTP {status}: {msg}"));
    }
    Ok(body)
}

/// Dispatch a single MCP tool call using `GraphStore` and either an in-process `RoutingServiceState`
/// or the private `bonsai-routing` HTTP service (`R05`, `A04`).
/// Trusted server-side identity. Never deserialized from tool JSON.
#[derive(Clone, Debug)]
pub struct ToolExecutionContext {
    pub role: crate::http_server::auth::Role,
    pub scope: Option<crate::http_server::auth::RoutingAccessScope>,
}

impl ToolExecutionContext {
    pub fn for_device(device: &str) -> Self {
        Self {
            role: crate::http_server::auth::Role::ApiReadonly,
            scope: Some(crate::http_server::auth::RoutingAccessScope {
                speaker: Some(device.to_string()),
                ..Default::default()
            }),
        }
    }
    /// Explicit privileged service/global identity for trusted internal callers.
    pub fn global(role: crate::http_server::auth::Role) -> Self {
        Self { role, scope: None }
    }
}

/// The shared execution boundary for MCP, prefetched context and agent tool calls.
pub async fn call_tool_with_store_and_routing(
    store: &std::sync::Arc<crate::graph::GraphStore>,
    routing: Option<&std::sync::Arc<crate::routing_service::RoutingServiceState>>,
    name: &str,
    args: &JsonValue,
    context: &ToolExecutionContext,
) -> Result<JsonValue, String> {
    let mut scoped_args = args.clone();
    let mode = store.ownership_access_mode().await;
    authorize_and_scope_tool_with_mode(
        name,
        &mut scoped_args,
        context.scope.as_ref(),
        Some(&context.role),
        mode,
    )?;
    let mut result = dispatch_authorized_tool(store, routing, name, &scoped_args).await?;
    if mode == crate::graph::ownership::OwnershipAccessMode::AuditOnly
        && context
            .scope
            .as_ref()
            .is_some_and(|s| s.tenant_id.is_some() || s.vrf_rd.is_some())
        && name != "evaluate_change_plan"
    {
        if let Some(obj) = result.as_object_mut() {
            obj.insert(
                "ownership_access".into(),
                json!({"mode":"audit_only", "tenant_isolation_enforced":false,
                "unsupported_vrf_ownership_enforced":false, "speaker_constraints_retained":true}),
            );
        }
        result = enforce_tool_output_bounds(result);
    }
    Ok(result)
}

async fn dispatch_authorized_tool(
    store: &std::sync::Arc<crate::graph::GraphStore>,
    routing: Option<&std::sync::Arc<crate::routing_service::RoutingServiceState>>,
    name: &str,
    args: &JsonValue,
) -> Result<JsonValue, String> {
    use crate::routing_service::{
        CompareSnapshotsQuery, EvidenceCoverageQuery, ExplainPrefixQuery, PeerTimelineQuery,
        PrefixJourneyQuery, build_compare_snapshots_json, build_evidence_coverage_json,
        build_explain_prefix_json, build_peer_timeline_json, build_prefix_journey_json,
    };

    match name {
        "get_incident" => tool_get_incident(store, args)
            .await
            .map(enforce_tool_output_bounds),
        "query_devices" => tool_query_devices(store, args)
            .await
            .map(enforce_tool_output_bounds),
        "get_device_blast_radius" => tool_get_device_blast_radius(store, args)
            .await
            .map(enforce_tool_output_bounds),
        "list_active_detections" => tool_list_active_detections(store, args)
            .await
            .map(enforce_tool_output_bounds),
        "query_graph" => tool_query_graph_with_store(store, args).await,
        "get_changes" => tool_get_changes_with_store(store, args).await,
        "get_prefix_journey" => {
            let prefix = args["prefix"]
                .as_str()
                .ok_or("missing argument: prefix")?
                .to_string();
            let q = PrefixJourneyQuery {
                prefix: Some(prefix.clone()),
                vrf_rd: args["vrf_rd"].as_str().map(String::from),
                speaker: args["speaker"].as_str().map(String::from),
                view: args["view"].as_str().map(String::from),
                limit: args["limit"].as_u64().map(|v| v as usize),
                tenant_id: args["tenant_id"].as_str().map(String::from),
            };
            let val = if let Some(st) = resolve_in_process_routing_state(store, routing).await {
                let (_, v) = build_prefix_journey_json(&st, &q).await;
                v
            } else {
                fetch_remote_routing_tool_json(
                    store,
                    "prefix-journey",
                    &[
                        ("prefix", Some(prefix)),
                        ("vrf_rd", q.vrf_rd),
                        ("speaker", q.speaker),
                        ("view", q.view),
                        ("limit", q.limit.map(|n| n.to_string())),
                        ("tenant_id", q.tenant_id),
                    ],
                )
                .await?
            };
            Ok(enforce_tool_output_bounds(val))
        }
        "get_peer_timeline" => {
            let q = PeerTimelineQuery {
                speaker: args["speaker"].as_str().map(String::from),
                peer: args["peer"].as_str().map(String::from),
                vrf_rd: args["vrf_rd"].as_str().map(String::from),
                limit: args["limit"].as_u64().map(|v| v as usize),
                tenant_id: args["tenant_id"].as_str().map(String::from),
            };
            let val = if let Some(st) = resolve_in_process_routing_state(store, routing).await {
                build_peer_timeline_json(&st, &q).await
            } else {
                fetch_remote_routing_tool_json(
                    store,
                    "peer-timeline",
                    &[
                        ("speaker", q.speaker),
                        ("peer", q.peer),
                        ("vrf_rd", q.vrf_rd),
                        ("limit", q.limit.map(|n| n.to_string())),
                        ("tenant_id", q.tenant_id),
                    ],
                )
                .await?
            };
            Ok(enforce_tool_output_bounds(val))
        }
        "get_evidence_coverage" => {
            let q = EvidenceCoverageQuery {
                speaker: args["speaker"].as_str().map(String::from),
                vrf_rd: args["vrf_rd"].as_str().map(String::from),
                prefix: args["prefix"].as_str().map(String::from),
                tenant_id: args["tenant_id"].as_str().map(String::from),
            };
            let val = if let Some(st) = resolve_in_process_routing_state(store, routing).await {
                build_evidence_coverage_json(&st, &q).await
            } else {
                fetch_remote_routing_tool_json(
                    store,
                    "evidence-coverage",
                    &[
                        ("speaker", q.speaker),
                        ("vrf_rd", q.vrf_rd),
                        ("prefix", q.prefix),
                        ("tenant_id", q.tenant_id),
                    ],
                )
                .await?
            };
            Ok(enforce_tool_output_bounds(val))
        }
        "compare_snapshots" => {
            let q = CompareSnapshotsQuery {
                before_seq: args["before_seq"].as_u64(),
                after_seq: args["after_seq"].as_u64(),
                speaker: args["speaker"].as_str().map(String::from),
                vrf_rd: args["vrf_rd"].as_str().map(String::from),
                prefix: args["prefix"].as_str().map(String::from),
                limit: args["limit"].as_u64().map(|v| v as usize),
            };
            let val = if let Some(st) = resolve_in_process_routing_state(store, routing).await {
                build_compare_snapshots_json(&st, &q).await
            } else {
                fetch_remote_routing_tool_json(
                    store,
                    "compare-snapshots",
                    &[
                        ("before_seq", q.before_seq.map(|n| n.to_string())),
                        ("after_seq", q.after_seq.map(|n| n.to_string())),
                        ("speaker", q.speaker),
                        ("vrf_rd", q.vrf_rd),
                        ("prefix", q.prefix),
                        ("limit", q.limit.map(|n| n.to_string())),
                    ],
                )
                .await?
            };
            Ok(enforce_tool_output_bounds(val))
        }
        "explain_missing_prefix" => {
            let prefix = args["prefix"]
                .as_str()
                .ok_or("missing argument: prefix")?
                .to_string();
            let q = ExplainPrefixQuery {
                question: args["question"].as_str().map(String::from),
                prefix: Some(prefix.clone()),
                vrf_rd: args["vrf_rd"].as_str().map(String::from),
                speaker: args["speaker"].as_str().map(String::from),
                tenant_id: args["tenant_id"].as_str().map(String::from),
            };
            let val = if let Some(st) = resolve_in_process_routing_state(store, routing).await {
                let (_, v) = build_explain_prefix_json(&st, &q).await;
                v
            } else {
                fetch_remote_routing_tool_json(
                    store,
                    "explain-prefix",
                    &[
                        ("question", q.question),
                        ("prefix", Some(prefix)),
                        ("vrf_rd", q.vrf_rd),
                        ("speaker", q.speaker),
                        ("tenant_id", q.tenant_id),
                    ],
                )
                .await?
            };
            Ok(enforce_tool_output_bounds(val))
        }
        "evaluate_change_plan" => {
            let plan: crate::remediation::RemediationPlan =
                serde_json::from_value(args["plan"].clone())
                    .map_err(|e| format!("invalid RemediationPlan: {e}"))?;
            let twin: crate::remediation::DigitalTwinSnapshot =
                serde_json::from_value(args["digital_twin_snapshot"].clone())
                    .map_err(|e| format!("invalid DigitalTwinSnapshot: {e}"))?;
            let report = crate::remediation::verify_plan_in_digital_twin(&plan, &twin);
            let val = serde_json::to_value(report).map_err(|e| e.to_string())?;
            Ok(enforce_tool_output_bounds(val))
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

/// Validate tool-level RBAC role and enforce identity-derived resource scoping on MCP tool arguments.
pub fn authorize_and_scope_tool(
    name: &str,
    args: &mut JsonValue,
    scope_opt: Option<&crate::http_server::auth::RoutingAccessScope>,
    role_opt: Option<&crate::http_server::auth::Role>,
) -> Result<(), String> {
    authorize_and_scope_tool_with_mode(
        name,
        args,
        scope_opt,
        role_opt,
        crate::graph::ownership::OwnershipAccessMode::Strict,
    )
}

pub fn authorize_and_scope_tool_with_mode(
    name: &str,
    args: &mut JsonValue,
    original_scope: Option<&crate::http_server::auth::RoutingAccessScope>,
    role_opt: Option<&crate::http_server::auth::Role>,
    mode: crate::graph::ownership::OwnershipAccessMode,
) -> Result<(), String> {
    use crate::graph::ownership::OwnershipAccessMode;
    use crate::http_server::auth::Role;
    let original_resource_scoped = original_scope
        .is_some_and(|s| s.speaker.is_some() || s.vrf_rd.is_some() || s.tenant_id.is_some());
    let effective = original_scope.map(|s| {
        if name == "evaluate_change_plan" {
            return s.clone();
        }
        s.for_ownership_mode(
            mode,
            matches!(
                name,
                "query_devices"
                    | "get_changes"
                    | "get_device_blast_radius"
                    | "list_active_detections"
                    | "get_incident"
            ),
        )
    });
    let scope_opt = effective.as_ref();

    if !args.is_object() {
        *args = json!({});
    }

    // Strip any client-supplied internal `_scoped_*` parameters to prevent spoofing
    if let Some(obj) = args.as_object_mut() {
        obj.retain(|k, _| !k.starts_with("_scoped_"));
    }

    // 1. Resource-scoped keys must never run arbitrary Cypher, regardless of role,
    // and unsupported tool/scope combinations must fail closed.
    if let Some(scope) = scope_opt {
        let is_resource_scoped = original_resource_scoped;

        if is_resource_scoped {
            if name == "query_graph" {
                return Err(
                    "routing_scope_violation: arbitrary graph queries ('query_graph') are disabled for resource-scoped API keys; use scoped diagnostic tools instead"
                        .to_string(),
                );
            }

            // Device-bound tools operate on GraphStore Device/ConfigSnapshot/DetectionEvent nodes,
            // which have no tenant_id or vrf_rd properties in the schema.
            // Without an explicit speaker/device scope, ownership cannot be established.
            let is_device_bound_tool = matches!(
                name,
                "query_devices"
                    | "get_changes"
                    | "get_device_blast_radius"
                    | "list_active_detections"
                    | "get_incident"
            );
            if mode == OwnershipAccessMode::Strict
                && is_device_bound_tool
                && scope.speaker.is_none()
            {
                return Err(format!(
                    "routing_scope_violation: tool '{name}' cannot establish ownership for tenant/VRF scoped keys without an explicit speaker/device scope"
                ));
            }

            // compare_snapshots does not support tenant_id scoping
            if name == "compare_snapshots"
                && scope.tenant_id.is_some()
                && scope.speaker.is_none()
                && scope.vrf_rd.is_none()
            {
                return Err(
                    "routing_scope_violation: tool 'compare_snapshots' does not support tenant-only scoping"
                        .to_string(),
                );
            }

            if name != "evaluate_change_plan" && scope.tenant_id.is_some() {
                return Err("routing_scope_violation: tenant ownership is unavailable for stored observations".into());
            }
            if is_device_bound_tool && scope.vrf_rd.is_some() {
                return Err(
                    "routing_scope_violation: device records do not support VRF ownership".into(),
                );
            }

            if matches!(
                name,
                "get_prefix_journey"
                    | "get_peer_timeline"
                    | "get_evidence_coverage"
                    | "explain_missing_prefix"
            ) && scope.speaker.is_none()
                && mode == OwnershipAccessMode::Strict
            {
                return Err("routing_scope_violation: transport evidence requires a speaker ownership constraint".into());
            }
            // Default deny any unknown or unclassified tool for restricted keys
            let is_known_scoped_tool = is_device_bound_tool
                || matches!(
                    name,
                    "get_prefix_journey"
                        | "get_peer_timeline"
                        | "get_evidence_coverage"
                        | "compare_snapshots"
                        | "explain_missing_prefix"
                        | "evaluate_change_plan"
                );
            if !is_known_scoped_tool {
                return Err(format!(
                    "routing_scope_violation: tool '{name}' is not permitted for restricted API keys"
                ));
            }
        }
    }

    // 2. Tool-level role authorization
    if let Some(&role) = role_opt {
        let required_role = match name {
            "evaluate_change_plan" => Role::Operator,
            "query_graph" => Role::Viewer,
            _ => Role::ApiReadonly,
        };
        if role < required_role {
            return Err(format!(
                "insufficient_permissions: tool '{name}' requires at least {} role, user has '{}'",
                required_role.as_str(),
                role.as_str(),
            ));
        }
    }

    // 3. Identity-derived resource scoping for other tools
    if let Some(scope) = scope_opt {
        // Speaker / device address enforcement
        if let Some(ref spk) = scope.speaker {
            // Check top-level 'speaker'
            if let Some(s) = args.get("speaker").and_then(|v| v.as_str()) {
                if s != spk {
                    return Err(format!(
                        "routing_scope_violation: requested speaker '{s}' violates token scope '{spk}'"
                    ));
                }
            } else if matches!(
                name,
                "get_prefix_journey"
                    | "get_peer_timeline"
                    | "get_evidence_coverage"
                    | "compare_snapshots"
                    | "explain_missing_prefix"
            ) {
                if let Some(obj) = args.as_object_mut() {
                    obj.insert("speaker".to_string(), json!(spk));
                }
            }

            // Check top-level 'device_address'
            if let Some(d) = args.get("device_address").and_then(|v| v.as_str()) {
                if !d.is_empty() && d != spk {
                    return Err(format!(
                        "routing_scope_violation: requested device_address '{d}' violates token scope '{spk}'"
                    ));
                }
            }

            // Check top-level 'address'
            if let Some(a) = args.get("address").and_then(|v| v.as_str()) {
                if !a.is_empty() && a != spk {
                    return Err(format!(
                        "routing_scope_violation: requested device address '{a}' violates token scope '{spk}'"
                    ));
                }
            }

            // Tool-specific defaults and tags for speaker
            match name {
                "get_changes" | "list_active_detections" => {
                    if let Some(obj) = args.as_object_mut() {
                        let needs_dev = obj
                            .get("device_address")
                            .and_then(|v| v.as_str())
                            .map(|s| s.is_empty())
                            .unwrap_or(true);
                        if needs_dev {
                            obj.insert("device_address".to_string(), json!(spk));
                        }
                        obj.insert("_scoped_speaker".to_string(), json!(spk));
                    }
                }
                "get_device_blast_radius" => {
                    if let Some(obj) = args.as_object_mut() {
                        if obj.get("address").is_none() {
                            obj.insert("address".to_string(), json!(spk));
                        }
                        obj.insert("_scoped_speaker".to_string(), json!(spk));
                    }
                }
                "query_devices" | "get_incident" => {
                    if let Some(obj) = args.as_object_mut() {
                        obj.insert("_scoped_speaker".to_string(), json!(spk));
                    }
                }
                _ => {}
            }
        }

        // VRF RD enforcement
        if let Some(ref vrf) = scope.vrf_rd {
            if let Some(v) = args.get("vrf_rd").and_then(|val| val.as_str()) {
                if v != vrf {
                    return Err(format!(
                        "routing_scope_violation: requested vrf_rd '{v}' violates token scope '{vrf}'"
                    ));
                }
            } else if matches!(
                name,
                "get_prefix_journey"
                    | "get_peer_timeline"
                    | "get_evidence_coverage"
                    | "compare_snapshots"
                    | "explain_missing_prefix"
            ) {
                if let Some(obj) = args.as_object_mut() {
                    obj.insert("vrf_rd".to_string(), json!(vrf));
                }
            }
        }

        // Tenant ID enforcement
        if let Some(ref tenant) = scope.tenant_id {
            if let Some(t) = args.get("tenant_id").and_then(|val| val.as_str()) {
                if t != tenant {
                    return Err(format!(
                        "routing_scope_violation: requested tenant_id '{t}' violates token scope '{tenant}'"
                    ));
                }
            } else if matches!(
                name,
                "get_prefix_journey"
                    | "get_peer_timeline"
                    | "get_evidence_coverage"
                    | "explain_missing_prefix"
            ) {
                if let Some(obj) = args.as_object_mut() {
                    obj.insert("tenant_id".to_string(), json!(tenant));
                }
            }
        }

        // Plan fields enforcement for evaluate_change_plan
        if name == "evaluate_change_plan" {
            if let Some(plan_obj) = args.get("plan").and_then(|p| p.as_object()) {
                if let Some(ref spk) = scope.speaker {
                    if let Some(dev) = plan_obj.get("device_address").and_then(|v| v.as_str()) {
                        if dev != spk {
                            return Err(format!(
                                "routing_scope_violation: plan device_address '{dev}' violates token scope '{spk}'"
                            ));
                        }
                    }
                }
                if let Some(ref vrf) = scope.vrf_rd {
                    if let Some(v) = plan_obj.get("vrf_rd").and_then(|val| val.as_str()) {
                        if v != vrf {
                            return Err(format!(
                                "routing_scope_violation: plan vrf_rd '{v}' violates token scope '{vrf}'"
                            ));
                        }
                    }
                }
                if let Some(ref tenant) = scope.tenant_id {
                    if let Some(t) = plan_obj.get("tenant_id").and_then(|val| val.as_str()) {
                        if t != tenant {
                            return Err(format!(
                                "routing_scope_violation: plan tenant_id '{t}' violates token scope '{tenant}'"
                            ));
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

// ── MCP POST /mcp handler ─────────────────────────────────────────────────────

pub async fn mcp_handler(
    State(state): State<AppState>,
    extensions: axum::http::Extensions,
    Json(req): Json<McpRpcRequest>,
) -> Result<Json<McpRpcResponse>, (StatusCode, String)> {
    let id = req.id.clone().unwrap_or(JsonValue::Null);

    let response = match req.method.as_str() {
        "initialize" => McpRpcResponse::ok(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "bonsai", "version": env!("CARGO_PKG_VERSION") }
            }),
        ),

        "tools/list" => McpRpcResponse::ok(id, tool_schemas()),

        "tools/call" => {
            let name = req.params["name"].as_str().unwrap_or("").to_string();
            let args = req.params["arguments"].clone();
            let Some(role) = extensions.get::<crate::http_server::auth::Role>() else {
                return Ok(Json(McpRpcResponse::err(
                    id,
                    -32000,
                    "missing authenticated tool principal",
                )));
            };
            let context = ToolExecutionContext {
                role: *role,
                scope: extensions
                    .get::<crate::http_server::auth::RoutingAccessScope>()
                    .cloned(),
            };
            let result =
                call_tool_with_store_and_routing(&state.store, None, &name, &args, &context).await;

            match result {
                Ok(payload) => McpRpcResponse::ok(id, mcp_content(payload)),
                Err(e) => McpRpcResponse::err(id, -32000, e),
            }
        }

        other => McpRpcResponse::err(id, -32601, format!("method not found: {other}")),
    };

    Ok(Json(response))
}

/// Dispatch a single MCP tool call by name. Used by the investigation runtime.
pub async fn call_tool(
    state: &AppState,
    name: &str,
    args: &JsonValue,
    context: &ToolExecutionContext,
) -> Result<JsonValue, String> {
    call_tool_with_store_and_routing(&state.store, None, name, args, context).await
}

/// Return tool definitions in the `AiToolDef` format expected by the AI provider.
pub fn ai_tool_definitions() -> Vec<crate::ai_provider::AiToolDef> {
    let schemas = tool_schemas();
    schemas["tools"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|t| crate::ai_provider::AiToolDef {
            name: t["name"].as_str().unwrap_or("").to_string(),
            description: t["description"].as_str().unwrap_or("").to_string(),
            parameters: t["inputSchema"].clone(),
        })
        .collect()
}

// ── Grounded incident response types (T5-2, used by http_server) ─────────────

#[derive(Serialize)]
pub struct GroundedIncidentResponse {
    /// The root DetectionEvent.
    pub detection: DetectionSummary,
    /// Topological blast radius.
    pub blast_radius: crate::graph::queries::BlastRadiusResult,
    /// Human-readable rule description from the catalogue.
    pub rule_description: &'static str,
    /// Observable patterns that signal recurrence.
    pub recurrence_indicators: &'static [&'static str],
    /// Procedural references: stable links to investigate this type of incident.
    pub procedural_references: Vec<ProceduralRef>,
}

#[derive(Serialize)]
pub struct DetectionSummary {
    pub id: String,
    pub device_address: String,
    pub rule_id: String,
    pub severity: String,
    pub fired_at_ns: i64,
    pub features_json: String,
    pub remediation_status: String,
    pub remediation_action: String,
}

#[derive(Serialize)]
pub struct ProceduralRef {
    /// "graph_query" | "api_endpoint" | "history_check"
    pub kind: &'static str,
    pub label: String,
    pub href: String,
}

/// Build procedural references for a detection (T5-2 grounded response).
/// References are derived from recurrence indicators + standard API endpoints.
pub fn procedural_refs(device_address: &str, rule_id: &str) -> Vec<ProceduralRef> {
    let enc = |s: &str| s.replace(' ', "%20");
    vec![
        ProceduralRef {
            kind: "api_endpoint",
            label: format!("Blast radius from {device_address}"),
            href: format!("/api/blast-radius/{}", enc(device_address)),
        },
        ProceduralRef {
            kind: "api_endpoint",
            label: format!("Device detail for {device_address}"),
            href: format!("/api/devices/{}", enc(device_address)),
        },
        ProceduralRef {
            kind: "api_endpoint",
            label: format!("Active detections for rule {rule_id}"),
            href: format!("/api/detections?rule_id={}", enc(rule_id)),
        },
        ProceduralRef {
            kind: "api_endpoint",
            label: "Config history for this device".to_string(),
            href: format!("/api/devices/{}/config-history", enc(device_address)),
        },
        ProceduralRef {
            kind: "api_endpoint",
            label: "Closed-loop trace".to_string(),
            href: "/api/trace/<detection_id>".to_string(),
        },
    ]
}

// ── Reference resolution types (T5-5, used by http_server) ───────────────────

#[derive(Serialize)]
pub struct ResolveResponse {
    pub query: String,
    pub candidates: Vec<ResolveCandidate>,
}

#[derive(Serialize)]
pub struct ResolveCandidate {
    /// "device" | "detection" | "rule"
    pub kind: &'static str,
    pub id: String,
    pub label: String,
    /// 0.0–1.0 confidence score based on match quality.
    pub score: f32,
}

/// Score a candidate string against a query. Returns 0.0 if no match.
/// exact = 1.0, prefix = 0.8, substring = 0.5.
pub fn match_score(candidate: &str, query: &str) -> f32 {
    let c = candidate.to_lowercase();
    let q = query.to_lowercase();
    if q.is_empty() {
        return 0.0;
    }
    if c == q {
        1.0
    } else if c.starts_with(&q) {
        0.8
    } else if c.contains(&q) {
        0.5
    } else {
        0.0
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_catalogue_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for rule in RULE_CATALOGUE {
            assert!(
                seen.insert(rule.rule_id),
                "duplicate rule_id in catalogue: {}",
                rule.rule_id
            );
        }
    }

    #[test]
    fn every_rule_has_three_recurrence_indicators() {
        for rule in RULE_CATALOGUE {
            assert!(
                rule.recurrence_indicators.len() >= 3,
                "rule {} has fewer than 3 recurrence_indicators",
                rule.rule_id
            );
        }
    }

    #[test]
    fn rule_meta_lookup_hit_and_miss() {
        assert!(rule_meta("bgp_session_down").is_some());
        assert!(rule_meta("nonexistent_rule").is_none());
    }

    #[test]
    fn readonly_cypher_rejects_mutations() {
        // Original cases
        assert!(!is_readonly_cypher("CREATE (n:Device)"));
        assert!(!is_readonly_cypher("MATCH (n) SET n.x = 1"));
        assert!(!is_readonly_cypher("MATCH (n) DELETE n"));
        assert!(!is_readonly_cypher("MERGE (n:Device {address: '1.2.3.4'})"));
        assert!(!is_readonly_cypher("MATCH (n) DETACH DELETE n"));
        assert!(!is_readonly_cypher("MATCH (n) REMOVE n.x"));
        // New mutation keywords
        assert!(!is_readonly_cypher("DROP TABLE Foo"));
        assert!(!is_readonly_cypher("CALL write_procedure()"));
        assert!(!is_readonly_cypher("LOAD FROM 'data.csv' CREATE (n)"));
        // D2-11 T1 additions
        assert!(!is_readonly_cypher("COPY Device FROM 'devices.csv'"));
        assert!(!is_readonly_cypher("FOREACH (x IN [1,2,3] | CREATE (n))"));
        assert!(!is_readonly_cypher("ALTER TABLE Device ADD col INT"));
        assert!(!is_readonly_cypher("RENAME TABLE Device TO Node"));
        assert!(!is_readonly_cypher("IMPORT DATABASE '/tmp/db'"));
        assert!(!is_readonly_cypher("EXPORT DATABASE '/tmp/out'"));
        // Bypass attempt: mutation hidden after comment stripping
        assert!(!is_readonly_cypher("MATCH (n) /* safe */ SET n.x = 1"));
        assert!(!is_readonly_cypher("MATCH (n) -- comment\nSET n.x = 1"));
        assert!(!is_readonly_cypher("MATCH (n) // comment\nCREATE (m)"));
        // Case-insensitive
        assert!(!is_readonly_cypher("match (n) set n.x = 1"));
        assert!(!is_readonly_cypher("Match (n) Create (m:Foo)"));
    }

    #[test]
    fn readonly_cypher_allows_reads() {
        assert!(is_readonly_cypher("MATCH (d:Device) RETURN d.address"));
        assert!(is_readonly_cypher(
            "MATCH (n:BgpNeighbor {device_address: $dev}) RETURN n.session_state"
        ));
        assert!(is_readonly_cypher(
            "MATCH (a)-[:CONNECTED_TO]->(b) RETURN a, b"
        ));
        // Word-boundary: OFFSET should not match SET; CREATED should not match CREATE
        assert!(is_readonly_cypher("MATCH (n) RETURN n SKIP 0 LIMIT 10"));
        assert!(is_readonly_cypher(
            "MATCH (n:Device {created_at: $t}) RETURN n"
        ));
        // Comment-only containing a mutation keyword is stripped and allowed
        assert!(is_readonly_cypher("MATCH (n) RETURN n -- SET is not here"));
        assert!(is_readonly_cypher("/* CREATE */ MATCH (n) RETURN n"));
    }

    #[test]
    fn match_score_exact_prefix_substring() {
        assert_eq!(match_score("spine1", "spine1"), 1.0);
        assert_eq!(match_score("spine1.dc1.example.com", "spine1"), 0.8);
        assert_eq!(match_score("dc1-spine1-router", "spine1"), 0.5);
        assert_eq!(match_score("leaf2", "spine1"), 0.0);
        assert_eq!(match_score("anything", ""), 0.0);
    }
}
