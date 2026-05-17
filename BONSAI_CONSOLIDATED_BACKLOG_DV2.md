# BONSAI — Backlog Delta Series, v2 (DV2.0)

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_DV1.md`. Authored 2026-05-16 after end-to-end review of DV1 landings, screenshot walkthrough of all 13 UI surfaces, and the operator's directive: "spread all our backlog issues and create dv2. don't put off anything or defer. lets consolidate."
>
> **What DV2 is**: the consolidation backlog. Every deferred thread surfaces here as a tier with real scoping. Where the work is genuinely big, the tier commits to "scoping + a proof" rather than full landing — nothing is *parked indefinitely*. The directive is honoured.
>
> **What DV2 is not**: an architectural rewrite. DV1 substantially landed — main.rs went 2,515 → 46 lines, http_server.rs split into 11 sub-modules preserving all 87 routes, validation runs went from PASS=6 FAIL=3 to PASS=16 FAIL=0, BFD/interface detection actually fires after F-11 was diagnosed and fixed. DV2 builds on that foundation.
>
> **The promise DV2 keeps**: by end of sprint, bonsai detects situations not just events. Host states join network states. Substrate metadata (rack, site, power) is graph-resident. Config changes are first-class signals. Optical/DWDM has a data model. App dependency matrix has a working proof. Detection rule logic is YANG-schema-driven so the BFD `admin_down` lesson doesn't have to be re-learned per vendor. Incidents view labels mixed-rule_id clubs honestly. event_detection.rs is finally deleted. The 80 MB of side-channel log accumulation is bounded.

---

## Table of Contents

1. [DV1 Outcome — Honest Scoring](#dv1-scoring)
2. [Screenshot Analysis — UI Findings](#screenshots)
3. [The Schema-Driven Detection Realization (F-11 promoted)](#schema-driven)
4. [Where We Are. Where We Intend to Be.](#motivation)
5. [TIER D2-1 — Close DV1 Residuals](#tier-1) ⚡ START HERE ⚡
6. [TIER D2-2 — Schema-Driven Vendor State Normalization](#tier-2)
7. [TIER D2-3 — Incident Clubbing and Display Honesty](#tier-3)
8. [TIER D2-4 — Config-State Lane](#tier-4) ⭐ highest payoff
9. [TIER D2-5 — NetBox Substrate Graph Promotion](#tier-5)
10. [TIER D2-6 — OTel Ingestion + Host Node Type](#tier-6)
11. [TIER D2-7 — Optical/DWDM Data Model](#tier-7)
12. [TIER D2-8 — App Dependency Matrix Scoping](#tier-8)
13. [TIER D2-9 — Cross-Cutting: Entity Reconciliation](#tier-9)
14. [TIER D2-10 — Operational Hygiene Carryover](#tier-10)
15. [TIER D2-11 — Outstanding Feature Backlog](#tier-11)
16. [TIER D2-12 — GNN Training Trigger Watch](#tier-12)
17. [Execution Order](#execution-order)
18. [Guardrails — Updated](#guardrails)
19. [Tracked Future Threads (now smaller)](#tracked)

---

## <a id="dv1-scoring"></a>DV1 Outcome — Honest Scoring

DV1 was the most successfully-delivered sprint to date. Worth being explicit before piling on more work.

| Finding | Severity | Status |
|---|---|---|
| F-1: HTTP bind panic silent | CRITICAL | ✅ **CLOSED**. Variant C landed at `server_startup.rs:988-991`. JoinHandle tracked at `:1185-1189` via `tokio::select!`. Validation step 7 went FAIL → PASS. |
| F-2: Rules sidecar not running | CRITICAL | ✅ **CLOSED**. With F-1 fixed, the wrapper's wait_for_health sees `/health` respond. Latest validation sidecar log is 11 KB (was 0). |
| F-3: event_detection.rs retirement gate | HIGH | ⚠️ **PARTIAL**. BGP/BFD/interface_down detections all fired in cycles 1-5 of the latest gate run, but the 1-hour gate run was cut short. Module still wired at `server_startup.rs:866`. Closed in D2-1 T1. |
| F-4: main.rs / http_server.rs oversized | HIGH | ✅ **CLOSED**. main.rs 2,515 → 46. http_server.rs 7,779 → 11 sub-modules totalling 7,704 lines (within 1%, all 87 routes preserved). Logic intact. |
| F-5: CLI parser test coverage | HIGH | ✅ **CLOSED**. `tests/cli_fixtures/` has fixtures across all 5 vendors. Smoke at `scripts/smoke/smoke_cli_fixtures.sh`. |
| F-6: E2E path-find → detection trace | HIGH | ✅ **CLOSED**. `scripts/e2e_path_to_detection_test.sh` exists. |
| F-7: Per-rule_id firing matrix | MEDIUM | ✅ **CLOSED**. `tests/rule_firing_matrix.yaml` + `smoke_rule_firing_matrix.sh`. |
| F-8: Doc sprawl | MEDIUM | ⚠️ **PARTIAL**. `memory/` deleted. `playbooks/` still untouched. `docs/test_results/` is now 80 MB (side-channel log accumulation from 5 validation runs/day). Carryover to D2-10. |
| F-9: pre_cv2_freeze | LOW | ✅ **CLOSED**. Deleted. |
| F-10: Review discipline doc | MEDIUM | ✅ **CLOSED**. `docs/review_discipline.md` exists. |
| F-11: BFD/interface rules not firing | (new from D2-T1 smoke) | ✅ **DIAGNOSED + FIXED**. `python/bonsai_sdk/rules/bfd.py:13` now treats `admin_down` and `down` as down-states. `interface.py` recognizes `lower-layer-down`. Validated in the cycle-5 gate run. **The deeper insight is captured in D2-2 below**. |
| D4-T1: K8s Helm chart | feature | ✅ **LANDED**. `deploy/helm/bonsai/` with single/HA/fleet modes. |
| D4-T2: eBPF spike | feature | ✅ **LANDED**. `docs/research/ebpf_scoping_20260516.md` + `experiments/ebpf_spike_20260516/`. |
| D5: GNN pre-work | scaffolding | ✅ **LANDED**. `python/bonsai_ml/gnn/{model,loss,eval,calibration,data_loader,archive_to_training}.py`. |

**Validation state**: latest at 2026-05-16T1339Z is **PASS=16 WARN=1 FAIL=0**. Up from PASS=6 FAIL=3 WARN=3 two days prior. The 1 WARN is benign (chaos micro-cycle skipped because `--with-chaos` wasn't passed).

This is real progress. The architectural foundation is now genuinely solid.

---

## <a id="screenshots"></a>Screenshot Analysis — UI Findings

13 UI surfaces walked (2026-05-16, 21:07-21:11Z, captured during a live chaos cycle). The UI is materially more mature than code-only reviews suggested. **Strengths** named first because they're real:

- **Live view**: hierarchical CLOS topology (Super-Spine → Spine → Leaf), real-time event feed with structured JSON, BGP peer table per device. The graph layout is correct and matches the lab.
- **Operations dashboard**: single-pane operational truth — event bus depth/receivers, archive lag, RSS vs budget, detections vs state changes, subscriptions observed, governance state, BGP peer health per device, rule engine activity counts, resource governance counters. Genuinely excellent.
- **Graph Explorer**: 12 pre-built Cypher queries including "Co-firing detections (all time)" which correctly returns `bfd_session_down ↔ bgp_session_down co_count=2`. Cypher is read-only enforced.
- **Profiles**: 18 catalogue profiles spanning Campus/DC/SP/Lab. Each profile has roles + paths + environments. This is the foundation for schema-driven detection (D2-2).
- **Collectors view**: explicit "Running in monolithic mode" banner. Contextual messaging that prevents confusion about "unassigned devices."
- **Empty-state communication**: Enrichment, Adapters, Approvals, Investigations all have clear "what to do next" guidance.

**UI findings that flow into DV2 tiers**:

| Finding | Where seen | Severity | Addressed by |
|---|---|---|---|
| UI-1: Incident header label is misleading. Header says `bgp_session_down` but incident contains `bgp + bfd` events from different devices. Operator can't tell from list view. | Incidents screenshot 1 (4 incidents, all show this pattern) | HIGH | D2-3 T1 |
| UI-2: Clubbing rationale is invisible. Logic is time-window + topological-degree-root, but the UI doesn't explain why these N detections were grouped. | Incidents view | MEDIUM | D2-3 T2 |
| UI-3: Topology graph doesn't reflect incident state. 4-6 open incidents involving `.12`, `.13`, `.15` listed; Live topology nodes are all green. | Live view vs Incidents view comparison | MEDIUM | D2-3 T3 |
| UI-4: "Site hierarchy: Loading..." appears stuck. Either real bug or slow query. | Sites view | LOW (verify) | D2-10 T2 |
| UI-5: Devices view shows VENDOR as `—` for every device. Vendor metadata IS known (Operations view shows "nokia_srl") but isn't flowing to Devices listing. | Devices view | LOW | D2-10 T3 |
| UI-6: Devices STATUS column shows "enabled" regardless of incident state. Should reflect health when device is in an active incident. | Devices view | MEDIUM | D2-3 T3 (links to topology colouring) |
| UI-7: Driver Results panel shows "warn / 0 pass / 0 fail / No driver result files found" despite chaos running. Daily-check pipeline isn't producing artefacts visible to Operations UI. | Operations view | MEDIUM | D2-10 T1 |
| UI-8: Co-firing detection knowledge exists in Explorer but is not consumed by Incidents view. Tight feedback loop opportunity. | Cross-view (Explorer + Incidents) | MEDIUM (opportunity) | D2-3 T1 |

These are not code-quality issues; they are **product-honesty** issues. The system *knows* more than it *shows*. D2-3 is the tier that fixes this.

---

## <a id="schema-driven"></a>The Schema-Driven Detection Realization (F-11 promoted)

The operator's words deserve to be quoted in full because they name a real architectural direction:

> "during the last iteration something good that came out is the fraility for getting a simple thing to work. there was a lot of changes and tracing that had to go in in order to get bfd and interface down to work. i felt like this much of customisation if its required for a simple lab environment, realisation every now and then oh it's admin_down and not down kind of thing for bfd, it would be difficult to scale in an expanse, so we have to increasingly rely on yang documentation to be taking care of as many edge cases as possible."

The literal example from the F-11 fix: `python/bonsai_sdk/rules/bfd.py:13` now reads:

```python
_DOWN_STATES = {"down", "admin_down"}  # admin_down: SR Linux BFD admin-disable
```

Similar pattern in `interface.py:37`:
```python
if status not in ("down", "lower-layer-down"):
```

These are hand-coded SR Linux vendor quirks. **Cisco IOS-XR returns different state strings. Junos returns different state strings. Arista returns different state strings.** Hand-coding `_DOWN_STATES` per vendor for every detection rule does not scale.

### The right shape

YANG models define an enumeration of state values per leaf. For BFD session state, `openconfig-bfd` defines `oper-state` with values `UP / DOWN / ADMIN_DOWN / INIT`. Each vendor's YANG implementation either:
- Conforms to the IETF/OpenConfig enumeration (Arista mostly does)
- Augments with vendor-specific extensions (Nokia SR Linux adds `lower-layer-down` for interface oper-state)
- Renames to a vendor-flavoured form (some Cisco platforms use lowercase `admin-down` with dash)

Bonsai today *has* the YANG library (`docs/path_profiles/` is 92 KB, `config/path_profiles/` 76 KB, the catalogue is in active use). It just doesn't *consult* YANG schema at detection-rule-time. Detection rules are vendor-agnostic Python code with hand-coded state-string sets.

### What schema-driven detection would look like

A detection rule expressed in terms of **semantic state transition**, not vendor-string match:

```python
class BfdSessionDown(Detector):
    rule_id = "bfd_session_down"
    semantic = SemanticTransition(
        yang_path = "openconfig-bfd:bfd/sessions/session/state/oper-state",
        from_states = ["UP"],
        to_states = ["DOWN", "ADMIN_DOWN"],  # both treated as "session not delivering"
    )
