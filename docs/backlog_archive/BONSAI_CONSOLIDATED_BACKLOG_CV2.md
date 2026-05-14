# BONSAI — Backlog Charlie Series, v2 (CV2.0)

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_CV1.md`. Authored 2026-05-09 after sprint-by-sprint code review of v22 (post-CV1 main).
>
> **What CV1 produced**: an enormous and architecturally-correct sprint of code. ~6,400 new lines across `change_detection.rs`, `config_store.rs`, `parser_chain.rs`, `synthesizer/`, `yang.rs`, `enrichment/multi_source.rs`, `integrations/servicenow_aiops.rs`. Eight starter synthesizer rules. Four vendor syslog pattern files. Two parser sidecar container definitions. CLI subcommands and HTTP APIs for YANG library and synthesizer.
>
> **What CV1 did NOT produce**: end-to-end verification. Two specific findings drive CV2's direction:
>
> 1. **Dead code at runtime**. The `MultiSourceEnricher` trait exists but has only one implementation (`GnmiGetConfigEnricher`); change_detection hardcodes that single implementation rather than going through a registry. The `ParserChain` is a complete implementation with sidecar orchestration and unit tests, but **zero runtime callsites** — nothing in the runtime ever calls it. The architecture is correct in shape; the wiring is incomplete in fact.
> 2. **Untested integration surface**. Splunk, Elastic, ServiceNow EM, ServiceNow AIOps, YANG sync, synthesizer-against-real-discovery — all have code that compiles, none have e2e validation reports. The Bv5/Bv6 testing discipline (driver results, daily checks) regressed during CV1 sprint.
>
> **What CV2 is**:
>
> 1. A sprint-wise audit recording what was done right, what was done wrong, what's specifically dead-code, what needs e2e validation
> 2. A serious commitment to **lightweight test scripts that don't burn tokens** — bash/Python smoke tests, pre-cargo-build wiring checks, integration probes that report structured pass/fail
> 3. The **modern streaming protocols** the operator named: BMP for BGP route-monitoring streaming, BGP-LS for IGP+TE topology, PCEP for SR-PCE state. These fit the streaming-first architecture as native data sources, not as enrichers
> 4. **Syslog as a streaming source** with a value-extraction layer — structured fact extraction, cross-source joins to gNMI graph state, fact-driven detection rules
> 5. A **clean-state reset plan** for both laptop and cloud lab runs, since so much new code has landed that stale archive risks contamination
>
> **What carries forward unchanged**: the discovery-driven layered ingestion model from CV1. The pivot is correct; what needs work is making CV1's pivot operationally real, not redrawing it.

---

## Table of Contents

1. [Audience and Positioning](#positioning)
2. [Sprint-by-Sprint Audit of CV1 Landings](#audit)
3. [Specific Findings: A-1 through A-9](#findings)
4. [Lightweight Test Discipline (the token-burn problem)](#testing)
5. [Modern Streaming Protocols — BMP, BGP-LS, PCEP](#streaming)
6. [Syslog as a Streaming Source](#syslog)
7. [Clean-State Reset Plan](#reset)
8. [TIER 1 — Wiring Audit + Dead Code Activation](#tier-1)
9. [TIER 2 — Lightweight Test Discipline](#tier-2)
10. [TIER 3 — End-to-End Validation Backlog](#tier-3)
11. [TIER 4 — Modern Streaming Protocol Tier](#tier-4) ⚡ THE NEW WORK ⚡
12. [TIER 5 — Syslog Value Extraction + Cross-Source Joins](#tier-5) ⚡ THE NEW WORK ⚡
13. [TIER 6 — Carryover from CV1](#tier-6)
14. [Execution Order](#execution-order)
15. [Guardrails — Updated](#guardrails)

---

## <a id="positioning"></a>Audience and Positioning

**Sharpened for streaming-first reality**:

> Bonsai is a graph-native streaming engine for network state. It consumes telemetry from **multiple streaming sources** — gNMI Subscribe for state, BMP for BGP route-quality, BGP-LS for IGP+TE topology, PCEP for SR-PCE/RSVP-TE LSP state, syslog for event signal — and binds them through a graph-native correlation layer. Discovery-driven layered ingestion (CV1) determines per-device what's achievable; CLI parsing fills gaps where streaming isn't available. The output is impact-aware incidents fed to AIOps platforms, with a GNN trained on the multi-source archive providing anomaly scores.

The single phrase that changed: **"multiple streaming sources"**. CV1 had streaming as one layer with everything else as fallback. CV2 acknowledges that BMP, BGP-LS, PCEP, and syslog are all streaming protocols in their own right, not enrichers. This sharpens the architectural claim and broadens the data surface.

---

## <a id="audit"></a>Sprint-by-Sprint Audit of CV1 Landings

CV1 had 8 sprints planned. Reality landed differently: most sprints' code shipped concurrently, but verification did not. Audit by intended sprint scope:

### CV1 Sprint 1 — Hygiene + lab metadata

| Item | Status | Comment |
|---|---|---|
| T1-1 verify leaf1 EVPN routes | ❓ Unverified | No new daily check after 2026-05-08 in repo. Bv6 T1-3 status was uncertain at CV1 start; still uncertain. **Run `scripts/check_lab.sh dc` and capture output.** |
| T1-2 GNN data loader role expansion | ✅ Done | `python/bonsai_ml/gnn/data_loader.py` extended; verified by reading the new feature names |
| T1-3 lab metadata for synthesizer | ✅ Done | Lab YAMLs now declare `role` and `environment`; `docs/lab_metadata.md` (139 lines) describes the convention |
| T1-4 cloud GNN training boundary | ✅ Done | `docs/cloud_lab.md` (60+ lines) explains archive-only-no-training |

**Sprint 1 verdict**: substantially done. Outstanding: T1-1 verification.

### CV1 Sprint 2 — Multi-source enrichment + layered ingestion

| Item | Status | Comment |
|---|---|---|
| T2-1 MultiSourceEnricher trait + scaffolding | ⚠️ **PARTIAL** — DEAD CODE | Trait defined (`enrichment/multi_source.rs:24`); `GnmiGetConfigEnricher` is the only impl. `change_detection.rs:284` hardcodes `GnmiGetConfigEnricher` — does not go through the trait registry. **No multi-source pluralism at runtime.** See A-1 below. |
| T2-2 local guarded config store | ✅ Done | `src/config_store.rs` (149 lines) — encrypted-at-rest, hash equality, diff representation |
| T2-3 change detection pipeline (3 triggers) | ✅ Done | `src/change_detection.rs` (851 lines) wires all three triggers; main.rs:738 starts the runtime |
| T2-4 multi-source parser with priority chain | ⚠️ **CODE-ONLY — DEAD CODE** | `src/parser_chain.rs` (293 lines) is a complete implementation. **Zero runtime callsites.** `grep -r "ParserChain::" src/` returns only file-internal references. See A-2 below. |
| T2-5 parser sidecar reference implementations | ⚠️ Unverified | `docker/sidecars/{bonsai-native-parser,pyats}/` exist; sidecar containers themselves not exercised; no e2e test |
| T2-6 provenance fields throughout the graph | ✅ Done (per CV1 schema) | Schema migration appears in graph/mod.rs; needs verification via Cypher query |
| T2-7 gNMI Readiness Report | ✅ Done | `discovery.rs` substantially extended; new `GnmiReadinessReport` struct; new HTTP endpoint |

**Sprint 2 verdict**: code landed, two pieces are architecturally dead. Substantial work to wire up.

### CV1 Sprint 3 — Path Relevance Synthesizer

| Item | Status | Comment |
|---|---|---|
| T3-1 synthesizer engine | ✅ Done | `src/synthesizer/mod.rs` (747 lines) — rule schema, recommendations with confidence, blockers, gaps |
| T3-2 starter rule library | ✅ Done | 8 starter rules: `dc_leaf, dc_spine, sp_p, sp_pe, sp_rr, campus_access, campus_core, campus_distribution` |
| T3-3 synthesizer recommendations UI | ⚠️ Partial | DeviceDrawer modified; needs verification that recommendations actually appear with rationale |
| T3-4 operator override library | ✅ Done | Override application logic in `synthesizer/mod.rs`; merge precedence implemented |

**Sprint 3 verdict**: code complete; e2e against real discovery not validated.

### CV1 Sprint 4 — YANG library lifecycle

| Item | Status | Comment |
|---|---|---|
| T4-1 online sync `bonsai yang-sync` | ⚠️ Unverified | `src/yang.rs` (862 lines); CLI subcommand wired; never run against real openconfig/public |
| T4-2 manual upload `bonsai yang-import` | ⚠️ Unverified | Same code path; never tested with real YANG bundle |
| T4-3 offline / restricted workflow | ⚠️ Unverified | Bundle/install mode coded; never tested two-machine |
| T4-4 synthesizer YANG awareness | ✅ Done | `synthesizer/mod.rs` calls `yang::evaluate_profile_requirements` |

**Sprint 4 verdict**: code substantial; **never run against real YANG repos**. Critical gap for operator-facing claim.

### CV1 Sprint 5 — Output adapter validation

| Item | Status | Comment |
|---|---|---|
| T5-1 Splunk HEC e2e test | ❌ NOT DONE | `scripts/sprint5_preflight.sh` exists; no `e2e_output_adapters/<date>-splunk-pass.md` written. |
| T5-2 Elastic e2e test | ❌ NOT DONE | Same — no test result artefact |
| T5-3 ServiceNow EM e2e test against PDI | ❌ NOT DONE | Same — `scripts/e2e_servicenow_pdi_test.sh` exists; no test results |
| T5-4 output adapter health monitoring | ⚠️ Partial | Code exists in adapter traits; no test |

**Sprint 5 verdict**: **the entire validation tier did not happen.** This is the gap the user named in their concern about Splunk/Elastic/EM not being tested.

### CV1 Sprint 6 — ServiceNow AIOps integration

| Item | Status | Comment |
|---|---|---|
| T6-1 bidirectional incident sync | ✅ Code done | `integrations/servicenow_aiops.rs` (989 lines); wired in main.rs:971 |
| T6-2 auto-correlation feeds ServiceNow | ✅ Code done | Same module |
| T6-3 auto-clearing | ✅ Code done | Resolve flow present |
| T6-4 root-cause hint via blast radius | ✅ Code done | `IncidentCandidate.root_cause_hint` field |
| T6-5 ServiceNow ITSM playbook bridge | ⚠️ Partial | Playbook proposal flow exists; never exercised |
| **e2e test against PDI** | ❌ NOT DONE | No `e2e_servicenow_aiops/<date>-pass.md` artefact |

**Sprint 6 verdict**: code complete; **never exercised against the operator's PDI**. Bidirectional flow unproven.

### CV1 Sprint 7 — GNN training (parallel)

Per Bv5 trigger condition (30+ days archive). **Trigger not yet met.** Archive accumulation continues passively. No work expected here yet.

### CV1 Sprint 8 — Documentation updates

| Item | Status | Comment |
|---|---|---|
| T8-1 DECISIONS.md | ✅ Done | Substantially expanded |
| T8-2 README.md | ✅ Done | Updated for CV1 model |
| T8-3 CLAUDE.md / AGENTS.md | ⚠️ Likely partial | Needs verification |
| T8-4 layered_ingestion architecture note | ⚠️ Unverified | Need to check `docs/architecture_layered_ingestion.md` exists |
| T8-5 deployment guide | ⚠️ Unverified | Need to check `docs/deployment_guide.md` exists |

**Sprint 8 verdict**: docs partly done. Lower priority per standing instruction.

### Summary

**Done well**: Sprint 1 hygiene; Sprint 2 change detection + config store + readiness report; Sprint 3 synthesizer + starter rules; Sprint 6 ServiceNow AIOps code; Sprint 8 DECISIONS+README.

**Done with critical gaps** (the dead-code surface): Sprint 2 multi-source pluralism; Sprint 2 parser chain.

**Not done at all**: Sprint 5 output adapter validation; Sprint 6 PDI e2e validation; Sprint 4 YANG sync against real repos.

---

## <a id="findings"></a>Specific Findings: A-1 through A-9

### A-1 — `MultiSourceEnricher` is a single-source enricher with extra typing

**Severity**: HIGH

**Location**: `src/enrichment/multi_source.rs`, `src/change_detection.rs:284`

**Evidence**: The trait `MultiSourceEnricher` is defined with `name()` and `capture()` methods. One implementation exists (`GnmiGetConfigEnricher`). `change_detection.rs::run_capture` calls `GnmiGetConfigEnricher` directly by name, not through any trait registry or polymorphic dispatch. There is no enricher registry, no per-vendor strategy selection, no fallback chain.

**Why it matters**: CV1 specified "multi-source enricher with priority chain" — the value proposition was that change detection picks the right capture method for each device (gNMI Get for OpenConfig devices, CLI for IOS classic, REST API for vendor-specific gear). What landed is the trait shape but the runtime always picks the same single implementation.

**Fix**: small refactor — ~30 lines in change_detection.rs. Build a registry at startup from configured enrichers; route capture requests by `(vendor, capability)` to the appropriate enricher. The `ParserChain` (A-2) integrates here as the CLI-based capture path.

**Effort**: 1-2 days.

### A-2 — `ParserChain` is fully implemented dead code

**Severity**: HIGH

**Location**: `src/parser_chain.rs` (293 lines including tests), `src/config.rs:770`

**Evidence**: `ParserChain` struct, `parse()` method, sidecar HTTP client, priority-chain logic with consensus mode, unit tests. Configuration struct (`ParserChainConfig`) with sidecars list, priority overrides, consensus mode toggle. **Zero callers in the runtime.** `grep -rn "ParserChain::" src/ --include="*.rs"` returns only definitional and test references.

**Why it matters**: CV1 promised multi-vendor CLI parsing through a sidecar architecture (pyATS, ntc-templates, SuzieQ, native). The whole point was to fill in for vendors that gNMI doesn't cover well. None of that is reachable at runtime today.

**Fix**: wire `ParserChain` into `MultiSourceEnricher` registry as one capture strategy (after fixing A-1). When a device's `GnmiReadinessReport` shows blockers, the enricher registry routes to the parser-chain-backed CLI capture path. This is the natural integration point.

**Effort**: 2-3 days plus sidecar deployment validation.

### A-3 — Output adapter end-to-end validation never happened

**Severity**: MEDIUM (operationally significant; not a code defect)

**Location**: `docs/test_results/e2e_output_adapters/`

**Evidence**: only `20260503-prometheus-fail.md` and `20260503-prometheus-pass.md`. **No Splunk, Elastic, ServiceNow EM test result documents.** Three weeks of code shipped without exercising the receiver-facing surface.

**Why it matters**: an output adapter that compiles but has never written a real event to a real receiver is not validated. Encoding bugs, schema mismatches, auth flow issues, retry semantics — all surface only on first real run.

**Fix**: Tier 3 below — three e2e validation runs, each producing a structured artefact in `docs/test_results/e2e_output_adapters/`. Lightweight scripts (Tier 2 framework).

### A-4 — ServiceNow AIOps bidirectional sync never exercised against PDI

**Severity**: MEDIUM

**Location**: `src/integrations/servicenow_aiops.rs` (989 lines), wired in main.rs:971

**Evidence**: substantial code, no `docs/test_results/e2e_servicenow_aiops/` directory. The bidirectional flow (bonsai opens incident → ServiceNow notifies on update → bonsai reflects state) has never been observed against a real PDI.

**Why it matters**: bidirectional integrations are notoriously brittle. Webhook auth, state-machine race conditions, incident-update field mapping — every one of these surfaces on first real run.

**Fix**: Tier 3 — single PDI e2e test exercising the full bidirectional cycle.

### A-5 — YANG sync never validated against real OpenConfig repo

**Severity**: MEDIUM

**Location**: `src/yang.rs` (862 lines), CLI subcommand wired in main.rs:1922

**Evidence**: `bonsai yang-sync` and `bonsai yang-import` exist as CLI commands. No test artefacts demonstrating actual fetch from `github.com/openconfig/public`, validation, indexing.

**Why it matters**: real OpenConfig repos contain YANG modules with quirks (deviations, augmentations, vendor-specific extensions). Validation logic that handles synthetic test fixtures often breaks on real modules.

**Fix**: Tier 3 — single yang-sync run against the real openconfig/public repo, captured outcome.

### A-6 — Synthesizer recommendations never exercised against real lab discovery

**Severity**: MEDIUM

**Location**: `src/synthesizer/mod.rs`

**Evidence**: 8 starter rules; engine produces recommendations. No test artefact showing recommendations being generated for the running 8-node DC lab and operator approving/overriding.

**Why it matters**: the synthesizer's value claim is that it picks paths *better than the static catalogue* given enrichment. That claim is unverified.

**Fix**: Tier 3 — exercise synthesizer against running DC lab, capture per-device recommendation report, compare against current static path-catalogue selection. Hand-validate.

### A-7 — Daily check operational regression

**Severity**: MEDIUM (operational discipline gap)

**Location**: `docs/test_results/daily_runs/`

**Evidence**: last daily check is `2026-05-08.md`. No 05-09 or later. Bv6 T1-1 chaos auto-restart logic exists; daily reports stopped.

**Why it matters**: the Bv5/Bv6 operational discipline (continuous chaos, daily verification) is what produces the GNN training archive. A regression in the daily check loop means archive quality degrades silently.

**Fix**: re-establish the daily check cron; verify chaos runner is up; reset baseline if needed (Tier 7 reset plan).

### A-8 — Sidecar containers built but never deployed

**Severity**: LOW (no operational impact yet; will become HIGH when A-2 is fixed)

**Location**: `docker/sidecars/bonsai-native-parser/`, `docker/sidecars/pyats/`

**Evidence**: directories exist with Dockerfile and supporting code. No compose profile actually runs them. No test verifying they respond to `/parse` HTTP requests.

**Why it matters**: when A-2 wiring lands, the sidecars must actually be deployable. If they have build errors or wrong interfaces, A-2 wiring will fail at first runtime.

**Fix**: Tier 2 — smoke test that builds and runs the sidecars, verifies HTTP `/parse` responds.

### A-9 — `change_detection.rs` syslog pattern matching is regex-only

**Severity**: LOW (works for common patterns; fragile for complex cases)

**Location**: `src/change_detection.rs:159-200`

**Evidence**: `SyslogPatternFile` reads `patterns: Vec<String>` and matches them as regex. No structured fact extraction (which user, which interface, which command). Just "this message matches a known config-changed pattern → trigger re-parse."

**Why it matters**: this is fine for change-trigger purposes but **misses the bigger value the operator is asking about**: extracting structured facts from syslog (user, interface, severity, error code) and joining them with graph state. The current code does pattern matching for one purpose; needs to evolve into a structured-extraction pipeline (Tier 5).

**Fix**: Tier 5 — extend pattern files to include capture groups + named fields; emit `SyslogFact` events with structured data; cross-source join layer.

---

## <a id="testing"></a>Lightweight Test Discipline (the token-burn problem)

The user's concern: "many of the items that has landed hasn't been tested end to end... we need to spend some time thinking how to effectively test without wasting a lot of tokens... more efforts should be in creating scripts that check and report functionalities so that we spend less time in saying lets fix this, wait for a lot of time for cargo build."

This is correct and is the **single most actionable improvement** to working discipline. Concrete framework:

### The token economy of the current loop

A typical "fix-then-verify" cycle today:
1. Make a code change
2. `cargo build --release` (3-7 minutes wall clock; LLM session waits, accumulates context)
3. Run a test, fail, see error
4. Make another change
5. `cargo build` again

Each iteration burns ~5K-15K tokens of context just on build outputs and waiting. Over 10 iterations, ~100K tokens burned on build wait, not progress.

### The new discipline — three layers

**Layer 1 — Wiring checks (no build, instant)**:
Bash scripts that grep the source tree to verify integration. These catch dead-code-surface issues like A-1 and A-2 *before* a build ever runs.

```bash
# scripts/check_wiring.sh
#!/usr/bin/env bash
set -e
echo "Wiring checks (zero compile cost)..."

