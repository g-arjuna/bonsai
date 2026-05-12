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
use lbug::Connection;
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
];

/// Lookup a rule by ID. O(n) over the catalogue (18 rules, negligible cost).
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
        McpRpcResponse { jsonrpc: "2.0", id, result: Some(result), error: None }
    }
    fn err(id: JsonValue, code: i32, message: impl Into<String>) -> Self {
        McpRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(McpRpcError { code, message: message.into() }),
        }
    }
}

fn mcp_content(value: JsonValue) -> JsonValue {
    json!({ "content": [{ "type": "text", "text": value.to_string() }] })
}

// ── Tool schema definitions ───────────────────────────────────────────────────

fn tool_schemas() -> JsonValue {
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
                "description": "Execute a read-only Cypher query against the bonsai graph database. Mutation keywords (CREATE, SET, DELETE, MERGE, REMOVE, DETACH) are rejected.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cypher": { "type": "string", "description": "Read-only Cypher query string" }
                    },
                    "required": ["cypher"]
                }
            }
        ]
    })
}

// ── Tool implementations ──────────────────────────────────────────────────────

async fn tool_get_incident(state: &AppState, args: &JsonValue) -> Result<JsonValue, String> {
    let id = args["id"].as_str().ok_or("missing argument: id")?.to_string();

    let detections = state
        .store
        .read_detections(500)
        .await
        .map_err(|e| e.to_string())?;

    let root = detections
        .into_iter()
        .find(|d| d.id == id)
        .ok_or_else(|| format!("DetectionEvent {id} not found"))?;

    let blast = {
        let db = state.store.db();
        let addr = root.device_address.clone();
        tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            crate::graph::queries::blast_radius(&conn, &addr, 2).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())??
    };

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

async fn tool_query_devices(state: &AppState, args: &JsonValue) -> Result<JsonValue, String> {
    let filter = args["filter"].as_str().unwrap_or("").to_lowercase();

    let db = state.store.db();
    let raw: Vec<(String, String, String)> = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let rows: Vec<_> = conn
            .query("MATCH (d:Device) RETURN d.address, d.hostname, d.vendor")
            .map_err(|e| e.to_string())?
            .map(|row| (read_str(&row[0]), read_str(&row[1]), read_str(&row[2])))
            .collect();
        Ok::<_, String>(rows)
    })
    .await
    .map_err(|e| e.to_string())??;

    let devices: Vec<JsonValue> = raw
        .into_iter()
        .filter(|(addr, host, _)| {
            filter.is_empty()
                || addr.to_lowercase().contains(&filter)
                || host.to_lowercase().contains(&filter)
        })
        .map(|(address, hostname, vendor)| {
            json!({ "address": address, "hostname": hostname, "vendor": vendor })
        })
        .collect();

    let count = devices.len();
    Ok(json!({ "devices": devices, "count": count }))
}

async fn tool_get_device_blast_radius(
    state: &AppState,
    args: &JsonValue,
) -> Result<JsonValue, String> {
    let address = args["address"].as_str().ok_or("missing argument: address")?.to_string();
    let max_hops = args["max_hops"].as_u64().unwrap_or(2).min(5) as usize;

    let db = state.store.db();
    let result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::queries::blast_radius(&conn, &address, max_hops).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    serde_json::to_value(&result).map_err(|e| e.to_string())
}

async fn tool_list_active_detections(
    state: &AppState,
    args: &JsonValue,
) -> Result<JsonValue, String> {
    let limit = args["limit"].as_u64().unwrap_or(50).min(500) as u32;
    let severity_filter = args["severity"].as_str().unwrap_or("").to_string();
    let rule_filter = args["rule_id"].as_str().unwrap_or("").to_string();
    let device_filter = args["device_address"].as_str().unwrap_or("").to_string();

    let detections = state
        .store
        .read_detections(limit)
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

pub fn is_readonly_cypher(cypher: &str) -> bool {
    let upper = cypher.to_uppercase();
    for keyword in ["CREATE ", "SET ", "DELETE ", "MERGE ", "REMOVE ", "DETACH "] {
        if upper.contains(keyword) {
            return false;
        }
    }
    // Also catch trailing keyword at end-of-string (e.g. "MATCH (n) DELETE")
    for keyword in ["CREATE", "SET", "DELETE", "MERGE", "REMOVE", "DETACH"] {
        if upper.trim_end() == keyword {
            return false;
        }
    }
    true
}

async fn tool_query_graph(state: &AppState, args: &JsonValue) -> Result<JsonValue, String> {
    let cypher = args["cypher"].as_str().ok_or("missing argument: cypher")?;

    if !is_readonly_cypher(cypher) {
        return Err("mutation keywords not permitted in read-only query_graph tool".to_string());
    }

    let cypher = cypher.to_string();
    let db = state.store.db();

    let rows: Vec<Vec<String>> = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let result: Vec<Vec<String>> = conn
            .query(&cypher)
            .map_err(|e| e.to_string())?
            .map(|row| row.iter().map(|v| format!("{v:?}")).collect())
            .collect();
        Ok::<_, String>(result)
    })
    .await
    .map_err(|e| e.to_string())??;

    let row_count = rows.len();
    Ok(json!({ "rows": rows, "row_count": row_count }))
}

// ── MCP POST /mcp handler ─────────────────────────────────────────────────────

pub async fn mcp_handler(
    State(state): State<AppState>,
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
            let args = &req.params["arguments"];

            let result = match name.as_str() {
                "get_incident" => tool_get_incident(&state, args).await,
                "query_devices" => tool_query_devices(&state, args).await,
                "get_device_blast_radius" => tool_get_device_blast_radius(&state, args).await,
                "list_active_detections" => tool_list_active_detections(&state, args).await,
                "query_graph" => tool_query_graph(&state, args).await,
                other => Err(format!("unknown tool: {other}")),
            };

            match result {
                Ok(payload) => McpRpcResponse::ok(id, mcp_content(payload)),
                Err(e) => McpRpcResponse::err(id, -32000, e),
            }
        }

        other => McpRpcResponse::err(id, -32601, format!("method not found: {other}")),
    };

    Ok(Json(response))
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
            href: format!("/api/trace/<detection_id>"),
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
        assert!(!is_readonly_cypher("CREATE (n:Device)"));
        assert!(!is_readonly_cypher("MATCH (n) SET n.x = 1"));
        assert!(!is_readonly_cypher("MATCH (n) DELETE n"));
        assert!(!is_readonly_cypher("MERGE (n:Device {address: '1.2.3.4'})"));
        assert!(!is_readonly_cypher("MATCH (n) DETACH DELETE n"));
        assert!(!is_readonly_cypher("MATCH (n) REMOVE n.x"));
    }

    #[test]
    fn readonly_cypher_allows_reads() {
        assert!(is_readonly_cypher("MATCH (d:Device) RETURN d.address"));
        assert!(is_readonly_cypher(
            "MATCH (n:BgpNeighbor {device_address: $dev}) RETURN n.session_state"
        ));
        assert!(is_readonly_cypher("MATCH (a)-[:CONNECTED_TO]->(b) RETURN a, b"));
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
