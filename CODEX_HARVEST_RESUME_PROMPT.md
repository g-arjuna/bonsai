# Codex — Harvest Session Prompt

> Paste this at the start of a new Codex session. Each run produces one playbook YAML entry for one operation + vendor combination. Run the session multiple times (one per target) to build the catalog.

---

You are helping me build the remediation playbook library for an open-source
project called **bonsai** — a streaming-first graph-native network state engine
that detects network anomalies via gNMI telemetry and closes the loop with
gNMI Set remediations.

I will give you, per session:
- A target **operation** (e.g., "BGP neighbor admin-state bounce")
- A target **vendor** (one of: `nokia_srl`, `cisco_xrd`, `juniper_crpd`, `arista_ceos`)
- **Vendor documentation** (pasted as content, or given as a URL you should fetch if tooling permits)
- **YANG tree dump** for that vendor (pasted as content — ground truth for path validation)

You produce: **one YAML playbook entry** that conforms exactly to the bonsai
playbook schema, validated against the YANG tree dump, and written to a file
under `playbooks/library/`.

## The bonsai playbook schema

Every entry in `playbooks/library/<detection_rule_id>.yaml` follows this shape.
Multiple vendors for the same detection rule live in one file under the same
`playbooks:` list.

```yaml
detection_rule_id: <snake_case matching a bonsai detection rule>
description: |
  One or two sentences describing the detection this playbook addresses.

playbooks:
  - name: <short_snake_case_name>           # e.g. srl_bgp_admin_state_bounce
    vendor: <vendor id from the four above>
    operation: <the target operation in plain English>
    description: |
      One or two sentences describing what this playbook does and when to use it.
    risk_tier: safe | disruptive | last_resort
    verified_on: []                          # empty at harvest; humans fill later
    confidence: unverified                   # always at harvest
    preconditions:
      - <Python expression referencing features.* fields>
    steps:
      - gnmi_set:
          path: "<yang path with {placeholders}>"
          value: '<RFC 7951 JSON string>'
      - sleep: <seconds>                     # only if multi-step
      - gnmi_set:
          path: "..."
          value: "..."
    verification:
      wait_seconds: <int>
      expected_graph_state: |
        <Cypher query that returns count > 0 when recovery is confirmed;
         use $device_address and other $-prefixed parameters>
    source_doc:
      url: "<URL of the doc page>"
      excerpt_summary: |
        2–3 sentence summary of what the doc says about this operation.
    notes: |
      Caveats, gotchas, firmware-specific behaviour, UNKNOWN items that need
      human research.
```

## Mandatory rules — read every time

1. **gNMI paths must exist in the YANG tree dump I provide.** If a path you
   want to emit is not in the tree dump, you MUST flag it. Do not invent paths.
   Common failure mode: hallucinating Cisco-IOS-XR module names or SR Linux
   container paths that sound right. Always grep the tree dump first.

2. **If the operation is not gNMI-remediable on this vendor** (e.g., Junos
   needs `<rpc>` not gNMI Set; XRd has no `-act` model for this operation),
   produce an entry with:
   - empty `steps: []`
   - `risk_tier: last_resort`
   - A clear note in `notes:` stating this operation is not gNMI-remediable
     on this vendor and explaining the alternative (CLI, NETCONF RPC, etc.)
   This is a valid output, not a failure — honest gaps make the catalog
   trustworthy.

3. **Preconditions are Python expressions** referencing fields on the
   `Features` dataclass from `python/bonsai_sdk/detection.py`:
   - `features.device_address` (str)
   - `features.peer_address` (str, for BGP)
   - `features.old_state`, `features.new_state` (str, for BGP transitions)
   - `features.if_name` (str, for interface rules)
   - `features.oper_status` (str)
   - `features.event_type` (str)
   - `features.occurred_at_ns` (int)
   Use bare expressions, not f-strings. Example: `features.peer_address != ""`.

4. **Values are RFC 7951 JSON.** String values are double-quoted inside single
   quotes to survive YAML: `value: '"disable"'`. Object values are JSON
   objects: `value: '{"neighbor-address":"{peer_address}","soft":true}'`.

5. **Placeholders** use `{feature_name}` syntax for substitution at runtime,
   resolved from the `Features` dict. Example:
   `path: "neighbor[peer-address={peer_address}]/admin-state"`.

6. **Risk tier guidelines**:
   - `safe`: reversible, minimal traffic impact (admin-state toggle, BGP soft clear)
   - `disruptive`: causes reconvergence or brief outage (interface bounce, process restart)
   - `last_resort`: device reboot, destructive config change, anything requiring human confirmation

