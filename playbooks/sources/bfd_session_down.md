# `bfd_session_down` Harvest Notes

This note captures the current, sourced understanding of BFD as a future Bonsai
detection and remediation surface.

## Why this matters operationally

BFD is often the earliest day-2 signal that transport or adjacency health is
degrading. In practice:

- BGP or OSPF sessions may flap *because* BFD is doing its job.
- A separate BFD detection helps operators distinguish cause from effect.
- Automatic remediation should usually target the owning protocol or the
  underlying interface, not "restart BFD" blindly.

## Detection posture

Recommended future detection rule: `bfd_session_down`

Suggested first detection semantics:

- Fire when a previously up BFD session transitions to down.
- Record enough context to identify the attachment point:
  - `device_address`
  - `if_name` where possible
  - future `peer_address`
  - future `protocol_owner` if derived

## Verification posture

This should stay human-first until Bonsai stores BFD session state explicitly in
the graph. Once that exists, a clean verification shape is:

```cypher
MATCH (d:Device {address: $device_address})-[:HAS_INTERFACE]->(i:Interface {name: $if_name})
MATCH (d)-[:HAS_BFD_SESSION]->(b:BfdSession)
WHERE b.session_state = "up"
RETURN count(b) > 0
```

That query is illustrative only; `BfdSession` is not part of the current graph
schema yet.

## Source-backed model hints

### Nokia SR Linux

Primary sources:

- [SR Linux BFD data model reference](https://documentation.nokia.com/srlinux/25-3/books/data-model-reference/srl_nokia-bfd_0.html)
- [SR Linux BFD configuration guide](https://documentation.nokia.com/srlinux/21-11/Configuration_Basics_Guide/configb-bfd.html)

Grounded observations:

- SR Linux exposes BFD state including `session-state` under the native BFD
  model.
- The native model clearly shows configurable `admin-state` for micro-BFD and
  `network-instance` BFD peer state.
- For vendor-neutral configuration, OpenConfig BFD is still the cleaner future
  path where supported.

Candidate OpenConfig path:

- `bfd/interfaces/interface[id={if_name}]/config/enabled`
- RFC 7951 value: `true` or `false`

Candidate OpenConfig peer-state anchor:

- `bfd/interfaces/interface[id={if_name}]/peers/peer/state/session-state`

Why not executable yet:

- Bonsai does not yet ingest BFD sessions into the graph.
- `if_name` alone may not uniquely identify the intended BFD peer in every
  topology.
- A BFD bounce can worsen churn if the root cause is lower-layer instability.

### Cisco IOS XR / XRd

Primary sources:

- [Cisco IOS XR gNMI overview](https://www.cisco.com/c/en/us/support/docs/ios-nx-os-software/ios-xr-software/221690-configure-gnmi-and-implement-pyang-in-io.html)
- [Cisco IOS XR BFD guide](https://www.cisco.com/c/en/us/td/docs/iosxr/ncs5500/routing/79x/b-routing-cg-ncs5500-79x/implementing-bfd.html)

Grounded observations:

- Cisco explicitly states that BFD can be configured and observed using
  `openconfig-bfd.yang`.
- This is a strong signal that OpenConfig BFD is the right first harvest path
  for XRd.

Candidate OpenConfig path:

- `bfd/interfaces/interface[id={if_name}]/config/enabled`

Why not executable yet:

- No Bonsai graph schema for BFD sessions yet.
- Need XRd-specific lab confirmation that the OpenConfig path is actually
  writable on the target image and that toggling it does not have broader side
  effects than expected.

### Arista EOS / cEOS

Primary sources:

- [Arista EOS OpenConfig read/write support](https://www.arista.com/support/toi/eos-4-28-0f)
- [OpenConfig BFD model](https://raw.githubusercontent.com/openconfig/public/master/release/models/bfd/openconfig-bfd.yang)

Grounded observations:

- Arista documents read/write OpenConfig exposure over gNMI when OpenConfig is
  enabled.
- That makes OpenConfig BFD the right candidate model for cEOS too.

Candidate OpenConfig path:

- `bfd/interfaces/interface[id={if_name}]/config/enabled`

Why not executable yet:

- Need cEOS lab validation for BFD write support and exact behavior.
- Still blocked on the absence of BFD session nodes in the graph.

## Bonsai recommendation

`bfd_session_down` should be implemented as a detection before it is attempted
as an automatic remediation target.

Recommended first playbook stance once the detection exists:

- `nokia_srl`: manual-only
- `cisco_xrd`: manual-only
- `arista_ceos`: manual-only

Once graph verification exists and lab tests prove vendor behavior, revisit
single-interface BFD enable/disable as a possible `safe` action.