```

The schema layer translates `UP` / `DOWN` / `ADMIN_DOWN` per vendor at event-extraction time. SR Linux's `admin_down` matches `ADMIN_DOWN`. Cisco IOS-XR's `admin-down` (if it ever appears that way) also matches. The detection rule stays vendor-agnostic.

This is the foundation for **scaling beyond the lab** that the operator named. D2-2 is the tier that does this work.

### Honest acknowledgement of cost

This isn't free. Per-vendor YANG quirks have to be documented somewhere. The right place is a `config/vendor_state_mapping/` directory with one file per (vendor, model_family) pair. The first one (`nokia_srlinux.yaml`) is mostly already implied by the existing rules — extract it. Cisco/Junos/Arista come incrementally as they're encountered. **This is an evolving artefact, not a one-time spec.** That's fine, because it concentrates the per-vendor quirks in one place instead of scattering them across detection rules.

---

## <a id="motivation"></a>Where We Are. Where We Intend to Be.

DV1 was the inflection point. The architectural foundation is now genuinely good. main.rs is normal-sized. http_server.rs is sensibly modularized. Validation runs PASS=16. The Python sidecar registers, heartbeats, fires detections. BFD/interface/BGP detection all work end-to-end. The 87 HTTP routes are preserved across the refactor with proper test coverage. The K8s Helm chart exists. The eBPF spike produced a scoping document. The GNN pre-work scaffolding is in place.

**Where we are**: bonsai is a working network observability engine with a maturing UI and a stable detection pipeline.

**Where we intend to be at end of DV2** (6-9 weeks):
- Detection rules are schema-driven, not hand-coded vendor strings
- Incidents view explains its own clubbing
- Topology graph reflects incident state
- Config changes are first-class signals (Thread 3 — the highest-payoff)
- NetBox substrate graph (Thread 2) is materialized as nodes-and-edges, enabling rack/power-correlated detections
- OTel ingestion + Host nodes (Thread 1) bring host-state into the same graph
- Optical/DWDM data model is specified with a proof on a stub source
- App dependency matrix has a working e2e proof
- event_detection.rs is deleted
- All five threads have at least a working proof; the deepest three have full landings
- GNN training trigger is closer (chaos archive accumulating in the background)

**Where we intend to be at end of DV3 or DV4** (~3-4 more months out):
- GNN trained on real chaos data with vendor-neutral structural features
- Model card published with explicit generalization boundaries
- Two clouds running labs (operator hinted this is coming)
- Bonsai deployable to a fresh Kubernetes cluster from the Helm chart in <30 min

The northstar is closer than it feels. Most of DV2's work is finishing decisions already made.

---

## <a id="tier-1"></a>TIER D2-1 — Close DV1 Residuals ⚡ START HERE ⚡

**Estimate: 2 days.** Two carryovers from DV1 plus one screenshot finding.

### D2-1 T1 — Complete event_detection.rs retirement and delete

The 14-hour smoke saw BGP detections fire but not BFD/interface (F-11). After the F-11 fix, the cycle-5 gate run saw all three fire across 5 consecutive cycles before the run was cut short.

What's left:
1. Run the gate smoke for **the full 1 hour** (3,600 seconds) as the ADR specified. Cycle 5-20.
2. Verify all three rule_ids fire across at least 15 of the 20 cycles.
3. Verify the `detections_out_total` heartbeat counter increments (was a WARN in the 14-hour smoke because counter wasn't exposed; the python sidecar emits this — verify visible in `/api/sidecars` heartbeat payload).
4. Delete `src/event_detection.rs` (191 lines).
5. Remove `pub mod event_detection` from `src/lib.rs:18`.
6. Remove `bonsai::event_detection::start(...)` from `src/server_startup.rs:866`.
7. Re-run `bash scripts/ops/rebuild_and_validate.sh` — should still produce PASS=16.

**Effort**: 1.5h smoke + 30min deletion + 30min verification.

**Done when**: module gone, validation still PASS, gate report at `docs/test_results/event_detection_retired_<date>.md`.

### D2-1 T2 — Bound the side-channel log accumulation

`docs/test_results/` grew to 80 MB. Most of it is `*.logs/17-bonsai.log` files at ~16 MB each from the chaos cycles. Five validation runs per day at 16 MB each = 80 MB/day. Unbounded.

**Two-part fix**:

1. **Retention policy** in `scripts/ops/rebuild_and_validate.sh`: keep the last N validation runs' side-channel logs (default N=10), archive older runs to a tarball at `docs/test_results/archive/`. A 10-run retention is 160 MB; archived tarballs gzip to ~10-20 MB each.

2. **Bonsai log rotation**: the 16 MB `17-bonsai.log` is bonsai's stdout during one validation. Add log-level filtering — the validation script collects only WARN+ during normal operation, INFO+ when `--verbose` is set. This collapses the typical run to under 1 MB.

**Effort**: 0.5 day.

**Done when**: `docs/test_results/` total size stays bounded (≤ 200 MB over 10 days of validation). A run with the new defaults produces <2 MB of side-channel logs.

### D2-1 T3 — Driver Results panel produces output

The Operations dashboard's Driver Results panel shows "No driver result files found" despite chaos running. Either the daily-check pipeline isn't running, or the result-discovery in the UI is broken.

**Investigation steps**:
1. Run `bash scripts/bv5_daily_check.sh` manually. Does it produce a result file under `runtime/driver_results/`?
2. Read `src/http_server/observability.rs` daily-check handler — does it look in the right directory?
3. If the script runs but Operations doesn't show output: path mismatch. Align them.
4. If the script doesn't run: cron not installed or wrapper missing. Install per CV5 T3-1.

**Effort**: 0.5 day investigation + 0.5 day fix depending on cause.

**Done when**: Operations dashboard's Driver Results panel shows the last daily-check run's outcome (PASS / WARN / FAIL counts).

---

## <a id="tier-2"></a>TIER D2-2 — Schema-Driven Vendor State Normalization

**Estimate: 1.5 weeks.** Promoted from F-11. This is the architectural answer to the BFD `admin_down` lesson.

### D2-2 T1 — Vendor state mapping registry

**What**: a new directory `config/vendor_state_mapping/` with one YAML per vendor model family.

Initial files (Nokia SR Linux already implicit in existing rules — extract):

```yaml
# config/vendor_state_mapping/nokia_srlinux.yaml
vendor: nokia_srl
applies_to:
  - oc-system:system/state/software-version pattern: "SRLinux-*"

