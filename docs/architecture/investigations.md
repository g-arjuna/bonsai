# Investigations — Architecture and Operator Guide

> Authored DV2, 2026-05-17. Addresses D2-3 T5: operator confusion about why
> Investigations view shows zero sessions despite active detections.

## What an Investigation Is

An **Investigation** is a named AI-assisted troubleshooting session anchored to
one or more `DetectionEvent` rows. It accumulates an ordered audit trail of
tool calls (graph queries, blast-radius lookups, device detail fetches) with
their inputs, outputs, and timestamps.

An investigation is **not** created automatically for every detection. It is a
deliberate operator action for detections that warrant deeper analysis.

## When to Open an Investigation

Open an investigation when:

1. A detection fires on a device you haven't seen fail before.
2. Multiple incidents are co-firing and the blast radius isn't obvious.
3. An incident repeats within 30 minutes (recurrence indicator on the detection row).
4. Remediation was attempted and the fault persists.

The Investigations view showing **zero sessions** is correct after a fresh
deploy — investigations are operator-triggered, not automatic. This is intentional:
automatic investigations would produce noise on noisy networks.

## Trigger Conditions

### Manual trigger (UI)

From the Investigations view, click **"Open investigation"**, supply:
- `detection_id` — the UUID from `/api/detections` or the incident's root detection
- `device_address` — the primary device under investigation

### Manual trigger (API)

```http
POST /api/investigations
Content-Type: application/json

{
  "detection_id": "3f8a1c2d-...",
  "device_address": "172.100.103.12:57400",
  "trigger": "operator"
}
```

`trigger` values: `"operator"` (manual) | `"recurrence"` (future: auto-triggered
when the same rule fires ≥3 times on the same device within 1 hour).

### Planned: automatic trigger on recurrence

The `"recurrence"` trigger is wired in the schema but the auto-trigger loop is
not yet implemented. When D2-3 / D2-11 lands the recurrence indicator in the
incident card, the auto-trigger can be enabled here. Track in backlog as
D2-3 T5 follow-up.

## Lifecycle

```
open  →  (tool calls accumulate)  →  complete
```

An investigation stays `open` until the operator calls:

```http
POST /api/investigations/{id}/complete
Content-Type: application/json

{
  "status": "resolved",
  "summary": "BGP session dropped due to interface lower-layer-down on spine01 eth0/1. Root cause: SFP pulled during maintenance.",
  "recommended_action": "Re-seat SFP or replace. Validate BFD session comes back.",
  "tokens_used": 0,
  "cost_usd": 0.0
}
```

Investigations never time out automatically. An operator can leave one open
across a maintenance window.

## Tool Call Audit Trail

Every tool the AI agent uses during an investigation is recorded:

```
GET /api/investigations/{id}/tool-calls
```

Returns an ordered list with:
- `tool_name` — e.g. `get_incident`, `get_device_blast_radius`, `query_graph`
- `input_json` — parameters passed
- `output_json` — result returned
- `called_at_ns` — epoch nanoseconds

This audit trail is the operator's evidence trail for change-management or
post-incident review.

## MCP Tools Available During an Investigation

The MCP server (`/mcp`) exposes these tools to the AI agent:

| Tool | Description |
|---|---|
| `get_incident` | Fetch grounded incident bundle (blast radius + runbook + live state) |
| `query_devices` | Filter device list by site, role, vendor, health |
| `get_device_blast_radius` | Impact assessment for one device |
| `list_active_detections` | Current anomalies in the system |
| `query_graph` | Read-only Cypher query against the topology graph |

All Cypher queries via `query_graph` are read-only gated — mutation keywords
are rejected. See `src/mcp_server.rs: is_readonly_cypher()`.

## Why Zero Investigations on a Fresh Deploy

The table `investigations` in the graph store starts empty. No detections
auto-create investigations. This is by design to avoid alert-storm during
initial onboarding.

**To generate your first investigation**:
1. Wait for or inject a detection (run `bash tests/chaos_harness/run.py --cycles 1`).
2. Note the detection UUID from `/api/detections` or the Incidents view.
3. POST to `/api/investigations` with that UUID.
4. The investigation appears in the Investigations view.

## Files

| File | Role |
|---|---|
| `src/http_server/remediation.rs` | `list_investigations_handler`, `create_investigation_handler`, `get_investigation_handler`, `complete_investigation_handler` |
| `src/graph/mod.rs` | `InvestigationRecord`, `ToolCallRecord` schema |
| `src/mcp_server.rs` | MCP tool definitions available during investigation |
| `ui/src/routes/Investigations.svelte` | UI surface |

## Guardrail

Never delete investigation records. They are the post-incident audit trail.
Archive is acceptable (move to `investigations_archive` table) after 90 days.
