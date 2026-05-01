# campus_distribution — Campus wired distribution switch

**Environment**: campus_wired  
**Roles**: distribution, core, border  
**Verification**: not-yet-verified (requires campus topology with STP root configured at distribution)

## YANG Models Required

| Path | Model | Notes |
|------|-------|-------|
| `interfaces` | `openconfig-interfaces` | All vendors |
| `lldp` | `openconfig-lldp` | All vendors |
| `vlans` | `openconfig-vlan` | Optional |
| `stp` | `openconfig-spanning-tree` | Optional |
| `network-instances/.../ospf` | `openconfig-ospfv2` | Optional — OSPF inter-VLAN routing |
| `network-instances/.../isis` | `openconfig-isis` | Optional — IS-IS if deployed |
| `bfd` | `openconfig-bfd` | Optional |

## Signal Rationale

Distribution devices aggregate access switches and route between VLANs. A failure here affects
all downstream access switches in the pod. Key signals:

- **Interface counters** (SAMPLE 30 s): uplink utilisation to core switches — saturation causes
  packet loss for all access switches beneath.
- **Interface oper-state** (ON_CHANGE): uplink flaps propagate STP topology changes immediately.
- **LLDP** (ON_CHANGE): physical topology to both access (downstream) and core (upstream).
- **STP** (ON_CHANGE, optional): distribution is often STP root bridge; topology changes here
  cascade across the pod. Root bridge changes are an incident signal.
- **OSPF/IS-IS** (ON_CHANGE, optional): inter-VLAN routing adjacency health — loss here causes
  silent black-holing of inter-VLAN traffic.
- **BFD** (ON_CHANGE, optional): fast uplink failure detection supplementing STP convergence.

## Lab Setup for Verification

Requires a topology with:
1. One or two distribution switches acting as STP root for a set of VLANs
2. Two or more access switches connected downstream
3. Core switch upstream (or campus_core device)
4. OSPF or IS-IS adjacency between distribution and core

Expected telemetry after convergence:
- `stp` shows root bridge role for target VLANs
- `network-instances/.../ospf` shows `FULL` adjacency to core
- Interface counters update on the uplink ports at 30 s intervals

## Known Gaps

- Cisco IOS-XR VLAN YANG paths not yet lab-verified; `openconfig-vlan` support on IOS-XR
  is limited — use optional flag.
- STP on Juniper cRPD is not supported in the container image; skip that path for lab testing.
