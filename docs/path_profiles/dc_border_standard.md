# dc_border_standard — DC border leaf / WAN-edge router

**Environment**: data_center  
**Roles**: border, edge, border-leaf  
**Verification**: not-yet-verified (requires topology with external BGP peering or WAN link)

## YANG Models Required

| Path | Model | Notes |
|------|-------|-------|
| `interfaces` | `openconfig-interfaces` | All vendors |
| `network-instances` | `openconfig-bgp`, `openconfig-network-instance` | External BGP peering + VRFs |
| `bfd` | `openconfig-bfd` | Optional |
| `lldp` | `openconfig-lldp` | Optional — may not run on WAN-facing ports |
| `vlans` | `openconfig-vlan` | Optional — trunk port L2 visibility |
| Nokia native BGP + BFD | `srl_nokia`, `srl_nokia-bfd` | Nokia SRL only |
| IOS-XR native BGP + counters | `Cisco-IOS-XR-ipv4-bgp-oper`, `Cisco-IOS-XR-infra-statsd-oper` | IOS-XR only |

## Signal Rationale

Border devices terminate the DC fabric at its external edge — WAN hand-off, peering with
other fabrics, or service chaining. This is where BGP session loss is most likely to cause
tenant-visible outages.

- **Interface counters** (SAMPLE 10 s): WAN uplink throughput and error rates; saturation on
  a border uplink is an incident before packet loss is measurable.
- **Interface oper-state** (ON_CHANGE): WAN link flap is a high-severity event.
- **BGP / network-instances** (ON_CHANGE): external BGP session state across all VRFs including
  route-leaking VRFs. Combined with enrichment (ServiceNow CMDB), session loss can be correlated
  against business services using the peered routes.
- **BFD** (ON_CHANGE, optional): fast WAN link failure detection supplementing BGP hold-down timers.
- **VLAN** (ON_CHANGE, optional): L2 trunk port membership for L2VPN or DCI scenarios.

## Vendor Notes

### Nokia SRL
Native paths for interface counters, BGP across all network instances (including VRFs), and BFD
are used because OC support on SRL border devices is more complete via native YANG at this path
granularity. The `srl_nokia` model advertisement signals support.

### Cisco IOS-XR
Native BGP neighbor state via `Cisco-IOS-XR-ipv4-bgp-oper` is included as a `fallback_for`
the OC `network-instances` path. IOS-XR advertises the native model more reliably than OC BGP
in containerised deployments.

## Lab Setup for Verification

Requires a topology with:
1. A border leaf connected to a simulated WAN or external AS (use FRR as a stub BGP peer)
2. At least one eBGP session in the default VRF and optionally one in a VRF

Expected telemetry after convergence:
- BGP session shows `ESTABLISHED` with non-zero `prefixes-received`
- BFD session shows `UP` on the WAN-facing interface (if configured)
- Interface counters update at 10 s intervals on the WAN uplink

## Known Gaps

- Arista cEOS: OC BGP path coverage is good; native YANG paths for border-specific counters
  not yet lab-tested. Optional paths are safe to subscribe.
- Juniper cRPD: `openconfig-network-instance` support for per-VRF BGP is partial in current
  cRPD versions; use `junos-routing-instance` native paths once cRPD accounts are available.
