# PHASE5_ADDENDUM_REMEDIATION_AND_NLQUERY.md

> Addendum to `PHASE5_REVIEW_AND_DESIGN.md`. Covers two topics that the main document handled briefly and the primary architect (you) wanted expanded: (1) how bonsai organically builds up the "know what to do" capability for remediations across diverse vendor behaviour, and (2) the natural-language query layer over the graph.
>
> **Neither requires deviation from the main Phase 5 baseline.** Both are additive. The existing seams are sufficient.

---

## Part A — How Bonsai Learns to Remediate

### The real problem you stumbled into

Rules fire. Detection exists. A BGP session went from established to idle. **Now what?**

The honest answer in every real system — commercial or open source — is that *the action is not derivable from the detection alone*. The same symptom ("BGP session down") has different correct responses depending on:

- Vendor (SRL needs admin-state toggle; XRd supports `clear bgp` action; Junos is different again)
- Topology position (flapping peer on a leaf is different from flapping peer on a route reflector)
- What else is happening (one peer down vs all peers down → different root cause → different fix)
- Confidence (new deployment vs mature link that suddenly broke)
- Whether the fix already failed once in the last hour (circuit breaker territory)

You cannot hard-code every combination. You cannot train a model to invent gNMI paths from scratch reliably. You cannot ship without some answer to this question.

The right architecture is **three layers that accrue capability over time**, each layer strictly extending the previous one. Do not skip layers.

### Layer 1: Curated playbook library (you already started this)

**What it is**: a structured registry of `{detection_pattern, vendor, action_template}` tuples, authored by humans who know the networks. Today in `python/bonsai_sdk/remediations.py` you have:

```python
_BGP_ADMIN_STATE_PATH: dict[str, str] = {
    "nokia_srl": "network-instance[name=default]/protocols/bgp/neighbor[peer-address={peer}]/admin-state",
}
```

This is a playbook library in embryonic form. Generalise it. The right shape is a per-vendor, per-detection playbook catalog that any operator can extend without writing Python.

**Proposed module**: `python/bonsai_sdk/playbooks/`

Structure:
```
playbooks/
  __init__.py
  catalog.py           # loads YAML, exposes query API
  library/
    bgp_session_down.yaml
    interface_admin_shut.yaml
    ospf_adjacency_lost.yaml
    mpls_lsp_down.yaml
    ...
```

Example `bgp_session_down.yaml`:
```yaml
detection_rule_id: bgp_session_down
description: |
  BGP session transitioned from established to idle.
  Playbooks are candidate remediations; selection is by vendor + preconditions.

playbooks:
  - name: srl_admin_state_bounce
    vendor: nokia_srl
    description: Disable then re-enable the BGP neighbor's admin-state.
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

  - name: xrd_clear_bgp_soft
    vendor: cisco_xrd
    description: Issue a soft clear on the BGP neighbor via gNMI action.
    preconditions:
      - features.peer_address != ""
    steps:
      - gnmi_set:
          path: "Cisco-IOS-XR-ipv4-bgp-act:clear-bgp/neighbor"
          value: '{"neighbor-address":"{peer_address}","soft":true}'
    verification:
      wait_seconds: 30
      expected_graph_state: |
        MATCH (d:Device {address: $device_address})-[:PEERS_WITH]->(n:BgpNeighbor {peer_address: $peer_address})
        WHERE n.session_state = "established"
        RETURN count(n) > 0
```

**Why YAML**: a network engineer can author a new playbook without touching Python. An ADR is written. The YAML is reviewed. It loads next start. This is the same discipline as ServiceNow runbooks but in a format that lives next to your code.

**Why "verification" is a graph query**: this is distinctive. Most remediation systems verify by re-running the trigger. Bonsai verifies against graph state because the graph is the source of truth for *current* state. The remediation returns success only when the graph reflects recovery — which is the only definition of "it worked" that matters.

