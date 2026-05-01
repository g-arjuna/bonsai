# campus_core — Campus core router/switch

**Environment**: campus_wired  
**Roles**: core, distribution, border  
**Verification**: not-yet-verified (requires campus topology with WAN hand-off or inter-site BGP)

## YANG Models Required

| Path | Model | Notes |
|------|-------|-------|
| `interfaces` | `openconfig-interfaces` | All vendors |
| `lldp` | `openconfig-lldp` | All vendors |
| `network-instances` | `openconfig-bgp`, `openconfig-network-instance` | Optional — WAN/inter-campus BGP |
| `network-instances/.../ospf` | `openconfig-ospfv2` | Optional |
| `network-instances/.../isis` | `openconfig-isis` | Optional |
| `bfd` | `openconfig-bfd` | Optional |

## Signal Rationale

Campus core devices carry inter-site routing and WAN hand-off. This profile mirrors a light
SP-P profile but at campus scale — the routing protocol surface is smaller and vendor diversity
is higher (enterprise gear alongside SP-class routers).

- **Interface counters** (SAMPLE 10 s): tighter sampling than distribution — core links are
  high-value and saturation events need rapid detection.
- **Interface oper-state** (ON_CHANGE): core link failures immediately affect inter-site reachability.
- **LLDP** (ON_CHANGE): physical topology across core nodes.
- **BGP** (ON_CHANGE, optional): WAN peering or inter-campus BGP — session loss is an incident.
- **OSPF/IS-IS** (ON_CHANGE, optional): core adjacency health — loss here causes routing black holes.
- **BFD** (ON_CHANGE, optional): fast core link failure detection; typically configured on every
  core-to-distribution link.

## Lab Setup for Verification

Requires a topology with:
1. A core switch/router with BGP or OSPF adjacencies to at least two distribution devices
2. Optionally a WAN link or stub external AS for BGP testing

Expected telemetry after convergence:
- BGP sessions show `ESTABLISHED` with non-zero prefix counts (if configured)
- OSPF/IS-IS adjacencies show `FULL`/`UP` state
- Core link counters updating at 10 s intervals

## Known Gaps

- Nokia SRL campus deployments are uncommon; SRL-native paths are not included in this profile.
  If SRL appears as a campus core device, the `dc_spine_standard` profile is a better starting point.
- OpenConfig BGP path granularity at campus scale does not include fine-grained per-prefix counters;
  use Prometheus remote-write from a route reflector for that detail.
