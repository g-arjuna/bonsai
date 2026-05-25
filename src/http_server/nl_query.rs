//! Natural-language → Cypher query endpoint (NL-to-Graph).
//!
//! Accepts a plain English question, sends it to the configured AI provider
//! with the graph schema as context, receives back a read-only Cypher query,
//! validates it, executes against LadybugDB, and returns both the generated
//! Cypher and the result rows.

use axum::{Json, extract::State, http::StatusCode};
use crate::ai_provider::{AiMessage, build_provider_with_key};
use lbug::Connection;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

use super::AppState;

// ── Daily token budget (shared across NL queries + investigations) ────────────

static DAILY_INPUT_TOKENS: AtomicU64 = AtomicU64::new(0);
static DAILY_OUTPUT_TOKENS: AtomicU64 = AtomicU64::new(0);
static BUDGET_DAY_EPOCH: AtomicU64 = AtomicU64::new(0);

const DAILY_TOKEN_LIMIT: u64 = 500_000;

fn today_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400
}

fn roll_day_if_needed() {
    let today = today_epoch();
    let stored = BUDGET_DAY_EPOCH.load(Ordering::Relaxed);
    if today != stored {
        if BUDGET_DAY_EPOCH.compare_exchange(stored, today, Ordering::SeqCst, Ordering::Relaxed).is_ok() {
            DAILY_INPUT_TOKENS.store(0, Ordering::Relaxed);
            DAILY_OUTPUT_TOKENS.store(0, Ordering::Relaxed);
        }
    }
}

fn charge_tokens(input: u64, output: u64) -> Result<(), String> {
    roll_day_if_needed();
    let prev_in = DAILY_INPUT_TOKENS.fetch_add(input, Ordering::Relaxed);
    let prev_out = DAILY_OUTPUT_TOKENS.fetch_add(output, Ordering::Relaxed);
    let total = prev_in + input + prev_out + output;
    if total > DAILY_TOKEN_LIMIT {
        Err(format!("Daily NL query token budget exceeded ({total} / {DAILY_TOKEN_LIMIT})"))
    } else {
        Ok(())
    }
}

pub fn daily_token_usage() -> (u64, u64, u64) {
    roll_day_if_needed();
    let i = DAILY_INPUT_TOKENS.load(Ordering::Relaxed);
    let o = DAILY_OUTPUT_TOKENS.load(Ordering::Relaxed);
    (i, o, DAILY_TOKEN_LIMIT)
}

// ── Graph schema context ─────────────────────────────────────────────────────

/// Compact graph schema description injected into the LLM system prompt.
/// Manually maintained to match the DDL in `graph/mod.rs`.
/// Also used by investigation_runtime.rs (D4-8 T5) for agent graph awareness.
pub const GRAPH_SCHEMA: &str = "\
## Graph Schema (LadybugDB / openCypher subset)

