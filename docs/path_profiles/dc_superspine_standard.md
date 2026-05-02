# dc_superspine_standard — Data-center superspine telemetry: interfaces, BGP route fanout, ECMP health, and LLDP.

**Environment**: data center  
**Roles**: superspine, spine, distribution, border  
**Vendor scope**: all vendors (OpenConfig + per-vendor natives)  
**Verification**: not-yet-verified

## Rationale

Superspine devices carry aggregated BGP sessions; interface and BGP fanout health are the primary signals. BFD on all uplinks.

## Subscribed Paths

| Path | Origin | Mode | Interval | Models | Vendors | Optional |
|------|--------|------|----------|--------|---------|----------|
| `interface[name=*]/statistics` | native | SAMPLE | 10s | any of: `srl_nokia` | nokia_srl | no |
| `interface[name=*]/oper-state` | native | ON_CHANGE | — | any of: `srl_nokia` | nokia_srl | no |
| `…ce[name=default]/protocols/bgp/neighbor[peer-address=*]` | native | ON_CHANGE | — | any of: `srl_nokia` | nokia_srl | no |
| `system/lldp/interface[name=*]/neighbor[id=*]` | native | ON_CHANGE | — | any of: `srl_nokia` | nokia_srl | no |
| `bfd/network-instance[name=default]` | native | ON_CHANGE | — | any of: `srl_nokia-bfd` | nokia_srl | yes |
| `…interfaces/interface[interface-name=*]/generic-counters` | native | SAMPLE | 10s | any of: `Cisco-IOS-XR-infra-statsd-oper` | cisco_xrd | no |
| `…instance/instance-active/default-vrf/neighbors/neighbor` | native | ON_CHANGE | — | any of: `Cisco-IOS-XR-ipv4-bgp-oper` | cisco_xrd | no |
| `interfaces` | openconfig | SAMPLE | 10s | `openconfig-interfaces` | all vendors | no |
| `interfaces` | openconfig | ON_CHANGE | — | `openconfig-interfaces` | all vendors | no |
| `network-instances` | openconfig | ON_CHANGE | — | any of: `openconfig-bgp`, `openconfig-network-instance` | all vendors | no |
| `bfd` | openconfig | ON_CHANGE | — | `openconfig-bfd` | all vendors | yes |
| `lldp` | openconfig | ON_CHANGE | — | `openconfig-lldp` | all vendors | no |

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
| `srl_nokia-bfd` | nokia_srl (any-of) |

## Vendor-Native Fallbacks

- **cisco_xrd** `Cisco-IOS-XR-ipv4-bgp-oper:bgp/instances/instance/instance-active/default-vrf/neighbors/neighbor` falls back for `network-instances` when the preferred OpenConfig model is not advertised.

## Path Rationales

- **`interface[name=*]/statistics`** [native] — SR Linux native interface counters for high-speed uplink monitoring.
- **`interface[name=*]/oper-state`** [native] — SR Linux native interface state — superspine link flaps propagate immediately.
- **`network-instance[name=default]/protocols/bgp/neighbor[peer-address=*]`** [native] — SR Linux native BGP — superspine may have hundreds of sessions.
- **`system/lldp/interface[name=*]/neighbor[id=*]`** [native] — SR Linux native LLDP for cabling verification.
- **`bfd/network-instance[name=default]`** [native] — SR Linux native BFD — fast failure detection on spine-leaf uplinks.
- **`Cisco-IOS-XR-infra-statsd-oper:infra-statistics/interfaces/interface[interface-name=*]/generic-counters`** [native] — IOS-XR native interface counters.
- **`Cisco-IOS-XR-ipv4-bgp-oper:bgp/instances/instance/instance-active/default-vrf/neighbors/neighbor`** [native] — IOS-XR native BGP neighbor state when openconfig-bgp not advertised.
- **`interfaces`** [openconfig] — OpenConfig interface counters for vendor-neutral traffic rates.
- **`interfaces`** [openconfig] — OpenConfig interface oper-state events.
- **`network-instances`** [openconfig] — OpenConfig BGP — all superspine sessions, peer state, prefixes.
- **`bfd`** [openconfig] — OpenConfig BFD fast failure detection when model is advertised.
- **`lldp`** [openconfig] — OpenConfig LLDP physical topology.

## Known Gaps

<!-- Add known gaps, vendor quirks, or lab-verification notes here. -->