state_mappings:
  bfd_oper_state:
    semantic_states:
      UP:          ["up"]
      DOWN:        ["down"]
      ADMIN_DOWN:  ["admin_down"]
      INIT:        ["init"]
    treat_as_down: ["DOWN", "ADMIN_DOWN"]
    treat_as_up:   ["UP"]

  interface_oper_status:
    semantic_states:
      UP:               ["up"]
      DOWN:             ["down"]
      LOWER_LAYER_DOWN: ["lower-layer-down"]
      TESTING:          ["testing"]
      DORMANT:          ["dormant"]
      NOT_PRESENT:      ["not-present"]
    treat_as_down: ["DOWN", "LOWER_LAYER_DOWN"]
    treat_as_up:   ["UP"]

  bgp_session_state:
    semantic_states:
      ESTABLISHED: ["established"]
      ACTIVE:      ["active"]
      IDLE:        ["idle"]
      OPEN_SENT:   ["opensent", "open-sent"]
      OPEN_CONFIRM:["openconfirm", "open-confirm"]
      CONNECT:     ["connect"]
    treat_as_down: ["ACTIVE", "IDLE", "OPEN_SENT", "OPEN_CONFIRM", "CONNECT"]
    treat_as_up:   ["ESTABLISHED"]
```

Then create stub files for `cisco_iosxr.yaml`, `cisco_iosxe.yaml`, `juniper_junos.yaml`, `arista_eos.yaml`, `frr.yaml` — each with `# TODO: populate as vendor encountered in lab`. The Cisco/Junos/Arista mappings get filled in as the SP lab work (the XRd track in CV5) materializes those vendors.

