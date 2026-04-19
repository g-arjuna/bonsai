# PHASE5_PLAYBOOK_LIBRARY_BOOTSTRAP.md

> How to bootstrap the bonsai remediation playbook library from vendor documentation, YANG models, and hands-on experiments. Progressive, tractable, and honest about what machines can do versus what humans must do.
>
> Companion to `PHASE5_ADDENDUM_REMEDIATION_AND_NLQUERY.md`. Read that first — this document fills in the "how do we get to 50 playbooks without writing each one from scratch" question.

---

## The Honest Lay of the Land

Before proposing a method, a few realities that shape the approach.

**YANG models are not the same as gNMI paths.** A YANG model describes a schema. gNMI paths are runtime addresses into that schema. Most vendors publish their YANG models publicly (Cisco on GitHub, Nokia in the SR Linux docs, Juniper via `jnx-*` modules, Arista as part of EOS releases). These tell you what *exists*. They do not tell you which of those paths accept gNMI Set operations — you have to experiment or read vendor release notes.

**OpenConfig covers less than people assume.** OpenConfig is strong for telemetry paths (read) and weak for action paths (write). Actions like "clear BGP session" or "restart OSPF neighbor" are typically vendor-native YANG modules with RPC-style semantics. You cannot write a portable remediation for most repair actions — the vendor-specific path is unavoidable.

**Documentation quality varies enormously.** Nokia SR Linux docs are clean and include gNMI examples. Cisco IOS-XR docs exist but are scattered across Netconf/YANG guides that predate gNMI. Juniper's Junos YANG docs are thorough but assume NETCONF mental models. Arista's CloudVision docs bridge EOS CLI and OpenConfig but leave vendor-native gNMI thinly documented.

**The gNMI Set "action" pattern** (invoking an RPC-like operation via Set on an action container) is not universally supported. Cisco IOS-XR has `Cisco-IOS-XR-*-act` models for this. SR Linux does not — you mutate configuration state and the platform does the right thing. Junos uses `<rpc>` operations that are NETCONF-native; gNMI coverage varies. This means the *shape* of a playbook differs by vendor: SRL = config mutation; XR = action RPC; Junos = often stateful CLI; cEOS = OpenConfig where available, EOS-native where not.

Given all this, the approach cannot be "scrape every vendor's docs into a universal schema." The approach must be **progressive, multi-source, and accept that each playbook lands with vendor-specific framing.**

---

## The Three-Source Harvest Strategy

Every playbook has three knowledge inputs. Bootstrap means establishing each source and feeding all three into the playbook catalog.

### Source 1: YANG model introspection (automated, high coverage, lower specificity)

**What it gives you**: all paths that *exist* on a vendor's device, with their data types and descriptions. This is your map of the schema territory.

**Where to get it**:
- Cisco IOS-XR: `https://github.com/YangModels/yang/tree/main/vendor/cisco/xr`
- Nokia SR Linux: `https://github.com/nokia/srlinux-yang-models`
- Juniper Junos: `https://github.com/Juniper/yang`
- Arista EOS: bundled with EOS releases; also on GitHub
- OpenConfig: `https://github.com/openconfig/public`

**Tool**: `pyang` — Python YANG parser/validator. Install via `pip install pyang`. Converts YANG modules into structured output.

**Concrete harvest step**:

```bash
# Clone each vendor's YANG repo into a staging area
mkdir -p yang-harvest/{xr,srlinux,junos,eos,openconfig}

# For each repo, run pyang to produce a tree dump
pyang -f tree --tree-depth 6 \
  --path yang-harvest/xr \
  yang-harvest/xr/*/ietf-*.yang yang-harvest/xr/*/Cisco-IOS-XR-*.yang \
  > yang-harvest/xr.tree

# Same for others
pyang -f tree --tree-depth 6 yang-harvest/srlinux/**/*.yang > yang-harvest/srlinux.tree
```

The output looks like:

```
+--rw interfaces
   +--rw interface* [name]
      +--rw name
      +--rw config
      |  +--rw enabled?     boolean
      |  +--rw description? string
      +--ro state
         +--ro oper-status?      oper-status
         +--ro counters
            +--ro in-octets?    uint64
```

This is your map. Combined with the list of actions you're trying to support (from detection rules), it tells you where to look.

**What to do with it**: a Python script (`scripts/harvest_yang.py`) that:

1. Parses the tree dumps
2. Finds paths matching a set of patterns ("anything with `admin-state` or `enabled` under `bgp/neighbor`", "anything under an `-act` container for XR")
3. Produces a structured JSON catalog: `{vendor, module, path, datatype, description, writable}`

