# sp_p_sr_te — Service-provider P-core with SR-TE: IS-IS, SR adjacency and prefix SIDs, SR-TE policy state, MPLS forwarding, and interfaces.

**Environment**: service provider  
**Roles**: p, core, edge  
**Vendor scope**: all vendors (OpenConfig + per-vendor natives)  
**Verification**: not-yet-verified

## Rationale

SR-TE P-core nodes carry segment-routing policy paths and SR adjacencies; IS-IS SR extensions, SR-TE policy state, and RSVP-TE (if present) are the primary signals beyond base transport.

## Subscribed Paths

| Path | Origin | Mode | Interval | Models | Vendors | Optional |
|------|--------|------|----------|--------|---------|----------|
| `…interfaces/interface[interface-name=*]/generic-counters` | native | SAMPLE | 10s | any of: `Cisco-IOS-XR-infra-statsd-oper` | cisco_xrd | no |
| `Cisco-IOS-XR-mpls-te-oper:mpls-te/tunnels/summary` | native | SAMPLE | 30s | any of: `Cisco-IOS-XR-mpls-te-oper` | cisco_xrd | yes |
| `…-segment-routing-ms-oper:srms/policy/policy-ipv4-backup` | native | ON_CHANGE | — | any of: `Cisco-IOS-XR-segment-routing-ms-oper` | cisco_xrd | yes |
| `…ame=default]/segment-routing/mpls/adjacency-sid-mapping` | native | ON_CHANGE | — | any of: `srl_nokia` | nokia_srl | yes |
| `interfaces` | openconfig | SAMPLE | 10s | `openconfig-interfaces` | all vendors | no |
| `interfaces` | openconfig | ON_CHANGE | — | `openconfig-interfaces` | all vendors | no |
| `lldp` | openconfig | ON_CHANGE | — | `openconfig-lldp` | all vendors | yes |
| `mpls` | openconfig | ON_CHANGE | — | `openconfig-mpls` | all vendors | no |
| `network-instances/network-instance/protocols/protocol/isis` | openconfig | ON_CHANGE | — | `openconfig-isis` | all vendors | no |
| `segment-routing` | openconfig | ON_CHANGE | — | `openconfig-segment-routing` | all vendors | no |
| `bfd` | openconfig | ON_CHANGE | — | `openconfig-bfd` | all vendors | yes |

## YANG Models Required

| Model | Vendor scope |
|-------|-------------|
| `Cisco-IOS-XR-infra-statsd-oper` | cisco_xrd (any-of) |
| `Cisco-IOS-XR-mpls-te-oper` | cisco_xrd (any-of) |
| `Cisco-IOS-XR-segment-routing-ms-oper` | cisco_xrd (any-of) |
| `openconfig-bfd` | all vendors |
| `openconfig-interfaces` | all vendors |
| `openconfig-isis` | all vendors |
| `openconfig-lldp` | all vendors |
| `openconfig-mpls` | all vendors |
| `openconfig-segment-routing` | all vendors |
| `srl_nokia` | nokia_srl (any-of) |

## Vendor-Native Fallbacks

- **cisco_xrd** `Cisco-IOS-XR-mpls-te-oper:mpls-te/tunnels/summary` falls back for `mpls` when the preferred OpenConfig model is not advertised.
- **cisco_xrd** `Cisco-IOS-XR-segment-routing-ms-oper:srms/policy/policy-ipv4-backup` falls back for `segment-routing` when the preferred OpenConfig model is not advertised.

## Path Rationales

- **`Cisco-IOS-XR-infra-statsd-oper:infra-statistics/interfaces/interface[interface-name=*]/generic-counters`** [native] — IOS-XR native interface counters.
- **`Cisco-IOS-XR-mpls-te-oper:mpls-te/tunnels/summary`** [native] — IOS-XR native MPLS-TE tunnel summary when openconfig-mpls not advertised.
- **`Cisco-IOS-XR-segment-routing-ms-oper:srms/policy/policy-ipv4-backup`** [native] — IOS-XR native SR mapping server policy state.
- **`network-instance[name=default]/segment-routing/mpls/adjacency-sid-mapping`** [native] — SR Linux native adjacency-SID mapping for SR label verification.
- **`interfaces`** [openconfig] — OpenConfig interface counters.
- **`interfaces`** [openconfig] — OpenConfig interface oper-state.
- **`lldp`** [openconfig] — OpenConfig LLDP when enabled on core interfaces.
- **`mpls`** [openconfig] — OpenConfig MPLS state — LSP health and label stack.
- **`network-instances/network-instance/protocols/protocol/isis`** [openconfig] — OpenConfig IS-IS state including SR extensions (adj-SID, prefix-SID).
- **`segment-routing`** [openconfig] — OpenConfig SR state — active policy paths and binding SID state.
- **`bfd`** [openconfig] — OpenConfig BFD for microsecond failure detection on SR paths.

## Known Gaps

<!-- Add known gaps, vendor quirks, or lab-verification notes here. -->
