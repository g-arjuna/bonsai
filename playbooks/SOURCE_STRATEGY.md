# Bonsai Playbook Harvest Strategy

This directory bootstraps the remediation catalog with the sources and
priorities that matter most for Bonsai's current DC/SP scope.

## Source precedence

Use sources in this order for every harvested playbook:

1. Vendor product documentation that explicitly describes the operation or
   the writable config leaf.
2. Vendor-published YANG modules or vendor-published YANG repositories.
3. Official OpenConfig models, but only when vendor documentation confirms
   that OpenConfig is writable over gNMI on that platform.

If a writable path is not clearly present in the sourced YANG tree, do not
invent it. Emit a no-op playbook with `steps: []` and explain the gap.

## Current detection-first harvest order

This order matches the existing detections in
`python/bonsai_sdk/rules/{bgp,interface}.py` and starts from the highest-value
operator actions.

1. `bgp_session_down`
   - Nokia SR Linux: vendor-native neighbor `admin-state` bounce is the best
     first real playbook.
   - Cisco XRd: hold until a per-neighbor writable BGP reset path is validated
     from sourced YANG.
   - Arista cEOS: hold until a per-neighbor writable BGP reset path is
     validated from sourced YANG.
2. `interface_down`
   - All vendors: technically harvestable paths exist for interface
     administrative state, but Bonsai cannot yet verify success cleanly because
     the graph schema does not store interface admin/oper state on the
     `Interface` node. Keep these as no-op entries until graph verification is
     improved or a separate admin-down detection exists.
3. `bgp_session_flap`
   - Prefer no-op/human investigation first. Repeated resets can worsen an
     unstable peer.
4. `bgp_all_peers_down`
   - No-op/human investigation. A box- or fabric-level fault is more likely
     than a single bad session.
5. `bgp_never_established`
   - No-op/human investigation until peer configuration intent is represented
     in graph context.
6. `interface_error_spike`, `interface_high_utilization`
   - Hold until the graph and detection features include enough context to
     distinguish congestion, optics faults, and policy issues from conditions
     that an automated state toggle would actually help.

## Primary sources

### Nokia SR Linux

- BGP configuration and reset guidance:
  https://documentation.nokia.com/srlinux/24-10/books/routing-protocols/bgp.html
- BGP neighbor data model reference (`admin-state` under neighbor):
  https://documentation.nokia.com/srlinux/22-3/SR_Linux_Book_Files/Data_Model_Reference/srl_nokia-network-instance_0.html
- Interface data model reference (`interface/name/admin-state`):
  https://documentation.nokia.com/srlinux/22-3/SR_Linux_Book_Files/Data_Model_Reference/srl_nokia-interfaces_0.html

### Cisco IOS XR / XRd

- gNMI transport and Set semantics:
  https://www.cisco.com/c/en/us/support/docs/ios-nx-os-software/ios-xr-software/221690-configure-gnmi-and-implement-pyang-in-io.html
- OpenConfig/native origin handling on IOS XR:
  https://www.cisco.com/c/en/us/td/docs/iosxr/cisco8000/programmability/b-programmability-configuration-guide-cisco8000/m-grpc-fundamentals-and-authentication.html
- OpenConfig behavior and active-agent rules:
  https://www.cisco.com/c/en/us/td/docs/iosxr/cisco8000/programmability/b-programmability-configuration-guide-cisco8000/m-data-models-for-network-automation.html
- Vendor-native interface YANG:
  https://raw.githubusercontent.com/YangModels/yang/main/vendor/cisco/xr/711/Cisco-IOS-XR-ifmgr-cfg.yang

### Arista EOS / cEOS

- OpenConfig read/write behavior over gNMI:
  https://www.arista.com/support/toi/eos-4-28-0f
- OpenConfig support landing page:
  https://www.arista.com/en/support/toi/tag/openconfig
- Vendor-published YANG repository:
  https://github.com/aristanetworks/yang

### OpenConfig

- Interfaces model:
  https://raw.githubusercontent.com/openconfig/public/master/release/models/interfaces/openconfig-interfaces.yang
- Network instance model:
  https://raw.githubusercontent.com/openconfig/public/master/release/models/network-instance/openconfig-network-instance.yang
- BGP model family:
  https://raw.githubusercontent.com/openconfig/public/master/release/models/bgp/openconfig-bgp-common.yang

## Practical guidance for the next sessions

- Prefer Nokia first for control-plane remediation because the documentation is
  explicit and the vendor-native paths are easy to validate.
- Prefer OpenConfig for interface admin-state on Cisco/Arista before touching
  vendor-native presence-leaf `shutdown` style models, because Bonsai's current
  playbook schema handles direct Set values better than create/delete presence
  semantics.
- Do not auto-heal interface symptoms until verification can prove recovery in
  graph state.

## Current catalog status

The current `playbooks/library/` directory now covers every detection rule that
exists in `python/bonsai_sdk/rules/`:

- `bfd_session_down`
- `bgp_session_down`
- `bgp_session_flap`
- `bgp_all_peers_down`
- `bgp_never_established`
- `interface_down`
- `interface_error_spike`
- `interface_high_utilization`
- `topology_edge_lost`

Executable entries currently available:

- Nokia SR Linux `bgp_session_down` via neighbor `admin-state` bounce
- Nokia SR Linux `topology_edge_lost` via interface LLDP `admin-state` bounce

All BFD entries and most other entries are intentionally manual-only until either:

- a vendor-validated gNMI Set path is confirmed, or
- the graph schema gains enough recovery-state context to verify success
  deterministically.
