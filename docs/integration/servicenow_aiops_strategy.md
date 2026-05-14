# ServiceNow AIOps Strategy

## Current state

Today Bonsai can:

- push topology and CI relationships toward ServiceNow through `/api/integrations/servicenow/aiops/sync`
- test ServiceNow reachability through `/api/integrations/servicenow/test`
- expose incidents, grounded incidents, device enrichment, and traces through the Bonsai HTTP API

What ServiceNow mostly receives today is a feed. Bonsai is useful, but the operator still has to pivot between raw alerts, topology context, and runbook clues.

## Gap analysis

A ServiceNow operator needs one actionable unit, not five separate API calls:

- what happened
- what is impacted
- why Bonsai believes it is real
- what procedure is relevant
- whether the event looks like maintenance, noise, or a genuine fault

Without that bundle, ServiceNow remains the place where alerts land, not the place where Bonsai’s network grounding becomes obvious.

## Future state: grounded incident bundle

The CV6 target is a grounded incident bundle that Bonsai can hand to ServiceNow as a single correlated object.

Bundle sections:

- `incident`: correlated detections, root event, severity, timing
- `topology_context`: blast radius, affected devices, affected services, likely upstream/downstream relationship
- `device_context`: site, environment, NetBox/CMDB enrichment, subscription readiness if relevant
- `procedural_context`: recurrence indicators, rule description, operator hints, runbook/playbook references
- `anomaly_context`: anomaly score and adversarial confidence when the GNN path is available
- `execution_context`: remediation status, approvals, trace link, rollback posture

The sample shape is captured in [sample_grounded_bundle.json](/home/arjuna/Desktop/bonsai/docs/integration/sample_grounded_bundle.json:1).

## API mapping

Existing Bonsai endpoints already provide most of the source material:

- `GET /api/incidents`
  produces the correlated incident shell
- `GET /api/incidents/{id}/grounded`
  produces the best current single-object grounding source
- `GET /api/devices/{address}`
  provides live device detail and recent detections
- `GET /api/devices/{address}/enrichment`
  provides NetBox and ServiceNow CMDB context
- `GET /api/trace/{id}`
  provides remediation and verification lineage
- `POST /api/integrations/servicenow/aiops/sync`
  keeps ServiceNow CI state aligned with Bonsai’s graph

Suggested ServiceNow landing points:

- Event Management alert/event tables receive the top-level incident signal
- Incident/work notes receive the grounded bundle summary and procedural references
- CMDB CI references receive affected-device and relationship linkage

## Operator workflow

1. ServiceNow receives or correlates an alert sourced from Bonsai.
2. The operator opens the correlated record and sees the grounded bundle summary.
3. Impacted devices and services are already attached, not inferred manually.
4. The record includes recurrence indicators and the best next procedure.
5. If Bonsai has proposed or executed remediation, the trace and approval state are visible.

The important product point is that Bonsai is not trying to replace ServiceNow AIOps. Bonsai is the L2/L3 network-grounding layer that ServiceNow does not natively own.

## Phasing

### CV6

- document the grounded bundle structure
- keep `/api/incidents/{id}/grounded` as the operator-facing precursor
- add a realistic sample bundle for demos and downstream contract discussion

### CV7

- add code that materializes the full grounded bundle from the existing APIs/store
- push the bundle, or a compacted version of it, into the ServiceNow incident workflow
- make playbook and remediation links first-class fields

### Post-GNN

- add anomaly score
- add adversarial confidence / maintenance-likelihood
- tune which bundles are informational versus action-driving

## Positioning against other AIOps tools

ServiceNow, Splunk, and Datadog are strong at ingestion, correlation, and workflow orchestration. Bonsai’s differentiation is narrower and more defensible:

- network-topology-native blast radius
- streaming gNMI-grounded state, not just logs and metrics
- direct linkage from incident to path profile, rule logic, and closed-loop action

That is the message to preserve in demos: Bonsai makes the AIOps platform smarter about the network.
