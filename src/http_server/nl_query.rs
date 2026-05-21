//! Natural-language → Cypher query endpoint (NL-to-Graph).
//!
//! Accepts a plain English question, sends it to Anthropic Claude with the
//! graph schema as context, receives back a read-only Cypher query, validates
//! it, executes against LadybugDB, and returns both the generated Cypher and
//! the result rows.
//!
//! Requires `ANTHROPIC_API_KEY` environment variable.

use axum::{Json, extract::State, http::StatusCode};
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
- Device(address STRING PK, vendor, hostname, role, site, updated_at)
- Site(id PK, name, parent_id, location, latitude, longitude, updated_at)
- Interface(id PK, device_address, name, oper_status, in_octets, out_octets, in_errors, out_errors, speed, mtu, updated_at)
- BgpNeighbor(id PK, device_address, peer_address, session_state, peer_as, updated_at)
- BfdSession(id PK, device_address, if_name, remote_address, session_state, updated_at)
- LldpNeighbor(id PK, device_address, local_if, chassis_id, port_id, system_name, updated_at)
- StateChangeEvent(id PK, device_address, event_type, details_json, occurred_at)
- DetectionEvent(id PK, device_address, rule_id, severity, features_json, remediation_status, fired_at)
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
- Incident(id PK, device_address, number, short_description, priority, state, updated_at)
- Location(id PK, name, kind, site_id, source, full_address, source_name, updated_at)
- Prefix(id PK, prefix, vlan_id, site, source, updated_at)
- Vlan(id PK, vid, name, site_name, source, updated_at)
- HostEndpoint(id PK, hostname, ip_address, mac_address, endpoint_type, source, updated_at)
- AppFlow(id PK, exporter_address, src_address, dst_address, protocol, src_port, dst_port, bytes, packets, updated_at)
- BgpRibEntry(id PK, rib_type, peer_address, prefix, next_hop, as_path, communities, updated_at)
- ConfigSnapshot(id PK, device_address, trigger, summary, confidence, parser, captured_at)
- ConfigChange(id PK, device_address, trigger, summary, confidence, parser, added_lines, removed_lines, changed_at)
- ChangeRequest(id PK, number, source, snow_sys_id, short_description, state, change_type, risk, assigned_to, assignment_group, affected_cis_json, planned_start_ns INT64, planned_end_ns INT64, correlation_id, external_ref, updated_at)

### Relationship Tables
- HAS_INTERFACE(Device → Interface)
- LOCATED_AT(Device → Site)
- PARENT_OF(Site → Site)
- PEERS_WITH(Device → BgpNeighbor)
- HAS_BFD_SESSION(Device → BfdSession)
- HAS_LLDP_NEIGHBOR(Device → LldpNeighbor)
- REPORTED_BY(Device → StateChangeEvent)
- CONNECTED_TO(Interface → Interface)
- TRIGGERED(Device → DetectionEvent)
- RESOLVES(Remediation → DetectionEvent)
- HAS_PROPOSAL(DetectionEvent → RemediationProposal)
- TRUST_MARKS(RemediationTrustMark → Remediation)
- HAS_INCIDENT(DetectionEvent → Incident)
- HAS_TOOL_CALL(Investigation → AgentToolCall)
- TRIGGERED_BY(DetectionEvent → StateChangeEvent)
- HAS_SUBSCRIPTION_STATUS(Device → SubscriptionStatus)
- BELONGS_TO_ENVIRONMENT(Site → Environment)
- HAS_ENRICHMENT_PROPERTY(Device → EnrichmentProperty)
- ENRICHMENT_PROPERTY_PROVENANCE(EnrichmentProperty → PropertyProvenance)
- RUNS_SERVICE(Device → Application)
- CARRIES_APPLICATION(Device → Application)
- HAS_INCIDENT(Device → Incident)
- IN_LOCATION(Device → Location)
- IN_SITE(Location → Site)
- LOC_PARENT_OF(Location → Location)
- CMDB_PARENT_OF(Device → Device) [rel_type, source_name, updated_at]
- HAS_PREFIX(Device → Prefix)
- CARRIES_FLOW(Device → AppFlow)
- SRC_HOST(AppFlow → HostEndpoint)
- DST_HOST(AppFlow → HostEndpoint)
- HOST_RUNS_SERVICE(HostEndpoint → Application)
- ON_VLAN(Interface → Vlan)
- HAS_RIB_ENTRY(Device → BgpRibEntry)
- AFFECTED_BY_CHANGE(Device → ChangeRequest) [role, updated_at]
- CHANGE_CAUSED_CONFIG(ConfigChange → ChangeRequest)
- CHANGE_CAUSED_DETECTION(DetectionEvent → ChangeRequest)
- RELATED_TO_CHANGE(Incident → ChangeRequest)