### Node Tables
- Device(address STRING PK, vendor, hostname, role, site, model, serial_number, uptime_seconds, updated_at)
- Site(id PK, name, parent_id, location, latitude, longitude, updated_at)
- Interface(id PK, device_address, name, oper_status, in_octets, out_octets, in_errors, out_errors, speed, mtu, updated_at)
- BgpNeighbor(id PK, device_address, peer_address, session_state, peer_as, updated_at)
- BfdSession(id PK, device_address, if_name, remote_address, session_state, updated_at)
- IsisAdjacency(id PK, device_address, system_id, adjacency_state, source_type, updated_at)
- LldpNeighbor(id PK, device_address, local_if, chassis_id, port_id, system_name, updated_at)
- BmpSession(id PK, device_address, router_address, peer_address, peer_as, session_state, adj_rib_in_routes, loc_rib_routes, updated_at)
- StateChangeEvent(id PK, device_address, event_type, detail, source_type, occurred_at)
- DetectionEvent(id PK, device_address, rule_id, severity, features_json, source_types, latency_ns, fired_at)
- Remediation(id PK, detection_id, action, status, detail_json, attempted_at, completed_at)
- RemediationProposal(id PK, detection_id, playbook_id, trust_key, status, operator_note, steps_json, rollback_steps_json, proposed_at, decided_at)
- RemediationTrustMark(remediation_id PK, trustworthy, reason, created_at)
- Investigation(id PK, detection_id, device_address, trigger, status, summary, proposal_json, tokens_used, cost_usd, started_at, completed_at)
- AgentToolCall(id PK, investigation_id, tool_name, input_json, output_json, called_at)
- SubscriptionStatus(id PK, device_address, path, mode, status, last_observed_at, updated_at)
- Environment(id PK, name, archetype, metadata_json)
- EnrichmentProperty(id PK, device_address, key, value, source_name, updated_at)
- PropertyProvenance(id PK, owner_kind, owner_id, source, parser, confidence, captured_at, details_json)
- Application(id PK, name, criticality, owner_group, environment, updated_at)
- Incident(id PK, snow_sys_id, state, assignment_group, opened_at_ns, detection_id, updated_at)
- Location(id PK, name, kind, site_id, source, full_address, source_name, updated_at)
- Prefix(id PK, prefix, vlan_id, site, source, updated_at)
- VLAN(id PK, vid, name, site_name, source, updated_at)
- HostEndpoint(id PK, hostname, ip_address, mac_address, endpoint_type, source, updated_at)
- AppFlow(id PK, exporter_address, src_address, dst_address, protocol, src_port, dst_port, bytes, packets, updated_at)
- BgpRibEntry(id PK, rib_type, peer_address, prefix, next_hop, as_path, communities, updated_at)
- ConfigSnapshot(id PK, device_address, trigger, summary, confidence, parser, captured_at)
- ConfigChange(id PK, device_address, trigger, summary, confidence, parser, added_lines, removed_lines, changed_at)
- ChangeRequest(id PK, number, source, snow_sys_id, short_description, state, change_type, risk, assigned_to, assignment_group, affected_cis_json, planned_start_ns INT64, planned_end_ns INT64, correlation_id, external_ref, updated_at)
- SensorReading(id PK, device_address, component_name, sensor_type, temperature_c DOUBLE, power_w DOUBLE, fan_rpm INT64, humidity_pct DOUBLE, updated_at)
- OpticsTelemetry(id PK, device_address, if_name, rx_power_dbm DOUBLE, tx_power_dbm DOUBLE, laser_bias_ma DOUBLE, temperature_c DOUBLE, voltage_v DOUBLE, updated_at)
- RedundancyGroup(id PK, name, group_type, protected_node, state, source, updated_at)
- OspfNeighbor(id PK, device_address, neighbor_id, area, state, updated_at)
- Vrf(id PK, device_address, name, rd, updated_at)

### Relationship Tables
- HAS_INTERFACE(Device → Interface)
- LOCATED_AT(Device → Site)
- PARENT_OF(Site → Site)
- PEERS_WITH(Device → BgpNeighbor)
- HAS_BFD_SESSION(Device → BfdSession)
- HAS_ISIS_ADJACENCY(Device → IsisAdjacency)
- HAS_LLDP_NEIGHBOR(Device → LldpNeighbor)
- HAS_BMP_SESSION(Device → BmpSession)
- REPORTED_BY(Device → StateChangeEvent)
- REPORTED_BY(SensorReading → Device) — note: same name, different FROM/TO
- CONNECTED_TO(Interface → Interface)
- TRIGGERED(Device → DetectionEvent)
- RESOLVES(Remediation → DetectionEvent)
- HAS_PROPOSAL(DetectionEvent → RemediationProposal)
- TRUST_MARKS(RemediationTrustMark → Remediation)
- HAS_INCIDENT(Device → Incident)
- HAS_TOOL_CALL(Investigation → AgentToolCall)
- TRIGGERED_BY(DetectionEvent → StateChangeEvent)
- HAS_SUBSCRIPTION_STATUS(Device → SubscriptionStatus)
- BELONGS_TO_ENVIRONMENT(Site → Environment)
- HAS_ENRICHMENT_PROPERTY(Device → EnrichmentProperty)
- ENRICHMENT_PROPERTY_PROVENANCE(EnrichmentProperty → PropertyProvenance)
- RUNS_SERVICE(Device → Application)
- CARRIES_APPLICATION(Device → Application)
- IN_LOCATION(Device → Location)
- IN_SITE(Location → Site)
- LOC_PARENT_OF(Location → Location)
- CMDB_PARENT_OF(Device → Device) [rel_type, source_name, updated_at]
- HAS_PREFIX(Device → Prefix)
- CARRIES_FLOW(Device → AppFlow)
- SRC_HOST(AppFlow → HostEndpoint)
- DST_HOST(AppFlow → HostEndpoint)
- HOST_RUNS_SERVICE(HostEndpoint → Application)
- ACCESS_VLAN(Interface → VLAN)
- TRUNK_VLAN(Interface → VLAN)
- HAS_RIB_ENTRY(Device → BgpRibEntry)
- AFFECTED_BY_CHANGE(Device → ChangeRequest) [role, updated_at]
- CHANGE_CAUSED_CONFIG(ConfigChange → ChangeRequest)
- CHANGE_CAUSED_DETECTION(DetectionEvent → ChangeRequest)
- RELATED_TO_CHANGE(Incident → ChangeRequest)
- OPTICS_ON(OpticsTelemetry → Interface)
- MEMBER_OF(Device → RedundancyGroup) [role, priority, state, updated_at]
- HAS_OSPF_NEIGHBOR(Device → OspfNeighbor)
- HAS_VRF(Device → Vrf)