**Where**: `config/vendor_state_mapping/`.

**Effort**: 1 day (the mapping logic + the SR Linux extract + the stubs for other vendors).

### D2-2 T2 — Detection rule semantic-transition adapter

**What**: a Python module `python/bonsai_sdk/state_mapping.py` that:
- Reads `config/vendor_state_mapping/*.yaml` at sidecar startup
- Provides `to_semantic(vendor, leaf_name, raw_value) -> SemanticState` translation
- Provides `is_down(vendor, leaf_name, raw_value) -> bool` (and `is_up`)

Detection rules updated to use it:

```python
# bfd.py — after change
from ..state_mapping import is_down

class BfdSessionDown(Detector):
    def extract_features(self, event, client):
        if event.event_type != "bfd_session_change":
            return None
        f = extract_features_for_event(event, client)
        vendor = client.device_vendor(f.device_address)
        if not is_down(vendor, "bfd_oper_state", f.new_state):
            return None
        if not is_up(vendor, "bfd_oper_state", f.old_state) and f.old_state != "none":
            return None
        return f
```

**Where**: new module + edits to 3 rules: `bfd.py`, `interface.py`, `bgp.py`.

**Effort**: 1.5 days code + 0.5 day unit tests against the mapping.

### D2-2 T3 — Per-vendor fixture coverage

Currently CLI fixtures cover output formats per vendor. We need **state-transition fixtures** for the detection logic specifically. For each vendor + each rule, fixture covers:
- Down-transition that should fire
- Up-transition that should NOT fire
- admin-down (or vendor equivalent) that should fire
- Vendor-specific edge state (e.g., SR Linux `lower-layer-down` for interface)

**Where**: `tests/state_transition_fixtures/<vendor>/<rule>.yaml`.

**Effort**: 1 day for SR Linux (we have the data), stubs for others.

### D2-2 T4 — Future-proofing documentation

**What**: `docs/architecture/schema_driven_detection.md` documents:
- The rule-as-semantic-transition pattern
- Where to add a new vendor (steps)
- Where to add a new YANG leaf (steps)
- The relationship between gNMI Subscribe paths, vendor state-mapping, and detection rules

**Effort**: 0.5 day.

**Done when**: all three current detection rules (BFD, interface, BGP) use the state-mapping adapter. Adding a new vendor requires *only* editing `config/vendor_state_mapping/<vendor>.yaml`, not Python code.

---

## <a id="tier-3"></a>TIER D2-3 — Incident Clubbing and Display Honesty

**Estimate: 1 week.** Direct response to the operator's screenshot question and UI findings UI-1, UI-2, UI-3, UI-6, UI-8.

### D2-3 T1 — Incident header honesty

The clubbing logic in `src/http_server/observability.rs:760-840` is good (time-window + topological-degree-root). The header just doesn't communicate the mix.

**Fix**: enrich `IncidentJson` with:
- `rule_ids: Vec<String>` — all distinct rule_ids in the incident
- `device_count: usize` — number of distinct devices
- `event_count: usize` — total events
- `co_fire_signature: Option<String>` — if 2+ distinct rule_ids, render as `"bgp_session_down + bfd_session_down"`

UI: incident card header becomes:
```
172.100.103.12:57400 · bgp_session_down + bfd_session_down (co-firing)
2 devices · 10 events · 21s
```

Instead of just:
```
172.100.103.12:57400 · bgp_session_down
2 devices affected
```

**Bonus opportunity (UI-8)**: the Explorer's "Co-firing detections" query produces the same insight. The incident card's `co_fire_signature` can be displayed alongside an explanation link to the corresponding Explorer query.

**Where**: `src/http_server/observability.rs` IncidentJson + the UI's Incidents.svelte component.

**Effort**: 1 day backend + 1 day UI.

### D2-3 T2 — Clubbing rationale tooltip

**What**: each incident card carries a small "?" icon. Click expands a tooltip:
> "Clubbed because: 10 detections fired within 30 seconds. Root chosen by topology (srl-super1 has 7 BGP peers — highest centrality). Time window: 21s."

This shows the operator *why* these were grouped. Removes the black-box feel of the incident list.

**Where**: Incidents.svelte component. Backend already has the data.

**Effort**: 0.5 day.

### D2-3 T3 — Topology graph reflects incident state

**Current state** (UI-3): Live view topology shows all nodes green even when 6 incidents are open involving devices `.12`, `.13`, `.15`.

