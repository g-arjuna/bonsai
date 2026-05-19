"""Graph-aware tools for the investigation agent (T3-1).

Each tool is a plain function that takes a BonsaiClient and keyword args.
The agent loop calls these after the model selects a tool via tool_use.

All tools are read-only except propose_playbook, which writes a
RemediationProposal — never executes it directly.
"""
from __future__ import annotations

import json
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from bonsai_sdk.client import BonsaiClient


def get_blast_radius(client: "BonsaiClient", device_address: str, max_hops: int = 3) -> dict:
    """Devices reachable within max_hops of device_address via topology links."""
    try:
        return client._http_json("GET", f"/api/blast-radius/{device_address}?max_hops={max_hops}")
    except Exception as exc:
        return {"error": str(exc)}


def get_application_impact(client: "BonsaiClient", device_address: str) -> dict:
    """Applications that run services on device_address or its neighbours."""
    cypher = (
        "MATCH (d:Device {address: $addr})-[:HAS_INTERFACE|CONNECTED_TO*0..2]-(n:Device) "
        "MATCH (app:Application)-[:RUNS_SERVICE]->(n) "
        "RETURN DISTINCT app.name, app.criticality, n.address AS host"
    )
    try:
        return client._http_json(
            "POST",
            "/api/explorer/query",
            {"cypher": cypher, "params": {"addr": device_address}},
        )
    except Exception as exc:
        return {"error": str(exc)}


def query_graph(client: "BonsaiClient", cypher: str) -> dict:
    """Execute a sanitised read-only Cypher query against the graph."""
    try:
        return client._http_json("POST", "/api/explorer/query", {"cypher": cypher})
    except Exception as exc:
        return {"error": str(exc)}


def ask_graph(client: "BonsaiClient", question: str) -> dict:
    """Ask a natural language question about the network graph.

    The server uses an LLM to generate a Cypher query from the question,
    executes it, and returns both the generated Cypher and result rows.
    Requires ANTHROPIC_API_KEY on the server.
    """
    try:
        return client._http_json("POST", "/api/explorer/ask", {"question": question})
    except Exception as exc:
        return {"error": str(exc)}


def get_recent_detections(
    client: "BonsaiClient", device_address: str, window_secs: int = 300
) -> dict:
    """DetectionEvents for device_address in the last window_secs seconds."""
    cypher = (
        "MATCH (d:Device {address: $addr})-[:TRIGGERED]->(e:DetectionEvent) "
        "WHERE e.fired_at > timestamp() - $window_ns "
        "RETURN e.id, e.rule_id, e.severity, e.fired_at ORDER BY e.fired_at DESC LIMIT 20"
    )
    try:
        return client._http_json(
            "POST",
            "/api/explorer/query",
            {"cypher": cypher, "params": {
                "addr": device_address,
                "window_ns": window_secs * 1_000_000_000,
            }},
        )
    except Exception as exc:
        return {"error": str(exc)}


def get_remediation_history(client: "BonsaiClient", device_address: str) -> dict:
    """Past remediations attempted on device_address."""
    cypher = (
        "MATCH (d:Device {address: $addr})-[:TRIGGERED]->(e:DetectionEvent)"
        "<-[:TRIGGERED_BY]-(r:Remediation) "
        "RETURN r.id, r.action, r.status, r.attempted_at ORDER BY r.attempted_at DESC LIMIT 10"
    )
    try:
        return client._http_json(
            "POST",
            "/api/explorer/query",
            {"cypher": cypher, "params": {"addr": device_address}},
        )
    except Exception as exc:
        return {"error": str(exc)}


def summarise(text: str) -> dict:
    """Return a structured summary dict from free-form text (identity tool)."""
    return {"summary": text}


def propose_playbook(
    client: "BonsaiClient",
    detection_id: str,
    playbook_id: str,
    rationale: str,
    rule_id: str = "agent_proposal",
    environment_archetype: str = "dc",
    site_id: str = "",
) -> dict:
    """Write a RemediationProposal to the approval queue — never executes.

    Requires human approval before any action is taken (mandatory gate).
    Audit-logged with purpose=AgentInvestigation.

    Looks up the playbook from the catalog to populate real steps and
    verification. Falls back to a placeholder if catalog lookup fails.
    """
    steps_list = [{"action": "playbook", "playbook_id": playbook_id, "rationale": rationale}]
    rollback_steps_list: list = []

    # Try to resolve the actual playbook steps from the catalog.
    try:
        from bonsai_sdk.playbooks.catalog import PlaybookCatalog
        catalog = PlaybookCatalog()
        candidates = catalog.for_detection(rule_id, "*")
        playbook = next((p for p in candidates if p.get("name") == playbook_id), None)
        if playbook and playbook.get("steps"):
            steps_list = list(playbook["steps"])
            # Append verify_graph step from playbook verification if present.
            vfy = playbook.get("verification", {})
            expected = vfy.get("expected_graph_state") or vfy.get("cypher")
            if expected:
                steps_list.append({
                    "verify_graph": {
                        "expected_graph_state": expected.strip(),
                        "wait_seconds": int(vfy.get("wait_seconds", 30)),
                    }
                })
    except Exception:
        pass  # Catalog unavailable — use placeholder steps.

    steps = json.dumps(steps_list)
    rollback_steps = json.dumps(rollback_steps_list)
    try:
        return client._http_json(
            "POST",
            "/api/approvals",
            {
                "detection_id": detection_id,
                "playbook_id": playbook_id,
                "rule_id": rule_id,
                "environment_archetype": environment_archetype,
                "site_id": site_id,
                "steps_json": steps,
                "rollback_steps_json": rollback_steps,
            },
        )
    except Exception as exc:
        return {"error": str(exc)}


