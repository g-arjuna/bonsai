# PHASE5_HARVEST_PROMPT.md

> The concrete prompt to use for harvesting vendor documentation into bonsai playbook YAML. Designed to be paired with one vendor doc page at a time and produce one playbook YAML entry per session.
>
> Companion to `PHASE5_PLAYBOOK_LIBRARY_BOOTSTRAP.md`.

---

## How to Use This Document

Each harvest session follows the same pattern:

1. Pick one target operation (e.g., "BGP neighbor admin-state toggle")
2. Find one authoritative vendor doc page describing it
3. Open a fresh Claude / GPT session (Sonnet is plenty for this)
4. Paste the **prompt template** below, filling in the `{OPERATION}`, `{VENDOR}`, and `{DOC_CONTENT}` slots
5. Review the output, spot-check against the YANG tree dumps you harvested separately
6. Validate in the lab if you have that vendor running
7. Commit the resulting YAML to `playbooks/library/`

Use one session per vendor per operation. Don't bundle five operations into one prompt — the output quality drops when the LLM has to reason about too many things at once.

---

## The Prompt Template

Copy this whole block into a new session. Fill in the three placeholders. Do not add extra context unless something is clearly missing — brevity produces better output than padding.

````
You are helping me build a network remediation playbook library for an open-source
project called bonsai. Bonsai detects network anomalies (BGP sessions going down,
interfaces erroring, etc.) and executes vendor-specific remediations via gNMI Set.

I need you to extract a remediation playbook from vendor documentation and produce
it as a structured YAML entry. Follow the schema and rules below exactly.

## Target operation
{OPERATION}

## Vendor
{VENDOR}  (one of: nokia_srl, cisco_xrd, juniper_crpd, arista_ceos)

## Source documentation
{DOC_CONTENT}
(paste the relevant section of vendor documentation here — a focused excerpt is
better than the whole page; aim for 200–1000 lines of the most relevant material)

## Output schema

Produce YAML conforming exactly to this structure. No commentary outside the YAML.
If information is missing, use the literal string `UNKNOWN` and add a `notes:`
field explaining what you couldn't determine.

```yaml
- name: <short_snake_case_name>           # e.g. srl_bgp_admin_state_bounce
  vendor: <vendor from the list above>
  operation: <the target operation>
  description: |
    One or two sentences describing what this playbook does and when to use it.
  risk_tier: safe | disruptive | last_resort
  verified_on: []                          # leave empty; humans fill in after lab test
  confidence: unverified                   # always 'unverified' at harvest time
  preconditions:
    - <Python-expression string referencing features.*>
  steps:
    - gnmi_set:
        path: "<yang path with {placeholders}>"
        value: '<RFC 7951 JSON string>'
    - sleep: <seconds>                     # only if required for multi-step
    - gnmi_set:                            # repeat as needed
        path: "..."
        value: "..."
  verification:
    wait_seconds: <int>
    expected_graph_state: |
      <Cypher query that returns a truthy count when recovery is confirmed;
       use $device_address and other $-prefixed parameters from features>
  source_doc:
    url: "<URL of the doc page>"
    excerpt_summary: |
      2-3 sentence summary of what the doc actually says about this operation.
  notes: |
    Any caveats, gotchas, firmware-version-specific behaviour, or items marked
    UNKNOWN that need human research.
```

## Rules

1. **gNMI paths must be plausible for this vendor.** If the doc shows CLI only
   and doesn't cover gNMI/NETCONF/YANG, mark the path as `UNKNOWN` and note it.
   Do not invent paths.

2. **Preconditions are Python expressions** that reference fields on a `Features`
   dataclass (see bonsai_sdk/detection.py). Common fields: `features.peer_address`,
   `features.old_state`, `features.new_state`, `features.device_address`,
   `features.if_name`, `features.oper_status`. Use bare expressions, not f-strings.

3. **risk_tier guidelines**:
   - `safe`: reversible, no traffic impact beyond brief reconvergence (e.g., BGP
     admin-state toggle on one neighbor)
   - `disruptive`: causes measurable traffic impact (e.g., interface shut, full
     routing-process restart)
   - `last_resort`: requires human confirmation even in auto-remediate mode
     (e.g., device reboot)

4. **verification.expected_graph_state** must be a Cypher query that returns a
   truthy count when recovery has occurred. Available node labels:
   `Device(address, vendor, hostname)`, `Interface(device_address, name, oper_status, ...)`,
   `BgpNeighbor(device_address, peer_address, session_state, peer_as, ...)`,
   `LldpNeighbor(...)`. Typical pattern: count the entity in its recovered state.