### Notes
- Device.address is the primary key and main join field (IP or IP:port).
- Device.hostname is the human-friendly name (e.g. 'spine1').
- source_name on EnrichmentProperty is 'netbox', 'servicenow_cmdb', 'cli', etc.
- All timestamps are TIMESTAMP_NS (nanosecond precision).
- Use MATCH ... RETURN only. Never use CREATE/SET/DELETE/MERGE (read-only).

### Detection → Remediation Pipeline
- DetectionEvent is fired by streaming rules. remediation_status tracks lifecycle ('', 'proposed', 'approved', 'failed').
- RemediationProposal is a human-in-the-loop approval gate: status is 'pending', 'approved', 'rejected', 'rolled_back', or 'failed'.
- trust_key is \"rule_id:environment_archetype:site_id:playbook_id\" — it maps to a TrustState (suggest_only → approve_each → auto_with_notification → auto_silent).
- Remediation is created AFTER a proposal is approved and execution succeeds/fails.
- Investigation nodes record LLM-driven analysis of detections (agent tool calls, summary, cost).
- Incident nodes link to ServiceNow incidents (snow_sys_id) and to DetectionEvents.

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
Cypher: MATCH (d:Device)-[:HAS_INCIDENT]->(i:Incident) WHERE i.priority <= '2' RETURN d.hostname, i.number, i.short_description, i.priority, i.state ORDER BY i.priority

User: What business services run on 10.0.0.1?
Cypher: MATCH (d:Device {address: '10.0.0.1'})-[:RUNS_SERVICE|CARRIES_APPLICATION]->(a:Application) RETURN a.name, a.criticality, a.owner_group

User: How many devices per vendor?
Cypher: MATCH (d:Device) RETURN d.vendor, count(d) AS device_count ORDER BY device_count DESC

User: Show enrichment conflicts
Cypher: MATCH (ep:EnrichmentProperty)-[:ENRICHMENT_PROPERTY_PROVENANCE]->(pv:PropertyProvenance) WHERE pv.details_json CONTAINS 'conflict' RETURN ep.device_address, ep.key, ep.value, ep.source_name, pv.confidence ORDER BY ep.device_address, ep.key

User: Which devices are in the NYC location?
Cypher: MATCH (d:Device)-[:IN_LOCATION]->(l:Location) WHERE l.name CONTAINS 'NYC' RETURN d.hostname, d.address, l.name, l.full_address

User: Show me BGP sessions that are down
Cypher: MATCH (d:Device)-[:PEERS_WITH]->(n:BgpNeighbor) WHERE n.session_state <> 'established' RETURN d.hostname, n.peer_address, n.session_state, n.peer_as ORDER BY d.hostname

User: What is the CMDB parent of leaf1?
Cypher: MATCH (p:Device)-[r:CMDB_PARENT_OF]->(c:Device) WHERE c.hostname CONTAINS 'leaf1' RETURN p.hostname AS parent, c.hostname AS child, r.rel_type

User: Show devices with interface errors
Cypher: MATCH (d:Device)-[:HAS_INTERFACE]->(i:Interface) WHERE i.in_errors > 0 OR i.out_errors > 0 RETURN d.hostname, i.name, i.in_errors, i.out_errors ORDER BY (i.in_errors + i.out_errors) DESC LIMIT 25

User: List all detections in the last 24 hours
Cypher: MATCH (d:Device)-[:TRIGGERED]->(de:DetectionEvent) RETURN d.hostname, de.rule_id, de.severity, de.fired_at ORDER BY de.fired_at DESC LIMIT 50

User: Show pending remediation proposals
Cypher: MATCH (de:DetectionEvent)-[:HAS_PROPOSAL]->(p:RemediationProposal) WHERE p.status = 'pending' RETURN p.id, de.rule_id, de.device_address, p.playbook_id, p.trust_key, p.proposed_at ORDER BY p.proposed_at DESC LIMIT 30

User: What remediations have been executed and what was the result?
Cypher: MATCH (r:Remediation)-[:RESOLVES]->(de:DetectionEvent) RETURN r.id, de.rule_id, de.device_address, r.action, r.status, r.attempted_at ORDER BY r.attempted_at DESC LIMIT 30

