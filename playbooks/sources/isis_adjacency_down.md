# `isis_adjacency_down` Harvest Notes

This note captures the current, sourced understanding of IS-IS adjacency loss
as a future Bonsai detection and remediation surface.

## Why this matters operationally

IS-IS adjacency loss is a high-value day-2 signal in service-provider and
routed-underlay environments because:

- it is usually interface-local and operationally actionable,
- it frequently precedes broader route loss,
- and it maps cleanly to how SP engineers reason about control-plane health.

## Detection posture

Recommended future detection rule: `isis_adjacency_down`

Suggested first detection semantics:

- Fire when an IS-IS adjacency on a specific interface transitions from up/full
  to down.
- Capture:
  - `device_address`
  - `if_name`
  - future `level`
  - future `neighbor_system_id`
  - future `old_state`
  - future `new_state`

## Verification posture

This should remain human-first until IS-IS adjacency state is represented in
the graph. A future verification shape could be:

```cypher
MATCH (d:Device {address: $device_address})-[:HAS_ISIS_NEIGHBOR]->(n:IsisNeighbor {if_name: $if_name})
WHERE n.adjacency_state = "up"
RETURN count(n) > 0
```

That query is illustrative only; `IsisNeighbor` is not part of the current
graph schema yet.

## Source-backed model hints

### Nokia SR Linux

Primary sources:

- [SR Linux IS-IS guide](https://documentation.nokia.com/srlinux/22-3/SR_Linux_Book_Files/Configuration_Basics_Guide/configb-is-is.html)
- [SR Linux IS-IS routing guide](https://documentation.nokia.com/srlinux/23-3/books/routing-protocols/is-is.html)

Grounded observations:

- SR Linux configures IS-IS per interface under
  `network-instance/protocols/isis/instance/interface`.
- The documentation shows interface-level family admin-state under the IS-IS
  interface, for example `ipv4-unicast { admin-state enable }`.
- The tools model explicitly documents a targeted interface adjacency clear:
  `tools network-instance default protocols isis instance i1 interface ethernet-1/1.1 adjacencies clear`

Grounded candidate paths:

- Interface-scoped IS-IS IPv4 family admin-state:
  `network-instance[name=default]/protocols/isis/instance[name={instance_name}]/interface[interface-name={if_name}]/ipv4-unicast/admin-state`
- Candidate targeted adjacency clear action:
  `network-instance[name=default]/protocols/isis/instance[name={instance_name}]/interface[interface-name={if_name}]/adjacencies/clear`

Why not executable yet:

- Bonsai does not yet carry `instance_name`, adjacency state, or IS-IS neighbor
  identity in detection features.
- The `adjacencies/clear` tools path is documented, but still needs explicit
  gNMI Set validation before it can be promoted into a real playbook.
- Broad family or instance bounces are more disruptive than a first action
  should be.

Operational recommendation:

- Best future first action, if validated, is the interface-scoped
  `adjacencies/clear` path.
- Second choice could be IS-IS interface family `admin-state` bounce.
- Avoid instance-wide IS-IS restarts as an automatic first play.

### Cisco IOS XR / XRd

Primary sources:

- [Cisco IOS XR gNMI overview](https://www.cisco.com/c/en/us/support/docs/ios-nx-os-software/ios-xr-software/221690-configure-gnmi-and-implement-pyang-in-io.html)
- [Cisco IOS XR programmability introduction](https://www.cisco.com/c/en/us/td/docs/iosxr/cisco8000/programmability/b-programmability-configuration-guide-cisco8000/m-programmability-introduction.html)

Grounded observations:

- Cisco documents the general YANG/gNMI programmability surface and how to
  discover supported models on-box or in published repositories.
- This harvest did not yet validate a specific XRd IS-IS adjacency-clear or
  interface-bounce path from sourced YANG.

Why not executable yet:

- Need a validated XRd IS-IS model path from vendor-published YANG or a tested
  OpenConfig equivalent.
- Need IS-IS adjacency state in the graph for verification.

### Arista EOS / cEOS

Primary sources:

- [Arista EOS OpenConfig read/write support](https://www.arista.com/support/toi/eos-4-28-0f)

Grounded observations:

- Arista documents a read/write OpenConfig surface, but this harvest did not
  validate a concrete cEOS IS-IS adjacency-reset path from primary docs or
  vendor YANG.

Why not executable yet:

- Need a validated IS-IS path and lab confirmation.
- Need IS-IS neighbor graph support.

## Bonsai recommendation

`isis_adjacency_down` is a top-tier future detection and belongs in the same
priority band as `ospf_adjacency_down`.

Recommended first playbook stance once the detection exists:

- `nokia_srl`: manual-only initially, but strong future candidate for
  interface-scoped adjacency clear
- `cisco_xrd`: manual-only
- `arista_ceos`: manual-only

If the SR Linux tools path is confirmed over gNMI, `isis_adjacency_down` could
become one of the cleanest examples of a bounded, operator-grade closed-loop
action in Bonsai.
