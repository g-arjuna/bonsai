# sp_p_core — Service-provider P-core and edge profile: interface, IS-IS, MPLS/LDP, and segment-routing telemetry; no BGP by default.

**Environment**: service provider  
**Roles**: p, core, edge  
**Vendor scope**: all vendors (OpenConfig + per-vendor natives)  
**Verification**: not-yet-verified

## Rationale

P-core and edge transport devices carry IGP and transport state; BGP is intentionally omitted unless another profile is selected.

## Subscribed Paths

| Path | Origin | Mode | Interval | Models | Vendors | Optional |
|------|--------|------|----------|--------|---------|----------|
| `interfaces` | openconfig | SAMPLE | 10s | `openconfig-interfaces` | all vendors | no |
| `interfaces` | openconfig | ON_CHANGE | — | `openconfig-interfaces` | all vendors | no |
| `lldp` | openconfig | ON_CHANGE | — | `openconfig-lldp` | all vendors | yes |
| `mpls` | openconfig | ON_CHANGE | — | `openconfig-mpls` | all vendors | no |
| `network-instances/network-instance/protocols/protocol/isis` | openconfig | ON_CHANGE | — | `openconfig-isis` | all vendors | no |
| `segment-routing` | openconfig | ON_CHANGE | — | `openconfig-segment-routing` | all vendors | no |

## YANG Models Required

| Model | Vendor scope |
|-------|-------------|
| `openconfig-interfaces` | all vendors |
| `openconfig-isis` | all vendors |
| `openconfig-lldp` | all vendors |
| `openconfig-mpls` | all vendors |
| `openconfig-segment-routing` | all vendors |

## Path Rationales

- **`interfaces`** [openconfig] — OpenConfig interface counters.
- **`interfaces`** [openconfig] — OpenConfig interface oper-state changes.
- **`lldp`** [openconfig] — OpenConfig LLDP topology evidence when advertised.
- **`mpls`** [openconfig] — MPLS state for transport health.
- **`network-instances/network-instance/protocols/protocol/isis`** [openconfig] — IS-IS state for SP underlay health.
- **`segment-routing`** [openconfig] — Segment-routing state when advertised.

## Known Gaps

<!-- Add known gaps, vendor quirks, or lab-verification notes here. -->