### Notes
- Device.address is the primary key and main join field (IP or IP:port).
- Device.hostname is the human-friendly name (e.g. 'spine1').
- StateChangeEvent.detail is a STRING (not details_json). source_type is 'gnmi', 'syslog', 'snmp', 'bmp', etc.
- DetectionEvent does NOT have a remediation_status column. To find unremediated detections, check for the absence of a RESOLVES edge.
- source_name on EnrichmentProperty is 'netbox', 'servicenow_cmdb', 'cli', etc.
- The VLAN node table is named VLAN (uppercase), not Vlan.
- All timestamps are TIMESTAMP_NS (nanosecond precision).
- Use MATCH ... RETURN only. Never use CREATE/SET/DELETE/MERGE (read-only).

### Detection → Remediation Pipeline
- DetectionEvent is fired by streaming rules. source_types is a JSON array of signal sources.
- RemediationProposal is a human-in-the-loop approval gate: status is 'pending', 'approved', 'rejected', 'rolled_back', or 'failed'.
- trust_key is \"rule_id:environment_archetype:site_id:playbook_id\" — it maps to a TrustState (suggest_only → approve_each → auto_with_notification → auto_silent).
- Remediation is created AFTER a proposal is approved and execution succeeds/fails.
- Investigation nodes record LLM-driven analysis of detections (agent tool calls, summary, cost).
- Incident nodes link to DetectionEvents via HAS_INCIDENT.
- To relate incidents back to devices, traverse Device -[:TRIGGERED]-> DetectionEvent -[:HAS_INCIDENT]-> Incident.
- To find unresolved detections: MATCH (de:DetectionEvent) WHERE NOT EXISTS { MATCH (r:Remediation)-[:RESOLVES]->(de) }

### Environmental Telemetry
- SensorReading captures chassis/component temperature, fan RPM, power draw, and humidity from gNMI paths.
- OpticsTelemetry captures optical interface metrics (RX/TX power dBm, laser bias, temperature) from transceivers.
- sensor_type: 'temperature', 'fan', 'power', 'humidity'.
- To find overheating: MATCH (s:SensorReading)-[:REPORTED_BY]->(d:Device) WHERE s.temperature_c > 75
- To find optical degradation: MATCH (o:OpticsTelemetry)-[:OPTICS_ON]->(i:Interface)<-[:HAS_INTERFACE]-(d:Device) WHERE o.rx_power_dbm < -20

### Change Management
- ChangeRequest represents a planned or in-progress change (ServiceNow CHG, AAP/Ansible job, manual maintenance).
- source: 'servicenow', 'aap', 'ansible_tower', 'manual', 'webhook'.
- state: 'new', 'scheduled', 'implement', 'review', 'closed', 'cancelled'.
- change_type: 'standard', 'normal', 'emergency'.
- AFFECTED_BY_CHANGE links devices to their planned change window.
- CHANGE_CAUSED_CONFIG links ConfigChanges that occurred during an active change window.
- CHANGE_CAUSED_DETECTION links DetectionEvents that occurred during an active change window.
- RELATED_TO_CHANGE links Incidents back to the authorising change ticket.
- A detection with a CHANGE_CAUSED_DETECTION edge is 'expected noise' — it fired during a planned maintenance.
";

const FEW_SHOT_EXAMPLES: &str = "\
## Examples

