# sp_pe_full — Service-provider PE/RR/peering profile: base telemetry plus MPLS, IS-IS, and segment-routing when advertised.

**Environment**: service provider  
**Roles**: pe, rr, peering  
**Vendor scope**: all vendors (OpenConfig + per-vendor natives)  
**Verification**: not-yet-verified

## Rationale

PE, route-reflector, and peering devices need base control-plane health plus SP transport/control-plane model coverage.

## Subscribed Paths

| Path | Origin | Mode | Interval | Models | Vendors | Optional |
|------|--------|------|----------|--------|---------|----------|
| `interfaces` | openconfig | SAMPLE | 10s | `openconfig-interfaces` | all vendors | no |
| `interfaces` | openconfig | ON_CHANGE | — | `openconfig-interfaces` | all vendors | no |
| `network-instances` | openconfig | ON_CHANGE | — | any of: `openconfig-bgp`, `openconfig-network-instance` | all vendors | no |
| `lldp` | openconfig | ON_CHANGE | — | `openconfig-lldp` | all vendors | yes |
| `bfd` | openconfig | ON_CHANGE | — | `openconfig-bfd` | all vendors | yes |
| `mpls` | openconfig | ON_CHANGE | — | `openconfig-mpls` | all vendors | no |
| `network-instances/network-instance/protocols/protocol/isis` | openconfig | ON_CHANGE | — | `openconfig-isis` | all vendors | no |
| `segment-routing` | openconfig | ON_CHANGE | — | `openconfig-segment-routing` | all vendors | no |

## YANG Models Required

| Model | Vendor scope |
|-------|-------------|
| `openconfig-bfd` | all vendors |
| `openconfig-bgp` | all vendors (any-of) |
| `openconfig-interfaces` | all vendors |
| `openconfig-isis` | all vendors |
| `openconfig-lldp` | all vendors |
| `openconfig-mpls` | all vendors |
| `openconfig-network-instance` | all vendors (any-of) |
| `openconfig-segment-routing` | all vendors |

## Path Rationales

- **`interfaces`** [openconfig] — OpenConfig interface counters.
- **`interfaces`** [openconfig] — OpenConfig interface oper-state changes.
- **`network-instances`** [openconfig] — OpenConfig BGP/network-instance state for PE/RR control plane.
- **`lldp`** [openconfig] — OpenConfig LLDP topology evidence when advertised.
- **`bfd`** [openconfig] — OpenConfig BFD state when available.
- **`mpls`** [openconfig] — MPLS state for PE transport health.
- **`network-instances/network-instance/protocols/protocol/isis`** [openconfig] — IS-IS state for SP underlay health.
- **`segment-routing`** [openconfig] — Segment-routing state when the model is advertised.

## Known Gaps

<!-- Add known gaps, vendor quirks, or lab-verification notes here. -->
