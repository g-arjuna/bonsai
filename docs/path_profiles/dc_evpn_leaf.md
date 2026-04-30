# dc_evpn_leaf — DC leaf with BGP EVPN

**Environment**: data_center  
**Roles**: leaf, access, border-leaf  
**Verification**: not-yet-verified (requires EVPN lab topology)

## YANG Models Required

| Path | Model | Notes |
|------|-------|-------|
| `interfaces` | `openconfig-interfaces` | All vendors |
| `network-instances` | `openconfig-bgp`, `openconfig-network-instance` | All vendors |
| `.../afi-safi[afi-safi-name=L2VPN_EVPN]` | `openconfig-bgp` | Optional; per-session EVPN AFI |
| `lldp` | `openconfig-lldp` | All vendors |
| `bfd` | `openconfig-bfd` | Optional |
| Nokia VxLAN path | `srl_nokia` | Nokia SRL only |
| `Cisco-IOS-XR-evpn-oper:...` | `Cisco-IOS-XR-evpn-oper` | IOS-XR only, optional |

## What EVPN Paths Add Over dc_leaf_minimal

- BGP L2VPN-EVPN AFI-SAFI state per session: tracks whether the EVPN peering is up and how many prefixes are being exchanged per type.
- Nokia SRL VxLAN bridge-table unicast destinations: MAC/IP bindings visible in the data plane — key for detecting EVPN convergence failures where BGP is up but MACs aren't being learned.
- IOS-XR native EVPN EVI neighbor state: fallback when `openconfig-evpn` is not advertised.

## Lab Setup for Verification

Requires a topology with:
1. Two EVPN leaves (Nokia SRL or cEOS)
2. A spine acting as route reflector for the EVPN overlay
3. A VxLAN VNI configured on each leaf

Expected telemetry after convergence:
- `afi-safi[afi-safi-name=L2VPN_EVPN]` shows `ESTABLISHED` and non-zero `prefixes-received`
- Nokia: `tunnel-interface/vxlan-interface/.../destination` shows remote VTEP entries

## Known Gaps

- OpenConfig does not have a standardised EVPN table path as of 2026. Route type breakdowns (type-2 MAC/IP, type-3 IMET, type-5 prefix) are not individually addressable via OC. Vendor-native paths are needed for that granularity.
- Juniper cRPD EVPN: native YANG paths not yet included; add when cRPD accounts are available for lab testing.