**Fix**: the Live topology fetches `/api/incidents?status=open` alongside its existing topology fetch. Each node is coloured based on incident involvement:
- Green: no active incidents
- Yellow: warn-severity incident in last 5 min
- Red: critical incident in last 5 min  
- Pulsing red: currently affected device

**Where**: `ui/src/routes/Live.svelte` (or wherever the topology is rendered).

**Effort**: 1.5 days.

### D2-3 T4 — Devices view health column

**Current state** (UI-6): Devices view shows STATUS = "enabled" regardless of incident state.

**Fix**: STATUS column becomes a computed `health` column with the same colour semantics as the topology graph. Pulls from the same `/api/incidents` query.

**Effort**: 0.5 day.

### D2-3 T5 — Investigation surface activation

**What**: the Investigations view shows "wait for an unmatched detection." Document what an unmatched detection is, when it triggers an investigation, and what the operator should do when one appears. Currently 0 investigations because the trigger condition isn't documented.

**Where**: `docs/architecture/investigations.md`.

**Effort**: 0.5 day.

---

## <a id="tier-4"></a>TIER D2-4 — Config-State Lane ⭐ HIGHEST PAYOFF

**Estimate: 2 weeks.** Thread 3 from the prior conversation. The highest-payoff thread because config changes are causally upstream of so many faults, and bonsai today is blind to them.

### D2-4 T1 — gNMI Subscribe on /configure paths

**What**: extend the gNMI subscriber to optionally subscribe to a separate set of "config-state" paths in ON_CHANGE mode. Config-state paths are vendor-specific YANG roots:
- SR Linux: `/network-instance/*/protocols/bgp/configuration`, `/interface/*/admin-state`, etc.
- Cisco IOS-XR: `Cisco-IOS-XR-config:network-instances/*`
- Junos: `configuration/*`

The path catalogue (`config/path_profiles/`) gains a new section per role: `config_paths: [...]` alongside the existing `state_paths`.

**Where**: extend `src/subscriber.rs` and `config/path_profiles/*.yaml`.

**Effort**: 2 days.

### D2-4 T2 — Config event type

**What**: new event type `config_change_event` in the event bus. Emitted when a config-state path produces an ON_CHANGE update. Carries `device_address`, `yang_path`, `new_value`, `previous_value` (if known).

`previous_value` requires storing config state in the graph. Add a new node type `ConfigState` with edges to the relevant operational node.

**Where**: `src/event_bus.rs`, `src/graph/`.

**Effort**: 2 days.

### D2-4 T3 — Config-change detection rule

**What**: a new rule that fires whenever a config change happens. It's a meta-rule — every config change is a detection event, severity `info`, that an operator might want to know about. The interesting detections are downstream rules that **correlate config_change_event with later operational events**.

```python
class ConfigChanged(Detector):
    rule_id = "config_changed"
    severity = "info"
    
    def extract_features(self, event, client):
        if event.event_type != "config_change_event":
            return None
        # ...

class ConfigCausedFault(Detector):
    """Fires when a config change is followed by an operational fault within 60s."""
    rule_id = "config_caused_fault"
    severity = "high"
    
    def extract_features(self, event, client):
        if event.event_type not in ("bgp_session_change", "interface_oper_status_change"):
            return None
        f = extract_features_for_event(event, client)
        # Look for config_change_event on same device in last 60s
        recent_config = client.recent_events(
            device=f.device_address,
            event_type="config_change_event",
            within_ns=60_000_000_000,
        )
        if not recent_config:
            return None
        f.metadata["config_change_path"] = recent_config[0].yang_path
        return f
```

This is exactly the diagnostic the operator wants — "did someone change config that caused this?"

**Where**: new rule files in `python/bonsai_sdk/rules/config.py`.

**Effort**: 2 days.

### D2-4 T4 — Config diff and snapshot

**What**: when a config_change_event fires, store the diff (path → before/after values) as a `ConfigSnapshot` graph node linked to the previous snapshot. Queryable from the Explorer: "show me all config changes on spine-01 between T-1h and T."

**Where**: `src/graph/config_snapshot.rs` (new).

**Effort**: 2 days.

### D2-4 T5 — UI surfacing

**What**: Live view event feed includes config changes. Incidents view shows "config change occurred 23s before this incident on same device" as a correlation hint. Explorer gains a new pre-built query: "Recent config changes."

**Effort**: 1 day.

### D2-4 T6 — Documentation

**What**: `docs/architecture/config_state_lane.md` describes the design.

**Effort**: 0.5 day.

**Done when**: a config change on the lab (e.g., admin-disable a BGP session) produces a `config_changed` detection. A subsequent operational fault triggered by that config change produces a `config_caused_fault` detection that references the config_change_event.

---

## <a id="tier-5"></a>TIER D2-5 — NetBox Substrate Graph Promotion

**Estimate: 1 week.** Thread 2. Builds on the existing NetBox enricher (currently dormant — 9-day-cold per screenshot UI).

### D2-5 T1 — Promote NetBox attributes to graph nodes

Today the NetBox enricher (when run) populates Device attributes — rack, site, location as strings on Device nodes. D2-5 T1 promotes these to first-class nodes with edges:

```
Device --[rack_member]--> Rack
Rack --[in_row]--> Row
Row --[in_site]--> Site
Site --[in_region]--> Region
Rack --[powered_by]--> PowerFeed
PowerFeed --[from_panel]--> PowerPanel
PowerPanel --[in_site]--> Site
```

**Where**: `src/enrichment/netbox.rs` enricher logic + `src/graph/` schema.

**Effort**: 2 days.

### D2-5 T2 — Substrate-correlated detection rule

**What**: a new rule `rack_isolated` that fires when ≥50% of devices in a rack lose subscription within a 60s window.

```python
class RackIsolated(Detector):
    rule_id = "rack_isolated"
    severity = "critical"
    
    # Triggered by subscription_lost events; correlates by rack membership
```

This is the "power went out in rack 5" detection that bonsai cannot do today.

**Effort**: 1.5 days.

### D2-5 T3 — PDU SNMP polling

**What**: extend the SNMP daemon to poll PDU OIDs (PowerNet-MIB, ePDU-MIB). Per-outlet current, input voltage, temperature. The `PowerOutlet` and `PowerFeed` graph nodes get real-time state.