User: Which devices are connected to spine1?
Cypher: MATCH (d:Device {hostname: 'spine1'})-[:HAS_INTERFACE]->(si:Interface)-[:CONNECTED_TO]-(ri:Interface)<-[:HAS_INTERFACE]-(nb:Device) RETURN DISTINCT nb.hostname, nb.address, si.name AS local_if, ri.name AS remote_if

User: Show me all critical incidents
Cypher: MATCH (d:Device)-[:TRIGGERED]->(de:DetectionEvent)-[:HAS_INCIDENT]->(i:Incident) RETURN d.hostname, de.rule_id, de.severity, i.id, i.snow_sys_id, i.state, i.assignment_group, i.updated_at ORDER BY i.updated_at DESC LIMIT 30

User: How many devices per vendor?
Cypher: MATCH (d:Device) RETURN d.vendor, count(d) AS device_count ORDER BY device_count DESC

User: Show me BGP sessions that are down
Cypher: MATCH (d:Device)-[:PEERS_WITH]->(n:BgpNeighbor) WHERE n.session_state <> 'established' RETURN d.hostname, n.peer_address, n.session_state, n.peer_as ORDER BY d.hostname

User: Show devices with interface errors
Cypher: MATCH (d:Device)-[:HAS_INTERFACE]->(i:Interface) WHERE i.in_errors > 0 OR i.out_errors > 0 RETURN d.hostname, i.name, i.in_errors, i.out_errors ORDER BY (i.in_errors + i.out_errors) DESC LIMIT 25

User: List all detections
Cypher: MATCH (d:Device)-[:TRIGGERED]->(de:DetectionEvent) RETURN d.hostname, de.rule_id, de.severity, de.fired_at ORDER BY de.fired_at DESC LIMIT 50

User: Which devices have unresolved detections with no remediation?
Cypher: MATCH (d:Device)-[:TRIGGERED]->(de:DetectionEvent) OPTIONAL MATCH (r:Remediation)-[:RESOLVES]->(de) WITH d, de, r WHERE r IS NULL RETURN d.hostname, d.address, de.rule_id, de.severity, de.fired_at ORDER BY de.severity, de.fired_at DESC LIMIT 50

User: Show pending remediation proposals
Cypher: MATCH (de:DetectionEvent)-[:HAS_PROPOSAL]->(p:RemediationProposal) WHERE p.status = 'pending' RETURN p.id, de.rule_id, de.device_address, p.playbook_id, p.trust_key, p.proposed_at ORDER BY p.proposed_at DESC LIMIT 30

User: Show me investigation summaries
Cypher: MATCH (i:Investigation) WHERE i.status = 'complete' RETURN i.device_address, i.detection_id, i.summary, i.tokens_used, i.cost_usd, i.started_at ORDER BY i.started_at DESC LIMIT 20

User: Show all active change requests
Cypher: MATCH (c:ChangeRequest) WHERE c.state IN ['new', 'scheduled', 'implement'] RETURN c.number, c.short_description, c.source, c.state, c.change_type, c.risk, c.planned_start_ns, c.planned_end_ns ORDER BY c.planned_start_ns DESC LIMIT 50

User: Which devices are affected by change CHG0012345?
Cypher: MATCH (d:Device)-[:AFFECTED_BY_CHANGE]->(c:ChangeRequest) WHERE c.number CONTAINS 'CHG0012345' RETURN d.hostname, d.address, c.number, c.short_description, c.state, c.change_type

User: Show detections that fired during a planned change
Cypher: MATCH (de:DetectionEvent)-[:CHANGE_CAUSED_DETECTION]->(c:ChangeRequest) RETURN de.device_address, de.rule_id, de.severity, de.fired_at, c.number, c.short_description ORDER BY de.fired_at DESC LIMIT 30

User: Are there any incidents linked to change tickets?
Cypher: MATCH (i:Incident)-[:RELATED_TO_CHANGE]->(c:ChangeRequest) RETURN i.id, i.snow_sys_id, i.state, c.number AS change_number, c.short_description AS change_desc ORDER BY i.updated_at DESC LIMIT 30

User: Which devices have overheating components?
Cypher: MATCH (s:SensorReading)-[:REPORTED_BY]->(d:Device) WHERE s.temperature_c > 75 RETURN d.hostname, d.address, s.component_name, s.sensor_type, s.temperature_c ORDER BY s.temperature_c DESC LIMIT 50

