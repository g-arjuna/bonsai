# campus_access — Campus wired access switch

**Environment**: campus_wired  
**Roles**: access, edge  
**Verification**: not-yet-verified (requires campus lab with Arista cEOS or Nokia SRL in access mode)

## YANG Models Required

| Path | Model | Notes |
|------|-------|-------|
| `interfaces` | `openconfig-interfaces` | All vendors |
| `lldp` | `openconfig-lldp` | All vendors |
| `vlans` | `openconfig-vlan` | Optional; varies by platform |
| `stp` | `openconfig-spanning-tree` | Optional; varies by platform |
| `system/aaa/...` | `openconfig-system` | Optional; 802.1x radius on supported platforms |

## OpenConfig Wireless / 802.1x Coverage Note

OpenConfig `openconfig-system` surfaces authentication state on some platforms (Arista EOS) but not uniformly. The 802.1x / RADIUS failure signal is low-coverage via OC today. If 802.1x telemetry is required, a vendor-native plugin catalogue is the right path.

## Spanning Tree Signals

`openconfig-spanning-tree` carries:
- Port role (root/designated/alternate/backup)
- Port state (blocking/forwarding/learning)
- Topology change notification (TCN) counters

A TCN storm on an access switch is a reliable precursor to a broadcast storm incident. The bonsai rule engine should fire on TCN rate exceeding threshold.

## Known Platform Gaps

- **Nokia SR Linux**: SR Linux is a DC-focused platform; campus VLAN and STP model support may be partial in current releases.
- **Juniper cRPD**: cRPD is a routing daemon, not a switch; this profile does not apply to cRPD.
- **Arista cEOS**: Strong OC VLAN and STP support; good candidate for campus lab verification.
- **Cisco IOS-XR**: Not a campus access platform; use IOS/IOS-XE or Catalyst native.

## Lab Setup for Verification

Use Arista cEOS in a ContainerLab campus topology:
- 2× access switch (cEOS)
- 1× distribution switch (cEOS)
- Connect with trunk links, configure VLANs 10/20
- Run LLDP; verify topology appears in bonsai graph
