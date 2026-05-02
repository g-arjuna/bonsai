# sp_pe_evpn — Service-provider PE with BGP EVPN: base PE telemetry plus EVPN AFI-SAFI session health, type-2/3/5 route counts, VxLAN or MPLS-based EVI state.

**Environment**: service provider  
**Roles**: pe, peering  
**Vendor scope**: all vendors (OpenConfig + per-vendor natives)  
**Verification**: not-yet-verified

## Rationale

SP EVPN PEs carry MAC-VPN and IP-VPN state; BGP EVPN session health per EVI, route type counts, and encap-layer state are the primary signals for DCI and metro-Ethernet services.

## Subscribed Paths

| Path | Origin | Mode | Interval | Models | Vendors | Optional |
|------|--------|------|----------|--------|---------|----------|
| `…interfaces/interface[interface-name=*]/generic-counters` | native | SAMPLE | 10s | any of: `Cisco-IOS-XR-infra-statsd-oper` | cisco_xrd | no |
| `…evpn-oper:evpn/active/evi-detail/evi-children/neighbors` | native | ON_CHANGE | — | any of: `Cisco-IOS-XR-evpn-oper` | cisco_xrd | yes |
| `Cisco-IOS-XR-evpn-oper:evpn/active/summary` | native | SAMPLE | 1m | any of: `Cisco-IOS-XR-evpn-oper` | cisco_xrd | yes |
| `…instance[name=*]/protocols/bgp/neighbor[peer-address=*]` | native | ON_CHANGE | — | any of: `srl_nokia` | nokia_srl | no |
| `…[index=*]/bridge-table/unicast-destinations/destination` | native | ON_CHANGE | — | any of: `srl_nokia` | nokia_srl | yes |
| `interfaces` | openconfig | SAMPLE | 10s | `openconfig-interfaces` | all vendors | no |
| `interfaces` | openconfig | ON_CHANGE | — | `openconfig-interfaces` | all vendors | no |
| `network-instances` | openconfig | ON_CHANGE | — | any of: `openconfig-bgp`, `openconfig-network-instance` | all vendors | no |
| `…s/neighbor/afi-safis/afi-safi[afi-safi-name=L2VPN_EVPN]` | openconfig | ON_CHANGE | — | any of: `openconfig-bgp` | all vendors | no |
| `mpls` | openconfig | ON_CHANGE | — | `openconfig-mpls` | all vendors | yes |
| `network-instances/network-instance/protocols/protocol/isis` | openconfig | ON_CHANGE | — | `openconfig-isis` | all vendors | no |
| `lldp` | openconfig | ON_CHANGE | — | `openconfig-lldp` | all vendors | yes |
| `bfd` | openconfig | ON_CHANGE | — | `openconfig-bfd` | all vendors | yes |

## YANG Models Required

| Model | Vendor scope |
|-------|-------------|
| `Cisco-IOS-XR-evpn-oper` | cisco_xrd (any-of) |
| `Cisco-IOS-XR-infra-statsd-oper` | cisco_xrd (any-of) |
| `openconfig-bfd` | all vendors |
| `openconfig-bgp` | all vendors (any-of) |
| `openconfig-interfaces` | all vendors |
| `openconfig-isis` | all vendors |
| `openconfig-lldp` | all vendors |
| `openconfig-mpls` | all vendors |
| `openconfig-network-instance` | all vendors (any-of) |
| `srl_nokia` | nokia_srl (any-of) |

## Vendor-Native Fallbacks

- **cisco_xrd** `Cisco-IOS-XR-evpn-oper:evpn/active/evi-detail/evi-children/neighbors` falls back for `network-instances` when the preferred OpenConfig model is not advertised.

## Path Rationales

- **`Cisco-IOS-XR-infra-statsd-oper:infra-statistics/interfaces/interface[interface-name=*]/generic-counters`** [native] — IOS-XR native interface counters.
- **`Cisco-IOS-XR-evpn-oper:evpn/active/evi-detail/evi-children/neighbors`** [native] — IOS-XR native EVPN EVI neighbor state when openconfig-evpn not advertised.
- **`Cisco-IOS-XR-evpn-oper:evpn/active/summary`** [native] — IOS-XR native EVPN summary — EVI counts and MAC/IP table sizes.
- **`network-instance[name=*]/protocols/bgp/neighbor[peer-address=*]`** [native] — SR Linux native BGP across all network instances (default + EVPN VRFs).
- **`tunnel-interface[name=*]/vxlan-interface[index=*]/bridge-table/unicast-destinations/destination`** [native] — SR Linux native VxLAN bridge-table state for EVPN-VxLAN DCI.
- **`interfaces`** [openconfig] — OpenConfig interface counters.
- **`interfaces`** [openconfig] — OpenConfig interface oper-state.
- **`network-instances`** [openconfig] — OpenConfig BGP and network-instance state across all EVIs.
- **`network-instances/network-instance/protocols/protocol/bgp/neighbors/neighbor/afi-safis/afi-safi[afi-safi-name=L2VPN_EVPN]`** [openconfig] — OpenConfig L2VPN-EVPN AFI-SAFI per session — route type counts and session state.
- **`mpls`** [openconfig] — OpenConfig MPLS when EVPN uses MPLS encapsulation.
- **`network-instances/network-instance/protocols/protocol/isis`** [openconfig] — OpenConfig IS-IS underlay state.
- **`lldp`** [openconfig] — OpenConfig LLDP.
- **`bfd`** [openconfig] — OpenConfig BFD.

## Known Gaps

<!-- Add known gaps, vendor quirks, or lab-verification notes here. -->