User: Show me optical interface health for spine1
Cypher: MATCH (d:Device)-[:HAS_INTERFACE]->(i:Interface)<-[:OPTICS_ON]-(o:OpticsTelemetry) WHERE d.hostname = 'spine1' RETURN i.name, o.rx_power_dbm, o.tx_power_dbm, o.laser_bias_ma, o.temperature_c, o.updated_at ORDER BY i.name

User: Show all BMP sessions and their state
Cypher: MATCH (d:Device)-[:HAS_BMP_SESSION]->(b:BmpSession) RETURN d.hostname, b.peer_address, b.session_state, b.adj_rib_in_routes, b.loc_rib_routes ORDER BY d.hostname

User: Which devices are in a redundancy group?
Cypher: MATCH (d:Device)-[m:MEMBER_OF]->(rg:RedundancyGroup) RETURN d.hostname, rg.name, rg.group_type, rg.state, m.role ORDER BY rg.group_type, rg.name

User: Show recent state change events from syslog
Cypher: MATCH (d:Device)-[:REPORTED_BY]->(e:StateChangeEvent) WHERE e.source_type = 'syslog' RETURN d.hostname, e.event_type, e.detail, e.occurred_at ORDER BY e.occurred_at DESC LIMIT 30

User: Show all active change requests
Cypher: MATCH (c:ChangeRequest) WHERE c.state IN ['new', 'scheduled', 'implement'] RETURN c.number, c.short_description, c.source, c.state, c.change_type, c.risk ORDER BY c.planned_start_ns DESC LIMIT 50

User: Show detections that fired during a planned change
Cypher: MATCH (de:DetectionEvent)-[:CHANGE_CAUSED_DETECTION]->(c:ChangeRequest) RETURN de.device_address, de.rule_id, de.severity, de.fired_at, c.number, c.short_description ORDER BY de.fired_at DESC LIMIT 30
";


// ── Request / Response types ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct AskBody {
    question: String,
}

#[derive(Serialize, Clone)]
pub(super) struct AskResponse {
    question: String,
    answer: String,
    steps: Vec<AgentStep>,
    columns: Vec<String>,
    rows: Vec<Vec<serde_json::Value>>,
    row_count: usize,
    tokens_used: u64,
    error: Option<String>,
}

#[derive(Serialize, Clone)]
pub(super) struct AgentStep {
    tool: String,
    input: String,
    output_summary: String,
    row_count: Option<usize>,
}

// ── Tool definitions for the agentic loop ───────────────────────────────────

fn agent_tools() -> Vec<crate::ai_provider::AiToolDef> {
    use crate::ai_provider::AiToolDef;
    vec![
        AiToolDef {
            name: "run_cypher".to_string(),
            description: "Execute a read-only Cypher query against the network graph database. Returns columns and rows as JSON. Use this to investigate the user's question. If the query returns 0 rows, consider searching for similar names or trying a different approach.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "cypher": { "type": "string", "description": "The Cypher query to execute (read-only, no CREATE/SET/DELETE/MERGE)" },
                    "reason": { "type": "string", "description": "Brief explanation of why you are running this query" }
                },
                "required": ["cypher", "reason"]
            }),
        },
        AiToolDef {
            name: "search_devices".to_string(),
            description: "Fuzzy search for devices by hostname. Use this when the user mentions a device name and you need to find the exact hostname/address before querying. Returns matching devices with address, hostname, vendor, role, and site.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Partial hostname to search for (case-insensitive substring match)" }
                },
                "required": ["pattern"]
            }),
        },
    ]
}

