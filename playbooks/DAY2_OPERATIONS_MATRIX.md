# Day-2 Operations Matrix

This document extends the current playbook library with a network-operations
view of the incidents a data center or service-provider engineer actually sees
day to day.

It is intentionally split into:

- what the engineer cares about operationally,
- what Bonsai can already observe or nearly observe,
- what is safe to automate later,
- and what should remain human-first even if a writable gNMI path exists.

This is guidance for future detection and playbook growth. It does **not**
change Phase 2 scope by itself.

## Operating principles

1. Prefer targeted, reversible actions over broad restarts.
2. Never treat "programmable" as equal to "safe to auto-remediate".
3. Only automate what the graph can verify after the action.
4. When the symptom suggests intent mismatch or physical fault, human-first is
   the correct default.

## Scenario matrix

| Scenario | Why operators care | Likely graph / telemetry signal | Best first action | Automation readiness | Why |
|---|---|---|---|---|---|
| Single BGP peer down after being established | Common underlay/control-plane incident on leaf-spine and SP edge links | Existing `bgp_session_down` | Targeted peer reset if vendor path is proven | `Ready now` on SR Linux only | Neighbor-level `admin-state` path is sourced and reversible on SR Linux |
| Repeated BGP peer flaps | Usually transport, optics, timers, BFD, or policy churn | Existing `bgp_session_flap` | Human investigation, not repeated resets | `Human-first` | Another bounce often worsens churn |
| All BGP peers on a node lost | High blast radius, likely node/fabric isolation or control-plane failure | Existing `bgp_all_peers_down` | Escalate and verify node health, reachability, source interface, recent changes | `Human-first` | Broad restart can hide root cause and increase impact |
| BGP peer never establishes | Usually intent mismatch: ASN, AFI-SAFI, auth, addressing, policy | Existing `bgp_never_established` | Validate intended configuration and transport reachability | `Human-first` | Resets rarely fix configuration mismatch |
| Interface oper-down | Common physical/layer-1 event | Existing `interface_down` | Distinguish admin-down vs physical fault vs lower-layer-down | `Blocked by graph` | Current graph lacks interface status verification |
| Interface error spike | Common optics/cabling/FEC/duplex symptom | Existing `interface_error_spike` | Investigate media, optics, far-end, FEC, queue drops | `Human-first` | A bounce is not a root-cause fix |
| Sustained high utilization | Common day-2 congestion signal | Existing `interface_high_utilization` | Capacity, TE, ECMP, policy, queueing analysis | `Human-first` | Busy links are often healthy links |
| LLDP neighbor disappeared on one interface | Strong topology-symptom signal in DC fabrics | Future candidate using `LldpNeighbor`/`CONNECTED_TO` drift | Check interface health, LLDP admin-state, recent topology changes | `Good future candidate` | Highly actionable if false positives are controlled |
| OSPF adjacency down on one interface | Common underlay routing incident in SP and routed DC | Future OSPF session-state detection | Validate interface state, BFD, timers, area, auth | `Good future candidate` | Single-adjacency failures are often local and reversible |
| IS-IS adjacency down on one interface | Common SP underlay symptom | Future IS-IS adjacency-state detection | Validate interface state, L1/L2 config, BFD, auth | `Good future candidate` | Fits SP scope and SR Linux docs are strong |
| BFD session down | Early indicator for BGP/IGP loss and fast-fail scenarios | Future BFD detection from telemetry/log events | Investigate transport, control packet filtering, timers, path asymmetry | `Good future candidate` | Great observability value, but remediation is indirect |
| MPLS LSP / SR policy failure | Important in SP topologies | Future MPLS/SR detections | Check IGP, SID/LSP state, constraints, headend policy intent | `Later` | Valuable but needs stronger graph schema first |

## What a day-2 engineer should know

### 1. Control-plane symptoms are not all equal

- A single established BGP peer dropping is often a local event and may be a
  candidate for a bounded, reversible action.
- A peer that never established is usually configuration intent mismatch.
- All peers dropping at once is a node- or fabric-level symptom and should not
  trigger broad automated resets.

### 2. Layer-1 and topology symptoms dominate real incidents

