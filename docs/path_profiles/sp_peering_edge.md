# sp_peering_edge — Service-provider peering edge: external BGP sessions, BFD, interface health, and prefix-filter state.

**Environment**: service provider  
**Roles**: peering, edge, ce-facing  
**Vendor scope**: all vendors (OpenConfig + per-vendor natives)  
**Verification**: not-yet-verified

## Rationale

Peering devices terminate eBGP sessions with peers and customers; session health, prefix-count anomalies, and BFD state are the primary failure signals. No MPLS or IGP paths by default.

## Subscribed Paths

| Path | Origin | Mode | Interval | Models | Vendors | Optional |
|------|--------|------|----------|--------|---------|----------|
| `…interfaces/interface[interface-name=*]/generic-counters` | native | SAMPLE | 10s | any of: `Cisco-IOS-XR-infra-statsd-oper` | cisco_xrd | no |
| `…instance/instance-active/default-vrf/neighbors/neighbor` | native | ON_CHANGE | — | any of: `Cisco-IOS-XR-ipv4-bgp-oper` | cisco_xrd | no |
| `…p/instances/instance/instance-active/default-vrf/afs/af` | native | SAMPLE | 1m | any of: `Cisco-IOS-XR-ipv4-bgp-oper` | cisco_xrd | yes |
| `…ce[name=default]/protocols/bgp/neighbor[peer-address=*]` | native | ON_CHANGE | — | any of: `srl_nokia` | nokia_srl | no |
| `interfaces` | openconfig | SAMPLE | 10s | `openconfig-interfaces` | all vendors | no |
| `interfaces` | openconfig | ON_CHANGE | — | `openconfig-interfaces` | all vendors | no |
| `network-instances` | openconfig | ON_CHANGE | — | any of: `openconfig-bgp`, `openconfig-network-instance` | all vendors | no |
| `…cols/protocol/bgp/neighbors/neighbor/afi-safis/afi-safi` | openconfig | SAMPLE | 1m | any of: `openconfig-bgp` | all vendors | yes |
| `bfd` | openconfig | ON_CHANGE | — | `openconfig-bfd` | all vendors | yes |
| `lldp` | openconfig | ON_CHANGE | — | `openconfig-lldp` | all vendors | yes |

## YANG Models Required

| Model | Vendor scope |
|-------|-------------|
| `Cisco-IOS-XR-infra-statsd-oper` | cisco_xrd (any-of) |
| `Cisco-IOS-XR-ipv4-bgp-oper` | cisco_xrd (any-of) |
| `openconfig-bfd` | all vendors |
| `openconfig-bgp` | all vendors (any-of) |
| `openconfig-interfaces` | all vendors |
| `openconfig-lldp` | all vendors |
| `openconfig-network-instance` | all vendors (any-of) |
| `srl_nokia` | nokia_srl (any-of) |

## Vendor-Native Fallbacks

- **cisco_xrd** `Cisco-IOS-XR-ipv4-bgp-oper:bgp/instances/instance/instance-active/default-vrf/neighbors/neighbor` falls back for `network-instances` when the preferred OpenConfig model is not advertised.

## Path Rationales

- **`Cisco-IOS-XR-infra-statsd-oper:infra-statistics/interfaces/interface[interface-name=*]/generic-counters`** [native] — IOS-XR native interface counters — peering link utilisation.
- **`Cisco-IOS-XR-ipv4-bgp-oper:bgp/instances/instance/instance-active/default-vrf/neighbors/neighbor`** [native] — IOS-XR native BGP neighbor state for eBGP peering sessions.
- **`Cisco-IOS-XR-ipv4-bgp-oper:bgp/instances/instance/instance-active/default-vrf/afs/af`** [native] — IOS-XR native BGP AFI summary — received prefix counts per peer.
- **`network-instance[name=default]/protocols/bgp/neighbor[peer-address=*]`** [native] — SR Linux native BGP peering session state.
- **`interfaces`** [openconfig] — OpenConfig interface counters — peering link bandwidth and error rates.
- **`interfaces`** [openconfig] — OpenConfig interface oper-state.
- **`network-instances`** [openconfig] — OpenConfig BGP — eBGP session state and received/advertised prefix counts.
- **`network-instances/network-instance/protocols/protocol/bgp/neighbors/neighbor/afi-safis/afi-safi`** [openconfig] — OpenConfig per-AFI prefix counts — anomaly detection on received-prefix count drops.
- **`bfd`** [openconfig] — OpenConfig BFD for fast peering link failure detection.
- **`lldp`** [openconfig] — OpenConfig LLDP — typically disabled on external peering interfaces but useful on IXP segments.

## Known Gaps

<!-- Add known gaps, vendor quirks, or lab-verification notes here. -->