/// Bootstrap a compact inventory snapshot so the LLM knows what's in the graph.
fn build_graph_context(db: &std::sync::Arc<lbug::Database>) -> String {
    let conn = match Connection::new(db) {
        Ok(c) => c,
        Err(_) => return "Graph database unavailable.".to_string(),
    };

    let mut ctx = String::from("## Live Graph Inventory\n\n");

    // Device count
    if let Ok(mut rows) = conn.query("MATCH (d:Device) RETURN count(d)") {
        if let Some(row) = rows.next() {
            ctx.push_str(&format!("Total devices: {}\n", val_str(&row[0])));
        }
    }

    // Hostnames sample (all, capped at 100)
    if let Ok(rows) = conn.query("MATCH (d:Device) RETURN d.hostname, d.address, d.vendor, d.role, d.site ORDER BY d.hostname LIMIT 100") {
        let devs: Vec<String> = rows.map(|r| {
            format!("  {} ({}, vendor={}, role={}, site={})",
                val_str(&r[0]), val_str(&r[1]), val_str(&r[2]), val_str(&r[3]), val_str(&r[4]))
        }).collect();
        if !devs.is_empty() {
            ctx.push_str(&format!("\nDevices ({}):\n{}\n", devs.len(), devs.join("\n")));
        }
    }

    // Sites
    if let Ok(rows) = conn.query("MATCH (s:Site) RETURN s.name, s.location LIMIT 50") {
        let sites: Vec<String> = rows.map(|r| format!("  {} ({})", val_str(&r[0]), val_str(&r[1]))).collect();
        if !sites.is_empty() {
            ctx.push_str(&format!("\nSites ({}):\n{}\n", sites.len(), sites.join("\n")));
        }
    }

    // Quick counts
    for (label, q) in [
        ("BGP neighbors", "MATCH (n:BgpNeighbor) RETURN count(n)"),
        ("Interfaces", "MATCH (i:Interface) RETURN count(i)"),
        ("Detections", "MATCH (d:DetectionEvent) RETURN count(d)"),
        ("BMP sessions", "MATCH (b:BmpSession) RETURN count(b)"),
        ("State change events", "MATCH (e:StateChangeEvent) RETURN count(e)"),
    ] {
        if let Ok(mut rows) = conn.query(q) {
            if let Some(row) = rows.next() {
                ctx.push_str(&format!("{}: {}\n", label, val_str(&row[0])));
            }
        }
    }

    ctx
}

fn val_str(v: &lbug::Value) -> String {
    match v {
        lbug::Value::String(s) => if s.is_empty() { "—".to_string() } else { s.clone() },
        lbug::Value::Int64(n) => n.to_string(),
        lbug::Value::Int32(n) => n.to_string(),
        lbug::Value::Double(f) => format!("{f:.2}"),
        lbug::Value::Bool(b) => b.to_string(),
        lbug::Value::Null(_) => "—".to_string(),
        other => format!("{other:?}"),
    }
}

/// Execute a tool call from the LLM agent.
fn execute_tool(
    db: &std::sync::Arc<lbug::Database>,
    tool_name: &str,
    args: &serde_json::Value,
) -> (String, Option<crate::graph::explorer::ExplorerResult>) {
    match tool_name {
        "run_cypher" => {
            let cypher = args["cypher"].as_str().unwrap_or("");
            if cypher.is_empty() {
                return ("Error: empty cypher query".to_string(), None);
            }
            if !crate::mcp_server::is_readonly_cypher(cypher) {
                return ("Error: mutation queries are not allowed. Use read-only MATCH...RETURN only.".to_string(), None);
            }
            let conn = match Connection::new(db) {
                Ok(c) => c,
                Err(e) => return (format!("Error: {e}"), None),
            };
            match crate::graph::explorer::execute_query(&conn, cypher) {
                Ok(result) => {
                    let summary = if result.row_count == 0 {
                        format!("Query returned 0 rows. Columns: [{}]. The query executed successfully but found no matching data. Consider: (1) searching for similar hostnames with search_devices, (2) using case-insensitive CONTAINS instead of exact match, (3) checking if the node/relationship type has any data.",
                            result.columns.join(", "))
                    } else {
                        // Return first 15 rows as compact JSON for LLM context
                        let preview_rows: Vec<&Vec<serde_json::Value>> = result.rows.iter().take(15).collect();
                        let table = serde_json::json!({
                            "columns": result.columns,
                            "rows": preview_rows,
                            "total_rows": result.row_count,
                            "truncated": result.truncated,
                        });
                        format!("Success: {} row(s). Data:\n{}", result.row_count, serde_json::to_string_pretty(&table).unwrap_or_default())
                    };
                    (summary, Some(result))
                }
                Err(e) => (format!("Query error: {e:#}. Check your Cypher syntax and that all node labels and relationship types exist in the schema."), None),
            }
        }
        "search_devices" => {
            let pattern = args["pattern"].as_str().unwrap_or("").to_lowercase();
            if pattern.is_empty() {
                return ("Error: empty search pattern".to_string(), None);
            }
            let conn = match Connection::new(db) {
                Ok(c) => c,
                Err(e) => return (format!("Error: {e}"), None),
            };
            let query = "MATCH (d:Device) WHERE toLower(d.hostname) CONTAINS $pat OR toLower(d.address) CONTAINS $pat RETURN d.address, d.hostname, d.vendor, d.role, d.site LIMIT 20";
            match conn.prepare(query) {
                Ok(mut stmt) => {
                    match conn.execute(&mut stmt, vec![("pat", lbug::Value::String(pattern.clone()))]) {
                        Ok(rows) => {
                            let matches: Vec<String> = rows.map(|r| {
                                format!("  {} (address={}, vendor={}, role={}, site={})",
                                    val_str(&r[1]), val_str(&r[0]), val_str(&r[2]), val_str(&r[3]), val_str(&r[4]))
                            }).collect();
                            if matches.is_empty() {
                                (format!("No devices found matching '{pattern}'. Try a shorter or different search term."), None)
                            } else {
                                (format!("Found {} device(s) matching '{pattern}':\n{}", matches.len(), matches.join("\n")), None)
                            }
                        }
                        Err(e) => (format!("Search error: {e}"), None),
                    }
                }
                Err(e) => (format!("Search error: {e}"), None),
            }
        }
        _ => (format!("Unknown tool: {tool_name}"), None),
    }
}