def check_change_context(client: "BonsaiClient", device_address: str) -> dict:
    """Check if a device is in an active change window (maintenance, CHG ticket, AAP job).

    Returns whether the device has active change requests, and their details.
    Use this early in an investigation to distinguish expected changes (during a
    planned maintenance window) from unexpected changes (possible rogue activity).
    """
    try:
        return client._http_json("GET", f"/api/changes/context/{device_address}")
    except Exception as exc:
        return {"error": str(exc)}


# ── tool registry ─────────────────────────────────────────────────────────────

# Anthropic tool schema for each tool.
TOOL_SCHEMAS: list[dict[str, Any]] = [
    {
        "name": "get_blast_radius",
        "description": "Return devices reachable within max_hops of a device via topology links.",
        "input_schema": {
            "type": "object",
            "properties": {
                "device_address": {"type": "string", "description": "Device address (host:port)"},
                "max_hops": {"type": "integer", "default": 3, "description": "Max hop depth (1-5)"},
            },
            "required": ["device_address"],
        },
    },
    {
        "name": "get_application_impact",
        "description": "Applications that run services on or near the device.",
        "input_schema": {
            "type": "object",
            "properties": {
                "device_address": {"type": "string"},
            },
            "required": ["device_address"],
        },
    },
    {
        "name": "query_graph",
        "description": "Execute a read-only Cypher query against the bonsai graph.",
        "input_schema": {
            "type": "object",
            "properties": {
                "cypher": {"type": "string", "description": "Read-only Cypher (no CREATE/DELETE/SET)"},
            },
            "required": ["cypher"],
        },
    },
    {
        "name": "ask_graph",
        "description": (
            "Ask a natural language question about the network and get results. "
            "The server generates a Cypher query from the question using an LLM, "
            "executes it, and returns the generated Cypher, explanation, and result rows. "
            "Use this when you need to explore the graph but don't know the exact Cypher syntax."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "question": {"type": "string", "description": "Natural language question, e.g. 'Which devices are connected to spine1?'"},
            },
            "required": ["question"],
        },
    },
    {
        "name": "get_recent_detections",
        "description": "DetectionEvents for a device in the last N seconds.",
        "input_schema": {
            "type": "object",
            "properties": {
                "device_address": {"type": "string"},
                "window_secs": {"type": "integer", "default": 300},
            },
            "required": ["device_address"],
        },
    },
    {
        "name": "get_remediation_history",
        "description": "Past remediations attempted for a device.",
        "input_schema": {
            "type": "object",
            "properties": {
                "device_address": {"type": "string"},
            },
            "required": ["device_address"],
        },
    },
    {
        "name": "summarise",
        "description": "Record a final summary narrative and conclude the investigation.",
        "input_schema": {
            "type": "object",
            "properties": {
                "text": {"type": "string", "description": "Plain-language investigation summary"},
            },
            "required": ["text"],
        },
    },
    {
        "name": "check_change_context",
        "description": (
            "Check if a device is currently in an active change window "
            "(ServiceNow CHG ticket, AAP/Ansible job, or manual maintenance). "
            "Use this to determine if a fault might be expected due to planned maintenance. "
            "Returns active change requests with their type, risk level, and time window."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "device_address": {"type": "string", "description": "Device address to check"},
            },
            "required": ["device_address"],
        },
    },
    {
        "name": "propose_playbook",
        "description": (
            "Submit a playbook proposal to the approval queue. "
            "NEVER executes directly — requires operator approval."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "detection_id": {"type": "string"},
                "playbook_id": {"type": "string"},
                "rationale": {"type": "string"},
            },
            "required": ["detection_id", "playbook_id", "rationale"],
        },
    },
]

# Map name -> callable (without client arg; caller passes client separately)
TOOL_FN: dict[str, Any] = {
    "get_blast_radius": get_blast_radius,
    "get_application_impact": get_application_impact,
    "query_graph": query_graph,
    "ask_graph": ask_graph,
    "get_recent_detections": get_recent_detections,
    "get_remediation_history": get_remediation_history,
    "check_change_context": check_change_context,
    "summarise": summarise,
    "propose_playbook": propose_playbook,
}

# Tools that do NOT need the client (pure functions)
_NO_CLIENT = {"summarise"}


def call_tool(client: "BonsaiClient", name: str, inputs: dict) -> dict:
    """Dispatch a tool call; inject client unless the tool is client-free."""
    fn = TOOL_FN.get(name)
    if fn is None:
        return {"error": f"unknown tool: {name}"}
    if name in _NO_CLIENT:
        return fn(**inputs)
    return fn(client, **inputs)
