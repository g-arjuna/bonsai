# sp_pe_l3vpn — SP PE with L3VPN

**Environment**: service_provider  
**Roles**: pe, peering  
**Verification**: not-yet-verified (requires SP L3VPN lab topology)

## YANG Models Required

| Path | Model | Notes |
|------|-------|-------|
| `interfaces` | `openconfig-interfaces` | All vendors |
| `network-instances` | `openconfig-bgp`, `openconfig-network-instance` | All vendors; covers VRFs |
| `.../afi-safi[L3VPN_IPV4_UNICAST]` | `openconfig-bgp` | Optional; VPNv4 prefix counts |
| `.../afi-safi[L3VPN_IPV6_UNICAST]` | `openconfig-bgp` | Optional; VPNv6 prefix counts |
| `mpls` | `openconfig-mpls` | VPN label bindings |
| `.../isis` | `openconfig-isis` | Underlay IS-IS |
| `segment-routing` | `openconfig-segment-routing` | Optional; when SR transport |
| `Cisco-IOS-XR-ip-rib-ipv4-oper:...` | `Cisco-IOS-XR-ip-rib-ipv4-oper` | IOS-XR only, optional |

## Key Signals for L3VPN SLA Detection

1. **VPNv4/VPNv6 prefix count per session**: sudden drop = customer route withdrawal or session reset.
2. **Per-VRF network-instance state**: `admin-status` down = customer VRF misconfiguration.
3. **MPLS LSP health**: broken LSP = traffic black-holing even when BGP is up.
4. **IS-IS adjacency**: PE losing underlay reachability precedes customer impact.

## IOS-XR Specifics

- `Cisco-IOS-XR-ip-rib-ipv4-oper` provides per-VRF route counts; useful for detecting prefix table exhaustion or route withdrawal storms.
- `openconfig-network-instance` VRF support on IOS-XR requires `openconfig-network-instance` to be advertised; verify with Capabilities before relying on it.

## Lab Setup for Verification

Requires: 2× PE (Nokia SRL or Cisco XRd), 1× RR, 2× CE (FRR). Configure:
- VPNv4 BGP between PEs and RR
- One L3VPN VRF per PE with a CE-facing interface
- Inject a prefix into one CE; verify it appears in the remote CE's VRF