const AGENT_SYSTEM_PROMPT: &str = "\
You are Bonsai Explorer — an AI assistant that helps network engineers investigate their network by querying a live graph database.

Your workflow:
1. ALWAYS start by understanding what the user is asking. If they mention a device name, use search_devices first to find the exact hostname.
2. Use run_cypher to query the graph. If a query returns 0 rows, DON'T give up — try alternative approaches:
   - Search for similar hostnames with search_devices
   - Use CONTAINS for partial matching instead of exact equality
   - Try querying a broader scope first, then narrow down
   - Check if the relevant data exists at all (e.g., 'MATCH (n:BgpNeighbor) RETURN count(n)')
3. You can run MULTIPLE queries to build up a complete picture before answering.
4. When you have enough information, provide a clear, concise natural-language answer.

IMPORTANT RULES:
- Only use MATCH ... RETURN queries. Never mutate data.
- Use the schema provided. Don't invent node labels or relationships.
- Device.hostname is the human-friendly name. Device.address is the IP/key.
- For topology: Device -[:HAS_INTERFACE]-> Interface -[:CONNECTED_TO]- Interface <-[:HAS_INTERFACE]- Device
- For BGP: Device -[:PEERS_WITH]-> BgpNeighbor
- For detections: Device -[:TRIGGERED]-> DetectionEvent
- For syslog events: Device -[:REPORTED_BY]-> StateChangeEvent
- When the user says a name like 'spine1', it might be 'srl-spine1' or 'spine-1' etc. Always search first.

After investigating, give a final answer in natural language that directly addresses the user's question. Include specific data points, numbers, and names. If the graph has no relevant data, say so clearly and explain what data would be needed.";

const MAX_AGENT_ROUNDS: usize = 6;

// ── Handler ──────────────────────────────────────────────────────────────────