User: Show me investigation summaries for recent incidents
Cypher: MATCH (i:Investigation) WHERE i.status = 'complete' RETURN i.device_address, i.detection_id, i.summary, i.tokens_used, i.cost_usd, i.started_at ORDER BY i.started_at DESC LIMIT 20

User: Which devices have unresolved detections with no remediation?
Cypher: MATCH (d:Device)-[:TRIGGERED]->(de:DetectionEvent) WHERE de.remediation_status = '' OR de.remediation_status = 'none' RETURN d.hostname, d.address, de.rule_id, de.severity, de.fired_at ORDER BY de.severity, de.fired_at DESC LIMIT 50

User: What playbooks have been proposed for BGP detections?
Cypher: MATCH (de:DetectionEvent)-[:HAS_PROPOSAL]->(p:RemediationProposal) WHERE de.rule_id CONTAINS 'bgp' RETURN de.rule_id, de.device_address, p.playbook_id, p.status, p.trust_key ORDER BY p.proposed_at DESC LIMIT 30

User: Show all active change requests
Cypher: MATCH (c:ChangeRequest) WHERE c.state IN ['new', 'scheduled', 'implement'] RETURN c.number, c.short_description, c.source, c.state, c.change_type, c.risk, c.planned_start_ns, c.planned_end_ns ORDER BY c.planned_start_ns DESC LIMIT 50

User: Which devices are affected by change CHG0012345?
Cypher: MATCH (d:Device)-[:AFFECTED_BY_CHANGE]->(c:ChangeRequest) WHERE c.number CONTAINS 'CHG0012345' RETURN d.hostname, d.address, c.number, c.short_description, c.state, c.change_type

User: Show detections that fired during a planned change
Cypher: MATCH (de:DetectionEvent)-[:CHANGE_CAUSED_DETECTION]->(c:ChangeRequest) RETURN de.device_address, de.rule_id, de.severity, de.fired_at, c.number, c.short_description ORDER BY de.fired_at DESC LIMIT 30

User: Are there any incidents linked to change tickets?
Cypher: MATCH (i:Incident)-[:RELATED_TO_CHANGE]->(c:ChangeRequest) RETURN i.number, i.short_description, i.state, c.number AS change_number, c.short_description AS change_desc ORDER BY i.updated_at DESC LIMIT 30
";

const SYSTEM_PROMPT: &str = "\
You are a Cypher query generator for the Bonsai network state engine graph database.

Given a user's natural-language question about the network, generate a single read-only Cypher query that answers it.

Rules:
1. Output ONLY a JSON object: {\"cypher\": \"...\", \"explanation\": \"...\"}
2. The cypher field must be valid openCypher (LadybugDB/Kuzu dialect).
3. NEVER use mutation keywords: CREATE, SET, DELETE, MERGE, REMOVE, DETACH, DROP, ALTER.
4. Always include ORDER BY and LIMIT where sensible (default LIMIT 50).
5. Use the schema below. Do NOT invent node labels or relationship types.
6. The explanation field should be 1-2 sentences describing what the query does.
7. If the question is ambiguous, make a reasonable assumption and note it in the explanation.
8. Use case-insensitive matching (CONTAINS or toLower) for hostname/name searches when the user gives a partial name.
";

// ── Request / Response types ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct AskBody {
    question: String,
}

#[derive(Serialize)]
pub(super) struct AskResponse {
    question: String,
    cypher: String,
    explanation: String,
    columns: Vec<String>,
    rows: Vec<Vec<serde_json::Value>>,
    row_count: usize,
    tokens_used: u64,
    error: Option<String>,
}

// ── Handler ──────────────────────────────────────────────────────────────────

pub(super) async fn explorer_ask_handler(
    State(state): State<AppState>,
    Json(body): Json<AskBody>,
) -> Result<Json<AskResponse>, (StatusCode, String)> {
    let question = body.question.trim().to_string();
    if question.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "question is required".to_string()));
    }

    // 1. Call Anthropic to generate Cypher
    let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "ANTHROPIC_API_KEY not set — natural language queries require an API key".to_string(),
        )
    })?;

    let (cypher, explanation, tokens) = generate_cypher(&api_key, &question).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("LLM error: {e}"))
    })?;

    // 2. Validate read-only
    if !crate::mcp_server::is_readonly_cypher(&cypher) {
        return Ok(Json(AskResponse {
            question,
            cypher,
            explanation: "Generated query contained mutation keywords and was rejected.".to_string(),
            columns: vec![],
            rows: vec![],
            row_count: 0,
            tokens_used: tokens,
            error: Some("LLM generated a mutating query — rejected for safety".to_string()),
        }));
    }

    // 3. Execute against graph
    let db = state.store.db();
    let cypher_clone = cypher.clone();
    let exec_result = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        crate::graph::explorer::execute_query(&conn, &cypher_clone).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match exec_result {
        Ok(result) => Ok(Json(AskResponse {
            question,
            cypher,
            explanation,
            columns: result.columns,
            rows: result.rows,
            row_count: result.row_count,
            tokens_used: tokens,
            error: None,
        })),
        Err(e) => Ok(Json(AskResponse {
            question,
            cypher,
            explanation,
            columns: vec![],
            rows: vec![],
            row_count: 0,
            tokens_used: tokens,
            error: Some(format!("Query execution failed: {e}")),
        })),
    }
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