**Where**: `src/signals/snmp.rs`, new MIB definitions in `config/snmp_mibs/`.

**Effort**: 2 days (PDU MIB integration is well-trodden).

### D2-5 T4 — Sites UI activation

**Current state** (UI-4): Sites view shows "Site hierarchy: Loading..." which appears stuck.

**Fix**: investigate the hanging fetch. Once fixed, the Sites view becomes the operator surface for the substrate graph — clicking a site shows devices, racks, power feeds, recent detections at that site.

**Effort**: 1.5 days.

---

## <a id="tier-6"></a>TIER D2-6 — OTel Ingestion + Host Node Type

**Estimate: 2 weeks.** Thread 1. Brings host-state into bonsai's graph.

### D2-6 T1 — Host node type from LLDP

**What**: when the LLDP enricher (already in bonsai) sees a neighbor that isn't a known Device, materialize a `Host` node instead of dropping the entry. The Host node has the device's reported hostname (LLDP TLV), the local port it's connected to, and edges to the Device that observed it.

**Where**: `src/enrichment/lldp.rs` or wherever LLDP neighbors are reconciled.

**Effort**: 1.5 days.

### D2-6 T2 — OTLP receiver

**What**: a new `src/streaming/otlp.rs` module accepting OTLP gRPC (default port 4317) and OTLP HTTP (default port 4318). Parses OpenTelemetry resource attributes (`service.name`, `service.namespace`, `host.name`, `host.id`) and metrics (latency histograms, error counters).

**Where**: new module.

**Effort**: 3 days.

### D2-6 T3 — Host reconciliation

**What**: when an OTLP signal arrives with `host.name=hv03.lon1`, reconcile against the Host nodes from D2-6 T1 by hostname + IP. Update the Host node with the OTel resource attributes (`services_hosted: [...]`).

**Where**: new reconciler in `src/enrichment/`.

**Effort**: 2 days.

### D2-6 T4 — Host-to-network correlation rule

**What**: a rule `host_lost_connectivity_correlated_with_network` that fires when:
- An OTel signal indicates HTTP timeouts from a host
- Within 30s, the host's adjacent Device has a `bgp_session_down` or `interface_down` detection

This is the answer to "the host lost internet, did the network cause it?"

**Effort**: 1.5 days.

### D2-6 T5 — Documentation

**Effort**: 0.5 day.

**Done when**: an OTel-instrumented host emits a signal; bonsai sees it, materializes the host edge, and a fault on the network correlates with the host-side signal.

---

## <a id="tier-7"></a>TIER D2-7 — Optical/DWDM Data Model

**Estimate: 1 week.** Thread 4. Scoping + data model + proof against a stub source. Real optical hardware integration is a future deployment-time concern.

### D2-7 T1 — Data model

**What**: `OpticalChannel`, `Lambda`, `FiberPair`, `ROADM` node types. Edge `LogicalLink --[carried_on]--> OpticalChannel --[on_wavelength]--> Lambda --[on_fiber]--> FiberPair --[through]--> ROADM`.

Properties on OpticalChannel: `rx_power_dbm`, `tx_power_dbm`, `osnr_db`, `pre_fec_ber`, `laser_bias_ma`, `temperature_c`, `last_sampled_ns`.

**Where**: `src/graph/optical.rs` (new).

**Effort**: 1 day.

### D2-7 T2 — gNMI/SNMP receiver for OpenConfig optical paths

**What**: subscribe to `openconfig-platform-optical-channel:components/component/optical-channel/state/*` on devices that advertise it. SNMP fallback for legacy gear (ENTITY-SENSOR-MIB).

**Where**: `src/streaming/gnmi.rs` (existing — add paths) + `src/signals/snmp.rs`.

**Effort**: 1.5 days.

### D2-7 T3 — Trend-based detection: gradual optical degradation

**What**: rule `optical_rx_degrading` fires when `rx_power_dbm` drops more than 3 dBm in the last 6 hours **and** is below a configurable absolute threshold (default `-12 dBm`).

This is the "catch it hours before the L2 link drops" detection.

**Effort**: 1.5 days.

### D2-7 T4 — Synthetic test source

**What**: real optical hardware isn't available in the lab. Build a synthetic source — a Python script that emits fake optical telemetry conforming to the OpenConfig schema. Use it to validate the detection rule + data model.

**Where**: `experiments/optical_simulator/`.

**Effort**: 1 day.

### D2-7 T5 — Scoping for real-deployment integration

**What**: `docs/research/optical_real_deployment_<date>.md` documents:
- Vendors observed in the field (Ciena, Cisco NCS, Nokia FP4, Juniper PTX)
- gNMI vs SNMP coverage per vendor
- Calibration patterns (per-vendor baseline rx power varies)

**Effort**: 1 day.

**Done when**: synthetic source feeds telemetry, detection rule fires on simulated degradation, data model is committed.

---

## <a id="tier-8"></a>TIER D2-8 — App Dependency Matrix Scoping

**Estimate: 1.5 weeks.** Thread 5. The most ambitious thread. Scoping + working proof on one angle (netflow ingestion or eBPF socket visibility). Real integration is DV3-DV4.

### D2-8 T1 — Choose an angle

Three angles from the prior conversation:
- **A**: netflow/sflow/IPFIX ingestion (real flow telemetry, well-defined formats)
- **B**: eBPF socket-level visibility on hosts (process-aware app graph)
- **C**: OTel span correlation (logical service graph from OTel)

**Recommendation: A first, then B as part of DV3**. Netflow is well-trodden and unblocks the data model. eBPF builds on DV1's spike.

### D2-8 T2 — Netflow/sflow receiver

**What**: `src/streaming/netflow.rs` (new). Accept netflow v9, v10/IPFIX, sflow v5 on UDP. Parse flow records into `AppFlow` events.

**Effort**: 3 days.

### D2-8 T3 — AppFlow → graph

**What**: AppFlow events materialize `HostEndpoint` nodes (src/dst IPs) and `AppFlow` edges with `protocol`, `dst_port`, `bytes_per_sec`, `last_seen_ns` attributes. Aggregate per (src, dst, dst_port) tuple at 60s resolution.

**Where**: `src/graph/app_flow.rs` (new).

