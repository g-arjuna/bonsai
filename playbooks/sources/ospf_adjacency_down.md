# `ospf_adjacency_down` Harvest Notes

This note captures the current, sourced understanding of OSPF adjacency loss as
a future Bonsai detection and remediation surface.

## Why this matters operationally

For routed DC and SP topologies, a single OSPF adjacency loss is one of the
highest-value signals during day-2 troubleshooting:

- it is local enough to be actionable,
- it often precedes larger reachability symptoms,
- and it is usually tied to one interface or one peer relationship.

## Detection posture

Recommended future detection rule: `ospf_adjacency_down`

Suggested first detection semantics:

- Fire when an OSPF adjacency on a specific interface transitions from full/up
  to down.
- Capture:
  - `device_address`
  - `if_name`
  - future `neighbor_id`
  - future `old_state`
  - future `new_state`

## Verification posture

This should remain human-first until OSPF adjacency state is represented in the
graph. A future verification shape could be:

```cypher
MATCH (d:Device {address: $device_address})-[:HAS_OSPF_NEIGHBOR]->(n:OspfNeighbor {if_name: $if_name})
WHERE n.adjacency_state = "full"
RETURN count(n) > 0
```

That query is illustrative only; `OspfNeighbor` is not in the current schema.

## Source-backed model hints

### Nokia SR Linux

Primary sources:

- [SR Linux OSPF configuration guide](https://documentation.nokia.com/srlinux/22-6/SR_Linux_Book_Files/Configuration_Basics_Guide/configb-ospf.html)
- [SR Linux network-instance data model reference](https://documentation.nokia.com/srlinux/21-11/Data_Model_Reference/srl_nokia-network-instance.html)
- [SR Linux tools network-instance data model reference](https://documentation.nokia.com/srlinux/22-3/SR_Linux_Book_Files/Data_Model_Reference/srl_nokia-tools-network-instance_0.html)

Grounded observations:

- SR Linux documents OSPF instance `admin-state enable` and per-area interface
  configuration under `network-instance/protocols/ospf/instance/area/interface`.
- The tools data model explicitly exposes a neighbor `clear` action under an
  OSPF interface:
  `network-instance ... protocols ospf instance ... area ... interface ... neighbors clear`

Grounded candidate paths:

- OSPF instance admin-state:
  `network-instance[name=default]/protocols/ospf/instance[name=default]/admin-state`
- OSPF interface admin-state:
  `network-instance[name=default]/protocols/ospf/instance[name=default]/area[area-id={area_id}]/interface[interface-name={if_name}]/admin-state`
- Candidate targeted clear action from tools model:
  `network-instance[name=default]/protocols/ospf/instance[name=default]/area[area-id={area_id}]/interface[interface-name={if_name}]/neighbors/clear`

Why not executable yet:

- Bonsai does not yet carry `area_id` or OSPF neighbor state in detection
  features.
- The `tools` clear path needs explicit gNMI Set validation before we can treat
  it as a real playbook step.
- Broad OSPF instance bounces are too disruptive for a first action.

Operational recommendation:

- First future executable action, if validated, should be the interface-scoped
  `neighbors/clear` path from the tools model.
- Second choice could be interface-level OSPF `admin-state` bounce.
- Avoid instance-wide OSPF restarts as an automatic first play.

### Cisco IOS XR / XRd

Primary sources:

- [Cisco IOS XR gNMI overview](https://www.cisco.com/c/en/us/support/docs/ios-nx-os-software/ios-xr-software/221690-configure-gnmi-and-implement-pyang-in-io.html)
- [Cisco IOS XR programmability / data models](https://www.cisco.com/c/en/us/td/docs/iosxr/cisco8000/programmability/b-programmability-configuration-guide-cisco8000/m-data-models-for-network-automation.html)

Grounded observations:

- Cisco documents gNMI and modeled configuration behavior, but this harvest did
  not yet validate a specific XRd OSPF neighbor-reset or interface-bounce path
  from sourced YANG.
- XRd OSPF should therefore remain a source-harvest and lab-validation target,
  not a speculative playbook.

Why not executable yet:

- Need a validated vendor-native or OpenConfig OSPF path for XRd.
- Need OSPF neighbor state in the graph for verification.

### Arista EOS / cEOS

Primary sources:

- [Arista EOS OpenConfig read/write support](https://www.arista.com/support/toi/eos-4-28-0f)

Grounded observations:

- Arista documents a read/write OpenConfig surface, but this harvest did not
  validate a concrete cEOS OSPF adjacency-reset path from vendor-published YANG
  or primary docs.

Why not executable yet:

- Need a validated OSPF path and lab confirmation.
- Need OSPF neighbor graph support.

## Bonsai recommendation

`ospf_adjacency_down` is a top-tier future detection and is worth implementing
soon after `bfd_session_down`.

Recommended first playbook stance once the detection exists:

- `nokia_srl`: manual-only initially, but very promising future candidate for
  interface-scoped neighbor clear
- `cisco_xrd`: manual-only
- `arista_ceos`: manual-only

This is one of the best future scenarios for a tightly scoped, graph-verified
closed-loop action once the detection and schema catch up.