# Every trait implementation must be referenced outside its file
for trait_name in MultiSourceEnricher BusSubscriber Detector Enricher OutputAdapter; do
  defined_in=$(grep -rln "impl ${trait_name} for" src/ | head)
  used_outside=$(grep -rln "${trait_name}\b" src/ --include="*.rs" | \
                 grep -v -F "$defined_in" | head)
  if [ -z "$used_outside" ]; then
    echo "FAIL: ${trait_name} has no consumer outside its definition"
    exit 1
  fi
done

# Every public struct in module N must have a callsite outside module N
# (catches the ParserChain dead code pattern)
# ... etc
```

This catches A-1, A-2, A-8 in ~5 seconds instead of via end-to-end test. **Run before every PR.**

**Layer 2 — Pre-build smoke tests (Python, ~30 seconds)**:
Python scripts that exercise the runtime through HTTP API without restart. Bonsai stays running; smoke test sends crafted requests and verifies responses.

```python
# scripts/smoke_synthesizer.py
import requests
import sys

def main():
    r = requests.get("http://localhost:3000/api/devices")
    devices = r.json()["devices"]
    if not devices:
        sys.exit("FAIL: no devices found in registry")
    
    target = devices[0]["address"]
    r = requests.get(f"http://localhost:3000/api/devices/{target}/recommendations")
    recs = r.json()
    
    if recs.get("status") != "ok":
        sys.exit(f"FAIL: synthesizer returned {recs.get('status')}")
    if not recs.get("recommended_paths"):
        sys.exit("FAIL: synthesizer returned zero recommendations")
    
    print(f"PASS: {len(recs['recommended_paths'])} paths recommended for {target}")
    print(f"  matched_rules: {recs.get('matched_rules')}")
    print(f"  blockers: {len(recs.get('blockers', []))}")