- Missing LLDP neighbors, interface down, or rising error counters often carry
  more root-cause signal than the routing protocol symptom that follows.
- A routing session reset may restore service briefly, but if the optics or
  physical path is unstable, the symptom comes straight back.

### 3. BFD is often the hidden amplifier

- In real networks, BFD frequently turns a marginal transport problem into fast
  routing churn.
- Bonsai should eventually detect BFD-down separately so operators can tell the
  difference between "BGP is broken" and "BGP is reacting correctly to BFD".

### 4. High utilization is not a failure by itself

- DC engineers see hot uplinks during workload shifts, maintenance drains, and
  elephant-flow placement.
- Treat this as visibility and engineering input, not as a trigger for
  disruptive remediation.

## Best future additions by priority

### Priority 1: topology and adjacency awareness

1. `lldp_neighbor_lost`
2. `ospf_adjacency_down`
3. `isis_adjacency_down`
4. `bfd_session_down`

These are the strongest next additions because they line up with what operators
actually use during fault isolation and because they fit Bonsai's graph-native
model well.

### Priority 2: verification improvements

1. Add interface admin/oper state to `Interface` nodes.
2. Add enough protocol state for OSPF/IS-IS adjacency verification.
3. Record firmware/version or capability metadata so playbook routing can be
   conditioned on what a device actually supports.

### Priority 3: SP-specific actions after core adjacency signals are solid

1. MPLS transport anomalies
2. Segment-routing policy anomalies
3. IGP-to-label forwarding consistency checks

## Source highlights

### Nokia SR Linux

- LLDP interface/global admin-state:
  [SR Linux LLDP guide](https://documentation.nokia.com/srlinux/26-3/books/interfaces/lldp.html)
- OSPF configuration and interface enablement:
  [SR Linux OSPF guide](https://documentation.nokia.com/srlinux/22-6/SR_Linux_Book_Files/Configuration_Basics_Guide/configb-ospf.html)
- IS-IS interface enablement:
  [SR Linux IS-IS guide](https://documentation.nokia.com/srlinux/22-3/SR_Linux_Book_Files/Configuration_Basics_Guide/configb-is-is.html)
- BFD on subinterfaces and under OSPF/IS-IS:
  [SR Linux BFD guide](https://documentation.nokia.com/srlinux/21-11/Configuration_Basics_Guide/configb-bfd.html)

### Cisco IOS XR / XRd

- gNMI transport, Set semantics, and origin handling:
  [IOS XR gNMI overview](https://www.cisco.com/c/en/us/support/docs/ios-nx-os-software/ios-xr-software/221690-configure-gnmi-and-implement-pyang-in-io.html)
- OpenConfig/native configuration behavior:
  [IOS XR data models for automation](https://www.cisco.com/c/en/us/td/docs/iosxr/cisco8000/programmability/b-programmability-configuration-guide-cisco8000/m-data-models-for-network-automation.html)
- Interface configuration YANG:
  [Cisco-IOS-XR-ifmgr-cfg.yang](https://raw.githubusercontent.com/YangModels/yang/main/vendor/cisco/xr/711/Cisco-IOS-XR-ifmgr-cfg.yang)

### Arista EOS / cEOS

- OpenConfig read/write behavior over gNMI:
  [EOS 4.28 OpenConfig read/write TOI](https://www.arista.com/support/toi/eos-4-28-0f)
- Vendor YANG publication:
  [Arista YANG repository](https://github.com/aristanetworks/yang)

### OpenConfig

- Interfaces:
  [openconfig-interfaces.yang](https://raw.githubusercontent.com/openconfig/public/master/release/models/interfaces/openconfig-interfaces.yang)
- BGP common:
  [openconfig-bgp-common.yang](https://raw.githubusercontent.com/openconfig/public/master/release/models/bgp/openconfig-bgp-common.yang)

## Recommended next implementation sequence

1. Add graph fields needed for interface verification.
2. Add an `lldp_neighbor_lost` detection before touching OSPF/IS-IS remediation.
3. Harvest SR Linux LLDP and BFD paths first because Nokia docs are explicit.
4. Only then design OSPF/IS-IS playbooks, starting human-first and narrowing to
   safe single-interface actions where the path and verification are both real.