// ── Anthropic API call ───────────────────────────────────────────────────────

async fn generate_cypher(api_key: &str, question: &str) -> Result<(String, String, u64), String> {
    let client = reqwest::Client::new();

    let system = format!("{SYSTEM_PROMPT}\n\n{GRAPH_SCHEMA}\n\n{FEW_SHOT_EXAMPLES}");

    let body = serde_json::json!({
        "model": "claude-haiku-4-5-20251001",
        "max_tokens": 1024,
        "system": system,
        "messages": [
            {
                "role": "user",
                "content": question
            }
        ]
    });

    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Anthropic API {status}: {text}"));
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| format!("parse error: {e}"))?;

    // Extract token usage
    let input_tokens = json["usage"]["input_tokens"].as_u64().unwrap_or(0);
    let output_tokens = json["usage"]["output_tokens"].as_u64().unwrap_or(0);

    // Charge budget
    charge_tokens(input_tokens, output_tokens)?;

    let total = input_tokens + output_tokens;

    // Extract text content from the response
    let text = json["content"]
        .as_array()
        .and_then(|arr| arr.iter().find(|b| b["type"] == "text"))
        .and_then(|b| b["text"].as_str())
        .ok_or("No text content in LLM response")?;

    // Parse the JSON response from the model
    // Try to find JSON in the response (model may wrap in markdown code blocks)
    let json_str = extract_json_from_response(text);

    let parsed: serde_json::Value =
        serde_json::from_str(&json_str).map_err(|e| format!("Failed to parse LLM JSON: {e} — raw: {text}"))?;

    let cypher = parsed["cypher"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    let explanation = parsed["explanation"]
        .as_str()
        .unwrap_or("Generated query")
        .to_string();

    if cypher.is_empty() {
        return Err(format!("LLM returned empty cypher — raw response: {text}"));
    }

    Ok((cypher, explanation, total))
}

/// Extract JSON from LLM response, handling markdown code fences.
fn extract_json_from_response(text: &str) -> String {
    // Try bare JSON first
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        return trimmed.to_string();
    }
    // Try ```json ... ``` blocks
    if let Some(start) = trimmed.find("```json") {
        let after = &trimmed[start + 7..];
        if let Some(end) = after.find("```") {
            return after[..end].trim().to_string();
        }
    }
    // Try ``` ... ``` blocks
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        if let Some(end) = after.find("```") {
            return after[..end].trim().to_string();
        }
    }
    // Last resort: find first { and last }
    if let (Some(s), Some(e)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if e > s {
            return trimmed[s..=e].to_string();
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_bare() {
        let input = r#"{"cypher": "MATCH (d:Device) RETURN d", "explanation": "all devices"}"#;
        let result = extract_json_from_response(input);
        assert!(result.starts_with('{'));
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["cypher"], "MATCH (d:Device) RETURN d");
    }

    #[test]
    fn extract_json_fenced() {
        let input = "Here is the query:\n```json\n{\"cypher\": \"MATCH (d:Device) RETURN d\", \"explanation\": \"all\"}\n```";
        let result = extract_json_from_response(input);
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["cypher"], "MATCH (d:Device) RETURN d");
    }

    #[test]
    fn extract_json_wrapped_text() {
        let input = "Sure! Here's what I generated: {\"cypher\": \"MATCH (d) RETURN d\", \"explanation\": \"ok\"} hope that helps";
        let result = extract_json_from_response(input);
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["cypher"], "MATCH (d) RETURN d");
    }

    #[test]
    fn budget_day_roll() {
        roll_day_if_needed();
        let today = today_epoch();
        assert_eq!(BUDGET_DAY_EPOCH.load(Ordering::Relaxed), today);
    }
}