This JSON is not a playbook. It is the *ingredient list*. A human or an LLM turns it into playbooks with actual intent.

### Source 2: Vendor documentation (semi-automated, medium coverage, high specificity)

**What it gives you**: what the vendor intends you to do for specific operational tasks. The "how do I clear a BGP session" page from a vendor's automation guide.

**Where to get it**:
- Cisco: `https://www.cisco.com/c/en/us/td/docs/iosxr/...` (the Netconf/YANG programmability guides)
- Nokia SR Linux: the SR Linux docs site has gNMI examples under "programmable management"
- Juniper: the Junos Automation and Orchestration guide
- Arista: the EOS OpenConfig support matrix

**The realistic approach**: don't try to scrape these automatically. The pages are long, inconsistent, and much of the value is in examples that require context to understand. Instead:

**Use an LLM as an extraction assistant, one doc page at a time.** Feed the LLM a specific doc URL (or downloaded HTML) and a structured prompt asking: *"From this page, extract any gNMI Set paths, their intended purpose, required input values, and any caveats. Return as YAML."*

This is much slower than scraping but produces higher-quality entries. For a bootstrap library of 20–30 playbooks covering the common operations, this is one good weekend.

**Curate a target list of operations first**, not documentation pages. Examples:
- BGP neighbor admin-state bounce
- BGP session clear (soft/hard)
- OSPF interface cost adjustment
- OSPF neighbor adjacency reset
- Interface admin-state toggle
- Interface MTU change
- MPLS LSP reoptimization
- SR policy re-compute
- Static route add/remove
- ACL entry add/remove

For each operation × each vendor, harvest one entry. That's roughly 30–40 entries for a bootstrap library covering all four primary vendors.

### Source 3: Experimental validation (manual, low volume, highest confidence)

**What it gives you**: "this path actually works on this specific firmware version for this specific operation."

Every playbook entry must be validated in the lab before going into the catalog with confidence. This is the unavoidable manual step. It is also the step where most of your learning happens.

**Flow**:

1. Candidate playbook YAML written from Sources 1+2
2. Run bonsai in the lab, inject the fault the playbook addresses
3. Manually invoke the gNMI Set via `gnmic` CLI (or a small bonsai CLI you can add) and verify it produces the expected state change
4. Observe the graph: does the fault actually recover?
5. If yes, mark the playbook `verified: true` and tag with the firmware version it was tested on
6. If no, iterate — usually a path is slightly wrong or needs a companion setting

**This validation step produces the `verified_on` metadata** that makes the playbook trustworthy:

```yaml
playbooks:
  - name: srl_bgp_admin_state_bounce
    vendor: nokia_srl
    verified_on:
      - firmware: "24.7.2"
        last_tested: "2026-04-22"
        tested_by: "arjun"
    confidence: high
    ...
```

---

## The LLM-Assisted Extraction Pipeline

Given the three sources, here is a concrete pipeline you can actually execute. Call this a *harvest session*, one run per target operation.

### Session structure (one operation, all vendors, one sitting)

Pick one operation. Let's say "BGP neighbor admin-state bounce." Then:

**Step 1** — YANG introspection. Grep the four YANG tree dumps for `admin-state` or `enabled` under `bgp/neighbor`. Record candidate paths for each vendor.

**Step 2** — Doc extraction. For each vendor, find one authoritative doc page describing how to toggle a BGP neighbor via their management interface. Feed each doc page to the LLM with a *specific prompt* (see the separate `PHASE5_HARVEST_PROMPT.md` file) asking for structured playbook YAML. Collect four YAML drafts.

**Step 3** — Synthesis. Ask the LLM to merge the four drafts into a single playbook YAML with four `playbooks:` entries, one per vendor, validating that preconditions and verification queries are consistent across them. Use the YANG paths from Step 1 as ground truth; flag mismatches between doc and YANG.

**Step 4** — Lab validation. For whichever vendors you have running in ContainerLab, execute the gNMI Set manually via `gnmic` to verify the path works. Add `verified_on` metadata.

**Step 5** — Commit. The YAML lands in `playbooks/library/`. An ADR entry in DECISIONS.md captures which operations are now covered.

**Throughput**: one operation per session. Ten sessions gets you to roughly 40 playbook entries (operations × vendors). That's a meaningful bootstrap library.

### Which operations to harvest first

Priority is driven by what your *existing detection rules* need. Your Phase 4 rules produce these detection types:

1. `bgp_session_down` → needs BGP reset per vendor
2. `bgp_session_flap` → no remediation typically (log only), still catalog as "no-op playbook"
3. `interface_down` → needs interface admin-state toggle per vendor
4. `interface_error_spike` → needs interface counter reset per vendor (and possibly flap)
5. `topology_edge_lost` → typically no remediation (investigation needed)
6. `bgp_all_peers_down` → explicit "no-op, human required" playbook

Harvest these six first. That's your minimum viable catalog — every Phase 4 detection has at least a no-op or a real playbook. Then expand into operations that *would* be useful detections but aren't yet rules (OSPF, MPLS, SR).

### What NOT to try to automate

- **Don't try to infer playbook ordering** — which one to try first when multiple apply. Human judgment ranks these based on risk and reversibility. Encode it in YAML order with a `risk_tier` field (`safe` / `disruptive` / `last_resort`).

- **Don't try to have the LLM write verification queries from scratch.** These require knowing the graph schema. Provide the schema in the prompt (from `PHASE5_HARVEST_PROMPT.md`) and ask for verification queries as the final step, with a human review of each.

- **Don't promise Juniper/XRd coverage you haven't tested.** Mark entries `verified_on: []` and `confidence: unverified` where appropriate. An empty verified list is honest. Claiming tested-when-untested is how playbooks silently break in production.

---

## A Progressively-Better Feedback Loop

The library is not static. Every time a playbook runs in the field, it produces outcome data. Build the loop from day one:

1. **Record `Remediation.status` honestly** — success, failure, skipped. This is already in the schema.
2. **Add `firmware_version` to the Device node** (it comes from the Capabilities response). You'll want this for per-firmware playbook routing later.
3. **Track playbook version** — add a `playbook_version` field on Remediation. When a playbook is edited, bump its version. This lets you answer "did the new version of the playbook work better?"
4. **Periodic audit** — once a month, query `MATCH (r:Remediation) WHERE r.status = 'failed' RETURN r.action, count(*)` and look at what's failing. Failures concentrate — one bad playbook can produce most failures. Fixing it lifts the whole system.

This is the Model C training signal. With enough data, the ML selector learns which playbook variant works best for which feature pattern. But even before ML, the aggregate failure data tells *humans* which playbooks to rewrite.

---

## Bootstrap Completeness Target

A sensible goal for the bootstrap library:

- **6 of 6** detections from Phase 4 rules have at least one playbook entry (some may be "no-op, alert only")
- **3 of 4** primary vendors have at least one entry for each remediable detection
- **10+ total playbook entries** covering the most common operational scenarios
- **Each entry is validated on at least one firmware version in the lab**

That's the MVP catalog. Everything else is growth.

From that base, Layer 2 (ML selection) has enough variants to pick between, and Layer 3 (LLM suggestion) has enough few-shot examples to propose new entries for unseen vendors.

---

## What This Means for Phase 5 Sequencing

The main review document listed 8 pre-Phase-5 actions. This adds context around action #8 ("Convert existing hardcoded remediation paths into the first YAML playbook"). That action is not just a schema move — it's the first entry in a library that needs to grow to 10–15 entries before ML selection is meaningful.

Sequencing within Phase 5.0:

1. Scaffold `playbooks/` catalog module (action #8 from main review)
2. Move existing SRL BGP admin-state into YAML
3. Run one harvest session per Phase 4 detection type (6 sessions, ~1 week of part-time work)
4. Validate each in the lab with Nokia SRL + Cisco XRd
5. *Now* start ML training work — the catalog has enough entries to be worth learning over

This pushes the "start ML training" milestone out by roughly 1–2 weeks from what the main document implied. That's the right call. ML on a 1-playbook catalog learns nothing; ML on a 10-playbook catalog learns useful things.

---

## The Honest Constraint

I want to be direct about one thing.

Building a comprehensive global playbook library that covers every operation for every vendor at every firmware version is genuinely a *years-long* effort. Commercial products have teams of engineers doing this full time. You will not match that.

What you *will* build is a functional catalog of 20–40 entries that covers the common operations you care about in your lab, with a clean structure that *allows* growth. The LLM suggestion layer (Layer 3 from the addendum) is how the catalog keeps growing past what you personally write — but it still requires human approval for every new entry.

This is fine. The project's goal is not to ship a commercial replacement. It's to prove the architecture and understand the systems. A 30-entry catalog is plenty to demo the closed loop and to train the ML selector with meaningful signal.

Set the bar there. Don't try to build an industry-wide library with one person and an LLM. Build a catalog you trust, that covers your lab, that extends cleanly.

---

*Version 1.0 — written alongside the remediation addendum. Read the companion document `PHASE5_HARVEST_PROMPT.md` for the concrete LLM prompt to kick off the first harvest session.*