7. **Verification Cypher must be valid and testable**. Available nodes:
   - `Device(address, vendor, hostname, updated_at)`
   - `Interface(device_address, name, in_pkts, out_pkts, in_octets, out_octets, in_errors, out_errors, updated_at)`
   - `BgpNeighbor(device_address, peer_address, peer_as, session_state, established_transitions, updated_at)`
   - `LldpNeighbor(device_address, local_if, neighbor_id, chassis_id, system_name, port_id, updated_at)`
   - `StateChangeEvent(device_address, event_type, detail, occurred_at)`
   Edges: `HAS_INTERFACE`, `PEERS_WITH`, `HAS_LLDP_NEIGHBOR`, `CONNECTED_TO`, `REPORTED_BY`.
   Typical verification pattern: count the entity in its recovered state.

8. **Do not produce a playbook that modifies unrelated configuration.** If the
   doc shows a multi-step procedure that touches state beyond the operation,
   extract only the strictly required steps.

9. **Output only the YAML file content**, nothing else. No preamble, no
   trailing commentary, no code fence language tags other than the standard
   YAML block. The content should be copy-pasteable directly into
   `playbooks/library/<detection_rule_id>.yaml`.

## Worked example

**Input:**
- Target operation: `BGP neighbor admin-state bounce`
- Vendor: `nokia_srl`
- Detection rule id: `bgp_session_down`
- Documentation: Nokia SR Linux BGP configuration guide showing `admin-state`
  leaf under `network-instance/protocols/bgp/neighbor` with enum values
  `enable` / `disable`
- YANG tree dump contains:
  ```
  +--rw network-instance* [name]
     +--rw protocols
        +--rw bgp
           +--rw neighbor* [peer-address]
              +--rw admin-state?   srl_nokia-common:admin-state
  ```

**Output:**

```yaml
detection_rule_id: bgp_session_down
description: |
  BGP session transitioned from established to idle. Playbooks below bring
  the session back up via vendor-appropriate gNMI Set operations.

playbooks:
  - name: srl_bgp_admin_state_bounce
    vendor: nokia_srl
    operation: BGP neighbor admin-state bounce
    description: |
      Disable then re-enable a BGP neighbor's admin-state to force a clean
      session reset. Used when a peer session has dropped and a soft reset
      is appropriate.
    risk_tier: safe
    verified_on: []
    confidence: unverified
    preconditions:
      - features.peer_address != ""
      - features.old_state == "established"
    steps:
      - gnmi_set:
          path: "network-instance[name=default]/protocols/bgp/neighbor[peer-address={peer_address}]/admin-state"
          value: '"disable"'
      - sleep: 1
      - gnmi_set:
          path: "network-instance[name=default]/protocols/bgp/neighbor[peer-address={peer_address}]/admin-state"
          value: '"enable"'
    verification:
      wait_seconds: 30
      expected_graph_state: |
        MATCH (d:Device {address: $device_address})-[:PEERS_WITH]->(n:BgpNeighbor {peer_address: $peer_address})
        WHERE n.session_state = "established"
        RETURN count(n) > 0
    source_doc:
      url: "https://documentation.nokia.com/srlinux/24-7/books/config-basics/bgp.html"
      excerpt_summary: |
        SR Linux exposes BGP neighbor administrative state under the
        network-instance/protocols/bgp/neighbor subtree. Setting admin-state to
        disable tears down the session; setting it to enable permits
        re-establishment. Default network-instance is `default`.
    notes: |
      Assumes the neighbor lives in the default network-instance. Adjust the
      path for non-default VRFs. Lab validation pending — no entries in
      verified_on yet.
```

## Target for this session

**Operation**: `<TO FILL IN PER SESSION>`
**Vendor**: `<TO FILL IN PER SESSION>`
**Detection rule id**: `<TO FILL IN PER SESSION>`

## Documentation

`<PASTE OR REFERENCE THE RELEVANT VENDOR DOC EXCERPT HERE — 200-1000 LINES OF THE MOST RELEVANT MATERIAL>`

## YANG tree dump for this vendor

`<PASTE THE RELEVANT SECTION OF THE pyang TREE OUTPUT — ENOUGH TO COVER THE
PATHS NEEDED FOR THIS OPERATION; TYPICALLY 50-300 LINES>`

## Your task

1. Scan the YANG tree dump for paths relevant to the target operation.
2. Read the documentation for the intended procedure.
3. Produce one YAML playbook entry following the schema above.
4. Cross-check every path in your output against the YANG tree dump.
5. If any path is missing from the tree dump, flag it explicitly in `notes:`
   and consider whether the operation is actually gNMI-remediable on this
   vendor at all.
6. Output only the YAML. No commentary, no explanation, no alternatives.

Begin.
