# sp_rr_basic — Service-provider route-reflector profile: BGP session health, reflected prefix counts, no data-plane paths.

**Environment**: service provider  
**Roles**: rr, route-reflector  
**Vendor scope**: all vendors (OpenConfig + per-vendor natives)  
**Verification**: not-yet-verified

## Rationale

Route reflectors carry only control-plane state. The signal is entirely BGP: session count, prefix counts per client, and RR client group health. No MPLS or interface counters are required.

## Subscribed Paths

| Path | Origin | Mode | Interval | Models | Vendors | Optional |
|------|--------|------|----------|--------|---------|----------|
| `…instance/instance-active/default-vrf/neighbors/neighbor` | native | ON_CHANGE | — | any of: `Cisco-IOS-XR-ipv4-bgp-oper` | cisco_xrd | no |
| `…p/instances/instance/instance-active/default-vrf/afs/af` | native | SAMPLE | 1m | any of: `Cisco-IOS-XR-ipv4-bgp-oper` | cisco_xrd | yes |
| `network-instances` | openconfig | ON_CHANGE | — | any of: `openconfig-bgp`, `openconfig-network-instance` | all vendors | no |
| `…cols/protocol/bgp/neighbors/neighbor/afi-safis/afi-safi` | openconfig | SAMPLE | 1m | any of: `openconfig-bgp` | all vendors | yes |
| `interfaces` | openconfig | ON_CHANGE | — | `openconfig-interfaces` | all vendors | yes |
| `bfd` | openconfig | ON_CHANGE | — | `openconfig-bfd` | all vendors | yes |

## YANG Models Required

| Model | Vendor scope |
|-------|-------------|
| `Cisco-IOS-XR-ipv4-bgp-oper` | cisco_xrd (any-of) |
| `openconfig-bfd` | all vendors |
| `openconfig-bgp` | all vendors (any-of) |
| `openconfig-interfaces` | all vendors |
| `openconfig-network-instance` | all vendors (any-of) |

## Vendor-Native Fallbacks

- **cisco_xrd** `Cisco-IOS-XR-ipv4-bgp-oper:bgp/instances/instance/instance-active/default-vrf/neighbors/neighbor` falls back for `network-instances` when the preferred OpenConfig model is not advertised.

## Path Rationales

- **`Cisco-IOS-XR-ipv4-bgp-oper:bgp/instances/instance/instance-active/default-vrf/neighbors/neighbor`** [native] — IOS-XR native BGP neighbor state for RR client sessions.
- **`Cisco-IOS-XR-ipv4-bgp-oper:bgp/instances/instance/instance-active/default-vrf/afs/af`** [native] — IOS-XR native BGP AFI summary — prefix counts per address family.
- **`network-instances`** [openconfig] — OpenConfig BGP session and prefix state for all RR client sessions.
- **`network-instances/network-instance/protocols/protocol/bgp/neighbors/neighbor/afi-safis/afi-safi`** [openconfig] — OpenConfig per-AFI prefix count per session — key RR health indicator.
- **`interfaces`** [openconfig] — OpenConfig interface state — loopback health for RR reachability.
- **`bfd`** [openconfig] — OpenConfig BFD when RR uses BFD to accelerate client failure detection.

## Known Gaps

<!-- Add known gaps, vendor quirks, or lab-verification notes here. -->
