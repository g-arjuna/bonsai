# BONSAI — Backlog Bravo Series, v6 (Bv6.0)

> **A strategic recalibration**, not a continuation. Authored 2026-05-09 after end-to-end review of the Bv5 sprint landings plus operator's strategic question: how do we go from "graph-native L2/L3 fabric anomaly engine" to "genuinely useful open-source GNN application that competes in spirit with the netai.ai pitch"?
>
> The Bv5 work was excellent execution. Chaos infrastructure running on laptop AND OCI cloud. ML evaluation harnesses (rule baseline, tabular ML) and GNN data loader skeleton landed in code. Distributed mode validated end-to-end. ~1,100 new Python lines, ~600 new test lines. The northstar plumbing is in place.
>
> But Bv5 also surfaced an honest realisation: **what we have is narrower than what the field calls "graph-native AIOps."** Bonsai today detects 9 rule types, all gNMI-state-based, all on Layer 2-3 fabric protocol state. Real networks fail in ways our chaos cycle never tests: power, optical, wireless, configuration drift, service-provider WAN issues, certificate expiry, time-domain traffic effects. We won't build all of those, but we should be honest about which we can build, which we can simulate, and which we ingest only when deployed against real hardware.
>
> **Bv6 is the strategic plan to widen bonsai's signal surface enough that it becomes genuinely useful, while preserving the GNN northstar that's already on track.** Three things change vs prior backlogs:
>
> 1. **Signals tier becomes Tier 2** (was deferred for years across v9-Bv5). Syslog, SNMP traps, and config-drift detection are the pragmatic non-gNMI signals that ContainerLab can simulate well enough to train against.
> 2. **Output adapter validation becomes Tier 3** (only Prometheus has been tested end-to-end; Splunk, Elastic, ServiceNow EM have code but zero verification).
> 3. **ServiceNow AIOps integration becomes Tier 4** — not just event push (which the EM adapter does today) but real auto-correlation, auto-clearing, and bidirectional incident sync. This is what makes bonsai a credible AIOps feeder, which is the deployable use case.
>
> What stays unchanged: the GNN training timeline (Tier 5). Data continues to accumulate from Bv5's chaos runner. By the time Tier 1-4 mature, archive depth is sufficient for training. **No new sprint is added before the GNN; the GNN sprint runs in parallel as data accumulates.**

---

## Table of Contents