**Executor changes** (extension of today's `RemediationExecutor`):

```python
class PlaybookExecutor:
    def __init__(self, catalog: PlaybookCatalog, client: BonsaiClient, ...):
        self._catalog = catalog
        ...

    def select_playbook(self, detection: Detection, vendor: str) -> Optional[Playbook]:
        candidates = self._catalog.for_detection(detection.rule_id, vendor)
        # Filter by precondition evaluation on features
        applicable = [p for p in candidates if p.preconditions_met(detection.features)]
        if not applicable:
            return None
        # Layer 1 selection: first-match (ordered in YAML by preference)
        return applicable[0]

    def execute(self, playbook: Playbook, detection: Detection):
        for step in playbook.steps:
            ...  # dispatch to gnmi_set, sleep, etc via PushRemediation
        # Verification is a graph query
        return self._verify(playbook.verification, detection)
```

**How this layer accrues capability**: every time you hit a new remediation scenario in the lab, you write a YAML playbook. Over months, the catalog grows. This is the human-in-the-loop phase and it is unavoidable. Anyone who tells you ML will invent remediations from scratch without a curated base case is selling something.

**Concrete starter set** (write these, in order):
1. `bgp_session_down` — SRL admin-state bounce (already exists in prose, put in YAML)
2. `interface_admin_shut` — just un-shut, per vendor
3. `ospf_adjacency_lost` — interface bounce (the usual first try)
4. `mpls_lsp_down` — trigger CSPF re-optimization (SR-specific)
5. `bgp_all_peers_down` — *no playbook, alert only* — if all peers are down, the device is probably isolated; automation cannot fix it

That fifth entry is important. **Not every detection has a remediation. Saying "this needs human judgment" is a valid playbook choice.** Encode it explicitly.

### Layer 2: Learned playbook selection (Phase 5 Model C)

This is already in the Phase 5 baseline. Re-reading it through the playbook lens sharpens the shape.

When Layer 1 has multiple playbooks that all have their preconditions met, who picks? A classifier trained on outcomes. Input: detection features + candidate playbook names. Output: probability each playbook succeeds.

**Training data**: every Remediation node in the graph. `status` field labels the outcome. Over a few weeks of Phase 4 running, if you have 3 playbooks for `bgp_session_down` and 200 fires, you have enough data to learn which one to prefer for which feature pattern.

**Integration point**:

```python
class LearnedSelector:
    """Extends PlaybookExecutor.select_playbook with ML-based scoring when multiple playbooks are applicable."""
    def __init__(self, model_path: str, confidence_floor: float = 0.7):
        self._model = load(model_path)
        self._floor = confidence_floor

    def rank(self, candidates: list[Playbook], features: Features) -> list[Playbook]:
        if len(candidates) <= 1:
            return candidates
        vector = features_to_vector(features)
        scores = {p.name: self._model.predict_proba(vector, p.name) for p in candidates}
        ranked = sorted(candidates, key=lambda p: scores[p.name], reverse=True)
        # If top score is below confidence floor, fall back to YAML-order (Layer 1).
        if scores[ranked[0].name] < self._floor:
            return candidates  # deterministic fallback
        return ranked
```

The confidence floor fallback is not optional. When the model is uncertain, you fall back to the human-authored ordering. This is what prevents ML overreach.

### Layer 3: LLM-constructed actions (the interesting frontier)

This is where your question really points. What happens when there's *no playbook* for a new vendor or a new detection type? Can bonsai figure it out?

**The honest answer**: with careful scaffolding, yes — for a narrow class of cases and with a mandatory human-approval gate.

**How it works**:

```
  Detection fires, no playbook matches
       │
       ▼
  LLM Action Builder (new module)
       │
       ├── Input context:
       │     - Detection features
       │     - Device vendor, model, gNMI capabilities (from Capabilities RPC cache)
       │     - YANG schema paths available on this device (discovered at connect time)
       │     - Similar playbooks for other vendors (few-shot examples)
       │     - The detection reason text
       │
       ▼
  LLM proposes:
    - A vendor-appropriate YANG path
    - A gNMI Set value
    - A verification query
    - A reasoning trace
       │
       ▼
  HUMAN APPROVAL GATE (always, no auto-apply)
       │
       ▼
  If approved → written as a new YAML playbook (with provenance: "LLM-suggested, human-approved")
  If executed → runs through the normal PlaybookExecutor path
```

**This is the organic capability-building loop**. Each LLM-suggested-then-human-approved remediation becomes a new YAML playbook in the catalog. The library grows *with knowledge of your network*, not with whatever OpenConfig models theoretically exist.

**Why this is architecturally safe**:

1. The LLM *never* directly calls `gnmi_set`. It proposes; humans approve; the executor runs the YAML.
2. The approval gate is a real UI interaction, not a config flag. Phase 6 UI can have an "LLM suggestions" pane. Until Phase 6, the suggestion appears in the event stream, a human writes the YAML by hand. Either way, no LLM-in-the-execution-path.
3. The Rust `PushRemediation` RPC does not change. The same gNMI Set code runs whether the playbook came from a human or from an LLM-seeded human. The credential and transport discipline is preserved.
4. The LLM operates in a closed context: the gNMI capabilities that the device *actually* advertised. It cannot hallucinate paths the device doesn't support, because the schema passed to it comes from the live Capabilities response cached in bonsai.

**What tools the LLM needs**:

A single MCP-style tool, exposed over the existing gRPC API:

```proto
rpc SuggestRemediation(SuggestRemediationRequest) returns (SuggestRemediationResponse);

message SuggestRemediationRequest {
  string detection_id = 1;  // pulls context from the graph
}

message SuggestRemediationResponse {
  string proposed_yaml = 1;      // the YAML playbook the LLM suggests
  string reasoning    = 2;       // natural-language explanation
  float  confidence   = 3;
  repeated string similar_playbooks = 4;  // few-shot examples used
}
```

The Rust side of this RPC:
1. Reads the DetectionEvent from the graph
2. Looks up the Device + its cached Capabilities
3. Queries similar existing playbooks for other vendors
4. Calls an LLM (Anthropic API via the `anthropic` crate or similar) with a structured prompt
5. Validates the returned YAML parses as a valid playbook schema
6. Returns the proposal

Python layer calls this RPC and surfaces the proposal to the user. The user pastes the YAML into the catalog (or clicks approve in the eventual UI).

**When to build this**: Phase 5.5 or later, after the playbook catalog has grown organically to at least 10–15 hand-written entries. The LLM is better at extending a pattern than inventing one.

### Summary of the three layers

| Layer | Mechanism | What grows the capability | Failure mode |
|---|---|---|---|
| 1. Playbook library | YAML files, human-authored | Engineers encoding their knowledge | Coverage gaps for new scenarios |
| 2. Learned selection | Model C classifier on outcomes | Time running the system + outcome labels | Confidence floor fallback to Layer 1 |
| 3. LLM suggestion | Few-shot prompted proposals | New detections + human review | Human approval gate; never auto-applies |

**None of this changes the architecture in `PHASE5_REVIEW_AND_DESIGN.md`**. It's additive:
- Playbook catalog is a new directory, new module
- Layer 2 is Model C from the main baseline, viewed through a playbook lens
- Layer 3 is a new gRPC RPC and a new UI interaction (eventually)
- `RemediationExecutor` evolves into `PlaybookExecutor`, same responsibilities, richer inputs
- `PushRemediation` RPC on the Rust side is unchanged

**Action to add to the pre-Phase-5 list**: add `Playbook` and `PlaybookExecutor` as Phase 5.0 design work, before any ML is trained. Convert the existing hardcoded `_BGP_ADMIN_STATE_PATH` and `PLAYBOOKS` dict into a YAML playbook as the first entry in the new catalog. This is clean-up, not scope expansion.

---

## Part B — Natural Language Queries over the Graph

### Why this wasn't in the main Phase 5 doc

I deferred it intentionally. The reason was scope discipline: the Phase 5 baseline is about the *detect-predict-heal loop*, which is a closed system with no human in the inference path. Natural-language queries are a different kind of capability — an *observation interface* for humans asking ad-hoc questions. Mixing them risked confusing "Phase 5 ML" with "Phase 5 LLM features" and ending up with neither shipped.

But you're right that this capability matters. And it's genuinely easy now because of a decision made long ago: the gRPC `Query()` RPC exposes raw Cypher. An LLM with access to that RPC and the graph schema can answer most questions a network engineer would ask.

### Architecture — small, clean, already unblocked

```
  User: "What happened at 10 am on srl-spine1?"
                │
                ▼
  NLQuery module (Python) ──────┐
                │                │
                ▼                │
  LLM (Claude/GPT/local) ◄───────┤ system prompt includes:
                │                │   - graph schema (node tables, edge tables)
                ▼                │   - few-shot examples of Cypher queries
  Proposed Cypher                │   - today's date, timezone, device list
                │                │
                ▼                │
  Self-check: parse + sanity     │
                │                │
                ▼                │
  gRPC Query(cypher) ────────────┘
                │
                ▼
  Rows returned
                │
                ▼
  LLM summarises rows in natural language
                │
                ▼
  Answer to user
```

Two LLM calls (query planning + answer rendering). One deterministic gRPC call in the middle. Bonsai itself does not change.

### Concrete module

`python/bonsai_sdk/nlquery.py` — maybe 200 lines.

```python
class NLQuery:
    def __init__(self, client: BonsaiClient, llm: LLMProvider):
        self._client = client
        self._llm = llm
        self._schema = self._introspect_schema()  # cached graph schema

    def ask(self, question: str) -> NLResponse:
        # Step 1: plan
        cypher = self._llm.plan_query(
            question=question,
            schema=self._schema,
            examples=FEW_SHOT_EXAMPLES,
            context={"now": datetime.utcnow(), "devices": self._client.get_devices()},
        )
        # Step 2: validate
        if not self._looks_safe(cypher):
            return NLResponse(question=question, error="generated query rejected")
        # Step 3: run
        rows = self._client.query(cypher)
        # Step 4: render
        answer = self._llm.render_answer(question=question, cypher=cypher, rows=rows)
        return NLResponse(question=question, cypher=cypher, rows=rows, answer=answer)
```

Key design details:

**`_introspect_schema`**: runs once at init. Gets the node labels, edge types, and their properties from the graph. Caches them. Becomes part of the LLM system prompt. This is why vendor neutrality in your schema matters — the LLM sees `Device.vendor` as a property and can reason about it generically.

**`FEW_SHOT_EXAMPLES`**: a hand-curated list of `(natural_language, cypher)` pairs. Start with ten:
- "What devices are there?" → `MATCH (d:Device) RETURN d.hostname, d.address, d.vendor`
- "Which BGP sessions are down?" → `MATCH (n:BgpNeighbor) WHERE n.session_state <> 'established' RETURN ...`
- "What happened in the last hour?" → `MATCH (e:StateChangeEvent) WHERE e.occurred_at > $cutoff RETURN ...`
- "Show me detections on srl-spine1 today" → with date range + device filter
- etc.

These examples are the most valuable thing in this whole module. Time spent curating them beats time spent tuning the LLM.

**`_looks_safe`**: rejects queries containing DELETE, DETACH DELETE, DROP, CREATE, MERGE. Read-only interface. This is a hard guard, not an ML judgment call. The Rust side already has the `Query()` RPC running as an unconstrained Cypher endpoint — for natural language, we constrain to read-only by rejecting unsafe tokens before dispatch.

**Answer rendering**: the LLM receives the original question, the Cypher it generated, and the rows. It produces a plain-English answer. No tool-use loop needed; this is straight summarisation.

### Why this fits without deviation

- The existing gRPC `Query()` RPC is the sole integration point. Zero Rust changes required.
- The graph schema is already stable enough to describe in a prompt (the main DECISIONS.md decisions about node labels and edge types pay off here).
- Layered on top, not inside. A user who never uses NL queries gets exactly the system we designed. A user who does gets an additional surface.
- LLM-agnostic: `LLMProvider` is a trait (Claude, GPT-4, Ollama local, whatever). Matches the "LLM-agnostic" founding principle.

### Where this gets interesting (later)

Once this works, three natural extensions appear:

1. **Temporal questions** ("what did the graph look like at 10 am?") — this is where the deferred temporal bitemporal work becomes required. The NL layer will start asking questions that today's schema can't answer. That's the forcing function to build the bitemporal layer.

2. **Detection reasoning** ("why did bonsai think this was a BGP flap?") — the LLM reads the DetectionEvent, its `features_json`, the rule's source code, and explains. This is a killer demo feature.

3. **Remediation explanation** ("what did bonsai do and why?") — the LLM traverses `Remediation -[:RESOLVES]-> DetectionEvent`, reads the playbook YAML, and produces a human-readable incident timeline.

All three of these are natural extensions of the basic NL query. None require architectural changes.

### What to build when

**Phase 5.0 (with the ML work)**: don't build NLQuery. Focus on the closed loop.

**Phase 5.5 (immediately after Phase 5 demo)**: build NLQuery as a two-week side project. It's cheap, it's useful, it's demo-able, and it becomes the UI's natural next feature.

**Phase 6 (UI)**: the UI's chat box is an NLQuery frontend. The three views (topology, events, closed-loop trace) + a chat box is the complete demo product.

### Action to add to the roadmap

Add "Phase 5.5 — NLQuery module" as a distinct milestone between Phase 5 (ML) and Phase 6 (UI). It is small enough to not deserve its own phase in the traditional sense, but large enough that it should be called out so it doesn't get absorbed into "Phase 6 UI work" and deprioritised.

---

## Combined Summary

Both the remediation capability building and the natural language layer plug cleanly into the architecture already designed. Neither requires changes to `PHASE5_REVIEW_AND_DESIGN.md`. They extend it.

The remediation story is a three-layer stack — playbook library, learned selection, LLM suggestion with human approval. The first two are part of Phase 5. The third is Phase 5.5 or later, after the catalog has matured.

The natural language query is a separate small module sitting on top of the existing `Query()` gRPC RPC. It is Phase 5.5 work. It unlocks the UI's chat interface in Phase 6.

Neither deviates from founding principles. Both extend capability organically rather than through architectural change. Both are things that look simple after the main baseline is in place — which is the signal that the main baseline was done right.

The only addition to the pre-Phase-5 action list from this addendum is:

**8. Convert existing hardcoded remediation paths into the first YAML playbook.** Scaffolds the `playbooks/` directory, creates `catalog.py`, moves `_BGP_ADMIN_STATE_PATH` into `library/bgp_session_down.yaml`. No behaviour change. Prepares for Layer 2/3 work that comes later.

That brings the pre-Phase-5 action list from 7 to 8 items. Still all small, still all one-session tasks, still all hygiene before ML code begins.

---

*Addendum version 1.0 — issued alongside PHASE5_REVIEW_AND_DESIGN.md v1.0, covers remediation capability organic growth and natural-language query layer.*
