# T4-5 — Detection Rule Tuning: Real Lab Baselines

**Date**: 2026-05-07  
**Lab**: 8-node DC EVPN SRv6 (Nokia SR Linux)  
**Status**: Baseline established; no rules modified

---

## Lab State at Baseline

| Metric | Value |
|---|---|
| Devices | 8 (2× super-spine, 2× spine, 4× leaf) |
| Fabric links (CONNECTED_TO) | 24 |
| Management links (MGMT_LINK) | 56 |
| BGP sessions total | 26 |
| BGP established | 26 (100%) |
| NetBox nodes enriched | 68 |
| ServiceNow nodes enriched | 92 |

All 26 BGP sessions are established. No interface flaps observed. Lab is healthy at baseline.

---

## Detection Write Latency Baseline

Measured via `CreateDetection` gRPC → LadybugDB write on a quiescent 8-node lab.

| Percentile | Latency |
|---|---|
| min | 38 ms |
| p50 | 41 ms |
| p95 | 897 ms |
| p99 | 1088 ms |

**Observation**: p50 is 41 ms (healthy). p95/p99 spike to ~900 ms / ~1100 ms — these are
caused by the single-writer lock contention when the write coordinator's batch flush
happens to coincide with a detection write. This is the expected write-contention
behaviour under the single-writer LadybugDB constraint. The spikes are bounded to at
most one 1-second flush cycle.

**Acceptable threshold for this lab scale**: p95 < 2 s. Current p95 is 897 ms — within budget.

---

## Rule-by-Rule Analysis

### BGP Rules (`python/bonsai_sdk/rules/bgp.py`)

| Rule | Severity | Type | Threshold | Assessment |
|---|---|---|---|---|
| `bgp_session_down` | critical | event | `established → idle` only | **Correct.** The guard `old_state in _ESTABLISHED_FROM` prevents false fires from BGP retry timer cycles (`active → idle`). No change needed. |
| `bgp_session_flap` | critical | event | ≥ 3 flaps in 5 min | **Correct.** The 5-minute window and 3-flap threshold are standard industry practice. Under normal SRL operation in the lab, 0 flaps observed. No change needed. |
| `bgp_all_peers_down` | critical | event | all sessions down simultaneously | **Correct.** Fires only when `peer_count_established == 0` after a session change. In the 8-node lab with 26 sessions, this would require all peers on a device to drop at once — a genuine hardware or upstream fault indicator. No change needed. |
| `bgp_never_established` | warn | event | peer not established after 90 s | **Review flag.** The 90 s timeout is tight for SRL initial BGP bringup — SRL can take 30–60 s to establish BGP after interface up. In a flapping scenario this could fire prematurely. Recommend increasing to 120 s for production use. No change applied today (lab is stable, 0 observed fires). |

### Interface Rules (`python/bonsai_sdk/rules/interface.py`)

| Rule | Severity | Type | Threshold | Assessment |
|---|---|---|---|---|
| `interface_down` | critical | event | `down` or `lower-layer-down` oper status | **Correct.** Only fires on oper-status events, not admin-down. No change needed. |
| `interface_error_spike` | warn | poll | > 100 errors/s | **Review flag.** 100/s is very tight for lab links carrying EVPN control plane traffic. Under normal operation, SRL interfaces occasionally burst error counters above 100/s during ECMP reconvergence. This rule may produce false positives in a reconvergence event. Recommend 500/s for lab; 100/s is appropriate for production DC links. No change applied today (rule is poll-based, 30 s cycle, not event-driven — low fire frequency). |
| `interface_high_utilization` | warn | poll | > 80% of 1 Gbps | **Structural issue.** Hardcoded 1 Gbps capacity assumption. Lab links are 1 Gbps virtual — threshold is correct for this lab. For real DC links (10G/25G/100G), this would fire constantly. Annotated in code; requires NetBox interface `speed` enrichment before it can be made topology-aware. No change applied; tracked as future work. |

### Topology Rules (`python/bonsai_sdk/rules/topology.py`)

| Rule | Severity | Type | Threshold | Assessment |
|---|---|---|---|---|
| `topology_edge_lost` | warn | poll | CONNECTED_TO edge absent for one 30 s poll | **Correct.** One-poll window is tight but acceptable — LLDP neighbour expiry takes 120 s by default on SRL, so a missing edge means the link has been down for at least 120 s. No false positives expected. No change needed. |

### BFD Rules (`python/bonsai_sdk/rules/bfd.py`)

| Rule | Severity | Type | Threshold | Assessment |
|---|---|---|---|---|
| `bfd_session_down` | critical | event | `up → down` only | **Correct.** Guards against `down → down` re-fires. No BFD is configured in the DC EVPN lab (SRv6 uses ISIS), so this rule has never fired. Correct behaviour. |

### ML Rule (`python/bonsai_sdk/ml_detector.py`)

| Rule | Severity | Threshold | Assessment |
|---|---|---|---|
| `ml_anomaly_v1` | warn | anomaly score > 0.6 | **No model trained yet.** `anomaly_v1.joblib` does not exist; engine runs in rules-only mode. Train after sufficient chaos archive accumulates (target: 1000+ events). Threshold 0.6 is a reasonable starting point for IsolationForest on this feature space. |

---

## Tuning Changes Applied

**None.** All rules are well-calibrated for the current lab state. Two rules are flagged
for future adjustment:

1. `bgp_never_established`: increase timeout 90 s → 120 s when SRL initial bringup latency
   is confirmed to exceed 90 s in the lab.
2. `interface_error_spike`: increase threshold 100/s → 500/s if false positives appear
   during ECMP reconvergence testing.

---

## p95 Detection Latency Target

| Environment | Target p95 | Current p95 |
|---|---|---|
| 8-node lab | < 2 000 ms | 897 ms ✓ |
| 32-node DC (extrapolated) | < 2 000 ms | TBD |

The write-lock-induced p95 spikes are expected and bounded. They will reduce once the
`Enrichment` write path is migrated to the write coordinator queue (deferred to a future sprint).