1. [Audience and Positioning](#positioning)
2. [Bv5 Sprint Outcome — Verified Landing](#progress)
3. [Implementation Progress Snapshot](#progress-snapshot)
4. [The Real-World Gap — What Bonsai Doesn't Detect](#gap)
5. [Strategic Framework — What We Build, Simulate, or Ingest-Only](#framework)
6. [TIER 1 — Engineering Hygiene From Bv5 Surface](#tier-1)
7. [TIER 2 — Signals Tier (syslog + SNMP traps + config drift)](#tier-2) ⚡ THE WIDENING ⚡
8. [TIER 3 — Output Adapter Validation](#tier-3)
9. [TIER 4 — ServiceNow AIOps Integration](#tier-4)
10. [TIER 5 — GNN Training (parallel to Tier 2-4)](#tier-5)
11. [TIER 6 — Real-Hardware-Only Signals (schema + ingestion path, no simulation)](#tier-6)
12. [Carryover from Bv5](#carryover)
13. [Execution Order](#execution-order)
14. [Honest Northstar Assessment](#honest-northstar)
15. [Guardrails](#guardrails)

---

## <a id="positioning"></a>Audience and Positioning

**Sharpened, not changed.** Bonsai is positioned as:

> **An open-source graph-native AIOps feeder for controller-less network environments. It correlates multi-layer signal (gNMI state + syslog + SNMP traps + config drift + topology) into impact-aware incidents, ranks them by graph-derived blast radius, and pushes them to AIOps platforms (ServiceNow ITOM as flagship). A GNN trained on accumulated chaos archive provides anomaly scores that catch what rule-based detectors miss.**

The contrast with netai.ai sharpens: they sell a closed-source vertical product. Bonsai sells an open-source horizontal feeder that integrates into the AIOps platform of the operator's choice. **Different value proposition. Compatible coexistence.**

**Anti-positioning sharpened**: bonsai is not a replacement for ServiceNow ITOM, Splunk ITSI, Datadog NPM, or Cisco Catalyst Center. Bonsai feeds these. Bonsai is also not a config management system; it detects drift, it does not enforce.

---

## <a id="progress"></a>Bv5 Sprint Outcome — Verified Landing

End-to-end code review confirms Bv5 work landed substantially.

| Bv5 item | Status | Evidence |
|---|---|---|
| T1-A operational baseline + chaos start | ✅ Done | `docs/test_results/daily_runs/2026-05-08.md` shows lab healthy, archive at 919,904 rows, RSS stable at 1.27 GB, 48 Parquet files, 6/6 verification checks pass |
| T1-A-3 daily verification cron | ✅ Done | `scripts/bv5_daily_check.sh` produces structured daily reports |
| T1-B cloud spike | ✅ Done (per operator confirmation) | OCI Always Free running; archive accumulating |
| T2-1 GNN data loader skeleton | ✅ Done | `python/bonsai_ml/gnn/data_loader.py` (283 lines) — `BonsaiGraphData` dataclass with validation, snapshot reader contract, synthetic test fixtures (55 lines), unit tests (83 lines) |
| T2-2 rule-baseline eval harness | ✅ Done | `python/bonsai_ml/eval/rule_baseline.py` (367 lines) — TPR/FPR per rule, time-windowed matching with grace, data-source agnostic |
| T2-3 tabular ML eval harness | ✅ Done | `python/bonsai_ml/eval/tabular_ml.py` (255 lines) |
| T2-5 distributed mode validation | ✅ Done | `docs/test_results/e2e_compose/20260509-pass.md` — 2 collectors, 4 targets each, 8 nodes, 24 CONNECTED_TO edges; stack left running |
| Prometheus adapter e2e test | ✅ Done (Bv5 carryover) | `docs/test_results/e2e_output_adapters/20260503-prometheus-pass.md` |

### Not yet started

- ❌ T2-4 investigation agent productive use (still gated on token budget)
- ❌ T2-6 UI completion items (operator path overrides, subscription resolution audit)
- ❌ T2-7 documentation refresh
- ❌ T2-8 SP lab bring-up (correctly deferred — DC archive must complete first)
- ❌ Splunk / Elastic / ServiceNow EM adapter e2e tests (not in Bv5 scope, surfaced now in Tier 3)

### What real operation revealed

The 2026-05-08 daily report tells an interesting story:
- Archive is growing (919K rows at 2.6 MB compressed) — telemetry flows
- **chaos_runner daemon is NOT RUNNING** at the time of check (`no pid file`)
- Lab is healthy with one warning: `DC leaf1: no EVPN routes in mac-vrf-a`

This means the archive contains **919K rows of non-fault baseline data** — useful for understanding background noise, less useful for training a fault detector. The chaos cycle hadn't kicked off yet, or had stopped. **Operationally, the discipline of "chaos runner stays running" needs reinforcement.** Tier 1 below has a small fix for this.

---

## <a id="progress-snapshot"></a>Implementation Progress Snapshot

This section records the first Bv6 implementation batch so the backlog clearly
distinguishes touched work from untouched work.

### Landed or substantially advanced in code

- `T1-1` chaos runner auto-start/auto-restart:
  `scripts/chaos_runner.sh` now has `--ensure-running`, writes
  `restart_marker` entries, and `scripts/bv5_daily_check.sh` reports chaos
  cycle freshness and stale-injection age.
- `T1-2` BFD chaos coverage:
  `chaos_plans/always_on_dc.yaml`, `python/inject_fault.py`,
  `scripts/chaos_runner.py`, and tests now include `bfd_session_down`
  injection/heal paths for SR Linux subinterfaces.
- `T1-3` EVPN warning investigation:
  `scripts/check_lab.sh` now uses the SR Linux v26.x EVPN received-routes view
  instead of the older `mac-vrf-a protocols bgp-evpn routes` command path.
- `T1-4` threshold documentation:
  `docs/operational_health_thresholds.md` now captures first-pass warning and
  critical thresholds for write path, event bus, archive lag, and chaos
  freshness.
- `T1-5` GNN feature-space audit:
  `python/bonsai_ml/gnn/data_loader.py` now covers `vendor_frr` plus SP roles
  (`pe`, `p`, `rr`, `ce`) with accompanying unit coverage.
- `T2-1` syslog ingestion:
  `src/signals/syslog.rs` plus graph/event wiring landed, and
  `python/bonsai_sdk/rules/syslog.py` now includes the core five backlog rules
  plus explicit `BPDUGuard` and spanning-tree topology-change detection.
- `T2-2` SNMP trap ingestion:
  `src/signals/snmp.rs` now parses standard trap envelopes into structured
  events and `python/bonsai_sdk/rules/snmp.py` adds initial detector coverage
  for startup, auth burst, environmental, and FRU signals.

### Touched but not closed

- `T2-1` is not fully closed yet:
  ingestion and rule plumbing are present, but ContainerLab syslog simulation,
  chaos/e2e validation, and baseline metrics still need explicit proof.
- `T2-2` is not fully closed yet:
  the receiver now emits structured trap events, but richer MIB-aware decoding,
  broader v3 coverage, and trap-simulation/e2e validation still remain.

### Not touched in this batch

- `T2-3` configuration drift detection
- `T2-4` Layer 2-3 gNMI rules expansion beyond the BFD-coverage gap
- `T2-5` auto-correlation across signals
- All Tier 3 output-adapter e2e validation work
- All Tier 4 ServiceNow bidirectional incident-sync work
- Tier 6 real-hardware-only signal schema work

---

## <a id="gap"></a>The Real-World Gap — What Bonsai Doesn't Detect

This is what the operator's question surfaces. Honest inventory of the rule set today:

### What we have (9 rules, all gNMI-state)

- BGP: session down, session flap, all peers down, never established
- BFD: session down (zero chaos coverage — see Tier 1 fix)
- Interface: down, error spike, high utilization
- Topology: edge lost

### What real networks fail with that we don't detect

**Layer 1 / hardware** (cannot be simulated in ContainerLab):
- Power supply failures, PDU outages, dual-feed loss
- Environmental: temperature, humidity, fan failure
- Optical: lambda failure, OSNR degradation, BER spike, fiber cut
- Hardware FRU failures (line card, fabric module)

**Layer 2-3 protocol state we COULD detect but don't** (gNMI-feasible):
- LLDP neighbor mismatch (cabling error)
- IS-IS adjacency flap, LSP TLV errors
- BFD asymmetric paths
- LACP individual link failure
- VRRP/HSRP master election thrashing
- LDP session issues (SP-relevant)
- RSVP-TE LSP path errors (SP-relevant)
- SR-MPLS policy degradation

**Configuration anomalies** (detectable from gNMI Get + diff):
- Configuration drift (running-config differs from intended-config)
- Unauthorised changes (timestamp anomalies, user anomalies)
- Configuration syntax errors that loaded silently
- Missing AAA configuration
- Disabled critical features (BPDUGuard off on access port)

**Syslog-derived signals** (requires syslog tier):
- Authentication failures (SSH brute force, AAA failures)
- Spanning-tree topology changes
- Hardware error messages
- Software crashes (kernel panic, process restart)
- License expiry warnings

**SNMP-trap-derived signals** (requires trap tier):
- Cold/warm start, link up/down (lower fidelity than gNMI but real-deployment-relevant)
- Authentication failures
- Vendor-specific environmental traps

**Service-provider specific** (requires SP lab + SP rules):
- Customer SLA breach detection (latency, packet loss, jitter cross-PE)
- BGP route advertisement anomalies (more-specific hijack patterns)
- LSP path optimisation events
- VPN service touchpoint issues

**Wireless** (requires wireless data source — typically controller telemetry):
- AP signal strength variance, client roam events
- Co-channel interference
- DFS radar events
- 802.1x auth failures, EAP timeouts

**Time-domain / traffic effects** (requires telemetry at flow scale):
- Microburst detection
- Application-flow anomalies
- ECMP imbalance
- Buffer occupancy spikes
- Queue depth anomalies

### The honest read

That's roughly **40 signal classes** real networks produce. Bonsai detects 9 of them. Even if we doubled coverage to 18, we'd still be at half. **We will never reach 40.** What we can do:

- **Detect what gNMI gives us better** (~12 more rules)
- **Add syslog + SNMP traps** (~8 signal classes)
- **Add config drift** (~4 signal classes)
- **Add SP-specific gNMI rules** (~6 signal classes when SP lab is up)
- **Document the schema for hardware/wireless/optical signals** so operators deploying against real hardware can extend bonsai with custom enrichers/parsers

Realistic Bv6 + Bv7 endpoint: ~25-30 signal classes covered. **Enough to be genuinely useful in real-world DC + SP + small enterprise environments.** Wireless and hardware-FRU specifics remain operator-extension territory.

---

## <a id="framework"></a>Strategic Framework — What We Build, Simulate, or Ingest-Only

For each signal class identified above, decide one of three positions:

**(A) Build + simulate**: rule code in `python/bonsai_sdk/rules/`, chaos catalogue entry, ContainerLab-injectable. Trains the GNN. Proves out in CI. *Examples*: BGP, BFD, LLDP, syslog patterns, config drift, IS-IS, LDP, RSVP-TE, LACP, VRRP.

**(B) Build + accept-as-data-only**: rule code lands, schema lands, ingestion path lands. Cannot be simulated in lab. Documented as "feed real production data, do not expect chaos coverage." Train the GNN on (A) signals, run inference on (A)+(B) signals at deploy time. *Examples*: power/PSU, optical, wireless, hardware FRU, microbursts.

**(C) Schema-only**: data model exists in graph, ingestion endpoints exist, no detection rules ship. Operators add their own. *Examples*: vendor-specific environmental traps, custom flow analytics, business-application metrics.

This three-tier framework shapes Tier 2-6 below.

---

## <a id="tier-1"></a>TIER 1 — Engineering Hygiene From Bv5 Surface

The Bv5 daily report surfaced small operational issues. Land these in the next 1-2 weeks; they don't block Tier 2-5.

### T1-1 (Bv6) — Chaos runner auto-start + auto-restart

**What**: the daily report at `docs/test_results/daily_runs/2026-05-08.md` shows `chaos_runner daemon is NOT RUNNING (no pid file)`. The operator started it, then for some reason it stopped. **The data accumulating right now is mostly non-fault baseline. That's not what we want for GNN training.**

**Fix**:
- `scripts/chaos_runner.sh` gains a `--ensure-running` flag that's idempotent: if the daemon is up, do nothing; if down, start
- Add `*/30 * * * * cd ~/bonsai && bash scripts/chaos_runner.sh --ensure-running` to crontab
- The daily check script flags clearly when chaos has not run for >2 hours
- Restart events go to `runtime/chaos_log.jsonl` as `restart_marker` records (already specified in Bv5)

**Done when**: chaos runner survives operator's machine-sleep / reboot cycles; daily report includes "chaos cycles in last 24h: N"; cycles per day stays consistent.

### T1-2 (Bv6) — Add BFD chaos coverage (currently zero)

**What**: `chaos_plans/always_on_dc.yaml` has 18 fault entries: 6 netem_loss, 6 interface_shut, 6 bgp_session_down. **Zero BFD faults.** The detection rule `BfdSessionDown` exists but has no chaos coverage. **Cannot evaluate that rule's TPR/FPR.**

**Fix**: extend the chaos plan with 4-6 BFD-specific faults:
- BFD session timeout (kill BFD process on one side)
- BFD interval mismatch (modify config to break)
- BFD asymmetric (one side detect, other doesn't)
- BFD session never establishes (configure with wrong discriminator)

**Done when**: chaos catalogue exercises every active detection rule at least 30 times per week.

### T1-3 (Bv6) — Resolve EVPN routes warning on leaf1

**What**: lab health check reports `DC leaf1: no EVPN routes in mac-vrf-a`. This isn't a bonsai bug, but it's a lab-config issue that:
- Suppresses any EVPN-route-driven detection rules
- Pollutes any detection that depends on EVPN topology being complete
- Generates a constant warning in every daily check

**Fix**: investigate leaf1 EVPN configuration, add missing route advertisement, verify against the other leaves (which presumably do advertise).

**Done when**: lab health returns clean across all 8 nodes.

### T1-4 (Bv6) — Memory & write-coordinator headroom check

**What**: the daily snapshot shows `write_coordinator_queue_pct: 0` and `event_bus_depth: 3` — **green**, which is the target. Document the operational thresholds: when does intervention become necessary? Add to `docs/operational_health_thresholds.md`:
- write_coordinator_queue_pct > 50% sustained for 5min → investigate write contention
- event_bus_depth > 50% sustained for 5min → investigate slow subscriber
- archive_lag_millis > 30000 → investigate archive disk pressure

**Done when**: thresholds documented; daily check script colour-codes the snapshot per threshold; alerts fire when crossed.

### T1-5 (Bv6) — GNN data loader feature space audit

**What**: `python/bonsai_ml/gnn/data_loader.py:17-53` defines the feature space. Vendor features are nokia/cisco/juniper/arista; role features are super_spine/spine/leaf. **For SP topology, every PE/P/RR/CE device gets `role_other` — model can't learn role-specific patterns.** Same for vendors outside the four-DC set.

**Fix**: generalise feature space:
- Vendor: keep current four + add `vendor_frr`, `vendor_other`
- Role: add `role_pe`, `role_p`, `role_rr`, `role_ce` for SP; `role_access`, `role_distribution`, `role_core`, `role_edge` for campus
- Embedding dimensions: keep 4 for now; raise to 8-16 once Path A embeddings settle on a dimension that's empirically useful

**Done when**: feature space supports DC + SP + campus topologies without `role_other` becoming the dominant class.

---

## <a id="tier-2"></a>TIER 2 — Signals Tier (syslog + SNMP traps + config drift) ⚡ THE WIDENING ⚡

This is the work that takes bonsai from "graph-native L2/L3 fabric anomaly engine" to "graph-native AIOps feeder." Three signal sources, each with build+simulate position (per the framework above).

### T2-1 (Bv6) — Syslog ingestion daemon

**What**: a syslog receiver (RFC 5424 over UDP/514 and TCP/6514) that:
- Accepts syslog from devices in the lab + real deployments
- Parses structured fields (severity, facility, hostname, msg)
- Classifies messages into bonsai categories (auth, hw, software, protocol, license, custom)
- Emits SyslogEvent updates onto the bus (read by graph writer + output adapters)
- Persists raw + parsed in archive (separate Parquet schema from gNMI telemetry)

**Where**:
- New crate or `src/signals/syslog.rs`
- New graph node type `SyslogEvent` with edges to Device
- New archive partition for syslog
- ContainerLab simulation: `logger` command on each device or vendor-specific equivalent

**Detection rules** (new family in `python/bonsai_sdk/rules/syslog.py`):
- Authentication failure cluster (>N failed auths in window)
- Hardware error message (vendor-pattern matched)
- Software crash (process restart, kernel panic)
- License expiry warning
- BPDUGuard activation
- Spanning-tree topology change

**Done when**: 5+ syslog detection rules ship with chaos coverage; rules fire on simulated syslog from ContainerLab; rule baseline harness produces metrics.

### T2-2 (Bv6) — SNMP trap ingestion daemon

**What**: an SNMP trap receiver (UDP/162) that:
- Accepts v2c and v3 traps
- Parses with MIB awareness (bundle a curated set of common MIBs)
- Emits SnmpTrap updates onto the bus
- Same archive pattern as syslog

**Where**:
- `src/signals/snmp.rs` or separate crate
- New graph node type `SnmpTrap`
- Bundled MIBs: SNMPv2-MIB, IF-MIB, ENTITY-MIB, plus vendor essentials (CISCO-ENVMON, JUNIPER-CHASSIS)

**Detection rules** (`python/bonsai_sdk/rules/snmp.py`):
- Cold/warm start
- Auth failure burst
- Environmental threshold breach (PSU, temperature)
- Vendor-specific FRU failures

**Done when**: 4+ SNMP-trap-derived detection rules ship; ContainerLab can synthesise traps via `snmptrap` command; chaos coverage exists.

### T2-3 (Bv6) — Configuration drift detection

**What**: periodic snapshot of running-config from each device (gNMI Get of full config), diff against a baseline (intended-config or last-snapshot), produce `ConfigDriftEvent` when changes are detected.

**Three modes**:
1. **Baseline drift**: operator commits an "intended-config" to bonsai; daily Get + diff surfaces deviations
2. **Snapshot drift**: no baseline; surface any change since last snapshot (for environments without intended-config discipline)
3. **Authorised-change drift**: integrate with operator's change management (operator's RFC ID provided; drift outside RFC windows surfaces as anomaly)

**Where**:
- `src/signals/config_drift.rs`
- New graph node type `ConfigDriftEvent`
- New `intended-configs/` directory for baseline storage; encrypted at rest
- Rule: `ConfigDrifted` (severity = high if outside RFC window; medium otherwise)

**Done when**: drift detection runs nightly; manual `vtysh` change to lab device surfaces in `/api/incidents` within 24h.

### T2-4 (Bv6) — Layer 2-3 gNMI rules expansion

**What**: add the 6 most operationally-relevant rules we don't have yet:

- **LldpNeighborMismatch**: cabling errors (expected vs actual neighbor)
- **IsisAdjacencyFlap**: IS-IS instability
- **LacpIndividualLink**: LAG member failure
- **VrrpMasterFlap**: VRRP/HSRP election thrashing (if HA pairs present)
- **LdpSessionDown**: SP-relevant; runs when SP lab up
- **RsvpTeLspPathError**: SP-relevant

**Where**: `python/bonsai_sdk/rules/{lldp,isis,lacp,vrrp,ldp,rsvp_te}.py`. Each gets a chaos catalogue entry. Each gets the corresponding write_* helper in `src/graph/mod.rs` if missing.

**Done when**: all 6 rules have ≥30 chaos examples accumulated and rule-baseline metrics computed.

### T2-5 (Bv6) — Auto-correlation across signals

**What**: when detection events fire across signal sources within a time + topology window, group them into a single Incident automatically. Today's `group_into_incidents` in `http_server.rs` works on detections only; extend it to:
- Cross-signal: `BgpSessionDown` on a device + `LinkDown` syslog on the same device's interface = same incident
- Cross-device: detections on connected devices within window = same incident, with graph-blast-radius rationale
- Auto-clear: when all member detections clear, the incident clears

**Where**: extend `src/graph/queries.rs::group_into_incidents`. Add `IncidentCorrelation` with edge to each contributing detection.

**Done when**: a manually-induced multi-signal fault produces one incident with multiple correlated detections; healing the fault auto-clears the incident.

---

## <a id="tier-3"></a>TIER 3 — Output Adapter Validation

The user identified this gap: only Prometheus has been tested end-to-end. Splunk, Elastic, ServiceNow EM have code but zero verification.

### T3-1 (Bv6) — Splunk HEC adapter e2e test

**What**: run `scripts/e2e_output_adapter_test.sh splunk`:
- Bring up Splunk container via `compose-external.yml --profile splunk`
- Configure adapter in bonsai
- Run lab + chaos for 1 hour
- Verify events visible in Splunk: count, severity distribution, payload structure

**Done when**: `docs/test_results/e2e_output_adapters/<date>-splunk-pass.md` written with screenshots/queries.

### T3-2 (Bv6) — Elastic adapter e2e test

Same shape as T3-1 but for Elastic. Verify ECS-compatibility of payload structure (Elastic Common Schema).

### T3-3 (Bv6) — ServiceNow EM adapter e2e test against PDI

**What**: against operator's ServiceNow PDI:
- Configure EM adapter
- Run lab + chaos for 1 hour
- Verify em_event records created with correct severity mapping
- Verify deduplication (same fault doesn't duplicate-create)
- Verify auto-clear when fault heals (if EM adapter supports it)

**Done when**: `docs/test_results/e2e_output_adapters/<date>-servicenow-em-pass.md` written. **This is also the on-ramp for Tier 4.**

### T3-4 (Bv6) — Output adapter health monitoring

**What**: each adapter emits health metrics (`bonsai_output_adapter_publish_total{adapter,status}`, `bonsai_output_adapter_lag_seconds{adapter}`); Operations workspace surfaces per-adapter status; auto-degrades to local-archive-only if adapter fails for >5 minutes.

**Done when**: stopping the Splunk container during a run produces visible degraded state in UI; adapter recovers when Splunk returns.

---

## <a id="tier-4"></a>TIER 4 — ServiceNow AIOps Integration

The user explicitly asked about this. ServiceNow is the operator's chosen AIOps platform; bonsai's pitch as "feeder" is hollow until this is real.

Today bonsai has:
- ServiceNow CMDB enricher (read CIs from ServiceNow → enrich graph)
- ServiceNow EM adapter (push events out)

This is one-way ingest + one-way push. **Real AIOps integration is bidirectional and stateful.**

### T4-1 (Bv6) — Bidirectional incident sync

**What**: when bonsai produces a high-severity incident, post to ServiceNow Incident table (not just em_event). Subscribe to ServiceNow webhook for incident updates. Sync state both ways:
- Bonsai resolves incident → close ServiceNow incident
- ServiceNow incident closed by operator → mark bonsai incident as acknowledged
- ServiceNow incident assigned → reflect assignment in bonsai UI
- ServiceNow comments → visible in bonsai UI

**Where**:
- `src/output/servicenow_incident.rs` (separate from em_event)
- `src/api.rs` webhook receiver for ServiceNow
- New graph properties on Incident: `servicenow_sys_id`, `servicenow_state`, `servicenow_assignee`

**Done when**: a chaos-induced incident produces a ServiceNow incident; assigning it in ServiceNow UI updates bonsai UI; healing the chaos closes both.

### T4-2 (Bv6) — Auto-correlation feeds ServiceNow

**What**: leverage Tier 2-5 auto-correlation. Push **the correlated incident** (with all member detections, blast-radius, root-cause-rationale) as a single ServiceNow record. Not 10 separate events that an operator has to manually merge.

**This is the headline operational value**. The pitch becomes: "bonsai turns 30 raw events into 1 ServiceNow incident with full context."

**Done when**: a multi-signal fault produces 1 ServiceNow incident with structured `bonsai_correlation_summary` field that lists contributing detections, blast radius, and a topology snapshot link.

### T4-3 (Bv6) — Auto-clearing of correlated tickets

**What**: when the underlying chaos heals, bonsai's correlated incident clears. The ServiceNow record auto-resolves with a comment trail showing detection-by-detection what cleared and when.

**Done when**: chaos cycle inject + heal produces a ServiceNow incident that opens, accumulates detection comments, then auto-resolves with full trail.

### T4-4 (Bv6) — Root-cause hint via graph blast radius

**What**: on incident creation, populate ServiceNow's `cause_ci` field with the bonsai-derived likely root-cause CI (the device with highest blast-radius score among members of the correlated detections). Operators can override.

**Done when**: 80% of chaos-induced incidents have correctly identified the injected device as the root cause.

### T4-5 (Bv6) — ServiceNow ITSM playbook bridge

**What**: ServiceNow Workflow can call back to bonsai's `/api/remediations/propose` endpoint when an incident hits assignment. Bonsai returns a list of viable playbooks (from existing playbook library) with success-probability based on the trust model. Operator picks one in ServiceNow; bonsai executes and reports back.

**Where**: extend HIL workflow to accept ServiceNow as initiator (today only initiates from bonsai UI).

**Done when**: ServiceNow operator can trigger bonsai-driven remediation from within ServiceNow's UI.

---

## <a id="tier-5"></a>TIER 5 — GNN Training (parallel to Tier 2-4)

Unchanged from Bv5 Tier 3 except for the dependency call-out: the GNN trains on whatever signals exist in the archive when the trigger condition is met. **If Tier 2 (signals tier) lands before the trigger, GNN trains on multi-signal data — which is dramatically better than gNMI-only.**

Trigger condition (unchanged from Bv5):
- Archive depth ≥ 30 calendar days
- ≥ 500 chaos injections
- ≥ 50 examples per active detection rule
- Baselines stable for 7 consecutive days
- No crashes for 14 days
- Integrity verifies for 14 nights

### T5-1 — GNN training run (Bv5 carryover)

GraphSAGE/GAT 2-3 layers, train 25 days / validate 5 / test most recent. Multi-signal feature input if Tier 2 ready.

### T5-2 — Comparison study: rules vs tabular ML vs GNN

Use Bv5 harnesses. Confusion matrix.

### T5-3 — Online inference path

Graph snapshot every N seconds; GNN scores Devices; UI surfaces.

### T5-4 — Model card

Honest documentation. Include "what signals were in training data" — if Tier 2 incomplete at training time, model card calls out which signal classes are inference-only (no training data).

---

## <a id="tier-6"></a>TIER 6 — Real-Hardware-Only Signals (schema + ingestion path, no simulation)

These cannot be simulated meaningfully. Bonsai gains the schema and ingestion hooks; operators feed real production data when deploying against real hardware. **Documented as deploy-time-extensible, not validated in CI.**

### T6-1 — Power / environmental schema

Graph node type `EnvironmentalReading` with edges to Device. Properties: psu_status, fan_rpm, temperature_c, voltage_dc. Ingestion via gNMI for vendors that expose it (most do via OpenConfig); SNMP trap fallback for older gear.

### T6-2 — Optical layer schema

Graph node type `OpticalChannel` with edges to Interface. Properties: lambda, osnr_db, ber, tx_power_dbm, rx_power_dbm.

### T6-3 — Wireless schema

Graph node types `WirelessAp`, `WirelessClient`, `WirelessRadio`. Properties for signal strength, channel utilisation, client count, roam events. Ingestion typically via wireless-controller telemetry (Cisco WLC, Aruba, Mist).

### T6-4 — Hardware FRU schema

Graph node types `LineCard`, `FabricModule`, `PowerSupply`, `Fan`. Edges to chassis. Status properties.

**For each**: schema lands, write_* helpers land, but no detection rules ship and no chaos catalogue entries. Operators extend with custom rules. Deploy-time validation only.

---

## <a id="carryover"></a>Carryover from Bv5

Items remaining valid; deferred behind Tier 1-5:

- **Investigation agent productive use** (post-MVP, pending token budget) — Bv5 T2-4
- **HIL graduated remediation** in production (subsumed by Tier 4 T4-5)
- **Operator path overrides UI workspace** (Bv5 T2-6)
- **Subscription resolution audit** in DeviceDrawer (Bv5 T2-6)
- **mgmt-plane visibility toggle** verification (Bv3 T2-3)
- **Documentation refresh** (Bv5 T2-7) — standing lowest priority
- **SP lab bring-up** (Bv5 T2-8) — after DC archive completes
- **Catalogue plugin install command, AIOps readiness checklist, NL query, bulk CSV onboarding, scale architecture, S3 archive backend, campus topology, bitemporal schema, schema migration, Grafeo evaluation** — strategic carryover

Plus the Bv2 hardcoding catalogue (H-1 through H-12) — most addressed by Bv3-Bv4-Bv5 work; remainder opportunistic.

---

## <a id="execution-order"></a>Execution Order

Bv6 is the longest backlog of the Bravo series in scope. Sequencing matters.

### Sprint 1 (1-2 weeks) — Engineering hygiene
1. T1-1 chaos runner auto-restart (highest priority — fixes the silent-non-fault-archive issue)
2. T1-2 BFD chaos coverage
3. T1-3 leaf1 EVPN routes fix
4. T1-4 operational thresholds doc
5. T1-5 GNN data loader feature-space generalisation

### Sprint 2 (3-4 weeks) — Signals tier (the widening) ⚡
6. T2-1 syslog ingestion daemon + 5 rules
7. T2-2 SNMP trap ingestion daemon + 4 rules
8. T2-3 configuration drift detection
9. T2-4 layer 2-3 gNMI rule expansion (6 new rules)
10. T2-5 auto-correlation across signals

### Sprint 3 (1-2 weeks) — Output adapter validation
11. T3-1 Splunk HEC e2e test
12. T3-2 Elastic e2e test
13. T3-3 ServiceNow EM e2e test against PDI
14. T3-4 output adapter health monitoring

### Sprint 4 (2-3 weeks) — ServiceNow AIOps integration
15. T4-1 bidirectional incident sync
16. T4-2 auto-correlation feeds ServiceNow
17. T4-3 auto-clearing of correlated tickets
18. T4-4 root-cause hint via graph blast radius
19. T4-5 ServiceNow ITSM playbook bridge

### Sprint 5 (3-4 weeks, runs in parallel to Sprint 2-4) — GNN
20. T5-1 GNN training run (when trigger condition met)
21. T5-2 comparison study
22. T5-3 online inference
23. T5-4 model card

### Sprint 6 (1-2 weeks) — Real-hardware-only schemas
24. T6-1 power/environmental
25. T6-2 optical
26. T6-3 wireless
27. T6-4 hardware FRU

### Continuously running through all sprints
- Chaos cycle on DC lab (laptop)
- Cloud chaos cycle (OCI Always Free)
- Daily verification via cron

### Estimated total
**12-16 weeks** to a state where bonsai has:
- 25-30 active detection rules across multi-signal sources
- Validated end-to-end output adapters
- Real ServiceNow AIOps integration with auto-correlation and auto-clearing
- Trained Path B GNN with honest evaluation
- Schemas for hardware/wireless/optical signals (operator-extensible)

This is the genuinely-useful-open-source-GNN-application destination.

---

## <a id="honest-northstar"></a>Honest Northstar Assessment

The northstar from Bv2-mod was: "Path B GNN catches at least one cascading-failure class that rules + tabular ML miss, with documented confusion matrix on a held-out chaos test set."

That northstar is **technically sound but operationally narrow.** Achieving it produces a research artifact, not a useful product. The user is right to push for "genuinely useful open-source GNN application." That requires:

1. **Multi-signal training data** (not gNMI-only) — Tier 2
2. **Real AIOps integration** so the value lands in the operator's existing workflow — Tier 4
3. **Validated output paths** so events actually reach the operator's tools — Tier 3
4. **Operational discipline** so chaos data accumulates reliably — Tier 1 + Bv5 hygiene
5. **Honest documentation** about what's covered vs deploy-time-extensible — model card + Tier 6

The Bv2-mod northstar **stays valid** but sits within a larger product story. Achieving the technical northstar without Tier 2-4 produces a paper; achieving it with Tier 2-4 produces a tool.

Bv6 is the plan to land the tool, not just the paper.

---

## <a id="guardrails"></a>Guardrails

### New in Bv6

- **Build-or-simulate-or-ingest-only is an explicit choice for every signal class.** Tier 2 covers (A) build+simulate; Tier 6 covers (B) build+accept-as-data-only and (C) schema-only. Mixing categories silently is rejected.
- **Output adapter testing is non-optional.** Adapter code that hasn't been verified end-to-end against a real receiver is treated as alpha. Marked clearly in docs.
- **AIOps integration is bidirectional or it doesn't exist.** One-way push (today's EM adapter) is a strict subset; don't claim "AIOps integration" until Tier 4 is real.
- **GNN model card explicitly lists training-signal coverage.** Inference-time signals not present in training are flagged. Honest about generalisation boundaries.
- **Chaos plan covers every active detection rule.** Rules without chaos coverage are flagged in the daily report; we know we can't measure their TPR/FPR.

### Unchanged from v7-Bv5

All prior architectural invariants. Reference earlier backlogs.

### Anti-patterns to reject

- "We claim AIOps integration based on EM adapter alone" — no, that's one-way push; bidirectional is the bar
- "Train GNN on whatever data we have, document later" — no, signal coverage shapes model card before training
- "Add wireless chaos in ContainerLab" — no, wireless can't be meaningfully simulated; schema-only
- "Skip Tier 1 hygiene to get to GNN faster" — no, T1-1 + T1-2 directly affect data quality
- "Document all of Tier 6 in detail" — no, Tier 6 is schema + hooks, not detailed implementation
- All prior anti-patterns remain in force

---

## What Bv6 Explicitly Excludes

- Auth/RBAC, multi-tenancy, production HA, K8s
- Workspace split
- Auto-graduation of trust state
- Replacement positioning vs ServiceNow ITOM, Splunk ITSI, etc.
- Claim of feature parity with netai.ai (different product category)
- Wireless / hardware-FRU / optical chaos simulation
- Bidirectional integration with non-ServiceNow AIOps platforms (build pattern reusable but only ServiceNow lands in Bv6)

---

*Bv6.0 — authored 2026-05-09 after end-to-end review of Bv5 sprint landings (chaos infrastructure operational, ML eval harnesses + GNN data loader skeleton landed, distributed mode validated). Strategic recalibration toward "genuinely useful open-source GNN application." Three new structural tiers: signals tier (Tier 2 — syslog, SNMP traps, config drift, gNMI rule expansion, auto-correlation), output adapter validation (Tier 3 — Splunk, Elastic, ServiceNow EM e2e tests), ServiceNow AIOps integration (Tier 4 — bidirectional incident sync, auto-correlation feed, auto-clearing, root-cause hint, playbook bridge). Tier 5 GNN training continues as Bv5 specified, in parallel. Tier 6 covers real-hardware-only signals (power, optical, wireless, hardware FRU) as schema + ingestion path without simulation. Honest northstar assessment: Bv2-mod technical northstar stays valid but sits within a larger product story; Bv6 lands the tool, not just the paper. Estimated 12-16 weeks to genuinely-useful destination. References v7-Bv5 for unchanged context; Bv2-mod for original northstar definition.*