if __name__ == "__main__":
    main()
```

Run after the runtime is up. No rebuild needed. ~30 seconds. Tells AI session what works without burning a build cycle.

**Layer 3 — Periodic e2e (longer, scheduled)**:
Existing `scripts/e2e_*_test.sh` pattern, run nightly via cron. Always producing artefact in `docs/test_results/`. AI session reads artefact, doesn't re-run.

### The discipline rule

**No PR lands without**:
- Layer 1 wiring check passes
- Layer 2 smoke test exists for any new HTTP endpoint or new bus subscriber
- Tag the artefact path in PR description so AI sessions can find it

This isn't a tooling change; it's a working-pattern change. CV2 Tier 2 codifies it.

---

## <a id="streaming"></a>Modern Streaming Protocols — BMP, BGP-LS, PCEP

The operator named these specifically. Let me work through what each gives us, where they fit in the architecture, and the implementation shape.

### Why these matter

gNMI Subscribe gives us OpenConfig state — interfaces, BGP neighbor state, IS-IS adjacency state, BFD sessions. Excellent for *device-local* state.

What's missing:
- **BGP route-quality information** — per-prefix attributes, AS_PATH, communities, peer-by-peer view of how the routing table is actually being constructed. gNMI exposes some of this via `/network-instances/.../bgp-rib/` but vendor coverage is uneven and the data volume is heavy.
- **IGP topology with TE attributes** — link metrics, SR-SIDs, SRLGs, link bandwidth, admin groups. Each device's gNMI exposes its own view; reconstructing the global view requires joining across devices.
- **Traffic engineering state** — RSVP-TE LSPs, SR policies, computed paths. Exposed via PCEP from controllers/PCEs; gNMI coverage is partial and vendor-specific.

Each of these has a **purpose-built streaming protocol** that was designed for exactly this data. They're not replacements for gNMI; they're complements.

### BMP — BGP Monitoring Protocol (RFC 7854)

**What it is**: BGP routers stream their BGP state to a monitoring station. Per-peer Adj-RIB-In, Adj-RIB-Out, Loc-RIB. Route updates as they happen, not on poll.

**Why it's valuable for bonsai**:
- Per-prefix change visibility — see route-flap, route-leaks, hijack patterns as they happen
- Pre-policy and post-policy views (with the right BMP version) — see *why* a route was rejected
- Multi-vendor: Cisco, Juniper, Arista, Nokia, FRR all support BMP
- Streaming, not polling — fits architecture perfectly

**What it doesn't replace**: gNMI BGP-neighbor-state. gNMI tells us "session up" or "session down." BMP tells us "session up but advertising wrong prefixes." Complementary.

**Implementation effort**: substantial. BMP is a TCP-based protocol with its own message format (BGP-encoded headers). Two viable approaches:

1. **Embed parser** — Rust BMP parser in `src/streaming/bmp.rs`. Reasonable: ~600 lines for receiver + parser. Existing crate `bmp-parser` is BSD-licensed and reasonably mature.
2. **Use external BMP server** (OpenBMP, GoBMP) and consume via Kafka/syslog/REST. Less code; more deployment complexity.

**Recommendation**: embed parser. Bonsai is already a streaming receiver; adding another protocol is in-character.

**Where in graph**: new node types `BmpSession` (per BMP-speaking router), `BgpRibEntry` (per-prefix-per-peer). Edges: `BgpRibEntry-[FROM_PEER]->BgpNeighbor`, `BgpRibEntry-[ADVERTISED_BY]->Device`. Detection rules: `RouteLeakDetected` (prefix advertised that shouldn't be), `RouteFlap` (per-prefix flap rate exceeds threshold), `UnexpectedAsPath` (AS_PATH contains AS that shouldn't be there).

### BGP-LS — BGP Link-State Distribution (RFC 7752)

**What it is**: BGP carries IGP topology information. A single device (typically a route-reflector or dedicated speaker) can stream the entire IS-IS or OSPF topology — including TE attributes, SR-SIDs, SRLGs — over a normal BGP session.

**Why it's valuable for bonsai**:
- One streaming source gives you the entire IGP topology, no per-device gNMI gather needed
- TE attributes (admin groups, link colors, SRLGs, BW) come for free
- SR (segment routing) topology — Adj-SIDs, Node-SIDs, Prefix-SIDs — visible without per-device gNMI
- Standard, multi-vendor (Cisco, Juniper, Nokia, Arista all speak it)

**What it doesn't replace**: per-device IS-IS adjacency state via gNMI (which tells us if a specific device's neighbor is up). BGP-LS tells us global topology; gNMI tells us local adjacency state. Both useful.

**Implementation effort**: smaller than BMP. BGP-LS is encoded as a BGP NLRI, so the receiver is a normal BGP speaker that subscribes to the AFI/SAFI 16388/71. Use `gobgp` as a sidecar (popular, well-maintained, MIT-licensed) and consume the JSON stream it emits.

**Recommendation**: gobgp sidecar pattern. Avoids a Rust BGP implementation (which is months of work).

**Where in graph**: enrich existing `Device`, `Interface`, `IsisAdjacency` nodes with BGP-LS-derived attributes (TE-metric, admin-group, SID). New node `SrPolicy` for explicit SR policies discovered via BGP-LS. Detection rules: `SrPolicyDegraded` (best-path down), `SrlgRiskDetected` (multiple paths share an SRLG).

### PCEP — Path Computation Element Protocol (RFC 5440 + extensions)

**What it is**: TCP protocol between PCC (Path Computation Client, the router) and PCE (Path Computation Element, often a controller). Stateful PCEP (RFC 8231) means PCC reports each LSP's state to the PCE; that state is also visible to anyone who taps the PCEP session.

**Why it's valuable for bonsai**:
- Real-time visibility into TE LSPs: setup, teardown, re-optimization, path changes
- For SR-PCE-driven networks (which are increasingly common in SP), PCEP is THE way to see policy state
- Cisco SR-PCE, Juniper Northstar, open-source PathFinder all speak PCEP

**What it doesn't replace**: gNMI MPLS-LSP state per device. gNMI tells us per-device LSP state; PCEP tells us controller-mediated policy state. SR-PCE deployments need both.

**Implementation effort**: medium. PCEP parser is moderate complexity (~400-600 lines). Or use `pathfinderd`-like sidecar.

**Recommendation**: defer until SP lab is up. PCEP without an SR-PCE-controlled SP topology is academic.

**Where in graph**: new node types `PathComputationElement`, `LspTunnel`. Detection rules: `LspReoptimizationStorm` (excessive PCRpt rate), `LspPathChange` (LSP took a different physical path).

### How they fit the architecture

All three are **streaming receivers**. They publish to the existing `InProcessBus` as new `TelemetryUpdate` variants (or new event types entirely). They participate in the write coordinator. They flow into the same archive. **They're not enrichers; they're additional Layer 1 sources.**

The discovery-driven layered ingestion model (CV1) extends naturally: per-device, the gNMI Readiness Report becomes a multi-protocol readiness report. "Device speaks BMP yes/no, BGP-LS yes/no, gNMI streaming yes/no, PCEP-PCC yes/no." Synthesizer recommends per-protocol subscriptions.

### Sequencing recommendation

**Bv6/CV1 had no streaming-protocol tier**. CV2 introduces it as Tier 4. Within Tier 4:
- T4-1 BMP receiver — first because BMP is the most universally supported and gives the broadest immediate value
- T4-2 BGP-LS via gobgp sidecar — second because it's smaller code change but requires deploy of the sidecar
- T4-3 PCEP — defer to after SP lab is up

This is meaningfully scoped work. ~3-4 weeks for BMP + BGP-LS done well.

---

## <a id="syslog"></a>Syslog as a Streaming Source

The operator's third point: "syslog can also be thought as a streaming data source... break it down extract useful information and actively link it with the streaming info we are getting from gnmi etc."

This is correct and bonsai already has the infrastructure to do it. What's missing is the **fact-extraction layer** that turns a syslog message from "an event matched a pattern" into "we know X happened to Y at time Z."

### Where bonsai is today

- `src/signals/syslog.rs` — daemon receives syslog, classifies into Auth/Hardware/Software/Protocol/License/Custom, archives, publishes to bus
- `src/change_detection.rs` — uses syslog patterns to trigger config re-parse
- `python/bonsai_sdk/rules/syslog.py` — 5 rules that match patterns and fire detections

This is good but flat: a syslog message becomes either a detection or a re-parse trigger or both. The graph doesn't gain structured facts from the message.

### What's missing — structured fact extraction

A syslog like:
```
%BGP-5-ADJCHANGE: neighbor 10.0.0.1 Up
```

Today produces: a `SyslogEvent` with raw message, classification "Protocol", and possibly a `BgpAdjacencyUp` detection if a rule matches.

What we should also produce: a `SyslogFact` event with structured fields:
- `device`: derived from sender IP via target_map
- `protocol`: BGP
- `peer`: 10.0.0.1
- `state`: Up
- `severity`: 5 (notice)

This `SyslogFact` joins to the graph: `Device-[OBSERVED]->SyslogFact-[REGARDS_PEER]->BgpNeighbor`. Now graph queries can ask "show me all syslog observations about this BGP peer in the last hour" and join across syslog and gNMI worlds.

### Architecture — syslog enrichment pipeline

**Stage 1 — Receive (existing)**: UDP/TCP receiver, structured `SyslogEvent`, archive, bus.

**Stage 2 — Classify (existing)**: assign Category (Auth/Hardware/Software/Protocol/License/Custom).

**Stage 3 — Extract facts (NEW)**: per-vendor pattern library with capture groups and named fields. Pattern matches → produce `SyslogFact` with structured data.

```yaml
# config/syslog_patterns/cisco-iosxr.yaml
patterns:
  - id: bgp_adjacency_change
    regex: '^%BGP-(?P<severity>[0-9])-ADJCHANGE: neighbor (?P<peer_ip>[0-9a-fA-F:.]+) (?P<state>Up|Down)'
    facts:
      protocol: bgp
      peer_ip: "${peer_ip}"
      state: "${state}"
      severity_numeric: "${severity}"
    correlation_keys: [device, peer_ip]
  
  - id: interface_state_change
    regex: '^%LINK-3-UPDOWN: Interface (?P<interface>\S+), changed state to (?P<state>up|down)'
    facts:
      interface: "${interface}"
      state: "${state}"
    correlation_keys: [device, interface]