pub(super) async fn explorer_ask_handler(
    State(state): State<AppState>,
    Json(body): Json<AskBody>,
) -> Result<Json<AskResponse>, (StatusCode, String)> {
    let question = body.question.trim().to_string();
    if question.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "question is required".to_string()));
    }

    // 1. Resolve the active AI provider
    let (ai_cfg, api_key) = crate::http_server::settings::resolve_active_ai_provider(&state)
        .await
        .ok_or_else(|| (
            StatusCode::SERVICE_UNAVAILABLE,
            "Explorer Ask unavailable: no AI provider configured. Add a provider in Settings → LLM Providers and activate it.".to_string(),
        ))?;
    let provider = build_provider_with_key(&ai_cfg, api_key).map_err(|e| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("Explorer Ask unavailable: {e}"),
        )
    })?;

    // 2. Bootstrap graph context (device inventory, sites, counts)
    let db = state.store.db();
    let db_clone = db.clone();
    let graph_ctx = tokio::task::spawn_blocking(move || build_graph_context(&db_clone))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // 3. Build initial messages with full context
    let system_msg = format!(
        "{AGENT_SYSTEM_PROMPT}\n\n{GRAPH_SCHEMA}\n\n{FEW_SHOT_EXAMPLES}\n\n{graph_ctx}"
    );
    let tools = agent_tools();
    let mut messages = vec![
        AiMessage::system(system_msg),
        AiMessage::user(question.clone()),
    ];

    // 4. Agentic loop: let the LLM call tools iteratively
    let mut steps: Vec<AgentStep> = Vec::new();
    let mut last_result: Option<crate::graph::explorer::ExplorerResult> = None;
    let mut total_tokens: u64 = 0;

    for _round in 0..MAX_AGENT_ROUNDS {
        let resp = provider
            .complete(messages.clone(), tools.clone())
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("AI error: {e}")))?;

        charge_tokens(0, resp.tokens_used)
            .map_err(|e| (StatusCode::TOO_MANY_REQUESTS, e))?;
        total_tokens += resp.tokens_used;

        // If the model produced a text response with no tool calls, it's the final answer
        if resp.tool_calls.is_empty() {
            let answer = resp.content.unwrap_or_default();
            let (columns, rows, row_count) = if let Some(ref r) = last_result {
                (r.columns.clone(), r.rows.clone(), r.row_count)
            } else {
                (vec![], vec![], 0)
            };
            return Ok(Json(AskResponse {
                question,
                answer,
                steps,
                columns,
                rows,
                row_count,
                tokens_used: total_tokens,
                error: None,
            }));
        }

        // Process tool calls
        // Add assistant message with tool calls to conversation
        messages.push(AiMessage {
            role: "assistant".to_string(),
            content: resp.content.clone(),
            tool_calls: resp.tool_calls.clone(),
            tool_call_id: None,
            raw_parts: resp.raw_parts.clone(),
        });

        for tc in &resp.tool_calls {
            let args = &tc.arguments;
            let reason = args["reason"].as_str().unwrap_or("").to_string();
            let input_display = match tc.name.as_str() {
                "run_cypher" => args["cypher"].as_str().unwrap_or("").to_string(),
                "search_devices" => args["pattern"].as_str().unwrap_or("").to_string(),
                _ => tc.arguments.to_string(),
            };

            // Execute tool
            let db_ref = db.clone();
            let tool_name = tc.name.clone();
            let args_clone = args.clone();
            let (output, exec_result) = tokio::task::spawn_blocking(move || {
                execute_tool(&db_ref, &tool_name, &args_clone)
            })
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            let row_count = exec_result.as_ref().map(|r| r.row_count);
            if exec_result.is_some() {
                last_result = exec_result;
            }

            steps.push(AgentStep {
                tool: tc.name.clone(),
                input: if reason.is_empty() { input_display } else { format!("{reason}: {input_display}") },
                output_summary: if output.len() > 200 {
                    format!("{}… [truncated]", &output[..200])
                } else {
                    output.clone()
                },
                row_count,
            });

            // Add tool result to conversation
            messages.push(AiMessage::tool_result(tc.id.clone(), output));
        }
    }

    // Exhausted rounds — ask for a final answer
    messages.push(AiMessage::user(
        "You've used all your investigation rounds. Please provide your best answer now based on what you've found.".to_string()
    ));
    let final_resp = provider
        .complete(messages, vec![])
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("AI error: {e}")))?;
    charge_tokens(0, final_resp.tokens_used)
        .map_err(|e| (StatusCode::TOO_MANY_REQUESTS, e))?;
    total_tokens += final_resp.tokens_used;

    let answer = final_resp.content.unwrap_or_default();
    let (columns, rows, row_count) = if let Some(ref r) = last_result {
        (r.columns.clone(), r.rows.clone(), r.row_count)
    } else {
        (vec![], vec![], 0)
    };

    Ok(Json(AskResponse {
        question,
        answer,
        steps,
        columns,
        rows,
        row_count,
        tokens_used: total_tokens,
        error: None,
    }))
}

/// Budget status endpoint for the UI.
#[derive(Serialize)]
pub(super) struct NlBudgetResponse {
    daily_input_tokens: u64,
    daily_output_tokens: u64,
    daily_total_tokens: u64,
    daily_limit: u64,
}

pub(super) async fn nl_budget_handler() -> Json<NlBudgetResponse> {
    let (i, o, limit) = daily_token_usage();
    Json(NlBudgetResponse {
        daily_input_tokens: i,
        daily_output_tokens: o,
        daily_total_tokens: i + o,
        daily_limit: limit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_day_roll() {
        roll_day_if_needed();
        let today = today_epoch();
        assert_eq!(BUDGET_DAY_EPOCH.load(Ordering::Relaxed), today);
    }
}
