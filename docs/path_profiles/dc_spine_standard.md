# dc_spine_standard — Standard data-center spine telemetry: interfaces, BGP, LLDP, and BFD when available.

**Environment**: data center, home lab  
**Roles**: spine, superspine, distribution, border  
**Vendor scope**: all vendors (OpenConfig + per-vendor natives)  
**Verification**: not-yet-verified

## Rationale

Spine, superspine, distribution, and border devices need the same health and topology signals as leaves, with BGP fanout emphasized.

## Subscribed Paths

| Path | Origin | Mode | Interval | Models | Vendors | Optional |
|------|--------|------|----------|--------|---------|----------|
| `interface[name=*]/statistics` | native | SAMPLE | 10s | any of: `srl_nokia` | nokia_srl | no |
| `interface[name=*]/oper-state` | native | ON_CHANGE | — | any of: `srl_nokia` | nokia_srl | no |
| `…ce[name=default]/protocols/bgp/neighbor[peer-address=*]` | native | ON_CHANGE | — | any of: `srl_nokia` | nokia_srl | no |
| `system/lldp/interface[name=*]/neighbor[id=*]` | native | ON_CHANGE | — | any of: `srl_nokia` | nokia_srl | no |
| `bfd/network-instance[name=default]` | native | ON_CHANGE | — | any of: `srl_nokia-bfd` | nokia_srl | yes |
| `…interfaces/interface[interface-name=*]/generic-counters` | native | SAMPLE | 10s | any of: `Cisco-IOS-XR-infra-statsd-oper` | cisco_xrd | no |
| `…rnet-lldp-oper:lldp/nodes/node/neighbors/details/detail` | native | SAMPLE | 1m | any of: `Cisco-IOS-XR-ethernet-lldp-oper` | cisco_xrd | yes |
| `interfaces` | openconfig | SAMPLE | 10s | `openconfig-interfaces` | all vendors | no |
| `interfaces` | openconfig | ON_CHANGE | — | `openconfig-interfaces` | all vendors | no |
| `network-instances` | openconfig | ON_CHANGE | — | any of: `openconfig-bgp`, `openconfig-network-instance` | all vendors | no |
| `bfd` | openconfig | ON_CHANGE | — | `openconfig-bfd` | all vendors | yes |
| `lldp` | openconfig | ON_CHANGE | — | `openconfig-lldp` | all vendors | no |

## YANG Models Required

| Model | Vendor scope |
|-------|-------------|
| `Cisco-IOS-XR-ethernet-lldp-oper` | cisco_xrd (any-of) |
| `Cisco-IOS-XR-infra-statsd-oper` | cisco_xrd (any-of) |
| `openconfig-bfd` | all vendors |
| `openconfig-bgp` | all vendors (any-of) |
| `openconfig-interfaces` | all vendors |
| `openconfig-lldp` | all vendors |
| `openconfig-network-instance` | all vendors (any-of) |
| `srl_nokia` | nokia_srl (any-of) |
| `srl_nokia-bfd` | nokia_srl (any-of) |

## Path Rationales

- **`interface[name=*]/statistics`** [native] — SR Linux native interface counters.
- **`interface[name=*]/oper-state`** [native] — SR Linux native interface state transitions.
- **`network-instance[name=default]/protocols/bgp/neighbor[peer-address=*]`** [native] — SR Linux native BGP neighbor state.
- **`system/lldp/interface[name=*]/neighbor[id=*]`** [native] — SR Linux native LLDP topology evidence.
- **`bfd/network-instance[name=default]`** [native] — SR Linux native BFD state when the BFD model is advertised.
- **`Cisco-IOS-XR-infra-statsd-oper:infra-statistics/interfaces/interface[interface-name=*]/generic-counters`** [native] — IOS-XR native interface counters.
- **`Cisco-IOS-XR-ethernet-lldp-oper:lldp/nodes/node/neighbors/details/detail`** [native] — IOS-XR native LLDP neighbors; sampled so existing neighbors are observed.
- **`interfaces`** [openconfig] — OpenConfig interface counters.
- **`interfaces`** [openconfig] — OpenConfig interface oper-state changes.
- **`network-instances`** [openconfig] — OpenConfig BGP/network-instance state.
- **`bfd`** [openconfig] — OpenConfig BFD state when available.
- **`lldp`** [openconfig] — OpenConfig LLDP topology evidence.

## Known Gaps

<!-- Add known gaps, vendor quirks, or lab-verification notes here. -->