```

**Stage 4 — Cross-source join (NEW)**: when a `SyslogFact` arrives, attempt join with current graph state. Example:

```
SyslogFact arrives: { device: leaf1, protocol: bgp, peer_ip: 10.0.0.1, state: Down }
Graph query: MATCH (d:Device {hostname: 'leaf1'})-[:HAS_BGP_NEIGHBOR]->(n:BgpNeighbor {peer_address: '10.0.0.1'})
            RETURN n
If found: emit JoinedFact with both syslog source and gNMI graph context
If not found: emit OrphanFact (interesting — syslog says X but graph doesn't know X)
```

**Stage 5 — Detection rules (NEW)**: rules over `JoinedFact` and `OrphanFact`. Examples:
- `SyslogGnmiDisagreement`: syslog says BGP peer down, gNMI says peer up — possibly a syslog stuck or gNMI stale
- `OrphanInterfaceMention`: syslog references interface that's not in graph — possibly indicates discovery gap
- `MultiSourceCorrelation`: syslog + gNMI both report BGP peer down within window — high-confidence detection

This is genuinely **multi-source streaming correlation** — exactly the kind of "graph-native AIOps" capability that distinguishes a tool from a logger.

### Implementation effort

- Stage 3 extraction layer: ~400 lines + per-vendor pattern files (~50 lines each, 4 vendors initially) = ~600 lines
- Stage 4 join logic: ~300 lines (Cypher query helpers + join queue)
- Stage 5 cross-source rules: ~3-5 new Python rules in `python/bonsai_sdk/rules/cross_source.py`
- Total: ~1000-1200 lines + pattern library expansion

**Sequencing**: Tier 5 below. Lands after Tier 1 (wiring) so the multi-source enricher activation is settled first.

---

## <a id="reset"></a>Clean-State Reset Plan

The operator asked: "we also need to decide considering a lot of changes that have arrived if the laptop and cloud bonsai exes should be updated and a revised clean state run should start."

**Recommendation: yes, both.**

### Why reset is appropriate now

1. The current archive contains baseline data captured during CV1 sprint when the chaos runner was intermittently running (last daily check 2026-05-08). Mixed signal.
2. Multiple new code paths landed (change detection, synthesizer, ServiceNow AIOps); their effect on the archive (extra events, different graph structure) makes pre-CV1 data structurally different from post-CV1 data.
3. GNN training will benefit from a clean uniform-architecture archive rather than a mixed-architecture one.
4. The operator's discipline (Bv5 baseline-rotation pattern) supports periodic reset as standard operation.

### What "reset" means concretely

**Laptop**:
1. Halt chaos runner: `bash scripts/chaos_runner.sh --stop`
2. Snapshot current archive to a frozen tarball: `tar czf archive_pre_cv1_freeze.tar.gz runtime/archive/` (preserve for posterity)
3. Move existing archive aside: `mv runtime/archive runtime/archive.pre-cv2`
4. Stop bonsai
5. Pull latest CV1 main; rebuild
6. Bring up clean lab via `make -C lab/dc up`
7. Restart bonsai with fresh empty archive
8. Run new baseline (T1-A-1 from Bv5 — 24-hour quiet baseline)
9. Resume chaos runner

**Cloud (OCI Always Free)**:
Same steps, with:
- Sync final pre-reset archive to GitHub branch `archive-pre-cv2-freeze`
- After reset, daily sync resumes to fresh branch `archive-cv2`

**Total operator effort**: ~30 minutes laptop, ~30 minutes cloud, plus 24h baseline.

**Total wall-clock impact**: 24-48 hours of "clean baseline" time before chaos resumes. Acceptable.

### What NOT to reset

- The graph DB itself if it's smallish (under 500 MB) — useful for reference even after archive reset
- The chaos plan and fault catalogue — these stayed stable; carry forward
- Operator-curated overrides — preserve

### When to next reset

After CV2 Tier 4 (streaming protocols) and Tier 5 (syslog facts) land — those add new signal types and new edge types. Another reset before GNN training so training data has consistent structure throughout.

---

## <a id="tier-1"></a>TIER 1 — Wiring Audit + Dead Code Activation

Address A-1, A-2, A-8 first. These are the architectural lies that need correcting before more code lands on top.

### T1-1 (CV2) — `MultiSourceEnricher` registry + dispatch

**What**: refactor `change_detection.rs::run_capture` to consult an enricher registry. Registry is built at runtime from configured enrichers. Each capture request goes through `(target.vendor, target.capability) -> dispatch`. Default registry contains `GnmiGetConfigEnricher` and `ParserChainCliEnricher` (the wiring for A-2).

**Where**: new `src/enrichment/registry.rs`; refactor `change_detection.rs:284`.

**Done when**: capture request for an OpenConfig device routes to gNMI-Get; capture request for a vendor flagged as CLI-only routes to ParserChain. Smoke test (Tier 2) verifies routing.

### T1-2 (CV2) — Wire `ParserChain` into the runtime

**What**: a new `MultiSourceEnricher` implementation `ParserChainCliEnricher` that:
1. Opens SSH session via configured credentials
2. Runs configured commands per `(vendor, command_pattern)`
3. Sends raw output to `ParserChain::parse()`
4. Returns parsed JSON as `MultiSourceCapture`

**Where**: `src/enrichment/parser_chain_enricher.rs`; ssh client (use `russh` crate, well-maintained MIT).

**Done when**: parser chain enricher is one of the registry-routable enrichers (T1-1); smoke test runs against lab device, parses output via configured parser chain.

### T1-3 (CV2) — Sidecar smoke test

**What**: bash script `scripts/smoke_sidecars.sh` that:
1. Builds both sidecar containers
2. Runs them in detached mode
3. Posts a known input to each `/parse` endpoint
4. Verifies expected JSON structure
5. Tears down

**Where**: `scripts/smoke_sidecars.sh`.

**Done when**: smoke test passes; script ready for cron usage.

### T1-4 (CV2) — Wiring guards in CI

**What**: a CI step that grep-checks the source tree for the dead-code patterns. New rule: every public struct in module `M` must have at least one consumer outside module `M` *or* be in a documented "platform layer" allowlist.

**Where**: `.github/workflows/ci.yml` extension; `scripts/check_wiring.sh`.

**Done when**: CI fails if a new module is added without a consumer.

### T1-5 (CV2) — Verify all CV1 HTTP endpoints actually work

**What**: smoke test script that hits every new HTTP endpoint added in CV1 (`/api/devices/<addr>/recommendations`, `/api/devices/<addr>/gnmi-readiness`, `/api/devices/<addr>/config-history`, `/api/yang/...`) against a running bonsai. Asserts non-error response and non-empty data where expected.

**Where**: `scripts/smoke_cv1_endpoints.sh`.

**Done when**: smoke test passes against lab-deployed bonsai; structured artefact in `runtime/driver_results/cv1_endpoints.json`.

---

## <a id="tier-2"></a>TIER 2 — Lightweight Test Discipline

The token-economy work. Codify what's described in the section above.

### T2-1 (CV2) — Wiring check script

`scripts/check_wiring.sh` (cited in token-economy section). Runs in <10 seconds. Fails on dead-code patterns.

### T2-2 (CV2) — Smoke test framework

`scripts/smoke/` directory. Each script targets one subsystem. Each script:
- Takes target URL as arg (default localhost)
- Runs in <60 seconds
- Outputs structured JSON to `runtime/driver_results/smoke_<subsystem>.json`
- Exits 0 on pass, 1 on fail

Smokes to land:
- `smoke_synthesizer.sh`
- `smoke_change_detection.sh`
- `smoke_yang_library.sh`
- `smoke_servicenow_aiops.sh` (read-only ops only — list incidents, no creates)
- `smoke_output_adapters.sh`
- `smoke_signals_syslog.sh`
- `smoke_signals_snmp.sh`

### T2-3 (CV2) — `runtime/driver_results/` aggregation

Extend `bv5_daily_check.sh` to read every smoke artefact and aggregate. Operator sees one report; AI session reads one report.

### T2-4 (CV2) — Documentation of test discipline

`docs/testing_discipline.md` codifies the three-layer model (wiring, smoke, e2e). PRs must reference applicable layer.

---

## <a id="tier-3"></a>TIER 3 — End-to-End Validation Backlog

The CV1 verification gap. Each of these is a specific run with a specific artefact.

### T3-1 (CV2) — Splunk HEC adapter e2e

Bring up `compose-external --profile splunk`. Configure adapter. Run lab + 1 hour chaos. Verify events. Capture `docs/test_results/e2e_output_adapters/<date>-splunk-pass.md`.

### T3-2 (CV2) — Elastic adapter e2e

Same shape, Elastic.

### T3-3 (CV2) — ServiceNow EM adapter e2e

Against PDI. Capture artefact.

### T3-4 (CV2) — ServiceNow AIOps bidirectional sync e2e

Against PDI. Inject chaos. Verify incident opens in ServiceNow. Update assignment in ServiceNow. Verify reflection in bonsai. Heal chaos. Verify resolve in ServiceNow. Capture artefact.

### T3-5 (CV2) — YANG sync against real OpenConfig repo

`bonsai yang-sync --source github.com/openconfig/public`. Verify modules indexed. Capture artefact.

### T3-6 (CV2) — Synthesizer against real lab discovery

For each lab device: capture `GnmiReadinessReport`, capture synthesizer recommendations, hand-validate that recommendations match expected role-vocabulary subscriptions. Capture artefact.

---

## <a id="tier-4"></a>TIER 4 — Modern Streaming Protocol Tier ⚡ THE NEW WORK ⚡

### T4-1 (CV2) — BMP receiver

Embed Rust BMP parser. Per-prefix-per-peer state. New graph schema (`BmpSession`, `BgpRibEntry`). 3 detection rules: `RouteLeakDetected`, `RouteFlap`, `UnexpectedAsPath`.

### T4-2 (CV2) — BGP-LS via gobgp sidecar

Sidecar gobgp running BGP-LS receiver, JSON output consumed by bonsai. Enrich existing topology nodes with TE attributes. New node `SrPolicy`. 2 detection rules: `SrPolicyDegraded`, `SrlgRiskDetected`.

### T4-3 (CV2) — PCEP parser

Deferred until SP lab up. PCEP receiver, LSP state graph nodes, 2 detection rules: `LspReoptimizationStorm`, `LspPathChange`.

### T4-4 (CV2) — Multi-protocol readiness report

Extend `GnmiReadinessReport` to `StreamingReadinessReport` covering gNMI, BMP, BGP-LS, PCEP per-device. Synthesizer recommends per-protocol subscriptions.

### T4-5 (CV2) — Lab support for new protocols

ContainerLab DC + SP labs configured to enable BMP + BGP-LS where vendor supports. Chaos plan extensions for route-flap and SR-policy degradation.

---

## <a id="tier-5"></a>TIER 5 — Syslog Value Extraction + Cross-Source Joins ⚡ THE NEW WORK ⚡

### T5-1 (CV2) — Syslog pattern files extended with capture groups

Per-vendor patterns get named capture groups + fact field schemas. Initial patterns: BGP, OSPF, IS-IS, interface state, hardware errors, AAA failures.

### T5-2 (CV2) — `SyslogFact` event type + extraction pipeline

Stage 3 architecture from above. Receives `SyslogEvent` from bus, extracts facts, publishes `SyslogFact`.

### T5-3 (CV2) — Cross-source join engine

When `SyslogFact` arrives, attempt join with current graph state. Emits `JoinedFact` or `OrphanFact`.

### T5-4 (CV2) — Cross-source detection rules

3-5 new rules over `JoinedFact`/`OrphanFact`: `SyslogGnmiDisagreement`, `OrphanInterfaceMention`, `MultiSourceCorrelation`.

---

## <a id="tier-6"></a>TIER 6 — Carryover from CV1

Deferred behind Tier 1-5:

- CV1 Tier 7 GNN training (gates on archive depth + reset baseline)
- CV1 Tier 8 documentation refresh (lowest priority)
- Investigation agent (post-MVP, pending token budget)
- Output adapter productive use beyond e2e validation
- Real-hardware-only schemas (CV1 Tier 6)
- Bv2 hardcoding catalogue remainder

---

## <a id="execution-order"></a>Execution Order

### Sprint 1 (1-2 weeks) — Reset + wiring audit ⚡ START NOW ⚡
1. Reset plan (Section 7) — laptop + cloud
2. T1-1 enricher registry
3. T1-2 ParserChain enricher
4. T1-3 sidecar smoke test
5. T1-4 wiring guards in CI
6. T1-5 CV1 HTTP endpoint smoke

### Sprint 2 (1 week) — Test discipline
7. T2-1 wiring check script
8. T2-2 smoke test framework
9. T2-3 driver results aggregation
10. T2-4 testing discipline doc

### Sprint 3 (1-2 weeks) — E2E validation
11. T3-1 Splunk e2e
12. T3-2 Elastic e2e
13. T3-3 ServiceNow EM e2e
14. T3-4 ServiceNow AIOps bidirectional e2e
15. T3-5 YANG sync e2e
16. T3-6 synthesizer e2e

### Sprint 4 (3-4 weeks) — Modern streaming protocols
17. T4-1 BMP receiver
18. T4-2 BGP-LS via gobgp sidecar
19. T4-4 multi-protocol readiness
20. T4-5 lab support
21. T4-3 PCEP (deferred until SP lab up)

### Sprint 5 (2-3 weeks) — Syslog facts
22. T5-1 pattern files extended
23. T5-2 fact extraction pipeline
24. T5-3 cross-source join engine
25. T5-4 cross-source detection rules

### Sprint 6+ — GNN training when archive ready

Continuously throughout: chaos cycle on DC lab; cloud archive sync; daily smoke + e2e checks producing structured artefacts.

### Estimated total
**10-14 weeks** to a state where bonsai has:
- All CV1 dead code activated and exercised
- Lightweight test discipline in CI
- All output adapters and ServiceNow AIOps validated end-to-end
- BMP + BGP-LS + (eventually) PCEP as native streaming sources
- Syslog evolved from event signal to structured fact source with cross-source joins
- Clean uniform-architecture archive ready for GNN training

---

## <a id="guardrails"></a>Guardrails — Updated

### New in CV2

- **No PR lands without wiring check pass.** Dead-code-surface is rejected.
- **No HTTP endpoint lands without smoke test.** Token-burn is engineered out.
- **E2E artefact required for any integration claim.** "It compiles" is not "it works."
- **Streaming sources publish to the bus, not as enrichers.** BMP/BGP-LS/PCEP are first-class Layer-1 data, not Layer-2 fallback.
- **Syslog produces structured facts, not just events.** Pattern files include capture groups and field schemas.
- **Reset before structural archive change.** Mixed-architecture archives are excluded from training data.

### Unchanged from v7-CV1

All prior architectural invariants. Streaming-where-possible hot path. Vault-only credentials. OutputAdapter read-only on bus. AIOps-feeder positioning.

### Anti-patterns to reject

- "Trait exists therefore the architecture works" — no, callsites required
- "It compiles cleanly" — no, smoke test required
- "We'll validate later" — no, validation is the discipline
- "Just rerun cargo and see" — no, test scripts first
- "BMP/BGP-LS are nice-to-have" — no, they're streaming-first sources, not enhancements
- "Syslog is just signal" — no, syslog is multi-source data once we extract structure

---

## What CV2 Explicitly Excludes

- New ingestion layers beyond BMP/BGP-LS/PCEP (no NetFlow, sFlow, IPFIX yet)
- K8s/RBAC/multi-tenancy
- Wireless / hardware-FRU / optical chaos simulation
- Auto-execution of synthesizer recommendations
- Bidirectional integration with non-ServiceNow AIOps platforms

---

*CV2.0 — authored 2026-05-09 after sprint-by-sprint code review of post-CV1 main. Records substantial code landing (~6,400 new lines across change_detection, config_store, parser_chain, synthesizer, yang, multi_source enricher, ServiceNow AIOps integration) but identifies critical gaps: ParserChain and MultiSourceEnricher trait-but-only-one-impl are dead code at runtime; Splunk/Elastic/ServiceNow EM/AIOps/YANG/synthesizer have zero end-to-end validation; daily check loop regressed during CV1 sprint. Tier 1 activates dead code via enricher registry. Tier 2 establishes lightweight test discipline (wiring checks + smoke tests) to address the token-burn problem. Tier 3 lands missing e2e artefacts. Tier 4 introduces BMP, BGP-LS, PCEP as native streaming sources (not enrichers). Tier 5 evolves syslog from event signal to structured-fact streaming source with cross-source joins to gNMI graph state. Reset plan for laptop and cloud labs to avoid mixed-architecture archive contamination. Estimated 10-14 weeks to deployable state with multi-source streaming, validated integrations, and clean training archive ready for GNN. References v7-CV1 for unchanged context.*