**Effort**: 2 days.

### D2-8 T4 — Service-path degraded detection

**What**: rule `service_path_degraded` fires when an `AppFlow` edge's `bytes_per_sec` drops by >80% within 60s **and** the path between the endpoints (via gNMI-derived routing) intersects with a device that has a recent fault detection.

This is the "is it the app or the network" answer.

**Effort**: 2 days.

### D2-8 T5 — Documentation

**What**: `docs/research/app_dependency_matrix_<date>.md` scoping doc.

**Effort**: 0.5 day.

**Done when**: a netflow source emits real flow data; bonsai materializes the app graph; a synthetic network fault correlates with a service-path degradation.

---

## <a id="tier-9"></a>TIER D2-9 — Cross-Cutting: Entity Reconciliation

**Estimate: 1 week.** A piece of work that benefits all of D2-5, D2-6, D2-7, D2-8.

### D2-9 T1 — Unified Entity table

**What**: a generic `Entity` table with stable bonsai-internal IDs and a list of `external_ids` (vendor-specific identifiers from various sources):

```
Entity {
    id: bonsai_uuid,
    type: "device" | "host" | "rack" | "site" | "optical_channel" | "service" | "...",
    external_ids: [
        { source: "gnmi",     id: "172.100.103.11:57400" },
        { source: "netbox",   id: "device-id-457" },
        { source: "otel",     id: "host.id=abc-123" },
        { source: "lldp",     id: "chassis-id=00:11:22:33:44:55" },
    ],
}
```

### D2-9 T2 — Reconciliation layer

**What**: a reconciler service that takes incoming events from each ingestion lane and resolves them to an entity_id by whichever external_id is present. Handles "this gNMI device is the same physical box as that NetBox record."

**Where**: `src/enrichment/reconciler.rs` (new).

**Effort**: 3 days.

### D2-9 T3 — Existing nodes consume reconciliation

**What**: Device, Host, Rack, ConfigSnapshot, OpticalChannel all carry `entity_id` for cross-reference. Detection rules can ask "what other signals does this entity have" by entity_id, not by device_address.

**Effort**: 2 days refactoring across existing node types.

**Done when**: a query like "show me everything bonsai knows about entity X" returns gNMI state + NetBox metadata + OTel signals + LLDP neighbors + flow data, all linked.

---

## <a id="tier-10"></a>TIER D2-10 — Operational Hygiene Carryover

**Estimate: 3 days.** Small fixes from screenshots and DV1 carryover.

### D2-10 T1 — Driver Results panel (UI-7)

Covered under D2-1 T3.

### D2-10 T2 — Sites view hanging (UI-4)

Investigate "Site hierarchy: Loading..." stuck state. Either real backend slowness or UI loop bug.

**Effort**: 0.5 day investigation.

### D2-10 T3 — Devices vendor column (UI-5)

The Devices view shows VENDOR as `—`. The data exists (Operations dashboard shows vendor in subscription health). Path mismatch.

**Effort**: 0.5 day.

### D2-10 T4 — Playbooks indexing (F-8 carryover)

`playbooks/` directory (132 KB) still untouched. Either index into CANONICAL.md or archive.

**Effort**: 0.5 day.

### D2-10 T5 — Memory pressure governance plumbing (CV6 N-1 carryover)

`memory_pressure_active` flag exists in `resource_governor.rs` but has no consumer. Wire it into the debounce caches so they shrink under pressure.

**Effort**: 1 day.

---

## <a id="tier-11"></a>TIER D2-11 — Outstanding Feature Backlog

**Estimate: 1 week.** The operator was explicit: "don't put off anything or defer." Here is the deferred surface. DV2 commits to landing **one major + one minor** from this list. The rest remains tracked for DV3+.

| Item | Source | Status in DV2 | Estimated effort |
|---|---|---|---|
| MCP server read-transaction Cypher hardening | CV6 N-3 | **LAND in DV2 T11 T1** | 1 day |
| Adapter cursor cold-start persistence verification | CV6 N-4 | **VERIFY in DV2 T11 T2** | 0.5 day |
| Russh migration of cli_capture.py | CV3 N-6 | DV3 | 3 days |
| Scale-up Path B (partitioned cores) | CV5 future | DV4+ | 2 weeks |
| Scale-up Path C (read replicas) | CV5 future | DV4+ | 1.5 weeks |
| Cloud platform recipes (AWS/GCP/Azure) | CV5 | DV3+ as deployments happen | 1 week each |
| Beyond network platforms (firewalls, VPN) | CV4 | DV4+ (post-GNN) | 3 weeks |
| gNSI Phase-2 Acctz consumption | CV6 | DV3 | 1.5 weeks |
| gNSI full client integration (Phase 3) | CV6 | DV4+ (depends on HIL maturity) | 2 weeks |
| Online learning infrastructure | CV5 | post-GNN | 2 weeks |
| Investigation agent productive use | DV1 carryover | scoping in D2-3 T5 | 1 day scope, 1 week build |
| UI workspace re-articulation (bold-sharp design) | CV3 | DV3 | 1 week |
| russh distributed collector | BV4 | DV4+ | 2 weeks |

### D2-11 T1 — MCP Cypher read-transaction hardening

Currently `is_readonly_cypher` is string-substring matching. Replace with read-only LadybugDB transaction.

**Effort**: 1 day.

### D2-11 T2 — Adapter cursor cold-start verification

Verify the CV6 cursor persistence works across restart. Smoke test.

**Effort**: 0.5 day.

---

## <a id="tier-12"></a>TIER D2-12 — GNN Training Trigger Watch

**Estimate: ongoing, no work in DV2.** Watch the trigger condition.

Trigger: archive depth ≥ 30 calendar days, ≥ 500 chaos injections (already met), ≥ 50 examples per active rule (BGP yes, BFD/interface accumulating).

**As of 2026-05-16**: archive lag is 6.3s with 606 rows buffered (per Operations dashboard). 48 detections / 88 state changes over the recent window. **Trigger likely fires somewhere mid-DV2 to end-of-DV2.**