5. **If this operation cannot be performed via gNMI on this vendor** (e.g., Junos
   requires NETCONF RPC, not gNMI Set), produce an entry with empty `steps` and
   `risk_tier: last_resort` and clearly state in `notes` that this operation is
   not gNMI-remediable on this vendor.

6. **Do not produce a playbook that modifies unrelated configuration.** If the
   doc shows a multi-step procedure that touches unrelated state, extract only
   the steps strictly required for this operation.

7. **Use JSON-IETF format for values** — strings like `"disable"` must be double-
   quoted inside single quotes: `'"disable"'`. Object values are JSON objects.

## Worked example

For input:
- OPERATION: "BGP neighbor admin-state bounce"
- VENDOR: nokia_srl
- DOC_CONTENT: (Nokia SR Linux BGP configuration guide showing `admin-state` leaf
  under `network-instance/protocols/bgp/neighbor`, with values `enable` and `disable`)

Good output:

```yaml
- name: srl_bgp_admin_state_bounce
  vendor: nokia_srl
  operation: BGP neighbor admin-state bounce
  description: |
    Disable then re-enable a BGP neighbor to force a session reset. Used when a
    peer is stuck or flapping in a way that a clean reset resolves.
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
    url: "https://documentation.nokia.com/srlinux/..."
    excerpt_summary: |
      SR Linux exposes BGP neighbor administrative state under the
      network-instance/protocols/bgp/neighbor subtree. Setting admin-state to
      disable tears down the session; setting it to enable permits re-establishment.
  notes: |
    Tested on the documented SR Linux version range in the source. Lab validation
    pending. Assumes default network-instance; adjust path for non-default VRFs.
```

Now produce the YAML entry for the target operation and vendor above.
Output only the YAML. No preamble, no postamble, no code fence language tags
other than the standard YAML block.
````

---

## What To Do After the LLM Responds

1. **Read the output once top-to-bottom.** Does it make sense? Do the preconditions match Features fields that exist? Is the verification query correct Cypher?

2. **Cross-check the path against the YANG tree dump** you harvested with pyang. If the LLM's path doesn't appear in the tree dump, it's likely hallucinated. Common failure mode: Cisco XR paths — LLMs love to invent `Cisco-IOS-XR-*` module names that don't exist. Trust the YANG dump, not the LLM.

3. **Check risk_tier manually.** LLMs tend toward `safe` for almost everything. A device reboot should be `last_resort` regardless of what the model thought.

4. **Strip `UNKNOWN` fields before commit.** Either fill them in from YANG/docs or explicitly leave a `TODO` note.

5. **If you have the lab running, validate.** Run `gnmic` with the exact path and value from the YAML. Inject the relevant fault. Watch it recover. Record:
   ```yaml
   verified_on:
     - firmware: "24.7.2"
       last_tested: "2026-04-22"
       tested_by: "<your name>"
   confidence: high
   ```

6. **If you cannot validate right now**, leave `verified_on: []` and `confidence: unverified`. Honest gaps are better than false claims.

7. **Commit one PR per operation** with all its vendor variants. E.g., "Add BGP admin-state bounce playbook for SRL and XRd" — two entries, one file, one commit.

---

## Expected Tokens Per Session

A single harvest session, from input doc to committed YAML, uses roughly:

- **Input**: 2k–5k tokens (the doc excerpt + the prompt template)
- **Output**: 600–1200 tokens (one YAML entry)
- **Follow-up prompts** (usually 1–2): 1k–3k tokens combined

Total: roughly 5k–10k tokens per playbook entry. For a bootstrap catalog of 30–40 entries, this is well within a Claude Pro monthly budget with room to spare.

---

## A Short Note on Quality

The LLM is good at structure and bad at path-correctness. Trust it for:

- YAML shape
- Prose in `description`, `notes`, `excerpt_summary`
- Python expression syntax in `preconditions`
- Turning operational intent into Cypher verification queries

Distrust it for:

- Exact YANG paths (always cross-check)
- Vendor-specific behaviour nuances (the doc knows, the LLM paraphrases)
- Firmware version claims (the LLM will confidently invent ranges)

Your role as harvester is not "reviewer of correct LLM output." It is "domain expert who uses the LLM to do the boilerplate, then applies the one skill the LLM can't replicate — verifying the thing actually works on a real device." Keep that split clear and the catalog stays trustworthy.

---

*Version 1.0 — prompt designed for Claude Sonnet 4.6+ or GPT-4 class models. Should work without modification on smaller models with slightly lower output quality.*