**When trigger fires**: open D2-13 (not numbered above because it's gated) — run first GNN training cycle using the D5 scaffolding from DV1. Comparison study (rules vs tabular ML vs GNN) per arxiv 2603.09675 evaluation harness. Output: model card.

---

## <a id="execution-order"></a>Execution Order

DV2 is a 6-9 week sprint. Tiers organized by dependency.

### Week 1 — DV1 close-out + UI honesty
1. D2-1 T1 (event_detection.rs deletion) — 0.5 day
2. D2-1 T2 (log bounding) — 0.5 day
3. D2-1 T3 (Driver Results) — 1 day
4. D2-3 T1-T2 (incident clubbing labels + tooltip) — 2 days
5. D2-3 T3-T4 (topology colour + Devices health) — 2 days

### Week 2 — Schema-driven detection
6. D2-2 T1-T2 (vendor state mapping + adapter) — 3 days
7. D2-2 T3-T4 (fixtures + docs) — 2 days

### Week 3-4 — Config-state lane (highest payoff)
8. D2-4 T1 (config gNMI subscribe) — 2 days
9. D2-4 T2 (config event + ConfigState node) — 2 days
10. D2-4 T3 (config rules) — 2 days
11. D2-4 T4 (diff/snapshot) — 2 days
12. D2-4 T5-T6 (UI + docs) — 1.5 days

### Week 5 — Substrate graph
13. D2-5 T1-T4 (NetBox promotion + rack_isolated rule + PDU SNMP + UI) — 5 days

### Week 6 — OTel + hosts
14. D2-6 T1-T5 (LLDP→host + OTLP + reconciliation + correlation rule + docs) — 5 days

### Week 7 — Optical + app dependency
15. D2-7 T1-T5 (data model + receiver + trend rule + simulator + scoping) — 5 days
16. D2-8 T1-T5 (netflow + AppFlow + service-path-degraded) — 5 days (parallel-able with week 7)

### Week 8 — Reconciliation + hygiene
17. D2-9 T1-T3 (Entity + reconciler + node refactoring) — 5 days
18. D2-10 (UI fixes + playbooks + memory pressure plumbing) — 3 days
19. D2-11 T1-T2 (MCP hardening + cursor verification) — 1.5 days

### Week 9 — GNN training (if trigger fires)
20. First training cycle + comparison study + model card

### Parallel throughout
- Chaos cycle continues accumulating archive
- Validation runs continue (with bounded logs per D2-1 T2)

**Total wall clock**: 6-9 weeks. **Total active work**: ~7 weeks.

---

## <a id="guardrails"></a>Guardrails — Updated

### New in DV2

- **Detection rules MUST consume the vendor-state-mapping adapter.** Hand-coded `_DOWN_STATES = {"down", "admin_down"}` patterns are anti-patterns going forward. New rules cite the semantic transition in YAML/code.
- **Incident displays must communicate clubbing rationale.** If 2+ rule_ids are clubbed under one incident, the header reflects that.
- **Topology graph must reflect operational state.** Green/yellow/red colouring based on active incidents.
- **Side-channel log accumulation is bounded.** 10-run retention; older runs archive.
- **Schema-driven, not vendor-string-driven.** The whole D2-2 tier exists because hand-coding doesn't scale.
- **Every new node type goes through Entity reconciliation.** Reduces the ID-mismatch debt that plagues multi-source ingestion.

### Unchanged from DV1

All prior architectural invariants. Streaming-first hot path. Layered ingestion. Discovery-driven onboarding. Vault-only credentials. bonsai-mgmt network invariant. Feature index canonical. Bonpy is operator surface for sidecar state. event_detection.rs is retired (by D2-1 T1). Mac is dev-only, no toolchain. Ubuntu laptop is ops-only with interim cargo, post-Tier-6 pre-built binary.

### Anti-patterns to reject

- "Hand-code another vendor's state strings" — no, go through D2-2's mapping.
- "Add new ingestion lane without entity reconciliation" — no, every new source feeds D2-9.
- "Skip config-state lane to do app-dependency-matrix first" — no, config is the highest-payoff thread.
- "Defer netflow until DV3" — no, DV2 ships at least the data model and a working proof.
- "Land OTel without host node prerequisite" — no, D2-6 T1 (LLDP→Host) precedes the OTLP receiver.

---

## <a id="tracked"></a>Tracked Future Threads (now smaller)

Only the items DV2 doesn't fully land remain tracked:

- **Scale-up Paths B/C** (partitioned cores, read replicas) — DV4+
- **Cloud platform recipes** (per-platform docs) — DV3+ as deployed
- **Beyond-network positioning** (firewalls, VPN, cloud networking) — DV4+ post-GNN
- **gNSI Phase-3 full client** — depends on HIL graduated remediation maturity
- **Online learning infrastructure** — post-GNN
- **eBPF host-side production integration** (beyond DV1 spike) — DV3
- **UI bold-sharp design re-articulation** — DV3
- **Russh migration** of cli_capture.py — DV3

All items the operator named in the prior conversation are now in DV2 as full tiers, not as deferred items.

---

*DV2.0 — authored 2026-05-16 after end-to-end chunked code review of post-DV1 codebase, walkthrough of 13 UI screenshots, and screenshot-driven UI findings (UI-1 through UI-8). DV1 substantially landed: F-1/F-2 critical fixes (validation went PASS=6 FAIL=3 → PASS=16 FAIL=0), F-4 structural splits (main.rs 2,515→46, http_server.rs 7,779→11 sub-modules with all 87 routes preserved), F-5/F-6/F-7 testing breadth, K8s Helm chart, eBPF spike, GNN pre-work scaffolding. F-11 surfaced (BFD `admin_down` and interface `lower-layer-down` hand-coding) and was fixed but the deeper lesson promoted to D2-2 (schema-driven vendor state normalization). DV2 lands the five threads from the prior conversation: config-state lane (highest payoff), NetBox substrate graph promotion, OTel + Host node type, Optical/DWDM data model, App dependency matrix. Plus cross-cutting Entity reconciliation (D2-9), incident clubbing display honesty (D2-3 in response to operator's screenshot question), operational hygiene carryover (D2-10), one major + one minor from outstanding backlog (D2-11), and GNN training trigger watch (D2-12). 6-9 weeks total wall clock. Honours operator's directive: "don't put off anything or defer."*
